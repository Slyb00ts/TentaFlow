// =============================================================================
// Plik: flow_engine/validation.rs
// Opis: Walidacja semantyczna FlowDefinition (plan v4.2). Single source of
//       truth dla reguł flow — wołane z `CompiledFlow::compile` (defense in
//       depth dla load z DB) i z `dispatch/handlers.rs` save flow.
//       Reguły:
//         R1. każdy edge.from / edge.to wskazuje na istniejący node
//         R2. każdy node ma adapter w registry
//         R3. edge.from_port ∈ supported_output_ports producenta;
//             edge.to_port ∈ supported_input_ports konsumenta
//         R4. strict 1-input-edge dla każdego non-trigger node'a
//         R5. dokładnie jeden trigger node
//         R6. condition edges (from_port "true"/"false") tylko z node'a
//             "condition"
//         R7. streaming end-shape — edge `from_port="stream"` musi prowadzić
//             do node'a "output" z config.mode="stream", bez nodów po LLM
//             na ścieżce do output. Co najwyżej jedna gałąź streaming na flow.
// =============================================================================

use std::collections::{HashMap, HashSet};
use std::fmt;

use crate::flow_engine::node_adapter::AdapterRegistry;
use crate::flow_engine::types::{FlowDataType, FlowDefinition};

#[derive(Debug, Clone)]
pub enum FlowValidationError {
    UnknownNode {
        edge_endpoint: &'static str,
        node_id: String,
    },
    UnknownAdapter {
        node_id: String,
        node_type: String,
    },
    InvalidOutputPort {
        node_id: String,
        node_type: String,
        port: String,
        available: Vec<String>,
    },
    InvalidInputPort {
        node_id: String,
        node_type: String,
        port: String,
        available: Vec<String>,
    },
    MultipleInputs {
        node_id: String,
        actual: usize,
    },
    TriggerCount {
        actual: usize,
    },
    ConditionEdgeFromNonCondition {
        node_id: String,
        node_type: String,
        port: String,
    },
    StreamingNotToOutput {
        from_node: String,
        to_node: String,
    },
    StreamingOutputModeMismatch {
        node_id: String,
        actual: String,
    },
    MultipleStreamingBranches {
        count: usize,
    },
    /// R8: edge.data_type vs producent/konsument port_type.
    EdgeTypeMismatch {
        edge_id: String,
        side: &'static str,
        edge_type: FlowDataType,
        port_type: FlowDataType,
    },
    /// R8: producent.output_port_type vs konsument.input_port_type — oba
    /// konkretne typy, niekompatybilne.
    EdgePortTypesMismatch {
        from_node: String,
        from_port: String,
        from_type: FlowDataType,
        to_node: String,
        to_port: String,
        to_type: FlowDataType,
    },
}

impl fmt::Display for FlowValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownNode {
                edge_endpoint,
                node_id,
            } => write!(
                f,
                "edge {edge_endpoint} references unknown node '{node_id}'"
            ),
            Self::UnknownAdapter { node_id, node_type } => write!(
                f,
                "node '{node_id}' uses unregistered adapter type '{node_type}'"
            ),
            Self::InvalidOutputPort {
                node_id,
                node_type,
                port,
                available,
            } => write!(
                f,
                "node '{node_id}' (type '{node_type}') has no output port '{port}', available: {available:?}"
            ),
            Self::InvalidInputPort {
                node_id,
                node_type,
                port,
                available,
            } => write!(
                f,
                "node '{node_id}' (type '{node_type}') has no input port '{port}', available: {available:?}"
            ),
            Self::MultipleInputs { node_id, actual } => write!(
                f,
                "node '{node_id}' has {actual} incoming edges (1-input-edge rule)"
            ),
            Self::TriggerCount { actual } => write!(
                f,
                "flow must have exactly one trigger node, found {actual}"
            ),
            Self::ConditionEdgeFromNonCondition {
                node_id,
                node_type,
                port,
            } => write!(
                f,
                "edge from_port '{port}' (true/false) only allowed on 'condition' node, got '{node_id}' (type '{node_type}')"
            ),
            Self::StreamingNotToOutput { from_node, to_node } => write!(
                f,
                "streaming edge from '{from_node}' must lead to an 'output' node, got '{to_node}'"
            ),
            Self::StreamingOutputModeMismatch { node_id, actual } => write!(
                f,
                "streaming flow output node '{node_id}' must have config.mode='stream', got '{actual}'"
            ),
            Self::MultipleStreamingBranches { count } => write!(
                f,
                "flow has {count} streaming branches; only one allowed"
            ),
            Self::EdgeTypeMismatch {
                edge_id,
                side,
                edge_type,
                port_type,
            } => write!(
                f,
                "edge '{edge_id}' data_type {edge_type:?} incompatible with {side} port type {port_type:?}"
            ),
            Self::EdgePortTypesMismatch {
                from_node,
                from_port,
                from_type,
                to_node,
                to_port,
                to_type,
            } => write!(
                f,
                "edge {from_node}.{from_port} (type {from_type:?}) -> {to_node}.{to_port} (type {to_type:?}): incompatible types"
            ),
        }
    }
}

impl std::error::Error for FlowValidationError {}

/// Źródło flow definition — decyduje czy mandatoryjne reguły jak R-SAFETY
/// (pii_filter na chain LLM) są egzekwowane.
///
/// `UserDefined` — flow zapisany przez admina w DB (handlers save path,
/// seedy). Pełna walidacja, R-SAFETY enforce.
///
/// `Synthetic` — ad-hoc flow zbudowany w runtime (FlowDispatcher fallback
/// gdy resolver zwraca None dla danego modelu). R-SAFETY skip — synthetic
/// ma trivial topology trigger→capability→output, bez chain'a, R-SAFETY
/// nieaplikowalny. Admin który nie zdefiniował flow akceptuje raw output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationSource {
    UserDefined,
    Synthetic,
}

pub fn validate(
    def: &FlowDefinition,
    registry: &AdapterRegistry,
    source: ValidationSource,
) -> Result<(), FlowValidationError> {
    let _ = source;
    let nodes_by_id: HashMap<&str, &crate::flow_engine::types::FlowNode> =
        def.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    // R5 trigger uniqueness
    let trigger_count = def.nodes.iter().filter(|n| n.node_type == "trigger").count();
    if trigger_count != 1 {
        return Err(FlowValidationError::TriggerCount {
            actual: trigger_count,
        });
    }

    // R2 + port shape sanity
    for node in &def.nodes {
        if !registry.has(&node.node_type) {
            return Err(FlowValidationError::UnknownAdapter {
                node_id: node.id.clone(),
                node_type: node.node_type.clone(),
            });
        }
    }

    // R1, R3, R4, R6
    let mut incoming_count: HashMap<&str, usize> = HashMap::new();
    for edge in &def.edges {
        let from_node = nodes_by_id
            .get(edge.from.as_str())
            .ok_or_else(|| FlowValidationError::UnknownNode {
                edge_endpoint: "from",
                node_id: edge.from.clone(),
            })?;
        let to_node = nodes_by_id
            .get(edge.to.as_str())
            .ok_or_else(|| FlowValidationError::UnknownNode {
                edge_endpoint: "to",
                node_id: edge.to.clone(),
            })?;

        let from_adapter = registry.get(&from_node.node_type).expect("R2 enforced above");
        let to_adapter = registry.get(&to_node.node_type).expect("R2 enforced above");

        // R6: condition-port edges (`true`/`false`) tylko z node'a `condition`.
        // Sprawdzamy PRZED port-membership żeby błąd był jasny: "to nie jest
        // condition" zamiast generycznego "port not in list".
        if matches!(edge.from_port.as_str(), "true" | "false")
            && from_node.node_type != "condition"
        {
            return Err(FlowValidationError::ConditionEdgeFromNonCondition {
                node_id: from_node.id.clone(),
                node_type: from_node.node_type.clone(),
                port: edge.from_port.clone(),
            });
        }

        let out_ports = from_adapter.supported_output_ports();
        if !out_ports.contains(&edge.from_port.as_str()) {
            return Err(FlowValidationError::InvalidOutputPort {
                node_id: from_node.id.clone(),
                node_type: from_node.node_type.clone(),
                port: edge.from_port.clone(),
                available: out_ports.iter().map(|s| s.to_string()).collect(),
            });
        }
        let in_ports = to_adapter.supported_input_ports();
        if !in_ports.contains(&edge.to_port.as_str()) {
            return Err(FlowValidationError::InvalidInputPort {
                node_id: to_node.id.clone(),
                node_type: to_node.node_type.clone(),
                port: edge.to_port.clone(),
                available: in_ports.iter().map(|s| s.to_string()).collect(),
            });
        }

        // R8: typed edge compatibility. Trzy niezależne pary muszą być
        // compatible. Edge.data_type to deklaracja, NIE konwerter — gdy
        // producent Text a konsument Audio, edge.data_type cokolwiek nie
        // pomoże. `Any` na której kolwiek stronie = wildcard.
        let from_type = from_adapter.output_port_type(&edge.from_port);
        let to_type = to_adapter.input_port_type(&edge.to_port);
        if !from_type.compatible_with(to_type) {
            return Err(FlowValidationError::EdgePortTypesMismatch {
                from_node: from_node.id.clone(),
                from_port: edge.from_port.clone(),
                from_type,
                to_node: to_node.id.clone(),
                to_port: edge.to_port.clone(),
                to_type,
            });
        }
        let edge_id = edge
            .id
            .clone()
            .unwrap_or_else(|| format!("{}->{}", edge.from, edge.to));
        if !edge.data_type.compatible_with(from_type) {
            return Err(FlowValidationError::EdgeTypeMismatch {
                edge_id: edge_id.clone(),
                side: "from",
                edge_type: edge.data_type,
                port_type: from_type,
            });
        }
        if !edge.data_type.compatible_with(to_type) {
            return Err(FlowValidationError::EdgeTypeMismatch {
                edge_id,
                side: "to",
                edge_type: edge.data_type,
                port_type: to_type,
            });
        }

        *incoming_count.entry(to_node.id.as_str()).or_insert(0) += 1;
    }

    // R4: trigger ma 0 incoming (jest źródłem flow), każdy non-trigger ≤1.
    for node in &def.nodes {
        let count = incoming_count.get(node.id.as_str()).copied().unwrap_or(0);
        if node.node_type == "trigger" {
            if count > 0 {
                return Err(FlowValidationError::MultipleInputs {
                    node_id: node.id.clone(),
                    actual: count,
                });
            }
            continue;
        }
        if count > 1 {
            return Err(FlowValidationError::MultipleInputs {
                node_id: node.id.clone(),
                actual: count,
            });
        }
    }

    // R7: streaming end-shape (Stage 3d Krok 2d update — chain support).
    //
    // Reguła: edge `from_port="stream"` może iść albo bezpośrednio do
    // `output(mode=stream)`, albo do streaming-aware node'a (np. pii_filter,
    // tts_stream_bridge), który dalej feeduje stream chain — chain musi się
    // ostatecznie zakończyć na `output(mode=stream)`.
    //
    // - producent stream edge'a może mieć dwa wyjścia (np. `stream` + `full`
    //   dla mixed blocking + streaming flow), ale `from_port="stream"`
    //   może być tylko jeden.
    // - intermediate chain nodes wykrywane przez walk po `from_port="stream"`
    //   edges. Każdy intermediate node MUSI być w streaming_adapters slot
    //   rejestru (lookup w executor — runtime fail, R7 sprawdza tylko
    //   strukturę chain'a).
    let stream_edges: Vec<_> = def
        .edges
        .iter()
        .filter(|e| e.from_port == "stream")
        .collect();

    // R7 multi-branch guard: każdy node może mieć MAX 1 wychodzący edge
    // z `from_port="stream"`. Linear chain (stream → stream → stream)
    // OK; równoległe rozgałęzienie (jeden node ma 2 stream edges →
    // różne sink'i) odrzucone — runtime executor i tak fold'uje tylko
    // jedną ścieżkę, druga byłaby ignorowana.
    let mut stream_out_count: HashMap<&str, usize> = HashMap::new();
    for edge in &stream_edges {
        *stream_out_count.entry(edge.from.as_str()).or_insert(0) += 1;
    }
    for (node_id, count) in &stream_out_count {
        if *count > 1 {
            return Err(FlowValidationError::MultipleStreamingBranches {
                count: *count,
            });
        }
        let _ = node_id;
    }

    // Walk chain: zacznij od pierwszego stream edge'a, follow `from_port=
    // "stream"` aż do output sink. Wykrywaj cykle przez seen set.
    if !stream_edges.is_empty() {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut current_id = stream_edges[0].from.as_str();
        seen.insert(current_id);
        loop {
            let next_edge = def
                .edges
                .iter()
                .find(|e| e.from == current_id && e.from_port == "stream");
            let Some(edge) = next_edge else {
                // Brak dalszego stream edge'a — chain musi się skończyć na
                // output(mode=stream); jeśli current_id to nie output,
                // chain wisi w powietrzu.
                let last_node = nodes_by_id[current_id];
                if last_node.node_type != "output" {
                    return Err(FlowValidationError::StreamingNotToOutput {
                        from_node: current_id.to_string(),
                        to_node: "<chain end without output sink>".to_string(),
                    });
                }
                break;
            };
            let to_node = nodes_by_id[edge.to.as_str()];
            if to_node.node_type == "output" {
                let mode = to_node
                    .config
                    .get("mode")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if mode != "stream" {
                    return Err(FlowValidationError::StreamingOutputModeMismatch {
                        node_id: to_node.id.clone(),
                        actual: mode.to_string(),
                    });
                }
                break;
            }
            // Intermediate chain node — sprawdź że nie jest cyklem. Walidacja
            // czy node ma StreamingNodeAdapter zostaje na runtime executor
            // lookup (R7 nie ma dostępu do streaming_adapters slot, tylko
            // node_type registration).
            if !seen.insert(edge.to.as_str()) {
                return Err(FlowValidationError::StreamingNotToOutput {
                    from_node: edge.from.clone(),
                    to_node: format!("{} (cycle)", edge.to),
                });
            }
            current_id = edge.to.as_str();
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::node_adapters::{
        ConditionNodeAdapter, LlmNodeAdapter, OutputNodeAdapter, TriggerNodeAdapter,
    };
    use std::sync::Arc;

    fn registry() -> AdapterRegistry {
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        r.register(Arc::new(ConditionNodeAdapter::new()));
        r.register_llm(Arc::new(LlmNodeAdapter::new()));
        r
    }

    fn parse(json: &str) -> FlowDefinition {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn ok_minimal_flow() {
        let def = parse(
            r#"{"nodes":[{"id":"t","type":"trigger","config":{}},{"id":"o","type":"output","config":{}}],"edges":[{"from":"t","to":"o"}]}"#,
        );
        validate(&def, &registry(), crate::flow_engine::validation::ValidationSource::UserDefined).unwrap();
    }

    #[test]
    fn rejects_no_trigger() {
        let def = parse(
            r#"{"nodes":[{"id":"o","type":"output","config":{}}],"edges":[]}"#,
        );
        let err = validate(&def, &registry(), crate::flow_engine::validation::ValidationSource::UserDefined).unwrap_err();
        assert!(matches!(err, FlowValidationError::TriggerCount { actual: 0 }));
    }

    #[test]
    fn rejects_two_triggers() {
        let def = parse(
            r#"{"nodes":[{"id":"t1","type":"trigger","config":{}},{"id":"t2","type":"trigger","config":{}}],"edges":[]}"#,
        );
        let err = validate(&def, &registry(), crate::flow_engine::validation::ValidationSource::UserDefined).unwrap_err();
        assert!(matches!(err, FlowValidationError::TriggerCount { actual: 2 }));
    }

    #[test]
    fn rejects_multi_input_edge() {
        let def = parse(
            r#"{
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"c","type":"condition","config":{}},
                    {"id":"o","type":"output","config":{}}
                ],
                "edges":[
                    {"from":"t","to":"c"},
                    {"from":"c","to":"o","from_port":"true"},
                    {"from":"c","to":"o","from_port":"false"}
                ]
            }"#,
        );
        let err = validate(&def, &registry(), crate::flow_engine::validation::ValidationSource::UserDefined).unwrap_err();
        assert!(matches!(err, FlowValidationError::MultipleInputs { .. }));
    }

    #[test]
    fn rejects_unknown_adapter() {
        let def = parse(
            r#"{"nodes":[{"id":"t","type":"trigger","config":{}},{"id":"x","type":"mystery","config":{}}],"edges":[{"from":"t","to":"x"}]}"#,
        );
        let err = validate(&def, &registry(), crate::flow_engine::validation::ValidationSource::UserDefined).unwrap_err();
        assert!(matches!(err, FlowValidationError::UnknownAdapter { .. }));
    }

    #[test]
    fn ok_streaming_shape() {
        let def = parse(
            r#"{
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"l","type":"llm","config":{"model":"m"}},
                    {"id":"o","type":"output","config":{"mode":"stream"}}
                ],
                "edges":[
                    {"from":"t","to":"l"},
                    {"from":"l","to":"o","from_port":"stream"}
                ]
            }"#,
        );
        validate(&def, &registry(), crate::flow_engine::validation::ValidationSource::UserDefined).unwrap();
    }

    #[test]
    fn rejects_streaming_to_non_output() {
        let def = parse(
            r#"{
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"l","type":"llm","config":{"model":"m"}},
                    {"id":"c","type":"condition","config":{}}
                ],
                "edges":[
                    {"from":"t","to":"l"},
                    {"from":"l","to":"c","from_port":"stream"}
                ]
            }"#,
        );
        let err = validate(&def, &registry(), crate::flow_engine::validation::ValidationSource::UserDefined).unwrap_err();
        assert!(matches!(
            err,
            FlowValidationError::StreamingNotToOutput { .. }
        ));
    }

    #[test]
    fn rejects_streaming_without_mode_stream() {
        let def = parse(
            r#"{
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"l","type":"llm","config":{"model":"m"}},
                    {"id":"o","type":"output","config":{}}
                ],
                "edges":[
                    {"from":"t","to":"l"},
                    {"from":"l","to":"o","from_port":"stream"}
                ]
            }"#,
        );
        let err = validate(&def, &registry(), crate::flow_engine::validation::ValidationSource::UserDefined).unwrap_err();
        assert!(matches!(
            err,
            FlowValidationError::StreamingOutputModeMismatch { .. }
        ));
    }

    #[test]
    fn r8_rejects_text_to_audio_port_mismatch() {
        // tts adapter ma input_port_type = Text, ale w tym flow podajemy mu
        // edge z llm.full (Text). Ten przypadek przechodzi (Text → Text).
        // Negatywny: stt_adapter ma input_port_type = Audio, llm produkuje
        // Text → mismatch.
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        r.register(Arc::new(crate::flow_engine::node_adapters::SttNodeAdapter::new()));
        r.register_llm(Arc::new(LlmNodeAdapter::new()));

        let def = parse(
            r#"{
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"l","type":"llm","config":{"model":"m"}},
                    {"id":"s","type":"stt","config":{"model":"w"}},
                    {"id":"o","type":"output","config":{}}
                ],
                "edges":[
                    {"from":"t","to":"l"},
                    {"from":"l","to":"s"},
                    {"from":"s","to":"o"}
                ]
            }"#,
        );
        let err = validate(&def, &r, crate::flow_engine::validation::ValidationSource::UserDefined).unwrap_err();
        assert!(matches!(
            err,
            FlowValidationError::EdgePortTypesMismatch { .. }
        ), "got {:?}", err);
    }

    #[test]
    fn r8_accepts_explicit_data_type_when_matching() {
        // pii_filter (Text → Text) z explicit edge.data_type = "text" przechodzi.
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        r.register(Arc::new(crate::flow_engine::node_adapters::PiiFilterNodeAdapter::new()));

        let def = parse(
            r#"{
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"p","type":"pii_filter","config":{}},
                    {"id":"o","type":"output","config":{}}
                ],
                "edges":[
                    {"from":"t","to":"p","data_type":"text"},
                    {"from":"p","to":"o","data_type":"text"}
                ]
            }"#,
        );
        validate(&def, &r, crate::flow_engine::validation::ValidationSource::UserDefined).unwrap();
    }

    #[test]
    fn r8_rejects_explicit_edge_type_mismatching_producer() {
        // pii_filter produkuje Text, ale edge deklaruje Audio.
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        r.register(Arc::new(crate::flow_engine::node_adapters::PiiFilterNodeAdapter::new()));

        let def = parse(
            r#"{
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"p","type":"pii_filter","config":{}},
                    {"id":"o","type":"output","config":{}}
                ],
                "edges":[
                    {"from":"t","to":"p"},
                    {"from":"p","to":"o","data_type":"audio"}
                ]
            }"#,
        );
        let err = validate(&def, &r, crate::flow_engine::validation::ValidationSource::UserDefined).unwrap_err();
        assert!(matches!(
            err,
            FlowValidationError::EdgeTypeMismatch { side: "from", .. }
        ), "got {:?}", err);
    }

    /// Stage 3d Krok 2d: R7 update — chain z streaming-aware intermediate
    /// nodes. Validator akceptuje `llm.stream → pii_filter → output(stream)`.
    #[test]
    fn accepts_streaming_chain_with_intermediate_node() {
        use crate::flow_engine::node_adapters::PiiFilterNodeAdapter;
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        r.register_streaming(Arc::new(PiiFilterNodeAdapter::new()));
        r.register_llm(Arc::new(LlmNodeAdapter::new()));

        let def = parse(
            r#"{
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"l","type":"llm","config":{}},
                    {"id":"p","type":"pii_filter","config":{}},
                    {"id":"o","type":"output","config":{"mode":"stream"}}
                ],
                "edges":[
                    {"from":"t","to":"l"},
                    {"from":"l","to":"p","from_port":"stream"},
                    {"from":"p","to":"o","from_port":"stream"}
                ]
            }"#,
        );
        let res = validate(
            &def,
            &r,
            crate::flow_engine::validation::ValidationSource::UserDefined,
        );
        assert!(res.is_ok(), "expected chain to pass R7, got: {:?}", res.err());
    }

    /// R7 multi-branch guard: pojedynczy node nie może mieć dwóch
    /// wychodzących stream edges. Walidator wcześniej milczał i runtime
    /// fold'ował tylko jedną ścieżkę, druga była ignorowana.
    #[test]
    fn rejects_multiple_stream_branches_from_same_node() {
        use crate::flow_engine::node_adapters::PiiFilterNodeAdapter;
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        r.register_streaming(Arc::new(PiiFilterNodeAdapter::new()));
        r.register_llm(Arc::new(LlmNodeAdapter::new()));

        let def = parse(
            r#"{
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"l","type":"llm","config":{}},
                    {"id":"o1","type":"output","config":{"mode":"stream"}},
                    {"id":"o2","type":"output","config":{"mode":"stream"}}
                ],
                "edges":[
                    {"from":"t","to":"l"},
                    {"from":"l","to":"o1","from_port":"stream"},
                    {"from":"l","to":"o2","from_port":"stream"}
                ]
            }"#,
        );
        let err = validate(
            &def,
            &r,
            crate::flow_engine::validation::ValidationSource::UserDefined,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            FlowValidationError::MultipleStreamingBranches { .. }
        ));
    }

    /// Chain bez output sink (pii_filter na końcu) odrzucony przez R7.
    #[test]
    fn rejects_streaming_chain_without_output_sink() {
        use crate::flow_engine::node_adapters::PiiFilterNodeAdapter;
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        r.register_streaming(Arc::new(PiiFilterNodeAdapter::new()));
        r.register_llm(Arc::new(LlmNodeAdapter::new()));

        let def = parse(
            r#"{
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"l","type":"llm","config":{}},
                    {"id":"p","type":"pii_filter","config":{}}
                ],
                "edges":[
                    {"from":"t","to":"l"},
                    {"from":"l","to":"p","from_port":"stream"}
                ]
            }"#,
        );
        let err = validate(
            &def,
            &r,
            crate::flow_engine::validation::ValidationSource::UserDefined,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            FlowValidationError::StreamingNotToOutput { .. }
        ));
    }

    #[test]
    fn rejects_condition_port_from_non_condition() {
        let def = parse(
            r#"{
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"o","type":"output","config":{}}
                ],
                "edges":[
                    {"from":"t","to":"o","from_port":"true"}
                ]
            }"#,
        );
        let err = validate(&def, &registry(), crate::flow_engine::validation::ValidationSource::UserDefined).unwrap_err();
        assert!(matches!(
            err,
            FlowValidationError::ConditionEdgeFromNonCondition { .. }
        ));
    }
}
