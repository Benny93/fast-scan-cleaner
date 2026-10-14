use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    User,
    Agent,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    Refused(&'static str),
    Immediate,
    NeedsConfirm,
}

pub struct Gate<'a> {
    pub allowlist: &'a [PathBuf],
    pub scan_root: Option<&'a Path>,
    pub protected: &'a [PathBuf],
    pub home: &'a Path,
}

pub fn decide(path: &Path, origin: Origin, gate: &Gate) -> Decision {
    if path.parent().is_none() {
        return Decision::Refused("drive or filesystem root");
    }
    if gate.home.starts_with(path) {
        return Decision::Refused("home folder or one of its parents");
    }
    if gate.scan_root.is_some_and(|r| r.starts_with(path)) {
        return Decision::Refused("the scanned folder or one of its parents");
    }
    // An app right inside an Applications folder may go (the rest of /Applications stays
    // protected), but only ever with the user's confirmation.
    let directly_in = |dir: &Path| path.parent() == Some(dir);
    if path.extension().is_some_and(|e| e == "app") && (directly_in(Path::new("/Applications")) || directly_in(&gate.home.join("Applications"))) {
        return Decision::NeedsConfirm;
    }
    if gate.protected.iter().any(|p| path.starts_with(p) || p.starts_with(path)) {
        return Decision::Refused("system folder");
    }
    let allowlisted = gate.allowlist.iter().any(|a| path.starts_with(a) && path != a);
    // An app or anything inside one never goes without asking, allowlisted or not.
    let in_app = path.ancestors().any(|a| a.extension().is_some_and(|e| e.eq_ignore_ascii_case("app")));
    if origin == Origin::Agent && allowlisted && !in_app { Decision::Immediate } else { Decision::NeedsConfirm }
}

pub fn resolve(path: &Path) -> io::Result<PathBuf> {
    let bad = |why: &str| io::Error::new(io::ErrorKind::InvalidInput, format!("{}: {why}", path.display()));
    if !path.is_absolute() {
        return Err(bad("path must be absolute"));
    }
    let name = path.file_name().ok_or_else(|| bad("path has no file name"))?;
    // An existing non-symlink is canonicalised in full, so its case and any 8.3 short names are normalised.
    if let Ok(meta) = fs::symlink_metadata(path)
        && !meta.file_type().is_symlink()
    {
        return dunce::canonicalize(path);
    }
    // Symlinks and missing paths: canonicalise only the parent, keep the final component as given.
    let parent = path.parent().ok_or_else(|| bad("path has no parent"))?;
    Ok(dunce::canonicalize(parent)?.join(name))
}

pub struct TrashResult {
    pub path: PathBuf,
    pub error: Option<String>,
}

/// The only place in the program that removes files: always to the Trash, never permanently.
pub fn move_to_trash(paths: &[PathBuf]) -> Vec<TrashResult> {
    let ctx = trash_context();
    paths
        .iter()
        .map(|p| TrashResult { path: p.clone(), error: ctx.delete(p).err().map(|e| e.to_string()) })
        .collect()
}

/// On macOS the crate's default asks Finder via AppleScript, once per item: slow, and the Trash
/// sound plays for each. The native call is fast and silent; files still land in the Trash.
fn trash_context() -> trash::TrashContext {
    #[allow(unused_mut)]
    let mut ctx = trash::TrashContext::default();
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos as _};
        ctx.set_delete_method(DeleteMethod::NsFileManager);
    }
    ctx
}

#[cfg(test)]
mod tests {
    use super::*;
    use Decision::*;

    fn check(path: &str, origin: Origin) -> Decision {
        let allow = [PathBuf::from("/home/u/.cache"), PathBuf::from("/home/u/Downloads")];
        let protected = [PathBuf::from("/usr"), PathBuf::from("/etc")];
        let gate = Gate {
            allowlist: &allow,
            scan_root: Some(Path::new("/home/u/projects")),
            protected: &protected,
            home: Path::new("/home/u"),
        };
        decide(Path::new(path), origin, &gate)
    }

    #[test]
    fn decide_refuses_protected_roots_home_and_scan_root() {
        for p in ["/", "/home", "/home/u", "/home/u/projects", "/usr", "/usr/lib/x", "/etc/passwd"] {
            for o in [Origin::User, Origin::Agent] {
                assert!(matches!(check(p, o), Refused(_)), "{p} {o:?}");
            }
        }
    }

    #[test]
    fn decide_agent_inside_allowlist_is_immediate() {
        assert_eq!(check("/home/u/.cache/pip", Origin::Agent), Immediate);
        assert_eq!(check("/home/u/Downloads/a/b.dmg", Origin::Agent), Immediate);
    }

    #[test]
    fn decide_allowlisted_dir_itself_and_lookalikes_need_confirm() {
        assert_eq!(check("/home/u/.cache", Origin::Agent), NeedsConfirm);
        assert_eq!(check("/home/u/.cache-other/x", Origin::Agent), NeedsConfirm);
        assert_eq!(check("/home/u/Documents/x", Origin::Agent), NeedsConfirm);
    }

    #[test]
    fn decide_refusals_beat_allowlist() {
        let allow = [PathBuf::from("/home/u/Downloads")];
        let protected = [PathBuf::from("/home/u/Downloads/sys"), PathBuf::from("/opt/a/b")];
        let gate = Gate {
            allowlist: &allow,
            scan_root: Some(Path::new("/home/u/Downloads/proj")),
            protected: &protected,
            home: Path::new("/home/u"),
        };
        assert!(matches!(decide(Path::new("/home/u/Downloads/proj"), Origin::Agent, &gate), Refused(_)));
        assert!(matches!(decide(Path::new("/home/u/Downloads/sys/x"), Origin::Agent, &gate), Refused(_)));
        // An ancestor of a protected path is refused too.
        assert!(matches!(decide(Path::new("/opt/a"), Origin::Agent, &gate), Refused(_)));
    }

    #[test]
    fn decide_user_always_confirms() {
        assert_eq!(check("/home/u/.cache/pip", Origin::User), NeedsConfirm);
        assert_eq!(check("/home/u/projects/app/node_modules", Origin::User), NeedsConfirm);
    }

    #[test]
    fn resolve_removes_dotdot_and_rejects_bad_input() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir_all(base.join("allowed")).unwrap();
        std::fs::create_dir_all(base.join("secret")).unwrap();
        let sneaky = base.join("allowed").join("..").join("secret").join("f");
        assert_eq!(resolve(&sneaky).unwrap(), base.join("secret").join("f"));
        assert!(resolve(Path::new("relative/x")).is_err());
        assert!(resolve(&base.join("allowed").join("..")).is_err());
        assert!(resolve(Path::new("/")).is_err());
    }

    #[cfg(any(target_os = "macos", windows))]
    #[test]
    fn resolve_fixes_case_of_existing_final_component() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir_all(base.join("dir")).unwrap();
        assert_eq!(resolve(&base.join("DIR")).unwrap(), base.join("dir"));
    }

    #[cfg(unix)]
    #[test]
    fn resolve_follows_symlinked_parents_but_not_the_final_link() {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        std::fs::create_dir_all(base.join("allowed")).unwrap();
        std::fs::create_dir_all(base.join("secret")).unwrap();
        std::os::unix::fs::symlink(base.join("secret"), base.join("allowed/link")).unwrap();
        // A file reached through the link resolves to its real place, outside "allowed".
        assert_eq!(resolve(&base.join("allowed/link/f")).unwrap(), base.join("secret/f"));
        // The link itself stays inside "allowed"; trashing it removes only the link.
        assert_eq!(resolve(&base.join("allowed/link")).unwrap(), base.join("allowed/link"));
    }

    #[test]
    #[ignore = "touches the real Trash; run manually with --ignored"]
    fn move_to_trash_moves_a_file() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("edc-trash-test.txt");
        std::fs::write(&f, "x").unwrap();
        let r = move_to_trash(std::slice::from_ref(&f));
        assert!(r[0].error.is_none());
        assert!(!f.exists());
    }

    #[test]
    fn an_app_in_an_applications_folder_always_asks() {
        let home = PathBuf::from("/home/u");
        let protected = [PathBuf::from("/Applications")];
        let allowlist = [PathBuf::from("/Applications"), home.join("Applications")];
        let gate = Gate { allowlist: &allowlist, scan_root: None, protected: &protected, home: &home };
        for origin in [Origin::User, Origin::Agent] {
            assert_eq!(decide(Path::new("/Applications/Old.app"), origin, &gate), Decision::NeedsConfirm);
            assert_eq!(decide(&home.join("Applications/Old.app"), origin, &gate), Decision::NeedsConfirm, "allowlisted, still asks");
        }
        for still_refused in ["/Applications", "/Applications/Old.app/Contents", "/Applications/Utilities/Disk Utility.app", "/Applications/notes.txt"] {
            assert_eq!(decide(Path::new(still_refused), Origin::User, &gate), Decision::Refused("system folder"), "{still_refused}");
        }
    }

    #[test]
    fn nothing_inside_an_app_goes_without_asking() {
        let home = PathBuf::from("/home/u");
        let allowlist = [home.join("Applications"), home.join("Downloads")];
        let gate = Gate { allowlist: &allowlist, scan_root: None, protected: &[], home: &home };
        for p in ["Applications/Foo.app/Contents", "Applications/Chrome Apps/X.app", "Downloads/Foo.APP", "Downloads/Foo.app/Contents/MacOS/Foo"] {
            assert_eq!(decide(&home.join(p), Origin::Agent, &gate), Decision::NeedsConfirm, "{p}");
        }
        assert_eq!(decide(&home.join("Downloads/setup.dmg"), Origin::Agent, &gate), Decision::Immediate);
    }
}
