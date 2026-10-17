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
    Choice(&'static [&'static str]),
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
                value: ConfigValue::Choice(&["low", "medium", "high", "xhigh", "max"]),
                required: false,
            },
            ConfigKey {
                name: "stop",
                value: ConfigValue::TextList {
                    max_items: 4,
                    max_len: 256,
                },
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

/// The allow-listed block types in catalogue order, for the designer palette.
pub fn allowed_block_types() -> impl Iterator<Item = &'static str> {
    ALLOWED.iter().map(|block| block.node_type)
}

pub fn allowed_block(node_type: &str) -> Option<&'static AllowedBlock> {
    ALLOWED.iter().find(|block| block.node_type == node_type)
}

/// The settings `validate_block` refuses a block without, for the designer to
/// seed a new task with.
pub fn required_settings(node_type: &str) -> Vec<String> {
    allowed_block(node_type)
        .map(|block| {
            block
                .keys
                .iter()
                .filter(|key| key.required)
                .map(|key| key.name.to_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// Why a process may not run a block right now. `Unavailable` is "cannot tell":
/// a failed read is never a verdict on the initiator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BlockAccessError {
    #[error("process block is not available")]
    NotAllowed,
    // Same text as `NotAllowed`: a refused model must not tell the initiator
    // whether the model exists, as the chat endpoint does not.
    #[error("process block is not available")]
    ModelDenied,
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
        ConfigValue::Choice(choices) => value.as_str().is_some_and(|text| choices.contains(&text)),
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

/// The model a block calls, for the blocks that call one: what the model-access
/// check and the quota check are asked about.
pub fn block_model<'a>(node_type: &str, config: &'a Value) -> Option<&'a str> {
    if !needs_model(node_type) {
        return None;
    }
    config.get("model")?.as_str()
}

/// The answer of the quota check made before a block's job starts. `Unavailable`
/// is "cannot tell" (the check itself could not read what it needs) and, like
/// every such answer, is never reported as a spent quota.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuotaCheck {
    Within,
    Exceeded,
    Unavailable,
}

impl QuotaCheck {
    /// Only the gateway's own refusal means the quota is spent; a
    /// `QuotaReadError` and any other error are infrastructure.
    pub fn from_gateway(result: Result<()>) -> Self {
        match result {
            Ok(()) => Self::Within,
            Err(error)
                if matches!(
                    error.downcast_ref::<crate::error::CoreError>(),
                    Some(crate::error::CoreError::RateLimitExceeded { .. })
                ) =>
            {
                Self::Exceeded
            }
            Err(_) => Self::Unavailable,
        }
    }
}

/// Whether the block calls a model, so that its settings must name one.
pub fn needs_model(node_type: &str) -> bool {
    allowed_block(node_type).is_some_and(|block| block.keys.iter().any(|key| key.name == "model"))
}

/// A pinned graph read back by its structure: three nodes (`trigger`, one block,
/// `output`) joined by two plain edges, nothing else. It is deliberately NOT
/// compared with what today's code would build, so a later change to the graph
/// builder or the allow-list cannot make a published process unrunnable: what
/// is run is the pinned bytes (their SHA-256 is checked by the caller), and
/// whether the block may still run is an authorization question.
#[derive(Debug)]
pub struct PinnedBlock {
    pub node_type: String,
    pub config: Value,
    definition: FlowDefinition,
}

pub fn parse_pinned_block(graph_json: &str) -> Result<PinnedBlock> {
    let definition: FlowDefinition = serde_json::from_str(graph_json)?;
    ensure!(
        definition.nodes.len() == 3
            && definition.edges.len() == 2
            && definition.variables.is_empty(),
        "pinned block graph is not a one-block graph"
    );
    let node = |id: &str, node_type: Option<&str>| -> Result<&FlowNode> {
        let node = definition
            .nodes
            .iter()
            .find(|node| node.id == id)
            .with_context(|| format!("pinned block graph has no node {id}"))?;
        ensure!(
            node.region.is_none() && node_type.is_none_or(|expected| node.node_type == expected),
            "pinned block graph node {id} is not what a one-block graph holds"
        );
        Ok(node)
    };
    let trigger = node(TRIGGER_NODE_ID, Some("trigger"))?;
    let output = node(OUTPUT_NODE_ID, Some("output"))?;
    let block = node(BLOCK_NODE_ID, None)?;
    ensure!(
        [trigger, output]
            .iter()
            .all(|node| node.config.is_null() || node.config == Value::Object(Map::new())),
        "pinned block graph configures its trigger or output"
    );
    // The ports are those of the canonical graph. A block that has since left
    // the allow-list has no table entry; it is refused by authorization, not here.
    let ports = allowed_block(&block.node_type);
    let joins = |from: &str, to: &str, from_port: Option<&str>, to_port: Option<&str>| {
        definition
            .edges
            .iter()
            .filter(|edge| {
                edge.from == from
                    && edge.to == to
                    && edge.condition.is_none()
                    && edge.kind.is_none()
                    && edge.data_type == FlowDataType::Any
                    && from_port.is_none_or(|port| edge.from_port == port)
                    && to_port.is_none_or(|port| edge.to_port == port)
            })
            .count()
            == 1
    };
    ensure!(
        joins(
            TRIGGER_NODE_ID,
            BLOCK_NODE_ID,
            ports.map(|block| block.entry_port),
            Some("in")
        ) && joins(
            BLOCK_NODE_ID,
            OUTPUT_NODE_ID,
            Some("full"),
            ports.map(|block| block.exit_port)
        ),
        "pinned block graph is not trigger, block, output"
    );
    let config = match &block.config {
        Value::Null => Value::Object(Map::new()),
        other => other.clone(),
    };
    Ok(PinnedBlock {
        node_type: block.node_type.clone(),
        config,
        definition,
    })
}

impl PinnedBlock {
    /// The pin is the model's own block: same type, same settings.
    pub fn ensure_is(&self, node_type: &str, config: &Value) -> Result<()> {
        ensure!(
            self.node_type == node_type && &self.config == config,
            "the pinned block differs from the one the service task names"
        );
        Ok(())
    }

    /// The graph to compile for a run. Limits that must hold for every process
    /// run apply here, whatever an older pin carries: the platform never repeats
    /// a request after the dispatch boundary, so the LLM block's own retry loop
    /// is held to a single attempt.
    pub fn into_run_definition(mut self) -> FlowDefinition {
        if let Some(node) = self
            .definition
            .nodes
            .iter_mut()
            .find(|node| node.id == BLOCK_NODE_ID && node.node_type == "llm")
        {
            if let Value::Object(settings) = &mut node.config {
                settings.insert("max_request_attempts".to_owned(), Value::from(1));
            } else {
                node.config = serde_json::json!({ "max_request_attempts": 1 });
            }
        }
        self.definition
    }
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
        assert_eq!(parse_pinned_block(&first.0).unwrap().node_type, "llm");
    }

    /// The literal bytes of what publication has pinned for each allowed block.
    /// They must stay readable (and the builder must keep producing them for the
    /// same settings) whatever the graph builder or a node type looks like later.
    const GOLDEN: [(&str, &str, &str, &str); 3] = [
        (
            "llm",
            r#"{"model":"m"}"#,
            r#"{"edges":[{"condition":null,"from_node":"t1","from_port":"text","id":null,"label":null,"to_node":"b1"},{"condition":null,"from_node":"b1","id":null,"label":null,"to_node":"o1","to_port":"text"}],"nodes":[{"config":null,"id":"t1","label":null,"position":null,"type":"trigger"},{"config":{"model":"m"},"id":"b1","label":null,"position":null,"type":"llm"},{"config":null,"id":"o1","label":null,"position":null,"type":"output"}]}"#,
            "9901bd2b8da535637d93fdbe0d400dcda1044e784110bdcf042115cb48787c27",
        ),
        (
            "embeddings",
            r#"{"model":"e"}"#,
            r#"{"edges":[{"condition":null,"from_node":"t1","from_port":"text","id":null,"label":null,"to_node":"b1"},{"condition":null,"from_node":"b1","id":null,"label":null,"to_node":"o1","to_port":"embedding"}],"nodes":[{"config":null,"id":"t1","label":null,"position":null,"type":"trigger"},{"config":{"model":"e"},"id":"b1","label":null,"position":null,"type":"embeddings"},{"config":null,"id":"o1","label":null,"position":null,"type":"output"}]}"#,
            "5f898f9c30438ce6aaf6d76de49c2230f3dbff14c96eb77f9f970fa66be5cf01",
        ),
        (
            "pii_filter",
            "{}",
            r#"{"edges":[{"condition":null,"from_node":"t1","from_port":"text","id":null,"label":null,"to_node":"b1"},{"condition":null,"from_node":"b1","id":null,"label":null,"to_node":"o1","to_port":"text"}],"nodes":[{"config":null,"id":"t1","label":null,"position":null,"type":"trigger"},{"config":{},"id":"b1","label":null,"position":null,"type":"pii_filter"},{"config":null,"id":"o1","label":null,"position":null,"type":"output"}]}"#,
            "4fa2be9dfb5dc62f7c79369f9c90b06e2fa75847aeccee4bef30a0549927d07c",
        ),
    ];

    #[test]
    fn golden_pins_are_built_and_read_byte_for_byte() {
        assert_eq!(
            GOLDEN.len(),
            ALLOWED.len(),
            "every allowed block has a golden pin"
        );
        for (node_type, config, graph, sha256) in GOLDEN {
            let config: Value = serde_json::from_str(config).unwrap();
            assert_eq!(
                block_pin(node_type, &config).unwrap(),
                (graph.to_owned(), sha256.to_owned())
            );
            assert_eq!(hex::encode(Sha256::digest(graph.as_bytes())), sha256);
            let pinned = parse_pinned_block(graph).unwrap();
            assert_eq!(
                (pinned.node_type.as_str(), &pinned.config),
                (node_type, &config)
            );
            pinned.ensure_is(node_type, &config).unwrap();
        }
    }

    #[test]
    fn a_pin_is_read_by_structure_not_by_todays_builder() {
        let golden = GOLDEN[0].2;
        // A field a later FlowNode grows, another key order, a block narrowed out
        // of the allow-list: none of them makes a published pin unreadable.
        let mut grown: Value = serde_json::from_str(golden).unwrap();
        grown["nodes"][1]["future_field"] = json!(1);
        assert!(parse_pinned_block(&grown.to_string()).is_ok());
        let pretty =
            serde_json::to_string_pretty(&serde_json::from_str::<Value>(golden).unwrap()).unwrap();
        assert!(parse_pinned_block(&pretty).is_ok());
        let mut eight_stops: Value = serde_json::from_str(golden).unwrap();
        eight_stops["nodes"][1]["config"]["stop"] = json!(["1", "2", "3", "4", "5", "6", "7", "8"]);
        assert!(validate_block("llm", &eight_stops["nodes"][1]["config"]).is_err());
        assert!(parse_pinned_block(&eight_stops.to_string()).is_ok());
        let mut removed: Value = serde_json::from_str(golden).unwrap();
        removed["nodes"][1]["type"] = json!("a_block_no_longer_allowed");
        assert_eq!(
            parse_pinned_block(&removed.to_string()).unwrap().node_type,
            "a_block_no_longer_allowed"
        );
    }

    #[test]
    fn a_graph_that_is_not_trigger_block_output_is_refused() {
        let golden: Value = serde_json::from_str(GOLDEN[2].2).unwrap();
        let refused = |graph: Value| parse_pinned_block(&graph.to_string()).is_err();
        let mut extra = golden.clone();
        extra["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id": "x", "type": "output"}));
        assert!(refused(extra));
        let mut extra_edge = golden.clone();
        let edge = extra_edge["edges"][0].clone();
        extra_edge["edges"].as_array_mut().unwrap().push(edge);
        assert!(refused(extra_edge));
        let mut retargeted = golden.clone();
        retargeted["edges"][1]["to_node"] = json!("t1");
        assert!(refused(retargeted));
        let mut conditional = golden.clone();
        conditional["edges"][0]["condition"] = json!("true");
        assert!(refused(conditional));
        let mut streaming = golden.clone();
        streaming["nodes"][2]["config"] = json!({"mode": "stream"});
        assert!(refused(streaming));
        let mut renamed = golden.clone();
        renamed["nodes"][0]["type"] = json!("bus_consume");
        assert!(refused(renamed));
        let mut regional = golden.clone();
        regional["nodes"][1]["region"] = json!("r");
        assert!(refused(regional));
        let mut with_variables = golden.clone();
        with_variables["variables"] = json!([{"name": "v"}]);
        assert!(refused(with_variables));
    }

    #[test]
    fn a_rewired_port_or_edge_type_is_refused() {
        let golden: Value = serde_json::from_str(GOLDEN[0].2).unwrap();
        let refused = |graph: Value| parse_pinned_block(&graph.to_string()).is_err();
        for (edge, key, value) in [
            (0, "from_port", "audio"),
            (0, "to_port", "image"),
            (1, "from_port", "stream"),
            (1, "to_port", "audio"),
            (1, "data_type", "text"),
        ] {
            let mut rewired = golden.clone();
            rewired["edges"][edge][key] = json!(value);
            assert!(refused(rewired), "{edge} {key}");
        }
        // The canonical ports themselves are accepted.
        assert!(parse_pinned_block(&golden.to_string()).is_ok());
    }

    #[test]
    fn only_the_gateways_refusal_is_a_spent_quota() {
        let spent = anyhow::anyhow!(crate::error::CoreError::RateLimitExceeded {
            message: "token quota exceeded".into()
        });
        assert_eq!(QuotaCheck::from_gateway(Ok(())), QuotaCheck::Within);
        assert_eq!(QuotaCheck::from_gateway(Err(spent)), QuotaCheck::Exceeded);
        let unreadable = crate::compliance::ai_gateway::tests::unreadable_quota_error();
        assert_eq!(
            QuotaCheck::from_gateway(Err(unreadable)),
            QuotaCheck::Unavailable
        );
    }

    #[test]
    fn only_model_blocks_need_a_model() {
        assert!(needs_model("llm") && needs_model("embeddings"));
        assert!(!needs_model("pii_filter") && !needs_model("exec_command"));
    }

    #[test]
    fn the_pin_must_name_the_block_and_settings_of_the_task() {
        let pinned = parse_pinned_block(GOLDEN[0].2).unwrap();
        assert!(pinned.ensure_is("llm", &json!({"model": "m"})).is_ok());
        assert!(pinned.ensure_is("llm", &json!({"model": "other"})).is_err());
        assert!(pinned
            .ensure_is("embeddings", &json!({"model": "m"}))
            .is_err());
    }

    #[test]
    fn a_run_never_repeats_a_request_whatever_an_older_pin_says() {
        let mut older: Value = serde_json::from_str(GOLDEN[0].2).unwrap();
        older["nodes"][1]["config"]["max_request_attempts"] = json!(5);
        let definition = parse_pinned_block(&older.to_string())
            .unwrap()
            .into_run_definition();
        let llm = definition
            .nodes
            .iter()
            .find(|node| node.id == "b1")
            .unwrap();
        assert_eq!(llm.config["max_request_attempts"], 1);
        assert_eq!(llm.config["model"], "m");
        // The key is not offered for new pins.
        assert!(validate_block("llm", &json!({"model": "m", "max_request_attempts": 1})).is_err());
        let embeddings = parse_pinned_block(GOLDEN[1].2)
            .unwrap()
            .into_run_definition();
        assert!(embeddings.nodes[1]
            .config
            .get("max_request_attempts")
            .is_none());
    }

    #[test]
    fn stop_and_reasoning_effort_are_bounded() {
        let llm = |extra: Value| {
            let mut config = json!({"model": "m"});
            config
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            validate_block("llm", &config).is_ok()
        };
        assert!(llm(json!({"stop": ["a", "b", "c", "d"]})));
        assert!(!llm(json!({"stop": ["a", "b", "c", "d", "e"]})));
        for effort in ["low", "medium", "high", "xhigh", "max"] {
            assert!(llm(json!({ "reasoning_effort": effort })), "{effort}");
        }
        for effort in ["", "HIGH", "extreme", "low "] {
            assert!(!llm(json!({ "reasoning_effort": effort })), "{effort:?}");
        }
    }

    #[test]
    fn only_model_blocks_name_a_model() {
        assert_eq!(block_model("llm", &json!({"model": "m"})), Some("m"));
        assert_eq!(block_model("embeddings", &json!({"model": "e"})), Some("e"));
        assert_eq!(block_model("pii_filter", &json!({"model": "m"})), None);
        assert_eq!(block_model("exec_command", &json!({"model": "m"})), None);
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
