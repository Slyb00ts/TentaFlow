// =============================================================================
// Plik: mesh/iroh_manager.rs
// Opis: Menedzer mesh zbudowany na iroh::Endpoint. Odpowiednik QuicMeshManager,
//       rozni sie transportem (iroh QUIC + relay + LAN mDNS + DHT pkarr) i
//       brakiem warstwy AEAD (TLS 1.3 iroh wystarcza). Trzyma mape aktywnych
//       polaczen po EndpointId, emituje zdarzenia do broadcast::Receiver.
//       Message format na bidi streamie: [1 bajt discriminant][payload].
// =============================================================================

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use dashmap::DashMap;
use iroh::endpoint::Connection;
use iroh::{EndpointAddr, EndpointId, RelayUrl, TransportAddr};
use parking_lot::RwLock;
use tokio::sync::{broadcast, RwLock as AsyncRwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::mesh::security::MeshSecurity;
use crate::mesh::service_registry::MeshServiceRegistry;
use crate::net::iroh::{
    handler::IrohStreamError,
    pairing::{endpoint_addr_from_hints, hints_with_relay_fallback, PairingContactHints, PairingHandler},
    IrohConfig, IrohEndpoint, IrohEndpointError, ALPN_API, ALPN_MESH, ALPN_PAIRING,
};

/// Typ callbacka do obslugi forward requestow (compat z QuicMeshManager).
pub type ForwardHandler = Arc<
    dyn Fn(Vec<u8>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<u8>> + Send>>
        + Send
        + Sync,
>;

/// Odpowiedz komendy mesh — compat z QuicMeshManager.
#[derive(Debug, Clone)]
pub struct CommandWaitResponse {
    pub command_id: String,
    pub success: bool,
    pub output: String,
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
}

impl Default for IrohMeshConfig {
    fn default() -> Self {
        Self {
            node_id: String::new(),
            bind_addr: std::net::SocketAddr::from(([0, 0, 0, 0], 0)),
            relay_url: None,
            enable_lan_discovery: true,
            enable_dht_discovery: true,
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
    CrdtDeltaReceived {
        node_id: String,
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
    ServiceAnnounceReceived {
        node_id: String,
        data: Vec<u8>,
    },
    AliasSyncReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
    TrustRevokedReceived {
        node_id: String,
        revoked_node_id: String,
    },
    TrustedKeysSyncReceived {
        node_id: String,
        keys: Vec<(String, String)>,
    },
    NodeLeavingReceived {
        node_id: String,
    },
    ModelListUpdate {
        node_id: String,
        data: Vec<u8>,
    },
    ContainerListUpdate {
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
    FullStateReceived {
        node_id: String,
        state: Vec<u8>,
    },
    KeyRotationReceived {
        node_id: String,
        ephemeral_public_key_hex: String,
    },
    KeyRotationResponseReceived {
        node_id: String,
        ephemeral_public_key_hex: String,
    },
    RelayFrameReceived {
        from_node_id: String,
        frame: tentaflow_protocol::mesh::MeshRelayFrame,
    },
    ServiceQueryAllReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
    ServiceResponseAllReceived {
        from_node_id: String,
        data: Vec<u8>,
    },
    /// Odkryty nowy peer przez mDNS/DHT — wypala zanim zaczniemy dial.
    /// Pipeline pisze do peer_store z source=discovered zeby UI widzial peera
    /// nawet gdy dial nie zdazyl wypalic.
    PeerDiscovered {
        node_id: String,
        addresses: Vec<std::net::SocketAddr>,
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
    pub closed: bool,
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
    forward_handler: AsyncRwLock<Option<ForwardHandler>>,
    command_waiters: DashMap<String, tokio::sync::oneshot::Sender<CommandWaitResponse>>,
    service_reg: Arc<MeshServiceRegistry>,
    /// Per-peer mutex zabezpieczajacy przed rownoleglymi `endpoint.connect` do
    /// tego samego peera z roznych tasków (discovery, pairing, manual dial).
    /// DashMap — upsert/read lock-free per-shard.
    dial_locks: DashMap<String, Arc<tokio::sync::Mutex<()>>>,
    /// Stan logowania per-peer: kiedy ostatnio zalogowalismy discovery oraz
    /// ile bylo kolejnych nieudanych dialow. Sluzy do tlumienia spamu.
    peer_log_state: DashMap<String, PeerLogState>,
}

#[derive(Default)]
struct PeerLogState {
    last_discovery_log: Option<Instant>,
    consecutive_dial_failures: u32,
    last_dial_attempt: Option<Instant>,
}

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
        };

        let endpoint = IrohEndpoint::bind(iroh_config)
            .await
            .map_err(|e: IrohEndpointError| anyhow::anyhow!("iroh endpoint bind: {e:?}"))?;

        let local_id_hex = hex::encode(endpoint.id().as_bytes());
        // Duzy buffer — przy discovery burst (nowa siec, wiele peerow na raz)
        // subscriber pipeline moze chwilowo byc wolniejszy niz producent
        // eventow. 1024 bylo za malo, przy 100+ peerach Lagged sie zdarzal.
        let (event_tx, _rx) = broadcast::channel(16_384);
        let service_reg = Arc::new(MeshServiceRegistry::new(local_id_hex.clone()));

        Ok(Arc::new(Self {
            endpoint: Arc::new(endpoint),
            security,
            config,
            connections: Arc::new(DashMap::with_capacity(256)),
            event_tx,
            shutdown: CancellationToken::new(),
            local_node_id: RwLock::new(local_id_hex),
            next_connection_id: AtomicU64::new(1),
            forward_handler: AsyncRwLock::new(None),
            command_waiters: DashMap::new(),
            service_reg,
            dial_locks: DashMap::with_capacity(256),
            peer_log_state: DashMap::with_capacity(256),
        }))
    }

    /// Discovery spamuje na kazdy mDNS tick — logujemy pierwsze odkrycie peera
    /// i potem co najmniej co `DISCOVERY_LOG_COOLDOWN`. Zwraca true gdy log
    /// ma sie wyemitowac, false — stlumic.
    fn should_log_discovery(&self, peer_hex: &str) -> bool {
        const COOLDOWN: Duration = Duration::from_secs(60);
        let mut entry = self
            .peer_log_state
            .entry(peer_hex.to_string())
            .or_default();
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
        let mut entry = self
            .peer_log_state
            .entry(peer_hex.to_string())
            .or_default();
        entry.consecutive_dial_failures = entry.consecutive_dial_failures.saturating_add(1);
        entry.consecutive_dial_failures
    }

    /// Reset licznika po udanym polaczeniu.
    fn note_dial_success(&self, peer_hex: &str) {
        if let Some(mut entry) = self.peer_log_state.get_mut(peer_hex) {
            entry.consecutive_dial_failures = 0;
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
        let mut entry = self
            .peer_log_state
            .entry(peer_hex.to_string())
            .or_default();
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
                    use iroh::address_lookup::DiscoveryEvent;
                    if let DiscoveryEvent::Discovered { endpoint_info, .. } = ev {
                        let peer_id = endpoint_info.endpoint_id;
                        let peer_hex = hex::encode(peer_id.as_bytes());
                        if peer_hex == self_hex {
                            continue;
                        }
                        let addresses: Vec<std::net::SocketAddr> =
                            endpoint_info.data.ip_addrs().copied().collect();
                        let _ = self_arc.event_tx.send(IrohMeshEvent::PeerDiscovered {
                            node_id: peer_hex.clone(),
                            addresses: addresses.clone(),
                        });
                        if self_arc.is_connected(&peer_hex).await {
                            continue;
                        }
                        // Tlumimy rapid re-dial tego samego peera — cooldown
                        // jest dluzszy dla niesparowanych (30s) niz zaufanych
                        // (5s). Dzieki temu dwa nody w tej samej LANie nie
                        // wchodza w petle tie-break / re-discovery.
                        let is_trusted = self_arc.security.is_trusted(&peer_hex);
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
                        let me = Arc::clone(&self_arc);
                        tokio::spawn(async move {
                            let dummy = std::net::SocketAddr::from(([0, 0, 0, 0], 0));
                            match me.connect_to_peer(&peer_hex, dummy).await {
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
                            warn!("iroh_mesh: obsluga incoming nieudana: {}", e);
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
                let handler = PairingHandler::new(Arc::clone(&self.security), hostname());
                if let Err(e) = handler_accept_connection(&handler, connection).await {
                    warn!("iroh_mesh: pairing accept blad: {}", e);
                }
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
        if let Some(relay) = self.endpoint.inner().addr().relay_urls().next().cloned() {
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
        if let Some(relay) = self.endpoint.inner().addr().relay_urls().next().cloned() {
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
        let hints_resolved = hints_with_relay_fallback(self.endpoint.inner(), hints);
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

    /// Wysyla ramke `[disc][data]` na uni streamie do peera.
    pub async fn send_to_peer(
        &self,
        target_node_id: &str,
        discriminant: u8,
        data: &[u8],
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
        send.write_all(&[discriminant])
            .await
            .map_err(|e| anyhow::anyhow!("write discriminant: {e}"))?;
        if !data.is_empty() {
            send.write_all(data)
                .await
                .map_err(|e| anyhow::anyhow!("write payload: {e}"))?;
        }
        send.finish()
            .map_err(|e| anyhow::anyhow!("finish uni: {e}"))?;
        Ok(())
    }

    pub async fn broadcast_to_trusted(
        &self,
        discriminant: u8,
        data: &[u8],
        exclude: Option<&str>,
    ) -> Vec<(String, Result<()>)> {
        use futures::future::join_all;
        let trusted = self.security.trusted_node_ids_snapshot();
        let targets: Vec<String> = self
            .connections
            .iter()
            .map(|e| e.key().clone())
            .filter(|id| trusted.contains(id))
            .filter(|id| exclude.map(|e| id.as_str() != e).unwrap_or(true))
            .collect();
        // PARALLEL: send_to_peer na kazdy cel rownolegle. Dla 1000 peerow
        // sekwencyjne wysylanie trwalo 2-5s (open_uni + write + finish);
        // rownolegle spada do max(rtt) + overhead, zwykle <50ms.
        let futs = targets.into_iter().map(|node_id| async move {
            let res = self.send_to_peer(&node_id, discriminant, data).await;
            (node_id, res)
        });
        join_all(futs).await
    }

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
    // Convenience wrappers — odpowiedniki metod QuicMeshManager. Kazdy deleguje
    // do `send_to_peer` z odpowiednim discriminantem z `tentaflow_protocol::mesh`.
    // =========================================================================

    pub async fn send_heartbeat_data(&self, data: &[u8]) {
        use futures::future::join_all;
        let ids: Vec<String> = self.connected_peers().await;
        let futs = ids.into_iter().map(|id| async move {
            let _ = self
                .send_to_peer(&id, tentaflow_protocol::mesh::MESH_MSG_HEARTBEAT, data)
                .await;
        });
        join_all(futs).await;
    }

    /// Broadcast listy modeli do wszystkich polaczonych peerow. Wywolywane
    /// co `models_sync_interval` z pipeline.
    pub async fn send_models_sync_data(&self, data: &[u8]) {
        use futures::future::join_all;
        let ids: Vec<String> = self.connected_peers().await;
        let futs = ids.into_iter().map(|id| async move {
            let _ = self
                .send_to_peer(&id, tentaflow_protocol::mesh::MESH_MSG_MODEL_LIST, data)
                .await;
        });
        join_all(futs).await;
    }

    pub async fn send_node_info(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_to_peer(node_id, tentaflow_protocol::mesh::MESH_MSG_NODE_INFO, data)
            .await
    }

    pub async fn send_hello(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_to_peer(node_id, tentaflow_protocol::mesh::MESH_MSG_HELLO, data)
            .await
    }

    /// Wysyla TopologyAnnounce do jednego zaufanego peera (unicast).
    /// Broadcast realizuje pipeline przez iteracje listy peerow.
    pub async fn send_topology_announce(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_to_peer(
            node_id,
            tentaflow_protocol::mesh::MESH_MSG_TOPOLOGY_ANNOUNCE,
            data,
        )
        .await
    }

    pub async fn send_known_peers(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_to_peer(
            node_id,
            tentaflow_protocol::mesh::MESH_MSG_KNOWN_PEERS,
            data,
        )
        .await
    }

    pub async fn send_pairing_request(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_to_peer(
            node_id,
            tentaflow_protocol::mesh::MESH_MSG_PAIRING_REQUEST,
            data,
        )
        .await
    }

    pub async fn send_pairing_confirm(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_to_peer(
            node_id,
            tentaflow_protocol::mesh::MESH_MSG_PAIRING_CONFIRM,
            data,
        )
        .await
    }

    pub async fn send_pairing_reject(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_to_peer(
            node_id,
            tentaflow_protocol::mesh::MESH_MSG_PAIRING_REJECT,
            data,
        )
        .await
    }

    pub async fn send_trust_revoked(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_to_peer(
            node_id,
            tentaflow_protocol::mesh::MESH_MSG_TRUST_REVOKED,
            data,
        )
        .await
    }

    pub async fn send_trusted_keys_sync(&self, node_id: &str, data: &[u8]) -> Result<()> {
        self.send_to_peer(
            node_id,
            tentaflow_protocol::mesh::MESH_MSG_TRUSTED_KEYS_SYNC,
            data,
        )
        .await
    }

    pub async fn send_node_leaving(&self) {
        let data = vec![];
        let _ = self
            .broadcast_to_trusted(tentaflow_protocol::mesh::MESH_MSG_NODE_LEAVING, &data, None)
            .await;
    }

    pub async fn broadcast_node_info(&self, data: &[u8]) {
        let _ = self
            .broadcast_to_trusted(tentaflow_protocol::mesh::MESH_MSG_NODE_INFO, data, None)
            .await;
    }

    pub async fn broadcast_crdt_delta(&self, data: Vec<u8>) {
        let _ = self
            .broadcast_to_trusted(tentaflow_protocol::mesh::MESH_MSG_CRDT_DELTA, &data, None)
            .await;
    }

    pub async fn broadcast_alias_sync(&self, aliases_json: Vec<u8>) {
        let _ = self
            .broadcast_to_trusted(
                tentaflow_protocol::mesh::MESH_MSG_ALIAS_SYNC,
                &aliases_json,
                None,
            )
            .await;
    }

    /// Forward request na peera i czeka na odpowiedz. `request_id` uzyty w
    /// payloadzie dla tracking (format: [u32 id_len][id_bytes][payload]).
    pub async fn forward_request(
        &self,
        target_node_id: &str,
        request_id: &str,
        payload: Vec<u8>,
    ) -> Result<Vec<u8>> {
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

    /// Zwraca snapshot EndpointId wszystkich znanych polaczonych peerow.
    pub async fn connected_peer_ids(&self) -> Vec<String> {
        self.connected_peers().await
    }

    /// Zwraca referencje do rejestru serwisow mesh.
    pub fn service_registry(&self) -> &Arc<MeshServiceRegistry> {
        &self.service_reg
    }

    /// Ustawia callback dla incoming forward requestow.
    pub async fn set_forward_handler(&self, handler: ForwardHandler) {
        *self.forward_handler.write().await = Some(handler);
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
        let envelope = serde_json::json!({
            "command_id": command_id,
            "sender_node_id": self.node_id(),
            "command": command,
        });
        let data =
            serde_json::to_vec(&envelope).map_err(|e| anyhow::anyhow!("encode command: {e}"))?;
        self.send_command_and_wait_bytes(target_node_id, command_id, data, Duration::from_secs(600))
            .await
            .map(|r| crate::mesh::command_executor::CommandResponse {
                success: r.success,
                output: r.output,
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
        let envelope = serde_json::json!({
            "command_id": command_id,
            "sender_node_id": self.node_id(),
            "command": command,
        });
        let data =
            serde_json::to_vec(&envelope).map_err(|e| anyhow::anyhow!("encode command: {e}"))?;
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

        self.send_to_peer(
            target_node_id,
            tentaflow_protocol::mesh::MESH_MSG_COMMAND,
            &data,
        )
        .await?;

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

    /// Rozwiazuje oczekujacego waiter gdy nadejdzie CommandResponse.
    pub async fn resolve_command_waiter(
        &self,
        command_id: &str,
        success: bool,
        output: &str,
        error: Option<&str>,
    ) -> bool {
        if let Some((_, tx)) = self.command_waiters.remove(command_id) {
            let _ = tx.send(CommandWaitResponse {
                command_id: command_id.to_string(),
                success,
                output: output.to_string(),
                error: error.map(String::from),
            });
            true
        } else {
            false
        }
    }

    /// Obsluzyc komende otrzymana od peera — wywolac wlasciwy executor.
    pub async fn handle_command_received(&self, _from_node_id: &str, _data: &[u8]) {
        // Delegacja do executor-a odbywa sie na poziomie peer_manager / pipeline.
        // Ta metoda jest zachowana dla compat ale nie wykonuje akcji — wlasciwa
        // logika jest po stronie callerow konsumujacych `IrohMeshEvent::MeshCommandReceived`.
    }

    /// Obsluzyc odpowiedz na komende otrzymana od peera.
    pub async fn handle_command_response_received(&self, _from_node_id: &str, data: &[u8]) {
        // Parse JSON: { command_id, success, output, error? } i rozwiaz waiter.
        if let Ok(val) = serde_json::from_slice::<serde_json::Value>(data) {
            let command_id = val.get("command_id").and_then(|v| v.as_str()).unwrap_or("");
            let success = val
                .get("success")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let output = val.get("output").and_then(|v| v.as_str()).unwrap_or("");
            let error = val.get("error").and_then(|v| v.as_str());
            if !command_id.is_empty() {
                self.resolve_command_waiter(command_id, success, output, error)
                    .await;
            }
        }
    }

    pub async fn send_key_rotation(&self, target_node_id: &str, data: &[u8]) -> Result<()> {
        self.send_to_peer(
            target_node_id,
            tentaflow_protocol::mesh::MESH_MSG_KEY_ROTATION,
            data,
        )
        .await
    }

    pub async fn send_key_rotation_response(
        &self,
        target_node_id: &str,
        data: &[u8],
    ) -> Result<()> {
        self.send_to_peer(
            target_node_id,
            tentaflow_protocol::mesh::MESH_MSG_KEY_ROTATION_RESPONSE,
            data,
        )
        .await
    }

    /// Wysyla relay frame (multi-hop) do nastepnego noda w trasie.
    pub async fn send_relay_frame(&self, next_hop_id: &str, frame_bytes: &[u8]) -> Result<()> {
        self.send_to_peer(
            next_hop_id,
            tentaflow_protocol::mesh::MESH_MSG_RELAY_FRAME,
            frame_bytes,
        )
        .await
    }

    /// Wysyla payload przez relay do docelowego peera (wybiera pierwszy hop z config).
    pub async fn send_via_relay(&self, via_node_id: &str, frame_bytes: &[u8]) -> Result<()> {
        self.send_relay_frame(via_node_id, frame_bytes).await
    }
}

/// Kopia referencji uzywana w spawned tasks — bez `Arc<Self>` aby unikac cyklu.
#[derive(Clone)]
struct IrohMeshManagerRef {
    connections: Arc<DashMap<String, ActiveConnection>>,
    event_tx: broadcast::Sender<IrohMeshEvent>,
}

impl IrohMeshManagerRef {
    async fn handle_mesh_connection(
        &self,
        remote_hex: String,
        connection: Connection,
        connection_id: u64,
    ) {
        let mut close_reason: Option<String> = None;
        loop {
            let recv = match connection.accept_uni().await {
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
        let is_current = self
            .connections
            .get(&remote_hex)
            .map(|active| active.id == connection_id)
            .unwrap_or(false);
        if is_current {
            self.connections.remove(&remote_hex);
            let reason = close_reason.as_deref().unwrap_or("stream closed");
            info!(peer = %remote_hex, reason, "iroh_mesh: polaczenie zamkniete");
            let _ = self.event_tx.send(IrohMeshEvent::PeerDisconnected {
                node_id: remote_hex,
            });
        }
    }

    async fn handle_mesh_uni(
        &self,
        remote_hex: String,
        mut recv: iroh::endpoint::RecvStream,
    ) -> Result<(), IrohStreamError> {
        let mut disc = [0u8; 1];
        recv.read_exact(&mut disc)
            .await
            .map_err(|e| IrohStreamError::Io(format!("{e}")))?;
        // iroh RecvStream.read_to_end bierze limit bajtow, zwraca Vec<u8>.
        let payload = recv
            .read_to_end(MAX_MSG_BYTES)
            .await
            .map_err(|e| IrohStreamError::Io(format!("{e}")))?;
        if payload.len() > MAX_MSG_BYTES {
            return Err(IrohStreamError::FrameTooLarge(payload.len()));
        }

        use tentaflow_protocol::mesh::*;
        let event = match disc[0] {
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
            x if x == MESH_MSG_CRDT_DELTA => IrohMeshEvent::CrdtDeltaReceived {
                node_id: remote_hex,
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
            x if x == MESH_MSG_SERVICE_ANNOUNCE => IrohMeshEvent::ServiceAnnounceReceived {
                node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_ALIAS_SYNC => IrohMeshEvent::AliasSyncReceived {
                from_node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_MODEL_LIST => IrohMeshEvent::ModelListUpdate {
                node_id: remote_hex,
                data: payload,
            },
            x if x == MESH_MSG_TRUSTED_KEYS_SYNC => {
                let parsed = rkyv::from_bytes::<
                    tentaflow_protocol::mesh::TrustedKeysSyncPayload,
                    rkyv::rancor::Error,
                >(&payload);
                match parsed {
                    Ok(p) => IrohMeshEvent::TrustedKeysSyncReceived {
                        node_id: remote_hex,
                        keys: p
                            .keys
                            .into_iter()
                            .map(|e| (e.node_id, e.public_key_hex))
                            .collect(),
                    },
                    Err(e) => {
                        warn!(peer = %remote_hex, "iroh_mesh: nie udalo sie zdekodowac TrustedKeysSync: {}", e);
                        return Ok(());
                    }
                }
            }
            x if x == MESH_MSG_TRUST_REVOKED => {
                // payload: JSON { revoked_node_id }
                let revoked: String = serde_json::from_slice::<serde_json::Value>(&payload)
                    .ok()
                    .and_then(|v| {
                        v.get("revoked_node_id")
                            .and_then(|x| x.as_str())
                            .map(String::from)
                    })
                    .unwrap_or_default();
                IrohMeshEvent::TrustRevokedReceived {
                    node_id: remote_hex,
                    revoked_node_id: revoked,
                }
            }
            x if x == MESH_MSG_NODE_LEAVING => IrohMeshEvent::NodeLeavingReceived {
                node_id: remote_hex,
            },
            x if x == MESH_MSG_CONTAINER_LIST => IrohMeshEvent::ContainerListUpdate {
                node_id: remote_hex,
                data: payload,
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

/// Funkcja pomocnicza wywolywana przez accept loop przy pairing ALPN. Separacja
/// od manager-a ulatwia testowanie.
async fn handler_accept_connection(handler: &PairingHandler, connection: Connection) -> Result<()> {
    use iroh::protocol::ProtocolHandler;
    handler
        .accept(connection)
        .await
        .map_err(|e| anyhow::anyhow!("pairing accept: {e:?}"))?;
    Ok(())
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
                closed: path.is_closed(),
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

fn hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|s| s.into_string().ok())
        .unwrap_or_else(|| "unknown-host".to_string())
}

#[cfg(test)]
mod tests {
    use super::{endpoint_addr_from_target, is_private_socket_addr, transport_kind, transport_scope};
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
            vec!["192.168.1.10:7777"
                .parse::<std::net::SocketAddr>()
                .unwrap()]
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
    use iroh::{endpoint::Connection, SecretKey};
    use std::sync::Mutex;
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
        Arc::new(Mutex::new(conn))
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
        };
        IrohMeshManager::new(cfg, security)
            .await
            .expect("manager new")
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
        .expect("register timeout")
        ;
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
            manager.register_connection(peer_hex.clone(), inc_a.clone(), ConnectionDirection::Incoming),
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
            .register_connection(peer_hex.clone(), out_a.clone(), ConnectionDirection::Outgoing)
            .await;
        let winner_id = first.expect("outgoing wygrywa");

        // Potem przychodzi przegrany.
        let second = manager
            .register_connection(peer_hex.clone(), inc_a.clone(), ConnectionDirection::Incoming)
            .await;
        assert!(second.is_none(), "przegrany incoming nie dostaje id");

        // Mapa dalej trzyma ten sam connection_id.
        {
            let active = manager.connections.get(&peer_hex).expect("entry still present");
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
            .register_connection(peer_hex.clone(), out_first.clone(), ConnectionDirection::Outgoing)
            .await
            .expect("pierwszy outgoing");

        let second = manager
            .register_connection(peer_hex.clone(), out_second.clone(), ConnectionDirection::Outgoing)
            .await;
        assert!(second.is_none(), "duplikat kierunku → no-op");

        {
            let active = manager.connections.get(&peer_hex).expect("entry still present");
            assert_eq!(active.id, first);
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            out_second.close_reason().is_some(),
            "duplikat musi byc zamkniety"
        );
        assert!(
            out_first.close_reason().is_none(),
            "pierwszy dalej otwarty"
        );
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
