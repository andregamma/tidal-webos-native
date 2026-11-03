//! Tiny file logger: on the TV stderr goes nowhere, so everything useful is also written to
//! `<data dir>/tide.log` (previous run kept as `tide.log.old`).

use std::{
    fs::{self, File},
    io::Write,
    path::Path,
    sync::{Mutex, OnceLock},
    time::Instant,
};

static FILE: OnceLock<Mutex<File>> = OnceLock::new();
static START: OnceLock<Instant> = OnceLock::new();

pub fn init(dir: &Path) {
    START.get_or_init(Instant::now);
    let _ = fs::create_dir_all(dir);
    let path = dir.join("tide.log");
    let _ = fs::rename(&path, dir.join("tide.log.old"));
    if let Ok(f) = File::create(&path) {
        let _ = FILE.set(Mutex::new(f));
    }
    std::panic::set_hook(Box::new(|info| {
        line(&format!("PANIC: {info}"));
    }));
}

pub fn line(msg: &str) {
    let t = START.get().map_or(0.0, |s| s.elapsed().as_secs_f32());
    let text = format!("[{t:8.3}] {msg}");
    eprintln!("{text}");
    if let Some(f) = FILE.get() {
        if let Ok(mut f) = f.lock() {
            let _ = writeln!(f, "{text}");
            let _ = f.flush();
        }
    }
}

#[macro_export]
macro_rules! logf {
    ($($arg:tt)*) => { $crate::log::line(&format!($($arg)*)) };
}
