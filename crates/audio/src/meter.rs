//! Meters (ADR 0007 § 5, phase-3.md § 6): peak per block with a 1.5 s hold, RMS over the
//! latest 300 ms, per channel. The audio thread measures; the UI reads the values published in
//! atomics, at its own pace, without a lock.

use std::sync::atomic::{AtomicU32, Ordering};

use crate::{CHANNELS, SAMPLE_RATE};

/// How long a peak is held: 1.5 s.
pub const HOLD_FRAMES: u64 = SAMPLE_RATE as u64 * 3 / 2;
/// Window of the RMS: 300 ms.
pub const RMS_FRAMES: u64 = SAMPLE_RATE as u64 * 3 / 10;
/// Blocks kept for the RMS window: 300 ms of the shortest block (60p, 800 frames) is 18.
const RMS_BLOCKS: usize = 32;

/// Linear values of a meter, per channel (1.0 = 0 dBFS).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct MeterReading {
    /// Largest absolute sample of the latest block.
    pub peak: [f32; CHANNELS],
    /// Largest peak of the last 1.5 s.
    pub hold: [f32; CHANNELS],
    /// Root mean square of the last 300 ms.
    pub rms: [f32; CHANNELS],
}

/// Decibels of a linear level; −∞ for silence.
pub fn to_db(linear: f32) -> f32 {
    if linear > 0.0 {
        20.0 * linear.log10()
    } else {
        f32::NEG_INFINITY
    }
}

/// Linear gain of a level in decibels; 0 for −∞.
pub fn db_to_gain(db: f32) -> f32 {
    if db == f32::NEG_INFINITY {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

/// A meter as published: written by the audio thread, read by anyone.
#[derive(Debug, Default)]
pub struct MeterCell {
    /// peak, hold, rms of each channel, as `f32` bits.
    values: [AtomicU32; 3 * CHANNELS],
}

impl MeterCell {
    fn store(&self, r: &MeterReading) {
        for c in 0..CHANNELS {
            for (k, v) in [r.peak, r.hold, r.rms].iter().enumerate() {
                if let (Some(cell), Some(v)) = (self.values.get(k * CHANNELS + c), v.get(c)) {
                    cell.store(v.to_bits(), Ordering::Relaxed);
                }
            }
        }
    }

    /// The latest values (channels may come from two successive blocks).
    pub fn load(&self) -> MeterReading {
        let mut r = MeterReading::default();
        for c in 0..CHANNELS {
            for (k, v) in [&mut r.peak, &mut r.hold, &mut r.rms]
                .into_iter()
                .enumerate()
            {
                if let (Some(cell), Some(v)) = (self.values.get(k * CHANNELS + c), v.get_mut(c)) {
                    *v = f32::from_bits(cell.load(Ordering::Relaxed));
                }
            }
        }
        r
    }
}

/// The measuring side of one meter.
#[derive(Debug, Clone)]
pub struct Meter {
    /// Sum of squares and frames of the latest blocks, newest at `next − 1`.
    energy: [[f64; CHANNELS]; RMS_BLOCKS],
    frames: [u64; RMS_BLOCKS],
    next: usize,
    hold: [f32; CHANNELS],
    hold_age: [u64; CHANNELS],
}

impl Default for Meter {
    fn default() -> Self {
        Self {
            energy: [[0.0; CHANNELS]; RMS_BLOCKS],
            frames: [0; RMS_BLOCKS],
            next: 0,
            hold: [0.0; CHANNELS],
            hold_age: [0; CHANNELS],
        }
    }
}

impl Meter {
    /// Back to silence.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Measures a block (interleaved stereo) and publishes the result in `cell`.
    pub fn measure(&mut self, block: &[f32], cell: &MeterCell) -> MeterReading {
        let frames = (block.len() / CHANNELS) as u64;
        let mut peak = [0.0f32; CHANNELS];
        let mut energy = [0.0f64; CHANNELS];
        for frame in block.as_chunks::<CHANNELS>().0 {
            for ((p, e), &s) in peak.iter_mut().zip(energy.iter_mut()).zip(frame) {
                *p = p.max(s.abs());
                *e += f64::from(s) * f64::from(s);
            }
        }
        let slot = self.next % RMS_BLOCKS;
        if let (Some(e), Some(f)) = (self.energy.get_mut(slot), self.frames.get_mut(slot)) {
            *e = energy;
            *f = frames;
        }
        self.next = (slot + 1) % RMS_BLOCKS;

        let mut reading = MeterReading {
            peak,
            ..MeterReading::default()
        };
        // RMS: the newest blocks until 300 ms are covered.
        let mut sum = [0.0f64; CHANNELS];
        let mut covered = 0u64;
        for back in 1..=RMS_BLOCKS {
            let k = (self.next + RMS_BLOCKS - back) % RMS_BLOCKS;
            let (Some(e), Some(&f)) = (self.energy.get(k), self.frames.get(k)) else {
                break;
            };
            if f == 0 || covered >= RMS_FRAMES {
                break;
            }
            for (s, e) in sum.iter_mut().zip(e) {
                *s += e;
            }
            covered += f;
        }
        for c in 0..CHANNELS {
            let (Some(&p), Some(hold), Some(age), Some(rms), Some(h), Some(&s)) = (
                peak.get(c),
                self.hold.get_mut(c),
                self.hold_age.get_mut(c),
                reading.rms.get_mut(c),
                reading.hold.get_mut(c),
                sum.get(c),
            ) else {
                continue;
            };
            *age += frames;
            if p >= *hold || *age >= HOLD_FRAMES {
                *hold = p;
                *age = 0;
            }
            *h = *hold;
            *rms = if covered == 0 {
                0.0
            } else {
                (s / covered as f64).sqrt() as f32
            };
        }
        cell.store(&reading);
        reading
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(amplitude: f32, hz: f32, frames: usize, start: usize) -> Vec<f32> {
        (start..start + frames)
            .flat_map(|k| {
                let v = amplitude * (std::f32::consts::TAU * hz * k as f32 / 48_000.0).sin();
                [v, v * 0.5]
            })
            .collect()
    }

    #[test]
    fn decibels() {
        assert_eq!(to_db(0.0), f32::NEG_INFINITY);
        assert!((to_db(1.0)).abs() < 1e-6);
        assert!((to_db(0.5) + 6.0206).abs() < 1e-3);
        assert_eq!(db_to_gain(f32::NEG_INFINITY), 0.0);
        assert!((db_to_gain(10.0) - 3.1623).abs() < 1e-3);
        assert!((db_to_gain(-6.0206) - 0.5).abs() < 1e-4);
    }

    /// C7: a sine of known level reads its peak and RMS to ±0.1 dB; silence reads −∞.
    #[test]
    fn a_known_sine_reads_its_peak_and_rms() {
        let mut meter = Meter::default();
        let cell = MeterCell::default();
        let mut r = MeterReading::default();
        // 1 kHz at −6 dBFS (left) and −12 dBFS (right), blocks of 50p.
        for n in 0..50 {
            r = meter.measure(&sine(0.5, 1_000.0, 960, 960 * n), &cell);
        }
        assert_eq!(cell.load(), r);
        let peak_db = [-6.0206, -12.0412];
        for (c, db) in peak_db.into_iter().enumerate() {
            assert!((to_db(r.peak[c]) - db).abs() < 0.1, "{r:?}");
            // RMS of a sine: peak − 3.01 dB.
            assert!((to_db(r.rms[c]) - (db - 3.0103)).abs() < 0.1, "{r:?}");
        }
        let silence = vec![0.0; 2 * 960];
        for _ in 0..100 {
            r = meter.measure(&silence, &cell);
        }
        assert_eq!(to_db(r.peak[0]), f32::NEG_INFINITY);
        assert_eq!(to_db(r.rms[1]), f32::NEG_INFINITY);
        assert_eq!(to_db(r.hold[0]), f32::NEG_INFINITY);
    }

    #[test]
    fn the_peak_is_held_for_a_second_and_a_half() {
        let mut meter = Meter::default();
        let cell = MeterCell::default();
        let mut click = vec![0.0; 2 * 960];
        click[10] = 0.8;
        meter.measure(&click, &cell);
        let quiet = sine(0.1, 440.0, 960, 0);
        // 1.5 s = 75 blocks of 20 ms: held through the 74th block after it.
        for n in 1..75 {
            let r = meter.measure(&quiet, &cell);
            assert_eq!(r.hold[0], 0.8, "block {n}");
            assert!(r.peak[0] < 0.11);
        }
        let r = meter.measure(&quiet, &cell);
        assert!(r.hold[0] < 0.11, "{r:?}");
    }

    #[test]
    fn the_rms_covers_300_ms() {
        let mut meter = Meter::default();
        let cell = MeterCell::default();
        let loud = vec![0.5; 2 * 960];
        let silence = vec![0.0; 2 * 960];
        for _ in 0..20 {
            meter.measure(&loud, &cell);
        }
        // 300 ms = 15 blocks: after 10 blocks of silence, a third of the window is loud.
        let mut r = MeterReading::default();
        for _ in 0..10 {
            r = meter.measure(&silence, &cell);
        }
        assert!(
            (r.rms[0] - 0.5 * (5.0f32 / 15.0).sqrt()).abs() < 1e-4,
            "{r:?}"
        );
        for _ in 0..5 {
            r = meter.measure(&silence, &cell);
        }
        assert_eq!(r.rms[0], 0.0);
    }
}
