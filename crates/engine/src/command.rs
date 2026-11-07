//! The `Command` vocabulary: every change to the mixer goes through one of these
//! (spec `commands.md`). Serialisable for the journal and the future native API.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use vma_audio::SoloMode;
use vma_core::{AudioBus, FrameRate, InputId, InputKind, OutputId, Resolution, Rgb8};

use crate::output_spec::{InvalidSpec, OutputSpec, Secret, SrtMode, validate_srt};

/// Longest accepted transition, in milliseconds.
pub const MAX_TRANSITION_MS: u32 = 10_000;

/// Number of transition buttons (the T-Bar uses button 1).
pub const TRANSITION_BUTTONS: usize = 4;

/// Longest audio delay of an input, in milliseconds (`phase-3.md` § 5).
pub const MAX_AUDIO_DELAY_MS: u32 = 1_000;

/// A request to change the mixer state. Applied in arrival order on `vma-control`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    AddInput {
        spec: InputSpec,
        name: Option<String>,
    },
    RemoveInput {
        input: InputId,
    },
    SetPreview {
        input: InputId,
    },
    PreviewNext,
    PreviewPrevious,
    /// Preview ↔ Program, without transition. Refused while a transition runs.
    Cut,
    /// Sends an input straight to Program, Preview unchanged.
    CutDirect {
        input: InputId,
    },
    /// Transition of button `button` (1–4): Preview → Program with that button's effect.
    Transition {
        button: u8,
    },
    /// Quick Play transition. With `input`: that input goes to Program (Preview unchanged).
    QuickPlay {
        input: Option<InputId>,
    },
    /// T-Bar: 0 = Program alone, 255 = transition complete (effect of button 1).
    SetFader {
        value: u8,
    },
    SetTransition {
        button: u8,
        effect: Effect,
        duration_ms: u32,
    },
    SetQuickPlay {
        effect: Effect,
        duration_ms: u32,
    },
    SetMixDomain {
        domain: MixDomain,
    },
    RenameInput {
        input: InputId,
        name: String,
    },
    SetColor {
        input: InputId,
        color: Rgb8,
    },
    Play {
        input: InputId,
    },
    Pause {
        input: InputId,
    },
    Restart {
        input: InputId,
    },
    SetLoop {
        input: InputId,
        enabled: bool,
    },
    SetPosition {
        input: InputId,
        ms: u64,
    },
    SetAutoPlay(AutoPlay),
    // Outputs (Phase 2, `phase-2.md` § 5).
    AddOutput {
        spec: OutputSpec,
        name: Option<String>,
    },
    /// Refused while the output runs.
    UpdateOutput {
        output: OutputId,
        spec: OutputSpec,
    },
    /// Stops it first if it runs.
    RemoveOutput {
        output: OutputId,
    },
    StartOutput {
        output: OutputId,
    },
    StopOutput {
        output: OutputId,
    },
    /// Every recording output.
    StartRecording,
    StopRecording,
    /// Stream destination `slot` (1–3), or every one (`None`).
    StartStreaming {
        slot: Option<u8>,
    },
    StopStreaming {
        slot: Option<u8>,
    },
    /// Toggles the outputs between black and Program, over the FTB duration.
    FadeToBlack,
    SetFadeToBlackDuration {
        ms: u32,
    },
    // Audio (Phase 3, `phase-3.md` § 5). Refused for an input without sound.
    /// Fader of an input, from −∞ to +10 dB.
    SetInputGain {
        input: InputId,
        db: Db,
    },
    SetInputMute {
        input: InputId,
        mute: bool,
    },
    SetInputSolo {
        input: InputId,
        solo: bool,
    },
    /// Delays the sound of an input, 0 to [`MAX_AUDIO_DELAY_MS`].
    SetInputDelay {
        input: InputId,
        ms: u32,
    },
    /// Sends the input to `bus` (`on`) or not.
    SetInputBus {
        input: InputId,
        bus: AudioBus,
        on: bool,
    },
    SetAudioFollowVideo {
        input: InputId,
        on: bool,
    },
    SetBusGain {
        bus: AudioBus,
        db: Db,
    },
    SetBusMute {
        bus: AudioBus,
        mute: bool,
    },
    /// The bus an output carries. Refused while it runs, and for the full screen (no sound).
    SetOutputBus {
        output: OutputId,
        bus: AudioBus,
    },
    /// Headphones: device (`None`: the system's default) and what a solo hears.
    SetMonitor {
        device: Option<DeviceId>,
        solo_mode: SoloMode,
    },
}

/// A level in decibels: finite, or −∞ (silence). Never NaN, so it compares as `Eq`. The
/// journal writes −∞ as `"-inf"` (JSON has no infinity).
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct Db(f32);

impl Eq for Db {}

impl Db {
    pub const ZERO: Self = Self(0.0);
    pub const SILENT: Self = Self(f32::NEG_INFINITY);

    /// `None` for NaN and +∞.
    pub fn new(db: f32) -> Option<Self> {
        (db.is_finite() || db == f32::NEG_INFINITY).then_some(Self(db))
    }

    pub fn get(self) -> f32 {
        self.0
    }
}

impl fmt::Display for Db {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == f32::NEG_INFINITY {
            f.write_str("-∞ dB")
        } else {
            write!(f, "{:+.1} dB", self.0)
        }
    }
}

impl Serialize for Db {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.0 == f32::NEG_INFINITY {
            serializer.serialize_str("-inf")
        } else {
            serializer.serialize_f32(self.0)
        }
    }
}

impl<'de> Deserialize<'de> for Db {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Number(f32),
            Text(String),
        }
        let db = match Repr::deserialize(deserializer)? {
            Repr::Number(db) => db,
            Repr::Text(text) if text == "-inf" => f32::NEG_INFINITY,
            Repr::Text(text) => {
                return Err(serde::de::Error::custom(format!("not a level: {text}")));
            }
        };
        Self::new(db).ok_or_else(|| serde::de::Error::custom("not a level"))
    }
}

/// Stable identifier of a capture device, provided by `io-gst`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DeviceId(pub String);

/// Full description of an input source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputSpec {
    Capture {
        device: DeviceId,
    },
    File {
        path: PathBuf,
    },
    Image {
        path: PathBuf,
    },
    Color {
        color: Rgb8,
    },
    /// Synthetic source (tests, soaks): the compositor's test pattern, animated by the master
    /// frame index. `rate` and `resolution` are kept for the resampling tests of step 6; the
    /// pattern itself is drawn at the Program size and rate.
    TestSource {
        rate: FrameRate,
        resolution: Resolution,
    },
    /// Synthetic *external* source (soaks, rate tests): GStreamer `videotestsrc`, live, at its
    /// own rate and size, in NV12. Unlike [`Self::TestSource`] its pictures take the whole
    /// input path (pipeline, upload, resampling to the master rate).
    Synthetic {
        rate: FrameRate,
        resolution: Resolution,
    },
    /// Sync source (tests of Phase 3): a white picture once per second of its own clock and
    /// a 1 kHz beep of 100 ms on the same instant, `audio_lead_ms` earlier (negative: later),
    /// at its own frame rate and sample rate, its clock `drift_ppm` faster than the master.
    SyncSource {
        rate: FrameRate,
        sample_rate: u32,
        drift_ppm: i32,
        audio_lead_ms: i32,
        /// Size of its pictures; `None`: small (320×180), the compositor scales them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolution: Option<Resolution>,
    },
    /// Sound only: a microphone or line input (`io-gst` audio device, stable identifier).
    AudioDevice {
        device: DeviceId,
    },
    /// An SRT stream (MPEG-TS, pictures and sound) received as caller or listener.
    Srt(SrtSource),
    /// An NDI® source, by its name on the network (`MACHINE (Source)`), pictures and sound.
    Ndi {
        name: String,
    },
}

/// An SRT input (`phase-3.md` § 5; same fields as an SRT output).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SrtSource {
    pub mode: SrtMode,
    /// Sender to call (caller), or local address to listen on (listener; empty = all).
    pub host: String,
    pub port: u16,
    pub latency_ms: u32,
    /// Empty: no encryption; otherwise 10 to 79 characters. Written `***` everywhere.
    pub passphrase: Secret,
}

impl Default for SrtSource {
    /// Caller, 120 ms (`phase-3.md` § 6, like the output).
    fn default() -> Self {
        Self {
            mode: SrtMode::Caller,
            host: String::new(),
            port: 9000,
            latency_ms: 120,
            passphrase: Secret::none(),
        }
    }
}

impl SrtSource {
    /// `srt://host:port`, an IPv6 host in brackets. No secret in it.
    pub fn uri(&self) -> String {
        let host = self.host.trim();
        if host.contains(':') && !host.starts_with('[') {
            format!("srt://[{host}]:{}", self.port)
        } else {
            format!("srt://{host}:{}", self.port)
        }
    }
}

impl InputSpec {
    pub fn kind(&self) -> InputKind {
        match self {
            Self::Capture { .. } => InputKind::Capture,
            Self::File { .. } => InputKind::File,
            Self::Image { .. } => InputKind::Image,
            Self::Color { .. } => InputKind::Color,
            Self::TestSource { .. } | Self::Synthetic { .. } | Self::SyncSource { .. } => {
                InputKind::TestSource
            }
            Self::AudioDevice { .. } => InputKind::AudioDevice,
            Self::Srt(_) => InputKind::Srt,
            Self::Ndi { .. } => InputKind::Ndi,
        }
    }

    /// True for inputs that may carry sound: they get a strip in the audio mixer. A capture
    /// device without sound keeps a silent strip.
    pub fn has_audio(&self) -> bool {
        matches!(
            self,
            Self::Capture { .. }
                | Self::File { .. }
                | Self::SyncSource { .. }
                | Self::AudioDevice { .. }
                | Self::Srt(_)
                | Self::Ndi { .. }
        )
    }

    /// Checks the settings of a new input (an SRT input: like an SRT output).
    pub(crate) fn validate(&self) -> Result<(), InvalidSpec> {
        match self {
            Self::Srt(s) => validate_srt(s.mode, &s.host, s.port, s.latency_ms, &s.passphrase),
            Self::Ndi { name } if name.trim().is_empty() => Err(InvalidSpec::NdiName),
            _ => Ok(()),
        }
    }

    /// True for inputs the engine draws itself (no `io-gst` pipeline): colour, test pattern.
    pub fn is_generated(&self) -> bool {
        matches!(self, Self::Color { .. } | Self::TestSource { .. })
    }

    /// Name given to a new input when the command does not provide one.
    pub fn default_name(&self) -> String {
        let file_stem = |path: &PathBuf| {
            path.file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .filter(|s| !s.is_empty())
        };
        match self {
            Self::Capture { device } => device.0.clone(),
            Self::File { path } => file_stem(path).unwrap_or_else(|| "Vidéo".into()),
            Self::Image { path } => file_stem(path).unwrap_or_else(|| "Image".into()),
            Self::Color { .. } => "Couleur".into(),
            Self::TestSource { .. } => "Mire".into(),
            Self::Synthetic { rate, resolution } => format!("Synthétique {resolution} {rate}"),
            Self::SyncSource { rate, .. } => format!("Synchro {rate}"),
            Self::AudioDevice { .. } => "Son".into(),
            Self::Srt(srt) => format!("SRT {}", srt.uri().trim_start_matches("srt://")),
            // `MACHINE (Source)` → `Source`.
            Self::Ndi { name } => name
                .split_once(" (")
                .and_then(|(_, source)| source.strip_suffix(')'))
                .filter(|s| !s.is_empty())
                .unwrap_or(name)
                .to_owned(),
        }
    }
}

/// Transition effect available in Phase 1 (Wipe, Stinger… in Phase 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Effect {
    #[default]
    Fade,
    Cut,
}

/// Where a Fade mixes (ADR 0001 amendment 1, ADR 0005).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum MixDomain {
    /// Average of the encoded R'G'B' codes, like a broadcast mixer.
    #[default]
    Gamma,
    /// Mix in linear light.
    Linear,
}

/// Effect and duration of a transition button or of Quick Play.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransitionSetting {
    pub effect: Effect,
    pub duration_ms: u32,
}

impl TransitionSetting {
    pub const fn fade(duration_ms: u32) -> Self {
        Self {
            effect: Effect::Fade,
            duration_ms,
        }
    }

    pub const CUT: Self = Self {
        effect: Effect::Cut,
        duration_ms: 0,
    };

    /// Duration in frames at `rate`, or `None` when the transition is instantaneous (Cut
    /// effect or zero duration): `round(duration × rate)`, at least 1.
    pub fn frames(self, rate: FrameRate) -> Option<u32> {
        if self.effect == Effect::Cut || self.duration_ms == 0 {
            return None;
        }
        let num = u64::from(self.duration_ms) * u64::from(rate.num());
        let den = u64::from(rate.den()) * 1000;
        let frames = (2 * num + den) / (2 * den);
        Some(u32::try_from(frames).unwrap_or(u32::MAX).max(1))
    }
}

/// Auto-play options of video inputs, applied when they go to / leave Program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutoPlay {
    /// Start the incoming video when a transition begins.
    pub play_on_transition: bool,
    /// …from its beginning.
    pub restart_on_transition: bool,
    /// Pause the outgoing video when the transition ends.
    pub pause_after_transition: bool,
}

impl Default for AutoPlay {
    fn default() -> Self {
        Self {
            play_on_transition: true,
            restart_on_transition: false,
            pause_after_transition: true,
        }
    }
}

/// Who sent a command (journal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Ui,
    Api,
    Script,
}

/// Identifier of a command, unique within a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommandId(pub u64);

impl fmt::Display for CommandId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cmd#{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_to_nearest_at_every_master_rate() {
        // (rate, ms) → frames; 1 ms always yields one frame.
        let expect = [
            (FrameRate::FPS_25, [1, 1, 13, 25]),
            (FrameRate::FPS_29_97, [1, 1, 15, 30]),
            (FrameRate::FPS_30, [1, 1, 15, 30]),
            (FrameRate::FPS_50, [1, 2, 25, 50]),
            (FrameRate::FPS_59_94, [1, 2, 30, 60]),
            (FrameRate::FPS_60, [1, 2, 30, 60]),
        ];
        for (rate, frames) in expect {
            for (ms, want) in [1, 40, 500, 1000].into_iter().zip(frames) {
                assert_eq!(
                    TransitionSetting::fade(ms).frames(rate),
                    Some(want),
                    "{rate} {ms} ms"
                );
            }
            assert_eq!(TransitionSetting::fade(0).frames(rate), None);
            assert_eq!(TransitionSetting::CUT.frames(rate), None);
        }
    }

    #[test]
    fn journal_shape_of_commands() {
        let json = serde_json::to_string(&Command::Transition { button: 1 }).unwrap();
        assert_eq!(json, r#"{"Transition":{"button":1}}"#);
        assert_eq!(serde_json::to_string(&Command::Cut).unwrap(), r#""Cut""#);
        let add = Command::AddInput {
            spec: InputSpec::Color {
                color: Rgb8::new(255, 0, 0),
            },
            name: None,
        };
        let back: Command = serde_json::from_str(&serde_json::to_string(&add).unwrap()).unwrap();
        assert_eq!(back, add);
    }

    #[test]
    fn levels_in_the_journal() {
        let gain = |db| Command::SetInputGain {
            input: InputId(2),
            db,
        };
        let json = serde_json::to_string(&gain(Db::new(-6.5).unwrap())).unwrap();
        assert_eq!(json, r#"{"SetInputGain":{"input":2,"db":-6.5}}"#);
        let silent = serde_json::to_string(&gain(Db::SILENT)).unwrap();
        assert_eq!(silent, r#"{"SetInputGain":{"input":2,"db":"-inf"}}"#);
        for json in [&json, &silent] {
            let back: Command = serde_json::from_str(json).unwrap();
            assert_eq!(&serde_json::to_string(&back).unwrap(), json);
        }
        assert!(serde_json::from_str::<Db>(r#""loud""#).is_err());
        assert!(serde_json::from_str::<Db>("null").is_err());
        assert_eq!(Db::new(f32::NAN), None);
        assert_eq!(Db::new(f32::INFINITY), None);
        assert_eq!(Db::SILENT.to_string(), "-∞ dB");
        assert_eq!(Db::new(3.0).unwrap().to_string(), "+3.0 dB");
    }

    #[test]
    fn default_names() {
        let file = InputSpec::File {
            path: PathBuf::from("/media/clips/intro.mp4"),
        };
        assert_eq!(file.default_name(), "intro");
        assert_eq!(file.kind(), InputKind::File);
        let color = InputSpec::Color { color: Rgb8::BLACK };
        assert_eq!(color.default_name(), "Couleur");
    }

    #[test]
    fn ndi_inputs_are_named_after_their_source() {
        let ndi = |name: &str| InputSpec::Ndi { name: name.into() };
        assert_eq!(ndi("STUDIO (Caméra 1)").default_name(), "Caméra 1");
        assert_eq!(ndi("STUDIO (A (B))").default_name(), "A (B)");
        assert_eq!(ndi("Sans parenthèses").default_name(), "Sans parenthèses");
        assert_eq!(ndi("X").kind(), InputKind::Ndi);
        assert!(ndi("X").has_audio());
        assert!(ndi("X").validate().is_ok());
        assert_eq!(
            ndi("  ").validate(),
            Err(crate::output_spec::InvalidSpec::NdiName)
        );
    }
}
