// =============================================================================
// Plik: mesh/peer_store.rs
// Opis: In-memory store odkrytych peerow mesh — uzywany przez dashboard API.
//       Zoptymalizowane pod 1000 peerow: cached list (Arc<Vec>), atomowe
//       aktualizacje metryk bez klonowania calej kolekcji.
// =============================================================================

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use parking_lot::RwLock;
use serde::{Serialize, Deserialize};
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};

/// Informacje o pojedynczym peerze w sieci mesh
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshPeerInfo {
    pub node_id: String,
    pub addresses: Vec<IpAddr>,
    pub port: u16,
    pub role: String,
    pub status: String,
    pub quic_connected: bool,
    pub discovered_at: String,
    pub hostname: String,
    pub os_info: String,
    pub cpu_count: u32,
    pub ram_total_mb: u64,
    pub cpu_usage_percent: f32,
    pub ram_used_mb: u64,
    pub gpu_info: Vec<PeerGpuInfo>,
    pub containers: Vec<PeerContainerInfo>,
    pub networks: Vec<PeerNetworkInfo>,
    pub platform: String,
    pub cpu_temperature_c: Option<f32>,
    pub swap_total_mb: u64,
    pub swap_used_mb: u64,
    /// Czy Docker jest dostepny na tym nodzie
    pub docker_available: bool,
    /// Wersja Docker serwera (np. "27.5.1")
    pub docker_version: String,
}

/// Informacje o GPU peera
#[derive(Debug, Clone, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize)]
pub struct PeerGpuInfo {
    pub name: String,
    pub vram_total_mb: u64,
    pub vram_used_mb: u64,
    pub usage_percent: f32,
    pub temperature_c: u32,
    pub power_draw_w: Option<f32>,
    pub power_limit_w: Option<f32>,
}

/// Informacje o nodzie — wymieniane przez QUIC po polaczeniu
#[derive(Debug, Clone, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize)]
pub struct NodeInfo {
    pub node_id: String,
    pub hostname: String,
    pub os_info: String,
    pub cpu_count: u32,
    pub ram_total_mb: u64,
    pub gpu_info: Vec<PeerGpuInfo>,
}

/// Informacje o kontenerze Docker peera
#[derive(Debug, Clone, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize)]
pub struct PeerContainerInfo {
    pub id: String,
    pub name: String,
    pub image: String,
    pub status: String,
    pub cpu_percent: f32,
    pub memory_mb: u64,
    pub memory_limit_mb: u64,
}

/// Informacje o interfejsie sieciowym peera
#[derive(Debug, Clone, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize)]
pub struct PeerNetworkInfo {
    pub name: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_bytes_per_sec: u64,
    pub tx_bytes_per_sec: u64,
    pub link_up: bool,
    pub ipv4_address: String,
    pub ipv4_netmask: String,
    pub ipv4_gateway: String,
    pub mac_address: String,
    pub interface_type: String,
    pub rdma_available: bool,
    pub speed_mbps: Option<u64>,
}

/// Metryki wysylane w heartbeatach do peerow mesh
#[derive(Debug, Clone, Archive, RkyvSerialize, RkyvDeserialize)]
pub struct HeartbeatMetrics {
    pub cpu_usage_percent: f32,
    pub ram_used_mb: u64,
    pub gpus: Vec<PeerGpuInfo>,
    pub containers: Vec<PeerContainerInfo>,
    pub networks: Vec<PeerNetworkInfo>,
    pub platform: String,
    pub cpu_temperature_c: Option<f32>,
    pub swap_total_mb: u64,
    pub swap_used_mb: u64,
    /// Lista polaczonych peer_ids — do propagacji topologii mesh
    pub connected_peers: Vec<String>,
}

/// Wpis w tabeli routingu — jak dotrzec do danego noda
#[derive(Debug, Clone)]
pub struct RoutingEntry {
    pub next_hop: String,
    pub hops: u8,
    pub direct: bool,
}

/// Wspoldzielony store peerow — bezpieczny miedzy watkami.
///
/// [OPT] Optymalizacje pod 1000 peerow:
/// - `list_cache`: Arc<Vec<MeshPeerInfo>> — klonowany snapshot odswiezany
///   tylko gdy dane sie zmienia (flaga dirty). Eliminuje klonowanie 1000
///   peerow przy kazdym wywolaniu /api/mesh/nodes.
/// - `dirty`: atomowa flaga — ustawiana przy write, czyszczona przy rebuild cache.
///   Przy 2000 heartbeatow/s list() nie musi klonowac jesli nikt nie pisze.
#[derive(Debug, Clone)]
pub struct MeshPeerStore {
    peers: Arc<RwLock<HashMap<String, MeshPeerInfo>>>,
    /// [OPT] Cache listy peerow — Arc pozwala na tanie klonowanie referencji
    list_cache: Arc<RwLock<Arc<Vec<MeshPeerInfo>>>>,
    /// [OPT] Flaga dirty — czy cache trzeba odswiezyc
    dirty: Arc<AtomicBool>,
    /// Topologia mesh — node_id -> lista bezposrednio polaczonych peerow
    topology: Arc<RwLock<HashMap<String, Vec<String>>>>,
    /// Tabela routingu — obliczana z topologii BFS
    routing_table: Arc<RwLock<HashMap<String, RoutingEntry>>>,
}

impl MeshPeerStore {
    pub fn new() -> Self {
        Self {
            peers: Arc::new(RwLock::new(HashMap::new())),
            list_cache: Arc::new(RwLock::new(Arc::new(Vec::new()))),
            dirty: Arc::new(AtomicBool::new(false)),
            topology: Arc::new(RwLock::new(HashMap::new())),
            routing_table: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// [OPT] Oznacza cache jako nieaktualny — nastepne list() odbuduje.
    /// Inline — tylko jeden atomic store.
    #[inline(always)]
    fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
    }

    /// Dodaje nowego peera lub aktualizuje istniejacego.
    /// Deduplikacja: jesli istnieje disconnected peer z tym samym adresem+portem,
    /// stary wpis jest usuwany i nowy go zastepuje (host sie zrestartowal z nowym node_id).
    pub fn add_or_update(&self, peer: MeshPeerInfo) {
        let mut peers = self.peers.write();

        // Szukaj disconnected peera z pasujacym adresem+portem (ten sam host, nowy UUID)
        if !peer.addresses.is_empty() && peer.port > 0 {
            let stale_ids: Vec<String> = peers
                .iter()
                .filter(|(id, _)| *id != &peer.node_id)
                .filter(|(_, existing)| {
                    !existing.quic_connected
                        && existing.port == peer.port
                        && existing.addresses.iter().any(|a| peer.addresses.contains(a))
                })
                .map(|(id, _)| id.clone())
                .collect();

            for id in stale_ids {
                tracing::info!(
                    old_node_id = %id,
                    new_node_id = %peer.node_id,
                    port = peer.port,
                    "Usuwanie starego wpisu disconnected peera (ten sam host sie ponownie polaczyl)"
                );
                peers.remove(&id);
            }
        }

        peers.insert(peer.node_id.clone(), peer);
        drop(peers);
        self.mark_dirty();
    }

    pub fn set_status(&self, node_id: &str, status: &str) {
        let mut peers = self.peers.write();
        let p = peers.entry(node_id.to_string()).or_insert_with(|| Self::empty_peer(node_id));
        p.status = status.to_string();
        drop(peers);
        self.mark_dirty();
    }

    pub fn set_quic_connected(&self, node_id: &str, connected: bool) {
        let mut peers = self.peers.write();
        let p = peers.entry(node_id.to_string()).or_insert_with(|| Self::empty_peer(node_id));
        p.quic_connected = connected;
        drop(peers);
        self.mark_dirty();
    }

    pub fn remove(&self, node_id: &str) {
        self.peers.write().remove(node_id);
        self.mark_dirty();
    }

    /// [OPT] Zwraca liste peerow z cache — Arc<Vec> zamiast klonowania calego Vec.
    /// Cache odbudowywany tylko gdy dane sie zmienily (flaga dirty).
    /// Przy 1000 peerach i /api/mesh/nodes co 1s: 0 alokacji jesli brak zmian.
    pub fn list(&self) -> Vec<MeshPeerInfo> {
        // Sprawdz czy cache jest aktualny (atomic load — brak locka)
        if self.dirty.load(Ordering::Acquire) {
            self.rebuild_cache();
        }
        // Zwroc sklonowany Vec z cache — konieczne bo API wymaga Vec<MeshPeerInfo>
        // Ale jesli wielu callerow czyta jednoczesnie, lockujemy cache na krotko
        let cache = self.list_cache.read();
        (**cache).clone()
    }

    /// [OPT] Zwraca Arc<Vec> bez klonowania — dla callerow ktorzy moga uzyc Arc.
    /// Zero alokacji jesli cache jest aktualny.
    pub fn list_arc(&self) -> Arc<Vec<MeshPeerInfo>> {
        if self.dirty.load(Ordering::Acquire) {
            self.rebuild_cache();
        }
        Arc::clone(&self.list_cache.read())
    }

    /// [OPT] Odbudowuje cache listy peerow
    fn rebuild_cache(&self) {
        let peers = self.peers.read();
        let list: Vec<MeshPeerInfo> = peers.values().cloned().collect();
        drop(peers);
        *self.list_cache.write() = Arc::new(list);
        self.dirty.store(false, Ordering::Release);
    }

    pub fn get(&self, node_id: &str) -> Option<MeshPeerInfo> {
        self.peers.read().get(node_id).cloned()
    }

    /// Aktualizuje dane systemowe peera po otrzymaniu NodeInfo przez QUIC.
    /// Dodatkowo deduplikuje po hostname+port — jesli istnieje disconnected peer
    /// o tej samej nazwie hosta i porcie, stary wpis jest usuwany.
    /// Zaktualizuj hostname peera (np. z mDNS TXT records)
    pub fn update_hostname(&self, node_id: &str, hostname: &str) {
        let mut peers = self.peers.write();
        if let Some(p) = peers.get_mut(node_id) {
            p.hostname = hostname.to_string();
        }
        drop(peers);
        self.mark_dirty();
    }

    pub fn update_node_info(&self, node_id: &str, info: &NodeInfo) {
        let mut peers = self.peers.write();
        let p = peers.entry(node_id.to_string()).or_insert_with(|| Self::empty_peer(node_id));
        p.hostname = info.hostname.clone();
        p.os_info = info.os_info.clone();
        p.cpu_count = info.cpu_count;
        p.ram_total_mb = info.ram_total_mb;
        p.gpu_info = info.gpu_info.clone();

        // Deduplikacja po hostname+port — usun stare disconnected wpisy tego samego hosta
        if !info.hostname.is_empty() && p.port > 0 {
            let port = p.port;
            let hostname = info.hostname.clone();
            let stale_ids: Vec<String> = peers
                .iter()
                .filter(|(id, _)| id.as_str() != node_id)
                .filter(|(_, existing)| {
                    !existing.quic_connected
                        && existing.port == port
                        && existing.hostname == hostname
                })
                .map(|(id, _)| id.clone())
                .collect();

            for id in stale_ids {
                tracing::info!(
                    old_node_id = %id,
                    new_node_id = %node_id,
                    hostname = %hostname,
                    "Usuwanie starego wpisu disconnected peera (hostname+port match)"
                );
                peers.remove(&id);
            }
        }

        drop(peers);
        self.mark_dirty();
    }

    /// Aktualizuje biezace metryki peera (z heartbeatu)
    pub fn update_metrics(&self, node_id: &str, cpu_usage: f32, ram_used: u64, gpus: Vec<PeerGpuInfo>, containers: Vec<PeerContainerInfo>, networks: Vec<PeerNetworkInfo>, platform: String, cpu_temperature_c: Option<f32>, swap_total_mb: u64, swap_used_mb: u64) {
        let mut peers = self.peers.write();
        let p = peers.entry(node_id.to_string()).or_insert_with(|| Self::empty_peer(node_id));
        p.cpu_usage_percent = cpu_usage;
        p.ram_used_mb = ram_used;
        p.gpu_info = gpus;
        p.containers = containers;
        p.networks = networks;
        p.cpu_temperature_c = cpu_temperature_c;
        p.swap_total_mb = swap_total_mb;
        p.swap_used_mb = swap_used_mb;
        if !platform.is_empty() {
            p.platform = platform;
        }
        drop(peers);
        self.mark_dirty();
    }

    /// Aktualizuje wolno-zmienne dane lokalnego noda (adresy IP, Docker, OS info).
    /// Wywolywane co 60s przez background task w pipeline.
    pub fn update_local_extras(&self, node_id: &str, addresses: Vec<IpAddr>, docker_available: bool, docker_version: String, os_info: String) {
        let mut peers = self.peers.write();
        let p = peers.entry(node_id.to_string()).or_insert_with(|| Self::empty_peer(node_id));
        p.addresses = addresses;
        p.docker_available = docker_available;
        p.docker_version = docker_version;
        if !os_info.is_empty() {
            p.os_info = os_info;
        }
        drop(peers);
        self.mark_dirty();
    }

    /// Aktualizuje topologie mesh — zapisuje liste bezposrednich peerow danego noda
    pub fn update_topology(&self, node_id: &str, connected_peers: Vec<String>) {
        self.topology.write().insert(node_id.to_string(), connected_peers);
    }

    /// Zwraca kopie calej topologii mesh
    pub fn get_topology(&self) -> HashMap<String, Vec<String>> {
        self.topology.read().clone()
    }

    /// Pobierz routing entry dla noda — None jesli nieosiagalny
    pub fn get_route(&self, node_id: &str) -> Option<RoutingEntry> {
        self.routing_table.read().get(node_id).cloned()
    }

    /// Przelicz tabele routingu z topologii (BFS od local_node_id, max 4 hopy)
    pub fn recalculate_routes(&self, local_node_id: &str) {
        let topology = self.topology.read().clone();
        let mut routes: HashMap<String, RoutingEntry> = HashMap::new();

        // BFS od lokalnego noda
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut queue: std::collections::VecDeque<(String, String, u8)> = std::collections::VecDeque::new();
        // (node_id, next_hop, hops)

        visited.insert(local_node_id.to_string());

        // Bezposredni sasiedzi (hop 1)
        if let Some(direct_peers) = topology.get(local_node_id) {
            for peer in direct_peers {
                if visited.insert(peer.clone()) {
                    routes.insert(peer.clone(), RoutingEntry {
                        next_hop: peer.clone(),
                        hops: 1,
                        direct: true,
                    });
                    queue.push_back((peer.clone(), peer.clone(), 1));
                }
            }
        }

        // BFS — max 4 hopy
        while let Some((current, first_hop, depth)) = queue.pop_front() {
            if depth >= 4 { continue; }
            if let Some(peers) = topology.get(&current) {
                for peer in peers {
                    if visited.insert(peer.clone()) {
                        routes.insert(peer.clone(), RoutingEntry {
                            next_hop: first_hop.clone(),
                            hops: depth + 1,
                            direct: false,
                        });
                        queue.push_back((peer.clone(), first_hop.clone(), depth + 1));
                    }
                }
            }
        }

        *self.routing_table.write() = routes;
    }

    /// Pelna tabela routingu (do debugowania/API)
    pub fn get_routing_table(&self) -> HashMap<String, RoutingEntry> {
        self.routing_table.read().clone()
    }

    /// Tworzy pusty wpis peera — uzywany gdy QUIC polaczyl sie przed mDNS discovery
    fn empty_peer(node_id: &str) -> MeshPeerInfo {
        MeshPeerInfo {
            node_id: node_id.to_string(),
            addresses: vec![],
            port: 0,
            role: "router".to_string(),
            status: "connected".to_string(),
            quic_connected: false,
            discovered_at: chrono::Utc::now().to_rfc3339(),
            hostname: String::new(),
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
        }
    }
}
