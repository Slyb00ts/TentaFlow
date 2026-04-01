// =============================================================================
// Plik: api/dashboard/api_mesh.rs
// Opis: Endpointy API dla mesh networking — lista peerow, parowanie, zaufanie.
//       Wysyla wiadomosci parowania przez QUIC do zdalnych peerow.
// =============================================================================

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::net::SocketAddr;
use std::time::Instant;

use crate::db::{self, DbPool};
use crate::mesh::node_info_collector;
use crate::mesh::peer_store::MeshPeerStore;
use crate::mesh::quic_mesh::QuicMeshManager;
use crate::mesh::security::MeshSecurity;
use anyhow::Result;
use serde::Deserialize;
use tracing::{info, warn};

/// Ograniczenie czestotliwosci zmian konfiguracji sieci: max 1 na 30s per node
static NETWORK_CONFIG_RATE_LIMIT: LazyLock<Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Maksymalny rozmiar body dla endpointow mesh (64 KiB)
const MAX_MESH_BODY_SIZE: usize = 64 * 1024;

fn json_error(message: &str) -> String {
    serde_json::json!({"error": message}).to_string()
}

/// Sprawdza czy identyfikator zawiera tylko dozwolone znaki
fn is_valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() < 256 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// GET /api/mesh/peers — lista wszystkich peerow w mesh
pub fn handle_list_peers(store: &MeshPeerStore) -> Result<(u16, String)> {
    let peers = store.list();
    let json = serde_json::to_string(&peers)?;
    Ok((200, json))
}

/// GET /api/mesh/trusted — lista zaufanych nodow
pub fn handle_list_trusted(pool: &DbPool) -> Result<(u16, String)> {
    let nodes = db::repository::list_trusted_nodes(pool)?;
    let json = serde_json::to_string(&nodes)?;
    Ok((200, json))
}

/// GET /api/mesh/pending — lista oczekujacych parowan
pub fn handle_list_pending(pool: &DbPool) -> Result<(u16, String)> {
    // Wyczysc wygasle
    let _ = db::repository::cleanup_expired_pairings(pool);
    let pairings = db::repository::list_pending_pairings(pool)?;
    let json = serde_json::to_string(&pairings)?;
    Ok((200, json))
}

/// POST /api/mesh/pair/:node_id — rozpocznij parowanie (generuje PIN)
/// Po wygenerowaniu PIN wysyla PairingRequest przez QUIC do zdalnego peera.
/// VULN-021: Sprawdza czy istnieje juz oczekujace parowanie dla tego node_id.
pub async fn handle_initiate_pairing(
    pool: &DbPool,
    security: &Arc<MeshSecurity>,
    remote_node_id: &str,
    quic_mesh: &Option<Arc<QuicMeshManager>>,
    local_node_id: &str,
    peer_store: &MeshPeerStore,
) -> Result<(u16, String)> {
    if !is_valid_id(remote_node_id) {
        return Ok((400, json_error("Niepoprawny node_id")));
    }

    // VULN-021: Sprawdz czy juz istnieje oczekujace parowanie dla tego node_id
    if let Ok(Some(_)) = db::repository::get_pending_pairing(pool, remote_node_id) {
        return Ok((429, json_error("Parowanie dla tego noda juz trwa — poczekaj na wygasniecie lub odrzuc")));
    }

    let pin = security.initiate_pairing(remote_node_id)?;

    // Wyslij PairingRequest przez QUIC — synchronicznie, z informacja o bledzie
    if let Some(ref qm) = quic_mesh {
        let payload = serde_json::json!({
            "from_node_id": local_node_id,
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
                let mut addrs: Vec<std::net::IpAddr> = peer.addresses.iter()
                    .filter(|a| {
                        if let std::net::IpAddr::V4(v4) = a {
                            !v4.is_loopback()
                                && !(v4.octets()[0] == 172 && v4.octets()[1] >= 16 && v4.octets()[1] <= 31)
                                && !v4.is_link_local()
                        } else {
                            false
                        }
                    })
                    .copied()
                    .collect();
                // Fallback: jakikolwiek IPv4
                if addrs.is_empty() {
                    addrs = peer.addresses.iter().filter(|a| a.is_ipv4()).copied().collect();
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
            return Ok((502, json_error("Nie udalo sie wyslac PairingRequest — node moze nie byc osiagalny")));
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
    quic_mesh: &Option<Arc<QuicMeshManager>>,
    local_node_id: &str,
) -> Result<(u16, String)> {
    if !is_valid_id(remote_node_id) {
        return Ok((400, json_error("Niepoprawny node_id")));
    }

    if body.len() > MAX_MESH_BODY_SIZE {
        return Ok((413, json_error("Zbyt duzy request body")));
    }

    let req: ConfirmPairingRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Blad parsowania: {}", e))?;

    let hostname = req.hostname.as_deref().unwrap_or("");

    // Rate limit: max 3 proby PIN w 60s
    if !security.check_pin_rate_limit(remote_node_id) {
        return Ok((429, json_error("Zbyt wiele prob — poczekaj 60 sekund")));
    }

    // Weryfikuj PIN — jesli mamy go lokalnie (inicjator), sprawdz.
    // Jesli nie mamy (receiver — PIN nie przyszedl przez wire), przepusc.
    // PIN od user-a jest wysylany w PairingConfirm do inicjatora, ktory go zweryfikuje.
    let stored_pin = security.get_pending_pin(remote_node_id)
        .ok()
        .flatten();
    if let Some(ref expected) = stored_pin {
        match &req.pin {
            Some(provided) if provided == expected => {}
            _ => {
                return Ok((403, json_error("Nieprawidlowy PIN")));
            }
        }
    }

    // Pobierz klucz publiczny inicjatora zapisany w receive_pairing_request
    let remote_public_key = db::repository::get_setting(&security.db, &format!("pending_pubkey:{}", remote_node_id))
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
                let payload = serde_json::json!({
                    "from_node_id": local_node_id,
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
                            warn!("Blad wysylania NodeInfo po sparowaniu do {}: {}", node_id, e);
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
                        let payload = tentaflow_protocol::mesh::TrustedKeysSyncPayload { keys: entries };
                        if let Ok(sync_data) = rkyv::to_bytes::<rkyv::rancor::Error>(&payload).map(|v| v.to_vec()) {
                            // Wyslij do nowego peera
                            if let Err(e) = qm.send_trusted_keys_sync(&node_id, &sync_data).await {
                                warn!("Blad wysylania TrustedKeysSync do {}: {}", node_id, e);
                            }
                            // Broadcast do WSZYSTKICH pozostalych trusted peerow
                            qm.broadcast_to_trusted(
                                tentaflow_protocol::mesh::MESH_MSG_TRUSTED_KEYS_SYNC,
                                &sync_data,
                                Some(&node_id),
                            ).await;
                        }
                    }
                });
            }

            // Wyczysc tymczasowy klucz publiczny z pending
            let _ = db::repository::delete_setting(&security.db, &format!("pending_pubkey:{}", remote_node_id));

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
    quic_mesh: &Option<Arc<QuicMeshManager>>,
    local_node_id: &str,
) -> Result<(u16, String)> {
    if !is_valid_id(remote_node_id) {
        return Ok((400, json_error("Niepoprawny node_id")));
    }

    security.reject_pairing(remote_node_id)?;

    // Wyslij PairingReject przez QUIC w tle
    if let Some(ref qm) = quic_mesh {
        let payload = serde_json::json!({
            "from_node_id": local_node_id,
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
    quic_mesh: &Option<Arc<QuicMeshManager>>,
    local_node_id: &str,
) -> Result<(u16, String)> {
    if !is_valid_id(node_id) {
        return Ok((400, json_error("Niepoprawny node_id")));
    }

    // Audit log
    let _ = crate::db::repository::log_audit(
        &security.db, None, None, "trust_revoked", None,
        Some(&format!("Cofnieto zaufanie dla {} przez admina", node_id)), None, Some(node_id),
    );

    if let Some(ref qm) = quic_mesh {
        let payload = tentaflow_protocol::mesh::TrustRevokedPayload {
            revoked_node_id: node_id.to_string(),
            from_node_id: local_node_id.to_string(),
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
            if let Err(e) = qm.send_to_peer(&revoked_id, tentaflow_protocol::mesh::MESH_MSG_TRUST_REVOKED, &data).await {
                warn!("Blad wysylania TrustRevoked do revokowanego {}: {}", revoked_id, e);
            }
            qm.broadcast_to_trusted(
                tentaflow_protocol::mesh::MESH_MSG_TRUST_REVOKED,
                &data,
                Some(&revoked_id),
            ).await;

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

/// GET /api/mesh/identity — klucz publiczny tego noda
pub fn handle_get_identity(security: &Arc<MeshSecurity>) -> Result<(u16, String)> {
    let json = serde_json::json!({
        "public_key": security.public_key_hex(),
        "ed25519_key": security.ed25519_public_key_hex(),
        "x25519_key": security.x25519_public_key_hex(),
    })
    .to_string();
    Ok((200, json))
}

/// Pierwszy nie-loopback adres IPv4 jako string
fn first_non_loopback_ip(addresses: &[std::net::IpAddr]) -> Option<String> {
    addresses.iter()
        .find(|a| a.is_ipv4() && !a.is_loopback())
        .map(|a| a.to_string())
}

/// Lista adresow IP jako stringi
fn addresses_to_strings(addresses: &[std::net::IpAddr]) -> Vec<String> {
    addresses.iter().map(|a| a.to_string()).collect()
}

/// GPU count i nazwy z gpu_info
fn gpu_summary(gpu_info: &[crate::mesh::peer_store::PeerGpuInfo]) -> (usize, Vec<String>) {
    let count = gpu_info.len();
    let names: Vec<String> = gpu_info.iter().map(|g| g.name.clone()).collect();
    (count, names)
}

/// Sprawdza czy peer jest duplikatem lokalnego noda lub loopback-only
fn is_loopback_or_local_duplicate(peer: &crate::mesh::peer_store::MeshPeerInfo, local_node_id: &str) -> bool {
    // Duplikat lokalnego noda
    if peer.node_id == local_node_id && peer.hostname != local_node_id {
        return false; // To jest sam lokalny node — nie filtruj
    }

    // Peer z hostname "127.0.0.1" — to nie jest prawdziwy host
    if peer.hostname == "127.0.0.1" || peer.hostname == "::1" {
        return true;
    }

    // Peer ktorego jedyne adresy to loopback
    if !peer.addresses.is_empty() && peer.addresses.iter().all(|a| a.is_loopback()) {
        return true;
    }

    false
}

/// GET /api/mesh/nodes — lista wszystkich nodow (local + discovered + trusted).
/// WSZYSTKIE dane z peer_store cache — zero wywolan collect_*/sysinfo/docker.
pub fn handle_list_nodes(
    store: &MeshPeerStore,
    pool: &DbPool,
    local_node_id: &str,
    mesh_security: &Option<Arc<MeshSecurity>>,
) -> Result<(u16, String)> {
    let peers = store.list();
    let trusted = db::repository::list_trusted_nodes(pool)?;

    // Zbierz node_id juz obecnych w peers
    let peer_ids: std::collections::HashSet<String> = peers.iter().map(|p| p.node_id.clone()).collect();

    // Polacz — peers maja priorytet (maja metryki), trusted dodajemy jesli nie ma w peers
    let mut nodes: Vec<serde_json::Value> = peers.iter().filter(|p| {
        if p.node_id == local_node_id {
            return true;
        }
        !is_loopback_or_local_duplicate(p, local_node_id)
    }).map(|p| {
        let is_local = p.node_id == local_node_id;
        let is_trusted = is_local || trusted.iter().any(|t| t.node_id == p.node_id)
            || mesh_security.as_ref().map_or(false, |s| s.is_trusted(&p.node_id));
        let (source, trust) = if is_local {
            ("local", "local")
        } else if is_trusted {
            ("trusted", "trusted")
        } else {
            ("discovered", "discovered")
        };

        let ip = first_non_loopback_ip(&p.addresses);
        let ip_addresses = addresses_to_strings(&p.addresses);
        let (gpu_count, gpu_names) = gpu_summary(&p.gpu_info);

        // Kontenery: running / total
        let containers_running = p.containers.iter().filter(|c| c.status.contains("running") || c.status.contains("Up")).count();
        let containers_total = p.containers.len();

        // Siec: suma rx/tx per second
        let network_rx: u64 = p.networks.iter().map(|n| n.rx_bytes_per_sec).sum();
        let network_tx: u64 = p.networks.iter().map(|n| n.tx_bytes_per_sec).sum();

        // Routing info — relay/direct, hops, next_hop
        let route_info = if is_local {
            serde_json::json!({"direct": true, "hops": 0})
        } else if let Some(route) = store.get_route(&p.node_id) {
            serde_json::json!({
                "direct": route.direct,
                "hops": route.hops,
                "next_hop": if route.direct { serde_json::Value::Null } else { serde_json::Value::String(route.next_hop.clone()) },
            })
        } else {
            serde_json::json!({"direct": false, "hops": null})
        };

        serde_json::json!({
            "node_id": p.node_id,
            "hostname": p.hostname,
            "addresses": p.addresses,
            "ip": ip,
            "ip_addresses": ip_addresses,
            "port": p.port,
            "role": p.role,
            "status": p.status,
            "quic_connected": p.quic_connected,
            "os_info": p.os_info,
            "platform": p.platform,
            "cpu_count": p.cpu_count,
            "cpu_usage": p.cpu_usage_percent,
            "ram_total_mb": p.ram_total_mb,
            "cpu_usage_percent": p.cpu_usage_percent,
            "ram_used_mb": p.ram_used_mb,
            "gpu_info": p.gpu_info,
            "gpu_count": gpu_count,
            "gpu_names": gpu_names,
            "containers_running": containers_running,
            "containers_total": containers_total,
            "network_rx_bytes": network_rx,
            "network_tx_bytes": network_tx,
            "is_trusted": is_trusted,
            "is_local": is_local,
            "source": source,
            "trust": trust,
            "route": route_info,
        })
    }).collect();

    // Dodaj zaufane nody ktore nie sa w discovered
    for t in &trusted {
        if t.node_id == local_node_id {
            continue;
        }
        if t.hostname == "127.0.0.1" || t.hostname == "::1" {
            continue;
        }
        if !peer_ids.contains(&t.node_id) {
            nodes.push(serde_json::json!({
                "node_id": t.node_id,
                "hostname": t.hostname,
                "is_trusted": true,
                "is_local": false,
                "is_active": t.is_active,
                "source": "trusted",
                "trust": "trusted",
                "status": if t.is_active { "offline" } else { "inactive" },
            }));
        }
    }

    Ok((200, serde_json::to_string(&nodes)?))
}

/// GET /api/mesh/nodes/:id — szczegoly noda (metryki, serwisy, platforma).
/// WSZYSTKIE dane z peer_store cache — zero wywolan collect_*/sysinfo/docker.
pub fn handle_get_node(
    store: &MeshPeerStore,
    quic_mesh: &Option<Arc<QuicMeshManager>>,
    node_id: &str,
    local_node_id: &str,
    mesh_security: &Option<Arc<MeshSecurity>>,
    pool: &DbPool,
) -> Result<(u16, String)> {
    let peer = store.get(node_id);

    match peer {
        Some(p) => {
            let is_local = p.node_id == local_node_id;

            // Serwisy noda z rejestru
            let services = quic_mesh.as_ref().map(|qm| {
                let registry = qm.service_registry();
                let all = registry.visible_services();
                all.into_iter()
                    .filter(|s| s.node_id == node_id)
                    .collect::<Vec<_>>()
            }).unwrap_or_default();

            // Status zaufania
            let trusted = db::repository::list_trusted_nodes(pool)?;
            let is_trusted = is_local || trusted.iter().any(|t| t.node_id == p.node_id)
                || mesh_security.as_ref().map_or(false, |s| s.is_trusted(&p.node_id));
            let trust = if is_local { "local" } else if is_trusted { "trusted" } else { "discovered" };

            let ip = first_non_loopback_ip(&p.addresses);
            let ip_addresses = addresses_to_strings(&p.addresses);
            let (gpu_count, gpu_names) = gpu_summary(&p.gpu_info);
            let network_interfaces: Vec<serde_json::Value> = p.networks.iter().map(|n| {
                serde_json::json!({
                    "name": n.name,
                    "rx_bytes_per_sec": n.rx_bytes_per_sec,
                    "tx_bytes_per_sec": n.tx_bytes_per_sec,
                    "link_up": n.link_up,
                    "ipv4_address": n.ipv4_address,
                    "ipv4_netmask": n.ipv4_netmask,
                    "ipv4_gateway": n.ipv4_gateway,
                    "mac_address": n.mac_address,
                    "interface_type": n.interface_type,
                    "rdma_available": n.rdma_available,
                    "speed_mbps": n.speed_mbps,
                })
            }).collect();

            let json = serde_json::json!({
                "node_id": p.node_id,
                "hostname": p.hostname,
                "addresses": p.addresses,
                "ip": ip,
                "ip_addresses": ip_addresses,
                "port": p.port,
                "role": p.role,
                "status": p.status,
                "quic_connected": p.quic_connected,
                "os_info": p.os_info,
                "platform": p.platform,
                "cpu_count": p.cpu_count,
                "cpu_usage": p.cpu_usage_percent,
                "ram_total_mb": p.ram_total_mb,
                "cpu_usage_percent": p.cpu_usage_percent,
                "ram_used_mb": p.ram_used_mb,
                "cpu_temperature_c": p.cpu_temperature_c,
                "swap_total_mb": p.swap_total_mb,
                "swap_used_mb": p.swap_used_mb,
                "gpu_info": p.gpu_info,
                "gpu_count": gpu_count,
                "gpu_names": gpu_names,
                "containers": p.containers,
                "networks": p.networks,
                "network_interfaces": network_interfaces,
                "services": services,
                "is_local": is_local,
                "is_trusted": is_trusted,
                "trust": trust,
                "docker_available": p.docker_available,
                "docker_version": p.docker_version,
            });
            Ok((200, json.to_string()))
        }
        None => Ok((404, json_error(&format!("Node '{}' nie znaleziony", node_id)))),
    }
}

#[derive(Deserialize)]
pub struct ConnectRequest {
    pub address: String,
}

/// POST /api/mesh/connect — reczne polaczenie IP:port
pub async fn handle_connect(
    quic_mesh: &Option<Arc<QuicMeshManager>>,
    body: &[u8],
) -> Result<(u16, String)> {
    if body.len() > MAX_MESH_BODY_SIZE {
        return Ok((413, json_error("Zbyt duzy request body")));
    }

    let req: ConnectRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    let qm = match quic_mesh {
        Some(ref qm) => qm,
        None => return Ok((503, json_error("Mesh manager niedostepny"))),
    };

    let addr: SocketAddr = req.address.parse()
        .map_err(|_| anyhow::anyhow!("Niepoprawny format adresu (oczekiwany IP:port)"))?;

    // Odrzuc adresy wewnetrzne (ochrona przed SSRF)
    let ip = addr.ip();
    if ip.is_loopback() || ip.is_unspecified() {
        return Ok((400, json_error("Niedozwolony adres docelowy")));
    }
    if let std::net::IpAddr::V4(v4) = ip {
        if v4.is_link_local() {
            return Ok((400, json_error("Niedozwolony adres docelowy")));
        }
    }

    // Generuj tymczasowe node_id — peer wymieni sie prawdziwym po handshake
    let temp_node_id = format!("manual-{}", addr);

    match qm.connect_to_peer(&temp_node_id, addr).await {
        Ok(()) => Ok((200, serde_json::json!({"ok": true, "address": req.address}).to_string())),
        Err(e) => Ok((502, json_error(&format!("Blad polaczenia: {}", e)))),
    }
}

#[derive(Deserialize)]
pub struct CommandRequest {
    pub command_type: String,
    pub params: Option<serde_json::Value>,
}

/// POST /api/mesh/nodes/:id/command — wyslij komende do noda
pub async fn handle_send_command(
    quic_mesh: &Option<Arc<QuicMeshManager>>,
    mesh_security: &Option<Arc<MeshSecurity>>,
    node_id: &str,
    body: &[u8],
) -> Result<(u16, String)> {
    // Waliduj node_id z URL
    if !is_valid_id(node_id) {
        return Ok((400, json_error("Niepoprawny node_id")));
    }

    let is_trusted = mesh_security.as_ref().map_or(false, |s| s.is_trusted(node_id));
    if !is_trusted {
        return Ok((403, json_error("Node nie jest zaufany — nie mozna wyslac komendy")));
    }

    if body.len() > MAX_MESH_BODY_SIZE {
        return Ok((413, json_error("Zbyt duzy request body")));
    }

    let req: CommandRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    let qm = match quic_mesh {
        Some(ref qm) => qm,
        None => return Ok((503, json_error("Mesh manager niedostepny"))),
    };

    let params = req.params.unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

    // Waliduj container_id jesli jest obecny w parametrach
    if let Some(cid) = params.get("container_id").and_then(|v| v.as_str()) {
        if !is_valid_id(cid) {
            return Ok((400, json_error("Niepoprawny container_id")));
        }
    }

    // Dane do audytu konfiguracji sieci (wypelniane w galezi "network_config")
    let mut net_config_audit: Option<(String, bool, Option<String>)> = None;

    // Mapuj command_type na MeshCommandType
    let command = match req.command_type.as_str() {
        "list_containers" => tentaflow_protocol::mesh::MeshCommandType::ListContainers,
        "list_images" => tentaflow_protocol::mesh::MeshCommandType::ListImages,
        "container_start" => {
            let id = params.get("container_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            tentaflow_protocol::mesh::MeshCommandType::ContainerStart { container_id: id }
        }
        "container_stop" => {
            let id = params.get("container_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            tentaflow_protocol::mesh::MeshCommandType::ContainerStop { container_id: id }
        }
        "container_restart" => {
            let id = params.get("container_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            tentaflow_protocol::mesh::MeshCommandType::ContainerRestart { container_id: id }
        }
        "system_prune" => {
            let volumes = params.get("volumes").and_then(|v| v.as_bool()).unwrap_or(false);
            tentaflow_protocol::mesh::MeshCommandType::SystemPrune { volumes }
        }
        "network_config" => {
            // Rate limit: max 1 zmiana konfiguracji sieci na 30s per node
            {
                let mut rate_map = NETWORK_CONFIG_RATE_LIMIT.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(last) = rate_map.get(node_id) {
                    if last.elapsed() < std::time::Duration::from_secs(30) {
                        return Ok((429, json_error("Zbyt czeste zmiany konfiguracji sieci — odczekaj 30s")));
                    }
                }
                rate_map.insert(node_id.to_string(), Instant::now());
            }

            let interface = params.get("interface").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let ipv4 = params.get("ipv4").and_then(|v| v.as_str()).map(String::from);
            let netmask = params.get("netmask").and_then(|v| v.as_str()).map(String::from);
            let gateway = params.get("gateway").and_then(|v| v.as_str()).map(String::from);
            let dhcp = params.get("dhcp").and_then(|v| v.as_bool()).unwrap_or(false);
            let sudo_password = params.get("sudo_password").and_then(|v| v.as_str()).unwrap_or("").to_string();

            if interface.is_empty() {
                return Ok((400, json_error("Pole 'interface' jest wymagane")));
            }
            if sudo_password.is_empty() {
                return Ok((400, json_error("Pole 'sudo_password' jest wymagane")));
            }

            net_config_audit = Some((interface.clone(), dhcp, ipv4.clone()));

            tentaflow_protocol::mesh::MeshCommandType::NetworkConfig {
                interface,
                ipv4,
                netmask,
                gateway,
                dhcp,
                sudo_password,
            }
        }
        other => return Ok((400, json_error(&format!("Nieznany typ komendy: {}", other)))),
    };

    match qm.send_command(node_id, command).await {
        Ok(response) => {
            if let Some((ref iface, dhcp, ref ipv4)) = net_config_audit {
                info!(
                    node_id = %node_id,
                    interface = %iface,
                    dhcp = dhcp,
                    ipv4 = ?ipv4,
                    success = response.success,
                    "Konfiguracja sieci wykonana"
                );
            }
            let json = serde_json::json!({
                "success": response.success,
                "output": response.output,
                "error": response.error,
            });
            Ok((200, json.to_string()))
        }
        Err(e) => Ok((502, json_error(&format!("Blad wykonania komendy: {}", e)))),
    }
}

#[derive(Deserialize)]
struct NetworkConfigRequest {
    pub interface: String,
    pub ipv4: Option<String>,
    pub netmask: Option<String>,
    pub gateway: Option<String>,
    #[serde(default)]
    pub dhcp: bool,
    pub sudo_password: String,
}

/// POST /api/mesh/nodes/:id/network-config — zmiana konfiguracji sieciowej na zdalnym nodzie
pub async fn handle_network_config(
    quic_mesh: &Option<Arc<QuicMeshManager>>,
    mesh_security: &Option<Arc<MeshSecurity>>,
    node_id: &str,
    body: &[u8],
) -> Result<(u16, String)> {
    if !is_valid_id(node_id) {
        return Ok((400, json_error("Niepoprawny node_id")));
    }

    if body.len() > MAX_MESH_BODY_SIZE {
        return Ok((413, json_error("Zbyt duzy request body")));
    }

    // Rate limit: max 1 zmiana konfiguracji sieci na 30s per node
    {
        let mut rate_map = NETWORK_CONFIG_RATE_LIMIT.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(last) = rate_map.get(node_id) {
            if last.elapsed() < std::time::Duration::from_secs(30) {
                return Ok((429, json_error("Zbyt czeste zmiany konfiguracji sieci — odczekaj 30s")));
            }
        }
        rate_map.insert(node_id.to_string(), Instant::now());
    }

    // Sprawdz trust PRZED wyslaniem hasla sudo do zdalnego noda
    let is_trusted = mesh_security
        .as_ref()
        .map_or(false, |s| s.is_trusted(node_id));
    if !is_trusted {
        return Ok((403, json_error("Node nie jest zaufany — nie mozna wyslac konfiguracji sieci")));
    }

    let req: NetworkConfigRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    if req.interface.is_empty() {
        return Ok((400, json_error("Pole 'interface' jest wymagane")));
    }
    if req.sudo_password.is_empty() {
        return Ok((400, json_error("Pole 'sudo_password' jest wymagane")));
    }

    let qm = match quic_mesh {
        Some(ref qm) => qm,
        None => return Ok((503, json_error("Mesh manager niedostepny"))),
    };

    // Zachowaj dane do audytu przed przeniesieniem do command
    let log_interface = req.interface.clone();
    let log_dhcp = req.dhcp;
    let log_ipv4 = req.ipv4.clone();

    let command = tentaflow_protocol::mesh::MeshCommandType::NetworkConfig {
        interface: req.interface,
        ipv4: req.ipv4,
        netmask: req.netmask,
        gateway: req.gateway,
        dhcp: req.dhcp,
        sudo_password: req.sudo_password,
    };

    info!(node_id = %node_id, interface = %log_interface, "Wysylam NetworkConfig do noda");
    match qm.send_command(node_id, command).await {
        Ok(response) => {
            info!(
                node_id = %node_id,
                interface = %log_interface,
                dhcp = log_dhcp,
                ipv4 = ?log_ipv4,
                success = response.success,
                "Konfiguracja sieci wykonana"
            );
            let json = serde_json::json!({
                "success": response.success,
                "output": response.output,
                "error": response.error,
            });
            Ok((200, json.to_string()))
        }
        Err(e) => Ok((502, json_error(&format!("Blad wykonania komendy: {}", e)))),
    }
}

/// POST /api/mesh/retrust/:node_id — przywroc zaufanie (admin)
pub fn handle_retrust(
    security: &Arc<MeshSecurity>,
    node_id: &str,
) -> Result<(u16, String)> {
    if !is_valid_id(node_id) {
        return Ok((400, json_error("Niepoprawny node_id")));
    }

    security.admin_retrust(node_id)?;
    let json = serde_json::json!({"ok": true}).to_string();
    Ok((200, json))
}

/// GET /api/mesh/services — wszystkie serwisy w mesh
pub fn handle_list_mesh_services(
    quic_mesh: &Option<Arc<QuicMeshManager>>,
) -> Result<(u16, String)> {
    match quic_mesh {
        Some(ref qm) => {
            let services = qm.service_registry().visible_services();
            Ok((200, serde_json::to_string(&services)?))
        }
        None => Ok((200, "[]".to_string())),
    }
}
