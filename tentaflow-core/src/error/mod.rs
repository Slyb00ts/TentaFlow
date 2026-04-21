// =============================================================================
// Plik: error/mod.rs
// Opis: Hierarchia bledow dla TentaFlow.Core — wspolna dla Router, Desktop
//       i Mobile. Obejmuje bledy routera, sieci, gossip protocol, CRDT, peer
//       discovery oraz lokalnej inferencji.
// =============================================================================

/// Glowny typ bledu dla calego systemu TentaFlow.
///
/// Kazdy wariant reprezentuje kategorie bledow z konkretnego modulu lub operacji.
/// Wiekszosc wariantow zawiera `source: anyhow::Error` dla zachowania pelnego
/// lancucha bledow (error chain).
#[derive(thiserror::Error, Debug)]
pub enum CoreError {
    /// Blad parsowania lub walidacji konfiguracji (config.toml)
    #[error("Blad konfiguracji: {message}")]
    ConfigError {
        message: String,
        #[source]
        source: anyhow::Error,
    },

    /// Blad polaczenia sieciowego (timeout, connection refused, DNS)
    #[error("Blad sieciowy: {message}")]
    NetworkError {
        message: String,
        #[source]
        source: anyhow::Error,
    },

    /// Blad komunikacji z backendem LLM (HTTP 5xx, niepoprawna odpowiedz)
    #[error("Blad backendu '{backend_url}': {message}")]
    BackendError {
        backend_url: String,
        message: String,
        #[source]
        source: Option<anyhow::Error>,
    },

    /// Blad parsowania lub walidacji requestu od klienta (niepoprawny JSON, brak wymaganych pol)
    #[error("Niepoprawny request: {message}")]
    InvalidRequest {
        message: String,
        /// Szczegoly bledu (np. konkretne pole ktore jest niepoprawne)
        details: Option<String>,
    },

    /// Model nie zostal znaleziony w konfiguracji (brak model pool dla danej nazwy)
    #[error("Model '{model_name}' nie zostal znaleziony w konfiguracji")]
    ModelNotFound { model_name: String },

    /// Wszystkie backendy dla danego modelu sa niedostepne (circuit breaker OPEN lub health check failed)
    #[error("Wszystkie backendy dla modelu '{model_name}' sa niedostepne")]
    AllBackendsUnavailable { model_name: String },

    /// Przekroczono limit requestow (rate limiting)
    #[error("Przekroczono limit requestow: {message}")]
    RateLimitExceeded { message: String },

    /// Kolejka requestow jest pelna (backpressure)
    #[error("Kolejka requestow jest pelna dla modelu '{model_name}'")]
    QueueFull { model_name: String },

    /// Timeout podczas oczekiwania na odpowiedz z backendu
    #[error("Timeout podczas komunikacji z backendem '{backend_url}': {timeout_ms}ms")]
    Timeout {
        backend_url: String,
        timeout_ms: u64,
    },

    /// Blad middleware (request lub response middleware zablokowal request)
    #[error("Request zablokowany przez middleware: {reason}")]
    MiddlewareBlocked { reason: String },

    /// Blad wewnetrzny (bug — nie powinien sie zdarzyc w normalnej operacji)
    #[error("Wewnetrzny blad: {message}")]
    InternalError {
        message: String,
        #[source]
        source: Option<anyhow::Error>,
    },

    /// Blad zwiazany z TLS/SSL (certyfikat niepoprawny, problem z handshake)
    #[error("Blad TLS: {message}")]
    TlsError {
        message: String,
        #[source]
        source: anyhow::Error,
    },

    // =========================================================================
    // Bledy gossip protocol
    // =========================================================================
    /// Blad propagacji wiadomosci gossip (serializacja, dostarczenie, duplikat)
    #[error("Blad gossip protocol: {message}")]
    GossipError {
        message: String,
        #[source]
        source: Option<anyhow::Error>,
    },

    // =========================================================================
    // Bledy CRDT
    // =========================================================================
    /// Blad synchronizacji CRDT (merge conflict, niepoprawny stan, niezgodnosc wersji)
    #[error("Blad CRDT sync: {message}")]
    CrdtError {
        message: String,
        #[source]
        source: Option<anyhow::Error>,
    },

    // =========================================================================
    // Bledy peer discovery/connection
    // =========================================================================
    /// Blad odkrywania lub polaczenia z peerem (mDNS, QUIC handshake, autoryzacja)
    #[error("Blad peer '{peer_id}': {message}")]
    PeerError {
        peer_id: String,
        message: String,
        #[source]
        source: Option<anyhow::Error>,
    },

    // =========================================================================
    // Bledy lokalnej inferencji
    // =========================================================================
    /// Blad lokalnej inferencji (ladowanie modelu, generowanie, brak pamieci GPU)
    #[error("Blad inferencji '{model_path}': {message}")]
    InferenceError {
        model_path: String,
        message: String,
        #[source]
        source: Option<anyhow::Error>,
    },
}

impl CoreError {
    /// Mapuje CoreError na HTTP status code zgodnie z semantyka bledu.
    ///
    /// Uzywane przez HTTP server do zwracania odpowiednich kodow bledow klientom.
    pub fn status_code(&self) -> u16 {
        match self {
            // 400 Bad Request — blad po stronie klienta
            CoreError::InvalidRequest { .. } => 400,

            // 404 Not Found — model nie istnieje
            CoreError::ModelNotFound { .. } => 404,

            // 429 Too Many Requests — rate limiting
            CoreError::RateLimitExceeded { .. } => 429,

            // 503 Service Unavailable — backendy niedostepne lub kolejka pelna
            CoreError::AllBackendsUnavailable { .. } => 503,
            CoreError::QueueFull { .. } => 503,

            // 504 Gateway Timeout
            CoreError::Timeout { .. } => 504,

            // 500 Internal Server Error — wszystkie inne bledy
            _ => 500,
        }
    }

    /// Sprawdza czy blad jest mozliwy do retry (transient error).
    ///
    /// Uzywane przez retry logic w load balancerze — jesli blad jest transient,
    /// warto sprobowac ponownie na innym backendzie.
    pub fn is_retriable(&self) -> bool {
        matches!(
            self,
            CoreError::NetworkError { .. }
                | CoreError::BackendError { .. }
                | CoreError::Timeout { .. }
                | CoreError::GossipError { .. }
                | CoreError::PeerError { .. }
        )
    }
}

/// Typ Result uzywany w calym projekcie.
pub type Result<T> = anyhow::Result<T>;
