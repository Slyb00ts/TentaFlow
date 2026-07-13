// =============================================================================
// Plik: flow_engine/node_adapters/condition.rs
// Opis: ConditionNodeAdapter — eval warunku na polu envelope. Plan v4.2 D1:
//       czyta wyłącznie z inputs[0] (single-input hard rule). Bez cross-node
//       lookupu. Decyzja: `config.expression` (CEL bool nad envelope, §3.12)
//       albo legacy field/operator/value. Wynik trafia do
//       envelope.meta["condition_result"]; executor routuje krawędzie po
//       `from_port == "true" | "false"` (skip-semantyka §3.11 A).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashSet;
use tracing::warn;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::expr::{evaluate_bool, ExprScope};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

pub struct ConditionNodeAdapter;

impl ConditionNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ConditionNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl NodeAdapter for ConditionNodeAdapter {
    fn node_type(&self) -> &str {
        "condition"
    }

    fn input_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("in", FlowDataType::Any)]
    }

    fn output_ports(&self) -> Vec<PortSpec> {
        vec![
            PortSpec::new("true", FlowDataType::Any),
            PortSpec::new("false", FlowDataType::Any),
        ]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        _ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        let input = inputs
            .first()
            .ok_or_else(|| anyhow!("condition node requires exactly 1 input edge"))?;

        // Two decision sources, expression preferred (§3.12). A CEL bool over
        // the envelope is the general path; the legacy field/operator/value
        // triple stays for flows that predate CEL. The chosen source writes the
        // bool into `decision.result`, which `active_output_ports` reads back
        // for gating — the standalone bool is not needed past that point.
        let decision = if let Some(expr) = node
            .config
            .get("expression")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
        {
            let extras: [(&str, Value); 0] = [];
            let scope = ExprScope {
                vars: &input.envelope.variables,
                payload: &input.envelope.payload,
                artifacts: &input.envelope.artifacts,
                meta: &input.envelope.meta,
                extras: &extras,
            };
            let result = evaluate_bool(expr, &scope, None)
                .map_err(|e| anyhow!("condition node '{}': {e}", node.id))?;
            serde_json::json!({ "expression": expr, "result": result })
        } else {
            let field = node
                .config
                .get("field")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let operator = node
                .config
                .get("operator")
                .and_then(|v| v.as_str())
                .unwrap_or("equals");
            let expected = node.config.get("value").cloned().unwrap_or(Value::Null);
            let actual = resolve_field_value(field, &input.envelope);
            let result = evaluate_condition(&actual, operator, &expected);
            serde_json::json!({
                "field": field,
                "operator": operator,
                "result": result,
            })
        };

        // Passthrough envelope, ale dopisujemy decision do meta. `condition_result`
        // zostaje dla downstream odczytów (back-compat), ale faktyczne bramkowanie
        // robi teraz `active_output_ports` — executor pomija gałąź nieaktywnego
        // portu zamiast wykonywać obie.
        let mut out = (*input.envelope).clone();
        out.meta.insert("condition_result".into(), decision);
        Ok(out)
    }

    /// Bramkowanie (§3.11 A): aktywuje dokładnie jeden port wyjściowy. Wynik
    /// czytamy z `meta.condition_result.result` zapisanego w `execute` — jedno
    /// źródło prawdy, brak ponownej ewaluacji warunku. Brak wpisu (np. wynik
    /// przepuszczony przez inny adapter) traktujemy jako `false`, więc gałąź
    /// `true` jest bramkowana zachowawczo.
    fn active_output_ports(
        &self,
        _node: &FlowNode,
        result: &FlowEnvelope,
    ) -> Option<HashSet<String>> {
        let decided = result
            .meta
            .get("condition_result")
            .and_then(|v| v.get("result"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let port = if decided { "true" } else { "false" };
        Some(HashSet::from([port.to_string()]))
    }
}

/// Resolver pól z pojedynczego envelope. Brak cross-node lookupu (plan v4.2
/// D1). `field` może być:
/// - `"input"` → payload jako Text (Empty/non-Text → Null)
/// - `"model"` → meta["model"]
/// - `"x"` lub `"x.y.z"` → najpierw artifacts["x"] z opcjonalnym Json path,
///   potem fallback na meta path
pub fn resolve_field_value(field: &str, envelope: &FlowEnvelope) -> Value {
    if field == "input" {
        return flow_value_to_json(&envelope.payload);
    }
    if field == "model" {
        return envelope.meta.get("model").cloned().unwrap_or(Value::Null);
    }

    let (head, tail) = match field.split_once('.') {
        Some((h, t)) => (h, Some(t)),
        None => (field, None),
    };

    if let Some(artifact) = envelope.artifacts.get(head) {
        let as_json = flow_value_to_json(artifact);
        return match tail {
            Some(path) => resolve_json_path(&as_json, path),
            None => as_json,
        };
    }

    if let Some(meta_value) = envelope.meta.get(head) {
        return match tail {
            Some(path) => resolve_json_path(meta_value, path),
            None => meta_value.clone(),
        };
    }

    Value::Null
}

/// Konwersja FlowValue → serde_json::Value używana wyłącznie przez condition.
/// FlowValue ma adjacently-tagged serde (`{"kind": ..., "data": ...}`), co
/// rozsadza Json path adapters; tu wyciągamy "naturalną" reprezentację:
/// Text → String, Json → wewnętrzny Value, Embedding → Array<f64>, blob-y →
/// objekt z metadanymi (mime/size/sha) bez bytów.
fn flow_value_to_json(v: &FlowValue) -> Value {
    match v {
        FlowValue::Empty => Value::Null,
        FlowValue::Text(t) => Value::String(t.clone()),
        FlowValue::Json(j) => j.clone(),
        FlowValue::Embedding(vec) => {
            Value::Array(vec.iter().map(|f| serde_json::json!(*f)).collect())
        }
        FlowValue::Audio {
            blob_ref,
            mime,
            sample_rate,
        } => serde_json::json!({
            "blob_id": blob_ref.id,
            "size_bytes": blob_ref.size_bytes,
            "sha256": blob_ref.sha256,
            "mime": mime,
            "sample_rate": sample_rate,
        }),
        FlowValue::Image {
            blob_ref,
            mime,
            dims,
        } => serde_json::json!({
            "blob_id": blob_ref.id,
            "size_bytes": blob_ref.size_bytes,
            "sha256": blob_ref.sha256,
            "mime": mime,
            "dims": dims,
        }),
        FlowValue::Video {
            blob_ref,
            mime,
            duration_ms,
        } => serde_json::json!({
            "blob_id": blob_ref.id,
            "size_bytes": blob_ref.size_bytes,
            "sha256": blob_ref.sha256,
            "mime": mime,
            "duration_ms": duration_ms,
        }),
        FlowValue::Other {
            blob_ref,
            mime,
            filename,
        } => serde_json::json!({
            "blob_id": blob_ref.id,
            "size_bytes": blob_ref.size_bytes,
            "sha256": blob_ref.sha256,
            "mime": mime,
            "filename": filename,
        }),
    }
}

fn resolve_json_path(value: &Value, path: &str) -> Value {
    let mut current = value;
    for key in path.split('.') {
        match current.get(key) {
            Some(v) => current = v,
            None => return Value::Null,
        }
    }
    current.clone()
}

fn compare_numbers<F: Fn(f64, f64) -> bool>(a: &Value, b: &Value, cmp: F) -> bool {
    let to_num = |v: &Value| v.as_f64().or_else(|| v.as_i64().map(|i| i as f64));
    match (to_num(a), to_num(b)) {
        (Some(x), Some(y)) => cmp(x, y),
        _ => false,
    }
}

pub fn evaluate_condition(actual: &Value, operator: &str, expected: &Value) -> bool {
    match operator {
        "equals" | "eq" | "==" => actual == expected,
        "not_equals" | "neq" | "!=" => actual != expected,
        "contains" => match (actual.as_str(), expected.as_str()) {
            (Some(h), Some(n)) => h.contains(n),
            _ => false,
        },
        "not_contains" => match (actual.as_str(), expected.as_str()) {
            (Some(h), Some(n)) => !h.contains(n),
            _ => true,
        },
        "gt" | ">" => compare_numbers(actual, expected, |a, b| a > b),
        "gte" | ">=" => compare_numbers(actual, expected, |a, b| a >= b),
        "lt" | "<" => compare_numbers(actual, expected, |a, b| a < b),
        "lte" | "<=" => compare_numbers(actual, expected, |a, b| a <= b),
        "exists" => !actual.is_null(),
        "not_exists" => actual.is_null(),
        "is_empty" => {
            actual.is_null()
                || actual.as_str().is_some_and(|s| s.is_empty())
                || actual.as_array().is_some_and(|a| a.is_empty())
        }
        "is_not_empty" => {
            !actual.is_null()
                && !actual.as_str().is_some_and(|s| s.is_empty())
                && !actual.as_array().is_some_and(|a| a.is_empty())
        }
        _ => {
            warn!(operator, "condition: unknown operator");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::envelope::ArtifactProvenance;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use std::sync::Arc;

    fn condition_node(field: &str, op: &str, value: Value) -> FlowNode {
        FlowNode {
            id: "cond-1".into(),
            node_type: "condition".into(),
            config: serde_json::json!({
                "field": field,
                "operator": op,
                "value": value,
            }),
            position: None,
            label: None,
            region: None,
        }
    }

    fn make_input(env: FlowEnvelope) -> NodeInput {
        NodeInput {
            from_node_id: "src".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    #[tokio::test]
    async fn condition_input_field_equals_payload_text() {
        let env = FlowEnvelope::with_payload(FlowValue::Text("hello".into()));
        let inputs = vec![make_input(env)];
        let node = condition_node("input", "equals", serde_json::json!("hello"));
        let out = ConditionNodeAdapter
            .execute(&node, &inputs, &stub_ctx())
            .await
            .unwrap();
        let res = out
            .meta
            .get("condition_result")
            .and_then(|v| v.get("result"))
            .and_then(|v| v.as_bool())
            .unwrap();
        assert!(res);
    }

    #[tokio::test]
    async fn condition_artifact_field_with_json_path() {
        let mut env = FlowEnvelope::empty();
        env.put_artifact(
            "stt",
            FlowValue::Json(serde_json::json!({"language": "pl"})),
            ArtifactProvenance {
                producer_node_id: "stt".into(),
                producer_node_type: "stt".into(),
                timestamp_ms: 0,
            },
        )
        .unwrap();
        let inputs = vec![make_input(env)];
        let node = condition_node("stt.language", "equals", serde_json::json!("pl"));
        let out = ConditionNodeAdapter
            .execute(&node, &inputs, &stub_ctx())
            .await
            .unwrap();
        assert_eq!(
            out.meta
                .get("condition_result")
                .and_then(|v| v.get("result"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[tokio::test]
    async fn condition_passes_through_envelope_payload() {
        let env = FlowEnvelope::with_payload(FlowValue::Text("payload-text".into()));
        let inputs = vec![make_input(env)];
        let node = condition_node("input", "exists", Value::Null);
        let out = ConditionNodeAdapter
            .execute(&node, &inputs, &stub_ctx())
            .await
            .unwrap();
        // Payload identyczny — condition nie modyfikuje danych, tylko meta.
        assert_eq!(out.payload.as_text(), Some("payload-text"));
    }

    #[tokio::test]
    async fn condition_unknown_operator_returns_false() {
        let env = FlowEnvelope::with_payload(FlowValue::Text("x".into()));
        let inputs = vec![make_input(env)];
        let node = condition_node("input", "uknown_op", serde_json::json!("x"));
        let out = ConditionNodeAdapter
            .execute(&node, &inputs, &stub_ctx())
            .await
            .unwrap();
        assert_eq!(
            out.meta
                .get("condition_result")
                .and_then(|v| v.get("result"))
                .and_then(|v| v.as_bool()),
            Some(false)
        );
    }

    #[tokio::test]
    async fn active_output_ports_gates_true_when_condition_true() {
        let env = FlowEnvelope::with_payload(FlowValue::Text("hello".into()));
        let inputs = vec![make_input(env)];
        let node = condition_node("input", "equals", serde_json::json!("hello"));
        let out = ConditionNodeAdapter
            .execute(&node, &inputs, &stub_ctx())
            .await
            .unwrap();
        let active = ConditionNodeAdapter
            .active_output_ports(&node, &out)
            .unwrap();
        assert_eq!(active, HashSet::from(["true".to_string()]));
    }

    #[tokio::test]
    async fn active_output_ports_gates_false_when_condition_false() {
        let env = FlowEnvelope::with_payload(FlowValue::Text("hello".into()));
        let inputs = vec![make_input(env)];
        let node = condition_node("input", "equals", serde_json::json!("other"));
        let out = ConditionNodeAdapter
            .execute(&node, &inputs, &stub_ctx())
            .await
            .unwrap();
        let active = ConditionNodeAdapter
            .active_output_ports(&node, &out)
            .unwrap();
        assert_eq!(active, HashSet::from(["false".to_string()]));
    }

    fn expression_node(expr: &str) -> FlowNode {
        FlowNode {
            id: "cond-expr".into(),
            node_type: "condition".into(),
            config: serde_json::json!({ "expression": expr }),
            position: None,
            label: None,
            region: None,
        }
    }

    #[tokio::test]
    async fn expression_true_activates_true_port() {
        let mut env = FlowEnvelope::with_payload(FlowValue::Text("hi".into()));
        env.variables
            .insert("attempt".into(), FlowValue::Json(serde_json::json!(3)));
        let inputs = vec![make_input(env)];
        let node = expression_node("vars.attempt > 2 && payload == \"hi\"");
        let out = ConditionNodeAdapter
            .execute(&node, &inputs, &stub_ctx())
            .await
            .unwrap();
        assert_eq!(
            out.meta
                .get("condition_result")
                .and_then(|v| v.get("result"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        let active = ConditionNodeAdapter
            .active_output_ports(&node, &out)
            .unwrap();
        assert_eq!(active, HashSet::from(["true".to_string()]));
    }

    #[tokio::test]
    async fn expression_false_activates_false_port() {
        let mut env = FlowEnvelope::empty();
        env.variables
            .insert("attempt".into(), FlowValue::Json(serde_json::json!(1)));
        let inputs = vec![make_input(env)];
        let node = expression_node("vars.attempt > 2");
        let out = ConditionNodeAdapter
            .execute(&node, &inputs, &stub_ctx())
            .await
            .unwrap();
        let active = ConditionNodeAdapter
            .active_output_ports(&node, &out)
            .unwrap();
        assert_eq!(active, HashSet::from(["false".to_string()]));
    }

    #[tokio::test]
    async fn non_bool_expression_is_node_error() {
        let env = FlowEnvelope::with_payload(FlowValue::Text("hi".into()));
        let inputs = vec![make_input(env)];
        let node = expression_node("payload");
        let err = ConditionNodeAdapter
            .execute(&node, &inputs, &stub_ctx())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cond-expr"), "{err}");
        assert!(err.to_string().contains("expected bool"), "{err}");
    }

    #[tokio::test]
    async fn empty_expression_falls_back_to_legacy_field_logic() {
        // Blank expression must not hijack the decision — legacy path runs.
        let env = FlowEnvelope::with_payload(FlowValue::Text("go".into()));
        let inputs = vec![make_input(env)];
        let node = FlowNode {
            id: "c".into(),
            node_type: "condition".into(),
            config: serde_json::json!({
                "expression": "  ",
                "field": "input",
                "operator": "equals",
                "value": "go"
            }),
            position: None,
            label: None,
            region: None,
        };
        let out = ConditionNodeAdapter
            .execute(&node, &inputs, &stub_ctx())
            .await
            .unwrap();
        assert_eq!(
            out.meta
                .get("condition_result")
                .and_then(|v| v.get("result"))
                .and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn condition_advertises_true_false_ports() {
        let a = ConditionNodeAdapter;
        let in_names: Vec<String> = a.input_ports().iter().map(|p| p.name.clone()).collect();
        let out_names: Vec<String> = a.output_ports().iter().map(|p| p.name.clone()).collect();
        assert_eq!(in_names, vec!["in"]);
        assert_eq!(out_names, vec!["true", "false"]);
    }

    #[test]
    fn evaluate_condition_numeric_comparisons() {
        assert!(evaluate_condition(
            &serde_json::json!(5),
            "gt",
            &serde_json::json!(3)
        ));
        assert!(!evaluate_condition(
            &serde_json::json!(2),
            "gt",
            &serde_json::json!(3)
        ));
        assert!(evaluate_condition(
            &serde_json::json!(3.0),
            "gte",
            &serde_json::json!(3)
        ));
    }
}
