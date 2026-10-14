//! Identical files in the scanned folder: same size, then same first 64 KiB, then same content.

use crate::scan::{Kind, Node};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

pub const MIN_SIZE: u64 = 1_000_000;
const HEAD: usize = 64 * 1024;
const CHUNK: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Copy {
    pub path: PathBuf,
    /// Unix seconds.
    pub modified: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Group {
    pub size: u64,
    /// Sorted by path.
    pub copies: Vec<Copy>,
}

impl Group {
    /// Bytes freed by keeping one copy.
    pub fn wasted(&self) -> u64 {
        self.size * (self.copies.len() as u64).saturating_sub(1)
    }
}

#[derive(Debug, Default)]
pub struct DupeProgress {
    pub cancel: AtomicBool,
    pub files_done: AtomicU64,
    pub files_total: AtomicU64,
    pub bytes_read: AtomicU64,
}

/// Files of at least `MIN_SIZE` whose exact size occurs more than once: only those can have a copy.
pub fn candidates(tree: &Node, root: &Path) -> Vec<(PathBuf, u64)> {
    fn walk(n: &Node, path: &mut PathBuf, out: &mut Vec<(PathBuf, u64)>) {
        for c in n.children.iter().filter(|c| c.size >= MIN_SIZE) {
            path.push(&*c.name);
            match c.kind {
                Kind::File => out.push((path.clone(), c.size)),
                Kind::Dir => walk(c, path, out),
                Kind::Symlink => {}
            }
            path.pop();
        }
    }
    let mut all = Vec::new();
    walk(tree, &mut root.to_path_buf(), &mut all);
    let mut count: HashMap<u64, usize> = HashMap::new();
    for (_, s) in &all {
        *count.entry(*s).or_default() += 1;
    }
    all.retain(|(_, s)| count[s] > 1);
    all
}

type File = (PathBuf, u64, u64); // path, size, modified

/// None when cancelled. Unreadable, vanished or resized files are skipped.
// ponytail: 64-bit DefaultHasher, no crypto dependency; a false match needs equal size, equal
// first 64 KiB and a 2^-64 collision, and the copy still only goes to the Trash.
pub fn find(cands: Vec<(PathBuf, u64)>, p: &DupeProgress) -> Option<Vec<Group>> {
    // Annotated: the only insert is under cfg(unix), so Windows cannot infer the element type.
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    let files: Vec<File> = cands
        .into_iter()
        // The tree holds space on disk (whole blocks); group by the real length from here on.
        .filter_map(|(path, _)| {
            let m = std::fs::symlink_metadata(&path).ok().filter(|m| m.is_file())?;
            let size = m.len();
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if !seen.insert((m.dev(), m.ino())) {
                    return None; // hard link to a file already listed: deleting it frees nothing
                }
            }
            #[cfg(not(unix))]
            let _ = &mut seen;
            let modified = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs());
            Some((path, size, modified))
        })
        .collect();
    p.files_total.store(files.len() as u64, Relaxed);
    let heads = pass(files, Some(HEAD), p);
    if p.cancel.load(Relaxed) {
        return None;
    }
    let survivors: Vec<File> = heads.into_iter().flatten().collect();
    p.files_total.fetch_add(survivors.len() as u64, Relaxed);
    let full = pass(survivors, None, p);
    if p.cancel.load(Relaxed) {
        return None;
    }
    let mut groups: Vec<Group> = full
        .into_iter()
        .map(|mut v| {
            v.sort_by(|a, b| a.0.cmp(&b.0));
            Group { size: v[0].1, copies: v.into_iter().map(|(path, _, modified)| Copy { path, modified }).collect() }
        })
        .collect();
    groups.sort_by(|a, b| b.wasted().cmp(&a.wasted()).then_with(|| a.copies[0].path.cmp(&b.copies[0].path)));
    Some(groups)
}

/// Hashes every file (whole, or its first `limit` bytes) and returns the buckets with 2+ files.
fn pass(files: Vec<File>, limit: Option<usize>, p: &DupeProgress) -> Vec<Vec<File>> {
    let hashed: Vec<(File, Option<u64>)> = files
        .into_par_iter()
        .map(|f| {
            let h = hash_file(&f.0, limit, p);
            p.files_done.fetch_add(1, Relaxed);
            (f, h)
        })
        .collect();
    let mut by: HashMap<(u64, u64), Vec<File>> = HashMap::new();
    for (f, h) in hashed {
        if let Some(h) = h {
            by.entry((f.1, h)).or_default().push(f);
        }
    }
    by.into_values().filter(|v| v.len() > 1).collect()
}

fn hash_file(path: &Path, limit: Option<usize>, p: &DupeProgress) -> Option<u64> {
    let mut f = std::fs::File::open(path).ok()?;
    // Reading sets the access time on Linux (relatime), which "Old files" reads as "used".
    let accessed = f.metadata().and_then(|m| m.accessed()).ok();
    let hash = hash_open(&mut f, limit, p);
    if let Some(t) = accessed {
        let _ = f.set_times(std::fs::FileTimes::new().set_accessed(t));
    }
    hash
}

fn hash_open(f: &mut std::fs::File, limit: Option<usize>, p: &DupeProgress) -> Option<u64> {
    let mut h = DefaultHasher::new();
    let mut left = limit.unwrap_or(usize::MAX);
    let mut buf = vec![0u8; CHUNK.min(left)];
    while left > 0 {
        if p.cancel.load(Relaxed) {
            return None;
        }
        let want = buf.len().min(left);
        let n = f.read(&mut buf[..want]).ok()?;
        if n == 0 {
            break;
        }
        h.write(&buf[..n]);
        p.bytes_read.fetch_add(n as u64, Relaxed);
        left -= n;
    }
    Some(h.finish())
}

/// The copy to keep: the user's pick if one is in the group; else, in this order, a copy outside
/// the folders the cleanup suggests removing (`avoid`), outside Downloads/Desktop, the oldest,
/// the shortest path.
pub fn pick_keep(g: &Group, picks: &HashSet<PathBuf>, home: &Path, avoid: &[PathBuf]) -> usize {
    if let Some(i) = g.copies.iter().position(|c| picks.contains(&c.path)) {
        return i;
    }
    let (downloads, desktop) = (home.join("Downloads"), home.join("Desktop"));
    let rank = |c: &Copy| (avoid.iter().any(|a| c.path.starts_with(a)), c.path.starts_with(&downloads) || c.path.starts_with(&desktop), c.modified, c.path.as_os_str().len());
    (0..g.copies.len()).min_by_key(|&i| rank(&g.copies[i])).unwrap_or(0)
}

/// A copy that would go to the Trash, and the copy that stays.
#[derive(Debug, Clone, PartialEq)]
pub struct Removal {
    pub path: PathBuf,
    pub size: u64,
    pub modified: u64,
    pub kept: PathBuf,
}

impl Removal {
    /// Checked right before trashing: the kept copy still exists with the same length, and this
    /// copy was not changed since the search (else it may no longer be a copy).
    pub fn still_safe(&self) -> bool {
        let len = |p: &Path| std::fs::metadata(p).ok().map(|m| (m.len(), m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs())));
        len(&self.kept).is_some_and(|(n, _)| n == self.size) && len(&self.path) == Some((self.size, self.modified))
    }
}

/// Every non-kept copy of every group.
pub fn removals(groups: &[Group], picks: &HashSet<PathBuf>, home: &Path, avoid: &[PathBuf]) -> Vec<Removal> {
    let mut out = Vec::new();
    for g in groups {
        let kept = &g.copies[pick_keep(g, picks, home, avoid)].path;
        out.extend(g.copies.iter().filter(|c| &c.path != kept).map(|c| Removal { path: c.path.clone(), size: g.size, modified: c.modified, kept: kept.clone() }));
    }
    out
}

/// Drops removals whose kept copy lies in a folder that goes to the Trash in the same step.
pub fn without_kept_in(rs: Vec<Removal>, trashed: &[PathBuf]) -> Vec<Removal> {
    rs.into_iter().filter(|r| !trashed.iter().any(|t| r.kept.starts_with(t))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::Kind;

    const MB: usize = 1_000_000;

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
        p
    }
    fn cands(paths: &[&PathBuf]) -> Vec<(PathBuf, u64)> {
        paths.iter().map(|p| ((*p).clone(), std::fs::metadata(p).unwrap().len())).collect()
    }
    fn names(gs: &[Group]) -> Vec<Vec<String>> {
        gs.iter().map(|g| g.copies.iter().map(|c| c.path.file_name().unwrap().to_string_lossy().into_owned()).collect()).collect()
    }

    #[test]
    fn identical_files_group_and_lookalikes_do_not() {
        let t = tempfile::tempdir().unwrap();
        let a = vec![1u8; 2 * MB];
        let mut tail = a.clone();
        *tail.last_mut().unwrap() = 9; // same size, same first 64 KiB, different end
        let mut head = a.clone();
        head[0] = 9; // same size, different start
        let p = [write(t.path(), "a1", &a), write(t.path(), "a2", &a), write(t.path(), "sub/a3", &a), write(t.path(), "tail", &tail), write(t.path(), "head", &head)];
        let gs = find(cands(&p.iter().collect::<Vec<_>>()), &DupeProgress::default()).unwrap();
        assert_eq!(names(&gs), [["a1", "a2", "a3"]]);
        assert_eq!(gs[0].size, 2 * MB as u64);
        assert_eq!(gs[0].wasted(), 4 * MB as u64);
    }

    #[test]
    fn groups_sorted_by_wasted_bytes() {
        let t = tempfile::tempdir().unwrap();
        let small = vec![1u8; MB + 1];
        let big = vec![2u8; 3 * MB];
        let p = [write(t.path(), "s1", &small), write(t.path(), "s2", &small), write(t.path(), "s3", &small), write(t.path(), "b1", &big), write(t.path(), "b2", &big)];
        let gs = find(cands(&p.iter().collect::<Vec<_>>()), &DupeProgress::default()).unwrap();
        assert_eq!(names(&gs), [vec!["b1", "b2"], vec!["s1", "s2", "s3"]]);
    }

    #[cfg(unix)]
    #[test]
    fn hard_links_are_not_duplicates() {
        let t = tempfile::tempdir().unwrap();
        let a = write(t.path(), "a", &vec![3u8; 2 * MB]);
        let b = t.path().join("b");
        std::fs::hard_link(&a, &b).unwrap();
        assert!(find(cands(&[&a, &b]), &DupeProgress::default()).unwrap().is_empty());
    }

    #[test]
    fn vanished_or_resized_files_are_skipped() {
        let t = tempfile::tempdir().unwrap();
        let data = vec![4u8; 2 * MB];
        let a = write(t.path(), "a", &data);
        let b = write(t.path(), "b", &data);
        let c = write(t.path(), "c", &data);
        let list = cands(&[&a, &b, &c]);
        std::fs::remove_file(&b).unwrap();
        std::fs::write(&c, vec![4u8; 3 * MB]).unwrap();
        assert!(find(list, &DupeProgress::default()).unwrap().is_empty());
    }

    #[test]
    fn cancel_returns_none() {
        let t = tempfile::tempdir().unwrap();
        let data = vec![5u8; 2 * MB];
        let p = [write(t.path(), "a", &data), write(t.path(), "b", &data)];
        let prog = DupeProgress::default();
        prog.cancel.store(true, Relaxed);
        assert_eq!(find(cands(&p.iter().collect::<Vec<_>>()), &prog), None);
    }

    #[test]
    fn candidates_need_a_size_twin_and_one_megabyte() {
        let f = |n: &str, s: u64| Node { name: n.into(), size: s, kind: Kind::File, children: Box::default() };
        let d = |n: &str, ch: Vec<Node>| Node { name: n.into(), size: ch.iter().map(|c| c.size).sum(), kind: Kind::Dir, children: ch.into() };
        let tree = d("/r", vec![d("x", vec![f("a", 2_000_000), f("tiny1", 10), f("tiny2", 10)]), f("b", 2_000_000), f("lonely", 5_000_000)]);
        let mut got = candidates(&tree, Path::new("/r"));
        got.sort();
        assert_eq!(got, [(PathBuf::from("/r/b"), 2_000_000), (PathBuf::from("/r/x/a"), 2_000_000)]);
    }

    #[test]
    fn keep_avoids_copies_inside_cleanup_findings() {
        let c = |p: &str, m| Copy { path: p.into(), modified: m };
        let g = Group { size: 1, copies: vec![c("/h/Library/Caches/x", 1), c("/h/Documents/x", 9)] };
        assert_eq!(pick_keep(&g, &HashSet::new(), Path::new("/h"), &[PathBuf::from("/h/Library/Caches")]), 1);
    }

    #[test]
    fn keep_pick_survives_when_another_copy_leaves() {
        let c = |p: &str, m| Copy { path: p.into(), modified: m };
        let picks: HashSet<PathBuf> = [PathBuf::from("/h/c")].into();
        let g = Group { size: 1, copies: vec![c("/h/b", 1), c("/h/c", 9)] }; // "/h/a" was trashed
        assert_eq!(pick_keep(&g, &picks, Path::new("/h"), &[]), 1);
    }

    #[test]
    fn removal_only_when_kept_exists_and_copy_is_unchanged() {
        let t = tempfile::tempdir().unwrap();
        let data = vec![7u8; 2 * MB];
        let kept = write(t.path(), "kept", &data);
        let copy = write(t.path(), "copy", &data);
        let modified = |p: &Path| std::fs::metadata(p).unwrap().modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let r = Removal { path: copy.clone(), size: 2 * MB as u64, modified: modified(&copy), kept: kept.clone() };
        assert!(r.still_safe());
        std::fs::write(&copy, vec![8u8; 3 * MB]).unwrap();
        assert!(!r.still_safe(), "copy changed since the search");
        let r = Removal { path: copy.clone(), size: 3 * MB as u64, modified: modified(&copy), kept: kept.clone() };
        std::fs::remove_file(&kept).unwrap();
        assert!(!r.still_safe(), "kept copy is gone");
    }

    #[test]
    fn copies_whose_kept_file_is_trashed_too_are_dropped() {
        let r = |p: &str, k: &str| Removal { path: p.into(), size: 1, modified: 0, kept: k.into() };
        let rs = vec![r("/h/Documents/x", "/h/Library/Caches/x"), r("/h/Videos/y", "/h/Photos/y")];
        let kept: Vec<PathBuf> = without_kept_in(rs, &[PathBuf::from("/h/Library/Caches")]).into_iter().map(|r| r.path).collect();
        assert_eq!(kept, [PathBuf::from("/h/Videos/y")]);
    }

    #[test]
    fn default_keep_prefers_non_downloads_then_oldest() {
        let c = |p: &str, m| Copy { path: p.into(), modified: m };
        let home = Path::new("/h");
        let g = Group { size: 1, copies: vec![c("/h/Downloads/x", 1), c("/h/Photos/x", 9), c("/h/Docs/x", 5)] };
        assert_eq!(pick_keep(&g, &HashSet::new(), home, &[]), 2);
        let g = Group { size: 1, copies: vec![c("/h/Desktop/x", 1), c("/h/Downloads/x", 2)] };
        assert_eq!(pick_keep(&g, &HashSet::new(), home, &[]), 0);
    }

    #[test]
    fn hashing_keeps_the_access_time() {
        // Old files rely on access times off macOS; our own reading must not make a file look used.
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("a.bin");
        std::fs::write(&p, vec![7u8; 100_000]).unwrap();
        let old = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_500_000_000);
        std::fs::File::options().write(true).open(&p).unwrap().set_times(std::fs::FileTimes::new().set_accessed(old).set_modified(old)).unwrap();
        assert!(hash_file(&p, None, &DupeProgress::default()).is_some());
        assert_eq!(std::fs::metadata(&p).unwrap().accessed().unwrap(), old);
    }
}
