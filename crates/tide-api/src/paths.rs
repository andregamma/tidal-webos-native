use std::{env, path::PathBuf};

/// Where app data (the saved session, the log) lives.
///
/// Order: `TIDE_DATA_DIR`, `%LOCALAPPDATA%\tide-tv` (Windows), `$XDG_DATA_HOME/tide-tv`,
/// `~/.local/share/tide-tv`, and finally next to the executable (never the working directory,
/// which changes with how the app is launched). On the TV `tide-sdl` sets `TIDE_DATA_DIR`.
pub fn data_dir() -> PathBuf {
    if let Some(d) = env::var_os("TIDE_DATA_DIR") {
        return PathBuf::from(d);
    }
    #[cfg(windows)]
    if let Some(d) = env::var_os("LOCALAPPDATA").or_else(|| env::var_os("APPDATA")) {
        return PathBuf::from(d).join("tide-tv");
    }
    if let Some(d) = env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(d).join("tide-tv");
    }
    if let Some(h) = env::var_os("HOME").or_else(|| env::var_os("USERPROFILE")) {
        return PathBuf::from(h).join(".local/share/tide-tv");
    }
    env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("tide-data")))
        .unwrap_or_else(|| env::temp_dir().join("tide-tv"))
}
