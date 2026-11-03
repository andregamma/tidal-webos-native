pub mod home;
pub mod liked;
pub mod login;
pub mod player;
pub mod sidebar;

use crate::model::Track;
use crate::services::PlayerCmd;

/// What a screen asks the app to do after handling a key.
#[derive(Debug)]
pub enum Action {
    None,
    FocusSidebar,
    Back,
    RetryLogin,
    /// Show a short message.
    Toast(String),
    Play(Track),
    /// Playback control to forward to the player.
    Transport(PlayerCmd),
}
