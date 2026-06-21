// ===== Plik: services/graph/tests.rs — testy jednostkowe warstwy grafowej =====
//
// Realne testy (nie zaślepki): tworzenie kolekcji, upsert węzłów/krawędzi,
// zapytanie sąsiadów Datalogiem, wbudowany PageRank Cozo, PPR w Rust nad CSR,
// izolacja dwóch GraphKey (różne org / różne addon), egzekwowanie quoty oraz —
// runda 2 — współbieżność: atomowa quota pod równoległym zapisem, realne
// zamknięcie backendu przy eviction (plik daje się skasować/ponownie otworzyć),
// delete czekający na write-lock w trakcie zapisu z innego wątku, oraz wyścig
// dwóch wątków o tę samą NOWĄ kolekcję (UNIQUE → load existing).
// Wszystkie pliki lądują w tempdir na `/mnt/e` (TMPDIR ustawiany przez runner).

use std::sync::Arc;

use rusqlite::Connection;
use tempfile::TempDir;

use super::backend::{CozoBackend, GraphBackend};
use super::collection::GraphManager;
use super::error::GraphError;
use super::ppr::personalized_pagerank;
use crate::db::DbPool;

const ORG_A: &str = "org-a";
const ORG_B: &str = "org-b";

fn in_memory_db() -> DbPool {
    let conn = Connection::open_in_memory().unwrap();
    crate::db::migrations::run(&conn).unwrap();
    Arc::new(crate::db::Db::from_connection(conn))
}

/// Tempdir pod `/mnt/e` (a nie `/tmp` w RAM). Honoruje `TMPDIR` ustawiony przez
/// runner; gdy nie ustawiono, fallback na katalog scratch projektu.
fn tempdir() -> TempDir {
    if std::env::var_os("TMPDIR").is_some() {
        TempDir::new().unwrap()
    } else {
        let base = std::path::Path::new("/mnt/e/repos/rust/_scratch/tf-graph-tests");
        std::fs::create_dir_all(base).unwrap();
        TempDir::new_in(base).unwrap()
    }
}

fn mgr() -> (TempDir, GraphManager) {
    let dir = tempdir();
    let pool = in_memory_db();
    let mgr = GraphManager::with_root(pool, dir.path().to_path_buf());
    (dir, mgr)
}

#[test]
fn test_ensure_collection_creates_row_and_file() {
    let (_dir, mgr) = mgr();
    mgr.ensure_collection(ORG_A, "addon_a", "kg").unwrap();
    assert_eq!(mgr.node_count(ORG_A, "addon_a", "kg").unwrap(), 0);
    assert_eq!(mgr.edge_count(ORG_A, "addon_a", "kg").unwrap(), 0);

    // Wiersz DB istnieje pod PK (org, addon, collection).
    let conn = mgr.pool().read().unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM addon_graph_collections \
             WHERE org_id='org-a' AND addon_id='addon_a' AND collection='kg'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn test_ensure_collection_idempotent() {
    let (_dir, mgr) = mgr();
    mgr.ensure_collection(ORG_A, "addon_a", "kg").unwrap();
    mgr.ensure_collection(ORG_A, "addon_a", "kg").unwrap();
    // Dwa razy ensure => wciąż jeden wiersz, jeden otwarty backend.
    let conn = mgr.pool().read().unwrap();
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM addon_graph_collections \
             WHERE org_id='org-a' AND addon_id='addon_a' AND collection='kg'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn test_upsert_nodes_and_edges_counts() {
    let (_dir, mgr) = mgr();
    mgr.upsert_node_with_quota(ORG_A, "addon_a", "kg", "n1", "Acme", "{}", "null")
        .unwrap();
    mgr.upsert_node_with_quota(ORG_A, "addon_a", "kg", "n2", "Alice", "{}", "null")
        .unwrap();
    let edges = mgr
        .upsert_edge_with_quota(ORG_A, "addon_a", "kg", "n2", "works_at", "n1", 1.0, "{}", "null")
        .unwrap();
    assert_eq!(edges, 1);

    assert_eq!(mgr.node_count(ORG_A, "addon_a", "kg").unwrap(), 2);
    assert_eq!(mgr.edge_count(ORG_A, "addon_a", "kg").unwrap(), 1);

    // Replace istniejącego węzła nie zwiększa licznika.
    let n = mgr
        .upsert_node_with_quota(ORG_A, "addon_a", "kg", "n1", "Acme Corp", "{}", "null")
        .unwrap();
    assert_eq!(n, 2);
}

#[test]
fn test_neighbors_out_edges() {
    use super::backend::NeighborDir;
    let (_dir, mgr) = mgr();
    mgr.ensure_collection(ORG_A, "addon_a", "kg").unwrap();
    for (id, label) in [("n1", "Acme"), ("n2", "Globex"), ("n3", "Alice")] {
        mgr.upsert_node_with_quota(ORG_A, "addon_a", "kg", id, label, "{}", "null")
            .unwrap();
    }
    mgr.upsert_edge_with_quota(ORG_A, "addon_a", "kg", "n3", "works_at", "n1", 1.0, "{}", "null")
        .unwrap();
    mgr.upsert_edge_with_quota(ORG_A, "addon_a", "kg", "n3", "knows", "n2", 0.5, "{}", "null")
        .unwrap();

    // Sąsiedzi n3 (out-edges) przez bezpieczny prymityw host-budowany.
    let out = mgr
        .neighbors(ORG_A, "addon_a", "kg", "n3", NeighborDir::Out, None, 10)
        .unwrap();
    assert_eq!(out.len(), 2);
    let dsts: std::collections::HashSet<String> =
        out.iter().map(|(id, _, _)| id.clone()).collect();
    assert!(dsts.contains("n1") && dsts.contains("n2"));
}

#[test]
fn test_builtin_pagerank() {
    let (_dir, be) = build_sample_graph();
    let rows = be
        .run_query(
            r"
            ?[node, score] <~ PageRank(*edges[src, dst]);
            :order -score
            :limit 5
            ",
        )
        .unwrap();
    assert!(!rows.rows.is_empty());
    // Wyniki posortowane malejąco: pierwszy score >= ostatni.
    let first = rows.rows.first().unwrap()[1].get_float().unwrap();
    let last = rows.rows.last().unwrap()[1].get_float().unwrap();
    assert!(first >= last);
}

#[test]
fn test_ppr_over_exported_csr() {
    let (_dir, be) = build_sample_graph();
    let csr = be.export_edges().unwrap();
    assert!(csr.node_count() >= 5);
    assert!(csr.edge_count() >= 5);

    // Seed na 'rag' (węzeł centralny tematu) — PPR powinien dać mu wysoki wynik.
    let seed_idx = csr.index_of("rag").expect("seed node present");
    let scores = personalized_pagerank(&csr, &[seed_idx], 0.85, 50);
    assert_eq!(scores.len(), csr.node_count());
    // Suma wyników ~ 1 (rozkład prawdopodobieństwa).
    let sum: f64 = scores.iter().map(|(_, s)| s).sum();
    assert!((sum - 1.0).abs() < 1e-6, "PPR mass not conserved: {sum}");
    // Seed jest wśród najwyżej ocenionych.
    let top_ids: Vec<&str> = scores.iter().take(3).map(|(id, _)| id.as_str()).collect();
    assert!(top_ids.contains(&"rag"), "seed not in top-3: {top_ids:?}");
}

#[test]
fn test_export_csr_via_manager() {
    // Eksport CSR przez manager (read-lock, manager-owned lifetime).
    let (_dir, mgr) = mgr();
    for (id, _l) in [("a", ""), ("b", ""), ("c", "")] {
        mgr.upsert_node_with_quota(ORG_A, "addon_a", "kg", id, "", "{}", "null")
            .unwrap();
    }
    mgr.upsert_edge_with_quota(ORG_A, "addon_a", "kg", "a", "to", "b", 2.0, "{}", "null")
        .unwrap();
    mgr.upsert_edge_with_quota(ORG_A, "addon_a", "kg", "a", "to", "c", 1.0, "{}", "null")
        .unwrap();
    let csr = mgr.export_csr(ORG_A, "addon_a", "kg").unwrap();
    assert_eq!(csr.node_count(), 3);
    assert_eq!(csr.edge_count(), 2);
}

#[test]
fn test_isolation_between_orgs() {
    let (_dir, mgr) = mgr();
    mgr.upsert_node_with_quota(ORG_A, "addon_a", "kg", "n1", "A", "{}", "null")
        .unwrap();
    // Ta sama nazwa addona+kolekcji w org B — fizycznie osobny plik/graf.
    mgr.ensure_collection(ORG_B, "addon_a", "kg").unwrap();
    assert_eq!(mgr.node_count(ORG_B, "addon_a", "kg").unwrap(), 0);
    assert_eq!(mgr.node_count(ORG_A, "addon_a", "kg").unwrap(), 1);
}

#[test]
fn test_isolation_between_addons() {
    let (_dir, mgr) = mgr();
    mgr.upsert_node_with_quota(ORG_A, "addon_a", "kg", "n1", "A", "{}", "null")
        .unwrap();
    mgr.ensure_collection(ORG_A, "addon_b", "kg").unwrap();
    assert_eq!(mgr.node_count(ORG_A, "addon_a", "kg").unwrap(), 1);
    assert_eq!(mgr.node_count(ORG_A, "addon_b", "kg").unwrap(), 0);
}

#[test]
fn test_get_missing_collection_not_found() {
    let (_dir, mgr) = mgr();
    let res = mgr.node_count(ORG_A, "addon_a", "ghost");
    assert!(matches!(res, Err(GraphError::CollectionNotFound { .. })));
}

#[test]
fn test_invalid_collection_name_rejected() {
    let (_dir, mgr) = mgr();
    let res = mgr.ensure_collection(ORG_A, "addon_a", "bad/name");
    assert!(matches!(res, Err(GraphError::InvalidCollectionName(_))));
}

#[test]
fn test_collection_quota_enforced() {
    use super::collection::MAX_COLLECTIONS_PER_ADDON;
    let (_dir, mgr) = mgr();
    for i in 0..MAX_COLLECTIONS_PER_ADDON {
        mgr.ensure_collection(ORG_A, "addon_a", &format!("kg{i}")).unwrap();
    }
    let res = mgr.ensure_collection(ORG_A, "addon_a", "overflow");
    assert!(matches!(
        res,
        Err(GraphError::CollectionQuotaExceeded { .. })
    ));
}

#[test]
fn test_node_quota_enforced_via_resource_limit() {
    let (_dir, mgr) = mgr();
    // Ustaw twardy limit 2 węzłów dla addona.
    {
        let conn = mgr.pool().write().unwrap();
        conn.execute(
            "INSERT INTO addon_resource_limits (addon_id, graph_nodes_max) VALUES ('addon_q', 2)",
            [],
        )
        .unwrap();
    }
    mgr.upsert_node_with_quota(ORG_A, "addon_q", "kg", "n1", "", "{}", "null")
        .unwrap();
    mgr.upsert_node_with_quota(ORG_A, "addon_q", "kg", "n2", "", "{}", "null")
        .unwrap();
    let err = mgr
        .upsert_node_with_quota(ORG_A, "addon_q", "kg", "n3", "", "{}", "null")
        .unwrap_err();
    assert!(matches!(err, GraphError::NodeQuotaExceeded { .. }));
}

#[test]
fn test_delete_collection_removes_row_and_file() {
    let (_dir, mgr) = mgr();
    mgr.upsert_node_with_quota(ORG_A, "addon_a", "kg", "n1", "", "{}", "null")
        .unwrap();
    let file_path: String = {
        let conn = mgr.pool().read().unwrap();
        conn.query_row(
            "SELECT file_path FROM addon_graph_collections \
             WHERE org_id='org-a' AND addon_id='addon_a' AND collection='kg'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert!(std::path::Path::new(&file_path).exists());

    mgr.delete_collection(ORG_A, "addon_a", "kg").unwrap();
    assert!(!std::path::Path::new(&file_path).exists());
    let n: i64 = {
        let conn = mgr.pool().read().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM addon_graph_collections \
             WHERE org_id='org-a' AND addon_id='addon_a' AND collection='kg'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(n, 0);
}

#[test]
fn test_delete_all_for_addon_is_tenant_scoped() {
    // Ten sam `addon_id` w dwóch organizacjach. `delete_all_for_addon` MUSI
    // kasować TYLKO org, dla którego go wołamy — inny tenant nietknięty
    // (kluczowanie po (org_id, addon_id), nie samym addon).
    let (_dir, mgr) = mgr();
    mgr.upsert_node_with_quota(ORG_A, "addon_a", "kg", "n1", "", "{}", "null")
        .unwrap();
    mgr.upsert_node_with_quota(ORG_B, "addon_a", "kg", "n1", "", "{}", "null")
        .unwrap();

    let file_b: String = {
        let conn = mgr.pool().read().unwrap();
        conn.query_row(
            "SELECT file_path FROM addon_graph_collections \
             WHERE org_id='org-b' AND addon_id='addon_a' AND collection='kg'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };

    mgr.delete_all_for_addon(ORG_A, "addon_a").unwrap();

    let conn = mgr.pool().read().unwrap();
    let a: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM addon_graph_collections \
             WHERE org_id='org-a' AND addon_id='addon_a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let b: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM addon_graph_collections \
             WHERE org_id='org-b' AND addon_id='addon_a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(a, 0, "org-a collections must be deleted");
    assert_eq!(b, 1, "org-b collections must survive cross-tenant delete");
    drop(conn);
    // Plik org-b nadal istnieje (nie skasowany przy usuwaniu org-a).
    assert!(
        std::path::Path::new(&file_b).exists(),
        "org-b graph file must survive"
    );
}

#[test]
fn test_delete_collection_quiesce_then_recreate() {
    // Quiesce: po delete plik znika, a ponowne utworzenie tej samej kolekcji
    // startuje od pustego grafu (backend został realnie zamknięty, plik usunięty).
    let (_dir, mgr) = mgr();
    mgr.upsert_node_with_quota(ORG_A, "addon_a", "kg", "n1", "", "{}", "null")
        .unwrap();
    let file_path: String = {
        let conn = mgr.pool().read().unwrap();
        conn.query_row(
            "SELECT file_path FROM addon_graph_collections \
             WHERE org_id='org-a' AND addon_id='addon_a' AND collection='kg'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert!(std::path::Path::new(&file_path).exists());

    mgr.delete_collection(ORG_A, "addon_a", "kg").unwrap();
    assert!(!std::path::Path::new(&file_path).exists());

    // Odtworzenie -> świeży graf bez n1.
    mgr.ensure_collection(ORG_A, "addon_a", "kg").unwrap();
    assert_eq!(mgr.node_count(ORG_A, "addon_a", "kg").unwrap(), 0);
}

#[test]
fn test_count_source_of_truth_is_cozo() {
    // Rejestr SQLite może być stary; quota i prawda liczą się z Cozo. Tu
    // sztucznie psujemy cache rejestru, a backend (Cozo) i tak zwraca realne 2.
    let (_dir, mgr) = mgr();
    mgr.upsert_node_with_quota(ORG_A, "addon_a", "kg", "n1", "", "{}", "null")
        .unwrap();
    mgr.upsert_node_with_quota(ORG_A, "addon_a", "kg", "n2", "", "{}", "null")
        .unwrap();
    // Zafałszuj cache rejestru i wyrzuć backend z pamięci, żeby następny dostęp
    // realnie otworzył plik z dysku (i uruchomił rekonsyliację).
    {
        let conn = mgr.pool().write().unwrap();
        conn.execute(
            "UPDATE addon_graph_collections SET node_count = 999 \
             WHERE org_id='org-a' AND addon_id='addon_a' AND collection='kg'",
            [],
        )
        .unwrap();
    }
    mgr.invalidate_addon("addon_a");

    // Cozo = źródło prawdy: realnie 2 węzły mimo cache=999.
    assert_eq!(mgr.node_count(ORG_A, "addon_a", "kg").unwrap(), 2);
    // Otwarcie z dysku rekonsyliuje cache z Cozo (999 -> 2).
    let cached: i64 = {
        let conn = mgr.pool().read().unwrap();
        conn.query_row(
            "SELECT node_count FROM addon_graph_collections \
             WHERE org_id='org-a' AND addon_id='addon_a' AND collection='kg'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(cached, 2, "registry reconciled from Cozo on open");
}

#[test]
fn test_node_quota_counts_across_collections() {
    // Quota węzłów jest sumaryczna po kolekcjach (org, addon). Limit 2: po
    // jednym węźle w dwóch kolekcjach trzeci (w dowolnej) jest odrzucony.
    let (_dir, mgr) = mgr();
    {
        let conn = mgr.pool().write().unwrap();
        conn.execute(
            "INSERT INTO addon_resource_limits (addon_id, graph_nodes_max) VALUES ('addon_q', 2)",
            [],
        )
        .unwrap();
    }
    mgr.upsert_node_with_quota(ORG_A, "addon_q", "kg1", "n1", "", "{}", "null")
        .unwrap();
    mgr.upsert_node_with_quota(ORG_A, "addon_q", "kg2", "n1", "", "{}", "null")
        .unwrap();
    let err = mgr
        .upsert_node_with_quota(ORG_A, "addon_q", "kg1", "n2", "", "{}", "null")
        .unwrap_err();
    assert!(matches!(err, GraphError::NodeQuotaExceeded { .. }));
}

#[test]
fn test_lru_eviction_caps_open_handles() {
    use super::collection::MAX_OPEN_GRAPHS;
    let (_dir, mgr) = mgr();
    // Otwórz więcej kolekcji niż cap (każda to nowy addon, żeby nie wpaść w
    // MAX_COLLECTIONS_PER_ADDON). `node_count` wymusza realne otwarcie backendu.
    // Ani liczba wpisów w cache, ani liczba otwartych backendów nie przekracza capu.
    for i in 0..(MAX_OPEN_GRAPHS + 5) {
        mgr.upsert_node_with_quota(ORG_A, &format!("addon_{i}"), "kg", "n1", "", "{}", "null")
            .unwrap();
        assert!(
            mgr.cached_entries() <= MAX_OPEN_GRAPHS,
            "cached entries exceeded cap at iteration {i}: {}",
            mgr.cached_entries()
        );
        assert!(
            mgr.open_handles() <= MAX_OPEN_GRAPHS,
            "open handles exceeded cap at iteration {i}: {}",
            mgr.open_handles()
        );
    }
    // Wyeksmitowana kolekcja nadal jest osiągalna (re-open z dysku) — i ma swój
    // węzeł, więc re-open realnie wczytał plik.
    assert_eq!(mgr.node_count(ORG_A, "addon_0", "kg").unwrap(), 1);
}

#[test]
fn test_eviction_really_closes_backend_file_reusable() {
    // Runda 2 bug #3: eviction MUSI realnie zamknąć backend, żeby plik sled dał
    // się skasować/ponownie otworzyć bez „file locked". Otwieramy jeden graf,
    // potem przekraczamy cap innymi kolekcjami (najdawniej używany pierwszy =
    // nasz cel zostaje wyeksmitowany), a następnie kasujemy go z dysku — co
    // udałoby się tylko gdy sled zwolnił plik.
    use super::collection::MAX_OPEN_GRAPHS;
    let (_dir, mgr) = mgr();
    // Realnie otwórz backend ofiary (upsert wymusza `sled::open`).
    mgr.upsert_node_with_quota(ORG_A, "addon_victim", "kg", "n1", "", "{}", "null")
        .unwrap();
    let victim_path: String = {
        let conn = mgr.pool().read().unwrap();
        conn.query_row(
            "SELECT file_path FROM addon_graph_collections \
             WHERE org_id='org-a' AND addon_id='addon_victim' AND collection='kg'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert!(std::path::Path::new(&victim_path).exists());

    // Wypchnij ofiarę z cache: otwórz cap+kilka innych kolekcji.
    for i in 0..(MAX_OPEN_GRAPHS + 2) {
        mgr.ensure_collection(ORG_A, &format!("addon_filler_{i}"), "kg")
            .unwrap();
    }
    // Ofiara nie jest już otwarta w cache.
    assert!(mgr.open_handles() <= MAX_OPEN_GRAPHS);

    // Skasuj plik wprost z dysku — uda się tylko gdy sled zamknął uchwyt.
    let p = std::path::Path::new(&victim_path);
    if p.is_dir() {
        std::fs::remove_dir_all(p).expect("evicted sled dir must be removable");
    } else {
        std::fs::remove_file(p).expect("evicted sled file must be removable");
    }
    assert!(!p.exists());
}

#[test]
fn test_concurrent_node_quota_never_exceeds_limit() {
    // Runda 2 bug #4: N wątków pisze równolegle do TEJ SAMEJ kolekcji przy
    // limicie LIMIT. Łączna liczba węzłów po wszystkich zapisach NIE przekracza
    // limitu — count+mutacja są atomowe pod jednym write-lockiem kolekcji.
    const LIMIT: u64 = 50;
    const THREADS: usize = 8;
    const PER_THREAD: usize = 20; // 8*20 = 160 prób > LIMIT

    let (_dir, mgr) = mgr();
    {
        let conn = mgr.pool().write().unwrap();
        conn.execute(
            "INSERT INTO addon_resource_limits (addon_id, graph_nodes_max) VALUES ('addon_c', ?1)",
            rusqlite::params![LIMIT as i64],
        )
        .unwrap();
    }
    mgr.ensure_collection(ORG_A, "addon_c", "kg").unwrap();

    let mgr = Arc::new(mgr);
    let mut handles = Vec::new();
    for t in 0..THREADS {
        let m = Arc::clone(&mgr);
        handles.push(std::thread::spawn(move || {
            let mut ok = 0u64;
            for i in 0..PER_THREAD {
                let id = format!("t{t}_n{i}");
                match m.upsert_node_with_quota(ORG_A, "addon_c", "kg", &id, "", "{}", "null") {
                    Ok(_) => ok += 1,
                    Err(GraphError::NodeQuotaExceeded { .. }) => {}
                    Err(e) => panic!("unexpected error: {e}"),
                }
            }
            ok
        }));
    }
    let total_ok: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();

    // Realna liczba węzłów w Cozo nie przekracza limitu...
    let count = mgr.node_count(ORG_A, "addon_c", "kg").unwrap();
    assert!(
        count <= LIMIT,
        "node count {count} exceeded quota {LIMIT} under concurrent writers"
    );
    // ...i nie przekroczyliśmy go ani o jeden „udany" insert.
    assert_eq!(count, total_ok, "successful inserts must equal live count");
    assert_eq!(count, LIMIT, "writers should have filled exactly to the limit");
}

#[test]
fn test_concurrent_get_or_create_same_new_collection() {
    // Runda 2 bug #6: wiele wątków równolegle tworzy TĘ SAMĄ nową kolekcję.
    // Drugi+ wątek dostaje UNIQUE-violation na INSERT i MUSI załadować istniejący
    // wiersz, nie zwrócić surowego błędu DB. Wszystkie ensure'y się udają,
    // powstaje dokładnie jeden wiersz.
    const THREADS: usize = 12;
    let (_dir, mgr) = mgr();
    let mgr = Arc::new(mgr);

    let mut handles = Vec::new();
    for _ in 0..THREADS {
        let m = Arc::clone(&mgr);
        handles.push(std::thread::spawn(move || {
            m.ensure_collection(ORG_A, "addon_race", "kg")
        }));
    }
    for h in handles {
        h.join().unwrap().expect("ensure_collection must not surface raw UNIQUE error");
    }

    let n: i64 = {
        let conn = mgr.pool().read().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM addon_graph_collections \
             WHERE org_id='org-a' AND addon_id='addon_race' AND collection='kg'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(n, 1, "exactly one row for the contended new collection");
}

#[test]
fn test_delete_waits_for_concurrent_writer_no_corruption() {
    // Runda 2 bug #5: delete bierze write-lock slotu kolekcji, więc czeka aż
    // piszący wątek skończy swój upsert; backend jest zamykany i pliki kasowane
    // pod tym samym lockiem (żadne `sled::open` nie biegnie równolegle z usuwaniem
    // pliku). Piszący, który czekał na lock w trakcie delete, widzi tombstone i
    // dostaje błąd zamiast operować na kasowanym pliku. Kluczowe: ZERO korupcji
    // i ZERO paniki — niezależnie od tego, który wątek wygra wyścig, końcowy stan
    // jest spójny i kolekcja jest otwieralna.
    let (_dir, mgr) = mgr();
    mgr.ensure_collection(ORG_A, "addon_d", "kg").unwrap();
    let mgr = Arc::new(mgr);

    let writer = {
        let m = Arc::clone(&mgr);
        std::thread::spawn(move || {
            // Stały zestaw zapisów; każdy upsert albo się udaje, albo zwraca
            // czysty błąd (tombstone/closed/not-found) — NIGDY nie panikuje ani
            // nie koruptuje pliku. Po delete piszący może odtworzyć kolekcję
            // (semantyka: zapis tworzy kolekcję), co jest dopuszczalne.
            for i in 0..200 {
                let id = format!("w{i}");
                let _ = m.upsert_node_with_quota(ORG_A, "addon_d", "kg", &id, "", "{}", "null");
            }
        })
    };

    std::thread::sleep(std::time::Duration::from_millis(2));
    mgr.delete_collection(ORG_A, "addon_d", "kg").unwrap();
    writer.join().unwrap();

    // Brak korupcji: stan końcowy jest spójny. Kolekcja albo nie istnieje (delete
    // wygrał ostatecznie), albo została odtworzona przez piszącego — w obu
    // przypadkach `node_count` daje spójną liczbę bez błędu/korupcji.
    match mgr.node_count(ORG_A, "addon_d", "kg") {
        Ok(_) => {
            // Odtworzona/żywa: ponowne otwarcie i policzenie nie koruptuje.
            let again = mgr.node_count(ORG_A, "addon_d", "kg").unwrap();
            let _ = again;
        }
        Err(GraphError::CollectionNotFound { .. }) => {
            // Skasowana: wiersz DB zniknął.
            let n: i64 = {
                let conn = mgr.pool().read().unwrap();
                conn.query_row(
                    "SELECT COUNT(*) FROM addon_graph_collections \
                     WHERE org_id='org-a' AND addon_id='addon_d' AND collection='kg'",
                    [],
                    |r| r.get(0),
                )
                .unwrap()
            };
            assert_eq!(n, 0, "deleted collection must leave no row");
        }
        Err(e) => panic!("unexpected error after concurrent delete/write: {e}"),
    }

    // Sanity: kolekcję da się od nowa utworzyć i otworzyć (plik nie jest zablokowany).
    mgr.ensure_collection(ORG_A, "addon_d", "kg").unwrap();
    let _ = mgr.node_count(ORG_A, "addon_d", "kg").unwrap();
}

#[test]
fn test_ppr_weighted_differs_from_unweighted() {
    // Graf z silnie asymetrycznymi wagami daje INNY ranking niż gdyby wagi były
    // ignorowane. Węzeł `hub` ma dwie out-krawędzie: ciężką do `heavy`, lekką do
    // `light`. PPR ważony musi faworyzować `heavy`.
    let be = CozoBackend::open_in_memory().unwrap();
    for id in ["seed", "hub", "heavy", "light"] {
        be.upsert_node(id, "", "{}", "null").unwrap();
    }
    be.upsert_edge("seed", "to", "hub", 1.0, "{}", "null")
        .unwrap();
    be.upsert_edge("hub", "to", "heavy", 9.0, "{}", "null")
        .unwrap();
    be.upsert_edge("hub", "to", "light", 1.0, "{}", "null")
        .unwrap();

    let csr = be.export_edges().unwrap();
    let seed = csr.index_of("seed").unwrap();
    let scores = personalized_pagerank(&csr, &[seed], 0.85, 100);
    let score = |id: &str| scores.iter().find(|(x, _)| x == id).map(|(_, s)| *s).unwrap();
    // Ciężka krawędź => `heavy` zbiera istotnie więcej masy niż `light`.
    assert!(
        score("heavy") > score("light") * 2.0,
        "weighted PPR must favour the 9.0 edge: heavy={} light={}",
        score("heavy"),
        score("light")
    );
}

#[test]
fn test_ppr_dedups_seeds() {
    // Powtórzony seed nie zawyża jego masy. Wynik z [s, s, s] musi być
    // identyczny jak z [s].
    let (_dir, be) = build_sample_graph();
    let csr = be.export_edges().unwrap();
    let s = csr.index_of("rag").unwrap();
    let once = personalized_pagerank(&csr, &[s], 0.85, 50);
    let thrice = personalized_pagerank(&csr, &[s, s, s], 0.85, 50);
    for ((id_a, sa), (id_b, sb)) in once.iter().zip(thrice.iter()) {
        assert_eq!(id_a, id_b);
        assert!((sa - sb).abs() < 1e-12, "dedup changed score for {id_a}");
    }
}

#[test]
fn test_concurrent_global_quota_across_collections() {
    // Runda 3 bug F: N wątków pisze równolegle do RÓŻNYCH kolekcji TEGO SAMEGO
    // addona przy GLOBALNYM limicie węzłów. To dokładnie scenariusz, którego
    // runda 2 NIE pokrywała (per-kolekcyjny lock nie chroni sumy między
    // kolekcjami). Łączna liczba węzłów po WSZYSTKICH kolekcjach NIE przekracza
    // limitu — rezerwacja idzie przez atomowy ledger SQLite (`BEGIN IMMEDIATE`).
    const LIMIT: u64 = 30;
    const COLLECTIONS: usize = 6; // < MAX_COLLECTIONS_PER_ADDON
    const PER_COLLECTION: usize = 20; // 6*20 = 120 prób > LIMIT

    let (_dir, mgr) = mgr();
    {
        let conn = mgr.pool().write().unwrap();
        conn.execute(
            "INSERT INTO addon_resource_limits (addon_id, graph_nodes_max) VALUES ('addon_g', ?1)",
            rusqlite::params![LIMIT as i64],
        )
        .unwrap();
    }

    let mgr = Arc::new(mgr);
    let mut handles = Vec::new();
    for c in 0..COLLECTIONS {
        let m = Arc::clone(&mgr);
        let coll = format!("kg{c}");
        handles.push(std::thread::spawn(move || {
            let mut ok = 0u64;
            for i in 0..PER_COLLECTION {
                let id = format!("c{c}_n{i}");
                match m.upsert_node_with_quota(ORG_A, "addon_g", &coll, &id, "", "{}", "null") {
                    Ok(_) => ok += 1,
                    Err(GraphError::NodeQuotaExceeded { .. }) => {}
                    Err(e) => panic!("unexpected error: {e}"),
                }
            }
            ok
        }));
    }
    let total_ok: u64 = handles.into_iter().map(|h| h.join().unwrap()).sum();

    // Suma realnych węzłów po wszystkich kolekcjach (z Cozo) nie przekracza limitu.
    let mut live_total = 0u64;
    for c in 0..COLLECTIONS {
        live_total += mgr.node_count(ORG_A, "addon_g", &format!("kg{c}")).unwrap();
    }
    assert!(
        live_total <= LIMIT,
        "cross-collection node total {live_total} exceeded global quota {LIMIT}"
    );
    // Każdy udany insert to realny węzeł — żaden nie przeszedł ponad limit.
    assert_eq!(
        live_total, total_ok,
        "successful inserts must equal live count across collections"
    );
    assert_eq!(
        live_total, LIMIT,
        "concurrent writers should fill exactly to the global limit"
    );
}

#[test]
fn test_eviction_stale_handle_cannot_resurrect_under_load() {
    // Runda 3 bug G: pod obciążeniem (wiele wątków otwiera różne kolekcje, część
    // zostaje wyeksmitowana) liczba realnie otwartych backendów nigdy nie
    // przekracza capu, a licznik `open_backends` pozostaje spójny ze stanem
    // slotów (żaden przeterminowany Arc nie otworzył bazy „obok" licznika).
    use super::collection::MAX_OPEN_GRAPHS;
    let (_dir, mgr) = mgr();
    let mgr = Arc::new(mgr);

    const THREADS: usize = 8;
    const ADDONS: usize = MAX_OPEN_GRAPHS + 12;
    let mut handles = Vec::new();
    for t in 0..THREADS {
        let m = Arc::clone(&mgr);
        handles.push(std::thread::spawn(move || {
            for round in 0..4 {
                for a in 0..ADDONS {
                    let addon = format!("addon_{a}");
                    let id = format!("t{t}_r{round}_n{a}");
                    let _ = m.upsert_node_with_quota(ORG_A, &addon, "kg", &id, "", "{}", "null");
                    // Odczyt wymusza ponowne otwarcie po eviction.
                    let _ = m.node_count(ORG_A, &addon, "kg");
                }
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    // Cap otwartych backendów dotrzymany.
    assert!(
        mgr.open_handles() <= MAX_OPEN_GRAPHS,
        "open handles {} exceeded cap {MAX_OPEN_GRAPHS}",
        mgr.open_handles()
    );
    assert!(
        mgr.cached_entries() <= MAX_OPEN_GRAPHS,
        "cached entries {} exceeded cap {MAX_OPEN_GRAPHS}",
        mgr.cached_entries()
    );
    // Licznik open_backends zgadza się z realną liczbą slotów `Open` (brak
    // przecieku/podwójnego inkrementu od stale-Arc).
    assert_eq!(
        mgr.open_backends_counter() as usize,
        mgr.open_handles(),
        "open_backends counter desynced from live Open slots"
    );

    // Wyeksmitowana kolekcja nadal osiągalna (re-open z dysku, świeży wpis).
    let _ = mgr.node_count(ORG_A, "addon_0", "kg").unwrap();
}

#[test]
fn test_delete_cache_miss_serialized_no_resurrection() {
    // Runda 3 bug H: delete przy CACHE-MISS musi przejść przez ten sam per-key
    // punkt serializacji co get_or_create. Wątki: jeden powtarza delete, drugi
    // równolegle pisze (get_or_create + upsert). Po wszystkim stan jest spójny —
    // żaden zapis nie operuje na wpół-skasowanej bazie i nie panikuje, a delete
    // nigdy nie zostawia osieroconego wiersza bez możliwości świeżego utworzenia.
    let (_dir, mgr) = mgr();
    let mgr = Arc::new(mgr);

    let deleter = {
        let m = Arc::clone(&mgr);
        std::thread::spawn(move || {
            for _ in 0..150 {
                // Często trafia w cache-miss (writer dopiero co skasował wpis).
                m.delete_collection(ORG_A, "addon_h", "kg").unwrap();
            }
        })
    };
    let writer = {
        let m = Arc::clone(&mgr);
        std::thread::spawn(move || {
            for i in 0..150 {
                let id = format!("w{i}");
                match m.upsert_node_with_quota(ORG_A, "addon_h", "kg", &id, "", "{}", "null") {
                    Ok(_) => {}
                    Err(GraphError::CollectionNotFound { .. }) => {}
                    Err(GraphError::Backend(_)) => {} // przejściowy Removed/closed
                    Err(e) => panic!("unexpected error under delete/write race: {e}"),
                }
            }
        })
    };
    deleter.join().unwrap();
    writer.join().unwrap();

    // Końcowy delete domyka stan: brak wiersza, brak pliku, świeże utworzenie OK.
    mgr.delete_collection(ORG_A, "addon_h", "kg").unwrap();
    let n: i64 = {
        let conn = mgr.pool().read().unwrap();
        conn.query_row(
            "SELECT COUNT(*) FROM addon_graph_collections \
             WHERE org_id='org-a' AND addon_id='addon_h' AND collection='kg'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(n, 0, "deleted collection must leave no row");

    // Świeże utworzenie startuje od pustego grafu (brak wskrzeszenia starych danych).
    mgr.ensure_collection(ORG_A, "addon_h", "kg").unwrap();
    assert_eq!(mgr.node_count(ORG_A, "addon_h", "kg").unwrap(), 0);
}

#[test]
fn test_stale_handle_after_invalidate_does_not_open() {
    // Runda 4 bug 1: po `invalidate_addon` przeterminowany `Arc<GraphEntry>`
    // (trzymany sprzed inwalidacji) MUSI zobaczyć stan `Removed` i NIE otworzyć
    // bazy „obok" świeżego wpisu. Po reorderze (`mark_removed` przed `remove`)
    // żaden re-open nie podbija licznika otwartych ponad realny stan slotów.
    let (_dir, mgr) = mgr();
    mgr.upsert_node_with_quota(ORG_A, "addon_s", "kg", "n1", "", "{}", "null")
        .unwrap();
    // Backend jest teraz otwarty (1 slot Open, licznik = 1).
    assert_eq!(mgr.open_backends_counter(), 1);
    assert_eq!(mgr.open_handles(), 1);

    // Inwalidacja: slot → Removed, wpis zdjęty z mapy, licznik wraca do 0.
    mgr.invalidate_addon("addon_s");
    assert_eq!(
        mgr.open_backends_counter(),
        0,
        "invalidate must close backend and zero the open counter"
    );
    assert_eq!(mgr.open_handles(), 0);

    // Kolejny dostęp re-fetchuje świeży wpis i re-otwiera z dysku (dane przetrwały).
    assert_eq!(mgr.node_count(ORG_A, "addon_s", "kg").unwrap(), 1);
    // Po re-open dokładnie jeden backend otwarty — żaden stale-Arc nie otworzył drugiego.
    assert_eq!(
        mgr.open_backends_counter(),
        1,
        "exactly one backend open after re-fetch (no resurrection)"
    );
    assert_eq!(mgr.open_handles(), 1);
}

#[test]
fn test_delete_cache_miss_uses_deterministic_nonempty_path() {
    // Runda 4 bug 2: delete przy CACHE-MISS liczy ścieżkę deterministycznie z
    // klucza (NIE z wiersza DB, NIE `PathBuf::default()`). Równoległy writer i
    // deleter operują na tej samej, niepustej ścieżce — zero otwierania pustej
    // ścieżki, zero paniki/korupcji, a po wszystkim świeże utworzenie startuje
    // od pustego grafu.
    let (_dir, mgr) = mgr();
    let mgr = Arc::new(mgr);

    let writer = {
        let m = Arc::clone(&mgr);
        std::thread::spawn(move || {
            for i in 0..200 {
                let id = format!("w{i}");
                match m.upsert_node_with_quota(ORG_A, "addon_p", "kg", &id, "", "{}", "null") {
                    Ok(_) => {}
                    Err(GraphError::CollectionNotFound { .. }) => {}
                    Err(GraphError::Backend(_)) => {}
                    Err(e) => panic!("unexpected writer error: {e}"),
                }
            }
        })
    };
    let deleter = {
        let m = Arc::clone(&mgr);
        std::thread::spawn(move || {
            for _ in 0..200 {
                // Każdy delete liczy ścieżkę z klucza — nawet gdy wiersza DB nie
                // ma (cache-miss). Brak pustej ścieżki => brak błędu I/O na "".
                m.delete_collection(ORG_A, "addon_p", "kg").unwrap();
            }
        })
    };
    writer.join().unwrap();
    deleter.join().unwrap();

    // Domknięcie: brak wiersza, świeże utworzenie pustego grafu.
    mgr.delete_collection(ORG_A, "addon_p", "kg").unwrap();
    mgr.ensure_collection(ORG_A, "addon_p", "kg").unwrap();
    assert_eq!(mgr.node_count(ORG_A, "addon_p", "kg").unwrap(), 0);

    // Ścieżka w wierszu DB jest niepusta i deterministyczna (pod tempdir roota).
    let file_path: String = {
        let conn = mgr.pool().read().unwrap();
        conn.query_row(
            "SELECT file_path FROM addon_graph_collections \
             WHERE org_id='org-a' AND addon_id='addon_p' AND collection='kg'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert!(!file_path.is_empty(), "deterministic file path must be non-empty");
    assert!(file_path.ends_with("kg.cozo"), "path derived from key: {file_path}");
}

#[test]
fn test_with_write_progresses_under_open_pressure() {
    // Runda 4 bug 3: aktywny zbiór kluczy > MAX_OPEN_GRAPHS, wiele wątków
    // jednocześnie pisze. Każda operacja `with_write` MUSI zakończyć się w
    // skończonym czasie (brak livelocka/starvation) — pętla re-fetch jest
    // ograniczona, a po wyczerpaniu prób wymuszamy otwarcie (chwilowy over-cap).
    // Asercja: wszystkie N operacji faktycznie się zakończyły z sukcesem.
    use super::collection::MAX_OPEN_GRAPHS;
    let (_dir, mgr) = mgr();
    let mgr = Arc::new(mgr);

    const THREADS: usize = 8;
    const KEYS: usize = MAX_OPEN_GRAPHS + 16; // aktywny zbiór > cap
    const OPS_PER_THREAD: usize = KEYS * 3;

    let done = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let start = std::time::Instant::now();
    let mut handles = Vec::new();
    for t in 0..THREADS {
        let m = Arc::clone(&mgr);
        let d = Arc::clone(&done);
        handles.push(std::thread::spawn(move || {
            for i in 0..OPS_PER_THREAD {
                let addon = format!("addon_{}", i % KEYS);
                let id = format!("t{t}_n{i}");
                // Każdy upsert to `with_write`; pod presją eviction NIE może
                // zawisnąć. Tolerujemy jedynie przejściowy błąd contention.
                match m.upsert_node_with_quota(ORG_A, &addon, "kg", &id, "", "{}", "null") {
                    Ok(_) => {}
                    Err(GraphError::Backend(_)) => {}
                    Err(e) => panic!("unexpected error under open pressure: {e}"),
                }
                d.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let elapsed = start.elapsed();

    // Wszystkie operacje zakończone — brak livelocka.
    assert_eq!(
        done.load(std::sync::atomic::Ordering::Relaxed),
        THREADS * OPS_PER_THREAD,
        "every with_write op must complete (no livelock/starvation)"
    );
    // Rozsądny limit czasu — gdyby był livelock, ten test by nie skończył w sekundach.
    assert!(
        elapsed < std::time::Duration::from_secs(60),
        "concurrent with_write under pressure took too long: {elapsed:?}"
    );
    // Cap pozostaje twardy po ustaniu presji (force-open to tylko CHWILOWY over-cap).
    assert!(
        mgr.cached_entries() <= MAX_OPEN_GRAPHS,
        "cached entries {} exceeded cap after pressure",
        mgr.cached_entries()
    );
}

#[test]
fn test_cold_key_create_vs_delete_no_orphan() {
    // Runda 5: writer i deleter startują RÓWNOLEGLE na NIEISTNIEJĄCEJ kolekcji
    // (cold key — brak wpisu w mapie i brak wiersza DB). Przed fixem cold-create
    // ustanawiał punkt serializacji per-klucz ZA PÓŹNO: insert wiersza DB działo
    // się PRZED `intern_entry`, więc deleter mógł zainterować własny kanoniczny
    // wpis, skasować wiersz+pliki i oznaczyć Removed, a writer wstawiał świeży wpis
    // na skasowany wiersz i otwierał Cozo → ŻYWE pliki/backend BEZ wiersza DB.
    //
    // Po fixie wszystkie efekty uboczne (ensure_row + open) dzieją się POD slot-
    // write-lockiem kanonicznego wpisu, a delete bierze slot-lock TEGO SAMEGO
    // wpisu — cold-create i delete są wzajemnie wykluczające. Inwariant po KAŻDEJ
    // rundzie: albo kolekcja istnieje Z wierszem DB i spójnym ledgerem
    // (node_count == realny count z Cozo), albo nie istnieje WCALE (brak wiersza
    // I brak żywego backendu/plików). NIGDY żywe pliki/backend bez wiersza DB.
    use std::sync::Barrier;

    let (dir, mgr) = mgr();
    let mgr = Arc::new(mgr);
    let root = dir.path().to_path_buf();

    const ROUNDS: usize = 40;
    const WRITERS: usize = 4;

    for round in 0..ROUNDS {
        // Każda runda startuje od cold key: czyść wpis z mapy i wiersz DB.
        mgr.delete_collection(ORG_A, "addon_c", "kg").unwrap();

        let barrier = Arc::new(Barrier::new(WRITERS + 1));
        let mut handles = Vec::new();

        for w in 0..WRITERS {
            let m = Arc::clone(&mgr);
            let b = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                b.wait();
                let id = format!("r{round}_w{w}");
                match m.upsert_node_with_quota(ORG_A, "addon_c", "kg", &id, "", "{}", "null") {
                    Ok(_) => {}
                    Err(GraphError::CollectionNotFound { .. }) => {}
                    Err(GraphError::Backend(_)) => {} // przejściowy Removed/contention
                    Err(e) => panic!("unexpected writer error (cold-key race): {e}"),
                }
            }));
        }
        {
            let m = Arc::clone(&mgr);
            let b = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                b.wait();
                m.delete_collection(ORG_A, "addon_c", "kg").unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // INWARIANT końca rundy. Stan DB.
        let (db_nodes, has_row): (i64, bool) = {
            let conn = mgr.pool().read().unwrap();
            let row: Option<i64> = conn
                .query_row(
                    "SELECT node_count FROM addon_graph_collections \
                     WHERE org_id='org-a' AND addon_id='addon_c' AND collection='kg'",
                    [],
                    |r| r.get(0),
                )
                .ok();
            match row {
                Some(n) => (n, true),
                None => (0, false),
            }
        };

        // Plik na dysku.
        let file = root
            .join("org-a")
            .join("addon_c")
            .join("graph")
            .join("kg.cozo");
        let file_present = file.exists();

        if has_row {
            // Kolekcja istnieje: realny count z Cozo musi zgadzać się z ledgerem.
            let cozo_nodes = mgr.node_count(ORG_A, "addon_c", "kg").unwrap();
            assert_eq!(
                cozo_nodes as i64, db_nodes,
                "round {round}: ledger node_count {db_nodes} != Cozo {cozo_nodes}"
            );
        } else {
            // Kolekcji nie ma: NIE może być żywego pliku/backendu bez wiersza DB.
            assert!(
                !file_present,
                "round {round}: orphan graph file with NO DB row: {file:?}"
            );
            // Brak otwartego backendu dla skasowanej kolekcji.
            assert_eq!(
                mgr.open_backends_counter() as usize,
                mgr.open_handles(),
                "round {round}: open_backends counter desynced"
            );
        }
    }

    // Domknięcie: licznik otwartych zgadza się ze stanem slotów po wszystkich rundach.
    assert_eq!(
        mgr.open_backends_counter() as usize,
        mgr.open_handles(),
        "open_backends counter desynced from live Open slots after all rounds"
    );
}

// ----------------------------------------------------------------------------
// Pomocnicze
// ----------------------------------------------------------------------------

/// Buduje przykładowy graf tematyczny dla PageRank/PPR i zwraca otwarty backend
/// (in-memory, bez plików). Tempdir trzymany tylko po to, by sygnatura była
/// spójna z resztą testów.
fn build_sample_graph() -> (TempDir, CozoBackend) {
    let dir = tempdir();
    let be = CozoBackend::open_in_memory().unwrap();
    for (id, label) in [
        ("acme", "Acme Corp"),
        ("alice", "Alice"),
        ("bob", "Bob"),
        ("gdpr", "GDPR"),
        ("rag", "RAG"),
        ("embeddings", "Embeddings"),
        ("pagerank", "PageRank"),
        ("hnsw", "HNSW"),
    ] {
        be.upsert_node(id, label, "{}", "null").unwrap();
    }
    for (s, r, d, w) in [
        ("alice", "works_at", "acme", 1.0),
        ("bob", "works_at", "acme", 1.0),
        ("alice", "knows", "bob", 0.8),
        ("acme", "about", "gdpr", 0.9),
        ("acme", "about", "rag", 0.9),
        ("rag", "about", "embeddings", 0.7),
        ("rag", "about", "pagerank", 0.7),
        ("rag", "about", "hnsw", 0.7),
        ("embeddings", "about", "hnsw", 0.6),
        ("pagerank", "about", "rag", 0.6),
    ] {
        be.upsert_edge(s, r, d, w, "{}", "null").unwrap();
    }
    (dir, be)
}








