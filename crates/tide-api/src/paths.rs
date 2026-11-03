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

/// File name of the saved sign-in (holds only the refresh token).
pub const SESSION_FILE: &str = "tidlers-session.json";

/// Every place the saved sign-in is read from and written to, best first.
///
/// On the TV one location can be volatile or read-only depending on firmware / how the app is
/// launched, so the session is kept in more than one place: it is loaded from the first copy that
/// parses and written to every location that accepts it. On the desktop there is only
/// [`data_dir`].
pub fn session_files() -> Vec<PathBuf> {
    let mut dirs = vec![data_dir()];
    if cfg!(target_arch = "arm") {
        dirs.push(PathBuf::from("/media/developer/tide-tv"));
        if let Some(exe_dir) = env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
            dirs.push(exe_dir.join("tide-data"));
        }
        if let Some(h) = env::var_os("HOME") {
            dirs.push(PathBuf::from(h).join("tide-data"));
        }
    }
    let mut files: Vec<PathBuf> = Vec::new();
    for d in dirs {
        let f = d.join(SESSION_FILE);
        if !files.contains(&f) {
            files.push(f);
        }
    }
    files
}
