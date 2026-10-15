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

mod a11y;
mod about_view;
mod agent;
mod apps_card;
mod models_card;
mod cleanup;
mod dialog;
mod disk_map;
mod home;
mod hover_tip;
mod mascot;
mod nav_history;
mod old_files_card;
mod settings_view;
mod tree;
mod welcome;
mod wizard;

actions!(fastscancleaner, [Refresh, Back, Forward, TrashMarked, ClearMarked, ShowHome, ShowMap, ShowCleanup, ShowFreeUp, ShowSettings, ShowAbout]);

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

/// Tick or untick `key` in a set of ticked items, as a checkbox's click says.
pub(crate) fn tick<T: Eq + std::hash::Hash + Clone>(set: &mut std::collections::HashSet<T>, key: &T, on: bool) {
    if on {
        set.insert(key.clone());
    } else {
        set.remove(key);
    }
}

pub fn run(app: Arc<Shared>, rx: async_channel::Receiver<()>) {
    gpui_kit::application().with_assets(gpui_kit::assets::AllAssets).run(move |cx| {
        platform::set_app_icon();
        gpui_kit::init(cx);
        cx.bind_keys([
            KeyBinding::new("cmd-r", Refresh, None),
            // The sidebar has no keyboard focus of its own: views are one shortcut away.
            KeyBinding::new("cmd-1", ShowHome, None),
            KeyBinding::new("ctrl-1", ShowHome, None),
            KeyBinding::new("cmd-2", ShowMap, None),
            KeyBinding::new("ctrl-2", ShowMap, None),
            KeyBinding::new("cmd-3", ShowCleanup, None),
            KeyBinding::new("ctrl-3", ShowCleanup, None),
            KeyBinding::new("cmd-4", ShowFreeUp, None),
            KeyBinding::new("ctrl-4", ShowFreeUp, None),
            KeyBinding::new("cmd-5", ShowSettings, None),
            KeyBinding::new("ctrl-5", ShowSettings, None),
            KeyBinding::new("cmd-6", ShowAbout, None),
            KeyBinding::new("ctrl-6", ShowAbout, None),
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
                let shell = cx.new(|cx| Shell { app, view, mascot: None, tip, splash: Some((std::time::Instant::now(), cx.new(mascot::Mascot::new))) });
                // Any key skips the splash.
                let weak = shell.downgrade();
                cx.observe_keystrokes(move |_, _, cx| {
                    let _ = weak.update(cx, |s, cx| {
                        if s.splash.take().is_some() {
                            cx.notify();
                        }
                    });
                })
                .detach();
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
    pub(crate) volumes: Vec<platform::Volume>,
    pub(crate) port_input: Entity<InputState>,
    pub(crate) lang_select: Entity<gpui_kit::component::select::SelectState<gpui_kit::component::select::SearchableVec<String>>>,
    /// The last port the user typed was invalid; cleared by the next valid save.
    pub(crate) port_invalid: bool,
    /// Time of the scan the Over-time card compares with; None = the previous one.
    history_compare: Option<u64>,
    history_open: std::collections::HashSet<String>,
    /// Index into the welcome guide's steps while it is open.
    guide: Option<usize>,
    welcome_mascot: Option<Entity<mascot::Mascot>>,
    /// One focus handle per pressable area (see `a11y::Pressable`), stable across renders.
    focus_handles: std::cell::RefCell<std::collections::HashMap<SharedString, FocusHandle>>,
    /// One scroll handle per bounded dialog list, stable across renders (scroll position survives).
    scroll_handles: std::cell::RefCell<std::collections::HashMap<SharedString, ScrollHandle>>,
    /// A confirmation that just opened takes keyboard focus once (then Tab reaches its buttons).
    dialog_focused: bool,
    /// Screen of the last render: a new screen moves keyboard focus back to its top.
    focused_screen: Option<Screen>,
    pub(crate) hover_tip: Entity<hover_tip::HoverTip>,
    pub(crate) tree: tree::TreeState,
    pub(crate) wizard: wizard::WizardState,
    pub(crate) cleanup: cleanup::CleanupState,
    pub(crate) map: disk_map::MapState,
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
    /// Start time and mascot of the splash screen, until it has faded out or was skipped.
    splash: Option<(std::time::Instant, Entity<mascot::Mascot>)>,
}

/// Splash screen: shown this long, then faded out over `SPLASH_FADE_MS`.
const SPLASH_HOLD_MS: u64 = 900;
const SPLASH_FADE_MS: u64 = 300;

/// Splash opacity `elapsed_ms` after start; 0 means gone. Reduce motion: no fade.
fn splash_alpha(elapsed_ms: u64, reduce_motion: bool) -> f32 {
    if elapsed_ms >= SPLASH_HOLD_MS + SPLASH_FADE_MS {
        0.
    } else if reduce_motion || elapsed_ms <= SPLASH_HOLD_MS {
        1.
    } else {
        let t = (elapsed_ms - SPLASH_HOLD_MS) as f32 / SPLASH_FADE_MS as f32;
        1. - t * t
    }
}

/// The view content fades in while sliding up a little; keyed by screen, so each switch replays it.
fn view_transition(screen: Screen, body: AnyElement, reduce_motion: bool) -> AnyElement {
    let view = div().relative().size_full().child(body);
    if reduce_motion {
        return view.into_any_element();
    }
    view.with_animation(ElementId::Name(format!("view-{screen:?}").into()), Animation::new(Duration::from_millis(200)).with_easing(ease_out_quint()), |v, t| v.opacity(t).top(px((1. - t) * 12.)))
        .into_any_element()
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let splash = self.splash.as_ref().map(|(start, m)| (splash_alpha(start.elapsed().as_millis() as u64, cx.reduce_motion()), m.clone())).filter(|(alpha, _)| *alpha > 0.);
        if splash.is_some() {
            window.request_animation_frame();
        } else {
            self.splash = None;
        }
        // Changing screens notifies the app view, which makes this shell re-render too.
        // Hidden while a confirm dialog is open, since it draws above the whole app view.
        let guide_open = self.view.read(cx).guide.is_some();
        let home = {
            let c = self.app.core.lock().unwrap();
            c.screen == Screen::Home && c.pending.is_none()
        } && !guide_open;
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
            .children(splash.map(|(alpha, mascot)| {
                let t = cx.theme();
                div()
                    .id("splash")
                    .absolute()
                    .inset_0()
                    .occlude()
                    .bg(t.background)
                    .opacity(alpha)
                    .v_flex()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(mascot)
                    .child(div().text_3xl().font_weight(FontWeight::BOLD).text_color(t.foreground).child(t!("app_title").to_string()))
                    .child(div().text_color(t.muted_foreground).child(t!("home_tagline").to_string()))
                    .on_mouse_down(MouseButton::Left, cx.listener(|s, _, _, cx| {
                        s.splash = None;
                        cx.notify();
                    }))
            }))
    }
}

impl AppView {
    fn new(app: Arc<Shared>, rx: async_channel::Receiver<()>, hover_tip: Entity<hover_tip::HoverTip>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let guide = app.welcome_needed().then_some(0);
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
        // Agent input from MCP, applied to the window without the real mouse. Checked again here,
        // on the UI thread, so nothing slips past a confirmation that opened in the meantime.
        let inputs = app.inputs.clone();
        let gate = app.clone();
        cx.spawn_in(window, async move |this, cx| {
            while let Ok((input, reply)) = inputs.recv().await {
                if input == crate::state::Input::Reveal {
                    // Answer once a fresh frame is drawn, so the screenshot shows the current state.
                    // The view is cached, so it has to be told to render again.
                    let _ = this.update(cx, |_, cx| cx.notify());
                    let _ = cx.update(|window, _| {
                        if !window.is_visible() {
                            platform::order_front_quietly();
                        }
                        window.refresh();
                        // ponytail: two frames, because the disk map lays out in the frame after its parent
                        window.on_next_frame(move |window, _| {
                            window.refresh();
                            window.on_next_frame(move |_, _| {
                                let _ = reply.send(Ok(()));
                            });
                        });
                    });
                    continue;
                }
                let r = cx.update(|window, cx| {
                    gate.input_allowed()?;
                    agent::dispatch(input, gate.shot_width.load(Relaxed), window, cx)
                });
                let _ = reply.send(r.unwrap_or_else(|_| Err("app window is closed".into())));
            }
        })
        .detach();
        // While the welcome guide is open, redraw every second: its access step re-checks.
        cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(Duration::from_secs(1)).await;
            let alive = this.update(cx, |v, cx| {
                if v.guide.is_some() {
                    cx.notify();
                }
            });
            if alive.is_err() {
                break;
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
        let wizard_gb = cx.new(|cx| InputState::new(window, cx).placeholder("25").validate(|s, _| wizard::gb_typing_ok(s)));
        cx.subscribe_in(&wizard_gb, window, |this, input, ev: &gpui_kit::component::input::InputEvent, _, cx| {
            if matches!(ev, gpui_kit::component::input::InputEvent::Change)
                && let Some(b) = wizard::parse_gb(&input.read(cx).value())
            {
                this.wizard.target = crate::wizard::Target::Bytes(b);
                cx.notify();
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
            volumes: platform::volumes(),
            port_input,
            lang_select,
            port_invalid: false,
            history_compare: None,
            history_open: Default::default(),
            guide,
            welcome_mascot: None,
            focus_handles: Default::default(),
            scroll_handles: Default::default(),
            dialog_focused: false,
            focused_screen: None,
            hover_tip,
            tree: tree::TreeState {
                visible: true,
                open: Default::default(),
                rows: Default::default(),
                scroll: UniformListScrollHandle::new(),
                last_zoom: None,
                row_w: 0.,
                width: 280.,
                drag: None,
                hit_w: 0.,
                hits_scroll: UniformListScrollHandle::new(),
                query: tree_query,
                hits: None,
                search_gen: Default::default(),
            },
            wizard: wizard::WizardState {
                target: crate::wizard::Target::Bytes(10_000_000_000),
                ticks: Default::default(),
                seen: None,
                manual: false,
                trashed: None,
                open: Default::default(),
                gb: wizard_gb,
            },
            cleanup: cleanup::CleanupState {
                selected: Default::default(),
                open: Default::default(),
                git_selected: Default::default(),
                dupe_keep: Default::default(),
                dupes_shown: 50,
                apps_ticked: Default::default(),
                apps_open: Default::default(),
                models_ticked: Default::default(),
                old_ticked: Default::default(),
                old_open: Default::default(),
                old_shown: 50,
            },
            map: disk_map::MapState {
                marked: Default::default(),
                skip: None,
                focus: cx.focus_handle(),
                bounds: Rc::new(Cell::new(Bounds::default())),
                mini_bounds: Rc::new(Cell::new(Bounds::default())),
                show_list: false,
                details_open: true,
                file_table,
                table_key: None,
                history: Default::default(),
            },
        }
    }

    /// Step back/forward through Disk Map folders (mouse side buttons, ⌘[ / ⌘]).
    pub(crate) fn go_history(&mut self, forward: bool, cx: &mut Context<Self>) {
        if self.app.core.lock().unwrap().screen != Screen::DiskMap {
            return;
        }
        let target = if forward { self.map.history.forward() } else { self.map.history.back() };
        if let Some(path) = target {
            let _ = self.app.navigate(&path);
            cx.notify();
        }
    }

    fn show_view(&mut self, screen: Screen, cx: &mut Context<Self>) {
        // An open confirmation stays in front: the view behind it doesn't change.
        if self.app.core.lock().unwrap().pending.is_some() {
            return;
        }
        self.app.show(screen);
        cx.notify();
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
                    .child(self.nav_item(core, "nav_free_up", Lucide::Gauge, Screen::FreeUp, true, cx))
                    .child(self.nav_item(core, "nav_settings", Lucide::Settings, Screen::Settings, true, cx))
                    .child(self.nav_item(core, "nav_about", Lucide::Info, Screen::About, true, cx)),
            )
    }

    fn help(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let view = cx.entity().downgrade();
        Popover::new("help")
            .trigger(Button::new("help-btn").accessibility_label(t!("help_title").to_string()).ghost().small().icon(Lucide::CircleQuestionMark).tooltip(t!("help_title").to_string()))
            .content(move |_, _, _| {
                let view = view.clone();
                div()
                    .v_flex()
                    .w(px(320.))
                    .p_3()
                    .gap_2()
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(t!("help_title").to_string()))
                    .children(["help_1", "help_2", "help_3", "help_keys"].into_iter().enumerate().map(|(i, key)| {
                        div()
                            .h_flex()
                            .gap_2()
                            .items_start()
                            .child(Tag::primary().small().rounded_full().child((i + 1).to_string()))
                            .child(div().flex_1().min_w_0().text_sm().child(t!(key, modifier = if cfg!(target_os = "macos") { "⌘" } else { "Ctrl+" }).to_string()))
                    }))
                    .child(Button::new("help-welcome").outline().small().w_full().label(t!("welcome_show").to_string()).on_click(move |_, _, cx| {
                        let _ = view.update(cx, |this, cx| {
                            this.guide = Some(0);
                            cx.notify();
                        });
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
        // A scan picked on the guide's last step ends the guide.
        if self.guide_step() == Some(welcome::Step::Places) && core.scanning.is_some() {
            self.guide = None;
            self.app.welcome_done();
        }
        if self.guide.is_some() {
            let (bg, fg) = {
                let t = cx.theme();
                (t.background, t.foreground)
            };
            let guide = self.welcome(&core, cx);
            return div().id("app").relative().size_full().v_flex().bg(bg).text_color(fg).child(TitleBar::new()).child(guide).children(core.pending.as_ref().map(|p| self.confirm_dialog(p, cx))).children(Root::render_notification_layer(window, cx));
        }
        // The element that had focus is gone with the old screen; Tab would go nowhere.
        if self.focused_screen != Some(core.screen) {
            if self.focused_screen.is_some() {
                window.focus(&self.focus, cx);
            }
            self.focused_screen = Some(core.screen);
        }
        match (core.pending.is_some(), self.dialog_focused) {
            (true, false) => {
                window.focus(&self.focus_for("confirm-dialog", cx), cx);
                self.dialog_focused = true;
            }
            (false, true) => self.dialog_focused = false,
            _ => {}
        }
        let body = match core.screen {
            Screen::Home => self.home(&core, cx),
            Screen::DiskMap => self.disk_map(&core, cx),
            Screen::Cleanup => self.cleanup(&core, cx),
            Screen::FreeUp => self.free_up(&core, window, cx),
            Screen::Settings => self.settings_view(&core, window, cx),
            Screen::About => self.about_view(&core, window, cx),
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
            .on_action(cx.listener(|this, _: &ShowHome, _, cx| this.show_view(Screen::Home, cx)))
            .on_action(cx.listener(|this, _: &ShowMap, _, cx| this.show_view(Screen::DiskMap, cx)))
            .on_action(cx.listener(|this, _: &ShowCleanup, _, cx| this.show_view(Screen::Cleanup, cx)))
            .on_action(cx.listener(|this, _: &ShowFreeUp, _, cx| this.show_view(Screen::FreeUp, cx)))
            .on_action(cx.listener(|this, _: &ShowSettings, _, cx| this.show_view(Screen::Settings, cx)))
            .on_action(cx.listener(|this, _: &ShowAbout, _, cx| this.show_view(Screen::About, cx)))
            .on_action(cx.listener(|this, _: &Refresh, _, cx| {
                drop(this.app.refresh(None));
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &Back, _, cx| this.go_history(false, cx)))
            .on_action(cx.listener(|this, _: &Forward, _, cx| this.go_history(true, cx)))
            .on_action(cx.listener(|this, _: &TrashMarked, _, cx| this.trash_marked(cx)))
            .on_action(cx.listener(|this, _: &ClearMarked, _, cx| {
                this.map.marked.clear();
                cx.notify();
            }))
            .relative()
            .size_full()
            .v_flex()
            .bg(bg)
            .text_color(fg)
            .child(TitleBar::new().child(div().flex_1()).child(div().pr_2().child(self.help(cx))))
            .child(
                div().flex_1().min_h_0().h_flex().child(self.sidebar(&core, cx)).child(
                    div()
                        .v_flex()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .when(!notices.is_empty(), |d| d.child(div().v_flex().gap_2().px_6().pt_4().children(notices)))
                        .child(div().flex_1().min_h_0().child(view_transition(core.screen, body, cx.reduce_motion()))),
                ),
            )
            .children(core.pending.as_ref().map(|p| self.confirm_dialog(p, cx)))
            .children(notifications)
    }
}

#[cfg(test)]
mod tests {
    use super::{SPLASH_FADE_MS, SPLASH_HOLD_MS, splash_alpha};

    #[test]
    fn splash_holds_then_fades_out() {
        assert_eq!(splash_alpha(0, false), 1.);
        assert_eq!(splash_alpha(SPLASH_HOLD_MS, false), 1.);
        let mid = splash_alpha(SPLASH_HOLD_MS + SPLASH_FADE_MS / 2, false);
        assert!(mid > 0. && mid < 1., "{mid}");
        assert_eq!(splash_alpha(SPLASH_HOLD_MS + SPLASH_FADE_MS, false), 0.);
        // Reduce motion: no fade, just gone.
        assert_eq!(splash_alpha(SPLASH_HOLD_MS + SPLASH_FADE_MS / 2, true), 1.);
        assert_eq!(splash_alpha(SPLASH_HOLD_MS + SPLASH_FADE_MS, true), 0.);
    }
}
