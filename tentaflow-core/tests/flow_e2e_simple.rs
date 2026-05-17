// =============================================================================
// File: tests/flow_e2e_simple.rs — minimal Source→Threshold→Sink integration
// =============================================================================
//
// Drives a complete `flow_invoke`-equivalent path: register a compiled flow
// in the process-wide `registry`, invoke via `FlowScheduler`, assert the
// resulting `result_toml` contains exactly the records the threshold filter
// allowed through. Exercises operator dispatch + edge propagation + sink
// collection + finalize DB write in one pass.

use std::path::Path;
use std::sync::Arc;

use tentaflow_core::db::{init as init_db, DbPool};
use tentaflow_core::flow_runtime::parser::{compile, parse_flow_definition};
use tentaflow_core::flow_runtime::registry;
use tentaflow_core::flow_runtime::scheduler::FlowScheduler;
use tentaflow_core::flow_runtime::types::CompiledFlow;

fn fresh_db() -> DbPool {
    init_db(Path::new(":memory:")).expect("test db")
}

fn unique_addon() -> String {
    format!("flow-e2e-{}", uuid::Uuid::new_v4())
}

fn compile_flow(json: &str) -> Arc<CompiledFlow> {
    Arc::new(compile(parse_flow_definition(json).expect("parse")).expect("compile"))
}

#[tokio::test]
async fn source_threshold_sink_end_to_end() {
    let db = fresh_db();
    let sched = Arc::new(FlowScheduler::new(db.clone()));
    let addon = unique_addon();
    let flow_id = format!("flow-{}", uuid::Uuid::new_v4());

    let json = format!(
        r#"{{
            "schema_version": 1,
            "id": "{flow_id}",
            "operators": [
                {{ "id": "src", "type": "Source",
                   "params": {{ "stream": "input", "count": 3 }} }},
                {{ "id": "thr", "type": "Threshold",
                   "params": {{ "field": "v", "min": 0.5 }} }},
                {{ "id": "snk", "type": "Sink",
                   "params": {{ "kind": "invocation_result" }} }}
            ],
            "edges": [
                {{ "from": "src", "to": "thr" }},
                {{ "from": "thr", "to": "snk" }}
            ]
        }}"#
    );
    registry::global().register(&addon, compile_flow(&json));

    // Pass case: v=0.9 above threshold → 3 records survive.
    let input_pass = toml::from_str::<toml::Value>("v = 0.9").unwrap();
    let s_pass = sched
        .invoke(&addon, &flow_id, input_pass, 5_000)
        .await
        .expect("invoke pass");
    assert_eq!(s_pass.status, "completed");
    let pass_records: toml::Value =
        toml::from_str(s_pass.result_toml.as_deref().unwrap()).expect("decode");
    let pass_arr = pass_records
        .get("records")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(pass_arr.len(), 3, "expected 3 records past threshold");

    // Drop case: v=0.1 below threshold → 0 records.
    let input_drop = toml::from_str::<toml::Value>("v = 0.1").unwrap();
    let s_drop = sched
        .invoke(&addon, &flow_id, input_drop, 5_000)
        .await
        .expect("invoke drop");
    assert_eq!(s_drop.status, "completed");
    let drop_records: toml::Value =
        toml::from_str(s_drop.result_toml.as_deref().unwrap()).expect("decode");
    let drop_arr = drop_records
        .get("records")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(drop_arr.is_empty(), "expected zero records, got {drop_arr:?}");
}
