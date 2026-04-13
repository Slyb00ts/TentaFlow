// =============================================================================
// Plik: quic/server.rs
// Opis: QuicServer — nasluchuje QUIC na konfigurowalnym porcie, akceptuje
//       polaczenia od routera, dispatchuje kazdy bidirectional stream do Handler.
//
//       Mechanizmy zycia polaczenia:
//       - keep_alive_interval (10s) — Quinn sam wysyla PING, peer widzi aktywnosc
//       - max_idle_timeout (30s) — polaczenie uznane za martwe gdy brak jakiekolwiek
//         aktywnosci (PING tez sie liczy)
//       - connection.closed().await — future konczy sie z ConnectionError opisujacym
//         powod (peer close, idle timeout, network error)
//       - Shutdown przez watch channel → endpoint.close() → wszystkie peery dostaja
//         CONNECTION_CLOSE z naszym CloseCode::Shutdown
// =============================================================================

use anyhow::{Context, Result};
use quinn::{Endpoint, ServerConfig, TransportConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tentaflow_protocol::{ModelRequest, ModelResponse};
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

use super::handler::{HandleOutcome, Handler, HandlerError};
use super::protocol::{read_frame, write_frame, CloseCode};

pub struct QuicServerConfig {
    pub bind: SocketAddr,
    /// Keep-alive (sidecar → router). Rowniez router powinien miec swoje.
    pub keep_alive_interval: Duration,
    /// Idle timeout — polaczenie zamykane gdy brak aktywnosci przez ten czas.
    pub max_idle_timeout: Duration,
    /// Max rownoleglych bidirectional streamow na polaczenie.
    pub max_concurrent_bi_streams: u32,
    /// Certyfikat TLS (PEM) — self-signed gdy None.
    pub tls_cert_pem: Option<Vec<u8>>,
    pub tls_key_pem: Option<Vec<u8>>,
}

impl Default for QuicServerConfig {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:5000".parse().unwrap(),
            keep_alive_interval: Duration::from_secs(10),
            max_idle_timeout: Duration::from_secs(30),
            max_concurrent_bi_streams: 100,
            tls_cert_pem: None,
            tls_key_pem: None,
        }
    }
}

pub struct QuicServer<H: Handler> {
    config: QuicServerConfig,
    handler: Arc<H>,
}

impl<H: Handler> QuicServer<H> {
    pub fn new(config: QuicServerConfig, handler: H) -> Self {
        Self {
            config,
            handler: Arc::new(handler),
        }
    }

    /// Uruchamia QUIC endpoint i blokuje az do otrzymania shutdown przez `shutdown_rx`
    /// albo krytycznego bledu. Shutdown -> zamyka wszystkie aktywne polaczenia
    /// z CloseCode::Shutdown i czeka az endpoint dokonczy wyslanie CONNECTION_CLOSE.
    pub async fn run(self, mut shutdown_rx: watch::Receiver<bool>) -> Result<()> {
        let server_config = build_server_config(&self.config)?;
        let endpoint = Endpoint::server(server_config, self.config.bind)
            .with_context(|| format!("nie udalo sie zbindowac QUIC na {}", self.config.bind))?;

        info!(bind = %self.config.bind, "QUIC server nasluchuje");

        let handler = self.handler.clone();
        let endpoint_accept = endpoint.clone();

        loop {
            tokio::select! {
                biased;

                // Shutdown — wyjdz z petli i zamknij endpoint
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        info!("Shutdown: zamykam endpoint QUIC");
                        endpoint.close(CloseCode::Shutdown.code(), CloseCode::Shutdown.reason());
                        endpoint.wait_idle().await;
                        info!("Endpoint QUIC zamkniety czysto");
                        return Ok(());
                    }
                }

                incoming = endpoint_accept.accept() => {
                    let Some(incoming) = incoming else {
                        warn!("Endpoint::accept zwrocil None — kontynuuje petle");
                        continue;
                    };
                    let handler = handler.clone();
                    let shutdown_rx = shutdown_rx.clone();
                    tokio::spawn(async move {
                        match incoming.await {
                            Ok(conn) => {
                                let remote = conn.remote_address();
                                info!(peer = %remote, "Peer polaczony");
                                if let Err(e) = handle_connection(conn, handler, shutdown_rx).await {
                                    warn!(peer = %remote, "Obsluga polaczenia zakonczona z bledem: {}", e);
                                }
                            }
                            Err(e) => {
                                warn!("Handshake nieudany: {}", e);
                            }
                        }
                    });
                }
            }
        }
    }
}

/// Obsluguje zywe polaczenie — akceptuje bidirectional streamy, na kazdym
/// spawnuje handler. Petla konczy sie gdy peer zamknie polaczenie, idle timeout
/// wystrzeli, albo dostaniemy lokalny shutdown.
async fn handle_connection<H: Handler>(
    conn: quinn::Connection,
    handler: Arc<H>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    let remote = conn.remote_address();

    loop {
        tokio::select! {
            biased;

            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    info!(peer = %remote, "Shutdown: zamykam polaczenie");
                    conn.close(CloseCode::Shutdown.code(), CloseCode::Shutdown.reason());
                    return Ok(());
                }
            }

            // closed() konczy sie gdy peer zamknie albo idle timeout
            close_reason = conn.closed() => {
                info!(peer = %remote, reason = %close_reason, "Peer rozlaczyl sie");
                return Ok(());
            }

            stream = conn.accept_bi() => {
                match stream {
                    Ok((send, recv)) => {
                        let handler = handler.clone();
                        tokio::spawn(async move {
                            if let Err(e) = serve_stream(send, recv, handler).await {
                                debug!(peer = %remote, "Stream zakonczony z bledem: {}", e);
                            }
                        });
                    }
                    Err(quinn::ConnectionError::ApplicationClosed(ac)) => {
                        info!(peer = %remote, code = %ac.error_code, "Peer wyslal ApplicationClose");
                        return Ok(());
                    }
                    Err(quinn::ConnectionError::ConnectionClosed(cc)) => {
                        info!(peer = %remote, code = %cc.error_code, "Peer wyslal ConnectionClose");
                        return Ok(());
                    }
                    Err(quinn::ConnectionError::TimedOut) => {
                        warn!(peer = %remote, "Idle timeout — peer prawdopodobnie umarl");
                        return Ok(());
                    }
                    Err(e) => {
                        warn!(peer = %remote, "accept_bi blad: {}", e);
                        return Err(e.into());
                    }
                }
            }
        }
    }
}

/// Obsluguje pojedynczy bidirectional stream: odczytaj ModelRequest, wywolaj
/// handler, zapisz odpowiedz (unary lub stream) na send stream.
async fn serve_stream<H: Handler>(
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    handler: Arc<H>,
) -> Result<()> {
    let request: ModelRequest = match read_frame(&mut recv).await? {
        Some(r) => r,
        None => {
            debug!("Pusty stream — peer zamknal bez wyslania request");
            return Ok(());
        }
    };

    let request_id = request.request_id.clone();
    debug!(request_id = %request_id, "Odebrano ModelRequest");

    match handler.handle(request).await {
        Ok(HandleOutcome::Unary(response)) => {
            write_frame(&mut send, &response).await?;
            send.finish().ok();
        }
        Ok(HandleOutcome::Stream(mut rx)) => {
            while let Some(chunk) = rx.recv().await {
                write_frame(&mut send, &chunk).await?;
            }
            send.finish().ok();
        }
        Err(err) => {
            warn!(request_id = %request_id, "Handler blad: {}", err);
            let error_resp = make_error_response(&request_id, &err.to_string());
            // best-effort — peer moze juz byc rozlaczony
            let _ = write_frame(&mut send, &error_resp).await;
            send.finish().ok();
        }
    }

    Ok(())
}

fn make_error_response(request_id: &str, message: &str) -> ModelResponse {
    use tentaflow_protocol::{ErrorInfo, ErrorType, ModelResult};
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

fn build_server_config(cfg: &QuicServerConfig) -> Result<ServerConfig> {
    let (cert_der, key_der) = match (&cfg.tls_cert_pem, &cfg.tls_key_pem) {
        (Some(cert_pem), Some(key_pem)) => {
            let certs = rustls_pemfile::certs(&mut cert_pem.as_slice())
                .collect::<Result<Vec<_>, _>>()
                .context("parsowanie certyfikatu PEM")?;
            let key = rustls_pemfile::private_key(&mut key_pem.as_slice())
                .context("parsowanie klucza PEM")?
                .ok_or_else(|| anyhow::anyhow!("brak klucza w PEM"))?;
            let cert = certs.into_iter().next()
                .ok_or_else(|| anyhow::anyhow!("brak certyfikatu w PEM"))?;
            (cert, key)
        }
        _ => {
            info!("Brak cert/key — generuje self-signed dla localhost/0.0.0.0");
            let mut params = rcgen::CertificateParams::new(vec![
                "localhost".into(),
                "127.0.0.1".into(),
            ])?;
            params
                .distinguished_name
                .push(rcgen::DnType::CommonName, "tentaflow-sidecar");
            let key_pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
            let cert = params.self_signed(&key_pair)?;
            let cert_der = CertificateDer::from(cert.der().to_vec());
            let key_der = PrivateKeyDer::try_from(key_pair.serialize_der())
                .map_err(|e| anyhow::anyhow!("konwersja klucza DER: {:?}", e))?;
            (cert_der, key_der)
        }
    };

    let mut transport = TransportConfig::default();
    transport.keep_alive_interval(Some(cfg.keep_alive_interval));
    transport.max_idle_timeout(Some(
        cfg.max_idle_timeout
            .try_into()
            .context("max_idle_timeout poza zakresem VarInt")?,
    ));
    transport.max_concurrent_bidi_streams(cfg.max_concurrent_bi_streams.into());
    transport.max_concurrent_uni_streams(0u32.into());

    let mut server_config = ServerConfig::with_single_cert(vec![cert_der], key_der)?;
    server_config.transport = Arc::new(transport);

    Ok(server_config)
}

// Unused w server.rs, re-export dla testow
#[allow(dead_code)]
pub(crate) use super::protocol::MAX_FRAME_SIZE;
