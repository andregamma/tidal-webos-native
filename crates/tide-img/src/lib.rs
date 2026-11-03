//! Cover image loader: download over HTTP and decode on worker threads, so the UI thread
//! never waits. Results come back as tightly packed RGB8; uploading to the GPU is the
//! renderer's job (it must happen on the GL thread).

use std::{
    io::Read,
    sync::{
        mpsc::{channel, Receiver, Sender},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};

/// Largest response accepted for one image.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

pub enum Loaded {
    Ok { key: u64, w: u32, h: u32, rgb: Vec<u8> },
    Failed { key: u64, why: String },
}

pub struct Loader {
    req: Sender<(u64, String)>,
    res: Receiver<Loaded>,
}

impl Loader {
    /// `workers` threads fetch and decode in parallel.
    pub fn new(workers: usize) -> Self {
        let (req_tx, req_rx) = channel::<(u64, String)>();
        let (res_tx, res_rx) = channel::<Loaded>();
        let req_rx = Arc::new(Mutex::new(req_rx));
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(20))
            .user_agent("tide-tv/0.1")
            .build();
        for i in 0..workers.max(1) {
            let rx = req_rx.clone();
            let tx = res_tx.clone();
            let agent = agent.clone();
            let spawned = thread::Builder::new().name(format!("img-{i}")).spawn(move || loop {
                let job = match rx.lock() {
                    Ok(guard) => guard.recv(),
                    Err(_) => return,
                };
                let Ok((key, url)) = job else { return };
                let msg = match fetch(&agent, &url) {
                    Ok((w, h, rgb)) => Loaded::Ok { key, w, h, rgb },
                    Err(why) => Loaded::Failed { key, why },
                };
                if tx.send(msg).is_err() {
                    return;
                }
            });
            drop(spawned);
        }
        Self { req: req_tx, res: res_rx }
    }

    /// Queue a download. `key` is echoed back in the result.
    pub fn request(&self, key: u64, url: String) {
        let _ = self.req.send((key, url));
    }

    /// One finished image, if any (non-blocking).
    pub fn poll(&self) -> Option<Loaded> {
        self.res.try_recv().ok()
    }
}

fn fetch(agent: &ureq::Agent, url: &str) -> Result<(u32, u32, Vec<u8>), String> {
    // The URL is left out of error text on purpose.
    let resp = agent.get(url).call().map_err(|e| match e {
        ureq::Error::Status(code, _) => format!("HTTP {code}"),
        ureq::Error::Transport(t) => format!("network error: {}", t.kind()),
    })?;
    let mut bytes = Vec::new();
    resp.into_reader().take(MAX_BYTES).read_to_end(&mut bytes).map_err(|e| format!("download interrupted: {e}"))?;
    let img = image::load_from_memory(&bytes).map_err(|e| format!("decode failed: {e}"))?;
    let rgb = img.to_rgb8();
    let (w, h) = rgb.dimensions();
    Ok((w, h, rgb.into_raw()))
}
