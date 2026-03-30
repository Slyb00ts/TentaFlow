// =============================================================================
// Plik: mesh/crdt.rs
// Opis: CRDT — bezkonfliktowa replikacja stanu miedzy peerami mesh.
//       Implementacja LWW-Register, OR-Set i operation-based state sync.
// =============================================================================

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};

/// Zegar Lamport — logiczny timestamp z tie-breakingiem po node_id
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LamportClock {
    pub time: u64,
    pub node_id_hash: u64,
}

impl LamportClock {
    /// Tworzy nowy zegar z time=0
    pub fn new(node_id: &str) -> Self {
        let mut hasher = DefaultHasher::new();
        node_id.hash(&mut hasher);
        Self {
            time: 0,
            node_id_hash: hasher.finish(),
        }
    }

    /// Inkrementuje zegar i zwraca nowa wartosc
    pub fn tick(&mut self) -> Self {
        self.time += 1;
        *self
    }

    /// Merge z innym zegarem — przyjmij max(self, other) + 1
    pub fn merge(&mut self, other: &Self) {
        self.time = self.time.max(other.time) + 1;
    }
}

impl Ord for LamportClock {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.time
            .cmp(&other.time)
            .then_with(|| self.node_id_hash.cmp(&other.node_id_hash))
    }
}

impl PartialOrd for LamportClock {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// LWW-Register (Last-Writer-Wins) — rejestr z rozstrzyganiem konfliktow przez timestamp
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LwwRegister<T: Clone> {
    pub value: T,
    pub timestamp: LamportClock,
}

impl<T: Clone> LwwRegister<T> {
    pub fn new(value: T, clock: LamportClock) -> Self {
        Self {
            value,
            timestamp: clock,
        }
    }

    /// Merge z innym rejestrem. Zwraca true jesli wartosc sie zmienila.
    pub fn merge(&mut self, other: &Self) -> bool {
        if other.timestamp > self.timestamp {
            self.value = other.value.clone();
            self.timestamp = other.timestamp;
            true
        } else {
            false
        }
    }
}

/// OR-Set (Observed-Remove Set) — zbior z bezkonfliktowym dodawaniem i usuwaniem
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrSet<T: Clone + Eq + Hash> {
    pub elements: HashMap<T, LamportClock>,
    pub tombstones: HashMap<T, LamportClock>,
}

impl<T: Clone + Eq + Hash> OrSet<T> {
    pub fn new() -> Self {
        Self {
            elements: HashMap::new(),
            tombstones: HashMap::new(),
        }
    }

    /// Dodaj element do zbioru
    pub fn add(&mut self, element: T, clock: LamportClock) {
        // Dodanie wygrywa z usunieciem jesli ma nowszy timestamp
        let dominated_by_tombstone = self
            .tombstones
            .get(&element)
            .map_or(false, |ts| *ts >= clock);

        if !dominated_by_tombstone {
            self.elements.insert(element, clock);
        }
    }

    /// Usun element ze zbioru
    pub fn remove(&mut self, element: &T, clock: LamportClock) {
        let dominated_by_add = self
            .elements
            .get(element)
            .map_or(false, |ts| *ts > clock);

        if !dominated_by_add {
            self.elements.remove(element);
            self.tombstones.insert(element.clone(), clock);
        }
    }

    /// Sprawdz czy element nalezy do zbioru
    pub fn contains(&self, element: &T) -> bool {
        if let Some(add_ts) = self.elements.get(element) {
            if let Some(rm_ts) = self.tombstones.get(element) {
                // Element jest w zbiorze jesli dodanie jest nowsze niz usuniecie
                add_ts > rm_ts
            } else {
                true
            }
        } else {
            false
        }
    }

    /// Zwroc aktywne elementy zbioru
    pub fn elements(&self) -> Vec<&T> {
        self.elements
            .iter()
            .filter(|(elem, add_ts)| {
                self.tombstones
                    .get(*elem)
                    .map_or(true, |rm_ts| *add_ts > rm_ts)
            })
            .map(|(elem, _)| elem)
            .collect()
    }

    /// Merge z innym OR-Set. Zwraca true jesli stan sie zmienil.
    pub fn merge(&mut self, other: &Self) -> bool {
        let mut changed = false;

        // Merge elementow — zachowaj nowszy timestamp
        for (elem, other_ts) in &other.elements {
            match self.elements.get(elem) {
                Some(self_ts) if *self_ts >= *other_ts => {}
                _ => {
                    self.elements.insert(elem.clone(), *other_ts);
                    changed = true;
                }
            }
        }

        // Merge tombstonow — zachowaj nowszy timestamp
        for (elem, other_ts) in &other.tombstones {
            match self.tombstones.get(elem) {
                Some(self_ts) if *self_ts >= *other_ts => {}
                _ => {
                    self.tombstones.insert(elem.clone(), *other_ts);
                    changed = true;
                }
            }
        }

        changed
    }
}

impl<T: Clone + Eq + Hash> Default for OrSet<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Operacja CRDT — propagowana miedzy peerami przez gossip
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CrdtOperation {
    UpsertService {
        id: i64,
        name: String,
        data_json: String,
        clock: LamportClock,
    },
    DeleteService {
        id: i64,
        clock: LamportClock,
    },
    UpsertModel {
        id: i64,
        name: String,
        data_json: String,
        clock: LamportClock,
    },
    DeleteModel {
        id: i64,
        clock: LamportClock,
    },
    UpsertAlias {
        alias: String,
        target: String,
        clock: LamportClock,
    },
    DeleteAlias {
        alias: String,
        clock: LamportClock,
    },
    UpsertFlow {
        id: i64,
        data_json: String,
        clock: LamportClock,
    },
    UpsertPrompt {
        prompt_id: String,
        data_json: String,
        clock: LamportClock,
    },
    UpsertApiKey {
        id: i64,
        data_json: String,
        clock: LamportClock,
    },
    UpsertSetting {
        key: String,
        value: String,
        clock: LamportClock,
    },

    // --- Operacje na uzytkownikach (sync per username, nie per ID) ---
    UpsertUser {
        username: String,
        password_hash: String,
        display_name: String,
        email: String,
        is_active: bool,
        is_admin: bool,
        clock: LamportClock,
    },
    DeleteUser {
        username: String,
        clock: LamportClock,
    },

    // --- Operacje na grupach ---
    UpsertGroup {
        name: String,
        description: String,
        clock: LamportClock,
    },
    DeleteGroup {
        name: String,
        clock: LamportClock,
    },
    AddGroupMember {
        group_name: String,
        username: String,
        clock: LamportClock,
    },
    RemoveGroupMember {
        group_name: String,
        username: String,
        clock: LamportClock,
    },

    // --- Operacje na uprawnieniach ---
    SetPermission {
        addon_id: String,
        subject_type: String,
        subject_name: String,
        resource: String,
        access_level: String,
        clock: LamportClock,
    },
    DeletePermission {
        addon_id: String,
        subject_type: String,
        subject_name: String,
        resource: String,
        clock: LamportClock,
    },

    // --- Operacje na addonach ---
    SyncAddon {
        addon_id: String,
        name: String,
        version: String,
        manifest_json: String,
        platforms: String,
        wasm_hash: String,
        clock: LamportClock,
    },
    DeleteAddon {
        addon_id: String,
        clock: LamportClock,
    },

    // --- Operacje na konfiguracji addonow ---
    SetAddonConfig {
        addon_id: String,
        key: String,
        value: String,
        clock: LamportClock,
    },

    // --- Operacje na sekretach (zaszyfrowane) ---
    SetSecret {
        addon_id: String,
        username: Option<String>,
        key: String,
        encrypted_value: String,
        clock: LamportClock,
    },
    DeleteSecret {
        addon_id: String,
        username: Option<String>,
        key: String,
        clock: LamportClock,
    },

    // --- Operacje na SSO providerach ---
    UpsertSsoProvider {
        name: String,
        provider_type: String,
        client_id: String,
        client_secret_encrypted: String,
        discovery_url: String,
        enabled: bool,
        clock: LamportClock,
    },
    DeleteSsoProvider {
        name: String,
        clock: LamportClock,
    },

    // --- Operacje na sync exclusions ---
    SetSyncExclusion {
        group_name: String,
        resource_type: String,
        clock: LamportClock,
    },
    DeleteSyncExclusion {
        group_name: String,
        resource_type: String,
        clock: LamportClock,
    },

    // --- Operacje na zaufanych nodach mesh ---
    AddTrustedNode {
        node_id: String,
        public_key_hex: String,
        hostname: String,
        clock: LamportClock,
    },
    RemoveTrustedNode {
        node_id: String,
        clock: LamportClock,
    },
    RevokeTrustedNode {
        node_id: String,
        revoked_by: String,
        clock: LamportClock,
    },
}

impl CrdtOperation {
    /// Zwraca zegar operacji
    pub fn clock(&self) -> &LamportClock {
        match self {
            Self::UpsertService { clock, .. }
            | Self::DeleteService { clock, .. }
            | Self::UpsertModel { clock, .. }
            | Self::DeleteModel { clock, .. }
            | Self::UpsertAlias { clock, .. }
            | Self::DeleteAlias { clock, .. }
            | Self::UpsertFlow { clock, .. }
            | Self::UpsertPrompt { clock, .. }
            | Self::UpsertApiKey { clock, .. }
            | Self::UpsertSetting { clock, .. }
            | Self::UpsertUser { clock, .. }
            | Self::DeleteUser { clock, .. }
            | Self::UpsertGroup { clock, .. }
            | Self::DeleteGroup { clock, .. }
            | Self::AddGroupMember { clock, .. }
            | Self::RemoveGroupMember { clock, .. }
            | Self::SetPermission { clock, .. }
            | Self::DeletePermission { clock, .. }
            | Self::SyncAddon { clock, .. }
            | Self::DeleteAddon { clock, .. }
            | Self::SetAddonConfig { clock, .. }
            | Self::SetSecret { clock, .. }
            | Self::DeleteSecret { clock, .. }
            | Self::UpsertSsoProvider { clock, .. }
            | Self::DeleteSsoProvider { clock, .. }
            | Self::SetSyncExclusion { clock, .. }
            | Self::DeleteSyncExclusion { clock, .. }
            | Self::AddTrustedNode { clock, .. }
            | Self::RemoveTrustedNode { clock, .. }
            | Self::RevokeTrustedNode { clock, .. } => clock,
        }
    }

    /// Klucz unikalny operacji — do kompaktowania
    fn compact_key(&self) -> String {
        match self {
            Self::UpsertService { id, .. } | Self::DeleteService { id, .. } => {
                format!("service:{id}")
            }
            Self::UpsertModel { id, .. } | Self::DeleteModel { id, .. } => {
                format!("model:{id}")
            }
            Self::UpsertAlias { alias, .. } | Self::DeleteAlias { alias, .. } => {
                format!("alias:{alias}")
            }
            Self::UpsertFlow { id, .. } => format!("flow:{id}"),
            Self::UpsertPrompt { prompt_id, .. } => format!("prompt:{prompt_id}"),
            Self::UpsertApiKey { id, .. } => format!("apikey:{id}"),
            Self::UpsertSetting { key, .. } => format!("setting:{key}"),

            // Nowe operacje — klucze po nazwie
            Self::UpsertUser { username, .. } | Self::DeleteUser { username, .. } => {
                format!("user:{username}")
            }
            Self::UpsertGroup { name, .. } | Self::DeleteGroup { name, .. } => {
                format!("group:{name}")
            }
            Self::AddGroupMember { group_name, username, .. }
            | Self::RemoveGroupMember { group_name, username, .. } => {
                format!("group_member:{group_name}:{username}")
            }
            Self::SetPermission { addon_id, subject_type, subject_name, resource, .. }
            | Self::DeletePermission { addon_id, subject_type, subject_name, resource, .. } => {
                format!("perm:{addon_id}:{subject_type}:{subject_name}:{resource}")
            }
            Self::SyncAddon { addon_id, .. } | Self::DeleteAddon { addon_id, .. } => {
                format!("addon:{addon_id}")
            }
            Self::SetAddonConfig { addon_id, key, .. } => {
                format!("addon_config:{addon_id}:{key}")
            }
            Self::SetSecret { addon_id, username, key, .. }
            | Self::DeleteSecret { addon_id, username, key, .. } => {
                let uname = username.as_deref().unwrap_or("_global_");
                format!("secret:{addon_id}:{uname}:{key}")
            }
            Self::UpsertSsoProvider { name, .. } | Self::DeleteSsoProvider { name, .. } => {
                format!("sso:{name}")
            }
            Self::SetSyncExclusion { group_name, resource_type, .. }
            | Self::DeleteSyncExclusion { group_name, resource_type, .. } => {
                format!("sync_excl:{group_name}:{resource_type}")
            }
            Self::AddTrustedNode { node_id, .. }
            | Self::RemoveTrustedNode { node_id, .. }
            | Self::RevokeTrustedNode { node_id, .. } => {
                format!("trusted_node:{node_id}")
            }
        }
    }

    /// Zwraca node_id_hash zrodla operacji
    fn source_node_hash(&self) -> u64 {
        self.clock().node_id_hash
    }
}

/// Stan CRDT — log operacji + version vector do delta sync
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrdtState {
    pub operations_log: Vec<CrdtOperation>,
    pub version_vector: HashMap<String, u64>,
}

impl CrdtState {
    pub fn new() -> Self {
        Self {
            operations_log: Vec::new(),
            version_vector: HashMap::new(),
        }
    }

    /// Zastosuj operacje (lokalna zmiana)
    pub fn apply(&mut self, op: CrdtOperation) {
        let node_key = op.source_node_hash().to_string();
        let time = op.clock().time;

        // Aktualizuj version vector
        let entry = self.version_vector.entry(node_key).or_insert(0);
        if time > *entry {
            *entry = time;
        }

        self.operations_log.push(op);
    }

    /// Merge z remote state. Zwraca operacje nowsze niz lokalny stan (do aplikacji w DB).
    pub fn merge(&mut self, remote: &CrdtState) -> Vec<CrdtOperation> {
        let mut new_ops = Vec::new();

        for op in &remote.operations_log {
            let node_key = op.source_node_hash().to_string();
            let time = op.clock().time;
            let local_time = self.version_vector.get(&node_key).copied().unwrap_or(0);

            // Przyjmij operacje nowsze niz nasz version vector
            if time > local_time {
                new_ops.push(op.clone());
            }
        }

        // Zastosuj nowe operacje
        for op in &new_ops {
            self.apply(op.clone());
        }

        // Aktualizuj version vector z remote
        for (node, &time) in &remote.version_vector {
            let entry = self.version_vector.entry(node.clone()).or_insert(0);
            if time > *entry {
                *entry = time;
            }
        }

        new_ops
    }

    /// Delta od danej wersji — operacje nowsze niz podany version vector
    pub fn delta_since(&self, version_vector: &HashMap<String, u64>) -> CrdtState {
        let ops: Vec<CrdtOperation> = self
            .operations_log
            .iter()
            .filter(|op| {
                let node_key = op.source_node_hash().to_string();
                let time = op.clock().time;
                let known_time = version_vector.get(&node_key).copied().unwrap_or(0);
                time > known_time
            })
            .cloned()
            .collect();

        CrdtState {
            operations_log: ops,
            version_vector: self.version_vector.clone(),
        }
    }

    /// Kompaktowanie — zachowaj tylko najnowsza operacje per klucz
    pub fn compact(&mut self) {
        let mut latest: HashMap<String, (usize, &LamportClock)> = HashMap::new();

        // Znajdz najnowsza operacje per klucz
        for (idx, op) in self.operations_log.iter().enumerate() {
            let key = op.compact_key();
            match latest.get(&key) {
                Some((_, existing_clock)) if *existing_clock >= op.clock() => {}
                _ => {
                    latest.insert(key, (idx, op.clock()));
                }
            }
        }

        // Zachowaj indeksy najnowszych operacji
        let mut keep_indices: Vec<usize> = latest.values().map(|(idx, _)| *idx).collect();
        keep_indices.sort_unstable();

        let compacted: Vec<CrdtOperation> = keep_indices
            .into_iter()
            .map(|idx| self.operations_log[idx].clone())
            .collect();

        self.operations_log = compacted;
    }
}

impl Default for CrdtState {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Testy
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lamport_clock_ordering() {
        let mut clock_a = LamportClock::new("node-a");
        let mut clock_b = LamportClock::new("node-b");

        // Poczatkowy stan — oba na 0, roznia sie node_id_hash
        assert_eq!(clock_a.time, 0);
        assert_eq!(clock_b.time, 0);
        assert_ne!(clock_a.node_id_hash, clock_b.node_id_hash);

        // Po tick oba maja time=1, ale rozny node_id_hash decyduje o kolejnosci
        let t_a = clock_a.tick();
        let t_b = clock_b.tick();
        assert_eq!(t_a.time, 1);
        assert_eq!(t_b.time, 1);
        assert_ne!(t_a, t_b);
        // Deterministyczny porzadek — wyzszy hash wygrywa
        assert!(t_a != t_b);
        assert!(t_a < t_b || t_a > t_b);

        // Wyzszy time zawsze wygrywa
        let t_a2 = clock_a.tick();
        assert!(t_a2 > t_b);
    }

    #[test]
    fn lamport_clock_merge() {
        let mut clock_a = LamportClock::new("node-a");
        let mut clock_b = LamportClock::new("node-b");

        clock_a.tick(); // a.time = 1
        clock_b.tick(); // b.time = 1
        clock_b.tick(); // b.time = 2
        clock_b.tick(); // b.time = 3

        // Merge — clock_a powinien przeskoczyc do max(1, 3) + 1 = 4
        clock_a.merge(&clock_b);
        assert_eq!(clock_a.time, 4);
    }

    #[test]
    fn lww_register_merge_newer_wins() {
        let mut clock_a = LamportClock::new("node-a");
        let mut clock_b = LamportClock::new("node-b");

        let t_a = clock_a.tick();
        let _t_b1 = clock_b.tick();
        let t_b2 = clock_b.tick();

        let mut reg_a = LwwRegister::new("wartosc-a".to_string(), t_a);
        let reg_b = LwwRegister::new("wartosc-b".to_string(), t_b2);

        // b ma wyzszy timestamp — powinien wygrac
        let changed = reg_a.merge(&reg_b);
        assert!(changed);
        assert_eq!(reg_a.value, "wartosc-b");
    }

    #[test]
    fn lww_register_merge_older_loses() {
        let mut clock_a = LamportClock::new("node-a");
        let mut clock_b = LamportClock::new("node-b");

        let _t_a1 = clock_a.tick();
        let t_a2 = clock_a.tick();
        let t_b = clock_b.tick();

        let mut reg_a = LwwRegister::new("wartosc-a".to_string(), t_a2);
        let reg_b = LwwRegister::new("wartosc-b".to_string(), t_b);

        // a ma wyzszy timestamp — b nie powinien wygrac
        let changed = reg_a.merge(&reg_b);
        assert!(!changed);
        assert_eq!(reg_a.value, "wartosc-a");
    }

    #[test]
    fn lww_register_same_time_different_nodes() {
        let mut clock_a = LamportClock::new("node-a");
        let mut clock_b = LamportClock::new("node-b");

        let t_a = clock_a.tick();
        let t_b = clock_b.tick();

        // Ten sam time — node_id_hash decyduje kto wygra
        assert_eq!(t_a.time, t_b.time);

        let mut reg_a = LwwRegister::new("wartosc-a".to_string(), t_a);
        let reg_b = LwwRegister::new("wartosc-b".to_string(), t_b);

        let changed = reg_a.merge(&reg_b);

        if t_b > t_a {
            assert!(changed);
            assert_eq!(reg_a.value, "wartosc-b");
        } else {
            assert!(!changed);
            assert_eq!(reg_a.value, "wartosc-a");
        }
    }

    #[test]
    fn or_set_add_remove() {
        let mut clock = LamportClock::new("node-a");
        let mut set = OrSet::new();

        let t1 = clock.tick();
        set.add("element-1", t1);
        assert!(set.contains(&"element-1"));

        let t2 = clock.tick();
        set.add("element-2", t2);
        assert_eq!(set.elements().len(), 2);

        let t3 = clock.tick();
        set.remove(&"element-1", t3);
        assert!(!set.contains(&"element-1"));
        assert!(set.contains(&"element-2"));
        assert_eq!(set.elements().len(), 1);
    }

    #[test]
    fn or_set_add_after_remove() {
        let mut clock = LamportClock::new("node-a");
        let mut set = OrSet::new();

        let t1 = clock.tick();
        set.add("elem", t1);
        assert!(set.contains(&"elem"));

        let t2 = clock.tick();
        set.remove(&"elem", t2);
        assert!(!set.contains(&"elem"));

        // Ponowne dodanie z nowszym timestampem powinno zaadzialac
        let t3 = clock.tick();
        set.add("elem", t3);
        assert!(set.contains(&"elem"));
    }

    #[test]
    fn or_set_merge_concurrent() {
        let mut clock_a = LamportClock::new("node-a");
        let mut clock_b = LamportClock::new("node-b");
        let mut set_a = OrSet::new();
        let mut set_b = OrSet::new();

        // Node A dodaje "x"
        let t_a = clock_a.tick();
        set_a.add("x", t_a);

        // Node B dodaje "y"
        let t_b = clock_b.tick();
        set_b.add("y", t_b);

        // Merge — oba elementy powinny byc w zbiorze
        let changed = set_a.merge(&set_b);
        assert!(changed);
        assert!(set_a.contains(&"x"));
        assert!(set_a.contains(&"y"));
    }

    #[test]
    fn or_set_merge_concurrent_add_remove() {
        let mut clock_a = LamportClock::new("node-a");
        let mut clock_b = LamportClock::new("node-b");
        let mut set_a = OrSet::new();
        let mut set_b = OrSet::new();

        // Wspolny stan poczatkowy — oba dodaja "x"
        let t_a1 = clock_a.tick();
        let t_b1 = clock_b.tick();
        set_a.add("x", t_a1);
        set_b.add("x", t_b1);

        // Node A usuwa "x"
        let t_a2 = clock_a.tick();
        set_a.remove(&"x", t_a2);

        // Node B ustawia nowszy timestamp na "x" (dodaje ponownie)
        let _t_b2 = clock_b.tick();
        let t_b3 = clock_b.tick();
        set_b.add("x", t_b3);

        // Merge — add z b (time=3) > remove z a (time=2), wiec "x" powinien byc
        set_a.merge(&set_b);
        assert!(set_a.contains(&"x"));
    }

    #[test]
    fn crdt_state_apply_and_delta() {
        let mut clock = LamportClock::new("node-a");
        let mut state = CrdtState::new();

        // Zastosuj kilka operacji
        let t1 = clock.tick();
        state.apply(CrdtOperation::UpsertService {
            id: 1,
            name: "svc-1".into(),
            data_json: "{}".into(),
            clock: t1,
        });

        let t2 = clock.tick();
        state.apply(CrdtOperation::UpsertModel {
            id: 10,
            name: "model-a".into(),
            data_json: "{}".into(),
            clock: t2,
        });

        assert_eq!(state.operations_log.len(), 2);

        // Delta od pustego version vector — powinno zwrocic wszystko
        let empty_vv: HashMap<String, u64> = HashMap::new();
        let delta = state.delta_since(&empty_vv);
        assert_eq!(delta.operations_log.len(), 2);

        // Delta od aktualnego version vector — nic nowego
        let delta2 = state.delta_since(&state.version_vector);
        assert_eq!(delta2.operations_log.len(), 0);
    }

    #[test]
    fn crdt_state_merge_two_nodes() {
        let mut clock_a = LamportClock::new("node-a");
        let mut clock_b = LamportClock::new("node-b");
        let mut state_a = CrdtState::new();
        let mut state_b = CrdtState::new();

        // Node A dodaje serwis
        let t_a = clock_a.tick();
        state_a.apply(CrdtOperation::UpsertService {
            id: 1,
            name: "svc-a".into(),
            data_json: "{}".into(),
            clock: t_a,
        });

        // Node B dodaje model
        let t_b = clock_b.tick();
        state_b.apply(CrdtOperation::UpsertModel {
            id: 10,
            name: "model-b".into(),
            data_json: "{}".into(),
            clock: t_b,
        });

        // Merge B do A — powinien dostac operacje z B
        let new_ops = state_a.merge(&state_b);
        assert_eq!(new_ops.len(), 1);
        assert_eq!(state_a.operations_log.len(), 2);

        // Ponowny merge nie powinien dac nowych operacji
        let new_ops2 = state_a.merge(&state_b);
        assert_eq!(new_ops2.len(), 0);
    }

    #[test]
    fn crdt_state_compact() {
        let mut clock = LamportClock::new("node-a");
        let mut state = CrdtState::new();

        // Trzy updaty tego samego serwisu
        for i in 0..3 {
            let t = clock.tick();
            state.apply(CrdtOperation::UpsertService {
                id: 1,
                name: format!("svc-v{i}"),
                data_json: "{}".into(),
                clock: t,
            });
        }

        // Plus jeden inny serwis
        let t = clock.tick();
        state.apply(CrdtOperation::UpsertService {
            id: 2,
            name: "svc-other".into(),
            data_json: "{}".into(),
            clock: t,
        });

        assert_eq!(state.operations_log.len(), 4);

        // Po kompaktowaniu — 2 operacje (najnowsza per klucz)
        state.compact();
        assert_eq!(state.operations_log.len(), 2);

        // Najnowsza wersja serwisu 1 powinna byc v2
        let svc1_op = state
            .operations_log
            .iter()
            .find(|op| matches!(op, CrdtOperation::UpsertService { id: 1, .. }))
            .expect("powinien istniec serwis 1");

        if let CrdtOperation::UpsertService { name, .. } = svc1_op {
            assert_eq!(name, "svc-v2");
        }
    }

    #[test]
    fn crdt_state_delta_sync_scenario() {
        let mut clock_a = LamportClock::new("node-a");
        let mut clock_b = LamportClock::new("node-b");
        let mut state_a = CrdtState::new();
        let mut state_b = CrdtState::new();

        // Runda 1: Node A dodaje serwis
        let t_a1 = clock_a.tick();
        state_a.apply(CrdtOperation::UpsertService {
            id: 1,
            name: "svc-1".into(),
            data_json: "{}".into(),
            clock: t_a1,
        });

        // Sync A -> B przez delta
        let delta_ab = state_a.delta_since(&state_b.version_vector);
        let new_ops = state_b.merge(&delta_ab);
        assert_eq!(new_ops.len(), 1);

        // Runda 2: Node B dodaje alias
        let t_b1 = clock_b.tick();
        state_b.apply(CrdtOperation::UpsertAlias {
            alias: "gpt4".into(),
            target: "openai/gpt-4".into(),
            clock: t_b1,
        });

        // Node A dodaje kolejny serwis
        let t_a2 = clock_a.tick();
        state_a.apply(CrdtOperation::UpsertService {
            id: 2,
            name: "svc-2".into(),
            data_json: "{}".into(),
            clock: t_a2,
        });

        // Sync B -> A
        let delta_ba = state_b.delta_since(&state_a.version_vector);
        let new_ops_a = state_a.merge(&delta_ba);
        // Node A powinien dostac alias od B (serwis 1 juz ma)
        assert_eq!(new_ops_a.len(), 1);

        // Sync A -> B
        let delta_ab2 = state_a.delta_since(&state_b.version_vector);
        let new_ops_b = state_b.merge(&delta_ab2);
        // Node B powinien dostac serwis 2 od A
        assert_eq!(new_ops_b.len(), 1);

        // Oba stany powinny miec 3 operacje (+ duplikaty z merge)
        // Po kompaktowaniu powinno byc 3 unikalne
        state_a.compact();
        state_b.compact();
        assert_eq!(state_a.operations_log.len(), 3);
        assert_eq!(state_b.operations_log.len(), 3);
    }
}
