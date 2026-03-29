// =============================================================================
// Plik: screens/deploy_wizard.rs
// Opis: Wizard wdrazania LLM w egui — 5 krokow: silnik, model, parametry,
//       podglad, deploy. Odpowiednik webowego LLMDeployWizard.js.
// =============================================================================

use eframe::egui;
use egui::{Color32, RichText, Rounding, Stroke, Vec2};
use crate::widgets;

/// Stan wizarda wdrazania
#[derive(Debug, Clone, Default)]
pub struct DeployWizardState {
    pub open: bool,
    pub step: u8,
    pub target_peer_id: String,
    pub target_peer_name: String,

    // Krok 1: Silnik
    pub selected_engine: Option<String>,
    pub available_engines: Vec<EngineItem>,

    // Krok 2: Model
    pub selected_model: Option<String>,
    pub search_query: String,
    pub search_results: Vec<ModelItem>,
    pub default_models: Vec<ModelItem>,
    pub custom_model: String,

    // Krok 3: Parametry
    pub container_name: String,
    pub port: u16,
    pub deploy_mode: String,

    // Krok 4-5: Deploy
    pub preview_text: String,
    pub deploy_logs: Vec<String>,
    pub is_deploying: bool,
}

#[derive(Debug, Clone)]
pub struct EngineItem {
    pub id: String,
    pub name: String,
    pub description: String,
    pub deploy_mode: String,
    pub model_format: String,
}

#[derive(Debug, Clone)]
pub struct ModelItem {
    pub model_id: String,
    pub author: String,
    pub downloads: u64,
    pub likes: u64,
}

impl DeployWizardState {
    pub fn open_for_peer(peer_id: &str, peer_name: &str) -> Self {
        Self {
            open: true,
            step: 1,
            target_peer_id: peer_id.to_string(),
            target_peer_name: peer_name.to_string(),
            selected_engine: None,
            available_engines: default_engines(),
            selected_model: None,
            search_query: String::new(),
            search_results: Vec::new(),
            default_models: Vec::new(),
            custom_model: String::new(),
            container_name: format!("tentaflow-ai-llm-{}", random_suffix()),
            port: 5010,
            deploy_mode: "native".to_string(),
            preview_text: String::new(),
            deploy_logs: Vec::new(),
            is_deploying: false,
        }
    }
}

/// Renderuj wizard jako okno overlay — wywolyane z app.rs
pub fn ui(ctx: &egui::Context, wizard: &mut Option<DeployWizardState>) {
    let Some(wiz) = wizard.as_mut() else {
        return;
    };
    if !wiz.open {
        *wizard = None;
        return;
    }

    let mut should_close = false;

    egui::Window::new("Deploy LLM")
        .collapsible(false)
        .resizable(true)
        .default_width(560.0)
        .min_width(400.0)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ctx, |ui| {
            // Header z krokami
            ui.horizontal(|ui| {
                ui.heading(RichText::new("Deploy LLM").strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("Host: {}", wiz.target_peer_name)).size(12.0));
                });
            });
            ui.add_space(4.0);

            // Step indicator
            ui.horizontal(|ui| {
                for i in 1..=5u8 {
                    let (bg, fg) = if i == wiz.step {
                        (Color32::from_rgb(99, 102, 241), Color32::WHITE)
                    } else if i < wiz.step {
                        (Color32::from_rgb(34, 197, 94), Color32::WHITE)
                    } else {
                        (Color32::from_rgb(55, 55, 75), Color32::from_rgb(160, 160, 180))
                    };
                    let label = match i {
                        1 => "Silnik",
                        2 => "Model",
                        3 => "Parametry",
                        4 => "Podglad",
                        5 => "Deploy",
                        _ => "",
                    };
                    let frame = egui::Frame::none()
                        .fill(bg)
                        .rounding(Rounding::same(12.0))
                        .inner_margin(egui::Margin::symmetric(8.0, 2.0));
                    frame.show(ui, |ui| {
                        ui.label(RichText::new(format!("{} {}", i, label)).color(fg).size(11.0));
                    });
                    if i < 5 {
                        ui.label(RichText::new("\u{2192}").size(10.0).color(Color32::from_rgb(80, 80, 100)));
                    }
                }
            });

            ui.separator();
            ui.add_space(4.0);

            // Tresc kroku
            egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| {
                match wiz.step {
                    1 => render_step_engine(ui, wiz),
                    2 => render_step_model(ui, wiz),
                    3 => render_step_params(ui, wiz),
                    4 => render_step_preview(ui, wiz),
                    5 => render_step_deploy(ui, wiz),
                    _ => {}
                }
            });

            ui.add_space(8.0);
            ui.separator();

            // Przyciski nawigacji
            ui.horizontal(|ui| {
                if ui.button("Anuluj").clicked() {
                    should_close = true;
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if wiz.step == 5 {
                        let btn = egui::Button::new(
                            RichText::new("Deploy").color(Color32::WHITE),
                        ).fill(Color32::from_rgb(34, 197, 94));
                        if ui.add_enabled(!wiz.is_deploying, btn).clicked() {
                            wiz.is_deploying = true;
                            wiz.deploy_logs.push("Rozpoczynanie wdrazania...".to_string());
                        }
                    } else {
                        if ui.button("Dalej \u{2192}").clicked() && validate_step(wiz) {
                            if wiz.step == 3 {
                                generate_preview(wiz);
                            }
                            wiz.step += 1;
                        }
                    }

                    if wiz.step > 1 && !wiz.is_deploying {
                        if ui.button("\u{2190} Wstecz").clicked() {
                            wiz.step -= 1;
                        }
                    }
                });
            });
        });

    if should_close {
        *wizard = None;
    }
}

/// Krok 1: Wybor silnika
fn render_step_engine(ui: &mut egui::Ui, wiz: &mut DeployWizardState) {
    ui.label(RichText::new("Wybierz silnik inference:").strong());
    ui.add_space(8.0);

    let engines = wiz.available_engines.clone();
    for engine in &engines {
        let is_selected = wiz.selected_engine.as_deref() == Some(&engine.id);

        let frame = egui::Frame::none()
            .fill(if is_selected {
                Color32::from_rgb(30, 40, 70)
            } else {
                ui.visuals().faint_bg_color
            })
            .rounding(Rounding::same(6.0))
            .inner_margin(egui::Margin::same(12.0))
            .stroke(Stroke::new(
                if is_selected { 2.0 } else { 1.0 },
                if is_selected {
                    Color32::from_rgb(99, 102, 241)
                } else {
                    Color32::from_rgb(60, 60, 80)
                },
            ));

        let resp = frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&engine.name).strong().size(14.0));
                let mode_color = if engine.deploy_mode == "native" {
                    Color32::from_rgb(34, 197, 94)
                } else {
                    Color32::from_rgb(59, 130, 246)
                };
                widgets::badge(ui, &engine.deploy_mode, mode_color);
                widgets::badge(ui, &engine.model_format, Color32::from_rgb(80, 80, 100));
            });
            ui.label(
                RichText::new(&engine.description)
                    .size(11.0)
                    .color(Color32::from_rgb(140, 140, 160)),
            );
        });

        if resp.response.interact(egui::Sense::click()).clicked() {
            wiz.selected_engine = Some(engine.id.clone());
            wiz.deploy_mode = engine.deploy_mode.clone();
            // Zaladuj domyslne modele per silnik
            wiz.default_models = default_models_for(&engine.id);
            wiz.selected_model = None;
        }

        ui.add_space(4.0);
    }
}

/// Krok 2: Wybor modelu
fn render_step_model(ui: &mut egui::Ui, wiz: &mut DeployWizardState) {
    ui.label(RichText::new("Wybierz model:").strong());
    ui.add_space(4.0);

    // Pole wyszukiwania
    ui.horizontal(|ui| {
        ui.label("Szukaj:");
        ui.text_edit_singleline(&mut wiz.search_query);
    });
    ui.add_space(4.0);

    // Lista modeli (domyslne + wyniki wyszukiwania)
    let models = if wiz.search_results.is_empty() {
        &wiz.default_models
    } else {
        &wiz.search_results
    };

    let models_cloned = models.clone();
    for model in &models_cloned {
        let is_selected = wiz.selected_model.as_deref() == Some(&model.model_id);

        let frame = egui::Frame::none()
            .fill(if is_selected {
                Color32::from_rgb(30, 40, 70)
            } else {
                ui.visuals().faint_bg_color
            })
            .rounding(Rounding::same(4.0))
            .inner_margin(egui::Margin::same(8.0))
            .stroke(Stroke::new(
                1.0,
                if is_selected {
                    Color32::from_rgb(99, 102, 241)
                } else {
                    Color32::TRANSPARENT
                },
            ));

        let resp = frame.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&model.model_id).strong().size(12.0));
                if model.downloads > 0 {
                    ui.label(
                        RichText::new(format!("\u{2193}{}", format_count(model.downloads)))
                            .size(10.0)
                            .color(Color32::from_rgb(120, 120, 140)),
                    );
                }
            });
            ui.label(
                RichText::new(&model.author)
                    .size(10.0)
                    .color(Color32::from_rgb(120, 120, 140)),
            );
        });

        if resp.response.interact(egui::Sense::click()).clicked() {
            wiz.selected_model = Some(model.model_id.clone());
            wiz.custom_model.clear();
        }

        ui.add_space(2.0);
    }

    ui.add_space(8.0);
    ui.separator();
    ui.add_space(4.0);

    ui.label("Lub wpisz wlasny HuggingFace repo:");
    let resp = ui.text_edit_singleline(&mut wiz.custom_model);
    if resp.changed() && !wiz.custom_model.is_empty() {
        wiz.selected_model = Some(wiz.custom_model.clone());
    }
}

/// Krok 3: Parametry
fn render_step_params(ui: &mut egui::Ui, wiz: &mut DeployWizardState) {
    ui.label(RichText::new("Parametry wdrozenia:").strong());
    ui.add_space(8.0);

    egui::Grid::new("deploy_params_grid")
        .num_columns(2)
        .spacing([12.0, 8.0])
        .show(ui, |ui| {
            ui.label("Nazwa kontenera:");
            ui.text_edit_singleline(&mut wiz.container_name);
            ui.end_row();

            ui.label("Port:");
            ui.add(egui::DragValue::new(&mut wiz.port).range(1..=65535));
            ui.end_row();

            ui.label("Tryb:");
            let mode_text = if wiz.deploy_mode == "native" {
                "Natywny (bez Docker)"
            } else {
                "Docker Compose"
            };
            ui.label(RichText::new(mode_text).color(
                if wiz.deploy_mode == "native" {
                    Color32::from_rgb(34, 197, 94)
                } else {
                    Color32::from_rgb(59, 130, 246)
                },
            ));
            ui.end_row();
        });
}

/// Krok 4: Podglad
fn render_step_preview(ui: &mut egui::Ui, wiz: &mut DeployWizardState) {
    ui.label(RichText::new("Podglad konfiguracji:").strong());
    ui.add_space(4.0);

    let engine_name = wiz.selected_engine.as_deref().unwrap_or("-");
    let model_name = wiz.selected_model.as_deref().unwrap_or("-");

    // Summary
    egui::Grid::new("preview_summary")
        .num_columns(2)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            ui.label("Silnik:");
            ui.label(RichText::new(engine_name).strong());
            ui.end_row();

            ui.label("Model:");
            ui.label(RichText::new(model_name).strong());
            ui.end_row();

            ui.label("Nazwa:");
            ui.label(RichText::new(&wiz.container_name).strong());
            ui.end_row();

            ui.label("Port:");
            ui.label(RichText::new(wiz.port.to_string()).strong());
            ui.end_row();

            ui.label("Tryb:");
            ui.label(RichText::new(&wiz.deploy_mode).strong());
            ui.end_row();
        });

    ui.add_space(8.0);

    if wiz.deploy_mode == "docker" {
        ui.label("Docker Compose YAML:");
        let mut yaml = wiz.preview_text.clone();
        ui.add(
            egui::TextEdit::multiline(&mut yaml)
                .desired_width(f32::INFINITY)
                .desired_rows(12)
                .code_editor(),
        );
        wiz.preview_text = yaml;
    } else {
        ui.label("Komendy do wykonania:");
        let frame = egui::Frame::none()
            .fill(Color32::from_rgb(20, 20, 30))
            .rounding(Rounding::same(4.0))
            .inner_margin(egui::Margin::same(8.0));
        frame.show(ui, |ui| {
            ui.label(
                RichText::new(&wiz.preview_text)
                    .size(11.0)
                    .code()
                    .color(Color32::from_rgb(200, 200, 220)),
            );
        });
    }
}

/// Krok 5: Deploy
fn render_step_deploy(ui: &mut egui::Ui, wiz: &mut DeployWizardState) {
    ui.label(RichText::new("Wdrazanie:").strong());
    ui.add_space(4.0);

    if wiz.is_deploying {
        ui.spinner();
        ui.label("Wdrazanie w toku...");
    }

    // Logi
    if !wiz.deploy_logs.is_empty() {
        let frame = egui::Frame::none()
            .fill(Color32::from_rgb(20, 20, 30))
            .rounding(Rounding::same(4.0))
            .inner_margin(egui::Margin::same(8.0));
        frame.show(ui, |ui| {
            for log in &wiz.deploy_logs {
                ui.label(
                    RichText::new(log)
                        .size(11.0)
                        .code()
                        .color(Color32::from_rgb(180, 180, 200)),
                );
            }
        });
    } else {
        ui.label("Kliknij Deploy aby rozpoczac wdrazanie.");

        // Podsumowanie
        let engine = wiz.selected_engine.as_deref().unwrap_or("-");
        let model = wiz.selected_model.as_deref().unwrap_or("-");
        ui.add_space(8.0);
        ui.label(format!("Silnik: {}", engine));
        ui.label(format!("Model: {}", model));
        ui.label(format!("Port: {}", wiz.port));
        ui.label(format!("Tryb: {}", wiz.deploy_mode));
    }
}

/// Walidacja biezacego kroku
fn validate_step(wiz: &DeployWizardState) -> bool {
    match wiz.step {
        1 => wiz.selected_engine.is_some(),
        2 => wiz.selected_model.as_ref().map(|m| !m.is_empty()).unwrap_or(false),
        3 => !wiz.container_name.is_empty() && wiz.port > 0,
        _ => true,
    }
}

/// Generuj podglad (YAML lub komendy natywne)
fn generate_preview(wiz: &mut DeployWizardState) {
    let engine = wiz.selected_engine.as_deref().unwrap_or("llamacpp");
    let model = wiz.selected_model.as_deref().unwrap_or("");

    if wiz.deploy_mode == "native" {
        wiz.preview_text = match engine {
            "ollama" => format!(
                "# Ollama (natywnie)\nollama serve &\nollama pull {}\n\n# Serwer na porcie {}\nexport OLLAMA_HOST=0.0.0.0:{}",
                model, wiz.port, wiz.port
            ),
            "mlx" => format!(
                "# MLX (natywny in-process — Apple Silicon Metal GPU)\n# Model ladowany przez InferenceManager (mlx-rs)\n# Zero overhead — brak osobnego procesu\n#\n# Model: {}\n# Port: {} (OpenAI API endpoint)",
                model, wiz.port
            ),
            "llamacpp" => format!(
                "# LLama.cpp (natywnie)\n# Wymagany: llama-server (brew install llama.cpp)\n\nllama-server -m <model_path>/{}.gguf --port {} -ngl 99",
                model.split('/').last().unwrap_or(model), wiz.port
            ),
            _ => format!("# Silnik: {}\n# Model: {}\n# Port: {}", engine, model, wiz.port),
        };
    } else {
        // Docker Compose YAML
        wiz.preview_text = format!(
            "services:\n  {}:\n    image: registry.nextapp.pl/tentaflow-llm-{}:latest\n    container_name: {}\n    restart: unless-stopped\n    ports:\n      - \"{}:5010\"\n    environment:\n      - MODEL_ID={}\n    deploy:\n      resources:\n        reservations:\n          devices:\n            - driver: nvidia\n              count: all\n              capabilities: [gpu]",
            wiz.container_name, engine, wiz.container_name, wiz.port, model
        );
    }
}

/// Domyslna lista silnikow (lokalna, przed pobraniem z API)
fn default_engines() -> Vec<EngineItem> {
    vec![
        EngineItem {
            id: "sglang".to_string(),
            name: "SGLang".to_string(),
            description: "High-performance inference with RadixAttention".to_string(),
            deploy_mode: "docker".to_string(),
            model_format: "safetensors".to_string(),
        },
        EngineItem {
            id: "vllm".to_string(),
            name: "vLLM".to_string(),
            description: "High-throughput inference with PagedAttention".to_string(),
            deploy_mode: "docker".to_string(),
            model_format: "safetensors".to_string(),
        },
        EngineItem {
            id: "ollama".to_string(),
            name: "Ollama".to_string(),
            description: "Easy-to-use LLM runner with built-in model library".to_string(),
            deploy_mode: "native".to_string(),
            model_format: "ollama".to_string(),
        },
        EngineItem {
            id: "llamacpp".to_string(),
            name: "LLama.cpp".to_string(),
            description: "Lightweight C/C++ inference for GGUF models".to_string(),
            deploy_mode: "native".to_string(),
            model_format: "gguf".to_string(),
        },
        EngineItem {
            id: "mlx".to_string(),
            name: "MLX".to_string(),
            description: "Apple MLX framework for Apple Silicon inference".to_string(),
            deploy_mode: "native".to_string(),
            model_format: "mlx".to_string(),
        },
    ]
}

/// Domyslne modele per silnik
fn default_models_for(engine_id: &str) -> Vec<ModelItem> {
    match engine_id {
        "sglang" | "vllm" => vec![
            ModelItem { model_id: "speakleash/Bielik-11B-v3.0-Instruct-FP8-Dynamic".into(), author: "speakleash".into(), downloads: 0, likes: 0 },
            ModelItem { model_id: "meta-llama/Llama-3.1-8B-Instruct".into(), author: "meta-llama".into(), downloads: 0, likes: 0 },
            ModelItem { model_id: "Qwen/Qwen2.5-7B-Instruct".into(), author: "Qwen".into(), downloads: 0, likes: 0 },
            ModelItem { model_id: "mistralai/Mistral-7B-Instruct-v0.3".into(), author: "mistralai".into(), downloads: 0, likes: 0 },
        ],
        "llamacpp" => vec![
            ModelItem { model_id: "bartowski/Llama-3.1-8B-Instruct-GGUF".into(), author: "bartowski".into(), downloads: 0, likes: 0 },
            ModelItem { model_id: "TheBloke/Llama-2-7B-GGUF".into(), author: "TheBloke".into(), downloads: 0, likes: 0 },
            ModelItem { model_id: "bartowski/Qwen2.5-7B-Instruct-GGUF".into(), author: "bartowski".into(), downloads: 0, likes: 0 },
        ],
        "mlx" => vec![
            ModelItem { model_id: "mlx-community/Llama-3.1-8B-Instruct-4bit".into(), author: "mlx-community".into(), downloads: 0, likes: 0 },
            ModelItem { model_id: "mlx-community/Qwen2.5-7B-Instruct-4bit".into(), author: "mlx-community".into(), downloads: 0, likes: 0 },
            ModelItem { model_id: "mlx-community/Mistral-7B-Instruct-v0.3-4bit".into(), author: "mlx-community".into(), downloads: 0, likes: 0 },
            ModelItem { model_id: "mlx-community/phi-4-4bit".into(), author: "mlx-community".into(), downloads: 0, likes: 0 },
        ],
        "ollama" => vec![
            ModelItem { model_id: "llama3.1".into(), author: "Meta".into(), downloads: 0, likes: 0 },
            ModelItem { model_id: "qwen2.5".into(), author: "Alibaba".into(), downloads: 0, likes: 0 },
            ModelItem { model_id: "mistral".into(), author: "Mistral AI".into(), downloads: 0, likes: 0 },
            ModelItem { model_id: "phi3".into(), author: "Microsoft".into(), downloads: 0, likes: 0 },
            ModelItem { model_id: "gemma2".into(), author: "Google".into(), downloads: 0, likes: 0 },
        ],
        _ => vec![],
    }
}

fn random_suffix() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let chars: Vec<char> = "abcdefghijklmnopqrstuvwxyz0123456789".chars().collect();
    let mut result = String::with_capacity(5);
    let mut n = seed;
    for _ in 0..5 {
        result.push(chars[(n % chars.len() as u128) as usize]);
        n /= chars.len() as u128;
    }
    result
}

fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}
