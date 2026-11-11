//! Small drawing helpers shared by screens.

use crate::draw::DrawList;
use crate::geom::{Color, Rect};
use crate::images::ImgState;
use crate::text::TextMetrics;
use crate::Cx;

/// Cheap deterministic hash (splitmix64).
pub fn hash(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Placeholder cover art: a colour field plus a translucent disc.
pub fn art(out: &mut DrawList, rect: Rect, seed: u64, radius: f32) {
    let hue = (hash(seed) % 360) as f32;
    out.rect(rect, Color::from_hsl(hue, 0.55, 0.30), radius);
    let d = rect.w * 0.55;
    let disc = Rect::new(rect.x + rect.w * 0.40, rect.y + rect.h * 0.08, d, d);
    out.rect(disc, Color::from_hsl(hue + 35.0, 0.65, 0.52).with_alpha(0.55), d / 2.0);
    let dot = rect.w * 0.22;
    let small = Rect::new(rect.x + rect.w * 0.12, rect.y + rect.h * 0.62, dot, dot);
    out.rect(small, Color::from_hsl(hue - 30.0, 0.7, 0.62).with_alpha(0.6), dot / 2.0);
}

/// Colour derived from the same seed as [`art`], for backgrounds.
pub fn art_color(seed: u64, l: f32) -> Color {
    Color::from_hsl((hash(seed) % 360) as f32, 0.55, l)
}

/// Draw text centred inside `r`.
pub fn text_centered(
    out: &mut DrawList,
    m: &dyn TextMetrics,
    r: Rect,
    size: f32,
    color: Color,
    text: &str,
) {
    let w = m.text_width(text, size);
    out.text(r.x + (r.w - w) / 2.0, r.y + (r.h - size * 1.2) / 2.0, size, color, text);
}

/// Draw text so that its right edge is at `right`.
pub fn text_right(
    out: &mut DrawList,
    m: &dyn TextMetrics,
    right: f32,
    y: f32,
    size: f32,
    color: Color,
    text: &str,
) {
    let w = m.text_width(text, size);
    out.text(right - w, y, size, color, text);
}

/// `m:ss`
pub fn fmt_time(secs: f32) -> String {
    let s = secs.max(0.0) as u32;
    format!("{}:{:02}", s / 60, s % 60)
}

/// FNV-1a hash of a string (stable keys for images).
pub fn hash_str(s: &str) -> u64 {
    s.bytes().fold(14695981039346656037u64, |h, b| (h ^ b as u64).wrapping_mul(1099511628211))
}

/// Sizes the image CDN serves (`{px}x{px}`); a request is rounded up to the next one so a 4K
/// drawable never asks for an odd width. 1920 caps anything larger.
const ART_SIZES: [u32; 5] = [160, 320, 640, 1280, 1920];

/// Size to request for a cover authored at `base` logical px: the same artwork drawn on a 4K
/// drawable covers twice as many physical pixels, so ask for twice the pixels instead of
/// upscaling (which is what made covers the softest thing on screen).
fn art_px(base: u32, density: f32) -> u32 {
    let want = (base as f32 * density.max(1.0)).round() as u32;
    ART_SIZES.iter().copied().find(|s| *s >= want).unwrap_or(1920)
}

/// Cover image if loaded, placeholder art otherwise. `art` is a URL template
/// (`{px}` is replaced with the physical size); a missing image is requested from the platform.
pub fn cover(out: &mut DrawList, cx: &Cx, rect: Rect, seed: u64, art_tpl: Option<&str>, px: u32, radius: f32) {
    let Some(tpl) = art_tpl else {
        art(out, rect, seed, radius);
        return;
    };
    let px = art_px(px, cx.images.density());
    let key = hash_str(tpl) ^ (px as u64).wrapping_mul(0x9E37_79B9);
    match cx.images.state(key) {
        ImgState::Ready => out.image(rect, key, radius),
        ImgState::Missing => {
            out.want(key, tpl.replace("{px}", &px.to_string()));
            art(out, rect, seed, radius);
        }
        ImgState::Pending | ImgState::Failed => art(out, rect, seed, radius),
    }
}

/// Rounded label ("chip"); returns its width so the next one can be placed after it.
#[cfg(test)]
mod tests {
    use super::{art_px, cover};
    use crate::draw::DrawList;
    use crate::geom::Rect;
    use crate::images::{ImgState, Images};
    use crate::text::MonoMetrics;
    use crate::theme::Theme;

    struct AtDensity(f32);

    impl Images for AtDensity {
        fn state(&self, _key: u64) -> ImgState {
            ImgState::Missing
        }

        fn density(&self) -> f32 {
            self.0
        }
    }

    #[test]
    fn cover_requests_the_physical_size() {
        let theme = Theme::default();
        let images = AtDensity(2.0);
        let cx = crate::Cx { theme: &theme, m: &MonoMetrics, images: &images, time: 0.0 };
        let mut out = DrawList::new();
        cover(&mut out, &cx, Rect::new(0.0, 0.0, 320.0, 320.0), 1, Some("https://x/{px}x{px}.jpg"), 320, 0.0);
        assert_eq!(out.requests.len(), 1);
        assert!(out.requests[0].1.ends_with("640x640.jpg"), "{:?}", out.requests);
    }

    #[test]
    fn covers_are_requested_at_physical_size() {
        assert_eq!(art_px(320, 1.0), 320);
        assert_eq!(art_px(320, 2.0), 640);
        assert_eq!(art_px(160, 2.0), 320);
        assert_eq!(art_px(640, 2.0), 1280);
        // Rounded up to the next size the CDN serves, and capped at the largest.
        assert_eq!(art_px(320, 3.0), 1280);
        assert_eq!(art_px(640, 4.0), 1920);
    }
}

#[allow(clippy::too_many_arguments)]
pub fn chip(out: &mut DrawList, m: &dyn TextMetrics, x: f32, y: f32, text: &str, size: f32, bg: Color, fg: Color) -> f32 {
    let w = m.text_width(text, size) + 32.0;
    let h = size * 1.2 + 20.0;
    out.rect(Rect::new(x, y, w, h), bg, h / 2.0);
    out.text(x + 16.0, y + 10.0, size, fg, text);
    w
}

/// `44.1 kHz`, `96 kHz`.
pub fn fmt_khz(hz: u32) -> String {
    if hz % 1000 == 0 {
        format!("{} kHz", hz / 1000)
    } else {
        format!("{:.1} kHz", hz as f32 / 1000.0)
    }
}
