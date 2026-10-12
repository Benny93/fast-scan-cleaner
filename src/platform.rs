use std::path::{Path, PathBuf};

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

#[cfg(test)]
mod tests {
    use super::*;

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

#[cfg(not(target_os = "macos"))]
pub fn set_app_icon() {} // ponytail: Windows/Linux show the default icon until we ship installers (assets/icon.ico, icon.png)
