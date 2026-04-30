// ============ File: services_e2e.rs — light integration tests for the unified services pipeline ============
//
// These tests exercise the public surface that survived the N6/N7 refactor:
//
//   * `services::deploy::deploy()` writes both `services` and
//     `model_registry` atomically and surfaces the new endpoint.
//   * `services_repo::services::delete()` cascades to `model_registry`
//     thanks to the migration's `ON DELETE CASCADE`.
//   * `MeshServicesRegistry` accepts remote announces, exposes them
//     through `all_remote()`, and forgets a node on disconnect.
//
// We intentionally do not spin up the full `Supervisor` — its constructor
// pulls in tokio watch channels, the live-handles cache, and a port allocator
// the rest of the binary owns. The supervisor's first-tick contract is
// covered by its own unit tests (`services::supervisor::tests`).

use std::sync::{Arc, Mutex};

use rusqlite::Connection;
use serde_json::json;
use tentaflow_core::db;
use tentaflow_core::services::deploy::{deploy, DeployError};
use tentaflow_core::services::manifest::{
    ApiKind, Category, DeploySection, Engine, ModelPreset, NativeDeploy, NativeRuntime,
    ServiceManifest, TargetOs,
};
use tentaflow_core::services::mesh_registry::MeshServicesRegistry;
use tentaflow_core::services::ports::PortAllocator;
use tentaflow_core::services_repo::services as services_repo;
use tentaflow_core::services_repo::services::DeployMethod;
use tentaflow_protocol::{ServiceInfo, ServiceModelEntry};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn test_db() -> db::DbPool {
    let conn = Connection::open_in_memory().expect("open in-memory db");
    conn.execute_batch("PRAGMA foreign_keys=ON;")
        .expect("enable FK");
    db::migrations::run(&conn).expect("run migrations");
    Arc::new(Mutex::new(conn))
}

fn dummy_embedded_manifest(id: &str) -> ServiceManifest {
    ServiceManifest {
        engine: Engine {
            id: id.into(),
            category: Category::Llm,
            name: id.into(),
            description_pl: String::new(),
            description_en: String::new(),
            homepage: String::new(),
            license: String::new(),
            icon: None,
            resource_kind: None,
            requires_model: None,
            gpu_supported: None,
            default_port: 0,
            api: ApiKind::OpenaiCompatible,
            version: "0.0.1".into(),
        },
        deploy: DeploySection {
            docker: None,
            native: Some(NativeDeploy {
                platforms: vec![TargetOs::Linux, TargetOs::Macos, TargetOs::Windows],
                runtime: NativeRuntime::Embedded,
                feature_flag: None,
                binary_path: None,
                bundle_path: None,
            }),
            external: None,
        },
        model_presets: vec![ModelPreset {
            id: "preset-a".into(),
            display_name: "Preset A".into(),
            repo: "org/model".into(),
            quantization: None,
            recommended: true,
        }],
        docker_source_hash: String::new(),
        native_source_hash: String::new(),
    }
}

fn fake_service_info(id: i64, node_id: &str, model_name: &str) -> ServiceInfo {
    ServiceInfo {
        id,
        node_id: node_id.into(),
        engine_id: "fake-engine".into(),
        category: "llm".into(),
        display_name: "Fake".into(),
        deploy_method: "native_embedded".into(),
        transport: "embedded".into(),
        status: "running".into(),
        pinned: false,
        paused: false,
        runtime_pid: None,
        runtime_port: None,
        sidecar_quic_port: None,
        endpoint_url: None,
        restart_count: 0,
        health_last_err: None,
        models: vec![ServiceModelEntry {
            model_name: model_name.into(),
            display_name: None,
            capabilities: vec!["chat".into()],
            context_length: None,
            quantization: None,
            is_default: true,
        }],
        created_at: String::new(),
        updated_at: String::new(),
    }
}

// ---------------------------------------------------------------------------
// Deploy persistence
// ---------------------------------------------------------------------------

#[tokio::test]
async fn deploy_embedded_persists_to_services_and_model_registry() {
    let db = test_db();
    let ports = Arc::new(PortAllocator::new((45_900, 45_999), Default::default()).unwrap());
    let manifest = dummy_embedded_manifest("emb-e2e-persist");

    let outcome = deploy(
        DeployMethod::NativeEmbedded,
        &manifest,
        &json!({}),
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("embedded deploy succeeds");

    let sid = outcome.endpoint.handle.id;
    assert!(sid > 0);

    let conn = db.lock().unwrap();
    let row = services_repo::get(&conn, sid)
        .unwrap()
        .expect("services row");
    assert_eq!(row.engine_id, "emb-e2e-persist");
    assert_eq!(
        row.status,
        tentaflow_core::services_repo::services::ServiceStatus::Running
    );

    let model_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM model_registry WHERE service_id = ?1",
            rusqlite::params![sid],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(model_count, 1, "model preset propagated to model_registry");
}

// ---------------------------------------------------------------------------
// Delete cascades
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_service_cascades_to_model_registry() {
    let db = test_db();
    let ports = Arc::new(PortAllocator::new((45_700, 45_799), Default::default()).unwrap());
    let manifest = dummy_embedded_manifest("emb-e2e-cascade");

    let outcome = deploy(
        DeployMethod::NativeEmbedded,
        &manifest,
        &json!({}),
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect("seed deploy");
    let sid = outcome.endpoint.handle.id;

    {
        let conn = db.lock().unwrap();
        services_repo::delete(&conn, sid).unwrap();
    }

    let conn = db.lock().unwrap();
    assert!(
        services_repo::get(&conn, sid).unwrap().is_none(),
        "service row removed"
    );
    let leftovers: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM model_registry WHERE service_id = ?1",
            rusqlite::params![sid],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(leftovers, 0, "model_registry cascaded on FK");
}

// ---------------------------------------------------------------------------
// Mesh registry
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mesh_registry_replace_node_announces_remote_services() {
    let registry = MeshServicesRegistry::new();
    let services = vec![fake_service_info(42, "remote-B", "qwen3.5-0.8b")];
    registry.replace_node("remote-B".to_string(), services);

    assert_eq!(registry.remote_node_count(), 1);
    let all = registry.all_remote();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].0, "remote-B");
    assert_eq!(all[0].1.len(), 1);
    assert_eq!(all[0].1[0].id, 42);

    // unique_models must surface the announced model
    let models = registry.unique_models();
    assert!(models.iter().any(|m| m.model_name == "qwen3.5-0.8b"));
}

#[tokio::test]
async fn mesh_registry_remove_node_invalidates_remote_entries() {
    let registry = MeshServicesRegistry::new();
    registry.replace_node("B".to_string(), vec![fake_service_info(1, "B", "model-x")]);
    assert_eq!(registry.remote_node_count(), 1);

    registry.remove_node("B");

    assert_eq!(registry.remote_node_count(), 0);
    assert!(registry.all_remote().is_empty());
    assert!(registry.find_node_for_model("model-x").is_none());
}

// ---------------------------------------------------------------------------
// External strategy gating
// ---------------------------------------------------------------------------

/// External deploy must reject manifests without `[deploy.external]`. The
/// strategy ships in N7.5 and has no other place to be exercised end-to-end
/// without a live Ollama daemon — this test guards the manifest gate.
#[tokio::test]
async fn external_deploy_rejects_manifest_without_external_section() {
    let db = test_db();
    let ports = Arc::new(PortAllocator::new((45_500, 45_599), Default::default()).unwrap());
    // Embedded manifest — no [deploy.external].
    let manifest = dummy_embedded_manifest("emb-no-external");

    let err = deploy(
        DeployMethod::External,
        &manifest,
        &json!({}),
        &ports,
        &db,
        None,
        None,
    )
    .await
    .expect_err("external method on embedded manifest must fail");

    assert!(
        matches!(err, DeployError::Manifest(_)),
        "expected Manifest error, got {:?}",
        err
    );

    // The audit row must reflect the failure.
    let conn = db.lock().unwrap();
    let (status, _err): (String, Option<String>) = conn
        .query_row(
            "SELECT status, error_text FROM deployments \
             WHERE engine_id = 'emb-no-external' ORDER BY id DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(status, "failed");
}
