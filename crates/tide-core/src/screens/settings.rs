//! Settings page: the signed-in account (name, e-mail, plan), the streaming quality preference and
//! the language select.
//!
//! The page owns no I/O. The account arrives through `Evt::Account`; picking a language returns
//! `Action::SetLocale(code)` and picking a quality `Action::SetQuality(q)`. `App` forwards both to the
//! backend (`Cmd::SetLocale` / `Cmd::SetQuality`), which applies them to the Tidal client and persists
//! them. Strings are collected in the constants below so the future i18n layer has one place to
//! replace.

use super::Action;
use crate::draw::DrawList;
use crate::geom::Rect;
use crate::i18n::{self, LANGUAGES};
use crate::input::Key;
use crate::model::{Account, Plan};
use crate::quality::Quality;
use crate::screens::sidebar::COLLAPSED_W;
use crate::text::fit_text;
use crate::widgets::select::{Select, SelectEvent, SelectOption};
use crate::{ui, Cx, H, W};

const TITLE: &str = "Settings";
const SECTION_ACCOUNT: &str = "Account";
const SECTION_QUALITY: &str = "Audio quality";
const SECTION_LANGUAGE: &str = "Language";
const LABEL_NAME: &str = "Name";
const LABEL_EMAIL: &str = "Email";
const LABEL_PLAN: &str = "Plan";
const PLAN_UNAVAILABLE: &str = "Unavailable";
/// Shown under the quality field: what the preference means (the official client shows no tier
/// names in the player; here they only name the choices).
const QUALITY_HINT: &str = "Low and High are lossy (AAC); Lossless is CD quality. Max asks for HiRes,\nthe best the account can stream.";

const LEFT: f32 = 168.0;
const CARD_W: f32 = 1100.0;
const SECTION_ACCOUNT_Y: f32 = 150.0;
const CARD_Y: f32 = 196.0;
const ROW_H: f32 = 72.0;
const CARD_PAD: f32 = 12.0;
const VALUE_X: f32 = 280.0;
const SECTION_QUALITY_Y: f32 = 466.0;
const QUALITY_Y: f32 = 510.0;
const HINT_Y: f32 = 606.0;
const SECTION_LANGUAGE_Y: f32 = 664.0;
const FIELD_Y: f32 = 708.0;
const FIELD_W: f32 = 720.0;
const FIELD_H: f32 = 84.0;

/// Which field has the focus (Up/Down moves between them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Quality,
    Language,
}

pub struct SettingsScreen {
    /// `None` until the backend answers (skeleton shown meanwhile).
    account: Option<Account>,
    quality: Select,
    lang: Select,
    focus: Field,
    /// Bottom space kept free for the mini bar.
    inset: f32,
}

impl SettingsScreen {
    pub fn new(locale: &str) -> Self {
        let quality = Select::new(
            Quality::ALL
                .iter()
                .map(|q| SelectOption { value: q.id().to_string(), label: q.label().to_string() })
                .collect(),
        );
        let options = LANGUAGES
            .iter()
            .map(|l| SelectOption { value: l.code.to_string(), label: l.name.to_string() })
            .collect();
        let mut lang = Select::new(options);
        lang.select_value(i18n::resolve(locale));
        Self { account: None, quality, lang, focus: Field::Quality, inset: 0.0 }
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

    /// Show `quality` as the current preference (no event is emitted).
    pub fn set_quality(&mut self, quality: Quality) {
        self.quality.select_value(quality.id());
    }

    /// Put the focus on the quality field (the player's shortcut opens the page this way).
    pub fn focus_quality(&mut self) {
        self.focus = Field::Quality;
    }

    pub fn set_bottom_inset(&mut self, px: f32) {
        self.inset = px;
    }

    fn quality_field() -> Rect {
        Rect::new(LEFT, QUALITY_Y, FIELD_W, FIELD_H)
    }

    fn language_field() -> Rect {
        Rect::new(LEFT, FIELD_Y, FIELD_W, FIELD_H)
    }

    pub fn on_key(&mut self, k: Key) -> Action {
        // An open list owns every direction key, whichever field it belongs to.
        let field = if self.quality.is_open() {
            Field::Quality
        } else if self.lang.is_open() {
            Field::Language
        } else {
            self.focus
        };
        match field {
            Field::Quality => match self.quality.on_key(k) {
                SelectEvent::Changed(i) => {
                    return match self.quality.value_at(i).and_then(Quality::parse) {
                        Some(q) => Action::SetQuality(q),
                        None => Action::None,
                    };
                }
                SelectEvent::Handled => return Action::None,
                SelectEvent::Ignored => {}
            },
            Field::Language => match self.lang.on_key(k) {
                SelectEvent::Changed(i) => {
                    return match self.lang.value_at(i) {
                        Some(code) => Action::SetLocale(code.to_string()),
                        None => Action::None,
                    };
                }
                SelectEvent::Handled => return Action::None,
                SelectEvent::Ignored => {}
            },
        }
        match k {
            Key::Up => {
                if self.focus == Field::Language {
                    self.focus = Field::Quality;
                } else {
                    return Action::FocusSidebar;
                }
            }
            Key::Down => {
                if self.focus == Field::Quality {
                    self.focus = Field::Language;
                } else {
                    // The language select is the last control: Down runs off the end of the page.
                    return Action::FocusBar;
                }
            }
            Key::Left | Key::Back => return Action::FocusSidebar,
            _ => {}
        }
        Action::None
    }

    pub fn update(&mut self, dt: f32) {
        self.quality.update(dt, Self::quality_field(), self.inset);
        self.lang.update(dt, Self::language_field(), self.inset);
    }

    pub fn view(&self, out: &mut DrawList, cx: &Cx, content_focused: bool) {
        let t = &cx.theme;
        out.clip(Some(Rect::new(COLLAPSED_W, 0.0, W - COLLAPSED_W, H)));
        out.text(LEFT, 70.0, 56.0, t.text, TITLE);

        out.text(LEFT, SECTION_ACCOUNT_Y, 26.0, t.text_dim, SECTION_ACCOUNT);
        self.view_account(out, cx);

        out.text(LEFT, SECTION_QUALITY_Y, 26.0, t.text_dim, SECTION_QUALITY);
        self.quality.view_field(
            out,
            cx,
            Self::quality_field(),
            content_focused && self.focus == Field::Quality,
        );
        out.text(LEFT, HINT_Y, 20.0, t.text_dim, QUALITY_HINT.lines().next().unwrap_or(""));
        if let Some(second) = QUALITY_HINT.lines().nth(1) {
            out.text(LEFT, HINT_Y + 26.0, 20.0, t.text_dim, second);
        }

        out.text(LEFT, SECTION_LANGUAGE_Y, 26.0, t.text_dim, SECTION_LANGUAGE);
        self.lang.view_field(
            out,
            cx,
            Self::language_field(),
            content_focused && self.focus == Field::Language,
        );

        // Above everything else on the page.
        self.quality.view_popup(out, cx);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> SettingsScreen {
        let mut s = SettingsScreen::new("en-US");
        s.set_quality(Quality::Lossless);
        s
    }

    #[test]
    fn up_down_walk_the_fields_then_leave_the_page() {
        let mut s = screen();
        // Focus starts on quality; OK opens its list.
        assert!(matches!(s.on_key(Key::Ok), Action::None));
        assert!(s.quality.is_open());
        // Moving the cursor and picking emits the chosen quality.
        assert!(matches!(s.on_key(Key::Up), Action::None));
        assert!(matches!(s.on_key(Key::Ok), Action::SetQuality(Quality::High)));

        // Down moves to the language field, and past it to the mini bar.
        assert!(matches!(s.on_key(Key::Down), Action::None));
        assert!(matches!(s.on_key(Key::Down), Action::FocusBar));
        assert!(matches!(s.on_key(Key::Up), Action::None));
        assert!(matches!(s.on_key(Key::Up), Action::FocusSidebar));
        assert!(matches!(s.on_key(Key::Back), Action::FocusSidebar));
    }

    #[test]
    fn picking_a_quality_reports_it() {
        let mut s = screen();
        s.set_quality(Quality::Low);
        assert!(matches!(s.on_key(Key::Ok), Action::None));
        assert!(matches!(s.on_key(Key::Down), Action::None));
        assert!(matches!(s.on_key(Key::Ok), Action::SetQuality(Quality::High)));
        // The list is closed again: Down now moves the focus.
        assert!(matches!(s.on_key(Key::Down), Action::None));
    }

    #[test]
    fn the_page_shows_the_quality_section_with_the_current_choice() {
        use crate::draw::{Cmd as Draw, DrawList};
        use crate::images::NoImages;
        use crate::text::MonoMetrics;
        use crate::theme::Theme;

        let s = screen();
        let (theme, m) = (Theme::default(), MonoMetrics);
        let cx = Cx { theme: &theme, m: &m, images: &NoImages, time: 0.0 };
        let mut out = DrawList::new();
        s.view(&mut out, &cx, true);
        let texts: Vec<String> = out
            .cmds
            .iter()
            .filter_map(|c| if let Draw::Text { text, .. } = c { Some(text.clone()) } else { None })
            .collect();
        assert!(texts.iter().any(|t| t == SECTION_QUALITY));
        assert!(texts.iter().any(|t| t == "Lossless"), "the current choice: {texts:?}");
        assert!(texts.iter().any(|t| t == SECTION_LANGUAGE));
    }

    #[test]
    fn the_player_shortcut_puts_the_focus_on_quality() {
        let mut s = screen();
        s.focus_quality();
        assert!(matches!(s.on_key(Key::Ok), Action::None));
        assert!(s.quality.is_open());
    }
}
