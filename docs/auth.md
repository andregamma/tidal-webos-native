# Authentication

Sign-in is the OAuth device-code flow, done by the `tidlers` crate (`tide-api::tidlers_backend`).

* **Credentials**: none are configured by this app. `TidalAuth::with_oauth()` uses the library's own default
  client credentials, so there is no `config.json`, no environment variable and no secret in this repository.
  (They are whatever the library version pinned in `Cargo.toml` embeds; if the service rejects them, update or
  replace the library. Using them is the project's decision and subject to the service's terms.)
* **Flow**: `Boot` -> read the saved refresh token -> one forced refresh -> `LoggedIn`. Service unreachable
  (network not up right after power-on, timeout, 5xx) -> retried for ~26 s; if still failing the token is
  **kept**, the screen says to check the network, and OK retries the saved token (no new code).
  Rejected (400/401/403) -> `LoginRequired` -> `StartLogin`: QR + code on screen, library polls until approved
  (a new code is issued automatically when one expires, up to 6 times) -> token saved -> `LoggedIn`.
* **Stored data**: only the refresh token (the service does not rotate it on refresh), in
  `tidlers-session.json` (mode 0600, written with fsync + atomic rename, read back to verify).
  Desktop: `$TIDE_DATA_DIR`, else `$XDG_DATA_HOME/tide-tv`, else `~/.local/share/tide-tv`.
  TV: first writable of `/media/developer/tide-tv` (survives reinstalling the app), `<app dir>/tide-data`,
  `$HOME/tide-data`; the session is also copied to the other writable locations and re-created
  there on the next successful start. Logout deletes every copy. `tide.log` (same folder as the
  data dir, previous run in `tide.log.old`) lists which copies were found/saved and why a restore failed.
* **Known library limits**: `slow_down` is treated as fatal by `wait_for_oauth` (press OK to retry);
  scope is fixed to `r_usr w_usr w_sub`.
* Without the `tidlers` feature only the mock backend exists (no real sign-in).
