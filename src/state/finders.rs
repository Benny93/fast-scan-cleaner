//! Background finders: duplicates, unused apps, old files, AI models.

use super::*;

impl Shared {
    /// Store a check result unless the item started running meanwhile.
    /// Search the scanned folder for identical files in the background.
    pub fn start_dupes(self: &Arc<Self>) -> Result<(), &'static str> {
        let (cands, progress) = {
            let mut c = self.core.lock().unwrap();
            // During a scan the old tree is still shown; its results would outlive the scan.
            if c.scanning.is_some() {
                return Err("a scan is running");
            }
            let (Some(tree), Some(root)) = (&c.tree, &c.root) else { return Err("nothing scanned yet") };
            if matches!(c.dupes, Dupes::Running(_)) {
                return Ok(());
            }
            let cands = crate::finders::dupes::candidates(tree, root);
            let progress = Arc::new(crate::finders::dupes::DupeProgress::default());
            c.dupes = Dupes::Running(progress.clone());
            (cands, progress)
        };
        self.changed();
        let running = |me: &Shared, p: &Arc<crate::finders::dupes::DupeProgress>| matches!(&me.core.lock().unwrap().dupes, Dupes::Running(q) if Arc::ptr_eq(q, p));
        // Redraw the progress a few times a second while this search is the current one.
        let (ticker, p) = (self.clone(), progress.clone());
        std::thread::spawn(move || {
            while running(&ticker, &p) {
                std::thread::sleep(std::time::Duration::from_millis(250));
                ticker.changed();
            }
        });
        let me = self.clone();
        std::thread::spawn(move || {
            let groups = crate::finders::dupes::find(cands, &progress);
            let mut c = me.core.lock().unwrap();
            if matches!(&c.dupes, Dupes::Running(p) if Arc::ptr_eq(p, &progress)) {
                c.dupes = groups.map_or(Dupes::Idle, Dupes::Done);
            }
            drop(c);
            me.changed();
        });
        Ok(())
    }

    pub fn cancel_dupes(&self) {
        let mut c = self.core.lock().unwrap();
        if let Dupes::Running(p) = &c.dupes {
            p.cancel.store(true, Relaxed);
        }
        c.dupes = Dupes::Idle;
        drop(c);
        self.changed();
    }

    /// Look for apps not opened for a long time, in the background, once per run.
    pub fn load_apps(self: &Arc<Self>) {
        if self.deps.app_dirs.is_empty() {
            let mut c = self.core.lock().unwrap();
            if matches!(c.apps, Load::Idle) {
                c.apps = Load::Unavailable;
            }
            return;
        }
        self.load_once(|c| &mut c.apps, |me| {
            // ponytail: read once per load; an app started later is still tickable (macOS can trash it)
            let mut sys = sysinfo::System::new();
            sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
            let running: Vec<PathBuf> = sys.processes().values().filter_map(|p| p.exe().map(Path::to_path_buf)).collect();
            let found = crate::finders::apps::unused(&me.deps.runner, &me.deps.app_dirs, &me.deps.library, now_secs(), &running);
            found.map_or(Load::Unavailable, Load::Done)
        });
    }

    /// Look for old files in the background, once per run.
    pub fn load_old_files(self: &Arc<Self>) {
        self.load_once(|c| &mut c.old_files, |me| {
            let shots = me.deps.screenshots.clone().unwrap_or_else(|| crate::finders::oldfiles::screenshot_dir(&me.deps.runner, &me.deps.home));
            let mac = cfg!(target_os = "macos");
            let downloads = crate::finders::oldfiles::usable_downloads(&me.deps.downloads, &me.deps.home).unwrap_or(Path::new(""));
            let found = crate::finders::oldfiles::find(downloads, &shots, mac, !mac, now_secs());
            Load::Done(if mac { crate::finders::oldfiles::drop_opened(&me.deps.runner, found, now_secs()) } else { found })
        });
    }

    /// Look for downloaded AI models in the background, once per run.
    pub fn load_models(self: &Arc<Self>) {
        self.load_once(|c| &mut c.models, |me| Load::Done(crate::finders::models::list(&me.deps.ollama_dirs, &me.deps.hf_dirs)));
    }

    /// Runs `find` on a thread unless `slot` was already loaded (or is loading), then stores its result.
    fn load_once<T: Send + 'static>(self: &Arc<Self>, slot: fn(&mut Core) -> &mut Load<T>, find: impl FnOnce(&Shared) -> Load<T> + Send + 'static) {
        {
            let mut c = self.core.lock().unwrap();
            let s = slot(&mut c);
            if !matches!(s, Load::Idle) {
                return;
            }
            *s = Load::Loading;
        }
        self.changed();
        let me = self.clone();
        std::thread::spawn(move || {
            let found = find(&me);
            *slot(&mut me.core.lock().unwrap()) = found;
            me.changed();
        });
    }
}
