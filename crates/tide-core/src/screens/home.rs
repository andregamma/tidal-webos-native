use super::Action;
use crate::anim::Smooth;
use crate::draw::DrawList;
use crate::geom::Rect;
use crate::input::Key;
use crate::model::{Card, Shelf};
use crate::screens::sidebar::COLLAPSED_W;
use crate::text::fit_text;
use crate::{ui, Cx, H, W};

const LEFT: f32 = 168.0;
const TOP: f32 = 190.0;
const CARD: f32 = 260.0;
const GAP: f32 = 32.0;
const STEP: f32 = CARD + GAP;
const PITCH: f32 = 440.0; // vertical distance between shelves

struct ShelfState {
    title: String,
    cards: Vec<Card>,
    col: usize,
    scroll_x: Smooth,
    /// Per-card focus animation 0..1 (drives ring + scale).
    focus: Vec<Smooth>,
}

#[derive(Default)]
pub struct HomeScreen {
    shelves: Vec<ShelfState>,
    row: usize,
    scroll_y: Smooth,
}

impl HomeScreen {
    pub fn set_shelves(&mut self, shelves: Vec<Shelf>) {
        self.shelves = shelves
            .into_iter()
            .filter(|s| !s.cards.is_empty())
            .map(|s| ShelfState {
                focus: vec![Smooth::new(0.0); s.cards.len()],
                title: s.title,
                cards: s.cards,
                col: 0,
                scroll_x: Smooth::new(0.0),
            })
            .collect();
        self.row = 0;
        self.retarget(true);
    }

    pub fn on_key(&mut self, k: Key) -> Action {
        if self.shelves.is_empty() {
            return if k == Key::Left || k == Key::Back { Action::FocusSidebar } else { Action::None };
        }
        let row = self.row;
        match k {
            Key::Up => self.row = row.saturating_sub(1),
            Key::Down => self.row = (row + 1).min(self.shelves.len() - 1),
            Key::Left => {
                let s = &mut self.shelves[row];
                if s.col == 0 {
                    return Action::FocusSidebar;
                }
                s.col -= 1;
            }
            Key::Right => {
                let s = &mut self.shelves[row];
                s.col = (s.col + 1).min(s.cards.len() - 1);
            }
            Key::Ok => {
                let s = &self.shelves[row];
                return match &s.cards[s.col].play {
                    Some(t) => Action::Play(t.clone()),
                    None => Action::Toast("Albums, playlists and mixes open in a later milestone".into()),
                };
            }
            Key::Back => return Action::FocusSidebar,
            _ => {}
        }
        self.retarget(false);
        Action::None
    }

    fn retarget(&mut self, snap: bool) {
        let n = self.shelves.len() as f32;
        let view_h = H - TOP;
        let max_y = (n * PITCH - view_h).max(0.0);
        self.scroll_y.set((self.row as f32 * PITCH).min(max_y));
        let view_w = W - LEFT;
        for (ri, s) in self.shelves.iter_mut().enumerate() {
            let max_x = (s.cards.len() as f32 * STEP - GAP - view_w).max(0.0);
            s.scroll_x.set(((s.col as f32 - 1.0).max(0.0) * STEP).min(max_x));
            for (ci, f) in s.focus.iter_mut().enumerate() {
                f.set(if ri == self.row && ci == s.col { 1.0 } else { 0.0 });
            }
        }
        if snap {
            self.scroll_y.snap(self.scroll_y.target);
            for s in &mut self.shelves {
                s.scroll_x.snap(s.scroll_x.target);
                for f in &mut s.focus {
                    f.snap(f.target);
                }
            }
        }
    }

    pub fn update(&mut self, dt: f32) {
        self.scroll_y.update(dt);
        for s in &mut self.shelves {
            s.scroll_x.update(dt);
            for f in &mut s.focus {
                f.update(dt);
            }
        }
    }

    pub fn view(&self, out: &mut DrawList, cx: &Cx, content_focused: bool) {
        let t = &cx.theme;
        out.clip(Some(Rect::new(COLLAPSED_W, 0.0, W - COLLAPSED_W, H)));
        let sy = self.scroll_y.value;
        out.text(LEFT, TOP - 120.0 - sy, 56.0, t.text, "Home");

        if self.shelves.is_empty() {
            // Skeleton while loading.
            let a = 0.55 + 0.25 * (cx.time * 3.0).sin();
            for r in 0..2 {
                let y = TOP + r as f32 * PITCH;
                out.rect(Rect::new(LEFT, y, 260.0, 30.0), t.surface.mul_alpha(a), 10.0);
                for c in 0..6 {
                    let x = LEFT + c as f32 * STEP;
                    out.rect(Rect::new(x, y + 56.0, CARD, CARD), t.surface.mul_alpha(a), t.card_radius);
                }
            }
            out.clip(None);
            return;
        }

        for (ri, s) in self.shelves.iter().enumerate() {
            let y = TOP + ri as f32 * PITCH - sy;
            if y > H || y + PITCH < 0.0 {
                continue;
            }
            out.text(LEFT, y, 34.0, t.text, &s.title);
            let y0 = y + 60.0;
            let sx = s.scroll_x.value;
            for (ci, card) in s.cards.iter().enumerate() {
                let x = LEFT + ci as f32 * STEP - sx;
                if x > W || x + CARD < COLLAPSED_W {
                    continue;
                }
                let f = s.focus[ci].value;
                let base = Rect::new(x, y0, CARD, CARD);
                if f > 0.01 && content_focused {
                    out.rect(base.scaled(1.0 + 0.07 * f).inset(-5.0), t.accent.mul_alpha(f), t.card_radius + 8.0);
                }
                ui::cover(out, cx, base.scaled(1.0 + 0.05 * f), card.art_seed, card.art.as_deref(), 320, t.card_radius);
                let title = fit_text(cx.m, &card.title, 26.0, CARD);
                out.text(x, y0 + CARD + 18.0, 26.0, t.text, &title);
                let sub = fit_text(cx.m, &card.subtitle, 22.0, CARD);
                out.text(x, y0 + CARD + 52.0, 22.0, t.text_dim, &sub);
            }
        }
        out.clip(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Track;

    fn shelf(n: usize) -> Shelf {
        let track = Track { id: 1, title: "t".into(), artist: "a".into(), album: "b".into(), duration: 100.0, art: None };
        Shelf {
            title: "s".into(),
            cards: (0..n)
                .map(|i| Card { title: format!("c{i}"), subtitle: "".into(), art_seed: i as u64, art: None, play: Some(track.clone()) })
                .collect(),
        }
    }

    #[test]
    fn navigation_and_sidebar_handoff() {
        let mut h = HomeScreen::default();
        h.set_shelves(vec![shelf(5), shelf(2)]);
        assert!(matches!(h.on_key(Key::Left), Action::FocusSidebar));
        h.on_key(Key::Right);
        h.on_key(Key::Right);
        assert_eq!(h.shelves[0].col, 2);
        h.on_key(Key::Down);
        assert_eq!(h.row, 1);
        h.on_key(Key::Down); // clamped
        assert_eq!(h.row, 1);
        assert!(matches!(h.on_key(Key::Ok), Action::Play(_)));
    }
}
