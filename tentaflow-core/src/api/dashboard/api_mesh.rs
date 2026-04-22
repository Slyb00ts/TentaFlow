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
    quic_mesh: &Option<Arc<IrohMeshManager>>,
    _local_node_id: &str,
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

    // Wyslij PairingRequest przez QUIC — synchronicznie, z informacja o bledzie.
    // from_node_id musi byc Ed25519 pubkey hex (to samo ID ktorym iroh identyfikuje
    // peera), zeby odbiorca mogl odnalezc to samo ID w swoim discovered store.
    let local_ed25519 = security.ed25519_public_key_hex();
    if let Some(ref qm) = quic_mesh {
        let payload = serde_json::json!({
            "from_node_id": &local_ed25519,
            "public_key": security.public_key_hex(),
            "pin": &pin,
        });
        let data = payload.to_string().into_bytes();
        let node_id = remote_node_id.to_string();

        let mut sent = false;

        info!(
            target_node = %node_id,
            "Parowanie: wysylam PairingRequest"
        );

        // Proba 1: wyslij bezposrednio jesli jest polaczenie QUIC
        match qm.send_pairing_request(&node_id, &data).await {
            Ok(_) => {
                info!(target_node = %node_id, "PairingRequest wyslany (istniejace polaczenie)");
                sent = true;
            }
            Err(e) => {
                info!(target_node = %node_id, error = %e, "Brak istniejacego polaczenia — probuje nawiazac QUIC");
            }
        }

        // Brak polaczenia — nawiaz QUIC probujac kazdy adres IP peera
        if !sent {
            if let Some(peer) = peer_store.get(&node_id) {
                info!(
                    target_node = %node_id,
                    all_addresses = ?peer.addresses,
                    port = peer.port,
                    "Adresy peera z peer_store"
                );
                // Preferuj IPv4, nie-loopback, nie-Docker-bridge
                let mut addrs: Vec<std::net::IpAddr> = peer
                    .addresses
                    .iter()
                    .filter(|a| {
                        if let std::net::IpAddr::V4(v4) = a {
                            !v4.is_loopback()
                                && !(v4.octets()[0] == 172
                                    && v4.octets()[1] >= 16
                                    && v4.octets()[1] <= 31)
                                && !v4.is_link_local()
                        } else {
                            false
                        }
                    })
                    .copied()
                    .collect();
                // Fallback: jakikolwiek IPv4
                if addrs.is_empty() {
                    addrs = peer
                        .addresses
                        .iter()
                        .filter(|a| a.is_ipv4())
                        .copied()
                        .collect();
                }
                info!(
                    target_node = %node_id,
                    filtered_addresses = ?addrs,
                    "Adresy po filtracji (bez loopback/docker/link-local)"
                );
                // Probuj kazdy adres
                for ip in &addrs {
                    let addr = std::net::SocketAddr::new(*ip, peer.port);
                    info!(target_node = %node_id, address = %addr, "Probuje connect_to_peer");
                    match qm.connect_to_peer(&node_id, addr).await {
                        Ok(_) => {
                            info!(target_node = %node_id, address = %addr, "QUIC polaczony — wysylam PairingRequest");
                            if qm.send_pairing_request(&node_id, &data).await.is_ok() {
                                sent = true;
                                break;
                            }
                        }
                        Err(e) => {
                            warn!("connect_to_peer {} na {}: {}", node_id, addr, e);
                        }
                    }
                }
            }
        }

        if !sent {
            let _ = db::repository::delete_pending_pairing(&security.db, remote_node_id);
            return Ok((
                502,
                json_error("Nie udało się wysłać PairingRequest — node może nie być osiągalny"),
            ));
        }
    }

    let json = serde_json::json!({
        "pin": pin,
        "node_id": remote_node_id,
        "expires_in_seconds": 60,
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
                tokio::spawn(async move {
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
        tokio::spawn(async move {
            if let Err(e) = qm.send_pairing_reject(&node_id, &data).await {
                warn!("Blad wysylania PairingReject przez QUIC: {}", e);
            }
        });
    }

    let json = serde_json::json!({"ok": true}).to_string();
    Ok((200, json))
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
