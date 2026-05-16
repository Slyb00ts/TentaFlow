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
use crate::mesh::relay_health::{spawn_relay_health_monitor, RelayHealth};
use crate::mesh::security::MeshSecurity;
use crate::net::iroh::load_relay_url;
use crate::net::iroh::pairing::{
    load_trusted_contact_hints, merge_contact_hints, store_trusted_contact_hints,
    PairingContactHints,
};
use crate::routing::live_metrics;
use parking_lot::RwLock as PlRwLock;
use tokio_util::sync::CancellationToken;

/// Snapshot live-metrics routera — zwracany do heartbeat.
fn routing_metrics_snapshot() -> (u32, f32) {
    live_metrics::snapshot()
}

fn local_mesh_addresses(peer_store: &MeshPeerStore, local_node_id: &str) -> Vec<std::net::IpAddr> {
    peer_store
        .get(local_node_id)
        .map(|p| p.addresses)
        .unwrap_or_default()
}

fn is_self_discovery_ip_set(
    peer_store: &MeshPeerStore,
    local_node_id: &str,
    addrs: &[std::net::IpAddr],
) -> bool {
    let local_addrs = local_mesh_addresses(peer_store, local_node_id);
    !addrs.is_empty()
        && !local_addrs.is_empty()
        && addrs
            .iter()
            .all(|addr| local_addrs.iter().any(|local| local == addr))
}

fn is_self_discovery_socket_set(
    peer_store: &MeshPeerStore,
    local_node_id: &str,
    addrs: &[std::net::SocketAddr],
) -> bool {
    let ips: Vec<std::net::IpAddr> = addrs.iter().map(|addr| addr.ip()).collect();
    is_self_discovery_ip_set(peer_store, local_node_id, &ips)
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
    /// Snapshot zdrowia relay (URL, RTT, status, faktyczny bind addr) odswiezany
    /// w tle co 30s. Wstrzykiwany do `AppState.mesh_relay_health` zeby handler
    /// `NetworkRelayStatusRequest` mogl czytac stan bez dodatkowego I/O.
    pub relay_health: Arc<PlRwLock<RelayHealth>>,
    /// Cancellation token dla zadan w tle uruchomionych w pipeline (m.in.
    /// monitor relay). Trzymany zeby `shutdown()` mogl czysto zatrzymac petle.
    pub background_shutdown: CancellationToken,
}

impl MeshPipelineHandles {
    /// Graceful shutdown — zamyka iroh endpoint i wszystkie polaczenia.
    pub async fn shutdown(mut self) {
        self.background_shutdown.cancel();
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
    mesh_services_registry: Arc<crate::services::mesh_registry::MeshServicesRegistry>,
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
    // + iroh relay wystarczaja do discovery peerow. Na desktop respektujemy
    // `mesh.dht_enabled` z config.toml (default true) — uzytkownicy z ISP
    // blokujacym BitTorrent UDP moga wylaczyc i nie zalewac logow timeout-ami.
    let enable_dht = cfg!(not(any(target_os = "ios", target_os = "android")))
        && mesh_config.dht_enabled;
    let relay_url = load_relay_url(db_pool.as_ref(), Some(mesh_config));

    // Wyczysc stare wpisy `trusted_contact:*` z martwym relay URL zanim
    // IrohMeshManager zacznie reconnect — inaczej dial idzie na DNS NXDOMAIN.
    if let Some(ref db) = db_pool {
        match crate::net::iroh::pairing::sanitize_trusted_contacts(db) {
            Ok(n) if n > 0 => info!(
                cleaned = n,
                "sanitize_trusted_contacts: wyczyszczono stare wpisy"
            ),
            Ok(_) => debug!("sanitize_trusted_contacts: nic do czyszczenia"),
            Err(e) => warn!(error = %e, "sanitize_trusted_contacts: nieudany"),
        }
    }

    // Bind address: domyslnie `0.0.0.0:port` (mode=auto). Gdy user wybral
    // `custom` i wpisal istniejace IPv4 hosta — iroh bindne sie tylko na ten
    // jeden interfejs. Fallback do 0.0.0.0 z warnem gdy custom IP znikloby z
    // systemu (np. VPN wylaczony po restartcie).
    let bind_addr = match &db_pool {
        Some(db) => crate::mesh::network_interfaces::resolve_bind_addr(db, mesh_port),
        None => std::net::SocketAddr::from(([0u8, 0, 0, 0], mesh_port)),
    };
    tracing::info!(
        bind_addr = %bind_addr,
        relay = ?relay_url.as_ref().map(|r| r.to_string()),
        "mesh init: resolved bind + relay (z ustawien GUI / config.toml)"
    );

    // Klon URL relay zachowujemy zeby spawn_relay_health_monitor mogl pingowac
    // ten sam endpoint co iroh — `IrohMeshConfig` zjada `relay_url` mov'em.
    let relay_url_for_health = relay_url.clone();
    let bind_addr_actual = bind_addr.to_string();
    let relay_health = Arc::new(PlRwLock::new(RelayHealth::initial_pending(
        relay_url_for_health
            .as_ref()
            .map(|u| u.to_string())
            .unwrap_or_default(),
        bind_addr_actual.clone(),
    )));
    let background_shutdown = CancellationToken::new();
    spawn_relay_health_monitor(
        relay_url_for_health,
        bind_addr_actual,
        relay_health.clone(),
        background_shutdown.clone(),
    );

    let iroh_cfg = IrohMeshConfig {
        node_id: app_node_id.clone(),
        bind_addr,
        relay_url,
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
                db_pool.as_ref(),
            );

            // Wstrzykujemy executor PRZED uruchomieniem accept loopa, zeby
            // pierwsza komenda od peera zastala go juz wpietego. Bez tego okno
            // pomiedzy `start()` a `set_command_executor` powodowalo by zwroty
            // "command executor not configured" przy szybkim reconnectcie.
            {
                let executor = Arc::new(crate::mesh::command_executor::MeshCommandExecutor::new(
                    mesh_security.clone(),
                    local_node_id.clone(),
                    crate::paths::tentaflow_home().to_path_buf(),
                ));
                quic_mesh.set_command_executor(executor).await;
            }

            {
                let qm = quic_mesh.clone();
                tokio::spawn(async move {
                    qm.start();
                });
            }

            // Reconnect do trusted peerow po EndpointId — iroh sam rozwiazuje adres.
            {
                let sec = mesh_security.clone();
                if let Ok(trusted) = crate::db::repository::list_trusted_nodes(&mesh_security.db) {
                    for node in &trusted {
                        let qm = quic_mesh.clone();
                        let nid = node.node_id.clone();
                        let sec = sec.clone();
                        tokio::spawn(async move {
                            if let Some(hints) = trusted_contact_hints_for_peer(sec.as_ref(), &nid)
                            {
                                if let Err(e) = qm.connect_to_peer_with_hints(&hints).await {
                                    debug!(peer_id = %nid, "Reconnect via trusted hints: {}", e);
                                }
                            } else {
                                let dummy_addr = std::net::SocketAddr::from(([0, 0, 0, 0], 0));
                                if let Err(e) = qm.connect_to_peer(&nid, dummy_addr).await {
                                    debug!(peer_id = %nid, "Reconnect via iroh: {}", e);
                                }
                            }
                        });
                    }
                }
            }

            // PR4: reconnect is now event-driven via ReconnectManager; the
            // legacy 15s polling loop has been removed. ReconnectManager
            // subscribes to PeerDelta events and schedules dials with
            // exponential backoff + jitter against the registry timeline.
            if let Some(registry) = mesh_peer_store.registry().cloned() {
                let mgr = crate::mesh::reconnect::ReconnectManager::new(
                    registry,
                    quic_mesh.clone(),
                    local_node_id.clone(),
                );
                mgr.spawn();
            }

            // PR4: liveness scanning runs as a dedicated task that walks
            // the registry and emits LivenessTick triggers. The state
            // machine in peer_registry::state owns the actual transitions.
            if let Some(registry) = mesh_peer_store.registry().cloned() {
                let task = crate::mesh::liveness::LivenessTask::new(registry);
                task.spawn();
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
                mesh_services_registry.clone(),
            );

            let docker_cache = spawn_docker_cache();
            spawn_heartbeat_sender(
                quic_mesh.clone(),
                mesh_peer_store.clone(),
                local_node_id.clone(),
                docker_cache,
                db_pool.clone(),
                mesh_services_registry.clone(),
            );
            spawn_slow_refresh(
                mesh_peer_store.clone(),
                local_node_id.clone(),
                db_pool.clone(),
            );
            spawn_pairing_cleanup(mesh_security.clone());

            info!("Mesh networking uruchomiony (iroh transport)");

            Ok(MeshPipelineHandles {
                mdns: None,
                quic_mesh: Some(quic_mesh),
                security: Some(mesh_security),
                relay_health,
                background_shutdown,
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
                db_pool.as_ref(),
            );
            Ok(MeshPipelineHandles {
                mdns: None,
                quic_mesh: None,
                security: Some(mesh_security),
                relay_health,
                background_shutdown,
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
    db_pool: Option<&crate::db::DbPool>,
) {
    let raw_addresses = node_info_collector::collect_local_addresses();
    // IPv4 only + user-defined hide_* filtry. Bez DB (test/embed) przepuszczamy
    // IPv4 wszystkie, IPv6 ucinamy zawsze — mesh nie obsluguje v6.
    let local_addresses = match db_pool {
        Some(db) => {
            let filters = crate::mesh::network_interfaces::load_advertise_filters(db);
            let kind_map = crate::mesh::network_interfaces::ipv4_kind_map();
            crate::mesh::network_interfaces::filter_advertise_ips(
                &raw_addresses,
                &filters,
                &kind_map,
            )
        }
        None => raw_addresses
            .into_iter()
            .filter(|ip| ip.is_ipv4())
            .collect(),
    };
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
        nsys_available: false,
        nsys_version: String::new(),
        profiling_collectors_available: Vec::new(),
    });
}

fn trusted_contact_hints_for_peer(
    security: &MeshSecurity,
    node_id: &str,
) -> Option<PairingContactHints> {
    load_trusted_contact_hints(&security.db, node_id)
        .ok()
        .flatten()
}

fn prefer_address_first(addresses: &mut Vec<String>, preferred: Option<&str>) {
    let Some(preferred) = preferred else {
        return;
    };
    let Some(index) = addresses.iter().position(|addr| addr == preferred) else {
        return;
    };
    if index > 0 {
        let preferred = addresses.remove(index);
        addresses.insert(0, preferred);
    }
}

/// [SCALE] Handler PeerConnected wywolywany w tokio::spawn z per-peer lockiem
/// w event loopie mesh. Debounce 150ms + send Hello/KnownPeers/NodeInfo +
/// TrustedKeysSync. 100 peerow na raz daje ~150ms total zamiast 100*150ms
/// sekwencyjnie.
async fn handle_peer_connected(
    node_id: String,
    peer_store: MeshPeerStore,
    qm_events: Arc<IrohMeshManager>,
    local_node_info: NodeInfo,
    local_node_id: String,
    mesh_security: Option<Arc<MeshSecurity>>,
    last_sync_sent: Arc<dashmap::DashMap<String, std::time::Instant>>,
    sync_cooldown_secs: u64,
) {
    // Tie-break potrzebuje czasu zeby sie ustabilizowac. 150ms debounce.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    if !qm_events.is_connected(&node_id).await {
        debug!(
            peer_id = %node_id,
            "QUIC peer — PeerConnected zniwelowane przez tie-break w ciagu 150ms"
        );
        peer_store.set_quic_connected(&node_id, false);
        peer_store.set_status(&node_id, "disconnected");
        return;
    }
    info!(peer_id = %node_id, "QUIC peer polaczony");

    // Cache is_trusted raz — unikamy 3x DashMap lookup w dalszej czesci handlera.
    let is_trusted = match &mesh_security {
        Some(sec) => sec.is_trusted(&node_id),
        None => false,
    };

    // Emit event do GUI — toast "peer connected" + refresh mesh view.
    let hostname_ev = peer_store.get_hostname(&node_id).unwrap_or_default();
    crate::dispatch::system_event_broadcast::publish_mesh_peer_status(
        &node_id,
        &hostname_ev,
        "online",
        "",
    );

    // Wyslij minimalne Hello (hostname + platform) niezaleznie od trust.
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

    // KnownPeers — pozwala peerowi polaczyc sie z sasiadami bez mDNS.
    // known_peers_snapshot omija klonowanie Vec<MeshPeerInfo> — single-pass po DashMap,
    // wyciagamy 4 pola zamiast ~20. Przy 1000 peerow ~95% mniej alokacji.
    let known = peer_store.known_peers_snapshot(&node_id, &local_node_id);
    if !known.is_empty() {
        let payload = tentaflow_protocol::mesh::KnownPeersPayload { peers: known };
        if let Ok(kp_bytes) = rkyv::to_bytes::<rkyv::rancor::Error>(&payload) {
            if let Err(e) = qm_events.send_known_peers(&node_id, &kp_bytes).await {
                debug!("Blad wysylania KnownPeers do {}: {}", node_id, e);
            }
        }
    }

    // NodeInfo + TrustedKeysSync — TYLKO do zaufanych (is_trusted scache'owany powyzej).
    if is_trusted {
        if let Ok(info_bytes) = rkyv::to_bytes::<rkyv::rancor::Error>(&local_node_info) {
            if let Err(e) = qm_events.send_node_info(&node_id, &info_bytes).await {
                warn!("Blad wysylania NodeInfo do {}: {}", node_id, e);
            }
        }

        if let Some(ref sec) = mesh_security {
            let should_sync = last_sync_sent.get(&node_id).map_or(true, |t| {
                t.elapsed() >= std::time::Duration::from_secs(sync_cooldown_secs)
            });

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
                    let payload =
                        tentaflow_protocol::mesh::TrustedKeysSyncPayload { keys: entries };
                    if let Ok(sync_data) =
                        rkyv::to_bytes::<rkyv::rancor::Error>(&payload).map(|v| v.to_vec())
                    {
                        if let Err(e) = qm_events.send_trusted_keys_sync(&node_id, &sync_data).await
                        {
                            warn!("Blad wysylania TrustedKeysSync do {}: {}", node_id, e);
                        }
                    }
                }

                // Revoked node sync.
                let revoked = sec.get_revoked_node_ids();
                for revoked_id in &revoked {
                    let payload = tentaflow_protocol::mesh::TrustRevokedPayload {
                        revoked_node_id: revoked_id.clone(),
                        from_node_id: local_node_id.clone(),
                    };
                    if let Ok(data) =
                        rkyv::to_bytes::<rkyv::rancor::Error>(&payload).map(|v| v.to_vec())
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

            // F1b P3.B — push our HMAC issuer keys (pickup_token, frame_url,
            // recording_url) so the peer can verify tokens we mint. Only sent
            // to already-trusted peers; the receiver enforces the same gate.
            let advertise =
                crate::services::mesh_keys::sync::build_local_advertise(&local_node_id);
            if let Some(bytes) = crate::services::mesh_keys::sync::encode_advertise(&advertise)
            {
                if let Err(e) = qm_events.send_hmac_keys_sync(&node_id, &bytes).await {
                    warn!("Blad wysylania HmacKeysSync do {}: {}", node_id, e);
                }
            }
        }

        // Pull-on-connect: poprosic peera o pelny snapshot jego serwisow.
        // Wynik trafia do `MeshServicesRegistry` w handlerze
        // `ServicesGetResponseReceived`. Wysylamy tylko dla zaufanych — peer
        // i tak odrzuci request od niezaufanego (defense in depth).
        let pull = tentaflow_protocol::mesh::MeshServicesGetPayload {
            from_node_id: local_node_id.clone(),
        };
        if let Ok(bytes) = rkyv::to_bytes::<rkyv::rancor::Error>(&pull) {
            if let Err(e) = qm_events
                .send_to_peer(
                    &node_id,
                    tentaflow_protocol::mesh::MESH_MSG_SERVICES_GET,
                    &bytes,
                )
                .await
            {
                debug!(peer = %node_id, "MeshServicesGet send failed: {}", e);
            }
        }
    } else {
        debug!(peer_id = %node_id, "Peer niezaufany — pomijam wysylanie NodeInfo");
    }

    // Persist adresy trusted peera do DB (is_trusted scache'owany na poczatku handlera).
    if is_trusted {
        if let Some(ref sec) = mesh_security {
            if let Some((hostname, addresses, port)) = peer_store.contact_snapshot(&node_id) {
                if !addresses.is_empty() && port > 0 {
                    // Filtr IPv4 + advertise rules: do trusted_contact:* wrzucamy
                    // tylko to co user pozwolil widziec zdalnie (hide_docker/
                    // hide_cgnat itp.). Bez filtra peerzy dostawali np. adresy
                    // docker bridge, ktore sa nieosiagalne z zewnatrz hosta.
                    let filters = crate::mesh::network_interfaces::load_advertise_filters(&sec.db);
                    let kind_map = crate::mesh::network_interfaces::ipv4_kind_map();
                    let filtered_ips = crate::mesh::network_interfaces::filter_advertise_ips(
                        &addresses, &filters, &kind_map,
                    );
                    if filtered_ips.is_empty() {
                        debug!(
                            peer_id = %node_id,
                            "contact_snapshot: wszystkie adresy odrzucone przez advertise filters — pomijam persist"
                        );
                    } else {
                        let mut direct_addresses: Vec<String> = filtered_ips
                            .iter()
                            .map(|ip| format!("{}:{}", ip, port))
                            .collect();
                        let snapshot = qm_events.connection_snapshot(&node_id);
                        let selected_address = snapshot.as_ref().and_then(|c| c.address.as_deref());
                        let selected_is_direct = snapshot
                            .as_ref()
                            .map(|c| c.transport.as_str() == "p2p")
                            .unwrap_or(false);
                        if selected_is_direct {
                            prefer_address_first(&mut direct_addresses, selected_address);
                        }
                        // Gdy user wlaczyl prefer_same_subnet, po filtrze przestawiamy
                        // adres z tej samej /24 co peer na poczatek listy.
                        if crate::mesh::network_interfaces::load_prefer_same_subnet(&sec.db) {
                            crate::mesh::network_interfaces::sort_prefer_same_subnet(
                                &mut direct_addresses,
                                selected_address,
                            );
                        }
                        let addr_str = direct_addresses.join(",");
                        tracing::info!(
                            peer = %node_id,
                            raw_count = addresses.len(),
                            filtered_count = direct_addresses.len(),
                            advertised = %addr_str,
                            "advertise to peer: addresses po filtrach"
                        );
                        let _ = crate::db::repository::update_trusted_node_addresses(
                            &sec.db, &node_id, &addr_str,
                        );
                        let relay_url = snapshot
                            .as_ref()
                            .and_then(|c| c.relay_url.clone())
                            .or_else(|| qm_events.relay_url().map(|url| url.to_string()))
                            .unwrap_or_default();
                        let current = load_trusted_contact_hints(&sec.db, &node_id).ok().flatten();
                        let hints = merge_contact_hints(
                            current,
                            PairingContactHints {
                                node_id: node_id.clone(),
                                public_key_hex: String::new(),
                                hostname,
                                addresses: direct_addresses,
                                relay_url,
                            },
                        );
                        let _ = store_trusted_contact_hints(&sec.db, &node_id, &hints);
                    }
                }
            }
        }
    }

    // Znacz routing do przeliczenia — heartbeat tick (co ~5s) zrobi BFS.
    peer_store.mark_routes_dirty();
}

/// [SCALE] Handler PeerDisconnected wywolywany w tokio::spawn z per-peer
/// lockiem. Debounce 150ms + emit event + auto-reconnect dla trusted.
async fn handle_peer_disconnected(
    node_id: String,
    peer_store: MeshPeerStore,
    qm_events: Arc<IrohMeshManager>,
    mesh_security: Option<Arc<MeshSecurity>>,
    last_sync_sent: Arc<dashmap::DashMap<String, std::time::Instant>>,
) {
    // Po disconnect czyscimy cooldown — przy reconnecie od razu zsynchronizujemy klucze.
    last_sync_sent.remove(&node_id);
    // Debounce: tie-break swap moze podstawic inna sciezke w <150ms.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    if qm_events.is_connected(&node_id).await {
        debug!(
            peer_id = %node_id,
            "QUIC peer — PeerDisconnected zniwelowane przez natychmiastowy reconnect (tie-break swap)"
        );
        return;
    }
    peer_store.set_quic_connected(&node_id, false);
    peer_store.set_status(&node_id, "disconnected");
    peer_store.clear_heartbeat(&node_id);
    info!(peer_id = %node_id, "QUIC peer rozlaczony");

    // F1b P3.B — disconnected peer's HMAC keys are no longer trustworthy
    // for verifying new tokens; drop them from the pool. They will be
    // re-acquired on the next reconnect's advertise.
    crate::services::mesh_keys::sync::forget_peer(&node_id);

    let hostname = peer_store.get_hostname(&node_id).unwrap_or_default();
    crate::dispatch::system_event_broadcast::publish_mesh_peer_status(
        &node_id,
        &hostname,
        "offline",
        "QUIC disconnect",
    );

    peer_store.mark_routes_dirty();

    // Auto-reconnect dla trusted peerow.
    let should_reconnect = match &mesh_security {
        Some(sec) => sec.is_trusted(&node_id),
        None => false,
    };
    if should_reconnect {
        if let Some(ref sec) = mesh_security {
            let qm2 = qm_events.clone();
            let node_id2 = node_id.clone();
            let hints = trusted_contact_hints_for_peer(sec.as_ref(), &node_id);
            tokio::spawn(async move {
                if let Some(hints) = hints {
                    if let Err(e) = qm2.connect_to_peer_with_hints(&hints).await {
                        debug!(
                            peer_id = %node_id2,
                            "Reconnect after disconnect via trusted hints: {}",
                            e
                        );
                    }
                } else {
                    let dummy = std::net::SocketAddr::from(([0, 0, 0, 0], 0));
                    if let Err(e) = qm2.connect_to_peer(&node_id2, dummy).await {
                        debug!(peer_id = %node_id2, "Reconnect after disconnect: {}", e);
                    }
                }
            });
        }
    }
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
    mesh_services_registry: Arc<crate::services::mesh_registry::MeshServicesRegistry>,
) {
    let qm_events = quic_mesh.clone();
    let mut event_rx = quic_mesh.subscribe();

    // [SCALE] last_sync_sent wspoldzielony Arc<DashMap> — debouncowany
    // handler PeerConnected wyrzucony do tokio::spawn potrzebuje dostepu
    // z roznych taskow.
    let last_sync_sent: Arc<dashmap::DashMap<String, std::time::Instant>> =
        Arc::new(dashmap::DashMap::new());
    // Per-peer lock dla serializacji PeerConnected/Disconnected eventow
    // dla TEGO SAMEGO peera. Miedzy roznymi peerami zero kontencji.
    let peer_event_locks: Arc<dashmap::DashMap<String, Arc<tokio::sync::Mutex<()>>>> =
        Arc::new(dashmap::DashMap::new());

    // [SCALE] GC task: co 60s sprzata mapy od entries dla peerow ktorzy znikli.
    // Bez tego mapa rosnie monotonicznie przez caly czas uptime (1 entry per
    // unikalny node_id jaki kiedykolwiek widzielismy).
    {
        let locks_gc = peer_event_locks.clone();
        let sync_gc = last_sync_sent.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
            tick.tick().await; // pierwszy tick natychmiast — pomin
            loop {
                tick.tick().await;
                // Lock entry usuwamy gdy tylko mapa go trzyma (zaden handler
                // nie ma clone'a Arc) — strong_count == 1. Jesli wciaz ktos
                // pracuje, zostaje.
                locks_gc.retain(|_, arc| Arc::strong_count(arc) > 1);
                // last_sync_sent: wyrzuc wpisy starsze niz 10min — po takiej
                // ciszy peer i tak potrzebuje ponownego full-sync.
                let cutoff = std::time::Duration::from_secs(600);
                sync_gc.retain(|_, t| t.elapsed() < cutoff);
            }
        });
    }

    tokio::spawn(async move {
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
                        let target_trusted = match &mesh_security {
                            Some(sec) => sec.is_trusted(&entry.node_id),
                            None => false,
                        };

                        let addrs: Vec<std::net::IpAddr> = entry
                            .direct_addrs
                            .iter()
                            .filter_map(|s| s.parse::<std::net::SocketAddr>().ok())
                            .map(|sa| sa.ip())
                            .collect();
                        if is_self_discovery_ip_set(&peer_store, &local_node_id, &addrs) {
                            debug!(
                                peer = %entry.node_id,
                                addrs = ?addrs,
                                "Pomijam KnownPeers self-discovery po lokalnych adresach"
                            );
                            peer_store.remove(&entry.node_id);
                            continue;
                        }
                        if !addrs.is_empty() {
                            peer_store.set_addresses(&entry.node_id, addrs);
                        }
                        if !entry.hostname.is_empty() {
                            peer_store.set_hostname(&entry.node_id, &entry.hostname);
                        }
                        peer_store.set_status(&entry.node_id, "discovered");
                        if !target_trusted {
                            continue;
                        }
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
                        let hints = match &mesh_security {
                            Some(sec) => merge_contact_hints(
                                load_trusted_contact_hints(&sec.db, &entry.node_id)
                                    .ok()
                                    .flatten(),
                                PairingContactHints {
                                    node_id: entry.node_id.clone(),
                                    public_key_hex: String::new(),
                                    hostname: entry.hostname.clone(),
                                    addresses: entry.direct_addrs.clone(),
                                    relay_url: String::new(),
                                },
                            ),
                            None => continue,
                        };

                        let target = entry.node_id.clone();
                        let qm = qm_events.clone();
                        tokio::spawn(async move {
                            match qm.connect_to_peer_with_hints(&hints).await {
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

                    // Batch DB upsertow: cala TopologyAnnounce w jednej transakcji
                    // zamiast N osobnych COMMITow (N*fsync pod gossip burstem).
                    // Trzymamy tylko owned Stringi dla pol ktore sa SERIALIZOWANE
                    // (services_json, models_json); pozostale pola UpsertEntry
                    // borrow'uja bezposrednio z payload.entries — brak klonowania
                    // node_id/hostname/platform/os_info/connected_to/direct_addrs.
                    type TopoRow = (usize, String, String); // (entry_idx, services_json, models_json)
                    let mut topo_batch: Vec<TopoRow> = Vec::new();
                    let batch_now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);

                    // Aktualizuj peer_store + topologie dla kazdego wpisu
                    for (entry_idx, entry) in payload.entries.iter().enumerate() {
                        if entry.node_id == local_node_id {
                            continue;
                        }
                        let addrs: Vec<std::net::IpAddr> = entry
                            .direct_addrs
                            .iter()
                            .filter_map(|s| s.parse::<std::net::SocketAddr>().ok())
                            .map(|sa| sa.ip())
                            .collect();
                        if is_self_discovery_ip_set(&peer_store, &local_node_id, &addrs) {
                            debug!(
                                peer = %entry.node_id,
                                addrs = ?addrs,
                                "Pomijam TopologyAnnounce self-discovery po lokalnych adresach"
                            );
                            peer_store.remove(&entry.node_id);
                            continue;
                        }
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
                        // Cross-node service inventory now flows over the V2
                        // `MeshServicesAnnounce/Update` protocol (discriminants
                        // 0x40-0x43) into `mesh_services_registry`. The legacy
                        // `service_registry().update_remote` path is gone.
                        // Persystuj snapshot do DB — bootstrap po restarcie.
                        // Serializujemy bezposrednio Vec<ServiceSummary>/<ModelSummary>
                        // (derive SerdeSerialize) — omija intermediate serde_json::Value tree.
                        if db_pool.is_some() {
                            let services_json = serde_json::to_string(&entry.services)
                                .unwrap_or_else(|_| "[]".to_string());
                            let models_json = serde_json::to_string(&entry.models)
                                .unwrap_or_else(|_| "[]".to_string());
                            topo_batch.push((entry_idx, services_json, models_json));
                        }
                    }
                    if let Some(ref pool) = db_pool {
                        if !topo_batch.is_empty() {
                            let entries: Vec<crate::db::repository::mesh_topology::UpsertEntry> =
                                topo_batch
                                    .iter()
                                    .map(|(idx, sj, mj)| {
                                        let e = &payload.entries[*idx];
                                        crate::db::repository::mesh_topology::UpsertEntry {
                                            node_id: &e.node_id,
                                            hostname: &e.hostname,
                                            platform: &e.platform,
                                            os_info: &e.os_info,
                                            connected_to: &e.connected_to,
                                            direct_addrs: &e.direct_addrs,
                                            port: e.port,
                                            services_json: sj,
                                            models_json: mj,
                                            epoch: payload.epoch,
                                            now_ms: batch_now_ms,
                                        }
                                    })
                                    .collect();
                            if let Err(e) =
                                crate::db::repository::mesh_topology::upsert_batch(pool, &entries)
                            {
                                debug!("mesh_topology batch upsert: {}", e);
                            }
                        }
                    }
                    peer_store.mark_routes_dirty();

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
                            let target = entry.node_id.clone();
                            let qm = qm_events.clone();
                            let hints = merge_contact_hints(
                                load_trusted_contact_hints(&sec.db, &entry.node_id)
                                    .ok()
                                    .flatten(),
                                PairingContactHints {
                                    node_id: entry.node_id.clone(),
                                    public_key_hex: String::new(),
                                    hostname: entry.hostname.clone(),
                                    addresses: entry.direct_addrs.clone(),
                                    relay_url: String::new(),
                                },
                            );
                            tokio::spawn(async move {
                                match qm.connect_to_peer_with_hints(&hints).await {
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
                    // Make the peer visible to GUI as Discovered before any trust
                    // gating runs: even untrusted incoming connections must surface
                    // as pairing candidates. Frames from them are still rejected by
                    // the mesh gate.
                    peer_store.ensure_in_registry(&node_id);
                    let was_connected = peer_store.is_quic_connected(&node_id);
                    peer_store.set_quic_connected(&node_id, true);
                    peer_store.set_status(&node_id, "connected");
                    peer_store.mark_heartbeat(&node_id);
                    if was_connected {
                        debug!(peer_id = %node_id, "QUIC peer — duplicate connected event (iroh multi-path)");
                        continue;
                    }

                    // [SCALE] Debounce + full handler body przeniesione do
                    // spawnowanego taska. Glowny event loop nie blokuje na
                    // 150ms sleep'ie. Per-peer lock (peer_event_locks) zapewnia
                    // ze Connected/Disconnected dla TEGO SAMEGO peera sa
                    // serializowane, ale miedzy roznymi peerami pelna
                    // rownoleglosc — 100 peerow wchodzacych rownoczesnie daje
                    // ~150ms total, nie 100*150ms sekwencyjnie.
                    let peer_lock = peer_event_locks
                        .entry(node_id.clone())
                        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                        .clone();
                    let peer_store_c = peer_store.clone();
                    let qm_events_c = qm_events.clone();
                    let local_node_info_c = local_node_info.clone();
                    let local_node_id_c = local_node_id.clone();
                    let mesh_security_c = mesh_security.clone();
                    let last_sync_sent_c = last_sync_sent.clone();
                    tokio::spawn(async move {
                        let _guard = peer_lock.lock().await;
                        handle_peer_connected(
                            node_id,
                            peer_store_c,
                            qm_events_c,
                            local_node_info_c,
                            local_node_id_c,
                            mesh_security_c,
                            last_sync_sent_c,
                            SYNC_COOLDOWN_SECS,
                        )
                        .await;
                    });
                    continue;
                }
                Ok(IrohMeshEvent::PeerDisconnected { node_id }) => {
                    // Dedup — iroh multi-path moze emitowac kilka disconnect dla tego
                    // samego peera. Emit event tylko na transition connected→offline.
                    let was_connected = peer_store.is_quic_connected(&node_id);
                    if !was_connected {
                        debug!(peer_id = %node_id, "QUIC peer — duplicate disconnect event");
                        continue;
                    }

                    // Mesh services registry — wyrzuc snapshot zerwanego peera.
                    // Bez tego GUI aggregate (krok N3b) widzialby duchowe serwisy
                    // niedostepnego nodu az do nastepnego anti-drift broadcastu.
                    mesh_services_registry.remove_node(&node_id);

                    // [SCALE] Debounce + reszta przeniesione do spawnowanego
                    // taska z per-peer lockiem (wspolny z PeerConnected).
                    // 150ms inline sleep nie blokuje juz main event loop.
                    let peer_lock = peer_event_locks
                        .entry(node_id.clone())
                        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                        .clone();
                    let peer_store_c = peer_store.clone();
                    let qm_events_c = qm_events.clone();
                    let mesh_security_c = mesh_security.clone();
                    let last_sync_sent_c = last_sync_sent.clone();
                    tokio::spawn(async move {
                        let _guard = peer_lock.lock().await;
                        handle_peer_disconnected(
                            node_id,
                            peer_store_c,
                            qm_events_c,
                            mesh_security_c,
                            last_sync_sent_c,
                        )
                        .await;
                    });
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
                        peer_store.update_metrics(&node_id, &metrics);
                        // Aktualizuj topologie peera na podstawie jego connected_peers
                        peer_store.update_topology(&node_id, metrics.connected_peers);
                    }
                }
                Ok(IrohMeshEvent::PairingRequestReceived { peer_id, data }) => {
                    info!(peer_id = %peer_id, data_len = data.len(), "Odebrano PairingRequest przez QUIC");
                    if let Some(ref sec) = mesh_security {
                        match rkyv::from_bytes::<
                            tentaflow_protocol::mesh::MeshPairingRequestPayload,
                            rkyv::rancor::Error,
                        >(&data)
                        {
                            Ok(val) => {
                                let from_node_id = if val.from_node_id.is_empty() {
                                    peer_id.as_str()
                                } else {
                                    val.from_node_id.as_str()
                                };
                                info!(
                                    from_node_id = %from_node_id,
                                    peer_id = %peer_id,
                                    has_pin = !val.pin.is_empty(),
                                    has_pubkey = !val.public_key.is_empty(),
                                    "PairingRequest szczegoly"
                                );
                                if from_node_id == local_node_id {
                                    warn!("Odrzucono PairingRequest od samego siebie (from_node_id == local_node_id)");
                                    continue;
                                }
                                let pin = val.pin.as_str();
                                let public_key = val.public_key.as_str();
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
                                        let quic_mesh_clone = Some(qm_events.clone());
                                        let res = crate::mesh::admin_ops::confirm_pairing(
                                            sec,
                                            from_node_id,
                                            Some(pin),
                                            &quic_mesh_clone,
                                            &local_node_id,
                                            &peer_store,
                                        );
                                        match res {
                                            Ok(_) => {
                                                info!(from = %from_node_id, "Auto-confirm OK");
                                            }
                                            Err(e) => {
                                                warn!(from = %from_node_id, kind = ?e.kind, "Auto-confirm: {}", e.message);
                                            }
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                warn!(peer_id = %peer_id, "Blad parsowania PairingRequest rkyv: {}", e);
                            }
                        }
                    }
                }
                Ok(IrohMeshEvent::PairingConfirmReceived { peer_id, data }) => {
                    // Parsuj rkyv i zatwierdz parowanie — dodaj do zaufanych
                    if let Some(ref sec) = mesh_security {
                        match rkyv::from_bytes::<
                            tentaflow_protocol::mesh::MeshPairingConfirmPayload,
                            rkyv::rancor::Error,
                        >(&data)
                        {
                            Ok(val) => {
                                let from_node_id = if val.from_node_id.is_empty() {
                                    peer_id.as_str()
                                } else {
                                    val.from_node_id.as_str()
                                };
                                let public_key = val.public_key.as_str();
                                let hostname = val.hostname.as_str();
                                let received_pin = val.pin.as_str();

                                // Weryfikuj PIN — inicjator sprawdza czy receiver podal poprawny PIN.
                                // Constant-time compare: identical short PIN strings, but keep ct_eq
                                // for hardening against future variable-length PINs.
                                if let Ok(Some(expected_pin)) = sec.get_pending_pin(from_node_id) {
                                    if !received_pin.is_empty() {
                                        use subtle::ConstantTimeEq;
                                        let same = received_pin.len() == expected_pin.len()
                                            && bool::from(
                                                received_pin
                                                    .as_bytes()
                                                    .ct_eq(expected_pin.as_bytes()),
                                            );
                                        if !same {
                                            warn!(
                                                "PairingConfirm od {} — nieprawidlowy PIN",
                                                from_node_id
                                            );
                                            continue;
                                        }
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
                    // Parsuj rkyv i usun oczekujace parowanie
                    if let Some(ref sec) = mesh_security {
                        match rkyv::from_bytes::<
                            tentaflow_protocol::mesh::MeshPairingRejectPayload,
                            rkyv::rancor::Error,
                        >(&data)
                        {
                            Ok(val) => {
                                let from_node_id = if val.from_node_id.is_empty() {
                                    peer_id.as_str()
                                } else {
                                    val.from_node_id.as_str()
                                };
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
                                warn!(peer_id = %peer_id, "Blad parsowania PairingReject rkyv: {}", e);
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
                                // F1b P3.B — drop the peer's mirrored HMAC keys
                                // so their tokens stop verifying immediately.
                                crate::services::mesh_keys::sync::forget_peer(trusted_id);
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
                            crate::services::mesh_keys::sync::forget_peer(&revoked_node_id);
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
                Ok(IrohMeshEvent::HmacKeysSyncReceived { node_id, payload }) => {
                    // SECURITY: HMAC keys sync MUST be post-trust. Reject from
                    // untrusted peers — otherwise an attacker could inject fake
                    // HMAC keys into our verify pool and mint tokens we would
                    // accept. The `is_trusted` check below is a load-bearing
                    // security boundary; the `mesh_key_sync_integration`
                    // contract test (`receive_handler_has_is_trusted_gate`)
                    // greps this file to prove the gate did not regress.
                    let sender_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&node_id),
                        None => false,
                    };
                    if !sender_trusted {
                        warn!("Odrzucono HmacKeysSync od niezaufanego noda {}", node_id);
                        continue;
                    }
                    let accepted =
                        crate::services::mesh_keys::sync::ingest_advertise(&node_id, payload);
                    if accepted > 0 {
                        info!(
                            from = %node_id,
                            scopes = accepted,
                            "HmacKeysSync przyjety — peer keys zalezone do verify pool"
                        );
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
                    if is_self_discovery_socket_set(&peer_store, &local_node_id, &addresses) {
                        debug!(
                            peer = %node_id,
                            addrs = ?addresses,
                            "Pomijam PeerDiscovered wskazujacy na lokalny host"
                        );
                        peer_store.remove(&node_id);
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
                Ok(IrohMeshEvent::ServicesGetReceived { from_node_id, .. }) => {
                    // Peer prosi o pelny snapshot lokalnych serwisow. Tylko
                    // trusted — defense in depth, send_to_peer wymagal trustu
                    // po stronie inicjatora ale ktos moze otworzyc surowy stream.
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&from_node_id),
                        None => false,
                    };
                    if !is_trusted {
                        debug!(peer = %from_node_id, "MeshServicesGet od niezaufanego peera — ignoruje");
                        continue;
                    }
                    let pool = match &db_pool {
                        Some(p) => p.clone(),
                        None => {
                            debug!("MeshServicesGet: brak db_pool, pomijam odpowiedz");
                            continue;
                        }
                    };
                    let qm = qm_events.clone();
                    let local = local_node_id.clone();
                    let peer = from_node_id.clone();
                    tokio::spawn(async move {
                        let services = match crate::services::snapshot_builder::build_local_snapshot(
                            &pool, &local,
                        ) {
                            Ok(s) => s,
                            Err(e) => {
                                warn!(error = %e, "MeshServicesGet: build_local_snapshot failed");
                                return;
                            }
                        };
                        let payload = tentaflow_protocol::mesh::MeshServicesGetResponsePayload {
                            from_node_id: local,
                            services,
                        };
                        let bytes = match rkyv::to_bytes::<rkyv::rancor::Error>(&payload) {
                            Ok(b) => b,
                            Err(e) => {
                                warn!(error = %e, "MeshServicesGetResponse: rkyv encode failed");
                                return;
                            }
                        };
                        if let Err(e) = qm
                            .send_to_peer(
                                &peer,
                                tentaflow_protocol::mesh::MESH_MSG_SERVICES_GET_RESPONSE,
                                &bytes,
                            )
                            .await
                        {
                            debug!(peer = %peer, "MeshServicesGetResponse send failed: {}", e);
                        }
                    });
                }
                Ok(IrohMeshEvent::ServicesGetResponseReceived { from_node_id, data }) => {
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&from_node_id),
                        None => false,
                    };
                    if !is_trusted {
                        debug!(peer = %from_node_id, "MeshServicesGetResponse od niezaufanego — ignoruje");
                        continue;
                    }
                    match rkyv::from_bytes::<
                        tentaflow_protocol::mesh::MeshServicesGetResponsePayload,
                        rkyv::rancor::Error,
                    >(&data)
                    {
                        Ok(payload) => {
                            debug!(
                                peer = %from_node_id,
                                count = payload.services.len(),
                                "MeshServicesGetResponse: replace_node"
                            );
                            mesh_services_registry
                                .replace_node(payload.from_node_id, payload.services);
                        }
                        Err(e) => {
                            warn!(peer = %from_node_id, "MeshServicesGetResponse decode error: {}", e);
                        }
                    }
                }
                Ok(IrohMeshEvent::ServicesAnnounceReceived { from_node_id, data }) => {
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&from_node_id),
                        None => false,
                    };
                    if !is_trusted {
                        debug!(peer = %from_node_id, "MeshServicesAnnounce od niezaufanego — ignoruje");
                        continue;
                    }
                    match rkyv::from_bytes::<
                        tentaflow_protocol::mesh::MeshServicesAnnouncePayload,
                        rkyv::rancor::Error,
                    >(&data)
                    {
                        Ok(payload) => {
                            debug!(
                                peer = %from_node_id,
                                count = payload.services.len(),
                                "MeshServicesAnnounce: replace_node"
                            );
                            mesh_services_registry
                                .replace_node(payload.from_node_id, payload.services);
                        }
                        Err(e) => {
                            warn!(peer = %from_node_id, "MeshServicesAnnounce decode error: {}", e);
                        }
                    }
                }
                Ok(IrohMeshEvent::ServicesUpdateReceived { from_node_id, data }) => {
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&from_node_id),
                        None => false,
                    };
                    if !is_trusted {
                        debug!(peer = %from_node_id, "MeshServicesUpdate od niezaufanego — ignoruje");
                        continue;
                    }
                    match rkyv::from_bytes::<
                        tentaflow_protocol::mesh::MeshServicesUpdatePayload,
                        rkyv::rancor::Error,
                    >(&data)
                    {
                        Ok(payload) => {
                            debug!(peer = %from_node_id, "MeshServicesUpdate: apply_change");
                            mesh_services_registry
                                .apply_change(payload.from_node_id, payload.change);
                        }
                        Err(e) => {
                            warn!(peer = %from_node_id, "MeshServicesUpdate decode error: {}", e);
                        }
                    }
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
    db_pool: Option<crate::db::DbPool>,
    mesh_services_registry: Arc<crate::services::mesh_registry::MeshServicesRegistry>,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        let mut heartbeat_count: u64 = 0;
        // Probe cache for `CollectorRegistry::probe_available_ids`: raw probes
        // shell out (`which`, `--version`) per collector, so calling all 17
        // every 500 ms heartbeat = ~34 syscalls/s of pure noise. We refresh
        // at most once every 30 s; capability changes propagate next epoch.
        const PROBE_TTL: Duration = Duration::from_secs(30);
        let mut probe_cache: Option<(std::time::Instant, Vec<String>)> = None;
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

                // Capability nsys propagujemy w kazdym heartbeacie: peerzy
                // przy reconnect powinni miec aktualny stan. Detekcja jest
                // cache'owana (~5s) wewnatrz detect_capability, wiec wolanie z petli
                // 2 Hz nie odpala kosztownego `which`/`--version` w kazdym ticku.
                let nsys_cap = crate::profiling::detect_capability().await;

                // Multi-source profiling capability. The discover() set is
                // static, but probe() per collector can shell out, so we only
                // refresh when the cached snapshot is older than PROBE_TTL.
                // Probe runs on the blocking pool — never block the heartbeat
                // task on `which`/binary detection.
                let profiling_collectors_available = {
                    let cached = probe_cache
                        .as_ref()
                        .filter(|(t, _)| t.elapsed() < PROBE_TTL)
                        .map(|(_, ids)| ids.clone());
                    match cached {
                        Some(ids) => ids,
                        None => {
                            let ids = tokio::task::spawn_blocking(|| {
                                crate::profiling::collectors::CollectorRegistry::probe_available_ids(
                                    &crate::profiling::COLLECTOR_REGISTRY,
                                )
                            })
                            .await
                            .unwrap_or_default();
                            probe_cache = Some((std::time::Instant::now(), ids.clone()));
                            ids
                        }
                    }
                };

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
                    nsys_available: nsys_cap.available,
                    nsys_version: nsys_cap.version,
                    profiling_collectors_available,
                };

                // Aktualizuj metryki lokalnego noda w store — pojedyncze klonowanie
                // wewnatrz update_metrics zamiast czterokrotnego u callera.
                peer_store.update_metrics(&local_node_id, &hb);

                // Aktualizuj topologie lokalnego noda
                peer_store.update_topology(&local_node_id, connected_peers.clone());

                // Serializuj RAZ — broadcast do wszystkich peerow uzywa tych samych bajtow
                if let Ok(data) = rkyv::to_bytes::<rkyv::rancor::Error>(&hb) {
                    quic_mesh.send_heartbeat_data(&data).await;
                }

                // Tick routing co 10 heartbeatow (~5s) — faktyczny BFS odbywa sie
                // tylko jesli handlery zaznaczyly dirty. Coalescing: 100 PeerConnected
                // w burst daje 1x BFS zamiast 100.
                heartbeat_count += 1;
                if heartbeat_count % 10 == 0 {
                    peer_store.maybe_recalculate_routes(&local_node_id);
                }

                // Mesh services registry — anti-drift snapshot broadcast co 600
                // heartbeatow (~5 min). Naprawia rozjazd rejestru po nieudanych
                // push delta'ach (`MeshServicesUpdate`) lub gdy peer dolaczyl
                // bez pull-on-connect (np. po zmianie sieci, hardlinkowy reuse).
                if heartbeat_count % 600 == 0 {
                    if let Some(ref pool) = db_pool {
                        match crate::services::snapshot_builder::build_local_snapshot(
                            pool,
                            &local_node_id,
                        ) {
                            Ok(services) => {
                                let payload =
                                    tentaflow_protocol::mesh::MeshServicesAnnouncePayload {
                                        from_node_id: local_node_id.clone(),
                                        services,
                                    };
                                if let Ok(bytes) = rkyv::to_bytes::<rkyv::rancor::Error>(&payload) {
                                    let _ = quic_mesh
                                        .broadcast_to_trusted(
                                            tentaflow_protocol::mesh::MESH_MSG_SERVICES_ANNOUNCE,
                                            &bytes,
                                            None,
                                        )
                                        .await;
                                }
                            }
                            Err(e) => {
                                warn!(error = %e, "MeshServicesAnnounce: build_local_snapshot failed");
                            }
                        }
                    }
                }

                // ModelsSync broadcast co 60 heartbeatow (~30s). Serwer-side
                // scrape z service_registry zwraca aktualne aliasy + stan zaladowania.
                if heartbeat_count % 60 == 0 {
                    let models = collect_local_models(&mesh_services_registry);
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
                    let services: Vec<tentaflow_protocol::mesh::ServiceSummary> =
                        mesh_services_registry
                            .local()
                            .services
                            .iter()
                            .map(|s| tentaflow_protocol::mesh::ServiceSummary {
                                name: s.display_name.clone(),
                                service_type: s.category.clone(),
                                ready: matches!(s.status.as_str(), "running" | "ready"),
                            })
                            .collect();
                    let models_summary: Vec<tentaflow_protocol::mesh::ModelSummary> =
                        collect_local_models(&mesh_services_registry)
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
                        // Rownolegly broadcast — kazdy send_topology_announce blokuje
                        // sie na write do strumienia QUIC danego peera, sekwencyjne
                        // czekanie kumuluje sie liniowo z liczba peerow.
                        let sends = connected_peers.iter().map(|peer_id| {
                            let qm = quic_mesh.clone();
                            let pid = peer_id.clone();
                            let bv_ref = &bv;
                            async move {
                                if let Err(e) = qm.send_topology_announce(&pid, bv_ref).await {
                                    debug!(peer = %pid, "Blad wysylania TopologyAnnounce: {}", e);
                                }
                            }
                        });
                        futures::future::join_all(sends).await;
                    }
                }
            }
        }
    });
}

/// Builds `PeerModelInfo` list from the local snapshot of the V2 mesh services
/// registry. Only LOCAL services — peers' models arrive via `ModelsSync` from
/// their owners.
fn collect_local_models(
    mesh_services_registry: &Arc<crate::services::mesh_registry::MeshServicesRegistry>,
) -> Vec<crate::mesh::peer_store::PeerModelInfo> {
    let local = mesh_services_registry.local();
    local
        .services
        .iter()
        .flat_map(|svc| {
            let kind = svc.category.clone();
            let backend = svc.engine_id.clone();
            let loaded = matches!(svc.status.as_str(), "running" | "ready");
            svc.models
                .iter()
                .map(move |m| crate::mesh::peer_store::PeerModelInfo {
                    alias: m.model_name.clone(),
                    kind: kind.clone(),
                    backend: backend.clone(),
                    size_mb: 0,
                    loaded,
                })
        })
        .collect()
}

/// Slow refresh — co 60s odswiezaj wolno-zmienne dane lokalnego noda:
/// adresy IP, Docker availability/version, OS distro.
fn spawn_slow_refresh(
    peer_store: MeshPeerStore,
    local_node_id: String,
    db_pool: Option<crate::db::DbPool>,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            let db_for_task = db_pool.clone();
            let result = tokio::task::spawn_blocking(move || {
                let raw = node_info_collector::collect_local_addresses();
                // Ta sama logika co w `upsert_local_peer`: IPv4 only + advertise
                // filtry z settings. User moze przez 60s zmienic flagi i nie
                // chcemy zeby stary set adresow wrocil do peer_store.
                let addresses = match db_for_task.as_ref() {
                    Some(db) => {
                        let filters = crate::mesh::network_interfaces::load_advertise_filters(db);
                        let kind_map = crate::mesh::network_interfaces::ipv4_kind_map();
                        crate::mesh::network_interfaces::filter_advertise_ips(
                            &raw, &filters, &kind_map,
                        )
                    }
                    None => raw.into_iter().filter(|ip| ip.is_ipv4()).collect(),
                };
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
