//! The Durable Object that holds one handoff: created by the TV's request, filled by the service's
//! redirect, emptied by the TV's poll.
//!
//! There is one object per handoff id, addressed by name, so the code is written and read in the
//! same place. A KV namespace would not do: its writes are only eventually visible, and the code
//! lives for 60 seconds. The object is also what keeps the Worker itself free of state.
//!
//! Internal API, reachable only through a stub from `api.rs`:
//!   POST /create    { request, authorize_url }        -> 200
//!   GET  /login                                       -> 302 | 404 | 410
//!   POST /callback  { code, error, description }      -> 200 stored|denied | 400 | 404 | 409 | 410
//!   GET  /poll                                        -> 200 | 404 | 410

use serde::Deserialize;
use serde_json::json;
use worker::*;

use crate::protocol::{self, Callback, CallbackOutcome, Delivery, Handoff, HandoffRequest, Poll};
use crate::reply::{json, problem};

/// One record per object: there is one object per handoff.
const RECORD: &str = "handoff";

/// What `api.rs` hands over: the TV's request, and the sign-in page it built around it.
#[derive(Deserialize)]
struct Create {
    request: HandoffRequest,
    authorize_url: String,
}

#[durable_object]
pub struct HandoffStore {
    state: State,
    /// Kept for bindings this object may need later; the relay reads its config in the Worker.
    #[allow(dead_code)]
    env: Env,
}

impl DurableObject for HandoffStore {
    fn new(state: State, env: Env) -> Self {
        Self { state, env }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let path = req.url()?.path().to_string();
        match (req.method(), path.as_str()) {
            (Method::Post, "/create") => self.create(&mut req).await,
            (Method::Post, "/callback") => self.callback(&mut req).await,
            (Method::Get, "/poll") => self.poll().await,
            (Method::Get, "/login") => self.login().await,
            _ => problem(404, "unknown", "No such route on this handoff."),
        }
    }

    /// Due when the record stopped being useful: the handoff expired, or the code window closed.
    async fn alarm(&self) -> Result<Response> {
        self.state.storage().delete_all().await?;
        Response::empty()
    }
}

impl HandoffStore {
    async fn create(&self, req: &mut Request) -> Result<Response> {
        let body = req.text().await?;
        let Create { request, authorize_url } =
            serde_json::from_str(&body).map_err(|_| Error::RustError("bad handoff body".into()))?;

        let record = Handoff::new(&request, authorize_url, now_ms());
        let storage = self.state.storage();
        storage.put(RECORD, &record).await?;
        // Nothing here is useful after `drop_at_ms`: let the object delete itself instead of
        // leaving abandoned handoffs behind.
        storage.set_alarm(record.drop_at_ms()).await?;
        Response::empty()
    }

    /// `GET /auth/{id}` on the public side: the short address that ends on the sign-in page.
    async fn login(&self) -> Result<Response> {
        let Some(record) = self.record().await? else {
            return problem(404, "unknown", "This sign-in link is no longer valid.");
        };
        if now_ms() >= record.drop_at_ms() {
            return problem(410, "expired", "This sign-in expired. Start again on the TV.");
        }
        Response::redirect(Url::parse(&record.authorize_url)?)
    }

    async fn callback(&self, req: &mut Request) -> Result<Response> {
        let body = req.text().await?;
        let callback: Callback =
            serde_json::from_str(&body).map_err(|_| Error::RustError("bad callback body".into()))?;

        let storage = self.state.storage();
        let Some(mut record) = self.record().await? else {
            return problem(404, "unknown", "This sign-in link is not waiting for an answer.");
        };
        match record.accept_callback(now_ms(), callback) {
            CallbackOutcome::Stored => {
                let denied = matches!(record.delivery, Some(Delivery::Denied { .. }));
                storage.put(RECORD, &record).await?;
                storage.set_alarm(record.drop_at_ms()).await?;
                json(200, json!({ "status": if denied { "denied" } else { "stored" } }))
            }
            CallbackOutcome::AlreadyUsed => {
                problem(409, "already_used", "This sign-in was already answered.")
            }
            CallbackOutcome::Expired => {
                storage.delete_all().await?;
                problem(410, "expired", "This sign-in expired before the answer arrived.")
            }
            CallbackOutcome::Nothing => {
                problem(400, "missing_code", "The redirect carried no authorization code.")
            }
        }
    }

    async fn poll(&self) -> Result<Response> {
        let storage = self.state.storage();
        let Some(record) = self.record().await? else {
            return problem(404, "unknown", "No such handoff: it was answered, or it never existed.");
        };
        match record.poll(now_ms()) {
            Poll::Pending { expires_in_ms } => json(
                200,
                json!({
                    "status": "pending",
                    "expires_in": (expires_in_ms + 999) / 1000,
                    "retry_after_ms": protocol::POLL_INTERVAL_MS,
                }),
            ),
            Poll::Code { code, state } => {
                // One use only: the code leaves and the object starts over empty.
                storage.delete_all().await?;
                json(200, json!({ "status": "code", "code": code, "state": state }))
            }
            Poll::Denied { error, description } => {
                storage.delete_all().await?;
                json(200, json!({ "status": "denied", "error": error, "description": description }))
            }
            Poll::Expired => {
                storage.delete_all().await?;
                problem(410, "expired", "This sign-in expired before it was answered.")
            }
        }
    }

    async fn record(&self) -> Result<Option<Handoff>> {
        self.state.storage().get::<Handoff>(RECORD).await
    }
}

fn now_ms() -> i64 {
    Date::now().as_millis() as i64
}
