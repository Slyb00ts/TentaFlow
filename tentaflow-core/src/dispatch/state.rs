// =============================================================================
// Plik: dispatch/state.rs
// Opis: AppState — kontener wszystkich shared resources do ktorych handlery
//       potrzebuja dostepu (DB, Router, MeshPeerStore, settings cipher itd).
//       Konstruowany raz w binarce, wstrzykiwany do HandlerContext przy
//       kazdym dispatchu.
// =============================================================================

use std::sync::Arc;

use crate::config::RouterConfig;
use crate::crypto::{SecretsCipher, SettingsCipher};
use crate::db::DbPool;
use crate::license::LicenseChecker;
use crate::license::StaticLicenseChecker;
use crate::mesh::peer_store::MeshPeerStore;
use crate::metrics::RouterMetrics;
use crate::routing::router::Router;
use crate::services::runtime::quic_handle::ServiceManager;
use crate::services::handles_cache::LiveHandlesCache;
use crate::services::mesh_registry::MeshServicesRegistry;
use crate::services::ports::PortAllocator;

/// Wszystkie shared resources serwera. Handlery uzywaja przez `ctx.state`.
pub struct AppState {
    pub db: DbPool,
    pub router: Arc<Router>,
    pub mesh_peer_store: MeshPeerStore,
    pub service_manager: Arc<ServiceManager>,
    pub metrics: Arc<RouterMetrics>,
    pub settings_cipher: Arc<SettingsCipher>,
    pub cipher: Arc<SecretsCipher>,
    pub quic_mesh: Option<Arc<crate::mesh::iroh_manager::IrohMeshManager>>,
    pub local_node_id: Arc<str>,
    pub mesh_security: Option<Arc<crate::mesh::security::MeshSecurity>>,
    pub permission_checker: Option<Arc<crate::addon::permissions::PermissionChecker>>,
    pub license: Arc<dyn LicenseChecker>,
    pub meeting_manager: Arc<crate::meeting::MeetingManager>,
    /// Active VNC tunnels for same-node websockify bridging. Keyed by server-
    /// generated tunnel_id (UUID). Instantiated per WS connection so tunnels
    /// die with the socket that spawned them.
    pub vnc_tunnels:
        Arc<dashmap::DashMap<String, crate::api::dashboard::vnc_tunnel::VncTunnelEntry>>,
    /// Snapshot zdrowia relay iroh + faktyczny adres bind. Aktualizowany w tle
    /// przez `mesh::relay_health::spawn_relay_health_monitor`. Czytany przez
    /// handler `NetworkRelayStatusRequest`. `None` gdy mesh w ogole nie wystartowal
    /// (np. przy testach lub `mesh.enabled=false`).
    pub mesh_relay_health: Option<Arc<parking_lot::RwLock<crate::mesh::relay_health::RelayHealth>>>,
    /// Shared port allocator owned by the supervisor and consumed by the
    /// unified service deploy pipeline (`services::deploy::deploy`). `None`
    /// only in tests / when the supervisor failed to initialise.
    pub port_allocator: Option<Arc<PortAllocator>>,
    /// In-memory aggregator of services advertised by every reachable remote
    /// mesh node. The local node is read directly from the `services` SQLite
    /// table; remote snapshots arrive via `MeshServicesGet`/`Announce`/`Update`
    /// messages handled in `mesh::pipeline`.
    pub mesh_services_registry: Arc<MeshServicesRegistry>,
    /// Lock-free cache of live runtime handles (HTTP / QUIC / Embedded) keyed
    /// by `(node_id, service_id)`. Populated by the supervisor (krok N7.2);
    /// consumed by routing call sites (krok N7.3). Empty in N7.1.
    pub live_handles: Arc<LiveHandlesCache>,
}

impl AppState {
    /// Test fixture — tempfile SQLite + minimalne real components.
    /// Wylacznie do uzytku w testach (unit + integration). Production NIGDY
    /// nie wola for_test — produkcja konstruuje AppState ze swoich realnych
    /// resources w handle_request (server.rs).
    pub fn for_test() -> Arc<Self> {
        use std::path::PathBuf;

        // In-memory SQLite via tempfile (rusqlite ":memory:" nie kompatybilny z r2d2 pool).
        let tmp = tempfile::NamedTempFile::new().expect("tempfile for test DB");
        let path: PathBuf = tmp.path().to_path_buf();
        std::mem::forget(tmp); // keep file alive for test duration
        let db = crate::db::init(&path).expect("test DB init");

        // Test cipher key — 32 bajty zer (test only).
        let cipher_hex = "0".repeat(64);
        let cipher = Arc::new(SecretsCipher::new(&cipher_hex).expect("test cipher"));
        let settings_cipher = Arc::new(SettingsCipher::new(&[0u8; 32]));
        let metrics = RouterMetrics::new();
        let config = RouterConfig::default();
        let router = Arc::new(Router::new(config, Some(db.clone())).expect("test router"));
        let service_manager = router.service_manager().clone();
        let live_handles = service_manager.live_handles.clone();
        let mesh_peer_store = MeshPeerStore::new();

        let meeting_manager =
            crate::meeting::MeetingManager::new(db.clone(), Some(service_manager.clone()));
        Arc::new(Self {
            db,
            router,
            mesh_peer_store,
            service_manager,
            metrics,
            settings_cipher,
            cipher,
            quic_mesh: None,
            local_node_id: Arc::from("test-node"),
            mesh_security: None,
            permission_checker: None,
            license: Arc::new(StaticLicenseChecker::free()),
            meeting_manager,
            vnc_tunnels: Arc::new(dashmap::DashMap::new()),
            mesh_relay_health: None,
            port_allocator: None,
            mesh_services_registry: Arc::new(MeshServicesRegistry::new()),
            live_handles,
        })
    }
}
