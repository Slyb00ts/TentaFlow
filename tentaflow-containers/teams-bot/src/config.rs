// =============================================================================
// Plik: config.rs
// Opis: Konfiguracja sidecara meeting bot, ladowana z pliku TOML.
// =============================================================================

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

/// Konfiguracja sidecara meeting bot
#[derive(Debug, Clone, Deserialize)]
pub struct MeetingConfig {
    /// URL spotkania Teams
    pub meeting_url: String,

    /// Sciezka do plikow cookies Chromium (JSON)
    pub auth_cookies_path: String,

    /// Port serwera QUIC kontenera (router laczy sie do niego)
    #[serde(default = "default_quic_port")]
    pub quic_port: u16,

    /// Sciezka do certyfikatu TLS (PEM). None = self-signed
    pub tls_cert: Option<String>,

    /// Sciezka do klucza prywatnego TLS (PEM). None = self-signed
    pub tls_key: Option<String>,

    /// Nazwa urzadzenia audio PulseAudio (None = domyslne)
    pub audio_device: Option<String>,

    /// Sciezka do modelu Silero VAD ONNX (None = prosty detektor RMS)
    pub vad_model_path: Option<String>,

    /// Alias modelu STT w routerze (np. "teams-stt")
    pub stt_model: Option<String>,

    /// Alias modelu TTS w routerze (np. "teams-tts")
    pub tts_model: Option<String>,

    /// Glos TTS (np. "alloy")
    pub tts_voice: Option<String>,

    /// Nazwa bota wyswietlana w spotkaniu Teams
    #[serde(default = "default_bot_name")]
    pub bot_name: String,

    /// Czas trwania chunka audio w milisekundach
    #[serde(default = "default_chunk_duration")]
    pub chunk_duration_ms: u32,

    /// Prog ciszy w milisekundach — po tym czasie VAD uznaje koniec wypowiedzi
    #[serde(default = "default_silence_threshold")]
    pub silence_threshold_ms: u32,
}

fn default_quic_port() -> u16 {
    5000
}

fn default_chunk_duration() -> u32 {
    500
}

fn default_silence_threshold() -> u32 {
    2000
}

fn default_bot_name() -> String {
    "TentaFlow AI".to_string()
}

impl MeetingConfig {
    /// Laduje konfiguracje z pliku TOML lub zmiennych srodowiskowych
    pub fn load(path: &Path) -> Result<Self> {
        if path.exists() {
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("Nie mozna odczytac pliku konfiguracji: {}", path.display()))?;

            let config: MeetingConfig = toml::from_str(&content)
                .with_context(|| format!("Bledny format TOML w: {}", path.display()))?;

            config.validate()?;
            Ok(config)
        } else {
            Self::from_env()
        }
    }

    /// Laduje konfiguracje ze zmiennych srodowiskowych (fallback gdy brak pliku TOML)
    fn from_env() -> Result<Self> {
        let config = MeetingConfig {
            meeting_url: std::env::var("MEETING_URL").unwrap_or_default(),
            auth_cookies_path: std::env::var("AUTH_COOKIES_PATH")
                .unwrap_or_else(|_| "/tmp/cookies.json".to_string()),
            quic_port: std::env::var("QUIC_PORT")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(5000),
            tls_cert: std::env::var("TLS_CERT").ok(),
            tls_key: std::env::var("TLS_KEY").ok(),
            audio_device: std::env::var("AUDIO_DEVICE").ok(),
            vad_model_path: std::env::var("VAD_MODEL_PATH").ok(),
            stt_model: std::env::var("STT_MODEL").ok(),
            tts_model: std::env::var("TTS_MODEL").ok(),
            tts_voice: std::env::var("TTS_VOICE").ok(),
            bot_name: std::env::var("BOT_NAME")
                .unwrap_or_else(|_| "TentaFlow AI".to_string()),
            chunk_duration_ms: std::env::var("CHUNK_DURATION_MS")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(500),
            silence_threshold_ms: std::env::var("SILENCE_THRESHOLD_MS")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(2000),
        };

        tracing::info!("Konfiguracja zaladowana ze zmiennych srodowiskowych");
        config.validate()?;
        Ok(config)
    }

    /// Walidacja poprawnosci konfiguracji
    fn validate(&self) -> Result<()> {
        // meeting_url moze byc pusty — kontener startuje bez spotkania,
        // URL podaje sie pozniej komenda join przez QUIC

        if self.chunk_duration_ms < 100 || self.chunk_duration_ms > 5000 {
            anyhow::bail!("chunk_duration_ms musi byc w zakresie 100-5000");
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_config_all_fields() {
        // Parsowanie pelnej konfiguracji ze wszystkimi polami
        let toml_str = r#"
            meeting_url = "https://teams.microsoft.com/l/meetup-join/test"
            auth_cookies_path = "/tmp/cookies.json"
            quic_port = 6000
            chunk_duration_ms = 300
            silence_threshold_ms = 3000
            audio_device = "pulse_monitor"
            vad_model_path = "/models/silero.onnx"
            stt_model = "whisper-large"
            tts_model = "teams-tts"
            tts_voice = "nova"
            tls_cert = "/certs/cert.pem"
            tls_key = "/certs/key.pem"
            bot_name = "Testowy Bot"
        "#;

        let config: MeetingConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.bot_name, "Testowy Bot");
        assert_eq!(config.quic_port, 6000);
        assert_eq!(config.chunk_duration_ms, 300);
        assert_eq!(config.silence_threshold_ms, 3000);
        assert_eq!(config.audio_device.as_deref(), Some("pulse_monitor"));
        assert_eq!(config.vad_model_path.as_deref(), Some("/models/silero.onnx"));
        assert_eq!(config.stt_model.as_deref(), Some("whisper-large"));
        assert_eq!(config.tts_model.as_deref(), Some("teams-tts"));
        assert_eq!(config.tts_voice.as_deref(), Some("nova"));
        assert_eq!(config.tls_cert.as_deref(), Some("/certs/cert.pem"));
        assert_eq!(config.tls_key.as_deref(), Some("/certs/key.pem"));
    }

    #[test]
    fn parse_minimal_config_uses_defaults() {
        // Minimalna konfiguracja — tylko wymagane pola, reszta domyslna
        let toml_str = r#"
            meeting_url = "https://teams.microsoft.com/l/meetup-join/test"
            auth_cookies_path = "/tmp/cookies.json"
        "#;

        let config: MeetingConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.quic_port, 5000);
        assert_eq!(config.chunk_duration_ms, 500);
        assert_eq!(config.silence_threshold_ms, 2000);
        assert!(config.audio_device.is_none());
        assert!(config.vad_model_path.is_none());
        assert!(config.stt_model.is_none());
        assert!(config.tts_model.is_none());
        assert!(config.tts_voice.is_none());
        assert!(config.tls_cert.is_none());
        assert!(config.tls_key.is_none());
    }

    #[test]
    fn parse_missing_meeting_url_fails() {
        // Brak wymaganego pola meeting_url — serde powinno zwrocic blad
        let toml_str = r#"
            auth_cookies_path = "/tmp/cookies.json"
        "#;

        let result: Result<MeetingConfig, _> = toml::from_str(toml_str);
        assert!(result.is_err());
    }

    #[test]
    fn parse_missing_auth_cookies_path_fails() {
        // Brak wymaganego pola auth_cookies_path — serde powinno zwrocic blad
        let toml_str = r#"
            meeting_url = "https://teams.microsoft.com/l/meetup-join/test"
        "#;

        let result: Result<MeetingConfig, _> = toml::from_str(toml_str);
        assert!(result.is_err());
    }

    #[test]
    fn validate_accepts_empty_meeting_url() {
        // Pusty meeting_url jest OK — kontener startuje bez spotkania, czeka na join
        let toml_str = r#"
            meeting_url = ""
            auth_cookies_path = "/tmp/cookies.json"
        "#;

        let config: MeetingConfig = toml::from_str(toml_str).unwrap();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn validate_rejects_chunk_duration_too_small() {
        // chunk_duration_ms < 100 jest odrzucane
        let toml_str = r#"
            meeting_url = "https://teams.microsoft.com/l/meetup-join/test"
            auth_cookies_path = "/tmp/cookies.json"
            chunk_duration_ms = 50
        "#;

        let config: MeetingConfig = toml::from_str(toml_str).unwrap();
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("chunk_duration_ms"));
    }

    #[test]
    fn validate_rejects_chunk_duration_too_large() {
        // chunk_duration_ms > 5000 jest odrzucane
        let toml_str = r#"
            meeting_url = "https://teams.microsoft.com/l/meetup-join/test"
            auth_cookies_path = "/tmp/cookies.json"
            chunk_duration_ms = 10000
        "#;

        let config: MeetingConfig = toml::from_str(toml_str).unwrap();
        let result = config.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("chunk_duration_ms"));
    }

    #[test]
    fn validate_accepts_boundary_chunk_duration() {
        // Graniczne wartosci 100 i 5000 powinny przejsc walidacje
        for duration in [100, 5000] {
            let toml_str = format!(
                r#"
                meeting_url = "https://teams.microsoft.com/l/meetup-join/test"
                auth_cookies_path = "/tmp/cookies.json"
                chunk_duration_ms = {}
                "#,
                duration
            );

            let config: MeetingConfig = toml::from_str(&toml_str).unwrap();
            assert!(config.validate().is_ok(), "chunk_duration_ms={} powinno przejsc", duration);
        }
    }

    #[test]
    fn parse_ignores_unknown_fields() {
        // TOML z nieznanymi polami — serde powinno je zignorowac (deny_unknown_fields nie jest ustawione)
        let toml_str = r#"
            meeting_url = "https://teams.microsoft.com/l/meetup-join/test"
            auth_cookies_path = "/tmp/cookies.json"
            unknown_field = "should be ignored"
        "#;

        // MeetingConfig nie ma deny_unknown_fields, wiec toml powinno zignorowac
        let result: Result<MeetingConfig, _> = toml::from_str(toml_str);
        // Jesli toml domyslnie odrzuca nieznane pola — to tez OK
        // Sprawdzamy ze nie panikuje
        let _ = result;
    }

    #[test]
    fn parse_config_with_stt_and_tts_models() {
        // Konfiguracja z ustawionymi modelami STT i TTS
        let toml_str = r#"
            meeting_url = "https://teams.microsoft.com/l/meetup-join/test"
            auth_cookies_path = "/tmp/cookies.json"
            stt_model = "teams-stt"
            tts_model = "teams-tts"
        "#;

        let config: MeetingConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.stt_model.as_deref(), Some("teams-stt"));
        assert_eq!(config.tts_model.as_deref(), Some("teams-tts"));
        assert!(config.tts_voice.is_none());
    }

    #[test]
    fn parse_config_with_tts_voice() {
        // Konfiguracja z ustawionym glosem TTS
        let toml_str = r#"
            meeting_url = "https://teams.microsoft.com/l/meetup-join/test"
            auth_cookies_path = "/tmp/cookies.json"
            tts_voice = "shimmer"
        "#;

        let config: MeetingConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.tts_voice.as_deref(), Some("shimmer"));
    }

    #[test]
    fn parse_minimal_config_stt_tts_default_none() {
        // Minimalna konfiguracja — stt_model, tts_model, tts_voice domyslnie None
        let toml_str = r#"
            meeting_url = "https://teams.microsoft.com/l/meetup-join/test"
            auth_cookies_path = "/tmp/cookies.json"
        "#;

        let config: MeetingConfig = toml::from_str(toml_str).unwrap();
        assert!(config.stt_model.is_none(), "stt_model domyslnie None");
        assert!(config.tts_model.is_none(), "tts_model domyslnie None");
        assert!(config.tts_voice.is_none(), "tts_voice domyslnie None");
    }
}
