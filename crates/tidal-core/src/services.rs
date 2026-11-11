//! The only seam between UI and the outside world.
//!
//! The UI thread never blocks: it sends [`Cmd`]s and drains [`Evt`]s once per
//! frame. A backend thread (`tidal-api`, on the `tidlers` crate) does the slow
//! work. The UI never talks to the service itself.

use crate::model::{Account, LyricLine, Shelf, Track};
use std::sync::mpsc::{channel, Receiver, Sender};

/// Where the audio bytes come from. Produced by a backend, consumed by the player.
#[derive(Clone, Debug)]
pub enum StreamSource {
    /// One file/URL (e.g. FLAC or M4A).
    Single(String),
    /// Initialization segment + numbered media segments (DASH).
    Segmented { init: String, template: String, start_number: u32, segment_secs: f32 },
}

#[derive(Clone, Debug)]
pub struct StreamSpec {
    pub track_id: u64,
    /// e.g. `audio/flac`, `audio/mp4`; used as a decoder hint.
    pub mime: String,
    pub duration: f32,
    pub source: StreamSource,
    /// The service's own quality label (e.g. `LOSSLESS`, `HI_RES_LOSSLESS`), shown as-is.
    pub tier: Option<String>,
}

/// What is actually being played, as measured by the player (not the service's label).
#[derive(Clone, Debug, Default)]
pub struct StreamInfo {
    /// Decoder short name, upper case (`FLAC`, `AAC`...).
    pub codec: String,
    /// Sample rate of the decoded stream, in Hz.
    pub src_rate: u32,
    /// Bits per sample when the decoder reports it (lossless); `None` for lossy codecs.
    pub src_bits: Option<u32>,
    pub src_channels: u8,
    /// Format the audio device was opened with (what SDL got from the platform).
    pub dev_rate: u32,
    pub dev_channels: u8,
    /// SDL audio driver name (e.g. `pulseaudio`, `alsa`).
    pub driver: String,
    pub tier: Option<String>,
}

/// UI/backend -> player thread. Transport commands go straight from the UI to
/// the player (never through the backend, which may be busy with network I/O).
#[derive(Clone, Debug)]
pub enum PlayerCmd {
    Load(StreamSpec),
    Pause,
    Resume,
    Seek(f32),
    Stop,
}

#[derive(Debug)]
pub enum Cmd {
    /// Resolve the stream for a track and hand it to the player.
    Play { track_id: u64, duration: f32 },
    /// App start: restore (and refresh) a saved session if there is one.
    Boot,
    /// Sign in. PKCE is tried first, because it is the flow that yields HiRes.
    StartLogin,
    /// Skip PKCE and use the device-code (QR + short code) fallback: at most CD quality.
    StartLoginFallback,
    /// Abandon a sign-in that is waiting for the PKCE callback: the LAN server is closed.
    CancelLogin,
    /// Forget the saved session.
    Logout,
    LoadHome,
    LoadLiked,
    LoadLyrics(u64),
    /// Name, e-mail and plan of the signed-in account (Settings page).
    LoadAccount,
    /// Change the language at runtime and persist it. Carries a code from `i18n::LANGUAGES`
    /// (`pt-BR`); the backend applies it to the service client and saves it.
    SetLocale(String),
    /// Change the streaming quality preference and persist it. The backend caps it by what the
    /// account can stream; `Max` (the default) means "the best available".
    SetQuality(crate::quality::Quality),
}

#[derive(Debug)]
pub enum Evt {
    /// No usable saved session: the UI should start the login flow.
    LoginRequired,
    /// PKCE (primary flow). `qr_url` is the service's sign-in page; `paste_url`, when the relay sends
    /// one, is the page where the phone pastes the address it gets after signing in (the code goes to
    /// the client's own registered page, not to the relay). The phone can be on any network: nothing
    /// has to reach this TV (see `docs/relay.md`).
    PkceLogin { qr_url: String, paste_url: Option<String>, expires_in: u32 },
    /// PKCE could not be completed but the service is reachable: offer the device-code fallback
    /// (CD quality) with this reason.
    FallbackOffered(String),
    /// `Cmd::CancelLogin` took effect: nothing is waiting any more.
    LoginCancelled,
    /// Device-code fallback. `qr_url` is the full link to encode in the QR.
    LoginCode { user_code: String, verify_url: String, qr_url: String, expires_in: u32 },
    /// Login or session restore succeeded. `max_quality` is the account's highest sound quality as
    /// reported by the service (`HI_RES_LOSSLESS`, `LOSSLESS`...), when it could be read.
    LoggedIn { max_quality: Option<String> },
    /// Login cannot continue (bad config, denied, network). Shown on the login screen.
    LoginFailed(String),
    Home(Vec<Shelf>),
    Liked(Vec<Track>),
    Account(Account),
    /// `synced = false` means plain text without usable timestamps.
    Lyrics { track_id: u64, lines: Vec<LyricLine>, synced: bool },
    /// Format details of the stream that just started playing.
    StreamInfo(StreamInfo),
    /// Playback position in seconds (about 4 per second while playing).
    Position(f32),
    /// A seek landed: playback resumes here. A DASH stream can only resume on a segment boundary,
    /// so this can be a little earlier than the requested time; the bar adopts it as-is.
    Seeked(f32),
    TrackEnded,
    PlaybackError(String),
    Error(String),
}

/// What the UI holds.
pub struct AppSide {
    pub tx: Sender<Cmd>,
    pub rx: Receiver<Evt>,
    /// Transport commands (pause/seek/stop) straight to the player.
    pub player: Sender<PlayerCmd>,
}

/// What the backend thread holds.
pub struct BackendSide {
    pub rx: Receiver<Cmd>,
    pub tx: Sender<Evt>,
    /// Used to hand a resolved stream to the player (`PlayerCmd::Load`).
    pub player: Sender<PlayerCmd>,
}

/// What the player thread holds.
pub struct PlayerSide {
    pub rx: Receiver<PlayerCmd>,
    pub tx: Sender<Evt>,
}

pub struct Channels {
    pub app: AppSide,
    pub backend: BackendSide,
    pub player: PlayerSide,
}

pub fn channels() -> Channels {
    let (ctx, crx) = channel();
    let (etx, erx) = channel();
    let (ptx, prx) = channel();
    Channels {
        app: AppSide { tx: ctx, rx: erx, player: ptx.clone() },
        backend: BackendSide { rx: crx, tx: etx.clone(), player: ptx },
        player: PlayerSide { rx: prx, tx: etx },
    }
}

/// UI + backend only (the player side is dropped). Handy for tests.
pub fn channel_pair() -> (AppSide, BackendSide) {
    let c = channels();
    (c.app, c.backend)
}
