//! The « Mélangeur » panel (Phase 3 step 4, `ui.md`): one strip per input with sound — meter,
//! fader, mute, solo, sends to the buses, audio-follow-video, delay — then the strips of the
//! Master, A and B, and the headphones. Every action is a `Command`; the meters are read
//! from the engine at the UI's pace (never part of the state).

use egui::{Color32, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};
use vma_core::{AudioBus, InputId};
use vma_engine::{
    Command, Db, InputAudio, InputState, Level, MAX_AUDIO_DELAY_MS, MAX_GAIN_DB, MeterReading,
    SoloMode, to_db,
};

use super::{Switcher, View};

/// Height of the panel.
pub(super) const MIXER_HEIGHT: f32 = 236.0;
/// Width of a strip.
const STRIP_WIDTH: f32 = 92.0;
/// Width a strip takes in the panel, spacing and separator included.
const STRIP_SPAN: f32 = 136.0;
/// Height of a fader and of its meter.
const FADER_HEIGHT: f32 = 120.0;
/// Bottom of the fader and of the meters: below, −∞.
const FLOOR_DB: f32 = -60.0;
/// Width of the buttons beside a fader.
const BUTTON_WIDTH: f32 = 36.0;
/// Zones of a meter: green below −18 dBFS, yellow up to −6, red above.
const METER_ZONES: [(f32, Color32); 3] = [
    (-18.0, Color32::from_rgb(60, 190, 90)),
    (-6.0, Color32::from_rgb(230, 200, 50)),
    (0.0, Color32::from_rgb(230, 50, 50)),
];
/// Fill of an active mute.
const MUTED: Color32 = Color32::from_rgb(200, 40, 40);
/// Fill of an active solo.
const SOLOED: Color32 = Color32::from_rgb(220, 180, 40);

/// Width the mixer asks for beside the inputs: every strip, at most 60 % of `available`.
pub(super) fn panel_width(state: &vma_engine::State, available: f32) -> f32 {
    let inputs = state.inputs.iter().filter(|i| i.audio.is_some()).count();
    // Input strips, the three buses and the headphones, with their spacing and separators.
    let strips = (inputs.max(1) + 4) as f32;
    (strips * STRIP_SPAN + 24.0).min(available * 0.6)
}

/// What a fader moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fader {
    Input(InputId),
    Bus(AudioBus),
}

/// UI-side state of the mixer (never mixer state).
#[derive(Debug)]
pub(super) struct MixerUi {
    /// « Mélangeur » toggle of the toolbar.
    pub(super) open: bool,
    /// Fader held by the pointer, and its level.
    held: Option<(Fader, Db)>,
}

impl Default for MixerUi {
    fn default() -> Self {
        Self {
            open: true,
            held: None,
        }
    }
}

/// Short name of a bus, on the send toggles and the bus strips.
pub(crate) fn bus_label(bus: AudioBus) -> &'static str {
    match bus {
        AudioBus::Master => "M",
        AudioBus::A => "A",
        AudioBus::B => "B",
    }
}

/// Full name of a bus.
pub(crate) fn bus_name(bus: AudioBus) -> &'static str {
    match bus {
        AudioBus::Master => "Master",
        AudioBus::A => "Bus A",
        AudioBus::B => "Bus B",
    }
}

/// A level as shown: « −12,5 dB », « −∞ dB ».
fn db_text(db: f32) -> String {
    if db == f32::NEG_INFINITY || db < -200.0 {
        "−∞ dB".to_owned()
    } else {
        format!("{db:+.1} dB").replace('.', ",").replace('-', "−")
    }
}

/// Fraction of the scale (0 at the floor, 1 at the top) of a level.
fn scale(db: f32, top: f32) -> f32 {
    ((db - FLOOR_DB) / (top - FLOOR_DB)).clamp(0.0, 1.0)
}

/// Level of a fader at `fraction` of its height (from the bottom); −∞ at the very bottom.
fn fader_level(fraction: f32) -> Db {
    if fraction <= 0.0 {
        return Db::SILENT;
    }
    let db = FLOOR_DB + fraction.min(1.0) * (MAX_GAIN_DB - FLOOR_DB);
    Db::new((db * 10.0).round() / 10.0).unwrap_or(Db::ZERO)
}

/// What the meter of `name` reads out, for screen readers and tests.
pub(crate) fn meter_label(name: &str, reading: Option<&MeterReading>) -> String {
    match reading {
        None => format!("Vumètre {name} : pas de son"),
        Some(r) => {
            let peak = r.peak.iter().fold(0.0f32, |m, &v| m.max(v));
            let rms = r.rms.iter().fold(0.0f32, |m, &v| m.max(v));
            format!(
                "Vumètre {name} : crête {}, RMS {}",
                db_text(to_db(peak)),
                db_text(to_db(rms))
            )
        }
    }
}

impl Switcher {
    /// The « Mélangeur » panel.
    pub(super) fn mixer_panel(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal_top(|ui| {
                let state = view.state;
                let with_sound = state
                    .inputs
                    .iter()
                    .filter_map(|i| Some((i, i.audio?)))
                    .collect::<Vec<_>>();
                if with_sound.is_empty() {
                    ui.label(RichText::new("Aucun input avec du son").weak());
                }
                for (input, audio) in with_sound {
                    ui.push_id(("strip", input.id.0), |ui| {
                        self.input_strip(ui, view, input, audio);
                    });
                    ui.separator();
                }
                for bus in AudioBus::ALL {
                    ui.push_id(("bus", bus.index()), |ui| self.bus_strip(ui, view, bus));
                }
                ui.separator();
                self.headphones_strip(ui, view);
            });
        });
    }

    fn input_strip(
        &mut self,
        ui: &mut egui::Ui,
        view: &View<'_>,
        input: &InputState,
        audio: InputAudio,
    ) {
        let id = input.id;
        let number = view.state.number(id).unwrap_or(0);
        let name = format!("{number} {}", input.name);
        ui.vertical(|ui| {
            ui.set_width(STRIP_WIDTH);
            ui.add(egui::Label::new(RichText::new(&name).small().strong()).truncate());
            ui.horizontal_top(|ui| {
                let reading = (view.audio.levels)(Level::Input(id)).map(|[_, post]| post);
                meter(ui, &name, reading.as_ref());
                self.fader(ui, view, Fader::Input(id), audio.gain, &name);
                ui.vertical(|ui| {
                    ui.set_width(BUTTON_WIDTH);
                    if toggle(ui, audio.mute, "Muet", MUTED).clicked() {
                        let mute = !audio.mute;
                        self.send(ui, view, Command::SetInputMute { input: id, mute });
                    }
                    if toggle(ui, audio.solo, "Solo", SOLOED)
                        .on_hover_text("Écouter au casque (le Master ne change pas)")
                        .clicked()
                    {
                        let solo = !audio.solo;
                        self.send(ui, view, Command::SetInputSolo { input: id, solo });
                    }
                    for bus in AudioBus::ALL {
                        let on = audio.sends_to(bus);
                        let hover = format!("Envoyer vers {}", bus_name(bus));
                        let send =
                            egui::Button::selectable(on, RichText::new(bus_label(bus)).small())
                                .frame_when_inactive(true)
                                .min_size(Vec2::new(BUTTON_WIDTH, 0.0));
                        if ui.add(send).on_hover_text(hover).clicked() {
                            self.send(
                                ui,
                                view,
                                Command::SetInputBus {
                                    input: id,
                                    bus,
                                    on: !on,
                                },
                            );
                        }
                    }
                });
            });
            let mut follow = audio.follow_video;
            if ui
                .checkbox(&mut follow, "Suit l'image")
                .on_hover_text(
                    "Au Master seulement quand l'input est au Program (fondu avec les transitions)",
                )
                .changed()
            {
                self.send(
                    ui,
                    view,
                    Command::SetAudioFollowVideo {
                        input: id,
                        on: follow,
                    },
                );
            }
            let mut ms = audio.delay_ms;
            let delay = ui
                .add(
                    egui::DragValue::new(&mut ms)
                        .range(0..=MAX_AUDIO_DELAY_MS)
                        .speed(1)
                        .suffix(" ms"),
                )
                .on_hover_text("Retard du son de l'input");
            if delay.changed() && ms != audio.delay_ms {
                self.send(ui, view, Command::SetInputDelay { input: id, ms });
            }
        });
    }

    fn bus_strip(&mut self, ui: &mut egui::Ui, view: &View<'_>, bus: AudioBus) {
        let settings = view.state.audio.bus(bus);
        let name = bus_name(bus);
        ui.vertical(|ui| {
            ui.set_width(STRIP_WIDTH);
            ui.label(RichText::new(name).small().strong());
            ui.horizontal_top(|ui| {
                let reading = (view.audio.levels)(Level::Bus(bus)).map(|[_, post]| post);
                meter(ui, name, reading.as_ref());
                self.fader(ui, view, Fader::Bus(bus), settings.gain, name);
                if toggle(ui, settings.mute, "Muet", MUTED).clicked() {
                    let mute = !settings.mute;
                    self.send(ui, view, Command::SetBusMute { bus, mute });
                }
            });
        });
    }

    fn headphones_strip(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        let monitor = &view.state.audio.monitor;
        let soloing = view
            .state
            .inputs
            .iter()
            .any(|i| i.audio.is_some_and(|a| a.solo));
        ui.vertical(|ui| {
            ui.set_width(STRIP_WIDTH);
            ui.label(RichText::new("Casque").small().strong());
            let reading = (view.audio.levels)(Level::Headphones).map(|[_, post]| post);
            meter(ui, "Casque", reading.as_ref());
            ui.label(
                RichText::new(if soloing { "Solo" } else { "Master" })
                    .small()
                    .color(if soloing {
                        SOLOED
                    } else {
                        ui.visuals().text_color()
                    }),
            );
            self.headphones_device(ui, view);
            for (mode, label, hover) in [
                (SoloMode::Afl, "AFL", "Solo après fader"),
                (
                    SoloMode::Pfl,
                    "PFL",
                    "Solo avant fader (entend un input muet)",
                ),
            ] {
                if ui
                    .radio(monitor.solo_mode == mode, label)
                    .on_hover_text(hover)
                    .clicked()
                    && monitor.solo_mode != mode
                {
                    let device = monitor.device.clone();
                    self.send(
                        ui,
                        view,
                        Command::SetMonitor {
                            device,
                            solo_mode: mode,
                        },
                    );
                }
            }
        });
    }

    /// The device of the headphones (« Par défaut »: the system's), and why they do not play.
    fn headphones_device(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        let Some(activity) = view.io.headphones else {
            ui.label(RichText::new("Casque coupé").small().weak())
                .on_hover_text("Lancé sans sortie casque (--no-headphones, soak)");
            return;
        };
        let monitor = &view.state.audio.monitor;
        let shown = |id: &vma_engine::DeviceId| {
            view.io
                .audio_sinks
                .iter()
                .find(|d| d.id == *id)
                .map_or_else(|| format!("{} (absent)", id.0), |d| d.name.clone())
        };
        let selected = monitor
            .device
            .as_ref()
            .map_or_else(|| "Par défaut".to_owned(), shown);
        let mut chosen = None;
        let label = ui.label(RichText::new("Sortie").small());
        egui::ComboBox::from_id_salt("headphones-device")
            .width(STRIP_WIDTH - 8.0)
            .selected_text(RichText::new(selected).small())
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(monitor.device.is_none(), "Par défaut")
                    .clicked()
                {
                    chosen = Some(None);
                }
                for sink in view.io.audio_sinks {
                    let on = monitor.device.as_ref() == Some(&sink.id);
                    if ui.selectable_label(on, &sink.name).clicked() {
                        chosen = Some(Some(sink.id.clone()));
                    }
                }
            })
            .response
            .labelled_by(label.id);
        if let Some(device) = chosen.filter(|d| *d != monitor.device) {
            let solo_mode = monitor.solo_mode;
            self.send(ui, view, Command::SetMonitor { device, solo_mode });
        }
        if let Some(error) = &activity.error {
            ui.label(RichText::new("Casque muet").small().color(super::WARNING))
                .on_hover_text(error);
        }
    }

    /// A vertical fader, −∞ to +10 dB; double-click: back to 0 dB. Sends each new level
    /// while it is dragged.
    fn fader(&mut self, ui: &mut egui::Ui, view: &View<'_>, which: Fader, level: Db, name: &str) {
        ui.vertical(|ui| {
            let (rect, response) =
                ui.allocate_exact_size(Vec2::new(28.0, FADER_HEIGHT), Sense::click_and_drag());
            let mut wanted = None;
            if let Some(pos) = response
                .interact_pointer_pos()
                .filter(|_| response.is_pointer_button_down_on())
            {
                let db = fader_level((rect.bottom() - pos.y) / rect.height());
                if self
                    .mixer
                    .held
                    .is_none_or(|(f, held)| f != which || held != db)
                {
                    wanted = Some(db);
                }
                self.mixer.held = Some((which, db));
            } else if self.mixer.held.is_some_and(|(f, _)| f == which) {
                self.mixer.held = None;
            }
            if response.double_clicked() {
                wanted = Some(Db::ZERO);
            }
            if let Some(db) = wanted.filter(|&db| db != level) {
                let command = match which {
                    Fader::Input(input) => Command::SetInputGain { input, db },
                    Fader::Bus(bus) => Command::SetBusGain { bus, db },
                };
                self.send(ui, view, command);
            }
            let shown = match self.mixer.held {
                Some((f, db)) if f == which => db,
                _ => level,
            };
            let label = format!("Fader {name}");
            response.widget_info(|| {
                egui::WidgetInfo::slider(true, f64::from(shown.get().max(-1000.0)), &label)
            });
            let response = response.on_hover_text("Double-clic : 0 dB");

            let painter = ui.painter();
            let track = Rect::from_center_size(rect.center(), Vec2::new(4.0, rect.height()));
            painter.rect_filled(track, 2.0, ui.visuals().widgets.inactive.bg_fill);
            let zero = rect.bottom() - rect.height() * scale(0.0, MAX_GAIN_DB);
            painter.line_segment(
                [
                    egui::pos2(rect.left() + 4.0, zero),
                    egui::pos2(rect.right() - 4.0, zero),
                ],
                Stroke::new(1.0, ui.visuals().weak_text_color()),
            );
            let y = rect.bottom() - rect.height() * scale(shown.get(), MAX_GAIN_DB);
            let handle =
                Rect::from_center_size(egui::pos2(rect.center().x, y), Vec2::new(26.0, 8.0));
            let color = if response.dragged() {
                ui.visuals().widgets.active.fg_stroke.color
            } else {
                ui.visuals().widgets.inactive.fg_stroke.color
            };
            painter.rect_filled(handle, 2.0, color);
            ui.label(RichText::new(db_text(shown.get())).small().monospace());
        });
    }
}

/// A toggle button filled with `color` when on.
fn toggle(ui: &mut egui::Ui, on: bool, text: &str, color: Color32) -> egui::Response {
    let mut button = egui::Button::new(RichText::new(text).small())
        .wrap_mode(egui::TextWrapMode::Extend)
        .min_size(Vec2::new(BUTTON_WIDTH, 0.0));
    if on {
        button = button.fill(color);
    }
    ui.add(button)
}

/// A stereo meter: RMS bars, peak marks and held peaks, −60 to 0 dBFS.
fn meter(ui: &mut egui::Ui, name: &str, reading: Option<&MeterReading>) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(14.0, FADER_HEIGHT), Sense::hover());
    let label = meter_label(name, reading);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::ProgressIndicator, true, &label));
    let painter = ui.painter();
    painter.rect_filled(rect, 1.0, Color32::from_gray(20));
    let Some(r) = reading else {
        return;
    };
    let height = |linear: f32| rect.height() * scale(to_db(linear), 0.0);
    let half = rect.width() / 2.0;
    for c in 0..2 {
        let (rms, peak, hold) = (
            r.rms.get(c).copied().unwrap_or(0.0),
            r.peak.get(c).copied().unwrap_or(0.0),
            r.hold.get(c).copied().unwrap_or(0.0),
        );
        let x = rect.left() + half * c as f32;
        let column = |h: f32| {
            Rect::from_min_max(
                egui::pos2(x + 1.0, rect.bottom() - h),
                egui::pos2(x + half - 1.0, rect.bottom()),
            )
        };
        // The RMS bar, zone by zone.
        let top = height(rms);
        let mut floor = 0.0;
        for (limit, color) in METER_ZONES {
            let ceiling = rect.height() * scale(limit, 0.0);
            if top > floor {
                let zone = column(top.min(ceiling));
                let zone =
                    Rect::from_min_max(zone.min, egui::pos2(zone.max.x, rect.bottom() - floor));
                painter.rect_filled(zone, 0.0, color);
            }
            floor = ceiling;
        }
        let color = METER_ZONES
            .iter()
            .find(|(limit, _)| to_db(peak) <= *limit)
            .map_or(Color32::from_rgb(230, 50, 50), |(_, c)| *c);
        let mark = |h: f32, stroke: Stroke| {
            let y = rect.bottom() - h;
            painter.line_segment(
                [egui::pos2(x + 1.0, y), egui::pos2(x + half - 1.0, y)],
                stroke,
            );
        };
        if peak > 0.0 {
            mark(height(peak), Stroke::new(1.0, color));
        }
        if hold > 0.0 {
            mark(height(hold), Stroke::new(1.0, Color32::WHITE));
        }
    }
    painter.rect_stroke(
        rect,
        1.0,
        Stroke::new(1.0, Color32::from_gray(60)),
        StrokeKind::Inside,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fader_law_and_texts() {
        assert_eq!(fader_level(0.0), Db::SILENT);
        assert_eq!(fader_level(1.0).get(), MAX_GAIN_DB);
        assert_eq!(fader_level(60.0 / 70.0).get(), 0.0);
        assert_eq!(fader_level(2.0).get(), MAX_GAIN_DB);
        assert_eq!(db_text(-12.5), "−12,5 dB");
        assert_eq!(db_text(3.0), "+3,0 dB");
        assert_eq!(db_text(f32::NEG_INFINITY), "−∞ dB");
        assert_eq!(scale(f32::NEG_INFINITY, 0.0), 0.0);
        assert_eq!(scale(0.0, 0.0), 1.0);
    }
}
