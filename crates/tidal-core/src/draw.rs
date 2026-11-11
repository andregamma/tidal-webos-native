use crate::geom::{Color, Rect};

/// One primitive. The renderer batches these into as few draw calls as it can.
#[derive(Clone, Debug)]
pub enum Cmd {
    /// Rounded rectangle (radius in logical px).
    Rect { rect: Rect, color: Color, radius: f32 },
    /// Text; `y` is the top of the line box.
    Text { x: f32, y: f32, size: f32, color: Color, text: String },
    /// Set (or clear) the scissor rectangle. Forces a batch flush.
    Clip(Option<Rect>),
    /// A loaded cover image (see `Images`), rounded like a rect. Breaks the batch.
    Image { rect: Rect, key: u64, radius: f32, alpha: f32 },
}

/// The per-frame output of the UI.
pub struct DrawList {
    pub cmds: Vec<Cmd>,
    /// Images the views asked for this frame as `(key, url)`; the platform layer drains them.
    pub requests: Vec<(u64, String)>,
    alpha: f32,
}

impl DrawList {
    pub fn new() -> Self {
        Self { cmds: Vec::with_capacity(2048), requests: Vec::new(), alpha: 1.0 }
    }

    pub fn clear(&mut self) {
        self.cmds.clear();
        self.requests.clear();
        self.alpha = 1.0;
    }

    /// Global opacity multiplier applied to everything pushed afterwards.
    pub fn set_alpha(&mut self, a: f32) {
        self.alpha = a.clamp(0.0, 1.0);
    }

    pub fn rect(&mut self, rect: Rect, color: Color, radius: f32) {
        let color = color.mul_alpha(self.alpha);
        if color.a <= 0.003 || rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        self.cmds.push(Cmd::Rect { rect, color, radius });
    }

    pub fn text(&mut self, x: f32, y: f32, size: f32, color: Color, text: &str) {
        let color = color.mul_alpha(self.alpha);
        if color.a <= 0.003 || text.is_empty() {
            return;
        }
        self.cmds.push(Cmd::Text { x, y, size, color, text: text.to_string() });
    }

    pub fn image(&mut self, rect: Rect, key: u64, radius: f32) {
        if self.alpha <= 0.003 || rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        self.cmds.push(Cmd::Image { rect, key, radius, alpha: self.alpha });
    }

    /// Ask the platform layer to load an image for `key`.
    pub fn want(&mut self, key: u64, url: String) {
        if !self.requests.iter().any(|(k, _)| *k == key) {
            self.requests.push((key, url));
        }
    }

    pub fn clip(&mut self, rect: Option<Rect>) {
        self.cmds.push(Cmd::Clip(rect));
    }
}

impl Default for DrawList {
    fn default() -> Self {
        Self::new()
    }
}
