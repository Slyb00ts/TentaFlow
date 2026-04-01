// =============================================================================
// Plik: mesh/pipeline.rs
// Opis: Reużywalny pipeline mesh networking — mDNS discovery, QUIC mesh,
//       heartbeat sender, Docker container cache, NodeInfo exchange.
//       Uzywany przez Router.New, Desktop i Mobile (ta sama logika).
// =============================================================================

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tracing::{debug, error, info, warn};

use crate::config::MeshConfig;
use crate::mesh::discovery::{MdnsDiscovery, PeerEvent};
use crate::mesh::node_info_collector;
use crate::mesh::peer_store::{HeartbeatMetrics, MeshPeerInfo, MeshPeerStore, NodeInfo};
use crate::mesh::quic_mesh::{QuicMeshConfig, QuicMeshEvent, QuicMeshManager};
use crate::mesh::security::MeshSecurity;

/// Konfiguracja mesh pipeline
pub struct MeshPipelineConfig {
    /// Identyfikator tego noda
    pub node_id: String,
    /// Rola noda (np. "router", "desktop", "mobile")
    pub role: String,
    /// Konfiguracja mesh z pliku config
    pub mesh_config: MeshConfig,
}

/// Wynik uruchomienia mesh pipeline — trzeba trzymac alive do konca zycia aplikacji
pub struct MeshPipelineHandles {
    /// mDNS discovery — Drop wyrejestruje serwis. MUSI zyc.
    pub mdns: Option<MdnsDiscovery>,
    /// QuicMeshManager — potrzebny do forward handlerów i bezposredniej komunikacji
    pub quic_mesh: Option<Arc<QuicMeshManager>>,
    /// MeshSecurity — klucze, parowanie, szyfrowanie
    pub security: Option<Arc<MeshSecurity>>,
}

impl MeshPipelineHandles {
    /// Graceful shutdown — zamyka QUIC endpoint i wszystkie polaczenia,
    /// potem dropuje mDNS (wyrejestrowanie serwisu).
    /// BEZ tego porty UDP zostaja zajete jako zombie.
    pub async fn shutdown(mut self) {
        if let Some(ref qm) = self.quic_mesh {
            qm.send_node_leaving().await;
            qm.shutdown().await;
        }
        // mDNS dropowany automatycznie — wyrejestruje serwis
        self.mdns.take();
        self.quic_mesh.take();
        self.security.take();
        info!("Mesh pipeline zamkniety");
    }
}

/// Uruchamia caly mesh pipeline: mDNS + QUIC + heartbeat + Docker cache.
///
/// To jest ta sama logika co byla w Router.New i Desktop — teraz jest w Core.
/// Kazda aplikacja (Router, Desktop, Mobile) wywoluje te jedna funkcje.
///
/// Zwraca `MeshPipelineHandles` ktore MUSZA zyc do konca aplikacji.
pub async fn start_mesh_pipeline(
    config: MeshPipelineConfig,
    mesh_peer_store: &MeshPeerStore,
    db_pool: Option<crate::db::DbPool>,
) -> Result<MeshPipelineHandles> {
    let node_id = &config.node_id;
    let mesh_config = &config.mesh_config;
    let mesh_port = mesh_config.port;

    info!(
        "Inicjalizacja mesh networking (port {}, node_id: {})",
        mesh_port,
        &node_id[..8.min(node_id.len())]
    );

    // Inicjalizacja MeshSecurity (jesli dostepna baza danych)
    let mesh_security: Option<Arc<MeshSecurity>> = if let Some(ref pool) = db_pool {
        match MeshSecurity::new(pool.clone()) {
            Ok(sec) => {
                info!("MeshSecurity zainicjalizowany (klucz publiczny: {}...)", &sec.public_key_hex()[..16]);
                Some(Arc::new(sec))
            }
            Err(e) => {
                // VULN-015: Brak MeshSecurity = mesh dziala w trybie zero trust (odrzuca polaczenia)
                error!("Nie udalo sie zainicjalizowac MeshSecurity: {} — mesh bedzie odrzucac polaczenia!", e);
                None
            }
        }
    } else {
        None
    };

    // Zbierz NodeInfo lokalnego noda
    let local_node_info = node_info_collector::collect_node_info(node_id);

    // Dodaj lokalny node do store — widoczny na liscie hostow jako "(local)"
    let local_hostname = if local_node_info.hostname.is_empty() {
        "(local)".to_string()
    } else {
        format!("{} (local)", local_node_info.hostname)
    };
    // Zbierz dane lokalne na starcie — adresy, Docker, OS
    let local_addresses = node_info_collector::collect_local_addresses();
    let local_os_distro = node_info_collector::collect_os_distro();
    let (docker_available, docker_version) = node_info_collector::collect_docker_info();

    mesh_peer_store.add_or_update(MeshPeerInfo {
        node_id: node_id.clone(),
        addresses: local_addresses,
        port: mesh_port,
        role: config.role.clone(),
        status: "connected".to_string(),
        quic_connected: true,
        discovered_at: chrono::Utc::now().to_rfc3339(),
        hostname: local_hostname,
        os_info: if local_os_distro.is_empty() { local_node_info.os_info.clone() } else { local_os_distro },
        cpu_count: local_node_info.cpu_count,
        ram_total_mb: local_node_info.ram_total_mb,
        cpu_usage_percent: 0.0,
        ram_used_mb: 0,
        gpu_info: local_node_info.gpu_info.clone(),
        containers: vec![],
        networks: vec![],
        platform: node_info_collector::detect_platform(),
        cpu_temperature_c: None,
        swap_total_mb: 0,
        swap_used_mb: 0,
        docker_available,
        docker_version,
    });

    // mDNS discovery
    let mdns = match MdnsDiscovery::new(node_id, mesh_port) {
        Ok(mdns) => {
            let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
            if let Err(e) = mdns.start_discovery(tx) {
                warn!("Nie udalo sie uruchomic mDNS browse: {}", e);
            }

            // QuicMeshManager
            let quic_mesh_config = QuicMeshConfig {
                node_id: node_id.clone(),
                listen_port: mesh_port,
                heartbeat_interval: Duration::from_millis(mesh_config.heartbeat_interval_ms),
                reconnect_base: Duration::from_secs(1),
                reconnect_max: Duration::from_secs(30),
            };

            match QuicMeshManager::new(quic_mesh_config, mesh_security.clone()) {
                Ok(quic_mesh) => {
                    let qm = quic_mesh.clone();
                    tokio::spawn(async move {
                        qm.start();
                    });

                    // Reconnect do trusted peerow z zapisanych adresow w DB (przed mDNS)
                    if let Some(ref sec) = mesh_security {
                        if let Ok(trusted) = crate::db::repository::list_trusted_nodes(&sec.db) {
                            for node in &trusted {
                                if node.last_addresses.is_empty() { continue; }
                                let addrs: Vec<std::net::SocketAddr> = node.last_addresses
                                    .split(',')
                                    .filter_map(|s| s.trim().parse().ok())
                                    .collect();
                                if addrs.is_empty() { continue; }
                                let qm = quic_mesh.clone();
                                let nid = node.node_id.clone();
                                tokio::spawn(async move {
                                    for addr in &addrs {
                                        match qm.connect_to_peer(&nid, *addr).await {
                                            Ok(()) => {
                                                info!(peer_id = %nid, addr = %addr, "Reconnect z DB udany");
                                                break;
                                            }
                                            Err(e) => {
                                                debug!(peer_id = %nid, addr = %addr, "Reconnect z DB: {}", e);
                                            }
                                        }
                                    }
                                });
                            }
                        }
                    }

                    // Task 1: mDNS discovery → add to peer store → connect via QUIC
                    spawn_mdns_handler(rx, quic_mesh.clone(), mesh_peer_store.clone(), node_id.clone());

                    // Task 2: QUIC events → update peer store
                    spawn_quic_event_handler(
                        quic_mesh.clone(),
                        mesh_peer_store.clone(),
                        local_node_info.clone(),
                        mesh_security.clone(),
                        node_id.clone(),
                    );

                    // Docker container cache — co 5s
                    let docker_cache = spawn_docker_cache();

                    // Task 3: Heartbeat sender — co 500ms
                    spawn_heartbeat_sender(
                        quic_mesh.clone(),
                        mesh_peer_store.clone(),
                        node_id.clone(),
                        docker_cache,
                    );

                    // Task 4: Slow refresh — co 60s odswiezaj adresy IP, Docker, OS info
                    spawn_slow_refresh(
                        mesh_peer_store.clone(),
                        node_id.clone(),
                    );

                    // [CR-011] Task 5: Czyszczenie wygaslych parowan — co 30s
                    if let Some(ref sec) = mesh_security {
                        spawn_pairing_cleanup(sec.clone());
                    }

                    // Task 6: Rotacja kluczy szyfrowania — co 24h
                    if let Some(ref sec) = mesh_security {
                        spawn_key_rotation_task(quic_mesh.clone(), sec.clone());
                    }

                    // Task 7: Okresowe proby bezposredniego polaczenia z relay-only peerami
                    {
                        let qm = quic_mesh.clone();
                        let ps = mesh_peer_store.clone();
                        let sec = mesh_security.clone();
                        tokio::spawn(async move {
                            let mut interval = tokio::time::interval(Duration::from_secs(300));
                            interval.tick().await;
                            loop {
                                interval.tick().await;

                                let routing_table = ps.get_routing_table();
                                let sec_ref = match &sec {
                                    Some(s) => s,
                                    None => continue,
                                };

                                for (peer_id, entry) in &routing_table {
                                    // Pomijaj bezposrednio polaczone
                                    if entry.direct { continue; }

                                    // Tylko trusted peery
                                    if !sec_ref.is_trusted(peer_id) { continue; }

                                    // Moze juz nawiazano polaczenie od ostatniego recalc
                                    if qm.is_connected(peer_id).await { continue; }

                                    // Pobierz adresy z peer_store
                                    let mut addrs: Vec<std::net::SocketAddr> = Vec::new();
                                    if let Some(peer_info) = ps.get(peer_id) {
                                        for ip in &peer_info.addresses {
                                            if peer_info.port > 0 {
                                                addrs.push(std::net::SocketAddr::new(*ip, peer_info.port));
                                            }
                                        }
                                    }
                                    // Fallback: adresy z bazy danych
                                    if addrs.is_empty() {
                                        if let Ok(trusted) = crate::db::repository::list_trusted_nodes(&sec_ref.db) {
                                            if let Some(tn) = trusted.iter().find(|t| t.node_id == *peer_id) {
                                                for part in tn.last_addresses.split(',') {
                                                    if let Ok(addr) = part.trim().parse::<std::net::SocketAddr>() {
                                                        addrs.push(addr);
                                                    }
                                                }
                                            }
                                        }
                                    }

                                    if addrs.is_empty() { continue; }

                                    // Probuj kazdy adres po kolei
                                    for addr in &addrs {
                                        match qm.connect_to_peer(peer_id, *addr).await {
                                            Ok(()) => {
                                                info!(peer_id = %peer_id, addr = %addr, "Bezposrednie polaczenie nawiazane (bylo relay)");
                                                break;
                                            }
                                            Err(_) => continue,
                                        }
                                    }
                                }
                            }
                        });
                    }

                    info!("Mesh networking uruchomiony (QUIC mesh + mDNS)");

                    return Ok(MeshPipelineHandles {
                        mdns: Some(mdns),
                        quic_mesh: Some(quic_mesh),
                        security: mesh_security,
                    });
                }
                Err(e) => {
                    error!("Nie udalo sie utworzyc QuicMeshManager: {}", e);
                }
            }

            Some(mdns)
        }
        Err(e) => {
            warn!("Nie udalo sie uruchomic mDNS: {}", e);
            None
        }
    };

    Ok(MeshPipelineHandles {
        mdns,
        quic_mesh: None,
        security: mesh_security,
    })
}

// =============================================================================
// Wewnetrzne taski mesh pipeline
// =============================================================================

fn spawn_mdns_handler(
    mut rx: tokio::sync::mpsc::UnboundedReceiver<PeerEvent>,
    quic_mesh: Arc<QuicMeshManager>,
    peer_store: MeshPeerStore,
    local_node_id: String,
) {
    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            match event {
                PeerEvent::Discovered(peer) => {
                    // Pomijaj peery bez node_id
                    if peer.node_id == "unknown" || peer.node_id.is_empty() {
                        continue;
                    }

                    // Pomijaj wlasny node — nie chcemy sie sami ze soba laczyc
                    if peer.node_id == local_node_id {
                        continue;
                    }

                    // Jesli peer juz jest w store — zaktualizuj hostname jesli pusty, pomijaj reszte
                    if let Some(existing) = peer_store.get(&peer.node_id) {
                        if existing.quic_connected {
                            // Zaktualizuj hostname jesli brakowal przy pierwszym discovery
                            let new_hostname = peer.properties.get("hostname").cloned().unwrap_or_default();
                            if !new_hostname.is_empty() && existing.hostname.is_empty() {
                                peer_store.update_hostname(&peer.node_id, &new_hostname);
                            }
                            continue;
                        }
                    }

                    // Filtruj adresy: IPv4, nie-loopback, nie-Docker-bridge, nie-link-local
                    let mut addrs: Vec<IpAddr> = peer.addresses.iter()
                        .filter(|a| {
                            if let IpAddr::V4(v4) = a {
                                !v4.is_loopback()
                                    && !(v4.octets()[0] == 172 && v4.octets()[1] >= 16 && v4.octets()[1] <= 31)
                                    && !v4.is_link_local()
                            } else {
                                false
                            }
                        })
                        .copied()
                        .collect();
                    if addrs.is_empty() {
                        addrs = peer.addresses.iter().filter(|a| a.is_ipv4()).copied().collect();
                    }

                    // Pomijaj peery bez adresow — czekaj na re-announce z adresami
                    if addrs.is_empty() {
                        debug!(node_id = %peer.node_id, "mDNS peer bez adresow — czekam na re-announce");
                        continue;
                    }

                    debug!(
                        node_id = %peer.node_id,
                        port = peer.port,
                        "Odkryto nowego peera przez mDNS"
                    );

                    peer_store.add_or_update(MeshPeerInfo {
                        node_id: peer.node_id.clone(),
                        addresses: addrs.clone(),
                        port: peer.port,
                        role: peer
                            .properties
                            .get("role")
                            .cloned()
                            .unwrap_or_else(|| "unknown".to_string()),
                        status: "discovered".to_string(),
                        quic_connected: false,
                        discovered_at: chrono::Utc::now().to_rfc3339(),
                        hostname: peer.properties.get("hostname").cloned().unwrap_or_default(),
                        os_info: String::new(),
                        cpu_count: 0,
                        ram_total_mb: 0,
                        cpu_usage_percent: 0.0,
                        ram_used_mb: 0,
                        gpu_info: vec![],
                        containers: vec![],
                        networks: vec![],
                        platform: String::new(),
                        cpu_temperature_c: None,
                        swap_total_mb: 0,
                        swap_used_mb: 0,
                        docker_available: false,
                        docker_version: String::new(),
                    });

                    // Probuj kazdy adres az sie polacz
                    let mut connected = false;
                    for ip in &addrs {
                        let sock_addr = SocketAddr::new(*ip, peer.port);
                        match quic_mesh.connect_to_peer(&peer.node_id, sock_addr).await {
                            Ok(_) => {
                                connected = true;
                                break;
                            }
                            Err(e) => {
                                debug!(
                                    peer_id = %peer.node_id,
                                    addr = %sock_addr,
                                    error = %e,
                                    "connect_to_peer nieudany — probuje nastepny adres"
                                );
                            }
                        }
                    }
                    if !connected && !addrs.is_empty() {
                        peer_store.set_status(&peer.node_id, "connecting");
                    }
                }
                PeerEvent::Removed { fullname } => {
                    debug!(fullname = %fullname, "Peer usuniety z mDNS");
                }
            }
        }
    });
}

fn spawn_quic_event_handler(
    quic_mesh: Arc<QuicMeshManager>,
    peer_store: MeshPeerStore,
    local_node_info: NodeInfo,
    mesh_security: Option<Arc<MeshSecurity>>,
    local_node_id: String,
) {
    let qm_events = quic_mesh.clone();
    let mut event_rx = quic_mesh.subscribe();

    tokio::spawn(async move {
        let mut last_sync_sent: std::collections::HashMap<String, std::time::Instant> = std::collections::HashMap::new();
        const SYNC_COOLDOWN_SECS: u64 = 30;

        loop {
            match event_rx.recv().await {
                Ok(QuicMeshEvent::NodeInfoReceived { node_id, data }) => {
                    // Safety net — przetwarzaj NodeInfo TYLKO od trusted peerow
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&node_id),
                        None => false, // Zero trust — bez MeshSecurity nie przetwarzaj danych
                    };
                    if !is_trusted {
                        debug!(peer_id = %node_id, "Pomijam NodeInfo od niezaufanego peera (safety net)");
                        continue;
                    }
                    match rkyv::from_bytes::<NodeInfo, rkyv::rancor::Error>(&data) {
                        Ok(info) => {
                            info!(
                                peer_id = %node_id,
                                hostname = %info.hostname,
                                os = %info.os_info,
                                cpus = info.cpu_count,
                                ram_mb = info.ram_total_mb,
                                gpus = info.gpu_info.len(),
                                "Otrzymano NodeInfo od peera"
                            );
                            peer_store.update_node_info(&node_id, &info);
                        }
                        Err(e) => {
                            warn!(peer_id = %node_id, "Blad deserializacji NodeInfo: {}", e);
                        }
                    }
                }
                Ok(QuicMeshEvent::PeerConnected { node_id }) => {
                    info!(peer_id = %node_id, "QUIC peer polaczony");
                    peer_store.set_quic_connected(&node_id, true);
                    peer_store.set_status(&node_id, "connected");
                    // Wyslij swoje NodeInfo do nowego peera — TYLKO jesli zaufany
                    let should_send = match &mesh_security {
                        Some(sec) => sec.is_trusted(&node_id),
                        None => false, // Zero trust — bez MeshSecurity nie wysylaj danych
                    };
                    if should_send {
                        if let Ok(info_bytes) = rkyv::to_bytes::<rkyv::rancor::Error>(&local_node_info)
                        {
                            if let Err(e) = qm_events.send_node_info(&node_id, &info_bytes).await {
                                warn!("Blad wysylania NodeInfo do {}: {}", node_id, e);
                            }
                        }

                        // Synchronizacja zaufanych kluczy przy reconnect (z cooldownem)
                        if let Some(ref sec) = mesh_security {
                            let should_sync = last_sync_sent.get(&node_id)
                                .map_or(true, |t| t.elapsed() >= std::time::Duration::from_secs(SYNC_COOLDOWN_SECS));

                            if should_sync {
                                let all_keys = sec.get_all_trusted_keys();
                                if !all_keys.is_empty() {
                                    let entries: Vec<tentaflow_protocol::mesh::TrustedKeyEntry> = all_keys
                                        .iter()
                                        .map(|(nid, pk)| tentaflow_protocol::mesh::TrustedKeyEntry {
                                            node_id: nid.clone(),
                                            public_key_hex: pk.clone(),
                                        })
                                        .collect();
                                    let payload = tentaflow_protocol::mesh::TrustedKeysSyncPayload { keys: entries };
                                    if let Ok(sync_data) = rkyv::to_bytes::<rkyv::rancor::Error>(&payload).map(|v| v.to_vec()) {
                                        if let Err(e) = qm_events.send_trusted_keys_sync(&node_id, &sync_data).await {
                                            warn!("Blad wysylania TrustedKeysSync do {}: {}", node_id, e);
                                        }
                                    }
                                }

                                // Wyslij revokowane nody — peer moze nie wiedziec o revoke jesli byl offline
                                let revoked = sec.get_revoked_node_ids();
                                for revoked_id in &revoked {
                                    let payload = tentaflow_protocol::mesh::TrustRevokedPayload {
                                        revoked_node_id: revoked_id.clone(),
                                        from_node_id: local_node_id.clone(),
                                    };
                                    if let Ok(data) = rkyv::to_bytes::<rkyv::rancor::Error>(&payload).map(|v| v.to_vec()) {
                                        let _ = qm_events.send_to_peer(&node_id, tentaflow_protocol::mesh::MESH_MSG_TRUST_REVOKED, &data).await;
                                    }
                                }

                                last_sync_sent.insert(node_id.clone(), std::time::Instant::now());
                            }
                        }
                    } else {
                        info!(peer_id = %node_id, "Peer niezaufany — pomijam wysylanie NodeInfo");
                    }

                    // Persist adresy trusted peera do DB (do reconnectu po restarcie)
                    if let Some(ref sec) = mesh_security {
                        if sec.is_trusted(&node_id) {
                            if let Some(peer_info) = peer_store.get(&node_id) {
                                if !peer_info.addresses.is_empty() && peer_info.port > 0 {
                                    let addr_str = peer_info.addresses.iter()
                                        .map(|ip| format!("{}:{}", ip, peer_info.port))
                                        .collect::<Vec<_>>()
                                        .join(",");
                                    let _ = crate::db::repository::update_trusted_node_addresses(&sec.db, &node_id, &addr_str);
                                }
                            }
                        }
                    }

                    // Przelicz routing po polaczeniu nowego peera
                    peer_store.recalculate_routes(&local_node_id);
                }
                Ok(QuicMeshEvent::PeerDisconnected { node_id }) => {
                    info!(peer_id = %node_id, "QUIC peer rozlaczony");
                    peer_store.set_quic_connected(&node_id, false);
                    peer_store.set_status(&node_id, "disconnected");

                    // Przelicz routing po rozlaczeniu peera
                    peer_store.recalculate_routes(&local_node_id);

                    // Auto-reconnect dla trusted peerow
                    let should_reconnect = match &mesh_security {
                        Some(sec) => sec.is_trusted(&node_id),
                        None => false,
                    };
                    if should_reconnect {
                        let mut addrs: Vec<std::net::SocketAddr> = Vec::new();
                        // Adresy z peer_store
                        if let Some(peer_info) = peer_store.get(&node_id) {
                            for ip in &peer_info.addresses {
                                addrs.push(std::net::SocketAddr::new(*ip, peer_info.port));
                            }
                        }
                        // Fallback: adresy z DB
                        if addrs.is_empty() {
                            if let Some(ref sec) = mesh_security {
                                if let Ok(trusted) = crate::db::repository::list_trusted_nodes(&sec.db) {
                                    if let Some(tn) = trusted.iter().find(|t| t.node_id == node_id) {
                                        for part in tn.last_addresses.split(',') {
                                            if let Ok(addr) = part.trim().parse::<std::net::SocketAddr>() {
                                                addrs.push(addr);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        if !addrs.is_empty() {
                            qm_events.spawn_reconnect_loop(node_id.clone(), addrs);
                        }
                    }
                }
                Ok(QuicMeshEvent::HeartbeatReceived { node_id, heartbeat }) => {
                    // Safety net — przetwarzaj heartbeat TYLKO od trusted peerow
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&node_id),
                        None => false, // Zero trust — bez MeshSecurity nie przetwarzaj danych
                    };
                    if !is_trusted {
                        debug!(peer_id = %node_id, "Pomijam heartbeat od niezaufanego peera (safety net)");
                        continue;
                    }
                    if let Ok(metrics) =
                        rkyv::from_bytes::<HeartbeatMetrics, rkyv::rancor::Error>(&heartbeat)
                    {
                        peer_store.update_metrics(
                            &node_id,
                            metrics.cpu_usage_percent,
                            metrics.ram_used_mb,
                            metrics.gpus,
                            metrics.containers,
                            metrics.networks,
                            metrics.platform,
                            metrics.cpu_temperature_c,
                            metrics.swap_total_mb,
                            metrics.swap_used_mb,
                        );

                        // Aktualizuj topologie peera na podstawie jego connected_peers
                        peer_store.update_topology(&node_id, metrics.connected_peers);
                    }
                }
                Ok(QuicMeshEvent::PairingRequestReceived { peer_id, data }) => {
                    info!(peer_id = %peer_id, data_len = data.len(), "Odebrano PairingRequest przez QUIC");
                    if let Some(ref sec) = mesh_security {
                        match serde_json::from_slice::<serde_json::Value>(&data) {
                            Ok(val) => {
                                let from_node_id = val["from_node_id"].as_str().unwrap_or(&peer_id);
                                info!(
                                    from_node_id = %from_node_id,
                                    peer_id = %peer_id,
                                    has_pin = !val["pin"].as_str().unwrap_or("").is_empty(),
                                    has_pubkey = !val["public_key"].as_str().unwrap_or("").is_empty(),
                                    "PairingRequest szczegoly"
                                );
                                if from_node_id == local_node_id {
                                    warn!("Odrzucono PairingRequest od samego siebie (from_node_id == local_node_id)");
                                    continue;
                                }
                                let pin = val["pin"].as_str().unwrap_or("");
                                let public_key = val["public_key"].as_str().unwrap_or("");
                                if let Err(e) = sec.receive_pairing_request(from_node_id, pin, public_key) {
                                    warn!("Blad zapisu PairingRequest od {}: {}", peer_id, e);
                                } else {
                                    info!("PairingRequest od {} zapisany — oczekuje na potwierdzenie PIN", from_node_id);
                                }
                            }
                            Err(e) => {
                                warn!(peer_id = %peer_id, "Blad parsowania PairingRequest JSON: {}", e);
                            }
                        }
                    }
                }
                Ok(QuicMeshEvent::PairingConfirmReceived { peer_id, data }) => {
                    // Parsuj JSON i zatwierdz parowanie — dodaj do zaufanych
                    if let Some(ref sec) = mesh_security {
                        match serde_json::from_slice::<serde_json::Value>(&data) {
                            Ok(val) => {
                                let from_node_id = val["from_node_id"].as_str().unwrap_or(&peer_id);
                                let public_key = val["public_key"].as_str().unwrap_or("");
                                let hostname = val["hostname"].as_str().unwrap_or("");
                                let received_pin = val["pin"].as_str().unwrap_or("");

                                // Weryfikuj PIN — inicjator sprawdza czy receiver podal poprawny PIN
                                if let Ok(Some(expected_pin)) = sec.get_pending_pin(from_node_id) {
                                    if !received_pin.is_empty() && received_pin != expected_pin {
                                        warn!("PairingConfirm od {} — nieprawidlowy PIN", from_node_id);
                                        continue;
                                    }
                                }

                                if let Err(e) = sec.confirm_pairing(from_node_id, public_key, hostname, "mesh-quic") {
                                    warn!("Blad potwierdzenia parowania od {}: {}", peer_id, e);
                                } else {
                                    info!("Otrzymano PairingConfirm od {} — node zaufany", peer_id);

                                    // Po sparowaniu — wyslij NodeInfo do nowo zaufanego peera
                                    let target_node_id = from_node_id.to_string();
                                    if let Ok(info_bytes) = rkyv::to_bytes::<rkyv::rancor::Error>(&local_node_info) {
                                        if let Err(e) = qm_events.send_node_info(&target_node_id, &info_bytes).await {
                                            warn!("Blad wysylania NodeInfo po sparowaniu do {}: {}", target_node_id, e);
                                        } else {
                                            info!(peer_id = %target_node_id, "Wyslano NodeInfo do nowo zaufanego peera");
                                        }
                                    }

                                    // Wyslij TrustedKeysSync z naszymi zaufanymi kluczami
                                    let all_keys = sec.get_all_trusted_keys();
                                    if !all_keys.is_empty() {
                                        let entries: Vec<tentaflow_protocol::mesh::TrustedKeyEntry> = all_keys
                                            .iter()
                                            .map(|(nid, pk)| tentaflow_protocol::mesh::TrustedKeyEntry {
                                                node_id: nid.clone(),
                                                public_key_hex: pk.clone(),
                                            })
                                            .collect();
                                        let payload = tentaflow_protocol::mesh::TrustedKeysSyncPayload { keys: entries };
                                        let sync_data = rkyv::to_bytes::<rkyv::rancor::Error>(&payload)
                                            .map(|v| v.to_vec())
                                            .unwrap_or_default();
                                        if let Err(e) = qm_events.send_trusted_keys_sync(&target_node_id, &sync_data).await {
                                            warn!("Blad wysylania TrustedKeysSync do {}: {}", target_node_id, e);
                                        } else {
                                            info!(peer_id = %target_node_id, count = all_keys.len(), "Wyslano TrustedKeysSync");
                                        }
                                    }

                                    // Rozglosz zaktualizowana liste kluczy do WSZYSTKICH zaufanych peerow
                                    let updated_keys = sec.get_all_trusted_keys();
                                    if updated_keys.len() > 1 {
                                        let entries: Vec<tentaflow_protocol::mesh::TrustedKeyEntry> = updated_keys
                                            .iter()
                                            .map(|(nid, pk)| tentaflow_protocol::mesh::TrustedKeyEntry {
                                                node_id: nid.clone(),
                                                public_key_hex: pk.clone(),
                                            })
                                            .collect();
                                        let payload = tentaflow_protocol::mesh::TrustedKeysSyncPayload { keys: entries };
                                        let broadcast_data = rkyv::to_bytes::<rkyv::rancor::Error>(&payload)
                                            .map(|v| v.to_vec())
                                            .unwrap_or_default();
                                        // Broadcast do wszystkich trusted — pomija nowo sparowanego (juz dostal wyzej)
                                        let results = qm_events.broadcast_to_trusted(
                                            tentaflow_protocol::mesh::MESH_MSG_TRUSTED_KEYS_SYNC,
                                            &broadcast_data,
                                            Some(&target_node_id),
                                        ).await;
                                        for (pid, res) in &results {
                                            if let Err(e) = res {
                                                warn!("Blad broadcast TrustedKeysSync do {}: {}", pid, e);
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                warn!(peer_id = %peer_id, "Blad parsowania PairingConfirm JSON: {}", e);
                            }
                        }
                    }
                }
                Ok(QuicMeshEvent::PairingRejectReceived { peer_id, data }) => {
                    // Parsuj JSON i usun oczekujace parowanie
                    if let Some(ref sec) = mesh_security {
                        match serde_json::from_slice::<serde_json::Value>(&data) {
                            Ok(val) => {
                                let from_node_id = val["from_node_id"].as_str().unwrap_or(&peer_id);
                                if let Err(e) = sec.reject_pairing(from_node_id) {
                                    warn!("Blad odrzucenia parowania od {}: {}", peer_id, e);
                                } else {
                                    info!("Otrzymano PairingReject od {}", peer_id);
                                }
                            }
                            Err(e) => {
                                warn!(peer_id = %peer_id, "Blad parsowania PairingReject JSON: {}", e);
                            }
                        }
                    }
                }
                Ok(QuicMeshEvent::TrustRevokedReceived { node_id, revoked_node_id }) => {
                    if let Some(ref sec) = mesh_security {
                        let sender_trusted = sec.is_trusted(&node_id);
                        let i_am_revoked = revoked_node_id == local_node_id;

                        // Przypadek 1: ja zostalam odlaczony z mesh — usun WSZYSTKIE klucze
                        if i_am_revoked && sender_trusted {
                            let all_trusted = sec.get_all_trusted_keys();
                            for (trusted_id, _) in &all_trusted {
                                let _ = sec.unpair(trusted_id);
                            }
                            info!(
                                "Odlaczony z mesh przez {} — usunieto {} kluczy",
                                node_id, all_trusted.len()
                            );

                            let details = format!("Odlaczony z mesh przez {} — {} kluczy usunietych", node_id, all_trusted.len());
                            let _ = crate::db::repository::log_audit(
                                &sec.db, None, None, "removed_from_mesh", None,
                                Some(&details), None, Some(&node_id),
                            );
                            continue;
                        }

                        // Przypadek 2: ktos inny zostal odlaczony — usun TYLKO jego klucz
                        if sender_trusted && sec.is_trusted(&revoked_node_id) {
                            let _ = sec.unpair(&revoked_node_id);
                            info!("Usunieto {} z mesh (propagacja od {})", revoked_node_id, node_id);

                            let _ = crate::db::repository::log_audit(
                                &sec.db, None, None, "trust_revoked_propagation", None,
                                Some(&format!("Usunieto {} propagacja od {}", revoked_node_id, node_id)),
                                None, Some(&revoked_node_id),
                            );
                        } else if !sender_trusted && !i_am_revoked {
                            warn!("Odrzucono TrustRevoked od niezaufanego noda {}", node_id);
                        }
                    }
                }
                Ok(QuicMeshEvent::NodeLeavingReceived { node_id }) => {
                    let sender_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&node_id),
                        None => false,
                    };
                    if !sender_trusted {
                        warn!("NodeLeaving od niezaufanego noda {}", node_id);
                        continue;
                    }

                    info!("Node {} opuszcza mesh (graceful leave)", node_id);
                    qm_events.disconnect_peer(&node_id).await;
                }
                Ok(QuicMeshEvent::KeyRotationReceived { node_id, ephemeral_public_key_hex }) => {
                    if let Some(ref sec) = mesh_security {
                        if !sec.is_trusted(&node_id) {
                            warn!("KeyRotation od niezaufanego noda {}", node_id);
                            continue;
                        }
                        if let Ok(their_pub_bytes) = hex::decode(&ephemeral_public_key_hex) {
                            if their_pub_bytes.len() == 32 {
                                let mut key = [0u8; 32];
                                key.copy_from_slice(&their_pub_bytes);
                                match sec.respond_to_key_rotation(&node_id, &key) {
                                    Ok((our_pub, epoch)) => {
                                        info!(peer_id = %node_id, epoch, "Rotacja klucza — wyslanie odpowiedzi");
                                        let payload = tentaflow_protocol::mesh::KeyRotationResponsePayload {
                                            from_node_id: local_node_id.to_string(),
                                            ephemeral_public_key: hex::encode(our_pub),
                                        };
                                        let data = rkyv::to_bytes::<rkyv::rancor::Error>(&payload)
                                            .map(|v| v.to_vec())
                                            .unwrap_or_default();
                                        if let Err(e) = qm_events.send_key_rotation_response(&node_id, &data).await {
                                            warn!("Blad wysylania KeyRotationResponse do {}: {}", node_id, e);
                                        }
                                    }
                                    Err(e) => warn!("Blad rotacji klucza dla {}: {}", node_id, e),
                                }
                            }
                        }
                    }
                }
                Ok(QuicMeshEvent::KeyRotationResponseReceived { node_id, ephemeral_public_key_hex }) => {
                    if let Some(ref sec) = mesh_security {
                        if !sec.is_trusted(&node_id) {
                            warn!("KeyRotationResponse od niezaufanego noda {}", node_id);
                            continue;
                        }
                        if let Ok(their_pub_bytes) = hex::decode(&ephemeral_public_key_hex) {
                            if their_pub_bytes.len() == 32 {
                                let mut key = [0u8; 32];
                                key.copy_from_slice(&their_pub_bytes);
                                match sec.finalize_key_rotation(&node_id, &key) {
                                    Ok(epoch) => {
                                        info!(peer_id = %node_id, epoch, "Rotacja klucza sfinalizowana");
                                    }
                                    Err(e) => warn!("Blad finalizacji rotacji dla {}: {}", node_id, e),
                                }
                            }
                        }
                    }
                }
                Ok(QuicMeshEvent::TrustedKeysSyncReceived { node_id, keys }) => {
                    // Akceptuj sync TYLKO od trusted peera
                    let sender_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&node_id),
                        None => false,
                    };
                    if !sender_trusted {
                        warn!("Odrzucono TrustedKeysSync od niezaufanego noda {}", node_id);
                        continue;
                    }

                    if let Some(ref sec) = mesh_security {
                        let mut added = 0u32;
                        for (remote_node_id, public_key_hex) in &keys {
                            if sec.is_trusted(remote_node_id) {
                                continue;
                            }
                            match sec.add_trusted_key(remote_node_id, public_key_hex, "") {
                                Ok(()) => {
                                    added += 1;
                                    info!(node_id = %remote_node_id, "Dodano zaufany klucz z TrustedKeysSync od {}", node_id);
                                }
                                Err(e) => {
                                    warn!(node_id = %remote_node_id, "Blad dodawania klucza z TrustedKeysSync: {}", e);
                                }
                            }
                        }
                        if added > 0 {
                            info!(from = %node_id, added, "TrustedKeysSync przetworzony");
                            // Audit log
                            let details = format!("Dodano {} kluczy z TrustedKeysSync od {}", added, node_id);
                            let _ = crate::db::repository::log_audit(
                                &sec.db, None, None, "trusted_keys_sync", None,
                                Some(&details), None, Some(&node_id),
                            );
                        }
                    }
                }
                Ok(QuicMeshEvent::RelayFrameReceived { from_node_id: _, frame }) => {
                    // Sprawdz TTL
                    if frame.ttl == 0 {
                        warn!(source = %frame.source_node_id, dest = %frame.destination_node_id, "Relay frame TTL wyczerpany — odrzucam");
                        continue;
                    }

                    // Czy ja jestem odbiorca koncowym?
                    if frame.destination_node_id == local_node_id {
                        // Deszyfruj payload kluczem nadawcy (end-to-end)
                        if let Some(ref sec) = mesh_security {
                            match sec.decrypt_from_node(&frame.source_node_id, &frame.payload) {
                                Ok(_decrypted) => {
                                    info!(
                                        source = %frame.source_node_id,
                                        disc = frame.discriminant,
                                        hops = 4u8.saturating_sub(frame.ttl) + 1,
                                        "Otrzymano relay frame (multi-hop)"
                                    );
                                }
                                Err(e) => {
                                    warn!(source = %frame.source_node_id, "Blad deszyfrowania relay payload: {}", e);
                                }
                            }
                        }
                    } else {
                        // Forward do next-hop
                        let mut forwarded_frame = frame;
                        forwarded_frame.ttl -= 1;

                        if let Some(route) = peer_store.get_route(&forwarded_frame.destination_node_id) {
                            let frame_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&forwarded_frame)
                                .map(|v| v.to_vec())
                                .unwrap_or_default();
                            if let Err(e) = qm_events.send_relay_frame(&route.next_hop, &frame_bytes).await {
                                warn!(
                                    dest = %forwarded_frame.destination_node_id,
                                    next_hop = %route.next_hop,
                                    "Blad forwarding relay frame: {}", e
                                );
                            } else {
                                debug!(
                                    source = %forwarded_frame.source_node_id,
                                    dest = %forwarded_frame.destination_node_id,
                                    next_hop = %route.next_hop,
                                    ttl = forwarded_frame.ttl,
                                    "Relay frame forwarded"
                                );
                            }
                        } else {
                            warn!(dest = %forwarded_frame.destination_node_id, "Brak route — nie moge forwardowac relay frame");
                        }
                    }
                }
                Ok(QuicMeshEvent::MeshCommandReceived { from_node_id, command }) => {
                    info!(from = %from_node_id, "Otrzymano MeshCommand — przekazuje do executora");
                    qm_events.handle_command_received(&from_node_id, &command).await;
                }
                Ok(QuicMeshEvent::MeshCommandResponseReceived { from_node_id, data }) => {
                    qm_events.handle_command_response_received(&from_node_id, &data).await;
                }
                Ok(QuicMeshEvent::CrdtDeltaReceived { node_id, .. }) => {
                    // Safety net — przetwarzaj CRDT delta TYLKO od trusted peerow
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&node_id),
                        None => false, // Zero trust — bez MeshSecurity nie przetwarzaj danych
                    };
                    if !is_trusted {
                        debug!(peer_id = %node_id, "Pomijam CrdtDelta od niezaufanego peera (safety net)");
                    }
                    // Dalsze przetwarzanie CRDT delta (jesli bedzie potrzebne) — tu placeholder
                }
                Ok(QuicMeshEvent::FullStateReceived { node_id, .. }) => {
                    // Safety net — przetwarzaj FullState TYLKO od trusted peerow
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&node_id),
                        None => false, // Zero trust — bez MeshSecurity nie przetwarzaj danych
                    };
                    if !is_trusted {
                        debug!(peer_id = %node_id, "Pomijam FullState od niezaufanego peera (safety net)");
                    }
                    // Dalsze przetwarzanie FullState (jesli bedzie potrzebne) — tu placeholder
                }
                Ok(_) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                    warn!("Event receiver opuscil {} wiadomosci", n);
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

fn spawn_docker_cache() -> Arc<tokio::sync::RwLock<Vec<crate::mesh::peer_store::PeerContainerInfo>>>
{
    let docker_cache: Arc<
        tokio::sync::RwLock<Vec<crate::mesh::peer_store::PeerContainerInfo>>,
    > = Arc::new(tokio::sync::RwLock::new(vec![]));

    let dc = docker_cache.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            let containers = tokio::task::spawn_blocking(|| {
                node_info_collector::collect_docker_containers()
            })
            .await
            .unwrap_or_default();
            *dc.write().await = containers;
        }
    });

    docker_cache
}

/// [OPT] Heartbeat sender — co 500ms, zoptymalizowany pod 1000 peerow:
/// - Pre-alokowany bufor serializacji (reuse miedzy iteracjami)
/// - Metryki klonowane raz zamiast 3 razy (gpus, containers, networks)
/// - Serializacja RAZ, potem broadcast do wszystkich peerow
fn spawn_heartbeat_sender(
    quic_mesh: Arc<QuicMeshManager>,
    peer_store: MeshPeerStore,
    local_node_id: String,
    docker_cache: Arc<tokio::sync::RwLock<Vec<crate::mesh::peer_store::PeerContainerInfo>>>,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        let mut heartbeat_count: u64 = 0;
        loop {
            interval.tick().await;
            let metrics = tokio::task::spawn_blocking(|| {
                node_info_collector::collect_fast_metrics()
            })
            .await;
            if let Ok(m) = metrics {
                let containers = docker_cache.read().await.clone();
                let connected_peers = quic_mesh.connected_peer_ids().await;

                // [OPT] Buduj HeartbeatMetrics najpierw, potem aktualizuj store
                // z referencji — unika podwojnego klonowania gpus/containers/networks
                let hb = HeartbeatMetrics {
                    cpu_usage_percent: m.cpu_usage_percent,
                    ram_used_mb: m.ram_used_mb,
                    gpus: m.gpus,
                    containers,
                    networks: m.networks,
                    platform: node_info_collector::detect_platform(),
                    cpu_temperature_c: m.cpu_temperature_c,
                    swap_total_mb: m.swap_total_mb,
                    swap_used_mb: m.swap_used_mb,
                    connected_peers: connected_peers.clone(),
                };

                // Aktualizuj metryki lokalnego noda w store (klonowanie z hb)
                peer_store.update_metrics(
                    &local_node_id,
                    hb.cpu_usage_percent,
                    hb.ram_used_mb,
                    hb.gpus.clone(),
                    hb.containers.clone(),
                    hb.networks.clone(),
                    hb.platform.clone(),
                    hb.cpu_temperature_c,
                    hb.swap_total_mb,
                    hb.swap_used_mb,
                );

                // Aktualizuj topologie lokalnego noda
                peer_store.update_topology(&local_node_id, connected_peers);

                // Serializuj RAZ — broadcast do wszystkich peerow uzywa tych samych bajtow
                if let Ok(data) = rkyv::to_bytes::<rkyv::rancor::Error>(&hb) {
                    quic_mesh.send_heartbeat_data(&data).await;
                }

                // Przelicz routing co 10 heartbeatow (~5s)
                heartbeat_count += 1;
                if heartbeat_count % 10 == 0 {
                    peer_store.recalculate_routes(&local_node_id);
                }
            }
        }
    });
}

/// Slow refresh — co 60s odswiezaj wolno-zmienne dane lokalnego noda:
/// adresy IP, Docker availability/version, OS distro.
fn spawn_slow_refresh(
    peer_store: MeshPeerStore,
    local_node_id: String,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            let result = tokio::task::spawn_blocking(move || {
                let addresses = node_info_collector::collect_local_addresses();
                let (docker_available, docker_version) = node_info_collector::collect_docker_info();
                let os_info = node_info_collector::collect_os_distro();
                (addresses, docker_available, docker_version, os_info)
            })
            .await;

            if let Ok((addresses, docker_available, docker_version, os_info)) = result {
                peer_store.update_local_extras(
                    &local_node_id,
                    addresses,
                    docker_available,
                    docker_version,
                    os_info,
                );
            }
        }
    });
}

/// Periodyczna rotacja kluczy szyfrowania — co 24h
fn spawn_key_rotation_task(
    quic_mesh: Arc<QuicMeshManager>,
    security: Arc<MeshSecurity>,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(crate::mesh::security::KEY_ROTATION_INTERVAL);
        interval.tick().await;

        loop {
            interval.tick().await;
            info!("Rozpoczynam rotacje kluczy");
            rotate_all_keys(&quic_mesh, &security).await;
        }
    });
}

async fn rotate_all_keys(quic_mesh: &QuicMeshManager, security: &MeshSecurity) {
    let trusted_ids = security.trusted_node_ids_snapshot();

    // Wyczysc wygasle pending rotacje
    security.cleanup_pending_rotations();

    for peer_id in trusted_ids.iter() {
        let ephemeral_public = security.initiate_key_rotation(peer_id);
        let payload = tentaflow_protocol::mesh::KeyRotationPayload {
            from_node_id: quic_mesh.node_id().to_string(),
            ephemeral_public_key: hex::encode(ephemeral_public),
        };
        let data = rkyv::to_bytes::<rkyv::rancor::Error>(&payload)
            .map(|v| v.to_vec())
            .unwrap_or_default();

        match quic_mesh.send_key_rotation(peer_id, &data).await {
            Ok(_) => {
                info!(peer_id = %peer_id, "Wyslano KeyRotation request");
            }
            Err(e) => {
                warn!(peer_id = %peer_id, "Blad wysylania KeyRotation: {}", e);
            }
        }
    }
}

/// [CR-011] Periodyczne czyszczenie wygaslych parowan — co 30 sekund
fn spawn_pairing_cleanup(mesh_security: Arc<MeshSecurity>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        loop {
            interval.tick().await;
            match mesh_security.cleanup_expired() {
                Ok(count) => {
                    if count > 0 {
                        debug!("Wyczyszczono {} wygaslych parowan", count);
                    }
                }
                Err(e) => {
                    warn!("Blad czyszczenia wygaslych parowan: {}", e);
                }
            }
        }
    });
}
