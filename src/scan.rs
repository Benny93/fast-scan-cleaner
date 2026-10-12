use rayon::prelude::*;
use std::collections::{BinaryHeap, HashSet};
use std::cmp::Reverse;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    Symlink,
}

// ponytail: names via to_string_lossy; non-UTF-8 file names display fine but can't be trashed through the tree
#[derive(Debug, Clone)]
// Millions of these live in memory at once, so they use boxed slices (no spare capacity, 16 instead of 24 bytes each).
pub struct Node {
    pub name: Box<str>,
    pub size: u64,
    pub kind: Kind,
    /// Sorted by size, largest first.
    pub children: Box<[Node]>,
}

#[derive(Debug, Default)]
pub struct Progress {
    pub files: AtomicU64,
    /// Allocated bytes of the files counted so far.
    pub bytes: AtomicU64,
    pub denied: AtomicU64,
    pub cancel: AtomicBool,
    /// Folder being read right now (for display only).
    pub current: Mutex<PathBuf>,
    /// Running sizes of the top two folder levels, for the live map while scanning.
    pub live: Arc<Live>,
}

/// A folder whose size grows while the scan runs.
#[derive(Debug, Default)]
pub struct Live {
    pub name: String,
    pub bytes: AtomicU64,
    pub children: Mutex<Vec<Arc<Live>>>,
}

impl Live {
    fn new(name: String) -> Arc<Live> {
        Arc::new(Live { name, ..Live::default() })
    }

    /// The sizes counted so far as a small tree (folders only, largest first).
    pub fn snapshot(&self, name: &str) -> Node {
        let mut children: Vec<Node> = self.children.lock().unwrap().iter().map(|c| c.snapshot(&c.name)).filter(|n| n.size > 0).collect();
        children.sort_unstable_by(|a, b| b.size.cmp(&a.size));
        let own = self.bytes.load(Relaxed);
        let size = if own > 0 { own } else { children.iter().map(|c| c.size).sum() };
        Node { name: name.into(), size, kind: Kind::Dir, children: children.into() }
    }
}

struct Ctx<'a> {
    progress: &'a Progress,
    dev: u64,
    seen_inodes: Mutex<HashSet<(u64, u64)>>,
}

pub fn scan(root: &Path, progress: &Progress) -> std::io::Result<Node> {
    let meta = fs::symlink_metadata(root)?;
    let name = root.to_string_lossy().into_owned();
    let root_entry = Entry::from_meta(name, &meta);
    let ctx = Ctx { progress, dev: root_entry.dev, seen_inodes: Mutex::default() };
    Ok(if meta.is_dir() { scan_dir(root, root_entry.name, &ctx, 0, &progress.live, &[]) } else { leaf(root_entry, &ctx) })
}

/// `live` is this folder's live counter while `depth` < 2; `chain` holds the live counters
/// of the level 1 and 2 folders this folder is in (or is).
fn scan_dir(path: &Path, name: String, ctx: &Ctx, depth: u8, live: &Arc<Live>, chain: &[Arc<Live>]) -> Node {
    let mut node = Node { name: name.into(), size: 0, kind: Kind::Dir, children: Box::default() };
    if ctx.progress.cancel.load(Relaxed) {
        return node;
    }
    // ponytail: one shared lock per folder entered; switch to a per-thread slot if it ever shows up in profiles
    *ctx.progress.current.lock().unwrap() = path.to_path_buf();
    let Ok(entries) = list(path) else {
        ctx.progress.denied.fetch_add(1, Relaxed);
        return node;
    };
    node.children = entries
        .into_par_iter()
        .filter_map(|e| match e.kind {
            Kind::Dir => {
                let p = path.join(&e.name);
                if crate::platform::skip_dir(&p) || !same_device(&e, ctx) {
                    return None;
                }
                Some(if depth < 2 {
                    let child = Live::new(e.name.clone());
                    live.children.lock().unwrap().push(child.clone());
                    let chain = [chain, std::slice::from_ref(&child)].concat();
                    scan_dir(&p, e.name, ctx, depth + 1, &child, &chain)
                } else {
                    scan_dir(&p, e.name, ctx, depth + 1, live, chain)
                })
            }
            _ => Some(leaf(e, ctx)),
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();
    node.children.sort_unstable_by(|a, b| b.size.cmp(&a.size));
    node.size = node.children.iter().map(|c| c.size).sum();
    // One atomic add per folder (not per file) keeps the live map cheap.
    let own: u64 = node.children.iter().filter(|c| c.kind != Kind::Dir).map(|c| c.size).sum();
    for l in chain {
        l.bytes.fetch_add(own, Relaxed);
    }
    node
}

/// One folder entry with the metadata the scan needs.
struct Entry {
    name: String,
    kind: Kind,
    /// Allocated bytes (0 for folders and symlinks).
    size: u64,
    dev: u64,
    ino: u64,
    nlink: u64,
}

impl Entry {
    fn from_meta(name: String, meta: &fs::Metadata) -> Entry {
        // Windows junctions also report is_symlink(), so they are never followed.
        let kind = if meta.is_symlink() {
            Kind::Symlink
        } else if meta.is_dir() {
            Kind::Dir
        } else {
            Kind::File
        };
        #[cfg(unix)]
        let (size, dev, ino, nlink) = {
            use std::os::unix::fs::MetadataExt;
            (meta.blocks() * 512, meta.dev(), meta.ino(), meta.nlink())
        };
        #[cfg(not(unix))]
        let (size, dev, ino, nlink) = (meta.len(), 0, 0, 1); // ponytail: logical size and no hard-link dedupe on Windows; use GetCompressedFileSizeW + file index if it matters
        let (size, nlink) = if kind == Kind::File { (size, nlink) } else { (0, 1) };
        Entry { name, kind, size, dev, ino, nlink }
    }
}

/// Read a folder: one getattrlistbulk call per batch of entries on macOS (no stat per file),
/// readdir + lstat elsewhere or when the file system does not support it.
fn list(path: &Path) -> std::io::Result<Vec<Entry>> {
    #[cfg(target_os = "macos")]
    if let Some(entries) = bulk::list(path)? {
        return Ok(entries);
    }
    let entries: Vec<_> = fs::read_dir(path)?.filter_map(Result::ok).collect();
    Ok(entries
        .into_par_iter()
        .filter_map(|e| {
            let meta = fs::symlink_metadata(e.path()).ok()?;
            Some(Entry::from_meta(e.file_name().to_string_lossy().into_owned(), &meta))
        })
        .collect())
}

#[cfg(target_os = "macos")]
mod bulk {
    use super::{Entry, Kind};
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    // Not exported by the libc crate (sys/attr.h, sys/vnode.h).
    const ATTR_CMN_ERROR: u32 = 0x2000_0000;
    const VDIR: u32 = 2;
    const VLNK: u32 = 5;

    /// `Ok(None)` when the file system has no bulk support.
    pub fn list(path: &Path) -> std::io::Result<Option<Vec<Entry>>> {
        let c = CString::new(path.as_os_str().as_bytes())?;
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let res = read_all(fd);
        unsafe { libc::close(fd) };
        match res {
            Err(e) if e.raw_os_error() == Some(libc::ENOTSUP) => Ok(None),
            r => r.map(Some),
        }
    }

    fn read_all(fd: libc::c_int) -> std::io::Result<Vec<Entry>> {
        let mut al: libc::attrlist = unsafe { std::mem::zeroed() };
        al.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
        // Packed in this order: returned attrs, error, name, devid, objtype, fileid; then linkcount, allocsize.
        al.commonattr = libc::ATTR_CMN_RETURNED_ATTRS
            | ATTR_CMN_ERROR
            | libc::ATTR_CMN_NAME
            | libc::ATTR_CMN_DEVID
            | libc::ATTR_CMN_OBJTYPE
            | libc::ATTR_CMN_FILEID;
        al.fileattr = libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_ALLOCSIZE;
        let mut buf = vec![0u8; 128 * 1024];
        let mut out = Vec::new();
        loop {
            let n = unsafe {
                libc::getattrlistbulk(fd, (&raw mut al).cast(), buf.as_mut_ptr().cast(), buf.len(), libc::FSOPT_PACK_INVAL_ATTRS as u64)
            };
            if n < 0 {
                return Err(std::io::Error::last_os_error());
            }
            if n == 0 {
                return Ok(out);
            }
            let mut off = 0;
            for _ in 0..n {
                let rec = &buf[off..];
                let len = u32_at(rec, 0) as usize;
                off += len;
                // rec[4..24] = attribute_set_t of returned attrs (unused: invalid ones are packed as zero).
                if u32_at(rec, 24) != 0 {
                    continue; // this entry could not be read
                }
                let name_off = 28 + i32_at(rec, 28) as usize;
                let name_len = u32_at(rec, 32) as usize; // includes the trailing NUL
                let name = String::from_utf8_lossy(&rec[name_off..name_off + name_len.saturating_sub(1)]).into_owned();
                let dev = i32_at(rec, 36) as u64; // same widening as MetadataExt::dev
                let kind = match u32_at(rec, 40) {
                    VDIR => Kind::Dir,
                    VLNK => Kind::Symlink,
                    _ => Kind::File,
                };
                let ino = u64::from_ne_bytes(rec[44..52].try_into().unwrap());
                // File attributes are only meaningful for files.
                let (nlink, size) = match kind {
                    Kind::File => (u32_at(rec, 52) as u64, i64::from_ne_bytes(rec[56..64].try_into().unwrap()).max(0) as u64),
                    _ => (1, 0),
                };
                out.push(Entry { name, kind, size, dev, ino, nlink });
            }
        }
    }

    fn u32_at(b: &[u8], i: usize) -> u32 {
        u32::from_ne_bytes(b[i..i + 4].try_into().unwrap())
    }

    fn i32_at(b: &[u8], i: usize) -> i32 {
        i32::from_ne_bytes(b[i..i + 4].try_into().unwrap())
    }
}

fn leaf(e: Entry, ctx: &Ctx) -> Node {
    ctx.progress.files.fetch_add(1, Relaxed);
    let size = if e.nlink > 1 && !ctx.seen_inodes.lock().unwrap().insert((e.dev, e.ino)) { 0 } else { e.size };
    ctx.progress.bytes.fetch_add(size, Relaxed);
    Node { name: e.name.into(), size, kind: e.kind, children: Box::default() }
}

fn same_device(e: &Entry, ctx: &Ctx) -> bool {
    // Mounted folders on Windows are junctions, already skipped as symlinks (dev is 0 there).
    e.dev == ctx.dev
}

/// Split `rel` into its first component and the rest.
fn split(rel: &Path) -> Option<(String, &Path)> {
    let mut comps = rel.components();
    let first = comps.next()?.as_os_str().to_string_lossy().into_owned();
    Some((first, comps.as_path()))
}

impl Node {
    pub fn find(&self, rel: &Path) -> Option<&Node> {
        rel.components().try_fold(self, |node, c| {
            let name = c.as_os_str().to_string_lossy();
            node.children.iter().find(|ch| *ch.name == *name)
        })
    }

    pub fn remove(&mut self, rel: &Path) -> Option<u64> {
        let (first, rest) = split(rel)?;
        let i = self.children.iter().position(|c| *c.name == *first)?;
        let removed = if rest.as_os_str().is_empty() {
            let mut children = std::mem::take(&mut self.children).into_vec();
            let size = children.remove(i).size;
            self.children = children.into_boxed_slice();
            size
        } else {
            self.children[i].remove(rest)?
        };
        self.size = self.size.saturating_sub(removed);
        self.children.sort_by(|a, b| b.size.cmp(&a.size));
        Some(removed)
    }

    pub fn replace(&mut self, rel: &Path, mut new: Node) -> Option<()> {
        let (first, rest) = split(rel)?;
        let i = self.children.iter().position(|c| *c.name == *first)?;
        let old = self.children[i].size;
        if rest.as_os_str().is_empty() {
            new.name = std::mem::take(&mut self.children[i].name);
            self.children[i] = new;
        } else {
            self.children[i].replace(rest, new)?;
        }
        self.size = self.size - old + self.children[i].size;
        self.children.sort_by(|a, b| b.size.cmp(&a.size));
        Some(())
    }
}

pub fn largest_files(node: &Node, prefix: &Path, limit: usize) -> Vec<(PathBuf, u64)> {
    fn walk(node: &Node, path: &Path, limit: usize, heap: &mut BinaryHeap<Reverse<(u64, PathBuf)>>) {
        for c in &node.children {
            if c.kind == Kind::Dir {
                walk(c, &path.join(&*c.name), limit, heap);
            } else if heap.len() < limit || heap.peek().is_some_and(|Reverse((s, _))| c.size > *s) {
                heap.push(Reverse((c.size, path.join(&*c.name))));
                if heap.len() > limit {
                    heap.pop();
                }
            }
        }
    }
    let mut heap = BinaryHeap::new();
    walk(node, prefix, limit, &mut heap);
    let mut out: Vec<_> = heap.into_iter().map(|Reverse((s, p))| (p, s)).collect();
    out.sort_by(|a, b| b.1.cmp(&a.1));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(dir: &Path, rel: &str, len: usize) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, vec![7u8; len]).unwrap();
    }

    fn leaf(name: &str, size: u64) -> Node {
        Node { name: name.into(), size, kind: Kind::File, children: Box::default() }
    }

    fn dir(name: &str, children: Vec<Node>) -> Node {
        let mut children = children;
        children.sort_by(|a, b| b.size.cmp(&a.size));
        Node { name: name.into(), size: children.iter().map(|c| c.size).sum(), kind: Kind::Dir, children: children.into() }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn bulk_listing_matches_lstat() {
        let t = tempfile::tempdir().unwrap();
        file(t.path(), "größe 文件.bin", 70_000);
        file(t.path(), "sub/x", 10);
        std::os::unix::fs::symlink("sub", t.path().join("link")).unwrap();
        fs::hard_link(t.path().join("sub/x"), t.path().join("hard")).unwrap();
        let key = |e: &Entry| (e.name.clone(), e.kind, e.size, e.dev, e.ino, e.nlink);
        let mut bulk: Vec<_> = bulk::list(t.path()).unwrap().unwrap().iter().map(key).collect();
        let mut stat: Vec<_> = fs::read_dir(t.path())
            .unwrap()
            .map(|e| key(&Entry::from_meta(e.as_ref().unwrap().file_name().to_string_lossy().into_owned(), &fs::symlink_metadata(e.unwrap().path()).unwrap())))
            .collect();
        bulk.sort_by(|a, b| a.0.cmp(&b.0));
        stat.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(bulk.len(), 4);
        assert_eq!(bulk, stat);
    }

    #[test]
    fn live_sizes_match_the_final_tree() {
        let t = tempfile::tempdir().unwrap();
        file(t.path(), "a/b/c/d/x", 9000);
        file(t.path(), "a/y", 5000);
        file(t.path(), "e/z", 100);
        file(t.path(), "top", 7);
        let p = Progress::default();
        let tree = scan(t.path(), &p).unwrap();
        let live = p.live.snapshot("root");
        let shape = |n: &Node| -> Vec<(String, u64, Vec<(String, u64)>)> {
            n.children
                .iter()
                .filter(|c| c.kind == Kind::Dir)
                .map(|c| (c.name.to_string(), c.size, c.children.iter().filter(|g| g.kind == Kind::Dir).map(|g| (g.name.to_string(), g.size)).collect()))
                .collect()
        };
        assert_eq!(shape(&live), shape(&tree));
        assert_eq!(live.children.len(), 2);
    }

    #[test]
    fn scans_sizes_sorted_and_summed() {
        let tmp = tempfile::tempdir().unwrap();
        file(tmp.path(), "big/a.bin", 200_000);
        file(tmp.path(), "big/b.bin", 100_000);
        file(tmp.path(), "small.txt", 10);
        let p = Progress::default();
        let root = scan(tmp.path(), &p).unwrap();
        assert_eq!(root.kind, Kind::Dir);
        assert_eq!(&*root.children[0].name, "big");
        assert!(root.children[0].size >= 300_000);
        assert_eq!(root.size, root.children.iter().map(|c| c.size).sum::<u64>());
        assert_eq!(p.files.load(Relaxed), 3);
        assert_eq!(root.find(Path::new("big/a.bin")).unwrap().kind, Kind::File);
        assert_eq!(p.bytes.load(Relaxed), root.size);
        assert!(p.current.lock().unwrap().starts_with(tmp.path()));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed_and_loops_terminate() {
        let tmp = tempfile::tempdir().unwrap();
        file(tmp.path(), "real/data.bin", 100_000);
        std::os::unix::fs::symlink(tmp.path().join("real"), tmp.path().join("link")).unwrap();
        std::os::unix::fs::symlink(tmp.path(), tmp.path().join("real/loop")).unwrap();
        let root = scan(tmp.path(), &Progress::default()).unwrap();
        let link = root.find(Path::new("link")).unwrap();
        assert_eq!((link.kind, link.size), (Kind::Symlink, 0));
        assert_eq!(root.find(Path::new("real/loop")).unwrap().kind, Kind::Symlink);
    }

    #[cfg(unix)]
    #[test]
    fn hard_links_are_counted_once() {
        let tmp = tempfile::tempdir().unwrap();
        file(tmp.path(), "a.bin", 100_000);
        fs::hard_link(tmp.path().join("a.bin"), tmp.path().join("b.bin")).unwrap();
        let root = scan(tmp.path(), &Progress::default()).unwrap();
        let a = root.children.iter().map(|c| c.size).max().unwrap();
        assert_eq!(root.size, a);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_dirs_are_counted_and_skipped() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        file(tmp.path(), "locked/secret.bin", 1000);
        let locked = tmp.path().join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let p = Progress::default();
        let result = scan(tmp.path(), &p);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        let root = result.unwrap();
        // root can read anything, so the lock only takes effect for normal users
        if std::env::var("USER").as_deref() != Ok("root") {
            assert_eq!(p.denied.load(Relaxed), 1);
            assert_eq!(root.find(Path::new("locked")).unwrap().size, 0);
        }
    }

    #[test]
    fn missing_root_is_an_error() {
        assert!(scan(Path::new("/definitely/not/here"), &Progress::default()).is_err());
    }

    #[test]
    fn remove_shrinks_ancestors() {
        let mut t = dir("root", vec![dir("a", vec![leaf("x", 10), leaf("y", 5)]), leaf("b", 8)]);
        assert_eq!(t.remove(Path::new("a/x")), Some(10));
        assert_eq!(t.size, 13);
        assert_eq!(t.find(Path::new("a")).unwrap().size, 5);
        assert_eq!(&*t.children[0].name, "b"); // re-sorted: b (8) > a (5)
        assert_eq!(t.remove(Path::new("nope")), None);
        assert_eq!(t.size, 13);
    }

    #[test]
    fn replace_resizes_ancestors_and_keeps_name() {
        let mut t = dir("root", vec![dir("a", vec![leaf("x", 10)]), leaf("b", 8)]);
        t.replace(Path::new("a"), dir("ignored", vec![leaf("z", 1)])).unwrap();
        assert_eq!(t.size, 9);
        assert_eq!(t.find(Path::new("a/z")).unwrap().size, 1);
        assert!(t.replace(Path::new("missing"), leaf("m", 1)).is_none());
    }

    #[test]
    fn largest_files_across_subtree() {
        let t = dir("root", vec![dir("a", vec![leaf("x", 10), leaf("y", 1)]), leaf("b", 8), leaf("c", 3)]);
        assert_eq!(
            largest_files(&t, Path::new("pre"), 2),
            vec![(PathBuf::from("pre/a/x"), 10), (PathBuf::from("pre/b"), 8)]
        );
    }
}
