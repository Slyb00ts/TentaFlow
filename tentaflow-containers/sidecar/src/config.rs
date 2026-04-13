// =============================================================================
// Plik: config.rs
// Opis: Konfiguracja sidecara — wybor roli + parametry per-rola. Ladowana
//       z /data/config.toml (volume mount) z fallbackiem do config.default.toml.
// =============================================================================

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

/// Rola sidecara — okresla co robi z przychodzacymi requestami.
/// Wszystkie role dzielą wspolny QUIC server i format ModelRequest/ModelResponse.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Role {
    /// Forwarder do lokalnego HTTP API (vLLM, llama.cpp-server, sglang, sherpa-onnx-server).
    /// Kontener uruchamia silnik na `upstream_url`, sidecar tluczymaczy QUIC ↔ HTTP.
    ReverseProxy {
        /// URL lokalnego HTTP API np. "http://127.0.0.1:8000/v1"
        upstream_url: String,
        /// Timeout requestow (ms)
        #[serde(default = "default_timeout_ms")]
        timeout_ms: u64,
        /// Typ API — okresla format translacji QUIC → HTTP
        api: UpstreamApi,
    },
    /// Inferencja ONNX lokalnie w procesie sidecara — dla lekkich modeli
    /// (embeddings, reranker, sherpa-tts). Nie uruchamia osobnego procesu.
    OnnxInProcess {
        /// Sciezka do pliku modelu ONNX
        model_path: String,
        /// Typ zadania
        task: OnnxTask,
    },
    /// Pelny specjalizowany tryb (meeting bot itp.) — tryb nieabstrakcyjny,
    /// logika w osobnym module.
    TeamsBot,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpstreamApi {
    /// OpenAI-compatible: /v1/chat/completions, /v1/embeddings, /v1/audio/transcriptions
    OpenAi,
    /// llama.cpp native: /completion, /tokenize
    LlamaCpp,
    /// Sherpa ONNX HTTP serwer
    Sherpa,
    /// Custom HTTP — forwarduj surowo bez translacji
    RawHttp,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OnnxTask {
    Embedding,
    Reranking,
}

#[derive(Debug, Clone, Deserialize)]
pub struct QuicConfig {
    /// Port QUIC/UDP na ktorym sidecar nasluchuje
    #[serde(default = "default_quic_port")]
    pub port: u16,
    /// Sciezka do certyfikatu TLS (PEM). Jesli brak — wygeneruje self-signed.
    pub tls_cert: Option<String>,
    /// Sciezka do klucza prywatnego TLS (PEM).
    pub tls_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SidecarConfig {
    /// Nazwa instancji, publikowana w ServiceAnnounce (np. "llm-vllm-01")
    pub service_name: String,
    /// Lista aliasow modeli obslugiwanych przez ten sidecar — do rejestracji w routerze.
    #[serde(default)]
    pub model_aliases: Vec<String>,
    /// QUIC
    pub quic: QuicConfig,
    /// Rola
    pub role: Role,
}

fn default_quic_port() -> u16 {
    5000
}
fn default_timeout_ms() -> u64 {
    120_000
}

impl SidecarConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Nie udalo sie odczytac configu: {}", path.display()))?;
        let cfg: SidecarConfig = toml::from_str(&content)
            .with_context(|| format!("Blad parsowania TOML: {}", path.display()))?;
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_reverse_proxy_config() {
        let toml_str = r#"
service_name = "llm-vllm-01"
model_aliases = ["bielik-11b", "llama-3.1-8b"]

[quic]
port = 5000

[role]
kind = "reverse_proxy"
upstream_url = "http://127.0.0.1:8000/v1"
api = "open_ai"
"#;
        let cfg: SidecarConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.service_name, "llm-vllm-01");
        assert_eq!(cfg.model_aliases.len(), 2);
        assert_eq!(cfg.quic.port, 5000);
        match cfg.role {
            Role::ReverseProxy { upstream_url, api, .. } => {
                assert_eq!(upstream_url, "http://127.0.0.1:8000/v1");
                matches!(api, UpstreamApi::OpenAi);
            }
            _ => panic!("zla rola"),
        }
    }

    #[test]
    fn parse_onnx_config() {
        let toml_str = r#"
service_name = "embeddings-01"
model_aliases = ["embedding-gemma"]

[quic]
port = 5000

[role]
kind = "onnx_in_process"
model_path = "/data/models/embedding-gemma.onnx"
task = "embedding"
"#;
        let cfg: SidecarConfig = toml::from_str(toml_str).unwrap();
        match cfg.role {
            Role::OnnxInProcess { model_path, task } => {
                assert_eq!(model_path, "/data/models/embedding-gemma.onnx");
                matches!(task, OnnxTask::Embedding);
            }
            _ => panic!("zla rola"),
        }
    }

    #[test]
    fn parse_teams_bot_role() {
        let toml_str = r#"
service_name = "teams-bot-01"

[quic]
port = 5000

[role]
kind = "teams_bot"
"#;
        let cfg: SidecarConfig = toml::from_str(toml_str).unwrap();
        matches!(cfg.role, Role::TeamsBot);
    }
}
