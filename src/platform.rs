use std::path::{Path, PathBuf};

/// Folders searched for unused apps (macOS only). `FSC_APP_DIRS` (":"-separated) replaces them,
/// so demos and screenshots can use a folder of fake apps.
pub fn app_dirs() -> Vec<PathBuf> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    if let Some(v) = std::env::var_os("FSC_APP_DIRS") {
        return std::env::split_paths(&v).collect();
    }
    vec![PathBuf::from("/Applications"), home().join("Applications")]
}

/// `FSC_OLLAMA_DIR` / `FSC_HF_DIR` for demos and screenshots: once either is set, only the
/// overrides count (an unset one: no folder), so a demo never shows the real models.
fn model_override(var: &str) -> Option<Vec<PathBuf>> {
    let any = std::env::var_os("FSC_OLLAMA_DIR").is_some() || std::env::var_os("FSC_HF_DIR").is_some();
    any.then(|| std::env::var_os(var).map(PathBuf::from).into_iter().collect())
}

/// Resolved through links (trash requests are, so model roots must match them), each once.
fn dedup(v: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = v.into_iter().map(|p| dunce::canonicalize(&p).unwrap_or(p)).collect();
    let mut seen = std::collections::HashSet::new();
    v.retain(|p| seen.insert(p.clone()));
    v
}

/// Ollama model folders: `$OLLAMA_MODELS`, else `~/.ollama/models`; on Linux also the system service's.
pub fn ollama_dirs() -> Vec<PathBuf> {
    if let Some(v) = model_override("FSC_OLLAMA_DIR") {
        return dedup(v);
    }
    let mut v = vec![std::env::var_os("OLLAMA_MODELS").map_or_else(|| home().join(".ollama/models"), PathBuf::from)];
    if cfg!(target_os = "linux") {
        v.push(PathBuf::from("/usr/share/ollama/.ollama/models"));
    }
    dedup(v)
}

/// The Hugging Face hub cache: `$HF_HUB_CACHE`, else `$HF_HOME/hub`, else `~/.cache/huggingface/hub`.
pub fn hf_dirs() -> Vec<PathBuf> {
    if let Some(v) = model_override("FSC_HF_DIR") {
        return dedup(v);
    }
    let env = |k| std::env::var_os(k).map(PathBuf::from);
    dedup(vec![env("HF_HUB_CACHE").or_else(|| env("HF_HOME").map(|h| h.join("hub"))).unwrap_or_else(|| home().join(".cache/huggingface/hub"))])
}

/// Saved scan snapshots, one file per scanned folder.
pub fn history_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("FastScanCleaner").join("history")
}

pub fn settings_path() -> PathBuf {
    let config = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    let path = config.join("FastScanCleaner").join("settings.toml");
    // The app was called "Easy Disk Clear" before; bring its settings (recent scans, allowlist) along once.
    if let Err(e) = migrate_settings(&config.join("easy-disk-clear").join("settings.toml"), &path) {
        eprintln!("could not move the old settings: {e}");
    }
    path
}

/// Copies `old` to `new` if only `old` exists; the old file is left in place.
fn migrate_settings(old: &Path, new: &Path) -> std::io::Result<bool> {
    if new.exists() || !old.exists() {
        return Ok(false);
    }
    std::fs::create_dir_all(new.parent().unwrap_or(Path::new(".")))?;
    std::fs::copy(old, new)?;
    Ok(true)
}

pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// A suggested place to scan, shown on the Home screen.
#[derive(Debug, Clone, PartialEq)]
pub struct Place {
    /// Locale key of the place's name.
    pub key: &'static str,
    pub path: PathBuf,
    pub recommended: bool,
}

/// Common places worth scanning that exist on this machine (home folder first).
pub fn scan_places() -> Vec<Place> {
    existing_places([
        ("place_home", dirs::home_dir(), true),
        ("place_downloads", dirs::download_dir(), false),
        ("place_documents", dirs::document_dir(), false),
        ("place_desktop", dirs::desktop_dir(), false),
        ("place_caches", dirs::cache_dir(), false),
    ])
}

fn existing_places(candidates: impl IntoIterator<Item = (&'static str, Option<PathBuf>, bool)>) -> Vec<Place> {
    candidates
        .into_iter()
        .filter_map(|(key, path, recommended)| path.filter(|p| p.is_dir()).map(|path| Place { key, path, recommended }))
        .collect()
}

/// What "Scan my computer" scans.
pub fn default_scan_root() -> PathBuf {
    if cfg!(windows) { PathBuf::from("C:\\") } else { home() }
}

/// Folders whose whole subtree may never be moved to the Trash.
pub fn protected_subtrees() -> Vec<PathBuf> {
    let raw: &[&str] = if cfg!(target_os = "macos") {
        &["/System", "/usr", "/bin", "/sbin", "/Library", "/Applications", "/private"]
    } else if cfg!(windows) {
        // ponytail: fixed C: paths, read %SystemRoot%/%ProgramFiles% if Windows lives elsewhere
        &["C:\\Windows", "C:\\Program Files", "C:\\Program Files (x86)", "C:\\ProgramData"]
    } else {
        &["/usr", "/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32", "/etc", "/boot", "/proc", "/sys", "/dev", "/run"]
    };
    raw.iter().map(|p| dunce::canonicalize(p).unwrap_or_else(|_| PathBuf::from(p))).collect()
}

/// Virtual filesystems the scanner must not enter.
pub fn skip_dir(path: &Path) -> bool {
    cfg!(target_os = "linux") && ["/proc", "/sys", "/dev", "/run"].iter().any(|p| path == Path::new(p))
}

pub fn reveal(path: &Path) {
    let _ = opener::reveal(path);
}

pub fn has_permission_settings() -> bool {
    cfg!(target_os = "macos")
}

pub fn open_permission_settings() {
    let _ = opener::open("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles");
}

/// Full Disk Access from trying protected folders in order: the first one that exists decides.
/// None of them there: nothing protected to read, so nothing is missing.
fn fda_from(tries: impl IntoIterator<Item = std::io::Result<()>>) -> bool {
    for t in tries {
        match t {
            Ok(()) => return true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return false,
        }
    }
    true
}

/// macOS only: whether the app may read protected folders (Mail, Safari, app data).
pub fn full_disk_access() -> Option<bool> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let lib = home().join("Library");
    Some(fda_from(["Safari", "Mail"].map(|d| std::fs::read_dir(lib.join(d)).map(|_| ()))))
}

/// Exists once the welcome guide was finished or skipped.
pub fn welcome_marker() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("FastScanCleaner").join("welcome-seen")
}

#[derive(Debug, Clone)]
pub struct Volume {
    pub name: String,
    pub mount: PathBuf,
    pub total: u64,
    pub available: u64,
}

/// User-facing disks, without the many internal system mounts.
pub fn volumes() -> Vec<Volume> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    let mut out: Vec<Volume> = disks
        .list()
        .iter()
        .filter(|d| {
            let m = d.mount_point();
            if cfg!(target_os = "macos") {
                m == Path::new("/") || m.starts_with("/Volumes")
            } else if cfg!(windows) {
                true
            } else {
                !["/snap", "/boot", "/run", "/sys", "/proc", "/dev", "/var/lib"].iter().any(|p| m.starts_with(p))
            }
        })
        .map(|d| Volume {
            name: match d.name().to_string_lossy() {
                n if n.is_empty() => d.mount_point().display().to_string(),
                n => n.into_owned(),
            },
            mount: d.mount_point().to_path_buf(),
            total: d.total_space(),
            available: d.available_space(),
        })
        .collect();
    out.dedup_by(|a, b| a.mount == b.mount);
    out
}

/// Opens the OS storage settings (macOS Storage, Windows Storage Sense).
pub fn open_storage_tool() {
    let url = if cfg!(target_os = "macos") { "x-apple.systempreferences:com.apple.settings.Storage" } else { "ms-settings:storagesense" };
    let _ = opener::open(url);
}

/// Dock icon for the bare binary (a bundled .app gets it from AppIcon.icns instead).
#[cfg(target_os = "macos")]
pub fn set_app_icon() {
    use objc2::ClassType;
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::{MainThreadMarker, NSData};
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let data = NSData::with_bytes(include_bytes!("../assets/icon.png"));
    if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
        unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
    }
}

/// Puts the app's windows in front without activating the app, so a covered window paints
/// again (macOS stops drawing fully covered windows) while keyboard focus stays where it is.
#[cfg(target_os = "macos")]
pub fn order_front_quietly() {
    use objc2_app_kit::NSApplication;
    use objc2_foundation::MainThreadMarker;
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    for w in NSApplication::sharedApplication(mtm).windows().iter() {
        if w.isVisible() {
            unsafe { w.orderFrontRegardless() };
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn model_folders_are_resolved_through_links() {
        // Trash requests are canonicalised; the model roots must match them.
        let tmp = tempfile::tempdir().unwrap();
        let real = dunce::canonicalize(tmp.path()).unwrap().join("ssd");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(dedup(vec![link, real.clone(), tmp.path().join("missing")]), vec![real, tmp.path().join("missing")]);
    }

    #[test]
    fn model_folders_are_listed_once() {
        // Real folders: dedup canonicalises, and "/a" resolves to the current drive on Windows.
        let t = tempfile::tempdir().unwrap();
        let tmp = dunce::canonicalize(t.path()).unwrap();
        let (a, b) = (tmp.join("a"), tmp.join("b"));
        std::fs::create_dir(&a).unwrap();
        std::fs::create_dir(&b).unwrap();
        assert_eq!(dedup(vec![a.clone(), b.clone(), a.clone()]), vec![a.clone(), b]);
    }

    use super::*;

    #[test]
    fn full_disk_access_is_decided_by_the_first_protected_folder_that_exists() {
        use std::io::{Error, ErrorKind::*};
        let ok = || Ok(());
        let err = |k| Err(Error::from(k));
        assert!(fda_from([ok(), err(PermissionDenied)]));
        assert!(fda_from([err(NotFound), ok()]));
        assert!(!fda_from([err(PermissionDenied), ok()]));
        assert!(!fda_from([err(NotFound), err(PermissionDenied)]));
        assert!(fda_from([err(NotFound), err(NotFound)]), "nothing protected to read");
    }

    #[test]
    fn old_settings_are_copied_once_and_never_overwrite_new_ones() {
        let t = tempfile::tempdir().unwrap();
        let (old, new) = (t.path().join("easy-disk-clear/settings.toml"), t.path().join("FastScanCleaner/settings.toml"));
        assert!(!migrate_settings(&old, &new).unwrap()); // nothing to move
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, "language = \"de\"").unwrap();
        assert!(migrate_settings(&old, &new).unwrap());
        assert_eq!(std::fs::read_to_string(&new).unwrap(), "language = \"de\"");
        std::fs::write(&new, "language = \"fr\"").unwrap();
        assert!(!migrate_settings(&old, &new).unwrap()); // the new file wins
        assert_eq!(std::fs::read_to_string(&new).unwrap(), "language = \"fr\"");
        assert!(old.exists()); // the old one stays as a backup
    }

    #[test]
    fn only_existing_folders_are_offered() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("not-a-dir");
        std::fs::write(&file, "x").unwrap();
        let places = existing_places([
            ("place_home", Some(tmp.path().to_path_buf()), true),
            ("place_downloads", Some(tmp.path().join("missing")), false),
            ("place_documents", None, false),
            ("place_desktop", Some(file), false),
        ]);
        assert_eq!(places, vec![Place { key: "place_home", path: tmp.path().to_path_buf(), recommended: true }]);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn order_front_quietly() {} // ponytail: other platforms keep painting covered windows

#[cfg(not(target_os = "macos"))]
pub fn set_app_icon() {} // ponytail: Windows/Linux show the default icon until we ship installers (assets/icon.ico, icon.png)
