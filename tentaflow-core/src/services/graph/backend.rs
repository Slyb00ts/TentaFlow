// ===== Plik: services/graph/backend.rs — trait GraphBackend + CozoBackend =====
//
// Abstrakcja silnika grafowego pod warstwą `GraphManager`. `CozoBackend` owija
// jeden `cozo::DbInstance` na katalog per `(org, addon, collection)` — fizyczna
// izolacja: jeden plik/katalog = jedna kolekcja. Engine wybierany przy otwarciu
// wg PLATFORMY:
//   - natywnie (serwer/desktop/mobile): `sled` (czysto-Rust embedded KV),
//     opcjonalnie `rocksdb` (feature `graph-rocksdb`, duże grafy na serwerze),
//   - `wasm32` (dashboard w przeglądarce): TYLKO `mem` — sled NIE kompiluje się
//     na wasm (fs2/libc/mmap), persystencja w przeglądarce odłożona (snapshot/
//     export). To NIE jest „sled na wasm".
//
// DLACZEGO sled, nie sqlite na natywnym: cozo backend `sqlite` ciągnie crate
// `sqlite` → `sqlite3-src` z `links="sqlite3"`, co koliduje z naszym `rusqlite`
// (`bundled`, `libsqlite3-sys`, też `links="sqlite3"`). Cargo zabrania dwóch
// dostawców tego samego `links`, więc cozo-sqlite NIE może współistnieć z
// rdzeniem. sled w Cozo jest oznaczony jako *Experimental* — zaakceptowany
// świadomie, z idle-close uchwytów (`GraphManager`), bo Cozo NIE przepuszcza
// konfiguracji sled: `DbInstance::new("sled", path, options)` woła
// `sled::open(path)` i IGNORUJE `options` (potwierdzone w `cozo-0.7.6/src/lib.rs`
// i `storage/sled.rs`), a `SledStorage.db` jest prywatny — nie da się wstrzyknąć
// `sled::Config { cache_capacity, flush_every_ms }`. sled domyślnie bierze 1 GiB
// cache + flush 500ms na KAŻDĄ otwartą bazę, więc zamiast tuningu configu
// ograniczamy LICZBĘ jednocześnie otwartych baz (LRU/idle-close w
// `collection.rs`, cap `MAX_OPEN_GRAPHS`) — to jedyna realna dźwignia pamięci
// przy setkach kolekcji na telefonie.
//
// Schemat per kolekcja (relacje Cozo, tworzone leniwie przy `open_or_create`):
//   nodes { id: String => label, props, provenance, ts }
//   edges { src: String, rel: String, dst: String => props, weight, provenance, ts }
//
// Wewnętrzne, HOST-BUDOWANE zapytania read-only (neighbors/pagerank/export_csr/
// tombstone-list) idą przez `ScriptMutability::Immutable`; mutacje (upsert węzła/
// krawędzi) przez `ScriptMutability::Mutable`. `export_edges` zrzuca
// (src,dst,weight) jako ważony CSR pod liczenie PPR w Rust (`ppr.rs`).
//
// BRAK RAW DATALOG OD ADDONA (finalny rework B1+B2): surowy `graph_query` został
// usunięty z Etapu 0 — addon nie podaje skryptu. Wszystkie zapytania tutaj buduje
// HOST z bezpiecznych prymitywów (stała struktura, parametry `$name`), więc nie
// potrzebują sandbox-watchdoga ani wstrzykiwanego `:timeout` per-zapytanie:
// `run_query`/`run_query_params` to prosta, synchroniczna ścieżka `run_script
// Immutable`. Budżet ciężkich prymitywów egzekwuje warstwa host-fn (clamp
// iteracji/seedów/limitu + cap współbieżności in-flight).
//
// SOFT-DELETE (korekta B1+B2, CRIT #3/#4): delete to TYLKO tombstone (`:put`
// markera O(1)). Stary hard-delete przez `:replace` całej relacji (O(N+E),
// nieatomowy, obchodził bug `:rm` na sled) został usunięty. Węzły tombstone ORAZ
// ich incydentne krawędzie są wykluczane ze WSZYSTKICH ścieżek retrievalu
// (neighbors/pagerank/export_csr/query) — fizyczny purge to późniejsza kompakcja.

use std::collections::BTreeMap;
use std::path::Path;

use cozo::{DbInstance, NamedRows, ScriptMutability};

use super::csr::Csr;
use super::error::{GraphError, Result};

/// Marker label węzła soft-deleted (tombstone). Retrieval filtruje węzły o tym
/// labelu; wybrany tak, by nie kolidował z realnym labelem ekstrakcji.
pub const TOMBSTONE_LABEL: &str = "__tombstone__";

/// Kierunek trawersacji krawędzi dla `neighbors`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NeighborDir {
    /// Krawędzie wychodzące (`src == node`).
    Out,
    /// Krawędzie wchodzące (`dst == node`).
    In,
    /// Oba kierunki.
    Both,
}

/// Silnik storage CozoDB. `Sled` natywnie (czysto-Rust embedded KV, NIE wasm);
/// `Mem` na wasm32 (jedyny działający backend w przeglądarce, ulotny); `RocksDb`
/// tylko gdy zbudowano z feature `graph-rocksdb` (serwer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphEngine {
    Sled,
    RocksDb,
    Mem,
}

impl GraphEngine {
    /// Nazwa silnika w formie tekstowej trzymanej w kolumnie DB `engine`
    /// oraz przekazywanej do `DbInstance::new`.
    pub fn as_str(self) -> &'static str {
        match self {
            GraphEngine::Sled => "sled",
            GraphEngine::RocksDb => "rocksdb",
            GraphEngine::Mem => "mem",
        }
    }

    /// Parsuje wartość z kolumny `engine`. Nieznana wartość => `None`.
    pub fn parse(s: &str) -> Option<GraphEngine> {
        match s {
            "sled" => Some(GraphEngine::Sled),
            "rocksdb" => Some(GraphEngine::RocksDb),
            "mem" => Some(GraphEngine::Mem),
            _ => None,
        }
    }

    /// Domyślny engine dla tej platformy/buildu. Na wasm32 wyłącznie `mem`
    /// (sled nie kompiluje się na wasm). Natywnie RocksDB gdy wkompilowany
    /// feature, inaczej sled (NIE błąd — świadomy fallback wg projektu 0.1).
    pub fn default_for_build() -> GraphEngine {
        #[cfg(target_arch = "wasm32")]
        {
            GraphEngine::Mem
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            if cfg!(feature = "graph-rocksdb") {
                GraphEngine::RocksDb
            } else {
                GraphEngine::Sled
            }
        }
    }

    /// Czy ten build/platforma potrafi otworzyć daną kolekcję `engine`. RocksDB
    /// wymaga feature; sled wymaga platformy nie-wasm; mem zawsze dostępny.
    fn is_available(self) -> bool {
        match self {
            GraphEngine::Mem => true,
            GraphEngine::Sled => !cfg!(target_arch = "wasm32"),
            GraphEngine::RocksDb => {
                cfg!(feature = "graph-rocksdb") && !cfg!(target_arch = "wasm32")
            }
        }
    }
}

/// Kontrakt silnika grafowego. Trzymany za `dyn` w `GraphManager`, żeby (jak w
/// vector) móc w przyszłości wpiąć alternatywny backend bez zmiany managera.
pub trait GraphBackend: Send + Sync {
    /// Liczba węzłów (relacja `nodes`).
    fn node_count(&self) -> Result<u64>;

    /// Liczba krawędzi (relacja `edges`).
    fn edge_count(&self) -> Result<u64>;

    /// Upsert węzła. `props`/`provenance` to surowe stringi JSON (warstwa
    /// host-fn serializuje strukturę addona); pusty => `"{}"` / `"null"`.
    fn upsert_node(&self, id: &str, label: &str, props_json: &str, provenance_json: &str)
        -> Result<()>;

    /// Upsert krawędzi skierowanej `src -[rel]-> dst` z wagą.
    fn upsert_edge(
        &self,
        src: &str,
        rel: &str,
        dst: &str,
        weight: f64,
        props_json: &str,
        provenance_json: &str,
    ) -> Result<()>;

    /// Czy węzeł o danym `id` istnieje. Zapytanie parametryzowane (`$id`) —
    /// żadnego `format!()` z danych addona (poprawka codex pkt 4).
    fn node_exists(&self, id: &str) -> Result<bool>;

    /// Czy krawędź `(src, rel, dst)` istnieje. Parametryzowane (`$src/$rel/$dst`).
    fn edge_exists(&self, src: &str, rel: &str, dst: &str) -> Result<bool>;

    /// Read-only zapytanie Cozo budowane przez HOST (`Immutable`). Addon NIE
    /// podaje skryptu — to wewnętrzna ścieżka (pagerank/tombstone-list/export);
    /// prosta, synchroniczna, bez watchdoga.
    fn run_query(&self, script: &str) -> Result<NamedRows>;

    /// Read-only zapytanie Cozo budowane przez HOST z PARAMETRAMI (`Immutable`).
    /// Parametry wiązane jako `$name` (host buduje stałą strukturę, np. neighbors).
    fn run_query_params(
        &self,
        script: &str,
        params: BTreeMap<String, cozo::DataValue>,
    ) -> Result<NamedRows>;

    /// Mutowalny skrypt (transakcja) — używany wewnętrznie przez upserty.
    fn run_tx(&self, script: &str) -> Result<NamedRows>;

    /// Sąsiedzi węzła `node` w kierunku `direction` (out/in/both), opcjonalnie
    /// filtrowani po relacji `rel`, ograniczeni do `limit` wierszy. Zwraca
    /// trójki `(id, rel, weight)`. Zapytanie parametryzowane (`$node`/`$rel`).
    fn neighbors(
        &self,
        node: &str,
        direction: NeighborDir,
        rel: Option<&str>,
        limit: u32,
    ) -> Result<Vec<(String, String, f64)>>;

    /// Soft-delete węzła `id` (tombstone, O(1) `:put`): zostawia wiersz, ustawia
    /// label na marker tombstone i zeruje props/provenance. Wiersz krawędzi
    /// nietknięty (łańcuch provenance żyje), ale retrieval wyklucza krawędzie
    /// incydentne do tombstone'owanego węzła. Zwraca `true`, gdy węzeł istniał.
    fn delete_node(&self, id: &str) -> Result<bool>;

    /// Soft-delete pojedynczej krawędzi `(src, rel, dst)` (O(1) `:put alive=false`).
    /// Wiersz zostaje, ale retrieval go pomija; ponowny upsert tej krawędzi ją
    /// ożywia. Zwraca `true`, gdy krawędź istniała.
    fn delete_edge(&self, src: &str, rel: &str, dst: &str) -> Result<bool>;

    /// Eksport krawędzi do CSR (offsets+targets) nad spójnym snapshotem grafu —
    /// wejście dla PPR w Rust. Zwraca też listę id węzłów (indeks CSR -> id).
    fn export_edges(&self) -> Result<Csr>;
}

/// Implementacja oparta o `cozo::DbInstance` (jeden plik = jedna kolekcja).
pub struct CozoBackend {
    db: DbInstance,
    engine: GraphEngine,
}

impl CozoBackend {
    /// Otwiera (lub tworzy) plik kolekcji pod `path` na silniku `engine`.
    /// Przy pierwszym utworzeniu zakłada schemat (`:create nodes/edges`); przy
    /// ponownym otwarciu `:create` na istniejącej relacji zwraca błąd Cozo,
    /// który tu traktujemy jako „schemat już jest" i kontynuujemy.
    pub fn open_or_create(path: &Path, engine: GraphEngine) -> Result<Self> {
        if !engine.is_available() {
            return Err(GraphError::Backend(format!(
                "graph engine '{}' is not available on this build/platform \
                 (rocksdb needs the 'graph-rocksdb' feature; sled is unavailable on wasm32)",
                engine.as_str()
            )));
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| GraphError::Io {
                path: Some(parent.to_path_buf()),
                source: e,
            })?;
        }

        let db = DbInstance::new(engine.as_str(), path, "")
            .map_err(|e| GraphError::Backend(format!("cozo open failed: {e}")))?;

        let backend = CozoBackend { db, engine };
        backend.ensure_schema()?;
        Ok(backend)
    }

    /// Otwiera ulotną kolekcję w pamięci (`mem` backend) — używane w testach
    /// oraz na wasm32 (jedyny działający backend w przeglądarce). Engine
    /// raportowany jako `Mem` (ulotny, bez plików).
    pub fn open_in_memory() -> Result<Self> {
        let db = DbInstance::new("mem", "", "")
            .map_err(|e| GraphError::Backend(format!("cozo mem open failed: {e}")))?;
        let backend = CozoBackend {
            db,
            engine: GraphEngine::Mem,
        };
        backend.ensure_schema()?;
        Ok(backend)
    }

    pub fn engine(&self) -> GraphEngine {
        self.engine
    }

    /// Mapuje błąd Cozo na `GraphError::Datalog`. Zapytania buduje host, więc błąd
    /// oznacza wadę po stronie hosta (regresja), nie złośliwe wejście addona.
    fn map_query_err(e: impl std::fmt::Display) -> GraphError {
        GraphError::Datalog(e.to_string())
    }

    /// Uruchamia HOST-budowane read-only zapytanie Cozo synchronicznie
    /// (`Immutable`). Bez watchdoga/`:timeout` — addon nie podaje skryptu, a koszt
    /// ciężkich prymitywów (pagerank/ppr) jest ograniczony w warstwie host-fn
    /// (clamp iteracji/seedów + cap współbieżności in-flight).
    fn run_immutable(
        &self,
        script: &str,
        params: BTreeMap<String, cozo::DataValue>,
    ) -> Result<NamedRows> {
        self.db
            .run_script(script, params, ScriptMutability::Immutable)
            .map_err(Self::map_query_err)
    }

    /// Zakłada relacje `nodes` i `edges`, jeśli jeszcze nie istnieją. `:create`
    /// na istniejącej relacji w Cozo to błąd — wykrywamy istnienie przez
    /// `::relations` i tworzymy tylko brakujące, żeby ponowne otwarcie pliku
    /// nie failowało.
    fn ensure_schema(&self) -> Result<()> {
        let existing = self.list_relations()?;
        if !existing.iter().any(|r| r == "nodes") {
            self.run_tx(
                r"
                :create nodes {
                    id: String
                    =>
                    label: String default '',
                    props: String default '{}',
                    provenance: String default 'null',
                    ts: Float default 0.0,
                }
                ",
            )?;
        }
        if !existing.iter().any(|r| r == "edges") {
            // `alive` (default true) to flaga soft-delete krawędzi: delete krawędzi
            // ustawia `alive=false` przez `:put` (O(1), ten sam klucz src/rel/dst),
            // a retrieval filtruje `alive == true`. Lustro tombstone węzła (label),
            // ale krawędź nie ma labela, więc trzyma osobny marker.
            self.run_tx(
                r"
                :create edges {
                    src: String,
                    rel: String,
                    dst: String,
                    =>
                    weight: Float default 1.0,
                    props: String default '{}',
                    provenance: String default 'null',
                    ts: Float default 0.0,
                    alive: Bool default true,
                }
                ",
            )?;
        }
        Ok(())
    }

    /// Lista nazw relacji w tej bazie (`::relations` sys-op — wywoływane tylko
    /// przez host, nigdy przez skrypt addona).
    fn list_relations(&self) -> Result<Vec<String>> {
        let rows = match self
            .db
            .run_script("::relations", BTreeMap::new(), ScriptMutability::Immutable)
        {
            Ok(r) => r,
            // Świeża baza bez żadnych relacji może zwrócić błąd zamiast pustego
            // zbioru — traktujemy jak „brak relacji".
            Err(_) => return Ok(Vec::new()),
        };
        let name_idx = rows.headers.iter().position(|h| h == "name");
        let Some(idx) = name_idx else {
            return Ok(Vec::new());
        };
        Ok(rows
            .rows
            .iter()
            .filter_map(|row| row.get(idx).and_then(|v| v.get_str()).map(str::to_string))
            .collect())
    }

    /// Liczy wiersze relacji `rel` przez agregację `count` w GŁOWIE reguły
    /// (poprawna forma agregacji Cozo — agregat w nagłówku grupuje po pozostałych
    /// zmiennych; bez innych zmiennych liczy całość). `count` w ciele reguły
    /// (`total = count(id)`) NIE istnieje w Cozo 0.7.6.
    fn count_relation(&self, rel: &str) -> Result<u64> {
        let count_script = match rel {
            "nodes" => "?[count(id)] := *nodes{id}",
            "edges" => "?[count(src)] := *edges{src, rel, dst}",
            _ => return Err(GraphError::Backend(format!("unknown relation {rel}"))),
        };
        // Wewnętrzny, mały count w ścieżce zapisu (reconcile/quota) — synchroniczny
        // `Immutable`, jak pozostałe host-budowane zapytania.
        let rows = self
            .db
            .run_script(count_script, BTreeMap::new(), ScriptMutability::Immutable)
            .map_err(Self::map_query_err)?;
        let total = rows
            .rows
            .first()
            .and_then(|r| r.first())
            .and_then(|v| v.get_int())
            .unwrap_or(0);
        Ok(total.max(0) as u64)
    }

}

impl GraphBackend for CozoBackend {
    fn node_count(&self) -> Result<u64> {
        self.count_relation("nodes")
    }

    fn edge_count(&self) -> Result<u64> {
        self.count_relation("edges")
    }

    fn upsert_node(
        &self,
        id: &str,
        label: &str,
        props_json: &str,
        provenance_json: &str,
    ) -> Result<()> {
        let mut params = BTreeMap::new();
        params.insert("id".to_string(), cozo::DataValue::from(id));
        params.insert("label".to_string(), cozo::DataValue::from(label));
        params.insert("props".to_string(), cozo::DataValue::from(props_json));
        params.insert(
            "provenance".to_string(),
            cozo::DataValue::from(provenance_json),
        );
        params.insert("ts".to_string(), cozo::DataValue::from(now_ts()));
        self.db
            .run_script(
                r"
                ?[id, label, props, provenance, ts] <- [[$id, $label, $props, $provenance, $ts]]
                :put nodes {id => label, props, provenance, ts}
                ",
                params,
                ScriptMutability::Mutable,
            )
            .map_err(|e| GraphError::Datalog(e.to_string()))?;
        Ok(())
    }

    fn upsert_edge(
        &self,
        src: &str,
        rel: &str,
        dst: &str,
        weight: f64,
        props_json: &str,
        provenance_json: &str,
    ) -> Result<()> {
        let mut params = BTreeMap::new();
        params.insert("src".to_string(), cozo::DataValue::from(src));
        params.insert("rel".to_string(), cozo::DataValue::from(rel));
        params.insert("dst".to_string(), cozo::DataValue::from(dst));
        params.insert("weight".to_string(), cozo::DataValue::from(weight));
        params.insert("props".to_string(), cozo::DataValue::from(props_json));
        params.insert(
            "provenance".to_string(),
            cozo::DataValue::from(provenance_json),
        );
        params.insert("ts".to_string(), cozo::DataValue::from(now_ts()));
        // Upsert ożywia krawędź (`alive=true`): ponowny zapis tej samej krawędzi
        // po jej soft-delete przywraca ją do retrievalu.
        self.db
            .run_script(
                r"
                ?[src, rel, dst, weight, props, provenance, ts, alive] <-
                    [[$src, $rel, $dst, $weight, $props, $provenance, $ts, true]]
                :put edges {src, rel, dst => weight, props, provenance, ts, alive}
                ",
                params,
                ScriptMutability::Mutable,
            )
            .map_err(|e| GraphError::Datalog(e.to_string()))?;
        Ok(())
    }

    fn node_exists(&self, id: &str) -> Result<bool> {
        let mut params = BTreeMap::new();
        params.insert("id".to_string(), cozo::DataValue::from(id));
        let rows = self
            .db
            .run_script(
                "?[id] := *nodes{id}, id == $id\n:limit 1",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| GraphError::Datalog(e.to_string()))?;
        Ok(!rows.rows.is_empty())
    }

    fn edge_exists(&self, src: &str, rel: &str, dst: &str) -> Result<bool> {
        let mut params = BTreeMap::new();
        params.insert("src".to_string(), cozo::DataValue::from(src));
        params.insert("rel".to_string(), cozo::DataValue::from(rel));
        params.insert("dst".to_string(), cozo::DataValue::from(dst));
        let rows = self
            .db
            .run_script(
                "?[src] := *edges{src, rel, dst}, src == $src, rel == $rel, dst == $dst\n:limit 1",
                params,
                ScriptMutability::Immutable,
            )
            .map_err(|e| GraphError::Datalog(e.to_string()))?;
        Ok(!rows.rows.is_empty())
    }

    fn run_query(&self, script: &str) -> Result<NamedRows> {
        self.run_immutable(script, BTreeMap::new())
    }

    fn run_query_params(
        &self,
        script: &str,
        params: BTreeMap<String, cozo::DataValue>,
    ) -> Result<NamedRows> {
        self.run_immutable(script, params)
    }

    fn run_tx(&self, script: &str) -> Result<NamedRows> {
        self.db
            .run_script(script, BTreeMap::new(), ScriptMutability::Mutable)
            .map_err(|e| GraphError::Datalog(e.to_string()))
    }

    fn neighbors(
        &self,
        node: &str,
        direction: NeighborDir,
        rel: Option<&str>,
        limit: u32,
    ) -> Result<Vec<(String, String, f64)>> {
        let mut params = BTreeMap::new();
        params.insert("node".to_string(), cozo::DataValue::from(node));
        // Filtr relacji egzekwowany przez regułę z `$rel`; gdy brak filtra,
        // przekazujemy pusty string i pomijamy warunek w wariancie bez filtra.
        let rel_clause = if let Some(r) = rel {
            params.insert("rel".to_string(), cozo::DataValue::from(r));
            ", rel == $rel"
        } else {
            ""
        };

        // Tombstone węzłów-sąsiadów odfiltrowany przez join z *nodes (label),
        // martwe krawędzie (`alive=false`) odfiltrowane przez `alive == true`.
        // Filtrujemy też tombstone NA SAMYM `$node` przez join — sąsiedztwo
        // węzła tombstone jest puste (korekta B1+B2).
        let script = match direction {
            NeighborDir::Out => format!(
                "?[nbr, rel, weight] := *edges{{src, rel, dst: nbr, weight, alive}}, src == $node{rel_clause}, alive == true, \
                 *nodes{{id: src, label: ls}}, ls != '{TOMBSTONE_LABEL}', \
                 *nodes{{id: nbr, label}}, label != '{TOMBSTONE_LABEL}'\n:limit {limit}"
            ),
            NeighborDir::In => format!(
                "?[nbr, rel, weight] := *edges{{src: nbr, rel, dst, weight, alive}}, dst == $node{rel_clause}, alive == true, \
                 *nodes{{id: dst, label: ld}}, ld != '{TOMBSTONE_LABEL}', \
                 *nodes{{id: nbr, label}}, label != '{TOMBSTONE_LABEL}'\n:limit {limit}"
            ),
            NeighborDir::Both => format!(
                "out[nbr, rel, weight] := *edges{{src, rel, dst: nbr, weight, alive}}, src == $node{rel_clause}, alive == true, *nodes{{id: src, label: ls}}, ls != '{TOMBSTONE_LABEL}'\n\
                 inn[nbr, rel, weight] := *edges{{src: nbr, rel, dst, weight, alive}}, dst == $node{rel_clause}, alive == true, *nodes{{id: dst, label: ld}}, ld != '{TOMBSTONE_LABEL}'\n\
                 ?[nbr, rel, weight] := out[nbr, rel, weight], *nodes{{id: nbr, label}}, label != '{TOMBSTONE_LABEL}'\n\
                 ?[nbr, rel, weight] := inn[nbr, rel, weight], *nodes{{id: nbr, label}}, label != '{TOMBSTONE_LABEL}'\n\
                 :limit {limit}"
            ),
        };

        let rows = self.run_query_params(&script, params)?;
        let mut out = Vec::with_capacity(rows.rows.len());
        for r in &rows.rows {
            let (Some(id), Some(rel)) = (
                r.first().and_then(|v| v.get_str()),
                r.get(1).and_then(|v| v.get_str()),
            ) else {
                continue;
            };
            let w = r
                .get(2)
                .and_then(|v| v.get_float().or_else(|| v.get_int().map(|i| i as f64)))
                .unwrap_or(1.0);
            out.push((id.to_string(), rel.to_string(), w));
        }
        Ok(out)
    }

    fn delete_node(&self, id: &str) -> Result<bool> {
        if !self.node_exists(id)? {
            return Ok(false);
        }
        // Soft-delete (tombstone) O(1): nadpisz wiersz markerem labela i wyzeruj
        // props/provenance. Krawędzie incydentne odpadają z retrievalu przez join
        // z `*nodes` na nie-tombstone (patrz `neighbors`/`export_edges`). Stary
        // hard-delete przez `:replace` całej relacji (O(N+E), bug `:rm` na sled)
        // usunięty — fizyczny purge to późniejsza kompakcja.
        let mut params = BTreeMap::new();
        params.insert("id".to_string(), cozo::DataValue::from(id));
        params.insert("label".to_string(), cozo::DataValue::from(TOMBSTONE_LABEL));
        params.insert("ts".to_string(), cozo::DataValue::from(now_ts()));
        self.db
            .run_script(
                r"
                ?[id, label, props, provenance, ts] <- [[$id, $label, '{}', 'null', $ts]]
                :put nodes {id => label, props, provenance, ts}
                ",
                params,
                ScriptMutability::Mutable,
            )
            .map_err(|e| GraphError::Datalog(e.to_string()))?;
        Ok(true)
    }

    fn delete_edge(&self, src: &str, rel: &str, dst: &str) -> Result<bool> {
        // Idempotencja: krawędź nieistniejąca LUB już martwa → `false` (nic do
        // zrobienia). Sprawdzamy `alive == true` na tym kluczu.
        let mut probe = BTreeMap::new();
        probe.insert("src".to_string(), cozo::DataValue::from(src));
        probe.insert("rel".to_string(), cozo::DataValue::from(rel));
        probe.insert("dst".to_string(), cozo::DataValue::from(dst));
        let alive = self.db.run_script(
            "?[src] := *edges{src, rel, dst, alive}, src == $src, rel == $rel, dst == $dst, alive == true\n:limit 1",
            probe,
            ScriptMutability::Immutable,
        ).map_err(|e| GraphError::Datalog(e.to_string()))?;
        if alive.rows.is_empty() {
            return Ok(false);
        }
        // Soft-delete O(1): ustaw `alive=false` na tym samym kluczu (src,rel,dst).
        // Retrieval filtruje `alive == true`; ponowny upsert ożywia krawędź.
        let mut params = BTreeMap::new();
        params.insert("src".to_string(), cozo::DataValue::from(src));
        params.insert("rel".to_string(), cozo::DataValue::from(rel));
        params.insert("dst".to_string(), cozo::DataValue::from(dst));
        params.insert("ts".to_string(), cozo::DataValue::from(now_ts()));
        self.db
            .run_script(
                r"
                ?[src, rel, dst, weight, props, provenance, ts, alive] :=
                    *edges{src, rel, dst, weight, props, provenance},
                    src == $src, rel == $rel, dst == $dst, ts = $ts, alive = false
                :put edges {src, rel, dst => weight, props, provenance, ts, alive}
                ",
                params,
                ScriptMutability::Mutable,
            )
            .map_err(|e| GraphError::Datalog(e.to_string()))?;
        Ok(true)
    }

    fn export_edges(&self) -> Result<Csr> {
        // Lista węzłów NIE-tombstone (deterministyczny porządek -> stabilny indeks
        // CSR). Węzły tombstone wypadają z grafu PPR/PageRank.
        let node_rows = self.run_query(&format!(
            "?[id] := *nodes{{id, label}}, label != '{TOMBSTONE_LABEL}'\n:order id"
        ))?;
        let ids: Vec<String> = node_rows
            .rows
            .iter()
            .filter_map(|r| r.first().and_then(|v| v.get_str()).map(str::to_string))
            .collect();

        // Krawędzie żywe (`alive`) między nie-tombstone węzłami, posortowane po src
        // dla lokalności budowy CSR; waga niesiona do CSR pod ważony PPR. Join z
        // `*nodes` po obu końcach wyklucza krawędzie incydentne do tombstone.
        let edge_rows = self.run_query(&format!(
            "?[src, dst, weight] := *edges{{src, dst, weight, alive}}, alive == true, \
             *nodes{{id: src, label: ls}}, ls != '{TOMBSTONE_LABEL}', \
             *nodes{{id: dst, label: ld}}, ld != '{TOMBSTONE_LABEL}'\n:order src"
        ))?;
        let mut triples: Vec<(String, String, f64)> = Vec::with_capacity(edge_rows.rows.len());
        for r in &edge_rows.rows {
            let (Some(s), Some(d)) = (
                r.first().and_then(|v| v.get_str()),
                r.get(1).and_then(|v| v.get_str()),
            ) else {
                continue;
            };
            // Waga może przyjść jako Float lub Int (literał `1`); brak/null => 1.0.
            let w = r
                .get(2)
                .and_then(|v| v.get_float().or_else(|| v.get_int().map(|i| i as f64)))
                .unwrap_or(1.0);
            triples.push((s.to_string(), d.to_string(), w));
        }
        Ok(Csr::from_edges(ids, &triples))
    }
}

/// Znacznik czasu w sekundach (epoch, f64) — zapis do kolumny `ts`.
fn now_ts() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}
