//! First-start guide: welcome, Full Disk Access (macOS), what to scan first.

use super::{AppView, mascot};
use crate::platform;
use crate::state::{Core, Screen};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rust_i18n::t;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Step {
    Welcome,
    Access,
    Places,
}

pub(super) fn steps(mac: bool) -> &'static [Step] {
    if mac { &[Step::Welcome, Step::Access, Step::Places] } else { &[Step::Welcome, Step::Places] }
}

impl AppView {
    fn guide_steps() -> &'static [Step] {
        steps(platform::has_permission_settings())
    }

    pub(super) fn guide_step(&self) -> Option<Step> {
        let s = Self::guide_steps();
        self.guide.map(|i| s[i.min(s.len() - 1)])
    }

    /// Skip, "Not now": the guide is done and Home shows.
    pub(super) fn close_welcome(&mut self, cx: &mut Context<Self>) {
        self.guide = None;
        self.app.welcome_done();
        self.app.show(Screen::Home);
        cx.notify();
    }

    pub(super) fn welcome(&mut self, core: &Core, cx: &mut Context<Self>) -> Div {
        let all = Self::guide_steps();
        let ix = self.guide.unwrap_or(0).min(all.len() - 1);
        let theme = cx.theme();
        let (muted, success, primary, border) = (theme.muted_foreground, theme.success, theme.primary, theme.border);
        let mascot = self.welcome_mascot.get_or_insert_with(|| cx.new(mascot::Mascot::new)).clone();
        let next = |label: String, primary_button: bool, cx: &mut Context<Self>| {
            Button::new("welcome-next").label(label).map(|b| if primary_button { b.primary() } else { b.ghost() }).on_click(cx.listener(|this, _, _, cx| {
                this.guide = this.guide.map(|i| i + 1);
                cx.notify();
            }))
        };
        let heading = |s: String| div().text_2xl().font_weight(FontWeight::BOLD).child(s);
        let text = |s: String| div().text_color(muted).child(s);
        let body = match all[ix] {
            Step::Welcome => div()
                .v_flex()
                .items_center()
                .gap_4()
                .child(mascot)
                .child(heading(t!("welcome_title").to_string()))
                .child(div().v_flex().items_center().gap_1().child(text(t!("welcome_what").to_string())).child(text(t!("welcome_trash").to_string())))
                .child(next(t!("welcome_next").to_string(), true, cx)),
            Step::Access => {
                let allowed = platform::full_disk_access().unwrap_or(true);
                div()
                    .v_flex()
                    .gap_4()
                    .child(heading(t!("welcome_access_title").to_string()))
                    .child(text(t!("welcome_access_why").to_string()))
                    .child(div().font_weight(FontWeight::SEMIBOLD).text_color(if allowed { success } else { muted }).child(if allowed { t!("welcome_access_yes") } else { t!("welcome_access_no") }.to_string()))
                    .when(!allowed, |d| {
                        d.child(div().h_flex().child(Button::new("welcome-open").outline().label(t!("welcome_access_open").to_string()).on_click(|_, _, _| platform::open_permission_settings())))
                            .child(div().text_sm().text_color(muted).child(t!("welcome_access_restart").to_string()))
                    })
                    .child(div().h_flex().justify_end().child(if allowed { next(t!("welcome_next").to_string(), true, cx) } else { next(t!("welcome_continue_without").to_string(), false, cx) }))
            }
            Step::Places => div()
                .v_flex()
                .gap_4()
                .child(heading(t!("welcome_places_title").to_string()))
                .child(self.place_picker(core, cx))
                .child(div().h_flex().justify_end().child(Button::new("welcome-not-now").ghost().label(t!("welcome_not_now").to_string()).on_click(cx.listener(|this, _, _, cx| this.close_welcome(cx))))),
        };
        let dots = div().h_flex().gap_2().children((0..all.len()).map(|i| div().size(px(8.)).rounded_full().bg(if i == ix { primary } else { border })));
        div()
            .flex_1()
            .min_h_0()
            .v_flex()
            .items_center()
            .p_6()
            .child(
                div()
                    .w_full()
                    .max_w(px(640.))
                    .h_flex()
                    .justify_between()
                    .child(dots)
                    .child(Button::new("welcome-skip").ghost().small().label(t!("welcome_skip").to_string()).on_click(cx.listener(|this, _, _, cx| this.close_welcome(cx)))),
            )
            .child(div().flex_1().w_full().max_w(px(640.)).v_flex().justify_center().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::{Step, steps};

    #[::core::prelude::v1::test]
    fn the_access_step_is_only_on_macos() {
        assert_eq!(steps(true), [Step::Welcome, Step::Access, Step::Places]);
        assert_eq!(steps(false), [Step::Welcome, Step::Places]);
    }
}
