// =============================================================================
// Plik: tentaflow-transport/src/client.rs
// Opis: `ServiceClient` — trzyma pojedyncze `iroh::Connection` z auto-reconnect.
//       API: `request(ModelRequest) → ModelResponse` (unary) i `open_bi(req)`
//       (streaming). Uzywany jednocześnie przez tentaflow-core (komunikacja z
//       sidecarami) oraz tentaflow-client/native (komunikacja z nodem).
// =============================================================================

use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::{Connection, RecvStream, SendStream};
use iroh::{Endpoint, EndpointAddr, EndpointId};
use parking_lot::Mutex;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use tentaflow_protocol::{ModelRequest, ModelResponse};

use crate::endpoint::DEFAULT_REQUEST_TIMEOUT;
use crate::error::TransportError;
use crate::framing::{read_frame, write_frame};
use crate::ALPN_SERVICE;

/// Konfiguracja klienta.
#[derive(Clone)]
pub struct ServiceClientConfig {
    /// Nazwa serwisu dla logow.
    pub name: String,
    /// Docelowy `EndpointId` (32-bajtowy Ed25519 public key).
    pub endpoint_id: EndpointId,
    /// ALPN — defaultowo `tentaflow-service/v1`.
    pub alpn: Vec<u8>,
    /// Timeout pojedynczego unary requestu.
    pub request_timeout: Duration,
    /// Czy w tle ma dzialac watcher odpalajacy reconnect po utracie polaczenia.
    pub auto_reconnect: bool,
    /// Interwal miedzy probami reconnect gdy polaczenia nie ma.
    pub reconnect_interval: Duration,
}

impl ServiceClientConfig {
    /// Minimalna konfiguracja dla podanego EndpointId.
    pub fn new(name: impl Into<String>, endpoint_id: EndpointId) -> Self {
        Self {
            name: name.into(),
            endpoint_id,
            alpn: ALPN_SERVICE.to_vec(),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            auto_reconnect: true,
            reconnect_interval: Duration::from_secs(2),
        }
    }
}

/// Klient iroh dla pojedynczego peera (sidecar albo node).
pub struct ServiceClient {
    endpoint: Endpoint,
    config: Arc<ServiceClientConfig>,
    connection: Arc<Mutex<Option<Connection>>>,
    shutdown_rx: watch::Receiver<bool>,
}

impl ServiceClient {
    /// Binduje endpoint (albo przyjmuje istniejacy) i laczy sie z peerem.
    /// Preferowana forma — owner endpointa tworzy go raz i re-uzywa dla wielu
    /// klientow, zeby oszczedzic socketow UDP.
    pub async fn connect(
        endpoint: Endpoint,
        config: ServiceClientConfig,
        shutdown_rx: watch::Receiver<bool>,
    ) -> Result<Self, TransportError> {
        let config = Arc::new(config);
        let addr = EndpointAddr::new(config.endpoint_id);
        let connection = endpoint
            .connect(addr, &config.alpn)
            .await
            .map_err(|e| TransportError::connect(format!("{}: {e:?}", config.name)))?;

        info!(service = %config.name, endpoint_id = %config.endpoint_id.fmt_short(), "iroh service client polaczony");

        let client = Self {
            endpoint,
            config,
            connection: Arc::new(Mutex::new(Some(connection))),
            shutdown_rx,
        };

        if client.config.auto_reconnect {
            client.spawn_keepalive_task();
        }

        Ok(client)
    }

    pub fn name(&self) -> &str {
        &self.config.name
    }

    pub fn endpoint_id(&self) -> EndpointId {
        self.config.endpoint_id
    }

    /// Wysyla `ModelRequest`, czeka na pelny `ModelResponse` i zwraca go.
    pub async fn request(
        &self,
        request: ModelRequest,
    ) -> Result<ModelResponse, TransportError> {
        let conn = self.ensure_connection().await?;
        let timeout = self.config.request_timeout;

        let task = async move {
            let (mut send, mut recv) = conn
                .open_bi()
                .await
                .map_err(|e| TransportError::closed(format!("{e}")))?;

            write_frame(&mut send, &request).await?;
            send.finish().map_err(|e| TransportError::closed(format!("finish: {e}")))?;

            let response = read_frame::<ModelResponse>(&mut recv)
                .await?
                .ok_or(TransportError::PeerClosedEarly)?;
            Ok::<ModelResponse, TransportError>(response)
        };

        match tokio::time::timeout(timeout, task).await {
            Ok(res) => res,
            Err(_) => Err(TransportError::Timeout {
                ms: timeout.as_millis() as u64,
            }),
        }
    }

    /// Otwiera bidi stream i wysyla pojedynczy `ModelRequest`. Zwraca pelny
    /// send/recv — caller czyta kolejne ramki `ModelStreamChunk` do zamkniecia
    /// recv.
    pub async fn open_bi(
        &self,
        request: ModelRequest,
    ) -> Result<(SendStream, RecvStream), TransportError> {
        let conn = self.ensure_connection().await?;
        let (mut send, recv) = conn
            .open_bi()
            .await
            .map_err(|e| TransportError::closed(format!("{e}")))?;
        write_frame(&mut send, &request).await?;
        Ok((send, recv))
    }

    /// Sprawdza i odswieza polaczenie jesli sie zamknelo.
    pub async fn ensure_connection(&self) -> Result<Connection, TransportError> {
        let current = self.connection.lock().clone();
        if let Some(conn) = current {
            if conn.close_reason().is_none() {
                return Ok(conn);
            }
        }

        let addr = EndpointAddr::new(self.config.endpoint_id);
        let new_conn = self
            .endpoint
            .connect(addr, &self.config.alpn)
            .await
            .map_err(|e| {
                TransportError::connect(format!("reconnect {}: {e:?}", self.config.name))
            })?;

        *self.connection.lock() = Some(new_conn.clone());
        Ok(new_conn)
    }

    /// Zamyka polaczenie i endpoint. Po `close()` klient nie moze byc uzyty
    /// ponownie.
    pub async fn close(self) {
        if let Some(conn) = self.connection.lock().take() {
            conn.close(0u32.into(), b"client_shutdown");
        }
        self.endpoint.close().await;
    }

    fn spawn_keepalive_task(&self) {
        let connection = Arc::clone(&self.connection);
        let endpoint = self.endpoint.clone();
        let config = Arc::clone(&self.config);
        let mut shutdown_rx = self.shutdown_rx.clone();

        tokio::spawn(async move {
            loop {
                if *shutdown_rx.borrow() {
                    debug!(service = %config.name, "keepalive: shutdown");
                    break;
                }

                let conn_snapshot = connection.lock().clone();
                let Some(conn) = conn_snapshot else {
                    tokio::select! {
                        _ = tokio::time::sleep(config.reconnect_interval) => {}
                        _ = shutdown_rx.changed() => {
                            if *shutdown_rx.borrow() { break; }
                        }
                    }

                    let addr = EndpointAddr::new(config.endpoint_id);
                    match endpoint.connect(addr, &config.alpn).await {
                        Ok(new_conn) => {
                            *connection.lock() = Some(new_conn);
                            info!(service = %config.name, "iroh service reconnected");
                        }
                        Err(e) => {
                            warn!(service = %config.name, "iroh reconnect fail: {e:?}");
                        }
                    }
                    continue;
                };

                tokio::select! {
                    close_reason = conn.closed() => {
                        warn!(service = %config.name, "iroh connection closed: {close_reason:?}");
                        *connection.lock() = None;
                    }
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            conn.close(0u32.into(), b"shutdown");
                            break;
                        }
                    }
                }
            }
        });
    }
}
