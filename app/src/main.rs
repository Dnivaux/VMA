//! VMA — main binary: wires the window, the engine and the UI together.

mod cli;
mod screens;
mod soak;

use anyhow::Context as _;
use std::process::ExitCode;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use vma_engine::{EngineConfig, Journal, RenderEngine, RenderStats, State, StateReader};
use vma_ui::MainUi;

/// Exit code of a soak run that completed with missed or repeated frames.
const EXIT_SOAK_FAILED: u8 = 2;
/// GPU lost: how long after the grace period the window waits for `vma-io` to finalise the
/// outputs before it closes anyway (closing finalises them too, without waiting).
const FINALISE_TIMEOUT: Duration = Duration::from_secs(10);

/// GPU lost (ADR 0006 § 7), `lost_for` ago: whether the application may close. Not while an
/// output still gets black or is being finalised (the full screen shows nothing any more),
/// unless finalising takes too long.
fn gpu_loss_handled(state: &State, lost_for: Duration, grace: Duration) -> bool {
    let busy = state
        .outputs
        .iter()
        .any(|o| o.status.is_active() && !o.spec.is_fullscreen());
    !busy || lost_for >= grace + FINALISE_TIMEOUT
}

/// The engine's settings from the command line.
fn engine_config(options: &cli::Options) -> EngineConfig {
    let mut config = EngineConfig {
        rate: options.rate,
        resolution: options.resolution,
        // Ticks dated on the clock of the pipelines: without it the sound of every input is
        // looked for where there is none (ADR 0007 § 1).
        stream_clock: Some(vma_io_gst::stream_clock()),
        ..EngineConfig::default()
    };
    if let Some(margin) = options.spin_margin {
        config.spin_margin = margin;
    }
    config.sync_test = options.sync_test;
    if let Some(grace) = options.gpu_loss_grace {
        config.device_lost_grace = grace;
    }
    config
}

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

    let config = engine_config(&options);

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
    // eframe on our own event loop, to list the screens (full-screen output). Its errors end
    // the loop: kept here to be returned.
    let started = OnceLock::new();
    let event_loop = winit::event_loop::EventLoop::<eframe::UserEvent>::with_user_event()
        .build()
        .context("window system unavailable")?;
    let screens = vma_ui::Screens::default();
    let mut app = screens::WithScreens::new(
        eframe::create_native(
            "VMA",
            native_options,
            Box::new(|cc| {
                let journal = match &options.journal_dir {
                    Some(dir) => Journal::Directory(dir.clone()),
                    None => Journal::Disabled,
                };
                let app = App::new(
                    cc,
                    config,
                    journal,
                    &options,
                    &device_lost,
                    &soak_handle,
                    screens.clone(),
                );
                let _ = started.set(app.as_ref().map(|_| ()).map_err(|e| format!("{e:#}")));
                Ok(Box::new(app?))
            }),
            &event_loop,
        ),
        screens.clone(),
    );
    event_loop
        .run_app(&mut app)
        .context("window event loop failed")?;
    drop(app);
    match started.into_inner() {
        Some(Ok(())) => {}
        Some(Err(e)) => anyhow::bail!("start-up failed: {e}"),
        None => anyhow::bail!("window / GPU initialisation failed (see the log)"),
    }

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
    /// Owns the engine and `vma-io` (dropped after the engine: its requests channel closes
    /// first).
    ui: MainUi,
    render_stats: Arc<RenderStats>,
    ui_probe: Arc<soak::UiProbe>,
    device_lost: &'a OnceLock<String>,
    state: StateReader,
    device_lost_grace: Duration,
    /// When this window first saw the GPU lost.
    lost_since: Option<Instant>,
}

impl<'a> App<'a> {
    fn new(
        cc: &eframe::CreationContext<'_>,
        config: EngineConfig,
        journal: Journal,
        options: &cli::Options,
        device_lost: &'a OnceLock<String>,
        soak_handle: &OnceLock<soak::SoakHandle>,
        screens: vma_ui::Screens,
    ) -> anyhow::Result<Self> {
        let (inputs, record, soak_options) = (
            &options.inputs,
            options.record.clone(),
            options.soak.clone(),
        );
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
        let (mut engine, readers) = RenderEngine::spawn(
            render_state.device.clone(),
            render_state.queue.clone(),
            config,
            journal,
        )?;
        // Images, capture and files (ADR 0004). Without GStreamer the runtime still serves
        // still images; without the thread at all, those inputs stay « Démarrage ».
        let io = engine.take_io_port().and_then(|port| {
            let io_options = vma_io_gst::IoOptions {
                headphones: options.headphones,
            };
            match vma_io_gst::IoRuntime::start_with_options(port, io_options) {
                Ok(io) => Some(io),
                Err(e) => {
                    tracing::error!(error = %e, "io runtime not started");
                    None
                }
            }
        });
        // Synthetic sources asked for on the command line (soaks, rate tests). Without them a
        // soak still needs a moving picture on air: the test pattern. A normal session starts
        // empty, like the reference.
        let script = |spec| {
            engine.commands().send(
                vma_engine::Source::Script,
                vma_engine::Command::AddInput { spec, name: None },
            )
        };
        for input in inputs {
            script(vma_engine::InputSpec::Synthetic {
                rate: input.rate,
                resolution: input.resolution,
            })?;
        }
        if soak_options.is_some() && inputs.is_empty() {
            script(vma_engine::InputSpec::TestSource {
                rate: config.rate,
                resolution: config.resolution,
            })?;
        }
        // `--record`: the recording present from the start, into that folder, started now.
        if let Some(folder) = record {
            for command in [
                vma_engine::Command::UpdateOutput {
                    output: vma_engine::State::DEFAULT_RECORDING,
                    spec: vma_engine::OutputSpec::Record(vma_engine::RecordSpec {
                        folder: Some(folder),
                        ..vma_engine::RecordSpec::default()
                    }),
                },
                vma_engine::Command::StartRecording,
            ] {
                engine
                    .commands()
                    .send(vma_engine::Source::Script, command)?;
            }
        }

        // `--stream`: one RTMP destination, started now.
        if let Some(stream) = &options.stream {
            for command in [
                vma_engine::Command::AddOutput {
                    spec: vma_engine::OutputSpec::Rtmp(vma_engine::RtmpSpec {
                        url: stream.url.clone(),
                        key: stream.key.clone(),
                        video_kbps: None,
                    }),
                    name: None,
                },
                vma_engine::Command::StartStreaming { slot: None },
            ] {
                engine
                    .commands()
                    .send(vma_engine::Source::Script, command)?;
            }
        }

        let textures = {
            let mut renderer = render_state.renderer.write();
            let mut register = |reader: &vma_engine::FrameReader| {
                reader.targets().each_ref().map(|target| {
                    renderer.register_native_texture(
                        &render_state.device,
                        target.view(),
                        wgpu::FilterMode::Linear,
                    )
                })
            };
            vma_ui::OutputTextures {
                program: register(&readers.program),
                preview: register(&readers.preview),
                thumbnails: register(&readers.thumbnails),
                fullscreen: register(&readers.fullscreen),
            }
        };
        // `--fullscreen`: the full-screen output present and on from the start.
        if let Some(wanted) = &options.fullscreen {
            start_fullscreen(&engine, wanted, &screens.list())?;
        }

        let render_stats = engine.render_stats();
        let state = engine.state();
        let ui_probe = Arc::new(soak::UiProbe::default());
        let stream_key = options
            .stream
            .as_ref()
            .map(|s| s.key.clone())
            .unwrap_or_default();
        let soak_context = soak_options.map(|options| {
            (
                options,
                soak::SoakContext {
                    adapter: adapter.clone(),
                    config,
                    clock: engine.clock_stats(),
                    render: Arc::clone(&render_stats),
                    ui: Arc::clone(&ui_probe),
                    fullscreen: Arc::default(),
                    state: engine.state(),
                    input_stats: engine.input_stats(),
                    outputs: engine.outputs(),
                    commands: engine.commands(),
                    // Mix the sources on air while the soak runs (Fade, then Cut, …).
                    transitions: inputs.len() >= 2,
                    stream_key: stream_key.clone(),
                },
            )
        });
        let ui = MainUi::new(
            engine,
            readers,
            textures,
            adapter,
            Box::new(Io(io, options.headphones)),
            screens,
        );
        if let Some((options, mut context)) = soak_context {
            context.fullscreen = ui.fullscreen_stats();
            let handle = soak::spawn(cc.egui_ctx.clone(), options, context)?;
            // Only ever set here, once.
            let _ = soak_handle.set(handle);
        }

        Ok(Self {
            ui,
            render_stats,
            ui_probe,
            device_lost,
            state,
            device_lost_grace: config.device_lost_grace,
            lost_since: None,
        })
    }
}

/// Adds the full-screen output on `wanted` (a screen name, or its number from 1 in `screens`)
/// and starts it, as script commands.
fn start_fullscreen(
    engine: &RenderEngine,
    wanted: &str,
    screens: &[vma_ui::Screen],
) -> anyhow::Result<()> {
    let by_number = wanted
        .parse::<usize>()
        .ok()
        .and_then(|n| n.checked_sub(1))
        .and_then(|i| screens.get(i));
    let screen = by_number.map_or_else(|| wanted.to_owned(), |s| s.name.clone());
    if !screens.iter().any(|s| s.name == screen) {
        tracing::warn!(%screen, "--fullscreen: screen not connected: the output stays hidden until it is");
    }
    let commands = engine.commands();
    commands.send(
        vma_engine::Source::Script,
        vma_engine::Command::AddOutput {
            spec: vma_engine::OutputSpec::Fullscreen(vma_engine::FullscreenSpec {
                screen: Some(screen),
            }),
            name: None,
        },
    )?;
    // Its id comes from `vma-control`: wait for the state that has it.
    let state = engine.state();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let output = loop {
        if let Some(o) = state.load().outputs.iter().find(|o| o.spec.is_fullscreen()) {
            break o.id;
        }
        if std::time::Instant::now() > deadline {
            anyhow::bail!("--fullscreen: the output was not added");
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    commands.send(
        vma_engine::Source::Script,
        vma_engine::Command::StartOutput { output },
    )?;
    Ok(())
}

/// `vma-io` as the UI sees it (the UI does not depend on GStreamer).
#[derive(Debug)]
/// The runtime, and whether it plays the headphones.
struct Io(Option<vma_io_gst::IoRuntime>, bool);

impl vma_ui::IoStatus for Io {
    fn unavailable(&self) -> Option<String> {
        match &self.0 {
            None => Some("le thread d'entrées n'a pas démarré (voir le journal)".into()),
            Some(io) => io.gstreamer_error().map(str::to_owned),
        }
    }

    fn devices(&self) -> Vec<vma_ui::CaptureChoice> {
        self.0
            .iter()
            .flat_map(|io| io.devices())
            .map(|d| vma_ui::CaptureChoice {
                id: d.id,
                name: d.name,
            })
            .collect()
    }

    fn capture_mode(&self, input: vma_core::InputId) -> Option<String> {
        Some(self.0.as_ref()?.pipeline_info(input)?.mode?.to_string())
    }

    fn converting(&self, input: vma_core::InputId) -> Option<bool> {
        Some(self.0.as_ref()?.pipeline_info(input)?.converting)
    }

    fn output_activity(&self, output: vma_core::OutputId) -> Option<vma_ui::OutputActivity> {
        let a = self.0.as_ref()?.output_activity(output)?;
        Some(vma_ui::OutputActivity {
            running_for: a.since.elapsed(),
            bytes: a.bytes,
            kbps: a.kbps,
            reconnections: a.reconnections,
        })
    }

    fn default_record_folder(&self) -> Option<std::path::PathBuf> {
        Some(vma_io_gst::default_record_folder())
    }

    fn audio_devices(&self) -> (Vec<vma_ui::CaptureChoice>, Vec<vma_ui::CaptureChoice>) {
        let Some(io) = &self.0 else {
            return (Vec::new(), Vec::new());
        };
        let choices = |devices: Vec<vma_io_gst::CaptureDevice>| {
            devices
                .into_iter()
                .map(|d| vma_ui::CaptureChoice {
                    id: d.id,
                    name: d.name,
                })
                .collect()
        };
        let devices = io.audio_devices();
        (choices(devices.sources), choices(devices.sinks))
    }

    fn headphones(&self) -> Option<vma_ui::HeadphonesActivity> {
        let io = self.0.as_ref()?;
        if !self.1 {
            return None;
        }
        let status = io.headphones();
        Some(vma_ui::HeadphonesActivity {
            device: status.device,
            playing: status.playing,
            error: status.error,
        })
    }
}

impl eframe::App for App<'_> {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.ui_probe.frame();
        // GPU lost (ADR 0001, ADR 0006 § 7): the window cannot render any more either. Once
        // the outputs are finalised, close it; `main` then exits with an error so that the
        // operator (or a supervisor) restarts us.
        if self.device_lost.get().is_none() && self.render_stats.snapshot().device_lost {
            let now = Instant::now();
            let since = *self.lost_since.get_or_insert_with(|| {
                tracing::error!(grace = ?self.device_lost_grace, "GPU device lost: the window closes once the outputs are finalised");
                now
            });
            if gpu_loss_handled(&self.state.load(), now - since, self.device_lost_grace) {
                let reason = self
                    .render_stats
                    .device_lost_reason()
                    .unwrap_or("unknown")
                    .to_owned();
                tracing::error!(%reason, "GPU device lost: closing the window");
                let _ = self.device_lost.set(reason);
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
        self.ui.show(ui);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use vma_engine::{Command, Event, FullscreenSpec, OutputSpec};

    /// The engine dates its ticks on the clock of the pipelines, as `vma-io` dates the sound
    /// (without it, no input is ever heard: found in the manual test of 2026-11-03).
    #[test]
    fn the_engine_ticks_on_the_clock_of_the_pipelines() {
        let options = cli::parse(Vec::<String>::new()).unwrap();
        let clock = engine_config(&options).stream_clock.unwrap();
        let pipelines = vma_io_gst::stream_clock();
        let gap = clock.now().abs_diff(pipelines.now());
        assert!(gap < 1_000_000, "{gap} ns apart");
    }

    #[test]
    fn after_a_gpu_loss_the_window_waits_for_the_outputs() {
        let grace = Duration::from_secs(10);
        let mut state = State::new(vma_engine::Format {
            rate: vma_core::FrameRate::FPS_50,
            resolution: vma_core::Resolution::HD_1080,
        });
        let fullscreen = OutputSpec::Fullscreen(FullscreenSpec::default());
        state
            .apply(&Command::AddOutput {
                spec: fullscreen,
                name: None,
            })
            .unwrap();
        let fs = state.outputs.last().unwrap().id;
        state.apply(&Command::StartOutput { output: fs }).unwrap();
        // Only the full screen on: nothing to finalise, close at once.
        assert!(gpu_loss_handled(&state, Duration::ZERO, grace));

        state.apply(&Command::StartRecording).unwrap();
        state.apply_event(&Event::DeviceLost { frame: 10 });
        assert!(!gpu_loss_handled(&state, grace, grace), "still black");
        state.apply_event(&Event::DeviceLostGraceOver);
        assert!(!gpu_loss_handled(&state, grace, grace), "being finalised");
        // `vma-io` never answers: closing finalises anyway.
        assert!(gpu_loss_handled(&state, grace + FINALISE_TIMEOUT, grace));
        let run = state.output(State::DEFAULT_RECORDING).unwrap().run;
        state.apply_event(&Event::OutputStopped {
            output: State::DEFAULT_RECORDING,
            run,
        });
        assert!(gpu_loss_handled(&state, grace, grace));
    }
}
