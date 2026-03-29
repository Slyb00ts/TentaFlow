// =============================================================================
// Models — rejestr modeli + aliasy (jak web Models.js)
// =============================================================================

use eframe::egui;
use egui::{Color32, RichText};
use crate::state::{SharedAppState, ServiceType, UiCommand};
use crate::widgets;

pub fn ui(ctx: &egui::Context, state: &SharedAppState) {
    egui::CentralPanel::default().show(ctx, |ui| {
        let modal_open = ui.memory_mut(|m| *m.data.get_temp_mut_or_default::<bool>(egui::Id::new("model_modal")));

        let add_clicked = widgets::page_header_with_button(ui, "Modele", "+ Dodaj model");
        if add_clicked {
            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("model_modal"), true));
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            let s = state.read().unwrap_or_else(|e| e.into_inner());

            // ── Models table ──
            widgets::section_header(ui, "Rejestr modeli");

            if s.models.is_empty() {
                widgets::empty_table(ui, "Brak zarejestrowanych modeli");
            } else {
                egui_extras::TableBuilder::new(ui)
                    .striped(true)
                    .column(egui_extras::Column::auto().at_least(140.0))
                    .column(egui_extras::Column::auto().at_least(120.0))
                    .column(egui_extras::Column::auto().at_least(80.0))
                    .column(egui_extras::Column::auto().at_least(90.0))
                    .column(egui_extras::Column::auto().at_least(60.0))
                    .column(egui_extras::Column::auto().at_least(60.0))
                    .column(egui_extras::Column::auto().at_least(60.0))
                    .column(egui_extras::Column::auto().at_least(80.0))
                    .header(22.0, |mut header| {
                        header.col(|ui| { ui.strong("Nazwa"); });
                        header.col(|ui| { ui.strong("Display Name"); });
                        header.col(|ui| { ui.strong("Typ"); });
                        header.col(|ui| { ui.strong("Strategia"); });
                        header.col(|ui| { ui.strong("Serwisy"); });
                        header.col(|ui| { ui.strong("Publiczny"); });
                        header.col(|ui| { ui.strong("Aktywny"); });
                        header.col(|ui| { ui.strong("Akcje"); });
                    })
                    .body(|mut body| {
                        for model in &s.models {
                            body.row(28.0, |mut row| {
                                row.col(|ui| { ui.label(&model.name); });
                                row.col(|ui| { ui.label(&model.display_name); });
                                row.col(|ui| {
                                    widgets::badge(ui, &model.service_type.to_string(), svc_type_color(&model.service_type));
                                });
                                row.col(|ui| { ui.label(&model.strategy); });
                                row.col(|ui| {
                                    widgets::badge(ui, &model.service_count.to_string(), Color32::from_rgb(99, 102, 241));
                                });
                                row.col(|ui| {
                                    let (text, color) = if model.is_public {
                                        ("Tak", Color32::from_rgb(34, 197, 94))
                                    } else {
                                        ("Nie", Color32::from_rgb(108, 112, 134))
                                    };
                                    ui.colored_label(color, text);
                                });
                                row.col(|ui| {
                                    let (text, color) = if model.is_active {
                                        ("Aktywny", Color32::from_rgb(34, 197, 94))
                                    } else {
                                        ("Nieaktywny", Color32::from_rgb(239, 68, 68))
                                    };
                                    widgets::badge(ui, text, color);
                                });
                                row.col(|ui| {
                                    let model_id = model.id;
                                    ui.horizontal(|ui| {
                                        ui.small_button("\u{270F}").on_hover_text("Edytuj");
                                        if ui.small_button("\u{2716}").on_hover_text("Usun").clicked() {
                                            state.read().unwrap_or_else(|e| e.into_inner()).send_command(
                                                UiCommand::DeleteModelEntry(model_id),
                                            );
                                        }
                                    });
                                });
                            });
                        }
                    });
            }

            // ── Model aliases ──
            ui.add_space(16.0);
            widgets::section_header(ui, "Aliasy modeli");

            if s.model_aliases.is_empty() {
                widgets::empty_table(ui, "Brak aliasow");
            } else {
                egui_extras::TableBuilder::new(ui)
                    .striped(true)
                    .column(egui_extras::Column::auto().at_least(150.0))
                    .column(egui_extras::Column::auto().at_least(150.0))
                    .column(egui_extras::Column::auto().at_least(80.0))
                    .column(egui_extras::Column::auto().at_least(80.0))
                    .header(22.0, |mut header| {
                        header.col(|ui| { ui.strong("Alias"); });
                        header.col(|ui| { ui.strong("Model docelowy"); });
                        header.col(|ui| { ui.strong("Status"); });
                        header.col(|ui| { ui.strong("Akcje"); });
                    })
                    .body(|mut body| {
                        for alias in &s.model_aliases {
                            body.row(26.0, |mut row| {
                                row.col(|ui| { ui.label(RichText::new(&alias.alias).monospace()); });
                                row.col(|ui| { ui.label(&alias.target_model); });
                                row.col(|ui| {
                                    let (text, color) = if alias.is_active {
                                        ("Aktywny", Color32::from_rgb(34, 197, 94))
                                    } else {
                                        ("Nieaktywny", Color32::from_rgb(239, 68, 68))
                                    };
                                    widgets::badge(ui, text, color);
                                });
                                row.col(|ui| {
                                    let alias_id = alias.id;
                                    ui.horizontal(|ui| {
                                        ui.small_button("\u{270F}");
                                        if ui.small_button("\u{2716}").on_hover_text("Usun").clicked() {
                                            state.read().unwrap_or_else(|e| e.into_inner()).send_command(
                                                UiCommand::DeleteModelAlias(alias_id),
                                            );
                                        }
                                    });
                                });
                            });
                        }
                    });
            }

            // ── Pobieranie modelu z HuggingFace ──
            ui.add_space(16.0);
            widgets::section_header(ui, "Pobierz model z HuggingFace");
            drop(s);

            ui.horizontal(|ui| {
                let mut hf_model_id: String = ui.memory_mut(|m| {
                    m.data.get_temp_mut_or_default::<String>(egui::Id::new("hf_model_id")).clone()
                });
                ui.label("Model ID:");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut hf_model_id)
                        .hint_text("np. mlx-community/Llama-3-8B-4bit")
                        .desired_width(300.0),
                );
                if response.changed() {
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("hf_model_id"), hf_model_id.clone()));
                }

                if ui.button("Pobierz").clicked() && !hf_model_id.trim().is_empty() {
                    state.read().unwrap_or_else(|e| e.into_inner()).send_command(
                        UiCommand::DownloadModel { model_id: hf_model_id.trim().to_string() },
                    );
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("hf_model_id"), String::new()));
                }
            });

            let s = state.read().unwrap_or_else(|e| e.into_inner());

            // ── Postep pobierania ──
            if !s.download_progress.is_empty() {
                ui.add_space(8.0);
                widgets::section_header(ui, "Pobieranie modeli");
                for (name, progress) in &s.download_progress {
                    ui.horizontal(|ui| {
                        ui.label(name);
                        ui.add(egui::ProgressBar::new(*progress).show_percentage());
                    });
                }
            }

            // ── Lokalne modele ──
            ui.add_space(16.0);
            widgets::section_header(ui, "Lokalne modele");

            if s.local_models.is_empty() {
                widgets::empty_table(ui, "Brak lokalnych modeli — pobierz model z HuggingFace");
            } else {
                // Zbierz dane do uzycia po dropie locka
                let models_snapshot: Vec<_> = s.local_models.clone();
                drop(s);

                for lm in &models_snapshot {
                    ui.horizontal(|ui| {
                        let status_color = if lm.loaded {
                            Color32::from_rgb(34, 197, 94)
                        } else {
                            Color32::from_rgb(108, 112, 134)
                        };
                        widgets::status_badge(
                            ui,
                            &format!(
                                "{} ({}, {} MB){}",
                                lm.name,
                                if lm.format.is_empty() { "?" } else { &lm.format },
                                lm.size_mb,
                                if lm.loaded {
                                    format!(" — {:.1} tok/s, {} MB VRAM", lm.tokens_per_second, lm.vram_used_mb)
                                } else {
                                    String::new()
                                },
                            ),
                            status_color,
                        );

                        if lm.loaded {
                            if ui.button("Wyladuj").clicked() {
                                state.read().unwrap_or_else(|e| e.into_inner()).send_command(
                                    UiCommand::UnloadModel,
                                );
                            }
                        } else {
                            if ui.button("Zaladuj").clicked() {
                                state.read().unwrap_or_else(|e| e.into_inner()).send_command(
                                    UiCommand::LoadModel { path: lm.path.clone() },
                                );
                            }
                        }

                        if ui.small_button("\u{2716}").on_hover_text("Usun z dysku").clicked() {
                            state.read().unwrap_or_else(|e| e.into_inner()).send_command(
                                UiCommand::DeleteLocalModel { path: lm.path.clone() },
                            );
                        }
                    });
                }
            }
        });

        // ── Add modal ──
        if modal_open {
            let mut open = true;
            egui::Window::new("Dodaj model")
                .collapsible(false)
                .resizable(false)
                .default_width(400.0)
                .open(&mut open)
                .show(ctx, |ui| {
                    let mut name: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("mdl_name")).clone());
                    ui.label("Nazwa modelu lub URL HuggingFace:");
                    ui.text_edit_singleline(&mut name);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("mdl_name"), name));

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Dodaj").clicked() {
                            let name_val: String = ui.memory(|m| m.data.get_temp(egui::Id::new("mdl_name")).unwrap_or_default());
                            state.read().unwrap_or_else(|e| e.into_inner()).send_command(
                                UiCommand::CreateModelEntry {
                                    model_name: name_val,
                                    display_name: String::new(),
                                    service_type: String::from("Llm"),
                                    connection_type: String::from("direct"),
                                    is_public: false,
                                    config_json: String::from("{}"),
                                },
                            );
                            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("model_modal"), false));
                        }
                        if ui.button("Anuluj").clicked() {
                            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("model_modal"), false));
                        }
                    });
                });
            if !open {
                ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("model_modal"), false));
            }
        }
    });
}

fn svc_type_color(st: &ServiceType) -> Color32 {
    match st {
        ServiceType::Llm => Color32::from_rgb(99, 102, 241),
        ServiceType::Tts => Color32::from_rgb(168, 85, 247),
        ServiceType::Stt => Color32::from_rgb(236, 72, 153),
        ServiceType::Rag => Color32::from_rgb(14, 165, 233),
        ServiceType::Embedding => Color32::from_rgb(20, 184, 166),
        ServiceType::Vision => Color32::from_rgb(245, 158, 11),
        ServiceType::Router => Color32::from_rgb(34, 197, 94),
        ServiceType::Memory => Color32::from_rgb(244, 63, 94),
        ServiceType::Reranker => Color32::from_rgb(139, 92, 246),
    }
}
