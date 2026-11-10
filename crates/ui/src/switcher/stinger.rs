//! Stingers (Phase 4 step 8, ADR 0010 § 5): the two slots in « Réglages » (file, cut point,
//! state), and the effects a transition button or Quick Play can take.

use egui::RichText;
use vma_engine::{Command, Effect, StingerSlot, StingerState, StingerStatus};

use super::{FileKind, Switcher, View};

/// Every effect of a transition button or of Quick Play.
pub(super) const EFFECTS: [Effect; 4] = [
    Effect::Fade,
    Effect::Cut,
    Effect::Stinger(StingerSlot::ALL[0]),
    Effect::Stinger(StingerSlot::ALL[1]),
];

/// « Prêt : 2,00 s, 1920x1080 50p, avec alpha », « Chargement… », the reason of a failure.
pub(super) fn describe_status(stinger: &StingerState) -> String {
    match &stinger.status {
        StingerStatus::Empty => "Aucun fichier".to_owned(),
        StingerStatus::Loading => "Chargement…".to_owned(),
        StingerStatus::Failed { reason } => format!("Erreur : {reason}"),
        StingerStatus::Ready {
            resolution,
            rate,
            alpha,
            ..
        } => {
            let secs = f64::from(stinger.duration_ms().unwrap_or(0)) / 1000.0;
            let alpha = if *alpha == vma_core::AlphaMode::Opaque {
                ""
            } else {
                ", avec alpha"
            };
            format!("Prêt : {secs:.2} s, {resolution} {rate}{alpha}").replace('.', ",")
        }
    }
}

impl Switcher {
    /// The « Stingers » section of « Réglages ».
    pub(super) fn stinger_settings(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        ui.label(RichText::new("Stingers").strong());
        for slot in StingerSlot::ALL {
            let stinger = view.state.stinger(slot);
            let n = slot.number();
            ui.horizontal(|ui| {
                ui.label(format!("Stinger {n}"));
                let file = stinger
                    .file
                    .as_ref()
                    .and_then(|f| f.file_name())
                    .map_or_else(|| "—".to_owned(), |f| f.to_string_lossy().into_owned());
                ui.label(RichText::new(file).monospace());
                if ui.button(format!("Fichier du stinger {n}…")).clicked() {
                    self.file_request = Some(FileKind::Stinger(slot));
                }
                if stinger.file.is_some() && ui.button(format!("Vider le stinger {n}")).clicked() {
                    self.send(
                        ui,
                        view,
                        Command::SetStinger {
                            slot,
                            file: None,
                            cut_point_ms: None,
                        },
                    );
                }
            });
            ui.label(RichText::new(describe_status(stinger)).small());
            ui.horizontal(|ui| {
                let mut half = stinger.cut_point_ms.is_none();
                let mut ms = stinger
                    .cut_point_ms
                    .or_else(|| stinger.cut_ms())
                    .unwrap_or(0);
                let toggled = ui
                    .checkbox(&mut half, format!("Bascule au milieu ({n})"))
                    .changed();
                let label = ui.label("Point de bascule");
                let dragged = ui
                    .add_enabled(
                        !half,
                        egui::DragValue::new(&mut ms)
                            .range(0..=stinger.duration_ms().unwrap_or(60_000))
                            .speed(10)
                            .suffix(" ms"),
                    )
                    .labelled_by(label.id)
                    .changed();
                if toggled || dragged {
                    self.send(
                        ui,
                        view,
                        Command::SetStinger {
                            slot,
                            file: stinger.file.clone(),
                            cut_point_ms: (!half).then_some(ms),
                        },
                    );
                }
            });
        }
    }
}
