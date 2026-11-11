//! The backend, built on the `tidlers` crate.
//!
//! * Credentials: the PKCE flow signs in with the client named by `TIDAL_CLIENT_ID` /
//!   `TIDAL_CLIENT_SECRET`, because the authorization code belongs to that client and every refresh
//!   afterwards has to authenticate as it. Unset, the client embedded in the library is used: its
//!   `redirect_uri` is the service's own page, so the phone brings the code back by hand through the
//!   relay's paste page. The device-code fallback keeps the library's OAuth client.
//! * Only the refresh token is persisted (the library's refresh never rotates it). Every boot does
//!   one forced refresh, which also populates the user info the library needs for favourites.
//!   The token is kept in several locations (see `paths::session_files`), written with fsync, and
//!   a transient failure at boot (network not up yet right after the TV powers on) is retried and
//!   never throws the saved token away.
//! * The library is async; this thread owns a current-thread tokio runtime and
//!   blocks on one command at a time.

mod home;
mod map;
mod relay;

use crate::diag;
use crate::paths::session_files;
use crate::settings;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    error::Error as StdError,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, Sender},
    thread,
    time::{Duration, Instant},
};
use tide_core::i18n;
use tide_core::model::Account;
use tide_core::quality::Quality;
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
/// Wait between relay polls, taken in small steps so a cancel is noticed within one of them.
const POLL_STEP: Duration = Duration::from_millis(150);
const POLL_STEPS: u32 = 10;
const HOME_ITEMS: u32 = 40;

#[derive(PartialEq, Serialize, Deserialize)]
struct Saved {
    refresh_token: String,
    /// Which sign-in produced the token. PKCE and device-code tokens belong to different clients,
    /// so the refresh has to use the same one.
    #[serde(default)]
    flow: Flow,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Flow {
    /// Device-code fallback, and what a session file written before PKCE existed means.
    #[default]
    Device,
    Pkce,
}

/// How one PKCE attempt ended.
enum PkceAttempt {
    /// Signed in.
    Done,
    /// The user cancelled, or left the flow.
    Cancelled,
    /// A new `Cmd::StartLogin` arrived while waiting: start over.
    Restart,
    /// The user asked for the device-code flow while waiting.
    Fallback,
    /// PKCE cannot work here, or the token exchange failed: offer the fallback.
    GiveUp(String),
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
    /// Current app language (`pt-BR`). Every client built from now on gets it (see `new_client`) and
    /// `Cmd::SetLocale` applies it to the one already signed in.
    locale: String,
    /// Highest sound quality the account can stream (`HI_RES_LOSSLESS`, `LOSSLESS`...), read at
    /// sign-in or on the first play. `None` while unknown.
    max_quality: Option<String>,
    /// Quality the user asked for in Settings (`Max` = the best the account allows).
    preference: Quality,
    client: Option<TidalClient>,
    player: Option<Sender<PlayerCmd>>,
    /// The command channel, so a blocking sign-in can notice a cancel or a fallback request.
    /// `None` in unit tests, which call `handle` directly.
    cmds: Option<Receiver<Cmd>>,
    /// Commands that arrived while a sign-in was blocking; handled right after it returns.
    deferred: VecDeque<Cmd>,
}

pub fn run(be: BackendSide) {
    let saved = settings::Settings::load();
    let mut b = Backend::new(session_files())
        .with_commands(be.rx)
        .with_player(be.player.clone())
        .with_locale(&saved.locale)
        .with_quality(Quality::parse(&saved.audio_quality).unwrap_or_default());
    let send = |e: Evt| {
        let _ = be.tx.send(e);
    };
    loop {
        let Ok(cmd) = b.recv() else { break };
        b.handle(cmd, &send);
        // Commands read while a sign-in was blocking and only handled now.
        while let Some(cmd) = b.deferred.pop_front() {
            b.handle(cmd, &send);
        }
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

/// DASH URLs come out of the manifest's XML with the entities still in place. A signed stream URL
/// that keeps `&amp;` loses its `token` parameter on the way to the CDN, which answers 403.
fn xml_unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

/// Directory a manifest's relative URLs are resolved against: its `<BaseURL>` entry, else the
/// directory of any absolute URL in it (DASH keeps the init segment next to the segments).
fn dash_base(urls: &[String], media: &str) -> Option<String> {
    let media = xml_unescape(media);
    let parent = |u: &str| u.rsplit_once('/').map(|(dir, _)| format!("{dir}/"));
    if let Some(found) = urls
        .iter()
        .map(|u| xml_unescape(u))
        .find(|u| u.starts_with("http") && !u.contains("$Number$") && *u != media)
    {
        return if found.ends_with('/') { Some(found) } else { parent(&found) };
    }
    // No absolute `<BaseURL>`: the segments' own directory is it.
    if media.starts_with("http") {
        return parent(&media);
    }
    None
}

/// A manifest URL ready to fetch: entities undone and relative paths joined to `base`.
fn dash_url(raw: Option<&str>, base: Option<&str>) -> Option<String> {
    let raw = xml_unescape(raw?);
    // A bare directory is not a resource; fetching it only earns a 403. This is also what the
    // crate's own `get_init_url` hands over when the manifest has a `<BaseURL>` but no
    // `initialization`.
    if raw.is_empty() || raw.ends_with('/') {
        return None;
    }
    if raw.starts_with("http") {
        return Some(raw);
    }
    Some(format!("{}{}", base?, raw.trim_start_matches('/')))
}

/// `host/path` plus the query parameter *names*, never their values: those are signed tokens.
fn url_shape(url: &str) -> String {
    let (rest, query) = match url.split_once('?') {
        Some((a, b)) => (a, Some(b)),
        None => (url, None),
    };
    let mut out = rest.split_once("://").map_or(rest, |(_, host)| host).to_string();
    if let Some(q) = query {
        let names: Vec<&str> = q
            .split('&')
            .filter_map(|p| p.split('=').next())
            .filter(|n| !n.is_empty())
            .collect();
        out.push_str(&format!(" [?{}]", names.join(",")));
    }
    if out.len() > 100 {
        out.truncate(100);
        out.push_str("...");
    }
    out
}

/// The quality the account's subscription allows, from the service's `highestSoundQuality` label.
/// `HI_RES` and `HI_RES_LOSSLESS` both mean HiRes FLAC: the first is what most HiRes subscriptions
/// report, the second is the newer label. `None` (unknown, or a label this app does not know) caps
/// nothing, so the user's preference is asked for as-is and the service answers with what it can.
fn ceiling_for(account: Option<&str>) -> Option<Quality> {
    let account = account?.trim().to_ascii_uppercase();
    if account.contains("HI_RES") || account.contains("HIRES") {
        return Some(Quality::Max);
    }
    if account.contains("LOSSLESS") {
        return Some(Quality::Lossless);
    }
    if account.contains("HIGH") {
        return Some(Quality::High);
    }
    if account.contains("LOW") {
        return Some(Quality::Low);
    }
    None
}

/// What the `tidlers` client calls each of our levels.
fn api_quality(quality: Quality) -> AudioQuality {
    match quality {
        Quality::Low => AudioQuality::Low,
        Quality::High => AudioQuality::High,
        Quality::Lossless => AudioQuality::Lossless,
        // The client has no separate HiRes-Lossless level; HiRes is FLAC 24-bit.
        Quality::Max => AudioQuality::HiRes,
    }
}

/// Quality to ask the service for: the user's preference, capped by the account's ceiling. The
/// default preference is `Max`, which gives "always the best the account can stream".
/// `TIDAL_QUALITY` (`low`, `high`, `lossless`, `hires`) still overrides both, for debugging.
fn quality_for(account: Option<&str>, forced: Option<&str>, preference: Quality) -> AudioQuality {
    match forced.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
        Some("low") => return AudioQuality::Low,
        Some("high") => return AudioQuality::High,
        Some("lossless") | Some("cd") => return AudioQuality::Lossless,
        Some("hires") | Some("hi_res") => return AudioQuality::HiRes,
        Some(other) => {
            diag::log(&format!("quality: TIDAL_QUALITY={other} is not low|high|lossless|hires, using the preference"));
        }
        None => {}
    }
    api_quality(preference.capped(ceiling_for(account)))
}

fn read_saved(path: &Path) -> Option<Saved> {
    let text = fs::read_to_string(path).ok()?;
    let saved: Saved = serde_json::from_str(&text).ok()?;
    (!saved.refresh_token.is_empty()).then_some(saved)
}

#[cfg(test)]
fn read_token(path: &Path) -> Option<String> {
    read_saved(path).map(|s| s.refresh_token)
}

/// Environment variables naming the PKCE client. With both set, that client signs in; the relay builds
/// the sign-in page from it and the code comes back only if Tidal has registered the relay's `/callback`
/// for it. Unset, the client embedded in `tidlers` is used, and its redirect (the service's own page)
/// means the address has to be pasted on the relay.
const CLIENT_ID_ENV: &str = "TIDAL_CLIENT_ID";
const CLIENT_SECRET_ENV: &str = "TIDAL_CLIENT_SECRET";

/// Credentials of the PKCE client. Only [`ClientCredentials::id`] is ever logged; the secret is not,
/// which is why this has no `Debug`.
struct ClientCredentials {
    id: String,
    secret: String,
}

impl ClientCredentials {
    /// Both halves, trimmed, or nothing: half a pair is a configuration mistake, not a client.
    fn parse(id: Option<String>, secret: Option<String>) -> Option<Self> {
        fn clean(value: Option<String>) -> Option<String> {
            value.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
        }
        Some(Self { id: clean(id)?, secret: clean(secret)? })
    }

    fn from_env() -> Option<Self> {
        Self::parse(std::env::var(CLIENT_ID_ENV).ok(), std::env::var(CLIENT_SECRET_ENV).ok())
    }
}

/// Whether an environment variable carries a value, without ever looking at it.
fn env_present(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| !v.trim().is_empty())
}

/// Make `auth` authenticate as `credentials`, in both views the crate keeps: `pkce_config` is what the
/// code exchange and the refresh grant read, `client_id`/`client_secret` are the client-credentials
/// pair. The PKCE pair itself (verifier, challenge, device key) is untouched.
fn use_pkce_credentials(auth: &mut TidalAuth, credentials: &ClientCredentials) {
    auth.pkce_config.client_id = credentials.id.clone();
    auth.pkce_config.client_secret = credentials.secret.clone();
    auth.client_id = credentials.id.clone();
    auth.client_secret = credentials.secret.clone();
}

/// `TidalAuth` for `flow`.
///
/// PKCE: the verifier is still the crate's, but the client is the one named by [`CLIENT_ID_ENV`] and
/// [`CLIENT_SECRET_ENV`] when both are set. The challenge that client is asked to authorize, the
/// exchange that follows and every refresh for the rest of the session then belong to the same client.
/// Adopting another client's id while keeping the crate's secret is what made a PKCE session die on its
/// first refresh. With the variables unset, the crate's own pair is used end to end — redirect included.
fn auth_for(flow: Flow) -> TidalAuth {
    if flow == Flow::Device {
        // The device-code flow signs in with the crate's own OAuth client: another client, another
        // token, so it keeps its own credentials (that is why `Flow` is saved with the token).
        return TidalAuth::with_oauth();
    }
    let mut auth = TidalAuth::with_pkce();
    match ClientCredentials::from_env() {
        Some(credentials) => {
            use_pkce_credentials(&mut auth, &credentials);
            diag::log(&format!("auth: pkce client {} (from {CLIENT_ID_ENV})", credentials.id));
        }
        None => diag::log(&format!(
            "auth: {CLIENT_ID_ENV}={}, {CLIENT_SECRET_ENV}={} (both are needed): using the PKCE client embedded in tidlers, whose redirect is the service's own page (the phone pastes the address on the relay)",
            if env_present(CLIENT_ID_ENV) { "set" } else { "missing" },
            if env_present(CLIENT_SECRET_ENV) { "set" } else { "missing" },
        )),
    }
    auth
}

/// Write atomically and durably: temp file (owner-only on unix) -> fsync -> rename -> fsync the
/// directory. Without the fsyncs a TV that is switched off right after sign-in can lose the file.
fn write_session(path: &Path, token: &str, flow: Flow) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let data = serde_json::to_vec(&Saved { refresh_token: token.to_string(), flow }).map_err(io::Error::other)?;
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
        Self {
            rt,
            session_paths,
            retry_delays,
            restore_pending: false,
            locale: i18n::DEFAULT_LOCALE.to_string(),
            max_quality: None,
            preference: Quality::default(),
            client: None,
            player: None,
            cmds: None,
            deferred: VecDeque::new(),
        }
    }

    /// Give the backend the command channel (see `Backend::cmds`). `run` does this.
    pub fn with_commands(mut self, cmds: Receiver<Cmd>) -> Self {
        self.cmds = Some(cmds);
        self
    }

    fn recv(&self) -> Result<Cmd, std::sync::mpsc::RecvError> {
        self.cmds.as_ref().expect("commands are attached by run()").recv()
    }

    pub fn with_player(mut self, player: Sender<PlayerCmd>) -> Self {
        self.player = Some(player);
        self
    }

    pub fn with_locale(mut self, code: &str) -> Self {
        self.locale = i18n::resolve(code).to_string();
        self
    }

    pub fn with_quality(mut self, quality: Quality) -> Self {
        self.preference = quality;
        self
    }

    /// A client that can refresh (and sign in) with the client credentials of `flow`: a PKCE token
    /// belongs to the PKCE client and a device-code token to the other one.
    fn new_client(&self, flow: Flow) -> TidalClient {
        let auth = auth_for(flow);
        let mut client = TidalClient::new(&auth);
        // The real level is chosen per play (see `quality`); this is only the client's own default.
        client.set_audio_quality(api_quality(self.preference));
        // `session.locale` is a plain `String` in tidlers 0.5.0 and uses `pt_BR`, not `pt-BR`.
        client.session.locale = map::tidal_locale(&self.locale);
        client
    }

    pub fn handle(&mut self, cmd: Cmd, send: &dyn Fn(Evt)) {
        match cmd {
            Cmd::Boot => self.boot(send),
            Cmd::StartLogin => self.login(send),
            // Explicit choice of the reduced-quality flow (the dialog after a failed PKCE).
            Cmd::StartLoginFallback => self.login_device(send),
            // Only meaningful inside a PKCE wait, where `poll_cmd` takes it (see `login_pkce`).
            Cmd::CancelLogin => {}
            Cmd::Logout => {
                self.client = None;
                self.max_quality = None;
                self.restore_pending = false;
                for p in &self.session_paths {
                    let _ = fs::remove_file(p);
                }
                send(Evt::LoginRequired);
            }
            Cmd::LoadHome => self.load_home(send),
            Cmd::LoadLiked => self.load_liked(send),
            Cmd::LoadLyrics(id) => self.load_lyrics(id, send),
            Cmd::LoadAccount => self.load_account(send),
            Cmd::SetLocale(code) => self.set_locale(&code, send),
            Cmd::SetQuality(quality) => self.set_quality(quality, send),
            Cmd::Play { track_id, duration } => self.play(track_id, duration, send),
        }
    }

    /// Write the token to every location and read it back to prove it landed. Returns how many
    /// copies are good. `send` is told only when none could be written.
    fn save(&self, refresh_token: &str, flow: Flow, send: &dyn Fn(Evt)) -> usize {
        let mut good = 0;
        let want = Saved { refresh_token: refresh_token.to_string(), flow };
        for path in &self.session_paths {
            match write_session(path, refresh_token, flow) {
                Ok(()) if read_saved(path).as_ref() == Some(&want) => {
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

    /// The saved session: the first location that holds a valid one.
    fn load_saved(&self) -> Option<Saved> {
        for path in &self.session_paths {
            match read_saved(path) {
                Some(saved) => {
                    let flow = if saved.flow == Flow::Pkce { "pkce" } else { "device-code" };
                    diag::log(&format!("session found in {} ({flow} sign-in)", path.display()));
                    return Some(saved);
                }
                None => diag::log(&format!("no usable session in {}", path.display())),
            }
        }
        None
    }

    /// Bring back the saved session. Network-type failures are retried with a back-off and are
    /// never reported as "sign in again": only a definitive refusal is.
    fn restore(&mut self, send: &dyn Fn(Evt)) -> Restore {
        let Some(saved) = self.load_saved() else { return Restore::NoSession };
        let token = saved.refresh_token.clone();
        for attempt in 0..=self.retry_delays.len() {
            if attempt > 0 {
                let wait = self.retry_delays[attempt - 1];
                diag::log(&format!("restore: retrying in {}s (attempt {})", wait.as_secs(), attempt + 1));
                thread::sleep(wait);
            }
            let mut client = self.new_client(saved.flow);
            client.session.auth.refresh_token = Some(token.clone());
            match self.rt.block_on(client.refresh_access_token(true)) {
                Ok(_) => {
                    diag::log("restore: session restored");
                    self.client = Some(client);
                    // Heal: re-create any copy that went missing (volatile dir, wiped app dir...).
                    if self.session_paths.iter().any(|p| read_saved(p).as_ref() != Some(&saved)) {
                        self.save(&token, saved.flow, send);
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
                send(Evt::LoggedIn { max_quality: None });
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
                    return send(Evt::LoggedIn { max_quality: None });
                }
                Restore::Transient => return send(Evt::LoginFailed(Self::restore_failed_message())),
                Restore::NoSession | Restore::Rejected => self.restore_pending = false,
            }
        }
        // PKCE first: it is the only flow that yields HiRes. The device-code flow is offered when it
        // cannot work (or when the user asks for it).
        loop {
            match self.login_pkce(send) {
                PkceAttempt::Done => return,
                PkceAttempt::Cancelled => return send(Evt::LoginCancelled),
                PkceAttempt::Restart => continue,
                PkceAttempt::Fallback => return self.login_device(send),
                PkceAttempt::GiveUp(reason) => return send(Evt::FallbackOffered(reason)),
            }
        }
    }

    /// One command, if any arrived while a sign-in was blocking. A cancel or a switch to the
    /// fallback must act immediately; anything else is kept for after the sign-in.
    fn poll_cmd(&mut self) -> Option<Cmd> {
        let cmd = self.cmds.as_ref()?.try_recv().ok()?;
        match cmd {
            Cmd::CancelLogin | Cmd::StartLoginFallback | Cmd::StartLogin => Some(cmd),
            other => {
                self.deferred.push_back(other);
                None
            }
        }
    }

    /// Primary flow: the phone signs in on the service's page; the relay holds the handoff and this TV
    /// polls it (`docs/relay.md`). The code arrives either from the relay's own `redirect_uri` or — when
    /// the client's registered redirect is the service's page, which is what the library embeds — pasted
    /// by the user on the relay's page. Either way the exchange happens here, with the verifier.
    fn login_pkce(&mut self, send: &dyn Fn(Evt)) -> PkceAttempt {
        let base = relay::base();
        let http = match relay::client() {
            Ok(http) => http,
            Err(e) => return PkceAttempt::GiveUp(format!("Could not prepare the sign-in: {e}")),
        };
        let state = relay::random_state();

        // The crate makes the pair (the verifier stays here, only the challenge travels) over the
        // client of `auth_for`: the same one the relay will name and the session will refresh with.
        let mut client = self.new_client(Flow::Pkce);
        let (challenge, unique_key, own_client_id, own_redirect) = {
            let pkce = &client.session.auth.pkce_config;
            (
                pkce.code_challenge.clone(),
                pkce.client_unique_key.clone(),
                pkce.client_id.clone(),
                pkce.redirect_uri.clone(),
            )
        };

        // The client and the redirect travel with the challenge: the authorize request has to name a
        // pair Tidal registered together. A client of our own is only registered for the relay's
        // `redirect_uri`, so that one is left to the relay; the client the library embeds only for its
        // own page, and the phone then brings the code back. Asking for the wrong pair is what makes
        // `login.tidal.com` answer `11102` before it even shows the sign-in page.
        let custom = ClientCredentials::from_env().is_some();
        let ask = relay::Ask {
            challenge: &challenge,
            unique_key: &unique_key,
            state: &state,
            client_id: &own_client_id,
            redirect_uri: if custom { None } else { Some(own_redirect.as_str()) },
        };
        let handoff = match self.rt.block_on(relay::create(&http, &base, &ask)) {
            Ok(handoff) => handoff,
            Err(e) => return PkceAttempt::GiveUp(format!("Could not ask the relay for a sign-in: {e}")),
        };
        // The code was issued for the relay's client and redirect. Both sides read
        // `TIDAL_CLIENT_ID`, so they agree; if they do not, the exchange might still work but every
        // later refresh would authenticate as another client and the session would be dead on the
        // next boot. Say so instead of signing in to a session that cannot last.
        if handoff.client_id != own_client_id {
            diag::log(&format!(
                "pkce: the relay signs in with client {} but this build has {}: {CLIENT_ID_ENV}/{CLIENT_SECRET_ENV} must be the same on both sides",
                handoff.client_id, own_client_id
            ));
            return PkceAttempt::GiveUp(format!(
                "The relay signs in with client {} but this app is set to {}. Use the same {CLIENT_ID_ENV} and {CLIENT_SECRET_ENV} on both.",
                handoff.client_id, own_client_id
            ));
        }
        client.session.auth.pkce_config.redirect_uri = handoff.redirect_uri.clone();

        // The pair the service validates before it shows the login page: the id and the redirect it has
        // registered for it (`login.tidal.com` answers `11102` when they do not match). Everything the
        // screen needs as a QR code is logged too, so a desktop run can open the links by hand.
        diag::log(&format!(
            "pkce: client {own_client_id}, redirect {}, sign-in page {}, short link {}, paste page {}",
            handoff.redirect_uri,
            handoff.authorize_url,
            handoff.auth_url,
            handoff.paste_url.as_deref().unwrap_or("(none: the code comes back by itself)")
        ));
        send(Evt::PkceLogin {
            qr_url: handoff.authorize_url.clone(),
            paste_url: handoff.paste_url.clone(),
            expires_in: handoff.expires_in.max(1),
        });

        let code = match self.await_code(&http, &base, &handoff, &state) {
            Ok(code) => code,
            Err(attempt) => return attempt,
        };

        // The relay never saw the verifier; the exchange happens here, exactly as it did before.
        let callback = format!(
            "{}?code={}&state={}",
            handoff.redirect_uri,
            relay::url_encode(&code),
            relay::url_encode(&state)
        );
        match self.rt.block_on(client.finish_pkce_login(&callback)) {
            Ok(()) => {
                let token = client.session.auth.refresh_token.clone().unwrap_or_default();
                self.save(&token, Flow::Pkce, send);
                self.client = Some(client);
                let max_quality = self.read_max_quality();
                self.max_quality = max_quality.clone();
                send(Evt::LoggedIn { max_quality });
                PkceAttempt::Done
            }
            Err(e) => {
                // The refused exchange's body names the problem (unknown client, redirect not
                // registered, expired code); it never carries a token.
                diag::log(&format!("pkce: the token exchange failed: {}", describe(&e)));
                PkceAttempt::GiveUp(format!("The service refused the sign-in: {e}"))
            }
        }
    }

    /// Poll the relay until the code arrives, noticing a cancel or a fallback request between polls.
    fn await_code(
        &mut self,
        http: &relay::Http,
        base: &str,
        handoff: &relay::Handoff,
        state: &str,
    ) -> Result<String, PkceAttempt> {
        let deadline = Instant::now() + Duration::from_secs(handoff.expires_in.max(1) as u64);
        let mut failures = 0;
        loop {
            if let Some(attempt) = self.interrupted() {
                return Err(attempt);
            }
            match self.rt.block_on(relay::poll(http, base, &handoff.handoff_id)) {
                Ok(relay::Poll::Pending) => failures = 0,
                Ok(relay::Poll::Code { code, state: returned }) => {
                    if !relay::eq_const_time(returned.as_deref().unwrap_or_default(), state) {
                        diag::log("pkce: the relay answered for another sign-in");
                        return Err(PkceAttempt::GiveUp("The relay answered for another sign-in.".into()));
                    }
                    return Ok(code);
                }
                Ok(relay::Poll::Denied { error, description }) => {
                    let reason = match description {
                        Some(detail) if !detail.is_empty() => format!("{error} ({detail})"),
                        _ => error,
                    };
                    return Err(PkceAttempt::GiveUp(format!("The service refused the sign-in: {reason}")));
                }
                Ok(relay::Poll::Expired) => {
                    return Err(PkceAttempt::GiveUp(
                        "The sign-in link expired before the phone answered.".into(),
                    ));
                }
                Err(e) => {
                    failures += 1;
                    diag::log(&format!("pkce: relay poll failed ({failures}): {e}"));
                    if failures > relay::MAX_POLL_FAILURES {
                        return Err(PkceAttempt::GiveUp(format!("Lost the relay: {e}")));
                    }
                }
            }
            if Instant::now() >= deadline {
                return Err(PkceAttempt::GiveUp("No phone opened the link in time.".into()));
            }
            if let Some(attempt) = self.pause_before_next_poll() {
                return Err(attempt);
            }
        }
    }

    /// A command that must interrupt a wait, as the attempt it ends.
    fn interrupted(&mut self) -> Option<PkceAttempt> {
        self.poll_cmd().map(|cmd| match cmd {
            Cmd::StartLoginFallback => PkceAttempt::Fallback,
            Cmd::StartLogin => PkceAttempt::Restart,
            _ => PkceAttempt::Cancelled,
        })
    }

    /// Wait out one poll interval in small steps, so a cancel is noticed within one step.
    fn pause_before_next_poll(&mut self) -> Option<PkceAttempt> {
        for _ in 0..POLL_STEPS {
            thread::sleep(POLL_STEP);
            if let Some(attempt) = self.interrupted() {
                return Some(attempt);
            }
        }
        None
    }

    /// Fallback: device code (QR + short code on the phone). At most CD quality.
    fn login_device(&mut self, send: &dyn Fn(Evt)) {
        for _ in 0..MAX_CODE_REFRESHES {
            let mut client = self.new_client(Flow::Device);
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
                    self.save(&tokens.refresh_token, Flow::Device, send);
                    self.client = Some(client);
                    let max_quality = self.read_max_quality();
                    self.max_quality = max_quality.clone();
                    return send(Evt::LoggedIn { max_quality });
                }
                // Polling window elapsed: issue a fresh code.
                Err(TidalError::RequestClient(RequestClientError::Timeout)) => continue,
                Err(e) => return send(Evt::LoginFailed(format!("{e}. Press OK to try again."))),
            }
        }
        send(Evt::LoginFailed("Timed out waiting for approval. Press OK to try again.".into()));
    }

    /// Highest sound quality the account can stream, as the service reports it. `None` when the
    /// request fails: a missing label must not block a sign-in that worked.
    fn read_max_quality(&self) -> Option<String> {
        let client = self.client.as_ref()?;
        match self.rt.block_on(client.subscription()) {
            Ok(sub) => {
                diag::log(&format!("account: highest sound quality {}", sub.highest_sound_quality));
                Some(sub.highest_sound_quality)
            }
            Err(e) => {
                diag::log(&format!("account: could not read the subscription: {}", describe(&e)));
                None
            }
        }
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

    /// Runtime quality change: used by the next play (still capped by the account at that point),
    /// applied to the signed-in client and persisted.
    fn set_quality(&mut self, quality: Quality, send: &dyn Fn(Evt)) {
        self.preference = quality;
        if let Some(client) = self.client.as_mut() {
            client.set_audio_quality(api_quality(quality));
        }
        let copies = settings::save_quality(quality.id());
        diag::log(&format!("quality: set to {} ({copies} copies saved)", quality.label()));
        if copies == 0 {
            send(Evt::Error("Could not save the audio quality: it will reset when the app restarts.".into()));
        }
    }

    /// Runtime language change: remembered for every client created later, applied to the signed-in
    /// client right away (its next request already carries the new `locale`) and persisted.
    fn set_locale(&mut self, code: &str, send: &dyn Fn(Evt)) {
        self.locale = i18n::resolve(code).to_string();
        if let Some(client) = self.client.as_mut() {
            client.session.locale = map::tidal_locale(&self.locale);
        }
        let copies = settings::save_locale(&self.locale);
        diag::log(&format!("language set to {} ({copies} copies saved)", self.locale));
        if copies == 0 {
            send(Evt::Error("Could not save the language: it will reset when the app restarts.".into()));
        }
    }

    /// Name and e-mail come from the user info the sign-in already filled in; the plan is one extra
    /// request, and its failure only leaves the plan blank.
    fn load_account(&mut self, send: &dyn Fn(Evt)) {
        if !self.ensure_ready(send) {
            return;
        }
        let Some(client) = self.client.as_ref() else { return };
        let Some(user) = client.user_info.as_ref() else {
            return send(Evt::Error("Could not load the account: no user information".into()));
        };
        let (name, email) = (map::display_name(user), user.email.clone());
        let plan = match self.rt.block_on(client.subscription()) {
            Ok(sub) => Some(map::map_plan(&sub)),
            Err(e) => {
                diag::log(&format!("account: could not load the subscription: {}", describe(&e)));
                None
            }
        };
        send(Evt::Account(Account { name, email, plan }));
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
        match self.rt.block_on(home::fetch(client, HOME_ITEMS)) {
            Ok(p) => {
                for s in &p.skipped {
                    diag::log(&format!("home: skipped {s}"));
                }
                diag::log(&format!("home: {} feed items -> {} shelves, {} skipped", p.seen, p.shelves.len(), p.skipped.len()));
                if p.shelves.is_empty() {
                    send(Evt::Error("The Home feed had no shelves this app understands (details in tide.log)".into()));
                }
                send(Evt::Home(p.shelves));
            }
            Err(e) => {
                send(Evt::Error(format!("Could not load Home: {e}")));
                // log the error too, because the UI may not show it if the user navigates away before the request completes.
                diag::log(&format!("Could not load Home: {e}"));
            }
        }
    }

    /// Streaming quality to ask the service for: the user's preference (default `Max`), capped by
    /// the highest the account can play. `TIDAL_QUALITY` (`low|high|lossless|hires`) still wins when
    /// set, for comparing streams while debugging.
    fn quality(&mut self) -> AudioQuality {
        // Read at sign-in; a restored session reads it once, here, on the first play.
        if self.max_quality.is_none() && self.client.is_some() {
            self.max_quality = self.read_max_quality();
        }
        let forced = std::env::var("TIDAL_QUALITY").ok().filter(|v| !v.trim().is_empty());
        quality_for(self.max_quality.as_deref(), forced.as_deref(), self.preference)
    }

    fn play(&mut self, id: u64, duration: f32, send: &dyn Fn(Evt)) {
        if !self.ensure_ready(send) {
            return;
        }
        // Before borrowing the client: reading the account ceiling needs `&self`.
        let quality = self.quality();
        let cfg = TrackPlaybackInfoConfig { audio_quality: Some(quality.clone()), ..Default::default() };
        let Some(client) = self.client.as_ref() else { return };
        let info = match self.rt.block_on(client.get_track_postpaywall_playback_info(id.to_string(), Some(cfg))) {
            Ok(i) => i,
            Err(e) => return send(Evt::PlaybackError(format!("Could not get the stream: {e}"))),
        };
        // What the service said it is sending. No URL is logged: it carries a signed token.
        diag::log(&format!(
            "play {id}: asked {quality:?} (preference {}, account {}), got {} manifest={} kind={} urls={}",
            self.preference.label(),
            self.max_quality.as_deref().unwrap_or("unknown"),
            info.audio_quality,
            info.manifest_mime_type,
            match &info.manifest_parsed {
                Some(ParsedTrackManifest::Json(_)) => "json",
                Some(ParsedTrackManifest::Dash(_)) => "dash",
                None => "none",
            },
            info.get_stream_urls().map_or(0, |u| u.len()),
        ));
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
                // The crate hands these over straight from the XML: entities still escaped and
                // relative to `<BaseURL>`. A signed URL that keeps `&amp;` reaches the CDN with
                // no `token` at all, which answers 403 (that is the "init segment failed" case).
                let base = dash_base(&d.urls, d.get_media_template().map_or("", String::as_str));
                let init = dash_url(d.initialization_url.as_deref(), base.as_deref());
                let template = dash_url(d.get_media_template().map(String::as_str), base.as_deref());
                let (Some(init), Some(template)) = (init, template) else {
                    diag::log(&format!(
                        "dash: unusable manifest ({}), base={:?}, urls={}",
                        info.manifest_mime_type,
                        base.as_deref().map(url_shape),
                        d.urls.len()
                    ));
                    return send(Evt::PlaybackError(format!(
                        "Unsupported DASH manifest ({})",
                        info.manifest_mime_type
                    )));
                };
                diag::log(&format!("dash: init {}, media {}", url_shape(&init), url_shape(&template)));
                let segment_secs = match (d.duration, d.timescale) {
                    (Some(du), Some(ts)) if ts > 0 => du as f32 / ts as f32,
                    _ => 4.0,
                };
                StreamSpec {
                    track_id: id,
                    mime: d.mime_type.clone(),
                    duration,
                    source: StreamSource::Segmented {
                        init,
                        template,
                        start_number: d.start_number.unwrap_or(1),
                        segment_secs,
                    },
                    tier: Some(info.audio_quality.clone()),
                }
            }
            None => {
                return send(Evt::PlaybackError(format!(
                    "No playable manifest ({})",
                    info.manifest_mime_type
                )))
            }
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
        write_session(&path, "tok", Flow::Device).unwrap();
        assert_eq!(read_token(&path).as_deref(), Some("tok"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // Overwrite keeps working and leaves no temp file behind.
        write_session(&path, "tok2", Flow::Device).unwrap();
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
        assert_eq!(b.save("tok", Flow::Device, &|_| {}), 2);
        // First copy lost/corrupt: the second one still restores the session.
        fs::write(&a, b"garbage").unwrap();
        assert_eq!(b.load_saved().map(|s| s.refresh_token).as_deref(), Some("tok"));
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
        assert_eq!(b.save("tok", Flow::Device, &|e| if matches!(e, Evt::Error(_)) { errors.set(errors.get() + 1) }), 1);
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
        assert_eq!(b.save("tok", Flow::Device, &|e| out.borrow_mut().push(e)), 0);
        assert!(matches!(&out.borrow()[..], [Evt::Error(_)]));
    }

    #[test]
    fn logout_deletes_every_copy() {
        let (a, c) = (tmp("la").join("s.json"), tmp("lc").join("s.json"));
        let mut b = Backend::new(vec![a.clone(), c.clone()]);
        b.save("tok", Flow::Device, &|_| {});
        assert!(matches!(&collect(&mut b, Cmd::Logout)[..], [Evt::LoginRequired]));
        assert!(!a.exists() && !c.exists());
    }

    #[test]
    fn unreachable_service_keeps_the_token_and_asks_to_retry_not_to_reauthorize() {
        // Port 1 on localhost refuses connections quickly, standing in for "network not up".
        // We cannot redirect the library's host here, so only check the no-delay offline path
        // when the sandbox really has no route; otherwise skip.
        let path = tmp("net").join("s.json");
        write_session(&path, "tok", Flow::Device).unwrap();
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

    #[test]
    fn the_pkce_client_comes_from_the_environment_and_keeps_the_pair() {
        let credentials = ClientCredentials::parse(Some(" id ".into()), Some(" secret ".into())).unwrap();
        assert_eq!(credentials.id, "id");
        assert_eq!(credentials.secret, "secret");
        let mut auth = TidalAuth::with_pkce();
        let (challenge, verifier, key) = (
            auth.pkce_config.code_challenge.clone(),
            auth.pkce_config.code_verifier.clone(),
            auth.pkce_config.client_unique_key.clone(),
        );
        use_pkce_credentials(&mut auth, &credentials);
        assert_eq!(auth.pkce_config.client_id, "id");
        assert_eq!(auth.pkce_config.client_secret, "secret");
        assert_eq!(auth.client_id, "id");
        assert_eq!(auth.client_secret, "secret");
        // The pair the challenge was made from is untouched: only the client changes.
        assert_eq!(auth.pkce_config.code_challenge, challenge);
        assert_eq!(auth.pkce_config.code_verifier, verifier);
        assert_eq!(auth.pkce_config.client_unique_key, key);
    }

    #[test]
    fn half_a_client_is_no_client() {
        assert!(ClientCredentials::parse(None, Some("s".into())).is_none());
        assert!(ClientCredentials::parse(Some("i".into()), None).is_none());
        assert!(ClientCredentials::parse(Some("  ".into()), Some("s".into())).is_none());
        assert!(ClientCredentials::parse(Some("".into()), Some(" ".into())).is_none());
    }

    #[test]
    fn a_device_code_client_keeps_the_library_oauth_credentials() {
        let auth = auth_for(Flow::Device);
        assert!(auth.oauth_login);
        assert!(!auth.pkce_login);
        assert_eq!(auth.client_id, TidalAuth::with_oauth().client_id);
    }

    #[test]
    fn the_sign_in_flow_is_saved_and_old_files_mean_device_code() {
        let path = tmp("flow").join("s.json");
        write_session(&path, "tok", Flow::Pkce).unwrap();
        assert_eq!(read_saved(&path).unwrap().flow, Flow::Pkce);
        // A file written before PKCE existed has no `flow`: it is a device-code session.
        fs::write(&path, b"{\"refresh_token\":\"old\"}").unwrap();
        assert_eq!(read_saved(&path).unwrap().flow, Flow::Device);
    }

    #[test]
    fn a_cancel_outside_a_waiting_sign_in_does_nothing() {
        let mut b = Backend::new(vec![tmp("cancel").join("s.json")]);
        assert!(collect(&mut b, Cmd::CancelLogin).is_empty());
    }
    #[test]
    fn the_quality_asked_for_is_the_best_the_account_can_play() {
        // `HI_RES` is what a HiRes subscription reports: it must ask for HiRes, not Lossless.
        assert!(matches!(quality_for(Some("HI_RES"), None, Quality::Max), AudioQuality::HiRes));
        assert!(matches!(quality_for(Some("HI_RES_LOSSLESS"), None, Quality::Max), AudioQuality::HiRes));
        assert!(matches!(quality_for(Some(" HIRES "), None, Quality::Max), AudioQuality::HiRes));
        // CD-only accounts (or the device-code flow) never ask for HiRes.
        assert!(matches!(quality_for(Some("LOSSLESS"), None, Quality::Max), AudioQuality::Lossless));
        assert!(matches!(quality_for(Some("HIGH"), None, Quality::Max), AudioQuality::High));
        // Ceiling unknown (the subscription call failed): ask for the top, the service falls back.
        assert!(matches!(quality_for(None, None, Quality::Max), AudioQuality::HiRes));
    }

    #[test]
    fn the_preference_is_capped_by_the_account() {
        // Lower than the account allows: ask for exactly that.
        assert!(matches!(quality_for(Some("HI_RES"), None, Quality::Low), AudioQuality::Low));
        assert!(matches!(quality_for(Some("HI_RES"), None, Quality::High), AudioQuality::High));
        assert!(matches!(quality_for(Some("HI_RES"), None, Quality::Lossless), AudioQuality::Lossless));
        // Higher than the account allows: never ask above the ceiling.
        assert!(matches!(quality_for(Some("HIGH"), None, Quality::Lossless), AudioQuality::High));
        assert!(matches!(quality_for(Some("LOW"), None, Quality::Max), AudioQuality::Low));
    }

    #[test]
    fn tide_quality_overrides_the_account_and_the_preference() {
        assert!(matches!(quality_for(Some("HI_RES_LOSSLESS"), Some("lossless"), Quality::Max), AudioQuality::Lossless));
        assert!(matches!(quality_for(Some("LOSSLESS"), Some("high"), Quality::Low), AudioQuality::High));
        assert!(matches!(quality_for(Some("LOSSLESS"), Some(" hires "), Quality::Low), AudioQuality::HiRes));
        assert!(matches!(quality_for(Some("LOSSLESS"), Some("low"), Quality::Max), AudioQuality::Low));
        // Garbage falls back to the preference (capped) instead of silently downgrading.
        assert!(matches!(quality_for(Some("HI_RES"), Some("ultra"), Quality::Lossless), AudioQuality::Lossless));
    }

    #[test]
    fn dash_urls_are_unescaped_and_joined_to_the_base_url() {
        assert_eq!(xml_unescape("x?token=1&amp;exp=2"), "x?token=1&exp=2");
        let urls = vec![
            "https://cdn.example.com/media/".to_string(),
            "init.mp4".to_string(),
            "seg-$Number$.m4s".to_string(),
        ];
        let base = dash_base(&urls, "seg-$Number$.m4s");
        assert_eq!(base.as_deref(), Some("https://cdn.example.com/media/"));
        assert_eq!(
            dash_url(Some("init.mp4"), base.as_deref()).as_deref(),
            Some("https://cdn.example.com/media/init.mp4")
        );
        // Already absolute: only the entities are undone.
        assert_eq!(
            dash_url(Some("https://cdn.example.com/m/init.mp4?token=1&amp;e=2"), None).as_deref(),
            Some("https://cdn.example.com/m/init.mp4?token=1&e=2")
        );
        // A directory is not an init segment, whatever the crate's `get_init_url` fallback says.
        assert_eq!(dash_url(Some("https://cdn.example.com/m/"), None), None);
        assert_eq!(
            url_shape("https://cdn.example.com/m/init.mp4?token=abc&exp=1"),
            "cdn.example.com/m/init.mp4 [?token,exp]"
        );
    }
}
