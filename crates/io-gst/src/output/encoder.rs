//! Encoder ladder (ADR 0006 § 6): the first element of the ladder that exists **and** reaches
//! `READY` is used; the ones skipped before it are kept with their reason (UI, reports). Never
//! a GPL encoder (`x264enc`, `x265enc`): HEVC has no software fallback.

use gstreamer as gst;
use gstreamer::prelude::*;
use vma_core::VideoCodec;

/// How an encoder family names its settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Family {
    Nvenc,
    Qsv,
    Va,
    MediaFoundation,
    OpenH264,
    /// AAC encoders (`avenc_aac`, `voaacenc`, `mfaacenc`): bit rate in bit/s.
    Aac,
}

/// One rung of a ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Candidate {
    pub element: &'static str,
    pub family: Family,
}

const fn rung(element: &'static str, family: Family) -> Candidate {
    Candidate { element, family }
}

pub(crate) const H264: &[Candidate] = &[
    rung("nvh264enc", Family::Nvenc),
    rung("nvd3d11h264enc", Family::Nvenc),
    rung("qsvh264enc", Family::Qsv),
    rung("vah264enc", Family::Va),
    rung("mfh264enc", Family::MediaFoundation),
    rung("openh264enc", Family::OpenH264),
];

pub(crate) const HEVC: &[Candidate] = &[
    rung("nvh265enc", Family::Nvenc),
    rung("nvd3d11h265enc", Family::Nvenc),
    rung("qsvh265enc", Family::Qsv),
    rung("vah265enc", Family::Va),
    rung("mfh265enc", Family::MediaFoundation),
];

pub(crate) const AAC: &[Candidate] = &[
    rung("avenc_aac", Family::Aac),
    rung("voaacenc", Family::Aac),
    rung("mfaacenc", Family::Aac),
];

pub(crate) fn video_ladder(codec: VideoCodec) -> &'static [Candidate] {
    match codec {
        VideoCodec::H264 => H264,
        VideoCodec::Hevc => HEVC,
    }
}

/// An encoder that was not retained, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub element: &'static str,
    pub reason: String,
}

/// The encoder an output uses, and the ones tried before it (empty: first choice).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderChoice {
    pub element: &'static str,
    pub skipped: Vec<Skipped>,
}

impl EncoderChoice {
    /// A fallback: a better encoder of the ladder was not usable.
    pub fn is_fallback(&self) -> bool {
        !self.skipped.is_empty()
    }
}

impl std::fmt::Display for EncoderChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.element)?;
        if self.is_fallback() {
            write!(f, " (fallback: {})", list(&self.skipped))?;
        }
        Ok(())
    }
}

/// `a: reason, b: reason`.
pub(crate) fn list(skipped: &[Skipped]) -> String {
    skipped
        .iter()
        .map(|s| format!("{}: {}", s.element, s.reason))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Walks `ladder` and returns the first rung `probe` accepts, or every refusal.
pub(crate) fn select<T>(
    ladder: &[Candidate],
    mut probe: impl FnMut(&'static str) -> Result<T, String>,
) -> Result<(T, Candidate, EncoderChoice), Vec<Skipped>> {
    let mut skipped = Vec::new();
    for &candidate in ladder {
        match probe(candidate.element) {
            Ok(found) => {
                let choice = EncoderChoice {
                    element: candidate.element,
                    skipped,
                };
                return Ok((found, candidate, choice));
            }
            Err(reason) => skipped.push(Skipped {
                element: candidate.element,
                reason,
            }),
        }
    }
    Err(skipped)
}

/// The real probe: the element exists and reaches `READY` (it opens its device there). Given
/// back in `NULL`, ready to be added to a pipeline.
pub(crate) fn probe_element(name: &'static str) -> Result<gst::Element, String> {
    let element = gst::ElementFactory::make(name)
        .build()
        .map_err(|_| "not installed".to_owned())?;
    let ready = element.set_state(gst::State::Ready);
    let _ = element.set_state(gst::State::Null);
    ready
        .map(|_| element)
        .map_err(|e| format!("does not start ({e})"))
}

/// Video encoder settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct VideoTuning {
    pub kbps: u32,
    /// Constant bit rate (streaming); variable otherwise (recording).
    pub cbr: bool,
    /// Frames between key frames.
    pub gop: u32,
}

/// Applies `tuning` with each family's property names. No B-frames anywhere: no reordering
/// delay, and fewer pictures held by the encoder (they come from the engine's shared pool).
pub(crate) fn tune_video(element: &gst::Element, family: Family, tuning: VideoTuning) {
    let kbps = tuning.kbps.to_string();
    let gop = tuning.gop.to_string();
    let rc = if tuning.cbr { "cbr" } else { "vbr" };
    let settings: &[(&str, &str)] = match family {
        Family::Nvenc => &[
            ("bitrate", &kbps),
            ("gop-size", &gop),
            ("bframes", "0"),
            ("rc-mode", rc),
        ],
        Family::Qsv => &[
            ("bitrate", &kbps),
            ("gop-size", &gop),
            ("b-frames", "0"),
            ("rate-control", rc),
        ],
        Family::Va => &[
            ("bitrate", &kbps),
            ("key-int-max", &gop),
            ("b-frames", "0"),
            ("rate-control", rc),
        ],
        Family::MediaFoundation => &[("bitrate", &kbps), ("gop-size", &gop), ("bframes", "0")],
        Family::OpenH264 => {
            let bps = (u64::from(tuning.kbps) * 1000).to_string();
            for (name, value) in [
                ("bitrate", bps.as_str()),
                ("gop-size", &gop),
                ("rate-control", "bitrate"),
            ] {
                set(element, name, value);
            }
            return;
        }
        Family::Aac => &[],
    };
    for (name, value) in settings {
        set(element, name, value);
    }
}

/// Applies the AAC bit rate.
pub(crate) fn tune_audio(element: &gst::Element, kbps: u32) {
    set(element, "bitrate", &(u64::from(kbps) * 1000).to_string());
}

/// Sets `name` from its string form if the element has it and the value fits; logs otherwise
/// (the element keeps its default — never a panic over a property).
pub(crate) fn set(element: &gst::Element, name: &str, value: &str) {
    let factory = element
        .factory()
        .map(|f| f.name().to_string())
        .unwrap_or_default();
    let Some(pspec) = element.find_property(name) else {
        tracing::debug!(element = %factory, property = name, "encoder has no such property");
        return;
    };
    let flags = pspec.flags();
    let writable = flags.contains(gst::glib::ParamFlags::WRITABLE)
        && !flags.contains(gst::glib::ParamFlags::CONSTRUCT_ONLY);
    // Out of range: GLib warns and leaves the property as it was.
    let parsed = gst::glib::Value::deserialize(value, pspec.value_type())
        .ok()
        .filter(|_| writable);
    match parsed {
        Some(v) => element.set_property_from_value(name, &v),
        None => {
            tracing::warn!(element = %factory, property = name, value, "encoder setting refused")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present(
        available: &'static [&'static str],
    ) -> impl FnMut(&'static str) -> Result<(), String> {
        move |name| {
            if available.contains(&name) {
                Ok(())
            } else {
                Err("not installed".into())
            }
        }
    }

    #[test]
    fn h264_without_hardware_falls_back_to_openh264() {
        let (_, candidate, choice) = select(H264, present(&["openh264enc"])).unwrap();
        assert_eq!(candidate.family, Family::OpenH264);
        assert_eq!(choice.element, "openh264enc");
        assert!(choice.is_fallback());
        let tried: Vec<_> = choice.skipped.iter().map(|s| s.element).collect();
        assert_eq!(
            tried,
            [
                "nvh264enc",
                "nvd3d11h264enc",
                "qsvh264enc",
                "vah264enc",
                "mfh264enc"
            ]
        );
        assert!(
            choice
                .to_string()
                .starts_with("openh264enc (fallback: nvh264enc: not installed")
        );
    }

    #[test]
    fn first_usable_rung_wins_and_refusals_keep_their_reason() {
        let mut probe = |name: &'static str| match name {
            "nvh264enc" => Err("does not start (no device)".to_owned()),
            "qsvh264enc" | "openh264enc" => Ok(()),
            _ => Err("not installed".to_owned()),
        };
        let (_, _, choice) = select(H264, &mut probe).unwrap();
        assert_eq!(choice.element, "qsvh264enc");
        assert_eq!(
            choice.skipped,
            [
                Skipped {
                    element: "nvh264enc",
                    reason: "does not start (no device)".into()
                },
                Skipped {
                    element: "nvd3d11h264enc",
                    reason: "not installed".into()
                },
            ]
        );
        let (_, _, first) = select(H264, present(&["nvh264enc"])).unwrap();
        assert!(!first.is_fallback());
        assert_eq!(first.to_string(), "nvh264enc");
    }

    #[test]
    fn hevc_has_no_software_fallback() {
        let tried = select(HEVC, present(&["openh264enc", "x265enc"])).unwrap_err();
        assert_eq!(tried.len(), HEVC.len());
        for ladder in [H264, HEVC, AAC] {
            assert!(
                ladder.iter().all(|c| !c.element.starts_with('x')),
                "no GPL encoder"
            );
        }
    }
}
