// =============================================================================
// Plik: api/dashboard/api_clusters.rs
// Opis: CRUD endpointy clusterow mesh — tworzenie, edycja, czlonkostwo nodow.
// =============================================================================

use crate::db::{self, DbPool};
use anyhow::Result;
use serde::{Deserialize, Serialize};

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

    db::repository::add_cluster_member(pool, cluster_id, &req.node_id, role)?;
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
