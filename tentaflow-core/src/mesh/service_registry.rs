// =============================================================================
// Plik: mesh/service_registry.rs
// Opis: In-memory registry serwisow ze wszystkich nodow mesh. Przechowuje
//       lokalne i zdalne serwisy, topologie polaczen. Obsluguje dedup
//       serwisow i routing multi-hop (BFS).
// =============================================================================

use std::collections::{HashMap, HashSet, VecDeque};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use tentaflow_protocol::mesh::MeshServiceInfo;

// =============================================================================
// Zunifikowany model — deduplikacja po nazwie
// =============================================================================

/// Informacja o unikalnym modelu dostepnym w mesh (z wielu nodow)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedModelInfo {
    pub model_name: String,
    pub service_type: String,
    pub instances: Vec<ModelInstance>,
}

/// Instancja modelu na konkretnym nodzie
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInstance {
    pub node_id: String,
    pub node_name: String,
    pub service_id: String,
    pub status: String,
}

// =============================================================================
// MeshServiceRegistry
// =============================================================================

/// Rejestr serwisow ze wszystkich nodow mesh
pub struct MeshServiceRegistry {
    local_node_id: String,
    local_services: RwLock<Vec<MeshServiceInfo>>,
    remote_services: RwLock<HashMap<String, Vec<MeshServiceInfo>>>,
    direct_connections: RwLock<HashMap<String, HashSet<String>>>,
}

impl MeshServiceRegistry {
    pub fn new(local_node_id: String) -> Self {
        Self {
            local_node_id,
            local_services: RwLock::new(Vec::new()),
            remote_services: RwLock::new(HashMap::new()),
            direct_connections: RwLock::new(HashMap::new()),
        }
    }

    /// Rejestruj lokalny serwis
    pub fn register_local(&self, service: MeshServiceInfo) {
        info!(
            service_id = %service.service_id,
            service_name = %service.service_name,
            "Rejestracja lokalnego serwisu"
        );
        self.local_services.write().push(service);
    }

    /// Wyrejestruj lokalny serwis po ID
    pub fn unregister_local(&self, service_id: &str) {
        let mut services = self.local_services.write();
        let before = services.len();
        services.retain(|s| s.service_id != service_id);
        if services.len() < before {
            info!(service_id = %service_id, "Wyrejestrowano lokalny serwis");
        }
    }

    /// Aktualizuj serwisy ze zdalnego noda (po ServiceAnnounce)
    pub fn update_remote(&self, node_id: &str, services: Vec<MeshServiceInfo>) {
        debug!(
            node_id = %node_id,
            count = services.len(),
            "Aktualizacja zdalnych serwisow"
        );
        self.remote_services
            .write()
            .insert(node_id.to_string(), services);
    }

    /// Usun serwisy noda (po disconnect)
    pub fn remove_node(&self, node_id: &str) {
        self.remote_services.write().remove(node_id);
        self.direct_connections.write().remove(node_id);
        info!(node_id = %node_id, "Usunieto serwisy i topologie noda");
    }

    /// Aktualizuj topologie — kto widzi kogo bezposrednio
    pub fn update_topology(&self, node_id: &str, connected_to: HashSet<String>) {
        debug!(
            node_id = %node_id,
            connected = connected_to.len(),
            "Aktualizacja topologii"
        );
        self.direct_connections
            .write()
            .insert(node_id.to_string(), connected_to);
    }

    /// Zwraca serwisy widoczne z perspektywy lokalnego noda.
    /// Dedup: jesli lokalny node widzi node C bezposrednio,
    /// nie pokazuj serwisow C przez node B.
    pub fn visible_services(&self) -> Vec<MeshServiceInfo> {
        let local = self.local_services.read();
        let remote = self.remote_services.read();

        // Zbierz node_id do ktorych mamy bezposrednie polaczenie
        let directly_connected: HashSet<&String> = remote.keys().collect();

        let mut result: Vec<MeshServiceInfo> = local.clone();

        // Dodaj serwisy ze zdalnych nodow — dedup po owner_node_id
        let mut seen_owners: HashSet<String> = HashSet::new();
        seen_owners.insert(self.local_node_id.clone());

        // Bezposrednie polaczenia — priorytet
        for (node_id, services) in remote.iter() {
            for svc in services {
                let owner = if svc.node_id.is_empty() {
                    node_id
                } else {
                    &svc.node_id
                };

                if seen_owners.contains(owner) {
                    continue;
                }

                // Jesli widzimy owner bezposrednio, dodaj serwis tylko
                // gdy pochodzi z tego bezposredniego polaczenia
                if directly_connected.contains(owner) && owner != node_id {
                    continue;
                }

                result.push(svc.clone());
                seen_owners.insert(owner.clone());
            }
        }

        result
    }

    /// Zwraca unikalne modele (deduplikowane po nazwie modelu)
    pub fn unique_models(&self) -> Vec<UnifiedModelInfo> {
        let all_services = self.visible_services();
        let mut model_map: HashMap<(String, String), Vec<ModelInstance>> = HashMap::new();

        for svc in &all_services {
            for model_name in &svc.models {
                let key = (model_name.clone(), svc.service_type.clone());
                model_map
                    .entry(key)
                    .or_default()
                    .push(ModelInstance {
                        node_id: svc.node_id.clone(),
                        node_name: svc.service_name.clone(),
                        service_id: svc.service_id.clone(),
                        status: svc.status.clone(),
                    });
            }
        }

        model_map
            .into_iter()
            .map(|((model_name, service_type), instances)| UnifiedModelInfo {
                model_name,
                service_type,
                instances,
            })
            .collect()
    }

    /// Znajdz dowolny node ktory ma serwis danego typu (bez konkretnego modelu)
    pub fn find_service_by_type(&self, service_type: &str) -> Option<String> {
        let all_services = self.visible_services();

        all_services
            .iter()
            .find(|svc| {
                svc.node_id != self.local_node_id
                    && svc.service_type == service_type
                    && svc.status == "running"
            })
            .map(|svc| svc.node_id.clone())
    }

    /// Znajdz node ktory ma dany serwis (typ + model)
    pub fn find_service_node(&self, service_type: &str, model_name: &str) -> Option<String> {
        let all_services = self.visible_services();

        all_services
            .iter()
            .find(|svc| {
                svc.node_id != self.local_node_id
                    && svc.service_type == service_type
                    && svc.models.iter().any(|m| m == model_name)
                    && svc.status == "running"
            })
            .map(|svc| svc.node_id.clone())
    }

    /// Znajdz sciezke multi-hop do noda (BFS po topologii)
    pub fn find_route(&self, target_node_id: &str) -> Option<Vec<String>> {
        if target_node_id == self.local_node_id {
            return Some(vec![self.local_node_id.clone()]);
        }

        let topology = self.direct_connections.read();
        let remote = self.remote_services.read();

        // Bezposrednie polaczenie z lokalnego noda
        let local_neighbors: HashSet<String> = remote.keys().cloned().collect();

        if local_neighbors.contains(target_node_id) {
            return Some(vec![self.local_node_id.clone(), target_node_id.to_string()]);
        }

        // BFS
        let mut visited: HashSet<String> = HashSet::new();
        visited.insert(self.local_node_id.clone());

        let mut queue: VecDeque<Vec<String>> = VecDeque::new();

        for neighbor in &local_neighbors {
            let path = vec![self.local_node_id.clone(), neighbor.clone()];
            if neighbor == target_node_id {
                return Some(path);
            }
            queue.push_back(path);
            visited.insert(neighbor.clone());
        }

        while let Some(path) = queue.pop_front() {
            let current = path.last().unwrap();
            let neighbors = topology.get(current);

            if let Some(neighbors) = neighbors {
                for next in neighbors {
                    if visited.contains(next) {
                        continue;
                    }
                    visited.insert(next.clone());

                    let mut new_path = path.clone();
                    new_path.push(next.clone());

                    if next == target_node_id {
                        return Some(new_path);
                    }

                    queue.push_back(new_path);
                }
            }
        }

        None
    }

    /// Pobierz lokalne serwisy (do ServiceAnnounce)
    pub fn local_services(&self) -> Vec<MeshServiceInfo> {
        self.local_services.read().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_service(
        service_id: &str,
        service_type: &str,
        node_id: &str,
        models: Vec<&str>,
    ) -> MeshServiceInfo {
        MeshServiceInfo {
            service_id: service_id.to_string(),
            service_name: format!("{}-svc", service_type),
            service_type: service_type.to_string(),
            node_id: node_id.to_string(),
            quic_port: 4433,
            quic_url: String::new(),
            status: "running".to_string(),
            models: models.into_iter().map(String::from).collect(),
            load_percent: 10,
        }
    }

    #[test]
    fn rejestracja_i_widocznosc_lokalnych() {
        let registry = MeshServiceRegistry::new("node-a".to_string());
        let svc = make_service("svc-1", "llm", "node-a", vec!["llama3"]);
        registry.register_local(svc);

        let visible = registry.visible_services();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].service_id, "svc-1");
    }

    #[test]
    fn wyrejestrowanie_lokalne() {
        let registry = MeshServiceRegistry::new("node-a".to_string());
        let svc = make_service("svc-1", "llm", "node-a", vec!["llama3"]);
        registry.register_local(svc);
        registry.unregister_local("svc-1");

        let visible = registry.visible_services();
        assert!(visible.is_empty());
    }

    #[test]
    fn zdalne_serwisy_widoczne() {
        let registry = MeshServiceRegistry::new("node-a".to_string());
        let remote_svc = make_service("svc-2", "tts", "node-b", vec!["piper"]);
        registry.update_remote("node-b", vec![remote_svc]);

        let visible = registry.visible_services();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].node_id, "node-b");
    }

    #[test]
    fn remove_node_czysci_serwisy() {
        let registry = MeshServiceRegistry::new("node-a".to_string());
        registry.update_remote("node-b", vec![make_service("s1", "llm", "node-b", vec!["m1"])]);
        registry.remove_node("node-b");

        let visible = registry.visible_services();
        assert!(visible.is_empty());
    }

    #[test]
    fn unique_models_dedup() {
        let registry = MeshServiceRegistry::new("node-a".to_string());
        registry.register_local(make_service("s1", "llm", "node-a", vec!["llama3"]));
        registry.update_remote(
            "node-b",
            vec![make_service("s2", "llm", "node-b", vec!["llama3", "mistral"])],
        );

        let models = registry.unique_models();
        let llama = models.iter().find(|m| m.model_name == "llama3");
        assert!(llama.is_some());
        assert_eq!(llama.unwrap().instances.len(), 2);
    }

    #[test]
    fn find_service_node_dziala() {
        let registry = MeshServiceRegistry::new("node-a".to_string());
        registry.update_remote(
            "node-b",
            vec![make_service("s1", "tts", "node-b", vec!["piper"])],
        );

        let result = registry.find_service_node("tts", "piper");
        assert_eq!(result, Some("node-b".to_string()));

        let result = registry.find_service_node("stt", "whisper");
        assert!(result.is_none());
    }

    #[test]
    fn find_route_bezposredni() {
        let registry = MeshServiceRegistry::new("node-a".to_string());
        registry.update_remote("node-b", vec![]);

        let route = registry.find_route("node-b");
        assert_eq!(route, Some(vec!["node-a".to_string(), "node-b".to_string()]));
    }

    #[test]
    fn find_route_multi_hop() {
        let registry = MeshServiceRegistry::new("node-a".to_string());

        // A widzi B bezposrednio
        registry.update_remote("node-b", vec![]);
        // B widzi C
        let mut b_connections = HashSet::new();
        b_connections.insert("node-c".to_string());
        b_connections.insert("node-a".to_string());
        registry.update_topology("node-b", b_connections);

        let route = registry.find_route("node-c");
        assert_eq!(
            route,
            Some(vec![
                "node-a".to_string(),
                "node-b".to_string(),
                "node-c".to_string()
            ])
        );
    }

    #[test]
    fn find_route_brak_sciezki() {
        let registry = MeshServiceRegistry::new("node-a".to_string());
        let route = registry.find_route("node-z");
        assert!(route.is_none());
    }
}
