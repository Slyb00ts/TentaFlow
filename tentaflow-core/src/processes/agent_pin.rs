// ===== File: agent_pin.rs — publication-time checks and pin of an agent step =====
//
// An agent is not frozen by publication (owner decision 2 of B3: a process uses
// the agent's current version), so the pin is a record for the history, not a
// lock: which agent, a digest of what it was at publication, and the tools the
// step was limited to. Everything that makes the step runnable at all is
// checked here, once, against the live registry.

use rusqlite::Connection;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::agents::{allowlist_admits_entry, AgentRuntime};
use crate::db::{models::DbAgent, repository, DbPool};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AgentPinError {
    #[error("process initiator is inactive or outside the organization")]
    Initiator,
    #[error("agent availability could not be determined")]
    Unavailable,
    /// A fact about the agent or the step that publication cannot accept; the
    /// text names it for the author.
    #[error("{0}")]
    Refused(String),
}

/// What publication stores for an agent step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPin {
    pub graph_json: String,
    pub graph_sha256: String,
}

/// The agent as a step can run it today: it exists, is enabled, does not
/// continue itself outside the step and runs on a model (a CLI engine has no way
/// to call the completion tool). This is the
/// whole of what a step needs from a registry that may change after
/// publication, because a process uses the agent's current version.
pub fn runnable_agent(conn: &Connection, agent_id: &str) -> Result<DbAgent, AgentPinError> {
    let agent = repository::get_agent_on(conn, agent_id)
        .map_err(|_| AgentPinError::Unavailable)?
        .ok_or_else(|| AgentPinError::Refused(format!("the agent {agent_id} does not exist")))?;
    if !agent.is_enabled {
        let name = agent.display_name.as_deref().unwrap_or(&agent.name);
        return Err(AgentPinError::Refused(format!(
            "the agent \"{name}\" is disabled"
        )));
    }
    // A continuation is a new top-level run, which would leave the step: it has
    // none of the step's narrowing and nothing cancels it with the step.
    if agent.on_child_complete == "continue" {
        return Err(AgentPinError::Refused(
            "an agent that continues on its own when a delegate finishes cannot be a process step"
                .into(),
        ));
    }
    match AgentRuntime::parse(&agent.runtime_json) {
        Ok(AgentRuntime::Llm) => Ok(agent),
        Ok(AgentRuntime::Cli(_)) => Err(AgentPinError::Refused(
            "an agent that runs on a CLI engine cannot be a process step".into(),
        )),
        Err(_) => Err(AgentPinError::Unavailable),
    }
}

/// Checks that `agent_id` can be a process step with this tool list and
/// iteration budget: it is [`runnable_agent`], the step's tools are a subset of
/// the agent's own, and the step does not ask for more iterations than the
/// agent allows.
pub fn check_agent(
    conn: &Connection,
    agent_id: &str,
    allowed_tools: &[String],
    max_iterations: u32,
) -> Result<DbAgent, AgentPinError> {
    let agent = runnable_agent(conn, agent_id)?;
    if let Some(tool) = allowed_tools
        .iter()
        .find(|tool| !allowlist_admits_entry(&agent.tools_json, tool))
    {
        return Err(AgentPinError::Refused(format!(
            "the step allows the tool {tool}, which the agent itself does not have"
        )));
    }
    if i64::from(max_iterations) > agent.max_iterations {
        return Err(AgentPinError::Refused(format!(
            "the step allows {max_iterations} iterations, the agent only {}",
            agent.max_iterations
        )));
    }
    Ok(agent)
}

/// Canonical pin of an agent step: its id, a digest of the parts of the agent
/// that decide what it does, and the tool list of the step.
pub fn agent_pin(agent: &DbAgent, allowed_tools: &[String]) -> Result<AgentPin, serde_json::Error> {
    let digest = hex::encode(Sha256::digest(
        serde_json::to_string(&json!({
            "system_prompt": agent.system_prompt,
            "model": agent.model,
            "tools_json": agent.tools_json,
            "params_json": agent.params_json,
            "runtime_json": agent.runtime_json,
        }))?
        .as_bytes(),
    ));
    let graph_json = serde_json::to_string(&json!({
        "agent_id": agent.id,
        "digest": digest,
        "allowed_tools": allowed_tools,
    }))?;
    let graph_sha256 = hex::encode(Sha256::digest(graph_json.as_bytes()));
    Ok(AgentPin {
        graph_json,
        graph_sha256,
    })
}

/// [`check_agent`] and [`agent_pin`] in one: the pin publication stores.
pub fn pin_agent(
    pool: &DbPool,
    agent_id: &str,
    allowed_tools: &[String],
    max_iterations: u32,
) -> Result<AgentPin, AgentPinError> {
    let conn = pool.read().map_err(|_| AgentPinError::Unavailable)?;
    let agent = check_agent(&conn, agent_id, allowed_tools, max_iterations)?;
    agent_pin(&agent, allowed_tools).map_err(|_| AgentPinError::Unavailable)
}

/// The only block types a critic's harness may contain. A critic only checks, and
/// narrowing its tool catalog does not reach a block of its own harness graph: a
/// block that writes, executes, publishes, asks a person or starts other agents
/// acts regardless of the tools the run was given. So the list is of what is
/// known to be harmless, not of what is known to be harmful: the blocks of the
/// registry's own harness and the pure transforms of text. An addon block
/// (`addon.*`) is never on it.
const HARNESS_BLOCKS: &[&str] = &[
    "trigger",
    "output",
    "conversation_history",
    "agent_context",
    "compact_context",
    "llm",
    "tool_exec",
    "persist_turn",
    "condition",
    "combine",
    "pii_filter",
    "sentence_buffer",
    "tts_clean",
];

/// How many flows deep a harness may nest through `loop`, `map` and `subflow`
/// bodies before the check gives up: past the limit the body is not looked at,
/// so it is not allowed.
const MAX_HARNESS_DEPTH: usize = 4;

/// The config key naming the body flow of a block that runs another flow.
fn body_flow_key(node_type: &str) -> Option<&'static str> {
    match node_type {
        "loop" | "map" => Some("body_flow_id"),
        "subflow" => Some("flow_id"),
        _ => None,
    }
}

/// Why a harness graph cannot run a critic: it holds a block that is not known
/// to be harmless, here or in the body of a `loop`, `map` or `subflow` it runs.
/// `flow_id` is the flow the graph belongs to. A body is read as the flow row the
/// engine runs it from (the engine pins no body), so the answer holds for the
/// moment it is given.
pub fn critic_harness_refusal(
    conn: &Connection,
    flow_id: &str,
    graph_json: &str,
) -> Result<(), AgentPinError> {
    check_harness_graph(conn, graph_json, &mut vec![flow_id.to_owned()])
}

fn check_harness_graph(
    conn: &Connection,
    graph_json: &str,
    path: &mut Vec<String>,
) -> Result<(), AgentPinError> {
    let definition: crate::flow_engine::types::FlowDefinition = serde_json::from_str(graph_json)
        .map_err(|_| {
            AgentPinError::Refused("the harness flow of the critic cannot be read".into())
        })?;
    for node in &definition.nodes {
        let node_type = node.node_type.as_str();
        let Some(key) = body_flow_key(node_type) else {
            if HARNESS_BLOCKS.contains(&node_type) {
                continue;
            }
            return Err(AgentPinError::Refused(format!(
                "the harness flow of the critic has a block that may have an effect ({node_type})"
            )));
        };
        let body_id = node
            .config
            .get(key)
            .and_then(|value| value.as_str())
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                AgentPinError::Refused(format!(
                    "the harness flow of the critic has a {node_type} block with no body flow it can check"
                ))
            })?;
        if path.iter().any(|visited| visited == body_id) {
            return Err(AgentPinError::Refused(format!(
                "the harness flow of the critic runs the flow {body_id} from inside itself"
            )));
        }
        if path.len() >= MAX_HARNESS_DEPTH {
            return Err(AgentPinError::Refused(format!(
                "the harness flow of the critic nests flows deeper than {MAX_HARNESS_DEPTH}"
            )));
        }
        let body: Option<String> = conn
            .query_row(
                "SELECT flow_json FROM flows WHERE id=?1",
                [body_id],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                _ => Err(AgentPinError::Unavailable),
            })?;
        let body = body.ok_or_else(|| {
            AgentPinError::Refused(format!(
                "the harness flow of the critic runs the flow {body_id}, which does not exist"
            ))
        })?;
        path.push(body_id.to_owned());
        check_harness_graph(conn, &body, path)?;
        path.pop();
    }
    Ok(())
}

/// The harness flow an agent runs, the way a run resolves it.
fn harness_flow_id(agent: &DbAgent) -> Result<String, AgentPinError> {
    match agent.flow_id.as_deref().filter(|id| !id.is_empty()) {
        Some(flow_id) => Ok(flow_id.to_owned()),
        None => crate::flow_engine::node_adapters::flow_for(agent)
            .map(str::to_owned)
            .map_err(|error| AgentPinError::Refused(format!("{error:#}"))),
    }
}

/// [`runnable_agent`] for a critic: its harness must also be free of blocks with
/// an effect.
pub fn runnable_critic(conn: &Connection, agent_id: &str) -> Result<DbAgent, AgentPinError> {
    let agent = runnable_agent(conn, agent_id)?;
    let flow_id = harness_flow_id(&agent)?;
    let flow_json: Option<String> = conn
        .query_row(
            "SELECT flow_json FROM flows WHERE id=?1",
            [&flow_id],
            |row| row.get(0),
        )
        .map(Some)
        .or_else(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            _ => Err(AgentPinError::Unavailable),
        })?;
    let flow_json = flow_json.ok_or_else(|| {
        AgentPinError::Refused("the harness flow of the critic does not exist".into())
    })?;
    critic_harness_refusal(conn, &flow_id, &flow_json)?;
    Ok(agent)
}

/// The pin publication stores for the critic of a step: the agent as it runs as
/// a critic, which means with the read-only tools `tools` (the intersection with
/// its own tools is taken at run time, so the critic needs no particular tool of
/// its own, only to be runnable).
pub fn pin_critic(
    pool: &DbPool,
    agent_id: &str,
    tools: &[String],
) -> Result<AgentPin, AgentPinError> {
    let conn = pool.read().map_err(|_| AgentPinError::Unavailable)?;
    let agent = runnable_critic(&conn, agent_id)?;
    agent_pin(&agent, tools).map_err(|_| AgentPinError::Unavailable)
}
