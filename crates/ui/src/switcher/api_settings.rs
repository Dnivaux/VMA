//! Réglages → API de contrôle (ADR 0012): reachable from the network or not, the ports, what
//! became of each server; « Appliquer » starts them again ([`Switcher::take_api_request`]).

use egui::RichText;
use vma_api::{ApiSettings, ApiStatus, ServerStatus};

use super::{Switcher, View, WARNING};

/// What the switcher knows of the control APIs.
#[derive(Debug, Clone, Copy)]
pub struct ApiView<'a> {
    pub settings: ApiSettings,
    pub status: &'a ApiStatus,
    /// `false`: VMA was started with `--no-api`.
    pub enabled: bool,
}

impl ApiView<'static> {
    /// No API (tests).
    pub fn none() -> Self {
        static NONE: std::sync::OnceLock<ApiStatus> = std::sync::OnceLock::new();
        Self {
            settings: ApiSettings::default(),
            status: NONE.get_or_init(ApiStatus::default),
            enabled: false,
        }
    }
}

impl Switcher {
    /// New settings for the APIs, once.
    pub fn take_api_request(&mut self) -> Option<ApiSettings> {
        self.api.request.take()
    }

    /// A warning in the top bar when a port could not be opened.
    pub(super) fn api_warning(ui: &mut egui::Ui, view: &View<'_>) {
        if !view.api.status.any_failed() {
            return;
        }
        let failures: Vec<String> = view
            .api
            .status
            .servers()
            .iter()
            .filter_map(|(name, s)| match s {
                ServerStatus::Failed(why) => Some(format!("{name} : {why}")),
                _ => None,
            })
            .collect();
        ui.label(RichText::new("API de contrôle : un port n'a pas pu s'ouvrir").color(WARNING))
            .on_hover_text(format!(
                "{}\nRéglages → API de contrôle pour changer de port.",
                failures.join("\n")
            ));
    }

    /// The section of the settings window.
    pub(super) fn api_section(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        ui.label(RichText::new("API de contrôle").strong());
        if !view.api.enabled {
            ui.label("Fermées : VMA a été lancé avec --no-api.");
            return;
        }
        let draft = self.api.draft.get_or_insert(view.api.settings);
        ui.checkbox(&mut draft.network, "Accessible depuis le réseau");
        if draft.network {
            ui.label(
                RichText::new(
                    "Tout poste du réseau pourra piloter l'émission : l'API compat n'a pas de \
                     mot de passe.",
                )
                .color(WARNING),
            );
        }
        egui::Grid::new("vma.api_ports").show(ui, |ui| {
            for (label, port) in [
                ("Port compat TCP (Companion)", &mut draft.compat_tcp_port),
                ("Port compat HTTP", &mut draft.compat_http_port),
                ("Port de l'API native", &mut draft.native_port),
            ] {
                let name = ui.label(label);
                ui.add(egui::DragValue::new(port).range(1024..=65535))
                    .labelled_by(name.id);
                ui.end_row();
            }
        });
        for (name, status) in view.api.status.servers() {
            let (text, warn) = match status {
                ServerStatus::Listening(at) => (format!("{name} : à l'écoute sur {at}"), false),
                ServerStatus::Failed(why) => (format!("{name} : fermée ({why})"), true),
                ServerStatus::Off => (format!("{name} : fermée"), false),
            };
            let text = RichText::new(text).small();
            ui.label(if warn {
                text.color(WARNING)
            } else {
                text.weak()
            });
        }
        let changed = *draft != view.api.settings;
        let settings = *draft;
        if ui
            .add_enabled(changed, egui::Button::new("Appliquer"))
            .on_hover_text("Ferme les API puis les rouvre avec ces réglages")
            .clicked()
        {
            self.api.request = Some(settings);
            self.api.draft = None;
        }
    }
}

/// The section's own state.
#[derive(Debug, Default)]
pub(super) struct ApiUi {
    /// What is being typed, from the settings in use.
    draft: Option<ApiSettings>,
    request: Option<ApiSettings>,
}
