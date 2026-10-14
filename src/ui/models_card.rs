//! "Local AI models" on the Cleanup page: Ollama models and the Hugging Face cache.

use super::a11y::item_label;
use super::{AppView, card, loc};
use crate::actions::Origin;
use crate::format;
use crate::finders::models::{Model, Source};
use crate::platform;
use crate::state::{Core, Models, now_secs};
use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::*;
use gpui_kit::*;
use rust_i18n::t;
use std::path::PathBuf;

/// "Downloaded 5 days ago" below 60 days, then months below two years, then years.
pub(super) fn downloaded_text(at: u64, now: u64) -> String {
    let days = now.saturating_sub(at) / 86_400;
    match days {
        0..60 => t!("models_downloaded_days", n = days.to_string()),
        60..730 => t!("models_downloaded_months", n = (days / 30).to_string()),
        _ => t!("models_downloaded_years", n = (days / 365).to_string()),
    }
    .to_string()
}

fn source_text(s: Source) -> String {
    t!(match s {
        Source::Ollama => "models_ollama",
        Source::HuggingFace => "models_hf",
        Source::HfDataset => "models_hf_dataset",
    })
    .to_string()
}

impl AppView {
    pub(super) fn models_card(&self, core: &Core, cx: &mut Context<Self>) -> Option<AnyElement> {
        let Models::Done(models) = &core.models else { return None };
        if models.is_empty() {
            return None;
        }
        let t = cx.theme();
        let (muted, border, bg) = (t.muted_foreground, t.border, t.background);
        let l = loc();
        let now = now_secs();
        let key = |m: &Model| m.paths.first().cloned().unwrap_or_default();
        let mut list = div().v_flex();
        for (i, m) in models.iter().enumerate() {
            let (pick, reveal) = (key(m), key(m));
            let mut sub = source_text(m.source);
            if let Some(at) = m.downloaded {
                sub = format!("{sub} · {}", downloaded_text(at, now));
            }
            if m.source == Source::Ollama && m.size == 0 {
                sub = format!("{sub} · {}", t!("models_shared"));
            }
            list = list.child(
                div()
                    .h_flex()
                    .gap_3()
                    .py_1()
                    .child(Checkbox::new(("model-pick", i)).accessibility_label(item_label(&m.name, &format::size(m.size, &l))).checked(self.cleanup.models_ticked.contains(&pick)).on_click(cx.listener(move |this, on: &bool, _, cx| {
                        super::tick(&mut this.cleanup.models_ticked, &pick, *on);
                        cx.notify();
                    })))
                    .child(div().v_flex().flex_1().min_w_0().child(div().truncate().font_weight(FontWeight::MEDIUM).child(m.name.clone())).child(div().text_xs().text_color(muted).child(sub)))
                    .child(Button::new(("model-reveal", i)).accessibility_label(t!("details_show").to_string()).ghost().xsmall().icon(Lucide::FolderOpen).tooltip(t!("details_show").to_string()).on_click(move |_, _, _| platform::reveal(&reveal)))
                    .child(div().w(px(90.)).flex_none().text_right().font_weight(FontWeight::SEMIBOLD).child(format::size(m.size, &l))),
            );
        }
        let picked: Vec<Model> = models.iter().filter(|m| self.cleanup.models_ticked.contains(&key(m))).cloned().collect();
        let size: u64 = picked.iter().map(|m| m.size).sum();
        let body = div()
            .v_flex()
            .gap_3()
            .child(div().text_sm().text_color(muted).child(t!("models_intro").to_string()))
            .child(list)
            .child(div().h_flex().justify_end().child(Button::new("models-trash").danger().icon(Lucide::Trash).label(t!("models_trash", size = format::size(size, &l)).to_string()).disabled(picked.is_empty() || core.pending.is_some()).on_click(cx.listener(move |this, _, _, cx| {
                // Re-read now: a model pulled since the listing may share an Ollama blob.
                let paths: Vec<PathBuf> = picked.iter().flat_map(crate::finders::models::trash_paths).collect();
                let _ = this.app.request_trash(paths, None, Origin::User);
                cx.notify();
            }))));
        let title = div().text_lg().font_weight(FontWeight::SEMIBOLD).child(t!("models_title").to_string());
        Some(card(border, bg).child(title).child(body).into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::downloaded_text;

    #[::core::prelude::v1::test]
    fn downloaded_text_steps_from_days_to_years() {
        rust_i18n::set_locale("en");
        let day = 86_400;
        assert_eq!(downloaded_text(100 * day - 5 * day, 100 * day), "Downloaded 5 days ago");
        assert_eq!(downloaded_text(0, 90 * day), "Downloaded 3 months ago");
        assert_eq!(downloaded_text(0, 800 * day), "Downloaded 2 years ago");
    }
}
