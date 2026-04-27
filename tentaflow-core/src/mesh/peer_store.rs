// =============================================================================
// Plik: mesh/peer_store.rs
// Opis: In-memory store odkrytych peerow mesh — uzywany przez dashboard API.
//       Zoptymalizowane pod 1000 peerow: cached list (Arc<Vec>), atomowe
//       aktualizacje metryk bez klonowania calej kolekcji.
// =============================================================================

use arc_swap::ArcSwap;
use dashmap::DashMap;
use rkyv::{Archive, Deserialize as RkyvDeserialize, Serialize as RkyvSerialize};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Informacje o modelu zaladowanym na nodzie mesh
#[derive(Debug, Clone, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize)]
pub struct PeerModelInfo {
    /// Alias/nazwa modelu (np. "qwen3.5-0.8b", "whisper-large-v3")
    pub alias: String,
    /// Kategoria: "llm", "stt", "tts", "embeddings", "image", "vision"
    pub kind: String,
    /// Backend ktory serwuje model (np. "llama-cpp", "mlx", "vllm", "whisper-rs")
    pub backend: String,
    /// Rozmiar pliku wag w MB (0 jesli nieznany)
    pub size_mb: u64,
    /// Czy model jest zaladowany do pamieci i gotowy do inferencji
    pub loaded: bool,
}

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
    /// Modele zaladowane / dostepne na nodzie (propagowane przez ModelsSync).
    #[serde(default)]
    pub models: Vec<PeerModelInfo>,
    /// Liczba aktualnie obslugiwanych requestow (snapshot z heartbeat).
    #[serde(default)]
    pub active_requests: u32,
    /// Wygenerowane tokenow/sekunde w ostatnim oknie metryk.
    #[serde(default)]
    pub tokens_per_sec: f32,
    /// Czy peer ma zainstalowany `nsys` (NVIDIA Nsight Systems CLI). GUI uzywa
    /// tego pola do warunkowego pokazywania przycisku Profile na karcie peera.
    #[serde(default)]
    pub nsys_available: bool,
    /// Wersja `nsys` zaraportowana przez peera (pusta gdy `nsys_available=false`).
    #[serde(default)]
    pub nsys_version: String,
}

/// Producent GPU — wykrywany po nazwie / PCI; uzywany do gating profilowania
/// (np. NVIDIA Nsight Systems wymaga `vendor == Nvidia`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Archive, RkyvSerialize, RkyvDeserialize)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Apple,
    Other,
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
    /// Producent GPU — wykrywany po nazwie/PCI. Domyslnie `Other` dopoki
    /// detekcja nie jest podlaczona (PR2 doda klasyfikacje).
    pub vendor: GpuVendor,
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
    pub numa_node: Option<i32>,
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
    /// Aktualna liczba obslugiwanych requestow (inference, ingest, itp.)
    pub active_requests: u32,
    /// Wygenerowane tokeny/sekunde w oknie metrycznym (tylko LLM).
    pub tokens_per_sec: f32,
    /// Capability flag: `true` gdy peer wykryl dziajace `nsys` w PATH. Propaguje
    /// sie z heartbeatami tak, zeby GUI nie musialo pingowac peera dla samego
    /// wyswietlenia przycisku Profile.
    pub nsys_available: bool,
    /// Wersja `nsys` (pusta gdy `nsys_available=false`).
    pub nsys_version: String,
}

/// Broadcast z lista modeli zaladowanych/dostepnych na nodzie. Wysylany co
/// `models_sync_interval` (domyslnie 30s) oraz po kazdej zmianie listy modeli.
#[derive(Debug, Clone, Archive, RkyvSerialize, RkyvDeserialize)]
pub struct ModelsSync {
    pub models: Vec<PeerModelInfo>,
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
/// [SCALE] Optymalizacje pod tysiace peerow:
/// - `peers: DashMap` — lock-free per-shard, 2000 heartbeatow/s nie
///   szereguje sie na globalnym locku.
/// - `list_cache: ArcSwap<Vec>` — atomowa publikacja snapshotu, readers
///   nie biora zadnego locka (`load_full` = atomic ptr copy).
/// - `topology: DashMap` — insert per-peer bez globalnej synchronizacji.
/// - `routing_table: ArcSwap<HashMap>` — BFS buduje nowa mape i publikuje
///   atomowo. Readers (`get_route`) zero-lock.
/// - `last_heartbeat_ms: DashMap` — mark/clear per-peer lock-free.
#[derive(Debug, Clone)]
pub struct MeshPeerStore {
    peers: Arc<DashMap<String, MeshPeerInfo>>,
    /// Snapshot listy peerow — publikowany atomowo, czytany bez locka.
    list_cache: Arc<ArcSwap<Vec<MeshPeerInfo>>>,
    /// Flaga dirty — ustawiana przy write, czyszczona przy rebuild cache.
    dirty: Arc<AtomicBool>,
    /// Topologia mesh — node_id -> lista bezposrednio polaczonych peerow
    topology: Arc<DashMap<String, Vec<String>>>,
    /// Tabela routingu — obliczana z topologii BFS, publikowana atomowo.
    routing_table: Arc<ArcSwap<HashMap<String, RoutingEntry>>>,
    /// Ostatni odebrany heartbeat per peer (unix millis). Liveness timer sprawdza
    /// aktualnosc — >2s = degraded, >5s = offline + force disconnect.
    last_heartbeat_ms: Arc<DashMap<String, i64>>,
    /// Flaga dirty dla routing_table — handlery tylko zaznaczaja, periodyczny
    /// task wola `maybe_recalculate_routes` i coalesce'uje burst reconnectow.
    routes_dirty: Arc<AtomicBool>,
}

impl MeshPeerStore {
    pub fn new() -> Self {
        Self {
            peers: Arc::new(DashMap::with_capacity(256)),
            list_cache: Arc::new(ArcSwap::from_pointee(Vec::new())),
            dirty: Arc::new(AtomicBool::new(false)),
            topology: Arc::new(DashMap::with_capacity(256)),
            routing_table: Arc::new(ArcSwap::from_pointee(HashMap::new())),
            last_heartbeat_ms: Arc::new(DashMap::with_capacity(256)),
            routes_dirty: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Odnotuj odebrany heartbeat od peera (uzywane przez liveness timer).
    pub fn mark_heartbeat(&self, node_id: &str) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.last_heartbeat_ms.insert(node_id.to_string(), now_ms);
    }

    /// Snapshot ostatnich heartbeatow — (node_id, age_ms).
    pub fn heartbeat_ages(&self) -> Vec<(String, i64)> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.last_heartbeat_ms
            .iter()
            .map(|e| (e.key().clone(), now_ms - *e.value()))
            .collect()
    }

    /// Usun wpis heartbeat (po PeerDisconnected).
    pub fn clear_heartbeat(&self, node_id: &str) {
        self.last_heartbeat_ms.remove(node_id);
    }

    /// [OPT] Oznacza cache jako nieaktualny — nastepne list() odbuduje.
    /// Inline — tylko jeden atomic store.
    #[inline(always)]
    fn mark_dirty(&self) {
        self.dirty.store(true, Ordering::Release);
    }

    fn normalize_hostname(hostname: &str) -> String {
        hostname
            .trim()
            .trim_end_matches(" (local)")
            .trim()
            .to_string()
    }

    /// Zbiera id-ki disconnected peerow z pasujacym hostname+port (rozne od
    /// `node_id`). Nie zmienia mapy — caller musi wywolac `peers.remove` dla
    /// kazdego zwroconego id (kazde remove lock-free per-shard).
    fn stale_ids_by_hostname_port(
        peers: &DashMap<String, MeshPeerInfo>,
        node_id: &str,
        hostname: &str,
        port: u16,
    ) -> Vec<String> {
        let normalized = Self::normalize_hostname(hostname);
        if normalized.is_empty() || port == 0 {
            return Vec::new();
        }
        peers
            .iter()
            .filter(|e| e.key().as_str() != node_id)
            .filter(|e| {
                let v = e.value();
                !v.quic_connected
                    && v.port == port
                    && Self::normalize_hostname(&v.hostname) == normalized
            })
            .map(|e| e.key().clone())
            .collect()
    }

    fn remove_stale_by_hostname_port(
        peers: &DashMap<String, MeshPeerInfo>,
        node_id: &str,
        hostname: &str,
        port: u16,
    ) {
        let normalized = Self::normalize_hostname(hostname);
        for id in Self::stale_ids_by_hostname_port(peers, node_id, hostname, port) {
            tracing::info!(
                old_node_id = %id,
                new_node_id = %node_id,
                hostname = %normalized,
                port,
                "Usuwanie stalego wpisu peera (hostname+port match)"
            );
            peers.remove(&id);
        }
    }

    /// Dodaje nowego peera lub aktualizuje istniejacego.
    /// Deduplikacja: jesli istnieje disconnected peer z tym samym adresem+portem,
    /// stary wpis jest usuwany i nowy go zastepuje (host sie zrestartowal z nowym node_id).
    pub fn add_or_update(&self, peer: MeshPeerInfo) {
        // Szukaj disconnected peera z pasujacym adresem+portem (ten sam host, nowy UUID)
        if !peer.addresses.is_empty() && peer.port > 0 {
            let stale_ids: Vec<String> = self
                .peers
                .iter()
                .filter(|e| *e.key() != peer.node_id)
                .filter(|e| {
                    let v = e.value();
                    !v.quic_connected
                        && v.port == peer.port
                        && v.addresses.iter().any(|a| peer.addresses.contains(a))
                })
                .map(|e| e.key().clone())
                .collect();

            for id in stale_ids {
                tracing::info!(
                    old_node_id = %id,
                    new_node_id = %peer.node_id,
                    port = peer.port,
                    "Usuwanie starego wpisu disconnected peera (ten sam host sie ponownie polaczyl)"
                );
                self.peers.remove(&id);
            }
        }

        self.peers.insert(peer.node_id.clone(), peer);
        self.mark_dirty();
    }

    pub fn set_status(&self, node_id: &str, status: &str) {
        self.peers
            .entry(node_id.to_string())
            .or_insert_with(|| Self::empty_peer(node_id))
            .status = status.to_string();
        self.mark_dirty();
    }

    pub fn set_quic_connected(&self, node_id: &str, connected: bool) {
        self.peers
            .entry(node_id.to_string())
            .or_insert_with(|| Self::empty_peer(node_id))
            .quic_connected = connected;
        self.mark_dirty();
    }

    pub fn is_quic_connected(&self, node_id: &str) -> bool {
        self.peers
            .get(node_id)
            .map(|p| p.quic_connected)
            .unwrap_or(false)
    }

    pub fn set_addresses(&self, node_id: &str, addrs: Vec<IpAddr>) {
        if addrs.is_empty() {
            return;
        }
        self.peers
            .entry(node_id.to_string())
            .or_insert_with(|| Self::empty_peer(node_id))
            .addresses = addrs;
        self.mark_dirty();
    }

    /// Hostname — ustawiany na podstawie Hello payload od peera lub seed_local.
    /// Nigdy nie nadpisuje niepustej wartosci pustym stringiem.
    pub fn set_hostname(&self, node_id: &str, hostname: &str) {
        let hostname = Self::normalize_hostname(hostname);
        if hostname.is_empty() {
            return;
        }
        let port = {
            let mut entry = self
                .peers
                .entry(node_id.to_string())
                .or_insert_with(|| Self::empty_peer(node_id));
            entry.hostname = hostname.clone();
            entry.port
        };
        Self::remove_stale_by_hostname_port(&self.peers, node_id, &hostname, port);
        self.mark_dirty();
    }

    pub fn set_platform(&self, node_id: &str, platform: &str) {
        if platform.is_empty() {
            return;
        }
        self.peers
            .entry(node_id.to_string())
            .or_insert_with(|| Self::empty_peer(node_id))
            .platform = platform.to_string();
        self.mark_dirty();
    }

    pub fn set_os_info(&self, node_id: &str, os_info: &str) {
        if os_info.is_empty() {
            return;
        }
        self.peers
            .entry(node_id.to_string())
            .or_insert_with(|| Self::empty_peer(node_id))
            .os_info = os_info.to_string();
        self.mark_dirty();
    }

    pub fn remove(&self, node_id: &str) {
        self.peers.remove(node_id);
        self.mark_dirty();
    }

    /// Zwraca liste peerow (klon Vec — API wymaga owned).
    /// Cache przebudowywany tylko gdy dane sie zmienily (flaga dirty).
    pub fn list(&self) -> Vec<MeshPeerInfo> {
        if self.dirty.load(Ordering::Acquire) {
            self.rebuild_cache();
        }
        // ArcSwap::load_full daje Arc<Vec> bez locka; (*).clone() robi jeden
        // klon vektora. Czytelnicy rownolegli sie nie blokuja.
        (**self.list_cache.load()).clone()
    }

    /// Arc na cache — zero alokacji na ten klon (tanie shared-ownership).
    pub fn list_arc(&self) -> Arc<Vec<MeshPeerInfo>> {
        if self.dirty.load(Ordering::Acquire) {
            self.rebuild_cache();
        }
        self.list_cache.load_full()
    }

    fn rebuild_cache(&self) {
        let list: Vec<MeshPeerInfo> =
            self.peers.iter().map(|e| e.value().clone()).collect();
        self.list_cache.store(Arc::new(list));
        self.dirty.store(false, Ordering::Release);
    }

    pub fn get(&self, node_id: &str) -> Option<MeshPeerInfo> {
        self.peers.get(node_id).map(|p| p.clone())
    }

    /// Zwraca tylko hostname — bez klonowania reszty MeshPeerInfo (mnóstwo String/Vec).
    /// Hot path: publish_mesh_peer_status, diagnostyka.
    pub fn get_hostname(&self, node_id: &str) -> Option<String> {
        self.peers.get(node_id).map(|p| p.hostname.clone())
    }

    /// Buduje KnownPeersPayload entries bezposrednio z mapy — omija klonowanie
    /// calego Vec<MeshPeerInfo>. Filtruje w locie. Jeden pass, write! zamiast format!
    /// Wywolywane przy kazdym PeerConnected w handle_peer_connected.
    pub fn known_peers_snapshot(
        &self,
        exclude_a: &str,
        exclude_b: &str,
    ) -> Vec<tentaflow_protocol::mesh::KnownPeerEntry> {
        let mut out = Vec::with_capacity(self.peers.len().saturating_sub(2));
        for entry in self.peers.iter() {
            let p = entry.value();
            if p.node_id == exclude_a || p.node_id == exclude_b {
                continue;
            }
            if !p.quic_connected || p.addresses.is_empty() {
                continue;
            }
            let direct_addrs: Vec<String> = p
                .addresses
                .iter()
                .map(|ip| format!("{}:{}", ip, p.port))
                .collect();
            out.push(tentaflow_protocol::mesh::KnownPeerEntry {
                node_id: p.node_id.clone(),
                hostname: p.hostname.clone(),
                direct_addrs,
                port: p.port,
            });
        }
        out
    }

    /// Snapshot hostname + addresses + port — dla persystencji trusted contact
    /// hints. Omija klonowanie pozostalych ~20 pol MeshPeerInfo.
    pub fn contact_snapshot(
        &self,
        node_id: &str,
    ) -> Option<(String, Vec<std::net::IpAddr>, u16)> {
        self.peers
            .get(node_id)
            .map(|p| (p.hostname.clone(), p.addresses.clone(), p.port))
    }

    /// Aktualizuje dane systemowe peera po otrzymaniu NodeInfo przez QUIC.
    /// Dodatkowo deduplikuje po hostname+port — jesli istnieje disconnected peer
    /// o tej samej nazwie hosta i porcie, stary wpis jest usuwany.
    /// Zaktualizuj hostname peera (np. z mDNS TXT records)
    pub fn update_hostname(&self, node_id: &str, hostname: &str) {
        let hostname = Self::normalize_hostname(hostname);
        if hostname.is_empty() {
            return;
        }
        let port = if let Some(mut p) = self.peers.get_mut(node_id) {
            p.hostname = hostname.clone();
            p.port
        } else {
            return;
        };
        Self::remove_stale_by_hostname_port(&self.peers, node_id, &hostname, port);
        self.mark_dirty();
    }

    pub fn update_node_info(&self, node_id: &str, info: &NodeInfo) {
        let (hostname, port) = {
            let mut entry = self
                .peers
                .entry(node_id.to_string())
                .or_insert_with(|| Self::empty_peer(node_id));
            entry.hostname = Self::normalize_hostname(&info.hostname);
            entry.os_info = info.os_info.clone();
            entry.cpu_count = info.cpu_count;
            entry.ram_total_mb = info.ram_total_mb;
            entry.gpu_info = info.gpu_info.clone();
            (entry.hostname.clone(), entry.port)
        };
        Self::remove_stale_by_hostname_port(&self.peers, node_id, &hostname, port);
        self.mark_dirty();
    }

    /// Aktualizuje biezace metryki peera (z heartbeatu)
    pub fn update_metrics(
        &self,
        node_id: &str,
        cpu_usage: f32,
        ram_used: u64,
        gpus: Vec<PeerGpuInfo>,
        containers: Vec<PeerContainerInfo>,
        networks: Vec<PeerNetworkInfo>,
        platform: String,
        cpu_temperature_c: Option<f32>,
        swap_total_mb: u64,
        swap_used_mb: u64,
        active_requests: u32,
        tokens_per_sec: f32,
        nsys_available: bool,
        nsys_version: String,
    ) {
        let mut entry = self
            .peers
            .entry(node_id.to_string())
            .or_insert_with(|| Self::empty_peer(node_id));
        entry.cpu_usage_percent = cpu_usage;
        entry.ram_used_mb = ram_used;
        entry.gpu_info = gpus;
        entry.containers = containers;
        entry.networks = networks;
        entry.cpu_temperature_c = cpu_temperature_c;
        entry.swap_total_mb = swap_total_mb;
        entry.swap_used_mb = swap_used_mb;
        entry.active_requests = active_requests;
        entry.tokens_per_sec = tokens_per_sec;
        entry.nsys_available = nsys_available;
        entry.nsys_version = nsys_version;
        if !platform.is_empty() {
            entry.platform = platform;
        }
        drop(entry);
        self.mark_dirty();
    }

    /// Inicjalizuje wpis lokalnego noda — wywoływane przy starcie tentaflow
    /// niezaleznie od config.mesh.enabled. Dzieki temu /api/mesh/nodes zawsze
    /// zwraca przynajmniej local. node_info wypelnione przez node_info_collector.
    pub fn seed_local(
        &self,
        node_id: &str,
        hostname: String,
        os_info: String,
        platform: String,
        cpu_count: u32,
        ram_total_mb: u64,
        gpu_info: Vec<PeerGpuInfo>,
        addresses: Vec<IpAddr>,
        docker_available: bool,
        docker_version: String,
    ) {
        let (hostname, port) = {
            let mut entry = self
                .peers
                .entry(node_id.to_string())
                .or_insert_with(|| Self::empty_peer(node_id));
            entry.hostname = Self::normalize_hostname(&hostname);
            entry.os_info = os_info;
            entry.platform = platform;
            entry.cpu_count = cpu_count;
            entry.ram_total_mb = ram_total_mb;
            entry.gpu_info = gpu_info;
            entry.addresses = addresses;
            entry.docker_available = docker_available;
            entry.docker_version = docker_version;
            if entry.role.is_empty() {
                entry.role = "router".to_string();
            }
            entry.status = "connected".to_string();
            entry.quic_connected = true;
            (entry.hostname.clone(), entry.port)
        };
        Self::remove_stale_by_hostname_port(&self.peers, node_id, &hostname, port);
        self.mark_dirty();
    }

    /// Aktualizuje liste modeli propagowanych przez ModelsSync. Nadpisuje
    /// calkowicie — peer jest zrodlem prawdy dla swoich modeli.
    pub fn update_models(&self, node_id: &str, models: Vec<PeerModelInfo>) {
        self.peers
            .entry(node_id.to_string())
            .or_insert_with(|| Self::empty_peer(node_id))
            .models = models;
        self.mark_dirty();
    }

    /// Aktualizuje wolno-zmienne dane lokalnego noda (adresy IP, Docker, OS info).
    /// Wywolywane co 60s przez background task w pipeline.
    pub fn update_local_extras(
        &self,
        node_id: &str,
        addresses: Vec<IpAddr>,
        docker_available: bool,
        docker_version: String,
        os_info: String,
    ) {
        {
            let mut entry = self
                .peers
                .entry(node_id.to_string())
                .or_insert_with(|| Self::empty_peer(node_id));
            entry.addresses = addresses;
            entry.docker_available = docker_available;
            entry.docker_version = docker_version;
            if !os_info.is_empty() {
                entry.os_info = os_info;
            }
        }
        self.mark_dirty();
    }

    /// Aktualizuje topologie mesh — zapisuje liste bezposrednich peerow danego noda
    pub fn update_topology(&self, node_id: &str, connected_peers: Vec<String>) {
        self.topology.insert(node_id.to_string(), connected_peers);
    }

    /// Zwraca kopie calej topologii mesh
    pub fn get_topology(&self) -> HashMap<String, Vec<String>> {
        self.topology
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect()
    }

    /// Pobierz routing entry dla noda — None jesli nieosiagalny.
    /// Zero-lock: ArcSwap::load daje Guard bez mutex'a.
    pub fn get_route(&self, node_id: &str) -> Option<RoutingEntry> {
        self.routing_table.load().get(node_id).cloned()
    }

    /// Oznacza routing_table jako wymagajacy przeliczenia. Handlery tylko wolaja
    /// ten no-op zamiast pelnego BFS — periodyczny task robi faktyczna robote.
    pub fn mark_routes_dirty(&self) {
        self.routes_dirty.store(true, Ordering::Release);
    }

    /// Periodyczny tick: jesli flag dirty ustawiony, wykonaj BFS. Idempotentne —
    /// zwraca od razu gdy nic sie nie zmienilo. Zaprojektowane do wolania z
    /// jednego taska w tle (nie trzeba synchronizacji miedzy wieloma).
    pub fn maybe_recalculate_routes(&self, local_node_id: &str) {
        if self.routes_dirty.swap(false, Ordering::AcqRel) {
            self.recalculate_routes(local_node_id);
        }
    }

    /// Przelicz tabele routingu z topologii (BFS od local_node_id, max 4 hopy).
    /// Buduje nowa HashMap poza locka, publikuje atomowo przez ArcSwap.
    pub fn recalculate_routes(&self, local_node_id: &str) {
        let topology: HashMap<String, Vec<String>> = self
            .topology
            .iter()
            .map(|e| (e.key().clone(), e.value().clone()))
            .collect();
        let mut routes: HashMap<String, RoutingEntry> = HashMap::new();

        // BFS od lokalnego noda
        let mut visited: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut queue: std::collections::VecDeque<(String, String, u8)> =
            std::collections::VecDeque::new();
        // (node_id, next_hop, hops)

        visited.insert(local_node_id.to_string());

        // Bezposredni sasiedzi (hop 1)
        if let Some(direct_peers) = topology.get(local_node_id) {
            for peer in direct_peers {
                if visited.insert(peer.clone()) {
                    routes.insert(
                        peer.clone(),
                        RoutingEntry {
                            next_hop: peer.clone(),
                            hops: 1,
                            direct: true,
                        },
                    );
                    queue.push_back((peer.clone(), peer.clone(), 1));
                }
            }
        }

        // BFS — max 5 hopow (wymagane dla multi-hop mesh).
        while let Some((current, first_hop, depth)) = queue.pop_front() {
            if depth >= 5 {
                continue;
            }
            if let Some(peers) = topology.get(&current) {
                for peer in peers {
                    if visited.insert(peer.clone()) {
                        routes.insert(
                            peer.clone(),
                            RoutingEntry {
                                next_hop: first_hop.clone(),
                                hops: depth + 1,
                                direct: false,
                            },
                        );
                        queue.push_back((peer.clone(), first_hop.clone(), depth + 1));
                    }
                }
            }
        }

        self.routing_table.store(Arc::new(routes));
    }

    /// Pelna tabela routingu (do debugowania/API). Zero-lock read.
    pub fn get_routing_table(&self) -> HashMap<String, RoutingEntry> {
        (**self.routing_table.load()).clone()
    }

    /// Upsert minimalnego wpisu peera z TopologyAnnounce — tworzy `MeshPeerInfo`
    /// jesli nieistnieje, ale NIE nadpisuje metryk/GPU/usluge jesli peer juz znany
    /// z bezposredniej komunikacji. Sluzy do widocznosci nodow osiagalnych przez relay.
    pub fn upsert_gossip_peer(
        &self,
        node_id: &str,
        hostname: &str,
        platform: &str,
        os_info: &str,
        addresses: Vec<std::net::IpAddr>,
        port: u16,
    ) {
        let hostname = Self::normalize_hostname(hostname);
        let (dedupe_hostname, dedupe_port) = {
            let mut entry = self
                .peers
                .entry(node_id.to_string())
                .or_insert_with(|| Self::empty_peer(node_id));
            if !hostname.is_empty() && entry.hostname.is_empty() {
                entry.hostname = hostname.clone();
            }
            if !platform.is_empty() && entry.platform.is_empty() {
                entry.platform = platform.to_string();
            }
            if !os_info.is_empty() && entry.os_info.is_empty() {
                entry.os_info = os_info.to_string();
            }
            if entry.addresses.is_empty() && !addresses.is_empty() {
                entry.addresses = addresses;
            }
            if entry.port == 0 && port != 0 {
                entry.port = port;
            }
            if entry.status == "disconnected" || entry.status.is_empty() {
                entry.status = "reachable".to_string();
            }
            (entry.hostname.clone(), entry.port)
        };
        Self::remove_stale_by_hostname_port(&self.peers, node_id, &dedupe_hostname, dedupe_port);
        self.mark_dirty();
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
            models: vec![],
            active_requests: 0,
            tokens_per_sec: 0.0,
            nsys_available: false,
            nsys_version: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_gpu_info_with_vendor_round_trip() {
        let gpu = PeerGpuInfo {
            name: "NVIDIA RTX 4090".to_string(),
            vram_total_mb: 24576,
            vram_used_mb: 8192,
            usage_percent: 73.5,
            temperature_c: 68,
            power_draw_w: Some(310.0),
            power_limit_w: Some(450.0),
            vendor: GpuVendor::Nvidia,
        };

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&gpu).expect("encode");
        let decoded =
            rkyv::from_bytes::<PeerGpuInfo, rkyv::rancor::Error>(&bytes).expect("decode");

        assert_eq!(decoded.name, gpu.name);
        assert_eq!(decoded.vram_total_mb, gpu.vram_total_mb);
        assert_eq!(decoded.vram_used_mb, gpu.vram_used_mb);
        assert!((decoded.usage_percent - gpu.usage_percent).abs() < f32::EPSILON);
        assert_eq!(decoded.temperature_c, gpu.temperature_c);
        assert_eq!(decoded.power_draw_w, gpu.power_draw_w);
        assert_eq!(decoded.power_limit_w, gpu.power_limit_w);
        assert_eq!(decoded.vendor, GpuVendor::Nvidia);
    }

    #[test]
    fn gpu_vendor_all_variants_round_trip() {
        for v in [
            GpuVendor::Nvidia,
            GpuVendor::Amd,
            GpuVendor::Intel,
            GpuVendor::Apple,
            GpuVendor::Other,
        ] {
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&v).expect("encode");
            let decoded =
                rkyv::from_bytes::<GpuVendor, rkyv::rancor::Error>(&bytes).expect("decode");
            assert_eq!(decoded, v);
        }
    }

    /// Heartbeat z polami `nsys_available` / `nsys_version` round-trip rkyv —
    /// peer odbierajacy ramke musi widziec capability nadawcy. Walidacja
    /// schematu po dodaniu pol w PR3b (advertisement Nsight w heartbeat).
    #[test]
    fn nsight_capability_in_heartbeat_round_trip() {
        let hb = HeartbeatMetrics {
            cpu_usage_percent: 12.5,
            ram_used_mb: 2048,
            gpus: vec![],
            containers: vec![],
            networks: vec![],
            platform: "linux".to_string(),
            cpu_temperature_c: Some(55.0),
            swap_total_mb: 0,
            swap_used_mb: 0,
            connected_peers: vec!["abc".to_string()],
            active_requests: 1,
            tokens_per_sec: 42.0,
            nsys_available: true,
            nsys_version: "2024.5.1".to_string(),
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&hb).expect("encode");
        let decoded =
            rkyv::from_bytes::<HeartbeatMetrics, rkyv::rancor::Error>(&bytes).expect("decode");
        assert!(decoded.nsys_available);
        assert_eq!(decoded.nsys_version, "2024.5.1");
        assert_eq!(decoded.platform, "linux");
        assert_eq!(decoded.connected_peers, vec!["abc".to_string()]);
    }
}
