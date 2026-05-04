// =============================================================================
// Plik: services/tts/client.rs
// Opis: HTTP client dla TTS API — wysyla tekst do backendu TTS (np. OpenAI TTS)
//       i zwraca audio bytes gotowe do streaming do klienta.
// =============================================================================

use crate::error::{CoreError, Result};

use crate::api::openai::types::TTSRequest;

use reqwest::Client;
use std::time::Duration;
use tracing::debug;

/// Konfiguracja TTS (compatibility type dla starego API)
#[derive(Clone)]
pub struct TTSConfigCompat {
    pub url: String,
    pub api_key: Option<String>,
    pub api_key_env: Option<String>,
    pub model: String,
    pub voice: String,
    pub response_format: String,
    pub speed: f32,
    pub timeout_ms: u64,
}

impl std::fmt::Debug for TTSConfigCompat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TTSConfigCompat")
            .field("url", &self.url)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("api_key_env", &self.api_key_env)
            .field("model", &self.model)
            .field("voice", &self.voice)
            .field("response_format", &self.response_format)
            .field("speed", &self.speed)
            .field("timeout_ms", &self.timeout_ms)
            .finish()
    }
}

/// HTTP Client dla TTS API (Text-to-Speech).
///
/// Podobny do BackendClient ale specjalizowany dla TTS operations.
/// Uzywa reqwest::Client z connection pooling i timeout.
#[derive(Clone)]
pub struct TTSClient {
    /// Konfiguracja TTS
    config: TTSConfigCompat,

    /// HTTP client (reqwest) - reusable
    client: Client,

    /// Pre-zbudowany base URL dla /audio/speech
    base_url: String,

    /// Pre-zbudowany naglowek Authorization
    auth_header: String,
}

impl TTSClient {
    /// Tworzy nowy TTS client.
    ///
    /// Wczytuje API key z config.api_key (priorytet) lub ze zmiennej srodowiskowej
    /// (config.api_key_env). Konfiguruje reqwest::Client z timeout dostosowanym dla TTS.
    pub fn new(config: TTSConfigCompat) -> Result<Self> {
        // Wczytaj API key: priorytet dla direct key, fallback do env var
        let api_key = if let Some(ref key) = config.api_key {
            key.clone()
        } else if let Some(ref env_var) = config.api_key_env {
            std::env::var(env_var).map_err(|_| CoreError::ConfigError {
                message: format!(
                    "Zmienna srodowiskowa '{}' nie jest ustawiona (TTS API key)",
                    env_var
                ),
                source: anyhow::anyhow!("Missing TTS API key env var"),
            })?
        } else {
            return Err(CoreError::ConfigError {
                message: "Brak api_key ani api_key_env w konfiguracji TTS".to_string(),
                source: anyhow::anyhow!("No TTS API key configured"),
            }
            .into());
        };

        // Utwórz reqwest::Client z timeout
        let client = Client::builder()
            .timeout(Duration::from_millis(config.timeout_ms))
            .connect_timeout(Duration::from_secs(5))
            .pool_max_idle_per_host(2)
            .build()
            .map_err(|e| CoreError::InternalError {
                message: "Nie mozna utworzyc TTS HTTP client".to_string(),
                source: Some(e.into()),
            })?;

        debug!(
            "TTS client utworzony: {} (model: {}, voice: {}, format: {})",
            config.url, config.model, config.voice, config.response_format
        );

        let base_url = format!("{}/audio/speech", config.url.trim_end_matches('/'));
        let auth_header = format!("Bearer {}", api_key);

        Ok(Self {
            config,
            client,
            base_url,
            auth_header,
        })
    }

    /// Konwertuje tekst na audio bytes (synteza mowy).
    ///
    /// Wysyla request do TTS API i zwraca raw audio bytes gotowe
    /// do streaming do klienta jako AudioChunk.
    pub async fn synthesize(&self, text: &str) -> Result<Vec<u8>> {
        self.synthesize_with_options(text, None, None, None).await
    }

    /// Synteza mowy z opcjonalnymi parametrami (voice, format, speed).
    /// Jesli parametr jest None, uzywa wartosci z config.
    pub async fn synthesize_with_options(
        &self,
        text: &str,
        voice: Option<&str>,
        format: Option<&str>,
        speed: Option<f32>,
    ) -> Result<Vec<u8>> {
        if text.trim().is_empty() {
            return Err(CoreError::InvalidRequest {
                message: "Tekst do syntezy mowy nie moze byc pusty".to_string(),
                details: None,
            }
            .into());
        }

        let voice = voice.unwrap_or(&self.config.voice);
        let format = format.unwrap_or(&self.config.response_format);
        let speed = speed.unwrap_or(self.config.speed);

        debug!(
            "Synteza mowy: {} znakow, voice={}, format={} -> {}",
            text.len(),
            voice,
            format,
            self.base_url
        );

        // Utwórz TTS request
        let request = TTSRequest {
            model: self.config.model.clone(),
            input: text.to_string(),
            voice: voice.to_string(),
            response_format: Some(format.to_string()),
            speed: Some(speed),
            language: None,
        };

        // Wyslij POST request
        let response = self
            .client
            .post(&self.base_url)
            .header("Authorization", &self.auth_header)
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| self.map_reqwest_error(e))?;

        let status = response.status();
        debug!("TTS response status: {}", status);

        // Sprawdz status code
        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_else(|_| String::new());
            return Err(CoreError::BackendError {
                backend_url: self.config.url.clone(),
                message: format!("TTS API error ({}): {}", status, error_body),
                source: None,
            }
            .into());
        }

        // Sprawdz rozmiar odpowiedzi przed pobraniem (max 50MB)
        const MAX_RESPONSE_SIZE: u64 = 50 * 1024 * 1024;
        if let Some(content_length) = response.content_length() {
            if content_length > MAX_RESPONSE_SIZE {
                return Err(CoreError::BackendError {
                    backend_url: self.config.url.clone(),
                    message: format!(
                        "Odpowiedz TTS przekracza limit rozmiaru: {} bajtow (max {} bajtow)",
                        content_length, MAX_RESPONSE_SIZE
                    ),
                    source: None,
                }
                .into());
            }
        }

        // Przeczytaj audio bytes
        let audio_bytes = response
            .bytes()
            .await
            .map_err(|e| CoreError::BackendError {
                backend_url: self.config.url.clone(),
                message: format!("Nie mozna przeczytac audio bytes: {}", e),
                source: Some(e.into()),
            })?;

        debug!(
            "TTS synteza OK: {} bajtow audio (format: {})",
            audio_bytes.len(),
            self.config.response_format
        );

        Ok(audio_bytes.into())
    }

    /// Mapuje reqwest::Error na CoreError.
    fn map_reqwest_error(&self, err: reqwest::Error) -> CoreError {
        if err.is_timeout() {
            CoreError::Timeout {
                backend_url: self.config.url.clone(),
                timeout_ms: self.config.timeout_ms,
            }
        } else if err.is_connect() || err.is_request() {
            CoreError::NetworkError {
                message: format!("Blad polaczenia z TTS backend: {}", self.config.url),
                source: err.into(),
            }
        } else {
            CoreError::BackendError {
                backend_url: self.config.url.clone(),
                message: format!("Blad reqwest (TTS): {}", err),
                source: Some(err.into()),
            }
        }
    }

    /// Zwraca URL backendu TTS (dla logowania i debugowania)
    pub fn url(&self) -> &str {
        &self.config.url
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_tts_client_creation() {
        // Test wymaga TTS API key w zmiennej srodowiskowej
        // W prawdziwych testach uzyjemy mock servera (wiremock)
    }
}
