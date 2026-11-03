//! The real backend, built on the `tidlers` crate (feature `tidlers`).
//!
//! * Credentials: none are configured here. `TidalAuth::with_oauth()` uses the library's own
//!   default client credentials; nothing in this app stores or reads a client id/secret.
//! * Only the refresh token is persisted. Every boot does one forced refresh,
//!   which also populates the user info the library needs for favourites.
//! * The library is async; this thread owns a current-thread tokio runtime and
//!   blocks on one command at a time.

mod map;

use crate::diag;
use crate::paths::data_dir;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};
use std::sync::mpsc::Sender;
use tide_core::services::{BackendSide, Cmd, Evt, PlayerCmd, StreamSource, StreamSpec};
use tidlers::{
    auth::TidalAuth,
    client::models::{
        playback::AudioQuality,
        track::{
            config::TrackPlaybackInfoConfig,
            playback::ParsedTrackManifest,
        },
    },
    requests::RequestClientError,
    TidalClient, TidalError,
};
use tokio::runtime::{Builder, Runtime};

const MAX_CODE_REFRESHES: u32 = 6;
const FAVORITES_PAGE: u32 = 300;
const HOME_ITEMS: u32 = 40;

#[derive(Serialize, Deserialize)]
struct Saved {
    refresh_token: String,
}

pub struct Backend {
    rt: Runtime,
    session_path: PathBuf,
    client: Option<TidalClient>,
    player: Option<Sender<PlayerCmd>>,
}

pub fn run(be: BackendSide) {
    let mut b = Backend::new(data_dir().join("tidlers-session.json")).with_player(be.player.clone());
    while let Ok(cmd) = be.rx.recv() {
        b.handle(cmd, &|e| {
            let _ = be.tx.send(e);
        });
    }
}

/// Only a definitive "your credentials/refresh token are bad" answer counts.
fn is_rejected(e: &TidalError) -> bool {
    match e {
        TidalError::RequestClient(RequestClientError::StatusCode { status, .. }) => {
            matches!(status.as_u16(), 400 | 401 | 403)
        }
        TidalError::RequestClient(RequestClientError::Unauthorized | RequestClientError::InvalidCredentials) => true,
        _ => false,
    }
}

impl Backend {
    pub fn new(session_path: PathBuf) -> Self {
        let rt = Builder::new_current_thread().enable_all().build().expect("tokio runtime");
        Self { rt, session_path, client: None, player: None }
    }

    pub fn with_player(mut self, player: Sender<PlayerCmd>) -> Self {
        self.player = Some(player);
        self
    }

    fn new_client() -> TidalClient {
        let auth = TidalAuth::with_oauth();
        let mut client = TidalClient::new(&auth);
        client.set_audio_quality(AudioQuality::High);
        client
    }

    pub fn handle(&mut self, cmd: Cmd, send: &dyn Fn(Evt)) {
        match cmd {
            Cmd::Boot => self.boot(send),
            Cmd::StartLogin => self.login(send),
            Cmd::Logout => {
                self.client = None;
                let _ = fs::remove_file(&self.session_path);
                send(Evt::LoginRequired);
            }
            Cmd::LoadHome => self.load_home(send),
            Cmd::LoadLiked => self.load_liked(send),
            Cmd::LoadLyrics(id) => self.load_lyrics(id, send),
            Cmd::Play { track_id, duration } => self.play(track_id, duration, send),
        }
    }

    fn save(&self, refresh_token: &str, send: &dyn Fn(Evt)) {
        let result = (|| -> std::io::Result<()> {
            if let Some(dir) = self.session_path.parent() {
                fs::create_dir_all(dir)?;
            }
            let tmp = self.session_path.with_extension("tmp");
            fs::write(&tmp, serde_json::to_vec(&Saved { refresh_token: refresh_token.to_string() })?)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
            }
            fs::rename(tmp, &self.session_path)
        })();
        match result {
            Ok(()) => diag::log(&format!("session saved to {}", self.session_path.display())),
            Err(e) => {
                diag::log(&format!("could not save the session to {}: {e}", self.session_path.display()));
                send(Evt::Error(format!("Could not save the session: {e}")));
            }
        }
    }

    fn boot(&mut self, send: &dyn Fn(Evt)) {
        diag::log(&format!("boot: session file {}", self.session_path.display()));
        let saved: Option<Saved> = match fs::read_to_string(&self.session_path) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(s) => Some(s),
                Err(e) => {
                    diag::log(&format!("boot: session file unreadable: {e}"));
                    None
                }
            },
            Err(e) => {
                diag::log(&format!("boot: no session file ({e})"));
                None
            }
        };
        let Some(saved) = saved else { return send(Evt::LoginRequired) };

        let mut client = Self::new_client();
        client.session.auth.refresh_token = Some(saved.refresh_token);
        match self.rt.block_on(client.refresh_access_token(true)) {
            Ok(_) => {
                diag::log("boot: session restored");
                self.client = Some(client);
                send(Evt::LoggedIn);
            }
            Err(e) if is_rejected(&e) => {
                // Keep the file: a successful login overwrites it, and a wrongly
                // classified error must not erase a good token. Only Logout deletes it.
                diag::log(&format!("boot: refresh rejected: {e}"));
                send(Evt::LoginRequired);
            }
            Err(e) => {
                diag::log(&format!("boot: refresh failed: {e}"));
                send(Evt::LoginFailed(format!("Could not restore your session: {e}. Press OK to sign in again.")));
            }
        }
    }

    fn login(&mut self, send: &dyn Fn(Evt)) {
        for _ in 0..MAX_CODE_REFRESHES {
            let mut client = Self::new_client();
            let link = match self.rt.block_on(client.get_oauth_link()) {
                Ok(l) => l,
                Err(e) => return send(Evt::LoginFailed(e.to_string())),
            };
            let qr = link.verification_uri_complete.clone();
            send(Evt::LoginCode {
                user_code: link.user_code.clone(),
                verify_url: link.verification_uri.clone(),
                qr_url: if qr.starts_with("http") { qr } else { format!("https://{qr}") },
                expires_in: link.expires_in as u32,
            });
            let waited = self.rt.block_on(client.wait_for_oauth(&link.device_code, link.expires_in, link.interval, None));
            match waited {
                Ok(tokens) => {
                    self.save(&tokens.refresh_token, send);
                    self.client = Some(client);
                    return send(Evt::LoggedIn);
                }
                // Polling window elapsed: issue a fresh code.
                Err(TidalError::RequestClient(RequestClientError::Timeout)) => continue,
                Err(e) => return send(Evt::LoginFailed(format!("{e}. Press OK to try again."))),
            }
        }
        send(Evt::LoginFailed("Timed out waiting for approval. Press OK to try again.".into()));
    }

    /// True when signed in; refreshes the access token first if it is about to expire.
    fn ensure_ready(&mut self, send: &dyn Fn(Evt)) -> bool {
        let Some(client) = self.client.as_mut() else {
            send(Evt::Error("Not signed in".into()));
            return false;
        };
        if let Err(e) = self.rt.block_on(client.refresh_access_token(false)) {
            send(Evt::Error(format!("Could not refresh the session: {e}")));
        }
        true
    }

    fn load_liked(&mut self, send: &dyn Fn(Evt)) {
        if !self.ensure_ready(send) {
            return;
        }
        let Some(client) = self.client.as_ref() else { return };
        match self.rt.block_on(client.get_collection_track_favorites(Some(FAVORITES_PAGE), Some(0))) {
            Ok(r) => send(Evt::Liked(r.items.iter().map(|e| map::map_track(&e.item)).collect())),
            Err(e) => send(Evt::Error(format!("Could not load liked songs: {e}"))),
        }
    }

    fn load_home(&mut self, send: &dyn Fn(Evt)) {
        if !self.ensure_ready(send) {
            return;
        }
        let Some(client) = self.client.as_ref() else { return };
        match self.rt.block_on(client.get_home_feed(HOME_ITEMS)) {
            Ok(feed) => send(Evt::Home(map::map_home(&feed))),
            Err(e) => send(Evt::Error(format!("Could not load Home: {e}"))),
        }
    }

    /// Streaming quality: `TIDE_QUALITY` = low | high | lossless (default) | hires.
    fn quality() -> AudioQuality {
        match std::env::var("TIDE_QUALITY").unwrap_or_default().to_ascii_lowercase().as_str() {
            "low" => AudioQuality::Low,
            "high" => AudioQuality::High,
            "hires" => AudioQuality::HiRes,
            _ => AudioQuality::Lossless,
        }
    }

    fn play(&mut self, id: u64, duration: f32, send: &dyn Fn(Evt)) {
        if !self.ensure_ready(send) {
            return;
        }
        let Some(client) = self.client.as_ref() else { return };
        let cfg = TrackPlaybackInfoConfig { audio_quality: Some(Self::quality()), ..Default::default() };
        let info = match self.rt.block_on(client.get_track_postpaywall_playback_info(id.to_string(), Some(cfg))) {
            Ok(i) => i,
            Err(e) => return send(Evt::PlaybackError(format!("Could not get the stream: {e}"))),
        };
        let spec = match &info.manifest_parsed {
            Some(ParsedTrackManifest::Json(m)) => {
                // Only clear streams are played. Encrypted ones are refused: this app does
                // not implement decryption of protected content.
                if !m.encryption_type.eq_ignore_ascii_case("NONE") {
                    return send(Evt::PlaybackError("This stream is protected and cannot be played here".into()));
                }
                let Some(url) = m.urls.first() else {
                    return send(Evt::PlaybackError("The service returned no stream URL".into()));
                };
                StreamSpec {
                    track_id: id,
                    mime: m.mime_type.clone(),
                    duration,
                    source: StreamSource::Single(url.clone()),
                    tier: Some(info.audio_quality.clone()),
                }
            }
            Some(ParsedTrackManifest::Dash(d)) => {
                let (Some(init), Some(template)) = (d.get_init_url(), d.get_media_template()) else {
                    return send(Evt::PlaybackError("Unsupported DASH manifest".into()));
                };
                let segment_secs = match (d.duration, d.timescale) {
                    (Some(du), Some(ts)) if ts > 0 => du as f32 / ts as f32,
                    _ => 4.0,
                };
                StreamSpec {
                    track_id: id,
                    mime: d.mime_type.clone(),
                    duration,
                    source: StreamSource::Segmented {
                        init: init.clone(),
                        template: template.clone(),
                        start_number: d.start_number.unwrap_or(1),
                        segment_secs,
                    },
                    tier: Some(info.audio_quality.clone()),
                }
            }
            None => return send(Evt::PlaybackError("No playable manifest".into())),
        };
        match &self.player {
            Some(p) => {
                let _ = p.send(PlayerCmd::Load(spec));
            }
            None => send(Evt::PlaybackError("No player attached".into())),
        }
    }

    fn load_lyrics(&mut self, id: u64, send: &dyn Fn(Evt)) {
        if !self.ensure_ready(send) {
            return;
        }
        let Some(client) = self.client.as_ref() else { return };
        let (lines, synced) = match self.rt.block_on(client.get_track_lyrics(id.to_string())) {
            Ok(r) => map::parse_lyrics(&r.lyrics),
            Err(_) => (Vec::new(), true), // no lyrics for this track
        };
        send(Evt::Lyrics { track_id: id, lines, synced });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_without_saved_session_requires_login() {
        let path = std::env::temp_dir().join(format!("tide-tl-{}", std::process::id())).join("s.json");
        let mut b = Backend::new(path);
        let out = std::cell::RefCell::new(Vec::new());
        b.handle(Cmd::Boot, &|e| out.borrow_mut().push(e));
        assert!(matches!(&out.borrow()[..], [Evt::LoginRequired]));
    }

    #[test]
    fn rejection_classification() {
        assert!(is_rejected(&TidalError::RequestClient(RequestClientError::Unauthorized)));
        assert!(!is_rejected(&TidalError::NotFound));
        assert!(!is_rejected(&TidalError::RequestClient(RequestClientError::Timeout)));
    }
}
