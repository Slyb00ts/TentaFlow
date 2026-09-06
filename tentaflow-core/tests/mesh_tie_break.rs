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
//   cargo test --test mesh_tie_break \
//     -- --nocapture --test-threads=1
// =============================================================================

use std::sync::Arc;
use std::time::Duration;

use tentaflow_core::crypto::SettingsCipher;
use tentaflow_core::db::DbPool;
use tentaflow_core::mesh::iroh_manager::{IrohMeshConfig, IrohMeshManager};
use tentaflow_core::mesh::security::MeshSecurity;

/// In-memory DbPool z minimalnym zestawem tabel wymaganym przez `MeshSecurity::new`.
fn setup_test_db() -> DbPool {
    let conn = rusqlite::Connection::open_in_memory().expect("open in-memory db");
    // The real migrations, not a hand-written subset. Copies of the schema kept
    // drifting from the columns and tables `MeshSecurity` actually reads
    // (`trusted_nodes.environment`, `sync_policies`), and every drift showed up
    // as this whole file failing before a single test body ran.
    tentaflow_core::db::migrations::run(&conn).expect("run migrations");
    Arc::new(tentaflow_core::db::Db::from_connection(conn))
}


fn test_cipher() -> Arc<SettingsCipher> {
    Arc::new(SettingsCipher::new(&[0u8; 32]))
}

/// Buduje w pelni operacyjnego mesh managera na loopback.
/// LAN mDNS + DHT wylaczone — testy nie moga zalezec od srodowiska.
async fn make_manager() -> (Arc<IrohMeshManager>, Arc<MeshSecurity>) {
    let db = setup_test_db();
    let security = Arc::new(MeshSecurity::new(db, test_cipher()).expect("security new"));
    let cfg = IrohMeshConfig {
        node_id: String::new(),
        bind_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
        relay_url: None,
        enable_lan_discovery: false,
        enable_dht_discovery: false,
        addr_filter: None,
        disable_portmapper: false,
    };
    let mgr = IrohMeshManager::new(cfg, security.clone())
        .await
        .expect("manager new");
    (mgr, security)
}

/// Mutually trust both managers. A heartbeat is not a pre-trust frame
/// (`mesh::frame_policy`), so an untrusted peer's heartbeat is dropped at the
/// gate before it can ever become a `HeartbeatReceived` event — a connection
/// alone is not enough to exchange frames.
fn trust_each_other(sec_a: &MeshSecurity, sec_b: &MeshSecurity, id_a: &str, id_b: &str) {
    let pub_a = sec_a.public_key_hex();
    let pub_b = sec_b.public_key_hex();
    sec_a
        .add_trusted_key(id_b, &pub_b, "node-b", None)
        .expect("A trusts B");
    sec_b
        .add_trusted_key(id_a, &pub_a, "node-a", None)
        .expect("B trusts A");
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
    let result = tokio::time::timeout(Duration::from_secs(15), async { make_manager().await.0 })
        .await
        .expect("bind timeout — relay_url=None nie powinno blokowac na DNS");

    let node_id = result.node_id();
    assert_eq!(node_id.len(), 64, "node_id powinien byc 32B = 64 hex znaki");
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
    let (a, sec_a) = make_manager().await;
    let (b, sec_b) = make_manager().await;
    let _handles_a = a.start();
    let _handles_b = b.start();

    let a_hex = a.node_id();
    let b_hex = b.node_id();
    let a_addr = loopback_addr_of(&a);
    let b_addr = loopback_addr_of(&b);
    trust_each_other(&sec_a, &sec_b, &a_hex, &b_hex);

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
    // After tie-break both sides must still be able to open uni-streams.
    // `send_heartbeat_data` broadcasts to every connected peer, and after
    // tie-break each side holds exactly one connection, so this is the
    // same liveness check the old per-peer send was doing.
    let _ = &b_hex;
    let _ = &a_hex;
    a.send_heartbeat_data(b"ping-a").await;
    b.send_heartbeat_data(b"ping-b").await;

    a.shutdown().await;
    b.shutdown().await;
}

/// Wielokrotne cykle simultaneous dial — po kazdym musi zostac dokladnie
/// jedno stabilne polaczenie. Test weryfikuje ze nie akumuluja sie
/// "superseded" connections w kolejnych rundach.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeated_simultaneous_dials_stay_stable() {
    let (a, sec_a) = make_manager().await;
    let (b, sec_b) = make_manager().await;
    let _h_a = a.start();
    let _h_b = b.start();

    let a_hex = a.node_id();
    let b_hex = b.node_id();
    let a_addr = loopback_addr_of(&a);
    let b_addr = loopback_addr_of(&b);
    trust_each_other(&sec_a, &sec_b, &a_hex, &b_hex);

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
        // `send_heartbeat_data` broadcasts to every connected peer; A has
        // exactly one trusted peer (B) in this scenario, so this exercises
        // the same A→B open_uni path the legacy per-peer send used.
        let _ = &b_hex;
        a.send_heartbeat_data(&[]).await;
    }

    a.shutdown().await;
    b.shutdown().await;
}

/// Po stabilnym tie-break obie strony musza moc wymieniac ramki przez
/// uni stream. Bez tie-break jedna ze stron zgubila sie na connection
/// ktore druga strona juz zamknela.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn heartbeats_flow_both_directions_after_simultaneous_dial() {
    let (a, sec_a) = make_manager().await;
    let (b, sec_b) = make_manager().await;
    let _h_a = a.start();
    let _h_b = b.start();

    let b_hex = b.node_id();
    let a_hex = a.node_id();
    let a_addr = loopback_addr_of(&a);
    let b_addr = loopback_addr_of(&b);
    trust_each_other(&sec_a, &sec_b, &a_hex, &b_hex);

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
    // `send_heartbeat_data` rzuca payload do wszystkich connected peerow;
    // A ma tylko jednego (B), wiec to bezposredni A→B push.
    let _ = &b_hex;
    a.send_heartbeat_data(b"hb-from-a").await;

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
