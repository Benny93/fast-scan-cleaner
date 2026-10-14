//! System tools (Trash, Homebrew, Docker, Git, …): checking them and running confirmed cleanups.

use super::*;

impl Shared {
    /// Switch screens; Disk Map and Cleanup need a finished scan.
    /// Open Cleanup with the system tools section at the top.
    pub fn show_system_tools(self: &Arc<Self>) {
        self.show(Screen::Cleanup);
        self.core.lock().unwrap().system_first = true;
        self.changed();
    }

    fn env(&self) -> cleaners::Env<'_> {
        cleaners::Env { run: &self.deps.runner, trash_dir: self.deps.trash_dir.as_deref() }
    }

    /// Check every system cleaner in the background (items already running are left alone).
    pub fn check_system(self: &Arc<Self>) {
        let (tools, repos) = {
            let mut c = self.core.lock().unwrap();
            let running = |c: &Core, cl: Cleaner, repo: Option<&Path>| c.system.get(&cleaners::item_id(cl, repo)).is_some_and(|i| matches!(i.status, SystemStatus::Running));
            let tools: Vec<Cleaner> = Cleaner::TOOLS.into_iter().filter(|t| t.applies(std::env::consts::OS) && !running(&c, *t, None)).collect();
            let mut repos = match (&c.tree, &c.root) {
                (Some(t), Some(r)) => cleaners::git_repos(t, r, cleaners::GIT_REPO_LIMIT),
                _ => Vec::new(),
            };
            repos.retain(|r| !running(&c, Cleaner::GitBranches, Some(r)) && !running(&c, Cleaner::GitWorktrees, Some(r)));
            c.system.retain(|_, i| i.repo.is_none() || matches!(i.status, SystemStatus::Running));
            for t in &tools {
                set_status(&mut c, *t, None, SystemStatus::Checking);
            }
            for r in &repos {
                for g in [Cleaner::GitBranches, Cleaner::GitWorktrees] {
                    set_status(&mut c, g, Some(r), SystemStatus::Checking);
                }
            }
            (tools, repos)
        };
        self.changed();
        for t in tools {
            let me = self.clone();
            std::thread::spawn(move || {
                let status = cleaners::check_tool(t, &me.env());
                me.finish_check(t, None, status);
            });
        }
        if !repos.is_empty() {
            let me = self.clone();
            std::thread::spawn(move || {
                use rayon::prelude::*;
                repos.par_iter().for_each(|r| me.check_repo(r));
            });
        }
    }

    fn check_repo(&self, repo: &Path) {
        let (branches, worktrees) = match cleaners::check_git(repo, &self.env()) {
            Ok(f) => {
                let sizes: u64 = {
                    let c = self.core.lock().unwrap();
                    f.worktrees.iter().filter_map(|w| size_in_tree(&c, w)).sum()
                };
                (
                    SystemStatus::Ready { estimate: Estimate::Count(f.branches.len() as u64), detail: f.branches },
                    SystemStatus::Ready { estimate: worktree_estimate(sizes, f.worktrees.len()), detail: f.worktrees.iter().map(|p| p.display().to_string()).collect() },
                )
            }
            Err(reason) => (SystemStatus::Unavailable { reason: reason.clone() }, SystemStatus::Unavailable { reason }),
        };
        self.finish_check(Cleaner::GitBranches, Some(repo), branches);
        self.finish_check(Cleaner::GitWorktrees, Some(repo), worktrees);
    }

    /// Check one system tool again, e.g. the Trash after files were moved there.
    pub fn recheck_tool(self: &Arc<Self>, tool: Cleaner) {
        {
            let mut c = self.core.lock().unwrap();
            if c.system.get(&cleaners::item_id(tool, None)).is_some_and(|i| matches!(i.status, SystemStatus::Running)) {
                return;
            }
            set_status(&mut c, tool, None, SystemStatus::Checking);
        }
        self.changed();
        let me = self.clone();
        std::thread::spawn(move || {
            let status = cleaners::check_tool(tool, &me.env());
            me.finish_check(tool, None, status);
        });
    }

    fn finish_check(&self, cleaner: Cleaner, repo: Option<&Path>, status: SystemStatus) {
        let mut c = self.core.lock().unwrap();
        let id = cleaners::item_id(cleaner, repo);
        if !c.system.get(&id).is_some_and(|i| matches!(i.status, SystemStatus::Running)) {
            set_status(&mut c, cleaner, repo, status);
        }
        drop(c);
        self.changed();
    }

    pub fn request_system_cleanup(&self, id: &str, reason: Option<String>, origin: Origin) -> Result<oneshot::Receiver<SystemOutcome>, String> {
        self.request_system_batch(vec![(id.to_string(), None)], reason, origin)
    }

    /// Opens one confirmation for several system cleanups. `only` limits a Git item to some of the
    /// branches or worktrees its last check found; anything not in that list is refused, so a
    /// selection can never name a branch or path the check did not offer.
    pub fn request_system_batch(&self, picks: Vec<(String, Option<Vec<String>>)>, reason: Option<String>, origin: Origin) -> Result<oneshot::Receiver<SystemOutcome>, String> {
        let mut c = self.core.lock().unwrap();
        if c.pending.is_some() {
            return Err("another confirmation is already open".into());
        }
        let mut requests = Vec::new();
        for (id, only) in picks {
            if requests.iter().any(|r: &SystemRequest| r.id == id) {
                continue;
            }
            let item = c.system.get(&id).ok_or("unknown system cleanup")?.clone();
            let SystemStatus::Ready { estimate, detail } = item.status else { return Err("not ready (still checking, unavailable or running)".into()) };
            let (estimate, detail) = match only {
                None if estimate.is_nothing() => return Err("nothing to clean up".into()),
                None => (estimate, detail),
                Some(only) if only.is_empty() || !only.iter().all(|o| detail.contains(o)) => return Err("selection is not part of the last check".into()),
                Some(only) => (Estimate::Count(only.len() as u64), only),
            };
            requests.push(SystemRequest { id, cleaner: item.cleaner, repo: item.repo, estimate, detail });
        }
        if requests.is_empty() {
            return Err("nothing selected".into());
        }
        let (tx, rx) = oneshot::channel();
        c.pending = Some(Pending { items: Vec::new(), reason, origin, reply: None, system: requests, system_reply: Some(tx) });
        drop(c);
        self.changed();
        Ok(rx)
    }

    /// Runs the confirmed cleanups one after another (two Git commands on one repository at once
    /// would fight over its lock) and replies with the first failure, else the total freed.
    pub(super) fn run_systems(self: &Arc<Self>, reqs: Vec<SystemRequest>, reply: Option<oneshot::Sender<SystemOutcome>>) {
        // The caller already marked the items Running under the lock that took the pending.
        self.changed();
        let me = self.clone();
        std::thread::spawn(move || {
            let outcomes: Vec<SystemOutcome> = reqs.iter().map(|req| me.run_one(req)).collect();
            let outcome = outcomes.iter().find(|o| matches!(o, SystemOutcome::Failed { .. })).cloned().unwrap_or_else(|| SystemOutcome::Done {
                freed: outcomes.iter().map(|o| if let SystemOutcome::Done { freed } = o { *freed } else { None }).sum(),
            });
            me.changed();
            if let Some(tx) = reply {
                let _ = tx.send(outcome);
            }
        });
    }

    fn run_one(self: &Arc<Self>, req: &SystemRequest) -> SystemOutcome {
        let me = self;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| cleaners::run(req.cleaner, req.repo.as_deref(), &req.detail, &me.env())))
            .unwrap_or_else(|_| Err("internal error".to_string()));
        if req.cleaner == Cleaner::GitWorktrees {
            // Drop removed worktrees from the Disk Map and findings (some may have been skipped).
            let mut c = me.core.lock().unwrap();
            for p in req.detail.iter().map(Path::new).filter(|p| !p.exists()) {
                forget(&mut c, p);
            }
            let files = c.stats.map_or(0, |s| s.files);
            c.stats = c.tree.as_ref().map(|t| stats_of(t, files));
        }
        let outcome = match result {
            Ok(()) => {
                // Re-check so the card shows what is left.
                let after = match req.repo.as_deref() {
                    Some(repo) => {
                        set_status(&mut me.core.lock().unwrap(), req.cleaner, Some(repo), SystemStatus::Checking);
                        me.check_repo(repo);
                        me.core.lock().unwrap().system.get(&req.id).map(|i| i.status.clone())
                    }
                    None => {
                        let s = cleaners::check_tool(req.cleaner, &me.env());
                        set_status(&mut me.core.lock().unwrap(), req.cleaner, None, s.clone());
                        Some(s)
                    }
                };
                let freed = match (req.estimate, after) {
                    (Estimate::Bytes(before), Some(SystemStatus::Ready { estimate: Estimate::Bytes(now), .. })) => Some(before.saturating_sub(now)),
                    _ => None,
                };
                me.core.lock().unwrap().system_toasts.push((req.cleaner, req.repo.clone(), freed));
                SystemOutcome::Done { freed }
            }
            Err(error) => {
                set_status(&mut me.core.lock().unwrap(), req.cleaner, req.repo.as_deref(), SystemStatus::Failed { error: error.clone() });
                SystemOutcome::Failed { error }
            }
        };
        me.changed();
        outcome
    }

    /// Declines the open confirmation only if it is the system cleanup `id`; otherwise touches nothing.
    pub fn decline_system_if_pending(&self, id: &str) -> bool {
        let mut c = self.core.lock().unwrap();
        if !c.pending.as_ref().is_some_and(|p| p.system.iter().any(|r| r.id == id)) {
            return false;
        }
        let mut pending = c.pending.take().unwrap();
        drop(c);
        if let Some(tx) = pending.system_reply.take() {
            let _ = tx.send(SystemOutcome::Declined);
        }
        self.changed();
        true
    }
}
