//! VMA — main binary: wires the window, the engine and the UI together.

use anyhow::Context as _;
use vma_engine::{EngineConfig, RenderEngine};
use vma_ui::MainUi;

fn main() -> anyhow::Result<()> {
    // Logs go through a background writer thread: the render thread never does I/O itself.
    let (log_writer, _log_guard) = tracing_appender::non_blocking(std::io::stderr());
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,wgpu_core=warn,wgpu_hal=warn,naga=warn".into()),
        )
        .with_writer(log_writer)
        .init();

    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_title("VMA")
            .with_inner_size([1400.0, 820.0])
            .with_min_inner_size([800.0, 480.0]),
        ..Default::default()
    };

    eframe::run_native(
        "VMA",
        options,
        Box::new(|cc| {
            let app = App::new(cc)?;
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
    .context("window / GPU initialisation failed")
}

struct App {
    ui: MainUi,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> anyhow::Result<Self> {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .context("the wgpu renderer is not available")?;
        let info = render_state.adapter.get_info();
        let adapter = format!(
            "{} · {:?} · {:?}",
            info.name, info.backend, info.device_type
        );
        tracing::info!(%adapter, "GPU adapter");

        render_state
            .device
            .on_uncaptured_error(std::sync::Arc::new(|error| {
                tracing::error!(%error, "uncaptured GPU error");
            }));

        let (engine, reader) = RenderEngine::spawn(
            render_state.device.clone(),
            render_state.queue.clone(),
            EngineConfig::default(),
        )?;

        let program_textures = {
            let mut renderer = render_state.renderer.write();
            reader.targets().each_ref().map(|target| {
                renderer.register_native_texture(
                    &render_state.device,
                    target.view(),
                    wgpu::FilterMode::Linear,
                )
            })
        };

        Ok(Self {
            ui: MainUi::new(engine, reader, program_textures, adapter),
        })
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.ui.show(ui);
    }
}
