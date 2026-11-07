//! « Ajouter → NDI®… » (Phase 3 step 7): a source seen on the network, or a name typed in
//! (`MACHINE (Source)`), then `AddInput`. NDI® is a registered trademark of Vizrt NDI AB.

use egui::RichText;
use vma_engine::{Command, InputSpec};

use super::{Switcher, View};

impl Switcher {
    /// The « Nouvel input NDI® » window, while one is being added.
    pub(super) fn ndi_input_window(&mut self, ui: &egui::Ui, view: &View<'_>) {
        let Some(mut name) = self.new_ndi.take() else {
            return;
        };
        let mut open = true;
        let mut done = false;
        egui::Window::new("Nouvel input NDI®")
            .open(&mut open)
            .resizable(false)
            .show(ui.ctx(), |ui| {
                ui.label("Sources sur le réseau");
                if view.io.ndi_sources.is_empty() {
                    ui.label(RichText::new("Aucune source détectée pour l'instant").weak());
                }
                for source in view.io.ndi_sources {
                    ui.radio_value(&mut name, source.clone(), source);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    let label = ui.label("Nom");
                    ui.add(
                        egui::TextEdit::singleline(&mut name)
                            .hint_text("MACHINE (Source)")
                            .desired_width(220.0),
                    )
                    .labelled_by(label.id);
                });
                ui.horizontal(|ui| {
                    let add = ui.add_enabled(!name.trim().is_empty(), egui::Button::new("Ajouter"));
                    if add.clicked() {
                        let spec = InputSpec::Ndi {
                            name: name.trim().to_owned(),
                        };
                        self.send(ui, view, Command::AddInput { spec, name: None });
                        done = true;
                    }
                    if ui.button("Annuler").clicked() {
                        done = true;
                    }
                });
            });
        if open && !done {
            self.new_ndi = Some(name);
        }
    }
}
