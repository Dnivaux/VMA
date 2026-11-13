//! The « Replay » panel (Phase 6 step 7, `phase-6.md` F9), in the zone of the audio mixer as
//! the reference has it: recording, marks (« −5 s », « −10 s », « Entrée » / « Sortie »),
//! « Rejouer », the player (image par image, lecture, arrière, vitesse), the angles and the
//! list of events. A mini-panel beside the Replay A input card keeps the gestures of a show
//! (record, −5 s / −10 s, « Rejouer », its speed). The mouse wheel over either: Maj = speed,
//! Ctrl = image par image, Alt = seconde par seconde.
//!
//! Also the windows « Réglages du replay » (session, cameras, « Rejouer ») and « Enregistrement
//! ISO » (the reference's MultiCorder: the inputs recorded each in its own file). Every action
//! is a `Command`; the drafts of the windows follow the state until they are edited.

use std::path::PathBuf;

use egui::{Color32, Event, MouseWheelUnit, Rect, RichText};
use vma_core::InputId;
use vma_engine::{
    Command, InputSpec, InputStatus, MAX_REPLAY_CAMERAS, MAX_REPLAY_SPEED, Mark, ReplayChannel,
    ReplayEvent, State, TRANSITION_BUTTONS,
};

use super::icons::{self, Icon};
use super::outputs::ON_AIR;
use super::{Switcher, View, WARNING};

/// Height the panel asks for (as the mixer).
pub(super) const REPLAY_HEIGHT: f32 = 236.0;
/// Width the panel asks for beside the inputs, at most 60 % of what there is.
pub(super) fn panel_width(available: f32) -> f32 {
    PANEL_WIDTH.min(available * 0.6)
}
const PANEL_WIDTH: f32 = 640.0;
/// Speeds offered as presets (BRIEF: 25, 50, 75 %).
const SPEEDS: [u8; 4] = [25, 50, 75, 100];
/// The one-click marks, seconds.
const MARKS: [u32; 2] = [5, 10];
/// Speed change of one wheel notch, percent.
const WHEEL_SPEED: i32 = 5;
/// Wheel travel of one notch when the backend counts in points.
const NOTCH_POINTS: f32 = 40.0;
/// Width of the mini-panel beside the Replay A card.
pub(super) const MINI_WIDTH: f32 = 112.0;
/// Fill of the open « Entrée ».
const MARKING: Color32 = Color32::from_rgb(40, 140, 200);

/// The folder a replay window asked for: the caller opens the dialog and gives the result back
/// with [`Switcher::set_replay_folder`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayFolder {
    /// The replay's session (a dedicated SSD).
    Session,
    /// Where the ISO files go.
    Iso,
}

/// A window's settings while edited: they follow the state until the operator changes them,
/// and again once the state has them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Draft<T> {
    /// The state's settings when the draft was last in step with them.
    base: T,
    edit: T,
}

impl<T: Clone + PartialEq> Draft<T> {
    fn follow<'a>(slot: &'a mut Option<Self>, current: &T) -> &'a mut Self {
        if let Some(d) = slot.as_mut() {
            if d.edit == *current {
                d.base = current.clone();
            } else if d.edit == d.base {
                // Untouched, and the state moved (another window, the API).
                *slot = None;
            }
        }
        slot.get_or_insert_with(|| Self {
            base: current.clone(),
            edit: current.clone(),
        })
    }
}

/// « Réglages du replay ».
#[derive(Debug, Clone, PartialEq, Eq)]
struct Session {
    folder: Option<PathBuf>,
    keep_minutes: u32,
    kbps: u32,
    cameras: Vec<InputId>,
    auto_speed: u8,
    auto_button: u8,
}

impl Session {
    fn of(state: &State) -> Self {
        let r = &state.replay;
        Self {
            folder: r.folder.clone(),
            keep_minutes: r.keep_minutes,
            kbps: r.kbps,
            cameras: r.cameras.clone(),
            auto_speed: r.auto_speed,
            auto_button: r.auto_button,
        }
    }
}

/// « Enregistrement ISO ».
#[derive(Debug, Clone, PartialEq, Eq)]
struct Iso {
    folder: Option<PathBuf>,
    inputs: Vec<InputId>,
    split_minutes: Option<u32>,
}

impl Iso {
    fn of(state: &State) -> Self {
        let iso = &state.replay.iso;
        Self {
            folder: iso.folder.clone(),
            inputs: iso.inputs.clone(),
            split_minutes: iso.split_minutes,
        }
    }
}

/// What a wheel notch moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wheel {
    Speed,
    Frame,
    Second,
}

/// UI-side state of the replay (never replay state).
#[derive(Debug, Default)]
pub(super) struct ReplayUi {
    /// « Replay » toggle of the bottom bar (the mixer's zone: one or the other).
    pub(super) open: bool,
    settings_open: bool,
    session: Option<Draft<Session>>,
    pub(super) iso_open: bool,
    iso: Option<Draft<Iso>>,
    folder_request: Option<ReplayFolder>,
    /// The comment being typed, and its event.
    comment: Option<(u32, String)>,
    /// Speed slider held: its value.
    speed_held: Option<u8>,
    /// Wheel travel not yet a notch, and what it moves.
    wheel: (f32, Option<Wheel>),
}

/// « 4,0 s ».
fn seconds(ns: u64) -> String {
    format!("{:.1} s", ns as f64 / 1e9).replace('.', ",")
}

/// The name of an input as the operator knows it: « 2 Caméra 1 ».
fn input_name(state: &State, id: InputId) -> String {
    match (state.number(id), state.input(id)) {
        (Some(n), Some(i)) => format!("{n} {}", i.name),
        _ => format!("Input {id}"),
    }
}

/// The inputs a recorder takes (cameras, files, streams), in display order.
fn recordable(state: &State) -> impl Iterator<Item = InputId> + '_ {
    state
        .inputs
        .iter()
        .filter(|i| i.spec.is_recordable())
        .map(|i| i.id)
}

/// The camera the player shows: chosen while playing, else the event's, else the first one.
fn current_angle(state: &State) -> Option<InputId> {
    let replay = &state.replay;
    let event = replay
        .player
        .event
        .and_then(|id| replay.events.iter().find(|e| e.id == id));
    replay
        .player
        .angle
        .or_else(|| event.and_then(|e| e.angle))
        .filter(|a| replay.cameras.contains(a))
        .or_else(|| replay.cameras.first().copied())
}

/// Pictures in one second of `camera` (its rate when known, else the master's).
fn frames_per_second(state: &State, camera: Option<InputId>) -> i32 {
    let rate = camera
        .and_then(|c| state.input(c))
        .and_then(|i| match i.status {
            InputStatus::Live { source: Some(f) } => f.rate,
            _ => None,
        })
        .unwrap_or(state.format.rate);
    rate.as_f64().round().max(1.0) as i32
}

/// Gives a combo box without label its accessible name.
fn combo_name(response: &egui::Response, name: &str) {
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ComboBox, true, name));
}

/// A labelled toggle with an icon, in red when on.
fn red_when_on(icon: Icon, text: &str, on: bool) -> egui::Button<'static> {
    let text = RichText::new(text).strong();
    if on {
        icons::button(icon, text.color(Color32::WHITE)).fill(ON_AIR)
    } else {
        icons::button(icon, text)
    }
}

impl Switcher {
    /// The folder dialog a replay window asked for, once (see [`ReplayFolder`]).
    pub fn take_replay_folder_request(&mut self) -> Option<ReplayFolder> {
        self.replay.folder_request.take()
    }

    /// Puts a folder chosen in the dialog into the window that asked for it.
    pub fn set_replay_folder(&mut self, which: ReplayFolder, folder: PathBuf) {
        match which {
            ReplayFolder::Session => {
                if let Some(d) = &mut self.replay.session {
                    d.edit.folder = Some(folder);
                }
            }
            ReplayFolder::Iso => {
                if let Some(d) = &mut self.replay.iso {
                    d.edit.folder = Some(folder);
                }
            }
        }
    }

    /// The « Replay » panel.
    pub(super) fn replay_panel(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        let area = ui.max_rect();
        let state = view.state;
        let replay = &state.replay;
        let channel = state.replay_input(ReplayChannel::A);

        ui.horizontal(|ui| {
            self.replay_record_button(ui, view);
            let summary = match (&replay.folder, replay.cameras.len()) {
                (None, _) => RichText::new("Aucun dossier de session").color(WARNING),
                (Some(_), 0) => RichText::new("Aucune caméra choisie").color(WARNING),
                (Some(_), n) => RichText::new(format!(
                    "{n} caméra{} · {} min gardées",
                    if n > 1 { "s" } else { "" },
                    replay.keep_minutes
                ))
                .weak(),
            };
            ui.label(summary);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let name = "Réglages du replay";
                let cog = ui.add(icons::only(Icon::Settings)).on_hover_text(name);
                if icons::named(cog, name, None).clicked() {
                    self.replay.settings_open = true;
                }
            });
        });
        for failure in &replay.failures {
            ui.label(
                RichText::new(format!(
                    "Enregistrement arrêté : {} — {}",
                    input_name(state, failure.input),
                    failure.reason
                ))
                .color(WARNING),
            );
        }
        if channel.is_none() {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Pas d'input Replay A").color(WARNING));
                if ui
                    .add(icons::button(Icon::Plus, "Ajouter Replay A"))
                    .clicked()
                {
                    self.add_replay_input(ui, view);
                }
            });
        }

        ui.horizontal(|ui| {
            self.mark_buttons(ui, view);
            ui.separator();
            self.rejouer_button(ui, view, None);
        });
        ui.add_enabled_ui(channel.is_some(), |ui| {
            ui.horizontal(|ui| self.transport(ui, view));
            ui.horizontal(|ui| self.angles(ui, view));
        });
        ui.separator();
        self.event_list(ui, view);
        self.replay_wheel(ui, view, area);
    }

    /// « Enregistrer le replay » / « REC replay »: not set yet, it opens the settings.
    fn replay_record_button(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        let replay = &view.state.replay;
        let on = replay.recording;
        let label = if on {
            "REC replay"
        } else {
            "Enregistrer le replay"
        };
        let response = ui
            .add(red_when_on(Icon::CircleDot, label, on))
            .on_hover_text(if on {
                "Arrêter l'enregistrement du replay"
            } else {
                "Enregistrer les caméras du replay"
            });
        if response.clicked() {
            if on {
                self.send(ui, view, Command::StopReplayRecording);
            } else if replay.folder.is_none() || replay.cameras.is_empty() {
                self.replay.settings_open = true;
            } else {
                self.send(ui, view, Command::StartReplayRecording);
            }
        }
    }

    /// « −5 s », « −10 s », « Entrée », « Sortie », « Annuler ».
    fn mark_buttons(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        let replay = &view.state.replay;
        ui.add_enabled_ui(replay.recording, |ui| {
            for seconds in MARKS {
                let mark = ui
                    .add(egui::Button::new(
                        RichText::new(format!("−{seconds} s")).strong(),
                    ))
                    .on_hover_text(format!("Un événement des {seconds} dernières secondes"))
                    .on_disabled_hover_text("Le replay n'enregistre pas");
                if mark.clicked() {
                    self.mark(ui, view, Mark::Last { seconds });
                }
            }
            let marking = replay.marking.is_some();
            let mut entry = egui::Button::new("Entrée");
            if marking {
                entry = entry.fill(MARKING);
            }
            if ui
                .add(entry)
                .on_hover_text("Ouvrir un événement maintenant")
                .clicked()
            {
                self.mark(ui, view, Mark::In);
            }
            if ui
                .add_enabled(marking, egui::Button::new("Sortie"))
                .on_hover_text("Fermer l'événement maintenant")
                .clicked()
            {
                self.mark(ui, view, Mark::Out);
            }
            if marking && ui.button("Annuler").clicked() {
                self.mark(ui, view, Mark::Cancel);
            }
        });
    }

    fn mark(&mut self, ui: &egui::Ui, view: &View<'_>, mark: Mark) {
        // The instant is stamped by `vma-control` when it receives the mark.
        self.send(ui, view, Command::ReplayMark { mark, at_ns: None });
    }

    /// « Rejouer »: the event (`None`: the last one) at the replay speed, there and back.
    fn rejouer_button(&mut self, ui: &mut egui::Ui, view: &View<'_>, id: Option<u32>) {
        let state = view.state;
        let replay = &state.replay;
        let ready = state.replay_input(ReplayChannel::A).is_some() && !replay.events.is_empty();
        let running = replay.auto.is_some();
        let button = red_when_on(Icon::Undo2, "Rejouer", running);
        let response = ui
            .add_enabled(ready, button)
            .on_hover_text(format!(
                "Le dernier événement dans Replay A à {} %, transition {} à l'aller et au retour",
                replay.auto_speed, replay.auto_button
            ))
            .on_disabled_hover_text("Aucun événement, ou pas d'input Replay A");
        if response.clicked() {
            self.send(ui, view, Command::ReplayAuto { id });
        }
    }

    /// Image par image, lecture / pause, arrière, vitesse.
    fn transport(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        let player = &view.state.replay.player;
        let back = ui
            .add(icons::only(Icon::StepBack))
            .on_hover_text("Image précédente (Ctrl + molette)");
        if icons::named(back, "Image précédente", None).clicked() {
            self.send(ui, view, Command::ReplayStep { frames: -1 });
        }
        let (icon, name, command) = if player.playing {
            (Icon::Pause, "Pause", Command::ReplayPause)
        } else {
            (Icon::Play, "Lecture", Command::ReplayPlay)
        };
        let play = ui.add(icons::only(icon)).on_hover_text(name);
        if icons::named(play, name, None).clicked() {
            self.send(ui, view, command);
        }
        let next = ui
            .add(icons::only(Icon::StepForward))
            .on_hover_text("Image suivante (Ctrl + molette)");
        if icons::named(next, "Image suivante", None).clicked() {
            self.send(ui, view, Command::ReplayStep { frames: 1 });
        }
        let reverse = ui
            .add(icons::only(Icon::Rewind).selected(player.reverse))
            .on_hover_text("Lire en arrière");
        if icons::named(reverse, "Arrière", Some(player.reverse)).clicked() {
            let on = !player.reverse;
            self.send(ui, view, Command::ReplayReverse { on });
        }
        ui.separator();
        for speed in SPEEDS {
            let on = player.speed == speed && self.replay.speed_held.is_none();
            if ui
                .add(egui::Button::selectable(on, format!("{speed} %")))
                .clicked()
                && !on
            {
                self.send(ui, view, Command::ReplaySpeed { percent: speed });
            }
        }
        let mut speed = self.replay.speed_held.unwrap_or(player.speed);
        let slider = ui
            .add(egui::Slider::new(&mut speed, 0..=MAX_REPLAY_SPEED).suffix(" %"))
            .on_hover_text("Vitesse du lecteur (Maj + molette)");
        if slider.dragged() || slider.changed() {
            if speed != self.replay.speed_held.unwrap_or(player.speed) {
                self.send(ui, view, Command::ReplaySpeed { percent: speed });
            }
            self.replay.speed_held = Some(speed);
        }
        if !slider.dragged() {
            self.replay.speed_held = None;
        }
    }

    /// One button per replay camera: the angle shown, changed at the same moment.
    fn angles(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        let state = view.state;
        ui.label("Angle :");
        if state.replay.cameras.is_empty() {
            ui.label(RichText::new("aucune caméra").weak());
        }
        let current = current_angle(state);
        for camera in state.replay.cameras.iter().copied() {
            let on = current == Some(camera);
            let name = input_name(state, camera);
            if ui
                .add(egui::Button::selectable(on, name))
                .on_hover_text("Cette caméra, au même moment")
                .clicked()
                && !on
            {
                self.send(ui, view, Command::ReplayAngle { input: camera });
            }
        }
    }

    /// The events, newest first: angle, speed, comment, play, « Rejouer », delete.
    fn event_list(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        let state = view.state;
        let replay = &state.replay;
        if replay.events.is_empty() {
            ui.label(RichText::new("Aucun événement : « −5 s » en crée un").weak());
            return;
        }
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("replay_events")
                    .num_columns(7)
                    .spacing([8.0, 4.0])
                    .striped(true)
                    .show(ui, |ui| {
                        for title in ["N°", "Durée", "Angle", "Vitesse", "Commentaire"] {
                            ui.label(RichText::new(title).small().weak());
                        }
                        ui.end_row();
                        for event in replay.events.iter().rev() {
                            self.event_row(ui, view, event);
                            ui.end_row();
                        }
                    });
            });
    }

    fn event_row(&mut self, ui: &mut egui::Ui, view: &View<'_>, event: &ReplayEvent) {
        let state = view.state;
        let id = event.id;
        let playing = state.replay.player.event == Some(id);
        let number = RichText::new(format!("#{id}")).strong();
        ui.label(if playing {
            number.color(MARKING)
        } else {
            number
        });
        ui.label(seconds(event.out_ns.saturating_sub(event.in_ns)));

        let edit = |angle, speed, comment: &str| Command::EditReplayEvent {
            id,
            angle,
            speed,
            comment: comment.to_owned(),
        };
        let mut angle = event.angle;
        let shown = |a: Option<InputId>| match a {
            Some(a) => input_name(state, a),
            None => "Par défaut".to_owned(),
        };
        let combo = egui::ComboBox::from_id_salt(("replay_angle", id))
            .selected_text(shown(angle))
            .width(110.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut angle, None, shown(None));
                for camera in state.replay.cameras.iter().copied() {
                    ui.selectable_value(&mut angle, Some(camera), shown(Some(camera)));
                }
            });
        combo_name(&combo.response, &format!("Angle de l'événement {id}"));
        if angle != event.angle {
            self.send(ui, view, edit(angle, event.speed, &event.comment));
        }
        let mut speed = event.speed;
        let rate = |s: Option<u8>| match s {
            Some(s) => format!("{s} %"),
            None => "Lecteur".to_owned(),
        };
        let combo = egui::ComboBox::from_id_salt(("replay_speed", id))
            .selected_text(rate(speed))
            .width(70.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut speed, None, rate(None));
                for s in SPEEDS {
                    ui.selectable_value(&mut speed, Some(s), rate(Some(s)));
                }
            });
        combo_name(&combo.response, &format!("Vitesse de l'événement {id}"));
        if speed != event.speed {
            self.send(ui, view, edit(event.angle, speed, &event.comment));
        }

        // Typed here, sent when the field is left.
        let typing = self.replay.comment.as_ref().filter(|(e, _)| *e == id);
        let mut text = typing.map_or_else(|| event.comment.clone(), |(_, t)| t.clone());
        let field = ui.add_sized(
            [140.0, 20.0],
            egui::TextEdit::singleline(&mut text)
                .id_salt(("replay_comment", id))
                .hint_text("Commentaire"),
        );
        if field.changed() || field.has_focus() {
            self.replay.comment = Some((id, text.clone()));
        }
        if field.lost_focus() {
            self.replay.comment = None;
            if text != event.comment {
                self.send(ui, view, edit(event.angle, event.speed, &text));
            }
        }

        ui.horizontal(|ui| {
            let name = format!("Lire l'événement {id}");
            let play = ui
                .add(icons::only(Icon::Play).small())
                .on_hover_text("Dans Replay A, à la vitesse du lecteur");
            if icons::named(play, &name, None).clicked() {
                self.send(ui, view, Command::ReplayPlayEvent { id: Some(id) });
            }
            let name = format!("Rejouer l'événement {id}");
            let again = ui
                .add(icons::only(Icon::Undo2).small())
                .on_hover_text("À l'antenne et retour, à la vitesse du « Rejouer »");
            if icons::named(again, &name, None).clicked() {
                self.send(ui, view, Command::ReplayAuto { id: Some(id) });
            }
            let name = format!("Supprimer l'événement {id}");
            let delete = ui
                .add(icons::only(Icon::Trash).small())
                .on_hover_text("Supprimer l'événement");
            if icons::named(delete, &name, None).clicked() {
                self.send(ui, view, Command::DeleteReplayEvent { id });
            }
        });
    }

    /// The mini-panel beside the Replay A card: record, −5 s / −10 s, « Rejouer », its speed.
    pub(super) fn replay_mini_panel(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        let state = view.state;
        let replay = &state.replay;
        let frame = egui::Frame::new()
            .inner_margin(4.0)
            .corner_radius(4.0)
            .stroke(ui.visuals().widgets.noninteractive.bg_stroke);
        let shown = frame.show(ui, |ui| {
            ui.set_width(MINI_WIDTH);
            ui.vertical_centered_justified(|ui| {
                self.replay_record_button_small(ui, view);
                ui.horizontal(|ui| self.mark_buttons_small(ui, view));
                self.rejouer_button(ui, view, None);
                let speed = replay.auto_speed;
                ui.menu_button(format!("Vitesse {speed} %"), |ui| {
                    for s in SPEEDS {
                        if ui.selectable_label(s == speed, format!("{s} %")).clicked() {
                            let button = replay.auto_button;
                            self.send(ui, view, Command::SetReplayAuto { speed: s, button });
                            ui.close();
                        }
                    }
                })
                .response
                .on_hover_text("Vitesse du « Rejouer »");
            });
        });
        self.replay_wheel(ui, view, shown.response.rect);
    }

    fn replay_record_button_small(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        let on = view.state.replay.recording;
        let label = if on { "REC" } else { "Enregistrer" };
        let response = ui
            .add(red_when_on(Icon::CircleDot, label, on))
            .on_hover_text(if on {
                "Arrêter l'enregistrement du replay"
            } else {
                "Enregistrer les caméras du replay"
            });
        if icons::named(response, "Enregistrer le replay", Some(on)).clicked() {
            let replay = &view.state.replay;
            if on {
                self.send(ui, view, Command::StopReplayRecording);
            } else if replay.folder.is_none() || replay.cameras.is_empty() {
                self.replay.settings_open = true;
            } else {
                self.send(ui, view, Command::StartReplayRecording);
            }
        }
    }

    fn mark_buttons_small(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        ui.add_enabled_ui(view.state.replay.recording, |ui| {
            for seconds in MARKS {
                if ui
                    .add(egui::Button::new(format!("−{seconds} s")).small())
                    .on_disabled_hover_text("Le replay n'enregistre pas")
                    .clicked()
                {
                    self.mark(ui, view, Mark::Last { seconds });
                }
            }
        });
    }

    /// The wheel over `area`: Maj = speed, Ctrl = image par image, Alt = seconde par seconde.
    fn replay_wheel(&mut self, ui: &egui::Ui, view: &View<'_>, area: Rect) {
        if !ui.rect_contains_pointer(area) || view.state.replay_input(ReplayChannel::A).is_none() {
            return;
        }
        let notches: Vec<(Wheel, f32)> = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    Event::MouseWheel {
                        unit,
                        delta,
                        modifiers,
                        ..
                    } => {
                        let what = if modifiers.shift {
                            Wheel::Speed
                        } else if modifiers.command || modifiers.ctrl {
                            Wheel::Frame
                        } else if modifiers.alt {
                            Wheel::Second
                        } else {
                            return None;
                        };
                        // Some systems turn a vertical wheel with Maj into a horizontal one.
                        let amount = if delta.y != 0.0 { delta.y } else { delta.x };
                        let notch = match unit {
                            MouseWheelUnit::Point => NOTCH_POINTS,
                            MouseWheelUnit::Line | MouseWheelUnit::Page => 1.0,
                        };
                        Some((what, amount / notch))
                    }
                    _ => None,
                })
                .collect()
        });
        for (what, amount) in notches {
            let (travel, last) = &mut self.replay.wheel;
            if *last != Some(what) {
                *travel = 0.0;
                *last = Some(what);
            }
            *travel += amount;
            let steps = travel.trunc();
            *travel -= steps;
            let steps = steps as i32;
            if steps == 0 {
                continue;
            }
            let state = view.state;
            let command = match what {
                Wheel::Speed => {
                    let speed = i32::from(state.replay.player.speed) + steps * WHEEL_SPEED;
                    let max = i32::from(MAX_REPLAY_SPEED);
                    let percent = u8::try_from(speed.clamp(0, max)).unwrap_or(0);
                    Command::ReplaySpeed { percent }
                }
                Wheel::Frame => Command::ReplayStep { frames: steps },
                Wheel::Second => Command::ReplayStep {
                    frames: steps * frames_per_second(state, current_angle(state)),
                },
            };
            self.send(ui, view, command);
        }
    }

    /// Adds the input « Replay A ».
    pub(super) fn add_replay_input(&mut self, ui: &egui::Ui, view: &View<'_>) {
        let spec = InputSpec::Replay {
            channel: ReplayChannel::A,
        };
        self.send(ui, view, Command::AddInput { spec, name: None });
    }

    /// « Réglages du replay ».
    pub(super) fn replay_settings_window(&mut self, ui: &egui::Ui, view: &View<'_>) {
        if !self.replay.settings_open {
            self.replay.session = None;
            return;
        }
        let state = view.state;
        let recording = state.replay.recording;
        let current = Session::of(state);
        let mut open = true;
        let mut send = Vec::new();
        egui::Window::new("Réglages du replay")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ui.ctx(), |ui| {
                let draft = Draft::follow(&mut self.replay.session, &current);
                let edit = &mut draft.edit;
                ui.label(RichText::new("Session").strong());
                ui.add_enabled_ui(!recording, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Dossier :");
                        match &edit.folder {
                            Some(f) => ui.label(RichText::new(f.display().to_string()).monospace()),
                            None => ui.label(RichText::new("aucun").color(WARNING)),
                        };
                        if ui
                            .add(icons::button(Icon::FolderOpen, "Choisir…"))
                            .on_hover_text("Un SSD dédié de préférence")
                            .clicked()
                        {
                            self.replay.folder_request = Some(ReplayFolder::Session);
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.label("Gardé :");
                        ui.add(
                            egui::DragValue::new(&mut edit.keep_minutes)
                                .range(1..=24 * 60)
                                .suffix(" min"),
                        );
                        ui.label("Débit par caméra :");
                        let mut mbps = edit.kbps / 1000;
                        if ui
                            .add(
                                egui::DragValue::new(&mut mbps)
                                    .range(1..=400)
                                    .suffix(" Mb/s"),
                            )
                            .changed()
                        {
                            edit.kbps = mbps * 1000;
                        }
                    });
                });
                if recording {
                    ui.label(
                        RichText::new("Arrêter l'enregistrement du replay pour les changer")
                            .small()
                            .weak(),
                    );
                }

                ui.separator();
                ui.label(RichText::new(format!("Caméras ({MAX_REPLAY_CAMERAS} au plus)")).strong());
                let mut any = false;
                for camera in recordable(state) {
                    any = true;
                    let mut on = edit.cameras.contains(&camera);
                    let enabled = on || edit.cameras.len() < MAX_REPLAY_CAMERAS;
                    let check = egui::Checkbox::new(&mut on, input_name(state, camera));
                    if ui.add_enabled(enabled, check).changed() {
                        if on {
                            edit.cameras.push(camera);
                        } else {
                            edit.cameras.retain(|c| *c != camera);
                        }
                    }
                }
                if !any {
                    ui.label(
                        RichText::new("Aucun input enregistrable (caméra, vidéo, flux)").weak(),
                    );
                }

                ui.separator();
                ui.label(RichText::new("« Rejouer »").strong());
                ui.horizontal(|ui| {
                    ui.label("Vitesse :");
                    for s in SPEEDS {
                        ui.radio_value(&mut edit.auto_speed, s, format!("{s} %"));
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Transition :");
                    for b in 1..=TRANSITION_BUTTONS as u8 {
                        ui.radio_value(&mut edit.auto_button, b, format!("bouton {b}"));
                    }
                });

                let changed = draft.edit != current;
                ui.separator();
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(changed, egui::Button::new("Appliquer"))
                        .clicked()
                    {
                        let edit = &draft.edit;
                        if (&edit.folder, edit.keep_minutes, edit.kbps)
                            != (&current.folder, current.keep_minutes, current.kbps)
                        {
                            send.push(Command::SetReplaySession {
                                folder: edit.folder.clone(),
                                keep_minutes: edit.keep_minutes,
                                kbps: edit.kbps,
                            });
                        }
                        if edit.cameras != current.cameras {
                            send.push(Command::SetReplayCameras {
                                inputs: edit.cameras.clone(),
                            });
                        }
                        if (edit.auto_speed, edit.auto_button)
                            != (current.auto_speed, current.auto_button)
                        {
                            send.push(Command::SetReplayAuto {
                                speed: edit.auto_speed,
                                button: edit.auto_button,
                            });
                        }
                    }
                    if ui
                        .add_enabled(changed, egui::Button::new("Annuler"))
                        .clicked()
                    {
                        draft.edit = current.clone();
                    }
                });
            });
        for command in send {
            self.send(ui, view, command);
        }
        if !open {
            self.replay.settings_open = false;
        }
    }

    /// « ISO » in the bottom bar: a click starts or stops, the cog opens the window. Not set
    /// (no input chosen), the click opens it.
    pub(super) fn iso_button(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        let iso = &view.state.replay.iso;
        let on = iso.recording;
        let label = if on {
            format!("ISO ({})", iso.inputs.len())
        } else {
            "ISO".to_owned()
        };
        let response = ui
            .add(red_when_on(Icon::HardDrive, &label, on))
            .on_hover_text(if on {
                "Arrêter les enregistrements ISO"
            } else {
                "Enregistrer chaque input choisi dans son propre fichier"
            });
        if icons::named(response, "ISO", Some(on)).clicked() {
            if on {
                self.send(ui, view, Command::StopIso);
            } else if iso.inputs.is_empty() {
                self.replay.iso_open = true;
            } else {
                self.start_iso(ui, view, &Iso::of(view.state));
            }
        }
        let name = "Réglages ISO";
        let cog = ui.add(icons::only(Icon::Settings)).on_hover_text(name);
        if icons::named(cog, name, None).clicked() {
            self.replay.iso_open = true;
        }
    }

    /// Starts the ISO with `settings`, sent first if they are not the state's (without a
    /// folder: the recordings').
    fn start_iso(&mut self, ui: &egui::Ui, view: &View<'_>, settings: &Iso) {
        let mut settings = settings.clone();
        if settings.folder.is_none() {
            settings.folder = view.io.record_folder.map(std::path::Path::to_path_buf);
        }
        if settings != Iso::of(view.state) {
            self.send(
                ui,
                view,
                Command::SetIso {
                    folder: settings.folder,
                    inputs: settings.inputs,
                    split_minutes: settings.split_minutes,
                },
            );
        }
        self.send(ui, view, Command::StartIso);
    }

    /// « Enregistrement ISO » (the reference's MultiCorder).
    pub(super) fn iso_window(&mut self, ui: &egui::Ui, view: &View<'_>) {
        if !self.replay.iso_open {
            self.replay.iso = None;
            return;
        }
        let state = view.state;
        let recording = state.replay.iso.recording;
        let current = Iso::of(state);
        let mut open = true;
        let mut start = None;
        let mut send = Vec::new();
        egui::Window::new("Enregistrement ISO")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ui.ctx(), |ui| {
                let draft = Draft::follow(&mut self.replay.iso, &current);
                let edit = &mut draft.edit;
                ui.label("Chaque input coché dans son propre fichier MKV, avec son son.");
                ui.add_enabled_ui(!recording, |ui| {
                    let mut any = false;
                    for input in recordable(state) {
                        any = true;
                        let mut on = edit.inputs.contains(&input);
                        if ui.checkbox(&mut on, input_name(state, input)).changed() {
                            if on {
                                edit.inputs.push(input);
                            } else {
                                edit.inputs.retain(|i| *i != input);
                            }
                        }
                    }
                    if !any {
                        ui.label(
                            RichText::new("Aucun input enregistrable (caméra, vidéo, flux)").weak(),
                        );
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label("Dossier :");
                        let folder = edit.folder.as_deref().or(view.io.record_folder);
                        match folder {
                            Some(f) => ui.label(RichText::new(f.display().to_string()).monospace()),
                            None => ui.label(RichText::new("aucun").color(WARNING)),
                        };
                        if ui
                            .add(icons::button(Icon::FolderOpen, "Choisir…"))
                            .clicked()
                        {
                            self.replay.folder_request = Some(ReplayFolder::Iso);
                        }
                    });
                    ui.horizontal(|ui| {
                        let mut split = edit.split_minutes.is_some();
                        let mut minutes = edit.split_minutes.unwrap_or(20);
                        ui.checkbox(&mut split, "Nouveau fichier toutes les");
                        ui.add_enabled(
                            split,
                            egui::DragValue::new(&mut minutes)
                                .range(1..=24 * 60)
                                .suffix(" min"),
                        );
                        edit.split_minutes = split.then_some(minutes);
                    });
                });
                for failure in &state.replay.failures {
                    ui.label(
                        RichText::new(format!(
                            "Enregistrement arrêté : {} — {}",
                            input_name(state, failure.input),
                            failure.reason
                        ))
                        .color(WARNING),
                    );
                }

                let changed = draft.edit != current;
                ui.separator();
                ui.horizontal(|ui| {
                    if recording {
                        if ui
                            .add(red_when_on(Icon::CircleStop, "Arrêter l'ISO", true))
                            .clicked()
                        {
                            send.push(Command::StopIso);
                        }
                        return;
                    }
                    let ready = !draft.edit.inputs.is_empty();
                    if ui
                        .add_enabled(ready, icons::button(Icon::CircleDot, "Démarrer l'ISO"))
                        .on_disabled_hover_text("Cocher au moins un input")
                        .clicked()
                    {
                        start = Some(draft.edit.clone());
                    }
                    if ui
                        .add_enabled(changed, egui::Button::new("Appliquer"))
                        .clicked()
                    {
                        let edit = draft.edit.clone();
                        send.push(Command::SetIso {
                            folder: edit.folder,
                            inputs: edit.inputs,
                            split_minutes: edit.split_minutes,
                        });
                    }
                    if ui
                        .add_enabled(changed, egui::Button::new("Annuler"))
                        .clicked()
                    {
                        draft.edit = current.clone();
                    }
                });
            });
        for command in send {
            self.send(ui, view, command);
        }
        if let Some(settings) = start {
            self.start_iso(ui, view, &settings);
        }
        if !open {
            self.replay.iso_open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_in_french() {
        assert_eq!(seconds(4_000_000_000), "4,0 s");
        assert_eq!(seconds(10_550_000_000), "10,6 s");
    }

    #[test]
    fn a_draft_follows_the_state_until_edited() {
        let mut slot = None;
        let d = Draft::follow(&mut slot, &1);
        assert_eq!(d.edit, 1);
        // The state moves, the draft untouched: it follows.
        assert_eq!(Draft::follow(&mut slot, &2).edit, 2);
        // Edited: it stays, whatever the state does.
        Draft::follow(&mut slot, &2).edit = 5;
        assert_eq!(Draft::follow(&mut slot, &3).edit, 5);
        // The state takes it: in step again, then follows.
        assert_eq!(Draft::follow(&mut slot, &5).edit, 5);
        assert_eq!(Draft::follow(&mut slot, &7).edit, 7);
    }
}
