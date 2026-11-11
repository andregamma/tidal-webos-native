//! Progressive HTTP source: a background thread downloads into memory while the
//! decoder reads (and seeks) over what has arrived so far.
//!
//! Limits (POC): the whole stream is kept in RAM (a lossless track is tens of MB;
//! watch this on low-memory TVs) and seeking past the downloaded part waits for it.
//! TODO: HTTP Range requests + a bounded window/disk spool.

use std::{
    io::{self, Read, Seek, SeekFrom},
    sync::{Arc, Condvar, Mutex},
    thread,
    time::{Duration, Instant},
};
use symphonia::core::io::MediaSource;
use tide_core::services::{StreamSource, StreamSpec};

#[derive(Default)]
struct Buf {
    data: Vec<u8>,
    total: Option<u64>,
    /// HTTP status and content type of the stream request, for messages that name the problem.
    status: Option<u16>,
    content_type: Option<String>,
    done: bool,
    error: Option<String>,
    stop: bool,
}

/// Enough of the body to tell a container from a playlist or an error page.
const HEAD: usize = 16;
/// How long `open` waits for those bytes before letting the decoder wait instead.
const HEAD_WAIT: Duration = Duration::from_secs(20);

/// What the first bytes of the stream look like.
#[derive(Debug, PartialEq, Eq)]
enum Head {
    /// A container the probe can be handed: FLAC, MP4/CMAF, WAV, MP3, Ogg.
    Audio,
    /// An HLS playlist (a list of segment URLs). Not streamable here.
    Hls,
    /// Anything else: an error page, JSON, or a format nobody here handles.
    Other,
}

fn classify(head: &[u8]) -> Head {
    let h = head;
    if h.starts_with(b"fLaC") || h.starts_with(b"RIFF") || h.starts_with(b"ID3") || h.starts_with(b"OggS") {
        return Head::Audio;
    }
    // MP4/CMAF (also the DASH init segment): the `ftyp` box starts at offset 4.
    if h.len() >= 8 && &h[4..8] == b"ftyp" {
        return Head::Audio;
    }
    if h.starts_with(b"#EXTM3U") {
        return Head::Hls;
    }
    Head::Other
}

/// ` (HTTP 403, text/html)` when known; appended to a failure so the cause is not guessed.
fn describe_http(b: &Buf) -> String {
    let mut out = String::new();
    if let Some(s) = b.status {
        out.push_str(&format!("HTTP {s}"));
    }
    if let Some(ct) = &b.content_type {
        if !out.is_empty() {
            out.push_str(", ");
        }
        out.push_str(ct);
    }
    if out.is_empty() {
        String::new()
    } else {
        format!(" ({out})")
    }
}

type Shared = Arc<(Mutex<Buf>, Condvar)>;

pub struct ProgressiveSource {
    shared: Shared,
    pos: u64,
}

impl Drop for ProgressiveSource {
    fn drop(&mut self) {
        let (m, cv) = &*self.shared;
        m.lock().unwrap_or_else(|e| e.into_inner()).stop = true;
        cv.notify_all();
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .user_agent("tide-tv/0.1")
        .build()
}

impl ProgressiveSource {
    pub fn open(spec: &StreamSpec) -> Result<Self, String> {
        let shared: Shared = Arc::new((Mutex::new(Buf::default()), Condvar::new()));
        let s2 = shared.clone();
        let source = spec.source.clone();
        let duration = spec.duration;
        thread::Builder::new()
            .name("download".into())
            .spawn(move || {
                let r = match source {
                    StreamSource::Single(url) => download_single(&url, &s2),
                    StreamSource::Segmented { init, template, start_number, segment_secs } => {
                        download_segments(&init, &template, start_number, segment_secs, duration, &s2)
                    }
                };
                let (m, cv) = &*s2;
                let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
                match r {
                    Ok(()) => g.done = true,
                    Err(e) => g.error = Some(e),
                }
                drop(g);
                cv.notify_all();
            })
            .map_err(|e| e.to_string())?;
        let src = Self { shared, pos: 0 };
        // The decoder reports a bad stream as "unsupported" and hides the HTTP error that
        // caused it; catching the first bytes here keeps the real cause in the message.
        src.check_head()?;
        Ok(src)
    }

    fn check_head(&self) -> Result<(), String> {
        let (m, cv) = &*self.shared;
        let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
        let deadline = Instant::now() + HEAD_WAIT;
        loop {
            if g.data.len() >= HEAD {
                break;
            }
            if let Some(e) = &g.error {
                return Err(e.clone());
            }
            if g.done {
                if g.data.is_empty() {
                    return Err(format!("The stream sent no data{}", describe_http(&g)));
                }
                break;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break; // slow start: let the decoder wait instead of failing here
            }
            g = cv
                .wait_timeout(g, left.min(Duration::from_millis(500)))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        match classify(&g.data) {
            Head::Audio => Ok(()),
            Head::Hls => Err("The service sent an HLS playlist, which this player cannot stream".into()),
            Head::Other => Err(format!(
                "The stream is not audio this player understands{}",
                describe_http(&g)
            )),
        }
    }
}

fn append(shared: &Shared, bytes: &[u8]) -> bool {
    let (m, cv) = &**shared;
    let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
    if g.stop {
        return false;
    }
    g.data.extend_from_slice(bytes);
    drop(g);
    cv.notify_all();
    true
}

fn download_single(url: &str, shared: &Shared) -> Result<(), String> {
    // Error text deliberately omits the URL (it can carry signed tokens).
    let resp = agent().get(url).call().map_err(|e| match e {
        ureq::Error::Status(code, _) => format!("stream request failed (HTTP {code})"),
        ureq::Error::Transport(t) => format!("network error: {}", t.kind()),
    })?;
    {
        let (m, cv) = &**shared;
        let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
        g.status = Some(resp.status());
        g.content_type = resp.header("Content-Type").map(str::to_string);
        g.total = resp.header("Content-Length").and_then(|v| v.parse::<u64>().ok());
        drop(g);
        cv.notify_all();
    }
    let mut r = resp.into_reader();
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let n = r.read(&mut chunk).map_err(|e| format!("download interrupted: {e}"))?;
        if n == 0 {
            return Ok(());
        }
        if !append(shared, &chunk[..n]) {
            return Ok(()); // reader went away
        }
    }
}

fn fetch(url: &str) -> Result<Vec<u8>, u16> {
    let resp = agent().get(url).call().map_err(|e| match e {
        ureq::Error::Status(code, _) => code,
        ureq::Error::Transport(_) => 0,
    })?;
    let mut out = Vec::new();
    resp.into_reader().read_to_end(&mut out).map_err(|_| 0u16)?;
    Ok(out)
}

fn download_segments(
    init: &str,
    template: &str,
    start: u32,
    segment_secs: f32,
    duration: f32,
    shared: &Shared,
) -> Result<(), String> {
    let init_bytes = fetch(init).map_err(|c| format!("init segment failed (HTTP {c})"))?;
    if !append(shared, &init_bytes) {
        return Ok(());
    }
    // Segment count is not in the parsed manifest: derive it from the track length,
    // with slack, and treat the first failing segment after the first as the end.
    let wanted = ((duration / segment_secs.max(0.5)).ceil() as u32).saturating_add(2);
    for n in 0..wanted {
        let url = template.replace("$Number$", &(start + n).to_string());
        match fetch(&url) {
            Ok(bytes) => {
                if !append(shared, &bytes) {
                    return Ok(());
                }
            }
            Err(_) if n > 0 => return Ok(()),
            Err(c) => return Err(format!("first media segment failed (HTTP {c})")),
        }
    }
    Ok(())
}

impl Read for ProgressiveSource {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let (m, cv) = &*self.shared;
        let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            let p = self.pos as usize;
            if p < g.data.len() {
                let n = out.len().min(g.data.len() - p);
                out[..n].copy_from_slice(&g.data[p..p + n]);
                self.pos += n as u64;
                return Ok(n);
            }
            if let Some(e) = &g.error {
                return Err(io::Error::other(e.clone()));
            }
            if g.done {
                return Ok(0);
            }
            let (ng, t) = cv.wait_timeout(g, Duration::from_secs(30)).unwrap_or_else(|e| e.into_inner());
            g = ng;
            if t.timed_out() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "stream stalled"));
            }
        }
    }
}

impl Seek for ProgressiveSource {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let target = match from {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::Current(d) => self.pos as i64 + d,
            SeekFrom::End(d) => {
                // Needs the total: from Content-Length, else wait for the download to finish.
                let (m, cv) = &*self.shared;
                let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    if let Some(t) = g.total {
                        break t as i64 + d;
                    }
                    if g.done {
                        break g.data.len() as i64 + d;
                    }
                    if let Some(e) = &g.error {
                        return Err(io::Error::other(e.clone()));
                    }
                    g = cv.wait_timeout(g, Duration::from_millis(200)).unwrap_or_else(|e| e.into_inner()).0;
                }
            }
        };
        if target < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek before start"));
        }
        self.pos = target as u64;
        Ok(self.pos)
    }
}

impl MediaSource for ProgressiveSource {
    fn is_seekable(&self) -> bool {
        // Until the length is known a seek to the end cannot be answered — and `isomp4` rejects
        // a source that claims to be seekable without one ("seek error: stream is not
        // seekable"), which is what a DASH stream looks like while its segments download. As a
        // plain stream the demuxer reads the fragments as they arrive.
        self.byte_len().is_some()
    }

    fn byte_len(&self) -> Option<u64> {
        let g = self.shared.0.lock().unwrap_or_else(|e| e.into_inner());
        g.total.or(if g.done { Some(g.data.len() as u64) } else { None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first bytes are what decides between "hand this to the decoder" and a message that
    /// names the real problem.
    #[test]
    fn the_first_bytes_tell_audio_from_a_playlist_or_an_error_page() {
        assert_eq!(classify(b"fLaC\0\0\0\0"), Head::Audio);
        assert_eq!(classify(b"RIFF\0\0\0\0WAVE"), Head::Audio);
        assert_eq!(classify(b"\0\0\0\x20ftypM4A "), Head::Audio); // MP4 / DASH init
        assert_eq!(classify(b"#EXTM3U\n#EXT-X-VERSION:3\n"), Head::Hls);
        assert_eq!(classify(b"{\"status\":401}"), Head::Other);
        assert_eq!(classify(b"<html><head>"), Head::Other);
    }

    /// Claiming to be seekable without a length makes `isomp4` fail the whole stream, which is
    /// what a DASH download (no `Content-Length`) used to do.
    #[test]
    fn a_stream_without_a_known_length_does_not_claim_to_be_seekable() {
        let shared: Shared = Arc::new((Mutex::new(Buf::default()), Condvar::new()));
        let src = ProgressiveSource { shared: Arc::clone(&shared), pos: 0 };
        assert!(!src.is_seekable());

        shared.0.lock().unwrap().total = Some(64);
        assert!(src.is_seekable());
        assert_eq!(src.byte_len(), Some(64));
    }
}
