// =============================================================================
// Services — zarzadzanie serwisami AI (CRUD, status QUIC)
// Odpowiada: web Services.js
// =============================================================================

use eframe::egui;
use egui::{Color32, RichText};
use crate::state::{SharedAppState, QuicStatus, ServiceType, UiCommand};
use crate::widgets;

pub fn ui(ctx: &egui::Context, state: &SharedAppState) {
    egui::CentralPanel::default().show(ctx, |ui| {
        let modal_open = ui.memory_mut(|m| *m.data.get_temp_mut_or_default::<bool>(egui::Id::new("svc_modal")));

        let add_clicked = widgets::page_header_with_button(ui, "Serwisy", "+ Dodaj serwis");
        if add_clicked {
            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("svc_modal"), true));
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            let s = state.read().unwrap_or_else(|e| e.into_inner());

            if s.services.is_empty() {
                widgets::empty_table(ui, "Brak zarejestrowanych serwisow. Kliknij '+ Dodaj serwis' aby dodac.");
            } else {
                egui_extras::TableBuilder::new(ui)
                    .striped(true)
                    .column(egui_extras::Column::auto().at_least(150.0))
                    .column(egui_extras::Column::auto().at_least(80.0))
                    .column(egui_extras::Column::auto().at_least(180.0))
                    .column(egui_extras::Column::auto().at_least(120.0))
                    .column(egui_extras::Column::auto().at_least(120.0))
                    .column(egui_extras::Column::auto().at_least(80.0))
                    .header(22.0, |mut header| {
                        header.col(|ui| { ui.strong("Nazwa"); });
                        header.col(|ui| { ui.strong("Typ"); });
                        header.col(|ui| { ui.strong("Adres QUIC"); });
                        header.col(|ui| { ui.strong("Status QUIC"); });
                        header.col(|ui| { ui.strong("Utworzono"); });
                        header.col(|ui| { ui.strong("Akcje"); });
                    })
                    .body(|mut body| {
                        for svc in &s.services {
                            body.row(28.0, |mut row| {
                                row.col(|ui| { ui.label(&svc.name); });
                                row.col(|ui| {
                                    widgets::badge(ui, &svc.service_type.to_string(), svc_type_color(&svc.service_type));
                                });
                                row.col(|ui| {
                                    ui.label(RichText::new(&svc.quic_address).monospace().size(12.0));
                                });
                                row.col(|ui| {
                                    let (color, label) = quic_display(&svc.quic_status);
                                    widgets::status_badge(ui, label, color);
                                });
                                row.col(|ui| { ui.label(svc.created_at.as_deref().unwrap_or("-")); });
                                row.col(|ui| {
                                    ui.horizontal(|ui| {
                                        ui.small_button("\u{270F}").on_hover_text("Edytuj");
                                        if ui.small_button("\u{2716}").on_hover_text("Usun").clicked() {
                                            state.read().unwrap_or_else(|e| e.into_inner()).send_command(
                                                UiCommand::DeleteService(svc.id),
                                            );
                                        }
                                    });
                                });
                            });
                        }
                    });
            }
        });

        // ── Add modal ──
        if modal_open {
            let mut open = true;
            egui::Window::new("Dodaj serwis")
                .collapsible(false)
                .resizable(false)
                .default_width(400.0)
                .open(&mut open)
                .show(ctx, |ui| {
                    let mut name: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("svc_name")).clone());
                    let mut addr: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("svc_addr")).clone());

                    ui.label("Nazwa serwisu:");
                    ui.text_edit_singleline(&mut name);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("svc_name"), name));

                    ui.label("Adres QUIC:");
                    ui.text_edit_singleline(&mut addr);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("svc_addr"), addr));

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Dodaj").clicked() {
                            let svc_name: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("svc_name")).clone());
                            let svc_addr: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("svc_addr")).clone());
                            state.read().unwrap_or_else(|e| e.into_inner()).send_command(
                                UiCommand::CreateService {
                                    name: svc_name,
                                    service_type: String::from("Llm"),
                                    strategy: String::from("round_robin"),
                                    config_json: format!("{{\"quic_address\":\"{}\"}}", svc_addr),
                                },
                            );
                            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("svc_name"), String::new()));
                            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("svc_addr"), String::new()));
                            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("svc_modal"), false));
                        }
                        if ui.button("Anuluj").clicked() {
                            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("svc_modal"), false));
                        }
                    });
                });
            if !open {
                ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("svc_modal"), false));
            }
        }
    });
}

fn svc_type_color(st: &ServiceType) -> Color32 {
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

fn quic_display(status: &QuicStatus) -> (Color32, &'static str) {
    match status {
        QuicStatus::Connected => (Color32::from_rgb(34, 197, 94), "Connected"),
        QuicStatus::Connecting => (Color32::from_rgb(234, 179, 8), "Connecting"),
        QuicStatus::Disconnected => (Color32::from_rgb(239, 68, 68), "Disconnected"),
        QuicStatus::ConfigError => (Color32::from_rgb(108, 112, 134), "Config Error"),
    }
}
