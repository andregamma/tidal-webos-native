use serde::{Deserialize, Serialize};
use std::{fs, io, path::Path};

/// Persisted login. Contains secrets: never log it.
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds when `access_token` expires.
    pub expires_at: u64,
    pub user_id: Option<u64>,
    pub country_code: Option<String>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").field("expires_at", &self.expires_at).field("user_id", &self.user_id).finish_non_exhaustive()
    }
}

impl Session {
    /// Refresh when less than an hour is left.
    pub fn needs_refresh(&self, now: u64) -> bool {
        self.expires_at <= now + 3600
    }
}

pub fn load(path: &Path) -> Option<Session> {
    serde_json::from_str(&fs::read_to_string(path).ok()?).ok()
}

/// Write atomically (temp file + rename) with owner-only permissions on unix.
pub fn save(path: &Path, s: &Session) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, serde_json::to_vec(s).map_err(io::Error::other)?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(tmp, path)
}

pub fn delete(path: &Path) {
    let _ = fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_delete() {
        let dir = std::env::temp_dir().join(format!("tide-store-test-{}", std::process::id()));
        let path = dir.join("session.json");
        let s = Session { access_token: "a".into(), refresh_token: "r".into(), expires_at: 10, user_id: Some(1), country_code: None };
        save(&path, &s).unwrap();
        assert_eq!(load(&path), Some(s.clone()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        delete(&path);
        assert!(load(&path).is_none());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn refresh_window() {
        let s = Session { access_token: "a".into(), refresh_token: "r".into(), expires_at: 10_000, user_id: None, country_code: None };
        assert!(!s.needs_refresh(1_000));
        assert!(s.needs_refresh(7_000));
    }

    #[test]
    fn debug_hides_tokens() {
        let s = Session { access_token: "SECRET".into(), refresh_token: "SECRET2".into(), expires_at: 1, user_id: None, country_code: None };
        let d = format!("{s:?}");
        assert!(!d.contains("SECRET"));
    }
}
