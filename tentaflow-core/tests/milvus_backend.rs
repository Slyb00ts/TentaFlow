#![cfg(feature = "vector-milvus")]
// =============================================================================
// File: tests/milvus_backend.rs
// Purpose: Live integration test for the external Milvus backend. Ignored by
//          default — requires a running Milvus (set MILVUS_URL, defaults to
//          http://localhost:19530). Run with:
//            cargo test --features vector-milvus --test milvus_backend -- --ignored
// =============================================================================

use std::time::Duration;

use rusqlite::params;
use tentaflow_core::services::vector::{Metric, MilvusBackend, NamespaceManager, VectorBackend};

fn url() -> String {
    std::env::var("MILVUS_URL").unwrap_or_else(|_| "http://localhost:19530".to_string())
}

// Milvus makes freshly-inserted rows queryable asynchronously; retry the search
// briefly so the test is not racy against segment flush/visibility.
fn search_until_nonempty(
    be: &dyn VectorBackend,
    q: &[f32],
    k: usize,
) -> Vec<tentaflow_core::services::vector::SearchHit> {
    // Freshly-inserted rows become searchable after a short consistency delay,
    // and the client surfaces "no results" as an Err — retry on both.
    for _ in 0..40 {
        if let Ok(hits) = be.search(q, k, None, &[]) {
            if !hits.is_empty() {
                return hits;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Vec::new()
}

#[test]
#[ignore = "requires a running Milvus server (set MILVUS_URL)"]
fn milvus_insert_search_delete_roundtrip() {
    let collection = format!("tf_it_{}", std::process::id());
    let be = MilvusBackend::connect(
        &url(),
        None,
        None,
        &collection,
        4,
        Metric::Cosine,
        &[],
        false,
    )
    .expect("connect");

    be.upsert(10, &[1.0, 0.0, 0.0, 0.0], &[], None)
        .expect("upsert 10");
    be.upsert(20, &[0.0, 1.0, 0.0, 0.0], &[], None)
        .expect("upsert 20");
    be.upsert(30, &[0.0, 0.0, 1.0, 0.0], &[], None)
        .expect("upsert 30");

    let hits = search_until_nonempty(&be, &[0.9, 0.1, 0.0, 0.0], 2);
    assert!(!hits.is_empty(), "search returned no hits after retries");
    assert_eq!(hits[0].ref_id, 10, "nearest neighbour should be id 10");

    assert!(be.has_ref(10), "has_ref(10) should be true after upsert");
    assert!(be.delete(10).expect("delete 10"));

    assert_eq!(be.dim(), 4);
    assert_eq!(be.metric(), Metric::Cosine);
}

#[test]
#[ignore = "requires a running Milvus server (set MILVUS_URL)"]
fn milvus_metadata_fields_filter_and_return() {
    use tentaflow_sdk_spec::{Field, FieldSpec, FieldType, FieldValue, Filter};

    let collection = format!("tf_meta_{}", std::process::id());
    let schema = vec![
        FieldSpec {
            name: "source".into(),
            field_type: FieldType::Str,
            indexed: true,
        },
        FieldSpec {
            name: "score".into(),
            field_type: FieldType::Int,
            indexed: true,
        },
    ];
    let be = MilvusBackend::connect(
        &url(),
        None,
        None,
        &collection,
        4,
        Metric::Cosine,
        &schema,
        false,
    )
    .expect("connect with schema");

    be.upsert(
        1,
        &[1.0, 0.0, 0.0, 0.0],
        &[
            Field {
                name: "source".into(),
                value: FieldValue::Str("inbox".into()),
            },
            Field {
                name: "score".into(),
                value: FieldValue::Int(42),
            },
        ],
        None,
    )
    .expect("upsert 1");
    be.upsert(
        2,
        &[0.0, 1.0, 0.0, 0.0],
        &[
            Field {
                name: "source".into(),
                value: FieldValue::Str("web".into()),
            },
            Field {
                name: "score".into(),
                value: FieldValue::Int(5),
            },
        ],
        None,
    )
    .expect("upsert 2");

    // Filter source == 'inbox' AND score >= 10 -> only id 1; return both fields.
    let filter = Filter::And(vec![
        Filter::Eq("source".into(), FieldValue::Str("inbox".into())),
        Filter::Gte("score".into(), FieldValue::Int(10)),
    ]);
    let mut hits = Vec::new();
    for _ in 0..40 {
        if let Ok(h) = be.search(
            &[0.95, 0.05, 0.0, 0.0],
            5,
            Some(&filter),
            &["source".to_string(), "score".to_string()],
        ) {
            if !h.is_empty() {
                hits = h;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert_eq!(
        hits.len(),
        1,
        "only id 1 matches source=inbox AND score>=10"
    );
    assert_eq!(hits[0].ref_id, 1);
    let src = hits[0].fields.iter().find(|f| f.name == "source");
    assert!(matches!(src.map(|f| &f.value), Some(FieldValue::Str(s)) if s == "inbox"));
    let score = hits[0].fields.iter().find(|f| f.name == "score");
    assert!(matches!(score.map(|f| &f.value), Some(FieldValue::Int(42))));
}

#[test]
#[ignore = "requires a running Milvus server (set MILVUS_URL)"]
fn milvus_hybrid_search_dense_plus_sparse() {
    use tentaflow_sdk_spec::{Fusion, SparseVector};

    let collection = format!("tf_hybrid_{}", std::process::id());
    let be = MilvusBackend::connect(
        &url(),
        None,
        None,
        &collection,
        4,
        Metric::Cosine,
        &[],
        true,
    )
    .expect("connect with sparse");

    be.upsert(
        1,
        &[1.0, 0.0, 0.0, 0.0],
        &[],
        Some(&SparseVector {
            indices: vec![100, 200],
            values: vec![0.9, 0.1],
        }),
    )
    .expect("upsert 1");
    be.upsert(
        2,
        &[0.0, 1.0, 0.0, 0.0],
        &[],
        Some(&SparseVector {
            indices: vec![300, 400],
            values: vec![0.8, 0.2],
        }),
    )
    .expect("upsert 2");

    // Dense near X (doc 1) + sparse term 300 (doc 2). RRF should surface both.
    let mut hits = Vec::new();
    for _ in 0..40 {
        if let Ok(h) = be.hybrid_search(
            &[0.9, 0.1, 0.0, 0.0],
            &SparseVector {
                indices: vec![300],
                values: vec![1.0],
            },
            5,
            None,
            &[],
            Fusion::Rrf(60),
        ) {
            if h.len() >= 2 {
                hits = h;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let ids: std::collections::HashSet<u64> = hits.iter().map(|h| h.ref_id).collect();
    assert!(
        ids.contains(&1) && ids.contains(&2),
        "hybrid should fuse dense + sparse"
    );
}

#[test]
#[ignore = "requires a running Milvus server (set MILVUS_URL)"]
fn namespace_manager_routes_to_milvus_per_addon_config() {
    // Admin selects Milvus for a specific addon via reserved addon_config keys;
    // the NamespaceManager must then build a Milvus-backed namespace for it.
    let dir = tempfile::TempDir::new().unwrap();
    let root = tempfile::TempDir::new().unwrap();
    let pool = tentaflow_core::db::init(&dir.path().join("test.db")).expect("init db");
    {
        let conn = pool.write().unwrap();
        // Structured `__vector_config` (manual external Milvus) — the format the
        // backend picker persists and NamespaceManager reads.
        let cfg = format!(
            r#"{{"backend":"milvus","milvus_source":"manual","manual_uri":"{}"}}"#,
            url()
        );
        conn.execute(
            "INSERT INTO addon_config (addon_id, key, value, is_secret, updated_at) \
             VALUES (?1, ?2, ?3, 0, datetime('now'))",
            params!["addon_milvus", "__vector_config", cfg],
        )
        .unwrap();
    }

    let mgr = NamespaceManager::with_root(pool, root.path().to_path_buf());
    let be = mgr
        .get_or_create(
            "org-test",
            "addon_milvus",
            "faces",
            4,
            Metric::Cosine,
            &[],
            false,
        )
        .expect("get_or_create routed to milvus");

    be.upsert(101, &[1.0, 0.0, 0.0, 0.0], &[], None)
        .expect("upsert via milvus");
    be.upsert(202, &[0.0, 1.0, 0.0, 0.0], &[], None)
        .expect("upsert via milvus");

    let hits = search_until_nonempty(be.as_ref(), &[0.95, 0.05, 0.0, 0.0], 1);
    assert!(
        !hits.is_empty(),
        "manager-routed Milvus search returned nothing"
    );
    assert_eq!(hits[0].ref_id, 101);
}
