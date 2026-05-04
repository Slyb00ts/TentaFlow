// =============================================================================
// Rules — reguly PII, Fast Path, TTS Cleaning (jak web Rules tabs)
// =============================================================================

use eframe::egui;
use egui::{Color32, RichText};
use crate::state::{SharedAppState, UiCommand};
use crate::widgets;

#[derive(Debug, Clone, Copy, PartialEq)]
enum RulesTab {
    Pii,
    FastPath,
    TtsCleaning,
}

pub fn ui(ctx: &egui::Context, state: &SharedAppState) {
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.heading("Reguly");
        ui.add_space(4.0);

        // Tab selector
        let current_tab = ui.memory_mut(|m| *m.data.get_temp_mut_or_default::<u8>(egui::Id::new("rules_tab")));
        let tab = match current_tab {
            1 => RulesTab::FastPath,
            2 => RulesTab::TtsCleaning,
            _ => RulesTab::Pii,
        };

        ui.horizontal(|ui| {
            if ui.selectable_label(tab == RulesTab::Pii, "PII Rules").clicked() {
                ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("rules_tab"), 0u8));
            }
            if ui.selectable_label(tab == RulesTab::FastPath, "Fast Path").clicked() {
                ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("rules_tab"), 1u8));
            }
            if ui.selectable_label(tab == RulesTab::TtsCleaning, "TTS Cleaning").clicked() {
                ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("rules_tab"), 2u8));
            }
        });
        ui.separator();

        egui::ScrollArea::vertical().show(ui, |ui| {
            match tab {
                RulesTab::Pii => pii_tab(ui, state),
                RulesTab::FastPath => fast_path_tab(ui, state),
                RulesTab::TtsCleaning => tts_cleaning_tab(ui, state),
            }
        });

        // ── PII Create modal ──
        let pii_modal_open = ui.memory_mut(|m| *m.data.get_temp_mut_or_default::<bool>(egui::Id::new("pii_modal")));
        if pii_modal_open {
            let mut open = true;
            egui::Window::new("Dodaj regule PII")
                .collapsible(false)
                .resizable(false)
                .default_width(400.0)
                .open(&mut open)
                .show(ctx, |ui| {
                    let mut name: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("pii_new_name")).clone());
                    let mut category: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("pii_new_category")).clone());
                    let mut pattern: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("pii_new_pattern")).clone());
                    let mut replacement: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("pii_new_replacement")).clone());
                    let mut priority: String = ui.memory_mut(|m| {
                        let v = m.data.get_temp_mut_or_default::<String>(egui::Id::new("pii_new_priority")).clone();
                        if v.is_empty() { "0".to_string() } else { v }
                    });

                    ui.label("Nazwa:");
                    ui.text_edit_singleline(&mut name);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("pii_new_name"), name));

                    ui.label("Kategoria:");
                    ui.text_edit_singleline(&mut category);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("pii_new_category"), category));

                    ui.label("Wzorzec (regex):");
                    ui.text_edit_singleline(&mut pattern);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("pii_new_pattern"), pattern));

                    ui.label("Zamiennik:");
                    ui.text_edit_singleline(&mut replacement);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("pii_new_replacement"), replacement));

                    ui.label("Priorytet:");
                    ui.text_edit_singleline(&mut priority);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("pii_new_priority"), priority));

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Dodaj").clicked() {
                            let name_val: String = ui.memory(|m| m.data.get_temp(egui::Id::new("pii_new_name")).unwrap_or_default());
                            let category_val: String = ui.memory(|m| m.data.get_temp(egui::Id::new("pii_new_category")).unwrap_or_default());
                            let pattern_val: String = ui.memory(|m| m.data.get_temp(egui::Id::new("pii_new_pattern")).unwrap_or_default());
                            let replacement_val: String = ui.memory(|m| m.data.get_temp(egui::Id::new("pii_new_replacement")).unwrap_or_default());
                            let priority_val: String = ui.memory(|m| m.data.get_temp(egui::Id::new("pii_new_priority")).unwrap_or_default());
                            let priority_num = priority_val.parse::<i64>().unwrap_or(0);
                            state.read().unwrap_or_else(|e| e.into_inner()).send_command(UiCommand::CreatePiiRule {
                                name: name_val,
                                category: category_val,
                                pattern: pattern_val,
                                replacement: replacement_val,
                                priority: priority_num,
                            });
                            // Clear form fields
                            ui.memory_mut(|m| {
                                m.data.insert_temp(egui::Id::new("pii_new_name"), String::new());
                                m.data.insert_temp(egui::Id::new("pii_new_category"), String::new());
                                m.data.insert_temp(egui::Id::new("pii_new_pattern"), String::new());
                                m.data.insert_temp(egui::Id::new("pii_new_replacement"), String::new());
                                m.data.insert_temp(egui::Id::new("pii_new_priority"), String::new());
                                m.data.insert_temp(egui::Id::new("pii_modal"), false);
                            });
                        }
                        if ui.button("Anuluj").clicked() {
                            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("pii_modal"), false));
                        }
                    });
                });
            if !open {
                ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("pii_modal"), false));
            }
        }

        // ── Fast Path Create modal ──
        let fp_modal_open = ui.memory_mut(|m| *m.data.get_temp_mut_or_default::<bool>(egui::Id::new("fp_modal")));
        if fp_modal_open {
            let mut open = true;
            egui::Window::new("Dodaj wzorzec Fast Path")
                .collapsible(false)
                .resizable(false)
                .default_width(400.0)
                .open(&mut open)
                .show(ctx, |ui| {
                    let mut module: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("fp_new_module")).clone());
                    let mut pattern_type: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("fp_new_pattern_type")).clone());
                    let mut pattern: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("fp_new_pattern")).clone());
                    let mut match_type: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("fp_new_match_type")).clone());
                    let mut result_json: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("fp_new_result_json")).clone());
                    let mut priority: String = ui.memory_mut(|m| {
                        let v = m.data.get_temp_mut_or_default::<String>(egui::Id::new("fp_new_priority")).clone();
                        if v.is_empty() { "0".to_string() } else { v }
                    });

                    ui.label("Modul:");
                    ui.text_edit_singleline(&mut module);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("fp_new_module"), module));

                    ui.label("Typ wzorca:");
                    ui.text_edit_singleline(&mut pattern_type);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("fp_new_pattern_type"), pattern_type));

                    ui.label("Wzorzec:");
                    ui.text_edit_singleline(&mut pattern);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("fp_new_pattern"), pattern));

                    ui.label("Typ dopasowania:");
                    ui.text_edit_singleline(&mut match_type);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("fp_new_match_type"), match_type));

                    ui.label("Wynik (JSON):");
                    ui.text_edit_singleline(&mut result_json);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("fp_new_result_json"), result_json));

                    ui.label("Priorytet:");
                    ui.text_edit_singleline(&mut priority);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("fp_new_priority"), priority));

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Dodaj").clicked() {
                            let module_val: String = ui.memory(|m| m.data.get_temp(egui::Id::new("fp_new_module")).unwrap_or_default());
                            let pattern_type_val: String = ui.memory(|m| m.data.get_temp(egui::Id::new("fp_new_pattern_type")).unwrap_or_default());
                            let pattern_val: String = ui.memory(|m| m.data.get_temp(egui::Id::new("fp_new_pattern")).unwrap_or_default());
                            let match_type_val: String = ui.memory(|m| m.data.get_temp(egui::Id::new("fp_new_match_type")).unwrap_or_default());
                            let result_json_val: String = ui.memory(|m| m.data.get_temp(egui::Id::new("fp_new_result_json")).unwrap_or_default());
                            let priority_val: String = ui.memory(|m| m.data.get_temp(egui::Id::new("fp_new_priority")).unwrap_or_default());
                            let priority_num = priority_val.parse::<i64>().unwrap_or(0);
                            state.read().unwrap_or_else(|e| e.into_inner()).send_command(UiCommand::CreateFastPath {
                                module: module_val,
                                pattern_type: pattern_type_val,
                                pattern: pattern_val,
                                match_type: match_type_val,
                                result_json: result_json_val,
                                priority: priority_num,
                            });
                            ui.memory_mut(|m| {
                                m.data.insert_temp(egui::Id::new("fp_new_module"), String::new());
                                m.data.insert_temp(egui::Id::new("fp_new_pattern_type"), String::new());
                                m.data.insert_temp(egui::Id::new("fp_new_pattern"), String::new());
                                m.data.insert_temp(egui::Id::new("fp_new_match_type"), String::new());
                                m.data.insert_temp(egui::Id::new("fp_new_result_json"), String::new());
                                m.data.insert_temp(egui::Id::new("fp_new_priority"), String::new());
                                m.data.insert_temp(egui::Id::new("fp_modal"), false);
                            });
                        }
                        if ui.button("Anuluj").clicked() {
                            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("fp_modal"), false));
                        }
                    });
                });
            if !open {
                ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("fp_modal"), false));
            }
        }

        // ── TTS Create modal ──
        let tts_modal_open = ui.memory_mut(|m| *m.data.get_temp_mut_or_default::<bool>(egui::Id::new("tts_modal")));
        if tts_modal_open {
            let mut open = true;
            egui::Window::new("Dodaj regule TTS")
                .collapsible(false)
                .resizable(false)
                .default_width(400.0)
                .open(&mut open)
                .show(ctx, |ui| {
                    let mut rule_type: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("tts_new_rule_type")).clone());
                    let mut pattern: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("tts_new_pattern")).clone());
                    let mut replacement: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("tts_new_replacement")).clone());
                    let mut language: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("tts_new_language")).clone());
                    let mut priority: String = ui.memory_mut(|m| {
                        let v = m.data.get_temp_mut_or_default::<String>(egui::Id::new("tts_new_priority")).clone();
                        if v.is_empty() { "0".to_string() } else { v }
                    });

                    ui.label("Typ reguly:");
                    ui.text_edit_singleline(&mut rule_type);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("tts_new_rule_type"), rule_type));

                    ui.label("Wzorzec:");
                    ui.text_edit_singleline(&mut pattern);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("tts_new_pattern"), pattern));

                    ui.label("Zamiennik:");
                    ui.text_edit_singleline(&mut replacement);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("tts_new_replacement"), replacement));

                    ui.label("Jezyk:");
                    ui.text_edit_singleline(&mut language);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("tts_new_language"), language));

                    ui.label("Priorytet:");
                    ui.text_edit_singleline(&mut priority);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("tts_new_priority"), priority));

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Dodaj").clicked() {
                            let rule_type_val: String = ui.memory(|m| m.data.get_temp(egui::Id::new("tts_new_rule_type")).unwrap_or_default());
                            let pattern_val: String = ui.memory(|m| m.data.get_temp(egui::Id::new("tts_new_pattern")).unwrap_or_default());
                            let replacement_val: String = ui.memory(|m| m.data.get_temp(egui::Id::new("tts_new_replacement")).unwrap_or_default());
                            let language_val: String = ui.memory(|m| m.data.get_temp(egui::Id::new("tts_new_language")).unwrap_or_default());
                            let priority_val: String = ui.memory(|m| m.data.get_temp(egui::Id::new("tts_new_priority")).unwrap_or_default());
                            let priority_num = priority_val.parse::<i64>().unwrap_or(0);
                            state.read().unwrap_or_else(|e| e.into_inner()).send_command(UiCommand::CreateTtsRule {
                                rule_type: rule_type_val,
                                pattern: pattern_val,
                                replacement: replacement_val,
                                language: language_val,
                                priority: priority_num,
                            });
                            ui.memory_mut(|m| {
                                m.data.insert_temp(egui::Id::new("tts_new_rule_type"), String::new());
                                m.data.insert_temp(egui::Id::new("tts_new_pattern"), String::new());
                                m.data.insert_temp(egui::Id::new("tts_new_replacement"), String::new());
                                m.data.insert_temp(egui::Id::new("tts_new_language"), String::new());
                                m.data.insert_temp(egui::Id::new("tts_new_priority"), String::new());
                                m.data.insert_temp(egui::Id::new("tts_modal"), false);
                            });
                        }
                        if ui.button("Anuluj").clicked() {
                            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("tts_modal"), false));
                        }
                    });
                });
            if !open {
                ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("tts_modal"), false));
            }
        }
    });
}

fn pii_tab(ui: &mut egui::Ui, state: &SharedAppState) {
    // Add button
    if ui.button("+ Dodaj regule PII").clicked() {
        ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("pii_modal"), true));
    }

    // Test area
    ui.add_space(8.0);
    ui.label("Test PII:");
    let mut test_text: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("pii_test")).clone());
    ui.horizontal(|ui| {
        ui.text_edit_singleline(&mut test_text);
        if ui.button("Testuj").clicked() {
            // Client-side test: check which PII rules match the test text (simple substring match)
            let s = state.read().unwrap_or_else(|e| e.into_inner());
            let mut matched: Vec<String> = Vec::new();
            for rule in &s.pii_rules {
                if rule.is_active && test_text.contains(&rule.pattern) {
                    matched.push(rule.name.clone());
                }
            }
            let result = if matched.is_empty() {
                "Brak dopasowanych regul".to_string()
            } else {
                format!("Dopasowane reguly: {}", matched.join(", "))
            };
            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("pii_test_result"), result));
        }
    });
    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("pii_test"), test_text));

    let test_result: String = ui.memory(|m| m.data.get_temp(egui::Id::new("pii_test_result")).unwrap_or_default());
    if !test_result.is_empty() {
        ui.label(format!("Wynik: {}", test_result));
    }

    ui.add_space(12.0);

    let s = state.read().unwrap_or_else(|e| e.into_inner());
    if s.pii_rules.is_empty() {
        widgets::empty_table(ui, "Brak regul PII");
    } else {
        egui_extras::TableBuilder::new(ui)
            .striped(true)
            .column(egui_extras::Column::auto().at_least(120.0))
            .column(egui_extras::Column::auto().at_least(80.0))
            .column(egui_extras::Column::auto().at_least(150.0))
            .column(egui_extras::Column::auto().at_least(100.0))
            .column(egui_extras::Column::auto().at_least(60.0))
            .column(egui_extras::Column::auto().at_least(60.0))
            .column(egui_extras::Column::auto().at_least(60.0))
            .header(22.0, |mut header| {
                header.col(|ui| { ui.strong("Nazwa"); });
                header.col(|ui| { ui.strong("Kategoria"); });
                header.col(|ui| { ui.strong("Wzorzec"); });
                header.col(|ui| { ui.strong("Zamiennik"); });
                header.col(|ui| { ui.strong("Priorytet"); });
                header.col(|ui| { ui.strong("Aktywna"); });
                header.col(|ui| { ui.strong("Akcje"); });
            })
            .body(|mut body| {
                for rule in &s.pii_rules {
                    body.row(26.0, |mut row| {
                        row.col(|ui| { ui.label(&rule.name); });
                        row.col(|ui| {
                            let color = category_color(&rule.category);
                            widgets::badge(ui, &rule.category, color);
                        });
                        row.col(|ui| { ui.label(RichText::new(&rule.pattern).monospace().size(11.0)); });
                        row.col(|ui| { ui.label(&rule.replacement); });
                        row.col(|ui| { ui.label(rule.priority.to_string()); });
                        row.col(|ui| {
                            let color = if rule.is_active { Color32::from_rgb(34, 197, 94) } else { Color32::from_rgb(239, 68, 68) };
                            let text = if rule.is_active { "Tak" } else { "Nie" };
                            ui.colored_label(color, text);
                        });
                        row.col(|ui| {
                            ui.horizontal(|ui| {
                                let _ = ui.small_button("\u{270F}");
                                if ui.small_button("\u{2716}").clicked() {
                                    state.read().unwrap_or_else(|e| e.into_inner()).send_command(UiCommand::DeletePiiRule(rule.id));
                                }
                            });
                        });
                    });
                }
            });
    }
}

fn fast_path_tab(ui: &mut egui::Ui, state: &SharedAppState) {
    // Add button
    if ui.button("+ Dodaj wzorzec Fast Path").clicked() {
        ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("fp_modal"), true));
    }
    ui.add_space(8.0);

    let s = state.read().unwrap_or_else(|e| e.into_inner());
    if s.fast_path_patterns.is_empty() {
        widgets::empty_table(ui, "Brak wzorcow Fast Path");
    } else {
        egui_extras::TableBuilder::new(ui)
            .striped(true)
            .column(egui_extras::Column::auto().at_least(120.0))
            .column(egui_extras::Column::auto().at_least(150.0))
            .column(egui_extras::Column::auto().at_least(150.0))
            .column(egui_extras::Column::auto().at_least(60.0))
            .column(egui_extras::Column::auto().at_least(60.0))
            .column(egui_extras::Column::auto().at_least(60.0))
            .header(22.0, |mut header| {
                header.col(|ui| { ui.strong("Nazwa"); });
                header.col(|ui| { ui.strong("Wzorzec"); });
                header.col(|ui| { ui.strong("Odpowiedz"); });
                header.col(|ui| { ui.strong("Priorytet"); });
                header.col(|ui| { ui.strong("Aktywny"); });
                header.col(|ui| { ui.strong("Akcje"); });
            })
            .body(|mut body| {
                for pat in &s.fast_path_patterns {
                    body.row(26.0, |mut row| {
                        row.col(|ui| { ui.label(&pat.name); });
                        row.col(|ui| { ui.label(RichText::new(&pat.pattern).monospace().size(11.0)); });
                        row.col(|ui| { ui.label(&pat.response); });
                        row.col(|ui| { ui.label(pat.priority.to_string()); });
                        row.col(|ui| {
                            let color = if pat.is_active { Color32::from_rgb(34, 197, 94) } else { Color32::from_rgb(239, 68, 68) };
                            ui.colored_label(color, if pat.is_active { "Tak" } else { "Nie" });
                        });
                        row.col(|ui| {
                            ui.horizontal(|ui| {
                                let _ = ui.small_button("\u{270F}");
                                if ui.small_button("\u{2716}").clicked() {
                                    state.read().unwrap_or_else(|e| e.into_inner()).send_command(UiCommand::DeleteFastPath(pat.id));
                                }
                            });
                        });
                    });
                }
            });
    }
}

fn tts_cleaning_tab(ui: &mut egui::Ui, state: &SharedAppState) {
    // Add button
    if ui.button("+ Dodaj regule TTS").clicked() {
        ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("tts_modal"), true));
    }
    ui.add_space(8.0);

    let s = state.read().unwrap_or_else(|e| e.into_inner());
    if s.tts_cleaning_rules.is_empty() {
        widgets::empty_table(ui, "Brak regul czyszczenia TTS");
    } else {
        egui_extras::TableBuilder::new(ui)
            .striped(true)
            .column(egui_extras::Column::auto().at_least(120.0))
            .column(egui_extras::Column::auto().at_least(150.0))
            .column(egui_extras::Column::auto().at_least(100.0))
            .column(egui_extras::Column::auto().at_least(60.0))
            .column(egui_extras::Column::auto().at_least(60.0))
            .column(egui_extras::Column::auto().at_least(60.0))
            .header(22.0, |mut header| {
                header.col(|ui| { ui.strong("Nazwa"); });
                header.col(|ui| { ui.strong("Wzorzec"); });
                header.col(|ui| { ui.strong("Zamiennik"); });
                header.col(|ui| { ui.strong("Priorytet"); });
                header.col(|ui| { ui.strong("Aktywna"); });
                header.col(|ui| { ui.strong("Akcje"); });
            })
            .body(|mut body| {
                for rule in &s.tts_cleaning_rules {
                    body.row(26.0, |mut row| {
                        row.col(|ui| { ui.label(&rule.name); });
                        row.col(|ui| { ui.label(RichText::new(&rule.pattern).monospace().size(11.0)); });
                        row.col(|ui| { ui.label(&rule.replacement); });
                        row.col(|ui| { ui.label(rule.priority.to_string()); });
                        row.col(|ui| {
                            let color = if rule.is_active { Color32::from_rgb(34, 197, 94) } else { Color32::from_rgb(239, 68, 68) };
                            ui.colored_label(color, if rule.is_active { "Tak" } else { "Nie" });
                        });
                        row.col(|ui| {
                            ui.horizontal(|ui| {
                                let _ = ui.small_button("\u{270F}");
                                if ui.small_button("\u{2716}").clicked() {
                                    state.read().unwrap_or_else(|e| e.into_inner()).send_command(UiCommand::DeleteTtsRule(rule.id));
                                }
                            });
                        });
                    });
                }
            });
    }
}

fn category_color(cat: &str) -> Color32 {
    match cat {
        "tax_id" => Color32::from_rgb(239, 68, 68),
        "personal_id" => Color32::from_rgb(234, 179, 8),
        "email" => Color32::from_rgb(14, 165, 233),
        "phone" => Color32::from_rgb(168, 85, 247),
        "address" => Color32::from_rgb(34, 197, 94),
        "name" => Color32::from_rgb(99, 102, 241),
        _ => Color32::from_rgb(108, 112, 134),
    }
}
