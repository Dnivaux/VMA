//! What an output is (Phase 2 step 5, `phase-2.md` § 5): its settings, as the state, the
//! journal and the UI see them. `io-gst` turns them into pipelines.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use vma_core::{Container, VideoCodec};

/// Number of stream destinations (RTMP or SRT), started together or one by one.
pub const STREAM_SLOTS: u8 = 3;

/// Most outputs configured at once (recordings and streams).
pub const MAX_OUTPUTS: usize = 8;

/// Accepted video bit rates, kbit/s.
pub const VIDEO_KBPS_RANGE: std::ops::RangeInclusive<u32> = 100..=200_000;

/// Accepted SRT latencies, milliseconds.
pub const SRT_LATENCY_RANGE: std::ops::RangeInclusive<u32> = 20..=8_000;

/// A stream key or an SRT passphrase. Never written out: `Serialize` and `Debug` give `***`
/// (journal, logs, reports, the future API). Only `io-gst` reads it, to open the pipeline.
///
/// A journal read back gives [`Secret::Redacted`]: replaying it needs the secret again
/// (starting such an output is refused).
#[derive(Clone, PartialEq, Eq, Default)]
pub enum Secret {
    /// The value; empty = no secret (a server without key, SRT without encryption).
    Value(String),
    /// Read back from a journal: the value was not written.
    #[default]
    Redacted,
}

/// What a non-empty secret is written as.
const REDACTED: &str = "***";

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self::Value(value.into())
    }

    /// No secret at all.
    pub fn none() -> Self {
        Self::Value(String::new())
    }

    /// The value, for the pipeline only; `None` if it was redacted.
    pub fn expose(&self) -> Option<&str> {
        match self {
            Self::Value(v) => Some(v),
            Self::Redacted => None,
        }
    }

    /// Empty: nothing secret.
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Value(v) if v.is_empty())
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Value(v) if v.is_empty() => f.write_str("Secret(none)"),
            Self::Value(_) => write!(f, "Secret({REDACTED})"),
            Self::Redacted => f.write_str("Secret(redacted)"),
        }
    }
}

impl Serialize for Secret {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(if self.is_empty() { "" } else { REDACTED })
    }
}

impl<'de> Deserialize<'de> for Secret {
    /// `"***"` gives [`Secret::Redacted`]; any other string is the value (a hand-written
    /// script may carry one).
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Ok(if value == REDACTED {
            Self::Redacted
        } else {
            Self::Value(value)
        })
    }
}

/// Full description of an output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputSpec {
    Record(RecordSpec),
    Rtmp(RtmpSpec),
    Srt(SrtSpec),
    /// The Program, full screen on a screen of this computer (Phase 2 step 8). Shown by the
    /// UI, not by `io-gst`.
    Fullscreen(FullscreenSpec),
    /// The Program and a bus as an NDI® source on the network, uncompressed (Phase 3 step 8).
    Ndi(NdiSpec),
}

/// The NDI® output (one at most).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NdiSpec {
    /// Name announced on the network (NDI adds the machine's: `MACHINE (VMA Program)`).
    pub name: String,
}

impl Default for NdiSpec {
    fn default() -> Self {
        Self {
            name: "VMA Program".into(),
        }
    }
}

/// The full-screen output.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct FullscreenSpec {
    /// Name of the screen, as the windowing system gives it (« DP-2 », « HDMI-A-1 »…);
    /// `None`: the first screen of the list. A screen that is not connected leaves the output
    /// on, not shown, until it comes back.
    pub screen: Option<String>,
}

/// A local recording (defaults: `phase-2.md` § 6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordSpec {
    /// `None`: the user's Videos folder (created if needed).
    pub folder: Option<PathBuf>,
    pub codec: VideoCodec,
    pub container: Container,
    /// `None`: ≈ 40 Mb/s at 1080p50, scaled with the pixel rate.
    pub video_kbps: Option<u32>,
}

impl Default for RecordSpec {
    fn default() -> Self {
        Self {
            folder: None,
            codec: VideoCodec::H264,
            container: Container::Mkv,
            video_kbps: None,
        }
    }
}

/// An RTMP or RTMPS destination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RtmpSpec {
    /// Server address, `rtmp://` or `rtmps://`, without the key.
    pub url: String,
    pub key: Secret,
    /// `None`: 6 Mb/s at 1080p50 (4.5 Mb/s at 30p), scaled with the pixel rate.
    pub video_kbps: Option<u32>,
}

/// Who opens an SRT connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SrtMode {
    /// VMA calls a listening receiver (default).
    #[default]
    Caller,
    /// VMA waits for a receiver to call.
    Listener,
}

/// An SRT output (MPEG-TS).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SrtSpec {
    pub mode: SrtMode,
    /// Receiver to call (caller), or local address to listen on (listener; empty = all).
    pub host: String,
    pub port: u16,
    pub latency_ms: u32,
    /// Empty: no encryption; otherwise 10 to 79 characters (SRT).
    pub passphrase: Secret,
    /// `None`: 8 Mb/s at 1080p50, scaled with the pixel rate.
    pub video_kbps: Option<u32>,
}

impl Default for SrtSpec {
    fn default() -> Self {
        Self {
            mode: SrtMode::Caller,
            host: String::new(),
            port: 9000,
            latency_ms: 120,
            passphrase: Secret::none(),
            video_kbps: None,
        }
    }
}

/// Why output settings are refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InvalidSpec {
    #[error("video bit rate {0} kbit/s is out of range (100–200000)")]
    Bitrate(u32),
    #[error("the stream address must start with rtmp:// or rtmps://")]
    RtmpUrl,
    #[error("the SRT receiver address is missing")]
    SrtHost,
    #[error("the SRT port must not be 0")]
    SrtPort,
    #[error("SRT latency {0} ms is out of range (20–8000)")]
    SrtLatency(u32),
    #[error("the SRT passphrase must have 10 to 79 characters")]
    SrtPassphrase,
    #[error("the screen name is empty")]
    Screen,
    #[error("the NDI® name is empty")]
    NdiName,
}

impl OutputSpec {
    /// Streams (RTMP, SRT) take one of the [`STREAM_SLOTS`] destinations.
    pub fn is_stream(&self) -> bool {
        matches!(self, Self::Rtmp(_) | Self::Srt(_))
    }

    /// Shown by the UI on a screen: no `io-gst` pipeline, no encoder.
    pub fn is_fullscreen(&self) -> bool {
        matches!(self, Self::Fullscreen(_))
    }

    pub fn is_ndi(&self) -> bool {
        matches!(self, Self::Ndi(_))
    }

    /// Recording, stream, full screen and NDI® cannot turn into one another.
    pub(crate) fn same_kind(&self, other: &Self) -> bool {
        self.is_recording() == other.is_recording()
            && self.is_stream() == other.is_stream()
            && self.is_fullscreen() == other.is_fullscreen()
            && self.is_ndi() == other.is_ndi()
    }

    pub fn is_recording(&self) -> bool {
        matches!(self, Self::Record(_))
    }

    /// The secret of the spec is not known (read back from a journal).
    pub fn secret_missing(&self) -> bool {
        match self {
            Self::Record(_) | Self::Fullscreen(_) | Self::Ndi(_) => false,
            Self::Rtmp(rtmp) => rtmp.key.expose().is_none(),
            Self::Srt(srt) => srt.passphrase.expose().is_none(),
        }
    }

    /// Name given to a new output when the command does not provide one.
    pub fn default_name(&self, slot: Option<u8>) -> String {
        match (self, slot) {
            (Self::Record(_), _) => "Enregistrement".into(),
            (Self::Fullscreen(_), _) => "Plein écran".into(),
            (Self::Ndi(_), _) => "NDI®".into(),
            (_, Some(slot)) => format!("Stream {slot}"),
            (_, None) => "Stream".into(),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), InvalidSpec> {
        let kbps = match self {
            Self::Record(r) => r.video_kbps,
            Self::Rtmp(r) => r.video_kbps,
            Self::Srt(s) => s.video_kbps,
            Self::Fullscreen(_) | Self::Ndi(_) => None,
        };
        if let Some(kbps) = kbps
            && !VIDEO_KBPS_RANGE.contains(&kbps)
        {
            return Err(InvalidSpec::Bitrate(kbps));
        }
        match self {
            Self::Record(_) => Ok(()),
            Self::Ndi(ndi) if ndi.name.trim().is_empty() => Err(InvalidSpec::NdiName),
            Self::Ndi(_) => Ok(()),
            Self::Fullscreen(f) => match &f.screen {
                Some(name) if name.trim().is_empty() => Err(InvalidSpec::Screen),
                _ => Ok(()),
            },
            Self::Rtmp(rtmp) => {
                let url = rtmp.url.trim().to_ascii_lowercase();
                let host = url
                    .strip_prefix("rtmp://")
                    .or_else(|| url.strip_prefix("rtmps://"));
                match host {
                    Some(rest) if !rest.is_empty() => Ok(()),
                    _ => Err(InvalidSpec::RtmpUrl),
                }
            }
            Self::Srt(srt) => validate_srt(
                srt.mode,
                &srt.host,
                srt.port,
                srt.latency_ms,
                &srt.passphrase,
            ),
        }
    }
}

/// The rules of an SRT link, output or input: a caller has a host, a port, a latency of 20 to
/// 8 000 ms, no passphrase or one of 10 to 79 characters.
pub(crate) fn validate_srt(
    mode: SrtMode,
    host: &str,
    port: u16,
    latency_ms: u32,
    passphrase: &Secret,
) -> Result<(), InvalidSpec> {
    if mode == SrtMode::Caller && host.trim().is_empty() {
        return Err(InvalidSpec::SrtHost);
    }
    if port == 0 {
        return Err(InvalidSpec::SrtPort);
    }
    if !SRT_LATENCY_RANGE.contains(&latency_ms) {
        return Err(InvalidSpec::SrtLatency(latency_ms));
    }
    match passphrase.expose() {
        Some(p) if !p.is_empty() && !(10..=79).contains(&p.chars().count()) => {
            Err(InvalidSpec::SrtPassphrase)
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_never_written_out() {
        let key = Secret::new("live_2f9c8e1b");
        assert_eq!(serde_json::to_string(&key).unwrap(), r#""***""#);
        assert_eq!(format!("{key:?}"), "Secret(***)");
        assert_eq!(key.expose(), Some("live_2f9c8e1b"));
        let spec = OutputSpec::Rtmp(RtmpSpec {
            url: "rtmp://live.example.net/app".into(),
            key,
            video_kbps: None,
        });
        let json = serde_json::to_string(&spec).unwrap();
        assert!(!json.contains("live_2f9c8e1b"), "{json}");
        assert!(!format!("{spec:?}").contains("live_2f9c8e1b"));

        // Read back: redacted, so starting it again needs the key.
        let back: OutputSpec = serde_json::from_str(&json).unwrap();
        assert!(back.secret_missing());
        assert!(!spec.secret_missing());
        // No secret at all stays empty (nothing to ask for).
        let none = serde_json::to_string(&Secret::none()).unwrap();
        assert_eq!(none, r#""""#);
        assert!(serde_json::from_str::<Secret>(&none).unwrap().is_empty());
    }

    #[test]
    fn invalid_settings_are_refused() {
        let rtmp = |url: &str| {
            OutputSpec::Rtmp(RtmpSpec {
                url: url.into(),
                key: Secret::none(),
                video_kbps: None,
            })
            .validate()
        };
        assert_eq!(rtmp("rtmp://a/app"), Ok(()));
        assert_eq!(rtmp("RTMPS://a/app"), Ok(()));
        assert_eq!(rtmp("http://a/app"), Err(InvalidSpec::RtmpUrl));
        assert_eq!(rtmp("rtmp://"), Err(InvalidSpec::RtmpUrl));
        let srt = |f: fn(&mut SrtSpec)| {
            let mut spec = SrtSpec {
                host: "10.0.0.2".into(),
                ..SrtSpec::default()
            };
            f(&mut spec);
            OutputSpec::Srt(spec).validate()
        };
        assert_eq!(srt(|_| {}), Ok(()));
        assert_eq!(srt(|s| s.host.clear()), Err(InvalidSpec::SrtHost));
        assert_eq!(
            srt(|s| {
                s.host.clear();
                s.mode = SrtMode::Listener;
            }),
            Ok(())
        );
        assert_eq!(srt(|s| s.port = 0), Err(InvalidSpec::SrtPort));
        assert_eq!(srt(|s| s.latency_ms = 5), Err(InvalidSpec::SrtLatency(5)));
        assert_eq!(
            srt(|s| s.passphrase = Secret::new("short")),
            Err(InvalidSpec::SrtPassphrase)
        );
        assert_eq!(srt(|s| s.passphrase = Secret::new("long enough")), Ok(()));
        let record = OutputSpec::Record(RecordSpec {
            video_kbps: Some(50),
            ..RecordSpec::default()
        });
        assert_eq!(record.validate(), Err(InvalidSpec::Bitrate(50)));
    }
}
