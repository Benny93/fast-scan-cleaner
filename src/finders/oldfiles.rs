//! Files people forget about: very old screenshots, installers left in Downloads and downloads
//! nobody has used for a year. No UI; works without a scan.

use crate::finders::cleaners::{CHECK_TIMEOUT, Cmd, CmdOutput, Runner};
use std::collections::{HashMap, HashSet};
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const OLD_AFTER: u64 = 365 * 86_400;
pub const INSTALLER_AFTER: u64 = 30 * 86_400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Group {
    Screenshot,
    Installer,
    Download,
}

impl Group {
    pub const ALL: [Group; 3] = [Group::Screenshot, Group::Installer, Group::Download];
}

#[derive(Debug, Clone, PartialEq)]
pub struct OldFile {
    pub path: PathBuf,
    pub group: Group,
    /// Unix seconds.
    pub last_used: u64,
    pub size: u64,
}

/// Default screenshot and screen recording names (macOS, Windows, GNOME) in the app's languages.
const SHOT_PREFIXES: [&str; 14] = ["Screenshot", "Screen Shot", "Screen Recording", "Bildschirmfoto", "Bildschirmaufnahme", "Capture d’écran", "Capture d'écran", "Enregistrement de l’écran", "Captura de pantalla", "Grabación de pantalla", "截屏", "录屏", "スクリーンショット", "画面収録"];
const INSTALLERS: [&str; 9] = ["dmg", "pkg", "mpkg", "exe", "msi", "msix", "appimage", "deb", "rpm"];

pub fn is_screenshot_name(name: &str) -> bool {
    SHOT_PREFIXES.iter().any(|p| name.starts_with(p))
}

fn is_installer(p: &Path) -> bool {
    p.extension().and_then(|e| e.to_str()).is_some_and(|e| INSTALLERS.iter().any(|i| e.eq_ignore_ascii_case(i)))
}

fn secs(t: std::io::Result<SystemTime>) -> u64 {
    t.ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs())
}

/// The newer of modified and (with `with_atime`) accessed: errs on the side of keeping. On macOS
/// the access time is left out: Spotlight reads new images and PDFs, which updates it.
fn used(m: &Metadata, with_atime: bool) -> u64 {
    let modified = secs(m.modified());
    if with_atime { modified.max(secs(m.accessed())) } else { modified }
}

/// Newest use of any file under `dir`. Folders' own times are left out: listing a folder (as
/// this very search does) counts as accessing it.
fn newest_file(dir: &Path, with_atime: bool) -> Option<u64> {
    let mut newest = None;
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let Ok(m) = e.metadata() else { continue };
        let t = if m.is_dir() {
            newest_file(&e.path(), with_atime)
        } else if m.is_file() {
            Some(used(&m, with_atime))
        } else {
            None
        };
        newest = newest.max(t);
    }
    newest
}

/// Top-level entries of `dir`, without symlinks and hidden ones (".DS_Store", ".localized").
fn entries(dir: &Path) -> Vec<(PathBuf, Metadata)> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    rd.flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|e| Some((e.path(), e.metadata().ok()?)))
        .filter(|(_, m)| !m.file_type().is_symlink())
        .collect()
}

/// Old files in `downloads` and `shots` (top level only), screenshots first, each group biggest
/// first. `shots_by_name`: only files named like screenshots count in `shots` (macOS, where it
/// is usually the Desktop); otherwise every file there does.
pub fn find(downloads: &Path, shots: &Path, shots_by_name: bool, with_atime: bool, now: u64) -> Vec<OldFile> {
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut seen = HashSet::new();
    let mut picked: Vec<(PathBuf, Metadata, Group)> = Vec::new();
    for (p, m) in entries(shots) {
        if m.is_file() && (!shots_by_name || is_screenshot_name(&name(&p))) && seen.insert(p.clone()) {
            picked.push((p, m, Group::Screenshot));
        }
    }
    for (p, m) in entries(downloads) {
        if !seen.insert(p.clone()) {
            continue;
        }
        let group = if m.is_file() && is_screenshot_name(&name(&p)) {
            Group::Screenshot
        } else if m.is_file() && is_installer(&p) {
            Group::Installer
        } else {
            Group::Download
        };
        picked.push((p, m, group));
    }
    let mut out: Vec<OldFile> = picked
        .into_iter()
        .filter_map(|(path, m, group)| {
            let last_used = if m.is_dir() { newest_file(&path, with_atime).unwrap_or_else(|| secs(m.modified())) } else { used(&m, with_atime) };
            let limit = if group == Group::Installer { INSTALLER_AFTER } else { OLD_AFTER };
            if now.saturating_sub(last_used) <= limit {
                return None;
            }
            let size = if m.is_dir() { crate::scan::scan(&path, &crate::scan::Progress::default()).map_or(0, |n| n.size) } else { m.len() };
            Some(OldFile { path, group, last_used, size })
        })
        .collect();
    out.sort_by_key(|o| (Group::ALL.iter().position(|g| *g == o.group), std::cmp::Reverse(o.size)));
    out
}

/// The Downloads folder to search, unless it is the home folder or above it (a Linux
/// `XDG_DOWNLOAD_DIR="$HOME"`): then every untouched folder in home would count as a download.
pub fn usable_downloads<'a>(downloads: &'a Path, home: &Path) -> Option<&'a Path> {
    (!home.starts_with(downloads)).then_some(downloads)
}

/// Paths per `mdls` call: keeps the argument list and the run time small.
const MDLS_BATCH: usize = 500;

/// macOS: drop files, folders and bundles Spotlight saw a person open recently
/// (`kMDItemLastUsedDate`), which the modification time can't tell. Asked in batches; a batch
/// Spotlight can't answer (or a path that isn't UTF-8) keeps the modification-time result.
pub fn drop_opened(run: &Runner, files: Vec<OldFile>, now: u64) -> Vec<OldFile> {
    let ask: Vec<&str> = files.iter().filter_map(|o| o.path.to_str()).collect();
    let mut opened: HashMap<PathBuf, u64> = HashMap::new();
    for batch in ask.chunks(MDLS_BATCH) {
        let mut args: Vec<String> = ["-name", "kMDItemLastUsedDate", "-raw"].map(String::from).to_vec();
        args.extend(batch.iter().map(|p| p.to_string()));
        let CmdOutput::Done { ok: true, stdout, .. } = run(&Cmd { program: "mdls", args, timeout: CHECK_TIMEOUT }) else { continue };
        let mut v: Vec<&str> = stdout.split('\0').collect();
        if v.len() == batch.len() + 1 && v.last() == Some(&"") {
            v.pop();
        }
        if v.len() != batch.len() {
            continue;
        }
        opened.extend(batch.iter().zip(v).filter_map(|(p, d)| Some((PathBuf::from(p), crate::finders::apps::parse_date(d)?))));
    }
    files
        .into_iter()
        .filter_map(|mut o| {
            if let Some(&t) = opened.get(&o.path) {
                o.last_used = o.last_used.max(t);
            }
            let limit = if o.group == Group::Installer { INSTALLER_AFTER } else { OLD_AFTER };
            (now.saturating_sub(o.last_used) > limit).then_some(o)
        })
        .collect()
}

/// Where screenshots are saved: on macOS the system setting, else the Desktop; elsewhere
/// <Pictures>/Screenshots.
pub fn screenshot_dir(run: &Runner, home: &Path) -> PathBuf {
    if cfg!(target_os = "macos") {
        mac_screenshot_dir(run, home)
    } else {
        dirs::picture_dir().unwrap_or_else(|| home.join("Pictures")).join("Screenshots")
    }
}

fn mac_screenshot_dir(run: &Runner, home: &Path) -> PathBuf {
    let cmd = Cmd::new("defaults", &["read", "com.apple.screencapture", "location"], CHECK_TIMEOUT);
    if let CmdOutput::Done { ok: true, stdout, .. } = run(&cmd) {
        let s = stdout.trim();
        let p = match s.strip_prefix('~') {
            Some(rest) => home.join(rest.trim_start_matches('/')),
            None => PathBuf::from(s),
        };
        if p.is_absolute() && p.is_dir() {
            return p;
        }
    }
    home.join("Desktop")
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File, FileTimes};
    use std::sync::Arc;
    use std::time::Duration;

    const NOW: u64 = 1_800_000_000;
    const DAY: u64 = 86_400;

    fn touch(p: &Path, len: usize, days_ago: u64) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![0u8; len]).unwrap();
        let t = UNIX_EPOCH + Duration::from_secs(NOW - days_ago * DAY);
        File::options().write(true).open(p).unwrap().set_times(FileTimes::new().set_accessed(t).set_modified(t)).unwrap();
    }

    fn names(found: &[OldFile]) -> Vec<(String, Group)> {
        found.iter().map(|o| (o.path.file_name().unwrap().to_string_lossy().into_owned(), o.group)).collect()
    }

    #[test]
    fn screenshot_names_in_every_language() {
        for n in ["Screenshot 2024-01-01 at 10.00.00.png", "Screen Shot 2019-05-05.png", "Screen Recording 2024.mov", "Bildschirmfoto 2024-01-01 um 10.00.00.png", "Bildschirmaufnahme 2024.mov", "Capture d’écran 2024.png", "Capture d'écran 2024.png", "Enregistrement de l’écran 2024.mov", "Captura de pantalla 2024.png", "Grabación de pantalla 2024.mov", "截屏2024-01-01 10.00.00.png", "录屏2024.mov", "スクリーンショット 2024-01-01.png", "画面収録 2024.mov"] {
            assert!(is_screenshot_name(n), "{n}");
        }
        for n in ["holiday.png", "my Screenshot.png", "screenshot.png"] {
            assert!(!is_screenshot_name(n), "{n}");
        }
    }

    #[test]
    fn old_screenshots_installers_and_downloads_are_found() {
        let tmp = tempfile::tempdir().unwrap();
        let (dl, desk) = (tmp.path().join("Downloads"), tmp.path().join("Desktop"));
        touch(&desk.join("Screenshot old.png"), 100, 400);
        touch(&desk.join("Screenshot new.png"), 100, 10);
        touch(&desk.join("notes.txt"), 100, 900);
        touch(&dl.join("Zoom.pkg"), 3000, 40);
        touch(&dl.join("Fresh.DMG"), 3000, 10);
        touch(&dl.join("report.pdf"), 20_000, 500);
        touch(&dl.join("recent.pdf"), 2000, 100);
        touch(&dl.join("old-folder/a.bin"), 500, 800);
        touch(&dl.join("mixed-folder/a.bin"), 500, 800);
        touch(&dl.join("mixed-folder/b.bin"), 500, 5);
        touch(&dl.join(".DS_Store"), 10, 900);
        let found = find(&dl, &desk, true, true, NOW);
        let want: Vec<(String, Group)> = vec![("Screenshot old.png".into(), Group::Screenshot), ("Zoom.pkg".into(), Group::Installer), ("report.pdf".into(), Group::Download), ("old-folder".into(), Group::Download)];
        assert_eq!(names(&found), want);
        assert_eq!(found[0].last_used, NOW - 400 * DAY);
        assert_eq!(found[1].size, 3000);
        assert_eq!(found[3].last_used, NOW - 800 * DAY, "a folder counts by its newest file, not by its own times");
    }

    #[test]
    fn a_screenshot_in_downloads_is_listed_once() {
        let tmp = tempfile::tempdir().unwrap();
        let dl = tmp.path().join("Downloads");
        touch(&dl.join("Screenshot 2020.png"), 100, 900);
        assert_eq!(names(&find(&dl, &dl, true, true, NOW)), vec![("Screenshot 2020.png".to_string(), Group::Screenshot)]);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_skipped_and_a_screenshots_folder_counts_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let (dl, shots) = (tmp.path().join("Downloads"), tmp.path().join("Screenshots"));
        touch(&tmp.path().join("elsewhere.zip"), 100, 900);
        fs::create_dir_all(&dl).unwrap();
        std::os::unix::fs::symlink(tmp.path().join("elsewhere.zip"), dl.join("link.zip")).unwrap();
        touch(&shots.join("Capture 2020.png"), 100, 900);
        assert_eq!(names(&find(&dl, &shots, false, true, NOW)), vec![("Capture 2020.png".to_string(), Group::Screenshot)]);
    }

    #[test]
    fn a_future_date_is_never_old() {
        let tmp = tempfile::tempdir().unwrap();
        touch(&tmp.path().join("report.pdf"), 100, 0);
        assert!(find(tmp.path(), &tmp.path().join("none"), true, true, NOW - 900 * DAY).is_empty());
    }

    fn defaults(out: CmdOutput) -> Runner {
        Arc::new(move |c: &Cmd| {
            assert_eq!(c.program, "defaults");
            out.clone()
        })
    }

    #[test]
    fn macos_screenshot_folder_follows_the_setting() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        fs::create_dir_all(h.join("Pictures/Shots")).unwrap();
        let done = |s: String| CmdOutput::Done { ok: true, stdout: s, stderr: String::new() };
        assert_eq!(mac_screenshot_dir(&defaults(done("~/Pictures/Shots\n".into())), h), h.join("Pictures/Shots"));
        assert_eq!(mac_screenshot_dir(&defaults(done(format!("{}\n", h.join("Pictures/Shots").display()))), h), h.join("Pictures/Shots"));
        assert_eq!(mac_screenshot_dir(&defaults(done("~/missing".into())), h), h.join("Desktop"));
        let unset = CmdOutput::Done { ok: false, stdout: String::new(), stderr: "does not exist".into() };
        assert_eq!(mac_screenshot_dir(&defaults(unset), h), h.join("Desktop"));
    }

    #[test]
    fn without_access_times_only_modification_counts() {
        // Spotlight reads new images and PDFs, which updates their access time.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("report.pdf");
        touch(&p, 100, 500);
        let recent = UNIX_EPOCH + Duration::from_secs(NOW - DAY);
        File::options().write(true).open(&p).unwrap().set_times(FileTimes::new().set_accessed(recent)).unwrap();
        assert!(find(tmp.path(), &tmp.path().join("none"), true, true, NOW).is_empty(), "with access times it was read yesterday");
        assert_eq!(find(tmp.path(), &tmp.path().join("none"), true, false, NOW).len(), 1);
    }

    #[test]
    fn files_spotlight_saw_opened_recently_are_dropped() {
        let old = |name: &str, group| OldFile { path: PathBuf::from(format!("/d/{name}")), group, last_used: NOW - 500 * DAY, size: 1 };
        let files = vec![old("a.pdf", Group::Download), old("b.pdf", Group::Download), old("c.dmg", Group::Installer), old("Tool.app", Group::Download)];
        let run: Runner = Arc::new(|c: &Cmd| {
            assert_eq!(c.program, "mdls");
            assert!(c.args.iter().any(|a| a.ends_with("Tool.app")), "folders and bundles are asked about too");
            CmdOutput::Done { ok: true, stdout: "2026-10-01 00:00:00 +0000\0(null)\x002020-01-01 00:00:00 +0000\x002026-12-01 00:00:00 +0000".into(), stderr: String::new() }
        });
        let kept = drop_opened(&run, files.clone(), NOW);
        assert_eq!(kept.iter().map(|o| o.path.file_name().unwrap().to_str().unwrap()).collect::<Vec<_>>(), ["b.pdf", "c.dmg"]);
        let failing: Runner = Arc::new(|_: &Cmd| CmdOutput::NotFound);
        assert_eq!(drop_opened(&failing, files.clone(), NOW), files, "no Spotlight: keep what modification times say");
    }

    #[test]
    fn spotlight_is_asked_in_batches_and_a_failed_batch_only_affects_itself() {
        let files: Vec<OldFile> = (0..600).map(|i| OldFile { path: PathBuf::from(format!("/d/{i:03}.pdf")), group: Group::Download, last_used: NOW - 500 * DAY, size: 1 }).collect();
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = calls.clone();
        let run: Runner = Arc::new(move |c: &Cmd| {
            let n = c.args.len() - 3;
            assert!(n <= 500, "{n} paths in one call");
            if seen.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0 {
                // First batch: everything was opened recently.
                CmdOutput::Done { ok: true, stdout: vec!["2026-12-01 00:00:00 +0000"; n].join("\0"), stderr: String::new() }
            } else {
                CmdOutput::TimedOut
            }
        });
        let kept = drop_opened(&run, files, NOW);
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
        assert_eq!(kept.len(), 100, "the 500 opened ones go; the failed batch keeps its 100");
    }

    #[test]
    fn downloads_that_is_the_home_folder_is_not_searched() {
        let home = Path::new("/home/u");
        assert_eq!(usable_downloads(Path::new("/home/u/Downloads"), home), Some(Path::new("/home/u/Downloads")));
        assert_eq!(usable_downloads(Path::new("/home/u"), home), None);
        assert_eq!(usable_downloads(Path::new("/"), home), None);
    }
}
