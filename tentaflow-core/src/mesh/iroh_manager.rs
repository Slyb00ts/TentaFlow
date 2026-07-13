// =============================================================================
// Plik: mesh/iroh_manager.rs
// Opis: Menedzer mesh zbudowany na iroh::Endpoint. Odpowiednik QuicMeshManager,
//       rozni sie transportem (iroh QUIC + relay + LAN mDNS + DHT pkarr) i
//       brakiem warstwy AEAD (TLS 1.3 iroh wystarcza). Trzyma mape aktywnych
//       polaczen po EndpointId, emituje zdarzenia do broadcast::Receiver.
//       Uni streamy mesh przenosza podpisane envelope UFP/2.
// =============================================================================

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use dashmap::DashMap;
use iroh::endpoint::Connection;
use iroh::{EndpointAddr, EndpointId, RelayUrl, TransportAddr};
use parking_lot::RwLock;
use tokio::sync::{broadcast, mpsc, RwLock as AsyncRwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::mesh::security::MeshSecurity;
use crate::net::iroh::{
    handler::IrohStreamError,
    pairing::{
        endpoint_addr_from_hints, hints_with_relay_fallback, load_trusted_contact_hints,
        merge_contact_hints, PairingContactHints, PairingHandler,
    },
    IrohConfig, IrohEndpoint, IrohEndpointError, ALPN_API, ALPN_ARTIFACT, ALPN_BASELINE, ALPN_MESH,
    ALPN_PAIRING,
};

/// Typ callbacka do obslugi forward requestow (compat z QuicMeshManager).
pub type ForwardHandler = Arc<
    dyn Fn(Vec<u8>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<u8>> + Send>>
        + Send
        + Sync,
>;

pub type ForwardStreamHandler = Arc<
    dyn Fn(
            Vec<u8>,
            mpsc::UnboundedSender<Vec<u8>>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// Owner-side camera relay handler. Unlike `ForwardStreamHandler` it writes into
/// a BOUNDED channel (`send().await`) so a slow observer back-pressures the
/// StreamHub broadcast drain instead of growing memory without limit. The QUIC
/// writer reads the bounded receiver and applies flow-control on the wire.
pub type CameraStreamHandler = Arc<
    dyn Fn(
            Vec<u8>,
            mpsc::Sender<Vec<u8>>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// Owner-side LiDAR relay handler. Identical shape to `CameraStreamHandler`
/// (payload → frames via a BOUNDED channel so a slow observer back-pressures the
/// StreamHub broadcast drain instead of growing memory without limit) but a
/// different payload (LiDAR subscribe) — kept separate so the two never alias.
pub type LidarStreamHandler = Arc<
    dyn Fn(
            Vec<u8>,
            mpsc::Sender<Vec<u8>>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// Bounded capacity for the owner-side camera relay channel between the
/// StreamHub broadcast drain and the QUIC writer. Matches the StreamHub
/// broadcast capacity so the relay never queues more than one broadcast window
/// before either the wire drains it or the slow observer is cut.
const CAMERA_RELAY_CHANNEL_CAPACITY: usize = 32;

/// Aborts the wrapped task when dropped. Used so an early return on the
/// owner-side camera bi-stream cancels the spawned relay handler immediately
/// instead of leaving it parked in `recv().await` holding a StreamHub handle.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Odpowiedz komendy mesh — typed payload zamiast string output.
#[derive(Debug, Clone)]
pub struct CommandWaitResponse {
    pub command_id: String,
    pub ok: bool,
    pub payload: tentaflow_protocol::mesh::MeshCommandResponsePayload,
    pub error: Option<String>,
}

const MAX_MSG_BYTES: usize = 16 * 1024 * 1024;

/// Konfiguracja startowa mesh menedzera iroh.
pub struct IrohMeshConfig {
    /// Identyfikator lokalny — informacja pomocnicza (iroh uzywa EndpointId z keypair).
    pub node_id: String,
    /// Adres bind dla QUIC.
    pub bind_addr: std::net::SocketAddr,
    /// URL publicznego lub self-hosted relay.
    pub relay_url: Option<RelayUrl>,
    /// Czy wlaczyc wbudowane LAN mDNS (swarm-discovery). Na iOS false —
    /// iOS blokuje raw multicast bez Apple entitlementa; LAN discovery
    /// idzie przez natywny Bonjour (NWBrowser) w warstwie Swift.
    pub enable_lan_discovery: bool,
    /// Czy wlaczyc DHT (pkarr-mainline) discovery. Mobile defaultowo false —
    /// DHT listening + bootstrap dodaje ~0.5-1s do starta i na mobile nie
    /// jest potrzebne (LAN Bonjour + iroh relay wystarcza).
    pub enable_dht_discovery: bool,
    /// Gotowy filtr adresow publikowanych przez iroh (relay + mDNS). Buduje go
    /// pipeline na bazie ustawien GUI (`mesh.bind_mode` + filtry advertise) i
    /// wstrzykuje tutaj — iroh_manager nie dotyka DbPool.
    pub addr_filter: Option<iroh::address_lookup::AddrFilter>,
    /// Wylacz portmapper iroh (UPnP/NAT-PMP/PCP) — przy pinowanym interfejsie.
    pub disable_portmapper: bool,
}

impl Default for IrohMeshConfig {
    fn default() -> Self {
        Self {
            node_id: String::new(),
            bind_addr: std::net::SocketAddr::from(([0, 0, 0, 0], 0)),
            relay_url: None,
            enable_lan_discovery: true,
            enable_dht_discovery: true,
            addr_filter: None,
            disable_portmapper: false,
        }
    }
}

/// Zdarzenia emitowane przez IrohMeshManager.
#[derive(Debug, Clone)]
pub enum IrohMeshEvent {
    PeerConnected {
        node_id: String,
    },
    PeerDisconnected {
        node_id: String,
    },
    HeartbeatReceived {
        node_id: String,
        heartbeat: Vec<u8>,
    },
    NodeInfoReceived {
        node_id: String,
        data: Vec<u8>,
    },
    HelloReceived {
        node_id: String,
        data: Vec<u8>,
    },
    TopologyAnnounceReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
    KnownPeersReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
    PairingRequestReceived {
        peer_id: String,
        data: Vec<u8>,
    },
    PairingConfirmReceived {
        peer_id: String,
        data: Vec<u8>,
    },
    PairingRejectReceived {
        peer_id: String,
        data: Vec<u8>,
    },
    PairingTrusted {
        hints: PairingContactHints,
    },
    AliasSyncReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
    /// Pelny snapshot trwalej konfiguracji routingu (klastry + czlonkowie)
    /// od zaufanego peera. JSON `RoutingSyncPayload`; odbiorca tylko zapisuje
    /// lokalnie — nigdy nie re-broadcastuje (anty-petla).
    RoutingSyncReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
    TrustRevokedReceived {
        node_id: String,
        revoked_node_id: String,
    },
    TrustedKeysSyncReceived {
        node_id: String,
        /// (node_id, public_key_hex, origin_approved_at). `approved_at` keeps the
        /// trust-expiry TTL anchored to the origin's first pairing across mirroring.
        keys: Vec<(String, String, String)>,
    },
    /// F1b P3.B — peer pushed its HMAC issuer keys (pickup_token, frame_url,
    /// recording_url). Payload carries raw 32-byte secrets + optional
    /// previous-window key per scope; receiver must already trust the sender
    /// (the dispatcher enforces this in `pipeline.rs`).
    HmacKeysSyncReceived {
        node_id: String,
        payload: tentaflow_protocol::mesh::HmacKeysSyncPayload,
    },
    /// F1b P3.C-1 — trust-paired peer asked us for a frame whose `frame_url`
    /// they hold. Server-side handling (lookup in local frame store, build
    /// `FrameProxyResponsePayload`) is wired in P3.C-2.
    FrameProxyRequestReceived {
        from_node_id: String,
        payload: tentaflow_protocol::mesh::FrameProxyRequestPayload,
    },
    /// F1b P3.C-1 — trust-paired peer replied to one of our outstanding
    /// proxy requests. Client-side completion (pending-map lookup, oneshot
    /// resolve) is wired in P3.C-2.
    FrameProxyResponseReceived {
        from_node_id: String,
        payload: tentaflow_protocol::mesh::FrameProxyResponsePayload,
    },
    StorageProxyRequestReceived {
        from_node_id: String,
        payload: tentaflow_protocol::mesh::StorageProxyRequestPayload,
    },
    StorageProxyResponseReceived {
        from_node_id: String,
        payload: tentaflow_protocol::mesh::StorageProxyResponsePayload,
    },
    NodeLeavingReceived {
        node_id: String,
    },
    ModelListUpdate {
        node_id: String,
        data: Vec<u8>,
    },
    MeshCommandReceived {
        from_node_id: String,
        command: Vec<u8>,
    },
    MeshCommandResponseReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
    MeshDeployProgressReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
    MeshLogChunkReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
    ForwardRequestReceived {
        from_node_id: String,
        request_id: String,
        payload: Vec<u8>,
    },
    /// Alias dla compat z legacy QuicMeshEvent::ForwardRequest.
    ForwardRequest {
        node_id: String,
        request_id: String,
        payload: Vec<u8>,
    },
    /// Odkryty nowy peer przez mDNS/DHT — wypala zanim zaczniemy dial.
    /// Pipeline pisze do peer_store z source=discovered zeby UI widzial peera
    /// nawet gdy dial nie zdazyl wypalic.
    PeerDiscovered {
        node_id: String,
        addresses: Vec<std::net::SocketAddr>,
        /// Nazwa rozgłaszana przez peera w mDNS `user_data` (hostname). Pusta,
        /// gdy peer nie podał — UI spada wtedy na skrócony node_id.
        hostname: String,
    },
    /// Pull request: peer prosi nas o pelny snapshot lokalnych serwisow
    /// (`MESH_MSG_SERVICES_GET`).
    ServicesGetReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
    /// Odpowiedz peera na nasz pull (`MESH_MSG_SERVICES_GET_RESPONSE`).
    ServicesGetResponseReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
    /// Periodyczny anti-drift broadcast peera (`MESH_MSG_SERVICES_ANNOUNCE`).
    ServicesAnnounceReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
    /// Push delta peera (`MESH_MSG_SERVICES_UPDATE`).
    ServicesUpdateReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
    /// Periodyczny anti-drift broadcast robotow peera (`MESH_MSG_ROBOTS_ANNOUNCE`).
    RobotsAnnounceReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
    /// Pull request: peer prosi nas o pelny snapshot lokalnych robotow
    /// (`MESH_MSG_ROBOTS_GET`).
    RobotsGetReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
    /// Odpowiedz peera na nasz pull robotow (`MESH_MSG_ROBOTS_GET_RESPONSE`).
    RobotsGetResponseReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
    /// Push delta robotow peera (`MESH_MSG_ROBOTS_UPDATE`).
    RobotsUpdateReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
    SyncPushReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
    SyncAckReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
    SyncPullReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
    SyncPullResponseReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
    SyncSnapshotPullReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
    SyncSnapshotResponseReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
}

/// Kierunek polaczenia QUIC z perspektywy lokalnego noda. Uzywany przez
/// deterministyczny tie-break, gdy A i B dialuja sie jednoczesnie i iroh
/// tworzy dwa oddzielne fizyczne connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConnectionDirection {
    /// My wywolalismy `endpoint.connect()` do peera.
    Outgoing,
    /// Peer zrobil `accept` na naszym endpoincie.
    Incoming,
}

/// Aktywne polaczenie zalogowane przez manager.
struct ActiveConnection {
    id: u64,
    connection: Connection,
    direction: ConnectionDirection,
}

#[derive(Debug, Clone)]
pub struct ConnectionPathSnapshot {
    pub transport: String,
    pub address: String,
    pub selected: bool,
}

#[derive(Debug, Clone)]
pub struct ConnectionSnapshot {
    pub transport: String,
    pub scope: Option<String>,
    pub address: Option<String>,
    pub relay_url: Option<String>,
    pub paths: Vec<ConnectionPathSnapshot>,
}

/// Glowny menedzer mesh uzywajacy iroh.
///
/// SCALABILITY: glowne mapy (connections, dial_locks, peer_log_state) uzywaja
/// `DashMap` zamiast `RwLock<HashMap>` zeby read/write byl lock-free per-shard.
/// Przy 1000+ peerach rozne operacje (dial, heartbeat broadcast, is_connected)
/// nie konkuruja o ten sam lock. Event bus ma rozszerzony buffer (16K)
/// — inaczej przy burst discovery subskrybenci dostaja Lagged i gubia eventy.
pub struct IrohMeshManager {
    endpoint: Arc<IrohEndpoint>,
    security: Arc<MeshSecurity>,
    config: IrohMeshConfig,
    connections: Arc<DashMap<String, ActiveConnection>>,
    event_tx: broadcast::Sender<IrohMeshEvent>,
    shutdown: CancellationToken,
    local_node_id: RwLock<String>,
    next_connection_id: AtomicU64,
    forward_handler: Arc<AsyncRwLock<Option<ForwardHandler>>>,
    forward_stream_handler: Arc<AsyncRwLock<Option<ForwardStreamHandler>>>,
    /// Owner-side handler for live camera relay bi-streams. Same shape as
    /// `forward_stream_handler` (payload → frames via `tx`) but a different
    /// payload (camera subscribe) — kept separate so the two never alias.
    camera_stream_handler: Arc<AsyncRwLock<Option<CameraStreamHandler>>>,
    /// Owner-side handler for live LiDAR relay bi-streams. Same shape as
    /// `camera_stream_handler` (payload → frames via `tx`) but a different
    /// payload (LiDAR subscribe) — kept separate so the two never alias.
    lidar_stream_handler: Arc<AsyncRwLock<Option<LidarStreamHandler>>>,
    command_waiters: DashMap<String, tokio::sync::oneshot::Sender<CommandWaitResponse>>,
    /// Per-peer mutex zabezpieczajacy przed rownoleglymi `endpoint.connect` do
    /// tego samego peera z roznych tasków (discovery, pairing, manual dial).
    /// DashMap — upsert/read lock-free per-shard.
    dial_locks: DashMap<String, Arc<tokio::sync::Mutex<()>>>,
    /// Stan logowania per-peer: kiedy ostatnio zalogowalismy discovery oraz
    /// ile bylo kolejnych nieudanych dialow. Sluzy do tlumienia spamu.
    peer_log_state: DashMap<String, PeerLogState>,
    /// Executor komend mesh — wstrzykiwany przez pipeline po stworzeniu managera.
    /// `None` w testach ktore nie potrzebuja egzekucji komend; produkcyjnie
    /// pipeline ZAWSZE wpina realny executor zanim zacznie nasluchiwac eventow.
    command_executor: AsyncRwLock<Option<Arc<crate::mesh::command_executor::MeshCommandExecutor>>>,
}

#[derive(Default)]
struct PeerLogState {
    last_discovery_log: Option<Instant>,
    consecutive_dial_failures: u32,
    last_dial_attempt: Option<Instant>,
    /// Od kiedy ten (nie-preferowany, wyzszy node_id) wezel widzi peera bez
    /// polaczenia. Po `NONPREFERRED_DIAL_GRACE` wolno mu dialowac jako fallback,
    /// gdy preferowany (nizszy) nie zdolal nas dosiegnac. Resetowane gdy
    /// polaczenie istnieje.
    nonpreferred_defer_since: Option<Instant>,
}

/// Ile czeka nie-preferowany (wyzszy node_id) wezel zanim zacznie dialowac
/// jako fallback. Normalnie proaktywnie dialuje tylko nizszy node_id; wyzszy
/// czeka na incoming. Grace pokrywa przypadek, gdy nizszy nie moze nas dosiegnac
/// (NAT/asymetryczna lacznosc) — wtedy wyzszy przejmuje inicjatywe.
const NONPREFERRED_DIAL_GRACE: Duration = Duration::from_secs(12);

impl IrohMeshManager {
    /// Tworzy manager bind'ujac iroh Endpoint z discovery (LAN + DHT + relay).
    pub async fn new(config: IrohMeshConfig, security: Arc<MeshSecurity>) -> Result<Arc<Self>> {
        let secret_key = build_secret_key_from_security(&security)?;
        let iroh_config = IrohConfig {
            secret_key,
            bind_addr: config.bind_addr,
            relay_url: config.relay_url.clone(),
            enable_lan_discovery: config.enable_lan_discovery,
            enable_dht_discovery: config.enable_dht_discovery,
            addr_filter: config.addr_filter.clone(),
            disable_portmapper: config.disable_portmapper,
        };

        let endpoint = IrohEndpoint::bind(iroh_config)
            .await
            .map_err(|e: IrohEndpointError| anyhow::anyhow!("iroh endpoint bind: {e:?}"))?;

        // Rozglaszamy nazwe urzadzenia w mDNS user_data (TXT), zeby inne nody
        // widzialy czytelna nazwe peera JESZCZE przed parowaniem zamiast hex
        // node_id. To samo zrodlo co `NodeInfo.hostname`, wiec nazwa w sekcji
        // "Wykryte" zgadza sie z ta po sparowaniu.
        let device_name = crate::mesh::node_info_collector::local_hostname();
        if !device_name.is_empty() {
            if let Ok(user_data) = device_name.parse::<iroh::address_lookup::UserData>() {
                endpoint
                    .inner()
                    .set_user_data_for_address_lookup(Some(user_data));
            }
        }

        let local_id_hex = hex::encode(endpoint.id().as_bytes());
        info!(
            target: "mesh::identity",
            iroh_node_id = %endpoint.id().to_string(),
            iroh_node_id_bytes_hex = %local_id_hex,
            ed25519_hex = %security.ed25519_public_key_hex(),
            "local iroh identity"
        );
        // Duzy buffer — przy discovery burst (nowa siec, wiele peerow na raz)
        // subscriber pipeline moze chwilowo byc wolniejszy niz producent
        // eventow. 1024 bylo za malo, przy 100+ peerach Lagged sie zdarzal.
        let (event_tx, _rx) = broadcast::channel(16_384);

        Ok(Arc::new(Self {
            endpoint: Arc::new(endpoint),
            security,
            config,
            connections: Arc::new(DashMap::with_capacity(256)),
            event_tx,
            shutdown: CancellationToken::new(),
            local_node_id: RwLock::new(local_id_hex),
            next_connection_id: AtomicU64::new(1),
            forward_handler: Arc::new(AsyncRwLock::new(None)),
            forward_stream_handler: Arc::new(AsyncRwLock::new(None)),
            camera_stream_handler: Arc::new(AsyncRwLock::new(None)),
            lidar_stream_handler: Arc::new(AsyncRwLock::new(None)),
            command_waiters: DashMap::new(),
            dial_locks: DashMap::with_capacity(256),
            peer_log_state: DashMap::with_capacity(256),
            command_executor: AsyncRwLock::new(None),
        }))
    }

    /// Wstrzykuje executor komend mesh. Pipeline wola to raz, zaraz po stworzeniu
    /// managera. Bez tego `handle_command_received` odpowie peerowi bledem
    /// "no command executor", co od razu unaocznia brakujace okablowanie zamiast
    /// po cichu ignorowac komendy.
    pub async fn set_command_executor(
        &self,
        executor: Arc<crate::mesh::command_executor::MeshCommandExecutor>,
    ) {
        *self.command_executor.write().await = Some(executor);
    }

    /// Aktualnie wpiety executor — uzywany przez wyzsze warstwy do wstrzykiwania
    /// `ServiceActionContext` (krok N3b) po pelnej inicjalizacji AppState.
    pub async fn command_executor(
        &self,
    ) -> Option<Arc<crate::mesh::command_executor::MeshCommandExecutor>> {
        self.command_executor.read().await.clone()
    }

    /// Discovery spamuje na kazdy mDNS tick — logujemy pierwsze odkrycie peera
    /// i potem co najmniej co `DISCOVERY_LOG_COOLDOWN`. Zwraca true gdy log
    /// ma sie wyemitowac, false — stlumic.
    fn should_log_discovery(&self, peer_hex: &str) -> bool {
        const COOLDOWN: Duration = Duration::from_secs(60);
        let mut entry = self.peer_log_state.entry(peer_hex.to_string()).or_default();
        let now = Instant::now();
        let emit = match entry.last_discovery_log {
            Some(prev) => now.duration_since(prev) >= COOLDOWN,
            None => true,
        };
        if emit {
            entry.last_discovery_log = Some(now);
        }
        emit
    }

    /// Liczy kolejne nieudane dial-y. Zwraca nowa wartosc licznika; 1 = pierwszy
    /// fail w serii, >1 = kolejny z rzedu bez sukcesu.
    fn note_dial_failure(&self, peer_hex: &str) -> u32 {
        let mut entry = self.peer_log_state.entry(peer_hex.to_string()).or_default();
        entry.consecutive_dial_failures = entry.consecutive_dial_failures.saturating_add(1);
        entry.consecutive_dial_failures
    }

    /// Reset licznika po udanym polaczeniu.
    fn note_dial_success(&self, peer_hex: &str) {
        if let Some(mut entry) = self.peer_log_state.get_mut(peer_hex) {
            entry.consecutive_dial_failures = 0;
            entry.nonpreferred_defer_since = None;
        }
    }

    /// Czy powinniśmy PROAKTYWNIE (z discovery/reconnect) dialować tego peera.
    ///
    /// Asymetria zapobiega kolizjom: stale dialuje tylko węzeł z niższym
    /// `node_id`; wyższy czeka na incoming i dialuje dopiero jako fallback po
    /// `NONPREFERRED_DIAL_GRACE` bez połączenia (gdy niższy nie może nas
    /// dosięgnąć). Jeśli połączenie już istnieje — nikt nie dialuje. Jawne diale
    /// (pairing, baseline, sync) NIE używają tej bramki — wołają `connect_*`
    /// bezpośrednio.
    pub fn should_proactively_dial(&self, peer_hex: &str) -> bool {
        if self.connections.contains_key(peer_hex) {
            if let Some(mut entry) = self.peer_log_state.get_mut(peer_hex) {
                entry.nonpreferred_defer_since = None;
            }
            return false;
        }
        let we_are_preferred = self.local_node_id.read().as_str() < peer_hex;
        if we_are_preferred {
            return true;
        }
        let mut entry = self.peer_log_state.entry(peer_hex.to_string()).or_default();
        let now = Instant::now();
        match entry.nonpreferred_defer_since {
            None => {
                entry.nonpreferred_defer_since = Some(now);
                false
            }
            Some(since) => now.duration_since(since) >= NONPREFERRED_DIAL_GRACE,
        }
    }

    /// Cooldown miedzy sekwencyjnymi probami dialu tego samego peera. Bez
    /// tego mDNS wyzwala dial co sekunde — obaj peerowie probuja jednoczesnie,
    /// tie-break pierwsza zamyka, zostaje druga, mDNS znowu wyzwala, loop.
    /// Trusted peery (sparowane) uzywaja krotszego cooldownu zeby szybko
    /// wrocic po realnym padzie, niesparowane dluzszego zeby nie spamowac
    /// dopoki user nie kliknie pairing.
    fn try_consume_dial_attempt(&self, peer_hex: &str, is_trusted: bool) -> bool {
        let cooldown = if is_trusted {
            Duration::from_secs(5)
        } else {
            Duration::from_secs(30)
        };
        let mut entry = self.peer_log_state.entry(peer_hex.to_string()).or_default();
        let now = Instant::now();
        if let Some(prev) = entry.last_dial_attempt {
            if now.duration_since(prev) < cooldown {
                return false;
            }
        }
        entry.last_dial_attempt = Some(now);
        true
    }

    /// Startuje accept loop + heartbeat loop + discovery loop (LAN mDNS).
    /// Zwraca JoinHandles do monitorowania.
    pub fn start(self: &Arc<Self>) -> Vec<JoinHandle<()>> {
        let mut handles = Vec::new();

        let me = Arc::clone(self);
        handles.push(tokio::spawn(async move {
            Self::run_accept_loop(me).await;
        }));

        let me = Arc::clone(self);
        handles.push(tokio::spawn(async move {
            Self::run_discovery_loop(me).await;
        }));

        handles
    }

    /// Konsumuje strumien `DiscoveryEvent` z iroh mDNS. Dla kazdego swiezo
    /// odkrytego peera (nie-self, nie-juz-polaczonego) wola `connect_to_peer`
    /// po EndpointId — iroh sam rozwiazuje adres. To jest brakujacy most
    /// pomiedzy warstwa odkrywania a warstwa mesh: bez niego SWIM gossip ma
    /// puste seed peers.
    async fn run_discovery_loop(self_arc: Arc<Self>) {
        use futures::StreamExt;

        let mut events = match self_arc.endpoint.mdns_discovery_events().await {
            Some(s) => s,
            None => {
                info!("iroh_mesh: LAN discovery wylaczone — discovery loop pominietа");
                return;
            }
        };

        let self_hex = self_arc.local_node_id.read().clone();
        info!(self_id = %self_hex, "iroh_mesh: discovery loop wystartowal");

        loop {
            tokio::select! {
                _ = self_arc.shutdown.cancelled() => {
                    info!("iroh_mesh: discovery loop shutdown");
                    return;
                }
                ev = events.next() => {
                    let Some(ev) = ev else {
                        info!("iroh_mesh: discovery stream zamkniety");
                        return;
                    };
                    use iroh_mdns_address_lookup::DiscoveryEvent;
                    if let DiscoveryEvent::Discovered { endpoint_info, .. } = ev {
                        let peer_id = endpoint_info.endpoint_id;
                        let peer_hex = hex::encode(peer_id.as_bytes());
                        if peer_hex == self_hex {
                            continue;
                        }
                        let addresses: Vec<std::net::SocketAddr> =
                            endpoint_info.data.ip_addrs().copied().collect();
                        // Nazwa urzadzenia rozglaszana w mDNS user_data (TXT) —
                        // pozwala pokazac czytelna nazwe peera JESZCZE przed
                        // parowaniem (bez tego UI ma tylko hex node_id).
                        let advertised_name = endpoint_info
                            .data
                            .user_data()
                            .map(|u| u.to_string())
                            .unwrap_or_default();
                        let _ = self_arc.event_tx.send(IrohMeshEvent::PeerDiscovered {
                            node_id: peer_hex.clone(),
                            addresses: addresses.clone(),
                            hostname: advertised_name,
                        });
                        let is_trusted = self_arc.security.is_trusted(&peer_hex);
                        if !is_trusted {
                            continue;
                        }
                        if self_arc.is_connected(&peer_hex).await {
                            continue;
                        }
                        // Asymetria: tylko nizszy node_id dialuje proaktywnie;
                        // wyzszy czeka na incoming (fallback po grace). Bez tego
                        // oba dialuja naraz → kolizje i spam tie-break.
                        if !self_arc.should_proactively_dial(&peer_hex) {
                            continue;
                        }
                        // Tlumimy rapid re-dial tego samego trusted peera.
                        if !self_arc.try_consume_dial_attempt(&peer_hex, is_trusted) {
                            debug!(peer = %peer_hex, trusted = is_trusted, "iroh_mesh: dial pominiety (cooldown)");
                            continue;
                        }
                        let log_it = self_arc.should_log_discovery(&peer_hex);
                        if log_it {
                            info!(peer = %peer_hex, addrs = ?addresses, trusted = is_trusted, "iroh_mesh: peer odkryty — dial");
                        } else {
                            debug!(peer = %peer_hex, "iroh_mesh: peer re-odkryty (log stlumiony)");
                        }
                        let merged_hints = merge_contact_hints(
                            load_trusted_contact_hints(&self_arc.security.db, &peer_hex)
                                .ok()
                                .flatten(),
                            PairingContactHints {
                                node_id: peer_hex.clone(),
                                public_key_hex: String::new(),
                                hostname: String::new(),
                                addresses: addresses.iter().map(|addr| addr.to_string()).collect(),
                                relay_url: String::new(),
                            },
                        );
                        let me = Arc::clone(&self_arc);
                        tokio::spawn(async move {
                            match me.connect_to_peer_with_hints(&merged_hints).await {
                                Ok(_) => {
                                    me.note_dial_success(&peer_hex);
                                }
                                Err(e) => {
                                    let fails = me.note_dial_failure(&peer_hex);
                                    if fails == 1 {
                                        warn!(peer = %peer_hex, "iroh_mesh: dial nieudany: {}", e);
                                    } else {
                                        debug!(peer = %peer_hex, fails, "iroh_mesh: dial nieudany (powtorka): {}", e);
                                    }
                                }
                            }
                        });
                    }
                }
            }
        }
    }

    async fn run_accept_loop(self_arc: Arc<Self>) {
        let ep = self_arc.endpoint.inner().clone();
        loop {
            tokio::select! {
                _ = self_arc.shutdown.cancelled() => {
                    info!("iroh_mesh: accept loop shutdown");
                    return;
                }
                incoming = ep.accept() => {
                    let Some(incoming) = incoming else {
                        info!("iroh_mesh: endpoint closed — accept loop exiting");
                        return;
                    };
                    let me = Arc::clone(&self_arc);
                    tokio::spawn(async move {
                        if let Err(e) = me.handle_incoming(incoming).await {
                            if is_transient_incoming_finalize_error(&e) {
                                debug!("iroh_mesh: incoming handshake zakonczony przed finalizacja: {}", e);
                            } else {
                                warn!("iroh_mesh: obsluga incoming nieudana: {}", e);
                            }
                        }
                    });
                }
            }
        }
    }

    fn clone_for_spawn(&self) -> IrohMeshManagerRef {
        IrohMeshManagerRef {
            connections: Arc::clone(&self.connections),
            event_tx: self.event_tx.clone(),
            security: Arc::clone(&self.security),
            forward_handler: Arc::clone(&self.forward_handler),
            forward_stream_handler: Arc::clone(&self.forward_stream_handler),
            camera_stream_handler: Arc::clone(&self.camera_stream_handler),
            lidar_stream_handler: Arc::clone(&self.lidar_stream_handler),
        }
    }

    async fn handle_incoming(&self, incoming: iroh::endpoint::Incoming) -> Result<()> {
        let connecting = incoming.accept().context("accept incoming")?;
        let connection = connecting.await.context("finalize connection")?;
        let alpn = connection.alpn();

        let remote_id = connection.remote_id();
        let remote_hex = hex::encode(remote_id.as_bytes());
        match alpn {
            a if a == ALPN_MESH => {
                match self
                    .register_connection(
                        remote_hex.clone(),
                        connection.clone(),
                        ConnectionDirection::Incoming,
                    )
                    .await
                {
                    Some(connection_id) => {
                        let _ = self.event_tx.send(IrohMeshEvent::PeerConnected {
                            node_id: remote_hex.clone(),
                        });
                        info!(peer = %remote_hex, "iroh_mesh: polaczenie nawiazane (incoming)");
                        self.note_dial_success(&remote_hex);
                        let me = self.clone_for_spawn();
                        tokio::spawn(async move {
                            me.handle_mesh_connection(remote_hex, connection, connection_id)
                                .await;
                        });
                    }
                    None => {
                        debug!(
                            peer = %remote_hex,
                            "iroh_mesh: incoming connection odrzucone przez tie-break"
                        );
                    }
                }
            }
            a if a == ALPN_PAIRING => {
                // PairingHandler::accept uzywany przez iroh Router jest tutaj
                // zastepowany manualnym obslugiwaniem — w pelnej integracji
                // ProtocolHandler jest rejestrowany przy bind przez Router.
                let handler = PairingHandler::new(
                    Arc::clone(&self.security),
                    crate::mesh::node_info_collector::local_hostname(),
                );
                match handler_accept_connection(&handler, connection).await {
                    Ok(Some(hints)) => {
                        let _ = self.event_tx.send(IrohMeshEvent::PairingTrusted { hints });
                    }
                    Ok(None) => {}
                    Err(e) => warn!("iroh_mesh: pairing accept blad: {}", e),
                }
            }
            a if a == ALPN_BASELINE => {
                // Strona dawcy baseline-adopt: joiner dialuje, my akceptujemy
                // bidirectional stream i wykonujemy sekwencje dawcy. remote_id z
                // polaczenia jest autorytatywnym node_id peera (anti-spoof).
                let security = Arc::clone(&self.security);
                let local_node_id = self.node_id();
                tokio::spawn(async move {
                    let (send, recv) = match connection.accept_bi().await {
                        Ok(v) => v,
                        Err(e) => {
                            warn!(peer = %remote_hex, "baseline: accept_bi nieudane: {}", e);
                            return;
                        }
                    };
                    let mut stream =
                        crate::sync::baseline_transport::IrohFrameStream::new(send, recv);
                    match crate::sync::baseline_transport::run_donor_session(
                        &mut stream,
                        &security,
                        &local_node_id,
                        &remote_hex,
                    )
                    .await
                    {
                        Ok(()) => info!(peer = %remote_hex, "baseline: donor session OK"),
                        Err(e) => warn!(peer = %remote_hex, "baseline: donor session blad: {}", e),
                    }
                });
            }
            a if a == ALPN_ARTIFACT => {
                // Odbiorca bulk-transferu artefaktu modelu. Nadawca (zaufany peer)
                // otwiera bi-stream i pcha [name_len u32][name][zip_len u64][zip];
                // my składamy, rozpakowujemy do lokalnego katalogu i odsyłamy
                // [path_len u32][path]. remote_id z połączenia = autorytatywny peer.
                if !self.security.is_trusted(&remote_hex) {
                    warn!(peer = %remote_hex, "artifact: nieufny peer — odrzucam");
                    connection.close(0u32.into(), b"untrusted");
                    return Ok(());
                }
                tokio::spawn(async move {
                    let (mut send, mut recv) = match connection.accept_bi().await {
                        Ok(v) => v,
                        Err(e) => {
                            warn!(peer = %remote_hex, "artifact: accept_bi nieudane: {}", e);
                            return;
                        }
                    };
                    match crate::ml_studio::mesh_artifact::recv_artifact_stream(&mut recv).await {
                        Ok((name, zip_path)) => {
                            // Unzip + walidacja to długie sync IO (snapshot HF =
                            // setki GB) — spawn_blocking, żeby nie dławić runtime.
                            let zip_for_store = zip_path.clone();
                            let stored = tokio::task::spawn_blocking(move || {
                                crate::ml_studio::mesh_artifact::store_artifact_zip(
                                    &name,
                                    &zip_for_store,
                                )
                            })
                            .await
                            .unwrap_or_else(|e| Err(anyhow::anyhow!("store join: {e}")));
                            crate::ml_studio::mesh_artifact::remove_transfer_tmp(&zip_path);
                            match stored {
                                Ok(path) => {
                                    let pb = path.as_bytes();
                                    let _ = send.write_all(&(pb.len() as u32).to_be_bytes()).await;
                                    let _ = send.write_all(pb).await;
                                    let _ = send.finish();
                                    info!(peer = %remote_hex, "artifact: odebrano i rozpakowano do {}", path);
                                }
                                Err(e) => {
                                    warn!(peer = %remote_hex, "artifact: rozpakowanie nieudane: {}", e);
                                    let _ = send.write_all(&0u32.to_be_bytes()).await;
                                    let _ = send.finish();
                                }
                            }
                        }
                        Err(e) => {
                            warn!(peer = %remote_hex, "artifact: odbiór streamu nieudany: {}", e)
                        }
                    }
                    // Daj czas na flush odpowiedzi zanim połączenie zniknie.
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                });
            }
            a if a == ALPN_API => {
                debug!(
                    "iroh_mesh: ALPN_API otrzymane — delegacja do dashboard layer (zadanie #56)"
                );
            }
            other => {
                warn!(
                    "iroh_mesh: nieznany ALPN: {:?}",
                    String::from_utf8_lossy(other)
                );
            }
        }
        Ok(())
    }

    /// Rejestruje fizyczna QUIC connection w mapie z deterministycznym tie-break'em.
    ///
    /// Gdy A i B dialuja sie jednoczesnie, iroh tworzy dwa oddzielne connections
    /// (A→B outgoing u A / incoming u B, i odwrotnie). Bez tie-break'u kazda strona
    /// zatrzymywala swoje ostatnie (outgoing) i zamykala przeciwne (incoming) —
    /// koncowo obie strony trzymaly _rozne_ fizyczne connections i nie mogly nic
    /// wymienic.
    ///
    /// Reguła: wygrywa connection, ktorej dialer ma leksykograficznie mniejszy
    /// hex endpoint_id. Obie strony patrza na te same ID → zbiegaja sie na tym
    /// samym fizycznym connectionie.
    ///
    /// Zwraca `Some(id)` gdy ta connection wygrala i zostala w mapie.
    /// Zwraca `None` gdy przegrala — connection jest zamknieta, caller NIE powinien
    /// emitowac `PeerConnected` ani uruchamiac `handle_mesh_connection`.
    async fn register_connection(
        &self,
        remote_hex: String,
        conn: Connection,
        direction: ConnectionDirection,
    ) -> Option<u64> {
        let self_hex = self.local_node_id.read().clone();
        // Preferowany dialer to ten z leksykograficznie mniejszym endpoint_id.
        let prefer_outgoing = self_hex.as_str() < remote_hex.as_str();
        let new_is_winner = matches!(
            (direction, prefer_outgoing),
            (ConnectionDirection::Outgoing, true) | (ConnectionDirection::Incoming, false)
        );

        let new_id = self.next_connection_id.fetch_add(1, Ordering::Relaxed);

        use dashmap::mapref::entry::Entry;
        match self.connections.entry(remote_hex.clone()) {
            Entry::Occupied(mut occ) => {
                let existing_dir = occ.get().direction;
                if existing_dir == direction {
                    // Duplikat tego samego kierunku — iroh retry/migration.
                    drop(occ);
                    conn.close(0u32.into(), b"duplicate");
                    None
                } else if !new_is_winner {
                    drop(occ);
                    conn.close(0u32.into(), b"tie-break-loser");
                    None
                } else {
                    let prev = occ.insert(ActiveConnection {
                        id: new_id,
                        connection: conn,
                        direction,
                    });
                    drop(occ);
                    prev.connection.close(0u32.into(), b"tie-break-loser");
                    Some(new_id)
                }
            }
            Entry::Vacant(vac) => {
                vac.insert(ActiveConnection {
                    id: new_id,
                    connection: conn,
                    direction,
                });
                Some(new_id)
            }
        }
    }

    /// Relay dokladany do kazdego wychodzacego diala. endpoint.addr() zna
    /// relay dopiero po zestawieniu sesji home-relay — tuz po starcie lista
    /// jest pusta i dial tworzylby polaczenie p2p-only bez sciezki relay do
    /// failoveru. Skonfigurowany relay z configu/DB jest wtedy backstopem.
    fn dial_relay_url(&self) -> Option<RelayUrl> {
        self.endpoint
            .inner()
            .addr()
            .relay_urls()
            .next()
            .cloned()
            .or_else(|| self.config.relay_url.clone())
    }

    /// Zwraca (lub tworzy) per-peer mutex zabezpieczajacy przed rownoleglymi
    /// dialami do tego samego peera z roznych tasków.
    fn dial_lock_for(&self, peer_hex: &str) -> Arc<tokio::sync::Mutex<()>> {
        self.dial_locks
            .entry(peer_hex.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    // =========================================================================
    // Public API (podzbior odpowiadajacy QuicMeshManager)
    // =========================================================================

    pub fn node_id(&self) -> String {
        self.local_node_id.read().clone()
    }

    pub fn endpoint_id(&self) -> EndpointId {
        self.endpoint.id()
    }

    pub fn endpoint(&self) -> &iroh::Endpoint {
        self.endpoint.inner()
    }

    pub fn relay_url(&self) -> Option<RelayUrl> {
        self.config.relay_url.clone()
    }

    pub fn connection_snapshot(&self, node_id: &str) -> Option<ConnectionSnapshot> {
        let active = self.connections.get(node_id)?;
        Some(connection_snapshot_from_connection(&active.connection))
    }

    pub fn connection_snapshots(&self) -> HashMap<String, ConnectionSnapshot> {
        self.connections
            .iter()
            .map(|entry| {
                (
                    entry.key().clone(),
                    connection_snapshot_from_connection(&entry.value().connection),
                )
            })
            .collect()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<IrohMeshEvent> {
        self.event_tx.subscribe()
    }

    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        self.connections.clear();
        // Bez tego iroh przy dropie tokio runtime cancellowal wszystkie
        // ActiveRelayActor sequencyjnie generujac setki linii spamu
        // "JoinError::Cancelled" i "Home relay not set". `close()` dorzuca
        // CONNECTION_CLOSE peerom i czeka az relay actorzy zamkna kanaly
        // czysto. Awaitujemy z timeout 3s zeby shutdown nie wisial gdy
        // relay nie odpowiada.
        let close_fut = self.endpoint.inner().close();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), close_fut).await;
    }

    /// Laczy sie z peerem po hex-enkodowanym EndpointId. Gdy caller poda
    /// konkretny `SocketAddr`, dolaczamy go do EndpointAddr jako fallback dla
    /// recznego diala po adresie z peer_store. To pozwala na pairing po samym
    /// hash ID nawet wtedy, gdy iroh discovery jeszcze nie zna adresu peera.
    pub async fn connect_to_peer(
        &self,
        node_id_hex: &str,
        addr: std::net::SocketAddr,
    ) -> Result<()> {
        // iroh rzucilby blad przy dialu do siebie, ale taniej odrzucic tutaj.
        if node_id_hex == self.local_node_id.read().as_str() {
            return Ok(());
        }
        let peer_id_str = node_id_hex.to_string();
        let lock = self.dial_lock_for(&peer_id_str);
        let _guard = lock.lock().await;
        // Po wzieciu locka re-sprawdz is_connected — inny task mogl juz zadialowac.
        if self.is_connected(&peer_id_str).await {
            return Ok(());
        }
        // Relay-first nawet przy dialu z samej dyskowerki: direct IP idzie w
        // parze z naszym home relay, co otwiera sciezke jesli peer siedzi za
        // NATem albo w innej sieci.
        let mut endpoint_addr = endpoint_addr_from_target(node_id_hex, Some(addr))?;
        if let Some(relay) = self.dial_relay_url() {
            endpoint_addr = endpoint_addr.with_relay_url(relay);
        }
        let connection = self
            .endpoint
            .connect(endpoint_addr, ALPN_MESH)
            .await
            .map_err(|e| anyhow::anyhow!("iroh connect: {e:?}"))?;
        match self
            .register_connection(
                peer_id_str.clone(),
                connection.clone(),
                ConnectionDirection::Outgoing,
            )
            .await
        {
            Some(connection_id) => {
                let _ = self.event_tx.send(IrohMeshEvent::PeerConnected {
                    node_id: peer_id_str.clone(),
                });
                info!(peer = %peer_id_str, "iroh_mesh: polaczenie nawiazane (outgoing)");
                let me = self.clone_for_spawn();
                tokio::spawn(async move {
                    me.handle_mesh_connection(peer_id_str, connection, connection_id)
                        .await;
                });
                Ok(())
            }
            None => {
                debug!(
                    peer = %peer_id_str,
                    "iroh_mesh: outgoing odrzucone przez tie-break, peer polaczony przez incoming"
                );
                Ok(())
            }
        }
    }

    /// Laczy sie z peerem podajac explicit direct address (IP+port). Uzywane
    /// na iOS gdzie swarm-discovery mDNS nie dziala — Swift NWBrowser znajduje
    /// peera przez systemowy Bonjour i przekazuje adres do Rust. iroh probuje
    /// hole-punch na direct addr; jak padnie → fallback na relay.
    pub async fn connect_to_peer_direct(
        &self,
        node_id_hex: &str,
        direct_addr: std::net::SocketAddr,
    ) -> Result<()> {
        if node_id_hex == self.local_node_id.read().as_str() {
            return Ok(());
        }
        let peer_id_str = node_id_hex.to_string();
        let lock = self.dial_lock_for(&peer_id_str);
        let _guard = lock.lock().await;
        if self.is_connected(&peer_id_str).await {
            return Ok(());
        }
        let endpoint_id = parse_endpoint_id(node_id_hex)?;
        let mut addr = EndpointAddr::new(endpoint_id).with_ip_addr(direct_addr);
        if let Some(relay) = self.dial_relay_url() {
            addr = addr.with_relay_url(relay);
        }
        let connection = self
            .endpoint
            .connect(addr, ALPN_MESH)
            .await
            .map_err(|e| anyhow::anyhow!("iroh connect direct: {e:?}"))?;
        match self
            .register_connection(
                peer_id_str.clone(),
                connection.clone(),
                ConnectionDirection::Outgoing,
            )
            .await
        {
            Some(connection_id) => {
                let _ = self.event_tx.send(IrohMeshEvent::PeerConnected {
                    node_id: peer_id_str.clone(),
                });
                let me = self.clone_for_spawn();
                tokio::spawn(async move {
                    me.handle_mesh_connection(peer_id_str, connection, connection_id)
                        .await;
                });
                Ok(())
            }
            None => {
                debug!(
                    peer = %peer_id_str,
                    "iroh_mesh: outgoing direct odrzucone przez tie-break, peer polaczony przez incoming"
                );
                Ok(())
            }
        }
    }

    pub async fn connect_to_peer_with_hints(&self, hints: &PairingContactHints) -> Result<()> {
        if hints.node_id == *self.local_node_id.read() {
            return Ok(());
        }
        let peer_id_str = hints.node_id.clone();
        let lock = self.dial_lock_for(&peer_id_str);
        let _guard = lock.lock().await;
        if self.is_connected(&peer_id_str).await {
            return Ok(());
        }
        // Relay-first: dokladamy nasz home relay jako fallback zawsze gdy
        // hints go nie maja (direct addrs leca rownolegle).
        let hints_resolved = hints_with_relay_fallback(
            self.endpoint.inner(),
            hints,
            self.config.relay_url.as_ref().map(|u| u.as_str()),
        );
        let addr = endpoint_addr_from_hints(&hints_resolved)?;
        let connection = self
            .endpoint
            .connect(addr, ALPN_MESH)
            .await
            .map_err(|e| anyhow::anyhow!("iroh connect hinted: {e:?}"))?;
        match self
            .register_connection(
                peer_id_str.clone(),
                connection.clone(),
                ConnectionDirection::Outgoing,
            )
            .await
        {
            Some(connection_id) => {
                let _ = self.event_tx.send(IrohMeshEvent::PeerConnected {
                    node_id: peer_id_str.clone(),
                });
                let me = self.clone_for_spawn();
                tokio::spawn(async move {
                    me.handle_mesh_connection(peer_id_str, connection, connection_id)
                        .await;
                });
                Ok(())
            }
            None => {
                debug!(
                    peer = %peer_id_str,
                    "iroh_mesh: outgoing hinted odrzucone przez tie-break, peer polaczony przez incoming"
                );
                Ok(())
            }
        }
    }

    /// Joiner baseline-adopt: dialuje dawce na ALPN_BASELINE i wykonuje pelna
    /// sekwencje pobrania snapshotu (Elect -> Ack -> Header -> chunki -> import).
    /// Dial uzywa zapisanych trusted contact hints dawcy (adres + relay), bo
    /// pairing juz potwierdzony. Po sukcesie joiner ma stan org dawcy.
    ///
    /// Wywolywane przez `begin_baseline_adopt_after_confirm` (po confirm) gdy
    /// lokalny nod jest joinerem, oraz przez crash-recovery przy starcie.
    pub async fn pull_baseline_from_donor(
        &self,
        donor_node_id: &str,
        epoch_seen: u64,
    ) -> Result<()> {
        let local_node_id = self.node_id();
        if donor_node_id == local_node_id {
            return Err(anyhow::anyhow!(
                "baseline pull: donor == local node — nothing to pull"
            ));
        }

        let hints = load_trusted_contact_hints(&self.security.db, donor_node_id)
            .map_err(|e| anyhow::anyhow!("baseline pull: load donor hints: {e}"))?
            .ok_or_else(|| {
                anyhow::anyhow!("baseline pull: no trusted contact hints for donor {donor_node_id}")
            })?;
        let hints_resolved = hints_with_relay_fallback(
            self.endpoint.inner(),
            &hints,
            self.config.relay_url.as_ref().map(|u| u.as_str()),
        );
        let addr = endpoint_addr_from_hints(&hints_resolved)
            .map_err(|e| anyhow::anyhow!("baseline pull: donor addr: {e}"))?;

        let connection = self
            .endpoint
            .connect(addr, ALPN_BASELINE)
            .await
            .map_err(|e| anyhow::anyhow!("baseline pull: connect ALPN_BASELINE: {e:?}"))?;
        let (send, recv) = connection
            .open_bi()
            .await
            .map_err(|e| anyhow::anyhow!("baseline pull: open_bi: {e}"))?;
        let mut stream = crate::sync::baseline_transport::IrohFrameStream::new(send, recv);
        let cipher = Arc::clone(self.security.settings_cipher_ref());
        crate::sync::baseline_transport::run_joiner_session(
            &mut stream,
            &self.security.db,
            &local_node_id,
            donor_node_id,
            &cipher,
            epoch_seen,
        )
        .await
        .map_err(|e| anyhow::anyhow!("baseline pull: joiner session: {e}"))?;
        Ok(())
    }

    /// Bulk-push artefaktu modelu (ZIP w PLIKU) do `target_node_id` jednym
    /// bi-streamem na ALPN_ARTIFACT — zamiast tysięcy round-tripów komend mesh.
    /// Wysyła `[name_len u32][name][zip_len u64][zip]` czytając plik porcjami
    /// (stały RAM — snapshot HF potrafi mieć setki GB), odbiera
    /// `[path_len u32][path]` (ścieżka katalogu artefaktu na węźle docelowym).
    /// Zwraca tę ścieżkę.
    pub async fn push_artifact_stream(
        &self,
        target_node_id: &str,
        name: &str,
        zip_path: &std::path::Path,
        progress_key: Option<&str>,
    ) -> Result<String> {
        use crate::ml_studio::mesh_artifact::{
            ArtifactTransferProgress, ARTIFACT_CHUNK_BYTES, ARTIFACT_STALL_SECS,
        };
        let hints = load_trusted_contact_hints(&self.security.db, target_node_id)
            .map_err(|e| anyhow::anyhow!("artifact push: load hints: {e}"))?
            .ok_or_else(|| anyhow::anyhow!("artifact push: brak hintów dla {target_node_id}"))?;
        let hints_resolved = hints_with_relay_fallback(
            self.endpoint.inner(),
            &hints,
            self.config.relay_url.as_ref().map(|u| u.as_str()),
        );
        let addr = endpoint_addr_from_hints(&hints_resolved)
            .map_err(|e| anyhow::anyhow!("artifact push: addr: {e}"))?;
        let connection = self
            .endpoint
            .connect(addr, ALPN_ARTIFACT)
            .await
            .map_err(|e| anyhow::anyhow!("artifact push: connect ALPN_ARTIFACT: {e:?}"))?;
        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|e| anyhow::anyhow!("artifact push: open_bi: {e}"))?;

        let total = tokio::fs::metadata(zip_path)
            .await
            .map_err(|e| anyhow::anyhow!("artifact push: metadata zip: {e}"))?
            .len();
        send.write_all(&(name.len() as u32).to_be_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("artifact push: write name_len: {e}"))?;
        send.write_all(name.as_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("artifact push: write name: {e}"))?;
        send.write_all(&total.to_be_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("artifact push: write zip_len: {e}"))?;

        // Strumieniujemy ZIP z PLIKU porcjami (`read` do bufora → zapisy CZĄSTKOWE
        // `write`, nie `write_all`) z watchdogiem STALL: każdy `write` przyjmuje
        // tyle bajtów, ile zmieści okno flow-control QUIC, i zwraca natychmiast po
        // przyjęciu JAKICHKOLWIEK bajtów. Świeży limit bezczynności
        // (ARTIFACT_STALL_SECS) na każdy `write` → dopóki choć bajt zostaje
        // przyjęty w oknie, licznik się resetuje. Timeout = ZERO przyjętych bajtów
        // przez okno = odbiorca nie czyta (STALL). Aktywny transfer (nawet bardzo
        // wolny) NIGDY nie pada na sztywny deadline. Postęp → mapa B/s.
        use tokio::io::AsyncReadExt;
        let mut file = tokio::fs::File::open(zip_path)
            .await
            .map_err(|e| anyhow::anyhow!("artifact push: open zip: {e}"))?;
        let stall = std::time::Duration::from_secs(ARTIFACT_STALL_SECS);
        let mut sent: u64 = 0;
        let mut window_start = tokio::time::Instant::now();
        let mut window_bytes: u64 = 0;
        let mut rate_bps: u64 = 0;
        let mut buf = vec![0u8; ARTIFACT_CHUNK_BYTES];
        while sent < total {
            let want = ((total - sent) as usize).min(buf.len());
            let filled = file
                .read(&mut buf[..want])
                .await
                .map_err(|e| anyhow::anyhow!("artifact push: read zip: {e}"))?;
            if filled == 0 {
                anyhow::bail!("artifact push: plik zip skrócony w trakcie ({sent}/{total} B)");
            }
            let mut off = 0usize;
            while off < filled {
                let n = match tokio::time::timeout(stall, send.write(&buf[off..filled])).await {
                    Ok(Ok(n)) if n > 0 => n,
                    Ok(Ok(_)) => continue,
                    Ok(Err(e)) => return Err(anyhow::anyhow!("artifact push: write zip: {e}")),
                    Err(_) => {
                        if let Some(k) = progress_key {
                            crate::ml_studio::mesh_artifact::clear_artifact_progress(k);
                        }
                        anyhow::bail!(
                            "artifact push: transfer utknął — brak przyjętych bajtów przez {}s ({}/{} B)",
                            ARTIFACT_STALL_SECS,
                            sent,
                            total
                        );
                    }
                };
                sent += n as u64;
                window_bytes += n as u64;
                off += n;
                let win = window_start.elapsed().as_secs_f64();
                if win >= 1.0 {
                    rate_bps = (window_bytes as f64 / win) as u64;
                    window_start = tokio::time::Instant::now();
                    window_bytes = 0;
                }
                if let Some(k) = progress_key {
                    crate::ml_studio::mesh_artifact::set_artifact_progress_pub(
                        k,
                        ArtifactTransferProgress {
                            bytes_sent: sent,
                            bytes_total: total,
                            rate_bps,
                        },
                    );
                }
            }
        }
        send.finish()
            .map_err(|e| anyhow::anyhow!("artifact push: finish: {e}"))?;

        // Odpowiedź `[path_len][path]` z odbiorcy. Odbiorca NAJPIERW rozpakowuje i
        // waliduje cały ZIP (dla snapshotu HF to setki GB = pojedyncze minuty
        // czystego IO), a dopiero potem pisze odpowiedź — w tym oknie nie płynie
        // ŻADEN bajt, więc pierwszy odczyt dostaje osobny, duży budżet zamiast
        // 30-sekundowego STALL (ktory mierzy transfer, nie unzip).
        let unzip_budget = std::time::Duration::from_secs(3600);
        let mut len_buf = [0u8; 4];
        read_reply_stall(&mut recv, &mut len_buf, unzip_budget).await?;
        let n = u32::from_be_bytes(len_buf) as usize;
        if n == 0 {
            anyhow::bail!("węzeł docelowy nie zapisał artefaktu");
        }
        let mut pbuf = vec![0u8; n];
        read_reply_stall(&mut recv, &mut pbuf, stall).await?;
        Ok(String::from_utf8_lossy(&pbuf).to_string())
    }

    /// Wysyla ramke `[disc][data]` na uni streamie do peera.
    pub async fn connected_peers(&self) -> Vec<String> {
        self.connections.iter().map(|e| e.key().clone()).collect()
    }

    pub async fn is_connected(&self, node_id: &str) -> bool {
        self.connections.contains_key(node_id)
    }

    pub async fn disconnect_peer(&self, node_id: &str) {
        if let Some((_, active)) = self.connections.remove(node_id) {
            active.connection.close(0u32.into(), b"disconnect");
            let _ = self.event_tx.send(IrohMeshEvent::PeerDisconnected {
                node_id: node_id.to_string(),
            });
        }
        // Sprzataj per-peer dial lock — odpada gdy rozlaczany peer
        // nie bedzie juz dialowany w tym cyklu zycia managera.
        self.dial_locks.remove(node_id);
    }

    // =========================================================================
    // Convenience wrappers — odpowiedniki metod QuicMeshManager. Kazdy
    // wraca payload w podpisanej UFP/2 envelope przez `send_ufp2_to_peer`.
    // =========================================================================

    pub async fn send_heartbeat_data(&self, data: &[u8]) {
        use futures::future::join_all;
        // UFP/2 wire: the heartbeat body is CBOR, wrapped in a signed envelope.
        let mut futs = Vec::with_capacity(self.connections.len());
        for entry in self.connections.iter() {
            let id = entry.key().clone();
            futs.push(async move {
                if let Err(e) = self
                    .send_ufp2_to_peer(&id, tentaflow_protocol::mesh::MESH_MSG_HEARTBEAT, data)
                    .await
                {
                    tracing::debug!(
                        target: "mesh::ufp2",
                        peer = %id,
                        error = %e,
                        "send_heartbeat_data: UFP/2 heartbeat send failed"
                    );
                }
            });
        }
        join_all(futs).await;
    }

    /// UFP/2 sender path: write the full envelope bytes to a fresh
    /// uni-stream without any leading discriminator byte. The first byte
    /// of `wire` is the CBOR map header — receivers detect UFP/2 vs
    /// legacy by inspecting it (`mesh::ufp2::looks_like_ufp2_envelope_first_byte`).
    async fn send_raw_envelope_to_peer(&self, target_node_id: &str, wire: &[u8]) -> Result<()> {
        let connection = self
            .connections
            .get(target_node_id)
            .ok_or_else(|| anyhow::anyhow!("brak polaczenia z {}", target_node_id))?
            .connection
            .clone();
        let mut send = connection
            .open_uni()
            .await
            .map_err(|e| anyhow::anyhow!("open_uni: {e}"))?;
        send.write_all(wire)
            .await
            .map_err(|e| anyhow::anyhow!("write ufp2 envelope: {e}"))?;
        send.finish()
            .map_err(|e| anyhow::anyhow!("finish ufp2 uni: {e}"))?;
        Ok(())
    }

    /// Jak `send_raw_envelope_to_peer`, ale po `finish()` czeka na potwierdzenie
    /// odbioru strumienia przez peera (`SendStream::stopped` → `Ok(None)` gdy peer
    /// zack'owal odbior danych po naszym FIN), z ograniczeniem `ack_timeout`.
    /// Bez tej bariery `finish()` tylko lokalnie kolejkuje FIN, wiec nastepowy
    /// `Connection::close` moze kazac zdalnej stronie porzucic jeszcze
    /// niedostarczone dane (kontrakt quinn). Timeout, bo revokowany/wolny peer
    /// moze nie zack'owac na czas — wtedy i tak wracamy (best-effort delivery).
    async fn send_raw_envelope_to_peer_acked(
        &self,
        target_node_id: &str,
        wire: &[u8],
        ack_timeout: Duration,
    ) -> Result<()> {
        let connection = self
            .connections
            .get(target_node_id)
            .ok_or_else(|| anyhow::anyhow!("brak polaczenia z {}", target_node_id))?
            .connection
            .clone();
        let mut send = connection
            .open_uni()
            .await
            .map_err(|e| anyhow::anyhow!("open_uni: {e}"))?;
        send.write_all(wire)
            .await
            .map_err(|e| anyhow::anyhow!("write ufp2 envelope: {e}"))?;
        send.finish()
            .map_err(|e| anyhow::anyhow!("finish ufp2 uni: {e}"))?;
        let _ = tokio::time::timeout(ack_timeout, send.stopped()).await;
        Ok(())
    }

    /// Buduje podpisana koperte UFP/2 wokol `data` dla jednego peera (walidacja
    /// dyskryminatora + wyprowadzenie pubkeya z iroh node id). Wspoldzielona
    /// przez `send_ufp2_to_peer` i wariant z ackiem.
    fn build_ufp2_wire(
        &self,
        target_node_id: &str,
        legacy_discriminator: u8,
        data: &[u8],
    ) -> Result<Vec<u8>> {
        if !crate::mesh::ufp2::is_migrated_to_ufp2_discriminator(legacy_discriminator) {
            return Err(anyhow::anyhow!(
                "send_ufp2_to_peer: discriminator 0x{:02X} is not on the UFP/2 unicast allowlist (bi-stream types FORWARD_REQ/FORWARD_STREAM_REQ use their own protocol)",
                legacy_discriminator
            ));
        }
        let dest_pubkey = parse_iroh_node_id_to_pubkey(target_node_id).ok_or_else(|| {
            anyhow::anyhow!(
                "send_ufp2_to_peer: cannot parse iroh node id {} as Ed25519 pubkey",
                target_node_id
            )
        })?;
        let source_pubkey = self.security.verifying_key_bytes();
        let epoch = self.current_policy_epoch();
        crate::mesh::ufp2::build_signed_envelope_wire(
            self.security.signing_key(),
            source_pubkey,
            dest_pubkey,
            legacy_discriminator,
            data.to_vec(),
            epoch,
        )
        .map_err(|e| anyhow::anyhow!("send_ufp2_to_peer: envelope build failed: {e}"))
    }

    /// Build a signed UFP/2 envelope around `data` for a single peer and
    /// dispatch it on a fresh uni-stream. Used by every send wrapper that
    /// has been migrated off the legacy raw discriminator wire. The
    /// destination peer's Ed25519 pubkey is derived from its iroh node id
    /// (which IS the pubkey in iroh's identity model).
    pub(crate) async fn send_ufp2_to_peer(
        &self,
        target_node_id: &str,
        legacy_discriminator: u8,
        data: &[u8],
    ) -> Result<()> {
        let wire = self.build_ufp2_wire(target_node_id, legacy_discriminator, data)?;
        self.send_raw_envelope_to_peer(target_node_id, &wire).await
    }

    /// Jak `send_ufp2_to_peer`, ale czeka na potwierdzenie odbioru przez peera
    /// (do `ack_timeout`) przed powrotem. Uzywane gdy nadawca zaraz zamknie
    /// polaczenie (revoke), zeby `Connection::close` nie porzucil notyfikacji.
    pub(crate) async fn send_ufp2_to_peer_acked(
        &self,
        target_node_id: &str,
        legacy_discriminator: u8,
        data: &[u8],
        ack_timeout: Duration,
    ) -> Result<()> {
        let wire = self.build_ufp2_wire(target_node_id, legacy_discriminator, data)?;
        self.send_raw_envelope_to_peer_acked(target_node_id, &wire, ack_timeout)
            .await
    }

    fn current_policy_epoch(&self) -> u32 {
        crate::db::repository::get_sync_permission_epoch(
            &self.security.db,
            crate::services::org::DEFAULT_ORG_ID,
        )
        .map(|epoch| epoch.min(u32::MAX as u64) as u32)
        .unwrap_or_else(|e| {
            tracing::warn!(
                target: "mesh::ufp2",
                error = %e,
                "current_policy_epoch: failed to read sync permission epoch"
            );
            0
        })
    }

    /// UFP/2 broadcast helper: build a per-peer signed envelope (each
    /// envelope's `destination.id` must match the receiver's pubkey, so
    /// broadcast cannot share one wire blob the way the legacy
    /// raw discriminator path could) and send to every trusted peer except
    /// `exclude`. Returns per-peer results so callers can log failures.
    pub async fn broadcast_ufp2_to_trusted(
        &self,
        legacy_discriminator: u8,
        data: &[u8],
        exclude: Option<&str>,
    ) -> Vec<(String, Result<()>)> {
        use futures::future::join_all;
        let trusted = self.security.trusted_node_ids_snapshot();
        let mut futs = Vec::with_capacity(self.connections.len());
        for entry in self.connections.iter() {
            let id = entry.key();
            if !trusted.contains(id) {
                continue;
            }
            if let Some(e) = exclude {
                if id.as_str() == e {
                    continue;
                }
            }
            let node_id = id.clone();
            futs.push(async move {
                let res = self
                    .send_ufp2_to_peer(&node_id, legacy_discriminator, data)
                    .await;
                (node_id, res)
            });
        }
        join_all(futs).await
    }

    /// Broadcast listy modeli do wszystkich polaczonych peerow. Wywolywane
    /// co `models_sync_interval` z pipeline.
    pub async fn send_models_sync_data(&self, data: &[u8]) {
        use futures::future::join_all;
        let mut futs = Vec::with_capacity(self.connections.len());
        for entry in self.connections.iter() {
            let id = entry.key().clone();
            futs.push(async move {
                let _ = self
                    .send_ufp2_to_peer(&id, tentaflow_protocol::mesh::MESH_MSG_MODEL_LIST, data)
                    .await;
            });
        }
        join_all(futs).await;
    }

    pub async fn send_node_info(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_ufp2_to_peer(node_id, tentaflow_protocol::mesh::MESH_MSG_NODE_INFO, data)
            .await
    }

    pub async fn send_hello(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_ufp2_to_peer(node_id, tentaflow_protocol::mesh::MESH_MSG_HELLO, data)
            .await
    }

    /// Wysyla TopologyAnnounce do jednego zaufanego peera (unicast).
    /// Broadcast realizuje pipeline przez iteracje listy peerow.
    pub async fn send_topology_announce(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_ufp2_to_peer(
            node_id,
            tentaflow_protocol::mesh::MESH_MSG_TOPOLOGY_ANNOUNCE,
            data,
        )
        .await
    }

    pub async fn send_known_peers(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_ufp2_to_peer(
            node_id,
            tentaflow_protocol::mesh::MESH_MSG_KNOWN_PEERS,
            data,
        )
        .await
    }

    pub async fn send_pairing_request(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_ufp2_to_peer(
            node_id,
            tentaflow_protocol::mesh::MESH_MSG_PAIRING_REQUEST,
            data,
        )
        .await
    }

    pub async fn send_pairing_confirm(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_ufp2_to_peer(
            node_id,
            tentaflow_protocol::mesh::MESH_MSG_PAIRING_CONFIRM,
            data,
        )
        .await
    }

    pub async fn send_pairing_reject(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_ufp2_to_peer(
            node_id,
            tentaflow_protocol::mesh::MESH_MSG_PAIRING_REJECT,
            data,
        )
        .await
    }

    pub async fn send_trust_revoked(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_ufp2_to_peer(
            node_id,
            tentaflow_protocol::mesh::MESH_MSG_TRUST_REVOKED,
            data,
        )
        .await
    }

    pub async fn send_trusted_keys_sync(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_ufp2_to_peer(
            node_id,
            tentaflow_protocol::mesh::MESH_MSG_TRUSTED_KEYS_SYNC,
            data,
        )
        .await
    }

    /// F1b P3.B — push this node's HMAC issuer keys to a trust-paired peer.
    /// Caller is responsible for trust + cooldown gating; this is a thin
    /// wrapper around `send_to_peer` with the right discriminant.
    pub async fn send_hmac_keys_sync(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_ufp2_to_peer(
            node_id,
            tentaflow_protocol::mesh::MESH_MSG_HMAC_KEYS_SYNC,
            data,
        )
        .await
    }

    pub async fn send_sync_push(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_ufp2_to_peer(node_id, tentaflow_protocol::mesh::MESH_MSG_SYNC_PUSH, data)
            .await
    }

    pub async fn send_sync_ack(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_ufp2_to_peer(node_id, tentaflow_protocol::mesh::MESH_MSG_SYNC_ACK, data)
            .await
    }

    pub async fn send_sync_pull(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_ufp2_to_peer(node_id, tentaflow_protocol::mesh::MESH_MSG_SYNC_PULL, data)
            .await
    }

    pub async fn send_sync_pull_response(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_ufp2_to_peer(
            node_id,
            tentaflow_protocol::mesh::MESH_MSG_SYNC_PULL_RESPONSE,
            data,
        )
        .await
    }

    pub async fn send_sync_snapshot_pull(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_ufp2_to_peer(
            node_id,
            tentaflow_protocol::mesh::MESH_MSG_SYNC_SNAPSHOT_PULL,
            data,
        )
        .await
    }

    pub async fn send_sync_snapshot_response(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_ufp2_to_peer(
            node_id,
            tentaflow_protocol::mesh::MESH_MSG_SYNC_SNAPSHOT_RESPONSE,
            data,
        )
        .await
    }

    /// F1b P3.C-1 — send a frame proxy request to a trust-paired peer.
    /// Caller is responsible for trust gating + correlating the
    /// `request_id` with a pending response slot (P3.C-2 wires the slot
    /// map). `data` is the CBOR-encoded `FrameProxyRequestPayload`.
    pub async fn send_frame_proxy_request(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_ufp2_to_peer(
            node_id,
            tentaflow_protocol::mesh::MESH_MSG_FRAME_PROXY_REQUEST,
            data,
        )
        .await
    }

    /// F1b P3.C-1 — send a frame proxy response to a trust-paired peer.
    /// Caller (P3.C-2 server handler) builds the encoded
    /// `FrameProxyResponsePayload` (Found / NotFound / Unavailable) and
    /// pushes it back on the same trust link.
    pub async fn send_frame_proxy_response(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_ufp2_to_peer(
            node_id,
            tentaflow_protocol::mesh::MESH_MSG_FRAME_PROXY_RESPONSE,
            data,
        )
        .await
    }

    pub async fn send_storage_proxy_request(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_ufp2_to_peer(
            node_id,
            tentaflow_protocol::mesh::MESH_MSG_STORAGE_PROXY_REQUEST,
            data,
        )
        .await
    }

    pub async fn send_storage_proxy_response(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_ufp2_to_peer(
            node_id,
            tentaflow_protocol::mesh::MESH_MSG_STORAGE_PROXY_RESPONSE,
            data,
        )
        .await
    }

    pub async fn send_node_leaving(&self) {
        let data = vec![];
        let _ = self
            .broadcast_ufp2_to_trusted(tentaflow_protocol::mesh::MESH_MSG_NODE_LEAVING, &data, None)
            .await;
    }

    pub async fn broadcast_node_info(&self, data: &[u8]) {
        let _ = self
            .broadcast_ufp2_to_trusted(tentaflow_protocol::mesh::MESH_MSG_NODE_INFO, data, None)
            .await;
    }

    pub async fn broadcast_alias_sync(&self, aliases_json: Vec<u8>) {
        let _ = self
            .broadcast_ufp2_to_trusted(
                tentaflow_protocol::mesh::MESH_MSG_ALIAS_SYNC,
                &aliases_json,
                None,
            )
            .await;
    }

    /// Broadcast snapshotu konfiguracji routingu (klastry + czlonkowie) do
    /// zaufanych peerow po mutacji. Payload to JSON `RoutingSyncPayload`.
    pub async fn broadcast_routing_sync(&self, routing_json: Vec<u8>) {
        let _ = self
            .broadcast_ufp2_to_trusted(
                tentaflow_protocol::mesh::MESH_MSG_ROUTING_SYNC,
                &routing_json,
                None,
            )
            .await;
    }

    /// Forward request na peera i czeka na odpowiedz. `request_id` uzyty w
    /// payloadzie dla tracking (format: [u32 id_len][id_bytes][payload]).
    /// Public trust check used by R3b.7 executor mesh dispatch before
    /// any payload write. Bypassing this and relying solely on the
    /// `connections` map would let pre-trust dial registrations forward
    /// requests to untrusted peers.
    pub fn is_trusted(&self, node_id: &str) -> bool {
        self.security.is_trusted(node_id)
    }

    pub async fn forward_request(
        &self,
        target_node_id: &str,
        request_id: &str,
        payload: Vec<u8>,
    ) -> Result<Vec<u8>> {
        // Codex R3b.7 H1: explicit trust gate before any payload write.
        // `connections` map alone is not trust-equivalent — register_connection
        // does not check trust, so we re-verify here.
        if !self.security.is_trusted(target_node_id) {
            return Err(anyhow::anyhow!(
                "mesh forward refused: peer '{}' is not trusted",
                target_node_id
            ));
        }
        let connection = self
            .connections
            .get(target_node_id)
            .ok_or_else(|| anyhow::anyhow!("brak polaczenia z {}", target_node_id))?
            .connection
            .clone();

        let request_id = request_id.to_string();
        let task = async move {
            let (mut send, mut recv) = connection
                .open_bi()
                .await
                .map_err(|e| anyhow::anyhow!("open_bi: {e}"))?;
            send.write_all(&[tentaflow_protocol::mesh::MESH_MSG_FORWARD_REQ])
                .await
                .map_err(|e| anyhow::anyhow!("write disc: {e}"))?;
            let id_bytes = request_id.as_bytes();
            send.write_all(&(id_bytes.len() as u32).to_be_bytes())
                .await
                .map_err(|e| anyhow::anyhow!("write id_len: {e}"))?;
            send.write_all(id_bytes)
                .await
                .map_err(|e| anyhow::anyhow!("write id: {e}"))?;
            send.write_all(&payload)
                .await
                .map_err(|e| anyhow::anyhow!("write payload: {e}"))?;
            send.finish().map_err(|e| anyhow::anyhow!("finish: {e}"))?;

            let response = recv
                .read_to_end(MAX_MSG_BYTES)
                .await
                .map_err(|e| anyhow::anyhow!("read response: {e}"))?;
            Ok::<_, anyhow::Error>(response)
        };

        tokio::time::timeout(Duration::from_secs(600), task)
            .await
            .map_err(|_| anyhow::anyhow!("forward_request timeout (600s)"))?
    }

    pub async fn forward_stream_request(
        &self,
        target_node_id: &str,
        request_id: &str,
        payload: Vec<u8>,
    ) -> Result<std::pin::Pin<Box<dyn futures::Stream<Item = Result<Vec<u8>>> + Send>>> {
        // Codex R3b.7 H1: explicit trust gate (mirror of forward_request).
        if !self.security.is_trusted(target_node_id) {
            return Err(anyhow::anyhow!(
                "mesh stream forward refused: peer '{}' is not trusted",
                target_node_id
            ));
        }
        let connection = self
            .connections
            .get(target_node_id)
            .ok_or_else(|| anyhow::anyhow!("brak polaczenia z {}", target_node_id))?
            .connection
            .clone();

        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|e| anyhow::anyhow!("open_bi: {e}"))?;
        send.write_all(&[tentaflow_protocol::mesh::MESH_MSG_FORWARD_STREAM_REQ])
            .await
            .map_err(|e| anyhow::anyhow!("write disc: {e}"))?;
        let id_bytes = request_id.as_bytes();
        send.write_all(&(id_bytes.len() as u32).to_be_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("write id_len: {e}"))?;
        send.write_all(id_bytes)
            .await
            .map_err(|e| anyhow::anyhow!("write id: {e}"))?;
        send.write_all(&payload)
            .await
            .map_err(|e| anyhow::anyhow!("write payload: {e}"))?;
        send.finish().map_err(|e| anyhow::anyhow!("finish: {e}"))?;

        let stream = async_stream::try_stream! {
            loop {
                let mut len_buf = [0u8; 4];
                if recv.read_exact(&mut len_buf).await.is_err() {
                    break;
                }
                let len = u32::from_be_bytes(len_buf) as usize;
                if len > MAX_MSG_BYTES {
                    Err(anyhow::anyhow!("forward stream frame too large: {}", len))?;
                }
                let mut frame = vec![0u8; len];
                recv.read_exact(&mut frame)
                    .await
                    .map_err(|e| anyhow::anyhow!("read stream frame: {e}"))?;
                yield frame;
            }
        };
        Ok(Box::pin(stream))
    }

    /// Observer side of the live camera relay. Opens a bi-stream to the owner
    /// node, sends a `CameraStreamSubscribePayload`, and returns a stream of raw
    /// frame bytes (each item is one CBOR `CameraStreamFrame` body — the caller
    /// decodes it). Mirrors `forward_stream_request` but with the camera relay
    /// discriminator. Trust-gated like every outbound bi-stream.
    pub async fn camera_stream_request(
        &self,
        owner_node_id: &str,
        camera_id: &str,
        org_id: &str,
    ) -> Result<std::pin::Pin<Box<dyn futures::Stream<Item = Result<Vec<u8>>> + Send>>> {
        if !self.security.is_trusted(owner_node_id) {
            return Err(anyhow::anyhow!(
                "camera stream relay refused: peer '{}' is not trusted",
                owner_node_id
            ));
        }
        let payload = tentaflow_protocol::cbor::encode(
            &tentaflow_protocol::mesh::CameraStreamSubscribePayload {
                camera_id: camera_id.to_string(),
                org_id: org_id.to_string(),
            },
        )
        .map_err(|e| anyhow::anyhow!("encode camera subscribe: {e}"))?;

        let connection = self
            .connections
            .get(owner_node_id)
            .ok_or_else(|| anyhow::anyhow!("brak polaczenia z {}", owner_node_id))?
            .connection
            .clone();

        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|e| anyhow::anyhow!("open_bi: {e}"))?;
        send.write_all(&[tentaflow_protocol::mesh::MESH_MSG_CAMERA_STREAM_SUBSCRIBE])
            .await
            .map_err(|e| anyhow::anyhow!("write disc: {e}"))?;
        // Request id is informational here (no per-request correlation on the
        // relay); use the camera id so owner-side logs are diagnosable.
        let id_bytes = camera_id.as_bytes();
        send.write_all(&(id_bytes.len() as u32).to_be_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("write id_len: {e}"))?;
        send.write_all(id_bytes)
            .await
            .map_err(|e| anyhow::anyhow!("write id: {e}"))?;
        send.write_all(&payload)
            .await
            .map_err(|e| anyhow::anyhow!("write payload: {e}"))?;
        send.finish().map_err(|e| anyhow::anyhow!("finish: {e}"))?;

        let stream = async_stream::try_stream! {
            loop {
                let mut len_buf = [0u8; 4];
                if recv.read_exact(&mut len_buf).await.is_err() {
                    break;
                }
                let len = u32::from_be_bytes(len_buf) as usize;
                if len > MAX_MSG_BYTES {
                    Err(anyhow::anyhow!("camera relay frame too large: {}", len))?;
                }
                let mut frame = vec![0u8; len];
                recv.read_exact(&mut frame)
                    .await
                    .map_err(|e| anyhow::anyhow!("read relay frame: {e}"))?;
                yield frame;
            }
        };
        Ok(Box::pin(stream))
    }

    /// Observer side of the live LiDAR relay. Opens a bi-stream to the owner node,
    /// sends a `LidarStreamSubscribePayload`, and returns a stream of raw frame
    /// bytes (each item is one CBOR `LidarStreamFrame` body — the caller decodes
    /// it). Mirrors `camera_stream_request` with the LiDAR relay discriminator.
    /// Trust-gated like every outbound bi-stream.
    pub async fn lidar_stream_request(
        &self,
        owner_node_id: &str,
        robot_id: &str,
        org_id: &str,
    ) -> Result<std::pin::Pin<Box<dyn futures::Stream<Item = Result<Vec<u8>>> + Send>>> {
        if !self.security.is_trusted(owner_node_id) {
            return Err(anyhow::anyhow!(
                "lidar stream relay refused: peer '{}' is not trusted",
                owner_node_id
            ));
        }
        let payload = tentaflow_protocol::cbor::encode(
            &tentaflow_protocol::mesh::LidarStreamSubscribePayload {
                robot_id: robot_id.to_string(),
                org_id: org_id.to_string(),
            },
        )
        .map_err(|e| anyhow::anyhow!("encode lidar subscribe: {e}"))?;

        let connection = self
            .connections
            .get(owner_node_id)
            .ok_or_else(|| anyhow::anyhow!("brak polaczenia z {}", owner_node_id))?
            .connection
            .clone();

        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .map_err(|e| anyhow::anyhow!("open_bi: {e}"))?;
        send.write_all(&[tentaflow_protocol::mesh::MESH_MSG_LIDAR_STREAM_SUBSCRIBE])
            .await
            .map_err(|e| anyhow::anyhow!("write disc: {e}"))?;
        // Request id is informational here (no per-request correlation on the
        // relay); use the robot id so owner-side logs are diagnosable.
        let id_bytes = robot_id.as_bytes();
        send.write_all(&(id_bytes.len() as u32).to_be_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("write id_len: {e}"))?;
        send.write_all(id_bytes)
            .await
            .map_err(|e| anyhow::anyhow!("write id: {e}"))?;
        send.write_all(&payload)
            .await
            .map_err(|e| anyhow::anyhow!("write payload: {e}"))?;
        send.finish().map_err(|e| anyhow::anyhow!("finish: {e}"))?;

        let stream = async_stream::try_stream! {
            loop {
                let mut len_buf = [0u8; 4];
                if recv.read_exact(&mut len_buf).await.is_err() {
                    break;
                }
                let len = u32::from_be_bytes(len_buf) as usize;
                if len > MAX_MSG_BYTES {
                    Err(anyhow::anyhow!("lidar relay frame too large: {}", len))?;
                }
                let mut frame = vec![0u8; len];
                recv.read_exact(&mut frame)
                    .await
                    .map_err(|e| anyhow::anyhow!("read relay frame: {e}"))?;
                yield frame;
            }
        };
        Ok(Box::pin(stream))
    }

    /// Zwraca snapshot EndpointId wszystkich znanych polaczonych peerow.
    pub async fn connected_peer_ids(&self) -> Vec<String> {
        self.connected_peers().await
    }

    /// Ustawia callback dla incoming forward requestow.
    pub async fn set_forward_handler(&self, handler: ForwardHandler) {
        *self.forward_handler.write().await = Some(handler);
    }

    pub async fn set_forward_stream_handler(&self, handler: ForwardStreamHandler) {
        *self.forward_stream_handler.write().await = Some(handler);
    }

    /// Installs the owner-side handler for live camera relay bi-streams. Carries
    /// a camera subscribe payload and writes frames into a BOUNDED channel so a
    /// slow observer back-pressures the StreamHub drain instead of buffering
    /// without limit (`CameraStreamHandler`).
    pub async fn set_camera_stream_handler(&self, handler: CameraStreamHandler) {
        *self.camera_stream_handler.write().await = Some(handler);
    }

    /// Installs the owner-side handler for live LiDAR relay bi-streams. Same
    /// bounded-channel back-pressure contract as `set_camera_stream_handler`
    /// (`LidarStreamHandler`).
    pub async fn set_lidar_stream_handler(&self, handler: LidarStreamHandler) {
        *self.lidar_stream_handler.write().await = Some(handler);
    }

    /// Pobiera RTT do peera w mikrosekundach. iroh udostepnia `remote_info`
    /// z metrykami RTT; na razie zwracamy None bo API `RemoteInfo` jest
    /// internal i bedzie wpiete po stabilizacji iroh 0.99+.
    pub async fn get_peer_rtt_us(&self, _peer_id: &str) -> Option<u64> {
        None
    }

    /// Wysyla komende typu `MeshCommandType` do peera (sync fire-and-forget).
    /// Zwraca CommandResponse otrzymany od peera po zakonczeniu.
    pub async fn send_command(
        self: &Arc<Self>,
        target_node_id: &str,
        command: tentaflow_protocol::mesh::MeshCommandType,
    ) -> Result<crate::mesh::command_executor::CommandResponse> {
        let command_id = format!(
            "cmd-{}-{}",
            self.node_id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        #[derive(serde::Serialize)]
        struct RequestEnvelope {
            command_id: String,
            sender_node_id: String,
            command: tentaflow_protocol::mesh::MeshCommandType,
        }
        let envelope = RequestEnvelope {
            command_id: command_id.clone(),
            sender_node_id: self.node_id(),
            command,
        };
        let data = crate::mesh::cbor::encode(&envelope)
            .map_err(|e| anyhow::anyhow!("encode command: {e}"))?;
        self.send_command_and_wait_bytes(target_node_id, command_id, data, Duration::from_secs(600))
            .await
            .map(|r| crate::mesh::command_executor::CommandResponse {
                ok: r.ok,
                payload: r.payload,
                error: r.error,
            })
    }

    /// Wysyla komende `MeshCommandType` i czeka na odpowiedz przez `timeout_secs`.
    pub async fn send_command_and_wait(
        &self,
        target_node_id: &str,
        command: tentaflow_protocol::mesh::MeshCommandType,
        timeout_secs: u64,
    ) -> Result<CommandWaitResponse> {
        let command_id = format!(
            "cmd-{}-{}",
            self.node_id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        #[derive(serde::Serialize)]
        struct RequestEnvelope {
            command_id: String,
            sender_node_id: String,
            command: tentaflow_protocol::mesh::MeshCommandType,
        }
        let envelope = RequestEnvelope {
            command_id: command_id.clone(),
            sender_node_id: self.node_id(),
            command,
        };
        let data = crate::mesh::cbor::encode(&envelope)
            .map_err(|e| anyhow::anyhow!("encode command: {e}"))?;
        self.send_command_and_wait_bytes(
            target_node_id,
            command_id,
            data,
            Duration::from_secs(timeout_secs),
        )
        .await
    }

    async fn send_command_and_wait_bytes(
        &self,
        target_node_id: &str,
        command_id: String,
        data: Vec<u8>,
        timeout: Duration,
    ) -> Result<CommandWaitResponse> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.command_waiters.insert(command_id.clone(), tx);

        // If the send fails the waiter would otherwise leak until the
        // explicit removals below in the timeout/drop arms — but those
        // only run after the `?` returns. Clear the waiter on send error.
        if let Err(e) = self
            .send_ufp2_to_peer(
                target_node_id,
                tentaflow_protocol::mesh::MESH_MSG_COMMAND,
                &data,
            )
            .await
        {
            self.command_waiters.remove(&command_id);
            return Err(e);
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(_)) => {
                self.command_waiters.remove(&command_id);
                anyhow::bail!("command waiter dropped before response")
            }
            Err(_) => {
                self.command_waiters.remove(&command_id);
                anyhow::bail!("command {} timed out", command_id)
            }
        }
    }

    /// Rozwiazuje oczekujacego waiter gdy nadejdzie CommandResponse z typed payloadem.
    pub async fn resolve_command_waiter(
        &self,
        command_id: &str,
        ok: bool,
        payload: tentaflow_protocol::mesh::MeshCommandResponsePayload,
        error: Option<String>,
    ) -> bool {
        if let Some((_, tx)) = self.command_waiters.remove(command_id) {
            let _ = tx.send(CommandWaitResponse {
                command_id: command_id.to_string(),
                ok,
                payload,
                error,
            });
            true
        } else {
            false
        }
    }

    /// Obsluzyc komende otrzymana od peera — dekoduje envelope CBOR, deleguje do
    /// `MeshCommandExecutor` (z weryfikacja trust), serializuje wynik i odsyla
    /// peerowi przez `MESH_MSG_COMMAND_RESPONSE`. Wire format jest symetryczny
    /// z `send_command`/`send_command_and_wait`.
    pub async fn handle_command_received(&self, from_node_id: &str, data: &[u8]) {
        #[derive(serde::Deserialize)]
        struct RequestEnvelope {
            command_id: String,
            #[serde(default)]
            sender_node_id: String,
            command: tentaflow_protocol::mesh::MeshCommandType,
        }

        #[derive(serde::Serialize)]
        struct ResponseEnvelope<'a> {
            command_id: &'a str,
            ok: bool,
            payload: &'a tentaflow_protocol::mesh::MeshCommandResponsePayload,
            #[serde(skip_serializing_if = "Option::is_none")]
            error: Option<&'a str>,
        }

        let envelope: RequestEnvelope = match crate::mesh::cbor::decode(data) {
            Ok(e) => e,
            Err(e) => {
                warn!(from = %from_node_id, "Niepoprawny envelope MeshCommand: {}", e);
                return;
            }
        };

        // Bierzemy tozsamosc nadawcy z transportu (iroh EndpointId), nie z pola
        // w envelope — pole serwuje tylko jako audit hint, gdyby ktos podszywal
        // sie w envelope to i tak `is_trusted` sprawdzi prawdziwy `from_node_id`.
        let _ = envelope.sender_node_id;

        let executor = match self.command_executor.read().await.clone() {
            Some(e) => e,
            None => {
                warn!(
                    from = %from_node_id,
                    cmd = %envelope.command_id,
                    "MeshCommand odebrana zanim wstrzyknieto executor — odsylam blad"
                );
                let resp = ResponseEnvelope {
                    command_id: &envelope.command_id,
                    ok: false,
                    payload: &tentaflow_protocol::mesh::MeshCommandResponsePayload::Empty,
                    error: Some("command executor not configured"),
                };
                if let Ok(bytes) = crate::mesh::cbor::encode(&resp) {
                    let _ = self
                        .send_ufp2_to_peer(
                            from_node_id,
                            tentaflow_protocol::mesh::MESH_MSG_COMMAND_RESPONSE,
                            &bytes,
                        )
                        .await;
                }
                return;
            }
        };

        let response = executor.execute(from_node_id, envelope.command).await;
        let resp_envelope = ResponseEnvelope {
            command_id: &envelope.command_id,
            ok: response.ok,
            payload: &response.payload,
            error: response.error.as_deref(),
        };
        match crate::mesh::cbor::encode(&resp_envelope) {
            Ok(bytes) => {
                if let Err(e) = self
                    .send_ufp2_to_peer(
                        from_node_id,
                        tentaflow_protocol::mesh::MESH_MSG_COMMAND_RESPONSE,
                        &bytes,
                    )
                    .await
                {
                    warn!(
                        to = %from_node_id,
                        cmd = %envelope.command_id,
                        "Nie udalo sie odeslac MeshCommandResponse: {}", e
                    );
                }
            }
            Err(e) => {
                warn!(
                    cmd = %envelope.command_id,
                    "Nie udalo sie zserializowac MeshCommandResponse: {}", e
                );
            }
        }
    }

    /// Obsluzyc odpowiedz na komende otrzymana od peera.
    pub async fn handle_command_response_received(&self, _from_node_id: &str, data: &[u8]) {
        // Wire format: CBOR envelope { command_id, ok, payload, error? } gdzie
        // `payload` to serde-zserializowany MeshCommandResponsePayload.
        #[derive(serde::Deserialize)]
        struct ResponseEnvelope {
            command_id: String,
            ok: bool,
            payload: tentaflow_protocol::mesh::MeshCommandResponsePayload,
            #[serde(default)]
            error: Option<String>,
        }
        if let Ok(env) = crate::mesh::cbor::decode::<ResponseEnvelope>(data) {
            if !env.command_id.is_empty() {
                self.resolve_command_waiter(&env.command_id, env.ok, env.payload, env.error)
                    .await;
            }
        }
    }
}

/// Kopia referencji uzywana w spawned tasks — bez `Arc<Self>` aby unikac cyklu.
#[derive(Clone)]
struct IrohMeshManagerRef {
    connections: Arc<DashMap<String, ActiveConnection>>,
    event_tx: broadcast::Sender<IrohMeshEvent>,
    security: Arc<MeshSecurity>,
    forward_handler: Arc<AsyncRwLock<Option<ForwardHandler>>>,
    forward_stream_handler: Arc<AsyncRwLock<Option<ForwardStreamHandler>>>,
    camera_stream_handler: Arc<AsyncRwLock<Option<CameraStreamHandler>>>,
    lidar_stream_handler: Arc<AsyncRwLock<Option<LidarStreamHandler>>>,
}

impl IrohMeshManagerRef {
    async fn handle_mesh_bi(
        &self,
        remote_hex: String,
        mut send: iroh::endpoint::SendStream,
        mut recv: iroh::endpoint::RecvStream,
    ) -> Result<(), IrohStreamError> {
        let mut disc = [0u8; 1];
        recv.read_exact(&mut disc)
            .await
            .map_err(|e| IrohStreamError::Io(format!("{e}")))?;
        if !self.security.is_trusted(&remote_hex) {
            tracing::debug!(
                target: "mesh::gate",
                peer = %remote_hex,
                frame_type = format!("0x{:02X}", disc[0]),
                "iroh_mesh: rejected bidi frame from untrusted peer"
            );
            return Ok(());
        }

        let payload = read_forward_payload(&mut recv).await?;
        match disc[0] {
            x if x == tentaflow_protocol::mesh::MESH_MSG_FORWARD_REQ => {
                let handler = self.forward_handler.read().await.clone();
                let Some(handler) = handler else {
                    return Ok(());
                };
                let response = handler(payload).await;
                send.write_all(&response)
                    .await
                    .map_err(|e| IrohStreamError::Io(format!("{e}")))?;
                send.finish()
                    .map_err(|e| IrohStreamError::Io(format!("{e}")))?;
            }
            x if x == tentaflow_protocol::mesh::MESH_MSG_FORWARD_STREAM_REQ => {
                let handler = self.forward_stream_handler.read().await.clone();
                let Some(handler) = handler else {
                    return Ok(());
                };
                let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
                let task = tokio::spawn(handler(payload, tx));
                while let Some(frame) = rx.recv().await {
                    if frame.len() > MAX_MSG_BYTES {
                        return Err(IrohStreamError::FrameTooLarge(frame.len()));
                    }
                    send.write_all(&(frame.len() as u32).to_be_bytes())
                        .await
                        .map_err(|e| IrohStreamError::Io(format!("{e}")))?;
                    send.write_all(&frame)
                        .await
                        .map_err(|e| IrohStreamError::Io(format!("{e}")))?;
                }
                let _ = task.await;
                send.finish()
                    .map_err(|e| IrohStreamError::Io(format!("{e}")))?;
            }
            x if x == tentaflow_protocol::mesh::MESH_MSG_CAMERA_STREAM_SUBSCRIBE => {
                let handler = self.camera_stream_handler.read().await.clone();
                let Some(handler) = handler else {
                    return Ok(());
                };
                // Owner side: the handler subscribes to the local StreamHub and
                // emits already-CBOR-encoded `CameraStreamFrame` blobs on a
                // BOUNDED channel. A slow observer back-pressures the handler's
                // broadcast drain (which then drops the subscription rather than
                // buffering forever) instead of growing this process's memory.
                // QUIC flow-control on `write_all` provides the on-wire bound.
                //
                // The handler task is wrapped in an abort-on-drop guard: any
                // early return below (write error, frame too large, observer
                // close) drops the guard, aborts the task, and drops `rx` — so
                // the handler can never sit in `recv().await` holding a dead
                // StreamHub subscription after the QUIC write half is gone.
                let (tx, mut rx) = mpsc::channel::<Vec<u8>>(CAMERA_RELAY_CHANNEL_CAPACITY);
                let task = AbortOnDrop(tokio::spawn(handler(payload, tx)));
                while let Some(frame) = rx.recv().await {
                    if frame.len() > MAX_MSG_BYTES {
                        return Err(IrohStreamError::FrameTooLarge(frame.len()));
                    }
                    send.write_all(&(frame.len() as u32).to_be_bytes())
                        .await
                        .map_err(|e| IrohStreamError::Io(format!("{e}")))?;
                    send.write_all(&frame)
                        .await
                        .map_err(|e| IrohStreamError::Io(format!("{e}")))?;
                }
                // Channel closed: the handler finished (source closed / observer
                // gone). The guard's Drop aborts the (already-finished) task.
                drop(task);
                send.finish()
                    .map_err(|e| IrohStreamError::Io(format!("{e}")))?;
            }
            x if x == tentaflow_protocol::mesh::MESH_MSG_LIDAR_STREAM_SUBSCRIBE => {
                let handler = self.lidar_stream_handler.read().await.clone();
                let Some(handler) = handler else {
                    return Ok(());
                };
                // Owner side: same bounded-channel / abort-on-drop contract as the
                // camera relay arm above. The handler subscribes to the local
                // StreamHub and emits already-CBOR-encoded `LidarStreamFrame` blobs
                // on a BOUNDED channel; a slow observer back-pressures the handler's
                // broadcast drain (which drops the subscription rather than
                // buffering) and QUIC flow-control on `write_all` bounds the wire.
                let (tx, mut rx) = mpsc::channel::<Vec<u8>>(CAMERA_RELAY_CHANNEL_CAPACITY);
                let task = AbortOnDrop(tokio::spawn(handler(payload, tx)));
                while let Some(frame) = rx.recv().await {
                    if frame.len() > MAX_MSG_BYTES {
                        return Err(IrohStreamError::FrameTooLarge(frame.len()));
                    }
                    send.write_all(&(frame.len() as u32).to_be_bytes())
                        .await
                        .map_err(|e| IrohStreamError::Io(format!("{e}")))?;
                    send.write_all(&frame)
                        .await
                        .map_err(|e| IrohStreamError::Io(format!("{e}")))?;
                }
                drop(task);
                send.finish()
                    .map_err(|e| IrohStreamError::Io(format!("{e}")))?;
            }
            other => {
                warn!(
                    peer = %remote_hex,
                    "iroh_mesh: nieznany bidi discriminant 0x{:02X}",
                    other
                );
            }
        }
        Ok(())
    }

    async fn handle_mesh_connection(
        &self,
        remote_hex: String,
        connection: Connection,
        connection_id: u64,
    ) {
        let close_reason: Option<String>;
        loop {
            tokio::select! {
                uni = connection.accept_uni() => {
                    let recv = match uni {
                        Ok(r) => r,
                        Err(e) => {
                            close_reason = Some(format!("{e}"));
                            break;
                        }
                    };
                    let me = self.clone();
                    let rhex = remote_hex.clone();
                    tokio::spawn(async move {
                        if let Err(e) = me.handle_mesh_uni(rhex, recv).await {
                            debug!("mesh uni handler blad: {}", e);
                        }
                    });
                }
                bi = connection.accept_bi() => {
                    let (send, recv) = match bi {
                        Ok(pair) => pair,
                        Err(e) => {
                            close_reason = Some(format!("{e}"));
                            break;
                        }
                    };
                    let me = self.clone();
                    let rhex = remote_hex.clone();
                    tokio::spawn(async move {
                        if let Err(e) = me.handle_mesh_bi(rhex, send, recv).await {
                            debug!("mesh bi handler blad: {}", e);
                        }
                    });
                }
            }
        }
        // Connection wymarl. Mapowanie usuwamy WYLACZNIE jesli nadal
        // wskazuje na nasz connection_id — gdy nowsze polaczenie
        // (tie-break / reconnect) juz przebilo nasz wpis, zostawiamy
        // jego stan w spokoju. Nie wysylamy wtedy PeerDisconnected, zeby
        // pipeline nie zerowal heartbeat livenessu zywego polaczenia.
        let was_current = match self.connections.get(&remote_hex) {
            Some(active) if active.id == connection_id => {
                drop(active);
                self.connections.remove(&remote_hex);
                true
            }
            _ => false,
        };
        let reason = close_reason.as_deref().unwrap_or("stream closed");
        if was_current {
            // DIAGNOSTYKA (mesh::pathdiag) — best-effort snapshot stanu sciezki
            // TUZ przed smiercia polaczenia. `connection` nadal w scope; po
            // bledzie accept_* paths() zwraca ostatni znany stan (akceptowalne).
            // Do usuniecia po debugu niestabilnosci QUIC.
            let close_snap = connection_snapshot_from_connection(&connection);
            let close_paths = close_snap
                .paths
                .iter()
                .map(|p| format!("{}@{} selected={}", p.transport, p.address, p.selected))
                .collect::<Vec<_>>()
                .join(" | ");
            info!(
                target: "mesh::pathdiag",
                peer = %remote_hex,
                reason,
                last_transport = %close_snap.transport,
                last_scope = ?close_snap.scope,
                paths = %close_paths,
                "iroh_mesh path diag: connection close snapshot"
            );
            info!(peer = %remote_hex, reason, "iroh_mesh: polaczenie zamkniete");
            if reason.contains("tie-break-loser") {
                let connections = Arc::clone(&self.connections);
                let event_tx = self.event_tx.clone();
                let peer_hex = remote_hex.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    if !connections.contains_key(&peer_hex) {
                        let _ =
                            event_tx.send(IrohMeshEvent::PeerDisconnected { node_id: peer_hex });
                    }
                });
            } else {
                let _ = self.event_tx.send(IrohMeshEvent::PeerDisconnected {
                    node_id: remote_hex,
                });
            }
        } else {
            debug!(
                peer = %remote_hex,
                reason,
                connection_id,
                "iroh_mesh: stary handler zakonczony — aktywne jest nowsze polaczenie, PeerDisconnected pominiety"
            );
        }
    }

    async fn handle_mesh_uni(
        &self,
        remote_hex: String,
        mut recv: iroh::endpoint::RecvStream,
    ) -> Result<(), IrohStreamError> {
        let mut first = [0u8; 1];
        recv.read_exact(&mut first)
            .await
            .map_err(|e| IrohStreamError::Io(format!("{e}")))?;
        // iroh RecvStream.read_to_end bierze limit bajtow, zwraca Vec<u8>.
        let tail = recv
            .read_to_end(MAX_MSG_BYTES)
            .await
            .map_err(|e| IrohStreamError::Io(format!("{e}")))?;
        if tail.len() > MAX_MSG_BYTES {
            return Err(IrohStreamError::FrameTooLarge(tail.len()));
        }

        // UFP/2 receive: every unicast mesh frame is a signed UFP/2
        // envelope. `classify_inbound` reassembles the bytes, decodes
        // CBOR, runs the structural validator, verifies the Ed25519
        // signature, and binds envelope.source.id / destination.id to the
        // transport peer / local node so a trusted peer cannot relay or
        // replay another node's envelope. The legacy discriminator is
        // recovered from envelope.kind so the existing event dispatch
        // below routes by `frame_type` unchanged.
        let local_pubkey = self.security.verifying_key_bytes();
        let peer_pubkey_opt = parse_iroh_node_id_to_pubkey(&remote_hex);
        let (frame_type, payload) = if let Some(peer_pubkey) = peer_pubkey_opt {
            match crate::mesh::ufp2::classify_inbound(first[0], tail, peer_pubkey, local_pubkey) {
                Ok(crate::mesh::ufp2::InboundMeshFrame::Ufp2(decoded)) => {
                    (decoded.legacy_discriminator, decoded.body)
                }
                Err(e) => {
                    tracing::warn!(
                        target: "mesh::ufp2",
                        peer = %remote_hex,
                        error = %e,
                        "handle_mesh_uni: UFP/2 dispatch rejected incoming frame"
                    );
                    return Ok(());
                }
            }
        } else {
            tracing::warn!(
                target: "mesh::ufp2",
                peer = %remote_hex,
                "handle_mesh_uni: cannot parse iroh node id as Ed25519 pubkey, dropping frame"
            );
            return Ok(());
        };

        // Pre-trust whitelist: untrusted peers may only send pairing handshake
        // frames. Every other mesh frame is dropped before any application
        // state (peer_store, registry, command executor, ...) is touched.
        let trusted_now = self.security.is_trusted(&remote_hex);
        tracing::debug!(
            target: "mesh::gate",
            remote_hex = %remote_hex,
            frame_type = format!("0x{:02X}", frame_type),
            is_trusted = trusted_now,
            "frame received, gate check"
        );
        if !crate::mesh::frame_policy::is_pre_trust_frame(frame_type) && !trusted_now {
            tracing::debug!(
                target: "mesh::gate",
                peer = %remote_hex,
                frame_type = format!("0x{:02X}", frame_type),
                "iroh_mesh: rejected mesh frame from untrusted peer"
            );
            let details = format!(
                "{{\"peer\":\"{}\",\"frame_type\":\"0x{:02X}\"}}",
                remote_hex, frame_type
            );
            let _ = crate::db::repository::log_audit(
                &self.security.db,
                None,
                None,
                "mesh.frame_rejected",
                None,
                Some(&details),
                None,
                Some(&remote_hex),
            );
            return Ok(());
        }

        use tentaflow_protocol::mesh::*;
        let event = match frame_type {
            x if x == MESH_MSG_HEARTBEAT => IrohMeshEvent::HeartbeatReceived {
                node_id: remote_hex,
                heartbeat: payload,
            },
            x if x == MESH_MSG_NODE_INFO => IrohMeshEvent::NodeInfoReceived {
                node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_HELLO => IrohMeshEvent::HelloReceived {
                node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_TOPOLOGY_ANNOUNCE => IrohMeshEvent::TopologyAnnounceReceived {
                from_node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_KNOWN_PEERS => IrohMeshEvent::KnownPeersReceived {
                from_node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_PAIRING_REQUEST => IrohMeshEvent::PairingRequestReceived {
                peer_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_PAIRING_CONFIRM => IrohMeshEvent::PairingConfirmReceived {
                peer_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_PAIRING_REJECT => IrohMeshEvent::PairingRejectReceived {
                peer_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_ALIAS_SYNC => IrohMeshEvent::AliasSyncReceived {
                from_node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_ROUTING_SYNC => IrohMeshEvent::RoutingSyncReceived {
                from_node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_MODEL_LIST => IrohMeshEvent::ModelListUpdate {
                node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_TRUSTED_KEYS_SYNC => {
                let parsed = crate::mesh::cbor::decode::<
                    tentaflow_protocol::mesh::TrustedKeysSyncPayload,
                >(&payload);
                match parsed {
                    Ok(p) => IrohMeshEvent::TrustedKeysSyncReceived {
                        node_id: remote_hex,
                        keys: p
                            .keys
                            .into_iter()
                            .map(|e| (e.node_id, e.public_key_hex, e.approved_at))
                            .collect(),
                    },
                    Err(e) => {
                        warn!(peer = %remote_hex, "iroh_mesh: nie udalo sie zdekodowac TrustedKeysSync: {}", e);
                        return Ok(());
                    }
                }
            }
            x if x == MESH_MSG_HMAC_KEYS_SYNC => {
                let parsed = crate::mesh::cbor::decode::<
                    tentaflow_protocol::mesh::HmacKeysSyncPayload,
                >(&payload);
                match parsed {
                    Ok(p) => IrohMeshEvent::HmacKeysSyncReceived {
                        node_id: remote_hex,
                        payload: p,
                    },
                    Err(e) => {
                        warn!(
                            peer = %remote_hex,
                            "iroh_mesh: failed to decode HmacKeysSync: {}",
                            e
                        );
                        return Ok(());
                    }
                }
            }
            x if x == MESH_MSG_FRAME_PROXY_REQUEST => {
                let parsed = crate::mesh::cbor::decode::<
                    tentaflow_protocol::mesh::FrameProxyRequestPayload,
                >(&payload);
                match parsed {
                    Ok(p) => IrohMeshEvent::FrameProxyRequestReceived {
                        from_node_id: remote_hex,
                        payload: p,
                    },
                    Err(e) => {
                        warn!(
                            peer = %remote_hex,
                            "iroh_mesh: failed to decode FrameProxyRequest: {}",
                            e
                        );
                        return Ok(());
                    }
                }
            }
            x if x == MESH_MSG_FRAME_PROXY_RESPONSE => {
                let parsed = crate::mesh::cbor::decode::<
                    tentaflow_protocol::mesh::FrameProxyResponsePayload,
                >(&payload);
                match parsed {
                    Ok(p) => IrohMeshEvent::FrameProxyResponseReceived {
                        from_node_id: remote_hex,
                        payload: p,
                    },
                    Err(e) => {
                        warn!(
                            peer = %remote_hex,
                            "iroh_mesh: failed to decode FrameProxyResponse: {}",
                            e
                        );
                        return Ok(());
                    }
                }
            }
            x if x == MESH_MSG_STORAGE_PROXY_REQUEST => {
                let parsed = crate::mesh::cbor::decode::<
                    tentaflow_protocol::mesh::StorageProxyRequestPayload,
                >(&payload);
                match parsed {
                    Ok(p) => IrohMeshEvent::StorageProxyRequestReceived {
                        from_node_id: remote_hex,
                        payload: p,
                    },
                    Err(e) => {
                        warn!(
                            peer = %remote_hex,
                            "iroh_mesh: failed to decode StorageProxyRequest: {}",
                            e
                        );
                        return Ok(());
                    }
                }
            }
            x if x == MESH_MSG_STORAGE_PROXY_RESPONSE => {
                let parsed = crate::mesh::cbor::decode::<
                    tentaflow_protocol::mesh::StorageProxyResponsePayload,
                >(&payload);
                match parsed {
                    Ok(p) => IrohMeshEvent::StorageProxyResponseReceived {
                        from_node_id: remote_hex,
                        payload: p,
                    },
                    Err(e) => {
                        warn!(
                            peer = %remote_hex,
                            "iroh_mesh: failed to decode StorageProxyResponse: {}",
                            e
                        );
                        return Ok(());
                    }
                }
            }
            x if x == MESH_MSG_TRUST_REVOKED => {
                let revoked = match crate::mesh::cbor::decode::<
                    tentaflow_protocol::mesh::TrustRevokedPayload,
                >(&payload)
                {
                    Ok(p) => p.revoked_node_id,
                    Err(e) => {
                        warn!(peer = %remote_hex, "iroh_mesh: failed to decode TrustRevoked CBOR: {}", e);
                        return Ok(());
                    }
                };
                IrohMeshEvent::TrustRevokedReceived {
                    node_id: remote_hex,
                    revoked_node_id: revoked,
                }
            }
            x if x == MESH_MSG_NODE_LEAVING => IrohMeshEvent::NodeLeavingReceived {
                node_id: remote_hex,
            },
            x if x == MESH_MSG_COMMAND => IrohMeshEvent::MeshCommandReceived {
                from_node_id: remote_hex,
                command: payload,
            },
            x if x == MESH_MSG_COMMAND_RESPONSE => IrohMeshEvent::MeshCommandResponseReceived {
                from_node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_DEPLOY_PROGRESS => IrohMeshEvent::MeshDeployProgressReceived {
                from_node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_LOG_CHUNK => IrohMeshEvent::MeshLogChunkReceived {
                from_node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_SERVICES_GET => IrohMeshEvent::ServicesGetReceived {
                from_node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_SERVICES_GET_RESPONSE => {
                IrohMeshEvent::ServicesGetResponseReceived {
                    from_node_id: remote_hex,
                    data: payload,
                }
            }
            x if x == MESH_MSG_SERVICES_ANNOUNCE => IrohMeshEvent::ServicesAnnounceReceived {
                from_node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_SERVICES_UPDATE => IrohMeshEvent::ServicesUpdateReceived {
                from_node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_ROBOTS_ANNOUNCE => IrohMeshEvent::RobotsAnnounceReceived {
                from_node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_ROBOTS_GET => IrohMeshEvent::RobotsGetReceived {
                from_node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_ROBOTS_GET_RESPONSE => IrohMeshEvent::RobotsGetResponseReceived {
                from_node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_ROBOTS_UPDATE => IrohMeshEvent::RobotsUpdateReceived {
                from_node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_SYNC_PUSH => IrohMeshEvent::SyncPushReceived {
                from_node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_SYNC_ACK => IrohMeshEvent::SyncAckReceived {
                from_node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_SYNC_PULL => IrohMeshEvent::SyncPullReceived {
                from_node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_SYNC_PULL_RESPONSE => IrohMeshEvent::SyncPullResponseReceived {
                from_node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_SYNC_SNAPSHOT_PULL => IrohMeshEvent::SyncSnapshotPullReceived {
                from_node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_SYNC_SNAPSHOT_RESPONSE => {
                IrohMeshEvent::SyncSnapshotResponseReceived {
                    from_node_id: remote_hex,
                    data: payload,
                }
            }
            other => {
                warn!(
                    peer = %remote_hex,
                    "iroh_mesh: nieznany discriminant 0x{:02X}, payload {} bajtow",
                    other,
                    payload.len()
                );
                return Ok(());
            }
        };

        let _ = self.event_tx.send(event);
        Ok(())
    }
}

async fn read_forward_payload(
    recv: &mut iroh::endpoint::RecvStream,
) -> Result<Vec<u8>, IrohStreamError> {
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| IrohStreamError::Io(format!("{e}")))?;
    let id_len = u32::from_be_bytes(len_buf) as usize;
    if id_len > 4096 {
        return Err(IrohStreamError::FrameTooLarge(id_len));
    }
    let mut id_buf = vec![0u8; id_len];
    recv.read_exact(&mut id_buf)
        .await
        .map_err(|e| IrohStreamError::Io(format!("{e}")))?;
    let payload = recv
        .read_to_end(MAX_MSG_BYTES)
        .await
        .map_err(|e| IrohStreamError::Io(format!("{e}")))?;
    if payload.len() > MAX_MSG_BYTES {
        return Err(IrohStreamError::FrameTooLarge(payload.len()));
    }
    Ok(payload)
}

/// Czyta dokładnie `buf.len()` bajtów odpowiedzi ze streamu z watchdogiem STALL:
/// każdy `read` ma świeży limit bezczynności i resetuje go po napłynięciu choć
/// jednego bajtu. Timeout = ZERO nowych bajtów przez okno (a nie „za wolno").
async fn read_reply_stall(
    recv: &mut iroh::endpoint::RecvStream,
    buf: &mut [u8],
    stall: std::time::Duration,
) -> Result<()> {
    let mut got = 0usize;
    while got < buf.len() {
        match tokio::time::timeout(stall, recv.read(&mut buf[got..])).await {
            Ok(Ok(Some(0))) | Ok(Ok(None)) => {
                anyhow::bail!(
                    "strumień odpowiedzi zamknięty przedwcześnie ({}/{} B)",
                    got,
                    buf.len()
                )
            }
            Ok(Ok(Some(k))) => got += k,
            Ok(Err(e)) => anyhow::bail!("read reply: {e}"),
            Err(_) => anyhow::bail!(
                "brak nowych bajtów odpowiedzi przez {}s — peer utknął",
                stall.as_secs()
            ),
        }
    }
    Ok(())
}

/// Funkcja pomocnicza wywolywana przez accept loop przy pairing ALPN. Separacja
/// od manager-a ulatwia testowanie.
async fn handler_accept_connection(
    handler: &PairingHandler,
    connection: Connection,
) -> Result<Option<PairingContactHints>> {
    let outcome = handler
        .accept_with_outcome(connection)
        .await
        .map_err(|e| anyhow::anyhow!("pairing accept: {e:?}"))?;
    Ok(outcome)
}

/// Parse an iroh node id (hex-encoded 32-byte Ed25519 public key) into the
/// raw byte form required by `NodeAddress::node` and UFP/2 signature scope.
/// Returns `None` on any decoding failure so callers can log + skip
/// without falling over.
fn parse_iroh_node_id_to_pubkey(node_id_hex: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(node_id_hex).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Some(out)
}

fn build_secret_key_from_security(security: &MeshSecurity) -> Result<iroh::SecretKey> {
    // MeshSecurity trzyma signing_key Ed25519; iroh uzywa wlasnego
    // wrapera. Ed25519 secret key 32B wystarcza do obu.
    // Extract bytes via public API — aktualne MeshSecurity nie eksportuje
    // prywatnego klucza, wiec na razie wczytujemy z DB przez setting.
    let db = &security.db;
    let stored = crate::db::repository::get_setting(db, "node_private_key")
        .context("read node_private_key")?
        .ok_or_else(|| anyhow::anyhow!("brak node_private_key w settings"))?;
    let hex_str = security
        .settings_cipher_ref()
        .decrypt(&stored)
        .context("decrypt node_private_key")?;
    let bytes = hex::decode(&hex_str).context("hex decode node_private_key")?;
    let key_bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("klucz prywatny 32 bajty"))?;
    Ok(iroh::SecretKey::from_bytes(&key_bytes))
}

fn parse_endpoint_id(hex_str: &str) -> Result<EndpointId> {
    let bytes = hex::decode(hex_str).context("hex decode node_id")?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("node_id musi byc 32 bajtami"))?;
    EndpointId::from_bytes(&arr).map_err(|e| anyhow::anyhow!("EndpointId: {e}"))
}

fn endpoint_addr_from_target(
    node_id_hex: &str,
    addr: Option<std::net::SocketAddr>,
) -> Result<EndpointAddr> {
    let endpoint_id = parse_endpoint_id(node_id_hex)?;
    let endpoint_addr = EndpointAddr::new(endpoint_id);
    Ok(match addr {
        Some(addr) if addr.port() != 0 && !addr.ip().is_unspecified() => {
            endpoint_addr.with_ip_addr(addr)
        }
        _ => endpoint_addr,
    })
}

fn connection_snapshot_from_connection(connection: &Connection) -> ConnectionSnapshot {
    let mut relay_url = None;
    let mut selected_transport = String::from("unknown");
    let mut selected_scope = None;
    let mut selected_address = None;
    let paths: Vec<ConnectionPathSnapshot> = connection
        .paths()
        .into_iter()
        .map(|path| {
            let transport = transport_kind(path.remote_addr());
            let address = transport_addr_label(path.remote_addr());
            if path.is_selected() {
                selected_transport = transport.clone();
                selected_scope = transport_scope(path.remote_addr());
                selected_address = Some(address.clone());
                if let TransportAddr::Relay(url) = path.remote_addr() {
                    relay_url = Some(url.to_string());
                }
            } else if relay_url.is_none() {
                if let TransportAddr::Relay(url) = path.remote_addr() {
                    relay_url = Some(url.to_string());
                }
            }
            ConnectionPathSnapshot {
                transport,
                address,
                selected: path.is_selected(),
            }
        })
        .collect();

    ConnectionSnapshot {
        transport: selected_transport,
        scope: selected_scope,
        address: selected_address,
        relay_url,
        paths,
    }
}

fn transport_kind(addr: &TransportAddr) -> String {
    if addr.is_relay() {
        String::from("relay")
    } else if addr.is_ip() {
        String::from("p2p")
    } else if addr.is_custom() {
        String::from("custom")
    } else {
        String::from("unknown")
    }
}

fn transport_scope(addr: &TransportAddr) -> Option<String> {
    match addr {
        TransportAddr::Ip(addr) => Some(if is_private_socket_addr(addr) {
            String::from("lan")
        } else {
            String::from("wan")
        }),
        TransportAddr::Relay(_) => Some(String::from("wan")),
        TransportAddr::Custom(_) => None,
        _ => None,
    }
}

fn transport_addr_label(addr: &TransportAddr) -> String {
    match addr {
        TransportAddr::Ip(addr) => addr.to_string(),
        TransportAddr::Relay(url) => url.to_string(),
        TransportAddr::Custom(addr) => addr.to_string(),
        _ => addr.to_string(),
    }
}

fn is_transient_incoming_finalize_error(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| cause.to_string().contains("finalize connection"))
}

fn is_private_socket_addr(addr: &std::net::SocketAddr) -> bool {
    match addr.ip() {
        std::net::IpAddr::V4(ip) => {
            ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_broadcast()
        }
        std::net::IpAddr::V6(ip) => {
            ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        endpoint_addr_from_target, is_private_socket_addr, transport_kind, transport_scope,
    };
    use iroh::TransportAddr;

    #[test]
    fn endpoint_addr_uses_manual_ip_when_provided() {
        let addr = endpoint_addr_from_target(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            Some("192.168.1.10:7777".parse().unwrap()),
        )
        .unwrap();
        let ips: Vec<_> = addr.ip_addrs().copied().collect();
        assert_eq!(
            ips,
            vec!["192.168.1.10:7777".parse::<std::net::SocketAddr>().unwrap()]
        );
    }

    #[test]
    fn endpoint_addr_ignores_unspecified_manual_addr() {
        let addr = endpoint_addr_from_target(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            Some("0.0.0.0:0".parse().unwrap()),
        )
        .unwrap();
        assert!(addr.ip_addrs().next().is_none());
    }

    #[test]
    fn transport_snapshot_helpers_classify_ip_scope() {
        let lan = TransportAddr::Ip("192.168.1.10:7777".parse().unwrap());
        let wan = TransportAddr::Ip("8.8.8.8:7777".parse().unwrap());
        let relay = TransportAddr::Relay("https://relay.example./".parse().unwrap());

        assert_eq!(transport_kind(&lan), "p2p");
        assert_eq!(transport_scope(&lan).as_deref(), Some("lan"));
        assert_eq!(transport_scope(&wan).as_deref(), Some("wan"));
        assert_eq!(transport_kind(&relay), "relay");
        assert_eq!(transport_scope(&relay).as_deref(), Some("wan"));
    }

    #[test]
    fn private_socket_addr_detects_ipv4_and_ipv6() {
        assert!(is_private_socket_addr(&"10.0.0.7:9000".parse().unwrap()));
        assert!(is_private_socket_addr(&"[fd00::1]:9000".parse().unwrap()));
        assert!(!is_private_socket_addr(&"1.1.1.1:9000".parse().unwrap()));
    }
}

// =============================================================================
// Testy tie-break dla `register_connection`.
//
// Testuja bezposrednio logike tie-break'u. Wymagaja prawdziwych obiektow
// `iroh::endpoint::Connection` — zero mockow. Setup:
//   1. Dwa prawdziwe `IrohEndpoint` bind'ed na loopback.
//   2. Dwa rownoczesne connect/accept daja cztery fizyczne `Connection`
//      (outgoing + incoming z perspektywy kazdej strony, ale na dwoch
//      oddzielnych fizycznych linkach QUIC).
//   3. `IrohMeshManager` z podmienionym `local_node_id` wymusza pozadana
//      relacje leksykograficzna i pozwala testowac kazdy branch tie-break'a.
// =============================================================================
#[cfg(test)]
mod tie_break_tests {
    use super::*;
    use crate::crypto::SettingsCipher;
    use crate::mesh::security::MeshSecurity;
    use iroh::endpoint::Connection;
    use std::time::Duration;

    /// In-memory DbPool z minimalnymi tabelami ktorych wymaga `MeshSecurity::new`.
    fn setup_test_db() -> crate::db::DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS trusted_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                node_id TEXT NOT NULL UNIQUE,
                public_key TEXT NOT NULL,
                hostname TEXT DEFAULT '',
                approved_by TEXT DEFAULT '',
                approved_at TEXT NOT NULL DEFAULT (datetime('now')),
                is_active INTEGER NOT NULL DEFAULT 1,
                last_addresses TEXT NOT NULL DEFAULT ''
            );
            CREATE TABLE IF NOT EXISTS pending_pairings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                remote_node_id TEXT NOT NULL,
                pin_code TEXT NOT NULL,
                direction TEXT NOT NULL CHECK(direction IN ('outgoing','incoming')),
                expires_at TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS revoked_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                node_id TEXT NOT NULL UNIQUE,
                revoked_by TEXT,
                revoked_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .expect("create tables");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    fn test_cipher() -> Arc<SettingsCipher> {
        Arc::new(SettingsCipher::new(&[0u8; 32]))
    }

    /// Buduje `IrohMeshManager` na loopback z wylaczonym discovery (mDNS/DHT),
    /// zeby test nie zalezal od srodowiska sieciowego.
    async fn make_manager() -> Arc<IrohMeshManager> {
        let db = setup_test_db();
        let security = Arc::new(MeshSecurity::new(db, test_cipher()).expect("security new"));
        let cfg = IrohMeshConfig {
            node_id: String::new(),
            bind_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            relay_url: None,
            enable_lan_discovery: false,
            enable_dht_discovery: false,
            ..Default::default()
        };
        IrohMeshManager::new(cfg, security)
            .await
            .expect("manager new")
    }

    /// Asymetria dialowania: nizszy node_id dialuje od razu, wyzszy odracza
    /// (czeka na incoming). `node_id` to 64-hex; "0"*64 jest mniejszy od
    /// kazdego realnego self_hex, "f"*64 wiekszy — wiec test jest
    /// deterministyczny niezaleznie od losowego klucza endpointu.
    #[tokio::test]
    async fn should_proactively_dial_respects_asymmetry() {
        let mgr = make_manager().await;
        let peer_low = "0".repeat(64);
        assert!(
            !mgr.should_proactively_dial(&peer_low),
            "wyzszy node_id nie powinien dialowac od razu — czeka na incoming"
        );
        let peer_high = "f".repeat(64);
        assert!(
            mgr.should_proactively_dial(&peer_high),
            "nizszy node_id powinien dialowac proaktywnie"
        );
    }

    /// Nawiazuje JEDNO fizyczne polaczenie QUIC: A dial do B (znany EndpointId).
    /// Zwraca `(conn_outgoing_na_A, conn_incoming_na_B)`. Obie wartosci to uchwyty
    /// do tego samego fizycznego linka z dwoch perspektyw.
    ///
    /// Z braku DNS/DHT w teście podajemy konkretny `SocketAddr` (loopback z
    /// `bound_sockets()`) zeby dial nie szedl przez discovery.
    async fn single_link(
        dialer: &IrohMeshManager,
        target: &IrohMeshManager,
    ) -> (Connection, Connection) {
        let target_id = target.endpoint.id();
        let sockets = target.endpoint.inner().bound_sockets();
        let direct_addr = sockets
            .into_iter()
            .find(|a| a.ip().is_loopback() || a.is_ipv4())
            .expect("target bound socket");
        let target_addr = EndpointAddr::new(target_id).with_ip_addr(direct_addr);
        let accept_ep = target.endpoint.inner().clone();

        // Accept task musi wystartowac przed connect, zeby handshake mial kto
        // zapiac po stronie target'a.
        let accept = tokio::spawn(async move {
            let incoming = accept_ep.accept().await.expect("incoming");
            let connecting = incoming.accept().expect("accept incoming");
            connecting.await.expect("finalize incoming")
        });

        let out = dialer
            .endpoint
            .connect(target_addr, ALPN_MESH)
            .await
            .expect("dial");
        let inc = accept.await.expect("accept task");
        (out, inc)
    }

    /// Ustawia `local_node_id` w managerze na wartosc ktora porownuje sie w
    /// zadany sposob z `peer_hex`. `self_smaller = true` → self < peer.
    fn force_relation(manager: &IrohMeshManager, peer_hex: &str, self_smaller: bool) {
        let forced = if self_smaller {
            // Klucz "0000..." jest zawsze mniejszy od peer_hex (peer_hex pochodzi
            // z losowego Ed25519 public key, statystycznie != same zera).
            assert!(peer_hex > "0", "peer_hex musi byc != pusty");
            "0".repeat(peer_hex.len())
        } else {
            // Klucz "ffff..." jest zawsze >= peer_hex.
            "f".repeat(peer_hex.len())
        };
        *manager.local_node_id.write() = forced;
    }

    /// Outgoing connection wygrywa gdy `self_id < peer_id`. W mapie
    /// powinien zostac wpis z direction=Outgoing, funkcja zwraca Some(id).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn register_outgoing_wins_when_self_id_smaller() {
        let manager = make_manager().await;
        let peer = make_manager().await;
        let peer_hex = hex::encode(peer.endpoint.id().as_bytes());
        force_relation(&manager, &peer_hex, true);

        let (out, _inc) = single_link(&manager, &peer).await;
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            manager.register_connection(peer_hex.clone(), out, ConnectionDirection::Outgoing),
        )
        .await
        .expect("register timeout");
        assert!(result.is_some(), "outgoing powinno wygrac");
        assert!(manager.is_connected(&peer_hex).await);
    }

    /// Outgoing connection przegrywa gdy `self_id > peer_id` (czyli to peer
    /// powinien byc dialerem) — mapa jest pusta, `None` zwracane.
    /// Uwaga: test sprawdza branch "pusta mapa + nowa jest losing direction" —
    /// w tej galezi kod i tak wpisuje connection (bo to pierwszy element),
    /// zwraca `Some(id)`. Walidacje przeprowadza nastepny test (podmien).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn register_first_outgoing_accepted_even_when_losing() {
        let manager = make_manager().await;
        let peer = make_manager().await;
        let peer_hex = hex::encode(peer.endpoint.id().as_bytes());
        force_relation(&manager, &peer_hex, false);

        let (out, _inc) = single_link(&manager, &peer).await;
        let result = tokio::time::timeout(
            Duration::from_secs(10),
            manager.register_connection(peer_hex.clone(), out, ConnectionDirection::Outgoing),
        )
        .await
        .expect("register timeout");
        assert!(result.is_some(), "pierwszy wpis zawsze wchodzi do mapy");
    }

    /// Klucz sedna: gdy w mapie jest ZWYCIEZCA i przychodzi nowy connection
    /// przeciwnego kierunku ktory tez by wygral (bo `self` zmienil sie
    /// albo to powtorzony dial) — nowa i tak PRZEGRYWA zgodnie z reguala
    /// tie-break i dostaje `None`.
    ///
    /// Scenariusz: `self_id < peer_id` → outgoing to zwyciezca. Najpierw
    /// rejestrujemy incoming (pusta mapa — wchodzi), potem outgoing (powinno
    /// podmienic przegranego incoming; poprzednie connection zostaje
    /// zamkniete). Sprawdzamy ze mapa trzyma outgoing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn winning_direction_replaces_losing_in_map() {
        let manager = make_manager().await;
        let peer = make_manager().await;
        let peer_hex = hex::encode(peer.endpoint.id().as_bytes());
        force_relation(&manager, &peer_hex, true); // self < peer → Outgoing wygrywa

        // Link 1: A → B (outgoing dla A)
        let (out_a, _inc_b) = single_link(&manager, &peer).await;
        // Link 2: B → A (incoming dla A)
        let (_out_b, inc_a) = single_link(&peer, &manager).await;

        // Najpierw probujemy zarejestrowac przegrywajacy incoming — wchodzi do
        // pustej mapy.
        let first = tokio::time::timeout(
            Duration::from_secs(10),
            manager.register_connection(
                peer_hex.clone(),
                inc_a.clone(),
                ConnectionDirection::Incoming,
            ),
        )
        .await
        .expect("register incoming timeout");
        assert!(first.is_some(), "pierwszy wpis wchodzi do mapy");

        // Potem rejestrujemy zwycieski outgoing — powinien podmienic.
        let second = tokio::time::timeout(
            Duration::from_secs(10),
            manager.register_connection(peer_hex.clone(), out_a, ConnectionDirection::Outgoing),
        )
        .await
        .expect("register outgoing timeout");
        assert!(second.is_some(), "zwycieski outgoing musi wejsc do mapy");

        // Poprzedni incoming powinien byc zamkniety z kodem tie-break.
        // `close_reason` nie jest natychmiastowe — iroh propaguje async. Damy
        // krotki bufor czasowy.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            inc_a.close_reason().is_some(),
            "przegrane incoming powinno byc zamkniete po podmianie"
        );
    }

    /// Gdy w mapie jest zwyciezca (outgoing) i przychodzi przegrywajacy
    /// incoming (bo `self_id < peer_id`) — nowy musi zostac zamkniety a mapa
    /// niezmieniona. Funkcja zwraca `None`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn loser_is_closed_and_map_unchanged() {
        let manager = make_manager().await;
        let peer = make_manager().await;
        let peer_hex = hex::encode(peer.endpoint.id().as_bytes());
        force_relation(&manager, &peer_hex, true); // self < peer → Outgoing wygrywa

        let (out_a, _inc_b) = single_link(&manager, &peer).await;
        let (_out_b, inc_a) = single_link(&peer, &manager).await;

        // Najpierw zwyciezca.
        let first = manager
            .register_connection(
                peer_hex.clone(),
                out_a.clone(),
                ConnectionDirection::Outgoing,
            )
            .await;
        let winner_id = first.expect("outgoing wygrywa");

        // Potem przychodzi przegrany.
        let second = manager
            .register_connection(
                peer_hex.clone(),
                inc_a.clone(),
                ConnectionDirection::Incoming,
            )
            .await;
        assert!(second.is_none(), "przegrany incoming nie dostaje id");

        // Mapa dalej trzyma ten sam connection_id.
        {
            let active = manager
                .connections
                .get(&peer_hex)
                .expect("entry still present");
            assert_eq!(active.id, winner_id, "zwyciezca w mapie niezmienny");
            assert_eq!(active.direction, ConnectionDirection::Outgoing);
        }

        // Przegrany incoming dostal close().
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            inc_a.close_reason().is_some(),
            "przegrany incoming powinien byc zamkniety"
        );
        // Zwyciezca dalej otwarty.
        assert!(
            out_a.close_reason().is_none(),
            "zwyciezca nie moze byc zamkniety"
        );
    }

    /// Duplikat tego samego kierunku to idempotent no-op — drugi register
    /// zwraca `None`, mapa dalej trzyma pierwszy connection_id, drugi
    /// connection zostaje zamkniety z reason "duplicate".
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn duplicate_direction_is_idempotent() {
        let manager = make_manager().await;
        let peer = make_manager().await;
        let peer_hex = hex::encode(peer.endpoint.id().as_bytes());
        force_relation(&manager, &peer_hex, true);

        let (out_first, _inc1) = single_link(&manager, &peer).await;
        let (out_second, _inc2) = single_link(&manager, &peer).await;

        let first = manager
            .register_connection(
                peer_hex.clone(),
                out_first.clone(),
                ConnectionDirection::Outgoing,
            )
            .await
            .expect("pierwszy outgoing");

        let second = manager
            .register_connection(
                peer_hex.clone(),
                out_second.clone(),
                ConnectionDirection::Outgoing,
            )
            .await;
        assert!(second.is_none(), "duplikat kierunku → no-op");

        {
            let active = manager
                .connections
                .get(&peer_hex)
                .expect("entry still present");
            assert_eq!(active.id, first);
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            out_second.close_reason().is_some(),
            "duplikat musi byc zamkniety"
        );
        assert!(out_first.close_reason().is_none(), "pierwszy dalej otwarty");
    }

    /// `dial_locks` musi zwracac ten sam `Arc<Mutex>` dla tego samego peera.
    /// To chroni przed rownoleglymi outgoing dial do tego samego peera.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dial_lock_is_shared_per_peer() {
        let manager = make_manager().await;
        let peer_hex = "a".repeat(64);
        let other_hex = "b".repeat(64);

        let lock1 = manager.dial_lock_for(&peer_hex);
        let lock2 = manager.dial_lock_for(&peer_hex);
        let lock_other = manager.dial_lock_for(&other_hex);

        assert!(
            Arc::ptr_eq(&lock1, &lock2),
            "ten sam peer = ten sam Arc<Mutex>"
        );
        assert!(
            !Arc::ptr_eq(&lock1, &lock_other),
            "rozni peerzy = rozne Arc<Mutex>"
        );
    }
}
