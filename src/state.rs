use crate::actions::{self, Decision, Gate, Origin, TrashResult};
use crate::rules::{self, Finding, Rules, Safety};
use crate::scan::{self, Kind, Node, Progress};
use crate::settings::Settings;
use crate::cleaners::{self, Cleaner, Estimate, SystemStatus};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Home,
    DiskMap,
    Cleanup,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ScanStats {
    pub files: u64,
    pub bytes: u64,
    pub largest_file: u64,
}

/// A scan that did not finish. `error` is `None` when the user cancelled it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Interrupted {
    pub root: PathBuf,
    pub files: u64,
    pub bytes: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeState {
    Idle,
    Scanning,
    Complete,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", content = "detail", rename_all = "lowercase")]
pub enum ItemStatus {
    Trashed,
    Refused(String),
    Failed(String),
    Pending,
    Declined,
}

pub type Report = Vec<(PathBuf, ItemStatus)>;

#[derive(Debug, Clone, Serialize)]
pub struct SystemItem {
    pub cleaner: Cleaner,
    pub repo: Option<PathBuf>,
    pub status: SystemStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum SystemOutcome {
    Done { freed: Option<u64> },
    Declined,
    Failed { error: String },
}

#[derive(Debug, Clone)]
pub struct SystemRequest {
    pub id: String,
    pub cleaner: Cleaner,
    pub repo: Option<PathBuf>,
    pub estimate: Estimate,
    pub detail: Vec<String>,
}

pub struct Pending {
    /// Size is `None` when the path is not in the current scan.
    pub items: Vec<(PathBuf, Option<u64>)>,
    pub reason: Option<String>,
    pub origin: Origin,
    reply: Option<oneshot::Sender<Report>>,
    /// Set for system cleanups (then `items` is empty); several when the user picked Git
    /// branches or worktrees in more than one repository. They run one after another.
    pub system: Vec<SystemRequest>,
    system_reply: Option<oneshot::Sender<SystemOutcome>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Banner {
    Home,
    Scanning(u64),
    Found(u64),
    Nothing,
    Confirm,
}

pub struct Core {
    pub settings: Settings,
    pub settings_recovered: bool,
    pub screen: Screen,
    pub root: Option<PathBuf>,
    /// Shared so the folder search can walk it without holding the core lock. Changes go
    /// through `Arc::make_mut`, which copies only while a search still holds a reference.
    pub tree: Option<Arc<Node>>,
    pub findings: Vec<Finding>,
    /// Relative to `root`.
    pub zoom: PathBuf,
    /// Relative to `root`.
    pub selected: Option<PathBuf>,
    pub pending: Option<Pending>,
    pub busy: bool,
    pub progress: Arc<Progress>,
    pub trash_errors: Vec<(PathBuf, String)>,
    /// Paths the user tried to trash that are protected (shown in the banner).
    pub refused: Vec<PathBuf>,
    /// Folder of the full scan that is running now (refreshes don't set it).
    pub scanning: Option<PathBuf>,
    /// Expected size of the running scan, from the last scan of the same folder.
    pub estimate: Option<u64>,
    pub stats: Option<ScanStats>,
    pub interrupted: Option<Interrupted>,
    /// (items, bytes) of successful trash actions not yet shown as a toast.
    pub toasts: Vec<(usize, u64)>,
    pub system: BTreeMap<String, SystemItem>,
    pub system_toasts: Vec<(Cleaner, Option<PathBuf>, Option<u64>)>,
    /// Cleanup was opened via a "System tools" shortcut (Home card or MCP): show that section first.
    pub system_first: bool,
    pub mcp_error: Option<String>,
    pub settings_error: Option<String>,
    /// Trashed while a full scan was running; removed again when its tree arrives.
    trashed_during_scan: Vec<PathBuf>,
}

impl Core {
    pub fn home_state(&self) -> HomeState {
        if self.scanning.is_some() {
            HomeState::Scanning
        } else if self.interrupted.is_some() {
            HomeState::Interrupted
        } else if self.tree.is_some() {
            HomeState::Complete
        } else {
            HomeState::Idle
        }
    }

    pub fn banner(&self) -> Banner {
        if self.pending.is_some() {
            Banner::Confirm
        } else if self.busy {
            Banner::Scanning(self.progress.files.load(Relaxed))
        } else if self.tree.is_none() {
            Banner::Home
        } else {
            let safe: u64 = self.findings.iter().filter(|f| f.safety == Safety::Safe).map(|f| f.size).sum();
            if safe > 0 { Banner::Found(safe) } else { Banner::Nothing }
        }
    }

    pub(crate) fn rel(&self, abs: &Path) -> Option<PathBuf> {
        abs.strip_prefix(self.root.as_ref()?).ok().map(Path::to_path_buf)
    }
}

fn stats_of(tree: &Node, files: u64) -> ScanStats {
    ScanStats { files, bytes: tree.size, largest_file: scan::largest_files(tree, Path::new(""), 1).first().map_or(0, |f| f.1) }
}

pub struct Deps {
    pub rules: Rules,
    pub trasher: fn(&[PathBuf]) -> Vec<TrashResult>,
    pub home: PathBuf,
    pub protected: Vec<PathBuf>,
    pub runner: cleaners::Runner,
    pub trash_dir: Option<PathBuf>,
}

impl Deps {
    pub fn real() -> Deps {
        Deps {
            rules: Rules::builtin(),
            trasher: actions::move_to_trash,
            home: dunce::canonicalize(crate::platform::home()).unwrap_or_else(|_| crate::platform::home()),
            protected: crate::platform::protected_subtrees(),
            runner: cleaners::real_runner(),
            trash_dir: if cfg!(target_os = "macos") {
                Some(crate::platform::home().join(".Trash"))
            } else if cfg!(windows) {
                None
            } else {
                dirs::data_dir().map(|d| d.join("Trash/files"))
            },
        }
    }
}

pub struct Shared {
    pub core: Mutex<Core>,
    pub deps: Deps,
    settings_path: PathBuf,
    notify: async_channel::Sender<()>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Shared {
    pub fn new(settings_path: PathBuf, notify: async_channel::Sender<()>) -> Arc<Shared> {
        let (settings, recovered) = Settings::load(&settings_path);
        Shared::build(settings, recovered, settings_path, notify, Deps::real())
    }

    pub fn build(settings: Settings, recovered: bool, settings_path: PathBuf, notify: async_channel::Sender<()>, deps: Deps) -> Arc<Shared> {
        Arc::new(Shared {
            core: Mutex::new(Core {
                settings,
                settings_recovered: recovered,
                screen: Screen::Home,
                root: None,
                tree: None,
                findings: Vec::new(),
                zoom: PathBuf::new(),
                selected: None,
                pending: None,
                busy: false,
                progress: Arc::default(),
                trash_errors: Vec::new(),
                refused: Vec::new(),
                scanning: None,
                estimate: None,
                stats: None,
                interrupted: None,
                toasts: Vec::new(),
                system: BTreeMap::new(),
                system_toasts: Vec::new(),
                system_first: false,
                mcp_error: None,
                settings_error: None,
                trashed_during_scan: Vec::new(),
            }),
            deps,
            settings_path,
            notify,
        })
    }

    /// Ask the UI to re-render. Coalesces: the channel holds at most one pending signal.
    pub fn changed(&self) {
        let _ = self.notify.try_send(());
    }

    pub fn start_scan(self: &Arc<Self>, path: PathBuf) -> oneshot::Receiver<Result<(), String>> {
        let (tx, rx) = oneshot::channel();
        let path = match dunce::canonicalize(&path) {
            Ok(p) => p,
            Err(e) => {
                self.core.lock().unwrap().interrupted = Some(Interrupted { root: path, files: 0, bytes: 0, error: Some(e.to_string()) });
                self.changed();
                let _ = tx.send(Err(e.to_string()));
                return rx;
            }
        };
        let progress = Arc::new(Progress::default());
        {
            let mut c = self.core.lock().unwrap();
            c.progress.cancel.store(true, Relaxed);
            c.progress = progress.clone();
            c.estimate = c.settings.recent_size(&path);
            if c.root.as_deref() == Some(path.as_path()) {
                // Rescanning the same folder: drop the old tree now instead of holding two copies
                // of millions of entries until the new one is done.
                c.tree = None;
                c.findings.clear();
                c.zoom = PathBuf::new();
                c.selected = None;
            }
            c.scanning = Some(path.clone());
            c.trashed_during_scan.clear();
            c.interrupted = None;
            c.busy = true;
            c.screen = Screen::Home;
            c.trash_errors.clear();
        }
        self.changed();
        let me = self.clone();
        std::thread::spawn(move || {
            let result = scan::scan(&path, &progress);
            let mut c = me.core.lock().unwrap();
            if !Arc::ptr_eq(&c.progress, &progress) {
                let _ = tx.send(Err("cancelled".into()));
                return; // a newer scan or cancel_scan replaced this one
            }
            c.busy = false;
            c.scanning = None;
            c.estimate = None;
            let trashed = std::mem::take(&mut c.trashed_during_scan);
            let mut recheck = false;
            let outcome = match result {
                Ok(tree) => {
                    c.findings = rules::findings(&tree, &path, &me.deps.rules);
                    c.settings.push_recent(path.clone(), tree.size, now_secs());
                    c.settings_error = c.settings.save(&me.settings_path).err().map(|e| e.to_string());
                    c.interrupted = None;
                    c.stats = Some(stats_of(&tree, progress.files.load(Relaxed)));
                    c.root = Some(path.clone());
                    c.tree = Some(Arc::new(tree));
                    c.zoom = PathBuf::new();
                    c.selected = None;
                    for p in &trashed {
                        forget(&mut c, p);
                    }
                    if !trashed.is_empty() {
                        let tree = c.tree.as_ref().unwrap();
                        let findings = rules::findings(tree, &path, &me.deps.rules);
                        let stats = stats_of(tree, progress.files.load(Relaxed));
                        c.findings = findings;
                        c.stats = Some(stats);
                    }
                    // Git rows belong to the old tree; re-check if the user has used Cleanup.
                    recheck = !c.system.is_empty();
                    if recheck {
                        c.system.retain(|_, i| i.repo.is_none() || matches!(i.status, SystemStatus::Running));
                    }
                    Ok(())
                }
                Err(e) => {
                    c.interrupted = Some(Interrupted {
                        root: path.clone(),
                        files: progress.files.load(Relaxed),
                        bytes: progress.bytes.load(Relaxed),
                        error: Some(e.to_string()),
                    });
                    Err(e.to_string())
                }
            };
            drop(c);
            if recheck {
                me.check_system();
            }
            me.changed();
            let _ = tx.send(outcome);
        });
        rx
    }

    pub fn refresh(self: &Arc<Self>, abs: Option<PathBuf>) -> oneshot::Receiver<Result<(), String>> {
        let (tx, rx) = oneshot::channel();
        let progress = Arc::new(Progress::default());
        let (root, rel) = {
            let mut c = self.core.lock().unwrap();
            if c.busy {
                let _ = tx.send(Err("a scan is already running".into()));
                return rx;
            }
            let Some(root) = c.root.clone() else {
                let _ = tx.send(Err("nothing scanned yet".into()));
                return rx;
            };
            let rel = match abs {
                Some(a) => match c.rel(&a) {
                    Some(r) => r,
                    None => {
                        let _ = tx.send(Err("path is outside the current scan".into()));
                        return rx;
                    }
                },
                None => c.zoom.clone(),
            };
            if rel.components().any(|x| !matches!(x, std::path::Component::Normal(_))) {
                let _ = tx.send(Err("invalid path".into()));
                return rx;
            }
            if !rel.as_os_str().is_empty() {
                c.progress = progress.clone();
                c.busy = true;
            }
            (root, rel)
        };
        if rel.as_os_str().is_empty() {
            return self.start_scan(root);
        }
        self.changed();
        let me = self.clone();
        std::thread::spawn(move || {
            let result = scan::scan(&root.join(&rel), &progress);
            let mut c = me.core.lock().unwrap();
            if !Arc::ptr_eq(&c.progress, &progress) {
                let _ = tx.send(Err("superseded".into()));
                return;
            }
            c.busy = false;
            let outcome = if c.root.as_ref() != Some(&root) || c.tree.is_none() {
                Err("a different folder was scanned meanwhile".to_string())
            } else {
                let tree = Arc::make_mut(c.tree.as_mut().unwrap());
                let ok = match result {
                    Ok(node) => tree.replace(&rel, node).is_some(),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => tree.remove(&rel).is_some(),
                    Err(_) => false,
                };
                let findings = rules::findings(tree, &root, &me.deps.rules);
                c.findings = findings;
                let files = c.stats.map_or(0, |s| s.files);
                c.stats = c.tree.as_ref().map(|t| stats_of(t, files));
                if c.tree.as_ref().unwrap().find(&c.zoom).is_none() {
                    c.zoom = rel.parent().map(Path::to_path_buf).unwrap_or_default();
                }
                if ok { Ok(()) } else { Err("folder is no longer in the scan".into()) }
            };
            drop(c);
            me.changed();
            let _ = tx.send(outcome);
        });
        rx
    }

    /// Stop the running full scan. Previous results stay; Home shows what was counted so far.
    pub fn cancel_scan(&self) {
        let mut c = self.core.lock().unwrap();
        let Some(root) = c.scanning.take() else { return };
        // A fresh Progress makes the running scan's result stale (see the ptr_eq check).
        let p = std::mem::take(&mut c.progress);
        p.cancel.store(true, Relaxed);
        c.interrupted = Some(Interrupted { root, files: p.files.load(Relaxed), bytes: p.bytes.load(Relaxed), error: None });
        c.busy = false;
        c.estimate = None;
        c.trashed_during_scan.clear();
        drop(c);
        self.changed();
    }

    /// "Back to Home" after a cancelled or failed scan.
    pub fn dismiss_interrupted(&self) {
        let mut c = self.core.lock().unwrap();
        c.interrupted = None;
        c.screen = Screen::Home;
        drop(c);
        self.changed();
    }

    /// Switch screens; Disk Map and Cleanup need a finished scan.
    /// Open Cleanup with the system tools section at the top.
    pub fn show_system_tools(self: &Arc<Self>) {
        self.show(Screen::Cleanup);
        self.core.lock().unwrap().system_first = true;
        self.changed();
    }

    pub fn show(self: &Arc<Self>, screen: Screen) {
        let mut c = self.core.lock().unwrap();
        // Cleanup also hosts the system tools, which work without a scan.
        let needs_scan = screen == Screen::DiskMap;
        c.screen = if needs_scan && c.tree.is_none() { Screen::Home } else { screen };
        c.system_first = false;
        let check = c.screen == Screen::Cleanup && c.system.is_empty();
        drop(c);
        self.changed();
        if check {
            self.check_system();
        }
    }

    pub fn navigate(&self, abs: &Path) -> Result<(), String> {
        let mut c = self.core.lock().unwrap();
        let rel = c.rel(abs).ok_or("path is outside the current scan")?;
        let is_dir = c.tree.as_ref().ok_or("nothing scanned yet")?.find(&rel).ok_or("path is not in the current scan")?.kind == Kind::Dir;
        c.zoom = if is_dir { rel.clone() } else { rel.parent().map(Path::to_path_buf).unwrap_or_default() };
        c.selected = Some(rel);
        c.screen = Screen::DiskMap;
        drop(c);
        self.changed();
        Ok(())
    }

    pub fn select(&self, abs: &Path) -> Result<(), String> {
        let mut c = self.core.lock().unwrap();
        let rel = c.rel(abs).ok_or("path is outside the current scan")?;
        c.tree.as_ref().ok_or("nothing scanned yet")?.find(&rel).ok_or("path is not in the current scan")?;
        c.selected = Some(rel);
        drop(c);
        self.changed();
        Ok(())
    }

    pub fn activate(&self, abs: &Path) -> Result<(), String> {
        let is_dir = {
            let c = self.core.lock().unwrap();
            c.rel(abs).and_then(|r| c.tree.as_ref()?.find(&r).map(|n| n.kind == Kind::Dir)).unwrap_or(false)
        };
        if is_dir { self.navigate(abs) } else { self.select(abs) }
    }

    pub fn request_trash(&self, paths: Vec<PathBuf>, reason: Option<String>, origin: Origin) -> (Report, Option<oneshot::Receiver<Report>>) {
        let mut report = Report::new();
        let (mut now, mut later) = (Vec::new(), Vec::new());
        let mut rx = None;
        {
            let mut c = self.core.lock().unwrap();
            c.refused.clear();
            let allowlist: Vec<PathBuf> = c.settings.allowlist.iter().map(|a| dunce::canonicalize(a).unwrap_or_else(|_| a.clone())).collect();
            let mut refused = Vec::new();
            {
                let gate = Gate { allowlist: &allowlist, scan_root: c.root.as_deref(), protected: &self.deps.protected, home: &self.deps.home };
                for p in paths {
                    match actions::resolve(&p) {
                        Err(e) => {
                            refused.push(p.clone());
                            report.push((p, ItemStatus::Refused(e.to_string())));
                        }
                        Ok(p) => match actions::decide(&p, origin, &gate) {
                            Decision::Refused(why) => {
                                refused.push(p.clone());
                                report.push((p, ItemStatus::Refused(why.into())));
                            }
                            Decision::Immediate => now.push(p),
                            Decision::NeedsConfirm => later.push(p),
                        },
                    }
                }
            }
            if origin == Origin::User {
                c.refused = refused;
            }
            if !later.is_empty() {
                if c.pending.is_some() {
                    report.extend(later.into_iter().map(|p| (p, ItemStatus::Refused("another confirmation is already open".into()))));
                } else {
                    let (tx, r) = oneshot::channel();
                    let items = later.iter().map(|p| (p.clone(), size_in_tree(&c, p))).collect();
                    report.extend(later.into_iter().map(|p| (p, ItemStatus::Pending)));
                    c.pending = Some(Pending { items, reason, origin, reply: Some(tx), system: Vec::new(), system_reply: None });
                    rx = Some(r);
                }
            }
        }
        // The (slow) trasher runs without the core lock so the UI keeps rendering.
        report.extend(self.trash_now(&now));
        self.changed();
        (report, rx)
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

    /// Store a check result unless the item started running meanwhile.
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
    fn run_systems(self: &Arc<Self>, reqs: Vec<SystemRequest>, reply: Option<oneshot::Sender<SystemOutcome>>) {
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

    pub fn resolve_pending(self: &Arc<Self>, accept: bool) -> Report {
        let mut c = self.core.lock().unwrap();
        let Some(mut pending) = c.pending.take() else { return Report::new() };
        if accept {
            for req in &pending.system {
                set_status(&mut c, req.cleaner, req.repo.as_deref(), SystemStatus::Running);
            }
        }
        drop(c);
        if !pending.system.is_empty() {
            let reqs = std::mem::take(&mut pending.system);
            let reply = pending.system_reply.take();
            if accept {
                self.run_systems(reqs, reply);
            } else if let Some(tx) = reply {
                let _ = tx.send(SystemOutcome::Declined);
            }
            self.changed();
            return Report::new();
        }
        let paths: Vec<PathBuf> = pending.items.iter().map(|(p, _)| p.clone()).collect();
        let report = if accept { self.trash_now(&paths) } else { paths.into_iter().map(|p| (p, ItemStatus::Declined)).collect() };
        if let Some(tx) = pending.reply.take() {
            let _ = tx.send(report.clone());
        }
        self.changed();
        report
    }

    /// Calls the trasher without holding `core`, then records the outcome under the lock.
    fn trash_now(&self, paths: &[PathBuf]) -> Report {
        if paths.is_empty() {
            return Report::new();
        }
        let results = (self.deps.trasher)(paths);
        let mut c = self.core.lock().unwrap();
        let (mut count, mut bytes) = (0usize, 0u64);
        let mut report = Report::new();
        for r in results {
            match r.error {
                None => {
                    bytes += size_in_tree(&c, &r.path).unwrap_or(0);
                    count += 1;
                    forget(&mut c, &r.path);
                    if c.scanning.is_some() {
                        c.trashed_during_scan.push(r.path.clone());
                    }
                    report.push((r.path, ItemStatus::Trashed));
                }
                Some(e) => {
                    c.trash_errors.push((r.path.clone(), e.clone()));
                    report.push((r.path, ItemStatus::Failed(e)));
                }
            }
        }
        if count > 0 {
            c.toasts.push((count, bytes));
            let files = c.stats.map_or(0, |s| s.files);
            c.stats = c.tree.as_ref().map(|t| stats_of(t, files));
        }
        report
    }

    pub fn update_settings(&self, f: impl FnOnce(&mut Settings)) {
        let mut c = self.core.lock().unwrap();
        f(&mut c.settings);
        c.settings_error = c.settings.save(&self.settings_path).err().map(|e| e.to_string());
        drop(c);
        self.changed();
    }

    pub fn set_mcp_error(&self, e: String) {
        self.core.lock().unwrap().mcp_error = Some(e);
        self.changed();
    }
}

fn set_status(c: &mut Core, cleaner: Cleaner, repo: Option<&Path>, status: SystemStatus) {
    c.system.insert(cleaners::item_id(cleaner, repo), SystemItem { cleaner, repo: repo.map(Path::to_path_buf), status });
}

/// Worktrees outside the scanned folder have no known size: count them instead of showing 0 bytes.
fn worktree_estimate(bytes: u64, count: usize) -> Estimate {
    if bytes == 0 && count > 0 { Estimate::Count(count as u64) } else { Estimate::Bytes(bytes) }
}

fn size_in_tree(c: &Core, abs: &Path) -> Option<u64> {
    c.rel(abs).and_then(|r| c.tree.as_ref()?.find(&r).map(|n| n.size))
}

/// Drop a trashed path from the tree, the findings and the view.
fn forget(c: &mut Core, abs: &Path) {
    let Some(rel) = c.rel(abs) else { return };
    let removed = c.tree.as_mut().and_then(|t| Arc::make_mut(t).remove(&rel)).unwrap_or(0);
    c.findings.retain(|f| !f.path.starts_with(abs));
    for f in c.findings.iter_mut().filter(|f| abs.starts_with(&f.path)) {
        f.size = f.size.saturating_sub(removed);
    }
    c.findings.retain(|f| f.size > 0);
    if c.zoom.starts_with(&rel) {
        c.zoom = rel.parent().map(Path::to_path_buf).unwrap_or_default();
    }
    if c.selected.as_ref().is_some_and(|s| s.starts_with(&rel)) {
        c.selected = None;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::cleaners::Runner;
    use std::fs;

    /// A scanned temp tree:
    ///   cache/a.bin (40 KB)   – allowlisted folder
    ///   docs/b.bin  (20 KB)
    ///   node_modules/c.bin (10 KB)
    pub(crate) struct Fixture {
        pub _tmp: tempfile::TempDir,
        pub base: PathBuf,
        pub app: Arc<Shared>,
        pub _rx: async_channel::Receiver<()>,
        pub _trash: tempfile::TempDir,
    }

    fn fake_trash(paths: &[PathBuf]) -> Vec<TrashResult> {
        paths
            .iter()
            .map(|p| TrashResult { path: p.clone(), error: fs::remove_file(p).or_else(|_| fs::remove_dir_all(p)).err().map(|e| e.to_string()) })
            .collect()
    }

    pub(crate) async fn fixture() -> Fixture {
        fixture_with(default_runner()).await
    }

    pub(crate) async fn fixture_with(runner: Runner) -> Fixture {
        let trash = tempfile::tempdir().unwrap();
        fs::write(trash.path().join("junk.bin"), vec![0u8; 1000]).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        for (rel, len) in [("cache/a.bin", 40_000), ("docs/b.bin", 20_000), ("node_modules/c.bin", 10_000)] {
            fs::create_dir_all(base.join(rel).parent().unwrap()).unwrap();
            fs::write(base.join(rel), vec![1u8; len]).unwrap();
        }
        let mut settings = Settings::default();
        settings.allowlist.push(base.join("cache"));
        let deps = Deps {
            rules: Rules::parse(include_str!("../rules/catalog.toml"), &base, "linux").unwrap(),
            trasher: fake_trash,
            home: PathBuf::from("/nonexistent-home"),
            protected: Vec::new(),
            runner,
            trash_dir: Some(trash.path().to_path_buf()),
        };
        let (tx, rx) = async_channel::bounded(1);
        let app = Shared::build(settings, false, base.join("settings.toml"), tx, deps);
        app.start_scan(base.clone()).await.unwrap().unwrap();
        Fixture { _tmp: tmp, base, app, _rx: rx, _trash: trash }
    }

    #[tokio::test]
    async fn scan_fills_tree_findings_and_recent() {
        let f = fixture().await;
        let c = f.app.core.lock().unwrap();
        assert_eq!(c.home_state(), HomeState::Complete);
        assert_eq!(c.screen, Screen::Home);
        let s = c.stats.unwrap();
        assert_eq!(s.files, 3);
        assert_eq!(s.bytes, c.tree.as_ref().unwrap().size);
        assert!(s.largest_file >= 40_000);
        assert_eq!(c.tree.as_ref().unwrap().children.len(), 3);
        assert_eq!(c.findings[0].rule_id, "node_modules");
        assert_eq!(c.settings.recent[0].path, f.base);
        assert!(f.base.join("settings.toml").exists());
        assert_eq!(c.banner(), Banner::Found(c.findings[0].size));
    }

    #[tokio::test]
    async fn agent_trash_in_allowlist_is_immediate() {
        let f = fixture().await;
        let (report, rx) = f.app.request_trash(vec![f.base.join("cache/a.bin")], Some("cache".into()), Origin::Agent);
        assert_eq!(report, vec![(f.base.join("cache/a.bin"), ItemStatus::Trashed)]);
        assert!(rx.is_none());
        assert!(!f.base.join("cache/a.bin").exists());
        let c = f.app.core.lock().unwrap();
        assert_eq!(c.tree.as_ref().unwrap().find(Path::new("cache")).unwrap().size, 0);
    }

    #[tokio::test]
    async fn agent_trash_outside_allowlist_waits_for_user() {
        let f = fixture().await;
        let target = f.base.join("docs/b.bin");
        let (report, rx) = f.app.request_trash(vec![target.clone()], Some("old".into()), Origin::Agent);
        assert_eq!(report, vec![(target.clone(), ItemStatus::Pending)]);
        assert!(target.exists());
        assert_eq!(f.app.core.lock().unwrap().banner(), Banner::Confirm);
        assert_eq!(f.app.resolve_pending(true), vec![(target.clone(), ItemStatus::Trashed)]);
        assert_eq!(rx.unwrap().await.unwrap(), vec![(target.clone(), ItemStatus::Trashed)]);
        assert!(!target.exists());
        assert!(f.app.core.lock().unwrap().pending.is_none());
    }

    #[tokio::test]
    async fn declined_and_second_request_while_pending() {
        let f = fixture().await;
        let (_, rx) = f.app.request_trash(vec![f.base.join("docs/b.bin")], None, Origin::Agent);
        let (second, rx2) = f.app.request_trash(vec![f.base.join("node_modules")], None, Origin::Agent);
        assert!(matches!(second[0].1, ItemStatus::Refused(_)));
        assert!(rx2.is_none());
        assert_eq!(f.app.core.lock().unwrap().pending.as_ref().unwrap().items.len(), 1);
        f.app.resolve_pending(false);
        assert_eq!(rx.unwrap().await.unwrap()[0].1, ItemStatus::Declined);
        assert!(f.base.join("docs/b.bin").exists());
    }

    #[tokio::test]
    async fn path_tricks_never_skip_confirmation() {
        let f = fixture().await;
        let sneaky = f.base.join("cache/../docs/b.bin");
        let (report, rx) = f.app.request_trash(vec![sneaky, PathBuf::from("relative/x")], None, Origin::Agent);
        assert!(report.contains(&(f.base.join("docs/b.bin"), ItemStatus::Pending)));
        assert!(report.iter().any(|(p, s)| p == Path::new("relative/x") && matches!(s, ItemStatus::Refused(_))));
        assert!(rx.is_some());
        assert!(f.base.join("docs/b.bin").exists());
    }

    #[tokio::test]
    async fn scan_root_cannot_be_trashed() {
        let f = fixture().await;
        let (report, _) = f.app.request_trash(vec![f.base.clone()], None, Origin::User);
        assert!(matches!(report[0].1, ItemStatus::Refused(_)));
        assert_eq!(f.app.core.lock().unwrap().refused, vec![f.base.clone()]);
        // Agent refusals are reported to the agent only, not to the user's banner.
        let (_, _) = f.app.request_trash(vec![f.base.clone()], None, Origin::Agent);
        assert!(f.app.core.lock().unwrap().refused.is_empty());
    }

    #[tokio::test]
    async fn pending_size_is_none_outside_the_scan() {
        let f = fixture().await;
        let other = tempfile::tempdir().unwrap();
        let outside = dunce::canonicalize(other.path()).unwrap().join("x.bin");
        std::fs::write(&outside, b"hi").unwrap();
        let inside = f.base.join("docs/b.bin");
        let (_, rx) = f.app.request_trash(vec![outside.clone(), inside.clone()], Some("r".into()), Origin::Agent);
        assert!(rx.is_some());
        let c = f.app.core.lock().unwrap();
        let items = &c.pending.as_ref().unwrap().items;
        assert_eq!(items.iter().find(|(p, _)| *p == outside).unwrap().1, None);
        assert!(items.iter().find(|(p, _)| *p == inside).unwrap().1.is_some_and(|s| s >= 20_000));
    }

    #[tokio::test]
    async fn scanning_a_missing_folder_reports_it_as_interrupted() {
        let f = fixture().await;
        assert!(f.app.start_scan(f.base.join("gone")).await.unwrap().is_err());
        let c = f.app.core.lock().unwrap();
        assert_eq!(c.home_state(), HomeState::Interrupted);
        assert!(c.interrupted.as_ref().unwrap().error.is_some());
        assert!(c.tree.is_some()); // previous results kept
    }

    #[tokio::test]
    async fn trash_resets_zoom_and_shrinks_findings() {
        let f = fixture().await;
        f.app.navigate(&f.base.join("node_modules/c.bin")).unwrap();
        {
            let c = f.app.core.lock().unwrap();
            assert_eq!(c.zoom, PathBuf::from("node_modules"));
            assert_eq!(c.selected, Some(PathBuf::from("node_modules/c.bin")));
        }
        let (_, _rx) = f.app.request_trash(vec![f.base.join("node_modules/c.bin")], None, Origin::User);
        f.app.resolve_pending(true);
        let c = f.app.core.lock().unwrap();
        assert_eq!(c.selected, None);
        assert_eq!(c.zoom, PathBuf::from("node_modules"));
        assert!(c.findings.iter().all(|x| x.size > 0));
        assert!(c.findings.iter().all(|x| x.rule_id != "node_modules"));
    }

    #[tokio::test]
    async fn trash_during_scan_is_not_resurrected() {
        let f = fixture().await;
        let target = f.base.join("cache/a.bin");
        let rx = f.app.start_scan(f.base.clone());
        let (_, _rx) = f.app.request_trash(vec![target.clone()], None, Origin::Agent);
        f.app.resolve_pending(true);
        let _ = rx.await;
        let c = f.app.core.lock().unwrap();
        assert!(c.tree.as_ref().unwrap().find(Path::new("cache/a.bin")).is_none());
        assert!(c.findings.iter().all(|x| x.path != target));
    }

    #[tokio::test]
    async fn refresh_picks_up_changes_and_keeps_zoom() {
        let f = fixture().await;
        f.app.navigate(&f.base.join("docs")).unwrap();
        fs::write(f.base.join("docs/new.bin"), vec![1u8; 50_000]).unwrap();
        f.app.refresh(None).await.unwrap().unwrap();
        let c = f.app.core.lock().unwrap();
        assert_eq!(c.zoom, PathBuf::from("docs"));
        assert!(c.tree.as_ref().unwrap().find(Path::new("docs/new.bin")).is_some());
        assert_eq!(&*c.tree.as_ref().unwrap().children[0].name, "docs");
    }

    #[tokio::test]
    async fn refresh_of_vanished_folder_removes_it() {
        let f = fixture().await;
        fs::remove_dir_all(f.base.join("docs")).unwrap();
        f.app.refresh(Some(f.base.join("docs"))).await.unwrap().unwrap();
        assert!(f.app.core.lock().unwrap().tree.as_ref().unwrap().find(Path::new("docs")).is_none());
    }

    #[tokio::test]
    async fn refresh_superseded_by_new_scan_is_discarded() {
        let f = fixture().await;
        let r1 = f.app.refresh(Some(f.base.join("docs")));
        let r2 = f.app.start_scan(f.base.clone());
        let _ = r1.await;
        r2.await.unwrap().unwrap();
        let c = f.app.core.lock().unwrap();
        // 3 fixture folders + settings.toml written by the first scan
        assert_eq!(c.tree.as_ref().unwrap().children.len(), 4);
        assert_eq!(c.home_state(), HomeState::Complete);
        assert!(!c.busy);
    }

    #[tokio::test]
    async fn settings_save_failure_is_reported() {
        let f = fixture().await;
        let blocker = f.base.join("blocker");
        fs::write(&blocker, "x").unwrap();
        let (tx, _rx) = async_channel::bounded(1);
        let deps = Deps { rules: Rules::builtin(), trasher: fake_trash, home: PathBuf::from("/nonexistent-home"), protected: Vec::new(), runner: default_runner(), trash_dir: None };
        let app = Shared::build(Settings::default(), false, blocker.join("sub/settings.toml"), tx, deps);
        app.update_settings(|s| s.mcp_port = 1);
        assert!(app.core.lock().unwrap().settings_error.is_some());
    }

    #[tokio::test]
    async fn refresh_rejects_dotdot() {
        let f = fixture().await;
        assert!(f.app.refresh(Some(f.base.join("docs/../.."))).await.unwrap().is_err());
    }

    #[tokio::test]
    async fn navigate_outside_scan_is_an_error() {
        let f = fixture().await;
        assert!(f.app.navigate(Path::new("/elsewhere")).is_err());
        assert!(f.app.select(&f.base.join("nope")).is_err());
    }

    #[tokio::test]
    async fn cancel_scan_keeps_partial_counts_and_old_results() {
        let f = fixture().await;
        {
            let mut c = f.app.core.lock().unwrap();
            c.scanning = Some(f.base.join("docs"));
            c.busy = true;
            c.progress.files.store(7, Relaxed);
            c.progress.bytes.store(700, Relaxed);
        }
        f.app.cancel_scan();
        {
            let c = f.app.core.lock().unwrap();
            assert_eq!(c.home_state(), HomeState::Interrupted);
            assert_eq!(c.interrupted, Some(Interrupted { root: f.base.join("docs"), files: 7, bytes: 700, error: None }));
            assert!(c.tree.is_some() && !c.busy && c.scanning.is_none());
        }
        f.app.dismiss_interrupted();
        assert_eq!(f.app.core.lock().unwrap().home_state(), HomeState::Complete);
    }

    #[tokio::test]
    async fn new_scan_keeps_previous_results_until_done() {
        let f = fixture().await;
        let rx = f.app.start_scan(f.base.join("docs"));
        assert!(f.app.core.lock().unwrap().tree.is_some());
        rx.await.unwrap().unwrap();
        let c = f.app.core.lock().unwrap();
        assert_eq!(c.root.as_deref(), Some(f.base.join("docs").as_path()));
        assert_eq!(c.home_state(), HomeState::Complete);
    }

    #[tokio::test]
    async fn show_requires_a_scan() {
        let f = fixture().await;
        f.app.show(Screen::Cleanup);
        assert_eq!(f.app.core.lock().unwrap().screen, Screen::Cleanup);
        f.app.core.lock().unwrap().tree = None;
        f.app.show(Screen::DiskMap);
        assert_eq!(f.app.core.lock().unwrap().screen, Screen::Home);
        f.app.show(Screen::Settings);
        assert_eq!(f.app.core.lock().unwrap().screen, Screen::Settings);
        // System tools don't need a scan, so Cleanup opens without one.
        f.app.show(Screen::Cleanup);
        assert_eq!(f.app.core.lock().unwrap().screen, Screen::Cleanup);
    }

    #[tokio::test]
    async fn system_tools_shortcut_puts_them_first_until_the_next_screen_change() {
        let f = fixture().await;
        f.app.show_system_tools();
        {
            let c = f.app.core.lock().unwrap();
            assert_eq!(c.screen, Screen::Cleanup);
            assert!(c.system_first);
        }
        f.app.show(Screen::Home);
        assert!(!f.app.core.lock().unwrap().system_first);
    }

    #[tokio::test]
    async fn trash_pushes_a_toast_and_updates_stats() {
        let f = fixture().await;
        let before = f.app.core.lock().unwrap().stats.unwrap();
        f.app.request_trash(vec![f.base.join("cache/a.bin")], None, Origin::Agent);
        let c = f.app.core.lock().unwrap();
        assert_eq!(c.toasts.len(), 1);
        assert_eq!(c.toasts[0].0, 1);
        assert!(c.toasts[0].1 >= 40_000);
        let after = c.stats.unwrap();
        assert!(after.bytes < before.bytes);
        assert!(after.largest_file < before.largest_file);
    }

    #[tokio::test]
    async fn navigate_opens_the_disk_map() {
        let f = fixture().await;
        f.app.navigate(&f.base.join("docs")).unwrap();
        assert_eq!(f.app.core.lock().unwrap().screen, Screen::DiskMap);
    }

    #[tokio::test]
    async fn failed_scan_request_does_not_outlive_a_successful_scan() {
        let f = fixture().await;
        let rx = f.app.start_scan(f.base.join("docs"));
        let _ = f.app.start_scan(f.base.join("missing")).await;
        rx.await.unwrap().unwrap();
        assert_eq!(f.app.core.lock().unwrap().home_state(), HomeState::Complete);
    }

    #[tokio::test]
    async fn cancel_discards_the_running_scan_result() {
        let f = fixture().await;
        let rx = f.app.start_scan(f.base.join("docs"));
        f.app.cancel_scan();
        let _ = rx.await;
        let c = f.app.core.lock().unwrap();
        assert!(c.tree.is_some());
        if c.interrupted.is_some() {
            assert_eq!(c.root.as_deref(), Some(f.base.as_path()));
        }
    }

    use crate::cleaners::CmdOutput;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    /// brew: 1 MiB to free (0 after a run); docker/xcrun/git not installed; tmutil: no snapshots.
    pub(crate) fn default_runner() -> Runner {
        let cleaned = Arc::new(AtomicBool::new(false));
        Arc::new(move |c| match (c.program, c.args.first().map(String::as_str)) {
            ("brew", _) if c.args.iter().any(|a| a == "--dry-run") => CmdOutput::Done {
                ok: true,
                stdout: if cleaned.load(Relaxed) { String::new() } else { "==> This operation would free approximately 1MB of disk space.".into() },
                stderr: String::new(),
            },
            ("brew", Some("cleanup")) => {
                cleaned.store(true, Relaxed);
                CmdOutput::Done { ok: true, stdout: String::new(), stderr: String::new() }
            }
            ("tmutil", _) => CmdOutput::Done { ok: true, stdout: "Snapshots for disk /:\n".into(), stderr: String::new() },
            ("xcode-select", _) => CmdOutput::Done { ok: true, stdout: "/Library/Developer/CommandLineTools\n".into(), stderr: String::new() },
            _ => CmdOutput::NotFound,
        })
    }

    async fn settled(app: &Arc<Shared>) {
        for _ in 0..200 {
            if !app.core.lock().unwrap().system.values().any(|i| matches!(i.status, SystemStatus::Checking | SystemStatus::Running)) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("system checks did not settle");
    }

    #[tokio::test]
    async fn opening_cleanup_checks_system_tools() {
        let f = fixture().await;
        f.app.show(Screen::Cleanup);
        settled(&f.app).await;
        let c = f.app.core.lock().unwrap();
        assert_eq!(c.system["homebrew"].status, SystemStatus::Ready { estimate: Estimate::Bytes(1_048_576), detail: vec![] });
        assert_eq!(c.system["docker"].status, SystemStatus::Unavailable { reason: "not_installed".into() });
        assert!(matches!(c.system["empty_trash"].status, SystemStatus::Ready { estimate: Estimate::Bytes(n), .. } if n >= 1000));
        assert_eq!(c.system.contains_key("tm_snapshots"), cfg!(target_os = "macos"));
    }

    #[tokio::test]
    async fn accepted_cleanup_runs_rechecks_and_toasts() {
        let f = fixture().await;
        f.app.check_system();
        settled(&f.app).await;
        let rx = f.app.request_system_cleanup("homebrew", Some("old bottles".into()), Origin::Agent).unwrap();
        {
            let c = f.app.core.lock().unwrap();
            let p = c.pending.as_ref().unwrap();
            assert_eq!(p.system[0].cleaner, Cleaner::Homebrew);
            assert!(p.items.is_empty());
        }
        f.app.resolve_pending(true);
        assert_eq!(rx.await.unwrap(), SystemOutcome::Done { freed: Some(1_048_576) });
        let c = f.app.core.lock().unwrap();
        assert_eq!(c.system["homebrew"].status, SystemStatus::Ready { estimate: Estimate::Bytes(0), detail: vec![] });
        assert_eq!(c.system_toasts, vec![(Cleaner::Homebrew, None, Some(1_048_576))]);
    }

    #[tokio::test]
    async fn declined_cleanup_never_runs() {
        let runs = Arc::new(AtomicUsize::new(0));
        let count = runs.clone();
        let inner = default_runner();
        let f = fixture_with(Arc::new(move |c| {
            if c.args.first().is_some_and(|a| a == "cleanup") && !c.args.iter().any(|a| a == "--dry-run") {
                count.fetch_add(1, Relaxed);
            }
            inner(c)
        }))
        .await;
        f.app.check_system();
        settled(&f.app).await;
        let rx = f.app.request_system_cleanup("homebrew", None, Origin::User).unwrap();
        f.app.resolve_pending(false);
        assert_eq!(rx.await.unwrap(), SystemOutcome::Declined);
        assert_eq!(runs.load(Relaxed), 0);
    }

    #[tokio::test]
    async fn system_requests_are_refused_when_not_ready_or_busy() {
        let f = fixture().await;
        assert!(f.app.request_system_cleanup("homebrew", None, Origin::Agent).is_err()); // not checked yet
        f.app.check_system();
        settled(&f.app).await;
        assert!(f.app.request_system_cleanup("docker", None, Origin::Agent).is_err()); // unavailable
        assert!(f.app.request_system_cleanup("nope", None, Origin::Agent).is_err()); // unknown
        if cfg!(target_os = "macos") {
            assert!(f.app.request_system_cleanup("tm_snapshots", None, Origin::Agent).is_err()); // nothing to clean
        }
        let _rx = f.app.request_trash(vec![f.base.join("docs/b.bin")], None, Origin::Agent);
        assert!(f.app.request_system_cleanup("homebrew", None, Origin::Agent).is_err()); // a confirmation is open
    }

    #[tokio::test]
    async fn decline_only_hits_its_own_request() {
        let f = fixture().await;
        f.app.check_system();
        settled(&f.app).await;
        let _trash = f.app.request_trash(vec![f.base.join("docs/b.bin")], None, Origin::Agent);
        assert!(!f.app.decline_system_if_pending("homebrew"));
        assert!(f.app.core.lock().unwrap().pending.is_some());
        f.app.resolve_pending(false);
        let rx = f.app.request_system_cleanup("homebrew", None, Origin::Agent).unwrap();
        assert!(f.app.decline_system_if_pending("homebrew"));
        assert_eq!(rx.await.unwrap(), SystemOutcome::Declined);
        assert!(f.app.core.lock().unwrap().pending.is_none());
    }

    #[tokio::test]
    async fn recheck_during_run_keeps_running() {
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let inner = default_runner();
        let f = fixture_with(Arc::new(move |c| {
            if c.program == "brew" && !c.args.iter().any(|a| a == "--dry-run") {
                let _ = release_rx.lock().unwrap().recv();
            }
            inner(c)
        }))
        .await;
        f.app.check_system();
        settled_checks(&f.app).await;
        let rx = f.app.request_system_cleanup("homebrew", None, Origin::User).unwrap();
        f.app.resolve_pending(true);
        assert_eq!(f.app.core.lock().unwrap().system["homebrew"].status, SystemStatus::Running);
        f.app.check_system();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(f.app.core.lock().unwrap().system["homebrew"].status, SystemStatus::Running);
        assert!(f.app.request_system_cleanup("homebrew", None, Origin::User).is_err());
        release_tx.send(()).unwrap();
        assert!(matches!(rx.await.unwrap(), SystemOutcome::Done { .. }));
    }

    async fn settled_checks(app: &Arc<Shared>) {
        for _ in 0..200 {
            if !app.core.lock().unwrap().system.values().any(|i| matches!(i.status, SystemStatus::Checking)) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        panic!("checks did not settle");
    }

    #[tokio::test]
    async fn failed_run_reports_error() {
        let inner = default_runner();
        let f = fixture_with(Arc::new(move |c| {
            if c.program == "brew" && !c.args.iter().any(|a| a == "--dry-run") {
                CmdOutput::Done { ok: false, stdout: String::new(), stderr: "Error: permission denied\n".into() }
            } else {
                inner(c)
            }
        }))
        .await;
        f.app.check_system();
        settled(&f.app).await;
        let rx = f.app.request_system_cleanup("homebrew", None, Origin::User).unwrap();
        f.app.resolve_pending(true);
        assert_eq!(rx.await.unwrap(), SystemOutcome::Failed { error: "Error: permission denied".into() });
        assert_eq!(f.app.core.lock().unwrap().system["homebrew"].status, SystemStatus::Failed { error: "Error: permission denied".into() });
    }

    #[test]
    fn worktrees_outside_the_scan_are_counted() {
        assert_eq!(worktree_estimate(0, 2), Estimate::Count(2));
        assert_eq!(worktree_estimate(5000, 2), Estimate::Bytes(5000));
        assert_eq!(worktree_estimate(0, 0), Estimate::Bytes(0));
    }

    /// Fake git: `<base>/repo` has a merged, clean worktree `<base>/wt`; `worktree remove` deletes it.
    fn git_runner(base: Arc<std::sync::Mutex<PathBuf>>) -> Runner {
        let inner = default_runner();
        Arc::new(move |c| {
            if c.program != "git" {
                return inner(c);
            }
            let ok = |out: String| CmdOutput::Done { ok: true, stdout: out, stderr: String::new() };
            let base = base.lock().unwrap().clone();
            match c.args[4].as_str() {
                "symbolic-ref" => ok("origin/main\n".into()),
                "for-each-ref" => ok("main\nwt\n".into()),
                "worktree" if c.args[5] == "list" => ok(format!("worktree {}\nbranch refs/heads/main\n\nworktree {}\nbranch refs/heads/wt\n", base.join("repo").display(), base.join("wt").display())),
                "worktree" if c.args[5] == "remove" => {
                    fs::remove_dir_all(&c.args[6]).unwrap();
                    ok(String::new())
                }
                _ => ok(String::new()), // status (clean), worktree prune
            }
        })
    }

    /// Fake git with merged branches old-a, old-b, old-c in every repo; records `branch -d` calls.
    fn branch_runner(deleted: Arc<std::sync::Mutex<Vec<(String, Vec<String>)>>>) -> Runner {
        let inner = default_runner();
        Arc::new(move |c| {
            if c.program != "git" {
                return inner(c);
            }
            let ok = |out: &str| CmdOutput::Done { ok: true, stdout: out.to_string(), stderr: String::new() };
            match c.args[4].as_str() {
                "symbolic-ref" => ok("origin/main\n"),
                "for-each-ref" => ok("main\nold-a\nold-b\nold-c\n"),
                "worktree" => ok(&format!("worktree {}\nbranch refs/heads/main\n", c.args[3])),
                "branch" => {
                    deleted.lock().unwrap().push((c.args[3].clone(), c.args[6..].to_vec()));
                    ok("")
                }
                _ => ok(""),
            }
        })
    }

    #[tokio::test]
    async fn picked_branches_across_repos_run_in_one_confirmation_and_nothing_else() {
        let deleted = Arc::new(std::sync::Mutex::new(Vec::new()));
        let f = fixture_with(branch_runner(deleted.clone())).await;
        for r in ["one", "two"] {
            fs::create_dir_all(f.base.join(r).join(".git")).unwrap();
        }
        f.app.start_scan(f.base.clone()).await.unwrap().unwrap();
        f.app.check_system();
        settled(&f.app).await;
        let id = |r: &str| cleaners::item_id(Cleaner::GitBranches, Some(&f.base.join(r)));
        // Something the last check did not offer is refused outright.
        assert!(f.app.request_system_batch(vec![(id("one"), Some(vec!["main".into()]))], None, Origin::User).is_err());
        assert!(f.app.request_system_batch(vec![(id("one"), Some(vec!["old-a".into(), "evil".into()]))], None, Origin::User).is_err());
        assert!(f.app.core.lock().unwrap().pending.is_none());
        let rx = f.app.request_system_batch(vec![(id("one"), Some(vec!["old-a".into(), "old-c".into()])), (id("two"), Some(vec!["old-b".into()]))], None, Origin::User).unwrap();
        assert_eq!(f.app.core.lock().unwrap().pending.as_ref().unwrap().system.len(), 2);
        f.app.resolve_pending(true);
        assert!(matches!(rx.await.unwrap(), SystemOutcome::Done { .. }));
        let mut calls = deleted.lock().unwrap().clone();
        calls.sort();
        let repo = |r: &str| f.base.join(r).display().to_string();
        assert_eq!(calls, vec![(repo("one"), vec!["old-a".into(), "old-c".into()]), (repo("two"), vec!["old-b".into()])]);
    }

    #[tokio::test]
    async fn rescan_rechecks_git_and_removed_worktrees_leave_the_tree() {
        let base_cell = Arc::new(std::sync::Mutex::new(PathBuf::new()));
        let f = fixture_with(git_runner(base_cell.clone())).await;
        *base_cell.lock().unwrap() = f.base.clone();
        f.app.check_system();
        settled(&f.app).await;
        assert!(f.app.core.lock().unwrap().system.values().all(|i| i.repo.is_none())); // no repo in the old tree
        fs::create_dir_all(f.base.join("repo/.git")).unwrap();
        fs::create_dir_all(f.base.join("wt")).unwrap();
        fs::write(f.base.join("wt/big.bin"), vec![1u8; 5000]).unwrap();
        f.app.start_scan(f.base.clone()).await.unwrap().unwrap();
        settled(&f.app).await;
        let id = cleaners::item_id(Cleaner::GitWorktrees, Some(&f.base.join("repo")));
        {
            let c = f.app.core.lock().unwrap();
            assert!(matches!(&c.system[&id].status, SystemStatus::Ready { estimate: Estimate::Bytes(n), detail } if *n >= 5000 && detail == &vec![f.base.join("wt").display().to_string()]));
        }
        let rx = f.app.request_system_cleanup(&id, None, Origin::User).unwrap();
        f.app.resolve_pending(true);
        assert!(matches!(rx.await.unwrap(), SystemOutcome::Done { .. }));
        assert!(!f.base.join("wt").exists());
        let c = f.app.core.lock().unwrap();
        assert!(c.tree.as_ref().unwrap().find(Path::new("wt")).is_none());
    }
}
