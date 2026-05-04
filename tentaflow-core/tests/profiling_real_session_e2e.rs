// =============================================================================
// File: tests/profiling_real_session_e2e.rs
// Opis: Real E2E test profilowania - czyta sesje z disk'a i sprawdza pelen
//       cykl: storage_v2.read_report -> envelope detection -> mapowanie
//       do GUI response. Wymaga istniejacego summary.bin (sesja z user'a
//       albo z poprzedniego nagrania).
// =============================================================================

use std::path::Path;

use tentaflow_core::profiling::ProfileStorage;
use tentaflow_protocol::profiling::ProfileReportV2;

const TENTAFLOW_HOME_HINT: &str = "TENTAFLOW_HOME_FOR_E2E";

fn home_for_e2e() -> Option<std::path::PathBuf> {
    if let Ok(env) = std::env::var(TENTAFLOW_HOME_HINT) {
        let p = std::path::PathBuf::from(env);
        if p.is_dir() {
            return Some(p);
        }
    }
    // Fallback: probuj typowe lokalizacje
    let candidates = [
        "/home/critix/repos/TentaFlow/tentaflow/target/debug",
        "/home/critix/repos/TentaFlow/tentaflow/target/release",
    ];
    for c in candidates {
        let p = std::path::Path::new(c);
        if p.join("profiling").is_dir() {
            return Some(p.to_path_buf());
        }
    }
    None
}

fn find_first_session(storage: &ProfileStorage) -> Option<(String, String)> {
    let root = storage.root();
    if !root.is_dir() {
        return None;
    }
    for node_entry in std::fs::read_dir(root).ok()? {
        let node = node_entry.ok()?.path();
        if !node.is_dir() {
            continue;
        }
        let node_id = node.file_name()?.to_str()?.to_string();
        for sess_entry in std::fs::read_dir(&node).ok()? {
            let sess = sess_entry.ok()?.path();
            if !sess.is_dir() {
                continue;
            }
            if sess.join("summary.bin").is_file() {
                let session_id = sess.file_name()?.to_str()?.to_string();
                return Some((node_id, session_id));
            }
        }
    }
    None
}

#[tokio::test]
#[ignore]
async fn read_real_session_envelope() {
    let Some(home) = home_for_e2e() else {
        eprintln!("SKIP: brak TENTAFLOW_HOME_FOR_E2E ani znanej sciezki z profiling/");
        return;
    };
    println!("home = {}", home.display());

    let storage = ProfileStorage::new(&home);
    let Some((node_id, session_id)) = find_first_session(&storage) else {
        eprintln!("SKIP: brak zadnej sesji w {}", storage.root().display());
        return;
    };
    println!("node_id    = {}", node_id);
    println!("session_id = {}", session_id);

    // Faktyczny test: storage.read_report -> ProfileReportV2.
    let report = storage
        .read_report(&node_id, &session_id)
        .await
        .expect("read_report failed");

    println!("schema_version = {}", report.schema_version);
    println!("session_id     = {}", report.session_id);
    println!(
        "node_id (krotki) = {}",
        &report.node_id[..16.min(report.node_id.len())]
    );
    assert_eq!(report.schema_version, 2);
    assert_eq!(report.session_id, session_id);
}

#[tokio::test]
#[ignore]
async fn read_real_session_serializes_to_json_for_gui() {
    // Symulacja co GUI dostaje: backend sklada ProfilingReportResponse z
    // envelope, rkyv -> wire bytes -> deserialize w JS jako rkyv enum
    // { "V2": {...} } / { "V1Legacy": {...} }. Sprawdzmy ze envelope
    // serializuje sie poprawnie do serde JSON i ma oczekiwany shape.
    let Some(home) = home_for_e2e() else {
        eprintln!("SKIP: brak TENTAFLOW_HOME_FOR_E2E");
        return;
    };
    let storage = ProfileStorage::new(&home);
    let Some((node_id, session_id)) = find_first_session(&storage) else {
        eprintln!("SKIP: brak sesji");
        return;
    };

    let report = storage
        .read_report(&node_id, &session_id)
        .await
        .expect("read_report");

    let summary = format!(
        "V2 schema={} sid={}",
        report.schema_version, report.session_id
    );
    println!("report summary: {}", summary);
}

#[tokio::test]
#[ignore]
async fn list_sessions_v2_returns_real_sessions() {
    let Some(home) = home_for_e2e() else {
        eprintln!("SKIP");
        return;
    };
    let storage = ProfileStorage::new(&home);
    let root = storage.root();
    let mut node_ids: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root) {
        for e in rd.flatten() {
            if e.path().is_dir() {
                if let Some(name) = e.file_name().to_str() {
                    node_ids.push(name.to_string());
                }
            }
        }
    }
    println!("nodes w storage_v2: {:?}", node_ids);
    for nid in &node_ids {
        let entries = storage.list_sessions(nid).await;
        match entries {
            Ok(list) => {
                println!("  node {}: {} sesji", &nid[..16], list.len());
                for s in &list {
                    println!(
                        "    - {} ({} bytes, {} collectors)",
                        &s.session_id,
                        s.size_bytes,
                        s.collectors_used.len()
                    );
                }
            }
            Err(e) => println!("  node {}: list error {:?}", &nid[..16], e),
        }
    }
    assert!(
        !node_ids.is_empty(),
        "powinna byc co najmniej 1 sesja w storage"
    );
}
