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
use crate::license::StaticLicenseChecker;
use crate::db::DbPool;
use crate::license::LicenseChecker;
use crate::mesh::peer_store::MeshPeerStore;
use crate::metrics::RouterMetrics;
use crate::routing::router::Router;
use crate::routing::service_manager::ServiceManager;

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
        let mesh_peer_store = MeshPeerStore::new();

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
        })
    }
}
