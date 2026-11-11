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
    io::{MediaSource, MediaSourceStream},
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
    /// The user asked for a pause. Separate from the mute applied while a seek is in flight,
    /// which must not outlive the seek when the user was playing.
    paused_by_user: AtomicBool,
    /// Whether the open stream can be seeked in place (one file). A segmented stream cannot:
    /// a seek there means reopening the download, so the player goes silent in the meantime.
    seekable: AtomicBool,
}

/// Start the player thread. `port` is shared with the platform audio layer.
pub fn spawn(side: PlayerSide, port: PcmPort) {
    thread::Builder::new().name("player".into()).spawn(move || run(side, port)).expect("spawn player thread");
}

fn run(side: PlayerSide, port: PcmPort) {
    let mut active: Option<Arc<Ctl>> = None;
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
            last = Instant::now();
            tick(&mut active, &port, &side.tx);
        }
    }
}

fn stop_active(active: &mut Option<Arc<Ctl>>) {
    if let Some(ctl) = active.take() {
        ctl.stop.store(true, Ordering::SeqCst);
    }
}

/// Undo the mute applied for a seek, unless the user paused in the meantime.
fn unmute_if_playing(ctl: &Ctl, port: &PcmPort) {
    if !ctl.paused_by_user.load(Ordering::SeqCst) {
        port.set_paused(false);
    }
}

/// A newer seek landed while the previous one was being opened. Confirming the old one would move
/// the bar back to it for a moment, so the next restart reports where playback really resumed.
fn superseded(ctl: &Ctl) -> bool {
    ctl.seek.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

fn handle(cmd: PlayerCmd, active: &mut Option<Arc<Ctl>>, port: &PcmPort, tx: &Sender<Evt>) {
    match cmd {
        PlayerCmd::Load(spec) => {
            stop_active(active);
            port.flush(0.0);
            port.set_paused(false);
            let ctl = Arc::new(Ctl::default());
            let (c, p, t) = (ctl.clone(), port.clone(), tx.clone());
            thread::Builder::new().name("decode".into()).spawn(move || session(spec, c, p, t)).ok();
            *active = Some(ctl);
        }
        PlayerCmd::Pause | PlayerCmd::Resume => {
            let pause = matches!(cmd, PlayerCmd::Pause);
            if let Some(ctl) = active {
                ctl.paused_by_user.store(pause, Ordering::SeqCst);
                port.set_paused(pause);
            }
        }
        PlayerCmd::Seek(s) => {
            if let Some(ctl) = active {
                let t = s.max(0.0) as f64;
                *ctl.seek.lock().unwrap_or_else(|e| e.into_inner()) = Some(t);
                if !ctl.seekable.load(Ordering::SeqCst) {
                    // A DASH seek means reopening the download, which takes a moment: go silent and
                    // drop what is queued now, so the old position does not keep playing (the flush
                    // also wakes the decoder, which is otherwise blocked pushing into a full queue).
                    port.flush(t);
                    port.set_paused(true);
                }
            }
        }
        PlayerCmd::Stop => {
            stop_active(active);
            port.flush(0.0);
            port.set_paused(false);
        }
    }
}

fn tick(active: &mut Option<Arc<Ctl>>, port: &PcmPort, tx: &Sender<Evt>) {
    let ended = match active {
        Some(ctl) => {
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
    // A DASH stream cannot be seeked in place, so a seek restarts the download at the segment
    // that holds that time and rebuilds the demuxer from the init segment.
    let mut current = spec.clone();
    let mut start_at = 0.0f64;
    loop {
        match play_stream(&current, &ctl, &port, &tx, start_at) {
            Ok(None) => break,
            Ok(Some(t)) => match seek_spec(&spec, t) {
                Some((next, at)) => {
                    current = next;
                    start_at = at;
                }
                // Nothing to seek: drop the request and keep playing from where it is.
                None => {}
            },
            Err(e) => {
                if !ctl.stop.load(Ordering::SeqCst) {
                    let _ = tx.send(Evt::PlaybackError(e));
                }
                break;
            }
        }
    }
    ctl.done.store(true, Ordering::SeqCst);
}

/// Where playback resumes: the segment that contains `t`, plus the time that segment begins at
/// (a seek can only land on a segment boundary). `None` when the stream is a single file.
fn seek_spec(spec: &StreamSpec, t: f64) -> Option<(StreamSpec, f64)> {
    let StreamSource::Segmented { init, template, start_number, segment_secs } = &spec.source else {
        return None;
    };
    let secs = f64::from(*segment_secs);
    if secs <= 0.0 {
        return None;
    }
    let step = (t.max(0.0) / secs).floor() as u32;
    let mut next = spec.clone();
    next.source = StreamSource::Segmented {
        init: init.clone(),
        template: template.clone(),
        start_number: start_number.saturating_add(step),
        segment_secs: *segment_secs,
    };
    Some((next, f64::from(step) * secs))
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

/// What the service said it was sending, for messages that name the problem.
fn stream_label(spec: &StreamSpec) -> String {
    let mime = if spec.mime.is_empty() { "no mime" } else { spec.mime.as_str() };
    match &spec.tier {
        Some(t) if !t.is_empty() => format!("{mime}, {t}"),
        _ => mime.to_string(),
    }
}

fn play_stream(
    spec: &StreamSpec,
    ctl: &Ctl,
    port: &PcmPort,
    tx: &Sender<Evt>,
    start_at: f64,
) -> Result<Option<f64>, String> {
    let src = ProgressiveSource::open(spec)?;
    // A segmented download has no length while it runs, so it cannot be seeked: say so before the
    // source is moved into the stream and the demuxer reads it once.
    let seekable = src.is_seekable();
    ctl.seekable.store(seekable, Ordering::SeqCst);
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
        .map_err(|e| format!("Unsupported or unreadable stream ({}, {e})", stream_label(spec)))?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .ok_or_else(|| format!("The stream has no audio track ({})", stream_label(spec)))?
        .clone();
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("No decoder for this stream ({}, {e})", stream_label(spec)))?;

    let rate = track.codec_params.sample_rate.ok_or("Unknown sample rate")?;
    let src_ch = track.codec_params.channels.map_or(2, |c| c.count());
    let out_ch = src_ch.clamp(1, 2); // more than 2 channels: first two only (no real downmix yet)
    port.configure(DeviceSpec { rate, channels: out_ch as u8 })?;
    if start_at > 0.0 {
        port.flush(start_at);
    }
    let mut gen = port.generation();
    // The player is muted while a seek is in flight (see `handle`); playback continues from here.
    if !superseded(ctl) {
        if start_at > 0.0 {
            let _ = tx.send(Evt::Seeked(start_at as f32));
        }
        unmute_if_playing(ctl, port);
    }
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
            return Ok(None);
        }
        let wanted = ctl.seek.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(t) = wanted {
            if !seekable {
                // Handing this to `isomp4` leaves it pointing at samples it cannot reach again
                // ("packet out-of-bounds for a non-seekable stream"), so restart instead.
                return Ok(Some(t));
            }
            let to = SeekTo::Time { time: Time::from(t), track_id: Some(track.id) };
            match format.seek(SeekMode::Accurate, to) {
                Ok(done) => {
                    decoder.reset();
                    let base = time_base.map_or(t, |tb| secs_of(tb.calc_time(done.actual_ts)));
                    gen = port.flush(base);
                    if !superseded(ctl) {
                        let _ = tx.send(Evt::Seeked(base as f32));
                        unmute_if_playing(ctl, port);
                    }
                }
                // The demuxer refused: nothing moved, so tell the bar where playback still is.
                Err(_) => {
                    if !superseded(ctl) {
                        let _ = tx.send(Evt::Seeked(port.position_secs() as f32));
                        unmute_if_playing(ctl, port);
                    }
                }
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
            return Ok(None);
        }
    }

    if port.wait_drained(gen) && !ctl.stop.load(Ordering::SeqCst) {
        let _ = tx.send(Evt::TrackEnded);
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dash_spec(start_number: u32) -> StreamSpec {
        StreamSpec {
            track_id: 7,
            mime: "audio/mp4".into(),
            duration: 240.0,
            source: StreamSource::Segmented {
                init: "https://cdn/x/init.mp4?token=t".into(),
                template: "https://cdn/x/1-$Number$.mp4?token=t".into(),
                start_number,
                segment_secs: 4.0,
            },
            tier: Some("LOSSLESS".into()),
        }
    }

    /// A DASH stream cannot be seeked in place, so the seek becomes "restart at the segment that
    /// holds that time". Segment 100..104s is number 1 + 25 = 26 and begins at 100s.
    #[test]
    fn seeking_a_dash_stream_restarts_at_the_segment_that_holds_the_time() {
        let (next, at) = seek_spec(&dash_spec(1), 103.9).expect("segmented stream");
        assert_eq!(at, 100.0);
        match next.source {
            StreamSource::Segmented { start_number, .. } => assert_eq!(start_number, 26),
            other => panic!("expected segments, got {other:?}"),
        }
    }

    /// A single file is seeked in place by the demuxer; there is nothing to restart.
    #[test]
    fn a_single_file_stream_has_nothing_to_restart() {
        let mut spec = dash_spec(1);
        spec.source = StreamSource::Single("https://cdn/x/track.flac".into());
        assert!(seek_spec(&spec, 100.0).is_none());
    }

    /// A DASH seek reopens the download, which takes a moment: the queued audio is from the wrong
    /// place and the position reports would still describe it, so both are dropped right away.
    #[test]
    fn a_seek_silences_the_old_position_at_once() {
        let port = PcmPort::new();
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut active = Some(Arc::new(Ctl::default()));
        handle(PlayerCmd::Seek(42.0), &mut active, &port, &tx);
        assert_eq!(port.position_secs(), 42.0);
        assert!(port.is_paused());
        handle(PlayerCmd::Resume, &mut active, &port, &tx);
        assert!(!port.is_paused());
    }

    /// Pressing right again while the previous seek is still opening must not be confirmed away.
    #[test]
    fn a_newer_seek_supersedes_the_one_being_opened() {
        let ctl = Ctl::default();
        assert!(!superseded(&ctl));
        *ctl.seek.lock().unwrap() = Some(30.0);
        assert!(superseded(&ctl));
    }

    /// The mute that covers a seek must not override a pause the user asked for.
    #[test]
    fn the_seek_mute_lifts_only_when_the_user_is_playing() {
        let port = PcmPort::new();
        let ctl = Ctl::default();
        port.set_paused(true);
        unmute_if_playing(&ctl, &port);
        assert!(!port.is_paused());
        port.set_paused(true);
        ctl.paused_by_user.store(true, std::sync::atomic::Ordering::SeqCst);
        unmute_if_playing(&ctl, &port);
        assert!(port.is_paused());
    }
}
