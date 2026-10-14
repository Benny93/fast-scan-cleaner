//! "Old files" on the Cleanup page: old screenshots, installers and downloads.

use super::a11y::{Pressable as _, item_label};
use super::{AppView, card, loc};
use crate::actions::Origin;
use crate::format;
use crate::finders::oldfiles::{Group, OldFile};
use crate::platform;
use crate::state::{Core, OldFiles, now_secs};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::*;
use gpui_kit::*;
use rust_i18n::t;
use std::path::PathBuf;

pub(super) fn group_title(g: Group) -> String {
    t!(match g {
        Group::Screenshot => "old_group_screenshot",
        Group::Installer => "old_group_installer",
        Group::Download => "old_group_download",
    })
    .to_string()
}

/// "Last used 40 days ago" below 60 days, then months below two years, then years.
pub(super) fn used_text(last_used: u64, now: u64) -> String {
    let days = now.saturating_sub(last_used) / 86_400;
    match days {
        0..60 => t!("old_used_days", n = days.to_string()),
        60..730 => t!("old_used_months", n = (days / 30).to_string()),
        _ => t!("old_used_years", n = (days / 365).to_string()),
    }
    .to_string()
}

impl AppView {
    pub(super) fn old_files_card(&self, core: &Core, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = cx.theme();
        let (muted, border, bg) = (t.muted_foreground, t.border, t.background);
        let l = loc();
        let title = div().text_lg().font_weight(FontWeight::SEMIBOLD).child(t!("old_title").to_string());
        let body = match &core.old_files {
            OldFiles::Idle | OldFiles::Unavailable => return None,
            OldFiles::Loading => div().text_sm().text_color(muted).child(t!("old_loading").to_string()),
            OldFiles::Done(list) if list.is_empty() => div().text_sm().child(t!("old_none").to_string()),
            OldFiles::Done(list) => {
                let now = now_secs();
                let mut rows = div().v_flex();
                for (gi, g) in Group::ALL.into_iter().enumerate() {
                    let items: Vec<&OldFile> = list.iter().filter(|o| o.group == g).collect();
                    if items.is_empty() {
                        continue;
                    }
                    let paths: Vec<PathBuf> = items.iter().map(|o| o.path.clone()).collect();
                    let all = paths.iter().all(|p| self.cleanup.old_ticked.contains(p));
                    let open = self.cleanup.old_open.contains(&g);
                    let size: u64 = items.iter().map(|o| o.size).sum();
                    rows = rows.child(
                        div()
                            .h_flex()
                            .gap_3()
                            .py_1()
                            .child(Checkbox::new(("old-group", gi)).accessibility_label(item_label(&group_title(g), &format::size(size, &l))).checked(all).on_click(cx.listener(move |this, on: &bool, _, cx| {
                                for p in &paths {
                                    super::tick(&mut this.cleanup.old_ticked, p, *on);
                                }
                                cx.notify();
                            })))
                            .child(
                                div()
                                    .id(("old-open", gi))
                                    .h_flex()
                                    .gap_2()
                                    .flex_1()
                                    .min_w_0()
                                    .cursor_pointer()
                                    .a11y_press(&self.focus_for(format!("old-open-{gi}"), cx), item_label(&group_title(g), &format::size(size, &l)), cx.theme().ring)
                                    .child(Icon::new(if open { Lucide::ChevronDown } else { Lucide::ChevronRight }).size_4().text_color(muted))
                                    .child(div().font_weight(FontWeight::MEDIUM).child(format!("{} ({})", group_title(g), items.len())))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if !this.cleanup.old_open.remove(&g) {
                                            this.cleanup.old_open.insert(g);
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(div().w(px(90.)).flex_none().text_right().font_weight(FontWeight::SEMIBOLD).child(format::size(size, &l))),
                    );
                    if open {
                        let mut sub = div().v_flex().pl_8();
                        for (i, o) in items.iter().take(self.cleanup.old_shown).enumerate() {
                            let (pick, reveal) = (o.path.clone(), o.path.clone());
                            let name = o.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            let id = gi * 1_000_000 + i;
                            sub = sub.child(
                                div()
                                    .h_flex()
                                    .gap_3()
                                    .py_1()
                                    .child(Checkbox::new(("old-pick", id)).accessibility_label(name.clone()).checked(self.cleanup.old_ticked.contains(&o.path)).on_click(cx.listener(move |this, on: &bool, _, cx| {
                                        super::tick(&mut this.cleanup.old_ticked, &pick, *on);
                                        cx.notify();
                                    })))
                                    .child(
                                        div()
                                            .id(("old-reveal", id))
                                            .v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .cursor_pointer()
                                            .a11y_press(&self.focus_for(format!("old-reveal-{id}"), cx), name.clone(), cx.theme().ring)
                                            .child(div().truncate().child(name))
                                            .child(div().text_xs().text_color(muted).child(used_text(o.last_used, now)))
                                            .on_click(move |_, _, _| platform::reveal(&reveal)),
                                    )
                                    .child(div().w(px(90.)).flex_none().text_right().child(format::size(o.size, &l))),
                            );
                        }
                        let more = items.len().saturating_sub(self.cleanup.old_shown);
                        if more > 0 {
                            sub = sub.child(div().child(Button::new(("old-more", gi)).ghost().small().label(t!("cleanup_show_more", count = more.min(50).to_string()).to_string()).on_click(cx.listener(|this, _, _, cx| {
                                this.cleanup.old_shown += 50;
                                cx.notify();
                            }))));
                        }
                        rows = rows.child(sub);
                    }
                }
                let picked: Vec<&OldFile> = list.iter().filter(|o| self.cleanup.old_ticked.contains(&o.path)).collect();
                let size: u64 = picked.iter().map(|o| o.size).sum();
                let paths: Vec<PathBuf> = picked.iter().map(|o| o.path.clone()).collect();
                div().v_flex().gap_3().child(rows).child(div().h_flex().justify_end().child(
                    Button::new("old-trash").danger().icon(Lucide::Trash).label(t!("old_trash", size = format::size(size, &l)).to_string()).disabled(paths.is_empty() || core.pending.is_some()).on_click(cx.listener(move |this, _, _, cx| {
                        let _ = this.app.request_trash(paths.clone(), None, Origin::User);
                        cx.notify();
                    })),
                ))
            }
        };
        Some(card(border, bg).child(title).child(body).into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::used_text;

    #[::core::prelude::v1::test]
    fn last_used_reads_in_days_months_or_years() {
        const D: u64 = 86_400;
        let now = 1_000 * D;
        assert_eq!(used_text(now - 40 * D, now), "Last used 40 days ago");
        assert_eq!(used_text(now - 400 * D, now), "Last used 13 months ago");
        assert_eq!(used_text(now - 800 * D, now), "Last used 2 years ago");
    }
}
