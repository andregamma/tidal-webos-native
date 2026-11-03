//! OAuth 2.0 device authorization grant (RFC 8628) + refresh, as described by
//! community documentation of the service. Written against a 2020-era wiki:
//! VERIFY every field against real responses before relying on it.

use super::{config::Config, http::Http, store::Session};
use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    #[serde(default)]
    pub verification_uri_complete: Option<String>,
    pub expires_in: u32,
    #[serde(default = "default_interval")]
    pub interval: u32,
}

fn default_interval() -> u32 {
    5 // RFC 8628 default when the server does not say
}

impl DeviceCode {
    /// Full link for the QR code (the service omits the scheme).
    pub fn qr_url(&self) -> String {
        let raw = self
            .verification_uri_complete
            .clone()
            .unwrap_or_else(|| format!("{}/{}", self.verification_uri.trim_end_matches('/'), self.user_code));
        if raw.starts_with("http://") || raw.starts_with("https://") {
            raw
        } else {
            format!("https://{raw}")
        }
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: u64,
    #[serde(default)]
    user: Option<UserInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserInfo {
    #[serde(default)]
    user_id: Option<u64>,
    #[serde(default)]
    country_code: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ErrorBody {
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum AuthError {
    /// Could not reach the server (retry later; keep any saved session).
    Network(String),
    /// The device code expired before the user approved.
    Expired,
    /// The user refused.
    Denied,
    /// The server definitively rejected our credentials/refresh token.
    Rejected,
    /// Anything else (HTTP 5xx, malformed reply, ...).
    Other(String),
}

impl std::fmt::Display for AuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuthError::Network(e) => write!(f, "{e}"),
            AuthError::Expired => write!(f, "The code expired"),
            AuthError::Denied => write!(f, "Sign-in was denied"),
            AuthError::Rejected => write!(f, "The saved session is no longer valid"),
            AuthError::Other(e) => write!(f, "{e}"),
        }
    }
}

fn error_code(body: &str) -> Option<String> {
    serde_json::from_str::<ErrorBody>(body).ok()?.error
}

pub fn request_device_code(http: &dyn Http, cfg: &Config) -> Result<DeviceCode, AuthError> {
    let url = format!("{}/device_authorization", cfg.auth_base);
    let r = http
        .post_form(&url, &[("client_id", &cfg.client_id), ("scope", &cfg.scope)], None)
        .map_err(AuthError::Network)?;
    if r.status != 200 {
        let code = error_code(&r.body).unwrap_or_default();
        return Err(AuthError::Other(format!("device authorization failed (HTTP {} {code})", r.status)));
    }
    serde_json::from_str(&r.body).map_err(|e| AuthError::Other(format!("unexpected device authorization reply: {e}")))
}

/// Poll until the user approves, the code expires, or access is denied.
/// `now` and `sleep` are injected so tests run instantly.
pub fn poll_for_session(
    http: &dyn Http,
    cfg: &Config,
    dc: &DeviceCode,
    now: &dyn Fn() -> u64,
    sleep: &dyn Fn(Duration),
) -> Result<Session, AuthError> {
    let url = format!("{}/token", cfg.auth_base);
    let deadline = now() + dc.expires_in as u64;
    let mut interval = dc.interval.max(1) as u64;
    let mut net_failures = 0;
    loop {
        sleep(Duration::from_secs(interval));
        if now() >= deadline {
            return Err(AuthError::Expired);
        }
        let form = [
            ("client_id", cfg.client_id.as_str()),
            ("device_code", dc.device_code.as_str()),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ("scope", cfg.scope.as_str()),
        ];
        let r = match http.post_form(&url, &form, Some((&cfg.client_id, &cfg.client_secret))) {
            Ok(r) => {
                net_failures = 0;
                r
            }
            Err(e) => {
                net_failures += 1;
                if net_failures >= 5 {
                    return Err(AuthError::Network(e));
                }
                continue; // transient: keep polling
            }
        };
        if r.status == 200 {
            return session_from_reply(&r.body, now(), None);
        }
        match error_code(&r.body).as_deref() {
            Some("authorization_pending") => {}
            Some("slow_down") => interval += 5,
            Some("expired_token") => return Err(AuthError::Expired),
            Some("access_denied") => return Err(AuthError::Denied),
            other => {
                return Err(AuthError::Other(format!("token request failed (HTTP {} {})", r.status, other.unwrap_or(""))))
            }
        }
    }
}

pub fn refresh(http: &dyn Http, cfg: &Config, old: &Session, now: u64) -> Result<Session, AuthError> {
    let url = format!("{}/token", cfg.auth_base);
    let form = [
        ("client_id", cfg.client_id.as_str()),
        ("refresh_token", old.refresh_token.as_str()),
        ("grant_type", "refresh_token"),
        ("scope", cfg.scope.as_str()),
    ];
    let r = http.post_form(&url, &form, Some((&cfg.client_id, &cfg.client_secret))).map_err(AuthError::Network)?;
    match r.status {
        200 => session_from_reply(&r.body, now, Some(old)),
        400 | 401 | 403 => Err(AuthError::Rejected),
        s => Err(AuthError::Other(format!("refresh failed (HTTP {s})"))),
    }
}

fn session_from_reply(body: &str, now: u64, prev: Option<&Session>) -> Result<Session, AuthError> {
    let t: TokenResponse =
        serde_json::from_str(body).map_err(|e| AuthError::Other(format!("unexpected token reply: {e}")))?;
    let refresh_token = t
        .refresh_token
        .or_else(|| prev.map(|p| p.refresh_token.clone()))
        .ok_or_else(|| AuthError::Other("token reply had no refresh_token".into()))?;
    Ok(Session {
        access_token: t.access_token,
        refresh_token,
        expires_at: now + t.expires_in,
        user_id: t.user.as_ref().and_then(|u| u.user_id).or(prev.and_then(|p| p.user_id)),
        country_code: t.user.and_then(|u| u.country_code).or_else(|| prev.and_then(|p| p.country_code.clone())),
    })
}
