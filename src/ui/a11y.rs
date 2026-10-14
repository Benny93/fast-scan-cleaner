//! Keyboard and screen-reader support shared by the screens.

use super::AppView;
use gpui_kit::*;
use rust_i18n::t;

/// "Old screenshots, 5.2 MB": the spoken name of a row or checkbox.
pub(crate) fn item_label(name: &str, size: &str) -> String {
    t!("a11y_item", name = name, size = size).to_string()
}

/// Spoken summary of the treemap: name, total, the biggest children.
pub(crate) fn map_summary(name: &str, total: &str, biggest: &[(String, String)]) -> String {
    let list = biggest.iter().map(|(n, s)| format!("{n} {s}")).collect::<Vec<_>>().join(", ");
    t!("a11y_map_summary", name = name, size = total, list = list).to_string()
}

/// A custom clickable area as a keyboard- and screen-reader-usable button. GPUI turns
/// Enter/Space on the focused element into its `on_click`.
pub(crate) trait Pressable: Sized {
    fn a11y_press(self, focus: &FocusHandle, label: impl Into<SharedString>, ring: Hsla) -> Self;
}

impl Pressable for Stateful<Div> {
    fn a11y_press(self, focus: &FocusHandle, label: impl Into<SharedString>, ring: Hsla) -> Self {
        // A 2 px ring drawn as a spread shadow: no layout shift, visible even over a canvas
        // (the mini map) and distinct from the tree's "current row" tint.
        let ring = BoxShadow { color: ring, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(2.), inset: false };
        self.track_focus(focus).role(Role::Button).aria_label(label).focus_visible(move |s| s.shadow(vec![ring]))
    }
}

/// The treemap's spoken summary for what is shown (the zoomed folder).
pub(crate) fn map_summary_of(core: &crate::state::Core) -> String {
    let Some(tree) = core.tree.as_ref() else { return String::new() };
    let node = tree.find(&core.zoom).unwrap_or(tree);
    let l = super::loc();
    let big: Vec<(String, String)> = node.children.iter().take(3).map(|c| (c.name.to_string(), crate::format::size(c.size, &l))).collect();
    let name = if core.zoom.as_os_str().is_empty() { core.root.as_ref().and_then(|r| r.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() } else { node.name.to_string() };
    map_summary(&name, &crate::format::size(node.size, &l), &big)
}

impl AppView {
    /// The same focus handle for the same key on every render (Tab order and focus survive).
    pub(super) fn focus_for(&self, key: impl Into<SharedString>, cx: &mut App) -> FocusHandle {
        self.focus_handles.borrow_mut().entry(key.into()).or_insert_with(|| cx.focus_handle().tab_stop(true)).clone()
    }
}
#[cfg(test)]
mod tests {
    use super::{item_label, map_summary};

    #[::core::prelude::v1::test]
    fn labels_read_naturally() {
        assert_eq!(item_label("Old screenshots", "5.2 MB"), "Old screenshots, 5.2 MB");
        let big = [("Videos".to_string(), "691 MB".to_string()), ("Photos".to_string(), "679 MB".to_string())];
        assert_eq!(map_summary("Demo", "3.0 GB", &big), "Disk map of Demo: 3.0 GB. Biggest: Videos 691 MB, Photos 679 MB.");
    }
}
