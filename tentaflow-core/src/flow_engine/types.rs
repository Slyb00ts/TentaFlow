// =============================================================================
// Plik: flow_engine/types.rs
// Opis: Typy DAG flow — node, edge, definition. Runtime types (envelope,
//       outcome, trace) żyją w `flow_engine/envelope.rs`. Stage 1d wycięło
//       legacy FlowContext / FlowExecutionResult / FlowStepLog — nowy stack
//       używa `FlowEnvelope` + `FlowExecutionOutcome` + `TraceStep`.
// =============================================================================

use serde::{Deserialize, Serialize};

/// Wezel w grafie flow DAG
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowNode {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: String,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default, deserialize_with = "deserialize_position")]
    pub position: Option<(f64, f64)>,
    #[serde(default)]
    pub label: Option<String>,
}

/// Parsuje pole `position` — akceptuje zarowno format GUI (`{"x":0,"y":0}`)
/// jak i tuple (`[0, 0]`) uzywane wewnetrznie w testach.
fn deserialize_position<'de, D>(deserializer: D) -> Result<Option<(f64, f64)>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::Array(arr)) if arr.len() == 2 => {
            let x = arr[0]
                .as_f64()
                .ok_or_else(|| serde::de::Error::custom("position[0] nie jest liczba"))?;
            let y = arr[1]
                .as_f64()
                .ok_or_else(|| serde::de::Error::custom("position[1] nie jest liczba"))?;
            Ok(Some((x, y)))
        }
        Some(serde_json::Value::Object(map)) => {
            let x = map
                .get("x")
                .and_then(|v| v.as_f64())
                .ok_or_else(|| serde::de::Error::custom("position.x brak lub nie-liczba"))?;
            let y = map
                .get("y")
                .and_then(|v| v.as_f64())
                .ok_or_else(|| serde::de::Error::custom("position.y brak lub nie-liczba"))?;
            Ok(Some((x, y)))
        }
        _ => Err(serde::de::Error::custom(
            "position musi byc {x,y} albo [x,y]",
        )),
    }
}

/// Krawedz (polaczenie) miedzy dwoma wezlami w DAG
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowEdge {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(rename = "from_node", alias = "from", alias = "source")]
    pub from: String,
    #[serde(rename = "to_node", alias = "to", alias = "target")]
    pub to: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub condition: Option<String>,

    /// Port wyjsciowy zrodlowego node'a. Default "full" — stream-aware
    /// adaptery (LLM) eksponuja tez port "stream".
    #[serde(
        default = "default_port_full",
        skip_serializing_if = "is_default_port_full"
    )]
    pub from_port: String,

    /// Port wejsciowy docelowego node'a. Default "in".
    #[serde(
        default = "default_port_in",
        skip_serializing_if = "is_default_port_in"
    )]
    pub to_port: String,
}

fn default_port_full() -> String {
    "full".to_string()
}

fn default_port_in() -> String {
    "in".to_string()
}

fn is_default_port_full(s: &str) -> bool {
    s == "full"
}

fn is_default_port_in(s: &str) -> bool {
    s == "in"
}

/// Pelna definicja flow (parsowana z flow_json w DB)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowDefinition {
    pub nodes: Vec<FlowNode>,
    pub edges: Vec<FlowEdge>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_without_ports_gets_defaults() {
        let json = r#"{"from":"a","to":"b"}"#;
        let edge: FlowEdge = serde_json::from_str(json).unwrap();
        assert_eq!(edge.from_port, "full");
        assert_eq!(edge.to_port, "in");
        assert!(edge.condition.is_none());
    }

    #[test]
    fn edge_with_explicit_ports_deserializes() {
        let json = r#"{"from":"a","to":"b","from_port":"stream","to_port":"audio"}"#;
        let edge: FlowEdge = serde_json::from_str(json).unwrap();
        assert_eq!(edge.from_port, "stream");
        assert_eq!(edge.to_port, "audio");
    }

    #[test]
    fn edge_default_ports_skip_serialize() {
        let edge = FlowEdge {
            id: None,
            from: "a".into(),
            to: "b".into(),
            label: None,
            condition: None,
            from_port: "full".into(),
            to_port: "in".into(),
        };
        let s = serde_json::to_string(&edge).unwrap();
        assert!(!s.contains("from_port"), "got: {s}");
        assert!(!s.contains("to_port"), "got: {s}");
    }
}
