//! Moving files to the Trash, and the confirmation that guards it and the system cleanups.

use super::*;

impl Shared {
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
            // Each path once, and nothing inside another requested path (it goes with that one),
            // e.g. a leftover two ticked apps share or a cache folder that is also a cleanup finding.
            let all: Vec<PathBuf> = now.iter().chain(&later).cloned().collect();
            let covered = |p: &PathBuf| all.iter().any(|q| q != p && p.starts_with(q));
            let mut seen = std::collections::HashSet::new();
            now.retain(|p| !covered(p) && seen.insert(p.clone()));
            later.retain(|p| !covered(p) && seen.insert(p.clone()));
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
        // Sizes before the move: model files outside the scan are measured on disk.
        let sizes: std::collections::HashMap<PathBuf, u64> = {
            let c = self.core.lock().unwrap();
            paths.iter().map(|p| (p.clone(), size_in_tree(&c, p).unwrap_or(0))).collect()
        };
        let results = (self.deps.trasher)(paths);
        let mut c = self.core.lock().unwrap();
        let (mut count, mut bytes) = (0usize, 0u64);
        let mut report = Report::new();
        for r in results {
            match r.error {
                None => {
                    bytes += sizes.get(&r.path).copied().unwrap_or(0);
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
}
