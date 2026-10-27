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

use super::agent_sink::critic_tools;
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

/// The pin publication stores for the critic of a step: the agent as it runs as
/// a critic, which means with the read-only tools of [`critic_tools`] (the
/// intersection with its own tools is taken at run time, so the critic needs no
/// particular tool of its own, only to be runnable).
pub fn pin_critic(pool: &DbPool, agent_id: &str) -> Result<AgentPin, AgentPinError> {
    let conn = pool.read().map_err(|_| AgentPinError::Unavailable)?;
    let agent = runnable_agent(&conn, agent_id)?;
    agent_pin(&agent, &critic_tools()).map_err(|_| AgentPinError::Unavailable)
}
