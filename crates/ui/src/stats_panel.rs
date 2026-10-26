use egui::{Color32, RichText};
use std::time::Duration;
use vma_engine::{FrameReader, RenderEngine};

const OK: Color32 = Color32::from_rgb(80, 200, 120);
const BAD: Color32 = Color32::from_rgb(230, 70, 70);

pub(crate) fn show(ui: &mut egui::Ui, engine: &RenderEngine, reader: &FrameReader, adapter: &str) {
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

    ui.separator();
    ui.label(RichText::new(adapter).small().weak());
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
