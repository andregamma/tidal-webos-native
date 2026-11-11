//! What the relay decides, as pure functions: no Workers type in here, so the rules that matter
//! (input validation, the authorize URL, when a code may be handed out) are unit-tested on the
//! developer machine with `cargo test`.
//!
//! A sign-in is one *handoff*. The TV sends the public half of its PKCE pair; the relay answers with
//! an id that doubles as the `state` of the service's authorize URL, so the service's redirect points
//! back at the same handoff; the TV polls that id until the code (or the refusal) shows up. The
//! verifier never leaves the TV and no token is ever made here.

use serde::{Deserialize, Serialize};

/// How long the user has to finish signing in on the phone, counted from the handoff request.
pub const HANDOFF_TTL_MS: i64 = 300_000;
/// How long an authorization code waits for the TV once the service has redirected.
pub const CODE_TTL_MS: i64 = 60_000;
/// The handoff id is 32 random bytes in base64url without padding: 43 characters, as long as the
/// `state` it replaces.
pub const ID_BYTES: usize = 32;
pub const ID_LEN: usize = 43;
/// RFC 7636 allows 43..=128 characters; `S256` (a SHA-256) always produces 43.
pub const CHALLENGE_MIN_LEN: usize = 43;
pub const CHALLENGE_MAX_LEN: usize = 128;
/// Upper bound for the TV's own `state` and for its `client_unique_key`.
pub const MAX_STATE_LEN: usize = 512;
pub const MAX_UNIQUE_KEY_LEN: usize = 128;
/// Longest authorization code we are willing to store, and longest error text we keep. The service
/// issues JWT-shaped codes: 453 characters for a plain `r_usr w_usr w_sub` grant.
pub const MAX_CODE_LEN: usize = 2048;
/// Longest address accepted on the paste page: a code this long plus the URL around it.
pub const MAX_PASTE_LEN: usize = 4096;
/// Bounds for the client a handoff signs in with, when the TV names it.
pub const MAX_CLIENT_ID_LEN: usize = 128;
pub const MAX_REDIRECT_LEN: usize = 512;
pub const MAX_TEXT_LEN: usize = 256;
/// Longest body accepted by `POST /v1/handoff`.
pub const MAX_BODY_LEN: usize = 4 * 1024;
/// How often the TV should poll while the user is on the phone.
pub const POLL_INTERVAL_MS: u64 = 1_500;

/// What the TV sends: the public half of its PKCE pair, plus what the service's own client sends
/// along with it.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HandoffRequest {
    pub code_challenge: String,
    /// The TV's own `state`, echoed back on delivery so the TV can check it.
    #[serde(default)]
    pub state: Option<String>,
    /// The library's per-install key: the token exchange sends it, so the authorize URL must too.
    #[serde(default)]
    pub client_unique_key: Option<String>,
    /// The client the code will belong to. When the TV signs in with the client the crate embeds, it
    /// names it here and the relay needs no credentials of its own to build the sign-in page.
    #[serde(default)]
    pub client_id: Option<String>,
    /// Where the service will deliver the code. Only the relay's own `redirect_uri` comes back here;
    /// any other one (the client's registered page) has to be brought back by hand (`/paste/{id}`).
    #[serde(default)]
    pub redirect_uri: Option<String>,
}

impl HandoffRequest {
    /// Parse and validate the JSON body of `POST /v1/handoff`.
    pub fn parse(body: &str) -> Result<HandoffRequest, Invalid> {
        if body.len() > MAX_BODY_LEN {
            return Err(Invalid::TooLarge);
        }
        let mut request: HandoffRequest = serde_json::from_str(body).map_err(|_| Invalid::Malformed)?;
        // An empty `state` is the same as none: the TV has nothing to check against.
        request.state = request.state.filter(|s| !s.is_empty());
        request.client_unique_key = request.client_unique_key.filter(|s| !s.is_empty());
        request.client_id = request.client_id.map(|v| v.trim().to_string()).filter(|s| !s.is_empty());
        request.redirect_uri = request.redirect_uri.map(|v| v.trim().to_string()).filter(|s| !s.is_empty());
        if let Some(id) = &request.client_id {
            if !valid_client_id(id) {
                return Err(Invalid::BadClientId);
            }
        }
        if let Some(uri) = &request.redirect_uri {
            if !valid_redirect(uri) {
                return Err(Invalid::BadRedirect);
            }
        }
        if let Some(state) = &request.state {
            if !valid_state(state) {
                return Err(Invalid::BadState);
            }
        }
        if let Some(key) = &request.client_unique_key {
            if !valid_unique_key(key) {
                return Err(Invalid::BadUniqueKey);
            }
        }
        if request.code_challenge.len() < CHALLENGE_MIN_LEN
            || request.code_challenge.len() > CHALLENGE_MAX_LEN
        {
            return Err(Invalid::ChallengeLength);
        }
        if !request.code_challenge.bytes().all(is_url_safe) {
            return Err(Invalid::ChallengeCharset);
        }
        Ok(request)
    }
}

/// Why a handoff request was refused. Every variant is the client's fault: a 400.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalid {
    TooLarge,
    Malformed,
    ChallengeLength,
    ChallengeCharset,
    BadState,
    BadUniqueKey,
    BadClientId,
    BadRedirect,
}

impl Invalid {
    pub fn message(self) -> &'static str {
        match self {
            Invalid::TooLarge => "The request body is too large.",
            Invalid::Malformed => "Expected a JSON body with a `code_challenge` string.",
            Invalid::ChallengeLength => "`code_challenge` must be 43 to 128 characters long.",
            Invalid::ChallengeCharset => "`code_challenge` must be base64url (A-Z, a-z, 0-9, '-', '_').",
            Invalid::BadState => "`state` must be at most 512 printable characters.",
            Invalid::BadUniqueKey => "`client_unique_key` must be at most 128 printable characters.",
            Invalid::BadClientId => "`client_id` must be at most 128 printable characters.",
            Invalid::BadRedirect => "`redirect_uri` must be an https URL of at most 512 characters.",
        }
    }
}

/// One sign-in attempt. Lives in its own Durable Object, addressed by the handoff id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Handoff {
    /// The challenge the TV generated; the code only makes sense to whoever holds its verifier.
    pub code_challenge: String,
    /// The TV's own `state`, echoed back on delivery so the TV can check it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tv_state: Option<String>,
    /// The sign-in page, kept so `GET /auth/{id}` can redirect to it without rebuilding anything.
    pub authorize_url: String,
    pub expires_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<Delivery>,
}

/// What the service's redirect left behind for the TV.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Delivery {
    /// The authorization code, kept just long enough to be picked up.
    Code { code: String, expires_at_ms: i64 },
    /// The service refused the sign-in (the user said no, or the request was bad).
    Denied { error: String, description: Option<String> },
}

impl Handoff {
    pub fn new(request: &HandoffRequest, authorize_url: String, now_ms: i64) -> Handoff {
        Handoff {
            code_challenge: request.code_challenge.clone(),
            tv_state: request.state.clone(),
            authorize_url,
            expires_at_ms: now_ms.saturating_add(HANDOFF_TTL_MS),
            delivery: None,
        }
    }

    /// The moment this record stops being useful, whatever it holds.
    pub fn drop_at_ms(&self) -> i64 {
        match &self.delivery {
            Some(Delivery::Code { expires_at_ms, .. }) => self.expires_at_ms.min(*expires_at_ms),
            _ => self.expires_at_ms,
        }
    }

    /// Take what the service's redirect carried. First answer wins; nothing overwrites it.
    pub fn accept_callback(&mut self, now_ms: i64, callback: Callback) -> CallbackOutcome {
        if now_ms >= self.drop_at_ms() {
            return CallbackOutcome::Expired;
        }
        if self.delivery.is_some() {
            return CallbackOutcome::AlreadyUsed;
        }
        if let Some(error) = callback.error {
            self.delivery = Some(Delivery::Denied { error, description: callback.description });
            return CallbackOutcome::Stored;
        }
        match callback.code {
            Some(code) => {
                // The shorter of the two windows: a code nobody picked up is dropped with the handoff.
                let expires_at_ms = now_ms.saturating_add(CODE_TTL_MS).min(self.expires_at_ms);
                self.delivery = Some(Delivery::Code { code, expires_at_ms });
                CallbackOutcome::Stored
            }
            None => CallbackOutcome::Nothing,
        }
    }

    /// What the TV sees when it polls. Never the code until the service has answered.
    pub fn poll(&self, now_ms: i64) -> Poll {
        if now_ms >= self.drop_at_ms() {
            return Poll::Expired;
        }
        match &self.delivery {
            None => Poll::Pending { expires_in_ms: (self.expires_at_ms - now_ms).max(0) },
            Some(Delivery::Code { code, .. }) => {
                Poll::Code { code: code.clone(), state: self.tv_state.clone() }
            }
            Some(Delivery::Denied { error, description }) => {
                Poll::Denied { error: error.clone(), description: description.clone() }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallbackOutcome {
    Stored,
    AlreadyUsed,
    Expired,
    /// The service sent neither a code nor an error.
    Nothing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Poll {
    Pending { expires_in_ms: i64 },
    Code { code: String, state: Option<String> },
    Denied { error: String, description: Option<String> },
    Expired,
}

/// What the service put in the redirect, already decoded and cleaned.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Callback {
    pub code: Option<String>,
    pub error: Option<String>,
    pub description: Option<String>,
}

impl Callback {
    /// Read `code` / `error` / `error_description` from the redirect's query string.
    pub fn from_query(query: &str) -> Callback {
        let code = param(query, "code")
            .filter(|c| !c.is_empty() && c.len() <= MAX_CODE_LEN && !c.chars().any(char::is_control));
        let error = param(query, "error").map(|e| clean(&e));
        let description = param(query, "error_description").map(|d| clean(&d));
        Callback { code, error, description }
    }

    pub fn is_empty(&self) -> bool {
        self.code.is_none() && self.error.is_none()
    }
}

/// The address the phone shows after signing in, carried back by hand.
///
/// When the app signs in with a client whose `redirect_uri` is the service's own page (the client the
/// TV's library embeds), the code never reaches this relay. The code is useless without the
/// `code_verifier`, which only the TV has, so a person carrying it is safe: the page checks it belongs
/// to this handoff and stores it exactly like a callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paste {
    /// The `state` the service echoed back: the handoff id, as in the authorize URL.
    pub state: Option<String>,
    pub callback: Callback,
}

impl Paste {
    /// Read a full URL (`https://…?code=…&state=…`) or just its query string.
    pub fn parse(text: &str) -> Result<Paste, PasteInvalid> {
        let text = text.trim().trim_matches(|c| matches!(c, '"' | '\'' | '<' | '>'));
        if text.len() > MAX_PASTE_LEN {
            return Err(PasteInvalid::TooLarge);
        }
        // A fragment never carries the code, and some browsers append one.
        let text = text.split('#').next().unwrap_or("");
        let query = match text.split_once('?') {
            Some((_, query)) => query,
            // Someone may paste just `code=…&state=…`.
            None if text.contains('=') => text,
            None => return Err(PasteInvalid::NoQuery),
        };
        let callback = Callback::from_query(query);
        if callback.is_empty() {
            return Err(PasteInvalid::NoCode);
        }
        Ok(Paste { state: param(query, "state"), callback })
    }
}

/// Why a pasted address was refused. Every variant is a 400 with its own sentence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteInvalid {
    TooLarge,
    NoQuery,
    NoCode,
}

impl PasteInvalid {
    pub fn message(self) -> &'static str {
        match self {
            PasteInvalid::TooLarge => "That address is too long. Copy only the page's own URL.",
            PasteInvalid::NoQuery => {
                "That does not look like the address of the page after signing in. Paste the whole \
                 address, starting with https://"
            }
            PasteInvalid::NoCode => {
                "That address carries no authorization code. Sign in first, then copy the address of \
                 the page that opens."
            }
        }
    }
}

/// The sign-in page the phone opens. It must end at the `redirect_uri` registered for this relay:
/// the service compares it exactly, and the TV has to send the same one when it exchanges the code.
///
/// The parameters mirror the ones the TV's own client sends (`appMode`, `restrict_signup`,
/// `client_unique_key`), because the token exchange that follows belongs to that client.
pub struct Authorize<'a> {
    pub endpoint: &'a str,
    pub client_id: &'a str,
    pub redirect_uri: &'a str,
    pub code_challenge: &'a str,
    pub state: &'a str,
    pub client_unique_key: Option<&'a str>,
    /// Empty means "do not send `scope`", which is what the TV's client does.
    pub scope: &'a str,
}

impl Authorize<'_> {
    pub fn url(&self) -> String {
        let mut params: Vec<(&str, &str)> = vec![
            ("response_type", "code"),
            ("client_id", self.client_id),
            ("redirect_uri", self.redirect_uri),
            ("code_challenge", self.code_challenge),
            ("code_challenge_method", "S256"),
            ("state", self.state),
        ];
        if let Some(key) = self.client_unique_key {
            params.push(("client_unique_key", key));
        }
        if !self.scope.is_empty() {
            params.push(("scope", self.scope));
        }
        params.push(("appMode", "android"));
        params.push(("restrict_signup", "true"));

        let mut url = String::with_capacity(256);
        url.push_str(self.endpoint);
        url.push(if self.endpoint.contains('?') { '&' } else { '?' });
        for (i, (key, value)) in params.into_iter().enumerate() {
            if i > 0 {
                url.push('&');
            }
            url.push_str(key);
            url.push('=');
            url.push_str(&encode_component(value));
        }
        url
    }
}

/// A handoff id, as it appears in a path or a `state`: 43 characters of base64url and nothing else.
pub fn valid_id(value: &str) -> bool {
    value.len() == ID_LEN && value.bytes().all(is_url_safe)
}

/// Value of `name` in an `a=1&b=2` query string, percent-decoded.
pub fn param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| decode_component(value))
    })
}

/// Percent-decode a query component; `+` means a space, as in form encoding.
pub fn decode_component(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                (Some(high), Some(low)) => {
                    out.push(high << 4 | low);
                    i += 3;
                }
                // A stray `%` is kept: it is data, not an escape.
                _ => {
                    out.push(b'%');
                    i += 1;
                }
            },
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// base64url (RFC 4648 section 5) without padding.
pub fn base64url(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(TABLE[n as usize & 63] as char);
        }
    }
    out
}

/// RFC 3986 percent-encoding: everything but the unreserved characters, space included as `%20`.
fn encode_component(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if is_url_safe(byte) {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 15) as usize] as char);
        }
    }
    out
}

/// The unreserved characters of RFC 3986: the only ones never encoded.
fn is_url_safe(byte: u8) -> bool {
    matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~')
}

fn valid_state(state: &str) -> bool {
    state.len() <= MAX_STATE_LEN && !state.chars().any(char::is_control)
}

fn valid_unique_key(key: &str) -> bool {
    key.len() <= MAX_UNIQUE_KEY_LEN && !key.chars().any(char::is_control)
}

/// A client id ends up in the authorize URL, so it has to be a plain token.
fn valid_client_id(id: &str) -> bool {
    id.len() <= MAX_CLIENT_ID_LEN && !id.chars().any(char::is_control) && id.bytes().all(is_url_safe)
}

/// A redirect URI has to be an absolute https URL: anything else would put junk in the sign-in page
/// (and no service accepts it anyway).
fn valid_redirect(uri: &str) -> bool {
    uri.len() <= MAX_REDIRECT_LEN
        && uri.starts_with("https://")
        && !uri.chars().any(char::is_control)
        && !uri.contains(['#', ' '])
}

/// Cut a service-supplied string to a size we are willing to store, without control characters.
fn clean(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(MAX_TEXT_LEN).collect()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
    const ID: &str = "0123456789012345678901234567890123456789012";
    const URL: &str = "https://login.tidal.com/authorize?client_id=cid&state=0123";

    fn request(state: Option<&str>) -> Handoff {
        let request = HandoffRequest {
            code_challenge: CHALLENGE.into(),
            state: state.map(Into::into),
            client_unique_key: None,
            client_id: None,
            redirect_uri: None,
        };
        Handoff::new(&request, URL.into(), 1_000)
    }

    #[test]
    fn a_valid_request_is_accepted_and_an_empty_state_becomes_none() {
        let body = format!(r#"{{"code_challenge":"{CHALLENGE}","state":""}}"#);
        let parsed = HandoffRequest::parse(&body).expect("valid");
        assert_eq!(parsed.code_challenge, CHALLENGE);
        assert_eq!(parsed.state, None);
        let parsed = HandoffRequest::parse(&format!(r#"{{"code_challenge":"{CHALLENGE}"}}"#)).expect("valid");
        assert_eq!(parsed.state, None);
        assert_eq!(parsed.client_unique_key, None);
    }

    #[test]
    fn bad_requests_are_named_precisely() {
        assert_eq!(HandoffRequest::parse("").unwrap_err(), Invalid::Malformed);
        assert_eq!(HandoffRequest::parse("{}").unwrap_err(), Invalid::Malformed);
        assert_eq!(
            HandoffRequest::parse(r#"{"code_challenge":"short"}"#).unwrap_err(),
            Invalid::ChallengeLength
        );
        let long = format!(r#"{{"code_challenge":"{}"}}"#, "a".repeat(129));
        assert_eq!(HandoffRequest::parse(&long).unwrap_err(), Invalid::ChallengeLength);
        let plus = format!(r#"{{"code_challenge":"{}"}}"#, "a".repeat(42) + "+");
        assert_eq!(HandoffRequest::parse(&plus).unwrap_err(), Invalid::ChallengeCharset);
        let long_state = format!(
            r#"{{"code_challenge":"{CHALLENGE}","state":"{}"}}"#,
            "s".repeat(MAX_STATE_LEN + 1)
        );
        assert_eq!(HandoffRequest::parse(&long_state).unwrap_err(), Invalid::BadState);
        let long_key = format!(
            r#"{{"code_challenge":"{CHALLENGE}","client_unique_key":"{}"}}"#,
            "k".repeat(MAX_UNIQUE_KEY_LEN + 1)
        );
        assert_eq!(HandoffRequest::parse(&long_key).unwrap_err(), Invalid::BadUniqueKey);
        assert_eq!(HandoffRequest::parse(&"x".repeat(MAX_BODY_LEN + 1)).unwrap_err(), Invalid::TooLarge);
    }

    #[test]
    fn the_created_handoff_lasts_five_minutes_and_holds_nothing_yet() {
        let handoff = request(Some("tv-state"));
        assert_eq!(handoff.expires_at_ms, 1_000 + HANDOFF_TTL_MS);
        assert_eq!(handoff.delivery, None);
        assert_eq!(handoff.authorize_url, URL);
        assert_eq!(handoff.poll(1_000), Poll::Pending { expires_in_ms: HANDOFF_TTL_MS });
    }

    #[test]
    fn the_code_window_is_shorter_than_the_handoff_and_never_extends_it() {
        let mut handoff = request(None);
        let code = |code: &str| Callback { code: Some(code.into()), ..Callback::default() };

        let mut fresh = handoff.clone();
        assert_eq!(fresh.accept_callback(2_000, code("abc")), CallbackOutcome::Stored);
        assert_eq!(fresh.drop_at_ms(), 2_000 + CODE_TTL_MS);
        assert_eq!(fresh.poll(2_500), Poll::Code { code: "abc".into(), state: None });

        // A handoff about to expire does not keep its code for a full minute after that.
        handoff.expires_at_ms = 3_000;
        assert_eq!(handoff.accept_callback(2_000, code("abc")), CallbackOutcome::Stored);
        assert_eq!(handoff.drop_at_ms(), 3_000);
    }

    #[test]
    fn a_code_is_handed_out_once_and_only_until_its_window_closes() {
        let mut handoff = request(Some("tv-state"));
        let code = Callback { code: Some("abc".into()), ..Default::default() };
        assert_eq!(handoff.accept_callback(1_500, code), CallbackOutcome::Stored);
        assert_eq!(handoff.poll(1_600), Poll::Code { code: "abc".into(), state: Some("tv-state".into()) });

        let mut used = handoff.clone();
        let later = Callback { code: Some("later".into()), ..Default::default() };
        assert_eq!(used.accept_callback(1_700, later), CallbackOutcome::AlreadyUsed);
        assert_eq!(used.poll(1_700), Poll::Code { code: "abc".into(), state: Some("tv-state".into()) });

        // Past the code window, and past the handoff itself, nothing is left to deliver.
        assert_eq!(handoff.poll(1_000 + HANDOFF_TTL_MS), Poll::Expired);
        let mut stale = request(None);
        stale.expires_at_ms = 2_000;
        let x = Callback { code: Some("x".into()), ..Default::default() };
        assert_eq!(stale.accept_callback(2_000, x), CallbackOutcome::Expired);
    }

    #[test]
    fn a_code_window_that_closes_is_an_expiry_even_before_the_handoff_does() {
        let mut handoff = request(None);
        handoff.expires_at_ms = 10_000_000;
        handoff.accept_callback(2_000, Callback { code: Some("abc".into()), ..Default::default() });
        assert!(matches!(handoff.poll(2_000 + CODE_TTL_MS), Poll::Expired));
    }

    #[test]
    fn a_refusal_reaches_the_tv_as_the_service_wrote_it() {
        let mut handoff = request(None);
        let refusal = Callback {
            code: None,
            error: Some("access_denied".into()),
            description: Some("The user denied the request.".into()),
        };
        assert_eq!(handoff.accept_callback(1_100, refusal), CallbackOutcome::Stored);
        assert_eq!(
            handoff.poll(1_200),
            Poll::Denied {
                error: "access_denied".into(),
                description: Some("The user denied the request.".into()),
            }
        );
    }

    #[test]
    fn a_redirect_with_neither_a_code_nor_an_error_stores_nothing() {
        let mut handoff = request(None);
        assert_eq!(handoff.accept_callback(1_100, Callback::default()), CallbackOutcome::Nothing);
        assert_eq!(handoff.delivery, None);
    }

    #[test]
    fn the_authorize_url_carries_the_challenge_and_comes_back_to_the_relay() {
        let url = Authorize {
            endpoint: "https://login.tidal.com/authorize",
            client_id: "cid",
            redirect_uri: "https://tidal-relay.andregama.dev/callback",
            code_challenge: CHALLENGE,
            state: ID,
            client_unique_key: Some("unique-key"),
            scope: "",
        }
        .url();
        assert!(
            url.starts_with("https://login.tidal.com/authorize?response_type=code&client_id=cid&"),
            "{url}"
        );
        assert!(url.contains("&redirect_uri=https%3A%2F%2Ftidal-relay.andregama.dev%2Fcallback&"), "{url}");
        assert!(url.contains(&format!("&code_challenge={CHALLENGE}&code_challenge_method=S256&")), "{url}");
        assert!(url.contains(&format!("&state={ID}&")), "{url}");
        assert!(url.contains("&client_unique_key=unique-key&"), "{url}");
        assert!(url.ends_with("&appMode=android&restrict_signup=true"), "{url}");
        // The TV's client sends no `scope` on the authorize request; neither do we by default.
        assert!(!url.contains("scope="), "{url}");
    }

    #[test]
    fn a_configured_scope_is_sent_and_an_endpoint_with_a_query_gets_another_parameter() {
        let url = Authorize {
            endpoint: "https://example.test/auth?lang=en",
            client_id: "cid",
            redirect_uri: "https://relay.test/callback",
            code_challenge: CHALLENGE,
            state: ID,
            client_unique_key: None,
            scope: "r_usr w_usr",
        }
        .url();
        assert!(url.starts_with("https://example.test/auth?lang=en&response_type=code&"), "{url}");
        assert!(url.contains("&scope=r_usr%20w_usr&"), "{url}");
        assert!(!url.contains("client_unique_key"), "{url}");
    }

    #[test]
    fn the_redirect_is_read_from_the_query_string() {
        let callback = Callback::from_query("state=abc&code=some%2Bcode&error_description=no");
        assert_eq!(callback.code.as_deref(), Some("some+code"));
        assert_eq!(callback.error, None);
        assert_eq!(callback.description.as_deref(), Some("no"));

        let empty = Callback::from_query("state=abc&code=");
        assert!(empty.is_empty());

        // A refusal is what the service sends when the user says no.
        let denied = Callback::from_query("error=access_denied&error_description=The+user+denied");
        assert_eq!(denied.error.as_deref(), Some("access_denied"));
        assert_eq!(denied.description.as_deref(), Some("The user denied"));
    }

    #[test]
    fn a_pasted_address_is_read_from_the_query_it_carries() {
        let pasted = Paste::parse(" https://tidal.com/android/login/auth?code=abc.def&state=0123\n")
            .expect("a full address");
        assert_eq!(pasted.state.as_deref(), Some("0123"));
        assert_eq!(pasted.callback.code.as_deref(), Some("abc.def"));

        // Just the query, and the fragment some browsers add.
        let pasted = Paste::parse("code=abc&state=0123#frag").expect("a bare query");
        assert_eq!(pasted.callback.code.as_deref(), Some("abc"));
        assert_eq!(pasted.state.as_deref(), Some("0123"));
    }

    #[test]
    fn a_pasted_address_without_a_code_is_refused_with_a_sentence() {
        assert_eq!(Paste::parse("").unwrap_err(), PasteInvalid::NoQuery);
        assert_eq!(Paste::parse("https://tidal.com/").unwrap_err(), PasteInvalid::NoQuery);
        assert_eq!(Paste::parse("https://tidal.com/x?state=abc").unwrap_err(), PasteInvalid::NoCode);
        assert_eq!(
            Paste::parse(&"x".repeat(MAX_PASTE_LEN + 1)).unwrap_err(),
            PasteInvalid::TooLarge
        );
        for reason in [PasteInvalid::NoQuery, PasteInvalid::NoCode, PasteInvalid::TooLarge] {
            assert!(!reason.message().is_empty());
        }
    }

    #[test]
    fn a_pasted_refusal_is_kept_as_the_service_wrote_it() {
        let pasted = Paste::parse("https://tidal.com/x?error=access_denied&error_description=no+way")
            .expect("a refusal");
        assert_eq!(pasted.callback.error.as_deref(), Some("access_denied"));
        assert_eq!(pasted.callback.description.as_deref(), Some("no way"));
    }

    #[test]
    fn the_tv_may_name_the_client_and_redirect_the_code_belongs_to() {
        let body = format!(
            r#"{{"code_challenge":"{CHALLENGE}","client_id":"6BDSRdpK9hqEBTgU","redirect_uri":"https://tidal.com/android/login/auth"}}"#
        );
        let parsed = HandoffRequest::parse(&body).expect("valid");
        assert_eq!(parsed.client_id.as_deref(), Some("6BDSRdpK9hqEBTgU"));
        assert_eq!(parsed.redirect_uri.as_deref(), Some("https://tidal.com/android/login/auth"));

        // Empty means "the relay's own", and junk is refused before it reaches any URL.
        let body = format!(r#"{{"code_challenge":"{CHALLENGE}","client_id":"","redirect_uri":""}}"#);
        let parsed = HandoffRequest::parse(&body).expect("valid");
        assert_eq!(parsed.client_id, None);
        assert_eq!(parsed.redirect_uri, None);
        let body = format!(r#"{{"code_challenge":"{CHALLENGE}","client_id":"a b"}}"#);
        assert_eq!(HandoffRequest::parse(&body).unwrap_err(), Invalid::BadClientId);
        let body = format!(r#"{{"code_challenge":"{CHALLENGE}","redirect_uri":"http://x/y"}}"#);
        assert_eq!(HandoffRequest::parse(&body).unwrap_err(), Invalid::BadRedirect);
        let body = format!(r#"{{"code_challenge":"{CHALLENGE}","redirect_uri":"https://x/y#z"}}"#);
        assert_eq!(HandoffRequest::parse(&body).unwrap_err(), Invalid::BadRedirect);
    }

    #[test]
    fn query_values_are_decoded_by_name() {
        assert_eq!(param("code=x&state=y", "state").as_deref(), Some("y"));
        assert_eq!(param("state=y", "code"), None);
        assert_eq!(param("", "code"), None);
        assert_eq!(decode_component("a%20b+c"), "a b c");
        assert_eq!(decode_component("%zz"), "%zz");
        assert_eq!(decode_component("100%"), "100%");
        assert_eq!(decode_component("%41%42"), "AB");
    }

    #[test]
    fn only_a_43_character_base64url_id_names_a_handoff() {
        assert!(valid_id(ID));
        assert!(!valid_id(""));
        assert!(!valid_id(&ID[..42]));
        assert!(!valid_id(&"a".repeat(44)));
        assert!(!valid_id(&("a".repeat(42) + "/")));
        assert!(valid_id(&"A".repeat(43)));
        assert!(valid_id(&"-".repeat(43)));
        assert!(valid_id(&"_".repeat(43)));
    }

    #[test]
    fn base64url_vectors() {
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64url(&[0xff, 0xfe, 0xfd]), "__79");
    }

    #[test]
    fn a_stored_record_survives_a_json_round_trip() {
        let mut handoff = request(Some("tv-state"));
        handoff.accept_callback(1_500, Callback { code: Some("abc".into()), ..Default::default() });
        let text = serde_json::to_string(&handoff).expect("serialize");
        assert_eq!(serde_json::from_str::<Handoff>(&text).expect("deserialize"), handoff);
    }

    #[test]
    fn long_error_text_is_trimmed_and_control_characters_dropped() {
        let callback = Callback::from_query(&format!(
            "error=bad%0Anews&error_description={}",
            "x".repeat(MAX_TEXT_LEN + 50)
        ));
        assert_eq!(callback.error.as_deref(), Some("badnews"));
        assert_eq!(callback.description.map(|d| d.len()), Some(MAX_TEXT_LEN));
    }
}
