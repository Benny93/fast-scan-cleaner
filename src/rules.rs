use crate::scan::{Kind, Node};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Safety {
    Safe,
    Review,
    Keep,
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub id: String,
    pub safety: Safety,
    /// Locale key of the explanation.
    pub message: String,
}

pub struct Rules {
    rules: Vec<Rule>,
    set: GlobSet,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Finding {
    pub path: PathBuf,
    pub size: u64,
    pub rule_id: String,
    pub safety: Safety,
    pub message: String,
}

/// Findings with the given safety level and their total size.
pub fn by_safety(findings: &[Finding], safety: Safety) -> (Vec<&Finding>, u64) {
    let items: Vec<&Finding> = findings.iter().filter(|f| f.safety == safety).collect();
    let total = items.iter().map(|f| f.size).sum();
    (items, total)
}

#[derive(Deserialize)]
struct Catalog {
    rule: Vec<RawRule>,
}

#[derive(Deserialize)]
struct RawRule {
    id: String,
    os: Option<String>,
    glob: String,
    safety: Safety,
    message: String,
}

impl Rules {
    pub fn builtin() -> Rules {
        Rules::parse(include_str!("../rules/catalog.toml"), &crate::platform::home(), std::env::consts::OS)
            .expect("built-in catalog is valid")
    }

    pub fn parse(src: &str, home: &Path, os: &str) -> anyhow::Result<Rules> {
        let catalog: Catalog = toml::from_str(src)?;
        let home = globset::escape(&normalize(home));
        let case_insensitive = os == "windows" || os == "macos";
        let mut rules = Vec::new();
        let mut set = GlobSetBuilder::new();
        for r in catalog.rule.into_iter().filter(|r| r.os.as_deref().is_none_or(|o| o == os)) {
            let pattern = match r.glob.strip_prefix('~') {
                Some(rest) => format!("{home}{rest}"),
                None => r.glob.clone(),
            };
            set.add(GlobBuilder::new(&pattern).literal_separator(true).case_insensitive(case_insensitive).build()?);
            rules.push(Rule { id: r.id, safety: r.safety, message: r.message });
        }
        Ok(Rules { rules, set: set.build()? })
    }

    pub fn matching(&self, path: &Path) -> Option<&Rule> {
        self.set.matches(normalize(path)).into_iter().min().map(|i| &self.rules[i])
    }
}

fn normalize(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

pub fn findings(tree: &Node, root: &Path, rules: &Rules) -> Vec<Finding> {
    fn walk(node: &Node, path: &Path, rules: &Rules, out: &mut Vec<Finding>) {
        if let Some(r) = rules.matching(path) {
            if node.size > 0 {
                out.push(Finding {
                    path: path.to_path_buf(),
                    size: node.size,
                    rule_id: r.id.clone(),
                    safety: r.safety,
                    message: r.message.clone(),
                });
            }
            // Review-level folders (Downloads) still contain separately classified items.
            if r.safety != Safety::Review {
                return;
            }
        }
        for c in node.children.iter().filter(|c| c.kind != Kind::Symlink) {
            walk(c, &path.join(&*c.name), rules, out);
        }
    }
    let mut out = Vec::new();
    walk(tree, root, rules, &mut out);
    out.sort_by_key(|c| std::cmp::Reverse(c.size));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linux() -> Rules {
        Rules::parse(include_str!("../rules/catalog.toml"), Path::new("/home/u"), "linux").unwrap()
    }

    fn id(rules: &Rules, p: &str) -> Option<String> {
        rules.matching(Path::new(p)).map(|r| r.id.clone())
    }

    #[test]
    fn matches_most_specific_first_and_filters_os() {
        let r = linux();
        assert_eq!(id(&r, "/home/u/.cache/pip").as_deref(), Some("linux_cache"));
        assert_eq!(id(&r, "/home/u/.cache/pip/deep").as_deref(), None);
        assert_eq!(id(&r, "/home/u/code/app/node_modules").as_deref(), Some("node_modules"));
        assert_eq!(id(&r, "/home/u/code/app/node_modules/x").as_deref(), None);
        assert_eq!(id(&r, "/home/u/Downloads/setup.deb").as_deref(), Some("installer"));
        assert_eq!(id(&r, "/home/u/Downloads").as_deref(), Some("downloads"));
        assert_eq!(id(&r, "/home/u/Library/Caches/x").as_deref(), None); // macOS-only rule
        assert_eq!(id(&r, "/home/u/Documents").as_deref(), None);
    }

    #[test]
    fn os_specific_cache_rules() {
        let src = include_str!("../rules/catalog.toml");
        let mac = Rules::parse(src, Path::new("/Users/u"), "macos").unwrap();
        assert_eq!(id(&mac, "/Users/u/Library/Caches/Homebrew").as_deref(), Some("homebrew_cache")); // before generic caches
        assert_eq!(id(&mac, "/Users/u/Library/Caches/com.example").as_deref(), Some("caches"));
        assert_eq!(id(&mac, "/Users/u/Library/Logs/DiagnosticReports").as_deref(), Some("logs"));
        assert_eq!(id(&mac, "/Users/u/Library/Developer/Xcode/iOS DeviceSupport/17.0 (21A329)").as_deref(), Some("ios_device_support"));
        assert_eq!(id(&mac, "/Users/u/Library/Developer/Xcode/Archives").as_deref(), Some("xcode_archives"));
        assert_eq!(id(&mac, "/Users/u/Library/Developer/CoreSimulator/Caches").as_deref(), Some("simulator_caches"));
        assert_eq!(id(&mac, "/Users/u/Library/iTunes/iPhone Software Updates/x.ipsw").as_deref(), Some("ios_updates"));
        assert_eq!(id(&mac, "/Users/u/.cargo/registry").as_deref(), Some("cargo_registry"));

        let win = Rules::parse(src, Path::new("C:\\Users\\U"), "windows").unwrap();
        assert_eq!(id(&win, "C:\\Users\\U\\AppData\\Local\\CrashDumps").as_deref(), Some("crash_dumps"));
        assert_eq!(id(&win, "C:\\Users\\U\\AppData\\Local\\Google\\Chrome\\User Data\\Default\\Cache").as_deref(), Some("win_app_cache"));
        assert_eq!(id(&win, "C:\\Users\\U\\AppData\\Local\\Temp\\x").as_deref(), Some("win_temp"));

        let linux = Rules::parse(src, Path::new("/home/u"), "linux").unwrap();
        assert_eq!(id(&linux, "/home/u/snap/firefox/common/.cache").as_deref(), Some("snap_cache"));
        assert_eq!(id(&linux, "/home/u/.m2/repository").as_deref(), Some("maven_repo"));
        assert_eq!(id(&linux, "/home/u/Library/Logs/x").as_deref(), None); // macOS-only rule
    }

    #[test]
    fn windows_paths_and_case() {
        let r = Rules::parse(include_str!("../rules/catalog.toml"), Path::new("C:\\Users\\U"), "windows").unwrap();
        assert_eq!(id(&r, "C:\\Users\\U\\AppData\\Local\\Temp\\abc").as_deref(), Some("win_temp"));
        assert_eq!(id(&r, "c:\\users\\u\\appdata\\local\\temp\\abc").as_deref(), Some("win_temp"));
    }

    #[test]
    fn glob_characters_in_home_are_literal() {
        let r = Rules::parse(include_str!("../rules/catalog.toml"), Path::new("/home/[x]"), "linux").unwrap();
        assert_eq!(id(&r, "/home/[x]/.cache/a").as_deref(), Some("linux_cache"));
        assert_eq!(id(&r, "/home/x/.cache/a").as_deref(), None);
    }

    #[test]
    fn builtin_catalog_parses_and_messages_exist() {
        let src = include_str!("../rules/catalog.toml");
        let en = include_str!("../locales/en.yml");
        for os in ["macos", "linux", "windows"] {
            let r = Rules::parse(src, Path::new("/h"), os).unwrap();
            for rule in &r.rules {
                assert!(en.contains(&format!("\n{}:", rule.message)), "missing locale key {}", rule.message);
            }
        }
    }

    #[test]
    fn findings_stop_at_matches_and_sort_by_size() {
        let leaf = |n: &str, s| Node { name: n.into(), size: s, kind: Kind::File, children: Box::default() };
        let dir = |n: &str, c: Vec<Node>| Node { name: n.into(), size: c.iter().map(|x| x.size).sum(), kind: Kind::Dir, children: c.into() };
        let tree = dir(
            "/home/u",
            vec![
                dir("code", vec![dir("node_modules", vec![dir("node_modules", vec![leaf("x", 5)]), leaf("y", 1)])]),
                dir(".cache", vec![dir("pip", vec![leaf("w", 50)]), dir("empty", vec![])]),
            ],
        );
        let f = findings(&tree, Path::new("/home/u"), &linux());
        let got: Vec<_> = f.iter().map(|f| (f.path.clone(), f.size)).collect();
        assert_eq!(got, vec![(PathBuf::from("/home/u/.cache/pip"), 50), (PathBuf::from("/home/u/code/node_modules"), 6)]);
    }

    #[test]
    fn review_matches_do_not_stop_the_walk() {
        let leaf = |n: &str, s| Node { name: n.into(), size: s, kind: Kind::File, children: Box::default() };
        let dl = Node { name: "Downloads".into(), size: 15, kind: Kind::Dir, children: vec![leaf("x.dmg", 5), leaf("notes.txt", 10)].into() };
        let u = Node { name: "/home/u".into(), size: 15, kind: Kind::Dir, children: vec![dl].into() };
        let f = findings(&u, Path::new("/home/u"), &linux());
        let ids: Vec<_> = f.iter().map(|f| (f.rule_id.as_str(), f.size)).collect();
        assert!(ids.contains(&("downloads", 15)), "{ids:?}");
        assert!(ids.contains(&("installer", 5)), "{ids:?}");
    }

    #[test]
    fn by_safety_filters_and_sums() {
        let f = |id: &str, size, safety| Finding { path: PathBuf::from(id), size, rule_id: id.into(), safety, message: String::new() };
        let all = vec![f("a", 5, Safety::Safe), f("b", 7, Safety::Review), f("c", 3, Safety::Safe)];
        let (safe, total) = by_safety(&all, Safety::Safe);
        assert_eq!(safe.iter().map(|x| x.rule_id.as_str()).collect::<Vec<_>>(), ["a", "c"]);
        assert_eq!(total, 8);
        assert_eq!(by_safety(&all, Safety::Keep), (vec![], 0));
    }
}
