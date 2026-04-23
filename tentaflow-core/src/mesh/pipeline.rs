// =============================================================================
// Plik: mesh/pipeline.rs
// Opis: Reużywalny pipeline mesh networking — mDNS discovery, QUIC mesh,
//       heartbeat sender, Docker container cache, NodeInfo exchange.
//       Uzywany przez Router.New, Desktop i Mobile (ta sama logika).
// =============================================================================

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tracing::{debug, error, info, warn};

use crate::config::MeshConfig;
// use crate::mesh::discovery::{MdnsDiscovery, PeerEvent}; — usuniete wraz z mesh/discovery.rs
use crate::mesh::iroh_manager::{IrohMeshConfig, IrohMeshEvent, IrohMeshManager};
use crate::mesh::node_info_collector;
use crate::mesh::peer_store::{HeartbeatMetrics, MeshPeerInfo, MeshPeerStore, NodeInfo};
use crate::mesh::security::MeshSecurity;
use crate::net::iroh::load_relay_url;
use crate::routing::live_metrics;

/// Snapshot live-metrics routera — zwracany do heartbeat.
fn routing_metrics_snapshot() -> (u32, f32) {
    live_metrics::snapshot()
}

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
    /// Legacy: zachowane jako `Option<()>` dla compat z istniejacymi callerami.
    /// iroh obsluguje LAN mDNS przez MdnsAddressLookup, nie ma osobnego handle.
    pub mdns: Option<()>,
    /// IrohMeshManager — forward handler, connections, wszystkie send_* metody.
    pub quic_mesh: Option<Arc<IrohMeshManager>>,
    /// MeshSecurity — tozsamosc Ed25519, trusted_keys, pairing.
    pub security: Option<Arc<MeshSecurity>>,
}

impl MeshPipelineHandles {
    /// Graceful shutdown — zamyka iroh endpoint i wszystkie polaczenia.
    pub async fn shutdown(mut self) {
        if let Some(ref qm) = self.quic_mesh {
            qm.send_node_leaving().await;
            qm.shutdown().await;
        }
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
    _settings_cipher: std::sync::Arc<crate::crypto::SettingsCipher>,
    mesh_security: Arc<MeshSecurity>,
) -> Result<MeshPipelineHandles> {
    let app_node_id = &config.node_id;
    let mesh_config = &config.mesh_config;
    let mesh_port = mesh_config.port;

    info!(
        "Inicjalizacja mesh networking (port {}, node_id: {})",
        mesh_port,
        &app_node_id[..16.min(app_node_id.len())]
    );

    // iroh endpoint: LAN mDNS + pkarr-DHT discovery + relay — wszystko wbudowane.
    // mdns_enabled=false na iOS bo Apple blokuje raw multicast bez entitlementa;
    // zamiast tego Swift NWBrowser karmi iroh przez FFI tentaflow_mobile_add_discovered_peer.
    // DHT wylaczony na mobile — mainline bootstrap spowalnia start, a LAN Bonjour
    // + iroh relay wystarczaja do discovery peerow.
    let enable_dht = cfg!(not(any(target_os = "ios", target_os = "android")));
    let relay_url = db_pool
        .as_ref()
        .map(|db| load_relay_url(db, Some(mesh_config)))
        .or_else(|| mesh_config.iroh_relay_url.parse().ok());
    let iroh_cfg = IrohMeshConfig {
        node_id: app_node_id.clone(),
        bind_addr: std::net::SocketAddr::from(([0, 0, 0, 0], mesh_port)),
        relay_url,
        heartbeat_interval: Duration::from_millis(mesh_config.heartbeat_interval_ms),
        enable_lan_discovery: mesh_config.mdns_enabled,
        enable_dht_discovery: enable_dht,
    };

    let security_for_mesh = mesh_security.clone();

    match IrohMeshManager::new(iroh_cfg, security_for_mesh).await {
        Ok(quic_mesh) => {
            let local_node_id = quic_mesh.node_id();
            let local_node_info = node_info_collector::collect_node_info(&local_node_id);
            upsert_local_peer(
                mesh_peer_store,
                &local_node_id,
                &config.role,
                mesh_port,
                &local_node_info,
            );

            {
                let qm = quic_mesh.clone();
                tokio::spawn(async move {
                    qm.start();
                });
            }

            // Reconnect do trusted peerow po EndpointId — iroh sam rozwiazuje adres.
            {
                if let Ok(trusted) = crate::db::repository::list_trusted_nodes(&mesh_security.db)
                {
                    for node in &trusted {
                        let qm = quic_mesh.clone();
                        let nid = node.node_id.clone();
                        tokio::spawn(async move {
                            let dummy_addr = std::net::SocketAddr::from(([0, 0, 0, 0], 0));
                            if let Err(e) = qm.connect_to_peer(&nid, dummy_addr).await {
                                debug!(peer_id = %nid, "Reconnect via iroh: {}", e);
                            }
                        });
                    }
                }
            }

            // Reconnect loop — co 15s iteruje peer_store i dla kazdego peera
            // ktory nie jest aktualnie polaczony (quic_connected=false) probuje
            // `connect_to_peer`. Iroh rozwiazuje adres przez mDNS/DHT. Dzieki
            // temu peer ktory padl (PeerDisconnected) zostanie automatycznie
            // redialowany bez czekania na kolejny DiscoveryEvent.
            //
            // Dodatkowo na Resume (iOS wake z suspendu) robimy natychmiastowy
            // przebieg bez czekania do 15s — peerzy po naszej stronie mogli
            // nas uznac za martwych i trzeba od nowa zestawic QUIC.
            {
                let qm = quic_mesh.clone();
                let store = mesh_peer_store.clone();
                let self_id = local_node_id.clone();
                let mut lifecycle_rx = crate::lifecycle_signal::subscribe();
                tokio::spawn(async move {
                    let dummy = std::net::SocketAddr::from(([0, 0, 0, 0], 0));
                    let mut ticker = tokio::time::interval(Duration::from_secs(15));
                    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

                    let run_iteration = |trigger: &str| {
                        let peers = store.list();
                        let mut redialed = 0usize;
                        for p in peers.iter() {
                            if p.node_id == self_id || p.quic_connected {
                                continue;
                            }
                            let qm2 = qm.clone();
                            let nid = p.node_id.clone();
                            tokio::spawn(async move {
                                if let Err(e) = qm2.connect_to_peer(&nid, dummy).await {
                                    debug!(peer_id = %nid, "Reconnect loop: {}", e);
                                }
                            });
                            redialed += 1;
                        }
                        if redialed > 0 {
                            debug!("reconnect-loop({}): {} peer(ow) redial", trigger, redialed);
                        }
                    };

                    loop {
                        tokio::select! {
                            _ = ticker.tick() => {
                                run_iteration("timer");
                            }
                            lc = lifecycle_rx.recv() => {
                                match lc {
                                    Ok(crate::lifecycle_signal::LifecycleEvent::Resume) => {
                                        info!("mesh reconnect: Resume — force redial wszystkich peerow");
                                        run_iteration("resume");
                                    }
                                    Ok(_) => {}
                                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {}
                                }
                            }
                        }
                    }
                });
            }

            // Bootstrap peer_store z persistowanych snapshotow mesh_topology
            // (pozwala widziec znane nody zaraz po starcie, zanim przyjdzie gossip).
            if let Some(ref pool) = db_pool {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                let _ = crate::db::repository::mesh_topology::cleanup_stale(pool, now_ms);
                if let Ok(snaps) = crate::db::repository::mesh_topology::list_all(pool) {
                    for s in &snaps {
                        if s.node_id == local_node_id {
                            continue;
                        }
                        let addrs: Vec<std::net::IpAddr> = s
                            .direct_addrs
                            .iter()
                            .filter_map(|a| a.parse::<std::net::SocketAddr>().ok())
                            .map(|sa| sa.ip())
                            .collect();
                        mesh_peer_store.upsert_gossip_peer(
                            &s.node_id,
                            &s.hostname,
                            &s.platform,
                            &s.os_info,
                            addrs,
                            s.port,
                        );
                        mesh_peer_store.update_topology(&s.node_id, s.connected_to.clone());
                    }
                    if !snaps.is_empty() {
                        mesh_peer_store.recalculate_routes(&local_node_id);
                        info!(
                            "Bootstrap: zaladowano {} snapshot(ow) mesh_topology z DB",
                            snaps.len()
                        );
                    }
                }
            }

            spawn_quic_event_handler(
                quic_mesh.clone(),
                mesh_peer_store.clone(),
                local_node_info.clone(),
                Some(mesh_security.clone()),
                local_node_id.clone(),
                db_pool.clone(),
            );

            let docker_cache = spawn_docker_cache();
            spawn_heartbeat_sender(
                quic_mesh.clone(),
                mesh_peer_store.clone(),
                local_node_id.clone(),
                docker_cache,
            );
            spawn_slow_refresh(mesh_peer_store.clone(), local_node_id.clone());
            spawn_liveness_timer(
                mesh_peer_store.clone(),
                quic_mesh.clone(),
                local_node_id.clone(),
                Some(mesh_security.clone()),
            );

            spawn_pairing_cleanup(mesh_security.clone());

            info!("Mesh networking uruchomiony (iroh transport)");

            Ok(MeshPipelineHandles {
                mdns: None,
                quic_mesh: Some(quic_mesh),
                security: Some(mesh_security),
            })
        }
        Err(e) => {
            error!("Nie udalo sie utworzyc IrohMeshManager: {}", e);
            let local_node_info = node_info_collector::collect_node_info(app_node_id);
            upsert_local_peer(
                mesh_peer_store,
                app_node_id,
                &config.role,
                mesh_port,
                &local_node_info,
            );
            Ok(MeshPipelineHandles {
                mdns: None,
                quic_mesh: None,
                security: Some(mesh_security),
            })
        }
    }
}

fn upsert_local_peer(
    mesh_peer_store: &MeshPeerStore,
    local_node_id: &str,
    role: &str,
    mesh_port: u16,
    local_node_info: &NodeInfo,
) {
    let local_addresses = node_info_collector::collect_local_addresses();
    let local_os_distro = node_info_collector::collect_os_distro();
    let (docker_available, docker_version) = node_info_collector::collect_docker_info();

    mesh_peer_store.add_or_update(MeshPeerInfo {
        node_id: local_node_id.to_string(),
        addresses: local_addresses,
        port: mesh_port,
        role: role.to_string(),
        status: "connected".to_string(),
        quic_connected: true,
        discovered_at: chrono::Utc::now().to_rfc3339(),
        hostname: local_node_info.hostname.clone(),
        os_info: if local_os_distro.is_empty() {
            local_node_info.os_info.clone()
        } else {
            local_os_distro
        },
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
        models: vec![],
        active_requests: 0,
        tokens_per_sec: 0.0,
    });
}

// =============================================================================
// Wewnetrzne taski mesh pipeline
// =============================================================================

fn spawn_quic_event_handler(
    quic_mesh: Arc<IrohMeshManager>,
    peer_store: MeshPeerStore,
    local_node_info: NodeInfo,
    mesh_security: Option<Arc<MeshSecurity>>,
    local_node_id: String,
    db_pool: Option<crate::db::DbPool>,
) {
    let qm_events = quic_mesh.clone();
    let mut event_rx = quic_mesh.subscribe();

    tokio::spawn(async move {
        let mut last_sync_sent: std::collections::HashMap<String, std::time::Instant> =
            std::collections::HashMap::new();
        const SYNC_COOLDOWN_SECS: u64 = 30;

        // Dedup cache dla TopologyAnnounce — klucz (origin_node_id, epoch).
        // Max 512 wpisow, FIFO eviction. Zapobiega zapetleniom przy flood rebroadcast.
        let mut topo_seen: std::collections::VecDeque<(String, u64)> =
            std::collections::VecDeque::with_capacity(512);
        const TOPO_SEEN_CAP: usize = 512;

        // Cooldown na auto-dial z KnownPeers — zapobiega dial stormow gdy peer
        // wysyla wielokrotnie KnownPeers w jednej sekundzie (iroh multi-path).
        let mut last_dial_at: std::collections::HashMap<String, std::time::Instant> =
            std::collections::HashMap::new();
        const DIAL_COOLDOWN_SECS: u64 = 30;

        loop {
            match event_rx.recv().await {
                Ok(IrohMeshEvent::HelloReceived { node_id, data }) => {
                    // Hello przyjmujemy od KAZDEGO peera — to tylko identyfikacja
                    // (hostname + platform), bez metryk. Daje GUI czytelna nazwe
                    // na karcie discovered przed pairingiem.
                    use tentaflow_protocol::mesh::MeshHelloPayload;
                    match rkyv::from_bytes::<MeshHelloPayload, rkyv::rancor::Error>(&data) {
                        Ok(hello) => {
                            debug!(
                                peer_id = %node_id,
                                hostname = %hello.hostname,
                                platform = %hello.platform,
                                "Otrzymano Hello od peera"
                            );
                            peer_store.set_hostname(&node_id, &hello.hostname);
                            peer_store.set_platform(&node_id, &hello.platform);
                            if !hello.os_info.is_empty() {
                                peer_store.set_os_info(&node_id, &hello.os_info);
                            }
                        }
                        Err(e) => {
                            warn!(peer_id = %node_id, "Blad deserializacji Hello: {}", e);
                        }
                    }
                }
                Ok(IrohMeshEvent::KnownPeersReceived { from_node_id, data }) => {
                    // Pre-trust discovery gossip — peer X polaczyl sie z nami i przekazuje
                    // liste peerow ktorych on widzi (tj. jest z nimi polaczony QUIC-iem).
                    // Akceptujemy od KAZDEGO peera bo to tylko info dyskawerii, bez
                    // wrazliwych danych. Probujemy sie polaczyc z kazdym nieznanym.
                    use tentaflow_protocol::mesh::KnownPeersPayload;
                    let payload = match rkyv::from_bytes::<KnownPeersPayload, rkyv::rancor::Error>(
                        &data,
                    ) {
                        Ok(p) => p,
                        Err(e) => {
                            warn!(peer = %from_node_id, "Blad deserializacji KnownPeers: {}", e);
                            continue;
                        }
                    };
                    debug!(
                        from = %from_node_id,
                        count = payload.peers.len(),
                        "Otrzymano KnownPeers"
                    );
                    for entry in &payload.peers {
                        if entry.node_id == local_node_id {
                            continue;
                        }
                        if peer_store.is_quic_connected(&entry.node_id) {
                            continue;
                        }
                        // Cooldown — nie probuj ponownie przez 30s nawet jesli ten
                        // sam peer zostanie anonsowany znowu.
                        let recent = last_dial_at
                            .get(&entry.node_id)
                            .map(|t| {
                                t.elapsed() < std::time::Duration::from_secs(DIAL_COOLDOWN_SECS)
                            })
                            .unwrap_or(false);
                        if recent {
                            continue;
                        }
                        last_dial_at.insert(entry.node_id.clone(), std::time::Instant::now());

                        let addrs: Vec<std::net::IpAddr> = entry
                            .direct_addrs
                            .iter()
                            .filter_map(|s| s.parse::<std::net::SocketAddr>().ok())
                            .map(|sa| sa.ip())
                            .collect();
                        if !addrs.is_empty() {
                            peer_store.set_addresses(&entry.node_id, addrs);
                        }
                        if !entry.hostname.is_empty() {
                            peer_store.set_hostname(&entry.node_id, &entry.hostname);
                        }
                        peer_store.set_status(&entry.node_id, "discovered");

                        // Auto-dial — iroh sam zajmuje sie NAT traversal + relay gdy
                        // direct addr nie dziala.
                        let target = entry.node_id.clone();
                        let qm = qm_events.clone();
                        let dial_addr = entry
                            .direct_addrs
                            .iter()
                            .filter_map(|s| s.parse::<std::net::SocketAddr>().ok())
                            .next()
                            .unwrap_or_else(|| std::net::SocketAddr::from(([0, 0, 0, 0], 0)));
                        tokio::spawn(async move {
                            match qm.connect_to_peer(&target, dial_addr).await {
                                Ok(_) => debug!(peer = %target, "Auto-dial (KnownPeers): OK"),
                                Err(e) => debug!(peer = %target, "Auto-dial (KnownPeers): {}", e),
                            }
                        });
                    }
                }
                Ok(IrohMeshEvent::TopologyAnnounceReceived { from_node_id, data }) => {
                    // Gossip multi-hop — wprowadza nody osiagalne przez relay.
                    // Akceptujemy TYLKO od trusted peerow (bezpieczenstwo).
                    let sender_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&from_node_id),
                        None => false,
                    };
                    if !sender_trusted {
                        debug!(peer = %from_node_id, "Pomijam TopologyAnnounce od niezaufanego peera");
                        continue;
                    }

                    use tentaflow_protocol::mesh::TopologyAnnouncePayload;
                    let payload = match rkyv::from_bytes::<
                        TopologyAnnouncePayload,
                        rkyv::rancor::Error,
                    >(&data)
                    {
                        Ok(p) => p,
                        Err(e) => {
                            warn!(peer = %from_node_id, "Blad deserializacji TopologyAnnounce: {}", e);
                            continue;
                        }
                    };

                    // Dedup po (origin, epoch)
                    let key = (payload.origin_node_id.clone(), payload.epoch);
                    if topo_seen.iter().any(|k| *k == key) {
                        continue;
                    }
                    topo_seen.push_back(key);
                    if topo_seen.len() > TOPO_SEEN_CAP {
                        topo_seen.pop_front();
                    }

                    // Aktualizuj peer_store + topologie dla kazdego wpisu
                    for entry in &payload.entries {
                        if entry.node_id == local_node_id {
                            continue;
                        }
                        let addrs: Vec<std::net::IpAddr> = entry
                            .direct_addrs
                            .iter()
                            .filter_map(|s| s.parse::<std::net::SocketAddr>().ok())
                            .map(|sa| sa.ip())
                            .collect();
                        peer_store.upsert_gossip_peer(
                            &entry.node_id,
                            &entry.hostname,
                            &entry.platform,
                            &entry.os_info,
                            addrs,
                            entry.port,
                        );
                        peer_store.update_topology(&entry.node_id, entry.connected_to.clone());
                        // Modele jako PeerModelInfo — przepisujemy z ModelSummary
                        if !entry.models.is_empty() {
                            let models: Vec<crate::mesh::peer_store::PeerModelInfo> = entry
                                .models
                                .iter()
                                .map(|m| crate::mesh::peer_store::PeerModelInfo {
                                    alias: m.alias.clone(),
                                    kind: String::new(),
                                    backend: m.backend.clone(),
                                    size_mb: 0,
                                    loaded: m.loaded,
                                })
                                .collect();
                            peer_store.update_models(&entry.node_id, models);
                        }
                        // Uslugi — wpisujemy do service_registry jako remote
                        // (pozwala GUI mesh-service browserowi pokazac spark-002 uslugi).
                        if !entry.services.is_empty() {
                            let services: Vec<tentaflow_protocol::mesh::MeshServiceInfo> = entry
                                .services
                                .iter()
                                .map(|s| tentaflow_protocol::mesh::MeshServiceInfo {
                                    service_id: format!("{}-{}", entry.node_id, s.name),
                                    service_name: s.name.clone(),
                                    service_type: s.service_type.clone(),
                                    node_id: entry.node_id.clone(),
                                    quic_port: entry.port,
                                    quic_url: String::new(),
                                    status: if s.ready {
                                        "ready".to_string()
                                    } else {
                                        "stopped".to_string()
                                    },
                                    models: Vec::new(),
                                    load_percent: 0,
                                    engine_id: None,
                                    model_sizes_mb: Vec::new(),
                                })
                                .collect();
                            qm_events
                                .service_registry()
                                .update_remote(&entry.node_id, services);
                        }
                        // Persystuj snapshot do DB — bootstrap po restarcie.
                        if let Some(ref pool) = db_pool {
                            let services_json = serde_json::to_string(
                                &entry
                                    .services
                                    .iter()
                                    .map(|s| {
                                        serde_json::json!({
                                            "name": s.name,
                                            "service_type": s.service_type,
                                            "ready": s.ready,
                                        })
                                    })
                                    .collect::<Vec<_>>(),
                            )
                            .unwrap_or_else(|_| "[]".to_string());
                            let models_json = serde_json::to_string(
                                &entry
                                    .models
                                    .iter()
                                    .map(|m| {
                                        serde_json::json!({
                                            "alias": m.alias,
                                            "backend": m.backend,
                                            "loaded": m.loaded,
                                        })
                                    })
                                    .collect::<Vec<_>>(),
                            )
                            .unwrap_or_else(|_| "[]".to_string());
                            let now_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as i64)
                                .unwrap_or(0);
                            if let Err(e) = crate::db::repository::mesh_topology::upsert(
                                pool,
                                &entry.node_id,
                                &entry.hostname,
                                &entry.platform,
                                &entry.os_info,
                                &entry.connected_to,
                                &entry.direct_addrs,
                                entry.port,
                                &services_json,
                                &models_json,
                                payload.epoch,
                                now_ms,
                            ) {
                                debug!(node = %entry.node_id, "mesh_topology upsert: {}", e);
                            }
                        }
                    }
                    peer_store.recalculate_routes(&local_node_id);

                    // Auto-dial fallback: jesli gossip anonsuje trusted peera ktorego
                    // mDNS/DHT nie zlapal (2 nody na LAN nie widza sie przez multicast),
                    // probujemy sie polaczyc z niego przez direct_addrs z TopologyEntry.
                    // Iroh sam zajmie sie NAT traversal i relay gdy direct addr nie dziala.
                    if let Some(ref sec) = mesh_security {
                        for entry in &payload.entries {
                            if entry.node_id == local_node_id {
                                continue;
                            }
                            if !sec.is_trusted(&entry.node_id) {
                                continue;
                            }
                            if peer_store.is_quic_connected(&entry.node_id) {
                                continue;
                            }
                            let target = entry.node_id.clone();
                            let qm = qm_events.clone();
                            let addrs: Vec<std::net::SocketAddr> = entry
                                .direct_addrs
                                .iter()
                                .filter_map(|s| s.parse::<std::net::SocketAddr>().ok())
                                .collect();
                            tokio::spawn(async move {
                                let dial_addr = addrs.into_iter().next().unwrap_or_else(|| {
                                    std::net::SocketAddr::from(([0, 0, 0, 0], 0))
                                });
                                match qm.connect_to_peer(&target, dial_addr).await {
                                    Ok(_) => debug!(
                                        peer = %target,
                                        "Auto-dial z TopologyAnnounce udany — iroh polaczony"
                                    ),
                                    Err(e) => debug!(
                                        peer = %target,
                                        "Auto-dial z TopologyAnnounce nie zadzialal: {}",
                                        e
                                    ),
                                }
                            });
                        }
                    }

                    // Flood-rebroadcast — TTL - 1, pomijamy nadawce i origin.
                    if payload.ttl > 1 {
                        let mut forwarded = payload.clone();
                        forwarded.ttl -= 1;
                        if let Ok(forwarded_bytes) =
                            rkyv::to_bytes::<rkyv::rancor::Error>(&forwarded)
                        {
                            let bytes_vec = forwarded_bytes.to_vec();
                            let skip_from = from_node_id.clone();
                            let skip_origin = payload.origin_node_id.clone();
                            for peer in peer_store.list() {
                                if !peer.quic_connected {
                                    continue;
                                }
                                if peer.node_id == skip_from || peer.node_id == skip_origin {
                                    continue;
                                }
                                if peer.node_id == local_node_id {
                                    continue;
                                }
                                let trusted = match &mesh_security {
                                    Some(sec) => sec.is_trusted(&peer.node_id),
                                    None => false,
                                };
                                if !trusted {
                                    continue;
                                }
                                if let Err(e) = qm_events
                                    .send_topology_announce(&peer.node_id, &bytes_vec)
                                    .await
                                {
                                    debug!(peer = %peer.node_id, "Blad rebroadcast TopologyAnnounce: {}", e);
                                }
                            }
                        }
                    }
                }
                Ok(IrohMeshEvent::NodeInfoReceived { node_id, data }) => {
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
                Ok(IrohMeshEvent::PeerConnected { node_id }) => {
                    // Deduplikuj — iroh czesto generuje wiele PeerConnected dla tego
                    // samego peera (direct + relay path). Toast/event emitujemy tylko
                    // na prawdziwa transitioned offline→online.
                    let was_connected = peer_store.is_quic_connected(&node_id);
                    peer_store.set_quic_connected(&node_id, true);
                    peer_store.set_status(&node_id, "connected");
                    peer_store.mark_heartbeat(&node_id);
                    if was_connected {
                        debug!(peer_id = %node_id, "QUIC peer — duplicate connected event (iroh multi-path)");
                        continue;
                    }
                    info!(peer_id = %node_id, "QUIC peer polaczony");

                    // Emit event do GUI — toast "peer connected" + refresh mesh view.
                    let hostname_ev = peer_store
                        .get(&node_id)
                        .map(|p| p.hostname)
                        .unwrap_or_default();
                    crate::dispatch::system_event_broadcast::publish_mesh_peer_status(
                        &node_id,
                        &hostname_ev,
                        "online",
                        "",
                    );

                    // Wyslij minimalne Hello (hostname + platform) niezaleznie od trust —
                    // GUI potrzebuje rozpoznawalnej nazwy na karcie discovered przed
                    // zakonczeniem pairingu. To tylko info identyfikujace, bez metryk.
                    let hello = tentaflow_protocol::mesh::MeshHelloPayload {
                        hostname: local_node_info.hostname.clone(),
                        platform: node_info_collector::detect_platform(),
                        os_info: local_node_info.os_info.clone(),
                    };
                    if let Ok(hello_bytes) = rkyv::to_bytes::<rkyv::rancor::Error>(&hello) {
                        if let Err(e) = qm_events.send_hello(&node_id, &hello_bytes).await {
                            warn!("Blad wysylania Hello do {}: {}", node_id, e);
                        }
                    }

                    // Wyslij KnownPeers — liste wszystkich aktualnie polaczonych peerow,
                    // zeby nowy peer mogl sie z nimi polaczyc bez mDNS (ratuje scenariusz
                    // enterprise VLAN z zablokowanym inter-client multicast).
                    let known: Vec<tentaflow_protocol::mesh::KnownPeerEntry> = peer_store
                        .list()
                        .into_iter()
                        .filter(|p| p.node_id != node_id && p.node_id != local_node_id)
                        .filter(|p| p.quic_connected && !p.addresses.is_empty())
                        .map(|p| tentaflow_protocol::mesh::KnownPeerEntry {
                            node_id: p.node_id.clone(),
                            hostname: p.hostname.clone(),
                            direct_addrs: p
                                .addresses
                                .iter()
                                .map(|ip| format!("{}:{}", ip, p.port))
                                .collect(),
                            port: p.port,
                        })
                        .collect();
                    if !known.is_empty() {
                        let payload = tentaflow_protocol::mesh::KnownPeersPayload { peers: known };
                        if let Ok(kp_bytes) = rkyv::to_bytes::<rkyv::rancor::Error>(&payload) {
                            if let Err(e) = qm_events.send_known_peers(&node_id, &kp_bytes).await {
                                debug!("Blad wysylania KnownPeers do {}: {}", node_id, e);
                            }
                        }
                    }

                    // Wyslij swoje NodeInfo do nowego peera — TYLKO jesli zaufany
                    let should_send = match &mesh_security {
                        Some(sec) => sec.is_trusted(&node_id),
                        None => false, // Zero trust — bez MeshSecurity nie wysylaj danych
                    };
                    if should_send {
                        if let Ok(info_bytes) =
                            rkyv::to_bytes::<rkyv::rancor::Error>(&local_node_info)
                        {
                            if let Err(e) = qm_events.send_node_info(&node_id, &info_bytes).await {
                                warn!("Blad wysylania NodeInfo do {}: {}", node_id, e);
                            }
                        }

                        // Synchronizacja zaufanych kluczy przy reconnect (z cooldownem)
                        if let Some(ref sec) = mesh_security {
                            let should_sync = last_sync_sent.get(&node_id).map_or(true, |t| {
                                t.elapsed() >= std::time::Duration::from_secs(SYNC_COOLDOWN_SECS)
                            });

                            if should_sync {
                                let all_keys = sec.get_all_trusted_keys();
                                if !all_keys.is_empty() {
                                    let entries: Vec<tentaflow_protocol::mesh::TrustedKeyEntry> =
                                        all_keys
                                            .iter()
                                            .map(|(nid, pk)| {
                                                tentaflow_protocol::mesh::TrustedKeyEntry {
                                                    node_id: nid.clone(),
                                                    public_key_hex: pk.clone(),
                                                }
                                            })
                                            .collect();
                                    let payload =
                                        tentaflow_protocol::mesh::TrustedKeysSyncPayload {
                                            keys: entries,
                                        };
                                    if let Ok(sync_data) =
                                        rkyv::to_bytes::<rkyv::rancor::Error>(&payload)
                                            .map(|v| v.to_vec())
                                    {
                                        if let Err(e) = qm_events
                                            .send_trusted_keys_sync(&node_id, &sync_data)
                                            .await
                                        {
                                            warn!(
                                                "Blad wysylania TrustedKeysSync do {}: {}",
                                                node_id, e
                                            );
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
                                    if let Ok(data) =
                                        rkyv::to_bytes::<rkyv::rancor::Error>(&payload)
                                            .map(|v| v.to_vec())
                                    {
                                        let _ = qm_events
                                            .send_to_peer(
                                                &node_id,
                                                tentaflow_protocol::mesh::MESH_MSG_TRUST_REVOKED,
                                                &data,
                                            )
                                            .await;
                                    }
                                }

                                last_sync_sent.insert(node_id.clone(), std::time::Instant::now());
                            }
                        }
                    } else {
                        debug!(peer_id = %node_id, "Peer niezaufany — pomijam wysylanie NodeInfo");
                    }

                    // Persist adresy trusted peera do DB (do reconnectu po restarcie)
                    if let Some(ref sec) = mesh_security {
                        if sec.is_trusted(&node_id) {
                            if let Some(peer_info) = peer_store.get(&node_id) {
                                if !peer_info.addresses.is_empty() && peer_info.port > 0 {
                                    let addr_str = peer_info
                                        .addresses
                                        .iter()
                                        .map(|ip| format!("{}:{}", ip, peer_info.port))
                                        .collect::<Vec<_>>()
                                        .join(",");
                                    let _ = crate::db::repository::update_trusted_node_addresses(
                                        &sec.db, &node_id, &addr_str,
                                    );
                                }
                            }
                        }
                    }

                    // Przelicz routing po polaczeniu nowego peera
                    peer_store.recalculate_routes(&local_node_id);
                }
                Ok(IrohMeshEvent::PeerDisconnected { node_id }) => {
                    // Dedup — iroh multi-path moze emitowac kilka disconnect dla tego
                    // samego peera. Emit event tylko na transition connected→offline.
                    let was_connected = peer_store.is_quic_connected(&node_id);
                    peer_store.set_quic_connected(&node_id, false);
                    peer_store.set_status(&node_id, "disconnected");
                    peer_store.clear_heartbeat(&node_id);
                    if !was_connected {
                        debug!(peer_id = %node_id, "QUIC peer — duplicate disconnect event");
                        continue;
                    }
                    info!(peer_id = %node_id, "QUIC peer rozlaczony");

                    // Emituj event do GUI — pokazuje toast + odswieza karty mesh.
                    let hostname = peer_store
                        .get(&node_id)
                        .map(|p| p.hostname)
                        .unwrap_or_default();
                    crate::dispatch::system_event_broadcast::publish_mesh_peer_status(
                        &node_id,
                        &hostname,
                        "offline",
                        "QUIC disconnect",
                    );

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
                                if let Ok(trusted) =
                                    crate::db::repository::list_trusted_nodes(&sec.db)
                                {
                                    if let Some(tn) = trusted.iter().find(|t| t.node_id == node_id)
                                    {
                                        for part in tn.last_addresses.split(',') {
                                            if let Ok(addr) =
                                                part.trim().parse::<std::net::SocketAddr>()
                                            {
                                                addrs.push(addr);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        // iroh sam wykonuje reconnect przez discovery + relay
                        // gdy peer wroci online — nie potrzebujemy wlasnej petli.
                        let _ = addrs;
                    }
                }
                Ok(IrohMeshEvent::HeartbeatReceived { node_id, heartbeat }) => {
                    // Odnotuj heartbeat dla liveness timera ZAWSZE — sama ramka =
                    // peer zyje, niezaleznie od trust. Inaczej liveness bedzie
                    // wywalac wszystkich niezaufanych peerow co 15s.
                    peer_store.mark_heartbeat(&node_id);
                    // Safety net — przetwarzaj CONTENT heartbeatu TYLKO od trusted.
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&node_id),
                        None => false,
                    };
                    if !is_trusted {
                        debug!(peer_id = %node_id, "Pomijam content heartbeatu od niezaufanego peera (safety net)");
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
                            metrics.active_requests,
                            metrics.tokens_per_sec,
                        );

                        // Aktualizuj topologie peera na podstawie jego connected_peers
                        peer_store.update_topology(&node_id, metrics.connected_peers);
                    }
                }
                Ok(IrohMeshEvent::PairingRequestReceived { peer_id, data }) => {
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
                                if let Err(e) =
                                    sec.receive_pairing_request(from_node_id, pin, public_key)
                                {
                                    warn!("Blad zapisu PairingRequest od {}: {}", peer_id, e);
                                } else {
                                    info!("PairingRequest od {} zapisany — oczekuje na potwierdzenie PIN", from_node_id);
                                    // Auto-confirm jesli PIN pochodzi z naszego QR invite —
                                    // user na drugim nodzie juz zeskanowal kod i jego intent
                                    // jest jednoznaczny. Zadna dodatkowa akcja po stronie
                                    // wlasciciela tego noda nie jest potrzebna.
                                    if sec.consume_invite_pin(pin) {
                                        info!(
                                            from = %from_node_id,
                                            "PairingRequest PIN zgodny z QR invite — auto-confirm"
                                        );
                                        let body_json = serde_json::json!({
                                            "pin": pin,
                                            "hostname": "",
                                        })
                                        .to_string();
                                        let quic_mesh_clone = Some(qm_events.clone());
                                        let res =
                                            crate::api::dashboard::api_mesh::handle_confirm_pairing(
                                                sec,
                                                from_node_id,
                                                body_json.as_bytes(),
                                                &quic_mesh_clone,
                                                &local_node_id,
                                            );
                                        match res {
                                            Ok((status, _body)) if status == 200 => {
                                                info!(from = %from_node_id, "Auto-confirm OK");
                                            }
                                            Ok((status, body)) => {
                                                warn!(from = %from_node_id, status, body = %body, "Auto-confirm: non-200");
                                            }
                                            Err(e) => {
                                                warn!(from = %from_node_id, "Auto-confirm: {}", e);
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                warn!(peer_id = %peer_id, "Blad parsowania PairingRequest JSON: {}", e);
                            }
                        }
                    }
                }
                Ok(IrohMeshEvent::PairingConfirmReceived { peer_id, data }) => {
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
                                        warn!(
                                            "PairingConfirm od {} — nieprawidlowy PIN",
                                            from_node_id
                                        );
                                        continue;
                                    }
                                }

                                if let Err(e) = sec.confirm_pairing(
                                    from_node_id,
                                    public_key,
                                    hostname,
                                    "mesh-quic",
                                ) {
                                    warn!("Blad potwierdzenia parowania od {}: {}", peer_id, e);
                                } else {
                                    let _ = crate::net::iroh::pairing::delete_pending_contact_hints(
                                        &sec.db,
                                        from_node_id,
                                    );
                                    info!("Otrzymano PairingConfirm od {} — node zaufany", peer_id);

                                    // Po sparowaniu — wyslij NodeInfo do nowo zaufanego peera
                                    let target_node_id = from_node_id.to_string();
                                    if let Ok(info_bytes) =
                                        rkyv::to_bytes::<rkyv::rancor::Error>(&local_node_info)
                                    {
                                        if let Err(e) = qm_events
                                            .send_node_info(&target_node_id, &info_bytes)
                                            .await
                                        {
                                            warn!(
                                                "Blad wysylania NodeInfo po sparowaniu do {}: {}",
                                                target_node_id, e
                                            );
                                        } else {
                                            info!(peer_id = %target_node_id, "Wyslano NodeInfo do nowo zaufanego peera");
                                        }
                                    }

                                    // Wyslij TrustedKeysSync z naszymi zaufanymi kluczami
                                    let all_keys = sec.get_all_trusted_keys();
                                    if !all_keys.is_empty() {
                                        let entries: Vec<
                                            tentaflow_protocol::mesh::TrustedKeyEntry,
                                        > = all_keys
                                            .iter()
                                            .map(|(nid, pk)| {
                                                tentaflow_protocol::mesh::TrustedKeyEntry {
                                                    node_id: nid.clone(),
                                                    public_key_hex: pk.clone(),
                                                }
                                            })
                                            .collect();
                                        let payload =
                                            tentaflow_protocol::mesh::TrustedKeysSyncPayload {
                                                keys: entries,
                                            };
                                        let sync_data =
                                            rkyv::to_bytes::<rkyv::rancor::Error>(&payload)
                                                .map(|v| v.to_vec())
                                                .unwrap_or_default();
                                        if let Err(e) = qm_events
                                            .send_trusted_keys_sync(&target_node_id, &sync_data)
                                            .await
                                        {
                                            warn!(
                                                "Blad wysylania TrustedKeysSync do {}: {}",
                                                target_node_id, e
                                            );
                                        } else {
                                            info!(peer_id = %target_node_id, count = all_keys.len(), "Wyslano TrustedKeysSync");
                                        }
                                    }

                                    // Rozglosz zaktualizowana liste kluczy do WSZYSTKICH zaufanych peerow
                                    let updated_keys = sec.get_all_trusted_keys();
                                    if updated_keys.len() > 1 {
                                        let entries: Vec<
                                            tentaflow_protocol::mesh::TrustedKeyEntry,
                                        > = updated_keys
                                            .iter()
                                            .map(|(nid, pk)| {
                                                tentaflow_protocol::mesh::TrustedKeyEntry {
                                                    node_id: nid.clone(),
                                                    public_key_hex: pk.clone(),
                                                }
                                            })
                                            .collect();
                                        let payload =
                                            tentaflow_protocol::mesh::TrustedKeysSyncPayload {
                                                keys: entries,
                                            };
                                        let broadcast_data =
                                            rkyv::to_bytes::<rkyv::rancor::Error>(&payload)
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
                                                warn!(
                                                    "Blad broadcast TrustedKeysSync do {}: {}",
                                                    pid, e
                                                );
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
                Ok(IrohMeshEvent::PairingRejectReceived { peer_id, data }) => {
                    // Parsuj JSON i usun oczekujace parowanie
                    if let Some(ref sec) = mesh_security {
                        match serde_json::from_slice::<serde_json::Value>(&data) {
                            Ok(val) => {
                                let from_node_id = val["from_node_id"].as_str().unwrap_or(&peer_id);
                                if let Err(e) = sec.reject_pairing(from_node_id) {
                                    warn!("Blad odrzucenia parowania od {}: {}", peer_id, e);
                                } else {
                                    let _ = crate::net::iroh::pairing::delete_pending_contact_hints(
                                        &sec.db,
                                        from_node_id,
                                    );
                                    info!("Otrzymano PairingReject od {}", peer_id);
                                }
                            }
                            Err(e) => {
                                warn!(peer_id = %peer_id, "Blad parsowania PairingReject JSON: {}", e);
                            }
                        }
                    }
                }
                Ok(IrohMeshEvent::TrustRevokedReceived {
                    node_id,
                    revoked_node_id,
                }) => {
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
                                node_id,
                                all_trusted.len()
                            );

                            let details = format!(
                                "Odlaczony z mesh przez {} — {} kluczy usunietych",
                                node_id,
                                all_trusted.len()
                            );
                            let _ = crate::db::repository::log_audit(
                                &sec.db,
                                None,
                                None,
                                "removed_from_mesh",
                                None,
                                Some(&details),
                                None,
                                Some(&node_id),
                            );
                            continue;
                        }

                        // Przypadek 2: ktos inny zostal odlaczony — usun TYLKO jego klucz
                        if sender_trusted && sec.is_trusted(&revoked_node_id) {
                            let _ = sec.unpair(&revoked_node_id);
                            info!(
                                "Usunieto {} z mesh (propagacja od {})",
                                revoked_node_id, node_id
                            );

                            let _ = crate::db::repository::log_audit(
                                &sec.db,
                                None,
                                None,
                                "trust_revoked_propagation",
                                None,
                                Some(&format!(
                                    "Usunieto {} propagacja od {}",
                                    revoked_node_id, node_id
                                )),
                                None,
                                Some(&revoked_node_id),
                            );
                        } else if !sender_trusted && !i_am_revoked {
                            warn!("Odrzucono TrustRevoked od niezaufanego noda {}", node_id);
                        }
                    }
                }
                Ok(IrohMeshEvent::NodeLeavingReceived { node_id }) => {
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
                Ok(IrohMeshEvent::KeyRotationReceived { .. })
                | Ok(IrohMeshEvent::KeyRotationResponseReceived { .. }) => {
                    // Rotacja kluczy jest obsluzona przez iroh TLS per-connection —
                    // legacy zdarzenia od starych peerow sa ignorowane.
                }
                Ok(IrohMeshEvent::TrustedKeysSyncReceived { node_id, keys }) => {
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
                            let details =
                                format!("Dodano {} kluczy z TrustedKeysSync od {}", added, node_id);
                            let _ = crate::db::repository::log_audit(
                                &sec.db,
                                None,
                                None,
                                "trusted_keys_sync",
                                None,
                                Some(&details),
                                None,
                                Some(&node_id),
                            );
                        }
                    }
                }
                Ok(IrohMeshEvent::RelayFrameReceived {
                    from_node_id: _,
                    frame,
                }) => {
                    // Sprawdz TTL
                    if frame.ttl == 0 {
                        warn!(source = %frame.source_node_id, dest = %frame.destination_node_id, "Relay frame TTL wyczerpany — odrzucam");
                        continue;
                    }

                    // Czy ja jestem odbiorca koncowym?
                    if frame.destination_node_id == local_node_id {
                        // iroh TLS zapewnia end-to-end encryption na polaczeniu —
                        // payload jest juz odszyfrowany przy odbiorze streamu.
                        info!(
                            source = %frame.source_node_id,
                            disc = frame.discriminant,
                            hops = 5u8.saturating_sub(frame.ttl) + 1,
                            "Otrzymano relay frame (multi-hop)"
                        );
                    } else {
                        // Forward do next-hop
                        let mut forwarded_frame = frame;
                        forwarded_frame.ttl -= 1;

                        if let Some(route) =
                            peer_store.get_route(&forwarded_frame.destination_node_id)
                        {
                            let frame_bytes =
                                rkyv::to_bytes::<rkyv::rancor::Error>(&forwarded_frame)
                                    .map(|v| v.to_vec())
                                    .unwrap_or_default();
                            if let Err(e) = qm_events
                                .send_relay_frame(&route.next_hop, &frame_bytes)
                                .await
                            {
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
                Ok(IrohMeshEvent::MeshCommandReceived {
                    from_node_id,
                    command,
                }) => {
                    info!(from = %from_node_id, "Otrzymano MeshCommand — przekazuje do executora");
                    qm_events
                        .handle_command_received(&from_node_id, &command)
                        .await;
                }
                Ok(IrohMeshEvent::MeshCommandResponseReceived { from_node_id, data }) => {
                    qm_events
                        .handle_command_response_received(&from_node_id, &data)
                        .await;
                }
                Ok(IrohMeshEvent::CrdtDeltaReceived { node_id, .. }) => {
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
                Ok(IrohMeshEvent::FullStateReceived { node_id, .. }) => {
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
                Ok(IrohMeshEvent::ModelListUpdate { node_id, data }) => {
                    // ModelsSync — nadpisuje liste modeli danego peera.
                    // Format: rkyv-zakodowany `ModelsSync { models: Vec<PeerModelInfo> }`.
                    match rkyv::from_bytes::<crate::mesh::peer_store::ModelsSync, rkyv::rancor::Error>(
                        &data,
                    ) {
                        Ok(sync) => {
                            debug!(
                                node_id = %node_id,
                                models_count = sync.models.len(),
                                "ModelsSync odebrany"
                            );
                            peer_store.update_models(&node_id, sync.models);
                        }
                        Err(e) => {
                            warn!(node_id = %node_id, "Blad deserializacji ModelsSync: {}", e);
                        }
                    }
                }
                Ok(IrohMeshEvent::PeerDiscovered { node_id, addresses }) => {
                    // mDNS/DHT zobaczylo peera. Jesli peer juz polaczony, NodeInfo
                    // jest zrodlem prawdy — nie nadpisujemy. Inaczej dodaj do
                    // peer_store zeby UI pokazal go jako "discovered" (dashed
                    // pending card), nawet jesli dial jeszcze nie wypalil.
                    if node_id == local_node_id {
                        continue;
                    }
                    if peer_store.is_quic_connected(&node_id) {
                        continue;
                    }
                    let ips: Vec<std::net::IpAddr> = addresses.iter().map(|sa| sa.ip()).collect();
                    peer_store.set_addresses(&node_id, ips);
                    peer_store.set_status(&node_id, "discovered");
                    debug!(peer = %node_id, count = addresses.len(), "PeerDiscovered → peer_store");
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
    let docker_cache: Arc<tokio::sync::RwLock<Vec<crate::mesh::peer_store::PeerContainerInfo>>> =
        Arc::new(tokio::sync::RwLock::new(vec![]));

    let dc = docker_cache.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            let containers =
                tokio::task::spawn_blocking(|| node_info_collector::collect_docker_containers())
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
    quic_mesh: Arc<IrohMeshManager>,
    peer_store: MeshPeerStore,
    local_node_id: String,
    docker_cache: Arc<tokio::sync::RwLock<Vec<crate::mesh::peer_store::PeerContainerInfo>>>,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        let mut heartbeat_count: u64 = 0;
        loop {
            interval.tick().await;
            let metrics =
                tokio::task::spawn_blocking(|| node_info_collector::collect_fast_metrics()).await;
            if let Ok(m) = metrics {
                let containers = docker_cache.read().await.clone();
                let connected_peers = quic_mesh.connected_peer_ids().await;

                // [OPT] Buduj HeartbeatMetrics najpierw, potem aktualizuj store
                // z referencji — unika podwojnego klonowania gpus/containers/networks
                // Snapshot licznikow routingu — uzywane do wyswietlenia
                // "aktywne" i tok/s w Mesh UI per-node.
                let (active_requests, tokens_per_sec) = routing_metrics_snapshot();

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
                    active_requests,
                    tokens_per_sec,
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
                    hb.active_requests,
                    hb.tokens_per_sec,
                );

                // Aktualizuj topologie lokalnego noda
                peer_store.update_topology(&local_node_id, connected_peers.clone());

                // Serializuj RAZ — broadcast do wszystkich peerow uzywa tych samych bajtow
                if let Ok(data) = rkyv::to_bytes::<rkyv::rancor::Error>(&hb) {
                    quic_mesh.send_heartbeat_data(&data).await;
                }

                // Przelicz routing co 10 heartbeatow (~5s)
                heartbeat_count += 1;
                if heartbeat_count % 10 == 0 {
                    peer_store.recalculate_routes(&local_node_id);
                }

                // ModelsSync broadcast co 60 heartbeatow (~30s). Serwer-side
                // scrape z service_registry zwraca aktualne aliasy + stan zaladowania.
                if heartbeat_count % 60 == 0 {
                    let models = collect_local_models(&quic_mesh);
                    peer_store.update_models(&local_node_id, models.clone());
                    let sync = crate::mesh::peer_store::ModelsSync { models };
                    if let Ok(data) = rkyv::to_bytes::<rkyv::rancor::Error>(&sync) {
                        quic_mesh.send_models_sync_data(&data).await;
                    }
                }

                // TopologyAnnounce — gossip co 60 heartbeatow (~30s).
                // Kazdy node anonsuje SIEBIE: hostname + platform + bezposredni sasiedzi
                // + modele + uslugi. Flooding z dedupem (origin, epoch) dociera az do 5 hopow.
                if heartbeat_count % 60 == 30 {
                    let services: Vec<tentaflow_protocol::mesh::ServiceSummary> = quic_mesh
                        .service_registry()
                        .local_services()
                        .into_iter()
                        .map(|s| tentaflow_protocol::mesh::ServiceSummary {
                            name: s.service_name,
                            service_type: s.service_type,
                            ready: matches!(s.status.as_str(), "running" | "ready"),
                        })
                        .collect();
                    let models_summary: Vec<tentaflow_protocol::mesh::ModelSummary> =
                        collect_local_models(&quic_mesh)
                            .into_iter()
                            .map(|m| tentaflow_protocol::mesh::ModelSummary {
                                alias: m.alias,
                                backend: m.backend,
                                loaded: m.loaded,
                            })
                            .collect();
                    let self_info = peer_store.get(&local_node_id);
                    let hostname = self_info
                        .as_ref()
                        .map(|p| p.hostname.clone())
                        .unwrap_or_default();
                    let platform = node_info_collector::detect_platform();
                    let os_info = self_info
                        .as_ref()
                        .map(|p| p.os_info.clone())
                        .unwrap_or_default();
                    let port = self_info.as_ref().map(|p| p.port).unwrap_or(0);
                    let direct_addrs: Vec<String> = self_info
                        .as_ref()
                        .map(|p| {
                            p.addresses
                                .iter()
                                .map(|ip| format!("{}:{}", ip, port))
                                .collect()
                        })
                        .unwrap_or_default();
                    let entry = tentaflow_protocol::mesh::TopologyEntry {
                        node_id: local_node_id.clone(),
                        hostname,
                        platform,
                        os_info,
                        connected_to: connected_peers.clone(),
                        services,
                        models: models_summary,
                        direct_addrs,
                        port,
                    };
                    let epoch = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(heartbeat_count);
                    let payload = tentaflow_protocol::mesh::TopologyAnnouncePayload {
                        origin_node_id: local_node_id.clone(),
                        epoch,
                        ttl: 5,
                        entries: vec![entry],
                    };
                    if let Ok(bytes) = rkyv::to_bytes::<rkyv::rancor::Error>(&payload) {
                        let bv = bytes.to_vec();
                        for peer_id in &connected_peers {
                            if let Err(e) = quic_mesh.send_topology_announce(peer_id, &bv).await {
                                debug!(peer = %peer_id, "Blad wysylania TopologyAnnounce: {}", e);
                            }
                        }
                    }
                }
            }
        }
    });
}

/// Buduje liste `PeerModelInfo` z lokalnego service_registry. Tylko LOKALNE
/// serwisy (te na biezacym nodzie) — modele z peerow przychodza przez
/// ModelsSync od ich wlascicieli.
fn collect_local_models(
    quic_mesh: &Arc<IrohMeshManager>,
) -> Vec<crate::mesh::peer_store::PeerModelInfo> {
    let registry = quic_mesh.service_registry();
    registry
        .local_services()
        .into_iter()
        .flat_map(|svc| {
            let kind = svc.service_type.clone();
            let backend = svc.engine_id.clone().unwrap_or_default();
            let sizes = svc.model_sizes_mb.clone();
            let loaded = matches!(svc.status.as_str(), "running" | "ready");
            svc.models.into_iter().enumerate().map(move |(idx, alias)| {
                crate::mesh::peer_store::PeerModelInfo {
                    alias,
                    kind: kind.clone(),
                    backend: backend.clone(),
                    size_mb: sizes.get(idx).copied().unwrap_or(0),
                    loaded,
                }
            })
        })
        .collect()
}

/// Slow refresh — co 60s odswiezaj wolno-zmienne dane lokalnego noda:
/// adresy IP, Docker availability/version, OS distro.
fn spawn_slow_refresh(peer_store: MeshPeerStore, local_node_id: String) {
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

/// Liveness timer — co 1s sprawdza ile czasu minelo od ostatniego heartbeatu
/// dla kazdego trusted peera. Heartbeaty leca co 500ms, wiec:
///  - age < 2000ms  → OK, nic nie rob
///  - 2000..5000ms  → status='degraded', emit event (jesli byl 'connected')
///  - age > 5000ms  → status='offline', force disconnect, clear heartbeat,
///                     emit event 'offline' (auto-reconnect loop sie zaopiekuje).
fn spawn_liveness_timer(
    peer_store: MeshPeerStore,
    quic_mesh: Arc<IrohMeshManager>,
    local_node_id: String,
    mesh_security: Option<Arc<MeshSecurity>>,
) {
    tokio::spawn(async move {
        // Progi dobrane z zapasem — iroh multi-path czasem opuszcza heartbeat
        // podczas reroutingu. Liveness timer dziala TYLKO na zaufanych peerach
        // (dla nich gwarantujemy stabilny heartbeat co 500ms). Dla discovered/
        // unpaired iroh sam zarzadza zyciem polaczenia — nie wtracamy sie.
        const DEGRADED_MS: i64 = 10_000;
        const OFFLINE_MS: i64 = 30_000;
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            let ages = peer_store.heartbeat_ages();
            for (node_id, age_ms) in ages {
                if node_id == local_node_id {
                    continue;
                }
                // Tylko trusted peery — niezaufane moga sie laczyc/rozlaczac
                // po uznaniu iroh (multi-path, NAT rebinding, relay rotacja).
                let is_trusted = match &mesh_security {
                    Some(sec) => sec.is_trusted(&node_id),
                    None => false,
                };
                if !is_trusted {
                    continue;
                }
                let peer = match peer_store.get(&node_id) {
                    Some(p) => p,
                    None => continue,
                };
                if !peer.quic_connected {
                    continue;
                }
                if age_ms > OFFLINE_MS {
                    warn!(
                        peer = %node_id,
                        age_ms,
                        "Liveness timer: trusted peer nieaktywny > 30s — force disconnect"
                    );
                    peer_store.set_quic_connected(&node_id, false);
                    peer_store.set_status(&node_id, "offline");
                    peer_store.clear_heartbeat(&node_id);
                    crate::dispatch::system_event_broadcast::publish_mesh_peer_status(
                        &node_id,
                        &peer.hostname,
                        "offline",
                        "heartbeat timeout",
                    );
                    let qm = quic_mesh.clone();
                    let nid = node_id.clone();
                    tokio::spawn(async move {
                        qm.disconnect_peer(&nid).await;
                    });
                } else if age_ms > DEGRADED_MS && peer.status != "degraded" {
                    peer_store.set_status(&node_id, "degraded");
                    crate::dispatch::system_event_broadcast::publish_mesh_peer_status(
                        &node_id,
                        &peer.hostname,
                        "degraded",
                        "slow heartbeats",
                    );
                }
            }
        }
    });
}

/// Periodyczna rotacja kluczy szyfrowania — co 24h
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
