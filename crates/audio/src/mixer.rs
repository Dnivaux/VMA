//! The mixer (ADR 0007 § 5–6): per input a fader, a mute, sends to the buses and a solo; per
//! bus a fader and a mute; the headphones hear the Master, or the inputs in solo. Every gain
//! moves by a linear ramp over the block, from its value at the end of the previous one:
//! no click on a fader, a mute, a send, a solo or a cut of audio-follow-video.
//!
//! Signal path of an input: samples → *pre* meter → fader × mute → *post* meter → send to
//! each bus (× audio-follow-video for the Master). Solo AFL takes the post signal, PFL the
//! pre one (so a muted input can be listened to before it is opened).
//! Signal path of a bus: sum → *pre* meter → fader × mute → clamp to ±1 (counted) → *post*
//! meter.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use vma_core::AudioBus;

use crate::meter::{Meter, MeterCell, db_to_gain};
use crate::{CHANNELS, MAX_BLOCK};

/// Highest gain of a fader.
pub const MAX_GAIN_DB: f32 = 10.0;
const BUSES: usize = AudioBus::ALL.len();

/// What the headphones hear of an input in solo.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoloMode {
    /// After fader (and mute): as the input is in the mix.
    #[default]
    Afl,
    /// Before fader: the source itself.
    Pfl,
}

/// Settings of one input.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StripParams {
    /// Fader, from −∞ to [`MAX_GAIN_DB`] (above: clamped).
    pub gain_db: f32,
    pub mute: bool,
    pub solo: bool,
    /// Sends, indexed by [`AudioBus::index`].
    pub sends: [bool; BUSES],
    /// Share of the input in the Master by audio-follow-video, at the end of the block: 1 at
    /// the Program, 0 off it, the transition curve in between ([`equal_power`]). 1 when the
    /// option is off.
    pub on_air: f32,
}

impl Default for StripParams {
    /// 0 dB, not muted, to the Master only.
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            mute: false,
            solo: false,
            sends: [true, false, false],
            on_air: 1.0,
        }
    }
}

/// Settings of one bus.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BusParams {
    pub gain_db: f32,
    pub mute: bool,
}

impl Default for BusParams {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            mute: false,
        }
    }
}

/// Settings of the whole mix.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MixParams {
    /// Indexed by [`AudioBus::index`].
    pub buses: [BusParams; BUSES],
    pub solo_mode: SoloMode,
}

/// Shares of the outgoing and incoming input at `progress` (0 → 1) of a transition: equal
/// power, so the loudness holds through a fade between uncorrelated sources.
pub fn equal_power(progress: f32) -> (f32, f32) {
    let angle = progress.clamp(0.0, 1.0) * std::f32::consts::FRAC_PI_2;
    (angle.cos(), angle.sin())
}

/// One input in one block.
#[derive(Debug, Clone, Copy)]
pub struct StripInput<'a> {
    /// Slot of the input, below the `inputs` of [`Mixer::new`]: its ramps and meters.
    pub slot: usize,
    /// Interleaved stereo, the frames of the block (more are ignored, fewer are silence).
    pub samples: &'a [f32],
    pub params: StripParams,
}

/// Meters of the mixer, published for the UI.
#[derive(Debug)]
pub struct MixMeters {
    /// Per input slot: before and after fader.
    pub inputs: Box<[[MeterCell; 2]]>,
    /// Per bus ([`AudioBus::index`]): before and after fader.
    pub buses: [[MeterCell; 2]; BUSES],
    pub headphones: MeterCell,
}

/// Counters of the mixer, since it was made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MixStats {
    pub blocks: u64,
    /// Samples clamped to ±1, per bus.
    pub clipped: [u64; BUSES],
    pub headphones_clipped: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct StripState {
    /// Gains at the end of the previous block (0: absent then).
    post: f32,
    sends: [f32; BUSES],
    solo: f32,
    /// Given in the previous block.
    present: bool,
    /// Given in this block.
    seen: bool,
}

#[derive(Debug)]
struct MeterSet {
    inputs: Box<[[Meter; 2]]>,
    buses: [[Meter; 2]; BUSES],
    headphones: Meter,
}

/// The mixer, run by the audio thread once per tick. Nothing allocates after [`Mixer::new`].
#[derive(Debug)]
pub struct Mixer {
    strips: Box<[StripState]>,
    bus_gain: [f32; BUSES],
    /// Share of the Master in the headphones (0 while an input is in solo).
    headphones_master: f32,
    buses: [Box<[f32]>; BUSES],
    headphones: Box<[f32]>,
    post: Box<[f32]>,
    frames: usize,
    meters: MeterSet,
    published: Arc<MixMeters>,
    stats: MixStats,
}

fn buffer() -> Box<[f32]> {
    vec![0.0; MAX_BLOCK * CHANNELS].into_boxed_slice()
}

impl Mixer {
    /// A mixer for `inputs` slots.
    pub fn new(inputs: usize) -> Self {
        let published = Arc::new(MixMeters {
            inputs: (0..inputs).map(|_| Default::default()).collect(),
            buses: Default::default(),
            headphones: MeterCell::default(),
        });
        Self {
            strips: vec![StripState::default(); inputs].into_boxed_slice(),
            bus_gain: [0.0; BUSES],
            headphones_master: 1.0,
            buses: std::array::from_fn(|_| buffer()),
            headphones: buffer(),
            post: buffer(),
            frames: 0,
            meters: MeterSet {
                inputs: (0..inputs).map(|_| Default::default()).collect(),
                buses: Default::default(),
                headphones: Meter::default(),
            },
            published,
            stats: MixStats::default(),
        }
    }

    /// The meters, for the UI.
    pub fn meters(&self) -> Arc<MixMeters> {
        Arc::clone(&self.published)
    }

    pub fn stats(&self) -> MixStats {
        self.stats
    }

    /// The latest block of `bus` (interleaved stereo).
    pub fn bus(&self, bus: AudioBus) -> &[f32] {
        let len = self.frames * CHANNELS;
        self.buses
            .get(bus.index())
            .and_then(|b| b.get(..len))
            .unwrap_or_default()
    }

    /// The latest block of the headphones.
    pub fn headphones(&self) -> &[f32] {
        self.headphones
            .get(..self.frames * CHANNELS)
            .unwrap_or_default()
    }

    /// Mixes a block of `frames` frames (at most [`MAX_BLOCK`]). Inputs not given are absent:
    /// silent, their meters reset; one that comes back ramps in from silence.
    pub fn mix<'a>(
        &mut self,
        frames: usize,
        inputs: impl IntoIterator<Item = StripInput<'a>>,
        params: &MixParams,
    ) {
        let frames = frames.min(MAX_BLOCK);
        let len = frames * CHANNELS;
        self.frames = frames;
        self.stats.blocks += 1;
        for bus in &mut self.buses {
            bus.fill(0.0);
        }
        self.headphones.fill(0.0);
        for strip in self.strips.iter_mut() {
            strip.seen = false;
        }
        let mut any_solo = false;
        for input in inputs {
            let (Some(strip), Some(meters), Some(cells)) = (
                self.strips.get_mut(input.slot),
                self.meters.inputs.get_mut(input.slot),
                self.published.inputs.get(input.slot),
            ) else {
                continue;
            };
            let p = input.params;
            strip.present = true;
            strip.seen = true;
            any_solo |= p.solo;
            let samples = input.samples.get(..len).unwrap_or(input.samples);
            let pre = self.post.get_mut(..len).unwrap_or_default();
            pre.fill(0.0);
            pre.get_mut(..samples.len())
                .unwrap_or_default()
                .copy_from_slice(samples);
            meters[0].measure(pre, &cells[0]);
            // PFL: the source, before the fader.
            let solo = if p.solo { 1.0 } else { 0.0 };
            if params.solo_mode == SoloMode::Pfl {
                add_ramped(&mut self.headphones, pre, strip.solo, solo);
            }
            let post = if p.mute {
                0.0
            } else {
                db_to_gain(p.gain_db.min(MAX_GAIN_DB))
            };
            apply_ramp(pre, strip.post, post);
            strip.post = post;
            meters[1].measure(pre, &cells[1]);
            if params.solo_mode == SoloMode::Afl {
                add_ramped(&mut self.headphones, pre, strip.solo, solo);
            }
            strip.solo = solo;
            for (b, bus) in self.buses.iter_mut().enumerate() {
                let (Some(&on), Some(prev)) = (p.sends.get(b), strip.sends.get_mut(b)) else {
                    continue;
                };
                let follow = if b == AudioBus::Master.index() {
                    p.on_air.clamp(0.0, 1.0)
                } else {
                    1.0
                };
                let send = if on { follow } else { 0.0 };
                add_ramped(bus, pre, *prev, send);
                *prev = send;
            }
        }
        // Absent inputs: ramps from silence when they come back.
        for (slot, strip) in self.strips.iter_mut().enumerate() {
            if strip.present && !strip.seen {
                *strip = StripState::default();
                if let (Some(meters), Some(cells)) = (
                    self.meters.inputs.get_mut(slot),
                    self.published.inputs.get(slot),
                ) {
                    for (meter, cell) in meters.iter_mut().zip(cells) {
                        meter.reset();
                        meter.measure(&[], cell);
                    }
                }
            }
        }
        for (b, bus) in self.buses.iter_mut().enumerate() {
            let (Some(bp), Some(prev), Some(meters), Some(cells), Some(clipped)) = (
                params.buses.get(b),
                self.bus_gain.get_mut(b),
                self.meters.buses.get_mut(b),
                self.published.buses.get(b),
                self.stats.clipped.get_mut(b),
            ) else {
                continue;
            };
            let block = bus.get_mut(..len).unwrap_or_default();
            meters[0].measure(block, &cells[0]);
            let gain = if bp.mute {
                0.0
            } else {
                db_to_gain(bp.gain_db.min(MAX_GAIN_DB))
            };
            apply_ramp(block, *prev, gain);
            *prev = gain;
            *clipped += clamp(block);
            meters[1].measure(block, &cells[1]);
        }
        // Headphones: the Master, faded out while an input is in solo.
        let master_share = if any_solo { 0.0 } else { 1.0 };
        let master = self
            .buses
            .get(AudioBus::Master.index())
            .and_then(|b| b.get(..len))
            .unwrap_or_default();
        add_ramped(
            &mut self.headphones,
            master,
            self.headphones_master,
            master_share,
        );
        self.headphones_master = master_share;
        let block = self.headphones.get_mut(..len).unwrap_or_default();
        self.stats.headphones_clipped += clamp(block);
        self.meters
            .headphones
            .measure(block, &self.published.headphones);
    }
}

/// Gain of frame `f` of `frames` on a ramp from `from` (before the block) to `to` (its last
/// frame).
fn ramp(from: f32, to: f32, f: usize, frames: usize) -> f32 {
    if from == to {
        to
    } else {
        from + (to - from) * (f + 1) as f32 / frames as f32
    }
}

/// `block × ramp`, in place.
fn apply_ramp(block: &mut [f32], from: f32, to: f32) {
    let frames = block.len() / CHANNELS;
    for (f, frame) in block.as_chunks_mut::<CHANNELS>().0.iter_mut().enumerate() {
        let g = ramp(from, to, f, frames);
        for s in frame {
            *s *= g;
        }
    }
}

/// `dst += src × ramp`.
fn add_ramped(dst: &mut [f32], src: &[f32], from: f32, to: f32) {
    if from == 0.0 && to == 0.0 {
        return;
    }
    let frames = src.len() / CHANNELS;
    let (dst, _) = dst.as_chunks_mut::<CHANNELS>();
    let (src, _) = src.as_chunks::<CHANNELS>();
    for (f, (d, s)) in dst.iter_mut().zip(src).enumerate() {
        let g = ramp(from, to, f, frames);
        for (d, s) in d.iter_mut().zip(s) {
            *d += s * g;
        }
    }
}

/// Clamps to ±1; returns how many samples were beyond.
fn clamp(block: &mut [f32]) -> u64 {
    let mut clipped = 0;
    for s in block {
        if s.abs() > 1.0 {
            clipped += 1;
            *s = s.clamp(-1.0, 1.0);
        }
    }
    clipped
}

#[cfg(test)]
mod tests {
    use super::*;

    const N: usize = 960;

    fn constant(left: f32, right: f32) -> Vec<f32> {
        (0..N).flat_map(|_| [left, right]).collect()
    }

    fn strip(slot: usize, samples: &[f32], params: StripParams) -> StripInput<'_> {
        StripInput {
            slot,
            samples,
            params,
        }
    }

    /// Runs enough blocks for every ramp to settle; returns the last frame of each bus and of
    /// the headphones.
    fn settle(
        mixer: &mut Mixer,
        inputs: &[(usize, &[f32], StripParams)],
        params: &MixParams,
    ) -> ([[f32; 2]; 3], [f32; 2]) {
        for _ in 0..2 {
            mixer.mix(
                N,
                inputs.iter().map(|&(slot, s, p)| strip(slot, s, p)),
                params,
            );
        }
        let last = |b: &[f32]| [b[2 * N - 2], b[2 * N - 1]];
        (
            AudioBus::ALL.map(|bus| last(mixer.bus(bus))),
            last(mixer.headphones()),
        )
    }

    fn close(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-5 && (a[1] - b[1]).abs() < 1e-5
    }

    #[test]
    fn equal_power_keeps_the_power() {
        assert_eq!(equal_power(0.0), (1.0, 0.0));
        let (o, i) = equal_power(1.0);
        assert!(o.abs() < 1e-6 && (i - 1.0).abs() < 1e-6);
        for k in 0..=10 {
            let (o, i) = equal_power(k as f32 / 10.0);
            assert!((o * o + i * i - 1.0).abs() < 1e-6);
        }
    }

    /// C4: gains in dB, mute, sends, faders and mutes of the buses, sum of inputs.
    #[test]
    fn mixes_exactly() {
        let mut mixer = Mixer::new(4);
        let a = constant(0.1, 0.2);
        let b = constant(0.3, -0.3);
        let to_all = StripParams {
            sends: [true, true, true],
            ..StripParams::default()
        };
        let minus_6 = StripParams {
            gain_db: -6.0206,
            sends: [true, false, true],
            ..StripParams::default()
        };
        let mut params = MixParams::default();
        params.buses[AudioBus::B.index()].gain_db = 6.0206;
        let (buses, hp) = settle(&mut mixer, &[(0, &a, to_all), (2, &b, minus_6)], &params);
        assert!(close(buses[0], [0.1 + 0.15, 0.2 - 0.15]), "{buses:?}");
        assert!(close(buses[1], [0.1, 0.2]), "{buses:?}");
        assert!(close(buses[2], [2.0 * 0.25, 2.0 * 0.05]), "{buses:?}");
        assert!(close(hp, buses[0]), "headphones hear the Master: {hp:?}");

        // Mute of an input: gone from every bus; mute of a bus: silent.
        params.buses[AudioBus::A.index()].mute = true;
        let muted = StripParams {
            mute: true,
            ..minus_6
        };
        let (buses, _) = settle(&mut mixer, &[(0, &a, to_all), (2, &b, muted)], &params);
        assert!(close(buses[0], [0.1, 0.2]), "{buses:?}");
        assert!(close(buses[1], [0.0, 0.0]), "{buses:?}");
        assert!(close(buses[2], [0.2, 0.4]), "{buses:?}");

        // −∞ is silence; above +10 dB is +10 dB.
        let off = StripParams {
            gain_db: f32::NEG_INFINITY,
            ..to_all
        };
        let loud = StripParams {
            gain_db: 40.0,
            ..StripParams::default()
        };
        let quiet = constant(0.01, 0.0);
        let (buses, _) = settle(
            &mut mixer,
            &[(0, &a, off), (1, &quiet, loud)],
            &MixParams::default(),
        );
        assert!(close(buses[0], [0.031_623, 0.0]), "{buses:?}");
    }

    /// C4: a gain change ramps over one block, linearly, without a step.
    #[test]
    fn a_fader_move_ramps_over_one_block() {
        let mut mixer = Mixer::new(1);
        let a = constant(0.5, 0.5);
        let params = MixParams::default();
        settle(&mut mixer, &[(0, &a, StripParams::default())], &params);
        let muted = StripParams {
            mute: true,
            ..StripParams::default()
        };
        mixer.mix(N, [strip(0, &a, muted)], &params);
        let master = mixer.bus(AudioBus::Master);
        for f in 0..N {
            let expected = 0.5 * (1.0 - (f + 1) as f32 / N as f32);
            assert!((master[2 * f] - expected).abs() < 1e-6, "frame {f}");
        }
        mixer.mix(N, [strip(0, &a, muted)], &params);
        assert!(mixer.bus(AudioBus::Master).iter().all(|&s| s == 0.0));
    }

    /// C4: beyond ±1 is clamped and counted, never wrapped.
    #[test]
    fn clipping_is_clamped_and_counted() {
        let mut mixer = Mixer::new(2);
        let a = constant(0.8, -0.8);
        let params = MixParams::default();
        let (buses, _) = settle(
            &mut mixer,
            &[
                (0, &a, StripParams::default()),
                (1, &a, StripParams::default()),
            ],
            &params,
        );
        assert_eq!(buses[0], [1.0, -1.0]);
        assert!(mixer.stats().clipped[0] >= 2 * N as u64);
        assert_eq!(mixer.stats().clipped[1], 0);
    }

    /// C5: the headphones hear the inputs in solo (AFL: after fader; PFL: before), several at
    /// once; the Master does not change by a single sample.
    #[test]
    fn solo_goes_to_the_headphones_and_leaves_the_master_alone() {
        let a = constant(0.1, 0.1);
        let b = constant(0.2, 0.2);
        let c = constant(0.4, 0.4);
        let quieter = StripParams {
            gain_db: -6.0206,
            ..StripParams::default()
        };
        let base = [
            (0, a.as_slice(), StripParams::default()),
            (1, b.as_slice(), quieter),
            (2, c.as_slice(), StripParams::default()),
        ];
        let mut reference = Mixer::new(3);
        let (master, hp) = settle(&mut reference, &base, &MixParams::default());
        assert!(close(hp, master[0]));

        for mode in [SoloMode::Afl, SoloMode::Pfl] {
            let params = MixParams {
                solo_mode: mode,
                ..MixParams::default()
            };
            let mut mixer = Mixer::new(3);
            let mut soloed = base;
            soloed[0].2.solo = true;
            soloed[1].2.solo = true;
            settle(&mut mixer, &base, &params);
            for _ in 0..3 {
                mixer.mix(N, soloed.iter().map(|&(i, s, p)| strip(i, s, p)), &params);
                reference.mix(N, base.iter().map(|&(i, s, p)| strip(i, s, p)), &params);
                assert_eq!(
                    mixer.bus(AudioBus::Master),
                    reference.bus(AudioBus::Master),
                    "{mode:?}"
                );
            }
            let hp = [mixer.headphones()[0], mixer.headphones()[1]];
            let expected = match mode {
                SoloMode::Afl => 0.1 + 0.1,
                SoloMode::Pfl => 0.1 + 0.2,
            };
            assert!(close(hp, [expected, expected]), "{mode:?}: {hp:?}");
        }
    }

    /// PFL hears a muted input; AFL does not.
    #[test]
    fn pfl_hears_a_muted_input() {
        let a = constant(0.3, 0.3);
        let muted_solo = StripParams {
            mute: true,
            solo: true,
            ..StripParams::default()
        };
        for (mode, expected) in [(SoloMode::Pfl, 0.3), (SoloMode::Afl, 0.0)] {
            let mut mixer = Mixer::new(1);
            let params = MixParams {
                solo_mode: mode,
                ..MixParams::default()
            };
            let (buses, hp) = settle(&mut mixer, &[(0, &a, muted_solo)], &params);
            assert_eq!(buses[0], [0.0, 0.0]);
            assert!(close(hp, [expected, expected]), "{mode:?}: {hp:?}");
        }
    }

    /// C6 (mixer side): the on-air share weighs the Master only, ramped over the block; the
    /// buses A and B do not follow.
    #[test]
    fn audio_follow_video_weighs_the_master_only() {
        let a = constant(0.5, 0.5);
        let mut mixer = Mixer::new(1);
        let params = MixParams::default();
        let off_air = StripParams {
            sends: [true, true, false],
            on_air: 0.0,
            ..StripParams::default()
        };
        let (buses, _) = settle(&mut mixer, &[(0, &a, off_air)], &params);
        assert_eq!(buses[0], [0.0, 0.0]);
        assert!(close(buses[1], [0.5, 0.5]));
        // Cut to Program: one block of ramp, then full.
        let on_air = StripParams {
            on_air: 1.0,
            ..off_air
        };
        mixer.mix(N, [strip(0, &a, on_air)], &params);
        let master = mixer.bus(AudioBus::Master);
        assert!((master[0] - 0.5 / N as f32).abs() < 1e-6);
        assert!((master[2 * N - 2] - 0.5).abs() < 1e-6);
        // Half-way through a fade: cos(π/4).
        let (out, _) = equal_power(0.5);
        let fading = StripParams {
            on_air: out,
            ..off_air
        };
        let (buses, _) = settle(&mut mixer, &[(0, &a, fading)], &params);
        assert!(close(buses[0], [0.5 * out, 0.5 * out]), "{buses:?}");
        assert!(close(buses[1], [0.5, 0.5]));
    }

    /// An input that disappears is silent at once, its meters reset; back, it ramps in.
    #[test]
    fn an_absent_input_is_silent_and_ramps_back_in() {
        let a = constant(0.5, 0.5);
        let mut mixer = Mixer::new(2);
        let meters = mixer.meters();
        let params = MixParams::default();
        settle(&mut mixer, &[(1, &a, StripParams::default())], &params);
        assert!(meters.inputs[1][1].load().peak[0] > 0.49);
        mixer.mix(N, [], &params);
        assert!(mixer.bus(AudioBus::Master).iter().all(|&s| s == 0.0));
        assert_eq!(meters.inputs[1][1].load().peak, [0.0, 0.0]);
        mixer.mix(N, [strip(1, &a, StripParams::default())], &params);
        let master = mixer.bus(AudioBus::Master);
        assert!(master[0] < 1e-3, "{}", master[0]);
        assert!((master[2 * N - 2] - 0.5).abs() < 1e-6);
    }

    /// C7: the meters of the inputs (before / after fader) and of the buses are published.
    #[test]
    fn meters_are_published_before_and_after_the_faders() {
        let a = constant(0.5, 0.25);
        let mut mixer = Mixer::new(1);
        let meters = mixer.meters();
        let params = MixParams {
            buses: [
                BusParams {
                    gain_db: -6.0206,
                    mute: false,
                },
                BusParams::default(),
                BusParams::default(),
            ],
            ..MixParams::default()
        };
        let half = StripParams {
            gain_db: -6.0206,
            ..StripParams::default()
        };
        settle(&mut mixer, &[(0, &a, half)], &params);
        let near = |x: f32, y: f32| (x - y).abs() < 1e-4;
        let input = &meters.inputs[0];
        assert!(near(input[0].load().peak[0], 0.5));
        assert!(near(input[1].load().peak[0], 0.25));
        assert!(near(input[1].load().peak[1], 0.125));
        let master = &meters.buses[0];
        assert!(near(master[0].load().peak[0], 0.25));
        assert!(near(master[1].load().peak[0], 0.125));
        assert!(near(meters.headphones.load().peak[0], 0.125));
        assert_eq!(meters.buses[1][0].load().peak, [0.0, 0.0]);
    }
}
