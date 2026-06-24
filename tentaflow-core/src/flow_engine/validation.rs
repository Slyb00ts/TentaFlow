// =============================================================================
// Plik: flow_engine/validation.rs
// Opis: Walidacja semantyczna FlowDefinition (plan v4.2). Single source of
//       truth dla reguł flow — wołane z `CompiledFlow::compile` (defense in
//       depth dla load z DB) i z `dispatch/handlers.rs` save flow.
//       Reguły:
//         R1. każdy edge.from / edge.to wskazuje na istniejący node
//         R2. każdy node ma adapter w registry
//         R3. edge.from_port ∈ output_ports producenta;
//             edge.to_port ∈ input_ports konsumenta
//         R4. strict 1-input-edge dla każdego non-entry node'a
//         R5. dokładnie jeden węzeł-entry; entry ∈ {`trigger`,
//             `on_subagent_complete`} — request-driven XOR event-driven flow
//         R6. condition edges (from_port "true"/"false") tylko z node'a
//             "condition"
//         R7. streaming end-shape — edge `from_port="stream"` musi prowadzić
//             do node'a "output" z config.mode="stream". Head łańcucha musi być
//             zarejestrowanym `StreamProducerAdapter` (§3.11 B — nie zakładamy
//             już LLM). Co najwyżej jedna gałąź streaming na node (poza output).
//         R10. każda zmienna docelowa `output_mapping` node'a musi być
//             zadeklarowana w sekcji `variables` flow (§3.12). Brak sekcji =
//             zero dozwolonych zmiennych → output_mapping do czegokolwiek błąd.
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
    /// R5: a flow must have exactly one entry node, where an entry is either a
    /// `trigger` (request-driven) or an `on_subagent_complete` (event-driven).
    EntryNodeCount {
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
    /// R10: `output_mapping` zapisuje do zmiennej spoza sekcji `variables`.
    UndeclaredVariableTarget {
        node_id: String,
        variable: String,
    },
    /// R10: deklaracja `input_mapping`/`output_mapping` nie jest obiektem
    /// {klucz: "<CEL>"} albo wartość nie jest stringiem.
    MalformedMapping {
        node_id: String,
        mapping: &'static str,
        detail: String,
    },
    /// R7 (§3.11 B): head of a stream chain (`from_port="stream"` producer)
    /// has no registered `StreamProducerAdapter`.
    StreamProducerNotRegistered {
        node_id: String,
        node_type: String,
    },
    /// R7 (§3.12): a stream-producing node declares io-mapping. The streaming
    /// dispatch path drives the producer directly (no executor overlay), so
    /// `input_mapping`/`output_mapping` would silently no-op while the same
    /// flow's blocking path applies them — rejected to keep the two paths
    /// from diverging.
    StreamProducerWithIoMapping {
        node_id: String,
        node_type: String,
        mapping: &'static str,
    },
    /// R11: an inline loop region is structurally malformed. `detail` carries
    /// the specific breach (entry/exit count, boundary crossing, back-edge span,
    /// iteration cap) for the editor.
    InvalidLoopRegion {
        region_id: String,
        detail: String,
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
            Self::EntryNodeCount { actual } => write!(
                f,
                "flow must have exactly one entry node (trigger or on_subagent_complete), found {actual}"
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
            Self::UndeclaredVariableTarget { node_id, variable } => write!(
                f,
                "node '{node_id}' output_mapping writes undeclared variable '{variable}'; \
                 declare it in the flow's variables section"
            ),
            Self::MalformedMapping {
                node_id,
                mapping,
                detail,
            } => write!(f, "node '{node_id}' {mapping} is malformed: {detail}"),
            Self::StreamProducerNotRegistered { node_id, node_type } => write!(
                f,
                "node '{node_id}' (type '{node_type}') produces a stream edge but has no \
                 registered StreamProducerAdapter"
            ),
            Self::StreamProducerWithIoMapping {
                node_id,
                node_type,
                mapping,
            } => write!(
                f,
                "node '{node_id}' (type '{node_type}') is a stream producer and cannot declare \
                 {mapping}; the streaming path does not apply io-mapping to the producer"
            ),
            Self::InvalidLoopRegion { region_id, detail } => {
                write!(f, "loop region '{region_id}' is invalid: {detail}")
            }
        }
    }
}

impl std::error::Error for FlowValidationError {}

/// Entry node types — the two mutually exclusive flow entries. `trigger` is
/// request-driven; `on_subagent_complete` is event-driven (the reactor seeds it
/// when a sub-agent run settles). Both are sources (0 incoming edges) and emit
/// the seeded initial envelope. R5 requires exactly one entry of either kind.
pub fn is_entry_node_type(node_type: &str) -> bool {
    matches!(node_type, "trigger" | "on_subagent_complete")
}

/// Walidacja STRUKTURALNA bez rejestru adapterów (RAG E2.0 bug 4). Sprawdza
/// reguły niezależne od specyfikacji portów adaptera: R1 (każdy endpoint krawędzi
/// wskazuje na istniejący node), R5 (dokładnie jeden węzeł-entry), unikalność
/// node id i niepustość grafu. Używana przy rejestracji engine-flow gdy globalny
/// dispatcher (rejestr) nie jest jeszcze dostępny — żeby NIGDY nie persystować
/// strukturalnie niepoprawnego DAG. Gdy rejestr jest dostępny, wołamy pełne
/// `validate` (R1–R10), które ten podzbiór zawiera.
pub fn validate_structural(def: &FlowDefinition) -> Result<(), FlowValidationError> {
    let mut ids: HashSet<&str> = HashSet::with_capacity(def.nodes.len());
    for node in &def.nodes {
        if !ids.insert(node.id.as_str()) {
            return Err(FlowValidationError::UnknownAdapter {
                node_id: node.id.clone(),
                node_type: format!("duplicate node id '{}'", node.id),
            });
        }
    }

    let entry_count = def
        .nodes
        .iter()
        .filter(|n| is_entry_node_type(&n.node_type))
        .count();
    if entry_count != 1 {
        return Err(FlowValidationError::EntryNodeCount {
            actual: entry_count,
        });
    }

    for edge in &def.edges {
        if !ids.contains(edge.from.as_str()) {
            return Err(FlowValidationError::UnknownNode {
                edge_endpoint: "from",
                node_id: edge.from.clone(),
            });
        }
        if !ids.contains(edge.to.as_str()) {
            return Err(FlowValidationError::UnknownNode {
                edge_endpoint: "to",
                node_id: edge.to.clone(),
            });
        }
    }
    Ok(())
}

pub fn validate(
    def: &FlowDefinition,
    registry: &AdapterRegistry,
) -> Result<(), FlowValidationError> {
    let nodes_by_id: HashMap<&str, &crate::flow_engine::types::FlowNode> =
        def.nodes.iter().map(|n| (n.id.as_str(), n)).collect();

    // R5 entry uniqueness — exactly one entry, of either entry kind.
    let entry_count = def
        .nodes
        .iter()
        .filter(|n| is_entry_node_type(&n.node_type))
        .count();
    if entry_count != 1 {
        return Err(FlowValidationError::EntryNodeCount {
            actual: entry_count,
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
        let from_node = nodes_by_id.get(edge.from.as_str()).ok_or_else(|| {
            FlowValidationError::UnknownNode {
                edge_endpoint: "from",
                node_id: edge.from.clone(),
            }
        })?;
        let to_node =
            nodes_by_id
                .get(edge.to.as_str())
                .ok_or_else(|| FlowValidationError::UnknownNode {
                    edge_endpoint: "to",
                    node_id: edge.to.clone(),
                })?;

        let from_adapter = registry
            .get(&from_node.node_type)
            .expect("R2 enforced above");
        let to_adapter = registry.get(&to_node.node_type).expect("R2 enforced above");

        // R6: condition-port edges (`true`/`false`) tylko z node'a `condition`.
        // Sprawdzamy PRZED port-membership żeby błąd był jasny: "to nie jest
        // condition" zamiast generycznego "port not in list".
        if matches!(edge.from_port.as_str(), "true" | "false") && from_node.node_type != "condition"
        {
            return Err(FlowValidationError::ConditionEdgeFromNonCondition {
                node_id: from_node.id.clone(),
                node_type: from_node.node_type.clone(),
                port: edge.from_port.clone(),
            });
        }

        let out_ports = from_adapter.output_ports();
        if !out_ports.iter().any(|p| p.name == edge.from_port) {
            return Err(FlowValidationError::InvalidOutputPort {
                node_id: from_node.id.clone(),
                node_type: from_node.node_type.clone(),
                port: edge.from_port.clone(),
                available: out_ports.iter().map(|p| p.name.clone()).collect(),
            });
        }
        let in_ports = to_adapter.input_ports();
        if !in_ports.iter().any(|p| p.name == edge.to_port) {
            return Err(FlowValidationError::InvalidInputPort {
                node_id: to_node.id.clone(),
                node_type: to_node.node_type.clone(),
                port: edge.to_port.clone(),
                available: in_ports.iter().map(|p| p.name.clone()).collect(),
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

        // R4 counts only forward edges. The inline loop-region back edge feeds
        // the entry node a second time but is NOT a structural input — without
        // this exclusion the entry node would always read as 2-input and fail
        // the 1-input-edge rule.
        if !edge.is_loop_back() {
            *incoming_count.entry(to_node.id.as_str()).or_insert(0) += 1;
        }
    }

    // R4: an entry node has 0 incoming (it is the flow source), every non-entry
    // node ≤1. Wyjątek: `combine` to fan-in node ktory z definicji konsumuje N
    // incoming edges (kazdy z osobnego brancha) i czeka na wszystkie zanim
    // wyemituje swoj single text output. Walidacja R4 nie liczy go.
    for node in &def.nodes {
        let count = incoming_count.get(node.id.as_str()).copied().unwrap_or(0);
        if is_entry_node_type(&node.node_type) {
            if count > 0 {
                return Err(FlowValidationError::MultipleInputs {
                    node_id: node.id.clone(),
                    actual: count,
                });
            }
            continue;
        }
        if node.node_type == "combine" {
            continue;
        }
        // `text_extract` stoi na fan-inie z `document_router`: zbiera krawędzie z
        // portów `text` ORAZ `unknown`, ale router aktywuje DOKŁADNIE jeden port
        // (lustro combine), więc w runtime zawsze żyje co najwyżej jedna krawędź.
        // Zwolnienie z R4 pozwala kierować nieobsługiwany typ (`unknown`) w ten sam
        // węzeł, który dla tekstu dekoduje treść, a dla nieznanego binarnego rzuca
        // twardy błąd ingestu — zamiast cichego placeholdera w indeksie.
        if node.node_type == "text_extract" {
            continue;
        }
        // `output` ma 6 typed input portow (text/audio/image/video/embedding
        // /other) — kazdy branch flow moze emitowac inny typ jednoczesnie
        // (np. text z LLM + audio z TTS w streamingu). Wymaga zwolnienia z
        // 1-input-edge.
        if node.node_type == "output" {
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

    // R7 multi-branch guard (luzny dla typed-output node'a):
    //
    // 1. Per-node: max 1 wychodzący edge z `from_port="stream"` POZA
    //    `output` node — output ma 6 typed input portow i moze przyjac
    //    rownolegle stream tekstu (LLM→PII→output.text) plus stream
    //    audio (TTS_bridge→output.audio). Stara reguła zakazywała tej
    //    konfiguracji bo runtime executor fold'owal jeden chain;
    //    chain-merge wraca w nastepnym kroku ale topologia juz jest
    //    legalna w GUI.
    //
    // 2. Per-flow: dowolna liczba niezaleznych producerów stream'u DOZWOLONA
    //    pod warunkiem ze WSZYSTKIE konczacych sie chainów wpadaja do
    //    `output` node. Sprawdzamy w walk-chain ponizej.
    let mut stream_out_count: HashMap<&str, usize> = HashMap::new();
    let mut stream_in_count: HashMap<&str, usize> = HashMap::new();
    for edge in &stream_edges {
        *stream_out_count.entry(edge.from.as_str()).or_insert(0) += 1;
        *stream_in_count.entry(edge.to.as_str()).or_insert(0) += 1;
    }
    // Mapa node_id -> typ; potrzebna zeby wykluczyc output z per-node limitu
    // (wszystkie inne typy wciaz max 1 stream out).
    let node_type_by_id: HashMap<&str, &str> = def
        .nodes
        .iter()
        .map(|n| (n.id.as_str(), n.node_type.as_str()))
        .collect();
    for (node_id, count) in &stream_out_count {
        if *count > 1 {
            let nt = node_type_by_id.get(node_id).copied().unwrap_or("");
            // Tylko output moze miec wiele wychodzacych stream edges (i nawet
            // to nie ma sensu — output to terminal sink — ale defensywnie
            // zostawiamy dziure bo R7 sprawdza tez ze terminal.from_port=
            // stream prowadzi do output, co dla output samego siebie nie
            // ma jak skomponowac).
            if nt != "output" {
                return Err(FlowValidationError::MultipleStreamingBranches { count: *count });
            }
        }
    }

    // Walk chain dla KAŻDEGO niezaleznego producenta stream'u. Producent =
    // node z >=1 wychodzacym stream edge ale BEZ wchodzacego stream edge
    // (intermediate w chain'ie ma incoming + outgoing stream → caly chain
    // policzymy raz od jego producenta). Multi-producer pozwala na
    // rownolegly stream tekstu i audio do output (output ma 6 typed input
    // portow). Runtime executor jeszcze nie skleja N strumieni w jeden
    // wynik klienta — to wraca w follow-up; walidacja juz akceptuje
    // topologie.
    let producers: Vec<&str> = stream_out_count
        .keys()
        .copied()
        .filter(|node_id| !stream_in_count.contains_key(*node_id))
        .collect();
    // The exit node of an inline loop region is itself a valid stream producer:
    // the region is the contracted producer unit (its `llm` member is the real
    // token source). Such a node need not implement `StreamProducerAdapter` —
    // the executor drives the region's streaming runner. A region exit is the
    // source of that region's `loop_back` edge.
    let region_exit_ids: HashSet<&str> = def
        .edges
        .iter()
        .filter(|e| e.is_loop_back())
        .map(|e| e.from.as_str())
        .collect();

    for producer in producers {
        // §3.11 B — the head of a stream chain must be a registered
        // `StreamProducerAdapter` (LLM is one such producer). R7 no longer
        // assumes node_type=="llm" — any registered producer is accepted.
        let producer_node = nodes_by_id[producer];
        let is_region_exit = region_exit_ids.contains(producer);
        if !is_region_exit && !registry.is_stream_producer(&producer_node.node_type) {
            return Err(FlowValidationError::StreamProducerNotRegistered {
                node_id: producer_node.id.clone(),
                node_type: producer_node.node_type.clone(),
            });
        }
        // §3.12: the streaming dispatch path drives the producer via
        // `produce_stream(node, ...)` with the raw config — io-mapping never
        // overlays there. Forbid it so a savable flow cannot behave one way in
        // blocking dispatch and silently no-op the mapping in streaming.
        if producer_node.config.get("input_mapping").is_some() {
            return Err(FlowValidationError::StreamProducerWithIoMapping {
                node_id: producer_node.id.clone(),
                node_type: producer_node.node_type.clone(),
                mapping: "input_mapping",
            });
        }
        if producer_node.config.get("output_mapping").is_some() {
            return Err(FlowValidationError::StreamProducerWithIoMapping {
                node_id: producer_node.id.clone(),
                node_type: producer_node.node_type.clone(),
                mapping: "output_mapping",
            });
        }
        let mut seen: HashSet<&str> = HashSet::new();
        let mut current_id = producer;
        seen.insert(current_id);
        loop {
            let next_edge = def
                .edges
                .iter()
                .find(|e| e.from == current_id && e.from_port == "stream");
            let Some(edge) = next_edge else {
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
            if !seen.insert(edge.to.as_str()) {
                return Err(FlowValidationError::StreamingNotToOutput {
                    from_node: edge.from.clone(),
                    to_node: format!("{} (cycle)", edge.to),
                });
            }
            current_id = edge.to.as_str();
        }
    }

    // R10: output_mapping targets must be declared variables (§3.12). Absent
    // `variables` section = empty allow-set, so any output_mapping is rejected.
    let declared: HashSet<&str> = def.variables.iter().map(|v| v.name.as_str()).collect();
    for node in &def.nodes {
        validate_mapping_shape(node, "input_mapping")?;
        let Some(mapping) = node.config.get("output_mapping") else {
            continue;
        };
        let obj = mapping
            .as_object()
            .ok_or_else(|| FlowValidationError::MalformedMapping {
                node_id: node.id.clone(),
                mapping: "output_mapping",
                detail: "must be an object {variable: \"<CEL>\"}".to_string(),
            })?;
        for (variable, expr) in obj {
            if !expr.is_string() {
                return Err(FlowValidationError::MalformedMapping {
                    node_id: node.id.clone(),
                    mapping: "output_mapping",
                    detail: format!("value for '{variable}' must be a CEL string"),
                });
            }
            if !declared.contains(variable.as_str()) {
                return Err(FlowValidationError::UndeclaredVariableTarget {
                    node_id: node.id.clone(),
                    variable: variable.clone(),
                });
            }
        }
    }

    // R11: inline loop-region integrity.
    validate_loop_regions(def)?;

    Ok(())
}

/// R11 — inline loop-region integrity. For every distinct `FlowNode.region` id:
///   * exactly one `loop_back` edge, both endpoints in this region;
///   * the back edge's target (entry) and source (exit) are members;
///   * no forward (non-loop_back) edge crosses the region boundary except an
///     external edge INTO the entry and an external edge OUT of the exit;
///   * the entry's `loop_max_iterations` (if set) is within the hard cap.
///
/// Runs after R1 (endpoints exist), so node lookups here are infallible.
fn validate_loop_regions(def: &FlowDefinition) -> Result<(), FlowValidationError> {
    use std::collections::BTreeMap;

    // region id → member node ids.
    let mut members: BTreeMap<&str, HashSet<&str>> = BTreeMap::new();
    for node in &def.nodes {
        if let Some(region_id) = node.region.as_deref() {
            members.entry(region_id).or_default().insert(node.id.as_str());
        }
    }
    if members.is_empty() {
        return Ok(());
    }

    let region_of: HashMap<&str, &str> = def
        .nodes
        .iter()
        .filter_map(|n| n.region.as_deref().map(|r| (n.id.as_str(), r)))
        .collect();

    for (region_id, region_members) in &members {
        // Exactly one back edge touching this region.
        let back_edges: Vec<_> = def
            .edges
            .iter()
            .filter(|e| {
                e.is_loop_back()
                    && (region_members.contains(e.from.as_str())
                        || region_members.contains(e.to.as_str()))
            })
            .collect();
        if back_edges.len() != 1 {
            return Err(FlowValidationError::InvalidLoopRegion {
                region_id: region_id.to_string(),
                detail: format!(
                    "expected exactly one loop_back edge, found {}",
                    back_edges.len()
                ),
            });
        }
        let back = back_edges[0];
        if !region_members.contains(back.from.as_str())
            || !region_members.contains(back.to.as_str())
        {
            return Err(FlowValidationError::InvalidLoopRegion {
                region_id: region_id.to_string(),
                detail: "loop_back edge must connect two nodes of the same region".to_string(),
            });
        }
        let entry = back.to.as_str();
        let exit = back.from.as_str();

        // No forward edge crosses the region boundary except INTO entry / OUT of
        // exit. A forward edge with exactly one endpoint in the region is a
        // crossing; it is legal only when that endpoint is entry (incoming) or
        // exit (outgoing).
        for edge in def.edges.iter().filter(|e| !e.is_loop_back()) {
            let from_in = region_members.contains(edge.from.as_str());
            let to_in = region_members.contains(edge.to.as_str());
            if from_in == to_in {
                // Wholly inside or wholly outside. A wholly-inside edge must not
                // join two different regions' members.
                if from_in
                    && region_of.get(edge.from.as_str()) != region_of.get(edge.to.as_str())
                {
                    return Err(FlowValidationError::InvalidLoopRegion {
                        region_id: region_id.to_string(),
                        detail: "an internal edge connects nodes of different regions".to_string(),
                    });
                }
                continue;
            }
            if to_in && edge.to.as_str() != entry {
                return Err(FlowValidationError::InvalidLoopRegion {
                    region_id: region_id.to_string(),
                    detail: format!(
                        "external edge enters region at non-entry node '{}'",
                        edge.to
                    ),
                });
            }
            if from_in && edge.from.as_str() != exit {
                return Err(FlowValidationError::InvalidLoopRegion {
                    region_id: region_id.to_string(),
                    detail: format!(
                        "external edge leaves region at non-exit node '{}'",
                        edge.from
                    ),
                });
            }
        }

        // Iteration cap.
        if let Some(entry_node) = def.nodes.iter().find(|n| n.id == entry) {
            if let Some(max) = entry_node
                .config
                .get("loop_max_iterations")
                .and_then(|v| v.as_i64())
            {
                if max > crate::flow_engine::cache::LOOP_REGION_MAX_ITERATIONS_CAP as i64 {
                    return Err(FlowValidationError::InvalidLoopRegion {
                        region_id: region_id.to_string(),
                        detail: format!(
                            "loop_max_iterations {max} exceeds cap {}",
                            crate::flow_engine::cache::LOOP_REGION_MAX_ITERATIONS_CAP
                        ),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Shared shape check for io-mapping declarations: the key must hold an object
/// of `{name: "<CEL string>"}`. Reused for `input_mapping` (R10 does not gate
/// its keys — they target node config, not variables — but the shape must be
/// well-formed so the executor's overlay cannot misfire silently).
fn validate_mapping_shape(
    node: &crate::flow_engine::types::FlowNode,
    mapping: &'static str,
) -> Result<(), FlowValidationError> {
    let Some(value) = node.config.get(mapping) else {
        return Ok(());
    };
    let obj = value
        .as_object()
        .ok_or_else(|| FlowValidationError::MalformedMapping {
            node_id: node.id.clone(),
            mapping,
            detail: "must be an object {key: \"<CEL>\"}".to_string(),
        })?;
    for (key, expr) in obj {
        if !expr.is_string() {
            return Err(FlowValidationError::MalformedMapping {
                node_id: node.id.clone(),
                mapping,
                detail: format!("value for '{key}' must be a CEL string"),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::node_adapters::{
        ConditionNodeAdapter, LlmNodeAdapter, OnSubagentCompleteNodeAdapter, OutputNodeAdapter,
        TriggerNodeAdapter,
    };
    use std::sync::Arc;

    fn registry() -> AdapterRegistry {
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OnSubagentCompleteNodeAdapter::new()));
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
            r#"{"nodes":[{"id":"t","type":"trigger","config":{}},{"id":"o","type":"output","config":{}}],"edges":[{"from":"t","to":"o","from_port":"text","to_port":"text"}]}"#,
        );
        validate(&def, &registry()).unwrap();
    }

    #[test]
    fn rejects_no_entry() {
        let def = parse(r#"{"nodes":[{"id":"o","type":"output","config":{}}],"edges":[]}"#);
        let err = validate(&def, &registry()).unwrap_err();
        assert!(matches!(
            err,
            FlowValidationError::EntryNodeCount { actual: 0 }
        ));
    }

    #[test]
    fn rejects_two_triggers() {
        let def = parse(
            r#"{"nodes":[{"id":"t1","type":"trigger","config":{}},{"id":"t2","type":"trigger","config":{}}],"edges":[]}"#,
        );
        let err = validate(&def, &registry()).unwrap_err();
        assert!(matches!(
            err,
            FlowValidationError::EntryNodeCount { actual: 2 }
        ));
    }

    /// R5: `on_subagent_complete` is a valid sole entry (event-driven flow).
    #[test]
    fn ok_on_subagent_complete_as_sole_entry() {
        let def = parse(
            r#"{"nodes":[
                {"id":"e","type":"on_subagent_complete","config":{"agent_id":"a1"}},
                {"id":"o","type":"output","config":{}}
            ],"edges":[{"from":"e","to":"o","from_port":"text","to_port":"text"}]}"#,
        );
        validate(&def, &registry()).expect("event entry must validate as the one entry");
    }

    /// R5: a flow with TWO entries of mixed kind (trigger + on_subagent_complete)
    /// is rejected — entries are mutually exclusive.
    #[test]
    fn rejects_trigger_plus_event_entry() {
        let def = parse(
            r#"{"nodes":[
                {"id":"t","type":"trigger","config":{}},
                {"id":"e","type":"on_subagent_complete","config":{"agent_id":"a1"}},
                {"id":"o","type":"output","config":{}}
            ],"edges":[
                {"from":"t","to":"o","from_port":"text","to_port":"text"}
            ]}"#,
        );
        let err = validate(&def, &registry()).unwrap_err();
        assert!(matches!(
            err,
            FlowValidationError::EntryNodeCount { actual: 2 }
        ));
    }

    /// An `on_subagent_complete` entry is a source with NO input ports, so any
    /// inbound edge is structurally rejected (R3: the entry has no `to_port` to
    /// target) — the same guarantee that keeps `trigger` a source.
    #[test]
    fn rejects_event_entry_with_incoming_edge() {
        let def = parse(
            r#"{"nodes":[
                {"id":"e","type":"on_subagent_complete","config":{"agent_id":"a1"}},
                {"id":"c","type":"condition","config":{}}
            ],"edges":[
                {"from":"c","to":"e","from_port":"true","to_port":"text"}
            ]}"#,
        );
        let err = validate(&def, &registry()).unwrap_err();
        assert!(
            matches!(err, FlowValidationError::InvalidInputPort { .. }),
            "an entry node has no input ports, so an inbound edge must be rejected; got {err:?}"
        );
    }

    #[test]
    fn rejects_multi_input_edge() {
        // Output node jest zwolniony z R4 (multi-modal sink), wiec pivotem
        // testu staje sie LLM ktory MUSI miec dokladnie 1 incoming.
        let def = parse(
            r#"{
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"c","type":"condition","config":{}},
                    {"id":"l","type":"llm","config":{"model":"m"}},
                    {"id":"o","type":"output","config":{}}
                ],
                "edges":[
                    {"from":"t","to":"c","from_port":"text"},
                    {"from":"c","to":"l","from_port":"true"},
                    {"from":"c","to":"l","from_port":"false"},
                    {"from":"l","to":"o","to_port":"text"}
                ]
            }"#,
        );
        let err = validate(&def, &registry()).unwrap_err();
        assert!(matches!(err, FlowValidationError::MultipleInputs { .. }));
    }

    #[test]
    fn rejects_unknown_adapter() {
        let def = parse(
            r#"{"nodes":[{"id":"t","type":"trigger","config":{}},{"id":"x","type":"mystery","config":{}}],"edges":[{"from":"t","to":"x","from_port":"text"}]}"#,
        );
        let err = validate(&def, &registry()).unwrap_err();
        assert!(matches!(err, FlowValidationError::UnknownAdapter { .. }));
    }

    /// Streaming shape sanity — LLM bezpośrednio do `output(stream)`.
    /// Sprawdza R7 streaming end-shape.
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
                    {"from":"t","to":"l","from_port":"text"},
                    {"from":"l","to":"o","from_port":"stream","to_port":"text"}
                ]
            }"#,
        );
        validate(&def, &registry()).unwrap();
    }

    /// §3.12 — R7 rejects a stream producer carrying `input_mapping`: the
    /// streaming dispatch path drives the producer with the raw config and
    /// never overlays io-mapping, so allowing it would silently diverge from
    /// the blocking path on the same saved flow.
    #[test]
    fn rejects_stream_producer_with_input_mapping() {
        let def = parse(
            r#"{
                "variables":[{"name":"chosen","type":"text"}],
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"l","type":"llm","config":{"model":"m","input_mapping":{"model":"vars.chosen"}}},
                    {"id":"o","type":"output","config":{"mode":"stream"}}
                ],
                "edges":[
                    {"from":"t","to":"l","from_port":"text"},
                    {"from":"l","to":"o","from_port":"stream","to_port":"text"}
                ]
            }"#,
        );
        let err = validate(&def, &registry()).unwrap_err();
        assert!(
            matches!(
                err,
                FlowValidationError::StreamProducerWithIoMapping {
                    mapping: "input_mapping",
                    ..
                }
            ),
            "expected StreamProducerWithIoMapping(input_mapping), got {err:?}"
        );
    }

    /// §3.12 — same rejection for `output_mapping` on a stream producer (the
    /// finalizer never applies output_mapping to the producer either).
    #[test]
    fn rejects_stream_producer_with_output_mapping() {
        let def = parse(
            r#"{
                "variables":[{"name":"answer","type":"text"}],
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"l","type":"llm","config":{"model":"m","output_mapping":{"answer":"payload"}}},
                    {"id":"o","type":"output","config":{"mode":"stream"}}
                ],
                "edges":[
                    {"from":"t","to":"l","from_port":"text"},
                    {"from":"l","to":"o","from_port":"stream","to_port":"text"}
                ]
            }"#,
        );
        let err = validate(&def, &registry()).unwrap_err();
        assert!(
            matches!(
                err,
                FlowValidationError::StreamProducerWithIoMapping {
                    mapping: "output_mapping",
                    ..
                }
            ),
            "expected StreamProducerWithIoMapping(output_mapping), got {err:?}"
        );
    }

    /// A non-producer node carrying io-mapping in a streaming flow is still
    /// fine — only the producer is restricted. The pre-producer `llm`-less
    /// path applies io-mapping normally.
    #[test]
    fn accepts_io_mapping_on_pre_producer_node_in_streaming_flow() {
        let def = parse(
            r#"{
                "variables":[{"name":"seen","type":"text"}],
                "nodes":[
                    {"id":"t","type":"trigger","config":{"output_mapping":{"seen":"payload"}}},
                    {"id":"l","type":"llm","config":{"model":"m"}},
                    {"id":"o","type":"output","config":{"mode":"stream"}}
                ],
                "edges":[
                    {"from":"t","to":"l","from_port":"text"},
                    {"from":"l","to":"o","from_port":"stream","to_port":"text"}
                ]
            }"#,
        );
        validate(&def, &registry()).unwrap();
    }

    /// §3.11 B — R7 accepts a non-LLM `StreamProducerAdapter` as the head of a
    /// stream chain terminating at output(stream).
    #[test]
    fn ok_streaming_shape_non_llm_producer() {
        use crate::flow_engine::node_adapter::test_support::TestStreamProducer;
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        r.register_stream_producer(Arc::new(TestStreamProducer::new("test_producer")));
        let def = parse(
            r#"{
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"p","type":"test_producer","config":{}},
                    {"id":"o","type":"output","config":{"mode":"stream"}}
                ],
                "edges":[
                    {"from":"t","to":"p","from_port":"text","to_port":"in"},
                    {"from":"p","to":"o","from_port":"stream","to_port":"text"}
                ]
            }"#,
        );
        validate(&def, &r).unwrap();
    }

    /// §3.11 B — R7 rejects a stream edge whose producer node has a `stream`
    /// output port (so R3 passes) but is NOT registered in the stream-producer
    /// slot. Here the test adapter is registered via plain `register` instead
    /// of `register_stream_producer`.
    #[test]
    fn rejects_stream_from_non_producer() {
        use crate::flow_engine::node_adapter::test_support::TestStreamProducer;
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        // Plain register: the adapter exposes a `stream` port (R3 ok) but is
        // absent from the producer slot (R7 must reject).
        r.register(Arc::new(TestStreamProducer::new("fake_stream")));
        let def = parse(
            r#"{
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"p","type":"fake_stream","config":{}},
                    {"id":"o","type":"output","config":{"mode":"stream"}}
                ],
                "edges":[
                    {"from":"t","to":"p","from_port":"text","to_port":"in"},
                    {"from":"p","to":"o","from_port":"stream","to_port":"text"}
                ]
            }"#,
        );
        let err = validate(&def, &r).unwrap_err();
        assert!(
            matches!(err, FlowValidationError::StreamProducerNotRegistered { .. }),
            "expected StreamProducerNotRegistered, got {err:?}"
        );
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
                    {"from":"t","to":"l","from_port":"text"},
                    {"from":"l","to":"c","from_port":"stream"}
                ]
            }"#,
        );
        let err = validate(&def, &registry()).unwrap_err();
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
                    {"from":"t","to":"l","from_port":"text"},
                    {"from":"l","to":"o","from_port":"stream","to_port":"text"}
                ]
            }"#,
        );
        let err = validate(&def, &registry()).unwrap_err();
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
        r.register(Arc::new(
            crate::flow_engine::node_adapters::SttNodeAdapter::new(),
        ));
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
                    {"from":"t","to":"l","from_port":"text"},
                    {"from":"l","to":"s"},
                    {"from":"s","to":"o","to_port":"text"}
                ]
            }"#,
        );
        let err = validate(&def, &r).unwrap_err();
        assert!(
            matches!(err, FlowValidationError::EdgePortTypesMismatch { .. }),
            "got {:?}",
            err
        );
    }

    #[test]
    fn r8_accepts_explicit_data_type_when_matching() {
        // pii_filter (Text → Text) z explicit edge.data_type = "text" przechodzi.
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        r.register(Arc::new(
            crate::flow_engine::node_adapters::PiiFilterNodeAdapter::new(),
        ));

        let def = parse(
            r#"{
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"p","type":"pii_filter","config":{}},
                    {"id":"o","type":"output","config":{}}
                ],
                "edges":[
                    {"from":"t","to":"p","from_port":"text","data_type":"text"},
                    {"from":"p","to":"o","data_type":"text","to_port":"text"}
                ]
            }"#,
        );
        validate(&def, &r).unwrap();
    }

    #[test]
    fn r8_rejects_explicit_edge_type_mismatching_producer() {
        // pii_filter produkuje Text, ale edge deklaruje Audio.
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        r.register(Arc::new(
            crate::flow_engine::node_adapters::PiiFilterNodeAdapter::new(),
        ));

        let def = parse(
            r#"{
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"p","type":"pii_filter","config":{}},
                    {"id":"o","type":"output","config":{}}
                ],
                "edges":[
                    {"from":"t","to":"p","from_port":"text"},
                    {"from":"p","to":"o","data_type":"audio","to_port":"text"}
                ]
            }"#,
        );
        let err = validate(&def, &r).unwrap_err();
        assert!(
            matches!(
                err,
                FlowValidationError::EdgeTypeMismatch { side: "from", .. }
            ),
            "got {:?}",
            err
        );
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
                    {"from":"t","to":"l","from_port":"text"},
                    {"from":"l","to":"p","from_port":"stream"},
                    {"from":"p","to":"o","from_port":"stream","to_port":"text"}
                ]
            }"#,
        );
        let res = validate(&def, &r);
        assert!(
            res.is_ok(),
            "expected chain to pass R7, got: {:?}",
            res.err()
        );
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
                    {"from":"t","to":"l","from_port":"text"},
                    {"from":"l","to":"o1","from_port":"stream","to_port":"text"},
                    {"from":"l","to":"o2","from_port":"stream","to_port":"text"}
                ]
            }"#,
        );
        let err = validate(&def, &r).unwrap_err();
        assert!(matches!(
            err,
            FlowValidationError::MultipleStreamingBranches { .. }
        ));
    }

    /// R7 multi-producer: dwa niezalezne LLM kazdy ze swoim stream chain'em
    /// jest TERAZ DOZWOLONY (output ma 6 typed input portow, moze
    /// jednoczesnie przyjac stream tekstu i stream audio). Walidacja
    /// akceptuje, runtime executor jeszcze nie skleja N strumieni — to
    /// follow-up. Test pilnuje ze topologia jest legalna od strony
    /// kompilacji.
    #[test]
    fn accepts_multiple_independent_stream_producers_into_typed_output() {
        let mut r = AdapterRegistry::new();
        r.register(Arc::new(TriggerNodeAdapter::new()));
        r.register(Arc::new(OutputNodeAdapter::new()));
        r.register(Arc::new(ConditionNodeAdapter::new()));
        r.register(Arc::new(
            crate::flow_engine::node_adapters::PiiFilterNodeAdapter::new(),
        ));
        r.register_llm(Arc::new(LlmNodeAdapter::new()));

        let def = parse(
            r#"{
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"c","type":"condition","config":{}},
                    {"id":"l1","type":"llm","config":{}},
                    {"id":"l2","type":"llm","config":{}},
                    {"id":"p1","type":"pii_filter","config":{}},
                    {"id":"p2","type":"pii_filter","config":{}},
                    {"id":"o1","type":"output","config":{"mode":"stream"}},
                    {"id":"o2","type":"output","config":{"mode":"stream"}}
                ],
                "edges":[
                    {"from":"t","to":"c","from_port":"text"},
                    {"from":"c","to":"l1","from_port":"true"},
                    {"from":"c","to":"l2","from_port":"false"},
                    {"from":"l1","to":"p1","from_port":"stream"},
                    {"from":"p1","to":"o1","from_port":"stream","to_port":"text"},
                    {"from":"l2","to":"p2","from_port":"stream"},
                    {"from":"p2","to":"o2","from_port":"stream","to_port":"text"}
                ]
            }"#,
        );
        validate(&def, &r).expect("multi-producer streaming should validate");
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
                    {"from":"t","to":"l","from_port":"text"},
                    {"from":"l","to":"p","from_port":"stream"}
                ]
            }"#,
        );
        let err = validate(&def, &r).unwrap_err();
        assert!(matches!(
            err,
            FlowValidationError::StreamingNotToOutput { .. }
        ));
    }

    #[test]
    fn r10_rejects_output_mapping_to_undeclared_variable() {
        let def = parse(
            r#"{
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"o","type":"output","config":{"output_mapping":{"answer":"payload"}}}
                ],
                "edges":[
                    {"from":"t","to":"o","from_port":"text","to_port":"text"}
                ]
            }"#,
        );
        let err = validate(&def, &registry()).unwrap_err();
        assert!(
            matches!(
                err,
                FlowValidationError::UndeclaredVariableTarget { ref variable, .. } if variable == "answer"
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn r10_accepts_output_mapping_to_declared_variable() {
        let def = parse(
            r#"{
                "variables":[{"name":"answer","type":"text"}],
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"o","type":"output","config":{"output_mapping":{"answer":"payload"}}}
                ],
                "edges":[
                    {"from":"t","to":"o","from_port":"text","to_port":"text"}
                ]
            }"#,
        );
        validate(&def, &registry()).expect("declared variable target must pass R10");
    }

    #[test]
    fn r10_absent_variables_section_rejects_any_output_mapping() {
        // No `variables` key at all = empty allow-set.
        let def = parse(
            r#"{
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"o","type":"output","config":{"output_mapping":{"x":"1"}}}
                ],
                "edges":[
                    {"from":"t","to":"o","from_port":"text","to_port":"text"}
                ]
            }"#,
        );
        let err = validate(&def, &registry()).unwrap_err();
        assert!(matches!(
            err,
            FlowValidationError::UndeclaredVariableTarget { .. }
        ));
    }

    #[test]
    fn r10_input_mapping_keys_are_not_gated_by_declarations() {
        // input_mapping targets node config, not variables — must pass even
        // without a variables section, as long as it is well-formed.
        let def = parse(
            r#"{
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"l","type":"llm","config":{"input_mapping":{"model":"vars.m"}}},
                    {"id":"o","type":"output","config":{}}
                ],
                "edges":[
                    {"from":"t","to":"l","from_port":"text"},
                    {"from":"l","to":"o","to_port":"text"}
                ]
            }"#,
        );
        validate(&def, &registry()).expect("input_mapping must not be gated by R10");
    }

    #[test]
    fn r10_rejects_malformed_output_mapping() {
        let def = parse(
            r#"{
                "variables":[{"name":"x","type":"any"}],
                "nodes":[
                    {"id":"t","type":"trigger","config":{}},
                    {"id":"o","type":"output","config":{"output_mapping":{"x":42}}}
                ],
                "edges":[
                    {"from":"t","to":"o","from_port":"text","to_port":"text"}
                ]
            }"#,
        );
        let err = validate(&def, &registry()).unwrap_err();
        assert!(matches!(err, FlowValidationError::MalformedMapping { .. }));
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
                    {"from":"t","to":"o","from_port":"true","to_port":"text"}
                ]
            }"#,
        );
        let err = validate(&def, &registry()).unwrap_err();
        assert!(matches!(
            err,
            FlowValidationError::ConditionEdgeFromNonCondition { .. }
        ));
    }

    /// §3.11 B — R7 accepts the three harness stream producers (subflow / loop /
    /// agent), proving the harness streaming topology validates: Agent Run's
    /// `loop` → output(stream), and Harness's `subflow` → output(stream). The
    /// full registry registers all three as stream producers; a flow wiring
    /// their `stream` port must validate AND resolve as the stream producer.
    #[test]
    fn r7_accepts_harness_stream_producers() {
        use crate::flow_engine::cache::CompiledFlow;
        use crate::flow_engine::dispatcher::build_registry_for_test;

        let reg = build_registry_for_test();
        // (node_type, config-key for the body/flow/agent id) — validation does
        // not dereference these ids, only checks the streaming topology.
        for (producer, cfg) in [
            ("subflow", r#"{"flow_id":"any-id"}"#),
            ("loop", r#"{"body_flow_id":"any-id"}"#),
            ("agent", r#"{"agent_id":"any-id"}"#),
        ] {
            let flow_json = format!(
                r#"{{
                    "nodes":[
                        {{"id":"t","type":"trigger","config":{{}}}},
                        {{"id":"p","type":"{producer}","config":{cfg}}},
                        {{"id":"o","type":"output","config":{{"mode":"stream"}}}}
                    ],
                    "edges":[
                        {{"from":"t","to":"p","from_port":"text","to_port":"in"}},
                        {{"from":"p","to":"o","from_port":"stream","to_port":"text"}}
                    ]
                }}"#
            );
            let def = parse(&flow_json);
            validate(&def, &reg)
                .unwrap_or_else(|e| panic!("R7 rejected {producer} stream producer: {e:?}"));
            // And the compiler resolves the block as the flow's stream producer.
            let cf = CompiledFlow::from_json("0", &flow_json, &reg)
                .unwrap_or_else(|e| panic!("compile {producer}: {e:?}"));
            assert!(cf.is_streaming, "{producer} flow must be streaming");
            assert!(
                cf.stream_producer_run_idx(&reg).is_some(),
                "{producer} must resolve as the stream producer"
            );
        }
    }
}
