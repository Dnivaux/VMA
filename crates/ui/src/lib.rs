//! egui user interface (spec `ui.md`).
//!
//! The UI never mutates engine state: every action is a `Command` sent to the engine, and
//! each frame is drawn from the latest state snapshot. [`MainUi`] wires the engine to the
//! [`Switcher`] (the mixer surface) and the statistics panel.

mod stats_panel;
mod switcher;
mod tally;

pub use switcher::{Images, Switcher, View};
pub use tally::{TALLY_PREVIEW, TALLY_PROGRAM, Tally};

use egui::{Rect, TextureId};
use vma_compositor::thumbnails::{self, ATLAS_COLUMNS, ATLAS_ROWS};
use vma_engine::{Readers, RenderEngine};

/// egui textures of every engine output, registered once at startup (one per slot).
#[derive(Debug, Clone, Copy)]
pub struct OutputTextures {
    pub program: [TextureId; 3],
    pub preview: [TextureId; 3],
    pub thumbnails: [TextureId; 3],
}

/// Root UI state.
#[derive(Debug)]
pub struct MainUi {
    engine: RenderEngine,
    readers: Readers,
    textures: OutputTextures,
    switcher: Switcher,
    adapter: String,
}

impl MainUi {
    pub fn new(
        engine: RenderEngine,
        readers: Readers,
        textures: OutputTextures,
        adapter: String,
    ) -> Self {
        Self {
            engine,
            readers,
            textures,
            switcher: Switcher::default(),
            adapter,
        }
    }

    /// Draws one UI frame.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        // Take the newest frames before recording anything that samples them (ADR 0001).
        self.readers.program.update();
        self.readers.preview.update();
        self.readers.thumbnails.update();

        if self.switcher.show_stats {
            egui::Panel::right("stats")
                .resizable(false)
                .default_size(280.0)
                .show(ui, |ui| {
                    stats_panel::show(ui, &self.engine, &self.readers.program, &self.adapter);
                });
        }

        let pick = |set: &[TextureId; 3], slot: usize| set.get(slot).copied().unwrap_or_default();
        let thumbs = &self.readers.thumbnails;
        let thumbnail_uv = |input| {
            let cell = thumbs.cell_of(input)?;
            let (x, y, w, h) = thumbnails::cell_rect(cell)?;
            let (aw, ah) = ((ATLAS_COLUMNS * w) as f32, (ATLAS_ROWS * h) as f32);
            Some(Rect::from_min_max(
                egui::pos2(x as f32 / aw, y as f32 / ah),
                egui::pos2((x + w) as f32 / aw, (y + h) as f32 / ah),
            ))
        };
        let state = self.engine.state().load();
        let stats = self.engine.control_stats();
        let rejection = stats.last_rejection();
        let commands = self.engine.commands();
        let view = View {
            state: &state,
            commands: &commands,
            images: Images {
                program: pick(&self.textures.program, self.readers.program.current_slot()),
                preview: pick(&self.textures.preview, self.readers.preview.current_slot()),
                thumbnails: pick(&self.textures.thumbnails, thumbs.current_slot()),
                thumbnail_uv: &thumbnail_uv,
            },
            frame: self.engine.feedback().last_frame(),
            rejection: rejection.as_deref(),
        };
        self.switcher.show(ui, &view);

        // Program and Preview change every tick: keep repainting at the display rate.
        ui.ctx().request_repaint();
    }
}
