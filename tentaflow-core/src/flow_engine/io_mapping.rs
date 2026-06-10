// ===== File: flow_engine/io_mapping.rs — generic Camunda-style io-mapping over CEL (HARNESS_PLAN §3.12) =====
//
// One seam, every adapter. The executor wraps each node's `execute` with two
// optional config-driven steps:
//   * `input_mapping: {<config_key>: "<CEL>"}` — evaluated against the INBOUND
//     envelope BEFORE execute, results overlaid onto the node's config so any
//     block (incl. `addon.*`) can compute config from variables
//     (`llm.model = vars.chosen_model`) without touching one adapter.
//   * `output_mapping: {<variable>: "<CEL>"}` — evaluated against the node's
//     RESULT envelope AFTER execute, results written into `result.variables`.
//
// Both keys live under `node.config`. Absence of either key is a no-op (zero
// cost: no scope is built, no clone happens). An expression failure surfaces as
// a node error carrying node name + expression + cause (§3.12).

use std::collections::BTreeMap;

use serde_json::Value;

use super::envelope::{FlowEnvelope, FlowValue};
use super::expr::{evaluate, flow_value_to_json, ExprScope};
use super::types::FlowNode;

/// Config keys reserved for io-mapping declarations. Stripped from the config
/// overlay so a node never sees them as real settings.
const INPUT_MAPPING_KEY: &str = "input_mapping";
const OUTPUT_MAPPING_KEY: &str = "output_mapping";

/// Builds a read-only CEL scope from an envelope. `extras` carry loop/map
/// locals (`item`, `index`, `iteration`) bound as top-level variables; phase-4
/// callers pass an empty slice — the loop/map blocks of phase 5 fill it.
pub fn scope_from_envelope<'a>(
    envelope: &'a FlowEnvelope,
    extras: &'a [(&'a str, Value)],
) -> ExprScope<'a> {
    ExprScope {
        vars: &envelope.variables,
        payload: &envelope.payload,
        artifacts: &envelope.artifacts,
        meta: &envelope.meta,
        extras,
    }
}

/// Evaluates `node.config.input_mapping` against `inbound` and returns the
/// node config with the results overlaid (top-level key merge). Returns the
/// config unchanged (no clone) when no `input_mapping` is present. Errors
/// carry the node name so the executor surfaces "node '<id>': <expr error>".
pub fn apply_input_mapping(node: &FlowNode, inbound: &FlowEnvelope) -> Result<Value, String> {
    let Some(mapping) = node
        .config
        .get(INPUT_MAPPING_KEY)
        .and_then(|v| v.as_object())
    else {
        return Ok(node.config.clone());
    };
    if mapping.is_empty() {
        return Ok(strip_mapping_keys(node.config.clone()));
    }

    let extras: [(&str, Value); 0] = [];
    let scope = scope_from_envelope(inbound, &extras);

    let mut overlay: Vec<(String, Value)> = Vec::with_capacity(mapping.len());
    for (config_key, expr_value) in mapping {
        let expr = expr_value.as_str().ok_or_else(|| {
            format!(
                "node '{}' input_mapping['{config_key}'] must be a CEL string, got {expr_value}",
                node.id
            )
        })?;
        let result = evaluate(expr, &scope, None)
            .map_err(|e| format!("node '{}' input_mapping['{config_key}']: {e}", node.id))?;
        overlay.push((config_key.clone(), result));
    }

    // Overlay onto a config that no longer carries the mapping declarations,
    // so the adapter sees only real settings. Computed keys win over static.
    let mut config = strip_mapping_keys(node.config.clone());
    if let Value::Object(map) = &mut config {
        for (k, v) in overlay {
            map.insert(k, v);
        }
    }
    Ok(config)
}

/// Evaluates `node.config.output_mapping` against the node's `result` envelope
/// and writes each result into `result.variables`. No-op (no clone, no scope)
/// when the key is absent. Errors carry the node name.
pub fn apply_output_mapping(node: &FlowNode, result: &mut FlowEnvelope) -> Result<(), String> {
    let Some(mapping) = node
        .config
        .get(OUTPUT_MAPPING_KEY)
        .and_then(|v| v.as_object())
    else {
        return Ok(());
    };
    if mapping.is_empty() {
        return Ok(());
    }

    // The scope reads the result envelope; collect evaluations first to avoid
    // holding an immutable borrow of `result` while writing `result.variables`.
    let mut writes: BTreeMap<String, FlowValue> = BTreeMap::new();
    {
        let extras: [(&str, Value); 0] = [];
        let scope = scope_from_envelope(result, &extras);
        for (variable, expr_value) in mapping {
            let expr = expr_value.as_str().ok_or_else(|| {
                format!(
                    "node '{}' output_mapping['{variable}'] must be a CEL string, got {expr_value}",
                    node.id
                )
            })?;
            let value = evaluate(expr, &scope, None)
                .map_err(|e| format!("node '{}' output_mapping['{variable}']: {e}", node.id))?;
            writes.insert(variable.clone(), json_to_flow_value(value));
        }
    }
    for (variable, value) in writes {
        result.variables.insert(variable, value);
    }
    Ok(())
}

/// Removes io-mapping declaration keys from a config object so the adapter
/// never receives them as settings. Leaves non-object configs untouched.
fn strip_mapping_keys(mut config: Value) -> Value {
    if let Value::Object(map) = &mut config {
        map.remove(INPUT_MAPPING_KEY);
        map.remove(OUTPUT_MAPPING_KEY);
    }
    config
}

/// Maps a CEL result (`serde_json::Value`) into a `FlowValue` for storage in
/// `variables`. Mirrors `expr::flow_value_to_json` so a value written by one
/// node reads back the same way in a later expression: `Null` → `Empty`,
/// string → `Text`, everything else → `Json`. Blob variants are never produced
/// by CEL (it only sees blob descriptors), so there is no reverse for them.
fn json_to_flow_value(value: Value) -> FlowValue {
    match value {
        Value::Null => FlowValue::Empty,
        Value::String(s) => FlowValue::Text(s),
        other => FlowValue::Json(other),
    }
}

/// True when the node declares either io-mapping key — lets the executor skip
/// the wrapping work entirely for the common case (most nodes have neither).
pub fn has_io_mapping(node: &FlowNode) -> bool {
    node.config.get(INPUT_MAPPING_KEY).is_some() || node.config.get(OUTPUT_MAPPING_KEY).is_some()
}

/// Round-trips a stored variable back to JSON for assertions/tests. Re-exported
/// projection so callers outside this module do not depend on `expr` directly.
pub fn variable_to_json(value: &FlowValue) -> Value {
    flow_value_to_json(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(config: Value) -> FlowNode {
        FlowNode {
            id: "n1".into(),
            node_type: "test".into(),
            config,
            position: None,
            label: None,
        }
    }

    fn envelope_with_vars(vars: &[(&str, FlowValue)]) -> FlowEnvelope {
        let mut env = FlowEnvelope::empty();
        for (k, v) in vars {
            env.variables.insert((*k).to_string(), v.clone());
        }
        env
    }

    #[test]
    fn no_mapping_returns_config_unchanged() {
        let n = node(json!({"model": "qwen", "temperature": 0.7}));
        let env = FlowEnvelope::empty();
        let config = apply_input_mapping(&n, &env).unwrap();
        assert_eq!(config, json!({"model": "qwen", "temperature": 0.7}));
    }

    #[test]
    fn input_mapping_overlays_computed_config_and_strips_declaration() {
        let n = node(json!({
            "model": "default",
            "input_mapping": { "model": "vars.chosen_model" }
        }));
        let env = envelope_with_vars(&[("chosen_model", FlowValue::Text("qwen3.6".into()))]);
        let config = apply_input_mapping(&n, &env).unwrap();
        assert_eq!(
            config.get("model").and_then(|v| v.as_str()),
            Some("qwen3.6")
        );
        // The declaration must not leak into the adapter config.
        assert!(config.get("input_mapping").is_none());
    }

    #[test]
    fn input_mapping_adds_new_keys_from_payload_and_meta() {
        let mut env = FlowEnvelope::empty();
        env.payload = FlowValue::Json(json!({"limit": 5}));
        env.meta.insert("model".into(), json!("m1"));
        let n = node(json!({
            "input_mapping": {
                "max_results": "payload.limit * 2",
                "model": "meta.model"
            }
        }));
        let config = apply_input_mapping(&n, &env).unwrap();
        assert_eq!(config.get("max_results"), Some(&json!(10)));
        assert_eq!(config.get("model"), Some(&json!("m1")));
    }

    #[test]
    fn input_mapping_error_carries_node_and_expression() {
        let n = node(json!({ "input_mapping": { "x": "vars.nope.deeper" } }));
        let env = FlowEnvelope::empty();
        let err = apply_input_mapping(&n, &env).unwrap_err();
        assert!(err.contains("node 'n1'"), "{err}");
        assert!(err.contains("input_mapping['x']"), "{err}");
        assert!(err.contains("vars.nope.deeper"), "{err}");
    }

    #[test]
    fn output_mapping_writes_variables_from_result() {
        let n = node(json!({
            "output_mapping": {
                "answer": "payload",
                "doubled": "vars.seed + vars.seed"
            }
        }));
        let mut result = FlowEnvelope::with_payload(FlowValue::Text("hi".into()));
        result
            .variables
            .insert("seed".into(), FlowValue::Json(json!(21)));
        apply_output_mapping(&n, &mut result).unwrap();
        assert_eq!(
            result.variables.get("answer"),
            Some(&FlowValue::Text("hi".into()))
        );
        assert_eq!(
            result.variables.get("doubled"),
            Some(&FlowValue::Json(json!(42)))
        );
    }

    #[test]
    fn output_mapping_no_key_is_noop() {
        let n = node(json!({"model": "m"}));
        let mut result = FlowEnvelope::empty();
        apply_output_mapping(&n, &mut result).unwrap();
        assert!(result.variables.is_empty());
    }

    #[test]
    fn output_mapping_error_carries_node_and_variable() {
        let n = node(json!({ "output_mapping": { "bad": "1 +" } }));
        let mut result = FlowEnvelope::empty();
        let err = apply_output_mapping(&n, &mut result).unwrap_err();
        assert!(err.contains("node 'n1'"), "{err}");
        assert!(err.contains("output_mapping['bad']"), "{err}");
    }

    #[test]
    fn json_to_flow_value_mirrors_projection() {
        assert_eq!(json_to_flow_value(json!(null)), FlowValue::Empty);
        assert_eq!(
            json_to_flow_value(json!("text")),
            FlowValue::Text("text".into())
        );
        assert_eq!(
            json_to_flow_value(json!({"a": 1})),
            FlowValue::Json(json!({"a": 1}))
        );
        // Round-trips through the scope projection: a stored Json reads back
        // the same JSON in a later expression.
        let stored = json_to_flow_value(json!([1, 2, 3]));
        assert_eq!(variable_to_json(&stored), json!([1, 2, 3]));
    }

    #[test]
    fn has_io_mapping_detects_either_key() {
        assert!(!has_io_mapping(&node(json!({"model": "m"}))));
        assert!(has_io_mapping(&node(json!({"input_mapping": {}}))));
        assert!(has_io_mapping(&node(json!({"output_mapping": {}}))));
    }
}
