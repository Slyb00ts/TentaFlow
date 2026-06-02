// =============================================================================
// Plik: mesh.rs
// Opis: Typy wiadomosci mesh dla komunikacji gossip, membership
//       i service discovery miedzy nodami TentaFlow.AI przez QUIC.
// =============================================================================

use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

use crate::profiling::{
    ProfilingActiveInfoRequest, ProfilingActiveInfoResponse, ProfilingDeleteRequest,
    ProfilingDeleteResponse, ProfilingDownloadRequest, ProfilingDownloadResponse,
    ProfilingReportRequest, ProfilingReportResponse, ProfilingSessionsRequest,
    ProfilingSessionsResponse, ProfilingStartRequest, ProfilingStartResponse, ProfilingStopRequest,
    ProfilingStopResponse,
};

// =============================================================================
// Glowny enum wiadomosci mesh
// =============================================================================

/// Wiadomosc protokolu mesh - gossip, membership, service discovery
/// i forwarding requestow miedzy nodami.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
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
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
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
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
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
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
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
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
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
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
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
#[derive(Clone, SerdeSerialize, SerdeDeserialize)]
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
    /// Forwarded `ServiceUpdateRequest`. Receiver wykonuje pełną logikę
    /// edycji serwisu (merge config_json, opcjonalny stop+respawn), tak
    /// samo jak handler na lokalnym nodzie. Zwraca status przez
    /// `ServiceActionResult` (success/failure z message).
    ServiceUpdateRemote {
        service_id: i64,
        model_repo: Option<String>,
        model_preset_id: Option<String>,
        gpu_memory_utilization: Option<f32>,
        max_model_len: Option<u32>,
        max_num_seqs: Option<u32>,
        max_num_batched_tokens: Option<u32>,
        kv_cache_dtype: Option<String>,
        chunked_prefill: Option<bool>,
        vllm_args_override: Option<String>,
        pinned: Option<bool>,
        paused: Option<bool>,
        restart_after_save: bool,
    },
}

// =============================================================================
// Typed payload odpowiedzi na komende mesh
// =============================================================================

/// Typed payload odpowiedzi na `MeshCommandType`. Zastepuje `output: String`,
/// zeby kazda komenda miala scisle zdefiniowany typ wyniku — bez parsowania
/// JSON-a w warstwie aplikacyjnej.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
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
            Self::ServiceUpdateRemote {
                service_id,
                restart_after_save,
                ..
            } => f
                .debug_struct("ServiceUpdateRemote")
                .field("service_id", service_id)
                .field("restart_after_save", restart_after_save)
                .finish(),
        }
    }
}

// =============================================================================
// Discriminant bytes dla identyfikacji wiadomosci na streamach QUIC
// =============================================================================

pub const MESH_MSG_HEARTBEAT: u8 = 0x10;
pub const MESH_MSG_FORWARD_REQ: u8 = 0x13;
pub const MESH_MSG_MODEL_LIST: u8 = 0x15;
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
/// Online authority storage request for central-only addon data.
pub const MESH_MSG_STORAGE_PROXY_REQUEST: u8 = 0x34;
/// Online authority storage response for central-only addon data.
pub const MESH_MSG_STORAGE_PROXY_RESPONSE: u8 = 0x35;
pub const MESH_MSG_NODE_LEAVING: u8 = 0x27;
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
/// Multi-node HMAC issuer key sync (F1b P3.B). Carries this peer's
/// pickup_token / frame_url / recording_url 32-byte HMAC keys (current +
/// optional previous-window key) so the receiver can verify tokens issued
/// by this peer. Sent only between trust-paired peers; verifier-only, never
/// used by the receiver for signing.
pub const MESH_MSG_HMAC_KEYS_SYNC: u8 = 0x44;

/// F1b P3.C — frame proxy request. Sent by a peer that received a signed
/// `frame_url` for a frame whose `raw_ref` is not held locally. The receiver
/// (the frame's owning node) looks up the frame in its local store and
/// replies with `MESH_MSG_FRAME_PROXY_RESPONSE`. Trusted-peer only; the
/// pre-trust whitelist intentionally excludes this discriminant.
pub const MESH_MSG_FRAME_PROXY_REQUEST: u8 = 0x45;

/// F1b P3.C — frame proxy response. Returns the encoded frame bytes plus the
/// wire-stable metadata mirror, or a typed miss (NotFound / Unavailable).
/// Trusted-peer only.
pub const MESH_MSG_FRAME_PROXY_RESPONSE: u8 = 0x46;
/// Sync Ledger push — paczka podpisanych operacji dla zaufanego peera.
pub const MESH_MSG_SYNC_PUSH: u8 = 0x47;
/// Sync Ledger ACK — potwierdzenie operacji zapisanych przez odbiorce.
pub const MESH_MSG_SYNC_ACK: u8 = 0x48;
/// Sync Ledger pull — prosba o zakres operacji z partycji.
pub const MESH_MSG_SYNC_PULL: u8 = 0x49;
/// Sync Ledger pull response — zakres operacji z partycji.
pub const MESH_MSG_SYNC_PULL_RESPONSE: u8 = 0x4A;
/// Sync Ledger snapshot pull — prosba o pakiet snapshotu partycji.
pub const MESH_MSG_SYNC_SNAPSHOT_PULL: u8 = 0x4B;
/// Sync Ledger snapshot response — metadane snapshotu, blob i tail operacji.
pub const MESH_MSG_SYNC_SNAPSHOT_RESPONSE: u8 = 0x4C;

// =============================================================================
// Struktury wire format dla nowych wiadomosci mesh (CBOR zero-copy)
// =============================================================================

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct TrustRevokedPayload {
    pub revoked_node_id: String,
    pub from_node_id: String,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct KeyRotationPayload {
    pub from_node_id: String,
    pub ephemeral_public_key: String,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct KeyRotationResponsePayload {
    pub from_node_id: String,
    pub ephemeral_public_key: String,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct TrustedKeyEntry {
    pub node_id: String,
    pub public_key_hex: String,
}

/// Minimal payload dla `MESH_MSG_HELLO` — tylko hostname + platform + OS.
/// Wysylany do kazdego peera (trusted/discovered) po nawiazaniu polaczenia,
/// zeby GUI mogl pokazac nazwe hosta przed zakończeniem pairingu.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshHelloPayload {
    pub hostname: String,
    pub platform: String,
    pub os_info: String,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct TrustedKeysSyncPayload {
    pub keys: Vec<TrustedKeyEntry>,
}

/// One scope of the local node's HMAC issuer key state, mirrored to a trusted
/// peer so it can verify tokens we issued.
///
/// `scope` is the wire-stable issuer name: "pickup_token", "frame_url",
/// "recording_url" (matches `services::key_storage` file names). `current_key`
/// is the active 32-byte HMAC secret. `previous_key` carries the still-valid
/// previous-window key after a rotation; `previous_expires_unix_ms = 0`
/// signals no previous-window key is active.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct HmacKeyEntry {
    pub scope: String,
    pub current_key: Vec<u8>,
    pub previous_key: Vec<u8>,
    pub previous_expires_unix_ms: u64,
    /// Truncated SHA-256 of `current_key` — diagnostic only, never used as
    /// trust input. Kept short (8 bytes) to keep log lines readable.
    pub key_id: Vec<u8>,
}

/// Payload of `MESH_MSG_HMAC_KEYS_SYNC` — one entry per issuer scope held by
/// the sender. F1b P3.B sends three: pickup_token, frame_url, recording_url.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct HmacKeysSyncPayload {
    pub from_node_id: String,
    pub keys: Vec<HmacKeyEntry>,
}

/// F1b P3.C — wire-stable mirror of `services::frame_storage::FrameMetadata`.
/// The in-memory struct uses a Rust `enum` for pixel format and an `Option`
/// for the PTS, which do not round-trip cleanly through every CBOR
/// derivation chain we support. The wire form keeps everything as primitives
/// + strings so receivers in any language / version pair safely.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct FrameMetadataWire {
    pub camera_id: String,
    pub width: u32,
    pub height: u32,
    /// Wire-stable pixel format name. Currently always "rgb24" (only
    /// variant produced by the F1a GStreamer pipeline); new connectors
    /// will add additional names here without breaking older receivers.
    pub pixel_format: String,
    pub timestamp_unix_ms: u64,
}

/// F1b P3.C — payload of `MESH_MSG_FRAME_PROXY_REQUEST`.
///
/// `raw_ref` is the storage-layer reference embedded in the signed `frame_url`
/// (already validated by the receiver of the URL before this message is
/// sent). `request_id` is generated by the requester and copied back into
/// the response so the requester can match async replies — multiple
/// in-flight requests share the same uni-stream peer connection.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct FrameProxyRequestPayload {
    pub raw_ref: String,
    pub request_id: String,
}

/// F1b P3.C — payload of `MESH_MSG_FRAME_PROXY_RESPONSE`.
///
/// `Found` carries the encoded frame bytes + metadata. `NotFound` is sent
/// when the frame's owning node has no record of `raw_ref` (already
/// evicted, never existed). `Unavailable` covers the mid-fetch failure
/// case (source connector disconnected while we were assembling the
/// response, IO error, etc.) so the requester can distinguish a hard miss
/// from a transient one.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub enum FrameProxyResponsePayload {
    Found {
        raw_ref: String,
        request_id: String,
        bytes: Vec<u8>,
        metadata: FrameMetadataWire,
    },
    NotFound {
        raw_ref: String,
        request_id: String,
    },
    Unavailable {
        raw_ref: String,
        request_id: String,
        reason: String,
    },
}

/// Wire payload dla `MESH_MSG_PAIRING_REQUEST` — wysylany przez istniejacy mesh
/// stream przez inicjatora parowania. `from_node_id` to Ed25519 pubkey hex
/// (= iroh endpoint id). `public_key` to X25519 pubkey hex uzywany do KEX.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshPairingRequestPayload {
    pub from_node_id: String,
    pub public_key: String,
    pub pin: String,
}

/// Payload pierwszego kontaktu na osobnym ALPN `tentaflow-pairing/v2`.
/// Kodowany CBOR-em, nie JSON-em. `sender_node_id` musi byc rowny iroh
/// `remote_id`, a pierwsze 64 znaki `sender_public_key_hex` musza byc tym
/// samym Ed25519 public key.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct PairingFirstContactRequest {
    pub sender_node_id: String,
    pub sender_public_key_hex: String,
    pub sender_hostname: String,
    pub pin: String,
    pub sender_addresses: Vec<String>,
    pub sender_relay_url: String,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct PairingTrustedKeyEntry {
    pub node_id: String,
    pub public_key_hex: String,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PairingFirstContactResponse {
    Confirm {
        receiver_public_key_hex: String,
        receiver_hostname: String,
        trusted_keys: Vec<PairingTrustedKeyEntry>,
    },
    Pending {
        receiver_hostname: String,
    },
    Reject {
        reason: String,
    },
}

/// Wire payload dla `MESH_MSG_PAIRING_CONFIRM` — wysylany w odpowiedzi przez
/// receivera po walidacji PIN-u przez admina.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshPairingConfirmPayload {
    pub from_node_id: String,
    pub public_key: String,
    pub hostname: String,
    pub pin: String,
}

/// Wire payload dla `MESH_MSG_PAIRING_REJECT` — wysylany gdy admin odrzuca prosbe.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshPairingRejectPayload {
    pub from_node_id: String,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct NodeLeavingPayload {
    pub node_id: String,
}

/// Podsumowanie uslugi dostepnej na zdalnym nodzie — wysylane w TopologyAnnounce.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct ServiceSummary {
    pub name: String,
    pub service_type: String,
    pub ready: bool,
}

/// Podsumowanie modelu zaladowanego na zdalnym nodzie — wysylane w TopologyAnnounce.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct ModelSummary {
    pub alias: String,
    pub backend: String,
    pub loaded: bool,
}

/// Jeden wpis w TopologyAnnounce — metadane noda + jego bezposredni sasiedzi + uslugi.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
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
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct KnownPeerEntry {
    pub node_id: String,
    pub hostname: String,
    pub direct_addrs: Vec<String>,
    pub port: u16,
}

/// Payload KnownPeers — wysylany po PeerConnected przez nowo podlaczonego
/// peera. Zawiera liste wszystkich aktualnie polaczonych peerow, zeby odbiorca
/// mogl proboxac sie z nimi polaczyc bez mDNS.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct KnownPeersPayload {
    pub peers: Vec<KnownPeerEntry>,
}

/// Payload gossip topologii — floodowany z dedupem.
/// `origin_node_id` + `epoch` identyfikuja unikalna wersje wiadomosci.
/// `ttl` zmniejszane przy kazdym rebroadcascie (start 5, drop przy 0).
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct TopologyAnnouncePayload {
    pub origin_node_id: String,
    pub epoch: u64,
    pub ttl: u8,
    pub entries: Vec<TopologyEntry>,
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
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshServicesGetPayload {
    pub from_node_id: String,
}

/// Odpowiedz na `MeshServicesGetPayload` — pelen snapshot lokalnego nodu.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshServicesGetResponsePayload {
    pub from_node_id: String,
    pub services: Vec<crate::message_body::ServiceInfo>,
}

/// Periodyczny anti-drift broadcast (co ~5 min). Pelen stan zastepuje to co
/// odbiorca trzyma w `MeshServicesRegistry` dla danego nodu.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshServicesAnnouncePayload {
    pub from_node_id: String,
    pub services: Vec<crate::message_body::ServiceInfo>,
}

/// Push delta — wysylane natychmiast po lokalnej mutacji (deploy/stop/pin/
/// pause/rename/delete). Odbiorca aplikuje `change` na swoim widoku nodu.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshServicesUpdatePayload {
    pub from_node_id: String,
    pub change: crate::message_body::ServiceChange,
}

// =============================================================================
// Sync Ledger — wire payloads
// =============================================================================

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshSyncOperationWire {
    pub op_id: Vec<u8>,
    pub partition_id: String,
    pub partition_sequence: u64,
    pub operation: Vec<u8>,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshSyncPushPayload {
    pub from_node_id: String,
    pub operations: Vec<MeshSyncOperationWire>,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshSyncAckPayload {
    pub from_node_id: String,
    pub operation_ids: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshSyncPullPayload {
    pub from_node_id: String,
    pub partition_id: String,
    pub from_sequence: u64,
    pub limit: u32,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshSyncPullResponsePayload {
    pub from_node_id: String,
    pub partition_id: String,
    pub from_sequence: u64,
    pub operations: Vec<MeshSyncOperationWire>,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshSyncSnapshotPullPayload {
    pub from_node_id: String,
    pub partition_id: String,
    pub up_to_sequence: u64,
    pub snapshot_id: String,
    pub include_tail: bool,
    pub tail_limit: u32,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshSyncSnapshotResponsePayload {
    pub from_node_id: String,
    pub partition_id: String,
    pub up_to_sequence: u64,
    pub snapshot_id: String,
    pub snapshot_bytes: Vec<u8>,
    pub blob_bytes: Vec<u8>,
    pub operations_after_snapshot: Vec<MeshSyncOperationWire>,
}

// =============================================================================
// Sync Ledger — baseline-adopt pairing (faza A: definicje typow)
// =============================================================================
//
// Baseline adopt: gdy nowy node dolacza do mesh, wybierany jest donor, ktory
// przesyla pelny baseline core'a (snapshot tabel) w chunkach. Typy ponizej
// opisuja wire format negocjacji donora i transferu. Faza A dodaje wylacznie
// definicje + (de)serializacje; podpiecie do ledgera nastapi w fazie B.

/// Monotoniczny epoch baseline'u. Porzadek leksykograficzny (counter,
/// origin_node) daje deterministyczny tie-break gdy dwa nody wybija ten sam
/// licznik.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BaselineEpoch {
    pub counter: u64,
    pub origin_node: String,
}

impl Ord for BaselineEpoch {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.counter
            .cmp(&other.counter)
            .then_with(|| self.origin_node.cmp(&other.origin_node))
    }
}

impl PartialOrd for BaselineEpoch {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Propozycja donora baseline'u wyslana przez dolaczajacy node.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct BaselineElect {
    pub node_id: String,
    pub proposed_donor: String,
    pub epoch_seen: u64,
}

/// Odpowiedz donora na `BaselineElect` — akceptacja albo odrzucenie roli donora.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct BaselineAck {
    pub accepted: bool,
    pub donor: String,
    pub joiner: String,
    pub epoch: u64,
}

/// Naglowek transferu baseline'u — opisuje co i ile zostanie przeslane przed
/// pierwszym chunkiem.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct BaselineHeader {
    pub schema_version: u32,
    pub epoch: u64,
    pub tables: Vec<String>,
    pub row_counts: Vec<u64>,
    pub total_bytes: u64,
    pub max_bytes: u64,
}

/// Pojedynczy chunk baseline'u. `content_hash` to 32-bajtowy hash `bytes`,
/// weryfikowany przez odbiorce przed zlozeniem snapshotu.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct BaselineChunk {
    pub seq: u64,
    pub content_hash: [u8; 32],
    pub bytes: Vec<u8>,
}

/// Potwierdzenie pojedynczego chunka baseline'u.
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct BaselineChunkAck {
    pub seq: u64,
    pub ok: bool,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub enum StorageValueWire {
    Null,
    Bool(bool),
    I64(i64),
    F64(f64),
    Text(String),
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub enum StorageProxyRequestKind {
    SqlExec {
        query: String,
        params: Vec<StorageValueWire>,
    },
    SqlQuery {
        query: String,
        params: Vec<StorageValueWire>,
        one: bool,
        limit: Option<u32>,
    },
    KvGet {
        instance_id: String,
        key: String,
    },
    KvSet {
        instance_id: String,
        key: String,
        value: Vec<u8>,
    },
    KvDelete {
        instance_id: String,
        key: String,
    },
    KvList {
        instance_id: String,
        prefix: Option<String>,
    },
    BlobGetChunk {
        sha256: String,
        offset: u64,
        length: u32,
    },
    BlobPutChunk {
        blob_id: String,
        sha256: String,
        mime: String,
        size_bytes: u64,
        chunk_index: u32,
        chunk_count: u32,
        chunk_sha256: String,
        bytes: Vec<u8>,
    },
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct StorageProxyRequestPayload {
    pub request_id: String,
    pub from_node_id: String,
    pub org_id: String,
    pub addon_id: String,
    pub resource_type: String,
    pub resource_id: String,
    pub actor_user_id: Option<i64>,
    pub kind: StorageProxyRequestKind,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub enum StorageProxyResponseKind {
    SqlExec {
        rows_affected: u64,
        last_insert_id: i64,
    },
    SqlRows {
        columns: Vec<String>,
        rows: Vec<Vec<StorageValueWire>>,
    },
    SqlOne {
        row: Option<Vec<StorageValueWire>>,
    },
    KvValue {
        value: Option<Vec<u8>>,
    },
    KvWrite {
        rows_affected: u64,
    },
    KvKeys {
        keys: Vec<String>,
    },
    BlobChunk {
        sha256: String,
        mime: String,
        size_bytes: u64,
        offset: u64,
        bytes: Vec<u8>,
    },
    BlobWrite {
        blob_id: String,
        sha256: String,
        complete: bool,
        received_chunks: u32,
    },
    Error {
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct StorageProxyResponsePayload {
    pub request_id: String,
    pub from_node_id: String,
    pub kind: StorageProxyResponseKind,
}

// =============================================================================
// Typy protokolu meeting bot sidecar
// =============================================================================

/// Wiadomosc transkrypcji z sidecara meeting bot
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingTranscript {
    /// Nazwa mowcy
    pub speaker: String,
    /// Tekst transkrypcji
    pub text: String,
    /// Timestamp w milisekundach (unix epoch)
    pub timestamp_ms: u64,
}

/// Komenda mowienia wysylana do sidecara meeting bot (TTS)
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeetingSpeakCommand {
    /// Tekst do wypowiedzenia
    pub text: String,
    /// Identyfikator glosu TTS
    pub voice: String,
    /// Model TTS do uzycia
    pub model: String,
}

/// Kontrola spotkania — komendy i zdarzenia miedzy addonem a sidecarem
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
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
#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize)]
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
    /// Serializacja do bajtow CBOR.
    pub fn serialize_cbor(&self) -> Result<Vec<u8>, String> {
        crate::cbor::encode(self)
    }

    /// Deserializacja z bajtow CBOR.
    pub fn deserialize_cbor(bytes: &[u8]) -> Result<MeshMessage, String> {
        crate::cbor::decode(bytes)
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
            .serialize_cbor()
            .expect("Serializacja ping powinna sie udac");
        let decoded =
            MeshMessage::deserialize_cbor(&bytes).expect("Deserializacja ping powinna sie udac");

        match decoded {
            MeshMessage::Ping { from, incarnation } => {
                assert_eq!(from.as_str(), "node-1");
                assert_eq!(incarnation, 42);
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
            .serialize_cbor()
            .expect("Serializacja join powinna sie udac");
        let decoded =
            MeshMessage::deserialize_cbor(&bytes).expect("Deserializacja join powinna sie udac");

        match decoded {
            MeshMessage::Join {
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
    fn test_forward_roundtrip() {
        let payload = vec![1u8, 2, 3, 4, 5];
        let msg = MeshMessage::ForwardRequest {
            request_id: "req-001".to_string(),
            target_node: "node-5".to_string(),
            payload: payload.clone(),
        };

        let bytes = msg
            .serialize_cbor()
            .expect("Serializacja forward powinna sie udac");
        let decoded =
            MeshMessage::deserialize_cbor(&bytes).expect("Deserializacja forward powinna sie udac");

        match decoded {
            MeshMessage::ForwardRequest {
                request_id,
                target_node,
                payload: decoded_payload,
            } => {
                assert_eq!(request_id.as_str(), "req-001");
                assert_eq!(target_node.as_str(), "node-5");
                assert_eq!(decoded_payload.as_slice(), &[1, 2, 3, 4, 5]);
            }
            _ => panic!("Oczekiwano wariantu ForwardRequest"),
        }
    }

    // =========================================================================
    // Testy typow baseline-adopt
    // =========================================================================

    #[test]
    fn baseline_epoch_orders_by_counter_then_origin() {
        let a = BaselineEpoch {
            counter: 1,
            origin_node: "node_z".to_string(),
        };
        let b = BaselineEpoch {
            counter: 2,
            origin_node: "node_a".to_string(),
        };
        assert!(b > a);

        let same_counter_a = BaselineEpoch {
            counter: 5,
            origin_node: "node_a".to_string(),
        };
        let same_counter_b = BaselineEpoch {
            counter: 5,
            origin_node: "node_b".to_string(),
        };
        assert!(same_counter_b > same_counter_a);
    }

    #[test]
    fn baseline_epoch_roundtrip() {
        let epoch = BaselineEpoch {
            counter: 42,
            origin_node: "donor-1".to_string(),
        };
        let bytes = crate::cbor::encode(&epoch).expect("encode");
        let decoded = crate::cbor::decode::<BaselineEpoch>(&bytes).expect("decode");
        assert_eq!(decoded, epoch);
    }

    #[test]
    fn baseline_elect_and_ack_roundtrip() {
        let elect = BaselineElect {
            node_id: "joiner-1".to_string(),
            proposed_donor: "donor-1".to_string(),
            epoch_seen: 7,
        };
        let bytes = crate::cbor::encode(&elect).expect("encode");
        let decoded = crate::cbor::decode::<BaselineElect>(&bytes).expect("decode");
        assert_eq!(decoded.node_id, "joiner-1");
        assert_eq!(decoded.proposed_donor, "donor-1");
        assert_eq!(decoded.epoch_seen, 7);

        let ack = BaselineAck {
            accepted: true,
            donor: "donor-1".to_string(),
            joiner: "joiner-1".to_string(),
            epoch: 7,
        };
        let bytes = crate::cbor::encode(&ack).expect("encode");
        let decoded = crate::cbor::decode::<BaselineAck>(&bytes).expect("decode");
        assert!(decoded.accepted);
        assert_eq!(decoded.epoch, 7);
    }

    #[test]
    fn baseline_header_and_chunk_roundtrip() {
        let header = BaselineHeader {
            schema_version: 52,
            epoch: 7,
            tables: vec!["flows".to_string(), "roles".to_string()],
            row_counts: vec![3, 5],
            total_bytes: 4096,
            max_bytes: 1_048_576,
        };
        let bytes = crate::cbor::encode(&header).expect("encode");
        let decoded = crate::cbor::decode::<BaselineHeader>(&bytes).expect("decode");
        assert_eq!(decoded.tables, vec!["flows", "roles"]);
        assert_eq!(decoded.row_counts, vec![3, 5]);
        assert_eq!(decoded.schema_version, 52);

        let chunk = BaselineChunk {
            seq: 2,
            content_hash: [9u8; 32],
            bytes: vec![1, 2, 3, 4],
        };
        let bytes = crate::cbor::encode(&chunk).expect("encode");
        let decoded = crate::cbor::decode::<BaselineChunk>(&bytes).expect("decode");
        assert_eq!(decoded.seq, 2);
        assert_eq!(decoded.content_hash, [9u8; 32]);
        assert_eq!(decoded.bytes, vec![1, 2, 3, 4]);

        let ack = BaselineChunkAck { seq: 2, ok: true };
        let bytes = crate::cbor::encode(&ack).expect("encode");
        let decoded = crate::cbor::decode::<BaselineChunkAck>(&bytes).expect("decode");
        assert_eq!(decoded.seq, 2);
        assert!(decoded.ok);
    }

    // =========================================================================
    // Testy typow meeting bot
    // =========================================================================

    /// Pomocnicza makra do roundtrip testow CBOR dla typow meeting bot.
    /// Serializuje do bajtow i deserializuje z decoded — zwraca &Decoded.
    macro_rules! cbor_serialize {
        ($value:expr) => {
            crate::cbor::encode($value).expect("Serializacja CBOR powinna sie udac")
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

        let bytes = cbor_serialize!(&transcript);
        let decoded = crate::cbor::decode::<MeetingTranscript>(&bytes).expect("decode");

        assert_eq!(decoded.speaker.as_str(), "Jan Kowalski");
        assert_eq!(decoded.text.as_str(), "Dzien dobry, zaczynamy spotkanie.");
        assert_eq!(decoded.timestamp_ms, 1_710_000_000_000);
    }

    #[test]
    fn test_meeting_transcript_empty_fields() {
        // Transkrypcja z pustymi polami
        let transcript = MeetingTranscript {
            speaker: "".to_string(),
            text: "".to_string(),
            timestamp_ms: 0,
        };

        let bytes = cbor_serialize!(&transcript);
        let decoded = crate::cbor::decode::<MeetingTranscript>(&bytes).expect("decode");

        assert_eq!(decoded.speaker.as_str(), "");
        assert_eq!(decoded.text.as_str(), "");
        assert_eq!(decoded.timestamp_ms, 0);
    }

    #[test]
    fn test_meeting_speak_command_roundtrip() {
        // Serializacja komendy mowienia TTS
        let cmd = MeetingSpeakCommand {
            text: "Prosze o ciszę.".to_string(),
            voice: "alloy".to_string(),
            model: "tts-1".to_string(),
        };

        let bytes = cbor_serialize!(&cmd);
        let decoded = crate::cbor::decode::<MeetingSpeakCommand>(&bytes).expect("decode");

        assert_eq!(decoded.text.as_str(), "Prosze o ciszę.");
        assert_eq!(decoded.voice.as_str(), "alloy");
        assert_eq!(decoded.model.as_str(), "tts-1");
    }

    #[test]
    fn test_meeting_control_join_roundtrip() {
        let ctrl = MeetingControl::Join {
            meeting_url: "https://teams.microsoft.com/l/meetup-join/abc".to_string(),
        };

        let bytes = cbor_serialize!(&ctrl);
        let decoded = crate::cbor::decode::<MeetingControl>(&bytes).expect("decode");

        match decoded {
            MeetingControl::Join { meeting_url } => {
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

        let bytes = cbor_serialize!(&ctrl);
        let decoded = crate::cbor::decode::<MeetingControl>(&bytes).expect("decode");

        assert!(matches!(decoded, MeetingControl::Leave));
    }

    #[test]
    fn test_meeting_control_mute_roundtrip() {
        // Mute i unmute
        for muted_val in [true, false] {
            let ctrl = MeetingControl::Mute { muted: muted_val };

            let bytes = cbor_serialize!(&ctrl);
            let decoded = crate::cbor::decode::<MeetingControl>(&bytes).expect("decode");

            match decoded {
                MeetingControl::Mute { muted } => assert_eq!(muted, muted_val),
                _ => panic!("Oczekiwano wariantu Mute"),
            }
        }
    }

    #[test]
    fn test_meeting_control_state_changed_joining() {
        let ctrl = MeetingControl::StateChanged {
            state: MeetingState::Joining,
        };

        let bytes = cbor_serialize!(&ctrl);
        let decoded = crate::cbor::decode::<MeetingControl>(&bytes).expect("decode");

        match decoded {
            MeetingControl::StateChanged { state } => {
                assert!(matches!(state, MeetingState::Joining));
            }
            _ => panic!("Oczekiwano wariantu StateChanged"),
        }
    }

    #[test]
    fn test_meeting_control_state_changed_connected() {
        let ctrl = MeetingControl::StateChanged {
            state: MeetingState::Connected,
        };

        let bytes = cbor_serialize!(&ctrl);
        let decoded = crate::cbor::decode::<MeetingControl>(&bytes).expect("decode");

        match decoded {
            MeetingControl::StateChanged { state } => {
                assert!(matches!(state, MeetingState::Connected));
            }
            _ => panic!("Oczekiwano wariantu StateChanged"),
        }
    }

    #[test]
    fn test_meeting_control_state_changed_reconnecting() {
        let ctrl = MeetingControl::StateChanged {
            state: MeetingState::Reconnecting,
        };

        let bytes = cbor_serialize!(&ctrl);
        let decoded = crate::cbor::decode::<MeetingControl>(&bytes).expect("decode");

        match decoded {
            MeetingControl::StateChanged { state } => {
                assert!(matches!(state, MeetingState::Reconnecting));
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

        let bytes = cbor_serialize!(&ctrl);
        let decoded = crate::cbor::decode::<MeetingControl>(&bytes).expect("decode");

        match decoded {
            MeetingControl::StateChanged { state } => match state {
                MeetingState::Ended { reason } => {
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

        let bytes = cbor_serialize!(&ctrl);
        let decoded = crate::cbor::decode::<MeetingControl>(&bytes).expect("decode");

        match decoded {
            MeetingControl::StateChanged { state } => {
                assert!(matches!(state, MeetingState::AuthExpired));
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

        let bytes = cbor_serialize!(&ctrl);
        let decoded = crate::cbor::decode::<MeetingControl>(&bytes).expect("decode");

        match decoded {
            MeetingControl::StateChanged { state } => match state {
                MeetingState::Kicked { reason } => {
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

        let bytes = cbor_serialize!(&ctrl);
        let decoded = crate::cbor::decode::<MeetingControl>(&bytes).expect("decode");

        match decoded {
            MeetingControl::SidecarHealth { healthy, uptime_s } => {
                assert!(healthy);
                assert_eq!(uptime_s, 3600);
            }
            _ => panic!("Oczekiwano wariantu SidecarHealth"),
        }
    }

    #[test]
    fn test_meeting_state_ended_with_reason() {
        let state = MeetingState::Ended {
            reason: "Meeting ended by host".to_string(),
        };

        let bytes = cbor_serialize!(&state);
        let decoded = crate::cbor::decode::<MeetingState>(&bytes).expect("decode");

        match decoded {
            MeetingState::Ended { reason } => {
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

        let bytes = cbor_serialize!(&state);
        let decoded = crate::cbor::decode::<MeetingState>(&bytes).expect("decode");

        match decoded {
            MeetingState::Kicked { reason } => {
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
            .serialize_cbor()
            .expect("Serializacja heartbeat powinna sie udac");
        let decoded = MeshMessage::deserialize_cbor(&bytes)
            .expect("Deserializacja heartbeat powinna sie udac");

        match decoded {
            MeshMessage::Heartbeat(hb) => {
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
            version_vector: vec![(0xCAFE, 200), (0xBEEF, 150)],
            platform: "linux".to_string(),
            cpu_count: 16,
            docker_available: true,
            docker_version: "24.0.7".to_string(),
            cluster_id: Some("gpu-farm".to_string()),
        });

        let bytes = msg
            .serialize_cbor()
            .expect("Serializacja full state powinna sie udac");
        let decoded = MeshMessage::deserialize_cbor(&bytes)
            .expect("Deserializacja full state powinna sie udac");

        match decoded {
            MeshMessage::FullStateExchange(state) => {
                assert_eq!(state.node_id.as_str(), "node-20");
                assert_eq!(state.role.as_str(), "worker");
                assert_eq!(state.capabilities.len(), 2);
                assert_eq!(state.models.len(), 1);
                assert_eq!(state.models[0].name.as_str(), "llama3-70b");
                assert_eq!(state.models[0].max_context, 8192);
                assert_eq!(state.containers.len(), 1);
                assert_eq!(state.containers[0].name.as_str(), "vllm-server");
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
            let bytes = crate::cbor::encode(&p).expect("encode");
            crate::cbor::decode::<MeshCommandResponsePayload>(&bytes)
                .expect("decode");
        }
    }

    #[test]
    fn sync_ledger_payloads_roundtrip_cbor() {
        let op = MeshSyncOperationWire {
            op_id: vec![7; 32],
            partition_id: "addon/contacts/persons/1".to_string(),
            partition_sequence: 4,
            operation: vec![1, 2, 3, 4],
        };
        let push = MeshSyncPushPayload {
            from_node_id: "node-a".to_string(),
            operations: vec![op.clone()],
        };
        let bytes = crate::cbor::encode(&push).expect("encode push");
        let decoded = crate::cbor::decode::<MeshSyncPushPayload>(&bytes)
            .expect("decode push");
        assert_eq!(decoded.operations[0].op_id, op.op_id);
        assert_eq!(decoded.operations[0].partition_sequence, 4);

        let ack = MeshSyncAckPayload {
            from_node_id: "node-b".to_string(),
            operation_ids: vec![vec![7; 32]],
        };
        let bytes = crate::cbor::encode(&ack).expect("encode ack");
        let decoded = crate::cbor::decode::<MeshSyncAckPayload>(&bytes)
            .expect("decode ack");
        assert_eq!(decoded.operation_ids.len(), 1);

        let pull = MeshSyncPullPayload {
            from_node_id: "node-a".to_string(),
            partition_id: "addon/contacts/persons/1".to_string(),
            from_sequence: 2,
            limit: 128,
        };
        let bytes = crate::cbor::encode(&pull).expect("encode pull");
        let decoded = crate::cbor::decode::<MeshSyncPullPayload>(&bytes)
            .expect("decode pull");
        assert_eq!(decoded.from_sequence, 2);

        let response = MeshSyncPullResponsePayload {
            from_node_id: "node-b".to_string(),
            partition_id: "addon/contacts/persons/1".to_string(),
            from_sequence: 2,
            operations: vec![op],
        };
        let bytes = crate::cbor::encode(&response).expect("encode response");
        let decoded = crate::cbor::decode::<MeshSyncPullResponsePayload>(&bytes)
            .expect("decode response");
        assert_eq!(decoded.operations.len(), 1);

        let snapshot_pull = MeshSyncSnapshotPullPayload {
            from_node_id: "node-a".to_string(),
            partition_id: "addon/contacts/persons/1".to_string(),
            up_to_sequence: 10,
            snapshot_id: "snapshot-a".to_string(),
            include_tail: true,
            tail_limit: 64,
        };
        let bytes =
            crate::cbor::encode(&snapshot_pull).expect("encode snapshot pull");
        let decoded = crate::cbor::decode::<MeshSyncSnapshotPullPayload>(&bytes)
            .expect("decode snapshot pull");
        assert_eq!(decoded.snapshot_id, "snapshot-a");
        assert!(decoded.include_tail);

        let snapshot_response = MeshSyncSnapshotResponsePayload {
            from_node_id: "node-b".to_string(),
            partition_id: "addon/contacts/persons/1".to_string(),
            up_to_sequence: 10,
            snapshot_id: "snapshot-a".to_string(),
            snapshot_bytes: vec![1, 2, 3],
            blob_bytes: vec![4, 5, 6],
            operations_after_snapshot: vec![MeshSyncOperationWire {
                op_id: vec![9; 32],
                partition_id: "addon/contacts/persons/1".to_string(),
                partition_sequence: 11,
                operation: vec![7, 8],
            }],
        };
        let bytes = crate::cbor::encode(&snapshot_response).expect("encode snapshot response");
        let decoded =
            crate::cbor::decode::<MeshSyncSnapshotResponsePayload>(&bytes)
                .expect("decode snapshot response");
        assert_eq!(decoded.blob_bytes, vec![4, 5, 6]);
        assert_eq!(decoded.operations_after_snapshot.len(), 1);
    }

    // =========================================================================
    // F1b P3.C-1 — frame proxy wire payload roundtrips
    // =========================================================================

    fn sample_metadata() -> FrameMetadataWire {
        FrameMetadataWire {
            camera_id: "cam-front-door".into(),
            width: 1920,
            height: 1080,
            pixel_format: "rgb24".into(),
            timestamp_unix_ms: 1_715_000_000_123,
        }
    }

    #[test]
    fn test_frame_metadata_wire_roundtrip() {
        let meta = sample_metadata();
        let bytes = crate::cbor::encode(&meta).expect("encode metadata");
        let decoded =
            crate::cbor::decode::<FrameMetadataWire>(&bytes).expect("decode");
        assert_eq!(decoded.camera_id, "cam-front-door");
        assert_eq!(decoded.width, 1920);
        assert_eq!(decoded.height, 1080);
        assert_eq!(decoded.pixel_format, "rgb24");
        assert_eq!(decoded.timestamp_unix_ms, 1_715_000_000_123);
    }

    #[test]
    fn test_frame_proxy_request_payload_roundtrip() {
        let req = FrameProxyRequestPayload {
            raw_ref: "frame-store/cam-front-door/2026-05-15T10:00:00.123".into(),
            request_id: "req-abc-001".into(),
        };
        let bytes = crate::cbor::encode(&req).expect("encode");
        let decoded = crate::cbor::decode::<FrameProxyRequestPayload>(&bytes)
            .expect("decode");
        assert_eq!(
            decoded.raw_ref,
            "frame-store/cam-front-door/2026-05-15T10:00:00.123"
        );
        assert_eq!(decoded.request_id, "req-abc-001");
    }

    #[test]
    fn test_frame_proxy_response_found_roundtrip() {
        let resp = FrameProxyResponsePayload::Found {
            raw_ref: "frame-store/cam-1/abc".into(),
            request_id: "req-1".into(),
            bytes: vec![0x10, 0x20, 0x30, 0x40, 0x50],
            metadata: sample_metadata(),
        };
        let bytes = crate::cbor::encode(&resp).expect("encode");
        let decoded = crate::cbor::decode::<FrameProxyResponsePayload>(&bytes)
            .expect("decode");
        match decoded {
            FrameProxyResponsePayload::Found {
                raw_ref,
                request_id,
                bytes,
                metadata,
            } => {
                assert_eq!(raw_ref, "frame-store/cam-1/abc");
                assert_eq!(request_id, "req-1");
                assert_eq!(bytes, vec![0x10, 0x20, 0x30, 0x40, 0x50]);
                assert_eq!(metadata.camera_id, "cam-front-door");
                assert_eq!(metadata.width, 1920);
            }
            other => panic!("expected Found, got {:?}", other),
        }
    }

    #[test]
    fn test_frame_proxy_response_not_found_roundtrip() {
        let resp = FrameProxyResponsePayload::NotFound {
            raw_ref: "frame-store/cam-1/missing".into(),
            request_id: "req-2".into(),
        };
        let bytes = crate::cbor::encode(&resp).expect("encode");
        let decoded = crate::cbor::decode::<FrameProxyResponsePayload>(&bytes)
            .expect("decode");
        match decoded {
            FrameProxyResponsePayload::NotFound {
                raw_ref,
                request_id,
            } => {
                assert_eq!(raw_ref, "frame-store/cam-1/missing");
                assert_eq!(request_id, "req-2");
            }
            other => panic!("expected NotFound, got {:?}", other),
        }
    }

    #[test]
    fn test_frame_proxy_response_unavailable_roundtrip() {
        let resp = FrameProxyResponsePayload::Unavailable {
            raw_ref: "frame-store/cam-1/abc".into(),
            request_id: "req-3".into(),
            reason: "source connector disconnected mid-fetch".into(),
        };
        let bytes = crate::cbor::encode(&resp).expect("encode");
        let decoded = crate::cbor::decode::<FrameProxyResponsePayload>(&bytes)
            .expect("decode");
        match decoded {
            FrameProxyResponsePayload::Unavailable {
                raw_ref,
                request_id,
                reason,
            } => {
                assert_eq!(raw_ref, "frame-store/cam-1/abc");
                assert_eq!(request_id, "req-3");
                assert_eq!(reason, "source connector disconnected mid-fetch");
            }
            other => panic!("expected Unavailable, got {:?}", other),
        }
    }
}
