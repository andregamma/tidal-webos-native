//! Real backend. M1: authentication + session persistence. Content commands
//! still return mock data until M2.

pub mod auth;
pub mod config;
pub mod http;
pub mod store;

use self::auth::AuthError;
use self::config::Config;
use self::http::{Http, UreqHttp};
use self::store::Session;
use crate::mock;
use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use std::sync::mpsc::Sender;
use tide_core::services::{BackendSide, Cmd, Evt, PlayerCmd, StreamSource, StreamSpec};

/// How many times an expired code is replaced by a fresh one before giving up.
const MAX_CODE_REFRESHES: u32 = 6;

pub struct Backend {
    http: Box<dyn Http>,
    cfg: Result<Config, String>,
    session_path: PathBuf,
    session: Option<Session>,
    clock: Box<dyn Fn() -> u64 + Send>,
    sleeper: Box<dyn Fn(Duration) + Send>,
    player: Option<Sender<PlayerCmd>>,
}

pub fn run(be: BackendSide) {
    let mut b = Backend::new(
        Box::new(UreqHttp::new()),
        Config::load(),
        config::data_dir().join("session.json"),
        Box::new(|| SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())),
        Box::new(std::thread::sleep),
    )
    .with_player(be.player.clone());
    while let Ok(cmd) = be.rx.recv() {
        b.handle(cmd, &|e| {
            let _ = be.tx.send(e);
        });
    }
}

impl Backend {
    pub fn new(
        http: Box<dyn Http>,
        cfg: Result<Config, String>,
        session_path: PathBuf,
        clock: Box<dyn Fn() -> u64 + Send>,
        sleeper: Box<dyn Fn(Duration) + Send>,
    ) -> Self {
        Self { http, cfg, session_path, session: None, clock, sleeper, player: None }
    }

    /// Connect the player so `Cmd::Play` can reach it.
    pub fn with_player(mut self, player: Sender<PlayerCmd>) -> Self {
        self.player = Some(player);
        self
    }

    pub fn handle(&mut self, cmd: Cmd, send: &dyn Fn(Evt)) {
        match cmd {
            Cmd::Boot => self.boot(send),
            Cmd::StartLogin => self.login(send),
            Cmd::Logout => {
                self.session = None;
                store::delete(&self.session_path);
                send(Evt::LoginRequired);
            }
            // Content is still mock data, so playback is simulated (no audio).
            Cmd::Play { track_id, duration } => {
                if let Some(p) = &self.player {
                    let source = StreamSource::Simulated;
                    let _ = p.send(PlayerCmd::Load(StreamSpec { track_id, mime: String::new(), duration, source }));
                }
            }
            // M2: replace with real requests using `self.session`.
            Cmd::LoadHome => send(Evt::Home(mock::home())),
            Cmd::LoadLiked => send(Evt::Liked(mock::catalog())),
            Cmd::LoadLyrics(id) => {
                let dur = mock::catalog().into_iter().find(|t| t.id == id).map_or(180.0, |t| t.duration);
                send(Evt::Lyrics { track_id: id, lines: mock::lyrics(dur), synced: true });
            }
        }
    }

    fn boot(&mut self, send: &dyn Fn(Evt)) {
        let cfg = match &self.cfg {
            Ok(c) => c.clone(),
            Err(e) => return send(Evt::LoginFailed(e.clone())),
        };
        let Some(saved) = store::load(&self.session_path) else {
            return send(Evt::LoginRequired);
        };
        let now = (self.clock)();
        if !saved.needs_refresh(now) {
            self.session = Some(saved);
            return send(Evt::LoggedIn);
        }
        match auth::refresh(self.http.as_ref(), &cfg, &saved, now) {
            Ok(fresh) => {
                self.persist(&fresh, send);
                self.session = Some(fresh);
                send(Evt::LoggedIn);
            }
            Err(AuthError::Rejected) => {
                store::delete(&self.session_path);
                send(Evt::LoginRequired);
            }
            // Offline or server trouble: a not-yet-expired token still works.
            Err(e) if saved.expires_at > now => {
                send(Evt::Error(format!("Could not refresh the session: {e}")));
                self.session = Some(saved);
                send(Evt::LoggedIn);
            }
            Err(e) => send(Evt::LoginFailed(format!("Could not restore your session: {e}. Press OK to sign in again."))),
        }
    }

    fn login(&mut self, send: &dyn Fn(Evt)) {
        let cfg = match &self.cfg {
            Ok(c) => c.clone(),
            Err(e) => return send(Evt::LoginFailed(e.clone())),
        };
        for _ in 0..MAX_CODE_REFRESHES {
            let dc = match auth::request_device_code(self.http.as_ref(), &cfg) {
                Ok(dc) => dc,
                Err(e) => return send(Evt::LoginFailed(e.to_string())),
            };
            send(Evt::LoginCode {
                user_code: dc.user_code.clone(),
                verify_url: dc.verification_uri.clone(),
                qr_url: dc.qr_url(),
                expires_in: dc.expires_in,
            });
            match auth::poll_for_session(self.http.as_ref(), &cfg, &dc, &*self.clock, &*self.sleeper) {
                Ok(s) => {
                    self.persist(&s, send);
                    self.session = Some(s);
                    return send(Evt::LoggedIn);
                }
                Err(AuthError::Expired) => continue, // issue a fresh code automatically
                Err(e) => return send(Evt::LoginFailed(format!("{e}. Press OK to try again."))),
            }
        }
        send(Evt::LoginFailed("Timed out waiting for approval. Press OK to try again.".into()));
    }

    fn persist(&self, s: &Session, send: &dyn Fn(Evt)) {
        if let Err(e) = store::save(&self.session_path, s) {
            send(Evt::Error(format!("Could not save the session: {e}")));
        }
    }
}

#[cfg(test)]
mod tests;
