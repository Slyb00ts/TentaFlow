// =============================================================================
// Plik: flow_engine/types.rs
// Opis: Typy DAG flow — node, edge, definition. Runtime types (envelope,
//       outcome, trace) żyją w `flow_engine/envelope.rs`. Stage 1d wycięło
//       legacy FlowContext / FlowExecutionResult / FlowStepLog — nowy stack
//       używa `FlowEnvelope` + `FlowExecutionOutcome` + `TraceStep`.
// =============================================================================

use serde::{Deserialize, Serialize};

/// Typ danych płynących edge'em flow. Etap 2 używa go jako deklaracji (nie
/// konwertera) — walidacja R8 sprawdza zgodność producenta, konsumenta i
/// edge'a. `Any` jest przejściowym fallback'em dla legacy flow_json + portów
/// które nie wiedzą jaki typ przepuszczają (passthrough adaptery: trigger,
/// output, condition, conversation_history, session_context, speaker_context).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FlowDataType {
    #[default]
    Any,
    Text,
    Audio,
    Image,
    Video,
    Embedding,
    Json,
    /// Generyczny plik / dokument (PDF, DOCX, XLSX, ZIP itp.) — wszystko co
    /// nie jest natywnym media type (audio/image/video) ani structured data
    /// (text/json/embedding). Adaptery konsumujace `Other` musza patrzec na
    /// `FlowValue::Other.mime` zeby zdecydowac co z tym zrobic.
    Other,
}

impl FlowDataType {
    /// Stable lowercase tag uzywany w wire (CBOR) i GUI rendering. Spojny z
    /// `serde(rename_all = "snake_case")` zeby JSON i string surface daly ten
    /// sam tag.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            FlowDataType::Any => "any",
            FlowDataType::Text => "text",
            FlowDataType::Audio => "audio",
            FlowDataType::Image => "image",
            FlowDataType::Video => "video",
            FlowDataType::Embedding => "embedding",
            FlowDataType::Json => "json",
            FlowDataType::Other => "other",
        }
    }

    /// `Any` na której kolwiek stronie = wildcard (compatible z każdym
    /// konkretnym typem). Inaczej wymaga dokładnego match'a.
    pub fn compatible_with(self, other: FlowDataType) -> bool {
        matches!(self, FlowDataType::Any) || matches!(other, FlowDataType::Any) || self == other
    }

    /// Mapowanie z `FlowValue` na typ. `Empty` → `None` (brak payloadu ≠
    /// wildcard) — caller decyduje czy to legalne (np. trigger może
    /// wystartować flow bez payloadu).
    pub fn from_value(v: &crate::flow_engine::envelope::FlowValue) -> Option<Self> {
        use crate::flow_engine::envelope::FlowValue;
        match v {
            FlowValue::Empty => None,
            FlowValue::Text(_) => Some(FlowDataType::Text),
            FlowValue::Json(_) => Some(FlowDataType::Json),
            FlowValue::Audio { .. } => Some(FlowDataType::Audio),
            FlowValue::Image { .. } => Some(FlowDataType::Image),
            FlowValue::Video { .. } => Some(FlowDataType::Video),
            FlowValue::Embedding(_) => Some(FlowDataType::Embedding),
            FlowValue::Other { .. } => Some(FlowDataType::Other),
        }
    }
}

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
    /// Inline loop region this node belongs to (`None` = outside any region).
    /// Nodes sharing a region id form one loop body run inline over a single
    /// envelope by the executor; the back edge (`FlowEdge.kind == "loop_back"`)
    /// closes the cycle without the outer DAG becoming cyclic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
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

    /// Deklarowany typ danych płynących edge'em (Etap 2). Default `Any` żeby
    /// legacy flow_json round-trippowało byte-identycznie. Walidacja R8
    /// sprawdza zgodność z `producent.output_port_type` i
    /// `konsument.input_port_type`.
    #[serde(default, skip_serializing_if = "is_default_data_type")]
    pub data_type: FlowDataType,

    /// Structural edge kind. `Some("loop_back")` marks the single back edge of
    /// an inline loop region: it is excluded from the topological in-degree (so
    /// the outer DAG stays acyclic) and from R4's incoming-edge count. `None` =
    /// an ordinary forward edge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// `FlowEdge.kind` value marking the inline loop-region back edge.
pub const EDGE_KIND_LOOP_BACK: &str = "loop_back";

impl FlowEdge {
    /// True when this edge is the back edge of an inline loop region.
    pub fn is_loop_back(&self) -> bool {
        self.kind.as_deref() == Some(EDGE_KIND_LOOP_BACK)
    }
}

fn is_default_data_type(t: &FlowDataType) -> bool {
    matches!(t, FlowDataType::Any)
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

/// Deklaracja zmiennej flow (§3.12). Flow Builder pokazuje je w panelu flow;
/// R10 wymaga, by kazdy `output_mapping` zapisywal wylacznie do zadeklarowanej
/// zmiennej. Pole opcjonalne w flow_json — brak sekcji = zero dozwolonych
/// zmiennych (output_mapping do czegokolwiek = blad walidacji).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableDeclaration {
    pub name: String,
    #[serde(rename = "type", default)]
    pub var_type: FlowDataType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Pelna definicja flow (parsowana z flow_json w DB)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowDefinition {
    pub nodes: Vec<FlowNode>,
    pub edges: Vec<FlowEdge>,
    /// Zadeklarowane zmienne flow (§3.12 / R10). Default puste — legacy
    /// flow_json bez tej sekcji round-trippuje byte-identycznie i nie pozwala
    /// na zaden output_mapping (zachowawczo).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<VariableDeclaration>,
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
            data_type: FlowDataType::Any,
            kind: None,
        };
        let s = serde_json::to_string(&edge).unwrap();
        assert!(!s.contains("from_port"), "got: {s}");
        assert!(!s.contains("to_port"), "got: {s}");
    }

    /// Sekcja `variables` w ksztalcie emitowanym przez edytor zmiennych
    /// (variables.js) parsuje sie do FlowDefinition: type jako FlowDataType
    /// snake_case, default jako dowolny JSON, description opcjonalne. To kontrakt
    /// UI <-> backend (R10 czyta te deklaracje).
    #[test]
    fn variables_section_from_ui_parses() {
        let json = r#"{
            "nodes":[{"id":"t1","type":"trigger","config":{}}],
            "edges":[],
            "variables":[
                {"name":"chosen_model","type":"text","default":"qwen3.6:27b","description":"router pick"},
                {"name":"attempts","type":"json","default":0},
                {"name":"flag","type":"any"}
            ]
        }"#;
        let def: FlowDefinition = serde_json::from_str(json).expect("parses");
        assert_eq!(def.variables.len(), 3);
        assert_eq!(def.variables[0].name, "chosen_model");
        assert_eq!(def.variables[0].var_type, FlowDataType::Text);
        assert_eq!(
            def.variables[0].default.as_ref().unwrap().as_str(),
            Some("qwen3.6:27b")
        );
        assert_eq!(def.variables[1].var_type, FlowDataType::Json);
        assert_eq!(def.variables[1].default.as_ref().unwrap().as_i64(), Some(0));
        // Brak default/description => None (skip_serializing_if w round-trip).
        assert!(def.variables[2].default.is_none());
        assert!(def.variables[2].description.is_none());
        assert_eq!(def.variables[2].var_type, FlowDataType::Any);
    }

    /// Legacy flow_json bez sekcji `variables` round-trippuje byte-identycznie
    /// (pusta lista nie jest serializowana — zachowawczy default).
    #[test]
    fn empty_variables_omitted_in_serialization() {
        let def = FlowDefinition {
            nodes: vec![],
            edges: vec![],
            variables: vec![],
        };
        let s = serde_json::to_string(&def).unwrap();
        assert!(!s.contains("variables"), "got: {s}");
    }
}
