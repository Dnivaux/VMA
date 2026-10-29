use egui::{Color32, RichText};
use std::time::Duration;
use vma_engine::{FrameReader, InputStatus, RenderEngine};

use crate::IoStatus;

const OK: Color32 = Color32::from_rgb(80, 200, 120);
const BAD: Color32 = Color32::from_rgb(230, 70, 70);

pub(crate) fn show(
    ui: &mut egui::Ui,
    engine: &RenderEngine,
    io: &dyn IoStatus,
    reader: &FrameReader,
    adapter: &str,
) {
    let clock = engine.clock_stats().snapshot();
    let render = engine.render_stats().snapshot();
    let ui_dt = ui.input(|i| i.stable_dt);

    ui.heading("Statistiques");
    ui.separator();

    egui::Grid::new("stats_grid")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .striped(true)
        .show(ui, |ui| {
            row(ui, "Cadence cible", clock.rate.to_string());
            row(
                ui,
                "Cadence mesurée",
                format!("{:.2} fps", render.measured_fps),
            );
            counter(ui, "Images manquées (horloge)", clock.missed);
            counter(ui, "Images répétées (GPU saturé)", render.gpu_dropped);
            counter(ui, "Erreurs GPU", render.gpu_errors);
            row(ui, "Images rendues", render.frames_rendered.to_string());
            row(
                ui,
                "Image affichée",
                format!("#{}", reader.current_frame_index()),
            );
            row(ui, "Rendu CPU (dernier)", ms(render.last_cpu));
            row(ui, "Rendu CPU (moyen)", ms(render.avg_cpu));
            row(ui, "Rendu CPU (max)", ms(render.max_cpu));
            row(
                ui,
                "Retard réveil p50 / p99",
                format!("{} / {}", ms(clock.wake.p50), ms(clock.wake.p99)),
            );
            row(ui, "Retard réveil (max)", ms(clock.max_lateness));
            row(
                ui,
                "Marge d'attente active",
                ms(engine.config().spin_margin),
            );
            row(ui, "Images GPU en vol", render.gpu_in_flight.to_string());
            row(
                ui,
                "Rafraîchissement UI",
                if ui_dt > 0.0 {
                    format!("{:.0} Hz", 1.0 / ui_dt)
                } else {
                    "—".into()
                },
            );
            row(ui, "Utilisation GPU", "non mesurée (à venir)".into());
            let control = engine.control_stats();
            let journal = engine.journal_stats();
            row(ui, "Commandes acceptées", control.accepted().to_string());
            row(ui, "Commandes refusées", control.rejected().to_string());
            row(
                ui,
                "Journal : lignes écrites",
                journal.written().to_string(),
            );
            counter(
                ui,
                "Journal : lignes perdues",
                journal.dropped() + journal.write_errors(),
            );
        });

    inputs(ui, engine, io);

    ui.separator();
    ui.label(RichText::new(adapter).small().weak());
}

/// One line per input with a picture feed (spec `ui.md`, « Statistiques »): state, source
/// format, capture mode, conversion, pictures received per second, repeated, skipped, dropped
/// with a busy transfer buffer (shown only when non-zero), arrival → tick latency.
fn inputs(ui: &mut egui::Ui, engine: &RenderEngine, io: &dyn IoStatus) {
    let state = engine.state().load();
    let feeds = engine.input_stats().load();
    if feeds.is_empty() {
        return;
    }
    ui.separator();
    ui.label(RichText::new("Inputs").strong());
    let now = ui.input(|i| i.time);
    egui::Grid::new("input_stats_grid")
        .num_columns(2)
        .spacing([12.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            for (id, stats) in feeds.iter() {
                let Some(input) = state.input(*id) else {
                    continue;
                };
                let s = stats.snapshot();
                let status = match input.status {
                    InputStatus::Live { source: Some(f) } => format!(
                        "{} {}{}",
                        f.resolution,
                        f.pixel,
                        f.rate.map(|r| format!(" {r}")).unwrap_or_default()
                    ),
                    InputStatus::Live { source: None } => "en direct".into(),
                    InputStatus::Starting => "démarrage".into(),
                    InputStatus::SignalLost { .. } => "signal perdu".into(),
                };
                row(ui, &input.name, status);
                if let Some(mode) = io.capture_mode(*id) {
                    row(ui, "  mode de capture", mode);
                }
                match io.converting(*id) {
                    Some(true) => row(ui, "  conversion", "videoconvert (CPU)".into()),
                    Some(false) => row(ui, "  conversion", "aucune".into()),
                    None => {}
                }
                // Received per second, from the previous sample kept in egui's memory.
                let key = egui::Id::unique(("vma.input_rate", id.0));
                let previous: Option<(u64, f64, f64)> = ui.data(|d| d.get_temp(key));
                let rate = match previous {
                    Some((count, at, rate)) if now - at < 1.0 => {
                        ui.data_mut(|d| d.insert_temp(key, (count, at, rate)));
                        rate
                    }
                    Some((count, at, _)) => {
                        let rate = (s.received - count.min(s.received)) as f64 / (now - at);
                        ui.data_mut(|d| d.insert_temp(key, (s.received, now, rate)));
                        rate
                    }
                    None => {
                        ui.data_mut(|d| d.insert_temp(key, (s.received, now, 0.0)));
                        0.0
                    }
                };
                row(
                    ui,
                    "  reçues / répétées / sautées",
                    format!("{rate:.1}/s · {} · {}", s.repeated, s.skipped),
                );
                if s.upload_busy > 0 {
                    row(ui, "  upload occupé (GPU)", s.upload_busy.to_string());
                }
                row(
                    ui,
                    "  latence p50 / p99 / max",
                    format!(
                        "{} / {} / {}",
                        ms(s.latency.p50),
                        ms(s.latency.p99),
                        ms(s.latency.max)
                    ),
                );
            }
        });
}

fn row(ui: &mut egui::Ui, label: &str, value: String) {
    ui.label(label);
    ui.label(RichText::new(value).monospace());
    ui.end_row();
}

fn counter(ui: &mut egui::Ui, label: &str, value: u64) {
    ui.label(label);
    let color = if value == 0 { OK } else { BAD };
    ui.label(
        RichText::new(value.to_string())
            .monospace()
            .strong()
            .color(color),
    );
    ui.end_row();
}

fn ms(d: Duration) -> String {
    format!("{:.3} ms", d.as_secs_f64() * 1000.0)
}
