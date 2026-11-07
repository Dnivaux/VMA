//! « À propos »: version, licences of what VMA is built with, trademark notices (ADR 0008
//! § 4).

use egui::RichText;

use super::Switcher;

/// Main components and their licences (the complete list: `cargo deny list`).
const LICENCES: &[(&str, &str)] = &[
    ("wgpu, egui, eframe", "MIT ou Apache-2.0"),
    ("Bindings GStreamer (gstreamer-rs)", "MIT ou Apache-2.0"),
    (
        "GStreamer (bibliothèque système, liée dynamiquement)",
        "LGPL-2.1 ou ultérieure",
    ),
    ("Plugin NDI® de gst-plugins-rs (gst-plugin-ndi)", "MPL-2.0"),
];

impl Switcher {
    pub(super) fn about_window(&mut self, ui: &egui::Ui) {
        let mut open = self.about_open;
        egui::Window::new("À propos")
            .open(&mut open)
            .resizable(false)
            .show(ui.ctx(), |ui| {
                ui.heading(format!("VMA {}", env!("CARGO_PKG_VERSION")));
                ui.label("Mélangeur vidéo en direct, logiciel libre.");
                ui.separator();
                egui::Grid::new("licences").num_columns(2).show(ui, |ui| {
                    for (what, licence) in LICENCES {
                        ui.label(*what);
                        ui.label(RichText::new(*licence).weak());
                        ui.end_row();
                    }
                });
                ui.separator();
                ui.label(
                    "Le runtime NDI® n'est pas fourni avec VMA : il est installé à part par \
                     l'utilisateur, et chargé au démarrage s'il est présent.",
                );
                ui.label(RichText::new("NDI® is a registered trademark of Vizrt NDI AB.").small());
            });
        self.about_open = open;
    }
}
