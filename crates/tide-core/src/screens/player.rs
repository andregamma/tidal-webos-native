use super::Action;
use crate::anim::Smooth;
use crate::draw::DrawList;
use crate::geom::{Color, Rect};
use crate::input::Key;
use crate::model::{LyricLine, Track};
use crate::services::{PlayerCmd, StreamInfo};
use crate::text::fit_text;
use crate::{ui, Cx, H, W};

const LYRIC_PITCH: f32 = 86.0;
const LYRIC_X: f32 = 960.0;
const LYRIC_W: f32 = 860.0;

/// Now-playing screen with synced lyrics.
///
/// M0: playback position is *simulated* here (`pos += dt`). In M3 a player
/// thread (decoder + audio output) owns the clock and sends `Evt::Position`;
/// this struct then only mirrors it.
pub struct PlayerScreen {
    track: Option<Track>,
    pos: f32,
    playing: bool,
    /// `None` = still loading, `Some(empty)` = no lyrics.
    lyrics: Option<Vec<LyricLine>>,
    /// False when the lyrics have no usable timestamps (shown static, Up/Down scroll).
    synced: bool,
    lyric_scroll: Smooth,
    /// Measured format of the stream being played (None until the player reports it).
    info: Option<StreamInfo>,
}

impl PlayerScreen {
    pub fn new() -> Self {
        Self {
            track: None,
            pos: 0.0,
            playing: false,
            lyrics: None,
            synced: true,
            lyric_scroll: Smooth::new(0.0).with_rate(9.0),
            info: None,
        }
    }

    pub fn open(&mut self, track: Track) {
        self.track = Some(track);
        self.pos = 0.0;
        self.playing = true;
        self.lyrics = None;
        self.info = None;
        self.lyric_scroll.snap(0.0);
    }

    pub fn set_stream_info(&mut self, info: StreamInfo) {
        self.info = Some(info);
    }

    /// Authoritative position from the player.
    pub fn set_position(&mut self, secs: f32) {
        self.pos = secs;
    }

    pub fn set_playing(&mut self, playing: bool) {
        self.playing = playing;
    }

    pub fn on_ended(&mut self) {
        self.playing = false;
        self.pos = 0.0;
    }

    pub fn track_id(&self) -> Option<u64> {
        self.track.as_ref().map(|t| t.id)
    }

    pub fn set_lyrics(&mut self, track_id: u64, lines: Vec<LyricLine>, synced: bool) {
        if self.track_id() == Some(track_id) {
            self.lyrics = Some(lines);
            self.synced = synced;
            self.lyric_scroll.snap(0.0);
        }
    }

    fn active_line(&self) -> usize {
        match &self.lyrics {
            Some(l) if self.synced => l.partition_point(|x| x.t <= self.pos).saturating_sub(1),
            _ => 0,
        }
    }

    pub fn on_key(&mut self, k: Key) -> Action {
        let dur = self.track.as_ref().map_or(0.0, |t| t.duration);
        match k {
            Key::Ok | Key::PlayPause => {
                self.playing = !self.playing;
                return Action::Transport(if self.playing { PlayerCmd::Resume } else { PlayerCmd::Pause });
            }
            Key::Left | Key::Right | Key::Prev => {
                self.pos = match k {
                    Key::Left => (self.pos - 10.0).max(0.0),
                    Key::Right => (self.pos + 10.0).min(dur),
                    _ => 0.0,
                };
                return Action::Transport(PlayerCmd::Seek(self.pos));
            }
            Key::Up | Key::Down if !self.synced => {
                let n = self.lyrics.as_ref().map_or(0, |l| l.len()) as f32;
                let d = if k == Key::Down { 1.0 } else { -1.0 };
                self.lyric_scroll.set((self.lyric_scroll.target + d).clamp(0.0, (n - 6.0).max(0.0)));
            }
            Key::Back => return Action::Back,
            _ => {}
        }
        Action::None
    }

    pub fn update(&mut self, dt: f32) {
        let dur = self.track.as_ref().map_or(0.0, |t| t.duration);
        // Extrapolate between the player's position reports so the bar moves smoothly.
        if self.playing && dur > 0.0 {
            self.pos = (self.pos + dt).min(dur);
        }
        if self.synced {
            self.lyric_scroll.set(self.active_line() as f32);
        }
        self.lyric_scroll.update(dt);
    }

    pub fn view(&self, out: &mut DrawList, cx: &Cx) {
        let t = &cx.theme;
        let Some(track) = &self.track else { return };

        // Soft colour wash derived from the cover.
        out.rect(
            Rect::new(-300.0, -300.0, 1300.0, 1300.0),
            ui::art_color(track.id, 0.30).with_alpha(0.16),
            650.0,
        );

        // Cover + metadata.
        let cover = Rect::new(180.0, 140.0, 600.0, 600.0);
        ui::cover(out, cx, cover, track.id, track.art.as_deref(), 640, 28.0);
        let title = fit_text(cx.m, &track.title, 46.0, 640.0);
        out.text(180.0, 780.0, 46.0, t.text, &title);
        let sub = fit_text(cx.m, &format!("{} · {}", track.artist, track.album), 28.0, 640.0);
        out.text(180.0, 842.0, 28.0, t.text_dim, &sub);

        // Quality: the service's label (grey) and what the decoder actually got (accent),
        // then what the audio device was opened with.
        if let Some(i) = &self.info {
            let mut x = 180.0;
            if let Some(tier) = &i.tier {
                x += ui::chip(out, cx.m, x, 84.0, &tier.replace('_', " "), 22.0, t.surface_focus, t.text_dim) + 12.0;
            }
            let src = match i.src_bits {
                Some(b) => format!("{} · {}-bit / {}", i.codec, b, ui::fmt_khz(i.src_rate)),
                None => format!("{} · {}", i.codec, ui::fmt_khz(i.src_rate)),
            };
            ui::chip(out, cx.m, x, 84.0, &src, 22.0, t.accent.with_alpha(0.18), t.accent);
            let same = i.dev_rate == i.src_rate;
            let line = format!(
                "SDL output: {} · {} ch · {}{}",
                ui::fmt_khz(i.dev_rate),
                i.dev_channels,
                i.driver,
                if same { "" } else { "  (differs from source)" }
            );
            let col = if same { t.text_dim } else { t.accent };
            out.text(180.0, 878.0, 20.0, col, &line);
        }

        // Progress bar.
        let (bx, by, bw) = (180.0, 920.0, 600.0);
        out.rect(Rect::new(bx, by, bw, 8.0), t.surface_focus, 4.0);
        let frac = if track.duration > 0.0 { (self.pos / track.duration).clamp(0.0, 1.0) } else { 0.0 };
        out.rect(Rect::new(bx, by, bw * frac, 8.0), t.accent, 4.0);
        out.rect(Rect::new(bx + bw * frac - 12.0, by - 8.0, 24.0, 24.0), t.text, 12.0);
        out.text(bx, by + 28.0, 24.0, t.text_dim, &ui::fmt_time(self.pos));
        ui::text_right(out, cx.m, bx + bw, by + 28.0, 24.0, t.text_dim, &ui::fmt_time(track.duration));

        // Play/pause glyph (shapes, so no font dependency).
        let c = Rect::new(bx + bw / 2.0 - 36.0, by + 70.0, 72.0, 72.0);
        out.rect(c, t.surface, 36.0);
        if self.playing {
            out.rect(Rect::new(c.x + 24.0, c.y + 22.0, 8.0, 28.0), t.text, 3.0);
            out.rect(Rect::new(c.x + 40.0, c.y + 22.0, 8.0, 28.0), t.text, 3.0);
        } else {
            ui::text_centered(out, cx.m, c, 30.0, t.text, "▶");
        }
        out.text(bx, by + 96.0, 22.0, t.text_dim, "◀ ▶  seek 10s");

        // Lyrics.
        out.clip(Some(Rect::new(LYRIC_X - 20.0, 60.0, LYRIC_W + 60.0, H - 120.0)));
        match &self.lyrics {
            None => out.text(LYRIC_X, 500.0, 36.0, t.text_dim, "Loading lyrics…"),
            Some(l) if l.is_empty() => out.text(LYRIC_X, 500.0, 36.0, t.text_dim, "No lyrics available"),
            Some(lines) => {
                let active = if self.synced { self.active_line() } else { usize::MAX };
                let center = H / 2.0 - 30.0;
                if !self.synced {
                    out.text(LYRIC_X, 70.0, 24.0, t.text_dim, "Lyrics are not time-synced · Up/Down to scroll");
                }
                for (i, line) in lines.iter().enumerate() {
                    let y = if self.synced { center + (i as f32 - self.lyric_scroll.value) * LYRIC_PITCH } else { 150.0 + (i as f32 - self.lyric_scroll.value) * LYRIC_PITCH };
                    if y < -LYRIC_PITCH || y > H {
                        continue;
                    }
                    let dist = (i as f32 - self.lyric_scroll.value).abs();
                    let k = if self.synced { (dist / 3.0).min(1.0) } else { 0.0 };
                    let col: Color = t.text.lerp(t.text_dim, k).mul_alpha(1.0 - 0.55 * k);
                    let col = if i == active { t.text } else { col };
                    let s = fit_text(cx.m, &line.text, 44.0, LYRIC_W);
                    out.text(LYRIC_X, y, 44.0, col, &s);
                }
            }
        }
        out.clip(None);
        let _ = W;
    }
}

impl Default for PlayerScreen {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw::DrawList;
    use crate::text::MonoMetrics;
    use crate::theme::Theme;

    fn track() -> Track {
        Track { id: 1, title: "T".into(), artist: "A".into(), album: "B".into(), duration: 100.0, art: None }
    }

    fn lines(n: usize) -> Vec<LyricLine> {
        (0..n).map(|i| LyricLine { t: i as f32 * 5.0, text: format!("line {i}") }).collect()
    }

    #[test]
    fn synced_lyrics_follow_playback_position() {
        let mut p = PlayerScreen::new();
        p.open(track());
        p.set_lyrics(1, lines(10), true);
        for _ in 0..(12 * 60) {
            p.update(1.0 / 60.0);
        }
        assert_eq!(p.active_line(), 2); // 12 s in, lines every 5 s
        assert!(p.lyric_scroll.target >= 2.0);
    }

    #[test]
    fn unsynced_lyrics_scroll_with_up_down_and_ignore_position() {
        let mut p = PlayerScreen::new();
        p.open(track());
        p.set_lyrics(1, lines(20), false);
        for _ in 0..(30 * 60) {
            p.update(1.0 / 60.0);
        }
        assert_eq!(p.active_line(), 0);
        assert_eq!(p.lyric_scroll.target, 0.0);
        p.on_key(Key::Down);
        p.on_key(Key::Down);
        assert_eq!(p.lyric_scroll.target, 2.0);
        for _ in 0..40 {
            p.on_key(Key::Down);
        }
        assert_eq!(p.lyric_scroll.target, 14.0); // clamped to n - 6
        p.on_key(Key::Up);
        assert_eq!(p.lyric_scroll.target, 13.0);
    }

    #[test]
    fn lyrics_for_another_track_are_ignored_and_view_renders() {
        let mut p = PlayerScreen::new();
        p.open(track());
        p.set_lyrics(999, lines(3), true);
        assert!(p.lyrics.is_none());
        let (theme, m) = (Theme::default(), MonoMetrics);
        let cx = Cx { theme: &theme, m: &m, images: &crate::images::NoImages, time: 0.0 };
        let mut out = DrawList::new();
        p.set_lyrics(1, lines(3), false);
        p.view(&mut out, &cx);
        assert!(!out.cmds.is_empty());
    }
}
