use crate::actions::{self, Decision, Gate, Origin, TrashResult};
use crate::rules::{self, Finding, Rules, Safety};
use crate::scan::{self, Kind, Node, Progress};
use crate::settings::Settings;
use crate::finders::cleaners::{self, Cleaner, Estimate, SystemStatus};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

mod finders;
mod scanning;
mod system;
mod trash;
#[cfg(test)]
pub(crate) mod tests;

/// A list a background finder fills once per run; trashed entries leave it.
#[derive(Debug, Default)]
pub enum Load<T> {
    #[default]
    Idle,
    Loading,
    Done(Vec<T>),
    /// Apps only: not macOS, or Spotlight could not tell; the card is hidden.
    Unavailable,
}

/// Old screenshots, installers and downloads.
pub type OldFiles = Load<crate::finders::oldfiles::OldFile>;
/// Downloaded AI models (Ollama, Hugging Face).
pub type Models = Load<crate::finders::models::Model>;
/// Apps not opened for a long time (macOS).
pub type Apps = Load<crate::finders::apps::App>;

/// The duplicate finder's state for the current scan.
#[derive(Debug, Default)]
pub enum Dupes {
    #[default]
    Idle,
    Running(Arc<crate::finders::dupes::DupeProgress>),
    Done(Vec<crate::finders::dupes::Group>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Home,
    DiskMap,
    Cleanup,
    FreeUp,
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
    pub dupes: Dupes,
    pub apps: Apps,
    pub models: Models,
    pub old_files: OldFiles,
    /// Snapshots of the current root's past full scans, oldest first.
    pub history: Vec<crate::scan::snapshots::Snapshot>,
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
    pub history_dir: Option<PathBuf>,
    pub app_dirs: Vec<PathBuf>,
    pub ollama_dirs: Vec<PathBuf>,
    pub hf_dirs: Vec<PathBuf>,
    pub library: PathBuf,
    pub downloads: PathBuf,
    /// None: read the system setting when loading.
    pub screenshots: Option<PathBuf>,
    /// "Seen" marker of the welcome guide; None: never shown (tests).
    pub welcome_marker: Option<PathBuf>,
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
            history_dir: Some(crate::platform::history_dir()),
            app_dirs: crate::platform::app_dirs(),
            ollama_dirs: crate::platform::ollama_dirs(),
            hf_dirs: crate::platform::hf_dirs(),
            library: std::env::var_os("FSC_APP_LIBRARY").map(PathBuf::from).unwrap_or_else(|| crate::platform::home().join("Library")),
            downloads: std::env::var_os("FSC_DOWNLOADS_DIR").map(PathBuf::from).or_else(dirs::download_dir).unwrap_or_else(|| crate::platform::home().join("Downloads")),
            screenshots: std::env::var_os("FSC_SCREENSHOTS_DIR").map(PathBuf::from),
            welcome_marker: Some(std::env::var_os("FSC_WELCOME_MARKER").map(PathBuf::from).unwrap_or_else(crate::platform::welcome_marker)),
        }
    }
}

pub struct Shared {
    pub core: Mutex<Core>,
    pub deps: Deps,
    settings_path: PathBuf,
    notify: async_channel::Sender<()>,
    /// Agent input for the window; the UI drains it and answers each one.
    pub inputs: async_channel::Receiver<(Input, oneshot::Sender<Result<(), String>>)>,
    input_tx: async_channel::Sender<(Input, oneshot::Sender<Result<(), String>>)>,
    /// Width of the latest screenshot, so agent points can be mapped to the window.
    pub shot_width: std::sync::atomic::AtomicU32,
}

/// Input an agent sends to the window (window points, same as the screenshot's pixels).
#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Click { x: f32, y: f32 },
    Hover { x: f32, y: f32 },
    Scroll { x: f32, y: f32, dy: f32 },
    Text(String),
    Key(String),
    /// Make sure the window is painted and up to date (before a screenshot). Always allowed.
    Reveal,
}

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Shared {
    pub fn new(settings_path: PathBuf, notify: async_channel::Sender<()>) -> Arc<Shared> {
        let (settings, recovered) = Settings::load(&settings_path);
        Shared::build(settings, recovered, settings_path, notify, Deps::real())
    }

    pub fn build(settings: Settings, recovered: bool, settings_path: PathBuf, notify: async_channel::Sender<()>, deps: Deps) -> Arc<Shared> {
        let (input_tx, inputs) = async_channel::unbounded();
        Arc::new(Shared {
            inputs,
            input_tx,
            shot_width: Default::default(),
            core: Mutex::new(Core {
                dupes: Dupes::Idle,
                apps: Apps::Idle,
                models: Models::Idle,
                old_files: OldFiles::Idle,
                history: Vec::new(),
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

    /// Agents may drive the window except where that would act for the user: while a
    /// confirmation is open (the delete gate) or on Settings (agents never change settings).
    /// The UI checks this again right before it applies each input.
    pub fn input_allowed(&self) -> Result<(), &'static str> {
        let c = self.core.lock().unwrap();
        if c.pending.is_some() {
            return Err("a confirmation is open; only the user can answer it");
        }
        if c.screen == Screen::Settings {
            return Err("Settings can only be changed by the user");
        }
        Ok(())
    }

    pub async fn send_input(&self, input: Input) -> Result<(), String> {
        if input != Input::Reveal {
            self.input_allowed()?;
        }
        let (tx, rx) = oneshot::channel();
        self.input_tx.send((input, tx)).await.map_err(|_| "app window is closed".to_string())?;
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(r)) => r,
            _ => Err("app window did not respond".into()),
        }
    }

    /// Ask the UI to re-render. Coalesces: the channel holds at most one pending signal.
    pub fn changed(&self) {
        let _ = self.notify.try_send(());
    }

    pub fn welcome_needed(&self) -> bool {
        self.deps.welcome_marker.as_ref().is_some_and(|m| !m.exists())
    }

    /// Errors are ignored: at worst the guide shows once more.
    pub fn welcome_done(&self) {
        if let Some(m) = &self.deps.welcome_marker {
            if let Some(dir) = m.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(m, b"");
        }
    }

    pub fn show(self: &Arc<Self>, screen: Screen) {
        let mut c = self.core.lock().unwrap();
        // Cleanup also hosts the system tools, which work without a scan.
        let needs_scan = screen == Screen::DiskMap;
        c.screen = if needs_scan && c.tree.is_none() { Screen::Home } else { screen };
        c.system_first = false;
        let cleanup = matches!(c.screen, Screen::Cleanup | Screen::FreeUp);
        let check = cleanup && c.system.is_empty();
        drop(c);
        self.changed();
        if check {
            self.check_system();
        }
        if cleanup {
            self.load_apps();
            self.load_old_files();
            self.load_models();
        }
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
    c.rel(abs).and_then(|r| c.tree.as_ref()?.find(&r).map(|n| n.size)).or_else(|| {
        let Apps::Done(apps) = &c.apps else { return None };
        apps.iter().find_map(|a| if a.path == abs { Some(a.own) } else { a.leftovers.iter().find(|(p, _)| p == abs).map(|(_, n)| *n) })
    })
    .or_else(|| match &c.old_files {
        OldFiles::Done(list) => list.iter().find(|o| o.path == abs).map(|o| o.size),
        _ => None,
    })
    .or_else(|| match &c.models {
        // A Hugging Face folder has its counted size; Ollama manifests and blobs are single files.
        Models::Done(list) => list
            .iter()
            .find(|m| m.source != crate::finders::models::Source::Ollama && m.paths.first().is_some_and(|p| p == abs))
            .map(|m| m.size)
            .or_else(|| list.iter().any(|m| abs.starts_with(&m.root)).then(|| std::fs::metadata(abs).ok().map(|md| md.len())).flatten()),
        _ => None,
    })
}

/// Drop a trashed path from the tree, the findings and the view.
fn forget(c: &mut Core, abs: &Path) {
    if let Models::Done(list) = &mut c.models {
        list.retain(|m| !m.paths.first().is_some_and(|p| p.starts_with(abs)));
    }
    if let OldFiles::Done(list) = &mut c.old_files {
        list.retain(|o| !o.path.starts_with(abs));
    }
    if let Apps::Done(apps) = &mut c.apps {
        apps.retain(|a| !a.path.starts_with(abs));
        for a in apps.iter_mut() {
            a.leftovers.retain(|(p, _)| !p.starts_with(abs));
        }
    }
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
    if let Dupes::Done(groups) = &mut c.dupes {
        for g in groups.iter_mut() {
            g.copies.retain(|x| !x.path.starts_with(abs));
        }
        groups.retain(|g| g.copies.len() > 1);
    }
}
