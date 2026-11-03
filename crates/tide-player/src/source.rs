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
    time::Duration,
};
use symphonia::core::io::MediaSource;
use tide_core::services::{StreamSource, StreamSpec};

#[derive(Default)]
struct Buf {
    data: Vec<u8>,
    total: Option<u64>,
    done: bool,
    error: Option<String>,
    stop: bool,
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
                    StreamSource::Simulated => Err("simulated streams have no data".into()),
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
        Ok(Self { shared, pos: 0 })
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
    if let Some(len) = resp.header("Content-Length").and_then(|v| v.parse::<u64>().ok()) {
        let (m, cv) = &**shared;
        m.lock().unwrap_or_else(|e| e.into_inner()).total = Some(len);
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
        true
    }

    fn byte_len(&self) -> Option<u64> {
        let g = self.shared.0.lock().unwrap_or_else(|e| e.into_inner());
        g.total.or(if g.done { Some(g.data.len() as u64) } else { None })
    }
}
