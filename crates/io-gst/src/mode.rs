//! Choice of a capture mode among those a device announces (spec `io-gst.md`, « Mode de
//! capture »):
//!
//! 1. resolution: the Program's if offered, else the largest one that fits in it, else the
//!    smallest one;
//! 2. rate: the closest at or above the master rate, else the highest below it;
//! 3. a raw format rather than MJPEG when it reaches that same rate (formats uploaded as is
//!    first, see `pipeline.rs`); MJPEG otherwise (decoded by `decodebin`).
//!
//! Other encodings (H.264 cameras…) are ignored in Phase 1.

use std::cmp::Ordering;

use gstreamer as gst;
use vma_core::{FrameRate, Resolution};

/// How the pictures of a mode are encoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Encoding {
    /// `video/x-raw` with this GStreamer format name (`YUY2`, `NV12`…).
    Raw(String),
    /// `image/jpeg`.
    Jpeg,
}

/// A mode a capture device runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureMode {
    pub encoding: Encoding,
    pub resolution: Resolution,
    pub rate: FrameRate,
}

impl CaptureMode {
    /// Fixed caps asking the device for this mode.
    pub(crate) fn caps(&self) -> gst::Caps {
        let (w, h) = (self.resolution.width(), self.resolution.height());
        let size = |v: u32| i32::try_from(v).unwrap_or(i32::MAX);
        let rate = gst::Fraction::new(
            i32::try_from(self.rate.num()).unwrap_or(i32::MAX),
            i32::try_from(self.rate.den()).unwrap_or(1),
        );
        let builder = match &self.encoding {
            Encoding::Raw(format) => gst::Caps::builder("video/x-raw").field("format", format),
            Encoding::Jpeg => gst::Caps::builder("image/jpeg"),
        };
        builder
            .field("width", size(w))
            .field("height", size(h))
            .field("framerate", rate)
            .build()
    }
}

impl std::fmt::Display for CaptureMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let encoding = match &self.encoding {
            Encoding::Raw(format) => format.as_str(),
            Encoding::Jpeg => "MJPEG",
        };
        write!(f, "{} {} {encoding}", self.resolution, self.rate)
    }
}

/// Raw formats the compositor uploads without conversion, in order of preference.
const UPLOADED: [&str; 6] = ["NV12", "I420", "YUY2", "UYVY", "RGBA", "BGRA"];

/// Every concrete mode in `caps`: lists expanded, ranges reduced to the values that matter
/// (the Program size or rate clamped into the range, and the range's ends).
pub(crate) fn modes(
    caps: &gst::CapsRef,
    target: Resolution,
    master: FrameRate,
) -> Vec<CaptureMode> {
    let mut all = Vec::new();
    for s in caps.iter() {
        let encodings: Vec<Encoding> = match s.name().as_str() {
            "video/x-raw" => strings(s, "format")
                .into_iter()
                .map(Encoding::Raw)
                .collect(),
            "image/jpeg" => vec![Encoding::Jpeg],
            _ => continue,
        };
        let widths = ints(s, "width", target.width());
        let heights = ints(s, "height", target.height());
        let rates = rates(s, master);
        for encoding in &encodings {
            for &w in &widths {
                for &h in &heights {
                    let Ok(resolution) = Resolution::new(w, h) else {
                        continue;
                    };
                    for &rate in &rates {
                        all.push(CaptureMode {
                            encoding: encoding.clone(),
                            resolution,
                            rate,
                        });
                    }
                }
            }
        }
    }
    all
}

/// The mode to open, by the rules of this module. `None` if no mode is usable.
pub(crate) fn choose(
    modes: &[CaptureMode],
    target: Resolution,
    master: FrameRate,
) -> Option<CaptureMode> {
    let area = |r: Resolution| u64::from(r.width()) * u64::from(r.height());
    let sizes = || modes.iter().map(|m| m.resolution);
    let resolution = if sizes().any(|r| r == target) {
        target
    } else {
        sizes()
            .filter(|r| r.width() <= target.width() && r.height() <= target.height())
            .max_by_key(|r| area(*r))
            .or_else(|| sizes().min_by_key(|r| area(*r)))?
    };
    let at_size: Vec<&CaptureMode> = modes
        .iter()
        .filter(|m| m.resolution == resolution)
        .collect();
    let best_rate = at_size
        .iter()
        .map(|m| m.rate)
        .min_by(|a, b| rate_rank(*a, *b, master))?;
    at_size
        .into_iter()
        .filter(|m| rate_rank(m.rate, best_rate, master) == Ordering::Equal)
        .min_by_key(|m| encoding_rank(&m.encoding))
        .cloned()
}

/// Orders two rates by preference against the master rate (`Less` = `a` is better).
fn rate_rank(a: FrameRate, b: FrameRate, master: FrameRate) -> Ordering {
    let key = |r: FrameRate| {
        let (r, m) = (r.as_f64(), master.as_f64());
        // Exact rationals compare equal; a small epsilon absorbs float noise only.
        if r + 1e-9 >= m {
            (0, r - m)
        } else {
            (1, m - r)
        }
    };
    let (ka, kb) = (key(a), key(b));
    ka.0.cmp(&kb.0).then(ka.1.total_cmp(&kb.1))
}

/// Raw formats uploaded as is, then other raw formats, then MJPEG.
fn encoding_rank(encoding: &Encoding) -> usize {
    match encoding {
        Encoding::Raw(f) => UPLOADED
            .iter()
            .position(|u| u == f)
            .unwrap_or(UPLOADED.len()),
        Encoding::Jpeg => UPLOADED.len() + 1,
    }
}

fn strings(s: &gst::StructureRef, field: &str) -> Vec<String> {
    if let Ok(v) = s.get::<String>(field) {
        return vec![v];
    }
    s.get::<gst::List>(field)
        .map(|l| l.iter().filter_map(|v| v.get::<String>().ok()).collect())
        .unwrap_or_default()
}

fn ints(s: &gst::StructureRef, field: &str, target: u32) -> Vec<u32> {
    let positive = |v: i32| u32::try_from(v).ok().filter(|v| *v > 0);
    if let Ok(v) = s.get::<i32>(field) {
        return positive(v).into_iter().collect();
    }
    if let Ok(l) = s.get::<gst::List>(field) {
        return l
            .iter()
            .filter_map(|v| v.get::<i32>().ok())
            .filter_map(positive)
            .collect();
    }
    if let Ok(r) = s.get::<gst::IntRange<i32>>(field) {
        let (min, max) = (
            positive(r.min()).unwrap_or(1),
            positive(r.max()).unwrap_or(1),
        );
        let mut v = vec![target.clamp(min, max.max(min)), min, max];
        v.sort_unstable();
        v.dedup();
        return v;
    }
    Vec::new()
}

fn fraction(f: gst::Fraction) -> Option<FrameRate> {
    let num = u32::try_from(f.numer()).ok()?;
    let den = u32::try_from(f.denom()).ok()?;
    FrameRate::new(num, den).ok()
}

fn rates(s: &gst::StructureRef, master: FrameRate) -> Vec<FrameRate> {
    if let Ok(f) = s.get::<gst::Fraction>("framerate") {
        return fraction(f).into_iter().collect();
    }
    if let Ok(l) = s.get::<gst::List>("framerate") {
        return l
            .iter()
            .filter_map(|v| v.get::<gst::Fraction>().ok())
            .filter_map(fraction)
            .collect();
    }
    if let Ok(r) = s.get::<gst::FractionRange>("framerate") {
        let (min, max) = (fraction(r.min()), fraction(r.max()));
        let mut v: Vec<FrameRate> = min.into_iter().chain(max).collect();
        if let (Some(min), Some(max)) = (min, max)
            && min.as_f64() <= master.as_f64()
            && master.as_f64() <= max.as_f64()
        {
            v.push(master);
        }
        return v;
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn pick(caps: &str, target: Resolution, master: FrameRate) -> Option<String> {
        gst::init().unwrap();
        let caps = gst::Caps::from_str(caps).unwrap();
        choose(&modes(&caps, target, master), target, master).map(|m| m.to_string())
    }

    const HD: Resolution = Resolution::HD_1080;

    fn r(w: u32, h: u32) -> Resolution {
        Resolution::new(w, h).unwrap()
    }

    /// A typical USB webcam: raw YUY2 is slow at large sizes, MJPEG reaches 30 fps.
    const WEBCAM: &str = "\
        video/x-raw, format=YUY2, width=1920, height=1080, framerate={ 5/1 }; \
        video/x-raw, format=YUY2, width=1280, height=720, framerate={ 10/1, 5/1 }; \
        video/x-raw, format=YUY2, width=640, height=480, framerate={ 30/1, 15/1 }; \
        image/jpeg, width=1920, height=1080, framerate={ 30/1, 15/1 }; \
        image/jpeg, width=1280, height=720, framerate={ 60/1, 30/1 }; \
        image/jpeg, width=640, height=480, framerate={ 30/1 }";

    #[test]
    fn program_size_with_mjpeg_when_raw_is_too_slow() {
        let m = choose_mode(WEBCAM, HD, FrameRate::FPS_25);
        assert_eq!(
            (m.encoding, m.resolution, m.rate),
            (Encoding::Jpeg, HD, FrameRate::new(30, 1).unwrap())
        );
    }

    fn choose_mode(caps: &str, target: Resolution, master: FrameRate) -> CaptureMode {
        gst::init().unwrap();
        let caps = gst::Caps::from_str(caps).unwrap();
        choose(&modes(&caps, target, master), target, master).unwrap()
    }

    #[test]
    fn raw_is_preferred_when_it_reaches_the_rate() {
        let m = choose_mode(WEBCAM, r(640, 480), FrameRate::FPS_25);
        assert_eq!(m.encoding, Encoding::Raw("YUY2".into()));
        assert_eq!(m.rate, FrameRate::new(30, 1).unwrap());
    }

    #[test]
    fn closest_rate_at_or_above_the_master_else_the_highest_below() {
        // 720p: MJPEG 60 is the closest at or above 50.
        let m = choose_mode(WEBCAM, r(1280, 720), FrameRate::FPS_50);
        assert_eq!((m.encoding, m.rate), (Encoding::Jpeg, FrameRate::FPS_60));
        // 720p at 25: 30 (above) beats 60.
        let m = choose_mode(WEBCAM, r(1280, 720), FrameRate::FPS_25);
        assert_eq!(m.rate, FrameRate::new(30, 1).unwrap());
        // 1080p at 50: nothing above, the highest below (30).
        let m = choose_mode(WEBCAM, HD, FrameRate::FPS_50);
        assert_eq!(
            (m.encoding, m.rate),
            (Encoding::Jpeg, FrameRate::new(30, 1).unwrap())
        );
        // The exact master rate wins, even fractional.
        let m = choose_mode(
            "video/x-raw, format=NV12, width=1920, height=1080, framerate={ 30/1, 30000/1001, 25/1 }",
            HD,
            FrameRate::FPS_29_97,
        );
        assert_eq!(m.rate, FrameRate::FPS_29_97);
    }

    #[test]
    fn largest_size_that_fits_else_the_smallest() {
        let caps = "video/x-raw, format=YUY2, width={ 640, 1280, 2560 }, height={ 360, 720, 1440 }, framerate=30/1";
        // 1080p not offered: 1280×720 is the largest that fits (2560×… does not).
        assert_eq!(
            choose_mode(caps, HD, FrameRate::FPS_25).resolution,
            r(1280, 720)
        );
        // Nothing fits in 320×180: the smallest offered.
        assert_eq!(
            choose_mode(caps, r(320, 180), FrameRate::FPS_25).resolution,
            r(640, 360)
        );
    }

    #[test]
    fn uploaded_formats_first_and_ranges_are_clamped() {
        let m = choose_mode(
            "video/x-raw, format={ Y444, YUY2, NV12 }, width=1920, height=1080, framerate=50/1",
            HD,
            FrameRate::FPS_50,
        );
        assert_eq!(m.encoding, Encoding::Raw("NV12".into()));
        // A virtual camera with ranges: the Program size and the master rate.
        let m = choose_mode(
            "video/x-raw, format=I420, width=[ 16, 4096 ], height=[ 16, 4096 ], framerate=[ 1/1, 120/1 ]",
            HD,
            FrameRate::FPS_59_94,
        );
        assert_eq!((m.resolution, m.rate), (HD, FrameRate::FPS_59_94));
        assert_eq!(
            m.caps().to_string(),
            "video/x-raw, format=(string)I420, width=(int)1920, height=(int)1080, framerate=(fraction)60000/1001"
        );
    }

    #[test]
    fn unusable_caps_give_no_mode() {
        assert_eq!(
            pick(
                "video/x-h264, width=1920, height=1080, framerate=30/1",
                HD,
                FrameRate::FPS_25
            ),
            None
        );
        assert_eq!(pick("EMPTY", HD, FrameRate::FPS_25), None);
    }
}
