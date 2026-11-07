/// Remote-control level input. The platform layer maps hardware keys to this
/// (see `tide-sdl/src/main.rs::map_key`); the core never sees raw key codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Ok,
    Back,
    PlayPause,
    Next,
    Prev,
    /// Open Settings on the audio-quality field (keyboard `Q`, webOS remote's green button).
    Quality,
}

impl Key {
    pub fn parse(s: &str) -> Option<Key> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "up" => Key::Up,
            "down" => Key::Down,
            "left" => Key::Left,
            "right" => Key::Right,
            "ok" | "enter" => Key::Ok,
            "back" => Key::Back,
            "play" | "playpause" => Key::PlayPause,
            "next" => Key::Next,
            "prev" => Key::Prev,
            "quality" | "q" => Key::Quality,
            _ => return None,
        })
    }
}
