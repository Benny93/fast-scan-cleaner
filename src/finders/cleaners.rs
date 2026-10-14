use crate::scan::{Kind, Node};
use serde::Serialize;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const CHECK_TIMEOUT: Duration = Duration::from_secs(20);
pub const RUN_TIMEOUT: Duration = Duration::from_secs(600);
/// Pseudo-program handled by the real runner with the `trash` crate (Windows/Linux).
pub const PURGE_TRASH: &str = "@purge_trash";
/// `Unavailable` reason for a missing tool (the UI translates it).
pub const NOT_INSTALLED: &str = "not_installed";
/// Error for emptying the Trash when macOS denied controlling Finder (the UI translates it).
pub const AUTOMATION_DENIED: &str = "automation_denied";

#[derive(Debug, Clone, PartialEq)]
pub struct Cmd {
    pub program: &'static str,
    pub args: Vec<String>,
    pub timeout: Duration,
}

impl Cmd {
    pub fn new(program: &'static str, args: &[&str], timeout: Duration) -> Cmd {
        Cmd { program, args: args.iter().map(|a| a.to_string()).collect(), timeout }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum CmdOutput {
    NotFound,
    TimedOut,
    Done { ok: bool, stdout: String, stderr: String },
}

pub type Runner = Arc<dyn Fn(&Cmd) -> CmdOutput + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Cleaner {
    EmptyTrash,
    Homebrew,
    Docker,
    TmSnapshots,
    Simulators,
    GitBranches,
    GitWorktrees,
}

impl Cleaner {
    /// Everything except the per-repository Git cleaners.
    pub const TOOLS: [Cleaner; 5] = [Cleaner::EmptyTrash, Cleaner::Homebrew, Cleaner::Docker, Cleaner::TmSnapshots, Cleaner::Simulators];

    pub fn applies(self, os: &str) -> bool {
        match self {
            Cleaner::TmSnapshots | Cleaner::Simulators => os == "macos",
            Cleaner::Homebrew => os != "windows",
            _ => true,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Cleaner::EmptyTrash => "empty_trash",
            Cleaner::Homebrew => "homebrew",
            Cleaner::Docker => "docker",
            Cleaner::TmSnapshots => "tm_snapshots",
            Cleaner::Simulators => "simulators",
            Cleaner::GitBranches => "git-branches",
            Cleaner::GitWorktrees => "git-worktrees",
        }
    }

    pub fn name_key(self) -> &'static str {
        match self {
            Cleaner::EmptyTrash => "sys_trash_name",
            Cleaner::Homebrew => "sys_brew_name",
            Cleaner::Docker => "sys_docker_name",
            Cleaner::TmSnapshots => "sys_tm_name",
            Cleaner::Simulators => "sys_sim_name",
            Cleaner::GitBranches => "sys_git_branches",
            Cleaner::GitWorktrees => "sys_git_worktrees",
        }
    }

    pub fn advice_key(self) -> &'static str {
        match self {
            Cleaner::EmptyTrash => "sys_trash_advice",
            Cleaner::Homebrew => "sys_brew_advice",
            Cleaner::Docker => "sys_docker_advice",
            Cleaner::TmSnapshots => "sys_tm_advice",
            Cleaner::Simulators => "sys_sim_advice",
            Cleaner::GitBranches => "sys_git_branches_advice",
            Cleaner::GitWorktrees => "sys_git_worktrees_advice",
        }
    }
}

pub fn item_id(c: Cleaner, repo: Option<&Path>) -> String {
    match repo {
        Some(r) => format!("{}:{}", c.id(), r.display()),
        None => c.id().to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Estimate {
    Bytes(u64),
    Count(u64),
    Unknown,
}

impl Estimate {
    pub fn is_nothing(self) -> bool {
        matches!(self, Estimate::Bytes(0) | Estimate::Count(0))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SystemStatus {
    Checking,
    Ready { estimate: Estimate, detail: Vec<String> },
    Unavailable { reason: String },
    Running,
    Failed { error: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Worktree {
    pub path: PathBuf,
    /// Short branch name; `None` when detached or bare.
    pub branch: Option<String>,
    pub prunable: bool,
    pub locked: bool,
}

/// Non-empty trimmed lines.
pub fn parse_lines(stdout: &str) -> Vec<String> {
    stdout.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
}

pub fn find_tool(name: &str) -> Option<PathBuf> {
    let exe = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).filter(|d| d.is_absolute()).collect()).unwrap_or_default();
    // GUI apps started from Finder get a minimal PATH.
    dirs.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    dirs.extend(dirs::home_dir().map(|h| h.join(".docker/bin")));
    dirs.into_iter().map(|d| d.join(&exe)).find(|p| p.is_file())
}

pub fn real_runner() -> Runner {
    Arc::new(run_command)
}


/// "1.2GB", "512 MB", "0B", "3.5 KiB" → bytes. `base` 1000 (Docker) or 1024 (Homebrew).
pub fn parse_size(s: &str, base: f64) -> Option<u64> {
    let s = s.trim();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: f64 = num.parse().ok()?;
    let power = match unit.trim().to_ascii_lowercase().trim_end_matches("ib").trim_end_matches('b') {
        "" => 0,
        "k" => 1,
        "m" => 2,
        "g" => 3,
        "t" => 4,
        _ => return None,
    };
    Some((n * base.powi(power)).round() as u64)
}

/// `brew cleanup --dry-run`: "...would free approximately 1.2GB of disk space." (0 if nothing).
pub fn parse_brew_dry_run(stdout: &str) -> u64 {
    stdout
        .lines()
        .find_map(|l| l.split("would free approximately ").nth(1))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|size| parse_size(size, 1024.0))
        .unwrap_or(0)
}

/// `docker system df --format '{{json .}}'`: one JSON object per line; sums "Reclaimable" of
/// containers and build cache only (`system prune` keeps volumes and non-dangling images).
pub fn parse_docker_df(stdout: &str) -> u64 {
    stdout
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|v| matches!(v["Type"].as_str(), Some("Containers" | "Build Cache")))
        .filter_map(|v| v["Reclaimable"].as_str().map(String::from))
        .filter_map(|r| r.split_whitespace().next().and_then(|s| parse_size(s, 1000.0)))
        .sum()
}

/// `tmutil listlocalsnapshots /`: counts snapshot lines.
pub fn parse_tmutil(stdout: &str) -> u64 {
    stdout.lines().filter(|l| l.trim_start().starts_with("com.apple.TimeMachine.")).count() as u64
}

/// `xcrun simctl list devices unavailable -j`: counts devices.
pub fn parse_simctl(stdout: &str) -> Option<u64> {
    let v: serde_json::Value = serde_json::from_str(stdout).ok()?;
    Some(v["devices"].as_object()?.values().filter_map(|d| d.as_array()).map(|a| a.len() as u64).sum())
}

/// `git worktree list --porcelain`. The first entry is the main worktree.
pub fn parse_worktrees(stdout: &str) -> Vec<Worktree> {
    stdout
        .split("\n\n")
        .filter_map(|block| {
            let mut wt = Worktree { path: PathBuf::new(), branch: None, prunable: false, locked: false };
            for line in block.lines() {
                if let Some(p) = line.strip_prefix("worktree ") {
                    wt.path = PathBuf::from(p);
                } else if let Some(b) = line.strip_prefix("branch ") {
                    wt.branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
                } else if line.starts_with("prunable") {
                    wt.prunable = true;
                } else if line == "locked" || line.starts_with("locked ") {
                    wt.locked = true;
                }
            }
            (!wt.path.as_os_str().is_empty()).then_some(wt)
        })
        .collect()
}

/// Repositories (folders with a `.git` directory) in the scanned tree, at most `limit`.
/// At most this many repositories of a scan are checked for merged branches and worktrees.
pub const GIT_REPO_LIMIT: usize = 200;

/// Breadth first, so the repositories closest to the scanned folder come first (not the ones
/// inside the biggest folders) when there are more than `limit`.
pub fn git_repos(tree: &Node, root: &Path, limit: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut queue = std::collections::VecDeque::from([(tree, root.to_path_buf())]);
    while let Some((node, path)) = queue.pop_front() {
        if out.len() >= limit {
            break;
        }
        if node.children.iter().any(|c| c.kind == Kind::Dir && &*c.name == ".git") {
            out.push(path.clone());
        }
        for c in node.children.iter().filter(|c| c.kind == Kind::Dir && &*c.name != ".git" && &*c.name != "node_modules") {
            queue.push_back((c, path.join(&*c.name)));
        }
    }
    out
}

fn run_command(cmd: &Cmd) -> CmdOutput {
    if cmd.program == PURGE_TRASH {
        return purge_trash();
    }
    let Some(program) = find_tool(cmd.program) else { return CmdOutput::NotFound };
    let mut command = std::process::Command::new(program);
    command.args(&cmd.args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    // Own process group so a timeout can kill grandchildren too (brew -> ruby, xcrun -> simctl).
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(_) => return CmdOutput::NotFound,
    };
    // Drain the pipes on threads so a chatty tool can't block on a full pipe.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            String::from_utf8_lossy(&buf).into_owned()
        })
    };
    let out = drain(child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let err = drain(child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>));
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if start.elapsed() < cmd.timeout => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                #[cfg(unix)]
                // SAFETY: plain syscall on the process group we created above.
                unsafe {
                    libc::killpg(child.id() as i32, libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                // Don't join the drain threads: a surviving grandchild may hold the pipes open.
                return CmdOutput::TimedOut;
            }
        }
    };
    let (stdout, stderr) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    match status {
        Some(s) => CmdOutput::Done { ok: s.success(), stdout, stderr },
        None => CmdOutput::TimedOut,
    }
}

#[cfg(any(windows, all(unix, not(target_os = "macos"))))]
fn purge_trash() -> CmdOutput {
    match trash::os_limited::list().and_then(trash::os_limited::purge_all) {
        Ok(()) => CmdOutput::Done { ok: true, stdout: String::new(), stderr: String::new() },
        Err(e) => CmdOutput::Done { ok: false, stdout: String::new(), stderr: e.to_string() },
    }
}

#[cfg(target_os = "macos")]
fn purge_trash() -> CmdOutput {
    CmdOutput::Done { ok: false, stdout: String::new(), stderr: "not supported on macOS".into() }
}

pub struct Env<'a> {
    pub run: &'a Runner,
    /// The user's Trash folder (macOS/Linux); `None` → count via the OS API (Windows).
    pub trash_dir: Option<&'a Path>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GitFindings {
    pub main: String,
    pub branches: Vec<String>,
    pub worktrees: Vec<PathBuf>,
}

fn first_line(s: &str) -> String {
    s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("failed").to_string()
}

/// Run a check command; `Err` holds the `Unavailable` reason.
fn checked(env: &Env, cmd: Cmd) -> Result<String, String> {
    match (env.run)(&cmd) {
        CmdOutput::NotFound => Err(NOT_INSTALLED.into()),
        CmdOutput::TimedOut => Err("timed out".into()),
        CmdOutput::Done { ok: true, stdout, .. } => Ok(stdout),
        CmdOutput::Done { ok: false, stderr, stdout } => Err(first_line(if stderr.trim().is_empty() { &stdout } else { &stderr })),
    }
}

fn ready(estimate: Estimate) -> SystemStatus {
    SystemStatus::Ready { estimate, detail: Vec::new() }
}

pub fn check_tool(c: Cleaner, env: &Env) -> SystemStatus {
    let result = match c {
        Cleaner::EmptyTrash => check_trash(env),
        Cleaner::Homebrew => checked(env, Cmd::new("brew", &["cleanup", "--dry-run"], CHECK_TIMEOUT)).map(|o| Estimate::Bytes(parse_brew_dry_run(&o))),
        Cleaner::Docker => checked(env, Cmd::new("docker", &["system", "df", "--format", "{{json .}}"], CHECK_TIMEOUT)).map(|o| Estimate::Bytes(parse_docker_df(&o))),
        Cleaner::TmSnapshots => checked(env, Cmd::new("tmutil", &["listlocalsnapshots", "/"], CHECK_TIMEOUT)).map(|o| Estimate::Count(parse_tmutil(&o))),
        Cleaner::Simulators if !dev_tools_ok(env) => Err(NOT_INSTALLED.into()),
        Cleaner::Simulators => checked(env, Cmd::new("xcrun", &["simctl", "list", "devices", "unavailable", "-j"], CHECK_TIMEOUT))
            .and_then(|o| parse_simctl(&o).map(Estimate::Count).ok_or_else(|| "unexpected output".to_string())),
        Cleaner::GitBranches | Cleaner::GitWorktrees => Err("use check_git".into()),
    };
    match result {
        Ok(e) => ready(e),
        Err(reason) => SystemStatus::Unavailable { reason },
    }
}

/// Without Full Disk Access the Trash may be unreadable: then the size is unknown (still runnable).
fn check_trash(env: &Env) -> Result<Estimate, String> {
    match env.trash_dir {
        Some(dir) if !dir.exists() => Ok(Estimate::Bytes(0)),
        Some(dir) => {
            let progress = crate::scan::Progress::default();
            match crate::scan::scan(dir, &progress) {
                Ok(n) if progress.denied.load(std::sync::atomic::Ordering::Relaxed) == 0 => Ok(Estimate::Bytes(n.size)),
                _ => Ok(Estimate::Unknown),
            }
        }
        None => trash_count(),
    }
}

/// On macOS, `xcrun` and `git` are stubs that pop up an install dialog when the Command Line
/// Tools are missing; `xcode-select -p` tells without that side effect.
fn dev_tools_ok(env: &Env) -> bool {
    !cfg!(target_os = "macos") || checked(env, Cmd::new("xcode-select", &["-p"], CHECK_TIMEOUT)).is_ok()
}

#[cfg(any(windows, all(unix, not(target_os = "macos"))))]
fn trash_count() -> Result<Estimate, String> {
    trash::os_limited::list().map(|l| Estimate::Count(l.len() as u64)).map_err(|e| e.to_string())
}

#[cfg(target_os = "macos")]
fn trash_count() -> Result<Estimate, String> {
    Ok(Estimate::Unknown)
}

fn git(repo: &Path, args: &[&str], timeout: Duration) -> Cmd {
    // No fsmonitor: a repo's config must not start a daemon or hook for our status checks.
    let mut all = vec!["-c".to_string(), "core.fsmonitor=false".into(), "-C".into(), repo.display().to_string()];
    all.extend(args.iter().map(|a| a.to_string()));
    Cmd { program: "git", args: all, timeout }
}

const REGENERABLE: &[&str] = &[
    "target", "node_modules", "build", "dist", "out", ".venv", "venv", "__pycache__", ".gradle", ".next", ".nuxt", "coverage", ".dart_tool", "Pods", "DerivedData", ".turbo",
    ".parcel-cache", ".pytest_cache", ".mypy_cache", ".tox",
];

/// `--untracked-files=normal` collapses a wholly ignored folder to one "!! dir/" line instead of
/// listing its files, so an ignored file elsewhere (even inside a tracked `build/`) shows up.
const STATUS_ARGS: &[&str] = &["status", "--porcelain", "--ignored", "--untracked-files=normal", "--ignore-submodules=none"];

/// A worktree may be removed only if `git status` (STATUS_ARGS) shows nothing except ignored
/// folders ("!! <path>/") whose own name is a regenerable build-output directory.
pub fn worktree_is_disposable(status_output: &str) -> bool {
    status_output.lines().filter(|l| !l.trim().is_empty()).all(|l| {
        l.strip_prefix("!! ")
            .and_then(|p| p.strip_suffix('/'))
            .and_then(|p| p.rsplit('/').next())
            .is_some_and(|name| REGENERABLE.contains(&name))
    })
}

/// `worktree_is_disposable`, plus: each "!! <dir>/" must be ignored itself. `git status` also
/// collapses a non-ignored folder that holds only ignored files (`out/prod.key` with `*.key`).
fn worktree_disposable_now(path: &Path, env: &Env) -> bool {
    let Ok(status) = checked(env, git(path, STATUS_ARGS, CHECK_TIMEOUT)) else { return false };
    worktree_is_disposable(&status)
        && status.lines().filter(|l| !l.trim().is_empty()).all(|l| {
            l.strip_prefix("!! ")
                .filter(|dir| !dir.starts_with('"')) // quoted by git (special characters): keep
                .is_some_and(|dir| checked(env, git(path, &["check-ignore", "-q", "--", dir], CHECK_TIMEOUT)).is_ok())
        })
}

pub fn check_git(repo: &Path, env: &Env) -> Result<GitFindings, String> {
    if !dev_tools_ok(env) {
        return Err(NOT_INSTALLED.into());
    }
    // origin/HEAD can name a branch that does not exist locally; then try main and master.
    let remote_head = checked(env, git(repo, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"], CHECK_TIMEOUT))
        .ok()
        .map(|out| out.trim().trim_start_matches("origin/").to_string());
    let main = remote_head
        .into_iter()
        .chain(["main".to_string(), "master".to_string()])
        .find(|b| checked(env, git(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{b}")], CHECK_TIMEOUT)).is_ok())
        .ok_or("no main branch")?;
    let merged = parse_lines(&checked(env, git(repo, &["for-each-ref", &format!("--merged=refs/heads/{main}"), "--format=%(refname:lstrip=2)", "refs/heads/"], CHECK_TIMEOUT))?);
    let worktrees = parse_worktrees(&checked(env, git(repo, &["worktree", "list", "--porcelain"], CHECK_TIMEOUT))?);
    let checked_out: Vec<&str> = worktrees.iter().filter_map(|w| w.branch.as_deref()).collect();
    let branches = merged.iter().filter(|b| **b != main && !checked_out.contains(&b.as_str()) && !b.starts_with('-')).cloned().collect();
    let worktrees = worktrees
        .iter()
        .skip(1) // the main worktree
        .filter(|w| !w.prunable && !w.locked && w.branch.as_ref().is_some_and(|b| merged.contains(b)))
        .filter(|w| worktree_disposable_now(&w.path, env))
        .map(|w| w.path.clone())
        .collect();
    Ok(GitFindings { main, branches, worktrees })
}

/// Run a cleaner. `detail` is what the user confirmed (branch names / worktree paths).
pub fn run(c: Cleaner, repo: Option<&Path>, detail: &[String], env: &Env) -> Result<(), String> {
    let exec = |cmd: Cmd| match (env.run)(&cmd) {
        CmdOutput::Done { ok: true, .. } => Ok(()),
        CmdOutput::Done { stderr, stdout, .. } => Err(first_line(if stderr.trim().is_empty() { &stdout } else { &stderr })),
        CmdOutput::NotFound => Err(NOT_INSTALLED.to_string()),
        CmdOutput::TimedOut => Err("timed out".to_string()),
    };
    match c {
        Cleaner::EmptyTrash if cfg!(target_os = "macos") => {
            let (stdout, stderr) = match (env.run)(&Cmd::new("osascript", &["-e", "tell application \"Finder\" to empty trash"], RUN_TIMEOUT)) {
                CmdOutput::Done { ok: true, .. } => return Ok(()),
                CmdOutput::Done { stdout, stderr, .. } => (stdout, stderr),
                CmdOutput::NotFound => return Err(NOT_INSTALLED.to_string()),
                CmdOutput::TimedOut => return Err("timed out".to_string()),
            };
            // -1743: the user has not allowed us to control Finder (Automation privacy setting).
            if stderr.contains("-1743") || stdout.contains("-1743") {
                Err(AUTOMATION_DENIED.to_string())
            } else {
                Err(first_line(if stderr.trim().is_empty() { &stdout } else { &stderr }))
            }
        }
        Cleaner::EmptyTrash => exec(Cmd::new(PURGE_TRASH, &[], RUN_TIMEOUT)),
        Cleaner::Homebrew => exec(Cmd::new("brew", &["cleanup"], RUN_TIMEOUT)),
        Cleaner::Docker => exec(Cmd::new("docker", &["system", "prune", "-f"], RUN_TIMEOUT)),
        Cleaner::TmSnapshots => exec(Cmd::new("tmutil", &["thinlocalsnapshots", "/", "999999999999", "4"], RUN_TIMEOUT)),
        Cleaner::Simulators => exec(Cmd::new("xcrun", &["simctl", "delete", "unavailable"], RUN_TIMEOUT)),
        Cleaner::GitBranches => {
            let repo = repo.ok_or("no repository")?;
            let names: Vec<&str> = detail.iter().map(String::as_str).filter(|b| !b.starts_with('-')).collect();
            if names.is_empty() {
                return Ok(());
            }
            let mut args = vec!["branch", "-d"];
            args.extend(names);
            exec(git(repo, &args, RUN_TIMEOUT))
        }
        Cleaner::GitWorktrees => {
            let repo = repo.ok_or("no repository")?;
            // Re-check right before removing: the worktree may have changed since the user saw it.
            let errors: Vec<String> = detail
                .iter()
                .filter_map(|p| {
                    if !worktree_disposable_now(Path::new(p), env) {
                        return Some(format!("{p}: changed since the check"));
                    }
                    exec(git(repo, &["worktree", "remove", p], RUN_TIMEOUT)).err().map(|e| format!("{p}: {e}"))
                })
                .collect();
            let _ = exec(git(repo, &["worktree", "prune"], RUN_TIMEOUT));
            if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(parse_size("0B", 1000.0), Some(0));
        assert_eq!(parse_size("1.2GB", 1000.0), Some(1_200_000_000));
        assert_eq!(parse_size("512MB", 1000.0), Some(512_000_000));
        assert_eq!(parse_size("3.5kB", 1000.0), Some(3_500));
        assert_eq!(parse_size("1.5GB", 1024.0), Some(1_610_612_736));
        assert_eq!(parse_size("2 MiB", 1024.0), Some(2_097_152));
        assert_eq!(parse_size("12", 1000.0), Some(12));
        assert_eq!(parse_size("lots", 1000.0), None);
    }

    #[test]
    fn brew() {
        let out = "Would remove: /Users/u/Library/Caches/Homebrew/foo--1.0.bottle.tar.gz (1.1MB)\n==> This operation would free approximately 1.5GB of disk space.\n";
        assert_eq!(parse_brew_dry_run(out), 1_610_612_736);
        assert_eq!(parse_brew_dry_run(""), 0);
    }

    #[test]
    fn docker() {
        let out = r#"{"Active":"2","Reclaimable":"1.2GB (50%)","Size":"2.4GB","TotalCount":"5","Type":"Images"}
{"Active":"0","Reclaimable":"0B","Size":"0B","TotalCount":"0","Type":"Containers"}
{"Active":"0","Reclaimable":"512MB","Size":"512MB","TotalCount":"9","Type":"Build Cache"}
"#;
        // Only containers and build cache: `prune` keeps volumes and non-dangling images.
        assert_eq!(parse_docker_df(out), 512_000_000);
        assert_eq!(parse_docker_df("not json"), 0);
    }

    #[test]
    fn tmutil() {
        let out = "Snapshots for disk /:\ncom.apple.TimeMachine.2026-10-01-101010.local\ncom.apple.TimeMachine.2026-10-02-101010.local\n";
        assert_eq!(parse_tmutil(out), 2);
        assert_eq!(parse_tmutil("Snapshots for disk /:\n"), 0);
    }

    #[test]
    fn simctl() {
        let out = r#"{"devices":{"com.apple.CoreSimulator.SimRuntime.iOS-16-0":[{"name":"iPhone 14"},{"name":"iPad"}],"com.apple.CoreSimulator.SimRuntime.iOS-17-0":[]}}"#;
        assert_eq!(parse_simctl(out), Some(2));
        assert_eq!(parse_simctl("garbage"), None);
    }

    #[test]
    fn worktrees() {
        let out = "worktree /r/main\nHEAD abc\nbranch refs/heads/main\n\nworktree /r/wt-a\nHEAD def\nbranch refs/heads/feature/a\n\nworktree /r/wt-b\nHEAD 123\ndetached\nprunable gitdir file points to non-existent location\n\nworktree /r/wt-c\nHEAD 456\nbranch refs/heads/c\nlocked in use\n";
        assert_eq!(
            parse_worktrees(out),
            vec![
                Worktree { path: "/r/main".into(), branch: Some("main".into()), prunable: false, locked: false },
                Worktree { path: "/r/wt-a".into(), branch: Some("feature/a".into()), prunable: false, locked: false },
                Worktree { path: "/r/wt-b".into(), branch: None, prunable: true, locked: false },
                Worktree { path: "/r/wt-c".into(), branch: Some("c".into()), prunable: false, locked: true },
            ]
        );
    }

    #[test]
    fn disposable_worktree_status() {
        assert!(worktree_is_disposable(""));
        assert!(worktree_is_disposable("!! target/\n!! node_modules/\n"));
        assert!(worktree_is_disposable("!! sub/target/\n"));
        assert!(!worktree_is_disposable("!! .env\n"));
        assert!(!worktree_is_disposable("!! data/\n"));
        assert!(!worktree_is_disposable("?? notes.txt\n"));
        assert!(!worktree_is_disposable(" M src/x.rs\n"));
        assert!(!worktree_is_disposable("!! target/out.bin\n"));
        assert!(!worktree_is_disposable("!! build/release.key\n"));
        assert!(!worktree_is_disposable("!! target\n"));
        assert!(!worktree_is_disposable("?? x\n"));
    }

    #[test]
    fn repos_in_tree() {
        let dir = |n: &str, c: Vec<Node>| Node { name: n.into(), size: 1, kind: Kind::Dir, children: c.into() };
        let tree = dir(
            "/r",
            vec![
                dir("a", vec![dir(".git", vec![]), dir("src", vec![])]),
                dir("b", vec![dir("node_modules", vec![dir("x", vec![dir(".git", vec![])])])]),
                dir("c", vec![dir("d", vec![dir(".git", vec![])])]),
            ],
        );
        assert_eq!(git_repos(&tree, Path::new("/r"), 10), vec![PathBuf::from("/r/a"), PathBuf::from("/r/c/d")]);
        assert_eq!(git_repos(&tree, Path::new("/r"), 1), vec![PathBuf::from("/r/a")]);
    }

    #[test]
    fn cleaner_table() {
        assert!(Cleaner::TmSnapshots.applies("macos") && !Cleaner::TmSnapshots.applies("linux"));
        assert!(!Cleaner::Homebrew.applies("windows") && Cleaner::Homebrew.applies("linux"));
        assert_eq!(item_id(Cleaner::GitBranches, Some(Path::new("/r/a"))), "git-branches:/r/a");
        assert_eq!(item_id(Cleaner::Docker, None), "docker");
        assert!(Estimate::Bytes(0).is_nothing() && !Estimate::Unknown.is_nothing());
    }

    #[cfg(unix)]
    #[test]
    fn runner_timeout_kills_grandchildren() {
        let start = Instant::now();
        let out = real_runner()(&Cmd::new("sh", &["-c", "sleep 5; echo late"], Duration::from_millis(200)));
        assert_eq!(out, CmdOutput::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(2), "{:?}", start.elapsed());
    }

    #[cfg(unix)]
    #[test]
    fn runner_runs_reports_and_times_out() {
        let run = real_runner();
        match run(&Cmd::new("sh", &["-c", "echo hi; echo oops >&2; exit 3"], CHECK_TIMEOUT)) {
            CmdOutput::Done { ok, stdout, stderr } => {
                assert!(!ok);
                assert_eq!(stdout, "hi\n");
                assert_eq!(stderr, "oops\n");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(run(&Cmd::new("sleep", &["5"], Duration::from_millis(200))), CmdOutput::TimedOut);
        assert_eq!(run(&Cmd::new("definitely-not-a-real-tool-xyz", &[], CHECK_TIMEOUT)), CmdOutput::NotFound);
    }

    use std::sync::Mutex;

    fn done(stdout: &str) -> CmdOutput {
        CmdOutput::Done { ok: true, stdout: stdout.into(), stderr: String::new() }
    }

    fn fake(f: impl Fn(&Cmd) -> CmdOutput + Send + Sync + 'static) -> Runner {
        Arc::new(f)
    }

    #[test]
    fn check_tools_with_fake_runner() {
        let run = fake(|c| match (c.program, c.args.first().map(String::as_str)) {
            ("brew", Some("cleanup")) => done("==> This operation would free approximately 2MB of disk space."),
            ("docker", _) => CmdOutput::Done { ok: false, stdout: String::new(), stderr: "Cannot connect to the Docker daemon at unix:///var/run/docker.sock.\nmore".into() },
            ("tmutil", _) => CmdOutput::TimedOut,
            ("xcode-select", _) => done("/Library/Developer/CommandLineTools\n"),
            _ => CmdOutput::NotFound,
        });
        let env = Env { run: &run, trash_dir: None };
        assert_eq!(check_tool(Cleaner::Homebrew, &env), SystemStatus::Ready { estimate: Estimate::Bytes(2_097_152), detail: vec![] });
        assert_eq!(
            check_tool(Cleaner::Docker, &env),
            SystemStatus::Unavailable { reason: "Cannot connect to the Docker daemon at unix:///var/run/docker.sock.".into() }
        );
        assert_eq!(check_tool(Cleaner::TmSnapshots, &env), SystemStatus::Unavailable { reason: "timed out".into() });
        assert_eq!(check_tool(Cleaner::Simulators, &env), SystemStatus::Unavailable { reason: NOT_INSTALLED.into() });
    }

    #[test]
    fn check_trash_measures_the_trash_folder() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("old.bin"), vec![1u8; 5000]).unwrap();
        let run = fake(|_| CmdOutput::NotFound);
        let env = Env { run: &run, trash_dir: Some(tmp.path()) };
        match check_tool(Cleaner::EmptyTrash, &env) {
            SystemStatus::Ready { estimate: Estimate::Bytes(n), .. } => assert!(n >= 5000),
            other => panic!("{other:?}"),
        }
        let missing = tmp.path().join("missing");
        let env = Env { run: &run, trash_dir: Some(&missing) };
        assert_eq!(check_tool(Cleaner::EmptyTrash, &env), SystemStatus::Ready { estimate: Estimate::Bytes(0), detail: vec![] });
    }

    #[test]
    fn run_uses_fixed_commands_and_reports_failure() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let runner = fake(move |c| {
            log.lock().unwrap().push((c.program, c.args.clone()));
            if c.program == "docker" { CmdOutput::Done { ok: false, stdout: String::new(), stderr: "denied\n".into() } } else { done("") }
        });
        let env = Env { run: &runner, trash_dir: None };
        assert_eq!(run_cleaner(Cleaner::Homebrew, &env), Ok(()));
        assert_eq!(run_cleaner(Cleaner::Docker, &env), Err("denied".into()));
        assert_eq!(super::run(Cleaner::GitBranches, Some(Path::new("/r")), &["done".into(), "-x".into()], &env), Ok(()));
        let seen = seen.lock().unwrap();
        assert_eq!(seen[0], ("brew", vec!["cleanup".to_string()]));
        assert_eq!(seen[1], ("docker", vec!["system".to_string(), "prune".into(), "-f".into()]));
        // "-x" is skipped: branch names starting with '-' are never passed
        assert_eq!(seen[2], ("git", vec!["-c".to_string(), "core.fsmonitor=false".into(), "-C".into(), "/r".into(), "branch".into(), "-d".into(), "done".into()]));
    }

    fn run_cleaner(c: Cleaner, env: &Env) -> Result<(), String> {
        run(c, None, &[], env)
    }

    /// Real git: merged branch + clean merged worktree are found; dirty worktree and
    /// unmerged/checked-out branches are not. Skipped when git is not installed.
    #[test]
    fn git_integration() {
        if find_tool("git").is_none() {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        let repo = base.join("repo");
        std::fs::create_dir(&repo).unwrap();
        let git = |dir: &Path, args: &[&str]| {
            let ok = std::process::Command::new("git").arg("-C").arg(dir).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").output().unwrap().status.success();
            assert!(ok, "git {args:?}");
        };
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "t@example.com"]);
        git(&repo, &["config", "user.name", "T"]);
        std::fs::write(repo.join("f"), "1").unwrap();
        std::fs::write(repo.join(".gitignore"), ".env\ntarget/\n").unwrap();
        git(&repo, &["add", "f", ".gitignore"]);
        git(&repo, &["commit", "-q", "-m", "one"]);
        git(&repo, &["branch", "done"]);
        git(&repo, &["checkout", "-q", "-b", "open"]);
        std::fs::write(repo.join("g"), "2").unwrap();
        git(&repo, &["add", "g"]);
        git(&repo, &["commit", "-q", "-m", "two"]);
        git(&repo, &["checkout", "-q", "main"]);
        let wt_clean = base.join("wt-clean");
        let wt_dirty = base.join("wt-dirty");
        git(&repo, &["worktree", "add", "-q", wt_clean.to_str().unwrap(), "-b", "wt-clean"]);
        git(&repo, &["worktree", "add", "-q", wt_dirty.to_str().unwrap(), "-b", "wt-dirty"]);
        std::fs::write(wt_dirty.join("scratch.txt"), "untracked").unwrap();
        let wt_env = base.join("wt-env");
        let wt_build = base.join("wt-build");
        git(&repo, &["worktree", "add", "-q", wt_env.to_str().unwrap(), "-b", "wt-env"]);
        git(&repo, &["worktree", "add", "-q", wt_build.to_str().unwrap(), "-b", "wt-build"]);
        std::fs::write(wt_env.join(".env"), "SECRET=1").unwrap();
        std::fs::create_dir(wt_build.join("target")).unwrap();
        std::fs::write(wt_build.join("target/out.bin"), "x").unwrap();

        let run = real_runner();
        let env = Env { run: &run, trash_dir: None };
        git(&repo, &["checkout", "-q", "--detach"]);
        let detached = check_git(&repo, &env).unwrap();
        assert!(detached.branches.iter().all(|b| !b.contains("detached")), "{detached:?}");
        git(&repo, &["checkout", "-q", "main"]);
        let found = check_git(&repo, &env).unwrap();
        assert_eq!(found.main, "main");
        assert_eq!(found.branches, vec!["done".to_string()]);
        let mut wts = found.worktrees.clone();
        wts.sort();
        let mut expected = vec![wt_build.clone(), wt_clean.clone()];
        expected.sort();
        assert_eq!(wts, expected); // wt-env (ignored .env) and wt-dirty are not offered

        let wt = [wt_clean.display().to_string(), wt_build.display().to_string()];
        assert_eq!(super::run(Cleaner::GitWorktrees, Some(&repo), &wt, &env), Ok(()));
        assert!(!wt_clean.exists());
        assert!(!wt_build.exists()); // only an ignored target/ inside: removed
        assert!(wt_dirty.exists());
        assert_eq!(super::run(Cleaner::GitBranches, Some(&repo), &["done".into()], &env), Ok(()));
        let mut left = check_git(&repo, &env).unwrap().branches;
        left.sort();
        assert_eq!(left, vec!["wt-build".to_string(), "wt-clean".to_string()]); // their worktrees are gone, so now offered
    }

    fn tgit(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git").arg("-C").arg(dir).args(args).env("GIT_CONFIG_GLOBAL", "/dev/null").output().unwrap().status.success();
        assert!(ok, "git {args:?}");
    }

    /// A repo on `main` with a committed `.gitignore` (`.env`, `*.key`, `target/`) and `build/README`.
    fn temp_repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let base = dunce::canonicalize(tmp.path()).unwrap();
        let repo = base.join("repo");
        std::fs::create_dir_all(repo.join("build")).unwrap();
        tgit(&repo, &["init", "-q", "-b", "main"]);
        tgit(&repo, &["config", "user.email", "t@example.com"]);
        tgit(&repo, &["config", "user.name", "T"]);
        std::fs::write(repo.join(".gitignore"), ".env\n*.key\ntarget/\n").unwrap();
        std::fs::write(repo.join("build/README"), "r").unwrap();
        tgit(&repo, &["add", ".gitignore", "build/README"]);
        tgit(&repo, &["commit", "-q", "-m", "one"]);
        (tmp, base, repo)
    }

    /// origin/HEAD naming a branch that only exists on the remote (never checked out here) must
    /// not fail the repo: fall back to the local `main`.
    #[test]
    fn git_origin_head_to_missing_local_branch_falls_back_to_main() {
        if find_tool("git").is_none() {
            return;
        }
        let (_tmp, _base, repo) = temp_repo();
        tgit(&repo, &["branch", "merged-one"]);
        let head = repo.join(".git/refs/remotes/origin");
        std::fs::create_dir_all(&head).unwrap();
        std::fs::write(head.join("HEAD"), "ref: refs/remotes/origin/feat/elsewhere\n").unwrap();
        let run = real_runner();
        let env = Env { run: &run, trash_dir: None };
        let found = check_git(&repo, &env).unwrap();
        assert_eq!((found.main.as_str(), found.branches), ("main", vec!["merged-one".to_string()]));
    }

    /// An ignored secret inside a tracked `build/` folder keeps the worktree.
    #[test]
    fn git_ignored_file_in_regenerable_named_dir_is_kept() {
        if find_tool("git").is_none() {
            return;
        }
        let (_tmp, base, repo) = temp_repo();
        let wt = base.join("wt-key");
        tgit(&repo, &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "wt-key"]);
        std::fs::write(wt.join("build/release.key"), "secret").unwrap();
        let run = real_runner();
        let env = Env { run: &run, trash_dir: None };
        assert_eq!(check_git(&repo, &env).unwrap().worktrees, Vec::<PathBuf>::new());
    }

    /// `out/` is not ignored itself, only the `*.key` file inside, yet `git status` collapses it
    /// to "!! out/": the worktree must be kept. An ignored `target/` still counts as disposable.
    #[test]
    fn git_build_named_dir_must_itself_be_ignored() {
        if find_tool("git").is_none() {
            return;
        }
        let (_tmp, base, repo) = temp_repo();
        let wt_key = base.join("wt-out");
        let wt_target = base.join("wt-target");
        tgit(&repo, &["worktree", "add", "-q", wt_key.to_str().unwrap(), "-b", "wt-out"]);
        tgit(&repo, &["worktree", "add", "-q", wt_target.to_str().unwrap(), "-b", "wt-target"]);
        std::fs::create_dir(wt_key.join("out")).unwrap();
        std::fs::write(wt_key.join("out/prod.key"), "secret").unwrap();
        std::fs::create_dir(wt_target.join("target")).unwrap();
        std::fs::write(wt_target.join("target/out.bin"), "x").unwrap();
        let run = real_runner();
        let env = Env { run: &run, trash_dir: None };
        assert_eq!(check_git(&repo, &env).unwrap().worktrees, vec![wt_target.clone()]);
        let err = super::run(Cleaner::GitWorktrees, Some(&repo), &[wt_key.display().to_string()], &env).unwrap_err();
        assert!(err.contains("changed since the check"), "{err}");
        assert!(wt_key.join("out/prod.key").exists());
    }

    /// Quoted status lines (special characters) are never disposable; check-ignore failures keep the worktree.
    #[test]
    fn check_ignore_gates_disposable_dirs() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let runner = fake(move |c| {
            let a: Vec<&str> = c.args.iter().map(String::as_str).collect();
            log.lock().unwrap().push(a.join(" "));
            match a.get(4) {
                Some(&"status") => done(if a[3] == "/q" { "!! \"target\\t/\"\n" } else { "!! target/\n!! sub/node_modules/\n" }),
                Some(&"check-ignore") if a[3] == "/no" && a[7] == "sub/node_modules/" => CmdOutput::Done { ok: false, stdout: String::new(), stderr: String::new() },
                Some(&"check-ignore") => done(""),
                _ => CmdOutput::NotFound,
            }
        });
        let env = Env { run: &runner, trash_dir: None };
        assert!(worktree_disposable_now(Path::new("/ok"), &env));
        assert!(!worktree_disposable_now(Path::new("/no"), &env));
        assert!(!worktree_disposable_now(Path::new("/q"), &env));
        let seen = seen.lock().unwrap();
        assert!(seen.contains(&"-c core.fsmonitor=false -C /ok check-ignore -q -- target/".to_string()), "{seen:?}");
        assert!(seen.contains(&"-c core.fsmonitor=false -C /ok check-ignore -q -- sub/node_modules/".to_string()), "{seen:?}");
    }

    /// A worktree that changed between the check and the confirmed run is skipped.
    #[test]
    fn git_worktree_rechecked_before_removal() {
        if find_tool("git").is_none() {
            return;
        }
        let (_tmp, base, repo) = temp_repo();
        let wt = base.join("wt-x");
        tgit(&repo, &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "wt-x"]);
        let run = real_runner();
        let env = Env { run: &run, trash_dir: None };
        assert_eq!(check_git(&repo, &env).unwrap().worktrees, vec![wt.clone()]);
        std::fs::write(wt.join(".env"), "SECRET=1").unwrap();
        let err = super::run(Cleaner::GitWorktrees, Some(&repo), &[wt.display().to_string()], &env).unwrap_err();
        assert!(err.contains("changed since the check"), "{err}");
        assert!(wt.join(".env").exists());
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_trash_is_unknown_not_empty() {
        use std::os::unix::fs::PermissionsExt;
        if unsafe { libc::geteuid() } == 0 {
            return; // root reads everything
        }
        let tmp = tempfile::tempdir().unwrap();
        let locked = tmp.path().join("locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::write(locked.join("x.bin"), vec![1u8; 5000]).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        let run = fake(|_| CmdOutput::NotFound);
        let env = Env { run: &run, trash_dir: Some(tmp.path()) };
        let status = check_tool(Cleaner::EmptyTrash, &env);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(status, SystemStatus::Ready { estimate: Estimate::Unknown, detail: vec![] });
    }

    /// Without the Command Line Tools, xcrun/git would pop up an install dialog: don't run them.
    #[cfg(target_os = "macos")]
    #[test]
    fn missing_dev_tools_skips_xcrun_and_git() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        let run = fake(move |c| {
            log.lock().unwrap().push(c.program);
            CmdOutput::Done { ok: false, stdout: String::new(), stderr: "xcode-select: error: unable to get active developer directory".into() }
        });
        let env = Env { run: &run, trash_dir: None };
        assert_eq!(check_tool(Cleaner::Simulators, &env), SystemStatus::Unavailable { reason: NOT_INSTALLED.into() });
        assert_eq!(check_git(Path::new("/r"), &env), Err(NOT_INSTALLED.to_string()));
        assert!(seen.lock().unwrap().iter().all(|p| *p == "xcode-select"), "{:?}", seen.lock().unwrap());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn finder_automation_denied_is_reported() {
        let run = fake(|_| CmdOutput::Done { ok: false, stdout: String::new(), stderr: "execution error: Not authorized to send Apple events to Finder. (-1743)\n".into() });
        let env = Env { run: &run, trash_dir: None };
        assert_eq!(run_cleaner(Cleaner::EmptyTrash, &env), Err(AUTOMATION_DENIED.to_string()));
    }
}
