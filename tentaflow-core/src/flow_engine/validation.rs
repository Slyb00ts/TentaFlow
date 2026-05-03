// =============================================================================
// Plik: flow_engine/validation.rs
// Opis: Walidacja semantyczna FlowDefinition przed zapisem w DB — sprawdza
//       ze porty na krawedziach pasuja do metadanych adapterow. Daje UI
//       natychmiastowy feedback zamiast pozniejszego bledu runtime.
// =============================================================================

use std::fmt;

use crate::flow_engine::adapters::AdapterRegistry;
use crate::flow_engine::types::FlowDefinition;

/// Blad walidacji struktury flow.
#[derive(Debug, Clone)]
pub enum FlowValidationError {
    /// Krawedz odwoluje sie do nieistniejacego wezla (po `id`).
    UnknownNode {
        edge_endpoint: String,
        node_id: String,
    },
    /// Typ wezla nie jest zarejestrowany w AdapterRegistry.
    UnknownAdapter { node_id: String, node_type: String },
    /// Edge.from_port nie istnieje na liscie portow wyjsciowych adaptera.
    InvalidOutputPort {
        node_id: String,
        node_type: String,
        port: String,
        available: Vec<&'static str>,
    },
    /// Edge.to_port nie istnieje na liscie portow wejsciowych adaptera.
    InvalidInputPort {
        node_id: String,
        node_type: String,
        port: String,
        available: Vec<&'static str>,
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
                "edge {} references unknown node id '{}'",
                edge_endpoint, node_id
            ),
            Self::UnknownAdapter { node_id, node_type } => write!(
                f,
                "node '{}' uses unregistered adapter type '{}'",
                node_id, node_type
            ),
            Self::InvalidOutputPort {
                node_id,
                node_type,
                port,
                available,
            } => write!(
                f,
                "node '{}' (type '{}') has no output port '{}', available: {:?}",
                node_id, node_type, port, available
            ),
            Self::InvalidInputPort {
                node_id,
                node_type,
                port,
                available,
            } => write!(
                f,
                "node '{}' (type '{}') has no input port '{}', available: {:?}",
                node_id, node_type, port, available
            ),
        }
    }
}

impl std::error::Error for FlowValidationError {}

/// Waliduje semantyczna poprawnosc flow — kazdy edge musi odwolywac sie do
/// istniejacych nodes, typ kazdego node'a musi byc zarejestrowany, a porty
/// (from_port/to_port) musza byc wsrod supported_{output,input}_ports adaptera.
pub fn validate_flow(
    flow: &FlowDefinition,
    registry: &AdapterRegistry,
) -> Result<(), FlowValidationError> {
    for edge in &flow.edges {
        let from_node = flow
            .nodes
            .iter()
            .find(|n| n.id == edge.from)
            .ok_or_else(|| FlowValidationError::UnknownNode {
                edge_endpoint: "from".to_string(),
                node_id: edge.from.clone(),
            })?;

        let from_adapter = registry.get(&from_node.node_type).ok_or_else(|| {
            FlowValidationError::UnknownAdapter {
                node_id: from_node.id.clone(),
                node_type: from_node.node_type.clone(),
            }
        })?;

        let out_ports = from_adapter.supported_output_ports();
        if !out_ports.contains(&edge.from_port.as_str()) {
            return Err(FlowValidationError::InvalidOutputPort {
                node_id: from_node.id.clone(),
                node_type: from_node.node_type.clone(),
                port: edge.from_port.clone(),
                available: out_ports.to_vec(),
            });
        }

        let to_node = flow.nodes.iter().find(|n| n.id == edge.to).ok_or_else(|| {
            FlowValidationError::UnknownNode {
                edge_endpoint: "to".to_string(),
                node_id: edge.to.clone(),
            }
        })?;

        let to_adapter = registry.get(&to_node.node_type).ok_or_else(|| {
            FlowValidationError::UnknownAdapter {
                node_id: to_node.id.clone(),
                node_type: to_node.node_type.clone(),
            }
        })?;

        let in_ports = to_adapter.supported_input_ports();
        if !in_ports.contains(&edge.to_port.as_str()) {
            return Err(FlowValidationError::InvalidInputPort {
                node_id: to_node.id.clone(),
                node_type: to_node.node_type.clone(),
                port: edge.to_port.clone(),
                available: in_ports.to_vec(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow_engine::adapters::NodeAdapter;
    use crate::flow_engine::types::{FlowEdge, FlowNode};
    use anyhow::Result;
    use serde_json::Value;

    struct FullOnlyAdapter;
    impl NodeAdapter for FullOnlyAdapter {
        fn execute(
            &self,
            _node_config: &Value,
            _ctx: &mut crate::flow_engine::types::FlowContext,
        ) -> impl std::future::Future<Output = Result<Value>> + Send {
            async { Ok(Value::Null) }
        }
        fn node_type(&self) -> &'static str {
            "full_only"
        }
    }

    struct StreamAdapter;
    impl NodeAdapter for StreamAdapter {
        fn execute(
            &self,
            _node_config: &Value,
            _ctx: &mut crate::flow_engine::types::FlowContext,
        ) -> impl std::future::Future<Output = Result<Value>> + Send {
            async { Ok(Value::Null) }
        }
        fn node_type(&self) -> &'static str {
            "streamy"
        }
        fn supported_output_ports(&self) -> &'static [&'static str] {
            &["stream", "full"]
        }
    }

    fn sample_registry() -> AdapterRegistry {
        let mut r = AdapterRegistry::new();
        r.register(FullOnlyAdapter);
        r.register(StreamAdapter);
        r
    }

    fn node(id: &str, ty: &str) -> FlowNode {
        FlowNode {
            id: id.to_string(),
            node_type: ty.to_string(),
            config: Value::Null,
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
        }
    }

    #[test]
    fn valid_default_ports() {
        let flow = FlowDefinition {
            nodes: vec![node("a", "streamy"), node("b", "full_only")],
            edges: vec![edge("a", "b", "full", "in")],
        };
        assert!(validate_flow(&flow, &sample_registry()).is_ok());
    }

    #[test]
    fn valid_stream_port() {
        let flow = FlowDefinition {
            nodes: vec![node("a", "streamy"), node("b", "full_only")],
            edges: vec![edge("a", "b", "stream", "in")],
        };
        assert!(validate_flow(&flow, &sample_registry()).is_ok());
    }

    #[test]
    fn rejects_stream_from_full_only() {
        let flow = FlowDefinition {
            nodes: vec![node("a", "full_only"), node("b", "full_only")],
            edges: vec![edge("a", "b", "stream", "in")],
        };
        let err = validate_flow(&flow, &sample_registry()).unwrap_err();
        match err {
            FlowValidationError::InvalidOutputPort { port, node_id, .. } => {
                assert_eq!(port, "stream");
                assert_eq!(node_id, "a");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_to_port() {
        let flow = FlowDefinition {
            nodes: vec![node("a", "streamy"), node("b", "full_only")],
            edges: vec![edge("a", "b", "full", "ghost")],
        };
        let err = validate_flow(&flow, &sample_registry()).unwrap_err();
        assert!(matches!(err, FlowValidationError::InvalidInputPort { .. }));
    }

    #[test]
    fn rejects_unknown_node() {
        let flow = FlowDefinition {
            nodes: vec![node("a", "streamy")],
            edges: vec![edge("a", "missing", "full", "in")],
        };
        let err = validate_flow(&flow, &sample_registry()).unwrap_err();
        assert!(matches!(err, FlowValidationError::UnknownNode { .. }));
    }

    #[test]
    fn validate_rejects_stream_port_on_tts_node() {
        use crate::config::RouterConfig;
        use crate::flow_engine::adapters::tts::TtsNodeAdapter;
        use crate::services::runtime::quic_handle::ServiceManager;
        use std::sync::Arc;

        let config = Arc::new(RouterConfig::default());
        let service_manager = Arc::new(
            ServiceManager::new(config.clone(), None).expect("ServiceManager with empty config"),
        );

        let mut registry = AdapterRegistry::new();
        registry.register(FullOnlyAdapter);
        registry.register(TtsNodeAdapter::new(service_manager, config));

        let flow = FlowDefinition {
            nodes: vec![node("t", "tts"), node("sink", "full_only")],
            edges: vec![edge("t", "sink", "stream", "in")],
        };
        let err = validate_flow(&flow, &registry).unwrap_err();
        match err {
            FlowValidationError::InvalidOutputPort {
                node_id,
                node_type,
                port,
                available,
            } => {
                assert_eq!(node_id, "t");
                assert_eq!(node_type, "tts");
                assert_eq!(port, "stream");
                assert_eq!(available, vec!["full"]);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn rejects_unknown_adapter() {
        let flow = FlowDefinition {
            nodes: vec![node("a", "nope"), node("b", "full_only")],
            edges: vec![edge("a", "b", "full", "in")],
        };
        let err = validate_flow(&flow, &sample_registry()).unwrap_err();
        assert!(matches!(err, FlowValidationError::UnknownAdapter { .. }));
    }
}
