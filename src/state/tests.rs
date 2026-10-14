use super::*;
use crate::finders::cleaners::Runner;
use std::fs;

/// A scanned temp tree:
///   cache/a.bin (40 KB)   – allowlisted folder
///   docs/b.bin  (20 KB)
///   node_modules/c.bin (10 KB)
pub(crate) struct Fixture {
    pub _tmp: tempfile::TempDir,
    pub base: PathBuf,
    pub app: Arc<Shared>,
    pub _rx: async_channel::Receiver<()>,
    pub _trash: tempfile::TempDir,
    pub _hist: tempfile::TempDir,
}

fn fake_trash(paths: &[PathBuf]) -> Vec<TrashResult> {
    paths
        .iter()
        .map(|p| TrashResult { path: p.clone(), error: fs::remove_file(p).or_else(|_| fs::remove_dir_all(p)).err().map(|e| e.to_string()) })
        .collect()
}

pub(crate) async fn fixture() -> Fixture {
    fixture_with(default_runner()).await
}

pub(crate) async fn fixture_with(runner: Runner) -> Fixture {
    let trash = tempfile::tempdir().unwrap();
    let hist = tempfile::tempdir().unwrap();
    fs::write(trash.path().join("junk.bin"), vec![0u8; 1000]).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let base = dunce::canonicalize(tmp.path()).unwrap();
    for (rel, len) in [("cache/a.bin", 40_000), ("docs/b.bin", 20_000), ("node_modules/c.bin", 10_000)] {
        fs::create_dir_all(base.join(rel).parent().unwrap()).unwrap();
        fs::write(base.join(rel), vec![1u8; len]).unwrap();
    }
    let mut settings = Settings::default();
    settings.allowlist.push(base.join("cache"));
    let deps = Deps {
        rules: Rules::parse(include_str!("../../rules/catalog.toml"), &base, "linux").unwrap(),
        trasher: fake_trash,
        home: PathBuf::from("/nonexistent-home"),
        protected: Vec::new(),
        runner,
        trash_dir: Some(trash.path().to_path_buf()),
        history_dir: Some(hist.path().to_path_buf()),
        app_dirs: vec![base.join("Applications")],
        library: base.join("Library"),
        downloads: base.join("Downloads"),
        screenshots: Some(base.join("Shots")),
        welcome_marker: Some(hist.path().join("welcome/welcome-seen")),
        ollama_dirs: vec![base.join("ollama")],
        hf_dirs: vec![base.join("hub")],
    };
    let (tx, rx) = async_channel::bounded(1);
    let app = Shared::build(settings, false, base.join("settings.toml"), tx, deps);
    app.start_scan(base.clone()).await.unwrap().unwrap();
    Fixture { _tmp: tmp, base, app, _rx: rx, _trash: trash, _hist: hist }
}

fn old_app(path: PathBuf, leftover: PathBuf) -> crate::finders::apps::App {
    crate::finders::apps::App { path, name: "Old".into(), id: Some("com.old".into()), last_used: 1, own: 700, leftovers: vec![(leftover, 30)], running: false, movable: true }
}

#[tokio::test]
async fn apps_load_when_cleanup_opens() {
    let f = fixture().await;
    assert!(matches!(f.app.core.lock().unwrap().apps, Apps::Idle));
    f.app.show(Screen::Cleanup);
    for _ in 0..100 {
        if !matches!(f.app.core.lock().unwrap().apps, Apps::Loading) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // The fixture's Applications folder doesn't exist: nothing unused, and no mdls call.
    assert!(matches!(&f.app.core.lock().unwrap().apps, Apps::Done(a) if a.is_empty()));
}

#[tokio::test]
async fn models_load_when_cleanup_opens() {
    let f = fixture().await;
    assert!(matches!(f.app.core.lock().unwrap().models, Models::Idle));
    f.app.show(Screen::Cleanup);
    for _ in 0..100 {
        if !matches!(f.app.core.lock().unwrap().models, Models::Loading) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(matches!(&f.app.core.lock().unwrap().models, Models::Done(m) if m.is_empty()));
}

#[tokio::test]
async fn trashed_models_leave_the_list_and_show_their_sizes() {
    let f = fixture().await;
    let hub = f.base.join("hub");
    let dir = hub.join("models--o--m");
    fs::create_dir_all(&dir).unwrap();
    let ol = f.base.join("ollama");
    let manifest = ol.join("manifests/registry.ollama.ai/library/a/1");
    let blob = ol.join("blobs/sha256-x");
    fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    fs::create_dir_all(blob.parent().unwrap()).unwrap();
    fs::write(&manifest, [0u8; 7]).unwrap();
    fs::write(&blob, [0u8; 300]).unwrap();
    let hf = crate::finders::models::Model { name: "o/m".into(), source: crate::finders::models::Source::HuggingFace, size: 4242, downloaded: None, paths: vec![dir.clone()], root: hub };
    let oll = crate::finders::models::Model { name: "a:1".into(), source: crate::finders::models::Source::Ollama, size: 300, downloaded: None, paths: vec![manifest.clone(), blob.clone()], root: ol };
    f.app.core.lock().unwrap().models = Models::Done(vec![hf, oll]);
    f.app.core.lock().unwrap().root = Some(f.base.join("cache")); // the models are outside the scanned folder
    let _rx = f.app.request_trash(vec![dir.clone(), manifest.clone(), blob.clone()], None, Origin::User);
    assert_eq!(f.app.core.lock().unwrap().pending.as_ref().unwrap().items, vec![(dir, Some(4242)), (manifest, Some(7)), (blob, Some(300))]);
    f.app.resolve_pending(true);
    assert!(matches!(&f.app.core.lock().unwrap().models, Models::Done(m) if m.is_empty()));
    assert_eq!(f.app.core.lock().unwrap().toasts.last(), Some(&(3, 4242 + 7 + 300)), "sizes are taken before the files move");
}

#[tokio::test]
async fn trashed_apps_leave_the_list_and_show_their_size() {
    let f = fixture().await;
    let (app, plist) = (f.base.join("docs/Old.app"), f.base.join("docs/com.old.plist"));
    fs::create_dir_all(&app).unwrap();
    fs::write(&plist, b"p").unwrap();
    f.app.core.lock().unwrap().apps = Apps::Done(vec![old_app(app.clone(), plist.clone()), old_app(PathBuf::from("/elsewhere/Other.app"), PathBuf::from("/elsewhere/x"))]);
    let outside = PathBuf::from("/elsewhere/Other.app");
    f.app.core.lock().unwrap().root = Some(f.base.join("cache")); // the app is outside the scanned folder
    let _rx = f.app.request_trash(vec![app.clone(), plist.clone()], None, Origin::User);
    assert_eq!(f.app.core.lock().unwrap().pending.as_ref().unwrap().items, vec![(app.clone(), Some(700)), (plist.clone(), Some(30))]);
    f.app.resolve_pending(true);
    let c = f.app.core.lock().unwrap();
    let Apps::Done(left) = &c.apps else { panic!("{:?}", c.apps) };
    assert_eq!(left.iter().map(|a| &a.path).collect::<Vec<_>>(), vec![&outside]);
}

#[tokio::test]
async fn a_path_is_asked_for_once_even_when_requested_twice() {
    let f = fixture().await;
    let (docs, b) = (f.base.join("docs"), f.base.join("docs/b.bin"));
    let _rx = f.app.request_trash(vec![b.clone(), docs.clone(), docs.clone()], None, Origin::User);
    let items: Vec<PathBuf> = f.app.core.lock().unwrap().pending.as_ref().unwrap().items.iter().map(|(p, _)| p.clone()).collect();
    assert_eq!(items, vec![docs], "the duplicate and the file inside it go with the folder");
}

#[tokio::test]
async fn old_files_load_when_cleanup_opens_and_leave_when_trashed() {
    let f = fixture().await;
    let old = f.base.join("Downloads/report.pdf");
    fs::create_dir_all(old.parent().unwrap()).unwrap();
    fs::write(&old, vec![0u8; 2000]).unwrap();
    let t = std::time::SystemTime::now() - std::time::Duration::from_secs(400 * 86_400);
    fs::File::options().write(true).open(&old).unwrap().set_times(fs::FileTimes::new().set_accessed(t).set_modified(t)).unwrap();
    f.app.show(Screen::Cleanup);
    for _ in 0..100 {
        if !matches!(f.app.core.lock().unwrap().old_files, OldFiles::Loading) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert!(matches!(&f.app.core.lock().unwrap().old_files, OldFiles::Done(l) if l.len() == 1 && l[0].path == old));
    let _rx = f.app.request_trash(vec![old.clone()], None, Origin::User);
    f.app.resolve_pending(true);
    assert!(matches!(&f.app.core.lock().unwrap().old_files, OldFiles::Done(l) if l.is_empty()));
}

#[tokio::test]
async fn welcome_shows_until_done() {
    let f = fixture().await;
    assert!(f.app.welcome_needed());
    f.app.welcome_done();
    assert!(!f.app.welcome_needed());
}

#[tokio::test]
async fn every_full_scan_adds_a_snapshot() {
    let f = fixture().await;
    assert_eq!(f.app.core.lock().unwrap().history.len(), 1);
    f.app.start_scan(f.base.clone()).await.unwrap().unwrap();
    let c = f.app.core.lock().unwrap();
    assert_eq!(c.history.len(), 2);
    assert_eq!(c.history[1].total, c.tree.as_ref().unwrap().size);
    assert_eq!(crate::scan::snapshots::load(f._hist.path(), &f.base).len(), 2);
}

async fn dupes_done(app: &Arc<Shared>) -> Vec<crate::finders::dupes::Group> {
    for _ in 0..500 {
        if let Dupes::Done(g) = &app.core.lock().unwrap().dupes {
            return g.clone();
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("duplicate search did not finish");
}

#[tokio::test]
async fn duplicates_are_found_and_trashed_copies_leave_their_group() {
    let f = fixture().await;
    for n in ["d1.bin", "docs/d2.bin"] {
        fs::write(f.base.join(n), vec![6u8; 1_500_000]).unwrap();
    }
    f.app.start_scan(f.base.clone()).await.unwrap().unwrap();
    f.app.start_dupes().unwrap();
    let g = dupes_done(&f.app).await;
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].copies.len(), 2);
    let (_, rx) = f.app.request_trash(vec![f.base.join("d1.bin")], None, Origin::User);
    assert!(rx.is_some());
    f.app.resolve_pending(true);
    assert!(matches!(&f.app.core.lock().unwrap().dupes, Dupes::Done(g) if g.is_empty()));
}

#[tokio::test]
async fn a_new_scan_discards_duplicate_results() {
    let f = fixture().await;
    f.app.start_dupes().unwrap();
    dupes_done(&f.app).await;
    f.app.start_scan(f.base.clone()).await.unwrap().unwrap();
    assert!(matches!(f.app.core.lock().unwrap().dupes, Dupes::Idle));
}

#[tokio::test]
async fn no_duplicate_search_while_a_scan_runs() {
    let f = fixture().await;
    f.app.core.lock().unwrap().scanning = Some(f.base.clone());
    assert!(f.app.start_dupes().is_err());
    assert!(matches!(f.app.core.lock().unwrap().dupes, Dupes::Idle));
}

#[tokio::test]
async fn cancel_dupes_goes_back_to_idle() {
    let f = fixture().await;
    f.app.start_dupes().unwrap();
    f.app.cancel_dupes();
    assert!(matches!(f.app.core.lock().unwrap().dupes, Dupes::Idle));
}

#[tokio::test]
async fn scan_fills_tree_findings_and_recent() {
    let f = fixture().await;
    let c = f.app.core.lock().unwrap();
    assert_eq!(c.home_state(), HomeState::Complete);
    assert_eq!(c.screen, Screen::Home);
    let s = c.stats.unwrap();
    assert_eq!(s.files, 3);
    assert_eq!(s.bytes, c.tree.as_ref().unwrap().size);
    assert!(s.largest_file >= 40_000);
    assert_eq!(c.tree.as_ref().unwrap().children.len(), 3);
    assert_eq!(c.findings[0].rule_id, "node_modules");
    assert_eq!(c.settings.recent[0].path, f.base);
    assert!(f.base.join("settings.toml").exists());
    assert_eq!(c.banner(), Banner::Found(c.findings[0].size));
}

#[tokio::test]
async fn agent_trash_in_allowlist_is_immediate() {
    let f = fixture().await;
    let (report, rx) = f.app.request_trash(vec![f.base.join("cache/a.bin")], Some("cache".into()), Origin::Agent);
    assert_eq!(report, vec![(f.base.join("cache/a.bin"), ItemStatus::Trashed)]);
    assert!(rx.is_none());
    assert!(!f.base.join("cache/a.bin").exists());
    let c = f.app.core.lock().unwrap();
    assert_eq!(c.tree.as_ref().unwrap().find(Path::new("cache")).unwrap().size, 0);
}

#[tokio::test]
async fn agent_trash_outside_allowlist_waits_for_user() {
    let f = fixture().await;
    let target = f.base.join("docs/b.bin");
    let (report, rx) = f.app.request_trash(vec![target.clone()], Some("old".into()), Origin::Agent);
    assert_eq!(report, vec![(target.clone(), ItemStatus::Pending)]);
    assert!(target.exists());
    assert_eq!(f.app.core.lock().unwrap().banner(), Banner::Confirm);
    assert_eq!(f.app.resolve_pending(true), vec![(target.clone(), ItemStatus::Trashed)]);
    assert_eq!(rx.unwrap().await.unwrap(), vec![(target.clone(), ItemStatus::Trashed)]);
    assert!(!target.exists());
    assert!(f.app.core.lock().unwrap().pending.is_none());
}

#[tokio::test]
async fn declined_and_second_request_while_pending() {
    let f = fixture().await;
    let (_, rx) = f.app.request_trash(vec![f.base.join("docs/b.bin")], None, Origin::Agent);
    let (second, rx2) = f.app.request_trash(vec![f.base.join("node_modules")], None, Origin::Agent);
    assert!(matches!(second[0].1, ItemStatus::Refused(_)));
    assert!(rx2.is_none());
    assert_eq!(f.app.core.lock().unwrap().pending.as_ref().unwrap().items.len(), 1);
    f.app.resolve_pending(false);
    assert_eq!(rx.unwrap().await.unwrap()[0].1, ItemStatus::Declined);
    assert!(f.base.join("docs/b.bin").exists());
}

#[tokio::test]
async fn path_tricks_never_skip_confirmation() {
    let f = fixture().await;
    let sneaky = f.base.join("cache/../docs/b.bin");
    let (report, rx) = f.app.request_trash(vec![sneaky, PathBuf::from("relative/x")], None, Origin::Agent);
    assert!(report.contains(&(f.base.join("docs/b.bin"), ItemStatus::Pending)));
    assert!(report.iter().any(|(p, s)| p == Path::new("relative/x") && matches!(s, ItemStatus::Refused(_))));
    assert!(rx.is_some());
    assert!(f.base.join("docs/b.bin").exists());
}

#[tokio::test]
async fn scan_root_cannot_be_trashed() {
    let f = fixture().await;
    let (report, _) = f.app.request_trash(vec![f.base.clone()], None, Origin::User);
    assert!(matches!(report[0].1, ItemStatus::Refused(_)));
    assert_eq!(f.app.core.lock().unwrap().refused, vec![f.base.clone()]);
    // Agent refusals are reported to the agent only, not to the user's banner.
    let (_, _) = f.app.request_trash(vec![f.base.clone()], None, Origin::Agent);
    assert!(f.app.core.lock().unwrap().refused.is_empty());
}

#[tokio::test]
async fn pending_size_is_none_outside_the_scan() {
    let f = fixture().await;
    let other = tempfile::tempdir().unwrap();
    let outside = dunce::canonicalize(other.path()).unwrap().join("x.bin");
    std::fs::write(&outside, b"hi").unwrap();
    let inside = f.base.join("docs/b.bin");
    let (_, rx) = f.app.request_trash(vec![outside.clone(), inside.clone()], Some("r".into()), Origin::Agent);
    assert!(rx.is_some());
    let c = f.app.core.lock().unwrap();
    let items = &c.pending.as_ref().unwrap().items;
    assert_eq!(items.iter().find(|(p, _)| *p == outside).unwrap().1, None);
    assert!(items.iter().find(|(p, _)| *p == inside).unwrap().1.is_some_and(|s| s >= 20_000));
}

#[tokio::test]
async fn scanning_a_missing_folder_reports_it_as_interrupted() {
    let f = fixture().await;
    assert!(f.app.start_scan(f.base.join("gone")).await.unwrap().is_err());
    let c = f.app.core.lock().unwrap();
    assert_eq!(c.home_state(), HomeState::Interrupted);
    assert!(c.interrupted.as_ref().unwrap().error.is_some());
    assert!(c.tree.is_some()); // previous results kept
}

#[tokio::test]
async fn trash_resets_zoom_and_shrinks_findings() {
    let f = fixture().await;
    f.app.navigate(&f.base.join("node_modules/c.bin")).unwrap();
    {
        let c = f.app.core.lock().unwrap();
        assert_eq!(c.zoom, PathBuf::from("node_modules"));
        assert_eq!(c.selected, Some(PathBuf::from("node_modules/c.bin")));
    }
    let (_, _rx) = f.app.request_trash(vec![f.base.join("node_modules/c.bin")], None, Origin::User);
    f.app.resolve_pending(true);
    let c = f.app.core.lock().unwrap();
    assert_eq!(c.selected, None);
    assert_eq!(c.zoom, PathBuf::from("node_modules"));
    assert!(c.findings.iter().all(|x| x.size > 0));
    assert!(c.findings.iter().all(|x| x.rule_id != "node_modules"));
}

#[tokio::test]
async fn trash_during_scan_is_not_resurrected() {
    let f = fixture().await;
    let target = f.base.join("cache/a.bin");
    let rx = f.app.start_scan(f.base.clone());
    let (_, _rx) = f.app.request_trash(vec![target.clone()], None, Origin::Agent);
    f.app.resolve_pending(true);
    let _ = rx.await;
    let c = f.app.core.lock().unwrap();
    assert!(c.tree.as_ref().unwrap().find(Path::new("cache/a.bin")).is_none());
    assert!(c.findings.iter().all(|x| x.path != target));
}

#[tokio::test]
async fn refresh_picks_up_changes_and_keeps_zoom() {
    let f = fixture().await;
    f.app.navigate(&f.base.join("docs")).unwrap();
    fs::write(f.base.join("docs/new.bin"), vec![1u8; 50_000]).unwrap();
    f.app.refresh(None).await.unwrap().unwrap();
    let c = f.app.core.lock().unwrap();
    assert_eq!(c.zoom, PathBuf::from("docs"));
    assert!(c.tree.as_ref().unwrap().find(Path::new("docs/new.bin")).is_some());
    assert_eq!(&*c.tree.as_ref().unwrap().children[0].name, "docs");
}

#[tokio::test]
async fn refresh_of_vanished_folder_removes_it() {
    let f = fixture().await;
    fs::remove_dir_all(f.base.join("docs")).unwrap();
    f.app.refresh(Some(f.base.join("docs"))).await.unwrap().unwrap();
    assert!(f.app.core.lock().unwrap().tree.as_ref().unwrap().find(Path::new("docs")).is_none());
}

#[tokio::test]
async fn refresh_superseded_by_new_scan_is_discarded() {
    let f = fixture().await;
    let r1 = f.app.refresh(Some(f.base.join("docs")));
    let r2 = f.app.start_scan(f.base.clone());
    let _ = r1.await;
    r2.await.unwrap().unwrap();
    let c = f.app.core.lock().unwrap();
    // 3 fixture folders + settings.toml written by the first scan
    assert_eq!(c.tree.as_ref().unwrap().children.len(), 4);
    assert_eq!(c.home_state(), HomeState::Complete);
    assert!(!c.busy);
}

#[tokio::test]
async fn settings_save_failure_is_reported() {
    let f = fixture().await;
    let blocker = f.base.join("blocker");
    fs::write(&blocker, "x").unwrap();
    let (tx, _rx) = async_channel::bounded(1);
    let deps = Deps { rules: Rules::builtin(), trasher: fake_trash, home: PathBuf::from("/nonexistent-home"), protected: Vec::new(), runner: default_runner(), trash_dir: None, history_dir: None, app_dirs: Vec::new(), library: PathBuf::from("/nonexistent-home/Library"), downloads: PathBuf::from("/nonexistent-home/Downloads"), screenshots: Some(PathBuf::from("/nonexistent-home/Shots")), welcome_marker: None, ollama_dirs: Vec::new(), hf_dirs: Vec::new() };
    let app = Shared::build(Settings::default(), false, blocker.join("sub/settings.toml"), tx, deps);
    app.update_settings(|s| s.mcp_port = 1);
    assert!(app.core.lock().unwrap().settings_error.is_some());
}

#[tokio::test]
async fn refresh_rejects_dotdot() {
    let f = fixture().await;
    assert!(f.app.refresh(Some(f.base.join("docs/../.."))).await.unwrap().is_err());
}

#[tokio::test]
async fn navigate_outside_scan_is_an_error() {
    let f = fixture().await;
    assert!(f.app.navigate(Path::new("/elsewhere")).is_err());
    assert!(f.app.select(&f.base.join("nope")).is_err());
}

#[tokio::test]
async fn cancel_scan_keeps_partial_counts_and_old_results() {
    let f = fixture().await;
    {
        let mut c = f.app.core.lock().unwrap();
        c.scanning = Some(f.base.join("docs"));
        c.busy = true;
        c.progress.files.store(7, Relaxed);
        c.progress.bytes.store(700, Relaxed);
    }
    f.app.cancel_scan();
    {
        let c = f.app.core.lock().unwrap();
        assert_eq!(c.home_state(), HomeState::Interrupted);
        assert_eq!(c.interrupted, Some(Interrupted { root: f.base.join("docs"), files: 7, bytes: 700, error: None }));
        assert!(c.tree.is_some() && !c.busy && c.scanning.is_none());
    }
    f.app.dismiss_interrupted();
    assert_eq!(f.app.core.lock().unwrap().home_state(), HomeState::Complete);
}

#[tokio::test]
async fn new_scan_keeps_previous_results_until_done() {
    let f = fixture().await;
    let rx = f.app.start_scan(f.base.join("docs"));
    assert!(f.app.core.lock().unwrap().tree.is_some());
    rx.await.unwrap().unwrap();
    let c = f.app.core.lock().unwrap();
    assert_eq!(c.root.as_deref(), Some(f.base.join("docs").as_path()));
    assert_eq!(c.home_state(), HomeState::Complete);
}

#[tokio::test]
async fn show_requires_a_scan() {
    let f = fixture().await;
    f.app.show(Screen::Cleanup);
    assert_eq!(f.app.core.lock().unwrap().screen, Screen::Cleanup);
    f.app.core.lock().unwrap().tree = None;
    f.app.show(Screen::DiskMap);
    assert_eq!(f.app.core.lock().unwrap().screen, Screen::Home);
    f.app.show(Screen::Settings);
    assert_eq!(f.app.core.lock().unwrap().screen, Screen::Settings);
    // System tools don't need a scan, so Cleanup opens without one.
    f.app.show(Screen::Cleanup);
    assert_eq!(f.app.core.lock().unwrap().screen, Screen::Cleanup);
}

#[tokio::test]
async fn system_tools_shortcut_puts_them_first_until_the_next_screen_change() {
    let f = fixture().await;
    f.app.show_system_tools();
    {
        let c = f.app.core.lock().unwrap();
        assert_eq!(c.screen, Screen::Cleanup);
        assert!(c.system_first);
    }
    f.app.show(Screen::Home);
    assert!(!f.app.core.lock().unwrap().system_first);
}

#[tokio::test]
async fn trash_pushes_a_toast_and_updates_stats() {
    let f = fixture().await;
    let before = f.app.core.lock().unwrap().stats.unwrap();
    f.app.request_trash(vec![f.base.join("cache/a.bin")], None, Origin::Agent);
    let c = f.app.core.lock().unwrap();
    assert_eq!(c.toasts.len(), 1);
    assert_eq!(c.toasts[0].0, 1);
    assert!(c.toasts[0].1 >= 40_000);
    let after = c.stats.unwrap();
    assert!(after.bytes < before.bytes);
    assert!(after.largest_file < before.largest_file);
}

#[tokio::test]
async fn navigate_opens_the_disk_map() {
    let f = fixture().await;
    f.app.navigate(&f.base.join("docs")).unwrap();
    assert_eq!(f.app.core.lock().unwrap().screen, Screen::DiskMap);
}

#[tokio::test]
async fn failed_scan_request_does_not_outlive_a_successful_scan() {
    let f = fixture().await;
    let rx = f.app.start_scan(f.base.join("docs"));
    let _ = f.app.start_scan(f.base.join("missing")).await;
    rx.await.unwrap().unwrap();
    assert_eq!(f.app.core.lock().unwrap().home_state(), HomeState::Complete);
}

#[tokio::test]
async fn cancel_discards_the_running_scan_result() {
    let f = fixture().await;
    let rx = f.app.start_scan(f.base.join("docs"));
    f.app.cancel_scan();
    let _ = rx.await;
    let c = f.app.core.lock().unwrap();
    assert!(c.tree.is_some());
    if c.interrupted.is_some() {
        assert_eq!(c.root.as_deref(), Some(f.base.as_path()));
    }
}

use crate::finders::cleaners::CmdOutput;
use std::sync::atomic::{AtomicBool, AtomicUsize};

/// brew: 1 MiB to free (0 after a run); docker/xcrun/git not installed; tmutil: no snapshots.
pub(crate) fn default_runner() -> Runner {
    let cleaned = Arc::new(AtomicBool::new(false));
    Arc::new(move |c| match (c.program, c.args.first().map(String::as_str)) {
        ("brew", _) if c.args.iter().any(|a| a == "--dry-run") => CmdOutput::Done {
            ok: true,
            stdout: if cleaned.load(Relaxed) { String::new() } else { "==> This operation would free approximately 1MB of disk space.".into() },
            stderr: String::new(),
        },
        ("brew", Some("cleanup")) => {
            cleaned.store(true, Relaxed);
            CmdOutput::Done { ok: true, stdout: String::new(), stderr: String::new() }
        }
        ("tmutil", _) => CmdOutput::Done { ok: true, stdout: "Snapshots for disk /:\n".into(), stderr: String::new() },
        ("xcode-select", _) => CmdOutput::Done { ok: true, stdout: "/Library/Developer/CommandLineTools\n".into(), stderr: String::new() },
        _ => CmdOutput::NotFound,
    })
}

async fn settled(app: &Arc<Shared>) {
    for _ in 0..200 {
        if !app.core.lock().unwrap().system.values().any(|i| matches!(i.status, SystemStatus::Checking | SystemStatus::Running)) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("system checks did not settle");
}

#[tokio::test]
async fn opening_cleanup_checks_system_tools() {
    let f = fixture().await;
    f.app.show(Screen::Cleanup);
    settled(&f.app).await;
    let c = f.app.core.lock().unwrap();
    assert_eq!(c.system["homebrew"].status, SystemStatus::Ready { estimate: Estimate::Bytes(1_048_576), detail: vec![] });
    assert_eq!(c.system["docker"].status, SystemStatus::Unavailable { reason: "not_installed".into() });
    assert!(matches!(c.system["empty_trash"].status, SystemStatus::Ready { estimate: Estimate::Bytes(n), .. } if n >= 1000));
    assert_eq!(c.system.contains_key("tm_snapshots"), cfg!(target_os = "macos"));
}

#[tokio::test]
async fn recheck_tool_checks_only_that_tool() {
    let f = fixture().await;
    f.app.recheck_tool(Cleaner::EmptyTrash);
    settled(&f.app).await;
    let c = f.app.core.lock().unwrap();
    assert!(matches!(c.system["empty_trash"].status, SystemStatus::Ready { estimate: Estimate::Bytes(n), .. } if n >= 1000));
    assert!(!c.system.contains_key("homebrew"));
}

#[tokio::test]
async fn opening_free_up_checks_system_tools() {
    let f = fixture().await;
    f.app.show(Screen::FreeUp);
    settled(&f.app).await;
    let c = f.app.core.lock().unwrap();
    assert_eq!(c.screen, Screen::FreeUp);
    assert_eq!(c.system["homebrew"].status, SystemStatus::Ready { estimate: Estimate::Bytes(1_048_576), detail: vec![] });
}

#[tokio::test]
async fn accepted_cleanup_runs_rechecks_and_toasts() {
    let f = fixture().await;
    f.app.check_system();
    settled(&f.app).await;
    let rx = f.app.request_system_cleanup("homebrew", Some("old bottles".into()), Origin::Agent).unwrap();
    {
        let c = f.app.core.lock().unwrap();
        let p = c.pending.as_ref().unwrap();
        assert_eq!(p.system[0].cleaner, Cleaner::Homebrew);
        assert!(p.items.is_empty());
    }
    f.app.resolve_pending(true);
    assert_eq!(rx.await.unwrap(), SystemOutcome::Done { freed: Some(1_048_576) });
    let c = f.app.core.lock().unwrap();
    assert_eq!(c.system["homebrew"].status, SystemStatus::Ready { estimate: Estimate::Bytes(0), detail: vec![] });
    assert_eq!(c.system_toasts, vec![(Cleaner::Homebrew, None, Some(1_048_576))]);
}

#[tokio::test]
async fn declined_cleanup_never_runs() {
    let runs = Arc::new(AtomicUsize::new(0));
    let count = runs.clone();
    let inner = default_runner();
    let f = fixture_with(Arc::new(move |c| {
        if c.args.first().is_some_and(|a| a == "cleanup") && !c.args.iter().any(|a| a == "--dry-run") {
            count.fetch_add(1, Relaxed);
        }
        inner(c)
    }))
    .await;
    f.app.check_system();
    settled(&f.app).await;
    let rx = f.app.request_system_cleanup("homebrew", None, Origin::User).unwrap();
    f.app.resolve_pending(false);
    assert_eq!(rx.await.unwrap(), SystemOutcome::Declined);
    assert_eq!(runs.load(Relaxed), 0);
}

#[tokio::test]
async fn system_requests_are_refused_when_not_ready_or_busy() {
    let f = fixture().await;
    assert!(f.app.request_system_cleanup("homebrew", None, Origin::Agent).is_err()); // not checked yet
    f.app.check_system();
    settled(&f.app).await;
    assert!(f.app.request_system_cleanup("docker", None, Origin::Agent).is_err()); // unavailable
    assert!(f.app.request_system_cleanup("nope", None, Origin::Agent).is_err()); // unknown
    if cfg!(target_os = "macos") {
        assert!(f.app.request_system_cleanup("tm_snapshots", None, Origin::Agent).is_err()); // nothing to clean
    }
    let _rx = f.app.request_trash(vec![f.base.join("docs/b.bin")], None, Origin::Agent);
    assert!(f.app.request_system_cleanup("homebrew", None, Origin::Agent).is_err()); // a confirmation is open
}

#[tokio::test]
async fn agent_input_is_refused_at_the_delete_gate_and_in_settings() {
    let f = fixture().await;
    let ui = f.app.inputs.clone();
    tokio::spawn(async move {
        while let Ok((_, reply)) = ui.recv().await {
            let _ = reply.send(Ok(()));
        }
    });
    assert_eq!(f.app.send_input(Input::Click { x: 1.0, y: 1.0 }).await, Ok(()));
    let _rx = f.app.request_trash(vec![f.base.join("docs/b.bin")], None, Origin::Agent);
    assert!(f.app.send_input(Input::Key("enter".into())).await.is_err(), "the confirmation is the user's");
    f.app.resolve_pending(false);
    assert_eq!(f.app.send_input(Input::Text("x".into())).await, Ok(()));
    f.app.show(Screen::Settings);
    assert!(f.app.send_input(Input::Click { x: 1.0, y: 1.0 }).await.is_err(), "settings are the user's");
    assert!(f.app.input_allowed().is_err());
}

#[tokio::test]
async fn decline_only_hits_its_own_request() {
    let f = fixture().await;
    f.app.check_system();
    settled(&f.app).await;
    let _trash = f.app.request_trash(vec![f.base.join("docs/b.bin")], None, Origin::Agent);
    assert!(!f.app.decline_system_if_pending("homebrew"));
    assert!(f.app.core.lock().unwrap().pending.is_some());
    f.app.resolve_pending(false);
    let rx = f.app.request_system_cleanup("homebrew", None, Origin::Agent).unwrap();
    assert!(f.app.decline_system_if_pending("homebrew"));
    assert_eq!(rx.await.unwrap(), SystemOutcome::Declined);
    assert!(f.app.core.lock().unwrap().pending.is_none());
}

#[tokio::test]
async fn recheck_during_run_keeps_running() {
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let release_rx = Arc::new(Mutex::new(release_rx));
    let inner = default_runner();
    let f = fixture_with(Arc::new(move |c| {
        if c.program == "brew" && !c.args.iter().any(|a| a == "--dry-run") {
            let _ = release_rx.lock().unwrap().recv();
        }
        inner(c)
    }))
    .await;
    f.app.check_system();
    settled_checks(&f.app).await;
    let rx = f.app.request_system_cleanup("homebrew", None, Origin::User).unwrap();
    f.app.resolve_pending(true);
    assert_eq!(f.app.core.lock().unwrap().system["homebrew"].status, SystemStatus::Running);
    f.app.check_system();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(f.app.core.lock().unwrap().system["homebrew"].status, SystemStatus::Running);
    assert!(f.app.request_system_cleanup("homebrew", None, Origin::User).is_err());
    release_tx.send(()).unwrap();
    assert!(matches!(rx.await.unwrap(), SystemOutcome::Done { .. }));
}

async fn settled_checks(app: &Arc<Shared>) {
    for _ in 0..200 {
        if !app.core.lock().unwrap().system.values().any(|i| matches!(i.status, SystemStatus::Checking)) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("checks did not settle");
}

#[tokio::test]
async fn failed_run_reports_error() {
    let inner = default_runner();
    let f = fixture_with(Arc::new(move |c| {
        if c.program == "brew" && !c.args.iter().any(|a| a == "--dry-run") {
            CmdOutput::Done { ok: false, stdout: String::new(), stderr: "Error: permission denied\n".into() }
        } else {
            inner(c)
        }
    }))
    .await;
    f.app.check_system();
    settled(&f.app).await;
    let rx = f.app.request_system_cleanup("homebrew", None, Origin::User).unwrap();
    f.app.resolve_pending(true);
    assert_eq!(rx.await.unwrap(), SystemOutcome::Failed { error: "Error: permission denied".into() });
    assert_eq!(f.app.core.lock().unwrap().system["homebrew"].status, SystemStatus::Failed { error: "Error: permission denied".into() });
}

#[test]
fn worktrees_outside_the_scan_are_counted() {
    assert_eq!(worktree_estimate(0, 2), Estimate::Count(2));
    assert_eq!(worktree_estimate(5000, 2), Estimate::Bytes(5000));
    assert_eq!(worktree_estimate(0, 0), Estimate::Bytes(0));
}

/// Fake git: `<base>/repo` has a merged, clean worktree `<base>/wt`; `worktree remove` deletes it.
fn git_runner(base: Arc<std::sync::Mutex<PathBuf>>) -> Runner {
    let inner = default_runner();
    Arc::new(move |c| {
        if c.program != "git" {
            return inner(c);
        }
        let ok = |out: String| CmdOutput::Done { ok: true, stdout: out, stderr: String::new() };
        let base = base.lock().unwrap().clone();
        match c.args[4].as_str() {
            "symbolic-ref" => ok("origin/main\n".into()),
            "for-each-ref" => ok("main\nwt\n".into()),
            "worktree" if c.args[5] == "list" => ok(format!("worktree {}\nbranch refs/heads/main\n\nworktree {}\nbranch refs/heads/wt\n", base.join("repo").display(), base.join("wt").display())),
            "worktree" if c.args[5] == "remove" => {
                fs::remove_dir_all(&c.args[6]).unwrap();
                ok(String::new())
            }
            _ => ok(String::new()), // status (clean), worktree prune
        }
    })
}

/// Fake git with merged branches old-a, old-b, old-c in every repo; records `branch -d` calls.
/// (repo, branches) per `git branch -d` call.
type Deleted = Arc<std::sync::Mutex<Vec<(String, Vec<String>)>>>;

fn branch_runner(deleted: Deleted) -> Runner {
    let inner = default_runner();
    Arc::new(move |c| {
        if c.program != "git" {
            return inner(c);
        }
        let ok = |out: &str| CmdOutput::Done { ok: true, stdout: out.to_string(), stderr: String::new() };
        match c.args[4].as_str() {
            "symbolic-ref" => ok("origin/main\n"),
            "for-each-ref" => ok("main\nold-a\nold-b\nold-c\n"),
            "worktree" => ok(&format!("worktree {}\nbranch refs/heads/main\n", c.args[3])),
            "branch" => {
                deleted.lock().unwrap().push((c.args[3].clone(), c.args[6..].to_vec()));
                ok("")
            }
            _ => ok(""),
        }
    })
}

#[tokio::test]
async fn picked_branches_across_repos_run_in_one_confirmation_and_nothing_else() {
    let deleted = Arc::new(std::sync::Mutex::new(Vec::new()));
    let f = fixture_with(branch_runner(deleted.clone())).await;
    for r in ["one", "two"] {
        fs::create_dir_all(f.base.join(r).join(".git")).unwrap();
    }
    f.app.start_scan(f.base.clone()).await.unwrap().unwrap();
    f.app.check_system();
    settled(&f.app).await;
    let id = |r: &str| cleaners::item_id(Cleaner::GitBranches, Some(&f.base.join(r)));
    // Something the last check did not offer is refused outright.
    assert!(f.app.request_system_batch(vec![(id("one"), Some(vec!["main".into()]))], None, Origin::User).is_err());
    assert!(f.app.request_system_batch(vec![(id("one"), Some(vec!["old-a".into(), "evil".into()]))], None, Origin::User).is_err());
    assert!(f.app.core.lock().unwrap().pending.is_none());
    let rx = f.app.request_system_batch(vec![(id("one"), Some(vec!["old-a".into(), "old-c".into()])), (id("two"), Some(vec!["old-b".into()]))], None, Origin::User).unwrap();
    assert_eq!(f.app.core.lock().unwrap().pending.as_ref().unwrap().system.len(), 2);
    f.app.resolve_pending(true);
    assert!(matches!(rx.await.unwrap(), SystemOutcome::Done { .. }));
    let mut calls = deleted.lock().unwrap().clone();
    calls.sort();
    let repo = |r: &str| f.base.join(r).display().to_string();
    assert_eq!(calls, vec![(repo("one"), vec!["old-a".into(), "old-c".into()]), (repo("two"), vec!["old-b".into()])]);
}

#[tokio::test]
async fn rescan_rechecks_git_and_removed_worktrees_leave_the_tree() {
    let base_cell = Arc::new(std::sync::Mutex::new(PathBuf::new()));
    let f = fixture_with(git_runner(base_cell.clone())).await;
    *base_cell.lock().unwrap() = f.base.clone();
    f.app.check_system();
    settled(&f.app).await;
    assert!(f.app.core.lock().unwrap().system.values().all(|i| i.repo.is_none())); // no repo in the old tree
    fs::create_dir_all(f.base.join("repo/.git")).unwrap();
    fs::create_dir_all(f.base.join("wt")).unwrap();
    fs::write(f.base.join("wt/big.bin"), vec![1u8; 5000]).unwrap();
    f.app.start_scan(f.base.clone()).await.unwrap().unwrap();
    settled(&f.app).await;
    let id = cleaners::item_id(Cleaner::GitWorktrees, Some(&f.base.join("repo")));
    {
        let c = f.app.core.lock().unwrap();
        assert!(matches!(&c.system[&id].status, SystemStatus::Ready { estimate: Estimate::Bytes(n), detail } if *n >= 5000 && detail == &vec![f.base.join("wt").display().to_string()]));
    }
    let rx = f.app.request_system_cleanup(&id, None, Origin::User).unwrap();
    f.app.resolve_pending(true);
    assert!(matches!(rx.await.unwrap(), SystemOutcome::Done { .. }));
    assert!(!f.base.join("wt").exists());
    let c = f.app.core.lock().unwrap();
    assert!(c.tree.as_ref().unwrap().find(Path::new("wt")).is_none());
}
