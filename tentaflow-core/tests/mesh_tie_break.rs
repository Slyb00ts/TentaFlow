// =============================================================================
// Plik: tests/mesh_tie_break.rs
// Opis: Testy integracyjne tie-break mesh iroh. Weryfikuja, ze:
//       (1) simultaneous dial daje JEDNO stabilne polaczenie po obu stronach,
//       (2) powtorzone cykle dial nie generuja "superseded" — przegrane
//           connections sa zamykane przez register_connection, a otwarty
//           zwyciezca obsluguje uni stream,
//       (3) `IrohMeshConfig { relay_url: None }` bind'uje sie bez internetu.
//
// Uruchomienie:
//   cargo test --test mesh_tie_break --features dashboard-api \
//     -- --nocapture --test-threads=1
// =============================================================================

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tentaflow_core::crypto::SettingsCipher;
use tentaflow_core::db::DbPool;
use tentaflow_core::mesh::iroh_manager::{IrohMeshConfig, IrohMeshManager};
use tentaflow_core::mesh::security::MeshSecurity;

/// In-memory DbPool z minimalnym zestawem tabel wymaganym przez `MeshSecurity::new`.
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
        );",
    )
    .expect("create tables");
    Arc::new(Mutex::new(conn))
}

fn test_cipher() -> Arc<SettingsCipher> {
    Arc::new(SettingsCipher::new(&[0u8; 32]))
}

/// Buduje w pelni operacyjnego mesh managera na loopback.
/// LAN mDNS + DHT wylaczone — testy nie moga zalezec od srodowiska.
async fn make_manager() -> Arc<IrohMeshManager> {
    let db = setup_test_db();
    let security = Arc::new(MeshSecurity::new(db, test_cipher()).expect("security new"));
    let cfg = IrohMeshConfig {
        node_id: String::new(),
        bind_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        relay_url: None,
        enable_lan_discovery: false,
        enable_dht_discovery: false,
    };
    IrohMeshManager::new(cfg, security)
        .await
        .expect("manager new")
}

/// Pobiera loopback socket addr (IPv4) na ktorym bindowal manager.
fn loopback_addr_of(mgr: &IrohMeshManager) -> std::net::SocketAddr {
    mgr.endpoint()
        .bound_sockets()
        .into_iter()
        .find(|a| a.is_ipv4())
        .expect("bound v4 socket")
}

/// `IrohMeshConfig { relay_url: None }` musi bind'owac sie bez dostepu do
/// internetu. Preset N0 wola relay w tle, ale sam bind (UDP socket +
/// setup pkarr publisher) nie moze czekac na DNS resolve.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn starts_without_internet_with_relay_none() {
    let result = tokio::time::timeout(Duration::from_secs(15), async { make_manager().await })
        .await
        .expect("bind timeout — relay_url=None nie powinno blokowac na DNS");

    let node_id = result.node_id();
    assert_eq!(
        node_id.len(),
        64,
        "node_id powinien byc 32B = 64 hex znaki"
    );
    assert!(
        !result.endpoint().bound_sockets().is_empty(),
        "endpoint powinien miec co najmniej jeden bind socket"
    );
}

/// Simultaneous dial: obie strony robia `connect_to_peer_direct` do siebie
/// w tym samym `tokio::join!`. Tie-break musi zbiec oba nody na JEDNO
/// fizyczne polaczenie (po jednej stronie w mapie; obie strony widza 1).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn simultaneous_dial_converges_to_single_connection() {
    let a = make_manager().await;
    let b = make_manager().await;
    let _handles_a = a.start();
    let _handles_b = b.start();

    let a_hex = a.node_id();
    let b_hex = b.node_id();
    let a_addr = loopback_addr_of(&a);
    let b_addr = loopback_addr_of(&b);

    // Obie strony dialuja jednoczesnie.
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
    .expect("simultaneous dial timeout");
    r1.expect("A→B dial result");
    r2.expect("B→A dial result");

    // Dajemy tie-break czas na propagacje close() do przegranego connection
    // i accept loopom na rejestracje incoming.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let peers_a = a.connected_peers().await;
    let peers_b = b.connected_peers().await;
    assert_eq!(
        peers_a,
        vec![b_hex.clone()],
        "A widzi dokladnie jednego peera (B)"
    );
    assert_eq!(
        peers_b,
        vec![a_hex.clone()],
        "B widzi dokladnie jednego peera (A)"
    );

    // Test ze connection zostalo "zywe": kazda strona moze otworzyc uni stream
    // i wyslac heartbeat. Przed tie-break'iem jedna ze stron dostawala
    // "superseded (code 0)" przy open_uni.
    a.send_to_peer(&b_hex, tentaflow_protocol::mesh::MESH_MSG_HEARTBEAT, b"ping-a")
        .await
        .expect("A → B open_uni musi sie udac po tie-break");
    b.send_to_peer(&a_hex, tentaflow_protocol::mesh::MESH_MSG_HEARTBEAT, b"ping-b")
        .await
        .expect("B → A open_uni musi sie udac po tie-break");

    a.shutdown().await;
    b.shutdown().await;
}

/// Wielokrotne cykle simultaneous dial — po kazdym musi zostac dokladnie
/// jedno stabilne polaczenie. Test weryfikuje ze nie akumuluja sie
/// "superseded" connections w kolejnych rundach.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_simultaneous_dials_stay_stable() {
    let a = make_manager().await;
    let b = make_manager().await;
    let _h_a = a.start();
    let _h_b = b.start();

    let a_hex = a.node_id();
    let b_hex = b.node_id();
    let a_addr = loopback_addr_of(&a);
    let b_addr = loopback_addr_of(&b);

    for round in 0..5u32 {
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
        let (r1, r2) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(dial_ab, dial_ba)
        })
        .await
        .unwrap_or_else(|_| panic!("runda {round} timeout"));
        r1.unwrap_or_else(|e| panic!("runda {round} A→B: {e}"));
        r2.unwrap_or_else(|e| panic!("runda {round} B→A: {e}"));

        // Stabilizacja tie-break'a.
        tokio::time::sleep(Duration::from_millis(300)).await;

        assert_eq!(
            a.connected_peers().await.len(),
            1,
            "runda {round}: A powinno miec 1 peera"
        );
        assert_eq!(
            b.connected_peers().await.len(),
            1,
            "runda {round}: B powinno miec 1 peera"
        );

        // Heartbeat-like sanity check — potwierdzenie ze connection zyje.
        a.send_to_peer(&b_hex, tentaflow_protocol::mesh::MESH_MSG_HEARTBEAT, &[])
            .await
            .unwrap_or_else(|e| panic!("runda {round} A→B open_uni: {e}"));
    }

    a.shutdown().await;
    b.shutdown().await;
}

/// Po stabilnym tie-break obie strony musza moc wymieniac ramki przez
/// uni stream. Bez tie-break jedna ze stron zgubila sie na connection
/// ktore druga strona juz zamknela.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn heartbeats_flow_both_directions_after_simultaneous_dial() {
    let a = make_manager().await;
    let b = make_manager().await;
    let _h_a = a.start();
    let _h_b = b.start();

    let b_hex = b.node_id();
    let a_hex = a.node_id();
    let a_addr = loopback_addr_of(&a);
    let b_addr = loopback_addr_of(&b);

    // Subskrybuj zdarzenia PRZED dialem, inaczej mozemy stracic HeartbeatReceived.
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

    // A wysyla heartbeat, B powinno dostac event HeartbeatReceived.
    a.send_to_peer(&b_hex, tentaflow_protocol::mesh::MESH_MSG_HEARTBEAT, b"hb-from-a")
        .await
        .expect("A heartbeat send");

    let received = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match events_b.recv().await {
                Ok(tentaflow_core::mesh::iroh_manager::IrohMeshEvent::HeartbeatReceived {
                    node_id,
                    heartbeat,
                }) if node_id == a_hex => return heartbeat,
                Ok(_) => continue,
                Err(_) => continue,
            }
        }
    })
    .await
    .expect("B powinno dostac HeartbeatReceived od A przed timeout'em");
    assert_eq!(received, b"hb-from-a");

    a.shutdown().await;
    b.shutdown().await;
}
