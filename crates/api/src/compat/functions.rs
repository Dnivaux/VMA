//! The functions of the compat API, as `Command`s (`docs/specs/api-compat.md` § 4). Names are
//! compared without case (the Companion module sends `fadeToBlack`, `quickPlay`).

use vma_core::{AudioBus, InputId, OverlayChannel, Rgb8};
use vma_engine::{Command, Db, Effect, InputSpec, OutputSpec, State, StingerSlot};

use super::call::Call;

mod replay;

/// Why a call is not turned into commands. Written as is in `ER` lines and HTTP 500 bodies.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompatError {
    #[error("function not supported by VMA: {0}")]
    Unsupported(String),
    #[error("{function}: parameter {param} missing")]
    Missing {
        function: String,
        param: &'static str,
    },
    #[error("{function}: invalid {param} {value:?}")]
    Invalid {
        function: String,
        param: &'static str,
        value: String,
    },
    #[error("no such input: {0}")]
    NoInput(String),
    #[error("VMA has one mix only (Mix=0)")]
    Mix,
    #[error("{0}")]
    Refused(String),
}

/// Volume 0–100 of the compat API to a fader level: 100 = 0 dB, 0 = silence, amplitude
/// (volume / 100)² in between (a fader law; the reference's own is to be observed, Q3).
pub fn volume_to_db(volume: f32) -> Db {
    let v = (volume / 100.0).clamp(0.0, 1.0);
    if v <= 0.0 {
        Db::SILENT
    } else {
        Db::new(40.0 * v.log10()).unwrap_or(Db::SILENT)
    }
}

/// The other way: 0–100, a gain above 0 dB counts as 100.
pub fn db_to_volume(db: Db) -> f32 {
    let db = db.get();
    if db == f32::NEG_INFINITY {
        0.0
    } else {
        (100.0 * 10f32.powf(db / 40.0)).clamp(0.0, 100.0)
    }
}

struct Ctx<'a> {
    call: &'a Call,
    state: &'a State,
}

impl Ctx<'_> {
    fn name(&self) -> String {
        self.call.function.clone()
    }

    fn missing(&self, param: &'static str) -> CompatError {
        CompatError::Missing {
            function: self.name(),
            param,
        }
    }

    fn invalid(&self, param: &'static str, value: &str) -> CompatError {
        CompatError::Invalid {
            function: self.name(),
            param,
            value: value.to_owned(),
        }
    }

    /// `Input`: a number from 1 (0 = Preview, -1 = Program), a key, or a name (exact).
    fn input(&self) -> Result<Option<InputId>, CompatError> {
        let Some(text) = self.call.param("Input") else {
            return Ok(None);
        };
        let state = self.state;
        let found = if let Ok(number) = text.trim().parse::<i64>() {
            match number {
                0 => state.preview,
                -1 => state.program,
                n => usize::try_from(n - 1)
                    .ok()
                    .and_then(|n| state.inputs.get(n))
                    .map(|i| i.id),
            }
        } else if let Ok(key) = text.parse() {
            state.inputs.iter().find(|i| i.key == key).map(|i| i.id)
        } else {
            state.inputs.iter().find(|i| i.name == text).map(|i| i.id)
        };
        found
            .map(Some)
            .ok_or_else(|| CompatError::NoInput(text.to_owned()))
    }

    fn required_input(&self) -> Result<InputId, CompatError> {
        self.input()?.ok_or_else(|| self.missing("Input"))
    }

    fn value(&self) -> Result<&str, CompatError> {
        self.call
            .param("Value")
            .ok_or_else(|| self.missing("Value"))
    }

    fn number<T: std::str::FromStr>(&self, param: &'static str) -> Result<Option<T>, CompatError> {
        self.call
            .param(param)
            .map(|v| v.trim().parse().map_err(|_| self.invalid(param, v)))
            .transpose()
    }

    /// `Mix`: only 0 (the main mix), alone or in a list.
    fn main_mix(&self) -> Result<(), CompatError> {
        match self.call.param("Mix") {
            Some(mixes) if mixes.split(',').any(|m| m.trim() != "0") => Err(CompatError::Mix),
            _ => Ok(()),
        }
    }

    fn effect(&self, name: &str) -> Result<Effect, CompatError> {
        match name.to_ascii_lowercase().as_str() {
            "fade" => Ok(Effect::Fade),
            "cut" => Ok(Effect::Cut),
            other => other
                .strip_prefix("stinger")
                .and_then(|n| n.parse().ok())
                .and_then(StingerSlot::new)
                .map(Effect::Stinger)
                .ok_or_else(|| self.invalid("Value", name)),
        }
    }
}

/// The trailing number of `name` after `prefix` (`transition3` → 3), within `range`.
fn numbered(name: &str, prefix: &str, range: std::ops::RangeInclusive<u8>) -> Option<u8> {
    name.strip_prefix(prefix)
        .and_then(|n| n.parse().ok())
        .filter(|n| range.contains(n))
}

fn bus(letter: &str) -> Option<AudioBus> {
    match letter.trim().to_ascii_uppercase().as_str() {
        "M" | "MASTER" => Some(AudioBus::Master),
        "A" => Some(AudioBus::A),
        "B" => Some(AudioBus::B),
        _ => None,
    }
}

/// The commands `call` stands for, given the show `state`.
pub fn resolve(call: &Call, state: &State) -> Result<Vec<Command>, CompatError> {
    let cx = Ctx { call, state };
    let name = call.function.to_ascii_lowercase();
    let one = |c: Command| Ok(vec![c]);

    // Transitions.
    match name.as_str() {
        "cut" => {
            return one(match cx.input()? {
                Some(input) => Command::CutDirect { input },
                None => Command::Cut,
            });
        }
        "fade" => {
            let duration_ms = match cx.number::<u32>("Duration")? {
                Some(ms) => ms,
                None => state.buttons.first().map_or(500, |b| b.duration_ms),
            };
            return one(Command::Mix {
                effect: Effect::Fade,
                duration_ms,
                input: cx.input()?,
            });
        }
        "cutdirect" | "activeinput" => {
            cx.main_mix()?;
            return one(Command::CutDirect {
                input: cx.required_input()?,
            });
        }
        "previewinput" => {
            cx.main_mix()?;
            return one(Command::SetPreview {
                input: cx.required_input()?,
            });
        }
        "previewinputnext" => return one(Command::PreviewNext),
        "previewinputprevious" => return one(Command::PreviewPrevious),
        "quickplay" => {
            return one(Command::QuickPlay { input: cx.input()? });
        }
        "setfader" => {
            let value = cx
                .number::<u8>("Value")?
                .ok_or_else(|| cx.missing("Value"))?;
            return one(Command::SetFader { value });
        }
        "fadetoblack" => return one(Command::FadeToBlack),
        "overlayinputalloff" => return one(Command::OverlayAllOff),
        _ => {}
    }
    if let Some(button) = numbered(&name, "transition", 1..=4) {
        return one(Command::Transition { button });
    }
    if let Some(slot) = numbered(&name, "stinger", 1..=u8::MAX).map(StingerSlot::new) {
        let slot = slot.ok_or_else(|| CompatError::Unsupported(cx.name()))?;
        cx.main_mix()?;
        return one(Command::Mix {
            effect: Effect::Stinger(slot),
            duration_ms: 0,
            input: cx.input()?,
        });
    }
    for (prefix, effect_only) in [
        ("settransitioneffect", true),
        ("settransitionduration", false),
    ] {
        if let Some(button) = numbered(&name, prefix, 1..=4) {
            let current = state
                .buttons
                .get(usize::from(button) - 1)
                .copied()
                .unwrap_or(State::DEFAULT_BUTTONS[0]);
            let value = cx.value()?;
            let (effect, duration_ms) = if effect_only {
                (cx.effect(value)?, current.duration_ms)
            } else {
                let ms = value
                    .trim()
                    .parse()
                    .map_err(|_| cx.invalid("Value", value))?;
                (current.effect, ms)
            };
            return one(Command::SetTransition {
                button,
                effect,
                duration_ms,
            });
        }
    }

    // Overlays: OverlayInput1, …In, …Out, …Off, …Last.
    if let Some(rest) = name.strip_prefix("overlayinput") {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        let (number, suffix) = rest.split_at(digits);
        let channel = number
            .parse::<u8>()
            .ok()
            .and_then(|n| OverlayChannel::new(n).ok())
            .ok_or_else(|| CompatError::Unsupported(cx.name()))?;
        cx.main_mix()?;
        return match suffix {
            "" => one(Command::OverlayToggle {
                channel,
                input: cx.required_input()?,
            }),
            "in" => one(Command::OverlayIn {
                channel,
                input: cx.required_input()?,
            }),
            "out" => one(Command::OverlayOut { channel }),
            "off" => one(Command::OverlayOff { channel }),
            "last" => {
                let last = state
                    .overlays
                    .get(usize::from(channel.number()) - 1)
                    .and_then(|o| o.input)
                    .ok_or_else(|| {
                        CompatError::Refused(format!(
                            "overlay {} has had no input",
                            channel.number()
                        ))
                    })?;
                one(Command::OverlayIn {
                    channel,
                    input: last,
                })
            }
            _ => Err(CompatError::Unsupported(cx.name())),
        };
    }

    // Outputs.
    let recording = state
        .outputs
        .iter()
        .any(|o| matches!(o.spec, OutputSpec::Record(_)) && o.status.is_on());
    match name.as_str() {
        "startrecording" => return one(Command::StartRecording),
        "stoprecording" => return one(Command::StopRecording),
        "startstoprecording" => {
            return one(if recording {
                Command::StopRecording
            } else {
                Command::StartRecording
            });
        }
        "startstreaming" | "stopstreaming" | "startstopstreaming" => {
            // Stream numbers from 0; VMA's destinations from 1.
            let slot = cx
                .number::<u8>("Value")?
                .map(|n| n.checked_add(1).ok_or_else(|| cx.invalid("Value", "255")))
                .transpose()?;
            let streaming = state.outputs.iter().any(|o| {
                matches!(o.spec, OutputSpec::Rtmp(_) | OutputSpec::Srt(_))
                    && (slot.is_none() || o.slot == slot)
                    && o.status.is_on()
            });
            let start = match name.as_str() {
                "startstreaming" => true,
                "stopstreaming" => false,
                _ => !streaming,
            };
            return one(if start {
                Command::StartStreaming { slot }
            } else {
                Command::StopStreaming { slot }
            });
        }
        "fullscreen" | "fullscreenon" | "fullscreenoff" => {
            let output = state
                .outputs
                .iter()
                .find(|o| o.spec.is_fullscreen())
                .ok_or_else(|| CompatError::Refused("no full-screen output is set".into()))?;
            let start = match name.as_str() {
                "fullscreenon" => true,
                "fullscreenoff" => false,
                _ => !output.status.is_on(),
            };
            return one(if start {
                Command::StartOutput { output: output.id }
            } else {
                Command::StopOutput { output: output.id }
            });
        }
        _ => {}
    }

    // Sound.
    let audio_of = |input: InputId| state.input(input).and_then(|i| i.audio);
    let toggle = |suffix: &str, current: bool| match suffix {
        "on" => Some(true),
        "off" => Some(false),
        "" => Some(!current),
        _ => None,
    };
    for (prefix, kind) in [("audioauto", 1), ("audiobus", 2), ("audio", 0), ("solo", 3)] {
        let Some(suffix) = name.strip_prefix(prefix) else {
            continue;
        };
        if !matches!(suffix, "" | "on" | "off") {
            continue;
        }
        let input = cx.required_input()?;
        let audio = audio_of(input);
        return match kind {
            // « Audio on » is sound heard: not muted.
            0 => {
                let on = toggle(suffix, !audio.is_some_and(|a| a.mute)).unwrap_or(true);
                one(Command::SetInputMute { input, mute: !on })
            }
            1 => {
                let on = toggle(suffix, audio.is_some_and(|a| a.follow_video)).unwrap_or(true);
                one(Command::SetAudioFollowVideo { input, on })
            }
            2 => {
                let value = cx.value()?;
                let mut commands = Vec::new();
                for letter in value.split(',') {
                    let bus = bus(letter).ok_or_else(|| cx.invalid("Value", value))?;
                    let current = audio.is_some_and(|a| a.sends.get(bus.index()) == Some(&true));
                    let on = toggle(suffix, current).unwrap_or(true);
                    commands.push(Command::SetInputBus { input, bus, on });
                }
                Ok(commands)
            }
            _ => {
                let solo = toggle(suffix, audio.is_some_and(|a| a.solo)).unwrap_or(true);
                one(Command::SetInputSolo { input, solo })
            }
        };
    }
    let volume = |cx: &Ctx<'_>| -> Result<Db, CompatError> {
        let value = cx.value()?;
        let v: f32 = value
            .trim()
            .parse()
            .map_err(|_| cx.invalid("Value", value))?;
        if !(0.0..=100.0).contains(&v) {
            return Err(cx.invalid("Value", value));
        }
        Ok(volume_to_db(v))
    };
    match name.as_str() {
        "setvolume" => {
            return one(Command::SetInputGain {
                input: cx.required_input()?,
                db: volume(&cx)?,
            });
        }
        "setmastervolume" | "setbusavolume" | "setbusbvolume" => {
            let bus = match name.as_str() {
                "setmastervolume" => AudioBus::Master,
                "setbusavolume" => AudioBus::A,
                _ => AudioBus::B,
            };
            return one(Command::SetBusGain {
                bus,
                db: volume(&cx)?,
            });
        }
        _ => {}
    }
    for (prefix, bus) in [
        ("masteraudio", AudioBus::Master),
        ("busaaudio", AudioBus::A),
        ("busbaudio", AudioBus::B),
    ] {
        if let Some(suffix) = name.strip_prefix(prefix) {
            let muted = state.audio.buses.get(bus.index()).is_some_and(|b| b.mute);
            let Some(on) = toggle(suffix, !muted) else {
                continue;
            };
            return one(Command::SetBusMute { bus, mute: !on });
        }
    }

    // Inputs.
    match name.as_str() {
        "play" | "pause" | "playpause" | "restart" => {
            let input = cx.required_input()?;
            let playing = state
                .input(input)
                .and_then(|i| i.playback)
                .is_some_and(|p| p.playing);
            return one(match name.as_str() {
                "play" => Command::Play { input },
                "pause" => Command::Pause { input },
                "restart" => Command::Restart { input },
                _ if playing => Command::Pause { input },
                _ => Command::Play { input },
            });
        }
        "loop" | "loopon" | "loopoff" => {
            let input = cx.required_input()?;
            let looping = state
                .input(input)
                .and_then(|i| i.playback)
                .is_some_and(|p| p.looping);
            let enabled = toggle(name.trim_start_matches("loop"), looping).unwrap_or(true);
            return one(Command::SetLoop { input, enabled });
        }
        "setposition" => {
            let input = cx.required_input()?;
            let ms = cx
                .number::<u64>("Value")?
                .ok_or_else(|| cx.missing("Value"))?;
            return one(Command::SetPosition { input, ms });
        }
        "setinputname" => {
            return one(Command::RenameInput {
                input: cx.required_input()?,
                name: cx.value()?.to_owned(),
            });
        }
        "removeinput" => {
            return one(Command::RemoveInput {
                input: cx.required_input()?,
            });
        }
        "addinput" => {
            let value = cx.value()?;
            let (kind, what) = value
                .split_once('|')
                .ok_or_else(|| cx.invalid("Value", value))?;
            let spec = match kind.trim().to_ascii_lowercase().as_str() {
                "video" => InputSpec::File { path: what.into() },
                "image" => InputSpec::Image { path: what.into() },
                "colour" | "color" => InputSpec::Color {
                    color: hex_color(what).ok_or_else(|| cx.invalid("Value", value))?,
                },
                _ => return Err(CompatError::Unsupported(format!("AddInput {kind}"))),
            };
            return one(Command::AddInput { spec, name: None });
        }
        _ => {}
    }
    if let Some(commands) = replay::resolve(&cx, &name) {
        return commands;
    }
    Err(CompatError::Unsupported(cx.name()))
}

/// `#RRGGBB` (or without `#`).
fn hex_color(text: &str) -> Option<Rgb8> {
    let hex = text.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return None;
    }
    let byte = |at: usize| u8::from_str_radix(hex.get(at..at + 2)?, 16).ok();
    Some(Rgb8::new(byte(0)?, byte(2)?, byte(4)?))
}
