// =============================================================================
// Plik: api/dashboard/api_mesh.rs
// Opis: Domenowe helpery mesh (pairing/trust). Wczesniej byly HTTP handlerami
//       REST; po FAZA 1b sa wolane bezposrednio przez async binary handlers
//       (patrz dispatch/mesh_write_handlers.rs). Zwracaja krotke (u16, json)
//       dla historycznej kompatybilnosci — mapper w handler konwertuje na
//       MessageBody / ProtocolError.
// =============================================================================

use std::sync::Arc;

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
use anyhow::Result;
use serde::Deserialize;
use tracing::{info, warn};

/// Maksymalny rozmiar body dla endpointow mesh (64 KiB)
const MAX_MESH_BODY_SIZE: usize = 64 * 1024;

fn json_error(message: &str) -> String {
    serde_json::json!({"error": message}).to_string()
}

/// Sprawdza czy identyfikator zawiera tylko dozwolone znaki
fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() < 256
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// POST /api/mesh/pair/:node_id — rozpocznij parowanie (generuje PIN)
/// Po wygenerowaniu PIN wysyla PairingRequest przez QUIC do zdalnego peera.
/// VULN-021: Sprawdza czy istnieje juz oczekujace parowanie dla tego node_id.
pub async fn handle_initiate_pairing(
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
) -> Result<(u16, String)> {
    if !is_valid_id(remote_node_id) {
        return Ok((400, json_error("Niepoprawny node_id")));
    }

    // VULN-021: Sprawdz czy juz istnieje oczekujace parowanie dla tego node_id
    if let Ok(Some(_)) = db::repository::get_pending_pairing(pool, remote_node_id) {
        return Ok((
            429,
            json_error("Parowanie dla tego noda juz trwa — poczekaj na wygasniecie lub odrzuc"),
        ));
    }

    let pin = security.initiate_pairing_with_pin(remote_node_id, pin_hint)?;
    let mut completed = false;

    if let Some(ref qm) = quic_mesh {
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
            let payload = serde_json::json!({
                "from_node_id": security.ed25519_public_key_hex(),
                "public_key": security.public_key_hex(),
                "pin": &pin,
            });
            let data = payload.to_string().into_bytes();
            info!(target_node = %remote_hints.node_id, "Parowanie: wysylam PairingRequest przez istniejacy mesh stream");
            if let Err(e) = qm.send_pairing_request(&remote_hints.node_id, &data).await {
                warn!(target_node = %remote_hints.node_id, "PairingRequest przez mesh failed: {}", e);
                let _ = db::repository::delete_pending_pairing(&security.db, remote_node_id);
                return Ok((
                    502,
                    json_error("Nie udało się wysłać PairingRequest — node może nie być osiągalny"),
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
                    store_trusted_contact_hints(&security.db, remote_node_id, &remote_hints)?;
                    if let Err(e) = qm.connect_to_peer_with_hints(&remote_hints).await {
                        warn!(
                            target_node = %remote_hints.node_id,
                            "Pairing confirmed, ale mesh connect nieudany: {}",
                            e
                        );
                    } else {
                        let local_info = node_info_collector::collect_node_info(local_node_id);
                        if let Ok(info_bytes) = rkyv::to_bytes::<rkyv::rancor::Error>(&local_info) {
                            if let Err(e) =
                                qm.send_node_info(&remote_hints.node_id, &info_bytes).await
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
                        let _ =
                            db::repository::delete_pending_pairing(&security.db, remote_node_id);
                        return Ok((
                            409,
                            json_error(
                                "Zdalny node nie potwierdzil zaproszenia QR — sprawdz czy PIN i kod sa nadal aktualne",
                            ),
                        ));
                    }
                    store_pending_contact_hints(&security.db, remote_node_id, &remote_hints)?;
                }
                Err(e) => {
                    warn!(target_node = %remote_hints.node_id, "PairingRequest delivery failed: {}", e);
                    let _ = delete_pending_contact_hints(&security.db, remote_node_id);
                    let _ = db::repository::delete_pending_pairing(&security.db, remote_node_id);
                    return Ok((
                        502,
                        json_error(
                            "Nie udało się wysłać PairingRequest — node może nie być osiągalny",
                        ),
                    ));
                }
            }
        }
    } else {
        let _ = db::repository::delete_pending_pairing(&security.db, remote_node_id);
        return Ok((503, json_error("Mesh manager niedostepny")));
    }

    let json = serde_json::json!({
        "pin": if completed { String::new() } else { pin.clone() },
        "node_id": remote_node_id,
        "expires_in_seconds": 60,
        "completed": completed,
    })
    .to_string();
    Ok((200, json))
}

#[derive(Deserialize)]
pub struct ConfirmPairingRequest {
    pub pin: Option<String>,
    pub hostname: Option<String>,
}

/// POST /api/mesh/pair/:node_id/confirm — potwierdz parowanie
/// Po potwierdzeniu wysyla PairingConfirm przez QUIC do zdalnego peera.
pub fn handle_confirm_pairing(
    security: &Arc<MeshSecurity>,
    remote_node_id: &str,
    body: &[u8],
    quic_mesh: &Option<Arc<IrohMeshManager>>,
    local_node_id: &str,
) -> Result<(u16, String)> {
    info!(
        remote_node_id = %remote_node_id,
        len = remote_node_id.len(),
        "handle_confirm_pairing: start"
    );
    if !is_valid_id(remote_node_id) {
        warn!(
            "handle_confirm_pairing: is_valid_id rejected remote_node_id={:?} bytes={:?}",
            remote_node_id,
            remote_node_id.as_bytes()
        );
        return Ok((400, json_error("Niepoprawny node_id")));
    }

    if body.len() > MAX_MESH_BODY_SIZE {
        return Ok((413, json_error("Zbyt duzy request body")));
    }

    let req: ConfirmPairingRequest =
        serde_json::from_slice(body).map_err(|e| anyhow::anyhow!("Blad parsowania: {}", e))?;

    let hostname = req.hostname.as_deref().unwrap_or("");

    // Rate limit: max 3 proby PIN w 60s
    if !security.check_pin_rate_limit(remote_node_id) {
        return Ok((429, json_error("Zbyt wiele prob — poczekaj 60 sekund")));
    }

    // Weryfikuj PIN — jesli mamy go lokalnie (inicjator), sprawdz.
    // Jesli nie mamy (receiver — PIN nie przyszedl przez wire), przepusc.
    // PIN od user-a jest wysylany w PairingConfirm do inicjatora, ktory go zweryfikuje.
    let stored_pin = security.get_pending_pin(remote_node_id).ok().flatten();
    if let Some(ref expected) = stored_pin {
        match &req.pin {
            Some(provided) if provided == expected => {}
            _ => {
                return Ok((403, json_error("Nieprawidlowy PIN")));
            }
        }
    }

    // Pobierz klucz publiczny inicjatora zapisany w receive_pairing_request
    let remote_public_key =
        db::repository::get_setting(&security.db, &format!("pending_pubkey:{}", remote_node_id))
            .ok()
            .flatten()
            .unwrap_or_default();

    if remote_public_key.is_empty() {
        return Ok((400, serde_json::json!({"error": "Brak klucza publicznego inicjatora — parowanie nie moze byc potwierdzone"}).to_string()));
    }

    match security.confirm_pairing(remote_node_id, &remote_public_key, hostname, "admin") {
        Ok(()) => {
            let pending_hints = load_pending_contact_hints(&security.db, remote_node_id)
                .ok()
                .flatten();
            if let Some(ref hints) = pending_hints {
                let _ = store_trusted_contact_hints(&security.db, remote_node_id, hints);
            }
            // Wyslij PairingConfirm + NodeInfo przez QUIC w tle
            if let Some(ref qm) = quic_mesh {
                let pin_for_confirm = req.pin.clone().unwrap_or_default();
                // from_node_id MUSI byc Ed25519 pubkey hex — inicjator rozpoznaje peera
                // po iroh endpoint_id (= Ed25519 pubkey hex). UUID lokalnego noda nie
                // matchuje sie z zadnym wpisem w trusted_nodes po stronie inicjatora.
                let local_ed25519 = security.ed25519_public_key_hex();
                let payload = serde_json::json!({
                    "from_node_id": &local_ed25519,
                    "public_key": security.public_key_hex(),
                    "hostname": hostname,
                    "pin": pin_for_confirm,
                });
                let qm = qm.clone();
                let sec_clone = security.clone();
                let node_id = remote_node_id.to_string();
                let local_nid = local_node_id.to_string();
                let data = payload.to_string().into_bytes();
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

                    // Poczekaj az PairingConfirm dotrze — QUIC nie gwarantuje kolejnosci miedzy streamami
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

                    // Wyslij NodeInfo do nowo zaufanego peera
                    let local_info = node_info_collector::collect_node_info(&local_nid);
                    if let Ok(info_bytes) = rkyv::to_bytes::<rkyv::rancor::Error>(&local_info) {
                        if let Err(e) = qm.send_node_info(&node_id, &info_bytes).await {
                            warn!(
                                "Blad wysylania NodeInfo po sparowaniu do {}: {}",
                                node_id, e
                            );
                        }
                    }

                    // Wyslij TrustedKeysSync do inicjatora
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
                            // Wyslij do nowego peera
                            if let Err(e) = qm.send_trusted_keys_sync(&node_id, &sync_data).await {
                                warn!("Blad wysylania TrustedKeysSync do {}: {}", node_id, e);
                            }
                            // Broadcast do WSZYSTKICH pozostalych trusted peerow
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

            // Wyczysc tymczasowy klucz publiczny z pending
            let _ = db::repository::delete_setting(
                &security.db,
                &format!("pending_pubkey:{}", remote_node_id),
            );
            let _ = delete_pending_contact_hints(&security.db, remote_node_id);

            let json = serde_json::json!({"ok": true, "node_id": remote_node_id}).to_string();
            Ok((200, json))
        }
        Err(e) => Ok((400, json_error(&e.to_string()))),
    }
}

/// POST /api/mesh/pair/:node_id/reject — odrzuc parowanie
/// Po odrzuceniu wysyla PairingReject przez QUIC do zdalnego peera.
pub fn handle_reject_pairing(
    security: &Arc<MeshSecurity>,
    remote_node_id: &str,
    quic_mesh: &Option<Arc<IrohMeshManager>>,
    local_node_id: &str,
) -> Result<(u16, String)> {
    if !is_valid_id(remote_node_id) {
        return Ok((400, json_error("Niepoprawny node_id")));
    }

    security.reject_pairing(remote_node_id)?;
    let pending_hints = load_pending_contact_hints(&security.db, remote_node_id)
        .ok()
        .flatten();

    // Wyslij PairingReject przez QUIC w tle. from_node_id = Ed25519 pubkey hex,
    // zeby peer rozpoznal nas po iroh endpoint_id.
    if let Some(ref qm) = quic_mesh {
        let _ = local_node_id;
        let payload = serde_json::json!({
            "from_node_id": security.ed25519_public_key_hex(),
        });
        let qm = qm.clone();
        let node_id = remote_node_id.to_string();
        let data = payload.to_string().into_bytes();
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

    let json = serde_json::json!({"ok": true}).to_string();
    Ok((200, json))
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

/// DELETE /api/mesh/trust/:node_id — cofnij zaufanie i broadcast do mesh
pub fn handle_revoke_trust(
    security: &Arc<MeshSecurity>,
    node_id: &str,
    quic_mesh: &Option<Arc<IrohMeshManager>>,
    local_node_id: &str,
) -> Result<(u16, String)> {
    if !is_valid_id(node_id) {
        return Ok((400, json_error("Niepoprawny node_id")));
    }

    // Audit log
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
        // from_node_id = Ed25519 pubkey hex (identyfikator iroh).
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
            // Wyslij PRZED revoke — klucze szyfrowania jeszcze istnieja
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

            // Unpair PO wyslaniu — teraz mozna usunac klucze
            if let Err(e) = sec.unpair(&revoked_id) {
                warn!("Blad unpair dla {}: {}", revoked_id, e);
            }
            sec.clear_revoking(&revoked_id);
            // NIE disconnectuj — kaskadowe disconnect powodowaly failujace broadcasty.
            // Connection umrze po QUIC idle timeout (60s).
        });
    } else {
        // Brak QUIC — unpair lokalnie
        security.mark_revoking(node_id);
        security.unpair(node_id)?;
        security.clear_revoking(node_id);
    }
    let _ = delete_trusted_contact_hints(&security.db, node_id);

    let json = serde_json::json!({"ok": true}).to_string();
    Ok((200, json))
}

/// POST /api/mesh/retrust/:node_id — przywroc zaufanie (admin)
pub fn handle_retrust(security: &Arc<MeshSecurity>, node_id: &str) -> Result<(u16, String)> {
    if !is_valid_id(node_id) {
        return Ok((400, json_error("Niepoprawny node_id")));
    }

    security.admin_retrust(node_id)?;
    let json = serde_json::json!({"ok": true}).to_string();
    Ok((200, json))
}
