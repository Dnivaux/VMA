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
use egui_kittest::kittest::Queryable;
use vma_core::{FrameRate, InputId, Resolution, Rgb8};
use vma_engine::{
    AutoPlay, Command, CommandProbe, CommandSender, DeviceId, Effect, Format, InputSpec, Level,
    MeterReading, MixDomain, Source, State, Timeline,
};
use vma_ui::{
    AudioView, CaptureChoice, FileKind, Images, IoView, OutputActivity, Screen, Switcher, View,
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
    };
    let mut harness = Harness::builder()
        .with_size(Vec2::new(1400.0, 820.0))
        // Short simulated frames, so that two clicks one frame apart make a double-click.
        .with_step_dt(0.05)
        .build_ui_state(
            |ui, rig: &mut Rig| {
                let no_thumbnail = |_| None::<Rect>;
                let timeline = rig.timeline;
                let timeline = move |_| timeline;
                let no_mode = |_| None::<String>;
                let activity = rig.activity;
                let activity = move |_| activity;
                let view = View {
                    state: &rig.state,
                    commands: &rig.commands,
                    images: Images {
                        program: TextureId::default(),
                        preview: TextureId::default(),
                        thumbnails: TextureId::default(),
                        thumbnail_uv: &no_thumbnail,
                    },
                    io: IoView {
                        unavailable: rig.unavailable.as_deref(),
                        devices: &rig.devices,
                        timeline: &timeline,
                        capture_mode: &no_mode,
                        output_activity: &activity,
                        record_folder: Some(std::path::Path::new("/home/op/Vidéos")),
                    },
                    screens: &rig.screens,
                    frame: 0,
                    fade_to_black: rig.fade_to_black,
                    rejection: None,
                    audio: AudioView {
                        levels: &rig.levels,
                    },
                };
                rig.switcher.show(ui, &view);
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
    h.get_by_label("+ Ajouter").click();
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
    h.get_by_label("+ Ajouter").click();
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
    h.get_by_label("+ Ajouter").click();
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
        h.get_by_label("+ Ajouter").click();
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
    h.get_by_label("+ Ajouter").click();
    h.run();
    h.get_by_label("Vidéo…").click();
    h.run();
    assert!(h.state().files.is_empty(), "video disabled");
    if h.query_by_label("Image…").is_none() {
        h.get_by_label("+ Ajouter").click();
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
