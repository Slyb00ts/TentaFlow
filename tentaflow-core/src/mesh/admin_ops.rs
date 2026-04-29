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
        AdminError::new(AdminErrorKind::BadRequest, "remote_relay_url is not a valid URL")
    })?;
    if parsed.scheme() != "https" {
        return Err(AdminError::new(
            AdminErrorKind::BadRequest,
            "remote_relay_url must use https scheme",
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| AdminError::new(AdminErrorKind::BadRequest, "remote_relay_url missing host"))?;
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
        relay_url: qm.relay_url().map(|url| url.to_string()).unwrap_or_default(),
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
        return Err(AdminError::new(AdminErrorKind::BadRequest, "invalid node_id"));
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
        let data = rkyv::to_bytes::<rkyv::rancor::Error>(&payload)
            .map(|v| v.to_vec())
            .map_err(|e| {
                error!(target_node = %remote_hints.node_id, "rkyv encode PairingRequest failed: {}", e);
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
        info!(target_node = %remote_hints.node_id, "Parowanie: wysylam PairingRequest przez ALPN_PAIRING");
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
                if let Err(e) = qm.connect_to_peer_with_hints(&remote_hints).await {
                    warn!(
                        target_node = %remote_hints.node_id,
                        "Pairing confirmed, ale mesh connect nieudany: {}",
                        e
                    );
                } else {
                    let local_info = node_info_collector::collect_node_info(local_node_id);
                    if let Ok(info_bytes) = rkyv::to_bytes::<rkyv::rancor::Error>(&local_info) {
                        if let Err(e) = qm.send_node_info(&remote_hints.node_id, &info_bytes).await
                        {
                            warn!(
                                target_node = %remote_hints.node_id,
                                "Pairing confirmed, ale NodeInfo send nieudany: {}",
                                e
                            );
                        }
                    }

                    let all_keys = security.get_all_trusted_keys();
                    if !all_keys.is_empty() {
                        let entries: Vec<tentaflow_protocol::mesh::TrustedKeyEntry> = all_keys
                            .iter()
                            .map(|(nid, pk)| tentaflow_protocol::mesh::TrustedKeyEntry {
                                node_id: nid.clone(),
                                public_key_hex: pk.clone(),
                            })
                            .collect();
                        let payload =
                            tentaflow_protocol::mesh::TrustedKeysSyncPayload { keys: entries };
                        if let Ok(sync_data) =
                            rkyv::to_bytes::<rkyv::rancor::Error>(&payload).map(|v| v.to_vec())
                        {
                            if let Err(e) = qm
                                .send_trusted_keys_sync(&remote_hints.node_id, &sync_data)
                                .await
                            {
                                warn!(
                                    target_node = %remote_hints.node_id,
                                    "Pairing confirmed, ale TrustedKeysSync send nieudany: {}",
                                    e
                                );
                            }
                        }
                    }
                }
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
pub fn confirm_pairing(
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
        return Err(AdminError::new(AdminErrorKind::BadRequest, "invalid node_id"));
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

    let hostname = peer_store
        .get_hostname(remote_node_id)
        .unwrap_or_default();

    security
        .confirm_pairing(remote_node_id, &remote_public_key, &hostname, "admin")
        .map_err(|e| {
            error!(target_node = %remote_node_id, "security.confirm_pairing failed: {}", e);
            AdminError::new(AdminErrorKind::BadRequest, "failed to confirm pairing")
        })?;

    // Mirror the freshly-trusted pubkey into the peer registry so the
    // persistence writer can produce a peer_persisted row for this peer.
    // remote_public_key is hex (Ed25519 32B = 64 chars, or Ed25519+X25519
    // 64B = 128 chars). The registry stores raw bytes.
    if let (Some(reg), Ok(pubkey_bytes)) = (
        peer_store.registry(),
        hex::decode(remote_public_key.as_str()),
    ) {
        let mut id_bytes = [0u8; 32];
        if hex::decode_to_slice(remote_node_id, &mut id_bytes).is_ok() {
            reg.set_pubkey(&id_bytes, std::sync::Arc::<[u8]>::from(pubkey_bytes.as_slice()));
            reg.set_trust(&id_bytes, crate::mesh::peer_registry::TrustState::Trusted);
        }
    }

    let pending_hints = load_pending_contact_hints(&security.db, remote_node_id)
        .ok()
        .flatten();
    if let Some(ref hints) = pending_hints {
        let _ = store_trusted_contact_hints(&security.db, remote_node_id, hints);
    }

    if let Some(ref qm) = quic_mesh {
        // from_node_id MUST be Ed25519 pubkey hex — initiator identifies peer
        // by iroh endpoint_id (= Ed25519 pubkey hex).
        let payload = tentaflow_protocol::mesh::MeshPairingConfirmPayload {
            from_node_id: security.ed25519_public_key_hex(),
            public_key: security.public_key_hex(),
            hostname: hostname.clone(),
            pin: provided.to_string(),
        };
        let data = match rkyv::to_bytes::<rkyv::rancor::Error>(&payload).map(|v| v.to_vec()) {
            Ok(d) => d,
            Err(e) => {
                error!(target_node = %remote_node_id, "rkyv encode PairingConfirm failed: {}", e);
                return Err(AdminError::new(
                    AdminErrorKind::Internal,
                    "internal mesh error",
                ));
            }
        };
        let qm = qm.clone();
        let sec_clone = security.clone();
        let node_id = remote_node_id.to_string();
        let local_nid = local_node_id.to_string();
        let pending_hints = pending_hints.clone();
        tokio::spawn(async move {
            if let Some(hints) = pending_hints {
                if let Err(e) = qm.connect_to_peer_with_hints(&hints).await {
                    warn!("Blad laczenia do peera z pending hints {}: {}", node_id, e);
                }
            }
            if let Err(e) = qm.send_pairing_confirm(&node_id, &data).await {
                warn!("Blad wysylania PairingConfirm przez QUIC: {}", e);
            }

            // QUIC nie gwarantuje kolejnosci miedzy streamami — pozwol PairingConfirm dotrzec.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;

            let local_info = node_info_collector::collect_node_info(&local_nid);
            if let Ok(info_bytes) = rkyv::to_bytes::<rkyv::rancor::Error>(&local_info) {
                if let Err(e) = qm.send_node_info(&node_id, &info_bytes).await {
                    warn!(
                        "Blad wysylania NodeInfo po sparowaniu do {}: {}",
                        node_id, e
                    );
                }
            }

            let all_keys = sec_clone.get_all_trusted_keys();
            if !all_keys.is_empty() {
                let entries: Vec<tentaflow_protocol::mesh::TrustedKeyEntry> = all_keys
                    .iter()
                    .map(|(nid, pk)| tentaflow_protocol::mesh::TrustedKeyEntry {
                        node_id: nid.clone(),
                        public_key_hex: pk.clone(),
                    })
                    .collect();
                let payload =
                    tentaflow_protocol::mesh::TrustedKeysSyncPayload { keys: entries };
                if let Ok(sync_data) =
                    rkyv::to_bytes::<rkyv::rancor::Error>(&payload).map(|v| v.to_vec())
                {
                    if let Err(e) = qm.send_trusted_keys_sync(&node_id, &sync_data).await {
                        warn!("Blad wysylania TrustedKeysSync do {}: {}", node_id, e);
                    }
                    qm.broadcast_to_trusted(
                        tentaflow_protocol::mesh::MESH_MSG_TRUSTED_KEYS_SYNC,
                        &sync_data,
                        Some(&node_id),
                    )
                    .await;
                }
            }
        });
    }

    let _ = db::repository::delete_setting(
        &security.db,
        &format!("pending_pubkey:{}", remote_node_id),
    );
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
        return Err(AdminError::new(AdminErrorKind::BadRequest, "invalid node_id"));
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
        let data = rkyv::to_bytes::<rkyv::rancor::Error>(&payload)
            .map(|v| v.to_vec())
            .map_err(|e| {
                error!(target_node = %remote_node_id, "rkyv encode PairingReject failed: {}", e);
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
        return Err(AdminError::new(AdminErrorKind::BadRequest, "Niepoprawny node_id"));
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
        let data = rkyv::to_bytes::<rkyv::rancor::Error>(&payload)
            .map(|v| v.to_vec())
            .unwrap_or_default();
        let revoked_id = node_id.to_string();
        security.mark_revoking(node_id);
        tokio::spawn(async move {
            // Wyslij PRZED revoke — klucze szyfrowania jeszcze istnieja.
            if let Err(e) = qm
                .send_to_peer(
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
            qm.broadcast_to_trusted(
                tentaflow_protocol::mesh::MESH_MSG_TRUST_REVOKED,
                &data,
                Some(&revoked_id),
            )
            .await;

            if let Err(e) = sec.unpair(&revoked_id) {
                warn!("Blad unpair dla {}: {}", revoked_id, e);
            }
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
        security.clear_revoking(node_id);
    }
    let _ = delete_trusted_contact_hints(&security.db, node_id);

    Ok(())
}

/// Przywraca zaufanie po revocation (admin override).
pub fn retrust(security: &Arc<MeshSecurity>, node_id: &str) -> Result<(), AdminError> {
    if !is_valid_id(node_id) {
        return Err(AdminError::new(AdminErrorKind::BadRequest, "Niepoprawny node_id"));
    }

    security.admin_retrust(node_id).map_err(|e| {
        error!(target_node = %node_id, "admin_retrust failed: {}", e);
        AdminError::new(AdminErrorKind::Internal, "internal mesh error")
    })?;
    Ok(())
}
