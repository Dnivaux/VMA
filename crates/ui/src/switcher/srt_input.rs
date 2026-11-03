//! « Ajouter → SRT… » (Phase 3 step 6): the settings of a new SRT input, as for an SRT
//! output (mode, host, port, latency, passphrase in a password field), then `AddInput`.

use egui::RichText;
use vma_engine::{Command, InputSpec, SRT_LATENCY_RANGE, SrtMode};

use super::outputs::secret;
use super::{Switcher, View};

impl Switcher {
    /// The « Nouvel input SRT » window, while one is being added.
    pub(super) fn srt_input_window(&mut self, ui: &egui::Ui, view: &View<'_>) {
        let Some(mut srt) = self.new_srt.take() else {
            return;
        };
        let mut open = true;
        let mut done = false;
        egui::Window::new("Nouvel input SRT")
            .open(&mut open)
            .resizable(false)
            .show(ui.ctx(), |ui| {
                egui::Grid::new("srt-input").num_columns(2).show(ui, |ui| {
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
                                SrtMode::Caller => "adresse de l'émetteur",
                                SrtMode::Listener => "toutes les interfaces",
                            })
                            .desired_width(200.0),
                    )
                    .labelled_by(label.id);
                    ui.end_row();
                    let label = ui.label("Port");
                    ui.add(egui::DragValue::new(&mut srt.port).range(1..=u16::MAX))
                        .labelled_by(label.id);
                    ui.end_row();
                    let label = ui.label("Latence");
                    ui.add(
                        egui::DragValue::new(&mut srt.latency_ms)
                            .range(SRT_LATENCY_RANGE)
                            .suffix(" ms"),
                    )
                    .labelled_by(label.id);
                    ui.end_row();
                    secret(ui, "Phrase de passe", &mut srt.passphrase);
                });
                if srt.mode == SrtMode::Listener {
                    ui.label(
                        RichText::new("En attente d'un appelant tant qu'aucun ne se connecte")
                            .small()
                            .weak(),
                    );
                }
                ui.horizontal(|ui| {
                    if ui.button("Ajouter").clicked() {
                        let spec = InputSpec::Srt(srt.clone());
                        self.send(ui, view, Command::AddInput { spec, name: None });
                        done = true;
                    }
                    if ui.button("Annuler").clicked() {
                        done = true;
                    }
                });
            });
        if open && !done {
            self.new_srt = Some(srt);
        }
    }
}
