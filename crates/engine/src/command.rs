//! The `Command` vocabulary: every change to the mixer goes through one of these
//! (spec `commands.md`). Serialisable for the journal and the future native API.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use vma_audio::SoloMode;
use vma_core::{
    AudioBus, FrameRate, InputId, InputKind, OutputId, OverlayChannel, Resolution, Rgb8,
};

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
    /// A transition of this effect and duration, outside the buttons (the compat API's `Fade`,
    /// `Cut` with a `Duration`): the Preview, or `input` directly (as Quick Play).
    Mix {
        effect: Effect,
        duration_ms: u32,
        input: Option<InputId>,
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
    // Overlays (Phase 4, `phase-4.md` § 5, ADR 0010). Refused for an input without pictures.
    /// `input` enters `channel` with the channel's transition, replacing the input there.
    OverlayIn {
        channel: OverlayChannel,
        input: InputId,
    },
    /// The input of `channel` leaves with the channel's transition.
    OverlayOut {
        channel: OverlayChannel,
    },
    /// The input of `channel` leaves at once, whatever the transition.
    OverlayOff {
        channel: OverlayChannel,
    },
    /// Out if `input` is in `channel`, else in.
    OverlayToggle {
        channel: OverlayChannel,
        input: InputId,
    },
    /// Every channel off at once.
    OverlayAllOff,
    /// Entry and exit transition of `channel`: Cut or Fade.
    SetOverlayTransition {
        channel: OverlayChannel,
        effect: Effect,
        duration_ms: u32,
    },
    /// Stinger `slot` (Phase 4 step 8): its file (`None` empties the slot; a new one is
    /// preloaded by `vma-io`) and where it cuts from A to B, in ms from its start (`None`: half
    /// the file). Refused while that stinger plays.
    SetStinger {
        slot: StingerSlot,
        file: Option<PathBuf>,
        cut_point_ms: Option<u32>,
    },
    // HTML inputs (Phase 4 step 7, `phase-4.md` § 5). Refused for any other input. None
    // reloads the page except `HtmlReload` and `SetHtmlUrl`; all are journaled in full.
    /// DOM event `vma-data` in the page, `detail` = this JSON (checked).
    HtmlData {
        input: InputId,
        json: String,
    },
    /// JavaScript run in the page as is.
    HtmlScript {
        input: InputId,
        script: String,
    },
    HtmlReload {
        input: InputId,
    },
    /// Loads another page in the input (kept for its next openings).
    SetHtmlUrl {
        input: InputId,
        url: HtmlUrl,
    },
    /// Previous page of the input's history.
    HtmlBack {
        input: InputId,
    },
    /// Next page of the input's history.
    HtmlForward {
        input: InputId,
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
    /// Seed of the keys given to new inputs: sent once at start-up, so that a journal replays
    /// to the same keys (ADR 0013).
    SeedInputKeys {
        seed: u64,
    },
    /// The key of an input, as a project gives it back. Refused if another input has it.
    SetInputKey {
        input: InputId,
        key: InputKey,
    },
    /// Replaces the show by a project's (ADR 0013), as the commands that build it would: each
    /// part checked as such, the parts refused listed in [`State::load_problems`](crate::State).
    /// Refused as a whole while a transition runs or an output is active.
    LoadProject {
        project: Box<crate::project::ProjectSnapshot>,
    },
}

/// The stable key of an input (ADR 0013): a GUID, by which the compat API and a project name
/// it. Written in its usual form, `xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct InputKey(u128);

impl InputKey {
    /// The key of input `id` under `seed`: random-looking, the same for the same pair, with
    /// the version (4) and variant bits of a random GUID.
    pub fn derive(seed: u64, id: InputId) -> Self {
        let high = splitmix64(seed ^ u64::from(id.0).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let low = splitmix64(high ^ seed.rotate_left(32));
        let raw = (u128::from(high) << 64) | u128::from(low);
        let versioned = (raw & !(0xF << 76)) | (0x4 << 76);
        Self((versioned & !(0b11 << 62)) | (0b10 << 62))
    }

    pub fn from_u128(value: u128) -> Self {
        Self(value)
    }
}

fn splitmix64(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl fmt::Display for InputKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let v = self.0;
        write!(
            f,
            "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
            v >> 96,
            (v >> 80) & 0xFFFF,
            (v >> 64) & 0xFFFF,
            (v >> 48) & 0xFFFF,
            v & 0xFFFF_FFFF_FFFF
        )
    }
}

/// A key that is not a GUID.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not an input key (a GUID): {0:?}")]
pub struct InvalidKey(pub String);

impl std::str::FromStr for InputKey {
    type Err = InvalidKey;

    /// The usual form, any case, braces allowed.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || InvalidKey(text.to_owned());
        let bare = text
            .strip_prefix('{')
            .and_then(|t| t.strip_suffix('}'))
            .unwrap_or(text);
        let groups: Vec<&str> = bare.split('-').collect();
        let lengths = [8, 4, 4, 4, 12];
        if groups.len() != lengths.len() || groups.iter().zip(lengths).any(|(g, n)| g.len() != n) {
            return Err(invalid());
        }
        let hex: String = groups.concat();
        if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(invalid());
        }
        u128::from_str_radix(&hex, 16)
            .map(Self)
            .map_err(|_| invalid())
    }
}

impl Serialize for InputKey {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for InputKey {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
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
    /// A web page rendered off screen with its alpha (Phase 4, ADR 0009), at the master rate;
    /// `size`: `None` = the Program's; `css`: style sheet added to every page it loads.
    Html {
        url: HtmlUrl,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<Resolution>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        css: Option<String>,
    },
}

/// Longest JSON, script or style sheet sent to an HTML input, in bytes.
pub const MAX_HTML_TEXT: usize = 64 * 1024;

/// Address of an HTML input: `http://`, `https://` or `file://`. Credentials in it
/// (`user:pass@`) are never written out: `Serialize` and `Debug` give `***` in their place
/// (journal, logs, reports), like a [`Secret`]; replaying such a journal opens the page without
/// them.
#[derive(Clone, PartialEq, Eq)]
pub struct HtmlUrl(String);

impl HtmlUrl {
    pub fn new(url: impl Into<String>) -> Self {
        Self(url.into().trim().to_owned())
    }

    /// The address as given, for `vma-io` only.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The address with its credentials, if any, replaced by `***`.
    pub fn redacted(&self) -> String {
        let Some((scheme, rest)) = self.0.split_once("://") else {
            return self.0.clone();
        };
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let (authority, path) = rest.split_at(authority_end);
        match authority.rsplit_once('@') {
            Some((_, host)) => format!("{scheme}://***@{host}{path}"),
            None => self.0.clone(),
        }
    }

    pub(crate) fn validate(&self) -> Result<(), InvalidSpec> {
        match self.0.split_once("://") {
            Some((scheme, rest))
                if !rest.is_empty()
                    && ["http", "https", "file"]
                        .iter()
                        .any(|s| scheme.eq_ignore_ascii_case(s)) =>
            {
                Ok(())
            }
            _ => Err(InvalidSpec::HtmlUrl),
        }
    }
}

impl fmt::Debug for HtmlUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HtmlUrl({})", self.redacted())
    }
}

impl Serialize for HtmlUrl {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.redacted())
    }
}

impl<'de> Deserialize<'de> for HtmlUrl {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::new)
    }
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
            Self::Html { .. } => InputKind::Html,
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
                | Self::Html { .. }
        )
    }

    /// Checks the settings of a new input (an SRT input: like an SRT output).
    pub(crate) fn validate(&self) -> Result<(), InvalidSpec> {
        match self {
            Self::Srt(s) => validate_srt(s.mode, &s.host, s.port, s.latency_ms, &s.passphrase),
            Self::Ndi { name } if name.trim().is_empty() => Err(InvalidSpec::NdiName),
            Self::Html { url, css, .. } => {
                url.validate()?;
                match css {
                    Some(css) if css.len() > MAX_HTML_TEXT => Err(InvalidSpec::HtmlTooLong),
                    _ => Ok(()),
                }
            }
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
            Self::Html { url, .. } => html_name(url.expose()),
        }
    }
}

/// `https://scores.example/live/board.html?x=1` → `board`; a page without a file name gives its
/// host; `HTML` when there is neither.
fn html_name(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let path = rest.split(['?', '#']).next().unwrap_or(rest);
    let (authority, path) = path.split_once('/').unwrap_or((path, ""));
    let file = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("");
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    [stem, host]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or("HTML")
        .to_owned()
}

/// Transition effect: Fade and Cut (Phase 1), Stinger (Phase 4, ADR 0010 § 5): a video
/// with alpha played over the Program, which cuts from A to B under it at the stinger's cut
/// point; it lasts as long as the file (the setting's duration is not used).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Effect {
    #[default]
    Fade,
    Cut,
    Stinger(StingerSlot),
}

/// One of the two stinger slots (`phase-4.md` Q7), 1 or 2. Written as its number; any other
/// number is refused when read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct StingerSlot(u8);

impl StingerSlot {
    pub const COUNT: usize = 2;
    pub const ALL: [Self; Self::COUNT] = [Self(1), Self(2)];

    pub fn new(number: u8) -> Option<Self> {
        (1..=2).contains(&number).then_some(Self(number))
    }

    pub fn number(self) -> u8 {
        self.0
    }

    /// 0 or 1.
    pub fn index(self) -> usize {
        usize::from(self.0.saturating_sub(1)).min(Self::COUNT - 1)
    }
}

impl fmt::Display for StingerSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "stinger {}", self.0)
    }
}

impl<'de> Deserialize<'de> for StingerSlot {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let n = u8::deserialize(deserializer)?;
        Self::new(n).ok_or_else(|| serde::de::Error::custom(format!("no stinger {n} (1–2)")))
    }
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
    /// effect or zero duration): `round(duration × rate)`, at least 1. A stinger's length is
    /// its file's (`State`), not this.
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

    fn html(url: &str) -> InputSpec {
        InputSpec::Html {
            url: HtmlUrl::new(url),
            size: None,
            css: None,
        }
    }

    #[test]
    fn html_inputs_take_web_and_file_addresses_only() {
        use crate::output_spec::InvalidSpec;
        for ok in [
            "https://scores.example/board.html",
            "http://127.0.0.1:8080/",
            "file:///home/me/overlay.html",
            "FILE:///C:/titres/bas.html",
        ] {
            assert_eq!(html(ok).validate(), Ok(()), "{ok}");
        }
        for bad in [
            "",
            "overlay.html",
            "ftp://x/y",
            "javascript:alert(1)",
            "https://",
        ] {
            assert_eq!(html(bad).validate(), Err(InvalidSpec::HtmlUrl), "{bad:?}");
        }
        assert_eq!(html("file:///a.html").kind(), InputKind::Html);
        assert!(
            html("file:///a.html").has_audio(),
            "its page's sound (step 9)"
        );
    }

    #[test]
    fn html_inputs_are_named_after_their_page() {
        assert_eq!(
            html("https://scores.example/live/board.html?x=1#y").default_name(),
            "board"
        );
        assert_eq!(
            html("file:///home/me/bas-de-titre.html").default_name(),
            "bas-de-titre"
        );
        assert_eq!(
            html("https://user:pw@scores.example/").default_name(),
            "scores.example"
        );
        assert_eq!(
            html("https://scores.example").default_name(),
            "scores.example"
        );
    }

    /// Credentials in a page address never reach the journal, the logs or a report.
    #[test]
    fn credentials_in_an_html_address_are_never_written() {
        let url = HtmlUrl::new("https://op:s3cret@scores.example/board.html?token=1");
        assert_eq!(
            url.redacted(),
            "https://***@scores.example/board.html?token=1"
        );
        assert!(!format!("{url:?}").contains("s3cret"));
        let spec = InputSpec::Html {
            url: url.clone(),
            size: Some(Resolution::HD_720),
            css: None,
        };
        let json = serde_json::to_string(&spec).unwrap();
        assert!(!json.contains("s3cret"), "{json}");
        let back: InputSpec = serde_json::from_str(&json).unwrap();
        let InputSpec::Html {
            url: read, size, ..
        } = back
        else {
            panic!("{back:?}")
        };
        assert_eq!(
            read.expose(),
            "https://***@scores.example/board.html?token=1"
        );
        assert_eq!(size, Some(Resolution::HD_720));
        assert_eq!(
            url.expose(),
            "https://op:s3cret@scores.example/board.html?token=1"
        );
        // An `@` after the host is not a credential.
        let plain = HtmlUrl::new("https://scores.example/@team/board.html");
        assert_eq!(plain.redacted(), plain.expose());
        // Without a size, the field is not written (the Program's).
        let json = serde_json::to_string(&html("file:///a.html")).unwrap();
        assert_eq!(json, r#"{"Html":{"url":"file:///a.html"}}"#);
    }
}
