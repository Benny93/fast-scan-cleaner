//! Apps in the Applications folders that were not opened for a long time, with the files they
//! leave in ~/Library. macOS only: the dates come from Spotlight (`mdls`).

use crate::finders::cleaners::{CHECK_TIMEOUT, Cmd, CmdOutput, Runner};
use std::path::{Path, PathBuf};

/// Not opened for this long counts as unused.
pub const UNUSED_AFTER: u64 = 180 * 86_400;

#[derive(Debug, Clone, PartialEq)]
pub struct App {
    pub path: PathBuf,
    pub name: String,
    pub id: Option<String>,
    /// Unix seconds.
    pub last_used: u64,
    /// Size of the app itself.
    pub own: u64,
    pub leftovers: Vec<(PathBuf, u64)>,
    pub running: bool,
    /// False when moving it needs an administrator (e.g. installed for all users).
    pub movable: bool,
}

impl App {
    pub fn size(&self) -> u64 {
        self.own + self.leftovers.iter().map(|(_, n)| n).sum::<u64>()
    }

    /// The app and its leftovers: one Trash request.
    pub fn paths(&self) -> Vec<PathBuf> {
        std::iter::once(self.path.clone()).chain(self.leftovers.iter().map(|(p, _)| p.clone())).collect()
    }

    pub fn removable(&self) -> bool {
        self.movable && !self.running
    }
}

/// `*.app` folders (not symlinks) directly inside `dirs`, sorted.
fn list(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut apps: Vec<PathBuf> = dirs
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "app") && std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()))
        .collect();
    apps.sort();
    apps
}

/// "2025-03-01 10:22:33 +0000" (mdls prints UTC) → Unix seconds.
pub(crate) fn parse_date(s: &str) -> Option<u64> {
    let (date, rest) = s.trim().split_once(' ')?;
    let num = |x: &str| x.parse::<i64>().ok();
    let mut d = date.split('-').map(num);
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = rest.split(' ').next()?.split(':').map(num);
    let (h, min, sec) = (t.next()??, t.next()??, t.next()??);
    // Days from civil (Howard Hinnant), the inverse of `snapshots::day`.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + h * 3600 + min * 60 + sec).ok()
}

/// `mdls -name kMDItemCFBundleIdentifier -name kMDItemLastUsedDate -raw <n apps>`: values
/// separated by NUL, bundle id then date per app, "(null)" when missing. `None` unless every
/// value is accounted for.
pub fn parse_mdls(out: &str, n: usize) -> Option<Vec<(Option<String>, Option<u64>)>> {
    let mut v: Vec<&str> = out.split('\0').collect();
    if v.len() == 2 * n + 1 && v.last() == Some(&"") {
        v.pop();
    }
    if v.len() != 2 * n {
        return None;
    }
    let val = |s: &str| Some(s.trim()).filter(|s| !s.is_empty() && *s != "(null)").map(String::from);
    v.chunks(2)
        .map(|c| {
            let date = match val(c[1]) {
                Some(d) => Some(parse_date(&d)?),
                None => None,
            };
            Some((val(c[0]), date))
        })
        .collect()
}

/// Files an app leaves in `library` (~/Library), only those that exist, sorted.
pub fn leftovers(library: &Path, name: &str, id: Option<&str>) -> Vec<PathBuf> {
    let ok = |s: &str| !s.is_empty() && !s.contains('/') && s != "." && s != "..";
    let mut c = Vec::new();
    if ok(name) {
        c.push(library.join("Application Support").join(name));
        c.push(library.join("Logs").join(name));
    }
    if let Some(id) = id.filter(|i| ok(i)) {
        for d in ["Application Support", "Caches", "Containers", "HTTPStorages", "WebKit"] {
            c.push(library.join(d).join(id));
        }
        c.push(library.join("Preferences").join(format!("{id}.plist")));
        c.push(library.join("Saved Application State").join(format!("{id}.savedState")));
    }
    c.retain(|p| std::fs::symlink_metadata(p).is_ok());
    c.sort();
    c.dedup();
    c
}

fn size_of(p: &Path) -> u64 {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => crate::scan::scan(p, &crate::scan::Progress::default()).map_or(0, |n| n.size),
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

/// Moving a folder needs write access to it and to its parent.
#[cfg(unix)]
fn movable(p: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let writable = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes()).is_ok_and(|c| unsafe { libc::access(c.as_ptr(), libc::W_OK) } == 0);
    writable(p) && p.parent().is_some_and(writable)
}

#[cfg(not(unix))]
fn movable(_: &Path) -> bool {
    true
}

/// Apps in `dirs` not opened for `UNUSED_AFTER`, biggest first. `None` when `mdls` fails or
/// its output can't be matched to the apps.
pub fn unused(run: &Runner, dirs: &[PathBuf], library: &Path, now: u64, running: &[PathBuf]) -> Option<Vec<App>> {
    let paths = list(dirs);
    if paths.is_empty() {
        return Some(Vec::new());
    }
    let mut args: Vec<String> = ["-name", "kMDItemCFBundleIdentifier", "-name", "kMDItemLastUsedDate", "-raw"].map(String::from).to_vec();
    args.extend(paths.iter().map(|p| p.to_string_lossy().into_owned()));
    let CmdOutput::Done { ok: true, stdout, .. } = run(&Cmd { program: "mdls", args, timeout: CHECK_TIMEOUT }) else { return None };
    let meta = parse_mdls(&stdout, paths.len())?;
    let stem = |p: &Path| p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    // Every installed app, used or not: their files must not leave with an unused app.
    let everyone: Vec<(PathBuf, String, Option<String>)> = paths.iter().zip(&meta).map(|(p, (id, _))| (p.clone(), stem(p).to_lowercase(), id.clone())).collect();
    let mut apps: Vec<App> = paths
        .into_iter()
        .zip(meta)
        // ponytail: no date is not "never opened" (Safari and Office report none), so those are skipped
        .filter_map(|(path, (id, used))| used.filter(|u| now.saturating_sub(*u) >= UNUSED_AFTER).map(|u| (path, id, u)))
        .map(|(path, id, last_used)| {
            let name = stem(&path);
            let others: Vec<_> = everyone.iter().filter(|(p, _, _)| *p != path).collect();
            let shared = |leftover: &Path| {
                let file = leftover.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                // By name: "Firefox" is also Firefox Developer Edition's folder. By id: a second copy of the app.
                (file == name && others.iter().any(|(_, s, _)| s.starts_with(&name.to_lowercase())))
                    || (id.as_deref().is_some_and(|i| file.starts_with(i)) && others.iter().any(|(_, _, o)| *o == id))
            };
            let leftovers = leftovers(library, &name, id.as_deref()).into_iter().filter(|p| !shared(p)).map(|p| (p.clone(), size_of(&p))).collect();
            App { own: size_of(&path), running: running.iter().any(|e| e.starts_with(&path)), movable: movable(&path), leftovers, name, id, last_used, path }
        })
        .collect();
    apps.sort_by_key(|a| std::cmp::Reverse(a.size()));
    Some(apps)
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::finders::cleaners::{Cmd, CmdOutput, Runner};
    use std::fs;
    use std::sync::Arc;

    const T0: u64 = 1_740_824_553; // 2025-03-01 10:22:33 UTC

    #[test]
    fn mdls_dates_are_utc_seconds() {
        assert_eq!(parse_date("1970-01-01 00:00:00 +0000"), Some(0));
        assert_eq!(parse_date("2025-03-01 10:22:33 +0000"), Some(T0));
        assert_eq!(parse_date("yesterday"), None);
    }

    #[test]
    fn mdls_output_pairs_id_and_date_per_app() {
        let out = "com.a\0(null)\0com.b\x002025-03-01 10:22:33 +0000";
        let want = vec![(Some("com.a".to_string()), None), (Some("com.b".to_string()), Some(T0))];
        assert_eq!(parse_mdls(out, 2), Some(want.clone()));
        assert_eq!(parse_mdls(&format!("{out}\0"), 2), Some(want), "a trailing separator is fine");
        assert_eq!(parse_mdls(out, 3), None, "wrong count: no guessing");
        assert_eq!(parse_mdls("com.a\0last week", 1), None, "unreadable date: no guessing");
    }

    #[test]
    fn leftovers_are_the_existing_ones_by_id_and_name() {
        let tmp = tempfile::tempdir().unwrap();
        let lib = tmp.path();
        for d in ["Caches/com.x", "Application Support/X", "Logs"] {
            fs::create_dir_all(lib.join(d)).unwrap();
        }
        fs::create_dir_all(lib.join("Preferences")).unwrap();
        fs::write(lib.join("Preferences/com.x.plist"), b"p").unwrap();
        assert_eq!(leftovers(lib, "X", Some("com.x")), vec![lib.join("Application Support/X"), lib.join("Caches/com.x"), lib.join("Preferences/com.x.plist")]);
        assert_eq!(leftovers(lib, "X", None), vec![lib.join("Application Support/X")]);
        assert!(leftovers(lib, "../X", None).is_empty() && leftovers(lib, "", Some("..")).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn system_folders_are_not_movable() {
        assert!(!movable(Path::new("/usr/bin")));
        assert!(movable(tempfile::tempdir().unwrap().path()));
    }

    fn mdls(stdout: &'static str) -> Runner {
        Arc::new(move |c: &Cmd| {
            assert_eq!(c.program, "mdls");
            assert!(c.args.iter().any(|a| a == "-raw"));
            CmdOutput::Done { ok: true, stdout: stdout.into(), stderr: String::new() }
        })
    }

    #[test]
    fn only_apps_unused_for_six_months_are_listed() {
        let tmp = tempfile::tempdir().unwrap();
        let (apps, lib) = (tmp.path().join("Applications"), tmp.path().join("Library"));
        for a in ["New.app", "NoDate.app", "Old.app/Contents/MacOS"] {
            fs::create_dir_all(apps.join(a)).unwrap();
        }
        fs::write(apps.join("Old.app/Contents/MacOS/Old"), vec![0u8; 5000]).unwrap();
        fs::write(apps.join("notes.app"), b"a file, not an app").unwrap();
        fs::create_dir_all(lib.join("Caches")).unwrap();
        fs::write(lib.join("Caches/com.old"), vec![0u8; 3000]).unwrap();
        // Sorted: New, NoDate, Old.
        let run = mdls("com.new\x002025-09-01 00:00:00 +0000\0(null)\0(null)\0com.old\x002025-03-01 10:22:33 +0000");
        let now = T0 + 200 * 86_400;
        let running = [apps.join("Old.app/Contents/MacOS/Old")];
        let found = unused(&run, &[apps.clone(), tmp.path().join("missing")], &lib, now, &running).unwrap();
        assert_eq!(found.len(), 1, "recent and undated apps are not listed: {found:?}");
        let a = &found[0];
        assert_eq!((a.name.as_str(), a.id.as_deref(), a.last_used), ("Old", Some("com.old"), T0));
        assert_eq!(a.leftovers, vec![(lib.join("Caches/com.old"), 3000)]);
        assert!(a.own >= 5000 && a.size() == a.own + 3000);
        assert!(a.running && a.movable && !a.removable());
        assert_eq!(a.paths(), vec![apps.join("Old.app"), lib.join("Caches/com.old")]);
    }

    #[test]
    fn mdls_failure_lists_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(tmp.path().join("A.app")).unwrap();
        let run: Runner = Arc::new(|_: &Cmd| CmdOutput::NotFound);
        assert_eq!(unused(&run, &[tmp.path().to_path_buf()], tmp.path(), T0, &[]), None);
        assert_eq!(unused(&run, &[tmp.path().join("none")], tmp.path(), T0, &[]), Some(Vec::new()), "no apps: no mdls call needed");
    }

    #[test]
    fn leftovers_another_installed_app_uses_stay() {
        let tmp = tempfile::tempdir().unwrap();
        let (apps, lib) = (tmp.path().join("Applications"), tmp.path().join("Library"));
        for a in ["Firefox.app", "Firefox Developer Edition.app", "Foo.app", "Foo 2.app"] {
            fs::create_dir_all(apps.join(a)).unwrap();
        }
        for d in ["Application Support/Firefox", "Caches/org.mozilla.firefox", "Caches/com.foo", "Application Support/Foo"] {
            fs::create_dir_all(lib.join(d)).unwrap();
        }
        // Sorted: Firefox Developer Edition (recent), Firefox (old), Foo 2 (recent), Foo (old); Foo 2 is a copy of Foo.
        let run = mdls("org.mozilla.firefoxdeveloperedition\x002026-10-01 00:00:00 +0000\0org.mozilla.firefox\x002025-03-01 10:22:33 +0000\0com.foo\x002026-10-01 00:00:00 +0000\0com.foo\x002025-03-01 10:22:33 +0000");
        let found = unused(&run, &[apps], &lib, T0 + 200 * 86_400, &[]).unwrap();
        let left = |n: &str| found.iter().find(|a| a.name == n).unwrap().leftovers.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>();
        assert_eq!(left("Firefox"), vec![lib.join("Caches/org.mozilla.firefox")], "the profile folder is shared with Developer Edition");
        assert!(left("Foo").is_empty(), "Foo 2 is a copy: same bundle id and name folder");
    }
}
