// =============================================================================
// File: tests/flow_install_compile.rs — F1c P5 chunk A install + registry e2e
// =============================================================================
//
// Builds a temporary addon directory with a minimal manifest.toml + one
// `*.flow.json`, runs `lifecycle::install`, and asserts:
//   * install succeeds for a valid flow,
//   * the registry exposes the flow under (addon_id, flow_id),
//   * install fails when the flow.json contains a cycle (DB row is rolled
//     back implicitly because the compile happens before the INSERT).

use std::io::Write;
use std::path::Path;

use tentaflow_core::addon::lifecycle::install;
use tentaflow_core::db::{init as db_init, DbPool};
use tentaflow_core::flow_runtime::registry;

const MANIFEST: &str = r#"
[addon]
id = "flow-runtime-test"
version = "1.0.0"
display_name = "Flow Runtime Test"
wasm_file = "addon.wasm"

[[flow_template]]
id = "test-flow"
display_name = "Test Flow"
path = "flows/test.flow.json"
description = "minimal source->sink"
"#;

const GOOD_FLOW: &str = r#"{
    "schema_version": 1,
    "id": "test-flow",
    "operators": [
        { "id": "src", "type": "Source", "params": {} },
        { "id": "snk", "type": "Sink",   "params": {} }
    ],
    "edges": [
        { "from": "src", "to": "snk" }
    ],
    "is_long_running": false,
    "max_runtime_ms": 0
}"#;

const CYCLIC_FLOW: &str = r#"{
    "schema_version": 1,
    "id": "test-flow",
    "operators": [
        { "id": "a", "type": "Source",    "params": {} },
        { "id": "b", "type": "Threshold", "params": {} },
        { "id": "c", "type": "Sink",      "params": {} }
    ],
    "edges": [
        { "from": "a", "to": "b" },
        { "from": "b", "to": "c" },
        { "from": "c", "to": "a" }
    ]
}"#;

fn write_file(path: &Path, content: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir -p");
    }
    let mut f = std::fs::File::create(path).expect("create");
    f.write_all(content).expect("write");
}

fn build_addon_dir(tmp: &Path, flow_json: &str) {
    write_file(&tmp.join("manifest.toml"), MANIFEST.as_bytes());
    write_file(&tmp.join("addon.wasm"), b"\x00asm\x01\x00\x00\x00");
    write_file(&tmp.join("flows/test.flow.json"), flow_json.as_bytes());
}

fn fresh_db() -> DbPool {
    db_init(Path::new(":memory:")).expect("db init")
}

#[test]
fn install_compiles_and_registers_flow() {
    // Make sure the registry does not carry state from another test run.
    registry::global().unregister_addon("flow-runtime-test");

    let tmp = tempfile::tempdir().expect("tempdir");
    build_addon_dir(tmp.path(), GOOD_FLOW);

    let db = fresh_db();
    let manifest = install(tmp.path(), &db).expect("install should succeed");
    assert_eq!(manifest.addon_id, "flow-runtime-test");
    assert_eq!(manifest.flow_templates.len(), 1);

    let listed = registry::global().list_for_addon("flow-runtime-test");
    assert_eq!(listed, vec!["test-flow".to_string()]);
    let compiled = registry::global()
        .get("flow-runtime-test", "test-flow")
        .expect("flow registered");
    assert_eq!(compiled.def.operators.len(), 2);
    assert_eq!(compiled.topo_order, vec!["src".to_string(), "snk".to_string()]);

    // Cleanup so other tests do not see this entry.
    registry::global().unregister_addon("flow-runtime-test");
}

#[test]
fn install_rejects_cyclic_flow() {
    registry::global().unregister_addon("flow-runtime-test");

    let tmp = tempfile::tempdir().expect("tempdir");
    build_addon_dir(tmp.path(), CYCLIC_FLOW);

    let db = fresh_db();
    let err = install(tmp.path(), &db).expect_err("install must fail on cycle");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("compile failed") && msg.to_lowercase().contains("cycle"),
        "expected compile/cycle error, got: {msg}"
    );

    assert!(
        registry::global()
            .get("flow-runtime-test", "test-flow")
            .is_none(),
        "flow must not be registered after install fail"
    );
}
