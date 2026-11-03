use crate::{pcm::{DeviceSpec, PcmPort}, source::ProgressiveSource};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{RecvTimeoutError, Sender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};
use symphonia::core::{
    audio::SampleBuffer,
    codecs::DecoderOptions,
    errors::Error as SymError,
    formats::{FormatOptions, SeekMode, SeekTo},
    io::MediaSourceStream,
    meta::MetadataOptions,
    probe::Hint,
    units::Time,
};
use tide_core::services::{Evt, PlayerCmd, PlayerSide, StreamInfo, StreamSource, StreamSpec};

const REPORT_EVERY: Duration = Duration::from_millis(250);

/// Control block shared with one decode session thread.
#[derive(Default)]
struct Ctl {
    stop: AtomicBool,
    done: AtomicBool,
    seek: Mutex<Option<f64>>,
}

enum Active {
    /// No audio: a fake clock for the mock backend.
    Sim { pos: f32, dur: f32, paused: bool },
    Real(Arc<Ctl>),
}

/// Start the player thread. `port` is shared with the platform audio layer.
pub fn spawn(side: PlayerSide, port: PcmPort) {
    thread::Builder::new().name("player".into()).spawn(move || run(side, port)).expect("spawn player thread");
}

fn run(side: PlayerSide, port: PcmPort) {
    let mut active: Option<Active> = None;
    let mut last = Instant::now();
    loop {
        match side.rx.recv_timeout(Duration::from_millis(100)) {
            Ok(cmd) => handle(cmd, &mut active, &port, &side.tx),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                stop_active(&mut active);
                return;
            }
        }
        if last.elapsed() >= REPORT_EVERY {
            let dt = last.elapsed().as_secs_f32();
            last = Instant::now();
            tick(dt, &mut active, &port, &side.tx);
        }
    }
}

fn stop_active(active: &mut Option<Active>) {
    if let Some(Active::Real(ctl)) = active.take() {
        ctl.stop.store(true, Ordering::SeqCst);
    }
}

fn handle(cmd: PlayerCmd, active: &mut Option<Active>, port: &PcmPort, tx: &Sender<Evt>) {
    match cmd {
        PlayerCmd::Load(spec) => {
            stop_active(active);
            port.flush(0.0);
            port.set_paused(false);
            *active = Some(match spec.source {
                StreamSource::Simulated => {
                    // Fake details so the quality indicator can be seen without real audio.
                    let _ = tx.send(Evt::StreamInfo(StreamInfo {
                        codec: "FLAC".into(),
                        src_rate: 44_100,
                        src_bits: Some(16),
                        src_channels: 2,
                        dev_rate: 44_100,
                        dev_channels: 2,
                        driver: "simulated".into(),
                        tier: Some("LOSSLESS".into()),
                    }));
                    Active::Sim { pos: 0.0, dur: spec.duration, paused: false }
                }
                _ => {
                    let ctl = Arc::new(Ctl::default());
                    let (c, p, t) = (ctl.clone(), port.clone(), tx.clone());
                    thread::Builder::new().name("decode".into()).spawn(move || session(spec, c, p, t)).ok();
                    Active::Real(ctl)
                }
            });
        }
        PlayerCmd::Pause | PlayerCmd::Resume => {
            let pause = matches!(cmd, PlayerCmd::Pause);
            match active {
                Some(Active::Sim { paused, .. }) => *paused = pause,
                Some(Active::Real(_)) => port.set_paused(pause),
                None => {}
            }
        }
        PlayerCmd::Seek(s) => match active {
            Some(Active::Sim { pos, dur, .. }) => *pos = s.clamp(0.0, *dur),
            Some(Active::Real(ctl)) => *ctl.seek.lock().unwrap_or_else(|e| e.into_inner()) = Some(s.max(0.0) as f64),
            None => {}
        },
        PlayerCmd::Stop => {
            stop_active(active);
            port.flush(0.0);
            port.set_paused(false);
        }
    }
}

fn tick(dt: f32, active: &mut Option<Active>, port: &PcmPort, tx: &Sender<Evt>) {
    let ended = match active {
        Some(Active::Sim { pos, dur, paused }) => {
            if !*paused {
                *pos += dt;
            }
            if *pos >= *dur {
                let _ = tx.send(Evt::TrackEnded);
                true
            } else {
                let _ = tx.send(Evt::Position(*pos));
                false
            }
        }
        Some(Active::Real(ctl)) => {
            if ctl.done.load(Ordering::SeqCst) {
                true
            } else {
                let _ = tx.send(Evt::Position(port.position_secs() as f32));
                false
            }
        }
        None => false,
    };
    if ended {
        *active = None;
    }
}

fn session(spec: StreamSpec, ctl: Arc<Ctl>, port: PcmPort, tx: Sender<Evt>) {
    if let Err(e) = play_stream(&spec, &ctl, &port, &tx) {
        if !ctl.stop.load(Ordering::SeqCst) {
            let _ = tx.send(Evt::PlaybackError(e));
        }
    }
    ctl.done.store(true, Ordering::SeqCst);
}

fn ext_for(mime: &str) -> Option<&'static str> {
    match mime.to_ascii_lowercase().as_str() {
        "audio/flac" | "audio/x-flac" => Some("flac"),
        "audio/mp4" | "audio/m4a" | "audio/x-m4a" | "audio/aac" => Some("m4a"),
        "audio/mpeg" => Some("mp3"),
        "audio/wav" | "audio/x-wav" => Some("wav"),
        _ => None,
    }
}

fn secs_of(t: Time) -> f64 {
    t.seconds as f64 + t.frac
}

fn play_stream(spec: &StreamSpec, ctl: &Ctl, port: &PcmPort, tx: &Sender<Evt>) -> Result<(), String> {
    let src = ProgressiveSource::open(spec)?;
    let mss = MediaSourceStream::new(Box::new(src), Default::default());
    let mut hint = Hint::new();
    if !spec.mime.is_empty() {
        hint.mime_type(&spec.mime);
    }
    if let Some(ext) = ext_for(&spec.mime) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("Unsupported or unreadable stream: {e}"))?;
    let mut format = probed.format;
    let track = format.default_track().ok_or("The stream has no audio track")?.clone();
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("No decoder for this stream: {e}"))?;

    let rate = track.codec_params.sample_rate.ok_or("Unknown sample rate")?;
    let src_ch = track.codec_params.channels.map_or(2, |c| c.count());
    let out_ch = src_ch.clamp(1, 2); // more than 2 channels: first two only (no real downmix yet)
    port.configure(DeviceSpec { rate, channels: out_ch as u8 })?;
    let mut gen = port.generation();
    if let Some((dev, driver)) = port.device_info() {
        let codec = symphonia::default::get_codecs()
            .get_codec(track.codec_params.codec)
            .map_or_else(|| "?".to_string(), |d| d.short_name.to_uppercase());
        let _ = tx.send(Evt::StreamInfo(StreamInfo {
            codec,
            src_rate: rate,
            src_bits: track.codec_params.bits_per_sample,
            src_channels: src_ch as u8,
            dev_rate: dev.rate,
            dev_channels: dev.channels,
            driver,
            tier: spec.tier.clone(),
        }));
    }
    let time_base = track.codec_params.time_base;

    let mut sbuf: Option<SampleBuffer<f32>> = None;
    let mut tmp: Vec<f32> = Vec::new();
    loop {
        if ctl.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        let wanted = ctl.seek.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(t) = wanted {
            let to = SeekTo::Time { time: Time::from(t), track_id: Some(track.id) };
            if let Ok(done) = format.seek(SeekMode::Accurate, to) {
                decoder.reset();
                let base = time_base.map_or(t, |tb| secs_of(tb.calc_time(done.actual_ts)));
                gen = port.flush(base);
            }
        }

        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(SymError::IoError(e)) => return Err(e.to_string()),
            Err(e) => return Err(format!("Stream error: {e}")),
        };
        if packet.track_id() != track.id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymError::DecodeError(_)) => continue, // corrupt packet: skip it
            Err(e) => return Err(format!("Decode error: {e}")),
        };
        let sp = *decoded.spec();
        let in_ch = sp.channels.count();
        if sbuf.as_ref().is_none_or(|s| s.capacity() < decoded.capacity() * in_ch) {
            sbuf = Some(SampleBuffer::new(decoded.capacity() as u64, sp));
        }
        let Some(sb) = sbuf.as_mut() else { continue };
        sb.copy_interleaved_ref(decoded);
        let samples: &[f32] = if in_ch == out_ch {
            sb.samples()
        } else {
            tmp.clear();
            for frame in sb.samples().chunks_exact(in_ch) {
                tmp.extend_from_slice(&frame[..out_ch]);
            }
            &tmp
        };
        if !port.push(samples, gen) && ctl.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
    }

    if port.wait_drained(gen) && !ctl.stop.load(Ordering::SeqCst) {
        let _ = tx.send(Evt::TrackEnded);
    }
    Ok(())
}
