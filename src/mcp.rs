use crate::actions::Origin;
use crate::scan::{self, Kind, Node};
use crate::state::{ItemStatus, Report, Shared};
use base64::Engine;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{ErrorData as McpError, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

/// How long an agent's trash request waits for the user before it counts as declined.
const CONFIRM_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Clone)]
pub struct Tools {
    pub app: Arc<Shared>,
}

#[derive(Deserialize, JsonSchema)]
pub struct PathArg {
    /// Absolute path; "~" means the home folder.
    pub path: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct OptPathArg {
    /// Absolute path; defaults to the folder currently shown.
    pub path: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct TreeArgs {
    /// Absolute path; defaults to the folder currently shown.
    pub path: Option<String>,
    /// Levels of children to include, 1–4 (default 1).
    pub depth: Option<u32>,
    /// Skip entries smaller than this many bytes (default 0).
    pub min_size: Option<u64>,
}

#[derive(Deserialize, JsonSchema)]
pub struct TopArgs {
    /// Absolute path; defaults to the scan root.
    pub path: Option<String>,
    /// Number of files, 1–200 (default 20).
    pub limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScreenName {
    Home,
    DiskMap,
    Cleanup,
    SystemTools,
}

#[derive(Deserialize, JsonSchema)]
pub struct ScreenArgs {
    /// home, disk_map (needs a scan), cleanup, or system_tools (Cleanup with the system tools first).
    pub screen: ScreenName,
}

#[derive(Deserialize, JsonSchema)]
pub struct SystemArgs {
    /// Id from list_system_cleanups, e.g. "homebrew" or "git-worktrees:/path/to/repo".
    pub id: String,
    /// Plain-language reason, shown to the user in the confirmation dialog.
    pub reason: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct TrashArgs {
    /// Absolute paths to move to the Trash.
    pub paths: Vec<String>,
    /// Plain-language reason, shown to the user in the confirmation dialog.
    pub reason: String,
}

fn ok_json(v: impl Serialize) -> Result<CallToolResult, McpError> {
    Ok(CallToolResult::success(vec![ContentBlock::text(serde_json::to_string_pretty(&v).expect("serializable"))]))
}

fn err(msg: impl Into<String>) -> McpError {
    McpError::invalid_params(msg.into(), None)
}

/// True for a Host or Origin header value that points at this machine.
pub fn is_local(value: &str) -> bool {
    let host = value.split("://").last().unwrap_or(value);
    let host = host.rsplit_once(':').map_or(host, |(h, port)| if port.chars().all(|c| c.is_ascii_digit()) { h } else { host });
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

pub fn client_config(port: u16) -> String {
    serde_json::json!({ "mcpServers": { "fastscancleaner": { "type": "http", "url": format!("http://127.0.0.1:{port}/mcp") } } }).to_string()
}

/// Expands a leading `~` (exactly "~", or "~/" and "~\\"); everything else is returned as given.
/// Never resolves symlinks: trashing a link must act on the link, not its target.
fn expand(p: &str, home: &Path) -> PathBuf {
    match p {
        "~" => home.to_path_buf(),
        _ => match p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
            Some(rest) => home.join(rest),
            None => PathBuf::from(p),
        },
    }
}

/// Like `expand`, then canonicalised the way actions do (a final symlink is kept, only its parent is resolved),
/// so the path can be compared with the scan root.
fn expand_resolved(p: &str, home: &Path) -> PathBuf {
    let p = expand(p, home);
    crate::actions::resolve(&p).unwrap_or(p)
}

fn kind_str(k: Kind) -> &'static str {
    match k {
        Kind::Dir => "dir",
        Kind::File => "file",
        Kind::Symlink => "symlink",
    }
}

#[derive(Serialize)]
struct TreeOut {
    name: String,
    path: PathBuf,
    size: u64,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    advice: Option<AdviceOut>,
    children: Vec<TreeOut>,
    /// Children left out because of depth, min_size or the 50-per-folder cap.
    more: usize,
}

#[derive(Serialize)]
struct AdviceOut {
    safety: crate::rules::Safety,
    text: String,
}

const MAX_CHILDREN: usize = 50;

impl Tools {
    fn advice(&self, path: &Path) -> Option<AdviceOut> {
        self.app.deps.rules.matching(path).map(|r| AdviceOut { safety: r.safety, text: rust_i18n::t!(&r.message, locale = "en").to_string() })
    }

    fn tree_out(&self, node: &Node, path: PathBuf, depth: u32, min_size: u64) -> TreeOut {
        let mut children = Vec::new();
        let mut more = 0;
        for c in &node.children {
            if depth == 0 || c.size < min_size || children.len() == MAX_CHILDREN {
                more += 1;
            } else {
                children.push(self.tree_out(c, path.join(&*c.name), depth - 1, min_size));
            }
        }
        TreeOut { name: node.name.to_string(), advice: self.advice(&path), path, size: node.size, kind: kind_str(node.kind), children, more }
    }

    /// Absolute path + relative path of `p` (default: current zoom) inside the scan.
    fn locate(&self, p: Option<&str>) -> Result<(PathBuf, PathBuf), McpError> {
        let c = self.app.core.lock().unwrap();
        let root = c.root.clone().ok_or_else(|| err("nothing scanned yet; call scan first"))?;
        let abs = match p {
            Some(p) => expand_resolved(p, &self.app.deps.home),
            None => root.join(&c.zoom),
        };
        let rel = abs.strip_prefix(&root).map_err(|_| err("path is outside the current scan"))?.to_path_buf();
        Ok((abs, rel))
    }
}

fn report_out(report: Report) -> Vec<serde_json::Value> {
    report
        .into_iter()
        .map(|(path, status)| {
            let mut v = serde_json::to_value(status).unwrap();
            v["path"] = serde_json::json!(path);
            v
        })
        .collect()
}

#[tool_router]
impl Tools {
    #[tool(description = "Current screen, scan root, zoomed folder, selection, scan progress (with the largest folders so far), unreadable folder count, pending confirmation and disk usage.")]
    fn get_status(&self) -> Result<CallToolResult, McpError> {
        let c = self.app.core.lock().unwrap();
        let root = c.root.clone();
        let abs = |rel: &PathBuf| root.as_ref().map(|r| r.join(rel));
        ok_json(serde_json::json!({
            "screen": format!("{:?}", c.screen),
            "root": root,
            "zoom": abs(&c.zoom),
            "selected": c.selected.as_ref().and_then(abs),
            "busy": c.busy,
            "scanning": c.scanning,
            "last_scan": c.stats,
            "interrupted": c.interrupted,
            "files_scanned": c.progress.files.load(Relaxed),
            // Largest top-level folders counted so far (sizes still growing).
            "largest_so_far": c.scanning.as_ref().map(|s| c.progress.live.snapshot("").children.iter().take(5)
                .map(|n| serde_json::json!({"path": s.join(&*n.name), "size": n.size})).collect::<Vec<_>>()),
            "unreadable_folders": c.progress.denied.load(Relaxed),
            "total_size": c.tree.as_ref().map(|t| t.size),
            "waiting_for_user_confirmation": c.pending.is_some(),
            "volumes": crate::platform::volumes().iter().map(|v| serde_json::json!({
                "name": v.name, "mount": v.mount, "total": v.total, "available": v.available
            })).collect::<Vec<_>>(),
        }))
    }

    #[tool(description = "Folders scanned recently, newest first.")]
    fn list_recent_scans(&self) -> Result<CallToolResult, McpError> {
        ok_json(&self.app.core.lock().unwrap().settings.recent)
    }

    #[tool(description = "Scan a folder (the app shows it live). Returns when the scan is finished; large disks can take minutes.")]
    async fn scan(&self, Parameters(PathArg { path }): Parameters<PathArg>) -> Result<CallToolResult, McpError> {
        let rx = self.app.start_scan(expand(&path, &self.app.deps.home));
        rx.await.map_err(|_| err("scan was cancelled"))?.map_err(err)?;
        let c = self.app.core.lock().unwrap();
        ok_json(serde_json::json!({
            "root": c.root,
            "total_size": c.tree.as_ref().map(|t| t.size),
            "files": c.progress.files.load(Relaxed),
            "unreadable_folders": c.progress.denied.load(Relaxed),
            "findings": c.findings.len(),
        }))
    }

    #[tool(description = "Re-scan a folder inside the current scan (default: the folder currently shown) and update the view.")]
    async fn refresh(&self, Parameters(OptPathArg { path }): Parameters<OptPathArg>) -> Result<CallToolResult, McpError> {
        let abs = path.map(|p| expand_resolved(&p, &self.app.deps.home));
        self.app.refresh(abs).await.map_err(|_| err("refresh was cancelled"))?.map_err(err)?;
        ok_json(serde_json::json!({ "refreshed": true }))
    }

    #[tool(description = "Sizes of a folder and its children, largest first, with built-in advice where known. Max 50 children per folder.")]
    fn get_tree(&self, Parameters(TreeArgs { path, depth, min_size }): Parameters<TreeArgs>) -> Result<CallToolResult, McpError> {
        let (abs, rel) = self.locate(path.as_deref())?;
        let c = self.app.core.lock().unwrap();
        let node = c.tree.as_ref().and_then(|t| t.find(&rel)).ok_or_else(|| err("path is not in the current scan"))?;
        ok_json(self.tree_out(node, abs, depth.unwrap_or(1).clamp(1, 4), min_size.unwrap_or(0)))
    }

    #[tool(description = "Largest files inside a folder of the current scan (default: the whole scan).")]
    fn top_items(&self, Parameters(TopArgs { path, limit }): Parameters<TopArgs>) -> Result<CallToolResult, McpError> {
        let (abs, rel) = match path {
            Some(p) => self.locate(Some(&p))?,
            None => {
                let root = self.app.core.lock().unwrap().root.clone().ok_or_else(|| err("nothing scanned yet; call scan first"))?;
                (root, PathBuf::new())
            }
        };
        let c = self.app.core.lock().unwrap();
        let node = c.tree.as_ref().and_then(|t| t.find(&rel)).ok_or_else(|| err("path is not in the current scan"))?;
        let files = scan::largest_files(node, &abs, limit.unwrap_or(20).clamp(1, 200));
        ok_json(files.into_iter().map(|(path, size)| serde_json::json!({ "path": path, "size": size })).collect::<Vec<_>>())
    }

    #[tool(description = "Known space users found by the built-in guide (caches, installers, node_modules, …) with a safety level: safe, review or keep.")]
    fn get_findings(&self) -> Result<CallToolResult, McpError> {
        let c = self.app.core.lock().unwrap();
        ok_json(
            c.findings
                .iter()
                .map(|f| serde_json::json!({
                    "path": f.path, "size": f.size, "rule_id": f.rule_id, "safety": f.safety,
                    "advice": rust_i18n::t!(&f.message, locale = "en"),
                }))
                .collect::<Vec<_>>(),
        )
    }

    #[tool(description = "Show a folder in the app's map (or a file's folder, with the file selected) so the user sees what you are talking about.")]
    fn navigate(&self, Parameters(PathArg { path }): Parameters<PathArg>) -> Result<CallToolResult, McpError> {
        self.app.navigate(&expand_resolved(&path, &self.app.deps.home)).map_err(err)?;
        ok_json(serde_json::json!({ "ok": true }))
    }

    #[tool(description = "Select an item in the app so its details and advice are shown to the user.")]
    fn select(&self, Parameters(PathArg { path }): Parameters<PathArg>) -> Result<CallToolResult, McpError> {
        self.app.select(&expand_resolved(&path, &self.app.deps.home)).map_err(err)?;
        ok_json(serde_json::json!({ "ok": true }))
    }

    #[tool(description = "PNG screenshot of the app window.")]
    fn screenshot(&self) -> Result<CallToolResult, McpError> {
        let png = capture().map_err(|e| McpError::internal_error(e, None))?;
        Ok(CallToolResult::success(vec![ContentBlock::image(base64::engine::general_purpose::STANDARD.encode(png), "image/png")]))
    }

    #[tool(description = "Switch the app to a screen so the user sees what you are talking about: home, disk_map (needs a scan; otherwise stays on home), cleanup, or system_tools (Cleanup with the system tools section first). Settings are not reachable. Returns the screen actually shown.")]
    fn show_screen(&self, Parameters(ScreenArgs { screen }): Parameters<ScreenArgs>) -> Result<CallToolResult, McpError> {
        match screen {
            ScreenName::Home => self.app.show(crate::state::Screen::Home),
            ScreenName::DiskMap => self.app.show(crate::state::Screen::DiskMap),
            ScreenName::Cleanup => self.app.show(crate::state::Screen::Cleanup),
            ScreenName::SystemTools => self.app.show_system_tools(),
        }
        ok_json(serde_json::json!({ "screen": format!("{:?}", self.app.core.lock().unwrap().screen) }))
    }

    #[tool(description = "System cleanups that are NOT restorable (empty Trash, Homebrew, Docker, Time Machine local snapshots, unavailable simulators, merged git branches/worktrees in scanned repos) with their status and estimate. Runs the checks if none are cached (up to ~25 s).")]
    async fn list_system_cleanups(&self) -> Result<CallToolResult, McpError> {
        if self.app.core.lock().unwrap().system.is_empty() {
            self.app.check_system();
        }
        for _ in 0..125 {
            if !self.app.core.lock().unwrap().system.values().any(|i| matches!(i.status, crate::cleaners::SystemStatus::Checking)) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let c = self.app.core.lock().unwrap();
        ok_json(
            c.system
                .iter()
                .map(|(id, i)| serde_json::json!({ "id": id, "cleaner": i.cleaner, "repo": i.repo, "status": i.status }))
                .collect::<Vec<_>>(),
        )
    }

    #[tool(description = "Ask the user to run one system cleanup from list_system_cleanups. These are NOT restorable; the user always confirms in the app (no allowlist). Waits up to 10 minutes. Returns status done (with freed bytes when known), declined or failed.")]
    async fn request_system_cleanup(&self, Parameters(SystemArgs { id, reason }): Parameters<SystemArgs>) -> Result<CallToolResult, McpError> {
        let mut rx = self.app.request_system_cleanup(&id, Some(reason), Origin::Agent).map_err(err)?;
        let internal = || crate::state::SystemOutcome::Failed { error: "internal error".into() };
        let outcome = match tokio::time::timeout(CONFIRM_TIMEOUT, &mut rx).await {
            Ok(Ok(o)) => o,
            Ok(Err(_)) => internal(),
            Err(_) => {
                if self.app.decline_system_if_pending(&id) {
                    crate::state::SystemOutcome::Declined
                } else {
                    // Confirmed already and still running: wait for the real result.
                    rx.await.unwrap_or_else(|_| internal())
                }
            }
        };
        ok_json(outcome)
    }

    #[tool(description = "Move items to the Trash (never deletes permanently). Items inside folders the user allow-listed are moved at once; anything else opens a confirmation dialog and this call waits for the user's decision (up to 10 minutes). Per-item status: trashed, refused, failed or declined.")]
    async fn request_trash(&self, Parameters(TrashArgs { paths, reason }): Parameters<TrashArgs>) -> Result<CallToolResult, McpError> {
        let paths = paths.iter().map(|p| expand(p, &self.app.deps.home)).collect();
        let (mut report, rx) = self.app.request_trash(paths, Some(reason), Origin::Agent);
        if let Some(rx) = rx {
            let decided = match tokio::time::timeout(CONFIRM_TIMEOUT, rx).await {
                Ok(Ok(r)) => r,
                _ => self.app.resolve_pending(false),
            };
            report.retain(|(_, s)| *s != ItemStatus::Pending);
            report.extend(decided);
        }
        ok_json(report_out(report))
    }
}

#[tool_handler]
impl ServerHandler for Tools {
    fn get_info(&self) -> ServerConfig {
        // Agents see this name instead of the library's default ("rmcp").
        let mut app = Implementation::from_build_env();
        app.name = "fastscancleaner".into();
        app.title = Some("FastScanCleaner".into());
        app.version = env!("CARGO_PKG_VERSION").into(); // from_build_env expands inside rmcp, so it reports rmcp's version
        let mut info = ServerConfig::default();
        info.server_info = app;
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info
    }
}

fn capture() -> Result<Vec<u8>, String> {
    let pid = std::process::id();
    let window = xcap::Window::all()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|w| w.pid().ok() == Some(pid) && !w.is_minimized().unwrap_or(true))
        .ok_or("app window not found (on macOS, allow Screen Recording for this app)")?;
    let image = window.capture_image().map_err(|e| e.to_string())?;
    let mut png = Vec::new();
    image.write_to(&mut std::io::Cursor::new(&mut png), xcap::image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(png)
}

async fn local_only(req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    use axum::{http::StatusCode, response::IntoResponse};
    let allowed = ["host", "origin"].iter().all(|n| req.headers().get(*n).and_then(|v| v.to_str().ok()).is_none_or(is_local));
    if allowed { next.run(req).await } else { StatusCode::FORBIDDEN.into_response() }
}

/// The MCP endpoint at `/mcp`, behind the localhost-only guard.
fn router(app: Arc<Shared>) -> axum::Router {
    use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager};
    let tools = Tools { app };
    let config = StreamableHttpServerConfig::default().with_allowed_origins(["http://localhost:*", "http://127.0.0.1:*", "http://[::1]:*"]);
    let service = StreamableHttpService::new(move || Ok(tools.clone()), LocalSessionManager::default().into(), config);
    axum::Router::new().nest_service("/mcp", service).layer(axum::middleware::from_fn(local_only))
}

pub fn spawn(app: Arc<Shared>, port: u16) {
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async move {
            let router = router(app.clone());
            match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
                Ok(listener) => {
                    if let Err(e) = axum::serve(listener, router).await {
                        app.set_mcp_error(e.to_string());
                    }
                }
                Err(e) => app.set_mcp_error(format!("port {port}: {e}")),
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::tests::fixture;

    fn body(r: &CallToolResult) -> serde_json::Value {
        serde_json::from_str(&r.content[0].as_text().expect("text content").text).unwrap()
    }

    #[test]
    fn localhost_guard() {
        for ok in ["127.0.0.1:7878", "localhost:7878", "localhost", "http://127.0.0.1:7878", "http://localhost:1"] {
            assert!(is_local(ok), "{ok}");
        }
        for bad in ["evil.com", "localhost.evil.com:7878", "127.0.0.1.nip.io", "http://evil.com", "10.0.0.1:7878"] {
            assert!(!is_local(bad), "{bad}");
        }
    }

    #[test]
    fn tool_list_is_exactly_this() {
        // Guards the safety model: no tool may change settings or the allowlist.
        let mut names: Vec<String> = Tools::tool_router().list_all().into_iter().map(|t| t.name.to_string()).collect();
        names.sort();
        assert_eq!(
            names,
            ["get_findings", "get_status", "get_tree", "list_recent_scans", "list_system_cleanups", "navigate", "refresh", "request_system_cleanup", "request_trash", "scan", "screenshot", "select", "show_screen", "top_items"]
        );
    }

    #[tokio::test]
    async fn status_tree_top_and_findings() {
        let f = fixture().await;
        let t = Tools { app: f.app.clone() };
        let s = body(&t.get_status().unwrap());
        assert_eq!(s["screen"], "Home");
        assert!(s["last_scan"]["files"].as_u64().unwrap() >= 3);
        assert_eq!(s["root"], f.base.to_string_lossy().as_ref());

        let tree = body(&t.get_tree(Parameters(TreeArgs { path: None, depth: Some(2), min_size: None })).unwrap());
        assert_eq!(tree["children"].as_array().unwrap().len(), 3);
        assert_eq!(tree["children"][0]["name"], "cache");
        assert_eq!(tree["children"][0]["children"][0]["name"], "a.bin");

        let top = body(&t.top_items(Parameters(TopArgs { path: None, limit: Some(1) })).unwrap());
        assert_eq!(top[0]["path"], f.base.join("cache/a.bin").to_string_lossy().as_ref());

        let findings = body(&t.get_findings().unwrap());
        assert_eq!(findings[0]["rule_id"], "node_modules");
        assert!(findings[0]["advice"].as_str().unwrap().contains("npm install"));
    }

    #[tokio::test]
    async fn navigate_moves_the_ui() {
        let f = fixture().await;
        let t = Tools { app: f.app.clone() };
        t.navigate(Parameters(PathArg { path: f.base.join("docs").to_string_lossy().into() })).unwrap();
        assert_eq!(f.app.core.lock().unwrap().zoom, PathBuf::from("docs"));
        assert!(t.navigate(Parameters(PathArg { path: "/elsewhere".into() })).is_err());
    }

    #[tokio::test]
    async fn trash_allowlisted_now_and_others_after_confirm() {
        let f = fixture().await;
        let t = Tools { app: f.app.clone() };
        let r = body(
            &t.request_trash(Parameters(TrashArgs { paths: vec![f.base.join("cache/a.bin").to_string_lossy().into()], reason: "cache".into() }))
                .await
                .unwrap(),
        );
        assert_eq!(r[0]["status"], "trashed");

        let app = f.app.clone();
        let confirmer = tokio::spawn(async move {
            while app.core.lock().unwrap().pending.is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            app.resolve_pending(true);
        });
        let r = body(
            &t.request_trash(Parameters(TrashArgs { paths: vec![f.base.join("docs/b.bin").to_string_lossy().into()], reason: "old".into() }))
                .await
                .unwrap(),
        );
        confirmer.await.unwrap();
        assert_eq!(r.as_array().unwrap().len(), 1);
        assert_eq!(r[0]["status"], "trashed");
    }

    #[tokio::test]
    async fn scan_tool_waits_for_completion() {
        let f = fixture().await;
        let t = Tools { app: f.app.clone() };
        let r = body(&t.scan(Parameters(PathArg { path: f.base.join("docs").to_string_lossy().into() })).await.unwrap());
        assert_eq!(r["root"], f.base.join("docs").to_string_lossy().as_ref());
        assert!(t.scan(Parameters(PathArg { path: "/definitely/missing".into() })).await.is_err());
    }

    #[test]
    fn expand_only_expands_tilde() {
        let home = Path::new("/h");
        assert_eq!(expand("~", home), PathBuf::from("/h"));
        assert_eq!(expand("~/x/y", home), PathBuf::from("/h/x/y"));
        assert_eq!(expand("~foo", home), PathBuf::from("~foo"));
        assert_eq!(expand("/a/b", home), PathBuf::from("/a/b"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn trashing_a_symlink_removes_only_the_link() {
        let f = fixture().await;
        let link = f.base.join("cache/link");
        std::os::unix::fs::symlink(f.base.join("docs"), &link).unwrap();
        let t = Tools { app: f.app.clone() };
        let r = body(&t.request_trash(Parameters(TrashArgs { paths: vec![link.to_string_lossy().into()], reason: "x".into() })).await.unwrap());
        assert_eq!(r[0]["status"], "trashed");
        assert!(std::fs::symlink_metadata(&link).is_err(), "link should be gone");
        assert!(f.base.join("docs/b.bin").exists(), "link target must be untouched");
    }

    #[tokio::test]
    async fn show_screen_switches_the_app() {
        let f = fixture().await;
        let t = Tools { app: f.app.clone() };
        let r = body(&t.show_screen(Parameters(ScreenArgs { screen: ScreenName::SystemTools })).unwrap());
        assert_eq!(r["screen"], "Cleanup");
        assert!(f.app.core.lock().unwrap().system_first);
        let r = body(&t.show_screen(Parameters(ScreenArgs { screen: ScreenName::DiskMap })).unwrap());
        assert_eq!(r["screen"], "DiskMap");
        f.app.core.lock().unwrap().tree = None;
        let r = body(&t.show_screen(Parameters(ScreenArgs { screen: ScreenName::DiskMap })).unwrap());
        assert_eq!(r["screen"], "Home"); // the map needs a scan
    }

    #[tokio::test]
    async fn system_cleanups_list_and_confirmed_run() {
        let f = fixture().await;
        let t = Tools { app: f.app.clone() };
        let list = body(&t.list_system_cleanups().await.unwrap());
        let brew = list.as_array().unwrap().iter().find(|i| i["id"] == "homebrew").unwrap();
        assert_eq!(brew["status"]["state"], "ready");
        assert_eq!(brew["status"]["estimate"]["value"], 1_048_576);

        let app = f.app.clone();
        let confirmer = tokio::spawn(async move {
            while app.core.lock().unwrap().pending.is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            app.resolve_pending(true);
        });
        let r = body(&t.request_system_cleanup(Parameters(SystemArgs { id: "homebrew".into(), reason: "old bottles".into() })).await.unwrap());
        confirmer.await.unwrap();
        assert_eq!(r["status"], "done");
        assert_eq!(r["freed"], 1_048_576);

        assert!(t.request_system_cleanup(Parameters(SystemArgs { id: "docker".into(), reason: "x".into() })).await.is_err());
    }

    async fn post(app: Arc<Shared>, headers: &[(&str, &str)]) -> axum::http::StatusCode {
        use tower::ServiceExt;
        let mut req = axum::http::Request::builder().method("POST").uri("/mcp").header("content-type", "application/json");
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        router(app).oneshot(req.body(axum::body::Body::from("{}")).unwrap()).await.unwrap().status()
    }

    #[tokio::test]
    async fn http_layer_rejects_foreign_host_and_origin() {
        use axum::http::StatusCode;
        let f = fixture().await;
        assert_eq!(post(f.app.clone(), &[("host", "evil.com")]).await, StatusCode::FORBIDDEN);
        assert_eq!(post(f.app.clone(), &[("host", "127.0.0.1:7878"), ("origin", "http://evil.com")]).await, StatusCode::FORBIDDEN);
        assert_eq!(post(f.app.clone(), &[("host", "127.0.0.1:7878"), ("origin", "null")]).await, StatusCode::FORBIDDEN);
        assert_ne!(post(f.app.clone(), &[("host", "127.0.0.1:7878")]).await, StatusCode::FORBIDDEN);
    }
}
