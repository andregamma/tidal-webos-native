/// Domain types shared by UI and backend. Deliberately small: the backend maps
/// whatever the service returns into these.
#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub id: u64,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration: f32,
    /// Cover image URL template; `{px}` is replaced by the wanted size (e.g. `.../{px}x{px}.jpg`).
    pub art: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LyricLine {
    /// Start time in seconds.
    pub t: f32,
    pub text: String,
}

/// A tile on the Home screen. Activating it plays `play` when present;
/// albums/playlists/mixes have no single track and need a collection screen (M2).
#[derive(Clone, Debug)]
pub struct Card {
    pub title: String,
    pub subtitle: String,
    pub art_seed: u64,
    /// Cover image URL template (see `Track::art`); placeholder art until it loads.
    pub art: Option<String>,
    pub play: Option<Track>,
}

#[derive(Clone, Debug)]
pub struct Shelf {
    pub title: String,
    pub cards: Vec<Card>,
}

/// Read-only view of what is playing right now, borrowed from the player state.
/// Widgets that show playback (the mini bar, future overlays) take this instead of
/// reaching into a screen, so they stay independent of every page.
#[derive(Clone, Copy, Debug)]
pub struct NowPlaying<'a> {
    pub track: &'a Track,
    /// Seconds into the track.
    pub pos: f32,
    pub playing: bool,
    /// What the decoder actually got (None until the player reports it).
    pub quality: Option<&'a crate::services::StreamInfo>,
}
