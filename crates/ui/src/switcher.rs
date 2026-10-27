//! The mixer surface: Preview, transition column, Program, input bar, shortcuts and the
//! settings windows (spec `ui.md`).
//!
//! It never changes the state: every action sends a `Command`, and every frame redraws from
//! the latest snapshot. It holds no GPU object either (only egui texture ids), so tests run
//! it headless with a probe instead of an engine.

use egui::{Color32, Key, Rect, RichText, Sense, Stroke, StrokeKind, TextureId, Vec2};
use vma_core::{InputId, Rgb8};
use vma_engine::{
    AutoPlay, Command, CommandId, CommandSender, DeviceId, Effect, InputSpec, InputState,
    InputStatus, MAX_NAME_CHARS, MAX_TRANSITION_MS, MixDomain, Playback, RejectionNotice,
    SendError, Source, State, Timeline, TransitionSetting, TransitionState,
};

use crate::tally::{TALLY_PREVIEW, TALLY_PROGRAM, Tally};

/// Width of the transition column.
const COLUMN_WIDTH: f32 = 150.0;
/// Size of an input card's thumbnail.
const THUMB: Vec2 = Vec2::new(160.0, 90.0);
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
}

impl IoView<'static> {
    /// No device, no timeline, nothing unavailable.
    pub fn empty() -> Self {
        fn no_timeline(_: InputId) -> Option<Timeline> {
            None
        }
        fn no_mode(_: InputId) -> Option<String> {
            None
        }
        Self {
            unavailable: None,
            devices: &[],
            timeline: &no_timeline,
            capture_mode: &no_mode,
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
    /// Last frame index processed by the render thread (transition progress).
    pub frame: u64,
    /// Last command refused by the engine.
    pub rejection: Option<&'a RejectionNotice>,
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

        self.shortcuts(ui, view);

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Réglages").clicked() {
                    self.settings_open = true;
                }
                ui.toggle_value(&mut self.show_stats, "Statistiques");
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
            });
        });
        egui::Panel::bottom("inputs")
            .resizable(false)
            .exact_size(THUMB.y + 96.0)
            .show(ui, |ui| self.input_bar(ui, view));
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
                ui.button("Pause")
                    .clicked()
                    .then_some(Command::Pause { input: id })
            } else {
                ui.button("Lecture")
                    .clicked()
                    .then_some(Command::Play { input: id })
            };
            if let Some(command) = command {
                self.send(ui, view, command);
            }
            if ui
                .button("Début")
                .on_hover_text("Revenir au début")
                .clicked()
            {
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
                .add_sized(wide, egui::Button::new("Quick Play"))
                .on_hover_text(format!("Preview → Program, {}", describe(state.quick_play)))
                .clicked()
            {
                self.active_button = None;
                self.send(ui, view, Command::QuickPlay { input: None });
            }
            if ui.add_sized(wide, egui::Button::new("Cut")).clicked() {
                self.send(ui, view, Command::Cut);
            }
            ui.add_space(6.0);
            for (index, setting) in state.buttons.iter().enumerate() {
                let button = index as u8 + 1;
                let label = format!("{button} · {}", describe(*setting));
                let response = ui.add_sized(wide, egui::Button::new(label));
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
            self.t_bar(ui, view);
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
            ui.radio_value(&mut effect, Effect::Fade, "Fade");
            ui.radio_value(&mut effect, Effect::Cut, "Cut");
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
        let height = ui.available_height().clamp(80.0, 240.0);
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
        ui.horizontal(|ui| {
            ui.label(RichText::new("Inputs").strong());
            ui.menu_button("+ Ajouter", |ui| self.add_menu(ui, view));
        });
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.horizontal(|ui| {
                for (index, input) in view.state.inputs.iter().enumerate() {
                    self.input_card(ui, view, index + 1, input);
                }
            });
        });
    }

    fn add_menu(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        ui.menu_button("Couleur", |ui| {
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
        if ui.button("Mire").clicked() {
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
            ui.menu_button("Caméra / capture", |ui| {
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
            });
            if ui.button("Vidéo…").clicked() {
                self.file_request = Some(FileKind::Video);
                ui.close();
            }
        });
        if unavailable.is_some() {
            ui.label(
                RichText::new("GStreamer introuvable :\ncaméras et vidéos indisponibles")
                    .small()
                    .color(WARNING),
            );
        }
        if ui.button("Image…").clicked() {
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
        frame.show(ui, |ui| {
            ui.set_width(THUMB.x);
            ui.vertical(|ui| {
                let (rect, thumb) = ui.allocate_exact_size(THUMB, Sense::click());
                match (view.images.thumbnail_uv)(id) {
                    Some(uv) => egui::Image::from_texture((view.images.thumbnails, THUMB))
                        .uv(uv)
                        .paint_at(ui, rect),
                    // No picture yet (or not a GPU-backed input yet): black.
                    None => {
                        ui.painter().rect_filled(rect, 0.0, Color32::BLACK);
                    }
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

                let title = ui.add(
                    egui::Label::new(RichText::new(format!("{number} {}", input.name)).strong())
                        .truncate()
                        .sense(Sense::click()),
                );
                if title.double_clicked() {
                    self.input_settings = Some((id, input.name.clone()));
                }

                ui.horizontal(|ui| {
                    if ui
                        .small_button("×")
                        .on_hover_text("Fermer l'input")
                        .clicked()
                    {
                        self.send(ui, view, Command::RemoveInput { input: id });
                    }
                    if ui
                        .small_button("QP")
                        .on_hover_text("Quick Play : cet input au Program")
                        .clicked()
                    {
                        self.send(ui, view, Command::QuickPlay { input: Some(id) });
                    }
                    if ui
                        .small_button("Cut")
                        .on_hover_text("Cet input au Program, sans transition")
                        .clicked()
                    {
                        self.send(ui, view, Command::CutDirect { input: id });
                    }
                    if let Some(playback) = input.playback {
                        let mut looping = playback.looping;
                        if ui.toggle_value(&mut looping, "Loop").changed() {
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
                    ui.radio_value(&mut effect, Effect::Fade, "Fade");
                    ui.radio_value(&mut effect, Effect::Cut, "Cut");
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
        InputStatus::Live { source: Some(f) } => Some(format!(
            "{} {}{}",
            f.resolution,
            f.pixel,
            f.rate.map(|r| format!(" {r}")).unwrap_or_default()
        )),
        _ => None,
    };
    match (capture, delivered) {
        (Some(c), Some(d)) => Some(format!("Mode {c} → {d}")),
        (Some(c), None) => Some(format!("Mode {c}")),
        (None, d) => d,
    }
}

/// « Fade 0,5 s », « Cut ».
fn describe(setting: TransitionSetting) -> String {
    match setting.effect {
        Effect::Cut => "Cut".to_owned(),
        Effect::Fade if setting.duration_ms == 0 => "Cut".to_owned(),
        Effect::Fade => {
            let secs = f64::from(setting.duration_ms) / 1000.0;
            format!("Fade {}", format!("{secs} s").replace('.', ","))
        }
    }
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
