// =============================================================================
// File: bus/replication/test_support.rs — shared `#[cfg(test)]` mesh fixture
// =============================================================================
//
// `make_test_mesh_manager` used to be copy-pasted three times (this
// module's own tests, `init.rs`'s tests, and `bus::native`'s tests) — all
// three needed the exact same loopback, discovery-disabled
// `IrohMeshManager` and had drifted into small path-qualification/literal
// differences that carried no behavioral meaning. Consolidated here so all
// three test modules share one definition instead of one each.

use std::net::SocketAddr;
use std::sync::Arc;

use crate::crypto::SettingsCipher;
use crate::db::{Db, DbPool};
use crate::mesh::iroh_manager::{IrohMeshConfig, IrohMeshManager};
use crate::mesh::security::MeshSecurity;

/// Loopback, discovery-disabled `IrohMeshManager` for replication/native-app
/// tests that only need a live mesh HANDLE (`router::set_mesh_manager`,
/// `replication::init`, `cfg.mesh`) and never actually dial or accept on
/// it. Cheap: binds one ephemeral UDP socket, starts no accept loop, so
/// nothing but the returned `Arc` holds a strong reference to it.
pub(crate) async fn make_test_mesh_manager() -> Arc<IrohMeshManager> {
    let conn = rusqlite::Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE TABLE IF NOT EXISTS trusted_nodes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            node_id TEXT NOT NULL UNIQUE,
            public_key TEXT NOT NULL,
            hostname TEXT DEFAULT '',
            approved_by TEXT DEFAULT '',
            approved_at TEXT NOT NULL DEFAULT (datetime('now')),
            is_active INTEGER NOT NULL DEFAULT 1,
            last_addresses TEXT NOT NULL DEFAULT '',
            environment TEXT
        );
        CREATE TABLE IF NOT EXISTS pending_pairings (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            remote_node_id TEXT NOT NULL,
            pin_code TEXT NOT NULL,
            direction TEXT NOT NULL CHECK(direction IN ('outgoing','incoming')),
            expires_at TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE TABLE IF NOT EXISTS revoked_nodes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            node_id TEXT NOT NULL UNIQUE,
            revoked_by TEXT,
            revoked_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )
    .expect("create tables");
    let db: DbPool = Arc::new(Db::from_connection(conn));
    let cipher = Arc::new(SettingsCipher::new(&[0u8; 32]));
    let security = Arc::new(MeshSecurity::new(db, cipher).expect("security new"));
    let cfg = IrohMeshConfig {
        node_id: String::new(),
        bind_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
        relay_url: None,
        enable_lan_discovery: false,
        enable_dht_discovery: false,
        ..Default::default()
    };
    IrohMeshManager::new(cfg, security)
        .await
        .expect("manager new")
}
