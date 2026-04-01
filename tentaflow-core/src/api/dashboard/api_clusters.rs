// =============================================================================
// Plik: api/dashboard/api_clusters.rs
// Opis: CRUD endpointy clusterow mesh — tworzenie, edycja, czlonkostwo nodow.
// =============================================================================

use crate::db::{self, DbPool};
use crate::mesh::cluster_probe::{
    NodeInterface, PairProbeResult, DetectionResult,
    filter_reachable_pairs, select_fastest_per_pair, optimal_assignment,
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

/// Globalny stan aktywnych probe streamow
static PROBE_STREAMS: LazyLock<Mutex<HashMap<String, mpsc::Receiver<String>>>> =
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
    let req: ProbeRequest = serde_json::from_slice(body)
        .map_err(|e| anyhow::anyhow!("Niepoprawny JSON: {}", e))?;

    if req.nodes.len() < 2 {
        return Ok((400, r#"{"error":"Minimum 2 nody wymagane"}"#.to_string()));
    }

    let probe_id = uuid::Uuid::new_v4().to_string();

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

    // Pre-filter subnetow + smart probe (najszybszy per para)
    let reachable = filter_reachable_pairs(&node_interfaces);
    let to_probe = select_fastest_per_pair(&reachable);

    let total = to_probe.len();

    // Kanal SSE
    let (tx, rx) = mpsc::channel::<String>(64);

    let probe_id_clone = probe_id.clone();
    let qm = quic_mesh.clone();
    tokio::spawn(async move {
        run_probe_orchestration(qm, to_probe, tx, probe_id_clone).await;
    });

    {
        let mut streams = PROBE_STREAMS.lock().await;
        streams.insert(probe_id.clone(), rx);
    }

    Ok((200, serde_json::json!({
        "probe_id": probe_id,
        "total_pairs": total,
    }).to_string()))
}

/// GET /api/clusters/probe/:probe_id — pobierz receiver SSE wynikow probing
pub async fn handle_probe_stream(probe_id: &str) -> Option<mpsc::Receiver<String>> {
    let mut streams = PROBE_STREAMS.lock().await;
    streams.remove(probe_id)
}

/// DELETE /api/clusters/probe/:probe_id — anuluj probing
pub async fn handle_delete_probe(probe_id: &str) -> Result<(u16, String)> {
    let mut streams = PROBE_STREAMS.lock().await;
    streams.remove(probe_id);
    Ok((200, r#"{"ok":true}"#.to_string()))
}

/// Orkiestracja probing par — wysyla komendy BandwidthProbe do nodow
async fn run_probe_orchestration(
    qm: Arc<crate::mesh::quic_mesh::QuicMeshManager>,
    pairs: Vec<(NodeInterface, NodeInterface)>,
    tx: mpsc::Sender<String>,
    _probe_id: String,
) {
    use tentaflow_protocol::mesh::MeshCommandType;

    let total = pairs.len();
    let mut results: Vec<PairProbeResult> = Vec::new();
    let nonce: [u8; 32] = rand::random();

    // Probuj po 4 rownolegle
    for chunk in pairs.chunks(4) {
        let mut handles = Vec::new();

        for (iface_a, iface_b) in chunk {
            let qm = qm.clone();
            let nonce = nonce;
            let a = iface_a.clone();
            let b = iface_b.clone();
            let tx = tx.clone();
            let progress = results.len() + handles.len() + 1;

            handles.push(tokio::spawn(async move {
                probe_pair(&qm, &a, &b, &nonce, progress, total, &tx).await
            }));
        }

        for h in handles {
            if let Ok(Ok(result)) = h.await {
                results.push(result);
            }
        }
    }

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

    // Wyslij BandwidthProbe{mode:server} do node_b
    let server_cmd = MeshCommandType::BandwidthProbe {
        target_ip: iface_b.ip.clone(),
        target_port: 0,
        bind_interface: iface_b.name.clone(),
        duration_ms: 2000,
        mode: "server".to_string(),
        nonce: nonce.to_vec(),
        num_streams: 4,
    };

    let server_response = qm.send_command_and_wait(&iface_b.node_id, server_cmd, 5).await?;

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

    // Parsuj port z odpowiedzi serwera
    let port: u16 = serde_json::from_str::<serde_json::Value>(&server_response.output)
        .ok()
        .and_then(|v| v["port"].as_u64())
        .unwrap_or(0) as u16;

    if port == 0 {
        return Err(anyhow::anyhow!("Serwer nie zwrocil portu"));
    }

    // Wyslij BandwidthProbe{mode:client} do node_a
    let client_cmd = MeshCommandType::BandwidthProbe {
        target_ip: iface_b.ip.clone(),
        target_port: port,
        bind_interface: iface_a.name.clone(),
        duration_ms: 2000,
        mode: "client".to_string(),
        nonce: nonce.to_vec(),
        num_streams: 4,
    };

    let client_response = qm.send_command_and_wait(&iface_a.node_id, client_cmd, 5).await?;

    let bandwidth_mbps = serde_json::from_str::<serde_json::Value>(&client_response.output)
        .ok()
        .and_then(|v| v["bandwidth_mbps"].as_f64())
        .unwrap_or(0.0);

    let result = PairProbeResult {
        node_a: iface_a.node_id.clone(),
        node_b: iface_b.node_id.clone(),
        interface_a: iface_a.name.clone(),
        interface_b: iface_b.name.clone(),
        bandwidth_mbps,
        latency_us: 0,
        reachable: client_response.success && bandwidth_mbps > 0.0,
        rdma: iface_a.rdma_available && iface_b.rdma_available,
    };

    // Wyslij SSE event z postepem
    let event_data = serde_json::json!({
        "node_a": result.node_a,
        "node_b": result.node_b,
        "interface_a": result.interface_a,
        "interface_b": result.interface_b,
        "bandwidth_mbps": result.bandwidth_mbps,
        "reachable": result.reachable,
        "rdma": result.rdma,
        "progress": progress,
        "total": total,
    });
    let _ = tx.send(format!("event: probe_result\ndata: {}\n\n", event_data)).await;

    Ok(result)
}
