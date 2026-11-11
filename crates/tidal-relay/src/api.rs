//! The Worker's routes. Everything here is wasm32-only; the decisions it leans on live in
//! `protocol.rs`, and the per-handoff memory lives in the Durable Object (`store.rs`).

use serde_json::json;
use worker::*;

use crate::protocol::{self, Authorize, Callback, HandoffRequest};
use crate::reply::{json, problem};

/// The Durable Object binding that holds one handoff per id (see `wrangler.toml`).
const HANDOFFS: &str = "HANDOFFS";

pub async fn route(mut req: Request, env: Env) -> Result<Response> {
    let url = req.url()?;
    let method = req.method();
    let path = url.path().to_string();

    if method == Method::Options {
        // CORS preflight: nothing else is served for it, and 200 is enough for a browser.
        return Ok(cors(Response::ok("")?).await);
    }
    match (method, path.as_str()) {
        (Method::Post, "/v1/handoff") => Ok(cors(create(&mut req, &env).await?).await),
        (Method::Get, path) if path.starts_with("/v1/handoff/") => {
            let id = &path["/v1/handoff/".len()..];
            Ok(cors(poll(&env, id).await?).await)
        }
        (Method::Get, path) if path.starts_with("/auth/") => {
            let id = &path["/auth/".len()..];
            login(&env, id).await
        }
        (Method::Get, path) if path.starts_with("/paste/") => {
            let id = &path["/paste/".len()..];
            paste_form(&env, id).await
        }
        (Method::Post, path) if path.starts_with("/paste/") => {
            let id = path["/paste/".len()..].to_string();
            paste_submit(&mut req, &env, &id).await
        }
        (Method::Get, "/callback") => callback(&env, url.query().unwrap_or("")).await,
        (Method::Get, "/") => Ok(cors(json(200, index())?).await),
        (Method::Get, "/health") => Ok(cors(json(200, json!({ "status": "ok" }))?).await),
        (_, "/v1/handoff") | (_, "/callback") => {
            Ok(cors(problem(405, "method_not_allowed", "See docs/relay.md for the routes.")?).await)
        }
        _ => Ok(cors(problem(404, "not_found", "Not found.")?).await),
    }
}

/// Everything the relay can be configured with, all of it optional: a TV that names its own client and
/// redirect (the client `tidlers` embeds, whose redirect is the service's own page) needs nothing set
/// here. The variables only matter for a client whose registered redirect is this relay's `/callback`.
struct Config {
    client_id: Option<String>,
    redirect_uri: Option<String>,
    endpoint: String,
    scope: String,
}

impl Config {
    fn load(env: &Env) -> Config {
        Config {
            client_id: var(env, "TIDAL_CLIENT_ID"),
            redirect_uri: var(env, "REDIRECT_URI"),
            endpoint: var(env, "TIDAL_AUTHORIZE_URL")
                .unwrap_or_else(|| "https://login.tidal.com/authorize".to_string()),
            scope: var(env, "TIDAL_SCOPE").unwrap_or_default(),
        }
    }
}

fn var(env: &Env, name: &str) -> Option<String> {
    env.var(name).ok().map(|v| v.to_string()).filter(|v| !v.is_empty())
}

/// `POST /v1/handoff`: the TV asks for a sign-in, the relay answers with the page to show.
async fn create(req: &mut Request, env: &Env) -> Result<Response> {
    let config = Config::load(env);
    // The links this handoff hands to the phone. `wrangler dev` reports the custom domain from
    // `wrangler.toml` with an `http` scheme, which a phone cannot use while testing on the
    // developer's machine, so `PUBLIC_BASE` can name the address the phone should really open.
    let origin = match var(env, "PUBLIC_BASE") {
        Some(base) => base,
        None => req.url()?.origin().ascii_serialization(),
    };
    let origin = origin.trim_end_matches('/');
    let body = req.text().await?;
    let request = match HandoffRequest::parse(&body) {
        Ok(request) => request,
        Err(invalid) => return problem(400, "invalid_request", invalid.message()),
    };

    // The TV names the client it signs in with; without that the relay uses its own credentials.
    let Some(client_id) = request.client_id.clone().or_else(|| config.client_id.clone()) else {
        return problem(
            500,
            "not_configured",
            "No `client_id` in the request and no TIDAL_CLIENT_ID here: set one (`.dev.vars` locally, `wrangler secret put TIDAL_CLIENT_ID` when deployed).",
        );
    };
    let Some(redirect_uri) = request.redirect_uri.clone().or_else(|| config.redirect_uri.clone()) else {
        return problem(
            500,
            "not_configured",
            "No `redirect_uri` in the request and no REDIRECT_URI here: this client can only be used with the one registered for it.",
        );
    };

    let id = random_id();
    let authorize_url = Authorize {
        endpoint: &config.endpoint,
        client_id: client_id.as_str(),
        redirect_uri: redirect_uri.as_str(),
        code_challenge: &request.code_challenge,
        state: &id,
        client_unique_key: request.client_unique_key.as_deref(),
        scope: &config.scope,
    }
    .url();
    // The code only reaches this Worker when the redirect is its own `/callback`. For any other
    // redirect (the service's own page, which is where the embedded client sends it) the phone has to
    // bring the address back by hand, so the answer carries the paste page.
    let paste_url = (config.redirect_uri.as_deref() != Some(redirect_uri.as_str()))
        .then(|| format!("{origin}/paste/{id}"));

    // The record exists before anyone is sent to the sign-in page: a redirect that arrives first
    // would have nowhere to land.
    let stub = stub(env, &id)?;
    let payload = serde_json::to_string(&json!({ "request": request, "authorize_url": authorize_url }))?;
    let stored = call(&stub, "/create", Some(payload)).await?;
    if stored.status_code() != 200 {
        // Without the record the handoff would answer nothing: better to fail here than to send the
        // phone to a sign-in page whose result nobody will read.
        return problem(503, "unavailable", "The relay could not start this hand-in. Try again.");
    }

    json(
        200,
        json!({
            "handoff_id": id,
            "authorize_url": authorize_url,
            "auth_url": format!("{origin}/auth/{id}"),
            "paste_url": paste_url,
            "redirect_uri": redirect_uri,
            "client_id": client_id,
            "expires_in": protocol::HANDOFF_TTL_MS / 1000,
            "poll_after_ms": protocol::POLL_INTERVAL_MS,
        }),
    )
}

/// `GET /v1/handoff/{id}`: the TV asks whether the phone has finished yet.
async fn poll(env: &Env, id: &str) -> Result<Response> {
    if !protocol::valid_id(id) {
        return problem(404, "unknown", "Not a handoff id.");
    }
    call(&stub(env, id)?, "/poll", None).await
}

/// `GET /auth/{id}`: the short address, which ends on the service's sign-in page.
async fn login(env: &Env, id: &str) -> Result<Response> {
    if !protocol::valid_id(id) {
        return page(404, "Link not found", NOT_VALID);
    }
    let response = call(&stub(env, id)?, "/login", None).await?;
    if response.status_code() == 302 {
        return Ok(response);
    }
    page(response.status_code(), "Sign-in unavailable", NOT_VALID)
}

const NOT_VALID: &str = "This sign-in link is not valid any more. Start again on the TV.";

/// `GET /callback`: where the service sends the phone. `state` is the handoff id, so this route can
/// find the handoff without keeping any index of its own.
async fn callback(env: &Env, query: &str) -> Result<Response> {
    let state = protocol::param(query, "state").unwrap_or_default();
    if !protocol::valid_id(&state) {
        return page(400, "Sign-in failed", NOT_VALID);
    }
    let answer = Callback::from_query(query);
    if answer.is_empty() {
        return page(400, "Sign-in failed", "The service answered without a code. Start again on the TV.");
    }
    store(env, &state, answer).await
}

/// `GET /paste/{id}`: the page for an address the service handed to the phone instead of to us.
async fn paste_form(env: &Env, id: &str) -> Result<Response> {
    if !protocol::valid_id(id) {
        return page(404, "Link not found", NOT_VALID);
    }
    // `/login` answers `302` while the handoff is alive. The page it points at is not needed here:
    // the form is the destination.
    let response = call(&stub(env, id)?, "/login", None).await?;
    if response.status_code() != 302 {
        return page(response.status_code(), "This link is over", NOT_VALID);
    }
    form_page(id, None)
}

/// `POST /paste/{id}`: the address the phone shows after signing in, carried here by hand.
async fn paste_submit(req: &mut Request, env: &Env, id: &str) -> Result<Response> {
    if !protocol::valid_id(id) {
        return page(404, "Link not found", NOT_VALID);
    }
    let body = req.text().await?;
    let pasted = protocol::param(&body, "url").unwrap_or_default();
    let paste = match protocol::Paste::parse(&pasted) {
        Ok(paste) => paste,
        // The form is what the user sees next: say what was wrong, in words, and let them retry.
        Err(invalid) => return form_page(id, Some(invalid.message())),
    };
    // The address has to belong to this handoff: the `state` the service echoed is the id in the path.
    match paste.state.as_deref() {
        Some(state) if state == id => {}
        Some(_) => {
            return form_page(
                id,
                Some("That address belongs to another sign-in. Scan the code on the TV again."),
            )
        }
        None => {
            return form_page(
                id,
                Some("That address has no sign-in id at the end: copy the whole address, including what comes after `state=`."),
            )
        }
    }
    store(env, id, paste.callback).await
}

/// Hand one authorization code (or one refusal) to the object that holds the handoff, and answer the
/// phone with what it means. The code is worthless here: only the TV has the verifier.
async fn store(env: &Env, id: &str, answer: Callback) -> Result<Response> {
    let payload = serde_json::to_string(&answer)?;
    let mut response = call(&stub(env, id)?, "/callback", Some(payload)).await?;
    let status = response.status_code();
    let body = response.text().await.unwrap_or_default();
    let kind = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| value.get("status").and_then(|s| s.as_str()).map(str::to_string))
        .unwrap_or_default();

    match (status, kind.as_str()) {
        (200, "stored") => {
            page(200, "Sign-in complete", "You can close this window and go back to the TV.")
        }
        (200, "denied") => {
            page(200, "Sign-in refused", "Nothing was granted. Go back to the TV to try again.")
        }
        (409, _) => page(409, "Already used", "This sign-in was answered before. Go back to the TV."),
        (410, _) => page(410, "Expired", "This sign-in took too long. Start again on the TV."),
        _ => page(status, "Sign-in failed", NOT_VALID),
    }
}

/// The object that holds this handoff, addressed by its id.
fn stub(env: &Env, id: &str) -> Result<Stub> {
    env.durable_object(HANDOFFS)?.id_from_name(id)?.get_stub()
}

/// Talk to the object that holds a handoff. The origin is made up: a stub only looks at the path.
async fn call(stub: &Stub, path: &str, body: Option<String>) -> Result<Response> {
    let url = format!("https://handoff.internal{path}");
    let mut init = RequestInit::new();
    init.with_method(if body.is_some() { Method::Post } else { Method::Get });
    // `/login` answers with the `302` the phone has to follow. A subrequest follows redirects by
    // default, which would fetch the sign-in page here and lose the redirect.
    init.with_redirect(RequestRedirect::Manual);
    if let Some(payload) = body {
        init.with_body(Some(payload.into()));
    }
    let request = Request::new_with_init(&url, &init)?;
    stub.fetch_with_request(request).await
}

/// The TV is a native client, but the relay answers browsers too: say so explicitly.
///
/// The answer is rebuilt rather than edited: one that came back from a Durable Object has immutable
/// headers, and editing those throws `Can't modify immutable headers`.
async fn cors(mut response: Response) -> Response {
    let status = response.status_code();
    let content_type = response
        .headers()
        .get("content-type")
        .ok()
        .flatten()
        .unwrap_or_else(|| "application/json; charset=utf-8".to_string());
    let body = response.text().await.unwrap_or_default();
    let Ok(mut fresh) = (if status == 200 {
        Response::ok(body)
    } else {
        Response::error(body, status)
    }) else {
        return Response::error("", 500).expect("a plain 500 is always buildable");
    };
    let headers = fresh.headers_mut();
    let _ = headers.set("Content-Type", &content_type);
    let _ = headers.set("Cache-Control", "no-store");
    let _ = headers.set("Access-Control-Allow-Origin", "*");
    let _ = headers.set("Access-Control-Allow-Methods", "GET, POST, OPTIONS");
    let _ = headers.set("Access-Control-Allow-Headers", "content-type");
    let _ = headers.set("Access-Control-Max-Age", "86400");
    fresh
}

fn index() -> serde_json::Value {
    json!({
        "service": "tidal-relay",
        "about": "PKCE redirect relay: it carries an authorization code to the TV and never sees a token or a verifier.",
        "routes": {
            "handoff": "POST /v1/handoff { code_challenge, state?, client_unique_key? }",
            "poll": "GET /v1/handoff/{id}",
            "sign_in": "GET /auth/{id}",
            "paste": "GET|POST /paste/{id} (address of the page after signing in)",
            "callback": "GET /callback?code=...&state=...",
        },
    })
}

/// 32 bytes from the runtime's CSPRNG, base64url: the handoff id, which doubles as the `state` the
/// service echoes back on the redirect.
fn random_id() -> String {
    use wasm_bindgen::{JsCast, JsValue};

    // `crypto` is a global in the Workers runtime; `web-sys` only provides the typed view of it.
    let crypto = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("crypto"))
        .ok()
        .and_then(|value| value.dyn_into::<web_sys::Crypto>().ok())
        .expect("the Workers runtime has crypto");
    let mut bytes = [0u8; protocol::ID_BYTES];
    crypto.get_random_values_with_u8_array(&mut bytes).expect("the runtime has a CSPRNG");
    protocol::base64url(&bytes)
}

/// The paste page: no scripts, one field. `id` is a validated handoff id (base64url only) and `error`
/// is one of this file's own sentences, so neither can inject anything.
fn form_page(id: &str, error: Option<&str>) -> Result<Response> {
    let note = match error {
        Some(text) => format!(
            "<p style=\"color:#ffb4b4;line-height:1.5;margin:0 0 16px\">{text}</p>"
        ),
        None => String::new(),
    };
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<title>Finish signing in</title></head>\
<body style=\"font-family:system-ui,sans-serif;background:#0b0d12;color:#f2f4f8;\
margin:0;min-height:100vh;display:grid;place-items:center\">\
<main style=\"padding:24px;max-width:32rem;width:100%\">\
<h1 style=\"font-weight:600;font-size:1.6rem;margin:0 0 12px\">Finish signing in</h1>\
<p style=\"color:#8b93a7;line-height:1.5;margin:0 0 16px\">After signing in, copy the address of the \
page that opened and paste it below.</p>\
{note}\
<form method=\"post\" action=\"/paste/{id}\">\
<textarea id=\"url\" name=\"url\" rows=\"4\" required autocomplete=\"off\" autocapitalize=\"off\" \
spellcheck=\"false\" inputmode=\"url\" placeholder=\"https://tidal.com/android/login/auth?code=...\" \
style=\"width:100%;box-sizing:border-box;font:inherit;padding:12px;border-radius:12px;\
border:1px solid #2b3040;background:#151923;color:#f2f4f8\"></textarea>\
<button type=\"submit\" style=\"margin-top:16px;width:100%;padding:14px;border:0;border-radius:12px;\
background:#33d17a;color:#06110b;font:inherit;font-weight:600\">Finish sign-in</button>\
</form>\
<p style=\"color:#8b93a7;font-size:0.85rem;line-height:1.5;margin:16px 0 0\">The TV completes the \
sign-in itself: this page only hands the code over, and the code is useless without the TV.</p>\
</main></body></html>"
    );
    let mut response = Response::ok(body)?;
    let headers = response.headers_mut();
    headers.set("Content-Type", "text/html; charset=utf-8")?;
    headers.set("Cache-Control", "no-store")?;
    headers.set("X-Content-Type-Options", "nosniff")?;
    headers.set(
        "Content-Security-Policy",
        "default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; base-uri 'none'",
    )?;
    Ok(response)
}

/// A page for the phone: no scripts and no data, just what to do next. `title` and `message` are
/// constants from this file, never anything a request carried.
fn page(status: u16, title: &str, message: &str) -> Result<Response> {
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
<title>{title}</title></head>\
<body style=\"font-family:system-ui,sans-serif;background:#0b0d12;color:#f2f4f8;\
margin:0;min-height:100vh;display:grid;place-items:center\">\
<main style=\"text-align:center;padding:24px;max-width:32rem\">\
<h1 style=\"font-weight:600;font-size:1.6rem;margin:0 0 12px\">{title}</h1>\
<p style=\"color:#8b93a7;line-height:1.5;margin:0\">{message}</p>\
</main></body></html>"
    );
    let mut response = if status == 200 {
        Response::ok(body)?
    } else {
        Response::error(body, status)?
    };
    let headers = response.headers_mut();
    headers.set("Content-Type", "text/html; charset=utf-8")?;
    headers.set("Cache-Control", "no-store")?;
    headers.set("X-Content-Type-Options", "nosniff")?;
    headers.set("Content-Security-Policy", "default-src 'none'; style-src 'unsafe-inline'")?;
    Ok(response)
}
