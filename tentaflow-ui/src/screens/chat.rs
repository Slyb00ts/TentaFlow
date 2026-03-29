// =============================================================================
// Chat/Playground — interfejs konwersacji (jak web Chat.js)
// =============================================================================

use eframe::egui;
use egui::{Color32, RichText, Rounding, Stroke};
use crate::state::{SharedAppState, ChatRole};
use crate::widgets;

pub fn ui(ctx: &egui::Context, state: &SharedAppState) {
    // ── Right sidebar: chat settings ──
    let show_settings = ctx.memory_mut(|m| *m.data.get_temp_mut_or_default::<bool>(egui::Id::new("chat_settings_open")));

    if show_settings {
        egui::SidePanel::right("chat_settings_panel")
            .default_width(280.0)
            .show(ctx, |ui| {
                ui.heading("Parametry");
                ui.separator();
                ui.add_space(4.0);

                // System prompt
                let mut sys_prompt: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("chat_sys_prompt")).clone());
                ui.label("System Prompt:");
                ui.add(egui::TextEdit::multiline(&mut sys_prompt).desired_rows(4).desired_width(f32::INFINITY));
                ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("chat_sys_prompt"), sys_prompt));

                ui.add_space(8.0);

                // Temperature
                let mut temp: f32 = ui.memory_mut(|m| {
                    let v = m.data.get_temp_mut_or_default::<f32>(egui::Id::new("chat_temp"));
                    if *v == 0.0 { *v = 0.7; }
                    *v
                });
                ui.label("Temperature:");
                ui.add(egui::Slider::new(&mut temp, 0.0..=2.0).step_by(0.1));
                ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("chat_temp"), temp));

                // Max tokens
                let mut max_tokens: u32 = ui.memory_mut(|m| {
                    let v = m.data.get_temp_mut_or_default::<u32>(egui::Id::new("chat_max_tok"));
                    if *v == 0 { *v = 4096; }
                    *v
                });
                ui.label("Max Tokens:");
                let mut mt = max_tokens as f32;
                ui.add(egui::Slider::new(&mut mt, 64.0..=32768.0).logarithmic(true));
                max_tokens = mt as u32;
                ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("chat_max_tok"), max_tokens));

                // Top P
                let mut top_p: f32 = ui.memory_mut(|m| {
                    let v = m.data.get_temp_mut_or_default::<f32>(egui::Id::new("chat_top_p"));
                    if *v == 0.0 { *v = 1.0; }
                    *v
                });
                ui.label("Top P:");
                ui.add(egui::Slider::new(&mut top_p, 0.0..=1.0).step_by(0.05));
                ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("chat_top_p"), top_p));

                ui.add_space(12.0);
                ui.separator();

                // TTS / STT checkboxes
                let mut tts_enabled: bool = ui.memory_mut(|m| *m.data.get_temp_mut_or_default::<bool>(egui::Id::new("chat_tts")));
                let mut stt_enabled: bool = ui.memory_mut(|m| *m.data.get_temp_mut_or_default::<bool>(egui::Id::new("chat_stt")));

                ui.checkbox(&mut tts_enabled, "TTS (Text-to-Speech)");
                ui.checkbox(&mut stt_enabled, "STT (Speech-to-Text)");

                ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("chat_tts"), tts_enabled));
                ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("chat_stt"), stt_enabled));
            });
    }

    egui::CentralPanel::default().show(ctx, |ui| {
        // ── Toolbar ──
        ui.horizontal(|ui| {
            // Model selector
            let s = state.read().unwrap_or_else(|e| e.into_inner());
            let model_names: Vec<String> = s.models.iter().map(|m| m.name.clone()).collect();
            drop(s);

            let mut model_idx: usize = ui.memory_mut(|m| *m.data.get_temp_mut_or_default::<usize>(egui::Id::new("chat_model_idx")));
            let selected_name = model_names.get(model_idx).cloned().unwrap_or_else(|| "Wybierz model".to_string());

            egui::ComboBox::from_id_salt("chat_model_select")
                .selected_text(&selected_name)
                .width(200.0)
                .show_ui(ui, |ui| {
                    for (i, name) in model_names.iter().enumerate() {
                        if ui.selectable_value(&mut model_idx, i, name).clicked() {
                            ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("chat_model_idx"), model_idx));
                        }
                    }
                });

            // Settings toggle
            let settings_open = ui.memory_mut(|m| *m.data.get_temp_mut_or_default::<bool>(egui::Id::new("chat_settings_open")));
            let btn_label = if settings_open { "\u{2699} Ukryj" } else { "\u{2699} Parametry" };
            if ui.button(btn_label).clicked() {
                ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("chat_settings_open"), !settings_open));
            }

            // New conversation
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("+ Nowa rozmowa").clicked() {
                    if let Ok(mut s) = state.write() {
                        s.chat_messages.clear();
                    }
                }
            });
        });
        ui.separator();

        // ── Messages area ──
        let messages_area_height = ui.available_height() - 60.0;

        egui::ScrollArea::vertical()
            .max_height(messages_area_height)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                let s = state.read().unwrap_or_else(|e| e.into_inner());

                if s.chat_messages.is_empty() {
                    ui.add_space(64.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new("Rozpocznij konwersacje...").size(16.0).color(Color32::from_rgb(108, 112, 134)));
                    });
                } else {
                    ui.add_space(8.0);
                    for msg in &s.chat_messages {
                        render_message(ui, msg);
                        ui.add_space(8.0);
                    }
                }
            });

        // ── Input area ──
        ui.separator();
        ui.add_space(4.0);

        let mut input: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("chat_input")).clone());
        let mut send = false;

        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::multiline(&mut input)
                    .desired_rows(2)
                    .desired_width(ui.available_width() - 80.0)
                    .hint_text("Napisz wiadomosc... (Shift+Enter = nowa linia)"),
            );

            // Enter to send (without Shift)
            if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !ui.input(|i| i.modifiers.shift) {
                send = true;
            }

            ui.vertical(|ui| {
                if ui.button(RichText::new("Wyslij").strong()).clicked() {
                    send = true;
                }
            });
        });

        if send && !input.trim().is_empty() {
            if let Ok(mut s) = state.write() {
                s.add_chat_message(ChatRole::User, input.trim().to_string());
                // TODO: send to Core for inference
            }
            input.clear();
        }

        ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("chat_input"), input));
    });
}

fn render_message(ui: &mut egui::Ui, msg: &crate::state::ChatMessage) {
    let is_user = msg.role == ChatRole::User;
    let is_system = msg.role == ChatRole::System;

    let (bg, border_color, role_label, role_color) = if is_user {
        (
            Color32::from_rgba_premultiplied(99, 102, 241, 20),
            Color32::from_rgb(99, 102, 241),
            "Ty",
            Color32::from_rgb(99, 102, 241),
        )
    } else if is_system {
        (
            Color32::from_rgba_premultiplied(234, 179, 8, 15),
            Color32::from_rgb(234, 179, 8),
            "System",
            Color32::from_rgb(234, 179, 8),
        )
    } else {
        (
            Color32::from_rgba_premultiplied(34, 197, 94, 12),
            Color32::from_rgb(34, 197, 94),
            "AI",
            Color32::from_rgb(34, 197, 94),
        )
    };

    let frame = egui::Frame::none()
        .fill(bg)
        .rounding(Rounding::same(8.0))
        .inner_margin(egui::Margin::same(12.0))
        .stroke(Stroke::new(1.0, border_color));

    frame.show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(RichText::new(role_label).strong().color(role_color));
            ui.label(
                RichText::new(msg.timestamp.format("%H:%M:%S").to_string())
                    .size(10.0)
                    .color(Color32::from_rgb(108, 112, 134)),
            );

            // Stats (if assistant)
            if !is_user && !is_system {
                if let Some(dur) = msg.duration_secs {
                    ui.label(RichText::new(format!("{:.1}s", dur)).size(10.0).color(Color32::from_rgb(108, 112, 134)));
                }
                if let Some(tok) = msg.token_count {
                    ui.label(RichText::new(format!("{} tok", tok)).size(10.0).color(Color32::from_rgb(108, 112, 134)));
                }
                if let Some(tps) = msg.tokens_per_sec {
                    ui.label(RichText::new(format!("{:.1} tok/s", tps)).size(10.0).color(Color32::from_rgb(108, 112, 134)));
                }
            }
        });

        // Reasoning block (collapsed)
        if let Some(ref reasoning) = msg.reasoning_content {
            if !reasoning.is_empty() {
                ui.add_space(4.0);
                egui::CollapsingHeader::new(RichText::new("Rozumowanie").size(11.0).color(Color32::from_rgb(168, 85, 247)))
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.label(RichText::new(reasoning).size(12.0).color(Color32::from_rgb(166, 173, 200)));
                    });
            }
        }

        ui.add_space(4.0);
        ui.label(&msg.content);
    });
}
