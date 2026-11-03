//! Desktop simulator / platform layer.
//!
//! This is the *only* crate that knows about SDL2. The TV build replaces the
//! window/GL-context/input parts of this file; `tide-core` and `tide-gfx`
//! stay untouched. Keyboard emulates the remote:
//!   arrows = D-pad, Enter = OK, Esc/Backspace = Back, Space/P = Play/Pause,
//!   N/B = Next/Prev, F3 = fps HUD.

use std::{
    env,
    fs::{self, File},
    io::BufWriter,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime},
};

use sdl2::{
    event::{Event, WindowEvent},
    keyboard::Keycode,
    video::{GLProfile, SwapInterval},
};
use tide_core::{
    app::{App, Start},
    draw::DrawList,
    input::Key,
    theme::Theme,
    H, W,
};
use tide_gfx::Renderer;
use tide_core::services::{channels, PlayerCmd, StreamSource, StreamSpec};

mod audio;
mod log;
mod remote;

/// Embedded so the TV package does not depend on a runtime asset path.
static FONT: &[u8] = include_bytes!("../../../assets/fonts/DejaVuSans.ttf");

struct Opts {
    start: Start,
    size: Option<(u32, u32)>,
    screenshot: Option<PathBuf>,
    frames: Option<u32>,
    keys: Vec<Key>,
    theme: PathBuf,
    hud: bool,
    vsync: bool,
    backend: String,
    /// Debug: play this URL directly, without any service (e.g. a local FLAC over http).
    play_url: Option<String>,
    mime: String,
    duration: f32,
}

fn parse_args() -> Opts {
    let mut o = Opts {
        start: Start::Login,
        size: None,
        screenshot: None,
        frames: None,
        keys: vec![],
        theme: PathBuf::from("assets/theme.json"),
        hud: false,
        vsync: true,
        backend: std::env::var("TIDE_BACKEND").unwrap_or_else(|_| {
            // The TV build has no command line: use the real backend when it is compiled in.
            if cfg!(all(target_arch = "arm", feature = "tidlers")) { "tidlers".into() } else { "mock".into() }
        }),
        play_url: None,
        mime: String::new(),
        duration: 600.0,
    };
    for a in env::args().skip(1) {
        let (k, v) = a.split_once('=').unwrap_or((a.as_str(), ""));
        match k {
            "--screen" => {
                o.start = match v {
                    "home" => Start::Home,
                    "liked" => Start::Liked,
                    "settings" => Start::Settings,
                    _ => Start::Login,
                }
            }
            "--size" => {
                if let Some((w, h)) = v.split_once('x') {
                    o.size = w.parse().ok().zip(h.parse().ok());
                }
            }
            "--screenshot" => o.screenshot = Some(PathBuf::from(v)),
            "--frames" => o.frames = v.parse().ok(),
            "--keys" => o.keys = v.split(',').filter_map(Key::parse).collect(),
            "--theme" => o.theme = PathBuf::from(v),
            "--hud" => o.hud = true,
            "--no-vsync" => o.vsync = false,
            "--backend" => o.backend = v.to_string(),
            "--play-url" => o.play_url = Some(v.to_string()),
            "--mime" => o.mime = v.to_string(),
            "--duration" => o.duration = v.parse().unwrap_or(600.0),
            "--help" | "-h" => {
                println!(
                    "tide-tv [--screen=login|home|liked|settings] [--size=WxH] [--backend=mock|tidlers] [--play-url=URL --mime=audio/flac] [--theme=path] [--hud] [--no-vsync]\n        \
                     [--screenshot=out.png [--frames=N] [--keys=down,right,ok,...]]"
                );
                std::process::exit(0);
            }
            _ => eprintln!("ignoring unknown argument {a}"),
        }
    }
    o
}

/// Single place that maps hardware keys to remote-control keys.
/// TODO(TV): verify what the webOS SDL build reports for the remote's buttons.
fn map_key(k: Keycode) -> Option<Key> {
    Some(match k {
        Keycode::Up => Key::Up,
        Keycode::Down => Key::Down,
        Keycode::Left => Key::Left,
        Keycode::Right => Key::Right,
        Keycode::Return | Keycode::KpEnter => Key::Ok,
        Keycode::Escape | Keycode::Backspace => Key::Back,
        Keycode::Space | Keycode::P => Key::PlayPause,
        Keycode::N | Keycode::AudioNext => Key::Next,
        Keycode::B | Keycode::AudioPrev => Key::Prev,
        // Remote media buttons (webOS maps Play/Stop/Rew/FF to these).
        Keycode::AudioPlay => Key::PlayPause,
        Keycode::AudioStop => Key::Back,
        _ => return None,
    })
}

fn mtime(p: &Path) -> Option<SystemTime> {
    fs::metadata(p).and_then(|m| m.modified()).ok()
}

fn save_png(path: &Path, rgba: &[u8], w: u32, h: u32) -> Result<(), Box<dyn std::error::Error>> {
    let mut enc = png::Encoder::new(BufWriter::new(File::create(path)?), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(rgba)?;
    Ok(())
}

/// True when `dir` exists (or can be created) and a file can be written in it.
fn writable_dir(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".write-test");
    let ok = std::fs::write(&probe, b"ok").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opts = parse_args();

    // On the TV the app directory is the only place we know is writable; elsewhere keep the
    // XDG-style default so existing config files keep working.
    let on_tv = cfg!(target_arch = "arm");
    if on_tv && std::env::var_os("TIDE_DATA_DIR").is_none() {
        // Prefer /media/developer (survives reinstalling the app); take the first directory we can
        // really write a file in, not just create.
        let mut candidates = vec![std::path::PathBuf::from("/media/developer/tide-tv")];
        if let Some(d) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("tide-data"))) {
            candidates.push(d);
        }
        candidates.push(std::path::Path::new(&std::env::var_os("HOME").unwrap_or_default()).join("tide-data"));
        if let Some(dir) = candidates.iter().find(|d| writable_dir(d)) {
            std::env::set_var("TIDE_DATA_DIR", dir);
        }
    }
    let log_dir = tide_api::paths::data_dir();
    log::init(&log_dir);
    tide_api::diag::set_sink(log::line);
    logf!("tide-tv {} starting (tv build: {on_tv}, data dir: {})", env!("CARGO_PKG_VERSION"), log_dir.display());
    // Saved app settings (language). The backend reads the same file on its own thread; the core does
    // no I/O, so it is handed the value here.
    let saved = tide_api::settings::Settings::load();
    logf!("language: {}", saved.locale);
    let shot = opts.screenshot.is_some();
    let (win_w, win_h) = opts.size.unwrap_or(if shot { (1920, 1080) } else { (1280, 720) });

    let sdl = sdl2::init()?;
    let video = sdl.video()?;
    remote::install();
    logf!("video driver: {}", video.current_video_driver());
    {
        // GLES 2.0 is the lowest common denominator across LG TV generations.
        let a = video.gl_attr();
        a.set_context_profile(GLProfile::GLES);
        a.set_context_version(2, 0);
        a.set_double_buffer(true);
    }
    let window = video.window("tide-tv", win_w, win_h).opengl().position_centered().resizable().build()?;
    let _ctx = window.gl_create_context()?;
    window.gl_make_current(&_ctx)?;
    let _ = video.gl_set_swap_interval(if opts.vsync && !shot { SwapInterval::VSync } else { SwapInterval::Immediate });

    let gl = unsafe { glow::Context::from_loader_function(|s| video.gl_get_proc_address(s) as *const _) };
    let mut renderer = Renderer::new(gl, FONT, (W, H))?;
    let (pw, ph) = window.drawable_size();
    renderer.set_drawable_size(pw, ph);

    let theme = Theme::load(&opts.theme).unwrap_or_else(|e| {
        logf!("theme {}: {e} (using defaults)", opts.theme.display());
        Theme::default()
    });
    let mut theme_stamp = mtime(&opts.theme);

    let ch = channels();
    let player_tx = ch.app.player.clone();
    let port = tide_player::PcmPort::new();
    tide_player::spawn(ch.player, port.clone());
    let mut audio = audio::AudioOut::new(&sdl, port);
    match opts.backend.as_str() {
        #[cfg(feature = "tidlers")]
        "tidlers" => tide_api::spawn_tidlers(ch.backend),
        #[cfg(not(feature = "tidlers"))]
        "tidlers" => return Err("rebuild with `--features tidlers` to use this backend".into()),
        _ => tide_api::spawn_mock(ch.backend),
    }
    if let Some(url) = &opts.play_url {
        let _ = player_tx.send(PlayerCmd::Load(StreamSpec {
            track_id: 0,
            mime: opts.mime.clone(),
            duration: opts.duration,
            source: StreamSource::Single(url.clone()),
            tier: None,
        }));
    }
    let mut app = App::new(ch.app, theme, opts.start).with_locale(&saved.locale);
    let mut list = DrawList::new();
    let loader = tide_img::Loader::new(2);
    let mut events = sdl.event_pump()?;

    // On the TV the HUD (fps + last key code) starts on; the red button toggles it.
    let mut hud = opts.hud || on_tv;
    let (mut last, mut fps_t, mut fps_n, mut fps) = (Instant::now(), Instant::now(), 0u32, 0.0f32);
    let mut frame = 0u32;
    let mut theme_poll = 0.0f32;
    let key_start = 40u32; // frames to wait for mock data before scripted keys
    let total_frames = opts.frames.unwrap_or(key_start + opts.keys.len() as u32 * 10 + 70);

    'main: loop {
        let mut keys: Vec<Key> = Vec::new();
        for e in events.poll_iter() {
            match e {
                Event::Quit { .. } => break 'main,
                Event::Window { win_event: WindowEvent::SizeChanged(..), .. } => {
                    let (w, h) = window.drawable_size();
                    renderer.set_drawable_size(w, h);
                }
                Event::KeyDown { keycode: Some(Keycode::F3), repeat: false, .. } => hud = !hud,
                Event::KeyDown { keycode: Some(k), repeat, .. } => {
                    if let Some(key) = map_key(k) {
                        // Auto-repeat helps scrolling but is harmful for Back/OK/Play.
                        let repeatable = matches!(key, Key::Up | Key::Down | Key::Left | Key::Right);
                        if !repeat || repeatable {
                            keys.push(key);
                        }
                    }
                }
                _ => {}
            }
        }

        keys.extend(remote::drain());
        if keys.contains(&Key::Back) {
            logf!("input: {keys:?}");
        }
        if remote::take_hud_toggles() % 2 == 1 {
            hud = !hud;
        }

        let now = Instant::now();
        let dt = if shot { 1.0 / 60.0 } else { (now - last).as_secs_f32().min(0.05) };
        last = now;

        if shot && frame >= key_start && (frame - key_start) % 10 == 0 {
            if let Some(k) = opts.keys.get(((frame - key_start) / 10) as usize) {
                keys.push(*k);
            }
        }

        // Live theme reload.
        theme_poll += dt;
        if theme_poll > 0.5 {
            theme_poll = 0.0;
            let stamp = mtime(&opts.theme);
            if stamp != theme_stamp {
                theme_stamp = stamp;
                match Theme::load(&opts.theme) {
                    Ok(t) => {
                        app.theme = t;
                        logf!("theme reloaded");
                    }
                    Err(e) => logf!("theme error: {e}"),
                }
            }
        }

        // Hand decoded covers to the GPU (a couple per frame keeps frames smooth).
        for _ in 0..2 {
            match loader.poll() {
                Some(tide_img::Loaded::Ok { key, w, h, rgb }) => renderer.upload_image(key, &rgb, w, h),
                Some(tide_img::Loaded::Failed { key, why }) => {
                    logf!("image {key:#x} failed: {why}");
                    renderer.mark_failed(key);
                }
                None => break,
            }
        }

        audio.poll();
        app.update(dt, &keys);
        if app.should_quit() && !shot {
            logf!("quit requested by the app (Back on the root screen)");
            break;
        }

        list.clear();
        app.view(&mut list, &renderer, &renderer);
        for (key, url) in list.requests.drain(..) {
            if renderer.mark_pending(key) {
                loader.request(key, url);
            }
        }
        if hud {
            let t = &app.theme;
            let sc = remote::LAST_SCANCODE.load(std::sync::atomic::Ordering::Relaxed);
            list.text(W - 470.0, 16.0, 22.0, t.accent, &format!("{fps:.0} fps  {} calls  key {sc}", renderer.draw_calls));
        }
        renderer.render(&list, app.theme.bg);

        frame += 1;
        if shot {
            if frame >= total_frames {
                let (px, w, h) = renderer.read_rgba();
                let path = opts.screenshot.as_ref().unwrap();
                save_png(path, &px, w, h)?;
                println!("saved {} ({}x{}, {} draw calls)", path.display(), w, h, renderer.draw_calls);
                break;
            }
            thread::sleep(Duration::from_millis(16)); // keep real time for the mock backend
        }
        window.gl_swap_window();

        fps_n += 1;
        if fps_t.elapsed() >= Duration::from_secs(1) {
            fps = fps_n as f32 / fps_t.elapsed().as_secs_f32();
            if opts.hud || hud {
                println!("{fps:.1} fps");
            }
            fps_n = 0;
            fps_t = Instant::now();
        }
    }
    Ok(())
}
