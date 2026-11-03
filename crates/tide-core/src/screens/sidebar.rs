use crate::anim::Smooth;
use crate::draw::DrawList;
use crate::geom::{Color, Rect};
use crate::{ui, Cx, H, W};

pub const COLLAPSED_W: f32 = 120.0;
pub const EXPANDED_W: f32 = 520.0;
const ITEMS_TOP: f32 = 300.0;
const ITEM_H: f32 = 84.0;
const ITEM_PITCH: f32 = 100.0;

pub struct Item {
    pub glyph: &'static str,
    pub label: &'static str,
}

/// Index of the entry that opens the full player.
pub const NOW_PLAYING: usize = 2;
/// Index of the entry that opens the Settings page.
pub const SETTINGS: usize = 3;

pub const ITEMS: [Item; 4] = [
    Item { glyph: "H", label: "Home" },
    Item { glyph: "♥", label: "Liked songs" },
    Item { glyph: "♪", label: "Playing now" },
    Item { glyph: "⚙", label: "Settings" },
];

/// Left rail. Collapsed = icons only; focused = expands as an overlay (content
/// does not re-layout, which keeps the expand animation cheap).
pub struct Sidebar {
    pub sel: usize,
    expand: Smooth,
    hl_y: Smooth,
}

impl Sidebar {
    pub fn new() -> Self {
        Self { sel: 0, expand: Smooth::new(0.0).with_rate(16.0), hl_y: Smooth::new(0.0) }
    }

    pub fn move_sel(&mut self, delta: i32) {
        let n = ITEMS.len() as i32;
        self.sel = (self.sel as i32 + delta).clamp(0, n - 1) as usize;
        self.hl_y.set(self.sel as f32 * ITEM_PITCH);
    }

    pub fn select(&mut self, i: usize) {
        self.sel = i.min(ITEMS.len() - 1);
        self.hl_y.snap(self.sel as f32 * ITEM_PITCH);
    }

    pub fn update(&mut self, dt: f32, focused: bool) {
        self.expand.set(if focused { 1.0 } else { 0.0 });
        self.expand.update(dt);
        self.hl_y.update(dt);
    }

    /// `current` = index of the route currently shown (accent icon).
    pub fn view(&self, out: &mut DrawList, cx: &Cx, current: usize) {
        let t = &cx.theme;
        let e = self.expand.value;
        let w = COLLAPSED_W + (EXPANDED_W - COLLAPSED_W) * e;

        if e > 0.01 {
            out.rect(Rect::new(0.0, 0.0, W, H), Color::BLACK.with_alpha(0.55 * e), 0.0);
        }
        out.clip(Some(Rect::new(0.0, 0.0, w, H)));
        out.rect(Rect::new(0.0, 0.0, w, H), t.sidebar_bg, 0.0);
        out.text(36.0, 110.0, 40.0, t.accent, "tide");

        if e > 0.01 {
            out.rect(
                Rect::new(20.0, ITEMS_TOP + self.hl_y.value, w - 40.0, ITEM_H),
                t.surface_focus.mul_alpha(e),
                24.0,
            );
        }
        for (i, item) in ITEMS.iter().enumerate() {
            let y = ITEMS_TOP + i as f32 * ITEM_PITCH;
            let icon = Rect::new(36.0, y + 16.0, 52.0, 52.0);
            let active = i == current;
            out.rect(icon, if active { t.accent } else { t.surface }, 16.0);
            ui::text_centered(out, cx.m, icon, 30.0, if active { t.bg } else { t.text_dim }, item.glyph);
            if e > 0.02 {
                out.text(112.0, y + 22.0, 34.0, t.text.mul_alpha(e), item.label);
            }
        }
        out.clip(None);
    }
}

impl Default for Sidebar {
    fn default() -> Self {
        Self::new()
    }
}
