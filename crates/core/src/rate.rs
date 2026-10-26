use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Errors produced when building a [`FrameRate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RateError {
    #[error("frame rate numerator must be > 0")]
    ZeroNumerator,
    #[error("frame rate denominator must be > 0")]
    ZeroDenominator,
}

/// A rational frame rate, in frames per second: `num / den`.
///
/// Rational (not float) so that 59.94 (60000/1001) stays exact and long-running schedules
/// never drift. Always stored in reduced form, so equality is structural (also when
/// deserialised: `{"num":120000,"den":2002}` reads as 59.94).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawFrameRate")]
pub struct FrameRate {
    num: u32,
    den: u32,
}

#[derive(Deserialize)]
struct RawFrameRate {
    num: u32,
    den: u32,
}

impl TryFrom<RawFrameRate> for FrameRate {
    type Error = RateError;

    fn try_from(raw: RawFrameRate) -> Result<Self, Self::Error> {
        Self::new(raw.num, raw.den)
    }
}

impl FrameRate {
    pub const FPS_25: Self = Self { num: 25, den: 1 };
    pub const FPS_29_97: Self = Self {
        num: 30000,
        den: 1001,
    };
    pub const FPS_30: Self = Self { num: 30, den: 1 };
    pub const FPS_50: Self = Self { num: 50, den: 1 };
    pub const FPS_59_94: Self = Self {
        num: 60000,
        den: 1001,
    };
    pub const FPS_60: Self = Self { num: 60, den: 1 };

    /// Project default (brief §4, Phase 0).
    pub const DEFAULT: Self = Self::FPS_50;

    /// Master rates supported from Phase 1, each with its own acceptance run (ADR 0002).
    pub const MASTER_RATES: [Self; 6] = [
        Self::FPS_25,
        Self::FPS_29_97,
        Self::FPS_30,
        Self::FPS_50,
        Self::FPS_59_94,
        Self::FPS_60,
    ];

    pub fn new(num: u32, den: u32) -> Result<Self, RateError> {
        if num == 0 {
            return Err(RateError::ZeroNumerator);
        }
        if den == 0 {
            return Err(RateError::ZeroDenominator);
        }
        let g = gcd(num, den);
        Ok(Self {
            num: num / g,
            den: den / g,
        })
    }

    pub fn num(self) -> u32 {
        self.num
    }

    pub fn den(self) -> u32 {
        self.den
    }

    pub fn as_f64(self) -> f64 {
        f64::from(self.num) / f64::from(self.den)
    }

    /// Start of frame `index` relative to frame 0, in nanoseconds: the first whole nanosecond
    /// at or after the exact rational start (rounded up), so that
    /// `frame_at_nanos(frame_offset_nanos(i)) == i` always holds.
    ///
    /// Computed from the frame index (not by accumulating a period) so there is no drift,
    /// whatever the run length. Uses 128-bit arithmetic: cannot overflow for any `u64` index.
    pub fn frame_offset_nanos(self, index: u64) -> u128 {
        let num = u128::from(self.num);
        (u128::from(index) * u128::from(self.den) * 1_000_000_000).div_ceil(num)
    }

    /// Nominal duration of one frame (rounded up to the nanosecond).
    pub fn frame_duration(self) -> Duration {
        let nanos = u64::try_from(self.frame_offset_nanos(1)).unwrap_or(u64::MAX);
        Duration::from_nanos(nanos)
    }

    /// Index of the last frame whose start time is `<= elapsed_nanos`.
    pub fn frame_at_nanos(self, elapsed_nanos: u128) -> u64 {
        let idx = elapsed_nanos * u128::from(self.num) / (u128::from(self.den) * 1_000_000_000);
        u64::try_from(idx).unwrap_or(u64::MAX)
    }
}

impl Default for FrameRate {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl fmt::Display for FrameRate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.den == 1 {
            write!(f, "{}p", self.num)
        } else {
            write!(f, "{:.2}p", self.as_f64())
        }
    }
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_zero() {
        assert_eq!(FrameRate::new(0, 1), Err(RateError::ZeroNumerator));
        assert_eq!(FrameRate::new(50, 0), Err(RateError::ZeroDenominator));
    }

    #[test]
    fn reduces() {
        assert_eq!(FrameRate::new(100, 2).unwrap(), FrameRate::FPS_50);
        assert_eq!(FrameRate::new(120000, 2002).unwrap(), FrameRate::FPS_59_94);
    }

    #[test]
    fn fifty_fps_is_20ms() {
        assert_eq!(
            FrameRate::FPS_50.frame_duration(),
            Duration::from_millis(20)
        );
        assert_eq!(FrameRate::FPS_50.frame_offset_nanos(50), 1_000_000_000);
    }

    #[test]
    fn ntsc_rate_does_not_drift_over_24h() {
        let r = FrameRate::FPS_59_94;
        // 24 h of 59.94 fps = 5_178_816 frames exactly spans 86_400 s.
        let frames = 24 * 3600 * 60000 / 1001;
        let nanos = r.frame_offset_nanos(frames);
        let expected = 86_400_000_000_000u128;
        assert!(expected - nanos < r.frame_duration().as_nanos());
    }

    #[test]
    fn frame_at_is_inverse_of_offset() {
        for r in [FrameRate::FPS_25, FrameRate::FPS_50, FrameRate::FPS_59_94] {
            for i in [0u64, 1, 2, 49, 50, 1_000_000] {
                let t = r.frame_offset_nanos(i);
                assert_eq!(r.frame_at_nanos(t), i, "{r} frame {i}");
                if t > 0 {
                    assert_eq!(r.frame_at_nanos(t - 1), i - 1, "{r} frame {i} - 1ns");
                }
            }
        }
    }

    #[test]
    fn display() {
        assert_eq!(FrameRate::FPS_50.to_string(), "50p");
        assert_eq!(FrameRate::FPS_59_94.to_string(), "59.94p");
        assert_eq!(FrameRate::FPS_29_97.to_string(), "29.97p");
    }

    #[test]
    fn master_rate_constants_are_reduced() {
        for r in FrameRate::MASTER_RATES {
            assert_eq!(FrameRate::new(r.num(), r.den()), Ok(r));
        }
    }

    #[test]
    fn serde_validates_and_reduces() {
        let json = serde_json::to_string(&FrameRate::FPS_59_94).unwrap();
        assert_eq!(json, r#"{"num":60000,"den":1001}"#);
        let r: FrameRate = serde_json::from_str(r#"{"num":120000,"den":2002}"#).unwrap();
        assert_eq!(r, FrameRate::FPS_59_94);
        assert!(serde_json::from_str::<FrameRate>(r#"{"num":50,"den":0}"#).is_err());
    }
}
