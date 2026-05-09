// =============================================================================
// Plik: flow_engine/synthetic.rs
// Opis: Buildery synthetic ad-hoc flow definitions dla Universal Flow Gateway
//       (stage 3d). Używane przez FlowDispatcher gdy resolver::resolve_flow
//       zwraca None — admin nie skonfigurował flow dla danego modelu, runtime
//       buduje minimalny trigger→capability→output, model wstawiany z requestu.
//
//       Synthetic flow nie idzie do DB, żyje wyłącznie w runtime, kompilowany
//       z `ValidationSource::Synthetic` (R-SAFETY skip).
// =============================================================================

use serde_json::json;

use crate::flow_engine::types::{FlowDefinition, FlowEdge, FlowNode};

/// Synthetic chat flow: trigger → llm(model) → pii_filter → output.
/// `pii_filter` wstawiony domyślnie żeby utrzymać security parity po
/// demolicji wire-layer middleware (Krok 6) — bez node'a synthetic flow
/// wypuściłby raw LLM output bez czyszczenia PII.
pub fn synthetic_chat(model: &str) -> FlowDefinition {
    FlowDefinition {
        nodes: vec![
            trigger_node(),
            capability_node("llm", "l1", model),
            pii_filter_node(),
            output_node(),
        ],
        edges: vec![
            // Trigger emituje typed payload. Chat = Text, więc trigger.text → llm.in.
            edge("t1", "l1", "text", "in"),
            edge("l1", "p1", "full", "in"),
            // Output ma 6 typed input portow (text/audio/image/video/embedding
            // /other) — pii_filter zwraca Text wiec idziemy do `text`.
            edge("p1", "o1", "full", "text"),
        ],
    }
}

/// Synthetic chat-stream flow: trigger → llm(model) → pii_filter →
/// output(stream). LLM emituje przez `stream` port, pii_filter (jako
/// `StreamingNodeAdapter`) przepuszcza i wycina PII per zdanie, sink ma
/// `mode=stream`.
pub fn synthetic_chat_stream(model: &str) -> FlowDefinition {
    let mut output = output_node();
    output.config = json!({ "mode": "stream" });
    FlowDefinition {
        nodes: vec![
            trigger_node(),
            capability_node("llm", "l1", model),
            pii_filter_node(),
            output,
        ],
        edges: vec![
            edge("t1", "l1", "text", "in"),
            edge("l1", "p1", "stream", "in"),
            edge("p1", "o1", "stream", "text"),
        ],
    }
}

/// Synthetic TTS flow: trigger → tts(model) → output.
pub fn synthetic_tts(model: &str) -> FlowDefinition {
    FlowDefinition {
        nodes: vec![
            trigger_node(),
            capability_node("tts", "t2", model),
            output_node(),
        ],
        edges: vec![
            edge("t1", "t2", "text", "in"),
            // TTS produkuje Audio → output.audio.
            edge("t2", "o1", "full", "audio"),
        ],
    }
}

/// Synthetic STT flow: trigger → stt(model) → output.
pub fn synthetic_stt(model: &str) -> FlowDefinition {
    FlowDefinition {
        nodes: vec![
            trigger_node(),
            capability_node("stt", "s1", model),
            output_node(),
        ],
        edges: vec![
            // STT konsumuje audio, wiec trigger.audio → stt.in.
            edge("t1", "s1", "audio", "in"),
            // STT zwraca tekst transkrypcji → output.text.
            edge("s1", "o1", "full", "text"),
        ],
    }
}

/// Synthetic embeddings flow: trigger → embeddings(model) → output.
pub fn synthetic_embeddings(model: &str) -> FlowDefinition {
    FlowDefinition {
        nodes: vec![
            trigger_node(),
            capability_node("embeddings", "e1", model),
            output_node(),
        ],
        edges: vec![
            edge("t1", "e1", "text", "in"),
            // Embeddings produkuje Embedding → output.embedding.
            edge("e1", "o1", "full", "embedding"),
        ],
    }
}

fn trigger_node() -> FlowNode {
    FlowNode {
        id: "t1".to_string(),
        node_type: "trigger".to_string(),
        config: serde_json::Value::Null,
        position: None,
        label: None,
    }
}

fn capability_node(node_type: &str, id: &str, model: &str) -> FlowNode {
    FlowNode {
        id: id.to_string(),
        node_type: node_type.to_string(),
        config: json!({ "model": model }),
        position: None,
        label: None,
    }
}

fn pii_filter_node() -> FlowNode {
    FlowNode {
        id: "p1".to_string(),
        node_type: "pii_filter".to_string(),
        config: serde_json::Value::Null,
        position: None,
        label: None,
    }
}

fn output_node() -> FlowNode {
    FlowNode {
        id: "o1".to_string(),
        node_type: "output".to_string(),
        config: serde_json::Value::Null,
        position: None,
        label: None,
    }
}

fn edge(from: &str, to: &str, from_port: &str, to_port: &str) -> FlowEdge {
    FlowEdge {
        id: None,
        from: from.to_string(),
        to: to.to_string(),
        label: None,
        condition: None,
        from_port: from_port.to_string(),
        to_port: to_port.to_string(),
        data_type: crate::flow_engine::types::FlowDataType::Any,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::cache::CompiledFlow;
    use crate::flow_engine::node_adapter::AdapterRegistry;
    use crate::flow_engine::node_adapters::{
        LlmNodeAdapter, OutputNodeAdapter, PiiFilterNodeAdapter, TriggerNodeAdapter,
    };
    use crate::flow_engine::validation::ValidationSource;
    use std::sync::Arc;

    fn min_registry() -> AdapterRegistry {
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        r.register_streaming(Arc::new(PiiFilterNodeAdapter::new()));
        r.register_llm(Arc::new(LlmNodeAdapter::new()));
        r
    }

    #[test]
    fn synthetic_chat_compiles_with_synthetic_source() {
        let def = synthetic_chat("qwen3.5-0.8b");
        let compiled = CompiledFlow::compile(0, def, &min_registry(), ValidationSource::Synthetic);
        assert!(compiled.is_ok(), "synthetic chat: {:?}", compiled.err());
    }

    #[test]
    fn synthetic_chat_stream_marks_streaming() {
        let def = synthetic_chat_stream("qwen3.5-0.8b");
        let compiled = CompiledFlow::compile(0, def, &min_registry(), ValidationSource::Synthetic)
            .expect("compile");
        assert!(compiled.is_streaming, "stream-port edge musi włączyć is_streaming");
    }

    #[test]
    fn synthetic_chat_carries_model_in_llm_config() {
        let def = synthetic_chat("qwen3.5-0.8b");
        let llm = def.nodes.iter().find(|n| n.node_type == "llm").unwrap();
        assert_eq!(
            llm.config.get("model").and_then(|v| v.as_str()),
            Some("qwen3.5-0.8b")
        );
    }

    #[test]
    fn synthetic_tts_carries_model() {
        let def = synthetic_tts("xtts-v2");
        let tts = def.nodes.iter().find(|n| n.node_type == "tts").unwrap();
        assert_eq!(
            tts.config.get("model").and_then(|v| v.as_str()),
            Some("xtts-v2")
        );
        assert_eq!(def.edges.len(), 2);
        // Pierwsza krawedz: trigger.text → tts.in. Druga: tts.full → output.in.
        assert_eq!(def.edges[0].from_port, "text");
        assert_eq!(def.edges[1].from_port, "full");
    }

    #[test]
    fn synthetic_stt_carries_model() {
        let def = synthetic_stt("whisper-large-v3");
        let stt = def.nodes.iter().find(|n| n.node_type == "stt").unwrap();
        assert_eq!(
            stt.config.get("model").and_then(|v| v.as_str()),
            Some("whisper-large-v3")
        );
    }

    #[test]
    fn synthetic_embeddings_carries_model() {
        let def = synthetic_embeddings("nomic-embed-text-v1.5");
        let emb = def
            .nodes
            .iter()
            .find(|n| n.node_type == "embeddings")
            .unwrap();
        assert_eq!(
            emb.config.get("model").and_then(|v| v.as_str()),
            Some("nomic-embed-text-v1.5")
        );
    }
}
