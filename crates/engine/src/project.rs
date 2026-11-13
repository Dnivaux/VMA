//! The settings of a show as a project keeps them (ADR 0013): what the operator set, not what
//! runs (no output status, lost signal, counters or playback position). Taken from the
//! [`State`] by [`State::project`], applied by [`Command::LoadProject`]. The project file
//! (`vma-project`) wraps it with its format and version.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use vma_core::{AudioBus, OverlayChannel};

use crate::command::{
    AutoPlay, Command, InputKey, InputSpec, MixDomain, MultiviewSettings, StingerSlot,
    TRANSITION_BUTTONS, TransitionSetting,
};
use crate::output_spec::OutputSpec;
use crate::replay::ReplayState;
use crate::state::{BusState, Format, InputAudio, Monitor, State};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSnapshot {
    pub format: Format,
    /// In display order.
    pub inputs: Vec<ProjectInput>,
    /// Positions in `inputs`.
    pub preview: Option<usize>,
    pub program: Option<usize>,
    pub buttons: [TransitionSetting; TRANSITION_BUTTONS],
    pub quick_play: TransitionSetting,
    pub auto_play: AutoPlay,
    pub mix_domain: MixDomain,
    /// Entry and exit of each overlay channel, in the order of [`OverlayChannel::ALL`].
    pub overlays: [TransitionSetting; OverlayChannel::COUNT],
    /// In the order of [`StingerSlot::ALL`].
    pub stingers: [ProjectStinger; StingerSlot::COUNT],
    /// In display order.
    pub outputs: Vec<ProjectOutput>,
    /// Indexed by [`AudioBus::index`].
    pub buses: [BusState; 3],
    pub monitor: Monitor,
    pub ftb_ms: u32,
    /// ADR 0014; its cells name inputs by their keys, kept by the project.
    #[serde(default)]
    pub multiview: MultiviewSettings,
    /// Phase 6: the replay's and the ISO's settings.
    #[serde(default)]
    pub replay: ProjectReplay,
}

/// The settings of the replay and of the ISO recordings (Phase 6), inputs named by their
/// position in [`ProjectSnapshot::inputs`]. Not the events: they belong to the recording of a
/// session, not to the show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectReplay {
    pub folder: Option<PathBuf>,
    pub keep_minutes: u32,
    pub kbps: u32,
    pub cameras: Vec<usize>,
    pub iso_folder: Option<PathBuf>,
    pub iso_inputs: Vec<usize>,
    pub iso_split_minutes: Option<u32>,
    /// « Rejouer »: speed (percent) and transition button.
    pub auto_speed: u8,
    pub auto_button: u8,
}

impl Default for ProjectReplay {
    fn default() -> Self {
        let replay = ReplayState::default();
        Self {
            folder: None,
            keep_minutes: replay.keep_minutes,
            kbps: replay.kbps,
            cameras: Vec::new(),
            iso_folder: None,
            iso_inputs: Vec::new(),
            iso_split_minutes: None,
            auto_speed: replay.auto_speed,
            auto_button: replay.auto_button,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectInput {
    pub key: InputKey,
    pub name: String,
    pub spec: InputSpec,
    /// Inputs with a timeline only.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub looping: bool,
    /// Inputs that may carry sound only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<InputAudio>,
    /// The file of the spec relative to the project's folder: written and resolved by the
    /// project file, ignored here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectStinger {
    pub file: Option<PathBuf>,
    pub cut_point_ms: Option<u32>,
    /// As [`ProjectInput::relative`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relative: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectOutput {
    pub name: String,
    /// Secrets are `***` once written: the project file keeps them apart.
    pub spec: OutputSpec,
    pub bus: AudioBus,
}

/// A part of a project that could not be loaded; the rest was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoadProblem {
    /// What it is about, as the operator knows it (input or output name, « Stinger 1 »…).
    pub what: String,
    pub reason: String,
}

impl State {
    /// The settings of the show, for a project.
    pub fn project(&self) -> ProjectSnapshot {
        let position = |id| self.inputs.iter().position(|i| Some(i.id) == id);
        ProjectSnapshot {
            format: self.format,
            inputs: self
                .inputs
                .iter()
                .map(|i| ProjectInput {
                    key: i.key,
                    name: i.name.clone(),
                    spec: i.spec.clone(),
                    looping: i.playback.is_some_and(|p| p.looping),
                    audio: i.audio,
                    relative: None,
                })
                .collect(),
            preview: position(self.preview),
            program: position(self.program),
            buttons: self.buttons,
            quick_play: self.quick_play,
            auto_play: self.auto_play,
            mix_domain: self.mix_domain,
            overlays: self.overlays.each_ref().map(|o| o.transition),
            stingers: self.stingers.each_ref().map(|s| ProjectStinger {
                file: s.file.clone(),
                cut_point_ms: s.cut_point_ms,
                relative: None,
            }),
            outputs: self
                .outputs
                .iter()
                .map(|o| ProjectOutput {
                    name: o.name.clone(),
                    spec: o.spec.clone(),
                    bus: o.bus,
                })
                .collect(),
            buses: self.audio.buses,
            monitor: self.audio.monitor.clone(),
            ftb_ms: self.ftb.duration_ms,
            multiview: self.multiview.clone(),
            replay: {
                let replay = &self.replay;
                let positions = |ids: &[vma_core::InputId]| {
                    ids.iter()
                        .filter_map(|id| self.inputs.iter().position(|i| i.id == *id))
                        .collect()
                };
                ProjectReplay {
                    folder: replay.folder.clone(),
                    keep_minutes: replay.keep_minutes,
                    kbps: replay.kbps,
                    cameras: positions(&replay.cameras),
                    iso_folder: replay.iso.folder.clone(),
                    iso_inputs: positions(&replay.iso.inputs),
                    iso_split_minutes: replay.iso.split_minutes,
                    auto_speed: replay.auto_speed,
                    auto_button: replay.auto_button,
                }
            },
        }
    }
}

impl ProjectSnapshot {
    /// An empty show at `format`: what « Nouveau » loads.
    pub fn empty(format: Format) -> Self {
        State::new(format).project()
    }

    /// The command that loads it.
    pub fn load(self) -> Command {
        Command::LoadProject {
            project: Box::new(self),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{Db, DeviceId, Effect, HtmlUrl};
    use crate::output_spec::{RecordSpec, RtmpSpec, Secret};
    use crate::state::{OutputStatus, Rejection};
    use vma_audio::SoloMode;
    use vma_core::{FrameRate, Resolution, Rgb8};

    const HD50: Format = Format {
        rate: FrameRate::FPS_50,
        resolution: Resolution::HD_1080,
    };

    fn add(state: &mut State, spec: InputSpec, name: &str) -> vma_core::InputId {
        state
            .apply(&Command::AddInput {
                spec,
                name: Some(name.into()),
            })
            .unwrap();
        state.inputs.last().unwrap().id
    }

    /// A show with every kind of setting changed from its default.
    fn show() -> State {
        let mut s = State::new(HD50);
        s.apply(&Command::SeedInputKeys { seed: 42 }).unwrap();
        let red = add(
            &mut s,
            InputSpec::Color {
                color: Rgb8::new(200, 0, 0),
            },
            "Rouge",
        );
        let clip = add(
            &mut s,
            InputSpec::File {
                path: "/media/générique.mp4".into(),
            },
            "Générique",
        );
        let cam = add(
            &mut s,
            InputSpec::Capture {
                device: DeviceId("cam-1".into()),
            },
            "Caméra 1",
        );
        let page = add(
            &mut s,
            InputSpec::Html {
                url: HtmlUrl::new("file:///scores/board.html"),
                size: None,
                css: Some("body { margin: 0 }".into()),
            },
            "Score",
        );
        add(
            &mut s,
            InputSpec::AudioDevice {
                device: DeviceId("mic".into()),
            },
            "Micro",
        );
        for command in [
            Command::SetLoop {
                input: clip,
                enabled: true,
            },
            Command::SetInputGain {
                input: cam,
                db: Db::new(-6.0).unwrap(),
            },
            Command::SetInputMute {
                input: page,
                mute: true,
            },
            Command::SetInputDelay {
                input: cam,
                ms: 120,
            },
            Command::SetInputBus {
                input: cam,
                bus: AudioBus::A,
                on: true,
            },
            Command::SetAudioFollowVideo {
                input: clip,
                on: true,
            },
            Command::SetPreview { input: cam },
            Command::CutDirect { input: clip },
            Command::SetTransition {
                button: 2,
                effect: Effect::Cut,
                duration_ms: 0,
            },
            Command::SetQuickPlay {
                effect: Effect::Fade,
                duration_ms: 800,
            },
            Command::SetAutoPlay(AutoPlay {
                play_on_transition: false,
                restart_on_transition: true,
                pause_after_transition: true,
            }),
            Command::SetMixDomain {
                domain: MixDomain::Linear,
            },
            Command::SetFadeToBlackDuration { ms: 2500 },
            Command::SetOverlayTransition {
                channel: OverlayChannel::ALL[2],
                effect: Effect::Cut,
                duration_ms: 0,
            },
            Command::SetStinger {
                slot: StingerSlot::ALL[1],
                file: Some("/media/stinger.webm".into()),
                cut_point_ms: Some(400),
            },
            Command::SetBusGain {
                bus: AudioBus::B,
                db: Db::new(-3.0).unwrap(),
            },
            Command::SetBusMute {
                bus: AudioBus::A,
                mute: true,
            },
            Command::SetMonitor {
                device: Some(DeviceId("casque".into())),
                solo_mode: SoloMode::Pfl,
            },
            Command::SetMultiview {
                settings: MultiviewSettings {
                    layout: crate::command::MultiviewLayout::PlusFourteen,
                    titles_below: true,
                    names_on_views: true,
                    slots: vec![crate::command::MultiviewSlot::Empty],
                },
            },
            Command::AddOutput {
                spec: OutputSpec::Rtmp(RtmpSpec {
                    url: "rtmp://live.example/app".into(),
                    key: Secret::new("k3y"),
                    video_kbps: Some(4500),
                }),
                name: Some("Direct".into()),
            },
            Command::UpdateOutput {
                output: State::DEFAULT_RECORDING,
                spec: OutputSpec::Record(RecordSpec {
                    folder: Some("/enregistrements".into()),
                    ..RecordSpec::default()
                }),
            },
            Command::SetOutputBus {
                output: State::DEFAULT_RECORDING,
                bus: AudioBus::B,
            },
            Command::SetReplaySession {
                folder: Some("/ssd/replay".into()),
                keep_minutes: 45,
                kbps: 60_000,
            },
            Command::SetReplayCameras {
                inputs: vec![cam, clip],
            },
            Command::SetReplayAuto {
                speed: 25,
                button: 2,
            },
            Command::SetIso {
                folder: Some("/iso".into()),
                inputs: vec![cam],
                split_minutes: Some(20),
            },
        ] {
            s.apply(&command).unwrap();
        }
        let _ = red;
        s
    }

    /// E1: a show, its project loaded into a fresh VMA, gives the same project back; keys and
    /// the recording's number are kept.
    #[test]
    fn a_project_loads_back_to_the_same_show() {
        let original = show();
        let project = original.project();
        let mut fresh = State::new(HD50);
        fresh.apply(&Command::SeedInputKeys { seed: 7 }).unwrap();
        add(
            &mut fresh,
            InputSpec::Color {
                color: Rgb8::new(0, 0, 9),
            },
            "Avant",
        );
        let io = fresh.apply(&project.clone().load()).unwrap();
        assert!(fresh.load_problems.is_empty(), "{:?}", fresh.load_problems);
        assert_eq!(fresh.project(), project);
        assert!(fresh.output(State::DEFAULT_RECORDING).is_some());
        let keys = |s: &State| s.inputs.iter().map(|i| i.key).collect::<Vec<_>>();
        assert_eq!(keys(&fresh), keys(&original));
        assert!(!io.is_empty(), "inputs and the stinger are opened");
        // Loading it again changes nothing.
        fresh.apply(&project.clone().load()).unwrap();
        assert_eq!(fresh.project(), project);
    }

    #[test]
    fn a_project_survives_json() {
        let project = show().project();
        let json = serde_json::to_string_pretty(&project).unwrap();
        assert!(!json.contains("k3y"), "no secret in the project");
        let back: ProjectSnapshot = serde_json::from_str(&json).unwrap();
        // The stream key comes back as « redacted »: the project file restores it.
        let mut expected = project;
        if let OutputSpec::Rtmp(rtmp) = &mut expected.outputs[1].spec {
            rtmp.key = Secret::Redacted;
        }
        assert_eq!(back, expected);
        let unknown = json.replacen("\"ftb_ms\"", "\"surprise\": 1, \"ftb_ms\"", 1);
        assert!(serde_json::from_str::<ProjectSnapshot>(&unknown).is_err());
    }

    /// E2: what cannot be loaded is listed, the rest is loaded.
    #[test]
    fn what_cannot_be_loaded_is_listed_and_the_rest_is_loaded() {
        let mut project = show().project();
        project.inputs[1].name = "x".repeat(500);
        project.format.rate = FrameRate::FPS_25;
        let mut fresh = State::new(HD50);
        fresh.apply(&project.load()).unwrap();
        let problems = &fresh.load_problems;
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert_eq!(problems[0].what, "Format");
        assert!(problems[1].reason.contains("name"), "{problems:?}");
        assert_eq!(fresh.inputs.len(), 4);
        assert_eq!(fresh.format, HD50, "the master format does not change");
    }

    /// Q7: never while an output runs or a transition is under way.
    #[test]
    fn no_load_while_an_output_runs_or_a_transition_is_under_way() {
        let mut s = show();
        let project = State::new(HD50).project();
        s.apply(&Command::StartRecording).unwrap();
        assert!(
            s.output(State::DEFAULT_RECORDING)
                .unwrap()
                .status
                .is_active()
        );
        let before = s.clone();
        assert_eq!(
            s.apply(&project.clone().load()),
            Err(Rejection::OutputActive(State::DEFAULT_RECORDING))
        );
        assert_eq!(s, before);

        let mut s = show();
        s.apply(&Command::Transition { button: 1 }).unwrap();
        assert_eq!(
            s.apply(&project.clone().load()),
            Err(Rejection::TransitionRunning)
        );

        // Nor while the replay or the ISO records: its inputs would go.
        let mut s = show();
        s.apply(&Command::StartReplayRecording).unwrap();
        assert_eq!(
            s.apply(&project.clone().load()),
            Err(Rejection::ReplayRecording)
        );
        let mut s = show();
        s.apply(&Command::StartIso).unwrap();
        assert_eq!(s.apply(&project.load()), Err(Rejection::IsoRecording));
        let _ = OutputStatus::Idle;
    }

    /// « Nouveau »: an empty show, the recording kept; a project without recording drops it.
    #[test]
    fn a_new_show_is_empty_and_a_project_may_have_no_recording() {
        let mut s = show();
        s.apply(&ProjectSnapshot::empty(HD50).load()).unwrap();
        assert!(s.inputs.is_empty());
        assert_eq!(s.project(), ProjectSnapshot::empty(HD50));

        let mut none = ProjectSnapshot::empty(HD50);
        none.outputs.clear();
        s.apply(&none.load()).unwrap();
        assert!(s.outputs.is_empty());
    }
}
