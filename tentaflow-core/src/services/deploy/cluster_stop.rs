// ===== File: services/deploy/cluster_stop.rs — stopping a whole distributed deployment =====
//
// A distributed (multi-node tensor-parallel) deployment has ONE record — the
// `cluster_deployments` row, its member list and two leased ports — and that
// record lives only on the node that coordinated the deploy. Every member node
// holds its own service row and container. `stop_held` is the coordinator's
// teardown; `coordinator_candidates` / `member_nodes` let a dashboard on any
// other node find the coordinator (or, when no node holds a record any more,
// the members) from what the mesh snapshots advertise.

use std::collections::HashSet;
use std::sync::Arc;

use tentaflow_protocol::mesh::MeshCommandType;
use tentaflow_protocol::{ClusterDeployMemberStatus, ClusterDeployStopResponse, ServiceInfo};
use tracing::warn;

use crate::db::DbPool;
use crate::mesh::iroh_manager::IrohMeshManager;
use crate::services::ports::PortAllocator;

/// How long the coordinator waits for one member to remove its container and
/// row. `docker stop` alone may use its 10 s grace before the forced remove.
pub const MEMBER_STOP_TIMEOUT_SECS: u64 = 60;

/// How long a non-coordinator waits for the coordinator's whole teardown. The
/// coordinator stops members one after another, each bounded by
/// `MEMBER_STOP_TIMEOUT_SECS`; the dashboard itself waits 180 s.
pub const COORDINATOR_STOP_TIMEOUT_SECS: u64 = 150;

/// What a node needs to tear down its share of a deployment and reach the
/// other members.
pub struct ClusterStopDeps<'a> {
    pub db: &'a DbPool,
    pub ports: Arc<PortAllocator>,
    pub mesh: &'a Arc<IrohMeshManager>,
    pub local_node_id: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub enum StopHeldError {
    /// This node holds no record of the deployment — it is not the coordinator.
    NotHeld,
    /// The request named a cluster the deployment does not belong to.
    ClusterMismatch,
    Db(String),
}

/// Removes this node's containers and service rows of the deployment and
/// announces the removed rows to the mesh. An empty result means nothing of
/// the deployment is left here; errors mean a container may still run.
pub async fn stop_local_member(
    deps: &ClusterStopDeps<'_>,
    deployment_cluster_id: &str,
) -> Vec<String> {
    super::distributed::forget_head_spec(deployment_cluster_id);
    let (removed, errors) =
        super::distributed::stop_distributed(deps.db, deps.ports.clone(), deployment_cluster_id)
            .await;
    announce_removed(deps.mesh, deps.local_node_id, &removed).await;
    errors
}

async fn announce_removed(mesh: &Arc<IrohMeshManager>, local_node_id: &str, service_ids: &[i64]) {
    for &service_id in service_ids {
        let payload = tentaflow_protocol::mesh::MeshServicesUpdatePayload {
            from_node_id: local_node_id.to_string(),
            change: tentaflow_protocol::ServiceChange::Removed { service_id },
        };
        match crate::mesh::cbor::encode(&payload) {
            Ok(bytes) => {
                let _ = mesh
                    .broadcast_ufp2_to_trusted(
                        tentaflow_protocol::mesh::MESH_MSG_SERVICES_UPDATE,
                        &bytes,
                        None,
                    )
                    .await;
            }
            Err(e) => warn!(error = %e, service_id, "cluster stop: removal announce encode failed"),
        }
    }
}

/// Tears down every member node of the deployment — this node directly, the
/// others through `ServiceStopDistributed` — and reports one status per node.
/// `hostname` is left empty: only the node answering the dashboard knows the
/// names of its peers.
pub async fn teardown_members(
    deps: &ClusterStopDeps<'_>,
    deployment_cluster_id: &str,
    members: &[(String, String)],
) -> Vec<ClusterDeployMemberStatus> {
    let mut statuses = Vec::with_capacity(members.len());
    let mut seen = HashSet::new();
    for (node_id, role) in members {
        // One container per node, however many member rows name it.
        if !seen.insert(node_id.clone()) {
            continue;
        }
        let error = if node_id == deps.local_node_id {
            let errors = stop_local_member(deps, deployment_cluster_id).await;
            (!errors.is_empty()).then(|| errors.join("; "))
        } else {
            let cmd = MeshCommandType::ServiceStopDistributed {
                deployment_cluster_id: deployment_cluster_id.to_string(),
            };
            match deps
                .mesh
                .send_command_and_wait(node_id, cmd, MEMBER_STOP_TIMEOUT_SECS)
                .await
            {
                Ok(resp) if resp.ok => None,
                Ok(resp) => Some(resp.error.unwrap_or_else(|| "stop nieudany".to_string())),
                Err(e) => Some(format!("mesh send nieudany: {e}")),
            }
        };
        statuses.push(ClusterDeployMemberStatus {
            node_id: node_id.clone(),
            hostname: String::new(),
            role: role.clone(),
            ok: error.is_none(),
            deploy_id: None,
            error,
        });
    }
    statuses
}

/// The coordinator's teardown of a deployment whose record THIS node holds:
/// every member is stopped, and only when all of them are gone are the leased
/// ports released and the record deleted. A partial teardown keeps the record
/// with status `failed`, so the stop can be retried and no Ray container is
/// left running untracked. A non-empty `cluster_id` must match the record.
pub async fn stop_held(
    deps: &ClusterStopDeps<'_>,
    deployment_cluster_id: &str,
    cluster_id: &str,
) -> Result<ClusterDeployStopResponse, StopHeldError> {
    let dep = crate::db::repository::get_cluster_deployment(deps.db, deployment_cluster_id)
        .map_err(|e| StopHeldError::Db(e.to_string()))?
        .ok_or(StopHeldError::NotHeld)?;
    if !cluster_id.is_empty() && dep.cluster_id != cluster_id {
        return Err(StopHeldError::ClusterMismatch);
    }
    let members: Vec<(String, String)> =
        crate::db::repository::list_cluster_deployment_members(deps.db, deployment_cluster_id)
            .map_err(|e| StopHeldError::Db(e.to_string()))?
            .into_iter()
            .map(|m| (m.node_id, m.role))
            .collect();

    let statuses = teardown_members(deps, deployment_cluster_id, &members).await;
    let all_ok = statuses.iter().all(|s| s.ok);
    if all_ok {
        // The serve and torch.distributed leases were taken on THIS node at
        // deploy time, never on the workers, and `deploy::stop` never frees
        // them. dist_port == 0 marks a row that predates its allocation.
        let _ = deps.ports.release(dep.port as u16);
        if dep.dist_port > 0 {
            let _ = deps.ports.release(dep.dist_port as u16);
        }
        if let Err(e) =
            crate::db::repository::delete_cluster_deployment(deps.db, deployment_cluster_id)
        {
            warn!(error = %e, deployment_cluster_id, "cluster stop: record delete failed");
        }
    } else {
        let _ = crate::db::repository::set_cluster_deployment_status(
            deps.db,
            deployment_cluster_id,
            "failed",
        );
    }

    let _ = crate::db::repository::log_audit(
        deps.db,
        None,
        None,
        "cluster.deploy_stop",
        Some(&format!(
            "cluster:{} dep:{}",
            dep.cluster_id, deployment_cluster_id
        )),
        Some(if all_ok { "ok" } else { "partial" }),
        None,
        Some(deps.local_node_id),
    );

    Ok(ClusterDeployStopResponse {
        ok: all_ok,
        members: statuses,
        message: (!all_ok)
            .then(|| "teardown niekompletny — rekord zachowany, ponów STOP".to_string()),
    })
}

/// Remote nodes that may hold the record of `deployment_cluster_id`, most
/// likely first: the coordinators member rows name, then every node carrying
/// a member row — members deployed before the coordinator was recorded name
/// none, and such a deployment was normally started from one of its members.
/// The local node is left out; the caller has already read its own database.
pub fn coordinator_candidates(
    services: &[ServiceInfo],
    deployment_cluster_id: &str,
    local_node_id: &str,
) -> Vec<String> {
    let rows = || {
        services
            .iter()
            .filter(|s| s.cluster_deployment_id == deployment_cluster_id)
    };
    let named = rows()
        .map(|s| s.cluster_coordinator_node_id.clone())
        .filter(|n| !n.is_empty());
    let carriers = rows().map(|s| s.node_id.clone());
    let mut seen = HashSet::new();
    named
        .chain(carriers)
        .filter(|n| n != local_node_id && seen.insert(n.clone()))
        .collect()
}

/// Every node (local included) advertising a member row of the deployment,
/// with the member's role from its row. Used only when no node holds the
/// record any more, to remove what the members still run.
pub fn member_nodes(
    services: &[ServiceInfo],
    deployment_cluster_id: &str,
) -> Vec<(String, String)> {
    let mut seen = HashSet::new();
    services
        .iter()
        .filter(|s| s.cluster_deployment_id == deployment_cluster_id)
        .filter(|s| seen.insert(s.node_id.clone()))
        .map(|s| (s.node_id.clone(), String::new()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(node: &str, id: i64, dep: &str, coordinator: &str) -> ServiceInfo {
        ServiceInfo {
            id,
            node_id: node.to_string(),
            engine_id: "vllm-spark".to_string(),
            category: "llm".to_string(),
            display_name: "GLM".to_string(),
            deploy_method: "docker".to_string(),
            transport: "http_direct".to_string(),
            status: "running".to_string(),
            pinned: false,
            paused: false,
            runtime_pid: None,
            runtime_port: Some(8100),
            sidecar_quic_port: None,
            endpoint_url: None,
            restart_count: 0,
            health_last_err: None,
            active_deploy_id: String::new(),
            last_deploy_id: String::new(),
            deployment_progress_pct: 100,
            progress_message: None,
            usage_json: None,
            usage_updated_at: None,
            models: Vec::new(),
            update_available: false,
            created_at: "2026-01-01 00:00:00".into(),
            updated_at: "2026-01-01 00:00:00".into(),
            request_time_parameters: Default::default(),
            gpu_selection: String::new(),
            cluster_deployment_id: dep.to_string(),
            cluster_coordinator_node_id: coordinator.to_string(),
        }
    }

    #[test]
    fn recorded_coordinator_is_asked_first_even_when_it_runs_no_member() {
        let services = vec![
            member("rig24", 1, "dep-a", "laptop"),
            member("rig25", 2, "dep-a", "laptop"),
        ];
        assert_eq!(
            coordinator_candidates(&services, "dep-a", "rig25"),
            vec!["laptop".to_string(), "rig24".to_string()]
        );
    }

    #[test]
    fn legacy_members_without_a_coordinator_offer_their_own_nodes() {
        let services = vec![
            member("rig24", 1, "dep-a", ""),
            member("rig25", 2, "dep-a", ""),
            member("rig26", 3, "dep-other", ""),
        ];
        assert_eq!(
            coordinator_candidates(&services, "dep-a", "laptop"),
            vec!["rig24".to_string(), "rig25".to_string()]
        );
    }

    #[test]
    fn the_local_node_and_duplicates_are_never_candidates() {
        let services = vec![
            member("rig24", 1, "dep-a", "rig24"),
            member("rig24", 2, "dep-a", "rig24"),
            member("rig25", 3, "dep-a", "rig24"),
        ];
        assert_eq!(
            coordinator_candidates(&services, "dep-a", "rig24"),
            vec!["rig25".to_string()]
        );
    }

    #[test]
    fn member_nodes_lists_each_carrier_once_including_the_local_node() {
        let services = vec![
            member("rig24", 1, "dep-a", ""),
            member("rig24", 2, "dep-a", ""),
            member("rig25", 3, "dep-a", ""),
            member("rig26", 4, "", ""),
        ];
        let nodes: Vec<String> = member_nodes(&services, "dep-a")
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(nodes, vec!["rig24".to_string(), "rig25".to_string()]);
    }

    const LOCAL: &str = "node-local";
    const DEP: &str = "dep-glm";

    fn fresh_db() -> DbPool {
        crate::db::init(std::path::Path::new(":memory:")).expect("test db")
    }

    /// A deployment coordinated by THIS node with a single member, also this
    /// node, plus that member's service row — the shape a finished cluster
    /// deploy leaves behind. Returns the allocator holding the two leases.
    fn seed_held_deployment(db: &DbPool, serve_port: u16) -> Arc<PortAllocator> {
        let ports = Arc::new(
            PortAllocator::new((serve_port, serve_port + 1), HashSet::new()).expect("allocator"),
        );
        ports.reserve(serve_port).unwrap();
        ports.reserve(serve_port + 1).unwrap();
        crate::db::repository::upsert_cluster_deployment(
            db,
            &crate::db::models::DbClusterDeployment {
                deployment_cluster_id: DEP.to_string(),
                cluster_id: "cluster-a".to_string(),
                engine_id: "vllm-spark".to_string(),
                model: "glm".to_string(),
                served_model_name: "glm".to_string(),
                tp_size: 2,
                head_node_id: LOCAL.to_string(),
                port: i64::from(serve_port),
                dist_port: i64::from(serve_port + 1),
                endpoint_url: None,
                status: "running".to_string(),
                created_at: String::new(),
                updated_at: String::new(),
            },
            &[crate::db::models::DbClusterDeploymentMember {
                deployment_cluster_id: DEP.to_string(),
                node_id: LOCAL.to_string(),
                role: "head".to_string(),
                container_name: "tentaflow-vllm-spark-0".to_string(),
            }],
        )
        .expect("seed deployment");
        let conn = db.write().expect("db write");
        conn.execute(
            "INSERT INTO services (engine_id, category, display_name, deploy_method, transport, config_json) \
             VALUES ('vllm-spark', 'llm', 'GLM', 'docker', 'http_direct', ?1)",
            rusqlite::params![format!(
                r#"{{"_distributed":{{"deployment_cluster_id":"{DEP}","coordinator_node_id":"{LOCAL}"}}}}"#
            )],
        )
        .expect("seed member row");
        ports
    }

    fn member_rows(db: &DbPool) -> usize {
        let conn = db.read().expect("db read");
        crate::services_repo::services::list_all(&conn)
            .expect("list services")
            .into_iter()
            .filter(|s| s.config_json.contains(DEP))
            .count()
    }

    #[tokio::test]
    async fn a_node_without_the_record_reports_not_held_and_touches_nothing() {
        let db = fresh_db();
        let ports = Arc::new(PortAllocator::new((47_300, 47_301), HashSet::new()).unwrap());
        let mesh = crate::bus::replication::test_support::make_test_mesh_manager().await;
        let deps = ClusterStopDeps {
            db: &db,
            ports,
            mesh: &mesh,
            local_node_id: LOCAL,
        };
        assert_eq!(stop_held(&deps, DEP, "").await.unwrap_err(), StopHeldError::NotHeld);
    }

    #[tokio::test]
    async fn a_named_cluster_that_does_not_own_the_deployment_is_refused() {
        let db = fresh_db();
        let ports = seed_held_deployment(&db, 47_310);
        let mesh = crate::bus::replication::test_support::make_test_mesh_manager().await;
        let deps = ClusterStopDeps {
            db: &db,
            ports: ports.clone(),
            mesh: &mesh,
            local_node_id: LOCAL,
        };
        assert_eq!(
            stop_held(&deps, DEP, "cluster-b").await.unwrap_err(),
            StopHeldError::ClusterMismatch
        );
        assert!(crate::db::repository::get_cluster_deployment(&db, DEP)
            .unwrap()
            .is_some());
        assert_eq!(member_rows(&db), 1);
        assert_eq!(ports.acquire().ok(), None, "leases stay taken");
    }

    #[tokio::test]
    async fn the_coordinator_removes_members_record_and_both_leases() {
        let db = fresh_db();
        let ports = seed_held_deployment(&db, 47_320);
        let mesh = crate::bus::replication::test_support::make_test_mesh_manager().await;
        let deps = ClusterStopDeps {
            db: &db,
            ports: ports.clone(),
            mesh: &mesh,
            local_node_id: LOCAL,
        };
        let resp = stop_held(&deps, DEP, "cluster-a").await.expect("held");
        assert!(resp.ok, "{resp:?}");
        assert_eq!(resp.members.len(), 1);
        assert_eq!(resp.members[0].node_id, LOCAL);
        assert_eq!(resp.members[0].role, "head");
        assert!(crate::db::repository::get_cluster_deployment(&db, DEP)
            .unwrap()
            .is_none());
        assert_eq!(member_rows(&db), 0);
        let mut freed = ports.acquire_many(2).expect("serve and dist ports released");
        freed.sort_unstable();
        assert_eq!(freed, vec![47_320, 47_321]);
    }

    #[tokio::test]
    async fn an_unreachable_member_keeps_the_record_as_failed_and_the_leases() {
        let db = fresh_db();
        let ports = seed_held_deployment(&db, 47_330);
        {
            let conn = db.write().unwrap();
            conn.execute(
                "INSERT INTO cluster_deployment_members (deployment_cluster_id, node_id, role, container_name) \
                 VALUES (?1, ?2, 'worker', 'c')",
                rusqlite::params![DEP, "f".repeat(64)],
            )
            .unwrap();
        }
        let mesh = crate::bus::replication::test_support::make_test_mesh_manager().await;
        let deps = ClusterStopDeps {
            db: &db,
            ports: ports.clone(),
            mesh: &mesh,
            local_node_id: LOCAL,
        };
        let resp = stop_held(&deps, "dep-glm", "").await.expect("held");
        assert!(!resp.ok);
        let worker = resp.members.iter().find(|m| m.role == "worker").unwrap();
        assert!(!worker.ok);
        assert!(worker.error.is_some());
        let record = crate::db::repository::get_cluster_deployment(&db, DEP)
            .unwrap()
            .expect("record kept for a retry");
        assert_eq!(record.status, "failed");
        assert_eq!(ports.acquire().ok(), None, "leases stay taken");
    }
}
