# Authentication

Sign-in is the OAuth device-code flow, done by the `tidlers` crate (`tide-api::tidlers_backend`).

* **Credentials**: none are configured by this app. `TidalAuth::with_oauth()` uses the library's own default
  client credentials, so there is no `config.json`, no environment variable and no secret in this repository.
  (They are whatever the library version pinned in `Cargo.toml` embeds; if the service rejects them, update or
  replace the library. Using them is the project's decision and subject to the service's terms.)
* **Flow**: `Boot` -> read the saved refresh token -> one forced refresh -> `LoggedIn`; rejected (400/401/403)
  -> token deleted -> `LoginRequired` -> `StartLogin`: QR + code on screen, library polls until approved
  (a new code is issued automatically when one expires, up to 6 times) -> token saved -> `LoggedIn`.
* **Stored data**: only the refresh token, in `<data dir>/tidlers-session.json` (mode 0600 on unix).
  Delete the file to force a new sign-in. Data dir: `$TIDE_DATA_DIR`, else `$XDG_DATA_HOME/tide-tv`,
  else `~/.local/share/tide-tv`; on the TV: `<app dir>/tide-data`.
* **Known library limits**: `slow_down` is treated as fatal by `wait_for_oauth` (press OK to retry);
  scope is fixed to `r_usr w_usr w_sub`.
* Without the `tidlers` feature only the mock backend exists (no real sign-in).
