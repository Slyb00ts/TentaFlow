// =============================================================================
// File: admin_ops.rs — domain operations for mesh pairing and trust management
// =============================================================================

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use std::sync::LazyLock;

use anyhow::Result;
use dashmap::DashMap;
use subtle::ConstantTimeEq;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{error, info, warn};

use crate::db::{self, DbPool};
use crate::mesh::iroh_manager::IrohMeshManager;
use crate::mesh::node_info_collector;
use crate::mesh::peer_registry::{TransportHints, TrustState};
use crate::mesh::peer_store::MeshPeerStore;
use crate::mesh::security::MeshSecurity;
use crate::net::iroh::pairing::{
    delete_pending_contact_hints, delete_trusted_contact_hints, initiate_pairing_over_iroh,
    load_pending_contact_hints, store_pending_contact_hints, store_trusted_contact_hints,
    PairingAttemptOutcome, PairingContactHints,
};

/// Per-node async lock chroniacy okno TOCTOU pomiedzy `get_pending_pairing` a
/// `initiate_pairing_with_pin` w `initiate_pairing`. Trzymane globalnie zeby
/// roznie zywane handlery nie omijaly siebie.
static PENDING_INIT_LOCKS: LazyLock<DashMap<String, Arc<AsyncMutex<()>>>> =
    LazyLock::new(|| DashMap::with_capacity(32));

fn pending_init_lock(node_id: &str) -> Arc<AsyncMutex<()>> {
    PENDING_INIT_LOCKS
        .entry(node_id.to_string())
        .or_insert_with(|| Arc::new(AsyncMutex::new(())))
        .clone()
}

/// Constant-time PIN compare. Lengths must match — for 6-digit PINs they always
/// do, but the guard keeps the function safe for any future caller.
fn pin_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

fn registry_hints_from_pairing(hints: Option<&PairingContactHints>) -> TransportHints {
    let mut out = TransportHints::default();
    if let Some(hints) = hints {
        for address in &hints.addresses {
            if let Ok(socket) = address.parse::<SocketAddr>() {
                out.addresses.push(socket);
            }
        }
        if !hints.relay_url.is_empty() {
            out.relay_url = Some(Arc::<str>::from(hints.relay_url.as_str()));
        }
        if !hints.hostname.is_empty() {
            out.hostname_dns = Some(Arc::<str>::from(hints.hostname.as_str()));
        }
    }
    out
}

pub fn mirror_trusted_peer_to_registry(
    peer_store: &MeshPeerStore,
    remote_node_id: &str,
    remote_public_key_hex: &str,
    hostname: &str,
    hints: Option<&PairingContactHints>,
) {
    let Some(reg) = peer_store.registry() else {
        return;
    };
    let mut id_bytes = [0u8; 32];
    if hex::decode_to_slice(remote_node_id, &mut id_bytes).is_err() {
        return;
    }
    reg.upsert_discovered(id_bytes, registry_hints_from_pairing(hints));
    if let Ok(pubkey_bytes) = hex::decode(remote_public_key_hex) {
        if !pubkey_bytes.is_empty() {
            reg.set_pubkey(&id_bytes, Arc::<[u8]>::from(pubkey_bytes.as_slice()));
        }
    }
    reg.set_trust(&id_bytes, TrustState::Trusted);
    if !hostname.is_empty() {
        reg.set_hostname(&id_bytes, Arc::<str>::from(hostname));
    }
}

/// Po potwierdzonym parowaniu obie strony decyduja role baseline-adopt i utrwala
/// single-flight stan adopcji. Gdy lokalny nod jest JOINEREM, zapisuje stan w
/// fazie `Elected` i (gdy mesh manager dostepny) odpala w tle transport iroh:
/// dial dawcy na ALPN_BASELINE, sekwencja `BaselineElect` -> `BaselineAck` ->
/// `BaselineHeader` + `BaselineChunk`*, zlozenie i atomowy import przez
/// `core_baseline::run_baseline_adopt`. Gdy lokalny nod jest DAWCA, zapisuje
/// tylko role — to dawca odpowiada na przychodzacy `BaselineElect` snapshotem
/// (handler ALPN_BASELINE w `iroh_manager`).
///
/// Elekcja i stan single-flight sa utrwalane SYNCHRONICZNIE (blokuja split-brain
/// natychmiast); samo pobranie snapshotu przez joinera idzie w tle (sieciowe,
/// moze trwac) i jest wznawialne przy starcie z trwalego stanu `Elected`.
fn begin_baseline_adopt_after_confirm(
    db: &DbPool,
    remote_node_id: &str,
    quic_mesh: &Option<Arc<IrohMeshManager>>,
) {
    use crate::sync::core_baseline::{begin_adopt_atomic, BaselinePhase, BaselineRole, BeginOutcome};

    let donor_epoch = crate::sync::runtime::core_epoch();

    // Content-aware auto-election. The old "lowest node_id is donor" rule was
    // data-blind: a freshly installed (empty) node with a lower id was elected
    // donor over a populated peer, so the data-holder adopted the empty baseline
    // and lost its content. Instead BOTH sides arm as JOINER and dial the peer;
    // the authoritative role is settled in the baseline transport, where the
    // donor session compares ledger op counts (carried in `BaselineElect`) and
    // serves only when it genuinely holds more content. The empty node's pull is
    // answered with a snapshot; the data-holder's reciprocal pull is refused by
    // the empty peer (it is not the rightful donor), so content flows one way:
    // data-holder -> empty node. Two populated nodes auto-pairing do NOT auto-
    // adopt (the would-be joiner's pull is refused both ways) — merging two
    // populated nodes stays an explicit admin action (`admin_start_baseline_adopt`).
    match begin_adopt_atomic(
        db,
        BaselineRole::Joiner,
        remote_node_id,
        &donor_epoch,
        BaselinePhase::Elected,
    ) {
        Ok(BeginOutcome::Started) | Ok(BeginOutcome::Resume(_)) => {}
        Err(e) => {
            warn!(
                peer = %remote_node_id,
                "baseline adopt: atomic election failed (single-flight conflict?): {}",
                e
            );
            return;
        }
    }

    info!(
        peer = %remote_node_id,
        "baseline adopt: arming as JOINER — pulling peer baseline in background (donor settled by content)"
    );
    if let Some(qm) = quic_mesh.clone() {
        let donor_node_id = remote_node_id.to_string();
        let epoch_seen = donor_epoch.counter;
        tokio::spawn(async move {
            pull_baseline_with_hint_retry(&qm, &donor_node_id, epoch_seen).await;
        });
    } else {
        warn!(
            peer = %remote_node_id,
            "baseline adopt: brak mesh managera — joiner wznowi pull przy starcie"
        );
    }
}

/// Pulls the donor baseline, retrying with backoff while the donor's contact hints
/// have not yet been resolved. Right after pairing-confirm the donor's network address
/// (contact hints) arrives a moment later via NodeInfo/mesh, so an immediate pull often
/// fails with "no trusted contact hints" even though the donor is reachable seconds later.
/// We retry SPECIFICALLY that not-yet-resolved case with a bounded backoff (~capped at
/// roughly a minute), succeeding as soon as the hints land. Any other error (or exhausting
/// the retries) falls through to the durable `Elected` state, which the startup resume
/// finishes — so this only shortens the common "hints arrive late" delay, never replaces
/// the resume fallback.
async fn pull_baseline_with_hint_retry(
    qm: &Arc<IrohMeshManager>,
    donor_node_id: &str,
    epoch_seen: u64,
) {
    // Backoff schedule between attempts: 2s, 4s, 8s, 16s, 16s — ~46s total wall time,
    // bounded so a genuinely absent donor does not retry forever (startup resume covers it).
    const BACKOFF_SECS: [u64; 5] = [2, 4, 8, 16, 16];
    let mut attempt = 0usize;
    loop {
        match qm.pull_baseline_from_donor(donor_node_id, epoch_seen).await {
            Ok(()) => {
                if attempt > 0 {
                    info!(
                        donor = %donor_node_id,
                        attempt = attempt + 1,
                        "baseline adopt: pull succeeded after waiting for donor contact hints"
                    );
                }
                return;
            }
            Err(e) => {
                // Retry transients that occur right after pairing while the iroh path is
                // still settling: contact hints not yet resolved, OR the freshly-opened
                // baseline stream dropping (relay->direct path switch) before/while the
                // snapshot transfers. Both clear within seconds once the link stabilizes.
                let es = e.to_string();
                let retryable = es.contains("no trusted contact hints for donor")
                    || es.contains("connection lost")
                    || es.contains("connection reset")
                    || es.contains("connection closed")
                    || es.contains("ConnectionLost")
                    || es.contains("timed out")
                    || es.contains("timeout");
                if retryable && attempt < BACKOFF_SECS.len() {
                    let delay = BACKOFF_SECS[attempt];
                    attempt += 1;
                    info!(
                        donor = %donor_node_id,
                        attempt,
                        delay_secs = delay,
                        reason = %es,
                        "baseline adopt: transient pull error — retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                    continue;
                }
                warn!(
                    donor = %donor_node_id,
                    attempts = attempt + 1,
                    "baseline adopt: pull failed (peer may be the joiner, or resume at startup): {}",
                    e
                );
                return;
            }
        }
    }
}

async fn send_pairing_bootstrap(
    qm: &Arc<IrohMeshManager>,
    security: &Arc<MeshSecurity>,
    target_node_id: &str,
    local_node_id: &str,
) -> Result<(), AdminError> {
    let local_info = node_info_collector::collect_node_info(local_node_id);
    let info_bytes = crate::mesh::cbor::encode(&local_info).map_err(|e| {
        error!(target_node = %target_node_id, "CBOR encode NodeInfo failed: {}", e);
        AdminError::new(AdminErrorKind::Internal, "internal mesh error")
    })?;
    qm.send_node_info(target_node_id, &info_bytes)
        .await
        .map_err(|e| {
            warn!(
                target_node = %target_node_id,
                "NodeInfo send after pairing failed: {}",
                e
            );
            AdminError::new(
                AdminErrorKind::DeliveryFailed,
                "pairing completed, but node info exchange failed",
            )
        })?;

    let all_keys = security.get_all_trusted_keys();
    if !all_keys.is_empty() {
        let entries: Vec<tentaflow_protocol::mesh::TrustedKeyEntry> = all_keys
            .iter()
            .map(|(nid, pk)| tentaflow_protocol::mesh::TrustedKeyEntry {
                node_id: nid.clone(),
                public_key_hex: pk.clone(),
            })
            .collect();
        let payload = tentaflow_protocol::mesh::TrustedKeysSyncPayload { keys: entries };
        if let Ok(sync_data) = crate::mesh::cbor::encode(&payload) {
            if let Err(e) = qm.send_trusted_keys_sync(target_node_id, &sync_data).await {
                warn!(
                    target_node = %target_node_id,
                    "TrustedKeysSync after pairing failed: {}",
                    e
                );
            }
            qm.broadcast_ufp2_to_trusted(
                tentaflow_protocol::mesh::MESH_MSG_TRUSTED_KEYS_SYNC,
                &sync_data,
                Some(target_node_id),
            )
            .await;
        }
    }

    Ok(())
}

/// SSRF / hostile-network guard for raw IPs given by the client. Rejects
/// loopback, unspecified, IPv4 link-local, IPv6 link-local. Mirrors the logic
/// in `mesh_connect`.
fn is_safe_remote_ip(ip: IpAddr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() {
        return false;
    }
    match ip {
        IpAddr::V4(v4) => !v4.is_link_local(),
        IpAddr::V6(v6) => {
            // fe80::/10 — link local. `is_unicast_link_local` jest unstable, robimy recznie.
            let seg0 = v6.segments()[0];
            (seg0 & 0xffc0) != 0xfe80
        }
    }
}

fn validate_remote_addresses(addrs: &[String]) -> Result<(), AdminError> {
    for s in addrs {
        let parsed: SocketAddr = s.parse().map_err(|_| {
            AdminError::new(
                AdminErrorKind::BadRequest,
                "remote address is not a valid IP:port",
            )
        })?;
        if !is_safe_remote_ip(parsed.ip()) {
            return Err(AdminError::new(
                AdminErrorKind::BadRequest,
                "remote address rejected (loopback/unspecified/link-local)",
            ));
        }
    }
    Ok(())
}

fn validate_remote_relay_url(url_str: &str) -> Result<(), AdminError> {
    if url_str.is_empty() {
        return Ok(());
    }
    let parsed = url::Url::parse(url_str).map_err(|_| {
        AdminError::new(
            AdminErrorKind::BadRequest,
            "remote_relay_url is not a valid URL",
        )
    })?;
    if parsed.scheme() != "https" {
        return Err(AdminError::new(
            AdminErrorKind::BadRequest,
            "remote_relay_url must use https scheme",
        ));
    }
    let host = parsed.host_str().ok_or_else(|| {
        AdminError::new(AdminErrorKind::BadRequest, "remote_relay_url missing host")
    })?;
    // Direct IP literal — apply SSRF guard.
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !is_safe_remote_ip(ip) {
            return Err(AdminError::new(
                AdminErrorKind::BadRequest,
                "remote_relay_url host rejected (loopback/unspecified/link-local)",
            ));
        }
    } else {
        // DNS name — apply same charset rules as hostname validation.
        validate_hostname(host)?;
    }
    Ok(())
}

fn validate_hostname(name: &str) -> Result<(), AdminError> {
    if name.is_empty() {
        return Ok(());
    }
    if name.len() > 253 {
        return Err(AdminError::new(
            AdminErrorKind::BadRequest,
            "remote_hostname exceeds 253 chars",
        ));
    }
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphanumeric() {
        return Err(AdminError::new(
            AdminErrorKind::BadRequest,
            "remote_hostname must start with alphanumeric",
        ));
    }
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '-' || c == '.') {
            return Err(AdminError::new(
                AdminErrorKind::BadRequest,
                "remote_hostname has illegal character",
            ));
        }
    }
    Ok(())
}

/// Wynik inicjacji parowania zwracany do warstwy dispatch.
pub struct InitiateOutcome {
    pub pin: String,
    pub completed: bool,
}

/// Wynik potwierdzenia parowania — zaufany identyfikator dla GUI.
pub struct ConfirmOutcome {
    pub trusted_node_id: String,
}

/// Klasa bledu operacji admina mesh — mapowana na `ProtocolError` w warstwie
/// dispatch. Trzymamy ja niezaleznie od `ProtocolError`, zeby `mesh::admin_ops`
/// nie zalezalo od `tentaflow-protocol`.
#[derive(Debug)]
pub enum AdminErrorKind {
    BadRequest,
    AlreadyPending,
    RateLimited,
    BadPin,
    DeliveryFailed,
    MeshUnavailable,
    Internal,
}

#[derive(Debug)]
pub struct AdminError {
    pub kind: AdminErrorKind,
    pub message: String,
}

impl AdminError {
    fn new(kind: AdminErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for AdminError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AdminError {}

/// Walidacja identyfikatora — chroni przed path-traversal i znakami kontrolnymi.
fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() < 256
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

fn local_contact_hints(
    local_node_id: &str,
    peer_store: &MeshPeerStore,
    qm: &Arc<IrohMeshManager>,
) -> PairingContactHints {
    let peer = peer_store.get(local_node_id);
    let (hostname, addresses) = match peer {
        Some(peer) => (
            peer.hostname,
            peer.addresses
                .iter()
                .map(|ip| format!("{}:{}", ip, peer.port))
                .collect(),
        ),
        None => (String::new(), Vec::new()),
    };
    PairingContactHints {
        node_id: local_node_id.to_string(),
        public_key_hex: String::new(),
        hostname,
        addresses,
        relay_url: qm
            .relay_url()
            .map(|url| url.to_string())
            .unwrap_or_default(),
    }
}

fn remote_contact_hints(
    remote_node_id: &str,
    remote_public_key: &str,
    remote_addresses: &[String],
    remote_relay_url: &str,
    remote_hostname: &str,
    peer_store: &MeshPeerStore,
    local_relay_url: &str,
) -> PairingContactHints {
    if !remote_addresses.is_empty() || !remote_relay_url.is_empty() || !remote_hostname.is_empty() {
        return PairingContactHints {
            node_id: remote_node_id.to_string(),
            public_key_hex: remote_public_key.to_string(),
            hostname: remote_hostname.to_string(),
            addresses: remote_addresses.to_vec(),
            relay_url: if remote_relay_url.is_empty() {
                local_relay_url.to_string()
            } else {
                remote_relay_url.to_string()
            },
        };
    }

    if let Some(peer) = peer_store.get(remote_node_id) {
        return PairingContactHints {
            node_id: remote_node_id.to_string(),
            public_key_hex: remote_public_key.to_string(),
            hostname: peer.hostname,
            addresses: if peer.port > 0 {
                peer.addresses
                    .iter()
                    .map(|ip| format!("{}:{}", ip, peer.port))
                    .collect()
            } else {
                Vec::new()
            },
            relay_url: if remote_relay_url.is_empty() {
                local_relay_url.to_string()
            } else {
                remote_relay_url.to_string()
            },
        };
    }

    PairingContactHints {
        node_id: remote_node_id.to_string(),
        public_key_hex: remote_public_key.to_string(),
        hostname: remote_hostname.to_string(),
        addresses: remote_addresses.to_vec(),
        relay_url: if remote_relay_url.is_empty() {
            local_relay_url.to_string()
        } else {
            remote_relay_url.to_string()
        },
    }
}

/// Rozpoczyna parowanie: generuje PIN i wysyla `PairingRequest` przez QUIC
/// (istniejacy mesh stream) lub ALPN_PAIRING (gdy znamy hinty transportu z QR).
#[allow(clippy::too_many_arguments)]
pub async fn initiate_pairing(
    pool: &DbPool,
    security: &Arc<MeshSecurity>,
    remote_node_id: &str,
    remote_public_key: &str,
    remote_addresses: &[String],
    remote_relay_url: &str,
    remote_hostname: &str,
    quic_mesh: &Option<Arc<IrohMeshManager>>,
    local_node_id: &str,
    peer_store: &MeshPeerStore,
    pin_hint: &str,
) -> Result<InitiateOutcome, AdminError> {
    if !is_valid_id(remote_node_id) {
        return Err(AdminError::new(
            AdminErrorKind::BadRequest,
            "invalid node_id",
        ));
    }

    // Validate user-controlled transport hints BEFORE any DB write or I/O.
    validate_remote_addresses(remote_addresses)?;
    validate_remote_relay_url(remote_relay_url)?;
    validate_hostname(remote_hostname)?;

    // Per-node lock closes the TOCTOU window between get_pending_pairing and
    // initiate_pairing_with_pin. Held until pin row is committed below.
    let init_lock = pending_init_lock(remote_node_id);
    let _init_guard = init_lock.lock().await;

    if let Ok(Some(_)) = db::repository::get_pending_pairing(pool, remote_node_id) {
        return Err(AdminError::new(
            AdminErrorKind::AlreadyPending,
            "pairing already in progress for this node — wait or reject",
        ));
    }

    let pin = security
        .initiate_pairing_with_pin(remote_node_id, pin_hint)
        .map_err(|e| {
            error!(target_node = %remote_node_id, "initiate_pairing_with_pin failed: {}", e);
            AdminError::new(AdminErrorKind::Internal, "failed to initialize pairing")
        })?;
    let mut completed = false;

    let qm = match quic_mesh {
        Some(qm) => qm,
        None => {
            let _ = db::repository::delete_pending_pairing(&security.db, remote_node_id);
            return Err(AdminError::new(
                AdminErrorKind::MeshUnavailable,
                "Mesh manager niedostepny",
            ));
        }
    };

    let local_hints = local_contact_hints(local_node_id, peer_store, qm);
    let remote_hints = remote_contact_hints(
        remote_node_id,
        remote_public_key,
        remote_addresses,
        remote_relay_url,
        remote_hostname,
        peer_store,
        &local_hints.relay_url,
    );
    let can_use_existing_mesh = remote_addresses.is_empty()
        && remote_relay_url.is_empty()
        && remote_hostname.is_empty()
        && qm.is_connected(remote_node_id).await;

    if can_use_existing_mesh {
        let payload = tentaflow_protocol::mesh::MeshPairingRequestPayload {
            from_node_id: security.ed25519_public_key_hex(),
            public_key: security.public_key_hex(),
            pin: pin.clone(),
        };
        let data = crate::mesh::cbor::encode(&payload).map_err(|e| {
            error!(target_node = %remote_hints.node_id, "CBOR encode PairingRequest failed: {}", e);
            AdminError::new(AdminErrorKind::Internal, "internal mesh error")
        })?;
        info!(target_node = %remote_hints.node_id, "pairing: sending PairingRequest via existing mesh stream");
        if let Err(e) = qm.send_pairing_request(&remote_hints.node_id, &data).await {
            warn!(target_node = %remote_hints.node_id, "PairingRequest via mesh failed: {}", e);
            let _ = db::repository::delete_pending_pairing(&security.db, remote_node_id);
            return Err(AdminError::new(
                AdminErrorKind::DeliveryFailed,
                "failed to deliver PairingRequest — node may be unreachable",
            ));
        }
    } else {
        info!(target_node = %remote_hints.node_id, "Parowanie: wysylam FirstContact przez ALPN_PAIRING");
        match initiate_pairing_over_iroh(
            qm.endpoint(),
            &remote_hints,
            security.as_ref(),
            &pin,
            &local_hints.hostname,
            local_hints.addresses.clone(),
            local_hints.relay_url.clone(),
        )
        .await
        {
            Ok(PairingAttemptOutcome::Confirmed) => {
                store_trusted_contact_hints(&security.db, remote_node_id, &remote_hints)
                    .map_err(|e| {
                        error!(target_node = %remote_hints.node_id, "store_trusted_contact_hints failed: {}", e);
                        AdminError::new(AdminErrorKind::Internal, "internal mesh error")
                    })?;
                let trusted_public_key =
                    db::repository::get_trusted_node_public_key(&security.db, remote_node_id)
                        .map_err(|e| {
                            error!(target_node = %remote_hints.node_id, "load trusted pubkey failed: {}", e);
                            AdminError::new(AdminErrorKind::Internal, "internal mesh error")
                        })?
                        .unwrap_or_else(|| remote_hints.public_key_hex.clone());
                mirror_trusted_peer_to_registry(
                    peer_store,
                    remote_node_id,
                    &trusted_public_key,
                    &remote_hints.hostname,
                    Some(&remote_hints),
                );
                qm.connect_to_peer_with_hints(&remote_hints)
                    .await
                    .map_err(|e| {
                        warn!(
                            target_node = %remote_hints.node_id,
                            "Pairing confirmed, but mesh connect failed: {}",
                            e
                        );
                        AdminError::new(
                            AdminErrorKind::DeliveryFailed,
                            "pairing completed, but mesh connection is not ready",
                        )
                    })?;
                send_pairing_bootstrap(qm, security, &remote_hints.node_id, local_node_id).await?;
                begin_baseline_adopt_after_confirm(&security.db, &remote_hints.node_id, quic_mesh);
                completed = true;
            }
            Ok(PairingAttemptOutcome::Pending) => {
                if !pin_hint.is_empty() {
                    let _ = delete_pending_contact_hints(&security.db, remote_node_id);
                    let _ = db::repository::delete_pending_pairing(&security.db, remote_node_id);
                    return Err(AdminError::new(
                        AdminErrorKind::AlreadyPending,
                        "Zdalny node nie potwierdzil zaproszenia QR — sprawdz czy PIN i kod sa nadal aktualne",
                    ));
                }
                store_pending_contact_hints(&security.db, remote_node_id, &remote_hints)
                    .map_err(|e| {
                        error!(target_node = %remote_hints.node_id, "store_pending_contact_hints failed: {}", e);
                        AdminError::new(AdminErrorKind::Internal, "internal mesh error")
                    })?;
            }
            Err(e) => {
                warn!(target_node = %remote_hints.node_id, "PairingRequest delivery failed: {}", e);
                let _ = delete_pending_contact_hints(&security.db, remote_node_id);
                let _ = db::repository::delete_pending_pairing(&security.db, remote_node_id);
                return Err(AdminError::new(
                    AdminErrorKind::DeliveryFailed,
                    "Nie udało się wysłać PairingRequest — node może nie być osiągalny",
                ));
            }
        }
    }

    Ok(InitiateOutcome {
        pin: if completed { String::new() } else { pin },
        completed,
    })
}

/// Potwierdza parowanie (rate-limit PIN, walidacja, sync kluczy w tle).
/// Hostname pobierany z `peer_store` po sparowaniu — eliminuje duplikat pola
/// w protokole. Fallback do pustego stringa gdy peer nieznany.
pub async fn confirm_pairing(
    security: &Arc<MeshSecurity>,
    remote_node_id: &str,
    pin: Option<&str>,
    quic_mesh: &Option<Arc<IrohMeshManager>>,
    local_node_id: &str,
    peer_store: &MeshPeerStore,
) -> Result<ConfirmOutcome, AdminError> {
    info!(
        remote_node_id = %remote_node_id,
        len = remote_node_id.len(),
        "confirm_pairing: start"
    );
    if !is_valid_id(remote_node_id) {
        warn!(
            "confirm_pairing: is_valid_id rejected remote_node_id={:?} bytes={:?}",
            remote_node_id,
            remote_node_id.as_bytes()
        );
        return Err(AdminError::new(
            AdminErrorKind::BadRequest,
            "invalid node_id",
        ));
    }

    if !security.check_pin_rate_limit(remote_node_id) {
        return Err(AdminError::new(
            AdminErrorKind::RateLimited,
            "too many attempts — wait 60 seconds",
        ));
    }

    // CR-001: gate on stored_pin presence. If pending pairing expired or never
    // existed, we MUST refuse — silently accepting None lets an attacker who
    // knows node_id bypass PIN validation entirely.
    let expected = security
        .get_pending_pin(remote_node_id)
        .ok()
        .flatten()
        .ok_or_else(|| AdminError::new(AdminErrorKind::BadPin, "no pending pairing"))?;

    let provided = pin.unwrap_or("");
    // CR-006: constant-time compare — counter for the rate limiter is bumped
    // by check_pin_rate_limit above (single source of truth in security.rs).
    if !pin_eq(provided, &expected) {
        return Err(AdminError::new(AdminErrorKind::BadPin, "invalid PIN"));
    }

    let remote_public_key =
        db::repository::get_setting(&security.db, &format!("pending_pubkey:{}", remote_node_id))
            .ok()
            .flatten()
            .unwrap_or_default();

    if remote_public_key.is_empty() {
        return Err(AdminError::new(
            AdminErrorKind::BadRequest,
            "missing initiator public key — cannot confirm pairing",
        ));
    }

    let hostname = peer_store.get_hostname(remote_node_id).unwrap_or_default();

    security
        .confirm_pairing(remote_node_id, &remote_public_key, &hostname, "admin")
        .map_err(|e| {
            error!(target_node = %remote_node_id, "security.confirm_pairing failed: {}", e);
            AdminError::new(AdminErrorKind::BadRequest, "failed to confirm pairing")
        })?;

    let pending_hints = load_pending_contact_hints(&security.db, remote_node_id)
        .ok()
        .flatten();
    if let Some(ref hints) = pending_hints {
        let _ = store_trusted_contact_hints(&security.db, remote_node_id, hints);
    }
    mirror_trusted_peer_to_registry(
        peer_store,
        remote_node_id,
        &remote_public_key,
        &hostname,
        pending_hints.as_ref(),
    );

    // Receiver-side election: arm the single-flight baseline-adopt state BEFORE any
    // unreliable network send/bootstrap below. The peer is already trusted and the
    // pending row is cleared, so if a send fails afterwards the receiver still has a
    // durable `Elected` row that krok 2 can resume from — instead of a trusted peer
    // with no adopt state and no retry. `decide_roles` is pure, so both ends compute
    // the identical donor/joiner split.
    begin_baseline_adopt_after_confirm(&security.db, remote_node_id, quic_mesh);

    if let Some(ref qm) = quic_mesh {
        if let Some(ref hints) = pending_hints {
            qm.connect_to_peer_with_hints(hints).await.map_err(|e| {
                warn!(
                    target_node = %remote_node_id,
                    "mesh connect after confirm failed: {}",
                    e
                );
                AdminError::new(
                    AdminErrorKind::DeliveryFailed,
                    "pairing confirmed, but mesh connection is not ready",
                )
            })?;
        }

        let payload = tentaflow_protocol::mesh::MeshPairingConfirmPayload {
            from_node_id: security.ed25519_public_key_hex(),
            public_key: security.public_key_hex(),
            hostname: hostname.clone(),
            pin: provided.to_string(),
        };
        let data = match crate::mesh::cbor::encode(&payload) {
            Ok(d) => d,
            Err(e) => {
                error!(target_node = %remote_node_id, "CBOR encode PairingConfirm failed: {}", e);
                return Err(AdminError::new(
                    AdminErrorKind::Internal,
                    "internal mesh error",
                ));
            }
        };
        qm.send_pairing_confirm(remote_node_id, &data)
            .await
            .map_err(|e| {
                warn!(
                    target_node = %remote_node_id,
                    "PairingConfirm send failed: {}",
                    e
                );
                AdminError::new(
                    AdminErrorKind::DeliveryFailed,
                    "pairing confirmed, but confirmation delivery failed",
                )
            })?;

        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        send_pairing_bootstrap(qm, security, remote_node_id, local_node_id).await?;
    }

    let _ =
        db::repository::delete_setting(&security.db, &format!("pending_pubkey:{}", remote_node_id));
    let _ = delete_pending_contact_hints(&security.db, remote_node_id);

    Ok(ConfirmOutcome {
        trusted_node_id: remote_node_id.to_string(),
    })
}

/// Odrzuca parowanie i wysyla `PairingReject` w tle.
pub fn reject_pairing(
    security: &Arc<MeshSecurity>,
    remote_node_id: &str,
    quic_mesh: &Option<Arc<IrohMeshManager>>,
) -> Result<(), AdminError> {
    if !is_valid_id(remote_node_id) {
        return Err(AdminError::new(
            AdminErrorKind::BadRequest,
            "invalid node_id",
        ));
    }

    security.reject_pairing(remote_node_id).map_err(|e| {
        error!(target_node = %remote_node_id, "reject_pairing failed: {}", e);
        AdminError::new(AdminErrorKind::Internal, "internal mesh error")
    })?;
    let pending_hints = load_pending_contact_hints(&security.db, remote_node_id)
        .ok()
        .flatten();

    if let Some(ref qm) = quic_mesh {
        let payload = tentaflow_protocol::mesh::MeshPairingRejectPayload {
            from_node_id: security.ed25519_public_key_hex(),
        };
        let data = crate::mesh::cbor::encode(&payload).map_err(|e| {
            error!(target_node = %remote_node_id, "CBOR encode PairingReject failed: {}", e);
            AdminError::new(AdminErrorKind::Internal, "internal mesh error")
        })?;
        let qm = qm.clone();
        let node_id = remote_node_id.to_string();
        let pending_hints = pending_hints.clone();
        tokio::spawn(async move {
            if let Some(hints) = pending_hints {
                if let Err(e) = qm.connect_to_peer_with_hints(&hints).await {
                    warn!("Blad laczenia do peera z pending hints {}: {}", node_id, e);
                }
            }
            if let Err(e) = qm.send_pairing_reject(&node_id, &data).await {
                warn!("Blad wysylania PairingReject przez QUIC: {}", e);
            }
        });
    }
    let _ = delete_pending_contact_hints(&security.db, remote_node_id);

    Ok(())
}

/// Cofa zaufanie i broadcastuje TrustRevoked. Audyt zapisywany jako pierwsza
/// operacja, zeby cofniecie bylo widoczne nawet gdy QUIC delivery zawiedzie.
pub fn revoke_trust(
    security: &Arc<MeshSecurity>,
    node_id: &str,
    quic_mesh: &Option<Arc<IrohMeshManager>>,
    local_node_id: &str,
) -> Result<(), AdminError> {
    if !is_valid_id(node_id) {
        return Err(AdminError::new(
            AdminErrorKind::BadRequest,
            "Niepoprawny node_id",
        ));
    }

    let _ = crate::db::repository::log_audit(
        &security.db,
        None,
        None,
        "trust_revoked",
        None,
        Some(&format!("Cofnieto zaufanie dla {} przez admina", node_id)),
        None,
        Some(node_id),
    );

    if let Some(ref qm) = quic_mesh {
        let _ = local_node_id;
        let payload = tentaflow_protocol::mesh::TrustRevokedPayload {
            revoked_node_id: node_id.to_string(),
            from_node_id: security.ed25519_public_key_hex(),
        };
        let qm = qm.clone();
        let sec = security.clone();
        let data = crate::mesh::cbor::encode(&payload).unwrap_or_default();
        let revoked_id = node_id.to_string();
        security.mark_revoking(node_id);
        tokio::spawn(async move {
            // Wyslij PRZED revoke — klucze szyfrowania jeszcze istnieja.
            if let Err(e) = qm
                .send_ufp2_to_peer(
                    &revoked_id,
                    tentaflow_protocol::mesh::MESH_MSG_TRUST_REVOKED,
                    &data,
                )
                .await
            {
                warn!(
                    "Blad wysylania TrustRevoked do revokowanego {}: {}",
                    revoked_id, e
                );
            }
            qm.broadcast_ufp2_to_trusted(
                tentaflow_protocol::mesh::MESH_MSG_TRUST_REVOKED,
                &data,
                Some(&revoked_id),
            )
            .await;

            if let Err(e) = sec.unpair(&revoked_id) {
                warn!("Blad unpair dla {}: {}", revoked_id, e);
            }
            // Drop the revoked node's advertised robots immediately so the resolver
            // can't route a command to it before the QUIC idle disconnect fires.
            crate::mesh::robot_dispatch::global().remove_node(&revoked_id);
            sec.clear_revoking(&revoked_id);
            // Nie disconnectujemy — kaskadowe disconnect powodowaly failujace broadcasty.
            // Connection umrze po QUIC idle timeout (60s).
        });
    } else {
        security.mark_revoking(node_id);
        security.unpair(node_id).map_err(|e| {
            error!(target_node = %node_id, "security.unpair failed: {}", e);
            AdminError::new(AdminErrorKind::Internal, "internal mesh error")
        })?;
        crate::mesh::robot_dispatch::global().remove_node(node_id);
        security.clear_revoking(node_id);
    }
    let _ = delete_trusted_contact_hints(&security.db, node_id);

    Ok(())
}

/// Przywraca zaufanie po revocation (admin override).
pub fn retrust(security: &Arc<MeshSecurity>, node_id: &str) -> Result<(), AdminError> {
    if !is_valid_id(node_id) {
        return Err(AdminError::new(
            AdminErrorKind::BadRequest,
            "Niepoprawny node_id",
        ));
    }

    security.admin_retrust(node_id).map_err(|e| {
        error!(target_node = %node_id, "admin_retrust failed: {}", e);
        AdminError::new(AdminErrorKind::Internal, "internal mesh error")
    })?;
    Ok(())
}

#[cfg(test)]
mod baseline_adopt_admin_tests {
    use super::*;
    use crate::sync::core_baseline::{load_adopt_state, BaselinePhase, BaselineRole};

    fn setup_test_db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::migrations::run(&conn).unwrap();
        Arc::new(crate::db::Db::from_connection(conn))
    }

    fn test_cipher() -> Arc<crate::crypto::SettingsCipher> {
        Arc::new(crate::crypto::SettingsCipher::new(&[0u8; 32]))
    }

    /// Buduje MeshSecurity z jednym zaufanym peerem `donor` (valid Ed25519 key
    /// pozyczony z drugiej, niezależnej tożsamości).
    fn security_with_trusted_donor(db: &DbPool, donor: &str) -> Arc<MeshSecurity> {
        let security = Arc::new(MeshSecurity::new(db.clone(), test_cipher()).unwrap());
        let other_db = setup_test_db();
        let other = MeshSecurity::new(other_db, test_cipher()).unwrap();
        security
            .add_trusted_key(donor, &other.public_key_hex(), "donor-host")
            .unwrap();
        security
    }

    #[test]
    fn rejects_untrusted_donor() {
        let db = setup_test_db();
        let security = Arc::new(MeshSecurity::new(db.clone(), test_cipher()).unwrap());
        let err = admin_start_baseline_adopt(&db, &security, "local-node", "donor-node", &None)
            .expect_err("untrusted donor must be rejected");
        assert!(matches!(err.kind, AdminErrorKind::BadRequest));
        // No state must be written when the donor is rejected.
        assert!(load_adopt_state(&db).unwrap().is_none());
    }

    #[test]
    fn rejects_self_as_donor() {
        let db = setup_test_db();
        let security = security_with_trusted_donor(&db, "local-node");
        let err = admin_start_baseline_adopt(&db, &security, "local-node", "local-node", &None)
            .expect_err("self donor must be rejected");
        assert!(matches!(err.kind, AdminErrorKind::BadRequest));
    }

    #[test]
    fn starts_adopt_as_joiner_for_trusted_donor() {
        let db = setup_test_db();
        let security = security_with_trusted_donor(&db, "donor-node");
        let outcome = admin_start_baseline_adopt(&db, &security, "local-node", "donor-node", &None)
            .expect("trusted donor must start adopt");
        assert!(outcome.started);

        let state = load_adopt_state(&db).unwrap().expect("state persisted");
        assert_eq!(state.role, BaselineRole::Joiner);
        assert_eq!(state.peer, "donor-node");
        assert_eq!(state.phase, BaselinePhase::Elected);
    }

    #[test]
    fn second_start_with_different_donor_conflicts_single_flight() {
        let db = setup_test_db();
        let security = Arc::new(MeshSecurity::new(db.clone(), test_cipher()).unwrap());
        let other_db = setup_test_db();
        let other = MeshSecurity::new(other_db, test_cipher()).unwrap();
        security
            .add_trusted_key("donor-a", &other.public_key_hex(), "a")
            .unwrap();
        let other_db2 = setup_test_db();
        let other2 = MeshSecurity::new(other_db2, test_cipher()).unwrap();
        security
            .add_trusted_key("donor-b", &other2.public_key_hex(), "b")
            .unwrap();

        admin_start_baseline_adopt(&db, &security, "local-node", "donor-a", &None)
            .expect("first start ok");
        let err = admin_start_baseline_adopt(&db, &security, "local-node", "donor-b", &None)
            .expect_err("second start with different donor must conflict");
        assert!(matches!(err.kind, AdminErrorKind::AlreadyPending));
    }
}

/// Wynik admina-inicjowanej adopcji baseline'u.
#[derive(Debug)]
pub struct AdoptStartOutcome {
    /// `true` gdy nowa adopcja faktycznie ruszyla (lokalny nod jako joiner,
    /// pull snapshotu wystartowal w tle). `false` gdy to wznowienie istniejacej.
    pub started: bool,
    pub message: String,
}

/// Admin-inicjowana adopcja baseline'u od JAWNIE wskazanego dawcy. W odroznieniu
/// od `begin_baseline_adopt_after_confirm` (auto po pairingu, role z nizszego
/// node_id) admin podaje dawce explicit: `decide_roles(.., proposed_donor=donor)`
/// wymusza, ze wskazany peer jest dawca, a lokalny nod joinerem.
///
/// Bezpieczenstwo: dawca MUSI byc zaufanym sparowanym peerem (`is_trusted`);
/// inaczej odrzucamy. Stan single-flight jest utrwalany atomowo (`begin_adopt_atomic`),
/// wiec rownolegly start o innym celu dostaje twardy konflikt. Sam pull snapshotu
/// idzie w tle (sieciowy, wznawialny przy starcie ze stanu `Elected`).
pub fn admin_start_baseline_adopt(
    db: &DbPool,
    security: &Arc<MeshSecurity>,
    local_node_id: &str,
    donor_node_id: &str,
    quic_mesh: &Option<Arc<IrohMeshManager>>,
) -> Result<AdoptStartOutcome, AdminError> {
    use crate::sync::core_baseline::{
        begin_adopt_atomic, decide_roles, local_role, BaselinePhase, BaselineRole, BeginOutcome,
    };

    if !is_valid_id(donor_node_id) {
        return Err(AdminError::new(
            AdminErrorKind::BadRequest,
            "Niepoprawny donor_node_id",
        ));
    }
    if donor_node_id == local_node_id {
        return Err(AdminError::new(
            AdminErrorKind::BadRequest,
            "donor == local node — nie ma od kogo adoptowac",
        ));
    }
    if !security.is_trusted(donor_node_id) {
        return Err(AdminError::new(
            AdminErrorKind::BadRequest,
            "wskazany dawca nie jest zaufanym sparowanym peerem",
        ));
    }

    let donor_epoch = crate::sync::runtime::core_epoch();
    // Admin wskazuje dawce jawnie — proposed_donor wymusza role.
    let (donor, _joiner) = decide_roles(local_node_id, donor_node_id, Some(donor_node_id));
    let role = local_role(local_node_id, &donor);
    if role != BaselineRole::Joiner {
        // decide_roles z proposed_donor=donor_node_id zawsze daje joinera lokalnie;
        // ta galaz to obrona przed regresja, nie realny przeplyw.
        return Err(AdminError::new(
            AdminErrorKind::Internal,
            "internal mesh error",
        ));
    }

    match begin_adopt_atomic(
        db,
        BaselineRole::Joiner,
        donor_node_id,
        &donor_epoch,
        BaselinePhase::Elected,
    ) {
        Ok(BeginOutcome::Started) => {}
        Ok(BeginOutcome::Resume(_)) => {
            return Ok(AdoptStartOutcome {
                started: false,
                message: "adopcja juz w toku z tym dawca — wznawiam".to_string(),
            });
        }
        Err(e) => {
            return Err(AdminError::new(
                AdminErrorKind::AlreadyPending,
                format!("adopcja zablokowana (single-flight): {e}"),
            ));
        }
    }

    let _ = crate::db::repository::log_audit(
        &security.db,
        None,
        None,
        "baseline_adopt_started",
        None,
        Some(&format!(
            "Admin rozpoczal adopcje baseline'u od dawcy {donor_node_id}"
        )),
        None,
        Some(donor_node_id),
    );

    if let Some(qm) = quic_mesh.clone() {
        let donor = donor_node_id.to_string();
        let epoch_seen = donor_epoch.counter;
        tokio::spawn(async move {
            if let Err(e) = qm.pull_baseline_from_donor(&donor, epoch_seen).await {
                warn!(
                    donor = %donor,
                    "baseline adopt (admin): pull nieudany (wznowi przy starcie): {}",
                    e
                );
            }
        });
    } else {
        warn!(
            donor = %donor_node_id,
            "baseline adopt (admin): brak mesh managera — joiner wznowi pull przy starcie"
        );
    }

    Ok(AdoptStartOutcome {
        started: true,
        message: "adopcja rozpoczeta".to_string(),
    })
}
