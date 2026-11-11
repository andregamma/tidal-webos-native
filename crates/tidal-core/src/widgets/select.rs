//! A drop-down "select" for a remote control.
//!
//! Closed it is a single field showing the current option. OK opens a list; Up/Down move through
//! it, OK picks, Back/Left/Right close it without changing anything. While the list is open the
//! widget swallows every direction key, so focus cannot wander off the page by accident.
//!
//! Like [`super::minibar::MiniBar`] it knows nothing about routes or the backend: the page owns it,
//! calls [`Select::update`] every frame (which also lays the list out), draws the field in its normal
//! place with [`Select::view_field`] and draws the list **last** with [`Select::view_popup`] so it
//! sits above the rest of the page.

use crate::anim::Smooth;
use crate::draw::DrawList;
use crate::geom::{Color, Rect};
use crate::input::Key;
use crate::screens::sidebar::COLLAPSED_W;
use crate::text::fit_text;
use crate::{ui, Cx, H, W};

const ROW_H: f32 = 72.0;
const PAD: f32 = 8.0;
/// Gap between the field and the list.
const GAP: f32 = 12.0;
/// Distance kept from the screen edge.
const EDGE: f32 = 24.0;
const MAX_VISIBLE: usize = 7;
const MIN_VISIBLE: usize = 3;
/// The list opens below the field when at least this many rows fit there; otherwise on whichever
/// side has more room (typically above, when the mini bar takes the bottom of the screen).
const PREFER_BELOW: usize = 5;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SelectOption {
    /// Stable value (e.g. `pt-BR`), shown dimmed at the right of the row unless it only repeats the
    /// label in another case (e.g. quality `max` / `Max`).
    pub value: String,
    /// What the user reads (e.g. `Português (Brasil)`).
    pub label: String,
}

impl SelectOption {
    /// The value is worth a column of its own when it says more than the label does.
    fn shows_value(&self) -> bool {
        !self.value.eq_ignore_ascii_case(&self.label)
    }
}

/// What a key press did.
#[derive(Debug, PartialEq, Eq)]
pub enum SelectEvent {
    /// Not for the select (closed and not OK): the page decides what the key means.
    Ignored,
    /// Consumed (open/close/move).
    Handled,
    /// A different option was chosen (index into the options).
    Changed(usize),
}

pub struct Select {
    options: Vec<SelectOption>,
    selected: usize,
    cursor: usize,
    open: bool,
    /// 0 = closed .. 1 = open (fade of the list).
    anim: Smooth,
    scroll: Smooth,
    hl_y: Smooth,
    /// List rectangle and rows shown, recomputed by [`Select::update`].
    panel: Rect,
    visible: usize,
}

impl Select {
    pub fn new(options: Vec<SelectOption>) -> Self {
        Self {
            options,
            selected: 0,
            cursor: 0,
            open: false,
            anim: Smooth::new(0.0).with_rate(18.0),
            scroll: Smooth::default(),
            hl_y: Smooth::default(),
            panel: Rect::default(),
            visible: MIN_VISIBLE,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Value of the current option.
    pub fn value(&self) -> Option<&str> {
        self.options.get(self.selected).map(|o| o.value.as_str())
    }

    /// Value of the option at `i`.
    pub fn value_at(&self, i: usize) -> Option<&str> {
        self.options.get(i).map(|o| o.value.as_str())
    }

    /// Make `value` the current option without emitting anything (unknown values are ignored).
    pub fn select_value(&mut self, value: &str) {
        if let Some(i) = self.options.iter().position(|o| o.value == value) {
            self.selected = i;
            if !self.open {
                self.cursor = i;
            }
        }
    }

    pub fn on_key(&mut self, k: Key) -> SelectEvent {
        if !self.open {
            if k == Key::Ok && !self.options.is_empty() {
                self.open_list();
                return SelectEvent::Handled;
            }
            return SelectEvent::Ignored;
        }
        match k {
            Key::Up => self.move_cursor(-1),
            Key::Down => self.move_cursor(1),
            Key::Ok => {
                self.open = false;
                if self.cursor != self.selected {
                    self.selected = self.cursor;
                    return SelectEvent::Changed(self.selected);
                }
            }
            Key::Back | Key::Left | Key::Right => self.open = false,
            _ => return SelectEvent::Ignored,
        }
        SelectEvent::Handled
    }

    fn open_list(&mut self) {
        self.open = true;
        self.cursor = self.selected;
        self.scroll.snap(0.0);
        self.retarget();
        self.scroll.snap(self.scroll.target);
        self.hl_y.snap(self.cursor as f32 * ROW_H);
    }

    fn move_cursor(&mut self, delta: i32) {
        let last = self.options.len().saturating_sub(1) as i32;
        self.cursor = (self.cursor as i32 + delta).clamp(0, last) as usize;
        self.retarget();
    }

    /// Keep the cursor row inside the visible window.
    fn retarget(&mut self) {
        let view_h = self.visible as f32 * ROW_H;
        let top = self.cursor as f32 * ROW_H;
        let cur = self.scroll.target;
        if top < cur {
            self.scroll.set(top);
        } else if top + ROW_H > cur + view_h {
            self.scroll.set(top + ROW_H - view_h);
        }
        self.hl_y.set(top);
    }

    /// Where the list goes and how many rows it shows. `inset` is the bottom space the page keeps
    /// free for the mini bar.
    fn place(&self, field: Rect, inset: f32) -> (Rect, usize) {
        let n = self.options.len();
        let want = n.min(MAX_VISIBLE);
        let rows = |space: f32| (((space - 2.0 * PAD) / ROW_H).floor().max(0.0) as usize).min(want);
        let below = rows(H - EDGE - inset - field.bottom() - GAP);
        let above = rows(field.y - EDGE - GAP);
        let up = below < want.min(PREFER_BELOW) && above > below;
        let visible = (if up { above } else { below }).max(MIN_VISIBLE.min(n));
        let h = visible as f32 * ROW_H + 2.0 * PAD;
        let y = if up { field.y - GAP - h } else { field.bottom() + GAP };
        (Rect::new(field.x, y, field.w, h), visible)
    }

    /// Per frame: lay the list out around `field` and advance the animations.
    pub fn update(&mut self, dt: f32, field: Rect, inset: f32) {
        let (panel, visible) = self.place(field, inset);
        let relayout = visible != self.visible;
        self.panel = panel;
        self.visible = visible;
        if self.open && relayout {
            self.retarget();
        }
        self.anim.set(if self.open { 1.0 } else { 0.0 });
        self.anim.update(dt);
        self.scroll.update(dt);
        self.hl_y.update(dt);
    }

    /// The closed field: current label, its code and a caret. `focused` draws the focus ring.
    pub fn view_field(&self, out: &mut DrawList, cx: &Cx, rect: Rect, focused: bool) {
        let t = &cx.theme;
        if focused {
            out.rect(rect.inset(-4.0), t.accent, 28.0);
            out.rect(rect, t.surface_focus, 24.0);
        } else {
            out.rect(rect, t.surface, 24.0);
        }
        let Some(opt) = self.options.get(self.selected) else { return };
        let caret = if self.open { "▴" } else { "▾" };
        let caret_color = if focused { t.accent } else { t.text_dim };
        let ty = |size: f32| rect.y + (rect.h - size * 1.2) / 2.0;
        ui::text_right(out, cx.m, rect.right() - 36.0, ty(34.0), 34.0, caret_color, caret);
        if opt.shows_value() {
            ui::text_right(out, cx.m, rect.right() - 96.0, ty(26.0), 26.0, t.text_dim, &opt.value);
        }
        let label = fit_text(cx.m, &opt.label, 34.0, rect.w - 36.0 - 180.0);
        out.text(rect.x + 36.0, ty(34.0), 34.0, t.text, &label);
    }

    /// The open list with a dimmed backdrop. Call it after everything else on the page.
    pub fn view_popup(&self, out: &mut DrawList, cx: &Cx) {
        let a = self.anim.value;
        if a < 0.01 {
            return;
        }
        let t = &cx.theme;
        out.rect(Rect::new(COLLAPSED_W, 0.0, W - COLLAPSED_W, H), Color::BLACK.with_alpha(0.5 * a), 0.0);

        let p = self.panel;
        out.rect(p.inset(-2.0), t.surface_focus.mul_alpha(a), 26.0);
        out.rect(p, t.sidebar_bg.mul_alpha(a), 24.0);

        let rows = Rect::new(p.x, p.y + PAD, p.w, self.visible as f32 * ROW_H);
        out.clip(Some(rows));
        let sy = self.scroll.value;
        out.rect(
            Rect::new(rows.x + 8.0, rows.y + self.hl_y.value - sy + 4.0, rows.w - 16.0, ROW_H - 8.0),
            t.surface_focus.mul_alpha(a),
            18.0,
        );
        for (i, o) in self.options.iter().enumerate() {
            let y = rows.y + i as f32 * ROW_H - sy;
            if y + ROW_H < rows.y || y > rows.bottom() {
                continue;
            }
            let chosen = i == self.selected;
            if chosen {
                out.text(rows.x + 28.0, y + (ROW_H - 32.0 * 1.2) / 2.0, 32.0, t.accent.mul_alpha(a), "✓");
            }
            let label = fit_text(cx.m, &o.label, 32.0, p.w - 72.0 - 36.0 - 170.0);
            let color = if chosen { t.accent } else { t.text };
            out.text(rows.x + 72.0, y + (ROW_H - 32.0 * 1.2) / 2.0, 32.0, color.mul_alpha(a), &label);
            if o.shows_value() {
                ui::text_right(
                    out,
                    cx.m,
                    rows.right() - 36.0,
                    y + (ROW_H - 26.0 * 1.2) / 2.0,
                    26.0,
                    t.text_dim.mul_alpha(a),
                    &o.value,
                );
            }
        }
        let n = self.options.len();
        if n > self.visible {
            let total = n as f32 * ROW_H;
            let th = rows.h * rows.h / total;
            let thumb_y = rows.y + rows.h * sy / total;
            out.rect(
                Rect::new(p.right() - 12.0, thumb_y + 4.0, 4.0, (th - 8.0).max(8.0)),
                t.text_dim.mul_alpha(0.6 * a),
                2.0,
            );
        }
        out.clip(Some(Rect::new(COLLAPSED_W, 0.0, W - COLLAPSED_W, H)));
    }
}
