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
    AutoPlay, Command, CommandProbe, CommandSender, Effect, Format, InputSpec, MixDomain, Source,
    State,
};
use vma_ui::{Images, Switcher, View};

struct Rig {
    switcher: Switcher,
    state: State,
    commands: CommandSender,
    probe: CommandProbe,
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
    };
    let mut harness = Harness::builder()
        .with_size(Vec2::new(1400.0, 820.0))
        // Short simulated frames, so that two clicks one frame apart make a double-click.
        .with_step_dt(0.05)
        .build_ui_state(
            |ui, rig: &mut Rig| {
                let no_thumbnail = |_| None::<Rect>;
                let view = View {
                    state: &rig.state,
                    commands: &rig.commands,
                    images: Images {
                        program: TextureId::default(),
                        preview: TextureId::default(),
                        thumbnails: TextureId::default(),
                        thumbnail_uv: &no_thumbnail,
                    },
                    frame: 0,
                    rejection: None,
                };
                rig.switcher.show(ui, &view);
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
