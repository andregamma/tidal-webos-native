use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::{Duration, Instant},
};

/// Output format requested from the platform audio layer (interleaved f32).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceSpec {
    pub rate: u32,
    pub channels: u8,
}

struct State {
    queue: VecDeque<f32>,
    /// Max queued samples (about 2 s of audio).
    cap: usize,
    /// Format of the open device (None until the platform confirms).
    spec: Option<DeviceSpec>,
    requested: Option<DeviceSpec>,
    /// The last format asked for, and whether the platform has confirmed opening it.
    last_request: Option<DeviceSpec>,
    opened: bool,
    /// SDL audio driver in use (diagnostics).
    driver: String,
    device_error: Option<String>,
    paused: bool,
    /// Bumped on every flush; producers holding an old value must stop pushing.
    generation: u32,
    /// Frames played since the last flush, plus where that flush was in the track.
    consumed: u64,
    base_secs: f64,
}

/// Shared PCM queue between the decoder thread and the audio callback.
#[derive(Clone)]
pub struct PcmPort {
    inner: Arc<(Mutex<State>, Condvar)>,
}

impl Default for PcmPort {
    fn default() -> Self {
        Self::new()
    }
}

impl PcmPort {
    pub fn new() -> Self {
        let st = State {
            queue: VecDeque::new(),
            cap: 0,
            spec: None,
            requested: None,
            last_request: None,
            opened: false,
            driver: String::new(),
            device_error: None,
            paused: false,
            generation: 0,
            consumed: 0,
            base_secs: 0.0,
        };
        Self { inner: Arc::new((Mutex::new(st), Condvar::new())) }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.inner.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ---- platform side -------------------------------------------------

    /// Call from the audio callback: copy queued samples, pad with silence.
    pub fn fill(&self, out: &mut [f32]) {
        let mut s = self.lock();
        let ch = s.spec.map_or(2, |d| d.channels as usize).max(1);
        let n = if s.paused { 0 } else { out.len().min(s.queue.len()) };
        for (o, v) in out.iter_mut().zip(s.queue.drain(..n)) {
            *o = v;
        }
        out[n..].iter_mut().for_each(|o| *o = 0.0);
        s.consumed += (n / ch) as u64;
        drop(s);
        self.inner.1.notify_all();
    }

    /// The decoder wants a device with this format (poll from the main loop).
    pub fn take_request(&self) -> Option<DeviceSpec> {
        self.lock().requested.take()
    }

    /// `spec` is what the platform actually opened (SDL may convert to the hardware format
    /// internally; that step is not visible here).
    pub fn device_ready(&self, spec: DeviceSpec, driver: String) {
        let mut s = self.lock();
        s.spec = Some(spec);
        s.driver = driver;
        s.opened = true;
        s.device_error = None;
        drop(s);
        self.inner.1.notify_all();
    }

    pub fn device_failed(&self, msg: String) {
        self.lock().device_error = Some(msg);
        self.inner.1.notify_all();
    }

    // ---- decoder / control side ---------------------------------------

    /// Ensure the device matches `want`, waiting for the platform to (re)open it.
    pub fn configure(&self, want: DeviceSpec) -> Result<(), String> {
        let (m, cv) = &*self.inner;
        let mut s = m.lock().unwrap_or_else(|e| e.into_inner());
        if s.opened && s.last_request == Some(want) {
            return Ok(());
        }
        s.queue.clear();
        s.generation = s.generation.wrapping_add(1);
        s.cap = want.rate as usize * want.channels as usize * 2;
        s.requested = Some(want);
        s.last_request = Some(want);
        s.opened = false;
        s.spec = None;
        s.device_error = None;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if s.opened {
                return Ok(());
            }
            if let Some(e) = s.device_error.take() {
                return Err(format!("Could not open the audio device: {e}"));
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("No audio output became available".into());
            }
            s = cv.wait_timeout(s, left).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    /// Format and driver of the open device, if any.
    pub fn device_info(&self) -> Option<(DeviceSpec, String)> {
        let s = self.lock();
        s.spec.map(|d| (d, s.driver.clone()))
    }

    pub fn generation(&self) -> u32 {
        self.lock().generation
    }

    /// Drop queued audio; the next sample played corresponds to `base_secs`.
    pub fn flush(&self, base_secs: f64) -> u32 {
        let mut s = self.lock();
        s.queue.clear();
        s.generation = s.generation.wrapping_add(1);
        s.consumed = 0;
        s.base_secs = base_secs;
        let g = s.generation;
        drop(s);
        self.inner.1.notify_all();
        g
    }

    /// Queue samples, blocking while the queue is full. False = a flush happened
    /// (this producer is stale or the track changed): drop the chunk.
    pub fn push(&self, samples: &[f32], gen: u32) -> bool {
        let (m, cv) = &*self.inner;
        let mut s = m.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if s.generation != gen {
                return false;
            }
            if s.queue.is_empty() || s.queue.len() + samples.len() <= s.cap {
                s.queue.extend(samples.iter().copied());
                return true;
            }
            s = cv.wait_timeout(s, Duration::from_millis(100)).unwrap_or_else(|e| e.into_inner()).0;
        }
    }

    pub fn set_paused(&self, paused: bool) {
        self.lock().paused = paused;
    }

    pub fn is_paused(&self) -> bool {
        self.lock().paused
    }

    /// Position of the sample currently being played, in seconds.
    pub fn position_secs(&self) -> f64 {
        let s = self.lock();
        let rate = s.spec.map_or(44_100, |d| d.rate).max(1) as f64;
        s.base_secs + s.consumed as f64 / rate
    }

    /// Wait until everything queued has been played. False if a flush interrupted it.
    pub fn wait_drained(&self, gen: u32) -> bool {
        let (m, cv) = &*self.inner;
        let mut s = m.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if s.generation != gen {
                return false;
            }
            if s.queue.is_empty() {
                return true;
            }
            s = cv.wait_timeout(s, Duration::from_millis(100)).unwrap_or_else(|e| e.into_inner()).0;
        }
    }
}
