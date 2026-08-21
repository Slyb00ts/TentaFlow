// =============================================================================
// Plik: flow_engine/node_adapters/output.rs
// Opis: OutputNodeAdapter — terminal sink flow. Ma 6 typed input portów (text
//       / audio / image / video / embedding / other) — kazdy branch flow moze
//       zwrocic inny typ danych w jednej odpowiedzi (np. tekst + audio
//       razem). Zwolniony z R4 (1-input-edge) zeby N branchy moglo wpadac
//       jednoczesnie. Adapter w tym kroku przepuszcza envelope z primary
//       inputu (text→audio→image→video→embedding→other priorytet); pelne
//       multimodal merge (multi-payload envelope) wraca w nastepnym kroku.
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::types::{FlowDataType, FlowNode};

pub struct OutputNodeAdapter;

impl OutputNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for OutputNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

// 6 typed input portów per modality + 1 output port `full` z typem `Any`
// (output moze zwrocic dowolna kombinacje typow w envelope.payload +
// envelope.artifacts; konsument output'u to caller flow_engine, nie inny
// node, wiec out_port_type nie jest egzekwowany przez R8).

/// Priorytet portow przy wyborze primary envelope gdy do output trafia kilka
/// branchy. Modyfikacje listy zmieniaja kolejnosc fallback'a.
const PORT_PRIORITY: &[&str] = &["text", "audio", "image", "video", "embedding", "other"];

/// Meta key selecting the terminal shape of the answer. It lives in `meta`, not
/// in the node config, because ONE shell serves two callers: the RAG addon asks
/// blocking and needs `{answer, citations}` in the payload, while the project
/// chat streams the same graph and needs the payload untouched. A shell pinned
/// to one shape in its config could only ever serve one of them.
pub const OUTPUT_MODE_META: &str = "output_mode";

/// `meta[OUTPUT_MODE_META]` value that serializes `{answer, citations}` from
/// `meta["rag_citations"]` into the Text payload.
pub const OUTPUT_MODE_CITATIONS: &str = "citations";

/// `meta[OUTPUT_MODE_META]` value that leaves the payload as produced. Any
/// value other than [`OUTPUT_MODE_CITATIONS`] — and an absent key — resolves
/// here, so a flow that never heard of the RAG shell is untouched.
pub const OUTPUT_MODE_STREAM: &str = "stream";

#[async_trait]
impl NodeAdapter for OutputNodeAdapter {
    fn node_type(&self) -> &str {
        "output"
    }

    fn input_ports(&self) -> Vec<PortSpec> {
        vec![
            PortSpec::new("text", FlowDataType::Text),
            PortSpec::new("audio", FlowDataType::Audio),
            PortSpec::new("image", FlowDataType::Image),
            PortSpec::new("video", FlowDataType::Video),
            PortSpec::new("embedding", FlowDataType::Embedding),
            PortSpec::new("other", FlowDataType::Other),
        ]
    }

    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("full", FlowDataType::Any)]
    }

    async fn execute(
        &self,
        _node: &FlowNode,
        inputs: &[NodeInput],
        _ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        if inputs.is_empty() {
            return Err(anyhow!("output node requires >=1 input edge"));
        }
        // Wybor primary envelope: pierwszy input ktorego `to_port` (= nazwa
        // wlasnego typed input portu, niesiona w `NodeInput.from_port` ale to
        // OD producenta — uzywamy `find_by_port`/inputs.iter() z preferencja
        // PORT_PRIORITY). NodeInput nie niesie `to_port` (jest implicit na
        // konsumencie), ale my wiemy ze edge.to_port to nasz input port. W
        // executor.rs build_inputs przekazuje wszystkie krawedzie incoming —
        // dopasowanie po typie payloadu jest najprostszym sygnałem.
        let mut primary: Option<FlowEnvelope> = None;
        'outer: for prio in PORT_PRIORITY {
            let prio_type = self.input_port_type(prio);
            for inp in inputs {
                let payload_kind =
                    crate::flow_engine::types::FlowDataType::from_value(&inp.envelope.payload);
                if payload_kind == Some(prio_type) {
                    primary = Some((*inp.envelope).clone());
                    break 'outer;
                }
            }
        }
        // Zaden input nie pasuje do typed portow — zwroc pierwszy (Any
        // fallback, np. Empty / Json).
        let mut out = primary.unwrap_or_else(|| (*inputs[0].envelope).clone());

        // Citation mode: the envelope carries the real retrieval hits in
        // meta["rag_citations"], so the answer is serialized as `{answer,
        // citations}` JSON into the Text payload — the caller gets the LLM text
        // TOGETHER with exactly what retrieval returned (one source of truth,
        // no second SELECT). The mode comes from meta; `rag_finalize` seeds the
        // RAG default and an entry point that streams stamps `stream` instead,
        // so a generic flow (no mode in meta) stays a passthrough.
        let emit = out
            .meta
            .get(OUTPUT_MODE_META)
            .and_then(|v| v.as_str())
            .is_some_and(|m| m == OUTPUT_MODE_CITATIONS);
        if emit {
            if let (Some(answer), Some(citations)) = (
                out.payload.as_text().map(str::to_string),
                out.meta.get("rag_citations").cloned(),
            ) {
                let wrapped = serde_json::json!({
                    "answer": answer,
                    "citations": citations,
                });
                out.payload = FlowValue::Text(serde_json::to_string(&wrapped).unwrap_or(answer));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use std::sync::Arc;

    fn output_node() -> FlowNode {
        FlowNode {
            id: "out-1".into(),
            node_type: "output".into(),
            config: serde_json::Value::Null,
            position: None,
            label: None,
            region: None,
        }
    }

    #[tokio::test]
    async fn output_passes_through_payload_and_meta() {
        let adapter = OutputNodeAdapter::new();
        let mut env = FlowEnvelope::with_payload(FlowValue::Text("hello".into()));
        env.meta
            .insert("request_id".into(), serde_json::json!("r-1"));

        let inputs = vec![NodeInput {
            from_node_id: "llm-1".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }];

        let result = adapter
            .execute(&output_node(), &inputs, &stub_ctx())
            .await
            .unwrap();
        assert_eq!(result.payload.as_text(), Some("hello"));
        assert_eq!(
            result.meta.get("request_id").and_then(|v| v.as_str()),
            Some("r-1")
        );
    }

    #[tokio::test]
    async fn output_picks_text_branch_when_text_and_audio_both_arrive() {
        let adapter = OutputNodeAdapter::new();
        let env_audio = FlowEnvelope::with_payload(FlowValue::Audio {
            blob_ref: crate::flow_engine::blob_store::BlobRef {
                id: "b1".into(),
                size_bytes: 1,
                mime: "audio/wav".into(),
                sha256: "s".into(),
            },
            mime: "audio/wav".into(),
            sample_rate: None,
        });
        let env_text = FlowEnvelope::with_payload(FlowValue::Text("priority-wins".into()));
        let inputs = vec![
            NodeInput {
                from_node_id: "tts".into(),
                from_port: "full".into(),
                envelope: Arc::new(env_audio),
            },
            NodeInput {
                from_node_id: "llm".into(),
                from_port: "stream".into(),
                envelope: Arc::new(env_text),
            },
        ];
        let r = adapter
            .execute(&output_node(), &inputs, &stub_ctx())
            .await
            .unwrap();
        assert_eq!(r.payload.as_text(), Some("priority-wins"));
    }

    /// One envelope, one node config (`mode: "stream"`, the shell's saved
    /// shape) — the two RAG modes are told apart ONLY by `meta`.
    fn rag_answer_envelope(mode: Option<&str>) -> FlowEnvelope {
        let mut env = FlowEnvelope::with_payload(FlowValue::Text("LLM answer".into()));
        env.meta.insert(
            "rag_citations".into(),
            serde_json::json!([{"doc_id": "d1", "chunk_index": 3, "text": "t", "score": 0.2}]),
        );
        if let Some(m) = mode {
            env.meta
                .insert(OUTPUT_MODE_META.into(), serde_json::json!(m));
        }
        env
    }

    /// The node config of the shared RAG shell: `mode: "stream"` is the
    /// streaming end-shape R7 demands, NOT a mode pin for this adapter.
    fn shell_output_node() -> FlowNode {
        FlowNode {
            id: "out".into(),
            node_type: "output".into(),
            config: serde_json::json!({ "mode": "stream" }),
            position: None,
            label: None,
            region: None,
        }
    }

    #[tokio::test]
    async fn output_emits_answer_and_citations_when_meta_selects_citations() {
        // Blocking caller (RAG addon `ask`): meta picks the citation block, so
        // the payload carries the LLM text TOGETHER with the real hits.
        let adapter = OutputNodeAdapter::new();
        let inputs = vec![NodeInput {
            from_node_id: "llm".into(),
            from_port: "full".into(),
            envelope: Arc::new(rag_answer_envelope(Some(OUTPUT_MODE_CITATIONS))),
        }];
        let r = adapter
            .execute(&shell_output_node(), &inputs, &stub_ctx())
            .await
            .unwrap();
        let text = r.payload.as_text().expect("payload Text");
        let parsed: serde_json::Value = serde_json::from_str(text).expect("content is JSON");
        assert_eq!(parsed["answer"].as_str(), Some("LLM answer"));
        assert_eq!(parsed["citations"].as_array().map(|a| a.len()), Some(1));
        assert_eq!(parsed["citations"][0]["doc_id"].as_str(), Some("d1"));
        assert_eq!(parsed["citations"][0]["chunk_index"].as_i64(), Some(3));
    }

    #[tokio::test]
    async fn output_passes_payload_through_when_meta_selects_stream() {
        // Same node, same citations in meta — the streaming caller (project
        // chat) gets the raw answer, never the JSON envelope.
        let adapter = OutputNodeAdapter::new();
        let inputs = vec![NodeInput {
            from_node_id: "llm".into(),
            from_port: "full".into(),
            envelope: Arc::new(rag_answer_envelope(Some(OUTPUT_MODE_STREAM))),
        }];
        let r = adapter
            .execute(&shell_output_node(), &inputs, &stub_ctx())
            .await
            .unwrap();
        assert_eq!(r.payload.as_text(), Some("LLM answer"));
    }

    #[tokio::test]
    async fn output_passthrough_when_meta_carries_no_mode() {
        // A generic flow whose envelope happens to carry citations (e.g. a
        // `project_knowledge` node) must not be rewritten into RAG JSON.
        let adapter = OutputNodeAdapter::new();
        let inputs = vec![NodeInput {
            from_node_id: "llm".into(),
            from_port: "full".into(),
            envelope: Arc::new(rag_answer_envelope(None)),
        }];
        let r = adapter
            .execute(&output_node(), &inputs, &stub_ctx())
            .await
            .unwrap();
        assert_eq!(r.payload.as_text(), Some("LLM answer"));
    }

    #[tokio::test]
    async fn output_errors_when_no_inputs() {
        let adapter = OutputNodeAdapter::new();
        let err = adapter
            .execute(&output_node(), &[], &stub_ctx())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("requires >=1 input edge"));
    }

    #[test]
    fn output_advertises_six_typed_input_ports_and_full_output() {
        let a = OutputNodeAdapter::new();
        let in_names: Vec<String> = a.input_ports().iter().map(|p| p.name.clone()).collect();
        let out_names: Vec<String> = a.output_ports().iter().map(|p| p.name.clone()).collect();
        assert_eq!(
            in_names,
            vec!["text", "audio", "image", "video", "embedding", "other"]
        );
        assert_eq!(out_names, vec!["full"]);
        assert_eq!(a.node_type(), "output");
        assert_eq!(a.input_port_type("text"), FlowDataType::Text);
        assert_eq!(a.input_port_type("audio"), FlowDataType::Audio);
        assert_eq!(a.input_port_type("image"), FlowDataType::Image);
        assert_eq!(a.input_port_type("video"), FlowDataType::Video);
        assert_eq!(a.input_port_type("embedding"), FlowDataType::Embedding);
        assert_eq!(a.input_port_type("other"), FlowDataType::Other);
        assert_eq!(a.output_port_type("full"), FlowDataType::Any);
    }
}
