use serde::Deserialize;
use std::{env, fs, path::PathBuf};

/// Where app data (config, session) lives.
///
/// Order: `TIDE_DATA_DIR`, `$XDG_DATA_HOME/tide-tv`, `~/.local/share/tide-tv`.
/// TODO(TV): pick the app's persistent writable directory on webOS and set
/// `TIDE_DATA_DIR` from the launcher.
pub fn data_dir() -> PathBuf {
    if let Some(d) = env::var_os("TIDE_DATA_DIR") {
        return PathBuf::from(d);
    }
    if let Some(d) = env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(d).join("tide-tv");
    }
    let home = env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".local/share/tide-tv")
}

#[derive(Clone, Debug, Deserialize)]
pub struct Config {
    pub client_id: String,
    pub client_secret: String,
    #[serde(default = "default_auth_base")]
    pub auth_base: String,
    #[serde(default = "default_scope")]
    pub scope: String,
}

fn default_auth_base() -> String {
    "https://auth.tidal.com/v1/oauth2".into()
}

fn default_scope() -> String {
    "r_usr w_usr w_sub".into()
}

impl Config {
    /// Credentials are *never* compiled in. Sources, in order:
    /// env `TIDAL_CLIENT_ID` + `TIDAL_CLIENT_SECRET` (+ optional `TIDAL_AUTH_BASE`),
    /// then `<data_dir>/config.json`.
    pub fn load() -> Result<Config, String> {
        if let (Ok(id), Ok(secret)) = (env::var("TIDAL_CLIENT_ID"), env::var("TIDAL_CLIENT_SECRET")) {
            return Ok(Config {
                client_id: id,
                client_secret: secret,
                auth_base: env::var("TIDAL_AUTH_BASE").unwrap_or_else(|_| default_auth_base()),
                scope: default_scope(),
            });
        }
        // Candidates: $TIDE_CONFIG, <data dir>/config.json, and a location that survives
        // reinstalling the app on a TV.
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(p) = env::var_os("TIDE_CONFIG") {
            candidates.push(PathBuf::from(p));
        }
        candidates.push(data_dir().join("config.json"));
        candidates.push(PathBuf::from("/media/developer/tide-tv/config.json"));
        let Some(path) = candidates.iter().find(|p| p.is_file()) else {
            return Err(format!(
                "Missing client credentials. Set TIDAL_CLIENT_ID and TIDAL_CLIENT_SECRET, or create {}",
                candidates.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(" or ")
            ));
        };
        let text = fs::read_to_string(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("Invalid {}: {e}", path.display()))
    }
}
