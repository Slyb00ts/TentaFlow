// =============================================================================
// Plik: mesh.rs
// Opis: Typy wiadomosci mesh dla komunikacji gossip, membership, CRDT sync
//       i service discovery miedzy nodami TentaFlow.AI przez QUIC.
// =============================================================================

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

use crate::profiling::{
    ProfilingActiveInfoRequest, ProfilingActiveInfoResponse, ProfilingDeleteRequest,
    ProfilingDeleteResponse, ProfilingDownloadRequest, ProfilingDownloadResponse,
    ProfilingReportRequest, ProfilingReportResponse, ProfilingSessionsRequest,
    ProfilingSessionsResponse, ProfilingStartRequest, ProfilingStartResponse, ProfilingStopRequest,
    ProfilingStopResponse,
};

// =============================================================================
// Typ operacji CRDT
// =============================================================================

/// Rodzaj operacji CRDT przesylanej w synchronizacji stanu.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub enum CrdtOpType {
    /// LWW-Register - ustawienie wartosci
    SetValue(String),
    /// OR-Set - dodanie elementu
    AddElement(String),
    /// OR-Set - usuniecie elementu
    RemoveElement(String),
}

// =============================================================================
// Operacja synchronizacji CRDT
// =============================================================================

/// Pojedyncza operacja CRDT w serializowalnej formie.
/// Zawiera zegar logiczny (czas + hash noda) do rozwiazywania konfliktow.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct CrdtSyncOp {
    /// Czas logiczny operacji
    pub clock_time: u64,
    /// Hash identyfikatora noda (czesc zegara wektorowego)
    pub clock_node_hash: u64,
    /// Klucz danych ktorego dotyczy operacja
    pub key: String,
    /// Typ operacji CRDT
    pub op_type: CrdtOpType,
}

// =============================================================================
// Glowny enum wiadomosci mesh
// =============================================================================

/// Wiadomosc protokolu mesh - gossip, membership, CRDT, service discovery
/// i forwarding requestow miedzy nodami.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub enum MeshMessage {
    // -- Gossip --
    /// Ping do sprawdzenia czy nod zyje
    Ping { from: String, incarnation: u64 },

    /// Odpowiedz na ping
    PingAck { from: String, incarnation: u64 },

    /// Posredni ping przez inny nod (protocol SWIM)
    IndirectPing {
        from: String,
        target: String,
        incarnation: u64,
    },

    // -- Membership --
    /// Dolaczenie noda do mesh
    Join {
        node_id: String,
        addr: String,
        role: String,
        capabilities: Vec<String>,
    },

    /// Opuszczenie mesh przez nod
    Leave { node_id: String },

    // -- CRDT sync --
    /// Synchronizacja stanu CRDT - lista operacji do zaaplikowania
    StateSync {
        from: String,
        operations: Vec<CrdtSyncOp>,
    },

    /// Zadanie synchronizacji stanu od podanego czasu
    StateSyncRequest { from: String, since_time: u64 },

    // -- Forwarding --
    /// Przekazanie requestu do innego noda
    ForwardRequest {
        request_id: String,
        target_node: String,
        payload: Vec<u8>,
    },

    /// Odpowiedz na przekazany request
    ForwardResponse {
        request_id: String,
        payload: Vec<u8>,
    },

    // -- Stale polaczenia QUIC --
    /// Heartbeat wysylany co 500ms na stalym polaczeniu
    Heartbeat(MeshHeartbeat),

    /// Pelna wymiana stanu po nawiazaniu polaczenia QUIC
    FullStateExchange(MeshFullState),

    /// Przyrostowa synchronizacja CRDT (delta)
    CrdtDeltaSync {
        from: String,
        operations: Vec<CrdtSyncOp>,
        version_vector: Vec<(u64, u64)>,
    },

    /// Aktualizacja listy modeli na nodzie
    ModelListUpdate {
        node_id: String,
        models: Vec<MeshModelInfo>,
    },

    /// Aktualizacja listy kontenerow na nodzie
    ContainerListUpdate {
        node_id: String,
        containers: Vec<MeshContainerInfo>,
    },

    // -- Parowanie mesh (bezpieczenstwo) --
    /// Zadanie parowania — wysylane do noda po mDNS discovery
    PairingRequest { from_node_id: String, pin: String },

    /// Potwierdzenie parowania — wymiana kluczy publicznych
    PairingConfirm {
        from_node_id: String,
        public_key: Vec<u8>,
    },

    /// Odrzucenie parowania
    PairingReject { from_node_id: String },

    /// Cofniecie zaufania — node nie jest juz zaufany
    TrustRevoked { node_id: String },

    /// Synchronizacja kluczy zaufanych nodow po zatwierdzeniu parowania
    TrustedKeysSync { keys: Vec<(String, Vec<u8>)> },

    /// Rotacja klucza szyfrowania — wymiana ephemeral X25519 public key
    KeyRotation {
        from_node_id: String,
        ephemeral_public_key: String,
    },

    /// Odpowiedz na rotacje klucza — zawiera ephemeral public key drugiej strony
    KeyRotationResponse {
        from_node_id: String,
        ephemeral_public_key: String,
    },

    /// Graceful leave — node opuszcza mesh (nie revoke, chwilowe odlaczenie)
    NodeLeaving { node_id: String },

    // -- Komendy zarzadzania --
    /// Komenda zarzadzania wyslana do sparowanego noda
    MeshCommand {
        command_id: String,
        from_node_id: String,
        command: MeshCommandType,
    },

    /// Odpowiedz na komende zarzadzania — typed payload zamiast goluego stringa.
    MeshCommandResponse {
        command_id: String,
        from_node_id: String,
        ok: bool,
        payload: MeshCommandResponsePayload,
        error: Option<String>,
    },

    /// Streaming postepu deploy (od noda wykonujacego)
    MeshDeployProgress {
        command_id: String,
        from_node_id: String,
        phase: String,
        message: String,
        percent: u8,
        is_done: bool,
    },

    /// Fragment logow kontenera (streaming)
    MeshLogChunk {
        command_id: String,
        from_node_id: String,
        container_id: String,
        data: String,
        is_stderr: bool,
        is_done: bool,
    },

    // -- Cluster --
    /// Informacja o clusterze nodow
    ClusterInfo {
        cluster_id: String,
        name: String,
        node_ids: Vec<String>,
        strategy: String,
    },

}

// =============================================================================
// Metryki GPU w heartbeat
// =============================================================================

/// Metryki pojedynczego GPU przesylane w heartbeat.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshGpuMetric {
    /// Indeks GPU na nodzie
    pub index: u32,
    /// Uzycie GPU w procentach (0-100)
    pub usage_percent: f32,
    /// Zuzycie VRAM w MB
    pub vram_used_mb: u64,
    /// Calkowita VRAM w MB
    pub vram_total_mb: u64,
    /// Temperatura GPU w stopniach Celsjusza
    pub temperature_c: f32,
}

// =============================================================================
// Heartbeat stalego polaczenia QUIC
// =============================================================================

/// Heartbeat wysylany co 500ms na stalym polaczeniu QUIC.
/// Zawiera metryki zasobow noda do load balancingu.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshHeartbeat {
    /// Identyfikator noda
    pub node_id: String,
    /// Hostname noda
    pub hostname: String,
    /// Adresy IP noda (np. ["192.168.1.10", "10.0.0.5"])
    pub ip_addresses: Vec<String>,
    /// Timestamp w milisekundach (unix epoch)
    pub timestamp_ms: u64,
    /// Uzycie CPU w procentach (0-100)
    pub cpu_usage_percent: f32,
    /// Zuzycie RAM w MB
    pub ram_used_mb: u64,
    /// Calkowita pamiec RAM w MB
    pub ram_total_mb: u64,
    /// Metryki poszczegolnych GPU
    pub gpu_metrics: Vec<MeshGpuMetric>,
    /// Srednie obciazenie systemu (1 minuta)
    pub load_avg_1m: f32,
    /// Liczba aktywnych requestow
    pub active_requests: u32,
    /// Platforma noda: "linux", "macos", "windows", "android", "ios"
    pub platform: String,
    /// Liczba serwisow uruchomionych na nodzie
    pub services_count: u32,
    /// Czy Docker jest uruchomiony
    pub docker_running: bool,
}

// =============================================================================
// Informacja o modelu AI
// =============================================================================

/// Opis modelu AI zaladowanego na nodzie mesh.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshModelInfo {
    /// Nazwa modelu
    pub name: String,
    /// Rozmiar modelu w bajtach
    pub size_bytes: u64,
    /// Backend inferencyjny (np. "llama.cpp", "vllm")
    pub backend: String,
    /// Maksymalny rozmiar kontekstu w tokenach
    pub max_context: u32,
    /// Kwantyzacja modelu (np. "Q4_K_M", "FP16")
    pub quantization: String,
}

// =============================================================================
// Informacja o kontenerze Docker
// =============================================================================

/// Opis kontenera Docker dzialajacego na nodzie mesh.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshContainerInfo {
    /// Identyfikator kontenera
    pub id: String,
    /// Nazwa kontenera
    pub name: String,
    /// Obraz Docker
    pub image: String,
    /// Status kontenera (np. "running", "exited")
    pub status: String,
    /// Lista mapowanych portow (np. "8080:80")
    pub ports: Vec<String>,
    /// Uzycie CPU w procentach
    pub cpu_percent: f32,
    /// Uzycie pamieci w MB
    pub memory_mb: u64,
}

// =============================================================================
// Pelny stan noda po nawiazaniu polaczenia QUIC
// =============================================================================

/// Pelna wymiana stanu po nawiazaniu polaczenia QUIC.
/// Wysylana jednokrotnie przy handshake, potem aktualizacje przyrostowe.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshFullState {
    /// Identyfikator noda
    pub node_id: String,
    /// Hostname noda
    pub hostname: String,
    /// Adresy IP noda
    pub ip_addresses: Vec<String>,
    /// Rola noda w mesh (np. "router", "desktop", "mobile")
    pub role: String,
    /// Lista zdolnosci noda
    pub capabilities: Vec<String>,
    /// Zaladowane modele AI
    pub models: Vec<MeshModelInfo>,
    /// Dzialajace kontenery Docker
    pub containers: Vec<MeshContainerInfo>,
    /// Operacje CRDT do synchronizacji
    pub crdt_operations: Vec<CrdtSyncOp>,
    /// Wektor wersji: pary (hash_noda, czas_logiczny)
    pub version_vector: Vec<(u64, u64)>,
    /// Platforma noda: "linux", "macos", "windows", "android", "ios"
    pub platform: String,
    /// Liczba rdzeni CPU
    pub cpu_count: u32,
    /// Czy Docker jest dostepny na nodzie
    pub docker_available: bool,
    /// Wersja Docker (pusty string jesli niedostepny)
    pub docker_version: String,
    /// Identyfikator clustera (jesli nod nalezy do clustera)
    pub cluster_id: Option<String>,
}

// =============================================================================
// Typ komendy zarzadzania
// =============================================================================

/// Rodzaj komendy zarzadzania wysylanej przez mesh do sparowanego noda.
/// Obejmuje operacje Docker, certyfikaty i serwisy.
#[derive(Archive, Deserialize, Serialize, Clone, SerdeSerialize, SerdeDeserialize)]
pub enum MeshCommandType {
    /// Uruchomienie kontenera
    ContainerStart {
        container_id: String,
    },
    /// Zatrzymanie kontenera
    ContainerStop {
        container_id: String,
    },
    /// Restart kontenera
    ContainerRestart {
        container_id: String,
    },
    /// Lista kontenerow
    ListContainers,
    /// Lista obrazow Docker
    ListImages,
    /// Czyszczenie Docker (prune)
    SystemPrune {
        volumes: bool,
    },
    /// Wgranie certyfikatow TLS
    ProvisionCerts {
        cert_pem: String,
        key_pem: String,
        target_dir: String,
    },
    /// Dodanie serwisu na nodzie
    AddService {
        service_config: String,
    },
    /// Zmiana konfiguracji sieciowej na zdalnym nodzie
    NetworkConfig {
        interface: String,
        ipv4: Option<String>,
        netmask: Option<String>,
        gateway: Option<String>,
        dhcp: bool,
        sudo_password: String,
    },
    /// Probe przepustowosci sieci miedzy nodami (TCP multi-stream lub RDMA)
    BandwidthProbe {
        target_ip: String,
        target_port: u16,
        rdma_port: u16,
        bind_interface: String,
        duration_ms: u32,
        mode: String,
        nonce: Vec<u8>,
        num_streams: u8,
    },
    /// Anulowanie probing sesji
    BandwidthProbeCancel,

    /// Multi-source profiling: start sesji.
    ProfilingStart(ProfilingStartRequest),
    /// Multi-source profiling: stop sesji + zwrot pelnego raportu.
    ProfilingStop(ProfilingStopRequest),
    /// Multi-source profiling: lista sesji widocznych na nodzie.
    ProfilingSessions(ProfilingSessionsRequest),
    /// Multi-source profiling: pobranie raportu.
    ProfilingReport(ProfilingReportRequest),
    /// Multi-source profiling: usuniecie sesji.
    ProfilingDelete(ProfilingDeleteRequest),
    /// Multi-source profiling: pobranie tar.gz z calym katalogiem sesji.
    ProfilingDownload(ProfilingDownloadRequest),
    /// Multi-source profiling: snapshot aktywnej sesji (Some) albo None.
    ProfilingActiveInfo(ProfilingActiveInfoRequest),

    // -- Cross-node service action forwarding (krok N3b). `service_id` is
    //    interpreted in the receiver's local SQLite namespace; the receiver
    //    runs the action against its own DB and returns the result.
    ServiceStartRemote {
        service_id: i64,
    },
    ServiceDeleteRemote {
        service_id: i64,
    },
    ServicePinRemote {
        service_id: i64,
        pinned: bool,
    },
    ServicePauseRemote {
        service_id: i64,
        paused: bool,
    },
    /// Forwarded `ServiceManifestDeployRequest`. The receiver re-runs the same
    /// validation + tokio::spawn deploy that a local request would, and
    /// returns the synchronously generated `deploy_id` (slug). Logs continue
    /// to flow on the receiver's local websocket bus — cross-node log
    /// streaming is intentionally not part of N3b.
    ServiceDeployRemote {
        engine_id: String,
        deploy_method: String,
        config_json: String,
    },
}

// =============================================================================
// Typed payload odpowiedzi na komende mesh
// =============================================================================

/// Typed payload odpowiedzi na `MeshCommandType`. Zastepuje `output: String`,
/// zeby kazda komenda miala scisle zdefiniowany typ wyniku — bez parsowania
/// JSON-a w warstwie aplikacyjnej.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub enum MeshCommandResponsePayload {
    /// Komendy void — zwracaja sam status (start/stop/restart/remove kontenera,
    /// add service, deploy stack, pull image, provision certs, bandwidth-cancel).
    Empty,
    /// Lista kontenerow zwracana przez `ListContainers`.
    ContainerList(Vec<MeshContainerInfo>),
    /// Lista obrazow zwracana przez `ListImages`.
    ImageList(Vec<String>),
    /// Wynik probing przepustowosci (server side: porty otwarte do polaczenia).
    BandwidthProbeServerStarted { tcp_port: u16, rdma_port: u16 },
    /// Wynik probing przepustowosci (client side: zmierzone metryki).
    BandwidthProbeClientResult {
        bandwidth_mbps: f64,
        bytes_transferred: u64,
        duration_ms: u64,
        latency_us: u64,
        streams_completed: u8,
        rdma: bool,
    },
    /// Nieforemny tekst — uzywany tylko dla `SystemPrune` (human-readable summary
    /// zwracane przez Docker daemon) i `NetworkConfig` (diagnostyczny output).
    Text(String),

    /// Multi-source profiling: potwierdzenie startu sesji.
    ProfilingStart(ProfilingStartResponse),
    /// Multi-source profiling: zatrzymanie + raport ProfileReportV2.
    ProfilingStop(ProfilingStopResponse),
    /// Multi-source profiling: lista sesji.
    ProfilingSessions(ProfilingSessionsResponse),
    /// Multi-source profiling: raport sesji.
    ProfilingReport(ProfilingReportResponse),
    /// Multi-source profiling: potwierdzenie usuniecia.
    ProfilingDelete(ProfilingDeleteResponse),
    /// Multi-source profiling: tar.gz katalogu sesji.
    ProfilingDownload(ProfilingDownloadResponse),
    /// Multi-source profiling: snapshot aktywnej sesji.
    ProfilingActiveInfo(ProfilingActiveInfoResponse),

    /// Cross-node service action result (stop/delete/pin/pause/rename) — the
    /// generic ok/error already lives in the outer `MeshCommandResponse`, so
    /// the payload is `Empty` for all five.
    ServiceActionResult,
    /// Cross-node deploy result — carries the slug allocated by the receiver
    /// so the initiator can wire the deploy log websocket back to that node.
    ServiceDeployResult {
        deploy_id: String,
        engine_id: String,
        deploy_method: String,
    },
}

impl std::fmt::Debug for MeshCommandType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ContainerStart { container_id } => f
                .debug_struct("ContainerStart")
                .field("container_id", container_id)
                .finish(),
            Self::ContainerStop { container_id } => f
                .debug_struct("ContainerStop")
                .field("container_id", container_id)
                .finish(),
            Self::ContainerRestart { container_id } => f
                .debug_struct("ContainerRestart")
                .field("container_id", container_id)
                .finish(),
            Self::ListContainers => write!(f, "ListContainers"),
            Self::ListImages => write!(f, "ListImages"),
            Self::SystemPrune { volumes } => f
                .debug_struct("SystemPrune")
                .field("volumes", volumes)
                .finish(),
            Self::ProvisionCerts {
                cert_pem: _,
                key_pem: _,
                target_dir,
            } => f
                .debug_struct("ProvisionCerts")
                .field("cert_pem", &"[CERT]")
                .field("key_pem", &"***")
                .field("target_dir", target_dir)
                .finish(),
            Self::AddService { service_config } => f
                .debug_struct("AddService")
                .field("service_config", service_config)
                .finish(),
            Self::NetworkConfig {
                interface,
                ipv4,
                netmask,
                gateway,
                dhcp,
                sudo_password: _,
            } => f
                .debug_struct("NetworkConfig")
                .field("interface", interface)
                .field("ipv4", ipv4)
                .field("netmask", netmask)
                .field("gateway", gateway)
                .field("dhcp", dhcp)
                .field("sudo_password", &"***")
                .finish(),
            Self::BandwidthProbe {
                target_ip, mode, ..
            } => f
                .debug_struct("BandwidthProbe")
                .field("target_ip", target_ip)
                .field("mode", mode)
                .finish(),
            Self::BandwidthProbeCancel => write!(f, "BandwidthProbeCancel"),
            Self::ProfilingStart(req) => f
                .debug_struct("ProfilingStart")
                .field("node_id", &req.node_id)
                .field("label", &req.label)
                .field("elevation_password", &"***")
                .finish(),
            Self::ProfilingStop(req) => f
                .debug_struct("ProfilingStop")
                .field("node_id", &req.node_id)
                .field("session_id", &req.session_id)
                .finish(),
            Self::ProfilingSessions(req) => f
                .debug_struct("ProfilingSessions")
                .field("node_id", &req.node_id)
                .finish(),
            Self::ProfilingReport(req) => f
                .debug_struct("ProfilingReport")
                .field("node_id", &req.node_id)
                .field("session_id", &req.session_id)
                .finish(),
            Self::ProfilingDelete(req) => f
                .debug_struct("ProfilingDelete")
                .field("node_id", &req.node_id)
                .field("session_id", &req.session_id)
                .finish(),
            Self::ProfilingDownload(req) => f
                .debug_struct("ProfilingDownload")
                .field("node_id", &req.node_id)
                .field("session_id", &req.session_id)
                .finish(),
            Self::ProfilingActiveInfo(req) => f
                .debug_struct("ProfilingActiveInfo")
                .field("node_id", &req.node_id)
                .finish(),
            Self::ServiceStartRemote { service_id } => f
                .debug_struct("ServiceStartRemote")
                .field("service_id", service_id)
                .finish(),
            Self::ServiceDeleteRemote { service_id } => f
                .debug_struct("ServiceDeleteRemote")
                .field("service_id", service_id)
                .finish(),
            Self::ServicePinRemote { service_id, pinned } => f
                .debug_struct("ServicePinRemote")
                .field("service_id", service_id)
                .field("pinned", pinned)
                .finish(),
            Self::ServicePauseRemote { service_id, paused } => f
                .debug_struct("ServicePauseRemote")
                .field("service_id", service_id)
                .field("paused", paused)
                .finish(),
            Self::ServiceDeployRemote {
                engine_id,
                deploy_method,
                ..
            } => f
                .debug_struct("ServiceDeployRemote")
                .field("engine_id", engine_id)
                .field("deploy_method", deploy_method)
                .finish(),
        }
    }
}

// =============================================================================
// Discriminant bytes dla identyfikacji wiadomosci na streamach QUIC
// =============================================================================

pub const MESH_MSG_HEARTBEAT: u8 = 0x10;
pub const MESH_MSG_CRDT_DELTA: u8 = 0x11;
pub const MESH_MSG_FULL_STATE: u8 = 0x12;
pub const MESH_MSG_FORWARD_REQ: u8 = 0x13;
pub const MESH_MSG_FORWARD_RES: u8 = 0x14;
pub const MESH_MSG_MODEL_LIST: u8 = 0x15;
pub const MESH_MSG_CONTAINER_LIST: u8 = 0x16;
pub const MESH_MSG_NODE_INFO: u8 = 0x18;
/// Minimal hello — hostname + platform. Wysylany przy kazdym PeerConnected
/// (trusted LUB discovered), zeby GUI mogl pokazac ludzka nazwe (spark-002)
/// zamiast skrotu hex przed zakonczeniem pairingu.
pub const MESH_MSG_HELLO: u8 = 0x19;
/// Gossip topologii mesh — floodowany z dedupem (origin, epoch) i TTL.
/// Kazdy zaufany peer broadcastuje swoj wpis co 30s; kazdy odbiorca rebroadcastuje
/// do swoich bezposrednich sasiadow (oprocz nadawcy). Dzieki temu mainpc dowiaduje sie
/// o spark-002 przez spark-001 — z nazwa, platforma i lista uslug.
pub const MESH_MSG_TOPOLOGY_ANNOUNCE: u8 = 0x1A;
/// Lekki anons 'oto kogo znam' — wysylany do nowo podlaczonego peera bez
/// wymagania zaufania. Rozwiazuje scenariusz 3 nodow na VLAN gdzie mDNS
/// multicast jest blokowany miedzy czescia klientow (typowe na enterprise
/// switches z IGMP snooping / client isolation). Tylko node_id + hostname +
/// adresy — bez uslug/modeli (pre-pairing = pre-trust).
pub const MESH_MSG_KNOWN_PEERS: u8 = 0x1B;
pub const MESH_MSG_PAIRING_REQUEST: u8 = 0x20;
pub const MESH_MSG_PAIRING_CONFIRM: u8 = 0x21;
pub const MESH_MSG_PAIRING_REJECT: u8 = 0x22;
pub const MESH_MSG_TRUST_REVOKED: u8 = 0x23;
pub const MESH_MSG_TRUSTED_KEYS_SYNC: u8 = 0x24;
pub const MESH_MSG_COMMAND: u8 = 0x30;
pub const MESH_MSG_COMMAND_RESPONSE: u8 = 0x31;
pub const MESH_MSG_DEPLOY_PROGRESS: u8 = 0x32;
pub const MESH_MSG_LOG_CHUNK: u8 = 0x33;
pub const MESH_MSG_CLUSTER_INFO: u8 = 0x36;
pub const MESH_MSG_KEY_ROTATION: u8 = 0x25;
pub const MESH_MSG_KEY_ROTATION_RESPONSE: u8 = 0x26;
pub const MESH_MSG_NODE_LEAVING: u8 = 0x27;
pub const MESH_MSG_RELAY_FRAME: u8 = 0x37;
pub const MESH_MSG_FORWARD_STREAM_REQ: u8 = 0x38;
pub const MESH_MSG_ALIAS_SYNC: u8 = 0x39;
/// Pull request: nowo polaczony peer prosi o pelny snapshot serwisow.
pub const MESH_MSG_SERVICES_GET: u8 = 0x40;
/// Odpowiedz na `MESH_MSG_SERVICES_GET` — pelen snapshot lokalnego nodu.
pub const MESH_MSG_SERVICES_GET_RESPONSE: u8 = 0x41;
/// Periodyczny anti-drift broadcast pelnego stanu serwisow (co ~5min).
pub const MESH_MSG_SERVICES_ANNOUNCE: u8 = 0x42;
/// Push delta — pojedyncza zmiana (deploy/stop/pin/pause/rename/delete).
pub const MESH_MSG_SERVICES_UPDATE: u8 = 0x43;

// =============================================================================
// Struktury wire format dla nowych wiadomosci mesh (rkyv zero-copy)
// =============================================================================

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize, Archive, Deserialize, Serialize)]
#[rkyv(derive(Debug))]
pub struct TrustRevokedPayload {
    pub revoked_node_id: String,
    pub from_node_id: String,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize, Archive, Deserialize, Serialize)]
#[rkyv(derive(Debug))]
pub struct KeyRotationPayload {
    pub from_node_id: String,
    pub ephemeral_public_key: String,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize, Archive, Deserialize, Serialize)]
#[rkyv(derive(Debug))]
pub struct KeyRotationResponsePayload {
    pub from_node_id: String,
    pub ephemeral_public_key: String,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize, Archive, Deserialize, Serialize)]
#[rkyv(derive(Debug))]
pub struct TrustedKeyEntry {
    pub node_id: String,
    pub public_key_hex: String,
}

/// Minimal payload dla `MESH_MSG_HELLO` — tylko hostname + platform + OS.
/// Wysylany do kazdego peera (trusted/discovered) po nawiazaniu polaczenia,
/// zeby GUI mogl pokazac nazwe hosta przed zakończeniem pairingu.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize, Archive, Deserialize, Serialize)]
#[rkyv(derive(Debug))]
pub struct MeshHelloPayload {
    pub hostname: String,
    pub platform: String,
    pub os_info: String,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize, Archive, Deserialize, Serialize)]
#[rkyv(derive(Debug))]
pub struct TrustedKeysSyncPayload {
    pub keys: Vec<TrustedKeyEntry>,
}

/// Wire payload dla `MESH_MSG_PAIRING_REQUEST` — wysylany przez istniejacy mesh
/// stream przez inicjatora parowania. `from_node_id` to Ed25519 pubkey hex
/// (= iroh endpoint id). `public_key` to X25519 pubkey hex uzywany do KEX.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize, Archive, Deserialize, Serialize)]
#[rkyv(derive(Debug))]
pub struct MeshPairingRequestPayload {
    pub from_node_id: String,
    pub public_key: String,
    pub pin: String,
}

/// Wire payload dla `MESH_MSG_PAIRING_CONFIRM` — wysylany w odpowiedzi przez
/// receivera po walidacji PIN-u przez admina.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize, Archive, Deserialize, Serialize)]
#[rkyv(derive(Debug))]
pub struct MeshPairingConfirmPayload {
    pub from_node_id: String,
    pub public_key: String,
    pub hostname: String,
    pub pin: String,
}

/// Wire payload dla `MESH_MSG_PAIRING_REJECT` — wysylany gdy admin odrzuca prosbe.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize, Archive, Deserialize, Serialize)]
#[rkyv(derive(Debug))]
pub struct MeshPairingRejectPayload {
    pub from_node_id: String,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize, Archive, Deserialize, Serialize)]
#[rkyv(derive(Debug))]
pub struct NodeLeavingPayload {
    pub node_id: String,
}

/// Podsumowanie uslugi dostepnej na zdalnym nodzie — wysylane w TopologyAnnounce.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize, Archive, Deserialize, Serialize)]
#[rkyv(derive(Debug))]
pub struct ServiceSummary {
    pub name: String,
    pub service_type: String,
    pub ready: bool,
}

/// Podsumowanie modelu zaladowanego na zdalnym nodzie — wysylane w TopologyAnnounce.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize, Archive, Deserialize, Serialize)]
#[rkyv(derive(Debug))]
pub struct ModelSummary {
    pub alias: String,
    pub backend: String,
    pub loaded: bool,
}

/// Jeden wpis w TopologyAnnounce — metadane noda + jego bezposredni sasiedzi + uslugi.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize, Archive, Deserialize, Serialize)]
#[rkyv(derive(Debug))]
pub struct TopologyEntry {
    pub node_id: String,
    pub hostname: String,
    pub platform: String,
    pub os_info: String,
    pub connected_to: Vec<String>,
    pub services: Vec<ServiceSummary>,
    pub models: Vec<ModelSummary>,
    pub direct_addrs: Vec<String>,
    pub port: u16,
}

/// Pojedynczy wpis w KnownPeersPayload — minimalne dane potrzebne zeby
/// spoznionemu nodowi udalo sie dial'nac peera bez polegania na mDNS.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize, Archive, Deserialize, Serialize)]
#[rkyv(derive(Debug))]
pub struct KnownPeerEntry {
    pub node_id: String,
    pub hostname: String,
    pub direct_addrs: Vec<String>,
    pub port: u16,
}

/// Payload KnownPeers — wysylany po PeerConnected przez nowo podlaczonego
/// peera. Zawiera liste wszystkich aktualnie polaczonych peerow, zeby odbiorca
/// mogl proboxac sie z nimi polaczyc bez mDNS.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize, Archive, Deserialize, Serialize)]
#[rkyv(derive(Debug))]
pub struct KnownPeersPayload {
    pub peers: Vec<KnownPeerEntry>,
}

/// Payload gossip topologii — floodowany z dedupem.
/// `origin_node_id` + `epoch` identyfikuja unikalna wersje wiadomosci.
/// `ttl` zmniejszane przy kazdym rebroadcascie (start 5, drop przy 0).
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize, Archive, Deserialize, Serialize)]
#[rkyv(derive(Debug))]
pub struct TopologyAnnouncePayload {
    pub origin_node_id: String,
    pub epoch: u64,
    pub ttl: u8,
    pub entries: Vec<TopologyEntry>,
}

/// Ramka relay do multi-hop routingu — payload zaszyfrowany end-to-end kluczem docelowego noda.
/// `discriminant` informuje odbiorce jaki typ wiadomosci jest w srodku.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize, Archive, Deserialize, Serialize)]
#[rkyv(derive(Debug))]
pub struct MeshRelayFrame {
    pub request_id: String,
    pub source_node_id: String,
    pub destination_node_id: String,
    pub ttl: u8,
    pub discriminant: u8,
    pub payload: Vec<u8>,
}

// =============================================================================
// Mesh services registry — wire payloads (krok N3a)
// =============================================================================
//
// Cross-node services sync flows over four discriminants 0x40..0x43. The full
// `ServiceInfo` struct lives in `message_body` (it is also returned by the
// local `ServiceListRequest`); we re-use it here so receivers can drop a
// snapshot straight into the in-memory `MeshServicesRegistry`.

/// Pull request: nowo polaczony peer prosi o pelny snapshot serwisow.
#[derive(Debug, Clone, Archive, Deserialize, Serialize)]
#[rkyv(derive(Debug))]
pub struct MeshServicesGetPayload {
    pub from_node_id: String,
}

/// Odpowiedz na `MeshServicesGetPayload` — pelen snapshot lokalnego nodu.
#[derive(Debug, Clone, Archive, Deserialize, Serialize)]
#[rkyv(derive(Debug))]
pub struct MeshServicesGetResponsePayload {
    pub from_node_id: String,
    pub services: Vec<crate::message_body::ServiceInfo>,
}

/// Periodyczny anti-drift broadcast (co ~5 min). Pelen stan zastepuje to co
/// odbiorca trzyma w `MeshServicesRegistry` dla danego nodu.
#[derive(Debug, Clone, Archive, Deserialize, Serialize)]
#[rkyv(derive(Debug))]
pub struct MeshServicesAnnouncePayload {
    pub from_node_id: String,
    pub services: Vec<crate::message_body::ServiceInfo>,
}

/// Push delta — wysylane natychmiast po lokalnej mutacji (deploy/stop/pin/
/// pause/rename/delete). Odbiorca aplikuje `change` na swoim widoku nodu.
#[derive(Debug, Clone, Archive, Deserialize, Serialize)]
#[rkyv(derive(Debug))]
pub struct MeshServicesUpdatePayload {
    pub from_node_id: String,
    pub change: crate::message_body::ServiceChange,
}

// =============================================================================
// Typy protokolu meeting bot sidecar
// =============================================================================

/// Wiadomosc transkrypcji z sidecara meeting bot
#[derive(Archive, Deserialize, Serialize, Debug, Clone, SerdeSerialize, SerdeDeserialize)]
#[rkyv(derive(Debug))]
pub struct MeetingTranscript {
    /// Nazwa mowcy
    pub speaker: String,
    /// Tekst transkrypcji
    pub text: String,
    /// Timestamp w milisekundach (unix epoch)
    pub timestamp_ms: u64,
}

/// Komenda mowienia wysylana do sidecara meeting bot (TTS)
#[derive(Archive, Deserialize, Serialize, Debug, Clone, SerdeSerialize, SerdeDeserialize)]
#[rkyv(derive(Debug))]
pub struct MeetingSpeakCommand {
    /// Tekst do wypowiedzenia
    pub text: String,
    /// Identyfikator glosu TTS
    pub voice: String,
    /// Model TTS do uzycia
    pub model: String,
}

/// Kontrola spotkania — komendy i zdarzenia miedzy addonem a sidecarem
#[derive(Archive, Deserialize, Serialize, Debug, Clone, SerdeSerialize, SerdeDeserialize)]
#[rkyv(derive(Debug))]
pub enum MeetingControl {
    /// Dolacz do spotkania pod podanym URL
    Join { meeting_url: String },
    /// Opusc spotkanie
    Leave,
    /// Wycisz/odcisz mikrofon
    Mute { muted: bool },
    /// Zmiana stanu spotkania (zdarzenie z sidecara)
    StateChanged { state: MeetingState },
    /// Healthcheck sidecara
    SidecarHealth { healthy: bool, uptime_s: u64 },
}

/// Stan spotkania raportowany przez sidecar
#[derive(Archive, Deserialize, Serialize, Debug, Clone, SerdeSerialize, SerdeDeserialize)]
#[rkyv(derive(Debug))]
pub enum MeetingState {
    /// Laczenie ze spotkaniem
    Joining,
    /// Polaczony
    Connected,
    /// Ponowne laczenie po utracie polaczenia
    Reconnecting,
    /// Spotkanie zakonczone
    Ended { reason: String },
    /// Autoryzacja wygasla
    AuthExpired,
    /// Wyrzucony ze spotkania
    Kicked { reason: String },
}

// =============================================================================
// Helpery serializacji
// =============================================================================

impl MeshMessage {
    /// Serializacja do bajtow rkyv (zero-copy format).
    /// Zwraca AlignedVec ktory derefuje do &[u8].
    pub fn serialize_rkyv(&self) -> Result<rkyv::util::AlignedVec, rkyv::rancor::Error> {
        rkyv::to_bytes::<rkyv::rancor::Error>(self)
    }

    /// Deserializacja z bajtow rkyv (zero-copy access do archived formy)
    pub fn deserialize_rkyv(bytes: &[u8]) -> Result<&ArchivedMeshMessage, rkyv::rancor::Error> {
        rkyv::access::<ArchivedMeshMessage, rkyv::rancor::Error>(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ping_roundtrip() {
        let msg = MeshMessage::Ping {
            from: "node-1".to_string(),
            incarnation: 42,
        };

        let bytes = msg
            .serialize_rkyv()
            .expect("Serializacja ping powinna sie udac");
        let archived =
            MeshMessage::deserialize_rkyv(&bytes).expect("Deserializacja ping powinna sie udac");

        match archived {
            ArchivedMeshMessage::Ping { from, incarnation } => {
                assert_eq!(from.as_str(), "node-1");
                assert_eq!(*incarnation, 42);
            }
            _ => panic!("Oczekiwano wariantu Ping"),
        }
    }

    #[test]
    fn test_join_roundtrip() {
        let msg = MeshMessage::Join {
            node_id: "node-2".to_string(),
            addr: "192.168.1.10:4433".to_string(),
            role: "worker".to_string(),
            capabilities: vec!["llm".to_string(), "embedding".to_string()],
        };

        let bytes = msg
            .serialize_rkyv()
            .expect("Serializacja join powinna sie udac");
        let archived =
            MeshMessage::deserialize_rkyv(&bytes).expect("Deserializacja join powinna sie udac");

        match archived {
            ArchivedMeshMessage::Join {
                node_id,
                addr,
                role,
                capabilities,
            } => {
                assert_eq!(node_id.as_str(), "node-2");
                assert_eq!(addr.as_str(), "192.168.1.10:4433");
                assert_eq!(role.as_str(), "worker");
                assert_eq!(capabilities.len(), 2);
            }
            _ => panic!("Oczekiwano wariantu Join"),
        }
    }

    #[test]
    fn test_state_sync_roundtrip() {
        let ops = vec![
            CrdtSyncOp {
                clock_time: 100,
                clock_node_hash: 0xDEAD,
                key: "model/status".to_string(),
                op_type: CrdtOpType::SetValue("ready".to_string()),
            },
            CrdtSyncOp {
                clock_time: 101,
                clock_node_hash: 0xBEEF,
                key: "active_models".to_string(),
                op_type: CrdtOpType::AddElement("llama3".to_string()),
            },
        ];

        let msg = MeshMessage::StateSync {
            from: "node-3".to_string(),
            operations: ops,
        };

        let bytes = msg
            .serialize_rkyv()
            .expect("Serializacja state sync powinna sie udac");
        let archived = MeshMessage::deserialize_rkyv(&bytes)
            .expect("Deserializacja state sync powinna sie udac");

        match archived {
            ArchivedMeshMessage::StateSync { from, operations } => {
                assert_eq!(from.as_str(), "node-3");
                assert_eq!(operations.len(), 2);
            }
            _ => panic!("Oczekiwano wariantu StateSync"),
        }
    }

    #[test]
    fn test_forward_roundtrip() {
        let payload = vec![1u8, 2, 3, 4, 5];
        let msg = MeshMessage::ForwardRequest {
            request_id: "req-001".to_string(),
            target_node: "node-5".to_string(),
            payload: payload.clone(),
        };

        let bytes = msg
            .serialize_rkyv()
            .expect("Serializacja forward powinna sie udac");
        let archived =
            MeshMessage::deserialize_rkyv(&bytes).expect("Deserializacja forward powinna sie udac");

        match archived {
            ArchivedMeshMessage::ForwardRequest {
                request_id,
                target_node,
                payload: archived_payload,
            } => {
                assert_eq!(request_id.as_str(), "req-001");
                assert_eq!(target_node.as_str(), "node-5");
                assert_eq!(archived_payload.as_slice(), &[1, 2, 3, 4, 5]);
            }
            _ => panic!("Oczekiwano wariantu ForwardRequest"),
        }
    }

    // =========================================================================
    // Testy typow meeting bot
    // =========================================================================

    /// Pomocnicza makra do roundtrip testow rkyv dla typow meeting bot.
    /// Serializuje do bajtow i deserializuje z archived — zwraca &Archived.
    macro_rules! rkyv_serialize {
        ($value:expr) => {
            rkyv::to_bytes::<rkyv::rancor::Error>($value)
                .expect("Serializacja rkyv powinna sie udac")
        };
    }

    #[test]
    fn test_meeting_transcript_roundtrip() {
        // Serializacja i deserializacja transkrypcji spotkania
        let transcript = MeetingTranscript {
            speaker: "Jan Kowalski".to_string(),
            text: "Dzien dobry, zaczynamy spotkanie.".to_string(),
            timestamp_ms: 1_710_000_000_000,
        };

        let bytes = rkyv_serialize!(&transcript);
        let archived = rkyv::access::<ArchivedMeetingTranscript, rkyv::rancor::Error>(&bytes)
            .expect("Dostep do archived powinna sie udac");

        assert_eq!(archived.speaker.as_str(), "Jan Kowalski");
        assert_eq!(archived.text.as_str(), "Dzien dobry, zaczynamy spotkanie.");
        assert_eq!(archived.timestamp_ms, 1_710_000_000_000);
    }

    #[test]
    fn test_meeting_transcript_empty_fields() {
        // Transkrypcja z pustymi polami
        let transcript = MeetingTranscript {
            speaker: "".to_string(),
            text: "".to_string(),
            timestamp_ms: 0,
        };

        let bytes = rkyv_serialize!(&transcript);
        let archived = rkyv::access::<ArchivedMeetingTranscript, rkyv::rancor::Error>(&bytes)
            .expect("Dostep do archived powinna sie udac");

        assert_eq!(archived.speaker.as_str(), "");
        assert_eq!(archived.text.as_str(), "");
        assert_eq!(archived.timestamp_ms, 0);
    }

    #[test]
    fn test_meeting_speak_command_roundtrip() {
        // Serializacja komendy mowienia TTS
        let cmd = MeetingSpeakCommand {
            text: "Prosze o ciszę.".to_string(),
            voice: "alloy".to_string(),
            model: "tts-1".to_string(),
        };

        let bytes = rkyv_serialize!(&cmd);
        let archived = rkyv::access::<ArchivedMeetingSpeakCommand, rkyv::rancor::Error>(&bytes)
            .expect("Dostep do archived powinna sie udac");

        assert_eq!(archived.text.as_str(), "Prosze o ciszę.");
        assert_eq!(archived.voice.as_str(), "alloy");
        assert_eq!(archived.model.as_str(), "tts-1");
    }

    #[test]
    fn test_meeting_control_join_roundtrip() {
        let ctrl = MeetingControl::Join {
            meeting_url: "https://teams.microsoft.com/l/meetup-join/abc".to_string(),
        };

        let bytes = rkyv_serialize!(&ctrl);
        let archived = rkyv::access::<ArchivedMeetingControl, rkyv::rancor::Error>(&bytes)
            .expect("Dostep do archived powinna sie udac");

        match archived {
            ArchivedMeetingControl::Join { meeting_url } => {
                assert_eq!(
                    meeting_url.as_str(),
                    "https://teams.microsoft.com/l/meetup-join/abc"
                );
            }
            _ => panic!("Oczekiwano wariantu Join"),
        }
    }

    #[test]
    fn test_meeting_control_leave_roundtrip() {
        let ctrl = MeetingControl::Leave;

        let bytes = rkyv_serialize!(&ctrl);
        let archived = rkyv::access::<ArchivedMeetingControl, rkyv::rancor::Error>(&bytes)
            .expect("Dostep do archived powinna sie udac");

        assert!(matches!(archived, ArchivedMeetingControl::Leave));
    }

    #[test]
    fn test_meeting_control_mute_roundtrip() {
        // Mute i unmute
        for muted_val in [true, false] {
            let ctrl = MeetingControl::Mute { muted: muted_val };

            let bytes = rkyv_serialize!(&ctrl);
            let archived = rkyv::access::<ArchivedMeetingControl, rkyv::rancor::Error>(&bytes)
                .expect("Dostep do archived powinna sie udac");

            match archived {
                ArchivedMeetingControl::Mute { muted } => assert_eq!(*muted, muted_val),
                _ => panic!("Oczekiwano wariantu Mute"),
            }
        }
    }

    #[test]
    fn test_meeting_control_state_changed_joining() {
        let ctrl = MeetingControl::StateChanged {
            state: MeetingState::Joining,
        };

        let bytes = rkyv_serialize!(&ctrl);
        let archived = rkyv::access::<ArchivedMeetingControl, rkyv::rancor::Error>(&bytes)
            .expect("Dostep do archived powinna sie udac");

        match archived {
            ArchivedMeetingControl::StateChanged { state } => {
                assert!(matches!(state, ArchivedMeetingState::Joining));
            }
            _ => panic!("Oczekiwano wariantu StateChanged"),
        }
    }

    #[test]
    fn test_meeting_control_state_changed_connected() {
        let ctrl = MeetingControl::StateChanged {
            state: MeetingState::Connected,
        };

        let bytes = rkyv_serialize!(&ctrl);
        let archived = rkyv::access::<ArchivedMeetingControl, rkyv::rancor::Error>(&bytes)
            .expect("Dostep do archived powinna sie udac");

        match archived {
            ArchivedMeetingControl::StateChanged { state } => {
                assert!(matches!(state, ArchivedMeetingState::Connected));
            }
            _ => panic!("Oczekiwano wariantu StateChanged"),
        }
    }

    #[test]
    fn test_meeting_control_state_changed_reconnecting() {
        let ctrl = MeetingControl::StateChanged {
            state: MeetingState::Reconnecting,
        };

        let bytes = rkyv_serialize!(&ctrl);
        let archived = rkyv::access::<ArchivedMeetingControl, rkyv::rancor::Error>(&bytes)
            .expect("Dostep do archived powinna sie udac");

        match archived {
            ArchivedMeetingControl::StateChanged { state } => {
                assert!(matches!(state, ArchivedMeetingState::Reconnecting));
            }
            _ => panic!("Oczekiwano wariantu StateChanged"),
        }
    }

    #[test]
    fn test_meeting_control_state_changed_ended() {
        let ctrl = MeetingControl::StateChanged {
            state: MeetingState::Ended {
                reason: "host ended".to_string(),
            },
        };

        let bytes = rkyv_serialize!(&ctrl);
        let archived = rkyv::access::<ArchivedMeetingControl, rkyv::rancor::Error>(&bytes)
            .expect("Dostep do archived powinna sie udac");

        match archived {
            ArchivedMeetingControl::StateChanged { state } => match state {
                ArchivedMeetingState::Ended { reason } => {
                    assert_eq!(reason.as_str(), "host ended");
                }
                _ => panic!("Oczekiwano MeetingState::Ended"),
            },
            _ => panic!("Oczekiwano wariantu StateChanged"),
        }
    }

    #[test]
    fn test_meeting_control_state_changed_auth_expired() {
        let ctrl = MeetingControl::StateChanged {
            state: MeetingState::AuthExpired,
        };

        let bytes = rkyv_serialize!(&ctrl);
        let archived = rkyv::access::<ArchivedMeetingControl, rkyv::rancor::Error>(&bytes)
            .expect("Dostep do archived powinna sie udac");

        match archived {
            ArchivedMeetingControl::StateChanged { state } => {
                assert!(matches!(state, ArchivedMeetingState::AuthExpired));
            }
            _ => panic!("Oczekiwano wariantu StateChanged"),
        }
    }

    #[test]
    fn test_meeting_control_state_changed_kicked() {
        let ctrl = MeetingControl::StateChanged {
            state: MeetingState::Kicked {
                reason: "disruption".to_string(),
            },
        };

        let bytes = rkyv_serialize!(&ctrl);
        let archived = rkyv::access::<ArchivedMeetingControl, rkyv::rancor::Error>(&bytes)
            .expect("Dostep do archived powinna sie udac");

        match archived {
            ArchivedMeetingControl::StateChanged { state } => match state {
                ArchivedMeetingState::Kicked { reason } => {
                    assert_eq!(reason.as_str(), "disruption");
                }
                _ => panic!("Oczekiwano MeetingState::Kicked"),
            },
            _ => panic!("Oczekiwano wariantu StateChanged"),
        }
    }

    #[test]
    fn test_meeting_control_sidecar_health_roundtrip() {
        let ctrl = MeetingControl::SidecarHealth {
            healthy: true,
            uptime_s: 3600,
        };

        let bytes = rkyv_serialize!(&ctrl);
        let archived = rkyv::access::<ArchivedMeetingControl, rkyv::rancor::Error>(&bytes)
            .expect("Dostep do archived powinna sie udac");

        match archived {
            ArchivedMeetingControl::SidecarHealth { healthy, uptime_s } => {
                assert!(*healthy);
                assert_eq!(*uptime_s, 3600);
            }
            _ => panic!("Oczekiwano wariantu SidecarHealth"),
        }
    }

    #[test]
    fn test_meeting_state_ended_with_reason() {
        let state = MeetingState::Ended {
            reason: "Meeting ended by host".to_string(),
        };

        let bytes = rkyv_serialize!(&state);
        let archived = rkyv::access::<ArchivedMeetingState, rkyv::rancor::Error>(&bytes)
            .expect("Dostep do archived powinna sie udac");

        match archived {
            ArchivedMeetingState::Ended { reason } => {
                assert_eq!(reason.as_str(), "Meeting ended by host");
            }
            _ => panic!("Oczekiwano wariantu Ended"),
        }
    }

    #[test]
    fn test_meeting_state_kicked_with_reason() {
        let state = MeetingState::Kicked {
            reason: "Removed by moderator".to_string(),
        };

        let bytes = rkyv_serialize!(&state);
        let archived = rkyv::access::<ArchivedMeetingState, rkyv::rancor::Error>(&bytes)
            .expect("Dostep do archived powinna sie udac");

        match archived {
            ArchivedMeetingState::Kicked { reason } => {
                assert_eq!(reason.as_str(), "Removed by moderator");
            }
            _ => panic!("Oczekiwano wariantu Kicked"),
        }
    }

    #[test]
    fn test_meeting_transcript_serde_json_roundtrip() {
        // Serializacja/deserializacja JSON transkrypcji
        let transcript = MeetingTranscript {
            speaker: "Anna Nowak".to_string(),
            text: "Test JSON roundtrip".to_string(),
            timestamp_ms: 999,
        };

        let json = serde_json::to_string(&transcript).unwrap();
        let result: MeetingTranscript = serde_json::from_str(&json).unwrap();
        assert_eq!(result.speaker, "Anna Nowak");
        assert_eq!(result.text, "Test JSON roundtrip");
        assert_eq!(result.timestamp_ms, 999);
    }

    #[test]
    fn test_meeting_control_serde_json_roundtrip() {
        // JSON roundtrip dla kazdego wariantu MeetingControl
        let controls = vec![
            MeetingControl::Join {
                meeting_url: "https://test".to_string(),
            },
            MeetingControl::Leave,
            MeetingControl::Mute { muted: true },
            MeetingControl::SidecarHealth {
                healthy: false,
                uptime_s: 0,
            },
        ];

        for ctrl in &controls {
            let json = serde_json::to_string(ctrl).unwrap();
            let result: MeetingControl = serde_json::from_str(&json).unwrap();
            assert_eq!(
                std::mem::discriminant(&result),
                std::mem::discriminant(ctrl)
            );
        }
    }

    #[test]
    fn test_serde_json_roundtrip() {
        let msg = MeshMessage::Ping {
            from: "node-1".to_string(),
            incarnation: 7,
        };

        let json = serde_json::to_string(&msg).expect("Serializacja JSON powinna sie udac");
        let deserialized: MeshMessage =
            serde_json::from_str(&json).expect("Deserializacja JSON powinna sie udac");

        match deserialized {
            MeshMessage::Ping { from, incarnation } => {
                assert_eq!(from, "node-1");
                assert_eq!(incarnation, 7);
            }
            _ => panic!("Oczekiwano wariantu Ping"),
        }
    }

    #[test]
    fn test_heartbeat_roundtrip() {
        let msg = MeshMessage::Heartbeat(MeshHeartbeat {
            node_id: "node-10".to_string(),
            hostname: "worker-01".to_string(),
            ip_addresses: vec!["192.168.1.10".to_string()],
            timestamp_ms: 1_710_000_000_000,
            cpu_usage_percent: 42.5,
            ram_used_mb: 16384,
            ram_total_mb: 32768,
            gpu_metrics: vec![
                MeshGpuMetric {
                    index: 0,
                    usage_percent: 87.3,
                    vram_used_mb: 20000,
                    vram_total_mb: 24576,
                    temperature_c: 72.0,
                },
                MeshGpuMetric {
                    index: 1,
                    usage_percent: 15.0,
                    vram_used_mb: 2048,
                    vram_total_mb: 24576,
                    temperature_c: 45.5,
                },
            ],
            load_avg_1m: 3.14,
            active_requests: 8,
            platform: "linux".to_string(),
            services_count: 3,
            docker_running: true,
        });

        let bytes = msg
            .serialize_rkyv()
            .expect("Serializacja heartbeat powinna sie udac");
        let archived = MeshMessage::deserialize_rkyv(&bytes)
            .expect("Deserializacja heartbeat powinna sie udac");

        match archived {
            ArchivedMeshMessage::Heartbeat(hb) => {
                assert_eq!(hb.node_id.as_str(), "node-10");
                assert_eq!(hb.timestamp_ms, 1_710_000_000_000);
                assert_eq!(hb.gpu_metrics.len(), 2);
                assert_eq!(hb.gpu_metrics[0].index, 0);
                assert_eq!(hb.gpu_metrics[1].vram_total_mb, 24576);
                assert_eq!(hb.active_requests, 8);
            }
            _ => panic!("Oczekiwano wariantu Heartbeat"),
        }
    }

    #[test]
    fn test_full_state_roundtrip() {
        let msg = MeshMessage::FullStateExchange(MeshFullState {
            node_id: "node-20".to_string(),
            hostname: "gpu-farm-01".to_string(),
            ip_addresses: vec!["10.0.0.20".to_string()],
            role: "worker".to_string(),
            capabilities: vec!["llm".to_string(), "tts".to_string()],
            models: vec![MeshModelInfo {
                name: "llama3-70b".to_string(),
                size_bytes: 40_000_000_000,
                backend: "vllm".to_string(),
                max_context: 8192,
                quantization: "FP16".to_string(),
            }],
            containers: vec![MeshContainerInfo {
                id: "abc123".to_string(),
                name: "vllm-server".to_string(),
                image: "vllm/vllm:latest".to_string(),
                status: "running".to_string(),
                ports: vec!["8000:8000".to_string()],
                cpu_percent: 55.0,
                memory_mb: 4096,
            }],
            crdt_operations: vec![CrdtSyncOp {
                clock_time: 200,
                clock_node_hash: 0xCAFE,
                key: "status".to_string(),
                op_type: CrdtOpType::SetValue("active".to_string()),
            }],
            version_vector: vec![(0xCAFE, 200), (0xBEEF, 150)],
            platform: "linux".to_string(),
            cpu_count: 16,
            docker_available: true,
            docker_version: "24.0.7".to_string(),
            cluster_id: Some("gpu-farm".to_string()),
        });

        let bytes = msg
            .serialize_rkyv()
            .expect("Serializacja full state powinna sie udac");
        let archived = MeshMessage::deserialize_rkyv(&bytes)
            .expect("Deserializacja full state powinna sie udac");

        match archived {
            ArchivedMeshMessage::FullStateExchange(state) => {
                assert_eq!(state.node_id.as_str(), "node-20");
                assert_eq!(state.role.as_str(), "worker");
                assert_eq!(state.capabilities.len(), 2);
                assert_eq!(state.models.len(), 1);
                assert_eq!(state.models[0].name.as_str(), "llama3-70b");
                assert_eq!(state.models[0].max_context, 8192);
                assert_eq!(state.containers.len(), 1);
                assert_eq!(state.containers[0].name.as_str(), "vllm-server");
                assert_eq!(state.crdt_operations.len(), 1);
                assert_eq!(state.version_vector.len(), 2);
            }
            _ => panic!("Oczekiwano wariantu FullStateExchange"),
        }
    }

    #[test]
    fn mesh_command_response_payload_variants_round_trip() {
        let payloads = vec![
            MeshCommandResponsePayload::Empty,
            MeshCommandResponsePayload::ImageList(vec!["img-a".into(), "img-b".into()]),
            MeshCommandResponsePayload::BandwidthProbeServerStarted {
                tcp_port: 5001,
                rdma_port: 5002,
            },
            MeshCommandResponsePayload::BandwidthProbeClientResult {
                bandwidth_mbps: 9876.5,
                bytes_transferred: 1_000_000,
                duration_ms: 2000,
                latency_us: 120,
                streams_completed: 4u8,
                rdma: false,
            },
            MeshCommandResponsePayload::Text("Total reclaimed space: 1.2GB".into()),
        ];
        for p in payloads {
            let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&p).expect("encode");
            rkyv::from_bytes::<MeshCommandResponsePayload, rkyv::rancor::Error>(&bytes)
                .expect("decode");
        }
    }
}
