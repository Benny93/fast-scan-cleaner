use gpui_kit::*;
use std::sync::Arc;
use std::time::Duration;

/// Frames in assets/mascot/sprite.png: 224 px each (drawn at 112 pt) in a COLS × COLS grid, FRAME_MS each.
const FRAMES: usize = 64;
const COLS: usize = 8;
const FRAME_MS: u64 = 50;
const SIZE: f32 = 112.;

/// The animated hard drive with sunglasses in the top-right corner of the Home screen.
/// Lives next to the cached app view (see `Shell`), so a frame only redraws itself; it exists
/// only while Home is shown (dropping it stops the timer).
pub struct Mascot {
    frame: usize,
    sprite: Arc<Image>,
}

impl Mascot {
    pub fn new(cx: &mut Context<Self>) -> Self {
        if !cx.reduce_motion() {
            cx.spawn(async move |this, cx| loop {
                cx.background_executor().timer(Duration::from_millis(FRAME_MS)).await;
                let alive = this.update(cx, |m, cx| {
                    m.frame = (m.frame + 1) % FRAMES;
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            })
            .detach();
        }
        Mascot { frame: 0, sprite: Arc::new(Image::from_bytes(ImageFormat::Png, include_bytes!("../../assets/mascot/sprite.png").to_vec())) }
    }
}

impl Render for Mascot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .size(px(SIZE))
            .overflow_hidden()
            .child(
                img(self.sprite.clone())
                    .absolute()
                    .left(px(-((self.frame % COLS) as f32) * SIZE))
                    .top(px(-((self.frame / COLS) as f32) * SIZE))
                    .size(px(SIZE * COLS as f32)),
            )
    }
}
