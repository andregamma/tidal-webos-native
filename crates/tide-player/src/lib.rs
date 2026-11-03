//! Player: network -> demux/decode (symphonia) -> PCM queue -> platform audio.
//!
//! The crate never opens an audio device itself. It exposes a [`PcmPort`]: the
//! platform layer (SDL on desktop and, if the TV's SDL build supports it, on webOS)
//! drains it from its audio callback via [`PcmPort::fill`] and opens/reopens the
//! device when [`PcmPort::take_request`] asks for a new format.
//!
//! A webOS-native backend (hardware decode through the system media pipeline)
//! would replace this whole decode path; the transport protocol
//! (`PlayerCmd`/`Evt`) does not change. See docs/audio.md.

mod engine;
mod pcm;
mod source;

pub use engine::spawn;
pub use pcm::{DeviceSpec, PcmPort};
