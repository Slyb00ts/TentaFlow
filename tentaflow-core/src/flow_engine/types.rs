// =============================================================================
// Plik: flow_engine/types.rs
// Opis: Typy danych dla DAG flow - wezly, krawedzie, kontekst wykonania
//       i wynik przetwarzania.
// =============================================================================

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Wezel w grafie flow DAG
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowNode {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: String,
    #[serde(default)]
    pub config: serde_json::Value,
    #[serde(default)]
    pub position: Option<(f64, f64)>,
    #[serde(default)]
    pub label: Option<String>,
}

/// Krawedz (polaczenie) miedzy dwoma wezlami w DAG
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowEdge {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(alias = "source", alias = "from_node")]
    pub from: String,
    #[serde(alias = "target", alias = "to_node")]
    pub to: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub condition: Option<String>,

    /// Port wyjsciowy zrodlowego node'a. Default "full" dla backward compat —
    /// stream-aware adaptery (LLM, TTS) eksponuja tez port "stream".
    /// skip_serializing_if chroni stare flow_json — edges bez jawnych portow
    /// round-trippuja byte-identycznie.
    #[serde(default = "default_port_full", skip_serializing_if = "is_default_port_full")]
    pub from_port: String,

    /// Port wejsciowy docelowego node'a. Default "in".
    #[serde(default = "default_port_in", skip_serializing_if = "is_default_port_in")]
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

/// Kontekst wykonania flow - gromadzi dane miedzy wezlami
#[derive(Debug, Clone, Default)]
pub struct FlowContext {
    pub request_id: String,
    pub model: String,
    pub input: String,
    pub variables: HashMap<String, serde_json::Value>,
    pub node_results: HashMap<String, serde_json::Value>,
    pub execution_log: Vec<FlowStepLog>,
    /// Oryginalne messages z ChatCompletionRequest
    pub messages: Vec<serde_json::Value>,
    /// Audio bytes dla STT
    pub audio_input: Option<Vec<u8>>,
    /// Czy request jest streaming
    pub stream: bool,
    /// Pelny oryginalny request (JSON)
    pub original_request: Option<serde_json::Value>,
    /// Typ serwisu (chat, rag, stt, tts, embeddings)
    pub service_type: String,
    /// ID sesji rozmowy (dla conversation_history, speaker_context)
    pub session_id: Option<String>,
    /// ID rozpoznanej osoby (speaker_context)
    pub person_id: Option<String>,
    /// Pewnosc rozpoznania glosu (0.0 - 1.0)
    pub speaker_confidence: f32,
    /// Imie rozpoznanego mowcy
    pub speaker_name: Option<String>,
    /// Kontynuuj flow nawet gdy wezel zwroci blad (domyslnie false)
    pub continue_on_error: bool,
    /// User context — sluzy ACL gateowi w try_dispatch przed uruchomieniem
    /// flow oraz w wezlach LLM/embedding gdy wywoluja routing dla user-a.
    pub user_id: Option<i64>,
    pub user_role: Option<String>,
}

impl FlowContext {
    pub fn new(request_id: String, model: String, input: String) -> Self {
        Self {
            request_id,
            model,
            input,
            ..Default::default()
        }
    }
}

/// Log pojedynczego kroku wykonania flow
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowStepLog {
    pub node_id: String,
    pub node_type: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub output_preview: Option<String>,
}

/// Wynik wykonania calego flow
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlowExecutionResult {
    pub status: String,
    pub output: serde_json::Value,
    pub execution_log: Vec<FlowStepLog>,
    pub total_latency_ms: i64,
    pub total_tokens: i64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
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

    #[test]
    fn edge_non_default_ports_serialize() {
        let edge = FlowEdge {
            id: None,
            from: "a".into(),
            to: "b".into(),
            label: None,
            condition: None,
            from_port: "stream".into(),
            to_port: "in".into(),
        };
        let s = serde_json::to_string(&edge).unwrap();
        assert!(s.contains("\"from_port\":\"stream\""));
        assert!(!s.contains("to_port"));
    }

    #[test]
    fn from_port_no_longer_aliased_to_condition() {
        // Chroni przed regresja buga: alias "from_port" -> condition mapowal
        // z powrotem stream ports na condition. Teraz from_port to real port.
        let json = r#"{"from":"a","to":"b","from_port":"stream"}"#;
        let edge: FlowEdge = serde_json::from_str(json).unwrap();
        assert!(edge.condition.is_none());
        assert_eq!(edge.from_port, "stream");
    }
}
