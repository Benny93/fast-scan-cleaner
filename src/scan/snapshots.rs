//! Small per-scan summaries (total + big folders) saved per scanned root, and their differences.

use crate::scan::{Kind, Node};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const MIN_FOLDER: u64 = 10_000_000;
pub const DEPTH: usize = 3;
pub const KEEP: usize = 100;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Unix seconds.
    pub at: u64,
    pub total: u64,
    /// Folders of at least `MIN_FOLDER` bytes, up to `DEPTH` levels, as ("a/b", size).
    pub folders: Vec<(String, u64)>,
}

pub fn take(tree: &Node, at: u64) -> Snapshot {
    fn walk(n: &Node, prefix: &str, depth: usize, out: &mut Vec<(String, u64)>) {
        for c in n.children.iter().filter(|c| matches!(c.kind, Kind::Dir) && c.size >= MIN_FOLDER) {
            let path = if prefix.is_empty() { c.name.to_string() } else { format!("{prefix}/{}", c.name) };
            out.push((path.clone(), c.size));
            if depth < DEPTH {
                walk(c, &path, depth + 1, out);
            }
        }
    }
    let mut folders = Vec::new();
    walk(tree, "", 1, &mut folders);
    Snapshot { at, total: tree.size, folders }
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Change {
    pub path: String,
    pub delta: i64,
    pub has_children: bool,
}

/// Changes of the direct children of `under` ("" = the root), biggest change first.
/// A folder missing from one snapshot counts as 0 there (it was below `MIN_FOLDER`).
// ponytail: linear lookups, fine for a few hundred folders per snapshot
pub fn diff(old: &Snapshot, new: &Snapshot, under: &str) -> Vec<Change> {
    fn rest<'a>(p: &'a str, under: &str) -> Option<&'a str> {
        if under.is_empty() { Some(p) } else { p.strip_prefix(under).and_then(|r| r.strip_prefix('/')) }
    }
    let all = || old.folders.iter().chain(&new.folders).map(|(p, _)| p.as_str());
    let size = |s: &Snapshot, p: &str| s.folders.iter().find(|(q, _)| q == p).map_or(0, |(_, n)| *n as i64);
    let mut paths: Vec<&str> = all().filter(|p| rest(p, under).is_some_and(|r| !r.contains('/'))).collect();
    paths.sort_unstable();
    paths.dedup();
    let mut out: Vec<Change> = paths
        .into_iter()
        .map(|p| Change { path: p.to_string(), delta: size(new, p) - size(old, p), has_children: all().any(|q| q.strip_prefix(p).is_some_and(|r| r.starts_with('/'))) })
        .filter(|c| c.delta != 0)
        .collect();
    out.sort_by_key(|c| std::cmp::Reverse(c.delta.unsigned_abs()));
    out
}

#[derive(Serialize, Deserialize)]
struct HistoryFile {
    root: PathBuf,
    snapshots: Vec<Snapshot>,
}

/// Readable start of the path plus a hash of all of it: similar roots ("my app", "my_app")
/// get their own file, and long roots stay under the file name length limit.
fn file_for(dir: &Path, root: &Path) -> PathBuf {
    let full = root.to_string_lossy();
    let name: String = full.chars().map(|c| if c.is_alphanumeric() { c } else { '_' }).take(60).collect();
    // FNV-1a: stable across Rust versions, unlike DefaultHasher.
    let hash = full.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    dir.join(format!("{name}-{hash:016x}.json"))
}

pub fn load(dir: &Path, root: &Path) -> Vec<Snapshot> {
    std::fs::read(file_for(dir, root))
        .ok()
        .and_then(|b| serde_json::from_slice::<HistoryFile>(&b).ok())
        .filter(|f| f.root == root)
        .map(|f| f.snapshots)
        .unwrap_or_default()
}

/// Appends `s`, keeps the newest `KEEP`, writes the file and returns the history.
/// History is a convenience: a write error is only logged.
pub fn record(dir: &Path, root: &Path, s: Snapshot) -> Vec<Snapshot> {
    let mut snapshots = load(dir, root);
    snapshots.push(s);
    let extra = snapshots.len().saturating_sub(KEEP);
    snapshots.drain(..extra);
    let file = HistoryFile { root: root.to_path_buf(), snapshots };
    // Write then rename, so a crash mid-write never leaves a truncated file behind.
    let path = file_for(dir, root);
    let tmp = path.with_extension("tmp");
    let written = std::fs::create_dir_all(dir)
        .and_then(|_| std::fs::write(&tmp, serde_json::to_vec(&file).map_err(std::io::Error::other)?))
        .and_then(|_| std::fs::rename(&tmp, &path));
    if let Err(e) = written {
        eprintln!("could not save scan history: {e}");
    }
    file.snapshots
}

/// "YYYY-MM-DD" in UTC (days-from-civil, Howard Hinnant).
pub fn day(secs: u64) -> String {
    let z = (secs / 86_400) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::Kind;

    const MB: u64 = 1_000_000;

    fn dir(name: &str, size: u64, children: Vec<Node>) -> Node {
        Node { name: name.into(), size, kind: Kind::Dir, children: children.into() }
    }
    fn file(name: &str, size: u64) -> Node {
        Node { name: name.into(), size, kind: Kind::File, children: Box::default() }
    }
    fn snap(at: u64, folders: &[(&str, u64)]) -> Snapshot {
        Snapshot { at, total: folders.iter().filter(|(p, _)| !p.contains('/')).map(|(_, n)| n).sum(), folders: folders.iter().map(|(p, n)| (p.to_string(), *n)).collect() }
    }

    #[test]
    fn take_keeps_big_folders_to_depth_three_and_no_files() {
        let deep = dir("d4", 50 * MB, vec![]);
        let tree = dir("/root", 200 * MB, vec![
            dir("a", 100 * MB, vec![dir("b", 80 * MB, vec![dir("c", 60 * MB, vec![deep])]), dir("small", 5 * MB, vec![])]),
            file("big.iso", 90 * MB),
        ]);
        let s = take(&tree, 7);
        assert_eq!(s.at, 7);
        assert_eq!(s.total, 200 * MB);
        assert_eq!(s.folders, vec![("a".into(), 100 * MB), ("a/b".into(), 80 * MB), ("a/b/c".into(), 60 * MB)]);
    }

    #[test]
    fn diff_lists_direct_children_by_biggest_change() {
        let old = snap(1, &[("a", 10 * MB), ("b", 50 * MB), ("c", 20 * MB), ("a/x", 10 * MB)]);
        let new = snap(2, &[("a", 40 * MB), ("b", 45 * MB), ("c", 20 * MB), ("d", 12 * MB), ("a/x", 35 * MB)]);
        let d = diff(&old, &new, "");
        let got: Vec<(&str, i64, bool)> = d.iter().map(|c| (c.path.as_str(), c.delta, c.has_children)).collect();
        assert_eq!(got, [("a", 30_000_000, true), ("d", 12_000_000, false), ("b", -5_000_000, false)]);
    }

    #[test]
    fn diff_under_a_folder_and_missing_counts_as_zero() {
        let old = snap(1, &[("a", 30 * MB), ("a/x", 20 * MB), ("a/x/deep", 15 * MB)]);
        let new = snap(2, &[("a", 30 * MB), ("a/y", 25 * MB)]);
        let got: Vec<(String, i64)> = diff(&old, &new, "a").into_iter().map(|c| (c.path, c.delta)).collect();
        assert_eq!(got, [("a/y".to_string(), 25_000_000), ("a/x".to_string(), -20_000_000)]);
    }

    #[test]
    fn record_round_trips_and_caps() {
        let t = tempfile::tempdir().unwrap();
        let root = Path::new("/Users/Shared/Demo");
        for i in 0..(KEEP as u64 + 3) {
            record(t.path(), root, snap(i, &[]));
        }
        let h = load(t.path(), root);
        assert_eq!(h.len(), KEEP);
        assert_eq!(h[0].at, 3);
        assert_eq!(h.last().unwrap().at, KEEP as u64 + 2);
    }

    #[test]
    fn corrupt_file_is_empty_history() {
        let t = tempfile::tempdir().unwrap();
        let root = Path::new("/x");
        record(t.path(), root, snap(1, &[]));
        let f = std::fs::read_dir(t.path()).unwrap().next().unwrap().unwrap().path();
        std::fs::write(&f, b"{not json").unwrap();
        assert!(load(t.path(), root).is_empty());
        assert_eq!(record(t.path(), root, snap(2, &[])).len(), 1);
    }

    #[test]
    fn load_ignores_other_root() {
        let t = tempfile::tempdir().unwrap();
        record(t.path(), Path::new("/a-b"), snap(1, &[]));
        assert!(load(t.path(), Path::new("/a_b")).is_empty());
        assert_eq!(load(t.path(), Path::new("/a-b")).len(), 1);
    }

    #[test]
    fn similar_roots_keep_their_own_history() {
        let t = tempfile::tempdir().unwrap();
        record(t.path(), Path::new("/p/my app"), snap(1, &[]));
        record(t.path(), Path::new("/p/my_app"), snap(2, &[]));
        record(t.path(), Path::new("/p/my app"), snap(3, &[]));
        assert_eq!(load(t.path(), Path::new("/p/my app")).len(), 2);
        assert_eq!(load(t.path(), Path::new("/p/my_app")).len(), 1);
    }

    #[test]
    fn very_long_roots_are_saved() {
        let t = tempfile::tempdir().unwrap();
        let root = PathBuf::from(format!("/{}", "x".repeat(400)));
        record(t.path(), &root, snap(1, &[]));
        assert_eq!(load(t.path(), &root).len(), 1);
        assert!(std::fs::read_dir(t.path()).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".tmp")));
    }

    #[test]
    fn day_formats_utc_dates() {
        assert_eq!(day(0), "1970-01-01");
        assert_eq!(day(951_782_400), "2000-02-29");
        assert_eq!(day(1_700_000_000), "2023-11-14");
    }
}
