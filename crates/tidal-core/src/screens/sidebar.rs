use crate::anim::Smooth;
use crate::draw::DrawList;
use crate::geom::{Color, Rect};
use crate::images::ImgState;
use crate::{ui, Cx, H, W};

pub const COLLAPSED_W: f32 = 120.0;
pub const EXPANDED_W: f32 = 520.0;
const ITEMS_TOP: f32 = 300.0;
const ITEM_H: f32 = 84.0;
const ITEM_PITCH: f32 = 100.0;

/// Texture keys the platform layer uploads the brand assets under (`assets/logo`): the white
/// wordmark for the open rail, the white icon for the collapsed one. The core itself never
/// loads a file.
pub const LOGO_WIDE_KEY: u64 = 0x7469_6461_6c2d_7764;
pub const LOGO_ICON_KEY: u64 = 0x7469_6461_6c2d_6963;

/// Boxes the logos are fitted (aspect-kept) into, in logical px.
const LOGO_ICON_BOX: Rect = Rect::new(36.0, 110.0, 52.0, 52.0);
const LOGO_WIDE_BOX: Rect = Rect::new(36.0, 114.0, 420.0, 44.0);

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
        self.logo(out, cx, e);

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

    /// Wordmark when the rail is open, icon when it is collapsed, cross-faded by `e`. Falls back
    /// to a text wordmark when the platform has not uploaded the textures (headless tests, a
    /// screenshot of a build without the assets).
    fn logo(&self, out: &mut DrawList, cx: &Cx, e: f32) {
        let icon = fit(LOGO_ICON_BOX, cx.images, LOGO_ICON_KEY);
        let wide = fit(LOGO_WIDE_BOX, cx.images, LOGO_WIDE_KEY);
        if icon.is_none() && wide.is_none() {
            out.text(36.0, 110.0, 40.0, cx.theme.accent, "TIDAL");
            return;
        }
        if let Some(r) = wide {
            out.set_alpha(e);
            out.image(r, LOGO_WIDE_KEY, 0.0);
        }
        if let Some(r) = icon {
            out.set_alpha(1.0 - e);
            out.image(r, LOGO_ICON_KEY, 0.0);
        }
        out.set_alpha(1.0);
    }
}

/// Keep the image's aspect ratio inside `box_` and centre it there. `None` until the texture is
/// on the GPU (the caller then draws its fallback instead of a hole).
fn fit(box_: Rect, images: &dyn crate::images::Images, key: u64) -> Option<Rect> {
    if images.state(key) != ImgState::Ready {
        return None;
    }
    let (w, h) = images.size(key)?;
    if w == 0 || h == 0 {
        return None;
    }
    let s = (box_.w / w as f32).min(box_.h / h as f32);
    let (dw, dh) = (w as f32 * s, h as f32 * s);
    Some(Rect::new(box_.x + (box_.w - dw) / 2.0, box_.y + (box_.h - dh) / 2.0, dw, dh))
}

impl Default for Sidebar {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::images::{ImgState, Images};

    /// Fixed answers from the platform layer, without a GPU.
    struct Stub(ImgState, Option<(u32, u32)>);

    impl Images for Stub {
        fn state(&self, _key: u64) -> ImgState {
            self.0
        }

        fn size(&self, _key: u64) -> Option<(u32, u32)> {
            self.1
        }
    }

    #[test]
    fn logo_keeps_its_ratio_inside_the_box() {
        // The wordmark asset is 753x100: it is height-bound in its box.
        let r = fit(LOGO_WIDE_BOX, &Stub(ImgState::Ready, Some((753, 100))), LOGO_WIDE_KEY).unwrap();
        assert!((r.h - LOGO_WIDE_BOX.h).abs() < 0.01, "{r:?}");
        assert!((r.w - 753.0 / 100.0 * LOGO_WIDE_BOX.h).abs() < 0.01, "{r:?}");
        assert!(r.x >= LOGO_WIDE_BOX.x && r.right() <= LOGO_WIDE_BOX.right(), "{r:?}");
    }

    #[test]
    fn logo_is_absent_until_the_texture_is_there() {
        assert!(fit(LOGO_ICON_BOX, &Stub(ImgState::Missing, None), LOGO_ICON_KEY).is_none());
        assert!(fit(LOGO_ICON_BOX, &Stub(ImgState::Ready, None), LOGO_ICON_KEY).is_none());
    }
}
