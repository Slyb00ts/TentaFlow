// =============================================================================
// Plik: net/quic/client.rs
// Opis: Uniwersalny klient QUIC — transport dla wszystkich TentaFlow Services.
//       Obsluguje polaczenia TLS, keepalive, auto-reconnect i streaming.
//       Nie wie nic o typach payload — to czysty transport QUIC + rkyv.
// Przyklad:
//   let client = QuicClient::connect(config, shutdown_rx).await?;
//   let response = client.send_request(request).await?;
// =============================================================================

use crate::error::CoreError;
use crate::net::quic::tls;
use tentaflow_protocol::*;

use anyhow::Context;
use quinn::{ClientConfig, Connection, Endpoint};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, watch};

/// Weryfikator TLS pomijajacy walidacje certyfikatu serwera.
/// Uzywany dla self-signed kontenerow (teams-bot, itp.).
#[derive(Debug)]
struct SkipServerVerification;

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}
use tracing::{debug, error, info, warn};

/// Konfiguracja QUIC polaczenia
#[derive(Debug, Clone)]
pub struct QuicConfig {
    /// Nazwa serwisu (dla logowania)
    pub name: String,

    /// QUIC URL (quic://host:port)
    pub url: String,

    /// Sciezka do certyfikatu CA (opcjonalnie - jesli None, uzywa systemowych CA)
    pub tls_ca: Option<String>,

    /// Nazwa serwera TLS (SNI) - jesli None, uzywany jest host z URL
    pub server_name: Option<String>,

    /// ALPN protocol (np. "h3" dla HTTP/3, "rag-protocol" dla RAG)
    pub alpn: String,

    /// Timeout dla requestow (ms)
    pub timeout_ms: u64,

    /// Czy wlaczyc auto-reconnect
    pub auto_reconnect: bool,

    /// Interwal miedzy probami reconnect (ms)
    pub reconnect_interval_ms: u64,

    /// Interwal keepalive (ms)
    pub keepalive_interval_ms: u64,

    /// Pomin walidacje certyfikatu serwera (dla self-signed kontenerow)
    pub skip_tls_verify: bool,
}

/// Uniwersalny QUIC client dla TentaFlow services.
///
/// Obsluguje wszystkie typy serwisow przez ModelRequest/ModelResponse.
/// Nie wie nic o specyfice payload — to czysty transport QUIC + rkyv.
pub struct QuicClient {
    /// Konfiguracja
    config: Arc<QuicConfig>,

    /// QUIC connection (moze byc None podczas reconnect)
    connection: Arc<Mutex<Option<Connection>>>,

    /// QUIC endpoint
    endpoint: Endpoint,

    /// Shutdown signal receiver (gdy true, nalezy zakonczyc)
    shutdown_rx: watch::Receiver<bool>,
}

impl QuicClient {
    /// Tworzy i laczy sie z TentaFlow service przez QUIC.
    ///
    /// Parametry:
    /// - config: QuicConfig z adresem i certyfikatami TLS
    /// - shutdown_rx: Receiver sygnalu shutdown (watch channel)
    ///
    /// Zwraca: Polaczony QuicClient
    pub async fn connect(config: QuicConfig, shutdown_rx: watch::Receiver<bool>) -> Result<Self, CoreError> {
        let config = Arc::new(config);

        debug!("Laczenie przez QUIC: {} ({})", config.name, config.url);

        // Zaladuj certyfikaty CA (jesli podano)
        let ca_certs = Self::load_ca_certs(&config)?;

        // Zbuduj konfig klienta QUIC (one-way TLS)
        let client_config = Self::build_client_config(ca_certs, &config)?;

        // Utworz endpoint
        let endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap())
            .context("Failed to create QUIC endpoint")
            .map_err(|e| CoreError::InternalError {
                message: "QUIC endpoint creation failed".to_string(),
                source: Some(e),
            })?;

        // Polacz
        let connection = Self::connect_to_server(&endpoint, &config, client_config).await?;

        debug!("Polaczono przez QUIC: {}", config.name);

        let client = Self {
            config,
            connection: Arc::new(Mutex::new(Some(connection))),
            endpoint,
            shutdown_rx,
        };

        // Start keepalive & auto-reconnect
        if client.config.auto_reconnect {
            client.spawn_keepalive_task();
        }

        Ok(client)
    }

    /// Wysyla ModelRequest i odbiera ModelResponse.
    ///
    /// Uniwersalna metoda — dziala dla wszystkich typow payload.
    pub async fn send_request(&self, request: ModelRequest) -> Result<ModelResponse, CoreError> {
        debug!("QuicClient.send_request: START request_id={}", request.request_id);

        let request_bytes = self.serialize_request(&request)?;

        debug!("QuicClient.send_request: zserializowano {} bajtow", request_bytes.len());

        // Wyslij i odbierz przez QUIC z timeoutem
        let timeout_duration = Duration::from_millis(self.config.timeout_ms);
        let response = tokio::time::timeout(timeout_duration, self.send_and_receive(&request_bytes))
            .await
            .map_err(|_| CoreError::Timeout {
                backend_url: self.config.url.clone(),
                timeout_ms: self.config.timeout_ms,
            })?
            ?;

        debug!(
            "ModelResponse odebrany: {} (result: {:?})",
            response.request_id,
            std::mem::discriminant(&response.result)
        );

        Ok(response)
    }

    /// Serializuje ModelRequest do bajtow rkyv (zero-copy).
    fn serialize_request(&self, request: &ModelRequest) -> Result<rkyv::util::AlignedVec, CoreError> {
        rkyv::to_bytes::<rkyv::rancor::Error>(request)
            .map_err(|e| CoreError::InternalError {
                message: format!("rkyv serialization error: {}", e),
                source: Some(e.into()),
            })
    }

    /// Wysyla request bytes przez QUIC i odbiera response.
    ///
    /// Otwiera bi-directional stream, wysyla, odbiera.
    /// Lock na connection trzymany tylko przez czas klonowania —
    /// quinn::Connection jest Clone (tanie, Arc wewnetrznie).
    async fn send_and_receive(&self, request_bytes: &[u8]) -> Result<ModelResponse, CoreError> {
        debug!("send_and_receive: START {} bajtow", request_bytes.len());

        // Pobierz connection — krotki lock tylko na czas klonowania
        let conn = {
            let conn_guard = self.connection.lock().await;
            conn_guard.as_ref().ok_or_else(|| CoreError::NetworkError {
                message: format!("Brak polaczenia QUIC z: {}", self.config.name),
                source: anyhow::anyhow!("Connection is None (reconnecting?)"),
            })?.clone()
        };

        debug!("send_and_receive: connection OK (lock zwolniony)");

        // Otworz bi-directional stream
        debug!("send_and_receive: otwieranie bi-directional stream...");
        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .map_err(|e| CoreError::NetworkError {
                message: format!("Nie mozna otworzyc QUIC stream: {}", e),
                source: e.into(),
            })?;
        debug!("send_and_receive: stream otwarty");

        // Wyslij request
        debug!("send_and_receive: wysylanie {} bajtow...", request_bytes.len());
        send.write_all(request_bytes)
            .await
            .map_err(|e| CoreError::NetworkError {
                message: format!("Blad wysylania request: {}", e),
                source: e.into(),
            })?;
        debug!("send_and_receive: write_all zakonczone");

        send.finish()
            .map_err(|e| CoreError::NetworkError {
                message: format!("Blad konczenia stream: {}", e),
                source: e.into(),
            })?;
        debug!("send_and_receive: finish() zakonczone, czekam na response...");

        // Odbierz response (max 10MB)
        let response_bytes = recv
            .read_to_end(10_000_000)
            .await
            .map_err(|e| CoreError::NetworkError {
                message: format!("Blad odbierania response: {}", e),
                source: e.into(),
            })?;

        debug!("send_and_receive: response odebrany, {} bajtow", response_bytes.len());

        if response_bytes.is_empty() {
            return Err(CoreError::NetworkError {
                message: "Pusta odpowiedz od serwisu".to_string(),
                source: anyhow::anyhow!("Empty response"),
            });
        }

        // Deserializuj z rkyv (z walidacja)
        use tentaflow_protocol::ArchivedModelResponse;
        let archived = rkyv::access::<ArchivedModelResponse, rkyv::rancor::Error>(&response_bytes)
            .map_err(|e| CoreError::InternalError {
                message: format!("rkyv access error (nieprawidlowa odpowiedz): {}", e),
                source: Some(e.into()),
            })?;

        let response: ModelResponse = rkyv::deserialize::<ModelResponse, rkyv::rancor::Error>(archived)
            .map_err(|e| CoreError::InternalError {
                message: format!("rkyv deserialization error: {}", e),
                source: Some(e.into()),
            })?;

        debug!("send_and_receive: deserializacja OK");
        Ok(response)
    }

    /// Laduje certyfikaty CA z pliku lub inline PEM (opcjonalne).
    /// Jesli brak CA, uzywa systemowych certyfikatow.
    fn load_ca_certs(config: &QuicConfig) -> Result<Vec<CertificateDer<'static>>, CoreError> {
        match &config.tls_ca {
            Some(ca_value) => tls::load_ca_certs(ca_value),
            None => {
                debug!("Brak CA — uzywam systemowych certyfikatow");
                Ok(vec![])
            }
        }
    }

    /// Buduje QUIC client config (one-way TLS — klient nie wysyla certyfikatu).
    fn build_client_config(
        ca_certs: Vec<CertificateDer<'static>>,
        config: &QuicConfig,
    ) -> Result<ClientConfig, CoreError> {
        let alpn = &config.alpn;
        use rustls::RootCertStore;

        let mut client_crypto = if config.skip_tls_verify {
            // Pomin walidacje certyfikatu serwera (self-signed kontenery)
            debug!("TLS: pomijam walidacje certyfikatu serwera (skip_tls_verify=true)");
            rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
                .with_no_client_auth()
        } else {
            let root_store = if ca_certs.is_empty() {
                let mut store = RootCertStore::empty();
                store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
                debug!("Uzywam systemowych certyfikatow CA ({} certyfikatow)", store.len());
                store
            } else {
                let mut store = RootCertStore::empty();
                for ca_cert in ca_certs {
                    store
                        .add(ca_cert)
                        .context("Failed to add CA cert to root store")
                        .map_err(|e| CoreError::ConfigError {
                            message: "Invalid CA certificate".to_string(),
                            source: e,
                        })?;
                }
                store
            };

            rustls::ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth()
        };

        // ALPN protocol — musi zgadzac sie z serwerem
        client_crypto.alpn_protocols = vec![alpn.as_bytes().to_vec()];
        debug!("QUIC client ALPN: {}", alpn);

        let mut client_config =
            ClientConfig::new(Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(
                client_crypto,
            )
            .map_err(|e| CoreError::ConfigError {
                message: format!("QUIC client config error: {}", e),
                source: anyhow::anyhow!(e),
            })?));

        // Transport config (timeout, keepalive) — wartosci z konfiguracji
        let mut transport = quinn::TransportConfig::default();
        let idle_timeout = Duration::from_millis(config.timeout_ms)
            .try_into()
            .map_err(|e| CoreError::ConfigError {
                message: format!("Nieprawidlowy timeout_ms: {}", config.timeout_ms),
                source: anyhow::anyhow!("{}", e),
            })?;
        transport.max_idle_timeout(Some(idle_timeout));
        transport.keep_alive_interval(Some(Duration::from_millis(config.keepalive_interval_ms)));
        // Pozwol serwerowi otwierac streamy do klienta (reverse QUIC — kontenery wysylaja STT/TTS requesty)
        transport.max_concurrent_bidi_streams(64u32.into());

        client_config.transport_config(Arc::new(transport));

        Ok(client_config)
    }

    /// Laczy sie z serwerem QUIC.
    async fn connect_to_server(
        endpoint: &Endpoint,
        config: &QuicConfig,
        client_config: ClientConfig,
    ) -> Result<Connection, CoreError> {
        // Parse URL (quic://host:port)
        let url = &config.url;
        let addr = url
            .strip_prefix("quic://")
            .ok_or_else(|| CoreError::ConfigError {
                message: format!("Invalid QUIC URL: {}", url),
                source: anyhow::anyhow!("URL must start with quic://"),
            })?;

        // Parse host:port (rsplit_once obsluguje IPv6 np. [::1]:8080)
        let (host, port_str) = addr.rsplit_once(':').ok_or_else(|| CoreError::ConfigError {
            message: format!("Invalid QUIC URL format: {}", url),
            source: anyhow::anyhow!("Expected format: quic://host:port"),
        })?;

        let port: u16 = port_str.parse().map_err(|e: std::num::ParseIntError| CoreError::ConfigError {
            message: format!("Invalid port in URL: {}", url),
            source: anyhow::anyhow!(e),
        })?;

        // Resolve DNS
        let socket_addr = tokio::net::lookup_host(format!("{}:{}", host, port))
            .await
            .context("DNS lookup failed")
            .map_err(|e| CoreError::NetworkError {
                message: format!("Cannot resolve host: {}", host),
                source: e,
            })?
            .next()
            .ok_or_else(|| CoreError::NetworkError {
                message: format!("No IP address found for host: {}", host),
                source: anyhow::anyhow!("DNS lookup returned empty"),
            })?;

        // SNI: jesli podano server_name, uzyj go zamiast hosta z URL
        let sni = config.server_name.as_deref().unwrap_or(host);
        info!("Laczenie QUIC: {} -> {} (SNI: '{}')", host, socket_addr, sni);

        // Connect
        let conn = endpoint
            .connect_with(client_config, socket_addr, sni)
            .map_err(|e| CoreError::NetworkError {
                message: format!("QUIC connect error: {}", e),
                source: e.into(),
            })?
            .await
            .map_err(|e| CoreError::NetworkError {
                message: format!("QUIC handshake failed: {}", e),
                source: e.into(),
            })?;

        debug!("QUIC connection established");

        Ok(conn)
    }

    /// Reconnect do serwera.
    async fn reconnect(endpoint: &Endpoint, config: &QuicConfig) -> Result<Connection, CoreError> {
        let ca_certs = Self::load_ca_certs(config)?;
        let client_config = Self::build_client_config(ca_certs, config)?;
        Self::connect_to_server(endpoint, config, client_config).await
    }

    /// Spawns connection monitor + auto-reconnect task.
    ///
    /// Uzywa `conn.closed().await` zamiast polling — natychmiast wykrywa
    /// zamkniecie polaczenia (timeout, error, explicit close).
    fn spawn_keepalive_task(&self) {
        let connection = self.connection.clone();
        let endpoint = self.endpoint.clone();
        let config = self.config.clone();
        let mut shutdown_rx = self.shutdown_rx.clone();
        let reconnect_interval = std::time::Duration::from_millis(config.reconnect_interval_ms);

        tokio::spawn(async move {
            loop {
                // Sprawdz czy mamy shutdown
                if *shutdown_rx.borrow() {
                    debug!("Keepalive task konczy sie (shutdown): {}", config.name);
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
                                "Polaczenie QUIC zamkniete: {} - powod: {:?}",
                                config.name, close_error
                            );
                            // Wyczysc stare polaczenie
                            {
                                let mut guard = connection.lock().await;
                                *guard = None;
                            }
                        }
                        _ = shutdown_rx.changed() => {
                            debug!("Keepalive task otrzymal shutdown signal: {}", config.name);
                            // Zamknij polaczenie
                            conn.close(0u32.into(), b"shutdown");
                            break;
                        }
                    }
                }

                // Sprawdz ponownie shutdown przed reconnect
                if *shutdown_rx.borrow() {
                    debug!("Keepalive task konczy sie przed reconnect (shutdown): {}", config.name);
                    break;
                }

                // Polaczenie zamkniete lub nie istnieje — sprobuj reconnect
                if config.auto_reconnect {
                    debug!("Reconnecting QUIC: {} (za {}ms)...", config.name, config.reconnect_interval_ms);

                    // Czekaj reconnect_interval, ale przerwij jesli shutdown
                    tokio::select! {
                        _ = tokio::time::sleep(reconnect_interval) => {}
                        _ = shutdown_rx.changed() => {
                            debug!("Keepalive task otrzymal shutdown podczas sleep: {}", config.name);
                            break;
                        }
                    }

                    // Sprawdz shutdown po sleep
                    if *shutdown_rx.borrow() {
                        break;
                    }

                    match Self::reconnect(&endpoint, &config).await {
                        Ok(new_conn) => {
                            let mut conn_guard = connection.lock().await;
                            *conn_guard = Some(new_conn);
                            debug!("Reconnect QUIC OK: {}", config.name);
                        }
                        Err(e) => {
                            error!("Reconnect QUIC failed: {} - {}", config.name, e);
                            // Poczekaj przed kolejna proba (z obsluga shutdown)
                            tokio::select! {
                                _ = tokio::time::sleep(reconnect_interval) => {}
                                _ = shutdown_rx.changed() => {
                                    debug!("Keepalive task otrzymal shutdown podczas retry sleep: {}", config.name);
                                    break;
                                }
                            }
                        }
                    }
                } else {
                    warn!("Auto-reconnect wylaczony dla: {}", config.name);
                    break;
                }
            }
            debug!("Keepalive task zakonczony: {}", config.name);
        });
    }

    /// Zwraca nazwe serwisu (dla logowania).
    pub fn name(&self) -> &str {
        &self.config.name
    }

    /// Zwraca URL (dla logowania).
    pub fn url(&self) -> &str {
        &self.config.url
    }

    /// Zwraca Connection dla zaawansowanych operacji (np. listen na callbacks).
    pub fn connection(&self) -> Arc<Mutex<Option<Connection>>> {
        self.connection.clone()
    }

    /// Wysyla PrefixCacheInitRequest do LLM Engine.
    ///
    /// Uzywane po polaczeniu do zainicjalizowania KV cache dla promptow systemowych.
    pub async fn send_prefix_cache_init(
        &self,
        mut init_request: PrefixCacheInitRequest,
    ) -> Result<PrefixCacheInitResponse, CoreError> {
        debug!(
            "QuicClient.send_prefix_cache_init: Wysylam {} promptow dla modelu {}",
            init_request.prompts.len(),
            init_request.model_name
        );

        let model_request = ModelRequest {
            request_id: std::mem::take(&mut init_request.request_id),
            payload: ModelPayload::PrefixCacheInit(init_request),
            stream: false,
            metadata: None,
            session_id: None,
        };

        let response = self.send_request(model_request).await?;

        match response.result {
            ModelResult::PrefixCacheInit(init_response) => {
                debug!(
                    "QuicClient.send_prefix_cache_init: Sukces - {} promptow zacheowanych",
                    init_response.cached_count
                );
                Ok(init_response)
            }
            ModelResult::Error(error_info) => {
                error!(
                    "QuicClient.send_prefix_cache_init: Blad - {}",
                    error_info.message
                );
                Err(CoreError::InternalError {
                    message: format!("PrefixCache init failed: {}", error_info.message),
                    source: None,
                })
            }
            other => {
                error!(
                    "QuicClient.send_prefix_cache_init: Nieoczekiwana odpowiedz: {:?}",
                    std::mem::discriminant(&other)
                );
                Err(CoreError::InternalError {
                    message: "Unexpected response type for PrefixCacheInit".to_string(),
                    source: None,
                })
            }
        }
    }

    /// Wysyla ModelRequest i zwraca Stream<ModelStreamChunk>.
    ///
    /// Dla requestow z `stream: true`, serwer wysyla wiele ModelStreamChunk
    /// zamiast pojedynczej ModelResponse. Ta metoda zwraca async Stream
    /// ktory emituje kolejne chunki.
    ///
    /// Format: [4 bajty: u32 big-endian rozmiar][N bajtow: rkyv data]
    pub async fn send_request_stream(
        &self,
        request: ModelRequest,
    ) -> Result<std::pin::Pin<Box<dyn futures::Stream<Item = Result<ModelStreamChunk, CoreError>> + Send>>, CoreError> {
        debug!("QuicClient.send_request_stream: START request_id={}", request.request_id);

        let request_bytes = self.serialize_request(&request)?;

        debug!("QuicClient.send_request_stream: zserializowano {} bajtow", request_bytes.len());

        // Pobierz connection
        let conn_guard = self.connection.lock().await;
        let conn = conn_guard.as_ref().ok_or_else(|| CoreError::NetworkError {
            message: format!("Brak polaczenia QUIC z: {}", self.config.name),
            source: anyhow::anyhow!("Connection is None (reconnecting?)"),
        })?.clone();
        drop(conn_guard);

        // Otworz bi-directional stream
        let (mut send, recv) = conn
            .open_bi()
            .await
            .map_err(|e| CoreError::NetworkError {
                message: format!("Nie mozna otworzyc QUIC stream: {}", e),
                source: e.into(),
            })?;

        // Wyslij request
        send.write_all(&request_bytes)
            .await
            .map_err(|e| CoreError::NetworkError {
                message: format!("Blad wysylania request: {}", e),
                source: e.into(),
            })?;

        send.finish()
            .map_err(|e| CoreError::NetworkError {
                message: format!("Blad konczenia stream: {}", e),
                source: e.into(),
            })?;

        debug!("QuicClient.send_request_stream: request wyslany, czekam na streaming response...");

        // Utworz async stream ktory czyta chunki
        const MAX_CHUNK_SIZE: usize = 10 * 1024 * 1024; // 10 MB
        let stream = futures::stream::unfold((recv, Vec::new()), |(mut recv, mut chunk_buf)| async move {
            // Czytaj rozmiar chunka (4 bajty, big-endian)
            let mut size_buf = [0u8; 4];
            match recv.read_exact(&mut size_buf).await {
                Ok(_) => {
                    let chunk_size = u32::from_be_bytes(size_buf) as usize;

                    if chunk_size == 0 {
                        // Pusty chunk — ignoruj i kontynuuj
                        debug!("QuicClient stream: otrzymano pusty chunk, kontynuuje");
                        return Some((Ok(ModelStreamChunk {
                            request_id: String::new(),
                            chunk: StreamChunkType::Metadata(ModelMetadata {
                                model_type: "skip".to_string(),
                                model_name: String::new(),
                                details: vec![],
                            }),
                        }), (recv, chunk_buf)));
                    }

                    if chunk_size > MAX_CHUNK_SIZE {
                        error!("QuicClient stream: chunk_size {} przekracza limit {} bajtow", chunk_size, MAX_CHUNK_SIZE);
                        return Some((Err(CoreError::InternalError {
                            message: format!("Rozmiar chunk {} przekracza limit {} bajtow", chunk_size, MAX_CHUNK_SIZE),
                            source: None,
                        }), (recv, chunk_buf)));
                    }

                    // Reuse bufora — resize zamiast nowej alokacji
                    chunk_buf.resize(chunk_size, 0);
                    match recv.read_exact(&mut chunk_buf).await {
                        Ok(_) => {
                            // Deserializuj ModelStreamChunk
                            use tentaflow_protocol::ArchivedModelStreamChunk;
                            match rkyv::access::<ArchivedModelStreamChunk, rkyv::rancor::Error>(&chunk_buf) {
                                Ok(archived) => {
                                    match rkyv::deserialize::<ModelStreamChunk, rkyv::rancor::Error>(archived) {
                                        Ok(chunk) => {
                                            debug!("QuicClient stream: received chunk type {:?}", std::mem::discriminant(&chunk.chunk));
                                            Some((Ok(chunk), (recv, chunk_buf)))
                                        }
                                        Err(e) => {
                                            error!("QuicClient stream: deserialize error: {}", e);
                                            Some((Err(CoreError::InternalError {
                                                message: format!("rkyv deserialize error: {}", e),
                                                source: Some(e.into()),
                                            }), (recv, chunk_buf)))
                                        }
                                    }
                                }
                                Err(e) => {
                                    error!("QuicClient stream: access error: {}", e);
                                    Some((Err(CoreError::InternalError {
                                        message: format!("rkyv access error: {}", e),
                                        source: Some(e.into()),
                                    }), (recv, chunk_buf)))
                                }
                            }
                        }
                        Err(e) => {
                            if matches!(e, quinn::ReadExactError::FinishedEarly(_)) {
                                debug!("QuicClient stream: koniec streamu (FinishedEarly)");
                                None
                            } else {
                                error!("QuicClient stream: read chunk error: {}", e);
                                Some((Err(CoreError::NetworkError {
                                    message: format!("Blad czytania chunk: {}", e),
                                    source: e.into(),
                                }), (recv, chunk_buf)))
                            }
                        }
                    }
                }
                Err(e) => {
                    if matches!(e, quinn::ReadExactError::FinishedEarly(_)) {
                        debug!("QuicClient stream: koniec streamu (FinishedEarly na size)");
                        None
                    } else {
                        error!("QuicClient stream: read size error: {}", e);
                        Some((Err(CoreError::NetworkError {
                            message: format!("Blad czytania rozmiaru chunk: {}", e),
                            source: e.into(),
                        }), (recv, chunk_buf)))
                    }
                }
            }
        });

        Ok(Box::pin(stream))
    }
}
