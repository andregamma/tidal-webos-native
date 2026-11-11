pub mod home;
pub mod liked;
pub mod login;
pub mod player;
pub mod settings;
pub mod sidebar;

use crate::model::Track;
use crate::quality::Quality;
use crate::services::PlayerCmd;

/// What a screen asks the app to do after handling a key.
#[derive(Debug)]
pub enum Action {
    None,
    FocusSidebar,
    /// Down ran off the end of the page: focus the mini bar (ignored when it is not shown).
    FocusBar,
    Back,
    RetryLogin,
    /// Sign in with the device-code fallback (CD quality) instead of PKCE.
    UseFallback,
    /// Stop waiting for the PKCE callback (closes the LAN server).
    CancelLogin,
    /// Signed in: go to the library.
    Continue,
    /// Show a short message.
    Toast(String),
    Play(Track),
    /// Playback control to forward to the player.
    Transport(PlayerCmd),
    /// The user picked a language (code from `i18n::LANGUAGES`, e.g. `pt-BR`).
    SetLocale(String),
    /// The user picked a streaming quality (Settings page).
    SetQuality(Quality),
    /// Open Settings on the audio-quality field (the player's shortcut).
    OpenQuality,
}
