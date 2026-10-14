//! "Apps you haven't used" on the Cleanup page (macOS).

use super::a11y::{Pressable as _, item_label};
use super::{AppView, card, loc};
use crate::actions::Origin;
use crate::finders::apps::App;
use crate::format;
use crate::platform;
use crate::state::{Apps, Core, now_secs};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rust_i18n::t;
use std::path::{Path, PathBuf};

/// "Last opened 14 months ago".
pub(super) fn last_used_text(app: &App, now: u64) -> String {
    t!("apps_last_used", n = (now.saturating_sub(app.last_used) / (30 * 86_400)).to_string()).to_string()
}

/// "~/Library/Caches/com.example" for paths in the home folder.
fn display(p: &Path, home: &Path) -> String {
    p.strip_prefix(home).map_or_else(|_| p.display().to_string(), |r| format!("~/{}", r.display()))
}

impl AppView {
    pub(super) fn apps_card(&self, core: &Core, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = cx.theme();
        let (muted, border, bg) = (t.muted_foreground, t.border, t.background);
        let l = loc();
        let title = div().text_lg().font_weight(FontWeight::SEMIBOLD).child(t!("apps_title").to_string());
        let body = match &core.apps {
            Apps::Idle | Apps::Unavailable => return None,
            Apps::Loading => div().text_sm().text_color(muted).child(t!("apps_loading").to_string()),
            Apps::Done(apps) if apps.is_empty() => div().text_sm().child(t!("apps_none").to_string()),
            Apps::Done(apps) => {
                let now = now_secs();
                let mut list = div().v_flex();
                for (i, a) in apps.iter().enumerate() {
                    let open = self.cleanup.apps_open.contains(&a.path);
                    let (pick, toggle, reveal) = (a.path.clone(), a.path.clone(), a.path.clone());
                    let mut sub = last_used_text(a, now);
                    if a.running {
                        sub = format!("{sub} · {}", t!("apps_running"));
                    } else if !a.movable {
                        sub = format!("{sub} · {}", t!("apps_needs_admin"));
                    }
                    list = list.child(
                        div()
                            .h_flex()
                            .gap_3()
                            .py_1()
                            .child(Checkbox::new(("app-pick", i)).accessibility_label(item_label(&a.name, &format::size(a.size(), &l))).checked(a.removable() && self.cleanup.apps_ticked.contains(&a.path)).disabled(!a.removable()).on_click(cx.listener(move |this, on: &bool, _, cx| {
                                super::tick(&mut this.cleanup.apps_ticked, &pick, *on);
                                cx.notify();
                            })))
                            .child(
                                div()
                                    .id(("app-open", i))
                                    .h_flex()
                                    .gap_2()
                                    .flex_1()
                                    .min_w_0()
                                    .cursor_pointer()
                                    .a11y_press(&self.focus_for(format!("app-open-{i}"), cx), item_label(&a.name, &format::size(a.size(), &l)), cx.theme().ring)
                                    .child(Icon::new(if open { Lucide::ChevronDown } else { Lucide::ChevronRight }).size_4().text_color(muted))
                                    .child(div().v_flex().min_w_0().child(div().truncate().font_weight(FontWeight::MEDIUM).child(a.name.clone())).child(div().text_xs().text_color(muted).child(sub)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if !this.cleanup.apps_open.remove(&toggle) {
                                            this.cleanup.apps_open.insert(toggle.clone());
                                        }
                                        cx.notify();
                                    })),
                            )
                            .when(!a.movable, |d| d.child(Button::new(("app-reveal", i)).ghost().xsmall().icon(Lucide::FolderOpen).label(t!("apps_show_in_finder").to_string()).on_click(move |_, _, _| platform::reveal(&reveal))))
                            .child(div().w(px(90.)).flex_none().text_right().font_weight(FontWeight::SEMIBOLD).child(format::size(a.size(), &l))),
                    );
                    if open {
                        let rows = std::iter::once((a.path.clone(), a.own)).chain(a.leftovers.iter().cloned()).map(|(p, n)| {
                            div()
                                .h_flex()
                                .gap_3()
                                .pl_8()
                                .text_sm()
                                .text_color(muted)
                                .child(div().flex_1().min_w_0().truncate().child(display(&p, &self.app.deps.home)))
                                .child(div().w(px(90.)).flex_none().text_right().child(format::size(n, &l)))
                        });
                        list = list.child(div().v_flex().children(rows));
                    }
                }
                let picked: Vec<&App> = apps.iter().filter(|a| a.removable() && self.cleanup.apps_ticked.contains(&a.path)).collect();
                let size: u64 = picked.iter().map(|a| a.size()).sum();
                let paths: Vec<PathBuf> = picked.iter().flat_map(|a| a.paths()).collect();
                div()
                    .v_flex()
                    .gap_3()
                    .child(div().text_sm().text_color(muted).child(t!("apps_intro").to_string()))
                    .child(list)
                    .child(div().h_flex().justify_end().child(Button::new("apps-trash").danger().icon(Lucide::Trash).label(t!("apps_trash", size = format::size(size, &l)).to_string()).disabled(paths.is_empty() || core.pending.is_some()).on_click(cx.listener(move |this, _, _, cx| {
                        let _ = this.app.request_trash(paths.clone(), None, Origin::User);
                        cx.notify();
                    }))))
            }
        };
        Some(card(border, bg).child(title).child(body).into_any_element())
    }
}
