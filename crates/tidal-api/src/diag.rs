//! Diagnostics from the backend thread. The platform layer routes them to its log file.

use std::sync::OnceLock;

static SINK: OnceLock<fn(&str)> = OnceLock::new();

/// Route backend diagnostics to the app's logger (call once at startup).
pub fn set_sink(f: fn(&str)) {
    let _ = SINK.set(f);
}

pub fn log(msg: &str) {
    match SINK.get() {
        Some(f) => f(msg),
        None => eprintln!("{msg}"),
    }
}
