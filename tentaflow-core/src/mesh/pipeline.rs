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
    let enable_dht =
        cfg!(not(any(target_os = "ios", target_os = "android"))) && mesh_config.dht_enabled;
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

    // Pin interfejsu + filtry advertise z GUI musza dotrzec do transportu iroh,
    // nie tylko do warstwy NodeInfo. Bez tego iroh enumeruje wszystkie karty
    // hosta (docker/tailscale/zapasowe NIC) i rozglasza je peerom jako
    // kandydatow hole-punchingu — selekcja sciezki QUIC oscyluje p2p<->relay i
    // polaczenia migocza. Snapshot wczytujemy raz (closure musi byc
    // Send+Sync+'static, bez DbPool); zmiana ustawien i tak restartuje pipeline.
    let (addr_filter, disable_portmapper) = match &db_pool {
        Some(db) => {
            let snapshot = crate::mesh::network_interfaces::build_addr_filter_snapshot(db);
            let disable_portmapper = matches!(
                snapshot.bind_mode,
                crate::mesh::network_interfaces::BindModeSnapshot::Custom(_)
            );
            let filter = iroh::address_lookup::AddrFilter::new(move |addrs| {
                use iroh::TransportAddr;
                std::borrow::Cow::Owned(
                    addrs
                        .iter()
                        .filter(|a| match a {
                            // Relay zawsze przepuszczamy — to fallback dla NAT.
                            TransportAddr::Relay(_) => true,
                            TransportAddr::Ip(sa) => match sa.ip() {
                                std::net::IpAddr::V4(v4) => snapshot.keep_transport_ip(v4),
                                // Logika advertise jest v4-only; IPv6 transport
                                // (link-local) zostawiamy iroh bez ingerencji.
                                std::net::IpAddr::V6(_) => true,
                            },
                            // TransportAddr jest non_exhaustive — przyszle warianty
                            // (np. nowy transport) przepuszczamy, nie blokujemy.
                            _ => true,
                        })
                        .cloned()
                        .collect(),
                )
            });
            (Some(filter), disable_portmapper)
        }
        None => (None, false),
    };

    let iroh_cfg = IrohMeshConfig {
        node_id: app_node_id.clone(),
        bind_addr,
        relay_url,
        enable_lan_discovery: mesh_config.mdns_enabled,
        enable_dht_discovery: enable_dht,
        addr_filter,
        disable_portmapper,
    };

    let security_for_mesh = mesh_security.clone();

    match IrohMeshManager::new(iroh_cfg, security_for_mesh).await {
        Ok(quic_mesh) => {
            let local_node_id = quic_mesh.node_id();

            // F2 P5 — register the mesh broadcast-on-rotate hook so the
            // on-disk key watcher in `services::mod` can push the rotated
            // HMAC keys to every trust-paired peer without waiting for the
            // next `PeerConnected`. Idempotent: a second mesh boot in the
            // same process keeps the first handle.
            crate::services::mesh_keys::register_broadcast_hook(
                local_node_id.clone(),
                quic_mesh.clone(),
            );

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

            // Crash-recovery baseline-adopt: jesli przy starcie istnieje trwaly
            // stan joinera w fazie Elected/Receiving (transfer nie dobiegl konca
            // przed awaria/restartem), wznow pobranie snapshotu. Faza Imported/
            // Completed jest wznawiana post-commit przez sam import (bez sieci),
            // wiec tu obslugujemy tylko stany wymagajace ponownego strumienia.
            {
                let qm = quic_mesh.clone();
                let db = mesh_security.db.clone();
                tokio::spawn(async move {
                    match crate::sync::baseline_transport::pending_joiner_resume(&db) {
                        Ok(Some((donor, epoch_seen))) => {
                            if let Err(e) = qm.pull_baseline_from_donor(&donor, epoch_seen).await {
                                warn!(
                                    donor = %donor,
                                    "baseline adopt: wznowienie pull przy starcie nieudane: {}",
                                    e
                                );
                            }
                        }
                        Ok(None) => {}
                        Err(e) => warn!("baseline adopt: odczyt stanu wznowienia nieudany: {}", e),
                    }
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
                            if !qm.should_proactively_dial(&nid) {
                                return;
                            }
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
            if let Some(ref pool) = db_pool {
                spawn_robot_advertiser(quic_mesh.clone(), local_node_id.clone(), pool.clone());
            }
            spawn_slow_refresh(
                mesh_peer_store.clone(),
                local_node_id.clone(),
                db_pool.clone(),
            );
            spawn_pairing_cleanup(mesh_security.clone());
            spawn_sync_repair_scheduler(quic_mesh.clone(), mesh_security.clone());
            spawn_trust_expiry_prune(
                quic_mesh.clone(),
                mesh_peer_store.clone(),
                mesh_security.clone(),
                mesh_config.trust_expiry_days,
            );

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
            let name_map = crate::mesh::network_interfaces::ipv4_name_map();
            crate::mesh::network_interfaces::filter_advertise_ips(
                &raw_addresses,
                &filters,
                &kind_map,
                &name_map,
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

/// Drains the baseline-epoch reconcile requests the sync runtime produced while
/// admitting incoming ops and, for each, runs ONE baseline adopt from the donor
/// whose epoch canonically won. This is the self-heal for diverged epochs: a node
/// that minted its own epoch during a migration adopts the mesh-wide winner's
/// baseline instead of rejecting its ops forever. The adopt is single-flight
/// (debounced in the runtime; `pull_baseline_from_donor` further serializes via
/// `begin_adopt_atomic`), so a flood of mismatched ops triggers exactly one pull
/// per donor+epoch. The debounce key is released when the pull settles so a later
/// divergence can retry.
fn spawn_epoch_reconcile_adopts(qm_events: &Arc<IrohMeshManager>) {
    let requests = crate::sync::runtime::take_pending_epoch_reconcile();
    for request in requests {
        let qm = qm_events.clone();
        tokio::spawn(async move {
            let donor = request.donor_node_id;
            let epoch_counter = request.donor_epoch_counter;
            if let Err(e) = qm.pull_baseline_from_donor(&donor, epoch_counter).await {
                warn!(
                    donor = %donor,
                    "sync runtime: epoch-reconcile baseline adopt failed (will retry on next mismatch): {}",
                    e
                );
            }
            // Release the debounce regardless of outcome: a failed adopt must be
            // retryable on the next mismatch, and a successful one bumped our epoch
            // so the next op no longer mismatches anyway.
            crate::sync::runtime::clear_epoch_adopt_inflight(&donor, epoch_counter);
        });
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
    // Re-assert pod per-peer lockiem. Inline set_quic_connected(true) z event
    // loopu moze zostac nadpisane przez wyscigajacy handle_peer_disconnected
    // poprzedniego polaczenia (jego is_connected-check zdazyl zobaczyc pusta
    // mape ZANIM nowe polaczenie sie zarejestrowalo, a zapis false wykonal sie
    // juz PO inline true). Bez tego store zostaje "disconnected" na zawsze
    // mimo zywego transportu — routing i sync omijaja zywego peera.
    peer_store.set_quic_connected(&node_id, true);
    peer_store.set_status(&node_id, "connected");
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
    if let Ok(hello_bytes) = crate::mesh::cbor::encode(&hello) {
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
        if let Ok(kp_bytes) = crate::mesh::cbor::encode(&payload) {
            if let Err(e) = qm_events.send_known_peers(&node_id, &kp_bytes).await {
                debug!("Blad wysylania KnownPeers do {}: {}", node_id, e);
            }
        }
    }

    // NodeInfo + TrustedKeysSync — TYLKO do zaufanych (is_trusted scache'owany powyzej).
    if is_trusted {
        if let Ok(info_bytes) = crate::mesh::cbor::encode(&local_node_info) {
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
                        .map(|(nid, pk, approved_at)| tentaflow_protocol::mesh::TrustedKeyEntry {
                            node_id: nid.clone(),
                            public_key_hex: pk.clone(),
                            approved_at: approved_at.clone(),
                        })
                        .collect();
                    let payload =
                        tentaflow_protocol::mesh::TrustedKeysSyncPayload { keys: entries };
                    if let Ok(sync_data) = crate::mesh::cbor::encode(&payload) {
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
                    if let Ok(data) = crate::mesh::cbor::encode(&payload) {
                        let _ = qm_events
                            .send_ufp2_to_peer(
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
            let advertise = crate::services::mesh_keys::sync::build_local_advertise(&local_node_id);
            if let Some(bytes) = crate::services::mesh_keys::sync::encode_advertise(&advertise) {
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
        if let Ok(bytes) = crate::mesh::cbor::encode(&pull) {
            if let Err(e) = qm_events
                .send_ufp2_to_peer(
                    &node_id,
                    tentaflow_protocol::mesh::MESH_MSG_SERVICES_GET,
                    &bytes,
                )
                .await
            {
                debug!(peer = %node_id, "MeshServicesGet send failed: {}", e);
            }
        }

        // Pull-on-connect for robots: ask the newly-connected peer for its full
        // owned-robot snapshot. Without this, a node that joins later never
        // discovers robots that were already advertised before it connected — the
        // periodic ANNOUNCE would eventually fill the gap, but only after up to
        // ~5 min. Mirrors the MeshServicesGet pull above; the response lands in
        // the global robot registry via `RobotsGetResponseReceived`. Trusted-only
        // (the responder re-checks trust — defense in depth).
        let robots_pull = crate::mesh::robot_dispatch::RobotsGetPayload {
            from_node_id: local_node_id.clone(),
        };
        if let Ok(bytes) = crate::mesh::cbor::encode(&robots_pull) {
            if let Err(e) = qm_events
                .send_ufp2_to_peer(
                    &node_id,
                    tentaflow_protocol::mesh::MESH_MSG_ROBOTS_GET,
                    &bytes,
                )
                .await
            {
                debug!(peer = %node_id, "RobotsGet send failed: {}", e);
            }
        }

        match crate::sync::runtime::build_push_payload_for_target(&node_id, 128) {
            Ok(Some(payload)) => {
                let op_count = payload.operations.len();
                match crate::mesh::cbor::encode(&payload) {
                    Ok(bytes) => {
                        if let Err(e) = qm_events
                            .send_ufp2_to_peer(
                                &node_id,
                                tentaflow_protocol::mesh::MESH_MSG_SYNC_PUSH,
                                &bytes,
                            )
                            .await
                        {
                            debug!(peer = %node_id, "SyncPush send failed: {}", e);
                        } else {
                            debug!(peer = %node_id, op_count, "SyncPush sent on connect");
                        }
                    }
                    Err(e) => warn!(peer = %node_id, "SyncPush encode error: {}", e),
                }
            }
            Ok(None) => {}
            Err(e) => warn!(peer = %node_id, "SyncPush build failed: {}", e),
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
                    let name_map = crate::mesh::network_interfaces::ipv4_name_map();
                    let filtered_ips = crate::mesh::network_interfaces::filter_advertise_ips(
                        &addresses, &filters, &kind_map, &name_map,
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
                if !qm2.should_proactively_dial(&node_id2) {
                    return;
                }
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
                    match crate::mesh::cbor::decode::<MeshHelloPayload>(&data) {
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
                    let payload = match crate::mesh::cbor::decode::<KnownPeersPayload>(&data) {
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
                        // Asymetria: nizszy node_id dialuje od razu, wyzszy czeka na
                        // incoming (fallback po grace). Bez tego oba dialuja naraz →
                        // kolizje i spam tie-break.
                        if !qm_events.should_proactively_dial(&entry.node_id) {
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
                    let payload = match crate::mesh::cbor::decode::<TopologyAnnouncePayload>(&data)
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
                            // Asymetria: tylko nizszy node_id dialuje od razu; wyzszy
                            // czeka na incoming (fallback po grace).
                            if !qm_events.should_proactively_dial(&entry.node_id) {
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
                        if let Ok(bytes_vec) = crate::mesh::cbor::encode(&forwarded) {
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
                    match crate::mesh::cbor::decode::<NodeInfo>(&data) {
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
                    // Drop the peer's advertised robots too, or the resolver could
                    // route a robot command to a disconnected owner.
                    crate::mesh::robot_dispatch::global().remove_node(&node_id);

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
                    if let Ok(metrics) = crate::mesh::cbor::decode::<HeartbeatMetrics>(&heartbeat) {
                        peer_store.update_metrics(&node_id, &metrics);
                        // Aktualizuj topologie peera na podstawie jego connected_peers
                        peer_store.update_topology(&node_id, metrics.connected_peers);
                    }
                    // Heartbeat nad zywym transportem = dowod polaczenia (to samo
                    // zalozenie, na ktorym peer_registry naprawia swoj stan w
                    // update_metrics). Jesli store ma quic_connected=false, to
                    // stale disconnect wygral wyscig eventow — napraw flage od
                    // razu i odpal pelny handler connected pod per-peer lockiem,
                    // bo stale disconnect zdazyl tez wyrzucic klucze HMAC
                    // (forget_peer) i cooldown TrustedKeysSync.
                    if !peer_store.is_quic_connected(&node_id)
                        && qm_events.is_connected(&node_id).await
                    {
                        warn!(
                            peer_id = %node_id,
                            "Heartbeat od peera oznaczonego jako rozlaczony — naprawiam stale disconnect"
                        );
                        peer_store.set_quic_connected(&node_id, true);
                        peer_store.set_status(&node_id, "connected");
                        peer_store.mark_routes_dirty();
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
                    }
                }
                Ok(IrohMeshEvent::PairingRequestReceived { peer_id, data }) => {
                    info!(peer_id = %peer_id, data_len = data.len(), "Odebrano PairingRequest przez QUIC");
                    if let Some(ref sec) = mesh_security {
                        match crate::mesh::cbor::decode::<
                            tentaflow_protocol::mesh::MeshPairingRequestPayload,
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
                                    warn!(
                                        "Odrzucono PairingRequest od samego siebie (from_node_id == local_node_id)"
                                    );
                                    continue;
                                }
                                let pin = val.pin.as_str();
                                let public_key = val.public_key.as_str();
                                if let Err(e) =
                                    sec.receive_pairing_request(from_node_id, pin, public_key)
                                {
                                    warn!("Blad zapisu PairingRequest od {}: {}", peer_id, e);
                                } else {
                                    info!(
                                        "PairingRequest od {} zapisany — oczekuje na potwierdzenie PIN",
                                        from_node_id
                                    );
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
                                        )
                                        .await;
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
                                warn!(peer_id = %peer_id, "Blad parsowania PairingRequest CBOR: {}", e);
                            }
                        }
                    }
                }
                Ok(IrohMeshEvent::PairingTrusted { hints }) => {
                    crate::mesh::admin_ops::mirror_trusted_peer_to_registry(
                        &peer_store,
                        &hints.node_id,
                        &hints.public_key_hex,
                        &hints.hostname,
                        Some(&hints),
                    );
                    if let Err(e) = qm_events.connect_to_peer_with_hints(&hints).await {
                        warn!(
                            peer = %hints.node_id,
                            "PairingTrusted: mesh connect failed: {}",
                            e
                        );
                        continue;
                    }
                    if let Ok(info_bytes) = crate::mesh::cbor::encode(&local_node_info) {
                        if let Err(e) = qm_events.send_node_info(&hints.node_id, &info_bytes).await
                        {
                            warn!(
                                peer = %hints.node_id,
                                "PairingTrusted: NodeInfo send failed: {}",
                                e
                            );
                        }
                    }
                    if let Some(ref sec) = mesh_security {
                        let all_keys = sec.get_all_trusted_keys();
                        if !all_keys.is_empty() {
                            let entries: Vec<tentaflow_protocol::mesh::TrustedKeyEntry> = all_keys
                                .iter()
                                .map(|(nid, pk, approved_at)| tentaflow_protocol::mesh::TrustedKeyEntry {
                                    node_id: nid.clone(),
                                    public_key_hex: pk.clone(),
                                    approved_at: approved_at.clone(),
                                })
                                .collect();
                            let payload =
                                tentaflow_protocol::mesh::TrustedKeysSyncPayload { keys: entries };
                            if let Ok(sync_data) = crate::mesh::cbor::encode(&payload) {
                                if let Err(e) = qm_events
                                    .send_trusted_keys_sync(&hints.node_id, &sync_data)
                                    .await
                                {
                                    warn!(
                                        peer = %hints.node_id,
                                        "PairingTrusted: TrustedKeysSync send failed: {}",
                                        e
                                    );
                                }
                            }
                        }
                    }
                }
                Ok(IrohMeshEvent::PairingConfirmReceived { peer_id, data }) => {
                    // Parsuj CBOR i zatwierdz parowanie — dodaj do zaufanych
                    if let Some(ref sec) = mesh_security {
                        match crate::mesh::cbor::decode::<
                            tentaflow_protocol::mesh::MeshPairingConfirmPayload,
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
                                    crate::mesh::admin_ops::mirror_trusted_peer_to_registry(
                                        &peer_store,
                                        from_node_id,
                                        public_key,
                                        hostname,
                                        None,
                                    );
                                    let _ = crate::net::iroh::pairing::delete_pending_contact_hints(
                                        &sec.db,
                                        from_node_id,
                                    );
                                    info!("Otrzymano PairingConfirm od {} — node zaufany", peer_id);

                                    // Po sparowaniu — wyslij NodeInfo do nowo zaufanego peera
                                    let target_node_id = from_node_id.to_string();
                                    if let Ok(info_bytes) =
                                        crate::mesh::cbor::encode(&local_node_info)
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
                                            .map(|(nid, pk, approved_at)| {
                                                tentaflow_protocol::mesh::TrustedKeyEntry {
                                                    node_id: nid.clone(),
                                                    public_key_hex: pk.clone(),
                                                    approved_at: approved_at.clone(),
                                                }
                                            })
                                            .collect();
                                        let payload =
                                            tentaflow_protocol::mesh::TrustedKeysSyncPayload {
                                                keys: entries,
                                            };
                                        let sync_data =
                                            crate::mesh::cbor::encode(&payload).unwrap_or_default();
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
                                            .map(|(nid, pk, approved_at)| {
                                                tentaflow_protocol::mesh::TrustedKeyEntry {
                                                    node_id: nid.clone(),
                                                    public_key_hex: pk.clone(),
                                                    approved_at: approved_at.clone(),
                                                }
                                            })
                                            .collect();
                                        let payload =
                                            tentaflow_protocol::mesh::TrustedKeysSyncPayload {
                                                keys: entries,
                                            };
                                        let broadcast_data =
                                            crate::mesh::cbor::encode(&payload).unwrap_or_default();
                                        // Broadcast do wszystkich trusted — pomija nowo sparowanego (juz dostal wyzej)
                                        let results = qm_events.broadcast_ufp2_to_trusted(
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
                                warn!(peer_id = %peer_id, "Blad parsowania PairingConfirm CBOR: {}", e);
                            }
                        }
                    }
                }
                Ok(IrohMeshEvent::PairingRejectReceived { peer_id, data }) => {
                    // Parsuj CBOR i usun oczekujace parowanie
                    if let Some(ref sec) = mesh_security {
                        match crate::mesh::cbor::decode::<
                            tentaflow_protocol::mesh::MeshPairingRejectPayload,
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
                                warn!(peer_id = %peer_id, "Blad parsowania PairingReject CBOR: {}", e);
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
                            for (trusted_id, _, _) in &all_trusted {
                                let _ = sec.unpair(trusted_id);
                                // F1b P3.B — drop the peer's mirrored HMAC keys
                                // so their tokens stop verifying immediately.
                                crate::services::mesh_keys::sync::forget_peer(trusted_id);
                                // Drop their advertised robots so the resolver stops
                                // routing to a node we no longer trust.
                                crate::mesh::robot_dispatch::global().remove_node(trusted_id);
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
                            // Drop the revoked node's advertised robots so the
                            // resolver stops routing commands to an untrusted owner.
                            crate::mesh::robot_dispatch::global().remove_node(&revoked_node_id);
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
                    // Drop the leaving node's advertised robots so the resolver
                    // stops routing commands to an owner that is going offline.
                    crate::mesh::robot_dispatch::global().remove_node(&node_id);
                    qm_events.disconnect_peer(&node_id).await;
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
                        for (remote_node_id, public_key_hex, approved_at) in &keys {
                            if sec.is_trusted(remote_node_id) {
                                continue;
                            }
                            match sec.add_trusted_key(
                                remote_node_id,
                                public_key_hex,
                                "",
                                Some(approved_at),
                            ) {
                                Ok(()) => {
                                    peer_store.ensure_trusted_peer(
                                        remote_node_id,
                                        public_key_hex,
                                        "",
                                    );
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
                Ok(IrohMeshEvent::MeshCommandReceived {
                    from_node_id,
                    command,
                }) => {
                    debug!(from = %from_node_id, "Otrzymano MeshCommand — przekazuje do executora");
                    qm_events
                        .handle_command_received(&from_node_id, &command)
                        .await;
                }
                Ok(IrohMeshEvent::MeshCommandResponseReceived { from_node_id, data }) => {
                    qm_events
                        .handle_command_response_received(&from_node_id, &data)
                        .await;
                }
                Ok(IrohMeshEvent::MeshDeployProgressReceived { from_node_id, data }) => {
                    let sender_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&from_node_id),
                        None => false,
                    };
                    if !sender_trusted {
                        warn!(
                            "Odrzucono DeployProgress od niezaufanego noda {}",
                            from_node_id
                        );
                        continue;
                    }
                    match crate::mesh::cbor::decode::<tentaflow_protocol::mesh::MeshMessage>(&data)
                    {
                        Ok(tentaflow_protocol::mesh::MeshMessage::MeshDeployProgress {
                            command_id,
                            phase,
                            message,
                            percent,
                            is_done,
                            ..
                        }) => {
                            let sender = crate::deploy::log_bus::sender_for(&command_id);
                            if is_done {
                                let _ = sender.send(crate::deploy::log_bus::BusMessage::End {
                                    deploy_id: command_id.clone(),
                                    final_status: phase.clone(),
                                    image_tag: String::new(),
                                    container_name: String::new(),
                                    error_message: message.clone(),
                                    duration_ms: 0,
                                });
                                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                                crate::deploy::log_bus::close(&command_id);
                            } else {
                                let line = crate::deploy::log_bus::LogLine {
                                    deploy_id: command_id,
                                    kind: phase.clone(),
                                    line: message.clone(),
                                    phase: if phase == "phase" || phase == "progress" {
                                        message
                                    } else {
                                        String::new()
                                    },
                                    progress_pct: percent as u32,
                                    ts_ms: crate::deploy::log_bus::now_ms(),
                                };
                                let _ = sender.send(crate::deploy::log_bus::BusMessage::Line(line));
                            }
                        }
                        Ok(other) => {
                            warn!(from = %from_node_id, kind = ?other, "Nieoczekiwany payload DeployProgress");
                        }
                        Err(e) => {
                            warn!(from = %from_node_id, "Blad dekodowania DeployProgress: {}", e);
                        }
                    }
                }
                Ok(IrohMeshEvent::ModelListUpdate { node_id, data }) => {
                    // ModelsSync — nadpisuje liste modeli danego peera.
                    // Format: CBOR-zakodowany `ModelsSync { models: Vec<PeerModelInfo> }`.
                    match crate::mesh::cbor::decode::<crate::mesh::peer_store::ModelsSync>(&data) {
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
                Ok(IrohMeshEvent::PeerDiscovered {
                    node_id,
                    addresses,
                    hostname,
                }) => {
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
                    // Nazwa z mDNS user_data — pozwala UI pokazac czytelna nazwe
                    // peera juz na karcie "discovered" (przed parowaniem). Nie
                    // nadpisujemy istniejacej niepusta nazwy pusta wartoscia.
                    if !hostname.is_empty() {
                        peer_store.set_hostname(&node_id, &hostname);
                    }
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
                        let bytes = match crate::mesh::cbor::encode(&payload) {
                            Ok(b) => b,
                            Err(e) => {
                                warn!(error = %e, "MeshServicesGetResponse: CBOR encode failed");
                                return;
                            }
                        };
                        if let Err(e) = qm
                            .send_ufp2_to_peer(
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
                    match crate::mesh::cbor::decode::<
                        tentaflow_protocol::mesh::MeshServicesGetResponsePayload,
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
                    match crate::mesh::cbor::decode::<
                        tentaflow_protocol::mesh::MeshServicesAnnouncePayload,
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
                    match crate::mesh::cbor::decode::<
                        tentaflow_protocol::mesh::MeshServicesUpdatePayload,
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
                Ok(IrohMeshEvent::RobotsAnnounceReceived { from_node_id, data }) => {
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&from_node_id),
                        None => false,
                    };
                    if !is_trusted {
                        debug!(peer = %from_node_id, "RobotsAnnounce od niezaufanego — ignoruje");
                        continue;
                    }
                    match crate::mesh::cbor::decode::<
                        crate::mesh::robot_dispatch::RobotsAnnouncePayload,
                    >(&data)
                    {
                        Ok(payload) => {
                            // Bind the announce to its transport-authenticated
                            // sender (and skip our own echo): a trusted node must
                            // not advertise robots on behalf of another node id.
                            // Key on `from_node_id` (transport), never the
                            // self-claimed payload field.
                            match crate::mesh::robot_dispatch::bind_announce_sender(
                                &payload.from_node_id,
                                &from_node_id,
                                &local_node_id,
                            ) {
                                Some(key) => {
                                    // Trust the transport identity, not the
                                    // self-claimed per-robot `node_id`: a trusted
                                    // peer must not advertise a robot owned by (or
                                    // spoofing) another node. Force every robot's
                                    // node_id to the authenticated sender before it
                                    // can enter the registry.
                                    let robots =
                                        crate::mesh::robot_dispatch::normalize_advertised_node_id(
                                            payload.robots,
                                            key,
                                        );
                                    debug!(
                                        peer = %from_node_id,
                                        count = robots.len(),
                                        "RobotsAnnounce: replace_node"
                                    );
                                    crate::mesh::robot_dispatch::global()
                                        .replace_node(key, robots);
                                }
                                None => {
                                    if payload.from_node_id != from_node_id
                                        && payload.from_node_id != local_node_id
                                    {
                                        warn!(
                                            peer = %from_node_id,
                                            claimed = %payload.from_node_id,
                                            "RobotsAnnounce: from_node_id mismatch — dropping"
                                        );
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!(peer = %from_node_id, "RobotsAnnounce decode error: {}", e);
                        }
                    }
                }
                Ok(IrohMeshEvent::RobotsGetReceived { from_node_id, .. }) => {
                    // Peer asks for our complete owned-robot snapshot (pull-on-
                    // connect). Trusted only — defense in depth, mirroring
                    // ServicesGetReceived: the initiator already gates on trust,
                    // but someone could open a raw stream.
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&from_node_id),
                        None => false,
                    };
                    if !is_trusted {
                        debug!(peer = %from_node_id, "RobotsGet od niezaufanego peera — ignoruje");
                        continue;
                    }
                    // Serve the GET from the CACHED local snapshot the advertiser
                    // refreshes every ~10 s — NO addon status-tool calls on the GET
                    // path. Running status tools per GET let a noisy/compromised
                    // trusted peer force repeated blocking status probes (DoS); the
                    // in-memory read is cheap and the ~10 s staleness is acceptable.
                    //
                    // Org scope is NOT filtered here: like the existing SERVICES
                    // discovery, the mesh advertises within the trust domain and the
                    // requester's org cannot be trusted at the mesh layer. Per-org
                    // visibility is enforced at the CONSUMPTION layer (the camera
                    // path's `remote_camera_owner` filters `org_id == caller_org`;
                    // the Robots dashboard list/control handlers filter by caller
                    // org). Each advertised robot carries `org_id`, so that layer can
                    // filter. A requester-declared org filter here would be bogus
                    // (forgeable) and inconsistent with services discovery.
                    let robots = crate::mesh::robot_dispatch::global()
                        .local_robots(&local_node_id);
                    let qm = qm_events.clone();
                    let local = local_node_id.clone();
                    let peer = from_node_id.clone();
                    tokio::spawn(async move {
                        let payload =
                            crate::mesh::robot_dispatch::RobotsGetResponsePayload {
                                from_node_id: local,
                                robots,
                            };
                        let bytes = match crate::mesh::cbor::encode(&payload) {
                            Ok(b) => b,
                            Err(e) => {
                                warn!(error = %e, "RobotsGetResponse: CBOR encode failed");
                                return;
                            }
                        };
                        if let Err(e) = qm
                            .send_ufp2_to_peer(
                                &peer,
                                tentaflow_protocol::mesh::MESH_MSG_ROBOTS_GET_RESPONSE,
                                &bytes,
                            )
                            .await
                        {
                            debug!(peer = %peer, "RobotsGetResponse send failed: {}", e);
                        }
                    });
                }
                Ok(IrohMeshEvent::RobotsGetResponseReceived { from_node_id, data }) => {
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&from_node_id),
                        None => false,
                    };
                    if !is_trusted {
                        debug!(peer = %from_node_id, "RobotsGetResponse od niezaufanego — ignoruje");
                        continue;
                    }
                    match crate::mesh::cbor::decode::<
                        crate::mesh::robot_dispatch::RobotsGetResponsePayload,
                    >(&data)
                    {
                        Ok(payload) => {
                            // Same identity binding as RobotsAnnounce: a trusted
                            // peer must not advertise robots on behalf of another
                            // node id, and every per-robot node_id is forced to the
                            // transport sender so the registry can never trust a
                            // spoofed owner (`remote_camera_owner`).
                            match crate::mesh::robot_dispatch::bind_announce_sender(
                                &payload.from_node_id,
                                &from_node_id,
                                &local_node_id,
                            ) {
                                Some(key) => {
                                    let robots =
                                        crate::mesh::robot_dispatch::normalize_advertised_node_id(
                                            payload.robots,
                                            key,
                                        );
                                    debug!(
                                        peer = %from_node_id,
                                        count = robots.len(),
                                        "RobotsGetResponse: replace_node"
                                    );
                                    crate::mesh::robot_dispatch::global()
                                        .replace_node(key, robots);
                                }
                                None => {
                                    if payload.from_node_id != from_node_id
                                        && payload.from_node_id != local_node_id
                                    {
                                        warn!(
                                            peer = %from_node_id,
                                            claimed = %payload.from_node_id,
                                            "RobotsGetResponse: from_node_id mismatch — dropping"
                                        );
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!(peer = %from_node_id, "RobotsGetResponse decode error: {}", e);
                        }
                    }
                }
                Ok(IrohMeshEvent::RobotsUpdateReceived { from_node_id, data }) => {
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&from_node_id),
                        None => false,
                    };
                    if !is_trusted {
                        debug!(peer = %from_node_id, "RobotsUpdate od niezaufanego — ignoruje");
                        continue;
                    }
                    match crate::mesh::cbor::decode::<
                        crate::mesh::robot_dispatch::RobotsUpdatePayload,
                    >(&data)
                    {
                        Ok(payload) => {
                            // Identity binding (same as RobotsAnnounce): key on the
                            // transport sender, never the self-claimed payload field,
                            // and re-own the changed robot to the transport sender so
                            // an Added/Updated delta cannot smuggle a spoofed node_id.
                            match crate::mesh::robot_dispatch::bind_announce_sender(
                                &payload.from_node_id,
                                &from_node_id,
                                &local_node_id,
                            ) {
                                Some(key) => {
                                    use crate::mesh::robot_dispatch::RobotChange;
                                    let change = match payload.change {
                                        RobotChange::Added(robot) => RobotChange::Added(
                                            crate::mesh::robot_dispatch::normalize_advertised_node_id(
                                                vec![robot],
                                                key,
                                            )
                                            .remove(0),
                                        ),
                                        RobotChange::Updated(robot) => RobotChange::Updated(
                                            crate::mesh::robot_dispatch::normalize_advertised_node_id(
                                                vec![robot],
                                                key,
                                            )
                                            .remove(0),
                                        ),
                                        RobotChange::Removed(id) => RobotChange::Removed(id),
                                    };
                                    debug!(peer = %from_node_id, "RobotsUpdate: apply_change");
                                    crate::mesh::robot_dispatch::global()
                                        .apply_change(key, change);
                                }
                                None => {
                                    if payload.from_node_id != from_node_id
                                        && payload.from_node_id != local_node_id
                                    {
                                        warn!(
                                            peer = %from_node_id,
                                            claimed = %payload.from_node_id,
                                            "RobotsUpdate: from_node_id mismatch — dropping"
                                        );
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!(peer = %from_node_id, "RobotsUpdate decode error: {}", e);
                        }
                    }
                }
                Ok(IrohMeshEvent::AliasSyncReceived { from_node_id, data }) => {
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&from_node_id),
                        None => false,
                    };
                    if !is_trusted {
                        debug!(peer = %from_node_id, "AliasSync od niezaufanego — ignoruje");
                        continue;
                    }
                    let Some(ref pool) = db_pool else {
                        debug!(peer = %from_node_id, "AliasSync bez db_pool — pomijam");
                        continue;
                    };
                    match serde_json::from_slice::<Vec<crate::db::models::DbModelAlias>>(&data) {
                        Ok(aliases) => {
                            match crate::db::repository::replace_model_aliases_from_sync(
                                pool, &aliases,
                            ) {
                                Ok(()) => {
                                    debug!(
                                        peer = %from_node_id,
                                        count = aliases.len(),
                                        "AliasSync: snapshot aliasow zapisany"
                                    );
                                    // Odswiez stan in-memory routera. Odbior synca
                                    // NIE re-broadcastuje (anty-petla) — dlatego nie
                                    // uzywamy broadcast_alias_mutation.
                                    if let Some(router) = crate::routing::router::active_router()
                                    {
                                        router.update_alias_cache_from_sync(aliases);
                                        router.rebuild_catalog();
                                    }
                                }
                                Err(e) => {
                                    warn!(
                                        peer = %from_node_id,
                                        "AliasSync: blad zapisu snapshotu: {}", e
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            warn!(peer = %from_node_id, "AliasSync decode error: {}", e);
                        }
                    }
                }
                Ok(IrohMeshEvent::RoutingSyncReceived { from_node_id, data }) => {
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&from_node_id),
                        None => false,
                    };
                    if !is_trusted {
                        debug!(peer = %from_node_id, "RoutingSync od niezaufanego — ignoruje");
                        continue;
                    }
                    let Some(ref pool) = db_pool else {
                        debug!(peer = %from_node_id, "RoutingSync bez db_pool — pomijam");
                        continue;
                    };
                    match serde_json::from_slice::<crate::routing::cluster_sync::RoutingSyncPayload>(
                        &data,
                    ) {
                        Ok(payload) => {
                            let clusters = payload.clusters.len();
                            let members = payload.members.len();
                            // Odbior synca tylko zapisuje snapshot — NIE
                            // re-broadcastuje (anty-petla).
                            match crate::routing::cluster_sync::apply_routing_sync(pool, payload) {
                                Ok(()) => {
                                    debug!(
                                        peer = %from_node_id,
                                        clusters,
                                        members,
                                        "RoutingSync: snapshot konfiguracji routingu zapisany"
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        peer = %from_node_id,
                                        "RoutingSync: blad zapisu snapshotu: {}", e
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            warn!(peer = %from_node_id, "RoutingSync decode error: {}", e);
                        }
                    }
                }
                Ok(IrohMeshEvent::SyncPushReceived { from_node_id, data }) => {
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&from_node_id),
                        None => false,
                    };
                    if !is_trusted {
                        debug!(peer = %from_node_id, "SyncPush od niezaufanego — ignoruje");
                        continue;
                    }
                    match crate::mesh::cbor::decode::<tentaflow_protocol::mesh::MeshSyncPushPayload>(
                        &data,
                    ) {
                        Ok(payload) => {
                            match crate::sync::runtime::handle_push_payload(&from_node_id, payload)
                            {
                                Ok(Some(ack)) => match crate::mesh::cbor::encode(&ack) {
                                    Ok(bytes) => {
                                        if let Err(e) = qm_events
                                            .send_ufp2_to_peer(
                                                &from_node_id,
                                                tentaflow_protocol::mesh::MESH_MSG_SYNC_ACK,
                                                &bytes,
                                            )
                                            .await
                                        {
                                            warn!(peer = %from_node_id, "SyncAck send failed: {}", e);
                                        }
                                    }
                                    Err(e) => {
                                        warn!(peer = %from_node_id, "SyncAck encode error: {}", e)
                                    }
                                },
                                Ok(None) => {}
                                Err(e) => {
                                    warn!(peer = %from_node_id, "SyncPush handle failed: {}", e)
                                }
                            }
                            spawn_epoch_reconcile_adopts(&qm_events);
                        }
                        Err(e) => warn!(peer = %from_node_id, "SyncPush decode error: {}", e),
                    }
                }
                Ok(IrohMeshEvent::SyncAckReceived { from_node_id, data }) => {
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&from_node_id),
                        None => false,
                    };
                    if !is_trusted {
                        debug!(peer = %from_node_id, "SyncAck od niezaufanego — ignoruje");
                        continue;
                    }
                    match crate::mesh::cbor::decode::<tentaflow_protocol::mesh::MeshSyncAckPayload>(
                        &data,
                    ) {
                        Ok(payload) => {
                            if let Err(e) =
                                crate::sync::runtime::handle_ack_payload(&from_node_id, payload)
                            {
                                warn!(peer = %from_node_id, "SyncAck handle failed: {}", e);
                            }
                        }
                        Err(e) => warn!(peer = %from_node_id, "SyncAck decode error: {}", e),
                    }
                }
                Ok(IrohMeshEvent::SyncPullReceived { from_node_id, data }) => {
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&from_node_id),
                        None => false,
                    };
                    if !is_trusted {
                        debug!(peer = %from_node_id, "SyncPull od niezaufanego — ignoruje");
                        continue;
                    }
                    match crate::mesh::cbor::decode::<tentaflow_protocol::mesh::MeshSyncPullPayload>(
                        &data,
                    ) {
                        Ok(payload) => {
                            match crate::sync::runtime::handle_pull_payload(&from_node_id, payload)
                            {
                                Ok(Some(crate::sync::runtime::MeshSyncPullResult::Operations(
                                    response,
                                ))) => {
                                    match crate::mesh::cbor::encode(&response) {
                                    Ok(bytes) => {
                                        if let Err(e) = qm_events
                                            .send_ufp2_to_peer(
                                                &from_node_id,
                                                tentaflow_protocol::mesh::MESH_MSG_SYNC_PULL_RESPONSE,
                                                &bytes,
                                            )
                                            .await
                                        {
                                            warn!(peer = %from_node_id, "SyncPullResponse send failed: {}", e);
                                        }
                                    }
                                    Err(e) => warn!(peer = %from_node_id, "SyncPullResponse encode error: {}", e),
                                }
                                }
                                Ok(Some(crate::sync::runtime::MeshSyncPullResult::Snapshot(
                                    response,
                                ))) => {
                                    match crate::mesh::cbor::encode(&response) {
                                        Ok(bytes) => {
                                            if let Err(e) = qm_events
                                                .send_ufp2_to_peer(
                                                    &from_node_id,
                                                    tentaflow_protocol::mesh::MESH_MSG_SYNC_SNAPSHOT_RESPONSE,
                                                    &bytes,
                                                )
                                                .await
                                            {
                                                warn!(peer = %from_node_id, "SyncSnapshotResponse send failed: {}", e);
                                            }
                                        }
                                        Err(e) => {
                                            warn!(peer = %from_node_id, "SyncSnapshotResponse encode error: {}", e)
                                        }
                                    }
                                }
                                Ok(None) => {}
                                Err(e) => {
                                    warn!(peer = %from_node_id, "SyncPull handle failed: {}", e)
                                }
                            }
                        }
                        Err(e) => warn!(peer = %from_node_id, "SyncPull decode error: {}", e),
                    }
                }
                Ok(IrohMeshEvent::SyncPullResponseReceived { from_node_id, data }) => {
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&from_node_id),
                        None => false,
                    };
                    if !is_trusted {
                        debug!(peer = %from_node_id, "SyncPullResponse od niezaufanego — ignoruje");
                        continue;
                    }
                    match crate::mesh::cbor::decode::<
                        tentaflow_protocol::mesh::MeshSyncPullResponsePayload,
                    >(&data)
                    {
                        Ok(payload) => match crate::sync::runtime::handle_pull_response_payload(
                            &from_node_id,
                            payload,
                        ) {
                            Ok(Some(ack)) => match crate::mesh::cbor::encode(&ack) {
                                Ok(bytes) => {
                                    if let Err(e) = qm_events
                                        .send_ufp2_to_peer(
                                            &from_node_id,
                                            tentaflow_protocol::mesh::MESH_MSG_SYNC_ACK,
                                            &bytes,
                                        )
                                        .await
                                    {
                                        warn!(peer = %from_node_id, "SyncAck send failed: {}", e);
                                    }
                                }
                                Err(e) => {
                                    warn!(peer = %from_node_id, "SyncAck encode error: {}", e)
                                }
                            },
                            Ok(None) => {}
                            Err(e) => {
                                warn!(peer = %from_node_id, "SyncPullResponse handle failed: {}", e)
                            }
                        },
                        Err(e) => {
                            warn!(peer = %from_node_id, "SyncPullResponse decode error: {}", e)
                        }
                    }
                    spawn_epoch_reconcile_adopts(&qm_events);
                }
                Ok(IrohMeshEvent::SyncSnapshotPullReceived { from_node_id, data }) => {
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&from_node_id),
                        None => false,
                    };
                    if !is_trusted {
                        debug!(peer = %from_node_id, "SyncSnapshotPull od niezaufanego — ignoruje");
                        continue;
                    }
                    match crate::mesh::cbor::decode::<
                        tentaflow_protocol::mesh::MeshSyncSnapshotPullPayload,
                    >(&data)
                    {
                        Ok(payload) => {
                            match crate::sync::runtime::handle_snapshot_pull_payload(
                                &from_node_id,
                                payload,
                            ) {
                                Ok(Some(response)) => {
                                    match crate::mesh::cbor::encode(&response) {
                                        Ok(bytes) => {
                                            if let Err(e) = qm_events
                                                .send_ufp2_to_peer(
                                                    &from_node_id,
                                                    tentaflow_protocol::mesh::MESH_MSG_SYNC_SNAPSHOT_RESPONSE,
                                                    &bytes,
                                                )
                                                .await
                                            {
                                                warn!(peer = %from_node_id, "SyncSnapshotResponse send failed: {}", e);
                                            }
                                        }
                                        Err(e) => {
                                            warn!(peer = %from_node_id, "SyncSnapshotResponse encode error: {}", e)
                                        }
                                    }
                                }
                                Ok(None) => {}
                                Err(e) => {
                                    warn!(peer = %from_node_id, "SyncSnapshotPull handle failed: {}", e)
                                }
                            }
                        }
                        Err(e) => {
                            warn!(peer = %from_node_id, "SyncSnapshotPull decode error: {}", e)
                        }
                    }
                }
                Ok(IrohMeshEvent::SyncSnapshotResponseReceived { from_node_id, data }) => {
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&from_node_id),
                        None => false,
                    };
                    if !is_trusted {
                        debug!(peer = %from_node_id, "SyncSnapshotResponse od niezaufanego — ignoruje");
                        continue;
                    }
                    match crate::mesh::cbor::decode::<
                        tentaflow_protocol::mesh::MeshSyncSnapshotResponsePayload,
                    >(&data)
                    {
                        Ok(payload) => {
                            match crate::sync::runtime::handle_snapshot_response_payload(
                                &from_node_id,
                                payload,
                            ) {
                                Ok(Some(ack)) => match crate::mesh::cbor::encode(&ack) {
                                    Ok(bytes) => {
                                        if let Err(e) = qm_events
                                            .send_ufp2_to_peer(
                                                &from_node_id,
                                                tentaflow_protocol::mesh::MESH_MSG_SYNC_ACK,
                                                &bytes,
                                            )
                                            .await
                                        {
                                            warn!(peer = %from_node_id, "SyncAck send failed: {}", e);
                                        }
                                    }
                                    Err(e) => {
                                        warn!(peer = %from_node_id, "SyncAck encode error: {}", e)
                                    }
                                },
                                Ok(None) => {}
                                Err(e) => {
                                    warn!(peer = %from_node_id, "SyncSnapshotResponse handle failed: {}", e)
                                }
                            }
                        }
                        Err(e) => {
                            warn!(peer = %from_node_id, "SyncSnapshotResponse decode error: {}", e)
                        }
                    }
                }
                Ok(IrohMeshEvent::FrameProxyRequestReceived {
                    from_node_id,
                    payload,
                }) => {
                    // Trust gate — only peers we have completed pairing
                    // with may pull frame bytes out of our LRU. Mirrors
                    // the gate applied to ServicesAnnounce / KeysSync.
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&from_node_id),
                        None => false,
                    };
                    if !is_trusted {
                        debug!(
                            peer = %from_node_id,
                            request_id = %payload.request_id,
                            "FrameProxyRequest from untrusted peer — dropped"
                        );
                        continue;
                    }
                    let iroh = qm_events.clone();
                    tokio::spawn(crate::services::frame_proxy::handle_request(
                        iroh,
                        from_node_id,
                        payload,
                    ));
                }
                Ok(IrohMeshEvent::FrameProxyResponseReceived {
                    from_node_id,
                    payload,
                }) => {
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&from_node_id),
                        None => false,
                    };
                    if !is_trusted {
                        debug!(
                            peer = %from_node_id,
                            "FrameProxyResponse from untrusted peer — dropped"
                        );
                        continue;
                    }
                    crate::services::frame_proxy::frame_proxy_client().handle_response(payload);
                }
                Ok(IrohMeshEvent::StorageProxyRequestReceived {
                    from_node_id,
                    payload,
                }) => {
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&from_node_id),
                        None => false,
                    };
                    if !is_trusted {
                        debug!(
                            peer = %from_node_id,
                            request_id = %payload.request_id,
                            "StorageProxyRequest from untrusted peer — dropped"
                        );
                        continue;
                    }
                    let Some(db) = db_pool.clone() else {
                        debug!(
                            peer = %from_node_id,
                            request_id = %payload.request_id,
                            "StorageProxyRequest without DB — dropped"
                        );
                        continue;
                    };
                    let iroh = qm_events.clone();
                    let local_node_id = local_node_id.clone();
                    tokio::spawn(crate::services::storage_proxy::handle_request(
                        iroh,
                        db,
                        local_node_id,
                        from_node_id,
                        payload,
                    ));
                }
                Ok(IrohMeshEvent::StorageProxyResponseReceived {
                    from_node_id,
                    payload,
                }) => {
                    let is_trusted = match &mesh_security {
                        Some(sec) => sec.is_trusted(&from_node_id),
                        None => false,
                    };
                    if !is_trusted {
                        debug!(
                            peer = %from_node_id,
                            "StorageProxyResponse from untrusted peer — dropped"
                        );
                        continue;
                    }
                    crate::services::storage_proxy::storage_proxy_client().handle_response(payload);
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

/// Dedicated robot-discovery advertiser. Runs OFF the heartbeat sender's critical
/// path on its own ~10 s interval so a slow/hung robot status read can never delay
/// mesh heartbeats. Each tick refreshes this node's local advertisement (only
/// PHYSICALLY CONNECTED robots, each status read bounded by `STATUS_CALL_TIMEOUT`)
/// and broadcasts to trusted peers when the advertised SET changed since the last
/// broadcast — so a remote node discovers a freshly-connected robot within ~10 s.
/// Change detection is order-insensitive (`sort_advertised`), avoiding a rebroadcast
/// storm when the set is steady. A periodic anti-drift FULL broadcast every ~5 min
/// repairs registry drift after a dropped delta or a peer that joined without
/// pull-on-connect. Trusted-only + identity-bound send semantics are identical to
/// the heartbeat broadcast (`broadcast_ufp2_to_trusted`).
fn spawn_robot_advertiser(
    quic_mesh: Arc<IrohMeshManager>,
    local_node_id: String,
    db_pool: crate::db::DbPool,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(10));
        let mut tick_count: u64 = 0;
        let mut last_broadcast_robots: Vec<crate::mesh::robot_dispatch::AdvertisedRobot> =
            Vec::new();
        loop {
            interval.tick().await;
            tick_count += 1;
            let robots = crate::mesh::robot_dispatch::sort_advertised(
                crate::mesh::robot_dispatch::refresh_local_advertisement(&db_pool, &local_node_id)
                    .await,
            );
            // Anti-drift FULL broadcast every ~5 min (30 ticks of 10 s): repairs
            // any drift left by a dropped UPDATE delta or a peer that joined
            // without the pull-on-connect path.
            let anti_drift = tick_count % 30 == 0;
            if anti_drift {
                let payload = crate::mesh::robot_dispatch::RobotsAnnouncePayload {
                    from_node_id: local_node_id.clone(),
                    robots: robots.clone(),
                };
                if let Ok(bytes) = crate::mesh::cbor::encode(&payload) {
                    let _ = quic_mesh
                        .broadcast_ufp2_to_trusted(
                            tentaflow_protocol::mesh::MESH_MSG_ROBOTS_ANNOUNCE,
                            &bytes,
                            None,
                        )
                        .await;
                }
            } else {
                // Steady state: push only the minimal delta vs the last broadcast
                // set so peers update incrementally without a full snapshot each
                // change. The keyed diff is order-insensitive, so a stable set
                // produces zero changes (no spurious traffic).
                let changes =
                    crate::mesh::robot_dispatch::diff_advertised(&last_broadcast_robots, &robots);
                for change in changes {
                    let payload = crate::mesh::robot_dispatch::RobotsUpdatePayload {
                        from_node_id: local_node_id.clone(),
                        change,
                    };
                    if let Ok(bytes) = crate::mesh::cbor::encode(&payload) {
                        let _ = quic_mesh
                            .broadcast_ufp2_to_trusted(
                                tentaflow_protocol::mesh::MESH_MSG_ROBOTS_UPDATE,
                                &bytes,
                                None,
                            )
                            .await;
                    }
                }
            }
            // Record the broadcast set (including empty) so the next tick diffs
            // against it. Storing the empty set lets an "all robots went offline"
            // transition emit Removed deltas once, then go quiet.
            last_broadcast_robots = robots;
        }
    });
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
                if let Ok(data) = crate::mesh::cbor::encode(&hb) {
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
                                if let Ok(bytes) = crate::mesh::cbor::encode(&payload) {
                                    let _ = quic_mesh
                                        .broadcast_ufp2_to_trusted(
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
                    if let Ok(data) = crate::mesh::cbor::encode(&sync) {
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
                    if let Ok(bv) = crate::mesh::cbor::encode(&payload) {
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
                        let name_map = crate::mesh::network_interfaces::ipv4_name_map();
                        crate::mesh::network_interfaces::filter_advertise_ips(
                            &raw, &filters, &kind_map, &name_map,
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

/// Parses a SQLite `datetime('now')` timestamp (`YYYY-MM-DD HH:MM:SS`, UTC) or an
/// RFC3339 string into a UNIX epoch in milliseconds. Used as the activity floor for
/// trust-expiry: a freshly paired peer that never connected has only `approved_at`,
/// so it must not be pruned before the TTL elapses since pairing.
fn approved_at_to_ms(approved_at: &str) -> Option<i64> {
    let trimmed = approved_at.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        return Some(dt.timestamp_millis());
    }
    chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S")
        .ok()
        .map(|naive| naive.and_utc().timestamp_millis())
}

/// Time-based trust expiry. Walks the trusted-node set and auto-removes any peer that
/// has neither connected nor been (re)paired within `trust_expiry_days`. This self-cleans
/// dead identities: a wiped/re-provisioned node gets a NEW ed25519 key, so its OLD identity
/// would otherwise sit `trusted` forever and the mesh would burn reconnect cycles dialing it.
///
/// A peer that is currently connected, or whose persisted `last_seen_ms` (last successful
/// connection, survives restart) is within the TTL, is NEVER pruned — only long-unreachable
/// identities are removed. `trust_expiry_days == 0` disables the prune entirely.
fn spawn_trust_expiry_prune(
    qm: Arc<IrohMeshManager>,
    peer_store: MeshPeerStore,
    mesh_security: Arc<MeshSecurity>,
    trust_expiry_days: u64,
) {
    if trust_expiry_days == 0 {
        info!("trust-expiry prune wylaczony (trust_expiry_days=0)");
        return;
    }
    tokio::spawn(async move {
        let ttl_ms = (trust_expiry_days as i64).saturating_mul(86_400_000);
        // Daily cadence is far finer than a 30-day TTL, so the check is cheap and a node
        // crossing the threshold is removed within a day without spamming the DB/log.
        let mut interval = tokio::time::interval(Duration::from_secs(24 * 60 * 60));
        loop {
            interval.tick().await;
            let trusted = match crate::db::repository::list_trusted_nodes(&mesh_security.db) {
                Ok(t) => t,
                Err(e) => {
                    warn!("trust-expiry prune: nie udalo sie odczytac trusted_nodes: {}", e);
                    continue;
                }
            };
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            for node in &trusted {
                // Reachable peers are never candidates — guards against revoking a healthy
                // node whose persisted last_seen happens to lag (bucketed every 30s).
                if qm.is_connected(&node.node_id).await {
                    continue;
                }
                let last_seen = crate::db::repository::get_peer_last_seen_ms(
                    &mesh_security.db,
                    &node.node_id,
                )
                .ok()
                .flatten()
                .unwrap_or(0);
                // Floor activity at pairing time so a just-paired peer that has not yet
                // connected is not pruned before the TTL elapses.
                let approved_ms = approved_at_to_ms(&node.approved_at).unwrap_or(0);
                let last_activity = last_seen.max(approved_ms);
                if last_activity == 0 || now_ms.saturating_sub(last_activity) <= ttl_ms {
                    continue;
                }

                let idle_days = now_ms.saturating_sub(last_activity) / 86_400_000;
                info!(
                    peer = %node.node_id,
                    idle_days,
                    "trust-expiry prune: usuwam martwa zaufana tozsamosc (brak polaczenia w oknie TTL)"
                );
                let _ = crate::db::repository::log_audit(
                    &mesh_security.db,
                    None,
                    None,
                    "trust_expired",
                    None,
                    Some(&format!(
                        "Auto-revoke zaufania dla {} — brak polaczenia od {} dni",
                        node.node_id, idle_days
                    )),
                    None,
                    Some(&node.node_id),
                );
                // Drop trusted_nodes row + in-memory keys + rebuild snapshot.
                if let Err(e) = mesh_security.unpair(&node.node_id) {
                    warn!(peer = %node.node_id, "trust-expiry prune: unpair nieudany: {}", e);
                    continue;
                }
                // Remove from the sync target set (sync_nodes filtered on trust_status).
                let _ = crate::db::repository::delete_sync_node(&mesh_security.db, &node.node_id);
                // Clear persisted contact hints + peer_persisted row, so the reconnect
                // manager stops dialing the dead identity.
                let _ = crate::net::iroh::pairing::delete_trusted_contact_hints(
                    &mesh_security.db,
                    &node.node_id,
                );
                // Forget the in-memory registry entry so liveness/reconnect drop it now.
                if let Some(registry) = peer_store.registry() {
                    if let Ok(id_bytes) = hex_to_node_id(&node.node_id) {
                        registry.forget(&id_bytes);
                    }
                }
                peer_store.remove(&node.node_id);
            }
        }
    });
}

/// Decodes a hex node_id into the 32-byte key shape the peer registry uses.
fn hex_to_node_id(node_id_hex: &str) -> Result<[u8; 32]> {
    let mut out = [0u8; 32];
    hex::decode_to_slice(node_id_hex, &mut out)
        .map_err(|e| anyhow::anyhow!("invalid node_id hex: {e}"))?;
    Ok(out)
}

fn spawn_sync_repair_scheduler(qm: Arc<IrohMeshManager>, mesh_security: Arc<MeshSecurity>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            interval.tick().await;
            run_sync_repair_scheduler_tick_with(
                qm.as_ref(),
                mesh_security.as_ref(),
                |peer_id| crate::sync::runtime::build_push_payload_for_target(peer_id, 128),
                |peer_id| {
                    crate::sync::runtime::build_repair_pull_payloads_for_peer(peer_id, 16, 256)
                },
            )
            .await;
        }
    });
}

pub(crate) async fn run_sync_repair_scheduler_tick_with<BuildPush, BuildRepairs>(
    qm: &IrohMeshManager,
    mesh_security: &MeshSecurity,
    mut build_push: BuildPush,
    mut build_repairs: BuildRepairs,
) where
    BuildPush: FnMut(
        &str,
    ) -> crate::sync::ledger::LedgerResult<
        Option<tentaflow_protocol::mesh::MeshSyncPushPayload>,
    >,
    BuildRepairs: FnMut(
        &str,
    ) -> crate::sync::ledger::LedgerResult<
        Vec<tentaflow_protocol::mesh::MeshSyncPullPayload>,
    >,
{
    // Convert pending core write-captures into outbox operations before the
    // per-peer push below reads the outbox, so core writes made while the process
    // is running (e.g. a Flow saved in the Flow Builder) propagate within one tick
    // instead of waiting for the next restart's startup drain. SQL/KV/blob
    // captures publish immediately after commit, so they are intentionally not
    // drained here to avoid double-emitting the same operation.
    if let Err(e) = crate::sync::runtime::drain_pending_core_captures_online(256) {
        warn!("sync repair: core capture drain failed: {}", e);
    }
    // Re-enqueue already-minted ops to receivers that gained access after the mint.
    // A grant only bumps the permission epoch; without this the receiver — which may
    // hold the position as a partition-less redacted placeholder — could never pull
    // the full op. Runs before the per-peer push below so the backfilled entries ship
    // in the same tick. Cheap when no epoch advanced.
    match crate::sync::runtime::backfill_outbox_for_permission_grants() {
        Ok(Some(count)) if count > 0 => {
            debug!("sync repair: permission backfill re-enqueued {} outbox entries", count);
        }
        Ok(_) => {}
        Err(e) => warn!("sync repair: permission backfill failed: {}", e),
    }
    let peers = qm.connected_peers().await;
    for peer_id in peers {
        if !mesh_security.is_trusted(&peer_id) {
            continue;
        }
        match build_push(&peer_id) {
            Ok(Some(payload)) => match crate::mesh::cbor::encode(&payload) {
                Ok(bytes) => {
                    if let Err(e) = qm
                        .send_ufp2_to_peer(
                            &peer_id,
                            tentaflow_protocol::mesh::MESH_MSG_SYNC_PUSH,
                            &bytes,
                        )
                        .await
                    {
                        debug!(peer = %peer_id, "SyncPush retry send failed: {}", e);
                    }
                }
                Err(e) => warn!(peer = %peer_id, "SyncPush retry encode error: {}", e),
            },
            Ok(None) => {}
            Err(e) => warn!(peer = %peer_id, "SyncPush retry build failed: {}", e),
        }

        match build_repairs(&peer_id) {
            Ok(payloads) => {
                for payload in payloads {
                    match crate::mesh::cbor::encode(&payload) {
                        Ok(bytes) => {
                            if let Err(e) = qm
                                .send_ufp2_to_peer(
                                    &peer_id,
                                    tentaflow_protocol::mesh::MESH_MSG_SYNC_PULL,
                                    &bytes,
                                )
                                .await
                            {
                                debug!(peer = %peer_id, "SyncPull repair send failed: {}", e);
                            }
                        }
                        Err(e) => warn!(peer = %peer_id, "SyncPull repair encode error: {}", e),
                    }
                }
            }
            Err(e) => warn!(peer = %peer_id, "SyncPull repair build failed: {}", e),
        }
    }
}
