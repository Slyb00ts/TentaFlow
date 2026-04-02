// =============================================================================
// Plik: api/dashboard/api_clusters.rs
// Opis: CRUD endpointy clusterow mesh — tworzenie, edycja, czlonkostwo nodow.
// =============================================================================

use crate::db::{self, DbPool};
use crate::mesh::cluster_probe::{
    NodeInterface, PairProbeResult, DetectionResult,
    filter_reachable_pairs, optimal_assignment,
};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use tokio::sync::{mpsc, Mutex};

#[derive(Deserialize)]
pub struct CreateClusterRequest {
    pub name: String,
    pub description: Option<String>,
    pub strategy: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateClusterRequest {
    pub name: String,
    pub description: Option<String>,
    pub strategy: Option<String>,
}

#[derive(Deserialize)]
pub struct AddMemberRequest {
    pub node_id: String,
    pub role: Option<String>,
    #[serde(default)]
    pub interface_name: String,
    #[serde(default)]
    pub interface_ip: String,
    #[serde(default)]
    pub interface_speed_mbps: i64,
    #[serde(default)]
    pub interface_type: String,
}

#[derive(Serialize)]
pub struct ClusterWithMembers {
    #[serde(flatten)]
    pub cluster: db::models::DbCluster,
    pub members: Vec<db::models::DbClusterMember>,
}

/// GET /api/clusters — lista clusterow
pub fn handle_list(pool: &DbPool) -> Result<(u16, String)> {
    let clusters = db::repository::list_clusters(pool)?;
    Ok((200, serde_json::to_string(&clusters)?))
}

/// POST /api/clusters — utworz cluster (generuje cluster_id jako UUID)
pub fn handle_create(pool: &DbPool, body: &[u8]) -> Result<(u16, String)> {
    let req: CreateClusterRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    if req.name.trim().is_empty() {
        return Ok((400, r#"{"error":"Pole 'name' nie moze byc puste"}"#.to_string()));
    }

    let cluster_id = uuid::Uuid::new_v4().to_string();
    let description = req.description.as_deref().unwrap_or("");
    let strategy = req.strategy.as_deref().unwrap_or("distributed");

    let allowed_strategies = ["distributed", "replicated", "primary_replica"];
    if !allowed_strategies.contains(&strategy) {
        return Ok((400, r#"{"error":"Niepoprawna strategia"}"#.to_string()));
    }

    db::repository::create_cluster(pool, &cluster_id, &req.name, description, strategy)?;

    let cluster = db::repository::get_cluster(pool, &cluster_id)?;
    Ok((201, serde_json::to_string(&cluster)?))
}

/// GET /api/clusters/:id — szczegoly clustera z czlonkami
pub fn handle_get(pool: &DbPool, cluster_id: &str) -> Result<(u16, String)> {
    match db::repository::get_cluster(pool, cluster_id)? {
        Some(cluster) => {
            let members = db::repository::list_cluster_members(pool, cluster_id)?;
            let result = ClusterWithMembers { cluster, members };
            Ok((200, serde_json::to_string(&result)?))
        }
        None => Ok((404, serde_json::json!({"error": format!("Cluster '{}' nie istnieje", cluster_id)}).to_string())),
    }
}

/// PUT /api/clusters/:id — aktualizuj cluster
pub fn handle_update(pool: &DbPool, cluster_id: &str, body: &[u8]) -> Result<(u16, String)> {
    if db::repository::get_cluster(pool, cluster_id)?.is_none() {
        return Ok((404, serde_json::json!({"error": format!("Cluster '{}' nie istnieje", cluster_id)}).to_string()));
    }

    let req: UpdateClusterRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    let description = req.description.as_deref().unwrap_or("");
    let strategy = req.strategy.as_deref().unwrap_or("distributed");

    let allowed_strategies = ["distributed", "replicated", "primary_replica"];
    if !allowed_strategies.contains(&strategy) {
        return Ok((400, r#"{"error":"Niepoprawna strategia"}"#.to_string()));
    }

    db::repository::update_cluster(pool, cluster_id, &req.name, description, strategy)?;

    let cluster = db::repository::get_cluster(pool, cluster_id)?;
    Ok((200, serde_json::to_string(&cluster)?))
}

/// DELETE /api/clusters/:id — usun cluster
pub fn handle_delete(pool: &DbPool, cluster_id: &str) -> Result<(u16, String)> {
    if db::repository::get_cluster(pool, cluster_id)?.is_none() {
        return Ok((404, serde_json::json!({"error": format!("Cluster '{}' nie istnieje", cluster_id)}).to_string()));
    }

    db::repository::delete_cluster(pool, cluster_id)?;
    Ok((200, r#"{"ok":true}"#.to_string()))
}

/// POST /api/clusters/:id/members — dodaj node do clustera
pub fn handle_add_member(pool: &DbPool, cluster_id: &str, body: &[u8]) -> Result<(u16, String)> {
    if db::repository::get_cluster(pool, cluster_id)?.is_none() {
        return Ok((404, serde_json::json!({"error": format!("Cluster '{}' nie istnieje", cluster_id)}).to_string()));
    }

    let req: AddMemberRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    let role = req.role.as_deref().unwrap_or("worker");

    let allowed_roles = ["worker", "coordinator", "observer"];
    if !allowed_roles.contains(&role) {
        return Ok((400, r#"{"error":"Niepoprawna rola"}"#.to_string()));
    }

    db::repository::add_cluster_member(pool, cluster_id, &req.node_id, role, &req.interface_name, &req.interface_ip, req.interface_speed_mbps, &req.interface_type)?;
    Ok((201, serde_json::json!({"ok": true, "cluster_id": cluster_id, "node_id": req.node_id}).to_string()))
}

/// DELETE /api/clusters/:id/members/:node_id — usun node z clustera
pub fn handle_remove_member(pool: &DbPool, cluster_id: &str, node_id: &str) -> Result<(u16, String)> {
    if db::repository::get_cluster(pool, cluster_id)?.is_none() {
        return Ok((404, serde_json::json!({"error": format!("Cluster '{}' nie istnieje", cluster_id)}).to_string()));
    }

    db::repository::remove_cluster_member(pool, cluster_id, node_id)?;
    Ok((200, r#"{"ok":true}"#.to_string()))
}

// =============================================================================
// Bandwidth probe API
// =============================================================================

/// Globalny stan aktywnych probe streamow (receiver + czas utworzenia + jednorazowy token SSE)
static PROBE_STREAMS: LazyLock<Mutex<HashMap<String, (mpsc::Receiver<String>, std::time::Instant, String)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Deserialize)]
pub struct ProbeRequest {
    pub nodes: Vec<ProbeNodeEntry>,
}

#[derive(Deserialize)]
pub struct ProbeNodeEntry {
    pub node_id: String,
    pub interfaces: Vec<ProbeInterfaceEntry>,
}

#[derive(Deserialize)]
pub struct ProbeInterfaceEntry {
    pub name: String,
    pub ip: String,
    pub netmask: String,
    pub speed_mbps: u64,
    pub rdma: bool,
}

/// POST /api/clusters/probe — rozpocznij probing wszystkich par.
/// Zwraca probe_id. Wyniki streamowane przez GET /api/clusters/probe/:id (SSE).
pub async fn handle_start_probe(
    body: &[u8],
    quic_mesh: Arc<crate::mesh::quic_mesh::QuicMeshManager>,
) -> Result<(u16, String)> {
    // Limit rozmiaru body (max 64KB)
    if body.len() > 65536 {
        return Ok((413, r#"{"error":"Body za duze (max 64KB)"}"#.to_string()));
    }

    let req: ProbeRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    // Walidacja rozmiaru requestu
    if req.nodes.len() > 32 {
        return Ok((400, r#"{"error":"Max 32 nody"}"#.to_string()));
    }
    let total_interfaces: usize = req.nodes.iter().map(|n| n.interfaces.len()).sum();
    if total_interfaces > 256 {
        return Ok((400, r#"{"error":"Za duzo interfejsow (max 256)"}"#.to_string()));
    }

    // Czyszczenie przeterminowanych wpisow i limit rownoleglych probow
    {
        let mut streams = PROBE_STREAMS.lock().await;
        let now = std::time::Instant::now();
        streams.retain(|_, (_, created, _)| now.duration_since(*created).as_secs() < 60);
        if streams.len() >= 5 {
            return Ok((429, r#"{"error":"Za duzo aktywnych probow. Sprobuj pozniej."}"#.to_string()));
        }
    }

    tracing::info!("Probe request: {} nodow", req.nodes.len());
    for n in &req.nodes {
        tracing::info!("  Node {}: {} interfejsow", n.node_id, n.interfaces.len());
        for i in &n.interfaces {
            tracing::info!("    {} ip={} mask={} speed={}", i.name, i.ip, i.netmask, i.speed_mbps);
        }
    }

    if req.nodes.len() < 2 {
        return Ok((400, r#"{"error":"Minimum 2 nody wymagane"}"#.to_string()));
    }

    // Waliduj ze kazdy node jest znany i polaczony
    for n in &req.nodes {
        if !quic_mesh.is_connected(&n.node_id).await && n.node_id != quic_mesh.node_id() {
            return Ok((400, serde_json::json!({"error": format!("Node {} nie jest polaczony", n.node_id)}).to_string()));
        }
    }

    let probe_id = uuid::Uuid::new_v4().to_string();
    let sse_token: String = (0..32).map(|_| format!("{:02x}", rand::random::<u8>())).collect();

    // Konwersja na NodeInterface
    let node_interfaces: Vec<Vec<NodeInterface>> = req.nodes.iter().map(|n| {
        n.interfaces.iter().map(|i| NodeInterface {
            node_id: n.node_id.clone(),
            name: i.name.clone(),
            ip: i.ip.clone(),
            netmask: i.netmask.clone(),
            speed_mbps: i.speed_mbps,
            rdma_available: i.rdma,
        }).collect()
    }).collect();

    // Pre-filter subnetow — testuj WSZYSTKIE reachable pary (nie tylko najszybsza)
    let reachable = filter_reachable_pairs(&node_interfaces);
    tracing::info!("Probe: {} osiagalnych par interfejsow do probing", reachable.len());

    // Konwertuj na flat lista par do testowania (kazda para interfejsow osobno)
    let all_pairs: Vec<(NodeInterface, NodeInterface)> = reachable;
    let total = all_pairs.len();

    // Kanal SSE
    let (tx, rx) = mpsc::channel::<String>(64);

    let probe_id_clone = probe_id.clone();
    let qm = quic_mesh.clone();
    tokio::spawn(async move {
        // Daj frontendowi czas na polaczenie z SSE
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        run_probe_orchestration(qm, all_pairs, tx, probe_id_clone).await;
    });

    {
        let mut streams = PROBE_STREAMS.lock().await;
        streams.insert(probe_id.clone(), (rx, std::time::Instant::now(), sse_token.clone()));
    }

    Ok((200, serde_json::json!({
        "probe_id": probe_id,
        "total_pairs": total,
        "sse_token": sse_token,
    }).to_string()))
}

/// GET /api/clusters/probe/:probe_id — pobierz receiver SSE wynikow probing (z walidacja tokenu)
pub async fn handle_probe_stream_with_token(probe_id: &str, token: &str) -> Option<mpsc::Receiver<String>> {
    let mut streams = PROBE_STREAMS.lock().await;
    if let Some((_, _, stored_token)) = streams.get(probe_id) {
        if stored_token == token {
            return streams.remove(probe_id).map(|(rx, _, _)| rx);
        }
    }
    None
}

/// DELETE /api/clusters/probe/:probe_id — anuluj probing
pub async fn handle_delete_probe(probe_id: &str) -> Result<(u16, String)> {
    let mut streams = PROBE_STREAMS.lock().await;
    let _ = streams.remove(probe_id);
    Ok((200, r#"{"ok":true}"#.to_string()))
}

/// Orkiestracja probing — testuj WSZYSTKIE reachable pary interfejsow.
/// Scheduler z matryca zajetosci: nie testuj rownoczesnie na tym samym interfejsie.
/// Po kazdym tescie SSE event aktualizuje GUI.
async fn run_probe_orchestration(
    qm: Arc<crate::mesh::quic_mesh::QuicMeshManager>,
    pairs: Vec<(NodeInterface, NodeInterface)>,
    tx: mpsc::Sender<String>,
    _probe_id: String,
) {
    let total = pairs.len();
    let mut results: Vec<PairProbeResult> = Vec::new();
    let nonce: [u8; 32] = rand::random();

    // Kolejka par do przetestowania, posortowana: najszybsze najpierw
    let mut queue: Vec<(NodeInterface, NodeInterface)> = pairs;
    queue.sort_by(|a, b| {
        let speed_a = std::cmp::min(a.0.speed_mbps, a.1.speed_mbps);
        let speed_b = std::cmp::min(b.0.speed_mbps, b.1.speed_mbps);
        speed_b.cmp(&speed_a)
    });

    // Matryca zajetosci: (node_id, interface_name) -> zajety
    let mut busy: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    let mut done: Vec<bool> = vec![false; queue.len()];
    let mut completed = 0;

    while completed < queue.len() {
        let mut launched_any = false;
        let mut active_handles: Vec<(usize, tokio::task::JoinHandle<PairProbeResult>)> = Vec::new();

        // Znajdz wszystkie pary ktore mozna uruchomic rownolegle (rozne interfejsy)
        for i in 0..queue.len() {
            if done[i] { continue; }

            let key_a = (queue[i].0.node_id.clone(), queue[i].0.name.clone());
            let key_b = (queue[i].1.node_id.clone(), queue[i].1.name.clone());

            if busy.contains(&key_a) || busy.contains(&key_b) { continue; }

            // Oznacz jako zajete
            busy.insert(key_a);
            busy.insert(key_b);
            done[i] = true;

            let iface_a = queue[i].0.clone();
            let iface_b = queue[i].1.clone();
            let qm = qm.clone();
            let nonce_copy = nonce;
            let tx_clone = tx.clone();
            let progress = completed + active_handles.len() + 1;
            let total_pairs = total;

            let handle = tokio::spawn(async move {
                match probe_pair(&qm, &iface_a, &iface_b, &nonce_copy, progress, total_pairs, &tx_clone).await {
                    Ok(r) => {
                        tracing::info!("Probe wynik: {} ({}) <-> {} ({}) = {:.0} Mbps (reachable={})",
                            r.node_a, iface_a.name, r.node_b, iface_b.name, r.bandwidth_mbps, r.reachable);
                        r
                    }
                    Err(e) => {
                        tracing::error!("Probe error: {} ({}) <-> {} ({}): {}",
                            iface_a.node_id, iface_a.name, iface_b.node_id, iface_b.name, e);
                        PairProbeResult {
                            node_a: iface_a.node_id.clone(),
                            node_b: iface_b.node_id.clone(),
                            interface_a: iface_a.name.clone(),
                            interface_b: iface_b.name.clone(),
                            bandwidth_mbps: 0.0,
                            latency_us: 0,
                            reachable: false,
                            rdma: false,
                        }
                    }
                }
            });

            active_handles.push((i, handle));
            launched_any = true;
        }

        if !launched_any && active_handles.is_empty() {
            // Nic nie mozna uruchomic i nic nie dziala — stuck, przerwij
            break;
        }

        // Czekaj na WSZYSTKIE uruchomione w tym cyklu
        for (idx, handle) in active_handles {
            match handle.await {
                Ok(r) => {
                    // Zwolnij interfejsy
                    busy.remove(&(queue[idx].0.node_id.clone(), queue[idx].0.name.clone()));
                    busy.remove(&(queue[idx].1.node_id.clone(), queue[idx].1.name.clone()));
                    results.push(r);
                    completed += 1;
                }
                Err(e) => {
                    tracing::error!("Probe task panic: {}", e);
                    busy.remove(&(queue[idx].0.node_id.clone(), queue[idx].0.name.clone()));
                    busy.remove(&(queue[idx].1.node_id.clone(), queue[idx].1.name.clone()));
                    completed += 1;
                }
            }
        }
    }

    tracing::info!("Probe zakonczony: {} wynikow z {} par interfejsow", results.len(), total);

    // Optymalny algorytm przypisania
    let detection = optimal_assignment(&results);

    let _ = tx.send(format!(
        "event: detection_complete\ndata: {}\n\n",
        serde_json::to_string(&detection).unwrap_or_default()
    )).await;
}

/// Probuje jedna pare interfejsow: serwer na node_b, klient na node_a
async fn probe_pair(
    qm: &crate::mesh::quic_mesh::QuicMeshManager,
    iface_a: &NodeInterface,
    iface_b: &NodeInterface,
    nonce: &[u8; 32],
    progress: usize,
    total: usize,
    tx: &mpsc::Sender<String>,
) -> Result<PairProbeResult> {
    use tentaflow_protocol::mesh::MeshCommandType;

    // Dobierz ilosc streamow na podstawie predkosci linku
    let link_speed = std::cmp::min(iface_a.speed_mbps, iface_b.speed_mbps);
    let num_streams: u8 = if link_speed >= 100000 { 16 }      // 100G+ = 16 streamow
        else if link_speed >= 10000 { 8 }                       // 10G = 8 streamow
        else if link_speed >= 1000 { 2 }                        // 1G = 2 streamy
        else { 1 };                                             // <1G = 1 stream

    tracing::info!("Probe para: {} ({}) <-> {} ({}) streams={} link_speed={}",
        iface_a.node_id, iface_a.name, iface_b.node_id, iface_b.name, num_streams, link_speed);

    // Wyslij BandwidthProbe{mode:server} do node_b
    let server_cmd = MeshCommandType::BandwidthProbe {
        target_ip: iface_b.ip.clone(),
        target_port: 0,
        rdma_port: 0,
        bind_interface: iface_b.name.clone(),
        duration_ms: 2000,
        mode: "server".to_string(),
        nonce: nonce.to_vec(),
        num_streams,
    };

    let local_node_id_srv = qm.node_id().to_string();
    let server_response = if iface_b.node_id == local_node_id_srv {
        tracing::info!("  Serwer jest lokalny, uruchamiam probe server bezposrednio na {}", iface_b.ip);
        match crate::mesh::bandwidth_probe::start_probe_server(
            &iface_b.ip, nonce, num_streams, 2000,
        ).await {
            Ok((port, handle)) => {
                tokio::spawn(async move { let _ = handle.await; });
                crate::mesh::quic_mesh::CommandWaitResponse {
                    success: true,
                    output: serde_json::json!({"port": port}).to_string(),
                    error: None,
                }
            }
            Err(e) => crate::mesh::quic_mesh::CommandWaitResponse {
                success: false,
                output: String::new(),
                error: Some(e.to_string()),
            },
        }
    } else {
        tracing::info!("  Wysylam server cmd do {}", iface_b.node_id);
        qm.send_command_and_wait(&iface_b.node_id, server_cmd, 10).await?
    };
    tracing::info!("  Server response: success={} output={}", server_response.success, server_response.output);

    if !server_response.success {
        return Ok(PairProbeResult {
            node_a: iface_a.node_id.clone(),
            node_b: iface_b.node_id.clone(),
            interface_a: iface_a.name.clone(),
            interface_b: iface_b.name.clone(),
            bandwidth_mbps: 0.0,
            latency_us: 0,
            reachable: false,
            rdma: false,
        });
    }

    // Parsuj porty z odpowiedzi serwera (TCP + opcjonalnie RDMA)
    let server_json = serde_json::from_str::<serde_json::Value>(&server_response.output)
        .unwrap_or_default();
    let port: u16 = server_json["port"].as_u64().unwrap_or(0) as u16;
    let rdma_port: u16 = server_json["rdma_port"].as_u64().unwrap_or(0) as u16;

    if port == 0 {
        return Err(anyhow::anyhow!("Serwer nie zwrocil portu TCP"));
    }

    tracing::info!("  Serwer zwrocil tcp_port={} rdma_port={}", port, rdma_port);

    // Wyslij BandwidthProbe{mode:client} do node_a z oboma portami
    let client_cmd = MeshCommandType::BandwidthProbe {
        target_ip: iface_b.ip.clone(),
        target_port: port,
        rdma_port,
        bind_interface: iface_a.name.clone(),
        duration_ms: 2000,
        mode: "client".to_string(),
        nonce: nonce.to_vec(),
        num_streams,
    };

    // Jesli klient jest lokalnym nodem, uruchom probe bezposrednio (nie przez MeshCommand)
    let local_node_id = qm.node_id().to_string();
    let client_response = if iface_a.node_id == local_node_id {
        tracing::info!("  Klient jest lokalny, uruchamiam probe bezposrednio -> {}:{}", iface_b.ip, port);
        match crate::mesh::bandwidth_probe::start_probe_client(
            &iface_b.ip, port, &iface_a.name, nonce, num_streams, 2000,
        ).await {
            Ok(result) => {
                let output = serde_json::json!({
                    "bandwidth_mbps": result.bandwidth_mbps,
                    "bytes_transferred": result.bytes_transferred,
                    "duration_ms": result.duration_ms,
                    "latency_us": result.latency_us,
                    "streams_completed": result.streams_completed,
                }).to_string();
                crate::mesh::quic_mesh::CommandWaitResponse {
                    success: true,
                    output,
                    error: None,
                }
            }
            Err(e) => {
                tracing::error!("  Lokalny probe client failed: {}", e);
                crate::mesh::quic_mesh::CommandWaitResponse {
                    success: false,
                    output: String::new(),
                    error: Some(e.to_string()),
                }
            }
        }
    } else {
        tracing::info!("  Wysylam client cmd do {} -> {}:{}", iface_a.node_id, iface_b.ip, port);
        qm.send_command_and_wait(&iface_a.node_id, client_cmd, 10).await?
    };
    tracing::info!("  Client response: success={} output={}", client_response.success, client_response.output);

    let client_json = serde_json::from_str::<serde_json::Value>(&client_response.output)
        .unwrap_or_default();
    let bandwidth_mbps = client_json["bandwidth_mbps"].as_f64().unwrap_or(0.0);
    let mut latency_us = client_json["latency_us"].as_f64().unwrap_or(0.0) as u64;
    let is_rdma = client_json["rdma"].as_bool().unwrap_or(false);

    // Fallback: uzyj QUIC RTT jesli probe nie zmierzyl latency
    if latency_us == 0 {
        let rtt_a = qm.get_peer_rtt_us(&iface_a.node_id).await;
        let rtt_b = qm.get_peer_rtt_us(&iface_b.node_id).await;
        latency_us = rtt_a.or(rtt_b).unwrap_or(0) / 2;
    }

    let result = PairProbeResult {
        node_a: iface_a.node_id.clone(),
        node_b: iface_b.node_id.clone(),
        interface_a: iface_a.name.clone(),
        interface_b: iface_b.name.clone(),
        bandwidth_mbps,
        latency_us,
        reachable: client_response.success && bandwidth_mbps > 0.0,
        rdma: iface_a.rdma_available && iface_b.rdma_available || is_rdma,
    };

    // Wyslij SSE event z postepem
    let event_data = serde_json::json!({
        "node_a": result.node_a,
        "node_b": result.node_b,
        "interface_a": result.interface_a,
        "interface_b": result.interface_b,
        "bandwidth_mbps": result.bandwidth_mbps,
        "latency_us": result.latency_us,
        "reachable": result.reachable,
        "rdma": result.rdma,
        "progress": progress,
        "total": total,
    });
    let _ = tx.send(format!("event: probe_result\ndata: {}\n\n", event_data)).await;

    Ok(result)
}
