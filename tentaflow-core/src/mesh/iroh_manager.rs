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
use std::time::Duration;

use anyhow::{Context, Result};
use iroh::endpoint::Connection;
use iroh::{EndpointAddr, EndpointId, RelayUrl};
use parking_lot::RwLock;
use tokio::sync::{broadcast, RwLock as AsyncRwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::mesh::security::MeshSecurity;
use crate::mesh::service_registry::MeshServiceRegistry;
use crate::net::iroh::{
    handler::IrohStreamError, pairing::PairingHandler, IrohConfig, IrohEndpoint, IrohEndpointError,
    ALPN_API, ALPN_MESH, ALPN_PAIRING,
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
    /// Interwal wysylania heartbeatow.
    pub heartbeat_interval: Duration,
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
            heartbeat_interval: Duration::from_millis(500),
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

/// Aktywne polaczenie zalogowane przez manager.
struct ActiveConnection {
    connection: Connection,
    remote_id_hex: String,
}

/// Glowny menedzer mesh uzywajacy iroh.
pub struct IrohMeshManager {
    endpoint: Arc<IrohEndpoint>,
    security: Arc<MeshSecurity>,
    config: IrohMeshConfig,
    connections: Arc<AsyncRwLock<HashMap<String, ActiveConnection>>>,
    event_tx: broadcast::Sender<IrohMeshEvent>,
    shutdown: CancellationToken,
    local_node_id: RwLock<String>,
    forward_handler: AsyncRwLock<Option<ForwardHandler>>,
    command_waiters:
        AsyncRwLock<HashMap<String, tokio::sync::oneshot::Sender<CommandWaitResponse>>>,
    service_reg: Arc<MeshServiceRegistry>,
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
        let (event_tx, _rx) = broadcast::channel(1024);
        let service_reg = Arc::new(MeshServiceRegistry::new(local_id_hex.clone()));

        Ok(Arc::new(Self {
            endpoint: Arc::new(endpoint),
            security,
            config,
            connections: Arc::new(AsyncRwLock::new(HashMap::new())),
            event_tx,
            shutdown: CancellationToken::new(),
            local_node_id: RwLock::new(local_id_hex),
            forward_handler: AsyncRwLock::new(None),
            command_waiters: AsyncRwLock::new(HashMap::new()),
            service_reg,
        }))
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
            me.run_heartbeat_loop().await;
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
                        info!(peer = %peer_hex, addrs = ?addresses, "iroh_mesh: odkryty nowy peer — dial");
                        let me = Arc::clone(&self_arc);
                        tokio::spawn(async move {
                            let dummy = std::net::SocketAddr::from(([0, 0, 0, 0], 0));
                            if let Err(e) = me.connect_to_peer(&peer_hex, dummy).await {
                                warn!(peer = %peer_hex, "iroh_mesh: dial po discovery nieudany: {}", e);
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
            endpoint: Arc::clone(&self.endpoint),
            security: Arc::clone(&self.security),
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
        info!(peer = %remote_hex, alpn = ?alpn, "iroh_mesh: polaczenie zaakceptowane");

        match alpn {
            a if a == ALPN_MESH => {
                self.register_connection(remote_hex.clone(), connection.clone())
                    .await;
                let _ = self.event_tx.send(IrohMeshEvent::PeerConnected {
                    node_id: remote_hex.clone(),
                });
                let me = self.clone_for_spawn();
                tokio::spawn(async move {
                    me.handle_mesh_connection(remote_hex, connection).await;
                });
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

    async fn register_connection(&self, remote_hex: String, conn: Connection) {
        let mut map = self.connections.write().await;
        map.insert(
            remote_hex.clone(),
            ActiveConnection {
                connection: conn,
                remote_id_hex: remote_hex,
            },
        );
    }

    async fn handle_mesh_connection(&self, remote_hex: String, connection: Connection) {
        // Petla odbierania uni streamow. Format: [1B disc][payload].
        loop {
            let recv = match connection.accept_uni().await {
                Ok(r) => r,
                Err(e) => {
                    debug!(peer = %remote_hex, "mesh uni stream closed: {}", e);
                    break;
                }
            };
            let me = self.clone_for_spawn();
            let rhex = remote_hex.clone();
            tokio::spawn(async move {
                if let Err(e) = me.handle_mesh_uni(rhex, recv).await {
                    debug!("mesh uni handler blad: {}", e);
                }
            });
        }
        self.connections.write().await.remove(&remote_hex);
        let _ = self.event_tx.send(IrohMeshEvent::PeerDisconnected {
            node_id: remote_hex,
        });
    }

    async fn run_heartbeat_loop(&self) {
        let mut ticker = tokio::time::interval(self.config.heartbeat_interval);
        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => return,
                _ = ticker.tick() => {
                    // Heartbeat payload: placeholder. Callerzy mesh uzywaja
                    // send_heartbeat_data() z wlasnym payloadem.
                }
            }
        }
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

    pub fn subscribe(&self) -> broadcast::Receiver<IrohMeshEvent> {
        self.event_tx.subscribe()
    }

    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        self.connections.write().await.clear();
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
        if self.is_connected(node_id_hex).await {
            return Ok(());
        }
        let addr = endpoint_addr_from_target(node_id_hex, Some(addr))?;
        let connection = self
            .endpoint
            .connect(addr, ALPN_MESH)
            .await
            .map_err(|e| anyhow::anyhow!("iroh connect: {e:?}"))?;
        self.register_connection(node_id_hex.to_string(), connection.clone())
            .await;
        let _ = self.event_tx.send(IrohMeshEvent::PeerConnected {
            node_id: node_id_hex.to_string(),
        });
        let me = self.clone_for_spawn();
        let id = node_id_hex.to_string();
        tokio::spawn(async move {
            me.handle_mesh_connection(id, connection).await;
        });
        Ok(())
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
        if self.is_connected(node_id_hex).await {
            return Ok(());
        }
        let endpoint_id = parse_endpoint_id(node_id_hex)?;
        let addr = EndpointAddr::new(endpoint_id).with_ip_addr(direct_addr);
        let connection = self
            .endpoint
            .connect(addr, ALPN_MESH)
            .await
            .map_err(|e| anyhow::anyhow!("iroh connect direct: {e:?}"))?;
        self.register_connection(node_id_hex.to_string(), connection.clone())
            .await;
        let _ = self.event_tx.send(IrohMeshEvent::PeerConnected {
            node_id: node_id_hex.to_string(),
        });
        let me = self.clone_for_spawn();
        let id = node_id_hex.to_string();
        tokio::spawn(async move {
            me.handle_mesh_connection(id, connection).await;
        });
        Ok(())
    }

    /// Wysyla ramke `[disc][data]` na uni streamie do peera.
    pub async fn send_to_peer(
        &self,
        target_node_id: &str,
        discriminant: u8,
        data: &[u8],
    ) -> Result<()> {
        let map = self.connections.read().await;
        let active = map
            .get(target_node_id)
            .ok_or_else(|| anyhow::anyhow!("brak polaczenia z {}", target_node_id))?;
        let connection = active.connection.clone();
        drop(map);

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
        let trusted = self.security.trusted_node_ids_snapshot();
        let map = self.connections.read().await;
        let targets: Vec<String> = map
            .keys()
            .filter(|id| trusted.contains(*id))
            .filter(|id| exclude.map(|e| id.as_str() != e).unwrap_or(true))
            .cloned()
            .collect();
        drop(map);
        let mut results = Vec::with_capacity(targets.len());
        for node_id in targets {
            let res = self.send_to_peer(&node_id, discriminant, data).await;
            results.push((node_id, res));
        }
        results
    }

    pub async fn connected_peers(&self) -> Vec<String> {
        self.connections.read().await.keys().cloned().collect()
    }

    pub async fn is_connected(&self, node_id: &str) -> bool {
        self.connections.read().await.contains_key(node_id)
    }

    pub async fn disconnect_peer(&self, node_id: &str) {
        if let Some(active) = self.connections.write().await.remove(node_id) {
            active.connection.close(0u32.into(), b"disconnect");
            let _ = self.event_tx.send(IrohMeshEvent::PeerDisconnected {
                node_id: node_id.to_string(),
            });
        }
    }

    // =========================================================================
    // Convenience wrappers — odpowiedniki metod QuicMeshManager. Kazdy deleguje
    // do `send_to_peer` z odpowiednim discriminantem z `tentaflow_protocol::mesh`.
    // =========================================================================

    pub async fn send_heartbeat_data(&self, data: &[u8]) {
        let ids: Vec<String> = self.connected_peers().await;
        for id in ids {
            let _ = self
                .send_to_peer(&id, tentaflow_protocol::mesh::MESH_MSG_HEARTBEAT, data)
                .await;
        }
    }

    /// Broadcast listy modeli do wszystkich polaczonych peerow. Wywolywane
    /// co `models_sync_interval` z pipeline.
    pub async fn send_models_sync_data(&self, data: &[u8]) {
        let ids: Vec<String> = self.connected_peers().await;
        for id in ids {
            let _ = self
                .send_to_peer(&id, tentaflow_protocol::mesh::MESH_MSG_MODEL_LIST, data)
                .await;
        }
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
        let map = self.connections.read().await;
        let active = map
            .get(target_node_id)
            .ok_or_else(|| anyhow::anyhow!("brak polaczenia z {}", target_node_id))?;
        let connection = active.connection.clone();
        drop(map);

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
        self.command_waiters
            .write()
            .await
            .insert(command_id.clone(), tx);

        self.send_to_peer(
            target_node_id,
            tentaflow_protocol::mesh::MESH_MSG_COMMAND,
            &data,
        )
        .await?;

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(resp)) => Ok(resp),
            Ok(Err(_)) => {
                self.command_waiters.write().await.remove(&command_id);
                anyhow::bail!("command waiter dropped before response")
            }
            Err(_) => {
                self.command_waiters.write().await.remove(&command_id);
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
        if let Some(tx) = self.command_waiters.write().await.remove(command_id) {
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
    endpoint: Arc<IrohEndpoint>,
    security: Arc<MeshSecurity>,
    connections: Arc<AsyncRwLock<HashMap<String, ActiveConnection>>>,
    event_tx: broadcast::Sender<IrohMeshEvent>,
}

impl IrohMeshManagerRef {
    async fn handle_mesh_connection(&self, remote_hex: String, connection: Connection) {
        loop {
            let recv = match connection.accept_uni().await {
                Ok(r) => r,
                Err(e) => {
                    debug!(peer = %remote_hex, "mesh uni stream closed: {}", e);
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
        self.connections.write().await.remove(&remote_hex);
        let _ = self.event_tx.send(IrohMeshEvent::PeerDisconnected {
            node_id: remote_hex,
        });
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
            x if x == MESH_MSG_MODEL_LIST => IrohMeshEvent::ModelListUpdate {
                node_id: remote_hex,
                data: payload,
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

fn hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|s| s.into_string().ok())
        .unwrap_or_else(|| "unknown-host".to_string())
}

#[cfg(test)]
mod tests {
    use super::endpoint_addr_from_target;

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
}
