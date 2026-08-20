// =============================================================================
// Plik: api/unified_server.rs
// Opis: Zunifikowany serwer HTTPS obslugujacy jednoczesnie OpenAI API i Dashboard
//       na jednym porcie. Uzywa wbudowanych certyfikatow TLS. Wspoldzielony
//       miedzy Router.New, Desktop i Mobile.
// =============================================================================

use std::sync::Arc;

use anyhow::Result;
use http_body_util::combinators::UnsyncBoxBody;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::Request;
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, error, info, warn};

use crate::config::NodeConfig;
use crate::crypto::{generate_master_key, SecretsCipher, SettingsCipher};
use crate::db;
use crate::mesh::iroh_manager::IrohMeshManager;
use crate::mesh::peer_store::MeshPeerStore;
use crate::mesh::security::MeshSecurity;
use crate::metrics::RouterMetrics;
use crate::routing::Router;
use crate::services::runtime::quic_handle::ServiceManager;

/// The three identities a verified `/v1` API key resolves to, minted together so
/// no caller can pair the wrong `Principal` with the wrong `FlowActor`.
///
/// Extracted from the request path because this mapping is load-bearing: it
/// decides both authorization (`Principal`) and what the event log reports as
/// the actor (§2.5). A `user` key whose account is gone or inactive must NOT
/// slip through with no principal, and a `group` / `general` key has no user
/// behind it — reporting one would name a person for a service call.
pub(crate) struct ApiKeyIdentity {
    /// Kept so Tier-1 `*_for_user` paths still work. `None` for keys with no
    /// user behind them.
    pub user_ctx: Option<crate::auth::acl::UserContext>,
    /// The authoritative `/v1` gate principal.
    pub principal: crate::auth::acl::Principal,
    /// §2.5 actor. Always an `api_key`; `user_id` is `Some` only when the key is
    /// bound to a real, active user.
    pub actor: crate::flow_engine::dispatcher::FlowActor,
}

/// Maps a VERIFIED api_keys row onto the identities above. `Err` is the JSON
/// body returned as 401 — every unmapped shape (unknown `key_type`, missing
/// subject, deleted or deactivated user, unknown group) is a rejection, never a
/// degraded principal.
pub(crate) fn identity_for_api_key(
    db: &crate::db::DbPool,
    api_key_row: &crate::db::models::DbApiKey,
) -> std::result::Result<ApiKeyIdentity, &'static str> {
    use crate::flow_engine::dispatcher::FlowActor;

    match api_key_row.key_type.as_str() {
        "user" => {
            // A 'user' key MUST resolve to an existing, ACTIVE user; otherwise
            // it would slip through with no Principal.
            let resolved = api_key_row.subject_id.as_deref().and_then(|uid| {
                crate::db::repository::get_user_account_by_id(db, uid)
                    .ok()
                    .flatten()
                    .map(|u| (uid.to_string(), u))
            });
            match resolved {
                Some((uid, user)) if user.is_active => Ok(ApiKeyIdentity {
                    user_ctx: Some(crate::auth::acl::UserContext::new(
                        uid.clone(),
                        user.role.clone(),
                    )),
                    actor: FlowActor::api_key(api_key_row.uid.clone(), Some(uid.clone())),
                    principal: crate::auth::acl::Principal::User {
                        user_id: uid,
                        role: user.role,
                    },
                }),
                _ => Err(INVALID_API_KEY_BODY),
            }
        }
        "group" => {
            // A 'group' key MUST resolve to an existing group; no UserContext,
            // no admin-bypass — only group rules apply, and no user is behind it.
            match api_key_row.subject_id.as_deref().and_then(|gid| {
                crate::db::repository::get_group_by_id(db, gid)
                    .ok()
                    .flatten()
                    .map(|_| gid.to_string())
            }) {
                Some(group_id) => Ok(ApiKeyIdentity {
                    user_ctx: None,
                    actor: FlowActor::api_key(api_key_row.uid.clone(), None),
                    principal: crate::auth::acl::Principal::Group { group_id },
                }),
                None => Err(INVALID_API_KEY_BODY),
            }
        }
        "general" => {
            // A 'general' key carries its own explicit allowlist (subject_type=
            // 'api_key', subject_id=uid). No role, never admin-bypass, and it is
            // a SERVICE key: no user binding.
            Ok(ApiKeyIdentity {
                user_ctx: None,
                actor: FlowActor::api_key(api_key_row.uid.clone(), None),
                principal: crate::auth::acl::Principal::ApiKey {
                    uid: api_key_row.uid.clone(),
                },
            })
        }
        _ => Err(INVALID_API_KEY_BODY),
    }
}

const INVALID_API_KEY_BODY: &str =
    r#"{"error":{"type":"authentication_error","message":"Niepoprawny API key","code":"invalid_api_key"}}"#;

/// Sprawdza czy request powinien byc obsluzony przez OpenAI API handler.
/// Obejmuje takze publiczna dokumentacje REST (/openapi.json, /docs i jej asety),
/// ktora jest serwowana przez ten sam handler i — jak /health/ready — bez auth.
pub fn is_openai_path(path: &str) -> bool {
    path.starts_with("/v1/")
        || path == "/health"
        || path == "/ready"
        || path == "/openapi.json"
        || path == "/docs"
        || path.starts_with("/docs/")
}

/// Sciezki OpenAI handlera, ktore sa publiczne i NIE wymagaja klucza API.
/// /docs + /openapi.json sa dokumentacja (publiczna), /health + /ready to probe.
fn is_public_openai_path(path: &str) -> bool {
    path == "/health"
        || path == "/ready"
        // `/v1/*` aliases are handled as public probes in the OpenAI server and
        // documented as auth-free — they must be exempted here too, otherwise
        // the API-key gate answers a monitoring probe with 401.
        || path == "/v1/health"
        || path == "/v1/ready"
        || path == "/openapi.json"
        || path == "/docs"
        || path.starts_with("/docs/")
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
    quic_mesh: Option<Arc<IrohMeshManager>>,
    local_node_id: Arc<str>,
    mesh_security: Option<Arc<MeshSecurity>>,
    addon_manager: Option<Arc<crate::addon::AddonManager>>,
    mesh_relay_health: Option<Arc<parking_lot::RwLock<crate::mesh::relay_health::RelayHealth>>>,
    port_allocator: Option<Arc<crate::services::ports::PortAllocator>>,
    mesh_services_registry: Arc<crate::services::mesh_registry::MeshServicesRegistry>,
) -> Result<()> {
    let permission_checker = addon_manager
        .as_ref()
        .map(|m| m.permission_checker().clone());
    start_unified_server_with_permissions(
        config,
        db,
        metrics,
        router,
        mesh_peer_store,
        quic_mesh,
        local_node_id,
        mesh_security,
        permission_checker,
        addon_manager,
        mesh_relay_health,
        port_allocator,
        mesh_services_registry,
    )
}

/// Zunifikowany serwer z opcjonalnym PermissionChecker do natychmiastowej invalidacji cache
pub fn start_unified_server_with_permissions(
    config: &NodeConfig,
    db: &db::DbPool,
    metrics: &Arc<RouterMetrics>,
    router: &Arc<Router>,
    mesh_peer_store: &MeshPeerStore,
    quic_mesh: Option<Arc<IrohMeshManager>>,
    local_node_id: Arc<str>,
    mesh_security: Option<Arc<MeshSecurity>>,
    permission_checker: Option<Arc<crate::addon::permissions::PermissionChecker>>,
    addon_manager: Option<Arc<crate::addon::AddonManager>>,
    mesh_relay_health: Option<Arc<parking_lot::RwLock<crate::mesh::relay_health::RelayHealth>>>,
    port_allocator: Option<Arc<crate::services::ports::PortAllocator>>,
    mesh_services_registry: Arc<crate::services::mesh_registry::MeshServicesRegistry>,
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
    let master_key =
        db::repository::get_setting_secure(db, "encryption_master_key", &settings_cipher)
            .ok()
            .flatten()
            .unwrap_or_else(|| {
                let key = generate_master_key();
                let _ = db::repository::set_setting_secure(
                    db,
                    "encryption_master_key",
                    &key,
                    &settings_cipher,
                );
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
    let addon_manager = addon_manager.clone();
    let mesh_relay_health = mesh_relay_health.clone();
    let port_allocator = port_allocator.clone();
    let mesh_services_registry = mesh_services_registry.clone();

    // Initialise the process-wide pickup mTLS profile from the loaded config.
    // The verifier wired into rustls below offers client auth iff this profile
    // says pickup is required; the HTTP layer enforces fingerprint pinning.
    let pickup_mtls = config
        .server
        .mtls
        .clone()
        .map(|c| {
            crate::api::mtls::PickupMtlsConfig::new(c.pickup_required, c.client_cert_fingerprints)
        })
        .unwrap_or_default();
    let mtls_offers_client_auth = pickup_mtls.requests_client_cert();
    crate::api::mtls::set_pickup_mtls_config(pickup_mtls);

    // Per-installation certificate from <data>/tls (generated on first start,
    // regenerated when local IPs change). The certificate embedded in the
    // binary is only the emergency fallback when the data dir is unusable.
    let tls_acceptor = {
        let extra_sans = config
            .server
            .tls
            .as_ref()
            .map(|t| t.extra_sans.clone())
            .unwrap_or_default();
        let hostname = crate::mesh::node_info_collector::local_hostname();
        let (certs, key) = match crate::api::tls_identity::load_or_generate(
            &crate::paths::tls_dir(),
            &hostname,
            &extra_sans,
        ) {
            Ok(identity) => (identity.certs, identity.key),
            Err(e) => {
                warn!(
                    error = %e,
                    "TLS: per-installation certificate unavailable, using embedded fallback"
                );
                let cert_pem = include_bytes!("../../../certs/cert.pem");
                let key_pem = include_bytes!("../../../certs/key.pem");
                let certs = crate::api::tls_pem::parse_certs_pem(cert_pem)
                    .expect("Nie udalo sie sparsowac wbudowanego certyfikatu");
                let key = crate::api::tls_pem::parse_key_pem(key_pem)
                    .expect("Nie udalo sie sparsowac wbudowanego klucza");
                (certs, key)
            }
        };

        // TLS 1.3 only — F1b is HTTPS-native, no legacy clients to support.
        // Pinning the version here also pins AEAD-only cipher suites and
        // forward-secret key exchange (X25519 / P-256), eliminating the need
        // for an explicit cipher allowlist.
        let builder =
            rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13]);

        let mut tls_config = if mtls_offers_client_auth {
            builder
                .with_client_cert_verifier(crate::api::mtls::AnyClientCertVerifier::new())
                .with_single_cert(certs, key)
                .expect("Nie udalo sie skonfigurowac TLS (mTLS)")
        } else {
            builder
                .with_no_client_auth()
                .with_single_cert(certs, key)
                .expect("Nie udalo sie skonfigurowac TLS")
        };

        tls_config.alpn_protocols = vec![b"http/1.1".to_vec()];

        TlsAcceptor::from(Arc::new(tls_config))
    };

    // OAuth pending-state TTL purge: run once at startup, then hourly.
    crate::addon::oauth_cleanup::start_oauth_cleanup_task(db.clone());
    crate::scheduler::start(db.clone(), addon_manager.clone());
    // Project Studio run schedules. Separate loop from the admin scheduler: it
    // fires per-project runs out of `projects.db` hints, not addon actions.
    crate::project_studio::schedules::start(
        db.clone(),
        settings_cipher.clone(),
        local_node_id.clone(),
    );

    info!("Inicjalizacja unified HTTPS server na {}...", bind_addr);

    // Subskrybuj shutdown signal z ServiceManager — przy shutdown zamykamy
    // accept loop, dropping TcpListener i zwalniajac port TCP natychmiast
    // (bez TIME_WAIT zombie).
    let mut shutdown_rx = service_manager.shutdown_rx.clone();

    // Subskrypcja eventow cyklu zycia (iOS resume po suspend). Na wake
    // wymuszamy rebind listenera, bo iOS przy suspendzie moze uniewaznic
    // socket loopback (errno 9 EBADF / errno 57 ENOTCONN przy accept).
    let mut lifecycle_rx = crate::lifecycle_signal::subscribe();

    tokio::spawn(async move {
        // Outer rebind loop — listener jest tworzony od nowa gdy wymusi to
        // lifecycle signal LUB nastapi seria bledow accept (iOS po suspendzie).
        'rebind: loop {
            let listener = match TcpListener::bind(&bind_addr).await {
                Ok(l) => l,
                Err(e) => {
                    error!("Nie mozna zbindowac na {}: {} — retry za 1s", bind_addr, e);
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue 'rebind;
                }
            };

            info!(
                "Unified HTTPS server nasluchuje na {} (OpenAI API + Dashboard)",
                bind_addr
            );

            // Licznik kolejnych bledow accept — po 5 w ciagu 10s uznajemy
            // listener za zdychlego i robimy rebind (kernel mogl zresetowac socket).
            let mut consecutive_errors: u32 = 0;
            let mut first_error_at: Option<std::time::Instant> = None;

            loop {
                let accept = tokio::select! {
                    biased;
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            info!("Unified server: shutdown — zamykam listener");
                            return;
                        }
                        continue;
                    }
                    lc = lifecycle_rx.recv() => {
                        match lc {
                            Ok(crate::lifecycle_signal::LifecycleEvent::Resume) => {
                                warn!("Unified server: Resume — forsuje rebind listenera na {}", bind_addr);
                                continue 'rebind;
                            }
                            Ok(_) => continue,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                // Kanal zamkniety — nie powinno sie zdarzyc dla static OnceLock.
                                continue;
                            }
                        }
                    }
                    res = listener.accept() => res,
                };
                let (stream, remote_addr) = match accept {
                    Ok(conn) => {
                        consecutive_errors = 0;
                        first_error_at = None;
                        // TCP_NODELAY = wylacz Nagle algorithm. Dla streaming
                        // WS (chat tokens, metrics, deploy logs) każdy chunk
                        // jest mały (50-300 B) — Nagle czeka aż buffer się
                        // zapelni albo 200ms timeout, co blokuje LLM streaming
                        // do widocznych "tokenow co 30s". Loopback też dotyczy.
                        if let Err(e) = conn.0.set_nodelay(true) {
                            tracing::warn!("set_nodelay failed dla {}: {}", conn.1, e);
                        }
                        conn
                    }
                    Err(e) => {
                        error!("Blad akceptowania polaczenia: {}", e);
                        consecutive_errors += 1;
                        let now = std::time::Instant::now();
                        let first = first_error_at.get_or_insert(now);
                        if consecutive_errors >= 5
                            && now.duration_since(*first) < std::time::Duration::from_secs(10)
                        {
                            warn!(
                                "Unified server: {} bledow accept w {}ms — rebind listenera",
                                consecutive_errors,
                                now.duration_since(*first).as_millis()
                            );
                            continue 'rebind;
                        }
                        // Krotkim sleep unikamy busy-loopa na persystentnym EBADF.
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
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
                let am = addon_manager.clone();
                let mrh = mesh_relay_health.clone();
                let pa = port_allocator.clone();
                let msr = mesh_services_registry.clone();
                let license: Arc<dyn crate::license::LicenseChecker> =
                    Arc::new(crate::license::StaticLicenseChecker::free());

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
                    // Snapshot peer (client) certificate DER bytes, if the
                    // client offered one during the handshake. Forwarded into
                    // request extensions so /core/frame/pickup can pin the
                    // SHA-256 fingerprint at the HTTP layer.
                    let client_cert_der: Option<Vec<u8>> = tls_stream
                        .get_ref()
                        .1
                        .peer_certificates()
                        .and_then(|chain| chain.first().map(|c| c.as_ref().to_vec()));
                    let io = TokioIo::new(tls_stream);

                    // VULN-035: Przekaz remote_addr do handle_request
                    let remote_addr_str = remote_addr.to_string();
                    let client_cert_der = client_cert_der.clone();
                    let service = service_fn(move |mut req: Request<Incoming>| {
                        // Wstrzykuj peer cert DER do extensions — handlery
                        // (np. /core/frame/pickup) wyciagaja go przez
                        // `req.extensions().get::<ClientCertDer>()`.
                        if let Some(der) = client_cert_der.clone() {
                            req.extensions_mut()
                                .insert(crate::api::mtls::ClientCertDer(der));
                        }
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
                        let am = am.clone();
                        let mrh = mrh.clone();
                        let pa = pa.clone();
                        let msr = msr.clone();
                        let lic = license.clone();
                        let ra = remote_addr_str.clone();
                        async move {
                            let path = req.uri().path().to_string();

                            if is_openai_path(&path) {
                                let mut owner_user_ctx: Option<crate::auth::acl::UserContext> =
                                    None;
                                let mut principal: Option<crate::auth::acl::Principal> = None;
                                // §2.5 — the /v1 actor. Minted HERE because this
                                // is the only place the API key → user binding is
                                // known: the call itself never carries it, and a
                                // service key legitimately has none.
                                let mut flow_actor: Option<
                                    crate::flow_engine::dispatcher::FlowActor,
                                > = None;
                                // (uid, rate_limit_rps) of the authenticated key,
                                // captured for every key type so the per-key limiter
                                // can be enforced after auth (NOT keyed by IP).
                                let mut key_rate_limit: Option<(String, i64)> = None;
                                // VULN-001: Sprawdz API key dla sciezek OpenAI (oprocz
                                // publicznych: /health, /ready oraz dokumentacja /docs + /openapi.json)
                                if !is_public_openai_path(&path) {
                                    let api_key = req
                                        .headers()
                                        .get("authorization")
                                        .and_then(|v| v.to_str().ok())
                                        .and_then(|v| v.strip_prefix("Bearer "))
                                        .or_else(|| {
                                            req.headers()
                                                .get("x-api-key")
                                                .and_then(|v| v.to_str().ok())
                                        });

                                    let auth_error_msg = match api_key {
                                        Some(key) => {
                                            // Fail-closed: without a valid pepper we MUST NOT
                                            // compute a verifier (an empty pepper would derive
                                            // the HMAC under the wrong key and could match a
                                            // forged row), so a pepper error rejects the request.
                                            match crate::db::repository::get_or_create_api_key_pepper(&db, &sc) {
                                                Err(_) => Some(
                                                    r#"{"error":{"type":"authentication_error","message":"Blad weryfikacji API key","code":"api_key_verification_unavailable"}}"#,
                                                ),
                                                Ok(pepper) => {
                                            let verifier =
                                                crate::api::dashboard::auth::api_key_verifier(
                                                    key, &pepper,
                                                );
                                            match crate::db::repository::verify_api_key(
                                                &db, &verifier,
                                            ) {
                                                Ok(Some(api_key_row)) => {
                                                    // Capture the per-key budget regardless of type;
                                                    // it is only enforced once the key resolves to a
                                                    // valid principal below.
                                                    key_rate_limit = Some((
                                                        api_key_row.uid.clone(),
                                                        api_key_row.rate_limit_rps,
                                                    ));
                                                    // §2.5 — the actor is
                                                    // minted HERE with the
                                                    // principal, because this is
                                                    // the only place the API key
                                                    // → user binding is known.
                                                    match identity_for_api_key(
                                                        &db,
                                                        &api_key_row,
                                                    ) {
                                                        Ok(identity) => {
                                                            owner_user_ctx = identity.user_ctx;
                                                            flow_actor = Some(identity.actor);
                                                            principal = Some(identity.principal);
                                                            None
                                                        }
                                                        Err(body) => Some(body),
                                                    }
                                                }
                                                _ => Some(
                                                    r#"{"error":{"type":"authentication_error","message":"Niepoprawny API key","code":"invalid_api_key"}}"#,
                                                ),
                                            }
                                                }
                                            }
                                        }
                                        None => Some(
                                            r#"{"error":{"type":"authentication_error","message":"Brak API key. Uzyj naglowka Authorization: Bearer <key> lub x-api-key","code":"missing_api_key"}}"#,
                                        ),
                                    };

                                    if let Some(err_body) = auth_error_msg {
                                        let full = http_body_util::Full::new(
                                            hyper::body::Bytes::from(err_body),
                                        );
                                        let mut resp = hyper::Response::builder()
                                            .status(401)
                                            .header("Content-Type", "application/json")
                                            .body(UnsyncBoxBody::new(full.map_err(
                                                |e| -> Box<dyn std::error::Error + Send + Sync> {
                                                    match e {}
                                                },
                                            )))
                                            .unwrap();
                                        // Early auth failure bypasses the handler path — apply the
                                        // same unconditional security headers (HSTS etc.).
                                        crate::api::mtls::apply_universal_security_headers(
                                            resp.headers_mut(),
                                        );
                                        return Ok(resp);
                                    }

                                    // Per-key rate limit (token bucket keyed by key uid, not IP).
                                    // Enforced only after the key authenticated successfully.
                                    if let Some((ref uid, rps)) = key_rate_limit {
                                        if let Some(retry) =
                                            crate::api::rate_limit::per_key_rate_limiter()
                                                .check(uid, rps)
                                        {
                                            let retry_secs = retry.ceil().max(1.0) as u64;
                                            let body = r#"{"error":{"type":"rate_limit_error","message":"Przekroczono limit zadan dla tego klucza API","code":"rate_limit_exceeded"}}"#;
                                            let full = http_body_util::Full::new(
                                                hyper::body::Bytes::from(body),
                                            );
                                            let mut resp = hyper::Response::builder()
                                                .status(429)
                                                .header("Content-Type", "application/json")
                                                .header("Retry-After", retry_secs.to_string())
                                                .body(UnsyncBoxBody::new(full.map_err(
                                                    |e| -> Box<dyn std::error::Error + Send + Sync> {
                                                        match e {}
                                                    },
                                                )))
                                                .unwrap();
                                            // This early 429 bypasses the normal handler path, so
                                            // apply the same security headers (HSTS is uncondi-
                                            // tional) that every other response gets.
                                            crate::api::mtls::apply_universal_security_headers(
                                                resp.headers_mut(),
                                            );
                                            return Ok(resp);
                                        }
                                    }
                                }

                                // Wstrzykuje UserContext (Tier1 `*_for_user`) oraz Principal
                                // (autorytatywna brama /v1 — dziala dla user/group/general)
                                // do request extensions.
                                if let Some(uc) = owner_user_ctx {
                                    req.extensions_mut().insert(uc);
                                }
                                if let Some(p) = principal {
                                    req.extensions_mut().insert(p);
                                }
                                if let Some(a) = flow_actor {
                                    req.extensions_mut().insert(a);
                                }
                                let resp =
                                    crate::api::openai::server::handle_request(req, router).await?;
                                let mut resp = resp.map(|body| {
                                    UnsyncBoxBody::new(body.map_err(
                                        |e| -> Box<dyn std::error::Error + Send + Sync> {
                                            Box::new(e)
                                        },
                                    ))
                                });
                                crate::api::mtls::apply_universal_security_headers(
                                    resp.headers_mut(),
                                );
                                Ok::<_, hyper::Error>(resp)
                            } else {
                                let resp = crate::api::dashboard::server::handle_request(
                                    req, db, metrics, cipher, sc, sm, router, mps, qm, lni, msec,
                                    pc, am, lic, mrh, pa, ra, msr,
                                )
                                .await?;
                                let mut resp = resp.map(|body| {
                                    UnsyncBoxBody::new(body.map_err(
                                        |e| -> Box<dyn std::error::Error + Send + Sync> {
                                            e.into()
                                        },
                                    ))
                                });
                                crate::api::mtls::apply_universal_security_headers(
                                    resp.headers_mut(),
                                );
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
            } // koniec wewnetrznej petli accept
        } // koniec 'rebind loop
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::acl::Principal;
    use crate::db::models::DbApiKey;
    use crate::flow_engine::dispatcher::ActorKind;
    use std::path::Path;

    fn fresh_db() -> db::DbPool {
        db::init(Path::new(":memory:")).expect("in-memory db")
    }

    fn key_row(uid: &str, key_type: &str, subject_id: Option<&str>) -> DbApiKey {
        DbApiKey {
            id: 1,
            uid: uid.to_string(),
            key_verifier: "v".to_string(),
            key_prefix: "tf_".to_string(),
            name: "test key".to_string(),
            key_type: key_type.to_string(),
            subject_id: subject_id.map(|s| s.to_string()),
            rate_limit_rps: 10,
            is_active: true,
            created_at: String::new(),
            last_used_at: None,
        }
    }

    /// A `user` key backed by an ACTIVE account resolves to that user on all
    /// three axes: the Tier-1 `UserContext`, the `/v1` gate `Principal`, and the
    /// §2.5 actor — which stays an `api_key` naming the KEY, with the user it is
    /// bound to. Reporting `ActorKind::User` here would hide that a key was used.
    #[test]
    fn user_key_with_active_account_binds_the_actor_to_that_user() {
        let db = fresh_db();
        let user_id = crate::db::repository::create_user_account(
            &db, "alice", "hash", "Alice", "a@example.com",
        )
        .expect("user");

        let identity =
            identity_for_api_key(&db, &key_row("key-user", "user", Some(&user_id))).expect("ok");

        assert_eq!(
            identity.user_ctx.as_ref().map(|u| u.user_id.as_str()),
            Some(user_id.as_str())
        );
        assert!(matches!(identity.principal, Principal::User { .. }));
        assert_eq!(identity.actor.kind(), ActorKind::ApiKey);
        assert_eq!(identity.actor.id(), Some("key-user"));
        assert_eq!(identity.actor.user_id(), Some(user_id.as_str()));
    }

    /// An INACTIVE account is a rejection, not a degraded principal: letting the
    /// key through with `principal = None` would put it on the default-DENY
    /// path's blind side, and stamping a user would attribute calls to someone
    /// whose access was revoked.
    #[test]
    fn user_key_with_inactive_account_is_refused() {
        let db = fresh_db();
        let user_id = crate::db::repository::create_user_account(
            &db, "bob", "hash", "Bob", "b@example.com",
        )
        .expect("user");
        {
            let conn = db.write().expect("db lock");
            conn.execute(
                "UPDATE user_accounts SET is_active = 0 WHERE id = ?1",
                rusqlite::params![user_id],
            )
            .expect("deactivate");
        }

        assert!(identity_for_api_key(&db, &key_row("key-user", "user", Some(&user_id))).is_err());
        // A user key naming an account that does not exist at all is refused too.
        assert!(identity_for_api_key(&db, &key_row("key-user", "user", Some("ghost"))).is_err());
    }

    /// A `group` key has NO user behind it. `actor.user_id()` must stay `None`
    /// so the UI shows a service key instead of naming a person.
    #[test]
    fn group_key_resolves_to_the_group_with_no_user_behind_it() {
        let db = fresh_db();
        let group_id =
            crate::db::repository::create_group(&db, "ops", "ops team").expect("group");

        let identity =
            identity_for_api_key(&db, &key_row("key-group", "group", Some(&group_id))).expect("ok");

        assert!(identity.user_ctx.is_none());
        assert!(matches!(identity.principal, Principal::Group { .. }));
        assert_eq!(identity.actor.kind(), ActorKind::ApiKey);
        assert_eq!(identity.actor.id(), Some("key-group"));
        assert_eq!(identity.actor.user_id(), None);

        // An unknown group is a rejection, not a group principal with a dangling id.
        assert!(identity_for_api_key(&db, &key_row("key-group", "group", Some("nope"))).is_err());
    }

    /// A `general` key is a pure service key: its own allowlist, no role, and no
    /// user binding. It needs no DB lookup, so it must not acquire one either.
    #[test]
    fn general_key_is_a_service_key_with_no_user_binding() {
        let db = fresh_db();
        let identity = identity_for_api_key(&db, &key_row("key-general", "general", None))
            .expect("ok");

        assert!(identity.user_ctx.is_none());
        match &identity.principal {
            Principal::ApiKey { uid } => assert_eq!(uid, "key-general"),
            other => panic!("expected ApiKey principal, got {other:?}"),
        }
        assert_eq!(identity.actor.kind(), ActorKind::ApiKey);
        assert_eq!(identity.actor.id(), Some("key-general"));
        assert_eq!(identity.actor.user_id(), None);
    }

    /// An unknown `key_type` is refused. Without this arm a future key type
    /// would reach the gate with no principal at all.
    #[test]
    fn unknown_key_type_is_refused() {
        let db = fresh_db();
        assert!(identity_for_api_key(&db, &key_row("key-x", "robot", Some("u-1"))).is_err());
    }
}
