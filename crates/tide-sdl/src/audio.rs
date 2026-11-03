//! SDL2 audio output for the player's PCM queue.
//!
//! SDL audio is the one path that is identical on desktop and (if the TV's SDL build
//! provides an audio driver) on webOS. Driver selection is SDL's: override it with
//! `SDL_AUDIODRIVER=<name>`; the chosen/available drivers are printed on first use.
//! TODO(TV): verify on hardware; the system-native alternative is the webOS media pipeline.

use sdl2::audio::{AudioCallback, AudioDevice, AudioSpecDesired};
use tide_player::{DeviceSpec, PcmPort};

struct Callback(PcmPort);

impl AudioCallback for Callback {
    type Channel = f32;
    fn callback(&mut self, out: &mut [f32]) {
        self.0.fill(out);
    }
}

pub struct AudioOut {
    sub: Result<sdl2::AudioSubsystem, String>,
    port: PcmPort,
    device: Option<AudioDevice<Callback>>,
    announced: bool,
}

impl AudioOut {
    pub fn new(sdl: &sdl2::Sdl, port: PcmPort) -> Self {
        Self { sub: sdl.audio(), port, device: None, announced: false }
    }

    /// Call every frame: (re)opens the device when the decoder asks for a new format.
    pub fn poll(&mut self) {
        let Some(want) = self.port.take_request() else { return };
        self.device = None; // close the old device first
        let sub = match &self.sub {
            Ok(s) => s,
            Err(e) => return self.port.device_failed(format!("SDL audio unavailable: {e}")),
        };
        // About 100 ms per callback keeps the mutex-guarded queue comfortable at any rate.
        let frames = (want.rate / 10).next_power_of_two().clamp(1024, 16384) as u16;
        let desired = AudioSpecDesired { freq: Some(want.rate as i32), channels: Some(want.channels), samples: Some(frames) };
        let port = self.port.clone();
        // What SDL reports as opened. (It may still convert to the hardware format internally.)
        let mut got = DeviceSpec { rate: want.rate, channels: want.channels };
        let result = sub.open_playback(None::<&str>, &desired, |obtained| {
            got = DeviceSpec { rate: obtained.freq as u32, channels: obtained.channels };
            Callback(port)
        });
        match result {
            Ok(dev) => {
                dev.resume();
                let driver = sub.current_audio_driver().to_string();
                if !self.announced {
                    self.announced = true;
                    let names: Vec<_> = sdl2::audio::drivers().collect();
                    crate::logf!("audio: using driver {driver:?} (available: {names:?})");
                }
                crate::logf!(
                    "audio: device opened at {} Hz, {} ch (asked {} Hz, {} ch)",
                    got.rate, got.channels, want.rate, want.channels
                );
                self.device = Some(dev);
                self.port.device_ready(got, driver);
            }
            Err(e) => {
                crate::logf!("audio: could not open a device: {e}");
                self.port.device_failed(e);
            }
        }
    }
}
