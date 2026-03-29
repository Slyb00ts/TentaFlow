// =============================================================================
// Flow Builder — lista flow + podglad (jak web FlowBuilder.js)
// =============================================================================

use eframe::egui;
use egui::{Color32, RichText};
use crate::state::{SharedAppState, FlowStatus, UiCommand};
use crate::widgets;

pub fn ui(ctx: &egui::Context, state: &SharedAppState) {
    egui::CentralPanel::default().show(ctx, |ui| {
        let modal_open = ui.memory_mut(|m| *m.data.get_temp_mut_or_default::<bool>(egui::Id::new("flow_modal")));

        let add_clicked = widgets::page_header_with_button(ui, "Flow Builder", "+ Nowy flow");
        if add_clicked {
            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("flow_modal"), true));
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            let s = state.read().unwrap_or_else(|e| e.into_inner());

            if s.flows.is_empty() {
                widgets::empty_table(ui, "Brak zdefiniowanych flow. Kliknij '+ Nowy flow' aby utworzyc.");
            } else {
                egui_extras::TableBuilder::new(ui)
                    .striped(true)
                    .column(egui_extras::Column::auto().at_least(180.0))
                    .column(egui_extras::Column::auto().at_least(150.0))
                    .column(egui_extras::Column::auto().at_least(80.0))
                    .column(egui_extras::Column::auto().at_least(80.0))
                    .column(egui_extras::Column::auto().at_least(140.0))
                    .column(egui_extras::Column::auto().at_least(80.0))
                    .header(22.0, |mut header| {
                        header.col(|ui| { ui.strong("Nazwa"); });
                        header.col(|ui| { ui.strong("Opis"); });
                        header.col(|ui| { ui.strong("Typ"); });
                        header.col(|ui| { ui.strong("Status"); });
                        header.col(|ui| { ui.strong("Ostatnie uruchomienie"); });
                        header.col(|ui| { ui.strong("Akcje"); });
                    })
                    .body(|mut body| {
                        for flow in &s.flows {
                            body.row(28.0, |mut row| {
                                row.col(|ui| { ui.label(&flow.name); });
                                row.col(|ui| {
                                    let desc = if flow.description.len() > 40 {
                                        format!("{}...", &flow.description[..40])
                                    } else {
                                        flow.description.clone()
                                    };
                                    ui.label(RichText::new(desc).size(12.0));
                                });
                                row.col(|ui| { ui.label(&flow.service_type); });
                                row.col(|ui| {
                                    let (text, color) = flow_status_display(&flow.status);
                                    widgets::badge(ui, text, color);
                                });
                                row.col(|ui| {
                                    let text = flow.last_run
                                        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                                        .unwrap_or_else(|| "-".to_string());
                                    ui.label(text);
                                });
                                row.col(|ui| {
                                    ui.horizontal(|ui| {
                                        ui.small_button("\u{270F}").on_hover_text("Edytuj");
                                        if ui.small_button("\u{2716}").on_hover_text("Usun").clicked() {
                                            let cmd = UiCommand::DeleteFlow(flow.id);
                                            state.read().unwrap_or_else(|e| e.into_inner()).send_command(cmd);
                                        }
                                    });
                                });
                            });
                        }
                    });
            }

            // ── Visual flow preview ──
            let selected_flow = ui.memory_mut(|m| *m.data.get_temp_mut_or_default::<i64>(egui::Id::new("flow_preview_id")));
            if selected_flow > 0 {
                if let Some(flow) = s.flows.iter().find(|f| f.id == selected_flow) {
                    ui.add_space(16.0);
                    widgets::section_header(ui, &format!("Podglad: {}", flow.name));
                    ui.label(&flow.description);
                    ui.add_space(8.0);

                    // Simplified pipeline visualization
                    ui.horizontal(|ui| {
                        let steps = ["Input", "System Prompt", "LLM", "Output Parser", "Output"];
                        for (i, step) in steps.iter().enumerate() {
                            let frame = egui::Frame::none()
                                .fill(Color32::from_rgb(30, 30, 46))
                                .rounding(egui::Rounding::same(6.0))
                                .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                                .stroke(egui::Stroke::new(1.0, Color32::from_rgb(99, 102, 241)));
                            frame.show(ui, |ui| {
                                ui.label(RichText::new(*step).size(12.0));
                            });
                            if i < steps.len() - 1 {
                                ui.label(RichText::new("\u{2192}").size(16.0));
                            }
                        }
                    });
                }
            }
        });

        // ── New flow modal ──
        if modal_open {
            let mut open = true;
            egui::Window::new("Nowy flow")
                .collapsible(false)
                .resizable(false)
                .default_width(400.0)
                .open(&mut open)
                .show(ctx, |ui| {
                    let mut name: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("flow_name")).clone());
                    let mut desc: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("flow_desc")).clone());

                    ui.label("Nazwa flow:");
                    ui.text_edit_singleline(&mut name);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("flow_name"), name));

                    ui.label("Opis:");
                    ui.text_edit_singleline(&mut desc);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("flow_desc"), desc));

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Utworz").clicked() {
                            let name = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("flow_name")).clone());
                            let desc = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("flow_desc")).clone());
                            let cmd = UiCommand::CreateFlow {
                                name,
                                description: desc,
                                service_type: "default".to_string(),
                                flow_json: "{}".to_string(),
                            };
                            state.read().unwrap_or_else(|e| e.into_inner()).send_command(cmd);
                            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("flow_modal"), false));
                        }
                        if ui.button("Anuluj").clicked() {
                            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("flow_modal"), false));
                        }
                    });
                });
            if !open {
                ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("flow_modal"), false));
            }
        }
    });
}

fn flow_status_display(status: &FlowStatus) -> (&'static str, Color32) {
    match status {
        FlowStatus::Active => ("Aktywny", Color32::from_rgb(34, 197, 94)),
        FlowStatus::Inactive => ("Nieaktywny", Color32::from_rgb(108, 112, 134)),
        FlowStatus::Failed => ("Blad", Color32::from_rgb(239, 68, 68)),
        FlowStatus::Draft => ("Szkic", Color32::from_rgb(234, 179, 8)),
        FlowStatus::Archived => ("Archiwum", Color32::from_rgb(108, 112, 134)),
    }
}
