//! PTZ cameras in the engine's state (Phase 7A, ADR 0018 § 6–7): which inputs are PTZ
//! cameras and where they are, whether they answer, the presets saved from VMA, and the
//! virtual inputs (an input showing a camera's picture and sending it to a preset when put
//! in Preview). `vma-io` runs one client per camera ([`IoRequest::PtzCamera`]) and sends
//! each command ([`IoRequest::Ptz`]).

use serde::{Deserialize, Serialize};
use vma_core::InputId;
pub use vma_ptz::{
    CameraAddress, Direction, FocusMode, LensMove, LinkEvent, MoveKind, PRESETS, Preset, Speed,
    Transport, ViscaCommand,
};

use crate::command::InputSpec;
use crate::state::{Change, IoRequest, Rejection, State};

/// Where a PTZ camera is, and the presets saved from VMA (the camera keeps the positions;
/// VMA only knows which ones it saved).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PtzSettings {
    pub address: CameraAddress,
    /// In order, without duplicates.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub saved: Vec<Preset>,
}

/// Whether a camera answers (ADR 0018 § 4).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PtzLink {
    /// Not probed yet.
    #[default]
    Connecting,
    Reachable,
    Unreachable {
        reason: String,
    },
}

/// The PTZ side of a camera input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputPtz {
    pub settings: PtzSettings,
    #[serde(default)]
    pub link: PtzLink,
    /// The last command refused by the camera, or dropped while it did not answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<String>,
}

/// What the operator (or the API) asks of a PTZ camera; sent to a camera input or to one of
/// its virtual inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PtzAction {
    /// Pan / tilt (`Stop` included).
    Move {
        direction: Direction,
        #[serde(default)]
        speed: Speed,
    },
    Zoom(LensMove),
    Focus(LensMove),
    FocusMode(FocusMode),
    /// Focus once (manual mode).
    FocusOnce,
    Home,
    /// The current position becomes `preset`.
    Save(Preset),
    /// Back to `preset`. A camera on air moves only once confirmed (Q5 of `phase-7.md`).
    Recall {
        preset: Preset,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        confirmed: bool,
    },
}

impl PtzAction {
    pub fn visca(self) -> ViscaCommand {
        match self {
            Self::Move { direction, speed } => ViscaCommand::PanTilt { direction, speed },
            Self::Zoom(lens) => ViscaCommand::Zoom(lens),
            Self::Focus(lens) => ViscaCommand::Focus(lens),
            Self::FocusMode(mode) => ViscaCommand::FocusMode(mode),
            Self::FocusOnce => ViscaCommand::FocusOnePush,
            Self::Home => ViscaCommand::Home,
            Self::Save(preset) => ViscaCommand::PresetSave(preset),
            Self::Recall { preset, .. } => ViscaCommand::PresetRecall(preset),
        }
    }
}

/// True for the inputs that may be PTZ cameras: a camera, a stream, NDI® (and the synthetic
/// sources of the tests and soaks).
pub fn may_be_ptz(spec: &InputSpec) -> bool {
    matches!(
        spec,
        InputSpec::Capture { .. }
            | InputSpec::Srt(_)
            | InputSpec::Ndi { .. }
            | InputSpec::Synthetic { .. }
            | InputSpec::SyncSource { .. }
            | InputSpec::TestSource { .. }
    )
}

impl State {
    /// The PTZ camera `input` drives: itself, or the camera of a virtual input.
    pub fn ptz_camera_of(&self, input: InputId) -> Option<InputId> {
        let found = self.input(input)?;
        match &found.spec {
            InputSpec::PtzView { camera, .. } => self
                .inputs
                .iter()
                .find(|i| i.key == *camera && i.ptz.is_some())
                .map(|i| i.id),
            _ => found.ptz.is_some().then_some(input),
        }
    }

    /// The input whose picture `input` shows: its camera for a virtual input (ADR 0018 § 7),
    /// itself otherwise. A virtual input without its camera shows black (no source).
    pub fn picture_of(&self, input: InputId) -> InputId {
        match self.input(input).map(|i| &i.spec) {
            Some(InputSpec::PtzView { camera, .. }) => self
                .inputs
                .iter()
                .find(|i| i.key == *camera)
                .map_or(input, |i| i.id),
            _ => input,
        }
    }

    /// The virtual inputs of `camera`, in display order.
    pub fn ptz_views(&self, camera: InputId) -> Vec<InputId> {
        let Some(key) = self.input(camera).map(|i| i.key) else {
            return Vec::new();
        };
        self.inputs
            .iter()
            .filter(|i| matches!(i.spec, InputSpec::PtzView { camera: c, .. } if c == key))
            .map(|i| i.id)
            .collect()
    }

    /// True while `camera`'s picture is on air: on Program, in a transition, or in an overlay
    /// shown — by itself or by one of its virtual inputs.
    pub fn ptz_on_air(&self, camera: InputId) -> bool {
        let shows = |input: Option<InputId>| input.is_some_and(|i| self.picture_of(i) == camera);
        let in_transition = self.transition.is_some_and(|t| {
            let (from, to) = t.inputs();
            shows(from) || shows(Some(to))
        });
        shows(self.program) || in_transition || self.overlays.iter().any(|o| o.on && shows(o.input))
    }

    /// `Command::SetPtzCamera`: `input` becomes a PTZ camera at `address`, or is no longer one
    /// (its virtual inputs go with it).
    pub(crate) fn set_ptz_camera(
        &mut self,
        input: InputId,
        address: Option<&CameraAddress>,
    ) -> Result<Change, Rejection> {
        let found = self.input(input).ok_or(Rejection::UnknownInput(input))?;
        let Some(address) = address else {
            if found.ptz.is_none() {
                return Ok(Change::default());
            }
            let mut change = Change::changed();
            for view in self.ptz_views(input) {
                if let Ok(removed) = self.remove_input(view) {
                    change.io.extend(removed.io);
                }
            }
            if let Ok(camera) = self.input_mut_ptz(input) {
                camera.ptz = None;
            }
            change.io.push(IoRequest::PtzCamera {
                input,
                address: None,
            });
            return Ok(change);
        };
        if !may_be_ptz(&found.spec) {
            return Err(Rejection::NotPtzCapable(input));
        }
        let address = CameraAddress {
            transport: address.transport,
            host: address.host.trim().to_owned(),
            port: address.port,
        };
        if address.host.is_empty() || address.port == 0 {
            return Err(Rejection::PtzAddress);
        }
        if found.ptz.as_ref().map(|p| &p.settings.address) == Some(&address) {
            return Ok(Change::default());
        }
        let saved = found
            .ptz
            .as_ref()
            .map(|p| p.settings.saved.clone())
            .unwrap_or_default();
        let camera = self.input_mut_ptz(input)?;
        camera.ptz = Some(InputPtz {
            settings: PtzSettings {
                address: address.clone(),
                saved,
            },
            link: PtzLink::Connecting,
            problem: None,
        });
        Ok(Change {
            changed: true,
            io: vec![IoRequest::PtzCamera {
                input,
                address: Some(address),
            }],
        })
    }

    /// `Command::Ptz`: `action` for the camera of `input`.
    pub(crate) fn ptz_action(
        &mut self,
        input: InputId,
        action: PtzAction,
    ) -> Result<Change, Rejection> {
        self.require_input(input)?;
        let camera = self.ptz_camera_of(input).ok_or(Rejection::NotPtz(input))?;
        let mut change = Change::default();
        match action {
            PtzAction::Recall { confirmed, .. } => {
                if !confirmed && self.ptz_on_air(camera) {
                    return Err(Rejection::PtzOnAir(camera));
                }
                if self
                    .ptz_held
                    .is_some_and(|view| self.ptz_camera_of(view) == Some(camera))
                {
                    self.ptz_held = None;
                    change.changed = true;
                }
            }
            PtzAction::Save(preset) => {
                let ptz = self.input_mut_ptz(camera)?.ptz.as_mut();
                if let Some(ptz) = ptz
                    && let Err(at) = ptz.settings.saved.binary_search(&preset)
                {
                    ptz.settings.saved.insert(at, preset);
                    change.changed = true;
                }
            }
            _ => {}
        }
        change.io.push(IoRequest::Ptz {
            input: camera,
            command: action.visca(),
        });
        Ok(change)
    }

    /// A report of the client of camera `input`. `true` if the state changed.
    pub(crate) fn ptz_event(&mut self, input: InputId, event: &LinkEvent) -> bool {
        let Some(ptz) = self
            .inputs
            .iter_mut()
            .find(|i| i.id == input)
            .and_then(|i| i.ptz.as_mut())
        else {
            return false;
        };
        let before = (ptz.link.clone(), ptz.problem.clone());
        match event {
            LinkEvent::Reachable => {
                ptz.link = PtzLink::Reachable;
                ptz.problem = None;
            }
            LinkEvent::Unreachable { reason } => {
                ptz.link = PtzLink::Unreachable {
                    reason: reason.clone(),
                };
            }
            LinkEvent::Refused { error, .. } => {
                ptz.problem = Some(format!("commande refusée : {error}"));
            }
            LinkEvent::Dropped { .. } => {
                ptz.problem = Some("commande écartée : caméra injoignable".into());
            }
        }
        before != (ptz.link.clone(), ptz.problem.clone())
    }

    /// After every command and event (ADR 0018 § 7): a virtual input just put in Preview
    /// sends its camera to its preset, once; if the camera is on air, nothing is sent and the
    /// recall waits for the operator's confirmation ([`State::ptz_held`]). `true` if the
    /// state changed.
    pub(crate) fn ptz_follow_preview(
        &mut self,
        before: Option<InputId>,
        io: &mut Vec<IoRequest>,
    ) -> bool {
        if self.preview == before {
            return false;
        }
        let held = self.ptz_held.take();
        let view = self
            .preview
            .and_then(|p| match self.input(p).map(|i| &i.spec) {
                Some(InputSpec::PtzView { preset, .. }) => Some((p, *preset)),
                _ => None,
            });
        if let Some((view, preset)) = view
            && let Some(camera) = self.ptz_camera_of(view)
        {
            if self.ptz_on_air(camera) {
                self.ptz_held = Some(view);
            } else {
                io.push(IoRequest::Ptz {
                    input: camera,
                    command: ViscaCommand::PresetRecall(preset),
                });
            }
        }
        held != self.ptz_held
    }

    /// The camera of a virtual input being added must exist (ADR 0018 § 7).
    pub(crate) fn check_ptz_view(&self, spec: &InputSpec) -> Result<(), Rejection> {
        match spec {
            InputSpec::PtzView { camera, .. } => {
                if self
                    .inputs
                    .iter()
                    .any(|i| i.key == *camera && i.ptz.is_some())
                {
                    Ok(())
                } else {
                    Err(Rejection::UnknownPtzCamera(*camera))
                }
            }
            _ => Ok(()),
        }
    }

    /// The presets of a camera read back from a project (no command: the camera keeps them).
    pub(crate) fn restore_ptz_saved(&mut self, input: InputId, saved: &[Preset]) {
        if let Some(ptz) = self
            .inputs
            .iter_mut()
            .find(|i| i.id == input)
            .and_then(|i| i.ptz.as_mut())
        {
            let mut saved = saved.to_vec();
            saved.sort_unstable();
            saved.dedup();
            ptz.settings.saved = saved;
        }
    }

    fn input_mut_ptz(&mut self, input: InputId) -> Result<&mut crate::InputState, Rejection> {
        self.inputs
            .iter_mut()
            .find(|i| i.id == input)
            .ok_or(Rejection::UnknownInput(input))
    }

    fn require_input(&self, input: InputId) -> Result<(), Rejection> {
        self.input(input)
            .map(|_| ())
            .ok_or(Rejection::UnknownInput(input))
    }
}

#[cfg(test)]
mod tests {
    use vma_core::{FrameRate, Resolution, Rgb8};

    use super::*;
    use crate::command::{Command, DeviceId};
    use crate::state::Format;

    fn camera_spec(name: &str) -> InputSpec {
        InputSpec::Capture {
            device: DeviceId(name.into()),
            audio: None,
        }
    }

    fn address(host: &str) -> CameraAddress {
        CameraAddress::new(Transport::Udp, host)
    }

    fn preset(n: u8) -> Preset {
        Preset::new(n).unwrap()
    }

    /// Two cameras (the first one PTZ), a colour.
    fn show() -> (State, InputId, InputId, InputId) {
        let mut s = State::new(Format {
            rate: FrameRate::FPS_50,
            resolution: Resolution::HD_1080,
        });
        for spec in [
            camera_spec("cam1"),
            camera_spec("cam2"),
            InputSpec::Color {
                color: Rgb8::new(1, 2, 3),
            },
        ] {
            s.apply(&Command::AddInput { spec, name: None }).unwrap();
        }
        let (cam1, cam2, color) = (s.inputs[0].id, s.inputs[1].id, s.inputs[2].id);
        let io = s
            .apply(&Command::SetPtzCamera {
                input: cam1,
                address: Some(address("10.0.0.5")),
            })
            .unwrap();
        assert_eq!(
            io,
            [IoRequest::PtzCamera {
                input: cam1,
                address: Some(address("10.0.0.5")),
            }]
        );
        (s, cam1, cam2, color)
    }

    fn add_view(s: &mut State, camera: InputId, n: u8) -> InputId {
        let key = s.input(camera).unwrap().key;
        s.apply(&Command::AddInput {
            spec: InputSpec::PtzView {
                camera: key,
                preset: preset(n),
            },
            name: Some(format!("Plan {n}")),
        })
        .unwrap();
        s.inputs.last().unwrap().id
    }

    fn recalls(io: &[IoRequest]) -> Vec<(InputId, Preset)> {
        io.iter()
            .filter_map(|r| match r {
                IoRequest::Ptz {
                    input,
                    command: ViscaCommand::PresetRecall(p),
                } => Some((*input, *p)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn only_a_live_source_with_an_address_becomes_a_ptz_camera() {
        let (mut s, cam1, cam2, color) = show();
        assert_eq!(
            s.apply(&Command::SetPtzCamera {
                input: color,
                address: Some(address("10.0.0.6")),
            }),
            Err(Rejection::NotPtzCapable(color))
        );
        assert_eq!(
            s.apply(&Command::SetPtzCamera {
                input: cam2,
                address: Some(address("  ")),
            }),
            Err(Rejection::PtzAddress)
        );
        let version = s.version;
        assert_eq!(
            s.apply(&Command::SetPtzCamera {
                input: cam1,
                address: Some(address(" 10.0.0.5 ")),
            }),
            Ok(vec![]),
            "same address: nothing to do"
        );
        assert_eq!(s.version, version);
        let ptz = s.input(cam1).unwrap().ptz.clone().unwrap();
        assert_eq!(ptz.link, PtzLink::Connecting);
        assert_eq!(ptz.settings.address.port, 52381);
    }

    #[test]
    fn actions_go_to_the_camera_of_a_camera_or_of_its_view() {
        let (mut s, cam1, cam2, _) = show();
        let view = add_view(&mut s, cam1, 3);
        let up = PtzAction::Move {
            direction: Direction::Up,
            speed: Speed::FASTEST,
        };
        for target in [cam1, view] {
            let io = s.apply(&Command::Ptz {
                input: target,
                action: up,
            });
            assert_eq!(
                io,
                Ok(vec![IoRequest::Ptz {
                    input: cam1,
                    command: ViscaCommand::PanTilt {
                        direction: Direction::Up,
                        speed: Speed::FASTEST
                    },
                }])
            );
        }
        assert_eq!(
            s.apply(&Command::Ptz {
                input: cam2,
                action: PtzAction::Home
            }),
            Err(Rejection::NotPtz(cam2))
        );
        // Saving remembers the preset, once, in order.
        for n in [5, 2, 5] {
            s.apply(&Command::Ptz {
                input: cam1,
                action: PtzAction::Save(preset(n)),
            })
            .unwrap();
        }
        let saved = &s.input(cam1).unwrap().ptz.as_ref().unwrap().settings.saved;
        assert_eq!(saved, &[preset(2), preset(5)]);
    }

    #[test]
    fn a_view_needs_its_camera() {
        let (mut s, _, cam2, _) = show();
        let key = s.input(cam2).unwrap().key;
        assert_eq!(
            s.apply(&Command::AddInput {
                spec: InputSpec::PtzView {
                    camera: key,
                    preset: preset(1),
                },
                name: None,
            }),
            Err(Rejection::UnknownPtzCamera(key)),
            "cam2 is no PTZ camera"
        );
    }

    /// G3: a view put in Preview sends its camera to its preset once; nothing while the
    /// camera is on air, until confirmed.
    #[test]
    fn a_view_in_preview_recalls_its_preset_once() {
        let (mut s, cam1, cam2, _) = show();
        let wide = add_view(&mut s, cam1, 1);
        let close = add_view(&mut s, cam1, 2);
        // cam1 on Program after `show`: put cam2 there.
        s.apply(&Command::CutDirect { input: cam2 }).unwrap();
        assert!(!s.ptz_on_air(cam1));

        let io = s.apply(&Command::SetPreview { input: wide }).unwrap();
        assert_eq!(recalls(&io), [(cam1, preset(1))]);
        let io = s.apply(&Command::SetPreview { input: wide }).unwrap();
        assert!(recalls(&io).is_empty(), "already in Preview: once");
        let io = s.apply(&Command::SetPreview { input: close }).unwrap();
        assert_eq!(recalls(&io), [(cam1, preset(2))]);
        assert_eq!(s.ptz_held, None);

        // The view goes on air: the camera is on air through it.
        let io = s.apply(&Command::Cut).unwrap();
        assert!(recalls(&io).is_empty(), "cam2 came to Preview: no PTZ");
        assert!(s.ptz_on_air(cam1));
        let io = s.apply(&Command::SetPreview { input: wide }).unwrap();
        assert!(recalls(&io).is_empty(), "on air: nothing sent");
        assert_eq!(s.ptz_held, Some(wide));
        assert_eq!(
            s.apply(&Command::Ptz {
                input: wide,
                action: PtzAction::Recall {
                    preset: preset(1),
                    confirmed: false
                },
            }),
            Err(Rejection::PtzOnAir(cam1))
        );
        let io = s
            .apply(&Command::Ptz {
                input: wide,
                action: PtzAction::Recall {
                    preset: preset(1),
                    confirmed: true,
                },
            })
            .unwrap();
        assert_eq!(recalls(&io), [(cam1, preset(1))]);
        assert_eq!(s.ptz_held, None, "confirmed");

        // Leaving the Preview drops a recall still held.
        s.apply(&Command::SetPreview { input: close }).unwrap();
        assert_eq!(s.ptz_held, Some(close));
        s.apply(&Command::SetPreview { input: cam2 }).unwrap();
        assert_eq!(s.ptz_held, None);
    }

    #[test]
    fn a_camera_in_an_overlay_or_a_transition_is_on_air() {
        let (mut s, cam1, cam2, _) = show();
        let view = add_view(&mut s, cam1, 1);
        s.apply(&Command::CutDirect { input: cam2 }).unwrap();
        s.apply(&Command::OverlayIn {
            channel: vma_core::OverlayChannel::ALL[0],
            input: view,
        })
        .unwrap();
        assert!(s.ptz_on_air(cam1), "its view in an overlay");
    }

    #[test]
    fn a_view_shows_its_camera_on_the_render_plan() {
        let (mut s, cam1, cam2, _) = show();
        let view = add_view(&mut s, cam1, 1);
        s.apply(&Command::SetPreview { input: view }).unwrap();
        let plan = s.plan();
        assert_eq!(plan.preview, Some(cam1));
        assert_eq!(s.preview, Some(view), "the state keeps the view");
        s.apply(&Command::CutDirect { input: view }).unwrap();
        assert_eq!(s.plan().program, Some(cam1));
        assert_eq!(s.picture_of(cam2), cam2);
    }

    #[test]
    fn a_camera_takes_its_views_along() {
        let (mut s, cam1, _, _) = show();
        let one = add_view(&mut s, cam1, 1);
        let two = add_view(&mut s, cam1, 2);
        assert_eq!(s.ptz_views(cam1), [one, two]);
        let io = s
            .apply(&Command::SetPtzCamera {
                input: cam1,
                address: None,
            })
            .unwrap();
        assert!(io.contains(&IoRequest::PtzCamera {
            input: cam1,
            address: None
        }));
        assert!(s.input(one).is_none() && s.input(two).is_none());
        assert!(s.input(cam1).unwrap().ptz.is_none());

        let (mut s, cam1, _, _) = show();
        let one = add_view(&mut s, cam1, 1);
        let io = s.apply(&Command::RemoveInput { input: cam1 }).unwrap();
        assert!(s.input(one).is_none());
        assert!(io.contains(&IoRequest::PtzCamera {
            input: cam1,
            address: None
        }));
    }

    #[test]
    fn the_link_follows_the_client() {
        let (mut s, cam1, _, _) = show();
        let link = |s: &State| s.input(cam1).unwrap().ptz.clone().unwrap();
        let version = s.version;
        s.apply_event(&crate::state::Event::Ptz {
            input: cam1,
            event: LinkEvent::Unreachable {
                reason: "pas de réponse".into(),
            },
        });
        assert_eq!(
            link(&s).link,
            PtzLink::Unreachable {
                reason: "pas de réponse".into()
            }
        );
        assert_eq!(s.version, version + 1);
        s.apply_event(&crate::state::Event::Ptz {
            input: cam1,
            event: LinkEvent::Dropped {
                command: ViscaCommand::Home,
            },
        });
        assert!(link(&s).problem.unwrap().contains("injoignable"));
        s.apply_event(&crate::state::Event::Ptz {
            input: cam1,
            event: LinkEvent::Reachable,
        });
        assert_eq!(link(&s).link, PtzLink::Reachable);
        assert_eq!(link(&s).problem, None);
    }

    #[test]
    fn a_project_keeps_the_cameras_their_presets_and_views() {
        let (mut s, cam1, _, _) = show();
        s.apply(&Command::Ptz {
            input: cam1,
            action: PtzAction::Save(preset(4)),
        })
        .unwrap();
        let view = add_view(&mut s, cam1, 4);
        s.apply_event(&crate::state::Event::Ptz {
            input: cam1,
            event: LinkEvent::Reachable,
        });
        let project = s.project();
        let json = serde_json::to_string(&project).unwrap();
        let back: crate::project::ProjectSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, project);

        let mut loaded = State::new(s.format);
        let io = loaded
            .apply(&Command::LoadProject {
                project: Box::new(back),
            })
            .unwrap();
        assert_eq!(loaded.load_problems, []);
        let camera = &loaded.inputs[0];
        let ptz = camera.ptz.as_ref().unwrap();
        assert_eq!(
            ptz.settings,
            s.input(cam1).unwrap().ptz.as_ref().unwrap().settings
        );
        assert_eq!(ptz.link, PtzLink::Connecting, "what runs is not kept");
        assert!(io.contains(&IoRequest::PtzCamera {
            input: camera.id,
            address: Some(address("10.0.0.5")),
        }));
        let views = loaded.ptz_views(camera.id);
        assert_eq!(views.len(), 1);
        assert_eq!(
            loaded.input(views[0]).unwrap().name,
            s.input(view).unwrap().name
        );
    }
}
