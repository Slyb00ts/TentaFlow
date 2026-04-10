// =============================================================================
// Plik: api/unified_server.rs
// Opis: Zunifikowany serwer HTTPS obslugujacy jednoczesnie OpenAI API i Dashboard
//       na jednym porcie. Uzywa wbudowanych certyfikatow TLS. Wspoldzielony
//       miedzy Router.New, Desktop i Mobile.
// =============================================================================

use std::sync::Arc;

use anyhow::Result;
use http_body_util::BodyExt;
use http_body_util::combinators::UnsyncBoxBody;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::Request;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, error, info};

use crate::config::NodeConfig;
use crate::crypto::{generate_master_key, SecretsCipher, SettingsCipher};
use crate::db;
use crate::mesh::peer_store::MeshPeerStore;
use crate::mesh::quic_mesh::QuicMeshManager;
use crate::mesh::security::MeshSecurity;
use crate::metrics::RouterMetrics;
use crate::routing::Router;
use crate::routing::service_manager::ServiceManager;

/// Sprawdza czy request powinien byc obsluzony przez OpenAI API handler
pub fn is_openai_path(path: &str) -> bool {
    path.starts_with("/v1/")
        || path == "/health"
        || path == "/ready"
        || path == "/metrics"
}

/// Uruchamia zunifikowany serwer HTTPS obslugujacy OpenAI API + Dashboard
/// na jednym porcie. Serwer dziala w tle jako tokio task.
///
/// Parametry:
/// - `config` — konfiguracja node'a (bind address, wlaczenie API)
/// - `db` — pula polaczen SQLite
/// - `metrics` — wspoldzielone metryki routera
/// - `router` — router z logiką routingu
/// - `mesh_peer_store` — store peerow mesh
/// - `quic_mesh` — opcjonalny menedzer QUIC mesh
/// - `local_node_id` — identyfikator lokalnego node'a
pub fn start_unified_server(
    config: &NodeConfig,
    db: &db::DbPool,
    metrics: &Arc<RouterMetrics>,
    router: &Arc<Router>,
    mesh_peer_store: &MeshPeerStore,
    quic_mesh: Option<Arc<QuicMeshManager>>,
    local_node_id: Arc<str>,
    mesh_security: Option<Arc<MeshSecurity>>,
) -> Result<()> {
    start_unified_server_with_permissions(config, db, metrics, router, mesh_peer_store, quic_mesh, local_node_id, mesh_security, None)
}

/// Zunifikowany serwer z opcjonalnym PermissionChecker do natychmiastowej invalidacji cache
pub fn start_unified_server_with_permissions(
    config: &NodeConfig,
    db: &db::DbPool,
    metrics: &Arc<RouterMetrics>,
    router: &Arc<Router>,
    mesh_peer_store: &MeshPeerStore,
    quic_mesh: Option<Arc<QuicMeshManager>>,
    local_node_id: Arc<str>,
    mesh_security: Option<Arc<MeshSecurity>>,
    permission_checker: Option<Arc<crate::addon::permissions::PermissionChecker>>,
) -> Result<()> {
    if !config.protocols.openai_api.enabled {
        info!("Unified HTTP server wylaczony w konfiguracji");
        return Ok(());
    }

    let bind_addr = config.protocols.openai_api.bind.clone();

    // Ladowanie master key z pliku na dysku i inicjalizacja SettingsCipher
    let file_master_key = crate::crypto::load_or_create_master_key()
        .expect("Nie udalo sie zaladowac master key z pliku");
    let settings_cipher = Arc::new(SettingsCipher::new(&file_master_key));

    // Migracja istniejacych plaintextowych sekretow
    match crate::crypto::migrate_plaintext_secrets(db, &settings_cipher) {
        Ok(n) if n > 0 => info!("Zaszyfrowano {} plaintextowych sekretow w bazie", n),
        Err(e) => error!("Blad migracji sekretow: {}", e),
        _ => {}
    }

    // SecretsCipher (dla addonow) — encryption_master_key z bazy odszyfrowany przez SettingsCipher
    let master_key = db::repository::get_setting_secure(db, "encryption_master_key", &settings_cipher)
        .ok()
        .flatten()
        .unwrap_or_else(|| {
            let key = generate_master_key();
            let _ = db::repository::set_setting_secure(db, "encryption_master_key", &key, &settings_cipher);
            info!("Wygenerowano nowy encryption_master_key i zapisano w bazie");
            key
        });

    let cipher = Arc::new(
        SecretsCipher::new(&master_key).expect("Nieprawidlowy encryption_master_key w bazie"),
    );

    let router = router.clone();
    let db = db.clone();
    let metrics = metrics.clone();
    let service_manager: Arc<ServiceManager> = router.service_manager().clone();
    let mesh_peer_store = mesh_peer_store.clone();
    let quic_mesh = quic_mesh.clone();
    let local_node_id = local_node_id.clone();
    let mesh_security = mesh_security.clone();
    let permission_checker = permission_checker.clone();

    // Wbudowane certyfikaty TLS z katalogu certs/ repozytorium
    let tls_acceptor = {
        let cert_pem = include_bytes!("../../../certs/cert.pem");
        let key_pem = include_bytes!("../../../certs/key.pem");

        let certs = crate::net::quic::tls::parse_certs_pem(cert_pem)
            .expect("Nie udalo sie sparsowac wbudowanego certyfikatu");
        let key = crate::net::quic::tls::parse_key_pem(key_pem)
            .expect("Nie udalo sie sparsowac wbudowanego klucza");

        let mut tls_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .expect("Nie udalo sie skonfigurowac TLS");

        tls_config.alpn_protocols = vec![b"http/1.1".to_vec(), b"h2".to_vec()];

        TlsAcceptor::from(Arc::new(tls_config))
    };

    info!("Inicjalizacja unified HTTPS server na {}...", bind_addr);

    tokio::spawn(async move {
        let listener = match TcpListener::bind(&bind_addr).await {
            Ok(l) => l,
            Err(e) => {
                error!("Nie mozna zbindowac na {}: {}", bind_addr, e);
                return;
            }
        };

        info!("Unified HTTPS server nasluchuje na {} (OpenAI API + Dashboard)", bind_addr);

        loop {
            let (stream, remote_addr) = match listener.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    error!("Blad akceptowania polaczenia: {}", e);
                    continue;
                }
            };

            let tls_acceptor = tls_acceptor.clone();
            let router = router.clone();
            let db = db.clone();
            let metrics = metrics.clone();
            let cipher = cipher.clone();
            let sc = settings_cipher.clone();
            let sm = service_manager.clone();
            let mps = mesh_peer_store.clone();
            let qm = quic_mesh.clone();
            let lni = local_node_id.clone();
            let msec = mesh_security.clone();
            let pc = permission_checker.clone();

            tokio::spawn(async move {
                // TLS handshake
                let tls_stream = match tls_acceptor.accept(stream).await {
                    Ok(s) => s,
                    Err(e) => {
                        // Klient probowal HTTP bez TLS lub przerwano handshake
                        debug!("TLS handshake nieudany od {}: {}", remote_addr, e);
                        return;
                    }
                };
                let io = TokioIo::new(tls_stream);

                // VULN-035: Przekaz remote_addr do handle_request
                let remote_addr_str = remote_addr.to_string();
                let service = service_fn(move |req: Request<Incoming>| {
                    let router = router.clone();
                    let db = db.clone();
                    let metrics = metrics.clone();
                    let cipher = cipher.clone();
                    let sc = sc.clone();
                    let sm = sm.clone();
                    let mps = mps.clone();
                    let qm = qm.clone();
                    let lni = lni.clone();
                    let msec = msec.clone();
                    let pc = pc.clone();
                    let ra = remote_addr_str.clone();
                    async move {
                        let path = req.uri().path().to_string();

                        if is_openai_path(&path) {
                            // VULN-001: Sprawdz API key dla sciezek OpenAI (oprocz /health i /ready)
                            if path != "/health" && path != "/ready" {
                                let api_key = req.headers().get("authorization")
                                    .and_then(|v| v.to_str().ok())
                                    .and_then(|v| v.strip_prefix("Bearer "))
                                    .or_else(|| req.headers().get("x-api-key").and_then(|v| v.to_str().ok()));

                                let auth_error_msg = match api_key {
                                    Some(key) => {
                                        let key_hash = crate::api::dashboard::auth::hash_api_key(key);
                                        if crate::db::repository::verify_api_key(&db, &key_hash)
                                            .ok()
                                            .flatten()
                                            .is_some()
                                        {
                                            None // Klucz poprawny
                                        } else {
                                            Some(r#"{"error":{"type":"authentication_error","message":"Niepoprawny API key","code":"invalid_api_key"}}"#)
                                        }
                                    }
                                    None => {
                                        Some(r#"{"error":{"type":"authentication_error","message":"Brak API key. Uzyj naglowka Authorization: Bearer <key> lub x-api-key","code":"missing_api_key"}}"#)
                                    }
                                };

                                if let Some(err_body) = auth_error_msg {
                                    let full = http_body_util::Full::new(hyper::body::Bytes::from(err_body));
                                    let resp = hyper::Response::builder()
                                        .status(401)
                                        .header("Content-Type", "application/json")
                                        .body(UnsyncBoxBody::new(full.map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { match e {} })))
                                        .unwrap();
                                    return Ok(resp);
                                }
                            }

                            let resp = crate::api::openai::server::handle_request(req, router).await?;
                            let resp = resp.map(|body| {
                                UnsyncBoxBody::new(body.map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) }))
                            });
                            Ok::<_, hyper::Error>(resp)
                        } else {
                            let resp = crate::api::dashboard::server::handle_request(req, db, metrics, cipher, sc, sm, router, mps, qm, lni, msec, pc, ra).await?;
                            let resp = resp.map(|body| {
                                UnsyncBoxBody::new(body.map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() }))
                            });
                            Ok::<_, hyper::Error>(resp)
                        }
                    }
                });

                let conn = http1::Builder::new()
                    .serve_connection(io, service)
                    .with_upgrades();
                if let Err(e) = conn.await {
                    let msg = e.to_string();
                    if !msg.contains("connection closed") && !msg.contains("incomplete") {
                        error!("Blad obslugi polaczenia od {}: {}", remote_addr, e);
                    }
                }
            });
        }
    });

    Ok(())
}
