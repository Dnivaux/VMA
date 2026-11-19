//! The PTZ functions of the compat API (`api-compat.md` § 4, « PTZ »; Phase 7A, ADR 0018). The
//! reference's virtual inputs take the camera's current position in a preset of their own:
//! VMA's take the first preset none of the camera's virtual inputs uses.

use vma_core::InputId;
use vma_engine::{
    Command, Direction, FocusMode, InputSpec, LensMove, Preset, PtzAction, Speed, State,
};

use super::{CompatError, Ctx};

/// `Value`: a speed from 0 to 1 (absent: VMA's default, half way).
fn speed(cx: &Ctx<'_>) -> Result<Speed, CompatError> {
    let Some(value) = cx.call.param("Value") else {
        return Ok(Speed::DEFAULT);
    };
    let speed: f32 = value
        .trim()
        .parse()
        .map_err(|_| cx.invalid("Value", value))?;
    if !(0.0..=1.0).contains(&speed) {
        return Err(cx.invalid("Value", value));
    }
    Ok(Speed::new((speed * 100.0).round().clamp(1.0, 100.0) as u8).unwrap_or(Speed::DEFAULT))
}

/// The preset of a virtual input.
fn view_preset(state: &State, input: InputId) -> Option<Preset> {
    match state.input(input)?.spec {
        InputSpec::PtzView { preset, .. } => Some(preset),
        _ => None,
    }
}

/// The commands of `name` (in lower case) if it is a PTZ function.
pub(super) fn resolve(cx: &Ctx<'_>, name: &str) -> Option<Result<Vec<Command>, CompatError>> {
    if !name.starts_with("ptz") {
        return None;
    }
    Some(resolve_ptz(cx, name))
}

fn resolve_ptz(cx: &Ctx<'_>, name: &str) -> Result<Vec<Command>, CompatError> {
    let state = cx.state;
    let input = cx.required_input()?;
    let camera = state
        .ptz_camera_of(input)
        .ok_or_else(|| CompatError::Refused(format!("input {} is not a PTZ camera", input)))?;
    let ptz = |action| Command::Ptz { input, action };
    let pan = |direction| -> Result<Vec<Command>, CompatError> {
        Ok(vec![ptz(PtzAction::Move {
            direction,
            speed: speed(cx)?,
        })])
    };
    let not_a_view = || CompatError::Refused(format!("input {input} is not a PTZ virtual input"));
    match name {
        "ptzmoveup" => pan(Direction::Up),
        "ptzmovedown" => pan(Direction::Down),
        "ptzmoveleft" => pan(Direction::Left),
        "ptzmoveright" => pan(Direction::Right),
        "ptzmoveupleft" => pan(Direction::UpLeft),
        "ptzmoveupright" => pan(Direction::UpRight),
        "ptzmovedownleft" => pan(Direction::DownLeft),
        "ptzmovedownright" => pan(Direction::DownRight),
        // « Stop all PTZ movement ».
        "ptzmovestop" => Ok(vec![
            ptz(PtzAction::Move {
                direction: Direction::Stop,
                speed: Speed::DEFAULT,
            }),
            ptz(PtzAction::Zoom(LensMove::Stop)),
            ptz(PtzAction::Focus(LensMove::Stop)),
        ]),
        "ptzzoomin" => Ok(vec![ptz(PtzAction::Zoom(LensMove::In(speed(cx)?)))]),
        "ptzzoomout" => Ok(vec![ptz(PtzAction::Zoom(LensMove::Out(speed(cx)?)))]),
        "ptzzoomstop" => Ok(vec![ptz(PtzAction::Zoom(LensMove::Stop))]),
        "ptzfocusfar" => Ok(vec![ptz(PtzAction::Focus(LensMove::In(speed(cx)?)))]),
        "ptzfocusnear" => Ok(vec![ptz(PtzAction::Focus(LensMove::Out(speed(cx)?)))]),
        "ptzfocusstop" => Ok(vec![ptz(PtzAction::Focus(LensMove::Stop))]),
        "ptzfocusauto" => Ok(vec![ptz(PtzAction::FocusMode(FocusMode::Auto))]),
        "ptzfocusmanual" => Ok(vec![ptz(PtzAction::FocusMode(FocusMode::Manual))]),
        "ptzhome" => Ok(vec![ptz(PtzAction::Home)]),
        // An explicit request: it moves a camera on air too (no one to confirm it).
        "ptzmovetovirtualinputposition" => {
            let preset = view_preset(state, input).ok_or_else(not_a_view)?;
            Ok(vec![ptz(PtzAction::Recall {
                preset,
                confirmed: true,
            })])
        }
        "ptzmovetovirtualinputpositionbyindex" => {
            let value = cx.value()?;
            let index: usize = value
                .trim()
                .parse()
                .map_err(|_| cx.invalid("Value", value))?;
            let view = state
                .ptz_views(camera)
                .get(index)
                .copied()
                .ok_or_else(|| cx.invalid("Value", value))?;
            let preset = view_preset(state, view).ok_or_else(not_a_view)?;
            Ok(vec![ptz(PtzAction::Recall {
                preset,
                confirmed: true,
            })])
        }
        "ptzcreatevirtualinput" => {
            let used: Vec<Preset> = state
                .ptz_views(camera)
                .into_iter()
                .filter_map(|v| view_preset(state, v))
                .collect();
            let preset = Preset::all().find(|p| !used.contains(p)).ok_or_else(|| {
                CompatError::Refused("all 16 presets have a virtual input".into())
            })?;
            let source = state
                .input(camera)
                .ok_or_else(|| CompatError::NoInput(camera.to_string()))?;
            Ok(vec![
                Command::Ptz {
                    input: camera,
                    action: PtzAction::Save(preset),
                },
                Command::AddInput {
                    spec: InputSpec::PtzView {
                        camera: source.key,
                        preset,
                    },
                    name: Some(format!("{} {preset}", source.name)),
                },
            ])
        }
        "ptzupdatevirtualinput" => {
            let preset = view_preset(state, input).ok_or_else(not_a_view)?;
            Ok(vec![ptz(PtzAction::Save(preset))])
        }
        _ => Err(CompatError::Unsupported(cx.name())),
    }
}

#[cfg(test)]
mod tests {
    use vma_core::{FrameRate, Resolution};
    use vma_engine::{CameraAddress, DeviceId, Format, Transport};

    use super::super::{Call, resolve};
    use super::*;

    /// 1 a PTZ camera « Cam », 2 a camera without PTZ, 3 and 4 virtual inputs of 1 on presets 1
    /// and 2.
    fn show() -> (State, InputId, InputId, InputId) {
        let mut s = State::new(Format {
            rate: FrameRate::FPS_50,
            resolution: Resolution::HD_1080,
        });
        for name in ["Cam", "Autre"] {
            s.apply(&Command::AddInput {
                spec: InputSpec::Capture {
                    device: DeviceId(name.into()),
                    audio: None,
                },
                name: Some(name.into()),
            })
            .unwrap();
        }
        let cam = s.inputs[0].id;
        s.apply(&Command::SetPtzCamera {
            input: cam,
            address: Some(CameraAddress::new(Transport::Udp, "10.0.0.5")),
        })
        .unwrap();
        let key = s.inputs[0].key;
        for n in [1, 2] {
            s.apply(&Command::AddInput {
                spec: InputSpec::PtzView {
                    camera: key,
                    preset: Preset::new(n).unwrap(),
                },
                name: None,
            })
            .unwrap();
        }
        let (one, two) = (s.inputs[2].id, s.inputs[3].id);
        (s, cam, one, two)
    }

    fn call(state: &State, function: &str, query: &str) -> Result<Vec<Command>, CompatError> {
        resolve(&Call::new(function, query), state)
    }

    fn preset(n: u8) -> Preset {
        Preset::new(n).unwrap()
    }

    #[test]
    fn moves_take_their_speed_from_value() {
        let (s, cam, one, _) = show();
        let ptz = |input, action| Command::Ptz { input, action };
        assert_eq!(
            call(&s, "PTZMoveUp", "Input=1&Value=1"),
            Ok(vec![ptz(
                cam,
                PtzAction::Move {
                    direction: Direction::Up,
                    speed: Speed::FASTEST
                }
            )])
        );
        assert_eq!(
            call(&s, "ptzmovedownleft", "Input=Cam"),
            Ok(vec![ptz(
                cam,
                PtzAction::Move {
                    direction: Direction::DownLeft,
                    speed: Speed::DEFAULT
                }
            )]),
            "no Value: the default speed; any case"
        );
        assert_eq!(
            call(&s, "PTZZoomIn", "Input=3&Value=0.25"),
            Ok(vec![ptz(
                one,
                PtzAction::Zoom(LensMove::In(Speed::new(25).unwrap()))
            )]),
            "a virtual input drives its camera"
        );
        assert_eq!(
            call(&s, "PTZMoveStop", "Input=1").unwrap().len(),
            3,
            "« stop all PTZ movement »: pan / tilt, zoom, focus"
        );
        for (function, action) in [
            ("PTZZoomStop", PtzAction::Zoom(LensMove::Stop)),
            ("PTZFocusStop", PtzAction::Focus(LensMove::Stop)),
            ("PTZFocusAuto", PtzAction::FocusMode(FocusMode::Auto)),
            ("PTZFocusManual", PtzAction::FocusMode(FocusMode::Manual)),
            ("PTZHome", PtzAction::Home),
            (
                "PTZFocusFar",
                PtzAction::Focus(LensMove::In(Speed::DEFAULT)),
            ),
            (
                "PTZFocusNear",
                PtzAction::Focus(LensMove::Out(Speed::DEFAULT)),
            ),
            ("PTZZoomOut", PtzAction::Zoom(LensMove::Out(Speed::DEFAULT))),
        ] {
            assert_eq!(
                call(&s, function, "Input=1"),
                Ok(vec![ptz(cam, action)]),
                "{function}"
            );
        }
        assert!(matches!(
            call(&s, "PTZMoveUp", "Input=1&Value=2"),
            Err(CompatError::Invalid { .. })
        ));
        assert!(matches!(
            call(&s, "PTZHome", "Input=2"),
            Err(CompatError::Refused(_))
        ));
        assert!(matches!(
            call(&s, "PTZHome", ""),
            Err(CompatError::Missing { .. })
        ));
        assert!(matches!(
            call(&s, "PTZSpin", "Input=1"),
            Err(CompatError::Unsupported(_))
        ));
    }

    #[test]
    fn virtual_inputs() {
        let (s, cam, one, two) = show();
        let recall = |input, n| Command::Ptz {
            input,
            action: PtzAction::Recall {
                preset: preset(n),
                confirmed: true,
            },
        };
        assert_eq!(
            call(&s, "PTZMoveToVirtualInputPosition", "Input=4"),
            Ok(vec![recall(two, 2)])
        );
        assert!(matches!(
            call(&s, "PTZMoveToVirtualInputPosition", "Input=1"),
            Err(CompatError::Refused(_))
        ));
        assert_eq!(
            call(
                &s,
                "PTZMoveToVirtualInputPositionByIndex",
                "Input=1&Value=1"
            ),
            Ok(vec![recall(cam, 2)]),
            "index from 0 among the camera's virtual inputs"
        );
        assert!(matches!(
            call(
                &s,
                "PTZMoveToVirtualInputPositionByIndex",
                "Input=1&Value=2"
            ),
            Err(CompatError::Invalid { .. })
        ));
        assert_eq!(
            call(&s, "PTZUpdateVirtualInput", "Input=3"),
            Ok(vec![Command::Ptz {
                input: one,
                action: PtzAction::Save(preset(1))
            }])
        );
        let key = s.inputs[0].key;
        assert_eq!(
            call(&s, "PTZCreateVirtualInput", "Input=1"),
            Ok(vec![
                Command::Ptz {
                    input: cam,
                    action: PtzAction::Save(preset(3))
                },
                Command::AddInput {
                    spec: InputSpec::PtzView {
                        camera: key,
                        preset: preset(3)
                    },
                    name: Some("Cam 3".into()),
                }
            ]),
            "the first preset no virtual input uses"
        );
    }
}
