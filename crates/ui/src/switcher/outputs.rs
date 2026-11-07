//! Outputs in the switcher (Phase 2 steps 5 and 8): the record / stream / FTB buttons and the
//! « Sorties » window (one recording, up to 3 stream destinations, one full screen). Every
//! action is a `Command`; settings are edited in a local draft and sent with « Appliquer ».
//!
//! Secrets (stream key, SRT passphrase) are typed in password fields: never shown, never in
//! a label.

use std::collections::HashMap;
use std::path::PathBuf;

use egui::{Color32, RichText, Vec2};
use vma_core::FrameRate;
use vma_core::{AudioBus, Container, OutputId, VideoCodec};
use vma_engine::{
    Command, FullscreenSpec, MAX_OUTPUTS, MAX_TRANSITION_MS, OutputSpec, OutputState, OutputStatus,
    RecordSpec, RtmpSpec, STREAM_SLOTS, Secret, SrtMode, SrtSpec, VIDEO_KBPS_RANGE,
};

use super::mixer::bus_name;
use super::{Switcher, View, WARNING};
use crate::output_stats::{duration, kind_label, status_label};
use crate::{Screen, default_screen, screen_index};

/// Fill of a button whose output is on.
const ON_AIR: Color32 = Color32::from_rgb(200, 40, 40);
/// Video bit rate offered when « Automatique » is unticked, kbit/s.
const MANUAL_KBPS: u32 = 6_000;

/// UI-side state of the outputs (never output state).
#[derive(Debug, Default)]
pub(super) struct OutputsUi {
    pub(super) open: bool,
    /// Settings being edited: the spec they started from, and the draft.
    drafts: HashMap<OutputId, (OutputSpec, OutputSpec)>,
    /// A stream destination being added.
    new_stream: Option<OutputSpec>,
    /// Folder dialog asked for a recording, not taken by the caller yet.
    folder_request: Option<OutputId>,
}

impl OutputsUi {
    /// The draft of `output`, started from `spec` (and started again if `spec` changed while
    /// the draft was untouched).
    fn draft(&mut self, output: OutputId, spec: &OutputSpec) -> &mut OutputSpec {
        if let Some((base, draft)) = self.drafts.get(&output)
            && base != spec
            && draft == base
        {
            self.drafts.remove(&output);
        }
        &mut self
            .drafts
            .entry(output)
            .or_insert_with(|| (spec.clone(), spec.clone()))
            .1
    }

    fn forget(&mut self, output: OutputId) {
        self.drafts.remove(&output);
    }
}

impl Switcher {
    /// The folder dialog the operator asked for, once: the caller opens it and gives the
    /// result back with [`Self::set_record_folder`].
    pub fn take_folder_request(&mut self) -> Option<OutputId> {
        self.outputs.folder_request.take()
    }

    /// Puts a folder chosen in the dialog into the draft of a recording.
    pub fn set_record_folder(&mut self, output: OutputId, folder: PathBuf) {
        if let Some((_, OutputSpec::Record(record))) = self.outputs.drafts.get_mut(&output) {
            record.folder = Some(folder);
        }
    }

    /// « Enregistrer », « Streamer », « Sorties » in the toolbar.
    pub(super) fn output_buttons(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        let state = view.state;
        let since = |o: &OutputState| {
            (view.io.output_activity)(o.id).map_or(0, |a| a.running_for.as_secs())
        };
        let recording = state
            .outputs
            .iter()
            .filter(|o| o.spec.is_recording() && o.status.is_on())
            .map(since)
            .max();
        let streaming = state
            .outputs
            .iter()
            .filter(|o| o.spec.is_stream() && o.status.is_on())
            .map(since)
            .max();

        let button = |label: String, on: bool| {
            let text = RichText::new(label).strong();
            if on {
                egui::Button::new(text.color(Color32::WHITE)).fill(ON_AIR)
            } else {
                egui::Button::new(text)
            }
        };
        let record = match recording {
            Some(secs) => button(format!("REC {}", duration(secs)), true),
            None => button("Enregistrer".into(), false),
        };
        let response = ui.add(record).on_hover_text(match recording {
            Some(_) => "Arrêter l'enregistrement",
            None => "Enregistrer le Program",
        });
        if response.clicked() {
            let command = match recording {
                Some(_) => Command::StopRecording,
                None => Command::StartRecording,
            };
            self.send(ui, view, command);
        }

        let has_streams = state.outputs.iter().any(|o| o.spec.is_stream());
        let stream = match streaming {
            Some(secs) => button(format!("EN DIRECT {}", duration(secs)), true),
            None => button("Streamer".into(), false),
        };
        let response = ui
            .add_enabled(has_streams, stream)
            .on_hover_text(match streaming {
                Some(_) => "Arrêter toutes les destinations",
                None => "Démarrer toutes les destinations",
            })
            .on_disabled_hover_text("Aucune destination : à ajouter dans « Sorties »");
        if response.clicked() {
            let command = match streaming {
                Some(_) => Command::StopStreaming { slot: None },
                None => Command::StartStreaming { slot: None },
            };
            self.send(ui, view, command);
        }
        let failed = state
            .outputs
            .iter()
            .any(|o| matches!(o.status, OutputStatus::Failed { .. }));
        let label = if failed {
            RichText::new("Sorties (erreur)").color(WARNING)
        } else {
            RichText::new("Sorties")
        };
        ui.toggle_value(&mut self.outputs.open, label);
    }

    /// « FTB » in the transition column: fade the outputs to black and back.
    pub(super) fn ftb_button(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        let ftb = view.state.ftb;
        let wide = Vec2::new(ui.available_width(), 30.0);
        let level = view.fade_to_black;
        let mut button = egui::Button::new(RichText::new("FTB").strong());
        if ftb.active || level > 0.0 {
            button = button.fill(ON_AIR);
        }
        let response = ui.add_sized(wide, button).on_hover_text(format!(
            "Fondu au noir des sorties ({} ms) ; la vue Program n'est pas concernée",
            ftb.duration_ms
        ));
        if response.clicked() {
            self.send(ui, view, Command::FadeToBlack);
        }
        response.context_menu(|ui| {
            ui.label("Fondu au noir");
            let mut ms = ftb.duration_ms;
            let edit = ui.add(
                egui::DragValue::new(&mut ms)
                    .range(0..=MAX_TRANSITION_MS)
                    .speed(10)
                    .suffix(" ms"),
            );
            if edit.changed() && ms != ftb.duration_ms {
                self.send(ui, view, Command::SetFadeToBlackDuration { ms });
            }
        });
        if level > 0.0 && level < 1.0 {
            ui.add(egui::ProgressBar::new(level).desired_height(4.0));
        }
    }

    /// The « Sorties » window.
    pub(super) fn outputs_window(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        let mut open = self.outputs.open;
        egui::Window::new("Sorties")
            .open(&mut open)
            .default_width(460.0)
            .resizable(true)
            .show(ui.ctx(), |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for output in &view.state.outputs {
                        ui.push_id(("output", output.id.0), |ui| {
                            self.output_card(ui, view, output);
                        });
                        ui.separator();
                    }
                    self.add_outputs(ui, view);
                });
            });
        self.outputs.open = open;
    }

    fn output_card(&mut self, ui: &mut egui::Ui, view: &View<'_>, output: &OutputState) {
        let id = output.id;
        ui.horizontal(|ui| {
            ui.label(RichText::new(&output.name).strong());
            ui.label(RichText::new(kind_label(&output.spec)).weak());
            let color = match output.status {
                OutputStatus::Failed { .. } => Color32::from_rgb(230, 70, 70),
                OutputStatus::Running { .. } => Color32::from_rgb(80, 200, 120),
                _ => ui.visuals().text_color(),
            };
            ui.label(RichText::new(status_label(&output.status)).color(color));
        });
        match &output.status {
            OutputStatus::Running { encoder, target } => {
                if !encoder.is_empty() {
                    ui.label(
                        RichText::new(crate::output_stats::short_encoder(encoder))
                            .small()
                            .monospace(),
                    )
                    .on_hover_text(encoder);
                }
                if let Some(target) = target {
                    ui.add(egui::Label::new(RichText::new(target).small()).wrap());
                }
            }
            OutputStatus::Failed { reason } | OutputStatus::Reconnecting { reason, .. } => {
                ui.add(egui::Label::new(RichText::new(reason).color(WARNING)).wrap());
            }
            _ => {}
        }

        if let OutputSpec::Fullscreen(f) = &output.spec {
            screen_notices(
                ui,
                view.screens,
                f,
                view.state.format.rate,
                output.status.is_on(),
            );
        }
        if !output.spec.is_fullscreen() {
            ui.horizontal(|ui| {
                ui.label("Son :");
                ui.add_enabled_ui(!output.status.is_active(), |ui| {
                    for bus in AudioBus::ALL {
                        if ui
                            .selectable_label(output.bus == bus, bus_name(bus))
                            .clicked()
                            && output.bus != bus
                        {
                            self.send(ui, view, Command::SetOutputBus { output: id, bus });
                        }
                    }
                });
            });
        }
        if output.status.is_active() {
            self.outputs.forget(id);
            ui.label(RichText::new(summary(&output.spec)).small().weak());
            if output.status != OutputStatus::Stopping && ui.button("Arrêter").clicked() {
                self.send(ui, view, Command::StopOutput { output: id });
            }
            return;
        }

        let record_folder = view.io.record_folder;
        let draft = self.outputs.draft(id, &output.spec);
        let browse = spec_editor(ui, draft, record_folder, view.screens);
        let changed = *draft != output.spec;
        let draft = draft.clone();
        if browse {
            self.outputs.folder_request = Some(id);
        }
        ui.horizontal(|ui| {
            if ui.button("Démarrer").clicked() {
                // Unsaved settings go first: the start uses them.
                if changed {
                    self.send(
                        ui,
                        view,
                        Command::UpdateOutput {
                            output: id,
                            spec: draft.clone(),
                        },
                    );
                }
                self.send(ui, view, Command::StartOutput { output: id });
            }
            if ui
                .add_enabled(changed, egui::Button::new("Appliquer"))
                .clicked()
            {
                self.send(
                    ui,
                    view,
                    Command::UpdateOutput {
                        output: id,
                        spec: draft.clone(),
                    },
                );
            }
            if ui
                .add_enabled(changed, egui::Button::new("Annuler"))
                .clicked()
            {
                self.outputs.forget(id);
            }
            if ui.button("Supprimer").clicked() {
                self.outputs.forget(id);
                self.send(ui, view, Command::RemoveOutput { output: id });
            }
        });
    }

    /// « + Destination de stream », « + Enregistrement ».
    fn add_outputs(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        let state = view.state;
        let full = state.outputs.len() >= MAX_OUTPUTS;
        let streams = state.outputs.iter().filter(|o| o.spec.is_stream()).count();
        if let Some(mut spec) = self.outputs.new_stream.take() {
            ui.label(RichText::new("Nouvelle destination").strong());
            spec_editor(ui, &mut spec, None, &[]);
            let mut keep = true;
            ui.horizontal(|ui| {
                if ui.button("Ajouter").clicked() {
                    self.send(
                        ui,
                        view,
                        Command::AddOutput {
                            spec: spec.clone(),
                            name: None,
                        },
                    );
                    keep = false;
                }
                if ui.button("Annuler").clicked() {
                    keep = false;
                }
            });
            if keep {
                self.outputs.new_stream = Some(spec);
            }
            return;
        }
        ui.horizontal(|ui| {
            let can_stream = !full && streams < usize::from(STREAM_SLOTS);
            if ui
                .add_enabled(can_stream, egui::Button::new("+ Destination de stream"))
                .on_disabled_hover_text(format!("{STREAM_SLOTS} destinations au plus"))
                .clicked()
            {
                self.outputs.new_stream = Some(OutputSpec::Rtmp(RtmpSpec {
                    url: String::new(),
                    key: Secret::none(),
                    video_kbps: None,
                }));
            }
            let has_fullscreen = state.outputs.iter().any(|o| o.spec.is_fullscreen());
            if !has_fullscreen
                && ui
                    .add_enabled(!full, egui::Button::new("+ Plein écran"))
                    .on_hover_text("Le Program (après FTB) en plein écran sur un écran")
                    .clicked()
            {
                self.send(
                    ui,
                    view,
                    Command::AddOutput {
                        spec: OutputSpec::Fullscreen(FullscreenSpec {
                            screen: default_screen(view.screens),
                        }),
                        name: None,
                    },
                );
            }
            let has_ndi = state.outputs.iter().any(|o| o.spec.is_ndi());
            if !has_ndi {
                let add = ui
                    .add_enabled(
                        !full && view.io.ndi_unavailable.is_none(),
                        egui::Button::new("+ NDI®"),
                    )
                    .on_hover_text("Le Program et son son, non compressés, sur le réseau");
                let add = match view.io.ndi_unavailable {
                    Some(reason) => add.on_disabled_hover_text(format!(
                        "Runtime NDI® introuvable : installer le runtime NDI (NDI Tools), \
                         puis redémarrer VMA.\n{reason}"
                    )),
                    None => add,
                };
                if add.clicked() {
                    self.send(
                        ui,
                        view,
                        Command::AddOutput {
                            spec: OutputSpec::Ndi(vma_engine::NdiSpec::default()),
                            name: None,
                        },
                    );
                }
            }
            let has_recording = state.outputs.iter().any(|o| o.spec.is_recording());
            if !has_recording
                && ui
                    .add_enabled(!full, egui::Button::new("+ Enregistrement"))
                    .clicked()
            {
                self.send(
                    ui,
                    view,
                    Command::AddOutput {
                        spec: OutputSpec::Record(RecordSpec::default()),
                        name: None,
                    },
                );
            }
        });
    }
}

/// One line describing the settings of a running output (no secret).
fn summary(spec: &OutputSpec) -> String {
    let rate = |kbps: Option<u32>| kbps.map_or("débit auto".into(), |k| format!("{k} kb/s"));
    match spec {
        OutputSpec::Record(r) => format!(
            "{} · {} · {}",
            codec_label(r.codec),
            container_label(r.container),
            rate(r.video_kbps)
        ),
        OutputSpec::Rtmp(r) => format!("{} · {}", r.url, rate(r.video_kbps)),
        OutputSpec::Srt(s) => format!(
            "{} {}:{} · {} ms · {}",
            mode_label(s.mode),
            s.host,
            s.port,
            s.latency_ms,
            rate(s.video_kbps)
        ),
        OutputSpec::Fullscreen(f) => format!(
            "Program après FTB · écran {}",
            f.screen.as_deref().unwrap_or("par défaut")
        ),
        OutputSpec::Ndi(n) => format!("Program non compressé · nom « {} »", n.name),
    }
}

/// Under a full-screen output: its screen missing, alone, or at a refresh the Program does
/// not fall evenly on.
fn screen_notices(
    ui: &mut egui::Ui,
    screens: &[Screen],
    spec: &FullscreenSpec,
    rate: FrameRate,
    on: bool,
) {
    let warn = |ui: &mut egui::Ui, text: String| {
        ui.add(egui::Label::new(RichText::new(text).color(WARNING).small()).wrap());
    };
    if screens.is_empty() {
        return;
    }
    let Some(screen) = screen_index(screens, spec.screen.as_deref()).and_then(|i| screens.get(i))
    else {
        let name = spec.screen.as_deref().unwrap_or_default();
        warn(
            ui,
            if on {
                format!("Écran « {name} » non branché : la sortie n'est pas affichée")
            } else {
                format!("Écran « {name} » non branché")
            },
        );
        return;
    };
    if screens.len() == 1 {
        warn(
            ui,
            "Un seul écran : la sortie couvrira cette fenêtre".into(),
        );
    }
    if screen.judders_at(rate) {
        warn(
            ui,
            format!(
                "{} : le Program à {rate} y tombe inégalement (saccades) ; régler l'écran sur un multiple de la cadence",
                screen.label()
            ),
        );
    }
}

fn codec_label(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "H.264",
        VideoCodec::Hevc => "HEVC",
    }
}

fn container_label(container: Container) -> &'static str {
    match container {
        Container::Mkv => "MKV",
        Container::Mp4 => "MP4",
    }
}

fn mode_label(mode: SrtMode) -> &'static str {
    match mode {
        SrtMode::Caller => "appelant",
        SrtMode::Listener => "écoute",
    }
}

/// Edits `spec` in place; `true` if the folder dialog was asked for.
fn spec_editor(
    ui: &mut egui::Ui,
    spec: &mut OutputSpec,
    record_folder: Option<&std::path::Path>,
    screens: &[Screen],
) -> bool {
    let mut browse = false;
    egui::Grid::new("spec")
        .num_columns(2)
        .spacing([8.0, 4.0])
        .show(ui, |ui| match spec {
            OutputSpec::Ndi(ndi) => {
                let label = ui.label("Nom NDI®");
                ui.add(
                    egui::TextEdit::singleline(&mut ndi.name)
                        .hint_text("VMA Program")
                        .desired_width(220.0),
                )
                .labelled_by(label.id);
                ui.end_row();
            }
            OutputSpec::Fullscreen(fullscreen) => {
                let label = ui.label("Écran");
                let shown = match screen_index(screens, fullscreen.screen.as_deref()) {
                    Some(i) => screens.get(i).map(Screen::label).unwrap_or_default(),
                    None => match &fullscreen.screen {
                        Some(name) => format!("{name} (non branché)"),
                        None => "premier écran".into(),
                    },
                };
                egui::ComboBox::from_id_salt("screen")
                    .selected_text(shown)
                    .width(260.0)
                    .show_ui(ui, |ui| {
                        for screen in screens {
                            let chosen = fullscreen.screen.as_deref() == Some(&screen.name);
                            if ui.selectable_label(chosen, screen.label()).clicked() {
                                fullscreen.screen = Some(screen.name.clone());
                            }
                        }
                    })
                    .response
                    .labelled_by(label.id);
                ui.end_row();
            }
            OutputSpec::Record(record) => {
                let label = ui.label("Dossier");
                ui.horizontal(|ui| {
                    let mut text = record
                        .folder
                        .as_ref()
                        .map(|f| f.display().to_string())
                        .unwrap_or_default();
                    let hint = record_folder.map_or("Vidéos".to_owned(), |f| {
                        format!("Vidéos ({})", f.display())
                    });
                    let edit = ui
                        .add(
                            egui::TextEdit::singleline(&mut text)
                                .hint_text(hint)
                                .desired_width(240.0),
                        )
                        .labelled_by(label.id);
                    if edit.changed() {
                        record.folder = (!text.trim().is_empty()).then(|| PathBuf::from(text));
                    }
                    browse = ui.button("Parcourir…").clicked();
                });
                ui.end_row();
                ui.label("Codec");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut record.codec, VideoCodec::H264, "H.264");
                    ui.radio_value(&mut record.codec, VideoCodec::Hevc, "HEVC")
                        .on_hover_text("Encodeur matériel requis");
                });
                ui.end_row();
                ui.label("Conteneur");
                ui.horizontal(|ui| {
                    ui.radio_value(&mut record.container, Container::Mkv, "MKV")
                        .on_hover_text("Reste lisible après une coupure");
                    ui.radio_value(&mut record.container, Container::Mp4, "MP4");
                });
                ui.end_row();
                bitrate(ui, &mut record.video_kbps);
            }
            OutputSpec::Rtmp(_) | OutputSpec::Srt(_) => {
                ui.label("Protocole");
                ui.horizontal(|ui| {
                    let rtmp = matches!(spec, OutputSpec::Rtmp(_));
                    if ui.radio(rtmp, "RTMP").clicked() && !rtmp {
                        *spec = OutputSpec::Rtmp(RtmpSpec {
                            url: String::new(),
                            key: Secret::none(),
                            video_kbps: kbps_of(spec),
                        });
                    }
                    if ui.radio(!rtmp, "SRT").clicked() && rtmp {
                        *spec = OutputSpec::Srt(SrtSpec {
                            video_kbps: kbps_of(spec),
                            ..SrtSpec::default()
                        });
                    }
                });
                ui.end_row();
                match spec {
                    OutputSpec::Rtmp(rtmp) => {
                        let label = ui.label("Adresse");
                        ui.add(
                            egui::TextEdit::singleline(&mut rtmp.url)
                                .hint_text("rtmp://serveur/app")
                                .desired_width(260.0),
                        )
                        .labelled_by(label.id);
                        ui.end_row();
                        secret(ui, "Clé de stream", &mut rtmp.key);
                        bitrate(ui, &mut rtmp.video_kbps);
                    }
                    OutputSpec::Srt(srt) => {
                        ui.label("Mode");
                        ui.horizontal(|ui| {
                            ui.radio_value(&mut srt.mode, SrtMode::Caller, "Appelant");
                            ui.radio_value(&mut srt.mode, SrtMode::Listener, "Écoute");
                        });
                        ui.end_row();
                        let label = ui.label("Hôte");
                        ui.add(
                            egui::TextEdit::singleline(&mut srt.host)
                                .hint_text(match srt.mode {
                                    SrtMode::Caller => "adresse du récepteur",
                                    SrtMode::Listener => "toutes les interfaces",
                                })
                                .desired_width(200.0),
                        )
                        .labelled_by(label.id);
                        ui.end_row();
                        ui.label("Port");
                        ui.add(egui::DragValue::new(&mut srt.port).range(1..=u16::MAX));
                        ui.end_row();
                        ui.label("Latence");
                        ui.add(
                            egui::DragValue::new(&mut srt.latency_ms)
                                .range(vma_engine::SRT_LATENCY_RANGE)
                                .suffix(" ms"),
                        );
                        ui.end_row();
                        secret(ui, "Phrase de passe", &mut srt.passphrase);
                        bitrate(ui, &mut srt.video_kbps);
                    }
                    OutputSpec::Record(_) | OutputSpec::Fullscreen(_) | OutputSpec::Ndi(_) => {}
                }
            }
        });
    browse
}

fn kbps_of(spec: &OutputSpec) -> Option<u32> {
    match spec {
        OutputSpec::Record(r) => r.video_kbps,
        OutputSpec::Rtmp(r) => r.video_kbps,
        OutputSpec::Srt(s) => s.video_kbps,
        OutputSpec::Fullscreen(_) | OutputSpec::Ndi(_) => None,
    }
}

/// « Débit vidéo »: automatic, or a value in kbit/s.
fn bitrate(ui: &mut egui::Ui, kbps: &mut Option<u32>) {
    ui.label("Débit vidéo");
    ui.horizontal(|ui| {
        let mut auto = kbps.is_none();
        if ui.checkbox(&mut auto, "Automatique").changed() {
            *kbps = if auto { None } else { Some(MANUAL_KBPS) };
        }
        if let Some(value) = kbps {
            ui.add(
                egui::DragValue::new(value)
                    .range(VIDEO_KBPS_RANGE)
                    .speed(50)
                    .suffix(" kb/s"),
            );
        }
    });
    ui.end_row();
}

/// A password field for a secret; a redacted one (read back from a journal) is empty, to be
/// typed again.
pub(super) fn secret(ui: &mut egui::Ui, label: &str, secret: &mut Secret) {
    let label = ui.label(label);
    let mut text = secret.expose().unwrap_or_default().to_owned();
    let hint = if secret.expose().is_none() {
        "à saisir de nouveau"
    } else {
        "aucune"
    };
    let edit = ui
        .add(
            egui::TextEdit::singleline(&mut text)
                .password(true)
                .hint_text(hint)
                .desired_width(200.0),
        )
        .labelled_by(label.id);
    if edit.changed() {
        *secret = Secret::new(text);
    }
    ui.end_row();
}
