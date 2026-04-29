// ============ File: mesh_registry.rs — In-memory aggregator of services from all known mesh nodes ============

// Cross-node `services` projection. The local node persists its rows in the
// SQLite `services` table; remote nodes' snapshots arrive via
// `MeshServicesGet`/`Announce`/`Update` messages and end up here. The local
// node is intentionally NOT inserted — readers (GUI aggregate, forwarding
// lookup) merge `services_repo::list_all` with `all_remote()` to get a global
// view.

use std::sync::Arc;
use std::time::Instant;

use dashmap::DashMap;
use tentaflow_protocol::{ServiceChange, ServiceInfo};

/// Snapshot kept per remote node: the last full vector of `ServiceInfo`
/// received and the wallclock instant we received it. `last_seen_at` is used
/// only for diagnostics — eviction is driven by explicit `remove_node` calls
/// from the disconnect handler, so the registry never quietly drops a
/// reachable peer's services because of a stale timer.
#[derive(Debug, Clone)]
pub struct RemoteNodeSnapshot {
    pub services: Vec<ServiceInfo>,
    pub last_seen_at: Instant,
}

/// In-memory aggregator of services advertised by every reachable remote
/// mesh node. The local node is NOT here; readers union with the local
/// `services` table to produce a full mesh view.
pub struct MeshServicesRegistry {
    remote: Arc<DashMap<String, RemoteNodeSnapshot>>,
}

impl Default for MeshServicesRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl MeshServicesRegistry {
    pub fn new() -> Self {
        Self {
            remote: Arc::new(DashMap::new()),
        }
    }

    /// Replace the snapshot stored for `node_id`. Used for full-state messages
    /// (`MeshServicesGetResponse`, `MeshServicesAnnounce`). Bumps `last_seen_at`.
    pub fn replace_node(&self, node_id: String, services: Vec<ServiceInfo>) {
        self.remote.insert(
            node_id,
            RemoteNodeSnapshot {
                services,
                last_seen_at: Instant::now(),
            },
        );
    }

    /// Apply an incremental change to `node_id`'s snapshot. If the node is not
    /// known yet (no prior `replace_node`), `Added`/`Updated` create a new
    /// entry; `Removed` is a no-op. `last_seen_at` is bumped in every case
    /// where the entry actually changes.
    pub fn apply_change(&self, node_id: String, change: ServiceChange) {
        match change {
            ServiceChange::Added(svc) => {
                let mut entry = self
                    .remote
                    .entry(node_id)
                    .or_insert_with(|| RemoteNodeSnapshot {
                        services: Vec::new(),
                        last_seen_at: Instant::now(),
                    });
                // Replace if same id already there (idempotent re-deliver),
                // else append.
                if let Some(slot) = entry.services.iter_mut().find(|s| s.id == svc.id) {
                    *slot = svc;
                } else {
                    entry.services.push(svc);
                }
                entry.last_seen_at = Instant::now();
            }
            ServiceChange::Updated(svc) => {
                let mut entry = self
                    .remote
                    .entry(node_id)
                    .or_insert_with(|| RemoteNodeSnapshot {
                        services: Vec::new(),
                        last_seen_at: Instant::now(),
                    });
                if let Some(slot) = entry.services.iter_mut().find(|s| s.id == svc.id) {
                    *slot = svc;
                } else {
                    entry.services.push(svc);
                }
                entry.last_seen_at = Instant::now();
            }
            ServiceChange::Removed { service_id } => {
                if let Some(mut entry) = self.remote.get_mut(&node_id) {
                    let before = entry.services.len();
                    entry.services.retain(|s| s.id != service_id);
                    if entry.services.len() != before {
                        entry.last_seen_at = Instant::now();
                    }
                }
            }
        }
    }

    /// Drop everything we know about `node_id`. Called from the
    /// `PeerDisconnected` handler so a node going offline disappears from the
    /// aggregate immediately instead of lingering until a stale-timer fires.
    pub fn remove_node(&self, node_id: &str) {
        self.remote.remove(node_id);
    }

    /// Snapshot of all remote nodes' services. Used by the GUI aggregate
    /// (krok N3b) and by tests.
    pub fn all_remote(&self) -> Vec<(String, Vec<ServiceInfo>)> {
        self.remote
            .iter()
            .map(|e| (e.key().clone(), e.value().services.clone()))
            .collect()
    }

    /// Snapshot for a single node, when known. Used by cross-node action
    /// forwarding (krok N3b) to find which peer owns a given service id.
    pub fn for_node(&self, node_id: &str) -> Option<RemoteNodeSnapshot> {
        self.remote.get(node_id).map(|e| e.value().clone())
    }

    /// Find which remote node currently advertises `service_id`. Returns the
    /// first match — within one mesh `(node_id, service_id)` is unique because
    /// the id comes from the owning node's local SQLite rowid.
    pub fn find_node_for_service(&self, service_id: i64) -> Option<String> {
        for entry in self.remote.iter() {
            if entry.value().services.iter().any(|s| s.id == service_id) {
                return Some(entry.key().clone());
            }
        }
        None
    }

    /// Number of remote nodes currently in the registry. Diagnostics only.
    pub fn remote_node_count(&self) -> usize {
        self.remote.len()
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_protocol::{ServiceChange, ServiceInfo};

    fn svc(id: i64, node_id: &str, name: &str) -> ServiceInfo {
        ServiceInfo {
            id,
            node_id: node_id.to_string(),
            engine_id: "vllm".to_string(),
            category: "llm".to_string(),
            display_name: name.to_string(),
            deploy_method: "docker".to_string(),
            transport: "http_direct".to_string(),
            status: "running".to_string(),
            pinned: false,
            paused: false,
            runtime_pid: None,
            runtime_port: Some(8000),
            sidecar_quic_port: None,
            endpoint_url: Some("http://127.0.0.1:8000".into()),
            restart_count: 0,
            health_last_err: None,
            models: Vec::new(),
            created_at: "2026-01-01 00:00:00".into(),
            updated_at: "2026-01-01 00:00:00".into(),
        }
    }

    #[test]
    fn replace_node_inserts_and_updates_last_seen() {
        let reg = MeshServicesRegistry::new();
        reg.replace_node("nodeA".into(), vec![svc(1, "nodeA", "first")]);
        let snap = reg.for_node("nodeA").expect("node present");
        assert_eq!(snap.services.len(), 1);
        assert_eq!(snap.services[0].id, 1);

        let ts0 = snap.last_seen_at;
        std::thread::sleep(std::time::Duration::from_millis(2));
        reg.replace_node(
            "nodeA".into(),
            vec![svc(2, "nodeA", "second"), svc(3, "nodeA", "third")],
        );
        let snap2 = reg.for_node("nodeA").expect("node still present");
        assert_eq!(snap2.services.len(), 2);
        assert!(snap2.last_seen_at > ts0);
    }

    #[test]
    fn apply_change_added_appends() {
        let reg = MeshServicesRegistry::new();
        reg.replace_node("nodeA".into(), vec![svc(1, "nodeA", "one")]);
        reg.apply_change("nodeA".into(), ServiceChange::Added(svc(2, "nodeA", "two")));
        let snap = reg.for_node("nodeA").unwrap();
        let ids: Vec<i64> = snap.services.iter().map(|s| s.id).collect();
        assert_eq!(ids, vec![1, 2]);
    }

    #[test]
    fn apply_change_updated_replaces_by_id() {
        let reg = MeshServicesRegistry::new();
        reg.replace_node(
            "nodeA".into(),
            vec![svc(1, "nodeA", "old"), svc(2, "nodeA", "two")],
        );
        let mut updated = svc(1, "nodeA", "new-name");
        updated.status = "stopped".to_string();
        reg.apply_change("nodeA".into(), ServiceChange::Updated(updated));

        let snap = reg.for_node("nodeA").unwrap();
        assert_eq!(snap.services.len(), 2);
        let s1 = snap.services.iter().find(|s| s.id == 1).unwrap();
        assert_eq!(s1.display_name, "new-name");
        assert_eq!(s1.status, "stopped");
    }

    #[test]
    fn apply_change_removed_filters_by_id() {
        let reg = MeshServicesRegistry::new();
        reg.replace_node(
            "nodeA".into(),
            vec![svc(1, "nodeA", "one"), svc(2, "nodeA", "two")],
        );
        reg.apply_change("nodeA".into(), ServiceChange::Removed { service_id: 1 });
        let snap = reg.for_node("nodeA").unwrap();
        assert_eq!(snap.services.len(), 1);
        assert_eq!(snap.services[0].id, 2);
    }

    #[test]
    fn remove_node_drops_entry() {
        let reg = MeshServicesRegistry::new();
        reg.replace_node("nodeA".into(), vec![svc(1, "nodeA", "x")]);
        reg.replace_node("nodeB".into(), vec![svc(7, "nodeB", "y")]);
        reg.remove_node("nodeA");
        assert!(reg.for_node("nodeA").is_none());
        assert!(reg.for_node("nodeB").is_some());
        assert_eq!(reg.remote_node_count(), 1);
    }

    #[test]
    fn find_node_for_service_returns_correct_node() {
        let reg = MeshServicesRegistry::new();
        reg.replace_node("nodeA".into(), vec![svc(11, "nodeA", "a1")]);
        reg.replace_node(
            "nodeB".into(),
            vec![svc(22, "nodeB", "b1"), svc(23, "nodeB", "b2")],
        );
        assert_eq!(reg.find_node_for_service(11).as_deref(), Some("nodeA"));
        assert_eq!(reg.find_node_for_service(23).as_deref(), Some("nodeB"));
        assert!(reg.find_node_for_service(999).is_none());
    }

    #[test]
    fn apply_change_added_to_unknown_node_creates_entry() {
        let reg = MeshServicesRegistry::new();
        reg.apply_change(
            "nodeNew".into(),
            ServiceChange::Added(svc(5, "nodeNew", "fresh")),
        );
        let snap = reg.for_node("nodeNew").expect("entry created");
        assert_eq!(snap.services.len(), 1);
        assert_eq!(snap.services[0].id, 5);
    }

    #[test]
    fn all_remote_returns_every_known_node() {
        let reg = MeshServicesRegistry::new();
        reg.replace_node("nodeA".into(), vec![svc(1, "nodeA", "a")]);
        reg.replace_node("nodeB".into(), vec![svc(2, "nodeB", "b")]);
        let mut all = reg.all_remote();
        all.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].0, "nodeA");
        assert_eq!(all[1].0, "nodeB");
    }
}
