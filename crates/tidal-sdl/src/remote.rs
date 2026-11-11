//! Remote-control keys that the `sdl2` crate cannot decode.
//!
//! SDL-webOS adds scancodes outside upstream's enum (e.g. Back = 482). The `sdl2` crate turns
//! unknown scancodes into `None`, so those key events would be invisible to `poll_iter`.
//! An SDL *event watch* sees the raw event instead. This relies on the SDL being built with
//! upstream's struct layout (`SDL_WEBOS_BROKEN_ABI=OFF`), which is why the TV build bundles its own SDL.
//! Values below are from webosbrew/SDL-webOS `include/SDL_scancode.h`.

use std::{
    os::raw::{c_int, c_void},
    sync::{
        atomic::{AtomicI32, Ordering},
        Mutex,
    },
};
use tidal_core::input::Key;

const WEBOS_HOME: i32 = 384;
const WEBOS_BACK: i32 = 482;
const WEBOS_RED: i32 = 486;
const WEBOS_GREEN: i32 = 487;
const WEBOS_EXIT: i32 = 505;
// Upstream scancodes whose keycodes the `sdl2` crate does not know.
const AUDIO_REWIND: i32 = 285;
const AUDIO_FAST_FORWARD: i32 = 286;

/// Last key-down scancode seen (diagnostics: shown by the HUD, written to the log).
pub static LAST_SCANCODE: AtomicI32 = AtomicI32::new(-1);
static PENDING: Mutex<Vec<Key>> = Mutex::new(Vec::new());
static HUD_TOGGLES: AtomicI32 = AtomicI32::new(0);

/// Number of times the red button was pressed since the last call (toggles the HUD).
pub fn take_hud_toggles() -> i32 {
    HUD_TOGGLES.swap(0, Ordering::Relaxed)
}

fn map_webos(scancode: i32) -> Option<Key> {
    match scancode {
        WEBOS_BACK => Some(Key::Back),
        // Green opens Settings on the audio-quality field; red toggles the HUD (see `watch`).
        WEBOS_GREEN => Some(Key::Quality),
        AUDIO_REWIND => Some(Key::Left),
        AUDIO_FAST_FORWARD => Some(Key::Right),
        // Home/Exit are handled by the system (launcher); do not act on them.
        WEBOS_HOME | WEBOS_EXIT => None,
        _ => None,
    }
}

unsafe extern "C" fn watch(_userdata: *mut c_void, event: *mut sdl2::sys::SDL_Event) -> c_int {
    let e = &*event;
    if e.type_ == sdl2::sys::SDL_EventType::SDL_KEYDOWN as u32 {
        let sc = e.key.keysym.scancode as i32;
        LAST_SCANCODE.store(sc, Ordering::Relaxed);
        if sc == WEBOS_RED {
            HUD_TOGGLES.fetch_add(1, Ordering::Relaxed);
        }
        // Holding Back must not repeat: it would walk up through every screen.
        if e.key.repeat == 0 {
            if let Some(k) = map_webos(sc) {
                if let Ok(mut p) = PENDING.lock() {
                    p.push(k);
                }
            }
        }
    }
    0
}

pub fn install() {
    unsafe {
        sdl2::sys::SDL_AddEventWatch(Some(watch), std::ptr::null_mut());
    }
}

/// Keys captured by the watch since the last call.
pub fn drain() -> Vec<Key> {
    PENDING.lock().map(|mut p| std::mem::take(&mut *p)).unwrap_or_default()
}
