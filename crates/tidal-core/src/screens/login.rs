use super::Action;
use crate::draw::DrawList;
use crate::geom::{Color, Rect};
use crate::input::Key;
use crate::ui;
use crate::Cx;
use qrcode::{Color as Module, QrCode};

/// How long the phone has to open the link and come back through the relay, in seconds.
const PKCE_WINDOW: f32 = 180.0;
/// Where the single-QR views put the code panel (device code, and PKCE when the code comes back to the
/// relay on its own).
const PANEL: Rect = Rect { x: 240.0, y: 340.0, w: 440.0, h: 440.0 };
/// The paste flow: sign in on the left, bring the address back on the right.
const QR_SIGN_IN: Rect = Rect { x: 240.0, y: 330.0, w: 400.0, h: 400.0 };
const QR_PASTE: Rect = Rect { x: 680.0, y: 330.0, w: 400.0, h: 400.0 };

enum State {
    /// Waiting for the backend (restoring a saved session / starting the sign-in).
    Checking,
    /// Nothing in flight: the user cancelled, or the service has not been asked yet.
    Idle,
    /// PKCE (primary): the QR opens the service's login page, the callback lands on this TV.
    Pkce(PkceView),
    /// PKCE did not complete: offer the device-code fallback.
    Offer(Offer),
    /// Device-code fallback: the user approves on their phone with a short code.
    Code(CodeView),
    /// Signed in, but the account cannot stream HiRes (the fallback does not).
    Reduced(Reduced),
    Failed(String),
}

/// A QR matrix (true = dark) and its width in modules.
struct Qr {
    modules: Vec<bool>,
    n: usize,
}

impl Qr {
    fn new(text: &str) -> Option<Qr> {
        let c = QrCode::new(text.as_bytes()).ok()?;
        let n = c.width();
        Some(Qr { modules: c.to_colors().into_iter().map(|m| m == Module::Dark).collect(), n })
    }

    fn view(&self, out: &mut DrawList, panel: Rect) {
        // 4-module quiet zone; integer cell size keeps modules crisp.
        let cell = (panel.w / (self.n as f32 + 8.0)).floor().max(2.0);
        let side = cell * self.n as f32;
        let (ox, oy) = (panel.x + (panel.w - side) / 2.0, panel.y + (panel.h - side) / 2.0);
        let dark = Color::rgba(0.0, 0.0, 0.0, 1.0);
        for row in 0..self.n {
            // Merge horizontal runs: far fewer rects than one per module.
            let mut col = 0;
            while col < self.n {
                if self.modules[row * self.n + col] {
                    let start = col;
                    while col < self.n && self.modules[row * self.n + col] {
                        col += 1;
                    }
                    // Grow by 0.5px so neighbouring runs overlap: two anti-aliased
                    // edges meeting at the same line would leave a light seam.
                    let run = Rect::new(ox + start as f32 * cell, oy + row as f32 * cell, (col - start) as f32 * cell, cell);
                    out.rect(run.inset(-0.5), dark, 0.0);
                } else {
                    col += 1;
                }
            }
        }
    }
}

struct PkceView {
    expires_in: f32,
    shown_at: f32,
    qr: Option<Qr>,
    /// The relay's paste page, present when the service delivers the code to the client's own page
    /// instead of to the relay: the phone has to carry the address back by hand.
    paste_qr: Option<Qr>,
}

struct Offer {
    reason: String,
    /// 0 = try PKCE again, 1 = use the device-code fallback.
    choice: usize,
}

struct CodeView {
    user_code: String,
    verify_url: String,
    expires_in: f32,
    shown_at: f32,
    qr: Option<Qr>,
}

struct Reduced {
    quality: String,
    /// 0 = try PKCE again, 1 = continue into the library.
    choice: usize,
}

/// Sign-in screen. The backend drives it: PKCE first (the only flow that yields HiRes) and the
/// device-code flow as the fallback, each with its own view.
pub struct LoginScreen {
    state: State,
}

impl Default for LoginScreen {
    fn default() -> Self {
        Self { state: State::Checking }
    }
}

impl LoginScreen {
    pub fn set_checking(&mut self) {
        self.state = State::Checking;
    }

    /// Nothing is being attempted any more (the PKCE server was closed).
    pub fn set_idle(&mut self) {
        self.state = State::Idle;
    }

    pub fn set_failed(&mut self, msg: String) {
        self.state = State::Failed(msg);
    }

    /// `now` = app time (seconds) when the link was received, for the countdown. `paste_url` is the
    /// relay's page for an address the service hands to the phone instead of to the relay.
    pub fn set_pkce(&mut self, qr_url: &str, paste_url: Option<&str>, expires_in: u32, now: f32) {
        let expires_in = if expires_in == 0 { PKCE_WINDOW } else { expires_in as f32 };
        self.state = State::Pkce(PkceView {
            expires_in,
            shown_at: now,
            qr: Qr::new(qr_url),
            paste_qr: paste_url.and_then(Qr::new),
        });
    }

    /// PKCE could not be completed: show the transition dialog.
    pub fn set_offer(&mut self, reason: String) {
        self.state = State::Offer(Offer { reason, choice: 0 });
    }

    /// Signed in with reduced quality (the fallback): offer to try PKCE again.
    pub fn set_reduced_quality(&mut self, quality: String) {
        self.state = State::Reduced(Reduced { quality, choice: 1 });
    }

    /// `now` = app time (seconds) when the code was received, for the countdown.
    pub fn set_code(&mut self, user_code: String, verify_url: String, qr_url: &str, expires_in: u32, now: f32) {
        let qr = Qr::new(qr_url);
        self.state = State::Code(CodeView { user_code, verify_url, expires_in: expires_in as f32, shown_at: now, qr });
    }

    pub fn on_key(&mut self, k: Key) -> Action {
        let action = match (&mut self.state, k) {
            (State::Failed(_), Key::Ok) | (State::Idle, Key::Ok) => Action::RetryLogin,
            // OK cannot approve anything here: it is the only way out besides leaving the flow.
            (State::Pkce(_), Key::Ok) => Action::UseFallback,
            (State::Pkce(_), Key::Back) => Action::CancelLogin,
            (State::Offer(o), Key::Left | Key::Right) => {
                o.choice ^= 1;
                Action::None
            }
            (State::Offer(o), Key::Ok) => {
                if o.choice == 0 {
                    Action::RetryLogin
                } else {
                    Action::UseFallback
                }
            }
            (State::Reduced(r), Key::Left | Key::Right) => {
                r.choice ^= 1;
                Action::None
            }
            (State::Reduced(r), Key::Ok) => {
                if r.choice == 0 {
                    Action::RetryLogin
                } else {
                    Action::Continue
                }
            }
            // Back dismisses the notice: the user is signed in already.
            (State::Reduced(_), Key::Back) => Action::Continue,
            (_, Key::Back) => Action::Back,
            _ => Action::None,
        };
        match action {
            Action::RetryLogin | Action::UseFallback => self.state = State::Checking,
            Action::CancelLogin => self.state = State::Idle,
            _ => {}
        }
        action
    }

    pub fn view(&self, out: &mut DrawList, cx: &Cx) {
        let t = &cx.theme;
        let (title, subtitle) = match &self.state {
            State::Code(_) => ("Sign in", "Link this TV with a short code."),
            State::Reduced(_) => ("Signed in", "This TV is not streaming HiRes."),
            State::Offer(_) => ("Sign in", "The phone did not finish signing in."),
            _ => ("Sign in", "Link this TV to your account using your phone."),
        };
        out.text(240.0, 170.0, 72.0, t.text, title);
        out.text(240.0, 262.0, 32.0, t.text_dim, subtitle);

        match &self.state {
            State::Checking => {
                let a = 0.5 + 0.5 * (cx.time * 4.0).sin().abs();
                out.text(240.0, 420.0, 40.0, t.text_dim.mul_alpha(a), "Please wait…");
            }
            State::Idle => out.text(240.0, 420.0, 40.0, t.text_dim, "Press OK to sign in."),
            State::Failed(msg) => {
                out.rect(Rect::new(240.0, 380.0, 1440.0, 270.0), t.surface, 24.0);
                out.text(280.0, 410.0, 36.0, t.text, "Sign-in problem");
                for (i, line) in crate::text::wrap_text(cx.m, msg, 28.0, 1360.0, 3).iter().enumerate() {
                    out.text(280.0, 470.0 + i as f32 * 38.0, 28.0, t.text_dim, line);
                }
                out.text(280.0, 596.0, 28.0, t.accent, "Press OK to try again");
            }
            State::Pkce(p) => self.view_pkce(out, cx, p),
            State::Offer(o) => self.view_offer(out, cx, o),
            State::Code(c) => self.view_code(out, cx, c),
            State::Reduced(r) => self.view_reduced(out, cx, r),
        }
    }

    /// PKCE: the QR opens the service's sign-in page. With a paste page, a second QR brings the
    /// address the service handed to the phone back here; without one, the relay receives the code.
    fn view_pkce(&self, out: &mut DrawList, cx: &Cx, p: &PkceView) {
        let t = &cx.theme;
        match &p.paste_qr {
            Some(paste) => {
                for (panel, qr, caption) in [
                    (QR_SIGN_IN, p.qr.as_ref(), "1. Sign in"),
                    (QR_PASTE, Some(paste), "2. Paste the address"),
                ] {
                    out.rect(panel, Color::WHITE, 24.0);
                    if let Some(qr) = qr {
                        qr.view(out, panel);
                    }
                    let below = Rect::new(panel.x, panel.y + panel.h + 14.0, panel.w, 34.0);
                    ui::text_centered(out, cx.m, below, 28.0, t.text, caption);
                }
                for (i, line) in [
                    "1. Scan the left code and sign in.",
                    "2. Copy the address of the page that opens.",
                    "3. Scan the right code and paste it there.",
                ]
                .iter()
                .enumerate()
                {
                    out.text(1140.0, 340.0 + i as f32 * 48.0, 28.0, t.text_dim, line);
                }
                out.text(1140.0, 500.0, 24.0, t.text_dim, "HiRes Lossless. The phone can be on any network.");
                out.text(1140.0, 560.0, 24.0, t.text_dim, "The address is the page's URL, not its text.");
            }
            None => {
                out.rect(PANEL, Color::WHITE, 28.0);
                if let Some(qr) = &p.qr {
                    qr.view(out, PANEL);
                }
                out.text(800.0, 340.0, 32.0, t.text_dim, "Scan the code with the phone camera");
                out.text(800.0, 400.0, 32.0, t.text_dim, "and sign in; this TV finishes on its own.");
                out.text(800.0, 480.0, 28.0, t.text_dim, "HiRes Lossless. The phone can be on any network.");
            }
        }

        let (dot, label) = if p.paste_qr.is_some() { (1140.0, 800.0) } else { (800.0, 744.0) };
        let a = 0.45 + 0.35 * (cx.time * 3.0).sin();
        out.rect(Rect::new(dot, label, 18.0, 18.0), t.accent.mul_alpha(a), 9.0);
        out.text(dot + 36.0, label - 12.0, 30.0, t.text_dim, "Waiting for the phone…");
        let left = (p.expires_in - (cx.time - p.shown_at)).max(0.0);
        out.text(dot, label + 56.0, 24.0, t.text_dim, &format!("Link expires in {}", ui::fmt_time(left)));
        out.text(dot, label + 116.0, 24.0, t.text_dim, "OK: common QR code (CD quality)    Back: cancel");
    }

    /// Shown when PKCE failed: retry it, or accept the fallback.
    fn view_offer(&self, out: &mut DrawList, cx: &Cx, o: &Offer) {
        let t = &cx.theme;
        out.rect(Rect::new(240.0, 340.0, 1440.0, 420.0), t.surface, 24.0);
        out.text(280.0, 372.0, 36.0, t.text, "Could not finish signing in");
        for (i, line) in crate::text::wrap_text(cx.m, &o.reason, 26.0, 1360.0, 2).iter().enumerate() {
            out.text(280.0, 428.0 + i as f32 * 34.0, 26.0, t.text_dim, line);
        }
        let hint = "Usually the link expired, the relay could not be reached, or the address from \
                    the phone was not pasted in time. You can also sign in with a short code, but that \
                    cannot stream HiRes.";
        for (i, line) in crate::text::wrap_text(cx.m, hint, 26.0, 1360.0, 3).iter().enumerate() {
            out.text(280.0, 500.0 + i as f32 * 34.0, 26.0, t.text_dim, line);
        }
        button(out, cx, Rect::new(280.0, 640.0, 240.0, 72.0), "Try again", o.choice == 0);
        button(out, cx, Rect::new(548.0, 640.0, 420.0, 72.0), "Use the common QR code", o.choice == 1);
    }

    fn view_code(&self, out: &mut DrawList, cx: &Cx, c: &CodeView) {
        let t = &cx.theme;
        out.rect(PANEL, Color::WHITE, 28.0);
        if let Some(qr) = &c.qr {
            qr.view(out, PANEL);
        }

        out.text(800.0, 340.0, 32.0, t.text_dim, "1. Scan the code, or open");
        out.text(800.0, 392.0, 46.0, t.accent, &c.verify_url);
        out.text(800.0, 520.0, 32.0, t.text_dim, "2. Enter this code");
        out.text(800.0, 572.0, 110.0, t.text, &c.user_code);
        let a = 0.45 + 0.35 * (cx.time * 3.0).sin();
        out.rect(Rect::new(800.0, 744.0, 18.0, 18.0), t.accent.mul_alpha(a), 9.0);
        out.text(836.0, 732.0, 30.0, t.text_dim, "Waiting for approval…");
        let left = (c.expires_in - (cx.time - c.shown_at)).max(0.0);
        out.text(800.0, 800.0, 24.0, t.text_dim, &format!("Code expires in {}", ui::fmt_time(left)));
        out.text(800.0, 860.0, 24.0, t.accent, "This method cannot stream HiRes: maximum quality is CD.");
    }

    fn view_reduced(&self, out: &mut DrawList, cx: &Cx, r: &Reduced) {
        let t = &cx.theme;
        out.rect(Rect::new(240.0, 340.0, 1440.0, 380.0), t.surface, 24.0);
        out.text(280.0, 372.0, 40.0, t.text, "Signed in, but not in HiRes");
        out.text(280.0, 444.0, 30.0, t.text_dim, &format!("Quality: {}", quality_label(&r.quality)));
        let hint = "A short-code sign-in cannot stream HiRes. Use Try HiRes to go through the phone page again.";
        for (i, line) in crate::text::wrap_text(cx.m, hint, 28.0, 1360.0, 2).iter().enumerate() {
            out.text(280.0, 500.0 + i as f32 * 36.0, 28.0, t.text_dim, line);
        }
        button(out, cx, Rect::new(280.0, 600.0, 260.0, 72.0), "Try HiRes", r.choice == 0);
        button(out, cx, Rect::new(568.0, 600.0, 320.0, 72.0), "Continue anyway", r.choice == 1);
    }
}

/// A button that shows the remote's focus the same way the rest of the app does.
fn button(out: &mut DrawList, cx: &Cx, r: Rect, label: &str, focused: bool) {
    let t = &cx.theme;
    if focused {
        out.rect(r.inset(-4.0), t.accent, 22.0);
    }
    out.rect(r, t.surface_focus, 18.0);
    ui::text_centered(out, cx.m, r, 30.0, t.text, label);
}

/// What the service says the account can stream, in words a listener understands.
fn quality_label(quality: &str) -> String {
    let q = quality.to_ascii_uppercase();
    if q.contains("HI_RES") || q.contains("HIRES") {
        "HiRes Lossless (up to 24-bit/192kHz)".into()
    } else if q.contains("LOSSLESS") {
        "Lossless CD (16-bit/44.1kHz)".into()
    } else {
        quality.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_real_qr_matrix() {
        let mut l = LoginScreen::default();
        l.set_pkce("https://login.tidal.com/authorize?client_id=x", None, 180, 0.0);
        let State::Pkce(p) = &l.state else { panic!("expected pkce state") };
        let qr = p.qr.as_ref().unwrap();
        assert!(qr.n >= 21 && qr.modules.len() == qr.n * qr.n);
        assert!(qr.modules.iter().any(|&d| d) && qr.modules.iter().any(|&d| !d));
        // No paste page from this relay: one QR is all there is.
        assert!(p.paste_qr.is_none());
    }

    #[test]
    fn a_paste_page_gives_the_flow_a_second_qr_code() {
        let mut l = LoginScreen::default();
        l.set_pkce(
            "https://login.tidal.com/authorize?client_id=x",
            Some("https://tidal-relay.andregama.dev/paste/abc"),
            300,
            0.0,
        );
        let State::Pkce(p) = &l.state else { panic!("expected pkce state") };
        assert!(p.qr.is_some());
        assert!(p.paste_qr.as_ref().is_some_and(|qr| qr.n >= 21));
        assert_eq!(p.expires_in, 300.0);
    }

    #[test]
    fn failed_state_retries_on_ok() {
        let mut l = LoginScreen::default();
        l.set_failed("boom".into());
        assert!(matches!(l.on_key(Key::Ok), Action::RetryLogin));
        assert!(matches!(l.state, State::Checking));
    }

    #[test]
    fn back_on_pkce_cancels_and_returns_to_idle() {
        let mut l = LoginScreen::default();
        l.set_pkce("https://login.tidal.com/authorize", Some("https://relay/paste/abc"), 180, 0.0);
        assert!(matches!(l.on_key(Key::Back), Action::CancelLogin));
        assert!(matches!(l.state, State::Idle));
        // Nothing is in flight any more: OK starts over.
        assert!(matches!(l.on_key(Key::Ok), Action::RetryLogin));
        assert!(matches!(l.state, State::Checking));
    }

    #[test]
    fn offer_switches_between_retry_and_fallback() {
        let mut l = LoginScreen::default();
        l.set_offer("no route".into());
        assert!(matches!(l.on_key(Key::Ok), Action::RetryLogin));
        l.set_offer("no route".into());
        assert!(matches!(l.on_key(Key::Right), Action::None));
        assert!(matches!(l.on_key(Key::Ok), Action::UseFallback));
        assert!(matches!(l.state, State::Checking));
    }

    #[test]
    fn reduced_quality_continues_by_default() {
        let mut l = LoginScreen::default();
        l.set_reduced_quality("LOSSLESS".into());
        assert!(matches!(l.on_key(Key::Ok), Action::Continue));
        l.set_reduced_quality("LOSSLESS".into());
        assert!(matches!(l.on_key(Key::Left), Action::None));
        assert!(matches!(l.on_key(Key::Ok), Action::RetryLogin));
        l.set_reduced_quality("LOSSLESS".into());
        assert!(matches!(l.on_key(Key::Back), Action::Continue));
    }

    #[test]
    fn quality_labels_are_readable() {
        assert!(quality_label("HI_RES_LOSSLESS").starts_with("HiRes Lossless"));
        assert!(quality_label("LOSSLESS").starts_with("Lossless CD"));
        assert_eq!(quality_label("HIGH"), "HIGH");
    }
}
