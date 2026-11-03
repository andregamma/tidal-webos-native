//! Settings page: the signed-in account (name, e-mail, plan) and the language select.
//!
//! The page owns no I/O. The account arrives through `Evt::Account`; picking a language returns
//! `Action::SetLocale(code)` and `App` forwards it to the backend (`Cmd::SetLocale`), which applies
//! it to the Tidal client and persists it. Strings are collected in the constants below so the
//! future i18n layer has one place to replace.

use super::Action;
use crate::draw::DrawList;
use crate::geom::Rect;
use crate::i18n::{self, LANGUAGES};
use crate::input::Key;
use crate::model::{Account, Plan};
use crate::screens::sidebar::COLLAPSED_W;
use crate::text::fit_text;
use crate::widgets::select::{Select, SelectEvent, SelectOption};
use crate::{ui, Cx, H, W};

const TITLE: &str = "Settings";
const SECTION_ACCOUNT: &str = "Account";
const SECTION_LANGUAGE: &str = "Language";
const LABEL_NAME: &str = "Name";
const LABEL_EMAIL: &str = "Email";
const LABEL_PLAN: &str = "Plan";
const PLAN_UNAVAILABLE: &str = "Unavailable";

const LEFT: f32 = 168.0;
const CARD_W: f32 = 1100.0;
const SECTION_ACCOUNT_Y: f32 = 160.0;
const CARD_Y: f32 = 206.0;
const ROW_H: f32 = 72.0;
const CARD_PAD: f32 = 12.0;
const VALUE_X: f32 = 280.0;
const SECTION_LANGUAGE_Y: f32 = 478.0;
const FIELD_Y: f32 = 524.0;
const FIELD_W: f32 = 720.0;
const FIELD_H: f32 = 84.0;

pub struct SettingsScreen {
    /// `None` until the backend answers (skeleton shown meanwhile).
    account: Option<Account>,
    lang: Select,
    /// Bottom space kept free for the mini bar.
    inset: f32,
}

impl SettingsScreen {
    pub fn new(locale: &str) -> Self {
        let options = LANGUAGES
            .iter()
            .map(|l| SelectOption { value: l.code.to_string(), label: l.name.to_string() })
            .collect();
        let mut lang = Select::new(options);
        lang.select_value(i18n::resolve(locale));
        Self { account: None, lang, inset: 0.0 }
    }

    pub fn set_account(&mut self, account: Account) {
        self.account = Some(account);
    }

    /// Signed out: forget the previous user's data.
    pub fn clear_account(&mut self) {
        self.account = None;
    }

    /// Show `locale` as the current language (no event is emitted).
    pub fn set_locale(&mut self, locale: &str) {
        self.lang.select_value(i18n::resolve(locale));
    }

    pub fn set_bottom_inset(&mut self, px: f32) {
        self.inset = px;
    }

    fn field() -> Rect {
        Rect::new(LEFT, FIELD_Y, FIELD_W, FIELD_H)
    }

    pub fn on_key(&mut self, k: Key) -> Action {
        match self.lang.on_key(k) {
            SelectEvent::Changed(i) => {
                return match self.lang.value_at(i) {
                    Some(code) => Action::SetLocale(code.to_string()),
                    None => Action::None,
                };
            }
            SelectEvent::Handled => return Action::None,
            SelectEvent::Ignored => {}
        }
        match k {
            Key::Left | Key::Back => Action::FocusSidebar,
            // The select is the last (only) control: Down runs off the end of the page.
            Key::Down => Action::FocusBar,
            _ => Action::None,
        }
    }

    pub fn update(&mut self, dt: f32) {
        self.lang.update(dt, Self::field(), self.inset);
    }

    pub fn view(&self, out: &mut DrawList, cx: &Cx, content_focused: bool) {
        let t = &cx.theme;
        out.clip(Some(Rect::new(COLLAPSED_W, 0.0, W - COLLAPSED_W, H)));
        out.text(LEFT, 70.0, 56.0, t.text, TITLE);

        out.text(LEFT, SECTION_ACCOUNT_Y, 26.0, t.text_dim, SECTION_ACCOUNT);
        self.view_account(out, cx);

        out.text(LEFT, SECTION_LANGUAGE_Y, 26.0, t.text_dim, SECTION_LANGUAGE);
        self.lang.view_field(out, cx, Self::field(), content_focused);

        // Above everything else on the page.
        self.lang.view_popup(out, cx);
        out.clip(None);
    }

    fn view_account(&self, out: &mut DrawList, cx: &Cx) {
        let t = &cx.theme;
        let card = Rect::new(LEFT, CARD_Y, CARD_W, 3.0 * ROW_H + 2.0 * CARD_PAD);
        out.rect(card, t.surface, 24.0);

        let pulse = 0.55 + 0.25 * (cx.time * 3.0).sin();
        let labels = [LABEL_NAME, LABEL_EMAIL, LABEL_PLAN];
        for (i, label) in labels.iter().enumerate() {
            let row = Rect::new(card.x, card.y + CARD_PAD + i as f32 * ROW_H, card.w, ROW_H);
            if i > 0 {
                out.rect(Rect::new(row.x + 40.0, row.y, row.w - 80.0, 2.0), t.surface_focus, 0.0);
            }
            out.text(row.x + 40.0, row.y + (ROW_H - 26.0 * 1.2) / 2.0, 26.0, t.text_dim, label);

            let vx = row.x + VALUE_X;
            let vy = row.y + (ROW_H - 32.0 * 1.2) / 2.0;
            let max_w = row.w - VALUE_X - 40.0;
            let Some(acc) = &self.account else {
                out.rect(Rect::new(vx, row.y + (ROW_H - 28.0) / 2.0, 360.0, 28.0), t.surface_focus.mul_alpha(pulse), 14.0);
                continue;
            };
            match i {
                0 => out.text(vx, vy, 32.0, t.text, &fit_text(cx.m, &acc.name, 32.0, max_w)),
                1 => out.text(vx, vy, 32.0, t.text, &fit_text(cx.m, &acc.email, 32.0, max_w)),
                _ => match &acc.plan {
                    Some(p) => {
                        let detail = plan_detail(p);
                        // Detail is right-aligned; the plan name takes what is left of it.
                        let detail_w = cx.m.text_width(&detail, 24.0);
                        let name = fit_text(cx.m, &p.name, 32.0, (max_w - detail_w - 32.0).max(120.0));
                        out.text(vx, vy, 32.0, t.text, &name);
                        ui::text_right(out, cx.m, row.right() - 40.0, row.y + (ROW_H - 24.0 * 1.2) / 2.0, 24.0, t.text_dim, &detail);
                    }
                    None => out.text(vx, vy, 32.0, t.text_dim, PLAN_UNAVAILABLE),
                },
            }
        }
    }
}

impl Default for SettingsScreen {
    fn default() -> Self {
        Self::new(i18n::DEFAULT_LOCALE)
    }
}

/// `Ativo · até 2026-12-01`: whatever of it the service told us.
fn plan_detail(p: &Plan) -> String {
    let status = match p.status.to_ascii_uppercase().as_str() {
        "ACTIVE" => "Active".to_string(),
        "" => String::new(),
        other => other.to_ascii_lowercase(),
    };
    let mut parts: Vec<String> = Vec::new();
    if !status.is_empty() {
        parts.push(status);
    }
    if let Some(d) = &p.valid_until {
        parts.push(format!("até {d}"));
    }
    parts.join(" · ")
}
