//! The `Command` vocabulary: every change to the mixer goes through one of these
//! (spec `commands.md`). Serialisable for the journal and the future native API.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use vma_core::{FrameRate, InputId, InputKind, Resolution, Rgb8};

/// Longest accepted transition, in milliseconds.
pub const MAX_TRANSITION_MS: u32 = 10_000;

/// Number of transition buttons (the T-Bar uses button 1).
pub const TRANSITION_BUTTONS: usize = 4;

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
}

impl InputSpec {
    pub fn kind(&self) -> InputKind {
        match self {
            Self::Capture { .. } => InputKind::Capture,
            Self::File { .. } => InputKind::File,
            Self::Image { .. } => InputKind::Image,
            Self::Color { .. } => InputKind::Color,
            Self::TestSource { .. } => InputKind::TestSource,
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
    fn default_names() {
        let file = InputSpec::File {
            path: PathBuf::from("/media/clips/intro.mp4"),
        };
        assert_eq!(file.default_name(), "intro");
        assert_eq!(file.kind(), InputKind::File);
        let color = InputSpec::Color { color: Rgb8::BLACK };
        assert_eq!(color.default_name(), "Couleur");
    }
}
