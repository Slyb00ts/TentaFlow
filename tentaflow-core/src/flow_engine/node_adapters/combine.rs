// =============================================================================
// Plik: flow_engine/node_adapters/combine.rs
// Opis: CombineNodeAdapter — fan-in node. Konsumuje N incoming edges z
//       roznych branchy flow, czeka az wszystkie wygeneruja envelope, laczy
//       ich tekstowa reprezentacje w jeden output (FlowValue::Text). Zwolniony
//       z R4 (1-input-edge) w validation.rs.
//
//       Metadane (session_id w envelope.meta + ctx.session_id, conversation
//       history, system_prompts) bierze z pierwszego inputu — wszystkie
//       branche zaczynaja od tego samego triggera, wiec metadane sa zwykle
//       identyczne. Payload nadpisany na zlepiony tekst, artifacts z
//       pierwszego inputu zachowane. Variables z wszystkich live branchy
//       scalane per-key polityka z configu (§3.12).
// =============================================================================

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::flow_engine::envelope::{FlowEnvelope, FlowValue, NodeInput};
use crate::flow_engine::node_adapter::{ExecutionContext, NodeAdapter, PortSpec};
use crate::flow_engine::node_adapters::variable_merge::{merge_ordered, MergeSource};
use crate::flow_engine::types::{FlowDataType, FlowNode};

pub struct CombineNodeAdapter;

impl CombineNodeAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CombineNodeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

/// Domyslny separator miedzy textami z poszczegolnych branchy. Operator
/// moze nadpisac w `node.config["separator"]`.
const DEFAULT_SEPARATOR: &str = "\n\n";

#[async_trait]
impl NodeAdapter for CombineNodeAdapter {
    fn node_type(&self) -> &str {
        "combine"
    }

    fn input_ports(&self) -> Vec<PortSpec> {
        // Combine akceptuje wszystko (text, json, audio z pre-text bridge,
        // image z OCR itd.) — kazdy input mapowany na text representation
        // przez `flow_value_to_text`.
        vec![PortSpec::new("in", FlowDataType::Any)]
    }

    fn output_ports(&self) -> Vec<PortSpec> {
        vec![PortSpec::new("full", FlowDataType::Text)]
    }

    async fn execute(
        &self,
        node: &FlowNode,
        inputs: &[NodeInput],
        _ctx: &ExecutionContext,
    ) -> Result<FlowEnvelope> {
        if inputs.is_empty() {
            return Err(anyhow!(
                "combine node '{}' has no incoming edges (need >=1)",
                node.id
            ));
        }

        let separator = node
            .config
            .get("separator")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_SEPARATOR);

        // Deterministyczna kolejnosc: po `from_node_id`, zeby ten sam zestaw
        // branchy zawsze laczyl sie tak samo. Inputs nie sa gwarantowanie
        // posortowane przez executor.
        let mut sorted: Vec<&NodeInput> = inputs.iter().collect();
        sorted.sort_by(|a, b| a.from_node_id.cmp(&b.from_node_id));

        // Puste czlony odfiltrowane — inaczej audio (renderowane do "") albo
        // Empty payload zostawialy wiszace separatory w prompcie LLM.
        let parts: Vec<String> = sorted
            .iter()
            .map(|inp| flow_value_to_text(&inp.envelope.payload))
            .filter(|s| !s.is_empty())
            .collect();
        let joined = parts.join(separator);

        // Bierzemy envelope z pierwszego (po sortowaniu) brancha jako baze —
        // niesie session_id w meta, conversation context, artifacts. Pozostali
        // branche maja zwykle te same metadane (wspolny trigger), wiec ich
        // meta nie laczymy zeby uniknac konfliktu duplikatow.
        let mut out = (*sorted[0].envelope).clone();
        out.payload = FlowValue::Text(joined);
        // Variables z wszystkich live branchy scalane per-key polityka (§3.12).
        // Skipped poprzednicy nie maja outputu, wiec build_inputs ich pomija —
        // tu sa tylko zywe wejscia. Bez wejsc lub w liniowym flow (1 wejscie)
        // to passthrough variables pierwszego brancha. Deterministyczna
        // kolejnosc po from_node_id zachowana przez `sorted`.
        let sources: Vec<MergeSource<'_>> = sorted
            .iter()
            .map(|inp| MergeSource {
                port: Some(inp.from_port.as_str()),
                variables: &inp.envelope.variables,
            })
            .collect();
        out.variables = merge_ordered(node, &format!("combine node '{}'", node.id), &sources)?;
        Ok(out)
    }
}

/// Mapuje dowolny `FlowValue` na string. Dla typow blob-owych zwraca krotki
/// placeholder z mime, zeby downstream LLM widzial ze byl zalacznik bez
/// inline'owania bytes.
fn flow_value_to_text(v: &FlowValue) -> String {
    match v {
        FlowValue::Empty => String::new(),
        FlowValue::Text(s) => s.clone(),
        FlowValue::Json(j) => j.to_string(),
        // Audio NIE jest inline'owane do tekstu — w pipeline audio→tekst zawsze
        // idzie przez STT, a surowy placeholder "<audio: ...>" w prompcie LLM to
        // smiec (text LLM nic z nim nie zrobi). Zwracamy "" → combine je odfiltruje.
        FlowValue::Audio { .. } => String::new(),
        FlowValue::Image { mime, .. } => format!("<image: {mime}>"),
        FlowValue::Video { mime, .. } => format!("<video: {mime}>"),
        FlowValue::Embedding(values) => format!("<embedding: {} dims>", values.len()),
        FlowValue::Other { mime, filename, .. } => match filename {
            Some(name) => format!("<file: {name} ({mime})>"),
            None => format!("<file: {mime}>"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::node_adapter::test_support::stub_ctx;
    use std::sync::Arc;

    fn combine_node(separator: Option<&str>) -> FlowNode {
        let config = match separator {
            Some(s) => serde_json::json!({ "separator": s }),
            None => serde_json::Value::Null,
        };
        FlowNode {
            id: "c1".into(),
            node_type: "combine".into(),
            config,
            position: None,
            label: None,
            region: None,
        }
    }

    fn input(from_node_id: &str, payload: FlowValue) -> NodeInput {
        let mut env = FlowEnvelope::with_payload(payload);
        env.meta
            .insert("session_id".into(), serde_json::json!("test-session-42"));
        NodeInput {
            from_node_id: from_node_id.into(),
            from_port: "full".into(),
            envelope: Arc::new(env),
        }
    }

    fn input_with_vars(
        from_node_id: &str,
        from_port: &str,
        vars: &[(&str, FlowValue)],
    ) -> NodeInput {
        let mut env = FlowEnvelope::with_payload(FlowValue::Text(from_node_id.into()));
        for (k, v) in vars {
            env.variables.insert((*k).to_string(), v.clone());
        }
        NodeInput {
            from_node_id: from_node_id.into(),
            from_port: from_port.into(),
            envelope: Arc::new(env),
        }
    }

    fn combine_node_with_policy(policy: serde_json::Value) -> FlowNode {
        FlowNode {
            id: "c1".into(),
            node_type: "combine".into(),
            config: serde_json::json!({ "variable_merge_policy": policy }),
            position: None,
            label: None,
            region: None,
        }
    }

    #[tokio::test]
    async fn combine_joins_text_inputs_with_default_separator() {
        let adapter = CombineNodeAdapter::new();
        let inputs = vec![
            input("branch-a", FlowValue::Text("hello".into())),
            input("branch-b", FlowValue::Text("world".into())),
        ];
        let ctx = stub_ctx();
        let out = adapter
            .execute(&combine_node(None), &inputs, &ctx)
            .await
            .unwrap();
        assert_eq!(out.payload.as_text(), Some("hello\n\nworld"));
    }

    #[tokio::test]
    async fn combine_uses_custom_separator_from_config() {
        let adapter = CombineNodeAdapter::new();
        let inputs = vec![
            input("a", FlowValue::Text("x".into())),
            input("b", FlowValue::Text("y".into())),
            input("c", FlowValue::Text("z".into())),
        ];
        let ctx = stub_ctx();
        let out = adapter
            .execute(&combine_node(Some(" | ")), &inputs, &ctx)
            .await
            .unwrap();
        assert_eq!(out.payload.as_text(), Some("x | y | z"));
    }

    #[tokio::test]
    async fn combine_sorts_inputs_by_from_node_id_for_determinism() {
        let adapter = CombineNodeAdapter::new();
        let inputs = vec![
            input("z-last", FlowValue::Text("zzz".into())),
            input("a-first", FlowValue::Text("aaa".into())),
            input("m-mid", FlowValue::Text("mmm".into())),
        ];
        let ctx = stub_ctx();
        let out = adapter
            .execute(&combine_node(None), &inputs, &ctx)
            .await
            .unwrap();
        assert_eq!(out.payload.as_text(), Some("aaa\n\nmmm\n\nzzz"));
    }

    #[tokio::test]
    async fn combine_propagates_session_id_from_first_branch() {
        let adapter = CombineNodeAdapter::new();
        let inputs = vec![
            input("branch-a", FlowValue::Text("hi".into())),
            input("branch-b", FlowValue::Text("there".into())),
        ];
        let ctx = stub_ctx();
        let out = adapter
            .execute(&combine_node(None), &inputs, &ctx)
            .await
            .unwrap();
        assert_eq!(
            out.meta.get("session_id").and_then(|v| v.as_str()),
            Some("test-session-42")
        );
    }

    #[tokio::test]
    async fn combine_handles_mixed_payload_types() {
        let adapter = CombineNodeAdapter::new();
        let inputs = vec![
            input("branch-a", FlowValue::Text("transcript".into())),
            input(
                "branch-b",
                FlowValue::Other {
                    blob_ref: crate::flow_engine::blob_store::BlobRef {
                        id: "b1".into(),
                        sha256: "deadbeef".into(),
                        size_bytes: 100,
                        mime: "application/pdf".into(),
                    },
                    mime: "application/pdf".into(),
                    filename: Some("report.pdf".into()),
                },
            ),
        ];
        let ctx = stub_ctx();
        let out = adapter
            .execute(&combine_node(None), &inputs, &ctx)
            .await
            .unwrap();
        // Sorted: a (transcript), b (file placeholder)
        assert_eq!(
            out.payload.as_text(),
            Some("transcript\n\n<file: report.pdf (application/pdf)>")
        );
    }

    #[tokio::test]
    async fn combine_rejects_empty_inputs() {
        let adapter = CombineNodeAdapter::new();
        let ctx = stub_ctx();
        let err = adapter
            .execute(&combine_node(None), &[], &ctx)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no incoming edges"));
    }

    #[tokio::test]
    async fn combine_passthrough_variables_from_single_branch() {
        let adapter = CombineNodeAdapter::new();
        let inputs = vec![input_with_vars(
            "a",
            "full",
            &[("done", FlowValue::Json(serde_json::json!(true)))],
        )];
        let out = adapter
            .execute(&combine_node(None), &inputs, &stub_ctx())
            .await
            .unwrap();
        assert_eq!(
            out.variables.get("done"),
            Some(&FlowValue::Json(serde_json::json!(true)))
        );
    }

    #[tokio::test]
    async fn combine_merges_disjoint_variables_without_policy() {
        let adapter = CombineNodeAdapter::new();
        let inputs = vec![
            input_with_vars("a", "full", &[("x", FlowValue::Text("1".into()))]),
            input_with_vars("b", "full", &[("y", FlowValue::Text("2".into()))]),
        ];
        let out = adapter
            .execute(&combine_node(None), &inputs, &stub_ctx())
            .await
            .unwrap();
        assert_eq!(out.variables.get("x"), Some(&FlowValue::Text("1".into())));
        assert_eq!(out.variables.get("y"), Some(&FlowValue::Text("2".into())));
    }

    #[tokio::test]
    async fn combine_same_value_same_key_is_not_a_conflict() {
        let adapter = CombineNodeAdapter::new();
        let inputs = vec![
            input_with_vars("a", "full", &[("k", FlowValue::Text("same".into()))]),
            input_with_vars("b", "full", &[("k", FlowValue::Text("same".into()))]),
        ];
        let out = adapter
            .execute(&combine_node(None), &inputs, &stub_ctx())
            .await
            .unwrap();
        assert_eq!(
            out.variables.get("k"),
            Some(&FlowValue::Text("same".into()))
        );
    }

    #[tokio::test]
    async fn combine_conflicting_values_without_policy_is_error() {
        let adapter = CombineNodeAdapter::new();
        let inputs = vec![
            input_with_vars("a", "full", &[("k", FlowValue::Text("one".into()))]),
            input_with_vars("b", "full", &[("k", FlowValue::Text("two".into()))]),
        ];
        let err = adapter
            .execute(&combine_node(None), &inputs, &stub_ctx())
            .await
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("conflicting values for variable 'k'"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn combine_last_wins_policy_resolves_conflict() {
        let adapter = CombineNodeAdapter::new();
        // Sorted by from_node_id: a then b — b wins.
        let inputs = vec![
            input_with_vars("a", "full", &[("k", FlowValue::Text("one".into()))]),
            input_with_vars("b", "full", &[("k", FlowValue::Text("two".into()))]),
        ];
        let node = combine_node_with_policy(serde_json::json!({"k": "last_wins"}));
        let out = adapter.execute(&node, &inputs, &stub_ctx()).await.unwrap();
        assert_eq!(out.variables.get("k"), Some(&FlowValue::Text("two".into())));
    }

    #[tokio::test]
    async fn combine_prefer_port_policy_picks_winning_port() {
        let adapter = CombineNodeAdapter::new();
        let inputs = vec![
            input_with_vars("a", "draft", &[("k", FlowValue::Text("draft".into()))]),
            input_with_vars("b", "final", &[("k", FlowValue::Text("final".into()))]),
        ];
        let node = combine_node_with_policy(serde_json::json!({"k": "prefer_port:final"}));
        let out = adapter.execute(&node, &inputs, &stub_ctx()).await.unwrap();
        assert_eq!(
            out.variables.get("k"),
            Some(&FlowValue::Text("final".into()))
        );
    }

    #[tokio::test]
    async fn combine_collect_policy_builds_array_in_input_order() {
        let adapter = CombineNodeAdapter::new();
        let inputs = vec![
            input_with_vars("a", "full", &[("k", FlowValue::Text("a".into()))]),
            input_with_vars("b", "full", &[("k", FlowValue::Text("b".into()))]),
        ];
        let node = combine_node_with_policy(serde_json::json!({"k": "collect"}));
        let out = adapter.execute(&node, &inputs, &stub_ctx()).await.unwrap();
        assert_eq!(
            out.variables.get("k"),
            Some(&FlowValue::Json(serde_json::json!(["a", "b"])))
        );
    }

    #[tokio::test]
    async fn combine_unknown_merge_policy_is_error() {
        let adapter = CombineNodeAdapter::new();
        let inputs = vec![input_with_vars(
            "a",
            "full",
            &[("k", FlowValue::Text("v".into()))],
        )];
        let node = combine_node_with_policy(serde_json::json!({"k": "merge_somehow"}));
        let err = adapter
            .execute(&node, &inputs, &stub_ctx())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("unknown merge policy"), "{err}");
    }

    #[test]
    fn combine_advertises_correct_ports_and_types() {
        let a = CombineNodeAdapter::new();
        assert_eq!(a.node_type(), "combine");
        let in_names: Vec<String> = a.input_ports().iter().map(|p| p.name.clone()).collect();
        let out_names: Vec<String> = a.output_ports().iter().map(|p| p.name.clone()).collect();
        assert_eq!(in_names, vec!["in"]);
        assert_eq!(out_names, vec!["full"]);
        assert_eq!(a.input_port_type("in"), FlowDataType::Any);
        assert_eq!(a.output_port_type("full"), FlowDataType::Text);
    }
}
