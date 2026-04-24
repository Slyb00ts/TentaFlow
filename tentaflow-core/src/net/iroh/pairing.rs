// =============================================================================
// Plik: net/iroh/pairing.rs
// Opis: Handler iroh protokolu parowania (ALPN `tentaflow-pairing/v1`).
//       Przyjmuje polaczenia inicjatora, zapisuje oczekujace parowanie wraz
//       z hintami transportowymi i potrafi auto-potwierdzic flow QR invite.
//       Request/response sa len-prefixed JSON; mesh stream sluzy dalej do
//       heartbeatow i synchronizacji juz po zestawieniu zaufania.
// =============================================================================

use std::net::SocketAddr;
use std::sync::Arc;

use iroh::endpoint::Connection;
use iroh::{EndpointAddr, RelayUrl};
use iroh::protocol::ProtocolHandler;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::db;
use crate::mesh::security::MeshSecurity;

const MAX_FRAME_BYTES: usize = 64 * 1024;
const PENDING_CONTACT_PREFIX: &str = "pending_contact:";
const TRUSTED_CONTACT_PREFIX: &str = "trusted_contact:";

/// Hinty transportowe potrzebne do first-contact pairingu oraz do pozniejszego
/// `confirm/reject`, gdy drugi nod nie jest jeszcze obecny w peer_store.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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

/// Zadanie parowania wyslane przez inicjatora — node B → node A.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingRequest {
    /// Hex-enkodowany EndpointId noda B (Ed25519 pub).
    pub sender_node_id: String,
    /// Kombinowany klucz publiczny noda B (128 hex — Ed25519 + X25519).
    pub sender_public_key_hex: String,
    /// Hostname noda B — do wyswietlenia w logu zaufanych.
    pub sender_hostname: String,
    /// PIN przekazany recznie albo z invite QR.
    pub pin: String,
    /// Znane adresy `ip:port` inicjatora — potrzebne do pozniejszego confirm.
    pub sender_addresses: Vec<String>,
    /// Relay URL inicjatora — pozwala dogadac confirm nawet bez autodiscovery.
    pub sender_relay_url: String,
}

/// Odpowiedz noda A potwierdzajaca lub odrzucajaca parowanie.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PairingResponse {
    Confirm {
        /// Klucz publiczny noda A do zapisu po stronie noda B (128 hex).
        receiver_public_key_hex: String,
        /// Hostname noda A.
        receiver_hostname: String,
        /// Lista (node_id, public_key_hex) juz zaufanych przez A.
        trusted_keys: Vec<(String, String)>,
    },
    Pending {
        receiver_hostname: String,
    },
    Reject {
        reason: String,
    },
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
    pub fn verify_request(&self, req: &PairingRequest) -> PairingResponse {
        if !self.security.check_pin_rate_limit(&req.sender_node_id) {
            return PairingResponse::Reject {
                reason: "przekroczony limit prob PIN".into(),
            };
        }

        if req.sender_public_key_hex.len() != 128 {
            return PairingResponse::Reject {
                reason: "klucz publiczny musi miec 128 hex znakow".into(),
            };
        }

        if req.pin.len() != 6 || !req.pin.chars().all(|c| c.is_ascii_digit()) {
            return PairingResponse::Reject {
                reason: "PIN musi miec 6 cyfr".into(),
            };
        }

        if let Err(e) = self.security.receive_pairing_request(
            &req.sender_node_id,
            &req.pin,
            &req.sender_public_key_hex,
        ) {
            return PairingResponse::Reject {
                reason: format!("zapis pending pairing nieudany: {e}"),
            };
        }

        let hints = PairingContactHints {
            node_id: req.sender_node_id.clone(),
            public_key_hex: req.sender_public_key_hex.clone(),
            hostname: req.sender_hostname.clone(),
            addresses: req.sender_addresses.clone(),
            relay_url: req.sender_relay_url.clone(),
        };
        if let Err(e) = store_pending_contact_hints(&self.security.db, &req.sender_node_id, &hints) {
            warn!(peer = %req.sender_node_id, "pairing: zapis pending contact hints nieudany: {}", e);
        }

        if self.security.consume_invite_pin(&req.pin) {
            if let Err(e) = self.security.confirm_pairing(
                &req.sender_node_id,
                &req.sender_public_key_hex,
                &req.sender_hostname,
                "iroh-pairing",
            ) {
                return PairingResponse::Reject {
                    reason: format!("zapis trusted_node nieudany: {e}"),
                };
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
            PairingResponse::Confirm {
                receiver_public_key_hex: self.security.public_key_hex(),
                receiver_hostname: self.local_hostname.clone(),
                trusted_keys: self.security.get_all_trusted_keys(),
            }
        } else {
            info!(
                peer = %req.sender_node_id,
                hostname = %req.sender_hostname,
                "Parowanie zapisane jako pending nad iroh transportem"
            );
            PairingResponse::Pending {
                receiver_hostname: self.local_hostname.clone(),
            }
        }
    }

    async fn handle_stream(
        &self,
        mut send: iroh::endpoint::SendStream,
        mut recv: iroh::endpoint::RecvStream,
    ) -> anyhow::Result<()> {
        // Format: [u32 BE len][JSON PairingRequest].
        let mut len_buf = [0u8; 4];
        recv.read_exact(&mut len_buf)
            .await
            .map_err(|e| anyhow::anyhow!("pairing: read len: {e}"))?;
        let len = u32::from_be_bytes(len_buf) as usize;
        if len > MAX_FRAME_BYTES {
            anyhow::bail!("pairing frame too large: {} bytes", len);
        }

        let mut body = vec![0u8; len];
        recv.read_exact(&mut body)
            .await
            .map_err(|e| anyhow::anyhow!("pairing: read body: {e}"))?;

        let request: PairingRequest = serde_json::from_slice(&body)
            .map_err(|e| anyhow::anyhow!("pairing: JSON decode: {e}"))?;

        let response = self.verify_request(&request);
        let response_bytes = serde_json::to_vec(&response)
            .map_err(|e| anyhow::anyhow!("pairing: JSON encode response: {e}"))?;

        send.write_all(&(response_bytes.len() as u32).to_be_bytes())
            .await
            .map_err(|e| anyhow::anyhow!("pairing: write len: {e}"))?;
        send.write_all(&response_bytes)
            .await
            .map_err(|e| anyhow::anyhow!("pairing: write body: {e}"))?;
        send.finish()
            .map_err(|e| anyhow::anyhow!("pairing: finish send stream: {e}"))?;

        Ok(())
    }
}

impl ProtocolHandler for PairingHandler {
    async fn accept(&self, connection: Connection) -> Result<(), iroh::protocol::AcceptError> {
        let (send, recv) = match connection.accept_bi().await {
            Ok(v) => v,
            Err(e) => {
                warn!("pairing: accept_bi nieudane: {}", e);
                return Err(iroh::protocol::AcceptError::from_err(e));
            }
        };

        if let Err(e) = self.handle_stream(send, recv).await {
            warn!("pairing: obsluga streamu nieudana: {}", e);
        }
        Ok(())
    }
}

/// Klient uruchamiany przez inicjatora (node B): laczy sie do node A przez
/// `endpoint.connect(receiver_id, ALPN_PAIRING)`, buduje `PairingRequest`,
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
    // Zawsze pairing relay-first: jesli hints nie niosa relay_url, uzupelniamy
    // go naszym home relay. Direct adresy (gdy sa) zostaja — iroh probuje ich
    // rownolegle i hole-punchuje LAN-side po otwartej sesji relay.
    let receiver_hints = hints_with_relay_fallback(endpoint, receiver);
    let endpoint_addr = endpoint_addr_from_hints(&receiver_hints)?;

    let request = PairingRequest {
        sender_node_id: sender_node_id.clone(),
        sender_public_key_hex: security.public_key_hex(),
        sender_hostname: local_hostname.to_string(),
        pin: pin.to_string(),
        sender_addresses: local_addresses,
        sender_relay_url: local_relay_url,
    };
    let body = serde_json::to_vec(&request)
        .map_err(|e| anyhow::anyhow!("pairing: encode request: {e}"))?;

    let connection = endpoint
        .connect(endpoint_addr, super::ALPN_PAIRING)
        .await
        .map_err(|e| anyhow::anyhow!("pairing: connect: {e}"))?;
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|e| anyhow::anyhow!("pairing: open_bi: {e}"))?;

    send.write_all(&(body.len() as u32).to_be_bytes())
        .await
        .map_err(|e| anyhow::anyhow!("pairing: write len: {e}"))?;
    send.write_all(&body)
        .await
        .map_err(|e| anyhow::anyhow!("pairing: write body: {e}"))?;
    send.finish()
        .map_err(|e| anyhow::anyhow!("pairing: finish: {e}"))?;

    let mut len_buf = [0u8; 4];
    recv.read_exact(&mut len_buf)
        .await
        .map_err(|e| anyhow::anyhow!("pairing: read response len: {e}"))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        anyhow::bail!("pairing response too large: {} bytes", len);
    }
    let mut resp_bytes = vec![0u8; len];
    recv.read_exact(&mut resp_bytes)
        .await
        .map_err(|e| anyhow::anyhow!("pairing: read response body: {e}"))?;

    let response: PairingResponse = serde_json::from_slice(&resp_bytes)
        .map_err(|e| anyhow::anyhow!("pairing: JSON decode response: {e}"))?;

    match response {
        PairingResponse::Confirm {
            receiver_public_key_hex,
            receiver_hostname,
            trusted_keys,
        } => {
            security
                .confirm_pairing(
                    &receiver.node_id,
                    &receiver_public_key_hex,
                    &receiver_hostname,
                    "iroh-pairing",
                )
                .map_err(|e| anyhow::anyhow!("confirm_pairing receiver: {e}"))?;
            for (nid, pk) in trusted_keys {
                let _ = security.add_trusted_key(&nid, &pk, "mesh-sync");
            }
            Ok(PairingAttemptOutcome::Confirmed)
        }
        PairingResponse::Pending { .. } => {
            info!(peer = %receiver.node_id, "PairingRequest dostarczony — oczekuje na potwierdzenie");
            Ok(PairingAttemptOutcome::Pending)
        }
        PairingResponse::Reject { reason } => {
            anyhow::bail!("pairing rejected: {reason}")
        }
    }
}

pub fn load_pending_contact_hints(
    db: &crate::db::DbPool,
    remote_node_id: &str,
) -> anyhow::Result<Option<PairingContactHints>> {
    let Some(raw) = db::repository::get_setting(db, &pending_contact_setting_key(remote_node_id))?
    else {
        return Ok(None);
    };
    let hints = serde_json::from_str::<PairingContactHints>(&raw)
        .map_err(|e| anyhow::anyhow!("pending contact decode: {e}"))?;
    Ok(Some(hints))
}

pub fn store_pending_contact_hints(
    db: &crate::db::DbPool,
    remote_node_id: &str,
    hints: &PairingContactHints,
) -> anyhow::Result<()> {
    let raw = serde_json::to_string(hints)
        .map_err(|e| anyhow::anyhow!("pending contact encode: {e}"))?;
    db::repository::set_setting(db, &pending_contact_setting_key(remote_node_id), &raw)?;
    Ok(())
}

pub fn delete_pending_contact_hints(
    db: &crate::db::DbPool,
    remote_node_id: &str,
) -> anyhow::Result<()> {
    db::repository::delete_setting(db, &pending_contact_setting_key(remote_node_id))?;
    Ok(())
}

pub fn load_trusted_contact_hints(
    db: &crate::db::DbPool,
    remote_node_id: &str,
) -> anyhow::Result<Option<PairingContactHints>> {
    let Some(raw) = db::repository::get_setting(db, &trusted_contact_setting_key(remote_node_id))?
    else {
        return Ok(None);
    };
    let hints = serde_json::from_str::<PairingContactHints>(&raw)
        .map_err(|e| anyhow::anyhow!("trusted contact decode: {e}"))?;
    Ok(Some(hints))
}

pub fn store_trusted_contact_hints(
    db: &crate::db::DbPool,
    remote_node_id: &str,
    hints: &PairingContactHints,
) -> anyhow::Result<()> {
    let raw = serde_json::to_string(hints)
        .map_err(|e| anyhow::anyhow!("trusted contact encode: {e}"))?;
    db::repository::set_setting(db, &trusted_contact_setting_key(remote_node_id), &raw)?;
    Ok(())
}

pub fn delete_trusted_contact_hints(
    db: &crate::db::DbPool,
    remote_node_id: &str,
) -> anyhow::Result<()> {
    db::repository::delete_setting(db, &trusted_contact_setting_key(remote_node_id))?;
    Ok(())
}

/// Wzorce hostow uznanych za martwe relay URL — usuwane przy starcie mesh.
/// Do commitu e9552dc zapisywalismy domyslne `https://use.iroh.network/`, ktore
/// od dawna nie resolwuje DNS. Po naprawie Fazy 1 nowe eventy zapisuja pusty
/// string, ale w bazie starych instalacji moze lezec martwy URL — sanitizer
/// musi go wyczyscic zanim `connect_to_peer_with_hints` wejdzie w dial fail.
const DEAD_RELAY_PATTERNS: &[&str] = &["use.iroh.network"];

/// Czysci pole `relay_url` w `trusted_contact:*` gdy matchuje wzorzec martwego
/// hosta (patrz `DEAD_RELAY_PATTERNS`). Pojedyncze bledy dekodowania/zapisu
/// sa tylko logowane — iteracja idzie dalej zeby nie zablokowac startu mesh
/// jednym skorumpowanym wpisem.
///
/// Zwraca liczbe faktycznie zaktualizowanych wpisow.
pub fn sanitize_trusted_contacts(db: &crate::db::DbPool) -> anyhow::Result<usize> {
    let rows = db::repository::list_settings_with_prefix(db, TRUSTED_CONTACT_PREFIX)?;
    let mut cleaned = 0usize;

    for (key, raw_value) in rows {
        let mut hints = match serde_json::from_str::<PairingContactHints>(&raw_value) {
            Ok(h) => h,
            Err(e) => {
                warn!(
                    key = %key,
                    "sanitize_trusted_contacts: pominieto wpis — decode nieudany: {}",
                    e
                );
                continue;
            }
        };

        let has_dead = DEAD_RELAY_PATTERNS
            .iter()
            .any(|p| hints.relay_url.contains(p));
        if !has_dead {
            continue;
        }

        let original = std::mem::take(&mut hints.relay_url);
        let serialized = match serde_json::to_string(&hints) {
            Ok(s) => s,
            Err(e) => {
                warn!(
                    key = %key,
                    "sanitize_trusted_contacts: pominieto wpis — encode nieudany: {}",
                    e
                );
                continue;
            }
        };
        match db::repository::set_setting(db, &key, &serialized) {
            Ok(()) => {
                cleaned += 1;
                info!(
                    key = %key,
                    old_url = %original,
                    "sanitize_trusted_contacts: wyczyszczono martwy relay URL"
                );
            }
            Err(e) => warn!(
                key = %key,
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
) -> PairingContactHints {
    if !hints.relay_url.trim().is_empty() {
        return hints.clone();
    }
    let our_relay = endpoint
        .addr()
        .relay_urls()
        .next()
        .map(|u| u.to_string())
        .unwrap_or_default();
    if our_relay.is_empty() {
        return hints.clone();
    }
    let mut filled = hints.clone();
    filled.relay_url = our_relay;
    filled
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

    #[test]
    fn endpoint_addr_zawiera_direct_i_relay() {
        let node_id = hex::encode(iroh::SecretKey::generate().public().as_bytes());
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
    fn sanitize_trusted_contacts_clears_dead_relay() {
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("init test DB");

        let dead = PairingContactHints {
            node_id: "peer-dead".to_string(),
            public_key_hex: "aa".to_string(),
            hostname: "host-dead".to_string(),
            addresses: vec!["10.0.0.1:8090".to_string()],
            relay_url: "https://use.iroh.network/".to_string(),
        };
        let good = PairingContactHints {
            node_id: "peer-good".to_string(),
            public_key_hex: "bb".to_string(),
            hostname: "host-good".to_string(),
            addresses: vec![],
            relay_url: "https://my-relay.example.com/".to_string(),
        };
        let empty = PairingContactHints {
            node_id: "peer-empty".to_string(),
            public_key_hex: "cc".to_string(),
            hostname: "host-empty".to_string(),
            addresses: vec![],
            relay_url: String::new(),
        };

        store_trusted_contact_hints(&db, "peer-dead", &dead).unwrap();
        store_trusted_contact_hints(&db, "peer-good", &good).unwrap();
        store_trusted_contact_hints(&db, "peer-empty", &empty).unwrap();

        let cleaned = sanitize_trusted_contacts(&db).expect("sanitize");
        assert_eq!(cleaned, 1, "tylko jeden wpis powinien byc czyszczony");

        let loaded_dead = load_trusted_contact_hints(&db, "peer-dead")
            .expect("load dead")
            .expect("dead present");
        assert!(
            loaded_dead.relay_url.is_empty(),
            "dead URL powinien byc wyczyszczony"
        );
        // Pozostale pola nietkniete.
        assert_eq!(loaded_dead.hostname, "host-dead");
        assert_eq!(loaded_dead.addresses, vec!["10.0.0.1:8090".to_string()]);

        let loaded_good = load_trusted_contact_hints(&db, "peer-good")
            .expect("load good")
            .expect("good present");
        assert_eq!(
            loaded_good.relay_url, "https://my-relay.example.com/",
            "dobry URL nietkniety"
        );

        let loaded_empty = load_trusted_contact_hints(&db, "peer-empty")
            .expect("load empty")
            .expect("empty present");
        assert!(loaded_empty.relay_url.is_empty(), "pusty dalej pusty");

        // Idempotentnosc — drugi przebieg nie powinien nic zmieniac.
        let cleaned2 = sanitize_trusted_contacts(&db).expect("sanitize idempotent");
        assert_eq!(cleaned2, 0, "drugi przebieg nie powinien nic czyscic");
    }

    #[test]
    fn endpoint_addr_pomija_niepoprawne_adresy() {
        let node_id = hex::encode(iroh::SecretKey::generate().public().as_bytes());
        let hints = PairingContactHints {
            node_id,
            public_key_hex: String::new(),
            hostname: String::new(),
            addresses: vec!["nie-adres".to_string(), "127.0.0.1:8090".to_string()],
            relay_url: String::new(),
        };

        let addr = endpoint_addr_from_hints(&hints).expect("endpoint addr");
        let direct: Vec<_> = addr.ip_addrs().copied().collect();
        assert_eq!(direct, vec!["127.0.0.1:8090".parse::<SocketAddr>().unwrap()]);
    }
}
