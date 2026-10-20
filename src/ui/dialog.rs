use super::cleanup::{estimate_text, system_name};
use super::{AppView, loc};
use crate::finders::cleaners::Cleaner;
use crate::actions::Origin;
use crate::format;
use crate::state::Pending;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rust_i18n::t;
use gpui_kit::component::scroll::{Scrollbar, ScrollbarAxis, ScrollbarMode};

impl AppView {
    /// The same scroll handle for the same list on every render, so its scroll position survives.
    fn scroll_for(&self, key: &str) -> ScrollHandle {
        self.scroll_handles.borrow_mut().entry(key.into()).or_insert_with(ScrollHandle::default).clone()
    }

    /// A list capped at `max` px that always shows a vertical scrollbar once it overflows.
    ///
    /// `overflow_y_scrollbar()` copies the max-height onto its outer wrapper, so inside an
    /// auto-sized dialog the inner viewport never gets a definite height, the scroll handle sees
    /// no overflow, and the thumb is never drawn. Here the same element is both the bounded
    /// viewport and the scroll host, with an explicit `Always` scrollbar overlaid on top.
    /// The bar reads the handle in prepaint, so it can only appear on the second frame; the
    /// dialog schedules that frame when it opens (see `AppView::render`).
    fn bounded_scroll(&self, key: &str, max: f32, list: impl FnOnce(Stateful<Div>) -> Stateful<Div>) -> AnyElement {
        let handle = self.scroll_for(key);
        let list = list(div().id(key.to_string()).v_flex().gap_1());
        div()
            .relative()
            .child(list.max_h(px(max)).track_scroll(&handle).overflow_y_scroll())
            .child(div().absolute().inset_0().child(Scrollbar::new(&handle).id(format!("{key}-bar")).axis(ScrollbarAxis::Vertical).mode(ScrollbarMode::Always).viewport_from_layout()))
            .into_any_element()
    }

    pub(super) fn confirm_dialog(&self, p: &Pending, cx: &mut Context<Self>) -> AnyElement {
        if !p.system.is_empty() {
            return self.system_confirm_dialog(p, cx);
        }
        let l = loc();
        let theme = cx.theme();
        let (bg, border, muted, warning) = (theme.background, theme.border, theme.muted_foreground, theme.warning);
        let total: u64 = p.items.iter().filter_map(|(_, s)| *s).sum();
        let reason = p.reason.as_ref().map(|r| if r.chars().count() > 300 { format!("{}…", r.chars().take(300).collect::<String>()) } else { r.clone() });
        div()
            .id("confirm-overlay")
            .track_focus(&self.focus_for("confirm-dialog", cx))
            .role(Role::Dialog)
            .absolute()
            .inset_0()
            .occlude()
            .bg(hsla(0.0, 0.0, 0.0, 0.55))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .v_flex()
                    .w(px(560.))
                    .max_w(relative(0.9))
                    .p_6()
                    .gap_3()
                    .rounded_xl()
                    .bg(bg)
                    .border_1()
                    .border_color(border)
                    .shadow_lg()
                    .child(div().h_flex().gap_2().child(Icon::new(Lucide::TriangleAlert).text_color(warning)).child(div().text_xl().font_weight(FontWeight::BOLD).child(t!("confirm_title").to_string())))
                    .when(p.origin == Origin::Agent, |d| d.child(div().h_flex().gap_2().child(Tag::info().child(div().h_flex().gap_1().child(Icon::new(Lucide::Bot).size_3()).child(t!("ai_assistant").to_string()))).child(div().flex_1().min_w_0().child(t!("confirm_agent").to_string()))))
                    .children(reason.map(|r| div().italic().child(t!("confirm_reason", reason = r).to_string())))
                    .child(self.bounded_scroll("confirm-items", 280., |list| {
                        list.children(p.items.iter().map(|(path, size)| {
                            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.display().to_string());
                            let parent = path.parent().map(|d| d.display().to_string()).unwrap_or_default();
                            let size = size.map(|s| format::size(s, &l)).unwrap_or_else(|| t!("confirm_size_unknown").to_string());
                            div()
                                .h_flex()
                                .items_start()
                                .gap_3()
                                .child(Icon::new(if path.extension().is_some() { Lucide::File } else { Lucide::Folder }))
                                .child(div().w(px(110.)).flex_none().text_right().text_sm().font_weight(FontWeight::SEMIBOLD).child(size))
                                .child(
                                    div()
                                        .v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(div().font_weight(FontWeight::BOLD).child(name))
                                        .child(div().text_xs().text_color(muted).child(parent)),
                                )
                        }))
                    }))
                    .child(div().text_sm().text_color(muted).child(t!("confirm_restore").to_string()))
                    .child(
                        div()
                            .h_flex()
                            .justify_end()
                            .gap_2()
                            .child(Button::new("confirm-cancel").outline().label(t!("confirm_cancel").to_string()).on_click(cx.listener(|this, _, _, cx| {
                                this.app.resolve_pending(false);
                                cx.notify();
                            })))
                            .child(
                                Button::new("confirm-ok")
                                    .danger()
                                    .icon(Lucide::Trash)
                                    .label(if p.items.len() == 1 { t!("confirm_ok_one", size = format::size(total, &l)) } else { t!("confirm_ok", count = p.items.len().to_string(), size = format::size(total, &l)) }.to_string())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.app.resolve_pending(true);
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn system_confirm_dialog(&self, p: &Pending, cx: &mut Context<Self>) -> AnyElement {
        let req = &p.system[0];
        // Several requests = Git branches/worktrees picked in more than one repository.
        let many = p.system.len() > 1;
        let theme = cx.theme();
        let (bg, border, muted, danger) = (theme.background, theme.border, theme.muted_foreground, theme.danger);
        let git = p.system.iter().any(|r| r.cleaner == Cleaner::GitBranches);
        let title = if git { t!("confirm_sys_title_git") } else { t!("confirm_sys_title") }.to_string();
        let estimate = if many { String::new() } else { estimate_text(req.estimate) };
        let reason = p.reason.as_ref().map(|r| if r.chars().count() > 300 { format!("{}…", r.chars().take(300).collect::<String>()) } else { r.clone() });
        div()
            .id("confirm-overlay")
            .track_focus(&self.focus_for("confirm-dialog", cx))
            .role(Role::Dialog)
            .absolute()
            .inset_0()
            .occlude()
            .bg(hsla(0.0, 0.0, 0.0, 0.55))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .v_flex()
                    .w(px(560.))
                    .max_w(relative(0.9))
                    .p_6()
                    .gap_3()
                    .rounded_xl()
                    .bg(bg)
                    .border_1()
                    .border_color(border)
                    .shadow_lg()
                    .child(div().h_flex().gap_2().child(Icon::new(Lucide::TriangleAlert).text_color(danger)).child(div().text_xl().font_weight(FontWeight::BOLD).child(title)))
                    .when(!many, |d| {
                        d.child(div().font_weight(FontWeight::SEMIBOLD).child(system_name(req.cleaner, req.repo.as_deref())))
                            .child(div().text_sm().text_color(muted).child(t!(req.cleaner.advice_key()).to_string()))
                    })
                    .when(!estimate.is_empty(), |d| d.child(div().text_sm().child(estimate)))
                    .when(git, |d| d.child(div().text_sm().child(t!("confirm_sys_git_note").to_string())))
                    .when(p.origin == Origin::Agent, |d| d.child(div().h_flex().gap_2().child(Tag::info().child(div().h_flex().gap_1().child(Icon::new(Lucide::Bot).size_3()).child(t!("ai_assistant").to_string()))).child(div().flex_1().min_w_0().child(t!("confirm_agent").to_string()))))
                    .children(reason.map(|r| div().italic().child(t!("confirm_reason", reason = r).to_string())))
                    .child(self.bounded_scroll("confirm-sys-detail", 260., |list| {
                        list.children(p.system.iter().filter(|r| !r.detail.is_empty()).map(|r| {
                            div()
                                .v_flex()
                                .gap_1()
                                .pb_2()
                                .when(many, |d| d.child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(system_name(r.cleaner, r.repo.as_deref()))))
                                .children(r.detail.iter().map(|line| div().text_sm().child(line.clone())))
                        }))
                    }))
                    .child(
                        div()
                            .h_flex()
                            .justify_end()
                            .gap_2()
                            .child(Button::new("confirm-cancel").outline().label(t!("confirm_cancel").to_string()).on_click(cx.listener(|this, _, _, cx| {
                                this.app.resolve_pending(false);
                                cx.notify();
                            })))
                            .child(Button::new("confirm-sys-ok").danger().icon(Lucide::Trash).label(t!("confirm_sys_ok").to_string()).on_click(cx.listener(|this, _, _, cx| {
                                this.app.resolve_pending(true);
                                cx.notify();
                            }))),
                    ),
            )
            .into_any_element()
    }
}
