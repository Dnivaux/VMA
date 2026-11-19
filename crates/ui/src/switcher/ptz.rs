//! PTZ cameras (Phase 7A, ADR 0018), in the settings of an input as the reference has them:
//! the camera's address, then its controls — pad held down to move, speed, zoom and focus held
//! down, home, 16 presets (recall, or save the position), virtual inputs. Under the Preview,
//! the confirmation of a recall held back because the camera is on air (Q5).

use egui::{Color32, RichText};
use vma_core::InputId;
use vma_engine::{
    CameraAddress, Command, Direction, FocusMode, InputSpec, InputState, LensMove, MoveKind,
    PRESETS, Preset, PtzAction, PtzLink, Speed, Transport, may_be_ptz,
};

use super::icons::{self, Icon};
use super::{Switcher, View, WARNING};

const REACHABLE: Color32 = Color32::from_rgb(90, 190, 110);

/// What is being typed and held in the PTZ section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PtzUi {
    /// The input the fields were filled for.
    input: Option<InputId>,
    transport: Transport,
    host: String,
    port: String,
    speed: Speed,
    /// The move held down, sent when pressed; its stop is sent when released.
    held: Option<(InputId, PtzAction)>,
    /// The next preset clicked saves the position instead of recalling it.
    saving: bool,
    /// A recall clicked while the camera is on air, waiting for « Déplacer quand même ».
    confirm: Option<(InputId, Preset)>,
}

impl Default for PtzUi {
    fn default() -> Self {
        Self {
            input: None,
            transport: Transport::Udp,
            host: String::new(),
            port: Transport::Udp.default_port().to_string(),
            speed: Speed::DEFAULT,
            held: None,
            saving: false,
            confirm: None,
        }
    }
}

fn transport_name(transport: Transport) -> &'static str {
    match transport {
        Transport::Udp => "VISCA over IP (UDP)",
        Transport::Tcp => "VISCA (TCP)",
    }
}

/// The stop of a held move.
fn stop_of(action: PtzAction) -> Option<PtzAction> {
    match action {
        PtzAction::Move { speed, .. } => Some(PtzAction::Move {
            direction: Direction::Stop,
            speed,
        }),
        PtzAction::Zoom(_) => Some(PtzAction::Zoom(LensMove::Stop)),
        PtzAction::Focus(_) => Some(PtzAction::Focus(LensMove::Stop)),
        _ => None,
    }
}

fn kind_of(action: PtzAction) -> Option<MoveKind> {
    action.visca().move_kind()
}

impl Switcher {
    /// The « Caméra PTZ » section of the settings of `input` (a camera, a stream, NDI®, or a
    /// virtual input).
    pub(super) fn ptz_settings(&mut self, ui: &mut egui::Ui, view: &View<'_>, input: &InputState) {
        let state = view.state;
        let is_view = matches!(input.spec, InputSpec::PtzView { .. });
        if !is_view && !may_be_ptz(&input.spec) {
            return;
        }
        if self.ptz.input != Some(input.id) {
            self.ptz_release(ui, view);
            let address = input.ptz.as_ref().map(|p| &p.settings.address);
            self.ptz = PtzUi {
                input: Some(input.id),
                transport: address.map_or(Transport::Udp, |a| a.transport),
                host: address.map(|a| a.host.clone()).unwrap_or_default(),
                port: address
                    .map_or(Transport::Udp.default_port(), |a| a.port)
                    .to_string(),
                speed: self.ptz.speed,
                ..PtzUi::default()
            };
        }
        ui.separator();
        ui.label(RichText::new("Caméra PTZ").strong());
        if is_view {
            self.ptz_view_header(ui, view, input);
        } else {
            self.ptz_address(ui, view, input);
        }
        let Some(camera) = state.ptz_camera_of(input.id) else {
            return;
        };
        if let Some(ptz) = state.input(camera).and_then(|c| c.ptz.as_ref()) {
            match &ptz.link {
                PtzLink::Connecting => {
                    ui.label("Connexion…");
                }
                PtzLink::Reachable => {
                    ui.label(RichText::new("Connectée").color(REACHABLE));
                }
                PtzLink::Unreachable { reason } => {
                    ui.label(RichText::new(format!("Injoignable : {reason}")).color(WARNING));
                }
            }
            if let Some(problem) = &ptz.problem {
                ui.label(RichText::new(problem).small().color(WARNING));
            }
        }
        self.ptz_controls(ui, view, input.id, camera);
    }

    /// A virtual input: its camera, its preset, and « Mettre à jour la position ».
    fn ptz_view_header(&mut self, ui: &mut egui::Ui, view: &View<'_>, input: &InputState) {
        let InputSpec::PtzView { camera, preset } = input.spec else {
            return;
        };
        let name = view
            .state
            .inputs
            .iter()
            .find(|i| i.key == camera)
            .map_or_else(|| "(absente)".to_owned(), |c| c.name.clone());
        ui.label(format!("Input virtuel de « {name} », préréglage {preset}"));
        if ui
            .button("Mettre à jour la position")
            .on_hover_text("La position actuelle de la caméra devient celle de cet input")
            .clicked()
        {
            self.send(
                ui,
                view,
                Command::Ptz {
                    input: input.id,
                    action: PtzAction::Save(preset),
                },
            );
        }
    }

    /// Protocol, address, port; « Connecter », « Appliquer », « Retirer ».
    fn ptz_address(&mut self, ui: &mut egui::Ui, view: &View<'_>, input: &InputState) {
        let current = input.ptz.as_ref().map(|p| &p.settings.address);
        ui.horizontal(|ui| {
            let label = ui.label("Protocole");
            let before = self.ptz.transport;
            egui::ComboBox::from_id_salt("vma.ptz.transport")
                .selected_text(transport_name(self.ptz.transport))
                .show_ui(ui, |ui| {
                    for transport in [Transport::Udp, Transport::Tcp] {
                        ui.selectable_value(
                            &mut self.ptz.transport,
                            transport,
                            transport_name(transport),
                        );
                    }
                })
                .response
                .labelled_by(label.id);
            // The usual port of the protocol, unless one was typed.
            if self.ptz.transport != before && self.ptz.port == before.default_port().to_string() {
                self.ptz.port = self.ptz.transport.default_port().to_string();
            }
        });
        ui.horizontal(|ui| {
            let label = ui.label("Adresse de la caméra");
            ui.add(
                egui::TextEdit::singleline(&mut self.ptz.host)
                    .hint_text("192.168.1.90")
                    .desired_width(160.0),
            )
            .labelled_by(label.id);
            let label = ui.label("Port");
            ui.add(egui::TextEdit::singleline(&mut self.ptz.port).desired_width(60.0))
                .labelled_by(label.id);
        });
        let typed = self
            .ptz
            .port
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0 && !self.ptz.host.trim().is_empty())
            .map(|port| CameraAddress {
                transport: self.ptz.transport,
                host: self.ptz.host.trim().to_owned(),
                port,
            });
        ui.horizontal(|ui| {
            let label = if current.is_some() {
                "Appliquer"
            } else {
                "Connecter"
            };
            let changed = typed.is_some() && typed.as_ref() != current;
            if ui.add_enabled(changed, egui::Button::new(label)).clicked() {
                self.send(
                    ui,
                    view,
                    Command::SetPtzCamera {
                        input: input.id,
                        address: typed.clone(),
                    },
                );
            }
            if current.is_some()
                && ui
                    .button("Retirer la PTZ")
                    .on_hover_text("Ses inputs virtuels sont supprimés avec elle")
                    .clicked()
            {
                self.send(
                    ui,
                    view,
                    Command::SetPtzCamera {
                        input: input.id,
                        address: None,
                    },
                );
            }
        });
    }

    /// The controls of `camera`, sent through `target` (the camera or one of its views).
    fn ptz_controls(
        &mut self,
        ui: &mut egui::Ui,
        view: &View<'_>,
        target: InputId,
        camera: InputId,
    ) {
        let speed = self.ptz.speed;
        let mut pressed: Option<PtzAction> = None;
        let mut clicked: Option<PtzAction> = None;
        let mut hold = |ui: &mut egui::Ui, icon: Icon, name: &str, action: PtzAction| {
            let response = icons::named(ui.add(icons::only(icon)), name, None);
            if response.is_pointer_button_down_on() {
                pressed = Some(action);
            }
        };
        let pan = |direction| PtzAction::Move { direction, speed };
        ui.horizontal(|ui| {
            egui::Grid::new("vma.ptz.pad").show(ui, |ui| {
                hold(ui, Icon::ArrowUpLeft, "Haut gauche", pan(Direction::UpLeft));
                hold(ui, Icon::ArrowUp, "Haut", pan(Direction::Up));
                hold(
                    ui,
                    Icon::ArrowUpRight,
                    "Haut droite",
                    pan(Direction::UpRight),
                );
                ui.end_row();
                hold(ui, Icon::ArrowLeft, "Gauche", pan(Direction::Left));
                let home = icons::named(ui.add(icons::only(Icon::House)), "Origine", None);
                if home.clicked() {
                    clicked = Some(PtzAction::Home);
                }
                hold(ui, Icon::ArrowRight, "Droite", pan(Direction::Right));
                ui.end_row();
                hold(
                    ui,
                    Icon::ArrowDownLeft,
                    "Bas gauche",
                    pan(Direction::DownLeft),
                );
                hold(ui, Icon::ArrowDown, "Bas", pan(Direction::Down));
                hold(
                    ui,
                    Icon::ArrowDownRight,
                    "Bas droite",
                    pan(Direction::DownRight),
                );
                ui.end_row();
            });
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label("Zoom");
                    hold(
                        ui,
                        Icon::ZoomIn,
                        "Zoom avant",
                        PtzAction::Zoom(LensMove::In(speed)),
                    );
                    hold(
                        ui,
                        Icon::ZoomOut,
                        "Zoom arrière",
                        PtzAction::Zoom(LensMove::Out(speed)),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Mise au point");
                    hold(
                        ui,
                        Icon::Plus,
                        "Plus loin",
                        PtzAction::Focus(LensMove::In(speed)),
                    );
                    hold(
                        ui,
                        Icon::Minus,
                        "Plus près",
                        PtzAction::Focus(LensMove::Out(speed)),
                    );
                });
                ui.horizontal(|ui| {
                    if ui
                        .button("Auto")
                        .on_hover_text("Mise au point automatique")
                        .clicked()
                    {
                        clicked = Some(PtzAction::FocusMode(FocusMode::Auto));
                    }
                    if ui.button("Manuelle").clicked() {
                        clicked = Some(PtzAction::FocusMode(FocusMode::Manual));
                    }
                    if ui
                        .add(icons::button(Icon::Focus, "Une fois"))
                        .on_hover_text("Mise au point unique (en manuel)")
                        .clicked()
                    {
                        clicked = Some(PtzAction::FocusOnce);
                    }
                });
            });
        });
        let mut percent = self.ptz.speed.get();
        ui.horizontal(|ui| {
            let label = ui.label("Vitesse");
            ui.add(egui::Slider::new(&mut percent, 1..=100))
                .labelled_by(label.id);
        });
        if let Some(speed) = Speed::new(percent) {
            self.ptz.speed = speed;
        }
        if let Some(action) = clicked {
            self.send(
                ui,
                view,
                Command::Ptz {
                    input: target,
                    action,
                },
            );
        }
        self.ptz_hold(ui, view, target, pressed);
        self.ptz_presets(ui, view, target, camera);
    }

    /// Sends the move pressed this frame, and the stop of the one released.
    fn ptz_hold(
        &mut self,
        ui: &egui::Ui,
        view: &View<'_>,
        target: InputId,
        pressed: Option<PtzAction>,
    ) {
        let now = pressed.map(|action| (target, action));
        if now == self.ptz.held {
            return;
        }
        if let Some((input, before)) = self.ptz.held.take()
            && let Some(stop) = stop_of(before)
            // A move of the same kind replaces it without a stop in between.
            && now.is_none_or(|(_, action)| kind_of(action) != kind_of(before))
        {
            self.send(
                ui,
                view,
                Command::Ptz {
                    input,
                    action: stop,
                },
            );
        }
        if let Some((input, action)) = now {
            self.send(ui, view, Command::Ptz { input, action });
        }
        self.ptz.held = now;
    }

    /// A move still held when its controls disappear is stopped.
    pub(super) fn ptz_release(&mut self, ui: &egui::Ui, view: &View<'_>) {
        if let Some((input, action)) = self.ptz.held.take()
            && let Some(stop) = stop_of(action)
        {
            self.send(
                ui,
                view,
                Command::Ptz {
                    input,
                    action: stop,
                },
            );
        }
    }

    /// The 16 presets: a click recalls (confirmed when the camera is on air), or saves while
    /// « Enregistrer la position » is on; « Nouvel input virtuel ».
    fn ptz_presets(
        &mut self,
        ui: &mut egui::Ui,
        view: &View<'_>,
        target: InputId,
        camera: InputId,
    ) {
        let state = view.state;
        let saved = state
            .input(camera)
            .and_then(|c| c.ptz.as_ref())
            .map(|p| p.settings.saved.clone())
            .unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label("Préréglages");
            icons::toggle(
                ui,
                &mut self.ptz.saving,
                Icon::Save,
                "Enregistrer la position",
            )
            .on_hover_text("Le prochain préréglage cliqué prend la position actuelle");
        });
        let mut chosen = None;
        egui::Grid::new("vma.ptz.presets").show(ui, |ui| {
            for preset in Preset::all() {
                let text = RichText::new(preset.to_string());
                let text = if saved.contains(&preset) {
                    text.strong()
                } else {
                    text.weak()
                };
                let button = ui
                    .add(egui::Button::new(text).min_size(egui::vec2(28.0, 0.0)))
                    .on_hover_text(format!("Préréglage {preset}"));
                let button = icons::named(button, &format!("Préréglage {preset}"), None);
                if button.clicked() {
                    chosen = Some(preset);
                }
                if preset.number() % 4 == 0 {
                    ui.end_row();
                }
            }
        });
        if let Some(preset) = chosen {
            if self.ptz.saving {
                self.ptz.saving = false;
                self.send(
                    ui,
                    view,
                    Command::Ptz {
                        input: target,
                        action: PtzAction::Save(preset),
                    },
                );
            } else if state.ptz_on_air(camera) {
                self.ptz.confirm = Some((target, preset));
            } else {
                self.ptz_recall(ui, view, target, preset, false);
            }
        }
        if let Some((input, preset)) = self.ptz.confirm {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(format!(
                        "Caméra à l'antenne : aller au préréglage {preset} ?"
                    ))
                    .color(WARNING),
                );
                if ui.button("Déplacer quand même").clicked() {
                    self.ptz.confirm = None;
                    self.ptz_recall(ui, view, input, preset, true);
                }
                if ui.button("Annuler").clicked() {
                    self.ptz.confirm = None;
                }
            });
        }
        // A new virtual input on the first preset no virtual input of this camera uses.
        let used: Vec<Preset> = state
            .ptz_views(camera)
            .into_iter()
            .filter_map(|v| match state.input(v)?.spec {
                InputSpec::PtzView { preset, .. } => Some(preset),
                _ => None,
            })
            .collect();
        let free = Preset::all().find(|p| !used.contains(p));
        let add = ui
            .add_enabled(
                free.is_some(),
                icons::button(Icon::Plus, "Nouvel input virtuel"),
            )
            .on_hover_text(format!(
                "Enregistre la position actuelle dans un préréglage libre (sur {PRESETS}) et \
                 crée un input qui y envoie la caméra quand il passe en Preview"
            ))
            .on_disabled_hover_text("Les 16 préréglages ont chacun leur input virtuel");
        if add.clicked()
            && let Some(preset) = free
            && let Some(camera_input) = state.input(camera)
        {
            self.send(
                ui,
                view,
                Command::Ptz {
                    input: camera,
                    action: PtzAction::Save(preset),
                },
            );
            self.send(
                ui,
                view,
                Command::AddInput {
                    spec: InputSpec::PtzView {
                        camera: camera_input.key,
                        preset,
                    },
                    name: Some(format!("{} {preset}", camera_input.name)),
                },
            );
        }
    }

    fn ptz_recall(
        &mut self,
        ui: &egui::Ui,
        view: &View<'_>,
        input: InputId,
        preset: Preset,
        confirmed: bool,
    ) {
        self.send(
            ui,
            view,
            Command::Ptz {
                input,
                action: PtzAction::Recall { preset, confirmed },
            },
        );
    }

    /// Under the Preview: a virtual input there whose camera is on air (ADR 0018 § 7).
    pub(super) fn ptz_held_row(&mut self, ui: &mut egui::Ui, view: &View<'_>, held: InputId) {
        let Some(InputSpec::PtzView { preset, .. }) = view.state.input(held).map(|i| &i.spec)
        else {
            return;
        };
        let preset = *preset;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Caméra à l'antenne").color(WARNING))
                .on_hover_text(format!(
                    "Le préréglage {preset} n'a pas été rappelé : la caméra montrerait son \
                     mouvement à l'antenne"
                ));
            if ui
                .add(icons::button(Icon::Move, "Déplacer la caméra"))
                .clicked()
            {
                self.ptz_recall(ui, view, held, preset, true);
            }
        });
    }
}

/// « PTZ » with the state of its link, on the card of a PTZ camera (G2).
pub(super) fn link_badge(ui: &mut egui::Ui, input: &InputState) {
    let Some(ptz) = &input.ptz else {
        return;
    };
    let (color, hover) = match &ptz.link {
        PtzLink::Connecting => (
            ui.visuals().weak_text_color(),
            "PTZ : connexion…".to_owned(),
        ),
        PtzLink::Reachable => (REACHABLE, "PTZ : connectée".to_owned()),
        PtzLink::Unreachable { reason } => (WARNING, format!("PTZ injoignable : {reason}")),
    };
    ui.label(RichText::new("PTZ").small().color(color))
        .on_hover_text(hover);
}
