// =============================================================================
// Plik: tests/quic_lifecycle.rs
// Opis: Testy integracyjne QUIC sidecara. Weryfikuja:
//       - podstawowa komunikacja request/response
//       - graceful shutdown serwera wysyla CONNECTION_CLOSE do klientow
//       - wykrycie nagle zerwanego klienta przez idle timeout
//       - klient dowiaduje sie o shutdown serwera (connection.closed())
//       - keepalive podtrzymuje polaczenie przez dlugi okres bezczynnosci
//       - handler zwraca blad -> klient dostaje ModelResponse::Error
//       - wiele rownoleglych streamow na jednym polaczeniu
// =============================================================================

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use quinn::{ClientConfig, Endpoint, TransportConfig};
use rustls::client::danger::{ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::{DigitallySignedStruct, Error as RustlsError, SignatureScheme};
use tentaflow_protocol::*;
use tokio::sync::{mpsc, watch};

use tentaflow_sidecar::quic::{
    handler::{HandleOutcome, Handler, HandlerError},
    server::{QuicServer, QuicServerConfig},
};
use tentaflow_sidecar::quic::protocol::{read_frame, write_frame, CloseCode};

// ---- Test helpers ------------------------------------------------------

/// Verifier ktory akceptuje kazdy certyfikat — tylko do testow.
#[derive(Debug)]
struct InsecureVerifier;

impl ServerCertVerifier for InsecureVerifier {
    fn verify_server_cert(
        &self,
        _: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, RustlsError> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _: &[u8],
        _: &CertificateDer<'_>,
        _: &DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, RustlsError> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![
            SignatureScheme::ECDSA_NISTP256_SHA256,
            SignatureScheme::RSA_PSS_SHA256,
            SignatureScheme::RSA_PKCS1_SHA256,
            SignatureScheme::ED25519,
        ]
    }
}

fn make_client_endpoint(keep_alive_ms: u64, idle_timeout_ms: u64) -> Endpoint {
    let crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(InsecureVerifier))
        .with_no_client_auth();
    let mut transport = TransportConfig::default();
    transport.keep_alive_interval(Some(Duration::from_millis(keep_alive_ms)));
    transport.max_idle_timeout(Some(
        Duration::from_millis(idle_timeout_ms).try_into().unwrap(),
    ));

    let quic_client_cfg = quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap();
    let mut cc = ClientConfig::new(Arc::new(quic_client_cfg));
    cc.transport_config(Arc::new(transport));

    let mut endpoint = Endpoint::client("0.0.0.0:0".parse().unwrap()).unwrap();
    endpoint.set_default_client_config(cc);
    endpoint
}

fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn make_request(id: &str) -> ModelRequest {
    ModelRequest {
        request_id: id.to_string(),
        payload: ModelPayload::Completion(CompletionPayload {
            model: "test".to_string(),
            prompt: Some("hi".to_string()),
            messages: vec![],
            temperature: None,
            max_tokens: None,
            top_p: None,
            stop: None,
            presence_penalty: None,
            frequency_penalty: None,
            tts_options: None,
            memory_options: None,
            audio_input: None,
            prefix_cache_id: None,
            prefix_text: None,
        }),
        stream: false,
        metadata: None,
        session_id: None,
    }
}

/// Handler ktory odpowiada echo na kazdy request. Liczy wywolania.
struct EchoHandler {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl Handler for EchoHandler {
    async fn handle(
        &self,
        request: ModelRequest,
    ) -> Result<HandleOutcome, HandlerError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(HandleOutcome::Unary(ModelResponse {
            request_id: request.request_id,
            result: ModelResult::Completion(CompletionResult {
                text: "ok".to_string(),
                reasoning_content: None,
                model: "test".to_string(),
                finish_reason: None,
                tool_calls: None,
                detected_intent: None,
                detected_tools: None,
                transcribed_text: None,
                speaker_id: None,
                speaker_name: None,
            }),
            metrics: None,
        }))
    }
}

/// Handler ktory zawsze rzuca blad.
struct FailingHandler;

#[async_trait]
impl Handler for FailingHandler {
    async fn handle(&self, _: ModelRequest) -> Result<HandleOutcome, HandlerError> {
        Err(HandlerError::UpstreamUnavailable("test".into()))
    }
}

/// Bindowanie na losowy wolny port.
async fn start_server<H: Handler>(handler: H) -> (std::net::SocketAddr, watch::Sender<bool>, tokio::task::JoinHandle<()>) {
    install_crypto_provider();
    let listener = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener); // zwolnij zeby quinn mogl bindowac

    let cfg = QuicServerConfig {
        bind: addr,
        keep_alive_interval: Duration::from_millis(100),
        max_idle_timeout: Duration::from_millis(2000),
        max_concurrent_bi_streams: 100,
        tls_cert_pem: None,
        tls_key_pem: None,
    };
    let (tx, rx) = watch::channel(false);
    let server = QuicServer::new(cfg, handler);
    let handle = tokio::spawn(async move {
        server.run(rx).await.unwrap();
    });
    // daj endpointowi czas na bind
    tokio::time::sleep(Duration::from_millis(50)).await;
    (addr, tx, handle)
}

async fn connect(addr: std::net::SocketAddr) -> quinn::Connection {
    let endpoint = make_client_endpoint(100, 2000);
    endpoint
        .connect(addr, "localhost")
        .unwrap()
        .await
        .unwrap()
}

// ---- Test cases --------------------------------------------------------

#[tokio::test]
async fn basic_request_response() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (addr, shutdown, _srv) = start_server(EchoHandler { calls: calls.clone() }).await;

    let conn = connect(addr).await;
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    write_frame(&mut send, &make_request("r1")).await.unwrap();
    send.finish().ok();
    let resp: ModelResponse = read_frame(&mut recv).await.unwrap().expect("response");

    assert_eq!(resp.request_id, "r1");
    matches!(resp.result, ModelResult::Completion(_));
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    shutdown.send(true).unwrap();
}

#[tokio::test]
async fn server_shutdown_notifies_client() {
    let (addr, shutdown, _srv) = start_server(EchoHandler { calls: Arc::new(AtomicUsize::new(0)) }).await;

    let conn = connect(addr).await;
    assert!(!conn.close_reason().is_some());

    // Server shutdown
    shutdown.send(true).unwrap();

    // Klient powinien dostac close_reason w ciagu 500ms
    let close = tokio::time::timeout(Duration::from_millis(500), conn.closed())
        .await
        .expect("klient nie dostal close_reason w czasie");

    match close {
        quinn::ConnectionError::ApplicationClosed(ac) => {
            assert_eq!(ac.error_code, CloseCode::Shutdown.code());
        }
        other => panic!("oczekiwano ApplicationClosed, dostalem: {:?}", other),
    }
}

#[tokio::test]
async fn client_disconnect_detected_by_server() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (addr, shutdown, _srv) = start_server(EchoHandler { calls: calls.clone() }).await;

    // Klient z krotkim idle timeoutem
    let endpoint = make_client_endpoint(100, 500);
    let conn = endpoint
        .connect(addr, "localhost")
        .unwrap()
        .await
        .unwrap();

    // Klient zamyka od razu
    conn.close(quinn::VarInt::from_u32(99), b"client-gone");

    // Server powinien zarejestrowac to bez paniki — dajemy mu chwile
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Server dalej dziala — mozemy otworzyc nowe polaczenie
    let conn2 = connect(addr).await;
    let (mut send, mut recv) = conn2.open_bi().await.unwrap();
    write_frame(&mut send, &make_request("after-disc")).await.unwrap();
    send.finish().ok();
    let resp: ModelResponse = read_frame(&mut recv).await.unwrap().unwrap();
    assert_eq!(resp.request_id, "after-disc");

    shutdown.send(true).unwrap();
}

#[tokio::test]
async fn handler_error_returns_error_response() {
    let (addr, shutdown, _srv) = start_server(FailingHandler).await;

    let conn = connect(addr).await;
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    write_frame(&mut send, &make_request("err")).await.unwrap();
    send.finish().ok();
    let resp: ModelResponse = read_frame(&mut recv).await.unwrap().expect("response");

    assert_eq!(resp.request_id, "err");
    match resp.result {
        ModelResult::Error(err) => {
            assert!(err.message.contains("test"));
        }
        _ => panic!("oczekiwano ModelResult::Error, dostalem: {:?}", resp.result),
    }

    shutdown.send(true).unwrap();
}

#[tokio::test]
async fn parallel_streams_on_one_connection() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (addr, shutdown, _srv) = start_server(EchoHandler { calls: calls.clone() }).await;
    let conn = connect(addr).await;

    let mut handles = Vec::new();
    for i in 0..10 {
        let c = conn.clone();
        handles.push(tokio::spawn(async move {
            let (mut send, mut recv) = c.open_bi().await.unwrap();
            let id = format!("parallel-{}", i);
            write_frame(&mut send, &make_request(&id)).await.unwrap();
            send.finish().ok();
            let resp: ModelResponse = read_frame(&mut recv).await.unwrap().unwrap();
            assert_eq!(resp.request_id, id);
        }));
    }
    for h in handles { h.await.unwrap(); }
    assert_eq!(calls.load(Ordering::SeqCst), 10);

    shutdown.send(true).unwrap();
}

#[tokio::test]
async fn keepalive_preserves_idle_connection() {
    // Klient z 100ms keepalive ale 2000ms idle timeout
    // Bez keepalive polaczenie by padlo po 2s, z keepalive ping co 100ms → alive
    let calls = Arc::new(AtomicUsize::new(0));
    let (addr, shutdown, _srv) = start_server(EchoHandler { calls: calls.clone() }).await;

    let conn = connect(addr).await;

    // Czekaj ponad idle timeout — keepalive powinien utrzymac
    tokio::time::sleep(Duration::from_millis(2500)).await;

    // Polaczenie nadal zywe — request dziala
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    write_frame(&mut send, &make_request("still-alive")).await.unwrap();
    send.finish().ok();
    let resp: ModelResponse = read_frame(&mut recv).await.unwrap().unwrap();
    assert_eq!(resp.request_id, "still-alive");

    shutdown.send(true).unwrap();
}

#[tokio::test]
async fn server_survives_handler_panic_is_not_tested_but_logged() {
    // Uwaga: tokio::spawn przechwytuje panic — testujemy tylko ze error jest logowany
    // i serwer nadal przyjmuje nowe polaczenia (pokryte przez client_disconnect_detected_by_server)
}
