//! VMA — main binary: wires the window, the engine and the UI together.

mod cli;
mod soak;

use anyhow::Context as _;
use std::process::ExitCode;
use std::sync::{Arc, OnceLock};
use vma_engine::{EngineConfig, Journal, RenderEngine, RenderStats};
use vma_ui::MainUi;

/// Exit code of a soak run that completed with missed or repeated frames.
const EXIT_SOAK_FAILED: u8 = 2;

fn main() -> anyhow::Result<ExitCode> {
    let options = cli::parse(std::env::args().skip(1))?;
    if options.help {
        #[allow(clippy::print_stdout, reason = "--help output")]
        {
            print!("{}", cli::HELP);
        }
        return Ok(ExitCode::SUCCESS);
    }

    // Logs go through a background writer thread: the render thread never does I/O itself.
    let (log_writer, _log_guard) = tracing_appender::non_blocking(std::io::stderr());
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,wgpu_core=warn,wgpu_hal=warn,naga=warn".into()),
        )
        .with_writer(log_writer)
        .init();

    let mut config = EngineConfig::default();
    if let Some(margin) = options.spin_margin {
        config.spin_margin = margin;
    }

    let native_options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: egui::ViewportBuilder::default()
            .with_title("VMA")
            .with_inner_size([1400.0, 820.0])
            .with_min_inner_size([800.0, 480.0]),
        ..Default::default()
    };

    let device_lost = OnceLock::new();
    let soak_handle = OnceLock::new();
    eframe::run_native(
        "VMA",
        native_options,
        Box::new(|cc| {
            let app = App::new(cc, config, options.soak.clone(), &device_lost, &soak_handle)?;
            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
    .context("window / GPU initialisation failed")?;

    let soak_passed = soak_handle
        .into_inner()
        .map(soak::SoakHandle::finish)
        .transpose()?;
    if let Some(reason) = device_lost.into_inner() {
        anyhow::bail!("GPU device lost ({reason}): restart the application");
    }
    Ok(match soak_passed {
        Some(false) => ExitCode::from(EXIT_SOAK_FAILED),
        _ => ExitCode::SUCCESS,
    })
}

struct App<'a> {
    ui: MainUi,
    render_stats: Arc<RenderStats>,
    ui_probe: Arc<soak::UiProbe>,
    device_lost: &'a OnceLock<String>,
}

impl<'a> App<'a> {
    fn new(
        cc: &eframe::CreationContext<'_>,
        config: EngineConfig,
        soak_options: Option<cli::SoakOptions>,
        device_lost: &'a OnceLock<String>,
        soak_handle: &OnceLock<soak::SoakHandle>,
    ) -> anyhow::Result<Self> {
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

        // The engine installs the uncaptured-error handler and the device-lost callback.
        let (engine, reader) = RenderEngine::spawn(
            render_state.device.clone(),
            render_state.queue.clone(),
            config,
            // Commands come from the UI from step 4 of the Phase 1 plan (journal enabled then).
            Journal::Disabled,
        )?;
        // Until the UI can add inputs (step 4), start with the test pattern on air.
        engine.commands().send(
            vma_engine::Source::Script,
            vma_engine::Command::AddInput {
                spec: vma_engine::InputSpec::TestSource {
                    rate: config.rate,
                    resolution: config.resolution,
                },
                name: None,
            },
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

        let render_stats = engine.render_stats();
        let ui_probe = Arc::new(soak::UiProbe::default());
        if let Some(options) = soak_options {
            let handle = soak::spawn(
                cc.egui_ctx.clone(),
                options,
                soak::SoakContext {
                    adapter: adapter.clone(),
                    config,
                    clock: engine.clock_stats(),
                    render: Arc::clone(&render_stats),
                    ui: Arc::clone(&ui_probe),
                },
            )?;
            // Only ever set here, once.
            let _ = soak_handle.set(handle);
        }

        Ok(Self {
            ui: MainUi::new(engine, reader, program_textures, adapter),
            render_stats,
            ui_probe,
            device_lost,
        })
    }
}

impl eframe::App for App<'_> {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.ui_probe.frame();
        // GPU lost (ADR 0001): the window cannot render any more either. Close it; `main`
        // then exits with an error so that the operator (or a supervisor) restarts us.
        if self.device_lost.get().is_none() && self.render_stats.snapshot().device_lost {
            let reason = self
                .render_stats
                .device_lost_reason()
                .unwrap_or("unknown")
                .to_owned();
            tracing::error!(%reason, "GPU device lost: closing the window");
            let _ = self.device_lost.set(reason);
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        self.ui.show(ui);
    }
}
