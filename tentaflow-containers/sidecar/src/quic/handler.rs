// =============================================================================
// Plik: quic/handler.rs
// Opis: Trait Handler — interfejs ktory rola (ReverseProxy, Onnx, TeamsBot)
//       implementuje zeby obsluzyc ModelRequest. QuicServer wola handle_request
//       po kazdym otrzymanym request na bidirectional stream.
// =============================================================================

use async_trait::async_trait;
use tentaflow_protocol::{ModelRequest, ModelResponse, ModelStreamChunk};
use tokio::sync::mpsc;

/// Blad obslugi requesta. Mapowany na ModelResponse z error message.
#[derive(Debug, thiserror::Error)]
pub enum HandlerError {
    #[error("Upstream timeout")]
    Timeout,
    #[error("Upstream niedostepny: {0}")]
    UpstreamUnavailable(String),
    #[error("Niewspierane zapytanie: {0}")]
    UnsupportedRequest(String),
    #[error("Blad wewnetrzny: {0}")]
    Internal(String),
}

/// Wynik handlowania requesta — albo pojedyncza odpowiedz, albo strumien chunkow.
pub enum HandleOutcome {
    /// Jednorazowa odpowiedz — serwer wysle ja i zamknie stream.
    Unary(ModelResponse),
    /// Strumien — serwer wysyla kazdy chunk jako length-prefixed rkyv,
    /// kanal `rx` jest konsumowany az do zamkniecia.
    Stream(mpsc::Receiver<ModelStreamChunk>),
}

#[async_trait]
pub trait Handler: Send + Sync + 'static {
    /// Glowna metoda dispatch. Wolana raz per request z routera.
    async fn handle(&self, request: ModelRequest) -> Result<HandleOutcome, HandlerError>;

    /// Opcjonalna informacja do ServiceAnnounce (nazwa serwisu + model aliasy).
    /// Domyslnie sidecar bierze z configu — handler moze nadpisac.
    fn advertise_models(&self) -> Vec<String> {
        Vec::new()
    }
}
