// ===== File: processes/blocks.rs — Flow Builder blocks a BPMN Service task may run on their own =====

use anyhow::{bail, ensure, Context, Result};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::flow_engine::envelope::FlowValue;
use crate::flow_engine::types::{FlowDataType, FlowDefinition, FlowEdge, FlowNode};

/// Largest serialized block configuration; it is part of the model and of the pin.
const MAX_CONFIG_BYTES: usize = 64 * 1024;

/// Node ids of the synthesized graph. Fixed, so the pin is a pure function of
/// the block type and its configuration.
const TRIGGER_NODE_ID: &str = "t1";
const BLOCK_NODE_ID: &str = "b1";
const OUTPUT_NODE_ID: &str = "o1";

#[derive(Clone, Copy)]
enum ConfigValue {
    Text { max_len: usize },
    Number { min: f64, max: f64 },
    Count { min: u64, max: u64 },
    TextList { max_items: usize, max_len: usize },
}

struct ConfigKey {
    name: &'static str,
    value: ConfigValue,
    required: bool,
}

/// A block that may be a Service task, with the ports its one-block graph is
/// wired through and the configuration keys it accepts.
pub struct AllowedBlock {
    pub node_type: &'static str,
    /// Trigger output port feeding the block's `in` port.
    entry_port: &'static str,
    /// Output input port that takes the block's `full` port.
    exit_port: &'static str,
    keys: &'static [ConfigKey],
}

const MODEL: ConfigKey = ConfigKey {
    name: "model",
    value: ConfigValue::Text { max_len: 256 },
    required: true,
};

/// The server-side allow-list. A block is here only when its whole contract
/// fits a process: it takes text from the activity input and gives back JSON
/// the activity result can carry.
///
/// Left out on purpose: `stt` and `tts` (audio comes from, and goes to, the
/// blob store, which a process cannot read), `reranker` (JSON ports that the
/// trigger and output nodes of a one-block graph do not have), everything that
/// runs other flows, agents, commands or CLIs (`subflow`, `map`, `loop`,
/// `agent*`, `spawn`, `await_subagents`, `exec_command`, `delegate_cli`,
/// `workspace_context`), and everything that talks to a person or the bus
/// (`ask_user`, `bus_*`).
const ALLOWED: &[AllowedBlock] = &[
    AllowedBlock {
        node_type: "llm",
        entry_port: "text",
        exit_port: "text",
        keys: &[
            MODEL,
            ConfigKey {
                name: "system_prompt",
                value: ConfigValue::Text { max_len: 32 * 1024 },
                required: false,
            },
            ConfigKey {
                name: "temperature",
                value: ConfigValue::Number { min: 0.0, max: 2.0 },
                required: false,
            },
            ConfigKey {
                name: "top_p",
                value: ConfigValue::Number { min: 0.0, max: 1.0 },
                required: false,
            },
            ConfigKey {
                name: "frequency_penalty",
                value: ConfigValue::Number {
                    min: -2.0,
                    max: 2.0,
                },
                required: false,
            },
            ConfigKey {
                name: "presence_penalty",
                value: ConfigValue::Number {
                    min: -2.0,
                    max: 2.0,
                },
                required: false,
            },
            ConfigKey {
                name: "max_tokens",
                value: ConfigValue::Count {
                    min: 1,
                    max: 1_000_000,
                },
                required: false,
            },
            ConfigKey {
                name: "reasoning_effort",
                value: ConfigValue::Text { max_len: 32 },
                required: false,
            },
            ConfigKey {
                name: "stop",
                value: ConfigValue::TextList {
                    max_items: 8,
                    max_len: 256,
                },
                required: false,
            },
            ConfigKey {
                name: "max_request_attempts",
                value: ConfigValue::Count { min: 1, max: 5 },
                required: false,
            },
        ],
    },
    AllowedBlock {
        node_type: "embeddings",
        entry_port: "text",
        exit_port: "embedding",
        keys: &[MODEL],
    },
    AllowedBlock {
        node_type: "pii_filter",
        entry_port: "text",
        exit_port: "text",
        keys: &[],
    },
];

pub fn allowed_block(node_type: &str) -> Option<&'static AllowedBlock> {
    ALLOWED.iter().find(|block| block.node_type == node_type)
}

/// Why a process may not run a block right now. `Unavailable` is "cannot tell":
/// a failed read is never a verdict on the initiator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BlockAccessError {
    #[error("process block is not available")]
    NotAllowed,
    #[error("process initiator is inactive or outside the organization")]
    Initiator,
    #[error("process block access could not be determined")]
    Unavailable,
}

/// Checks a block type and its configuration against the allow-list: known
/// keys only, each of the right type and within range, required keys present.
pub fn validate_block(node_type: &str, config: &Value) -> Result<&'static AllowedBlock> {
    let block = allowed_block(node_type)
        .with_context(|| format!("block {node_type} cannot be run as a service task"))?;
    let object = config
        .as_object()
        .context("block configuration must be an object")?;
    ensure!(
        serde_json::to_vec(config)?.len() <= MAX_CONFIG_BYTES,
        "block configuration exceeds 64 KiB"
    );
    for key in object.keys() {
        ensure!(
            block.keys.iter().any(|allowed| allowed.name == key),
            "block {node_type} does not accept the setting {key}"
        );
    }
    for key in block.keys {
        match object.get(key.name) {
            None if key.required => bail!("block {node_type} requires the setting {}", key.name),
            None => {}
            Some(value) => validate_value(node_type, key, value)?,
        }
    }
    Ok(block)
}

fn validate_value(node_type: &str, key: &ConfigKey, value: &Value) -> Result<()> {
    let name = key.name;
    let valid = match key.value {
        ConfigValue::Text { max_len } => value.as_str().is_some_and(|text| {
            text.len() <= max_len && (!key.required || !text.trim().is_empty())
        }),
        ConfigValue::Number { min, max } => value
            .as_f64()
            .is_some_and(|number| number.is_finite() && (min..=max).contains(&number)),
        ConfigValue::Count { min, max } => value
            .as_u64()
            .is_some_and(|count| (min..=max).contains(&count)),
        ConfigValue::TextList { max_items, max_len } => value.as_array().is_some_and(|items| {
            items.len() <= max_items
                && items
                    .iter()
                    .all(|item| item.as_str().is_some_and(|text| text.len() <= max_len))
        }),
    };
    ensure!(valid, "block {node_type} setting {name} is invalid");
    Ok(())
}

fn graph_node(id: &str, node_type: &str, config: Value) -> FlowNode {
    FlowNode {
        id: id.to_owned(),
        node_type: node_type.to_owned(),
        config,
        position: None,
        label: None,
        region: None,
    }
}

fn graph_edge(from: &str, to: &str, from_port: &str, to_port: &str) -> FlowEdge {
    FlowEdge {
        id: None,
        from: from.to_owned(),
        to: to.to_owned(),
        label: None,
        condition: None,
        from_port: from_port.to_owned(),
        to_port: to_port.to_owned(),
        data_type: FlowDataType::Any,
        kind: None,
    }
}

fn block_graph(node_type: &str, config: Value, block: &AllowedBlock) -> FlowDefinition {
    FlowDefinition {
        nodes: vec![
            graph_node(TRIGGER_NODE_ID, "trigger", Value::Null),
            graph_node(BLOCK_NODE_ID, node_type, config),
            graph_node(OUTPUT_NODE_ID, "output", Value::Null),
        ],
        edges: vec![
            graph_edge(TRIGGER_NODE_ID, BLOCK_NODE_ID, block.entry_port, "in"),
            graph_edge(BLOCK_NODE_ID, OUTPUT_NODE_ID, "full", block.exit_port),
        ],
        variables: Vec::new(),
    }
}

/// Canonical JSON of the one-block graph (`trigger -> block -> output`) and its
/// SHA-256. Keys are sorted, so the same type and configuration always give the
/// same bytes; this is what publication pins.
pub fn block_pin(node_type: &str, config: &Value) -> Result<(String, String)> {
    let block = validate_block(node_type, config)?;
    let definition = block_graph(node_type, config.clone(), block);
    let canonical = super::repository::canonical_json_value(&serde_json::to_value(&definition)?);
    let graph_json = serde_json::to_string(&canonical)?;
    let sha256 = hex::encode(Sha256::digest(graph_json.as_bytes()));
    Ok((graph_json, sha256))
}

/// The block a pinned graph runs. The graph must be exactly what [`block_pin`]
/// builds for its block, so a hand-made or altered pin cannot reach the
/// executor through the block path.
pub fn pinned_block_type(graph_json: &str) -> Result<String> {
    let definition: FlowDefinition = serde_json::from_str(graph_json)?;
    let block = definition
        .nodes
        .iter()
        .find(|node| node.id == BLOCK_NODE_ID)
        .context("pinned block graph has no block node")?;
    let config = match &block.config {
        Value::Null => Value::Object(Map::new()),
        other => other.clone(),
    };
    let (expected, _) = block_pin(&block.node_type, &config)?;
    ensure!(
        graph_json == expected,
        "pinned block graph differs from the one built for its block"
    );
    Ok(block.node_type.clone())
}

/// The payload a text block takes: the string the `payload` mapping produced.
/// Anything else cannot be a prompt and is refused before the block runs.
pub fn block_payload(input: &Value) -> Result<FlowValue> {
    match input.get("payload") {
        Some(Value::String(text)) => Ok(FlowValue::Text(text.clone())),
        _ => bail!("a block service task needs its payload mapping to produce a string"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn only_listed_blocks_with_known_settings_pass() {
        assert!(validate_block("llm", &json!({"model": "m", "max_tokens": 10})).is_ok());
        assert!(validate_block("embeddings", &json!({"model": "e"})).is_ok());
        assert!(validate_block("pii_filter", &json!({})).is_ok());
        for refused in [
            "agent",
            "agent_router",
            "spawn",
            "await_subagents",
            "subflow",
            "map",
            "loop",
            "exec_command",
            "delegate_cli",
            "workspace_context",
            "bus_publish",
            "bus_consume",
            "ask_user",
            "stt",
            "tts",
            "reranker",
            "output",
            "trigger",
            "addon.notes.search",
        ] {
            assert!(
                validate_block(refused, &json!({"model": "m"})).is_err(),
                "{refused}"
            );
        }
        for (config, why) in [
            (json!({}), "model is required"),
            (json!({"model": ""}), "blank model"),
            (json!({"model": "m", "tools": []}), "unknown setting"),
            (
                json!({"model": "m", "audio_output": {"voice": "x"}}),
                "audio output is not offered",
            ),
            (
                json!({"model": "m", "temperature": 3}),
                "temperature out of range",
            ),
            (
                json!({"model": "m", "max_tokens": 0}),
                "max_tokens below range",
            ),
            (json!({"model": "m", "stop": [1]}), "stop item not text"),
            (json!({"model": 5}), "model not text"),
            (json!([]), "config not an object"),
        ] {
            assert!(validate_block("llm", &config).is_err(), "{why}");
        }
        assert!(validate_block("pii_filter", &json!({"model": "m"})).is_err());
    }

    #[test]
    fn the_pin_is_a_pure_function_of_type_and_configuration() {
        let first = block_pin("llm", &json!({"model": "m", "max_tokens": 5})).unwrap();
        let reordered = block_pin("llm", &json!({"max_tokens": 5, "model": "m"})).unwrap();
        assert_eq!(first, reordered);
        assert_ne!(
            first.1,
            block_pin("llm", &json!({"model": "other"})).unwrap().1
        );
        assert_eq!(first.1, hex::encode(Sha256::digest(first.0.as_bytes())));
        assert_eq!(pinned_block_type(&first.0).unwrap(), "llm");
    }

    #[test]
    fn an_altered_pin_is_not_a_block_pin() {
        let (graph, _) = block_pin("pii_filter", &json!({})).unwrap();
        let mut definition: Value = serde_json::from_str(&graph).unwrap();
        definition["nodes"][1]["type"] = json!("exec_command");
        assert!(pinned_block_type(&definition.to_string()).is_err());
        let mut extra: Value = serde_json::from_str(&graph).unwrap();
        extra["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id": "x", "type": "output"}));
        assert!(pinned_block_type(&extra.to_string()).is_err());
        // Equal content in another key order is not the canonical text.
        let reformatted =
            serde_json::to_string_pretty(&serde_json::from_str::<Value>(&graph).unwrap()).unwrap();
        assert!(pinned_block_type(&reformatted).is_err());
    }

    #[test]
    fn payload_must_be_a_string() {
        assert_eq!(
            block_payload(&json!({"payload": "hello"})).unwrap(),
            FlowValue::Text("hello".into())
        );
        for input in [
            json!({"payload": {"a": 1}}),
            json!({"payload": 7}),
            json!({"payload": null}),
            json!({}),
        ] {
            assert!(block_payload(&input).is_err());
        }
    }
}
