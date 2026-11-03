//! Backends for `tide-core`'s `Cmd`/`Evt` protocol.
//!
//! * [`mock`] – fake data with artificial latency; playback is simulated.
//! * [`settings`] – the app's own persistent settings (language); shared by every backend.
//! * `tidlers_backend` – the real service, built on the `tidlers` crate (feature `tidlers`):
//!   sign-in, favourites, lyrics, stream URLs. It uses the library's own client credentials.
//!
//! All speak the same protocol, so the UI never changes.

pub mod mock;
pub mod diag;
pub mod paths;
pub mod settings;
#[cfg(feature = "tidlers")]
pub mod tidlers_backend;

use std::thread;
use tide_core::services::BackendSide;

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    thread::Builder::new().name(name.into()).spawn(f).expect("spawn backend thread");
}

pub fn spawn_mock(be: BackendSide) {
    spawn("backend-mock", move || mock::run(be));
}

#[cfg(feature = "tidlers")]
pub fn spawn_tidlers(be: BackendSide) {
    spawn("backend-tidlers", move || tidlers_backend::run(be));
}
