// =============================================================================
// Settings — konfiguracja (jak web Settings.js z 5 sekcjami)
// =============================================================================

use eframe::egui;
use egui::{Color32, RichText};
use crate::state::{SharedAppState, UiCommand};
use crate::widgets;

pub fn ui(ctx: &egui::Context, state: &SharedAppState) {
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.heading("Ustawienia");
        ui.separator();

        egui::ScrollArea::vertical().show(ui, |ui| {
            // ── 1. Key-Value Settings ──
            egui::CollapsingHeader::new(RichText::new("Ustawienia ogolne").size(16.0).strong())
                .default_open(true)
                .show(ui, |ui| {
                    let s = state.read().unwrap_or_else(|e| e.into_inner());

                    if s.settings.is_empty() {
                        ui.label("Brak ustawien");
                    } else {
                        egui_extras::TableBuilder::new(ui)
                            .striped(true)
                            .column(egui_extras::Column::auto().at_least(200.0))
                            .column(egui_extras::Column::auto().at_least(250.0))
                            .column(egui_extras::Column::auto().at_least(150.0))
                            .header(22.0, |mut header| {
                                header.col(|ui| { ui.strong("Klucz"); });
                                header.col(|ui| { ui.strong("Wartosc"); });
                                header.col(|ui| { ui.strong("Zmieniono"); });
                            })
                            .body(|mut body| {
                                for entry in &s.settings {
                                    body.row(24.0, |mut row| {
                                        row.col(|ui| { ui.label(RichText::new(&entry.key).monospace().size(12.0)); });
                                        row.col(|ui| {
                                            let is_secret = entry.key.contains("password") || entry.key.contains("secret") || entry.key.contains("key_pem");
                                            if is_secret {
                                                ui.label("********");
                                            } else {
                                                ui.label(&entry.value);
                                            }
                                        });
                                        row.col(|ui| { ui.label(entry.updated_at.as_deref().unwrap_or("-")); });
                                    });
                                }
                            });
                    }
                });

            ui.add_space(12.0);

            // ── 2. Speaker Recognition ──
            egui::CollapsingHeader::new(RichText::new("Rozpoznawanie mowcy").size(16.0).strong())
                .default_open(false)
                .show(ui, |ui| {
                    let mut high: f32 = ui.memory_mut(|m| {
                        let v = m.data.get_temp_mut_or_default::<f32>(egui::Id::new("spk_high"));
                        if *v == 0.0 { *v = 0.78; }
                        *v
                    });
                    let mut medium: f32 = ui.memory_mut(|m| {
                        let v = m.data.get_temp_mut_or_default::<f32>(egui::Id::new("spk_medium"));
                        if *v == 0.0 { *v = 0.55; }
                        *v
                    });
                    let mut enrollment: f32 = ui.memory_mut(|m| {
                        let v = m.data.get_temp_mut_or_default::<f32>(egui::Id::new("spk_enroll"));
                        if *v == 0.0 { *v = 0.70; }
                        *v
                    });
                    let mut samples: u32 = ui.memory_mut(|m| {
                        let v = m.data.get_temp_mut_or_default::<u32>(egui::Id::new("spk_samples"));
                        if *v == 0 { *v = 3; }
                        *v
                    });

                    ui.label("Prog wysokiej pewnosci:");
                    ui.add(egui::Slider::new(&mut high, 0.0..=1.0).step_by(0.01));
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("spk_high"), high));

                    ui.label("Prog sredniej pewnosci:");
                    ui.add(egui::Slider::new(&mut medium, 0.0..=1.0).step_by(0.01));
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("spk_medium"), medium));

                    ui.label("Prog zapisu glosu:");
                    ui.add(egui::Slider::new(&mut enrollment, 0.0..=1.0).step_by(0.01));
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("spk_enroll"), enrollment));

                    ui.label("Wymagane probki glosu:");
                    let mut s_f = samples as f32;
                    ui.add(egui::Slider::new(&mut s_f, 1.0..=20.0).step_by(1.0));
                    samples = s_f as u32;
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("spk_samples"), samples));

                    ui.add_space(4.0);
                    if ui.button("Zapisz").clicked() {
                        let commands = vec![
                            UiCommand::SetSetting { key: "speaker_high_threshold".to_string(), value: format!("{}", high) },
                            UiCommand::SetSetting { key: "speaker_medium_threshold".to_string(), value: format!("{}", medium) },
                            UiCommand::SetSetting { key: "speaker_enrollment_threshold".to_string(), value: format!("{}", enrollment) },
                            UiCommand::SetSetting { key: "speaker_required_samples".to_string(), value: format!("{}", samples) },
                        ];
                        for cmd in commands {
                            state.read().unwrap_or_else(|e| e.into_inner()).send_command(cmd);
                        }
                    }
                });

            ui.add_space(12.0);

            // ── 3. Flow Engine ──
            egui::CollapsingHeader::new(RichText::new("Flow Engine").size(16.0).strong())
                .default_open(false)
                .show(ui, |ui| {
                    let mut enabled: bool = ui.memory_mut(|m| *m.data.get_temp_mut_or_default::<bool>(egui::Id::new("flow_enabled")));
                    let mut debug: bool = ui.memory_mut(|m| *m.data.get_temp_mut_or_default::<bool>(egui::Id::new("flow_debug")));
                    let mut timeout: u32 = ui.memory_mut(|m| {
                        let v = m.data.get_temp_mut_or_default::<u32>(egui::Id::new("flow_timeout"));
                        if *v == 0 { *v = 30000; }
                        *v
                    });

                    ui.checkbox(&mut enabled, "Wlacz flow engine");
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("flow_enabled"), enabled));

                    ui.checkbox(&mut debug, "Tryb debug");
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("flow_debug"), debug));

                    ui.label("Domyslny timeout (ms):");
                    let mut t_f = timeout as f32;
                    ui.add(egui::Slider::new(&mut t_f, 1000.0..=120000.0).logarithmic(true));
                    timeout = t_f as u32;
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("flow_timeout"), timeout));

                    ui.add_space(4.0);
                    if ui.button("Zapisz").clicked() {
                        let commands = vec![
                            UiCommand::SetSetting { key: "flow_engine_enabled".to_string(), value: format!("{}", enabled) },
                            UiCommand::SetSetting { key: "flow_engine_debug".to_string(), value: format!("{}", debug) },
                            UiCommand::SetSetting { key: "flow_engine_timeout".to_string(), value: format!("{}", timeout) },
                        ];
                        for cmd in commands {
                            state.read().unwrap_or_else(|e| e.into_inner()).send_command(cmd);
                        }
                    }
                });

            ui.add_space(12.0);

            // ── 4. Portainer instances ──
            egui::CollapsingHeader::new(RichText::new("Instancje Portainer").size(16.0).strong())
                .default_open(false)
                .show(ui, |ui| {
                    let s = state.read().unwrap_or_else(|e| e.into_inner());

                    if s.portainer_instances.is_empty() {
                        ui.label("Brak instancji Portainer");
                    } else {
                        egui_extras::TableBuilder::new(ui)
                            .striped(true)
                            .column(egui_extras::Column::auto().at_least(120.0))
                            .column(egui_extras::Column::auto().at_least(200.0))
                            .column(egui_extras::Column::auto().at_least(80.0))
                            .column(egui_extras::Column::auto().at_least(100.0))
                            .header(22.0, |mut header| {
                                header.col(|ui| { ui.strong("Nazwa"); });
                                header.col(|ui| { ui.strong("URL"); });
                                header.col(|ui| { ui.strong("Auth"); });
                                header.col(|ui| { ui.strong("Akcje"); });
                            })
                            .body(|mut body| {
                                for inst in &s.portainer_instances {
                                    body.row(24.0, |mut row| {
                                        row.col(|ui| { ui.label(&inst.name); });
                                        row.col(|ui| { ui.label(RichText::new(&inst.url).monospace().size(11.0)); });
                                        row.col(|ui| { ui.label(&inst.auth_type); });
                                        row.col(|ui| {
                                            ui.horizontal(|ui| {
                                                ui.small_button("Test");
                                                if ui.small_button("\u{2716}").clicked() {
                                                    let cmd = UiCommand::DeletePortainerInstance(inst.id);
                                                    state.read().unwrap_or_else(|e| e.into_inner()).send_command(cmd);
                                                }
                                            });
                                        });
                                    });
                                }
                            });
                    }

                    ui.add_space(8.0);
                    // Add form
                    ui.label(RichText::new("Dodaj instancje:").strong());
                    let mut p_name: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("port_name")).clone());
                    let mut p_url: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("port_url")).clone());

                    ui.horizontal(|ui| {
                        ui.label("Nazwa:");
                        ui.text_edit_singleline(&mut p_name);
                    });
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("port_name"), p_name));

                    ui.horizontal(|ui| {
                        ui.label("URL:");
                        ui.text_edit_singleline(&mut p_url);
                    });
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("port_url"), p_url));

                    if ui.button("Dodaj").clicked() {
                        let name = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("port_name")).clone());
                        let url = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("port_url")).clone());
                        let cmd = UiCommand::CreatePortainerInstance {
                            name,
                            url,
                            api_key: String::new(),
                            username: String::new(),
                            password: String::new(),
                        };
                        state.read().unwrap_or_else(|e| e.into_inner()).send_command(cmd);
                    }
                });

            ui.add_space(12.0);

            // ── 5. TLS Certificate ──
            egui::CollapsingHeader::new(RichText::new("Certyfikat TLS").size(16.0).strong())
                .default_open(false)
                .show(ui, |ui| {
                    ui.label("Certyfikat (PEM):");
                    let mut cert: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("tls_cert")).clone());
                    ui.add(egui::TextEdit::multiline(&mut cert).desired_rows(6).desired_width(f32::INFINITY).font(egui::TextStyle::Monospace));
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("tls_cert"), cert));

                    ui.add_space(8.0);
                    ui.label("Klucz prywatny (PEM):");
                    let mut key: String = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("tls_key")).clone());
                    ui.add(egui::TextEdit::multiline(&mut key).desired_rows(6).desired_width(f32::INFINITY).font(egui::TextStyle::Monospace));
                    ui.memory_mut(|m| m.data.insert_temp(egui::Id::new("tls_key"), key));

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Zapisz certyfikat").clicked() {
                            let cert = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("tls_cert")).clone());
                            let key = ui.memory_mut(|m| m.data.get_temp_mut_or_default::<String>(egui::Id::new("tls_key")).clone());
                            state.read().unwrap_or_else(|e| e.into_inner()).send_command(
                                UiCommand::SetSetting { key: "tls_cert".to_string(), value: cert.clone() },
                            );
                            state.read().unwrap_or_else(|e| e.into_inner()).send_command(
                                UiCommand::SetSetting { key: "tls_key".to_string(), value: key.clone() },
                            );
                        }
                        if ui.button("Dystrybuuj do agentow").clicked() {
                            // TODO
                        }
                    });
                });
        });
    });
}
