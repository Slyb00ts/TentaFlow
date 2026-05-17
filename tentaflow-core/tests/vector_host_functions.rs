// =============================================================================
// File: tests/vector_host_functions.rs
// Purpose: Integration tests for the F1c P3 vector storage stack — exercises
//          the public services API (`NamespaceManager` + `UsearchBackend`)
//          plus the host-function helpers (`decode_vector`, `check_gate`,
//          `map_vector_error`) end-to-end against a real on-disk SQLite + a
//          tempdir for the `.usearch` files. The wasmtime ABI wiring is
//          covered indirectly: every host function in `vector.rs` reduces to
//          these helpers + the manager so a regression at this layer is the
//          same defect a guest addon would observe.
// =============================================================================

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tempfile::TempDir;
use tentaflow_core::addon::errors::AbiError;
use tentaflow_core::addon::host_functions::vector::test_api as vector_api;
use tentaflow_core::addon::manifest::VectorNamespaceSpec;
use tentaflow_core::services::vector::{
    Metric, NamespaceManager, VectorError, MAX_NAMESPACES_PER_ADDON,
};

fn open_pool() -> (TempDir, tentaflow_core::db::DbPool) {
    let dir = TempDir::new().expect("tempdir");
    let path = dir.path().join("test.db");
    let pool = tentaflow_core::db::init(&path).expect("init DB");
    (dir, pool)
}

fn mgr_with_temproot(pool: tentaflow_core::db::DbPool, root: PathBuf) -> NamespaceManager {
    // Use the `with_root` test ctor to avoid touching `~/.tentaflow`.
    NamespaceManager::with_root(pool, root)
}

fn spec(name: &str, dim: u32, distance: &str, gate: Option<&str>) -> VectorNamespaceSpec {
    VectorNamespaceSpec {
        name: name.to_string(),
        dimensions: dim,
        distance: distance.to_string(),
        data_class: "B".to_string(),
        gate: gate.map(|s| s.to_string()),
    }
}

// -----------------------------------------------------------------------------
// decode_vector — wire format round-trip
// -----------------------------------------------------------------------------

#[test]
fn decode_vector_roundtrip() {
    let raw = vec![0.0f32, 1.0, -1.5, 3.14];
    let mut bytes = Vec::new();
    for f in &raw {
        bytes.extend_from_slice(&f.to_le_bytes());
    }
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let decoded = vector_api::decode_vector(&b64).expect("decode ok");
    assert_eq!(decoded, raw);
}

#[test]
fn decode_vector_rejects_non_multiple_of_4() {
    use base64::Engine;
    let bad = base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3]);
    assert!(vector_api::decode_vector(&bad).is_err());
}

#[test]
fn decode_vector_rejects_empty() {
    assert!(vector_api::decode_vector("").is_err());
}

// -----------------------------------------------------------------------------
// check_gate — placeholder enforcement for namespaces declaring a gate
// -----------------------------------------------------------------------------

#[test]
fn check_gate_passes_when_namespace_has_no_gate() {
    let s = spec("attributes", 768, "cosine", None);
    assert!(vector_api::check_gate(&s, None).is_ok());
    assert!(vector_api::check_gate(&s, Some("ignored")).is_ok());
}

#[test]
fn check_gate_denies_when_gate_declared_but_no_claim() {
    let s = spec("faces", 512, "cosine", Some("d4-historical"));
    let err = vector_api::check_gate(&s, None).unwrap_err();
    assert_eq!(err, AbiError::GateNotSatisfied);
}

#[test]
fn check_gate_denies_when_gate_declared_with_empty_claim() {
    let s = spec("faces", 512, "cosine", Some("d4-historical"));
    let err = vector_api::check_gate(&s, Some("")).unwrap_err();
    assert_eq!(err, AbiError::GateNotSatisfied);
}

#[test]
fn check_gate_allows_when_gate_declared_with_nonempty_claim() {
    let s = spec("faces", 512, "cosine", Some("d4-historical"));
    assert!(vector_api::check_gate(&s, Some("claim_abc123")).is_ok());
}

// -----------------------------------------------------------------------------
// map_vector_error — every variant maps to the expected AbiError
// -----------------------------------------------------------------------------

#[test]
fn map_vector_error_quota_maps_to_quota_exceeded() {
    let (abi, reason) = vector_api::map_vector_error(VectorError::VectorQuotaExceeded {
        addon_id: "x".into(),
        current: 100,
        max: 100,
    });
    assert_eq!(abi, AbiError::QuotaExceeded);
    assert_eq!(reason, "vector_quota_exceeded");
}

#[test]
fn map_vector_error_not_found_maps_to_not_found() {
    let (abi, _) = vector_api::map_vector_error(VectorError::NamespaceNotFound {
        addon_id: "x".into(),
        namespace: "y".into(),
    });
    assert_eq!(abi, AbiError::NotFound);
}

// -----------------------------------------------------------------------------
// End-to-end via NamespaceManager — simulates the work a host function does
// after permission/audit hooks have run.
// -----------------------------------------------------------------------------

#[test]
fn e2e_vector_upsert_search_delete_roundtrip() {
    let (_dir, pool) = open_pool();
    let root = TempDir::new().unwrap();
    let mgr = mgr_with_temproot(pool, root.path().to_path_buf());

    let be = mgr
        .get_or_create("addon_a", "faces", 4, Metric::Cosine)
        .expect("open ns");

    be.upsert(1, &[1.0, 0.0, 0.0, 0.0]).expect("upsert 1");
    be.upsert(2, &[0.0, 1.0, 0.0, 0.0]).expect("upsert 2");
    be.upsert(3, &[0.0, 0.0, 1.0, 0.0]).expect("upsert 3");
    be.save().expect("persist");
    mgr.update_count("addon_a", "faces", be.count()).unwrap();

    // Search: query close to vector 1 returns it first.
    let hits = be.search(&[0.99, 0.01, 0.0, 0.0], 2).expect("search");
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].ref_id, 1);

    // Delete vector 1, search again: vector 2 wins.
    assert!(be.delete(1).expect("delete"));
    be.save().expect("persist after delete");
    let hits2 = be.search(&[0.99, 0.01, 0.0, 0.0], 1).expect("search 2");
    assert_eq!(hits2.len(), 1);
    assert_ne!(hits2[0].ref_id, 1);
}

#[test]
fn e2e_cross_addon_namespace_isolation() {
    let (_dir, pool) = open_pool();
    let root = TempDir::new().unwrap();
    let mgr = mgr_with_temproot(pool, root.path().to_path_buf());

    let a = mgr
        .get_or_create("addon_a", "faces", 3, Metric::Cosine)
        .unwrap();
    let b = mgr
        .get_or_create("addon_b", "faces", 3, Metric::Cosine)
        .unwrap();
    a.upsert(100, &[1.0, 0.0, 0.0]).unwrap();
    a.upsert(200, &[0.0, 1.0, 0.0]).unwrap();

    // addon_b's "faces" namespace is a different on-disk file: it sees
    // nothing of addon_a's data.
    assert_eq!(b.count(), 0);
    let hits = b.search(&[1.0, 0.0, 0.0], 5).unwrap();
    assert!(hits.is_empty());
}

#[test]
fn e2e_quota_enforcement_at_namespace_limit() {
    let (_dir, pool) = open_pool();
    let root = TempDir::new().unwrap();
    let mgr = mgr_with_temproot(pool, root.path().to_path_buf());

    for i in 0..MAX_NAMESPACES_PER_ADDON {
        mgr.get_or_create("addon_a", &format!("ns_{i}"), 4, Metric::Cosine)
            .unwrap();
    }
    // 11th namespace must be rejected by the quota check.
    let res = mgr.get_or_create("addon_a", "overflow", 4, Metric::Cosine);
    assert!(matches!(
        res,
        Err(VectorError::NamespaceQuotaExceeded { .. })
    ));
}

#[test]
fn e2e_persist_survives_manager_restart() {
    let (_dir, pool) = open_pool();
    let root = TempDir::new().unwrap();
    let root_path = root.path().to_path_buf();

    {
        let mgr = mgr_with_temproot(pool.clone(), root_path.clone());
        let be = mgr
            .get_or_create("addon_a", "attrs", 3, Metric::Cosine)
            .unwrap();
        be.upsert(42, &[1.0, 0.0, 0.0]).unwrap();
        be.save().unwrap();
    }

    // Build a fresh manager against the same DB + on-disk root.
    let mgr2 = mgr_with_temproot(pool, root_path);
    let be2 = mgr2.get("addon_a", "attrs").expect("reopen ns");
    assert_eq!(be2.count(), 1);
    let hits = be2.search(&[1.0, 0.0, 0.0], 1).unwrap();
    assert_eq!(hits[0].ref_id, 42);
}

#[test]
fn e2e_delete_namespace_clears_db_and_file() {
    let (_dir, pool) = open_pool();
    let root = TempDir::new().unwrap();
    let mgr = mgr_with_temproot(pool.clone(), root.path().to_path_buf());

    let be = mgr
        .get_or_create("addon_a", "scratch", 3, Metric::Cosine)
        .unwrap();
    be.upsert(1, &[1.0, 0.0, 0.0]).unwrap();
    be.save().unwrap();

    mgr.delete_namespace("addon_a", "scratch").expect("delete");

    // DB row gone.
    let conn = pool.lock().unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM addon_vector_namespaces WHERE addon_id='addon_a' AND namespace='scratch'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);

    // Subsequent get() returns NamespaceNotFound.
    drop(conn);
    let res = mgr.get("addon_a", "scratch");
    assert!(matches!(res, Err(VectorError::NamespaceNotFound { .. })));
}

#[test]
fn e2e_namespace_geometry_mismatch_rejected_on_reopen() {
    let (_dir, pool) = open_pool();
    let root = TempDir::new().unwrap();
    let mgr = mgr_with_temproot(pool, root.path().to_path_buf());

    mgr.get_or_create("addon_a", "faces", 512, Metric::Cosine)
        .unwrap();

    // Caller passes a different dim — must be rejected, not silently coerced.
    let res = mgr.get_or_create("addon_a", "faces", 768, Metric::Cosine);
    assert!(matches!(res, Err(VectorError::DimMismatch { .. })));

    // Different metric is also rejected.
    let res2 = mgr.get_or_create("addon_a", "faces", 512, Metric::Euclidean);
    assert!(matches!(res2, Err(VectorError::MetricMismatch { .. })));
}

// -----------------------------------------------------------------------------
// Sanity: AbiError variants we rely on still exist
// -----------------------------------------------------------------------------

#[test]
fn abi_error_codes_used_by_vector_api_are_stable() {
    // If any of these are renumbered the SDK + host fn agreements break.
    assert_eq!(AbiError::Permission.as_i32(), 1);
    assert_eq!(AbiError::NotFound.as_i32(), 2);
    assert_eq!(AbiError::Operation.as_i32(), 5);
    assert_eq!(AbiError::QuotaExceeded.as_i32(), 11);
    assert_eq!(AbiError::GateNotSatisfied.as_i32(), 22);
}

// Quiet unused-import warning for the std::sync re-export when Mutex/Arc
// happen not to be needed by an individual edit.
#[allow(dead_code)]
fn _silence_imports() -> (Arc<Mutex<()>>,) {
    (Arc::new(Mutex::new(())),)
}
