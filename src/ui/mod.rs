use crate::state::{Core, Screen, Shared};
use crate::{format, platform, settings};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::InputState;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::sidebar::{Sidebar, SidebarHeader, SidebarMenu, SidebarMenuItem};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rust_i18n::t;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;
use std::time::Duration;

mod cleanup;
mod dialog;
mod disk_map;
mod home;
mod hover_tip;
mod mascot;
mod settings_view;
mod tree;

actions!(fastscancleaner, [Refresh, Back, Forward, TrashMarked, ClearMarked]);

pub fn apply_language(setting: Option<&str>) {
    rust_i18n::set_locale(format::pick_locale(setting, sys_locale::get_locale()));
}

pub fn apply_theme(theme: &settings::Theme, window: &mut Window, cx: &mut App) {
    let dark = match theme {
        settings::Theme::Light => false,
        settings::Theme::Dark => true,
        settings::Theme::System => matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark),
    };
    Theme::change(if dark { ThemeMode::Dark } else { ThemeMode::Light }, Some(window), cx);
    brand_colors(dark, cx);
}

/// Blue accent instead of the kit's black/white default (matches the mockup).
fn brand_colors(dark: bool, cx: &mut App) {
    let c = |hex: u32| -> Hsla { rgb(hex).into() };
    let (base, hover, active, soft, soft_fg) = if dark {
        (c(0x3B82F6), c(0x60A5FA), c(0x2563EB), c(0x3B82F6).opacity(0.2), c(0x93C5FD))
    } else {
        (c(0x2563EB), c(0x1D4ED8), c(0x1E40AF), c(0xDBEAFE), c(0x1D4ED8))
    };
    let t = Theme::global_mut(cx);
    (t.primary, t.primary_hover, t.primary_active, t.primary_foreground) = (base, hover, active, c(0xFFFFFF));
    (t.button_primary, t.button_primary_hover, t.button_primary_active, t.button_primary_foreground) = (base, hover, active, c(0xFFFFFF));
    (t.info, t.info_hover, t.info_active, t.info_foreground) = (base, hover, active, c(0xFFFFFF));
    (t.link, t.link_hover, t.link_active) = (base, hover, active);
    (t.progress_bar, t.ring, t.slider_bar, t.switch) = (base, base, base, base);
    (t.sidebar_primary, t.sidebar_primary_foreground) = (base, c(0xFFFFFF));
    (t.sidebar_accent, t.sidebar_accent_foreground) = (soft, soft_fg);
    // The kit pairs its bright dark-mode green/yellow with white text; pick dark or white by lightness.
    let on = |bg: Hsla| if bg.l > 0.55 { c(0x0A0A0A) } else { c(0xFFFFFF) };
    (t.success_foreground, t.warning_foreground, t.danger_foreground) = (on(t.success), on(t.warning), on(t.danger));
    // Buttons read a cached copy of the colours; rebuild it.
    t.tokens = t.colors.into();
    // Always show scrollbars, so a long list visibly continues (the kit's default fades them out).
    // Goes through the setter: it also updates the layer that draws the scrollbars.
    Theme::set_scrollbar_mode(gpui_kit::component::scroll::ScrollbarMode::Always, cx);
}

fn loc() -> String {
    rust_i18n::locale().to_string()
}

/// A white (or dark-theme) rounded card.
pub(crate) fn card(border: Hsla, bg: Hsla) -> Div {
    div().v_flex().gap_3().p_5().rounded_xl().border_1().border_color(border).bg(bg).shadow_sm()
}

/// Page heading with an optional leading icon.
pub(crate) fn heading(icon: Option<AnyElement>, title: String, subtitle: String, muted: Hsla) -> Div {
    div()
        .h_flex()
        .gap_3()
        .items_center()
        .children(icon)
        .child(div().v_flex().gap_1().child(div().text_2xl().font_weight(FontWeight::BOLD).child(title)).child(div().text_color(muted).child(subtitle)))
}

pub fn run(app: Arc<Shared>, rx: async_channel::Receiver<()>) {
    gpui_kit::application().with_assets(gpui_kit::assets::AllAssets).run(move |cx| {
        platform::set_app_icon();
        gpui_kit::init(cx);
        cx.bind_keys([
            KeyBinding::new("cmd-r", Refresh, None),
            KeyBinding::new("ctrl-r", Refresh, None),
            KeyBinding::new("cmd-[", Back, None),
            KeyBinding::new("ctrl-[", Back, None),
            KeyBinding::new("cmd-]", Forward, None),
            KeyBinding::new("ctrl-]", Forward, None),
            // Only while the treemap has focus, so text fields keep their Backspace.
            KeyBinding::new("backspace", TrashMarked, Some("DiskMap")),
            KeyBinding::new("delete", TrashMarked, Some("DiskMap")),
            KeyBinding::new("escape", ClearMarked, Some("DiskMap")),
        ]);
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.spawn(async move |cx| {
            let options = WindowOptions {
                // Not drawn (custom title bar), but shown in the Window menu, Mission Control and the taskbar.
                titlebar: Some(TitlebarOptions { title: Some(t!("window_title").to_string().into()), ..TitleBar::title_bar_options() }),
                window_bounds: Some(WindowBounds::Windowed(Bounds { origin: point(px(80.), px(60.)), size: size(px(1280.), px(840.)) })),
                window_min_size: Some(size(px(960.), px(640.))),
                ..Default::default()
            };
            cx.open_window(options, |window, cx| {
                let tip = cx.new(|_| hover_tip::HoverTip::default());
                let view = cx.new(|cx| AppView::new(app.clone(), rx, tip.clone(), window, cx));
                let shell = cx.new(|_| Shell { app, view, mascot: None, tip });
                cx.new(|cx| Root::new(shell, window, cx))
            })
            .expect("failed to open window");
        })
        .detach();
        cx.activate(true);
    });
}

pub struct AppView {
    pub(crate) app: Arc<Shared>,
    focus: FocusHandle,
    /// Disk Map shows the file table instead of the treemap.
    pub(crate) show_list: bool,
    pub(crate) details_open: bool,
    /// Treemap area size from the last frame; the layout uses it.
    pub(crate) map_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Same for the small map on the Home screen.
    pub(crate) mini_bounds: Rc<Cell<Bounds<Pixels>>>,
    pub(crate) volumes: Vec<platform::Volume>,
    pub(crate) port_input: Entity<InputState>,
    pub(crate) lang_select: Entity<gpui_kit::component::select::SelectState<gpui_kit::component::select::SearchableVec<String>>>,
    /// The last port the user typed was invalid; cleared by the next valid save.
    pub(crate) port_invalid: bool,
    pub(crate) file_table: Entity<gpui_kit::component::table::TableState<disk_map::FileRows>>,
    /// (zoomed folder, its size) the table was last filled for.
    pub(crate) table_key: Option<(std::path::PathBuf, u64)>,
    /// Folders visited in the Disk Map, for back/forward.
    pub(crate) history: crate::history::History,
    /// Findings ticked on the Cleanup screen.
    pub(crate) cleanup_selected: std::collections::BTreeSet<std::path::PathBuf>,
    /// Expanded groups on the Cleanup screen ("safety:rule" for findings, "git:<kind>" and "git:<item id>").
    pub(crate) cleanup_open: std::collections::BTreeSet<String>,
    /// Git branches / worktrees ticked on the Cleanup screen: (system item id, branch name or path).
    pub(crate) git_selected: std::collections::BTreeSet<(String, String)>,
    /// Folder tree next to the map: shown?, expanded folders (relative), rows, scroll, last synced zoom.
    pub(crate) tree_visible: bool,
    pub(crate) tree_open: std::collections::BTreeSet<std::path::PathBuf>,
    pub(crate) tree_rows: Rc<Vec<tree::TreeRow>>,
    pub(crate) tree_scroll: UniformListScrollHandle,
    pub(crate) tree_last_zoom: Option<std::path::PathBuf>,
    /// Row widths of the tree and of the search results (all rows equal, for horizontal scrolling).
    pub(crate) tree_row_w: f32,
    /// Width of the folder tree pane, changed by dragging its right edge; `Some` while dragging
    /// (the pointer's offset from the edge when the drag started).
    pub(crate) tree_width: f32,
    pub(crate) tree_drag: Option<f32>,
    pub(crate) tree_hit_w: f32,
    pub(crate) tree_hits_scroll: UniformListScrollHandle,
    /// Fuzzy search over the whole scan: the field, the results (`None` = no query, show the tree)
    /// and a counter that cancels an older search when the query changes.
    pub(crate) tree_query: Entity<InputState>,
    pub(crate) hover_tip: Entity<hover_tip::HoverTip>,
    pub(crate) tree_hits: Option<Rc<Vec<tree::Hit>>>,
    pub(crate) tree_search_gen: Arc<std::sync::atomic::AtomicU64>,
    /// Blocks right-clicked in the treemap (absolute paths), for trashing several at once.
    pub(crate) map_marked: std::collections::BTreeSet<std::path::PathBuf>,
    pub(crate) map_focus: FocusHandle,
    /// Disk Map shows a folder's "+ N small items" as their own map: (absolute folder, first child shown).
    pub(crate) map_skip: Option<(std::path::PathBuf, usize)>,
}

/// Window content: the app, drawn from GPUI's view cache, with the mascot on top.
/// A mascot frame re-renders only the mascot and this shell; the app's last frame is reused
/// unless the app itself changed.
struct Shell {
    app: Arc<Shared>,
    view: Entity<AppView>,
    /// Only exists while Home is shown.
    mascot: Option<Entity<mascot::Mascot>>,
    /// The treemap's hover label, drawn above everything.
    tip: Entity<hover_tip::HoverTip>,
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Changing screens notifies the app view, which makes this shell re-render too.
        // Hidden while a confirm dialog is open, since it draws above the whole app view.
        let home = {
            let c = self.app.core.lock().unwrap();
            c.screen == Screen::Home && c.pending.is_none()
        };
        match (home, self.mascot.is_some()) {
            (true, false) => self.mascot = Some(cx.new(mascot::Mascot::new)),
            (false, true) => self.mascot = None,
            _ => {}
        }
        div()
            .relative()
            .size_full()
            .child(AnyView::from(self.view.clone()).cached(StyleRefinement::default().size_full()))
            .children(self.mascot.clone().map(|m| div().absolute().top(px(40.)).right(px(16.)).child(m)))
            .child(div().absolute().inset_0().child(self.tip.clone()))
    }
}

impl AppView {
    fn new(app: Arc<Shared>, rx: async_channel::Receiver<()>, hover_tip: Entity<hover_tip::HoverTip>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Re-render on every core change; show pending toasts; keep disk usage current.
        cx.spawn_in(window, async move |this, cx| {
            while rx.recv().await.is_ok() {
                let alive = this.update_in(cx, |v, window, cx| {
                    let toasts = std::mem::take(&mut v.app.core.lock().unwrap().toasts);
                    let l = loc();
                    for (count, bytes) in toasts {
                        let size = format::size(bytes, &l);
                        let msg = if count == 1 { t!("toast_trashed_one", size = size) } else { t!("toast_trashed", count = count.to_string(), size = size) }.to_string();
                        window.push_notification(Notification::success(msg), cx);
                    }
                    let system = std::mem::take(&mut v.app.core.lock().unwrap().system_toasts);
                    for (cleaner, repo, freed) in system {
                        let name = cleanup::system_name(cleaner, repo.as_deref());
                        let msg = match freed {
                            Some(b) => t!("toast_sys_freed", name = name, size = format::size(b, &l)).to_string(),
                            None => t!("toast_sys_done", name = name).to_string(),
                        };
                        window.push_notification(Notification::success(msg), cx);
                    }
                    v.volumes = platform::volumes();
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        // Live counters while a scan runs.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_millis(150)).await;
            let alive = this.update(cx, |v, cx| {
                if v.app.core.lock().unwrap().busy {
                    cx.notify();
                }
            });
            if alive.is_err() {
                break;
            }
        })
        .detach();
        let (theme, port, language) = {
            let c = app.core.lock().unwrap();
            (c.settings.theme.clone(), c.settings.mcp_port, c.settings.language.clone())
        };
        apply_theme(&theme, window, cx);
        let port_input = cx.new(|cx| InputState::new(window, cx).default_value(port.to_string()));
        use gpui_kit::component::select::{SearchableVec, SelectEvent, SelectState};
        // ponytail: the "System" label is translated once at startup
        let names: Vec<String> = std::iter::once(t!("settings_system").to_string()).chain(format::LANGS.iter().map(|(_, n)| n.to_string())).collect();
        let current = language.as_deref().and_then(|c| format::LANGS.iter().position(|(x, _)| *x == c)).map_or(0, |i| i + 1);
        let lang_select = cx.new(|cx| SelectState::new(SearchableVec::new(names.clone()), Some(IndexPath::new(current)), window, cx));
        cx.subscribe_in(&lang_select, window, move |this, _, ev: &SelectEvent<SearchableVec<String>>, _, cx| {
            if let SelectEvent::Confirm(Some(value)) = ev {
                let code = names.iter().position(|n| n == value).and_then(|i| i.checked_sub(1)).map(|i| format::LANGS[i].0);
                this.app.update_settings(|s| s.language = code.map(String::from));
                apply_language(code);
                cx.notify();
            }
        })
        .detach();
        let tree_query = cx.new(|cx| InputState::new(window, cx).placeholder(t!("tree_search").to_string()));
        cx.subscribe_in(&tree_query, window, |this, input, ev: &gpui_kit::component::input::InputEvent, _, cx| {
            if matches!(ev, gpui_kit::component::input::InputEvent::Change) {
                let q = input.read(cx).value().to_string();
                this.start_tree_search(q, cx);
            }
        })
        .detach();
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let file_table = cx.new(|cx| gpui_kit::component::table::TableState::new(disk_map::FileRows { rows: Vec::new() }, window, cx).row_selectable(true));
        cx.subscribe_in(&file_table, window, |this, table, ev: &gpui_kit::component::table::TableEvent, _, cx| {
            if let gpui_kit::component::table::TableEvent::SelectRow(ix) = ev {
                let path = table.read(cx).delegate().rows.get(*ix).map(|r| r.path.clone());
                if let Some(p) = path {
                    let _ = this.app.select(&p);
                    cx.notify();
                }
            }
        })
        .detach();
        AppView {
            app,
            focus,
            show_list: false,
            details_open: true,
            map_bounds: Rc::new(Cell::new(Bounds::default())),
            mini_bounds: Rc::new(Cell::new(Bounds::default())),
            volumes: platform::volumes(),
            port_input,
            lang_select,
            port_invalid: false,
            file_table,
            table_key: None,
            history: Default::default(),
            cleanup_selected: Default::default(),
            cleanup_open: Default::default(),
            git_selected: Default::default(),
            tree_visible: true,
            tree_open: Default::default(),
            tree_rows: Default::default(),
            tree_scroll: UniformListScrollHandle::new(),
            tree_last_zoom: None,
            tree_row_w: 0.,
            tree_width: 280.,
            tree_drag: None,
            tree_hit_w: 0.,
            tree_hits_scroll: UniformListScrollHandle::new(),
            tree_query,
            hover_tip,
            tree_hits: None,
            tree_search_gen: Default::default(),
            map_marked: Default::default(),
            map_focus: cx.focus_handle(),
            map_skip: None,
        }
    }

    /// Step back/forward through Disk Map folders (mouse side buttons, ⌘[ / ⌘]).
    pub(crate) fn go_history(&mut self, forward: bool, cx: &mut Context<Self>) {
        if self.app.core.lock().unwrap().screen != Screen::DiskMap {
            return;
        }
        let target = if forward { self.history.forward() } else { self.history.back() };
        if let Some(path) = target {
            let _ = self.app.navigate(&path);
            cx.notify();
        }
    }

    fn nav_item(&self, core: &Core, key: &str, icon: Lucide, screen: Screen, enabled: bool, cx: &mut Context<Self>) -> SidebarMenuItem {
        SidebarMenuItem::new(t!(key).to_string())
            .icon(icon)
            .active(core.screen == screen)
            .disable(!enabled)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.app.show(screen);
                cx.notify();
            }))
    }

    fn sidebar(&self, core: &Core, cx: &mut Context<Self>) -> impl IntoElement {
        let has_scan = core.tree.is_some();
        let primary = cx.theme().primary;
        Sidebar::<SidebarMenu>::new("nav")
            .w(px(210.))
            .header(
                SidebarHeader::new().child(
                    div()
                        .h_flex()
                        .gap_2()
                        .child(Icon::new(Lucide::HardDrive).text_color(primary))
                        .child(div().font_weight(FontWeight::SEMIBOLD).child(t!("app_title").to_string())),
                ),
            )
            .child(
                SidebarMenu::new()
                    .child(self.nav_item(core, "nav_home", Lucide::House, Screen::Home, true, cx))
                    .child(self.nav_item(core, "nav_map", Lucide::Map, Screen::DiskMap, has_scan, cx))
                    .child(self.nav_item(core, "nav_cleanup", Lucide::Sparkles, Screen::Cleanup, true, cx))
                    .child(self.nav_item(core, "nav_settings", Lucide::Settings, Screen::Settings, true, cx)),
            )
    }

    fn help(&self) -> impl IntoElement {
        Popover::new("help")
            .trigger(Button::new("help-btn").ghost().small().icon(Lucide::CircleQuestionMark).tooltip(t!("help_title").to_string()))
            .content(|_, _, _| {
                div()
                    .v_flex()
                    .w(px(320.))
                    .p_3()
                    .gap_2()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(t!("help_title").to_string()))
                    .children(["help_1", "help_2", "help_3"].into_iter().enumerate().map(|(i, key)| {
                        div()
                            .h_flex()
                            .gap_2()
                            .items_start()
                            .child(Tag::primary().small().rounded_full().child((i + 1).to_string()))
                            .child(div().flex_1().min_w_0().text_sm().child(t!(key).to_string()))
                    }))
            })
    }

    /// Problems worth telling the user about on every screen.
    fn notices(&self, core: &Core) -> Vec<AnyElement> {
        let mut out = Vec::new();
        let denied = core.progress.denied.load(Relaxed);
        if denied > 0 && !core.busy {
            let text = if platform::has_permission_settings() {
                t!("banner_denied", count = denied.to_string()).to_string()
            } else {
                format!("{} {}", t!("banner_denied", count = denied.to_string()), t!("banner_admin"))
            };
            out.push(
                div()
                    .h_flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(Alert::warning("denied", text)))
                    .when(platform::has_permission_settings(), |d| {
                        d.child(Button::new("grant").small().label(t!("banner_grant").to_string()).on_click(|_, _, _| platform::open_permission_settings()))
                    })
                    .into_any_element(),
            );
        }
        if core.settings_recovered {
            out.push(Alert::warning("recovered", t!("banner_recovered").to_string()).into_any_element());
        }
        if let Some(e) = &core.settings_error {
            out.push(Alert::error("settings-error", t!("banner_settings_failed", error = e).to_string()).into_any_element());
        }
        for (i, p) in core.refused.iter().enumerate() {
            out.push(Alert::error(("refused", i), t!("banner_refused", path = p.display().to_string()).to_string()).into_any_element());
        }
        for (i, (p, e)) in core.trash_errors.iter().enumerate() {
            out.push(Alert::error(("trash-error", i), t!("banner_trash_failed", path = p.display().to_string(), error = e).to_string()).into_any_element());
        }
        out
    }
}

impl Render for AppView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let app = self.app.clone();
        let core = app.core.lock().unwrap();
        let body = match core.screen {
            Screen::Home => self.home(&core, cx),
            Screen::DiskMap => self.disk_map(&core, cx),
            Screen::Cleanup => self.cleanup(&core, cx),
            Screen::Settings => self.settings_view(&core, window, cx),
        };
        let notices = self.notices(&core);
        let (bg, fg) = {
            let t = cx.theme();
            (t.background, t.foreground)
        };
        let notifications = Root::render_notification_layer(window, cx);
        div()
            .id("app")
            .key_context("FastScanCleaner")
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &Refresh, _, cx| {
                let _ = this.app.refresh(None);
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Back, _, cx| this.go_history(false, cx)))
            .on_action(cx.listener(|this, _: &Forward, _, cx| this.go_history(true, cx)))
            .on_action(cx.listener(|this, _: &TrashMarked, _, cx| this.trash_marked(cx)))
            .on_action(cx.listener(|this, _: &ClearMarked, _, cx| {
                this.map_marked.clear();
                cx.notify();
            }))
            .relative()
            .size_full()
            .v_flex()
            .bg(bg)
            .text_color(fg)
            .child(TitleBar::new().child(div().flex_1()).child(div().pr_2().child(self.help())))
            .child(
                div().flex_1().min_h_0().h_flex().child(self.sidebar(&core, cx)).child(
                    div()
                        .v_flex()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .when(!notices.is_empty(), |d| d.child(div().v_flex().gap_2().px_6().pt_4().children(notices)))
                        .child(div().flex_1().min_h_0().child(body)),
                ),
            )
            .children(core.pending.as_ref().map(|p| self.confirm_dialog(p, cx)))
            .children(notifications)
    }
}
