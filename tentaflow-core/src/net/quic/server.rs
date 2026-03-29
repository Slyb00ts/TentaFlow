// =============================================================================
// Plik: net/quic/server.rs
// Opis: Serwer QUIC przyjmujacy polaczenia od klientow zewnetrznych (.NET, etc.).
//       Przekazuje requesty do Router i zwraca odpowiedzi przez
//       protokol rkyv (zero-copy serialization).
// Przyklad:
//   let server = QuicServer::new(config, router);
//   server.run().await?;
// =============================================================================

use crate::net::quic::tls;
use anyhow::{Context, Result};
use quinn::{Endpoint, ServerConfig as QuinnServerConfig};
use tentaflow_protocol::*;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

// ============================================================================
// Tymczasowe placeholder typy — do usuniecia po przeniesieniu modulow
// ============================================================================

// TODO: wymaga przeniesienia crate::config::QuicProtocolConfig
/// Konfiguracja serwera QUIC (tymczasowy placeholder).
#[derive(Debug, Clone)]
pub struct QuicProtocolConfig {
    pub enabled: bool,
    pub bind: String,
    pub tls_cert: Option<String>,
    pub tls_key: Option<String>,
    pub max_connections: usize,
    pub max_streams_per_connection: usize,
    pub idle_timeout_ms: u64,
}

// TODO: wymaga przeniesienia crate::routing::router::Router
/// Interfejs routera do przekazywania requestow (tymczasowy placeholder).
///
/// Docelowo zastapiony przez `crate::routing::router::Router`.
/// Definiowany jako trait aby umozliwic kompilacje zanim Router zostanie przeniesiony.
pub trait RouterHandler: Send + Sync + 'static {
    fn route_model_request(
        &self,
        request_bytes: &[u8],
        is_forwarded: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<u8>>> + Send + '_>>;
}

/// Rejestr aktywnych streaming requestow z tokenami anulowania.
type ActiveRequests = Arc<RwLock<HashMap<String, CancellationToken>>>;

/// Serwer QUIC przyjmujacy polaczenia od klientow zewnetrznych.
pub struct QuicServer<R: RouterHandler> {
    /// Konfiguracja serwera QUIC (bind address, TLS, timeouty)
    config: QuicProtocolConfig,

    /// Router do przekazywania requestow do backendow
    router: Arc<R>,

    /// Rejestr aktywnych streaming requestow z tokenami anulowania
    active_requests: ActiveRequests,
}

impl<R: RouterHandler> QuicServer<R> {
    /// Tworzy nowy serwer QUIC.
    pub fn new(config: QuicProtocolConfig, router: Arc<R>) -> Self {
        Self {
            config,
            router,
            active_requests: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Uruchamia serwer QUIC i nasluchuje na polaczenia.
    pub async fn run(&self) -> Result<()> {
        if !self.config.enabled {
            debug!("Serwer QUIC wylaczony w konfiguracji");
            return Ok(());
        }

        let server_config = self.create_server_config()?;
        let bind_addr: SocketAddr = self.config.bind.parse()
            .context("Nieprawidlowy adres bind dla serwera QUIC")?;

        let endpoint = Endpoint::server(server_config, bind_addr)?;
        debug!("Serwer QUIC nasluchuje na {}", bind_addr);

        // Graceful shutdown — czekaj na sygnal Ctrl+C
        let shutdown = async {
            tokio::signal::ctrl_c().await.ok();
            debug!("Otrzymano sygnal zamkniecia, zamykam serwer QUIC...");
        };

        tokio::pin!(shutdown);

        // Glowna petla akceptowania polaczen
        loop {
            tokio::select! {
                conn = endpoint.accept() => {
                    match conn {
                        Some(incoming) => {
                            let router = self.router.clone();
                            let active_requests = self.active_requests.clone();

                            // Spawnuj handler dla kazdego polaczenia
                            tokio::spawn(async move {
                                match incoming.await {
                                    Ok(connection) => {
                                        debug!(
                                            "Klient QUIC polaczony: {}",
                                            connection.remote_address()
                                        );
                                        Self::handle_connection(connection, router, active_requests).await;
                                    }
                                    Err(e) => {
                                        error!("Polaczenie QUIC nieudane: {}", e);
                                    }
                                }
                            });
                        }
                        None => {
                            warn!("Endpoint QUIC zamkniety");
                            break;
                        }
                    }
                }
                _ = &mut shutdown => {
                    debug!("Zamykanie endpointu QUIC...");
                    endpoint.close(0u32.into(), b"server shutdown");
                    endpoint.wait_idle().await;
                    debug!("Serwer QUIC zamkniety.");
                    break;
                }
            }
        }

        Ok(())
    }

    // Obsluguje pojedyncze polaczenie QUIC od klienta.
    async fn handle_connection(
        connection: quinn::Connection,
        router: Arc<R>,
        active_requests: ActiveRequests,
    ) {
        let remote = connection.remote_address();
        debug!("handle_connection: start dla {}", remote);

        // Petla akceptujaca strumienie az do zamkniecia polaczenia
        loop {
            debug!("handle_connection: czekam na strumien...");
            match connection.accept_bi().await {
                Ok((send, recv)) => {
                    let router = router.clone();
                    let active_requests = active_requests.clone();

                    // Spawnuj handler dla kazdego strumienia
                    tokio::spawn(async move {
                        if let Err(e) = Self::handle_stream(send, recv, router, active_requests).await {
                            error!("Blad obslugi strumienia: {}", e);
                        }
                    });
                }
                Err(e) => {
                    debug!("Polaczenie {} zamkniete: {}", remote, e);
                    break;
                }
            }
        }
    }

    // Obsluguje pojedynczy strumien bidirektionalny (request-response).
    async fn handle_stream(
        mut send: quinn::SendStream,
        mut recv: quinn::RecvStream,
        router: Arc<R>,
        active_requests: ActiveRequests,
    ) -> Result<()> {
        debug!("handle_stream: start");

        // Odczytaj request (maksymalnie 100MB dla duzych dokumentow)
        debug!("handle_stream: odczytywanie requestu...");
        let request_bytes = recv
            .read_to_end(100 * 1024 * 1024)
            .await
            .context("Nie udalo sie odczytac requestu")?;

        debug!("handle_stream: odebrano {} bajtow", request_bytes.len());

        if request_bytes.is_empty() {
            anyhow::bail!("Pusty request");
        }

        // Pierwszy bajt okresla typ wiadomosci (discriminator)
        let first_byte = request_bytes[0];
        debug!("handle_stream: pierwszy bajt = 0x{:02X}", first_byte);

        debug!("handle_stream: routing requestu...");

        if first_byte == MESSAGE_TYPE_CANCEL_REQUEST {
            debug!("handle_stream: typ = CancelRequest");
            let response_bytes = Self::handle_cancel_request(&request_bytes[1..], active_requests).await?;
            send.write_all(&response_bytes).await?;
            send.finish()?;
        } else {
            debug!("handle_stream: typ = ModelRequest");
            // Deleguj routing do RouterHandler
            let response_bytes = router.route_model_request(&request_bytes, false).await?;
            debug!("handle_stream: routing zakonczony, {} bajtow odpowiedzi", response_bytes.len());
            send.write_all(&response_bytes).await?;
            send.finish()?;
        }

        debug!("Wyslano odpowiedz");

        Ok(())
    }

    // Obsluguje CancelRequest — anulowanie aktywnego streaming requestu.
    async fn handle_cancel_request(
        request_bytes: &[u8],
        active_requests: ActiveRequests,
    ) -> Result<Vec<u8>> {
        let archived = rkyv::access::<ArchivedCancelRequest, rkyv::rancor::Error>(request_bytes)
            .context("Nie udalo sie zdeserializowac CancelRequest")?;

        let request_id = archived.request_id.to_string();
        let reason = archived.reason.as_ref().map(|r| r.to_string());

        debug!("handle_cancel_request: request_id={}, reason={:?}", request_id, reason);

        // Jeden write lock — znajdz, anuluj i usun atomowo
        let response = {
            let mut requests = active_requests.write().await;
            if let Some(token) = requests.remove(&request_id) {
                token.cancel();
                debug!("handle_cancel_request: anulowano request_id={}", request_id);

                CancelResponse {
                    request_id: request_id.clone(),
                    success: true,
                    status: CancellationStatus::Cancelled,
                    message: Some("Request cancelled successfully".to_string()),
                }
            } else {
                warn!("handle_cancel_request: nie znaleziono request_id={}", request_id);

                CancelResponse {
                    request_id: request_id.clone(),
                    success: false,
                    status: CancellationStatus::NotFound,
                    message: Some("Request not found or already completed".to_string()),
                }
            }
        };

        let response_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&response)
            .context("Nie udalo sie zserializowac CancelResponse")?;

        Ok(response_bytes.into_vec())
    }

    // Wysyla pojedynczy chunk streamu przez QUIC z length-prefix.
    //
    // Format: [4 bajty dlugosci (u32 BE)][rkyv bytes]
    pub async fn send_stream_chunk(
        send: &mut quinn::SendStream,
        chunk: &ModelStreamChunk,
    ) -> Result<()> {
        let chunk_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(chunk)
            .context("Nie udalo sie zserializowac ModelStreamChunk")?;

        // Lacz length prefix + dane w jeden bufor (jedno write_all zamiast dwoch)
        let len = chunk_bytes.len() as u32;
        let mut frame = Vec::with_capacity(4 + chunk_bytes.len());
        frame.extend_from_slice(&len.to_be_bytes());
        frame.extend_from_slice(&chunk_bytes);
        send.write_all(&frame).await?;

        Ok(())
    }

    // Tworzy odpowiedz bledu w formacie ModelResponse.
    #[allow(dead_code)]
    fn error_response(request_id: &str, error_type: ErrorType, message: &str) -> ModelResponse {
        ModelResponse {
            request_id: request_id.to_string(),
            result: ModelResult::Error(ErrorInfo {
                error_type,
                message: message.to_string(),
                details: None,
            }),
            metrics: None,
        }
    }

    // Domyslne certyfikaty wbudowane w binarie z katalogu certs/ repozytorium
    const DEFAULT_CERT_PEM: &[u8] = include_bytes!("../../../../certs/cert.pem");
    const DEFAULT_KEY_PEM: &[u8] = include_bytes!("../../../../certs/key.pem");

    // Tworzy konfiguracje TLS dla serwera QUIC.
    // Jesli config podaje sciezki do certyfikatow — uzywa ich.
    // W przeciwnym razie uzywa certyfikatow wbudowanych w binarie.
    fn create_server_config(&self) -> Result<QuinnServerConfig> {
        let (certs, key) = if let (Some(cert_path), Some(key_path)) =
            (self.config.tls_cert.as_ref(), self.config.tls_key.as_ref())
        {
            (tls::load_certs(cert_path)?, tls::load_private_key(key_path)?)
        } else {
            info!("Uzycie wbudowanych certyfikatow TLS z certs/");
            let certs = tls::parse_certs_pem(Self::DEFAULT_CERT_PEM)?;
            let key = tls::parse_key_pem(Self::DEFAULT_KEY_PEM)?;
            (certs, key)
        };

        // Skonfiguruj TLS bez uwierzytelniania klienta
        let mut server_crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .context("Nie udalo sie skonfigurowac TLS")?;

        // ALPN protocol — klienci AI (router)
        server_crypto.alpn_protocols = vec![
            b"tentaflow".to_vec(),
        ];

        let mut server_config = QuinnServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)?
        ));

        // Konfiguracja transportu
        let mut transport = quinn::TransportConfig::default();

        // Maksymalna liczba rownoczesnych strumieni bidirektionalnych
        transport.max_concurrent_bidi_streams((self.config.max_streams_per_connection as u32).into());

        // Timeout bezczynnosci — zamknij polaczenie po braku aktywnosci
        transport.max_idle_timeout(Some(
            std::time::Duration::from_millis(self.config.idle_timeout_ms)
                .try_into()
                .context("Nieprawidlowa wartosc idle_timeout_ms w konfiguracji QUIC")?
        ));

        // Keepalive — wysylaj pakiety co 15 sekund aby utrzymac polaczenie
        transport.keep_alive_interval(Some(std::time::Duration::from_secs(15)));

        server_config.transport_config(Arc::new(transport));

        Ok(server_config)
    }
}
