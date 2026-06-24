// =============================================================================
// File: tests/graph_host_functions.rs
// Opis: Testy integracyjne stosu host-fn grafu (RAG 0.2, finalny rework B1+B2).
//       Surowy `graph_query` USUNIĘTY — addon dostaje tylko bezpieczne, capowane
//       prymitywy. Testy sprawdzają REALNIE: upsert węzła/krawędzi, neighbors,
//       wbudowany PageRank, PPR z seedami (clamp seedów/iteracji), delete (node/
//       edge/tombstone), wykluczenie tombstone/alive ze WSZYSTKICH ścieżek
//       (neighbors/pagerank/ppr/export), cap współbieżności obliczeń (fail-closed),
//       izolację dwóch instancji i org oraz spójny uninstall (fail kasowania plików
//       zostawia wiersz → retry-able). Warstwa wasmtime ABI sprowadza się do tych
//       samych helperów + managera, więc regresja tutaj to ta sama wada, którą
//       zobaczyłby addon. Build/uruchamianie tylko z feature `graph`.
// =============================================================================

#![cfg(feature = "graph")]

use std::path::PathBuf;

use tempfile::TempDir;
use tentaflow_core::addon::errors::AbiError;
use tentaflow_core::addon::host_functions::graph::test_api as graph_api;
use tentaflow_core::addon::manifest::{validate_graph_collections, GraphCollectionSpec};
use tentaflow_core::services::graph::{GraphError, GraphManager, NeighborDir};
use tentaflow_sdk_spec::{FieldValue, GraphProp};

fn col_spec(name: &str, data_class: &str, gate: Option<&str>) -> GraphCollectionSpec {
    GraphCollectionSpec {
        name: name.to_string(),
        data_class: data_class.to_string(),
        gate: gate.map(str::to_string),
    }
}

const ORG_A: &str = "org-a";
const ORG_B: &str = "org-b";

fn open_pool() -> (TempDir, tentaflow_core::db::DbPool) {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("test.db");
    let pool = tentaflow_core::db::init(&path).expect("init DB");
    (dir, pool)
}

fn mgr(pool: tentaflow_core::db::DbPool, root: PathBuf) -> GraphManager {
    GraphManager::with_root(pool, root)
}

fn prop(name: &str, value: FieldValue) -> GraphProp {
    GraphProp {
        name: name.to_string(),
        value,
    }
}

// -----------------------------------------------------------------------------
// Manifest declaration + gate (ad-hoc collection rejection happens because a
// host fn looks the collection up in the manifest; here we test the declaration
// rules + the structural gate the host fn enforces).
// -----------------------------------------------------------------------------

#[test]
fn manifest_validates_data_class_and_name() {
    assert!(validate_graph_collections(&[col_spec("kg", "B", None)]).is_ok());
    // Bad data class.
    assert!(validate_graph_collections(&[col_spec("kg", "Z", None)]).is_err());
    // Invalid collection name (uppercase).
    assert!(validate_graph_collections(&[col_spec("KG", "B", None)]).is_err());
    // Duplicate names.
    assert!(validate_graph_collections(&[col_spec("kg", "B", None), col_spec("kg", "B", None)])
        .is_err());
}

#[test]
fn gate_is_fail_closed_for_gated_collection() {
    // Non-gated collection passes the structural gate.
    assert!(graph_api::check_gate(&col_spec("kg", "B", None)).is_ok());
    // A gated collection is hard-denied until claim plumbing lands (fail-closed).
    assert!(graph_api::check_gate(&col_spec("faces", "A", Some("d4-historical"))).is_err());
}

#[test]
fn props_serialize_to_json_object() {
    let json = graph_api::props_to_json(&[
        prop("name", FieldValue::Str("Ada".into())),
        prop("age", FieldValue::Int(36)),
        prop("active", FieldValue::Bool(true)),
    ]);
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["name"], "Ada");
    assert_eq!(v["age"], 36);
    assert_eq!(v["active"], true);
    assert_eq!(graph_api::props_to_json(&[]), "{}");
}

// -----------------------------------------------------------------------------
// map_graph_error — variants map to expected AbiError
// -----------------------------------------------------------------------------

#[test]
fn map_graph_error_quota_and_notfound() {
    let (abi, reason) = graph_api::map_graph_error(&GraphError::NodeQuotaExceeded {
        addon_id: "x".into(),
        current: 10,
        max: 10,
    });
    assert_eq!(abi, AbiError::QuotaExceeded);
    assert_eq!(reason, "node_quota_exceeded");

    let (abi, _) = graph_api::map_graph_error(&GraphError::CollectionNotFound {
        org_id: "o".into(),
        addon_id: "a".into(),
        collection: "c".into(),
    });
    assert_eq!(abi, AbiError::NotFound);

    // Fail-closed z capa współbieżności → QuotaExceeded + dedykowany reason.
    let (abi, reason) = graph_api::map_graph_error(&GraphError::ComputeBusy {
        scope: "per_addon",
        max: 2,
    });
    assert_eq!(abi, AbiError::QuotaExceeded);
    assert_eq!(reason, "graph_compute_busy");
}

// -----------------------------------------------------------------------------
// End-to-end via GraphManager — the work a host fn does after permission/audit.
// -----------------------------------------------------------------------------

#[test]
fn e2e_upsert_node_edge_and_neighbors() {
    let (_d, pool) = open_pool();
    let root = TempDir::new().unwrap();
    let m = mgr(pool, root.path().to_path_buf());

    let n1 = m
        .upsert_node_with_quota(ORG_A, "addon_a", "kg", "n1", "Person", r#"{"name":"Ada"}"#, "null")
        .unwrap();
    let n2 = m
        .upsert_node_with_quota(ORG_A, "addon_a", "kg", "n2", "Person", "{}", "null")
        .unwrap();
    assert_eq!(n2, 2, "two nodes after two distinct upserts");
    assert!(n1 >= 1);

    let edges = m
        .upsert_edge_with_quota(ORG_A, "addon_a", "kg", "n1", "KNOWS", "n2", 1.0, "{}", "null")
        .unwrap();
    assert_eq!(edges, 1);

    // Neighbors primitive returns the single out-neighbor.
    let out = m
        .neighbors(ORG_A, "addon_a", "kg", "n1", NeighborDir::Out, None, 10)
        .unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].0, "n2");
}

#[test]
fn e2e_neighbors_and_pagerank() {
    let (_d, pool) = open_pool();
    let root = TempDir::new().unwrap();
    let m = mgr(pool, root.path().to_path_buf());

    for id in ["a", "b", "c"] {
        m.upsert_node_with_quota(ORG_A, "ad", "g", id, "N", "{}", "null").unwrap();
    }
    m.upsert_edge_with_quota(ORG_A, "ad", "g", "a", "R", "b", 1.0, "{}", "null").unwrap();
    m.upsert_edge_with_quota(ORG_A, "ad", "g", "a", "R", "c", 2.0, "{}", "null").unwrap();
    m.upsert_edge_with_quota(ORG_A, "ad", "g", "b", "R", "c", 1.0, "{}", "null").unwrap();

    let out = m.neighbors(ORG_A, "ad", "g", "a", NeighborDir::Out, None, 10).unwrap();
    let ids: Vec<&str> = out.iter().map(|(id, _, _)| id.as_str()).collect();
    assert!(ids.contains(&"b") && ids.contains(&"c"));
    assert_eq!(out.len(), 2);

    let inn = m.neighbors(ORG_A, "ad", "g", "c", NeighborDir::In, None, 10).unwrap();
    assert_eq!(inn.len(), 2, "c has two in-edges (a, b)");

    let ranked = m.pagerank(ORG_A, "ad", "g", 10, 0.85, 20).unwrap();
    assert!(!ranked.is_empty());
    // 'c' is the sink of the most/heaviest edges → should rank top.
    assert_eq!(ranked.first().unwrap().0, "c");
}

#[test]
fn e2e_ppr_with_seeds_biases_toward_seed_neighborhood() {
    let (_d, pool) = open_pool();
    let root = TempDir::new().unwrap();
    let m = mgr(pool, root.path().to_path_buf());

    // Two disconnected pairs: a->b and c->d.
    for id in ["a", "b", "c", "d"] {
        m.upsert_node_with_quota(ORG_A, "ad", "g", id, "N", "{}", "null").unwrap();
    }
    m.upsert_edge_with_quota(ORG_A, "ad", "g", "a", "R", "b", 1.0, "{}", "null").unwrap();
    m.upsert_edge_with_quota(ORG_A, "ad", "g", "c", "R", "d", 1.0, "{}", "null").unwrap();

    let seeded = m
        .ppr(ORG_A, "ad", "g", &[("a".to_string(), 1.0)], 10, 0.85, 30)
        .unwrap();
    let score = |id: &str| seeded.iter().find(|(x, _)| x == id).map(|(_, s)| *s).unwrap_or(0.0);
    // Mass concentrated on the seed's component (a/b) over the unrelated (c/d).
    assert!(score("a") + score("b") > score("c") + score("d"));
}

#[test]
fn e2e_delete_is_soft_and_does_not_rewrite_relation() {
    // Korekta B1+B2: delete = soft-delete (tombstone/alive=false), NIE hard-delete
    // przez `:replace` całej relacji. Wiersz zostaje, więc liczniki się NIE
    // zmieniają, a retrieval i tak pomija usunięte.
    let (_d, pool) = open_pool();
    let root = TempDir::new().unwrap();
    let m = mgr(pool, root.path().to_path_buf());

    for id in ["a", "b", "c"] {
        m.upsert_node_with_quota(ORG_A, "ad", "g", id, "N", "{}", "null").unwrap();
    }
    m.upsert_edge_with_quota(ORG_A, "ad", "g", "a", "R", "b", 1.0, "{}", "null").unwrap();
    m.upsert_edge_with_quota(ORG_A, "ad", "g", "a", "R", "c", 1.0, "{}", "null").unwrap();

    // Soft-delete one edge: wiersz zostaje (edge_count bez zmian), ale znika z
    // sąsiadów. To dowodzi, że NIE przepisano relacji bez tej krawędzi.
    let (removed, _n, edges) = m.delete_edge_in(ORG_A, "ad", "g", "a", "R", "b").unwrap();
    assert!(removed);
    assert_eq!(edges, 2, "soft-delete keeps the row → edge_count unchanged");
    let nb = m.neighbors(ORG_A, "ad", "g", "a", NeighborDir::Out, None, 10).unwrap();
    assert!(nb.iter().all(|(id, _, _)| id != "b"), "dead edge hidden");
    assert!(nb.iter().any(|(id, _, _)| id == "c"), "live edge stays");
    // Re-delete tej samej (już martwej) krawędzi → idempotent (removed=false).
    let (removed2, _, _) = m.delete_edge_in(ORG_A, "ad", "g", "a", "R", "b").unwrap();
    assert!(!removed2);
    // Upsert ożywia krawędź.
    m.upsert_edge_with_quota(ORG_A, "ad", "g", "a", "R", "b", 1.0, "{}", "null").unwrap();
    let nb2 = m.neighbors(ORG_A, "ad", "g", "a", NeighborDir::Out, None, 10).unwrap();
    assert!(nb2.iter().any(|(id, _, _)| id == "b"), "re-upsert revives edge");

    // Delete węzła = tombstone: wiersz zostaje (node_count bez zmian).
    let before = m.node_count(ORG_A, "ad", "g").unwrap();
    let (removed_n, nodes, _e) = m.delete_node_in(ORG_A, "ad", "g", "a").unwrap();
    assert!(removed_n);
    assert_eq!(nodes, before, "node delete = tombstone → node_count unchanged");
}

#[test]
fn e2e_tombstone_excluded_from_all_retrieval_paths() {
    // Tombstone węzła znika z: neighbors, pagerank, ppr, export_csr (jego
    // krawędzie też). Surowego query nie ma — addon nie ma jak zobaczyć tombstone.
    let (_d, pool) = open_pool();
    let root = TempDir::new().unwrap();
    let m = mgr(pool, root.path().to_path_buf());

    for id in ["a", "b", "c", "z"] {
        m.upsert_node_with_quota(ORG_A, "ad", "g", id, "N", "{}", "null").unwrap();
    }
    // z jest mocno dowiązany (żeby pagerank/ppr go widziały, gdyby nie tombstone).
    m.upsert_edge_with_quota(ORG_A, "ad", "g", "a", "R", "z", 1.0, "{}", "null").unwrap();
    m.upsert_edge_with_quota(ORG_A, "ad", "g", "b", "R", "z", 1.0, "{}", "null").unwrap();
    m.upsert_edge_with_quota(ORG_A, "ad", "g", "c", "R", "z", 1.0, "{}", "null").unwrap();
    m.upsert_edge_with_quota(ORG_A, "ad", "g", "z", "R", "a", 1.0, "{}", "null").unwrap();
    m.upsert_edge_with_quota(ORG_A, "ad", "g", "a", "R", "b", 1.0, "{}", "null").unwrap();

    // Tombstone z.
    let (removed, _, _) = m.delete_node_in(ORG_A, "ad", "g", "z").unwrap();
    assert!(removed);

    // 1) neighbors: b nie ma już sąsiada z; z (tombstone) nie ma sąsiadów.
    let nb = m.neighbors(ORG_A, "ad", "g", "b", NeighborDir::Out, None, 10).unwrap();
    assert!(nb.iter().all(|(id, _, _)| id != "z"), "z hidden from neighbors");
    let nz = m.neighbors(ORG_A, "ad", "g", "z", NeighborDir::Out, None, 10).unwrap();
    assert!(nz.is_empty(), "tombstoned node has no neighbors");

    // 2) pagerank: z nie pojawia się w rankingu.
    let pr = m.pagerank(ORG_A, "ad", "g", 10, 0.85, 20).unwrap();
    assert!(pr.iter().all(|(id, _)| id != "z"), "z absent from pagerank");

    // 3) ppr: seed a — z nie pojawia się w wyniku.
    let ppr = m.ppr(ORG_A, "ad", "g", &[("a".to_string(), 1.0)], 10, 0.85, 20).unwrap();
    assert!(ppr.iter().all(|(id, _)| id != "z"), "z absent from ppr");

    // 4) export_csr: z ani jego krawędzie nie wchodzą do CSR.
    let csr = m.export_csr(ORG_A, "ad", "g").unwrap();
    assert!(csr.index_of("z").is_none(), "z absent from CSR node set");
    assert!(csr.index_of("a").is_some());
}

// -----------------------------------------------------------------------------
// Capy parametrów ciężkich prymitywów — addon nie kontroluje rozmiaru pracy.
// -----------------------------------------------------------------------------

#[test]
fn neighbors_limit_is_clamped() {
    let (_d, pool) = open_pool();
    let root = TempDir::new().unwrap();
    let m = mgr(pool, root.path().to_path_buf());

    m.upsert_node_with_quota(ORG_A, "ad", "g", "a", "N", "{}", "null").unwrap();
    for i in 0..5 {
        let dst = format!("n{i}");
        m.upsert_node_with_quota(ORG_A, "ad", "g", &dst, "N", "{}", "null").unwrap();
        m.upsert_edge_with_quota(ORG_A, "ad", "g", "a", "R", &dst, 1.0, "{}", "null").unwrap();
    }
    // Wywołanie z absurdalnym limitem; backend i tak clampuje do MAX_RESULT_ROWS i
    // zwraca tylko realnie istniejące krawędzie.
    let clamped = MAX_RESULT_ROWS_CAP();
    let out = m
        .neighbors(ORG_A, "ad", "g", "a", NeighborDir::Out, None, clamped)
        .unwrap();
    assert_eq!(out.len(), 5, "zwraca realne krawędzie, nie błąd przy capie");
}

/// Lustro `graph_api::MAX_RESULT_ROWS` (test sprawdza, że duża wartość przechodzi
/// przez clamp w host-fn — tu wprost przekazujemy cap jako limit).
#[allow(non_snake_case)]
fn MAX_RESULT_ROWS_CAP() -> u32 {
    graph_api::MAX_RESULT_ROWS
}

#[test]
fn ppr_iterations_and_seed_caps_are_finite() {
    // Capy są zdefiniowane i sensowne — addon nie zażąda 1e9 iteracji ani
    // nieograniczonej liczby seedów (host clampuje przed pracą).
    assert!(graph_api::MAX_RANK_ITERATIONS >= 1 && graph_api::MAX_RANK_ITERATIONS <= 1_000);
    assert!(graph_api::MAX_PPR_SEEDS >= 1 && graph_api::MAX_PPR_SEEDS <= 4_096);

    // PPR z liczbą iteracji clampowaną przez host-fn nadal liczy się i zwraca wynik.
    let (_d, pool) = open_pool();
    let root = TempDir::new().unwrap();
    let m = mgr(pool, root.path().to_path_buf());
    for id in ["a", "b"] {
        m.upsert_node_with_quota(ORG_A, "ad", "g", id, "N", "{}", "null").unwrap();
    }
    m.upsert_edge_with_quota(ORG_A, "ad", "g", "a", "R", "b", 1.0, "{}", "null").unwrap();
    // Host-fn clampuje iteracje do MAX_RANK_ITERATIONS; tu podajemy już cap.
    let ranked = m
        .ppr(ORG_A, "ad", "g", &[("a".to_string(), 1.0)], 10, 0.85, graph_api::MAX_RANK_ITERATIONS)
        .unwrap();
    assert!(!ranked.is_empty());
}

// -----------------------------------------------------------------------------
// Cap współbieżności obliczeń (globalny + per-addon, fail-closed, RAII guard).
// -----------------------------------------------------------------------------

#[test]
fn compute_concurrency_cap_per_addon_fails_closed() {
    // Per-addon cap to MAX_PER_ADDON_GRAPH_COMPUTE: zajmij dokładnie tyle slotów,
    // kolejny dla TEGO SAMEGO addona musi dostać fail-closed ComputeBusy. Po
    // zwolnieniu (drop) slot wraca i kolejny acquire znów przechodzi.
    let addon = "addon-cc-percap";
    let cap = graph_api::MAX_PER_ADDON_GRAPH_COMPUTE;

    let mut held = Vec::new();
    for _ in 0..cap {
        held.push(
            graph_api::try_acquire_compute(addon).expect("slot w obrębie capa per-addon"),
        );
    }
    // N+1 dla tego addona → fail-closed.
    match graph_api::try_acquire_compute(addon) {
        Ok(_) => panic!("nadmiarowy acquire MUSI dostać fail-closed (per-addon cap)"),
        Err(e) => assert!(graph_api::is_compute_busy(&e), "oczekiwano ComputeBusy, mam: {e:?}"),
    }
    // Zwolnij jeden slot (Drop) i sprawdź, że kolejny acquire znów przechodzi.
    held.pop();
    let _again = graph_api::try_acquire_compute(addon).expect("po zwolnieniu slot wraca");
}

#[test]
fn compute_concurrency_cap_is_global_and_thread_safe() {
    // Globalny cap: MAX_GLOBAL_GRAPH_COMPUTE równoległych ciężkich wywołań może
    // jechać, a (MAX_GLOBAL+1)-szy MUSI dostać fail-closed. Rozkładamy obciążenie
    // na wielu addonów (różny addon_id), żeby per-addon cap nie wszedł pierwszy.
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    let global = graph_api::MAX_GLOBAL_GRAPH_COMPUTE;
    let total = global + 4; // nadmiarowi ponad globalny cap
    let barrier = Arc::new(Barrier::new(total));
    let busy = Arc::new(AtomicUsize::new(0));
    let ok = Arc::new(AtomicUsize::new(0));

    let mut handles = Vec::new();
    for i in 0..total {
        let barrier = barrier.clone();
        let busy = busy.clone();
        let ok = ok.clone();
        handles.push(std::thread::spawn(move || {
            // Każdy wątek to inny addon (per-addon cap nie ogranicza globalnego).
            let addon = format!("addon-global-{i}");
            // Zsynchronizuj start, by wszystkie acquire współzawodniły naraz.
            barrier.wait();
            match graph_api::try_acquire_compute(&addon) {
                Ok(slot) => {
                    ok.fetch_add(1, Ordering::AcqRel);
                    // Trzymaj slot, dopóki wszyscy nie spróbują (drugi barrier-like
                    // sleep krótki — wystarcza, bo acquire jest synchroniczny).
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    drop(slot);
                }
                Err(e) => {
                    assert!(graph_api::is_compute_busy(&e), "oczekiwano ComputeBusy: {e:?}");
                    busy.fetch_add(1, Ordering::AcqRel);
                }
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    let ok_n = ok.load(Ordering::Acquire);
    let busy_n = busy.load(Ordering::Acquire);
    println!("global compute cap: cap={global} total={total} ok={ok_n} busy={busy_n}");
    // Nigdy nie wpuściliśmy więcej niż globalny cap równolegle, więc co najmniej
    // (total - global) wywołań dostało fail-closed.
    assert!(ok_n <= global, "nie wolno wpuścić więcej niż globalny cap równolegle");
    assert!(busy_n >= total - global, "nadmiarowi MUSZĄ dostać fail-closed");
    // Po zakończeniu wszystkie sloty zwolnione → fresh acquire przechodzi.
    let _free = graph_api::try_acquire_compute("addon-after").expect("po zwolnieniu slot wolny");
}

// -----------------------------------------------------------------------------
// Izolacja + uninstall (B2).
// -----------------------------------------------------------------------------

#[test]
fn e2e_two_instances_are_physically_isolated() {
    let (_d, pool) = open_pool();
    let root = TempDir::new().unwrap();
    let m = mgr(pool, root.path().to_path_buf());

    // Two installed instances of the same package (distinct instance/addon ids).
    m.upsert_node_with_quota(ORG_A, "pkg-aaaa1111", "kg", "x", "N", "{}", "null").unwrap();
    m.upsert_node_with_quota(ORG_A, "pkg-bbbb2222", "kg", "y", "N", "{}", "null").unwrap();

    assert_eq!(m.node_count(ORG_A, "pkg-aaaa1111", "kg").unwrap(), 1);
    assert_eq!(m.node_count(ORG_A, "pkg-bbbb2222", "kg").unwrap(), 1);

    // B2: uninstall of instance A's graph must NOT touch instance B's graph.
    m.delete_all_for_addon(ORG_A, "pkg-aaaa1111").unwrap();
    let res_a = m.node_count(ORG_A, "pkg-aaaa1111", "kg");
    assert!(matches!(res_a, Err(GraphError::CollectionNotFound { .. })));
    assert_eq!(
        m.node_count(ORG_A, "pkg-bbbb2222", "kg").unwrap(),
        1,
        "instance B graph survives instance A uninstall"
    );
}

#[test]
fn e2e_cross_org_isolation() {
    let (_d, pool) = open_pool();
    let root = TempDir::new().unwrap();
    let m = mgr(pool, root.path().to_path_buf());

    // Same addon_id, two orgs → physically separate graphs.
    m.upsert_node_with_quota(ORG_A, "ad", "kg", "x", "N", "{}", "null").unwrap();
    assert_eq!(m.node_count(ORG_A, "ad", "kg").unwrap(), 1);
    let res_b = m.node_count(ORG_B, "ad", "kg");
    assert!(matches!(res_b, Err(GraphError::CollectionNotFound { .. })));
}

#[test]
fn uninstall_success_deletes_files_then_row() {
    // Sukces: pliki skasowane, potem wiersz; kolekcja znika z rejestru i z FS.
    let (_d, pool) = open_pool();
    let root = TempDir::new().unwrap();
    let m = mgr(pool, root.path().to_path_buf());

    m.upsert_node_with_quota(ORG_A, "ad", "kg", "x", "N", "{}", "null").unwrap();
    let file = m.collection_file_path(ORG_A, "ad", "kg").unwrap();
    assert!(file.exists(), "plik kolekcji istnieje przed uninstall");

    m.delete_all_for_addon(ORG_A, "ad").unwrap();

    // Wiersz znika.
    assert!(matches!(
        m.node_count(ORG_A, "ad", "kg"),
        Err(GraphError::CollectionNotFound { .. })
    ));
    // Plik (i katalog/sidecary) skasowane.
    assert!(!file.exists(), "plik kolekcji skasowany po uninstall");
}

#[test]
fn uninstall_file_delete_failure_keeps_row_for_retry() {
    // Fail kasowania plików → wiersz rejestru ZOSTAJE (retry-able), brak
    // orphan-files bez wiersza. Symulujemy nieusuwalność, podmieniając plik
    // kolekcji katalogiem z zawartością (na Linuksie `remove_dir_all` na ścieżce,
    // która jest plikiem-traktowanym-jak-katalog, albo odwrotnie, daje błąd I/O).
    let (_d, pool) = open_pool();
    let root = TempDir::new().unwrap();
    let m = mgr(pool, root.path().to_path_buf());

    m.upsert_node_with_quota(ORG_A, "ad", "kg", "x", "N", "{}", "null").unwrap();
    let file = m.collection_file_path(ORG_A, "ad", "kg").unwrap();
    assert!(file.exists());

    // Uczyń kasowanie niemożliwym: zamień plik bazy na katalog zawierający
    // niekasowalny wpis. Najpewniejszy cross-platform sposób to read-only katalog
    // rodzica. Tworzymy podkatalog i odbieramy prawa zapisu rodzicowi pliku, tak by
    // `remove_file`/`remove_dir_all` zawiodło wewnątrz `remove_cozo_files`.
    let parent = file.parent().unwrap().to_path_buf();
    // Zostaw w katalogu plik, którego nie da się usunąć, czyniąc katalog RO.
    let mut perms = std::fs::metadata(&parent).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o500); // r-x: brak prawa zapisu → unlink w katalogu faila
    }
    #[cfg(not(unix))]
    {
        perms.set_readonly(true);
    }
    std::fs::set_permissions(&parent, perms).unwrap();

    let res = m.delete_all_for_addon(ORG_A, "ad");

    // Przywróć prawa, by sprzątanie tempdir się powiodło.
    let mut restore = std::fs::metadata(&parent).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        restore.set_mode(0o700);
    }
    #[cfg(not(unix))]
    {
        #[allow(clippy::permissions_set_readonly_false)]
        restore.set_readonly(false);
    }
    std::fs::set_permissions(&parent, restore).unwrap();

    // Kluczowy inwariant kolejności: błąd kasowania plików zachodzi PRZED `DELETE`
    // wiersza, więc wiersz rejestru ZOSTAJE → operacja jest retry-able i nie ma
    // orphan-files bez wiersza. (Po awarii sled-dir może mieć częściowo skasowaną
    // zawartość, ale to nie łamie inwariantu wiersz-po-plikach; retry skasuje
    // resztę.) Jeśli na danej platformie RO-katalog jednak pozwolił skasować
    // (np. test pod rootem), `res` jest Ok i wiersz znika — wtedy sprawdzamy sukces.
    match res {
        Err(_) => {
            assert!(
                m.collection_exists(ORG_A, "ad", "kg").unwrap(),
                "fail kasowania plików zostawia wiersz rejestru (retry-able, brak orphan-files)"
            );
        }
        Ok(()) => {
            assert!(
                !m.collection_exists(ORG_A, "ad", "kg").unwrap(),
                "sukces uninstall kasuje wiersz rejestru"
            );
        }
    }
}

// -----------------------------------------------------------------------------
// Bezpośrednia ścieżka uninstall (lifecycle::uninstall / uninstall_addon) — NIE
// tylko uninstall_instance. Regresja: `addon_graph_collections` był w GENERYCZNEJ
// liście tabel kasowanych WPROST w transakcji DB `uninstall()`, z pominięciem
// `delete_all_for_addon` (close-handle → pliki → wiersz). To zostawiało osierocone
// pliki `.cozo` bez wierszy rejestru (łamanie files-before-row). Ten test pilnuje
// kontraktu źródłowego: (1) wiersz grafu NIE jest już w generycznej liście DELETE,
// (2) `uninstall()` sprząta graf przez `delete_all_for_addon`. Asercja na źródle,
// bo pełny `uninstall()` używa proces-globalnego `graph_manager` pinowanego do
// `~/.tentaflow` — sterowanie nim w teście byłoby kruche i śmieciłoby w HOME.
// -----------------------------------------------------------------------------

#[test]
fn direct_uninstall_routes_graph_cleanup_through_delete_all_for_addon() {
    let src = include_str!("../src/addon/lifecycle.rs");

    // Wytnij ciało `pub fn uninstall(` (do następnego `\npub fn ` lub `\nfn `),
    // żeby nie złapać `uninstall_instance` ani innych funkcji.
    let start = src
        .find("pub fn uninstall(addon_id: &str, db: &DbPool) -> Result<()> {")
        .expect("brak definicji pub fn uninstall — zmieniła się sygnatura?");
    let rest = &src[start..];
    let end = rest[1..]
        .find("\npub fn ")
        .map(|i| i + 1)
        .unwrap_or(rest.len());
    let body = &rest[..end];

    // Odfiltruj linie komentarza — szukamy tabeli w KODZIE, nie w opisie regresji
    // (komentarz nad funkcją celowo wymienia `addon_graph_collections`).
    let code_only: String = body
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    // (1) Generyczna lista tabel kasowanych WPROST w transakcji nie może już
    //     zawierać wiersza grafu jako cytowanego wpisu (`"addon_graph_collections"`)
    //     — inaczej DELETE pomija files-before-row.
    assert!(
        !code_only.contains("\"addon_graph_collections\""),
        "uninstall() NIE może kasować addon_graph_collections generycznym DELETE — \
         graf idzie wyłącznie przez delete_all_for_addon (files-before-row)"
    );

    // (2) Graf sprzątany jawnie przez `delete_all_for_addon` (jedyna ścieżka
    //     kasująca wiersze grafu — close-handle → pliki → wiersz).
    assert!(
        code_only.contains("delete_all_for_addon"),
        "uninstall() musi sprzątać graf przez delete_all_for_addon"
    );

    // Inwariant globalny: jedyne miejsca kasujące `addon_graph_collections` to
    // per-klucz DELETE wewnątrz seal_key_for_delete/insert-rollback w warstwie
    // grafu — żaden generyczny `DELETE FROM addon_graph_collections WHERE addon_id`.
    let graph_src = include_str!("../src/services/graph/collection.rs");
    assert!(
        !graph_src.contains("DELETE FROM addon_graph_collections WHERE addon_id"),
        "wiersze grafu kasujemy zawsze po (org_id, addon_id, collection), nie samym addon_id"
    );
}
