# The sign-in relay

PKCE issues the `authorization_code` to a `redirect_uri`, and the service only accepts one registered for
that client. A webOS TV has no public address to be one. The client `tidlers` embeds has its own redirect
(`https://tidal.com/android/login/auth`), which the phone's browser reaches but the TV cannot read.

So the app no longer listens for a callback. A small Worker at **`https://tidal-relay.andregama.dev`**
(`crates/tidal-relay`) holds one *handoff* per sign-in: it builds the sign-in page around the TV's
challenge, keeps the code for a few seconds and lets the TV pick it up over ordinary HTTPS. There are two
ways the code gets there, and the default is the second:

* **callback** — the relay's own `redirect_uri` (`/callback`) is registered for the client. The service
  sends the code straight to the relay; nothing is pasted. Requires configuring this Worker.
* **paste** — the client's registered redirect is the service's own page, so the code stays in the
  phone's address bar. The user copies the address and pastes it on `/paste/{id}`; the relay checks the
  `state` belongs to that handoff and stores the code. Needs no configuration at all.

What the relay never has: the **`code_verifier`**, any token, or any session. The TV keeps the verifier
and does the exchange itself, so a code that passes through here is worthless to the relay and to anyone
who reads the address: only the TV can redeem it.

## The exchange

1. The TV generates its own PKCE pair (`TidalAuth::with_pkce()`; the verifier stays in the process) and a
   random `state`, then posts the **public challenge** and the client/redirect the code will belong to:
   `POST /v1/handoff {"code_challenge", "state", "client_unique_key", "client_id", "redirect_uri"}`.
   A TV signing in with a client of its own leaves `redirect_uri` out, so the relay's own registered one
   is used and the code arrives by itself.
2. The relay answers `{ handoff_id, authorize_url, auth_url, paste_url, redirect_uri, client_id,
   expires_in, poll_after_ms }` and creates a record for that handoff. `authorize_url` is the service's
   sign-in page built with that `redirect_uri`, the `code_challenge` and `state = handoff_id`.
   `paste_url` comes back only when the redirect is not the relay's own, i.e. when the phone has to bring
   the code's address by hand.
3. The TV shows `authorize_url` as a QR code, and `paste_url` as a second one when it is there (both are
   also logged: a hand-off id is not something anyone types). `auth_url` (`/auth/<handoff_id>`) answers
   `302` to the sign-in page.
4. The phone signs in, and the code takes one of the two routes:
   * **callback**: the service redirects to `/callback?code=…&state=<handoff_id>`; the relay finds the
     record by that `state` and stores the code (or the refusal).
   * **paste**: the phone shows `https://…/login/auth?code=…&state=<handoff_id>`; the user pastes that
     address on `/paste/<id>`, which refuses it unless the `state` is this handoff's, then stores the
     code the same way.
   Either way the page tells the user to go back to the TV, and a second answer for the same handoff is
   refused (`409`).
5. The TV polls `GET /v1/handoff/<id>` every 1.5 s. The first poll after the code arrives returns it and
   **empties the record** — one use only. The TV checks the `state` it gets back, then calls
   `finish_pkce_login("<redirect_uri>?code=…&state=…")`, which exchanges the code with the verifier that
   never left the TV.

The relay is a mailbox for one code and a page that hands it over. It is not an authorization server: it
never talks to the service's `/token` endpoint.

## Routes

| Route | Answer |
| --- | --- |
| `POST /v1/handoff` | `200` with the handoff (below), `400` for an unusable body, `500` if neither the body nor the Worker names a client |
| `GET /v1/handoff/{id}` | `200 {"status":"pending"}`, `200 {"status":"code","code","state"}`, `200 {"status":"denied","error","description"}`, `410` expired, `404` unknown/answered |
| `GET /auth/{id}` | `302` to the sign-in page, or an HTML page saying the link is over |
| `GET /paste/{id}` | the form where the address from the phone is pasted; HTML page, no scripts |
| `POST /paste/{id}` | same page with the answer: `200` stored/denied, `409`/`410`, or the form again with what was wrong |
| `GET /callback` | the service's redirect; HTML page for the phone |
| `GET /`, `GET /health` | what this is, and a liveness check |

`POST /v1/handoff` validates the body: the challenge must be 43–128 characters (base64url), the optional
`state` up to 512, `client_unique_key` up to 128, `client_id` up to 128 printable characters and
`redirect_uri` an `https://` URL of up to 512, the body up to 4 KiB. The pasted address is capped at 4 KiB
and must carry a `code` (or an `error`) and a `state`. Answers carry
`cache-control: no-store`; the two JSON routes also allow CORS so a browser can drive the flow.

## Lifetime and state

One **Durable Object per handoff**, addressed by name (`id_from_name`), so the code is written and read in
one place. A KV namespace would not do: its writes are only eventually visible and the code lives for 60
seconds. The Worker itself keeps nothing.

* The handoff lives **300 s** (`HANDOFF_TTL_MS`).
* The code lives **60 s** (`CODE_TTL_MS`), never past the handoff.
* Delivery, refusal and expiry all delete the record.
* Every write also sets an alarm for the moment the record stops being useful, so abandoned handoffs
  delete themselves instead of piling up.

## Deployment

Nothing is deployed by the repository's build: `wrangler deploy` (or `npx wrangler@latest deploy` from
`crates/tidal-relay`) is the one explicit step. It creates the custom domain `tidal-relay.andregama.dev`
for the zone, the `HANDOFFS` Durable Object binding and the migration.

`crates/tidal-relay/wrangler.toml` holds the configuration. **All of it is optional**: the TV names its
client and redirect in every handoff, and the default flow (paste) needs nothing configured. The
variables only matter for a client whose registered redirect is this Worker's `/callback`, which is the
variant where the code arrives by itself.

* `TIDAL_CLIENT_ID` — only for that variant, and it has to be the **same client the TV signs in with**.
  It is public in the sense that it ends up in the URL the phone opens: a TV that sends its own client
  wins over this one, and the response echoes the pair back. Locally it comes from `.dev.vars`
  (git-ignored); when deployed, from `wrangler secret put TIDAL_CLIENT_ID`. The secret half is never
  needed here: the relay does not call the token endpoint.

* `REDIRECT_URI` — `https://tidal-relay.andregama.dev/callback`, and it must be registered for that
  client. It is also what tells the Worker whether a handoff will come back on its own: a handoff whose
  `redirect_uri` equals this one gets no `paste_url`.

* `TIDAL_AUTHORIZE_URL` — `https://login.tidal.com/authorize` by default.

* `TIDAL_SCOPE` — empty by default, which sends no `scope` (what the TV's client does).

The relay crate is its own workspace (`exclude`d from the root one) and the `worker` crate is a
wasm-only dependency, so `cargo test --workspace` and `make check` stay host-only.

## Working on it

```
make relay-test     # unit tests of the protocol rules (host, no wasm tooling)
make relay-check    # cargo check --target wasm32-unknown-unknown (needs rustup target add)
cd crates/tidal-relay && npx wrangler@latest dev   # local Worker + Durable Object on :8787
```

With a local Worker, point the app at it: `TIDE_RELAY_BASE=http://127.0.0.1:8787 make sim`.

`protocol.rs` holds every decision as a pure function (what a body may say, what a pasted address must
carry, what a poll answers, when a record dies), which is where the 20 tests are; `api.rs` is the routing
and the HTML pages; `store.rs` is the Durable Object.

## Known gap

The callback variant still rests on a client Tidal has registered
`https://tidal-relay.andregama.dev/callback` for, and none is known to exist: measured against the real
service, `login.tidal.com` answers `11102` for every client sent to a redirect it did not register
(`docs/auth.md` §Troubleshooting). That is why the default flow does not need it.

The paste variant is exercised end to end by hand against the real service (sign in with the embedded
client, copy the address, exchange it); what has not been exercised is the phone's copy step on every
browser — one that deep-links into an app and drops the address cannot finish, and the TV then offers the
device-code fallback.

The PKCE pair does not depend on the client id (`S256(code_verifier)`), so the mismatch this used to
have — authorize as the relay's client, refresh with the secret embedded in `tidlers` — only surfaced on
the second boot. It is now rejected on both sides: the TV sends its own client and compares the echo, so
a relay configured for another one gives up with that reason instead of signing in to a session that
cannot refresh.
