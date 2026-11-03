use super::Action;
use crate::draw::DrawList;
use crate::geom::{Color, Rect};
use crate::input::Key;
use crate::ui;
use crate::Cx;
use qrcode::{Color as Module, QrCode};

enum State {
    /// Waiting for the backend (restoring a saved session / requesting a code).
    Checking,
    Code(CodeView),
    Failed(String),
}

struct CodeView {
    user_code: String,
    verify_url: String,
    expires_in: f32,
    shown_at: f32,
    /// Square module matrix (true = dark) and its width.
    qr: Option<(Vec<bool>, usize)>,
}

/// Device-code login screen. The backend supplies the code + URL; the user
/// approves on their phone.
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

    pub fn set_failed(&mut self, msg: String) {
        self.state = State::Failed(msg);
    }

    /// `now` = app time (seconds) when the code was received, for the countdown.
    pub fn set_code(&mut self, user_code: String, verify_url: String, qr_url: &str, expires_in: u32, now: f32) {
        let qr = QrCode::new(qr_url.as_bytes()).ok().map(|c| {
            let n = c.width();
            (c.to_colors().into_iter().map(|m| m == Module::Dark).collect(), n)
        });
        self.state = State::Code(CodeView { user_code, verify_url, expires_in: expires_in as f32, shown_at: now, qr });
    }

    pub fn on_key(&mut self, k: Key) -> Action {
        match (&self.state, k) {
            (State::Failed(_), Key::Ok) => {
                self.state = State::Checking;
                Action::RetryLogin
            }
            (_, Key::Back) => Action::Back,
            _ => Action::None,
        }
    }

    pub fn view(&self, out: &mut DrawList, cx: &Cx) {
        let t = &cx.theme;
        out.text(240.0, 170.0, 72.0, t.text, "Sign in");
        out.text(240.0, 262.0, 32.0, t.text_dim, "Link this TV to your account using your phone.");

        match &self.state {
            State::Checking => {
                let a = 0.5 + 0.5 * (cx.time * 4.0).sin().abs();
                out.text(240.0, 420.0, 40.0, t.text_dim.mul_alpha(a), "Please wait…");
            }
            State::Failed(msg) => {
                out.rect(Rect::new(240.0, 380.0, 1440.0, 270.0), t.surface, 24.0);
                out.text(280.0, 410.0, 36.0, t.text, "Sign-in problem");
                for (i, line) in crate::text::wrap_text(cx.m, msg, 28.0, 1360.0, 3).iter().enumerate() {
                    out.text(280.0, 470.0 + i as f32 * 38.0, 28.0, t.text_dim, line);
                }
                out.text(280.0, 596.0, 28.0, t.accent, "Press OK to try again");
            }
            State::Code(c) => self.view_code(out, cx, c),
        }
    }

    fn view_code(&self, out: &mut DrawList, cx: &Cx, c: &CodeView) {
        let t = &cx.theme;
        let panel = Rect::new(240.0, 340.0, 440.0, 440.0);
        out.rect(panel, Color::WHITE, 28.0);
        if let Some((modules, n)) = &c.qr {
            // 4-module quiet zone; integer cell size keeps modules crisp.
            let cell = (panel.w / (*n as f32 + 8.0)).floor().max(2.0);
            let side = cell * *n as f32;
            let (ox, oy) = (panel.x + (panel.w - side) / 2.0, panel.y + (panel.h - side) / 2.0);
            let dark = Color::rgba(0.0, 0.0, 0.0, 1.0);
            for row in 0..*n {
                // Merge horizontal runs: far fewer rects than one per module.
                let mut col = 0;
                while col < *n {
                    if modules[row * n + col] {
                        let start = col;
                        while col < *n && modules[row * n + col] {
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

        out.text(800.0, 340.0, 32.0, t.text_dim, "1. Scan the code, or open");
        out.text(800.0, 392.0, 46.0, t.accent, &c.verify_url);
        out.text(800.0, 520.0, 32.0, t.text_dim, "2. Enter this code");
        out.text(800.0, 572.0, 110.0, t.text, &c.user_code);
        let a = 0.45 + 0.35 * (cx.time * 3.0).sin();
        out.rect(Rect::new(800.0, 744.0, 18.0, 18.0), t.accent.mul_alpha(a), 9.0);
        out.text(836.0, 732.0, 30.0, t.text_dim, "Waiting for approval…");
        let left = (c.expires_in - (cx.time - c.shown_at)).max(0.0);
        out.text(800.0, 800.0, 24.0, t.text_dim, &format!("Code expires in {}", ui::fmt_time(left)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_real_qr_matrix() {
        let mut l = LoginScreen::default();
        l.set_code("ABCDE".into(), "link.example.com".into(), "https://link.example.com/ABCDE", 300, 0.0);
        let State::Code(c) = &l.state else { panic!("expected code state") };
        let (m, n) = c.qr.as_ref().unwrap();
        assert!(*n >= 21 && m.len() == n * n);
        assert!(m.iter().any(|&d| d) && m.iter().any(|&d| !d));
    }

    #[test]
    fn failed_state_retries_on_ok() {
        let mut l = LoginScreen::default();
        l.set_failed("boom".into());
        assert!(matches!(l.on_key(Key::Ok), Action::RetryLogin));
        assert!(matches!(l.state, State::Checking));
    }
}
