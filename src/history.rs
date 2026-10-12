use std::path::{Path, PathBuf};

/// Browser-style back/forward over the folders shown in the Disk Map.
#[derive(Debug, Default)]
pub struct History {
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    current: Option<PathBuf>,
}

impl History {
    /// Record the folder now shown. A new folder (not reached via back/forward) clears the forward list.
    pub fn visit(&mut self, path: &Path) {
        if self.current.as_deref() == Some(path) {
            return;
        }
        if let Some(prev) = self.current.replace(path.to_path_buf()) {
            self.back.push(prev);
            self.forward.clear();
        }
    }

    pub fn back(&mut self) -> Option<PathBuf> {
        let prev = self.back.pop()?;
        self.forward.extend(self.current.replace(prev.clone()));
        Some(prev)
    }

    pub fn forward(&mut self) -> Option<PathBuf> {
        let next = self.forward.pop()?;
        self.back.extend(self.current.replace(next.clone()));
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn back_and_forward_like_a_browser() {
        let mut h = History::default();
        assert_eq!(h.back(), None);
        h.visit(&p("/a"));
        h.visit(&p("/a")); // same folder again is not a new step
        h.visit(&p("/a/b"));
        h.visit(&p("/a/b/c"));
        assert_eq!(h.back(), Some(p("/a/b")));
        h.visit(&p("/a/b")); // the UI reports the folder it just navigated to
        assert_eq!(h.back(), Some(p("/a")));
        assert_eq!(h.back(), None);
        assert_eq!(h.forward(), Some(p("/a/b")));
        assert_eq!(h.forward(), Some(p("/a/b/c")));
        assert_eq!(h.forward(), None);
    }

    #[test]
    fn a_new_folder_clears_forward() {
        let mut h = History::default();
        h.visit(&p("/a"));
        h.visit(&p("/a/b"));
        assert_eq!(h.back(), Some(p("/a")));
        h.visit(&p("/a"));
        h.visit(&p("/x"));
        assert_eq!(h.forward(), None);
        assert_eq!(h.back(), Some(p("/a")));
    }
}
