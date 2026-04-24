// =============================================================================
// Plik: flow_engine/adapters/condition.rs
// Opis: Adapter wezla condition/switch — ewaluuje warunek na polu z
//       FlowContext (z obsluga operatorow equals/contains/gt/lt/exists/...).
//       Wynik jest uzywany przez executor do blokowania krawedzi branch.
// =============================================================================

use anyhow::Result;
use serde_json::Value;
use tracing::warn;

use crate::flow_engine::adapters::NodeAdapter;
use crate::flow_engine::types::{FlowContext, FlowNode};

/// Rozwiazuje wartosc pola `field` z kontekstu flow. Obsluguje specjalne
/// nazwy (`input`, `model`), zmienne, jawne `node_id.field` i auto-lookup
/// w outputach wezlow (schodzac wstecz po execution_log).
pub fn resolve_field_value(field: &str, ctx: &FlowContext) -> Value {
    if field == "input" {
        return Value::String(ctx.input.clone());
    }
    if field == "model" {
        return Value::String(ctx.model.clone());
    }

    if let Some(val) = ctx.variables.get(field) {
        return val.clone();
    }

    if let Some((prefix, rest)) = field.split_once('.') {
        if let Some(result) = ctx.node_results.get(prefix) {
            return resolve_json_path(result, rest);
        }
    }

    for step in ctx.execution_log.iter().rev() {
        if let Some(result) = ctx.node_results.get(&step.node_id) {
            let resolved = resolve_json_path(result, field);
            if !resolved.is_null() {
                return resolved;
            }
        }
    }

    Value::Null
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

/// Porownuje dwie liczby (f64) uzywajac podanego predykatu. Zwraca false
/// gdy ktoras z wartosci nie jest liczba.
pub fn compare_numbers<F: Fn(f64, f64) -> bool>(a: &Value, b: &Value, cmp: F) -> bool {
    let a_num = a.as_f64().or_else(|| a.as_i64().map(|i| i as f64));
    let b_num = b.as_f64().or_else(|| b.as_i64().map(|i| i as f64));
    match (a_num, b_num) {
        (Some(x), Some(y)) => cmp(x, y),
        _ => false,
    }
}

/// Ewaluuje warunek `operator` pomiedzy `actual` a `expected`. Rozumie
/// operatory: equals/eq/==, not_equals/neq/!=, contains, not_contains,
/// gt/gte/lt/lte, exists/not_exists, is_empty/is_not_empty.
pub fn evaluate_condition(actual: &Value, operator: &str, expected: &Value) -> bool {
    match operator {
        "equals" | "eq" | "==" => actual == expected,
        "not_equals" | "neq" | "!=" => actual != expected,
        "contains" => {
            if let (Some(haystack), Some(needle)) = (actual.as_str(), expected.as_str()) {
                haystack.contains(needle)
            } else {
                false
            }
        }
        "not_contains" => {
            if let (Some(haystack), Some(needle)) = (actual.as_str(), expected.as_str()) {
                !haystack.contains(needle)
            } else {
                true
            }
        }
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
            warn!(operator = operator, "Nieznany operator warunku");
            false
        }
    }
}

/// Buduje output JSON dla wezla condition: field/operator/expected/result.
pub fn build_condition_output(node: &FlowNode, ctx: &FlowContext) -> Value {
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

    let actual = resolve_field_value(field, ctx);
    let result = evaluate_condition(&actual, operator, &expected);

    serde_json::json!({
        "type": "condition_result",
        "field": field,
        "operator": operator,
        "result": result,
    })
}

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

impl NodeAdapter for ConditionNodeAdapter {
    async fn execute(&self, node_config: &Value, ctx: &mut FlowContext) -> Result<Value> {
        let field = node_config
            .get("field")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let operator = node_config
            .get("operator")
            .and_then(|v| v.as_str())
            .unwrap_or("equals");
        let expected = node_config.get("value").cloned().unwrap_or(Value::Null);

        let actual = resolve_field_value(field, ctx);
        let result = evaluate_condition(&actual, operator, &expected);

        Ok(serde_json::json!({
            "type": "condition_result",
            "field": field,
            "operator": operator,
            "result": result,
        }))
    }

    fn node_type(&self) -> &'static str {
        "condition"
    }
}
