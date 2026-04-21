// =============================================================================
// Plik: tentaflow-transport/src/server.rs
// Opis: Serwer-helper — `serve_model_requests` uruchamia akceptor bidi streamow
//       i dla kazdego requestu wola `ModelHandler::handle`. Uzywany przez
//       sidecar oraz tentaflow-core (endpoint przyjmujacy klientow .NET/WASM).
// =============================================================================

use std::sync::Arc;

use async_trait::async_trait;
use iroh::endpoint::Connection;
use iroh::Endpoint;
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use tentaflow_protocol::{ErrorInfo, ErrorType, ModelRequest, ModelResponse, ModelResult, ModelStreamChunk};

use crate::error::TransportError;
use crate::framing::{read_frame, write_frame};
use crate::ALPN_SERVICE;

/// Blad obslugi pojedynczego requestu.
#[derive(Debug, thiserror::Error)]
pub enum HandleError {
    #[error("handler: upstream timeout")]
    Timeout,
    #[error("handler: upstream unavailable: {0}")]
    UpstreamUnavailable(String),
    #[error("handler: niewspierany request: {0}")]
    UnsupportedRequest(String),
    #[error("handler: blad wewnetrzny: {0}")]
    Internal(String),
}

/// Wynik obslugi requestu. Unary - jeden `ModelResponse`. Stream - strumien
/// `ModelStreamChunk` (np. tokeny LLM, audio STT, itd.).
pub enum ModelOutcome {
    Unary(ModelResponse),
    Stream(mpsc::Receiver<ModelStreamChunk>),
}

/// Trait ktory kazdy konsument (sidecar role, tentaflow node routing) implementuje
/// zeby dispatchowac `ModelRequest` na backend.
#[async_trait]
pub trait ModelHandler: Send + Sync + 'static {
    async fn handle(&self, request: ModelRequest) -> Result<ModelOutcome, HandleError>;
}

/// Uruchamia akceptor iroh na istniejacym endpoincie. Kazda nowa koneksja ze
/// ALPN-em `tentaflow-service/v1` spawnuje taska obslugujacego. Funkcja konczy
/// sie gdy `shutdown_rx` zmieni wartosc na `true` albo endpoint sie zamknie.
pub async fn serve_model_requests<H: ModelHandler>(
    endpoint: Endpoint,
    handler: Arc<H>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<(), TransportError> {
    info!(endpoint_id = %endpoint.id().fmt_short(), "iroh service server akceptuje polaczenia");

    loop {
        tokio::select! {
            biased;

            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    info!("iroh service server: shutdown");
                    endpoint.close().await;
                    return Ok(());
                }
            }

            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else {
                    debug!("endpoint.accept zwrocil None");
                    return Ok(());
                };

                let handler = Arc::clone(&handler);
                let shutdown_rx = shutdown_rx.clone();

                tokio::spawn(async move {
                    let connection = match incoming.await {
                        Ok(c) => c,
                        Err(e) => {
                            warn!("iroh incoming handshake failed: {e:?}");
                            return;
                        }
                    };

                    let alpn: &[u8] = connection.alpn();
                    if alpn != ALPN_SERVICE {
                        debug!(alpn = ?alpn, "pomijam polaczenie z innym ALPN");
                        connection.close(0u32.into(), b"wrong_alpn");
                        return;
                    }

                    if let Err(e) = handle_connection(connection, handler, shutdown_rx).await {
                        debug!("connection handling ended: {e}");
                    }
                });
            }
        }
    }
}

async fn handle_connection<H: ModelHandler>(
    conn: Connection,
    handler: Arc<H>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<(), TransportError> {
    let remote = conn.remote_id();

    loop {
        tokio::select! {
            biased;

            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    conn.close(0u32.into(), b"shutdown");
                    return Ok(());
                }
            }

            reason = conn.closed() => {
                debug!(peer = ?remote, "connection closed: {reason:?}");
                return Ok(());
            }

            stream = conn.accept_bi() => {
                match stream {
                    Ok((send, recv)) => {
                        let handler = Arc::clone(&handler);
                        tokio::spawn(async move {
                            if let Err(e) = serve_stream(send, recv, handler).await {
                                debug!("stream serve error: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        debug!(peer = ?remote, "accept_bi error: {e}");
                        return Ok(());
                    }
                }
            }
        }
    }
}

async fn serve_stream<H: ModelHandler>(
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
    handler: Arc<H>,
) -> Result<(), TransportError> {
    let Some(request) = read_frame::<ModelRequest>(&mut recv).await? else {
        return Ok(());
    };

    let request_id = request.request_id.clone();
    debug!(request_id = %request_id, "odebrano ModelRequest");

    match handler.handle(request).await {
        Ok(ModelOutcome::Unary(resp)) => {
            write_frame(&mut send, &resp).await?;
            let _ = send.finish();
        }
        Ok(ModelOutcome::Stream(mut rx)) => {
            while let Some(chunk) = rx.recv().await {
                write_frame(&mut send, &chunk).await?;
            }
            let _ = send.finish();
        }
        Err(e) => {
            warn!(request_id = %request_id, "handler error: {e}");
            let err_resp = make_error_response(&request_id, &e.to_string());
            let _ = write_frame(&mut send, &err_resp).await;
            let _ = send.finish();
        }
    }

    Ok(())
}

fn make_error_response(request_id: &str, message: &str) -> ModelResponse {
    ModelResponse {
        request_id: request_id.to_string(),
        result: ModelResult::Error(ErrorInfo {
            error_type: ErrorType::InternalError,
            message: message.to_string(),
            details: None,
        }),
        metrics: None,
    }
}
