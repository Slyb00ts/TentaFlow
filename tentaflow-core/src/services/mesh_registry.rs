// ============ File: mesh_registry.rs — In-memory aggregator of services from all known mesh nodes ============

// Cross-node `services` projection. Local node snapshot zywie obok zdalnych —
// supervisor odswiezza go (krok N7.2) z `services_repo::list_all`, zdalne
// snapshoty docieraja przez `MeshServicesGet`/`Announce`/`Update` (krok N3a).
// Czytelnicy uzywaja `visible_services` / `unique_models` dostac globalny
// widok bez jawnego mergeowania DB-list w call sites.

use std::sync::Arc;
use std::time::Instant;

use arc_swap::ArcSwap;
use dashmap::DashMap;
use parking_lot::RwLock;
use tentaflow_protocol::{ServiceChange, ServiceInfo};

/// R3e (F10): callback wywolywany po kazdej mutacji registry. Router
/// rejestruje tu `rebuild_catalog` zeby peer announce/remove/update odswiezyl
/// publiczny katalog w sub-second zamiast czekac na nastepny tick supervisora.
/// `Send + Sync + 'static` bo mutacje moga lecieć z dowolnego watka mesh
/// pipeline'u.
pub type RegistryChangeCallback = Arc<dyn Fn() + Send + Sync>;

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

/// Local node snapshot — populated by the supervisor (krok N7.2) every tick
/// from `services_repo::list_all`. Default is an empty vector with an empty
/// `node_id` so cross-node lookups during boot (before supervisor first tick)
/// return nothing rather than crashing.
#[derive(Debug, Clone, Default)]
pub struct LocalNodeSnapshot {
    pub node_id: String,
    pub services: Vec<ServiceInfo>,
}

/// Unified entry zwracany przez `unique_models`. Sklejony z `ServiceInfo` +
/// `ServiceModelEntry` z konkretnego serwisu, ktory go eksponuje. Kolejnosc
/// pol odpowiada szybkiemu lookup'owi po `model_name`; wlasciciel widoczny
/// w `node_id`/`service_id`.
#[derive(Debug, Clone)]
pub struct UnifiedModelEntry {
    pub model_name: String,
    pub display_name: Option<String>,
    pub service_id: i64,
    pub node_id: String,
    pub engine_id: String,
    pub category: String,
    pub status: String,
    pub transport: String,
    pub endpoint_url: Option<String>,
    pub capabilities: Vec<String>,
    pub context_length: Option<u32>,
    pub quantization: Option<String>,
    pub is_default: bool,
}

/// In-memory aggregator of services advertised by every reachable mesh node
/// plus the local node. Readers (routing, GUI aggregate) operate purely on
/// this struct and never touch the SQLite `services` table directly anymore.
pub struct MeshServicesRegistry {
    /// Local node snapshot. ArcSwap zeby readers byli lock-free; supervisor
    /// publikuje pelen vector na kazdym tick przez `replace_local`.
    local: ArcSwap<LocalNodeSnapshot>,
    /// Per-peer snapshots adwertyzowane przez zaufane nody.
    remote: Arc<DashMap<String, RemoteNodeSnapshot>>,
    /// R3e (F10): callback observer wywolywany po kazdej mutacji. Router
    /// subskrybuje tu `rebuild_catalog` przy `Router::start` zeby peer
    /// announce/remove/update odswiezyl publiczny katalog natychmiast.
    on_change: RwLock<Option<RegistryChangeCallback>>,
}

impl Default for MeshServicesRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl MeshServicesRegistry {
    pub fn new() -> Self {
        Self {
            local: ArcSwap::from_pointee(LocalNodeSnapshot::default()),
            remote: Arc::new(DashMap::new()),
            on_change: RwLock::new(None),
        }
    }

    /// Rejestruje callback wywolywany po kazdej mutacji. Idempotentne — kolejny
    /// `set_on_change` nadpisuje poprzedni; czysci ustawiajac `None`.
    pub fn set_on_change(&self, callback: Option<RegistryChangeCallback>) {
        *self.on_change.write() = callback;
    }

    /// Wywoluje observer, jesli zarejestrowany. Wszystkie mutator-y wolaja
    /// po zmianie. Zostaje cichy gdy callback panicuje — supervisor `tick`
    /// i tak odswiezy katalog w `<5s`.
    fn notify_change(&self) {
        let cb = self.on_change.read().clone();
        if let Some(cb) = cb {
            cb();
        }
    }

    // -------------------------------------------------------------------------
    // Local node
    // -------------------------------------------------------------------------

    /// Replace lokalnego node'a snapshot. Wolany przez supervisor po kazdym
    /// tick'u (full repo dump) oraz przez nawiezujacy snapshot publisher na
    /// startup.
    pub fn replace_local(&self, node_id: String, services: Vec<ServiceInfo>) {
        self.local
            .store(Arc::new(LocalNodeSnapshot { node_id, services }));
        self.notify_change();
    }

    /// Inkrementalna zmiana lokalnego snapshotu (po deploy, pause, delete itp.).
    /// `node_id` musi pasowac do aktualnego `local.node_id` — w przeciwnym
    /// wypadku zmiana jest ignorowana (dispatcher kierowal do remote).
    pub fn apply_local_change(&self, node_id: &str, change: ServiceChange) {
        let current = self.local.load();
        if current.node_id != node_id {
            return;
        }
        let mut services = current.services.clone();
        match change {
            ServiceChange::Added(svc) | ServiceChange::Updated(svc) => {
                if let Some(slot) = services.iter_mut().find(|s| s.id == svc.id) {
                    *slot = svc;
                } else {
                    services.push(svc);
                }
            }
            ServiceChange::Removed { service_id } => {
                services.retain(|s| s.id != service_id);
            }
        }
        self.local.store(Arc::new(LocalNodeSnapshot {
            node_id: current.node_id.clone(),
            services,
        }));
        self.notify_change();
    }

    /// Snapshot lokalnego node'a. Read-only; lock-free dzieki ArcSwap.
    pub fn local(&self) -> Arc<LocalNodeSnapshot> {
        self.local.load_full()
    }

    // -------------------------------------------------------------------------
    // Remote nodes
    // -------------------------------------------------------------------------

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
        self.notify_change();
    }

    /// Apply an incremental change to `node_id`'s snapshot. If `node_id`
    /// is the local node, deleguje na `apply_local_change`. Dla zdalnych
    /// nodow gdy entry nie istnieje (no prior `replace_node`), `Added`/
    /// `Updated` tworzy nowy entry; `Removed` jest no-op. `last_seen_at`
    /// jest bumpowany za kazdym razem kiedy entry sie zmienia.
    pub fn apply_change(&self, node_id: String, change: ServiceChange) {
        if self.local.load().node_id == node_id {
            self.apply_local_change(&node_id, change);
            return;
        }
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
        self.notify_change();
    }

    /// Drop everything we know about `node_id`. Called from the
    /// `PeerDisconnected` handler so a node going offline disappears from the
    /// aggregate immediately instead of lingering until a stale-timer fires.
    pub fn remove_node(&self, node_id: &str) {
        self.remote.remove(node_id);
        self.notify_change();
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

    // -------------------------------------------------------------------------
    // Aggregated views (local + remote)
    // -------------------------------------------------------------------------

    /// Wszystkie services widoczne w mesh — local snapshot + kazdy remote
    /// snapshot — zdedupowane po `(node_id, service_id)`. Kolejnosc:
    /// najpierw local, potem remote w kolejnosci iteratora DashMap.
    pub fn visible_services(&self) -> Vec<ServiceInfo> {
        let mut out: Vec<ServiceInfo> = Vec::new();
        let mut seen: std::collections::HashSet<(String, i64)> = std::collections::HashSet::new();
        for svc in &self.local.load().services {
            if seen.insert((svc.node_id.clone(), svc.id)) {
                out.push(svc.clone());
            }
        }
        for entry in self.remote.iter() {
            for svc in &entry.value().services {
                if seen.insert((svc.node_id.clone(), svc.id)) {
                    out.push(svc.clone());
                }
            }
        }
        out
    }

    /// Lista unikalnych modeli dostepnych w mesh, zgrupowana po `model_name`.
    /// Pierwszy znaleziony serwis (local-first) wygrywa kiedy dwa nody
    /// publikuja ten sam `model_name`. Uzywane przez routing (krok N7.3) i
    /// GUI catalog do wystawienia spojnej listy modeli.
    pub fn unique_models(&self) -> Vec<UnifiedModelEntry> {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out: Vec<UnifiedModelEntry> = Vec::new();
        for svc in self.visible_services() {
            for model in &svc.models {
                if seen.insert(model.model_name.clone()) {
                    out.push(UnifiedModelEntry {
                        model_name: model.model_name.clone(),
                        display_name: model.display_name.clone(),
                        service_id: svc.id,
                        node_id: svc.node_id.clone(),
                        engine_id: svc.engine_id.clone(),
                        category: svc.category.clone(),
                        status: svc.status.clone(),
                        transport: svc.transport.clone(),
                        endpoint_url: svc.endpoint_url.clone(),
                        capabilities: model.capabilities.clone(),
                        context_length: model.context_length,
                        quantization: model.quantization.clone(),
                        is_default: model.is_default,
                    });
                }
            }
        }
        out
    }

    /// Znajdz wlasciciela serwisu eksponujacego `model_name`. Zwraca pierwszy
    /// dopasowany `node_id` (local-first przez `visible_services` ordering).
    pub fn find_node_for_model(&self, model_name: &str) -> Option<String> {
        self.visible_services()
            .into_iter()
            .find(|s| s.models.iter().any(|m| m.model_name == model_name))
            .map(|s| s.node_id)
    }

    /// Konkretny `ServiceInfo` po `(node_id, service_id)`. Local probowany
    /// jako pierwszy, potem zdalne snapshoty. Zwraca `None` kiedy nic nie
    /// pasuje.
    pub fn find_service(&self, node_id: &str, service_id: i64) -> Option<ServiceInfo> {
        let local = self.local.load();
        if local.node_id == node_id {
            if let Some(s) = local.services.iter().find(|s| s.id == service_id) {
                return Some(s.clone());
            }
        }
        let remote = self.remote.get(node_id)?;
        remote.services.iter().find(|s| s.id == service_id).cloned()
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
            progress_message: None,
            models: Vec::new(),
            created_at: "2026-01-01 00:00:00".into(),
            updated_at: "2026-01-01 00:00:00".into(),
            request_time_parameters: Default::default(),
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

    // -------------------------------------------------------------------------
    // Local + aggregated (krok N7.1b)
    // -------------------------------------------------------------------------

    fn svc_with_model(id: i64, node: &str, name: &str, model: &str) -> ServiceInfo {
        let mut s = svc(id, node, name);
        s.models.push(tentaflow_protocol::ServiceModelEntry {
            model_name: model.to_string(),
            display_name: None,
            capabilities: Vec::new(),
            context_length: None,
            quantization: None,
            is_default: false,
        });
        s
    }

    #[test]
    fn replace_local_replaces_local_snapshot() {
        let reg = MeshServicesRegistry::new();
        assert!(reg.local().services.is_empty());
        reg.replace_local("local".into(), vec![svc(1, "local", "first")]);
        let snap = reg.local();
        assert_eq!(snap.node_id, "local");
        assert_eq!(snap.services.len(), 1);
        assert_eq!(snap.services[0].id, 1);

        reg.replace_local(
            "local".into(),
            vec![svc(2, "local", "second"), svc(3, "local", "third")],
        );
        assert_eq!(reg.local().services.len(), 2);
    }

    #[test]
    fn apply_local_change_routes_through_local_when_node_matches() {
        let reg = MeshServicesRegistry::new();
        reg.replace_local("local".into(), vec![svc(1, "local", "old")]);
        // Updated z apply_change powinno trafic do local, nie remote.
        let mut updated = svc(1, "local", "renamed");
        updated.status = "stopped".into();
        reg.apply_change("local".into(), ServiceChange::Updated(updated));
        let snap = reg.local();
        assert_eq!(snap.services[0].display_name, "renamed");
        assert_eq!(snap.services[0].status, "stopped");
        assert_eq!(reg.remote_node_count(), 0);
    }

    #[test]
    fn visible_services_includes_local_and_remote() {
        let reg = MeshServicesRegistry::new();
        reg.replace_local("local".into(), vec![svc(1, "local", "L1")]);
        reg.replace_node("peerA".into(), vec![svc(2, "peerA", "A1")]);
        let v = reg.visible_services();
        assert_eq!(v.len(), 2);
        assert!(v.iter().any(|s| s.node_id == "local" && s.id == 1));
        assert!(v.iter().any(|s| s.node_id == "peerA" && s.id == 2));
    }

    #[test]
    fn visible_services_dedups_by_node_id_service_id() {
        let reg = MeshServicesRegistry::new();
        // Wstawiamy ten sam (node_id, id) lokalnie i zdalnie — local wygrywa.
        reg.replace_local("nodeX".into(), vec![svc(7, "nodeX", "from-local")]);
        reg.replace_node("nodeX".into(), vec![svc(7, "nodeX", "from-remote")]);
        let v = reg.visible_services();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].display_name, "from-local");
    }

    #[test]
    fn unique_models_first_service_wins_for_duplicates() {
        let reg = MeshServicesRegistry::new();
        reg.replace_local(
            "local".into(),
            vec![svc_with_model(1, "local", "L1", "qwen-tiny")],
        );
        // Drugi node oferuje ten sam model — powinien byc pominiety.
        reg.replace_node(
            "peerA".into(),
            vec![svc_with_model(2, "peerA", "A1", "qwen-tiny")],
        );
        let models = reg.unique_models();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model_name, "qwen-tiny");
        assert_eq!(models[0].node_id, "local");
        assert_eq!(models[0].service_id, 1);
    }

    #[test]
    fn find_node_for_model_returns_correct_owner() {
        let reg = MeshServicesRegistry::new();
        reg.replace_local(
            "local".into(),
            vec![svc_with_model(1, "local", "L1", "model-a")],
        );
        reg.replace_node(
            "peerA".into(),
            vec![svc_with_model(11, "peerA", "A1", "model-b")],
        );
        assert_eq!(reg.find_node_for_model("model-a").as_deref(), Some("local"));
        assert_eq!(reg.find_node_for_model("model-b").as_deref(), Some("peerA"));
        assert!(reg.find_node_for_model("nope").is_none());
    }

    #[test]
    fn find_service_returns_correct_entry() {
        let reg = MeshServicesRegistry::new();
        reg.replace_local("local".into(), vec![svc(5, "local", "loc-5")]);
        reg.replace_node("peerA".into(), vec![svc(6, "peerA", "rem-6")]);

        let s_local = reg.find_service("local", 5).expect("local present");
        assert_eq!(s_local.display_name, "loc-5");
        let s_remote = reg.find_service("peerA", 6).expect("remote present");
        assert_eq!(s_remote.display_name, "rem-6");
        assert!(reg.find_service("local", 999).is_none());
        assert!(reg.find_service("ghost", 1).is_none());
    }

    /// R3e (F10): kazda mutacja registry wywoluje on_change callback.
    /// Router subskrybuje tu `rebuild_catalog`; dla testu uzywamy
    /// `AtomicUsize` zeby policzyc wywolania.
    #[test]
    fn on_change_fires_for_each_mutation() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let reg = Arc::new(MeshServicesRegistry::new());
        let counter = Arc::new(AtomicUsize::new(0));
        let counter_for_cb = counter.clone();
        reg.set_on_change(Some(Arc::new(move || {
            counter_for_cb.fetch_add(1, Ordering::SeqCst);
        })));

        // Mutator 1: replace_local
        reg.replace_local("local".into(), vec![svc(1, "local", "s1")]);
        assert_eq!(counter.load(Ordering::SeqCst), 1);

        // Mutator 2: apply_local_change (Added)
        reg.apply_local_change("local", ServiceChange::Added(svc(2, "local", "s2")));
        assert_eq!(counter.load(Ordering::SeqCst), 2);

        // Mutator 3: replace_node (peer announce / get response)
        reg.replace_node("peerA".into(), vec![svc(10, "peerA", "remote-1")]);
        assert_eq!(counter.load(Ordering::SeqCst), 3);

        // Mutator 4: apply_change (peer update)
        reg.apply_change(
            "peerA".into(),
            ServiceChange::Added(svc(11, "peerA", "remote-2")),
        );
        assert_eq!(counter.load(Ordering::SeqCst), 4);

        // Mutator 5: remove_node (peer disconnect)
        reg.remove_node("peerA");
        assert_eq!(counter.load(Ordering::SeqCst), 5);
    }

    /// Bez observera registry pracuje normalnie — observer to opt-in.
    #[test]
    fn on_change_unset_is_noop() {
        let reg = MeshServicesRegistry::new();
        reg.set_on_change(None);
        // Te wszystkie mutator-y nie powinny panicowac przy braku callback.
        reg.replace_local("local".into(), vec![svc(1, "local", "s1")]);
        reg.replace_node("peerA".into(), vec![svc(10, "peerA", "remote-1")]);
        reg.remove_node("peerA");
        // Sanity — registry mial dzialac jak zwykle.
        assert_eq!(reg.local().services.len(), 1);
    }
}
