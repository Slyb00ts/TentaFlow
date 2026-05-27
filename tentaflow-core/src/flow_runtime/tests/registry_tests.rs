// =============================================================================
// File: flow_runtime/tests/registry_tests.rs — registry insert/lookup/cleanup
// =============================================================================

use std::sync::Arc;

use crate::flow_runtime::parser::{compile, parse_flow_definition};
use crate::flow_runtime::registry::FlowRegistry;
use crate::flow_runtime::types::CompiledFlow;

fn make_flow(id: &str) -> Arc<CompiledFlow> {
    let json = format!(
        r#"{{
            "schema_version": 1,
            "id": "{id}",
            "operators": [
                {{ "id": "src", "type": "Source", "params": {{}} }},
                {{ "id": "snk", "type": "Sink",   "params": {{}} }}
            ],
            "edges": [ {{ "from": "src", "to": "snk" }} ]
        }}"#
    );
    let def = parse_flow_definition(&json).expect("parse");
    Arc::new(compile(def).expect("compile"))
}

#[test]
fn register_and_get() {
    let reg = FlowRegistry::new();
    reg.register("addon-a", make_flow("flow-1"));
    let hit = reg.get("addon-a", "flow-1").expect("registered flow");
    assert_eq!(hit.def.id, "flow-1");
    assert!(reg.get("addon-a", "flow-2").is_none());
    assert!(reg.get("addon-b", "flow-1").is_none());
}

#[test]
fn unregister_addon_drops_all_flows() {
    let reg = FlowRegistry::new();
    reg.register("addon-a", make_flow("flow-1"));
    reg.register("addon-a", make_flow("flow-2"));
    reg.register("addon-b", make_flow("flow-3"));

    let removed = reg.unregister_addon("addon-a");
    assert_eq!(removed, 2);
    assert!(reg.get("addon-a", "flow-1").is_none());
    assert!(reg.get("addon-a", "flow-2").is_none());
    assert!(reg.get("addon-b", "flow-3").is_some());
}

#[test]
fn list_for_addon_returns_only_owned() {
    let reg = FlowRegistry::new();
    reg.register("addon-a", make_flow("alpha"));
    reg.register("addon-a", make_flow("beta"));
    reg.register("addon-b", make_flow("gamma"));

    let listed = reg.list_for_addon("addon-a");
    assert_eq!(listed, vec!["alpha".to_string(), "beta".to_string()]);
    assert_eq!(reg.list_for_addon("addon-b"), vec!["gamma".to_string()]);
    assert!(reg.list_for_addon("addon-c").is_empty());
}
