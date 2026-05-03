// =============================================================================
// Plik: config/mod.rs
// Opis: Konfiguracja wezla — NodeConfig (dawniej RouterConfig). Parsowanie i
//       walidacja config.toml. Obsluguje konfiguracje routera, mesh networking
//       oraz lokalnej inferencji.
// Przyklad:
//   let config = NodeConfig::from_file("config.toml")?;
//   println!("Port: {}", config.protocols.openai_api.bind);
// =============================================================================

use crate::error::{CoreError, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

// =============================================================================
// Alias kompatybilnosci — istniejacy kod uzywajacy RouterConfig dziala dalej
// =============================================================================

pub type RouterConfig = NodeConfig;

// =============================================================================
// Glowna struktura konfiguracji wezla
// =============================================================================

/// Glowna struktura konfiguracji wezla.
///
/// Odpowiada strukturze pliku config.toml. Wszystkie pola sa deserializowane
/// automatycznie przez serde z TOML. Dawniej `RouterConfig` — alias zachowany
/// dla backward compatibility.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NodeConfig {
    /// Ogolne ustawienia serwera (limity polaczen, watki)
    pub server: ServerConfig,

    /// Konfiguracja protokolow wejsciowych (OpenAI API, gRPC, QUIC)
    pub protocols: ProtocolsConfig,

    /// Middleware (request/response validation, rate limiting)
    pub middleware: MiddlewareConfig,

    /// Rate limiting (limity per-second, burst)
    #[serde(default)]
    pub rate_limiting: RateLimitingConfig,

    /// Load balancing (health checks, circuit breaker)
    pub load_balancing: LoadBalancingConfig,

    /// Monitoring (Prometheus, health checks)
    #[serde(default)]
    pub monitoring: MonitoringConfig,

    /// Memory management (opcjonalne, dla przyszlych optymalizacji)
    #[serde(default)]
    pub memory: Option<MemoryConfig>,

    /// Security (CORS, IP whitelist, API keys)
    #[serde(default)]
    pub security: Option<SecurityConfig>,

    /// Rola wezla w mesh (router, desktop, mobile)
    #[serde(default)]
    pub node_role: NodeRole,

    /// Konfiguracja mesh networking (opcjonalna)
    #[serde(default)]
    pub mesh: Option<MeshConfig>,

    /// Konfiguracja lokalnej inferencji (opcjonalna)
    #[serde(default)]
    pub inference: Option<InferenceConfig>,

    /// Runtime services subsystem (port range, supervisor cadence, restart policy).
    /// Used by the unified services refactor (services_repo + services::deploy/supervisor).
    #[serde(default)]
    pub services_runtime: ServicesRuntimeConfig,
}

// =============================================================================
// Konfiguracja runtime serwisow (additive — wariant B refactor unifikacji)
// =============================================================================

/// Konfiguracja podsystemu runtime'u serwisow zarzadzanych przez `services_repo`
/// i `services::deploy/supervisor`. Sekcja TOML: `[services_runtime]`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServicesRuntimeConfig {
    /// Inclusive zakres portow ktore allocator moze rozdawac dla deploymentow.
    #[serde(default = "default_services_port_range")]
    pub port_range: (u16, u16),

    /// Interwal probek health-check w milisekundach.
    #[serde(default = "default_services_health_interval_ms")]
    pub health_check_interval_ms: u64,

    /// Maksymalna liczba prob restartow zanim supervisor oznaczy serwis jako Failed.
    #[serde(default = "default_services_max_restart_attempts")]
    pub max_restart_attempts: u32,

    /// Gorny limit (cap) dla exponential backoff miedzy restartami, w milisekundach.
    #[serde(default = "default_services_restart_backoff_max_ms")]
    pub restart_backoff_max_ms: u64,
}

impl Default for ServicesRuntimeConfig {
    fn default() -> Self {
        Self {
            port_range: default_services_port_range(),
            health_check_interval_ms: default_services_health_interval_ms(),
            max_restart_attempts: default_services_max_restart_attempts(),
            restart_backoff_max_ms: default_services_restart_backoff_max_ms(),
        }
    }
}

fn default_services_port_range() -> (u16, u16) {
    (5000, 6000)
}

fn default_services_health_interval_ms() -> u64 {
    2_000
}

fn default_services_max_restart_attempts() -> u32 {
    5
}

fn default_services_restart_backoff_max_ms() -> u64 {
    60_000
}

// =============================================================================
// Rola wezla w mesh
// =============================================================================

/// Rola wezla w sieci mesh
#[derive(Debug, Clone, Deserialize, Serialize, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NodeRole {
    /// Centralny router — przyjmuje requesty, deleguje do backendow
    #[default]
    Router,
    /// Stacja robocza z lokalnym GPU — moze uruchamiac inferencje
    Desktop,
    /// Urzadzenie mobilne — lekki klient mesh
    Mobile,
}

// =============================================================================
// Konfiguracja mesh networking
// =============================================================================

/// Konfiguracja sieci mesh miedzy wezlami TentaFlow
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MeshConfig {
    /// Wlacz mesh networking
    #[serde(default)]
    pub enabled: bool,

    /// Port QUIC dla komunikacji mesh (domyslnie 8090)
    #[serde(default = "default_mesh_port")]
    pub port: u16,

    /// Statyczni peerzy (adresy QUIC do polaczenia)
    #[serde(default)]
    pub static_peers: Vec<String>,

    /// Wlacz mDNS discovery
    #[serde(default = "default_true")]
    pub mdns_enabled: bool,

    /// Interwal heartbeat QUIC w milisekundach
    #[serde(default = "default_heartbeat_interval_ms")]
    pub heartbeat_interval_ms: u64,

    /// Timeout po ktorym peer jest uznawany za dead (ms)
    #[serde(default = "default_peer_timeout_ms")]
    pub peer_timeout_ms: u64,

    /// Nazwa klastra (tylko peery z ta sama nazwa sie lacza)
    #[serde(default = "default_cluster_name")]
    pub cluster_name: String,

    /// URL serwera relay iroh uzywanego gdy bezposrednie QUIC hole punching
    /// nie jest mozliwe (NAT, firewall). Pusty string (domyslnie) oznacza
    /// uzycie wbudowanego presetu N0 iroh (4 produkcyjne regiony
    /// `*.relay.n0.iroh-canary.iroh.link`). Niepusta wartosc zastepuje preset
    /// podanym URL; override mozna tez zrobic wpisem
    /// `settings.mesh.iroh_relay_url` w DB.
    #[serde(default = "default_iroh_relay_url")]
    pub iroh_relay_url: String,
}

/// Domyslnie pusty string — iroh uzyje wbudowanego presetu N0.
fn default_iroh_relay_url() -> String {
    String::new()
}

// =============================================================================
// Konfiguracja lokalnej inferencji
// =============================================================================

/// Konfiguracja lokalnej inferencji LLM na wezle
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct InferenceConfig {
    /// Wlacz lokalna inferencje
    #[serde(default)]
    pub enabled: bool,

    /// Sciezka do katalogu z modelami
    #[serde(default = "default_models_dir")]
    pub models_dir: String,

    /// Modele do zaladowania przy starcie
    #[serde(default)]
    pub autoload_models: Vec<String>,

    /// Maksymalna ilosc GPU layers do offload
    #[serde(default)]
    pub gpu_layers: Option<u32>,

    /// Preferowany backend: "llamacpp" lub "mlx"
    #[serde(default = "default_inference_backend")]
    pub backend: String,
}

// =============================================================================
// Konfiguracja serwera
// =============================================================================

/// Konfiguracja ogolna serwera
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    /// Maksymalna liczba jednoczesnych polaczen TCP
    pub max_total_connections: usize,

    /// Maksymalna liczba rownoleglych requestow (active + queued)
    pub max_concurrent_requests: usize,

    /// Maksymalna liczba requestow w kolejce (oczekujacych na backend)
    pub max_queued_requests: usize,

    /// Liczba watkow w thread pool (0 = auto = num_cpus)
    #[serde(default)]
    pub worker_threads: usize,

    /// Czy przypinac watki do rdzeni CPU (NUMA-aware)
    #[serde(default = "default_true")]
    pub cpu_affinity: bool,

    /// Level logowania (trace, debug, info, warn, error)
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// Format logow (json lub pretty)
    #[serde(default = "default_log_format")]
    pub log_format: String,
}

// =============================================================================
// Konfiguracja protokolow
// =============================================================================

/// Konfiguracja wszystkich protokolow
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProtocolsConfig {
    /// OpenAI API (REST + SSE)
    pub openai_api: ProtocolConfig,

    /// gRPC (NVIDIA NIM compatible) - opcjonalne w Fazie 0
    #[serde(default)]
    pub grpc: Option<ProtocolConfig>,

    /// QUIC + rkyv - opcjonalne w Fazie 0
    #[serde(default)]
    pub quic: Option<QuicProtocolConfig>,
}

/// Konfiguracja pojedynczego protokolu (OpenAI API lub gRPC)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProtocolConfig {
    /// Czy protokol jest wlaczony
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Adres i port nasluchiwania (np. "0.0.0.0:8080")
    pub bind: String,

    /// Sciezka do certyfikatu TLS (opcjonalne - dla testow lokalnych mozna pominac)
    #[serde(default)]
    pub tls_cert: Option<String>,

    /// Sciezka do klucza TLS (opcjonalne - dla testow lokalnych mozna pominac)
    #[serde(default)]
    pub tls_key: Option<String>,

    /// Maksymalna liczba polaczen dla tego protokolu
    pub max_connections: usize,

    /// Timeout na request (milisekundy)
    pub request_timeout_ms: u64,

    /// Max rozmiar body (bajty) - dla OpenAI API
    #[serde(default = "default_body_limit")]
    pub body_limit_bytes: usize,

    /// Opcjonalny CA dla mTLS (client authentication)
    #[serde(default)]
    pub mtls_client_ca: Option<String>,
}

/// Konfiguracja protokolu QUIC (rozszerzenie ProtocolConfig)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct QuicProtocolConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    pub bind: String,
    #[serde(default)]
    pub tls_cert: Option<String>,
    #[serde(default)]
    pub tls_key: Option<String>,
    pub max_connections: usize,

    /// Max liczba streamow per connection (QUIC multiplexing)
    #[serde(default = "default_quic_streams")]
    pub max_streams_per_connection: usize,

    /// Idle timeout dla polaczenia QUIC (ms)
    #[serde(default = "default_quic_idle_timeout")]
    pub idle_timeout_ms: u64,
}

// =============================================================================
// Middleware i rate limiting
// =============================================================================

/// Konfiguracja middleware
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MiddlewareConfig {
    /// Czy request middleware jest wlaczony (Faza 0: false = noop)
    #[serde(default)]
    pub request_validation_enabled: bool,

    /// Czy response middleware jest wlaczony (Faza 0: false = noop)
    #[serde(default)]
    pub response_filtering_enabled: bool,

    /// Czy rate limiting jest wlaczony
    #[serde(default = "default_true")]
    pub rate_limiting_enabled: bool,

    /// Czy audit logging jest wlaczony
    #[serde(default = "default_true")]
    pub audit_logging_enabled: bool,
}

/// Konfiguracja rate limiting
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RateLimitingConfig {
    /// Domyslny limit requestow per sekunda (per API key)
    #[serde(default = "default_rate_limit_rps")]
    pub default_requests_per_second: u32,

    /// Burst capacity (token bucket)
    #[serde(default = "default_rate_limit_burst")]
    pub default_burst: u32,
}

// =============================================================================
// Load balancing
// =============================================================================

/// Konfiguracja load balancingu
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LoadBalancingConfig {
    /// Interwal health checkow (ms)
    #[serde(default = "default_health_check_interval")]
    pub health_check_interval_ms: u64,

    /// Timeout na health check (ms)
    #[serde(default = "default_health_check_timeout")]
    pub health_check_timeout_ms: u64,

    /// Ile failed health checks zanim backend zostanie oznaczony jako unhealthy
    #[serde(default = "default_unhealthy_threshold")]
    pub unhealthy_threshold: u32,

    /// Ile successful health checks zanim backend zostanie oznaczony jako healthy
    #[serde(default = "default_healthy_threshold")]
    pub healthy_threshold: u32,

    /// Max czas oczekiwania w kolejce (ms)
    #[serde(default = "default_queue_timeout")]
    pub queue_timeout_ms: u64,

    /// Czy circuit breaker jest wlaczony
    #[serde(default = "default_true")]
    pub circuit_breaker_enabled: bool,

    /// Prog bledow dla circuit breaker (ile bledow -> OPEN)
    #[serde(default = "default_circuit_breaker_threshold")]
    pub circuit_breaker_threshold: u32,

    /// Czas w stanie OPEN przed przejsciem do HALF_OPEN (ms)
    #[serde(default = "default_circuit_breaker_timeout")]
    pub circuit_breaker_timeout_ms: u64,
}

// =============================================================================
// Runtime types reused przez warstwe transport_client / backend client
// =============================================================================

/// Typ polaczenia do backendu AI
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ConnectionType {
    /// OpenAI API compatible (HTTP/HTTPS REST API)
    #[serde(rename = "openai_api")]
    OpenAIApi {
        /// URL backendu (np. https://api.openai.com/v1)
        url: String,

        /// API key bezposrednio (opcjonalny, ma priorytet nad api_key_env)
        #[serde(default)]
        api_key: Option<String>,

        /// Zmienna srodowiskowa z API key
        #[serde(default)]
        api_key_env: Option<String>,

        /// Custom HTTP headers (dla specjalnych API jak Anthropic)
        #[serde(default)]
        extra_headers: Vec<(String, String)>,

        /// Custom endpoint path (np. "/infer" dla PaddleOCR, "/audio/speech" dla TTS)
        #[serde(default)]
        custom_endpoint: Option<String>,

        /// Request format transformation ("openai", "paddleocr", etc.)
        #[serde(default)]
        request_format: Option<String>,

        /// Dodatkowe parametry dla TTS (voice, model, speed, format)
        #[serde(default)]
        tts_config: Option<TTSParameters>,
    },

    /// QUIC connection (dla TentaFlow.Embeddings, TentaFlow.TTS)
    QUIC {
        /// QUIC URL (quic://host:port)
        quic_url: String,

        /// CA cert dla weryfikacji serwera (opcjonalne - jesli None, uzywa systemowych CA)
        #[serde(default)]
        tls_ca: Option<String>,

        /// Auto-reconnect po utracie polaczenia
        #[serde(default = "default_true")]
        auto_reconnect: bool,

        /// Interwal reconnect (ms)
        #[serde(default = "default_reconnect_interval")]
        reconnect_interval_ms: u64,

        /// Keepalive interval (ms)
        #[serde(default = "default_keepalive")]
        keepalive_interval_ms: u64,

        /// Dodatkowe parametry dla TTS (voice, speed) - opcjonalne
        #[serde(default)]
        tts_config: Option<TTSParameters>,
    },
}

/// Parametry specyficzne dla TTS (uzywane w ConnectionType::OpenAIApi)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TTSParameters {
    /// Model TTS: "tts-1" (szybki) lub "tts-1-hd" (wysoka jakosc)
    #[serde(default = "default_tts_model")]
    pub model: String,

    /// Glos: "alloy", "echo", "fable", "onyx", "nova", "shimmer"
    #[serde(default = "default_tts_voice")]
    pub voice: String,

    /// Format audio: "opus", "mp3", "aac", "flac", "wav", "pcm"
    #[serde(default = "default_tts_format")]
    pub response_format: String,

    /// Predkosc mowy (0.25-4.0)
    #[serde(default = "default_tts_speed")]
    pub speed: f32,
}

/// Pojedynczy backend w ramach serwisu (dla load balancing)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServiceBackend {
    /// Typ i parametry polaczenia
    #[serde(flatten)]
    pub connection: ConnectionType,

    /// Max rownoczesnych requestow dla tego backendu
    pub max_concurrent: usize,

    /// Timeout dla requestow do tego backendu (ms)
    pub timeout_ms: u64,

    /// Waga dla weighted load balancing
    #[serde(default = "default_weight")]
    pub weight: u32,

    /// Override nazwy modelu dla tego backendu (dla LLM)
    #[serde(default)]
    pub model_name_override: Option<String>,

    /// Custom health check path (opcjonalny)
    #[serde(default)]
    pub health_check_path: Option<String>,
}

/// Kategoria modelu LLM (dla KV Cache / Prefix Caching)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LlmModelCategory {
    /// Glowny LLM (bielik-11b) — odpowiedzi uzytkownikowi
    #[default]
    Main,
    /// Analyzer LLM (bielik-1.5b) — analiza dla Memory, tools
    Analyzer,
}

// =============================================================================
// Monitoring
// =============================================================================

/// Konfiguracja monitoringu
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MonitoringConfig {
    /// Czy Prometheus metrics sa wlaczone
    #[serde(default = "default_true")]
    pub prometheus_enabled: bool,

    /// Adres dla Prometheus endpoint
    #[serde(default = "default_prometheus_bind")]
    pub prometheus_bind: String,

    /// Czy health check endpoint jest wlaczony
    #[serde(default = "default_true")]
    pub health_check_enabled: bool,

    /// Adres dla health check endpoint
    #[serde(default = "default_health_bind")]
    pub health_check_bind: String,

    /// Sciezka dla health check
    #[serde(default = "default_health_path")]
    pub health_check_path: String,

    /// Czy OpenTelemetry tracing jest wlaczony
    #[serde(default)]
    pub tracing_enabled: bool,

    /// Endpoint dla tracingu (opcjonalny)
    #[serde(default)]
    pub tracing_endpoint: Option<String>,
}

// =============================================================================
// Memory management i security
// =============================================================================

/// Konfiguracja memory management (opcjonalne, dla Fazy 3)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MemoryConfig {
    pub total_ram_percentage: u8,
    pub connection_pool_percentage: u8,
    pub request_buffers_percentage: u8,
    pub response_cache_percentage: u8,
    pub other_percentage: u8,
    pub max_request_buffer_kb: usize,
    pub max_response_buffer_kb: usize,
}

/// Konfiguracja security (opcjonalne)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SecurityConfig {
    /// CORS enabled
    #[serde(default = "default_true")]
    pub cors_enabled: bool,

    /// CORS allowed origins
    #[serde(default)]
    pub cors_allowed_origins: Vec<String>,

    /// CORS allowed methods
    #[serde(default)]
    pub cors_allowed_methods: Vec<String>,

    /// CORS allowed headers
    #[serde(default)]
    pub cors_allowed_headers: Vec<String>,
}

// =============================================================================
// Wartosci domyslne — funkcje dla serde
// =============================================================================

fn default_true() -> bool {
    true
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_log_format() -> String {
    "json".to_string()
}

fn default_body_limit() -> usize {
    1_048_576 // 1 MB
}

fn default_quic_streams() -> usize {
    100
}

fn default_quic_idle_timeout() -> u64 {
    30_000 // 30 sekund
}

fn default_rate_limit_rps() -> u32 {
    100
}

fn default_rate_limit_burst() -> u32 {
    200
}

fn default_health_check_interval() -> u64 {
    5_000 // 5 sekund
}

fn default_health_check_timeout() -> u64 {
    2_000 // 2 sekundy
}

fn default_unhealthy_threshold() -> u32 {
    3
}

fn default_healthy_threshold() -> u32 {
    2
}

fn default_queue_timeout() -> u64 {
    30_000 // 30 sekund
}

fn default_circuit_breaker_threshold() -> u32 {
    5
}

fn default_circuit_breaker_timeout() -> u64 {
    60_000 // 60 sekund
}

fn default_weight() -> u32 {
    1
}

fn default_prometheus_bind() -> String {
    "0.0.0.0:9090".to_string()
}

fn default_health_bind() -> String {
    "0.0.0.0:8888".to_string()
}

fn default_health_path() -> String {
    "/health".to_string()
}

fn default_reconnect_interval() -> u64 {
    5_000 // 5 sekund
}

fn default_keepalive() -> u64 {
    10_000 // 10 sekund
}

fn default_tts_model() -> String {
    "tts-1".to_string()
}

fn default_tts_voice() -> String {
    "alloy".to_string()
}

fn default_tts_format() -> String {
    "opus".to_string()
}

fn default_tts_speed() -> f32 {
    1.0
}

fn default_mesh_port() -> u16 {
    8090
}

fn default_heartbeat_interval_ms() -> u64 {
    500
}

fn default_peer_timeout_ms() -> u64 {
    3000
}

fn default_cluster_name() -> String {
    "tentaflow".to_string()
}

fn default_models_dir() -> String {
    // Portable layout: shared models cache under <tentaflow_home>/models so
    // every backend (Docker, native venv, in-process) hits the same HF cache.
    crate::paths::models_root().to_string_lossy().into_owned()
}

fn default_inference_backend() -> String {
    "llamacpp".to_string()
}

// =============================================================================
// Implementacja — metody NodeConfig
// =============================================================================

impl NodeConfig {
    /// Wczytuje konfiguracje z pliku TOML.
    ///
    /// Algorytm:
    /// 1. Wczytaj plik jako String
    /// 2. Parsuj TOML -> NodeConfig
    /// 3. Zwaliduj wszystkie wartosci
    /// 4. Zwroc zwalidowana konfiguracje
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path).map_err(|e| CoreError::ConfigError {
            message: format!("Nie mozna wczytac pliku konfiguracji: {:?}", path),
            source: e.into(),
        })?;

        let config: NodeConfig = toml::from_str(&content).map_err(|e| CoreError::ConfigError {
            message: "Blad parsowania TOML".to_string(),
            source: e.into(),
        })?;

        config.validate()?;
        Ok(config)
    }

    /// Waliduje poprawnosc wszystkich wartosci w konfiguracji.
    fn validate(&self) -> Result<()> {
        if self.server.max_total_connections == 0 {
            return Err(CoreError::ConfigError {
                message: "max_total_connections musi byc > 0".to_string(),
                source: anyhow::anyhow!("Niepoprawna wartosc: 0"),
            }
            .into());
        }

        if self.protocols.openai_api.enabled {
            self.validate_protocol_config(&self.protocols.openai_api, "openai_api")?;
        }

        // Walidacja mesh config jesli obecna
        if let Some(ref mesh) = self.mesh {
            if mesh.enabled && mesh.port == 0 {
                return Err(CoreError::ConfigError {
                    message: "mesh.port musi byc > 0 gdy mesh jest wlaczony".to_string(),
                    source: anyhow::anyhow!("Niepoprawna wartosc portu: 0"),
                }
                .into());
            }
        }

        Ok(())
    }

    /// Waliduje konfiguracje pojedynczego protokolu
    fn validate_protocol_config(&self, config: &ProtocolConfig, protocol_name: &str) -> Result<()> {
        if !config.bind.contains(':') {
            return Err(CoreError::ConfigError {
                message: format!(
                    "Niepoprawny bind address dla {}: '{}'",
                    protocol_name, config.bind
                ),
                source: anyhow::anyhow!("Oczekiwano formatu 'host:port'"),
            }
            .into());
        }

        Ok(())
    }

    /// Serializuje konfiguracje do formatu TOML.
    pub fn to_toml_string(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|e| {
            CoreError::ConfigError {
                message: "Blad serializacji konfiguracji do TOML".to_string(),
                source: e.into(),
            }
            .into()
        })
    }
}

// =============================================================================
// Implementacje Default
// =============================================================================

impl Default for MiddlewareConfig {
    fn default() -> Self {
        Self {
            request_validation_enabled: false,
            response_filtering_enabled: false,
            rate_limiting_enabled: true,
            audit_logging_enabled: true,
        }
    }
}

impl Default for RateLimitingConfig {
    fn default() -> Self {
        Self {
            default_requests_per_second: default_rate_limit_rps(),
            default_burst: default_rate_limit_burst(),
        }
    }
}

impl Default for LoadBalancingConfig {
    fn default() -> Self {
        Self {
            health_check_interval_ms: default_health_check_interval(),
            health_check_timeout_ms: default_health_check_timeout(),
            unhealthy_threshold: default_unhealthy_threshold(),
            healthy_threshold: default_healthy_threshold(),
            queue_timeout_ms: default_queue_timeout(),
            circuit_breaker_enabled: true,
            circuit_breaker_threshold: default_circuit_breaker_threshold(),
            circuit_breaker_timeout_ms: default_circuit_breaker_timeout(),
        }
    }
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig {
                max_total_connections: 1000,
                max_concurrent_requests: 100,
                max_queued_requests: 50,
                worker_threads: 0,
                cpu_affinity: true,
                log_level: "info".to_string(),
                log_format: "json".to_string(),
            },
            protocols: ProtocolsConfig {
                openai_api: ProtocolConfig {
                    enabled: true,
                    bind: "0.0.0.0:8090".to_string(),
                    tls_cert: None,
                    tls_key: None,
                    max_connections: 500,
                    request_timeout_ms: 120_000,
                    body_limit_bytes: 1_048_576,
                    mtls_client_ca: None,
                },
                grpc: None,
                quic: Some(QuicProtocolConfig {
                    enabled: true,
                    bind: "0.0.0.0:8090".to_string(),
                    tls_cert: None,
                    tls_key: None,
                    max_connections: 100,
                    max_streams_per_connection: 100,
                    idle_timeout_ms: 30_000,
                }),
            },
            middleware: MiddlewareConfig::default(),
            rate_limiting: RateLimitingConfig::default(),
            load_balancing: LoadBalancingConfig::default(),
            monitoring: MonitoringConfig::default(),
            memory: None,
            security: None,
            node_role: NodeRole::default(),
            mesh: Some(MeshConfig {
                enabled: true,
                port: 8090,
                static_peers: vec![],
                mdns_enabled: true,
                heartbeat_interval_ms: default_heartbeat_interval_ms(),
                peer_timeout_ms: default_peer_timeout_ms(),
                cluster_name: "tentaflow".to_string(),
                iroh_relay_url: default_iroh_relay_url(),
            }),
            inference: None,
            services_runtime: ServicesRuntimeConfig::default(),
        }
    }
}

impl Default for MonitoringConfig {
    fn default() -> Self {
        Self {
            prometheus_enabled: true,
            prometheus_bind: default_prometheus_bind(),
            health_check_enabled: true,
            health_check_bind: default_health_bind(),
            health_check_path: default_health_path(),
            tracing_enabled: false,
            tracing_endpoint: None,
        }
    }
}
