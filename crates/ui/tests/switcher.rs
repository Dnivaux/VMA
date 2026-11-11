//! A11 / A17 (UI side): every click, double-click, menu, T-Bar move and shortcut of the
//! switcher sends exactly the expected `Command`. Headless: egui driven through AccessKit by
//! `egui_kittest`, a `CommandProbe` instead of an engine, no GPU.

// Test code: panicking on failure is the intended behaviour.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::print_stdout
)]

use egui::accesskit::Role;
use egui::{Rect, TextureId, Vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use vma_core::{FrameRate, InputId, Resolution, Rgb8};
use vma_engine::{
    AutoPlay, Command, CommandProbe, CommandSender, DeviceId, Effect, Format, InputSpec, Level,
    MeterReading, MixDomain, Source, State, Timeline,
};
use vma_ui::{
    AudioView, CaptureChoice, FileKind, HeadphonesActivity, Images, IoView, OutputActivity,
    ProjectNotice, ProjectRequest, ProjectView, Screen, Switcher, View,
};

struct Rig {
    switcher: Switcher,
    state: State,
    commands: CommandSender,
    probe: CommandProbe,
    /// What `vma-io` reports.
    unavailable: Option<String>,
    devices: Vec<CaptureChoice>,
    timeline: Option<Timeline>,
    /// Files the switcher asked for (`take_file_request`).
    files: Vec<FileKind>,
    /// What `vma-io` reports for every running output.
    activity: Option<OutputActivity>,
    /// Folder dialogs the switcher asked for (`take_folder_request`).
    folders: Vec<vma_core::OutputId>,
    fade_to_black: f32,
    /// Screens the application lists.
    screens: Vec<Screen>,
    /// Meters of the mixer.
    levels: fn(Level) -> Option<[MeterReading; 2]>,
    /// Audio devices `vma-io` lists, and the headphones.
    audio_sources: Vec<CaptureChoice>,
    audio_sinks: Vec<CaptureChoice>,
    headphones: Option<HeadphonesActivity>,
    /// NDI®: why it is unavailable, and the sources seen.
    ndi_unavailable: Option<String>,
    ndi_sources: Vec<String>,
    /// Why HTML inputs are unavailable (`vma-html` missing).
    html_unavailable: Option<String>,
    /// Plain pictures for Program, Preview and the thumbnails (screenshots only).
    pictures: Option<[egui::TextureHandle; 3]>,
    /// Draw them (`harness_on(_, true)`).
    render: bool,
    /// The project bar: what it shows, what it asked.
    project_path: Option<std::path::PathBuf>,
    project_modified: bool,
    project_recent: Vec<std::path::PathBuf>,
    project_notice: Option<ProjectNotice>,
    project_requests: Vec<ProjectRequest>,
}

impl Rig {
    /// Commands sent by the UI since the last call.
    fn sent(&self) -> Vec<Command> {
        self.probe
            .take()
            .into_iter()
            .map(|(source, command)| {
                assert_eq!(source, Source::Ui);
                command
            })
            .collect()
    }
}

/// A state with three colour inputs (1 on Program, 2 in Preview) and a test pattern.
fn state() -> State {
    let mut state = State::new(Format {
        rate: FrameRate::FPS_50,
        resolution: Resolution::HD_1080,
    });
    for r in [255, 0, 0] {
        state
            .apply(&Command::AddInput {
                spec: InputSpec::Color {
                    color: Rgb8::new(r, 100, 0),
                },
                name: None,
            })
            .unwrap();
    }
    state
        .apply(&Command::AddInput {
            spec: InputSpec::TestSource {
                rate: FrameRate::FPS_50,
                resolution: Resolution::HD_1080,
            },
            name: None,
        })
        .unwrap();
    state
}

fn id(n: u32) -> InputId {
    InputId(n)
}

fn harness(state: State) -> Harness<'static, Rig> {
    harness_on(state, false)
}

/// The harness, drawn by wgpu when `render` (screenshots), else without a GPU.
fn harness_on(state: State, render: bool) -> Harness<'static, Rig> {
    let (commands, probe) = CommandSender::probe();
    let rig = Rig {
        switcher: Switcher::default(),
        state,
        commands,
        probe,
        unavailable: None,
        devices: Vec::new(),
        timeline: None,
        files: Vec::new(),
        activity: None,
        folders: Vec::new(),
        fade_to_black: 0.0,
        screens: Vec::new(),
        levels: |_| None,
        audio_sources: Vec::new(),
        audio_sinks: Vec::new(),
        headphones: Some(HeadphonesActivity {
            playing: true,
            ..HeadphonesActivity::default()
        }),
        ndi_unavailable: None,
        ndi_sources: vec!["STUDIO (Caméra 1)".into(), "STUDIO (Titres)".into()],
        html_unavailable: None,
        pictures: None,
        render,
        project_path: None,
        project_modified: false,
        project_recent: Vec::new(),
        project_notice: None,
        project_requests: Vec::new(),
    };
    let builder = Harness::builder()
        // A 1080p screen: the inputs and the mixer side by side.
        .with_size(Vec2::new(1920.0, 1040.0))
        // Short simulated frames, so that two clicks one frame apart make a double-click.
        .with_step_dt(0.05);
    let builder = if render { builder.wgpu() } else { builder };
    let mut harness = builder.build_ui_state(
        |ui, rig: &mut Rig| {
            if rig.render && rig.pictures.is_none() {
                // Flat colours instead of video: Program, Preview, thumbnails.
                let flat = |name: &str, rgb: [u8; 3]| {
                    let [r, g, b] = rgb;
                    let image = egui::ColorImage::new(
                        [16, 9],
                        vec![egui::Color32::from_rgb(r, g, b); 16 * 9],
                    );
                    ui.ctx()
                        .load_texture(name, image, egui::TextureOptions::default())
                };
                rig.pictures = Some([
                    flat("program", [70, 40, 40]),
                    flat("preview", [40, 50, 70]),
                    flat("thumbnails", [55, 55, 60]),
                ]);
            }
            let pictures = rig.pictures.as_ref().map(|p| p.each_ref().map(|t| t.id()));
            let full = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            let has_pictures = pictures.is_some();
            let no_thumbnail = move |_| has_pictures.then_some(full);
            let timeline = rig.timeline;
            let timeline = move |_| timeline;
            let no_mode = |_| None::<String>;
            let activity = rig.activity;
            let activity = move |_| activity;
            let view = View {
                state: &rig.state,
                commands: &rig.commands,
                images: Images {
                    program: pictures.map_or_else(TextureId::default, |p| p[0]),
                    preview: pictures.map_or_else(TextureId::default, |p| p[1]),
                    thumbnails: pictures.map_or_else(TextureId::default, |p| p[2]),
                    thumbnail_uv: &no_thumbnail,
                },
                io: IoView {
                    unavailable: rig.unavailable.as_deref(),
                    devices: &rig.devices,
                    timeline: &timeline,
                    capture_mode: &no_mode,
                    output_activity: &activity,
                    record_folder: Some(std::path::Path::new("/home/op/Vidéos")),
                    audio_sources: &rig.audio_sources,
                    audio_sinks: &rig.audio_sinks,
                    headphones: rig.headphones.as_ref(),
                    ndi_unavailable: rig.ndi_unavailable.as_deref(),
                    html_unavailable: rig.html_unavailable.as_deref(),
                    ndi_sources: &rig.ndi_sources,
                },
                screens: &rig.screens,
                frame: 0,
                fade_to_black: rig.fade_to_black,
                rejection: None,
                audio: AudioView {
                    levels: &rig.levels,
                },
                project: ProjectView {
                    path: rig.project_path.as_deref(),
                    modified: rig.project_modified,
                    recent: &rig.project_recent,
                    autosaves: &[],
                    notice: rig.project_notice.as_ref(),
                },
            };
            rig.switcher.show(ui, &view);
            rig.project_requests
                .extend(rig.switcher.take_project_request());
            rig.files.extend(rig.switcher.take_file_request());
            rig.folders.extend(rig.switcher.take_folder_request());
        },
        rig,
    );
    harness.run();
    assert!(harness.state().sent().is_empty(), "nothing sent at rest");
    harness
}

/// The `nth` button labelled `label`, in reading order (top to bottom, left to right).
fn click_button(harness: &Harness<'_, Rig>, label: &str, nth: usize) {
    let mut buttons: Vec<_> = harness
        .get_all_by_role_and_label(Role::Button, label)
        .collect();
    buttons.sort_by(|a, b| {
        let (a, b) = (a.rect(), b.rect());
        (a.top(), a.left())
            .partial_cmp(&(b.top(), b.left()))
            .unwrap()
    });
    assert!(
        buttons.len() > nth,
        "only {} « {label} » buttons",
        buttons.len()
    );
    buttons[nth].click();
}

#[test]
fn transition_column() {
    let mut h = harness(state());
    for (label, want) in [
        ("Quick Play", Command::QuickPlay { input: None }),
        ("Cut", Command::Cut),
        ("1 · Fade 0,5 s", Command::Transition { button: 1 }),
        ("2 · Fade 1 s", Command::Transition { button: 2 }),
        ("3 · Fade 1,5 s", Command::Transition { button: 3 }),
        ("4 · Cut", Command::Transition { button: 4 }),
    ] {
        // The centre column comes first in reading order (above the input bar).
        click_button(&h, label, 0);
        h.run();
        assert_eq!(h.state().sent(), [want], "{label}");
    }
}

#[test]
fn input_cards() {
    let mut h = harness(state());
    h.get_by_label("Vignette 3").click();
    h.run();
    assert_eq!(h.state().sent(), [Command::SetPreview { input: id(3) }]);

    click_button(&h, "×", 1);
    h.run();
    assert_eq!(h.state().sent(), [Command::RemoveInput { input: id(2) }]);

    click_button(&h, "QP", 2);
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::QuickPlay { input: Some(id(3)) }]
    );

    // Card « Cut » buttons come after the centre column's Cut.
    click_button(&h, "Cut", 4);
    h.run();
    assert_eq!(h.state().sent(), [Command::CutDirect { input: id(4) }]);
}

#[test]
fn loop_toggle_only_on_videos() {
    let mut state = state();
    state
        .apply(&Command::AddInput {
            spec: InputSpec::File {
                path: "clip.mp4".into(),
            },
            name: None,
        })
        .unwrap();
    let mut h = harness(state);
    let toggles: Vec<_> = h.get_all_by_label("Loop").collect();
    assert_eq!(toggles.len(), 1, "one video, one Loop toggle");
    toggles[0].click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::SetLoop {
            input: id(5),
            enabled: true
        }]
    );
}

#[test]
fn keyboard_shortcuts() {
    let mut h = harness(state());
    h.key_press(egui::Key::Num2);
    h.run();
    assert_eq!(h.state().sent(), [Command::SetPreview { input: id(2) }]);
    h.key_press(egui::Key::Num4);
    h.run();
    assert_eq!(h.state().sent(), [Command::SetPreview { input: id(4) }]);
    // No input 7: nothing.
    h.key_press(egui::Key::Num7);
    h.run();
    assert!(h.state().sent().is_empty());
    h.key_press(egui::Key::Space);
    h.run();
    assert_eq!(h.state().sent(), [Command::Cut]);
    h.key_press(egui::Key::Enter);
    h.run();
    assert_eq!(h.state().sent(), [Command::Transition { button: 1 }]);
}

#[test]
fn double_click_opens_input_settings_and_typing_does_not_trigger_shortcuts() {
    let mut h = harness(state());
    h.get_by_label("2 Couleur").click();
    h.step();
    h.get_by_label("2 Couleur").click();
    h.run();
    assert!(
        h.query_by_label("Réglages de l'input 2").is_some(),
        "settings window"
    );
    assert!(
        h.state().sent().is_empty(),
        "opening settings sends nothing"
    );

    // Typing a name with digits and spaces: no SetPreview, no Cut.
    let field = h.get_by_role(Role::TextInput);
    field.focus();
    h.run();
    h.get_by_role(Role::TextInput).type_text(" 1 2");
    h.key_press(egui::Key::Space);
    h.run();
    assert!(
        h.state().sent().is_empty(),
        "shortcuts are off while typing"
    );

    // Leaving the field renames the input.
    h.get_by_label("Vignette 1").click();
    h.run();
    let sent = h.state().sent();
    assert!(
        sent.contains(&Command::RenameInput {
            input: id(2),
            name: "Couleur 1 2".into()
        }),
        "{sent:?}"
    );
}

#[test]
fn transition_button_menu_sets_effect() {
    let mut h = harness(state());
    let mut buttons: Vec<_> = h
        .get_all_by_role_and_label(Role::Button, "2 · Fade 1 s")
        .collect();
    buttons.remove(0).click_secondary();
    h.run();
    h.get_by_role_and_label(Role::RadioButton, "Cut").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::SetTransition {
            button: 2,
            effect: Effect::Cut,
            duration_ms: 1000
        }]
    );
}

#[test]
fn t_bar_sends_fader_values_and_ignores_the_rest_of_a_completed_drag() {
    let mut h = harness(state());
    let rect = h.get_by_label("T-Bar").rect();
    let at = |f: f32| egui::pos2(rect.center().x, rect.top() + rect.height() * f);

    h.drag_at(at(0.0));
    h.run();
    h.hover_at(at(0.5));
    h.run();
    h.hover_at(at(1.0));
    h.run();
    // Moving back after reaching the end must not start another transition.
    h.hover_at(at(0.6));
    h.run();
    h.drop_at(at(0.6));
    h.run();
    let values: Vec<u8> = h
        .state()
        .sent()
        .into_iter()
        .map(|c| match c {
            Command::SetFader { value } => value,
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(values.first(), Some(&0));
    assert!(values.iter().any(|v| (126..=129).contains(v)), "{values:?}");
    assert_eq!(values.last(), Some(&255), "{values:?}");
    assert!(
        values.windows(2).all(|w| w[0] < w[1]),
        "monotonic: {values:?}"
    );
}

#[test]
fn add_menu() {
    let mut h = harness(state());
    h.get_by_label("Ajouter un input").click();
    h.run();
    h.get_by_label("Mire").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::AddInput {
            spec: InputSpec::TestSource {
                rate: FrameRate::FPS_50,
                resolution: Resolution::HD_1080
            },
            name: None
        }]
    );
    h.get_by_label("Ajouter un input").click();
    h.run();
    h.get_by_label("Couleur").hover();
    h.run();
    h.get_by_label("Blanc").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::AddInput {
            spec: InputSpec::Color { color: Rgb8::WHITE },
            name: None
        }]
    );
}

#[test]
fn settings_window() {
    let mut h = harness(state());
    h.get_by_label("Réglages").click();
    h.run();
    h.get_by_label("… depuis le début").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::SetAutoPlay(AutoPlay {
            play_on_transition: true,
            restart_on_transition: true,
            pause_after_transition: true
        })]
    );
    h.get_by_label("Lumière linéaire").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::SetMixDomain {
            domain: MixDomain::Linear
        }]
    );
}

/// D9 (UI): buttons 1–4 of each card toggle that input in that overlay channel; the
/// channel it is in shows selected; a sound-only input has none; « Réglages » sets the
/// transition of each channel.
#[test]
fn overlay_buttons_and_channel_settings() {
    use vma_core::OverlayChannel;
    let channel = |n| OverlayChannel::new(n).unwrap();
    let mut state = state();
    state
        .apply(&Command::OverlayIn {
            channel: channel(3),
            input: id(2),
        })
        .unwrap();
    state
        .apply(&Command::AddInput {
            spec: InputSpec::AudioDevice {
                device: DeviceId("alsa_input.mic".into()),
            },
            name: None,
        })
        .unwrap();
    let mut h = harness(state);
    // Four cards with pictures, one sound-only: four buttons per channel.
    for n in ["1", "2", "3", "4"] {
        assert_eq!(
            h.get_all_by_role_and_label(Role::Button, n).count(),
            4,
            "{n}"
        );
    }
    click_button(&h, "1", 0);
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::OverlayToggle {
            channel: channel(1),
            input: id(1)
        }]
    );
    click_button(&h, "3", 1);
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::OverlayToggle {
            channel: channel(3),
            input: id(2)
        }]
    );
    let mut threes: Vec<_> = h.get_all_by_role_and_label(Role::Button, "3").collect();
    threes.sort_by(|a, b| a.rect().left().partial_cmp(&b.rect().left()).unwrap());
    let selected: Vec<bool> = threes
        .iter()
        .map(|b| b.accesskit_node().toggled() == Some(egui::accesskit::Toggled::True))
        .collect();
    assert_eq!(
        selected,
        [false, true, false, false],
        "input 2 in channel 3"
    );
    drop(threes);

    h.get_by_label("Réglages").click();
    h.run();
    h.get_by_label("Overlay 2").click();
    h.run();
    h.get_all_by_label("Cut").last().unwrap().click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::SetOverlayTransition {
            channel: channel(2),
            effect: Effect::Cut,
            duration_ms: 500
        }]
    );
}

#[test]
fn ui_follows_the_new_state_on_the_next_frame() {
    let mut h = harness(state());
    assert!(h.query_by_label("1 Plateau").is_none());
    h.state_mut()
        .state
        .apply(&Command::RenameInput {
            input: id(1),
            name: "Plateau".into(),
        })
        .unwrap();
    h.step();
    assert!(h.query_by_label("1 Plateau").is_some());
}

/// « Ajouter » : a connected capture device is added under its name; « Vidéo… » and
/// « Image… » ask the caller for a file (the dialog never runs inside the switcher).
#[test]
fn add_menu_capture_devices_and_files() {
    let mut h = harness(state());
    h.state_mut().devices = vec![CaptureChoice {
        id: DeviceId("v4l2:pci-0000:00:14.0-usb-0:2:1.0".into()),
        name: "Webcam HD".into(),
    }];
    h.run();
    h.get_by_label("Ajouter un input").click();
    h.run();
    h.get_by_label("Caméra / capture").hover();
    h.run();
    h.get_by_label("Webcam HD").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::AddInput {
            spec: InputSpec::Capture {
                device: DeviceId("v4l2:pci-0000:00:14.0-usb-0:2:1.0".into())
            },
            name: Some("Webcam HD".into())
        }]
    );
    for (label, kind) in [("Vidéo…", FileKind::Video), ("Image…", FileKind::Image)] {
        h.get_by_label("Ajouter un input").click();
        h.run();
        h.get_by_label(label).click();
        h.run();
        assert_eq!(std::mem::take(&mut h.state_mut().files), [kind], "{label}");
        assert!(h.state().sent().is_empty(), "the input comes with the file");
    }
}

/// GStreamer missing: a warning stays in the toolbar; capture and video cannot be added,
/// images still can.
#[test]
fn without_gstreamer_capture_and_video_are_disabled() {
    let mut h = harness(state());
    h.state_mut().unavailable = Some("simulated".into());
    h.run();
    assert!(
        h.query_by_label("GStreamer introuvable : caméras et vidéos désactivées")
            .is_some()
    );
    h.get_by_label("Ajouter un input").click();
    h.run();
    h.get_by_label("Vidéo…").click();
    h.run();
    assert!(h.state().files.is_empty(), "video disabled");
    if h.query_by_label("Image…").is_none() {
        h.get_by_label("Ajouter un input").click();
        h.run();
    }
    h.get_by_label("Image…").click();
    h.run();
    assert_eq!(h.state().files, [FileKind::Image]);
}

/// Under the Preview (or Program) showing a video: play / pause, back to the start, and the
/// position slider (sent once, when set), with position / duration shown.
#[test]
fn video_controls() {
    let mut state = state();
    state
        .apply(&Command::AddInput {
            spec: InputSpec::File {
                path: "clip.mp4".into(),
            },
            name: None,
        })
        .unwrap();
    state.apply(&Command::SetPreview { input: id(5) }).unwrap();
    let mut h = harness(state);
    h.state_mut().timeline = Some(Timeline {
        position_ms: 12_400,
        duration_ms: Some(63_000),
        ended: false,
    });
    h.run();
    assert!(h.query_by_label("00:12 / 01:03").is_some());
    assert!(
        h.query_by_label("Pause").is_none(),
        "paused: one Lecture button"
    );
    h.get_by_label("Lecture").click();
    h.run();
    assert_eq!(h.state().sent(), [Command::Play { input: id(5) }]);
    h.get_by_label("Début").click();
    h.run();
    assert_eq!(h.state().sent(), [Command::Restart { input: id(5) }]);
    // The position slider is named by the time it shows (the T-Bar is a slider too).
    h.get_by_role_and_label(Role::Slider, "00:12 / 01:03")
        .set_value_accesskit(30_000.0);
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::SetPosition {
            input: id(5),
            ms: 30_000
        }]
    );

    // Playing (state from the engine): the button pauses.
    h.state_mut()
        .state
        .apply(&Command::Play { input: id(5) })
        .unwrap();
    h.run();
    h.get_by_label("Pause").click();
    h.run();
    assert_eq!(h.state().sent(), [Command::Pause { input: id(5) }]);
}

// Outputs (Phase 2 step 5): A11 for the output controls, B13 and B15 on the UI side.

use vma_core::{Container, OutputId};
use vma_engine::{Event, OutputSpec, OutputStatus, RecordSpec, RtmpSpec, Secret};

const REC: OutputId = State::DEFAULT_RECORDING;

/// `state` with its recording (and an RTMP stream if `stream`) running.
fn running(mut state: State, stream: bool) -> State {
    if stream {
        state
            .apply(&Command::AddOutput {
                spec: OutputSpec::Rtmp(RtmpSpec {
                    url: "rtmp://live.example.net/app".into(),
                    key: Secret::new("k"),
                    video_kbps: None,
                }),
                name: None,
            })
            .unwrap();
        state
            .apply(&Command::StartStreaming { slot: None })
            .unwrap();
    }
    state.apply(&Command::StartRecording).unwrap();
    for o in state.outputs.clone() {
        state.apply_event(&Event::OutputRunning {
            output: o.id,
            run: o.run,
            encoder: "nvh264enc + avenc_aac".into(),
            target: None,
        });
    }
    state
}

#[test]
fn record_and_stream_buttons() {
    let mut h = harness(state());
    h.get_by_label("Enregistrer").click();
    h.run();
    assert_eq!(h.state().sent(), [Command::StartRecording]);
    // No destination yet: « Streamer » does nothing.
    h.get_by_label("Streamer").click();
    h.run();
    assert_eq!(h.state().sent(), []);

    let mut h = harness(running(state(), true));
    h.state_mut().activity = Some(OutputActivity {
        running_for: std::time::Duration::from_secs(83),
        ..OutputActivity::default()
    });
    h.run();
    h.get_by_label("REC 00:01:23").click();
    h.run();
    assert_eq!(h.state().sent(), [Command::StopRecording]);
    h.get_by_label("EN DIRECT 00:01:23").click();
    h.run();
    assert_eq!(h.state().sent(), [Command::StopStreaming { slot: None }]);
}

#[test]
fn ftb_button() {
    let mut h = harness(state());
    click_button(&h, "FTB", 0);
    h.run();
    assert_eq!(h.state().sent(), [Command::FadeToBlack]);
}

fn open_outputs(h: &mut Harness<'_, Rig>) {
    h.get_by_label("Sorties").click();
    h.run();
}

#[test]
fn a_stream_destination_is_added_with_its_key_hidden() {
    // B13 (UI side): the key is typed in a password field and shown nowhere.
    const KEY: &str = "live_93b2e7d4c1";
    let mut h = harness(state());
    open_outputs(&mut h);
    h.get_by_label("+ Destination de stream").click();
    h.run();
    for (field, text) in [
        ("Adresse", "rtmps://live.example.net/app"),
        ("Clé de stream", KEY),
    ] {
        h.get_by_label(field).focus();
        h.run();
        h.get_by_label(field).type_text(text);
        h.run();
    }
    assert!(
        h.query_by_label_contains(KEY).is_none(),
        "key shown as a label"
    );
    assert!(h.query_by_value(KEY).is_none(), "key shown as a value");
    h.get_by_label("Ajouter").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::AddOutput {
            spec: OutputSpec::Rtmp(RtmpSpec {
                url: "rtmps://live.example.net/app".into(),
                key: Secret::new(KEY),
                video_kbps: None,
            }),
            name: None,
        }]
    );
}

#[test]
fn output_settings_start_stop_and_remove() {
    let mut h = harness(state());
    open_outputs(&mut h);
    h.get_by_label("MP4").click();
    h.run();
    let mp4 = OutputSpec::Record(RecordSpec {
        container: Container::Mp4,
        ..RecordSpec::default()
    });
    h.get_by_label("Appliquer").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::UpdateOutput {
            output: REC,
            spec: mp4.clone()
        }]
    );
    // Not applied yet (the probe applies nothing): « Démarrer » sends the settings first.
    h.get_by_label("Démarrer").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [
            Command::UpdateOutput {
                output: REC,
                spec: mp4
            },
            Command::StartOutput { output: REC }
        ]
    );
    // The folder dialog belongs to the caller; its answer goes into the draft.
    h.get_by_label("Annuler").click();
    h.get_by_label("Parcourir…").click();
    h.run();
    assert_eq!(h.state().folders, [REC]);
    h.state_mut()
        .switcher
        .set_record_folder(REC, "/media/disque".into());
    h.run();
    h.get_by_label("Appliquer").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::UpdateOutput {
            output: REC,
            spec: OutputSpec::Record(RecordSpec {
                folder: Some("/media/disque".into()),
                ..RecordSpec::default()
            }),
        }]
    );
    h.get_by_label("Supprimer").click();
    h.run();
    assert_eq!(h.state().sent(), [Command::RemoveOutput { output: REC }]);

    let mut h = harness(running(state(), false));
    open_outputs(&mut h);
    assert!(
        h.query_by_label("Appliquer").is_none(),
        "no edit while running"
    );
    h.get_by_label("Arrêter").click();
    h.run();
    assert_eq!(h.state().sent(), [Command::StopOutput { output: REC }]);
}

#[test]
fn output_statistics_show_every_figure() {
    // B15 (UI side).
    let state = running(state(), true);
    let mut lost = state.outputs[1].clone();
    lost.status = OutputStatus::Reconnecting {
        attempt: 2,
        reason: "connection refused".into(),
    };
    let outputs = [state.outputs[0].clone(), lost];
    let counters = vma_engine::OutputCountersSnapshot {
        first_frame: Some(10),
        delivered: 4321,
        dropped: 7,
        queued: 2,
    };
    let activity = OutputActivity {
        running_for: std::time::Duration::from_secs(3723),
        bytes: Some(12_345_678),
        kbps: Some(40_000),
        reconnections: 0,
    };
    let h = Harness::builder()
        .with_size(Vec2::new(400.0, 600.0))
        .build_ui(move |ui| {
            let rows: Vec<_> = outputs
                .iter()
                .map(|o| vma_ui::OutputStatsRow {
                    output: o,
                    counters: o.spec.is_recording().then_some(counters),
                    activity: Some(if o.spec.is_recording() {
                        activity
                    } else {
                        OutputActivity {
                            running_for: std::time::Duration::from_secs(10),
                            reconnections: 5,
                            ..OutputActivity::default()
                        }
                    }),
                })
                .collect();
            vma_ui::output_stats(ui, &rows);
        });
    for text in [
        "Enregistrement",
        "en cours",
        "nvh264enc + avenc_aac",
        "01:02:03",
        "40,0 Mb/s",
        "12,3 Mo",
        "4321",
        "7",
        "Stream 1",
        "reconnexion (essai 2)",
        "connection refused",
        "5",
    ] {
        assert!(h.query_by_label(text).is_some(), "« {text} » missing");
    }
    for label in [
        "images envoyées",
        "images perdues",
        "file d'attente",
        "reconnexions",
    ] {
        assert!(
            h.query_by_label_contains(label).is_some(),
            "« {label} » missing"
        );
    }
}

fn screens() -> Vec<Screen> {
    vec![
        Screen {
            name: "eDP-1".into(),
            width: 2560,
            height: 1600,
            refresh_mhz: Some(60_000),
        },
        Screen {
            name: "DP-2".into(),
            width: 1920,
            height: 1080,
            refresh_mhz: Some(50_000),
        },
    ]
}

/// `state` with a full-screen output on screen `screen`, started if `on`.
fn with_fullscreen(mut state: State, screen: &str, on: bool) -> (State, OutputId) {
    state
        .apply(&Command::AddOutput {
            spec: OutputSpec::Fullscreen(FullscreenSpec {
                screen: Some(screen.into()),
            }),
            name: None,
        })
        .unwrap();
    let id = state.outputs.last().unwrap().id;
    if on {
        state.apply(&Command::StartOutput { output: id }).unwrap();
    }
    (state, id)
}

use vma_engine::FullscreenSpec;

#[test]
fn fullscreen_output_screen_choice_start_and_stop() {
    // A11 (UI side) for the full screen: add, choose the screen, start, stop.
    let mut h = harness(state());
    h.state_mut().screens = screens();
    open_outputs(&mut h);
    h.get_by_label("+ Plein écran").click();
    h.run();
    // The second screen is offered first (the first is often the operator's).
    assert_eq!(
        h.state().sent(),
        [Command::AddOutput {
            spec: OutputSpec::Fullscreen(FullscreenSpec {
                screen: Some("DP-2".into())
            }),
            name: None,
        }]
    );

    let (fs_state, fs) = with_fullscreen(state(), "DP-2", false);
    let mut h = harness(fs_state);
    h.state_mut().screens = screens();
    open_outputs(&mut h);
    assert!(
        h.query_by_label("+ Plein écran").is_none(),
        "one full screen"
    );
    h.get_by_label("Écran").click();
    h.run();
    h.get_by_label("eDP-1 · 2560 × 1600 · 60 Hz").click();
    h.run();
    // The recording's card comes first.
    click_button(&h, "Appliquer", 1);
    h.run();
    let edp = OutputSpec::Fullscreen(FullscreenSpec {
        screen: Some("eDP-1".into()),
    });
    assert_eq!(
        h.state().sent(),
        [Command::UpdateOutput {
            output: fs,
            spec: edp.clone()
        }]
    );
    click_button(&h, "Démarrer", 1);
    h.run();
    assert_eq!(
        h.state().sent(),
        [
            Command::UpdateOutput {
                output: fs,
                spec: edp
            },
            Command::StartOutput { output: fs }
        ]
    );

    let (fs_state, fs) = with_fullscreen(state(), "DP-2", true);
    let mut h = harness(fs_state);
    h.state_mut().screens = screens();
    open_outputs(&mut h);
    assert!(h.query_by_label_contains("non branché").is_none());
    assert!(
        h.query_by_label_contains("saccades").is_none(),
        "50 Hz for 50p"
    );
    h.get_all_by_label("Arrêter").last().unwrap().click();
    h.run();
    assert_eq!(h.state().sent(), [Command::StopOutput { output: fs }]);
}

#[test]
fn fullscreen_output_warns_about_its_screen() {
    // Screen unplugged while the output is on: on, but not shown.
    let mut h = harness(with_fullscreen(state(), "HDMI-A-1", true).0);
    h.state_mut().screens = screens();
    open_outputs(&mut h);
    assert!(
        h.query_by_label("Écran « HDMI-A-1 » non branché : la sortie n'est pas affichée")
            .is_some()
    );
    // 60 Hz for a 50p Program: uneven.
    let mut h = harness(with_fullscreen(state(), "eDP-1", false).0);
    h.state_mut().screens = screens();
    open_outputs(&mut h);
    assert!(h.query_by_label_contains("saccades").is_some());
    assert!(h.query_by_label_contains("Un seul écran").is_none());
    // A single screen: the output covers the UI.
    let mut h = harness(with_fullscreen(state(), "DP-2", false).0);
    h.state_mut().screens = screens()[1..].to_vec();
    open_outputs(&mut h);
    assert!(h.query_by_label_contains("Un seul écran").is_some());
}

// ── Mélangeur (Phase 3 step 4): A11 / C13 (UI side), C7 (meters shown) ──

/// `state()` with a video file, input 5 (« 5 clip »): it has sound.
fn with_sound() -> State {
    let mut state = state();
    state
        .apply(&Command::AddInput {
            spec: InputSpec::File {
                path: "clip.mp4".into(),
            },
            name: None,
        })
        .unwrap();
    state
}

#[test]
fn mixer_strip_buttons_send_their_commands() {
    use vma_core::AudioBus;
    use vma_engine::SoloMode;

    let mut h = harness(with_sound());
    // Strips: 5 clip, Master, Bus A, Bus B (only the video has sound).
    assert_eq!(h.get_all_by_label("Suit l'image").count(), 1);
    let expect = |h: &mut Harness<'_, Rig>, command: Command| {
        h.run();
        assert_eq!(h.state().sent(), [command]);
    };
    click_button(&h, "Muet", 0);
    expect(
        &mut h,
        Command::SetInputMute {
            input: id(5),
            mute: true,
        },
    );
    click_button(&h, "Solo", 0);
    expect(
        &mut h,
        Command::SetInputSolo {
            input: id(5),
            solo: true,
        },
    );
    h.get_by_label("M").click();
    expect(
        &mut h,
        Command::SetInputBus {
            input: id(5),
            bus: AudioBus::Master,
            on: false,
        },
    );
    h.get_by_label("A").click();
    expect(
        &mut h,
        Command::SetInputBus {
            input: id(5),
            bus: AudioBus::A,
            on: true,
        },
    );
    h.get_by_label("Suit l'image").click();
    expect(
        &mut h,
        Command::SetAudioFollowVideo {
            input: id(5),
            on: false,
        },
    );
    // Mute of the Master strip, then of bus B.
    click_button(&h, "Muet", 1);
    expect(
        &mut h,
        Command::SetBusMute {
            bus: AudioBus::Master,
            mute: true,
        },
    );
    click_button(&h, "Muet", 3);
    expect(
        &mut h,
        Command::SetBusMute {
            bus: AudioBus::B,
            mute: true,
        },
    );
    h.get_by_label("PFL").click();
    expect(
        &mut h,
        Command::SetMonitor {
            device: None,
            solo_mode: SoloMode::Pfl,
        },
    );
    // The mixer can be hidden.
    h.get_by_label("Mélangeur").click();
    h.run();
    assert_eq!(h.query_all_by_label("Suit l'image").count(), 0);
    assert!(h.state().sent().is_empty());
}

#[test]
fn faders_send_their_level_and_double_click_goes_back_to_zero() {
    use vma_core::AudioBus;
    use vma_engine::{Db, MAX_GAIN_DB};

    let mut h = harness(with_sound());
    let rect = h.get_by_label("Fader 5 clip").rect();
    let at = |f: f32| egui::pos2(rect.center().x, rect.bottom() - rect.height() * f);
    h.drag_at(at(0.5));
    h.run();
    h.hover_at(at(1.0));
    h.run();
    h.hover_at(at(0.0));
    h.run();
    h.drop_at(at(0.0));
    h.run();
    let levels: Vec<Db> = h
        .state()
        .sent()
        .into_iter()
        .map(|c| match c {
            Command::SetInputGain { input, db } if input == id(5) => db,
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(levels.first(), Some(&Db::new(-25.0).unwrap()), "{levels:?}");
    assert!(
        levels.contains(&Db::new(MAX_GAIN_DB).unwrap()),
        "{levels:?}"
    );
    assert_eq!(levels.last(), Some(&Db::SILENT), "{levels:?}");

    // The state has a level: a double-click sends 0 dB.
    let mut state = with_sound();
    state
        .apply(&Command::SetBusGain {
            bus: AudioBus::A,
            db: Db::new(-6.0).unwrap(),
        })
        .unwrap();
    let mut h = harness(state);
    h.get_by_label("Fader Bus A").click();
    h.run();
    h.get_by_label("Fader Bus A").click();
    h.run();
    assert!(h.state().sent().contains(&Command::SetBusGain {
        bus: AudioBus::A,
        db: Db::ZERO
    }));
}

#[test]
fn the_delay_of_an_input_is_sent() {
    let mut h = harness(with_sound());
    let rect = h.get_by_role(Role::SpinButton).rect();
    h.drag_at(rect.center());
    h.run();
    h.hover_at(rect.center() + Vec2::new(40.0, 0.0));
    h.run();
    h.drop_at(rect.center() + Vec2::new(40.0, 0.0));
    h.run();
    let sent = h.state().sent();
    assert!(!sent.is_empty());
    for command in sent {
        assert!(
            matches!(command, Command::SetInputDelay { input, ms } if input == id(5) && ms > 0 && ms <= 1_000),
            "{command:?}"
        );
    }
}

/// C7 (UI side): the meters show what the engine measures; without sound, they say so.
#[test]
fn meters_show_the_levels() {
    use vma_core::AudioBus;

    let mut h = harness(with_sound());
    h.state_mut().levels = |level| {
        let sine = MeterReading {
            peak: [0.5, 0.25],
            hold: [0.5, 0.5],
            rms: [0.354, 0.1],
        };
        match level {
            Level::Input(_) | Level::Bus(AudioBus::Master) => Some([sine, sine]),
            _ => None,
        }
    };
    h.run();
    h.get_by_label("Vumètre 5 clip : crête −6,0 dB, RMS −9,0 dB");
    h.get_by_label("Vumètre Master : crête −6,0 dB, RMS −9,0 dB");
    h.get_by_label("Vumètre Bus A : pas de son");
    h.get_by_label("Vumètre Casque : pas de son");
}

#[test]
fn each_output_chooses_its_bus_while_stopped() {
    use vma_core::AudioBus;

    let mut h = harness(state());
    open_outputs(&mut h);
    click_button(&h, "Bus A", 0);
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::SetOutputBus {
            output: REC,
            bus: AudioBus::A
        }]
    );
    // Running: the choice is frozen.
    let mut h = harness(running(state(), false));
    open_outputs(&mut h);
    click_button(&h, "Bus B", 0);
    h.run();
    assert!(h.state().sent().is_empty());
}

// ── Audio devices (Phase 3 step 5): C12 (UI side) ──

#[test]
fn a_microphone_is_added_from_the_menu() {
    let mut h = harness(state());
    h.state_mut().audio_sources = vec![CaptureChoice {
        id: DeviceId("alsa:alsa_input.usb-Mic-00.mono-fallback".into()),
        name: "Micro USB".into(),
    }];
    h.run();
    h.get_by_label("Ajouter un input").click();
    h.run();
    h.get_by_label("Son (micro, ligne)").hover();
    h.run();
    h.get_by_label("Micro USB").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::AddInput {
            spec: InputSpec::AudioDevice {
                device: DeviceId("alsa:alsa_input.usb-Mic-00.mono-fallback".into()),
            },
            name: Some("Micro USB".into()),
        }]
    );
}

/// A sound-only input has no Quick Play nor Cut on its card, and says what it is.
#[test]
fn a_sound_only_card() {
    let mut state = state();
    state
        .apply(&Command::AddInput {
            spec: InputSpec::AudioDevice {
                device: DeviceId("mic".into()),
            },
            name: Some("Micro".into()),
        })
        .unwrap();
    let mic = state.inputs.last().unwrap().id;
    state.apply_event(&vma_engine::Event::AudioLive { input: mic });
    let h = harness(state);
    assert_eq!(h.get_all_by_role_and_label(Role::Button, "QP").count(), 4);
    assert_eq!(
        h.get_all_by_role_and_label(Role::Button, "Cut").count(),
        4 + 1
    );
    // Its card and its mixer strip.
    assert_eq!(h.get_all_by_label("5 Micro").count(), 2);
}

#[test]
fn the_headphones_device_is_chosen_and_its_trouble_shown() {
    use vma_engine::SoloMode;

    let mut h = harness(with_sound());
    h.state_mut().audio_sinks = vec![CaptureChoice {
        id: DeviceId("alsa:alsa_output.usb-Headphones".into()),
        name: "Casque USB".into(),
    }];
    h.run();
    h.get_by_label("Sortie").click();
    h.run();
    h.get_by_label("Casque USB").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::SetMonitor {
            device: Some(DeviceId("alsa:alsa_output.usb-Headphones".into())),
            solo_mode: SoloMode::Afl,
        }]
    );
    h.state_mut().headphones = Some(HeadphonesActivity {
        error: Some("audio device x is not connected".into()),
        ..HeadphonesActivity::default()
    });
    h.run();
    h.get_by_label("Casque muet");
    h.state_mut().headphones = None;
    h.run();
    h.get_by_label("Casque coupé");
}

// ── SRT input (Phase 3 step 6): C9 / C13 (UI side) ──

/// « Ajouter → SRT… »: the settings are typed, the passphrase in a password field shown
/// nowhere; « Ajouter » sends them.
#[test]
fn an_srt_input_is_added_with_its_passphrase_hidden() {
    use vma_engine::{Secret, SrtMode, SrtSource};
    const PASSPHRASE: &str = "correct horse battery";
    let mut h = harness(state());
    h.get_by_label("Ajouter un input").click();
    h.run();
    h.get_by_label("SRT…").click();
    h.run();
    h.get_by_label("Écoute").click();
    h.run();
    h.get_by_label("Appelant").click();
    h.run();
    h.get_by_label("Hôte").click();
    h.run();
    h.get_by_label("Hôte").type_text("10.0.0.7");
    h.run();
    h.get_by_label("Phrase de passe").click();
    h.run();
    h.get_by_label("Phrase de passe").type_text(PASSPHRASE);
    h.run();
    assert!(h.query_by_label_contains(PASSPHRASE).is_none());
    assert!(h.state().sent().is_empty(), "nothing before « Ajouter »");
    h.get_by_label("Ajouter").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::AddInput {
            spec: InputSpec::Srt(SrtSource {
                mode: SrtMode::Caller,
                host: "10.0.0.7".into(),
                port: 9000,
                latency_ms: 120,
                passphrase: Secret::new(PASSPHRASE),
            }),
            name: None,
        }]
    );
    assert!(h.query_by_label("Nouvel input SRT").is_none(), "closed");
}

/// C10 (UI) and step 7: a source seen on the network is chosen and added by its NDI® name;
/// a name typed in is added as typed (trimmed); nothing is sent before « Ajouter ».
#[test]
fn an_ndi_input_is_added_from_the_sources_seen() {
    let mut h = harness(state());
    h.get_by_label("Ajouter un input").click();
    h.run();
    h.get_by_label("NDI®…").click();
    h.run();
    h.get_by_label("STUDIO (Titres)").click();
    h.run();
    assert!(h.state().sent().is_empty(), "nothing before « Ajouter »");
    h.get_by_label("Ajouter").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::AddInput {
            spec: InputSpec::Ndi {
                name: "STUDIO (Titres)".into()
            },
            name: None,
        }]
    );
    assert!(h.query_by_label("Nouvel input NDI®").is_none(), "closed");

    h.get_by_label("Ajouter un input").click();
    h.run();
    h.get_by_label("NDI®…").click();
    h.run();
    h.get_by_label("Nom").click();
    h.run();
    h.get_by_label("Nom").type_text(" RÉGIE (Program) ");
    h.run();
    h.get_by_label("Ajouter").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::AddInput {
            spec: InputSpec::Ndi {
                name: "RÉGIE (Program)".into()
            },
            name: None,
        }]
    );
}

/// C10 (UI): without the NDI® runtime, « NDI®… » is greyed and opens nothing; the rest of
/// the menu works. « À propos » carries the trademark notice.
#[test]
fn without_the_ndi_runtime_ndi_is_greyed() {
    let mut h = harness(state());
    h.state_mut().ndi_unavailable = Some("NDI® runtime not found".into());
    h.run();
    h.get_by_label("Ajouter un input").click();
    h.run();
    h.get_by_label("NDI®…").click();
    h.run();
    assert!(h.query_by_label("Nouvel input NDI®").is_none(), "disabled");
    assert!(h.state().sent().is_empty());
    if h.query_by_label("Mire").is_none() {
        h.get_by_label("Ajouter un input").click();
        h.run();
    }
    h.get_by_label("Mire").click();
    h.run();
    assert_eq!(h.state().sent().len(), 1, "the rest of the menu works");

    h.get_by_label("À propos").click();
    h.run();
    h.get_by_label("NDI® is a registered trademark of Vizrt NDI AB.");
    h.get_by_label_contains("MPL-2.0");
    // D12: CEF and Chromium, and where Chromium's notices are.
    h.get_by_label("Chromium Embedded Framework (CEF), dans vma-html (input HTML)");
    h.get_by_label_contains("CREDITS.html qui l'accompagne");
}

/// Step 8: « + NDI® » adds the NDI output with its default name (once); without the runtime
/// it is greyed and adds nothing.
#[test]
fn the_ndi_output_is_added_unless_the_runtime_is_missing() {
    let mut h = harness(state());
    open_outputs(&mut h);
    h.get_by_label("+ NDI®").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::AddOutput {
            spec: OutputSpec::Ndi(vma_engine::NdiSpec::default()),
            name: None,
        }]
    );

    let mut h = harness(state());
    h.state_mut().ndi_unavailable = Some("NDI® runtime not found".into());
    open_outputs(&mut h);
    h.get_by_label("+ NDI®").click();
    h.run();
    assert!(h.state().sent().is_empty(), "disabled without the runtime");
}

/// Phase 4 step 7 (D9, A11): « Ajouter → Page web (HTML)… » adds an HTML input at the
/// Program's size, or its own with a style sheet.
#[test]
fn an_html_input_is_added_with_its_address_size_and_css() {
    let mut h = harness(state());
    h.get_by_label("Ajouter un input").click();
    h.run();
    h.get_by_label("Page web (HTML)…").click();
    h.run();
    h.get_by_label("Adresse").click();
    h.run();
    h.get_by_label("Adresse")
        .type_text(" https://scores.example/board.html ");
    h.run();
    h.get_by_label("Ajouter").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::AddInput {
            spec: InputSpec::Html {
                url: vma_engine::HtmlUrl::new("https://scores.example/board.html"),
                size: None,
                css: None,
            },
            name: None,
        }]
    );
    assert!(h.query_by_label("Nouvel input HTML").is_none(), "closed");

    h.get_by_label("Ajouter un input").click();
    h.run();
    h.get_by_label("Page web (HTML)…").click();
    h.run();
    h.get_by_label("Adresse").click();
    h.run();
    h.get_by_label("Adresse")
        .type_text("file:///titres/bas.html");
    h.run();
    h.get_by_label("Taille propre (sinon celle du Program)")
        .click();
    h.run();
    for (field, value) in [("Largeur", "1280"), ("Hauteur", "200")] {
        h.get_by_label(field).click();
        h.run();
        // Clear the default (the Program's size), from its end.
        h.key_press(egui::Key::End);
        h.run();
        for _ in 0..5 {
            h.key_press(egui::Key::Backspace);
            h.run();
        }
        h.get_by_label(field).type_text(value);
        h.run();
    }
    h.get_by_label("CSS (appliquée à chaque page chargée, facultative)")
        .click();
    h.run();
    h.get_by_label("CSS (appliquée à chaque page chargée, facultative)")
        .type_text("body { zoom: 2 }");
    h.run();
    h.get_by_label("Ajouter").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::AddInput {
            spec: InputSpec::Html {
                url: vma_engine::HtmlUrl::new("file:///titres/bas.html"),
                size: Some(Resolution::new(1280, 200).unwrap()),
                css: Some("body { zoom: 2 }".into()),
            },
            name: None,
        }]
    );
}

/// D7 (UI): without `vma-html`, « Page web (HTML)… » is greyed and opens nothing.
#[test]
fn without_vma_html_html_inputs_are_greyed() {
    let mut h = harness(state());
    h.state_mut().html_unavailable = Some("vma-html introuvable".into());
    h.run();
    h.get_by_label("Ajouter un input").click();
    h.run();
    h.get_by_label("Page web (HTML)…").click();
    h.run();
    assert!(h.query_by_label("Nouvel input HTML").is_none(), "disabled");
    assert!(h.state().sent().is_empty());
}

/// D9 (UI, A11): the settings of an HTML input send each page command; the address is shown
/// without its credentials.
#[test]
fn the_settings_of_an_html_input_drive_its_page() {
    let mut state = state();
    state
        .apply(&Command::AddInput {
            spec: InputSpec::Html {
                url: vma_engine::HtmlUrl::new("https://op:pw@scores.example/board.html"),
                size: None,
                css: None,
            },
            name: None,
        })
        .unwrap();
    let page = state.inputs.last().unwrap().id;
    let mut h = harness(state);
    // The thumbnail: the name is also on its strip in the mixer (it has sound), drawn first.
    h.get_all_by_label("5 board").last().unwrap().click();
    h.step();
    h.get_all_by_label("5 board").last().unwrap().click();
    h.run();
    assert!(h.query_by_label("Réglages de l'input 5").is_some());
    let address = h.get_by_label("Adresse").value().unwrap_or_default();
    assert_eq!(address, "https://***@scores.example/board.html");
    for (button, command) in [
        ("Recharger", Command::HtmlReload { input: page }),
        ("Précédente", Command::HtmlBack { input: page }),
        ("Suivante", Command::HtmlForward { input: page }),
    ] {
        h.get_by_label(button).click();
        h.run();
        assert_eq!(h.state().sent(), [command]);
    }
    h.get_by_label("Données (JSON, événement « vma-data » de la page)")
        .click();
    h.run();
    h.get_by_label("Données (JSON, événement « vma-data » de la page)")
        .type_text(r#"{"home": 3}"#);
    h.run();
    h.get_by_label("Envoyer les données").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::HtmlData {
            input: page,
            json: r#"{"home": 3}"#.into()
        }]
    );
    h.get_by_label("Script (JavaScript exécuté dans la page)")
        .click();
    h.run();
    h.get_by_label("Script (JavaScript exécuté dans la page)")
        .type_text("go()");
    h.run();
    h.get_by_label("Exécuter le script").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::HtmlScript {
            input: page,
            script: "go()".into()
        }]
    );
}

/// Phase 4 step 8 (D9, A11): a transition button takes a stinger; « Réglages » sets the file
/// of a stinger (native dialog), its cut point, and empties it; its state is shown.
#[test]
fn stingers_are_set_in_the_settings_and_taken_by_the_buttons() {
    use vma_engine::{Effect, StingerSlot};
    let one = StingerSlot::new(1).unwrap();
    let mut state = state();
    state
        .apply(&Command::SetStinger {
            slot: one,
            file: Some("/clips/wipe.webm".into()),
            cut_point_ms: None,
        })
        .unwrap();
    state.apply_event(&vma_engine::Event::StingerReady {
        slot: one,
        file: "/clips/wipe.webm".into(),
        frames: 100,
        rate: FrameRate::FPS_50,
        resolution: Resolution::HD_1080,
        alpha: vma_core::AlphaMode::Straight,
    });
    let mut h = harness(state);
    let mut buttons: Vec<_> = h
        .get_all_by_role_and_label(Role::Button, "1 · Fade 0,5 s")
        .collect();
    buttons.remove(0).click_secondary();
    h.run();
    h.get_by_role_and_label(Role::RadioButton, "Stinger 1")
        .click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::SetTransition {
            button: 1,
            effect: Effect::Stinger(one),
            duration_ms: 500,
        }]
    );
    h.key_press(egui::Key::Escape);
    h.run();

    h.get_by_label("Réglages").click();
    h.run();
    h.get_by_label("wipe.webm");
    h.get_by_label("Prêt : 2,00 s, 1920x1080 50p, avec alpha");
    h.get_by_label("Fichier du stinger 2…").click();
    h.run();
    assert_eq!(
        h.state().files,
        [vma_ui::FileKind::Stinger(StingerSlot::new(2).unwrap())]
    );
    h.get_by_label("Bascule au milieu (1)").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::SetStinger {
            slot: one,
            file: Some("/clips/wipe.webm".into()),
            cut_point_ms: Some(1_000),
        }],
        "half the file, now set"
    );
    h.get_by_label("Vider le stinger 1").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::SetStinger {
            slot: one,
            file: None,
            cut_point_ms: None,
        }]
    );
}

/// Under the T-Bar, the overlay channels: a lit channel goes out on a click, an unlit one
/// brings back the input it showed last, an empty one does nothing.
#[test]
fn overlay_channels_under_the_t_bar() {
    use vma_core::OverlayChannel;
    let channel = |n| OverlayChannel::new(n).unwrap();
    let mut state = state();
    for (n, input) in [(1, 2), (2, 3)] {
        state
            .apply(&Command::OverlayIn {
                channel: channel(n),
                input: id(input),
            })
            .unwrap();
    }
    state
        .apply(&Command::OverlayOut {
            channel: channel(2),
        })
        .unwrap();
    let mut h = harness(state);
    let lit = |h: &Harness<'_, Rig>, n: u8| {
        h.get_by_label(&format!("Canal d'overlay {n}"))
            .accesskit_node()
            .toggled()
            == Some(egui::accesskit::Toggled::True)
    };
    assert!(lit(&h, 1));
    assert!(!lit(&h, 2));

    h.get_by_label("Canal d'overlay 1").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::OverlayOut {
            channel: channel(1)
        }]
    );
    h.get_by_label("Canal d'overlay 2").click();
    h.run();
    assert_eq!(
        h.state().sent(),
        [Command::OverlayIn {
            channel: channel(2),
            input: id(3)
        }]
    );
    h.get_by_label("Canal d'overlay 3").click();
    h.run();
    assert!(h.state().sent().is_empty(), "channel 3 is empty");
}

/// The sound button of a card mutes and unmutes its input, as its mixer strip does.
#[test]
fn the_sound_of_an_input_from_its_card() {
    let mut state = state();
    state
        .apply(&Command::AddInput {
            spec: InputSpec::AudioDevice {
                device: DeviceId("mic".into()),
            },
            name: Some("Micro".into()),
        })
        .unwrap();
    let mic = state.inputs.last().unwrap().id;
    let mut h = harness(state);
    // Only the input with sound has one.
    assert!(h.query_by_label("Son de l'input 1").is_none());
    for mute in [false, true] {
        h.state_mut()
            .state
            .apply(&Command::SetInputMute { input: mic, mute })
            .unwrap();
        h.run();
        h.get_by_label("Son de l'input 5").click();
        h.run();
        assert_eq!(
            h.state().sent(),
            [Command::SetInputMute {
                input: mic,
                mute: !mute
            }]
        );
    }
}

/// « Plein écran » in the bottom bar: opens « Sorties » while there is none, then starts and
/// stops the full screen; « … » beside opens « Sorties ».
#[test]
fn full_screen_from_the_bottom_bar() {
    let mut h = harness(state());
    assert!(h.query_by_label("+ Plein écran").is_none());
    h.get_by_label("Plein écran").click();
    h.run();
    assert!(h.state().sent().is_empty());
    assert!(
        h.query_by_label("+ Plein écran").is_some(),
        "« Sorties » open"
    );

    let (fs_state, fs) = with_fullscreen(state(), "DP-2", false);
    let mut h = harness(fs_state);
    h.get_by_label("Plein écran").click();
    h.run();
    assert_eq!(h.state().sent(), [Command::StartOutput { output: fs }]);

    let (fs_state, fs) = with_fullscreen(state(), "DP-2", true);
    let mut h = harness(fs_state);
    h.get_by_label("Plein écran").click();
    h.run();
    assert_eq!(h.state().sent(), [Command::StopOutput { output: fs }]);

    let mut h = harness(state());
    h.get_by_label("Réglages de l'enregistrement").click();
    h.run();
    assert!(
        h.query_by_label("+ Plein écran").is_some(),
        "« Sorties » open"
    );
}

/// A double click on the thumbnail opens the input's settings too (after putting it in
/// Preview, as the first click does).
#[test]
fn double_click_on_the_thumbnail_opens_the_settings() {
    let mut h = harness(state());
    h.get_by_label("Vignette 3").click();
    h.run();
    h.get_by_label("Vignette 3").click();
    h.run();
    assert!(h.query_by_label("Réglages de l'input 3").is_some());
    assert!(
        h.state()
            .sent()
            .iter()
            .all(|c| *c == Command::SetPreview { input: id(3) })
    );
}

/// « Réglages → Affichage » sets the size of the thumbnails.
#[test]
fn thumbnail_size() {
    let mut h = harness(state());
    let width = |h: &Harness<'_, Rig>| h.get_by_label("Vignette 1").rect().width();
    assert_eq!(width(&h), 160.0);
    h.get_by_label("Réglages").click();
    h.run();
    h.get_by_label("Grandes").click();
    h.run();
    assert_eq!(width(&h), 224.0);
    h.get_by_label("Petites").click();
    h.run();
    assert_eq!(width(&h), 128.0);
    assert!(h.state().sent().is_empty(), "a UI setting, not a command");
}

/// A picture of the main window with a show's worth of inputs, for reviewing the layout
/// without opening a window (`target/ui-screenshots/main.png`). Needs a GPU:
/// `cargo test -p vma-ui --test switcher -- --ignored screenshot`.
#[test]
#[ignore = "picture for review, needs a GPU"]
fn screenshot_of_the_main_window() {
    use vma_core::OverlayChannel;
    let mut state = state();
    for (spec, name) in [
        (
            InputSpec::File {
                path: "generique.mp4".into(),
            },
            "Générique",
        ),
        (
            InputSpec::AudioDevice {
                device: DeviceId("mic".into()),
            },
            "Micro plateau",
        ),
    ] {
        state
            .apply(&Command::AddInput {
                spec,
                name: Some(name.into()),
            })
            .unwrap();
    }
    let mic = state.inputs.last().unwrap().id;
    state.apply_event(&vma_engine::Event::AudioLive { input: mic });
    state
        .apply(&Command::OverlayIn {
            channel: OverlayChannel::new(1).unwrap(),
            input: id(3),
        })
        .unwrap();
    let (state, _) = with_fullscreen(state, "DP-2", false);
    let mut h = harness_on(state, true);
    h.run();
    let picture = h.render().unwrap();
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-screenshots");
    std::fs::create_dir_all(&dir).unwrap();
    picture.save(dir.join("main.png")).unwrap();
}

/// Phase 5 (ADR 0013): the project bar, top left; a show is not replaced without asking if
/// it has changes or outputs run; saving never asks.
#[test]
fn the_project_bar_asks_before_replacing_a_show() {
    let mut h = harness(state());
    for label in [
        "Nouveau",
        "Ouvrir",
        "Sauvegarder",
        "Sauvegarder sous",
        "Dernier",
        "Sans titre",
    ] {
        assert!(h.query_by_label(label).is_some(), "{label}");
    }
    h.get_by_label("Nouveau").click();
    h.run();
    assert_eq!(h.state().project_requests, [ProjectRequest::New]);

    let mut h = harness(state());
    h.state_mut().project_modified = true;
    h.state_mut().project_path = Some("/émissions/match.vma".into());
    h.run();
    assert!(
        h.query_by_label("match.vma •").is_some(),
        "marked as changed"
    );
    h.get_by_label("Nouveau").click();
    h.run();
    assert!(h.state().project_requests.is_empty(), "asked first");
    h.get_by_label("Annuler").click();
    h.run();
    assert!(h.query_by_label("Continuer").is_none());
    assert!(h.state().project_requests.is_empty());
    h.get_by_label("Nouveau").click();
    h.run();
    h.get_by_label("Continuer").click();
    h.run();
    assert_eq!(h.state().project_requests, [ProjectRequest::New]);
    h.get_by_label("Sauvegarder").click();
    h.run();
    h.get_by_label("Sauvegarder sous").click();
    h.run();
    assert_eq!(
        h.state().project_requests,
        [
            ProjectRequest::New,
            ProjectRequest::Save,
            ProjectRequest::SaveAs
        ]
    );

    let (running, _) = with_fullscreen(state(), "DP-2", true);
    let mut h = harness(running);
    h.state_mut().project_recent = vec!["/émissions/finale.vma".into()];
    h.run();
    h.get_by_label("Ouvrir").click();
    h.run();
    h.get_by_label("finale.vma").click();
    h.run();
    assert!(
        h.query_by_label_contains("seront arrêtées d'abord")
            .is_some(),
        "the outputs that run are named"
    );
    h.get_by_label("Continuer").click();
    h.run();
    assert_eq!(
        h.state().project_requests,
        [ProjectRequest::Open(Some("/émissions/finale.vma".into()))]
    );
    assert!(h.state().sent().is_empty(), "the caller stops the outputs");
}

/// What a project could not load is shown once it is opened; news of the bar in the top bar.
#[test]
fn what_a_project_could_not_load_is_shown() {
    let mut s = state();
    s.project_loads = 1;
    s.load_problems = vec![vma_engine::LoadProblem {
        what: "Caméra 2".into(),
        reason: "invalid input settings".into(),
    }];
    let mut h = harness(s);
    assert!(h.query_by_label("Caméra 2").is_some());
    assert!(h.query_by_label("invalid input settings").is_some());

    h.state_mut().project_notice = Some(ProjectNotice {
        id: 1,
        text: "Projet sauvegardé : /émissions/match.vma".into(),
        error: false,
    });
    h.run();
    assert!(
        h.query_by_label("Projet sauvegardé : /émissions/match.vma")
            .is_some()
    );
}
