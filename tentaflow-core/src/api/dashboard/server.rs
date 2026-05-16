// =============================================================================
// Plik: api/dashboard/server.rs
// Opis: HTTP server dashboardu - routing, middleware JWT auth, CORS.
// =============================================================================

use super::{api_addon_system, auth, static_files};
use crate::db::{self, DbPool};
use crate::license::{LicenseChecker, StaticLicenseChecker};
use crate::mesh::peer_store::MeshPeerStore;
use crate::metrics::RouterMetrics;
use crate::services::runtime::quic_handle::ServiceManager;
use std::sync::Arc;

use crate::routing::router::Router;
use futures::Stream;
use http_body_util::{BodyExt, Either, Full, StreamBody};
use hyper::body::Bytes;
use hyper::body::Frame;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{body::Incoming, Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::pin::Pin;
use tokio::net::TcpListener;
use tracing::{debug, error, info, warn};

type SseStream = Pin<Box<dyn Stream<Item = Result<Frame<Bytes>, std::io::Error>> + Send>>;
pub type DashboardBody = Either<Full<Bytes>, StreamBody<SseStream>>;

/// Serwer HTTP dashboardu z JWT auth
pub struct DashboardServer {
    db: DbPool,
    bind: String,
    metrics: Arc<RouterMetrics>,
    cipher: Arc<crate::crypto::SecretsCipher>,
    settings_cipher: Arc<crate::crypto::SettingsCipher>,
    service_manager: Arc<ServiceManager>,
    router: Arc<Router>,
    mesh_peer_store: MeshPeerStore,
    quic_mesh: Option<Arc<crate::mesh::iroh_manager::IrohMeshManager>>,
    local_node_id: Arc<str>,
    mesh_security: Option<Arc<crate::mesh::security::MeshSecurity>>,
    permission_checker: Option<Arc<crate::addon::permissions::PermissionChecker>>,
    addon_manager: Option<Arc<crate::addon::AddonManager>>,
    license: Arc<dyn LicenseChecker>,
    mesh_relay_health: Option<Arc<parking_lot::RwLock<crate::mesh::relay_health::RelayHealth>>>,
    port_allocator: Option<Arc<crate::services::ports::PortAllocator>>,
    mesh_services_registry: Arc<crate::services::mesh_registry::MeshServicesRegistry>,
}

impl DashboardServer {
    pub fn new(
        db: DbPool,
        bind: &str,
        metrics: Arc<RouterMetrics>,
        cipher: Arc<crate::crypto::SecretsCipher>,
        settings_cipher: Arc<crate::crypto::SettingsCipher>,
        service_manager: Arc<ServiceManager>,
        router: Arc<Router>,
        mesh_peer_store: MeshPeerStore,
    ) -> Self {
        Self {
            db,
            bind: bind.to_string(),
            metrics,
            cipher,
            settings_cipher,
            service_manager,
            router,
            mesh_peer_store,
            quic_mesh: None,
            local_node_id: Arc::from(""),
            mesh_security: None,
            permission_checker: None,
            addon_manager: None,
            license: Arc::new(StaticLicenseChecker::free()),
            mesh_relay_health: None,
            port_allocator: None,
            mesh_services_registry: Arc::new(
                crate::services::mesh_registry::MeshServicesRegistry::new(),
            ),
        }
    }

    /// Wstrzykuje shared `PortAllocator` (wlasnosciowo nalezy do supervisor).
    pub fn with_port_allocator(
        mut self,
        allocator: Option<Arc<crate::services::ports::PortAllocator>>,
    ) -> Self {
        self.port_allocator = allocator;
        self
    }

    /// Ustawia snapshot zdrowia relay aktualizowany w tle przez mesh pipeline.
    pub fn with_relay_health(
        mut self,
        relay_health: Option<Arc<parking_lot::RwLock<crate::mesh::relay_health::RelayHealth>>>,
    ) -> Self {
        self.mesh_relay_health = relay_health;
        self
    }

    /// Ustawia LicenseChecker — sprawdzanie tieru licencji (Free/Pro/Enterprise)
    pub fn with_license_checker(mut self, license: Arc<dyn LicenseChecker>) -> Self {
        self.license = license;
        self
    }

    /// Ustawia QUIC mesh manager i local node id — wymagane do forwardowania komend
    pub fn with_quic_mesh(
        mut self,
        quic_mesh: Option<Arc<crate::mesh::iroh_manager::IrohMeshManager>>,
        local_node_id: Arc<str>,
    ) -> Self {
        self.quic_mesh = quic_mesh;
        self.local_node_id = local_node_id;
        self
    }

    /// Ustawia MeshSecurity — bezpieczenstwo mesh (klucze, parowanie, szyfrowanie)
    pub fn with_mesh_security(
        mut self,
        security: Option<Arc<crate::mesh::security::MeshSecurity>>,
    ) -> Self {
        self.mesh_security = security;
        self
    }

    /// Ustawia PermissionChecker — proaktywny cache uprawnien addonow
    pub fn with_permission_checker(
        mut self,
        checker: Option<Arc<crate::addon::permissions::PermissionChecker>>,
    ) -> Self {
        self.permission_checker = checker;
        self
    }

    /// Ustawia AddonManager — udostepnia ui_panels cache i invoke_ui_action
    /// dla handlerów Apps menu / UI v2.
    pub fn with_addon_manager(
        mut self,
        addon_manager: Option<Arc<crate::addon::AddonManager>>,
    ) -> Self {
        self.addon_manager = addon_manager;
        self
    }

    /// Uruchamia serwer HTTP - blokuje do zakonczenia
    pub async fn run(&self) -> anyhow::Result<()> {
        let listener = TcpListener::bind(&self.bind).await?;
        info!("Dashboard server nasluchuje na {}", self.bind);

        let db = self.db.clone();
        let metrics = self.metrics.clone();
        let cipher = self.cipher.clone();
        let settings_cipher = self.settings_cipher.clone();
        let service_manager = self.service_manager.clone();
        let router = self.router.clone();
        let mesh_peer_store = self.mesh_peer_store.clone();
        let quic_mesh = self.quic_mesh.clone();
        let local_node_id = self.local_node_id.clone();
        let mesh_security = self.mesh_security.clone();
        let permission_checker = self.permission_checker.clone();
        let addon_manager = self.addon_manager.clone();
        let license = self.license.clone();
        let mesh_relay_health = self.mesh_relay_health.clone();
        let port_allocator = self.port_allocator.clone();
        let mesh_services_registry = self.mesh_services_registry.clone();

        // Wire up cross-node service action handlers (krok N3b). The mesh
        // command executor is created by `start_mesh_pipeline` long before
        // AppState (db_pool + port_allocator + iroh) is fully assembled, so
        // we inject the action context here once everything exists. Without
        // this the receiver of `ServiceDeleteRemote` / `ServicePinRemote` /
        // ... returns "service action context not configured".
        if let (Some(qm), Some(pa)) = (quic_mesh.clone(), port_allocator.clone()) {
            if let Some(executor) = qm.command_executor().await {
                executor
                    .set_service_action_context(
                        crate::mesh::command_executor::ServiceActionContext {
                            db: db.clone(),
                            port_allocator: pa,
                            iroh: qm.clone(),
                        },
                    )
                    .await;
            }
        }

        loop {
            let (stream, remote_addr) = match listener.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    error!("Blad akceptowania polaczenia (dashboard): {}", e);
                    continue;
                }
            };

            debug!("Dashboard: polaczenie od {}", remote_addr);

            let db_clone = db.clone();
            let metrics_clone = metrics.clone();
            let cipher_clone = cipher.clone();
            let sc_clone = settings_cipher.clone();
            let sm_clone = service_manager.clone();
            let router_clone = router.clone();
            let mps_clone = mesh_peer_store.clone();
            let qm_clone = quic_mesh.clone();
            let lni_clone = local_node_id.clone();
            let msec_clone = mesh_security.clone();
            let pc_clone = permission_checker.clone();
            let am_clone = addon_manager.clone();
            let lic_clone = license.clone();
            let mrh_clone = mesh_relay_health.clone();
            let pa_clone = port_allocator.clone();
            let msr_clone = mesh_services_registry.clone();
            // VULN-035: Przekaz remote_addr do handle_request (dual rate limiting)
            let remote_addr_str = remote_addr.to_string();

            tokio::spawn(async move {
                let io = TokioIo::new(stream);

                let service = service_fn(move |req| {
                    let db = db_clone.clone();
                    let metrics = metrics_clone.clone();
                    let cipher = cipher_clone.clone();
                    let sc = sc_clone.clone();
                    let sm = sm_clone.clone();
                    let router = router_clone.clone();
                    let mps = mps_clone.clone();
                    let qm = qm_clone.clone();
                    let lni = lni_clone.clone();
                    let msec = msec_clone.clone();
                    let pc = pc_clone.clone();
                    let am = am_clone.clone();
                    let lic = lic_clone.clone();
                    let mrh = mrh_clone.clone();
                    let pa = pa_clone.clone();
                    let msr = msr_clone.clone();
                    let ra = remote_addr_str.clone();
                    async move {
                        handle_request(
                            req, db, metrics, cipher, sc, sm, router, mps, qm, lni, msec, pc, am,
                            lic, mrh, pa, ra, msr,
                        )
                        .await
                    }
                });

                if let Err(e) = http1::Builder::new()
                    .serve_connection(io, service)
                    .with_upgrades()
                    .await
                {
                    if !e.is_incomplete_message() && !e.is_closed() {
                        error!("Blad obslugi polaczenia (dashboard): {}", e);
                    }
                }
            });
        }
    }
}

/// Sprawdza czy origin pochodzi z localhost
fn is_localhost_origin(origin: &str) -> bool {
    let host = origin
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    let host_without_port = host.split(':').next().unwrap_or("");
    matches!(
        host_without_port,
        "localhost" | "127.0.0.1" | "[::1]" | "::1"
    )
}

/// Tworzy Response<DashboardBody> z podanymi parametrami i opcjonalnym CORS origin
fn make_response_with_origin(
    status: u16,
    content_type: &str,
    body: Vec<u8>,
    origin: Option<&str>,
) -> Response<DashboardBody> {
    let mut builder = Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
        .header("Content-Type", content_type);

    if let Some(o) = origin {
        builder = builder
            .header("Access-Control-Allow-Origin", o)
            .header(
                "Access-Control-Allow-Methods",
                "GET, POST, PUT, DELETE, OPTIONS",
            )
            .header(
                "Access-Control-Allow-Headers",
                "Content-Type, Authorization",
            );
    }

    builder
        .body(Either::Left(Full::new(Bytes::from(body))))
        .unwrap()
}

fn json_response_cors(status: u16, body: String, origin: Option<&str>) -> Response<DashboardBody> {
    make_response_with_origin(status, "application/json", body.into_bytes(), origin)
}

fn json_error_cors(status: u16, message: &str, origin: Option<&str>) -> Response<DashboardBody> {
    let body = serde_json::json!({"error": message}).to_string();
    json_response_cors(status, body, origin)
}

/// Konwertuje Result z handlera na krotke (status, body) z formatowaniem bledu.
/// VULN-014: Nie ujawniaj szczegulow bledu w odpowiedzi — loguj wewnetrznie.
fn handle_result(result: anyhow::Result<(u16, String)>, error_status: u16) -> (u16, String) {
    match result {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Wewnetrzny blad serwera: {}", e);
            (
                error_status,
                r#"{"error":"Wewnetrzny blad serwera"}"#.to_string(),
            )
        }
    }
}

/// Reject any GET that smuggles a body onto an unauthenticated signed-URL
/// endpoint. Returns a pre-built 413 response when the request carries a
/// non-empty `Content-Length` or any `Transfer-Encoding` — preventing
/// pre-HMAC memory exhaustion. The body is *never* read here; the caller
/// should drop the request after this check so the connection terminates
/// without slurping bytes off the socket.
fn reject_unauth_get_body(
    headers: &hyper::HeaderMap,
) -> std::result::Result<(), Response<DashboardBody>> {
    if headers.contains_key(hyper::header::TRANSFER_ENCODING) {
        return Err(Response::builder()
            .status(StatusCode::PAYLOAD_TOO_LARGE)
            .header("Content-Type", "application/json")
            .body(Either::Left(Full::new(Bytes::from_static(
                b"{\"error\":\"body_not_allowed\"}",
            ))))
            .unwrap());
    }
    let cl = headers
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    match cl {
        None | Some(0) => Ok(()),
        Some(_) => Err(Response::builder()
            .status(StatusCode::PAYLOAD_TOO_LARGE)
            .header("Content-Type", "application/json")
            .body(Either::Left(Full::new(Bytes::from_static(
                b"{\"error\":\"body_not_allowed\"}",
            ))))
            .unwrap()),
    }
}

/// Wyciaga Bearer token z naglowka Authorization
fn extract_bearer_token(req: &Request<Incoming>) -> Option<&str> {
    req.headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

/// Glowny handler routingu
pub async fn handle_request(
    mut req: Request<Incoming>,
    db: DbPool,
    metrics: Arc<RouterMetrics>,
    cipher: Arc<crate::crypto::SecretsCipher>,
    settings_cipher: Arc<crate::crypto::SettingsCipher>,
    service_manager: Arc<ServiceManager>,
    router: Arc<Router>,
    mesh_peer_store: MeshPeerStore,
    quic_mesh: Option<Arc<crate::mesh::iroh_manager::IrohMeshManager>>,
    local_node_id: Arc<str>,
    mesh_security: Option<Arc<crate::mesh::security::MeshSecurity>>,
    permission_checker: Option<Arc<crate::addon::permissions::PermissionChecker>>,
    addon_manager: Option<Arc<crate::addon::AddonManager>>,
    license: Arc<dyn LicenseChecker>,
    mesh_relay_health: Option<Arc<parking_lot::RwLock<crate::mesh::relay_health::RelayHealth>>>,
    port_allocator: Option<Arc<crate::services::ports::PortAllocator>>,
    _remote_addr: String,
    mesh_services_registry: Arc<crate::services::mesh_registry::MeshServicesRegistry>,
) -> std::result::Result<Response<DashboardBody>, hyper::Error> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query_string = req.uri().query().unwrap_or("").to_string();

    // Wyciagnij i zwaliduj origin dla CORS
    let cors_origin: Option<String> = req
        .headers()
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .filter(|o| is_localhost_origin(o))
        .map(|o| o.to_string());

    debug!("Dashboard: {} {}", method, path);

    // CORS preflight
    if method == Method::OPTIONS {
        return Ok(make_response_with_origin(
            204,
            "text/plain",
            Vec::new(),
            cors_origin.as_deref(),
        ));
    }

    // VULN-038: CSRF — sprawdz Origin/Referer na requestach mutujacych
    // Wyklucz endpointy publiczne (login, SSO callback) — nie maja Auth header
    let csrf_exempt = path == "/api/auth/login"
        || path.contains("/oauth/callback")
        || path.contains("/sso/callback")
        || path == "/core/frame/pickup";
    if !csrf_exempt && (method == Method::POST || method == Method::PUT || method == Method::DELETE)
    {
        let has_origin = req.headers().get("origin").is_some();
        let has_referer = req.headers().get("referer").is_some();
        let has_auth = req.headers().get("authorization").is_some();

        // Jesli jest Origin — waliduj go wzgledem Host (jak wczesniej)
        if let Some(origin) = req.headers().get("origin").and_then(|v| v.to_str().ok()) {
            let host = req
                .headers()
                .get("host")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            let origin_host = origin
                .trim_start_matches("https://")
                .trim_start_matches("http://");
            if !origin_host.is_empty() && !host.is_empty() && !origin_host.starts_with(host) {
                return Ok(json_error_cors(
                    403,
                    "Niedozwolone zrodlo zadania (CSRF)",
                    cors_origin.as_deref(),
                ));
            }
        }

        // VULN-038: Requesty z przegladarki (bez explicit Authorization header) MUSZA miec Origin lub Referer.
        // API clients (curl, SDK) wysylaja Authorization header ale nie Origin — nie blokuj ich.
        if !has_origin && !has_referer && !has_auth {
            warn!("CSRF: mutujacy request bez Origin/Referer/Authorization — zablokowany");
            return Ok(json_error_cors(
                403,
                "Brak Origin — wymagany dla requestow z przegladarki (CSRF)",
                cors_origin.as_deref(),
            ));
        }
    }

    // WebSocket upgrade /ws/metrics
    if method == Method::GET && path == "/ws/metrics" {
        let (_ws_key, accept, ws_subprotocol) = match validate_ws_upgrade(
            &req,
            &db,
            &query_string,
            cors_origin.as_deref(),
            &settings_cipher,
        ) {
            Ok(v) => v,
            Err(resp) => return Ok(resp),
        };

        let upgrade = hyper::upgrade::on(&mut req);
        let metrics_clone = metrics.clone();

        tokio::spawn(async move {
            match upgrade.await {
                Ok(upgraded) => {
                    let io = TokioIo::new(upgraded);
                    super::ws_metrics::handle_ws_connection(io, metrics_clone).await;
                }
                Err(e) => {
                    error!("Blad WebSocket upgrade: {}", e);
                }
            }
        });

        let mut ws_resp = Response::builder()
            .status(StatusCode::SWITCHING_PROTOCOLS)
            .header("Upgrade", "websocket")
            .header("Connection", "Upgrade")
            .header("Sec-WebSocket-Accept", accept);
        // Odzwierciedl subprotocol w odpowiedzi (RFC 6455 wymaga)
        if let Some(ref proto) = ws_subprotocol {
            ws_resp = ws_resp.header("Sec-WebSocket-Protocol", proto.as_str());
        }
        let response = ws_resp.body(Either::Left(Full::new(Bytes::new()))).unwrap();

        return Ok(response);
    }

    // WebSocket upgrade /ws/api — binary rkyv protocol (bootstrap, Task #30).
    // Dispatch do `ws_binary::handle_ws_connection`. Auth jest re-checkowany
    // wewnatrz loopu per MessageBody variant po implementacji #26/#27.
    if method == Method::GET && path == "/ws/api" {
        // Anonymous WS OK — login flow musi zlozyc WS bez JWT zeby zalogowac.
        let (_ws_key, accept, ws_subprotocol) = match validate_ws_upgrade_optional_auth(
            &req,
            &db,
            cors_origin.as_deref(),
            &settings_cipher,
        ) {
            Ok(v) => v,
            Err(resp) => return Ok(resp),
        };

        // Extract (user_id, role) z JWT claims + DB lookup zeby propagowac
        // do dispatch ctx. Role z DB jest Zero Trust (nie z JWT).
        let (user_id, role) = match extract_ws_user_session(&req, &db, &settings_cipher) {
            Some((id, r)) => (Some(id), r),
            None => (None, None),
        };

        // Reuse jwt_secret jako HMAC key dla resume tokens (rotacja sekretu
        // automatycznie unieważnia wszystkie outstanding tokens — pozadane).
        let resume_secret = std::sync::Arc::new(
            db::repository::get_setting_secure(&db, "jwt_secret", &settings_cipher)
                .ok()
                .flatten()
                .map(|s| s.into_bytes())
                .unwrap_or_default(),
        );

        // AppState dla handlerow — wszystkie shared resources serwera w jednym Arc.
        let meeting_manager =
            crate::meeting::MeetingManager::new(db.clone(), Some(service_manager.clone()));
        let app_state = std::sync::Arc::new(crate::dispatch::AppState {
            db: db.clone(),
            router: router.clone(),
            mesh_peer_store: mesh_peer_store.clone(),
            service_manager: service_manager.clone(),
            metrics: metrics.clone(),
            settings_cipher: settings_cipher.clone(),
            cipher: cipher.clone(),
            quic_mesh: quic_mesh.clone(),
            local_node_id: local_node_id.clone(),
            mesh_security: mesh_security.clone(),
            permission_checker: permission_checker.clone(),
            addon_manager: addon_manager.clone(),
            license: license.clone(),
            meeting_manager,
            vnc_tunnels: std::sync::Arc::new(dashmap::DashMap::new()),
            mesh_relay_health: mesh_relay_health.clone(),
            port_allocator: port_allocator.clone(),
            mesh_services_registry: mesh_services_registry.clone(),
            live_handles: service_manager.live_handles.clone(),
        });

        let upgrade = hyper::upgrade::on(&mut req);

        tokio::spawn(async move {
            match upgrade.await {
                Ok(upgraded) => {
                    let io = TokioIo::new(upgraded);
                    super::ws_binary::handle_ws_connection(
                        io,
                        user_id,
                        role,
                        resume_secret,
                        app_state,
                    )
                    .await;
                }
                Err(e) => {
                    error!("Blad WebSocket upgrade (binary): {}", e);
                }
            }
        });

        let mut ws_resp = Response::builder()
            .status(StatusCode::SWITCHING_PROTOCOLS)
            .header("Upgrade", "websocket")
            .header("Connection", "Upgrade")
            .header("Sec-WebSocket-Accept", accept);
        if let Some(ref proto) = ws_subprotocol {
            ws_resp = ws_resp.header("Sec-WebSocket-Protocol", proto.as_str());
        }
        let response = ws_resp.body(Either::Left(Full::new(Bytes::new()))).unwrap();

        return Ok(response);
    }

    // Endpointy BEZ auth
    // SSO login redirect (bez auth — uzytkownik jeszcze nie zalogowany)
    if method == Method::GET && path.starts_with("/api/sso/login/") {
        let provider_id_str = path.strip_prefix("/api/sso/login/").unwrap_or("");
        let provider_id: i64 = match provider_id_str.parse() {
            Ok(id) => id,
            Err(_) => {
                return Ok(json_error_cors(
                    400,
                    "Niepoprawne ID providera",
                    cors_origin.as_deref(),
                ))
            }
        };
        // Okresl base URL z naglowka Host
        let host = req
            .headers()
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("localhost:8080");
        let scheme = if host.contains("localhost") || host.contains("127.0.0.1") {
            "http"
        } else {
            "https"
        };
        let redirect_base = format!("{scheme}://{host}");
        let _ = req.collect().await?;
        let (status, body) = handle_result(
            api_addon_system::handle_sso_login(&db, &cipher, provider_id, &redirect_base).await,
            500,
        );
        return Ok(json_response_cors(status, body, cors_origin.as_deref()));
    }

    // SSO callback (bez auth — redirect od providera OIDC)
    if method == Method::GET && path == "/api/sso/callback" {
        let host = req
            .headers()
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("localhost:8080");
        let scheme = if host.contains("localhost") || host.contains("127.0.0.1") {
            "http"
        } else {
            "https"
        };
        let redirect_base = format!("{scheme}://{host}");
        let _ = req.collect().await?;

        // Obsluga bledow — jesli Microsoft zwrocil blad
        if let Some(error) = query_string.split('&').find_map(|p| {
            let mut kv = p.splitn(2, '=');
            if kv.next() == Some("error") {
                kv.next().map(|v| v.to_string())
            } else {
                None
            }
        }) {
            let error_desc = query_string
                .split('&')
                .find_map(|p| {
                    let mut kv = p.splitn(2, '=');
                    if kv.next() == Some("error_description") {
                        kv.next()
                            .map(|v| urlencoding::decode(v).unwrap_or_default().to_string())
                    } else {
                        None
                    }
                })
                .unwrap_or_default();
            warn!("SSO callback blad: {} — {}", error, error_desc);
            return Ok(json_error_cors(
                400,
                &format!("Blad SSO: {} — {}", error, error_desc),
                cors_origin.as_deref(),
            ));
        }

        match api_addon_system::handle_sso_callback(
            &db,
            &cipher,
            &query_string,
            &redirect_base,
            &settings_cipher,
        )
        .await
        {
            Ok((_, body)) => {
                // Parsuj odpowiedz zeby wyciagnac redirect_url
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&body) {
                    if let Some(redirect_url) = parsed.get("redirect_url").and_then(|v| v.as_str())
                    {
                        // HTTP 302 redirect do dashboardu z tokenem
                        let response = Response::builder()
                            .status(StatusCode::FOUND)
                            .header("Location", redirect_url)
                            .body(Either::Left(Full::new(Bytes::new())))
                            .unwrap();
                        return Ok(response);
                    }
                }
                return Ok(json_response_cors(200, body, cors_origin.as_deref()));
            }
            Err(e) => {
                warn!("Blad SSO callback: {}", e);
                tracing::error!("Blad SSO callback: {}", e);
                return Ok(json_error_cors(
                    500,
                    "Wewnetrzny blad serwera",
                    cors_origin.as_deref(),
                ));
            }
        }
    }

    // Nowy OAuth addon callback (binary protocol) — GET /oauth/addon/callback?code=...&state=...
    // Zwraca HTML z postMessage do window.opener (popup flow).
    if method == Method::GET && path == "/oauth/addon/callback" {
        let _ = req.collect().await?;
        let result = super::oauth_addon_callback::handle_callback(&db, &query_string).await;
        let html = super::oauth_addon_callback::render_html(&result);
        // Twardy zestaw naglowkow bezpieczenstwa: blokada iframe, CSP, brak cache, brak referrera.
        let response = Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/html; charset=utf-8")
            .header("Cache-Control", "no-store")
            .header("Pragma", "no-cache")
            .header("X-Frame-Options", "DENY")
            .header(
                "Content-Security-Policy",
                "default-src 'none'; script-src 'unsafe-inline'; frame-ancestors 'none'",
            )
            .header("Referrer-Policy", "no-referrer")
            .body(Either::Left(Full::new(Bytes::from(html))))
            .unwrap();
        return Ok(response);
    }

    // Addon OAuth callback (bez auth — redirect od providera OAuth, np. Microsoft Teams)
    if method == Method::GET
        && path.starts_with("/api/addons/")
        && path.ends_with("/oauth/callback")
    {
        let _ = req.collect().await?;
        let (status, body) = handle_result(
            api_addon_system::handle_addon_oauth_callback(&db, &cipher, &path, &query_string).await,
            500,
        );
        // Jesli callback zwrocil redirect_url — zrob HTTP redirect
        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&body) {
            if let Some(redirect_url) = parsed.get("redirect_url").and_then(|v| v.as_str()) {
                let response = Response::builder()
                    .status(StatusCode::FOUND)
                    .header("Location", redirect_url)
                    .body(Either::Left(Full::new(Bytes::new())))
                    .unwrap();
                return Ok(response);
            }
        }
        return Ok(json_response_cors(status, body, cors_origin.as_deref()));
    }

    // Addon OAuth login — wymaga auth; obsluzony w bloku z JWT ponizej.

    // Service-to-Core frame pickup — services authenticate via X-Pickup-Token
    // (HMAC, scoped, one-shot) rather than JWT. Must be reachable WITHOUT the
    // dashboard's auth gate. See `api::frame_pickup`.
    if method == Method::POST && path == "/core/frame/pickup" {
        use crate::api::frame_pickup::{
            handle_pickup, PickupOutcome, PickupRequest, HDR_FRAME_HEIGHT, HDR_FRAME_PIXEL_FORMAT,
            HDR_FRAME_PTS, HDR_FRAME_REF, HDR_FRAME_TS_MS, HDR_FRAME_WIDTH, HDR_PICKUP_TOKEN,
            HDR_REQUEST_ID, HDR_SERVICE_ID,
        };
        let hdr = |name: &str| -> Option<String> {
            req.headers().get(name).and_then(|v| v.to_str().ok()).map(|s| s.to_string())
        };
        let token = hdr(HDR_PICKUP_TOKEN);
        let frame_ref = hdr(HDR_FRAME_REF);
        let service_id = hdr(HDR_SERVICE_ID);
        let request_id = hdr(HDR_REQUEST_ID);
        // Unauth endpoint — reject oversized bodies before reading them.
        // Pickup handler ignores body entirely; 1 KiB is a safety margin.
        const PICKUP_BODY_LIMIT: u64 = 1024;
        let content_length: u64 = req
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        if content_length > PICKUP_BODY_LIMIT {
            return Ok(Response::builder()
                .status(StatusCode::PAYLOAD_TOO_LARGE)
                .header("Content-Type", "application/json")
                .body(Either::Left(Full::new(Bytes::from_static(
                    b"{\"error\":\"payload_too_large\"}",
                ))))
                .unwrap());
        }
        let body = req.collect().await?.to_bytes();
        if body.len() as u64 > PICKUP_BODY_LIMIT {
            return Ok(Response::builder()
                .status(StatusCode::PAYLOAD_TOO_LARGE)
                .header("Content-Type", "application/json")
                .body(Either::Left(Full::new(Bytes::from_static(
                    b"{\"error\":\"payload_too_large\"}",
                ))))
                .unwrap());
        }

        let pr = PickupRequest {
            pickup_token: token.as_deref(),
            frame_ref: frame_ref.as_deref(),
            service_id: service_id.as_deref(),
            request_id: request_id.as_deref(),
        };
        let issuer = crate::services::pickup_token_issuer();
        let storage = crate::services::frame_storage();
        let outcome = handle_pickup(pr, issuer, storage, &db);
        let status = outcome.http_status();
        match outcome {
            PickupOutcome::Ok {
                bytes,
                width,
                height,
                pixel_format,
                timestamp_unix_ms,
                pts,
            } => {
                let mut builder = Response::builder()
                    .status(status)
                    .header("Content-Type", "application/octet-stream")
                    .header(HDR_FRAME_WIDTH, width.to_string())
                    .header(HDR_FRAME_HEIGHT, height.to_string())
                    .header(HDR_FRAME_PIXEL_FORMAT, pixel_format)
                    .header(HDR_FRAME_TS_MS, timestamp_unix_ms.to_string());
                if let Some(p) = pts {
                    builder = builder.header(HDR_FRAME_PTS, p.to_string());
                }
                let body = Bytes::copy_from_slice(&bytes);
                let resp = builder.body(Either::Left(Full::new(body))).unwrap();
                return Ok(resp);
            }
            PickupOutcome::BadHeaders(why)
            | PickupOutcome::HeaderMismatch(why) => {
                let body = format!("{{\"error\":\"{}\"}}", why);
                return Ok(Response::builder()
                    .status(status)
                    .header("Content-Type", "application/json")
                    .body(Either::Left(Full::new(Bytes::from(body))))
                    .unwrap());
            }
            PickupOutcome::Unauthorized(_) | PickupOutcome::FramePurged => {
                return Ok(Response::builder()
                    .status(status)
                    .header("Content-Type", "application/json")
                    .body(Either::Left(Full::new(Bytes::from_static(b"{\"error\":\"pickup_denied\"}"))))
                    .unwrap());
            }
        }
    }

    // GET /frames/<ref>?token=&exp=&ref= — addon-facing multi-use signed URL
    // for raw RGB24 frames out of `services::frame_storage`. Authenticated by
    // HMAC token only (no JWT, no cookies, no CSRF surface).
    if method == Method::GET && path.starts_with("/frames/") && path.len() > "/frames/".len() {
        use crate::api::frames::{
            handle_frame_url, parse_query, FrameOutcome, HDR_FRAME_HEIGHT, HDR_FRAME_PIXEL_FORMAT,
            HDR_FRAME_PTS, HDR_FRAME_TS_MS, HDR_FRAME_WIDTH,
        };
        if let Err(resp) = reject_unauth_get_body(req.headers()) {
            return Ok(resp);
        }
        drop(req);
        let path_ref = path.strip_prefix("/frames/").unwrap_or("");
        let q = match parse_query(&query_string) {
            Ok(q) => q,
            Err(why) => {
                let body = format!("{{\"error\":\"{}\"}}", why);
                return Ok(Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .header("Content-Type", "application/json")
                    .body(Either::Left(Full::new(Bytes::from(body))))
                    .unwrap());
            }
        };
        let issuer = crate::services::frame_url_issuer();
        let storage = crate::services::frame_storage();
        let outcome = handle_frame_url(path_ref, &q, issuer, storage, &db);
        let status = outcome.http_status();
        match outcome {
            FrameOutcome::Ok {
                bytes,
                width,
                height,
                pixel_format,
                timestamp_unix_ms,
                pts,
            } => {
                let mut builder = Response::builder()
                    .status(status)
                    .header("Content-Type", "application/octet-stream")
                    .header(HDR_FRAME_WIDTH, width.to_string())
                    .header(HDR_FRAME_HEIGHT, height.to_string())
                    .header(HDR_FRAME_PIXEL_FORMAT, pixel_format)
                    .header(HDR_FRAME_TS_MS, timestamp_unix_ms.to_string());
                if let Some(p) = pts {
                    builder = builder.header(HDR_FRAME_PTS, p.to_string());
                }
                let body = Bytes::copy_from_slice(&bytes);
                return Ok(builder.body(Either::Left(Full::new(body))).unwrap());
            }
            FrameOutcome::BadRequest(why) => {
                let body = format!("{{\"error\":\"{}\"}}", why);
                return Ok(Response::builder()
                    .status(status)
                    .header("Content-Type", "application/json")
                    .body(Either::Left(Full::new(Bytes::from(body))))
                    .unwrap());
            }
            FrameOutcome::Denied(_) | FrameOutcome::NotFound => {
                return Ok(Response::builder()
                    .status(status)
                    .header("Content-Type", "application/json")
                    .body(Either::Left(Full::new(Bytes::from_static(
                        b"{\"error\":\"frame_denied\"}",
                    ))))
                    .unwrap());
            }
        }
    }

    // GET /recordings/<ref>?token=&exp=&ref= — addon-facing signed URL for
    // snapshot PNG / segment MP4. HMAC-only auth, exactly like /frames/.
    // Wired under `feature = "camera"` because the recording subsystem
    // (snapshot encoder + segment muxer + DB row helpers) is camera-gated.
    #[cfg(feature = "camera")]
    if method == Method::GET && path.starts_with("/recordings/") && path.len() > "/recordings/".len() {
        use crate::api::recording::{
            handle_recording_url, parse_query, read_recording_file, RecordingFileOutcome,
            RecordingOutcome,
        };
        if let Err(resp) = reject_unauth_get_body(req.headers()) {
            return Ok(resp);
        }
        drop(req);
        let path_ref = path.strip_prefix("/recordings/").unwrap_or("");
        let q = match parse_query(&query_string) {
            Ok(q) => q,
            Err(why) => {
                let body = format!("{{\"error\":\"{}\"}}", why);
                return Ok(Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .header("Content-Type", "application/json")
                    .body(Either::Left(Full::new(Bytes::from(body))))
                    .unwrap());
            }
        };
        let issuer = crate::services::recording_url_issuer();
        let outcome = handle_recording_url(path_ref, &q, issuer, &db);
        let auth_status = outcome.http_status();
        match outcome {
            RecordingOutcome::Ok {
                content_type,
                hash_sha256,
                created_at,
                file_size_bytes,
                file_path,
                retention_class,
                owner_addon_id,
            } => {
                let file_outcome = read_recording_file(
                    &db,
                    path_ref,
                    &file_path,
                    &retention_class,
                    &owner_addon_id,
                    file_size_bytes,
                )
                .await;
                let status = file_outcome.http_status();
                return match file_outcome {
                    RecordingFileOutcome::Ok { bytes } => Ok(Response::builder()
                        .status(status)
                        .header("Content-Type", content_type)
                        .header("X-Recording-Hash", hash_sha256)
                        .header("X-Recording-Created-At", created_at.to_string())
                        .body(Either::Left(Full::new(Bytes::from(bytes))))
                        .unwrap()),
                    _ => Ok(Response::builder()
                        .status(status)
                        .header("Content-Type", "application/json")
                        .body(Either::Left(Full::new(Bytes::from_static(
                            b"{\"error\":\"recording_unavailable\"}",
                        ))))
                        .unwrap()),
                };
            }
            RecordingOutcome::BadRequest(why) => {
                let body = format!("{{\"error\":\"{}\"}}", why);
                return Ok(Response::builder()
                    .status(400)
                    .header("Content-Type", "application/json")
                    .body(Either::Left(Full::new(Bytes::from(body))))
                    .unwrap());
            }
            RecordingOutcome::Denied(_)
            | RecordingOutcome::NotFound
            | RecordingOutcome::InternalError(_) => {
                return Ok(Response::builder()
                    .status(auth_status)
                    .header("Content-Type", "application/json")
                    .body(Either::Left(Full::new(Bytes::from_static(
                        b"{\"error\":\"recording_denied\"}",
                    ))))
                    .unwrap());
            }
        }
    }

    // Pliki statyczne - sciezki poza /api/
    if method == Method::GET && !path.starts_with("/api/") {
        let (status, content_type, body) = static_files::serve(&path);
        return Ok(make_response_with_origin(
            status,
            content_type,
            body,
            cors_origin.as_deref(),
        ));
    }

    // Wszystkie /api/* (oprocz login) wymagaja JWT
    let claims = if path.starts_with("/api/") {
        let jwt_secret =
            match db::repository::get_setting_secure(&db, "jwt_secret", &settings_cipher) {
                Ok(Some(s)) => s,
                _ => {
                    return Ok(json_error_cors(
                        500,
                        "Brak jwt_secret w konfiguracji",
                        cors_origin.as_deref(),
                    ))
                }
            };

        let token = match extract_bearer_token(&req) {
            Some(t) => t,
            None => {
                return Ok(json_error_cors(
                    401,
                    "Brak tokenu autoryzacji",
                    cors_origin.as_deref(),
                ))
            }
        };

        match auth::validate_jwt(token, &jwt_secret) {
            Ok(c) => Some(c),
            Err(_) => {
                return Ok(json_error_cors(
                    401,
                    "Niepoprawny lub wygasniety token",
                    cors_origin.as_deref(),
                ))
            }
        }
    } else {
        None
    };

    // Routuj endpointy wymagajace auth
    let claims = match claims {
        Some(c) => c,
        None => {
            return Ok(json_error_cors(
                401,
                "Wymagana autoryzacja",
                cors_origin.as_deref(),
            ));
        }
    };

    // Walidacja Content-Type dla POST/PUT
    if method == Method::POST || method == Method::PUT {
        let content_type = req
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !content_type.contains("application/json") {
            let _ = req.collect().await?;
            return Ok(json_error_cors(
                415,
                "Wymagany Content-Type: application/json",
                cors_origin.as_deref(),
            ));
        }
    }

    // Pobierz body dla POST/PUT
    let body_bytes = if method == Method::POST || method == Method::PUT {
        req.collect().await?.to_bytes()
    } else {
        // Musimy skonsumowac body nawet dla GET/DELETE
        let _ = req.collect().await?;
        Bytes::new()
    };

    // Addon OAuth login (wymaga auth — musimy znac user_id)
    if method == Method::GET && path.starts_with("/api/addons/") && path.ends_with("/oauth/login") {
        let addon_id = path
            .strip_prefix("/api/addons/")
            .and_then(|rest| rest.strip_suffix("/oauth/login"))
            .unwrap_or("");
        if !addon_id.is_empty() {
            let (status, response_body) = handle_result(
                api_addon_system::handle_addon_oauth_login(&db, &claims, addon_id).await,
                500,
            );
            // Jesli auth_url — redirect przegladarki
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&response_body) {
                if let Some(auth_url) = parsed.get("auth_url").and_then(|v| v.as_str()) {
                    let response = Response::builder()
                        .status(StatusCode::FOUND)
                        .header("Location", auth_url)
                        .body(Either::Left(Full::new(Bytes::new())))
                        .unwrap();
                    return Ok(response);
                }
            }
            return Ok(json_response_cors(
                status,
                response_body,
                cors_origin.as_deref(),
            ));
        }
    }

    let (status, response_body) = route_api(
        &method,
        &path,
        &db,
        &claims,
        &body_bytes,
        port_allocator.clone(),
    )
    .await;

    Ok(json_response_cors(
        status,
        response_body,
        cors_origin.as_deref(),
    ))
}

/// Routuje endpointy /api/* do odpowiednich handlerow
async fn route_api(
    _method: &Method,
    _path: &str,
    _db: &DbPool,
    _claims: &auth::Claims,
    _body: &[u8],
    _port_allocator: Option<Arc<crate::services::ports::PortAllocator>>,
) -> (u16, String) {
    (404, r#"{"error":"Endpoint nie znaleziony"}"#.to_string())
}

/// Oblicza Sec-WebSocket-Accept z Sec-WebSocket-Key (RFC 6455)
fn compute_ws_accept(key: &str) -> String {
    tokio_tungstenite::tungstenite::handshake::derive_accept_key(key.as_bytes())
}

/// Waliduje WebSocket upgrade: sprawdza naglowek upgrade, JWT z naglowka
/// Sec-WebSocket-Protocol (subprotocol auth: bearer.<token>), sec-websocket-key.
/// VULN-007: Token TYLKO z Sec-WebSocket-Protocol — unikaj wycieku w logach/query string.
/// Zwraca (ws_key, ws_accept, subprotocol) lub gotowy error response.
/// subprotocol musi byc odzwierciedlony w odpowiedzi WebSocket (RFC 6455).
fn validate_ws_upgrade(
    req: &Request<Incoming>,
    db: &DbPool,
    _query_string: &str,
    cors_origin: Option<&str>,
    settings_cipher: &crate::crypto::SettingsCipher,
) -> Result<(String, String, Option<String>), Response<DashboardBody>> {
    let is_upgrade = req
        .headers()
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);

    if !is_upgrade {
        return Err(json_error_cors(
            400,
            "Wymagany WebSocket upgrade",
            cors_origin,
        ));
    }

    let jwt_secret = match db::repository::get_setting_secure(db, "jwt_secret", settings_cipher) {
        Ok(Some(s)) => s,
        _ => {
            return Err(json_error_cors(
                500,
                "Brak jwt_secret w konfiguracji",
                cors_origin,
            ))
        }
    };

    // TYLKO z naglowka Sec-WebSocket-Protocol (format: bearer.TOKEN)
    let proto_header = req
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_string());

    let subprotocol = proto_header
        .as_deref()
        .and_then(|v| v.split(',').find(|s| s.trim().starts_with("bearer.")))
        .map(|s| s.trim().to_string());

    let ws_token = subprotocol
        .as_deref()
        .and_then(|s| s.strip_prefix("bearer."))
        .map(|s| s.to_string());

    match ws_token {
        Some(ref t) if auth::validate_jwt(t, &jwt_secret).is_ok() => {}
        _ => {
            return Err(json_error_cors(
                401,
                "Brak lub niepoprawny token autoryzacji",
                cors_origin,
            ))
        }
    }

    let ws_key = match req.headers().get("sec-websocket-key") {
        Some(key) => key.to_str().unwrap_or("").to_string(),
        None => return Err(json_error_cors(400, "Brak Sec-WebSocket-Key", cors_origin)),
    };

    let accept = compute_ws_accept(&ws_key);
    Ok((ws_key, accept, subprotocol))
}

/// Walidacja WS upgrade dla `/ws/api` — pozwala anonymous (login flow musi
/// zlozyc WS bez JWT zeby wyslac AuthLoginRequest). Auth-aware policy check
/// dzieje sie potem per-handler.
fn validate_ws_upgrade_optional_auth(
    req: &Request<Incoming>,
    db: &DbPool,
    cors_origin: Option<&str>,
    settings_cipher: &crate::crypto::SettingsCipher,
) -> Result<(String, String, Option<String>), Response<DashboardBody>> {
    let is_upgrade = req
        .headers()
        .get("upgrade")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    if !is_upgrade {
        return Err(json_error_cors(
            400,
            "Wymagany WebSocket upgrade",
            cors_origin,
        ));
    }

    let proto_header = req
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.to_string());

    let subprotocol = proto_header
        .as_deref()
        .and_then(|v| v.split(',').find(|s| s.trim().starts_with("bearer.")))
        .map(|s| s.trim().to_string());

    // Jesli token podany — zwaliduj. Brak tokena = anonymous OK.
    if let Some(sub) = subprotocol.as_deref() {
        if let Some(token) = sub.strip_prefix("bearer.") {
            let jwt_secret =
                match db::repository::get_setting_secure(db, "jwt_secret", settings_cipher) {
                    Ok(Some(s)) => s,
                    _ => {
                        return Err(json_error_cors(
                            500,
                            "Brak jwt_secret w konfiguracji",
                            cors_origin,
                        ))
                    }
                };
            if auth::validate_jwt(token, &jwt_secret).is_err() {
                return Err(json_error_cors(401, "Niepoprawny token", cors_origin));
            }
        }
    }

    let ws_key = match req.headers().get("sec-websocket-key") {
        Some(key) => key.to_str().unwrap_or("").to_string(),
        None => return Err(json_error_cors(400, "Brak Sec-WebSocket-Key", cors_origin)),
    };

    let accept = compute_ws_accept(&ws_key);
    Ok((ws_key, accept, subprotocol))
}

/// Wyciaga (user_id, role) z JWT subprotokolu Sec-WebSocket-Protocol: bearer.<token>
/// + DB lookup dla role (Zero Trust — JWT nie nosi role per VULN-004).
/// Wolane PO `validate_ws_upgrade` (ktore juz zweryfikowalo token) — tu tylko
/// reparsujemy claims i wzbogacamy o role z DB.
/// Zwraca None gdy nie udalo sie extract (degraduje do anonymous session).
fn extract_ws_user_session(
    req: &Request<Incoming>,
    db: &DbPool,
    settings_cipher: &crate::crypto::SettingsCipher,
) -> Option<(i64, Option<String>)> {
    let jwt_secret = db::repository::get_setting_secure(db, "jwt_secret", settings_cipher)
        .ok()
        .flatten()?;

    let proto_header = req
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())?;

    let token = proto_header
        .split(',')
        .find(|s| s.trim().starts_with("bearer."))
        .and_then(|s| s.trim().strip_prefix("bearer."))?;

    let claims = auth::validate_jwt(token, &jwt_secret).ok()?;

    // Zero Trust: role z DB lookup, nie z JWT (chroni przed token-replay z
    // odebranymi uprawnieniami).
    let role = db::repository::get_user_account_by_id(db, claims.user_id)
        .ok()
        .flatten()
        .map(|acc| {
            if acc.is_admin {
                "admin".to_string()
            } else {
                "user".to_string()
            }
        });

    Some((claims.user_id, role))
}

