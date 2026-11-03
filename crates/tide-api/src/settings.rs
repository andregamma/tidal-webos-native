//! The app's persistent settings ("storage").
//!
//! Today that is only the language. It lives in `tide-settings.json`, next to the saved session and
//! with the same redundancy on the TV (see [`paths::storage_files`]): read from the first copy that
//! parses, written atomically (temp file, fsync, rename) to every location that accepts it.
//!
//! Deliberately separate from the session: signing out deletes the session but keeps the language.
//! Both backends call [`save_locale`] on `Cmd::SetLocale`, and `tide-sdl` calls [`Settings::load`] at
//! start so `tide-core` (which does no I/O) is handed the value instead of reading it.

use crate::{diag, paths};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
};
use tide_core::i18n;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Always a code from `i18n::LANGUAGES` (`pt-BR`).
    pub locale: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self { locale: i18n::DEFAULT_LOCALE.to_string() }
    }
}

impl Settings {
    /// From the first readable copy in the usual locations; defaults when there is none.
    pub fn load() -> Self {
        Self::load_from(&paths::settings_files())
    }

    pub fn load_from(files: &[PathBuf]) -> Self {
        files.iter().find_map(|p| read(p)).unwrap_or_default()
    }

    /// Write to every usual location; returns how many copies were written.
    pub fn save(&self) -> usize {
        self.save_to(&paths::settings_files())
    }

    pub fn save_to(&self, files: &[PathBuf]) -> usize {
        let Ok(data) = serde_json::to_vec_pretty(self) else { return 0 };
        let mut written = 0;
        for p in files {
            match write_atomic(p, &data) {
                Ok(()) => written += 1,
                Err(e) => diag::log(&format!("could not save the settings to {}: {e}", p.display())),
            }
        }
        written
    }
}

/// Persist a new language, keeping whatever else is stored. Returns how many copies were written.
pub fn save_locale(code: &str) -> usize {
    let mut s = Settings::load();
    s.locale = i18n::resolve(code).to_string();
    s.save()
}

fn read(path: &Path) -> Option<Settings> {
    let text = fs::read_to_string(path).ok()?;
    let mut s: Settings = serde_json::from_str(&text).ok()?;
    // Whatever the file says, only a known language gets through.
    s.locale = i18n::resolve(&s.locale).to_string();
    Some(s)
}

fn write_atomic(path: &Path, data: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(data)?;
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
