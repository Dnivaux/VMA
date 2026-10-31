//! Statistics of the outputs (B15, spec `ui.md`, « Statistiques »): state, encoder, bit
//! rate, frames sent / lost, queue, reconnections, duration, file size. Drawn from plain data,
//! so that it is tested without an engine.

use egui::{Color32, RichText};
use vma_engine::{OutputCountersSnapshot, OutputSpec, OutputState, OutputStatus};

use crate::OutputActivity;

const OK: Color32 = Color32::from_rgb(80, 200, 120);
const BAD: Color32 = Color32::from_rgb(230, 70, 70);

/// One output, as the statistics show it.
#[derive(Debug, Clone, Copy)]
pub struct OutputStatsRow<'a> {
    pub output: &'a OutputState,
    /// Frames delivered / dropped / queued by the engine (while attached).
    pub counters: Option<OutputCountersSnapshot>,
    /// What `vma-io` reports (while it runs).
    pub activity: Option<OutputActivity>,
}

/// Every output that is not idle; nothing if all are.
pub fn output_stats(ui: &mut egui::Ui, rows: &[OutputStatsRow<'_>]) {
    let shown: Vec<_> = rows
        .iter()
        .filter(|r| r.output.status != OutputStatus::Idle)
        .collect();
    if shown.is_empty() {
        return;
    }
    ui.separator();
    ui.label(RichText::new("Sorties").strong());
    egui::Grid::new("output_stats_grid")
        .num_columns(2)
        .spacing([12.0, 4.0])
        .striped(true)
        .show(ui, |ui| {
            for row in shown {
                let o = row.output;
                let color = match o.status {
                    OutputStatus::Failed { .. } => BAD,
                    OutputStatus::Running { .. } => OK,
                    _ => ui.visuals().text_color(),
                };
                ui.label(&o.name);
                ui.label(RichText::new(status_label(&o.status)).color(color));
                ui.end_row();
                match &o.status {
                    OutputStatus::Running { encoder, target } => {
                        line(ui, "  encodeur", encoder.clone());
                        if let Some(target) = target {
                            line(ui, "  fichier", target.clone());
                        }
                    }
                    OutputStatus::Failed { reason } => line(ui, "  raison", reason.clone()),
                    _ => {}
                }
                if let Some(a) = row.activity {
                    line(ui, "  durée", duration(a.running_for.as_secs()));
                    line(ui, "  débit", a.kbps.map_or_else(|| "—".to_owned(), kbps));
                    if o.spec.is_recording() {
                        line(ui, "  taille", a.bytes.map_or_else(|| "—".to_owned(), size));
                    }
                }
                if let Some(c) = row.counters {
                    line(ui, "  images envoyées", c.delivered.to_string());
                    ui.label("  images perdues");
                    ui.label(
                        RichText::new(c.dropped.to_string())
                            .monospace()
                            .strong()
                            .color(if c.dropped == 0 { OK } else { BAD }),
                    );
                    ui.end_row();
                    line(ui, "  file d'attente", c.queued.to_string());
                }
                if o.spec.is_stream() {
                    let attempts = match o.status {
                        OutputStatus::Reconnecting { attempt } => attempt,
                        _ => 0,
                    };
                    line(ui, "  reconnexions", attempts.to_string());
                }
            }
        });
}

/// The state of an output, in a few words.
pub(crate) fn status_label(status: &OutputStatus) -> String {
    match status {
        OutputStatus::Idle => "arrêtée".into(),
        OutputStatus::Starting => "démarrage…".into(),
        OutputStatus::Running { .. } => "en cours".into(),
        OutputStatus::Reconnecting { attempt } => format!("reconnexion (essai {attempt})"),
        OutputStatus::Stopping => "finalisation…".into(),
        OutputStatus::Failed { .. } => "erreur".into(),
    }
}

/// « Enregistrement », « RTMP », « SRT ».
pub(crate) fn kind_label(spec: &OutputSpec) -> &'static str {
    match spec {
        OutputSpec::Record(_) => "Enregistrement",
        OutputSpec::Rtmp(_) => "RTMP",
        OutputSpec::Srt(_) => "SRT",
    }
}

/// `hh:mm:ss`.
pub(crate) fn duration(secs: u64) -> String {
    format!("{:02}:{:02}:{:02}", secs / 3600, secs / 60 % 60, secs % 60)
}

fn kbps(kbps: u32) -> String {
    if kbps >= 1000 {
        format!("{:.1} Mb/s", f64::from(kbps) / 1000.0).replace('.', ",")
    } else {
        format!("{kbps} kb/s")
    }
}

fn size(bytes: u64) -> String {
    let mb = bytes as f64 / 1_000_000.0;
    if mb >= 1000.0 {
        format!("{:.2} Go", mb / 1000.0).replace('.', ",")
    } else {
        format!("{mb:.1} Mo").replace('.', ",")
    }
}

fn line(ui: &mut egui::Ui, label: &str, value: String) {
    ui.label(label);
    ui.label(RichText::new(value).monospace());
    ui.end_row();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(duration(0), "00:00:00");
        assert_eq!(duration(3_723), "01:02:03");
        assert_eq!(kbps(850), "850 kb/s");
        assert_eq!(kbps(40_000), "40,0 Mb/s");
        assert_eq!(size(12_345_678), "12,3 Mo");
        assert_eq!(size(2_500_000_000), "2,50 Go");
    }
}
