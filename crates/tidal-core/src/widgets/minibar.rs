//! The persistent "now playing" bar.
//!
//! Independent of every page: it is fed a [`NowPlaying`] snapshot, returns a [`BarAction`] for the
//! keys it handles, and knows nothing about routes, the backend or the player thread. `App` decides
//! when it is available, draws it above the page, and acts on the actions. A new page gets the bar
//! for free; it only has to keep [`RESERVED`] pixels clear at the bottom (see
//! `HomeScreen::set_bottom_inset`) and may return `Action::FocusBar` when Down runs off its end.
//!
//! Layout (modelled on the desktop client): cover, title/artist, play-pause, progress with times,
//! the quality chip and, on the right, a chevron that opens the full player.

use crate::anim::Smooth;
use crate::draw::DrawList;
use crate::geom::{Color, Rect};
use crate::input::Key;
use crate::model::NowPlaying;
use crate::quality::Quality;
use crate::screens::sidebar::COLLAPSED_W;
use crate::text::fit_text;
use crate::theme::Theme;
use crate::{ui, Cx, H, W};

pub const HEIGHT: f32 = 120.0;
const MARGIN_BOTTOM: f32 = 28.0;
const MARGIN_RIGHT: f32 = 48.0;
/// Bottom space (px) a page must keep free so the bar never covers its content.
pub const RESERVED: f32 = HEIGHT + MARGIN_BOTTOM + 16.0;

const BTN: f32 = 72.0;
const TEXT_W: f32 = 440.0;
const BTN_PLAY: usize = 0;
const BTN_OPEN: usize = 1;

/// What the bar asks `App` to do.
#[derive(Debug, PartialEq, Eq)]
pub enum BarAction {
    None,
    TogglePlay,
    /// Open the full player.
    OpenPlayer,
    /// Give focus back to the page.
    Leave,
}

pub struct MiniBar {
    /// 0 = hidden (slid down), 1 = shown.
    shown: Smooth,
    sel: usize,
}

impl MiniBar {
    pub fn new() -> Self {
        Self { shown: Smooth::new(0.0).with_rate(12.0), sel: BTN_PLAY }
    }

    /// Where the bar sits when fully shown (right of the collapsed sidebar, bottom of the screen).
    pub fn rect() -> Rect {
        let x = COLLAPSED_W + 24.0;
        Rect::new(x, H - MARGIN_BOTTOM - HEIGHT, W - MARGIN_RIGHT - x, HEIGHT)
    }

    /// `active` = the bar should be on screen (something is loaded and no full-screen page hides it).
    pub fn update(&mut self, dt: f32, active: bool) {
        self.shown.set(if active { 1.0 } else { 0.0 });
        self.shown.update(dt);
    }

    pub fn is_shown(&self) -> bool {
        self.shown.target > 0.5
    }

    /// Called when the bar takes focus: start on play/pause.
    pub fn focus(&mut self) {
        self.sel = BTN_PLAY;
    }

    pub fn on_key(&mut self, k: Key) -> BarAction {
        match k {
            Key::Left => self.sel = BTN_PLAY,
            Key::Right => self.sel = BTN_OPEN,
            Key::Up | Key::Back => return BarAction::Leave,
            Key::PlayPause => return BarAction::TogglePlay,
            Key::Ok => {
                return if self.sel == BTN_OPEN { BarAction::OpenPlayer } else { BarAction::TogglePlay };
            }
            _ => {}
        }
        BarAction::None
    }

    fn play_rect(r: Rect) -> Rect {
        Rect::new(r.x + 620.0, r.y + (r.h - BTN) / 2.0, BTN, BTN)
    }

    fn open_rect(r: Rect) -> Rect {
        Rect::new(r.right() - 20.0 - BTN, r.y + (r.h - BTN) / 2.0, BTN, BTN)
    }

    fn button(out: &mut DrawList, t: &Theme, rect: Rect, focused: bool) {
        if focused {
            out.rect(rect.inset(-6.0), t.accent, rect.w / 2.0 + 6.0);
        }
        out.rect(rect, t.surface_focus, rect.w / 2.0);
    }

    pub fn view(&self, out: &mut DrawList, cx: &Cx, np: &NowPlaying, focused: bool) {
        let a = self.shown.value;
        if a < 0.01 {
            return;
        }
        let t = cx.theme;
        let r = Self::rect().translate(0.0, (1.0 - a) * (HEIGHT + MARGIN_BOTTOM + 40.0));
        let cy = r.y + r.h / 2.0;
        out.set_alpha(a);

        out.rect(r.translate(0.0, 10.0).inset(-6.0), Color::BLACK.with_alpha(0.35), 38.0);
        out.rect(r, t.surface, 32.0);

        // Cover + text.
        ui::cover(out, cx, Rect::new(r.x + 16.0, r.y + 16.0, 88.0, 88.0), np.track.id, np.track.art.as_deref(), 160, 14.0);
        let title = fit_text(cx.m, &np.track.title, 30.0, TEXT_W);
        out.text(r.x + 128.0, cy - 40.0, 30.0, t.text, &title);
        let artist = fit_text(cx.m, &np.track.artist, 24.0, TEXT_W);
        out.text(r.x + 128.0, cy + 6.0, 24.0, t.text_dim, &artist);

        // Play / pause (shapes, so no font dependency).
        let play = Self::play_rect(r);
        Self::button(out, t, play, focused && self.sel == BTN_PLAY);
        if np.playing {
            out.rect(Rect::new(play.x + 24.0, play.y + 22.0, 8.0, 28.0), t.text, 3.0);
            out.rect(Rect::new(play.x + 40.0, play.y + 22.0, 8.0, 28.0), t.text, 3.0);
        } else {
            ui::text_centered(out, cx.m, play, 30.0, t.text, "▶");
        }

        // Progress with elapsed / total time.
        let (x0, x1) = (r.x + 736.0, r.x + 1360.0);
        out.text(x0, cy - 13.0, 22.0, t.text_dim, &ui::fmt_time(np.pos));
        ui::text_right(out, cx.m, x1, cy - 13.0, 22.0, t.text_dim, &ui::fmt_time(np.track.duration));
        let (bx, bw) = (x0 + 72.0, (x1 - x0) - 144.0);
        out.rect(Rect::new(bx, cy - 3.0, bw, 6.0), t.surface_focus, 3.0);
        let frac = if np.track.duration > 0.0 { (np.pos / np.track.duration).clamp(0.0, 1.0) } else { 0.0 };
        out.rect(Rect::new(bx, cy - 3.0, bw * frac, 6.0), t.accent, 3.0);

        // Open the full player.
        let open = Self::open_rect(r);
        Self::button(out, t, open, focused && self.sel == BTN_OPEN);
        ui::text_centered(out, cx.m, open, 44.0, t.text, "⌃");

        // Quality chip, left of the open button: the stream's own specs, coloured by its tier.
        if let Some(label) = np.quality.and_then(Quality::specs) {
            let (bg, fg) = np
                .quality
                .and_then(Quality::of_stream)
                .map_or((t.surface_focus, t.text_dim), |q| (q.chip_bg(), q.color()));
            let w = cx.m.text_width(&label, 22.0) + 32.0;
            let x = open.x - 24.0 - w;
            ui::chip(out, cx.m, x, cy - (22.0 * 1.2 + 20.0) / 2.0, &label, 22.0, bg, fg);
        }

        out.set_alpha(1.0);
    }
}

impl Default for MiniBar {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw::Cmd;
    use crate::images::NoImages;
    use crate::model::Track;
    use crate::services::StreamInfo;
    use crate::text::MonoMetrics;
    use crate::theme::Theme;

    fn track() -> Track {
        Track { id: 1, title: "Birds of a Feather".into(), artist: "Billie Eilish".into(), album: "HMHAS".into(), duration: 210.0, art: None }
    }

    fn shown_bar() -> MiniBar {
        let mut b = MiniBar::new();
        for _ in 0..120 {
            b.update(1.0 / 60.0, true);
        }
        b
    }

    fn texts(list: &DrawList) -> Vec<String> {
        list.cmds
            .iter()
            .filter_map(|c| if let Cmd::Text { text, .. } = c { Some(text.clone()) } else { None })
            .collect()
    }

    #[test]
    fn keys_move_between_buttons_and_report_actions() {
        let mut b = MiniBar::new();
        b.focus();
        assert_eq!(b.on_key(Key::Ok), BarAction::TogglePlay);
        assert_eq!(b.on_key(Key::Right), BarAction::None);
        assert_eq!(b.on_key(Key::Ok), BarAction::OpenPlayer);
        assert_eq!(b.on_key(Key::Left), BarAction::None);
        assert_eq!(b.on_key(Key::Ok), BarAction::TogglePlay);
        assert_eq!(b.on_key(Key::PlayPause), BarAction::TogglePlay);
        assert_eq!(b.on_key(Key::Up), BarAction::Leave);
        assert_eq!(b.on_key(Key::Back), BarAction::Leave);
        // Down must not open anything: it stays inert.
        assert_eq!(b.on_key(Key::Down), BarAction::None);
    }

    #[test]
    fn focus_always_starts_on_play() {
        let mut b = MiniBar::new();
        b.on_key(Key::Right);
        b.focus();
        assert_eq!(b.on_key(Key::Ok), BarAction::TogglePlay);
    }

    #[test]
    fn fits_on_screen_and_reserved_space_covers_it() {
        let r = MiniBar::rect();
        assert!(r.x >= COLLAPSED_W && r.right() <= W && r.bottom() <= H);
        assert!(RESERVED >= H - r.y);
    }

    #[test]
    fn hidden_bar_draws_nothing_and_shown_bar_draws_track_info() {
        let theme = Theme::default();
        let cx = Cx { theme: &theme, m: &MonoMetrics, images: &NoImages, time: 0.0 };
        let t = track();
        let np = NowPlaying { track: &t, pos: 69.0, playing: true, quality: None };
        let mut list = DrawList::new();

        MiniBar::new().view(&mut list, &cx, &np, false);
        assert!(list.cmds.is_empty());

        shown_bar().view(&mut list, &cx, &np, true);
        let txt = texts(&list);
        assert!(txt.contains(&"Birds of a Feather".to_string()));
        assert!(txt.contains(&"Billie Eilish".to_string()));
        assert!(txt.contains(&"1:09".to_string()) && txt.contains(&"3:30".to_string()));
        assert!(txt.contains(&"⌃".to_string()), "open-player chevron");
    }

    #[test]
    fn quality_chip_shows_the_stream_specs_in_the_tier_colour() {
        // The same specs and colours the full player uses (see `quality::Quality`).
        let mut i = StreamInfo { codec: "FLAC".into(), src_rate: 44_100, src_bits: Some(24), tier: Some("HI_RES".into()), ..Default::default() };
        assert_eq!(Quality::specs(&i).as_deref(), Some("FLAC · 24-bit / 44.1 kHz"));
        assert_eq!(Quality::of_stream(&i), Some(Quality::Max));
        i.src_bits = None;
        i.codec = "AAC".into();
        i.tier = Some("HIGH".into());
        assert_eq!(Quality::specs(&i).as_deref(), Some("AAC · 44.1 kHz"));
        assert_eq!(Quality::of_stream(&i), Some(Quality::High));
    }
}
