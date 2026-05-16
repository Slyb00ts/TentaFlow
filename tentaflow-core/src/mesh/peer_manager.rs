// =============================================================================
// Plik: mesh/peer_manager.rs
// Opis: Centralny punkt zarzadzania peerami w mesh — laczy gossip, CRDT i discovery.
//       Utrzymuje pelna mape peerow, emituje zdarzenia i prowadzi maintenance loop.
// =============================================================================

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::config::MeshConfig;
use crate::error::CoreError;
use crate::mesh::crdt::{CrdtOperation, CrdtState, LamportClock};
use crate::mesh::gossip::{GossipEngine, GossipEvent, PeerState};
use crate::mesh::iroh_manager::{IrohMeshEvent, IrohMeshManager};
use crate::mesh::peer_store::{PeerContainerInfo, PeerGpuInfo};

// Progi czasowe dla maintenance loop
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(5);
const SUSPECT_THRESHOLD: Duration = Duration::from_secs(10);
const DEAD_THRESHOLD: Duration = Duration::from_secs(30);

/// Rola wezla w sieci mesh
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeRole {
    Router,
    Desktop,
    Mobile,
}

impl NodeRole {
    /// Parsuj role z tekstu (kompatybilne z polem `role` w GossipConfig)
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "desktop" => Self::Desktop,
            "mobile" => Self::Mobile,
            _ => Self::Router,
        }
    }
}

impl std::fmt::Display for NodeRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Router => write!(f, "router"),
            Self::Desktop => write!(f, "desktop"),
            Self::Mobile => write!(f, "mobile"),
        }
    }
}

/// Metryki zasobow hosta (CPU, RAM, GPU)
#[derive(Debug, Clone)]
pub struct HostMetrics {
    pub cpu_percent: f32,
    pub ram_used_mb: u64,
    pub ram_total_mb: u64,
    pub gpu_metrics: Vec<PeerGpuInfo>,
    pub load_avg: f32,
    pub active_requests: u32,
    pub updated_at: Instant,
}

/// Informacja o modelu AI zaladowanym na peerze
#[derive(Debug, Clone)]
pub struct PeerModelInfo {
    pub name: String,
    pub size_bytes: u64,
    pub backend: String,
    pub max_context: u32,
    pub quantization: String,
}

/// Pelna informacja o peerze w mesh
#[derive(Debug, Clone)]
pub struct MeshPeer {
    pub node_id: String,
    pub addresses: Vec<SocketAddr>,
    pub role: NodeRole,
    pub capabilities: Vec<String>,
    pub state: PeerState,
    pub last_heartbeat: Instant,
    pub latency_ms: Option<u64>,
    pub services: Vec<String>,
    pub quic_connected: bool,
    pub host_metrics: Option<HostMetrics>,
    pub models: Vec<PeerModelInfo>,
    pub containers: Vec<PeerContainerInfo>,
}

/// Zdarzenie w sieci mesh
#[derive(Debug, Clone)]
pub enum MeshEvent {
    PeerJoined(String),
    PeerLeft(String),
    PeerSuspect(String),
    PeerUpdated(String),
    StateSync { from: String, operations: usize },
    QuicConnected(String),
    QuicDisconnected(String),
    HeartbeatReceived { from: String },
    ModelsUpdated { node_id: String, count: usize },
    ContainersUpdated { node_id: String, count: usize },
}

/// Centralny menedzer peerow — laczy gossip, CRDT, QUIC i discovery
pub struct PeerManager {
    node_id: String,
    gossip: Arc<GossipEngine>,
    crdt_state: Arc<RwLock<CrdtState>>,
    known_peers: Arc<RwLock<HashMap<String, MeshPeer>>>,
    event_tx: broadcast::Sender<MeshEvent>,
    clock: Arc<RwLock<LamportClock>>,
    _config: MeshConfig,
    quic_mesh: RwLock<Option<Arc<IrohMeshManager>>>,
}

impl PeerManager {
    /// Tworzy nowy PeerManager
    pub fn new(node_id: String, gossip: Arc<GossipEngine>, config: MeshConfig) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        let clock = LamportClock::new(&node_id);

        Self {
            node_id,
            gossip,
            crdt_state: Arc::new(RwLock::new(CrdtState::new())),
            known_peers: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
            clock: Arc::new(RwLock::new(clock)),
            _config: config,
            quic_mesh: RwLock::new(None),
        }
    }

    /// Dodaje peera odkrytego przez mDNS lub statyczna konfiguracje
    pub fn add_peer(
        &self,
        node_id: String,
        address: SocketAddr,
        role: &str,
        services: Vec<String>,
        capabilities: Vec<String>,
    ) -> Result<(), CoreError> {
        if node_id == self.node_id {
            return Ok(());
        }

        let peer = MeshPeer {
            node_id: node_id.clone(),
            addresses: vec![address],
            role: NodeRole::from_str(role),
            capabilities,
            state: PeerState::Alive,
            last_heartbeat: Instant::now(),
            latency_ms: None,
            services,
            quic_connected: false,
            host_metrics: None,
            models: vec![],
            containers: vec![],
        };

        let is_new = {
            let mut peers = self.known_peers.write();
            let existed = peers.contains_key(&node_id);
            peers.insert(node_id.clone(), peer);
            !existed
        };

        if is_new {
            info!(peer = %node_id, "Nowy peer dolaczyl do mesh");
            let _ = self.event_tx.send(MeshEvent::PeerJoined(node_id));
        } else {
            debug!(peer = %node_id, "Zaktualizowano istniejacego peera");
            let _ = self.event_tx.send(MeshEvent::PeerUpdated(node_id));
        }

        Ok(())
    }

    /// Usuwa peera z mapy
    pub fn remove_peer(&self, node_id: &str) {
        let removed = {
            let mut peers = self.known_peers.write();
            peers.remove(node_id).is_some()
        };

        if removed {
            info!(peer = %node_id, "Peer usuniety z mesh");
            let _ = self.event_tx.send(MeshEvent::PeerLeft(node_id.to_string()));
        }
    }

    /// Pobiera kopie peera po node_id
    pub fn get_peer(&self, node_id: &str) -> Option<MeshPeer> {
        let peers = self.known_peers.read();
        peers.get(node_id).cloned()
    }

    /// Zwraca liste wszystkich znanych peerow
    pub fn all_peers(&self) -> Vec<MeshPeer> {
        let peers = self.known_peers.read();
        peers.values().cloned().collect()
    }

    /// Filtruje peerow po capability (np. "llm", "tts", "embedding")
    pub fn peers_with_capability(&self, capability: &str) -> Vec<MeshPeer> {
        let peers = self.known_peers.read();
        peers
            .values()
            .filter(|p| {
                p.state == PeerState::Alive && p.capabilities.iter().any(|c| c == capability)
            })
            .cloned()
            .collect()
    }

    /// Filtruje peerow po nazwie serwisu AI
    pub fn peers_with_service(&self, service_name: &str) -> Vec<MeshPeer> {
        let peers = self.known_peers.read();
        peers
            .values()
            .filter(|p| p.state == PeerState::Alive && p.services.iter().any(|s| s == service_name))
            .cloned()
            .collect()
    }

    /// Aplikuje operacje CRDT otrzymane od innego peera
    pub fn sync_state(
        &self,
        from_node: &str,
        operations: Vec<CrdtOperation>,
    ) -> Result<(), CoreError> {
        let ops_count = operations.len();
        if ops_count == 0 {
            return Ok(());
        }

        {
            let mut state = self.crdt_state.write();
            let mut clock = self.clock.write();

            for op in &operations {
                clock.merge(op.clock());
            }

            let remote_state = CrdtState {
                operations_log: operations,
                version_vector: HashMap::new(),
            };
            state.merge(&remote_state);
        }

        info!(
            from = %from_node,
            count = ops_count,
            "Zsynchronizowano stan CRDT"
        );
        let _ = self.event_tx.send(MeshEvent::StateSync {
            from: from_node.to_string(),
            operations: ops_count,
        });

        Ok(())
    }

    /// Zwraca delta operacji CRDT od podanego zegara (do wyslania innemu peerowi)
    pub fn get_delta(&self, since: LamportClock) -> Vec<CrdtOperation> {
        let state = self.crdt_state.read();
        state
            .operations_log
            .iter()
            .filter(|op| *op.clock() > since)
            .cloned()
            .collect()
    }

    /// Aplikuje lokalna operacje CRDT i zwraca aktualny zegar
    pub fn apply_local_operation(&self, op: CrdtOperation) -> LamportClock {
        let mut state = self.crdt_state.write();
        let mut clock = self.clock.write();
        let ts = clock.tick();
        state.apply(op);
        ts
    }

    /// Zwraca kopie version vector CRDT (do delta sync)
    pub fn version_vector(&self) -> HashMap<u64, u64> {
        let state = self.crdt_state.read();
        state.version_vector.clone()
    }

    /// Kompaktuje log operacji CRDT
    pub fn compact_crdt(&self) {
        let mut state = self.crdt_state.write();
        let before = state.operations_log.len();
        state.compact();
        let after = state.operations_log.len();
        if before != after {
            debug!(before, after, "Skompaktowano log CRDT");
        }
    }

    /// Subskrypcja zdarzen mesh
    pub fn subscribe(&self) -> broadcast::Receiver<MeshEvent> {
        self.event_tx.subscribe()
    }

    /// Zwraca node_id tego wezla
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Zwraca referencje do gossip engine
    pub fn gossip(&self) -> &Arc<GossipEngine> {
        &self.gossip
    }

    /// Ustawia referencje do IrohMeshManager
    pub fn set_quic_mesh(&self, qm: Arc<IrohMeshManager>) {
        *self.quic_mesh.write() = Some(qm);
    }

    /// Aktualizuje metryki peera na podstawie heartbeatu
    pub fn update_peer_metrics(
        &self,
        node_id: &str,
        cpu: f32,
        ram_used: u64,
        ram_total: u64,
        gpus: Vec<PeerGpuInfo>,
        load: f32,
        active_reqs: u32,
    ) {
        let mut peers = self.known_peers.write();
        if let Some(peer) = peers.get_mut(node_id) {
            peer.host_metrics = Some(HostMetrics {
                cpu_percent: cpu,
                ram_used_mb: ram_used,
                ram_total_mb: ram_total,
                gpu_metrics: gpus,
                load_avg: load,
                active_requests: active_reqs,
                updated_at: Instant::now(),
            });
            peer.last_heartbeat = Instant::now();
        }
    }

    /// Aktualizuje liste modeli peera
    pub fn update_peer_models(&self, node_id: &str, models: Vec<PeerModelInfo>) {
        let count = models.len();
        let mut peers = self.known_peers.write();
        if let Some(peer) = peers.get_mut(node_id) {
            peer.models = models;
        }
        drop(peers);
        let _ = self.event_tx.send(MeshEvent::ModelsUpdated {
            node_id: node_id.to_string(),
            count,
        });
    }

    /// Aktualizuje liste kontenerow peera
    pub fn update_peer_containers(&self, node_id: &str, containers: Vec<PeerContainerInfo>) {
        let count = containers.len();
        let mut peers = self.known_peers.write();
        if let Some(peer) = peers.get_mut(node_id) {
            peer.containers = containers;
        }
        drop(peers);
        let _ = self.event_tx.send(MeshEvent::ContainersUpdated {
            node_id: node_id.to_string(),
            count,
        });
    }

    /// Oznacza peera jako polaczonego/rozlaczonego przez QUIC
    pub fn set_quic_connected(&self, node_id: &str, connected: bool) {
        let mut peers = self.known_peers.write();
        if let Some(peer) = peers.get_mut(node_id) {
            peer.quic_connected = connected;
        }
    }

    /// Znajduje najlepszy node dla danego modelu (najnizsze obciazenie).
    /// Score = cpu_percent + (srednie_gpu_percent * 2) — GPU wazniejsze.
    pub fn best_node_for_model(&self, model_name: &str) -> Option<(String, SocketAddr)> {
        let peers = self.known_peers.read();
        peers
            .values()
            .filter(|p| {
                p.state == PeerState::Alive
                    && p.quic_connected
                    && p.models.iter().any(|m| m.name == model_name)
            })
            .filter_map(|p| {
                let addr = p.addresses.first().copied()?;
                let score = Self::compute_load_score(p);
                Some((p.node_id.clone(), addr, score))
            })
            .min_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(id, addr, _)| (id, addr))
    }

    /// Znajduje najlepszy node z dana capability (np. "tts", "embedding")
    pub fn best_node_for_capability(&self, capability: &str) -> Option<(String, SocketAddr)> {
        let peers = self.known_peers.read();
        peers
            .values()
            .filter(|p| {
                p.state == PeerState::Alive
                    && p.quic_connected
                    && p.capabilities.iter().any(|c| c == capability)
            })
            .filter_map(|p| {
                let addr = p.addresses.first().copied()?;
                let score = Self::compute_load_score(p);
                Some((p.node_id.clone(), addr, score))
            })
            .min_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(id, addr, _)| (id, addr))
    }

    /// Oblicza score obciazenia peera: cpu + (srednie gpu * 2)
    fn compute_load_score(peer: &MeshPeer) -> f32 {
        let metrics = match &peer.host_metrics {
            Some(m) => m,
            None => return f32::MAX,
        };

        let gpu_avg = if metrics.gpu_metrics.is_empty() {
            0.0
        } else {
            let sum: f32 = metrics.gpu_metrics.iter().map(|g| g.usage_percent).sum();
            sum / metrics.gpu_metrics.len() as f32
        };

        metrics.cpu_percent + (gpu_avg * 2.0)
    }

    /// Zwraca liczbe znanych peerow
    pub fn peer_count(&self) -> usize {
        let peers = self.known_peers.read();
        peers.len()
    }

    /// Zwraca liczbe zywych peerow
    pub fn alive_peer_count(&self) -> usize {
        let peers = self.known_peers.read();
        peers
            .values()
            .filter(|p| p.state == PeerState::Alive)
            .count()
    }

    /// Uruchamia background loop: nasluchuje gossip events, QUIC events i ogarnia maintenance
    pub fn start_maintenance_loop(self: Arc<Self>) -> JoinHandle<()> {
        let manager = Arc::clone(&self);
        tokio::spawn(async move {
            let mut gossip_rx = manager.gossip.subscribe();
            let mut maintenance_tick = tokio::time::interval(MAINTENANCE_INTERVAL);

            // Subskrypcja QUIC events (jesli IrohMeshManager jest ustawiony)
            let quic_rx = {
                let qm = manager.quic_mesh.read();
                qm.as_ref().map(|q| q.subscribe())
            };
            let mut quic_rx = quic_rx;

            loop {
                tokio::select! {
                    event = gossip_rx.recv() => {
                        match event {
                            Ok(gossip_event) => {
                                manager.handle_gossip_event(gossip_event);
                            }
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                warn!(skipped = n, "Gossip receiver opuscil wiadomosci");
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                info!("Gossip channel zamkniety — koniec maintenance loop");
                                break;
                            }
                        }
                    }
                    event = async {
                        match quic_rx.as_mut() {
                            Some(rx) => rx.recv().await,
                            None => std::future::pending().await,
                        }
                    } => {
                        match event {
                            Ok(quic_event) => {
                                manager.handle_quic_event(quic_event).await;
                            }
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                warn!(skipped = n, "QUIC receiver opuscil wiadomosci");
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                info!("QUIC channel zamkniety");
                                quic_rx = None;
                            }
                        }
                    }
                    _ = maintenance_tick.tick() => {
                        manager.run_maintenance();
                    }
                }
            }
        })
    }

    /// Obsluguje zdarzenie z IrohMeshManager
    async fn handle_quic_event(&self, event: IrohMeshEvent) {
        match event {
            IrohMeshEvent::PeerConnected { node_id } => {
                self.set_quic_connected(&node_id, true);
                let _ = self.event_tx.send(MeshEvent::QuicConnected(node_id));
            }
            IrohMeshEvent::PeerDisconnected { node_id } => {
                self.set_quic_connected(&node_id, false);
                let _ = self
                    .event_tx
                    .send(MeshEvent::QuicDisconnected(node_id.clone()));

                // Trigger reconnect jesli peer nadal zyje w gossip
                let should_reconnect = {
                    let peers = self.known_peers.read();
                    peers
                        .get(&node_id)
                        .map_or(false, |p| p.state == PeerState::Alive)
                };

                if should_reconnect {
                    // iroh sam wznowi polaczenie przez discovery+relay gdy peer wroci.
                    let _ = self.quic_mesh.read();
                    let _ = node_id;
                }
            }
            IrohMeshEvent::HeartbeatReceived { node_id, heartbeat } => {
                // Deserializuj heartbeat z rkyv
                if let Ok(archived) = rkyv::access::<
                    rkyv::Archived<tentaflow_protocol::mesh::MeshHeartbeat>,
                    rkyv::rancor::Error,
                >(&heartbeat)
                {
                    let gpus: Vec<PeerGpuInfo> = archived
                        .gpu_metrics
                        .iter()
                        .map(|g| PeerGpuInfo {
                            name: format!("GPU {}", u32::from(g.index)),
                            usage_percent: g.usage_percent.into(),
                            vram_used_mb: g.vram_used_mb.into(),
                            vram_total_mb: g.vram_total_mb.into(),
                            temperature_c: f32::from(g.temperature_c) as u32,
                            power_draw_w: None,
                            power_limit_w: None,
                            vendor: crate::mesh::peer_store::GpuVendor::Other,
                        })
                        .collect();

                    self.update_peer_metrics(
                        &node_id,
                        archived.cpu_usage_percent.into(),
                        archived.ram_used_mb.into(),
                        archived.ram_total_mb.into(),
                        gpus,
                        archived.load_avg_1m.into(),
                        archived.active_requests.into(),
                    );
                } else {
                    warn!(peer_id = %node_id, "Nie udalo sie zdeserializowac heartbeatu");
                }
                let _ = self
                    .event_tx
                    .send(MeshEvent::HeartbeatReceived { from: node_id });
            }
            IrohMeshEvent::FullStateReceived { node_id, state } => {
                // Deserializuj FullState z rkyv i zaktualizuj modele + kontenery + CRDT
                if let Ok(archived) = rkyv::access::<
                    rkyv::Archived<tentaflow_protocol::mesh::MeshFullState>,
                    rkyv::rancor::Error,
                >(&state)
                {
                    let models: Vec<PeerModelInfo> = archived
                        .models
                        .iter()
                        .map(|m| PeerModelInfo {
                            name: m.name.to_string(),
                            size_bytes: m.size_bytes.into(),
                            backend: m.backend.to_string(),
                            max_context: m.max_context.into(),
                            quantization: m.quantization.to_string(),
                        })
                        .collect();

                    let containers: Vec<PeerContainerInfo> = archived
                        .containers
                        .iter()
                        .map(|c| PeerContainerInfo {
                            id: c.id.to_string(),
                            name: c.name.to_string(),
                            image: c.image.to_string(),
                            status: c.status.to_string(),
                            cpu_percent: c.cpu_percent.into(),
                            memory_mb: c.memory_mb.into(),
                            memory_limit_mb: 0,
                        })
                        .collect();

                    self.update_peer_models(&node_id, models);
                    self.update_peer_containers(&node_id, containers);

                    // CRDT sync z version_vector
                    let crdt_ops: Vec<CrdtOperation> = archived
                        .crdt_operations
                        .iter()
                        .filter_map(|op| {
                            // Konwersja CrdtSyncOp -> CrdtOperation
                            let clock = LamportClock {
                                time: op.clock_time.into(),
                                node_id_hash: op.clock_node_hash.into(),
                            };
                            match &op.op_type {
                                rkyv::Archived::<tentaflow_protocol::mesh::CrdtOpType>::SetValue(v) => {
                                    Some(CrdtOperation::UpsertAlias {
                                        alias: op.key.to_string(),
                                        target: v.to_string(),
                                        clock,
                                    })
                                }
                                _ => None,
                            }
                        })
                        .collect();

                    if !crdt_ops.is_empty() {
                        let _ = self.sync_state(&node_id, crdt_ops);
                    }

                    info!(peer_id = %node_id, "Przetworzono FullState od peera");
                } else {
                    warn!(peer_id = %node_id, "Nie udalo sie zdeserializowac FullState");
                }
            }
            IrohMeshEvent::ModelListUpdate { node_id, data } => {
                if let Ok(models) = serde_json::from_slice::<Vec<serde_json::Value>>(&data) {
                    let peer_models: Vec<PeerModelInfo> = models
                        .iter()
                        .filter_map(|m| {
                            Some(PeerModelInfo {
                                name: m.get("name")?.as_str()?.to_string(),
                                size_bytes: m.get("size_bytes")?.as_u64()?,
                                backend: m.get("backend")?.as_str()?.to_string(),
                                max_context: m.get("max_context")?.as_u64()? as u32,
                                quantization: m.get("quantization")?.as_str()?.to_string(),
                            })
                        })
                        .collect();
                    self.update_peer_models(&node_id, peer_models);
                }
            }
            IrohMeshEvent::ContainerListUpdate { node_id, data } => {
                if let Ok(containers) = serde_json::from_slice::<Vec<serde_json::Value>>(&data) {
                    let peer_containers: Vec<PeerContainerInfo> = containers
                        .iter()
                        .filter_map(|c| {
                            Some(PeerContainerInfo {
                                id: c.get("id")?.as_str()?.to_string(),
                                name: c.get("name")?.as_str()?.to_string(),
                                image: c.get("image")?.as_str()?.to_string(),
                                status: c.get("status")?.as_str()?.to_string(),
                                cpu_percent: c.get("cpu_percent")?.as_f64()? as f32,
                                memory_mb: c.get("memory_mb")?.as_u64()?,
                                memory_limit_mb: c
                                    .get("memory_limit_mb")
                                    .and_then(|v| v.as_u64())
                                    .unwrap_or(0),
                            })
                        })
                        .collect();
                    self.update_peer_containers(&node_id, peer_containers);
                }
            }
            IrohMeshEvent::CrdtDeltaReceived { .. }
            | IrohMeshEvent::ForwardRequest { .. }
            | IrohMeshEvent::ForwardRequestReceived { .. }
            | IrohMeshEvent::NodeInfoReceived { .. }
            | IrohMeshEvent::PairingRequestReceived { .. }
            | IrohMeshEvent::PairingConfirmReceived { .. }
            | IrohMeshEvent::PairingRejectReceived { .. }
            | IrohMeshEvent::MeshCommandReceived { .. }
            | IrohMeshEvent::MeshCommandResponseReceived { .. }
            | IrohMeshEvent::MeshDeployProgressReceived { .. }
            | IrohMeshEvent::MeshLogChunkReceived { .. }
            | IrohMeshEvent::TrustRevokedReceived { .. }
            | IrohMeshEvent::KeyRotationReceived { .. }
            | IrohMeshEvent::KeyRotationResponseReceived { .. }
            | IrohMeshEvent::TrustedKeysSyncReceived { .. }
            | IrohMeshEvent::HmacKeysSyncReceived { .. }
            | IrohMeshEvent::FrameProxyRequestReceived { .. }
            | IrohMeshEvent::FrameProxyResponseReceived { .. }
            | IrohMeshEvent::NodeLeavingReceived { .. }
            | IrohMeshEvent::RelayFrameReceived { .. }
            | IrohMeshEvent::AliasSyncReceived { .. }
            | IrohMeshEvent::PeerDiscovered { .. }
            | IrohMeshEvent::HelloReceived { .. }
            | IrohMeshEvent::TopologyAnnounceReceived { .. }
            | IrohMeshEvent::KnownPeersReceived { .. }
            | IrohMeshEvent::ServicesGetReceived { .. }
            | IrohMeshEvent::ServicesGetResponseReceived { .. }
            | IrohMeshEvent::ServicesAnnounceReceived { .. }
            | IrohMeshEvent::ServicesUpdateReceived { .. } => {
                // Obslugiwane w pipeline.rs
            }
        }
    }

    /// Obsluguje zdarzenie z gossip engine i aktualizuje mape peerow
    fn handle_gossip_event(&self, event: GossipEvent) {
        match event {
            GossipEvent::Join(summary) => {
                let node_id = summary.node_id.clone();
                let addr = summary.address;
                let peer = MeshPeer {
                    node_id: node_id.clone(),
                    addresses: vec![addr],
                    role: NodeRole::from_str(&summary.role),
                    capabilities: Vec::new(),
                    state: PeerState::Alive,
                    last_heartbeat: Instant::now(),
                    latency_ms: None,
                    services: summary.services,
                    quic_connected: false,
                    host_metrics: None,
                    models: vec![],
                    containers: vec![],
                };

                let is_new = {
                    let mut peers = self.known_peers.write();
                    let existed = peers.contains_key(&node_id);
                    peers.insert(node_id.clone(), peer);
                    !existed
                };

                if is_new {
                    let _ = self.event_tx.send(MeshEvent::PeerJoined(node_id.clone()));

                    // Trigger polaczenie QUIC do nowego peera
                    let qm = self.quic_mesh.read();
                    if let Some(qm) = qm.as_ref() {
                        let qm = Arc::clone(qm);
                        let nid = node_id;
                        tokio::spawn(async move {
                            if let Err(e) = qm.connect_to_peer(&nid, addr).await {
                                warn!(peer_id = %nid, "Nie udalo sie nawiazac QUIC: {}", e);
                            }
                        });
                    }
                }
            }
            GossipEvent::Alive(summary) => {
                let mut peers = self.known_peers.write();
                if let Some(peer) = peers.get_mut(&summary.node_id) {
                    peer.state = PeerState::Alive;
                    peer.last_heartbeat = Instant::now();
                    if !summary.services.is_empty() {
                        peer.services = summary.services;
                    }
                }
            }
            GossipEvent::Suspect(node_id) => {
                let changed = {
                    let mut peers = self.known_peers.write();
                    if let Some(peer) = peers.get_mut(&node_id) {
                        if peer.state != PeerState::Suspect {
                            peer.state = PeerState::Suspect;
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                };

                if changed {
                    warn!(peer = %node_id, "Peer podejrzany");
                    let _ = self.event_tx.send(MeshEvent::PeerSuspect(node_id));
                }
            }
            GossipEvent::Leave(node_id) => {
                let removed = {
                    let mut peers = self.known_peers.write();
                    peers.remove(&node_id).is_some()
                };

                if removed {
                    info!(peer = %node_id, "Peer opuscil mesh (gossip leave)");

                    // Rozlacz QUIC
                    let qm = self.quic_mesh.read();
                    if let Some(qm) = qm.as_ref() {
                        let qm = Arc::clone(qm);
                        let nid = node_id.clone();
                        tokio::spawn(async move {
                            qm.disconnect_peer(&nid).await;
                        });
                    }

                    let _ = self.event_tx.send(MeshEvent::PeerLeft(node_id));
                }
            }
            GossipEvent::ServiceUpdate { node_id, services } => {
                let updated = {
                    let mut peers = self.known_peers.write();
                    if let Some(peer) = peers.get_mut(&node_id) {
                        peer.services = services;
                        peer.last_heartbeat = Instant::now();
                        true
                    } else {
                        false
                    }
                };

                if updated {
                    let _ = self.event_tx.send(MeshEvent::PeerUpdated(node_id));
                }
            }
        }
    }

    /// Okresowy przeglad peerow — oznacza suspect/dead na podstawie heartbeatow
    fn run_maintenance(&self) {
        let now = Instant::now();
        let mut suspects = Vec::new();
        let mut dead = Vec::new();

        {
            let mut peers = self.known_peers.write();
            for peer in peers.values_mut() {
                let elapsed = now.duration_since(peer.last_heartbeat);

                match peer.state {
                    PeerState::Alive if elapsed > SUSPECT_THRESHOLD => {
                        peer.state = PeerState::Suspect;
                        suspects.push(peer.node_id.clone());
                    }
                    PeerState::Suspect if elapsed > DEAD_THRESHOLD => {
                        peer.state = PeerState::Dead;
                        dead.push(peer.node_id.clone());
                    }
                    _ => {}
                }
            }

            for id in &dead {
                peers.remove(id);
            }
        }

        for id in suspects {
            warn!(peer = %id, "Peer oznaczony jako suspect (brak heartbeat)");
            let _ = self.event_tx.send(MeshEvent::PeerSuspect(id));
        }

        for id in dead {
            info!(peer = %id, "Peer uznany za dead — usuniety z mesh");
            let _ = self.event_tx.send(MeshEvent::PeerLeft(id));
        }
    }
}

// =============================================================================
// Testy
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::gossip::GossipConfig;

    fn make_manager() -> PeerManager {
        let gossip_config = GossipConfig::default();
        let gossip = Arc::new(GossipEngine::new(gossip_config));
        let mesh_config = MeshConfig {
            enabled: true,
            port: 8090,
            static_peers: vec![],
            mdns_enabled: false,
            dht_enabled: false,
            heartbeat_interval_ms: 500,
            peer_timeout_ms: 3000,
            cluster_name: "test".to_string(),
            iroh_relay_url: String::new(),
        };
        PeerManager::new("test-node".to_string(), gossip, mesh_config)
    }

    #[test]
    fn add_and_get_peer() {
        let mgr = make_manager();
        let addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

        mgr.add_peer(
            "peer-1".to_string(),
            addr,
            "router",
            vec!["llm-svc".to_string()],
            vec!["llm".to_string(), "embedding".to_string()],
        )
        .unwrap();

        let peer = mgr.get_peer("peer-1").unwrap();
        assert_eq!(peer.node_id, "peer-1");
        assert_eq!(peer.role, NodeRole::Router);
        assert_eq!(peer.capabilities.len(), 2);
        assert_eq!(peer.services, vec!["llm-svc"]);
        assert_eq!(peer.state, PeerState::Alive);
    }

    #[test]
    fn skip_self_as_peer() {
        let mgr = make_manager();
        let addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

        mgr.add_peer("test-node".to_string(), addr, "router", vec![], vec![])
            .unwrap();
        assert_eq!(mgr.peer_count(), 0);
    }

    #[test]
    fn remove_peer() {
        let mgr = make_manager();
        let addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();

        mgr.add_peer("peer-1".to_string(), addr, "router", vec![], vec![])
            .unwrap();
        assert_eq!(mgr.peer_count(), 1);

        mgr.remove_peer("peer-1");
        assert_eq!(mgr.peer_count(), 0);
        assert!(mgr.get_peer("peer-1").is_none());
    }

    #[test]
    fn all_peers_returns_all() {
        let mgr = make_manager();

        for i in 0..3 {
            let addr: SocketAddr = format!("127.0.0.1:500{i}").parse().unwrap();
            mgr.add_peer(format!("peer-{i}"), addr, "router", vec![], vec![])
                .unwrap();
        }

        assert_eq!(mgr.all_peers().len(), 3);
    }

    #[test]
    fn filter_by_capability() {
        let mgr = make_manager();
        let addr1: SocketAddr = "127.0.0.1:5001".parse().unwrap();
        let addr2: SocketAddr = "127.0.0.1:5002".parse().unwrap();

        mgr.add_peer(
            "peer-llm".to_string(),
            addr1,
            "router",
            vec![],
            vec!["llm".to_string()],
        )
        .unwrap();

        mgr.add_peer(
            "peer-tts".to_string(),
            addr2,
            "desktop",
            vec![],
            vec!["tts".to_string()],
        )
        .unwrap();

        let llm_peers = mgr.peers_with_capability("llm");
        assert_eq!(llm_peers.len(), 1);
        assert_eq!(llm_peers[0].node_id, "peer-llm");

        let tts_peers = mgr.peers_with_capability("tts");
        assert_eq!(tts_peers.len(), 1);
        assert_eq!(tts_peers[0].node_id, "peer-tts");

        let empty = mgr.peers_with_capability("nonexistent");
        assert!(empty.is_empty());
    }

    #[test]
    fn filter_by_service() {
        let mgr = make_manager();
        let addr: SocketAddr = "127.0.0.1:5001".parse().unwrap();

        mgr.add_peer(
            "peer-1".to_string(),
            addr,
            "router",
            vec!["gpt4-proxy".to_string(), "embedding-svc".to_string()],
            vec![],
        )
        .unwrap();

        let results = mgr.peers_with_service("gpt4-proxy");
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn crdt_sync_and_delta() {
        let mgr = make_manager();

        let op1 = CrdtOperation::UpsertService {
            id: 1,
            name: "svc-remote".to_string(),
            data_json: "{}".to_string(),
            clock: LamportClock {
                time: 5,
                node_id_hash: 999,
            },
        };
        let op2 = CrdtOperation::UpsertAlias {
            alias: "gpt4".to_string(),
            target: "openai/gpt-4".to_string(),
            clock: LamportClock {
                time: 6,
                node_id_hash: 999,
            },
        };

        mgr.sync_state("remote-node", vec![op1, op2]).unwrap();

        let delta = mgr.get_delta(LamportClock {
            time: 4,
            node_id_hash: 0,
        });
        assert_eq!(delta.len(), 2);

        let delta2 = mgr.get_delta(LamportClock {
            time: 6,
            node_id_hash: 999,
        });
        assert_eq!(delta2.len(), 0);
    }

    #[test]
    fn sync_empty_operations_is_noop() {
        let mgr = make_manager();
        mgr.sync_state("remote", vec![]).unwrap();
        assert!(mgr
            .get_delta(LamportClock {
                time: 0,
                node_id_hash: 0
            })
            .is_empty());
    }

    #[test]
    fn maintenance_marks_suspect_and_dead() {
        let mgr = make_manager();
        let addr: SocketAddr = "127.0.0.1:5001".parse().unwrap();

        {
            let mut peers = mgr.known_peers.write();
            peers.insert(
                "stale-peer".to_string(),
                MeshPeer {
                    node_id: "stale-peer".to_string(),
                    addresses: vec![addr],
                    role: NodeRole::Router,
                    capabilities: vec![],
                    state: PeerState::Alive,
                    last_heartbeat: Instant::now() - Duration::from_secs(15),
                    latency_ms: None,
                    services: vec![],
                    quic_connected: false,
                    host_metrics: None,
                    models: vec![],
                    containers: vec![],
                },
            );
        }

        mgr.run_maintenance();

        let peer = mgr.get_peer("stale-peer").unwrap();
        assert_eq!(peer.state, PeerState::Suspect);

        {
            let mut peers = mgr.known_peers.write();
            let p = peers.get_mut("stale-peer").unwrap();
            p.last_heartbeat = Instant::now() - Duration::from_secs(35);
            p.state = PeerState::Suspect;
        }

        mgr.run_maintenance();

        assert!(mgr.get_peer("stale-peer").is_none());
        assert_eq!(mgr.peer_count(), 0);
    }

    #[test]
    fn node_role_parsing() {
        assert_eq!(NodeRole::from_str("router"), NodeRole::Router);
        assert_eq!(NodeRole::from_str("Router"), NodeRole::Router);
        assert_eq!(NodeRole::from_str("desktop"), NodeRole::Desktop);
        assert_eq!(NodeRole::from_str("Desktop"), NodeRole::Desktop);
        assert_eq!(NodeRole::from_str("mobile"), NodeRole::Mobile);
        assert_eq!(NodeRole::from_str("unknown"), NodeRole::Router);
    }

    #[test]
    fn alive_peer_count() {
        let mgr = make_manager();
        let addr1: SocketAddr = "127.0.0.1:5001".parse().unwrap();
        let addr2: SocketAddr = "127.0.0.1:5002".parse().unwrap();

        mgr.add_peer("p1".to_string(), addr1, "router", vec![], vec![])
            .unwrap();
        mgr.add_peer("p2".to_string(), addr2, "router", vec![], vec![])
            .unwrap();

        assert_eq!(mgr.alive_peer_count(), 2);

        {
            let mut peers = mgr.known_peers.write();
            peers.get_mut("p1").unwrap().state = PeerState::Suspect;
        }

        assert_eq!(mgr.alive_peer_count(), 1);
        assert_eq!(mgr.peer_count(), 2);
    }

    #[test]
    fn subscribe_receives_events() {
        let mgr = make_manager();
        let mut rx = mgr.subscribe();
        let addr: SocketAddr = "127.0.0.1:5001".parse().unwrap();

        mgr.add_peer("peer-1".to_string(), addr, "router", vec![], vec![])
            .unwrap();

        let event = rx.try_recv().unwrap();
        assert!(matches!(event, MeshEvent::PeerJoined(id) if id == "peer-1"));
    }

    #[test]
    fn new_peer_has_default_quic_fields() {
        let mgr = make_manager();
        let addr: SocketAddr = "127.0.0.1:5001".parse().unwrap();

        mgr.add_peer("peer-1".to_string(), addr, "router", vec![], vec![])
            .unwrap();

        let peer = mgr.get_peer("peer-1").unwrap();
        assert!(!peer.quic_connected);
        assert!(peer.host_metrics.is_none());
        assert!(peer.models.is_empty());
        assert!(peer.containers.is_empty());
    }

    #[test]
    fn set_quic_connected_updates_peer() {
        let mgr = make_manager();
        let addr: SocketAddr = "127.0.0.1:5001".parse().unwrap();

        mgr.add_peer("peer-1".to_string(), addr, "router", vec![], vec![])
            .unwrap();
        assert!(!mgr.get_peer("peer-1").unwrap().quic_connected);

        mgr.set_quic_connected("peer-1", true);
        assert!(mgr.get_peer("peer-1").unwrap().quic_connected);

        mgr.set_quic_connected("peer-1", false);
        assert!(!mgr.get_peer("peer-1").unwrap().quic_connected);
    }

    #[test]
    fn update_peer_metrics() {
        let mgr = make_manager();
        let addr: SocketAddr = "127.0.0.1:5001".parse().unwrap();

        mgr.add_peer("peer-1".to_string(), addr, "router", vec![], vec![])
            .unwrap();

        let gpus = vec![PeerGpuInfo {
            name: "GPU 0".to_string(),
            usage_percent: 80.0,
            vram_used_mb: 20000,
            vram_total_mb: 24000,
            temperature_c: 70,
            power_draw_w: None,
            power_limit_w: None,
            vendor: crate::mesh::peer_store::GpuVendor::Other,
        }];

        mgr.update_peer_metrics("peer-1", 45.0, 8000, 16000, gpus, 2.5, 10);

        let peer = mgr.get_peer("peer-1").unwrap();
        let metrics = peer.host_metrics.unwrap();
        assert_eq!(metrics.cpu_percent, 45.0);
        assert_eq!(metrics.ram_used_mb, 8000);
        assert_eq!(metrics.gpu_metrics.len(), 1);
        assert_eq!(metrics.active_requests, 10);
    }

    #[test]
    fn update_peer_models_and_containers() {
        let mgr = make_manager();
        let addr: SocketAddr = "127.0.0.1:5001".parse().unwrap();

        mgr.add_peer("peer-1".to_string(), addr, "router", vec![], vec![])
            .unwrap();

        let models = vec![PeerModelInfo {
            name: "llama3-8b".to_string(),
            size_bytes: 8_000_000_000,
            backend: "llama.cpp".to_string(),
            max_context: 4096,
            quantization: "Q4_K_M".to_string(),
        }];

        mgr.update_peer_models("peer-1", models);
        let peer = mgr.get_peer("peer-1").unwrap();
        assert_eq!(peer.models.len(), 1);
        assert_eq!(peer.models[0].name, "llama3-8b");

        let containers = vec![PeerContainerInfo {
            id: "abc123".to_string(),
            name: "vllm-server".to_string(),
            image: "vllm:latest".to_string(),
            status: "running".to_string(),
            cpu_percent: 30.0,
            memory_mb: 4096,
            memory_limit_mb: 0,
        }];

        mgr.update_peer_containers("peer-1", containers);
        let peer = mgr.get_peer("peer-1").unwrap();
        assert_eq!(peer.containers.len(), 1);
        assert_eq!(peer.containers[0].name, "vllm-server");
    }

    #[test]
    fn best_node_for_model_finds_least_loaded() {
        let mgr = make_manager();
        let addr1: SocketAddr = "127.0.0.1:5001".parse().unwrap();
        let addr2: SocketAddr = "127.0.0.1:5002".parse().unwrap();

        mgr.add_peer("peer-1".to_string(), addr1, "router", vec![], vec![])
            .unwrap();
        mgr.add_peer("peer-2".to_string(), addr2, "router", vec![], vec![])
            .unwrap();

        // Oba maja model, oba quic connected
        mgr.set_quic_connected("peer-1", true);
        mgr.set_quic_connected("peer-2", true);

        let model = PeerModelInfo {
            name: "llama3".to_string(),
            size_bytes: 8_000_000_000,
            backend: "llama.cpp".to_string(),
            max_context: 4096,
            quantization: "Q4_K_M".to_string(),
        };
        mgr.update_peer_models("peer-1", vec![model.clone()]);
        mgr.update_peer_models("peer-2", vec![model]);

        // peer-1 obciazony, peer-2 lekki
        mgr.update_peer_metrics("peer-1", 90.0, 8000, 16000, vec![], 5.0, 20);
        mgr.update_peer_metrics("peer-2", 10.0, 4000, 16000, vec![], 0.5, 2);

        let best = mgr.best_node_for_model("llama3");
        assert!(best.is_some());
        assert_eq!(best.unwrap().0, "peer-2");
    }

    #[test]
    fn best_node_for_model_requires_quic_and_alive() {
        let mgr = make_manager();
        let addr: SocketAddr = "127.0.0.1:5001".parse().unwrap();

        mgr.add_peer("peer-1".to_string(), addr, "router", vec![], vec![])
            .unwrap();

        let model = PeerModelInfo {
            name: "llama3".to_string(),
            size_bytes: 8_000_000_000,
            backend: "llama.cpp".to_string(),
            max_context: 4096,
            quantization: "Q4_K_M".to_string(),
        };
        mgr.update_peer_models("peer-1", vec![model]);
        mgr.update_peer_metrics("peer-1", 10.0, 4000, 16000, vec![], 0.5, 0);

        // Nie polaczony QUIC — nie powinien byc wybrany
        assert!(mgr.best_node_for_model("llama3").is_none());
    }

    #[test]
    fn best_node_for_capability_works() {
        let mgr = make_manager();
        let addr: SocketAddr = "127.0.0.1:5001".parse().unwrap();

        mgr.add_peer(
            "peer-tts".to_string(),
            addr,
            "router",
            vec![],
            vec!["tts".to_string()],
        )
        .unwrap();

        mgr.set_quic_connected("peer-tts", true);
        mgr.update_peer_metrics("peer-tts", 20.0, 4000, 16000, vec![], 1.0, 3);

        let best = mgr.best_node_for_capability("tts");
        assert!(best.is_some());
        assert_eq!(best.unwrap().0, "peer-tts");

        // Nieistniejaca capability
        assert!(mgr.best_node_for_capability("nonexistent").is_none());
    }

    #[test]
    fn compute_load_score_weights_gpu() {
        let mgr = make_manager();
        let addr: SocketAddr = "127.0.0.1:5001".parse().unwrap();

        mgr.add_peer("peer-1".to_string(), addr, "router", vec![], vec![])
            .unwrap();

        let gpus = vec![
            PeerGpuInfo {
                name: "GPU 0".to_string(),
                usage_percent: 50.0,
                vram_used_mb: 10000,
                vram_total_mb: 24000,
                temperature_c: 60,
                power_draw_w: None,
                power_limit_w: None,
                vendor: crate::mesh::peer_store::GpuVendor::Other,
            },
            PeerGpuInfo {
                name: "GPU 1".to_string(),
                usage_percent: 70.0,
                vram_used_mb: 15000,
                vram_total_mb: 24000,
                temperature_c: 65,
                power_draw_w: None,
                power_limit_w: None,
                vendor: crate::mesh::peer_store::GpuVendor::Other,
            },
        ];
        mgr.update_peer_metrics("peer-1", 30.0, 8000, 16000, gpus, 2.0, 5);

        let peer = mgr.get_peer("peer-1").unwrap();
        let score = PeerManager::compute_load_score(&peer);
        // cpu=30 + gpu_avg=(50+70)/2=60 * 2 = 150.0
        assert!((score - 150.0).abs() < 0.01);
    }
}
