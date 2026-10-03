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
    ProcessNodeKind, ProcessPayload, ProcessUserTask, ProcessUserTaskKind, ProcessUserTaskStatus,
    ProcessUserTaskSummary, ProcessVersion, ProcessVersionSummary, ProcessTimerKind,
    ProcessTimerSpec, ProcessTimerStatus, ProcessTimerSummary,
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

#[derive(Debug)]
pub(super) struct ProcessAuthorityDenied(pub &'static str);

impl std::fmt::Display for ProcessAuthorityDenied {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for ProcessAuthorityDenied {}

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessTimer {
    pub timer_id: String,
    pub org_id: String,
    pub definition_id: String,
    pub version: u32,
    pub node_id: String,
    pub kind: ProcessTimerKind,
    pub instance_id: Option<String>,
    pub token_id: Option<String>,
    pub rule: ProcessTimerSpec,
    pub total_firings: Option<u32>,
    pub timezone: String,
    pub anchor_at_ms: i64,
    pub due_at_ms: Option<i64>,
    pub occurrence: u64,
    pub revision: u64,
    pub status: ProcessTimerStatus,
    pub last_reason: Option<String>,
    pub next_check_at_ms: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct DueTimer {
    pub timer_id: String,
    pub kind: ProcessTimerKind,
    pub org_id: String,
    pub definition_id: String,
    pub version: u32,
    pub instance_id: Option<String>,
    pub token_id: Option<String>,
    pub occurrence: u64,
    pub revision: u64,
    pub due_at_ms: i64,
}

#[derive(Debug, Clone)]
pub enum TimerSnapshot {
    Start {
        actor: ProcessActor,
        timer: ProcessTimer,
        version: ProcessVersion,
    },
    Catch {
        actor: ProcessActor,
        timer: ProcessTimer,
        snapshot: RuntimeSnapshot,
    },
    Boundary {
        actor: ProcessActor,
        timer: ProcessTimer,
        snapshot: RuntimeSnapshot,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerUpdate {
    pub timer_id: String,
    pub expected_revision: u64,
    pub fired_occurrence: Option<u64>,
    pub occurrence: u64,
    pub due_at_ms: Option<i64>,
    pub status: ProcessTimerStatus,
    pub last_reason: Option<String>,
    pub next_check_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct RuntimeSnapshot {
    pub org_id: String,
    pub instance: ProcessInstance,
    pub model: ProcessModel,
    pub user_tasks: Vec<ProcessUserTask>,
    pub tokens: Vec<ProcessToken>,
    pub jobs: Vec<ProcessJob>,
    pub receipts: Vec<AndReceipt>,
    pub service_snapshots: Vec<PinnedServiceSnapshot>,
    pub timers: Vec<ProcessTimer>,
    pub boundary_incidents: Vec<BoundaryTimerIncident>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedEvent {
    pub kind: String,
    pub node_id: Option<String>,
    pub data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePlan {
    pub start_instance_id: Option<String>,
    pub consume_token_ids: Vec<String>,
    pub cancel_token_ids: Vec<String>,
    pub create_tokens: Vec<ProcessToken>,
    pub add_receipts: Vec<AndReceipt>,
    pub remove_receipts: Vec<AndReceipt>,
    pub create_user_tasks: Vec<ProcessUserTask>,
    pub complete_user_task_ids: Vec<String>,
    pub cancel_user_task_ids: Vec<String>,
    pub create_jobs: Vec<ProcessJob>,
    pub complete_job_ids: Vec<String>,
    pub cancel_job_ids: Vec<String>,
    pub add_incidents: Vec<ProcessIncident>,
    pub resolve_incident_ids: Vec<String>,
    pub variables: Value,
    pub status: ProcessInstanceStatus,
    pub events: Vec<PlannedEvent>,
    pub create_timers: Vec<ProcessTimer>,
    pub timer_updates: Vec<TimerUpdate>,
}

impl RuntimePlan {
    pub fn initial(variables: Value) -> Self {
        Self {
            start_instance_id: None,
            consume_token_ids: Vec::new(),
            cancel_token_ids: Vec::new(),
            create_tokens: Vec::new(),
            add_receipts: Vec::new(),
            remove_receipts: Vec::new(),
            create_user_tasks: Vec::new(),
            complete_user_task_ids: Vec::new(),
            cancel_user_task_ids: Vec::new(),
            create_jobs: Vec::new(),
            complete_job_ids: Vec::new(),
            cancel_job_ids: Vec::new(),
            add_incidents: Vec::new(),
            resolve_incident_ids: Vec::new(),
            variables,
            status: ProcessInstanceStatus::Running,
            events: Vec::new(),
            create_timers: Vec::new(),
            timer_updates: Vec::new(),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CancelledJobClaim {
    pub job_id: String,
    pub attempt: u32,
    pub fence: u64,
    pub worker_id: String,
}

#[derive(Debug)]
pub struct TimerFireOutcome {
    pub instance: ProcessInstance,
    pub cancelled_claims: Vec<CancelledJobClaim>,
}

#[derive(Debug, Clone)]
pub struct BoundaryTimerIncident {
    pub timer_id: String,
    pub token_id: String,
    pub incident_id: String,
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

pub(super) fn timer_reason(reason: &str) -> Result<String> {
    ensure!(!reason.is_empty(), "timer failure reason cannot be empty");
    let normalized = reason.chars().map(|character| {
        if character.is_control() { ' ' } else { character }
    }).collect::<String>();
    if normalized.len() <= 512 {
        return Ok(normalized);
    }
    let suffix = format!(
        "… (see process history; reason bytes={}, sha256={})",
        reason.len(), hex::encode(Sha256::digest(reason.as_bytes())),
    );
    let mut summary = String::new();
    for character in normalized.chars() {
        if summary.len() + character.len_utf8() + suffix.len() > 512 {
            break;
        }
        summary.push(character);
    }
    summary.push_str(&suffix);
    Ok(summary)
}

fn timer_kind_text(kind: &ProcessTimerKind) -> &'static str {
    match kind {
        ProcessTimerKind::Start => "start",
        ProcessTimerKind::Catch => "catch",
        ProcessTimerKind::Boundary => "boundary",
    }
}

fn timer_kind_from_text(value: &str) -> Result<ProcessTimerKind> {
    match value {
        "start" => Ok(ProcessTimerKind::Start),
        "catch" => Ok(ProcessTimerKind::Catch),
        "boundary" => Ok(ProcessTimerKind::Boundary),
        _ => bail!("unknown process timer kind {value}"),
    }
}

fn timer_status_text(status: &ProcessTimerStatus) -> &'static str {
    match status {
        ProcessTimerStatus::Pending => "pending",
        ProcessTimerStatus::Fired => "fired",
        ProcessTimerStatus::Cancelled => "cancelled",
        ProcessTimerStatus::Archived => "archived",
        ProcessTimerStatus::Blocked => "blocked",
        ProcessTimerStatus::Missed => "missed",
        ProcessTimerStatus::Error => "error",
    }
}

fn timer_status_from_text(value: &str) -> Result<ProcessTimerStatus> {
    match value {
        "pending" => Ok(ProcessTimerStatus::Pending),
        "fired" => Ok(ProcessTimerStatus::Fired),
        "cancelled" => Ok(ProcessTimerStatus::Cancelled),
        "archived" => Ok(ProcessTimerStatus::Archived),
        "blocked" => Ok(ProcessTimerStatus::Blocked),
        "missed" => Ok(ProcessTimerStatus::Missed),
        "error" => Ok(ProcessTimerStatus::Error),
        _ => bail!("unknown process timer status {value}"),
    }
}

fn timer_on(conn: &Connection, timer_id: &str) -> Result<ProcessTimer> {
    let (org_id, definition_id, version, node_id, kind, instance_id, token_id, rule_json,
        timezone, anchor_at_ms, due_at_ms, occurrence, revision, status, last_reason,
        next_check_at_ms, created_at_ms, updated_at_ms): (
        String, String, u32, String, String, Option<String>, Option<String>, String,
        String, i64, Option<i64>, u64, u64, String, Option<String>, i64, i64, i64,
    ) = conn.query_row(
        "SELECT org_id,definition_id,version,node_id,kind,instance_id,token_id,rule_json,timezone,anchor_at_ms,due_at_ms,occurrence,revision,status,last_reason,next_check_at_ms,created_at_ms,updated_at_ms FROM bpmn_timers WHERE timer_id=?1",
        [timer_id],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?,row.get(10)?,row_u64(row,11)?,row_u64(row,12)?,row.get(13)?,row.get(14)?,row.get(15)?,row.get(16)?,row.get(17)?)),
    ).context("process timer not found")?;
    let rule: ProcessTimerSpec = parse(rule_json)?;
    let total_firings = match &rule {
        ProcessTimerSpec::Cycle { total_firings, .. }
        | ProcessTimerSpec::Daily { total_firings, .. } => *total_firings,
        _ => Some(1),
    };
    Ok(ProcessTimer {
        timer_id: timer_id.to_string(), org_id, definition_id, version, node_id,
        kind: timer_kind_from_text(&kind)?, instance_id, token_id, rule, total_firings,
        timezone, anchor_at_ms, due_at_ms, occurrence, revision,
        status: timer_status_from_text(&status)?, last_reason, next_check_at_ms,
        created_at_ms, updated_at_ms,
    })
}

fn timer_summary(timer: &ProcessTimer, model: &ProcessModel) -> Result<ProcessTimerSummary> {
    let node_name = model.nodes.iter().find(|node| node.id == timer.node_id)
        .context("persisted timer node is absent from pinned process model")?.name.clone();
    let total_firings = match &timer.rule {
        ProcessTimerSpec::Cycle { total_firings, .. }
        | ProcessTimerSpec::Daily { total_firings, .. } => *total_firings,
        _ => None,
    };
    Ok(ProcessTimerSummary {
        timer_id: timer.timer_id.clone(), node_id: timer.node_id.clone(), node_name,
        kind: timer.kind.clone(), status: timer.status.clone(), due_at_ms: timer.due_at_ms,
        timezone: timer.timezone.clone(), occurrence: timer.occurrence, total_firings,
        last_reason: timer.last_reason.clone(),
        attached_to_id: if timer.kind == ProcessTimerKind::Boundary {
            timer_activation_node(model, timer)?.map(str::to_owned)
        } else {
            None
        },
        working_time: super::calendar::working_time_summary(
            &timer.rule,
            model.calendar_pin.as_ref(),
            timer.due_at_ms,
        )?,
    })
}

fn timer_ids_on(conn: &Connection, instance_id: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT timer_id FROM bpmn_timers WHERE instance_id=?1 ORDER BY created_at_ms,timer_id")?;
    let ids = stmt.query_map([instance_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(ids)
}

fn timer_activation_node<'a>(
    model: &'a ProcessModel,
    timer: &ProcessTimer,
) -> Result<Option<&'a str>> {
    let node = model
        .nodes
        .iter()
        .find(|node| node.id == timer.node_id)
        .context("process timer node is absent from its pinned model")?;
    match (&timer.kind, &node.kind) {
        (ProcessTimerKind::Start, ProcessNodeKind::TimerStart { timer: rule })
            if rule == &timer.rule =>
        {
            Ok(None)
        }
        (ProcessTimerKind::Catch, ProcessNodeKind::TimerCatch { timer: rule })
            if rule == &timer.rule =>
        {
            Ok(Some(&node.id))
        }
        (
            ProcessTimerKind::Boundary,
            ProcessNodeKind::BoundaryTimer {
                attached_to_id,
                timer: rule,
                ..
            },
        ) if rule == &timer.rule => {
            let activity = model
                .nodes
                .iter()
                .find(|node| node.id == *attached_to_id)
                .context("boundary timer attached activity is missing")?;
            ensure!(
                matches!(
                    &activity.kind,
                    ProcessNodeKind::UserTask { .. } | ProcessNodeKind::ServiceTask { .. }
                ),
                "boundary timer attachment is not an activity"
            );
            Ok(Some(attached_to_id))
        }
        _ => bail!("process timer rule does not match its pinned node"),
    }
}

fn timer_activation_live_on(conn: &Connection, timer: &ProcessTimer) -> Result<bool> {
    let model = current_version_model_on(conn, &timer.definition_id, timer.version)?;
    let Some(node_id) = timer_activation_node(&model, timer)? else {
        return Ok(true);
    };
    let instance_id = timer
        .instance_id
        .as_deref()
        .context("activity timer lacks instance")?;
    let token_id = timer
        .token_id
        .as_deref()
        .context("activity timer lacks waiting token")?;
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM bpmn_tokens t JOIN bpmn_instances i ON i.instance_id=t.instance_id WHERE t.token_id=?1 AND t.instance_id=?2 AND t.node_id=?3 AND t.status='waiting' AND i.definition_id=?4 AND i.version=?5 AND i.org_id=?6 AND i.status NOT IN ('completed','cancelled'))",
        params![token_id,instance_id,node_id,timer.definition_id,timer.version,timer.org_id],
        |row| row.get(0),
    )?)
}

fn boundary_incidents_on(
    conn: &Connection,
    instance_id: &str,
) -> Result<Vec<BoundaryTimerIncident>> {
    let mut statement = conn.prepare(
        "SELECT t.timer_id,t.token_id,x.incident_id FROM bpmn_timers t JOIN bpmn_events e ON e.instance_id=t.instance_id AND e.kind='timer_error' AND json_extract(e.data_json,'$.timer_id')=t.timer_id JOIN bpmn_incidents x ON x.incident_id=json_extract(e.data_json,'$.incident_id') AND x.instance_id=t.instance_id AND x.node_id=t.node_id AND x.code='TIMER_ERROR' AND x.resolved_at_ms IS NULL WHERE t.instance_id=?1 AND t.kind='boundary' AND t.status='error' AND json_extract(e.data_json,'$.attached_token_id')=t.token_id",
    )?;
    let incidents = statement
        .query_map([instance_id], |row| {
            Ok(BoundaryTimerIncident {
                timer_id: row.get(0)?,
                token_id: row.get(1)?,
                incident_id: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(incidents)
}

fn insert_timer_on(tx: &Transaction<'_>, timer: &ProcessTimer) -> Result<()> {
    ensure!(
        timer.revision == 1 && timer.occurrence > 0
            && ((timer.status == ProcessTimerStatus::Pending && timer.due_at_ms.is_some())
                || ((timer.kind == ProcessTimerKind::Boundary
                    || (timer.kind == ProcessTimerKind::Catch
                        && matches!(&timer.rule, ProcessTimerSpec::WorkingDuration { .. })))
                    && timer.status == ProcessTimerStatus::Error
                    && timer.due_at_ms.is_none()
                    && timer.last_reason.is_some())),
        "new process timer must have a pending due slot or an activity arming error"
    );
    let total_firings = match &timer.rule {
        ProcessTimerSpec::Cycle { total_firings, .. }
        | ProcessTimerSpec::Daily { total_firings, .. } => *total_firings,
        _ => Some(1),
    };
    ensure!(
        timer.total_firings == total_firings,
        "process timer count differs from its literal rule"
    );
    let model = current_version_model_on(tx, &timer.definition_id, timer.version)?;
    ensure!(
        model.timer_timezone.as_deref() == Some(timer.timezone.as_str()),
        "process timer timezone differs from pinned model"
    );
    timer_activation_node(&model, timer)?;
    let actual_org: String = tx.query_row(
        "SELECT org_id FROM bpmn_definitions WHERE definition_id=?1",
        [&timer.definition_id],
        |row| row.get(0),
    )?;
    ensure!(actual_org == timer.org_id, "process timer organization mismatch");
    if timer.kind != ProcessTimerKind::Start {
        ensure!(
            timer_activation_live_on(tx, timer)?,
            "activity timer does not match a waiting token"
        );
    } else {
        ensure!(
            timer.instance_id.is_none() && timer.token_id.is_none(),
            "start timer cannot have an instance token"
        );
    }
    tx.execute(
        "INSERT INTO bpmn_timers(timer_id,org_id,definition_id,version,node_id,kind,instance_id,token_id,rule_json,timezone,anchor_at_ms,due_at_ms,occurrence,revision,status,last_reason,next_check_at_ms,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
        params![timer.timer_id,timer.org_id,timer.definition_id,timer.version,timer.node_id,timer_kind_text(&timer.kind),timer.instance_id,timer.token_id,json(&timer.rule)?,timer.timezone,timer.anchor_at_ms,timer.due_at_ms,sql_integer(timer.occurrence)?,sql_integer(timer.revision)?,timer_status_text(&timer.status),timer.last_reason,timer.next_check_at_ms,timer.created_at_ms,timer.updated_at_ms],
    )?;
    Ok(())
}

fn update_timer_on(tx: &Transaction<'_>, update: &TimerUpdate, at_ms: i64) -> Result<()> {
    ensure!(update.occurrence > 0, "timer occurrence must be positive");
    if let Some(fired) = update.fired_occurrence {
        ensure!(fired > 0, "fired timer occurrence must be positive");
        if update.status == ProcessTimerStatus::Pending {
            ensure!(
                update.occurrence == fired.checked_add(1).context("timer occurrence overflow")?
                    && update.due_at_ms.is_some(),
                "repeating timer must advance to the next due occurrence"
            );
        } else {
            ensure!(
                update.status == ProcessTimerStatus::Fired
                    && update.occurrence == fired && update.due_at_ms.is_none(),
                "terminal timer must retain its last fired occurrence"
            );
        }
    }
    let changed = tx.execute(
        "UPDATE bpmn_timers SET occurrence=?1,due_at_ms=?2,status=?3,last_reason=?4,next_check_at_ms=?5,revision=revision+1,updated_at_ms=?6 WHERE timer_id=?7 AND revision=?8 AND status IN ('pending','blocked','archived')",
        params![sql_integer(update.occurrence)?,update.due_at_ms,timer_status_text(&update.status),update.last_reason,update.next_check_at_ms,at_ms,update.timer_id,sql_incrementable(update.expected_revision)?],
    )?;
    ensure!(changed == 1, "process timer changed before transition");
    Ok(())
}

fn due_candidate_matches(timer: &ProcessTimer, candidate: &DueTimer) -> bool {
    timer.timer_id == candidate.timer_id
        && timer.kind == candidate.kind
        && timer.org_id == candidate.org_id
        && timer.definition_id == candidate.definition_id
        && timer.version == candidate.version
        && timer.instance_id == candidate.instance_id
        && timer.token_id == candidate.token_id
        && timer.occurrence == candidate.occurrence
        && timer.revision == candidate.revision
        && timer.due_at_ms == Some(candidate.due_at_ms)
        && matches!(timer.status, ProcessTimerStatus::Pending | ProcessTimerStatus::Blocked)
}

pub fn due_timers(pool: &DbPool, at_ms: i64, limit: u32) -> Result<Vec<DueTimer>> {
    ensure!((1..=32).contains(&limit), "timer drain limit must be 1..=32");
    read_snapshot(pool, |conn| {
        let mut stmt = conn.prepare("SELECT timer_id FROM bpmn_timers WHERE status IN ('pending','blocked') AND due_at_ms<=?1 AND next_check_at_ms<=?1 ORDER BY due_at_ms,timer_id LIMIT ?2")?;
        let ids = stmt.query_map(params![at_ms, limit], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.into_iter().map(|id| {
            let timer = timer_on(conn, &id)?;
            Ok(DueTimer {
                timer_id: id, kind: timer.kind, org_id: timer.org_id,
                definition_id: timer.definition_id, version: timer.version,
                instance_id: timer.instance_id, token_id: timer.token_id,
                occurrence: timer.occurrence, revision: timer.revision,
                due_at_ms: timer.due_at_ms.context("due timer has no due instant")?,
            })
        }).collect()
    })
}

pub fn timer_snapshot(pool: &DbPool, candidate: &DueTimer) -> Result<TimerSnapshot> {
    read_snapshot(pool, |conn| {
        let timer = timer_on(conn, &candidate.timer_id)?;
        ensure!(due_candidate_matches(&timer, candidate), "timer candidate changed");
        if !timer_current_authority_on(conn, &timer)? {
            return Err(ProcessAuthorityDenied("timer authority is no longer current").into());
        }
        let owner_id: String = if timer.kind == ProcessTimerKind::Start {
            conn.query_row(
                "SELECT owner_user_id FROM bpmn_definitions WHERE definition_id=?1 AND org_id=?2",
                params![timer.definition_id,timer.org_id],
                |row| row.get(0),
            )?
        } else {
            conn.query_row(
                "SELECT initiator_user_id FROM bpmn_instances WHERE instance_id=?1 AND org_id=?2",
                params![timer.instance_id,timer.org_id],
                |row| row.get(0),
            )?
        };
        let actor = ProcessActor { org_id: timer.org_id.clone(), user_id: owner_id };
        require_actor(conn, &actor)?;
        if timer.kind == ProcessTimerKind::Start {
            require_owner(conn, &actor, &timer.definition_id)?;
            let version = version_on(conn, &timer.definition_id, timer.version)?;
            Ok(TimerSnapshot::Start { actor, timer, version })
        } else {
            let instance_id = timer.instance_id.as_deref().context("catch timer lacks instance")?;
            let snapshot = runtime_snapshot_on(conn, &actor, instance_id)?;
            if timer.kind == ProcessTimerKind::Boundary {
                Ok(TimerSnapshot::Boundary {
                    actor,
                    timer,
                    snapshot,
                })
            } else {
                Ok(TimerSnapshot::Catch { actor, timer, snapshot })
            }
        }
    })
}

fn timer_current_authority_on(conn: &Connection, timer: &ProcessTimer) -> Result<bool> {
    if timer.kind != ProcessTimerKind::Start && !timer_activation_live_on(conn, timer)? {
        return Ok(false);
    }
    let user_id: Option<String> = if timer.kind == ProcessTimerKind::Start {
        conn.query_row(
            "SELECT owner_user_id FROM bpmn_definitions WHERE definition_id=?1 AND org_id=?2 AND archived=0 AND published_version=?3",
            params![timer.definition_id,timer.org_id,timer.version],
            |row| row.get(0),
        ).optional()?
    } else {
        conn.query_row(
            "SELECT initiator_user_id FROM bpmn_instances WHERE instance_id=?1 AND org_id=?2 AND definition_id=?3 AND version=?4 AND status NOT IN ('completed','cancelled')",
            params![timer.instance_id,timer.org_id,timer.definition_id,timer.version],
            |row| row.get(0),
        ).optional()?
    };
    let Some(user_id) = user_id else { return Ok(false); };
    let actor = ProcessActor { org_id: timer.org_id.clone(), user_id };
    if !actor_active_on(conn, &actor)? {
        return Ok(false);
    }
    if timer.kind == ProcessTimerKind::Start
        && !actor_owns_on(conn, &actor, &timer.definition_id)?
    {
        return Ok(false);
    }
    let model = current_version_model_on(conn, &timer.definition_id, timer.version)?;
    for node in &model.nodes {
        if let tentaflow_protocol::processes::ProcessNodeKind::UserTask {
            assignee_user_id: Some(assignee), ..
        } = &node.kind {
            if !actor_active_on(conn, &ProcessActor {
                org_id: timer.org_id.clone(), user_id: assignee.clone(),
            })? {
                return Ok(false);
            }
        }
    }
    let snapshots_json: String = conn.query_row(
        "SELECT service_snapshots_json FROM bpmn_versions WHERE definition_id=?1 AND version=?2",
        params![timer.definition_id,timer.version],
        |row| row.get(0),
    )?;
    for snapshot in parse::<Vec<PinnedServiceSnapshot>>(snapshots_json)? {
        if let Err(error) = require_flow_current(conn, &actor, &snapshot.info.flow_id, None) {
            if error.downcast_ref::<ProcessAuthorityDenied>().is_some() {
                return Ok(false);
            }
            return Err(error);
        }
    }
    Ok(true)
}

pub fn record_timer_blocked(
    pool: &DbPool,
    candidate: &DueTimer,
    reason: &str,
    at_ms: i64,
) -> Result<bool> {
    ensure!(!reason.is_empty(), "timer failure reason cannot be empty");
    let full_reason = bounded_failure_message(reason);
    let next_check = at_ms.checked_add(60_000).context("timer retry horizon overflow")?;
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    let timer = timer_on(&tx, &candidate.timer_id)?;
    if !due_candidate_matches(&timer, candidate)
        || timer.next_check_at_ms > at_ms
        || candidate.due_at_ms > at_ms
    {
        return Ok(false);
    }
    if timer.kind != ProcessTimerKind::Start && !timer_activation_live_on(&tx, &timer)? {
        return Ok(false);
    }
    if timer_current_authority_on(&tx, &timer)? {
        return Ok(false);
    }
    let summary = if timer.kind == ProcessTimerKind::Start {
        full_reason.clone()
    } else {
        timer_reason(reason)?
    };
    let reason_changed = timer.last_reason.as_deref() != Some(summary.as_str());
    let changed = tx.execute(
        "UPDATE bpmn_timers SET status='blocked',last_reason=?1,next_check_at_ms=?2,revision=revision+1,updated_at_ms=?3 WHERE timer_id=?4 AND revision=?5 AND occurrence=?6 AND due_at_ms=?7 AND status IN ('pending','blocked')",
        params![summary,next_check,at_ms,candidate.timer_id,sql_incrementable(candidate.revision)?,sql_integer(candidate.occurrence)?,candidate.due_at_ms],
    )?;
    if changed == 0 {
        return Ok(false);
    }
    if reason_changed {
        if let Some(instance_id) = &timer.instance_id {
            let next_seq: u64 = tx.query_row(
                "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
                [instance_id],
                |row| row_u64(row, 0),
            )?;
            let model = current_version_model_on(&tx, &timer.definition_id, timer.version)?;
            insert_event_on(&tx, instance_id, next_seq, None, &PlannedEvent {
                kind: "timer_blocked".into(),
                node_id: Some(timer.node_id.clone()),
                data: serde_json::json!({"timer_id":timer.timer_id,"kind":timer.kind,"reason":full_reason,"due_at_ms":candidate.due_at_ms,"next_check_at_ms":next_check,"attached_to_id":if timer.kind == ProcessTimerKind::Boundary { timer_activation_node(&model, &timer)? } else { None },"attached_token_id":timer.token_id}),
            }, at_ms)?;
        } else {
            crate::db::repository::log_audit_scoped_tx(
                &tx, None, "process.timer_blocked", &timer.timer_id,
                "bpmn_timer", &timer.timer_id,
                Some(&json(&serde_json::json!({"reason":full_reason,"due_at_ms":candidate.due_at_ms,"next_check_at_ms":next_check}))?),
                "warning", "unclassified", Some(&timer.org_id), None,
            )?;
        }
    }
    tx.commit()?;
    Ok(true)
}

pub fn record_timer_failed(
    pool: &DbPool,
    candidate: &DueTimer,
    reason: &str,
    at_ms: i64,
) -> Result<bool> {
    ensure!(!reason.is_empty(), "timer failure reason cannot be empty");
    let full_reason = bounded_failure_message(reason);
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    let timer = timer_on(&tx, &candidate.timer_id)?;
    if !due_candidate_matches(&timer, candidate)
        || timer.next_check_at_ms > at_ms
        || candidate.due_at_ms > at_ms
    {
        return Ok(false);
    }
    if !timer_current_authority_on(&tx, &timer)? {
        return Ok(false);
    }
    if timer.kind != ProcessTimerKind::Start && !timer_activation_live_on(&tx, &timer)? {
        return Ok(false);
    }
    let summary = if timer.kind == ProcessTimerKind::Start {
        full_reason.clone()
    } else {
        timer_reason(reason)?
    };
    let changed = tx.execute(
        "UPDATE bpmn_timers SET status='error',last_reason=?1,next_check_at_ms=?2,revision=revision+1,updated_at_ms=?2 WHERE timer_id=?3 AND revision=?4 AND occurrence=?5 AND due_at_ms=?6 AND status IN ('pending','blocked')",
        params![summary,at_ms,candidate.timer_id,sql_incrementable(candidate.revision)?,sql_integer(candidate.occurrence)?,candidate.due_at_ms],
    )?;
    if changed == 0 {
        return Ok(false);
    }
    if let Some(instance_id) = &timer.instance_id {
        let changed_instance = tx.execute(
            "UPDATE bpmn_instances SET status='incident',revision=revision+1,updated_at_ms=?1 WHERE instance_id=?2 AND status NOT IN ('completed','cancelled') AND revision < 9223372036854775807",
            params![at_ms,instance_id],
        )?;
        ensure!(changed_instance == 1, "catch timer instance closed during failure recording");
        let incident_id = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO bpmn_incidents(incident_id,instance_id,node_id,job_id,code,message,at_ms) VALUES(?1,?2,?3,NULL,'TIMER_ERROR',?4,?5)",
            params![incident_id,instance_id,timer.node_id,incident_message(&full_reason),at_ms],
        )?;
        check_active_incident_budget_on(&tx, instance_id)?;
        let next_seq: u64 = tx.query_row(
            "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
            [instance_id],
            |row| row_u64(row, 0),
        )?;
        let model = current_version_model_on(&tx, &timer.definition_id, timer.version)?;
        let mut event = PlannedEvent {
            kind: "timer_error".into(),
            node_id: Some(timer.node_id.clone()),
            data: serde_json::json!({"timer_id":timer.timer_id,"kind":timer.kind,"incident_id":incident_id,"reason":full_reason,"due_at_ms":candidate.due_at_ms,"attached_to_id":if timer.kind == ProcessTimerKind::Boundary { timer_activation_node(&model, &timer)? } else { None },"attached_token_id":timer.token_id}),
        };
        if let Some(working_time) = super::calendar::working_time_summary(
            &timer.rule,
            model.calendar_pin.as_ref(),
            timer.due_at_ms,
        )? {
            event.data["working_time"] = serde_json::to_value(working_time)?;
            event.data["timezone"] = serde_json::json!(timer.timezone);
        }
        insert_event_on(&tx, instance_id, next_seq, None, &event, at_ms)?;
    } else {
        crate::db::repository::log_audit_scoped_tx(
            &tx, None, "process.timer_error", &timer.timer_id,
            "bpmn_timer", &timer.timer_id,
            Some(&json(&serde_json::json!({"reason":full_reason,"due_at_ms":candidate.due_at_ms}))?),
            "error", "unclassified", Some(&timer.org_id), None,
        )?;
    }
    tx.commit()?;
    Ok(true)
}

fn validate_boundary_plan_on(
    tx: &Transaction<'_>,
    timer: &ProcessTimer,
    plan: &RuntimePlan,
) -> Result<()> {
    let model = current_version_model_on(tx, &timer.definition_id, timer.version)?;
    let node = model
        .nodes
        .iter()
        .find(|node| node.id == timer.node_id)
        .context("boundary node is missing")?;
    let ProcessNodeKind::BoundaryTimer {
        cancel_activity,
        attached_to_id,
        ..
    } = &node.kind
    else {
        bail!("timer node is not a boundary");
    };
    let instance_id = timer
        .instance_id
        .as_deref()
        .context("boundary timer lacks instance")?;
    let token_id = timer
        .token_id
        .as_deref()
        .context("boundary timer lacks activation")?;
    ensure!(
        plan.events.iter().any(|event| event.kind == "timer_fired"
            && event.node_id.as_deref() == Some(timer.node_id.as_str())
            && event.data["attached_token_id"].as_str() == Some(token_id)
            && event.data["attached_to_id"].as_str() == Some(attached_to_id.as_str())
            && event.data["cancel_activity"].as_bool() == Some(*cancel_activity)),
        "boundary firing lacks its actual attachment facts"
    );
    if !cancel_activity {
        ensure!(
            plan.timer_updates.len() == 1
                && plan.cancel_token_ids.is_empty()
                && plan.cancel_user_task_ids.is_empty()
                && plan.cancel_job_ids.is_empty()
                && plan.resolve_incident_ids.is_empty(),
            "noninterrupting boundary cannot cancel its original activity"
        );
        return Ok(());
    }
    ensure!(
        plan.cancel_token_ids == [token_id.to_string()],
        "interrupting boundary must cancel its exact activation"
    );
    let equal_ids = |actual: &[String], expected: &[String]| {
        actual.len() == expected.len()
            && actual.iter().all(|id| expected.contains(id))
            && actual.iter().collect::<HashSet<_>>().len() == actual.len()
    };
    let mut task_stmt = tx.prepare("SELECT user_task_id FROM bpmn_user_tasks WHERE instance_id=?1 AND token_id=?2 AND status='open'")?;
    let task_ids = task_stmt
        .query_map(params![instance_id, token_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    ensure!(
        equal_ids(&plan.cancel_user_task_ids, &task_ids),
        "boundary interruption does not cancel the exact open tasks"
    );
    let mut job_stmt = tx.prepare("SELECT job_id FROM bpmn_jobs WHERE instance_id=?1 AND token_id=?2 AND status IN ('queued','running','error')")?;
    let job_ids = job_stmt
        .query_map(params![instance_id, token_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    ensure!(
        equal_ids(&plan.cancel_job_ids, &job_ids),
        "boundary interruption does not cancel the exact active service job"
    );
    let mut sibling_stmt = tx.prepare("SELECT timer_id FROM bpmn_timers WHERE instance_id=?1 AND token_id=?2 AND kind='boundary' AND timer_id<>?3 AND status IN ('pending','blocked')")?;
    let siblings = sibling_stmt
        .query_map(params![instance_id, token_id, timer.timer_id], |row| {
            row.get(0)
        })?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    let expected_updates = siblings
        .iter()
        .chain(std::iter::once(&timer.timer_id))
        .cloned()
        .collect::<Vec<_>>();
    ensure!(
        equal_ids(
            &plan
                .timer_updates
                .iter()
                .map(|update| update.timer_id.clone())
                .collect::<Vec<_>>(),
            &expected_updates
        ),
        "boundary interruption must disarm the complete current sibling set"
    );
    for sibling in siblings {
        let actual = timer_on(tx, &sibling)?;
        let update = plan
            .timer_updates
            .iter()
            .find(|update| update.timer_id == sibling)
            .context("sibling cancellation is missing")?;
        ensure!(
            update.expected_revision == actual.revision
                && update.occurrence == actual.occurrence
                && update.fired_occurrence.is_none()
                && update.due_at_ms.is_none()
                && update.status == ProcessTimerStatus::Cancelled
                && update.last_reason.as_deref() == Some("sibling_interrupted"),
            "sibling cancellation does not match its actual timer"
        );
        ensure!(
            plan.events
                .iter()
                .any(|event| event.kind == "timer_cancelled"
                    && event.data["timer_id"].as_str() == Some(sibling.as_str())
                    && event.data["winning_timer_id"].as_str() == Some(timer.timer_id.as_str())),
            "sibling cancellation lacks its winning timer history"
        );
    }
    let mut incident_ids = boundary_incidents_on(tx, instance_id)?
        .into_iter()
        .filter(|incident| incident.token_id == token_id)
        .map(|incident| incident.incident_id)
        .collect::<Vec<_>>();
    let mut incident_stmt = tx.prepare("SELECT x.incident_id FROM bpmn_incidents x JOIN bpmn_jobs j ON j.job_id=x.job_id AND j.instance_id=x.instance_id WHERE x.instance_id=?1 AND j.token_id=?2 AND x.resolved_at_ms IS NULL")?;
    incident_ids.extend(
        incident_stmt
            .query_map(params![instance_id, token_id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?,
    );
    incident_ids.sort();
    incident_ids.dedup();
    ensure!(
        equal_ids(&plan.resolve_incident_ids, &incident_ids),
        "boundary interruption resolves an unrelated incident or omits its activation incident"
    );
    Ok(())
}

pub fn fire_timer(
    pool: &DbPool,
    candidate: &DueTimer,
    actor: &ProcessActor,
    expected_instance_revision: Option<u64>,
    plan: &RuntimePlan,
    at_ms: i64,
) -> Result<Option<TimerFireOutcome>> {
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    let timer = timer_on(&tx, &candidate.timer_id)?;
    if !due_candidate_matches(&timer, candidate)
        || timer.next_check_at_ms > at_ms
        || candidate.due_at_ms > at_ms
    {
        return Ok(None);
    }
    if timer.kind != ProcessTimerKind::Start && !timer_activation_live_on(&tx, &timer)? {
        return Ok(None);
    }
    if let Some(instance_id) = timer.instance_id.as_deref() {
        let expected =
            expected_instance_revision.context("activity timer needs instance revision")?;
        let current: Option<(u64,String)> = tx.query_row(
            "SELECT revision,status FROM bpmn_instances WHERE instance_id=?1 AND org_id=?2 AND definition_id=?3 AND version=?4 AND initiator_user_id=?5",
            params![instance_id,timer.org_id,timer.definition_id,timer.version,actor.user_id],
            |row| Ok((row_u64(row,0)?,row.get(1)?)),
        ).optional()?;
        if !current.is_some_and(|(revision, status)| {
            revision == expected && !matches!(status.as_str(), "completed" | "cancelled")
        }) {
            return Ok(None);
        }
    }
    let updates = plan.timer_updates.iter()
        .filter(|update| update.timer_id == timer.timer_id)
        .collect::<Vec<_>>();
    ensure!(updates.len() == 1, "timer fire requires one timer update");
    if timer.kind == ProcessTimerKind::Boundary {
        validate_boundary_plan_on(&tx, &timer, plan)?;
    } else {
        ensure!(
            plan.timer_updates.len() == 1,
            "timer fire cannot update an unrelated timer"
        );
        ensure!(
            plan.cancel_token_ids.is_empty()
                && plan.cancel_user_task_ids.is_empty()
                && plan.cancel_job_ids.is_empty(),
            "only an interrupting boundary may cancel an activity"
        );
    }
    let update = updates[0];
    let pinned_model = current_version_model_on(&tx, &timer.definition_id, timer.version)?;
    let advance = super::timers::next_timer_occurrence(
        &timer,
        at_ms,
        super::timers::TimerAdvanceMode::Fire,
        pinned_model.calendar_pin.as_ref(),
    )?;
    ensure!(
        update.expected_revision == candidate.revision
            && update.fired_occurrence == Some(advance.selected_occurrence)
            && update.occurrence == advance.next_occurrence
            && update.due_at_ms == advance.next_due_at_ms
            && update.status == advance.status
            && update.last_reason.is_none()
            && update.next_check_at_ms == advance.next_due_at_ms.unwrap_or(at_ms),
        "timer transition does not match its persisted schedule"
    );
    ensure!(
        plan.events.iter().any(|event| event.kind == "timer_fired"
            && event.node_id.as_deref() == Some(timer.node_id.as_str())
            && event.data["timer_id"].as_str() == Some(timer.timer_id.as_str())
            && event.data["occurrence"].as_u64() == Some(advance.selected_occurrence)
            && event.data["planned_due_at_ms"].as_i64() == Some(advance.planned_due_at_ms)
            && event.data["skipped_count"].as_u64() == Some(advance.skipped_count)),
        "timer transition lacks its actual due-slot event"
    );
    ensure!(
        actor.org_id == timer.org_id,
        "timer actor organization changed before firing"
    );
    require_actor(&tx, actor)?;
    if !timer_current_authority_on(&tx, &timer)? {
        return Err(ProcessAuthorityDenied("timer authority is no longer current").into());
    }
    let mut cancelled_claims = Vec::new();
    let result = if timer.kind == ProcessTimerKind::Start {
        ensure!(expected_instance_revision.is_none(), "start timer has no existing instance");
        require_owner(&tx, actor, &timer.definition_id)?;
        let definition = definition_on(&tx, &timer.definition_id)?;
        ensure!(
            !definition.archived && definition.published_version == Some(timer.version),
            "timer start is no longer the active published version"
        );
        let instance_id = plan.start_instance_id.as_deref()
            .context("timer start transition lacks its instance identity")?;
        update_timer_on(&tx, update, at_ms)?;
        let mut instance_plan = plan.clone();
        instance_plan.timer_updates.clear();
        start_instance_on(
            &tx, actor, instance_id, &timer.definition_id, timer.version,
            &serde_json::json!({}), &instance_plan, at_ms,
            Some((&timer.timer_id, advance.selected_occurrence)),
        )?
    } else {
        ensure!(plan.start_instance_id.is_none(), "activity timer cannot start another instance");
        let instance_id = timer.instance_id.as_deref().context("activity timer lacks instance")?;
        let expected = expected_instance_revision.context("activity timer needs instance revision")?;
        let snapshots_json: String = tx.query_row(
            "SELECT service_snapshots_json FROM bpmn_versions WHERE definition_id=?1 AND version=?2",
            params![timer.definition_id,timer.version],
            |row| row.get(0),
        )?;
        for snapshot in parse::<Vec<PinnedServiceSnapshot>>(snapshots_json)? {
            require_flow_current(&tx, actor, &snapshot.info.flow_id, None)?;
        }
        cancelled_claims = apply_plan_on(&tx, instance_id, &actor.user_id, expected, plan, at_ms)?;
        instance_on(&tx, actor, instance_id)?
    };
    tx.commit()?;
    Ok(Some(TimerFireOutcome {
        instance: result,
        cancelled_claims,
    }))
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

fn actor_active_on(conn: &Connection, actor: &ProcessActor) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM user_accounts u JOIN org_memberships m ON m.user_id=u.id JOIN organizations o ON o.org_id=m.org_id WHERE u.id=?1 AND m.org_id=?2 AND u.is_active=1 AND o.status='active')",
        params![actor.user_id, actor.org_id], |row| row.get(0),
    )?)
}

fn require_actor(conn: &Connection, actor: &ProcessActor) -> Result<()> {
    if !actor_active_on(conn, actor)? {
        return Err(ProcessAuthorityDenied(
            "process actor is not active in the current organization",
        ).into());
    }
    Ok(())
}

fn actor_owns_on(conn: &Connection, actor: &ProcessActor, definition_id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM bpmn_definitions WHERE definition_id=?1 AND org_id=?2 AND owner_user_id=?3)",
        params![definition_id, actor.org_id, actor.user_id], |row| row.get(0),
    )?)
}

fn require_owner(conn: &Connection, actor: &ProcessActor, definition_id: &str) -> Result<()> {
    if !actor_owns_on(conn, actor, definition_id)? {
        return Err(ProcessAuthorityDenied("process definition not found").into());
    }
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
        .optional()?
        .ok_or(ProcessAuthorityDenied("process actor is inactive"))?;
    let (version, graph): (u32, String) = conn
        .query_row(
            "SELECT version,flow_json FROM flows WHERE id=?1 AND status='active'",
            [flow_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
        .ok_or(ProcessAuthorityDenied("source flow is no longer active"))?;
    if !crate::db::repository::resource_permissions::check_default_allow(
            conn,
            "flow",
            flow_id,
            &actor.user_id,
            &role
        )? {
        return Err(ProcessAuthorityDenied(
            "process actor no longer has source flow access",
        ).into());
    }
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
    let mut definition: ProcessDefinition = conn.query_row(
        "SELECT definition_id,name,description,owner_user_id,draft_revision,model_json,published_version,archived FROM bpmn_definitions WHERE definition_id=?1",
        [definition_id],
        |row| {
            let model_text: String = row.get(5)?;
            let model = serde_json::from_str(&model_text).map_err(|error| rusqlite::Error::FromSqlConversionFailure(5, rusqlite::types::Type::Text, Box::new(error)))?;
            Ok(ProcessDefinition {
                definition_id: row.get(0)?, name: row.get(1)?, description: row.get(2)?,
                owner_user_id: row.get(3)?, draft_revision: row_u64(row, 4)?, model,
                published_version: row.get(6)?, archived: row.get(7)?,
                calendar_pin_state: None,
            })
        },
    )?;
    definition.calendar_pin_state = super::calendar::calendar_pin_state(&definition.model)?;
    Ok(definition)
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
        let mut stmt = conn.prepare("SELECT definition_id,name,description,owner_user_id,draft_revision,published_version,archived,model_json FROM bpmn_definitions WHERE org_id=?1 AND owner_user_id=?2 ORDER BY updated_at_ms DESC,definition_id DESC LIMIT ?3 OFFSET ?4")?;
        let definitions = stmt
            .query_map(params![actor.org_id, actor.user_id, limit, offset], |row| {
                Ok((
                    ProcessDefinitionSummary {
                        definition_id: row.get(0)?,
                        name: row.get(1)?,
                        description: row.get(2)?,
                        owner_user_id: row.get(3)?,
                        draft_revision: row_u64(row, 4)?,
                        published_version: row.get(5)?,
                        archived: row.get(6)?,
                        calendar_pin_state: None,
                    },
                    row.get::<_, String>(7)?,
                ))
            })?
            .map(|row| {
                let (mut summary, model_json) = row?;
                let model: ProcessModel = serde_json::from_str(&model_json)?;
                summary.calendar_pin_state = super::calendar::calendar_pin_state(&model)?;
                Ok(summary)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok((definitions, total, offset.saturating_add(limit) < total))
    })
}

pub fn get_definition(
    pool: &DbPool,
    actor: &ProcessActor,
    definition_id: &str,
) -> Result<(ProcessDefinition, Option<ProcessTimerSummary>)> {
    read_snapshot(pool, |conn| {
        require_actor(conn, actor)?;
        require_owner(conn, actor, definition_id)?;
        let definition = definition_on(conn, definition_id)?;
        let timer_id: Option<String> = conn.query_row(
            "SELECT timer_id FROM bpmn_timers WHERE definition_id=?1 AND version=?2 AND kind='start'",
            params![definition_id, definition.published_version],
            |row| row.get(0),
        ).optional()?;
        let timer_start = if let (Some(id), Some(version)) = (timer_id, definition.published_version) {
            let model = current_version_model_on(conn, definition_id, version)?;
            Some(timer_summary(&timer_on(conn, &id)?, &model)?)
        } else {
            None
        };
        Ok((definition, timer_start))
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
    repin_calendar: Option<bool>,
) -> Result<(ProcessDefinition, ProcessVersion)> {
    if let Some(result) = replay_definition_publication(pool, actor, stamp, definition_id)? {
        return Ok(result);
    }
    let (original, _) = get_definition(pool, actor, definition_id)?;
    ensure!(
        !original.archived && original.draft_revision == expected_revision,
        "process draft revision conflict or archived definition"
    );
    let mut prepared_model = original.model.clone();
    let state = super::calendar::calendar_pin_state(&prepared_model)?;
    if state == Some(tentaflow_protocol::processes::ProcessCalendarPinState::Stale) {
        ensure!(
            repin_calendar == Some(true),
            "calendar data changed; refresh calendar data to publish a new version"
        );
    }
    if let Some(calendar) = &prepared_model.work_calendar {
        if state == Some(tentaflow_protocol::processes::ProcessCalendarPinState::Unpinned)
            || repin_calendar == Some(true)
        {
            prepared_model.calendar_pin = Some(super::calendar::mint_calendar_pin(
                calendar,
                prepared_model
                    .timer_timezone
                    .as_deref()
                    .context("configured calendar requires an explicit IANA timezone")?,
            )?);
        }
    }
    validate_model(&prepared_model)?;
    let model_json = json(&prepared_model)?;
    ensure!(
        model_json.len() <= MAX_MODEL_BYTES,
        "pinned process model exceeds 512 KiB"
    );
    let original_json = json(&original.model)?;
    #[cfg(test)]
    calendar_tests::PUBLICATION_PREFLIGHT.with(|gate| {
        if let Some((ready, resume)) = gate.borrow().as_ref() {
            ready.send(()).expect("publication preflight barrier");
            resume.recv().expect("publication writer barrier");
        }
    });
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    require_actor(&tx, actor)?;
    require_owner(&tx, actor, definition_id)?;
    if let Some(result) = command_replay(&tx, actor, stamp)? {
        return Ok(result);
    }
    let definition = definition_on(&tx, definition_id)?;
    ensure!(
        !definition.archived
            && definition.draft_revision == expected_revision
            && definition.model == original.model,
        "process draft revision conflict or archived definition"
    );
    let prepared_state = super::calendar::calendar_pin_state(&prepared_model)?;
    ensure!(
        prepared_model.work_calendar.is_none()
            || prepared_state
                == Some(tentaflow_protocol::processes::ProcessCalendarPinState::Current),
        "publication needs a Current verified calendar pin"
    );
    let mut service_nodes = HashSet::new();
    for node in &prepared_model.nodes {
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
            "flow snapshot exceeds 512 KiB"
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
    let now = now_ms()?;
    tx.execute(
        "INSERT INTO bpmn_versions(definition_id,version,model_json,model_sha256,service_snapshots_json,published_at_ms,published_by) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![definition_id, version, model_json, hex::encode(Sha256::digest(model_json.as_bytes())), json(&snapshots)?, now, actor.user_id],
    )?;
    tx.execute(
        "UPDATE bpmn_timers SET status='cancelled',last_reason='superseded_by_publication',due_at_ms=NULL,revision=revision+1,updated_at_ms=?1 WHERE definition_id=?2 AND kind='start' AND status IN ('pending','blocked','archived')",
        params![now, definition_id],
    )?;
    if let Some(node) = prepared_model.nodes.iter().find(|node| {
        matches!(
            node.kind,
            tentaflow_protocol::processes::ProcessNodeKind::TimerStart { .. }
        )
    }) {
        let tentaflow_protocol::processes::ProcessNodeKind::TimerStart { timer: rule } = &node.kind else {
            unreachable!("timer start node was selected by kind")
        };
        let timezone = prepared_model
            .timer_timezone
            .as_deref()
            .context("timed process lacks its validated timezone")?;
        let due = super::timers::resolve_timer_due(
            rule,
            timezone,
            ProcessTimerKind::Start,
            now,
            prepared_model.calendar_pin.as_ref(),
        )?;
        let total_firings = match rule {
            ProcessTimerSpec::Cycle { total_firings, .. }
            | ProcessTimerSpec::Daily { total_firings, .. } => *total_firings,
            _ => Some(1),
        };
        insert_timer_on(
            &tx,
            &ProcessTimer {
                timer_id: Uuid::new_v4().to_string(),
                org_id: actor.org_id.clone(),
                definition_id: definition_id.to_string(),
                version,
                node_id: node.id.clone(),
                kind: ProcessTimerKind::Start,
                instance_id: None,
                token_id: None,
                rule: rule.clone(),
                total_firings,
                timezone: timezone.to_string(),
                anchor_at_ms: now,
                due_at_ms: Some(due),
                occurrence: 1,
                revision: 1,
                status: ProcessTimerStatus::Pending,
                last_reason: None,
                next_check_at_ms: due,
                created_at_ms: now,
                updated_at_ms: now,
            },
        )?;
    }
    let model_changed = model_json != original_json;
    if model_changed {
        sql_incrementable(definition.draft_revision)?;
    }
    tx.execute(
        "UPDATE bpmn_definitions SET published_version=?1,updated_at_ms=?2,model_json=?4,draft_revision=draft_revision+?5 WHERE definition_id=?3",
        params![version, now, definition_id, model_json, i64::from(model_changed)],
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
    let current_version: Option<u32> = tx.query_row(
        "SELECT published_version FROM bpmn_definitions WHERE definition_id=?1",
        [definition_id],
        |row| row.get(0),
    )?;
    if let Some(version) = current_version {
        let timer_id: Option<String> = tx.query_row(
            "SELECT timer_id FROM bpmn_timers WHERE definition_id=?1 AND version=?2 AND kind='start'",
            params![definition_id,version],
            |row| row.get(0),
        ).optional()?;
        if let Some(id) = timer_id {
            let timer = timer_on(&tx, &id)?;
            let (update, skipped_count, settled_occurrence) = if archived
                && matches!(timer.status, ProcessTimerStatus::Pending | ProcessTimerStatus::Blocked)
            {
                (Some(TimerUpdate {
                    timer_id: id.clone(), expected_revision: timer.revision,
                    fired_occurrence: None, occurrence: timer.occurrence,
                    due_at_ms: timer.due_at_ms, status: ProcessTimerStatus::Archived,
                    last_reason: Some("definition_archived".into()), next_check_at_ms: now,
                }), 0, None)
            } else if !archived && timer.status == ProcessTimerStatus::Archived {
                let model = current_version_model_on(&tx, &timer.definition_id, timer.version)?;
                let advance = super::timers::next_timer_occurrence(
                    &timer,
                    now,
                    super::timers::TimerAdvanceMode::Restore,
                    model.calendar_pin.as_ref(),
                )?;
                let status = advance.status;
                let reason = match status {
                    ProcessTimerStatus::Missed => Some("missed_during_archive".into()),
                    ProcessTimerStatus::Fired => {
                        Some("finite_schedule_exhausted_during_archive".into())
                    }
                    _ => None,
                };
                (Some(TimerUpdate {
                    timer_id: id.clone(), expected_revision: timer.revision,
                    fired_occurrence: None, occurrence: advance.next_occurrence,
                    due_at_ms: advance.next_due_at_ms,
                    status,
                    last_reason: reason,
                    next_check_at_ms: advance.next_due_at_ms.unwrap_or(now),
                }), advance.skipped_count, Some(advance.selected_occurrence))
            } else {
                (None, 0, None)
            };
            if let Some(update) = update {
                update_timer_on(&tx, &update, now)?;
                let action = match &update.status {
                    ProcessTimerStatus::Archived => "process.timer_archived",
                    ProcessTimerStatus::Missed => "process.timer_missed",
                    ProcessTimerStatus::Fired => "process.timer_exhausted",
                    _ => "process.timer_restored",
                };
                crate::db::repository::log_audit_scoped_tx(
                    &tx, Some(&actor.user_id), action, &id, "bpmn_timer", &id,
                    Some(&json(&serde_json::json!({"occurrence":update.occurrence,"due_at_ms":update.due_at_ms,"status":update.status,"skipped_count":skipped_count,"settled_occurrence":settled_occurrence}))?),
                    "info", "unclassified", Some(&actor.org_id), None,
                )?;
            }
        }
    }
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
    let mut task_stmt = conn.prepare("SELECT user_task_id,node_id,name,assignee_user_id,kind,status,outputs_json,revision,token_id FROM bpmn_user_tasks WHERE instance_id=?1 AND (?2 IS NULL OR user_task_id=?2) ORDER BY created_at_ms,user_task_id")?;
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
                row.get::<_, Option<String>>(8)?,
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
                token_id,
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
                    token_id,
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
                    )? && job_activation_live_on(conn, id)?
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
    let timers = timer_ids_on(conn, instance_id)?
        .into_iter()
        .map(|id| timer_summary(&timer_on(conn, &id)?, &model))
        .collect::<Result<Vec<_>>>()?;
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
        timers,
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
    prior.timers = current.timers;
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
        let sql=format!("SELECT i.instance_id,i.definition_id,d.name,i.initiator_user_id,i.version,i.revision,i.status,i.created_at_ms,i.updated_at_ms,EXISTS(SELECT 1 FROM bpmn_incidents x JOIN bpmn_jobs j ON j.job_id=x.job_id AND j.instance_id=x.instance_id JOIN bpmn_tokens t ON t.token_id=j.token_id AND t.instance_id=j.instance_id AND t.node_id=j.node_id WHERE x.instance_id=i.instance_id AND x.resolved_at_ms IS NULL AND j.status IN ('error','completed') AND t.status='waiting' AND i.status='incident'){where_sql} ORDER BY i.updated_at_ms DESC,i.instance_id DESC LIMIT ?4 OFFSET ?5");
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
    let timers = timer_ids_on(conn, instance_id)?
        .into_iter()
        .map(|id| timer_on(conn, &id))
        .collect::<Result<Vec<_>>>()?;
    Ok(RuntimeSnapshot {
        org_id: actor.org_id.clone(),
        instance,
        model,
        user_tasks,
        tokens,
        jobs,
        receipts,
        service_snapshots,
        timers,
        boundary_incidents: boundary_incidents_on(conn, instance_id)?,
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
) -> Result<Vec<CancelledJobClaim>> {
    validate_variables(&plan.variables)?;
    for incident_id in &plan.resolve_incident_ids {
        let affected = tx.execute("UPDATE bpmn_incidents SET resolved_at_ms=?1 WHERE incident_id=?2 AND instance_id=?3 AND resolved_at_ms IS NULL", params![at_ms,incident_id,instance_id])?;
        ensure!(affected == 1, "process incident changed before transition");
    }
    let unresolved_incident: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM bpmn_timers WHERE instance_id=?1 AND kind='catch' AND status='error') OR EXISTS(SELECT 1 FROM bpmn_incidents WHERE instance_id=?1 AND resolved_at_ms IS NULL)",
        [instance_id],
        |row| row.get(0),
    )?;
    let status = if unresolved_incident {
        ProcessInstanceStatus::Incident
    } else {
        plan.status.clone()
    };
    let mut cancelled_claims = Vec::new();
    let affected = tx.execute(
        "UPDATE bpmn_instances SET status=?1,variables_json=?2,revision=revision+1,updated_at_ms=?3 WHERE instance_id=?4 AND revision=?5 AND status NOT IN ('completed','cancelled')",
        params![status_text(&status),json(&plan.variables)?,at_ms,instance_id,sql_incrementable(expected_revision)?],
    )?;
    ensure!(
        affected == 1,
        "process instance revision conflict or closed instance"
    );
    for token_id in &plan.consume_token_ids {
        let affected = tx.execute("UPDATE bpmn_tokens SET status='consumed' WHERE token_id=?1 AND instance_id=?2 AND status IN ('ready','waiting','joining')",params![token_id,instance_id])?;
        ensure!(affected == 1, "process token changed before transition");
    }
    for token_id in &plan.cancel_token_ids {
        let affected = tx.execute("UPDATE bpmn_tokens SET status='cancelled' WHERE token_id=?1 AND instance_id=?2 AND status='waiting'",params![token_id,instance_id])?;
        ensure!(
            affected == 1,
            "interrupted activity token changed before transition"
        );
    }
    for token in &plan.create_tokens {
        ensure!(
            matches!(token.status.as_str(), "ready" | "waiting" | "joining"),
            "invalid new token status"
        );
        tx.execute("INSERT INTO bpmn_tokens(token_id,instance_id,node_id,arrival_edge_id,fork_stack_json,status,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![token.token_id,instance_id,token.node_id,token.arrival_edge_id,json(&token.fork_stack)?,token.status,at_ms])?;
    }
    for token_id in &plan.consume_token_ids {
        let mut statement = tx.prepare("SELECT timer_id FROM bpmn_timers WHERE instance_id=?1 AND token_id=?2 AND kind='boundary' AND status IN ('pending','blocked')")?;
        let siblings = statement
            .query_map(params![instance_id, token_id], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for id in siblings {
            ensure!(
                plan.timer_updates.iter().any(|update| update.timer_id == id
                    && update.status == ProcessTimerStatus::Cancelled
                    && update.last_reason.as_deref() == Some("activity_completed")),
                "completed activity must disarm its boundary timers"
            );
        }
    }
    for timer in &plan.create_timers {
        ensure!(
            timer.instance_id.as_deref() == Some(instance_id)
                && matches!(
                    timer.kind,
                    ProcessTimerKind::Catch | ProcessTimerKind::Boundary
                )
                && timer.anchor_at_ms == at_ms
                && timer.created_at_ms == at_ms
                && timer.updated_at_ms == at_ms,
            "transition may only create activity timers for its instance"
        );
        insert_timer_on(tx, timer)?;
    }
    for update in &plan.timer_updates {
        let actual = timer_on(tx, &update.timer_id)?;
        ensure!(
            actual.instance_id.as_deref() == Some(instance_id)
                && matches!(
                    actual.kind,
                    ProcessTimerKind::Catch | ProcessTimerKind::Boundary
                ),
            "transition may only update its own activity timer"
        );
        if actual.kind == ProcessTimerKind::Boundary
            && update.status == ProcessTimerStatus::Cancelled
        {
            let token_id = actual
                .token_id
                .as_deref()
                .context("boundary cancellation lacks its activation")?;
            let reason = update
                .last_reason
                .as_deref()
                .context("boundary cancellation lacks its reason")?;
            ensure!(
                (reason == "activity_completed" && plan.consume_token_ids.iter().any(|id|id == token_id))
                    || (reason == "sibling_interrupted" && plan.cancel_token_ids.iter().any(|id|id == token_id)),
                "boundary cancellation does not follow its exact activity completion or interruption"
            );
            ensure!(
                plan.events
                    .iter()
                    .any(|event| event.kind == "timer_cancelled"
                        && event.data["timer_id"].as_str() == Some(actual.timer_id.as_str())
                        && event.data["attached_token_id"].as_str() == Some(token_id)
                        && event.data["reason"].as_str() == Some(reason)),
                "boundary cancellation lacks its actual activation history"
            );
        }
        update_timer_on(tx, update, at_ms)?;
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
    for task_id in &plan.cancel_user_task_ids {
        let affected = tx.execute("UPDATE bpmn_user_tasks SET status='cancelled',revision=revision+1,updated_at_ms=?1 WHERE user_task_id=?2 AND instance_id=?3 AND status='open' AND token_id IN (SELECT token_id FROM bpmn_tokens WHERE instance_id=?3 AND status='cancelled')",params![at_ms,task_id,instance_id])?;
        ensure!(
            affected == 1,
            "interrupted user task changed before transition"
        );
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
        let token_id = task
            .token_id
            .as_deref()
            .context("new user task lacks its waiting activation")?;
        let waiting: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM bpmn_tokens WHERE token_id=?1 AND instance_id=?2 AND node_id=?3 AND status='waiting')",params![token_id,instance_id,task.node_id],|row|row.get(0))?;
        ensure!(
            waiting,
            "new user task does not match its waiting activation"
        );
        tx.execute("INSERT INTO bpmn_user_tasks(user_task_id,instance_id,node_id,name,assignee_user_id,kind,status,outputs_json,revision,created_at_ms,updated_at_ms,token_id) VALUES(?1,?2,?3,?4,?5,?6,'open',?7,1,?8,?8,?9)",params![task.user_task_id,instance_id,task.node_id,task.name,task.assignee_user_id,task_kind_text(&task.kind),json(&task.outputs)?,at_ms,token_id])?;
    }
    for job_id in &plan.complete_job_ids {
        let affected=tx.execute("UPDATE bpmn_jobs SET status='completed',updated_at_ms=?1 WHERE job_id=?2 AND instance_id=?3 AND status='running'",params![at_ms,job_id,instance_id])?;
        ensure!(affected == 1, "service job changed before transition");
    }
    for job_id in &plan.cancel_job_ids {
        let (status,attempt,fence,worker_id):(String,u32,u64,Option<String>) = tx.query_row(
            "SELECT status,attempt,fence,worker_id FROM bpmn_jobs WHERE job_id=?1 AND instance_id=?2 AND token_id IN (SELECT token_id FROM bpmn_tokens WHERE instance_id=?2 AND status='cancelled')",
            params![job_id,instance_id], |row| Ok((row.get(0)?,row.get(1)?,row_u64(row,2)?,row.get(3)?)),
        ).context("interrupted job does not match its cancelled activation")?;
        ensure!(
            matches!(status.as_str(), "queued" | "running" | "error"),
            "interrupted service job is no longer cancellable"
        );
        if status == "running" {
            cancelled_claims.push(CancelledJobClaim {
                job_id: job_id.clone(),
                attempt,
                fence,
                worker_id: worker_id.context("running service job has no worker identity")?,
            });
        }
        sql_incrementable(fence)?;
        let changed = tx.execute("UPDATE bpmn_jobs SET status='cancelled',fence=fence+1,worker_id=NULL,lease_until_ms=NULL,updated_at_ms=?1 WHERE job_id=?2 AND instance_id=?3 AND status=?4 AND attempt=?5 AND fence=?6",params![at_ms,job_id,instance_id,status,attempt,sql_integer(fence)?])?;
        ensure!(
            changed == 1,
            "service job generation changed before interruption"
        );
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
    Ok(cancelled_claims)
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
    at_ms: i64,
) -> Result<ProcessInstance> {
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    require_actor(&tx, actor)?;
    require_owner(&tx, actor, definition_id)?;
    if let Some(prior) = command_replay::<ProcessInstance>(&tx, actor, stamp)? {
        require_instance_reader(&tx, actor, &prior.instance_id)?;
        return reproject_instance_on(&tx, actor, prior);
    }
    let result = start_instance_on(
        &tx, actor, instance_id, definition_id, version, initial_variables, plan, at_ms, None,
    )?;
    store_command(&tx, actor, stamp, &result, at_ms)?;
    tx.commit()?;
    Ok(result)
}

fn start_instance_on(
    tx: &Transaction<'_>,
    actor: &ProcessActor,
    instance_id: &str,
    definition_id: &str,
    version: u32,
    initial_variables: &Value,
    plan: &RuntimePlan,
    at_ms: i64,
    start_identity: Option<(&str, u64)>,
) -> Result<ProcessInstance> {
    validate_variables(initial_variables)?;
    ensure!(Uuid::parse_str(instance_id).is_ok(), "instance_id must be a UUID");
    ensure!(
        plan.start_instance_id.as_deref() == Some(instance_id),
        "start transition identity differs from the persisted instance"
    );
    require_actor(tx, actor)?;
    require_owner(tx, actor, definition_id)?;
    let definition = definition_on(tx, definition_id)?;
    ensure!(
        !definition.archived,
        "archived process cannot start new instances"
    );
    let model = current_version_model_on(tx, definition_id, version)?;
    let start_is_timed = model.nodes.iter().any(|node| {
        matches!(node.kind, tentaflow_protocol::processes::ProcessNodeKind::TimerStart { .. })
    });
    ensure!(
        start_is_timed == start_identity.is_some(),
        "timer start requires its persisted due slot; manual start cannot bypass the timer"
    );
    if let Some((timer_id, occurrence)) = start_identity {
        let timer = timer_on(tx, timer_id)?;
        ensure!(
            timer.kind == ProcessTimerKind::Start
                && timer.definition_id == definition_id
                && timer.version == version
                && timer.org_id == actor.org_id
                && occurrence > 0,
            "start identity does not match the current process timer"
        );
    }
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
        require_flow_current(tx, actor, &snapshot.info.flow_id, None)?;
    }
    tx.execute("INSERT INTO bpmn_instances(instance_id,definition_id,version,org_id,initiator_user_id,revision,status,variables_json,created_at_ms,updated_at_ms,start_timer_id,start_occurrence) VALUES(?1,?2,?3,?4,?5,1,'running',?6,?7,?7,?8,?9)",params![instance_id,definition_id,version,actor.org_id,actor.user_id,json(&vars)?,at_ms,start_identity.map(|(id,_)|id),start_identity.map(|(_,slot)|sql_integer(slot)).transpose()?])?;
    apply_plan_on(tx, instance_id, &actor.user_id, 1, plan, at_ms)?;
    instance_on(tx, actor, instance_id)
}

pub fn apply_transition(
    pool: &DbPool,
    actor: &ProcessActor,
    instance_id: &str,
    expected_revision: u64,
    plan: &RuntimePlan,
    at_ms: i64,
) -> Result<ProcessInstance> {
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
    at_ms: i64,
) -> Result<ProcessInstance> {
    validate_output(outputs)?;
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    require_actor(&tx, actor)?;
    require_instance_reader(&tx, actor, instance_id)?;
    if let Some(prior) = command_replay::<ProcessInstance>(&tx, actor, stamp)? {
        require_instance_reader(&tx, actor, &prior.instance_id)?;
        return reproject_instance_on(&tx, actor, prior);
    }
    let (assignee,kind,status,token_id,node_id):(String,String,String,Option<String>,String)=tx.query_row("SELECT assignee_user_id,kind,status,token_id,node_id FROM bpmn_user_tasks WHERE user_task_id=?1 AND instance_id=?2",params![user_task_id,instance_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).context("user task not found")?;
    let token_id = token_id.context("open user task lacks its waiting activation")?;
    let activation_live: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM bpmn_tokens t JOIN bpmn_instances i ON i.instance_id=t.instance_id WHERE t.token_id=?1 AND t.instance_id=?2 AND t.node_id=?3 AND t.status='waiting' AND i.status NOT IN ('completed','cancelled'))",params![token_id,instance_id,node_id],|row|row.get(0))?;
    ensure!(activation_live, "user task activation is no longer waiting");
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
    let current = instance_on(&tx, actor, instance_id)?;
    let model = current_version_model_on(&tx, &current.definition_id, current.version)?;
    for link in boundary_incidents_on(&tx, instance_id)? {
        tx.execute("UPDATE bpmn_incidents SET resolved_at_ms=?1 WHERE incident_id=?2 AND instance_id=?3 AND resolved_at_ms IS NULL",params![at_ms,link.incident_id,instance_id])?;
    }
    for id in timer_ids_on(&tx, instance_id)? {
        let timer = timer_on(&tx, &id)?;
        if !matches!(timer.status, ProcessTimerStatus::Pending | ProcessTimerStatus::Blocked) {
            continue;
        }
        tx.execute(
            "UPDATE bpmn_timers SET status='cancelled',last_reason='instance_cancelled',next_check_at_ms=?1,revision=revision+1,updated_at_ms=?1 WHERE timer_id=?2 AND revision=?3 AND status IN ('pending','blocked')",
            params![at_ms,id,sql_incrementable(timer.revision)?],
        )?;
        let seq: u64 = tx.query_row(
            "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
            [instance_id],
            |row| row_u64(row, 0),
        )?;
        insert_event_on(&tx, instance_id, seq, Some(&actor.user_id), &PlannedEvent {
                kind: "timer_cancelled".into(),
                node_id: Some(timer.node_id.clone()),
                data: serde_json::json!({"timer_id":id,"kind":timer.kind,"reason":"instance_cancelled","attached_to_id":if timer.kind == ProcessTimerKind::Boundary { timer_activation_node(&model,&timer)? } else { None },"attached_token_id":timer.token_id}),
        }, at_ms)?;
    }
    tx.execute("UPDATE bpmn_tokens SET status='cancelled' WHERE instance_id=?1 AND status IN ('ready','waiting','joining')",[instance_id])?;
    tx.execute("UPDATE bpmn_user_tasks SET status='cancelled',revision=revision+1,updated_at_ms=?2 WHERE instance_id=?1 AND status='open'",params![instance_id,at_ms])?;
    tx.execute("UPDATE bpmn_jobs SET status='cancelled',fence=fence+1,worker_id=NULL,lease_until_ms=NULL,updated_at_ms=?2 WHERE instance_id=?1 AND status IN ('queued','running','error')",params![instance_id,at_ms])?;
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

fn job_activation_live_on(conn: &Connection, job_id: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM bpmn_jobs j JOIN bpmn_tokens t ON t.token_id=j.token_id AND t.instance_id=j.instance_id AND t.node_id=j.node_id JOIN bpmn_instances i ON i.instance_id=j.instance_id WHERE j.job_id=?1 AND t.status='waiting' AND i.status NOT IN ('completed','cancelled'))",
        [job_id], |row| row.get(0),
    )?)
}

pub fn claim_job(pool: &DbPool, worker_id: &str, now_ms: i64) -> Result<Option<ClaimedProcessJob>> {
    ensure!(
        !worker_id.is_empty() && worker_id.len() <= 128,
        "invalid process worker ID"
    );
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    let queued:Option<(String,String,String,String,String,u32)>=tx.query_row(
        "SELECT j.job_id,j.instance_id,j.node_id,i.org_id,i.initiator_user_id,i.version FROM bpmn_jobs j JOIN bpmn_instances i ON i.instance_id=j.instance_id JOIN bpmn_tokens t ON t.token_id=j.token_id AND t.instance_id=j.instance_id AND t.node_id=j.node_id WHERE j.status='queued' AND i.status IN ('running','waiting','incident') AND t.status='waiting' ORDER BY j.created_at_ms,j.job_id LIMIT 1",
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
    if !job_activation_live_on(&tx, job_id)? {
        return Ok(false);
    }
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
    at_ms: i64,
) -> Result<ProcessInstance> {
    validate_output(&result.outputs)?;
    ensure!(
        json(result)?.len() <= 384 * 1024 - 4096,
        "activity result exceeds the process history budget"
    );
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
    let commit_now_ms = now_ms()?;
    ensure!(
        status == "running"
            && current_attempt == attempt
            && current_fence == fence
            && current_worker.as_deref() == Some(worker_id)
            && lease_until_ms.is_some_and(|lease| lease >= commit_now_ms)
            && job_activation_live_on(&tx, job_id)?,
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
    ensure!(
        job_activation_live_on(&tx, job_id)?,
        "service activity is no longer retryable"
    );
    let node_id:String=tx.query_row("SELECT j.node_id FROM bpmn_jobs j JOIN bpmn_incidents x ON x.job_id=j.job_id AND x.instance_id=j.instance_id AND x.resolved_at_ms IS NULL WHERE j.job_id=?1 AND j.instance_id=?2 AND j.status IN ('error','completed')",params![job_id,instance_id],|row|row.get(0)).context("retryable job not found")?;
    let flow_id = job_flow_id_on(&tx, &instance.definition_id, instance.version, &node_id)?;
    require_flow_current(&tx, actor, &flow_id, None)?;
    let affected=tx.execute("UPDATE bpmn_jobs SET status='queued',fence=fence+1,worker_id=NULL,lease_until_ms=NULL,result_json=NULL,updated_at_ms=?1 WHERE job_id=?2 AND instance_id=?3 AND status IN ('error','completed')",params![at_ms,job_id,instance_id])?;
    ensure!(affected == 1, "retryable job changed");
    tx.execute("UPDATE bpmn_user_tasks SET status='cancelled',revision=revision+1,updated_at_ms=?1 WHERE instance_id=?2 AND token_id=(SELECT token_id FROM bpmn_jobs WHERE job_id=?3) AND kind='verification' AND status='open'",params![at_ms,instance_id,job_id])?;
    tx.execute("UPDATE bpmn_incidents SET resolved_at_ms=?1 WHERE instance_id=?2 AND job_id=?3 AND resolved_at_ms IS NULL",params![at_ms,instance_id,job_id])?;
    tx.execute("UPDATE bpmn_instances SET status=CASE WHEN EXISTS(SELECT 1 FROM bpmn_incidents WHERE instance_id=?2 AND resolved_at_ms IS NULL) THEN 'incident' ELSE 'running' END,revision=revision+1,updated_at_ms=?1 WHERE instance_id=?2",params![at_ms,instance_id])?;
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
        ensure!(
            job_activation_live_on(&tx, job_id)?,
            "running service job no longer has a waiting activation"
        );
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
    if !job_activation_live_on(&tx, job_id)? {
        return Ok(false);
    }
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

    #[test]
    fn overdue_finite_timer_starts_one_pinned_instance_with_selected_slot() {
        let (db, actor) = fixture();
        let mut model = starter_model();
        model.timer_timezone = Some("UTC".into());
        model.nodes[0].kind = ProcessNodeKind::TimerStart {
            timer: ProcessTimerSpec::Cycle { seconds: 300, total_firings: Some(3) },
        };
        let draft = save_definition(
            &db,
            &actor,
            &stamp("create timed"),
            None,
            0,
            "Timed",
            "",
            &model,
        )
        .unwrap();
        let (_, pinned) = publish_definition(
            &db,
            &actor,
            &stamp("publish timed"),
            &draft.definition_id,
            draft.draft_revision,
            &[],
            None,
        )
        .unwrap();
        assert_eq!(pinned.model, model);
        let (_, timer) = get_definition(&db, &actor, &draft.definition_id).unwrap();
        let timer = timer.unwrap();
        assert_eq!(timer.status, ProcessTimerStatus::Pending);
        let third_due = timer.due_at_ms.unwrap() + 600_000;
        let candidates = due_timers(&db, third_due, 32).unwrap();
        assert_eq!(candidates.len(), 1);
        let snapshot = timer_snapshot(&db, &candidates[0]).unwrap();
        let plan = super::super::timers::plan_timer_fire(&snapshot, third_due).unwrap();
        let fired = fire_timer(&db, &candidates[0], &actor, None, &plan, third_due)
            .unwrap().unwrap();
        assert!(fired.cancelled_claims.is_empty());
        let fired = fired.instance;
        assert_eq!(fired.status, ProcessInstanceStatus::Completed);
        let conn = db.read().unwrap();
        let (slot, count): (i64, i64) = conn.query_row(
            "SELECT start_occurrence,(SELECT COUNT(*) FROM bpmn_instances WHERE start_timer_id=?1) FROM bpmn_instances WHERE instance_id=?2",
            params![timer.timer_id, fired.instance_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!((slot, count), (3, 1));
        let event: String = conn.query_row(
            "SELECT data_json FROM bpmn_events WHERE instance_id=?1 AND kind='timer_fired'",
            [&fired.instance_id], |row| row.get(0),
        ).unwrap();
        let event: Value = serde_json::from_str(&event).unwrap();
        assert_eq!(event["occurrence"], 3);
        assert_eq!(event["skipped_count"], 2);
        drop(conn);
        assert!(fire_timer(&db, &candidates[0], &actor, None, &plan, third_due).unwrap().is_none());
        assert!(due_timers(&db, third_due + 1, 32).unwrap().is_empty());
    }

    #[test]
    fn start_timer_retains_full_bounded_reason_in_summary_and_audit() {
        let (db, actor) = fixture();
        let mut model = starter_model();
        model.timer_timezone = Some("UTC".into());
        model.nodes[0].kind = ProcessNodeKind::TimerStart {
            timer: ProcessTimerSpec::Cycle { seconds: 300, total_firings: Some(1) },
        };
        let draft = save_definition(
            &db,
            &actor,
            &stamp("create blocked start"),
            None,
            0,
            "Blocked start",
            "",
            &model,
        )
        .unwrap();
        publish_definition(
            &db,
            &actor,
            &stamp("publish blocked start"),
            &draft.definition_id,
            draft.draft_revision,
            &[],
            None,
        )
        .unwrap();
        let (_, timer) = get_definition(&db, &actor, &draft.definition_id).unwrap();
        let due_at = timer.unwrap().due_at_ms.unwrap();
        let candidate = due_timers(&db, due_at, 32).unwrap().remove(0);
        db.write().unwrap().execute(
            "UPDATE user_accounts SET is_active=0 WHERE id=?1", [&actor.user_id],
        ).unwrap();
        let reason = "Owner account in Łódź 🧪 was revoked ".repeat(24);
        assert!(reason.len() > 512 && reason.len() < 32 * 1024);
        assert!(record_timer_blocked(&db, &candidate, &reason, due_at).unwrap());
        db.write().unwrap().execute(
            "UPDATE user_accounts SET is_active=1 WHERE id=?1", [&actor.user_id],
        ).unwrap();
        let (_, timer) = get_definition(&db, &actor, &draft.definition_id).unwrap();
        assert_eq!(timer.unwrap().last_reason.as_deref(), Some(reason.as_str()));
        let details: String = db.read().unwrap().query_row(
            "SELECT details FROM audit_log WHERE action='process.timer_blocked' AND resource_id=?1 ORDER BY id DESC LIMIT 1",
            [&candidate.timer_id], |row| row.get(0),
        ).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&details).unwrap()["reason"], reason);
    }

    #[test]
    fn catch_timer_failure_is_terminal_and_preserves_waiting_work() {
        let (db, actor) = fixture();
        let mut model = starter_model();
        model.timer_timezone = Some("UTC".into());
        model.nodes.insert(1, ProcessNode {
            id: "Wait_1".into(), name: "Wait".into(),
            kind: ProcessNodeKind::TimerCatch { timer: ProcessTimerSpec::Duration { seconds: 1 } },
        });
        model.sequence_flows[0].target_id = "Wait_1".into();
        model.sequence_flows.push(ProcessSequenceFlow {
            id: "Flow_2".into(), source_id: "Wait_1".into(),
            target_id: "End_1".into(), condition: None,
        });
        let draft = save_definition(
            &db,
            &actor,
            &stamp("create catch"),
            None,
            0,
            "Catch",
            "",
            &model,
        )
        .unwrap();
        publish_definition(
            &db,
            &actor,
            &stamp("publish catch"),
            &draft.definition_id,
            draft.draft_revision,
            &[],
            None,
        )
        .unwrap();
        let instance_id = Uuid::new_v4().to_string();
        let plan = super::super::runtime::plan_start(&model, &instance_id, &actor,
            &draft.definition_id, 1, json!({}), super::super::runtime::StartCause::Manual, 1_000).unwrap();
        let waiting = start_instance(&db, &actor, &stamp("start catch"), &instance_id,
            &draft.definition_id, 1, &json!({}), &plan, 1_000).unwrap();
        assert_eq!(waiting.status, ProcessInstanceStatus::Waiting);
        assert_eq!(waiting.timers.len(), 1);
        let due = due_timers(&db, 2_000, 32).unwrap();
        assert_eq!(due.len(), 1);
        let full_reason = "Calendar horizon in Łódź 🧪 ".repeat(32);
        assert!(full_reason.len() > 512);
        assert!(record_timer_failed(&db, &due[0], &full_reason, 2_000).unwrap());
        let after = get_instance(&db, &actor, &instance_id).unwrap();
        assert_eq!(after.status, ProcessInstanceStatus::Incident);
        assert_eq!(after.timers[0].status, ProcessTimerStatus::Error);
        assert!(after.timers[0].last_reason.as_ref().unwrap().len() <= 512);
        assert!(after.timers[0].last_reason.as_ref().unwrap().contains("see process history"));
        assert_eq!(after.incidents[0].code, "TIMER_ERROR");
        assert!(after.incidents[0].message.contains("see process history"));
        let conn = db.read().unwrap();
        let waiting_tokens: i64 = conn.query_row(
            "SELECT COUNT(*) FROM bpmn_tokens WHERE instance_id=?1 AND node_id='Wait_1' AND status='waiting'",
            [&instance_id], |row| row.get(0),
        ).unwrap();
        assert_eq!(waiting_tokens, 1);
        let timer_errors: i64 = conn.query_row(
            "SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1 AND kind='timer_error'",
            [&instance_id], |row| row.get(0),
        ).unwrap();
        assert_eq!(timer_errors, 1);
        let full_event: String = conn.query_row(
            "SELECT data_json FROM bpmn_events WHERE instance_id=?1 AND kind='timer_error'",
            [&instance_id], |row| row.get(0),
        ).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&full_event).unwrap()["reason"], full_reason);
        drop(conn);
        assert!(due_timers(&db, 3_000, 32).unwrap().is_empty());
        assert!(!record_timer_failed(&db, &due[0], "duplicate", 3_000).unwrap());
    }

    #[test]
    fn timer_fire_rechecks_future_assignee_after_snapshot_revocation() {
        let (db, actor) = fixture();
        let participant = ProcessActor { org_id: actor.org_id.clone(), user_id: "process-other".into() };
        let mut model = user_model(&participant.user_id);
        model.timer_timezone = Some("UTC".into());
        model.nodes.insert(2, ProcessNode {
            id: "Wait_1".into(), name: "Wait".into(),
            kind: ProcessNodeKind::TimerCatch { timer: ProcessTimerSpec::Duration { seconds: 1 } },
        });
        model.sequence_flows[1].target_id = "Wait_1".into();
        model.sequence_flows.push(ProcessSequenceFlow {
            id: "Flow_3".into(), source_id: "Wait_1".into(),
            target_id: "End_1".into(), condition: None,
        });
        let draft = save_definition(
            &db,
            &actor,
            &stamp("create revocation"),
            None,
            0,
            "Revocation",
            "",
            &model,
        )
        .unwrap();
        publish_definition(
            &db,
            &actor,
            &stamp("publish revocation"),
            &draft.definition_id,
            draft.draft_revision,
            &[],
            None,
        )
        .unwrap();
        let instance_id = Uuid::new_v4().to_string();
        let start_plan = super::super::runtime::plan_start(&model, &instance_id, &actor,
            &draft.definition_id, 1, json!({}), super::super::runtime::StartCause::Manual, 1_000).unwrap();
        let started = start_instance(&db, &actor, &stamp("start revocation"), &instance_id,
            &draft.definition_id, 1, &json!({}), &start_plan, 1_000).unwrap();
        let task_id = started.user_tasks[0].user_task_id.clone();
        let task_snapshot = runtime_snapshot(&db, &participant, &instance_id).unwrap();
        let completed_plan = super::super::runtime::plan_user_completion(
            &task_snapshot, &task_id, &json!({}), None, 1_500,
        ).unwrap();
        let waiting = complete_user_task(&db, &participant, &stamp("complete before timer"),
            &instance_id, &task_id, started.revision, &json!({}), None,
            &completed_plan, 1_500).unwrap();
        assert_eq!(waiting.status, ProcessInstanceStatus::Waiting);
        let due = due_timers(&db, 2_500, 32).unwrap();
        assert_eq!(due.len(), 1);
        let snapshot = timer_snapshot(&db, &due[0]).unwrap();
        let fire_plan = super::super::timers::plan_timer_fire(&snapshot, 2_500).unwrap();
        let before: (i64, i64, i64) = db.read().unwrap().query_row(
            "SELECT (SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1),
                    (SELECT COUNT(*) FROM bpmn_jobs WHERE instance_id=?1),
                    (SELECT COUNT(*) FROM bpmn_user_tasks WHERE instance_id=?1)",
            [&instance_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).unwrap();
        db.write().unwrap().execute(
            "UPDATE user_accounts SET is_active=0 WHERE id=?1", [&participant.user_id],
        ).unwrap();
        let denied = fire_timer(&db, &due[0], &actor, Some(waiting.revision), &fire_plan, 2_500)
            .unwrap_err();
        assert!(denied.downcast_ref::<ProcessAuthorityDenied>().is_some(), "{denied:#}");
        let after: (i64, i64, i64) = db.read().unwrap().query_row(
            "SELECT (SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1),
                    (SELECT COUNT(*) FROM bpmn_jobs WHERE instance_id=?1),
                    (SELECT COUNT(*) FROM bpmn_user_tasks WHERE instance_id=?1)",
            [&instance_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).unwrap();
        assert_eq!(after, before);
        let unchanged = get_instance(&db, &actor, &instance_id).unwrap();
        assert_eq!(unchanged.revision, waiting.revision);
        assert_eq!(unchanged.timers[0].status, ProcessTimerStatus::Pending);
        let full_reason = "Assignee revoked after Łódź 🧪 snapshot ".repeat(24);
        assert!(full_reason.len() > 512);
        assert!(record_timer_blocked(&db, &due[0], &full_reason, 2_500).unwrap());
        let blocked = get_instance(&db, &actor, &instance_id).unwrap();
        assert_eq!(blocked.timers[0].status, ProcessTimerStatus::Blocked);
        assert!(blocked.timers[0].last_reason.as_ref().unwrap().len() <= 512);
        assert!(blocked.timers[0].last_reason.as_ref().unwrap().contains("see process history"));
        let first_event_count: i64 = db.read().unwrap().query_row(
            "SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1 AND kind='timer_blocked'",
            [&instance_id], |row| row.get(0),
        ).unwrap();
        assert_eq!(first_event_count, 1);
        let full_event: String = db.read().unwrap().query_row(
            "SELECT data_json FROM bpmn_events WHERE instance_id=?1 AND kind='timer_blocked'",
            [&instance_id], |row| row.get(0),
        ).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&full_event).unwrap()["reason"], full_reason);
        let refreshed = due_timers(&db, 62_500, 32).unwrap();
        assert_eq!(refreshed.len(), 1);
        assert!(!record_timer_blocked(&db, &due[0], &full_reason, 62_500).unwrap());
        assert!(record_timer_blocked(&db, &refreshed[0], &full_reason, 62_500).unwrap());
        let repeated_event_count: i64 = db.read().unwrap().query_row(
            "SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1 AND kind='timer_blocked'",
            [&instance_id], |row| row.get(0),
        ).unwrap();
        assert_eq!(repeated_event_count, first_event_count);
        let changed_tail = format!("{full_reason} changed after the preview");
        let refreshed = due_timers(&db, 122_500, 32).unwrap();
        assert_eq!(refreshed.len(), 1);
        assert!(record_timer_blocked(&db, &refreshed[0], &changed_tail, 122_500).unwrap());
        let changed_event_count: i64 = db.read().unwrap().query_row(
            "SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1 AND kind='timer_blocked'",
            [&instance_id], |row| row.get(0),
        ).unwrap();
        assert_eq!(changed_event_count, first_event_count + 1);
    }

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
            None,
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
            None,
        )
        .unwrap();
        let instance_id = Uuid::new_v4().to_string();
        let start_plan = super::super::runtime::plan_start(
            &model, &instance_id, &actor, &definition.definition_id, 1,
            json!({}), super::super::runtime::StartCause::Manual, 1_000,
        ).unwrap();
        let waiting = start_instance(
            &db,
            &actor,
            &stamp("start"),
            &instance_id,
            &definition.definition_id,
            1,
            &json!({}),
            &start_plan,
            1_000,
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
        let plan = super::super::runtime::plan_user_completion(&snapshot, &task_id, &outputs, None, 2_000)
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
            2_000,
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
            2_000,
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
            2_000,
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
            None,
        )
        .unwrap();
        let instance_id = Uuid::new_v4().to_string();
        let plan = super::super::runtime::plan_start(
            &model, &instance_id, &actor, &definition.definition_id, 1,
            json!({}), super::super::runtime::StartCause::Manual, 1_000,
        ).unwrap();
        let command = stamp("start inactive");
        assert!(start_instance(
            &db,
            &actor,
            &command,
            &instance_id,
            &definition.definition_id,
            1,
            &json!({}),
            &plan,
            1_000,
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
                .0.draft_revision,
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

#[cfg(test)]
mod calendar_tests {
    use super::*;
    use super::super::runtime::test_support::{Fixture, stamp};
    use tentaflow_protocol::processes::{
        HolidayPolicy, ManualDayOff, ProcessCalendarPinState, ProcessWorkCalendar, WorkWindow,
    };

    fn configured_model(timed: bool) -> ProcessModel {
        let mut model = starter_model();
        model.timer_timezone = Some("UTC".into());
        model.work_calendar = Some(ProcessWorkCalendar {
            name: "Private published calendar".into(),
            weekly_windows: (1..=7)
                .map(|weekday| WorkWindow {
                    weekday,
                    start_minute: 0,
                    end_minute: 1440,
                })
                .collect(),
            manual_days_off: Vec::new(),
            holiday_policy: HolidayPolicy::None,
        });
        if timed {
            model.nodes[0].kind = ProcessNodeKind::TimerStart {
                timer: ProcessTimerSpec::WorkingDuration { seconds: 1 },
            };
        }
        model
    }

    fn save(
        fixture: &Fixture,
        previous: Option<&ProcessDefinition>,
        model: &ProcessModel,
    ) -> ProcessDefinition {
        save_definition(
            &fixture.db,
            &fixture.owner,
            &stamp("calendar save"),
            previous.map(|item| item.definition_id.as_str()),
            previous.map_or(0, |item| item.draft_revision),
            "Private calendar process",
            "",
            model,
        )
        .unwrap()
    }

    #[test]
    fn calendar_publication_resolves_current_pin_preserves_version_and_replays_exact_command() {
        let fixture = Fixture::new();
        let draft = save(&fixture, None, &configured_model(false));
        assert_eq!(
            draft.calendar_pin_state,
            Some(ProcessCalendarPinState::Unpinned)
        );
        let command = stamp("first mint without a timer");
        let first = publish_definition(
            &fixture.db,
            &fixture.owner,
            &command,
            &draft.definition_id,
            draft.draft_revision,
            &[],
            None,
        )
        .unwrap();
        assert_eq!(first.0.draft_revision, draft.draft_revision + 1);
        assert_eq!(
            first.0.calendar_pin_state,
            Some(ProcessCalendarPinState::Current)
        );
        assert!(first.1.model.calendar_pin.is_some());
        assert_eq!(first.0.model, first.1.model);
        assert_eq!(
            publish_definition(
                &fixture.db,
                &fixture.owner,
                &command,
                &draft.definition_id,
                draft.draft_revision,
                &[],
                None
            )
            .unwrap(),
            first
        );
        let mut edited = first.0.model.clone();
        edited.nodes[0].name = "Ordinary name edit after first mint".into();
        let saved = save(&fixture, Some(&first.0), &edited);
        assert_eq!(
            saved.calendar_pin_state,
            Some(ProcessCalendarPinState::Current)
        );
        assert_eq!(saved.model.calendar_pin, first.1.model.calendar_pin);
        let mut changed = saved.model.clone();
        changed.work_calendar.as_mut().unwrap().weekly_windows[0].start_minute = 60;
        let stale = save(&fixture, Some(&saved), &changed);
        assert_eq!(
            stale.calendar_pin_state,
            Some(ProcessCalendarPinState::Stale)
        );
        for flag in [None, Some(false)] {
            assert!(publish_definition(
                &fixture.db,
                &fixture.owner,
                &stamp("stale cannot silently refresh"),
                &stale.definition_id,
                stale.draft_revision,
                &[],
                flag
            )
            .is_err());
        }
        assert_eq!(
            get_definition(&fixture.db, &fixture.owner, &stale.definition_id)
                .unwrap()
                .0,
            stale
        );
        let refreshed = publish_definition(
            &fixture.db,
            &fixture.owner,
            &stamp("explicit refresh"),
            &stale.definition_id,
            stale.draft_revision,
            &[],
            Some(true),
        )
        .unwrap();
        assert_eq!(
            refreshed.0.calendar_pin_state,
            Some(ProcessCalendarPinState::Current)
        );
        assert_eq!(refreshed.1.version, 2);
        assert_eq!(refreshed.0.draft_revision, stale.draft_revision + 1);
        assert_ne!(refreshed.1.model.calendar_pin, first.1.model.calendar_pin);
        assert_eq!(
            get_version(&fixture.db, &fixture.owner, &draft.definition_id, 1).unwrap(),
            first.1
        );
        let (list, total, _) = list_definitions(&fixture.db, &fixture.owner, 0, 10).unwrap();
        assert_eq!(total, 1);
        assert_eq!(
            list[0].calendar_pin_state,
            Some(ProcessCalendarPinState::Current)
        );
        let unchanged = publish_definition(
            &fixture.db,
            &fixture.owner,
            &stamp("preserve matching retained release"),
            &refreshed.0.definition_id,
            refreshed.0.draft_revision,
            &[],
            Some(false),
        )
        .unwrap();
        assert_eq!(unchanged.0.draft_revision, refreshed.0.draft_revision);
        assert_eq!(
            unchanged.1.model.calendar_pin,
            refreshed.1.model.calendar_pin
        );
    }

    #[test]
    fn concurrent_calendar_publication_commits_one_pin_version_and_start_timer() {
        let fixture = Fixture::new();
        let draft = save(&fixture, None, &configured_model(true));
        let first = stamp("publication race one");
        let second = stamp("publication race two");
        let barrier = std::sync::Barrier::new(2);
        let outcomes = std::thread::scope(|scope| {
            let left = scope.spawn(|| {
                barrier.wait();
                publish_definition(
                    &fixture.db,
                    &fixture.owner,
                    &first,
                    &draft.definition_id,
                    draft.draft_revision,
                    &[],
                    None,
                )
            });
            let right = scope.spawn(|| {
                barrier.wait();
                publish_definition(
                    &fixture.db,
                    &fixture.owner,
                    &second,
                    &draft.definition_id,
                    draft.draft_revision,
                    &[],
                    None,
                )
            });
            vec![left.join().unwrap(), right.join().unwrap()]
        });
        assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
        assert_eq!(
            outcomes.iter().filter(|outcome| outcome.is_err()).count(),
            1
        );
        let (definition, timer) =
            get_definition(&fixture.db, &fixture.owner, &draft.definition_id).unwrap();
        let version = get_version(&fixture.db, &fixture.owner, &draft.definition_id, 1).unwrap();
        let timer = timer.unwrap();
        assert_eq!(timer.status, ProcessTimerStatus::Pending);
        assert_eq!(timer.due_at_ms, Some(version.published_at_ms + 1000));
        assert_eq!(timer.working_time.unwrap().due_offset_seconds, Some(0));
        assert_eq!(definition.model, version.model);
        let conn = fixture.db.read().unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM bpmn_versions", [], |row| row
                .get::<_, u32>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM bpmn_timers", [], |row| row
                .get::<_, u32>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM bpmn_commands WHERE command_id IN (?1,?2)",
                params![first.command_id, second.command_id],
                |row| row.get::<_, u32>(0)
            )
            .unwrap(),
            1
        );
    }

    #[test]
    fn forged_refresh_and_complete_pin_byte_overflow_preserve_published_start_and_command_state() {
        let fixture = Fixture::new();
        let draft = save(&fixture, None, &configured_model(true));
        let (published, version) = publish_definition(
            &fixture.db,
            &fixture.owner,
            &stamp("valid initial publication"),
            &draft.definition_id,
            draft.draft_revision,
            &[],
            None,
        )
        .unwrap();
        let old_timer = get_definition(&fixture.db, &fixture.owner, &draft.definition_id)
            .unwrap()
            .1;
        let mut forged = published.model.clone();
        let pin = forged.calendar_pin.as_mut().unwrap();
        pin.timezone_data.initial_offset_seconds = 60;
        let payload = serde_json::json!({"calendar":pin.calendar,"legal_release":pin.legal_release,"timezone_data":pin.timezone_data});
        let mut hash = Sha256::new();
        hash.update(b"tentaflow.process.calendar.pin\0");
        hash.update(serde_json::to_vec(&payload).unwrap());
        pin.sha256 = hex::encode(hash.finalize());
        assert!(save_definition(
            &fixture.db,
            &fixture.owner,
            &stamp("forged pin save"),
            Some(&draft.definition_id),
            published.draft_revision,
            "Forged",
            "",
            &forged
        )
        .is_err());
        // Exercise the publication trust boundary even if storage was corrupted after an authorized save.
        let original_json = fixture
            .db
            .read()
            .unwrap()
            .query_row(
                "SELECT model_json FROM bpmn_definitions WHERE definition_id=?1",
                [&draft.definition_id],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        fixture
            .db
            .write()
            .unwrap()
            .execute(
                "UPDATE bpmn_definitions SET model_json=?1 WHERE definition_id=?2",
                params![json(&forged).unwrap(), draft.definition_id],
            )
            .unwrap();
        let forged_command = stamp("forged explicit refresh");
        assert!(publish_definition(
            &fixture.db,
            &fixture.owner,
            &forged_command,
            &draft.definition_id,
            published.draft_revision,
            &[],
            Some(true)
        )
        .is_err());
        fixture
            .db
            .write()
            .unwrap()
            .execute(
                "UPDATE bpmn_definitions SET model_json=?1 WHERE definition_id=?2",
                params![original_json, draft.definition_id],
            )
            .unwrap();
        let mut oversized = published.model.clone();
        let calendar = oversized.work_calendar.as_mut().unwrap();
        let day = chrono::NaiveDate::from_ymd_opt(2030, 1, 1).unwrap();
        calendar.manual_days_off = (0..256)
            .map(|offset| ManualDayOff {
                date: day
                    .checked_add_days(chrono::Days::new(offset))
                    .unwrap()
                    .to_string(),
                reason: "\"".repeat(128),
            })
            .collect();
        super::super::calendar::validate_work_calendar(calendar).unwrap();
        let stale = save(&fixture, Some(&published), &oversized);
        let overflow_command = stamp("full pin serialization overflow");
        assert!(publish_definition(
            &fixture.db,
            &fixture.owner,
            &overflow_command,
            &draft.definition_id,
            stale.draft_revision,
            &[],
            Some(true)
        )
        .unwrap_err()
        .to_string()
        .contains("128 KiB"));
        assert_eq!(
            get_definition(&fixture.db, &fixture.owner, &draft.definition_id).unwrap(),
            (stale, old_timer)
        );
        assert_eq!(
            get_version(&fixture.db, &fixture.owner, &draft.definition_id, 1).unwrap(),
            version
        );
        let conn = fixture.db.read().unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM bpmn_versions", [], |row| row
                .get::<_, u32>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM bpmn_commands WHERE command_id IN (?1,?2)",
                params![forged_command.command_id, overflow_command.command_id],
                |row| row.get::<_, u32>(0)
            )
            .unwrap(),
            0
        );
    }

    thread_local! {
        pub(super) static PUBLICATION_PREFLIGHT: std::cell::RefCell<Option<(std::sync::mpsc::SyncSender<()>, std::sync::mpsc::Receiver<()>)>> = const { std::cell::RefCell::new(None) };
    }

    #[test]
    fn prepared_calendar_publication_rechecks_revision_account_org_flow_and_storage_atomically() {
        use super::super::runtime::test_support::{flow, graph, publish_model, service_model};
        for mutation in 0..5 {
            let fixture = Fixture::new();
            let flow_id = flow(
                &fixture.db,
                &fixture.owner,
                &graph("real source snapshot", None),
            );
            let mut model = service_model(
                &flow_id,
                tentaflow_protocol::processes::ActivityVerification::Human,
            );
            let base = configured_model(true);
            model.nodes[0].kind = base.nodes[0].kind.clone();
            model.work_calendar = base.work_calendar;
            model.timer_timezone = base.timer_timezone;
            let version = publish_model(&fixture, &model);
            let original =
                get_definition(&fixture.db, &fixture.owner, &version.definition_id).unwrap();
            let snapshots: Vec<PinnedServiceSnapshot> = serde_json::from_str(&fixture.db.read().unwrap().query_row("SELECT service_snapshots_json FROM bpmn_versions WHERE definition_id=?1 AND version=1", [&version.definition_id], |row| row.get::<_, String>(0)).unwrap()).unwrap();
            let attempt = stamp("prepared publication must revalidate");
            let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
            let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(0);
            let mut expected = original.0.clone();
            let result = std::thread::scope(|scope| {
                let f = &fixture;
                let draft = &original.0;
                let request = &attempt;
                let publishing = scope.spawn(move || {
                    PUBLICATION_PREFLIGHT
                        .with(|gate| *gate.borrow_mut() = Some((ready_tx, resume_rx)));
                    let result = publish_definition(
                        &f.db,
                        &f.owner,
                        request,
                        &draft.definition_id,
                        draft.draft_revision,
                        &snapshots,
                        None,
                    );
                    PUBLICATION_PREFLIGHT.with(|gate| *gate.borrow_mut() = None);
                    result
                });
                ready_rx
                    .recv_timeout(std::time::Duration::from_secs(10))
                    .expect("actual pin prepared before writer");
                match mutation {
                    0 => {
                        fixture
                            .db
                            .write()
                            .unwrap()
                            .execute(
                                "UPDATE user_accounts SET is_active=0 WHERE id=?1",
                                [&fixture.owner.user_id],
                            )
                            .unwrap();
                    }
                    1 => {
                        fixture
                            .db
                            .write()
                            .unwrap()
                            .execute(
                                "DELETE FROM org_memberships WHERE org_id=?1 AND user_id=?2",
                                params![fixture.owner.org_id, fixture.owner.user_id],
                            )
                            .unwrap();
                    }
                    2 => {
                        crate::db::repository::resource_permissions::set(
                            &fixture.db,
                            "flow",
                            &flow_id,
                            "user",
                            &fixture.owner.user_id,
                            "deny",
                        )
                        .unwrap();
                    }
                    3 => {
                        let mut changed = original.0.model.clone();
                        changed.work_calendar.as_mut().unwrap().weekly_windows[0].start_minute = 1;
                        expected = save(&fixture, Some(&original.0), &changed);
                    }
                    4 => {
                        fixture.db.write().unwrap().execute_batch("CREATE TRIGGER reject_prepared_start BEFORE INSERT ON bpmn_timers BEGIN SELECT RAISE(ABORT,'controlled timer storage failure'); END;").unwrap();
                    }
                    _ => unreachable!(),
                }
                resume_tx.send(()).unwrap();
                publishing.join().unwrap()
            });
            assert!(
                result.is_err(),
                "current mutation {mutation} cannot publish prepared data"
            );
            match mutation {
                0 => {
                    fixture
                        .db
                        .write()
                        .unwrap()
                        .execute(
                            "UPDATE user_accounts SET is_active=1 WHERE id=?1",
                            [&fixture.owner.user_id],
                        )
                        .unwrap();
                }
                1 => {
                    crate::services::org::repo::add_membership(
                        &fixture.db,
                        &fixture.owner.org_id,
                        &fixture.owner.user_id,
                        "role-org-viewer",
                        &fixture.owner.user_id,
                    )
                    .unwrap();
                }
                2 => {
                    crate::db::repository::resource_permissions::set(
                        &fixture.db,
                        "flow",
                        &flow_id,
                        "user",
                        &fixture.owner.user_id,
                        "allow",
                    )
                    .unwrap();
                }
                4 => {
                    fixture
                        .db
                        .write()
                        .unwrap()
                        .execute_batch("DROP TRIGGER reject_prepared_start")
                        .unwrap();
                }
                _ => (),
            }
            assert_eq!(
                get_definition(&fixture.db, &fixture.owner, &version.definition_id).unwrap(),
                (expected, original.1)
            );
            assert_eq!(
                get_version(&fixture.db, &fixture.owner, &version.definition_id, 1).unwrap(),
                version
            );
            let conn = fixture.db.read().unwrap();
            assert_eq!(
                conn.query_row("SELECT COUNT(*) FROM bpmn_versions", [], |row| row
                    .get::<_, u32>(0))
                    .unwrap(),
                1
            );
            assert_eq!(
                conn.query_row(
                    "SELECT COUNT(*) FROM bpmn_commands WHERE command_id=?1",
                    [&attempt.command_id],
                    |row| row.get::<_, u32>(0)
                )
                .unwrap(),
                0
            );
        }
    }
}
