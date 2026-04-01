// =============================================================================
// Plik: mesh.rs
// Opis: Typy wiadomosci mesh dla komunikacji gossip, membership, CRDT sync
//       i service discovery miedzy nodami TentaFlow.AI przez QUIC.
// =============================================================================

use rkyv::{Archive, Deserialize, Serialize};
use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

fn default_service_status() -> String {
    "running".to_string()
}

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
// Informacja o serwisie AI
// =============================================================================

/// Opis serwisu AI dostepnego na nodzie mesh.
/// Uzywany w service discovery i load balancingu.
#[derive(Archive, Deserialize, Serialize, Debug, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshServiceInfo {
    /// Identyfikator serwisu (UUID)
    #[serde(default)]
    pub service_id: String,
    /// Nazwa serwisu
    pub service_name: String,
    /// Typ serwisu: "llm", "tts", "embedding" itp.
    pub service_type: String,
    /// Identyfikator noda na ktorym dziala serwis
    pub node_id: String,
    /// Port QUIC na ktorym serwis nasluchuje
    pub quic_port: u16,
    /// Adres QUIC serwisu (z perspektywy owner node)
    #[serde(default)]
    pub quic_url: String,
    /// Status serwisu: "running", "stopped", "error"
    #[serde(default = "default_service_status")]
    pub status: String,
    /// Lista modeli dostepnych w serwisie
    pub models: Vec<String>,
    /// Obciazenie noda w procentach (0-100)
    pub load_percent: u8,
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
    Ping {
        from: String,
        incarnation: u64,
    },

    /// Odpowiedz na ping
    PingAck {
        from: String,
        incarnation: u64,
    },

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
    Leave {
        node_id: String,
    },

    // -- CRDT sync --

    /// Synchronizacja stanu CRDT - lista operacji do zaaplikowania
    StateSync {
        from: String,
        operations: Vec<CrdtSyncOp>,
    },

    /// Zadanie synchronizacji stanu od podanego czasu
    StateSyncRequest {
        from: String,
        since_time: u64,
    },

    // -- Service discovery --

    /// Ogloszenie dostepnych serwisow na nodzie
    ServiceAnnounce {
        node_id: String,
        services: Vec<MeshServiceInfo>,
    },

    /// Zapytanie o serwisy danego typu
    ServiceQuery {
        service_type: String,
        from: String,
    },

    /// Odpowiedz z lista serwisow
    ServiceResponse {
        services: Vec<MeshServiceInfo>,
        from: String,
    },

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
    PairingRequest {
        from_node_id: String,
        pin: String,
    },

    /// Potwierdzenie parowania — wymiana kluczy publicznych
    PairingConfirm {
        from_node_id: String,
        public_key: Vec<u8>,
    },

    /// Odrzucenie parowania
    PairingReject {
        from_node_id: String,
    },

    /// Cofniecie zaufania — node nie jest juz zaufany
    TrustRevoked {
        node_id: String,
    },

    /// Synchronizacja kluczy zaufanych nodow po zatwierdzeniu parowania
    TrustedKeysSync {
        keys: Vec<(String, Vec<u8>)>,
    },

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
    NodeLeaving {
        node_id: String,
    },

    // -- Komendy zarzadzania --

    /// Komenda zarzadzania wyslana do sparowanego noda
    MeshCommand {
        command_id: String,
        from_node_id: String,
        command: MeshCommandType,
    },

    /// Odpowiedz na komende zarzadzania
    MeshCommandResponse {
        command_id: String,
        from_node_id: String,
        success: bool,
        output: String,
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

    // -- Service discovery rozszerzony --

    /// Zapytanie o wszystkie widoczne serwisy w mesh
    ServiceQueryAll {
        from_node_id: String,
        request_id: String,
    },

    /// Odpowiedz z pelna lista serwisow (z dedup)
    ServiceResponseAll {
        from_node_id: String,
        request_id: String,
        services: Vec<MeshServiceInfo>,
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
    /// Dostepne serwisy AI
    pub services: Vec<MeshServiceInfo>,
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
    /// Pobranie obrazu Docker
    PullImage { image: String, tag: String },
    /// Deploy docker-compose stack
    DeployStack {
        stack_name: String,
        compose_yaml: String,
        registry_auth: Option<MeshRegistryAuth>,
    },
    /// Usuniecie stacka
    RemoveStack { stack_name: String },
    /// Uruchomienie kontenera
    ContainerStart { container_id: String },
    /// Zatrzymanie kontenera
    ContainerStop { container_id: String },
    /// Restart kontenera
    ContainerRestart { container_id: String },
    /// Usuniecie kontenera
    ContainerRemove { container_id: String, force: bool },
    /// Pobranie logow kontenera
    ContainerLogs {
        container_id: String,
        tail_lines: u32,
        follow: bool,
    },
    /// Lista kontenerow
    ListContainers,
    /// Lista obrazow Docker
    ListImages,
    /// Czyszczenie Docker (prune)
    SystemPrune { volumes: bool },
    /// Wgranie certyfikatow TLS
    ProvisionCerts {
        cert_pem: String,
        key_pem: String,
        target_dir: String,
    },
    /// Dodanie serwisu na nodzie
    AddService { service_config: String },
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
        bind_interface: String,
        duration_ms: u32,
        mode: String,
        nonce: Vec<u8>,
        num_streams: u8,
    },
    /// Anulowanie probing sesji
    BandwidthProbeCancel,
}

impl std::fmt::Debug for MeshCommandType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PullImage { image, tag } => {
                f.debug_struct("PullImage")
                    .field("image", image)
                    .field("tag", tag)
                    .finish()
            }
            Self::DeployStack { stack_name, compose_yaml, registry_auth } => {
                let masked_auth = registry_auth.as_ref().map(|auth| {
                    format!(
                        "MeshRegistryAuth {{ server: {:?}, username: {:?}, password: \"***\" }}",
                        auth.server, auth.username
                    )
                });
                f.debug_struct("DeployStack")
                    .field("stack_name", stack_name)
                    .field("compose_yaml", compose_yaml)
                    .field("registry_auth", &masked_auth)
                    .finish()
            }
            Self::RemoveStack { stack_name } => {
                f.debug_struct("RemoveStack")
                    .field("stack_name", stack_name)
                    .finish()
            }
            Self::ContainerStart { container_id } => {
                f.debug_struct("ContainerStart")
                    .field("container_id", container_id)
                    .finish()
            }
            Self::ContainerStop { container_id } => {
                f.debug_struct("ContainerStop")
                    .field("container_id", container_id)
                    .finish()
            }
            Self::ContainerRestart { container_id } => {
                f.debug_struct("ContainerRestart")
                    .field("container_id", container_id)
                    .finish()
            }
            Self::ContainerRemove { container_id, force } => {
                f.debug_struct("ContainerRemove")
                    .field("container_id", container_id)
                    .field("force", force)
                    .finish()
            }
            Self::ContainerLogs { container_id, tail_lines, follow } => {
                f.debug_struct("ContainerLogs")
                    .field("container_id", container_id)
                    .field("tail_lines", tail_lines)
                    .field("follow", follow)
                    .finish()
            }
            Self::ListContainers => write!(f, "ListContainers"),
            Self::ListImages => write!(f, "ListImages"),
            Self::SystemPrune { volumes } => {
                f.debug_struct("SystemPrune")
                    .field("volumes", volumes)
                    .finish()
            }
            Self::ProvisionCerts { cert_pem: _, key_pem: _, target_dir } => {
                f.debug_struct("ProvisionCerts")
                    .field("cert_pem", &"[CERT]")
                    .field("key_pem", &"***")
                    .field("target_dir", target_dir)
                    .finish()
            }
            Self::AddService { service_config } => {
                f.debug_struct("AddService")
                    .field("service_config", service_config)
                    .finish()
            }
            Self::NetworkConfig { interface, ipv4, netmask, gateway, dhcp, sudo_password: _ } => {
                f.debug_struct("NetworkConfig")
                    .field("interface", interface)
                    .field("ipv4", ipv4)
                    .field("netmask", netmask)
                    .field("gateway", gateway)
                    .field("dhcp", dhcp)
                    .field("sudo_password", &"***")
                    .finish()
            }
            Self::BandwidthProbe { target_ip, mode, .. } => {
                f.debug_struct("BandwidthProbe")
                    .field("target_ip", target_ip)
                    .field("mode", mode)
                    .finish()
            }
            Self::BandwidthProbeCancel => write!(f, "BandwidthProbeCancel"),
        }
    }
}

// =============================================================================
// Dane uwierzytelniania rejestru Docker
// =============================================================================

/// Dane logowania do prywatnego rejestru Docker.
#[derive(Archive, Deserialize, Serialize, Clone, SerdeSerialize, SerdeDeserialize)]
pub struct MeshRegistryAuth {
    /// Adres serwera rejestru
    pub server: String,
    /// Nazwa uzytkownika
    pub username: String,
    /// Haslo
    pub password: String,
}

impl std::fmt::Debug for MeshRegistryAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeshRegistryAuth")
            .field("server", &self.server)
            .field("username", &self.username)
            .field("password", &"***")
            .finish()
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
pub const MESH_MSG_SERVICE_ANNOUNCE: u8 = 0x17;
pub const MESH_MSG_NODE_INFO: u8 = 0x18;
pub const MESH_MSG_PAIRING_REQUEST: u8 = 0x20;
pub const MESH_MSG_PAIRING_CONFIRM: u8 = 0x21;
pub const MESH_MSG_PAIRING_REJECT: u8 = 0x22;
pub const MESH_MSG_TRUST_REVOKED: u8 = 0x23;
pub const MESH_MSG_TRUSTED_KEYS_SYNC: u8 = 0x24;
pub const MESH_MSG_COMMAND: u8 = 0x30;
pub const MESH_MSG_COMMAND_RESPONSE: u8 = 0x31;
pub const MESH_MSG_DEPLOY_PROGRESS: u8 = 0x32;
pub const MESH_MSG_LOG_CHUNK: u8 = 0x33;
pub const MESH_MSG_SERVICE_QUERY_ALL: u8 = 0x34;
pub const MESH_MSG_SERVICE_RESPONSE_ALL: u8 = 0x35;
pub const MESH_MSG_CLUSTER_INFO: u8 = 0x36;
pub const MESH_MSG_KEY_ROTATION: u8 = 0x25;
pub const MESH_MSG_KEY_ROTATION_RESPONSE: u8 = 0x26;
pub const MESH_MSG_NODE_LEAVING: u8 = 0x27;
pub const MESH_MSG_RELAY_FRAME: u8 = 0x37;

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

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize, Archive, Deserialize, Serialize)]
#[rkyv(derive(Debug))]
pub struct TrustedKeysSyncPayload {
    pub keys: Vec<TrustedKeyEntry>,
}

#[derive(Debug, Clone, SerdeSerialize, SerdeDeserialize, Archive, Deserialize, Serialize)]
#[rkyv(derive(Debug))]
pub struct NodeLeavingPayload {
    pub node_id: String,
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

        let bytes = msg.serialize_rkyv().expect("Serializacja ping powinna sie udac");
        let archived = MeshMessage::deserialize_rkyv(&bytes)
            .expect("Deserializacja ping powinna sie udac");

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

        let bytes = msg.serialize_rkyv().expect("Serializacja join powinna sie udac");
        let archived = MeshMessage::deserialize_rkyv(&bytes)
            .expect("Deserializacja join powinna sie udac");

        match archived {
            ArchivedMeshMessage::Join {
                node_id, addr, role, capabilities,
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

        let bytes = msg.serialize_rkyv().expect("Serializacja state sync powinna sie udac");
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
    fn test_service_info_roundtrip() {
        let msg = MeshMessage::ServiceAnnounce {
            node_id: "node-4".to_string(),
            services: vec![MeshServiceInfo {
                service_id: String::new(),
                service_name: "llm-server".to_string(),
                service_type: "llm".to_string(),
                node_id: "node-4".to_string(),
                quic_port: 4433,
                quic_url: String::new(),
                status: "running".to_string(),
                models: vec!["llama3-8b".to_string(), "mistral-7b".to_string()],
                load_percent: 35,
            }],
        };

        let bytes = msg.serialize_rkyv().expect("Serializacja service announce powinna sie udac");
        let archived = MeshMessage::deserialize_rkyv(&bytes)
            .expect("Deserializacja service announce powinna sie udac");

        match archived {
            ArchivedMeshMessage::ServiceAnnounce { node_id, services } => {
                assert_eq!(node_id.as_str(), "node-4");
                assert_eq!(services.len(), 1);
                assert_eq!(services[0].quic_port, 4433);
                assert_eq!(services[0].load_percent, 35);
                assert_eq!(services[0].models.len(), 2);
            }
            _ => panic!("Oczekiwano wariantu ServiceAnnounce"),
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

        let bytes = msg.serialize_rkyv().expect("Serializacja forward powinna sie udac");
        let archived = MeshMessage::deserialize_rkyv(&bytes)
            .expect("Deserializacja forward powinna sie udac");

        match archived {
            ArchivedMeshMessage::ForwardRequest {
                request_id, target_node, payload: archived_payload,
            } => {
                assert_eq!(request_id.as_str(), "req-001");
                assert_eq!(target_node.as_str(), "node-5");
                assert_eq!(archived_payload.as_slice(), &[1, 2, 3, 4, 5]);
            }
            _ => panic!("Oczekiwano wariantu ForwardRequest"),
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

        let bytes = msg.serialize_rkyv().expect("Serializacja heartbeat powinna sie udac");
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
            services: vec![MeshServiceInfo {
                service_id: String::new(),
                service_name: "llm-vllm".to_string(),
                service_type: "llm".to_string(),
                node_id: "node-20".to_string(),
                quic_port: 4433,
                quic_url: String::new(),
                status: "running".to_string(),
                models: vec!["llama3-70b".to_string()],
                load_percent: 55,
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

        let bytes = msg.serialize_rkyv().expect("Serializacja full state powinna sie udac");
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
                assert_eq!(state.services.len(), 1);
                assert_eq!(state.crdt_operations.len(), 1);
                assert_eq!(state.version_vector.len(), 2);
            }
            _ => panic!("Oczekiwano wariantu FullStateExchange"),
        }
    }
}
