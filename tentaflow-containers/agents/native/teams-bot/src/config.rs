// =============================================================================
// Plik: config.rs
// Opis: Konfiguracja sidecara meeting bot, ladowana z pliku TOML lub env.
//       Bot operuje na aliasach serwisow (stt/tts/summarization/flow) —
//       rozwiazanie aliasu na konkretny silnik/voice/model wykonuje router.
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

    /// Port UDP na ktorym iroh endpoint nasluchuje (router laczy sie po EndpointId).
    #[serde(default = "default_transport_port")]
    pub transport_port: u16,

    /// Sciezka do pliku z Ed25519 secret key (32 bajty raw). None = ephemeral.
    pub secret_key_path: Option<String>,

    /// Ed25519 secret key w formacie hex (64 znaki) — priorytet nad secret_key_path.
    /// Uzywany przez MeetingManager ktory generuje klucz i przekazuje env-em do kontenera.
    pub secret_key_hex: Option<String>,

    /// Stały klucz sesji/meeting_id przekazywany przez host. Uzywany do dopasowania
    /// sesji w `meeting_sessions` tworzonej przez MeetingManager — router zapisze
    /// transkrypty pod tym kluczem. None = bot wygeneruje uuid przy kazdym join.
    pub meeting_id_override: Option<String>,

    /// Wlacza LAN mDNS discovery. Domyslnie false bo teams-bot jest serwerem
    /// QUIC do ktorego host laczy sie po znanym endpoint_id + porcie — nie
    /// potrzebuje byc discoverable, a mDNS broadcast na docker bridge powodowal
    /// ze host widzial go jako kandydata do parowania w mesh.
    #[serde(default)]
    pub enable_lan_discovery: bool,

    /// Wlacza DHT pkarr-mainline. Domyslnie false z tego samego powodu co wyzej.
    #[serde(default)]
    pub enable_dht_discovery: bool,

    /// Nazwa urzadzenia audio PulseAudio (None = domyslne)
    pub audio_device: Option<String>,

    /// Sciezka do modelu Silero VAD ONNX (None = prosty detektor RMS)
    pub vad_model_path: Option<String>,

    /// Alias serwisu STT w routerze.
    #[serde(default = "default_stt_alias")]
    pub stt_alias: String,

    /// Alias serwisu summarization w routerze.
    #[serde(default = "default_summarization_alias")]
    pub summarization_alias: String,

    /// Alias serwisu TTS w routerze.
    #[serde(default = "default_tts_alias")]
    pub tts_alias: String,

    /// Alias LLM uzywany do generowania ODPOWIEDZI bota w real-time
    /// (oddzielny od `summarization_alias` ktory tylko robi summary).
    /// Pusty string lub brak modelu wpiętego pod ten alias = bot nie odpowiada.
    #[serde(default = "default_llm_alias")]
    pub llm_alias: String,

    /// Alias flow w routerze (rezerwacja — flow jest rozwiazywany przez router,
    /// bot trzyma pole dla spojnosci i przyszlego uzycia).
    #[serde(default = "default_flow_alias")]
    pub flow_alias: String,

    /// Czy bot ma odpowiadac w meeting'u (LLM -> TTS). Wymaga `llm_alias`
    /// wpietego na model. Default false zeby starsze deploymenty nie zaczęly
    /// nagle rozmawiać.
    #[serde(default)]
    pub respond_enabled: bool,

    /// Tryb aktywacji odpowiedzi:
    ///   - `always` — kazda wypowiedz idzie do LLM (drogie, glosne, debug)
    ///   - `wake_word` — tylko gdy `wake_word` w wypowiedzi (proste)
    ///   - `wake_word_intent` — wake_word + lokalny klasyfikator intencji
    ///     (regex/keyword, mikrosekundy — patrz `intent_classifier.rs`).
    ///     Akceptuje pytania ('?'), czasowniki rozkazujace i dluzsze
    ///     wypowiedzi; odrzuca krotkie powitania (<=3 slowa).
    /// Caller dashboard moze nadpisac envem RESPONSE_MODE.
    #[serde(default = "default_response_mode")]
    pub response_mode: String,

    /// Slowa kluczowe (po przecinku) ktore aktywuja odpowiedz.
    /// Dopasowanie case-insensitive, fragment slowa wystarczy. Pusta lista =
    /// zawsze aktywne (rownowazne z `response_mode=always`).
    #[serde(default = "default_wake_words")]
    pub wake_words: String,

    /// Lista wake_words po splitcie/trim/lowercase, kompilowana raz w
    /// `validate()`. Trzymana zeby `matches_wake_word` na hot-pathie STT
    /// nie alokowal Vec/String per kazda wypowiedz. `#[serde(skip)]` —
    /// zrodlem prawdy jest `wake_words` (CSV z TOML/env).
    #[serde(skip)]
    pub wake_words_compiled: Vec<String>,

    /// Systemowy prompt dla LLM odpowiadajacego (rola bota w spotkaniu).
    #[serde(default = "default_response_prompt")]
    pub response_prompt: String,

    /// Nazwa bota wyswietlana w spotkaniu Teams
    #[serde(default = "default_bot_name")]
    pub bot_name: String,

    /// Czas trwania chunka audio w milisekundach
    #[serde(default = "default_chunk_duration")]
    pub chunk_duration_ms: u32,

    /// Prog ciszy w milisekundach — po tym czasie VAD uznaje koniec wypowiedzi
    #[serde(default = "default_silence_threshold")]
    pub silence_threshold_ms: u32,

    /// Prog RMS powyzej ktorego VAD uznaje za mowe (uzywany gdy brak modelu Silero)
    #[serde(default = "default_vad_rms_threshold")]
    pub vad_rms_threshold: f32,

    /// Czy bot ma dolaczac z wlaczona kamerka (generowana z canvas przez MSTG).
    /// Default true: the canvas avatar is the visible identity of the bot in
    /// the meeting tile, so we want it on unless the deployment explicitly
    /// disables it. Falls back to "Continue without audio or video" when MSTG
    /// or OffscreenCanvas are not available in the Chromium build.
    #[serde(default = "default_bot_video_enabled")]
    pub bot_video_enabled: bool,

    /// Echo mode — gdy true, TTS wypowiada transkrypt ze STT (tryb testowy).
    /// Domyslnie false, bo bez tego powstaje feedback loop: bot slyszy wlasny glos
    /// przez glosniki/echo Teams i transkrybuje go ponownie.
    #[serde(default)]
    pub echo_mode: bool,

    /// Co ile sekund summarizer generuje podsumowanie z rolling bufferu
    /// transkrypcji i wysyla MeetingEvent do routera.
    #[serde(default = "default_summarization_interval_sec")]
    pub summarization_interval_sec: u64,

    /// Ile minut historii transkrypcji trzymamy w rolling bufferze. Starsze wpisy
    /// sa odrzucane. LLM dostaje okno z ostatnich N minut — bez tego dlugie
    /// spotkania dawalyby context overflow.
    #[serde(default = "default_transcript_buffer_minutes")]
    pub transcript_buffer_minutes: u64,

    /// Minimalna liczba wpisow transkrypcji w bufferze zanim summarizer
    /// uruchomi LLM. Zapobiega generowaniu na pustce (1-2 zdania → slaby JSON).
    #[serde(default = "default_summarization_min_entries")]
    pub summarization_min_entries: usize,

    /// Jezyk prompta transcription_summarization (pl/en/de/es/fr). Dopasowany
    /// do seeda w DB — patrz tentaflow-core/src/db/seed.rs.
    #[serde(default = "default_meeting_language")]
    pub meeting_language: String,
}

fn default_transport_port() -> u16 {
    5000
}

fn default_chunk_duration() -> u32 {
    250
}

fn default_silence_threshold() -> u32 {
    500
}

fn default_vad_rms_threshold() -> f32 {
    100.0
}

fn default_bot_name() -> String {
    "TentaFlow Jarvis".to_string()
}

fn default_bot_video_enabled() -> bool {
    true
}

fn default_stt_alias() -> String {
    "teams-stt".to_string()
}

fn default_summarization_alias() -> String {
    "teams-summarization".to_string()
}

fn default_tts_alias() -> String {
    "teams-tts".to_string()
}

fn default_flow_alias() -> String {
    "teams-flow".to_string()
}

fn default_llm_alias() -> String {
    "teams-llm".to_string()
}

fn default_response_prompt() -> String {
    // ZAWSZE odpowiadaj. Bez instrukcji ucieczki typu `<NO_RESPONSE>` ani
    // klauzul "tylko gdy zadaje pytanie" — bot dostaje wylacznie wypowiedzi
    // ze slowem aktywujacym (response_mode=wake_word), wiec mowca jasno
    // skierowal sie do bota i oczekuje reakcji. Krotko, po polsku, do rzeczy.
    "Jestes asystentem na spotkaniu Teams. Odpowiadaj zawsze, po polsku, \
1-2 zdaniami."
        .to_string()
}

fn default_response_mode() -> String {
    // Default = sam wake_word: bot zawsze odpowiada gdy w transkrypcie
    // pojawi sie slowo aktywujace. `wake_word_intent` jest dostepny dla
    // operatorow ktorzy chca dodatkowo odsiac krotkie powitania —
    // klasyfikacja jest lokalna (regex/keyword), bez LLM call.
    "wake_word".to_string()
}

fn default_wake_words() -> String {
    // Domyslnie pasujemy na imie bota (jarvis), kilka wariantow.
    "jarvis,tentaflow,asystencie,asystent,bot".to_string()
}

fn default_summarization_interval_sec() -> u64 {
    60
}

fn default_transcript_buffer_minutes() -> u64 {
    10
}

fn default_summarization_min_entries() -> usize {
    3
}

fn default_meeting_language() -> String {
    "pl".to_string()
}

/// Domyslna sciezka do modelu Silero VAD. Sprawdza po kolei:
/// 1. `/opt/models/silero_vad.onnx` (Docker)
/// 2. plik obok binarki `tentaflow-meeting`/`silero_vad.onnx` (native install)
/// 3. `<cache_dir>/tentaflow/teams-bot/silero_vad.onnx` (native fallback)
/// Zwraca pierwsza istniejaca sciezke, albo None — wtedy VadDetector przejdzie
/// na RMS fallback (gorsza jakosc, ale bot dziala).
fn default_vad_model_path() -> Option<String> {
    let docker_path = std::path::PathBuf::from("/opt/models/silero_vad.onnx");
    if docker_path.is_file() {
        return Some(docker_path.to_string_lossy().into_owned());
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("silero_vad.onnx");
            if candidate.is_file() {
                return Some(candidate.to_string_lossy().into_owned());
            }
            // Fallback dwa poziomy w gore — gdy binarka jest w target/release/
            // a model jest obok manifestu w native/teams-bot/models/.
            let near_repo = dir.join("models").join("silero_vad.onnx");
            if near_repo.is_file() {
                return Some(near_repo.to_string_lossy().into_owned());
            }
        }
    }
    if let Some(cache) = dirs_cache_dir() {
        let candidate = cache
            .join("tentaflow")
            .join("teams-bot")
            .join("silero_vad.onnx");
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

/// Lekka kopia `dirs::cache_dir()` zeby nie wciagac calej zaleznosci `dirs`
/// tylko po jeden lookup. Zwraca rozsadny default cache dir per platforma.
fn dirs_cache_dir() -> Option<std::path::PathBuf> {
    if let Ok(home) = std::env::var("HOME") {
        if cfg!(target_os = "macos") {
            return Some(std::path::PathBuf::from(home).join("Library").join("Caches"));
        }
        if cfg!(target_os = "linux") {
            if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
                return Some(std::path::PathBuf::from(xdg));
            }
            return Some(std::path::PathBuf::from(home).join(".cache"));
        }
    }
    if cfg!(target_os = "windows") {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            return Some(std::path::PathBuf::from(local));
        }
    }
    None
}

impl MeetingConfig {
    /// Laduje konfiguracje z pliku TOML lub zmiennych srodowiskowych
    pub fn load(path: &Path) -> Result<Self> {
        if path.exists() {
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("Nie mozna odczytac pliku konfiguracji: {}", path.display()))?;

            // Pusty plik / specjalny `/dev/null` -> traktuj jak brak konfigu i
            // uzyj env-owej sciezki. Native subprocess bota wskazuje na pusty
            // sentinel zeby wymusic from_env (Docker uzywa tylko env, ale w
            // niektorych testach plik moze byc pusty).
            if content.trim().is_empty() {
                return Self::from_env();
            }

            let mut config: MeetingConfig = toml::from_str(&content)
                .with_context(|| format!("Bledny format TOML w: {}", path.display()))?;

            config.validate()?;
            Ok(config)
        } else {
            Self::from_env()
        }
    }

    /// Laduje konfiguracje ze zmiennych srodowiskowych (fallback gdy brak pliku TOML)
    fn from_env() -> Result<Self> {
        let mut config = MeetingConfig {
            meeting_url: std::env::var("MEETING_URL").unwrap_or_default(),
            auth_cookies_path: std::env::var("AUTH_COOKIES_PATH")
                .unwrap_or_else(|_| "/tmp/cookies.json".to_string()),
            transport_port: std::env::var("TRANSPORT_PORT")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(5000),
            secret_key_path: std::env::var("SECRET_KEY_PATH").ok(),
            secret_key_hex: std::env::var("BOT_SECRET_KEY_HEX").ok(),
            meeting_id_override: std::env::var("MEETING_ID").ok(),
            enable_lan_discovery: std::env::var("ENABLE_LAN_DISCOVERY")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(false),
            enable_dht_discovery: std::env::var("ENABLE_DHT_DISCOVERY")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(false),
            audio_device: std::env::var("AUDIO_DEVICE").ok(),
            vad_model_path: std::env::var("VAD_MODEL_PATH")
                .ok()
                .or_else(default_vad_model_path),
            stt_alias: std::env::var("STT_ALIAS")
                .unwrap_or_else(|_| "teams-stt".to_string()),
            summarization_alias: std::env::var("SUMMARIZATION_ALIAS")
                .unwrap_or_else(|_| "teams-summarization".to_string()),
            tts_alias: std::env::var("TTS_ALIAS")
                .unwrap_or_else(|_| "teams-tts".to_string()),
            flow_alias: std::env::var("FLOW_ALIAS")
                .unwrap_or_else(|_| "teams-flow".to_string()),
            llm_alias: std::env::var("LLM_ALIAS")
                .unwrap_or_else(|_| "teams-llm".to_string()),
            respond_enabled: std::env::var("RESPOND_ENABLED")
                .ok()
                .map(|v| matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on"))
                .unwrap_or(false),
            response_prompt: std::env::var("RESPONSE_PROMPT")
                .unwrap_or_else(|_| default_response_prompt()),
            response_mode: std::env::var("RESPONSE_MODE")
                .unwrap_or_else(|_| default_response_mode()),
            wake_words: std::env::var("WAKE_WORDS")
                .unwrap_or_else(|_| default_wake_words()),
            wake_words_compiled: Vec::new(),
            bot_name: std::env::var("BOT_NAME")
                .unwrap_or_else(|_| "TentaFlow Jarvis".to_string()),
            chunk_duration_ms: std::env::var("CHUNK_DURATION_MS")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(250),
            silence_threshold_ms: std::env::var("SILENCE_THRESHOLD_MS")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(500),
            vad_rms_threshold: std::env::var("VAD_RMS_THRESHOLD")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(100.0),
            // Default true: the canvas avatar is the visible identity of the
            // bot in the meeting tile. Without it Teams renders a generic
            // initials circle and the user can't tell the bot ever joined.
            // Operators can still flip it off via BOT_VIDEO_ENABLED=false.
            bot_video_enabled: std::env::var("BOT_VIDEO_ENABLED")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(true),
            echo_mode: std::env::var("ECHO_MODE")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(false),
            summarization_interval_sec: std::env::var("SUMMARIZATION_INTERVAL_SEC")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(60),
            transcript_buffer_minutes: std::env::var("TRANSCRIPT_BUFFER_MINUTES")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(10),
            summarization_min_entries: std::env::var("SUMMARIZATION_MIN_ENTRIES")
                .ok().and_then(|v| v.parse().ok()).unwrap_or(3),
            meeting_language: std::env::var("MEETING_LANGUAGE")
                .unwrap_or_else(|_| "pl".to_string()),
        };

        tracing::info!("Konfiguracja zaladowana ze zmiennych srodowiskowych");
        config.validate()?;
        Ok(config)
    }

    /// Walidacja poprawnosci konfiguracji. Przy okazji kompiluje
    /// `wake_words_compiled` z CSV `wake_words` raz, zeby hot-path
    /// `matches_wake_word` nie powtarzal split/trim/lowercase per
    /// kazdy STT result.
    fn validate(&mut self) -> Result<()> {
        // meeting_url moze byc pusty — kontener startuje bez spotkania,
        // URL podaje sie pozniej komenda join przez QUIC

        if self.chunk_duration_ms < 100 || self.chunk_duration_ms > 5000 {
            anyhow::bail!("chunk_duration_ms musi byc w zakresie 100-5000");
        }

        self.wake_words_compiled = self
            .wake_words
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_config_all_fields() {
        let toml_str = r#"
            meeting_url = "https://teams.microsoft.com/l/meetup-join/test"
            auth_cookies_path = "/tmp/cookies.json"
            transport_port = 6000
            chunk_duration_ms = 300
            silence_threshold_ms = 3000
            audio_device = "pulse_monitor"
            vad_model_path = "/models/silero.onnx"
            stt_alias = "custom-stt"
            tts_alias = "custom-tts"
            summarization_alias = "custom-sum"
            flow_alias = "custom-flow"
            secret_key_path = "/data/endpoint-key.bin"
            bot_name = "Testowy Bot"
        "#;

        let config: MeetingConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.bot_name, "Testowy Bot");
        assert_eq!(config.transport_port, 6000);
        assert_eq!(config.chunk_duration_ms, 300);
        assert_eq!(config.silence_threshold_ms, 3000);
        assert_eq!(config.audio_device.as_deref(), Some("pulse_monitor"));
        assert_eq!(config.vad_model_path.as_deref(), Some("/models/silero.onnx"));
        assert_eq!(config.stt_alias, "custom-stt");
        assert_eq!(config.tts_alias, "custom-tts");
        assert_eq!(config.summarization_alias, "custom-sum");
        assert_eq!(config.flow_alias, "custom-flow");
        assert_eq!(config.secret_key_path.as_deref(), Some("/data/endpoint-key.bin"));
    }

    #[test]
    fn parse_minimal_config_uses_defaults() {
        let toml_str = r#"
            meeting_url = "https://teams.microsoft.com/l/meetup-join/test"
            auth_cookies_path = "/tmp/cookies.json"
        "#;

        let config: MeetingConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.transport_port, 5000);
        assert_eq!(config.chunk_duration_ms, 250);
        assert_eq!(config.silence_threshold_ms, 500);
        assert!(config.audio_device.is_none());
        assert!(config.vad_model_path.is_none());
        assert_eq!(config.stt_alias, "teams-stt");
        assert_eq!(config.tts_alias, "teams-tts");
        assert_eq!(config.summarization_alias, "teams-summarization");
        assert_eq!(config.flow_alias, "teams-flow");
        assert!(config.secret_key_path.is_none());
        assert!(!config.enable_lan_discovery);
        assert!(!config.enable_dht_discovery);
        assert!(config.bot_video_enabled, "bot_video_enabled domyslnie true (canvas avatar)");
        assert!(!config.echo_mode, "echo_mode domyslnie false");
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

        let mut config: MeetingConfig = toml::from_str(toml_str).unwrap();
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

        let mut config: MeetingConfig = toml::from_str(toml_str).unwrap();
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

        let mut config: MeetingConfig = toml::from_str(toml_str).unwrap();
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

            let mut config: MeetingConfig = toml::from_str(&toml_str).unwrap();
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

        let result: Result<MeetingConfig, _> = toml::from_str(toml_str);
        let _ = result;
    }

    #[test]
    fn parse_config_with_custom_aliases() {
        // Konfiguracja z ustawionymi aliasami STT/TTS/summarization/flow
        let toml_str = r#"
            meeting_url = "https://teams.microsoft.com/l/meetup-join/test"
            auth_cookies_path = "/tmp/cookies.json"
            stt_alias = "prod-stt"
            tts_alias = "prod-tts"
            summarization_alias = "prod-sum"
            flow_alias = "prod-flow"
        "#;

        let config: MeetingConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.stt_alias, "prod-stt");
        assert_eq!(config.tts_alias, "prod-tts");
        assert_eq!(config.summarization_alias, "prod-sum");
        assert_eq!(config.flow_alias, "prod-flow");
    }

    #[test]
    fn parse_minimal_config_aliases_use_defaults() {
        // Minimalna konfiguracja — aliasy przyjmuja wartosci domyslne
        let toml_str = r#"
            meeting_url = "https://teams.microsoft.com/l/meetup-join/test"
            auth_cookies_path = "/tmp/cookies.json"
        "#;

        let config: MeetingConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(config.stt_alias, "teams-stt");
        assert_eq!(config.tts_alias, "teams-tts");
        assert_eq!(config.summarization_alias, "teams-summarization");
        assert_eq!(config.flow_alias, "teams-flow");
    }
}
