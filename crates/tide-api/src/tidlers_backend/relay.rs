//! The TV's half of the sign-in relay (`crates/tide-relay`, described in `docs/relay.md`).
//!
//! The PKCE pair is made here and the verifier never leaves this process. The relay only receives
//! the public challenge and the redirect the code will be issued for, builds the service's authorize
//! URL around them and, once the phone has signed in, holds the `authorization_code` until this TV
//! asks for it. When that redirect is the client's own page (what the library embeds), the code never
//! reaches the relay: the phone brings the address back through the relay's paste page, and the code
//! is useless without the verifier anyway. The exchange (`finish_pkce_login`) always happens here.
//!
//! Everything that can be decided without a socket — reading an answer, comparing `state` — is a
//! plain function, so it is unit-tested on the host.

use serde::Deserialize;
use std::time::Duration;

/// Where the relay lives. `TIDE_RELAY_BASE` points a development build at a local `wrangler dev`.
pub const DEFAULT_BASE: &str = "https://tidal-relay.andregama.dev";
/// Environment variable that overrides [`DEFAULT_BASE`].
pub const BASE_ENV: &str = "TIDE_RELAY_BASE";
/// How long one relay request may take before it counts as a failure.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Failed polls tolerated in a row: a phone on a bad connection must not end the sign-in.
pub const MAX_POLL_FAILURES: u32 = 3;

/// The HTTP client used for the relay. Plain `reqwest`, already in the tree through `tidlers`.
pub type Http = reqwest::Client;

/// The handoff this TV is waiting on, as the relay described it.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Handoff {
    pub handoff_id: String,
    /// The service's sign-in page: what the QR code carries.
    pub authorize_url: String,
    /// The relay's short address: what gets typed on the phone.
    pub auth_url: String,
    /// Where the phone pastes the address it gets after signing in, when the code does not come back
    /// on its own (`redirect_uri` is not this relay's).
    #[serde(default)]
    pub paste_url: Option<String>,
    /// Where the code was issued; the exchange has to use exactly this.
    pub redirect_uri: String,
    /// The client the code belongs to; the exchange has to use exactly this.
    pub client_id: String,
    /// Seconds the relay keeps the handoff.
    #[serde(default)]
    pub expires_in: u32,
}

/// What one poll says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Poll {
    /// Nobody has signed in yet.
    Pending,
    /// The code is here, and it is for this `state`.
    Code { code: String, state: Option<String> },
    /// The phone refused the sign-in.
    Denied { error: String, description: Option<String> },
    /// The relay no longer holds this handoff: start over.
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The relay could not be reached, or answered something unusable.
    Transport(String),
    /// The relay answered with an error status.
    Refused { status: u16, message: String },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Transport(why) => write!(f, "{why}"),
            Error::Refused { status, message } => write!(f, "the relay answered {status}: {message}"),
        }
    }
}

impl std::error::Error for Error {}

/// One answer from `GET /v1/handoff/{id}`.
#[derive(Deserialize)]
struct Answer {
    status: String,
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    description: Option<String>,
}

/// The relay this build talks to: `TIDE_RELAY_BASE`, else [`DEFAULT_BASE`].
pub fn base() -> String {
    std::env::var(BASE_ENV)
        .ok()
        .map(|b| b.trim().trim_end_matches('/').to_string())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE.to_string())
}

pub fn client() -> Result<Http, Error> {
    Http::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|e| Error::Transport(format!("could not build the HTTP client: {e}")))
}

/// What the TV asks the relay for.
pub struct Ask<'a> {
    /// The public half of the PKCE pair.
    pub challenge: &'a str,
    /// The library's per-install key: the exchange sends it, so the sign-in page must too.
    pub unique_key: &'a str,
    /// This TV's own `state`, echoed back on delivery.
    pub state: &'a str,
    /// The client the code will belong to. Naming it here means the relay needs no credentials of its
    /// own; the sign-in page is built from it.
    pub client_id: &'a str,
    /// Where the service will deliver the code, when this TV knows it. `None` lets the relay use its
    /// own registered `redirect_uri`, which is the only one that works with a client of our own.
    pub redirect_uri: Option<&'a str>,
}

/// Ask the relay for a handoff. Only the public challenge, this TV's `state`, the library's device key
/// and the client/redirect the code belongs to travel; the verifier stays here.
pub async fn create(http: &Http, base: &str, ask: &Ask<'_>) -> Result<Handoff, Error> {
    let mut body = serde_json::json!({
        "code_challenge": ask.challenge,
        "state": ask.state,
        "client_unique_key": ask.unique_key,
        "client_id": ask.client_id,
    });
    if let Some(redirect) = ask.redirect_uri {
        body["redirect_uri"] = redirect.into();
    }
    let body = body.to_string();
    let response = http
        .post(format!("{base}/v1/handoff"))
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| Error::Transport(format!("could not reach the relay: {e}")))?;
    let status = response.status().as_u16();
    let text = body_of(response).await?;
    if status != 200 {
        return Err(refused(status, &text));
    }
    serde_json::from_str(&text)
        .map_err(|_| Error::Transport("the relay answered an unexpected handoff".into()))
}

/// Ask the relay whether the phone has finished yet.
pub async fn poll(http: &Http, base: &str, handoff_id: &str) -> Result<Poll, Error> {
    let response = http
        .get(format!("{base}/v1/handoff/{handoff_id}"))
        .send()
        .await
        .map_err(|e| Error::Transport(format!("could not reach the relay: {e}")))?;
    let status = response.status().as_u16();
    let text = body_of(response).await?;
    read_poll(status, &text)
}

async fn body_of(response: reqwest::Response) -> Result<String, Error> {
    response
        .text()
        .await
        .map_err(|e| Error::Transport(format!("the relay closed the answer: {e}")))
}

fn refused(status: u16, text: &str) -> Error {
    let message = serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| value.get("message").and_then(|m| m.as_str()).map(str::to_string))
        .unwrap_or_else(|| text.trim().to_string());
    Error::Refused { status, message }
}

/// Read one poll answer, with the HTTP status it came with as part of the answer.
pub fn read_poll(status: u16, body: &str) -> Result<Poll, Error> {
    let answer: Answer = serde_json::from_str(body)
        .map_err(|_| Error::Transport(format!("unreadable relay answer ({status})")))?;
    // Owned, so the fields below can be moved out of `answer`.
    let kind = answer.status.clone();
    match (status, kind.as_str()) {
        (200, "pending") => Ok(Poll::Pending),
        (200, "code") => match answer.code.filter(|code| !code.is_empty()) {
            Some(code) => Ok(Poll::Code { code, state: answer.state }),
            None => Err(Error::Transport("the relay sent a code answer without a code".into())),
        },
        (200, "denied") => Ok(Poll::Denied {
            error: answer.error.unwrap_or_else(|| "access_denied".into()),
            description: answer.description,
        }),
        // A handoff the relay forgot is as good as one that ran out.
        (410, _) | (404, _) => Ok(Poll::Expired),
        _ => Err(refused(status, body)),
    }
}

/// `state` for one attempt: 32 bytes from the OS CSPRNG, hex. The relay echoes it back with the
/// code, which is what tells this attempt's answer apart from any other.
pub fn random_state() -> String {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).expect("the platform has no random source");
    hex(&bytes)
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Percent-encode one query value, keeping only what never needs encoding.
pub fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(byte as char),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// `state` comparison that does not stop at the first difference. The length is compared first and
/// leaks nothing: every `state` this app makes is the same length.
pub fn eq_const_time(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pending_answer_is_pending() {
        assert_eq!(read_poll(200, r#"{"status":"pending"}"#).unwrap(), Poll::Pending);
    }

    #[test]
    fn a_code_answer_carries_the_state_back() {
        let poll = read_poll(200, r#"{"status":"code","code":"abc-123","state":"ff"}"#);
        assert_eq!(poll.unwrap(), Poll::Code { code: "abc-123".into(), state: Some("ff".into()) });
    }

    #[test]
    fn a_code_answer_without_a_code_is_a_transport_error() {
        for body in [r#"{"status":"code"}"#, r#"{"status":"code","code":""}"#] {
            assert!(matches!(read_poll(200, body), Err(Error::Transport(_))), "{body}");
        }
    }

    #[test]
    fn a_denial_keeps_the_reason_when_there_is_one() {
        let with_reason = r#"{"status":"denied","error":"access_denied","description":"no"}"#;
        assert_eq!(
            read_poll(200, with_reason).unwrap(),
            Poll::Denied { error: "access_denied".into(), description: Some("no".into()) }
        );
        assert_eq!(
            read_poll(200, r#"{"status":"denied"}"#).unwrap(),
            Poll::Denied { error: "access_denied".into(), description: None }
        );
    }

    #[test]
    fn a_forgotten_or_expired_handoff_ends_the_wait() {
        assert_eq!(read_poll(404, r#"{"status":"unknown"}"#).unwrap(), Poll::Expired);
        assert_eq!(read_poll(410, r#"{"status":"expired"}"#).unwrap(), Poll::Expired);
    }

    #[test]
    fn an_error_answer_keeps_the_status_and_the_message() {
        let body = r#"{"status":"not_configured","message":"set the client id"}"#;
        assert_eq!(
            read_poll(500, body).unwrap_err(),
            Error::Refused { status: 500, message: "set the client id".into() }
        );
    }

    #[test]
    fn unreadable_answers_are_transport_errors() {
        assert!(matches!(read_poll(200, "not json"), Err(Error::Transport(_))));
    }

    #[test]
    fn a_handoff_needs_the_fields_the_exchange_uses() {
        let json = r#"{"handoff_id":"id","authorize_url":"https://login/x","auth_url":
            "https://relay/auth/id","redirect_uri":"https://relay/callback","client_id":"abc",
            "expires_in":300,"poll_after_ms":1500}"#;
        let handoff: Handoff = serde_json::from_str(json).unwrap();
        assert_eq!(handoff.redirect_uri, "https://relay/callback");
        assert_eq!(handoff.expires_in, 300);
        // An older relay (or one whose own redirect takes the code) sends no paste page.
        assert_eq!(handoff.paste_url, None);
        assert!(serde_json::from_str::<Handoff>(r#"{"handoff_id":"id"}"#).is_err());
    }

    #[test]
    fn a_state_is_random_and_hex() {
        let state = random_state();
        assert_eq!(state.len(), 64);
        assert!(state.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(state, random_state());
    }

    #[test]
    fn state_comparison_sees_every_byte() {
        assert!(eq_const_time("abc", "abc"));
        assert!(!eq_const_time("abc", "abd"));
        assert!(!eq_const_time("abc", "ab"));
        assert!(!eq_const_time("", "a"));
    }

    #[test]
    fn query_values_are_encoded() {
        assert_eq!(url_encode("abc-123_x.y~"), "abc-123_x.y~");
        assert_eq!(url_encode("a+b&c=d"), "a%2Bb%26c%3Dd");
    }
}
