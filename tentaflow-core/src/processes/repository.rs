// ============ File: repository.rs — transactional BPMN definitions and runtime state ============

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tentaflow_protocol::processes::{
    ActivityResult, PinnedFlowInfo, ProcessDefinition, ProcessDefinitionSummary, ProcessEvent,
    ProcessIncident, ProcessInstance, ProcessInstanceStatus, ProcessInstanceSummary, ProcessModel,
    ProcessPayload, ProcessUserTask, ProcessUserTaskKind, ProcessUserTaskStatus,
    ProcessUserTaskSummary, ProcessVersion, ProcessVersionSummary,
};
use uuid::Uuid;

use super::model::{starter_model, validate_model, validate_variables, MAX_MODEL_BYTES};
use super::runtime::validate_output;
use crate::db::DbPool;

#[derive(Debug, Clone)]
pub struct ProcessActor {
    pub org_id: String,
    pub user_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinnedServiceSnapshot {
    pub info: PinnedFlowInfo,
    pub graph_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForkFrame {
    pub activation_id: String,
    pub split_node_id: String,
    pub join_node_id: String,
    pub branch_edge_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessToken {
    pub token_id: String,
    pub node_id: String,
    pub arrival_edge_id: Option<String>,
    pub fork_stack: Vec<ForkFrame>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AndReceipt {
    pub join_node_id: String,
    pub activation_id: String,
    pub branch_edge_id: String,
    pub token_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessJob {
    pub job_id: String,
    pub instance_id: String,
    pub node_id: String,
    pub token_id: String,
    pub input: Value,
    pub status: String,
    pub attempt: u32,
    pub fence: u64,
    pub worker_id: Option<String>,
    pub lease_until_ms: Option<i64>,
    pub result: Option<ActivityResult>,
}

#[derive(Debug, Clone)]
pub struct RuntimeSnapshot {
    pub instance: ProcessInstance,
    pub model: ProcessModel,
    pub user_tasks: Vec<ProcessUserTask>,
    pub tokens: Vec<ProcessToken>,
    pub jobs: Vec<ProcessJob>,
    pub receipts: Vec<AndReceipt>,
    pub service_snapshots: Vec<PinnedServiceSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedEvent {
    pub kind: String,
    pub node_id: Option<String>,
    pub data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePlan {
    pub consume_token_ids: Vec<String>,
    pub create_tokens: Vec<ProcessToken>,
    pub add_receipts: Vec<AndReceipt>,
    pub remove_receipts: Vec<AndReceipt>,
    pub create_user_tasks: Vec<ProcessUserTask>,
    pub complete_user_task_ids: Vec<String>,
    pub create_jobs: Vec<ProcessJob>,
    pub complete_job_ids: Vec<String>,
    pub add_incidents: Vec<ProcessIncident>,
    pub resolve_incident_ids: Vec<String>,
    pub variables: Value,
    pub status: ProcessInstanceStatus,
    pub events: Vec<PlannedEvent>,
}

impl RuntimePlan {
    pub fn initial(variables: Value) -> Self {
        Self {
            consume_token_ids: Vec::new(),
            create_tokens: Vec::new(),
            add_receipts: Vec::new(),
            remove_receipts: Vec::new(),
            create_user_tasks: Vec::new(),
            complete_user_task_ids: Vec::new(),
            create_jobs: Vec::new(),
            complete_job_ids: Vec::new(),
            add_incidents: Vec::new(),
            resolve_incident_ids: Vec::new(),
            variables,
            status: ProcessInstanceStatus::Running,
            events: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CommandStamp {
    pub command_id: String,
    pub request_hash: String,
}

#[derive(Debug, Clone)]
pub struct ClaimedProcessJob {
    pub actor: ProcessActor,
    pub job: ProcessJob,
    pub snapshot: RuntimeSnapshot,
}

fn now_ms() -> Result<i64> {
    Ok(i64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}

fn json<T: Serialize>(value: &T) -> Result<String> {
    Ok(serde_json::to_string(value)?)
}

fn parse<T: for<'de> Deserialize<'de>>(text: String) -> Result<T> {
    Ok(serde_json::from_str(&text)?)
}

fn sql_integer(value: u64) -> Result<i64> {
    i64::try_from(value).context("process counter exceeds SQLite INTEGER range")
}

fn sql_incrementable(value: u64) -> Result<i64> {
    let integer = sql_integer(value)?;
    ensure!(integer < i64::MAX, "process counter cannot advance");
    Ok(integer)
}

fn row_u64(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

pub fn request_hash<T: Serialize>(request: &T) -> Result<String> {
    Ok(hex::encode(Sha256::digest(serde_json::to_vec(request)?)))
}

fn require_actor(conn: &Connection, actor: &ProcessActor) -> Result<()> {
    let active: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM user_accounts u JOIN org_memberships m ON m.user_id=u.id JOIN organizations o ON o.org_id=m.org_id WHERE u.id=?1 AND m.org_id=?2 AND u.is_active=1 AND o.status='active')",
        params![actor.user_id, actor.org_id], |row| row.get(0),
    )?;
    ensure!(
        active,
        "process actor is not active in the current organization"
    );
    Ok(())
}

fn require_owner(conn: &Connection, actor: &ProcessActor, definition_id: &str) -> Result<()> {
    let owns: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM bpmn_definitions WHERE definition_id=?1 AND org_id=?2 AND owner_user_id=?3)",
        params![definition_id, actor.org_id, actor.user_id], |row| row.get(0),
    )?;
    ensure!(owns, "process definition not found");
    Ok(())
}

fn require_flow_current(
    conn: &Connection,
    actor: &ProcessActor,
    flow_id: &str,
    expected: Option<&PinnedServiceSnapshot>,
) -> Result<()> {
    let role: String = conn
        .query_row(
            "SELECT role FROM user_accounts WHERE id=?1 AND is_active=1",
            [&actor.user_id],
            |row| row.get(0),
        )
        .context("process actor is inactive")?;
    let (version, graph): (u32, String) = conn
        .query_row(
            "SELECT version,flow_json FROM flows WHERE id=?1 AND status='active'",
            [flow_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .context("source flow is no longer active")?;
    ensure!(
        crate::db::repository::resource_permissions::check_default_allow(
            conn,
            "flow",
            flow_id,
            &actor.user_id,
            &role
        )?,
        "process actor no longer has source flow access"
    );
    if let Some(snapshot) = expected {
        ensure!(
            snapshot.info.source_version == version,
            "source flow version changed before publication"
        );
        ensure!(
            snapshot.graph_json == graph
                && snapshot.info.graph_sha256 == hex::encode(Sha256::digest(graph.as_bytes())),
            "source flow changed before publication"
        );
    }
    Ok(())
}

fn require_instance_reader(
    conn: &Connection,
    actor: &ProcessActor,
    instance_id: &str,
) -> Result<bool> {
    let initiator: Option<String> = conn
        .query_row(
            "SELECT initiator_user_id FROM bpmn_instances WHERE instance_id=?1 AND org_id=?2",
            params![instance_id, actor.org_id],
            |row| row.get(0),
        )
        .optional()?;
    let initiator = initiator.context("process instance not found")?;
    if initiator == actor.user_id {
        return Ok(true);
    }
    let assigned: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM bpmn_user_tasks WHERE instance_id=?1 AND assignee_user_id=?2)",
        params![instance_id, actor.user_id],
        |row| row.get(0),
    )?;
    ensure!(assigned, "process instance not found");
    Ok(false)
}

fn command_replay<T: for<'de> Deserialize<'de>>(
    tx: &Transaction<'_>,
    actor: &ProcessActor,
    stamp: &CommandStamp,
) -> Result<Option<T>> {
    ensure!(
        Uuid::parse_str(&stamp.command_id).is_ok(),
        "command_id must be a UUID"
    );
    let stored: Option<(String, String)> = tx.query_row(
        "SELECT request_hash,result_json FROM bpmn_commands WHERE org_id=?1 AND actor_user_id=?2 AND command_id=?3",
        params![actor.org_id, actor.user_id, stamp.command_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional()?;
    if let Some((hash, result)) = stored {
        ensure!(
            hash == stamp.request_hash,
            "command_id was reused for a different request"
        );
        return Ok(Some(parse(result)?));
    }
    Ok(None)
}

fn store_command<T: Serialize>(
    tx: &Transaction<'_>,
    actor: &ProcessActor,
    stamp: &CommandStamp,
    result: &T,
    at_ms: i64,
) -> Result<()> {
    tx.execute(
        "INSERT INTO bpmn_commands(org_id,actor_user_id,command_id,request_hash,result_json,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6)",
        params![actor.org_id, actor.user_id, stamp.command_id, stamp.request_hash, json(result)?, at_ms],
    )?;
    Ok(())
}

fn definition_on(conn: &Connection, definition_id: &str) -> Result<ProcessDefinition> {
    conn.query_row(
        "SELECT definition_id,name,description,owner_user_id,draft_revision,model_json,published_version,archived FROM bpmn_definitions WHERE definition_id=?1",
        [definition_id],
        |row| {
            let model_text: String = row.get(5)?;
            let model = serde_json::from_str(&model_text).map_err(|error| rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(error)))?;
            Ok(ProcessDefinition {
                definition_id: row.get(0)?, name: row.get(1)?, description: row.get(2)?,
                owner_user_id: row.get(3)?, draft_revision: row_u64(row, 4)?, model,
                published_version: row.get(6)?, archived: row.get(7)?,
            })
        },
    ).map_err(Into::into)
}

fn page(offset: u32, limit: u32) -> Result<()> {
    ensure!(
        (1..=100).contains(&limit) && offset <= 100_000,
        "process page is out of bounds"
    );
    Ok(())
}

fn read_snapshot<T>(pool: &DbPool, read: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let conn = pool.read()?;
    let tx = conn.unchecked_transaction()?;
    let result = read(&tx)?;
    tx.commit()?;
    Ok(result)
}

pub fn list_definitions(
    pool: &DbPool,
    actor: &ProcessActor,
    offset: u32,
    limit: u32,
) -> Result<(Vec<ProcessDefinitionSummary>, u32, bool)> {
    page(offset, limit)?;
    read_snapshot(pool, |conn| {
        require_actor(conn, actor)?;
        let total: u32 = conn.query_row(
            "SELECT COUNT(*) FROM bpmn_definitions WHERE org_id=?1 AND owner_user_id=?2",
            params![actor.org_id, actor.user_id],
            |row| row.get(0),
        )?;
        let mut stmt = conn.prepare("SELECT definition_id,name,description,owner_user_id,draft_revision,published_version,archived FROM bpmn_definitions WHERE org_id=?1 AND owner_user_id=?2 ORDER BY updated_at_ms DESC,definition_id DESC LIMIT ?3 OFFSET ?4")?;
        let definitions = stmt
            .query_map(params![actor.org_id, actor.user_id, limit, offset], |row| {
                Ok(ProcessDefinitionSummary {
                    definition_id: row.get(0)?,
                    name: row.get(1)?,
                    description: row.get(2)?,
                    owner_user_id: row.get(3)?,
                    draft_revision: row_u64(row, 4)?,
                    published_version: row.get(5)?,
                    archived: row.get(6)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((definitions, total, offset.saturating_add(limit) < total))
    })
}

pub fn get_definition(
    pool: &DbPool,
    actor: &ProcessActor,
    definition_id: &str,
) -> Result<ProcessDefinition> {
    read_snapshot(pool, |conn| {
        require_actor(conn, actor)?;
        require_owner(conn, actor, definition_id)?;
        definition_on(conn, definition_id)
    })
}

pub fn save_definition(
    pool: &DbPool,
    actor: &ProcessActor,
    stamp: &CommandStamp,
    definition_id: Option<&str>,
    expected_revision: u64,
    name: &str,
    description: &str,
    model: &ProcessModel,
) -> Result<ProcessDefinition> {
    super::model::validate_draft(model)?;
    ensure!(
        !name.trim().is_empty() && name.len() <= 256 && !name.chars().any(char::is_control),
        "invalid process name"
    );
    ensure!(description.len() <= 1024, "process description is too long");
    let now = now_ms()?;
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    require_actor(&tx, actor)?;
    if let Some(id) = definition_id {
        require_owner(&tx, actor, id)?;
    }
    if let Some(result) = command_replay(&tx, actor, stamp)? {
        return Ok(result);
    }
    let id = if let Some(id) = definition_id {
        let changed = tx.execute(
            "UPDATE bpmn_definitions SET name=?1,description=?2,model_json=?3,draft_revision=draft_revision+1,updated_at_ms=?4 WHERE definition_id=?5 AND org_id=?6 AND owner_user_id=?7 AND draft_revision=?8 AND archived=0",
            params![name.trim(), description, json(model)?, now, id, actor.org_id, actor.user_id, sql_incrementable(expected_revision)?],
        )?;
        ensure!(
            changed == 1,
            "process draft revision conflict or archived definition"
        );
        id.to_string()
    } else {
        ensure!(
            expected_revision == 0,
            "new process expected_revision must be zero"
        );
        let id = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO bpmn_definitions(definition_id,org_id,owner_user_id,name,description,draft_revision,model_json,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,1,?6,?7,?7)",
            params![id, actor.org_id, actor.user_id, name.trim(), description, json(model)?, now],
        )?;
        id
    };
    let result = definition_on(&tx, &id)?;
    store_command(&tx, actor, stamp, &result, now)?;
    tx.commit()?;
    Ok(result)
}

pub fn create_starter_definition(
    pool: &DbPool,
    actor: &ProcessActor,
    stamp: &CommandStamp,
    name: &str,
) -> Result<ProcessDefinition> {
    save_definition(pool, actor, stamp, None, 0, name, "", &starter_model())
}

fn version_on(conn: &Connection, definition_id: &str, version: u32) -> Result<ProcessVersion> {
    conn.query_row(
        "SELECT model_json,published_at_ms,published_by,model_sha256,service_snapshots_json FROM bpmn_versions WHERE definition_id=?1 AND version=?2",
        params![definition_id, version],
        |row| {
            let model_json: String = row.get(0)?;
            let snapshots_json: String = row.get(4)?;
            let model: ProcessModel = serde_json::from_str(&model_json).map_err(|error| rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error)))?;
            let snapshots: Vec<PinnedServiceSnapshot> = serde_json::from_str(&snapshots_json).map_err(|error| rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(error)))?;
            Ok(ProcessVersion { definition_id: definition_id.to_string(), version, model,
                published_at_ms: row.get(1)?, published_by: row.get(2)?, model_sha256: row.get(3)?,
                service_flows: snapshots.into_iter().map(|snapshot| snapshot.info).collect() })
        },
    ).map_err(Into::into)
}

pub fn list_versions(
    pool: &DbPool,
    actor: &ProcessActor,
    definition_id: &str,
    offset: u32,
    limit: u32,
) -> Result<(Vec<ProcessVersionSummary>, u32, bool)> {
    page(offset, limit)?;
    read_snapshot(pool, |conn| {
        require_actor(conn, actor)?;
        require_owner(conn, actor, definition_id)?;
        let total: u32 = conn.query_row(
            "SELECT COUNT(*) FROM bpmn_versions WHERE definition_id=?1",
            [definition_id],
            |row| row.get(0),
        )?;
        let mut stmt = conn.prepare("SELECT version,published_at_ms,published_by,model_sha256 FROM bpmn_versions WHERE definition_id=?1 ORDER BY version DESC LIMIT ?2 OFFSET ?3")?;
        let versions = stmt
            .query_map(params![definition_id, limit, offset], |row| {
                Ok(ProcessVersionSummary {
                    definition_id: definition_id.into(),
                    version: row.get(0)?,
                    published_at_ms: row.get(1)?,
                    published_by: row.get(2)?,
                    model_sha256: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok((versions, total, offset.saturating_add(limit) < total))
    })
}

pub fn get_version(
    pool: &DbPool,
    actor: &ProcessActor,
    definition_id: &str,
    version: u32,
) -> Result<ProcessVersion> {
    read_snapshot(pool, |conn| {
        require_actor(conn, actor)?;
        require_owner(conn, actor, definition_id)?;
        version_on(conn, definition_id, version)
    })
}

pub fn publish_definition(
    pool: &DbPool,
    actor: &ProcessActor,
    stamp: &CommandStamp,
    definition_id: &str,
    expected_revision: u64,
    snapshots: &[PinnedServiceSnapshot],
) -> Result<(ProcessDefinition, ProcessVersion)> {
    let now = now_ms()?;
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    require_actor(&tx, actor)?;
    require_owner(&tx, actor, definition_id)?;
    if let Some(result) = command_replay(&tx, actor, stamp)? {
        return Ok(result);
    }
    let definition = definition_on(&tx, definition_id)?;
    ensure!(
        !definition.archived && definition.draft_revision == expected_revision,
        "process draft revision conflict or archived definition"
    );
    validate_model(&definition.model)?;
    let mut service_nodes = HashSet::new();
    for node in &definition.model.nodes {
        if let tentaflow_protocol::processes::ProcessNodeKind::ServiceTask { flow_id, .. } =
            &node.kind
        {
            service_nodes.insert((node.id.as_str(), flow_id.as_str()));
        }
    }
    ensure!(
        snapshots.len() == service_nodes.len(),
        "every service task requires one server-minted flow snapshot"
    );
    let mut covered = HashSet::new();
    for snapshot in snapshots {
        ensure!(
            service_nodes.contains(&(
                snapshot.info.node_id.as_str(),
                snapshot.info.flow_id.as_str()
            )),
            "flow snapshot does not match service task"
        );
        ensure!(
            covered.insert(snapshot.info.node_id.as_str()),
            "duplicate service task snapshot"
        );
        ensure!(
            snapshot.graph_json.len() <= MAX_MODEL_BYTES,
            "flow snapshot exceeds 2 MiB"
        );
        ensure!(
            snapshot.info.graph_sha256
                == hex::encode(Sha256::digest(snapshot.graph_json.as_bytes())),
            "flow snapshot hash mismatch"
        );
        require_flow_current(&tx, actor, &snapshot.info.flow_id, Some(snapshot))?;
    }
    let version = definition
        .published_version
        .unwrap_or(0)
        .checked_add(1)
        .context("process version overflow")?;
    let model_json = json(&definition.model)?;
    tx.execute(
        "INSERT INTO bpmn_versions(definition_id,version,model_json,model_sha256,service_snapshots_json,published_at_ms,published_by) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![definition_id, version, model_json, hex::encode(Sha256::digest(model_json.as_bytes())), json(&snapshots)?, now, actor.user_id],
    )?;
    tx.execute(
        "UPDATE bpmn_definitions SET published_version=?1,updated_at_ms=?2 WHERE definition_id=?3",
        params![version, now, definition_id],
    )?;
    let result = (
        definition_on(&tx, definition_id)?,
        version_on(&tx, definition_id, version)?,
    );
    let publication_frame = tentaflow_protocol::cbor::encode(
        &tentaflow_protocol::message_body::MessageBody::ProcessBody(
            ProcessPayload::DefinitionPublishResponse {
                definition: ProcessDefinitionSummary::from(&result.0),
                version: result.1.clone(),
            },
        ),
    )
    .map_err(anyhow::Error::msg)?;
    ensure!(
        publication_frame.len() <= 900 * 1024,
        "published process response exceeds the wire budget"
    );
    store_command(&tx, actor, stamp, &result, now)?;
    tx.commit()?;
    Ok(result)
}

pub fn replay_definition_publication(
    pool: &DbPool,
    actor: &ProcessActor,
    stamp: &CommandStamp,
    definition_id: &str,
) -> Result<Option<(ProcessDefinition, ProcessVersion)>> {
    read_snapshot(pool, |conn| {
        require_actor(conn, actor)?;
        require_owner(conn, actor, definition_id)?;
        ensure!(
            Uuid::parse_str(&stamp.command_id).is_ok(),
            "command_id must be a UUID"
        );
        let stored: Option<(String, String)> = conn
        .query_row(
            "SELECT request_hash,result_json FROM bpmn_commands WHERE org_id=?1 AND actor_user_id=?2 AND command_id=?3",
            params![actor.org_id, actor.user_id, stamp.command_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
        if let Some((hash, result)) = stored {
            ensure!(
                hash == stamp.request_hash,
                "command_id was reused for a different request"
            );
            let prior: (ProcessDefinition, ProcessVersion) = parse(result)?;
            ensure!(
                prior.0.definition_id == definition_id && prior.1.definition_id == definition_id,
                "publication command belongs to another definition"
            );
            return Ok(Some(prior));
        }
        Ok(None)
    })
}

pub fn archive_definition(
    pool: &DbPool,
    actor: &ProcessActor,
    stamp: &CommandStamp,
    definition_id: &str,
    expected_revision: u64,
    archived: bool,
) -> Result<ProcessDefinition> {
    let now = now_ms()?;
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    require_actor(&tx, actor)?;
    require_owner(&tx, actor, definition_id)?;
    if let Some(result) = command_replay(&tx, actor, stamp)? {
        return Ok(result);
    }
    let changed = tx.execute(
        "UPDATE bpmn_definitions SET archived=?1,draft_revision=draft_revision+1,updated_at_ms=?2 WHERE definition_id=?3 AND draft_revision=?4",
        params![archived, now, definition_id, sql_incrementable(expected_revision)?],
    )?;
    ensure!(changed == 1, "process draft revision conflict");
    let result = definition_on(&tx, definition_id)?;
    store_command(&tx, actor, stamp, &result, now)?;
    tx.commit()?;
    Ok(result)
}

fn status_text(status: &ProcessInstanceStatus) -> &'static str {
    match status {
        ProcessInstanceStatus::Running => "running",
        ProcessInstanceStatus::Waiting => "waiting",
        ProcessInstanceStatus::Completed => "completed",
        ProcessInstanceStatus::Incident => "incident",
        ProcessInstanceStatus::Cancelled => "cancelled",
    }
}

fn incident_message(message: &str) -> String {
    const MAX_BYTES: usize = 512;
    const SUFFIX: &str = "… (see process history)";
    if message.len() <= MAX_BYTES {
        return message.to_string();
    }
    let mut summary = String::new();
    for character in message.chars() {
        if summary.len() + character.len_utf8() + SUFFIX.len() > MAX_BYTES {
            break;
        }
        summary.push(character);
    }
    summary.push_str(SUFFIX);
    summary
}

pub(super) fn bounded_failure_message(message: &str) -> String {
    const MAX_BYTES: usize = 32 * 1024;
    if message.len() <= MAX_BYTES {
        return message.to_string();
    }
    let mut preview = String::new();
    for character in message.chars() {
        if preview.len() + character.len_utf8() > 8 * 1024 {
            break;
        }
        preview.push(character);
    }
    format!(
        "{preview}… (failure reason exceeded 32 KiB; original bytes={}, sha256={})",
        message.len(),
        hex::encode(Sha256::digest(message.as_bytes()))
    )
}

fn status_from_text(status: &str) -> Result<ProcessInstanceStatus> {
    Ok(match status {
        "running" => ProcessInstanceStatus::Running,
        "waiting" => ProcessInstanceStatus::Waiting,
        "completed" => ProcessInstanceStatus::Completed,
        "incident" => ProcessInstanceStatus::Incident,
        "cancelled" => ProcessInstanceStatus::Cancelled,
        _ => bail!("unknown process instance status {status}"),
    })
}

fn task_kind_text(kind: &ProcessUserTaskKind) -> &'static str {
    match kind {
        ProcessUserTaskKind::Work => "work",
        ProcessUserTaskKind::Verification => "verification",
    }
}

fn task_kind_from_text(kind: &str) -> Result<ProcessUserTaskKind> {
    match kind {
        "work" => Ok(ProcessUserTaskKind::Work),
        "verification" => Ok(ProcessUserTaskKind::Verification),
        _ => bail!("invalid user task kind"),
    }
}

fn task_status_from_text(status: &str) -> Result<ProcessUserTaskStatus> {
    match status {
        "open" => Ok(ProcessUserTaskStatus::Open),
        "completed" => Ok(ProcessUserTaskStatus::Completed),
        "cancelled" => Ok(ProcessUserTaskStatus::Cancelled),
        _ => bail!("invalid user task status"),
    }
}

fn current_version_model_on(
    conn: &Connection,
    definition_id: &str,
    version: u32,
) -> Result<ProcessModel> {
    let text: String = conn.query_row(
        "SELECT model_json FROM bpmn_versions WHERE definition_id=?1 AND version=?2",
        params![definition_id, version],
        |row| row.get(0),
    )?;
    parse(text)
}

fn tasks_on(
    conn: &Connection,
    actor: &ProcessActor,
    instance_id: &str,
    user_task_id: Option<&str>,
) -> Result<Vec<ProcessUserTask>> {
    let mut task_stmt = conn.prepare("SELECT user_task_id,node_id,name,assignee_user_id,kind,status,outputs_json,revision FROM bpmn_user_tasks WHERE instance_id=?1 AND (?2 IS NULL OR user_task_id=?2) ORDER BY created_at_ms,user_task_id")?;
    let task_rows = task_stmt
        .query_map(params![instance_id, user_task_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row_u64(row, 7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let user_tasks = task_rows
        .into_iter()
        .map(
            |(
                user_task_id,
                node_id,
                name,
                assignee_user_id,
                kind,
                status,
                outputs_json,
                revision,
            )| {
                let kind = task_kind_from_text(&kind)?;
                let status = task_status_from_text(&status)?;
                let can_complete =
                    status == ProcessUserTaskStatus::Open && assignee_user_id == actor.user_id;
                Ok(ProcessUserTask {
                    user_task_id,
                    node_id,
                    name,
                    assignee_user_id,
                    kind,
                    status,
                    outputs: parse(outputs_json)?,
                    revision,
                    can_complete,
                })
            },
        )
        .collect::<Result<Vec<_>>>()?;
    Ok(user_tasks)
}

fn task_summaries_on(
    conn: &Connection,
    actor: &ProcessActor,
    instance_id: &str,
) -> Result<Vec<ProcessUserTaskSummary>> {
    let mut stmt = conn.prepare("SELECT user_task_id,node_id,name,assignee_user_id,kind,status,revision FROM bpmn_user_tasks WHERE instance_id=?1 ORDER BY created_at_ms,user_task_id")?;
    let rows = stmt
        .query_map([instance_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row_u64(row, 6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(
            |(user_task_id, node_id, name, assignee_user_id, kind, status, revision)| {
                let status = task_status_from_text(&status)?;
                let can_complete =
                    status == ProcessUserTaskStatus::Open && assignee_user_id == actor.user_id;
                Ok(ProcessUserTaskSummary {
                    user_task_id,
                    node_id,
                    name,
                    assignee_user_id,
                    kind: task_kind_from_text(&kind)?,
                    status,
                    revision,
                    can_complete,
                })
            },
        )
        .collect()
}

fn instance_on(
    conn: &Connection,
    actor: &ProcessActor,
    instance_id: &str,
) -> Result<ProcessInstance> {
    let (definition_id, version, initiator_user_id, revision, status, variables_json, created_at_ms, updated_at_ms): (String,u32,String,u64,String,String,i64,i64) = conn.query_row(
        "SELECT definition_id,version,initiator_user_id,revision,status,variables_json,created_at_ms,updated_at_ms FROM bpmn_instances WHERE instance_id=?1 AND org_id=?2",
        params![instance_id, actor.org_id],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row_u64(row, 3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?)),
    ).context("process instance not found")?;
    let is_initiator = require_instance_reader(conn, actor, instance_id)?;
    let model = current_version_model_on(conn, &definition_id, version)?;
    let names: std::collections::HashMap<&str, &str> = model
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node.name.as_str()))
        .collect();
    let definition_name: String = conn.query_row(
        "SELECT name FROM bpmn_definitions WHERE definition_id=?1",
        [&definition_id],
        |row| row.get(0),
    )?;
    let mut active_nodes_stmt = conn.prepare("SELECT node_id FROM bpmn_tokens WHERE instance_id=?1 AND status IN ('ready','waiting','joining') ORDER BY created_at_ms,token_id")?;
    let active_node_ids = active_nodes_stmt
        .query_map([instance_id], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let user_tasks = task_summaries_on(conn, actor, instance_id)?;
    let mut incident_stmt = conn.prepare("SELECT incident_id,node_id,job_id,code,message,at_ms FROM bpmn_incidents WHERE instance_id=?1 AND resolved_at_ms IS NULL ORDER BY at_ms,incident_id")?;
    let incident_rows = incident_stmt
        .query_map([instance_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let incidents = incident_rows
        .into_iter()
        .map(|(incident_id, node_id, job_id, code, message, at_ms)| {
            let can_retry = if is_initiator {
                if let Some(id) = &job_id {
                    conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM bpmn_jobs WHERE job_id=?1 AND instance_id=?2 AND status IN ('error','completed'))",
                        params![id, instance_id],
                        |row| row.get::<_, bool>(0),
                    )?
                } else {
                    false
                }
            } else {
                false
            };
            Ok(ProcessIncident {
                incident_id,
                node_name: node_id
                    .as_ref()
                    .and_then(|id| names.get(id.as_str()).map(|name| (*name).to_string())),
                node_id,
                job_id,
                code,
                message,
                at_ms,
                can_retry,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let status = status_from_text(&status)?;
    let can_cancel = is_initiator
        && !matches!(
            status,
            ProcessInstanceStatus::Completed | ProcessInstanceStatus::Cancelled
        );
    let can_retry = incidents.iter().any(|incident| incident.can_retry);
    let instance = ProcessInstance {
        instance_id: instance_id.to_string(),
        definition_id,
        definition_name,
        initiator_user_id,
        version,
        revision,
        status,
        variables: parse(variables_json)?,
        active_node_ids,
        user_tasks,
        incidents,
        created_at_ms,
        updated_at_ms,
        can_cancel,
        can_retry,
    };
    ensure_instance_wire_budget(&instance)?;
    Ok(instance)
}

fn ensure_instance_wire_budget(instance: &ProcessInstance) -> Result<()> {
    let frame = tentaflow_protocol::cbor::encode(
        &tentaflow_protocol::message_body::MessageBody::ProcessBody(
            ProcessPayload::InstanceGetResponse {
                instance: instance.clone(),
            },
        ),
    )
    .map_err(anyhow::Error::msg)?;
    ensure!(
        frame.len() <= 900 * 1024,
        "process instance response exceeds the wire budget"
    );
    Ok(())
}

fn reproject_instance_on(
    conn: &Connection,
    actor: &ProcessActor,
    mut prior: ProcessInstance,
) -> Result<ProcessInstance> {
    let current = instance_on(conn, actor, &prior.instance_id)?;
    prior.can_cancel = current.can_cancel;
    prior.can_retry = current.can_retry;
    for task in &mut prior.user_tasks {
        task.can_complete = current
            .user_tasks
            .iter()
            .find(|current_task| current_task.user_task_id == task.user_task_id)
            .is_some_and(|current_task| current_task.can_complete);
    }
    for incident in &mut prior.incidents {
        incident.can_retry = current
            .incidents
            .iter()
            .find(|current_incident| current_incident.incident_id == incident.incident_id)
            .is_some_and(|current_incident| current_incident.can_retry);
    }
    ensure_instance_wire_budget(&prior)?;
    Ok(prior)
}

pub fn get_instance(
    pool: &DbPool,
    actor: &ProcessActor,
    instance_id: &str,
) -> Result<ProcessInstance> {
    read_snapshot(pool, |conn| {
        require_actor(conn, actor)?;
        instance_on(conn, actor, instance_id)
    })
}

pub fn get_user_task(
    pool: &DbPool,
    actor: &ProcessActor,
    instance_id: &str,
    user_task_id: &str,
) -> Result<ProcessUserTask> {
    read_snapshot(pool, |conn| {
        require_actor(conn, actor)?;
        let is_initiator = require_instance_reader(conn, actor, instance_id)?;
        let task = tasks_on(conn, actor, instance_id, Some(user_task_id))?
            .pop()
            .context("user task not found")?;
        ensure!(
            is_initiator || task.assignee_user_id == actor.user_id,
            "user task not found"
        );
        let frame = tentaflow_protocol::cbor::encode(
            &tentaflow_protocol::message_body::MessageBody::ProcessBody(
                ProcessPayload::UserTaskGetResponse { task: task.clone() },
            ),
        )
        .map_err(anyhow::Error::msg)?;
        ensure!(
            frame.len() <= 900 * 1024,
            "process user task detail exceeds the wire budget"
        );
        Ok(task)
    })
}

pub fn list_instances(
    pool: &DbPool,
    actor: &ProcessActor,
    definition_id: Option<&str>,
    offset: u32,
    limit: u32,
) -> Result<(Vec<ProcessInstanceSummary>, u32, bool)> {
    page(offset, limit)?;
    read_snapshot(pool, |conn| {
        require_actor(conn, actor)?;
        let where_sql=" FROM bpmn_instances i JOIN bpmn_definitions d ON d.definition_id=i.definition_id WHERE i.org_id=?1 AND (i.initiator_user_id=?2 OR EXISTS(SELECT 1 FROM bpmn_user_tasks t WHERE t.instance_id=i.instance_id AND t.assignee_user_id=?2)) AND (?3 IS NULL OR i.definition_id=?3)";
        let total: u32 = conn.query_row(
            &format!("SELECT COUNT(*){where_sql}"),
            params![actor.org_id, actor.user_id, definition_id],
            |row| row.get(0),
        )?;
        let sql=format!("SELECT i.instance_id,i.definition_id,d.name,i.initiator_user_id,i.version,i.revision,i.status,i.created_at_ms,i.updated_at_ms,EXISTS(SELECT 1 FROM bpmn_incidents x JOIN bpmn_jobs j ON j.job_id=x.job_id WHERE x.instance_id=i.instance_id AND x.resolved_at_ms IS NULL AND j.status IN ('error','completed')){where_sql} ORDER BY i.updated_at_ms DESC,i.instance_id DESC LIMIT ?4 OFFSET ?5");
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt
            .query_map(
                params![actor.org_id, actor.user_id, definition_id, limit, offset],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, u32>(4)?,
                        row_u64(row, 5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, i64>(7)?,
                        row.get::<_, i64>(8)?,
                        row.get::<_, bool>(9)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let instances = rows
            .into_iter()
            .map(
                |(
                    instance_id,
                    definition_id,
                    definition_name,
                    initiator_user_id,
                    version,
                    revision,
                    status,
                    created_at_ms,
                    updated_at_ms,
                    retryable,
                )| {
                    let status = status_from_text(&status)?;
                    let owns = initiator_user_id == actor.user_id;
                    Ok(ProcessInstanceSummary {
                        instance_id,
                        definition_id,
                        definition_name,
                        initiator_user_id,
                        version,
                        revision,
                        can_cancel: owns
                            && !matches!(
                                status,
                                ProcessInstanceStatus::Completed | ProcessInstanceStatus::Cancelled
                            ),
                        can_retry: owns && retryable,
                        status,
                        created_at_ms,
                        updated_at_ms,
                    })
                },
            )
            .collect::<Result<Vec<_>>>()?;
        Ok((instances, total, offset.saturating_add(limit) < total))
    })
}

pub fn list_events(
    pool: &DbPool,
    actor: &ProcessActor,
    instance_id: &str,
    after_seq: u64,
    limit: u32,
) -> Result<(Vec<ProcessEvent>, u64, bool)> {
    ensure!((1..=200).contains(&limit), "history limit must be 1..=200");
    read_snapshot(pool, |conn| {
        require_actor(conn, actor)?;
        require_instance_reader(conn, actor, instance_id)?;
        let (definition_id, version): (String, u32) = conn.query_row(
            "SELECT definition_id,version FROM bpmn_instances WHERE instance_id=?1",
            [instance_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let model = current_version_model_on(&conn, &definition_id, version)?;
        let names: std::collections::HashMap<&str, &str> = model
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node.name.as_str()))
            .collect();
        let mut stmt = conn.prepare("SELECT event_id,seq,at_ms,kind,node_id,actor_user_id,data_json FROM bpmn_events WHERE instance_id=?1 AND seq>?2 ORDER BY seq LIMIT ?3")?;
        let rows = stmt
            .query_map(
                params![instance_id, sql_integer(after_seq)?, limit],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row_u64(row, 1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut events = Vec::new();
        for (event_id, seq, at_ms, kind, node_id, actor_user_id, data_json) in rows {
            let node_name = node_id
                .as_ref()
                .and_then(|id| names.get(id.as_str()).map(|name| (*name).to_string()));
            events.push(ProcessEvent {
                event_id,
                seq,
                at_ms,
                kind,
                node_id,
                node_name,
                actor_user_id,
                data: parse(data_json)?,
            });
            let frame = tentaflow_protocol::cbor::encode(
                &tentaflow_protocol::message_body::MessageBody::ProcessBody(
                    ProcessPayload::HistoryResponse {
                        events: events.clone(),
                        next_seq: seq,
                        has_more: true,
                    },
                ),
            )
            .map_err(anyhow::Error::msg)?;
            if frame.len() > 900 * 1024 {
                events.pop();
                break;
            }
        }
        let next_seq = events.last().map_or(after_seq, |event| event.seq);
        let has_more: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM bpmn_events WHERE instance_id=?1 AND seq>?2)",
            params![instance_id, sql_integer(next_seq)?],
            |row| row.get(0),
        )?;
        Ok((events, next_seq, has_more))
    })
}

pub fn runtime_snapshot(
    pool: &DbPool,
    actor: &ProcessActor,
    instance_id: &str,
) -> Result<RuntimeSnapshot> {
    read_snapshot(pool, |conn| runtime_snapshot_on(conn, actor, instance_id))
}

fn runtime_snapshot_on(
    conn: &Connection,
    actor: &ProcessActor,
    instance_id: &str,
) -> Result<RuntimeSnapshot> {
    require_actor(conn, actor)?;
    let instance = instance_on(conn, actor, instance_id)?;
    let model = current_version_model_on(conn, &instance.definition_id, instance.version)?;
    let user_tasks = tasks_on(conn, actor, instance_id, None)?;
    let snapshots_json: String = conn.query_row(
        "SELECT service_snapshots_json FROM bpmn_versions WHERE definition_id=?1 AND version=?2",
        params![instance.definition_id, instance.version],
        |row| row.get(0),
    )?;
    let service_snapshots = parse(snapshots_json)?;
    let mut token_stmt = conn.prepare("SELECT token_id,node_id,arrival_edge_id,fork_stack_json,status FROM bpmn_tokens WHERE instance_id=?1 AND status NOT IN ('consumed','cancelled') ORDER BY created_at_ms,token_id")?;
    let token_rows = token_stmt
        .query_map([instance_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let tokens = token_rows
        .into_iter()
        .map(
            |(token_id, node_id, arrival_edge_id, fork_stack_json, status)| {
                Ok(ProcessToken {
                    token_id,
                    node_id,
                    arrival_edge_id,
                    fork_stack: parse(fork_stack_json)?,
                    status,
                })
            },
        )
        .collect::<Result<Vec<_>>>()?;
    let mut job_stmt = conn.prepare("SELECT job_id,node_id,token_id,input_json,status,attempt,fence,worker_id,lease_until_ms,result_json FROM bpmn_jobs WHERE instance_id=?1 ORDER BY created_at_ms,job_id")?;
    let job_rows = job_stmt
        .query_map([instance_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, u32>(5)?,
                row_u64(row, 6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, Option<String>>(9)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let jobs = job_rows
        .into_iter()
        .map(
            |(
                job_id,
                node_id,
                token_id,
                input_json,
                status,
                attempt,
                fence,
                worker_id,
                lease_until_ms,
                result_json,
            )| {
                Ok(ProcessJob {
                    job_id,
                    instance_id: instance_id.to_string(),
                    node_id,
                    token_id,
                    input: parse(input_json)?,
                    status,
                    attempt,
                    fence,
                    worker_id,
                    lease_until_ms,
                    result: result_json.map(parse).transpose()?,
                })
            },
        )
        .collect::<Result<Vec<_>>>()?;
    let mut receipt_stmt = conn.prepare("SELECT join_node_id,activation_id,branch_edge_id,token_id FROM bpmn_and_receipts WHERE instance_id=?1")?;
    let receipts = receipt_stmt
        .query_map([instance_id], |row| {
            Ok(AndReceipt {
                join_node_id: row.get(0)?,
                activation_id: row.get(1)?,
                branch_edge_id: row.get(2)?,
                token_id: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(token_stmt);
    drop(job_stmt);
    drop(receipt_stmt);
    Ok(RuntimeSnapshot {
        instance,
        model,
        user_tasks,
        tokens,
        jobs,
        receipts,
        service_snapshots,
    })
}

fn insert_event_on(
    tx: &Transaction<'_>,
    instance_id: &str,
    seq: u64,
    actor: Option<&str>,
    event: &PlannedEvent,
    at_ms: i64,
) -> Result<()> {
    ensure!(
        json(&event.data)?.len() <= 384 * 1024,
        "process event data exceeds 384 KiB"
    );
    let event_id = Uuid::new_v4().to_string();
    tx.execute(
        "INSERT INTO bpmn_events(event_id,instance_id,seq,at_ms,kind,node_id,actor_user_id,data_json) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
        params![event_id,instance_id,sql_integer(seq)?,at_ms,event.kind,event.node_id,actor,json(&event.data)?],
    )?;
    let org_id: String = tx.query_row(
        "SELECT org_id FROM bpmn_instances WHERE instance_id=?1",
        [instance_id],
        |row| row.get(0),
    )?;
    let details = serde_json::json!({
        "event_id": event_id,
        "seq": seq,
        "node_id": event.node_id,
    })
    .to_string();
    crate::db::repository::log_audit_scoped_tx(
        tx,
        actor,
        &format!("process.{}", event.kind),
        instance_id,
        "bpmn_instance",
        instance_id,
        Some(&details),
        "info",
        "unclassified",
        Some(&org_id),
        None,
    )?;
    Ok(())
}

fn check_active_incident_budget_on(conn: &Connection, instance_id: &str) -> Result<()> {
    let incident_bytes:i64=conn.query_row("SELECT COALESCE(SUM(length(message)),0) FROM bpmn_incidents WHERE instance_id=?1 AND resolved_at_ms IS NULL",[instance_id],|row|row.get(0))?;
    ensure!(
        incident_bytes <= 64 * 1024,
        "process active incidents exceed instance detail budget"
    );
    Ok(())
}

fn apply_plan_on(
    tx: &Transaction<'_>,
    instance_id: &str,
    actor_id: &str,
    expected_revision: u64,
    plan: &RuntimePlan,
    at_ms: i64,
) -> Result<()> {
    validate_variables(&plan.variables)?;
    let affected = tx.execute(
        "UPDATE bpmn_instances SET status=?1,variables_json=?2,revision=revision+1,updated_at_ms=?3 WHERE instance_id=?4 AND revision=?5 AND status NOT IN ('completed','cancelled')",
        params![status_text(&plan.status),json(&plan.variables)?,at_ms,instance_id,sql_incrementable(expected_revision)?],
    )?;
    ensure!(
        affected == 1,
        "process instance revision conflict or closed instance"
    );
    for token_id in &plan.consume_token_ids {
        let affected = tx.execute("UPDATE bpmn_tokens SET status='consumed' WHERE token_id=?1 AND instance_id=?2 AND status IN ('ready','waiting','joining')",params![token_id,instance_id])?;
        ensure!(affected == 1, "process token changed before transition");
    }
    for token in &plan.create_tokens {
        ensure!(
            matches!(token.status.as_str(), "ready" | "waiting" | "joining"),
            "invalid new token status"
        );
        tx.execute("INSERT INTO bpmn_tokens(token_id,instance_id,node_id,arrival_edge_id,fork_stack_json,status,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![token.token_id,instance_id,token.node_id,token.arrival_edge_id,json(&token.fork_stack)?,token.status,at_ms])?;
    }
    for receipt in &plan.remove_receipts {
        let affected=tx.execute("DELETE FROM bpmn_and_receipts WHERE instance_id=?1 AND join_node_id=?2 AND activation_id=?3 AND branch_edge_id=?4 AND token_id=?5",params![instance_id,receipt.join_node_id,receipt.activation_id,receipt.branch_edge_id,receipt.token_id])?;
        ensure!(
            affected == 1,
            "parallel join receipt changed before transition"
        );
    }
    for receipt in &plan.add_receipts {
        tx.execute("INSERT INTO bpmn_and_receipts(instance_id,join_node_id,activation_id,branch_edge_id,token_id,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6)",params![instance_id,receipt.join_node_id,receipt.activation_id,receipt.branch_edge_id,receipt.token_id,at_ms])?;
    }
    for task_id in &plan.complete_user_task_ids {
        let affected=tx.execute("UPDATE bpmn_user_tasks SET status='completed',revision=revision+1,updated_at_ms=?1 WHERE user_task_id=?2 AND instance_id=?3 AND status='open'",params![at_ms,task_id,instance_id])?;
        ensure!(affected == 1, "user task changed before transition");
    }
    for task in &plan.create_user_tasks {
        ensure!(
            task.status == ProcessUserTaskStatus::Open,
            "new user task must be open"
        );
        if task.kind == ProcessUserTaskKind::Verification {
            ensure!(
                json(&task.outputs)?.len() <= 384 * 1024 - 4096,
                "verification detail exceeds the process history budget"
            );
        } else {
            validate_output(&task.outputs)?;
        }
        let org_id: String = tx.query_row(
            "SELECT org_id FROM bpmn_instances WHERE instance_id=?1",
            [instance_id],
            |row| row.get(0),
        )?;
        require_actor(
            tx,
            &ProcessActor {
                org_id,
                user_id: task.assignee_user_id.clone(),
            },
        )?;
        tx.execute("INSERT INTO bpmn_user_tasks(user_task_id,instance_id,node_id,name,assignee_user_id,kind,status,outputs_json,revision,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,'open',?7,1,?8,?8)",params![task.user_task_id,instance_id,task.node_id,task.name,task.assignee_user_id,task_kind_text(&task.kind),json(&task.outputs)?,at_ms])?;
    }
    for job_id in &plan.complete_job_ids {
        let affected=tx.execute("UPDATE bpmn_jobs SET status='completed',updated_at_ms=?1 WHERE job_id=?2 AND instance_id=?3 AND status='running'",params![at_ms,job_id,instance_id])?;
        ensure!(affected == 1, "service job changed before transition");
    }
    for job in &plan.create_jobs {
        ensure!(
            job.instance_id == instance_id
                && job.status == "queued"
                && job.attempt == 0
                && job.fence == 0,
            "invalid queued service job"
        );
        tx.execute("INSERT INTO bpmn_jobs(job_id,instance_id,node_id,token_id,input_json,status,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,'queued',?6,?6)",params![job.job_id,instance_id,job.node_id,job.token_id,json(&job.input)?,at_ms])?;
    }
    for incident_id in &plan.resolve_incident_ids {
        let affected=tx.execute("UPDATE bpmn_incidents SET resolved_at_ms=?1 WHERE incident_id=?2 AND instance_id=?3 AND resolved_at_ms IS NULL",params![at_ms,incident_id,instance_id])?;
        ensure!(affected == 1, "process incident changed before transition");
    }
    for incident in &plan.add_incidents {
        ensure!(
            incident.message.len() <= 32 * 1024,
            "process incident message exceeds 32 KiB"
        );
        tx.execute("INSERT INTO bpmn_incidents(incident_id,instance_id,node_id,job_id,code,message,at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![incident.incident_id,instance_id,incident.node_id,incident.job_id,incident.code,incident_message(&incident.message),at_ms])?;
    }
    check_active_incident_budget_on(tx, instance_id)?;
    let next_seq: u64 = tx.query_row(
        "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
        [instance_id],
        |row| row_u64(row, 0),
    )?;
    for (index, event) in plan.events.iter().enumerate() {
        insert_event_on(
            tx,
            instance_id,
            next_seq
                .checked_add(u64::try_from(index)?)
                .context("process event sequence overflow")?,
            Some(actor_id),
            event,
            at_ms,
        )?;
    }
    Ok(())
}

pub fn replay_instance_command(
    pool: &DbPool,
    actor: &ProcessActor,
    stamp: &CommandStamp,
    instance_id: Option<&str>,
) -> Result<Option<ProcessInstance>> {
    read_snapshot(pool, |conn| {
        require_actor(conn, actor)?;
        if let Some(id) = instance_id {
            require_instance_reader(conn, actor, id)?;
        }
        ensure!(
            Uuid::parse_str(&stamp.command_id).is_ok(),
            "command_id must be a UUID"
        );
        let stored:Option<(String,String)>=conn.query_row("SELECT request_hash,result_json FROM bpmn_commands WHERE org_id=?1 AND actor_user_id=?2 AND command_id=?3",params![actor.org_id,actor.user_id,stamp.command_id],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
        if let Some((hash, result)) = stored {
            ensure!(
                hash == stamp.request_hash,
                "command_id was reused for a different request"
            );
            let prior: ProcessInstance = parse(result)?;
            require_instance_reader(conn, actor, &prior.instance_id)?;
            return Ok(Some(reproject_instance_on(conn, actor, prior)?));
        }
        Ok(None)
    })
}

pub fn start_instance(
    pool: &DbPool,
    actor: &ProcessActor,
    stamp: &CommandStamp,
    instance_id: &str,
    definition_id: &str,
    version: u32,
    initial_variables: &Value,
    plan: &RuntimePlan,
) -> Result<ProcessInstance> {
    validate_variables(initial_variables)?;
    ensure!(
        Uuid::parse_str(instance_id).is_ok(),
        "instance_id must be a UUID"
    );
    let at_ms = now_ms()?;
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    require_actor(&tx, actor)?;
    require_owner(&tx, actor, definition_id)?;
    if let Some(prior) = command_replay::<ProcessInstance>(&tx, actor, stamp)? {
        require_instance_reader(&tx, actor, &prior.instance_id)?;
        return reproject_instance_on(&tx, actor, prior);
    }
    let definition = definition_on(&tx, definition_id)?;
    ensure!(
        !definition.archived,
        "archived process cannot start new instances"
    );
    let model = current_version_model_on(&tx, definition_id, version)?;
    let mut vars = serde_json::to_value(&model.variables)?;
    for (key, value) in initial_variables
        .as_object()
        .context("process variables must be an object")?
    {
        vars.as_object_mut()
            .expect("model variables are object")
            .insert(key.clone(), value.clone());
    }
    validate_variables(&vars)?;
    ensure!(
        plan.variables == vars,
        "initial transition variables differ from requested inputs"
    );
    let snapshots_json: String = tx.query_row(
        "SELECT service_snapshots_json FROM bpmn_versions WHERE definition_id=?1 AND version=?2",
        params![definition_id, version],
        |row| row.get(0),
    )?;
    let snapshots: Vec<PinnedServiceSnapshot> = parse(snapshots_json)?;
    for snapshot in &snapshots {
        require_flow_current(&tx, actor, &snapshot.info.flow_id, None)?;
    }
    tx.execute("INSERT INTO bpmn_instances(instance_id,definition_id,version,org_id,initiator_user_id,revision,status,variables_json,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,1,'running',?6,?7,?7)",params![instance_id,definition_id,version,actor.org_id,actor.user_id,json(&vars)?,at_ms])?;
    apply_plan_on(&tx, instance_id, &actor.user_id, 1, plan, at_ms)?;
    let result = instance_on(&tx, actor, instance_id)?;
    store_command(&tx, actor, stamp, &result, at_ms)?;
    tx.commit()?;
    Ok(result)
}

pub fn apply_transition(
    pool: &DbPool,
    actor: &ProcessActor,
    instance_id: &str,
    expected_revision: u64,
    plan: &RuntimePlan,
) -> Result<ProcessInstance> {
    let at_ms = now_ms()?;
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    require_actor(&tx, actor)?;
    let initiator = require_instance_reader(&tx, actor, instance_id)?;
    ensure!(initiator, "only process initiator can advance the process");
    let instance = instance_on(&tx, actor, instance_id)?;
    let snapshots_json: String = tx.query_row(
        "SELECT service_snapshots_json FROM bpmn_versions WHERE definition_id=?1 AND version=?2",
        params![instance.definition_id, instance.version],
        |row| row.get(0),
    )?;
    let snapshots: Vec<PinnedServiceSnapshot> = parse(snapshots_json)?;
    for snapshot in &snapshots {
        require_flow_current(&tx, actor, &snapshot.info.flow_id, None)?;
    }
    apply_plan_on(
        &tx,
        instance_id,
        &actor.user_id,
        expected_revision,
        plan,
        at_ms,
    )?;
    let result = instance_on(&tx, actor, instance_id)?;
    tx.commit()?;
    Ok(result)
}

pub fn complete_user_task(
    pool: &DbPool,
    actor: &ProcessActor,
    stamp: &CommandStamp,
    instance_id: &str,
    user_task_id: &str,
    expected_revision: u64,
    outputs: &Value,
    approved: Option<bool>,
    plan: &RuntimePlan,
) -> Result<ProcessInstance> {
    validate_output(outputs)?;
    let at_ms = now_ms()?;
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    require_actor(&tx, actor)?;
    require_instance_reader(&tx, actor, instance_id)?;
    if let Some(prior) = command_replay::<ProcessInstance>(&tx, actor, stamp)? {
        require_instance_reader(&tx, actor, &prior.instance_id)?;
        return reproject_instance_on(&tx, actor, prior);
    }
    let (assignee,kind,status):(String,String,String)=tx.query_row("SELECT assignee_user_id,kind,status FROM bpmn_user_tasks WHERE user_task_id=?1 AND instance_id=?2",params![user_task_id,instance_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).context("user task not found")?;
    ensure!(
        assignee == actor.user_id && status == "open",
        "user task is not currently completable"
    );
    ensure!(
        (kind == "verification") == approved.is_some(),
        "verification decision must match user task kind"
    );
    ensure!(
        plan.complete_user_task_ids
            .iter()
            .any(|id| id == user_task_id),
        "completion plan does not consume the requested user task"
    );
    apply_plan_on(
        &tx,
        instance_id,
        &actor.user_id,
        expected_revision,
        plan,
        at_ms,
    )?;
    tx.execute(
        "UPDATE bpmn_user_tasks SET outputs_json=?1 WHERE user_task_id=?2",
        params![json(outputs)?, user_task_id],
    )?;
    let result = instance_on(&tx, actor, instance_id)?;
    store_command(&tx, actor, stamp, &result, at_ms)?;
    tx.commit()?;
    Ok(result)
}

pub fn cancel_instance(
    pool: &DbPool,
    actor: &ProcessActor,
    stamp: &CommandStamp,
    instance_id: &str,
    expected_revision: u64,
) -> Result<ProcessInstance> {
    let at_ms = now_ms()?;
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    require_actor(&tx, actor)?;
    let initiator = require_instance_reader(&tx, actor, instance_id)?;
    ensure!(initiator, "only process initiator may cancel");
    if let Some(prior) = command_replay::<ProcessInstance>(&tx, actor, stamp)? {
        return reproject_instance_on(&tx, actor, prior);
    }
    let changed=tx.execute("UPDATE bpmn_instances SET status='cancelled',revision=revision+1,updated_at_ms=?1 WHERE instance_id=?2 AND revision=?3 AND status NOT IN ('completed','cancelled')",params![at_ms,instance_id,sql_incrementable(expected_revision)?])?;
    ensure!(
        changed == 1,
        "process instance revision conflict or already closed"
    );
    tx.execute("UPDATE bpmn_tokens SET status='cancelled' WHERE instance_id=?1 AND status IN ('ready','waiting','joining')",[instance_id])?;
    tx.execute("UPDATE bpmn_user_tasks SET status='cancelled',revision=revision+1,updated_at_ms=?2 WHERE instance_id=?1 AND status='open'",params![instance_id,at_ms])?;
    tx.execute("UPDATE bpmn_jobs SET status='cancelled',fence=fence+1,lease_until_ms=NULL,updated_at_ms=?2 WHERE instance_id=?1 AND status IN ('queued','running')",params![instance_id,at_ms])?;
    let next_seq: u64 = tx.query_row(
        "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
        [instance_id],
        |row| row_u64(row, 0),
    )?;
    insert_event_on(
        &tx,
        instance_id,
        next_seq,
        Some(&actor.user_id),
        &PlannedEvent {
            kind: "cancelled".into(),
            node_id: None,
            data: Value::Null,
        },
        at_ms,
    )?;
    let result = instance_on(&tx, actor, instance_id)?;
    store_command(&tx, actor, stamp, &result, at_ms)?;
    tx.commit()?;
    Ok(result)
}

fn job_flow_id_on(
    conn: &Connection,
    definition_id: &str,
    version: u32,
    node_id: &str,
) -> Result<String> {
    let model = current_version_model_on(conn, definition_id, version)?;
    match model
        .nodes
        .iter()
        .find(|node| node.id == node_id)
        .map(|node| &node.kind)
    {
        Some(tentaflow_protocol::processes::ProcessNodeKind::ServiceTask { flow_id, .. }) => {
            Ok(flow_id.clone())
        }
        _ => bail!("service job references a non-service process node"),
    }
}

pub fn claim_job(pool: &DbPool, worker_id: &str, now_ms: i64) -> Result<Option<ClaimedProcessJob>> {
    ensure!(
        !worker_id.is_empty() && worker_id.len() <= 128,
        "invalid process worker ID"
    );
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    let queued:Option<(String,String,String,String,String,u32)>=tx.query_row(
        "SELECT j.job_id,j.instance_id,j.node_id,i.org_id,i.initiator_user_id,i.version FROM bpmn_jobs j JOIN bpmn_instances i ON i.instance_id=j.instance_id WHERE j.status='queued' AND i.status IN ('running','waiting') ORDER BY j.created_at_ms,j.job_id LIMIT 1",
        [],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
    ).optional()?;
    let Some((job_id, instance_id, node_id, org_id, user_id, version)) = queued else {
        return Ok(None);
    };
    let actor = ProcessActor { org_id, user_id };
    let definition_id: String = tx.query_row(
        "SELECT definition_id FROM bpmn_instances WHERE instance_id=?1",
        [&instance_id],
        |row| row.get(0),
    )?;
    let flow_id = job_flow_id_on(&tx, &definition_id, version, &node_id)?;
    if let Err(error) =
        require_actor(&tx, &actor).and_then(|_| require_flow_current(&tx, &actor, &flow_id, None))
    {
        let reason = bounded_failure_message(&error.to_string());
        tx.execute("UPDATE bpmn_jobs SET status='error',updated_at_ms=?1 WHERE job_id=?2 AND status='queued'",params![now_ms,job_id])?;
        tx.execute("UPDATE bpmn_instances SET status='incident',revision=revision+1,updated_at_ms=?1 WHERE instance_id=?2",params![now_ms,instance_id])?;
        let incident_id = Uuid::new_v4().to_string();
        tx.execute("INSERT INTO bpmn_incidents(incident_id,instance_id,node_id,job_id,code,message,at_ms) VALUES(?1,?2,?3,?4,'SOURCE_ACCESS_REVOKED',?5,?6)",params![incident_id,instance_id,node_id,job_id,incident_message(&reason),now_ms])?;
        let next_seq: u64 = tx.query_row(
            "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
            [&instance_id],
            |row| row_u64(row, 0),
        )?;
        insert_event_on(
            &tx,
            &instance_id,
            next_seq,
            None,
            &PlannedEvent {
                kind: "job_denied".into(),
                node_id: Some(node_id),
                data: serde_json::json!({"job_id":job_id,"incident_id":incident_id,"reason":reason}),
            },
            now_ms,
        )?;
        tx.commit()?;
        return Ok(None);
    }
    let affected=tx.execute("UPDATE bpmn_jobs SET status='running',attempt=attempt+1,fence=fence+1,worker_id=?1,lease_until_ms=?2,updated_at_ms=?3 WHERE job_id=?4 AND status='queued'",params![worker_id,now_ms+30_000,now_ms,job_id])?;
    ensure!(affected == 1, "process job changed before claim");
    let (attempt, fence): (u32, u64) = tx.query_row(
        "SELECT attempt,fence FROM bpmn_jobs WHERE job_id=?1",
        [&job_id],
        |row| Ok((row.get(0)?, row_u64(row, 1)?)),
    )?;
    let next_seq: u64 = tx.query_row(
        "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
        [&instance_id],
        |row| row_u64(row, 0),
    )?;
    insert_event_on(
        &tx,
        &instance_id,
        next_seq,
        Some(&actor.user_id),
        &PlannedEvent {
            kind: "service_claimed".into(),
            node_id: Some(node_id),
            data: serde_json::json!({"job_id":job_id,"attempt":attempt,"fence":fence}),
        },
        now_ms,
    )?;
    let snapshot = runtime_snapshot_on(&tx, &actor, &instance_id)?;
    let job = snapshot
        .jobs
        .iter()
        .find(|job| job.job_id == job_id)
        .cloned()
        .context("claimed process job disappeared")?;
    tx.commit()?;
    Ok(Some(ClaimedProcessJob {
        actor,
        job,
        snapshot,
    }))
}

pub fn renew_job_lease(
    pool: &DbPool,
    job_id: &str,
    attempt: u32,
    fence: u64,
    worker_id: &str,
    now_ms: i64,
) -> Result<bool> {
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    let owner:Option<(String,String,String,u32,String)>=tx.query_row("SELECT i.org_id,i.initiator_user_id,i.definition_id,i.version,j.node_id FROM bpmn_jobs j JOIN bpmn_instances i ON i.instance_id=j.instance_id WHERE j.job_id=?1",[job_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).optional()?;
    let Some((org_id, user_id, definition_id, version, node_id)) = owner else {
        return Ok(false);
    };
    let actor = ProcessActor { org_id, user_id };
    require_actor(&tx, &actor)?;
    let flow_id = job_flow_id_on(&tx, &definition_id, version, &node_id)?;
    require_flow_current(&tx, &actor, &flow_id, None)?;
    let changed=tx.execute("UPDATE bpmn_jobs SET lease_until_ms=?1,updated_at_ms=?2 WHERE job_id=?3 AND worker_id=?4 AND attempt=?5 AND fence=?6 AND status='running' AND lease_until_ms>=?2",params![now_ms+30_000,now_ms,job_id,worker_id,attempt,sql_integer(fence)?])?;
    tx.commit()?;
    Ok(changed == 1)
}

pub fn accept_job_result(
    pool: &DbPool,
    actor: &ProcessActor,
    job_id: &str,
    attempt: u32,
    fence: u64,
    worker_id: &str,
    result: &ActivityResult,
    expected_revision: u64,
    plan: &RuntimePlan,
) -> Result<ProcessInstance> {
    validate_output(&result.outputs)?;
    ensure!(
        json(result)?.len() <= 384 * 1024 - 4096,
        "activity result exceeds the process history budget"
    );
    let at_ms = now_ms()?;
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    require_actor(&tx, actor)?;
    let (instance_id,node_id,status,current_attempt,current_fence,current_worker,lease_until_ms,stored_result):(String,String,String,u32,u64,Option<String>,Option<i64>,Option<String>)=tx.query_row("SELECT instance_id,node_id,status,attempt,fence,worker_id,lease_until_ms,result_json FROM bpmn_jobs WHERE job_id=?1",[job_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row_u64(row,4)?,row.get(5)?,row.get(6)?,row.get(7)?))).context("process job not found")?;
    let instance = instance_on(&tx, actor, &instance_id)?;
    ensure!(
        instance.initiator_user_id == actor.user_id,
        "process job actor is not the initiator"
    );
    if status == "completed" || status == "error" {
        ensure!(
            current_attempt == attempt
                && current_fence == fence
                && stored_result.as_deref() == Some(json(result)?.as_str()),
            "service job result conflicts with accepted result"
        );
        return Ok(instance);
    }
    ensure!(
        status == "running"
            && current_attempt == attempt
            && current_fence == fence
            && current_worker.as_deref() == Some(worker_id)
            && lease_until_ms.is_some_and(|lease| lease >= at_ms),
        "service job fence is stale"
    );
    let flow_id = job_flow_id_on(&tx, &instance.definition_id, instance.version, &node_id)?;
    require_flow_current(&tx, actor, &flow_id, None)?;
    ensure!(
        plan.complete_job_ids.iter().any(|id| id == job_id),
        "result plan does not consume service job"
    );
    apply_plan_on(
        &tx,
        &instance_id,
        &actor.user_id,
        expected_revision,
        plan,
        at_ms,
    )?;
    let terminal = match result.outcome {
        tentaflow_protocol::processes::ActivityOutcome::Completed
        | tentaflow_protocol::processes::ActivityOutcome::NeedsHuman => "completed",
        tentaflow_protocol::processes::ActivityOutcome::Error => "error",
        tentaflow_protocol::processes::ActivityOutcome::Cancelled => "cancelled",
    };
    tx.execute("UPDATE bpmn_jobs SET status=?1,result_json=?2,lease_until_ms=NULL,updated_at_ms=?3 WHERE job_id=?4",params![terminal,json(result)?,at_ms,job_id])?;
    let result_instance = instance_on(&tx, actor, &instance_id)?;
    tx.commit()?;
    Ok(result_instance)
}

pub fn retry_job(
    pool: &DbPool,
    actor: &ProcessActor,
    stamp: &CommandStamp,
    instance_id: &str,
    job_id: &str,
    expected_revision: u64,
) -> Result<ProcessInstance> {
    let at_ms = now_ms()?;
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    require_actor(&tx, actor)?;
    let instance = instance_on(&tx, actor, instance_id)?;
    ensure!(
        instance.initiator_user_id == actor.user_id,
        "only process initiator may retry"
    );
    if let Some(prior) = command_replay::<ProcessInstance>(&tx, actor, stamp)? {
        return reproject_instance_on(&tx, actor, prior);
    }
    ensure!(
        instance.revision == expected_revision
            && instance.status == ProcessInstanceStatus::Incident,
        "process instance revision conflict or not incident"
    );
    sql_incrementable(expected_revision)?;
    let node_id:String=tx.query_row("SELECT j.node_id FROM bpmn_jobs j JOIN bpmn_incidents x ON x.job_id=j.job_id AND x.instance_id=j.instance_id AND x.resolved_at_ms IS NULL WHERE j.job_id=?1 AND j.instance_id=?2 AND j.status IN ('error','completed')",params![job_id,instance_id],|row|row.get(0)).context("retryable job not found")?;
    let flow_id = job_flow_id_on(&tx, &instance.definition_id, instance.version, &node_id)?;
    require_flow_current(&tx, actor, &flow_id, None)?;
    let affected=tx.execute("UPDATE bpmn_jobs SET status='queued',fence=fence+1,worker_id=NULL,lease_until_ms=NULL,result_json=NULL,updated_at_ms=?1 WHERE job_id=?2 AND instance_id=?3 AND status IN ('error','completed')",params![at_ms,job_id,instance_id])?;
    ensure!(affected == 1, "retryable job changed");
    tx.execute("UPDATE bpmn_user_tasks SET status='cancelled',revision=revision+1,updated_at_ms=?1 WHERE instance_id=?2 AND node_id=?3 AND kind='verification' AND status='open'",params![at_ms,instance_id,node_id])?;
    tx.execute("UPDATE bpmn_incidents SET resolved_at_ms=?1 WHERE instance_id=?2 AND job_id=?3 AND resolved_at_ms IS NULL",params![at_ms,instance_id,job_id])?;
    tx.execute("UPDATE bpmn_instances SET status='running',revision=revision+1,updated_at_ms=?1 WHERE instance_id=?2",params![at_ms,instance_id])?;
    let next_seq: u64 = tx.query_row(
        "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
        [instance_id],
        |row| row_u64(row, 0),
    )?;
    insert_event_on(
        &tx,
        instance_id,
        next_seq,
        Some(&actor.user_id),
        &PlannedEvent {
            kind: "job_retried".into(),
            node_id: Some(node_id),
            data: serde_json::json!({"job_id":job_id}),
        },
        at_ms,
    )?;
    let result = instance_on(&tx, actor, instance_id)?;
    store_command(&tx, actor, stamp, &result, at_ms)?;
    tx.commit()?;
    Ok(result)
}

pub fn recover_jobs(pool: &DbPool, worker_id: Option<&str>, now_ms: i64) -> Result<u32> {
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    let mut stmt=tx.prepare("SELECT j.job_id,j.instance_id,j.node_id FROM bpmn_jobs j WHERE j.status='running' AND (?1 IS NULL OR j.worker_id=?1) ORDER BY j.created_at_ms,j.job_id")?;
    let rows = stmt
        .query_map([worker_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    for (job_id, instance_id, node_id) in &rows {
        tx.execute("UPDATE bpmn_jobs SET status='error',fence=fence+1,worker_id=NULL,lease_until_ms=NULL,updated_at_ms=?1 WHERE job_id=?2 AND status='running'",params![now_ms,job_id])?;
        tx.execute("UPDATE bpmn_instances SET status='incident',revision=revision+1,updated_at_ms=?1 WHERE instance_id=?2 AND status NOT IN ('completed','cancelled')",params![now_ms,instance_id])?;
        let incident_id = Uuid::new_v4().to_string();
        tx.execute("INSERT INTO bpmn_incidents(incident_id,instance_id,node_id,job_id,code,message,at_ms) VALUES(?1,?2,?3,?4,'INTERRUPTED','The worker stopped before the external effect was confirmed',?5)",params![incident_id,instance_id,node_id,job_id,now_ms])?;
        let next_seq: u64 = tx.query_row(
            "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
            [instance_id],
            |row| row_u64(row, 0),
        )?;
        insert_event_on(
            &tx,
            instance_id,
            next_seq,
            None,
            &PlannedEvent {
                kind: "job_interrupted".into(),
                node_id: Some(node_id.clone()),
                data: serde_json::json!({"job_id":job_id,"incident_id":incident_id}),
            },
            now_ms,
        )?;
    }
    tx.commit()?;
    Ok(u32::try_from(rows.len())?)
}

pub fn fail_job(
    pool: &DbPool,
    job_id: &str,
    attempt: u32,
    fence: u64,
    worker_id: &str,
    code: &str,
    message: &str,
    now_ms: i64,
) -> Result<bool> {
    ensure!(
        !code.is_empty() && code.len() <= 128,
        "invalid process job failure"
    );
    let message = bounded_failure_message(message);
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    let row:Option<(String,String)>=tx.query_row("SELECT instance_id,node_id FROM bpmn_jobs WHERE job_id=?1 AND status='running' AND attempt=?2 AND fence=?3 AND worker_id=?4",params![job_id,attempt,sql_integer(fence)?,worker_id],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
    let Some((instance_id, node_id)) = row else {
        return Ok(false);
    };
    let changed=tx.execute("UPDATE bpmn_jobs SET status='error',fence=fence+1,worker_id=NULL,lease_until_ms=NULL,updated_at_ms=?1 WHERE job_id=?2 AND status='running' AND attempt=?3 AND fence=?4 AND worker_id=?5",params![now_ms,job_id,attempt,sql_integer(fence)?,worker_id])?;
    if changed == 0 {
        return Ok(false);
    }
    let active=tx.execute("UPDATE bpmn_instances SET status='incident',revision=revision+1,updated_at_ms=?1 WHERE instance_id=?2 AND status NOT IN ('completed','cancelled')",params![now_ms,instance_id])?;
    if active == 1 {
        let incident_id = Uuid::new_v4().to_string();
        tx.execute("INSERT INTO bpmn_incidents(incident_id,instance_id,node_id,job_id,code,message,at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![incident_id,instance_id,node_id,job_id,code,incident_message(&message),now_ms])?;
        let next_seq: u64 = tx.query_row(
            "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
            [&instance_id],
            |row| row_u64(row, 0),
        )?;
        insert_event_on(
            &tx,
            &instance_id,
            next_seq,
            None,
            &PlannedEvent {
                kind: "job_failed".into(),
                node_id: Some(node_id),
                data: serde_json::json!({"job_id":job_id,"incident_id":incident_id,"code":code,"message":message}),
            },
            now_ms,
        )?;
    }
    tx.commit()?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;
    use tentaflow_protocol::processes::{
        ProcessNode, ProcessNodeKind, ProcessSequenceFlow, ProcessUserTaskStatus,
    };

    use super::*;

    fn fixture() -> (DbPool, ProcessActor) {
        let db = crate::db::init(Path::new(":memory:")).expect("initialize process database");
        {
            let conn = db.write().expect("database writer");
            for (id, active) in [
                ("process-owner", 1),
                ("process-other", 1),
                ("process-unrelated", 1),
                ("process-inactive", 0),
            ] {
                conn.execute(
                    "INSERT INTO user_accounts(id,username,password_hash,is_active) VALUES(?1,?1,'x',?2)",
                    params![id, active],
                )
                .expect("create user");
                conn.execute(
                    "INSERT INTO org_memberships(org_id,user_id,role_id,granted_at,granted_by) VALUES('org-default',?1,'role-org-admin',datetime('now'),'test')",
                    [id],
                )
                .expect("create organization membership");
            }
        }
        (
            db,
            ProcessActor {
                org_id: "org-default".into(),
                user_id: "process-owner".into(),
            },
        )
    }

    fn stamp(label: &str) -> CommandStamp {
        CommandStamp {
            command_id: Uuid::new_v4().to_string(),
            request_hash: request_hash(&label).expect("hash command"),
        }
    }

    fn user_model(assignee: &str) -> ProcessModel {
        let mut model = starter_model();
        model.nodes.insert(
            1,
            ProcessNode {
                id: "User_1".into(),
                name: "Review".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: Some(assignee.into()),
                    output_mapping: Default::default(),
                },
            },
        );
        model.sequence_flows = vec![
            ProcessSequenceFlow {
                id: "Flow_1".into(),
                source_id: "Start_1".into(),
                target_id: "User_1".into(),
                condition: None,
            },
            ProcessSequenceFlow {
                id: "Flow_2".into(),
                source_id: "User_1".into(),
                target_id: "End_1".into(),
                condition: None,
            },
        ];
        model
    }

    #[test]
    fn private_definition_publishes_pinned_version_and_rejects_other_user() {
        let (db, actor) = fixture();
        let model = starter_model();
        let definition = save_definition(
            &db,
            &actor,
            &stamp("create"),
            None,
            0,
            "Private process",
            "",
            &model,
        )
        .unwrap();
        let other = ProcessActor {
            org_id: actor.org_id.clone(),
            user_id: "process-other".into(),
        };
        assert!(get_definition(&db, &other, &definition.definition_id).is_err());
        let publication = stamp("publish");
        let (published, version) = publish_definition(
            &db,
            &actor,
            &publication,
            &definition.definition_id,
            definition.draft_revision,
            &[],
        )
        .unwrap();
        assert_eq!(published.published_version, Some(1));
        assert_eq!(version.model, model);
        assert_eq!(
            replay_definition_publication(&db, &actor, &publication, &definition.definition_id)
                .unwrap(),
            Some((published, version))
        );
        assert!(replay_definition_publication(
            &db,
            &other,
            &publication,
            &definition.definition_id
        )
        .is_err());
    }

    #[test]
    fn user_completion_uses_instance_revision_and_replay_does_not_duplicate_history() {
        let (db, actor) = fixture();
        let participant = ProcessActor {
            org_id: actor.org_id.clone(),
            user_id: "process-other".into(),
        };
        let model = user_model(&participant.user_id);
        let definition = save_definition(
            &db,
            &actor,
            &stamp("create"),
            None,
            0,
            "Review process",
            "",
            &model,
        )
        .unwrap();
        publish_definition(
            &db,
            &actor,
            &stamp("publish"),
            &definition.definition_id,
            definition.draft_revision,
            &[],
        )
        .unwrap();
        let instance_id = Uuid::new_v4().to_string();
        let start_plan =
            super::super::runtime::plan_start(&model, &instance_id, &actor.user_id, json!({}))
                .unwrap();
        let waiting = start_instance(
            &db,
            &actor,
            &stamp("start"),
            &instance_id,
            &definition.definition_id,
            1,
            &json!({}),
            &start_plan,
        )
        .unwrap();
        assert_eq!(waiting.status, ProcessInstanceStatus::Waiting);
        assert_eq!(waiting.user_tasks[0].status, ProcessUserTaskStatus::Open);
        let task_id = waiting.user_tasks[0].user_task_id.clone();
        let unrelated = ProcessActor {
            org_id: actor.org_id.clone(),
            user_id: "process-unrelated".into(),
        };
        assert!(get_instance(&db, &unrelated, &instance_id).is_err());
        assert!(get_user_task(&db, &unrelated, &instance_id, &task_id).is_err());
        let snapshot = runtime_snapshot(&db, &participant, &instance_id).unwrap();
        let outputs = json!(["approved", {"case_id": "C-1"}]);
        let plan = super::super::runtime::plan_user_completion(&snapshot, &task_id, &outputs, None)
            .unwrap();
        let completion = stamp("complete");
        assert!(complete_user_task(
            &db,
            &participant,
            &completion,
            &instance_id,
            &task_id,
            waiting.revision - 1,
            &outputs,
            None,
            &plan,
        )
        .is_err());
        let before_events: i64 = db
            .read()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1",
                [&instance_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            get_instance(&db, &participant, &instance_id)
                .unwrap()
                .revision,
            waiting.revision
        );
        let completed = complete_user_task(
            &db,
            &participant,
            &completion,
            &instance_id,
            &task_id,
            waiting.revision,
            &outputs,
            None,
            &plan,
        )
        .unwrap();
        assert_eq!(completed.status, ProcessInstanceStatus::Completed);
        assert_eq!(
            get_user_task(&db, &participant, &instance_id, &task_id)
                .unwrap()
                .outputs,
            outputs
        );
        assert!(serde_json::to_value(&completed).unwrap()["user_tasks"][0]
            .get("outputs")
            .is_none());
        let after_completion_events: i64 = db
            .read()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1",
                [&instance_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(after_completion_events > before_events);
        let replayed = complete_user_task(
            &db,
            &participant,
            &completion,
            &instance_id,
            &task_id,
            waiting.revision,
            &outputs,
            None,
            &plan,
        )
        .unwrap();
        assert_eq!(replayed, completed);
        let after_events: i64 = db
            .read()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1",
                [&instance_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(after_events, after_completion_events);
        let mut cursor = 0;
        let mut paged_events = 0;
        loop {
            let (page, next, has_more) =
                list_events(&db, &participant, &instance_id, cursor, 1).unwrap();
            assert_eq!(page.len(), 1);
            assert!(next > cursor);
            paged_events += 1;
            cursor = next;
            if !has_more {
                break;
            }
        }
        assert_eq!(paged_events, after_events);
        let audit_events: i64 = db
            .read()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM audit_log WHERE resource_type='bpmn_instance' AND resource_id=?1 AND action LIKE 'process.%'",
                [&instance_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(audit_events, after_events);
        db.write()
            .unwrap()
            .execute(
                "UPDATE user_accounts SET is_active=0 WHERE id=?1",
                [&participant.user_id],
            )
            .unwrap();
        assert!(
            replay_instance_command(&db, &participant, &completion, Some(&instance_id)).is_err()
        );
    }

    #[test]
    fn inactive_assignee_rejects_start_without_instance_or_command() {
        let (db, actor) = fixture();
        let model = user_model("process-inactive");
        let definition = save_definition(
            &db,
            &actor,
            &stamp("create"),
            None,
            0,
            "Inactive assignee",
            "",
            &model,
        )
        .unwrap();
        publish_definition(
            &db,
            &actor,
            &stamp("publish"),
            &definition.definition_id,
            definition.draft_revision,
            &[],
        )
        .unwrap();
        let instance_id = Uuid::new_v4().to_string();
        let plan =
            super::super::runtime::plan_start(&model, &instance_id, &actor.user_id, json!({}))
                .unwrap();
        let command = stamp("start inactive");
        assert!(start_instance(
            &db,
            &actor,
            &command,
            &instance_id,
            &definition.definition_id,
            1,
            &json!({}),
            &plan
        )
        .is_err());
        let conn = db.read().unwrap();
        let instances: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bpmn_instances WHERE instance_id=?1",
                [&instance_id],
                |row| row.get(0),
            )
            .unwrap();
        let commands: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bpmn_commands WHERE command_id=?1",
                [&command.command_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!((instances, commands), (0, 0));
    }

    #[test]
    fn long_unicode_incident_preview_points_to_full_history() {
        let full = "Łódź—".repeat(300);
        let preview = incident_message(&full);
        assert!(preview.len() <= 512);
        assert!(preview.is_char_boundary(preview.len()));
        assert!(preview.ends_with("… (see process history)"));
        assert!(full.starts_with(preview.trim_end_matches("… (see process history)")));
        assert_eq!(incident_message("brief failure"), "brief failure");
        let oversized = "Łódź—".repeat(10_000);
        let bounded = bounded_failure_message(&oversized);
        assert!(bounded.len() < 9 * 1024);
        assert!(bounded.contains("failure reason exceeded 32 KiB"));
        assert!(bounded.contains("sha256="));
    }

    #[test]
    fn sqlite_counters_reject_negative_and_out_of_range_values() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(
            conn.query_row("SELECT 0", [], |row| row_u64(row, 0))
                .unwrap(),
            0
        );
        assert_eq!(
            conn.query_row("SELECT ?1", [i64::MAX], |row| row_u64(row, 0))
                .unwrap(),
            i64::MAX as u64
        );
        assert!(conn
            .query_row("SELECT -1", [], |row| row_u64(row, 0))
            .is_err());
        assert_eq!(sql_integer(i64::MAX as u64).unwrap(), i64::MAX);
        assert!(sql_integer((i64::MAX as u64) + 1).is_err());
        assert!(sql_incrementable(i64::MAX as u64).is_err());

        let (db, actor) = fixture();
        let definition = create_starter_definition(&db, &actor, &stamp("create"), "Counter")
            .expect("create real definition");
        let writer = db.write().unwrap();
        assert!(writer
            .execute(
                "UPDATE bpmn_definitions SET draft_revision=-1 WHERE definition_id=?1",
                [&definition.definition_id],
            )
            .is_err());
        assert!(writer
            .execute(
                "UPDATE bpmn_definitions SET published_version=-1 WHERE definition_id=?1",
                [&definition.definition_id],
            )
            .is_err());
        writer
            .execute(
                "UPDATE bpmn_definitions SET draft_revision=?1 WHERE definition_id=?2",
                params![i64::MAX, definition.definition_id],
            )
            .unwrap();
        drop(writer);
        assert!(save_definition(
            &db,
            &actor,
            &stamp("overflow"),
            Some(&definition.definition_id),
            i64::MAX as u64,
            "Counter edited",
            "",
            &definition.model,
        )
        .is_err());
        assert_eq!(
            get_definition(&db, &actor, &definition.definition_id)
                .unwrap()
                .draft_revision,
            i64::MAX as u64
        );
    }

    #[test]
    fn revision_event_and_fence_overflow_roll_back_real_cancellation() {
        use super::super::runtime::test_support::{
            flow, graph, service_model, start_model, Fixture,
        };
        use tentaflow_protocol::processes::ActivityVerification;

        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("counter", None));
        let instance = start_model(
            &fixture,
            &service_model(&flow_id, ActivityVerification::Human),
        );
        assert_eq!(instance.status, ProcessInstanceStatus::Running);
        let event_count: i64 = fixture
            .db
            .read()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1",
                [&instance.instance_id],
                |row| row.get(0),
            )
            .unwrap();
        fixture
            .db
            .write()
            .unwrap()
            .execute(
                "UPDATE bpmn_instances SET revision=?1 WHERE instance_id=?2",
                params![i64::MAX, instance.instance_id],
            )
            .unwrap();
        assert!(cancel_instance(
            &fixture.db,
            &fixture.owner,
            &stamp("revision-overflow"),
            &instance.instance_id,
            i64::MAX as u64,
        )
        .is_err());
        assert_eq!(
            get_instance(&fixture.db, &fixture.owner, &instance.instance_id)
                .unwrap()
                .status,
            ProcessInstanceStatus::Running
        );
        fixture
            .db
            .write()
            .unwrap()
            .execute(
                "UPDATE bpmn_instances SET revision=?1 WHERE instance_id=?2",
                params![
                    sql_integer(instance.revision).unwrap(),
                    instance.instance_id
                ],
            )
            .unwrap();
        let (event_id, original_seq): (String, i64) = fixture
            .db
            .read()
            .unwrap()
            .query_row(
                "SELECT event_id,seq FROM bpmn_events WHERE instance_id=?1 ORDER BY seq DESC LIMIT 1",
                [&instance.instance_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(fixture
            .db
            .write()
            .unwrap()
            .execute(
                "UPDATE bpmn_events SET seq=-1 WHERE event_id=?1",
                [&event_id],
            )
            .is_err());
        fixture
            .db
            .write()
            .unwrap()
            .execute(
                "UPDATE bpmn_events SET seq=?1 WHERE event_id=?2",
                params![i64::MAX, event_id],
            )
            .unwrap();
        assert!(cancel_instance(
            &fixture.db,
            &fixture.owner,
            &stamp("seq-overflow"),
            &instance.instance_id,
            instance.revision,
        )
        .is_err());
        assert_eq!(
            get_instance(&fixture.db, &fixture.owner, &instance.instance_id)
                .unwrap()
                .status,
            ProcessInstanceStatus::Running
        );
        fixture
            .db
            .write()
            .unwrap()
            .execute(
                "UPDATE bpmn_events SET seq=?1 WHERE event_id=?2",
                params![original_seq, event_id],
            )
            .unwrap();

        let job_id: String = fixture
            .db
            .read()
            .unwrap()
            .query_row(
                "SELECT job_id FROM bpmn_jobs WHERE instance_id=?1",
                [&instance.instance_id],
                |row| row.get(0),
            )
            .unwrap();
        assert!(fixture
            .db
            .write()
            .unwrap()
            .execute("UPDATE bpmn_jobs SET fence=-1 WHERE job_id=?1", [&job_id],)
            .is_err());
        fixture
            .db
            .write()
            .unwrap()
            .execute(
                "UPDATE bpmn_jobs SET fence=?1 WHERE job_id=?2",
                params![i64::MAX, job_id],
            )
            .unwrap();
        assert!(cancel_instance(
            &fixture.db,
            &fixture.owner,
            &stamp("fence-overflow"),
            &instance.instance_id,
            instance.revision,
        )
        .is_err());
        let after = get_instance(&fixture.db, &fixture.owner, &instance.instance_id).unwrap();
        assert_eq!(after.status, ProcessInstanceStatus::Running);
        assert_eq!(after.revision, instance.revision);
        assert_eq!(after.active_node_ids, instance.active_node_ids);
        let persisted_events: i64 = fixture
            .db
            .read()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1",
                [&instance.instance_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(persisted_events, event_count);
        let job_fence: i64 = fixture
            .db
            .read()
            .unwrap()
            .query_row(
                "SELECT fence FROM bpmn_jobs WHERE job_id=?1",
                [&job_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(job_fence, i64::MAX);
        let command_count: i64 = fixture
            .db
            .read()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM bpmn_commands WHERE actor_user_id=?1",
                [&fixture.owner.user_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            command_count, 3,
            "failed cancellation must not record a command"
        );
    }
}
