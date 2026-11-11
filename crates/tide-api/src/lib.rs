//! Backend for `tide-core`'s `Cmd`/`Evt` protocol.
//!
//! * [`settings`] – the app's own persistent settings (language, quality).
//! * `tidlers_backend` – the only backend, built on the `tidlers` crate: sign-in, favourites,
//!   Home, lyrics, stream URLs. It uses the library's own client credentials unless
//!   `TIDAL_CLIENT_ID`/`TIDAL_CLIENT_SECRET` are set.
//!
//! There is no mock backend: the app always talks to the real service.

pub mod diag;
pub mod paths;
pub mod settings;
pub mod tidlers_backend;

use std::thread;
use tide_core::services::BackendSide;

fn spawn_thread(name: &str, f: impl FnOnce() + Send + 'static) {
    thread::Builder::new().name(name.into()).spawn(f).expect("spawn backend thread");
}

pub fn spawn(be: BackendSide) {
    spawn_thread("backend-tidlers", move || tidlers_backend::run(be));
}
