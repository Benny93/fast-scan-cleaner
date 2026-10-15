use super::AppView;
use crate::state::Core;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::Button;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::*;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::*;
use rust_i18n::t;

const HOMEPAGE: &str = "https://github.com/Benny93/fast-scan-cleaner";

impl AppView {
    pub(super) fn about_view(&self, _core: &Core, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        div()
            .id("about")
            .v_flex()
            .size_full()
            .overflow_y_scrollbar()
            .p_8()
            .pt_16()
            .items_center()
            .gap_2()
            .child(
                div()
                    .size(px(72.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_2xl()
                    .bg(theme.primary.opacity(0.12))
                    .child(Icon::new(Lucide::HardDrive).size_8().text_color(theme.primary)),
            )
            .child(div().text_3xl().font_weight(FontWeight::BOLD).child(t!("app_title").to_string()))
            .child(Tag::primary().small().rounded_full().child(t!("about_version", version = env!("CARGO_PKG_VERSION")).to_string()))
            .child(div().text_color(muted).child(t!("home_tagline").to_string()))
            .child(div().mt_6().text_sm().text_color(muted).child(t!("about_license").to_string()))
            .child(
                Button::new("about-home")
                    .outline()
                    .icon(Lucide::ExternalLink)
                    .label(t!("about_home").to_string())
                    .on_click(|_, _, _| {
                        let _ = opener::open(HOMEPAGE);
                    }),
            )
            .into_any_element()
    }
}
