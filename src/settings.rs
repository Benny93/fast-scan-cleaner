use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const MAX_RECENT: usize = 8;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Light,
    Dark,
    System,
}

/// How the treemap is coloured.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MapColors {
    /// Folders by name, files by type: tells blocks apart.
    #[default]
    Types,
    /// Traffic light: safe / check first / keep; white without advice, light green when partly removable.
    // "removable" was a separate mode before; old settings files map to this one.
    #[serde(alias = "removable")]
    Advice,
}

/// Where the treemap puts the biggest blocks.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MapOrder {
    #[default]
    TopLeft,
    /// Like GrandPerspective.
    BottomLeft,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecentScan {
    pub path: PathBuf,
    pub size: u64,
    /// Unix seconds.
    pub scanned_at: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// `None` = follow the system language.
    pub language: Option<String>,
    pub theme: Theme,
    /// Folders inside which the AI assistant may trash items without asking.
    pub allowlist: Vec<PathBuf>,
    pub mcp_enabled: bool,
    pub mcp_port: u16,
    pub recent: Vec<RecentScan>,
    pub map_colors: MapColors,
    pub map_order: MapOrder,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            language: None,
            theme: Theme::System,
            allowlist: Vec::new(),
            mcp_enabled: false,
            mcp_port: 7878,
            recent: Vec::new(),
            map_colors: MapColors::default(),
            map_order: MapOrder::default(),
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> (Settings, bool) {
        let Ok(text) = std::fs::read_to_string(path) else {
            return (Settings::default(), false);
        };
        match toml::from_str(&text) {
            Ok(s) => (s, false),
            Err(_) => {
                let _ = std::fs::rename(path, path.with_extension("toml.bak"));
                (Settings::default(), true)
            }
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        let text = toml::to_string_pretty(self).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, path)
    }

    pub fn recent_size(&self, path: &Path) -> Option<u64> {
        self.recent.iter().find(|r| r.path == path).map(|r| r.size)
    }

    pub fn push_recent(&mut self, path: PathBuf, size: u64, now: u64) {
        self.recent.retain(|r| r.path != path);
        self.recent.insert(0, RecentScan { path, size, scanned_at: now });
        self.recent.truncate(MAX_RECENT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_removable_map_mode_loads_as_traffic_light() {
        let s: Settings = toml::from_str("map_colors = \"removable\"").unwrap();
        assert_eq!(s.map_colors, MapColors::Advice);
    }

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Settings::load(&dir.path().join("settings.toml")), (Settings::default(), false));
    }

    #[test]
    fn save_then_load_roundtrips_and_creates_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/settings.toml");
        let mut s = Settings { language: Some("de".into()), ..Settings::default() };
        s.allowlist.push("/tmp/x".into());
        s.push_recent("/a".into(), 5, 1);
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), (s, false));
        assert!(!path.with_extension("toml.tmp").exists());
    }

    #[test]
    fn corrupt_file_is_backed_up_and_defaults_used() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "this is = = not toml").unwrap();
        assert_eq!(Settings::load(&path), (Settings::default(), true));
        assert_eq!(std::fs::read_to_string(dir.path().join("settings.toml.bak")).unwrap(), "this is = = not toml");
    }

    #[test]
    fn partial_file_fills_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        std::fs::write(&path, "mcp_port = 9000\n").unwrap();
        let (s, corrupt) = Settings::load(&path);
        assert!(!corrupt);
        assert_eq!(s.mcp_port, 9000);
        assert_eq!(s.theme, Theme::System);
    }

    #[cfg(unix)]
    #[test]
    fn save_returns_error_for_non_utf8_path_instead_of_panicking() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        let mut s = Settings::default();
        s.allowlist.push(PathBuf::from(OsStr::from_bytes(b"/bad\xff")));
        assert!(s.save(&path).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn recent_dedupes_newest_first_and_caps() {
        let mut s = Settings::default();
        for i in 0..10 {
            s.push_recent(format!("/p{i}").into(), i, i);
        }
        s.push_recent("/p5".into(), 99, 100);
        assert_eq!(s.recent.len(), MAX_RECENT);
        assert_eq!(s.recent[0], RecentScan { path: "/p5".into(), size: 99, scanned_at: 100 });
        assert_eq!(s.recent.iter().filter(|r| r.path == Path::new("/p5")).count(), 1);
    }

    #[test]
    fn recent_size_finds_the_folder() {
        let mut s = Settings::default();
        s.push_recent("/a".into(), 10, 1);
        s.push_recent("/b".into(), 20, 2);
        assert_eq!(s.recent_size(Path::new("/a")), Some(10));
        assert_eq!(s.recent_size(Path::new("/c")), None);
    }
}
