use super::{AppView, card, heading, loc};
use crate::settings::MapColors;
use crate::state::{Core, HomeState, Screen};
use crate::{format, platform};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::alert::Alert;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::progress::Progress;
use gpui_kit::component::separator::Separator;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rust_i18n::t;
use std::path::Path;
use std::sync::atomic::Ordering::Relaxed;
use gpui_kit::component::scroll::ScrollableElement as _;

fn is_system(v: &platform::Volume) -> bool {
    v.mount == Path::new("/") || v.mount == Path::new("C:\\")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

struct Colors {
    muted: Hsla,
    primary: Hsla,
    success: Hsla,
    info: Hsla,
    border: Hsla,
    bg: Hsla,
    soft: Hsla,
}

fn colors(cx: &App) -> Colors {
    let t = cx.theme();
    Colors { muted: t.muted_foreground, primary: t.primary, success: t.success, info: t.info, border: t.border, bg: t.background, soft: t.secondary }
}

impl AppView {
    pub(super) fn home(&self, core: &Core, cx: &mut Context<Self>) -> AnyElement {
        let body = match core.home_state() {
            HomeState::Idle => self.home_idle(core, cx),
            HomeState::Scanning => self.home_scanning(core, cx),
            HomeState::Complete => self.home_complete(core, cx),
            HomeState::Interrupted => self.home_interrupted(core, cx),
        };
        div()
            .id("home")
            .size_full()
            .overflow_y_scrollbar()
            .child(div().v_flex().w_full().max_w(px(880.)).mx_auto().p_8().gap_5().children(body))
            .into_any_element()
    }

    /// Main drive with usage bar and legend; other drives as compact rows.
    fn drive_content(&self, cx: &mut Context<Self>) -> Div {
        let c = colors(cx);
        let l = loc();
        let mut vols: Vec<&platform::Volume> = self.volumes.iter().collect();
        vols.sort_by_key(|v| !is_system(v));
        let Some(main) = vols.first().copied() else { return div() };
        let used = main.total.saturating_sub(main.available);
        let share = |x: u64| if main.total > 0 { x as f64 * 100.0 / main.total as f64 } else { 0.0 };
        let others = vols.iter().skip(1).enumerate().map(|(i, v)| {
            let u = v.total.saturating_sub(v.available);
            let value = if v.total > 0 { u as f32 * 100.0 / v.total as f32 } else { 0.0 };
            div()
                .h_flex()
                .gap_3()
                .text_sm()
                .child(Icon::new(Lucide::HardDrive).size_4().text_color(c.muted))
                .child(div().w(px(160.)).truncate().child(v.name.clone()))
                .child(div().flex_1().child(Progress::new(("volume", i)).small().value(value)))
                .child(div().text_color(c.muted).child(t!("disk_used", used = format::size(u, &l), total = format::size(v.total, &l)).to_string()))
        });
        div()
            .v_flex()
            .gap_3()
            .child(
                div()
                    .h_flex()
                    .gap_4()
                    .items_start()
                    .child(div().size(px(48.)).flex_shrink_0().rounded_lg().bg(c.soft).flex().items_center().justify_center().child(Icon::new(Lucide::HardDrive).size_6().text_color(c.muted)))
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .gap_2()
                            .child(
                                div()
                                    .h_flex()
                                    .gap_2()
                                    .child(div().font_weight(FontWeight::SEMIBOLD).child(main.name.clone()))
                                    .when(is_system(main), |d| d.child(Tag::secondary().small().child(t!("main_drive").to_string()))),
                            )
                            .child(
                                div()
                                    .h_flex()
                                    .gap_1()
                                    .items_baseline()
                                    .child(div().text_2xl().font_weight(FontWeight::BOLD).child(format::size(used, &l)))
                                    .child(div().text_color(c.muted).child(t!("disk_used_of", total = format::size(main.total, &l)).to_string())),
                            )
                            .child(Progress::new("main-disk").value(share(used) as f32).color(c.primary))
                            .child(
                                div()
                                    .h_flex()
                                    .justify_between()
                                    .text_xs()
                                    .text_color(c.muted)
                                    .child(div().h_flex().gap_1().child(div().size(px(8.)).rounded_full().bg(c.primary)).child(
                                        t!("legend_used", size = format::size(used, &l), pct = format::pct(share(used), &l)).to_string(),
                                    ))
                                    .child(div().h_flex().gap_1().child(div().size(px(8.)).rounded_full().bg(c.success)).child(
                                        t!("legend_free", size = format::size(main.available, &l), pct = format::pct(share(main.available), &l)).to_string(),
                                    )),
                            ),
                    ),
            )
            .children(others)
    }

    fn stat_cards(&self, files: Option<u64>, largest: Option<u64>, dim: bool, cx: &mut Context<Self>) -> AnyElement {
        let c = colors(cx);
        let l = loc();
        let stat = |icon: Lucide, value: String, label: String| {
            card(c.border, c.bg)
                .flex_1()
                .h_flex()
                .gap_3()
                .child(Icon::new(icon).size_6().text_color(c.muted))
                .child(div().v_flex().child(div().text_xl().font_weight(FontWeight::BOLD).child(value)).child(div().text_sm().text_color(c.muted).child(label)))
        };
        div()
            .h_flex()
            .gap_4()
            .when(dim, |d| d.opacity(0.5))
            .child(stat(Lucide::File, files.map_or("–".into(), |n| format::count(n, &l)), t!("stat_files").to_string()))
            .child(stat(Lucide::FileText, largest.map_or("–".into(), |b| format::size(b, &l)), t!("stat_largest").to_string()))
            .into_any_element()
    }

    fn recent(&self, core: &Core, cx: &mut Context<Self>) -> AnyElement {
        let c = colors(cx);
        let l = loc();
        let now = now_secs();
        let rows: Vec<_> = core
            .settings
            .recent
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let path = r.path.clone();
                let name = r.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| r.path.display().to_string());
                let (key, n) = format::ago(now.saturating_sub(r.scanned_at));
                div()
                    .id(("recent", i))
                    .h_flex()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .rounded_lg()
                    .cursor_pointer()
                    .hover(|s| s.bg(c.soft))
                    .child(Icon::new(Lucide::Folder).text_color(c.primary))
                    .child(
                        div()
                            .v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().font_weight(FontWeight::MEDIUM).truncate().child(name))
                            .child(div().text_xs().text_color(c.muted).truncate().child(r.path.display().to_string())),
                    )
                    .child(div().text_sm().child(format::size(r.size, &l)))
                    .child(div().w(px(100.)).text_xs().text_color(c.muted).text_right().child(t!(key, n = n.to_string()).to_string()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let _ = this.app.start_scan(path.clone());
                        cx.notify();
                    }))
            })
            .collect();
        card(c.border, c.bg)
            .child(div().font_weight(FontWeight::SEMIBOLD).child(t!("home_recent").to_string()))
            .child(if rows.is_empty() {
                div().text_sm().text_color(c.muted).child(t!("home_no_recent").to_string()).into_any_element()
            } else {
                div().v_flex().children(rows).into_any_element()
            })
            .into_any_element()
    }

    /// "Where should I look?": one card per common place, plus "Other folder…".
    fn place_picker(&self, core: &Core, cx: &mut Context<Self>) -> Div {
        let c = colors(cx);
        let l = loc();
        let home = platform::home();
        let card_base = |id: (&'static str, usize)| {
            div()
                .id(id)
                .w(px(240.))
                .h(px(84.)) // every place card the same size, whatever it shows
                .h_flex()
                .gap_3()
                .items_start()
                .p_3()
                .rounded_lg()
                .border_1()
                .border_color(c.border)
                .cursor_pointer()
                .hover(|s| s.bg(c.soft).border_color(c.primary))
        };
        let places = platform::scan_places().into_iter().enumerate().map(|(i, place)| {
            let icon = match place.key {
                "place_home" => Lucide::House,
                "place_downloads" => Lucide::Download,
                "place_documents" => Lucide::FileText,
                "place_desktop" => Lucide::Monitor,
                _ => Lucide::Archive,
            };
            // "~/Downloads" reads better than the full path.
            let shown = match place.path.strip_prefix(&home) {
                Ok(rest) if rest.as_os_str().is_empty() => place.path.display().to_string(),
                Ok(rest) => format!("~/{}", rest.display()),
                Err(_) => place.path.display().to_string(),
            };
            let last = core.settings.recent_size(&place.path).map(|b| t!("place_last", size = format::size(b, &l)).to_string());
            let path = place.path.clone();
            card_base(("place", i))
                .when(place.recommended, |d| d.border_color(c.primary))
                .child(Icon::new(icon).size_5().flex_shrink_0().text_color(c.primary))
                .child(
                    div()
                        .v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(div().font_weight(FontWeight::SEMIBOLD).truncate().child(t!(place.key).to_string()))
                        .child(div().text_xs().text_color(c.muted).truncate().child(shown))
                        .child(
                            div()
                                .h_flex()
                                .gap_2()
                                .text_xs()
                                .text_color(c.muted)
                                .when(place.recommended, |d| d.child(Tag::primary().xsmall().child(t!("recommended").to_string())))
                                .children(last.map(|s| div().truncate().child(s))),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    let _ = this.app.start_scan(path.clone());
                    cx.notify();
                }))
        });
        let other = card_base(("place-other", 0))
            .child(Icon::new(Lucide::FolderOpen).size_5().flex_shrink_0().text_color(c.muted))
            .child(div().flex_1().min_w_0().font_weight(FontWeight::SEMIBOLD).child(t!("place_other").to_string()))
            .on_click(cx.listener(|this, _, _, cx| this.pick_folder(cx)));
        div()
            .v_flex()
            .gap_3()
            .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(t!("home_where_title").to_string()))
            .child(div().text_sm().text_color(c.muted).child(t!("home_where_hint").to_string()))
            .child(div().h_flex().flex_wrap().gap_3().children(places).child(other))
    }

    /// Link from Home to the system tools on the Cleanup screen (no scan needed).
    fn system_tools_shortcut(&self, cx: &mut Context<Self>) -> AnyElement {
        let c = colors(cx);
        div()
            .id("system-tools-shortcut")
            .h_flex()
            .gap_3()
            .p_4()
            .rounded_xl()
            .border_1()
            .border_color(c.border)
            .bg(c.bg)
            .cursor_pointer()
            .hover(|s| s.bg(c.soft).border_color(c.primary))
            .child(Icon::new(Lucide::Sparkles).size_5().flex_shrink_0().text_color(c.primary))
            .child(div().flex_1().min_w_0().font_weight(FontWeight::SEMIBOLD).child(t!("home_system_tools").to_string()))
            .child(Icon::new(Lucide::ArrowRight).size_4().text_color(c.muted))
            .on_click(cx.listener(|this, _, _, cx| {
                this.app.show_system_tools();
                cx.notify();
            }))
            .into_any_element()
    }

    fn home_idle(&self, core: &Core, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let c = colors(cx);
        vec![
            heading(None, t!("home_welcome").to_string(), t!("home_tagline").to_string(), c.muted).into_any_element(),
            card(c.border, c.bg)
                .child(self.drive_content(cx))
                .child(self.place_picker(core, cx))
                .into_any_element(),
            self.stat_cards(core.stats.map(|s| s.files), core.stats.map(|s| s.largest_file), false, cx),
            self.system_tools_shortcut(cx),
            self.recent(core, cx),
        ]
    }

    fn home_scanning(&self, core: &Core, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let c = colors(cx);
        let l = loc();
        let p = &core.progress;
        let (files, bytes) = (p.files.load(Relaxed), p.bytes.load(Relaxed));
        let current = p.current.lock().unwrap().display().to_string();
        let target = core.scanning.clone().unwrap_or_default();
        let title = if target == platform::default_scan_root() {
            t!("scan_title_computer").to_string()
        } else {
            let folder = target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| target.display().to_string());
            t!("scan_title_folder", folder = folder).to_string()
        };
        // ponytail: progress is estimated from the last scan of the same folder; first scans are indeterminate
        let pct = core.estimate.filter(|e| *e > 0).map(|e| (bytes as f64 * 100.0 / e as f64).min(99.0) as f32);
        let bar = match pct {
            Some(v) => Progress::new("scan").value(v).color(c.primary),
            None => Progress::new("scan").loading(true).color(c.primary),
        };
        let row = |icon: Lucide, label: String, value: String| {
            div()
                .h_flex()
                .gap_2()
                .text_sm()
                .child(Icon::new(icon).size_4().text_color(c.muted))
                .child(div().w(px(150.)).flex_shrink_0().whitespace_nowrap().text_color(c.muted).child(label))
                .child(div().flex_1().min_w_0().truncate().child(value))
        };
        vec![
            heading(Some(Spinner::new().large().color(c.primary).into_any_element()), title, t!("scan_subtitle").to_string(), c.muted).into_any_element(),
            card(c.border, c.bg)
                .child(div().h_flex().gap_3().child(div().flex_1().child(bar)).children(pct.map(|v| div().text_sm().font_weight(FontWeight::SEMIBOLD).child(format!("{v:.0}%")))))
                .child(
                    div()
                        .h_flex()
                        .gap_4()
                        .items_start()
                        .child(
                            div()
                                .v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_2()
                                .child(row(Lucide::Folder, t!("scan_current").to_string(), current))
                                .child(row(Lucide::File, t!("scan_files").to_string(), format::count(files, &l)))
                                .child(row(Lucide::HardDrive, t!("scan_size").to_string(), format::size(bytes, &l))),
                        )
                        .child(
                            div()
                                .w(px(230.))
                                .flex_shrink_0()
                                .h_flex()
                                .gap_2()
                                .items_start()
                                .p_3()
                                .rounded_lg()
                                .bg(c.info.opacity(0.1))
                                .text_sm()
                                .child(Icon::new(Lucide::Info).flex_shrink_0().text_color(c.info))
                                // min_w_0 lets the text wrap inside the box instead of overflowing it
                                .child(div().flex_1().min_w_0().child(t!("scan_keep_using").to_string())),
                        ),
                )
                .child(Separator::horizontal())
                .child(div().child(Button::new("cancel-scan").outline().icon(Lucide::CircleX).label(t!("scan_cancel").to_string()).on_click(cx.listener(|this, _, _, cx| {
                    this.app.cancel_scan();
                    cx.notify();
                }))))
                .into_any_element(),
            self.stat_cards(Some(files), None, true, cx),
            card(c.border, c.bg)
                .child(div().font_weight(FontWeight::SEMIBOLD).child(t!("map_preview_title").to_string()))
                // The live map always tells blocks apart; advice colours need the finished scan.
                .child(div().h(px(220.)).overflow_hidden().child(self.tree_map(core, &p.live.snapshot(&target.to_string_lossy()), &target, self.mini_bounds.clone(), MapColors::Types, false, cx)))
                .child(div().text_xs().text_color(c.muted).child(t!("scan_live_caption").to_string()))
                .into_any_element(),
        ]
    }

    fn home_complete(&self, core: &Core, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let c = colors(cx);
        let rescan = core.root.clone().unwrap_or_else(platform::default_scan_root);
        vec![
            heading(Some(Icon::new(Lucide::CircleCheck).size_8().text_color(c.success).into_any_element()), t!("done_title").to_string(), t!("done_subtitle").to_string(), c.muted)
                .into_any_element(),
            card(c.border, c.bg)
                .child(self.drive_content(cx))
                .child(Separator::horizontal())
                .child(
                    div()
                        .h_flex()
                        .gap_2()
                        .child(Button::new("rescan").outline().small().icon(Lucide::RefreshCw).label(t!("scan_again").to_string()).on_click(cx.listener(move |this, _, _, cx| {
                            let _ = this.app.start_scan(rescan.clone());
                            cx.notify();
                        })))
                        .child(Button::new("pick").ghost().small().icon(Lucide::FolderOpen).label(t!("home_pick").to_string()).on_click(cx.listener(|this, _, _, cx| this.pick_folder(cx)))),
                )
                .into_any_element(),
            self.stat_cards(core.stats.map(|s| s.files), core.stats.map(|s| s.largest_file), false, cx),
            card(c.border, c.bg)
                .child(
                    div()
                        .h_flex()
                        .child(div().flex_1().font_weight(FontWeight::SEMIBOLD).child(t!("map_preview_title").to_string()))
                        .child(Button::new("view-map").link().small().label(t!("map_view_full").to_string()).icon(Lucide::ArrowRight).on_click(cx.listener(|this, _, _, cx| {
                            this.app.show(Screen::DiskMap);
                            cx.notify();
                        }))),
                )
                .child(div().h(px(220.)).overflow_hidden().child(self.map_view(core, self.mini_bounds.clone(), false, cx)))
                .child(div().text_xs().text_color(c.muted).child(t!("map_caption").to_string()))
                .into_any_element(),
            Button::new("find-cleanup")
                .primary()
                .large()
                .w_full()
                .icon(Lucide::Sparkles)
                .label(t!("find_cleanup").to_string())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.app.show(Screen::Cleanup);
                    cx.notify();
                }))
                .into_any_element(),
            self.system_tools_shortcut(cx),
            self.recent(core, cx),
        ]
    }

    fn home_interrupted(&self, core: &Core, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(i) = core.interrupted.clone() else { return Vec::new() };
        let c = colors(cx);
        let l = loc();
        let (title, text) = match &i.error {
            None => (t!("cancelled_title").to_string(), t!("cancelled_text").to_string()),
            Some(e) => (t!("failed_title").to_string(), e.clone()),
        };
        let stat = |icon: Lucide, value: String, label: String| {
            div()
                .h_flex()
                .flex_1()
                .gap_3()
                .child(Icon::new(icon).size_6().text_color(c.muted))
                .child(div().v_flex().child(div().text_xl().font_weight(FontWeight::BOLD).child(value)).child(div().text_sm().text_color(c.muted).child(label)))
        };
        let root = i.root.clone();
        vec![
            Alert::error("interrupted", text).title(title).icon(Lucide::CircleAlert).into_any_element(),
            card(c.border, c.bg)
                .child(div().font_weight(FontWeight::SEMIBOLD).child(t!("partial_title").to_string()))
                .child(
                    div()
                        .h_flex()
                        .gap_4()
                        .child(stat(Lucide::File, format::count(i.files, &l), t!("partial_files").to_string()))
                        .child(Separator::vertical())
                        .child(stat(Lucide::HardDrive, format::size(i.bytes, &l), t!("partial_size").to_string())),
                )
                .into_any_element(),
            Button::new("scan-again").primary().large().w_full().icon(Lucide::RefreshCw).label(t!("scan_again").to_string()).on_click(cx.listener(move |this, _, _, cx| {
                let _ = this.app.start_scan(root.clone());
                cx.notify();
            }))
            .into_any_element(),
            Button::new("back-home").outline().large().w_full().label(t!("back_home").to_string()).on_click(cx.listener(|this, _, _, cx| {
                this.app.dismiss_interrupted();
                cx.notify();
            }))
            .into_any_element(),
        ]
    }

    pub(super) fn pick_folder(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: None });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(p) = paths.into_iter().next()
            {
                let _ = this.update(cx, |v, cx| {
                    let _ = v.app.start_scan(p);
                    cx.notify();
                });
            }
        })
        .detach();
    }
}
