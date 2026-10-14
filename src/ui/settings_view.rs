use super::{AppView, apply_theme};
use crate::mcp;
use crate::settings::Theme;
use crate::state::Core;
use gpui_kit::component::input::Input;
use gpui_kit::component::switch::Switch;
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonGroup, ButtonVariants as _};
use gpui_kit::component::group_box::{GroupBox, GroupBoxVariants as _};
use gpui_kit::component::select::Select;
use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use rust_i18n::t;
use gpui_kit::component::scroll::ScrollableElement as _;

fn section(title: String) -> GroupBox {
    GroupBox::new().outline().title(div().font_weight(FontWeight::SEMIBOLD).child(title))
}

impl AppView {
    pub(super) fn settings_view(&self, core: &Core, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, border, danger) = (theme.muted_foreground, theme.border, theme.danger);
        let s = &core.settings;

        let allow_rows = s.allowlist.iter().enumerate().map(|(i, p)| {
            let p2 = p.clone();
            div()
                .h_flex()
                .gap_3()
                .px_3()
                .py_1()
                .rounded_md()
                .border_1()
                .border_color(border)
                .child(Icon::new(Lucide::Folder))
                .child(div().flex_1().truncate().child(p.display().to_string()))
                .child(Button::new(("allow-remove", i)).accessibility_label(t!("settings_remove").to_string()).ghost().small().icon(Lucide::X).tooltip(t!("settings_remove").to_string()).on_click(cx.listener(move |this, _, _, cx| {
                    this.app.update_settings(|s| s.allowlist.retain(|a| *a != p2));
                    cx.notify();
                })))
        });

        let port = s.mcp_port;
        div()
            .id("settings")
            .v_flex()
            .size_full()
            .overflow_y_scrollbar()
            .p_8()
            .gap_8()
            .max_w(px(820.))
            .mx_auto()
            .child(div().text_2xl().font_weight(FontWeight::BOLD).child(t!("nav_settings").to_string()))
            .child(section(t!("settings_language").to_string()).child(Select::new(&self.lang_select).w(px(260.))))
            .child(
                section(t!("settings_theme").to_string()).child(
                    ButtonGroup::new("theme")
                        .outline()
                        .child(Button::new("theme-system").icon(Lucide::Settings).label(t!("settings_system").to_string()).selected(s.theme == Theme::System))
                        .child(Button::new("theme-light").label(t!("theme_light").to_string()).selected(s.theme == Theme::Light))
                        .child(Button::new("theme-dark").label(t!("theme_dark").to_string()).selected(s.theme == Theme::Dark))
                        .on_click(cx.listener(|this, ix: &Vec<usize>, window, cx| {
                            let Some(i) = ix.first() else { return };
                            let th = [Theme::System, Theme::Light, Theme::Dark][*i].clone();
                            this.app.update_settings(|s| s.theme = th.clone());
                            apply_theme(&th, window, cx);
                            cx.notify();
                        })),
                ),
            )
            .child(
                section(t!("settings_allowlist").to_string())
                    .when(s.allowlist.is_empty(), |d| d.child(div().text_sm().text_color(muted).child(t!("settings_allowlist_empty").to_string())))
                    .children(allow_rows)
                    .child(div().child(Button::new("allow-add").icon(Lucide::Plus).label(t!("settings_add").to_string()).on_click(cx.listener(|this, _, _, cx| this.add_allowed(cx))))),
            )
            .child(
                section(t!("settings_mcp").to_string())
                    .child(
                        div().h_flex().gap_3().child(Icon::new(Lucide::Bot)).child(
                            Switch::new("mcp-enabled").checked(s.mcp_enabled).label(t!("settings_mcp_enable").to_string()).on_click(cx.listener(|this, checked: &bool, _, cx| {
                                let on = *checked;
                                this.app.update_settings(|s| s.mcp_enabled = on);
                                cx.notify();
                            })),
                        ),
                    )
                    .child(
                        div()
                            .h_flex()
                            .gap_2()
                            .child(t!("settings_port").to_string())
                            .child(div().w(px(110.)).child(Input::new(&self.port_input)))
                            .child(Button::new("port-save").label(t!("settings_save").to_string()).on_click(cx.listener(|this, _, window, cx| {
                                match this.port_input.read(cx).value().trim().parse::<u16>() {
                                    Ok(p) if p >= 1024 => {
                                        this.app.update_settings(|s| s.mcp_port = p);
                                        this.port_invalid = false;
                                    }
                                    _ => {
                                        let stored = this.app.core.lock().unwrap().settings.mcp_port.to_string();
                                        this.port_input.update(cx, |i, cx| i.set_value(stored, window, cx));
                                        this.port_invalid = true;
                                    }
                                }
                                cx.notify();
                            }))),
                    )
                    .when(self.port_invalid, |d| d.child(div().text_sm().text_color(danger).child(t!("settings_port_invalid").to_string())))
                    .child(div().child(Button::new("mcp-copy").icon(Lucide::Copy).label(t!("settings_copy").to_string()).on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(mcp::client_config(port)));
                    })))
                    .child(div().text_sm().text_color(muted).child(t!("settings_restart").to_string()))
                    .children(core.mcp_error.as_ref().map(|e| div().text_sm().text_color(danger).child(t!("settings_mcp_error", error = e).to_string()))),
            )
            .into_any_element()
    }

    fn add_allowed(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: None });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(p) = paths.into_iter().next()
            {
                let p = dunce::canonicalize(&p).unwrap_or(p);
                let _ = this.update(cx, |v, cx| {
                    v.app.update_settings(|s| {
                        if !s.allowlist.contains(&p) {
                            s.allowlist.push(p);
                        }
                    });
                    cx.notify();
                });
            }
        })
        .detach();
    }
}
