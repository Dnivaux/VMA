//! VISCA messages (ADR 0018 § 2), from the public VISCA documentation: the commands VMA sends
//! and the replies of a camera. Camera address 1 (`0x81`; replies from `0x90`), as VISCA over
//! IP wants it. Encoding writes into a fixed buffer: no allocation per command.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Longest VISCA message VMA sends or reads (a VISCA packet is at most 16 bytes).
pub const MAX_MESSAGE: usize = 16;

/// End of every VISCA message.
pub const TERMINATOR: u8 = 0xFF;

/// First byte of a command to camera 1.
const TO_CAMERA: u8 = 0x81;
/// First byte of a reply from camera 1.
const FROM_CAMERA: u8 = 0x90;

/// Highest pan speed (`0x18`) and tilt speed (`0x14`, the lowest maximum among the cameras
/// documented), and the highest zoom / focus speed (`p` of the variable commands).
const PAN_MAX: u8 = 0x18;
const TILT_MAX: u8 = 0x14;
const LENS_MAX: u8 = 7;

/// Number of presets of a camera (Q4 of `phase-7.md`): memories 0 to 15.
pub const PRESETS: u8 = 16;

/// A speed from 1 (slowest) to 100 (fastest), mapped onto each motor's own scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Speed(u8);

impl Speed {
    pub const SLOWEST: Self = Self(1);
    pub const FASTEST: Self = Self(100);
    /// Default speed of the pad (half way).
    pub const DEFAULT: Self = Self(50);

    pub fn new(percent: u8) -> Option<Self> {
        (1..=100).contains(&percent).then_some(Self(percent))
    }

    pub fn get(self) -> u8 {
        self.0
    }

    /// `1 + (percent − 1) × (max − 1) / 99`, rounded: 1 → 1, 100 → `max`.
    fn scaled(self, min: u8, max: u8) -> u8 {
        let span = u32::from(max - min);
        let step = (u32::from(self.0 - 1) * span + 49) / 99;
        min + u8::try_from(step).unwrap_or(max - min)
    }

    fn pan(self) -> u8 {
        self.scaled(1, PAN_MAX)
    }

    fn tilt(self) -> u8 {
        self.scaled(1, TILT_MAX)
    }

    fn lens(self) -> u8 {
        self.scaled(0, LENS_MAX)
    }
}

impl Default for Speed {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TryFrom<u8> for Speed {
    type Error = String;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value).ok_or_else(|| format!("vitesse {value} hors de 1 à 100"))
    }
}

impl From<Speed> for u8 {
    fn from(speed: Speed) -> Self {
        speed.0
    }
}

/// A preset, numbered 1 to 16 for the operator (camera memories 0 to 15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "u8", into = "u8")]
pub struct Preset(u8);

impl Preset {
    /// `number`: 1 to [`PRESETS`].
    pub fn new(number: u8) -> Option<Self> {
        number.checked_sub(1).filter(|i| *i < PRESETS).map(Self)
    }

    /// 1 to 16.
    pub fn number(self) -> u8 {
        self.0 + 1
    }

    /// 0 to 15.
    pub fn index(self) -> usize {
        usize::from(self.0)
    }

    pub fn all() -> impl Iterator<Item = Self> {
        (0..PRESETS).map(Self)
    }
}

impl fmt::Display for Preset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.number())
    }
}

impl TryFrom<u8> for Preset {
    type Error = String;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::new(value).ok_or_else(|| format!("préréglage {value} hors de 1 à {PRESETS}"))
    }
}

impl From<Preset> for u8 {
    fn from(preset: Preset) -> Self {
        preset.number()
    }
}

/// Direction of a pan / tilt move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
    UpLeft,
    UpRight,
    DownLeft,
    DownRight,
    Stop,
}

impl Direction {
    /// The last two bytes of `Pan-tiltDrive`: pan (1 left, 2 right, 3 none), tilt (1 up,
    /// 2 down, 3 none).
    fn bytes(self) -> [u8; 2] {
        match self {
            Self::Up => [0x03, 0x01],
            Self::Down => [0x03, 0x02],
            Self::Left => [0x01, 0x03],
            Self::Right => [0x02, 0x03],
            Self::UpLeft => [0x01, 0x01],
            Self::UpRight => [0x02, 0x01],
            Self::DownLeft => [0x01, 0x02],
            Self::DownRight => [0x02, 0x02],
            Self::Stop => [0x03, 0x03],
        }
    }

    fn from_bytes(bytes: [u8; 2]) -> Option<Self> {
        [
            Self::Up,
            Self::Down,
            Self::Left,
            Self::Right,
            Self::UpLeft,
            Self::UpRight,
            Self::DownLeft,
            Self::DownRight,
            Self::Stop,
        ]
        .into_iter()
        .find(|d| d.bytes() == bytes)
    }
}

/// A zoom or focus move: towards tele / far (`In`), towards wide / near (`Out`), or stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LensMove {
    Stop,
    /// Zoom: tele (closer); focus: far.
    In(Speed),
    /// Zoom: wide; focus: near.
    Out(Speed),
}

impl LensMove {
    /// The byte of `CAM_Zoom` / `CAM_Focus`: `00` stop, `2p` tele / far, `3p` wide / near.
    fn byte(self) -> u8 {
        match self {
            Self::Stop => 0x00,
            Self::In(speed) => 0x20 | speed.lens(),
            Self::Out(speed) => 0x30 | speed.lens(),
        }
    }
}

/// Focus mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FocusMode {
    Auto,
    Manual,
}

/// The kind of a continuous move, for the queue of a camera: a newer move of the same kind
/// replaces the one waiting (ADR 0018 § 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveKind {
    PanTilt,
    Zoom,
    Focus,
}

/// What VMA asks of a camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ViscaCommand {
    /// `Pan-tiltDrive`: move (or stop) at `speed`.
    PanTilt { direction: Direction, speed: Speed },
    /// `Pan-tiltDrive Home`.
    Home,
    /// `CAM_Zoom`, variable speed.
    Zoom(LensMove),
    /// `CAM_Focus`, variable speed.
    Focus(LensMove),
    /// `CAM_Focus` auto / manual.
    FocusMode(FocusMode),
    /// `CAM_Focus One Push Trigger`: focus once, in manual mode.
    FocusOnePush,
    /// `CAM_Memory Set`: the current position becomes the preset.
    PresetSave(Preset),
    /// `CAM_Memory Recall`.
    PresetRecall(Preset),
    /// `CAM_PowerInq`: the probe of ADR 0018 § 4.
    PowerInquiry,
}

impl ViscaCommand {
    /// True for an inquiry (`8x 09 …`): its reply carries data and comes without an accusé.
    pub fn is_inquiry(self) -> bool {
        matches!(self, Self::PowerInquiry)
    }

    /// The kind of a continuous move (`None`: a one-off command).
    pub fn move_kind(self) -> Option<MoveKind> {
        match self {
            Self::PanTilt { .. } => Some(MoveKind::PanTilt),
            Self::Zoom(_) => Some(MoveKind::Zoom),
            Self::Focus(_) => Some(MoveKind::Focus),
            _ => None,
        }
    }

    /// Writes the message into `out` and returns it.
    pub fn encode(self, out: &mut [u8; MAX_MESSAGE]) -> &[u8] {
        let mut len = 0;
        let mut put = |bytes: &[u8]| {
            for &b in bytes {
                if let Some(slot) = out.get_mut(len) {
                    *slot = b;
                    len += 1;
                }
            }
        };
        match self {
            Self::PanTilt { direction, speed } => {
                let [pan, tilt] = direction.bytes();
                put(&[
                    TO_CAMERA,
                    0x01,
                    0x06,
                    0x01,
                    speed.pan(),
                    speed.tilt(),
                    pan,
                    tilt,
                ]);
            }
            Self::Home => put(&[TO_CAMERA, 0x01, 0x06, 0x04]),
            Self::Zoom(lens) => put(&[TO_CAMERA, 0x01, 0x04, 0x07, lens.byte()]),
            Self::Focus(lens) => put(&[TO_CAMERA, 0x01, 0x04, 0x08, lens.byte()]),
            Self::FocusMode(FocusMode::Auto) => put(&[TO_CAMERA, 0x01, 0x04, 0x38, 0x02]),
            Self::FocusMode(FocusMode::Manual) => put(&[TO_CAMERA, 0x01, 0x04, 0x38, 0x03]),
            Self::FocusOnePush => put(&[TO_CAMERA, 0x01, 0x04, 0x18, 0x01]),
            Self::PresetSave(preset) => put(&[TO_CAMERA, 0x01, 0x04, 0x3F, 0x01, preset.0]),
            Self::PresetRecall(preset) => put(&[TO_CAMERA, 0x01, 0x04, 0x3F, 0x02, preset.0]),
            Self::PowerInquiry => put(&[TO_CAMERA, 0x09, 0x04, 0x00]),
        }
        put(&[TERMINATOR]);
        out.get(..len).unwrap_or(&[])
    }

    /// Reads a message back (the simulated camera; `None`: not one VMA sends). Speeds come
    /// back on the camera's scale, so only the bytes are compared in tests.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let speed_from_lens =
            |p: u8| Speed::new(u8::try_from(1 + u32::from(p) * 99 / u32::from(LENS_MAX)).ok()?);
        let lens = |b: u8| match b {
            0x00 => Some(LensMove::Stop),
            0x20..=0x27 => speed_from_lens(b & 0x0F).map(LensMove::In),
            0x30..=0x37 => speed_from_lens(b & 0x0F).map(LensMove::Out),
            _ => None,
        };
        match bytes {
            [TO_CAMERA, 0x01, 0x06, 0x01, pan, _, d0, d1, TERMINATOR] => {
                let direction = Direction::from_bytes([*d0, *d1])?;
                let percent = 1 + u32::from(pan.checked_sub(1)?) * 99 / u32::from(PAN_MAX - 1);
                let speed = Speed::new(u8::try_from(percent).ok()?)?;
                Some(Self::PanTilt { direction, speed })
            }
            [TO_CAMERA, 0x01, 0x06, 0x04, TERMINATOR] => Some(Self::Home),
            [TO_CAMERA, 0x01, 0x04, 0x07, b, TERMINATOR] => lens(*b).map(Self::Zoom),
            [TO_CAMERA, 0x01, 0x04, 0x08, b, TERMINATOR] => lens(*b).map(Self::Focus),
            [TO_CAMERA, 0x01, 0x04, 0x38, 0x02, TERMINATOR] => {
                Some(Self::FocusMode(FocusMode::Auto))
            }
            [TO_CAMERA, 0x01, 0x04, 0x38, 0x03, TERMINATOR] => {
                Some(Self::FocusMode(FocusMode::Manual))
            }
            [TO_CAMERA, 0x01, 0x04, 0x18, 0x01, TERMINATOR] => Some(Self::FocusOnePush),
            [TO_CAMERA, 0x01, 0x04, 0x3F, 0x01, p, TERMINATOR] if *p < PRESETS => {
                Some(Self::PresetSave(Preset(*p)))
            }
            [TO_CAMERA, 0x01, 0x04, 0x3F, 0x02, p, TERMINATOR] if *p < PRESETS => {
                Some(Self::PresetRecall(Preset(*p)))
            }
            [TO_CAMERA, 0x09, 0x04, 0x00, TERMINATOR] => Some(Self::PowerInquiry),
            _ => None,
        }
    }
}

/// An error reported by a camera (`y0 6z ee FF`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CameraError {
    /// `01`: message length error.
    MessageLength,
    /// `02`: syntax error (a command this camera does not know).
    Syntax,
    /// `03`: command buffer full (both sockets busy).
    BufferFull,
    /// `04`: command cancelled.
    Cancelled,
    /// `05`: no socket (to cancel).
    NoSocket,
    /// `41`: command not executable now (e.g. a focus move in auto focus).
    NotExecutable,
    Other(u8),
}

impl CameraError {
    pub fn code(self) -> u8 {
        match self {
            Self::MessageLength => 0x01,
            Self::Syntax => 0x02,
            Self::BufferFull => 0x03,
            Self::Cancelled => 0x04,
            Self::NoSocket => 0x05,
            Self::NotExecutable => 0x41,
            Self::Other(code) => code,
        }
    }

    fn from_code(code: u8) -> Self {
        match code {
            0x01 => Self::MessageLength,
            0x02 => Self::Syntax,
            0x03 => Self::BufferFull,
            0x04 => Self::Cancelled,
            0x05 => Self::NoSocket,
            0x41 => Self::NotExecutable,
            other => Self::Other(other),
        }
    }
}

impl fmt::Display for CameraError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MessageLength => f.write_str("message de longueur invalide"),
            Self::Syntax => f.write_str("commande inconnue de la caméra"),
            Self::BufferFull => f.write_str("caméra occupée"),
            Self::Cancelled => f.write_str("commande annulée"),
            Self::NoSocket => f.write_str("aucune commande à annuler"),
            Self::NotExecutable => f.write_str("commande impossible maintenant"),
            Self::Other(code) => write!(f, "erreur {code:#04x}"),
        }
    }
}

/// A reply of the camera.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    /// `y0 4z FF`: the command is accepted, in socket `z`.
    Ack { socket: u8 },
    /// `y0 5z FF`: the command of socket `z` is done.
    Completion { socket: u8 },
    /// `y0 50 … FF`: the answer to an inquiry; `data` its first byte (`CAM_PowerInq`: `02`
    /// on, `03` standby).
    Answer { data: u8 },
    /// `y0 6z ee FF`.
    Error { socket: u8, error: CameraError },
}

impl Reply {
    /// `None`: not a reply from camera 1.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        match bytes {
            [FROM_CAMERA, b, TERMINATOR] if b & 0xF0 == 0x40 => {
                Some(Self::Ack { socket: b & 0x0F })
            }
            [FROM_CAMERA, b, TERMINATOR] if b & 0xF0 == 0x50 => {
                Some(Self::Completion { socket: b & 0x0F })
            }
            [FROM_CAMERA, 0x50, data, .., TERMINATOR] => Some(Self::Answer { data: *data }),
            [FROM_CAMERA, b, code, TERMINATOR] if b & 0xF0 == 0x60 => Some(Self::Error {
                socket: b & 0x0F,
                error: CameraError::from_code(*code),
            }),
            _ => None,
        }
    }

    /// Writes the reply (the simulated camera) into `out` and returns it.
    pub fn encode(self, out: &mut [u8; MAX_MESSAGE]) -> &[u8] {
        let bytes: &[u8] = match self {
            Self::Ack { socket } => &[FROM_CAMERA, 0x40 | (socket & 0x0F), TERMINATOR],
            Self::Completion { socket } => &[FROM_CAMERA, 0x50 | (socket & 0x0F), TERMINATOR],
            Self::Answer { data } => &[FROM_CAMERA, 0x50, data, TERMINATOR],
            Self::Error { socket, error } => &[
                FROM_CAMERA,
                0x60 | (socket & 0x0F),
                error.code(),
                TERMINATOR,
            ],
        };
        let len = bytes.len().min(MAX_MESSAGE);
        if let (Some(dst), Some(src)) = (out.get_mut(..len), bytes.get(..len)) {
            dst.copy_from_slice(src);
        }
        out.get(..len).unwrap_or(&[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(command: ViscaCommand) -> Vec<u8> {
        let mut out = [0; MAX_MESSAGE];
        command.encode(&mut out).to_vec()
    }

    fn speed(percent: u8) -> Speed {
        Speed::new(percent).unwrap()
    }

    #[test]
    fn speeds_span_each_motor_scale() {
        assert_eq!((Speed::SLOWEST.pan(), Speed::SLOWEST.tilt()), (0x01, 0x01));
        assert_eq!((Speed::FASTEST.pan(), Speed::FASTEST.tilt()), (0x18, 0x14));
        assert_eq!((Speed::SLOWEST.lens(), Speed::FASTEST.lens()), (0, 7));
        assert_eq!(Speed::DEFAULT.pan(), 0x0C);
        assert_eq!(Speed::new(0), None);
        assert_eq!(Speed::new(101), None);
        // Monotonic: a faster setting never moves slower.
        let mut previous = (0, 0, 0);
        for p in 1..=100 {
            let s = speed(p);
            let now = (s.pan(), s.tilt(), s.lens());
            assert!(now.0 >= previous.0 && now.1 >= previous.1 && now.2 >= previous.2);
            previous = now;
        }
    }

    #[test]
    fn pan_tilt_moves_are_encoded_to_the_byte() {
        let fast = speed(100);
        let cases = [
            (Direction::Up, [0x03, 0x01]),
            (Direction::Down, [0x03, 0x02]),
            (Direction::Left, [0x01, 0x03]),
            (Direction::Right, [0x02, 0x03]),
            (Direction::UpLeft, [0x01, 0x01]),
            (Direction::UpRight, [0x02, 0x01]),
            (Direction::DownLeft, [0x01, 0x02]),
            (Direction::DownRight, [0x02, 0x02]),
            (Direction::Stop, [0x03, 0x03]),
        ];
        for (direction, [p, t]) in cases {
            let command = ViscaCommand::PanTilt {
                direction,
                speed: fast,
            };
            assert_eq!(
                bytes(command),
                [0x81, 0x01, 0x06, 0x01, 0x18, 0x14, p, t, 0xFF],
                "{direction:?}"
            );
            assert_eq!(ViscaCommand::decode(&bytes(command)), Some(command));
        }
        assert_eq!(bytes(ViscaCommand::Home), [0x81, 0x01, 0x06, 0x04, 0xFF]);
    }

    #[test]
    fn lens_commands_are_encoded_to_the_byte() {
        use ViscaCommand as V;
        let slow = speed(1);
        let fast = speed(100);
        let cases: [(ViscaCommand, &[u8]); 11] = [
            (
                V::Zoom(LensMove::Stop),
                &[0x81, 0x01, 0x04, 0x07, 0x00, 0xFF],
            ),
            (
                V::Zoom(LensMove::In(slow)),
                &[0x81, 0x01, 0x04, 0x07, 0x20, 0xFF],
            ),
            (
                V::Zoom(LensMove::In(fast)),
                &[0x81, 0x01, 0x04, 0x07, 0x27, 0xFF],
            ),
            (
                V::Zoom(LensMove::Out(fast)),
                &[0x81, 0x01, 0x04, 0x07, 0x37, 0xFF],
            ),
            (
                V::Focus(LensMove::Stop),
                &[0x81, 0x01, 0x04, 0x08, 0x00, 0xFF],
            ),
            (
                V::Focus(LensMove::In(fast)),
                &[0x81, 0x01, 0x04, 0x08, 0x27, 0xFF],
            ),
            (
                V::Focus(LensMove::Out(slow)),
                &[0x81, 0x01, 0x04, 0x08, 0x30, 0xFF],
            ),
            (
                V::FocusMode(FocusMode::Auto),
                &[0x81, 0x01, 0x04, 0x38, 0x02, 0xFF],
            ),
            (
                V::FocusMode(FocusMode::Manual),
                &[0x81, 0x01, 0x04, 0x38, 0x03, 0xFF],
            ),
            (V::FocusOnePush, &[0x81, 0x01, 0x04, 0x18, 0x01, 0xFF]),
            (V::PowerInquiry, &[0x81, 0x09, 0x04, 0x00, 0xFF]),
        ];
        for (command, expected) in cases {
            assert_eq!(bytes(command), expected, "{command:?}");
            assert_eq!(
                ViscaCommand::decode(expected).map(bytes),
                Some(expected.to_vec()),
                "{command:?} read back"
            );
        }
    }

    #[test]
    fn presets_are_numbered_from_one_and_stored_from_zero() {
        let first = Preset::new(1).unwrap();
        let last = Preset::new(16).unwrap();
        assert_eq!(Preset::new(0), None);
        assert_eq!(Preset::new(17), None);
        assert_eq!(
            bytes(ViscaCommand::PresetSave(first)),
            [0x81, 0x01, 0x04, 0x3F, 0x01, 0x00, 0xFF]
        );
        assert_eq!(
            bytes(ViscaCommand::PresetRecall(last)),
            [0x81, 0x01, 0x04, 0x3F, 0x02, 0x0F, 0xFF]
        );
        assert_eq!(Preset::all().count(), 16);
        assert_eq!(
            ViscaCommand::decode(&[0x81, 0x01, 0x04, 0x3F, 0x02, 0x0F, 0xFF]),
            Some(ViscaCommand::PresetRecall(last))
        );
        assert_eq!(serde_json::to_string(&last).unwrap(), "16");
        assert!(serde_json::from_str::<Preset>("0").is_err());
    }

    #[test]
    fn replies_are_read_and_written() {
        let cases: [(&[u8], Reply); 6] = [
            (&[0x90, 0x41, 0xFF], Reply::Ack { socket: 1 }),
            (&[0x90, 0x52, 0xFF], Reply::Completion { socket: 2 }),
            (&[0x90, 0x50, 0x02, 0xFF], Reply::Answer { data: 0x02 }),
            (
                &[0x90, 0x60, 0x02, 0xFF],
                Reply::Error {
                    socket: 0,
                    error: CameraError::Syntax,
                },
            ),
            (
                &[0x90, 0x61, 0x41, 0xFF],
                Reply::Error {
                    socket: 1,
                    error: CameraError::NotExecutable,
                },
            ),
            (
                &[0x90, 0x60, 0x03, 0xFF],
                Reply::Error {
                    socket: 0,
                    error: CameraError::BufferFull,
                },
            ),
        ];
        for (raw, reply) in cases {
            assert_eq!(Reply::parse(raw), Some(reply), "{raw:02x?}");
            let mut out = [0; MAX_MESSAGE];
            assert_eq!(reply.encode(&mut out), raw);
        }
        assert_eq!(Reply::parse(&[0xA0, 0x41, 0xFF]), None, "another camera");
        assert_eq!(Reply::parse(&[0x90, 0x41]), None, "no terminator");
    }

    #[test]
    fn unknown_messages_are_not_decoded() {
        assert_eq!(
            ViscaCommand::decode(&[0x81, 0x01, 0x04, 0x00, 0x02, 0xFF]),
            None
        );
        assert_eq!(
            ViscaCommand::decode(&[0x81, 0x01, 0x04, 0x3F, 0x02, 0x10, 0xFF]),
            None,
            "preset 17"
        );
        assert_eq!(ViscaCommand::decode(&[]), None);
    }
}
