// =============================================================================
// Plik: services/rag/client.rs
// Opis: Klient iroh dla RAG Engine. Wewnetrznie owija `IrohServiceClient` —
//       caly ruch idzie przez wspolny iroh transport z ALPN
//       `tentaflow-service/v1`. Udostepnia `send_request(RAGPayload)`,
//       `send_ingest_request(IngestRequest)` oraz callback listener ktory
//       nasluchuje odwrotnych streamow od RAG engine (RAG → Router).
// =============================================================================

use crate::error::{CoreError, Result};
use crate::net::iroh_client::IrohServiceClient;
use tentaflow_protocol::*;
use tentaflow_transport::{read_frame, write_frame};

use std::sync::Arc;
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, warn};

/// Konfiguracja RAG engine — pole `quic_url` akceptuje teraz URL
/// `iroh://<hex>` albo czysty hex EndpointId. `tls_ca` zachowane dla compat,
/// ignorowane przez iroh.
#[derive(Debug, Clone)]
pub struct RAGEngineConfigCompat {
    pub name: String,
    pub quic_url: String,
    pub tls_ca: Option<String>,
    pub max_concurrent: usize,
    pub timeout_ms: u64,
    pub auto_reconnect: bool,
    pub reconnect_interval_ms: u64,
    pub keepalive_interval_ms: u64,
}

/// Klient iroh dla komunikacji z RAG engine.
pub struct RAGClient {
    config: Arc<RAGEngineConfigCompat>,
    inner: Arc<IrohServiceClient>,
    callback_tx: mpsc::UnboundedSender<(ModelRequest, mpsc::Sender<ModelResponse>)>,
    shutdown_rx: watch::Receiver<bool>,
}

impl RAGClient {
    pub async fn connect(
        config: RAGEngineConfigCompat,
        callback_tx: mpsc::UnboundedSender<(ModelRequest, mpsc::Sender<ModelResponse>)>,
        shutdown_rx: watch::Receiver<bool>,
    ) -> Result<Self> {
        let config = Arc::new(config);
        info!("Laczenie z RAG engine (iroh): {}", config.quic_url);

        let service_cfg = crate::net::iroh_client::IrohServiceConfig {
            name: config.name.clone(),
            url: config.quic_url.clone(),
            timeout_ms: config.timeout_ms,
            auto_reconnect: config.auto_reconnect,
            reconnect_interval_ms: config.reconnect_interval_ms,
            keepalive_interval_ms: config.keepalive_interval_ms,
            ..Default::default()
        };

        let inner = Arc::new(
            IrohServiceClient::connect(service_cfg, shutdown_rx.clone())
                .await
                .map_err(|e| CoreError::NetworkError {
                    message: format!("iroh connect RAG: {e}"),
                    source: anyhow::anyhow!(e.to_string()),
                })?,
        );

        info!("Polaczono z RAG engine: {}", config.name);

        let client = Self {
            config,
            inner,
            callback_tx,
            shutdown_rx,
        };

        client.spawn_callback_listener();

        Ok(client)
    }

    pub async fn send_request(&self, payload: RAGPayload) -> Result<RAGResult> {
        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::RAG(payload),
            stream: false,
            metadata: None,
            session_id: None,
        };
        debug!("RAG send_request: {}", request.request_id);

        let response = self
            .inner
            .send_request(request)
            .await
            .map_err(|e| CoreError::NetworkError {
                message: format!("iroh RAG request: {e}"),
                source: anyhow::anyhow!(e.to_string()),
            })?;

        match response.result {
            ModelResult::RAG(result) => Ok(result),
            ModelResult::Error(err) => Err(CoreError::BackendError {
                backend_url: format!("RAG ({})", self.config.name),
                message: err.message,
                source: err.details.map(anyhow::Error::msg),
            }
            .into()),
            _ => Err(CoreError::InternalError {
                message: "Unexpected ModelResult for RAG request".to_string(),
                source: None,
            }
            .into()),
        }
    }

    /// Ingest — wysyla `IngestRequest` w jednym bidi streamie jako
    /// length-prefixed rkyv, odbiera `IngestResponse`. Serwer RAG rozpoznaje
    /// typ po naglowku Envelope (SCHEMA_VERSION=5).
    pub async fn send_ingest_request(&self, request: IngestRequest) -> Result<IngestResponse> {
        debug!("RAG send_ingest_request: {}", request.request_id);

        let conn = self
            .inner
            .iroh_connection()
            .await
            .map_err(|e| CoreError::NetworkError {
                message: format!("iroh connection: {e}"),
                source: anyhow::anyhow!(e.to_string()),
            })?;

        let (mut send, mut recv) = conn.open_bi().await.map_err(|e| CoreError::NetworkError {
            message: format!("open_bi: {e}"),
            source: anyhow::anyhow!(e.to_string()),
        })?;

        write_frame(&mut send, &request)
            .await
            .map_err(|e| CoreError::NetworkError {
                message: format!("write ingest frame: {e}"),
                source: anyhow::anyhow!(e.to_string()),
            })?;
        let _ = send.finish();

        let response: IngestResponse = read_frame(&mut recv)
            .await
            .map_err(|e| CoreError::NetworkError {
                message: format!("read ingest response: {e}"),
                source: anyhow::anyhow!(e.to_string()),
            })?
            .ok_or_else(|| CoreError::NetworkError {
                message: "brak odpowiedzi ingest".to_string(),
                source: anyhow::anyhow!("peer closed stream"),
            })?;

        Ok(response)
    }

    /// Nasluchuje odwrotnych streamow od RAG engine — RAG wysyla `ModelRequest`
    /// z powrotem przez `accept_bi`, klient routuje je przez `callback_tx` do
    /// routera i odsyla `ModelResponse` tym samym strumieniem.
    fn spawn_callback_listener(&self) {
        let inner = Arc::clone(&self.inner);
        let callback_tx = self.callback_tx.clone();
        let config_name = self.config.name.clone();
        let mut shutdown_rx = self.shutdown_rx.clone();

        tokio::spawn(async move {
            info!("Callback listener start (RAG): {}", config_name);

            loop {
                if *shutdown_rx.borrow() {
                    info!("Callback listener shutdown: {}", config_name);
                    break;
                }

                let conn = match inner.iroh_connection().await {
                    Ok(c) => c,
                    Err(e) => {
                        debug!("Callback listener: brak polaczenia ({e}), czekam");
                        tokio::select! {
                            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => continue,
                            _ = shutdown_rx.changed() => {
                                if *shutdown_rx.borrow() { break; }
                            }
                        }
                        continue;
                    }
                };

                tokio::select! {
                    result = conn.accept_bi() => {
                        match result {
                            Ok((mut send, mut recv)) => {
                                let callback_tx = callback_tx.clone();
                                tokio::spawn(async move {
                                    let request: ModelRequest = match read_frame(&mut recv).await {
                                        Ok(Some(r)) => r,
                                        Ok(None) => return,
                                        Err(e) => {
                                            warn!("Callback read_frame error: {e}");
                                            return;
                                        }
                                    };

                                    let (resp_tx, mut resp_rx) = mpsc::channel::<ModelResponse>(1);
                                    if callback_tx.send((request, resp_tx)).is_err() {
                                        error!("Callback channel closed");
                                        return;
                                    }

                                    if let Some(resp) = resp_rx.recv().await {
                                        if let Err(e) = write_frame(&mut send, &resp).await {
                                            warn!("Callback write_frame error: {e}");
                                        }
                                        let _ = send.finish();
                                    }
                                });
                            }
                            Err(e) => {
                                debug!("Callback accept_bi zakonczone: {e}");
                                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                            }
                        }
                    }
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() { break; }
                    }
                }
            }
        });
    }

    pub fn config(&self) -> &RAGEngineConfigCompat {
        &self.config
    }

    pub fn inner_client(&self) -> Arc<IrohServiceClient> {
        Arc::clone(&self.inner)
    }
}
