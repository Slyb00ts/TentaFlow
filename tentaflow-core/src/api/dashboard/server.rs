// =============================================================================
// Plik: api/dashboard/server.rs
// Opis: HTTP server dashboardu - routing, middleware JWT auth, CORS.
// =============================================================================

use super::{
    api_addon_system, api_apikeys, api_auth, api_clusters, api_dashboard, api_deploy_recommend,
    api_fast_path, api_flows, api_hub, api_me_preferences, api_models, api_pii_rules, api_prompts,
    api_registries, api_services, api_tts_rules, auth, static_files,
};
use crate::db::{self, DbPool};
use crate::license::{LicenseChecker, StaticLicenseChecker};
use crate::mesh::peer_store::MeshPeerStore;
use crate::metrics::RouterMetrics;
use crate::routing::service_manager::ServiceManager;
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
    license: Arc<dyn LicenseChecker>,
    mesh_relay_health: Option<Arc<parking_lot::RwLock<crate::mesh::relay_health::RelayHealth>>>,
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
            license: Arc::new(StaticLicenseChecker::free()),
            mesh_relay_health: None,
        }
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
        let license = self.license.clone();
        let mesh_relay_health = self.mesh_relay_health.clone();

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
            let lic_clone = license.clone();
            let mrh_clone = mesh_relay_health.clone();
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
                    let lic = lic_clone.clone();
                    let mrh = mrh_clone.clone();
                    let ra = remote_addr_str.clone();
                    async move {
                        handle_request(
                            req, db, metrics, cipher, sc, sm, router, mps, qm, lni, msec, pc, lic,
                            mrh, ra,
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

/// Wyciaga Bearer token z naglowka Authorization
fn extract_bearer_token(req: &Request<Incoming>) -> Option<&str> {
    req.headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

/// Wyciaga segment :id ze sciezki (np. /api/services/5 -> Some(5))
fn extract_id_from_path(path: &str, prefix: &str) -> Option<i64> {
    path.strip_prefix(prefix)
        .and_then(|rest| rest.trim_matches('/').parse().ok())
}

/// Wyciaga id serwisu ze sciezki /api/services/:id/backends
fn extract_service_id_for_backends(path: &str) -> Option<i64> {
    let stripped = path.strip_prefix("/api/services/")?;
    let id_str = stripped.strip_suffix("/backends")?;
    id_str.parse().ok()
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
    license: Arc<dyn LicenseChecker>,
    mesh_relay_health: Option<Arc<parking_lot::RwLock<crate::mesh::relay_health::RelayHealth>>>,
    remote_addr: String,
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
        || path.contains("/sso/callback");
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

    // WebSocket upgrade /ws/deploy
    if method == Method::GET && path == "/ws/deploy" {
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
        let db_clone = db.clone();
        let settings_cipher_clone = settings_cipher.clone();
        let router_clone = router.clone();
        let lni_clone = local_node_id.clone();

        tokio::spawn(async move {
            match upgrade.await {
                Ok(upgraded) => {
                    let io = TokioIo::new(upgraded);
                    super::ws_deploy::handle_ws_connection(
                        io,
                        db_clone,
                        settings_cipher_clone,
                        router_clone,
                        lni_clone,
                    )
                    .await;
                }
                Err(e) => {
                    error!("Blad WebSocket upgrade (deploy): {}", e);
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
            license: license.clone(),
            meeting_manager,
            vnc_tunnels: std::sync::Arc::new(dashmap::DashMap::new()),
            mesh_relay_health: mesh_relay_health.clone(),
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
    if method == Method::POST && path == "/api/auth/login" {
        let body_bytes = req.collect().await?.to_bytes();
        // VULN-035: Przekaz remote_addr do handle_login (dual rate limiting per IP)
        let (status, body) =
            match api_auth::handle_login(&db, &body_bytes, &remote_addr, &settings_cipher) {
                Ok(r) => r,
                Err(e) => {
                    warn!("Blad logowania: {}", e);
                    (500, r#"{"error":"Wewnetrzny blad serwera"}"#.to_string())
                }
            };
        return Ok(json_response_cors(status, body, cors_origin.as_deref()));
    }

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

    // Addon OAuth login — inicjacja flow (wymaga auth, bo musimy wiedziec kto sie loguje)
    if method == Method::GET && path.starts_with("/api/addons/") && path.ends_with("/oauth/login") {
        // Ten endpoint wymaga JWT — zostanie obsluzony w route_addon_system_api ponizej
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

    // Probe SSE stream — PRZED auth check bo EventSource nie moze slac headerow
    // Walidacja jednorazowym tokenem SSE (nie JWT) — token zwrocony z POST /api/clusters/probe
    if path.starts_with("/api/clusters/probe/") && method == Method::GET {
        let sse_token = query_string
            .split('&')
            .find(|p| p.starts_with("token="))
            .and_then(|p| p.strip_prefix("token="))
            .unwrap_or("");
        if sse_token.is_empty() {
            return Ok(json_error_cors(
                401,
                "Brak tokenu SSE",
                cors_origin.as_deref(),
            ));
        }

        let probe_id = path
            .strip_prefix("/api/clusters/probe/")
            .unwrap_or("")
            .trim_matches('/');
        if !probe_id.is_empty() {
            if let Some(rx) =
                super::api_clusters::handle_probe_stream_with_token(probe_id, sse_token).await
            {
                let sse_stream = futures::stream::unfold(rx, |mut rx| async {
                    let msg = rx.recv().await?;
                    Some((Ok(Frame::data(Bytes::from(msg))), rx))
                });
                let boxed_stream: SseStream = Box::pin(sse_stream);
                let mut builder = Response::builder()
                    .status(StatusCode::OK)
                    .header("Content-Type", "text/event-stream")
                    .header("Cache-Control", "no-cache")
                    .header("Connection", "keep-alive");
                if let Some(ref origin) = cors_origin {
                    builder = builder
                        .header("Access-Control-Allow-Origin", origin.as_str())
                        .header(
                            "Access-Control-Allow-Methods",
                            "GET, POST, PUT, DELETE, OPTIONS",
                        )
                        .header(
                            "Access-Control-Allow-Headers",
                            "Content-Type, Authorization",
                        );
                }
                return Ok(builder
                    .body(Either::Right(StreamBody::new(boxed_stream)))
                    .unwrap());
            }
            return Ok(json_error_cors(
                404,
                "Probe stream nie istnieje",
                cors_origin.as_deref(),
            ));
        }
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

    // Registries API (async - handle_test jest async)
    if path.starts_with("/api/registries") {
        let (status, response_body) =
            route_registries_api(&method, &path, &db, &cipher, &body_bytes, &claims).await;
        return Ok(json_response_cors(
            status,
            response_body,
            cors_origin.as_deref(),
        ));
    }

// Clusters API — CRUD clusterow i czlonkostwa
    if path.starts_with("/api/clusters") {
        let (status, response_body) =
            route_clusters_api(&method, &path, &db, &body_bytes, &claims, &quic_mesh).await;
        return Ok(json_response_cors(
            status,
            response_body,
            cors_origin.as_deref(),
        ));
    }

    // Status QUIC serwisow
    if path == "/api/services/status" && method == Method::GET {
        let status = service_manager.get_service_status().await;
        let resp_body = serde_json::to_string(&status).unwrap_or("{}".to_string());
        return Ok(json_response_cors(200, resp_body, cors_origin.as_deref()));
    }

    // Model Pool API - podglad i zmiana strategii load-balancing per model
    if path == "/api/models/pool" && method == Method::GET {
        let pool_info = service_manager.get_model_pool_info();
        let models: Vec<_> = pool_info
            .iter()
            .map(|(name, services, strategy, service_type)| {
                serde_json::json!({
                    "model_name": name,
                    "services": services,
                    "strategy": strategy,
                    "service_type": service_type,
                })
            })
            .collect();
        let resp_body = serde_json::json!({"models": models}).to_string();
        return Ok(json_response_cors(200, resp_body, cors_origin.as_deref()));
    }
    // Ustawienie listy serwisow dla modelu w puli (VULN-008: admin only)
    if path.starts_with("/api/models/") && path.ends_with("/services") && method == Method::PUT {
        if require_admin(&claims, &db).is_some() {
            return Ok(json_error_cors(
                403,
                "Brak uprawnien administratora",
                cors_origin.as_deref(),
            ));
        }
        let model_name = path
            .strip_prefix("/api/models/")
            .and_then(|rest| rest.strip_suffix("/services"));
        if let Some(model_name) = model_name {
            let model_name = model_name.to_string();

            #[derive(serde::Deserialize)]
            struct SetServices {
                services: Vec<String>,
            }

            let payload: SetServices = match serde_json::from_slice(&body_bytes) {
                Ok(p) => p,
                Err(e) => {
                    return Ok(json_error_cors(
                        400,
                        &format!("Blad parsowania: {}", e),
                        cors_origin.as_deref(),
                    ))
                }
            };
            service_manager.set_model_services(&model_name, payload.services);
            let resp_body = serde_json::json!({"ok": true}).to_string();
            return Ok(json_response_cors(200, resp_body, cors_origin.as_deref()));
        }
    }
    if path.starts_with("/api/models/") && path.ends_with("/strategy") && method == Method::PUT {
        if require_admin(&claims, &db).is_some() {
            return Ok(json_error_cors(
                403,
                "Brak uprawnien administratora",
                cors_origin.as_deref(),
            ));
        }
        let model_name = path
            .strip_prefix("/api/models/")
            .and_then(|rest| rest.strip_suffix("/strategy"));
        if let Some(model_name) = model_name {
            let model_name = model_name.to_string();

            #[derive(serde::Deserialize)]
            struct SetStrategy {
                strategy: String,
            }

            let payload: SetStrategy = match serde_json::from_slice(&body_bytes) {
                Ok(p) => p,
                Err(e) => {
                    return Ok(json_error_cors(
                        400,
                        &format!("Blad parsowania: {}", e),
                        cors_origin.as_deref(),
                    ))
                }
            };
            let strategy = match payload.strategy.as_str() {
                "round_robin" => crate::routing::service_manager::PoolStrategy::RoundRobin,
                "least_loaded" => crate::routing::service_manager::PoolStrategy::LeastLoaded,
                _ => {
                    return Ok(json_error_cors(
                        400,
                        "Nieznana strategia. Dostepne: round_robin, least_loaded",
                        cors_origin.as_deref(),
                    ))
                }
            };
            if service_manager.set_model_strategy(&model_name, strategy) {
                let resp_body = serde_json::json!({"ok": true}).to_string();
                return Ok(json_response_cors(200, resp_body, cors_origin.as_deref()));
            } else {
                return Ok(json_error_cors(
                    404,
                    &format!("Model '{}' nie znaleziony w pool", model_name),
                    cors_origin.as_deref(),
                ));
            }
        }
    }

    // Deploy: vLLM VRAM calculator + smart config recommend.
    // Body: {model, gpus[], hf_token?, optional overrides}.
    // Wolane przez engine-deploy-wizard dla live recompute przy zmianie
    // modelu lub suwakow advanced settings. Endpoint async bo robi HF fetch.
    // Deploy: live limits dla suwakow GUI (max ctx, max seqs, max TP) na
    // podstawie wybranego modelu i listy GPU. GET zeby moc cache'owac, params
    // w query string.
    if method == Method::GET && path == "/api/deploy/vllm/limits" {
        if require_admin(&claims, &db).is_some() {
            return Ok(json_error_cors(
                403,
                "Brak uprawnien administratora",
                cors_origin.as_deref(),
            ));
        }
        let (status, response_body) = match api_deploy_recommend::handle_limits(&query_string).await
        {
            Ok(p) => p,
            Err(e) => (
                500,
                format!(r#"{{"error":"{}"}}"#, e.to_string().replace('"', "'")),
            ),
        };
        return Ok(json_response_cors(
            status,
            response_body,
            cors_origin.as_deref(),
        ));
    }

    if method == Method::POST && path == "/api/deploy/vllm/recommend" {
        if require_admin(&claims, &db).is_some() {
            return Ok(json_error_cors(
                403,
                "Brak uprawnien administratora",
                cors_origin.as_deref(),
            ));
        }
        let (status, response_body) = match api_deploy_recommend::handle_recommend(&body_bytes).await {
            Ok(p) => p,
            Err(e) => (
                500,
                format!(r#"{{"error":"{}"}}"#, e.to_string().replace('"', "'")),
            ),
        };
        return Ok(json_response_cors(
            status,
            response_body,
            cors_origin.as_deref(),
        ));
    }

    // Hub API — silniki, wyszukiwanie modeli HF, lokalne modele
    if path.starts_with("/api/hub/") {
        let (status, response_body) = route_hub_api(
            &method,
            &path,
            &query_string,
            &body_bytes,
            &mesh_peer_store,
            &claims,
            &db,
        )
        .await;
        return Ok(json_response_cors(
            status,
            response_body,
            cors_origin.as_deref(),
        ));
    }

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

    // Addon system API (users, groups, addons, sso management).
    // Audit log dostepny wylacznie przez binary protocol (AuditLog* variants).
    if path.starts_with("/api/users")
        || path.starts_with("/api/groups")
        || path.starts_with("/api/addons")
        || path == "/api/tools"
    {
        let (status, response_body) = route_addon_system_api(
            &method,
            &path,
            &db,
            &claims,
            &body_bytes,
            &permission_checker,
            Some(&router),
        );
        return Ok(json_response_cors(
            status,
            response_body,
            cors_origin.as_deref(),
        ));
    }

    // System capabilities — wizard startowy GUI pyta co maszyna potrafi.
    // Nie wymaga featury docker; jesli brak Dockera/Podmana to caps.deploy_backends
    // bedzie zawieralo tylko Native.
    if path == "/api/system/capabilities" && method == Method::GET {
        let caps = crate::system_check::collect();
        let body = serde_json::to_string(&caps).unwrap_or_else(|_| "{}".into());
        return Ok(json_response_cors(200, body, cors_origin.as_deref()));
    }

    // Profiling permissions: validate-sudo + collectors/status migrated to
    // binary protocol (ProfilingPayload::ValidateSudoRequest /
    // CollectorsStatusRequest). REST endpoints removed. GUI uses
    // ApiBinary.one('profilingValidateSudoRequest', ...) etc.

    // Voice profiles API — bulletproof speaker recognition.
    // Wolane przez LLM po detekcji introducji ("Cześć, tu Jan") albo przez
    // wewnetrzne toole. Na razie bez UI, fundament pod pozniejsze enrollment-by-LLM.
    #[cfg(feature = "inference-diarization")]
    if path.starts_with("/api/voice-profiles") {
        let (status, response_body) =
            crate::api::dashboard::api_voice_profiles::route_voice_profiles_api(
                &method,
                &path,
                &query_string,
                &db,
                &body_bytes,
            );
        return Ok(json_response_cors(
            status,
            response_body,
            cors_origin.as_deref(),
        ));
    }

    let (status, response_body) =
        route_api(&method, &path, &query_string, &db, &claims, &body_bytes);

    Ok(json_response_cors(
        status,
        response_body,
        cors_origin.as_deref(),
    ))
}

/// Parsuje parametr query string po nazwie, zwraca domyslna wartosc jesli brak
fn parse_query_param(query: &str, name: &str, default: i64) -> i64 {
    query
        .split('&')
        .find_map(|pair| {
            let mut parts = pair.splitn(2, '=');
            let key = parts.next()?;
            let val = parts.next()?;
            if key == name {
                val.parse().ok()
            } else {
                None
            }
        })
        .unwrap_or(default)
}

/// VULN-008: Sprawdza uprawnienia administratora w DB. Zwraca blad 403 jesli brak.
fn require_admin(claims: &auth::Claims, db: &DbPool) -> Option<(u16, String)> {
    let user_is_admin = db::repository::get_user_account_by_id(db, claims.user_id)
        .ok()
        .flatten()
        .map(|u| u.is_admin)
        .unwrap_or(false);
    if !user_is_admin {
        Some((
            403,
            r#"{"error":"Brak uprawnien administratora"}"#.to_string(),
        ))
    } else {
        None
    }
}

/// Routuje endpointy /api/* do odpowiednich handlerow
fn route_api(
    method: &Method,
    path: &str,
    query: &str,
    db: &DbPool,
    claims: &auth::Claims,
    body: &[u8],
) -> (u16, String) {
    match (method, path) {
        // Auth
        (&Method::GET, "/api/auth/me") => handle_result(api_auth::handle_me(claims), 500),
        (&Method::POST, "/api/auth/change-password") => {
            handle_result(api_auth::handle_change_password(db, claims, body), 400)
        }

        // Preferencje uzytkownika (preferowany jezyk dla TTS itd.)
        (&Method::GET, "/api/me/preferences") => {
            handle_result(api_me_preferences::handle_get(db, claims), 500)
        }
        (&Method::PUT, "/api/me/preferences") => {
            handle_result(api_me_preferences::handle_put(db, claims, body), 400)
        }

        // Dashboard
        (&Method::GET, "/api/dashboard") => handle_result(api_dashboard::handle_overview(db), 500),

        // Services
        (&Method::GET, "/api/services") => handle_result(api_services::handle_list(db), 500),
        (&Method::POST, "/api/services") => {
            if let Some(err) = require_admin(claims, db) {
                return err;
            }
            handle_result(api_services::handle_create(db, body), 400)
        }


        // API Keys
        (&Method::GET, "/api/apikeys") => handle_result(api_apikeys::handle_list(db), 500),
        (&Method::POST, "/api/apikeys") => {
            if let Some(err) = require_admin(claims, db) {
                return err;
            }
            handle_result(api_apikeys::handle_create(db, body), 400)
        }

        // Prompts
        (&Method::GET, "/api/prompts") => {
            let offset = parse_query_param(query, "offset", 0);
            let limit = parse_query_param(query, "limit", 50);
            handle_result(api_prompts::handle_list(db, offset, limit), 500)
        }
        (&Method::POST, "/api/prompts") => {
            if let Some(err) = require_admin(claims, db) {
                return err;
            }
            handle_result(api_prompts::handle_create(db, body), 400)
        }

        // Models
        (&Method::GET, "/api/models") => {
            let offset = parse_query_param(query, "offset", 0);
            let limit = parse_query_param(query, "limit", 50);
            handle_result(api_models::handle_list_entries(db, offset, limit), 500)
        }
        (&Method::POST, "/api/models") => {
            if let Some(err) = require_admin(claims, db) {
                return err;
            }
            handle_result(api_models::handle_create_entry(db, body), 400)
        }

        // Flows — migracja do binary WS (FAZA 3). REST usuniety:
        // - GET  /api/flows                       → FlowListRequest
        // - POST /api/flows                       → FlowCreateRequest
        // - GET  /api/flows/:id                   → FlowDetailRequest
        // - PUT  /api/flows/:id                   → FlowUpdateRequest
        // - DELETE /api/flows/:id                 → FlowDeleteRequest
        // - GET/POST /api/flows/:id/versions[/:vid[/restore]] → FlowVersion{List,Get,Restore}Request
        // - GET  /api/flow-node-templates         → FlowNodeTemplatesListRequest

        // Flow Model Bindings
        (&Method::GET, "/api/flow-bindings") => {
            handle_result(api_flows::handle_list_bindings(db), 500)
        }
        (&Method::POST, "/api/flow-bindings") => {
            if let Some(err) = require_admin(claims, db) {
                return err;
            }
            handle_result(api_flows::handle_create_binding(db, body), 400)
        }

        // Flow Node Templates — GET usuniety (binary FlowNodeTemplatesListRequest).
        // POST zostaje dla admin CRUD (na razie bez frontendu).
        (&Method::POST, "/api/flow-node-templates") => {
            if let Some(err) = require_admin(claims, db) {
                return err;
            }
            handle_result(api_flows::handle_create_node_template(db, body), 400)
        }

        // Flow Executions
        (&Method::GET, "/api/flow-executions") => {
            let offset = parse_query_param(query, "offset", 0);
            let limit = parse_query_param(query, "limit", 50);
            handle_result(api_flows::handle_list_executions(db, offset, limit), 500)
        }

        // PII Rules
        (&Method::GET, "/api/pii-rules") => {
            let offset = parse_query_param(query, "offset", 0);
            let limit = parse_query_param(query, "limit", 50);
            handle_result(api_pii_rules::handle_list(db, offset, limit), 500)
        }
        (&Method::POST, "/api/pii-rules") => {
            if let Some(err) = require_admin(claims, db) {
                return err;
            }
            handle_result(api_pii_rules::handle_create(db, body), 400)
        }

        // Fast Path Patterns
        (&Method::GET, "/api/fast-path-patterns") => {
            let offset = parse_query_param(query, "offset", 0);
            let limit = parse_query_param(query, "limit", 50);
            handle_result(api_fast_path::handle_list(db, offset, limit), 500)
        }
        (&Method::POST, "/api/fast-path-patterns") => {
            if let Some(err) = require_admin(claims, db) {
                return err;
            }
            handle_result(api_fast_path::handle_create(db, body), 400)
        }

        // TTS Rules
        (&Method::GET, "/api/tts-rules") => {
            let offset = parse_query_param(query, "offset", 0);
            let limit = parse_query_param(query, "limit", 50);
            handle_result(api_tts_rules::handle_list(db, offset, limit), 500)
        }
        (&Method::POST, "/api/tts-rules") => {
            if let Some(err) = require_admin(claims, db) {
                return err;
            }
            handle_result(api_tts_rules::handle_create(db, body), 400)
        }

        _ => {
            // VULN-008: Helper — admin check dla mutujacych endpointow z :id
            let admin_err = || -> (u16, String) {
                (
                    403,
                    r#"{"error":"Brak uprawnien administratora"}"#.to_string(),
                )
            };

            // Backendy serwisow: /api/services/:id/backends
            if let Some(sid) = extract_service_id_for_backends(path) {
                return match method {
                    &Method::GET => handle_result(api_services::handle_list_backends(db, sid), 500),
                    &Method::POST => {
                        if require_admin(claims, db).is_some() {
                            return admin_err();
                        }
                        handle_result(api_services::handle_create_backend(db, sid, body), 400)
                    }
                    _ => (405, r#"{"error":"Metoda niedozwolona"}"#.to_string()),
                };
            }

            // Backendy: /api/backends/:id
            if let Some(id) = extract_id_from_path(path, "/api/backends/") {
                return match method {
                    &Method::PUT => {
                        if require_admin(claims, db).is_some() {
                            return admin_err();
                        }
                        handle_result(api_services::handle_update_backend(db, id, body), 400)
                    }
                    &Method::DELETE => {
                        if require_admin(claims, db).is_some() {
                            return admin_err();
                        }
                        handle_result(api_services::handle_delete_backend(db, id), 500)
                    }
                    _ => (405, r#"{"error":"Metoda niedozwolona"}"#.to_string()),
                };
            }

            // Sciezki z :id - services/:id, services/:id/stats, apikeys/:id
            if let Some(id) = extract_id_from_path(path, "/api/services/") {
                if path.ends_with("/stats") {
                    if *method == Method::GET {
                        return handle_result(api_services::handle_stats(db, id), 500);
                    }
                } else {
                    return match *method {
                        Method::PUT => {
                            if require_admin(claims, db).is_some() {
                                return admin_err();
                            }
                            handle_result(api_services::handle_update(db, id, body), 400)
                        }
                        Method::DELETE => {
                            if require_admin(claims, db).is_some() {
                                return admin_err();
                            }
                            handle_result(api_services::handle_delete(db, id), 500)
                        }
                        _ => (405, r#"{"error":"Method not allowed"}"#.to_string()),
                    };
                }
            }

            if let Some(id) = extract_id_from_path(path, "/api/apikeys/") {
                if *method == Method::DELETE {
                    if require_admin(claims, db).is_some() {
                        return admin_err();
                    }
                    return handle_result(api_apikeys::handle_delete(db, id), 500);
                }
            }

            // Prompts /:id
            if let Some(id) = extract_id_from_path(path, "/api/prompts/") {
                return match *method {
                    Method::GET => handle_result(api_prompts::handle_get(db, id), 500),
                    Method::PUT => {
                        if require_admin(claims, db).is_some() {
                            return admin_err();
                        }
                        handle_result(api_prompts::handle_update(db, id, body), 400)
                    }
                    Method::DELETE => {
                        if require_admin(claims, db).is_some() {
                            return admin_err();
                        }
                        handle_result(api_prompts::handle_delete(db, id), 500)
                    }
                    _ => (405, r#"{"error":"Method not allowed"}"#.to_string()),
                };
            }

            // Models /:id
            if let Some(id) = extract_id_from_path(path, "/api/models/") {
                return match *method {
                    Method::GET => handle_result(api_models::handle_get_entry(db, id), 500),
                    Method::PUT => {
                        if require_admin(claims, db).is_some() {
                            return admin_err();
                        }
                        handle_result(api_models::handle_update_entry(db, id, body), 400)
                    }
                    Method::DELETE => {
                        if require_admin(claims, db).is_some() {
                            return admin_err();
                        }
                        handle_result(api_models::handle_delete_entry(db, id), 500)
                    }
                    _ => (405, r#"{"error":"Method not allowed"}"#.to_string()),
                };
            }

            // Flow Versions + Flows /:id — zmigrowane do binary WS (FAZA 3).
            // GET/POST /api/flows/:id/versions[/:vid[/restore]] → FlowVersion{List,Get,Restore}Request
            // GET/PUT/DELETE /api/flows/:id → FlowDetailRequest / FlowUpdateRequest / FlowDeleteRequest

            // Flow Bindings /:id
            if let Some(id) = extract_id_from_path(path, "/api/flow-bindings/") {
                return match *method {
                    Method::PUT => {
                        if require_admin(claims, db).is_some() {
                            return admin_err();
                        }
                        handle_result(api_flows::handle_update_binding(db, id, body), 400)
                    }
                    Method::DELETE => {
                        if require_admin(claims, db).is_some() {
                            return admin_err();
                        }
                        handle_result(api_flows::handle_delete_binding(db, id), 500)
                    }
                    _ => (405, r#"{"error":"Method not allowed"}"#.to_string()),
                };
            }

            // Flow Node Templates /:id
            if let Some(id) = extract_id_from_path(path, "/api/flow-node-templates/") {
                return match *method {
                    Method::PUT => {
                        if require_admin(claims, db).is_some() {
                            return admin_err();
                        }
                        handle_result(api_flows::handle_update_node_template(db, id, body), 400)
                    }
                    Method::DELETE => {
                        if require_admin(claims, db).is_some() {
                            return admin_err();
                        }
                        handle_result(api_flows::handle_delete_node_template(db, id), 500)
                    }
                    _ => (405, r#"{"error":"Method not allowed"}"#.to_string()),
                };
            }

            // Flow Executions /:id
            if let Some(id) = extract_id_from_path(path, "/api/flow-executions/") {
                return match *method {
                    Method::GET => handle_result(api_flows::handle_get_execution(db, id), 500),
                    Method::DELETE => {
                        if require_admin(claims, db).is_some() {
                            return admin_err();
                        }
                        handle_result(api_flows::handle_delete_execution(db, id), 500)
                    }
                    _ => (405, r#"{"error":"Method not allowed"}"#.to_string()),
                };
            }

            // PII Rules /:id
            if let Some(id) = extract_id_from_path(path, "/api/pii-rules/") {
                return match *method {
                    Method::PUT => {
                        if require_admin(claims, db).is_some() {
                            return admin_err();
                        }
                        handle_result(api_pii_rules::handle_update(db, id, body), 400)
                    }
                    Method::DELETE => {
                        if require_admin(claims, db).is_some() {
                            return admin_err();
                        }
                        handle_result(api_pii_rules::handle_delete(db, id), 500)
                    }
                    _ => (405, r#"{"error":"Method not allowed"}"#.to_string()),
                };
            }

            // Fast Path Patterns /:id
            if let Some(id) = extract_id_from_path(path, "/api/fast-path-patterns/") {
                return match *method {
                    Method::PUT => {
                        if require_admin(claims, db).is_some() {
                            return admin_err();
                        }
                        handle_result(api_fast_path::handle_update(db, id, body), 400)
                    }
                    Method::DELETE => {
                        if require_admin(claims, db).is_some() {
                            return admin_err();
                        }
                        handle_result(api_fast_path::handle_delete(db, id), 500)
                    }
                    _ => (405, r#"{"error":"Method not allowed"}"#.to_string()),
                };
            }

            // TTS Rules /:id
            if let Some(id) = extract_id_from_path(path, "/api/tts-rules/") {
                return match *method {
                    Method::PUT => {
                        if require_admin(claims, db).is_some() {
                            return admin_err();
                        }
                        handle_result(api_tts_rules::handle_update(db, id, body), 400)
                    }
                    Method::DELETE => {
                        if require_admin(claims, db).is_some() {
                            return admin_err();
                        }
                        handle_result(api_tts_rules::handle_delete(db, id), 500)
                    }
                    _ => (405, r#"{"error":"Method not allowed"}"#.to_string()),
                };
            }

            (404, r#"{"error":"Endpoint nie znaleziony"}"#.to_string())
        }
    }
}

/// Routuje endpointy /api/registries/* do odpowiednich handlerow (async)
async fn route_registries_api(
    method: &Method,
    path: &str,
    db: &DbPool,
    cipher: &Arc<crate::crypto::SecretsCipher>,
    body: &[u8],
    claims: &auth::Claims,
) -> (u16, String) {
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();

    match (method, segments.as_slice()) {
        // GET /api/registries
        (&Method::GET, ["api", "registries"]) => {
            handle_result(api_registries::handle_list(db), 500)
        }

        // POST /api/registries (VULN-008: admin only)
        (&Method::POST, ["api", "registries"]) => {
            if let Some(err) = require_admin(claims, db) {
                return err;
            }
            handle_result(api_registries::handle_create(db, cipher, body), 500)
        }

        // PUT /api/registries/:id (VULN-008: admin only)
        (&Method::PUT, ["api", "registries", id]) => {
            if let Some(err) = require_admin(claims, db) {
                return err;
            }
            match id.parse::<i64>() {
                Ok(id) => handle_result(api_registries::handle_update(db, cipher, id, body), 500),
                Err(_) => (400, r#"{"error":"Niepoprawne ID"}"#.to_string()),
            }
        }

        // DELETE /api/registries/:id (VULN-008: admin only)
        (&Method::DELETE, ["api", "registries", id]) => {
            if let Some(err) = require_admin(claims, db) {
                return err;
            }
            match id.parse::<i64>() {
                Ok(id) => handle_result(api_registries::handle_delete(db, id), 500),
                Err(_) => (400, r#"{"error":"Niepoprawne ID"}"#.to_string()),
            }
        }

        // POST /api/registries/:id/test
        (&Method::POST, ["api", "registries", id, "test"]) => match id.parse::<i64>() {
            Ok(id) => api_registries::handle_test(db, cipher, id).await,
            Err(_) => (400, r#"{"error":"Niepoprawne ID"}"#.to_string()),
        },

        _ => (
            404,
            r#"{"error":"Registry endpoint nie znaleziony"}"#.to_string(),
        ),
    }
}

/// Routuje endpointy /api/hub/* — silniki, modele HF, lokalne modele
/// VULN-030: Mutujace endpointy (download, delete) wymagaja uprawnien administratora
async fn route_hub_api(
    method: &Method,
    path: &str,
    query: &str,
    body: &[u8],
    mesh_peer_store: &MeshPeerStore,
    claims: &auth::Claims,
    db: &DbPool,
) -> (u16, String) {
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();

    let hub_result = |r: Result<String, String>, err_status: u16| -> (u16, String) {
        match r {
            Ok(body) => (200, body),
            Err(e) => (err_status, format!(r#"{{"error":"{}"}}"#, e)),
        }
    };

    match (method, segments.as_slice()) {
        // GET /api/hub/engines
        (&Method::GET, ["api", "hub", "engines"]) => {
            hub_result(api_hub::handle_list_engines(query, mesh_peer_store), 500)
        }

        // GET /api/hub/models/search?q=...&engine=...
        (&Method::GET, ["api", "hub", "models", "search"]) => {
            hub_result(api_hub::handle_search_models(query).await, 500)
        }

        // GET /api/hub/models/defaults?engine=...
        (&Method::GET, ["api", "hub", "models", "defaults"]) => {
            hub_result(api_hub::handle_default_models(query), 500)
        }

        // GET /api/hub/models/local
        (&Method::GET, ["api", "hub", "models", "local"]) => {
            hub_result(api_hub::handle_list_local_models(), 500)
        }

        // POST /api/hub/models/download (VULN-030: admin only)
        (&Method::POST, ["api", "hub", "models", "download"]) => {
            if let Some(err) = require_admin(claims, db) {
                return err;
            }
            hub_result(api_hub::handle_download_model(body).await, 400)
        }

        // DELETE /api/hub/models/local/{org}/{model} (VULN-030: admin only)
        (&Method::DELETE, ["api", "hub", "models", "local", org, model]) => {
            if let Some(err) = require_admin(claims, db) {
                return err;
            }
            let model_id = format!("{}/{}", org, model);
            hub_result(api_hub::handle_delete_local_model(&model_id), 500)
        }

        _ => (
            404,
            r#"{"error":"Hub endpoint nie znaleziony"}"#.to_string(),
        ),
    }
}

/// Routuje endpointy /api/users/*, /api/groups/*, /api/addons/*
fn route_addon_system_api(
    method: &Method,
    path: &str,
    db: &DbPool,
    claims: &auth::Claims,
    body: &[u8],
    permission_checker: &Option<Arc<crate::addon::permissions::PermissionChecker>>,
    router: Option<&Arc<Router>>,
) -> (u16, String) {
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();

    match (method, segments.as_slice()) {
        // --- Users ---
        // GET /api/users zmigrowane do binary `UsersListRequest` (dispatch handler).
        (&Method::POST, ["api", "users"]) => {
            handle_result(api_addon_system::handle_create_user(db, claims, body), 400)
        }
        (&Method::PUT, ["api", "users", id, "password"]) => match id.parse::<i64>() {
            Ok(uid) => handle_result(
                api_addon_system::handle_change_user_password(db, claims, uid, body),
                400,
            ),
            Err(_) => (400, r#"{"error":"Niepoprawne ID uzytkownika"}"#.to_string()),
        },
        (&Method::PUT, ["api", "users", id]) => match id.parse::<i64>() {
            Ok(uid) => handle_result(
                api_addon_system::handle_update_user(db, claims, uid, body),
                400,
            ),
            Err(_) => (400, r#"{"error":"Niepoprawne ID uzytkownika"}"#.to_string()),
        },
        (&Method::DELETE, ["api", "users", id]) => match id.parse::<i64>() {
            Ok(uid) => handle_result(api_addon_system::handle_delete_user(db, claims, uid), 500),
            Err(_) => (400, r#"{"error":"Niepoprawne ID uzytkownika"}"#.to_string()),
        },

        // --- Groups ---
        // VULN-036: Lista grup wymaga uprawnien administratora
        (&Method::GET, ["api", "groups"]) => {
            if let Some(err) = require_admin(claims, db) {
                return err;
            }
            handle_result(api_addon_system::handle_list_groups(db), 500)
        }
        (&Method::POST, ["api", "groups"]) => {
            handle_result(api_addon_system::handle_create_group(db, claims, body), 400)
        }
        (&Method::DELETE, ["api", "groups", id]) => match id.parse::<i64>() {
            Ok(gid) => handle_result(api_addon_system::handle_delete_group(db, claims, gid), 500),
            Err(_) => (400, r#"{"error":"Niepoprawne ID grupy"}"#.to_string()),
        },
        (&Method::POST, ["api", "groups", id, "members"]) => match id.parse::<i64>() {
            Ok(gid) => handle_result(
                api_addon_system::handle_add_group_member(db, claims, gid, body),
                400,
            ),
            Err(_) => (400, r#"{"error":"Niepoprawne ID grupy"}"#.to_string()),
        },
        (&Method::DELETE, ["api", "groups", gid, "members", uid]) => {
            match (gid.parse::<i64>(), uid.parse::<i64>()) {
                (Ok(g), Ok(u)) => handle_result(
                    api_addon_system::handle_remove_group_member(db, claims, g, u),
                    500,
                ),
                _ => (400, r#"{"error":"Niepoprawne ID"}"#.to_string()),
            }
        }

        // --- Addons ---
        // GET /api/addons zmigrowane do binary `AddonsListRequest` (dispatch handler).
        // POST /api/addons/install — zmigrowane do `AddonInstallRequest`.
        // PUT/DELETE /api/addons/:id (toggle/uninstall) — `AddonToggleRequest`/`AddonUninstallRequest`.
        // GET/PUT /api/addons/:id/config — `AddonConfigGetRequest`/`AddonConfigSetRequest`.
        // GET/PUT /api/addons/:id/limits — `AddonResourcesGetRequest`/`AddonResourcesSetRequest`.
        // GET /api/addons/:id/tools — `AddonToolsRequest`.
        // GET /api/addons/:id/network-rules — `AddonNetworkRulesGetRequest`.
        (&Method::POST, ["api", "addons", addon_id, "tools", tool_name]) => handle_result(
            api_addon_system::handle_invoke_addon_tool(
                db,
                addon_id,
                tool_name,
                &String::from_utf8_lossy(body),
                router,
            ),
            500,
        ),
        (&Method::GET, ["api", "addons", addon_id, "ui"]) => {
            handle_result(api_addon_system::handle_get_addon_ui(db, addon_id), 500)
        }
        // VULN-036: Uprawnienia addonu wymagaja uprawnien administratora
        (&Method::GET, ["api", "addons", addon_id, "permissions"]) => {
            if let Some(err) = require_admin(claims, db) {
                return err;
            }
            handle_result(
                api_addon_system::handle_get_addon_permissions(db, addon_id),
                500,
            )
        }
        (&Method::PUT, ["api", "addons", addon_id, "permissions"]) => handle_result(
            api_addon_system::handle_set_addon_permissions(
                db,
                claims,
                addon_id,
                body,
                permission_checker.as_ref(),
            ),
            400,
        ),

        // --- Tools (all addons) ---
        (&Method::GET, ["api", "tools"]) => {
            handle_result(api_addon_system::handle_list_all_tools(db), 500)
        }

        // --- Network Rules: per-rule approve/revoke (binary tylko get/set caly config) ---
        (&Method::PUT, ["api", "addons", addon_id, "network-rules", rule_id, "approve"]) => {
            handle_result(
                api_addon_system::handle_approve_network_rule(db, claims, addon_id, rule_id),
                400,
            )
        }
        (&Method::PUT, ["api", "addons", addon_id, "network-rules", rule_id, "revoke"]) => {
            handle_result(
                api_addon_system::handle_revoke_network_rule(db, claims, addon_id, rule_id),
                400,
            )
        }

        _ => (404, r#"{"error":"Endpoint nie znaleziony"}"#.to_string()),
    }
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

/// Routing endpointow clusters — CRUD clusterow, czlonkostwa nodow, probing
async fn route_clusters_api(
    method: &Method,
    path: &str,
    db: &DbPool,
    body: &[u8],
    claims: &auth::Claims,
    quic_mesh: &Option<Arc<crate::mesh::iroh_manager::IrohMeshManager>>,
) -> (u16, String) {
    // GET /api/clusters
    if path == "/api/clusters" && *method == Method::GET {
        return handle_result(api_clusters::handle_list(db), 500);
    }

    // POST /api/clusters (admin only)
    if path == "/api/clusters" && *method == Method::POST {
        if let Some(err) = require_admin(claims, db) {
            return err;
        }
        return handle_result(api_clusters::handle_create(db, body), 400);
    }

    // POST /api/clusters/probe — rozpocznij probing (admin only)
    if path == "/api/clusters/probe" && *method == Method::POST {
        if let Some(err) = require_admin(claims, db) {
            return err;
        }
        let qm = match quic_mesh {
            Some(qm) => qm.clone(),
            None => return (503, r#"{"error":"QuicMesh niedostepny"}"#.to_string()),
        };
        return handle_result(api_clusters::handle_start_probe(body, qm).await, 500);
    }

    // DELETE /api/clusters/probe/:probe_id — anuluj probing
    if path.starts_with("/api/clusters/probe/") && *method == Method::DELETE {
        let probe_id = path
            .strip_prefix("/api/clusters/probe/")
            .unwrap_or("")
            .trim_matches('/');
        if !probe_id.is_empty() {
            return handle_result(api_clusters::handle_delete_probe(probe_id).await, 500);
        }
    }

    // Sciezki z :id
    if path.starts_with("/api/clusters/") {
        let rest = &path["/api/clusters/".len()..];

        // POST /api/clusters/:id/members
        if rest.ends_with("/members") && *method == Method::POST {
            if let Some(err) = require_admin(claims, db) {
                return err;
            }
            let cluster_id = rest
                .strip_suffix("/members")
                .unwrap_or("")
                .trim_matches('/');
            if !cluster_id.is_empty() {
                return handle_result(api_clusters::handle_add_member(db, cluster_id, body), 400);
            }
        }

        // DELETE /api/clusters/:id/members/:node_id
        if rest.contains("/members/") && *method == Method::DELETE {
            if let Some(err) = require_admin(claims, db) {
                return err;
            }
            let parts: Vec<&str> = rest.splitn(2, "/members/").collect();
            if parts.len() == 2 {
                let cluster_id = parts[0].trim_matches('/');
                let node_id = parts[1].trim_matches('/');
                if !cluster_id.is_empty() && !node_id.is_empty() {
                    return handle_result(
                        api_clusters::handle_remove_member(db, cluster_id, node_id),
                        500,
                    );
                }
            }
        }

        // GET/PUT/DELETE /api/clusters/:id
        let cluster_id = rest.trim_matches('/');
        if !cluster_id.is_empty() && !cluster_id.contains('/') {
            return match *method {
                Method::GET => handle_result(api_clusters::handle_get(db, cluster_id), 500),
                Method::PUT => {
                    if let Some(err) = require_admin(claims, db) {
                        return err;
                    }
                    handle_result(api_clusters::handle_update(db, cluster_id, body), 400)
                }
                Method::DELETE => {
                    if let Some(err) = require_admin(claims, db) {
                        return err;
                    }
                    handle_result(api_clusters::handle_delete(db, cluster_id), 500)
                }
                _ => (405, r#"{"error":"Metoda niedozwolona"}"#.to_string()),
            };
        }
    }

    (
        404,
        serde_json::json!({"error": "Nieznany endpoint clusters"}).to_string(),
    )
}
