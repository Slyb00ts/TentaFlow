// =============================================================================
// Plik: routing/cluster_sync.rs
// Opis: Synchronizacja trwalej konfiguracji routingu (klastry + czlonkowie)
//       miedzy nodami mesh. Broadcast snapshotu po mutacji oraz zapis
//       snapshotu otrzymanego od zaufanego peera.
// =============================================================================

use std::sync::Arc;

use tracing::warn;

use crate::db::models::{DbCluster, DbClusterMember};
use crate::db::{self, DbPool};
use crate::mesh::iroh_manager::IrohMeshManager;

/// Pelny snapshot konfiguracji routingu przenoszony w `MESH_MSG_ROUTING_SYNC`
/// (JSON — ten sam format co alias sync). Tylko trwala konfiguracja tworzona
/// przez uzytkownika; dynamiczny stan (online/offline czlonkow) jest liczony
/// lokalnie per node i nie jest synchronizowany.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RoutingSyncPayload {
    pub clusters: Vec<DbCluster>,
    pub members: Vec<DbClusterMember>,
}

/// Broadcastuje aktualny snapshot konfiguracji routingu do zaufanych peerow
/// po udanej mutacji (create/update/delete klastra, add/remove czlonka).
/// Odbiorca tylko zapisuje snapshot lokalnie — nigdy nie re-broadcastuje
/// (anty-petla).
pub fn broadcast_routing_mutation(pool: &DbPool, quic_mesh: &Option<Arc<IrohMeshManager>>) {
    let Some(qm) = quic_mesh else {
        return;
    };
    let snapshot = match build_routing_snapshot(pool) {
        Ok(s) => s,
        Err(e) => {
            warn!("broadcast_routing_mutation: blad budowy snapshotu: {}", e);
            return;
        }
    };
    if let Ok(json) = serde_json::to_vec(&snapshot) {
        let qm = Arc::clone(qm);
        tokio::spawn(async move {
            qm.broadcast_routing_sync(json).await;
        });
    }
}

/// Buduje snapshot konfiguracji routingu z lokalnej bazy.
fn build_routing_snapshot(pool: &DbPool) -> anyhow::Result<RoutingSyncPayload> {
    Ok(RoutingSyncPayload {
        clusters: db::repository::list_clusters(pool)?,
        members: db::repository::list_all_cluster_members(pool)?,
    })
}

/// Zapisuje snapshot konfiguracji routingu otrzymany przez mesh sync do
/// lokalnej bazy. Wolane z pipeline po walidacji zaufania nadawcy.
pub fn apply_routing_sync(pool: &DbPool, payload: RoutingSyncPayload) -> anyhow::Result<()> {
    db::repository::replace_routing_config_from_sync(pool, &payload.clusters, &payload.members)
}
