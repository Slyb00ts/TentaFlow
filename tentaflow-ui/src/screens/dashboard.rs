// =============================================================================
// Dashboard — metryki w czasie rzeczywistym, wykres tokens/sec, tabela serwisow
// Odpowiada: web Dashboard.js
// =============================================================================

use eframe::egui;
use egui::{Color32, RichText, Vec2};
use crate::state::SharedAppState;
use crate::theme::Theme;
use crate::widgets;

pub fn ui(ctx: &egui::Context, state: &SharedAppState, theme: &Theme) {
    egui::CentralPanel::default().show(ctx, |ui| {
        egui::ScrollArea::vertical().show(ui, |ui| {
            let s = state.read().unwrap_or_else(|e| e.into_inner());
            let m = &s.metrics;

            // ── Metric cards ──
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                widgets::metric_card(
                    ui,
                    "Tokens In/s",
                    &format!("{:.1}", m.tokens_in_per_sec),
                    theme.accent,
                );
                widgets::metric_card(
                    ui,
                    "Tokens Out/s",
                    &format!("{:.1}", m.tokens_out_per_sec),
                    theme.accent,
                );
                widgets::metric_card(
                    ui,
                    "Aktywne zapytania",
                    &m.active_requests.to_string(),
                    Color32::from_rgb(168, 85, 247),
                );
                widgets::metric_card(
                    ui,
                    "Srednia latencja",
                    &format!("{:.0}ms", m.avg_latency_ms),
                    Color32::from_rgb(14, 165, 233),
                );
            });
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                widgets::metric_card(
                    ui,
                    "Aktywne serwisy",
                    &m.active_services.to_string(),
                    theme.success,
                );
                widgets::metric_card(
                    ui,
                    "Suma zapytan",
                    &m.total_requests.to_string(),
                    Color32::from_rgb(166, 173, 200),
                );
                widgets::metric_card(
                    ui,
                    "Tokeny (In/Out)",
                    &format!("{}/{}", m.total_input_tokens, m.total_output_tokens),
                    Color32::from_rgb(166, 173, 200),
                );
                widgets::metric_card(
                    ui,
                    "Bledy",
                    &m.total_errors.to_string(),
                    if m.total_errors > 0 { theme.error } else { Color32::from_rgb(108, 112, 134) },
                );
            });

            // ── Chart ──
            ui.add_space(16.0);
            widgets::section_header(ui, "Tokens/s");
            let history = m.tokens_history.clone();
            drop(s);

            if history.is_empty() {
                ui.label(RichText::new("Brak danych — oczekiwanie na metryki...").color(Color32::from_rgb(108, 112, 134)));
            } else {
                let chart_width = ui.available_width().min(900.0);
                widgets::sparkline_chart(ui, &history, theme.accent, Vec2::new(chart_width, 200.0));
            }

            // ── Services table ──
            ui.add_space(16.0);
            widgets::section_header(ui, "Przeglad serwisow");

            let s = state.read().unwrap_or_else(|e| e.into_inner());
            if s.services.is_empty() {
                widgets::empty_table(ui, "Brak zarejestrowanych serwisow");
            } else {
                egui_extras::TableBuilder::new(ui)
                    .striped(true)
                    .column(egui_extras::Column::auto().at_least(150.0))
                    .column(egui_extras::Column::auto().at_least(80.0))
                    .column(egui_extras::Column::auto().at_least(80.0))
                    .column(egui_extras::Column::auto().at_least(100.0))
                    .column(egui_extras::Column::auto().at_least(60.0))
                    .column(egui_extras::Column::auto().at_least(80.0))
                    .header(22.0, |mut header| {
                        header.col(|ui| { ui.strong("Nazwa"); });
                        header.col(|ui| { ui.strong("Typ"); });
                        header.col(|ui| { ui.strong("Status"); });
                        header.col(|ui| { ui.strong("Strategia"); });
                        header.col(|ui| { ui.strong("Backendy"); });
                        header.col(|ui| { ui.strong("Latencja"); });
                    })
                    .body(|mut body| {
                        for svc in &s.services {
                            body.row(24.0, |mut row| {
                                row.col(|ui| { ui.label(&svc.name); });
                                row.col(|ui| {
                                    widgets::badge(ui, &svc.service_type.to_string(), service_type_color(&svc.service_type));
                                });
                                row.col(|ui| {
                                    let (color, label) = status_display(&svc.status);
                                    widgets::status_badge(ui, label, color);
                                });
                                row.col(|ui| { ui.label(&svc.strategy); });
                                row.col(|ui| { ui.label(svc.backends.len().to_string()); });
                                row.col(|ui| { ui.label(format!("{:.0}ms", svc.avg_latency_ms)); });
                            });
                        }
                    });
            }
        });
    });
}

fn service_type_color(st: &crate::state::ServiceType) -> Color32 {
    use crate::state::ServiceType;
    match st {
        ServiceType::Llm => Color32::from_rgb(99, 102, 241),
        ServiceType::Tts => Color32::from_rgb(168, 85, 247),
        ServiceType::Stt => Color32::from_rgb(236, 72, 153),
        ServiceType::Embedding => Color32::from_rgb(20, 184, 166),
        ServiceType::Vision => Color32::from_rgb(245, 158, 11),
        ServiceType::Router => Color32::from_rgb(34, 197, 94),
        ServiceType::Memory => Color32::from_rgb(244, 63, 94),
        ServiceType::Reranker => Color32::from_rgb(139, 92, 246),
    }
}

fn status_display(status: &crate::state::ServiceStatus) -> (Color32, &'static str) {
    use crate::state::ServiceStatus;
    match status {
        ServiceStatus::Running => (Color32::from_rgb(34, 197, 94), "Aktywny"),
        ServiceStatus::Stopped => (Color32::from_rgb(108, 112, 134), "Zatrzymany"),
        ServiceStatus::Error => (Color32::from_rgb(239, 68, 68), "Blad"),
        ServiceStatus::Starting => (Color32::from_rgb(234, 179, 8), "Uruchamianie"),
    }
}
