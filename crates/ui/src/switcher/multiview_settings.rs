//! Réglages → MultiView (ADR 0014): layout, titles, what each input cell shows.

use egui::RichText;
use vma_engine::{Command, MultiviewLayout, MultiviewSlot};

use super::{Switcher, View};

fn layout_label(layout: MultiviewLayout) -> &'static str {
    match layout {
        MultiviewLayout::PlusEight => "Preview / Program + 8 inputs",
        MultiviewLayout::PlusFourteen => "Preview / Program + 14 inputs",
        MultiviewLayout::PlusTwo => "Preview / Program + 2 inputs",
        MultiviewLayout::Clean => "Preview / Program + 8, sans titres ni vumètres",
    }
}

impl Switcher {
    pub(super) fn multiview_section(&mut self, ui: &mut egui::Ui, view: &View<'_>) {
        let state = view.state;
        let current = &state.multiview;
        let mut settings = current.clone();
        ui.label(RichText::new("MultiView").strong())
            .on_hover_text("Montrée par un plein écran réglé sur « MultiView » (Sorties)");
        let name = ui.label("Disposition");
        egui::ComboBox::from_id_salt("vma.multiview_layout")
            .selected_text(layout_label(settings.layout))
            .show_ui(ui, |ui| {
                for layout in MultiviewLayout::ALL {
                    ui.selectable_value(&mut settings.layout, layout, layout_label(layout));
                }
            })
            .response
            .labelled_by(name.id);
        ui.checkbox(&mut settings.titles_below, "Titres sous les images");
        ui.checkbox(
            &mut settings.names_on_views,
            "Nom de l'input sur la Preview et le Program",
        );
        ui.collapsing("Ordre des cases", |ui| {
            let label = |slot: MultiviewSlot| match slot {
                MultiviewSlot::Auto => "Automatique".to_owned(),
                MultiviewSlot::Empty => "Vide".to_owned(),
                MultiviewSlot::Input(key) => state
                    .inputs
                    .iter()
                    .position(|i| i.key == key)
                    .and_then(|n| state.inputs.get(n).map(|i| format!("{} {}", n + 1, i.name)))
                    .unwrap_or_else(|| "Input retiré".to_owned()),
            };
            egui::Grid::new("vma.multiview_slots").show(ui, |ui| {
                for n in 0..settings.layout.inputs() {
                    if settings.slots.len() <= n {
                        settings.slots.resize(n + 1, MultiviewSlot::Auto);
                    }
                    let Some(slot) = settings.slots.get_mut(n) else {
                        continue;
                    };
                    let name = ui.label(format!("Case {}", n + 1));
                    egui::ComboBox::from_id_salt(("vma.multiview_slot", n))
                        .selected_text(label(*slot))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(slot, MultiviewSlot::Auto, "Automatique");
                            ui.selectable_value(slot, MultiviewSlot::Empty, "Vide");
                            for (i, input) in state.inputs.iter().enumerate() {
                                let text = format!("{} {}", i + 1, input.name);
                                ui.selectable_value(slot, MultiviewSlot::Input(input.key), text);
                            }
                        })
                        .response
                        .labelled_by(name.id);
                    ui.end_row();
                }
            });
        });
        // Trailing « Automatique » cells say nothing: not kept.
        while settings.slots.last() == Some(&MultiviewSlot::Auto) {
            settings.slots.pop();
        }
        settings.slots.truncate(vma_engine::MULTIVIEW_INPUTS);
        if settings != *current {
            self.send(ui, view, Command::SetMultiview { settings });
        }
    }
}
