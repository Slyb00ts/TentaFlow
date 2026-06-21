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
        node: &FlowNode,
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

        // RAG E2.0 — gdy flow jawnie tego zażąda (`emit_citations: true`) i
        // envelope niesie realne cytaty retrievalu w meta["rag_citations"],
        // serializuj odpowiedź jako `{answer, citations}` JSON do payloadu Text.
        // Dzięki temu addon dostaje odpowiedź LLM RAZEM z cytatami wybranymi
        // dokładnie przez retrieval (jedno źródło prawdy), bez osobnego SELECT-a.
        // Bez flagi output zachowuje się jak dotąd (passthrough) — generyczne
        // flow nie są dotknięte.
        let emit = node
            .config
            .get("emit_citations")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if emit {
            if let (Some(answer), Some(citations)) = (
                out.payload.as_text().map(str::to_string),
                out.meta.get("rag_citations").cloned(),
            ) {
                let wrapped = serde_json::json!({
                    "answer": answer,
                    "citations": citations,
                });
                out.payload =
                    FlowValue::Text(serde_json::to_string(&wrapped).unwrap_or(answer));
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

    #[tokio::test]
    async fn output_emits_answer_and_citations_when_flagged() {
        // RAG E2.0 bug 2 — z emit_citations output serializuje {answer, citations}
        // z meta.rag_citations (realne hity) razem z tekstem LLM.
        let adapter = OutputNodeAdapter::new();
        let mut env = FlowEnvelope::with_payload(FlowValue::Text("Odpowiedz LLM".into()));
        env.meta.insert(
            "rag_citations".into(),
            serde_json::json!([{"doc_id": "d1", "chunk_index": 3, "text": "t", "score": 0.2}]),
        );
        let inputs = vec![NodeInput {
            from_node_id: "llm".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }];
        let node = FlowNode {
            id: "out-1".into(),
            node_type: "output".into(),
            config: serde_json::json!({ "emit_citations": true }),
            position: None,
            label: None,
            region: None,
        };
        let r = adapter.execute(&node, &inputs, &stub_ctx()).await.unwrap();
        let text = r.payload.as_text().expect("payload Text");
        let parsed: serde_json::Value = serde_json::from_str(text).expect("treść to JSON");
        assert_eq!(parsed["answer"].as_str(), Some("Odpowiedz LLM"));
        assert_eq!(parsed["citations"].as_array().map(|a| a.len()), Some(1));
        assert_eq!(parsed["citations"][0]["doc_id"].as_str(), Some("d1"));
        assert_eq!(parsed["citations"][0]["chunk_index"].as_i64(), Some(3));
    }

    #[tokio::test]
    async fn output_passthrough_without_flag() {
        // Bez emit_citations output zachowuje się jak passthrough (generyczne flow).
        let adapter = OutputNodeAdapter::new();
        let mut env = FlowEnvelope::with_payload(FlowValue::Text("plain".into()));
        env.meta
            .insert("rag_citations".into(), serde_json::json!([{"doc_id": "d"}]));
        let inputs = vec![NodeInput {
            from_node_id: "llm".into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }];
        let r = adapter
            .execute(&output_node(), &inputs, &stub_ctx())
            .await
            .unwrap();
        assert_eq!(r.payload.as_text(), Some("plain"));
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
