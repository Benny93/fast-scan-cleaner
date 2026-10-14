//! Downloaded AI models: Ollama (manifests sharing blobs) and the Hugging Face hub cache.

use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Ollama,
    HuggingFace,
    HfDataset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Model {
    pub name: String,
    pub source: Source,
    /// Bytes moving it to the Trash frees (Ollama: only blobs no other model uses).
    pub size: u64,
    /// Unix seconds.
    pub downloaded: Option<u64>,
    /// What goes to the Trash; for Ollama the manifest comes first.
    pub paths: Vec<PathBuf>,
    /// The Ollama models folder or Hugging Face hub folder it was found in.
    pub root: PathBuf,
}

fn mtime(p: &Path) -> Option<u64> {
    fs::metadata(p).ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// `registry.ollama.ai/library/qwen3/8b` → "qwen3:8b", as `ollama list` shows it.
pub fn ollama_name(rel: &Path) -> String {
    let parts: Vec<String> = rel.iter().map(|c| c.to_string_lossy().into_owned()).collect();
    match parts.as_slice() {
        [reg, ns, model, tag] if reg == "registry.ollama.ai" && ns == "library" => format!("{model}:{tag}"),
        [reg, ns, model, tag] if reg == "registry.ollama.ai" => format!("{ns}/{model}:{tag}"),
        [reg, ns, model, tag] => format!("{reg}/{ns}/{model}:{tag}"),
        _ => parts.join("/"),
    }
}

/// Every file below `root/manifests`, and whether every folder below it could be read.
fn manifests(root: &Path) -> (Vec<PathBuf>, bool) {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> bool {
        let Ok(rd) = fs::read_dir(dir) else { return false };
        let mut ok = true;
        for e in rd {
            // .DS_Store and the like: Ollama names never start with a dot.
            if e.as_ref().is_ok_and(|e| e.file_name().to_string_lossy().starts_with('.')) {
                continue;
            }
            match e.as_ref().map(|e| e.file_type()) {
                Ok(Ok(t)) if t.is_dir() => ok &= walk(&e.unwrap().path(), out),
                Ok(Ok(t)) if t.is_file() => out.push(e.unwrap().path()),
                Ok(Ok(_)) => {}
                _ => ok = false,
            }
        }
        ok
    }
    let mut out = Vec::new();
    let dir = root.join("manifests");
    // No Ollama here at all is complete; an unreadable folder is not.
    let ok = !dir.exists() || walk(&dir, &mut out);
    (out, ok)
}

/// (digest, size) of a manifest's config and layers, each digest once; None when not a manifest.
fn layers(manifest: &Path) -> Option<Vec<(String, u64)>> {
    let v: serde_json::Value = serde_json::from_slice(&fs::read(manifest).ok()?).ok()?;
    let mut out: Vec<(String, u64)> = Vec::new();
    for l in v.get("config").into_iter().chain(v.get("layers")?.as_array()?) {
        let (Some(d), Some(n)) = (l["digest"].as_str(), l["size"].as_u64()) else { continue };
        if !out.iter().any(|(x, _)| x == d) {
            out.push((d.to_string(), n));
        }
    }
    Some(out)
}

/// `sha256:abc` → `<root>/blobs/sha256-abc`.
fn blob(root: &Path, digest: &str) -> PathBuf {
    root.join("blobs").join(digest.replace(':', "-"))
}

fn ollama(root: &Path) -> Vec<Model> {
    let (files, mut complete) = manifests(root);
    let mut all: Vec<(PathBuf, Vec<(String, u64)>)> = Vec::new();
    for m in files {
        match layers(&m) {
            Some(l) => all.push((m, l)),
            None => complete = false,
        }
    }
    let mut uses: HashMap<&str, usize> = HashMap::new();
    for (_, ls) in &all {
        for (d, _) in ls {
            *uses.entry(d.as_str()).or_default() += 1;
        }
    }
    all.iter()
        .map(|(m, ls)| {
            // Blobs only this manifest uses and that exist (a partial pull may lack some). A manifest
            // that couldn't be read may use any blob, so then none count as own.
            let own: Vec<(PathBuf, u64)> = ls.iter().filter(|(d, _)| complete && uses[d.as_str()] == 1).map(|(d, n)| (blob(root, d), *n)).filter(|(p, _)| p.exists()).collect();
            let rel = m.strip_prefix(root.join("manifests")).unwrap_or(m);
            Model {
                name: ollama_name(rel),
                source: Source::Ollama,
                size: own.iter().map(|(_, n)| n).sum(),
                downloaded: mtime(m),
                paths: std::iter::once(m.clone()).chain(own.into_iter().map(|(p, _)| p)).collect(),
                root: root.to_path_buf(),
            }
        })
        .collect()
}

/// `models--org--name` → ("org/name", HuggingFace), `datasets--…` likewise; anything else: None.
pub fn hf_name(dir: &str) -> Option<(String, Source)> {
    let (rest, source) = match dir.strip_prefix("models--") {
        Some(r) => (r, Source::HuggingFace),
        None => (dir.strip_prefix("datasets--")?, Source::HfDataset),
    };
    (!rest.is_empty()).then(|| (rest.replace("--", "/"), source))
}

/// Bytes of the files below `dir`; symlinks aren't followed (snapshots link into `blobs/`).
fn dir_size(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map_or(0, |m| m.len()),
            _ => 0,
        })
        .sum()
}

fn hf(root: &Path) -> Vec<Model> {
    fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let (name, source) = hf_name(&e.file_name().to_string_lossy())?;
            let p = e.path();
            Some(Model { name, source, size: dir_size(&p), downloaded: mtime(&p), paths: vec![p], root: root.to_path_buf() })
        })
        .collect()
}

/// All models in the given Ollama and Hugging Face folders, biggest first.
/// Missing or unreadable folders and broken manifests are skipped.
pub fn list(ollama_dirs: &[PathBuf], hf_dirs: &[PathBuf]) -> Vec<Model> {
    let mut all: Vec<Model> = ollama_dirs.iter().flat_map(|d| ollama(d)).chain(hf_dirs.iter().flat_map(|d| hf(d))).collect();
    all.sort_by(|a, b| b.size.cmp(&a.size).then_with(|| a.name.cmp(&b.name)));
    all
}

/// What to request when trashing `m`. Ollama: re-read from disk, so a blob that a model
/// pulled after the listing also uses stays; a manifest gone since → nothing.
pub fn trash_paths(m: &Model) -> Vec<PathBuf> {
    match m.source {
        Source::Ollama => ollama(&m.root).into_iter().find(|x| x.paths.first() == m.paths.first()).map_or_else(Vec::new, |x| x.paths),
        _ => m.paths.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A manifest at `manifests/<rel>`: the first layer is the config; blobs are sparse files.
    fn manifest(root: &Path, rel: &str, layers: &[(&str, u64)]) -> PathBuf {
        let p = root.join("manifests").join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let ls: Vec<_> = layers.iter().map(|(d, n)| serde_json::json!({ "digest": d, "size": n })).collect();
        fs::write(&p, serde_json::json!({ "config": ls[0], "layers": ls[1..] }).to_string()).unwrap();
        for (d, n) in layers {
            let b = blob(root, d);
            fs::create_dir_all(b.parent().unwrap()).unwrap();
            fs::File::create(&b).unwrap().set_len(*n).unwrap();
        }
        p
    }

    #[test]
    fn ollama_names_match_ollama_list() {
        assert_eq!(ollama_name(Path::new("registry.ollama.ai/library/qwen3/8b")), "qwen3:8b");
        assert_eq!(ollama_name(Path::new("registry.ollama.ai/dengcao/Qwen3-30B/Q5_K_M")), "dengcao/Qwen3-30B:Q5_K_M");
        assert_eq!(ollama_name(Path::new("hf.co/org/model/latest")), "hf.co/org/model:latest");
    }

    #[test]
    fn shared_blobs_count_for_neither_model() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let a = manifest(root, "registry.ollama.ai/library/a/1", &[("sha256:ca", 10), ("sha256:shared", 1000), ("sha256:own", 500)]);
        manifest(root, "registry.ollama.ai/library/b/1", &[("sha256:cb", 20), ("sha256:shared", 1000)]);
        let got = list(&[root.to_path_buf()], &[]);
        assert_eq!(got.iter().map(|m| (m.name.as_str(), m.size)).collect::<Vec<_>>(), vec![("a:1", 510), ("b:1", 20)]);
        assert_eq!(got[0].paths, vec![a, blob(root, "sha256:ca"), blob(root, "sha256:own")]);
        assert_eq!(got[0].source, Source::Ollama);
    }

    #[test]
    fn a_fully_shared_model_frees_nothing_and_sorts_last() {
        let tmp = tempfile::tempdir().unwrap();
        let (ol, hub) = (tmp.path().join("ollama"), tmp.path().join("hub"));
        manifest(&ol, "registry.ollama.ai/library/a/1", &[("sha256:ca", 10), ("sha256:s", 100)]);
        let b = manifest(&ol, "registry.ollama.ai/library/b/1", &[("sha256:s", 100)]);
        fs::create_dir_all(hub.join("models--o--m")).unwrap();
        fs::write(hub.join("models--o--m/w"), [0u8; 5]).unwrap();
        let got = list(&[ol], &[hub]);
        assert_eq!(got.iter().map(|m| (m.name.as_str(), m.size)).collect::<Vec<_>>(), vec![("a:1", 10), ("o/m", 5), ("b:1", 0)]);
        assert_eq!(got[2].paths, vec![b], "only the manifest goes");
    }

    #[test]
    fn trash_paths_recheck_blobs_shared_since_the_listing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        manifest(root, "registry.ollama.ai/library/a/1", &[("sha256:ca", 10), ("sha256:own", 500)]);
        let a = list(&[root.to_path_buf()], &[]).remove(0);
        assert!(a.paths.contains(&blob(root, "sha256:own")));
        manifest(root, "registry.ollama.ai/library/c/1", &[("sha256:cc", 1), ("sha256:own", 500)]);
        assert_eq!(trash_paths(&a), vec![a.paths[0].clone(), blob(root, "sha256:ca")]);
        fs::remove_file(&a.paths[0]).unwrap();
        assert!(trash_paths(&a).is_empty(), "a model already gone has nothing to trash");
    }

    #[test]
    fn missing_blobs_and_missing_folders_are_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        manifest(root, "registry.ollama.ai/library/a/1", &[("sha256:ca", 10), ("sha256:gone", 700)]);
        fs::remove_file(blob(root, "sha256:gone")).unwrap();
        let got = list(&[root.to_path_buf(), root.join("nope")], &[root.join("nope")]);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].size, got[0].paths.len()), (10, 2));
    }

    #[test]
    fn hidden_files_among_manifests_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        manifest(root, "registry.ollama.ai/library/a/1", &[("sha256:ca", 10)]);
        fs::write(root.join("manifests/.DS_Store"), "x").unwrap();
        assert_eq!(list(&[root.to_path_buf()], &[])[0].size, 10);
    }

    #[test]
    fn an_unreadable_manifest_keeps_every_blob() {
        // It may use any blob (e.g. a pull writing it right now), so no model's blobs go.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let a = manifest(root, "registry.ollama.ai/library/a/1", &[("sha256:ca", 10), ("sha256:own", 500)]);
        fs::write(root.join("manifests/registry.ollama.ai/library/a/broken"), "not json").unwrap();
        let got = list(&[root.to_path_buf()], &[]);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].size, got[0].paths.clone()), (0, vec![a.clone()]));
        assert_eq!(trash_paths(&got[0]), vec![a]);
    }

    #[test]
    fn hugging_face_folder_names() {
        assert_eq!(hf_name("models--org--name"), Some(("org/name".into(), Source::HuggingFace)));
        assert_eq!(hf_name("datasets--org--d"), Some(("org/d".into(), Source::HfDataset)));
        assert_eq!(hf_name("models--gpt2"), Some(("gpt2".into(), Source::HuggingFace)));
        assert_eq!(hf_name("spaces--org--x"), None);
        assert_eq!(hf_name("version.txt"), None);
        assert_eq!(hf_name("models--"), None);
    }

    #[cfg(unix)]
    #[test]
    fn hugging_face_files_count_once_despite_snapshot_links() {
        let tmp = tempfile::tempdir().unwrap();
        let m = tmp.path().join("models--o--m");
        fs::create_dir_all(m.join("blobs")).unwrap();
        fs::create_dir_all(m.join("snapshots/abc")).unwrap();
        fs::write(m.join("blobs/x"), [0u8; 100]).unwrap();
        std::os::unix::fs::symlink("../../blobs/x", m.join("snapshots/abc/model.safetensors")).unwrap();
        fs::create_dir_all(tmp.path().join("spaces--o--s")).unwrap();
        let got = list(&[], &[tmp.path().to_path_buf()]);
        assert_eq!(got.len(), 1);
        assert_eq!((got[0].size, got[0].paths.clone(), got[0].source), (100, vec![m], Source::HuggingFace));
    }
}
