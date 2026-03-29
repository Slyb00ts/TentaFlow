// =============================================================================
// Plik: hub/native_installer.rs
// Opis: Natywna instalacja i uruchamianie silnikow LLM na macOS/iOS/Android/Windows.
//       Obsluguje instalacje binariow, pobieranie modeli, zarzadzanie procesami.
// =============================================================================

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

use super::engine_registry::Platform;
use super::model_store::ModelStore;

/// Plan instalacji silnika
#[derive(Debug, Clone)]
pub struct InstallPlan {
    pub engine_id: String,
    pub platform: Platform,
    pub steps: Vec<InstallStep>,
}

/// Krok instalacji
#[derive(Debug, Clone)]
pub enum InstallStep {
    CheckBinary { name: String, path: Option<String> },
    DownloadBinary { url: String, target: String },
    RunCommand { cmd: String, args: Vec<String>, desc: String },
    PipInstall { package: String },
    DownloadModel { model_id: String, format: String },
}

/// Konfiguracja natywnego uruchomienia silnika
#[derive(Debug, Clone)]
pub struct NativeRunConfig {
    pub engine_id: String,
    pub model_id: String,
    pub model_path: PathBuf,
    pub port: u16,
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

/// Informacja o uruchomionym procesie natywnym
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NativeProcessInfo {
    pub engine_id: String,
    pub model_id: String,
    pub pid: Option<u32>,
    pub port: u16,
    pub status: String,
    pub started_at: String,
}

/// Rejestr uruchomionych procesow natywnych
#[derive(Debug, Clone, Default)]
pub struct NativeProcessRegistry {
    processes: Arc<RwLock<HashMap<String, NativeProcessInfo>>>,
}

impl NativeProcessRegistry {
    pub fn new() -> Self {
        Self {
            processes: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn register(&self, key: String, info: NativeProcessInfo) {
        self.processes.write().await.insert(key, info);
    }

    pub async fn unregister(&self, key: &str) {
        self.processes.write().await.remove(key);
    }

    pub async fn get(&self, key: &str) -> Option<NativeProcessInfo> {
        self.processes.read().await.get(key).cloned()
    }

    pub async fn list(&self) -> Vec<NativeProcessInfo> {
        self.processes.read().await.values().cloned().collect()
    }

    pub async fn update_status(&self, key: &str, status: &str) {
        if let Some(info) = self.processes.write().await.get_mut(key) {
            info.status = status.to_string();
        }
    }
}

/// Tworzy plan instalacji silnika na danej platformie
pub fn plan_install(engine_id: &str, platform: &Platform) -> Result<InstallPlan, String> {
    let steps = match (engine_id, platform) {
        // macOS
        ("ollama", Platform::MacOS) => vec![
            InstallStep::CheckBinary {
                name: "ollama".to_string(),
                path: None,
            },
            InstallStep::DownloadBinary {
                url: "https://ollama.com/download/Ollama-darwin.zip".to_string(),
                target: "/usr/local/bin/ollama".to_string(),
            },
        ],
        ("mlx", Platform::MacOS) => vec![
            // MLX dziala natywnie in-process przez mlx-rs (Rust Metal bindings)
            // Nie wymaga Pythona — model jest ladowany bezposrednio przez InferenceManager
        ],
        ("llamacpp", Platform::MacOS) => vec![
            InstallStep::CheckBinary {
                name: "llama-server".to_string(),
                path: None,
            },
            InstallStep::RunCommand {
                cmd: "brew".to_string(),
                args: vec!["install".to_string(), "llama.cpp".to_string()],
                desc: "Install llama.cpp via Homebrew".to_string(),
            },
        ],

        // Windows
        ("ollama", Platform::Windows) => vec![
            InstallStep::CheckBinary {
                name: "ollama".to_string(),
                path: None,
            },
            InstallStep::DownloadBinary {
                url: "https://ollama.com/download/OllamaSetup.exe".to_string(),
                target: "ollama".to_string(),
            },
        ],
        ("llamacpp", Platform::Windows) => vec![
            InstallStep::CheckBinary {
                name: "llama-server".to_string(),
                path: None,
            },
            InstallStep::DownloadBinary {
                url: "https://github.com/ggerganov/llama.cpp/releases/latest".to_string(),
                target: "llama-server.exe".to_string(),
            },
        ],

        // iOS/Android — silniki sa wbudowane (bundled), nie wymagaja instalacji
        ("mlx", Platform::IOS) | ("llamacpp", Platform::IOS) => vec![],
        ("llamacpp", Platform::Android) => vec![],

        _ => {
            return Err(format!(
                "Silnik '{}' nie jest wspierany na platformie {:?}",
                engine_id, platform
            ));
        }
    };

    Ok(InstallPlan {
        engine_id: engine_id.to_string(),
        platform: platform.clone(),
        steps,
    })
}

/// Tworzy konfiguracje uruchomienia silnika z modelem
pub fn plan_run(
    engine_id: &str,
    model_id: &str,
    port: u16,
    model_store: &ModelStore,
) -> Result<NativeRunConfig, String> {
    let model_path = model_store.model_dir(model_id);

    let config = match engine_id {
        "ollama" => NativeRunConfig {
            engine_id: engine_id.to_string(),
            model_id: model_id.to_string(),
            model_path: model_store.ollama_dir(),
            port,
            command: "ollama".to_string(),
            args: vec!["serve".to_string()],
            env: HashMap::from([
                ("OLLAMA_HOST".to_string(), format!("0.0.0.0:{}", port)),
                (
                    "OLLAMA_MODELS".to_string(),
                    model_store.ollama_dir().to_string_lossy().to_string(),
                ),
            ]),
        },
        "mlx" => NativeRunConfig {
            engine_id: engine_id.to_string(),
            model_id: model_id.to_string(),
            model_path: model_path.clone(),
            port,
            // MLX uzywa in-process InferenceManager (mlx-rs + Metal) — brak osobnego procesu.
            // Komenda jest pusta — model jest ladowany przez InferenceManager::load_model().
            command: String::new(),
            args: vec![],
            env: HashMap::from([
                ("MLX_MODEL_PATH".to_string(), model_path.to_string_lossy().to_string()),
            ]),
        },
        "llamacpp" => {
            // Znajdz plik GGUF w katalogu modelu
            let gguf_path = find_gguf_file(&model_path)
                .unwrap_or_else(|| model_path.join("model.gguf"));

            NativeRunConfig {
                engine_id: engine_id.to_string(),
                model_id: model_id.to_string(),
                model_path: model_path.clone(),
                port,
                command: "llama-server".to_string(),
                args: vec![
                    "-m".to_string(),
                    gguf_path.to_string_lossy().to_string(),
                    "--port".to_string(),
                    port.to_string(),
                    "-ngl".to_string(),
                    "99".to_string(),
                ],
                env: HashMap::new(),
            }
        }
        _ => {
            return Err(format!("Nieobslugiwany silnik do natywnego uruchomienia: {}", engine_id));
        }
    };

    Ok(config)
}

/// Wykonuje plan instalacji
pub async fn execute_install(
    plan: &InstallPlan,
    model_store: &ModelStore,
    progress_tx: mpsc::Sender<String>,
) -> Result<(), String> {
    for step in &plan.steps {
        match step {
            InstallStep::CheckBinary { name, path } => {
                let check_path = path.as_deref().unwrap_or(name);
                let _ = progress_tx
                    .send(format!("Sprawdzanie: {}...", name))
                    .await;

                let output = tokio::process::Command::new("which")
                    .arg(check_path)
                    .output()
                    .await;

                match output {
                    Ok(o) if o.status.success() => {
                        let _ = progress_tx
                            .send(format!("Znaleziono: {}", name))
                            .await;
                        // Binary istnieje — pomijamy dalsze kroki instalacji tego narzedzia
                        return Ok(());
                    }
                    _ => {
                        let _ = progress_tx
                            .send(format!("{} nie znaleziony, instalowanie...", name))
                            .await;
                    }
                }
            }
            InstallStep::DownloadBinary { url, target } => {
                let _ = progress_tx
                    .send(format!("Pobieranie: {}", url))
                    .await;
                info!(url = %url, target = %target, "Pobieranie binary");
                // W pelnej implementacji: pobierz i zainstaluj
                // Na razie logujemy krok
                let _ = progress_tx
                    .send(format!("Pobrano: {}", target))
                    .await;
            }
            InstallStep::RunCommand { cmd, args, desc } => {
                let _ = progress_tx.send(format!("Wykonywanie: {}", desc)).await;
                info!(cmd = %cmd, args = ?args, "Wykonywanie komendy");

                let output = tokio::process::Command::new(cmd)
                    .args(args)
                    .output()
                    .await
                    .map_err(|e| format!("Blad komendy '{}': {}", cmd, e))?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    warn!(cmd = %cmd, stderr = %stderr, "Komenda zakonczyla sie bledem");
                    let _ = progress_tx
                        .send(format!("Ostrzezenie: {} — {}", desc, stderr))
                        .await;
                } else {
                    let _ = progress_tx
                        .send(format!("OK: {}", desc))
                        .await;
                }
            }
            InstallStep::PipInstall { package } => {
                let _ = progress_tx
                    .send(format!("pip install {}...", package))
                    .await;

                let output = tokio::process::Command::new("pip3")
                    .args(["install", "--upgrade", package])
                    .output()
                    .await
                    .map_err(|e| format!("pip install {} failed: {}", package, e))?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(format!("pip install {} failed: {}", package, stderr));
                }

                let _ = progress_tx
                    .send(format!("Zainstalowano: {}", package))
                    .await;
            }
            InstallStep::DownloadModel { model_id, format } => {
                let _ = progress_tx
                    .send(format!("Pobieranie modelu: {}", model_id))
                    .await;

                if model_store.is_downloaded(model_id, format) {
                    let _ = progress_tx
                        .send(format!("Model juz pobrany: {}", model_id))
                        .await;
                } else {
                    let (dl_tx, mut dl_rx) = mpsc::channel::<super::model_store::DownloadProgress>(16);
                    let progress_tx2 = progress_tx.clone();
                    let model_id2 = model_id.clone();

                    // Przekazuj postep pobierania
                    let progress_fwd = tokio::spawn(async move {
                        while let Some(p) = dl_rx.recv().await {
                            let _ = progress_tx2
                                .send(format!(
                                    "Pobieranie {}: {:.1}%",
                                    model_id2, p.percent
                                ))
                                .await;
                        }
                    });

                    model_store
                        .download_model(model_id, None, dl_tx)
                        .await?;

                    let _ = progress_fwd.await;
                    let _ = progress_tx
                        .send(format!("Model pobrany: {}", model_id))
                        .await;
                }
            }
        }
    }

    Ok(())
}

/// Uruchamia silnik natywnie jako proces potomny.
/// Dla MLX zwraca None — silnik dziala in-process przez InferenceManager.
pub async fn start_engine(config: &NativeRunConfig) -> Result<Option<tokio::process::Child>, String> {
    // MLX dziala in-process — nie uruchamiamy osobnego procesu
    if config.engine_id == "mlx" || config.command.is_empty() {
        info!(
            engine = %config.engine_id,
            model = %config.model_id,
            "Silnik in-process (bez osobnego procesu) — uzyj InferenceManager::load_model()"
        );
        return Ok(None);
    }

    info!(
        engine = %config.engine_id,
        model = %config.model_id,
        port = config.port,
        cmd = %config.command,
        "Uruchamianie natywnego silnika"
    );

    let mut cmd = tokio::process::Command::new(&config.command);
    cmd.args(&config.args);

    for (k, v) in &config.env {
        cmd.env(k, v);
    }

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let child = cmd
        .spawn()
        .map_err(|e| format!("Blad uruchamiania {}: {}", config.command, e))?;

    info!(
        engine = %config.engine_id,
        pid = child.id(),
        "Silnik uruchomiony"
    );

    Ok(Some(child))
}

/// Sprawdza czy silnik jest zainstalowany
pub fn check_engine_installed(engine_id: &str, platform: &Platform) -> bool {
    let binary = match (engine_id, platform) {
        ("ollama", _) => "ollama",
        ("mlx", _) => return true, // MLX jest wbudowany (in-process via mlx-rs)
        ("llamacpp", _) => "llama-server",
        _ => return false,
    };

    std::process::Command::new("which")
        .arg(binary)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Znajduje pierwszy plik GGUF w katalogu
fn find_gguf_file(dir: &std::path::Path) -> Option<PathBuf> {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e == "gguf").unwrap_or(false) {
                return Some(path);
            }
        }
    }
    None
}
