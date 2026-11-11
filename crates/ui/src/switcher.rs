//! The mixer surface: Preview, transition column, Program, input bar, shortcuts and the
//! settings windows (spec `ui.md`).
//!
//! It never changes the state: every action sends a `Command`, and every frame redraws from
//! the latest snapshot. It holds no GPU object either (only egui texture ids), so tests run
//! it headless with a probe instead of an engine.

use std::path::Path;

use egui::{Color32, Key, Rect, RichText, Sense, Stroke, StrokeKind, TextureId, Vec2};
use vma_core::{InputId, OutputId, OverlayChannel, Rgb8};
use vma_engine::{
    AutoPlay, Command, CommandId, CommandSender, DeviceId, Effect, InputSpec, InputState,
    InputStatus, Level, MAX_NAME_CHARS, MAX_TRANSITION_MS, MeterReading, MixDomain, Playback,
    RejectionNotice, SendError, Source, State, Timeline, TransitionSetting, TransitionState,
};

use crate::tally::{TALLY_PREVIEW, TALLY_PROGRAM, Tally};
use crate::{HeadphonesActivity, OutputActivity};
use icons::Icon;

mod about;
mod html_input;
mod icons;
mod mixer;
mod ndi_input;
mod outputs;
mod srt_input;
mod stinger;

/// Width of the transition column.
const COLUMN_WIDTH: f32 = 150.0;
/// Height of everything an input card adds to its thumbnail (title, two rows of buttons).
const CARD_EXTRA: f32 = 84.0;
/// How long a message stays visible, in seconds.
const MESSAGE_SECS: f64 = 4.0;
/// Height of the video controls under Preview / Program.
const CONTROLS_HEIGHT: f32 = 28.0;
/// Colour of warnings and messages.
const WARNING: Color32 = Color32::from_rgb(230, 160, 40);
/// Shortcut keys selecting inputs 1–9 for Preview.
const NUMBER_KEYS: [Key; 9] = [
    Key::Num1,
    Key::Num2,
    Key::Num3,
    Key::Num4,
    Key::Num5,
    Key::Num6,
    Key::Num7,
    Key::Num8,
    Key::Num9,
];
/// Colours offered by « Ajouter → Couleur ».
const PRESET_COLORS: [(&str, Rgb8); 6] = [
    ("Noir", Rgb8::BLACK),
    ("Blanc", Rgb8::WHITE),
    ("Gris", Rgb8::new(128, 128, 128)),
    ("Rouge", Rgb8::new(200, 30, 30)),
    ("Vert", Rgb8::new(30, 160, 60)),
    ("Bleu", Rgb8::new(30, 60, 200)),
];

/// The egui textures to show, valid for this UI frame.
pub struct Images<'a> {
    pub program: TextureId,
    pub preview: TextureId,
    pub thumbnails: TextureId,
    /// UV rectangle of an input's thumbnail in the atlas, if it has one this frame.
    pub thumbnail_uv: &'a dyn Fn(InputId) -> Option<Rect>,
}

impl std::fmt::Debug for Images<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Images")
            .field("program", &self.program)
            .field("preview", &self.preview)
            .field("thumbnails", &self.thumbnails)
            .finish_non_exhaustive()
    }
}

/// A capture device offered by « Ajouter → Caméra / capture ».
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureChoice {
    pub id: DeviceId,
    pub name: String,
}

/// A file the operator asked for. The caller opens the dialog (the switcher never blocks)
/// and adds the input with the chosen path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Video,
    Image,
    /// The file of a stinger slot.
    Stinger(vma_engine::StingerSlot),
}

/// What the switcher shows about the inputs fed by `vma-io` (capture, files).
pub struct IoView<'a> {
    /// Why capture devices and videos are unavailable (GStreamer missing), if they are.
    pub unavailable: Option<&'a str>,
    /// Capture devices connected now.
    pub devices: &'a [CaptureChoice],
    /// Position and duration of a video input.
    pub timeline: &'a dyn Fn(InputId) -> Option<Timeline>,
    /// Mode a capture input runs in, as shown (« 1280x720 30p MJPEG »).
    pub capture_mode: &'a dyn Fn(InputId) -> Option<String>,
    /// What a running output is doing.
    pub output_activity: &'a dyn Fn(OutputId) -> Option<OutputActivity>,
    /// Where recordings go when no folder is chosen.
    pub record_folder: Option<&'a Path>,
    /// Microphones and line inputs connected now.
    pub audio_sources: &'a [CaptureChoice],
    /// Headphones and speakers connected now.
    pub audio_sinks: &'a [CaptureChoice],
    /// What the headphones are doing (`None`: off in this session).
    pub headphones: Option<&'a HeadphonesActivity>,
    /// Why NDI® is unavailable (runtime missing), if it is.
    pub ndi_unavailable: Option<&'a str>,
    /// Why HTML inputs are unavailable (`vma-html` missing), if they are.
    pub html_unavailable: Option<&'a str>,
    /// NDI® sources seen on the network now.
    pub ndi_sources: &'a [String],
}

impl IoView<'static> {
    /// No device, no timeline, no output activity, nothing unavailable.
    pub fn empty() -> Self {
        fn no_timeline(_: InputId) -> Option<Timeline> {
            None
        }
        fn no_mode(_: InputId) -> Option<String> {
            None
        }
        fn no_activity(_: OutputId) -> Option<OutputActivity> {
            None
        }
        Self {
            unavailable: None,
            devices: &[],
            timeline: &no_timeline,
            capture_mode: &no_mode,
            output_activity: &no_activity,
            record_folder: None,
            audio_sources: &[],
            audio_sinks: &[],
            headphones: None,
            ndi_unavailable: None,
            html_unavailable: None,
            ndi_sources: &[],
        }
    }
}

impl std::fmt::Debug for IoView<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IoView")
            .field("unavailable", &self.unavailable)
            .field("devices", &self.devices)
            .finish_non_exhaustive()
    }
}

/// Everything the switcher reads for one UI frame.
#[derive(Debug)]
pub struct View<'a> {
    pub state: &'a State,
    pub commands: &'a CommandSender,
    pub images: Images<'a>,
    pub io: IoView<'a>,
    /// Screens connected now, for the full-screen output (empty: not known).
    pub screens: &'a [crate::Screen],
    /// Last frame index processed by the render thread (transition progress).
    pub frame: u64,
    /// FTB level of the outputs (0 = Program, 1 = black).
    pub fade_to_black: f32,
    /// Last command refused by the engine.
    pub rejection: Option<&'a RejectionNotice>,
    pub audio: AudioView<'a>,
}

/// What the switcher reads of the audio mixer.
pub struct AudioView<'a> {
    /// Meters, before and after fader (`None`: no sound received).
    pub levels: &'a dyn Fn(Level) -> Option<[MeterReading; 2]>,
}

impl AudioView<'static> {
    /// Every meter without sound.
    pub fn silent() -> Self {
        fn none(_: Level) -> Option<[MeterReading; 2]> {
            None
        }
        Self { levels: &none }
    }
}

impl std::fmt::Debug for AudioView<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AudioView").finish_non_exhaustive()
    }
}

/// T-Bar interaction state.
#[derive(Debug, Default)]
struct TBar {
    /// Position while the user holds the handle.
    held: Option<u8>,
    /// Last value sent, to send each value once.
    sent: Option<u8>,
    /// The transition completed during this drag: ignore the rest of it.
    done: bool,
}

/// UI-side state of the switcher (never mixer state).
#[derive(Debug)]
pub struct Switcher {
    /// « Statistiques » toggle of the toolbar (the panel itself belongs to the caller).
    pub show_stats: bool,
    tbar: TBar,
    /// Input whose settings window is open, and the name being edited.
    input_settings: Option<(InputId, String)>,
    settings_open: bool,
    message: Option<(String, f64)>,
    seen_rejection: Option<CommandId>,
    /// Button pressed last (its transition gets the progress bar).
    active_button: Option<u8>,
    new_color: [u8; 3],
    /// File asked for in « Ajouter », not taken by the caller yet.
    file_request: Option<FileKind>,
    /// Position slider being dragged: input and position (ms), sent on release.
    scrub: Option<(InputId, f64)>,
    outputs: outputs::OutputsUi,
    mixer: mixer::MixerUi,
    /// The SRT input being added (« Ajouter → SRT… »).
    new_srt: Option<vma_engine::SrtSource>,
    /// The NDI® source name being chosen (« Ajouter → NDI®… »).
    new_ndi: Option<String>,
    /// The HTML input being added (« Ajouter → Page web (HTML)… »).
    new_html: Option<html_input::NewHtml>,
    /// What is typed in the settings of an HTML input.
    html_fields: html_input::HtmlFields,
    about_open: bool,
    /// Size of the input thumbnails (« Réglages → Affichage »).
    thumbs: ThumbSize,
    /// The SVG loader of the icons is installed.
    icons_ready: bool,
}

/// Size of the input thumbnails, as the reference lets the operator choose it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ThumbSize {
    Small,
    #[default]
    Medium,
    Large,
}

impl ThumbSize {
    const ALL: [Self; 3] = [Self::Small, Self::Medium, Self::Large];

    fn size(self) -> Vec2 {
        match self {
            Self::Small => Vec2::new(128.0, 72.0),
            Self::Medium => Vec2::new(160.0, 90.0),
            Self::Large => Vec2::new(224.0, 126.0),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Small => "Petites",
            Self::Medium => "Moyennes",
            Self::Large => "Grandes",
        }
    }
}

impl Default for Switcher {
    fn default() -> Self {
        Self {
            show_stats: true,
            tbar: TBar::default(),
            input_settings: None,
            settings_open: false,
            message: None,
            seen_rejection: None,
            active_button: None,
            new_color: [64, 64, 64],
            file_request: None,
            scrub: None,
            outputs: outputs::OutputsUi::default(),
            mixer: mixer::MixerUi::default(),
            new_srt: None,
            new_ndi: None,
            new_html: None,
            html_fields: html_input::HtmlFields::default(),
            about_open: false,
            thumbs: ThumbSize::default(),
            icons_ready: false,
        }
    }
}

impl Switcher {
    /// The file the operator asked to open (« Ajouter → Vidéo… / Image… »), once.
    pub fn take_file_request(&mut self) -> Option<FileKind> {
        self.file_request.take()
    }

    pub fn show(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        let now = ui.input(|i| i.time);
        if let Some(notice) = view.rejection
            && self.seen_rejection != Some(notice.id)
        {
            self.seen_rejection = Some(notice.id);
            self.message = Some((format!("Commande refusée : {}", notice.reason), now));
        }

        if !self.icons_ready {
            icons::install(ui.ctx());
            self.icons_ready = true;
        }
        self.shortcuts(ui, view);

        // Top: messages on the left, « Réglages » on the right (as in the reference).
        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(reason) = view.io.unavailable {
                    ui.label(
                        RichText::new("GStreamer introuvable : caméras et vidéos désactivées")
                            .color(WARNING),
                    )
                    .on_hover_text(reason);
                }
                if let Some((text, since)) = &self.message {
                    if now - since < MESSAGE_SECS {
                        ui.label(RichText::new(text).color(WARNING));
                    } else {
                        self.message = None;
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(icons::button(Icon::Info, "À propos")).clicked() {
                        self.about_open = true;
                    }
                    if ui.add(icons::button(Icon::Settings, "Réglages")).clicked() {
                        self.settings_open = true;
                    }
                });
            });
        });
        // Bottom: adding inputs and the outputs on the left, panels on the right.
        egui::Panel::bottom("footer").show(ui, |ui| {
            ui.horizontal(|ui| {
                let plus = icons::tinted(ui, Icon::Plus);
                ui.menu_button((plus, "Ajouter un input"), |ui| self.add_menu(ui, view));
                ui.separator();
                self.output_buttons(ui, view);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    icons::toggle(ui, &mut self.show_stats, Icon::ChartColumn, "Statistiques")
                        .on_hover_text("Panneau des statistiques");
                    icons::toggle(ui, &mut self.mixer.open, Icon::SlidersVertical, "Mélangeur")
                        .on_hover_text("Mélangeur audio");
                });
            });
        });
        // Inputs above it on the left, the audio mixer beside them on the right. The divider
        // is dragged up for more rows of inputs.
        let row = self.thumbs.size().y + CARD_EXTRA;
        let least = if self.mixer.open {
            mixer::MIXER_HEIGHT.max(row)
        } else {
            row
        };
        // At first, the inputs take what Preview and Program leave (16:9 pictures side by side).
        let side = ((ui.available_width() - COLUMN_WIDTH) / 2.0).max(0.0);
        let aspect = view.state.format.resolution.aspect_ratio();
        let views = side / aspect + 24.0 + CONTROLS_HEIGHT + 12.0;
        let first = (ui.available_height() - views).max(least);
        egui::Panel::bottom("inputs")
            .resizable(true)
            .default_size(first)
            .min_size(least)
            .show(ui, |ui| {
                if self.mixer.open {
                    let width = mixer::panel_width(view.state, ui.available_width());
                    egui::Panel::right("mixer")
                        .resizable(true)
                        .default_size(width)
                        .show(ui, |ui| self.mixer_panel(ui, view));
                }
                self.input_bar(ui, view);
            });
        egui::CentralPanel::default().show(ui, |ui| {
            let full = ui.available_rect_before_wrap();
            let side = ((full.width() - COLUMN_WIDTH) / 2.0).max(0.0);
            let preview = Rect::from_min_size(full.min, Vec2::new(side, full.height()));
            let column = Rect::from_min_size(
                full.min + Vec2::new(side, 0.0),
                Vec2::new(COLUMN_WIDTH, full.height()),
            );
            let program = Rect::from_min_size(
                full.min + Vec2::new(side + COLUMN_WIDTH, 0.0),
                Vec2::new(side, full.height()),
            );
            ui.allocate_rect(full, Sense::hover());
            self.output_view(ui, view, preview, Output::Preview);
            ui.scope_builder(egui::UiBuilder::new().max_rect(column.shrink(6.0)), |ui| {
                self.transition_column(ui, view);
            });
            self.output_view(ui, view, program, Output::Program);
        });

        self.input_settings_window(ui, view);
        self.settings_window(ui, view);
        self.outputs_window(ui, view);
        self.srt_input_window(ui, view);
        self.ndi_input_window(ui, view);
        self.html_input_window(ui, view);
        self.about_window(ui);
    }

    fn send(&mut self, ui: &egui::Ui, view: &View<'_>, command: Command) {
        if let Err(error) = view.commands.send(Source::Ui, command) {
            let text = match error {
                SendError::Full => "Le moteur est saturé : commande ignorée",
                SendError::Stopped => "Le moteur est arrêté",
            };
            self.message = Some((text.to_owned(), ui.input(|i| i.time)));
        }
    }

    fn shortcuts(&mut self, ui: &egui::Ui, view: &View<'_>) {
        // Typing in a text field must not switch the Program.
        if ui.ctx().egui_wants_keyboard_input() {
            return;
        }
        let (numbers, space, enter) = ui.input(|i| {
            (
                NUMBER_KEYS.map(|k| i.key_pressed(k)),
                i.key_pressed(Key::Space),
                i.key_pressed(Key::Enter),
            )
        });
        for (pressed, input) in numbers.iter().zip(&view.state.inputs) {
            if *pressed {
                self.send(ui, view, Command::SetPreview { input: input.id });
            }
        }
        if space {
            self.send(ui, view, Command::Cut);
        }
        if enter {
            self.active_button = Some(1);
            self.send(ui, view, Command::Transition { button: 1 });
        }
    }

    fn output_view(&mut self, ui: &mut egui::Ui, view: &View<'_>, area: Rect, output: Output) {
        let state = view.state;
        let (title, color, texture, input) = match output {
            Output::Preview => ("PREVIEW", TALLY_PREVIEW, view.images.preview, state.preview),
            Output::Program => ("PROGRAM", TALLY_PROGRAM, view.images.program, state.program),
        };
        let name = input
            .and_then(|id| state.input(id).map(|i| (id, i)))
            .map(|(id, i)| format!("{} {}", state.number(id).unwrap_or(0), i.name));
        let header = Rect::from_min_size(area.min, Vec2::new(area.width(), 22.0));
        ui.painter().text(
            header.left_center() + Vec2::new(6.0, 0.0),
            egui::Align2::LEFT_CENTER,
            match &name {
                Some(n) => format!("{title} · {n}"),
                None => title.to_owned(),
            },
            egui::FontId::proportional(14.0),
            color,
        );
        let mut body = Rect::from_min_max(area.min + Vec2::new(0.0, 24.0), area.max).shrink(6.0);
        let video = input.and_then(|id| state.input(id)?.playback.map(|p| (id, p)));
        if video.is_some() {
            body.max.y -= CONTROLS_HEIGHT;
        }
        let aspect = state.format.resolution.aspect_ratio();
        let size = if body.width() / body.height().max(1.0) > aspect {
            Vec2::new(body.height() * aspect, body.height())
        } else {
            Vec2::new(body.width(), body.width() / aspect)
        };
        // Top-aligned, centred horizontally.
        let rect =
            Rect::from_min_size(egui::pos2(body.center().x - size.x / 2.0, body.top()), size);
        egui::Image::from_texture((texture, size)).paint_at(ui, rect);
        ui.painter()
            .rect_stroke(rect, 0.0, Stroke::new(2.0, color), StrokeKind::Outside);
        if let Some((id, playback)) = video {
            let controls = Rect::from_min_size(
                egui::pos2(rect.left(), rect.bottom() + 4.0),
                Vec2::new(rect.width(), CONTROLS_HEIGHT - 4.0),
            );
            ui.scope_builder(egui::UiBuilder::new().max_rect(controls), |ui| {
                ui.push_id(output.name(), |ui| {
                    self.video_controls(ui, view, id, playback)
                });
            });
        }
    }

    /// Play / pause, back to the start, position (spec `ui.md`, « Contrôles vidéo »).
    fn video_controls(
        &mut self,
        ui: &mut egui::Ui,
        view: &View<'_>,
        id: InputId,
        playback: Playback,
    ) {
        let timeline = (view.io.timeline)(id).unwrap_or_default();
        ui.horizontal(|ui| {
            let command = if playback.playing {
                let pause = ui.add(icons::only(Icon::Pause)).on_hover_text("Pause");
                icons::named(pause, "Pause", None)
                    .clicked()
                    .then_some(Command::Pause { input: id })
            } else {
                let play = ui.add(icons::only(Icon::Play)).on_hover_text("Lecture");
                icons::named(play, "Lecture", None)
                    .clicked()
                    .then_some(Command::Play { input: id })
            };
            if let Some(command) = command {
                self.send(ui, view, command);
            }
            let start = ui
                .add(icons::only(Icon::SkipBack))
                .on_hover_text("Revenir au début");
            if icons::named(start, "Début", None).clicked() {
                self.send(ui, view, Command::Restart { input: id });
            }
            let duration = timeline.duration_ms.unwrap_or(0);
            let mut position = match self.scrub {
                Some((scrubbed, ms)) if scrubbed == id => ms,
                _ => timeline.position_ms.min(duration) as f64,
            };
            let time = ui.label(
                RichText::new(format!(
                    "{} / {}{}",
                    clock(position as u64),
                    clock(duration),
                    if timeline.ended { " · fin" } else { "" }
                ))
                .monospace(),
            );
            ui.spacing_mut().slider_width = ui.available_width().max(40.0);
            let slider = ui
                .add_enabled(
                    duration > 0,
                    egui::Slider::new(&mut position, 0.0..=duration as f64).show_value(false),
                )
                .labelled_by(time.id);
            if slider.dragged() {
                self.scrub = Some((id, position));
            }
            if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
                self.scrub = None;
                let ms = position.round() as u64;
                self.send(ui, view, Command::SetPosition { input: id, ms });
            }
        });
    }

    fn transition_column(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        let state = view.state;
        let wide = Vec2::new(ui.available_width(), 30.0);
        ui.vertical_centered_justified(|ui| {
            if ui
                .add_sized(wide, icons::button(Icon::Play, "Quick Play"))
                .on_hover_text(format!("Preview → Program, {}", describe(state.quick_play)))
                .clicked()
            {
                self.active_button = None;
                self.send(ui, view, Command::QuickPlay { input: None });
            }
            if ui
                .add_sized(wide, icons::button(Icon::Scissors, "Cut"))
                .on_hover_text("Preview → Program, sans transition")
                .clicked()
            {
                self.send(ui, view, Command::Cut);
            }
            ui.add_space(6.0);
            for (index, setting) in state.buttons.iter().enumerate() {
                let button = index as u8 + 1;
                let label = format!("{button} · {}", describe(*setting));
                // The button, and beside it the arrow that sets its effect and duration
                // (a right click on the button does the same).
                let arrow = 22.0;
                let gap = ui.spacing().item_spacing.x;
                let response = ui
                    .horizontal(|ui| {
                        let main = Vec2::new(wide.x - arrow - gap, wide.y);
                        let response = ui.add_sized(main, egui::Button::new(label));
                        ui.scope(|ui| {
                            ui.spacing_mut().button_padding.x = 2.0;
                            ui.set_min_size(Vec2::new(arrow, wide.y));
                            let arrow = icons::tinted(ui, Icon::ChevronDown);
                            ui.menu_button(arrow, |ui| {
                                self.button_menu(ui, view, button, *setting)
                            })
                            .response
                            .on_hover_text(format!("Effet et durée du bouton {button}"));
                        });
                        response
                    })
                    .inner;
                if response.clicked() {
                    self.active_button = Some(button);
                    self.send(ui, view, Command::Transition { button });
                }
                response.context_menu(|ui| self.button_menu(ui, view, button, *setting));
                if self.active_button == Some(button)
                    && let Some(t @ TransitionState::Timed { .. }) = state.transition
                {
                    ui.add(egui::ProgressBar::new(t.progress(view.frame)).desired_height(4.0));
                }
            }
            ui.add_space(8.0);
            self.ftb_button(ui, view);
            ui.add_space(8.0);
            self.t_bar(ui, view);
            ui.add_space(8.0);
            self.overlay_channels(ui, view);
        });
    }

    /// Under the T-Bar, one button per overlay channel: lit while the channel shows its
    /// input; a click takes it out, or brings back the input it showed last.
    fn overlay_channels(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        let state = view.state;
        ui.horizontal(|ui| {
            ui.add(icons::tinted(ui, Icon::Layers));
            ui.label(RichText::new("Overlays").weak());
        });
        ui.horizontal(|ui| {
            let gap = ui.spacing().item_spacing.x;
            let side = ((ui.available_width() - 3.0 * gap) / 4.0).max(16.0);
            for channel in OverlayChannel::ALL {
                let n = channel.number();
                let overlay = state.overlay(channel);
                let input = overlay
                    .input
                    .and_then(|id| state.input(id).map(|i| (id, i)));
                let on = overlay.on && input.is_some();
                let response = named_button(
                    ui,
                    Vec2::new(side, 26.0),
                    &n.to_string(),
                    &format!("Canal d'overlay {n}"),
                    on,
                    input.is_some(),
                );
                let response = match input {
                    Some((_, i)) if on => response.on_hover_text(format!("Sortir « {} »", i.name)),
                    Some((_, i)) => response.on_hover_text(format!("Rentrer « {} »", i.name)),
                    None => response.on_disabled_hover_text(format!("Overlay {n} : vide")),
                };
                if response.clicked()
                    && let Some((input, _)) = input
                {
                    let command = if on {
                        Command::OverlayOut { channel }
                    } else {
                        Command::OverlayIn { channel, input }
                    };
                    self.send(ui, view, command);
                }
            }
        });
    }

    fn button_menu(
        &mut self,
        ui: &mut egui::Ui,
        view: &View<'_>,
        button: u8,
        setting: TransitionSetting,
    ) {
        ui.label(format!("Bouton {button}"));
        let mut effect = setting.effect;
        let mut duration = setting.duration_ms;
        ui.horizontal(|ui| {
            for e in stinger::EFFECTS {
                ui.radio_value(&mut effect, e, describe_effect(e));
            }
        });
        ui.add_enabled(
            effect == Effect::Fade,
            egui::DragValue::new(&mut duration)
                .range(0..=MAX_TRANSITION_MS)
                .speed(10)
                .suffix(" ms"),
        );
        if effect != setting.effect || duration != setting.duration_ms {
            self.send(
                ui,
                view,
                Command::SetTransition {
                    button,
                    effect,
                    duration_ms: duration,
                },
            );
        }
    }

    /// Vertical T-Bar: top = Program alone, bottom = transition complete (button 1's effect).
    fn t_bar(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        let state = view.state;
        // Room for the overlay channels underneath.
        let height = (ui.available_height() - 60.0).clamp(80.0, 240.0);
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(36.0, height), Sense::click_and_drag());
        let pointer = response.interact_pointer_pos();
        if let Some(pos) = pointer.filter(|_| response.is_pointer_button_down_on()) {
            let value = ((pos.y - rect.top()) / rect.height() * 255.0)
                .round()
                .clamp(0.0, 255.0) as u8;
            if !self.tbar.done {
                self.tbar.held = Some(value);
                if self.tbar.sent != Some(value) {
                    self.tbar.sent = Some(value);
                    self.send(ui, view, Command::SetFader { value });
                    // Reaching the end completes the transition; the rest of this drag is
                    // ignored and the T-Bar shows its start position again.
                    self.tbar.done = value == u8::MAX;
                }
            }
        } else {
            self.tbar = TBar::default();
        }
        let value = self
            .tbar
            .held
            .filter(|_| !self.tbar.done)
            .unwrap_or(state.fader());
        response.widget_info(|| egui::WidgetInfo::slider(true, f64::from(value), "T-Bar"));

        let painter = ui.painter();
        let track = Rect::from_center_size(rect.center(), Vec2::new(6.0, rect.height()));
        painter.rect_filled(track, 3.0, ui.visuals().widgets.inactive.bg_fill);
        let y = rect.top() + rect.height() * f32::from(value) / 255.0;
        let handle = Rect::from_center_size(egui::pos2(rect.center().x, y), Vec2::new(34.0, 10.0));
        let handle_color = if state.transition.is_some() {
            TALLY_PROGRAM
        } else {
            ui.visuals().widgets.active.fg_stroke.color
        };
        painter.rect_filled(handle, 2.0, handle_color);
    }

    fn input_bar(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for (index, input) in view.state.inputs.iter().enumerate() {
                        self.input_card(ui, view, index + 1, input);
                    }
                });
            });
    }

    fn add_menu(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        ui.menu_button((icons::tinted(ui, Icon::Palette), "Couleur"), |ui| {
            for (name, color) in PRESET_COLORS {
                if ui.button(name).clicked() {
                    self.add(ui, view, InputSpec::Color { color });
                }
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.color_edit_button_srgb(&mut self.new_color);
                if ui.button("Ajouter cette couleur").clicked() {
                    let [r, g, b] = self.new_color;
                    self.add(
                        ui,
                        view,
                        InputSpec::Color {
                            color: Rgb8::new(r, g, b),
                        },
                    );
                }
            });
        });
        if ui.add(icons::button(Icon::Tv, "Mire")).clicked() {
            let format = view.state.format;
            self.add(
                ui,
                view,
                InputSpec::TestSource {
                    rate: format.rate,
                    resolution: format.resolution,
                },
            );
        }
        ui.separator();
        let unavailable = view.io.unavailable;
        ui.add_enabled_ui(unavailable.is_none(), |ui| {
            ui.menu_button(
                (icons::tinted(ui, Icon::Camera), "Caméra / capture"),
                |ui| {
                    if view.io.devices.is_empty() {
                        ui.label(RichText::new("Aucun périphérique détecté").weak());
                    }
                    for device in view.io.devices {
                        if ui
                            .button(&device.name)
                            .on_hover_text(&device.id.0)
                            .clicked()
                        {
                            let name: String = device.name.chars().take(MAX_NAME_CHARS).collect();
                            let spec = InputSpec::Capture {
                                device: device.id.clone(),
                            };
                            let name = (!name.trim().is_empty()).then_some(name);
                            self.send(ui, view, Command::AddInput { spec, name });
                            ui.close();
                        }
                    }
                },
            );
            ui.menu_button((icons::tinted(ui, Icon::Mic), "Son (micro, ligne)"), |ui| {
                if view.io.audio_sources.is_empty() {
                    ui.label(RichText::new("Aucun périphérique audio détecté").weak());
                }
                for device in view.io.audio_sources {
                    if ui
                        .button(&device.name)
                        .on_hover_text(&device.id.0)
                        .clicked()
                    {
                        let name: String = device.name.chars().take(MAX_NAME_CHARS).collect();
                        let spec = InputSpec::AudioDevice {
                            device: device.id.clone(),
                        };
                        let name = (!name.trim().is_empty()).then_some(name);
                        self.send(ui, view, Command::AddInput { spec, name });
                        ui.close();
                    }
                }
            });
            if ui.add(icons::button(Icon::Film, "Vidéo…")).clicked() {
                self.file_request = Some(FileKind::Video);
                ui.close();
            }
            if ui.add(icons::button(Icon::Cable, "SRT…")).clicked() {
                self.new_srt = Some(vma_engine::SrtSource::default());
                ui.close();
            }
            let ndi = ui.add_enabled(
                view.io.ndi_unavailable.is_none(),
                icons::button(Icon::Cast, "NDI®…"),
            );
            if let Some(reason) = view.io.ndi_unavailable {
                ndi.on_disabled_hover_text(format!(
                    "Runtime NDI® introuvable : installer le runtime NDI (NDI Tools), \
                     puis redémarrer VMA.\n{reason}"
                ));
            } else if ndi.clicked() {
                self.new_ndi = Some(String::new());
                ui.close();
            }
            self.html_menu_entry(ui, view);
        });
        if unavailable.is_some() {
            ui.label(
                RichText::new("GStreamer introuvable :\ncaméras et vidéos indisponibles")
                    .small()
                    .color(WARNING),
            );
        }
        if ui.add(icons::button(Icon::Image, "Image…")).clicked() {
            self.file_request = Some(FileKind::Image);
            ui.close();
        }
    }

    fn add(&mut self, ui: &egui::Ui, view: &View<'_>, spec: InputSpec) {
        self.send(ui, view, Command::AddInput { spec, name: None });
        ui.close();
    }

    fn input_card(
        &mut self,
        ui: &mut egui::Ui,
        view: &View<'_>,
        number: usize,
        input: &InputState,
    ) {
        let state = view.state;
        let id = input.id;
        let tally = Tally::of(state, id);
        let frame = egui::Frame::new()
            .inner_margin(4.0)
            .corner_radius(4.0)
            .stroke(Stroke::new(
                3.0,
                tally
                    .color()
                    .unwrap_or(ui.visuals().widgets.noninteractive.bg_stroke.color),
            ));
        let thumb_size = self.thumbs.size();
        frame.show(ui, |ui| {
            ui.set_width(thumb_size.x);
            ui.vertical(|ui| {
                // Title bar: number and name; double click = the input's settings.
                let title = ui.add(
                    egui::Label::new(RichText::new(format!("{number} {}", input.name)).strong())
                        .truncate()
                        .sense(Sense::click()),
                );
                let (rect, thumb) = ui.allocate_exact_size(thumb_size, Sense::click());
                match (view.images.thumbnail_uv)(id) {
                    Some(uv) => egui::Image::from_texture((view.images.thumbnails, thumb_size))
                        .uv(uv)
                        .paint_at(ui, rect),
                    // No picture yet (or not a GPU-backed input yet): black.
                    None => {
                        ui.painter().rect_filled(rect, 0.0, Color32::BLACK);
                    }
                }
                let sound_only = !input.spec.kind().has_video();
                if sound_only && matches!(input.status, InputStatus::Live { .. }) {
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "Son seul",
                        egui::FontId::proportional(13.0),
                        ui.visuals().weak_text_color(),
                    );
                }
                status_overlay(ui, rect, input);
                thumb.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::Role::Button,
                        true,
                        format!("Vignette {number}"),
                    )
                });
                // Mode (size, rate, format) on hover.
                let thumb = match source_mode(input, (view.io.capture_mode)(id)) {
                    Some(mode) => thumb.on_hover_text(mode),
                    None => thumb,
                };
                if thumb.clicked() {
                    self.send(ui, view, Command::SetPreview { input: id });
                }
                if title.double_clicked() || thumb.double_clicked() {
                    self.input_settings = Some((id, input.name.clone()));
                }

                // Close, Quick Play, Cut, Loop, then 1–4 and the sound: the reference's order.
                ui.horizontal(|ui| {
                    let close = ui
                        .add(icons::only(Icon::X).small())
                        .on_hover_text("Fermer l'input");
                    if icons::named(close, "×", None).clicked() {
                        self.send(ui, view, Command::RemoveInput { input: id });
                    }
                    // A sound-only input has no picture to put on air.
                    if !sound_only
                        && icons::named(
                            ui.add(icons::only(Icon::Play).small())
                                .on_hover_text("Quick Play : cet input au Program"),
                            "QP",
                            None,
                        )
                        .clicked()
                    {
                        self.send(ui, view, Command::QuickPlay { input: Some(id) });
                    }
                    if !sound_only
                        && icons::named(
                            ui.add(icons::only(Icon::Scissors).small())
                                .on_hover_text("Cut : cet input au Program, sans transition"),
                            "Cut",
                            None,
                        )
                        .clicked()
                    {
                        self.send(ui, view, Command::CutDirect { input: id });
                    }
                    if let Some(playback) = input.playback {
                        let looping = playback.looping;
                        let repeat = ui
                            .add(icons::only(Icon::Repeat).small().selected(looping))
                            .on_hover_text(if looping {
                                "Lecture en boucle : l'arrêter"
                            } else {
                                "Lire en boucle (Loop)"
                            });
                        if icons::named(repeat, "Loop", Some(looping)).clicked() {
                            let looping = !looping;
                            self.send(
                                ui,
                                view,
                                Command::SetLoop {
                                    input: id,
                                    enabled: looping,
                                },
                            );
                        }
                    }
                });
                ui.horizontal(|ui| {
                    // Overlay channels 1–4: in, or out if it is there (ADR 0010).
                    if !sound_only {
                        for channel in OverlayChannel::ALL {
                            let n = channel.number();
                            let on = state.overlay(channel).shows(id);
                            let hint = if on {
                                format!("Retirer de l'overlay {n}")
                            } else {
                                format!("Mettre en overlay {n}")
                            };
                            let button = ui
                                .add(egui::Button::new(n.to_string()).small().selected(on))
                                .on_hover_text(hint);
                            if button.clicked() {
                                self.send(ui, view, Command::OverlayToggle { channel, input: id });
                            }
                        }
                    }
                    // The sound of the input, on or muted (as its mixer strip's « Muet »).
                    if let Some(audio) = input.audio {
                        let (icon, fill) = if audio.mute {
                            (Icon::VolumeX, mixer::MUTED)
                        } else {
                            (Icon::Volume2, ui.visuals().widgets.inactive.weak_bg_fill)
                        };
                        let sound = ui.add(icons::only(icon).small().fill(fill));
                        let response = icons::named(
                            sound,
                            &format!("Son de l'input {number}"),
                            Some(audio.mute),
                        )
                        .on_hover_text(if audio.mute {
                            "Son coupé : le rétablir"
                        } else {
                            "Couper le son de l'input"
                        });
                        if response.clicked() {
                            let mute = !audio.mute;
                            self.send(ui, view, Command::SetInputMute { input: id, mute });
                        }
                    }
                });
            });
        });
    }

    fn input_settings_window(&mut self, ui: &egui::Ui, view: &View<'_>) {
        let Some((id, mut name)) = self.input_settings.take() else {
            return;
        };
        let Some(input) = view.state.input(id) else {
            return; // Removed meanwhile.
        };
        let mut open = true;
        egui::Window::new(format!(
            "Réglages de l'input {}",
            view.state.number(id).unwrap_or(0)
        ))
        .id(egui::Id::unique("vma.input_settings"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ui.ctx(), |ui| {
            ui.horizontal(|ui| {
                let label = ui.label("Nom");
                let edit = ui.text_edit_singleline(&mut name).labelled_by(label.id);
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                if (enter || edit.lost_focus()) && name.trim() != input.name {
                    self.send(
                        ui,
                        view,
                        Command::RenameInput {
                            input: id,
                            name: name.clone(),
                        },
                    );
                }
            });
            if let InputSpec::Color { color } = input.spec {
                ui.horizontal(|ui| {
                    ui.label("Couleur");
                    let mut rgb = [color.r, color.g, color.b];
                    if ui.color_edit_button_srgb(&mut rgb).changed() {
                        let [r, g, b] = rgb;
                        self.send(
                            ui,
                            view,
                            Command::SetColor {
                                input: id,
                                color: Rgb8::new(r, g, b),
                            },
                        );
                    }
                });
            }
            self.html_settings(ui, view, id);
            if let Some(playback) = input.playback {
                let mut looping = playback.looping;
                if ui
                    .checkbox(&mut looping, "Lecture en boucle (Loop)")
                    .changed()
                {
                    self.send(
                        ui,
                        view,
                        Command::SetLoop {
                            input: id,
                            enabled: looping,
                        },
                    );
                }
            }
        });
        if open {
            self.input_settings = Some((id, name));
        }
    }

    fn settings_window(&mut self, ui: &egui::Ui, view: &View<'_>) {
        if !self.settings_open {
            return;
        }
        let state = view.state;
        let mut open = true;
        egui::Window::new("Réglages")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ui.ctx(), |ui| {
                ui.label(RichText::new("Quick Play").strong());
                let qp = state.quick_play;
                let (mut effect, mut duration) = (qp.effect, qp.duration_ms);
                ui.horizontal(|ui| {
                    for e in stinger::EFFECTS {
                        ui.radio_value(&mut effect, e, describe_effect(e));
                    }
                    ui.add_enabled(
                        effect == Effect::Fade,
                        egui::DragValue::new(&mut duration)
                            .range(0..=MAX_TRANSITION_MS)
                            .speed(10)
                            .suffix(" ms"),
                    );
                });
                if effect != qp.effect || duration != qp.duration_ms {
                    self.send(
                        ui,
                        view,
                        Command::SetQuickPlay {
                            effect,
                            duration_ms: duration,
                        },
                    );
                }

                ui.separator();
                ui.label(RichText::new("Vidéos").strong());
                let mut auto: AutoPlay = state.auto_play;
                ui.checkbox(
                    &mut auto.play_on_transition,
                    "Démarrer la vidéo quand elle passe au Program",
                );
                ui.checkbox(&mut auto.restart_on_transition, "… depuis le début");
                ui.checkbox(
                    &mut auto.pause_after_transition,
                    "Mettre en pause la vidéo qui quitte le Program",
                );
                if auto != state.auto_play {
                    self.send(ui, view, Command::SetAutoPlay(auto));
                }

                ui.separator();
                ui.label(RichText::new("Overlays (entrée et sortie)").strong());
                egui::Grid::new("vma.overlay_transitions").show(ui, |ui| {
                    for channel in OverlayChannel::ALL {
                        let n = channel.number();
                        let current = state.overlay(channel).transition;
                        let (mut effect, mut duration) = (current.effect, current.duration_ms);
                        egui::ComboBox::from_label(format!("Overlay {n}"))
                            .selected_text(describe_effect(effect))
                            .show_ui(ui, |ui| {
                                for e in [Effect::Fade, Effect::Cut] {
                                    ui.selectable_value(&mut effect, e, describe_effect(e));
                                }
                            });
                        ui.add_enabled(
                            effect == Effect::Fade,
                            egui::DragValue::new(&mut duration)
                                .range(0..=MAX_TRANSITION_MS)
                                .speed(10)
                                .suffix(" ms"),
                        );
                        ui.end_row();
                        if effect != current.effect || duration != current.duration_ms {
                            self.send(
                                ui,
                                view,
                                Command::SetOverlayTransition {
                                    channel,
                                    effect,
                                    duration_ms: duration,
                                },
                            );
                        }
                    }
                });

                ui.separator();
                self.stinger_settings(ui, view);

                ui.separator();
                ui.label(RichText::new("Affichage").strong());
                ui.horizontal(|ui| {
                    ui.label("Vignettes des inputs");
                    for size in ThumbSize::ALL {
                        ui.radio_value(&mut self.thumbs, size, size.label());
                    }
                });

                ui.separator();
                ui.label(RichText::new("Mixage des Fade").strong());
                let mut domain = state.mix_domain;
                ui.radio_value(
                    &mut domain,
                    MixDomain::Gamma,
                    "Gamma (comme un mélangeur broadcast)",
                );
                ui.radio_value(&mut domain, MixDomain::Linear, "Lumière linéaire");
                if domain != state.mix_domain {
                    self.send(ui, view, Command::SetMixDomain { domain });
                }
            });
        self.settings_open = open;
    }
}

#[derive(Debug, Clone, Copy)]
enum Output {
    Preview,
    Program,
}

impl Output {
    fn name(self) -> &'static str {
        match self {
            Self::Preview => "preview",
            Self::Program => "program",
        }
    }
}

/// « 01:03 » (minutes : secondes), hours added past one hour.
fn clock(ms: u64) -> String {
    let s = ms / 1000;
    match s / 3600 {
        0 => format!("{:02}:{:02}", s / 60, s % 60),
        h => format!("{h}:{:02}:{:02}", s / 60 % 60, s % 60),
    }
}

/// What the thumbnail tooltip says about an input's source: the capture mode, and the
/// format of the pictures it delivers.
fn source_mode(input: &InputState, capture: Option<String>) -> Option<String> {
    let delivered = match input.status {
        InputStatus::Live { source: Some(f) } => Some(crate::stats_panel::source_label(&f)),
        _ => None,
    };
    match (capture, delivered) {
        (Some(c), Some(d)) => Some(format!("Mode {c} → {d}")),
        (Some(c), None) => Some(format!("Mode {c}")),
        (None, d) => d,
    }
}

/// « Fade 0,5 s », « Cut ».
fn describe_effect(effect: Effect) -> &'static str {
    match effect {
        Effect::Fade => "Fade",
        Effect::Cut => "Cut",
        Effect::Stinger(slot) if slot.number() == 1 => "Stinger 1",
        Effect::Stinger(_) => "Stinger 2",
    }
}

fn describe(setting: TransitionSetting) -> String {
    match setting.effect {
        Effect::Cut => "Cut".to_owned(),
        Effect::Fade if setting.duration_ms == 0 => "Cut".to_owned(),
        Effect::Fade => {
            let secs = f64::from(setting.duration_ms) / 1000.0;
            format!("Fade {}", format!("{secs} s").replace('.', ","))
        }
        Effect::Stinger(_) => describe_effect(setting.effect).to_owned(),
    }
}

/// A button showing `text` (a digit, a symbol) whose accessible name is `name`, so that
/// screen readers and tests tell it from the other buttons showing the same text.
fn named_button(
    ui: &mut egui::Ui,
    size: Vec2,
    text: &str,
    name: &str,
    selected: bool,
    enabled: bool,
) -> egui::Response {
    let sense = if enabled {
        Sense::click()
    } else {
        Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(size, sense);
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, enabled, selected, name));
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        let style = if !enabled {
            &visuals.widgets.noninteractive
        } else {
            visuals.widgets.style(&response)
        };
        let (fill, color) = if selected {
            (visuals.selection.bg_fill, visuals.selection.stroke.color)
        } else if enabled {
            (style.weak_bg_fill, style.text_color())
        } else {
            (style.weak_bg_fill, visuals.weak_text_color())
        };
        ui.painter().rect(
            rect,
            style.corner_radius,
            fill,
            style.bg_stroke,
            StrokeKind::Inside,
        );
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            text,
            egui::TextStyle::Button.resolve(ui.style()),
            color,
        );
    }
    response
}

/// Status written over a thumbnail when the input has no live picture.
fn status_overlay(ui: &egui::Ui, rect: Rect, input: &InputState) {
    let text = match input.status {
        InputStatus::Live { .. } => return,
        InputStatus::Starting => "Démarrage…".to_owned(),
        InputStatus::SignalLost { attempts, .. } => {
            format!("Signal perdu (tentative {attempts})")
        }
    };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(12.0),
        WARNING,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_labels() {
        assert_eq!(describe(TransitionSetting::fade(500)), "Fade 0,5 s");
        assert_eq!(describe(TransitionSetting::fade(1000)), "Fade 1 s");
        assert_eq!(describe(TransitionSetting::fade(1500)), "Fade 1,5 s");
        assert_eq!(describe(TransitionSetting::fade(0)), "Cut");
        assert_eq!(describe(TransitionSetting::CUT), "Cut");
    }

    #[test]
    fn clock_labels() {
        assert_eq!(clock(0), "00:00");
        assert_eq!(clock(63_999), "01:03");
        assert_eq!(clock(3_725_000), "1:02:05");
    }
}
