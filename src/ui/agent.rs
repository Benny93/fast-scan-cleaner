//! Clicks, scrolling and keys an AI assistant sends over MCP, replayed as real window input.

use crate::state::Input;
use gpui_kit::*;

/// Replays one input in `window`; points are in the latest screenshot's pixels (`shot_width` wide).
pub(super) fn dispatch(input: Input, shot_width: u32, window: &mut Window, cx: &mut App) -> Result<(), String> {
    // Points are in the latest screenshot's pixels; scale them to window points.
    let w = f32::from(window.viewport_size().width);
    let s = if shot_width > 0 && w > 0. { shot_width as f32 / w } else { 1. };
    let at = |x: f32, y: f32| point(px(x / s), px(y / s));
    let modifiers = Modifiers::default();
    let mut send = |e: PlatformInput, window: &mut Window| {
        window.dispatch_event(e, cx);
    };
    match input {
        Input::Hover { x, y } => send(PlatformInput::MouseMove(MouseMoveEvent { position: at(x, y), pressed_button: None, modifiers }), window),
        Input::Click { x, y } => {
            let position = at(x, y);
            send(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers }), window);
            send(PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left, position, modifiers, click_count: 1, first_mouse: false }), window);
            send(PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position, modifiers, click_count: 1 }), window);
        }
        Input::Scroll { x, y, dy } => {
            let delta = ScrollDelta::Pixels(point(px(0.), px(-dy / s)));
            send(PlatformInput::ScrollWheel(ScrollWheelEvent { position: at(x, y), delta, modifiers, touch_phase: TouchPhase::Moved }), window);
        }
        Input::Text(text) => {
            for ch in text.chars() {
                let key = if ch == ' ' { "space".into() } else { ch.to_lowercase().to_string() };
                window.dispatch_keystroke(Keystroke { modifiers, key, key_char: Some(ch.to_string()) }, cx);
            }
        }
        Input::Key(k) => {
            let keystroke = Keystroke::parse(&k).map_err(|e| e.to_string())?;
            window.dispatch_keystroke(keystroke.clone(), cx);
            // A real key also goes up: GPUI turns Enter/Space on a focused element into a click on release.
            window.dispatch_event(PlatformInput::KeyUp(KeyUpEvent { keystroke }), cx);
        }
        Input::Reveal => {}
    }
    window.refresh();
    Ok(())
}
