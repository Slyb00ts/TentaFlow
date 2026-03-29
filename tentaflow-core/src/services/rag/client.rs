// =============================================================================
// Plik: services/rag/client.rs
// Opis: QUIC client dla komunikacji z RAG Engine. Obsluguje nawiazywanie
//       polaczenia, wysylanie RAGRequest/IngestRequest, callback requests
//       od RAG oraz auto-reconnect z keepalive.
// =============================================================================

use crate::error::{Result, CoreError};
use tentaflow_protocol::*;

use anyhow::Context;
use quinn::{ClientConfig, Connection, Endpoint, RecvStream};
use rustls::pki_types::CertificateDer;
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Mutex};
use tracing::{debug, error, info, warn};

/// Konfiguracja RAG engine (one-way TLS - klient NIE wysyla certyfikatu)
#[derive(Debug, Clone)]
pub struct RAGEngineConfigCompat {
    pub name: String,
    pub quic_url: String,
    /// CA cert dla weryfikacji serwera (opcjonalne - jesli None, uzywa systemowych CA)
    pub tls_ca: Option<String>,
    pub max_concurrent: usize,
    pub timeout_ms: u64,
    pub auto_reconnect: bool,
    pub reconnect_interval_ms: u64,
    pub keepalive_interval_ms: u64,
}

/// QUIC client dla komunikacji z RAG engine.
///
/// Utrzymuje persistent connection i obsluguje:
/// - Main request/response stream
/// - Callback requests od RAG
/// - Auto-reconnect i keepalive
pub struct RAGClient {
    /// Konfiguracja RAG engine
    config: Arc<RAGEngineConfigCompat>,

    /// QUIC connection (moze byc None podczas reconnect)
    connection: Arc<Mutex<Option<Connection>>>,

    /// Endpoint QUIC
    endpoint: Endpoint,

    /// Channel dla callback requests (RAG -> Router)
    callback_tx: mpsc::UnboundedSender<(ModelRequest, mpsc::Sender<ModelResponse>)>,

    /// Shutdown signal receiver (gdy true, nalezy zakonczyc)
    shutdown_rx: watch::Receiver<bool>,
}

impl RAGClient {
    /// Tworzy i laczy sie z RAG serverem przez QUIC.
    ///
    /// Parametry:
    /// - config: RAGEngineConfigCompat z adresem i certyfikatami
    /// - callback_tx: Channel dla callback requests od RAG
    /// - shutdown_rx: Receiver dla shutdown signal
    ///
    /// Zwraca: Polaczony RAGClient
    ///
    /// Bledy:
    /// - NetworkError: Jesli nie mozna polaczyc sie z RAG serverem
    /// - InternalError: Jesli certyfikaty sa niepoprawne
    pub async fn connect(
        config: RAGEngineConfigCompat,
        callback_tx: mpsc::UnboundedSender<(ModelRequest, mpsc::Sender<ModelResponse>)>,
        shutdown_rx: watch::Receiver<bool>,
    ) -> Result<Self> {
        let config = Arc::new(config);

        info!("Laczenie z RAG engine: {}", config.quic_url);

        // Ladowanie certyfikatow CA (opcjonalne)
        let ca_certs = Self::load_ca_certs(&config)?;

        // Konfiguracja QUIC client (one-way TLS)
        let client_config = Self::build_client_config(ca_certs)?;

        // Tworzenie endpointu QUIC
        let endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap())
            .context("Failed to create QUIC endpoint")
            .map_err(|e| CoreError::InternalError {
                message: "QUIC endpoint creation failed".to_string(),
                source: Some(e),
            })?;

        // Polaczenie z RAG serverem
        let connection = Self::connect_to_rag(&endpoint, &config, client_config).await?;

        info!("Polaczono z RAG engine: {}", config.name);

        let client = Self {
            config,
            connection: Arc::new(Mutex::new(Some(connection))),
            endpoint,
            callback_tx,
            shutdown_rx,
        };

        // Start callback listener
        client.spawn_callback_listener();

        // Start keepalive i auto-reconnect
        if client.config.auto_reconnect {
            client.spawn_keepalive_task();
        }

        Ok(client)
    }

    /// Wysyla ModelRequest (RAG payload) do RAG engine i czeka na RAGResult.
    ///
    /// Parametry:
    /// - payload: RAGPayload z query i parametrami
    ///
    /// Zwraca: RAGResult od RAG engine
    pub async fn send_request(&self, payload: RAGPayload) -> Result<RAGResult> {
        let request = ModelRequest {
            request_id: uuid::Uuid::new_v4().to_string(),
            payload: ModelPayload::RAG(payload),
            stream: false,
            metadata: None,
            session_id: None,
        };

        debug!("Wysylanie ModelRequest (RAG): {}", request.request_id);

        // Serializacja i wyslanie
        let serialized = rkyv::to_bytes::<rkyv::rancor::Error>(&request)
            .context("Failed to serialize ModelRequest")
            .map_err(|e| CoreError::InternalError {
                message: "rkyv serialization failed".to_string(),
                source: Some(e),
            })?;

        let timeout_ms = self.config.timeout_ms;
        let response_data = tokio::time::timeout(
            tokio::time::Duration::from_millis(timeout_ms),
            self.send_and_receive(&serialized)
        )
        .await
        .map_err(|_| CoreError::Timeout {
            backend_url: format!("RAG ({})", self.config.name),
            timeout_ms,
        })??;

        info!("Odpowiedz od RAG otrzymana: {} bajtow", response_data.len());

        // Deserializacja odpowiedzi (rkyv)
        let model_response = Self::deserialize_model_response(&response_data)?;

        debug!("ModelResponse otrzymany: {}", model_response.request_id);

        // Ekstrakcja RAG result
        match model_response.result {
            ModelResult::RAG(rag_result) => {
                debug!("RAGResult extracted: {} chunks", rag_result.metadata.len());
                Ok(rag_result)
            }
            ModelResult::Error(error_info) => {
                Err(CoreError::BackendError {
                    backend_url: "RAG Engine".to_string(),
                    message: error_info.message,
                    source: error_info.details.map(anyhow::Error::msg),
                }.into())
            }
            _ => {
                Err(CoreError::InternalError {
                    message: "Unexpected ModelResult type for RAG request".to_string(),
                    source: Some(anyhow::anyhow!("Expected RAG or Error result")),
                }.into())
            }
        }
    }

    /// Wysyla IngestRequest do RAG engine i czeka na IngestResponse.
    ///
    /// Parametry:
    /// - request: IngestRequest do wyslania
    ///
    /// Zwraca: IngestResponse od RAG engine
    pub async fn send_ingest_request(&self, request: IngestRequest) -> Result<IngestResponse> {
        debug!("Wysylanie IngestRequest: {}", request.request_id);

        // Serializacja z discriminator byte
        let request_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&request)
            .context("Failed to serialize IngestRequest")
            .map_err(|e| CoreError::InternalError {
                message: "rkyv serialization failed".to_string(),
                source: Some(e),
            })?;

        let timeout_ms = self.config.timeout_ms;
        let response_data = tokio::time::timeout(
            tokio::time::Duration::from_millis(timeout_ms),
            self.send_and_receive_with_discriminator(MESSAGE_TYPE_INGEST_REQUEST, &request_bytes)
        )
        .await
        .map_err(|_| CoreError::Timeout {
            backend_url: format!("RAG ({})", self.config.name),
            timeout_ms,
        })??;

        debug!("IngestRequest wyslany: {}", request.request_id);

        // Deserializacja odpowiedzi (rkyv)
        let response = Self::deserialize_ingest_response(&response_data)?;

        debug!("IngestResponse otrzymany: {}", response.request_id);

        Ok(response)
    }

    /// Maksymalny rozmiar odpowiedzi (64 MB)
    const MAX_RESPONSE_SIZE: usize = 64 * 1024 * 1024;

    /// Timeout dla operacji callback (30 sekund)
    const CALLBACK_TIMEOUT_MS: u64 = 30_000;

    /// Pobiera polaczenie, otwiera stream, wysyla dane i odbiera odpowiedz.
    async fn send_and_receive(&self, data: &[u8]) -> Result<Vec<u8>> {
        let conn = {
            let conn_lock = self.connection.lock().await;
            conn_lock
                .as_ref()
                .ok_or_else(|| CoreError::NetworkError {
                    message: "QUIC connection not available".to_string(),
                    source: anyhow::anyhow!("Connection is None during reconnect"),
                })?
                .clone()
        };

        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .context("Failed to open QUIC stream")
            .map_err(|e| CoreError::NetworkError {
                message: "Failed to open stream to RAG".to_string(),
                source: e,
            })?;

        send.write_all(data)
            .await
            .context("Failed to write to stream")
            .map_err(|e| CoreError::NetworkError {
                message: "Failed to send data".to_string(),
                source: e,
            })?;

        send.finish()
            .context("Failed to finish send stream")
            .map_err(|e| CoreError::NetworkError {
                message: "Failed to finish stream".to_string(),
                source: e,
            })?;

        Self::read_stream_data(&mut recv).await
    }

    /// Wysyla dane z discriminator byte na poczatku.
    async fn send_and_receive_with_discriminator(&self, discriminator: u8, data: &[u8]) -> Result<Vec<u8>> {
        let conn = {
            let conn_lock = self.connection.lock().await;
            conn_lock
                .as_ref()
                .ok_or_else(|| CoreError::NetworkError {
                    message: "QUIC connection not available".to_string(),
                    source: anyhow::anyhow!("Connection is None during reconnect"),
                })?
                .clone()
        };

        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .context("Failed to open QUIC stream")
            .map_err(|e| CoreError::NetworkError {
                message: "Failed to open stream to RAG".to_string(),
                source: e,
            })?;

        send.write_all(&[discriminator])
            .await
            .context("Failed to write discriminator")
            .map_err(|e| CoreError::NetworkError {
                message: "Failed to send discriminator".to_string(),
                source: e,
            })?;

        send.write_all(data)
            .await
            .context("Failed to write to stream")
            .map_err(|e| CoreError::NetworkError {
                message: "Failed to send data".to_string(),
                source: e,
            })?;

        send.finish()
            .context("Failed to finish send stream")
            .map_err(|e| CoreError::NetworkError {
                message: "Failed to finish stream".to_string(),
                source: e,
            })?;

        Self::read_stream_data(&mut recv).await
    }

    /// Laduje certyfikaty CA z pliku lub inline PEM (opcjonalne).
    /// Jesli brak CA, uzywa systemowych certyfikatow.
    fn load_ca_certs(config: &RAGEngineConfigCompat) -> Result<Vec<CertificateDer<'static>>> {
        match &config.tls_ca {
            Some(ca_value) => {
                let ca_pem = if ca_value.trim_start().starts_with("-----BEGIN") {
                    debug!("CA podane jako inline PEM");
                    ca_value.as_bytes().to_vec()
                } else {
                    std::fs::read(ca_value)
                        .context("Failed to read CA cert")
                        .map_err(|e| CoreError::ConfigError {
                            message: format!("Cannot read CA: {}", ca_value),
                            source: e,
                        })?
                };

                let ca_certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &ca_pem[..])
                    .map(|cert| cert.map_err(anyhow::Error::from))
                    .collect::<Result<Vec<_>>>()
                    .map_err(|e| CoreError::ConfigError {
                        message: "Nieprawidlowy format certyfikatu CA".to_string(),
                        source: e,
                    })?;

                if ca_certs.is_empty() {
                    return Err(CoreError::ConfigError {
                        message: "Brak certyfikatow CA w podanych danych".to_string(),
                        source: anyhow::anyhow!("Empty CA data"),
                    }.into());
                }

                debug!("Zaladowano {} certyfikatow CA", ca_certs.len());
                Ok(ca_certs)
            }
            None => {
                debug!("Brak CA - uzywam systemowych certyfikatow");
                Ok(vec![])
            }
        }
    }

    /// Buduje QUIC client config (one-way TLS - klient NIE wysyla certyfikatu).
    fn build_client_config(ca_certs: Vec<CertificateDer<'static>>) -> Result<ClientConfig> {
        let root_store = if ca_certs.is_empty() {
            // Systemowe certyfikaty CA
            let mut store = rustls::RootCertStore::empty();
            store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            debug!("Uzywam systemowych certyfikatow CA ({} certyfikatow)", store.len());
            store
        } else {
            // Podane certyfikaty CA
            let mut store = rustls::RootCertStore::empty();
            for ca in ca_certs {
                store.add(ca).context("Failed to add CA cert").map_err(|e| {
                    CoreError::InternalError {
                        message: "Failed to add CA to root store".to_string(),
                        source: Some(e),
                    }
                })?;
            }
            store
        };

        // One-way TLS: klient NIE wysyla certyfikatu
        let mut client_crypto = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        // ALPN protocol - musi zgadzac sie z serwerem RAG ("rag-protocol")
        client_crypto.alpn_protocols = vec![b"rag-protocol".to_vec()];

        let mut client_config = ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)
                .context("Failed to create QUIC config")
                .map_err(|e| CoreError::InternalError {
                    message: "QUIC config creation failed".to_string(),
                    source: Some(e),
                })?,
        ));

        // Transport config
        let mut transport = quinn::TransportConfig::default();

        // Max idle timeout 60 sekund (zgodne z RAG)
        transport.max_idle_timeout(Some(
            std::time::Duration::from_secs(60)
                .try_into()
                .unwrap(),
        ));

        // Keep-alive co 15 sekund aby utrzymac polaczenie QUIC (PING frames)
        transport.keep_alive_interval(Some(std::time::Duration::from_secs(15)));

        client_config.transport_config(Arc::new(transport));

        Ok(client_config)
    }

    /// Laczy sie z RAG serverem.
    async fn connect_to_rag(
        endpoint: &Endpoint,
        config: &RAGEngineConfigCompat,
        client_config: ClientConfig,
    ) -> Result<Connection> {
        // Parse URL (quic://host:port)
        let url = &config.quic_url;
        let addr = url
            .strip_prefix("quic://")
            .ok_or_else(|| CoreError::ConfigError {
                message: format!("Invalid QUIC URL: {}", url),
                source: anyhow::anyhow!("URL must start with quic://"),
            })?;

        // Parsuj jako SocketAddr (IP:port), jesli fail to resolve DNS
        let socket_addr: std::net::SocketAddr = match addr.parse() {
            Ok(sa) => sa,
            Err(_) => {
                // DNS retentaflown dla hostname
                let mut addrs = tokio::net::lookup_host(addr)
                    .await
                    .context("Failed to resolve hostname")
                    .map_err(|e| CoreError::ConfigError {
                        message: format!("Cannot resolve hostname: {}", addr),
                        source: e,
                    })?;

                addrs
                    .next()
                    .ok_or_else(|| CoreError::ConfigError {
                        message: format!("No IP addresses found for hostname: {}", addr),
                        source: anyhow::anyhow!("DNS retentaflown returned no addresses"),
                    })?
            }
        };

        let host = addr.split(':').next().unwrap_or("localhost");

        let connection = endpoint
            .connect_with(client_config, socket_addr, host)
            .context("Failed to initiate QUIC connection")
            .map_err(|e| CoreError::NetworkError {
                message: format!("Cannot connect to RAG: {}", url),
                source: e,
            })?
            .await
            .context("QUIC connection handshake failed")
            .map_err(|e| CoreError::NetworkError {
                message: "QUIC handshake failed".to_string(),
                source: e,
            })?;

        Ok(connection)
    }


    /// Deserializuje ModelResponse uzywajac rkyv.
    ///
    /// Konwertuje archived types (zero-copy) do normalnych Rust types.
    fn deserialize_model_response(data: &[u8]) -> Result<ModelResponse> {
        if data.is_empty() {
            return Err(CoreError::NetworkError {
                message: "Pusta odpowiedz od RAG".to_string(),
                source: anyhow::anyhow!("Empty response"),
            }.into());
        }

        // Bezpieczna deserializacja z walidacja
        let archived = rkyv::access::<ArchivedModelResponse, rkyv::rancor::Error>(data)
            .map_err(|e| CoreError::InternalError {
                message: format!("rkyv access error (ModelResponse): {}", e),
                source: Some(e.into()),
            })?;

        // Deserializuj przez rkyv deserialize trait
        let response: ModelResponse = rkyv::deserialize::<ModelResponse, rkyv::rancor::Error>(archived)
            .context("Failed to deserialize ModelResponse")
            .map_err(|e| CoreError::InternalError {
                message: "rkyv deserialization failed".to_string(),
                source: Some(e),
            })?;

        Ok(response)
    }

    /// Deserializuje IngestResponse uzywajac rkyv.
    fn deserialize_ingest_response(data: &[u8]) -> Result<IngestResponse> {
        if data.is_empty() {
            return Err(CoreError::NetworkError {
                message: "Pusta odpowiedz od RAG (ingest)".to_string(),
                source: anyhow::anyhow!("Empty response"),
            }.into());
        }

        // Bezpieczna deserializacja z walidacja
        let archived = rkyv::access::<ArchivedIngestResponse, rkyv::rancor::Error>(data)
            .map_err(|e| CoreError::InternalError {
                message: format!("rkyv access error (IngestResponse): {}", e),
                source: Some(e.into()),
            })?;

        let response = IngestResponse {
            request_id: archived.request_id.to_string(),
            document_id: archived.document_id.to_string(),
            status: match &archived.status {
                ArchivedIngestionStatus::Success => IngestionStatus::Success,
                ArchivedIngestionStatus::Duplicate => IngestionStatus::Duplicate,
                ArchivedIngestionStatus::Updated => IngestionStatus::Updated,
                ArchivedIngestionStatus::LinkedToDuplicate => IngestionStatus::LinkedToDuplicate,
                ArchivedIngestionStatus::Error => IngestionStatus::Error,
            },
            chunk_count: archived.chunk_count.into(),
            vector_count: archived.vector_count.into(),
            indexed_in: archived
                .indexed_in
                .iter()
                .map(|s| s.to_string())
                .collect(),
            metrics: IngestMetrics {
                file_processing_ms: archived.metrics.file_processing_ms.into(),
                chunking_ms: archived.metrics.chunking_ms.into(),
                embedding_ms: archived.metrics.embedding_ms.into(),
                fts_indexing_ms: archived.metrics.fts_indexing_ms.into(),
                vector_indexing_ms: archived.metrics.vector_indexing_ms.into(),
                graph_indexing_ms: archived.metrics.graph_indexing_ms.into(),
                total_ms: archived.metrics.total_ms.into(),
                embedding_tokens_per_sec: archived.metrics.embedding_tokens_per_sec.as_ref().map(|f| (*f).into()),
            },
            error: archived.error.as_ref().map(|s| s.to_string()),
        };

        Ok(response)
    }

    /// Czyta wszystkie dane ze streamu z limitem rozmiaru.
    async fn read_stream_data(stream: &mut RecvStream) -> Result<Vec<u8>> {
        let mut buffer = Vec::with_capacity(16384);
        let mut chunk = [0u8; 16384];

        loop {
            match stream.read(&mut chunk).await {
                Ok(Some(n)) => {
                    if buffer.len() + n > Self::MAX_RESPONSE_SIZE {
                        return Err(CoreError::InternalError {
                            message: format!(
                                "Odpowiedz przekroczyla limit {} bajtow",
                                Self::MAX_RESPONSE_SIZE
                            ),
                            source: Some(anyhow::anyhow!("Response too large")),
                        }
                        .into());
                    }
                    buffer.extend_from_slice(&chunk[..n]);
                }
                Ok(None) => break,
                Err(e) => {
                    return Err(CoreError::NetworkError {
                        message: "Failed to read from stream".to_string(),
                        source: anyhow::Error::new(e),
                    }
                    .into())
                }
            }
        }

        Ok(buffer)
    }

    /// Startuje task nasluchujacy callback requests od RAG.
    fn spawn_callback_listener(&self) {
        let connection = self.connection.clone();
        let callback_tx = self.callback_tx.clone();
        let config_name = self.config.name.clone();
        let mut shutdown_rx = self.shutdown_rx.clone();

        tokio::spawn(async move {
            info!("Callback listener started dla RAG: {}", config_name);

            loop {
                // Sprawdz shutdown przed kazda iteracja
                if *shutdown_rx.borrow() {
                    info!("Callback listener konczy sie (shutdown): {}", config_name);
                    break;
                }

                // Get connection
                let conn = {
                    let conn_lock = connection.lock().await;
                    match conn_lock.as_ref() {
                        Some(c) => c.clone(),
                        None => {
                            // Czekaj na reconnect lub shutdown
                            tokio::select! {
                                _ = tokio::time::sleep(tokio::time::Duration::from_secs(1)) => {
                                    continue;
                                }
                                _ = shutdown_rx.changed() => {
                                    info!("Callback listener otrzymal shutdown signal: {}", config_name);
                                    break;
                                }
                            }
                        }
                    }
                };

                // Accept incoming bi-directional stream (callback od RAG)
                // z obsluga shutdown signal
                tokio::select! {
                    result = conn.accept_bi() => {
                        match result {
                            Ok((mut send, mut recv)) => {
                                let callback_tx = callback_tx.clone();

                                tokio::spawn(async move {
                                    let callback_timeout = tokio::time::Duration::from_millis(Self::CALLBACK_TIMEOUT_MS);

                                    // Odczyt danych callback z timeoutem
                                    let read_result = tokio::time::timeout(
                                        callback_timeout,
                                        Self::read_stream_data(&mut recv)
                                    ).await;

                                    let data = match read_result {
                                        Ok(Ok(d)) => d,
                                        Ok(Err(e)) => {
                                            error!("Failed to read callback request: {}", e);
                                            return;
                                        }
                                        Err(_) => {
                                            error!("Timeout reading callback request");
                                            return;
                                        }
                                    };

                                    match Self::deserialize_callback_request(&data) {
                                                Ok(callback_req) => {
                                                    debug!("Callback request: {}", callback_req.request_id);

                                                    let (resp_tx, mut resp_rx) = mpsc::channel(1);

                                                    if callback_tx.send((callback_req, resp_tx)).is_err() {
                                                        error!("Failed to send callback to handler");
                                                        return;
                                                    }

                                                    // Oczekiwanie na odpowiedz handlera z timeoutem
                                                    let recv_result = tokio::time::timeout(
                                                        tokio::time::Duration::from_millis(Self::CALLBACK_TIMEOUT_MS),
                                                        resp_rx.recv()
                                                    ).await;

                                                    if let Ok(Some(callback_resp)) = recv_result {
                                                        // Serializuj i wyslij odpowiedz
                                                        match Self::serialize_callback_response(&callback_resp) {
                                                            Ok(resp_data) => {
                                                                if let Err(e) = send.write_all(&resp_data).await {
                                                                    error!("Failed to send callback response: {}", e);
                                                                }
                                                                let _ = send.finish();
                                                            }
                                                            Err(e) => {
                                                                error!("Failed to serialize callback response: {}", e);
                                                            }
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    error!("Failed to deserialize callback request: {}", e);
                                                }
                                            }
                                });
                            }
                            Err(e) => {
                                warn!("Failed to accept callback stream: {}", e);
                                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                            }
                        }
                    }
                    _ = shutdown_rx.changed() => {
                        info!("Callback listener otrzymal shutdown signal: {}", config_name);
                        break;
                    }
                }
            }

            info!("Callback listener zakonczony: {}", config_name);
        });
    }

    /// Deserializuje ModelRequest (callback) uzywajac rkyv.
    fn deserialize_callback_request(data: &[u8]) -> Result<ModelRequest> {
        let archived = rkyv::access::<ArchivedModelRequest, rkyv::rancor::Error>(data)
            .context("Failed to access archived ModelRequest")?;

        // Deserializuj przez rkyv deserialize trait
        let request: ModelRequest = rkyv::deserialize::<ModelRequest, rkyv::rancor::Error>(archived)
            .context("Failed to deserialize ModelRequest")
            .map_err(|e| CoreError::InternalError {
                message: "rkyv deserialization failed".to_string(),
                source: Some(e),
            })?;

        Ok(request)
    }

    /// Serializuje ModelResponse (callback) uzywajac rkyv.
    fn serialize_callback_response(response: &ModelResponse) -> Result<Vec<u8>> {
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(response)
            .context("Failed to serialize ModelResponse")
            .map_err(|e| CoreError::InternalError {
                message: "rkyv serialization failed".to_string(),
                source: Some(e),
            })?;

        Ok(bytes.into_vec())
    }

    /// Startuje connection monitor + auto-reconnect task dla RAG.
    ///
    /// Uzywa `conn.closed().await` zamiast polling - natychmiast wykrywa
    /// zamkniecie polaczenia (timeout, error, explicit close).
    fn spawn_keepalive_task(&self) {
        let connection = self.connection.clone();
        let endpoint = self.endpoint.clone();
        let config = self.config.clone();
        let mut shutdown_rx = self.shutdown_rx.clone();
        let reconnect_interval = tokio::time::Duration::from_millis(config.reconnect_interval_ms);

        tokio::spawn(async move {
            info!("Connection monitor started dla RAG: {}", config.name);

            loop {
                // Sprawdz shutdown przed kazda iteracja
                if *shutdown_rx.borrow() {
                    info!("Keepalive task konczy sie (shutdown): {}", config.name);
                    break;
                }

                // Pobierz aktualne polaczenie
                let conn = {
                    let guard = connection.lock().await;
                    guard.clone()
                };

                if let Some(conn) = conn {
                    // Czekaj na zamkniecie polaczenia LUB shutdown signal
                    tokio::select! {
                        close_error = conn.closed() => {
                            warn!(
                                "Polaczenie QUIC do RAG zamkniete: {} - powod: {:?}",
                                config.name, close_error
                            );

                            // Wyczysc stare polaczenie
                            {
                                let mut guard = connection.lock().await;
                                *guard = None;
                            }
                        }
                        _ = shutdown_rx.changed() => {
                            info!("Keepalive task otrzymal shutdown signal: {}", config.name);
                            // Zamknij polaczenie gracefully
                            conn.close(0u32.into(), b"shutdown");
                            break;
                        }
                    }
                }

                // Sprawdz shutdown przed reconnect
                if *shutdown_rx.borrow() {
                    info!("Keepalive task konczy sie (shutdown przed reconnect): {}", config.name);
                    break;
                }

                // Polaczenie zamkniete lub nie istnieje - sprobuj reconnect
                if config.auto_reconnect {
                    info!("Reconnecting to RAG: {} (za {}ms)...", config.name, config.reconnect_interval_ms);

                    // Czekaj na reconnect interval LUB shutdown
                    tokio::select! {
                        _ = tokio::time::sleep(reconnect_interval) => {}
                        _ = shutdown_rx.changed() => {
                            info!("Keepalive task otrzymal shutdown signal podczas oczekiwania: {}", config.name);
                            break;
                        }
                    }

                    // Sprawdz shutdown po sleep
                    if *shutdown_rx.borrow() {
                        break;
                    }

                    match Self::reconnect(&endpoint, &config).await {
                        Ok(new_conn) => {
                            let mut conn_lock = connection.lock().await;
                            *conn_lock = Some(new_conn);
                            info!("Reconnected to RAG: {}", config.name);
                        }
                        Err(e) => {
                            error!("Reconnect to RAG failed: {} - {}", config.name, e);
                            // Poczekaj przed kolejna proba z obsluga shutdown
                            tokio::select! {
                                _ = tokio::time::sleep(reconnect_interval) => {}
                                _ = shutdown_rx.changed() => {
                                    info!("Keepalive task otrzymal shutdown signal: {}", config.name);
                                    break;
                                }
                            }
                        }
                    }
                } else {
                    warn!("Auto-reconnect wylaczony dla RAG: {}", config.name);
                    break;
                }
            }

            info!("Keepalive task zakonczony: {}", config.name);
        });
    }

    /// Reconnect do RAG servera.
    async fn reconnect(endpoint: &Endpoint, config: &RAGEngineConfigCompat) -> Result<Connection> {
        let ca_certs = Self::load_ca_certs(config)?;
        let client_config = Self::build_client_config(ca_certs)?;
        Self::connect_to_rag(endpoint, config, client_config).await
    }
}
