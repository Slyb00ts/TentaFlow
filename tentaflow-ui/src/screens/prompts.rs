// =============================================================================
// Prompts — zarzadzanie promptami (jak web Prompts.js)
// =============================================================================

use eframe::egui;
use egui::{Color32, RichText};
use crate::state::{SharedAppState, PromptType, UiCommand};
use crate::widgets;

pub fn ui(ctx: &egui::Context, state: &SharedAppState) {
    // Editor sidebar state
    let editing_id = ctx.memory_mut(|m| *m.data.get_temp_mut_or_default::<i64>(egui::Id::new("prompt_edit_id")));

    // Right sidebar for prompt editor
    if editing_id > 0 {
        egui::SidePanel::right("prompt_editor_panel")
            .default_width(350.0)
            .show(ctx, |ui| {
                let s = state.read().unwrap_or_else(|e| e.into_inner());
                if let Some(prompt) = s.prompts.iter().find(|p| p.id == editing_id) {
                    ui.heading(&prompt.name);
                    ui.label(RichText::new(&prompt.prompt_id).size(11.0).color(Color32::from_rgb(108, 112, 134)));
                    ui.add_space(4.0);

                    let type_color = prompt_type_color(&prompt.prompt_type);
                    widgets::badge(ui, &prompt.prompt_type.to_string(), type_color);

                    ui.add_space(8.0);
                    ui.label("Model domyslny:");
                    ui.label(RichText::new(&prompt.default_model).monospace());

                    ui.add_space(8.0);
                    ui.label("Wersja:");
                    ui.label(format!("v{}", prompt.version));

                    ui.add_space(12.0);
                    ui.separator();
                    ui.label("Tresc promptu:");
                    let mut content = prompt.content.clone();
                    let edit_name = prompt.name.clone();
                    let edit_prompt_type = prompt.prompt_type.to_string();
                    let edit_default_model = prompt.default_model.clone();
                    let edit_is_active = prompt.is_active;
                    drop(s);
                    egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut content)
                                .font(egui::TextStyle::Monospace)
                                .desired_width(f32::INFINITY)
                                .desired_rows(15),
                        );
                    });

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Zapisz").clicked() {
                            state.read().unwrap_or_else(|e| e.into_inner()).send_command(
                                UiCommand::UpdatePrompt {
                                    id: editing_id,
                                    name: edit_name.clone(),
                                    content: content.clone(),
                                    prompt_type: edit_prompt_type.clone(),
                                    default_model: edit_default_model.clone(),
                                    is_active: edit_is_active,
                                },
                            );
                        }
                        if ui.button("Zamknij").clicked() {
                            ctx.memory_mut(|m| m.data.insert_temp(egui::Id::new("prompt_edit_id"), 0i64));
                        }
                    });
                } else {
                    drop(s);
                    ctx.memory_mut(|m| m.data.insert_temp(egui::Id::new("prompt_edit_id"), 0i64));
                }
            });
    }

    egui::CentralPanel::default().show(ctx, |ui| {
        let modal_open = ui.memory_mut(|m| *m.data.get_temp_mut_or_default::<bool>(egui::Id::new("prompt_modal")));

        let add_clicked = widgets::page_header_with_button(ui, "Prompty", "+ Dodaj prompt");
        if add_clicked {
            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("prompt_modal"), true));
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            let s = state.read().unwrap_or_else(|e| e.into_inner());

            if s.prompts.is_empty() {
                widgets::empty_table(ui, "Brak promptow");
            } else {
                egui_extras::TableBuilder::new(ui)
                    .striped(true)
                    .column(egui_extras::Column::auto().at_least(180.0))
                    .column(egui_extras::Column::auto().at_least(80.0))
                    .column(egui_extras::Column::auto().at_least(120.0))
                    .column(egui_extras::Column::auto().at_least(60.0))
                    .column(egui_extras::Column::auto().at_least(80.0))
                    .column(egui_extras::Column::auto().at_least(80.0))
                    .header(22.0, |mut header| {
                        header.col(|ui| { ui.strong("Nazwa"); });
                        header.col(|ui| { ui.strong("Typ"); });
                        header.col(|ui| { ui.strong("Model"); });
                        header.col(|ui| { ui.strong("Wersja"); });
                        header.col(|ui| { ui.strong("Status"); });
                        header.col(|ui| { ui.strong("Akcje"); });
                    })
                    .body(|mut body| {
                        for prompt in &s.prompts {
                            let pid = prompt.id;
                            body.row(28.0, |mut row| {
                                row.col(|ui| {
                                    ui.vertical(|ui| {
                                        ui.label(&prompt.name);
                                        ui.label(RichText::new(&prompt.prompt_id).size(10.0).color(Color32::from_rgb(108, 112, 134)));
                                    });
                                });
                                row.col(|ui| {
                                    widgets::badge(ui, &prompt.prompt_type.to_string(), prompt_type_color(&prompt.prompt_type));
                                });
                                row.col(|ui| { ui.label(&prompt.default_model); });
                                row.col(|ui| { ui.label(format!("v{}", prompt.version)); });
                                row.col(|ui| {
                                    let (text, color) = if prompt.is_active {
                                        ("Aktywny", Color32::from_rgb(34, 197, 94))
                                    } else {
                                        ("Nieaktywny", Color32::from_rgb(239, 68, 68))
                                    };
                                    widgets::badge(ui, text, color);
                                });
                                row.col(|ui| {
                                    ui.horizontal(|ui| {
                                        if ui.small_button("\u{270F}").on_hover_text("Edytuj").clicked() {
                                            ctx.memory_mut(|m| m.data.insert_temp(egui::Id::new("prompt_edit_id"), pid));
                                        }
                                        if ui.small_button("\u{2716}").on_hover_text("Usun").clicked() {
                                            state.read().unwrap_or_else(|e| e.into_inner()).send_command(
                                                UiCommand::DeletePrompt(pid),
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
            egui::Window::new("Dodaj prompt")
                .collapsible(false)
                .resizable(false)
                .default_width(500.0)
                .open(&mut open)
                .show(ctx, |ui| {
                    let mut name: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("pmt_name")).clone());
                    let mut content: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("pmt_content")).clone());

                    ui.label("Nazwa:");
                    ui.text_edit_singleline(&mut name);
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("pmt_name"), name));

                    ui.label("Tresc:");
                    ui.add(egui::TextEdit::multiline(&mut content).desired_rows(8));
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("pmt_content"), content));

                    ui.add_space(8.0);
                    let create_name: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("pmt_name")).clone());
                    let create_content: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("pmt_content")).clone());
                    ui.horizontal(|ui| {
                        if ui.button("Dodaj").clicked() {
                            state.read().unwrap_or_else(|e| e.into_inner()).send_command(
                                UiCommand::CreatePrompt {
                                    prompt_id: format!("prompt_{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis()),
                                    name: create_name.clone(),
                                    content: create_content.clone(),
                                    prompt_type: "System".to_string(),
                                    default_model: String::new(),
                                },
                            );
                            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("prompt_modal"), false));
                        }
                        if ui.button("Anuluj").clicked() {
                            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("prompt_modal"), false));
                        }
                    });
                });
            if !open {
                ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("prompt_modal"), false));
            }
        }
    });
}

fn prompt_type_color(pt: &PromptType) -> Color32 {
    match pt {
        PromptType::System => Color32::from_rgb(99, 102, 241),
        PromptType::Suffix => Color32::from_rgb(168, 85, 247),
        PromptType::Template => Color32::from_rgb(14, 165, 233),
        PromptType::User => Color32::from_rgb(34, 197, 94),
    }
}
