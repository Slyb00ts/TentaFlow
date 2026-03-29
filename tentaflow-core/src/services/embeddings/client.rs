// =============================================================================
// Plik: services/embeddings/client.rs
// Opis: QUIC client dla komunikacji z Embeddings Engine — generowanie wektorow
//       z tekstu. TODO: zaimplementowac po utworzeniu zrodla w Routerze.
// =============================================================================

// TODO: Implementacja EmbeddingsClient (QUIC client dla embeddings engine).
//       Plik zrodlowy w Routerze (TentaFlow.Router/src/embeddings/client.rs)
//       jest zadeklarowany w mod.rs ale jeszcze nie istnieje.
//       Po jego implementacji, przeniesc tutaj z zamiana:
//       - crate::error::RouterError -> crate::error::CoreError
//       - crate::quic:: -> crate::net::quic::

/// Konfiguracja Embeddings engine
#[derive(Debug, Clone)]
pub struct EmbeddingsEngineConfigCompat {
    pub name: String,
    pub quic_url: String,
    pub tls_ca: Option<String>,
    pub max_concurrent: usize,
    pub timeout_ms: u64,
}

/// QUIC client dla komunikacji z Embeddings engine.
///
/// TODO: Zaimplementowac pelnego klienta QUIC analogicznie do RAGClient.
pub struct EmbeddingsClient;
