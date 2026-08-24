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

/// Chunk size for streamed file bodies (recordings). Matches the model-bundle
/// downloader so one slow client costs one chunk of memory, not a whole clip.
const FILE_STREAM_CHUNK_BYTES: usize = 256 * 1024;

/// Parses a single `Range: bytes=<start>-<end>` header against a known size and
/// returns the INCLUSIVE byte range. `<video>` sends this to seek a multi-GB
/// clip without downloading it whole. Multi-range, malformed and unsatisfiable
/// specs return `None` — the caller then serves the full body (200), which is a
/// valid HTTP response to any Range request.
fn parse_byte_range(raw: Option<&str>, size: u64) -> Option<(u64, u64)> {
    let spec = raw?.strip_prefix("bytes=")?.trim();
    if spec.contains(',') {
        return None; // multi-range: not worth the complexity, serve whole
    }
    let (from, to) = spec.split_once('-')?;
    let (start, end) = if from.is_empty() {
        // Suffix form `-N`: the LAST n bytes.
        let n: u64 = to.parse().ok()?;
        if n == 0 {
            return None;
        }
        (size.saturating_sub(n), size.checked_sub(1)?)
    } else {
        let start: u64 = from.parse().ok()?;
        let end = if to.is_empty() {
            size.checked_sub(1)?
        } else {
            to.parse::<u64>().ok()?.min(size.checked_sub(1)?)
        };
        (start, end)
    };
    if start > end || start >= size {
        return None;
    }
    Some((start, end))
}

/// Streams at most `limit` bytes from an already-positioned file handle. Unlike
/// the model-bundle `file_stream` (which runs to EOF) this stops at the end of
/// the requested range, so a 206 body matches its `Content-Length` exactly.
fn ranged_file_stream(
    file: tokio::fs::File,
    limit: u64,
) -> impl Stream<Item = Result<Frame<Bytes>, std::io::Error>> + Send {
    futures::stream::unfold((Some(file), limit), |(state, remaining)| async move {
        let mut file = state?;
        if remaining == 0 {
            return None;
        }
        use tokio::io::AsyncReadExt;
        let want = remaining.min(FILE_STREAM_CHUNK_BYTES as u64) as usize;
        let mut buf = vec![0u8; want];
        match file.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                let left = remaining - n as u64;
                Some((Ok(Frame::data(Bytes::from(buf))), (Some(file), left)))
            }
            Err(e) => Some((Err(e), (None, 0))),
        }
    })
}

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

    /// Ustawia AddonManager — udostepnia ui_panels cache dla Apps menu.
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

        crate::scheduler::start(db.clone(), addon_manager.clone());

        // Wire up cross-node service action handlers (krok N3b). The mesh
        // command executor is created by `start_mesh_pipeline` long before
        // AppState (db_pool + port_allocator + iroh) is fully assembled, so
        // we inject the action context here once everything exists. Without
        // this the receiver of `ServiceDeleteRemote` / `ServicePinRemote` /
        // ... returns "service action context not configured".
        if let (Some(qm), Some(pa), Some(am)) = (
            quic_mesh.clone(),
            port_allocator.clone(),
            addon_manager.clone(),
        ) {
            if let Some(executor) = qm.command_executor().await {
                executor
                    .set_service_action_context(
                        crate::mesh::command_executor::ServiceActionContext {
                            db: db.clone(),
                            port_allocator: pa,
                            iroh: qm.clone(),
                            router: router.clone(),
                            addon_manager: am.clone(),
                        },
                    )
                    .await;
            }
            // Wire the global robot-dispatch context so the `robot_dispatch_v1`
            // host function can route a controller action to the owning node — the
            // sender-side counterpart of the `RobotControl` receiver above.
            crate::mesh::robot_dispatch::set_dispatch_context(
                crate::mesh::robot_dispatch::RobotDispatchContext {
                    iroh: qm.clone(),
                    addon_manager: am,
                    local_node_id: local_node_id.to_string(),
                },
            );
            // Wire the recordings-pull context so the ML Studio layer can pull
            // camera recordings from a paired node without threading AppState.
            crate::mesh::recordings_pull::set_context(
                crate::mesh::recordings_pull::RecordingsPullContext {
                    iroh: qm.clone(),
                    local_node_id: local_node_id.to_string(),
                },
            );
        }

        // Code Studio owner side. A forwarded mesh request is executed here
        // through the ordinary dispatch handlers, so the receive path needs the
        // same shared resources a WebSocket connection assembles — but it has no
        // connection to assemble them from, and a per-connection AppState would
        // die with its socket. Register a server-lifetime one instead; without
        // it the owner answers every forwarded call with "code studio mesh
        // context is not initialized on this node". Registered outside the mesh
        // `if let` above because the non-mesh readers of `node_state()` must see
        // it on a node that never paired, and unconditionally on the runtime
        // because the call also starts assertion-key rotation.
        let ui_sessions = match crate::addon::ui_session::global_registry() {
            Some(registry) => registry.clone(),
            None => {
                crate::addon::ui_session::init_global_registry(Arc::new(
                    crate::addon::ui_session::SessionRegistry::new(),
                ));
                crate::addon::ui_session::global_registry()
                    .expect("global_registry must be set after init")
                    .clone()
            }
        };
        crate::code_studio::remote_proxy::install_owner_context(Arc::new(
            crate::dispatch::AppState {
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
                meeting_manager: crate::meeting::MeetingManager::new(
                    db.clone(),
                    Some(service_manager.clone()),
                ),
                vnc_tunnels: Arc::new(dashmap::DashMap::new()),
                mesh_relay_health: mesh_relay_health.clone(),
                port_allocator: port_allocator.clone(),
                mesh_services_registry: mesh_services_registry.clone(),
                live_handles: service_manager.live_handles.clone(),
                ui_sessions,
                progress_broker: crate::flow_engine::progress_broker::global_broker(),
                agent_run_manager: crate::agents::agent_run_manager_global(),
            },
        ));

        // §13 — the durability promise. Everything the previous process left
        // half-done is settled HERE, before the first connection is accepted:
        // orphan shells reaped, cached statuses re-derived from the timeline,
        // unfinished provisioning and live sessions moved to a resumable
        // terminal state. Same place as the owner context because both are
        // "this node owns Code Studio state" work, and both must be true on a
        // node that never paired.
        {
            let db_for_recovery = db.clone();
            let node_for_recovery = local_node_id.clone();
            match tokio::task::spawn_blocking(move || {
                reconcile_code_studio(&db_for_recovery, &node_for_recovery)
            })
            .await
            {
                Ok(report) => {
                    if report != CodeStudioRecovery::default() {
                        info!(
                            "code studio recovery: {} workspace(s) failed, {} session(s) \
                             interrupted, {} projection(s) corrected, {} terminal(s) reaped, \
                             {} sandbox(es) closed, {} effect(s) confirmed, {} retryable",
                            report.workspaces_failed,
                            report.sessions_interrupted,
                            report.projections_corrected,
                            report.terminals_reaped,
                            report.sandboxes_reconciled,
                            report.operations_completed,
                            report.operations_retryable
                        );
                    }
                    // Its own line, at warning level: an effect nobody can
                    // verify is the one outcome of this pass that needs a
                    // PERSON, and §22 alerts on one that lingers.
                    if report.operations_unknown > 0 {
                        warn!(
                            "code studio recovery: {} interrupted effect(s) could not be \
                             verified and are waiting for a decision",
                            report.operations_unknown
                        );
                    }
                }
                Err(e) => error!("code studio recovery task panicked: {e}"),
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

fn make_static_response_with_origin(
    path: &str,
    status: u16,
    content_type: &str,
    body: Vec<u8>,
    origin: Option<&str>,
    etag: &str,
    if_none_match: Option<&str>,
) -> Response<DashboardBody> {
    // Tylko sw.js + jego importScripts (sw-version.js) sa no-store — to one
    // napedzaja wykrywanie update'u SW i musza byc zawsze swieze. Reszta (w tym
    // wasm glue /js/protocol/) idzie przez ETag+rewalidacje: gdy tresc niezmieniona
    // browser dostaje 304 (bez ponownego pobrania MB), a zmiana wasm daje nowy
    // ETag i swieze bajty — schema-handshake i tak jest siatka bezpieczenstwa.
    let no_store = path == "/sw.js" || path == "/js/generated/sw-version.js";

    // Warunkowy GET: dla zasobow z etagiem (poza no-store) ustawiamy ETag +
    // Cache-Control:no-cache. Browser rewaliduje kazdy load (If-None-Match);
    // gdy tresc niezmieniona -> 304 bez body. Caching dziala ZAWSZE, niezaleznie
    // od service workera i zaufania do certa.
    let cacheable = status == 200 && !no_store && !etag.is_empty();
    let quoted = format!("\"{}\"", etag);
    if cacheable {
        if let Some(inm) = if_none_match {
            // RFC 7232 §3.2: `*` pasuje do kazdej reprezentacji; inaczej lista
            // etagow rozdzielona przecinkami, weak comparison (ignorujemy prefiks
            // `W/` i cudzyslowy — nasze etagi sa silne, ale proxy moze oslabic).
            let matches = inm.trim() == "*"
                || inm.split(',').map(|s| s.trim()).any(|s| {
                    let s = s.strip_prefix("W/").unwrap_or(s);
                    s.trim_matches('"') == etag
                });
            if matches {
                let mut b = Response::builder()
                    .status(StatusCode::NOT_MODIFIED)
                    .header("ETag", &quoted)
                    .header("Cache-Control", "no-cache");
                if let Some(o) = origin {
                    b = b.header("Access-Control-Allow-Origin", o);
                }
                return b.body(Either::Left(Full::new(Bytes::new()))).unwrap();
            }
        }
    }

    let mut builder = Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
        .header("Content-Type", content_type);

    if no_store {
        builder = builder
            .header("Cache-Control", "no-store")
            .header("Pragma", "no-cache");
    } else if cacheable {
        builder = builder
            .header("ETag", &quoted)
            .header("Cache-Control", "no-cache");
    }

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

/// Adds the always-on security headers we send back with every byte body
/// served from the HMAC-only signed-URL endpoints (`/frames`, `/recordings`).
/// `Cross-Origin-Resource-Policy: same-site` blocks cross-origin embedding
/// (a hostile page cannot pull a frame into a `<canvas>`); `Cache-Control:
/// private, no-store` keeps the bytes out of intermediate caches; the rest
/// are the standard browser-side hardening trio.
fn apply_signed_url_security_headers(
    mut builder: hyper::http::response::Builder,
) -> hyper::http::response::Builder {
    builder = builder
        .header("Cross-Origin-Resource-Policy", "same-site")
        .header("Referrer-Policy", "no-referrer")
        .header("Cache-Control", "private, no-store")
        .header("X-Content-Type-Options", "nosniff");
    builder
}

/// F1b P3.C-3 — map a `PickupOutcome` to the HTTP response. Shared by the
/// local fast path (`handle_pickup`) and the cross-node mesh-fallback path
/// (`frame_proxy::fetch_from_peer`) so both routes apply the same security
/// headers and emit the same body shape per status code.
fn pickup_outcome_to_response(
    outcome: crate::api::frame_pickup::PickupOutcome,
) -> Response<DashboardBody> {
    use crate::api::frame_pickup::{
        PickupOutcome, HDR_FRAME_HEIGHT, HDR_FRAME_PIXEL_FORMAT, HDR_FRAME_PTS, HDR_FRAME_TS_MS,
        HDR_FRAME_WIDTH,
    };
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
            builder = apply_signed_url_security_headers(builder);
            let body = Bytes::copy_from_slice(&bytes);
            builder.body(Either::Left(Full::new(body))).unwrap()
        }
        PickupOutcome::BadHeaders(why) | PickupOutcome::HeaderMismatch(why) => {
            let body = format!("{{\"error\":\"{}\"}}", why);
            let builder = apply_signed_url_security_headers(
                Response::builder()
                    .status(status)
                    .header("Content-Type", "application/json"),
            );
            builder
                .body(Either::Left(Full::new(Bytes::from(body))))
                .unwrap()
        }
        PickupOutcome::UpstreamUnavailable(reason) => {
            let body = format!("{{\"error\":\"{}\"}}", reason);
            let builder = apply_signed_url_security_headers(
                Response::builder()
                    .status(status)
                    .header("Content-Type", "application/json")
                    .header("Retry-After", "5"),
            );
            builder
                .body(Either::Left(Full::new(Bytes::from(body))))
                .unwrap()
        }
        PickupOutcome::Replay => {
            let builder = apply_signed_url_security_headers(
                Response::builder()
                    .status(status)
                    .header("Content-Type", "application/json"),
            );
            builder
                .body(Either::Left(Full::new(Bytes::from_static(
                    b"{\"error\":\"replay\"}",
                ))))
                .unwrap()
        }
        PickupOutcome::Unauthorized(_)
        | PickupOutcome::FramePurged
        | PickupOutcome::UpstreamNotFound => {
            let builder = apply_signed_url_security_headers(
                Response::builder()
                    .status(status)
                    .header("Content-Type", "application/json"),
            );
            builder
                .body(Either::Left(Full::new(Bytes::from_static(
                    b"{\"error\":\"pickup_denied\"}",
                ))))
                .unwrap()
        }
    }
}

/// Collapse-audit map: per-IP timestamp of the LAST audit row emitted for a
/// 429 denial, plus the count of denials inside the current window. We do
/// not want to write one row per refused token, so we coalesce: at most one
/// row per `AUDIT_429_WINDOW` per IP, carrying the in-window count.
///
/// Bounded by both an idle sweep (entries whose window has fully elapsed
/// twice over are useless) and a hard cap (LRU-evict the oldest 25 % once
/// `MAX_AUDIT_ENTRIES` is reached). Without these the map would grow
/// unbounded under a flood of unique forged-token attackers.
static RATE_LIMIT_AUDIT: std::sync::OnceLock<dashmap::DashMap<String, (std::time::Instant, u32)>> =
    std::sync::OnceLock::new();
const AUDIT_429_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
const AUDIT_IDLE_EVICT_AFTER: std::time::Duration = std::time::Duration::from_secs(120);
const MAX_AUDIT_ENTRIES: usize = 10_000;

fn rate_limit_audit_map() -> &'static dashmap::DashMap<String, (std::time::Instant, u32)> {
    RATE_LIMIT_AUDIT.get_or_init(dashmap::DashMap::new)
}

/// Amortized cleanup: called from `build_rate_limit_response`. Cheap when the
/// map is small (early-returns), aggressive once it crosses 1 000 entries.
fn sweep_rate_limit_audit(now: std::time::Instant) {
    let map = rate_limit_audit_map();
    if map.len() < 1_000 {
        return;
    }
    map.retain(|_, (last_seen, _)| {
        now.saturating_duration_since(*last_seen) < AUDIT_IDLE_EVICT_AFTER
    });
    if map.len() >= MAX_AUDIT_ENTRIES {
        let target = MAX_AUDIT_ENTRIES * 3 / 4;
        let mut snapshot: Vec<(String, std::time::Instant)> =
            map.iter().map(|e| (e.key().clone(), e.value().0)).collect();
        snapshot.sort_by_key(|(_, ts)| *ts);
        let drop_count = snapshot.len().saturating_sub(target);
        for (key, _) in snapshot.into_iter().take(drop_count) {
            map.remove(&key);
        }
    }
}

/// Build a 429 response and emit a collapsed-audit row if the per-IP window
/// has elapsed. `retry_after_secs` is rounded up to whole seconds for the
/// `Retry-After` header (HTTP requires integer seconds).
fn build_rate_limit_response(
    db: &DbPool,
    ip: &str,
    user_agent: Option<&str>,
    endpoint: &str,
    retry_after_secs: f64,
    global: bool,
) -> Response<DashboardBody> {
    let retry_after = retry_after_secs.ceil().max(1.0) as u64;
    let key = format!("{ip}|{endpoint}");
    let now = std::time::Instant::now();
    sweep_rate_limit_audit(now);
    let mut entry = rate_limit_audit_map().entry(key).or_insert((now, 0));
    let elapsed = now.saturating_duration_since(entry.0);
    entry.1 = entry.1.saturating_add(1);
    let should_emit = elapsed >= AUDIT_429_WINDOW || entry.0 == now;
    if should_emit {
        let denied_count = entry.1;
        let details = serde_json::json!({
            "endpoint": endpoint,
            "denied_count": denied_count,
            "window_secs": AUDIT_429_WINDOW.as_secs(),
            "source_ip": ip,
            "user_agent": user_agent.map(|s| s.chars().take(256).collect::<String>()).unwrap_or_default(),
            "global": global,
        })
        .to_string();
        if let Ok(conn) = db.write() {
            let _ = conn.execute(
                "INSERT INTO audit_log \
                    (timestamp, user_id, addon_id, action, resource_type, resource_id, \
                     result, error_message, severity, risk_class, details) \
                 VALUES (datetime('now'), NULL, NULL, 'rate_limit_denied', \
                         'http_endpoint', ?1, 'denied', NULL, 'warn', 'B', ?2)",
                rusqlite::params![endpoint, details],
            );
        }
        entry.0 = now;
        entry.1 = 0;
    }
    Response::builder()
        .status(StatusCode::TOO_MANY_REQUESTS)
        .header("Content-Type", "application/json")
        .header("Retry-After", retry_after.to_string())
        .body(Either::Left(Full::new(Bytes::from_static(
            b"{\"error\":\"rate_limited\"}",
        ))))
        .unwrap()
}

/// Single helper that rate-limits the signed-URL endpoints. Returns Err with
/// a pre-built 429 if the request must be refused.
fn check_signed_url_rate_limit(
    db: &DbPool,
    ip: &str,
    user_agent: Option<&str>,
    endpoint: &str,
) -> std::result::Result<(), Response<DashboardBody>> {
    use crate::api::rate_limit::{rate_limiter, RateLimitResult};
    match rate_limiter().check(ip) {
        RateLimitResult::Allow => Ok(()),
        RateLimitResult::IpLimit {
            retry_after_secs, ..
        } => Err(build_rate_limit_response(
            db,
            ip,
            user_agent,
            endpoint,
            retry_after_secs,
            false,
        )),
        RateLimitResult::GlobalLimit { retry_after_secs } => Err(build_rate_limit_response(
            db,
            ip,
            user_agent,
            endpoint,
            retry_after_secs,
            true,
        )),
    }
}

/// Charge the strict invalid-signed-token limiter after an HMAC token FAILED
/// verification on `/frames` or `/recordings`. Returns:
///   * `None`  → the strict bucket still had budget; caller serves the normal
///     401/403 (with its own already-emitted audit).
///   * `Some(resp)` → the strict bucket is exhausted; caller returns this 429
///     (with the `rate_limit_denied` audit) INSTEAD of the 401/403, bounding
///     both forged-token throughput and the audit-INSERT cost per IP.
///
/// Valid requests from a logged-in user never reach this path, so they are
/// never throttled beyond the generous pre-verify ceiling.
fn charge_invalid_signed_token(
    db: &DbPool,
    ip: &str,
    user_agent: Option<&str>,
    endpoint: &str,
) -> Option<Response<DashboardBody>> {
    use crate::api::rate_limit::{invalid_signed_token_limiter, RateLimitResult};
    match invalid_signed_token_limiter().check(ip) {
        RateLimitResult::Allow => None,
        RateLimitResult::IpLimit {
            retry_after_secs, ..
        } => Some(build_rate_limit_response(
            db,
            ip,
            user_agent,
            endpoint,
            retry_after_secs,
            false,
        )),
        RateLimitResult::GlobalLimit { retry_after_secs } => Some(build_rate_limit_response(
            db,
            ip,
            user_agent,
            endpoint,
            retry_after_secs,
            true,
        )),
    }
}

/// Bearer token for the `/models/*` endpoints, extracted before the request
/// is dropped (the streaming body handler must not hold `req`).
fn models_bearer_token(headers: &hyper::HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Resolved `/models/*` credential: parsed signed query OR an active API-key
/// uid (owned — the caller borrows it into `BundleAuth`).
enum ModelsAuth {
    Signed(crate::api::frames::FrameQuery),
    ApiKey(String),
}

/// Shared auth resolution for both `/models/*` endpoints. A Bearer header
/// wins over query params; without one the signed query is parsed as before.
/// Bearer failures are audited here (they never reach a handler) and mapped
/// to /v1-style JSON errors; a valid key is additionally run through the same
/// per-key token bucket as `/v1`.
fn resolve_models_auth(
    db: &DbPool,
    settings_cipher: &crate::crypto::SettingsCipher,
    bearer_token: Option<String>,
    query_string: &str,
    audit_ref: &str,
    ctx: crate::api::model_bundle::RequestContext<'_>,
) -> std::result::Result<ModelsAuth, Response<DashboardBody>> {
    use crate::api::model_bundle::{
        audit_api_key_rejected, resolve_bearer_api_key, BearerAuthResult,
    };
    let json_error = |status: StatusCode, body: &'static str| {
        Response::builder()
            .status(status)
            .header("Content-Type", "application/json")
            .body(Either::Left(Full::new(Bytes::from_static(body.as_bytes()))))
            .unwrap()
    };
    let Some(token) = bearer_token else {
        return match crate::api::frames::parse_query(query_string) {
            Ok(q) => Ok(ModelsAuth::Signed(q)),
            Err(why) => {
                let body = format!("{{\"error\":\"{}\"}}", why);
                Err(Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .header("Content-Type", "application/json")
                    .body(Either::Left(Full::new(Bytes::from(body))))
                    .unwrap())
            }
        };
    };
    match resolve_bearer_api_key(db, settings_cipher, &token) {
        BearerAuthResult::Ok(key) => {
            if let Some(retry) =
                crate::api::rate_limit::per_key_rate_limiter().check(&key.uid, key.rate_limit_rps)
            {
                let retry_secs = retry.ceil().max(1.0) as u64;
                return Err(Response::builder()
                    .status(StatusCode::TOO_MANY_REQUESTS)
                    .header("Content-Type", "application/json")
                    .header("Retry-After", retry_secs.to_string())
                    .body(Either::Left(Full::new(Bytes::from_static(
                        b"{\"error\":\"rate_limit_exceeded\"}",
                    ))))
                    .unwrap());
            }
            Ok(ModelsAuth::ApiKey(key.uid))
        }
        BearerAuthResult::Invalid => {
            audit_api_key_rejected(db, audit_ref, ctx, "invalid_api_key");
            Err(json_error(
                StatusCode::UNAUTHORIZED,
                "{\"error\":\"invalid_api_key\"}",
            ))
        }
        BearerAuthResult::Unavailable => {
            audit_api_key_rejected(db, audit_ref, ctx, "api_key_verification_unavailable");
            Err(json_error(
                StatusCode::UNAUTHORIZED,
                "{\"error\":\"api_key_verification_unavailable\"}",
            ))
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
    remote_addr: String,
    mesh_services_registry: Arc<crate::services::mesh_registry::MeshServicesRegistry>,
) -> std::result::Result<Response<DashboardBody>, hyper::Error> {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let query_string = req.uri().query().unwrap_or("").to_string();
    // Use the raw socket peer for rate-limiting + audit. We deliberately do
    // not honour `X-Forwarded-For` here: F1b core is meant to terminate the
    // TLS connection itself, and a reverse-proxy deployment must explicitly
    // opt in to XFF (not implemented in F1b — documented in the audit notes).
    let client_ip: String = remote_addr
        .rsplit_once(':')
        .map(|(host, _)| host.trim_matches(|c| c == '[' || c == ']').to_string())
        .unwrap_or_else(|| remote_addr.clone());
    let user_agent: Option<String> = req
        .headers()
        .get(hyper::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

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

    // WebSocket upgrade /ws/api — binary CBOR protocol (bootstrap, Task #30).
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
        let (user_id, role) = match extract_ws_user_session(req.headers(), &db, &settings_cipher) {
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

        // Ensure a single shared SessionRegistry. If already initialized (e.g. by
        // another code path), reuse the existing one. Both dispatch and host functions
        // must see the same instance.
        let shared_sessions = if let Some(registry) = crate::addon::ui_session::global_registry() {
            registry.clone()
        } else {
            let fresh = Arc::new(crate::addon::ui_session::SessionRegistry::new());
            crate::addon::ui_session::init_global_registry(fresh);
            crate::addon::ui_session::global_registry()
                .expect("global_registry must be set after init")
                .clone()
        };

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
            ui_sessions: shared_sessions.clone(),
            progress_broker: crate::flow_engine::progress_broker::global_broker(),
            agent_run_manager: crate::agents::agent_run_manager_global(),
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
            handle_pickup, log_outcome, verify_pickup_headers, PickupOutcome, PickupRequest,
            HDR_FRAME_REF, HDR_PICKUP_TOKEN, HDR_REQUEST_ID, HDR_SERVICE_ID,
        };
        use crate::services::pickup_tokens::{PickupVerifyError, VerifySource};
        // mTLS pinning gate: if the operator enabled `pickup_required`, the
        // connecting peer MUST present a client cert whose SHA-256 fingerprint
        // is on the allowlist. Default (single-node F1a/F1b) is `false`, in
        // which case this check is a no-op and HMAC token auth stands alone.
        let mtls_cfg = crate::api::mtls::pickup_mtls_config();
        if mtls_cfg.pickup_required {
            let peer_der = req
                .extensions()
                .get::<crate::api::mtls::ClientCertDer>()
                .map(|c| c.0.clone());
            let allowed = peer_der
                .as_deref()
                .map(|der| mtls_cfg.matches(der))
                .unwrap_or(false);
            if !allowed {
                warn!(
                    "/core/frame/pickup: mTLS pinning denied from {} (peer_cert_present={})",
                    client_ip,
                    peer_der.is_some()
                );
                return Ok(Response::builder()
                    .status(StatusCode::UNAUTHORIZED)
                    .header("Content-Type", "application/json")
                    .body(Either::Left(Full::new(Bytes::from_static(
                        b"{\"error\":\"mtls_required\"}",
                    ))))
                    .unwrap());
            }
        }
        if let Err(resp) = check_signed_url_rate_limit(
            &db,
            &client_ip,
            user_agent.as_deref(),
            "/core/frame/pickup",
        ) {
            return Ok(resp);
        }
        let hdr = |name: &str| -> Option<String> {
            req.headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
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

        // F1b P3.C-3 — split verify from consume so a Peer-source token can
        // be routed through frame_proxy instead of touching the local LRU.
        let verified = match verify_pickup_headers(&pr, issuer, &db) {
            Ok(v) => v,
            Err(outcome) => return Ok(pickup_outcome_to_response(outcome)),
        };

        let outcome = match &verified.source {
            VerifySource::Local => handle_pickup(pr, issuer, storage, &db),
            VerifySource::Peer(peer_node_id) => {
                // B-side replay protection: the mesh-fallback issuing node
                // owns the one-shot inflight contract, so we maintain a
                // process-local "this wire was already proxied through me"
                // map to stop double-spend on the verifying node.
                if let Err(PickupVerifyError::AlreadyConsumed) =
                    issuer.mesh_inflight_consume(&verified.token)
                {
                    return Ok(pickup_outcome_to_response(log_outcome(
                        &db,
                        &pr,
                        PickupOutcome::Replay,
                        Some(peer_node_id.clone()),
                    )));
                }
                match quic_mesh.as_ref() {
                    Some(iroh) => {
                        let fetch = crate::services::frame_proxy::fetch_from_peer(
                            iroh,
                            peer_node_id,
                            &verified.payload.raw_ref,
                            crate::services::frame_proxy::DEFAULT_FETCH_TIMEOUT,
                        )
                        .await;
                        match fetch {
                            Ok((bytes, meta)) => {
                                let pixel_format: &'static str = match meta.pixel_format.as_str() {
                                    "rgb24" => "rgb24",
                                    _ => "rgb24",
                                };
                                let outcome = PickupOutcome::Ok {
                                    bytes: std::sync::Arc::<[u8]>::from(bytes.into_boxed_slice()),
                                    width: meta.width,
                                    height: meta.height,
                                    pixel_format,
                                    timestamp_unix_ms: meta.timestamp_unix_ms,
                                    pts: None,
                                };
                                log_outcome(&db, &pr, outcome, Some(peer_node_id.clone()))
                            }
                            Err(crate::services::frame_proxy::FrameProxyError::NotFound(_)) => {
                                log_outcome(
                                    &db,
                                    &pr,
                                    PickupOutcome::UpstreamNotFound,
                                    Some(peer_node_id.clone()),
                                )
                            }
                            Err(crate::services::frame_proxy::FrameProxyError::Timeout(_)) => {
                                log_outcome(
                                    &db,
                                    &pr,
                                    PickupOutcome::UpstreamUnavailable("timeout"),
                                    Some(peer_node_id.clone()),
                                )
                            }
                            Err(crate::services::frame_proxy::FrameProxyError::Unavailable {
                                ..
                            }) => log_outcome(
                                &db,
                                &pr,
                                PickupOutcome::UpstreamUnavailable("upstream_unavailable"),
                                Some(peer_node_id.clone()),
                            ),
                            Err(_) => log_outcome(
                                &db,
                                &pr,
                                PickupOutcome::UpstreamUnavailable("proxy_error"),
                                Some(peer_node_id.clone()),
                            ),
                        }
                    }
                    None => log_outcome(
                        &db,
                        &pr,
                        PickupOutcome::UpstreamUnavailable("mesh_unavailable"),
                        Some(peer_node_id.clone()),
                    ),
                }
            }
        };

        return Ok(pickup_outcome_to_response(outcome));
    }

    // GET /frames/<ref>?token=&exp=&ref= — addon-facing multi-use signed URL
    // for raw RGB24 frames out of `services::frame_storage`. Authenticated by
    // HMAC token only (no JWT, no cookies, no CSRF surface).
    if method == Method::GET && path.starts_with("/frames/") && path.len() > "/frames/".len() {
        use crate::api::frames::{
            handle_frame_url, parse_query, FrameOutcome, RequestContext, HDR_FRAME_HEIGHT,
            HDR_FRAME_PIXEL_FORMAT, HDR_FRAME_PTS, HDR_FRAME_TS_MS, HDR_FRAME_WIDTH,
        };
        if let Err(resp) = reject_unauth_get_body(req.headers()) {
            return Ok(resp);
        }
        if let Err(resp) =
            check_signed_url_rate_limit(&db, &client_ip, user_agent.as_deref(), "/frames")
        {
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
        let ctx = RequestContext {
            source_ip: Some(client_ip.as_str()),
            user_agent: user_agent.as_deref(),
        };
        let outcome = handle_frame_url(path_ref, &q, issuer, storage, &db, ctx);
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
                // Frame storage holds raw RGB24. For browser `<img src>` we
                // must re-encode to JPEG — `application/octet-stream` would
                // fail to render and the dashboard would show a broken image.
                // Quality 75 is a good MVP balance (~50-150 KB per 1080p
                // frame). Later we will replace this snapshot polling with
                // WebRTC (Krok 5) and the JPEG re-encode disappears.
                let mut jpeg_buf: Vec<u8> = Vec::with_capacity(bytes.len() / 8);
                let color = if pixel_format == "rgb24" {
                    image::ExtendedColorType::Rgb8
                } else {
                    image::ExtendedColorType::Rgb8
                };
                use image::ImageEncoder;
                let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_buf, 75);
                let encode_result = encoder.write_image(&bytes, width, height, color);
                let (content_type, body_bytes) = match encode_result {
                    Ok(()) => ("image/jpeg", jpeg_buf),
                    Err(e) => {
                        tracing::warn!(
                            width,
                            height,
                            pixel_format,
                            error = %e,
                            "frames: JPEG encode failed, falling back to raw RGB"
                        );
                        ("application/octet-stream", bytes.to_vec())
                    }
                };
                let mut builder = Response::builder()
                    .status(status)
                    .header("Content-Type", content_type)
                    .header(HDR_FRAME_WIDTH, width.to_string())
                    .header(HDR_FRAME_HEIGHT, height.to_string())
                    .header(HDR_FRAME_PIXEL_FORMAT, pixel_format)
                    .header(HDR_FRAME_TS_MS, timestamp_unix_ms.to_string());
                if let Some(p) = pts {
                    builder = builder.header(HDR_FRAME_PTS, p.to_string());
                }
                builder = apply_signed_url_security_headers(builder);
                let body = Bytes::from(body_bytes);
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
            FrameOutcome::Denied(_) => {
                // Token verification FAILED (forged / expired / scope mismatch).
                // Charge the strict invalid-token bucket: if it is exhausted for
                // this IP, return 429 instead of 403 to bound forged-token spam
                // and its audit cost. `handle_frame_url` already emitted the
                // per-outcome "denied" audit above.
                if let Some(resp) =
                    charge_invalid_signed_token(&db, &client_ip, user_agent.as_deref(), "/frames")
                {
                    return Ok(resp);
                }
                return Ok(Response::builder()
                    .status(status)
                    .header("Content-Type", "application/json")
                    .body(Either::Left(Full::new(Bytes::from_static(
                        b"{\"error\":\"frame_denied\"}",
                    ))))
                    .unwrap());
            }
            FrameOutcome::NotFound => {
                // Valid token pointing at an evicted/unknown frame — NOT a
                // verification failure, so it must not charge the strict bucket
                // (a logged-in user hitting a stale thumbnail is legitimate).
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
    if method == Method::GET
        && path.starts_with("/recordings/")
        && path.len() > "/recordings/".len()
    {
        use crate::api::recording::{
            handle_recording_url, parse_query, read_recording_file, RecordingFileOutcome,
            RecordingOutcome, RequestContext,
        };
        if let Err(resp) = reject_unauth_get_body(req.headers()) {
            return Ok(resp);
        }
        if let Err(resp) =
            check_signed_url_rate_limit(&db, &client_ip, user_agent.as_deref(), "/recordings")
        {
            return Ok(resp);
        }
        // Capture Range BEFORE `req` is released — the streamed response below
        // needs it, and `size` (required to resolve the range) is only known
        // after the file is opened.
        let range_header = req
            .headers()
            .get(hyper::header::RANGE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
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
        let ctx = RequestContext {
            source_ip: Some(client_ip.as_str()),
            user_agent: user_agent.as_deref(),
        };
        let outcome = handle_recording_url(path_ref, &q, issuer, &db, ctx);
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
                    ctx,
                )
                .await;
                let status = file_outcome.http_status();
                return match file_outcome {
                    RecordingFileOutcome::Ok { mut file, size } => {
                        // STREAMED, never slurped: a clip can be gigabytes. `<video>`
                        // seeks via Range — without 206 + Content-Range the browser
                        // abandons the source and reports a bare "Format error".
                        let (code, start, length) =
                            match parse_byte_range(range_header.as_deref(), size) {
                                Some((s, e)) => (206u16, s, e - s + 1),
                                None => (200u16, 0, size),
                            };
                        if start > 0 {
                            use tokio::io::AsyncSeekExt;
                            if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
                                return Ok(Response::builder()
                                    .status(500)
                                    .header("Content-Type", "application/json")
                                    .body(Either::Left(Full::new(Bytes::from_static(
                                        b"{\"error\":\"recording_unavailable\"}",
                                    ))))
                                    .unwrap());
                            }
                        }
                        let stream: SseStream = Box::pin(ranged_file_stream(file, length));
                        let mut builder = Response::builder()
                            .status(code)
                            .header("Content-Type", content_type)
                            .header("Accept-Ranges", "bytes")
                            .header("Content-Length", length.to_string())
                            .header("X-Recording-Hash", hash_sha256)
                            .header("X-Recording-Created-At", created_at.to_string());
                        if code == 206 {
                            builder = builder.header(
                                "Content-Range",
                                format!("bytes {}-{}/{}", start, start + length - 1, size),
                            );
                        }
                        Ok(apply_signed_url_security_headers(builder)
                            .body(Either::Right(StreamBody::new(stream)))
                            .unwrap())
                    }
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
            RecordingOutcome::Denied(_) => {
                // Token verification FAILED (forged / expired / scope mismatch).
                // Charge the strict invalid-token bucket: exhausted → 429 instead
                // of 403, bounding forged-token spam and its audit cost.
                // `handle_recording_url` already emitted the "denied" audit above.
                if let Some(resp) = charge_invalid_signed_token(
                    &db,
                    &client_ip,
                    user_agent.as_deref(),
                    "/recordings",
                ) {
                    return Ok(resp);
                }
                return Ok(Response::builder()
                    .status(auth_status)
                    .header("Content-Type", "application/json")
                    .body(Either::Left(Full::new(Bytes::from_static(
                        b"{\"error\":\"recording_denied\"}",
                    ))))
                    .unwrap());
            }
            RecordingOutcome::NotFound | RecordingOutcome::InternalError(_) => {
                // Valid token, but the row is purged (404) or a DB/internal
                // failure (500). Neither is a token-verify failure, so the
                // strict invalid-token bucket must not be charged.
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

    // GET /ml-studio/exports/<ref>?token=&exp=&ref= — signed URL for an ML
    // Studio project export archive (zip). HMAC-only auth, exactly like
    // /recordings/. The ref IS the archive identity — there is no catalogue
    // table — so existence + containment are decided on the filesystem.
    if method == Method::GET
        && path.starts_with("/ml-studio/exports/")
        && path.len() > "/ml-studio/exports/".len()
    {
        use crate::api::ml_studio_export::{
            export_download_filename, handle_ml_studio_export_url, parse_query, read_export_file,
            ExportFileOutcome, ExportOutcome, RequestContext,
        };
        if let Err(resp) = reject_unauth_get_body(req.headers()) {
            return Ok(resp);
        }
        if let Err(resp) = check_signed_url_rate_limit(
            &db,
            &client_ip,
            user_agent.as_deref(),
            "/ml-studio/exports",
        ) {
            return Ok(resp);
        }
        // Capture Range BEFORE `req` is released — the streamed response below
        // needs it, and `size` (required to resolve the range) is only known
        // after the file is opened.
        let range_header = req
            .headers()
            .get(hyper::header::RANGE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        drop(req);
        let path_ref = path.strip_prefix("/ml-studio/exports/").unwrap_or("");
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
        let issuer = crate::services::ml_studio_export_url_issuer();
        let ctx = RequestContext {
            source_ip: Some(client_ip.as_str()),
            user_agent: user_agent.as_deref(),
        };
        let outcome = handle_ml_studio_export_url(path_ref, &q, issuer, &db, ctx);
        let auth_status = outcome.http_status();
        match outcome {
            ExportOutcome::Ok => {
                let file_outcome = read_export_file(&db, path_ref, ctx).await;
                let status = file_outcome.http_status();
                return match file_outcome {
                    ExportFileOutcome::Ok { mut file, size } => {
                        // STREAMED, never slurped: an export can be gigabytes.
                        // A paused download resumes via Range → 206 + Content-Range.
                        let (code, start, length) =
                            match parse_byte_range(range_header.as_deref(), size) {
                                Some((s, e)) => (206u16, s, e - s + 1),
                                None => (200u16, 0, size),
                            };
                        if start > 0 {
                            use tokio::io::AsyncSeekExt;
                            if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
                                return Ok(Response::builder()
                                    .status(500)
                                    .header("Content-Type", "application/json")
                                    .body(Either::Left(Full::new(Bytes::from_static(
                                        b"{\"error\":\"export_unavailable\"}",
                                    ))))
                                    .unwrap());
                            }
                        }
                        let stream: SseStream = Box::pin(ranged_file_stream(file, length));
                        let mut builder = Response::builder()
                            .status(code)
                            .header("Content-Type", "application/zip")
                            .header("Accept-Ranges", "bytes")
                            .header("Content-Length", length.to_string())
                            .header(
                                "Content-Disposition",
                                format!(
                                    "attachment; filename=\"{}\"",
                                    export_download_filename(path_ref)
                                ),
                            );
                        if code == 206 {
                            builder = builder.header(
                                "Content-Range",
                                format!("bytes {}-{}/{}", start, start + length - 1, size),
                            );
                        }
                        Ok(apply_signed_url_security_headers(builder)
                            .body(Either::Right(StreamBody::new(stream)))
                            .unwrap())
                    }
                    _ => Ok(Response::builder()
                        .status(status)
                        .header("Content-Type", "application/json")
                        .body(Either::Left(Full::new(Bytes::from_static(
                            b"{\"error\":\"export_unavailable\"}",
                        ))))
                        .unwrap()),
                };
            }
            ExportOutcome::BadRequest(why) => {
                let body = format!("{{\"error\":\"{}\"}}", why);
                return Ok(Response::builder()
                    .status(auth_status)
                    .header("Content-Type", "application/json")
                    .body(Either::Left(Full::new(Bytes::from(body))))
                    .unwrap());
            }
            ExportOutcome::Denied(_) => {
                // Token verification FAILED (forged / expired / scope mismatch).
                // Charge the strict invalid-token bucket: exhausted → 429 instead
                // of 403. `handle_ml_studio_export_url` already emitted the
                // "denied" audit above.
                if let Some(resp) = charge_invalid_signed_token(
                    &db,
                    &client_ip,
                    user_agent.as_deref(),
                    "/ml-studio/exports",
                ) {
                    return Ok(resp);
                }
                return Ok(Response::builder()
                    .status(auth_status)
                    .header("Content-Type", "application/json")
                    .body(Either::Left(Full::new(Bytes::from_static(
                        b"{\"error\":\"export_denied\"}",
                    ))))
                    .unwrap());
            }
        }
    }

    // GET /project-studio/exports/<ref>?token=&exp=&ref= — signed URL for a
    // Project Studio export archive (zip). HMAC-only auth, exactly like
    // /ml-studio/exports/. The ref IS the archive identity — there is no
    // catalogue table — so existence + containment are decided on the filesystem.
    if method == Method::GET
        && path.starts_with("/project-studio/exports/")
        && path.len() > "/project-studio/exports/".len()
    {
        use crate::api::project_studio_export::{
            export_download_filename, handle_project_studio_export_url, parse_query,
            read_export_file, ExportFileOutcome, ExportOutcome, RequestContext,
        };
        if let Err(resp) = reject_unauth_get_body(req.headers()) {
            return Ok(resp);
        }
        if let Err(resp) = check_signed_url_rate_limit(
            &db,
            &client_ip,
            user_agent.as_deref(),
            "/project-studio/exports",
        ) {
            return Ok(resp);
        }
        // Capture Range BEFORE `req` is released — the streamed response below
        // needs it, and `size` (required to resolve the range) is only known
        // after the file is opened.
        let range_header = req
            .headers()
            .get(hyper::header::RANGE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        drop(req);
        let path_ref = path.strip_prefix("/project-studio/exports/").unwrap_or("");
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
        let issuer = crate::services::project_studio_export_url_issuer();
        let ctx = RequestContext {
            source_ip: Some(client_ip.as_str()),
            user_agent: user_agent.as_deref(),
        };
        let outcome = handle_project_studio_export_url(path_ref, &q, issuer, &db, ctx);
        let auth_status = outcome.http_status();
        match outcome {
            ExportOutcome::Ok => {
                let file_outcome = read_export_file(&db, path_ref, ctx).await;
                let status = file_outcome.http_status();
                return match file_outcome {
                    ExportFileOutcome::Ok { mut file, size } => {
                        // STREAMED, never slurped: an export can be gigabytes.
                        // A paused download resumes via Range → 206 + Content-Range.
                        let (code, start, length) =
                            match parse_byte_range(range_header.as_deref(), size) {
                                Some((s, e)) => (206u16, s, e - s + 1),
                                None => (200u16, 0, size),
                            };
                        if start > 0 {
                            use tokio::io::AsyncSeekExt;
                            if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
                                return Ok(Response::builder()
                                    .status(500)
                                    .header("Content-Type", "application/json")
                                    .body(Either::Left(Full::new(Bytes::from_static(
                                        b"{\"error\":\"export_unavailable\"}",
                                    ))))
                                    .unwrap());
                            }
                        }
                        let stream: SseStream = Box::pin(ranged_file_stream(file, length));
                        let mut builder = Response::builder()
                            .status(code)
                            .header("Content-Type", "application/zip")
                            .header("Accept-Ranges", "bytes")
                            .header("Content-Length", length.to_string())
                            .header(
                                "Content-Disposition",
                                format!(
                                    "attachment; filename=\"{}\"",
                                    export_download_filename(path_ref)
                                ),
                            );
                        if code == 206 {
                            builder = builder.header(
                                "Content-Range",
                                format!("bytes {}-{}/{}", start, start + length - 1, size),
                            );
                        }
                        Ok(apply_signed_url_security_headers(builder)
                            .body(Either::Right(StreamBody::new(stream)))
                            .unwrap())
                    }
                    _ => Ok(Response::builder()
                        .status(status)
                        .header("Content-Type", "application/json")
                        .body(Either::Left(Full::new(Bytes::from_static(
                            b"{\"error\":\"export_unavailable\"}",
                        ))))
                        .unwrap()),
                };
            }
            ExportOutcome::BadRequest(why) => {
                let body = format!("{{\"error\":\"{}\"}}", why);
                return Ok(Response::builder()
                    .status(auth_status)
                    .header("Content-Type", "application/json")
                    .body(Either::Left(Full::new(Bytes::from(body))))
                    .unwrap());
            }
            ExportOutcome::Denied(_) => {
                // Token verification FAILED (forged / expired / scope mismatch).
                // Charge the strict invalid-token bucket: exhausted → 429 instead
                // of 403. `handle_project_studio_export_url` already emitted the
                // "denied" audit above.
                if let Some(resp) = charge_invalid_signed_token(
                    &db,
                    &client_ip,
                    user_agent.as_deref(),
                    "/project-studio/exports",
                ) {
                    return Ok(resp);
                }
                return Ok(Response::builder()
                    .status(auth_status)
                    .header("Content-Type", "application/json")
                    .body(Either::Left(Full::new(Bytes::from_static(
                        b"{\"error\":\"export_denied\"}",
                    ))))
                    .unwrap());
            }
        }
    }

    // GET /models/manifest/<bundle_ref> — vision model-bundle manifest for
    // instance-to-instance distribution. Auth: signed query (?token=&exp=&ref=,
    // same shape as /recordings) OR `Authorization: Bearer <api-key>` with an
    // explicit ('model_bundle', <bundle_ref>) allow scope. Per-file URLs inside
    // the manifest mirror the auth mode (signed vs token-less + Bearer).
    if method == Method::GET
        && path.starts_with("/models/manifest/")
        && path.len() > "/models/manifest/".len()
    {
        use crate::api::model_bundle::{
            handle_manifest, BundleAuth, ManifestOutcome, RequestContext,
        };
        if let Err(resp) = reject_unauth_get_body(req.headers()) {
            return Ok(resp);
        }
        if let Err(resp) =
            check_signed_url_rate_limit(&db, &client_ip, user_agent.as_deref(), "/models")
        {
            return Ok(resp);
        }
        let bearer_token = models_bearer_token(req.headers());
        drop(req);
        let bundle_ref = path.strip_prefix("/models/manifest/").unwrap_or("");
        let ctx = RequestContext {
            source_ip: Some(client_ip.as_str()),
            user_agent: user_agent.as_deref(),
        };
        let (query_storage, key_storage);
        let auth = match resolve_models_auth(
            &db,
            &settings_cipher,
            bearer_token,
            &query_string,
            bundle_ref,
            ctx,
        ) {
            Ok(ModelsAuth::Signed(q)) => {
                query_storage = q;
                BundleAuth::Signed(&query_storage)
            }
            Ok(ModelsAuth::ApiKey(uid)) => {
                key_storage = uid;
                BundleAuth::ApiKey {
                    key_uid: &key_storage,
                }
            }
            Err(resp) => return Ok(resp),
        };
        let issuer = crate::services::model_bundle_url_issuer();
        let outcome = handle_manifest(bundle_ref, &auth, issuer, &db, ctx).await;
        let status = outcome.http_status();
        return match outcome {
            ManifestOutcome::Ok { body } => Ok(apply_signed_url_security_headers(
                Response::builder()
                    .status(status)
                    .header("Content-Type", "application/json"),
            )
            .body(Either::Left(Full::new(Bytes::from(body))))
            .unwrap()),
            ManifestOutcome::BadRequest(why) => {
                let body = format!("{{\"error\":\"{}\"}}", why);
                Ok(Response::builder()
                    .status(status)
                    .header("Content-Type", "application/json")
                    .body(Either::Left(Full::new(Bytes::from(body))))
                    .unwrap())
            }
            ManifestOutcome::Denied(_)
            | ManifestOutcome::Forbidden(_)
            | ManifestOutcome::NotFound
            | ManifestOutcome::InternalError(_) => Ok(Response::builder()
                .status(status)
                .header("Content-Type", "application/json")
                .body(Either::Left(Full::new(Bytes::from_static(
                    b"{\"error\":\"model_bundle_denied\"}",
                ))))
                .unwrap()),
        };
    }

    // GET /models/file/<bundle_ref>/<name> — per-file download. Signed query
    // derived from a manifest token OR the same Bearer API key that fetched
    // the manifest. Bodies stream in chunks (weights reach ~126 MB) through
    // the same StreamBody slot SSE uses.
    if method == Method::GET
        && path.starts_with("/models/file/")
        && path.len() > "/models/file/".len()
    {
        use crate::api::model_bundle::{
            file_stream, handle_file, BundleAuth, FileOutcome, RequestContext,
        };
        if let Err(resp) = reject_unauth_get_body(req.headers()) {
            return Ok(resp);
        }
        if let Err(resp) =
            check_signed_url_rate_limit(&db, &client_ip, user_agent.as_deref(), "/models")
        {
            return Ok(resp);
        }
        let bearer_token = models_bearer_token(req.headers());
        drop(req);
        let rest = path.strip_prefix("/models/file/").unwrap_or("");
        let Some((bundle_ref, name)) = rest
            .split_once('/')
            .filter(|(b, n)| !b.is_empty() && !n.is_empty())
        else {
            return Ok(Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .header("Content-Type", "application/json")
                .body(Either::Left(Full::new(Bytes::from_static(
                    b"{\"error\":\"invalid_path\"}",
                ))))
                .unwrap());
        };
        let ctx = RequestContext {
            source_ip: Some(client_ip.as_str()),
            user_agent: user_agent.as_deref(),
        };
        let audit_ref = format!("{}/{}", bundle_ref, name);
        let (query_storage, key_storage);
        let auth = match resolve_models_auth(
            &db,
            &settings_cipher,
            bearer_token,
            &query_string,
            &audit_ref,
            ctx,
        ) {
            Ok(ModelsAuth::Signed(q)) => {
                query_storage = q;
                BundleAuth::Signed(&query_storage)
            }
            Ok(ModelsAuth::ApiKey(uid)) => {
                key_storage = uid;
                BundleAuth::ApiKey {
                    key_uid: &key_storage,
                }
            }
            Err(resp) => return Ok(resp),
        };
        let issuer = crate::services::model_bundle_url_issuer();
        let outcome = handle_file(bundle_ref, name, &auth, issuer, &db, ctx).await;
        let status = outcome.http_status();
        return match outcome {
            FileOutcome::Ok { file, size } => {
                // Handle was opened + fstat'ed during authorization (O_NOFOLLOW)
                // — stream that same handle, never re-open by path.
                let stream: SseStream = Box::pin(file_stream(file));
                Ok(apply_signed_url_security_headers(
                    Response::builder()
                        .status(status)
                        .header("Content-Type", "application/octet-stream")
                        .header("Content-Length", size.to_string()),
                )
                .body(Either::Right(StreamBody::new(stream)))
                .unwrap())
            }
            FileOutcome::BadRequest(why) => {
                let body = format!("{{\"error\":\"{}\"}}", why);
                Ok(Response::builder()
                    .status(status)
                    .header("Content-Type", "application/json")
                    .body(Either::Left(Full::new(Bytes::from(body))))
                    .unwrap())
            }
            FileOutcome::Denied(_)
            | FileOutcome::Forbidden(_)
            | FileOutcome::NotFound
            | FileOutcome::PathTraversal
            | FileOutcome::IoError => Ok(Response::builder()
                .status(status)
                .header("Content-Type", "application/json")
                .body(Either::Left(Full::new(Bytes::from_static(
                    b"{\"error\":\"model_bundle_denied\"}",
                ))))
                .unwrap()),
        };
    }

    // GET /ml-studio/share/<project_id>/manifest — cross-instance ML Studio
    // project share manifest. Auth mirrors /models/manifest: signed query
    // (?token=&exp=&ref=) OR `Authorization: Bearer <api-key>` with an explicit
    // ('ml_studio_export', <project_id>) allow scope. The archive URL inside the
    // manifest mirrors the auth mode (per-ref signed vs token-less + Bearer).
    if method == Method::GET
        && path.starts_with("/ml-studio/share/")
        && path.ends_with("/manifest")
        && path.len() > "/ml-studio/share//manifest".len()
    {
        use crate::api::ml_studio_share::{
            handle_share_manifest, RequestContext, ShareAuth, ShareManifestOutcome,
        };
        if let Err(resp) = reject_unauth_get_body(req.headers()) {
            return Ok(resp);
        }
        if let Err(resp) =
            check_signed_url_rate_limit(&db, &client_ip, user_agent.as_deref(), "/ml-studio/share")
        {
            return Ok(resp);
        }
        let bearer_token = models_bearer_token(req.headers());
        drop(req);
        let project_id = path
            .strip_prefix("/ml-studio/share/")
            .and_then(|r| r.strip_suffix("/manifest"))
            .unwrap_or("");
        let mb_ctx = crate::api::model_bundle::RequestContext {
            source_ip: Some(client_ip.as_str()),
            user_agent: user_agent.as_deref(),
        };
        let ctx = RequestContext {
            source_ip: Some(client_ip.as_str()),
            user_agent: user_agent.as_deref(),
        };
        let (query_storage, key_storage);
        let auth = match resolve_models_auth(
            &db,
            &settings_cipher,
            bearer_token,
            &query_string,
            project_id,
            mb_ctx,
        ) {
            Ok(ModelsAuth::Signed(q)) => {
                query_storage = q;
                ShareAuth::Signed(&query_storage)
            }
            Ok(ModelsAuth::ApiKey(uid)) => {
                key_storage = uid;
                ShareAuth::ApiKey {
                    key_uid: &key_storage,
                }
            }
            Err(resp) => return Ok(resp),
        };
        let issuer = crate::services::ml_studio_export_url_issuer();
        let outcome = handle_share_manifest(project_id, &auth, issuer, &db, ctx).await;
        let status = outcome.http_status();
        return match outcome {
            ShareManifestOutcome::Ok { body } => Ok(apply_signed_url_security_headers(
                Response::builder()
                    .status(status)
                    .header("Content-Type", "application/json"),
            )
            .body(Either::Left(Full::new(Bytes::from(body))))
            .unwrap()),
            ShareManifestOutcome::BadRequest(why) => {
                let body = format!("{{\"error\":\"{}\"}}", why);
                Ok(Response::builder()
                    .status(status)
                    .header("Content-Type", "application/json")
                    .body(Either::Left(Full::new(Bytes::from(body))))
                    .unwrap())
            }
            ShareManifestOutcome::Denied(_)
            | ShareManifestOutcome::Forbidden(_)
            | ShareManifestOutcome::NotFound
            | ShareManifestOutcome::InternalError(_) => Ok(Response::builder()
                .status(status)
                .header("Content-Type", "application/json")
                .body(Either::Left(Full::new(Bytes::from_static(
                    b"{\"error\":\"ml_studio_share_denied\"}",
                ))))
                .unwrap()),
        };
    }

    // GET /ml-studio/share/<project_id>/archive — on-demand project export ZIP
    // (up to ~8 GB) for the same signed query OR Bearer API key that fetched the
    // manifest. Range is supported so a paused download resumes; the archive is
    // built/cached on demand behind a global build semaphore.
    if method == Method::GET
        && path.starts_with("/ml-studio/share/")
        && path.ends_with("/archive")
        && path.len() > "/ml-studio/share//archive".len()
    {
        use crate::api::ml_studio_share::{
            archive_download_filename, handle_share_archive, RequestContext, ShareArchiveOutcome,
            ShareAuth,
        };
        if let Err(resp) = reject_unauth_get_body(req.headers()) {
            return Ok(resp);
        }
        if let Err(resp) =
            check_signed_url_rate_limit(&db, &client_ip, user_agent.as_deref(), "/ml-studio/share")
        {
            return Ok(resp);
        }
        // Capture Range BEFORE `req` is released — the streamed response needs
        // it, and `size` (to resolve the range) is only known after the open.
        let range_header = req
            .headers()
            .get(hyper::header::RANGE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let bearer_token = models_bearer_token(req.headers());
        drop(req);
        let project_id = path
            .strip_prefix("/ml-studio/share/")
            .and_then(|r| r.strip_suffix("/archive"))
            .unwrap_or("");
        let audit_ref = format!("{}/archive", project_id);
        let mb_ctx = crate::api::model_bundle::RequestContext {
            source_ip: Some(client_ip.as_str()),
            user_agent: user_agent.as_deref(),
        };
        let ctx = RequestContext {
            source_ip: Some(client_ip.as_str()),
            user_agent: user_agent.as_deref(),
        };
        let (query_storage, key_storage);
        let auth = match resolve_models_auth(
            &db,
            &settings_cipher,
            bearer_token,
            &query_string,
            &audit_ref,
            mb_ctx,
        ) {
            Ok(ModelsAuth::Signed(q)) => {
                query_storage = q;
                ShareAuth::Signed(&query_storage)
            }
            Ok(ModelsAuth::ApiKey(uid)) => {
                key_storage = uid;
                ShareAuth::ApiKey {
                    key_uid: &key_storage,
                }
            }
            Err(resp) => return Ok(resp),
        };
        let issuer = crate::services::ml_studio_export_url_issuer();
        let outcome = handle_share_archive(project_id, &auth, issuer, &db, ctx).await;
        let status = outcome.http_status();
        return match outcome {
            ShareArchiveOutcome::Ok {
                mut file,
                size,
                sha256,
            } => {
                // STREAMED, never slurped: an archive can be gigabytes. A paused
                // download resumes via Range → 206 + Content-Range.
                let (code, start, length) = match parse_byte_range(range_header.as_deref(), size) {
                    Some((s, e)) => (206u16, s, e - s + 1),
                    None => (200u16, 0, size),
                };
                if start > 0 {
                    use tokio::io::AsyncSeekExt;
                    if file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
                        return Ok(Response::builder()
                            .status(500)
                            .header("Content-Type", "application/json")
                            .body(Either::Left(Full::new(Bytes::from_static(
                                b"{\"error\":\"ml_studio_share_denied\"}",
                            ))))
                            .unwrap());
                    }
                }
                let stream: SseStream = Box::pin(ranged_file_stream(file, length));
                let mut builder = Response::builder()
                    .status(code)
                    .header("Content-Type", "application/zip")
                    .header("Accept-Ranges", "bytes")
                    .header("Content-Length", length.to_string())
                    // Whole-archive digest (NOT the partial range on a 206) so the
                    // puller verifies integrity without the manifest carrying sha256.
                    .header("X-Archive-Sha256", sha256)
                    .header(
                        "Content-Disposition",
                        format!(
                            "attachment; filename=\"{}\"",
                            archive_download_filename(project_id)
                        ),
                    );
                if code == 206 {
                    builder = builder.header(
                        "Content-Range",
                        format!("bytes {}-{}/{}", start, start + length - 1, size),
                    );
                }
                Ok(apply_signed_url_security_headers(builder)
                    .body(Either::Right(StreamBody::new(stream)))
                    .unwrap())
            }
            ShareArchiveOutcome::BadRequest(why) => {
                let body = format!("{{\"error\":\"{}\"}}", why);
                Ok(Response::builder()
                    .status(status)
                    .header("Content-Type", "application/json")
                    .body(Either::Left(Full::new(Bytes::from(body))))
                    .unwrap())
            }
            ShareArchiveOutcome::Denied(_)
            | ShareArchiveOutcome::Forbidden(_)
            | ShareArchiveOutcome::NotFound
            | ShareArchiveOutcome::PathTraversal
            | ShareArchiveOutcome::IoError => Ok(Response::builder()
                .status(status)
                .header("Content-Type", "application/json")
                .body(Either::Left(Full::new(Bytes::from_static(
                    b"{\"error\":\"ml_studio_share_denied\"}",
                ))))
                .unwrap()),
        };
    }

    // GET /legal/<doc_id>?token=&exp=&org=&nonce= — HMAC-signed download of a
    // RODO/GDPR PDF artifact. HMAC-only auth, same shape as `/recordings`
    // plus `org` + `nonce` extra fields (the legal binding is per-tenant +
    // unguessable). F2 P8.c.
    if method == Method::GET && path.starts_with("/legal/") && path.len() > "/legal/".len() {
        use crate::api::legal::{
            handle_legal_url, parse_query, read_legal_file, LegalFileOutcome, LegalOutcome,
            RequestContext,
        };
        if let Err(resp) = reject_unauth_get_body(req.headers()) {
            return Ok(resp);
        }
        if let Err(resp) =
            check_signed_url_rate_limit(&db, &client_ip, user_agent.as_deref(), "/legal")
        {
            return Ok(resp);
        }
        drop(req);
        let path_doc_id = path.strip_prefix("/legal/").unwrap_or("");
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
        let issuer = crate::services::legal_url_issuer();
        let ctx = RequestContext {
            source_ip: Some(client_ip.as_str()),
            user_agent: user_agent.as_deref(),
        };
        let outcome = handle_legal_url(path_doc_id, &q, issuer, &db, ctx);
        let auth_status = outcome.http_status();
        match outcome {
            LegalOutcome::Ok {
                org_id,
                pdf_path,
                content_hash,
                generated_at,
            } => {
                let file_outcome = read_legal_file(&db, path_doc_id, &org_id, &pdf_path, ctx).await;
                let status = file_outcome.http_status();
                return match file_outcome {
                    LegalFileOutcome::Ok { bytes } => Ok(apply_signed_url_security_headers(
                        Response::builder()
                            .status(status)
                            .header("Content-Type", "application/pdf")
                            .header("X-Legal-Hash", content_hash)
                            .header("X-Legal-Generated-At", generated_at.to_string()),
                    )
                    .body(Either::Left(Full::new(Bytes::from(bytes))))
                    .unwrap()),
                    _ => Ok(Response::builder()
                        .status(status)
                        .header("Content-Type", "application/json")
                        .body(Either::Left(Full::new(Bytes::from_static(
                            b"{\"error\":\"legal_unavailable\"}",
                        ))))
                        .unwrap()),
                };
            }
            LegalOutcome::BadRequest(why) => {
                let body = format!("{{\"error\":\"{}\"}}", why);
                return Ok(Response::builder()
                    .status(400)
                    .header("Content-Type", "application/json")
                    .body(Either::Left(Full::new(Bytes::from(body))))
                    .unwrap());
            }
            LegalOutcome::Denied(_)
            | LegalOutcome::Revoked
            | LegalOutcome::NotFound
            | LegalOutcome::InternalError(_) => {
                return Ok(Response::builder()
                    .status(auth_status)
                    .header("Content-Type", "application/json")
                    .body(Either::Left(Full::new(Bytes::from_static(
                        b"{\"error\":\"legal_denied\"}",
                    ))))
                    .unwrap());
            }
        }
    }

    // Pliki statyczne - sciezki poza /api/
    if method == Method::GET && !path.starts_with("/api/") {
        let if_none_match = req
            .headers()
            .get("if-none-match")
            .and_then(|v| v.to_str().ok());
        let (status, content_type, body, etag) = static_files::serve(&path);
        return Ok(make_static_response_with_origin(
            &path,
            status,
            content_type,
            body,
            cors_origin.as_deref(),
            &etag,
            if_none_match,
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
    headers: &hyper::HeaderMap,
    db: &DbPool,
    settings_cipher: &crate::crypto::SettingsCipher,
) -> Option<(String, Option<String>)> {
    let jwt_secret = db::repository::get_setting_secure(db, "jwt_secret", settings_cipher)
        .ok()
        .flatten()?;

    let proto_header = headers
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())?;

    let token = proto_header
        .split(',')
        .find(|s| s.trim().starts_with("bearer."))
        .and_then(|s| s.trim().strip_prefix("bearer."))?;

    let claims = auth::validate_jwt(token, &jwt_secret).ok()?;

    // Zero Trust: role z DB lookup, nie z JWT (chroni przed token-replay z
    // odebranymi uprawnieniami). A still-valid JWT must NOT mint a session when
    // the backing account was deleted or deactivated — resolve the account and
    // reject (no session) unless it exists AND is active.
    let account = db::repository::get_user_account_by_id(db, &claims.user_id)
        .ok()
        .flatten()?;
    if !account.is_active {
        return None;
    }
    // is_admin wymusza "admin"; poza tym honorujemy kolumnę `role`
    // (np. "power_user" przypisany w UI), z fallbackiem do "user".
    let role = if account.is_admin || account.role == "admin" {
        "admin".to_string()
    } else if account.role == "power_user" {
        "power_user".to_string()
    } else {
        "user".to_string()
    };

    Some((claims.user_id, Some(role)))
}

/// What one startup reconciliation pass repaired. Returned rather than only
/// logged so the pass is testable as a whole: §13 promises a crash leaves
/// RESUMABLE state, and a promise nobody exercises is a comment.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CodeStudioRecovery {
    /// Workspaces stuck mid-provisioning, moved to `error` with a retry hint.
    pub workspaces_failed: usize,
    /// Sessions left `creating`/`running`/`waiting_user`/`closing`, now
    /// `interrupted`.
    pub sessions_interrupted: usize,
    /// Cached status columns re-derived from the event tail (§13.3).
    pub projections_corrected: usize,
    /// Shells inherited from a previous life, killed and reaped.
    pub terminals_reaped: usize,
    /// `cli_instances` rows still claiming to be live, now `reaped` (D2).
    pub cli_instances_reaped: usize,
    /// Sandbox rows a crash left occupying a shared profile, now closed.
    pub sandboxes_reconciled: usize,
    /// Interrupted effects whose postcondition could be PROVEN, now `completed`
    /// (§13.1).
    pub operations_completed: usize,
    /// Interrupted effects that are idempotent and whose precondition still
    /// holds. They stay `pending` on purpose — the row is the coordinator's
    /// instruction to re-issue them, and this pass never executes anything.
    pub operations_retryable: usize,
    /// Interrupted effects nothing could prove. They wait for a person, and a
    /// lingering one is what §22 alerts on.
    pub operations_unknown: usize,
}

/// Code Studio startup reconciliation (§4.2, §6, §13.3). Runs ONCE per boot,
/// before the listener starts serving, and never fails the boot: a workspace
/// that cannot be reconciled is reported and skipped, because a node that
/// refuses to start is strictly worse than a node with one workspace in
/// `error`.
///
/// The order is the point:
///   1. reap orphan terminals FIRST — a shell from the previous process still
///      holds the worktree and would keep writing into it while we reconcile;
///   2. close the sandboxes of the previous life, for the same reason and
///      because their rows keep a shared profile occupied for a session that is
///      about to be resumed;
///   3. verify the projection against the timeline, while the cached statuses
///      still say what the crashed process believed;
///   4. settle the effect journal against the now-quiet worktree — every step
///      above exists so that what a probe reads is the state the crash left,
///      not something a survivor is still writing;
///   5. only then sweep live sessions to `interrupted` — `verify_projection`
///      deliberately leaves `interrupted` alone, so doing this first would make
///      step 3 a no-op.
///
/// Blocking (SQLite + filesystem): callers run it on a blocking thread.
pub fn reconcile_code_studio(db: &DbPool, node_id: &str) -> CodeStudioRecovery {
    use crate::code_studio::models::ExecMode;
    use crate::code_studio::sandbox::SandboxManager;
    use crate::code_studio::{
        cli_bridge, events, operations, provisioning, repository, session, workspace_db,
    };

    let mut report = CodeStudioRecovery::default();

    match provisioning::reconcile_interrupted(db, node_id) {
        Ok(count) => report.workspaces_failed = count,
        Err(e) => warn!("code studio: provisioning reconciliation failed: {e:#}"),
    }

    let workspaces = match repository::list_workspaces_on_node(db, node_id) {
        Ok(rows) => rows,
        Err(e) => {
            warn!("code studio: cannot list owned workspaces: {e:#}");
            return report;
        }
    };

    for workspace in workspaces {
        match crate::dispatch::stream_handlers::code_studio_terminal_registry(&workspace.id)
            .and_then(|registry| registry.reap_orphans())
        {
            Ok(reaped) => report.terminals_reaped += reaped.len(),
            Err(e) => warn!(
                "code studio: terminal reaping failed for workspace '{}': {e:#}",
                workspace.id
            ),
        }

        let pool = match workspace_db::open(&workspace.id) {
            Ok(pool) => pool,
            Err(e) => {
                // Expected for a workspace whose provisioning never created the
                // directory — the registry row was just moved to `error` above.
                warn!(
                    "code studio: workspace '{}' has no runtime database: {e:#}",
                    workspace.id
                );
                continue;
            }
        };

        let sessions = match session::list_session_ids(&pool) {
            Ok(ids) => ids,
            Err(e) => {
                warn!(
                    "code studio: cannot list sessions of workspace '{}': {e:#}",
                    workspace.id
                );
                Vec::new()
            }
        };

        // Reconciliation only closes rows and drops the layers the temporary
        // sweep already lost, so it needs no container configuration — nothing
        // here starts a runtime.
        let sandboxes = ExecMode::from_slug(&workspace.exec_mode)
            .ok_or_else(|| anyhow::anyhow!("unknown exec mode '{}'", workspace.exec_mode))
            .and_then(|exec_mode| SandboxManager::for_workspace(&workspace.id, exec_mode, None));
        let sandboxes = match sandboxes {
            Ok(manager) => Some(manager),
            Err(e) => {
                warn!(
                    "code studio: no sandbox manager for workspace '{}': {e:#}",
                    workspace.id
                );
                None
            }
        };

        for session_id in &sessions {
            if let Some(manager) = &sandboxes {
                match manager.reconcile_after_restart(&pool, session_id) {
                    Ok(closed) => report.sandboxes_reconciled += closed,
                    Err(e) => warn!(
                        "code studio: sandbox reconciliation failed for session '{session_id}': {e:#}"
                    ),
                }
            }

            match events::verify_projection(&pool, session_id) {
                Ok(corrections) => {
                    for correction in &corrections {
                        info!(
                            "code studio: {} '{}' status '{}' -> '{}' (from events)",
                            correction.entity,
                            correction.id,
                            correction.projected,
                            correction.from_events
                        );
                    }
                    report.projections_corrected += corrections.len();
                }
                Err(e) => {
                    warn!("code studio: projection check failed for session '{session_id}': {e:#}")
                }
            }

            // §13.1. Without this the journal is write-only across a restart:
            // every effect a crash interrupted stays `pending` for the life of
            // the node, and the `unknown` queue a person is supposed to work
            // through never fills.
            match operations::SessionProbe::for_session(&workspace.id, session_id)
                .and_then(|probe| operations::reconcile(&pool, session_id, &probe))
            {
                Ok(settled) => {
                    report.operations_completed += settled.completed();
                    report.operations_retryable += settled.retryable();
                    report.operations_unknown += settled.unknown();
                    if settled.unknown() > 0 {
                        warn!(
                            "code studio: session '{session_id}' left {} operation(s) nobody can \
                             verify; they need a decision",
                            settled.unknown()
                        );
                    }
                }
                Err(e) => warn!(
                    "code studio: operation reconciliation failed for session '{session_id}': {e:#}"
                ),
            }
        }

        // A vendor CLI instance from the previous life is a process this Core
        // does not supervise, and the bridge kills those at ITS startup (D2).
        // Recording them as `reaped` is what makes the ticket registry's
        // "a ticket outlives nothing" promise true across a restart: there is
        // no row left claiming a run that could still be spending.
        match cli_bridge::reap_orphaned_instances(&pool) {
            Ok(count) => report.cli_instances_reaped += count,
            Err(e) => warn!(
                "code studio: CLI instance reaping failed for workspace '{}': {e:#}",
                workspace.id
            ),
        }

        match session::reconcile_interrupted(&pool) {
            Ok(count) => report.sessions_interrupted += count,
            Err(e) => warn!(
                "code studio: session reconciliation failed for workspace '{}': {e:#}",
                workspace.id
            ),
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn header_map_with_bearer(token: &str) -> hyper::HeaderMap {
        let mut headers = hyper::HeaderMap::new();
        headers.insert(
            "sec-websocket-protocol",
            format!("bearer.{token}").parse().expect("header value"),
        );
        headers
    }

    /// §13 — the durability promise, exercised end to end.
    ///
    /// A crash leaves four kinds of debris, and this pass has to settle all of
    /// them in one go: a workspace stuck mid-provisioning, a session column
    /// that disagrees with its own timeline, a session the dead process still
    /// claims to be running, and a shell nobody owns. Before this pass was
    /// wired at boot, `reconcile_interrupted` and `verify_projection` had green
    /// unit tests and NO caller — the promise held only in the test suite.
    #[test]
    fn code_studio_recovery_settles_everything_a_crash_left_behind() {
        use crate::code_studio::events::{self, EventPayload, SessionEvent};
        use crate::code_studio::models::{
            AutonomyMode, EgressEnforcement, ExecMode, NewWorkspace, WorkspaceStatus,
        };
        use crate::code_studio::{paths as cs_paths, repository, workspace_db};

        let _guard = cs_paths::test_data_dir_guard();
        let data = tempdir().expect("data dir");
        crate::paths::set_category_override(
            crate::paths::StorageCategory::Data,
            Some(data.path().to_string_lossy().to_string()),
        );
        let registry = tempdir().expect("registry dir");
        let main_db = db::init(&registry.path().join("tentaflow.db")).expect("init db");

        let workspace = |id: &str| NewWorkspace {
            id: id.to_string(),
            org_id: "org-1".into(),
            owner_user_id: "u-1".into(),
            name: format!("Workspace {id}"),
            slug: id.to_string(),
            node_id: "node-1".into(),
            exec_mode: ExecMode::TrustedNative,
            container_image: None,
            egress_enforcement: EgressEnforcement::Unrestricted,
            repo_kind: "empty".into(),
            repo_url: None,
            repo_auth_kind: None,
            secret_ref: None,
            ssh_host_fingerprint: None,
            default_branch: None,
            target_branch: None,
            autonomy_ceiling: AutonomyMode::Normal,
            egress_policy: "local_only".into(),
            index_enabled: false,
            quota_disk_bytes: None,
            quota_sessions: None,
        };

        // (1) a workspace the previous process never finished provisioning.
        repository::create_workspace(&main_db, &workspace("recstuck")).expect("stuck workspace");

        // (2) a live workspace whose runtime db carries the rest of the debris.
        repository::create_workspace(&main_db, &workspace("reclive")).expect("live workspace");
        repository::set_status(&main_db, "reclive", WorkspaceStatus::Active, None)
            .expect("activate");
        cs_paths::create_workspace_layout("reclive").expect("layout");
        let pool = workspace_db::open("reclive").expect("workspace db");
        {
            let conn = pool.write().expect("write");
            conn.execute(
                "INSERT INTO sessions (id, workspace_id, user_id, title, branch, autonomy_mode, \
                  flow_id, flow_version_id, status, created_at, updated_at) \
                 VALUES ('recsess', 'reclive', 'u-1', 'S', 'cs/u/1', 'normal', 'f', 'v', \
                  'running', datetime('now'), datetime('now'))",
                [],
            )
            .expect("session row");
            // The projection lies: the column says the run failed, the timeline
            // (appended below) says it started and never finished.
            conn.execute(
                "INSERT INTO session_runs (run_id, session_id, ordinal, kind, trigger, status) \
                 VALUES ('recrun', 'recsess', 1, 'root', 'user', 'failed')",
                [],
            )
            .expect("run row");
            // A vendor CLI instance the dead process was supervising. Nobody
            // supervises it now, and the bridge kills its own children at
            // startup (D2), so a row still claiming `ready` describes a process
            // that no longer exists.
            conn.execute(
                "INSERT INTO cli_instances (id, session_id, run_id, engine_id, service_id, \
                  vendor_session_id, model, ticket_id, status, last_seq, started_at) \
                 VALUES ('recinst', 'recsess', 'recrun', 'codex', 1, 'v-1', 'gpt-5', NULL, \
                  'ready', 0, datetime('now'))",
                [],
            )
            .expect("cli instance row");
        }
        events::append(
            &pool,
            "recsess",
            SessionEvent::new(
                "rec-run-start",
                EventPayload::RunStarted {
                    run_id: "recrun".into(),
                    kind: "root".into(),
                    trigger: "user".into(),
                },
            ),
        )
        .expect("append run start");

        let report = reconcile_code_studio(&main_db, "node-1");

        assert_eq!(
            report.workspaces_failed, 1,
            "the interrupted provisioning must become a retryable error"
        );
        assert!(
            report.projections_corrected >= 1,
            "the stale run column must be re-derived from the timeline: {report:?}"
        );
        assert_eq!(
            report.sessions_interrupted, 1,
            "the session the dead process claimed to be running must be released"
        );
        assert_eq!(
            report.cli_instances_reaped, 1,
            "a CLI instance nobody supervises must not stay 'ready': {report:?}"
        );

        let stuck = repository::get_workspace(&main_db, "recstuck")
            .expect("read")
            .expect("row");
        assert_eq!(stuck.status, "error");
        assert!(stuck.status_detail.unwrap_or_default().contains("retry"));

        let (session_status, run_status): (String, String) = {
            let conn = pool.read().expect("read");
            (
                conn.query_row("SELECT status FROM sessions WHERE id='recsess'", [], |r| {
                    r.get(0)
                })
                .expect("session status"),
                conn.query_row(
                    "SELECT status FROM session_runs WHERE run_id='recrun'",
                    [],
                    |r| r.get(0),
                )
                .expect("run status"),
            )
        };
        // The run was corrected from the events FIRST (`failed` -> `running`),
        // and only then did the sweep release the session. The order is not
        // cosmetic: `verify_projection` arbitrates a session column only while
        // it reads `idle`/`running`/`waiting_user`, so sweeping to
        // `interrupted` first would put every session out of its reach.
        assert_eq!(run_status, "running");
        assert_eq!(session_status, "interrupted");

        let instance_status: String = {
            let conn = pool.read().expect("read");
            conn.query_row(
                "SELECT status FROM cli_instances WHERE id='recinst'",
                [],
                |r| r.get(0),
            )
            .expect("instance status")
        };
        assert_eq!(
            instance_status, "reaped",
            "the honest state of an unsupervised CLI instance is 'reaped', not 'ready'"
        );

        // Idempotent: a second boot finds nothing left to settle.
        let again = reconcile_code_studio(&main_db, "node-1");
        assert_eq!(again, CodeStudioRecovery::default(), "{again:?}");

        workspace_db::close("reclive");
        crate::paths::set_category_override(crate::paths::StorageCategory::Data, None);
    }

    /// §13.1 — the effect journal and the sandboxes are settled BY THE BOOT
    /// PASS, not only by a function with a unit test.
    ///
    /// This is the same trap the test above records: `reconcile_interrupted`
    /// and `verify_projection` were green and uncalled until the pass was
    /// wired. `operations::reconcile` and `SandboxManager::reconcile_after_restart`
    /// repeated it exactly — every effect a crash interrupted stayed `pending`
    /// for the life of the node, and no operation ever reached the `unknown`
    /// queue a person is supposed to work through. Asserting the OUTCOME here
    /// is what makes deleting the call a failing test rather than a silent
    /// regression.
    #[test]
    fn the_boot_pass_settles_the_effect_journal_and_the_sandboxes() {
        use crate::code_studio::models::{
            AutonomyMode, EgressEnforcement, ExecMode, NewWorkspace, WorkspaceStatus,
        };
        use crate::code_studio::operations::{
            self, OpKind, OperationInput, OperationRequest, OperationStatus, OriginKind,
            Postcondition, Precondition,
        };
        use crate::code_studio::pep::Capability;
        use crate::code_studio::{
            artifacts, fs as cs_fs, paths as cs_paths, repository, workspace_db,
        };

        let _guard = cs_paths::test_data_dir_guard();
        let data = tempdir().expect("data dir");
        crate::paths::set_category_override(
            crate::paths::StorageCategory::Data,
            Some(data.path().to_string_lossy().to_string()),
        );
        let registry = tempdir().expect("registry dir");
        let main_db = db::init(&registry.path().join("tentaflow.db")).expect("init db");

        repository::create_workspace(
            &main_db,
            &NewWorkspace {
                id: "recops".into(),
                org_id: "org-1".into(),
                owner_user_id: "u-1".into(),
                name: "Workspace recops".into(),
                slug: "recops".into(),
                node_id: "node-1".into(),
                exec_mode: ExecMode::TrustedNative,
                container_image: None,
                egress_enforcement: EgressEnforcement::Unrestricted,
                repo_kind: "empty".into(),
                repo_url: None,
                repo_auth_kind: None,
                secret_ref: None,
                ssh_host_fingerprint: None,
                default_branch: None,
                target_branch: None,
                autonomy_ceiling: AutonomyMode::Normal,
                egress_policy: "local_only".into(),
                index_enabled: false,
                quota_disk_bytes: None,
                quota_sessions: None,
            },
        )
        .expect("workspace");
        repository::set_status(&main_db, "recops", WorkspaceStatus::Active, None)
            .expect("activate");
        cs_paths::create_workspace_layout("recops").expect("layout");
        let pool = workspace_db::open("recops").expect("workspace db");
        {
            let conn = pool.write().expect("write");
            conn.execute(
                "INSERT INTO sessions (id, workspace_id, user_id, title, branch, autonomy_mode, \
                  flow_id, flow_version_id, status, created_at, updated_at) \
                 VALUES ('recopsess', 'recops', 'u-1', 'S', 'cs/u/1', 'normal', 'f', 'v', \
                  'idle', datetime('now'), datetime('now'))",
                [],
            )
            .expect("session row");
            // A sandbox the previous process never released. Its row keeps the
            // shared profile occupied for a session that is being resumed.
            conn.execute(
                "INSERT INTO sandboxes \
                  (id, session_id, mount_access, network_access, lease_id, owner_run_id, state, \
                   ephemeral, created_at) \
                 VALUES ('sbx-1', 'recopsess', 'rw', 'none', 'lease-1', NULL, 'ready', 0, \
                  datetime('now'))",
                [],
            )
            .expect("sandbox row");
        }

        // The write whose content REACHED THE DISK before the crash: fully
        // verifiable, so the pass has to close it without asking anybody.
        let worktree =
            cs_paths::session_worktree_dir("recops", "recopsess").expect("worktree path");
        std::fs::create_dir_all(&worktree).expect("worktree");
        let content = b"written before the crash\n";
        let stored = artifacts::put(&pool, "recops", content, "file_content").expect("input blob");
        let landed = operations::begin(
            &pool,
            &OperationRequest {
                workspace_id: "recops".into(),
                session_id: "recopsess".into(),
                run_id: None,
                origin_kind: OriginKind::Ui,
                origin_id: "crash".into(),
                logical_step: "fs_write:boot.txt".into(),
                op_kind: OpKind::FsWrite,
                capability: Capability::FsWrite,
                input: OperationInput::FileContent {
                    path: "boot.txt".into(),
                    content_sha256: stored.sha256,
                    size_bytes: content.len() as u64,
                },
                precondition: Precondition::FileAbsent {
                    path: "boot.txt".into(),
                },
                postcondition: Postcondition::FileBlobIs {
                    path: "boot.txt".into(),
                    sha256: cs_fs::blob_sha(content),
                },
                profile: None,
            },
        )
        .expect("open the write");
        std::fs::write(worktree.join("boot.txt"), content).expect("the effect landed");

        // The push nothing on this node can verify: it must become a question.
        let unverifiable = operations::begin(
            &pool,
            &OperationRequest {
                workspace_id: "recops".into(),
                session_id: "recopsess".into(),
                run_id: None,
                origin_kind: OriginKind::Ui,
                origin_id: "crash".into(),
                logical_step: "git_push:cs/u/1".into(),
                op_kind: OpKind::GitPush,
                capability: Capability::GitPush,
                input: OperationInput::Git {
                    operation: "push".into(),
                    refname: Some("refs/heads/cs/u/1".into()),
                    remote: Some("ssh://git@example.invalid/o/r.git".into()),
                    oids: Vec::new(),
                },
                precondition: Precondition::None,
                postcondition: Postcondition::None,
                profile: None,
            },
        )
        .expect("open the push");

        let report = reconcile_code_studio(&main_db, "node-1");

        assert_eq!(
            report.operations_completed, 1,
            "an effect whose postcondition holds must be closed by the boot pass: {report:?}"
        );
        assert_eq!(
            report.operations_unknown, 1,
            "an unverifiable effect must reach the queue a person works through: {report:?}"
        );
        assert_eq!(
            report.sandboxes_reconciled, 1,
            "the sandbox of the previous life still occupies its profile: {report:?}"
        );
        assert_eq!(
            operations::get(&pool, &landed.op_id)
                .expect("read")
                .expect("row")
                .status,
            OperationStatus::Completed
        );
        assert_eq!(
            operations::get(&pool, &unverifiable.op_id)
                .expect("read")
                .expect("row")
                .status,
            OperationStatus::Unknown
        );
        let live_sandboxes: i64 = {
            let conn = pool.read().expect("read");
            conn.query_row(
                "SELECT COUNT(*) FROM sandboxes WHERE state != 'stopped'",
                [],
                |row| row.get(0),
            )
            .expect("count")
        };
        assert_eq!(live_sandboxes, 0);

        // A second boot finds nothing: `unknown` is not a work queue this pass
        // picks up again, and a closed sandbox stays closed.
        let again = reconcile_code_studio(&main_db, "node-1");
        assert_eq!(again.operations_completed, 0, "{again:?}");
        assert_eq!(again.operations_unknown, 0, "{again:?}");
        assert_eq!(again.sandboxes_reconciled, 0, "{again:?}");

        workspace_db::close("recops");
        crate::paths::set_category_override(crate::paths::StorageCategory::Data, None);
    }

    #[test]
    fn ws_session_rejected_for_unknown_and_inactive_accounts() {
        let dir = tempdir().expect("tempdir");
        let db = db::init(&dir.path().join("core.db")).expect("db init");
        let cipher = Arc::new(crate::crypto::SettingsCipher::new(&[7u8; 32]));

        let secret = "test-jwt-secret-please-rotate";
        db::repository::set_setting_secure(&db, "jwt_secret", secret, &cipher)
            .expect("store jwt secret");

        // A valid JWT whose user_id matches no account must NOT mint a session.
        let unknown_id = uuid::Uuid::new_v4().to_string();
        let unknown_token = auth::generate_jwt(&unknown_id, "ghost", secret, 1).expect("jwt");
        assert!(
            extract_ws_user_session(&header_map_with_bearer(&unknown_token), &db, &cipher)
                .is_none(),
            "deleted/unknown user must not get a session"
        );

        // An existing but deactivated account must also be rejected.
        let active_id =
            db::repository::create_user_account(&db, "alice", "hash", "Alice", "a@example.com")
                .expect("create user");
        db::repository::update_user_account(&db, &active_id, "Alice", "a@example.com", false)
            .expect("deactivate user");
        let inactive_token = auth::generate_jwt(&active_id, "alice", secret, 1).expect("jwt");
        assert!(
            extract_ws_user_session(&header_map_with_bearer(&inactive_token), &db, &cipher)
                .is_none(),
            "inactive user must not get a session"
        );

        // Re-activating the account restores the session.
        db::repository::update_user_account(&db, &active_id, "Alice", "a@example.com", true)
            .expect("reactivate user");
        let active_token = auth::generate_jwt(&active_id, "alice", secret, 1).expect("jwt");
        let session = extract_ws_user_session(&header_map_with_bearer(&active_token), &db, &cipher)
            .expect("active user gets a session");
        assert_eq!(session.0, active_id);
        assert_eq!(session.1.as_deref(), Some("user"));
    }
}
