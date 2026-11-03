use super::Action;
use crate::anim::Smooth;
use crate::draw::DrawList;
use crate::geom::Rect;
use crate::input::Key;
use crate::model::Track;
use crate::screens::sidebar::COLLAPSED_W;
use crate::text::fit_text;
use crate::{ui, Cx, H, W};

const LEFT: f32 = 168.0;
const LIST_TOP: f32 = 210.0;
const ROW_H: f32 = 104.0;

#[derive(Default)]
pub struct LikedScreen {
    tracks: Vec<Track>,
    sel: usize,
    scroll: Smooth,
    hl_y: Smooth,
}

impl LikedScreen {
    pub fn set_tracks(&mut self, tracks: Vec<Track>) {
        self.tracks = tracks;
        self.sel = 0;
        self.scroll.snap(0.0);
        self.hl_y.snap(0.0);
    }

    pub fn on_key(&mut self, k: Key) -> Action {
        match k {
            Key::Up => self.sel = self.sel.saturating_sub(1),
            Key::Down => {
                if !self.tracks.is_empty() {
                    self.sel = (self.sel + 1).min(self.tracks.len() - 1)
                }
            }
            Key::Left | Key::Back => return Action::FocusSidebar,
            Key::Ok => {
                if let Some(t) = self.tracks.get(self.sel) {
                    return Action::Play(t.clone());
                }
            }
            _ => {}
        }
        self.retarget();
        Action::None
    }

    fn retarget(&mut self) {
        let view_h = H - LIST_TOP - 24.0;
        let top = self.sel as f32 * ROW_H;
        let cur = self.scroll.target;
        if top < cur {
            self.scroll.set(top);
        } else if top + ROW_H > cur + view_h {
            self.scroll.set(top + ROW_H - view_h);
        }
        self.hl_y.set(top);
    }

    pub fn update(&mut self, dt: f32) {
        self.scroll.update(dt);
        self.hl_y.update(dt);
    }

    pub fn view(&self, out: &mut DrawList, cx: &Cx, content_focused: bool) {
        let t = &cx.theme;
        out.clip(Some(Rect::new(COLLAPSED_W, 0.0, W - COLLAPSED_W, H)));
        out.text(LEFT, 70.0, 56.0, t.text, "Liked songs");
        out.text(LEFT, 140.0, 28.0, t.text_dim, &format!("{} tracks", self.tracks.len()));

        out.clip(Some(Rect::new(COLLAPSED_W, LIST_TOP - 10.0, W - COLLAPSED_W, H - LIST_TOP + 10.0)));
        let sy = self.scroll.value;
        let list_w = W - LEFT - 96.0;

        if self.tracks.is_empty() {
            let a = 0.55 + 0.25 * (cx.time * 3.0).sin();
            for i in 0..6 {
                out.rect(Rect::new(LEFT, LIST_TOP + i as f32 * ROW_H, list_w, ROW_H - 12.0), t.surface.mul_alpha(a), 20.0);
            }
            out.clip(None);
            return;
        }

        if content_focused {
            out.rect(
                Rect::new(LEFT - 24.0, LIST_TOP + self.hl_y.value - sy, list_w + 24.0, ROW_H - 8.0),
                t.surface_focus,
                22.0,
            );
        }
        for (i, tr) in self.tracks.iter().enumerate() {
            let y = LIST_TOP + i as f32 * ROW_H - sy;
            if y > H || y + ROW_H < LIST_TOP - 10.0 {
                continue;
            }
            ui::cover(out, cx, Rect::new(LEFT, y + 8.0, 76.0, 76.0), tr.id, tr.art.as_deref(), 160, 12.0);
            let title = fit_text(cx.m, &tr.title, 30.0, 640.0);
            out.text(LEFT + 104.0, y + 12.0, 30.0, t.text, &title);
            let artist = fit_text(cx.m, &tr.artist, 24.0, 640.0);
            out.text(LEFT + 104.0, y + 54.0, 24.0, t.text_dim, &artist);
            let album = fit_text(cx.m, &tr.album, 26.0, 480.0);
            out.text(LEFT + 860.0, y + 30.0, 26.0, t.text_dim, &album);
            ui::text_right(out, cx.m, LEFT + list_w - 24.0, y + 30.0, 26.0, t.text_dim, &ui::fmt_time(tr.duration));
        }
        out.clip(None);
    }
}
