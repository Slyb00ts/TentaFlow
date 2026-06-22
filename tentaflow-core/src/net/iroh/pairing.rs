// =============================================================================
// Plik: net/iroh/pairing.rs
// Opis: Handler iroh protokolu parowania (ALPN `tentaflow-pairing/v2`).
//       Przyjmuje polaczenia inicjatora, zapisuje oczekujace parowanie wraz
//       z hintami transportowymi i potrafi auto-potwierdzic flow QR invite.
//       Request/response sa len-prefixed CBOR; mesh stream sluzy dalej do
//       heartbeatow i synchronizacji juz po zestawieniu zaufania.
// =============================================================================

use std::io::Cursor;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use iroh::endpoint::Connection;
use iroh::protocol::ProtocolHandler;
use iroh::{EndpointAddr, RelayUrl};
use tentaflow_protocol::mesh::{
    PairingFirstContactRequest, PairingFirstContactResponse, PairingTrustedKeyEntry,
};
use tracing::{info, warn};

use crate::db;
use crate::mesh::security::MeshSecurity;

const MAX_FRAME_BYTES: usize = 64 * 1024;
const PENDING_CONTACT_PREFIX: &str = "pending_contact:";
const TRUSTED_CONTACT_PREFIX: &str = "trusted_contact:";

/// Hinty transportowe potrzebne do first-contact pairingu oraz do pozniejszego
/// `confirm/reject`, gdy drugi nod nie jest jeszcze obecny w peer_store.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct PairingContactHints {
    pub node_id: String,
    pub public_key_hex: String,
    pub hostname: String,
    pub addresses: Vec<String>,
    pub relay_url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingAttemptOutcome {
    Pending,
    Confirmed,
}

/// Obsluga przychodzacego parowania nad iroh stream.
#[derive(Clone)]
pub struct PairingHandler {
    security: Arc<MeshSecurity>,
    local_hostname: String,
}

impl std::fmt::Debug for PairingHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PairingHandler")
            .field("local_hostname", &self.local_hostname)
            .finish_non_exhaustive()
    }
}

impl PairingHandler {
    pub fn new(security: Arc<MeshSecurity>, local_hostname: impl Into<String>) -> Self {
        Self {
            security,
            local_hostname: local_hostname.into(),
        }
    }

    /// Weryfikacja requestu i zbudowanie odpowiedzi. Request zawsze zapisuje
    /// pending pairing lokalnie; auto-confirm odpala tylko dla aktywnego QR invite.
    pub fn verify_request(
        &self,
        req: &PairingFirstContactRequest,
        transport_node_id: &str,
    ) -> (PairingFirstContactResponse, Option<PairingContactHints>) {
        if !self.security.check_pin_rate_limit(&req.sender_node_id) {
            return (
                PairingFirstContactResponse::Reject {
                    reason: "przekroczony limit prob PIN".into(),
                },
                None,
            );
        }

        if let Err(reason) = validate_pairing_identity(
            &req.sender_node_id,
            &req.sender_public_key_hex,
            transport_node_id,
        ) {
            return (PairingFirstContactResponse::Reject { reason }, None);
        }

        if req.pin.len() != 6 || !req.pin.chars().all(|c| c.is_ascii_digit()) {
            return (
                PairingFirstContactResponse::Reject {
                    reason: "PIN musi miec 6 cyfr".into(),
                },
                None,
            );
        }

        if let Err(e) = self.security.receive_pairing_request(
            &req.sender_node_id,
            &req.pin,
            &req.sender_public_key_hex,
        ) {
            return (
                PairingFirstContactResponse::Reject {
                    reason: format!("zapis pending pairing nieudany: {e}"),
                },
                None,
            );
        }

        let hints = PairingContactHints {
            node_id: req.sender_node_id.clone(),
            public_key_hex: req.sender_public_key_hex.clone(),
            hostname: req.sender_hostname.clone(),
            addresses: req.sender_addresses.clone(),
            relay_url: req.sender_relay_url.clone(),
        };
        if let Err(e) = store_pending_contact_hints(&self.security.db, &req.sender_node_id, &hints)
        {
            warn!(peer = %req.sender_node_id, "pairing: zapis pending contact hints nieudany: {}", e);
        }

        if self.security.consume_invite_pin(&req.pin) {
            if let Err(e) = self.security.confirm_pairing(
                &req.sender_node_id,
                &req.sender_public_key_hex,
                &req.sender_hostname,
                "iroh-pairing",
            ) {
                return (
                    PairingFirstContactResponse::Reject {
                        reason: format!("zapis trusted_node nieudany: {e}"),
                    },
                    None,
                );
            }
            if let Err(e) =
                store_trusted_contact_hints(&self.security.db, &req.sender_node_id, &hints)
            {
                warn!(
                    peer = %req.sender_node_id,
                    "pairing: zapis trusted contact hints nieudany: {}",
                    e
                );
            }
            let _ = delete_pending_contact_hints(&self.security.db, &req.sender_node_id);
            info!(
                peer = %req.sender_node_id,
                hostname = %req.sender_hostname,
                "Parowanie zaakceptowane nad iroh transportem"
            );
            (
                PairingFirstContactResponse::Confirm {
                    receiver_public_key_hex: self.security.public_key_hex(),
                    receiver_hostname: self.local_hostname.clone(),
                    trusted_keys: self
                        .security
                        .get_all_trusted_keys()
                        .into_iter()
                        .map(|(node_id, public_key_hex, approved_at)| PairingTrustedKeyEntry {
                            node_id,
                            public_key_hex,
                            approved_at,
                        })
                        .collect(),
                },
                Some(hints),
            )
        } else {
            info!(
                peer = %req.sender_node_id,
                hostname = %req.sender_hostname,
                "Parowanie zapisane jako pending nad iroh transportem"
            );
            (
                PairingFirstContactResponse::Pending {
                    receiver_hostname: self.local_hostname.clone(),
                },
                None,
            )
        }
    }

    async fn handle_stream(
        &self,
        connection_remote_id: iroh::EndpointId,
        mut send: iroh::endpoint::SendStream,
        mut recv: iroh::endpoint::RecvStream,
    ) -> anyhow::Result<Option<PairingContactHints>> {
        let transport_node_id = hex::encode(connection_remote_id.as_bytes());
        let request: PairingFirstContactRequest = read_cbor_frame(&mut recv, "request").await?;

        let (response, trusted_hints) = self.verify_request(&request, &transport_node_id);
        write_cbor_frame(&mut send, &response, "response").await?;
        send.finish()
            .map_err(|e| anyhow::anyhow!("pairing: finish send stream: {e}"))?;

        // `accept()` upuszcza Connection po powrocie; czekamy na ACK, zeby peer
        // nie zgubil koncowki odpowiedzi przy szybkim zamknieciu streamu.
        let _ = tokio::time::timeout(Duration::from_secs(5), send.stopped()).await;

        Ok(trusted_hints)
    }

    pub async fn accept_with_outcome(
        &self,
        connection: Connection,
    ) -> Result<Option<PairingContactHints>, iroh::protocol::AcceptError> {
        let (send, recv) = match connection.accept_bi().await {
            Ok(v) => v,
            Err(e) => {
                warn!("pairing: accept_bi nieudane: {}", e);
                return Err(iroh::protocol::AcceptError::from_err(e));
            }
        };

        let remote_id = connection.remote_id();
        match self.handle_stream(remote_id, send, recv).await {
            Ok(outcome) => Ok(outcome),
            Err(e) => {
                warn!("pairing: obsluga streamu nieudana: {}", e);
                Ok(None)
            }
        }
    }
}

impl ProtocolHandler for PairingHandler {
    async fn accept(&self, connection: Connection) -> Result<(), iroh::protocol::AcceptError> {
        let _ = self.accept_with_outcome(connection).await?;
        Ok(())
    }
}

async fn read_cbor_frame<T>(recv: &mut iroh::endpoint::RecvStream, label: &str) -> anyhow::Result<T>
where
    T: serde::de::DeserializeOwned,
{
    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| anyhow::anyhow!("pairing: read {label} len: {e}"))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        anyhow::bail!("pairing {label} frame too large: {len} bytes");
    }

    let mut body = vec![0u8; len];
    recv.read_exact(&mut body)
        .await
        .map_err(|e| anyhow::anyhow!("pairing: read {label} body: {e}"))?;
    ciborium::de::from_reader(Cursor::new(body))
        .map_err(|e| anyhow::anyhow!("pairing: CBOR decode {label}: {e}"))
}

async fn write_cbor_frame<T>(
    send: &mut iroh::endpoint::SendStream,
    value: &T,
    label: &str,
) -> anyhow::Result<()>
where
    T: serde::Serialize,
{
    let mut body = Vec::new();
    ciborium::ser::into_writer(value, &mut body)
        .map_err(|e| anyhow::anyhow!("pairing: CBOR encode {label}: {e}"))?;
    if body.len() > MAX_FRAME_BYTES {
        anyhow::bail!("pairing {label} frame too large: {} bytes", body.len());
    }
    send.write_all(&(body.len() as u32).to_be_bytes())
        .await
        .map_err(|e| anyhow::anyhow!("pairing: write {label} len: {e}"))?;
    send.write_all(&body)
        .await
        .map_err(|e| anyhow::anyhow!("pairing: write {label} body: {e}"))?;
    Ok(())
}

fn validate_pairing_identity(
    node_id: &str,
    public_key_hex: &str,
    transport_node_id: &str,
) -> Result<(), String> {
    if node_id != transport_node_id {
        return Err("sender_node_id nie zgadza sie z iroh remote_id".into());
    }
    validate_public_key_shape(node_id, public_key_hex)
}

fn validate_public_key_shape(node_id: &str, public_key_hex: &str) -> Result<(), String> {
    if node_id.len() != 64 || !node_id.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("node_id musi miec 64 hex znaki".into());
    }
    if public_key_hex.len() != 128 || !public_key_hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("klucz publiczny musi miec 128 hex znakow".into());
    }
    if !public_key_hex.starts_with(node_id) {
        return Err("Ed25519 czesc klucza publicznego nie zgadza sie z node_id".into());
    }
    Ok(())
}

/// Klient uruchamiany przez inicjatora (node B): laczy sie do node A przez
/// `endpoint.connect(receiver_id, ALPN_PAIRING)`, buduje `PairingFirstContactRequest`,
/// wysyla, odczytuje odpowiedz. Po `Confirm` zapisuje A jako trusted + sync
/// trusted_keys z odpowiedzi.
pub async fn initiate_pairing_over_iroh(
    endpoint: &iroh::Endpoint,
    receiver: &PairingContactHints,
    security: &MeshSecurity,
    pin: &str,
    local_hostname: &str,
    local_addresses: Vec<String>,
    local_relay_url: String,
) -> anyhow::Result<PairingAttemptOutcome> {
    let sender_node_id = security.ed25519_public_key_hex();
    // Czekamy az nasz endpoint dopnie do home relay i zdazy opublikowac sie
    // w pkarr DNS. Bez tego pierwsze pairing-y po starcie czesto leca na
    // stare/brakujace rekordy i padaja z 'connect: timed out' 25s.
    // Timeout 8s — relay connect zwykle <1s, publikacja <5s; gdy sie nie
    // uda w 8s i tak probujemy dalej (moze byc tryb offline-LAN).
    if let Err(_) = tokio::time::timeout(std::time::Duration::from_secs(8), endpoint.online()).await
    {
        tracing::debug!("pairing: online() timeout 8s — proba bez pelnej rejestracji relay");
    }

    // Zawsze pairing relay-first: jesli hints nie niosa relay_url, uzupelniamy
    // go naszym home relay. Direct adresy (gdy sa) zostaja — iroh probuje ich
    // rownolegle i hole-punchuje LAN-side po otwartej sesji relay.
    let receiver_hints = hints_with_relay_fallback(endpoint, receiver, Some(&local_relay_url));
    let endpoint_addr = endpoint_addr_from_hints(&receiver_hints)?;

    let request = PairingFirstContactRequest {
        sender_node_id: sender_node_id.clone(),
        sender_public_key_hex: security.public_key_hex(),
        sender_hostname: local_hostname.to_string(),
        pin: pin.to_string(),
        sender_addresses: local_addresses,
        sender_relay_url: local_relay_url,
    };

    // Retry na timeout — pierwszy strzal moze trafic na stary pkarr rekord
    // (race po restarcie peera). Drugi strzal po 2s pauzie zwykle uderza w
    // juz wypublikowane swieze adresy.
    let connection =
        connect_pairing_with_retry(endpoint, endpoint_addr.clone(), &receiver_hints.node_id)
            .await?;
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|e| anyhow::anyhow!("pairing: open_bi: {e}"))?;

    write_cbor_frame(&mut send, &request, "request").await?;
    send.finish()
        .map_err(|e| anyhow::anyhow!("pairing: finish: {e}"))?;

    let response: PairingFirstContactResponse = read_cbor_frame(&mut recv, "response").await?;

    match response {
        PairingFirstContactResponse::Confirm {
            receiver_public_key_hex,
            receiver_hostname,
            trusted_keys,
        } => {
            validate_pairing_identity(
                &receiver.node_id,
                &receiver_public_key_hex,
                &receiver.node_id,
            )
            .map_err(|e| anyhow::anyhow!("pairing response identity: {e}"))?;
            security
                .confirm_pairing(
                    &receiver.node_id,
                    &receiver_public_key_hex,
                    &receiver_hostname,
                    "iroh-pairing",
                )
                .map_err(|e| anyhow::anyhow!("confirm_pairing receiver: {e}"))?;
            for entry in trusted_keys {
                if validate_public_key_shape(&entry.node_id, &entry.public_key_hex).is_ok() {
                    let _ = security.add_trusted_key(
                        &entry.node_id,
                        &entry.public_key_hex,
                        "mesh-sync",
                        Some(&entry.approved_at),
                    );
                }
            }
            Ok(PairingAttemptOutcome::Confirmed)
        }
        PairingFirstContactResponse::Pending { .. } => {
            info!(peer = %receiver.node_id, "PairingRequest dostarczony — oczekuje na potwierdzenie");
            Ok(PairingAttemptOutcome::Pending)
        }
        PairingFirstContactResponse::Reject { reason } => {
            anyhow::bail!("pairing rejected: {reason}")
        }
    }
}

pub fn load_pending_contact_hints(
    db: &crate::db::DbPool,
    remote_node_id: &str,
) -> anyhow::Result<Option<PairingContactHints>> {
    load_contact_hints_from_peer_db(db, remote_node_id, db::repository::TRUST_PENDING_PAIRING)
}

pub fn store_pending_contact_hints(
    db: &crate::db::DbPool,
    remote_node_id: &str,
    hints: &PairingContactHints,
) -> anyhow::Result<()> {
    store_contact_hints_to_peer_db(
        db,
        remote_node_id,
        hints,
        db::repository::TRUST_PENDING_PAIRING,
    )
}

pub fn delete_pending_contact_hints(
    db: &crate::db::DbPool,
    remote_node_id: &str,
) -> anyhow::Result<()> {
    db::repository::delete_setting(db, &pending_contact_setting_key(remote_node_id))?;
    delete_peer_if_state(db, remote_node_id, db::repository::TRUST_PENDING_PAIRING)?;
    Ok(())
}

pub fn load_trusted_contact_hints(
    db: &crate::db::DbPool,
    remote_node_id: &str,
) -> anyhow::Result<Option<PairingContactHints>> {
    load_contact_hints_from_peer_db(db, remote_node_id, db::repository::TRUST_TRUSTED)
}

pub fn store_trusted_contact_hints(
    db: &crate::db::DbPool,
    remote_node_id: &str,
    hints: &PairingContactHints,
) -> anyhow::Result<()> {
    store_contact_hints_to_peer_db(db, remote_node_id, hints, db::repository::TRUST_TRUSTED)
}

pub fn delete_trusted_contact_hints(
    db: &crate::db::DbPool,
    remote_node_id: &str,
) -> anyhow::Result<()> {
    db::repository::delete_setting(db, &trusted_contact_setting_key(remote_node_id))?;
    db::repository::delete_peer_persisted(db, &node_id_hex_to_bytes(remote_node_id)?)?;
    Ok(())
}

fn load_contact_hints_from_peer_db(
    db: &crate::db::DbPool,
    remote_node_id: &str,
    trust_state: i64,
) -> anyhow::Result<Option<PairingContactHints>> {
    let Ok(node_id) = node_id_hex_to_bytes(remote_node_id) else {
        return Ok(None);
    };
    let rows = db::repository::load_peer_persisted_all(db)?;
    let Some(row) = rows
        .into_iter()
        .find(|row| row.node_id == node_id && row.trust_state == trust_state)
    else {
        return Ok(None);
    };
    let hints_by_node = db::repository::load_peer_hints_all(db)?;
    let mut out = PairingContactHints {
        node_id: remote_node_id.to_string(),
        public_key_hex: hex::encode(row.pubkey),
        hostname: row.hostname.unwrap_or_default(),
        addresses: Vec::new(),
        relay_url: String::new(),
    };
    if let Some(rows) = hints_by_node.get(&node_id) {
        for hint in rows {
            if hint.hint_kind == db::repository::HINT_KIND_DIRECT_ADDR {
                out.addresses.push(hint.payload.clone());
            } else if hint.hint_kind == db::repository::HINT_KIND_RELAY_URL {
                out.relay_url = hint.payload.clone();
            } else if hint.hint_kind == db::repository::HINT_KIND_HOSTNAME
                && out.hostname.is_empty()
            {
                out.hostname = hint.payload.clone();
            }
        }
    }
    Ok(Some(out))
}

fn store_contact_hints_to_peer_db(
    db: &crate::db::DbPool,
    remote_node_id: &str,
    hints: &PairingContactHints,
    trust_state: i64,
) -> anyhow::Result<()> {
    let node_id = node_id_hex_to_bytes(remote_node_id)?;
    let pubkey = contact_pubkey_bytes(db, remote_node_id, hints)?;
    let now_ms = unix_ms();
    let persisted_ver = next_persisted_ver(db, &node_id, now_ms)?;
    let row = db::repository::PeerPersistedRow {
        node_id,
        pubkey,
        trust_state,
        hostname: if hints.hostname.is_empty() {
            None
        } else {
            Some(hints.hostname.clone())
        },
        platform: None,
        role: db::repository::ROLE_NODE,
        last_seen_ms: 0,
        persisted_ver,
        updated_at_ms: now_ms,
    };
    let mut hint_rows = Vec::new();
    for address in &hints.addresses {
        if !address.trim().is_empty() {
            hint_rows.push(db::repository::PeerHintRow {
                node_id,
                hint_kind: db::repository::HINT_KIND_DIRECT_ADDR,
                payload: address.clone(),
                last_ok_ms: None,
                fail_count: 0,
            });
        }
    }
    if !hints.relay_url.trim().is_empty() {
        hint_rows.push(db::repository::PeerHintRow {
            node_id,
            hint_kind: db::repository::HINT_KIND_RELAY_URL,
            payload: hints.relay_url.clone(),
            last_ok_ms: None,
            fail_count: 0,
        });
    }
    if !hints.hostname.trim().is_empty() {
        hint_rows.push(db::repository::PeerHintRow {
            node_id,
            hint_kind: db::repository::HINT_KIND_HOSTNAME,
            payload: hints.hostname.clone(),
            last_ok_ms: None,
            fail_count: 0,
        });
    }
    db::repository::upsert_peer_persisted_batch(db, &[row])?;
    db::repository::replace_peer_hints(db, &node_id, &hint_rows)?;
    db::repository::delete_setting(db, &pending_contact_setting_key(remote_node_id))?;
    db::repository::delete_setting(db, &trusted_contact_setting_key(remote_node_id))?;
    Ok(())
}

fn delete_peer_if_state(
    db: &crate::db::DbPool,
    remote_node_id: &str,
    trust_state: i64,
) -> anyhow::Result<()> {
    let node_id = node_id_hex_to_bytes(remote_node_id)?;
    let rows = db::repository::load_peer_persisted_all(db)?;
    if rows
        .into_iter()
        .any(|row| row.node_id == node_id && row.trust_state == trust_state)
    {
        db::repository::delete_peer_persisted(db, &node_id)?;
    }
    Ok(())
}

fn next_persisted_ver(
    db: &crate::db::DbPool,
    node_id: &[u8; 32],
    now_ms: i64,
) -> anyhow::Result<i64> {
    let current = db::repository::load_peer_persisted_all(db)?
        .into_iter()
        .find(|row| &row.node_id == node_id)
        .map(|row| row.persisted_ver)
        .unwrap_or(0);
    Ok(now_ms.max(current.saturating_add(1)))
}

fn contact_pubkey_bytes(
    db: &crate::db::DbPool,
    remote_node_id: &str,
    hints: &PairingContactHints,
) -> anyhow::Result<Vec<u8>> {
    if !hints.public_key_hex.is_empty() {
        return hex::decode(&hints.public_key_hex)
            .map_err(|e| anyhow::anyhow!("contact public_key_hex decode: {e}"));
    }
    if let Some(public_key) = db::repository::get_trusted_node_public_key(db, remote_node_id)? {
        return hex::decode(public_key)
            .map_err(|e| anyhow::anyhow!("trusted public_key decode: {e}"));
    }
    Ok(node_id_hex_to_bytes(remote_node_id)?.to_vec())
}

fn node_id_hex_to_bytes(remote_node_id: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = hex::decode(remote_node_id).map_err(|e| anyhow::anyhow!("node_id hex: {e}"))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("node_id musi miec 32 bajty"))?;
    Ok(arr)
}

fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn merge_contact_hints(
    current: Option<PairingContactHints>,
    fresh: PairingContactHints,
) -> PairingContactHints {
    let mut merged = fresh.clone();
    if let Some(current) = current {
        if merged.node_id.is_empty() {
            merged.node_id = current.node_id;
        }
        if merged.public_key_hex.is_empty() {
            merged.public_key_hex = current.public_key_hex;
        }
        if merged.hostname.is_empty() {
            merged.hostname = current.hostname;
        }
        if merged.relay_url.is_empty() {
            merged.relay_url = current.relay_url;
        }
        for addr in current.addresses {
            if !addr.is_empty() && !merged.addresses.contains(&addr) {
                merged.addresses.push(addr);
            }
        }
    }
    merged
}

/// Wzorce hostow uznanych za martwe relay URL — usuwane przy starcie mesh.
/// Do commitu e9552dc zapisywalismy domyslne `https://use.iroh.network/`, ktore
/// od dawna nie resolwuje DNS. Po naprawie Fazy 1 nowe eventy zapisuja pusty
/// string, ale w bazie starych instalacji moze lezec martwy URL — sanitizer
/// musi go wyczyscic zanim `connect_to_peer_with_hints` wejdzie w dial fail.
const DEAD_RELAY_PATTERNS: &[&str] = &["use.iroh.network"];

/// Czyszczenie zapisanych hintow zaufanych peerow:
///  1. Czysci `relay_url` gdy matchuje wzorzec martwego hosta (`DEAD_RELAY_PATTERNS`).
///  2. Przepuszcza liste `addresses` przez biezace `AdvertiseFilters` z settings.
///  3. IPv6 jest usuwany bezwarunkowo (mesh IPv4-only).
///
/// Zwraca liczbe faktycznie zaktualizowanych wpisow.
pub fn sanitize_trusted_contacts(db: &crate::db::DbPool) -> anyhow::Result<usize> {
    let rows = db::repository::load_peer_persisted_all(db)?;
    let mut cleaned = 0usize;

    let filters = crate::mesh::network_interfaces::load_advertise_filters(db);
    let kind_map = crate::mesh::network_interfaces::ipv4_kind_map();
    let name_map = crate::mesh::network_interfaces::ipv4_name_map();

    for row in rows {
        if row.trust_state != db::repository::TRUST_TRUSTED {
            continue;
        }
        let node_id = hex::encode(row.node_id);
        let Some(mut hints) = load_trusted_contact_hints(db, &node_id)? else {
            continue;
        };

        let original_addresses = hints.addresses.clone();
        let original_relay = hints.relay_url.clone();

        let has_dead_relay = DEAD_RELAY_PATTERNS
            .iter()
            .any(|p| hints.relay_url.contains(p));
        if has_dead_relay {
            hints.relay_url.clear();
        }

        // Filter addresses. Format "ip:port" — wyluskujemy IPv4, odrzucamy IPv6
        // oraz adresy ktorych filtr advertise nie przepuszcza.
        hints.addresses = hints
            .addresses
            .iter()
            .filter(|raw| {
                let Some(ip_part) = raw.split(':').next() else {
                    return false;
                };
                match ip_part.parse::<std::net::Ipv4Addr>() {
                    Ok(v4) => crate::mesh::network_interfaces::should_advertise_ip(
                        v4, &filters, &kind_map, &name_map,
                    ),
                    Err(_) => false,
                }
            })
            .cloned()
            .collect();

        let changed = hints.addresses != original_addresses || hints.relay_url != original_relay;
        if !changed {
            continue;
        }

        match store_trusted_contact_hints(db, &node_id, &hints) {
            Ok(()) => {
                cleaned += 1;
                info!(
                    node_id = %node_id,
                    dropped_addrs = original_addresses.len() - hints.addresses.len(),
                    relay_cleared = (has_dead_relay),
                    "sanitize_trusted_contacts: zaktualizowano wpis"
                );
            }
            Err(e) => warn!(
                node_id = %node_id,
                "sanitize_trusted_contacts: zapis nieudany: {}",
                e
            ),
        }
    }

    Ok(cleaned)
}

/// Gwarantuje ze `hints.relay_url` jest ustawiony — gdy peer go nie dostarczyl
/// (bare node_id, stary QR bez relay, autodiscovery LAN bez relay pola),
/// wpisujemy nasz wlasny home relay. Przy domyslnym secie obie strony sa w n0
/// relay mesh i peer jest osiagalny przez ten sam URL. Relay-first: adresy
/// bezposrednie zostaja (iroh tries them in parallel), ale relay zawsze jest
/// gotowy fallback, a po jego zestawieniu iroh sam hole-punchuje do direct
/// path gdy sasiedzi sa w LANie.
pub fn hints_with_relay_fallback(
    endpoint: &iroh::Endpoint,
    hints: &PairingContactHints,
    configured_relay: Option<&str>,
) -> PairingContactHints {
    if !hints.relay_url.trim().is_empty() {
        return hints.clone();
    }
    // endpoint.addr() lists the relay only once the home-relay session is up;
    // right after startup it is empty, so the configured relay from config/DB
    // is the backstop — without it early dials create p2p-only connections
    // with no relay path to fail over to.
    let our_relay = endpoint
        .addr()
        .relay_urls()
        .next()
        .map(|u| u.to_string())
        .or_else(|| {
            configured_relay
                .map(|u| u.trim().to_string())
                .filter(|u| !u.is_empty())
        })
        .unwrap_or_default();
    if our_relay.is_empty() {
        return hints.clone();
    }
    let mut filled = hints.clone();
    filled.relay_url = our_relay;
    filled
}

/// Dial pairing ALPN z jedna ponowna proba przy timeout. Miedzy probami robimy
/// krotkie tchniecie zeby iroh mial szanse odswiezyc address-lookup (gdy peer
/// dopiero sie publikuje w pkarr DNS po restarcie). Zwraca Connection albo
/// ostateczny blad.
async fn connect_pairing_with_retry(
    endpoint: &iroh::Endpoint,
    addr: EndpointAddr,
    peer_id_hex: &str,
) -> anyhow::Result<iroh::endpoint::Connection> {
    match endpoint.connect(addr.clone(), super::ALPN_PAIRING).await {
        Ok(c) => return Ok(c),
        Err(e) => {
            let msg = format!("{e:?}");
            let is_timeout = msg.contains("timed out") || msg.contains("TimedOut");
            if !is_timeout {
                return Err(anyhow::anyhow!("pairing: connect: {e:?}"));
            }
            tracing::info!(
                peer = %peer_id_hex,
                "pairing: pierwszy dial timeout — retry za 2s (pkarr moze publikowac swieze adresy)"
            );
        }
    }

    // Pauza — daje iroh chwile na rezolucje swiezszych rekordow pkarr.
    // Drugi retry z dluzszym czasem (15s) — gdy pierwszy 2s zabraklo na
    // propagacje DHT po restarcie peera (pkarr TTL default 5-10s).
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    match endpoint.connect(addr.clone(), super::ALPN_PAIRING).await {
        Ok(c) => return Ok(c),
        Err(e) => {
            let msg = format!("{e:?}");
            let is_timeout = msg.contains("timed out") || msg.contains("TimedOut");
            if !is_timeout {
                return Err(anyhow::anyhow!("pairing: connect (retry): {e:?}"));
            }
            tracing::info!(
                peer = %peer_id_hex,
                "pairing: drugi dial timeout — ostatnia proba za 15s"
            );
        }
    }

    tokio::time::sleep(std::time::Duration::from_secs(15)).await;
    endpoint
        .connect(addr, super::ALPN_PAIRING)
        .await
        .map_err(|e| anyhow::anyhow!("pairing: connect (retry-2): {e:?}"))
}

pub fn endpoint_addr_from_hints(hints: &PairingContactHints) -> anyhow::Result<EndpointAddr> {
    let receiver_id = parse_endpoint_id(&hints.node_id)?;
    let mut addr = EndpointAddr::new(receiver_id);
    for socket_addr in parse_socket_addrs(&hints.addresses) {
        addr = addr.with_ip_addr(socket_addr);
    }
    if !hints.relay_url.trim().is_empty() {
        let relay_url: RelayUrl = hints
            .relay_url
            .trim()
            .parse()
            .map_err(|e| anyhow::anyhow!("pairing relay url: {e}"))?;
        addr = addr.with_relay_url(relay_url);
    }
    Ok(addr)
}

fn parse_socket_addrs(addrs: &[String]) -> Vec<SocketAddr> {
    addrs.iter().filter_map(|addr| addr.parse().ok()).collect()
}

fn pending_contact_setting_key(remote_node_id: &str) -> String {
    format!("{PENDING_CONTACT_PREFIX}{remote_node_id}")
}

fn trusted_contact_setting_key(remote_node_id: &str) -> String {
    format!("{TRUSTED_CONTACT_PREFIX}{remote_node_id}")
}

fn parse_endpoint_id(hex_str: &str) -> anyhow::Result<iroh::EndpointId> {
    let bytes = hex::decode(hex_str).map_err(|e| anyhow::anyhow!("hex decode node_id: {e}"))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("node_id musi byc 32 bajtami"))?;
    iroh::EndpointId::from_bytes(&arr).map_err(|e| anyhow::anyhow!("EndpointId: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_node_id() -> String {
        hex::encode(iroh::SecretKey::generate().public().as_bytes())
    }

    fn test_public_key(node_id: &str) -> String {
        format!("{}{}", node_id, "11".repeat(32))
    }

    #[test]
    fn endpoint_addr_zawiera_direct_i_relay() {
        let node_id = test_node_id();
        let hints = PairingContactHints {
            node_id,
            public_key_hex: String::new(),
            hostname: "peer".to_string(),
            addresses: vec!["10.0.0.7:8090".to_string(), "192.168.1.7:8090".to_string()],
            relay_url: "https://relay.example./".to_string(),
        };

        let addr = endpoint_addr_from_hints(&hints).expect("endpoint addr");
        let direct: Vec<_> = addr.ip_addrs().copied().collect();
        let relays: Vec<_> = addr.relay_urls().cloned().collect();

        assert_eq!(direct.len(), 2);
        assert_eq!(relays.len(), 1);
        assert_eq!(relays[0].to_string(), "https://relay.example./");
    }

    #[test]
    fn validate_pairing_identity_rejects_transport_spoof() {
        let node_id = test_node_id();
        let public_key = test_public_key(&node_id);
        let other_node_id = test_node_id();

        let err = validate_pairing_identity(&node_id, &public_key, &other_node_id)
            .expect_err("spoofowany transport musi byc odrzucony");
        assert_eq!(err, "sender_node_id nie zgadza sie z iroh remote_id");
    }

    #[test]
    fn validate_pairing_identity_rejects_key_mismatch() {
        let node_id = test_node_id();
        let other_node_id = test_node_id();
        let public_key = test_public_key(&other_node_id);

        let err = validate_pairing_identity(&node_id, &public_key, &node_id)
            .expect_err("klucz niepasujacy do node_id musi byc odrzucony");
        assert_eq!(
            err,
            "Ed25519 czesc klucza publicznego nie zgadza sie z node_id"
        );
    }

    #[test]
    fn contact_hints_are_stored_in_peer_tables_not_settings() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("init test DB");
        let node_id = test_node_id();
        let public_key = test_public_key(&node_id);
        let hints = PairingContactHints {
            node_id: node_id.clone(),
            public_key_hex: public_key.clone(),
            hostname: "peer-a".to_string(),
            addresses: vec!["127.0.0.1:8090".to_string()],
            relay_url: "https://relay.example.com/".to_string(),
        };

        store_pending_contact_hints(&db, &node_id, &hints).expect("store pending hints");

        assert!(
            db::repository::get_setting(&db, &pending_contact_setting_key(&node_id))
                .expect("read setting")
                .is_none(),
            "aktywny zapis nie moze uzywac settings JSON"
        );

        let loaded = load_pending_contact_hints(&db, &node_id)
            .expect("load pending")
            .expect("pending present");
        assert_eq!(loaded.public_key_hex, public_key);
        assert_eq!(loaded.hostname, "peer-a");
        assert_eq!(loaded.addresses, vec!["127.0.0.1:8090".to_string()]);
        assert_eq!(loaded.relay_url, "https://relay.example.com/");

        let rows = db::repository::load_peer_persisted_all(&db).expect("load peers");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].trust_state, db::repository::TRUST_PENDING_PAIRING);
    }

    #[test]
    fn pending_delete_does_not_remove_promoted_trusted_peer() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("init test DB");
        let node_id = test_node_id();
        let public_key = test_public_key(&node_id);
        let hints = PairingContactHints {
            node_id: node_id.clone(),
            public_key_hex: public_key,
            hostname: "peer-b".to_string(),
            addresses: vec!["127.0.0.1:8091".to_string()],
            relay_url: String::new(),
        };

        store_pending_contact_hints(&db, &node_id, &hints).expect("store pending");
        store_trusted_contact_hints(&db, &node_id, &hints).expect("promote trusted");
        delete_pending_contact_hints(&db, &node_id).expect("delete pending");

        let trusted = load_trusted_contact_hints(&db, &node_id)
            .expect("load trusted")
            .expect("trusted still present");
        assert_eq!(trusted.hostname, "peer-b");

        let rows = db::repository::load_peer_persisted_all(&db).expect("load peers");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].trust_state, db::repository::TRUST_TRUSTED);
    }

    #[test]
    fn merge_contact_hints_keeps_fresh_order_and_appends_missing_current() {
        let current = PairingContactHints {
            node_id: "peer".to_string(),
            public_key_hex: "abcd".to_string(),
            hostname: "old-host".to_string(),
            addresses: vec!["10.0.0.8:8090".to_string(), "192.168.0.8:8090".to_string()],
            relay_url: "https://relay.old/".to_string(),
        };
        let fresh = PairingContactHints {
            node_id: "peer".to_string(),
            public_key_hex: String::new(),
            hostname: "new-host".to_string(),
            addresses: vec![
                "192.168.0.8:8090".to_string(),
                "10.10.10.8:8090".to_string(),
            ],
            relay_url: String::new(),
        };

        let merged = merge_contact_hints(Some(current), fresh);

        assert_eq!(merged.public_key_hex, "abcd");
        assert_eq!(merged.hostname, "new-host");
        assert_eq!(merged.relay_url, "https://relay.old/");
        assert_eq!(
            merged.addresses,
            vec![
                "192.168.0.8:8090".to_string(),
                "10.10.10.8:8090".to_string(),
                "10.0.0.8:8090".to_string(),
            ]
        );
    }

    #[test]
    fn sanitize_trusted_contacts_clears_dead_relay() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("init test DB");
        let dead_id = test_node_id();
        let good_id = test_node_id();
        let empty_id = test_node_id();

        let dead = PairingContactHints {
            node_id: dead_id.clone(),
            public_key_hex: test_public_key(&dead_id),
            hostname: "host-dead".to_string(),
            addresses: vec!["10.0.0.1:8090".to_string()],
            relay_url: "https://use.iroh.network/".to_string(),
        };
        let good = PairingContactHints {
            node_id: good_id.clone(),
            public_key_hex: test_public_key(&good_id),
            hostname: "host-good".to_string(),
            addresses: vec![],
            relay_url: "https://my-relay.example.com/".to_string(),
        };
        let empty = PairingContactHints {
            node_id: empty_id.clone(),
            public_key_hex: test_public_key(&empty_id),
            hostname: "host-empty".to_string(),
            addresses: vec![],
            relay_url: String::new(),
        };

        store_trusted_contact_hints(&db, &dead_id, &dead).unwrap();
        store_trusted_contact_hints(&db, &good_id, &good).unwrap();
        store_trusted_contact_hints(&db, &empty_id, &empty).unwrap();

        let cleaned = sanitize_trusted_contacts(&db).expect("sanitize");
        assert_eq!(cleaned, 1, "tylko jeden wpis powinien byc czyszczony");

        let loaded_dead = load_trusted_contact_hints(&db, &dead_id)
            .expect("load dead")
            .expect("dead present");
        assert!(
            loaded_dead.relay_url.is_empty(),
            "dead URL powinien byc wyczyszczony"
        );
        // Pozostale pola nietkniete.
        assert_eq!(loaded_dead.hostname, "host-dead");
        assert_eq!(loaded_dead.addresses, vec!["10.0.0.1:8090".to_string()]);

        let loaded_good = load_trusted_contact_hints(&db, &good_id)
            .expect("load good")
            .expect("good present");
        assert_eq!(
            loaded_good.relay_url, "https://my-relay.example.com/",
            "dobry URL nietkniety"
        );

        let loaded_empty = load_trusted_contact_hints(&db, &empty_id)
            .expect("load empty")
            .expect("empty present");
        assert!(loaded_empty.relay_url.is_empty(), "pusty dalej pusty");

        // Idempotentnosc — drugi przebieg nie powinien nic zmieniac.
        let cleaned2 = sanitize_trusted_contacts(&db).expect("sanitize idempotent");
        assert_eq!(cleaned2, 0, "drugi przebieg nie powinien nic czyscic");
    }

    #[test]
    fn endpoint_addr_pomija_niepoprawne_adresy() {
        let node_id = test_node_id();
        let hints = PairingContactHints {
            node_id,
            public_key_hex: String::new(),
            hostname: String::new(),
            addresses: vec!["nie-adres".to_string(), "127.0.0.1:8090".to_string()],
            relay_url: String::new(),
        };

        let addr = endpoint_addr_from_hints(&hints).expect("endpoint addr");
        let direct: Vec<_> = addr.ip_addrs().copied().collect();
        assert_eq!(
            direct,
            vec!["127.0.0.1:8090".parse::<SocketAddr>().unwrap()]
        );
    }
}
