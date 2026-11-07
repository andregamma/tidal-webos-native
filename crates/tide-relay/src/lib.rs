//! `tidal-relay`: the PKCE redirect relay of the TV client (see `docs/relay.md`).
//!
//! The TV generates its own PKCE pair and keeps the verifier; it sends only the challenge here. This
//! Worker builds the service's authorize URL around that challenge and the redirect the TV names,
//! receives the `authorization_code`, and holds it for a few seconds until the TV picks it up. When
//! that redirect is not the relay's own (the client the TV's library embeds sends the code to the
//! service's page instead), the phone brings the address back through `/paste/{id}`. It never sees a
//! token or a verifier: the TV does the exchange itself.
//!
//!   POST /v1/handoff        -> { handoff_id, authorize_url, auth_url, paste_url, redirect_uri, ... }
//!   GET  /v1/handoff/{id}   -> pending | code | denied | expired | unknown
//!   GET  /auth/{id}         -> 302 to the sign-in page (the short address typed on the phone)
//!   GET  /paste/{id}        -> form for the address the service handed to the phone instead
//!   POST /paste/{id}        -> takes that address, stores the code it carries
//!   GET  /callback          -> where the service sends the phone (`state` = handoff id)
//!   GET  /                  -> what this is
//!
//! `protocol.rs` holds every decision as a pure function, so the rules are unit-tested on the host.
//! The Worker half (`api.rs`) and the Durable Object that holds one handoff (`store.rs`) only exist
//! for wasm32; that is also why the `worker` crate is a wasm-only dependency.

pub mod protocol;

#[cfg(target_arch = "wasm32")]
mod api;
#[cfg(target_arch = "wasm32")]
mod store;

#[cfg(target_arch = "wasm32")]
pub use store::HandoffStore;

#[cfg(target_arch = "wasm32")]
use worker::*;

/// The two answer shapes the Worker and the object both send.
#[cfg(target_arch = "wasm32")]
mod reply {
    use worker::*;

    /// JSON with the headers every answer here needs: the right type, and no caching anywhere.
    pub fn json(status: u16, value: serde_json::Value) -> Result<Response> {
        let body = serde_json::to_string(&value)?;
        let mut response = if status == 200 {
            Response::ok(body)?
        } else {
            Response::error(body, status)?
        };
        let headers = response.headers_mut();
        headers.set("Content-Type", "application/json; charset=utf-8")?;
        headers.set("Cache-Control", "no-store")?;
        Ok(response)
    }

    /// An error a client can branch on: `status` repeats the HTTP answer in one word.
    pub fn problem(status: u16, code: &str, message: &str) -> Result<Response> {
        json(status, serde_json::json!({ "status": code, "message": message }))
    }
}

#[cfg(target_arch = "wasm32")]
#[event(fetch)]
pub async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    api::route(req, env).await
}
