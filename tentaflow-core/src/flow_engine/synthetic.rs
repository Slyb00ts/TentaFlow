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

/// Synthetic chat flow: trigger → llm(model) → output(blocking).
/// Streaming tryb wybierany jest na poziomie executora (LLM adapter umie
/// oba), więc synthetic ma jeden kształt który chat-blocking i chat-stream
/// path mogą reużyć.
pub fn synthetic_chat(model: &str) -> FlowDefinition {
    FlowDefinition {
        nodes: vec![
            trigger_node(),
            capability_node("llm", "l1", model),
            output_node(),
        ],
        edges: vec![
            edge("t1", "l1", "full", "in"),
            edge("l1", "o1", "full", "in"),
        ],
    }
}

/// Synthetic chat-stream flow: trigger → llm(model) → output(stream).
/// LLM emituje przez port `stream`; output ma `mode=stream`.
pub fn synthetic_chat_stream(model: &str) -> FlowDefinition {
    let mut output = output_node();
    output.config = json!({ "mode": "stream" });
    FlowDefinition {
        nodes: vec![
            trigger_node(),
            capability_node("llm", "l1", model),
            output,
        ],
        edges: vec![
            edge("t1", "l1", "full", "in"),
            edge("l1", "o1", "stream", "in"),
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
            edge("t1", "t2", "full", "in"),
            edge("t2", "o1", "full", "in"),
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
            edge("t1", "s1", "full", "in"),
            edge("s1", "o1", "full", "in"),
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
            edge("t1", "e1", "full", "in"),
            edge("e1", "o1", "full", "in"),
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
    use crate::flow_engine::node_adapters::{LlmNodeAdapter, OutputNodeAdapter, TriggerNodeAdapter};
    use crate::flow_engine::validation::ValidationSource;
    use std::sync::Arc;

    fn min_registry() -> AdapterRegistry {
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
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
}
