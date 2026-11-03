//! The real backend, built on the `tidlers` crate (feature `tidlers`).
//!
//! * Credentials: none are configured here. `TidalAuth::with_oauth()` uses the library's own
//!   default client credentials; nothing in this app stores or reads a client id/secret.
//! * Only the refresh token is persisted (the library's refresh never rotates it). Every boot does
//!   one forced refresh, which also populates the user info the library needs for favourites.
//!   The token is kept in several locations (see `paths::session_files`), written with fsync, and
//!   a transient failure at boot (network not up yet right after the TV powers on) is retried and
//!   never throws the saved token away.
//! * The library is async; this thread owns a current-thread tokio runtime and
//!   blocks on one command at a time.

mod map;

use crate::diag;
use crate::paths::session_files;
use serde::{Deserialize, Serialize};
use std::{
    error::Error as StdError,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::mpsc::Sender,
    thread,
    time::Duration,
};
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
/// Waits between attempts to restore the saved session when the service cannot be reached
/// (right after power-on the TV's network is often not up yet): ~26 s in total.
const RESTORE_RETRY_DELAYS_SECS: [u64; 4] = [2, 4, 8, 12];
const FAVORITES_PAGE: u32 = 300;
const HOME_ITEMS: u32 = 40;

#[derive(Serialize, Deserialize)]
struct Saved {
    refresh_token: String,
}

/// Result of trying to bring back the saved session.
enum Restore {
    Restored,
    /// Nothing saved (or nothing readable).
    NoSession,
    /// The service definitively refused the saved token: a new sign-in is needed.
    Rejected,
    /// Could not decide (network down, 5xx, timeout...): the saved token is still good as far as we know.
    Transient,
}

pub struct Backend {
    rt: Runtime,
    /// Every place the session is read from / written to (best first).
    session_paths: Vec<PathBuf>,
    retry_delays: Vec<Duration>,
    /// The last boot could not reach the service: the next sign-in request retries the
    /// restore first instead of sending the user through the device-code flow.
    restore_pending: bool,
    client: Option<TidalClient>,
    player: Option<Sender<PlayerCmd>>,
}

pub fn run(be: BackendSide) {
    let mut b = Backend::new(session_files()).with_player(be.player.clone());
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

/// `Display` of the error plus its whole `source()` chain: `reqwest` errors alone say only
/// "failed to do request", which hides whether it was DNS, TLS, a timeout...
fn describe(e: &dyn StdError) -> String {
    let mut out = e.to_string();
    let mut cur = e.source();
    while let Some(c) = cur {
        out.push_str(": ");
        out.push_str(&c.to_string());
        cur = c.source();
    }
    out
}

fn read_token(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let saved: Saved = serde_json::from_str(&text).ok()?;
    (!saved.refresh_token.is_empty()).then_some(saved.refresh_token)
}

/// Write atomically and durably: temp file (owner-only on unix) -> fsync -> rename -> fsync the
/// directory. Without the fsyncs a TV that is switched off right after sign-in can lose the file.
fn write_session(path: &Path, token: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let data = serde_json::to_vec(&Saved { refresh_token: token.to_string() }).map_err(io::Error::other)?;
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        // Mode at creation: nothing to fail on file systems without permissions (e.g. FAT).
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    {
        let mut f = opts.open(&tmp)?;
        f.write_all(&data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    #[cfg(unix)]
    if let Some(dir) = path.parent() {
        if let Ok(d) = fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

impl Backend {
    pub fn new(session_paths: Vec<PathBuf>) -> Self {
        let rt = Builder::new_current_thread().enable_all().build().expect("tokio runtime");
        let retry_delays = RESTORE_RETRY_DELAYS_SECS.iter().map(|s| Duration::from_secs(*s)).collect();
        Self { rt, session_paths, retry_delays, restore_pending: false, client: None, player: None }
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
                self.restore_pending = false;
                for p in &self.session_paths {
                    let _ = fs::remove_file(p);
                }
                send(Evt::LoginRequired);
            }
            Cmd::LoadHome => self.load_home(send),
            Cmd::LoadLiked => self.load_liked(send),
            Cmd::LoadLyrics(id) => self.load_lyrics(id, send),
            Cmd::Play { track_id, duration } => self.play(track_id, duration, send),
        }
    }

    /// Write the token to every location and read it back to prove it landed. Returns how many
    /// copies are good. `send` is told only when none could be written.
    fn save(&self, refresh_token: &str, send: &dyn Fn(Evt)) -> usize {
        let mut good = 0;
        for path in &self.session_paths {
            match write_session(path, refresh_token) {
                Ok(()) if read_token(path).as_deref() == Some(refresh_token) => {
                    diag::log(&format!("session saved to {}", path.display()));
                    good += 1;
                }
                Ok(()) => diag::log(&format!("session written to {} but reading it back failed", path.display())),
                Err(e) => diag::log(&format!("could not save the session to {}: {e}", path.display())),
            }
        }
        if good == 0 {
            send(Evt::Error("Could not save the session: you will have to sign in again next time.".into()));
        }
        good
    }

    /// The saved refresh token: the first location that holds a valid one.
    fn load_saved(&self) -> Option<String> {
        for path in &self.session_paths {
            match read_token(path) {
                Some(t) => {
                    diag::log(&format!("session found in {}", path.display()));
                    return Some(t);
                }
                None => diag::log(&format!("no usable session in {}", path.display())),
            }
        }
        None
    }

    /// Bring back the saved session. Network-type failures are retried with a back-off and are
    /// never reported as "sign in again": only a definitive refusal is.
    fn restore(&mut self, send: &dyn Fn(Evt)) -> Restore {
        let Some(token) = self.load_saved() else { return Restore::NoSession };
        for attempt in 0..=self.retry_delays.len() {
            if attempt > 0 {
                let wait = self.retry_delays[attempt - 1];
                diag::log(&format!("restore: retrying in {}s (attempt {})", wait.as_secs(), attempt + 1));
                thread::sleep(wait);
            }
            let mut client = Self::new_client();
            client.session.auth.refresh_token = Some(token.clone());
            match self.rt.block_on(client.refresh_access_token(true)) {
                Ok(_) => {
                    diag::log("restore: session restored");
                    self.client = Some(client);
                    // Heal: re-create any copy that went missing (volatile dir, wiped app dir...).
                    if self.session_paths.iter().any(|p| read_token(p).as_deref() != Some(token.as_str())) {
                        self.save(&token, send);
                    }
                    return Restore::Restored;
                }
                Err(e) if is_rejected(&e) => {
                    // Keep the files: a successful sign-in overwrites them, and a wrongly
                    // classified error must not erase a good token. Only Logout deletes them.
                    diag::log(&format!("restore: refresh rejected by the service: {}", describe(&e)));
                    return Restore::Rejected;
                }
                Err(e) => {
                    diag::log(&format!("restore: refresh failed (attempt {}): {}", attempt + 1, describe(&e)));
                }
            }
        }
        Restore::Transient
    }

    fn restore_failed_message() -> String {
        "Could not reach the service to restore your session. Check the TV's network connection, then press OK to retry.".into()
    }

    fn boot(&mut self, send: &dyn Fn(Evt)) {
        diag::log(&format!("boot: session locations: {:?}", self.session_paths));
        match self.restore(send) {
            Restore::Restored => {
                self.restore_pending = false;
                send(Evt::LoggedIn);
            }
            Restore::NoSession | Restore::Rejected => {
                self.restore_pending = false;
                send(Evt::LoginRequired);
            }
            Restore::Transient => {
                self.restore_pending = true;
                send(Evt::LoginFailed(Self::restore_failed_message()));
            }
        }
    }

    fn login(&mut self, send: &dyn Fn(Evt)) {
        // "Press OK to retry" after an unreachable service means: try the saved token again, not
        // "authorize this TV from scratch".
        if self.restore_pending {
            match self.restore(send) {
                Restore::Restored => {
                    self.restore_pending = false;
                    return send(Evt::LoggedIn);
                }
                Restore::Transient => return send(Evt::LoginFailed(Self::restore_failed_message())),
                Restore::NoSession | Restore::Rejected => self.restore_pending = false,
            }
        }
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

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tide-tl-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    fn collect(b: &mut Backend, cmd: Cmd) -> Vec<Evt> {
        let out = std::cell::RefCell::new(Vec::new());
        b.handle(cmd, &|e| out.borrow_mut().push(e));
        out.into_inner()
    }

    #[test]
    fn boot_without_saved_session_requires_login() {
        let mut b = Backend::new(vec![tmp("none").join("s.json")]);
        assert!(matches!(&collect(&mut b, Cmd::Boot)[..], [Evt::LoginRequired]));
    }

    #[test]
    fn rejection_classification() {
        assert!(is_rejected(&TidalError::RequestClient(RequestClientError::Unauthorized)));
        assert!(!is_rejected(&TidalError::NotFound));
        assert!(!is_rejected(&TidalError::RequestClient(RequestClientError::Timeout)));
    }

    #[test]
    fn session_roundtrip_and_permissions() {
        let path = tmp("rt").join("s.json");
        write_session(&path, "tok").unwrap();
        assert_eq!(read_token(&path).as_deref(), Some("tok"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // Overwrite keeps working and leaves no temp file behind.
        write_session(&path, "tok2").unwrap();
        assert_eq!(read_token(&path).as_deref(), Some("tok2"));
        assert!(!path.with_extension("tmp").exists());
    }

    #[test]
    fn empty_or_corrupt_file_is_not_a_session() {
        let path = tmp("bad").join("s.json");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"").unwrap();
        assert!(read_token(&path).is_none());
        fs::write(&path, b"{\"refresh_token\":\"\"}").unwrap();
        assert!(read_token(&path).is_none());
    }

    #[test]
    fn save_writes_every_location_and_load_skips_a_broken_one() {
        let (a, c) = (tmp("a").join("s.json"), tmp("c").join("s.json"));
        let b = Backend::new(vec![a.clone(), c.clone()]);
        assert_eq!(b.save("tok", &|_| {}), 2);
        // First copy lost/corrupt: the second one still restores the session.
        fs::write(&a, b"garbage").unwrap();
        assert_eq!(b.load_saved().as_deref(), Some("tok"));
    }

    #[test]
    fn save_survives_one_unwritable_location() {
        let good = tmp("good").join("s.json");
        // A path whose parent is a regular file cannot be created.
        let blocker = tmp("blocker");
        fs::create_dir_all(&blocker).unwrap();
        let file = blocker.join("file");
        fs::write(&file, b"x").unwrap();
        let bad = file.join("s.json");
        let b = Backend::new(vec![bad, good.clone()]);
        let errors = std::cell::Cell::new(0);
        assert_eq!(b.save("tok", &|e| if matches!(e, Evt::Error(_)) { errors.set(errors.get() + 1) }), 1);
        assert_eq!(errors.get(), 0, "one good copy is enough: no error shown");
        assert_eq!(read_token(&good).as_deref(), Some("tok"));
    }

    #[test]
    fn save_reports_when_nothing_could_be_written() {
        let blocker = tmp("blocker2");
        fs::create_dir_all(&blocker).unwrap();
        let file = blocker.join("file");
        fs::write(&file, b"x").unwrap();
        let b = Backend::new(vec![file.join("s.json")]);
        let out = std::cell::RefCell::new(Vec::new());
        assert_eq!(b.save("tok", &|e| out.borrow_mut().push(e)), 0);
        assert!(matches!(&out.borrow()[..], [Evt::Error(_)]));
    }

    #[test]
    fn logout_deletes_every_copy() {
        let (a, c) = (tmp("la").join("s.json"), tmp("lc").join("s.json"));
        let mut b = Backend::new(vec![a.clone(), c.clone()]);
        b.save("tok", &|_| {});
        assert!(matches!(&collect(&mut b, Cmd::Logout)[..], [Evt::LoginRequired]));
        assert!(!a.exists() && !c.exists());
    }

    #[test]
    fn unreachable_service_keeps_the_token_and_asks_to_retry_not_to_reauthorize() {
        // Port 1 on localhost refuses connections quickly, standing in for "network not up".
        // We cannot redirect the library's host here, so only check the no-delay offline path
        // when the sandbox really has no route; otherwise skip.
        let path = tmp("net").join("s.json");
        write_session(&path, "tok").unwrap();
        let mut b = Backend::new(vec![path.clone()]);
        b.retry_delays = vec![]; // no sleeping in tests
        let evts = collect(&mut b, Cmd::Boot);
        match &evts[..] {
            // Offline: transient -> LoginFailed, token kept, retry flag set.
            [Evt::LoginFailed(_)] => {
                assert!(b.restore_pending);
                assert_eq!(read_token(&path).as_deref(), Some("tok"));
            }
            // Online with a fake token the service rejects: sign-in required, file still kept.
            [Evt::LoginRequired] => assert_eq!(read_token(&path).as_deref(), Some("tok")),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
