// =============================================================================
// Plik: error/mod.rs
// Opis: Hierarchia bledow dla TentaFlow.Core — wspolna dla Router, Desktop
//       i Mobile. Obejmuje bledy routera, sieci, gossip protocol, peer
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

    /// No running STT service row (embedded or HTTP) is registered on this node.
    #[error("No STT service is running — deploy and start an STT engine in Services")]
    SttServiceUnavailable,

    /// Przekroczono limit requestow (rate limiting)
    #[error("Przekroczono limit requestow: {message}")]
    RateLimitExceeded { message: String },

    /// Kolejka requestow jest pelna (backpressure)
    #[error("Kolejka requestow jest pelna dla modelu '{model_name}'")]
    QueueFull { model_name: String },

    /// An SLA policy refused the call: the service is at its concurrency or
    /// call limit. `reason` is the stable refusal code (`calls_limit`,
    /// `concurrency_queue_full`, `concurrency_queue_timeout`); the edge answers
    /// 429 and tells the caller when to come back.
    #[error("Service overloaded ({reason}), retry after {retry_after_ms} ms")]
    Overloaded { reason: String, retry_after_ms: u64 },

    /// A model service already serves `max_concurrent` requests and none ended
    /// within the service's `timeout_ms`. A capacity signal of the backend, not
    /// a fault of the caller, so it is a 503 with `Retry-After` (unlike
    /// `Overloaded`, which is a refusal under a policy of the caller's flow).
    ///
    /// `backend` names the service for logs. It is deliberately not in the
    /// message, which reaches clients: the text starts with the stable code
    /// `backend.busy`, like `sla.overloaded`, and carries no node or service id.
    #[error("backend.busy: retry after {retry_after_ms} ms")]
    BackendBusy {
        backend: String,
        retry_after_ms: u64,
    },

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
            CoreError::RateLimitExceeded { .. } | CoreError::Overloaded { .. } => 429,

            // 503 Service Unavailable — backendy niedostepne lub kolejka pelna
            CoreError::AllBackendsUnavailable { .. } => 503,
            CoreError::SttServiceUnavailable => 503,
            CoreError::QueueFull { .. } | CoreError::BackendBusy { .. } => 503,

            // 504 Gateway Timeout
            CoreError::Timeout { .. } => 504,

            // 500 Internal Server Error — wszystkie inne bledy
            _ => 500,
        }
    }

    /// Message safe to return to an API client. The `Display` form names
    /// backend addresses and model paths on disk; that detail belongs in the
    /// server log, not in a response body.
    pub fn client_message(&self) -> String {
        match self {
            CoreError::BackendError { .. } | CoreError::AllBackendsUnavailable { .. } => {
                "Backend unavailable. Please try again later.".to_string()
            }
            CoreError::Timeout { .. } => "Request timed out. Please try again later.".to_string(),
            CoreError::InferenceError { .. } => {
                "Model processing failed. Please try again later.".to_string()
            }
            CoreError::ModelNotFound { model_name } => format!("Model not found: {model_name}"),
            CoreError::QueueFull { .. }
            | CoreError::Overloaded { .. }
            | CoreError::BackendBusy { .. } => {
                "Service is busy. Please try again later.".to_string()
            }
            CoreError::InternalError { message, .. } => format!("Internal error: {message}"),
            _ => self.to_string(),
        }
    }

    /// Value of the `Retry-After` header for a refusal that carries a delay,
    /// in whole seconds rounded up (the header has no sub-second form) and never
    /// zero, so a client that obeys it always pauses.
    pub fn retry_after_secs(&self) -> Option<u64> {
        match self {
            CoreError::Overloaded { retry_after_ms, .. }
            | CoreError::BackendBusy { retry_after_ms, .. } => {
                Some(retry_after_ms.div_ceil(1000).max(1))
            }
            _ => None,
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

#[cfg(test)]
mod overloaded_tests {
    use super::CoreError;

    fn overloaded(retry_after_ms: u64) -> CoreError {
        CoreError::Overloaded {
            reason: "concurrency_queue_full".into(),
            retry_after_ms,
        }
    }

    #[test]
    fn overloaded_is_a_429_with_a_retry_after_in_whole_seconds_rounded_up() {
        assert_eq!(overloaded(1).status_code(), 429);
        assert_eq!(overloaded(1).retry_after_secs(), Some(1));
        assert_eq!(overloaded(1_000).retry_after_secs(), Some(1));
        assert_eq!(overloaded(1_001).retry_after_secs(), Some(2));
        assert_eq!(overloaded(0).retry_after_secs(), Some(1), "never zero");
        assert_eq!(CoreError::SttServiceUnavailable.retry_after_secs(), None);
    }

    #[test]
    fn a_busy_backend_is_a_503_with_a_retry_after() {
        let busy = CoreError::BackendBusy {
            backend: "service 3".into(),
            retry_after_ms: 1_500,
        };
        assert_eq!(busy.status_code(), 503);
        assert_eq!(busy.retry_after_secs(), Some(2));
        assert_eq!(
            busy.client_message(),
            "Service is busy. Please try again later."
        );
    }

    #[test]
    fn an_internal_error_is_worded_in_english_for_the_client() {
        let internal = CoreError::InternalError {
            message: "flow.failed: the flow did not complete".into(),
            source: None,
        };
        assert_eq!(
            internal.client_message(),
            "Internal error: flow.failed: the flow did not complete"
        );
    }
}
