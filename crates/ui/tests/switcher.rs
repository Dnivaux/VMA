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
    AutoPlay, Command, CommandProbe, CommandSender, DeviceId, Effect, Format, InputSpec, MixDomain,
    Source, State, Timeline,
};
use vma_ui::{CaptureChoice, FileKind, Images, IoView, OutputActivity, Switcher, View};

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
                    frame: 0,
                    fade_to_black: rig.fade_to_black,
                    rejection: None,
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
    let mut failed = state.outputs[1].clone();
    failed.status = OutputStatus::Failed {
        reason: "connection refused".into(),
    };
    let outputs = [state.outputs[0].clone(), failed];
    let counters = vma_engine::OutputCountersSnapshot {
        delivered: 4321,
        dropped: 7,
        queued: 2,
    };
    let activity = OutputActivity {
        running_for: std::time::Duration::from_secs(3723),
        bytes: Some(12_345_678),
        kbps: Some(40_000),
    };
    let h = Harness::builder()
        .with_size(Vec2::new(400.0, 600.0))
        .build_ui(move |ui| {
            let rows: Vec<_> = outputs
                .iter()
                .map(|o| vma_ui::OutputStatsRow {
                    output: o,
                    counters: o.status.is_on().then_some(counters),
                    activity: o.status.is_on().then_some(activity),
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
        "erreur",
        "connection refused",
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
