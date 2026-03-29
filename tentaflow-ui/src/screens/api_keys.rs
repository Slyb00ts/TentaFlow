// =============================================================================
// API Keys — zarzadzanie kluczami API (jak web ApiKeys.js)
// =============================================================================

use eframe::egui;
use egui::{Color32, RichText};
use crate::state::{SharedAppState, UiCommand};
use crate::widgets;

pub fn ui(ctx: &egui::Context, state: &SharedAppState) {
    egui::CentralPanel::default().show(ctx, |ui| {
        let modal_open = ui.memory_mut(|m| *m.data.get_temp_mut_or_default::<bool>(egui::Id::new("apikey_modal")));

        let add_clicked = widgets::page_header_with_button(ui, "Klucze API", "+ Generuj nowy klucz");
        if add_clicked {
            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("apikey_modal"), true));
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            let s = state.read().unwrap_or_else(|e| e.into_inner());

            if s.api_keys.is_empty() {
                widgets::empty_table(ui, "Brak kluczy API. Kliknij '+ Generuj nowy klucz' aby utworzyc.");
            } else {
                egui_extras::TableBuilder::new(ui)
                    .striped(true)
                    .column(egui_extras::Column::auto().at_least(120.0))
                    .column(egui_extras::Column::auto().at_least(150.0))
                    .column(egui_extras::Column::auto().at_least(80.0))
                    .column(egui_extras::Column::auto().at_least(80.0))
                    .column(egui_extras::Column::auto().at_least(120.0))
                    .column(egui_extras::Column::auto().at_least(120.0))
                    .column(egui_extras::Column::auto().at_least(80.0))
                    .header(22.0, |mut header| {
                        header.col(|ui| { ui.strong("Prefix klucza"); });
                        header.col(|ui| { ui.strong("Nazwa"); });
                        header.col(|ui| { ui.strong("Limit RPS"); });
                        header.col(|ui| { ui.strong("Status"); });
                        header.col(|ui| { ui.strong("Utworzono"); });
                        header.col(|ui| { ui.strong("Ostatnio uzyte"); });
                        header.col(|ui| { ui.strong("Akcje"); });
                    })
                    .body(|mut body| {
                        for key in &s.api_keys {
                            body.row(26.0, |mut row| {
                                row.col(|ui| {
                                    ui.label(RichText::new(format!("{}...", key.key_prefix)).monospace().size(12.0));
                                });
                                row.col(|ui| { ui.label(&key.name); });
                                row.col(|ui| { ui.label(key.rate_limit_rps.to_string()); });
                                row.col(|ui| {
                                    let (text, color) = if key.is_active {
                                        ("Aktywny", Color32::from_rgb(34, 197, 94))
                                    } else {
                                        ("Nieaktywny", Color32::from_rgb(239, 68, 68))
                                    };
                                    widgets::badge(ui, text, color);
                                });
                                row.col(|ui| { ui.label(&key.created_at); });
                                row.col(|ui| { ui.label(key.last_used_at.as_deref().unwrap_or("-")); });
                                row.col(|ui| {
                                    if key.is_active {
                                        if ui.small_button("Dezaktywuj").clicked() {
                                            state.read().unwrap_or_else(|e| e.into_inner()).send_command(UiCommand::DeleteApiKey(key.id));
                                        }
                                    }
                                });
                            });
                        }
                    });
            }
        });

        // ── Generate modal ──
        if modal_open {
            let mut open = true;
            egui::Window::new("Generuj klucz API")
                .collapsible(false)
                .resizable(false)
                .default_width(400.0)
                .open(&mut open)
                .show(ctx, |ui| {
                    let mut name: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("ak_name")).clone());
                    let mut rps: String = ui.memory_mut(|m| {
                        let v = m.data.get_temp_mut_or_default::<String>(egui::Id::new("ak_rps")).clone();
                        if v.is_empty() { "100".to_string() } else { v }
                    });

                    ui.label("Nazwa klucza:");
                    ui.text_edit_singleline(&mut name);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("ak_name"), name));

                    ui.label("Limit zapytan na sekunde (RPS):");
                    ui.text_edit_singleline(&mut rps);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("ak_rps"), rps));

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Generuj").clicked() {
                            let name_val: String = ui.memory(|m| m.data.get_temp(egui::Id::new("ak_name")).unwrap_or_default());
                            let rps_val: String = ui.memory(|m| m.data.get_temp(egui::Id::new("ak_rps")).unwrap_or_default());
                            let rate_limit = rps_val.parse::<i64>().unwrap_or(100);
                            state.read().unwrap_or_else(|e| e.into_inner()).send_command(UiCommand::CreateApiKey {
                                name: name_val,
                                rate_limit_rps: rate_limit,
                            });
                            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("apikey_modal"), false));
                        }
                        if ui.button("Anuluj").clicked() {
                            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("apikey_modal"), false));
                        }
                    });
                });
            if !open {
                ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("apikey_modal"), false));
            }
        }
    });
}
