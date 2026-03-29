// =============================================================================
// Agents/Hosts — karty hostow z gauge CPU/RAM/GPU (jak web Hosts.js)
// =============================================================================

use eframe::egui;
use egui::{Color32, RichText, Rounding, Stroke};
use crate::state::SharedAppState;
use crate::widgets;
use super::deploy_wizard::DeployWizardState;

pub fn ui(ctx: &egui::Context, state: &SharedAppState) {
    ui_with_wizard(ctx, state, &mut None)
}

pub fn ui_with_wizard(ctx: &egui::Context, state: &SharedAppState, deploy_wizard: &mut Option<DeployWizardState>) {
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.heading("Hosts");
        ui.separator();
        ui.add_space(4.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            let s = state.read().unwrap_or_else(|e| e.into_inner());

            if s.peers.is_empty() {
                widgets::empty_table(ui, "Brak polaczonych hostow");
                return;
            }

            // ── Host cards grid ──
            let card_width = 380.0;
            let available = ui.available_width();
            let cols = ((available / card_width) as usize).max(1);

            let peers: Vec<_> = s.peers.clone();
            drop(s);

            let mut chunks = peers.chunks(cols);
            while let Some(chunk) = chunks.next() {
                ui.horizontal(|ui| {
                    for peer in chunk {
                        let is_online = peer.status == "online" || peer.status == "connected";

                        let frame = egui::Frame::none()
                            .fill(ui.visuals().faint_bg_color)
                            .rounding(Rounding::same(8.0))
                            .inner_margin(egui::Margin::same(16.0))
                            .stroke(Stroke::new(
                                1.0,
                                if is_online {
                                    Color32::from_rgb(69, 71, 90)
                                } else {
                                    Color32::from_rgb(50, 50, 60)
                                },
                            ));

                        frame.show(ui, |ui| {
                            ui.set_min_width(card_width - 40.0);

                            // Header
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(&peer.hostname).strong().size(16.0));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    let (dot, label) = if is_online {
                                        (Color32::from_rgb(34, 197, 94), "Connected")
                                    } else {
                                        (Color32::from_rgb(239, 68, 68), "Offline")
                                    };
                                    widgets::status_badge(ui, label, dot);
                                });
                            });

                            if !is_online {
                                return;
                            }

                            ui.add_space(8.0);

                            // CPU gauge
                            widgets::usage_gauge(ui, "CPU", peer.cpu_usage, 180.0);

                            // RAM gauge
                            let ram_pct = if peer.ram_total_mb > 0 {
                                peer.ram_used_mb as f64 / peer.ram_total_mb as f64 * 100.0
                            } else {
                                peer.ram_usage
                            };
                            ui.horizontal(|ui| {
                                widgets::usage_gauge(ui, "RAM", ram_pct, 180.0);
                                ui.label(
                                    RichText::new(widgets::format_ram(peer.ram_used_mb, peer.ram_total_mb))
                                        .size(10.0)
                                        .color(Color32::from_rgb(108, 112, 134)),
                                );
                            });

                            // GPU (if available)
                            if !peer.gpus.is_empty() {
                                let avg_gpu: f64 = peer.gpus.iter().map(|g| g.usage_percent).sum::<f64>() / peer.gpus.len() as f64;
                                let total_vram_used: u64 = peer.gpus.iter().map(|g| g.vram_used_mb).sum();
                                let total_vram: u64 = peer.gpus.iter().map(|g| g.vram_total_mb).sum();

                                widgets::usage_gauge(ui, "GPU", avg_gpu, 180.0);
                                ui.label(
                                    RichText::new(format!("VRAM: {}/{} MB", total_vram_used, total_vram))
                                        .size(10.0)
                                        .color(Color32::from_rgb(108, 112, 134)),
                                );
                            } else if let Some(ref gpu_info) = peer.gpu_info {
                                ui.label(RichText::new(format!("GPU: {}", gpu_info)).size(11.0));
                            }

                            // Containers count
                            if !peer.containers.is_empty() {
                                let running = peer.containers.iter().filter(|c| c.status == "running").count();
                                ui.add_space(4.0);
                                ui.label(
                                    RichText::new(format!("Kontenery: {}/{}", running, peer.containers.len()))
                                        .size(12.0),
                                );
                            }

                            // Deploy LLM button
                            ui.add_space(4.0);
                            if ui.button(RichText::new("Deploy LLM").size(11.0)).clicked() {
                                if deploy_wizard.is_some() {
                                    // Juz otwarty
                                } else {
                                    *deploy_wizard = Some(DeployWizardState::open_for_peer(
                                        &peer.node_id,
                                        &peer.hostname,
                                    ));
                                }
                            }

                            // Network stats
                            if peer.network_rx_bytes_sec > 0.0 || peer.network_tx_bytes_sec > 0.0 {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(format!(
                                        "\u{2193} {} \u{2191} {}",
                                        widgets::format_bytes(peer.network_rx_bytes_sec),
                                        widgets::format_bytes(peer.network_tx_bytes_sec),
                                    )).size(11.0));
                                });
                            }

                            // Labels
                            if !peer.labels.is_empty() {
                                ui.add_space(4.0);
                                ui.horizontal_wrapped(|ui| {
                                    for (k, v) in &peer.labels {
                                        widgets::badge(ui, &format!("{}={}", k, v), Color32::from_rgb(50, 50, 70));
                                    }
                                });
                            }
                        });
                    }
                });
                ui.add_space(8.0);
            }

            // ── All containers aggregate ──
            let s = state.read().unwrap_or_else(|e| e.into_inner());
            let all_containers: Vec<_> = s.peers.iter()
                .flat_map(|p| p.containers.iter().map(move |c| (p.hostname.as_str(), c)))
                .collect();

            if !all_containers.is_empty() {
                ui.add_space(16.0);
                widgets::section_header(ui, "Docker kontenery (wszystkie hosty)");

                egui_extras::TableBuilder::new(ui)
                    .striped(true)
                    .column(egui_extras::Column::auto().at_least(120.0))
                    .column(egui_extras::Column::auto().at_least(150.0))
                    .column(egui_extras::Column::auto().at_least(180.0))
                    .column(egui_extras::Column::auto().at_least(60.0))
                    .column(egui_extras::Column::auto().at_least(80.0))
                    .column(egui_extras::Column::auto().at_least(80.0))
                    .header(22.0, |mut header| {
                        header.col(|ui| { ui.strong("Host"); });
                        header.col(|ui| { ui.strong("Kontener"); });
                        header.col(|ui| { ui.strong("Obraz"); });
                        header.col(|ui| { ui.strong("CPU%"); });
                        header.col(|ui| { ui.strong("RAM MB"); });
                        header.col(|ui| { ui.strong("Status"); });
                    })
                    .body(|mut body| {
                        for (host, container) in &all_containers {
                            body.row(24.0, |mut row| {
                                row.col(|ui| { ui.label(*host); });
                                row.col(|ui| { ui.label(&container.name); });
                                row.col(|ui| { ui.label(RichText::new(&container.image).size(11.0)); });
                                row.col(|ui| { ui.label(format!("{:.1}", container.cpu_percent)); });
                                row.col(|ui| { ui.label(container.memory_mb.to_string()); });
                                row.col(|ui| {
                                    let color = if container.status == "running" {
                                        Color32::from_rgb(34, 197, 94)
                                    } else {
                                        Color32::from_rgb(108, 112, 134)
                                    };
                                    widgets::status_badge(ui, &container.status, color);
                                });
                            });
                        }
                    });
            }
        });
    });
}
