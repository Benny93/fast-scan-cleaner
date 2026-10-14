//! Scanning and moving around in the scanned tree.

use super::*;

impl Shared {
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
            if let Dupes::Running(p) = &c.dupes {
                p.cancel.store(true, Relaxed);
            }
            c.dupes = Dupes::Idle;
            c.interrupted = None;
            c.busy = true;
            c.screen = Screen::Home;
            c.trash_errors.clear();
        }
        self.changed();
        let me = self.clone();
        std::thread::spawn(move || {
            let result = scan::scan(&path, &progress);
            // Written before taking the lock: file I/O must not stall the UI.
            let history = match (&result, &me.deps.history_dir) {
                // A cancelled scan returns a partial tree: never record it.
                (Ok(tree), Some(dir)) if !progress.cancel.load(Relaxed) => Some(crate::scan::snapshots::record(dir, &path, crate::scan::snapshots::take(tree, now_secs()))),
                _ => None,
            };
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
                    c.history = history.unwrap_or_default();
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
}
