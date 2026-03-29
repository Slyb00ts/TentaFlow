// =============================================================================
// Plik: app.rs
// Opis: Glowna aplikacja egui — nawigacja, layout, routing ekranow.
//       Odwzorowuje layout web dashboard: sidebar + header + content.
// =============================================================================

use eframe::egui;
use crate::state::SharedAppState;
use crate::screens;
use crate::theme::Theme;

/// Glowna aplikacja TentaFlow
pub struct TentaFlowApp {
    state: SharedAppState,
    theme: Theme,
    current_screen: Screen,
    deploy_wizard: Option<screens::deploy_wizard::DeployWizardState>,
}

/// Dostepne ekrany — odpowiadaja sekcjom web dashboard
#[derive(Debug, Clone, PartialEq)]
pub enum Screen {
    Dashboard,
    Services,
    Models,
    ApiKeys,
    Prompts,
    FlowBuilder,
    Rules,
    Agents,
    Chat,
    Settings,
}

impl Default for Screen {
    fn default() -> Self {
        Screen::Dashboard
    }
}

/// Pozycje nawigacji sidebar
const NAV_ITEMS: &[(&str, &str, u8)] = &[
    ("\u{1F4CA}", "Dashboard",    0),  // 📊
    ("\u{2699}",  "Serwisy",      1),  // ⚙
    ("\u{1F9E0}", "Modele",       2),  // 🧠
    ("\u{1F511}", "Klucze API",   3),  // 🔑
    ("\u{1F4DD}", "Prompty",      4),  // 📝
    ("\u{1F500}", "Flow Builder", 5),  // 🔀
    ("\u{1F6E1}", "Reguly",       6),  // 🛡
    ("\u{1F916}", "Hosts",        7),  // 🤖
    ("\u{1F4AC}", "Playground",   8),  // 💬
    ("\u{1F527}", "Ustawienia",   9),  // 🔧
];

impl TentaFlowApp {
    pub fn new(state: SharedAppState) -> Self {
        Self {
            state,
            theme: Theme::default(),
            current_screen: Screen::default(),
            deploy_wizard: None,
        }
    }

    pub fn state(&self) -> &SharedAppState {
        &self.state
    }

    fn screen_from_index(idx: u8) -> Screen {
        match idx {
            0 => Screen::Dashboard,
            1 => Screen::Services,
            2 => Screen::Models,
            3 => Screen::ApiKeys,
            4 => Screen::Prompts,
            5 => Screen::FlowBuilder,
            6 => Screen::Rules,
            7 => Screen::Agents,
            8 => Screen::Chat,
            9 => Screen::Settings,
            _ => Screen::Dashboard,
        }
    }

    fn render_header(&self, ui: &mut egui::Ui) {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        let online_count = state.online_peer_count();
        let total_count = state.peers.len();
        let connected = state.mesh_connected;
        let active_req = state.metrics.active_requests;
        let latency = state.metrics.avg_latency_ms;
        drop(state);

        ui.horizontal(|ui| {
            ui.heading(
                egui::RichText::new("\u{269B} TentaFlow")
                    .color(self.theme.accent)
                    .strong(),
            );
            ui.separator();

            // Connection status
            let (status_color, status_text) = if connected {
                (self.theme.success, format!("\u{25CF} Mesh: {}/{}", online_count, total_count))
            } else {
                (self.theme.error, "\u{25CF} Offline".to_string())
            };
            ui.colored_label(status_color, status_text);

            ui.separator();
            ui.small(format!("Aktywne: {}", active_req));
            ui.small(format!("Latencja: {:.0}ms", latency));

            // Right side
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // User badge
                let node_role = self.state.read().map(|s| s.node_role.clone()).unwrap_or_default();
                ui.small(format!("\u{1F464} {}", if node_role.is_empty() { "Desktop" } else { &node_role }));
            });
        });
    }

    fn render_status_bar(&self, ui: &mut egui::Ui) {
        let state = self.state.read().unwrap_or_else(|e| e.into_inner());
        let m = &state.metrics;
        let requests = m.total_requests;
        let errors = m.total_errors;
        let tok_in = m.total_input_tokens;
        let tok_out = m.total_output_tokens;
        drop(state);

        ui.horizontal(|ui| {
            ui.small("v0.1.0");
            ui.separator();
            ui.small(format!("Zapytania: {}", requests));
            ui.separator();
            ui.small(format!("Bledy: {}", errors));
            ui.separator();
            ui.small(format!("Tokeny: {}in / {}out", tok_in, tok_out));
        });
    }
}

impl eframe::App for TentaFlowApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.theme.apply(ctx);

        // Header
        egui::TopBottomPanel::top("header_panel")
            .frame(
                egui::Frame::none()
                    .fill(self.theme.palette.header_bg)
                    .inner_margin(egui::Margin::symmetric(12.0, 8.0))
                    .stroke(egui::Stroke::new(1.0, self.theme.palette.border)),
            )
            .show(ctx, |ui| {
                self.render_header(ui);
            });

        // Status bar
        egui::TopBottomPanel::bottom("status_bar")
            .frame(
                egui::Frame::none()
                    .fill(self.theme.palette.status_bar_bg)
                    .inner_margin(egui::Margin::symmetric(12.0, 4.0)),
            )
            .show(ctx, |ui| {
                self.render_status_bar(ui);
            });

        // Sidebar
        egui::SidePanel::left("nav_panel")
            .default_width(170.0)
            .frame(
                egui::Frame::none()
                    .fill(self.theme.palette.sidebar_bg)
                    .inner_margin(egui::Margin::same(8.0))
                    .stroke(egui::Stroke::new(1.0, self.theme.palette.border)),
            )
            .show(ctx, |ui| {
                ui.add_space(4.0);

                let mut toggle_theme = false;

                for &(icon, label, idx) in NAV_ITEMS {
                    let screen = Self::screen_from_index(idx);
                    let selected = self.current_screen == screen;
                    let text = format!("{} {}", icon, label);

                    let response = ui.selectable_label(selected, text);
                    if response.clicked() {
                        self.current_screen = screen;
                    }
                }

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                let mode_label = if self.theme.dark_mode {
                    "\u{263E} Ciemny"
                } else {
                    "\u{2600} Jasny"
                };
                if ui.button(mode_label).clicked() {
                    toggle_theme = true;
                }

                if toggle_theme {
                    self.theme.toggle();
                }

                // Notifications
                let notif_count = self.state.read().map(|s| s.notifications.len()).unwrap_or(0);
                if notif_count > 0 {
                    ui.add_space(8.0);
                    ui.colored_label(self.theme.warning, format!("\u{1F514} {}", notif_count));
                }
            });

        // Content routing
        match self.current_screen {
            Screen::Dashboard => screens::dashboard::ui(ctx, &self.state, &self.theme),
            Screen::Services => screens::services::ui(ctx, &self.state),
            Screen::Models => screens::models::ui(ctx, &self.state),
            Screen::ApiKeys => screens::api_keys::ui(ctx, &self.state),
            Screen::Prompts => screens::prompts::ui(ctx, &self.state),
            Screen::FlowBuilder => screens::flow_builder::ui(ctx, &self.state),
            Screen::Rules => screens::rules::ui(ctx, &self.state),
            Screen::Agents => screens::agents::ui_with_wizard(ctx, &self.state, &mut self.deploy_wizard),
            Screen::Chat => screens::chat::ui(ctx, &self.state),
            Screen::Settings => screens::settings::ui(ctx, &self.state),
        }

        // Deploy wizard overlay (floating window)
        screens::deploy_wizard::ui(ctx, &mut self.deploy_wizard);
    }
}
