// =============================================================================
// Plik: hub/model_store.rs
// Opis: Centralny magazyn modeli — wspoldzielony katalog z organizacja
//       HuggingFace ({org}/{model_name}/). Wiele silnikow moze uzywac
//       tych samych plikow. Obsluga pobierania z HF z raportem postepu.
// =============================================================================

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Centralny magazyn modeli
#[derive(Debug, Clone)]
pub struct ModelStore {
    pub base_dir: PathBuf,
}

/// Informacja o lokalnie pobranym modelu
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalModel {
    pub model_id: String,
    pub format: String,
    pub size_bytes: u64,
    pub path: PathBuf,
    pub downloaded_at: String,
}

/// Postep pobierania modelu
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub model_id: String,
    pub file_name: String,
    pub bytes_downloaded: u64,
    pub bytes_total: u64,
    pub percent: f32,
}

impl ModelStore {
    /// Tworzy magazyn z domyslnym katalogiem per platforma
    pub fn default_for_platform() -> Self {
        let base = default_model_dir();
        Self { base_dir: base }
    }

    /// Tworzy magazyn z podanym katalogiem bazowym
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    /// Sciezka do katalogu modelu: {base}/huggingface/{org}/{model_name}/
    pub fn model_dir(&self, model_id: &str) -> PathBuf {
        let parts: Vec<&str> = model_id.splitn(2, '/').collect();
        if parts.len() == 2 {
            self.base_dir
                .join("huggingface")
                .join(parts[0])
                .join(parts[1])
        } else {
            self.base_dir.join("huggingface").join(model_id)
        }
    }

    /// Sciezka do katalogu modeli Ollama
    pub fn ollama_dir(&self) -> PathBuf {
        self.base_dir.join("ollama")
    }

    /// Sprawdza czy model jest juz pobrany (kompletnie).
    /// Uzywa pliku markera `.download_complete` ktory jest tworzony
    /// dopiero po pomyslnym pobraniu wszystkich plikow.
    pub fn is_downloaded(&self, model_id: &str, _format: &str) -> bool {
        let dir = self.model_dir(model_id);
        dir.join(".download_complete").exists()
    }

    /// Lista pobranych modeli
    pub fn list_models(&self) -> Vec<LocalModel> {
        let mut models = Vec::new();
        let hf_dir = self.base_dir.join("huggingface");

        if !hf_dir.exists() {
            return models;
        }

        // Iteruj po org/model
        if let Ok(orgs) = std::fs::read_dir(&hf_dir) {
            for org_entry in orgs.flatten() {
                if !org_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    continue;
                }
                let org_name = org_entry.file_name().to_string_lossy().to_string();

                if let Ok(model_dirs) = std::fs::read_dir(org_entry.path()) {
                    for model_entry in model_dirs.flatten() {
                        if !model_entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                            continue;
                        }
                        let model_name = model_entry.file_name().to_string_lossy().to_string();
                        let model_id = format!("{}/{}", org_name, model_name);
                        let model_path = model_entry.path();

                        let format = detect_format(&model_path);
                        let size = dir_size(&model_path);
                        let downloaded_at = model_entry
                            .metadata()
                            .ok()
                            .and_then(|m| m.modified().ok())
                            .map(|t| {
                                let dt: chrono::DateTime<chrono::Utc> = t.into();
                                dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
                            })
                            .unwrap_or_default();

                        models.push(LocalModel {
                            model_id,
                            format,
                            size_bytes: size,
                            path: model_path,
                            downloaded_at,
                        });
                    }
                }
            }
        }

        models
    }

    /// Usuwa pobrany model (z walidacja path traversal)
    pub fn delete_model(&self, model_id: &str) -> Result<(), String> {
        // VULN-019: Walidacja path traversal
        if model_id.contains("..") || model_id.starts_with('/') || model_id.starts_with('\\') {
            return Err("Path traversal wykryty".to_string());
        }
        let dir = self.model_dir(model_id);
        if dir.exists() {
            let canonical = dir.canonicalize().map_err(|e| format!("{}", e))?;
            if !canonical.starts_with(&self.base_dir) {
                return Err("Sciezka poza dozwolonym katalogiem".to_string());
            }
            std::fs::remove_dir_all(&dir)
                .map_err(|e| format!("Blad usuwania modelu {}: {}", model_id, e))?;
            info!(model_id = %model_id, "Model usuniety z magazynu");
        }
        Ok(())
    }

    /// Pobiera model z HuggingFace Hub
    pub async fn download_model(
        &self,
        model_id: &str,
        hf_token: Option<&str>,
        progress_tx: mpsc::Sender<DownloadProgress>,
    ) -> Result<PathBuf, String> {
        let model_dir = self.model_dir(model_id);
        std::fs::create_dir_all(&model_dir)
            .map_err(|e| format!("Blad tworzenia katalogu: {}", e))?;

        // Klient do API (krotkie requesty — timeout 30s wystarczy)
        let api_client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("HTTP client error: {}", e))?;

        // Klient do pobierania plikow — BEZ globalnego timeoutu (pliki moga miec wiele GB),
        // tylko connect_timeout zeby nie wisiec na zerwanych polaczeniach
        let dl_client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| format!("Download client error: {}", e))?;

        // Lista plikow z HF API
        let url = format!("https://huggingface.co/api/models/{}/tree/main", model_id);

        let mut req = api_client
            .get(&url)
            .header("User-Agent", "TentaFlow-AI/1.0");
        if let Some(token) = hf_token {
            req = req.header("Authorization", format!("Bearer {}", token));
        }

        let resp = req
            .send()
            .await
            .map_err(|e| format!("HF tree request failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!("HF API returned {}", resp.status()));
        }

        #[derive(Deserialize)]
        struct HfTreeEntry {
            #[serde(rename = "rfilename", alias = "path")]
            path: Option<String>,
            size: Option<u64>,
            #[serde(rename = "type")]
            entry_type: Option<String>,
        }

        let entries: Vec<HfTreeEntry> = resp
            .json()
            .await
            .map_err(|e| format!("HF tree parse error: {}", e))?;

        // Filtruj pliki do pobrania (pomijamy duze pliki niepotrzebne)
        let downloadable: Vec<_> = entries
            .iter()
            .filter(|e| {
                let etype = e.entry_type.as_deref().unwrap_or("file");
                if etype != "file" {
                    return false;
                }
                let path = e.path.as_deref().unwrap_or("");
                // Pobieraj kluczowe pliki
                path.ends_with(".safetensors")
                    || path.ends_with(".gguf")
                    || path.ends_with(".json")
                    || path.ends_with(".txt")
                    || path.ends_with(".model")
                    || path.ends_with(".tiktoken")
            })
            .collect();

        for entry in &downloadable {
            let file_name = entry.path.as_deref().unwrap_or("unknown");
            let file_size = entry.size.unwrap_or(0);

            let file_url = format!(
                "https://huggingface.co/{}/resolve/main/{}",
                model_id, file_name
            );

            let file_path = model_dir.join(file_name);

            // Stworz podkatalogi jesli potrzeba
            if let Some(parent) = file_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }

            // Sprawdz czy plik juz czesciowo/calkowicie pobrany
            let existing_size = file_path.metadata().map(|m| m.len()).unwrap_or(0);

            // Plik kompletny — pomijamy
            if existing_size == file_size && file_size > 0 {
                debug!(file = %file_name, "Plik juz pobrany, pomijam");
                continue;
            }

            // Wznawianie pobierania (resume) jesli plik czesciowo pobrany
            let resume_from = if existing_size > 0 && existing_size < file_size {
                info!(file = %file_name, existing = existing_size, total = file_size, "Wznawianie pobierania");
                existing_size
            } else {
                0
            };

            info!(file = %file_name, size = file_size, resume_from = resume_from, "Pobieranie pliku modelu");

            let mut dl_req = dl_client
                .get(&file_url)
                .header("User-Agent", "TentaFlow-AI/1.0");
            if let Some(token) = hf_token {
                dl_req = dl_req.header("Authorization", format!("Bearer {}", token));
            }
            if resume_from > 0 {
                dl_req = dl_req.header("Range", format!("bytes={}-", resume_from));
            }

            let dl_resp = dl_req
                .send()
                .await
                .map_err(|e| format!("Download failed for {}: {}", file_name, e))?;

            let status = dl_resp.status();
            if !status.is_success() && status != reqwest::StatusCode::PARTIAL_CONTENT {
                warn!(file = %file_name, status = %status, "Pomijam plik — blad HTTP");
                continue;
            }

            let total = if resume_from > 0 {
                file_size // Calkowity rozmiar pliku
            } else {
                dl_resp.content_length().unwrap_or(file_size)
            };

            // Otwieraj w trybie append jesli wznawiamy, inaczej tworz nowy
            let mut file = if resume_from > 0 {
                tokio::fs::OpenOptions::new()
                    .append(true)
                    .open(&file_path)
                    .await
                    .map_err(|e| format!("Cannot open {} for append: {}", file_name, e))?
            } else {
                tokio::fs::File::create(&file_path)
                    .await
                    .map_err(|e| format!("Cannot create {}: {}", file_name, e))?
            };

            let mut stream = dl_resp.bytes_stream();
            let mut downloaded: u64 = resume_from;
            let mut last_report: u64 = 0;

            use futures::StreamExt;
            use tokio::io::AsyncWriteExt;

            while let Some(chunk_result) = stream.next().await {
                let chunk = chunk_result
                    .map_err(|e: reqwest::Error| format!("Download stream error: {}", e))?;
                file.write_all(&chunk)
                    .await
                    .map_err(|e| format!("Write error: {}", e))?;
                downloaded += chunk.len() as u64;

                // Raportuj co 1 MB
                if downloaded - last_report >= 1_048_576 || downloaded == total {
                    last_report = downloaded;
                    let pct = if total > 0 {
                        (downloaded as f32 / total as f32) * 100.0
                    } else {
                        0.0
                    };
                    let _ = progress_tx
                        .send(DownloadProgress {
                            model_id: model_id.to_string(),
                            file_name: file_name.to_string(),
                            bytes_downloaded: downloaded,
                            bytes_total: total,
                            percent: pct,
                        })
                        .await;
                }
            }

            file.flush()
                .await
                .map_err(|e| format!("Flush error: {}", e))?;
        }

        // Zapisz marker kompletnego pobrania — is_downloaded() sprawdza ten plik
        let marker_path = model_dir.join(".download_complete");
        let _ = tokio::fs::write(&marker_path, chrono::Utc::now().to_rfc3339().as_bytes()).await;

        info!(model_id = %model_id, path = %model_dir.display(), "Model pobrany");
        Ok(model_dir)
    }
}

/// Domyslny katalog modeli per platforma
fn default_model_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("TentaFlow.AI")
            .join("models")
    }
    #[cfg(target_os = "ios")]
    {
        // iOS: Application Support w sandbox — dirs::data_dir() mapuje na
        // Library/Application Support, co jest poprawne dla duzych plikow modeli.
        // Documents/ jest widoczny w Files.app — modele tam nie powinny trafiac.
        dirs::data_dir()
            .unwrap_or_else(|| {
                // Fallback dla iOS gdy dirs nie rozpoznaje platformy
                PathBuf::from("/var/mobile/Library/Application Support")
            })
            .join("TentaFlow.AI")
            .join("models")
    }
    #[cfg(target_os = "linux")]
    {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("tentaflow-ai")
            .join("models")
    }
    #[cfg(target_os = "windows")]
    {
        dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("TentaFlow.AI")
            .join("models")
    }
    #[cfg(target_os = "android")]
    {
        // Android: wewnetrzna pamiec aplikacji
        let base = std::env::var("ANDROID_DATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/data/data/ai.tentaflow.mobile/files"));
        base.join("tentaflow-ai").join("models")
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "linux",
        target_os = "windows",
        target_os = "android"
    )))]
    {
        PathBuf::from("./models")
    }
}

/// Sprawdza czy katalog zawiera pliki o danym rozszerzeniu
fn has_files_with_ext(dir: &std::path::Path, ext: &str) -> bool {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.path().extension().map(|e| e == ext).unwrap_or(false) {
                return true;
            }
        }
    }
    false
}

/// Wykrywa format modelu w katalogu
fn detect_format(dir: &std::path::Path) -> String {
    if has_files_with_ext(dir, "gguf") {
        return "gguf".to_string();
    }
    // Sprawdz czy to MLX (safetensors + config z mlx)
    if has_files_with_ext(dir, "safetensors") {
        let config_path = dir.join("config.json");
        if config_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&config_path) {
                if content.contains("mlx") {
                    return "mlx".to_string();
                }
            }
        }
        return "safetensors".to_string();
    }
    "unknown".to_string()
}

/// Oblicza rozmiar katalogu rekurencyjnie
fn dir_size(dir: &std::path::Path) -> u64 {
    let mut total: u64 = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            } else if path.is_dir() {
                total += dir_size(&path);
            }
        }
    }
    total
}
