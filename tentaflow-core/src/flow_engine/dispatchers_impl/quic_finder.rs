// =============================================================================
// Plik: flow_engine/dispatchers_impl/quic_finder.rs
// Opis: Minimalny finder klienta QUIC po nazwie modelu/serwisu. Pozwala
//       wrapperom (memory_impl, w przyszłości llm/embeddings/tts/stt jeśli
//       wybiorą bezpośredni QUIC zamiast ModelRuntimeExecutor) trzymać tylko
//       ten wąski trait zamiast całego ServiceManagera.
// =============================================================================

use async_trait::async_trait;
use std::sync::Arc;

use crate::net::quic::QuicClient;
use crate::services::runtime::quic_handle::ServiceManager;

#[async_trait]
pub trait QuicClientFinder: Send + Sync {
    /// Zwraca pierwszy żywy klient QUIC dla danego modelu/serwisu albo
    /// `None` gdy registry nie ma takiego serwisu lub QUIC nie jest jeszcze
    /// zestawiony.
    async fn find(&self, model: &str) -> Option<Arc<QuicClient>>;
}

/// Default impl — deleguje do `ServiceManager::find_quic_client_for_model`.
/// Bootstrap (`Router::new`) tworzy `Arc::new(ServiceManagerQuicFinder { sm })`
/// raz i wstrzykuje jako `Arc<dyn QuicClientFinder>` do każdego wrappera,
/// który potrzebuje QUIC. Wrapper nie widzi `Arc<ServiceManager>` (D4
/// invariant).
pub struct ServiceManagerQuicFinder {
    sm: Arc<ServiceManager>,
}

impl ServiceManagerQuicFinder {
    pub fn new(sm: Arc<ServiceManager>) -> Self {
        Self { sm }
    }
}

#[async_trait]
impl QuicClientFinder for ServiceManagerQuicFinder {
    async fn find(&self, model: &str) -> Option<Arc<QuicClient>> {
        self.sm.find_quic_client_for_model(model).await
    }
}
