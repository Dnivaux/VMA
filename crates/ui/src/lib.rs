//! egui user interface (spec `ui.md`).
//!
//! The UI never mutates engine state: every action is a `Command` sent to the engine, and
//! each frame is drawn from the latest state snapshot. [`MainUi`] wires the engine to the
//! [`Switcher`] (the mixer surface) and the statistics panel.

mod stats_panel;
mod switcher;
mod tally;

pub use switcher::{CaptureChoice, FileKind, Images, IoView, Switcher, View};
pub use tally::{TALLY_PREVIEW, TALLY_PROGRAM, Tally};

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError};

use egui::{Rect, TextureId};
use vma_compositor::thumbnails::{self, ATLAS_COLUMNS, ATLAS_ROWS};
use vma_core::InputId;
use vma_engine::{Command, InputSpec, Readers, RenderEngine, Source};

/// egui textures of every engine output, registered once at startup (one per slot).
#[derive(Debug, Clone, Copy)]
pub struct OutputTextures {
    pub program: [TextureId; 3],
    pub preview: [TextureId; 3],
    pub thumbnails: [TextureId; 3],
}

/// What the UI needs from `vma-io` (capture devices, files). Implemented by the application
/// over `vma_io_gst::IoRuntime`, so that the UI does not depend on GStreamer.
pub trait IoStatus: std::fmt::Debug {
    /// Why capture devices and videos are unavailable (GStreamer missing), if they are.
    fn unavailable(&self) -> Option<String>;
    /// Capture devices connected now.
    fn devices(&self) -> Vec<CaptureChoice>;
    /// Mode a capture input runs in (« 1280x720 30p MJPEG »).
    fn capture_mode(&self, input: InputId) -> Option<String>;
    /// Whether `videoconvert` converts the pictures of `input` (`None`: no pipeline).
    fn converting(&self, input: InputId) -> Option<bool>;
}

/// File dialog running on its own thread: the UI keeps drawing while it is open.
#[derive(Debug)]
struct Picker {
    kind: FileKind,
    result: Receiver<Option<PathBuf>>,
}

/// Root UI state.
#[derive(Debug)]
pub struct MainUi {
    engine: RenderEngine,
    readers: Readers,
    textures: OutputTextures,
    switcher: Switcher,
    adapter: String,
    /// Dropped after the engine: `vma-control` stops sending requests to `vma-io` first.
    io: Box<dyn IoStatus>,
    picker: Option<Picker>,
}

impl MainUi {
    pub fn new(
        engine: RenderEngine,
        readers: Readers,
        textures: OutputTextures,
        adapter: String,
        io: Box<dyn IoStatus>,
    ) -> Self {
        Self {
            engine,
            readers,
            textures,
            switcher: Switcher::default(),
            adapter,
            io,
            picker: None,
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
                    stats_panel::show(
                        ui,
                        &self.engine,
                        self.io.as_ref(),
                        &self.readers.program,
                        &self.adapter,
                    );
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
        let timelines = self.engine.timelines();
        let timeline = |input| timelines.get(input);
        let io = self.io.as_ref();
        let capture_mode = |input| io.capture_mode(input);
        let unavailable = io.unavailable();
        let devices = io.devices();
        let view = View {
            state: &state,
            commands: &commands,
            images: Images {
                program: pick(&self.textures.program, self.readers.program.current_slot()),
                preview: pick(&self.textures.preview, self.readers.preview.current_slot()),
                thumbnails: pick(&self.textures.thumbnails, thumbs.current_slot()),
                thumbnail_uv: &thumbnail_uv,
            },
            io: IoView {
                unavailable: unavailable.as_deref(),
                devices: &devices,
                timeline: &timeline,
                capture_mode: &capture_mode,
            },
            frame: self.engine.feedback().last_frame(),
            rejection: rejection.as_deref(),
        };
        self.switcher.show(ui, &view);
        self.files(ui.ctx());

        // Program and Preview change every tick: keep repainting at the display rate.
        ui.ctx().request_repaint();
    }

    /// Opens the file dialog the switcher asked for, and adds the chosen file as an input.
    fn files(&mut self, ctx: &egui::Context) {
        if let Some(kind) = self.switcher.take_file_request()
            && self.picker.is_none()
        {
            self.picker = open_dialog(kind, ctx.clone());
        }
        let Some(picker) = &self.picker else {
            return;
        };
        match picker.result.try_recv() {
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) | Ok(None) => self.picker = None,
            Ok(Some(path)) => {
                let spec = match picker.kind {
                    FileKind::Video => InputSpec::File { path },
                    FileKind::Image => InputSpec::Image { path },
                };
                let command = Command::AddInput { spec, name: None };
                if let Err(e) = self.engine.commands().send(Source::Ui, command) {
                    tracing::warn!(error = %e, "input not added");
                }
                self.picker = None;
            }
        }
    }
}

/// Runs the native file dialog (`rfd`) on a thread of its own; `None` if it cannot start.
fn open_dialog(kind: FileKind, ctx: egui::Context) -> Option<Picker> {
    let (tx, result) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("vma-file-dialog".into())
        .spawn(move || {
            let dialog = match kind {
                FileKind::Video => rfd::FileDialog::new()
                    .set_title("Ajouter une vidéo")
                    .add_filter(
                        "Vidéos",
                        &[
                            "mp4", "mov", "mkv", "webm", "avi", "m4v", "mxf", "ts", "mts",
                        ],
                    ),
                FileKind::Image => rfd::FileDialog::new()
                    .set_title("Ajouter une image")
                    .add_filter("Images", &["png", "jpg", "jpeg"]),
            };
            let _ = tx.send(dialog.add_filter("Tous les fichiers", &["*"]).pick_file());
            ctx.request_repaint();
        });
    match spawned {
        Ok(_) => Some(Picker { kind, result }),
        Err(e) => {
            tracing::error!(error = %e, "cannot open the file dialog");
            None
        }
    }
}
