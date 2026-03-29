// =============================================================================
// Plik: inference/model_manager.rs
// Opis: Pobieranie i zarzadzanie modelami z HuggingFace Hub.
//       Cache lokalny, sledzenie postepu, weryfikacja plikow.
// =============================================================================

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{info, warn};

/// Format modelu
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ModelFormat {
    GGUF,
    SafeTensors,
    MLX,
}

impl ModelFormat {
    /// Rozpoznaje format na podstawie rozszerzenia pliku
    fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "gguf" => Some(Self::GGUF),
            "safetensors" => Some(Self::SafeTensors),
            "mlx" => Some(Self::MLX),
            _ => None,
        }
    }

}

/// Informacje o dostepnym modelu w repozytorium
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailableModel {
    pub repo_id: String,
    pub filename: String,
    pub size_bytes: u64,
    pub quantization: Option<String>,
    pub format: ModelFormat,
}

/// Model zapisany w lokalnym cache
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedModel {
    pub filename: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub format: ModelFormat,
    pub quantization: Option<String>,
}

/// Status pobierania
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DownloadStatus {
    Pending,
    Downloading,
    Verifying,
    Complete,
    Error(String),
}

/// Postep pobierania modelu
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub model_id: String,
    pub filename: String,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub percent: f32,
    pub speed_bps: u64,
    pub status: DownloadStatus,
}

/// Manager modeli — pobieranie z HuggingFace, cache lokalny
pub struct ModelManager {
    models_dir: PathBuf,
    progress_tx: broadcast::Sender<DownloadProgress>,
}

impl ModelManager {
    /// Tworzy nowy manager modeli z podanym katalogiem cache.
    /// Katalog jest tworzony jesli nie istnieje.
    pub fn new(models_dir: impl AsRef<Path>) -> Self {
        let models_dir = models_dir.as_ref().to_path_buf();
        let (progress_tx, _) = broadcast::channel(64);

        if let Err(e) = std::fs::create_dir_all(&models_dir) {
            warn!("Nie udalo sie utworzyc katalogu modeli {:?}: {}", models_dir, e);
        }

        Self {
            models_dir,
            progress_tx,
        }
    }

    /// Zwraca liste modeli w lokalnym cache.
    /// Skanuje katalog w poszukiwaniu plikow .gguf i .safetensors.
    pub fn list_cached_models(&self) -> anyhow::Result<Vec<CachedModel>> {
        let mut models = Vec::new();

        let entries = std::fs::read_dir(&self.models_dir)
            .with_context(|| format!("Nie mozna odczytac katalogu {:?}", self.models_dir))?;

        for entry in entries {
            let entry = entry?;
            let path = entry.path();

            if !path.is_file() {
                continue;
            }

            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            let format = match ModelFormat::from_extension(ext) {
                Some(f) => f,
                None => continue,
            };

            let metadata = std::fs::metadata(&path)
                .with_context(|| format!("Nie mozna odczytac metadanych {:?}", path))?;

            let filename = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_string();

            let quantization = extract_quantization(&filename);

            models.push(CachedModel {
                filename,
                path,
                size_bytes: metadata.len(),
                format,
                quantization,
            });
        }

        models.sort_by(|a, b| a.filename.cmp(&b.filename));
        Ok(models)
    }

    /// Pobiera model z HuggingFace Hub.
    /// Wykorzystuje hf-hub do pobrania pliku z repozytorium.
    /// Zwraca sciezke do pobranego pliku w cache.
    pub async fn download_model(
        &self,
        repo_id: &str,
        filename: &str,
    ) -> anyhow::Result<PathBuf> {
        let repo_id = repo_id.to_string();
        let filename = filename.to_string();
        let models_dir = self.models_dir.clone();
        let progress_tx = self.progress_tx.clone();

        // Sprawdz czy plik juz istnieje w cache
        let target_path = models_dir.join(&filename);
        if target_path.exists() {
            info!("Model '{}' juz istnieje w cache: {:?}", filename, target_path);
            return Ok(target_path);
        }

        // Wyslij status poczatkowy
        let _ = progress_tx.send(DownloadProgress {
            model_id: repo_id.clone(),
            filename: filename.clone(),
            downloaded_bytes: 0,
            total_bytes: 0,
            percent: 0.0,
            speed_bps: 0,
            status: DownloadStatus::Pending,
        });

        info!("Pobieranie modelu '{}' z repozytorium '{}'", filename, repo_id);

        // hf-hub API jest synchroniczne — uruchamiamy w osobnym watku
        let downloaded_path = tokio::task::spawn_blocking({
            let repo_id = repo_id.clone();
            let filename = filename.clone();
            let progress_tx = progress_tx.clone();

            move || -> anyhow::Result<PathBuf> {
                let start = Instant::now();

                // Wyslij status pobierania
                let _ = progress_tx.send(DownloadProgress {
                    model_id: repo_id.clone(),
                    filename: filename.clone(),
                    downloaded_bytes: 0,
                    total_bytes: 0,
                    percent: 0.0,
                    speed_bps: 0,
                    status: DownloadStatus::Downloading,
                });

                let api = hf_hub::api::sync::Api::new()
                    .context("Nie udalo sie utworzyc klienta HuggingFace Hub")?;

                let repo = api.model(repo_id.clone());

                // hf-hub pobiera plik do wlasnego cache i zwraca sciezke
                let hf_path = repo.get(&filename).with_context(|| {
                    format!(
                        "Nie udalo sie pobrac pliku '{}' z repozytorium '{}'",
                        filename, repo_id
                    )
                })?;

                let elapsed = start.elapsed();
                let file_size = std::fs::metadata(&hf_path)
                    .map(|m| m.len())
                    .unwrap_or(0);

                let speed_bps = if elapsed.as_secs() > 0 {
                    file_size / elapsed.as_secs()
                } else {
                    file_size
                };

                // Wyslij status weryfikacji
                let _ = progress_tx.send(DownloadProgress {
                    model_id: repo_id.clone(),
                    filename: filename.clone(),
                    downloaded_bytes: file_size,
                    total_bytes: file_size,
                    percent: 100.0,
                    speed_bps,
                    status: DownloadStatus::Verifying,
                });

                // Skopiuj z cache hf-hub do naszego katalogu modeli
                let target = models_dir.join(&filename);
                std::fs::copy(&hf_path, &target).with_context(|| {
                    format!(
                        "Nie udalo sie skopiowac modelu z {:?} do {:?}",
                        hf_path, target
                    )
                })?;

                // Weryfikacja rozmiaru po skopiowaniu
                let copied_size = std::fs::metadata(&target)
                    .map(|m| m.len())
                    .unwrap_or(0);

                if copied_size != file_size {
                    std::fs::remove_file(&target).ok();
                    anyhow::bail!(
                        "Rozmiar pliku po skopiowaniu ({}) nie zgadza sie z oryginatem ({})",
                        copied_size,
                        file_size
                    );
                }

                // Wyslij status zakonczenia
                let _ = progress_tx.send(DownloadProgress {
                    model_id: repo_id.clone(),
                    filename: filename.clone(),
                    downloaded_bytes: file_size,
                    total_bytes: file_size,
                    percent: 100.0,
                    speed_bps,
                    status: DownloadStatus::Complete,
                });

                info!(
                    "Model '{}' pobrany ({:.1} MB, {:.1} s)",
                    filename,
                    file_size as f64 / 1_048_576.0,
                    elapsed.as_secs_f64()
                );

                Ok(target)
            }
        })
        .await
        .context("Watek pobierania zakonczyl sie nieoczekiwanie")??;

        Ok(downloaded_path)
    }

    /// Usuwa model z lokalnego cache
    pub fn delete_model(&self, filename: &str) -> anyhow::Result<()> {
        let path = self.models_dir.join(filename);

        if !path.exists() {
            anyhow::bail!("Model '{}' nie istnieje w cache", filename);
        }

        let size = std::fs::metadata(&path)
            .map(|m| m.len())
            .unwrap_or(0);

        std::fs::remove_file(&path)
            .with_context(|| format!("Nie udalo sie usunac modelu {:?}", path))?;

        info!(
            "Usuniety model '{}' ({:.1} MB)",
            filename,
            size as f64 / 1_048_576.0
        );

        Ok(())
    }

    /// Subskrypcja na kanal postepu pobierania
    pub fn subscribe_progress(&self) -> broadcast::Receiver<DownloadProgress> {
        self.progress_tx.subscribe()
    }

    /// Laczny rozmiar cache w bajtach
    pub fn cache_size(&self) -> anyhow::Result<u64> {
        let entries = std::fs::read_dir(&self.models_dir)
            .with_context(|| format!("Nie mozna odczytac katalogu {:?}", self.models_dir))?;

        let mut total = 0u64;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                total += std::fs::metadata(&path)
                    .map(|m| m.len())
                    .unwrap_or(0);
            }
        }

        Ok(total)
    }

    /// Sciezka do katalogu modeli
    pub fn models_dir(&self) -> &Path {
        &self.models_dir
    }

    /// Zwraca sciezke do modelu w cache (jesli istnieje)
    pub fn get_model_path(&self, filename: &str) -> Option<PathBuf> {
        let path = self.models_dir.join(filename);
        if path.exists() {
            Some(path)
        } else {
            None
        }
    }
}

/// Wyciaga typ kwantyzacji z nazwy pliku.
/// Np. "mistral-7b-instruct-v0.3.Q4_K_M.gguf" -> Some("Q4_K_M")
fn extract_quantization(filename: &str) -> Option<String> {
    let patterns = [
        "Q2_K", "Q3_K_S", "Q3_K_M", "Q3_K_L",
        "Q4_0", "Q4_1", "Q4_K_S", "Q4_K_M",
        "Q5_0", "Q5_1", "Q5_K_S", "Q5_K_M",
        "Q6_K", "Q8_0", "F16", "F32",
        "IQ1_S", "IQ1_M", "IQ2_XXS", "IQ2_XS", "IQ2_S", "IQ2_M",
        "IQ3_XXS", "IQ3_XS", "IQ3_S", "IQ4_XS", "IQ4_NL",
    ];

    let upper = filename.to_uppercase();
    // Szukamy od najdluzszego wzorca zeby Q3_K_M pasowalo przed Q3_K
    let mut found: Option<&str> = None;
    for pattern in &patterns {
        if upper.contains(pattern) {
            match found {
                Some(prev) if prev.len() >= pattern.len() => {}
                _ => found = Some(pattern),
            }
        }
    }

    found.map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_quantization() {
        assert_eq!(
            extract_quantization("mistral-7b-instruct-v0.3.Q4_K_M.gguf"),
            Some("Q4_K_M".to_string())
        );
        assert_eq!(
            extract_quantization("model.Q8_0.gguf"),
            Some("Q8_0".to_string())
        );
        assert_eq!(
            extract_quantization("model.F16.safetensors"),
            Some("F16".to_string())
        );
        assert_eq!(extract_quantization("model.bin"), None);
    }

    #[test]
    fn test_model_format_from_extension() {
        assert_eq!(ModelFormat::from_extension("gguf"), Some(ModelFormat::GGUF));
        assert_eq!(
            ModelFormat::from_extension("safetensors"),
            Some(ModelFormat::SafeTensors)
        );
        assert_eq!(ModelFormat::from_extension("bin"), None);
    }

    #[test]
    fn test_model_manager_new() {
        let dir = std::env::temp_dir().join("tentaflow_test_mm");
        let manager = ModelManager::new(&dir);
        assert!(dir.exists());
        assert_eq!(manager.models_dir(), dir.as_path());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_list_cached_empty() {
        let dir = std::env::temp_dir().join("tentaflow_test_mm_empty");
        std::fs::create_dir_all(&dir).ok();
        let manager = ModelManager::new(&dir);
        let models = manager.list_cached_models().unwrap();
        assert!(models.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_list_cached_with_files() {
        let dir = std::env::temp_dir().join("tentaflow_test_mm_files");
        std::fs::create_dir_all(&dir).ok();

        // Tworzymy testowe pliki
        std::fs::write(dir.join("test.Q4_K_M.gguf"), b"fake gguf").unwrap();
        std::fs::write(dir.join("model.safetensors"), b"fake st").unwrap();
        std::fs::write(dir.join("readme.txt"), b"ignore").unwrap();

        let manager = ModelManager::new(&dir);
        let models = manager.list_cached_models().unwrap();

        assert_eq!(models.len(), 2);
        assert!(models.iter().any(|m| m.format == ModelFormat::GGUF));
        assert!(models.iter().any(|m| m.format == ModelFormat::SafeTensors));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_cache_size() {
        let dir = std::env::temp_dir().join("tentaflow_test_mm_size");
        std::fs::create_dir_all(&dir).ok();

        std::fs::write(dir.join("a.gguf"), vec![0u8; 1024]).unwrap();
        std::fs::write(dir.join("b.gguf"), vec![0u8; 2048]).unwrap();

        let manager = ModelManager::new(&dir);
        let size = manager.cache_size().unwrap();
        assert_eq!(size, 3072);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_delete_model() {
        let dir = std::env::temp_dir().join("tentaflow_test_mm_delete");
        std::fs::create_dir_all(&dir).ok();

        std::fs::write(dir.join("to_delete.gguf"), b"data").unwrap();

        let manager = ModelManager::new(&dir);
        assert!(manager.delete_model("to_delete.gguf").is_ok());
        assert!(!dir.join("to_delete.gguf").exists());

        // Proba usuniecia nieistniejacego pliku
        assert!(manager.delete_model("nonexistent.gguf").is_err());

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_get_model_path() {
        let dir = std::env::temp_dir().join("tentaflow_test_mm_path");
        std::fs::create_dir_all(&dir).ok();

        std::fs::write(dir.join("exists.gguf"), b"data").unwrap();

        let manager = ModelManager::new(&dir);
        assert!(manager.get_model_path("exists.gguf").is_some());
        assert!(manager.get_model_path("nope.gguf").is_none());

        std::fs::remove_dir_all(&dir).ok();
    }
}
