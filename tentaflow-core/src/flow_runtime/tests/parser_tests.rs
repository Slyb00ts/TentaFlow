// =============================================================================
// File: flow_runtime/tests/parser_tests.rs — schema + cycle detection coverage
// =============================================================================

use crate::flow_runtime::parser::{compile, parse_flow_definition};
use crate::flow_runtime::types::{FlowCompileError, MAX_OPERATORS_PER_FLOW};

fn minimal_flow() -> &'static str {
    r#"{
        "schema_version": 1,
        "id": "minimal",
        "operators": [
            { "id": "src", "type": "Source", "params": {} },
            { "id": "snk", "type": "Sink",   "params": {} }
        ],
        "edges": [
            { "from": "src", "to": "snk" }
        ],
        "is_long_running": false,
        "max_runtime_ms": 0
    }"#
}

#[test]
fn parse_minimal_flow_ok() {
    let def = parse_flow_definition(minimal_flow()).expect("parse");
    let compiled = compile(def).expect("compile");
    assert_eq!(compiled.def.id, "minimal");
    assert_eq!(compiled.topo_order, vec!["src".to_string(), "snk".to_string()]);
    assert_eq!(compiled.adjacency["src"], vec!["snk".to_string()]);
    assert!(compiled.adjacency["snk"].is_empty());
}

#[test]
fn reject_unsupported_schema_version() {
    let json = r#"{ "schema_version": 2, "id": "x", "operators": [], "edges": [] }"#;
    let def = parse_flow_definition(json).expect("parse");
    match compile(def) {
        Err(FlowCompileError::UnsupportedSchemaVersion { found: 2 }) => {}
        other => panic!("expected UnsupportedSchemaVersion(2), got {other:?}"),
    }
}

#[test]
fn reject_empty_operators() {
    let json = r#"{ "schema_version": 1, "id": "x", "operators": [], "edges": [] }"#;
    let def = parse_flow_definition(json).expect("parse");
    match compile(def) {
        Err(FlowCompileError::EmptyFlow) => {}
        other => panic!("expected EmptyFlow, got {other:?}"),
    }
}

#[test]
fn reject_too_many_operators() {
    let count = MAX_OPERATORS_PER_FLOW + 1;
    let mut ops = String::new();
    for i in 0..count {
        if i > 0 {
            ops.push(',');
        }
        ops.push_str(&format!(
            r#"{{ "id": "op{i}", "type": "Source", "params": {{}} }}"#
        ));
    }
    let json = format!(
        r#"{{ "schema_version": 1, "id": "x", "operators": [{ops}], "edges": [] }}"#
    );
    let def = parse_flow_definition(&json).expect("parse");
    match compile(def) {
        Err(FlowCompileError::TooManyOperators { count: n }) if n == count => {}
        other => panic!("expected TooManyOperators({count}), got {other:?}"),
    }
}

#[test]
fn reject_edge_unknown_op() {
    let json = r#"{
        "schema_version": 1,
        "id": "x",
        "operators": [
            { "id": "src", "type": "Source", "params": {} }
        ],
        "edges": [
            { "from": "src", "to": "ghost" }
        ]
    }"#;
    let def = parse_flow_definition(json).expect("parse");
    match compile(def) {
        Err(FlowCompileError::EdgeReferencesUnknownOperator { edge_idx: 0, op_id })
            if op_id == "ghost" => {}
        other => panic!("expected EdgeReferencesUnknownOperator, got {other:?}"),
    }
}

#[test]
fn reject_cycle_3_nodes() {
    let json = r#"{
        "schema_version": 1,
        "id": "x",
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
    let def = parse_flow_definition(json).expect("parse");
    match compile(def) {
        Err(FlowCompileError::Cycle { involved }) => {
            assert!(
                involved.contains(&"a".to_string())
                    && involved.contains(&"b".to_string())
                    && involved.contains(&"c".to_string()),
                "cycle should mention a, b, c — got {involved:?}"
            );
        }
        other => panic!("expected Cycle, got {other:?}"),
    }
}

#[test]
fn reject_port_on_non_branch() {
    let json = r#"{
        "schema_version": 1,
        "id": "x",
        "operators": [
            { "id": "src", "type": "Source", "params": {} },
            { "id": "snk", "type": "Sink",   "params": {} }
        ],
        "edges": [
            { "from": "src", "to": "snk", "port": "true" }
        ]
    }"#;
    let def = parse_flow_definition(json).expect("parse");
    match compile(def) {
        Err(FlowCompileError::PortOnNonBranch { edge_idx: 0, op_id, port })
            if op_id == "src" && port == "true" => {}
        other => panic!("expected PortOnNonBranch, got {other:?}"),
    }
}

#[test]
fn reject_invalid_port_value() {
    let json = r#"{
        "schema_version": 1,
        "id": "x",
        "operators": [
            { "id": "br",  "type": "Branch", "params": {} },
            { "id": "snk", "type": "Sink",   "params": {} }
        ],
        "edges": [
            { "from": "br", "to": "snk", "port": "maybe" }
        ]
    }"#;
    let def = parse_flow_definition(json).expect("parse");
    match compile(def) {
        Err(FlowCompileError::InvalidPort { edge_idx: 0, port }) if port == "maybe" => {}
        other => panic!("expected InvalidPort, got {other:?}"),
    }
}
