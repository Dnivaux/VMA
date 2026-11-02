//! Real-time audio of VMA (ADR 0007): the timestamped ring of each input, the rate estimate
//! and resampler that read it at the time of the picture shown, the mixer (gain, mute, buses,
//! solo, audio-follow-video) and the meters.
//!
//! No GStreamer here: the pipelines of `io-gst` write the rings, the `vma-audio` thread of
//! `engine` drives the rest once per tick. Nothing allocates after construction.

mod rate;
mod ring;

pub use rate::{BANDWIDTH_HZ, MAX_DEVIATION, RESET_FRAMES, RateEstimator, RateUpdate};
pub use ring::{Anchor, ReadOutcome, RingReader, RingWriter, frames_to_ns, ring};

/// Sample rate of the whole mix, the rings and the outputs.
pub const SAMPLE_RATE: u32 = 48_000;
/// Interleaved stereo everywhere (phase-3.md, Q9).
pub const CHANNELS: usize = 2;
/// Longest block of a tick, in frames: 25p (1 920).
pub const MAX_BLOCK: usize = SAMPLE_RATE as usize / 25;
/// Capacity of an input ring: 2 s.
pub const RING_FRAMES: usize = 2 * SAMPLE_RATE as usize;
