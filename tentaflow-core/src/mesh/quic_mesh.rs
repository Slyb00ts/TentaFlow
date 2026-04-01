// =============================================================================
// Plik: mesh/quic_mesh.rs
// Opis: QuicMeshManager — zarzadzanie stalymi polaczeniami QUIC miedzy nodami
//       mesh. Obsluguje heartbeaty, CRDT delta sync, forwarding requestow
//       i automatyczny reconnect z exponential backoff.
// =============================================================================

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use quinn::{ClientConfig, Endpoint, ServerConfig as QuinnServerConfig};
use rustls::pki_types::CertificateDer;
use tokio::sync::{broadcast, RwLock};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::mesh::command_executor::{CommandResponse, MeshCommandExecutor};
use crate::mesh::security::MeshSecurity;
use crate::mesh::service_registry::MeshServiceRegistry;
use tentaflow_protocol::mesh::{
    MeshCommandType, MeshMessage, MeshServiceInfo,
    MESH_MSG_COMMAND, MESH_MSG_COMMAND_RESPONSE, MESH_MSG_CONTAINER_LIST, MESH_MSG_CRDT_DELTA,
    MESH_MSG_DEPLOY_PROGRESS, MESH_MSG_FORWARD_REQ, MESH_MSG_FULL_STATE, MESH_MSG_HEARTBEAT,
    MESH_MSG_LOG_CHUNK, MESH_MSG_MODEL_LIST, MESH_MSG_NODE_INFO, MESH_MSG_PAIRING_CONFIRM,
    MESH_MSG_PAIRING_REJECT, MESH_MSG_PAIRING_REQUEST, MESH_MSG_CLUSTER_INFO,
    MESH_MSG_SERVICE_ANNOUNCE, MESH_MSG_SERVICE_QUERY_ALL, MESH_MSG_SERVICE_RESPONSE_ALL,
    MESH_MSG_TRUST_REVOKED, MESH_MSG_KEY_ROTATION, MESH_MSG_TRUSTED_KEYS_SYNC,
    MESH_MSG_KEY_ROTATION_RESPONSE, MESH_MSG_NODE_LEAVING, MESH_MSG_RELAY_FRAME,
    TrustRevokedPayload, KeyRotationPayload, KeyRotationResponsePayload,
    TrustedKeysSyncPayload, NodeLeavingPayload, MeshRelayFrame,
};

// =============================================================================
// Typy publiczne
// =============================================================================

/// Konfiguracja polaczen QUIC mesh
#[derive(Debug, Clone)]
pub struct QuicMeshConfig {
    /// Identyfikator tego noda
    pub node_id: String,
    /// Port nasluchiwania QUIC
    pub listen_port: u16,
    /// Interwal wysylania heartbeatow
    pub heartbeat_interval: Duration,
    /// Bazowy czas reconnectu (exponential backoff)
    pub reconnect_base: Duration,
    /// Maksymalny czas reconnectu
    pub reconnect_max: Duration,
}

impl Default for QuicMeshConfig {
    fn default() -> Self {
        Self {
            node_id: uuid::Uuid::new_v4().to_string(),
            listen_port: 5002,
            heartbeat_interval: Duration::from_millis(500),
            reconnect_base: Duration::from_secs(1),
            reconnect_max: Duration::from_secs(30),
        }
    }
}

/// Zdarzenie w mesh QUIC — caller (PeerManager) subskrybuje
#[derive(Debug, Clone)]
pub enum QuicMeshEvent {
    PeerConnected { node_id: String },
    PeerDisconnected { node_id: String },
    HeartbeatReceived { node_id: String, heartbeat: Vec<u8> },
    FullStateReceived { node_id: String, state: Vec<u8> },
    CrdtDeltaReceived { node_id: String, data: Vec<u8> },
    ForwardRequest { node_id: String, request_id: String, payload: Vec<u8> },
    ModelListUpdate { node_id: String, data: Vec<u8> },
    ContainerListUpdate { node_id: String, data: Vec<u8> },
    NodeInfoReceived { node_id: String, data: Vec<u8> },
    /// Otrzymano zadanie parowania od peera
    PairingRequestReceived { peer_id: String, data: Vec<u8> },
    /// Otrzymano potwierdzenie parowania od peera
    PairingConfirmReceived { peer_id: String, data: Vec<u8> },
    /// Otrzymano odrzucenie parowania od peera
    PairingRejectReceived { peer_id: String, data: Vec<u8> },
    /// Otrzymano komende zarzadzania od peera (command_id jest w payloadzie)
    MeshCommandReceived { from_node_id: String, command: Vec<u8> },
    /// Otrzymano cofniecie zaufania od peera
    TrustRevokedReceived { node_id: String, revoked_node_id: String },
    /// Otrzymano odpowiedz na komende zarzadzania
    MeshCommandResponseReceived { from_node_id: String, data: Vec<u8> },
    /// Otrzymano postep deploy od peera
    MeshDeployProgressReceived { from_node_id: String, data: Vec<u8> },
    /// Otrzymano fragment logow kontenera
    MeshLogChunkReceived { from_node_id: String, data: Vec<u8> },
    /// Otrzymano ServiceAnnounce od peera
    ServiceAnnounceReceived { node_id: String, data: Vec<u8> },
    /// Otrzymano zapytanie o wszystkie serwisy
    ServiceQueryAllReceived { from_node_id: String, data: Vec<u8> },
    /// Otrzymano odpowiedz z lista serwisow
    ServiceResponseAllReceived { from_node_id: String, data: Vec<u8> },
    /// Otrzymano zadanie rotacji klucza od peera
    KeyRotationReceived { node_id: String, ephemeral_public_key_hex: String },
    /// Otrzymano synchronizacje zaufanych kluczy po sparowaniu
    TrustedKeysSyncReceived { node_id: String, keys: Vec<(String, String)> },
    /// Otrzymano odpowiedz na rotacje klucza od peera
    KeyRotationResponseReceived { node_id: String, ephemeral_public_key_hex: String },
    /// Otrzymano informacje o opuszczeniu mesh przez peera
    NodeLeavingReceived { node_id: String },
    /// Otrzymano relay frame (multi-hop routing)
    RelayFrameReceived { from_node_id: String, frame: MeshRelayFrame },
}

/// Typ callbacka do obslugi forward requestow
pub type ForwardHandler = Arc<dyn Fn(Vec<u8>) -> std::pin::Pin<Box<dyn std::future::Future<Output = Vec<u8>> + Send>> + Send + Sync>;

/// Aktywne polaczenie mesh z peerem
struct MeshConnection {
    connection: quinn::Connection,
}

// =============================================================================
// QuicMeshManager
// =============================================================================

/// Menedzer stalych polaczen QUIC miedzy nodami mesh.
///
/// Regula inicjowania: node o nizszym node_id (leksykograficznie) inicjuje
/// polaczenie. Peer o wyzszym node_id czeka na accept.
pub struct QuicMeshManager {
    node_id: String,
    config: QuicMeshConfig,
    endpoint: Endpoint,
    connections: Arc<RwLock<HashMap<String, MeshConnection>>>,
    event_tx: broadcast::Sender<QuicMeshEvent>,
    forward_handler: Arc<RwLock<Option<ForwardHandler>>>,
    shutdown: CancellationToken,
    /// Bezpieczenstwo mesh — szyfrowanie, parowanie, filtrowanie zaufanych nodow
    security: Option<Arc<MeshSecurity>>,
    /// Executor komend zarzadzania od zdalnych nodow
    command_executor: Option<Arc<MeshCommandExecutor>>,
    /// Rejestr serwisow ze wszystkich nodow mesh
    service_registry: Arc<MeshServiceRegistry>,
    /// Oczekujace odpowiedzi na komendy: command_id -> oneshot sender
    pending_commands: Arc<RwLock<HashMap<String, tokio::sync::oneshot::Sender<CommandResponse>>>>,
    /// Mapowanie command_id -> target_node_id (do czyszczenia przy disconnect)
    command_to_node: Arc<RwLock<HashMap<String, String>>>,
    /// Zbior node_id dla ktorych juz dziala reconnect loop (deduplikacja)
    reconnecting: Arc<RwLock<std::collections::HashSet<String>>>,
}

impl QuicMeshManager {
    /// Tworzy nowy QuicMeshManager z self-signed TLS.
    ///
    /// Bind na 0.0.0.0:{listen_port}. ALPN: "tentaflow-mesh".
    /// Parametr `security` wlacza filtrowanie zaufanych nodow i szyfrowanie ChaCha20-Poly1305.
    pub fn new(config: QuicMeshConfig, security: Option<Arc<MeshSecurity>>) -> Result<Arc<Self>> {
        let (server_crypto, client_crypto) = Self::build_mesh_tls()?;

        let mut server_config = QuinnServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto)
                .context("Nie udalo sie utworzyc QuicServerConfig")?,
        ));

        let mut transport = quinn::TransportConfig::default();
        transport.max_concurrent_bidi_streams(64u32.into());
        transport.max_concurrent_uni_streams(128u32.into());
        transport.max_idle_timeout(Some(
            Duration::from_secs(60)
                .try_into()
                .context("Nieprawidlowy idle timeout")?,
        ));
        transport.keep_alive_interval(Some(Duration::from_secs(10)));
        server_config.transport_config(Arc::new(transport));

        let bind_addr: SocketAddr = format!("0.0.0.0:{}", config.listen_port)
            .parse()
            .context("Nieprawidlowy adres bind")?;

        let mut endpoint = Endpoint::server(server_config, bind_addr)
            .context(format!("Nie udalo sie utworzyc QUIC endpoint na {} (UDP) — sprawdz czy port nie jest zajety przez inny proces", bind_addr))?;

        // Konfiguracja klienta
        let mut quinn_client_config = ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(client_crypto)
                .context("Nie udalo sie utworzyc QuicClientConfig")?,
        ));

        let mut client_transport = quinn::TransportConfig::default();
        client_transport.max_concurrent_bidi_streams(64u32.into());
        client_transport.max_concurrent_uni_streams(128u32.into());
        client_transport.max_idle_timeout(Some(
            Duration::from_secs(60)
                .try_into()
                .context("Nieprawidlowy idle timeout klienta")?,
        ));
        client_transport.keep_alive_interval(Some(Duration::from_secs(10)));
        quinn_client_config.transport_config(Arc::new(client_transport));

        endpoint.set_default_client_config(quinn_client_config);

        let (event_tx, _) = broadcast::channel(512);

        let command_executor = security.as_ref().map(|sec| {
            Arc::new(MeshCommandExecutor::new(Arc::clone(sec)))
        });

        let service_registry = Arc::new(MeshServiceRegistry::new(config.node_id.clone()));

        Ok(Arc::new(Self {
            node_id: config.node_id.clone(),
            config,
            endpoint,
            connections: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
            forward_handler: Arc::new(RwLock::new(None)),
            shutdown: CancellationToken::new(),
            security,
            command_executor,
            service_registry,
            pending_commands: Arc::new(RwLock::new(HashMap::new())),
            command_to_node: Arc::new(RwLock::new(HashMap::new())),
            reconnecting: Arc::new(RwLock::new(std::collections::HashSet::new())),
        }))
    }

    /// Uruchamia taski: accept_loop, connection_monitor
    pub fn start(self: &Arc<Self>) -> Vec<JoinHandle<()>> {
        let mut handles = Vec::with_capacity(2);

        let this = Arc::clone(self);
        handles.push(tokio::spawn(async move { this.accept_loop().await }));

        let this = Arc::clone(self);
        handles.push(tokio::spawn(async move { this.connection_monitor().await }));

        info!(node_id = %self.node_id, port = self.config.listen_port, "QuicMeshManager uruchomiony");

        handles
    }

    /// Identyfikator tego noda
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Subskrypcja zdarzen mesh QUIC
    pub fn subscribe(&self) -> broadcast::Receiver<QuicMeshEvent> {
        self.event_tx.subscribe()
    }

    /// Token anulowania — do graceful shutdown
    pub fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    /// Ustawia handler dla forward requestow — wywolywany gdy peer wysyla komende
    pub async fn set_forward_handler(&self, handler: ForwardHandler) {
        *self.forward_handler.write().await = Some(handler);
    }

    /// Wysyla NodeLeaving do wszystkich trusted peerow i zamyka polaczenia
    pub async fn send_node_leaving(&self) {
        let payload = NodeLeavingPayload {
            node_id: self.node_id.clone(),
        };
        let data = rkyv::to_bytes::<rkyv::rancor::Error>(&payload)
            .map(|v| v.to_vec())
            .unwrap_or_default();
        self.broadcast_to_trusted(MESH_MSG_NODE_LEAVING, &data, None).await;

        let mut conns = self.connections.write().await;
        for (_, conn) in conns.drain() {
            conn.connection.close(0u32.into(), b"leaving");
        }
    }

    /// Zamyka endpoint i wszystkie polaczenia
    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        let conns = self.connections.read().await;
        for mc in conns.values() {
            mc.connection.close(0u32.into(), b"shutdown");
        }
        drop(conns);
        self.endpoint.close(0u32.into(), b"shutdown");
        self.endpoint.wait_idle().await;
        info!(node_id = %self.node_id, "QuicMeshManager zamkniety");
    }

    // =========================================================================
    // Polaczenia wychodzace
    // =========================================================================

    /// Laczy sie z peerem. Jesli polaczenie juz istnieje, zwraca Ok.
    /// Oba nody moga inicjowac — duplikaty wykrywane po handshake (TOCTOU check).
    pub async fn connect_to_peer(&self, node_id: &str, addr: SocketAddr) -> Result<()> {
        // Nie lacze sie z samym soba
        if self.node_id.as_str() == node_id {
            return Ok(());
        }

        // Sprawdz czy juz polaczony — jesli connection martwe, usun i polacz ponownie
        {
            let mut conns = self.connections.write().await;
            if let Some(mc) = conns.get(node_id) {
                if mc.connection.close_reason().is_some() {
                    info!(peer_id = %node_id, "Stare martwe polaczenie — usuwam, probuje nowe");
                    conns.remove(node_id);
                } else {
                    return Ok(());
                }
            }
        }

        info!(peer_id = %node_id, addr = %addr, "QUIC connect_to_peer START");

        let connection = tokio::time::timeout(
            Duration::from_secs(5),
            self.endpoint
                .connect(addr, "tentaflow-mesh")
                .context("Nie udalo sie zainicjowac QUIC connect")?
        ).await
            .map_err(|_| anyhow::anyhow!("QUIC connect timeout (5s) do {}", addr))?
            .context("QUIC handshake nieudany")?;

        // Sprawdz czy peer jest trusted — jesli nie, wyslij tylko node_id bez danych
        let peer_is_trusted = match &self.security {
            Some(sec) => sec.is_trusted(node_id),
            // VULN-015: Zero trust — brak security = BLOKUJ polaczenie
            None => {
                error!("MeshSecurity niedostepny — ODRZUCAM polaczenie (zero trust)");
                false
            }
        };

        // Wymiana FullState: otworz bidi-stream, wyslij discriminant 0x12
        let (mut send, mut recv) = connection
            .open_bi()
            .await
            .context("Nie udalo sie otworzyc bidi-stream do wymiany stanu")?;

        // Wyslij swoj node_id jako identyfikacje
        let id_bytes = self.node_id.as_bytes();
        let id_len = (id_bytes.len() as u32).to_be_bytes();
        send.write_all(&[MESH_MSG_FULL_STATE])
            .await
            .context("Blad wysylania discriminant")?;
        send.write_all(&id_len)
            .await
            .context("Blad wysylania dlugosci node_id")?;
        send.write_all(id_bytes)
            .await
            .context("Blad wysylania node_id")?;
        send.finish().context("Blad zamykania send stream")?;

        if !peer_is_trusted {
            debug!(peer_id = %node_id, "Peer niezaufany — polaczenie bez wymiany FullState");
        }

        // Odbierz FullState od peera (max 10MB)
        let state_bytes = recv
            .read_to_end(10 * 1024 * 1024)
            .await
            .context("Blad odczytu FullState od peera")?;

        if !state_bytes.is_empty() {
            let _ = self.event_tx.send(QuicMeshEvent::FullStateReceived {
                node_id: node_id.to_string(),
                state: state_bytes,
            });
        }

        // Zapisz polaczenie (sprawdz ponownie — inny task mogl polaczyc w miedzyczasie)
        {
            let mut conns = self.connections.write().await;
            if conns.contains_key(node_id) {
                connection.close(0u32.into(), b"duplicate");
                return Ok(());
            }
            conns.insert(
                node_id.to_string(),
                MeshConnection {
                    connection: connection.clone(),
                },
            );
        }

        let _ = self.event_tx.send(QuicMeshEvent::PeerConnected {
            node_id: node_id.to_string(),
        });

        debug!(peer_id = %node_id, "Polaczenie mesh nawiazane (initiator)");

        // Spawn handler dla przychodzacych streamow
        let peer_id = node_id.to_string();
        let this = self.connections.clone();
        let event_tx = self.event_tx.clone();
        let fh = self.forward_handler.clone();
        let shutdown = self.shutdown.clone();
        let security = self.security.clone();
        tokio::spawn(async move {
            Self::handle_peer_streams(connection, peer_id.clone(), event_tx.clone(), fh, shutdown, security)
                .await;
            // Polaczenie zamkniete — usun z mapy
            {
                let mut conns = this.write().await;
                conns.remove(&peer_id);
            }
            let _ = event_tx.send(QuicMeshEvent::PeerDisconnected {
                node_id: peer_id,
            });
        });

        Ok(())
    }

    // =========================================================================
    // Petla akceptowania polaczen
    // =========================================================================

    async fn accept_loop(&self) {
        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => {
                    debug!("accept_loop: shutdown");
                    break;
                }
                incoming = self.endpoint.accept() => {
                    match incoming {
                        Some(inc) => {
                            let connections = self.connections.clone();
                            let event_tx = self.event_tx.clone();
                            let fh = self.forward_handler.clone();
                            let shutdown = self.shutdown.clone();
                            let self_node_id = self.node_id.clone();

                            let security = self.security.clone();
                            tokio::spawn(async move {
                                Self::handle_incoming(
                                    inc,
                                    self_node_id,
                                    connections,
                                    event_tx,
                                    fh,
                                    shutdown,
                                    security,
                                )
                                .await;
                            });
                        }
                        None => {
                            debug!("accept_loop: endpoint zamkniety");
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Obsluguje polaczenie przychodzace — odczytuje node_id peera i waliduje regule
    async fn handle_incoming(
        incoming: quinn::Incoming,
        self_node_id: String,
        connections: Arc<RwLock<HashMap<String, MeshConnection>>>,
        event_tx: broadcast::Sender<QuicMeshEvent>,
        forward_handler: Arc<RwLock<Option<ForwardHandler>>>,
        shutdown: CancellationToken,
        security: Option<Arc<MeshSecurity>>,
    ) {
        let connection = match incoming.await {
            Ok(c) => c,
            Err(e) => {
                debug!("Polaczenie przychodzace nieudane: {}", e);
                return;
            }
        };

        // Czekaj na bidi-stream z identyfikacja peera (max 10s)
        let (mut send, mut recv) = match tokio::time::timeout(
            Duration::from_secs(10),
            connection.accept_bi(),
        ).await {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                error!("Blad akceptowania bidi-stream: {}", e);
                return;
            }
            Err(_) => {
                warn!("Timeout oczekiwania na bidi-stream od peera");
                connection.close(0u32.into(), b"accept_bi timeout");
                return;
            }
        };

        // Odczytaj: discriminant(1) + len(4) + node_id(N)
        let mut disc = [0u8; 1];
        if let Err(e) = recv.read_exact(&mut disc).await {
            error!("Blad odczytu discriminant: {}", e);
            return;
        }

        if disc[0] != MESH_MSG_FULL_STATE {
            error!(
                "Oczekiwano discriminant 0x{:02X}, otrzymano 0x{:02X}",
                MESH_MSG_FULL_STATE, disc[0]
            );
            return;
        }

        let mut len_buf = [0u8; 4];
        if let Err(e) = recv.read_exact(&mut len_buf).await {
            error!("Blad odczytu dlugosci node_id: {}", e);
            return;
        }
        let id_len = u32::from_be_bytes(len_buf) as usize;
        if id_len > 1024 {
            error!("node_id zbyt dlugi: {}", id_len);
            return;
        }

        let mut id_buf = vec![0u8; id_len];
        if let Err(e) = recv.read_exact(&mut id_buf).await {
            error!("Blad odczytu node_id: {}", e);
            return;
        }

        let peer_node_id = match String::from_utf8(id_buf) {
            Ok(s) => s,
            Err(e) => {
                error!("Nieprawidlowy UTF-8 w node_id: {}", e);
                return;
            }
        };

        // Nie akceptuj polaczenia od samego siebie
        if peer_node_id == self_node_id {
            connection.close(1u32.into(), b"self-connection");
            return;
        }

        // Wyslij swoj FullState jako odpowiedz — TYLKO jesli peer jest trusted
        // Brak security → zero trust — odrzuc polaczenie
        let peer_is_trusted = match &security {
            Some(sec) => sec.is_trusted(&peer_node_id),
            // VULN-015: Zero trust — brak security = ODRZUCAM polaczenie
            None => {
                error!("MeshSecurity niedostepny — ODRZUCAM polaczenie (zero trust)");
                false
            }
        };

        if peer_is_trusted {
            // Peer zaufany — wyslij pusty FullState (caller wypelni przez event)
            if let Err(e) = send.write_all(&[]).await {
                error!("Blad wysylania pustego FullState: {}", e);
            }
        } else {
            // Peer niezaufany — nie wysylaj zadnych danych stanu
            debug!(peer_id = %peer_node_id, "Peer niezaufany — nie wysylam FullState");
        }
        if let Err(e) = send.finish() {
            error!("Blad zamykania send stream: {}", e);
        }

        // Zapisz polaczenie (sprawdz duplikat — oba nody moga inicjowac jednoczesnie)
        {
            let mut conns = connections.write().await;
            if conns.contains_key(&peer_node_id) {
                debug!(peer_id = %peer_node_id, "Duplikat polaczenia w accept — zamykam nowe");
                connection.close(0u32.into(), b"duplicate");
                return;
            }
            conns.insert(
                peer_node_id.clone(),
                MeshConnection {
                    connection: connection.clone(),
                },
            );
        }

        let _ = event_tx.send(QuicMeshEvent::PeerConnected {
            node_id: peer_node_id.clone(),
        });

        debug!(peer_id = %peer_node_id, "Polaczenie mesh przyjete (acceptor)");

        // Obsluguj streamy peera
        Self::handle_peer_streams(
            connection,
            peer_node_id.clone(),
            event_tx.clone(),
            forward_handler,
            shutdown,
            security,
        )
        .await;

        // Po zamknieciu — usun z mapy
        {
            let mut conns = connections.write().await;
            conns.remove(&peer_node_id);
        }

        let _ = event_tx.send(QuicMeshEvent::PeerDisconnected {
            node_id: peer_node_id,
        });
    }

    // =========================================================================
    // Obsluga streamow peera
    // =========================================================================

    /// Nasluchuje na uni i bidi streamy od peera az do zamkniecia polaczenia
    async fn handle_peer_streams(
        conn: quinn::Connection,
        peer_node_id: String,
        event_tx: broadcast::Sender<QuicMeshEvent>,
        forward_handler: Arc<RwLock<Option<ForwardHandler>>>,
        shutdown: CancellationToken,
        security: Option<Arc<MeshSecurity>>,
    ) {
        info!(peer_id = %peer_node_id, remote = %conn.remote_address(), "handle_peer_streams START");
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    debug!(peer_id = %peer_node_id, "handle_peer_streams: shutdown");
                    break;
                }
                uni = conn.accept_uni() => {
                    match uni {
                        Ok(recv) => {
                            let pid = peer_node_id.clone();
                            let tx = event_tx.clone();
                            let sec = security.clone();
                            tokio::spawn(async move {
                                Self::handle_uni_stream(recv, pid, tx, sec).await;
                            });
                        }
                        Err(e) => {
                            debug!(peer_id = %peer_node_id, "Uni-stream zakonczony: {}", e);
                            break;
                        }
                    }
                }
                bi = conn.accept_bi() => {
                    match bi {
                        Ok((send, recv)) => {
                            let pid = peer_node_id.clone();
                            let tx = event_tx.clone();
                            let fh = forward_handler.clone();
                            let sec = security.clone();
                            tokio::spawn(async move {
                                Self::handle_bidi_stream(send, recv, pid, tx, fh, sec).await;
                            });
                        }
                        Err(e) => {
                            debug!(peer_id = %peer_node_id, "Bidi-stream zakonczony: {}", e);
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Obsluguje uni-directional stream: discriminant + payload
    /// Filtruje niezaufanych peerow (poza wiadomosciami parowania 0x20-0x22).
    /// Deszyfruje payload ChaCha20-Poly1305 dla wiadomosci 0x10-0x18 od trusted peerow.
    async fn handle_uni_stream(
        mut recv: quinn::RecvStream,
        peer_node_id: String,
        event_tx: broadcast::Sender<QuicMeshEvent>,
        security: Option<Arc<MeshSecurity>>,
    ) {
        // Odczytaj 1 bajt discriminantu
        let mut disc_buf = [0u8; 1];
        if let Err(e) = recv.read_exact(&mut disc_buf).await {
            warn!(peer_id = %peer_node_id, "Blad odczytu discriminant uni-stream: {}", e);
            return;
        }
        let discriminant = disc_buf[0];
        info!(peer_id = %peer_node_id, disc = format!("0x{:02X}", discriminant), "Odebrano uni-stream");

        // Sprawdz zaufanie peera — wiadomosci parowania (0x20-0x22) zawsze przepuszczaj
        let is_pairing_msg = matches!(
            discriminant,
            MESH_MSG_PAIRING_REQUEST | MESH_MSG_PAIRING_CONFIRM | MESH_MSG_PAIRING_REJECT
        );
        if !is_pairing_msg {
            if let Some(ref sec) = security {
                if !sec.is_trusted(&peer_node_id) {
                    warn!(
                        peer_id = %peer_node_id,
                        disc = format!("0x{:02X}", discriminant),
                        "Odrzucono wiadomosc od niezaufanego peera"
                    );
                    return;
                }
            }
        }

        // Limit odczytu na podstawie typu wiadomosci
        let max_size = match discriminant {
            MESH_MSG_HEARTBEAT | MESH_MSG_NODE_INFO => 64 * 1024,
            MESH_MSG_CRDT_DELTA | MESH_MSG_MODEL_LIST | MESH_MSG_CONTAINER_LIST => 1024 * 1024,
            MESH_MSG_PAIRING_REQUEST | MESH_MSG_PAIRING_CONFIRM | MESH_MSG_PAIRING_REJECT
            | MESH_MSG_TRUST_REVOKED | MESH_MSG_KEY_ROTATION | MESH_MSG_KEY_ROTATION_RESPONSE
            | MESH_MSG_NODE_LEAVING => 4096,
            MESH_MSG_TRUSTED_KEYS_SYNC => 1024 * 1024,
            MESH_MSG_COMMAND | MESH_MSG_COMMAND_RESPONSE => 1024 * 1024,
            MESH_MSG_DEPLOY_PROGRESS | MESH_MSG_LOG_CHUNK => 256 * 1024,
            MESH_MSG_SERVICE_ANNOUNCE | MESH_MSG_SERVICE_QUERY_ALL | MESH_MSG_SERVICE_RESPONSE_ALL => 1024 * 1024,
            MESH_MSG_RELAY_FRAME => 2 * 1024 * 1024,
            _ => 64 * 1024,
        };

        let raw_payload = match recv.read_to_end(max_size).await {
            Ok(d) => d,
            Err(e) => {
                warn!(peer_id = %peer_node_id, "Blad odczytu uni-stream: {}", e);
                return;
            }
        };

        // Deszyfruj payload dla wiadomosci danych (0x10-0x18) od trusted peerow
        // Wiadomosci parowania (0x20-0x22) NIE sa szyfrowane — brak shared secret przed parowaniem
        let is_data_msg = (0x10..=0x18).contains(&discriminant)
            || discriminant == MESH_MSG_TRUST_REVOKED
            || discriminant == MESH_MSG_KEY_ROTATION
            || discriminant == MESH_MSG_KEY_ROTATION_RESPONSE
            || discriminant == MESH_MSG_TRUSTED_KEYS_SYNC
            || discriminant == MESH_MSG_NODE_LEAVING
            || (0x30..=0x37).contains(&discriminant);
        let payload = if is_data_msg {
            if let Some(ref sec) = security {
                if sec.has_shared_secret(&peer_node_id) {
                    match sec.decrypt_from_node(&peer_node_id, &raw_payload) {
                        Ok(decrypted) => decrypted,
                        Err(e) => {
                            // [CR-001] Odrzucenie wiadomosci — brak fallbacku na plaintext
                            warn!(
                                peer_id = %peer_node_id,
                                disc = format!("0x{:02X}", discriminant),
                                "Deszyfrowanie nie powiodlo sie od {}: {} — ODRZUCAM", peer_node_id, e
                            );
                            return;
                        }
                    }
                } else {
                    // [CR-001] Brak shared secret dla wiadomosci danych — odrzucenie
                    warn!(peer_id = %peer_node_id, "Brak shared secret — odrzucam wiadomosc danych");
                    return;
                }
            } else {
                raw_payload
            }
        } else {
            raw_payload
        };

        let event = match discriminant {
            MESH_MSG_HEARTBEAT => QuicMeshEvent::HeartbeatReceived {
                node_id: peer_node_id,
                heartbeat: payload,
            },
            MESH_MSG_CRDT_DELTA => QuicMeshEvent::CrdtDeltaReceived {
                node_id: peer_node_id,
                data: payload,
            },
            MESH_MSG_MODEL_LIST => QuicMeshEvent::ModelListUpdate {
                node_id: peer_node_id,
                data: payload,
            },
            MESH_MSG_CONTAINER_LIST => QuicMeshEvent::ContainerListUpdate {
                node_id: peer_node_id,
                data: payload,
            },
            MESH_MSG_NODE_INFO => QuicMeshEvent::NodeInfoReceived {
                node_id: peer_node_id,
                data: payload,
            },
            MESH_MSG_PAIRING_REQUEST => QuicMeshEvent::PairingRequestReceived {
                peer_id: peer_node_id,
                data: payload,
            },
            MESH_MSG_PAIRING_CONFIRM => QuicMeshEvent::PairingConfirmReceived {
                peer_id: peer_node_id,
                data: payload,
            },
            MESH_MSG_PAIRING_REJECT => QuicMeshEvent::PairingRejectReceived {
                peer_id: peer_node_id,
                data: payload,
            },
            MESH_MSG_COMMAND => QuicMeshEvent::MeshCommandReceived {
                from_node_id: peer_node_id.clone(),
                command: payload,
            },
            MESH_MSG_COMMAND_RESPONSE => QuicMeshEvent::MeshCommandResponseReceived {
                from_node_id: peer_node_id,
                data: payload,
            },
            MESH_MSG_DEPLOY_PROGRESS => QuicMeshEvent::MeshDeployProgressReceived {
                from_node_id: peer_node_id,
                data: payload,
            },
            MESH_MSG_LOG_CHUNK => QuicMeshEvent::MeshLogChunkReceived {
                from_node_id: peer_node_id,
                data: payload,
            },
            MESH_MSG_SERVICE_ANNOUNCE => QuicMeshEvent::ServiceAnnounceReceived {
                node_id: peer_node_id,
                data: payload,
            },
            MESH_MSG_SERVICE_QUERY_ALL => QuicMeshEvent::ServiceQueryAllReceived {
                from_node_id: peer_node_id,
                data: payload,
            },
            MESH_MSG_SERVICE_RESPONSE_ALL => QuicMeshEvent::ServiceResponseAllReceived {
                from_node_id: peer_node_id,
                data: payload,
            },
            MESH_MSG_TRUST_REVOKED => {
                match rkyv::from_bytes::<TrustRevokedPayload, rkyv::rancor::Error>(&payload) {
                    Ok(msg) => {
                        if msg.revoked_node_id.is_empty() {
                            warn!(peer_id = %peer_node_id, "TrustRevoked bez revoked_node_id");
                            return;
                        }
                        QuicMeshEvent::TrustRevokedReceived {
                            node_id: peer_node_id,
                            revoked_node_id: msg.revoked_node_id,
                        }
                    }
                    Err(e) => {
                        warn!(peer_id = %peer_node_id, "Blad deserializacji TrustRevoked rkyv: {}", e);
                        return;
                    }
                }
            }
            MESH_MSG_KEY_ROTATION => {
                match rkyv::from_bytes::<KeyRotationPayload, rkyv::rancor::Error>(&payload) {
                    Ok(msg) => {
                        if msg.from_node_id != peer_node_id {
                            warn!(peer_id = %peer_node_id, claimed = %msg.from_node_id, "KeyRotation: from_node_id nie zgadza sie z peer transport ID");
                            return;
                        }
                        if msg.ephemeral_public_key.is_empty() {
                            warn!(peer_id = %peer_node_id, "KeyRotation bez ephemeral_public_key");
                            return;
                        }
                        QuicMeshEvent::KeyRotationReceived {
                            node_id: peer_node_id,
                            ephemeral_public_key_hex: msg.ephemeral_public_key,
                        }
                    }
                    Err(e) => {
                        warn!(peer_id = %peer_node_id, "Blad deserializacji KeyRotation rkyv: {}", e);
                        return;
                    }
                }
            }
            MESH_MSG_TRUSTED_KEYS_SYNC => {
                match rkyv::from_bytes::<TrustedKeysSyncPayload, rkyv::rancor::Error>(&payload) {
                    Ok(msg) => {
                        let keys: Vec<(String, String)> = msg.keys
                            .into_iter()
                            .map(|entry| (entry.node_id, entry.public_key_hex))
                            .collect();
                        if keys.is_empty() {
                            debug!(peer_id = %peer_node_id, "TrustedKeysSync — pusta lista kluczy");
                            return;
                        }
                        QuicMeshEvent::TrustedKeysSyncReceived {
                            node_id: peer_node_id,
                            keys,
                        }
                    }
                    Err(e) => {
                        warn!(peer_id = %peer_node_id, "Blad deserializacji TrustedKeysSync rkyv: {}", e);
                        return;
                    }
                }
            }
            MESH_MSG_KEY_ROTATION_RESPONSE => {
                match rkyv::from_bytes::<KeyRotationResponsePayload, rkyv::rancor::Error>(&payload) {
                    Ok(msg) => {
                        if msg.from_node_id != peer_node_id {
                            warn!(peer_id = %peer_node_id, claimed = %msg.from_node_id, "KeyRotationResponse: from_node_id nie zgadza sie z peer transport ID");
                            return;
                        }
                        if msg.ephemeral_public_key.is_empty() {
                            warn!(peer_id = %peer_node_id, "KeyRotationResponse bez ephemeral_public_key");
                            return;
                        }
                        QuicMeshEvent::KeyRotationResponseReceived {
                            node_id: peer_node_id,
                            ephemeral_public_key_hex: msg.ephemeral_public_key,
                        }
                    }
                    Err(e) => {
                        warn!(peer_id = %peer_node_id, "Blad deserializacji KeyRotationResponse rkyv: {}", e);
                        return;
                    }
                }
            }
            MESH_MSG_NODE_LEAVING => {
                QuicMeshEvent::NodeLeavingReceived {
                    node_id: peer_node_id,
                }
            }
            MESH_MSG_RELAY_FRAME => {
                match rkyv::from_bytes::<MeshRelayFrame, rkyv::rancor::Error>(&payload) {
                    Ok(frame) => {
                        QuicMeshEvent::RelayFrameReceived {
                            from_node_id: peer_node_id,
                            frame,
                        }
                    }
                    Err(e) => {
                        warn!(peer_id = %peer_node_id, "Blad deserializacji MeshRelayFrame: {}", e);
                        return;
                    }
                }
            }
            MESH_MSG_CLUSTER_INFO => {
                debug!(peer_id = %peer_node_id, "ClusterInfo otrzymany, obsluga w przyszlej wersji");
                return;
            }
            _ => {
                warn!(peer_id = %peer_node_id, "Nieznany discriminant uni-stream: 0x{:02X}", discriminant);
                return;
            }
        };

        let _ = event_tx.send(event);
    }

    /// Obsluguje bidi-directional stream (forward request)
    /// Filtruje niezaufanych peerow. Deszyfruje request i szyfruje odpowiedz.
    async fn handle_bidi_stream(
        mut send: quinn::SendStream,
        mut recv: quinn::RecvStream,
        peer_node_id: String,
        _event_tx: broadcast::Sender<QuicMeshEvent>,
        forward_handler: Arc<RwLock<Option<ForwardHandler>>>,
        security: Option<Arc<MeshSecurity>>,
    ) {
        // Sprawdz zaufanie peera — bidi-stream wymaga trusted
        if let Some(ref sec) = security {
            if !sec.is_trusted(&peer_node_id) {
                warn!(peer_id = %peer_node_id, "Odrzucono bidi-stream od niezaufanego peera");
                let _ = send.finish();
                return;
            }
        }

        let raw_data = match recv.read_to_end(10 * 1024 * 1024).await {
            Ok(d) => d,
            Err(e) => {
                warn!(peer_id = %peer_node_id, "Blad odczytu bidi-stream: {}", e);
                return;
            }
        };

        if raw_data.is_empty() {
            return;
        }

        // Deszyfruj dane — wymagane dla trusted peerow
        let data = if let Some(ref sec) = security {
            if sec.has_shared_secret(&peer_node_id) {
                match sec.decrypt_from_node(&peer_node_id, &raw_data) {
                    Ok(decrypted) => decrypted,
                    Err(e) => {
                        // [CR-001] Odrzucenie — brak fallbacku na plaintext
                        warn!(
                            peer_id = %peer_node_id,
                            "Deszyfrowanie bidi-stream nie powiodlo sie od {}: {} — ODRZUCAM", peer_node_id, e
                        );
                        return;
                    }
                }
            } else {
                // [CR-001] Brak shared secret dla bidi-stream — odrzucenie
                warn!(peer_id = %peer_node_id, "Brak shared secret dla bidi-stream — ODRZUCAM");
                let _ = send.finish();
                return;
            }
        } else {
            raw_data
        };

        let discriminant = data[0];
        let payload = data[1..].to_vec();

        match discriminant {
            MESH_MSG_FORWARD_REQ => {
                // Parsuj request_id: pierwsze 4 bajty = dlugosc, potem request_id, reszta payload
                if payload.len() < 4 {
                    warn!(peer_id = %peer_node_id, "ForwardRequest za krotki");
                    return;
                }
                let req_id_len = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
                if payload.len() < 4 + req_id_len {
                    warn!(peer_id = %peer_node_id, "ForwardRequest: nieprawidlowa dlugosc request_id");
                    return;
                }
                let _request_id = match String::from_utf8(payload[4..4 + req_id_len].to_vec()) {
                    Ok(s) => s,
                    Err(_) => {
                        warn!(peer_id = %peer_node_id, "ForwardRequest: nieprawidlowy request_id");
                        return;
                    }
                };
                let req_payload = payload[4 + req_id_len..].to_vec();

                // Klonuj Arc handlera i zwolnij lock przed wywolaniem
                let handler = {
                    let guard = forward_handler.read().await;
                    guard.clone()
                };

                let response = if let Some(h) = handler {
                    match tokio::time::timeout(Duration::from_secs(600), h(req_payload)).await {
                        Ok(resp) => resp,
                        Err(_) => {
                            warn!(peer_id = %peer_node_id, "Timeout wykonywania komendy forward (600s)");
                            br#"{"error":"Timeout wykonywania komendy (600s)"}"#.to_vec()
                        }
                    }
                } else {
                    vec![]
                };

                // [CR-001] Szyfruj odpowiedz — wymagane, brak fallbacku na plaintext
                let response_data = if let Some(ref sec) = security {
                    if sec.has_shared_secret(&peer_node_id) {
                        match sec.encrypt_for_node(&peer_node_id, &response) {
                            Ok(encrypted) => encrypted,
                            Err(e) => {
                                warn!(peer_id = %peer_node_id, "Blad szyfrowania odpowiedzi forward: {} — ODRZUCAM", e);
                                return;
                            }
                        }
                    } else {
                        warn!(peer_id = %peer_node_id, "Brak shared secret — nie wysylam odpowiedzi forward");
                        return;
                    }
                } else {
                    response
                };

                let _ = send.write_all(&response_data).await;
                let _ = send.finish();
            }
            other => {
                warn!(peer_id = %peer_node_id, "Nieznany discriminant bidi-stream: 0x{:02X}", other);
            }
        }
    }

    /// Wysyla heartbeat z podanymi danymi do wszystkich polaczonych i zaufanych peerow.
    /// Szyfruje payload ChaCha20-Poly1305 jesli dostepny shared secret.
    ///
    /// [OPT] Optymalizacje pod 1000 peerow:
    /// 1. Batch trust check: pobiera snapshot trusted_node_ids RAZ (1 Arc::clone)
    ///    zamiast 1000 read lockow RwLock na trusted_keys.
    /// 2. Parallel send: tokio::JoinSet zamiast sekwencyjnego wysylania.
    ///    Przy 1000 peerach: sekwencyjne = 1000 * (encrypt + QUIC send) = wolne.
    ///    Parallel = wszystkie szyfrowania + QUIC send jednoczesnie.
    /// 3. Pojedynczy lock na connections — lista pobierana raz, potem send bez locka.
    async fn send_heartbeat_to_all(&self, data: &[u8]) {
        let security = match &self.security {
            Some(sec) => sec,
            None => {
                debug!("Brak modulu MeshSecurity — pomijam heartbeat broadcast");
                return;
            }
        };

        // [OPT] Batch trust check — jeden Arc::clone zamiast 1000 lockow
        let trusted_set = security.trusted_node_ids_snapshot();

        // Jeden lock na connections — pobierz liste (peer_id, connection) dla trusted peerow
        let trusted_connections: Vec<(String, quinn::Connection)> = {
            let conns = self.connections.read().await;
            conns.iter()
                .filter(|(id, _)| trusted_set.contains(id.as_str()))
                .map(|(id, mc)| (id.clone(), mc.connection.clone()))
                .collect()
        };

        if trusted_connections.is_empty() {
            return;
        }

        // [OPT] Parallel send — tokio::JoinSet dla rownoczesnego wysylania
        let mut join_set = tokio::task::JoinSet::new();
        let security_arc = Arc::clone(security);
        let data_arc = Arc::from(data);

        for (peer_id, conn) in trusted_connections {
            let sec = Arc::clone(&security_arc);
            let payload = Arc::clone(&data_arc);
            join_set.spawn(async move {
                if let Err(e) = Self::send_uni_message_encrypted(
                    &conn, MESH_MSG_HEARTBEAT, &payload, &peer_id, &sec,
                ).await {
                    warn!(peer_id = %peer_id, "Blad wysylania heartbeat: {}", e);
                }
            });
        }

        // Czekaj na zakonczenie wszystkich wysylek
        while join_set.join_next().await.is_some() {}
    }

    /// Wysyla heartbeat (serializowany na zewnatrz) do wszystkich peerow
    pub async fn send_heartbeat_data(&self, data: &[u8]) {
        self.send_heartbeat_to_all(data).await;
    }

    /// Wysyla NodeInfo do konkretnego peera przez uni-stream.
    /// Sprawdza zaufanie peera i szyfruje payload jesli dostepny shared secret.
    pub async fn send_node_info(&self, node_id: &str, data: &[u8]) -> Result<()> {
        // Sprawdz zaufanie
        if let Some(ref sec) = self.security {
            if !sec.is_trusted(node_id) {
                return Err(anyhow::anyhow!("Peer {} nie jest zaufany — odmowa wyslania NodeInfo", node_id));
            }
        }
        let conn = {
            let conns = self.connections.read().await;
            conns
                .get(node_id)
                .map(|mc| mc.connection.clone())
                .ok_or_else(|| anyhow::anyhow!("Brak polaczenia z peerem: {}", node_id))?
        };
        // Szyfruj jesli mamy security
        if let Some(ref sec) = self.security {
            Self::send_uni_message_encrypted(&conn, MESH_MSG_NODE_INFO, data, node_id, sec).await
        } else {
            Self::send_uni_message(&conn, MESH_MSG_NODE_INFO, data).await
        }
    }

    /// Wysyla PairingRequest do konkretnego peera (0x20)
    pub async fn send_pairing_request(&self, node_id: &str, data: &[u8]) -> Result<()> {
        let conn = {
            let conns = self.connections.read().await;
            conns
                .get(node_id)
                .map(|mc| mc.connection.clone())
                .ok_or_else(|| anyhow::anyhow!("Brak polaczenia z peerem: {}", node_id))?
        };
        Self::send_uni_message(&conn, MESH_MSG_PAIRING_REQUEST, data).await
    }

    /// Wysyla PairingConfirm do konkretnego peera (0x21)
    pub async fn send_pairing_confirm(&self, node_id: &str, data: &[u8]) -> Result<()> {
        let conn = {
            let conns = self.connections.read().await;
            conns
                .get(node_id)
                .map(|mc| mc.connection.clone())
                .ok_or_else(|| anyhow::anyhow!("Brak polaczenia z peerem: {}", node_id))?
        };
        Self::send_uni_message(&conn, MESH_MSG_PAIRING_CONFIRM, data).await
    }

    /// Wysyla PairingReject do konkretnego peera (0x22)
    pub async fn send_pairing_reject(&self, node_id: &str, data: &[u8]) -> Result<()> {
        let conn = {
            let conns = self.connections.read().await;
            conns
                .get(node_id)
                .map(|mc| mc.connection.clone())
                .ok_or_else(|| anyhow::anyhow!("Brak polaczenia z peerem: {}", node_id))?
        };
        Self::send_uni_message(&conn, MESH_MSG_PAIRING_REJECT, data).await
    }

    /// Wysyla wiadomosc do konkretnego peera — szyfruje jesli dostepny shared secret
    pub async fn send_to_peer(&self, target_node_id: &str, discriminant: u8, data: &[u8]) -> Result<()> {
        let conn = {
            let conns = self.connections.read().await;
            conns
                .get(target_node_id)
                .map(|mc| mc.connection.clone())
                .ok_or_else(|| anyhow::anyhow!("Brak polaczenia z peerem: {}", target_node_id))?
        };
        if let Some(ref sec) = self.security {
            Self::send_uni_message_encrypted(&conn, discriminant, data, target_node_id, sec).await
        } else {
            Self::send_uni_message(&conn, discriminant, data).await
        }
    }

    /// Wysyla TrustRevoked do konkretnego peera (0x23)
    pub async fn send_trust_revoked(&self, target_node_id: &str, data: &[u8]) -> Result<()> {
        self.send_to_peer(target_node_id, MESH_MSG_TRUST_REVOKED, data).await
    }

    /// Wysyla KeyRotation do konkretnego peera (0x25)
    pub async fn send_key_rotation(&self, target_node_id: &str, data: &[u8]) -> Result<()> {
        self.send_to_peer(target_node_id, MESH_MSG_KEY_ROTATION, data).await
    }

    /// Wysyla TrustedKeysSync do konkretnego peera (0x24)
    pub async fn send_trusted_keys_sync(&self, target_node_id: &str, data: &[u8]) -> Result<()> {
        self.send_to_peer(target_node_id, MESH_MSG_TRUSTED_KEYS_SYNC, data).await
    }

    /// Wysyla KeyRotationResponse do konkretnego peera (0x26)
    pub async fn send_key_rotation_response(&self, target_node_id: &str, data: &[u8]) -> Result<()> {
        self.send_to_peer(target_node_id, MESH_MSG_KEY_ROTATION_RESPONSE, data).await
    }

    /// Wyslij relay frame do next-hop peera
    pub async fn send_relay_frame(&self, next_hop_id: &str, frame_bytes: &[u8]) -> Result<()> {
        self.send_to_peer(next_hop_id, MESH_MSG_RELAY_FRAME, frame_bytes).await
    }

    /// Wyslij wiadomosc do noda przez relay (multi-hop)
    pub async fn send_via_relay(
        &self,
        destination_node_id: &str,
        discriminant: u8,
        payload: &[u8],
        source_node_id: &str,
        peer_store: &crate::mesh::peer_store::MeshPeerStore,
    ) -> Result<()> {
        let route = peer_store.get_route(destination_node_id)
            .ok_or_else(|| anyhow::anyhow!("Brak route do {}", destination_node_id))?;

        // Payload jest juz zaszyfrowany end-to-end kluczem destination przez callera
        let frame = MeshRelayFrame {
            request_id: uuid::Uuid::new_v4().to_string(),
            source_node_id: source_node_id.to_string(),
            destination_node_id: destination_node_id.to_string(),
            ttl: 4,
            discriminant,
            payload: payload.to_vec(),
        };

        let frame_bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&frame)
            .map(|v| v.to_vec())
            .context("Blad serializacji MeshRelayFrame")?;

        self.send_to_peer(&route.next_hop, MESH_MSG_RELAY_FRAME, &frame_bytes).await
    }

    /// Broadcast wiadomosci do wszystkich polaczonych i zaufanych peerow.
    /// Pomija peera podanego w `exclude_node_id`. Zwraca wyniki per peer.
    pub async fn broadcast_to_trusted(
        &self,
        discriminant: u8,
        data: &[u8],
        exclude_node_id: Option<&str>,
    ) -> Vec<(String, Result<()>)> {
        let trusted_connections = self.collect_trusted_connections().await;
        let mut results = Vec::with_capacity(trusted_connections.len());

        for (peer_id, conn) in trusted_connections {
            if let Some(excl) = exclude_node_id {
                if peer_id == excl {
                    continue;
                }
            }
            let result = if let Some(ref sec) = self.security {
                Self::send_uni_message_encrypted(&conn, discriminant, data, &peer_id, sec).await
            } else {
                Self::send_uni_message(&conn, discriminant, data).await
            };
            results.push((peer_id, result));
        }

        results
    }

    /// Wysyla NodeInfo do wszystkich polaczonych i zaufanych peerow.
    /// Szyfruje payload jesli dostepny shared secret.
    /// [OPT] Parallel send + batch trust check — jak send_heartbeat_to_all.
    pub async fn broadcast_node_info(&self, data: &[u8]) {
        let trusted_connections = self.collect_trusted_connections().await;
        if trusted_connections.is_empty() {
            return;
        }

        let mut join_set = tokio::task::JoinSet::new();
        let data_arc = Arc::from(data);

        for (peer_id, conn) in trusted_connections {
            let sec = self.security.clone();
            let payload = Arc::clone(&data_arc);
            join_set.spawn(async move {
                let result = if let Some(ref sec) = sec {
                    Self::send_uni_message_encrypted(&conn, MESH_MSG_NODE_INFO, &payload, &peer_id, sec).await
                } else {
                    Self::send_uni_message(&conn, MESH_MSG_NODE_INFO, &payload).await
                };
                if let Err(e) = result {
                    warn!(peer_id = %peer_id, "Blad wysylania NodeInfo: {}", e);
                }
            });
        }

        while join_set.join_next().await.is_some() {}
    }

    /// Broadcast CRDT delta do wszystkich zaufanych peerow.
    /// Szyfruje payload jesli dostepny shared secret.
    /// [OPT] Parallel send + batch trust check — jak send_heartbeat_to_all.
    pub async fn broadcast_crdt_delta(&self, data: Vec<u8>) {
        let trusted_connections = self.collect_trusted_connections().await;
        if trusted_connections.is_empty() {
            return;
        }

        let mut join_set = tokio::task::JoinSet::new();
        let data_arc: Arc<[u8]> = Arc::from(data.as_slice());

        for (peer_id, conn) in trusted_connections {
            let sec = self.security.clone();
            let payload = Arc::clone(&data_arc);
            join_set.spawn(async move {
                let result = if let Some(ref sec) = sec {
                    Self::send_uni_message_encrypted(&conn, MESH_MSG_CRDT_DELTA, &payload, &peer_id, sec).await
                } else {
                    Self::send_uni_message(&conn, MESH_MSG_CRDT_DELTA, &payload).await
                };
                if let Err(e) = result {
                    warn!(peer_id = %peer_id, "Blad wysylania CRDT delta: {}", e);
                }
            });
        }

        while join_set.join_next().await.is_some() {}
    }

    /// Wysyla forward request do konkretnego peera i czeka na odpowiedz.
    /// Sprawdza zaufanie, szyfruje request i deszyfruje odpowiedz.
    pub async fn forward_request(
        &self,
        target_node_id: &str,
        request_id: &str,
        payload: Vec<u8>,
    ) -> Result<Vec<u8>> {
        // Sprawdz zaufanie
        if let Some(ref sec) = self.security {
            if !sec.is_trusted(target_node_id) {
                return Err(anyhow::anyhow!("Peer {} nie jest zaufany — odmowa forward request", target_node_id));
            }
        }

        let conn = {
            let conns = self.connections.read().await;
            conns
                .get(target_node_id)
                .map(|mc| mc.connection.clone())
                .ok_or_else(|| anyhow::anyhow!("Brak polaczenia z peerem: {}", target_node_id))?
        };

        let (mut send, mut recv) = conn
            .open_bi()
            .await
            .context("Nie udalo sie otworzyc bidi-stream do forwarding")?;

        // Format: discriminant(1) + req_id_len(4) + req_id(N) + payload
        let req_id_bytes = request_id.as_bytes();
        let req_id_len = (req_id_bytes.len() as u32).to_be_bytes();

        let mut frame = Vec::with_capacity(1 + 4 + req_id_bytes.len() + payload.len());
        frame.push(MESH_MSG_FORWARD_REQ);
        frame.extend_from_slice(&req_id_len);
        frame.extend_from_slice(req_id_bytes);
        frame.extend_from_slice(&payload);

        // [CR-001] Szyfruj caly frame — wymagane dla trusted peerow, brak fallbacku na plaintext
        let send_data = if let Some(ref sec) = self.security {
            if sec.has_shared_secret(target_node_id) {
                sec.encrypt_for_node(target_node_id, &frame)
                    .context("Blad szyfrowania forward request — odmowa wyslania bez szyfrowania")?
            } else {
                return Err(anyhow::anyhow!("Brak shared secret dla peera {} — odmowa wyslania forward request", target_node_id));
            }
        } else {
            frame
        };

        send.write_all(&send_data)
            .await
            .context("Blad wysylania forward request")?;
        send.finish().context("Blad zamykania send stream")?;

        let raw_response = tokio::time::timeout(
            Duration::from_secs(300),
            recv.read_to_end(10 * 1024 * 1024),
        ).await
        .map_err(|_| anyhow::anyhow!("Timeout oczekiwania na odpowiedz forward request (300s)"))?
        .context("Blad odczytu forward response")?;

        // [CR-001] Deszyfruj odpowiedz — wymagane, brak fallbacku na plaintext
        let response = if let Some(ref sec) = self.security {
            if sec.has_shared_secret(target_node_id) {
                sec.decrypt_from_node(target_node_id, &raw_response)
                    .context("Deszyfrowanie odpowiedzi forward nie powiodlo sie — odrzucam")?
            } else {
                return Err(anyhow::anyhow!("Brak shared secret dla peera {} — nie mozna odszyfrowac odpowiedzi", target_node_id));
            }
        } else {
            raw_response
        };

        Ok(response)
    }

    /// Rozlacza peera i usuwa z mapy polaczen
    pub async fn disconnect_peer(&self, node_id: &str) {
        let removed = {
            let mut conns = self.connections.write().await;
            conns.remove(node_id)
        };

        if let Some(mc) = removed {
            mc.connection.close(0u32.into(), b"disconnect");

            // Wyczysc pending_commands powiazane z rozlaczonym nodem
            let expired_ids: Vec<String> = {
                let cmd_map = self.command_to_node.read().await;
                cmd_map.iter()
                    .filter(|(_, nid)| nid.as_str() == node_id)
                    .map(|(cid, _)| cid.clone())
                    .collect()
            };
            if !expired_ids.is_empty() {
                let mut pending = self.pending_commands.write().await;
                let mut cmd_map = self.command_to_node.write().await;
                for cid in &expired_ids {
                    pending.remove(cid);
                    cmd_map.remove(cid);
                }
                debug!(peer_id = %node_id, count = expired_ids.len(), "Wyczyszczono pending commands rozlaczonego peera");
            }

            info!(peer_id = %node_id, "Peer rozlaczony");
            let _ = self.event_tx.send(QuicMeshEvent::PeerDisconnected {
                node_id: node_id.to_string(),
            });
        }
    }

    /// Lista identyfikatorow polaczonych peerow
    pub async fn connected_peers(&self) -> Vec<String> {
        let conns = self.connections.read().await;
        conns.keys().cloned().collect()
    }

    /// Lista polaczonych peer_ids — do propagacji topologii w heartbeat
    pub async fn connected_peer_ids(&self) -> Vec<String> {
        self.connections.read().await.keys().cloned().collect()
    }

    /// Czy peer o danym node_id jest polaczony
    pub async fn is_connected(&self, node_id: &str) -> bool {
        let conns = self.connections.read().await;
        conns.contains_key(node_id)
    }

    // =========================================================================
    // Reconnect z exponential backoff
    // =========================================================================

    /// Uruchamia petla reconnect z exponential backoff + jitter.
    /// Przyjmuje liste adresow — probuje kazdy w kazdej rundzie.
    /// Deduplikacja: nie spawnuje drugiej petli dla tego samego node_id.
    pub fn spawn_reconnect_loop(self: &Arc<Self>, node_id: String, addrs: Vec<SocketAddr>) {
        if addrs.is_empty() { return; }
        let this = Arc::clone(self);
        let reconnecting = self.reconnecting.clone();

        // Nie spawnuj duplikatu
        {
            match reconnecting.try_write() {
                Ok(mut set) => {
                    if set.contains(&node_id) { return; }
                    set.insert(node_id.clone());
                }
                Err(_) => return,
            }
        }

        tokio::spawn(async move {
            this.reconnect_loop(&node_id, &addrs).await;
            reconnecting.write().await.remove(&node_id);
        });
    }

    async fn reconnect_loop(&self, node_id: &str, addrs: &[SocketAddr]) {
        let mut delay = self.config.reconnect_base;

        loop {
            if self.shutdown.is_cancelled() {
                break;
            }

            // Sprawdz czy juz polaczony
            {
                let conns = self.connections.read().await;
                if conns.contains_key(node_id) { break; }
            }

            // Jitter: 0..500ms
            let jitter = Duration::from_millis(rand::random::<u64>() % 500);
            let total_delay = delay + jitter;

            debug!(
                peer_id = %node_id,
                delay_ms = total_delay.as_millis(),
                "Reconnect: czekam..."
            );

            tokio::select! {
                _ = self.shutdown.cancelled() => break,
                _ = tokio::time::sleep(total_delay) => {}
            }

            // Probuj kazdy adres
            for addr in addrs {
                if self.shutdown.is_cancelled() { return; }
                match self.connect_to_peer(node_id, *addr).await {
                    Ok(()) => {
                        let conns = self.connections.read().await;
                        if conns.contains_key(node_id) {
                            info!(peer_id = %node_id, addr = %addr, "Reconnect udany");
                            return;
                        }
                    }
                    Err(e) => {
                        debug!(peer_id = %node_id, addr = %addr, "Reconnect proba: {}", e);
                    }
                }
            }

            // Exponential backoff: podwoj delay, max reconnect_max
            delay = (delay * 2).min(self.config.reconnect_max);
        }
    }

    // =========================================================================
    // Monitor polaczen — wykrywa martwe connections
    // =========================================================================

    async fn connection_monitor(&self) {
        let mut interval = tokio::time::interval(Duration::from_secs(5));

        loop {
            tokio::select! {
                _ = self.shutdown.cancelled() => {
                    debug!("connection_monitor: shutdown");
                    break;
                }
                _ = interval.tick() => {
                    self.check_connections().await;
                }
            }
        }
    }

    /// Sprawdza stan polaczen i usuwa martwe
    async fn check_connections(&self) {
        let mut dead_peers = Vec::new();

        {
            let conns = self.connections.read().await;
            for (peer_id, mc) in conns.iter() {
                if mc.connection.close_reason().is_some() {
                    dead_peers.push(peer_id.clone());
                }
            }
        }

        if !dead_peers.is_empty() {
            let mut conns = self.connections.write().await;
            for peer_id in &dead_peers {
                conns.remove(peer_id);
                warn!(peer_id = %peer_id, "Martwe polaczenie usunite");
                let _ = self.event_tx.send(QuicMeshEvent::PeerDisconnected {
                    node_id: peer_id.clone(),
                });
            }
        }
    }

    // =========================================================================
    // Helpery — zbieranie polaczen i wysylka
    // =========================================================================

    /// [OPT] Zbiera liste (peer_id, Connection) tylko dla trusted peerow.
    /// Jeden lock na connections + jeden Arc::clone na trusted_set.
    /// Uzywane przez broadcast_node_info, broadcast_crdt_delta.
    async fn collect_trusted_connections(&self) -> Vec<(String, quinn::Connection)> {
        if let Some(ref sec) = self.security {
            let trusted_set = sec.trusted_node_ids_snapshot();
            let conns = self.connections.read().await;
            conns.iter()
                .filter(|(id, _)| trusted_set.contains(id.as_str()))
                .map(|(id, mc)| (id.clone(), mc.connection.clone()))
                .collect()
        } else {
            let conns = self.connections.read().await;
            conns.iter()
                .map(|(id, mc)| (id.clone(), mc.connection.clone()))
                .collect()
        }
    }

    /// Wysyla wiadomosc z szyfrowaniem ChaCha20-Poly1305 dla trusted peera.
    /// [CR-001] Brak fallbacku na plaintext — jesli szyfrowanie nie powiodlo sie, zwraca blad.
    ///
    /// [OPT] Optymalizacja: dwa write_all (discriminant + encrypted) zamiast
    /// alokacji Vec na frame. QUIC buforuje wewnetrznie — nie ma dodatkowego
    /// kosztu sieci. Unikamy jednej alokacji Vec per wysylka.
    async fn send_uni_message_encrypted(
        conn: &quinn::Connection,
        discriminant: u8,
        payload: &[u8],
        peer_id: &str,
        security: &MeshSecurity,
    ) -> Result<()> {
        if security.has_shared_secret(peer_id) {
            let encrypted = security.encrypt_for_node(peer_id, payload)
                .context("Blad szyfrowania wiadomosci — odmowa wyslania bez szyfrowania")?;
            let mut send = conn
                .open_uni()
                .await
                .context("Nie udalo sie otworzyc uni-stream")?;
            send.write_all(&[discriminant])
                .await
                .context("Blad wysylania discriminant uni-stream")?;
            send.write_all(&encrypted)
                .await
                .context("Blad wysylania encrypted payload uni-stream")?;
            send.finish().context("Blad zamykania uni-stream")?;
            if discriminant >= 0x30 {
                info!(peer_id = %peer_id, disc = format!("0x{:02X}", discriminant), encrypted_len = encrypted.len(), "Wyslano zaszyfrowana wiadomosc (command/data)");
            }
            Ok(())
        } else {
            Err(anyhow::anyhow!("Brak shared secret dla peera {} — odmowa wyslania wiadomosci", peer_id))
        }
    }

    /// Wysyla wiadomosc uni-directional: discriminant + payload.
    /// Optymalizacja: dwa write_all zamiast alokacji Vec na frame.
    async fn send_uni_message(
        conn: &quinn::Connection,
        discriminant: u8,
        payload: &[u8],
    ) -> Result<()> {
        let mut send = conn
            .open_uni()
            .await
            .context("Nie udalo sie otworzyc uni-stream")?;

        // Dwa zapisy zamiast alokacji Vec — QUIC buforuje wewnetrznie
        send.write_all(&[discriminant])
            .await
            .context("Blad wysylania discriminant uni-stream")?;
        send.write_all(payload)
            .await
            .context("Blad wysylania payload uni-stream")?;
        send.finish().context("Blad zamykania uni-stream")?;

        Ok(())
    }

    // =========================================================================
    // Komendy mesh i service registry
    // =========================================================================

    /// Wysyla komende zarzadzania do sparowanego noda i czeka na odpowiedz
    pub async fn send_command(
        &self,
        target_node_id: &str,
        command: MeshCommandType,
    ) -> Result<CommandResponse> {
        let security = self.security.as_ref()
            .ok_or_else(|| anyhow::anyhow!("Komendy zarzadzania wymagaja aktywnego modulu bezpieczenstwa"))?;

        if !security.is_trusted(target_node_id) {
            return Err(anyhow::anyhow!(
                "Node {} nie jest zaufany",
                target_node_id
            ));
        }

        let command_id = uuid::Uuid::new_v4().to_string();
        let msg = MeshMessage::MeshCommand {
            command_id: command_id.clone(),
            from_node_id: self.node_id.clone(),
            command,
        };

        let payload = msg
            .serialize_rkyv()
            .map_err(|e| anyhow::anyhow!("Blad serializacji MeshCommand: {}", e))?;

        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut pending = self.pending_commands.write().await;
            pending.insert(command_id.clone(), tx);
        }
        {
            let mut cmd_map = self.command_to_node.write().await;
            cmd_map.insert(command_id.clone(), target_node_id.to_string());
        }

        let conn = {
            let conns = self.connections.read().await;
            match conns.get(target_node_id) {
                Some(mc) => {
                    info!(
                        target = %target_node_id,
                        command_id = %command_id,
                        remote_addr = %mc.connection.remote_address(),
                        "send_command: wysylam"
                    );
                    mc.connection.clone()
                }
                None => {
                    return Err(anyhow::anyhow!("Brak polaczenia z peerem: {}", target_node_id));
                }
            }
        };

        Self::send_uni_message_encrypted(
            &conn,
            MESH_MSG_COMMAND,
            &payload,
            target_node_id,
            security,
        )
        .await?;

        // Czekaj na odpowiedz z timeoutem 120s
        match tokio::time::timeout(Duration::from_secs(120), rx).await {
            Ok(Ok(response)) => {
                info!(target = %target_node_id, command_id = %command_id, success = response.success, "send_command: odpowiedz");
                Ok(response)
            }
            Ok(Err(_)) => Err(anyhow::anyhow!("Kanal odpowiedzi zamkniety")),
            Err(_) => {
                self.pending_commands.write().await.remove(&command_id);
                self.command_to_node.write().await.remove(&command_id);
                Err(anyhow::anyhow!("Timeout (120s)"))
            }
        }
    }

    /// Zwraca referencje do rejestru serwisow
    pub fn service_registry(&self) -> &Arc<MeshServiceRegistry> {
        &self.service_registry
    }

    /// Obsluguje odebrana komende — wykonuje przez command_executor i wysyla odpowiedz
    pub async fn handle_command_received(&self, from_node_id: &str, data: &[u8]) {
        let archived = match MeshMessage::deserialize_rkyv(data) {
            Ok(a) => a,
            Err(e) => {
                warn!(from = %from_node_id, "Blad deserializacji MeshCommand: {}", e);
                return;
            }
        };

        let (command_id, cmd_from, command) = match archived {
            tentaflow_protocol::mesh::ArchivedMeshMessage::MeshCommand {
                command_id,
                from_node_id,
                command,
            } => {
                let cmd_type: MeshCommandType = match rkyv::deserialize::<MeshCommandType, rkyv::rancor::Error>(command) {
                    Ok(c) => c,
                    Err(e) => {
                        warn!("Blad deserializacji MeshCommandType: {}", e);
                        return;
                    }
                };
                (
                    command_id.as_str().to_string(),
                    from_node_id.as_str().to_string(),
                    cmd_type,
                )
            }
            _ => {
                warn!(from = %from_node_id, "Oczekiwano MeshCommand, otrzymano inny wariant");
                return;
            }
        };

        let executor = match &self.command_executor {
            Some(e) => e,
            None => {
                warn!("Brak command executor — odrzucam komende");
                return;
            }
        };

        let result = executor.execute(&cmd_from, command).await;

        let response_msg = MeshMessage::MeshCommandResponse {
            command_id,
            from_node_id: self.node_id.clone(),
            success: result.success,
            output: result.output,
            error: result.error,
        };

        let response_payload = match response_msg.serialize_rkyv() {
            Ok(p) => p,
            Err(e) => {
                warn!("Blad serializacji MeshCommandResponse: {}", e);
                return;
            }
        };

        let conn = {
            let conns = self.connections.read().await;
            match conns.get(&cmd_from) {
                Some(mc) => mc.connection.clone(),
                None => {
                    warn!(peer = %cmd_from, "Brak polaczenia do wyslania odpowiedzi");
                    return;
                }
            }
        };

        let send_result = if let Some(ref sec) = self.security {
            Self::send_uni_message_encrypted(
                &conn,
                MESH_MSG_COMMAND_RESPONSE,
                &response_payload,
                &cmd_from,
                sec,
            )
            .await
        } else {
            Self::send_uni_message(&conn, MESH_MSG_COMMAND_RESPONSE, &response_payload).await
        };

        if let Err(e) = send_result {
            warn!(peer = %cmd_from, "Blad wysylania MeshCommandResponse: {}", e);
        }
    }

    /// Obsluguje odebrana odpowiedz na komende — przekazuje do oczekujacego oneshot
    pub async fn handle_command_response_received(&self, from_node_id: &str, data: &[u8]) {
        let archived = match MeshMessage::deserialize_rkyv(data) {
            Ok(a) => a,
            Err(e) => {
                warn!(from = %from_node_id, "Blad deserializacji MeshCommandResponse: {}", e);
                return;
            }
        };

        let (command_id, success, output, error) = match archived {
            tentaflow_protocol::mesh::ArchivedMeshMessage::MeshCommandResponse {
                command_id,
                from_node_id: _,
                success,
                output,
                error,
            } => {
                let err: Option<String> = match error {
                    rkyv::option::ArchivedOption::Some(e) => Some(e.as_str().to_string()),
                    rkyv::option::ArchivedOption::None => None,
                };
                (
                    command_id.as_str().to_string(),
                    *success,
                    output.as_str().to_string(),
                    err,
                )
            }
            _ => {
                warn!(from = %from_node_id, "Oczekiwano MeshCommandResponse");
                return;
            }
        };

        let tx = {
            let mut pending = self.pending_commands.write().await;
            pending.remove(&command_id)
        };
        {
            let mut cmd_map = self.command_to_node.write().await;
            cmd_map.remove(&command_id);
        }

        if let Some(tx) = tx {
            let _ = tx.send(CommandResponse {
                success,
                output,
                error,
            });
        } else {
            debug!(command_id = %command_id, "Odpowiedz na komende bez oczekujacego odbiorcy");
        }
    }

    /// Obsluguje odebrane ServiceAnnounce — aktualizuje service_registry
    pub fn handle_service_announce(&self, node_id: &str, data: &[u8]) {
        let archived = match MeshMessage::deserialize_rkyv(data) {
            Ok(a) => a,
            Err(e) => {
                warn!(from = %node_id, "Blad deserializacji ServiceAnnounce: {}", e);
                return;
            }
        };

        if let tentaflow_protocol::mesh::ArchivedMeshMessage::ServiceAnnounce {
            node_id: _,
            services,
        } = archived
        {
            let svcs: Vec<MeshServiceInfo> = services
                .iter()
                .filter_map(|s| rkyv::deserialize::<MeshServiceInfo, rkyv::rancor::Error>(s).ok())
                .collect();

            self.service_registry.update_remote(node_id, svcs);
        }
    }

    /// Wbudowane certyfikaty TLS z katalogu certs/ repozytorium
    const DEFAULT_CERT_PEM: &[u8] = include_bytes!("../../../certs/cert.pem");
    const DEFAULT_KEY_PEM: &[u8] = include_bytes!("../../../certs/key.pem");

    /// Buduje konfiguracje TLS dla mesh (wbudowane certy, skip server verification)
    fn build_mesh_tls() -> Result<(rustls::ServerConfig, rustls::ClientConfig)> {
        use crate::net::quic::tls;

        let certs = tls::parse_certs_pem(Self::DEFAULT_CERT_PEM)?;
        let key_der = tls::parse_key_pem(Self::DEFAULT_KEY_PEM)?;

        // Server config — bez uwierzytelniania klienta
        let mut server_crypto = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key_der.clone_key())
            .context("Nie udalo sie skonfigurowac server TLS")?;

        server_crypto.alpn_protocols = vec![b"tentaflow-mesh".to_vec()];

        // Client config — pomijamy weryfikacje serwera (mesh internal)
        let mut client_crypto = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
            .with_no_client_auth();

        client_crypto.alpn_protocols = vec![b"tentaflow-mesh".to_vec()];

        Ok((server_crypto, client_crypto))
    }
}

// =============================================================================
// Pomijanie weryfikacji serwera TLS (mesh internal — wszystkie nody sa zaufane)
// =============================================================================

#[derive(Debug)]
struct SkipServerVerification;

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::ED25519,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
        ]
    }
}
