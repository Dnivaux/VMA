//! egui user interface.
//!
//! Phase 0: the Program view and the statistics panel. The UI never mutates engine state
//! directly; from Phase 1 every action goes through the engine's `Command` channel.

mod stats_panel;

use aiguillage_engine::{ProgramReader, RenderEngine};
use egui::{Color32, Stroke, TextureId};

/// Tally red, used for the Program border.
pub const TALLY_PROGRAM: Color32 = Color32::from_rgb(220, 30, 30);

/// Root UI state.
#[derive(Debug)]
pub struct MainUi {
    engine: RenderEngine,
    reader: ProgramReader,
    /// egui texture of each Program slot, registered once at startup.
    program_textures: [TextureId; 3],
    adapter: String,
}

impl MainUi {
    pub fn new(
        engine: RenderEngine,
        reader: ProgramReader,
        program_textures: [TextureId; 3],
        adapter: String,
    ) -> Self {
        Self {
            engine,
            reader,
            program_textures,
            adapter,
        }
    }

    /// Draws one UI frame.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        // Take the newest Program frame before recording anything that samples it.
        self.reader.update();

        egui::Panel::right("stats")
            .resizable(false)
            .default_size(280.0)
            .show(ui, |ui| {
                stats_panel::show(ui, &self.engine, &self.reader, &self.adapter);
            });

        egui::CentralPanel::default().show(ui, |ui| self.program_view(ui));

        // The Program changes every tick: keep repainting at the display rate.
        ui.ctx().request_repaint();
    }

    fn program_view(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("PROGRAM").strong().color(TALLY_PROGRAM));
            ui.label(format!(
                "{} · {}",
                self.engine.config().resolution,
                self.engine.config().rate
            ));
        });

        let aspect = self.engine.config().resolution.aspect_ratio();
        let area = ui.available_rect_before_wrap().shrink(4.0);
        let size = if area.width() / area.height() > aspect {
            egui::vec2(area.height() * aspect, area.height())
        } else {
            egui::vec2(area.width(), area.width() / aspect)
        };
        let rect = egui::Rect::from_center_size(area.center(), size);
        ui.allocate_rect(area, egui::Sense::hover());

        let texture = self
            .program_textures
            .get(self.reader.current_slot())
            .copied()
            .unwrap_or_default();
        egui::Image::from_texture((texture, size)).paint_at(ui, rect);
        ui.painter().rect_stroke(
            rect,
            0.0,
            Stroke::new(2.0, TALLY_PROGRAM),
            egui::StrokeKind::Outside,
        );
    }
}
