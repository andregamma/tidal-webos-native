# Authentication

Sign-in uses the **PKCE authorization-code flow** of the `tidlers` crate; the **device-code flow** is the
fallback. The relay (`docs/relay.md`) holds the handoff, and the code reaches the TV either through the
relay's own callback or pasted by the user on the relay's page. The design (screens, state machine,
risks) is in `docs/pkce.md`; this page is the map of the code.

## Why PKCE first

PKCE is the only flow that yields `HI_RES_LOSSLESS`. The device-code fallback still signs in, but at CD
quality, so it is offered only when PKCE cannot work — and the app says so on screen.

## Primary flow (PKCE through the relay)

1. `Cmd::StartLogin` → `tidlers_backend::login` → `login_pkce`:
   * forks `TidalAuth::with_pkce()`, which makes the PKCE pair — the verifier stays in this process and
     only the challenge travels,
   * makes a random `state` (32 bytes, hex) and posts it with the challenge, the client id and the
     `redirect_uri` of that client to the relay (`POST /v1/handoff`, see `docs/relay.md`).
2. The relay answers with the service's sign-in page, the short link, the pair the code will belong to and
   — when that `redirect_uri` is not the relay's own — a **paste page**. The pair is the one Tidal
   registered together: the library's client with its own page, or a client of our own (§Credentials)
   with the relay's `/callback`. A handoff naming another `client_id` is refused before any QR code is
   shown.
3. `Evt::PkceLogin { qr_url, paste_url, expires_in }`. The TV shows two QR codes: the service's sign-in
   page and the relay's paste page. No address is on screen (a hand-off id is nobody's idea of typeable);
   both URLs, and the short link, are in `tide.log` for desktop runs.
4. The phone signs in. The code comes back one of two ways:
   * **paste**, the default: the client `tidlers` embeds has its own registered redirect
     (`https://tidal.com/android/login/auth`), so the service hands the code to that page and it stays in
     the phone's address bar. The user copies the address and pastes it on `/paste/{id}`; the relay checks
     the `state` is this handoff's, then stores the code. Anyone who saw the address could not use it:
     the verifier never left this TV.
   * **callback**: with a client of our own (`TIDAL_CLIENT_ID` set) the TV names no redirect, the relay
     uses its registered `/callback` and receives the code itself, so nothing has to be pasted.
   Either way the TV polls once every 1.5 s and takes the code on the first answer that has one; the relay
   forgets it in the same step, so it is delivered once.
5. `finish_pkce_login("<redirect_uri>?code=…&state=…")` exchanges the code here, with the verifier. A code
   whose `state` is not this attempt's is refused (constant-time compare). The refresh token is saved with
   `flow: "pkce"`, then `subscription()` is read once: `Evt::LoggedIn { max_quality }` carries the
   account's `highest_sound_quality`. The UI goes straight to the library for HiRes (or an unknown label)
   and shows the "signed in, but not in HiRes" notice otherwise.
6. Limits: the handoff lives 300 s and the code 60 s (the relay's rules, not ours); the TV stops polling
   at the expiry it was given and gives up after 4 failed polls in a row. `Cmd::CancelLogin` (Back on the
   PKCE screen) and `Cmd::StartLoginFallback` (OK) interrupt the wait within ~150 ms. The code, the
   `state` and the verifier are never logged; the relay base, the sign-in page, the short link and the
   paste page are.

## Fallback (device code)

* Every PKCE failure (relay unreachable, link expired, code refused by the service) ends in
  `Evt::FallbackOffered(reason)`; the screen offers **Try again** / **Use the common QR code**
  (pkce.md §5.1). Choosing the second sends `Cmd::StartLoginFallback`.
* `login_device` is the old flow: `get_oauth_link()` → QR + short code → `wait_for_oauth()`. A new code is
  issued when one expires (max 6).
* Known library limits: `wait_for_oauth` treats `slow_down` as fatal (press OK to retry) and the scope is
  fixed to `r_usr w_usr w_sub`.
* After a fallback sign-in the app compares `max_quality` with `HI_RES_LOSSLESS` and offers
  **Try HiRes** / **Continue anyway** before entering the library.

## Credentials

**Unset by default, and that is a supported configuration.** Without the variables the app signs in with
the PKCE client embedded in `tidlers`; its registered redirect is the service's own page, so the phone
brings the address back through the relay's paste page (§Primary flow, step 4). Nothing has to be
registered anywhere, and the relay needs no credentials of its own.

Setting **`TIDAL_CLIENT_ID`** and **`TIDAL_CLIENT_SECRET`** switches the PKCE flow to your own client, and
then the TV stops naming a redirect: it sends only the `client_id` and the relay uses its own
`REDIRECT_URI` (`https://tidal-relay.andregama.dev/callback`). The code then arrives by itself — no
pasting. Tidal has to have that URI registered for that client: an unregistered pair is answered with
`11102` before the sign-in page even appears (§Troubleshooting), and a code delivered to a redirect nobody
can read would be useless anyway. The relay may know that client id (the TV compares the echo and refuses
a mismatch) but never the secret.

```bash
# either in .env at the repo root (what the Makefile loads), or exported in the shell
export TIDAL_CLIENT_ID="your-client-id"
export TIDAL_CLIENT_SECRET="your-client-secret"
make sim
```

Where they live, by target:

* **Simulator:** `.env` at the repo root (git-ignored). The `Makefile` does `-include .env` and exports
  exactly `TIDAL_CLIENT_ID`, `TIDAL_CLIENT_SECRET`, `TIDAL_QUALITY`, `TIDE_RELAY_BASE` and
  `TIDE_DATA_DIR` to every recipe, so the file is enough — no `export` in the terminal. Plain
  `KEY=value` lines, **no quotes** (make would keep them as part of the value).
* **A bare `cargo run`/`cargo test`** does not read `.env`: export the variables in that shell
  (`set -a; . ./.env; set +a` in bash) or use `make`.
* **Relay Worker:** `crates/tide-relay/.dev.vars` for `wrangler dev`; `wrangler secret put TIDAL_CLIENT_ID`
  when deployed.
* **On the TV:** nothing sets them yet — the app is a native binary started by webOS
  (`packaging/appinfo.json`, `main`), with no launcher script, so the PKCE flow has no client id there and
  falls back to the device-code flow. A wrapper script that exports these before the binary is the missing
  piece (and it is where the secret would end up inside the `.ipk`).

* **TV** (`tidlers_backend::auth_for`): the pair is written into the `PkceConfig` the crate builds, so the
  challenge, the code exchange **and every refresh of that session** (`new_client(Flow::Pkce)`, on each
  boot) authenticate as the same client. The challenge itself is only `S256(code_verifier)` and does not
  depend on the client id, which is why a mismatch is invisible until the refresh — so a handoff whose
  `client_id` is not this one is refused before the QR code is shown. Without the variables the crate's
  embedded PKCE client is used and a notice goes to `tide.log`.
* **Relay** (`crates/tide-relay`): it builds the authorize URL with the same `TIDAL_CLIENT_ID` — from
  `.dev.vars` locally, `wrangler secret put TIDAL_CLIENT_ID` when deployed — and never needs the secret,
  because it never calls `/token`.

What used to break: the exchange adopted the relay's `client_id` while the refresh kept the secret
embedded in the library. The code belonged to the relay's client, the refresh authenticated as another
one, and every session was refused (`invalid_client`) on the next boot. Both sides now read the same
environment variables, and a mismatch is reported on screen and in `tide.log` as
`the relay signs in with client X but this build has Y`.

The **device-code fallback** is a different client: it keeps the OAuth client credentials the crate
embeds, which is why the session file stores which flow produced the token (`Flow`).

Known library limit: `finish_pkce_login` posts `client_id`, `redirect_uri`, `code_verifier` and
`client_unique_key` to `/token` but **no `client_secret`** (the crate only uses the secret for the refresh
grant's Basic auth). That is what a public PKCE client expects. If the service answers the exchange with
an error about the client, the body it sent is logged (`pkce: the token exchange failed: …`), because that
body names the cause.

### `11102` on the sign-in page

`login.tidal.com` answers the generic "Algo deu errado … 11102" page **before** any e-mail/password
prompt: the request was refused on validation, so the PKCE pair, the `state` and the
`client_unique_key` are not the culprits (they only matter after a user signs in). What it validates is
the client:

* **`redirect_uri` must match, byte for byte, one registered for that `client_id`** in the Tidal
dashboard: same scheme, host, path (`/callback`), no trailing slash, no query, no fragment. The TV logs
the pair it is actually using (`pkce: client …, redirect …`), which is what to compare with the dashboard.
* **The client must be allowed to use this flow.** A TIDAL Developer Platform (Open API) app goes through
`authorize` with `scope` and its granted API access, while `appMode=android`/`client_unique_key` are
parameters of the mobile/streaming client that `tidlers` imitates. An Open API client without that access
is refused exactly here; and even when it signs in, it is not the client that yields HiRes stream URLs —
`finish_pkce_login` also hard-codes the exchange's `scope` to `r_usr w_usr w_sub`, which an Open API app
does not have.

Measured against the real service (November 2026): the client `tidlers` embeds (`6BDSRdpK9hqEBTgU`) with
**its own** redirect opens the sign-in page; the same client with the relay's `/callback` gives `11102`,
and so does a dashboard client (Open API) with the relay's `/callback`. A redirect is bound to the client
that registered it, and no dashboard app can register a URI it does not control the way the relay needs.

That is why the default flow no longer asks the service to redirect anywhere unusual: it signs in with the
client's own redirect and the phone carries the resulting address back (§Primary flow, step 4). `11102`
only comes back if `TIDAL_CLIENT_ID`/`TIDAL_CLIENT_SECRET` name a client whose redirect is not the relay's
`/callback` — in which case unset them.

## Stored data

Only the refresh token and the flow that produced it, in `tidlers-session.json` (mode 0600, written with
fsync + atomic rename and read back to verify). Desktop: `$TIDE_DATA_DIR`, else `%LOCALAPPDATA%\tide-tv`
(Windows) / `$XDG_DATA_HOME/tide-tv` / `~/.local/share/tide-tv`. TV: first writable of
`/media/developer/tide-tv` (survives reinstalling the app), `<app dir>/tide-data`, `$HOME/tide-data`; the
session is copied to the other writable locations and re-created there on the next successful start.
Logout deletes every copy. `tide.log` (same folder as the data dir, previous run in `tide.log.old`) lists
which copies were found/saved and why a restore failed.

The flow matters: a PKCE token must be refreshed with the PKCE client and a device-code token with the
other one. A file written before PKCE existed has no `flow` and is read as `device`.

## Risks

* **The paste step is manual.** The code sits in the phone's address bar and has to be copied before the
  handoff expires (300 s); the TV takes it on the next poll (1.5 s). If the service's page swallows the
  address (a deep link into the app that replaces the URL), the flow cannot finish and the fallback is
  offered. Nothing is served from the TV any more, so AP isolation, a different network or a firewall no
  longer matter; what has to be reachable is the relay.
* **The relay is in the path of the code** (never of the verifier) for up to 60 s, once per sign-in. It
  is a stateless Worker over a Durable Object, and it deletes the record as the TV picks the code up.
* The handoff id is not a secret in any meaningful sense (it is the URL of the paste page), but it is
  random, single-use and expires: it only says where a code should be dropped.
