// =============================================================================
// File: tests/mesh_frame_proxy_dispatch.rs
// Purpose: F1b P3.C-1 — wire-level dispatch tests for frame proxy mesh
//          frames. Two real IrohMeshManager instances exchange rkyv-encoded
//          FrameProxyRequest / FrameProxyResponse payloads over a uni
//          stream and we assert the receiving side emits the matching
//          IrohMeshEvent variant with all payload fields preserved.
//
// Run:
//   cargo test --test mesh_frame_proxy_dispatch --features dashboard-api \
//     -- --nocapture --test-threads=1
// =============================================================================

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tentaflow_core::crypto::SettingsCipher;
use tentaflow_core::db::DbPool;
use tentaflow_core::mesh::iroh_manager::{IrohMeshConfig, IrohMeshEvent, IrohMeshManager};
use tentaflow_core::mesh::security::MeshSecurity;
use tentaflow_protocol::mesh::{
    FrameMetadataWire, FrameProxyRequestPayload, FrameProxyResponsePayload,
};

fn setup_test_db() -> DbPool {
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
            last_addresses TEXT NOT NULL DEFAULT ''
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
        );
        CREATE TABLE IF NOT EXISTS audit_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            user_id INTEGER,
            tenant_id INTEGER,
            action TEXT NOT NULL,
            resource TEXT,
            details TEXT,
            ip_address TEXT,
            node_id TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )
    .expect("create tables");
    Arc::new(Mutex::new(conn))
}

fn test_cipher() -> Arc<SettingsCipher> {
    Arc::new(SettingsCipher::new(&[0u8; 32]))
}

/// Builds a manager + returns the underlying MeshSecurity so the test can
/// inject cross-node trust before any frame exchange happens. Without the
/// trust seed the pre-trust gate in `handle_mesh_uni` would drop the
/// frame and no IrohMeshEvent would ever be emitted.
async fn make_manager() -> (Arc<IrohMeshManager>, Arc<MeshSecurity>) {
    let db = setup_test_db();
    let security = Arc::new(MeshSecurity::new(db, test_cipher()).expect("security new"));
    let cfg = IrohMeshConfig {
        node_id: String::new(),
        bind_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        relay_url: None,
        enable_lan_discovery: false,
        enable_dht_discovery: false,
    };
    let mgr = IrohMeshManager::new(cfg, security.clone())
        .await
        .expect("manager new");
    (mgr, security)
}

fn loopback_addr_of(mgr: &IrohMeshManager) -> std::net::SocketAddr {
    mgr.endpoint()
        .bound_sockets()
        .into_iter()
        .find(|a| a.is_ipv4())
        .expect("bound v4 socket")
}

/// Mutually trust both managers. `node_id` is the Ed25519 hex (64 chars),
/// but `add_trusted_key` validates a full 128-char Ed25519+X25519 hex
/// blob — so we fetch each peer's combined `public_key_hex` and feed
/// that. Once both sides record the trust, the pre-trust gate in
/// `handle_mesh_uni` will let our 0x45 / 0x46 frames through.
fn trust_each_other(
    sec_a: &MeshSecurity,
    sec_b: &MeshSecurity,
    id_a: &str,
    id_b: &str,
) {
    let pub_a = sec_a.public_key_hex();
    let pub_b = sec_b.public_key_hex();
    sec_a
        .add_trusted_key(id_b, &pub_b, "node-b")
        .expect("A trusts B");
    sec_b
        .add_trusted_key(id_a, &pub_a, "node-a")
        .expect("B trusts A");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_frame_proxy_request_decoded_and_event_emitted() {
    let (a, sec_a) = make_manager().await;
    let (b, sec_b) = make_manager().await;
    let _h_a = a.start();
    let _h_b = b.start();

    let a_hex = a.node_id();
    let b_hex = b.node_id();
    let a_addr = loopback_addr_of(&a);
    let b_addr = loopback_addr_of(&b);

    trust_each_other(&sec_a, &sec_b, &a_hex, &b_hex);

    // Subscribe BEFORE dial so we do not miss the FrameProxyRequestReceived
    // emitted after the wire round-trip.
    let mut events_b = b.subscribe();

    let dial_ab = {
        let a = a.clone();
        let b_hex = b_hex.clone();
        async move { a.connect_to_peer_direct(&b_hex, b_addr).await }
    };
    let dial_ba = {
        let b = b.clone();
        let a_hex = a_hex.clone();
        async move { b.connect_to_peer_direct(&a_hex, a_addr).await }
    };
    let (r1, r2) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(dial_ab, dial_ba)
    })
    .await
    .expect("dial timeout");
    r1.expect("A→B");
    r2.expect("B→A");

    tokio::time::sleep(Duration::from_millis(500)).await;

    let req = FrameProxyRequestPayload {
        raw_ref: "frame-store/cam-1/abc-123".into(),
        request_id: "req-p3c1-1".into(),
    };
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&req).expect("encode request");
    a.send_frame_proxy_request(&b_hex, &bytes)
        .await
        .expect("A→B send frame proxy request");

    let received = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events_b.recv().await {
                Ok(IrohMeshEvent::FrameProxyRequestReceived {
                    from_node_id,
                    payload,
                }) if from_node_id == a_hex => return payload,
                Ok(_) => continue,
                Err(_) => continue,
            }
        }
    })
    .await
    .expect("FrameProxyRequestReceived event timeout");

    assert_eq!(received.raw_ref, "frame-store/cam-1/abc-123");
    assert_eq!(received.request_id, "req-p3c1-1");

    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_frame_proxy_response_decoded_and_event_emitted() {
    let (a, sec_a) = make_manager().await;
    let (b, sec_b) = make_manager().await;
    let _h_a = a.start();
    let _h_b = b.start();

    let a_hex = a.node_id();
    let b_hex = b.node_id();
    let a_addr = loopback_addr_of(&a);
    let b_addr = loopback_addr_of(&b);

    trust_each_other(&sec_a, &sec_b, &a_hex, &b_hex);

    let mut events_a = a.subscribe();

    let dial_ab = {
        let a = a.clone();
        let b_hex = b_hex.clone();
        async move { a.connect_to_peer_direct(&b_hex, b_addr).await }
    };
    let dial_ba = {
        let b = b.clone();
        let a_hex = a_hex.clone();
        async move { b.connect_to_peer_direct(&a_hex, a_addr).await }
    };
    let (r1, r2) = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(dial_ab, dial_ba)
    })
    .await
    .expect("dial timeout");
    r1.expect("A→B");
    r2.expect("B→A");

    tokio::time::sleep(Duration::from_millis(500)).await;

    let resp = FrameProxyResponsePayload::Found {
        raw_ref: "frame-store/cam-1/abc-123".into(),
        request_id: "req-p3c1-2".into(),
        bytes: vec![0xAA, 0xBB, 0xCC, 0xDD],
        metadata: FrameMetadataWire {
            camera_id: "cam-1".into(),
            width: 640,
            height: 480,
            pixel_format: "rgb24".into(),
            timestamp_unix_ms: 1_715_000_000_999,
        },
    };
    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&resp).expect("encode response");
    b.send_frame_proxy_response(&a_hex, &bytes)
        .await
        .expect("B→A send frame proxy response");

    let received = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events_a.recv().await {
                Ok(IrohMeshEvent::FrameProxyResponseReceived {
                    from_node_id,
                    payload,
                }) if from_node_id == b_hex => return payload,
                Ok(_) => continue,
                Err(_) => continue,
            }
        }
    })
    .await
    .expect("FrameProxyResponseReceived event timeout");

    match received {
        FrameProxyResponsePayload::Found {
            raw_ref,
            request_id,
            bytes,
            metadata,
        } => {
            assert_eq!(raw_ref, "frame-store/cam-1/abc-123");
            assert_eq!(request_id, "req-p3c1-2");
            assert_eq!(bytes, vec![0xAA, 0xBB, 0xCC, 0xDD]);
            assert_eq!(metadata.camera_id, "cam-1");
            assert_eq!(metadata.width, 640);
            assert_eq!(metadata.height, 480);
            assert_eq!(metadata.pixel_format, "rgb24");
            assert_eq!(metadata.timestamp_unix_ms, 1_715_000_000_999);
        }
        other => panic!("expected Found, got {:?}", other),
    }

    a.shutdown().await;
    b.shutdown().await;
}
