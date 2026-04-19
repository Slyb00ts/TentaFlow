// =============================================================================
// Plik: mesh/gossip.rs
// Opis: Protokol SWIM-like gossip — wykrywanie awarii peerow, propagacja stanu
//       i heartbeaty w mesh sieci routerow TentaFlow. Transport: UDP.
// =============================================================================

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rand::seq::IndexedRandom;
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, error, info, warn};

// Maksymalny rozmiar datagramu UDP dla gossip
const MAX_DATAGRAM_SIZE: usize = 65_507;

/// Stan peera w mesh
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PeerState {
    Alive,
    Suspect,
    Dead,
}

/// Informacje o peerze
#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub node_id: String,
    pub address: SocketAddr,
    pub hostname: String,
    pub role: String,
    pub services: Vec<String>,
    pub state: PeerState,
    pub incarnation: u64,
    pub last_seen: Instant,
    pub cluster_name: String,
}

/// Skrocone info o peerze (do przesylania w wiadomosciach)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerSummary {
    pub node_id: String,
    pub address: SocketAddr,
    pub hostname: String,
    pub role: String,
    pub services: Vec<String>,
    pub incarnation: u64,
    pub cluster_name: String,
}

impl From<&PeerInfo> for PeerSummary {
    fn from(p: &PeerInfo) -> Self {
        Self {
            node_id: p.node_id.clone(),
            address: p.address,
            hostname: p.hostname.clone(),
            role: p.role.clone(),
            services: p.services.clone(),
            incarnation: p.incarnation,
            cluster_name: p.cluster_name.clone(),
        }
    }
}

/// Typy wiadomosci gossip
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GossipMessage {
    /// Ping — pytanie "zyjesz?"
    Ping { sender: PeerSummary, seq: u64 },
    /// Ack — odpowiedz "zyje"
    Ack { sender: PeerSummary, seq: u64 },
    /// PingReq — popros inny peer o sprawdzenie trzeciego
    PingReq {
        sender: PeerSummary,
        target: SocketAddr,
        seq: u64,
    },
    /// Broadcast — informacja o zmianie stanu peera
    Broadcast { event: GossipEvent },
    /// FullSync — pelna wymiana listy peerow (anti-entropy)
    FullSync { peers: Vec<PeerSummary> },
    /// FullSyncReq — prosba o pelna liste peerow
    FullSyncReq { sender: PeerSummary },
}

/// Zdarzenia gossip
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GossipEvent {
    Join(PeerSummary),
    Leave(String),
    Suspect(String),
    Alive(PeerSummary),
    ServiceUpdate {
        node_id: String,
        services: Vec<String>,
    },
}

/// Konfiguracja gossip
#[derive(Debug, Clone)]
pub struct GossipConfig {
    pub node_id: String,
    pub listen_addr: SocketAddr,
    pub hostname: String,
    pub role: String,
    pub cluster_name: String,
    pub ping_interval: Duration,
    pub ping_timeout: Duration,
    pub suspect_timeout: Duration,
    pub dead_timeout: Duration,
    pub fanout: usize,
    pub full_sync_interval: Duration,
}

impl Default for GossipConfig {
    fn default() -> Self {
        Self {
            node_id: uuid::Uuid::new_v4().to_string(),
            listen_addr: "0.0.0.0:5002".parse().expect("poprawny adres"),
            hostname: hostname::get()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            role: "router".to_string(),
            cluster_name: "tentaflow".to_string(),
            ping_interval: Duration::from_secs(1),
            ping_timeout: Duration::from_millis(500),
            suspect_timeout: Duration::from_secs(5),
            dead_timeout: Duration::from_secs(15),
            fanout: 3,
            full_sync_interval: Duration::from_secs(30),
        }
    }
}

/// Oczekujacy ping — sledzenie wyslanych pingow czekajacych na Ack
struct PendingPing {
    target: SocketAddr,
    sent_at: Instant,
    indirect: bool,
}

/// Gossip engine — zarzadza stanem mesh
pub struct GossipEngine {
    config: GossipConfig,
    peers: Arc<RwLock<HashMap<String, PeerInfo>>>,
    local_services: Arc<RwLock<Vec<String>>>,
    seq_counter: Arc<AtomicU64>,
    incarnation: Arc<AtomicU64>,
    pending_pings: Arc<RwLock<HashMap<u64, PendingPing>>>,
    event_tx: broadcast::Sender<GossipEvent>,
    socket: Arc<RwLock<Option<Arc<UdpSocket>>>>,
}

impl GossipEngine {
    /// Tworzy nowa instancje gossip engine z podana konfiguracja
    pub fn new(config: GossipConfig) -> Self {
        let (event_tx, _) = broadcast::channel(256);
        Self {
            config,
            peers: Arc::new(RwLock::new(HashMap::new())),
            local_services: Arc::new(RwLock::new(Vec::new())),
            seq_counter: Arc::new(AtomicU64::new(0)),
            incarnation: Arc::new(AtomicU64::new(0)),
            pending_pings: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
            socket: Arc::new(RwLock::new(None)),
        }
    }

    /// Uruchamia gossip engine — binduje socket UDP i odpala background taski
    pub async fn start(&self) -> Result<(), crate::error::CoreError> {
        let sock = UdpSocket::bind(self.config.listen_addr)
            .await
            .map_err(|e| crate::error::CoreError::GossipError {
                message: format!("Nie mozna zbindowac {}: {}", self.config.listen_addr, e),
                source: Some(e.into()),
            })?;

        let sock = Arc::new(sock);
        {
            let mut guard = self.socket.write().await;
            *guard = Some(Arc::clone(&sock));
        }

        info!(
            addr = %self.config.listen_addr,
            node_id = %self.config.node_id,
            "Gossip engine uruchomiony"
        );

        // Task odbierajacy wiadomosci UDP
        self.spawn_receiver(Arc::clone(&sock));

        // Task ping loop
        self.spawn_ping_loop(Arc::clone(&sock));

        // Task failure detector — sprawdza timeouty pingow
        self.spawn_failure_detector(Arc::clone(&sock));

        // Task suspect/dead reaper
        self.spawn_state_reaper();

        // Task anti-entropy (full sync)
        self.spawn_anti_entropy(Arc::clone(&sock));

        Ok(())
    }

    /// Reczne dodanie peera (static seed peers)
    pub async fn add_peer(&self, addr: SocketAddr) {
        // Wyslij ping do nowego peera — jesli odpowie, dodamy go przez handle_message
        if let Some(sock) = self.get_socket().await {
            let seq = self.next_seq();
            let msg = GossipMessage::Ping {
                sender: self.local_summary().await,
                seq,
            };
            if let Err(e) = self.send_message(&sock, &msg, addr).await {
                warn!(addr = %addr, error = %e, "Nie udalo sie pingowac nowego peera");
            } else {
                let mut pending = self.pending_pings.write().await;
                pending.insert(
                    seq,
                    PendingPing {
                        target: addr,
                        sent_at: Instant::now(),
                        indirect: false,
                    },
                );
                debug!(addr = %addr, seq, "Wyslano ping do nowego peera");
            }
        }
    }

    /// Obsluga odebranej wiadomosci gossip z walidacja cluster_name
    pub async fn handle_message(&self, msg: GossipMessage, from: SocketAddr) {
        // Wczesne odrzucenie wiadomosci z innego klastra
        let foreign_cluster = match &msg {
            GossipMessage::Ping { sender, .. }
            | GossipMessage::Ack { sender, .. }
            | GossipMessage::PingReq { sender, .. }
            | GossipMessage::FullSyncReq { sender } => {
                sender.cluster_name != self.config.cluster_name
            }
            // Broadcast i FullSync sprawdzaja cluster_name per element wewnetrznie
            _ => false,
        };
        if foreign_cluster {
            debug!("Odrzucono wiadomosc z innego klastra od {}", from);
            return;
        }

        match msg {
            GossipMessage::Ping { sender, seq } => {
                self.handle_ping(sender, seq, from).await;
            }
            GossipMessage::Ack { sender, seq } => {
                self.handle_ack(sender, seq).await;
            }
            GossipMessage::PingReq {
                sender,
                target,
                seq,
            } => {
                self.handle_ping_req(sender, target, seq, from).await;
            }
            GossipMessage::Broadcast { event } => {
                self.handle_broadcast(event).await;
            }
            GossipMessage::FullSync { peers } => {
                self.handle_full_sync(peers).await;
            }
            GossipMessage::FullSyncReq { sender } => {
                self.handle_full_sync_req(sender, from).await;
            }
        }
    }

    /// Zwraca liste zywych peerow
    pub async fn get_alive_peers(&self) -> Vec<PeerInfo> {
        let peers = self.peers.read().await;
        peers
            .values()
            .filter(|p| p.state == PeerState::Alive)
            .cloned()
            .collect()
    }

    /// Aktualizacja lokalnych serwisow i broadcast do mesh
    pub async fn update_local_services(&self, services: Vec<String>) {
        {
            let mut local = self.local_services.write().await;
            *local = services.clone();
        }

        let event = GossipEvent::ServiceUpdate {
            node_id: self.config.node_id.clone(),
            services,
        };
        let _ = self.event_tx.send(event.clone());
        self.broadcast_event(event).await;
    }

    /// Subskrypcja zdarzen gossip
    pub fn subscribe(&self) -> broadcast::Receiver<GossipEvent> {
        self.event_tx.subscribe()
    }

    // =========================================================================
    // Metody prywatne — background taski
    // =========================================================================

    /// Lokalny PeerSummary do wysylania w wiadomosciach
    async fn local_summary(&self) -> PeerSummary {
        let services = self.local_services.read().await;
        make_local_summary(&self.config, &services, self.incarnation.load(Ordering::Relaxed))
    }

    fn next_seq(&self) -> u64 {
        self.seq_counter.fetch_add(1, Ordering::Relaxed)
    }

    async fn get_socket(&self) -> Option<Arc<UdpSocket>> {
        let guard = self.socket.read().await;
        guard.clone()
    }

    /// Serializacja i wyslanie wiadomosci UDP
    async fn send_message(
        &self,
        sock: &UdpSocket,
        msg: &GossipMessage,
        to: SocketAddr,
    ) -> Result<(), crate::error::CoreError> {
        do_send_message(sock, msg, to).await
    }

    /// Task odbierajacy datagramy UDP
    fn spawn_receiver(&self, sock: Arc<UdpSocket>) {
        let peers = Arc::clone(&self.peers);
        let pending_pings = Arc::clone(&self.pending_pings);
        let local_services = Arc::clone(&self.local_services);
        let seq_counter = Arc::clone(&self.seq_counter);
        let incarnation = Arc::clone(&self.incarnation);
        let event_tx = self.event_tx.clone();
        let config = self.config.clone();
        let socket_ref = Arc::clone(&self.socket);

        tokio::spawn(async move {
            let engine_ref = GossipEngineRef {
                config,
                peers,
                local_services,
                seq_counter,
                incarnation,
                pending_pings,
                event_tx,
                socket: socket_ref,
            };

            let mut buf = vec![0u8; MAX_DATAGRAM_SIZE];
            loop {
                match sock.recv_from(&mut buf).await {
                    Ok((len, from)) => {
                        let data = &buf[..len];
                        match serde_json::from_slice::<GossipMessage>(data) {
                            Ok(msg) => {
                                engine_ref.handle_message(msg, from).await;
                            }
                            Err(e) => {
                                warn!(from = %from, error = %e, "Nieczytelna wiadomosc gossip");
                            }
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "Blad odbioru UDP");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        });
    }

    /// Ping loop — co ping_interval wybiera losowo fanout peerow i pinguje
    fn spawn_ping_loop(&self, sock: Arc<UdpSocket>) {
        let peers = Arc::clone(&self.peers);
        let local_services = Arc::clone(&self.local_services);
        let seq_counter = Arc::clone(&self.seq_counter);
        let incarnation = Arc::clone(&self.incarnation);
        let pending_pings = Arc::clone(&self.pending_pings);
        let config = self.config.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(config.ping_interval);
            loop {
                interval.tick().await;

                // Zbierz adresy zywych i podejrzanych peerow
                let targets: Vec<SocketAddr> = {
                    let guard = peers.read().await;
                    guard
                        .values()
                        .filter(|p| p.state != PeerState::Dead)
                        .map(|p| p.address)
                        .collect()
                };

                if targets.is_empty() {
                    continue;
                }

                // Wybierz losowo fanout peerow
                let selected: Vec<_> = {
                    let mut rng = rand::rng();
                    targets
                        .choose_multiple(&mut rng, config.fanout.min(targets.len()))
                        .copied()
                        .collect()
                };

                let summary = {
                    let services = local_services.read().await;
                    PeerSummary {
                        node_id: config.node_id.clone(),
                        address: config.listen_addr,
                        hostname: config.hostname.clone(),
                        role: config.role.clone(),
                        services: services.clone(),
                        incarnation: incarnation.load(Ordering::Relaxed),
                        cluster_name: config.cluster_name.clone(),
                    }
                };

                for addr in selected {
                    let seq = seq_counter.fetch_add(1, Ordering::Relaxed);
                    let msg = GossipMessage::Ping {
                        sender: summary.clone(),
                        seq,
                    };

                    let data = match serde_json::to_vec(&msg) {
                        Ok(d) => d,
                        Err(_) => continue,
                    };

                    if let Err(e) = sock.send_to(&data, addr).await {
                        warn!(addr = %addr, error = %e, "Blad wysylania ping");
                        continue;
                    }

                    let mut pending = pending_pings.write().await;
                    pending.insert(
                        seq,
                        PendingPing {
                            target: addr,
                            sent_at: Instant::now(),
                            indirect: false,
                        },
                    );

                    debug!(addr = %addr, seq, "Wyslano ping");
                }
            }
        });
    }

    /// Failure detector — sprawdza oczekujace pingi i eskaluje do PingReq/Suspect
    fn spawn_failure_detector(&self, sock: Arc<UdpSocket>) {
        let peers = Arc::clone(&self.peers);
        let local_services = Arc::clone(&self.local_services);
        let seq_counter = Arc::clone(&self.seq_counter);
        let incarnation = Arc::clone(&self.incarnation);
        let pending_pings = Arc::clone(&self.pending_pings);
        let event_tx = self.event_tx.clone();
        let config = self.config.clone();

        tokio::spawn(async move {
            let check_interval = Duration::from_millis(200);
            let mut interval = tokio::time::interval(check_interval);

            loop {
                interval.tick().await;

                let now = Instant::now();
                let mut expired = Vec::new();

                // Znajdz pingi ktore przekroczyly timeout
                {
                    let pending = pending_pings.read().await;
                    for (&seq, ping) in pending.iter() {
                        if now.duration_since(ping.sent_at) > config.ping_timeout {
                            expired.push((seq, ping.target, ping.indirect));
                        }
                    }
                }

                for (seq, target, was_indirect) in expired {
                    // Usun z oczekujacych
                    {
                        let mut pending = pending_pings.write().await;
                        pending.remove(&seq);
                    }

                    if was_indirect {
                        // Indirect ping tez nie dostal odpowiedzi — oznacz jako Suspect
                        let mut guard = peers.write().await;
                        if let Some(peer) = guard
                            .values_mut()
                            .find(|p| p.address == target && p.state == PeerState::Alive)
                        {
                            peer.state = PeerState::Suspect;
                            warn!(
                                node_id = %peer.node_id,
                                addr = %target,
                                "Peer oznaczony jako Suspect"
                            );
                            let event = GossipEvent::Suspect(peer.node_id.clone());
                            let _ = event_tx.send(event);
                        }
                    } else {
                        // Bezposredni ping timeout — wyslij PingReq do innego peera
                        let other_peers: Vec<SocketAddr> = {
                            let guard = peers.read().await;
                            guard
                                .values()
                                .filter(|p| {
                                    p.address != target && p.state == PeerState::Alive
                                })
                                .map(|p| p.address)
                                .collect()
                        };

                        if let Some(relay) = {
                            let mut rng = rand::rng();
                            other_peers.choose(&mut rng).copied()
                        } {
                            let new_seq = seq_counter.fetch_add(1, Ordering::Relaxed);
                            let summary = {
                                let services = local_services.read().await;
                                PeerSummary {
                                    node_id: config.node_id.clone(),
                                    address: config.listen_addr,
                                    hostname: config.hostname.clone(),
                                    role: config.role.clone(),
                                    services: services.clone(),
                                    incarnation: incarnation.load(Ordering::Relaxed),
                                    cluster_name: config.cluster_name.clone(),
                                }
                            };

                            let msg = GossipMessage::PingReq {
                                sender: summary,
                                target,
                                seq: new_seq,
                            };

                            let data = match serde_json::to_vec(&msg) {
                                Ok(d) => d,
                                Err(_) => continue,
                            };

                            if sock.send_to(&data, relay).await.is_ok() {
                                let mut pending = pending_pings.write().await;
                                pending.insert(
                                    new_seq,
                                    PendingPing {
                                        target,
                                        sent_at: Instant::now(),
                                        indirect: true,
                                    },
                                );
                                debug!(
                                    relay = %relay,
                                    target = %target,
                                    seq = new_seq,
                                    "Wyslano PingReq przez posrednika"
                                );
                            }
                        } else {
                            // Brak posrednikow — od razu oznacz jako Suspect
                            let mut guard = peers.write().await;
                            if let Some(peer) = guard
                                .values_mut()
                                .find(|p| p.address == target && p.state == PeerState::Alive)
                            {
                                peer.state = PeerState::Suspect;
                                warn!(
                                    node_id = %peer.node_id,
                                    addr = %target,
                                    "Peer Suspect (brak posrednikow)"
                                );
                                let event = GossipEvent::Suspect(peer.node_id.clone());
                                let _ = event_tx.send(event);
                            }
                        }
                    }
                }
            }
        });
    }

    /// Reaper — eskaluje Suspect → Dead i usuwa Dead po dead_timeout
    fn spawn_state_reaper(&self) {
        let peers = Arc::clone(&self.peers);
        let event_tx = self.event_tx.clone();
        let suspect_timeout = self.config.suspect_timeout;
        let dead_timeout = self.config.dead_timeout;

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(1));
            loop {
                interval.tick().await;

                let now = Instant::now();
                let mut to_dead = Vec::new();
                let mut to_remove = Vec::new();

                {
                    let guard = peers.read().await;
                    for (id, peer) in guard.iter() {
                        let elapsed = now.duration_since(peer.last_seen);
                        match peer.state {
                            PeerState::Suspect if elapsed > suspect_timeout => {
                                to_dead.push(id.clone());
                            }
                            PeerState::Dead if elapsed > dead_timeout => {
                                to_remove.push(id.clone());
                            }
                            _ => {}
                        }
                    }
                }

                if !to_dead.is_empty() || !to_remove.is_empty() {
                    let mut guard = peers.write().await;
                    for id in &to_dead {
                        if let Some(peer) = guard.get_mut(id) {
                            if peer.state == PeerState::Suspect {
                                peer.state = PeerState::Dead;
                                warn!(node_id = %id, "Peer oznaczony jako Dead");
                                let event = GossipEvent::Leave(id.clone());
                                let _ = event_tx.send(event);
                            }
                        }
                    }
                    for id in &to_remove {
                        if let Some(peer) = guard.get(id) {
                            if peer.state == PeerState::Dead {
                                info!(node_id = %id, "Usuwanie martwego peera");
                                guard.remove(id);
                            }
                        }
                    }
                }
            }
        });
    }

    /// Anti-entropy — co full_sync_interval wymienia pelna liste peerow z losowym peerem
    fn spawn_anti_entropy(&self, sock: Arc<UdpSocket>) {
        let peers = Arc::clone(&self.peers);
        let local_services = Arc::clone(&self.local_services);
        let incarnation = Arc::clone(&self.incarnation);
        let config = self.config.clone();

        tokio::spawn(async move {
            let mut interval = tokio::time::interval(config.full_sync_interval);
            loop {
                interval.tick().await;

                let targets: Vec<SocketAddr> = {
                    let guard = peers.read().await;
                    guard
                        .values()
                        .filter(|p| p.state == PeerState::Alive)
                        .map(|p| p.address)
                        .collect()
                };

                let target = {
                    let mut rng = rand::rng();
                    targets.choose(&mut rng).copied()
                };

                if let Some(addr) = target {
                    let summary = {
                        let services = local_services.read().await;
                        PeerSummary {
                            node_id: config.node_id.clone(),
                            address: config.listen_addr,
                            hostname: config.hostname.clone(),
                            role: config.role.clone(),
                            services: services.clone(),
                            incarnation: incarnation.load(Ordering::Relaxed),
                            cluster_name: config.cluster_name.clone(),
                        }
                    };

                    let msg = GossipMessage::FullSyncReq { sender: summary };
                    if let Ok(data) = serde_json::to_vec(&msg) {
                        if let Err(e) = sock.send_to(&data, addr).await {
                            warn!(addr = %addr, error = %e, "Blad wysylania FullSyncReq");
                        } else {
                            debug!(addr = %addr, "Wyslano FullSyncReq");
                        }
                    }
                }
            }
        });
    }

    // =========================================================================
    // Handlery wiadomosci
    // =========================================================================

    async fn handle_ping(&self, sender: PeerSummary, seq: u64, from: SocketAddr) {
        // Ignoruj wiadomosci z innego klastra
        if sender.cluster_name != self.config.cluster_name {
            debug!(
                cluster = %sender.cluster_name,
                "Ignoruję ping z innego klastra"
            );
            return;
        }

        self.upsert_peer(&sender, from).await;

        // Odpowiedz Ack
        if let Some(sock) = self.get_socket().await {
            let ack = GossipMessage::Ack {
                sender: self.local_summary().await,
                seq,
            };
            if let Err(e) = self.send_message(&sock, &ack, from).await {
                warn!(from = %from, error = %e, "Blad wysylania Ack");
            }
        }
    }

    async fn handle_ack(&self, sender: PeerSummary, seq: u64) {
        if sender.cluster_name != self.config.cluster_name {
            return;
        }

        self.upsert_peer(&sender, sender.address).await;

        // Usun z oczekujacych pingow
        let mut pending = self.pending_pings.write().await;
        pending.remove(&seq);
    }

    async fn handle_ping_req(
        &self,
        sender: PeerSummary,
        target: SocketAddr,
        seq: u64,
        from: SocketAddr,
    ) {
        if sender.cluster_name != self.config.cluster_name {
            return;
        }

        self.upsert_peer(&sender, from).await;

        // Pinguj cel w imieniu nadawcy
        if let Some(sock) = self.get_socket().await {
            let ping = GossipMessage::Ping {
                sender: self.local_summary().await,
                seq,
            };
            if let Err(e) = self.send_message(&sock, &ping, target).await {
                warn!(target = %target, error = %e, "Blad indirect ping");
            } else {
                debug!(
                    requester = %from,
                    target = %target,
                    "Wykonuję indirect ping"
                );
            }
        }
    }

    async fn handle_broadcast(&self, event: GossipEvent) {
        match &event {
            GossipEvent::Join(summary) => {
                if summary.cluster_name != self.config.cluster_name {
                    return;
                }
                self.upsert_peer(summary, summary.address).await;
                info!(
                    node_id = %summary.node_id,
                    role = %summary.role,
                    "Nowy peer dolaczyl"
                );
            }
            GossipEvent::Leave(node_id) => {
                let mut guard = self.peers.write().await;
                if let Some(peer) = guard.get_mut(node_id) {
                    peer.state = PeerState::Dead;
                    info!(node_id = %node_id, "Peer opuscil mesh");
                }
            }
            GossipEvent::Suspect(node_id) => {
                let mut guard = self.peers.write().await;
                if let Some(peer) = guard.get_mut(node_id) {
                    if *node_id == self.config.node_id {
                        let new_inc = self.incarnation.fetch_add(1, Ordering::Relaxed) + 1;
                        info!(incarnation = new_inc, "Obrona przed suspect — zwiekszona incarnation");
                        drop(guard);
                        self.broadcast_alive().await;
                        return;
                    }
                    if peer.state == PeerState::Alive {
                        peer.state = PeerState::Suspect;
                    }
                }
            }
            GossipEvent::Alive(summary) => {
                if summary.cluster_name != self.config.cluster_name {
                    return;
                }
                let mut guard = self.peers.write().await;
                if let Some(peer) = guard.get_mut(&summary.node_id) {
                    if summary.incarnation > peer.incarnation {
                        peer.state = PeerState::Alive;
                        peer.incarnation = summary.incarnation;
                        peer.last_seen = Instant::now();
                        peer.services = summary.services.clone();
                        info!(
                            node_id = %summary.node_id,
                            incarnation = summary.incarnation,
                            "Peer potwierdzil zycie"
                        );
                    }
                }
            }
            GossipEvent::ServiceUpdate { node_id, services } => {
                let mut guard = self.peers.write().await;
                if let Some(peer) = guard.get_mut(node_id) {
                    peer.services = services.clone();
                    debug!(node_id = %node_id, "Zaktualizowano serwisy peera");
                }
            }
        }

        // Przekaz zdarzenie subskrybentom
        let _ = self.event_tx.send(event);
    }

    async fn handle_full_sync(&self, remote_peers: Vec<PeerSummary>) {
        for summary in remote_peers {
            if summary.cluster_name != self.config.cluster_name {
                continue;
            }
            if summary.node_id == self.config.node_id {
                continue;
            }
            self.upsert_peer(&summary, summary.address).await;
        }
        debug!("Przetworzono FullSync");
    }

    async fn handle_full_sync_req(&self, sender: PeerSummary, from: SocketAddr) {
        if sender.cluster_name != self.config.cluster_name {
            return;
        }

        self.upsert_peer(&sender, from).await;

        // Wyslij pelna liste peerow
        let peers: Vec<PeerSummary> = {
            let guard = self.peers.read().await;
            let mut list: Vec<PeerSummary> = guard
                .values()
                .filter(|p| p.state != PeerState::Dead)
                .map(PeerSummary::from)
                .collect();
            // Dodaj siebie
            list.push(self.local_summary().await);
            list
        };

        if let Some(sock) = self.get_socket().await {
            let msg = GossipMessage::FullSync { peers };
            if let Err(e) = self.send_message(&sock, &msg, from).await {
                warn!(from = %from, error = %e, "Blad wysylania FullSync");
            } else {
                debug!(from = %from, "Wyslano FullSync");
            }
        }
    }

    // =========================================================================
    // Helpery
    // =========================================================================

    /// Dodaj lub zaktualizuj peera na podstawie otrzymanego PeerSummary
    async fn upsert_peer(&self, summary: &PeerSummary, addr: SocketAddr) {
        let mut guard = self.peers.write().await;
        let is_new = do_upsert_peer(&mut guard, summary, addr, &self.config.node_id);

        if is_new {
            info!(
                node_id = %summary.node_id,
                addr = %addr,
                role = %summary.role,
                "Nowy peer odkryty"
            );
            let event = GossipEvent::Join(summary.clone());
            drop(guard);
            let _ = self.event_tx.send(event);
        }
    }

    /// Broadcast zdarzenia do wszystkich zywych peerow
    async fn broadcast_event(&self, event: GossipEvent) {
        let targets = {
            let guard = self.peers.read().await;
            collect_alive_targets(&guard)
        };

        if let Some(sock) = self.get_socket().await {
            let msg = GossipMessage::Broadcast { event };
            for addr in targets {
                if let Err(e) = self.send_message(&sock, &msg, addr).await {
                    warn!(addr = %addr, error = %e, "Blad broadcast");
                }
            }
        }
    }

    /// Obrona przed suspect — broadcast Alive z nowa incarnation
    async fn broadcast_alive(&self) {
        let summary = self.local_summary().await;
        let event = GossipEvent::Alive(summary);
        self.broadcast_event(event).await;
    }
}

// =============================================================================
// Wspolne funkcje — eliminacja duplikacji miedzy GossipEngine i GossipEngineRef
// =============================================================================

/// Buduje lokalny PeerSummary z poszczegolnych pol
fn make_local_summary(
    config: &GossipConfig,
    services: &[String],
    incarnation: u64,
) -> PeerSummary {
    PeerSummary {
        node_id: config.node_id.clone(),
        address: config.listen_addr,
        hostname: config.hostname.clone(),
        role: config.role.clone(),
        services: services.to_vec(),
        incarnation,
        cluster_name: config.cluster_name.clone(),
    }
}

/// Serializacja i wyslanie wiadomosci UDP
async fn do_send_message(
    sock: &UdpSocket,
    msg: &GossipMessage,
    to: SocketAddr,
) -> Result<(), crate::error::CoreError> {
    let data =
        serde_json::to_vec(msg).map_err(|e| crate::error::CoreError::GossipError {
            message: format!("Blad serializacji: {}", e),
            source: Some(e.into()),
        })?;

    sock.send_to(&data, to)
        .await
        .map_err(|e| crate::error::CoreError::GossipError {
            message: format!("Blad wysylania do {}: {}", to, e),
            source: Some(e.into()),
        })?;

    Ok(())
}

/// Dodaj lub zaktualizuj peera. Zwraca true jesli peer jest nowy.
fn do_upsert_peer(
    peers: &mut HashMap<String, PeerInfo>,
    summary: &PeerSummary,
    addr: SocketAddr,
    node_id: &str,
) -> bool {
    if summary.node_id == node_id {
        return false;
    }

    let is_new = !peers.contains_key(&summary.node_id);

    let peer = peers
        .entry(summary.node_id.clone())
        .or_insert_with(|| PeerInfo {
            node_id: summary.node_id.clone(),
            address: addr,
            hostname: summary.hostname.clone(),
            role: summary.role.clone(),
            services: summary.services.clone(),
            state: PeerState::Alive,
            incarnation: summary.incarnation,
            last_seen: Instant::now(),
            cluster_name: summary.cluster_name.clone(),
        });

    if !is_new && summary.incarnation >= peer.incarnation {
        let old_incarnation = peer.incarnation;
        peer.address = addr;
        peer.hostname = summary.hostname.clone();
        peer.services = summary.services.clone();
        peer.incarnation = summary.incarnation;
        peer.last_seen = Instant::now();
        if peer.state == PeerState::Suspect && summary.incarnation > old_incarnation {
            peer.state = PeerState::Alive;
        }
    }

    is_new
}

/// Obsluga broadcast Leave — oznacz peera jako Dead
fn do_broadcast_leave(peers: &mut HashMap<String, PeerInfo>, leave_node_id: &str) {
    if let Some(peer) = peers.get_mut(leave_node_id) {
        peer.state = PeerState::Dead;
    }
}

/// Obsluga broadcast Suspect (dla innego noda) — oznacz peera jako Suspect
fn do_broadcast_suspect_other(peers: &mut HashMap<String, PeerInfo>, suspect_node_id: &str) {
    if let Some(peer) = peers.get_mut(suspect_node_id) {
        if peer.state == PeerState::Alive {
            peer.state = PeerState::Suspect;
        }
    }
}

/// Obsluga broadcast Alive — aktualizuj peera jesli incarnation wyzsza
fn do_broadcast_alive(
    peers: &mut HashMap<String, PeerInfo>,
    summary: &PeerSummary,
    cluster_name: &str,
) {
    if summary.cluster_name != cluster_name {
        return;
    }
    if let Some(peer) = peers.get_mut(&summary.node_id) {
        if summary.incarnation > peer.incarnation {
            peer.state = PeerState::Alive;
            peer.incarnation = summary.incarnation;
            peer.last_seen = Instant::now();
            peer.services = summary.services.clone();
        }
    }
}

/// Obsluga broadcast ServiceUpdate — aktualizuj serwisy peera
fn do_broadcast_service_update(
    peers: &mut HashMap<String, PeerInfo>,
    update_node_id: &str,
    services: &[String],
) {
    if let Some(peer) = peers.get_mut(update_node_id) {
        peer.services = services.to_vec();
    }
}

/// Zbierz adresy zywych peerow do broadcastu
fn collect_alive_targets(peers: &HashMap<String, PeerInfo>) -> Vec<SocketAddr> {
    peers
        .values()
        .filter(|p| p.state == PeerState::Alive)
        .map(|p| p.address)
        .collect()
}

// =============================================================================
// GossipEngineRef — lekka referencja do uzytku w spawned taskach
// =============================================================================

/// Lekka referencja na dane GossipEngine do uzytku w taskach
struct GossipEngineRef {
    config: GossipConfig,
    peers: Arc<RwLock<HashMap<String, PeerInfo>>>,
    local_services: Arc<RwLock<Vec<String>>>,
    #[allow(dead_code)]
    seq_counter: Arc<AtomicU64>,
    incarnation: Arc<AtomicU64>,
    pending_pings: Arc<RwLock<HashMap<u64, PendingPing>>>,
    event_tx: broadcast::Sender<GossipEvent>,
    socket: Arc<RwLock<Option<Arc<UdpSocket>>>>,
}

impl GossipEngineRef {
    async fn local_summary(&self) -> PeerSummary {
        let services = self.local_services.read().await;
        make_local_summary(&self.config, &services, self.incarnation.load(Ordering::Relaxed))
    }

    async fn get_socket(&self) -> Option<Arc<UdpSocket>> {
        let guard = self.socket.read().await;
        guard.clone()
    }

    async fn send_message(
        &self,
        sock: &UdpSocket,
        msg: &GossipMessage,
        to: SocketAddr,
    ) -> Result<(), crate::error::CoreError> {
        do_send_message(sock, msg, to).await
    }

    /// Obsluga wiadomosci w kontekscie receivera
    async fn handle_message(&self, msg: GossipMessage, from: SocketAddr) {
        match msg {
            GossipMessage::Ping { sender, seq } => {
                if sender.cluster_name != self.config.cluster_name {
                    return;
                }
                self.upsert_peer(&sender, from).await;
                if let Some(sock) = self.get_socket().await {
                    let ack = GossipMessage::Ack {
                        sender: self.local_summary().await,
                        seq,
                    };
                    let _ = self.send_message(&sock, &ack, from).await;
                }
            }
            GossipMessage::Ack { sender, seq } => {
                if sender.cluster_name != self.config.cluster_name {
                    return;
                }
                self.upsert_peer(&sender, sender.address).await;
                let mut pending = self.pending_pings.write().await;
                pending.remove(&seq);
            }
            GossipMessage::PingReq {
                sender,
                target,
                seq,
            } => {
                if sender.cluster_name != self.config.cluster_name {
                    return;
                }
                self.upsert_peer(&sender, from).await;
                if let Some(sock) = self.get_socket().await {
                    let ping = GossipMessage::Ping {
                        sender: self.local_summary().await,
                        seq,
                    };
                    let _ = self.send_message(&sock, &ping, target).await;
                }
            }
            GossipMessage::Broadcast { event } => {
                self.handle_broadcast(event).await;
            }
            GossipMessage::FullSync { peers } => {
                for summary in peers {
                    if summary.cluster_name != self.config.cluster_name {
                        continue;
                    }
                    if summary.node_id == self.config.node_id {
                        continue;
                    }
                    self.upsert_peer(&summary, summary.address).await;
                }
            }
            GossipMessage::FullSyncReq { sender } => {
                if sender.cluster_name != self.config.cluster_name {
                    return;
                }
                self.upsert_peer(&sender, from).await;
                let peers: Vec<PeerSummary> = {
                    let guard = self.peers.read().await;
                    let mut list: Vec<PeerSummary> = guard
                        .values()
                        .filter(|p| p.state != PeerState::Dead)
                        .map(PeerSummary::from)
                        .collect();
                    list.push(self.local_summary().await);
                    list
                };
                if let Some(sock) = self.get_socket().await {
                    let msg = GossipMessage::FullSync { peers };
                    let _ = self.send_message(&sock, &msg, from).await;
                }
            }
        }
    }

    async fn handle_broadcast(&self, event: GossipEvent) {
        match &event {
            GossipEvent::Join(summary) => {
                if summary.cluster_name != self.config.cluster_name {
                    return;
                }
                self.upsert_peer(summary, summary.address).await;
            }
            GossipEvent::Leave(node_id) => {
                let mut guard = self.peers.write().await;
                do_broadcast_leave(&mut guard, node_id);
            }
            GossipEvent::Suspect(node_id) => {
                if *node_id == self.config.node_id {
                    let new_inc = self.incarnation.fetch_add(1, Ordering::Relaxed) + 1;
                    info!(incarnation = new_inc, "Obrona przed suspect");
                    let summary = self.local_summary().await;
                    let alive_event = GossipEvent::Alive(summary);
                    self.broadcast_to_all(alive_event).await;
                    return;
                }
                let mut guard = self.peers.write().await;
                do_broadcast_suspect_other(&mut guard, node_id);
            }
            GossipEvent::Alive(summary) => {
                let mut guard = self.peers.write().await;
                do_broadcast_alive(&mut guard, summary, &self.config.cluster_name);
            }
            GossipEvent::ServiceUpdate { node_id, services } => {
                let mut guard = self.peers.write().await;
                do_broadcast_service_update(&mut guard, node_id, services);
            }
        }
        let _ = self.event_tx.send(event);
    }

    async fn upsert_peer(&self, summary: &PeerSummary, addr: SocketAddr) {
        let mut guard = self.peers.write().await;
        let is_new = do_upsert_peer(&mut guard, summary, addr, &self.config.node_id);
        if is_new {
            let event = GossipEvent::Join(summary.clone());
            drop(guard);
            let _ = self.event_tx.send(event);
        }
    }

    async fn broadcast_to_all(&self, event: GossipEvent) {
        let targets = {
            let guard = self.peers.read().await;
            collect_alive_targets(&guard)
        };

        if let Some(sock) = self.get_socket().await {
            let msg = GossipMessage::Broadcast { event };
            for addr in targets {
                let _ = self.send_message(&sock, &msg, addr).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = GossipConfig::default();
        assert_eq!(config.role, "router");
        assert_eq!(config.cluster_name, "tentaflow");
        assert_eq!(config.fanout, 3);
        assert_eq!(config.ping_interval, Duration::from_secs(1));
        assert_eq!(config.ping_timeout, Duration::from_millis(500));
    }

    #[test]
    fn test_peer_summary_from_peer_info() {
        let info = PeerInfo {
            node_id: "test-1".into(),
            address: "127.0.0.1:5002".parse().unwrap(),
            hostname: "localhost".into(),
            role: "router".into(),
            services: vec!["llm".into()],
            state: PeerState::Alive,
            incarnation: 5,
            last_seen: Instant::now(),
            cluster_name: "tentaflow".into(),
        };

        let summary = PeerSummary::from(&info);
        assert_eq!(summary.node_id, "test-1");
        assert_eq!(summary.incarnation, 5);
        assert_eq!(summary.services, vec!["llm".to_string()]);
    }

    #[test]
    fn test_gossip_message_serialization() {
        let summary = PeerSummary {
            node_id: "node-1".into(),
            address: "127.0.0.1:5002".parse().unwrap(),
            hostname: "host-1".into(),
            role: "router".into(),
            services: vec![],
            incarnation: 0,
            cluster_name: "tentaflow".into(),
        };

        let msg = GossipMessage::Ping {
            sender: summary,
            seq: 42,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let decoded: GossipMessage = serde_json::from_str(&json).unwrap();

        match decoded {
            GossipMessage::Ping { sender, seq } => {
                assert_eq!(sender.node_id, "node-1");
                assert_eq!(seq, 42);
            }
            _ => panic!("Oczekiwano Ping"),
        }
    }

    #[test]
    fn test_gossip_event_serialization() {
        let event = GossipEvent::ServiceUpdate {
            node_id: "node-1".into(),
            services: vec!["llm".into(), "tts".into()],
        };
        let json = serde_json::to_string(&event).unwrap();
        let decoded: GossipEvent = serde_json::from_str(&json).unwrap();

        match decoded {
            GossipEvent::ServiceUpdate { node_id, services } => {
                assert_eq!(node_id, "node-1");
                assert_eq!(services.len(), 2);
            }
            _ => panic!("Oczekiwano ServiceUpdate"),
        }
    }

    #[tokio::test]
    async fn test_engine_creation_and_subscribe() {
        let config = GossipConfig {
            node_id: "test-node".into(),
            listen_addr: "127.0.0.1:0".parse().unwrap(),
            ..GossipConfig::default()
        };

        let engine = GossipEngine::new(config);
        let mut rx = engine.subscribe();

        // Sprawdz ze subskrypcja dziala
        let _ = engine.event_tx.send(GossipEvent::Leave("fake".into()));
        let event = rx.recv().await.unwrap();
        match event {
            GossipEvent::Leave(id) => assert_eq!(id, "fake"),
            _ => panic!("Oczekiwano Leave"),
        }
    }

    #[tokio::test]
    async fn test_get_alive_peers_empty() {
        let engine = GossipEngine::new(GossipConfig::default());
        let alive = engine.get_alive_peers().await;
        assert!(alive.is_empty());
    }

    #[tokio::test]
    async fn test_update_local_services() {
        let engine = GossipEngine::new(GossipConfig::default());
        let mut rx = engine.subscribe();

        engine
            .update_local_services(vec!["llm".into(), "tts".into()])
            .await;

        let services = engine.local_services.read().await;
        assert_eq!(services.len(), 2);
        assert_eq!(services[0], "llm");
        drop(services);

        let event = rx.recv().await.unwrap();
        match event {
            GossipEvent::ServiceUpdate { services, .. } => {
                assert_eq!(services.len(), 2);
            }
            _ => panic!("Oczekiwano ServiceUpdate"),
        }
    }
}
