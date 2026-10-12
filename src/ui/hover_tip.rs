//! The treemap's hover label. A normal tooltip stays where it opened; this one follows the mouse.
//! It is its own view drawn by `Shell` over the cached app view, so a mouse move redraws only
//! this label, not the whole window.

use gpui_kit::component::*;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

const GAP: f32 = 14.;
const MAX_W: f32 = 380.;

#[derive(Default)]
pub struct HoverTip {
    /// Blocks under the mouse as (id, nesting depth, text); the deepest one's text is shown.
    /// GPUI reports hover changes of nested blocks in no fixed order, so the depth decides,
    /// and leaving an inner block shows its parent's text again.
    stack: Vec<(usize, u8, String)>,
    pos: Point<Pixels>,
}

impl HoverTip {
    pub fn enter(&mut self, id: usize, depth: u8, text: String, cx: &mut Context<Self>) {
        self.stack.retain(|(i, ..)| *i != id);
        self.stack.push((id, depth, text));
        cx.notify();
    }

    pub fn leave(&mut self, id: usize, cx: &mut Context<Self>) {
        self.stack.retain(|(i, ..)| *i != id);
        cx.notify();
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if !self.stack.is_empty() {
            self.stack.clear();
            cx.notify();
        }
    }

    pub fn move_to(&mut self, pos: Point<Pixels>, cx: &mut Context<Self>) {
        self.pos = pos;
        if !self.stack.is_empty() {
            cx.notify();
        }
    }
}

impl Render for HoverTip {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some((.., text)) = self.stack.iter().rev().max_by_key(|(_, depth, _)| *depth) else { return div() };
        let t = cx.theme();
        let view = window.viewport_size();
        let (x, y) = (f32::from(self.pos.x), f32::from(self.pos.y));
        let (vw, vh) = (f32::from(view.width), f32::from(view.height));
        // Below right of the pointer; flipped left / above near the window's right or bottom edge.
        let left = x + GAP + MAX_W < vw;
        let below = y < vh * 0.6;
        div()
            .absolute()
            .map(|d| if left { d.left(px(x + GAP)) } else { d.right(px(vw - x + GAP)) })
            .map(|d| if below { d.top(px(y + GAP)) } else { d.bottom(px(vh - y + GAP)) })
            .max_w(px(MAX_W))
            .px_3()
            .py_2()
            .rounded_md()
            .border_1()
            .border_color(t.border)
            .bg(t.popover)
            .text_color(t.popover_foreground)
            .text_sm()
            .shadow_md()
            .children(text.split('\n').map(|line| div().min_h(px(8.)).child(line.to_string())))
    }
}
