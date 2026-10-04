// ============ File: repository.rs — transactional BPMN definitions and runtime state ============

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, ensure, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tentaflow_protocol::processes::{
    ActivityResult, PinnedFlowInfo, ProcessCallPinInfo, ProcessCallStatus, ProcessCallSummary,
    ProcessDefinition, ProcessDefinitionSummary, ProcessEvent, ProcessEventRaceStatus,
    ProcessEventRaceSummary, ProcessIncident, ProcessIncidentSelection, ProcessInstance,
    ProcessInstancePageInfo, ProcessInstancePageRequest, ProcessInstanceStatus,
    ProcessInstanceSummary, ProcessMessageDetail, ProcessMessageOrigin, ProcessMessageStartSummary,
    ProcessMessageStatus, ProcessMessageSummary, ProcessMessageTarget, ProcessModel, ProcessNode,
    ProcessNodeKind, ProcessPageInfo, ProcessPageSpec, ProcessPayload, ProcessRelatedInstance,
    ProcessScopeSummary, ProcessSubscriptionKind, ProcessSubscriptionStatus,
    ProcessSubscriptionSummary, ProcessTerminalError, ProcessTimerKind, ProcessTimerSpec,
    ProcessTimerStatus, ProcessTimerSummary, ProcessUserTask, ProcessUserTaskKind,
    ProcessUserTaskStatus, ProcessUserTaskSummary, ProcessVersion, ProcessVersionSummary,
};
use uuid::Uuid;

use super::model::{starter_model, validate_model, validate_variables, GatewayKind, MAX_MODEL_BYTES};
use super::runtime::validate_output;
use crate::db::DbPool;

#[cfg(test)]
thread_local! {
    pub(super) static PUBLICATION_PREFLIGHT: std::cell::RefCell<Option<(std::sync::mpsc::SyncSender<()>, std::sync::mpsc::Receiver<()>)>> = const { std::cell::RefCell::new(None) };
    pub(super) static CALL_VERSION_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static CALL_TRANSITION_PREFLIGHT: std::cell::RefCell<Option<(std::sync::mpsc::SyncSender<()>, std::sync::mpsc::Receiver<()>)>> = const { std::cell::RefCell::new(None) };
}

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

#[derive(Debug, Clone, Copy)]
pub(super) enum MessageClosed {
    Activation,
    Instance,
    Source,
}

impl MessageClosed {
    fn reason(self) -> &'static str {
        match self {
            Self::Activation => "activation_closed",
            Self::Instance => "target_instance_closed",
            Self::Source => "instance_cancelled",
        }
    }
}

impl std::fmt::Display for MessageClosed {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.reason())
    }
}

impl std::error::Error for MessageClosed {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinnedServiceSnapshot {
    pub info: PinnedFlowInfo,
    pub graph_json: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallActivation {
    pub call_id: String,
    pub parent_instance_id: String,
    pub parent_scope_id: String,
    pub parent_token_id: String,
    pub call_node_id: String,
    pub child_instance_id: String,
    pub definition_id: String,
    pub version: u32,
    pub called_definition_id: String,
    pub called_version: u32,
    pub model_sha256: String,
    pub revision: u64,
    pub status: ProcessCallStatus,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinnedCallVersion {
    pub version: ProcessVersion,
    pub services: Vec<PinnedServiceSnapshot>,
    pub name: String,
    pub admission_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallRequest {
    pub call_id: String,
    pub parent_scope_id: String,
    pub parent_token_id: String,
    pub call_node_id: String,
    pub child_instance_id: String,
    pub variables: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedCall {
    pub call: CallActivation,
    pub variables: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CallStep {
    Start {
        call: PlannedCall,
        plan: Box<RuntimePlan>,
    },
    Return {
        call: CallActivation,
        expected_revision: u64,
        status: ProcessCallStatus,
        source: Option<BusinessErrorSource>,
        plan: Box<RuntimePlan>,
    },
    Advance {
        instance_id: String,
        expected_revision: u64,
        plan: Box<RuntimePlan>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BusinessErrorSource {
    ErrorEnd {
        instance_id: String,
        token_id: String,
        fact: ProcessTerminalError,
        outputs: Value,
    },
    ServiceContract {
        instance_id: String,
        scope_id: String,
        node_id: String,
        token_id: String,
        job_id: String,
        attempt: u32,
        fence: u64,
        result_event_id: String,
        result: ActivityResult,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForkFrame {
    pub activation_id: String,
    pub split_node_id: String,
    pub join_node_id: String,
    pub branch_edge_id: String,
    pub gateway_kind: GatewayKind,
    pub selected_branch_edge_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessToken {
    pub token_id: String,
    pub scope_id: String,
    pub node_id: String,
    pub arrival_edge_id: Option<String>,
    pub fork_stack: Vec<ForkFrame>,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayReceipt {
    pub scope_id: String,
    pub gateway_kind: GatewayKind,
    pub join_node_id: String,
    pub activation_id: String,
    pub branch_edge_id: String,
    pub token_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessJob {
    pub job_id: String,
    pub instance_id: String,
    pub scope_id: String,
    pub node_id: String,
    pub token_id: String,
    pub input: Value,
    pub status: String,
    pub attempt: u32,
    pub fence: u64,
    pub worker_id: Option<String>,
    pub lease_until_ms: Option<i64>,
    pub result: Option<ActivityResult>,
    pub result_origin: Option<ActivityResultOrigin>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActivityResultOrigin {
    Envelope,
    Contract,
    Platform,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservedActivityResult {
    pub result: ActivityResult,
    pub origin: ActivityResultOrigin,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expression_observation: Option<ExpressionObservation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpressionObservation {
    pub normalized_outputs: Value,
    pub evaluation_variables: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventSubscription {
    pub subscription_id: String,
    pub instance_id: String,
    pub scope_id: String,
    pub org_id: String,
    pub definition_id: String,
    pub version: u32,
    pub node_id: String,
    pub token_id: String,
    pub kind: ProcessSubscriptionKind,
    pub message_name: Option<String>,
    pub correlation_key: Option<String>,
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_code: Option<String>,
    pub race_id: Option<String>,
    pub revision: u64,
    pub status: ProcessSubscriptionStatus,
    pub last_reason: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionUpdate {
    pub subscription_id: String,
    pub expected_revision: u64,
    pub status: ProcessSubscriptionStatus,
    pub last_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRace {
    pub race_id: String,
    pub instance_id: String,
    pub scope_id: String,
    pub gateway_node_id: String,
    pub activation_id: String,
    pub revision: u64,
    pub status: ProcessEventRaceStatus,
    pub winner_node_id: Option<String>,
    pub winner_subscription_id: Option<String>,
    pub winner_timer_id: Option<String>,
    pub won_at_ms: Option<i64>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRaceUpdate {
    pub race_id: String,
    pub expected_revision: u64,
    pub status: ProcessEventRaceStatus,
    pub winner_node_id: Option<String>,
    pub winner_subscription_id: Option<String>,
    pub winner_timer_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedMessage {
    pub message_id: String,
    pub target: ProcessMessageTarget,
    pub message_name: String,
    pub correlation_key: String,
    pub payload: Value,
    pub ttl_seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedMessage {
    pub message: PreparedMessage,
    pub source_scope_id: String,
    pub source_node_id: String,
    pub source_activation_id: String,
    pub source_event_index: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageKey {
    pub org_id: String,
    pub sender_user_id: String,
    pub message_id: String,
}

#[derive(Debug, Clone)]
pub struct MessageCandidate {
    pub key: MessageKey,
    pub revision: u64,
    pub next_check_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct MessageRecord {
    pub key: MessageKey,
    pub request_hash: String,
    pub origin: ProcessMessageOrigin,
    pub target: ProcessMessageTarget,
    pub message_name: String,
    pub correlation_key: String,
    pub payload: Option<Value>,
    pub payload_available: bool,
    pub payload_sha256: String,
    pub payload_bytes: u32,
    pub ttl_seconds: u32,
    pub received_at_ms: i64,
    pub expires_at_ms: i64,
    pub revision: u64,
    pub status: ProcessMessageStatus,
    pub last_reason: Option<String>,
    pub next_check_at_ms: i64,
    pub updated_at_ms: i64,
    pub resolved_instance_id: Option<String>,
    pub resolved_subscription_id: Option<String>,
    pub resolved_token_id: Option<String>,
    pub matched_instance_id: Option<String>,
    pub matched_version: Option<u32>,
    pub matched_node_id: Option<String>,
    pub matched_subscription_id: Option<String>,
    pub delivered_at_ms: Option<i64>,
    pub source_instance_id: Option<String>,
    pub source_scope_id: Option<String>,
    pub source_node_id: Option<String>,
}

#[derive(Debug, Clone)]
pub enum MessageSelection {
    Ready(MessageSnapshot),
    NoMatch,
    Ambiguous,
    Stale,
}

#[derive(Debug, Clone)]
pub struct MessageSnapshot {
    pub candidate: MessageCandidate,
    pub message: MessageRecord,
    pub target: MessageDeliveryTarget,
}

#[derive(Debug, Clone)]
pub enum MessageDeliveryTarget {
    Start {
        actor: ProcessActor,
        version: ProcessVersion,
        instance_id: String,
    },
    Catch {
        actor: ProcessActor,
        subscription: EventSubscription,
        snapshot: RuntimeSnapshot,
    },
}

#[derive(Debug)]
pub struct MessageDeliveryOutcome {
    pub message: ProcessMessageSummary,
    pub transition: ProcessTransitionOutcome,
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
    pub scope_id: Option<String>,
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
    pub race_id: Option<String>,
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
    pub receipts: Vec<GatewayReceipt>,
    pub service_snapshots: Vec<PinnedServiceSnapshot>,
    pub timers: Vec<ProcessTimer>,
    pub boundary_incidents: Vec<BoundaryEventIncident>,
    pub subscriptions: Vec<EventSubscription>,
    pub event_races: Vec<EventRace>,
    pub incidents: Vec<ProcessIncident>,
    pub scopes: Vec<ProcessScopeSummary>,
    pub scope_variables: BTreeMap<String, Value>,
    pub calls: Vec<CallActivation>,
    pub call_versions: Vec<PinnedCallVersion>,
    pub retained_scope_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedEvent {
    pub scope_id: String,
    pub kind: String,
    pub node_id: Option<String>,
    pub data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimePlan {
    pub start_instance_id: Option<String>,
    pub start_variables: Option<Value>,
    pub consume_token_ids: Vec<String>,
    pub cancel_token_ids: Vec<String>,
    pub create_tokens: Vec<ProcessToken>,
    pub add_gateway_receipts: Vec<GatewayReceipt>,
    pub remove_gateway_receipts: Vec<GatewayReceipt>,
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
    pub create_subscriptions: Vec<EventSubscription>,
    pub subscription_updates: Vec<SubscriptionUpdate>,
    pub create_event_races: Vec<EventRace>,
    pub race_updates: Vec<EventRaceUpdate>,
    pub create_messages: Vec<PlannedMessage>,
    pub create_scopes: Vec<PlannedScope>,
    pub scope_updates: Vec<ScopeUpdate>,
    pub cancel_scope_roots: Vec<String>,
    pub call_requests: Vec<CallRequest>,
    pub scope_terminal_errors: BTreeMap<String, ProcessTerminalError>,
    pub call_steps: Vec<CallStep>,
    pub event_ids: BTreeMap<usize, String>,
    pub terminal_error: Option<ProcessTerminalError>,
    pub business_error: Option<BusinessErrorSource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedScope {
    pub scope_id: String,
    pub parent_scope_id: String,
    pub parent_token_id: String,
    pub subprocess_node_id: String,
    pub variables: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScopeUpdate {
    pub scope_id: String,
    pub expected_revision: u64,
    pub status: ProcessInstanceStatus,
    pub variables: Option<Value>,
}

impl RuntimePlan {
    pub fn initial(variables: Value) -> Self {
        Self {
            start_instance_id: None,
            start_variables: None,
            consume_token_ids: Vec::new(),
            cancel_token_ids: Vec::new(),
            create_tokens: Vec::new(),
            add_gateway_receipts: Vec::new(),
            remove_gateway_receipts: Vec::new(),
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
            create_subscriptions: Vec::new(),
            subscription_updates: Vec::new(),
            create_event_races: Vec::new(),
            race_updates: Vec::new(),
            create_messages: Vec::new(),
            create_scopes: Vec::new(),
            scope_updates: Vec::new(),
            cancel_scope_roots: Vec::new(),
            call_requests: Vec::new(),
            scope_terminal_errors: BTreeMap::new(),
            call_steps: Vec::new(),
            event_ids: BTreeMap::new(),
            terminal_error: None,
            business_error: None,
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
pub struct ProcessTransitionOutcome {
    pub instance: ProcessInstance,
    pub cancelled_claims: Vec<CancelledJobClaim>,
}

#[derive(Deserialize)]
struct StoredInstanceIdentity {
    instance_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundaryActivationId {
    Timer(String),
    Subscription(String),
    ScopeEntry(String),
}

#[derive(Debug, Clone)]
pub struct BoundaryEventIncident {
    pub activation: BoundaryActivationId,
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
        timer_id: timer_id.to_string(),
        org_id,
        definition_id,
        version,
        node_id,
        kind: timer_kind_from_text(&kind)?,
        scope_id: conn.query_row(
            "SELECT scope_id FROM bpmn_timers WHERE timer_id=?1",
            [timer_id],
            |row| row.get(0),
        )?,
        instance_id,
        token_id,
        rule,
        total_firings,
        timezone,
        anchor_at_ms,
        due_at_ms,
        occurrence,
        revision,
        status: timer_status_from_text(&status)?,
        last_reason,
        next_check_at_ms,
        created_at_ms,
        updated_at_ms,

        race_id: conn.query_row(
            "SELECT race_id FROM bpmn_timers WHERE timer_id=?1",
            [timer_id],
            |row| row.get(0),
        )?,
    })
}

fn timer_summary(
    conn: &Connection,
    timer: &ProcessTimer,
    model: &ProcessModel,
) -> Result<ProcessTimerSummary> {
    let node_name = match (&timer.instance_id, &timer.scope_id) {
        (Some(instance), Some(scope)) => {
            scoped_node_on(conn, instance, scope, model, &timer.node_id)?
                .name
                .clone()
        }
        (None, None) if timer.kind == ProcessTimerKind::Start => model
            .nodes
            .iter()
            .find(|node| node.id == timer.node_id)
            .context("start timer node missing")?
            .name
            .clone(),
        _ => bail!("timer scope ownership is invalid"),
    };
    let total_firings = match &timer.rule {
        ProcessTimerSpec::Cycle { total_firings, .. }
        | ProcessTimerSpec::Daily { total_firings, .. } => *total_firings,
        _ => None,
    };
    Ok(ProcessTimerSummary {
        scope_id: timer.scope_id.clone(),
        timer_id: timer.timer_id.clone(),
        node_id: timer.node_id.clone(),
        node_name,
        kind: timer.kind.clone(),
        status: timer.status.clone(),
        due_at_ms: timer.due_at_ms,
        timezone: timer.timezone.clone(),
        occurrence: timer.occurrence,
        total_firings,
        last_reason: timer.last_reason.clone(),
        attached_to_id: if timer.kind == ProcessTimerKind::Boundary {
            timer_activation_node(conn, model, timer)?.map(str::to_owned)
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
    conn: &Connection,
    model: &'a ProcessModel,
    timer: &ProcessTimer,
) -> Result<Option<&'a str>> {
    let nodes = match (&timer.instance_id, &timer.scope_id) {
        (Some(instance), Some(scope)) => {
            let scopes = scopes_on(conn, instance, model)?;
            let path = scope_path(&scopes, instance, scope)?;
            super::model::scope_body(model, &path)?.0
        }
        (None, None) if timer.kind == ProcessTimerKind::Start => model.nodes.as_slice(),
        _ => bail!("timer scope ownership is invalid"),
    };
    let node = nodes
        .iter()
        .find(|node| node.id == timer.node_id)
        .context("timer node is absent from its exact pinned body")?;
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
            ensure!(
                nodes.iter().any(|node| &node.id == attached_to_id
                    && matches!(
                        node.kind,
                        ProcessNodeKind::UserTask { .. }
                            | ProcessNodeKind::ServiceTask { .. }
                            | ProcessNodeKind::SubProcess { .. }
                            | ProcessNodeKind::CallActivity { .. }
                    )),
                "boundary attachment is not an activity in its body"
            );
            Ok(Some(attached_to_id))
        }
        _ => bail!("timer rule/kind differs from its pinned node"),
    }
}

fn timer_activation_live_on(conn: &Connection, timer: &ProcessTimer) -> Result<bool> {
    let model = current_version_model_on(conn, &timer.definition_id, timer.version)?;
    let Some(node_id) = timer_activation_node(conn, &model, timer)? else {
        return Ok(true);
    };
    let instance_id = timer
        .instance_id
        .as_deref()
        .context("activity timer lacks instance")?;
    if !call_control_live_on(conn, instance_id)? {
        return Ok(false);
    }
    let token_id = timer
        .token_id
        .as_deref()
        .context("activity timer lacks waiting token")?;
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM bpmn_tokens t JOIN bpmn_instances i ON i.instance_id=t.instance_id JOIN bpmn_scopes s ON s.instance_id=t.instance_id AND s.scope_id=t.scope_id WHERE t.token_id=?1 AND t.instance_id=?2 AND t.node_id=?3 AND t.status='waiting' AND t.scope_id=?7 AND EXISTS(SELECT 1 FROM bpmn_scopes s WHERE s.instance_id=t.instance_id AND s.scope_id=t.scope_id AND (s.parent_scope_id IS NULL OR s.status NOT IN ('completed','cancelled','error'))) AND i.definition_id=?4 AND i.version=?5 AND i.org_id=?6 AND i.status NOT IN ('completed','cancelled','error'))",
        params![token_id,instance_id,node_id,timer.definition_id,timer.version,timer.org_id,timer.scope_id],
        |row| row.get(0),
    )?)
}

fn boundary_incidents_on(
    conn: &Connection,
    instance_id: &str,
) -> Result<Vec<BoundaryEventIncident>> {
    let mut statement = conn.prepare(
        "SELECT t.timer_id,t.token_id,x.incident_id FROM bpmn_timers t JOIN bpmn_events e ON e.instance_id=t.instance_id AND e.kind='timer_error' AND json_extract(e.data_json,'$.timer_id')=t.timer_id JOIN bpmn_incidents x ON x.incident_id=json_extract(e.data_json,'$.incident_id') AND x.instance_id=t.instance_id AND x.node_id=t.node_id AND x.code='TIMER_ERROR' AND x.resolved_at_ms IS NULL WHERE t.instance_id=?1 AND t.kind='boundary' AND t.status='error' AND json_extract(e.data_json,'$.attached_token_id')=t.token_id",
    )?;
    let incidents = statement
        .query_map([instance_id], |row| {
            Ok(BoundaryEventIncident {
                activation: BoundaryActivationId::Timer(row.get(0)?),
                token_id: row.get(1)?,
                incident_id: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut incidents = incidents;
    let mut q=conn.prepare("SELECT s.subscription_id,s.token_id,x.incident_id FROM bpmn_event_subscriptions s JOIN bpmn_events e ON e.instance_id=s.instance_id AND e.kind='message_error' AND json_extract(e.data_json,'$.subscription_id')=s.subscription_id JOIN bpmn_incidents x ON x.incident_id=json_extract(e.data_json,'$.incident_id') AND x.instance_id=s.instance_id AND x.node_id=s.node_id AND x.resolved_at_ms IS NULL WHERE s.instance_id=?1 AND json_extract(e.data_json,'$.attached_token_id')=s.token_id")?;
    incidents.extend(
        q.query_map([instance_id], |r| {
            Ok(BoundaryEventIncident {
                activation: BoundaryActivationId::Subscription(r.get(0)?),
                token_id: r.get(1)?,
                incident_id: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?,
    );
    let mut q=conn.prepare("SELECT json_extract(e.data_json,'$.parent_token_id'),x.incident_id FROM bpmn_events e JOIN bpmn_incidents x ON x.incident_id=json_extract(e.data_json,'$.incident_id') AND x.instance_id=e.instance_id AND x.scope_id=e.scope_id JOIN bpmn_tokens t ON t.token_id=json_extract(e.data_json,'$.parent_token_id') AND t.instance_id=e.instance_id AND t.scope_id=e.scope_id WHERE e.instance_id=?1 AND e.kind='scope_entry_failed' AND x.resolved_at_ms IS NULL AND t.status='waiting'")?;
    incidents.extend(
        q.query_map([instance_id], |row| {
            let token: String = row.get(0)?;
            Ok(BoundaryEventIncident {
                activation: BoundaryActivationId::ScopeEntry(token.clone()),
                token_id: token,
                incident_id: row.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?,
    );
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
    timer_activation_node(tx, &model, timer)?;
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
            timer.instance_id.is_none() && timer.token_id.is_none() && timer.scope_id.is_none(),
            "start timer cannot have an instance token"
        );
    }
    if let Some(id) = &timer.race_id {
        let race = race_on(tx, id)?;
        let scopes = scopes_on(tx, race.instance_id.as_str(), &model)?;
        let path = scope_path(&scopes, &race.instance_id, &race.scope_id)?;
        let flows = super::model::scope_body(&model, &path)?.1;
        ensure!(
            timer.kind == ProcessTimerKind::Catch
                && timer.instance_id.as_deref() == Some(race.instance_id.as_str())
                && timer.scope_id.as_deref() == Some(race.scope_id.as_str())
                && race.status == ProcessEventRaceStatus::Open
                && flows
                    .iter()
                    .any(|e| e.source_id == race.gateway_node_id && e.target_id == timer.node_id),
            "timer does not belong to its exact event race"
        );
    }
    tx.execute(
        "INSERT INTO bpmn_timers(timer_id,org_id,definition_id,version,node_id,kind,instance_id,token_id,rule_json,timezone,anchor_at_ms,due_at_ms,occurrence,revision,status,last_reason,next_check_at_ms,created_at_ms,updated_at_ms,race_id,scope_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21)",
        params![timer.timer_id,timer.org_id,timer.definition_id,timer.version,timer.node_id,timer_kind_text(&timer.kind),timer.instance_id,timer.token_id,json(&timer.rule)?,timer.timezone,timer.anchor_at_ms,timer.due_at_ms,sql_integer(timer.occurrence)?,sql_integer(timer.revision)?,timer_status_text(&timer.status),timer.last_reason,timer.next_check_at_ms,timer.created_at_ms,timer.updated_at_ms,timer.race_id,timer.scope_id],
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
        ids.into_iter()
            .map(|id| {
                let timer = timer_on(conn, &id)?;
                Ok(DueTimer {
                    timer_id: id,
                    kind: timer.kind,
                    org_id: timer.org_id,
                    definition_id: timer.definition_id,
                    version: timer.version,
                    instance_id: timer.instance_id,
                    token_id: timer.token_id,
                    occurrence: timer.occurrence,
                    revision: timer.revision,
                    due_at_ms: timer.due_at_ms.context("due timer has no due instant")?,
                })
            })
            .collect()
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
            "SELECT initiator_user_id FROM bpmn_instances WHERE instance_id=?1 AND org_id=?2 AND definition_id=?3 AND version=?4 AND status NOT IN ('completed','cancelled','error')",
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
    if let Some(id) = &timer.instance_id {
        if let Err(error) = require_call_control_authority_on(conn, &actor, id) {
            if error.downcast_ref::<ProcessAuthorityDenied>().is_some() {
                return Ok(false);
            }
            return Err(error);
        }
    }
    let model = current_version_model_on(conn, &timer.definition_id, timer.version)?;
    for node in super::model::all_nodes(&model) {
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
            insert_event_on(
                &tx,
                instance_id,
                next_seq,
                None,
                &PlannedEvent {
                    scope_id: timer
                        .scope_id
                        .clone()
                        .context("instance timer has no scope")?,
                    kind: "timer_blocked".into(),
                    node_id: Some(timer.node_id.clone()),
                    data: serde_json::json!({"timer_id":timer.timer_id,"kind":timer.kind,"reason":full_reason,"due_at_ms":candidate.due_at_ms,"next_check_at_ms":next_check,"attached_to_id":if timer.kind == ProcessTimerKind::Boundary { timer_activation_node(&tx, &model, &timer)? } else { None },"attached_token_id":timer.token_id}),
                },
                at_ms,
                None,
            )?;
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
            "UPDATE bpmn_instances SET status='incident',revision=revision+1,updated_at_ms=?1 WHERE instance_id=?2 AND status NOT IN ('completed','cancelled','error') AND revision < 9223372036854775807",
            params![at_ms,instance_id],
        )?;
        ensure!(
            changed_instance == 1,
            "catch timer instance closed during failure recording"
        );
        tx.execute("UPDATE bpmn_scopes SET status='incident',revision=revision+1,updated_at_ms=?1 WHERE instance_id=?2 AND scope_id=?3 AND parent_scope_id IS NOT NULL AND status NOT IN ('completed','cancelled','error')",params![at_ms,instance_id,timer.scope_id])?;
        let incident_id = Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO bpmn_incidents(incident_id,instance_id,node_id,job_id,code,message,at_ms,scope_id) VALUES(?1,?2,?3,NULL,'TIMER_ERROR',?4,?5,?6)",
            params![incident_id,instance_id,timer.node_id,incident_message(&full_reason),at_ms,timer.scope_id],
        )?;
        check_active_incident_budget_on(&tx, instance_id)?;
        let next_seq: u64 = tx.query_row(
            "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
            [instance_id],
            |row| row_u64(row, 0),
        )?;
        let model = current_version_model_on(&tx, &timer.definition_id, timer.version)?;
        let mut event = PlannedEvent {
            scope_id: timer
                .scope_id
                .clone()
                .context("instance timer has no scope")?,
            kind: "timer_error".into(),
            node_id: Some(timer.node_id.clone()),
            data: serde_json::json!({"timer_id":timer.timer_id,"kind":timer.kind,"incident_id":incident_id,"reason":full_reason,"due_at_ms":candidate.due_at_ms,"attached_to_id":if timer.kind == ProcessTimerKind::Boundary { timer_activation_node(&tx, &model, &timer)? } else { None },"attached_token_id":timer.token_id}),
        };
        if let Some(working_time) = super::calendar::working_time_summary(
            &timer.rule,
            model.calendar_pin.as_ref(),
            timer.due_at_ms,
        )? {
            event.data["working_time"] = serde_json::to_value(working_time)?;
            event.data["timezone"] = serde_json::json!(timer.timezone);
        }
        insert_event_on(&tx, instance_id, next_seq, None, &event, at_ms, None)?;
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
    activation: &BoundaryActivationId,
    accepted_job_id: Option<&str>,
    plan: &RuntimePlan,
) -> Result<()> {
    let (instance_id, scope_id, definition_id, version, node_id, token_id) = match activation {
        BoundaryActivationId::Timer(id) => {
            let timer = timer_on(tx, id)?;
            (
                timer.instance_id.context("boundary lacks instance")?,
                timer.scope_id.context("boundary lacks scope")?,
                timer.definition_id,
                timer.version,
                timer.node_id,
                timer.token_id.context("boundary lacks activation")?,
            )
        }
        BoundaryActivationId::Subscription(id) => {
            let sub = subscription_on(tx, id)?;
            ensure!(
                sub.status == ProcessSubscriptionStatus::Open && subscription_live_on(tx, &sub)?,
                "boundary subscription is no longer open"
            );
            (
                sub.instance_id,
                sub.scope_id,
                sub.definition_id,
                sub.version,
                sub.node_id,
                sub.token_id,
            )
        }
        BoundaryActivationId::ScopeEntry(_) => bail!("scope entry is not a firing boundary"),
    };
    let model = current_version_model_on(tx, &definition_id, version)?;
    let scopes = scopes_on(tx, &instance_id, &model)?;
    let node = scope_node(&model, &scopes, &instance_id, &scope_id, &node_id)?;
    let (attached_to_id, interrupt) = match &node.kind {
        ProcessNodeKind::BoundaryTimer {
            attached_to_id,
            cancel_activity,
            ..
        }
        | ProcessNodeKind::BoundaryMessage {
            attached_to_id,
            cancel_activity,
            ..
        }
        | ProcessNodeKind::BoundaryEscalation {
            attached_to_id,
            cancel_activity,
            ..
        } => (attached_to_id, *cancel_activity),
        ProcessNodeKind::BoundaryError { attached_to_id, .. } => (attached_to_id, true),
        _ => bail!("activation is not a boundary"),
    };
    let (actual_node, stack, status): (String, String, String) = tx.query_row(
        "SELECT node_id,fork_stack_json,status FROM bpmn_tokens WHERE token_id=?1 AND instance_id=?2 AND scope_id=?3",
        params![token_id,instance_id,scope_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
    )?;
    ensure!(
        actual_node == *attached_to_id
            && status == "waiting"
            && parse::<Vec<ForkFrame>>(stack)?.is_empty(),
        "boundary does not match its current attachment"
    );
    if let BoundaryActivationId::Timer(id) = activation {
        ensure!(
            plan.events.iter().any(|event| event.kind == "timer_fired"
                && event.node_id.as_deref() == Some(node_id.as_str())
                && event.data["timer_id"].as_str() == Some(id.as_str())
                && event.data["attached_token_id"].as_str() == Some(token_id.as_str())
                && event.data["attached_to_id"].as_str() == Some(attached_to_id.as_str())
                && event.data["cancel_activity"].as_bool() == Some(interrupt)),
            "boundary firing lacks its actual attachment facts"
        );
    }
    if reaches_error_end(plan) {
        return Ok(());
    }
    let equal_ids = |actual: &[String], expected: &[String]| {
        actual.len() == expected.len()
            && actual.iter().all(|id| expected.contains(id))
            && actual.iter().collect::<HashSet<_>>().len() == actual.len()
    };
    let mut own_incidents = boundary_incidents_on(tx, &instance_id)?
        .into_iter()
        .filter(|link| link.token_id == token_id && (interrupt || &link.activation == activation))
        .map(|link| link.incident_id)
        .collect::<Vec<_>>();
    let child = scopes.iter().find(|scope| {
        scope.parent_token_id.as_deref() == Some(token_id.as_str())
            && !matches!(
                scope.status,
                ProcessInstanceStatus::Completed
                    | ProcessInstanceStatus::Cancelled
                    | ProcessInstanceStatus::Error
            )
    });
    let expected_roots = if interrupt {
        child
            .map(|scope| vec![scope.scope_id.clone()])
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    ensure!(
        equal_ids(&plan.cancel_scope_roots, &expected_roots),
        "boundary scope closure differs from its actual attached child"
    );
    let closure = if let Some(root) = expected_roots.first() {
        descendant_scope_ids(&scopes, root)?
    } else {
        HashSet::new()
    };
    let active_scopes = scopes
        .iter()
        .filter(|scope| {
            closure.contains(&scope.scope_id)
                && !matches!(
                    scope.status,
                    ProcessInstanceStatus::Completed
                        | ProcessInstanceStatus::Cancelled
                        | ProcessInstanceStatus::Error
                )
        })
        .map(|scope| scope.scope_id.clone())
        .collect::<HashSet<_>>();
    let selected = |scope: &str, token: &str| {
        active_scopes.contains(scope) || (interrupt && token == token_id)
    };
    let mut stmt = tx.prepare("SELECT token_id,scope_id FROM bpmn_tokens WHERE instance_id=?1 AND status IN ('ready','waiting','joining')")?;
    let expected_tokens = stmt
        .query_map([&instance_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|(id, scope)| selected(scope, id))
        .map(|(id, _)| id)
        .collect::<Vec<_>>();
    ensure!(
        equal_ids(&plan.cancel_token_ids, &expected_tokens),
        "boundary must close exactly its attachment and active child subtree"
    );
    if !interrupt {
        ensure!(
            !plan.consume_token_ids.contains(&token_id),
            "noninterrupting boundary cannot consume its attachment"
        );
    }
    let mut stmt = tx.prepare("SELECT user_task_id,scope_id,token_id FROM bpmn_user_tasks WHERE instance_id=?1 AND status='open'")?;
    let tasks = stmt
        .query_map([&instance_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|(_, scope, token)| selected(scope, token.as_deref().unwrap_or("")))
        .map(|(id, _, _)| id)
        .collect::<Vec<_>>();
    ensure!(
        equal_ids(&plan.cancel_user_task_ids, &tasks),
        "boundary must cancel exactly its subtree's open work"
    );
    let mut stmt = tx.prepare("SELECT job_id,scope_id,token_id FROM bpmn_jobs WHERE instance_id=?1 AND status IN ('queued','running','error')")?;
    let jobs = stmt
        .query_map([&instance_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|(id, scope, token)| selected(scope, token) && Some(id.as_str()) != accepted_job_id)
        .map(|(id, _, _)| id)
        .collect::<Vec<_>>();
    ensure!(
        equal_ids(&plan.cancel_job_ids, &jobs),
        "boundary must cancel exactly its subtree's active job generations"
    );
    if let Some(id) = accepted_job_id {
        ensure!(
            plan.complete_job_ids == [id.to_owned()],
            "handled error must complete its accepted job"
        );
    }
    let mut stmt = tx.prepare("SELECT incident_id,scope_id,job_id FROM bpmn_incidents WHERE instance_id=?1 AND resolved_at_ms IS NULL")?;
    for (id, scope, job) in stmt
        .query_map([&instance_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    {
        let own_job = if let Some(job) = job {
            tx.query_row(
                "SELECT token_id=?1 FROM bpmn_jobs WHERE job_id=?2 AND instance_id=?3",
                params![token_id, job, instance_id],
                |row| row.get::<_, bool>(0),
            )
            .optional()?
            .unwrap_or(false)
        } else {
            false
        };
        if active_scopes.contains(&scope) || (interrupt && own_job) {
            own_incidents.push(id);
        }
    }
    if interrupt {
        let mut stmt = tx.prepare("SELECT data_json FROM bpmn_events WHERE instance_id=?1 AND scope_id=?2 AND kind='scope_entry_failed'")?;
        for data in stmt
            .query_map(params![instance_id, scope_id], |row| {
                row.get::<_, String>(0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        {
            let data: Value = parse(data)?;
            if data["parent_token_id"].as_str() == Some(token_id.as_str()) {
                if let Some(id) = data["incident_id"].as_str() {
                    let open:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM bpmn_incidents WHERE instance_id=?1 AND scope_id=?2 AND incident_id=?3 AND resolved_at_ms IS NULL)",params![instance_id,scope_id,id],|row|row.get(0))?;
                    if open {
                        own_incidents.push(id.to_owned());
                    }
                }
            }
        }
    }
    let mut stmt=tx.prepare("SELECT scope_id,join_node_id,activation_id,branch_edge_id,token_id,gateway_kind FROM bpmn_gateway_receipts WHERE instance_id=?1")?;
    let receipts = stmt
        .query_map([&instance_id], |row| {
            Ok(GatewayReceipt {
                scope_id: row.get(0)?,
                join_node_id: row.get(1)?,
                activation_id: row.get(2)?,
                branch_edge_id: row.get(3)?,
                token_id: row.get(4)?,
                gateway_kind: match row.get::<_, String>(5)?.as_str() { "parallel" => GatewayKind::Parallel, "inclusive" => GatewayKind::Inclusive, _ => return Err(rusqlite::Error::InvalidQuery) },
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        receipts
            .iter()
            .filter(|row| active_scopes.contains(&row.scope_id))
            .all(|row| plan
                .remove_gateway_receipts
                .iter()
                .any(|removed| removed.scope_id == row.scope_id
                    && removed.token_id == row.token_id
                    && removed.activation_id == row.activation_id
                    && removed.branch_edge_id == row.branch_edge_id
                    && removed.join_node_id == row.join_node_id)),
        "scope interruption omits a join receipt"
    );
    for scope in scopes
        .iter()
        .filter(|scope| active_scopes.contains(&scope.scope_id))
    {
        ensure!(
            plan.scope_updates
                .iter()
                .any(|update| update.scope_id == scope.scope_id
                    && update.expected_revision == scope.revision
                    && update.status == ProcessInstanceStatus::Cancelled
                    && update.variables.is_none()),
            "scope interruption must retain locals and close every active scope"
        );
        ensure!(
            plan.events
                .iter()
                .any(|event| event.scope_id == scope.scope_id
                    && event.kind == "scope_cancelled"
                    && event.data["parent_token_id"].as_str() == scope.parent_token_id.as_deref()),
            "scope interruption lacks actual activation history"
        );
    }
    own_incidents.sort();
    own_incidents.dedup();
    ensure!(
        equal_ids(&plan.resolve_incident_ids, &own_incidents),
        "boundary resolves an unrelated incident or omits its linked incident"
    );
    let timers = timer_ids_on(tx, &instance_id)?
        .iter()
        .map(|id| timer_on(tx, id))
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .filter(|timer| {
            (active_scopes.contains(timer.scope_id.as_deref().unwrap_or(""))
                || (timer.kind == ProcessTimerKind::Boundary
                    && timer.token_id.as_deref() == Some(token_id.as_str())))
                && matches!(
                    timer.status,
                    ProcessTimerStatus::Pending | ProcessTimerStatus::Blocked
                )
        })
        .collect::<Vec<_>>();
    let winner_timer = match activation {
        BoundaryActivationId::Timer(id) => Some(id.as_str()),
        _ => None,
    };
    let expected_timers = timers
        .iter()
        .filter(|timer| {
            active_scopes.contains(timer.scope_id.as_deref().unwrap_or(""))
                || interrupt
                || Some(timer.timer_id.as_str()) == winner_timer
        })
        .map(|timer| timer.timer_id.clone())
        .collect::<Vec<_>>();
    let current_timer_updates = plan
        .timer_updates
        .iter()
        .filter(|u| {
            !plan
                .create_timers
                .iter()
                .any(|timer| timer.timer_id == u.timer_id)
                && !timer_on(tx, &u.timer_id).is_ok_and(|timer| {
                    u.status == ProcessTimerStatus::Cancelled
                        && u.last_reason.as_deref() == Some("activity_completed")
                        && timer
                            .token_id
                            .as_ref()
                            .is_some_and(|id| plan.consume_token_ids.contains(id))
                })
        })
        .map(|u| u.timer_id.clone())
        .collect::<Vec<_>>();
    ensure!(
        equal_ids(&current_timer_updates, &expected_timers),
        "boundary must settle only its complete current timers and child subtree"
    );
    for timer in timers
        .iter()
        .filter(|timer| Some(timer.timer_id.as_str()) != winner_timer && interrupt)
    {
        ensure!(
            plan.timer_updates
                .iter()
                .any(|u| u.timer_id == timer.timer_id
                    && u.expected_revision == timer.revision
                    && u.occurrence == timer.occurrence
                    && u.fired_occurrence.is_none()
                    && u.due_at_ms.is_none()
                    && u.status == ProcessTimerStatus::Cancelled
                    && u.last_reason.as_deref()
                        == Some(
                            if active_scopes.contains(timer.scope_id.as_deref().unwrap_or("")) {
                                "scope_cancelled"
                            } else {
                                "sibling_interrupted"
                            }
                        )),
            "boundary timer sibling cancellation changed"
        );
    }
    let subscriptions = subscriptions_on(tx, &instance_id)?
        .into_iter()
        .filter(|sub| {
            (active_scopes.contains(&sub.scope_id)
                || (sub.token_id == token_id && sub.kind != ProcessSubscriptionKind::MessageCatch))
                && sub.status == ProcessSubscriptionStatus::Open
        })
        .collect::<Vec<_>>();
    let winner_sub = match activation {
        BoundaryActivationId::Subscription(id) => Some(id.as_str()),
        _ => None,
    };
    let expected_subs = subscriptions
        .iter()
        .filter(|sub| {
            active_scopes.contains(&sub.scope_id)
                || interrupt
                || Some(sub.subscription_id.as_str()) == winner_sub
        })
        .map(|sub| sub.subscription_id.clone())
        .collect::<Vec<_>>();
    let current_sub_updates = plan
        .subscription_updates
        .iter()
        .filter(|u| {
            !plan
                .create_subscriptions
                .iter()
                .any(|sub| sub.subscription_id == u.subscription_id)
                && !subscription_on(tx, &u.subscription_id).is_ok_and(|sub| {
                    u.status == ProcessSubscriptionStatus::Cancelled
                        && u.last_reason.as_deref() == Some("activity_completed")
                        && plan.consume_token_ids.contains(&sub.token_id)
                })
        })
        .map(|u| u.subscription_id.clone())
        .collect::<Vec<_>>();
    ensure!(
        equal_ids(&current_sub_updates, &expected_subs),
        "boundary must settle its actual subscription siblings and child subtree"
    );
    for sub in subscriptions {
        let winning = Some(sub.subscription_id.as_str()) == winner_sub;
        if active_scopes.contains(&sub.scope_id) || interrupt || winning {
            ensure!(
                plan.subscription_updates
                    .iter()
                    .any(|u| u.subscription_id == sub.subscription_id
                        && u.expected_revision == sub.revision
                        && u.status
                            == if winning {
                                ProcessSubscriptionStatus::Consumed
                            } else {
                                ProcessSubscriptionStatus::Cancelled
                            }
                        && u.last_reason.as_deref()
                            == if winning {
                                None
                            } else {
                                Some(if active_scopes.contains(&sub.scope_id) {
                                    "scope_cancelled"
                                } else {
                                    "sibling_interrupted"
                                })
                            }),
                "boundary subscription sibling cancellation changed"
            );
        }
    }
    let races = races_on(tx, &instance_id)?
        .into_iter()
        .filter(|race| {
            active_scopes.contains(&race.scope_id) && race.status == ProcessEventRaceStatus::Open
        })
        .collect::<Vec<_>>();
    ensure!(
        equal_ids(
            &plan
                .race_updates
                .iter()
                .map(|u| u.race_id.clone())
                .collect::<Vec<_>>(),
            &races
                .iter()
                .map(|race| race.race_id.clone())
                .collect::<Vec<_>>()
        ) && plan.race_updates.iter().all(|update| update.status
            == ProcessEventRaceStatus::Cancelled
            && update.winner_node_id.is_none()
            && update.winner_subscription_id.is_none()
            && update.winner_timer_id.is_none()),
        "boundary must close only its child subtree's open event races"
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
) -> Result<Option<ProcessTransitionOutcome>> {
    let timer = {
        let conn = pool.read()?;
        timer_on(&conn, &candidate.timer_id)?
    };
    let input = serde_json::json!({});
    let (source_id, initial) = if timer.kind == ProcessTimerKind::Start {
        (
            plan.start_instance_id
                .as_deref()
                .context("timer start lacks instance identity")?,
            Some((timer.definition_id.as_str(), timer.version, &input)),
        )
    } else {
        (
            timer
                .instance_id
                .as_deref()
                .context("activity timer lacks instance identity")?,
            None,
        )
    };
    let mut instance_plan = plan.clone();
    let start_update = if timer.kind == ProcessTimerKind::Start {
        let updates = instance_plan
            .timer_updates
            .iter()
            .filter(|update| update.timer_id == timer.timer_id)
            .cloned()
            .collect::<Vec<_>>();
        ensure!(updates.len() == 1, "timer fire requires one timer update");
        instance_plan
            .timer_updates
            .retain(|update| update.timer_id != timer.timer_id);
        updates.into_iter().next()
    } else {
        None
    };
    let (mut composite, mut conn) =
        prepare_call_plan(pool, actor, source_id, initial, &instance_plan, at_ms)?;
    if let Some(update) = start_update {
        composite.timer_updates.push(update);
    }
    let plan = &composite;
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
            revision == expected && !matches!(status.as_str(), "completed" | "cancelled" | "error")
        }) {
            return Ok(None);
        }
    }
    let updates = plan.timer_updates.iter()
        .filter(|update| update.timer_id == timer.timer_id)
        .collect::<Vec<_>>();
    ensure!(updates.len() == 1, "timer fire requires one timer update");
    if timer.kind == ProcessTimerKind::Boundary {
        validate_boundary_plan_on(
            &tx,
            &BoundaryActivationId::Timer(timer.timer_id.clone()),
            None,
            plan,
        )?;
    } else if timer.race_id.is_none() {
        ensure!(
            plan.timer_updates
                .iter()
                .all(|update| update.timer_id == timer.timer_id
                    || (update.status == ProcessTimerStatus::Cancelled
                        && update.last_reason.as_deref() == Some("activity_completed")
                        && timer_on(&tx, &update.timer_id).is_ok_and(|other| other.kind
                            == ProcessTimerKind::Boundary
                            && other
                                .token_id
                                .as_ref()
                                .is_some_and(|id| plan.consume_token_ids.contains(id))))),
            "timer fire cannot update an unrelated timer"
        );
        ensure!(
            reaches_error_end(plan)
                || (plan.cancel_token_ids.is_empty()
                    && plan.cancel_user_task_ids.is_empty()
                    && plan.cancel_job_ids.is_empty()
                    && plan.cancel_scope_roots.is_empty()),
            "only an interrupting boundary may cancel an activity"
        );
    }
    if let Some(race_id) = &timer.race_id {
        ensure!(
            (reaches_error_end(plan)
                || (plan.cancel_user_task_ids.is_empty()
                    && plan.cancel_job_ids.is_empty()
                    && plan.cancel_scope_roots.is_empty())),
            "event race cannot cancel another activity"
        );
        ensure!(
            plan.race_updates.len() == 1
                && plan.race_updates[0].race_id == *race_id
                && plan.race_updates[0].status == ProcessEventRaceStatus::Won
                && plan.race_updates[0].winner_timer_id.as_deref() == Some(timer.timer_id.as_str())
                && plan.race_updates[0].winner_subscription_id.is_none(),
            "timer firing does not win its actual event race"
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
        instance_plan
            .timer_updates
            .retain(|update| update.timer_id != timer.timer_id);
        start_instance_on(
            &tx, actor, instance_id, &timer.definition_id, timer.version,
            &serde_json::json!({}), &instance_plan, at_ms,
            Some((&timer.timer_id, advance.selected_occurrence)),
            None,
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
        instance_on(&tx, actor, instance_id, None)?
    };
    tx.commit()?;
    Ok(Some(ProcessTransitionOutcome {
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

fn canonical_json_value(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            let mut sorted = serde_json::Map::new();
            for key in keys {
                sorted.insert(key.clone(), canonical_json_value(&object[key]));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical_json_value).collect()),
        _ => value.clone(),
    }
}

fn expression_variables_hash(value: &Value) -> Result<String> {
    validate_variables(value)?;
    let mut hash = Sha256::new();
    hash.update(b"tentaflow:claim-vars:");
    hash.update(serde_json::to_vec(&canonical_json_value(value))?);
    Ok(hex::encode(hash.finalize()))
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
    let initiator = initiator.ok_or(ProcessAuthorityDenied("process instance not found"))?;
    if initiator == actor.user_id {
        return Ok(true);
    }
    let assigned: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM bpmn_user_tasks WHERE instance_id=?1 AND assignee_user_id=?2)",
        params![instance_id, actor.user_id],
        |row| row.get(0),
    )?;
    if !assigned {
        return Err(ProcessAuthorityDenied("process instance not found").into());
    }
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
) -> Result<(
    ProcessDefinition,
    Option<ProcessTimerSummary>,
    Option<ProcessMessageStartSummary>,
)> {
    read_snapshot(pool, |conn| {
        require_actor(conn, actor)?;
        require_owner(conn, actor, definition_id)?;
        let definition = definition_on(conn, definition_id)?;
        let timer_id: Option<String> = conn.query_row(
            "SELECT timer_id FROM bpmn_timers WHERE definition_id=?1 AND version=?2 AND kind='start'",
            params![definition_id, definition.published_version],
            |row| row.get(0),
        ).optional()?;
        let timer_start =
            if let (Some(id), Some(version)) = (timer_id, definition.published_version) {
                let model = current_version_model_on(conn, definition_id, version)?;
                Some(timer_summary(conn, &timer_on(conn, &id)?, &model)?)
            } else {
                None
            };
        let message_start = definition
            .published_version
            .map(|version| -> Result<Option<ProcessMessageStartSummary>> {
                let model = current_version_model_on(conn, definition_id, version)?;
                model
                    .nodes
                    .iter()
                    .find_map(|node| {
                        if let ProcessNodeKind::MessageStart { message_ref, .. } = &node.kind {
                            Some((node, message_ref))
                        } else {
                            None
                        }
                    })
                    .map(|(node, id)| {
                        Ok(ProcessMessageStartSummary {
                            node_id: node.id.clone(),
                            node_name: node.name.clone(),
                            message_name: model
                                .messages
                                .iter()
                                .find(|d| &d.message_id == id)
                                .context("message start declaration missing")?
                                .name
                                .clone(),
                            version,
                            can_send: !definition.archived,
                        })
                    })
                    .transpose()
            })
            .transpose()?
            .flatten();
        Ok((definition, timer_start, message_start))
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

fn call_pins_on(
    conn: &Connection,
    definition_id: &str,
    version: u32,
) -> Result<Vec<ProcessCallPinInfo>> {
    let mut query = conn.prepare("SELECT node_id,called_definition_id,called_version,called_element_json,model_sha256 FROM bpmn_call_pins WHERE definition_id=?1 AND version=?2 ORDER BY node_id")?;
    let rows = query
        .query_map(params![definition_id, version], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u32>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(
            |(node_id, called_definition_id, called_version, element, model_sha256)| {
                Ok(ProcessCallPinInfo {
                    node_id,
                    called_definition_id,
                    called_version,
                    called_element: parse(element)?,
                    model_sha256,
                })
            },
        )
        .collect()
}

fn call_status_text(status: &ProcessCallStatus) -> &'static str {
    match status {
        ProcessCallStatus::Waiting => "waiting",
        ProcessCallStatus::Returned => "returned",
        ProcessCallStatus::Error => "error",
        ProcessCallStatus::Cancelled => "cancelled",
        ProcessCallStatus::ReturnIncident => "return_incident",
    }
}

fn call_status_from_text(status: &str) -> Result<ProcessCallStatus> {
    Ok(match status {
        "waiting" => ProcessCallStatus::Waiting,
        "returned" => ProcessCallStatus::Returned,
        "error" => ProcessCallStatus::Error,
        "cancelled" => ProcessCallStatus::Cancelled,
        "return_incident" => ProcessCallStatus::ReturnIncident,
        _ => bail!("invalid process call status"),
    })
}

fn calls_on(conn: &Connection, instance_id: &str) -> Result<Vec<CallActivation>> {
    let mut q=conn.prepare("SELECT call_id,parent_instance_id,parent_scope_id,parent_token_id,call_node_id,child_instance_id,definition_id,version,called_definition_id,called_version,model_sha256,revision,status,created_at_ms,updated_at_ms FROM bpmn_calls WHERE parent_instance_id=?1 OR child_instance_id=?1 ORDER BY CASE WHEN status='waiting' THEN 0 ELSE 1 END,created_at_ms,call_id")?;
    let rows = q
        .query_map([instance_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, u32>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, u32>(9)?,
                r.get::<_, String>(10)?,
                row_u64(r, 11)?,
                r.get::<_, String>(12)?,
                r.get::<_, i64>(13)?,
                r.get::<_, i64>(14)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(
            |(
                call_id,
                parent_instance_id,
                parent_scope_id,
                parent_token_id,
                call_node_id,
                child_instance_id,
                definition_id,
                version,
                called_definition_id,
                called_version,
                model_sha256,
                revision,
                status,
                created_at_ms,
                updated_at_ms,
            )| {
                Ok(CallActivation {
                    call_id,
                    parent_instance_id,
                    parent_scope_id,
                    parent_token_id,
                    call_node_id,
                    child_instance_id,
                    definition_id,
                    version,
                    called_definition_id,
                    called_version,
                    model_sha256,
                    revision,
                    status: call_status_from_text(&status)?,
                    created_at_ms,
                    updated_at_ms,
                })
            },
        )
        .collect()
}

fn related_instance_on(
    conn: &Connection,
    actor: &ProcessActor,
    instance_id: &str,
) -> Result<Option<ProcessRelatedInstance>> {
    match require_instance_reader(conn, actor, instance_id) {
        Ok(_) => {}
        Err(error) if error.downcast_ref::<ProcessAuthorityDenied>().is_some() => return Ok(None),
        Err(error) => return Err(error),
    }
    let (definition_name,version,status):(String,u32,String)=conn.query_row("SELECT d.name,i.version,i.status FROM bpmn_instances i JOIN bpmn_definitions d ON d.definition_id=i.definition_id WHERE i.instance_id=?1 AND i.org_id=?2",params![instance_id,actor.org_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    Ok(Some(ProcessRelatedInstance {
        instance_id: instance_id.to_owned(),
        definition_name,
        version,
        status: status_from_text(&status)?,
        can_open: true,
    }))
}

fn call_page_on(
    conn: &Connection,
    actor: &ProcessActor,
    instance_id: &str,
    model: &ProcessModel,
    spec: Option<&ProcessPageSpec>,
) -> Result<(Vec<ProcessCallSummary>, ProcessPageInfo)> {
    let calls = calls_on(conn, instance_id)?;
    let (offset, limit) = detail_page_spec(spec)?;
    let rows = calls
        .iter()
        .skip(usize::try_from(offset)?)
        .take(usize::try_from(limit)?)
        .map(|call| {
            if call.parent_instance_id == instance_id {
                let node = scoped_node_on(
                    conn,
                    instance_id,
                    &call.parent_scope_id,
                    model,
                    &call.call_node_id,
                )?;
                Ok(ProcessCallSummary::Outgoing {
                    call_node_id: node.id.clone(),
                    call_node_name: node.name.clone(),
                    status: call.status.clone(),
                    child: related_instance_on(conn, actor, &call.child_instance_id)?,
                })
            } else {
                Ok(ProcessCallSummary::Incoming {
                    parent: related_instance_on(conn, actor, &call.parent_instance_id)?,
                })
            }
        })
        .collect::<Result<Vec<_>>>()?;
    let info = detail_page(spec, u32::try_from(calls.len())?, rows.len())?;
    Ok((rows, info))
}

fn require_call_version_on(
    conn: &Connection,
    actor: &ProcessActor,
    version: &ProcessVersion,
    new_admission: bool,
) -> Result<()> {
    require_actor(conn, actor)?;
    require_owner(conn, actor, &version.definition_id)?;
    if new_admission && definition_on(conn, &version.definition_id)?.archived {
        return Err(CallTransitionRejected("called process definition is archived").into());
    }
    ensure!(
        version
            .model
            .nodes
            .iter()
            .any(|n| n.kind == ProcessNodeKind::Start),
        "only an ordinary Start process can be called"
    );
    require_version_execution_on(conn, actor, &version.definition_id, version.version)
}

fn verify_call_pins_on(conn: &Connection, version: &ProcessVersion) -> Result<()> {
    let nodes = super::model::all_nodes(&version.model);
    let expected = nodes
        .iter()
        .filter(|node| matches!(node.kind, ProcessNodeKind::CallActivity { .. }))
        .collect::<Vec<_>>();
    ensure!(
        expected.len() == version.call_activities.len(),
        "published direct call pin set differs from its actual model"
    );
    for node in expected {
        let ProcessNodeKind::CallActivity {
            called_definition_id,
            called_version,
            called_element,
            ..
        } = &node.kind
        else {
            unreachable!()
        };
        let pin = version
            .call_activities
            .iter()
            .find(|pin| pin.node_id == node.id)
            .context("published direct call pin missing")?;
        let hash: String = conn.query_row(
            "SELECT model_sha256 FROM bpmn_versions WHERE definition_id=?1 AND version=?2",
            params![called_definition_id, called_version],
            |row| row.get(0),
        )?;
        ensure!(
            pin.called_definition_id == *called_definition_id
                && pin.called_version == *called_version
                && pin.called_element == *called_element
                && pin.model_sha256 == hash,
            "published direct call binding differs from immutable target"
        );
    }
    Ok(())
}

fn collect_call_versions_on(
    conn: &Connection,
    actor: &ProcessActor,
    definition_id: &str,
    version: u32,
    model: &ProcessModel,
    depth: usize,
    visiting: &mut HashSet<(String, u32)>,
    collected: &mut BTreeMap<(String, u32), PinnedCallVersion>,
    expanded: &mut BTreeMap<(String, u32), usize>,
    require_admission: bool,
) -> Result<usize> {
    let key = (definition_id.to_owned(), version);
    ensure!(
        visiting.insert(key.clone()),
        "process call version dependency cycle"
    );
    if let Some(height) = expanded.get(&key).copied() {
        ensure!(
            depth + height <= 3,
            "process call dependency depth exceeds three"
        );
        visiting.remove(&key);
        return Ok(height);
    }
    let mut height = 0;
    for node in super::model::all_nodes(model) {
        let ProcessNodeKind::CallActivity {
            called_definition_id,
            called_version,
            called_element,
            ..
        } = &node.kind
        else {
            continue;
        };
        ensure!(depth < 3, "process call dependency depth exceeds three");
        let child_key = (called_definition_id.clone(), *called_version);
        ensure!(
            !visiting.contains(&child_key),
            "process call version dependency cycle"
        );
        if !collected.contains_key(&child_key) {
            ensure!(
                collected.len() < 32,
                "process call dependencies exceed 32 distinct versions"
            );
            #[cfg(test)]
            CALL_VERSION_READS.with(|count| count.set(count.get() + 1));
            let target = version_on(conn, called_definition_id, *called_version)?;
            let (stored_model, services_json): (String, String) = conn.query_row(
                "SELECT model_json,service_snapshots_json FROM bpmn_versions WHERE definition_id=?1 AND version=?2",
                params![called_definition_id, called_version], |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            ensure!(
                target.model_sha256 == hex::encode(Sha256::digest(stored_model.as_bytes())),
                "called process model hash mismatch"
            );
            let services: Vec<PinnedServiceSnapshot> = parse(services_json)?;
            for service in &services {
                ensure!(
                    service.info.graph_sha256
                        == hex::encode(Sha256::digest(service.graph_json.as_bytes())),
                    "called service graph hash mismatch"
                );
            }
            verify_call_pins_on(conn, &target)?;
            if require_admission {
                require_call_version_on(conn, actor, &target, true)?;
            }
            let admission_error = if require_admission {
                None
            } else {
                require_call_version_on(conn, actor, &target, true)
                    .err()
                    .map(|error| format!("{error:#}"))
            };
            collected.insert(
                child_key.clone(),
                PinnedCallVersion {
                    version: target,
                    services,
                    name: definition_on(conn, called_definition_id)?.name,
                    admission_error,
                },
            );
        }
        let target = collected
            .get(&child_key)
            .context("resolved called version missing")?;
        ensure!(
            target.version.model.process_id == called_element.process_id
                && target
                    .version
                    .model
                    .target_namespace
                    .as_deref()
                    .unwrap_or("https://tentaflow.app/bpmn/1")
                    == called_element.namespace_uri,
            "called process QName differs from exact published target"
        );
        let child_height = if let Some(height) = expanded.get(&child_key).copied() {
            ensure!(
                depth + 1 + height <= 3,
                "process call dependency depth exceeds three"
            );
            height
        } else {
            let child_model = target.version.model.clone();
            collect_call_versions_on(
                conn,
                actor,
                called_definition_id,
                *called_version,
                &child_model,
                depth + 1,
                visiting,
                collected,
                expanded,
                require_admission,
            )?
        };
        height = height.max(child_height + 1);
    }
    visiting.remove(&key);
    expanded.insert(key, height);
    Ok(height)
}

fn pinned_call_versions_on(
    conn: &Connection,
    actor: &ProcessActor,
    definition_id: &str,
    version: u32,
    model: &ProcessModel,
) -> Result<Vec<PinnedCallVersion>> {
    let mut versions = BTreeMap::new();
    collect_call_versions_on(
        conn,
        actor,
        definition_id,
        version,
        model,
        0,
        &mut HashSet::new(),
        &mut versions,
        &mut BTreeMap::new(),
        false,
    )?;
    verify_call_pins_on(conn, &version_on(conn, definition_id, version)?)?;
    let mut statement=conn.prepare("SELECT called_definition_id,called_version,model_sha256 FROM bpmn_call_dependencies WHERE definition_id=?1 AND version=?2")?;
    let persisted = statement
        .query_map(params![definition_id, version], |row| {
            Ok((
                (row.get::<_, String>(0)?, row.get::<_, u32>(1)?),
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<BTreeMap<_, _>>>()?;
    ensure!(
        persisted.len() == versions.len()
            && versions
                .iter()
                .all(|(key, value)| persisted.get(key) == Some(&value.version.model_sha256)),
        "published transitive call closure differs from retained immutable dependencies"
    );
    Ok(versions.into_values().collect())
}

fn publish_call_pins_on(
    tx: &Transaction<'_>,
    actor: &ProcessActor,
    definition_id: &str,
    version: u32,
    model: &ProcessModel,
    model_json: &str,
    services_json: &str,
) -> Result<()> {
    if !super::model::all_nodes(model)
        .iter()
        .any(|node| matches!(node.kind, ProcessNodeKind::CallActivity { .. }))
    {
        return Ok(());
    }
    let mut versions = BTreeMap::new();
    collect_call_versions_on(
        tx,
        actor,
        definition_id,
        version,
        model,
        0,
        &mut HashSet::new(),
        &mut versions,
        &mut BTreeMap::new(),
        true,
    )?;
    let mut bytes = model_json
        .len()
        .checked_add(services_json.len())
        .context("process call byte budget overflow")?;
    for ((called_definition_id, called_version), snapshot) in &versions {
        let sizes: (u64, u64) = tx.query_row(
            "SELECT length(CAST(model_json AS BLOB)),length(CAST(service_snapshots_json AS BLOB)) FROM bpmn_versions WHERE definition_id=?1 AND version=?2",
            params![called_definition_id, called_version],
            |row| Ok((row_u64(row, 0)?, row_u64(row, 1)?)),
        )?;
        let model_bytes = usize::try_from(sizes.0)?;
        let service_bytes = usize::try_from(sizes.1)?;
        bytes = bytes
            .checked_add(model_bytes)
            .and_then(|v| v.checked_add(service_bytes))
            .context("process call byte budget overflow")?;
        ensure!(
            bytes <= 4 * 1024 * 1024,
            "process call pinned closure exceeds 4 MiB"
        );
        tx.execute("INSERT INTO bpmn_call_dependencies(definition_id,version,called_definition_id,called_version,model_sha256) VALUES(?1,?2,?3,?4,?5)",params![definition_id,version,called_definition_id,called_version,snapshot.version.model_sha256])?;
    }
    ensure!(
        bytes <= 4 * 1024 * 1024,
        "process call pinned closure exceeds 4 MiB"
    );
    for node in super::model::all_nodes(model) {
        let ProcessNodeKind::CallActivity {
            called_definition_id,
            called_version,
            called_element,
            ..
        } = &node.kind
        else {
            continue;
        };
        let target = versions
            .get(&(called_definition_id.clone(), *called_version))
            .context("resolved call target missing")?;
        tx.execute("INSERT INTO bpmn_call_pins(definition_id,version,node_id,called_definition_id,called_version,called_element_json,model_sha256) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![definition_id,version,node.id,called_definition_id,called_version,json(called_element)?,target.version.model_sha256])?;
    }
    Ok(())
}

fn version_on(conn: &Connection, definition_id: &str, version: u32) -> Result<ProcessVersion> {
    let call_activities = call_pins_on(conn, definition_id, version)?;
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
                service_flows: snapshots.into_iter().map(|snapshot| snapshot.info).collect(), call_activities:call_activities.clone() })
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
    let (original, _, _) = get_definition(pool, actor, definition_id)?;
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
    PUBLICATION_PREFLIGHT.with(|gate| {
        if let Some((ready, resume)) = gate.borrow().as_ref() {
            ready.send(()).expect("publication preflight barrier");
            resume.recv().expect("publication writer barrier");
        }
    });
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    require_actor(&tx, actor)?;
    require_owner(&tx, actor, definition_id)?;
    if let Some((_, identity)) =
        command_replay::<(serde_json::Value, serde_json::Value)>(&tx, actor, stamp)?
    {
        ensure!(
            identity["definition_id"].as_str() == Some(definition_id),
            "publication command belongs to another definition"
        );
        let version = u32::try_from(
            identity["version"]
                .as_u64()
                .context("publication version identity missing")?,
        )?;
        return Ok((
            definition_on(&tx, definition_id)?,
            version_on(&tx, definition_id, version)?,
        ));
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
    for node in super::model::all_nodes(&prepared_model) {
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
    publish_call_pins_on(
        &tx,
        actor,
        definition_id,
        version,
        &prepared_model,
        &model_json,
        &json(&snapshots)?,
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
                scope_id: None,
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

                race_id: None,
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
            #[derive(Deserialize)]
            struct DefinitionIdentity {
                definition_id: String,
            }
            #[derive(Deserialize)]
            struct VersionIdentity {
                definition_id: String,
                version: u32,
            }
            let prior: (DefinitionIdentity, VersionIdentity) = parse(result)?;
            ensure!(
                prior.0.definition_id == definition_id && prior.1.definition_id == definition_id,
                "publication command belongs to another definition"
            );
            return Ok(Some((
                definition_on(conn, definition_id)?,
                version_on(conn, definition_id, prior.1.version)?,
            )));
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
    if archived {
        let keys = message_keys_on(&tx, &actor.org_id, None, None, Some(definition_id), None)?;
        for key in keys {
            let message = message_on(&tx, &key, true)?;
            if matches!(message.target, ProcessMessageTarget::Start { .. })
                && matches!(
                    message.status,
                    ProcessMessageStatus::Pending
                        | ProcessMessageStatus::Blocked
                        | ProcessMessageStatus::Ambiguous
                )
            {
                update_message_state_on(
                    &tx,
                    &message,
                    ProcessMessageStatus::Cancelled,
                    Some("definition_archived"),
                    now,
                    now,
                )?;
            }
        }
    }
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
        ProcessInstanceStatus::Error => "error",
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
        "error" => ProcessInstanceStatus::Error,
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

pub(super) fn scope_path(
    scopes: &[ProcessScopeSummary],
    instance_id: &str,
    scope_id: &str,
) -> Result<Vec<String>> {
    let mut path = Vec::new();
    let mut current = scope_id;
    let mut seen = HashSet::new();
    loop {
        ensure!(seen.insert(current), "cyclic process scope ancestry");
        let scope = scopes
            .iter()
            .find(|s| s.scope_id == current)
            .context("process scope is missing from its instance")?;
        if current == instance_id {
            ensure!(
                scope.parent_scope_id.is_none() && scope.subprocess_node_id.is_none(),
                "root scope identity is invalid"
            );
            break;
        }
        ensure!(path.len() < 3, "process scope depth exceeds three");
        path.push(
            scope
                .subprocess_node_id
                .clone()
                .context("child scope has no subprocess node")?,
        );
        current = scope
            .parent_scope_id
            .as_deref()
            .context("child scope has no parent")?;
    }
    path.reverse();
    Ok(path)
}

pub(super) fn scope_node<'a>(
    model: &'a ProcessModel,
    scopes: &[ProcessScopeSummary],
    instance_id: &str,
    scope_id: &str,
    node_id: &str,
) -> Result<&'a ProcessNode> {
    let path = scope_path(scopes, instance_id, scope_id)?;
    let (nodes, _, _) = super::model::scope_body(model, &path)?;
    nodes
        .iter()
        .find(|node| node.id == node_id)
        .context("node does not belong to its actual process scope")
}

fn scopes_on(
    conn: &Connection,
    instance_id: &str,
    model: &ProcessModel,
) -> Result<Vec<ProcessScopeSummary>> {
    let mut statement = conn.prepare(
        "SELECT s.scope_id,s.parent_scope_id,s.subprocess_node_id,s.parent_token_id,COALESCE(s.revision,i.revision),COALESCE(s.status,i.status),COALESCE(s.created_at_ms,i.created_at_ms),COALESCE(s.updated_at_ms,i.updated_at_ms) FROM bpmn_scopes s JOIN bpmn_instances i ON i.instance_id=s.instance_id WHERE s.instance_id=?1 ORDER BY CASE WHEN COALESCE(s.status,i.status) IN ('completed','cancelled','error') THEN 1 ELSE 0 END,s.scope_id",
    )?;
    let rows = statement
        .query_map([instance_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row_u64(row, 4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ensure!(
        !rows.is_empty() && rows.len() <= 129,
        "process scope inventory is missing or exceeds its lifetime limit"
    );
    let mut scopes = rows
        .into_iter()
        .map(
            |(
                scope_id,
                parent_scope_id,
                subprocess_node_id,
                parent_token_id,
                revision,
                status,
                created_at_ms,
                updated_at_ms,
            )| {
                Ok(ProcessScopeSummary {
                    scope_id,
                    parent_scope_id,
                    subprocess_node_id,
                    subprocess_node_name: None,
                    terminal_error: None,
                    parent_token_id,
                    revision,
                    status: status_from_text(&status)?,
                    depth: 0,
                    created_at_ms,
                    updated_at_ms,
                })
            },
        )
        .collect::<Result<Vec<_>>>()?;
    for index in 0..scopes.len() {
        let path = scope_path(&scopes, instance_id, &scopes[index].scope_id)?;
        scopes[index].depth = u32::try_from(path.len())?;
        scopes[index].terminal_error=conn.query_row("SELECT CASE WHEN s.parent_scope_id IS NULL THEN i.terminal_error_json ELSE s.terminal_error_json END FROM bpmn_scopes s JOIN bpmn_instances i ON i.instance_id=s.instance_id WHERE s.scope_id=?1 AND s.instance_id=?2",params![scopes[index].scope_id,instance_id],|r|r.get::<_,Option<String>>(0))?.map(parse).transpose()?;
        if let (Some(parent), Some(node_id)) = (
            &scopes[index].parent_scope_id,
            &scopes[index].subprocess_node_id,
        ) {
            let node = scope_node(model, &scopes, instance_id, parent, node_id)?;
            ensure!(
                matches!(node.kind, ProcessNodeKind::SubProcess { .. }),
                "child scope parent node is not a subprocess"
            );
            scopes[index].subprocess_node_name = Some(node.name.clone());
        }
    }
    Ok(scopes)
}

fn scoped_node_on<'a>(
    conn: &Connection,
    instance_id: &str,
    scope_id: &str,
    model: &'a ProcessModel,
    node_id: &str,
) -> Result<&'a ProcessNode> {
    let scopes = scopes_on(conn, instance_id, model)?;
    scope_node(model, &scopes, instance_id, scope_id, node_id)
}

fn scope_variables_on(conn: &Connection, instance_id: &str, scope_id: &str) -> Result<Value> {
    let text: String = conn.query_row(
        "SELECT CASE WHEN s.scope_id=i.instance_id THEN i.variables_json ELSE s.local_variables_json END FROM bpmn_scopes s JOIN bpmn_instances i ON i.instance_id=s.instance_id WHERE s.instance_id=?1 AND s.scope_id=?2",
        params![instance_id,scope_id], |row| row.get(0),
    ).context("process scope local variables not found")?;
    let value = parse(text)?;
    validate_variables(&value)?;
    Ok(value)
}

pub(super) fn effective_scope_variables(
    scopes: &[ProcessScopeSummary],
    locals: &BTreeMap<String, Value>,
    instance_id: &str,
    root_variables: &Value,
    scope_id: &str,
) -> Result<Value> {
    let mut chain = Vec::new();
    let mut current = scope_id;
    let mut seen = HashSet::new();
    while current != instance_id {
        ensure!(seen.insert(current), "cyclic process variable ancestry");
        let scope = scopes
            .iter()
            .find(|scope| scope.scope_id == current)
            .context("process variable scope missing")?;
        ensure!(
            !matches!(
                scope.status,
                ProcessInstanceStatus::Completed
                    | ProcessInstanceStatus::Cancelled
                    | ProcessInstanceStatus::Error
            ),
            "terminal scope cannot supply active process variables"
        );
        chain.push(current);
        current = scope
            .parent_scope_id
            .as_deref()
            .context("variable scope has no parent")?;
    }
    let mut merged = root_variables
        .as_object()
        .context("root variables are not an object")?
        .clone();
    for id in chain.into_iter().rev() {
        let local = locals
            .get(id)
            .context("active scope local variables missing")?;
        for (key, value) in local
            .as_object()
            .context("scope variables are not an object")?
        {
            merged.insert(key.clone(), value.clone());
        }
    }
    let effective = Value::Object(merged);
    validate_variables(&effective)?;
    Ok(effective)
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
                    user_task_id: user_task_id.clone(),
                    node_id,
                    name,
                    assignee_user_id,
                    kind,
                    status,
                    outputs: parse(outputs_json)?,
                    revision,
                    can_complete,
                    token_id,
                    scope_id: conn.query_row(
                        "SELECT scope_id FROM bpmn_user_tasks WHERE user_task_id=?1",
                        [&user_task_id],
                        |row| row.get(0),
                    )?,
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
    selected_id: Option<&str>,
    offset: u32,
    limit: u32,
) -> Result<Vec<ProcessUserTaskSummary>> {
    let mut stmt = conn.prepare("SELECT user_task_id,node_id,name,assignee_user_id,kind,status,revision FROM bpmn_user_tasks WHERE instance_id=?1 AND (?2 IS NULL OR user_task_id=?2) ORDER BY CASE WHEN status='open' THEN 0 ELSE 1 END,created_at_ms DESC,user_task_id LIMIT ?3 OFFSET ?4")?;
    let rows = stmt
        .query_map(params![instance_id, selected_id, limit, offset], |row| {
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
                    scope_id: conn.query_row(
                        "SELECT scope_id FROM bpmn_user_tasks WHERE user_task_id=?1",
                        [&user_task_id],
                        |row| row.get(0),
                    )?,
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

fn detail_page(
    spec: Option<&ProcessPageSpec>,
    total: u32,
    returned: usize,
) -> Result<ProcessPageInfo> {
    let offset = spec.map_or(0, |p| p.offset);
    let next = offset
        .checked_add(u32::try_from(returned)?)
        .context("instance page overflow")?;
    let next_offset = (next < total).then_some(next);
    ensure!(
        next_offset.is_none() || returned > 0,
        "instance page cannot omit remaining full rows"
    );
    Ok(ProcessPageInfo {
        offset,
        total,
        next_offset,
        has_more: next_offset.is_some(),
    })
}
fn detail_page_spec(spec: Option<&ProcessPageSpec>) -> Result<(u32, u32)> {
    let (offset, limit) = spec.map_or((0, 20), |p| (p.offset, p.limit));
    ensure!(
        (1..=20).contains(&limit),
        "instance page limit must be 1..20"
    );
    Ok((offset, limit))
}
fn detail_ids_on(
    conn: &Connection,
    sql: &str,
    instance_id: &str,
    spec: Option<&ProcessPageSpec>,
) -> Result<Vec<String>> {
    let (offset, limit) = detail_page_spec(spec)?;
    let mut q = conn.prepare(sql)?;
    let ids = q
        .query_map(params![instance_id, limit, offset], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(ids)
}
fn incident_on(
    conn: &Connection,
    actor: &ProcessActor,
    instance_id: &str,
    id: &str,
    model: &ProcessModel,
) -> Result<ProcessIncidentSelection> {
    let (node_id,job_id,code,message,at_ms,resolved_at_ms):(Option<String>,Option<String>,String,String,i64,Option<i64>)=conn.query_row("SELECT node_id,job_id,code,message,at_ms,resolved_at_ms FROM bpmn_incidents WHERE instance_id=?1 AND incident_id=?2",params![instance_id,id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).context("process incident not found")?;
    let initiator = require_instance_reader(conn, actor, instance_id)?;
    let can_retry=initiator&&resolved_at_ms.is_none()&&match &job_id {Some(id)=>conn.query_row("SELECT EXISTS(SELECT 1 FROM bpmn_jobs WHERE job_id=?1 AND instance_id=?2 AND status IN ('completed','error') AND NOT (status='completed' AND result_origin='contract' AND json_extract(result_json,'$.outcome')='NeedsHuman'))",params![id,instance_id],|r|r.get::<_,bool>(0))?&&job_activation_live_on(conn,id)?,None=>false};
    let scope_id: String = conn.query_row(
        "SELECT scope_id FROM bpmn_incidents WHERE incident_id=?1 AND instance_id=?2",
        params![id, instance_id],
        |row| row.get(0),
    )?;
    let node_name = node_id
        .as_ref()
        .map(|node| -> Result<String> {
            Ok(scoped_node_on(conn, instance_id, &scope_id, model, node)?
                .name
                .clone())
        })
        .transpose()?;
    Ok(ProcessIncidentSelection {
        incident: ProcessIncident {
            incident_id: id.to_owned(),
            scope_id,
            node_id,
            node_name,
            job_id,
            code,
            message,
            at_ms,
            can_retry,
        },
        resolved_at_ms,
    })
}
fn incidents_on(
    conn: &Connection,
    actor: &ProcessActor,
    instance_id: &str,
    model: &ProcessModel,
) -> Result<Vec<ProcessIncident>> {
    let mut q=conn.prepare("SELECT incident_id FROM bpmn_incidents WHERE instance_id=?1 AND resolved_at_ms IS NULL ORDER BY at_ms,incident_id")?;
    let ids = q
        .query_map([instance_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ids.iter()
        .map(|id| Ok(incident_on(conn, actor, instance_id, id, model)?.incident))
        .collect()
}
fn subscription_summary(
    conn: &Connection,
    s: &EventSubscription,
    model: &ProcessModel,
) -> Result<ProcessSubscriptionSummary> {
    let n = scoped_node_on(conn, &s.instance_id, &s.scope_id, model, &s.node_id)?;
    let attached_to_id = match &n.kind {
        ProcessNodeKind::BoundaryMessage { attached_to_id, .. }
        | ProcessNodeKind::BoundaryError { attached_to_id, .. }
        | ProcessNodeKind::BoundaryEscalation { attached_to_id, .. } => {
            Some(attached_to_id.clone())
        }
        _ => None,
    };
    Ok(ProcessSubscriptionSummary {
        subscription_id: s.subscription_id.clone(),
        scope_id: s.scope_id.clone(),
        node_id: s.node_id.clone(),
        node_name: n.name.clone(),
        token_id: s.token_id.clone(),
        kind: s.kind.clone(),
        status: s.status.clone(),
        revision: s.revision,
        message_name: s.message_name.clone(),
        correlation_key: s.correlation_key.clone(),
        error_code: s.error_code.clone(),
        escalation_code: s.escalation_code.clone(),
        attached_to_id,
        race_id: s.race_id.clone(),
        last_reason: s.last_reason.clone(),
    })
}
fn race_summary_on(
    conn: &Connection,
    r: &EventRace,
    model: &ProcessModel,
) -> Result<ProcessEventRaceSummary> {
    let n = scoped_node_on(conn, &r.instance_id, &r.scope_id, model, &r.gateway_node_id)?;
    let mut q=conn.prepare("SELECT subscription_id FROM bpmn_event_subscriptions WHERE race_id=?1 ORDER BY subscription_id")?;
    let branch_subscription_ids = q
        .query_map([&r.race_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    let mut q =
        conn.prepare("SELECT timer_id FROM bpmn_timers WHERE race_id=?1 ORDER BY timer_id")?;
    let branch_timer_ids = q
        .query_map([&r.race_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(ProcessEventRaceSummary {
        race_id: r.race_id.clone(),
        scope_id: r.scope_id.clone(),
        gateway_node_id: r.gateway_node_id.clone(),
        gateway_name: n.name.clone(),
        status: r.status.clone(),
        revision: r.revision,
        winner_node_id: r.winner_node_id.clone(),
        branch_subscription_ids,
        branch_timer_ids,
    })
}
fn outgoing_page_on(
    conn: &Connection,
    actor: &ProcessActor,
    instance_id: &str,
    spec: Option<&ProcessPageSpec>,
) -> Result<(Vec<ProcessMessageSummary>, ProcessPageInfo)> {
    let (offset, limit) = detail_page_spec(spec)?;
    let mut total = 0u32;
    let mut rows = Vec::new();
    let mut statement = conn.prepare(MESSAGE_KEYS_SQL)?;
    let mut keys = statement.query(params![
        actor.org_id,
        instance_id,
        None::<&str>,
        None::<&str>,
        None::<&str>
    ])?;
    while let Some(row) = keys.next()? {
        let key = MessageKey {
            org_id: actor.org_id.clone(),
            sender_user_id: row.get(0)?,
            message_id: row.get(1)?,
        };
        let m = message_on(conn, &key, false)?;
        let summary = match message_summary_on(conn, actor, &m) {
            Ok(s) => s,
            Err(e) if e.downcast_ref::<ProcessAuthorityDenied>().is_some() => continue,
            Err(e) => return Err(e),
        };
        if total >= offset && rows.len() < usize::try_from(limit)? {
            rows.push(summary);
        }
        total = total
            .checked_add(1)
            .context("outgoing message count overflow")?;
    }
    let page = detail_page(spec, total, rows.len())?;
    Ok((rows, page))
}
fn instance_on(
    conn: &Connection,
    actor: &ProcessActor,
    instance_id: &str,
    pages: Option<&ProcessInstancePageRequest>,
) -> Result<ProcessInstance> {
    let is_initiator = require_instance_reader(conn, actor, instance_id)?;
    instance_state_on(conn, actor, instance_id, pages, is_initiator)
}

fn instance_state_on(
    conn: &Connection,
    actor: &ProcessActor,
    instance_id: &str,
    pages: Option<&ProcessInstancePageRequest>,
    is_initiator: bool,
) -> Result<ProcessInstance> {
    let (definition_id,version,initiator_user_id,revision,status,variables_json,created_at_ms,updated_at_ms):(String,u32,String,u64,String,String,i64,i64)=conn.query_row("SELECT definition_id,version,initiator_user_id,revision,status,variables_json,created_at_ms,updated_at_ms FROM bpmn_instances WHERE instance_id=?1 AND org_id=?2",params![instance_id,actor.org_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,row_u64(r,3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?))).context("process instance not found")?;
    let model = current_version_model_on(conn, &definition_id, version)?;
    let definition_name = conn.query_row(
        "SELECT name FROM bpmn_definitions WHERE definition_id=?1",
        [&definition_id],
        |r| r.get(0),
    )?;
    let mut q=conn.prepare("SELECT DISTINCT node_id FROM bpmn_tokens WHERE instance_id=?1 AND status IN ('ready','waiting','joining') ORDER BY node_id")?;
    let active_node_ids = q
        .query_map([instance_id], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    let spec = |f: fn(&ProcessInstancePageRequest) -> Option<&ProcessPageSpec>| pages.and_then(f);
    let task_spec = spec(|p| p.user_tasks.as_ref());
    let incident_spec = spec(|p| p.incidents.as_ref());
    let timer_spec = spec(|p| p.timers.as_ref());
    let sub_spec = spec(|p| p.subscriptions.as_ref());
    let race_spec = spec(|p| p.event_races.as_ref());
    let out_spec = spec(|p| p.outgoing_messages.as_ref());
    let scope_spec = spec(|p| p.scopes.as_ref());
    let call_spec = spec(|p| p.calls.as_ref());
    let (calls, call_page) = call_page_on(conn, actor, instance_id, &model, call_spec)?;
    let scope_inventory = scopes_on(conn, instance_id, &model)?;
    let (scope_offset, scope_limit) = detail_page_spec(scope_spec)?;
    let scopes = scope_inventory
        .iter()
        .skip(usize::try_from(scope_offset)?)
        .take(usize::try_from(scope_limit)?)
        .cloned()
        .collect::<Vec<_>>();
    let counts:(u32,u32,u32,u32,u32)=conn.query_row("SELECT (SELECT COUNT(*) FROM bpmn_user_tasks WHERE instance_id=?1),(SELECT COUNT(*) FROM bpmn_incidents WHERE instance_id=?1 AND resolved_at_ms IS NULL),(SELECT COUNT(*) FROM bpmn_timers WHERE instance_id=?1),(SELECT COUNT(*) FROM bpmn_event_subscriptions WHERE instance_id=?1),(SELECT COUNT(*) FROM bpmn_event_races WHERE instance_id=?1)",[instance_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
    let (offset, limit) = detail_page_spec(task_spec)?;
    let user_tasks = task_summaries_on(conn, actor, instance_id, None, offset, limit)?;
    let incident_ids=detail_ids_on(conn,"SELECT incident_id FROM bpmn_incidents WHERE instance_id=?1 AND resolved_at_ms IS NULL ORDER BY at_ms DESC,incident_id LIMIT ?2 OFFSET ?3",instance_id,incident_spec)?;
    let incidents = incident_ids
        .iter()
        .map(|id| Ok(incident_on(conn, actor, instance_id, id, &model)?.incident))
        .collect::<Result<Vec<_>>>()?;
    let timers=detail_ids_on(conn,"SELECT timer_id FROM bpmn_timers WHERE instance_id=?1 ORDER BY CASE WHEN status IN ('pending','blocked') THEN 0 WHEN status='error' THEN 1 ELSE 2 END,created_at_ms DESC,timer_id LIMIT ?2 OFFSET ?3",instance_id,timer_spec)?.iter().map(|id|timer_summary(conn,&timer_on(conn,id)?,&model)).collect::<Result<Vec<_>>>()?;
    let subscriptions=detail_ids_on(conn,"SELECT subscription_id FROM bpmn_event_subscriptions WHERE instance_id=?1 ORDER BY CASE WHEN status='open' THEN 0 WHEN status='error' THEN 1 ELSE 2 END,created_at_ms DESC,subscription_id LIMIT ?2 OFFSET ?3",instance_id,sub_spec)?.iter().map(|id|subscription_summary(conn,&subscription_on(conn,id)?,&model)).collect::<Result<Vec<_>>>()?;
    let event_races=detail_ids_on(conn,"SELECT race_id FROM bpmn_event_races WHERE instance_id=?1 ORDER BY CASE WHEN status='open' THEN 0 ELSE 1 END,created_at_ms DESC,race_id LIMIT ?2 OFFSET ?3",instance_id,race_spec)?.iter().map(|id|race_summary_on(conn,&race_on(conn,id)?,&model)).collect::<Result<Vec<_>>>()?;
    let (outgoing_messages, outgoing_page) = outgoing_page_on(conn, actor, instance_id, out_spec)?;
    let selected_user_task = pages
        .and_then(|p| p.selected_user_task_id.as_deref())
        .map(|id| {
            task_summaries_on(conn, actor, instance_id, Some(id), 0, 1)?
                .pop()
                .context("selected task not found")
        })
        .transpose()?;
    let selected_incident = pages
        .and_then(|p| p.selected_incident_id.as_deref())
        .map(|id| incident_on(conn, actor, instance_id, id, &model))
        .transpose()?;
    let can_retry = is_initiator
        && incidents_on(conn, actor, instance_id, &model)?
            .iter()
            .any(|i| i.can_retry);
    let status = status_from_text(&status)?;
    let closed = matches!(
        status,
        ProcessInstanceStatus::Completed
            | ProcessInstanceStatus::Cancelled
            | ProcessInstanceStatus::Error
    );
    let mut message_names = super::model::all_nodes(&model)
        .into_iter()
        .filter_map(|n| match &n.kind {
            ProcessNodeKind::MessageCatch { message_ref, .. }
            | ProcessNodeKind::BoundaryMessage { message_ref, .. } => Some(message_ref),
            _ => None,
        })
        .map(|id| {
            model
                .messages
                .iter()
                .find(|d| &d.message_id == id)
                .map(|d| d.name.clone())
                .context("pinned receiving declaration missing")
        })
        .collect::<Result<Vec<_>>>()?;
    message_names.sort();
    message_names.dedup();
    let page_info = ProcessInstancePageInfo {
        calls: call_page,
        user_tasks: detail_page(task_spec, counts.0, user_tasks.len())?,
        incidents: detail_page(incident_spec, counts.1, incidents.len())?,
        timers: detail_page(timer_spec, counts.2, timers.len())?,
        subscriptions: detail_page(sub_spec, counts.3, subscriptions.len())?,
        event_races: detail_page(race_spec, counts.4, event_races.len())?,
        outgoing_messages: outgoing_page,
        scopes: detail_page(
            scope_spec,
            u32::try_from(scope_inventory.len())?,
            scopes.len(),
        )?,
    };
    let instance = ProcessInstance {
        instance_id: instance_id.to_owned(),
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
        can_cancel: is_initiator && !closed,
        can_retry,
        timers,
        subscriptions,
        event_races,
        outgoing_messages,
        message_names,
        can_send_message: Some(!closed),
        pages: Some(page_info),
        selected_user_task,
        selected_incident,
        scopes,
        calls,
        terminal_error: conn
            .query_row(
                "SELECT terminal_error_json FROM bpmn_instances WHERE instance_id=?1",
                [instance_id],
                |r| r.get::<_, Option<String>>(0),
            )?
            .map(parse)
            .transpose()?,
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
    instance_id: &str,
) -> Result<ProcessInstance> {
    instance_on(conn, actor, instance_id, None)
}

pub fn get_instance(
    pool: &DbPool,
    actor: &ProcessActor,
    instance_id: &str,
    pages: Option<&ProcessInstancePageRequest>,
) -> Result<ProcessInstance> {
    read_snapshot(pool, |conn| {
        require_actor(conn, actor)?;
        instance_on(conn, actor, instance_id, pages)
    })
}

pub fn get_scope(
    pool: &DbPool,
    actor: &ProcessActor,
    instance_id: &str,
    scope_id: &str,
) -> Result<(ProcessScopeSummary, Value, Vec<String>)> {
    read_snapshot(pool, |conn| {
        require_actor(conn, actor)?;
        require_instance_reader(conn, actor, instance_id)?;
        let (definition_id, version): (String, u32) = conn.query_row(
            "SELECT definition_id,version FROM bpmn_instances WHERE instance_id=?1 AND org_id=?2",
            params![instance_id, actor.org_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let model = current_version_model_on(conn, &definition_id, version)?;
        let scope = scopes_on(conn, instance_id, &model)?
            .into_iter()
            .find(|scope| scope.scope_id == scope_id)
            .context("process scope not found")?;
        let variables = scope_variables_on(conn, instance_id, scope_id)?;
        let mut statement = conn.prepare("SELECT DISTINCT node_id FROM bpmn_tokens WHERE instance_id=?1 AND scope_id=?2 AND status IN ('ready','waiting','joining') ORDER BY node_id")?;
        let active_node_ids = statement
            .query_map(params![instance_id, scope_id], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        let frame = tentaflow_protocol::cbor::encode(
            &tentaflow_protocol::message_body::MessageBody::ProcessBody(
                ProcessPayload::ScopeGetResponse {
                    scope: scope.clone(),
                    variables: variables.clone(),
                    active_node_ids: active_node_ids.clone(),
                },
            ),
        )
        .map_err(anyhow::Error::msg)?;
        ensure!(
            frame.len() <= 900 * 1024,
            "process scope detail exceeds the wire budget"
        );
        Ok((scope, variables, active_node_ids))
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
        let sql=format!("SELECT i.instance_id,i.definition_id,d.name,i.initiator_user_id,i.version,i.revision,i.status,i.created_at_ms,i.updated_at_ms,EXISTS(SELECT 1 FROM bpmn_incidents x JOIN bpmn_jobs j ON j.job_id=x.job_id AND j.instance_id=x.instance_id JOIN bpmn_tokens t ON t.token_id=j.token_id AND t.instance_id=j.instance_id AND t.node_id=j.node_id WHERE x.instance_id=i.instance_id AND x.resolved_at_ms IS NULL AND j.status IN ('error','completed') AND NOT (j.status='completed' AND j.result_origin='contract' AND json_extract(j.result_json,'$.outcome')='NeedsHuman') AND t.status='waiting' AND i.status='incident'){where_sql} ORDER BY i.updated_at_ms DESC,i.instance_id DESC LIMIT ?4 OFFSET ?5");
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
                                ProcessInstanceStatus::Completed
                                    | ProcessInstanceStatus::Cancelled
                                    | ProcessInstanceStatus::Error
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
        let names: std::collections::HashMap<&str, &str> = super::model::all_nodes(&model)
            .into_iter()
            .map(|node| (node.id.as_str(), node.name.as_str()))
            .collect();
        let mut stmt = conn.prepare("SELECT event_id,seq,at_ms,kind,node_id,actor_user_id,data_json,scope_id FROM bpmn_events WHERE instance_id=?1 AND seq>?2 ORDER BY seq LIMIT ?3")?;
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
                        row.get::<_, String>(7)?,
                    ))
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut events = Vec::new();
        for (event_id, seq, at_ms, kind, node_id, actor_user_id, data_json, scope_id) in rows {
            let node_name = node_id
                .as_ref()
                .and_then(|id| names.get(id.as_str()).map(|name| (*name).to_string()));
            events.push(ProcessEvent {
                scope_id,
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
    read_snapshot(pool, |conn| {
        require_actor(conn, actor)?;
        require_instance_reader(conn, actor, instance_id)?;
        runtime_snapshot_on(conn, actor, instance_id)
    })
}

fn runtime_snapshot_on(
    conn: &Connection,
    actor: &ProcessActor,
    instance_id: &str,
) -> Result<RuntimeSnapshot> {
    let is_initiator = require_instance_reader(conn, actor, instance_id)?;
    let instance = instance_state_on(conn, actor, instance_id, None, is_initiator)?;
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
                    scope_id: conn.query_row(
                        "SELECT scope_id FROM bpmn_tokens WHERE token_id=?1",
                        [&token_id],
                        |row| row.get(0),
                    )?,
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
                let result_origin = conn
                    .query_row(
                        "SELECT result_origin FROM bpmn_jobs WHERE job_id=?1",
                        [&job_id],
                        |r| r.get::<_, Option<String>>(0),
                    )?
                    .as_deref()
                    .map(result_origin)
                    .transpose()?;
                Ok(ProcessJob {
                    scope_id: conn.query_row(
                        "SELECT scope_id FROM bpmn_jobs WHERE job_id=?1",
                        [&job_id],
                        |row| row.get(0),
                    )?,
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

                    result_origin,
                })
            },
        )
        .collect::<Result<Vec<_>>>()?;
    let mut receipt_stmt = conn.prepare("SELECT join_node_id,activation_id,branch_edge_id,token_id,scope_id,gateway_kind FROM bpmn_gateway_receipts WHERE instance_id=?1")?;
    let receipts = receipt_stmt.query_map([instance_id], |row| {
        Ok(GatewayReceipt {
            join_node_id: row.get(0)?, activation_id: row.get(1)?,
            branch_edge_id: row.get(2)?, token_id: row.get(3)?, scope_id: row.get(4)?,
            gateway_kind: match row.get::<_, String>(5)?.as_str() {
                "parallel" => GatewayKind::Parallel, "inclusive" => GatewayKind::Inclusive,
                _ => return Err(rusqlite::Error::InvalidQuery),
            },
        })
    })?.collect::<rusqlite::Result<Vec<_>>>()?;
    drop(token_stmt);
    drop(job_stmt);
    drop(receipt_stmt);
    let timers = timer_ids_on(conn, instance_id)?
        .into_iter()
        .map(|id| timer_on(conn, &id))
        .collect::<Result<Vec<_>>>()?;
    let incidents = incidents_on(conn, actor, instance_id, &model)?;
    let subscriptions = subscriptions_on(conn, instance_id)?;
    let event_races = races_on(conn, instance_id)?;
    let scopes = scopes_on(conn, instance_id, &model)?;
    let mut scope_variables = BTreeMap::new();
    for scope in &scopes {
        if scope.scope_id != instance_id
            && !matches!(
                scope.status,
                ProcessInstanceStatus::Completed
                    | ProcessInstanceStatus::Cancelled
                    | ProcessInstanceStatus::Error
            )
        {
            scope_variables.insert(
                scope.scope_id.clone(),
                scope_variables_on(conn, instance_id, &scope.scope_id)?,
            );
        }
    }
    let retained_scope_count = call_tree_ids_on(conn, instance_id)?.iter().try_fold(
        0usize,
        |total, id| -> Result<usize> {
            let count = usize::try_from(conn.query_row(
                "SELECT COUNT(*) FROM bpmn_scopes WHERE instance_id=?1",
                [id],
                |row| row_u64(row, 0),
            )?)?;
            total
                .checked_add(count)
                .context("call scope count overflow")
        },
    )?;
    let calls = calls_on(conn, instance_id)?;
    let call_versions = pinned_call_versions_on(
        conn,
        actor,
        &instance.definition_id,
        instance.version,
        &model,
    )?;
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
        subscriptions,
        event_races,
        incidents,
        scopes,
        scope_variables,
        calls,
        call_versions,
        retained_scope_count,
    })
}

fn insert_event_on(
    tx: &Transaction<'_>,
    instance_id: &str,
    seq: u64,
    actor: Option<&str>,
    event: &PlannedEvent,
    at_ms: i64,
    planned_event_id: Option<&str>,
) -> Result<String> {
    ensure!(
        json(&event.data)?.len() <= 384 * 1024,
        "process event data exceeds 384 KiB"
    );
    let event_id = planned_event_id
        .map(str::to_owned)
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    ensure!(
        Uuid::parse_str(&event_id).is_ok(),
        "process event identity must be a UUID"
    );
    tx.execute(
        "INSERT INTO bpmn_events(event_id,instance_id,seq,at_ms,kind,node_id,actor_user_id,data_json,scope_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![event_id,instance_id,sql_integer(seq)?,at_ms,event.kind,event.node_id,actor,json(&event.data)?,event.scope_id],
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
    Ok(event_id)
}

fn validate_terminal_error_on(
    conn: &Connection,
    instance_id: &str,
    fact: &ProcessTerminalError,
) -> Result<()> {
    ensure!(
        json(fact)?.len() <= 4096,
        "terminal business error exceeds 4 KiB"
    );
    let (definition_id, version): (String, u32) = conn.query_row(
        "SELECT definition_id,version FROM bpmn_instances WHERE instance_id=?1",
        [instance_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let model = current_version_model_on(conn, &definition_id, version)?;
    let node = scoped_node_on(
        conn,
        instance_id,
        &fact.source_scope_id,
        &model,
        &fact.source_node_id,
    )?;
    ensure!(
        matches!(&node.kind,ProcessNodeKind::ErrorEnd {error_ref} if error_ref==&fact.error_ref),
        "terminal error is not the exact pinned ErrorEnd"
    );
    ensure!(
        model.errors.iter().any(|e| e.error_id == fact.error_ref
            && e.error_code == fact.error_code
            && !fact.error_code.is_empty()),
        "terminal error code differs from its actual declaration"
    );
    let text:String=conn.query_row("SELECT data_json FROM bpmn_events WHERE event_id=?1 AND instance_id=?2 AND scope_id=?3 AND node_id=?4 AND kind='error_end_reached'",params![fact.source_event_id,instance_id,fact.source_scope_id,fact.source_node_id],|r|r.get(0)).context("terminal ErrorEnd source event missing")?;
    let data: Value = parse(text)?;
    ensure!(
        data["source_event_id"].as_str() == Some(fact.source_event_id.as_str())
            && data["error_ref"].as_str() == Some(fact.error_ref.as_str())
            && data["error_code"].as_str() == Some(fact.error_code.as_str())
            && data["source_instance_id"].as_str() == Some(instance_id),
        "terminal ErrorEnd source identity mismatch"
    );
    let token = data["source_token_id"]
        .as_str()
        .context("ErrorEnd factual token missing")?;
    let actual:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM bpmn_tokens WHERE token_id=?1 AND instance_id=?2 AND scope_id=?3 AND node_id=?4 AND status IN ('consumed','cancelled'))",params![token,instance_id,fact.source_scope_id,fact.source_node_id],|r|r.get(0))?;
    ensure!(actual, "ErrorEnd has no actual retained source control");
    Ok(())
}

fn check_active_incident_budget_on(conn: &Connection, instance_id: &str) -> Result<()> {
    let incident_bytes:i64=conn.query_row("SELECT COALESCE(SUM(length(CAST(message AS BLOB))),0) FROM bpmn_incidents WHERE instance_id=?1 AND resolved_at_ms IS NULL",[instance_id],|row|row.get(0))?;
    ensure!(
        incident_bytes <= 64 * 1024,
        "process active incidents exceed instance detail budget"
    );
    Ok(())
}

pub(super) fn descendant_scope_ids(
    scopes: &[ProcessScopeSummary],
    root: &str,
) -> Result<HashSet<String>> {
    ensure!(
        scopes.iter().any(|scope| scope.scope_id == root),
        "scope closure root is missing"
    );
    let mut closure = HashSet::from([root.to_owned()]);
    loop {
        let previous = closure.len();
        for scope in scopes {
            if scope
                .parent_scope_id
                .as_ref()
                .is_some_and(|parent| closure.contains(parent))
            {
                closure.insert(scope.scope_id.clone());
            }
        }
        if closure.len() == previous {
            return Ok(closure);
        }
    }
}

fn validate_scope_plan_on(
    tx: &Transaction<'_>,
    instance_id: &str,
    plan: &RuntimePlan,
    at_ms: i64,
) -> Result<Vec<ProcessScopeSummary>> {
    let (definition_id, version): (String, u32) = tx.query_row(
        "SELECT definition_id,version FROM bpmn_instances WHERE instance_id=?1",
        [instance_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let model = current_version_model_on(tx, &definition_id, version)?;
    let mut scopes = scopes_on(tx, instance_id, &model)?;
    let retained_terminal = scopes
        .iter()
        .filter(|scope| {
            matches!(
                scope.status,
                ProcessInstanceStatus::Completed
                    | ProcessInstanceStatus::Cancelled
                    | ProcessInstanceStatus::Error
            )
        })
        .map(|scope| scope.scope_id.clone())
        .collect::<HashSet<_>>();

    ensure!(
        scopes
            .len()
            .checked_add(plan.create_scopes.len())
            .is_some_and(|total| total <= 129),
        "process lifetime scope capacity exceeds 129"
    );
    let mut locals = BTreeMap::new();
    for scope in &scopes {
        if scope.scope_id != instance_id
            && !matches!(
                scope.status,
                ProcessInstanceStatus::Completed
                    | ProcessInstanceStatus::Cancelled
                    | ProcessInstanceStatus::Error
            )
        {
            locals.insert(
                scope.scope_id.clone(),
                scope_variables_on(tx, instance_id, &scope.scope_id)?,
            );
        }
    }
    for child in &plan.create_scopes {
        ensure!(
            Uuid::parse_str(&child.scope_id).is_ok()
                && child.scope_id != instance_id
                && !scopes.iter().any(|scope| scope.scope_id == child.scope_id),
            "new process scope identity already exists or is invalid"
        );
        let parent = scopes
            .iter()
            .find(|scope| scope.scope_id == child.parent_scope_id)
            .context("new scope has no actual parent")?;
        ensure!(
            !matches!(
                parent.status,
                ProcessInstanceStatus::Completed
                    | ProcessInstanceStatus::Cancelled
                    | ProcessInstanceStatus::Error
            ),
            "new scope parent is terminal"
        );
        let node = scope_node(
            &model,
            &scopes,
            instance_id,
            &child.parent_scope_id,
            &child.subprocess_node_id,
        )?;
        ensure!(
            matches!(node.kind, ProcessNodeKind::SubProcess { .. }),
            "new scope node is not a subprocess in its parent body"
        );
        ensure!(
            plan.create_tokens
                .iter()
                .any(|token| token.token_id == child.parent_token_id
                    && token.scope_id == child.parent_scope_id
                    && token.node_id == child.subprocess_node_id
                    && token.status == "waiting"),
            "new child scope must own its new actual parent waiting token"
        );
        ensure!(
            plan.events.iter().any(|event| event.kind == "scope_entered"
                && event.scope_id == child.scope_id
                && event.data["scope_id"].as_str() == Some(child.scope_id.as_str())
                && event.data["parent_scope_id"].as_str() == Some(child.parent_scope_id.as_str())
                && event.data["parent_token_id"].as_str() == Some(child.parent_token_id.as_str())
                && event.data["subprocess_node_id"].as_str()
                    == Some(child.subprocess_node_id.as_str())),
            "new scope lacks its exact activation history"
        );
        validate_variables(&child.variables)?;
        let depth = parent
            .depth
            .checked_add(1)
            .context("process scope depth overflow")?;
        ensure!(depth <= 3, "process scope depth exceeds three");
        scopes.push(ProcessScopeSummary {
            scope_id: child.scope_id.clone(),
            parent_scope_id: Some(child.parent_scope_id.clone()),
            subprocess_node_id: Some(child.subprocess_node_id.clone()),
            subprocess_node_name: Some(node.name.clone()),
            terminal_error: None,
            parent_token_id: Some(child.parent_token_id.clone()),
            revision: 1,
            status: ProcessInstanceStatus::Running,
            depth,
            created_at_ms: at_ms,
            updated_at_ms: at_ms,
        });
        locals.insert(child.scope_id.clone(), child.variables.clone());
    }
    let mut updated = HashSet::new();
    for update in &plan.scope_updates {
        ensure!(
            update.scope_id != instance_id && updated.insert(&update.scope_id),
            "scope update duplicates or replaces canonical root state"
        );
        let scope = scopes
            .iter_mut()
            .find(|scope| scope.scope_id == update.scope_id)
            .context("updated scope is outside this instance")?;
        ensure!(
            scope.revision == update.expected_revision
                && !matches!(
                    scope.status,
                    ProcessInstanceStatus::Completed
                        | ProcessInstanceStatus::Cancelled
                        | ProcessInstanceStatus::Error
                ),
            "process scope revision conflict or terminal scope"
        );
        sql_incrementable(update.expected_revision)?;
        if let Some(variables) = &update.variables {
            validate_variables(variables)?;
            locals.insert(update.scope_id.clone(), variables.clone());
        }
        scope.status = update.status.clone();
    }
    validate_variables(&plan.variables)?;
    let mut active_bytes = if matches!(
        plan.status,
        ProcessInstanceStatus::Completed
            | ProcessInstanceStatus::Cancelled
            | ProcessInstanceStatus::Error
    ) {
        0usize
    } else {
        json(&plan.variables)?.len()
    };
    for scope in &scopes {
        if scope.scope_id == instance_id
            || matches!(
                scope.status,
                ProcessInstanceStatus::Completed
                    | ProcessInstanceStatus::Cancelled
                    | ProcessInstanceStatus::Error
            )
        {
            continue;
        }
        ensure!(
            !matches!(
                plan.status,
                ProcessInstanceStatus::Completed
                    | ProcessInstanceStatus::Cancelled
                    | ProcessInstanceStatus::Error
            ),
            "root cannot close with an active child scope"
        );
        let local = locals
            .get(&scope.scope_id)
            .context("active child has no proposed local variables")?;
        validate_variables(local)?;
        active_bytes = active_bytes
            .checked_add(json(local)?.len())
            .context("active scope variable budget overflow")?;
        effective_scope_variables(
            &scopes,
            &locals,
            instance_id,
            &plan.variables,
            &scope.scope_id,
        )?;
    }
    ensure!(
        active_bytes <= 1024 * 1024,
        "active process scope variables exceed 1 MiB"
    );
    for scope in &scopes {
        if scope.scope_id == instance_id
            || !matches!(
                scope.status,
                ProcessInstanceStatus::Completed
                    | ProcessInstanceStatus::Cancelled
                    | ProcessInstanceStatus::Error
            )
        {
            continue;
        }
        let descendants = descendant_scope_ids(&scopes, &scope.scope_id)?;
        ensure!(
            scopes
                .iter()
                .filter(|child| descendants.contains(&child.scope_id))
                .all(|child| matches!(
                    child.status,
                    ProcessInstanceStatus::Completed
                        | ProcessInstanceStatus::Cancelled
                        | ProcessInstanceStatus::Error
                )),
            "terminal scope still has an active descendant"
        );
    }
    for token in &plan.create_tokens {
        ensure!(
            Uuid::parse_str(&token.token_id).is_ok()
                && !retained_terminal.contains(&token.scope_id),
            "new process token identity or terminal scope is invalid"
        );
        let path = scope_path(&scopes, instance_id, &token.scope_id)?;
        let (nodes, flows, _) = super::model::scope_body(&model, &path)?;
        ensure!(
            nodes.iter().any(|node| node.id == token.node_id),
            "new token node is outside its exact scope body"
        );
        if let Some(edge) = &token.arrival_edge_id {
            ensure!(
                flows
                    .iter()
                    .any(|flow| &flow.id == edge && flow.target_id == token.node_id),
                "token arrival edge is outside its exact scope body"
            );
        }
        let pairs = super::model::gateway_pairs(nodes, flows)?;
        for frame in &token.fork_stack {
            let pair = pairs.get(&frame.split_node_id).context("token fork pair missing from its exact body")?;
            let selected = &frame.selected_branch_edge_ids;
            ensure!(pair.join_node_id == frame.join_node_id && pair.kind == frame.gateway_kind
                && !selected.is_empty()
                && selected.windows(2).all(|window| window[0] < window[1])
                && selected.binary_search(&frame.branch_edge_id).is_ok()
                && selected.iter().all(|edge| pair.branch_to_incoming_edge.contains_key(edge))
                && (frame.gateway_kind == GatewayKind::Inclusive
                    || selected.iter().map(String::as_str).eq(pair.branch_to_incoming_edge.keys().map(String::as_str))),
                "token fork frame differs from selected branches of its paired body");
        }
    }
    for event in &plan.events {
        ensure!(
            scopes.iter().any(|scope| scope.scope_id == event.scope_id),
            "event scope is outside its actual instance"
        );
        if event.kind == "parallel_split" {
            let node_id = event.node_id.as_deref().context("parallel split node missing")?;
            let activation = event.data["activation_id"].as_str().context("parallel split activation missing")?;
            let path = scope_path(&scopes, instance_id, &event.scope_id)?;
            let (nodes, flows, _) = super::model::scope_body(&model, &path)?;
            let pair = super::model::gateway_pairs(nodes, flows)?.remove(node_id)
                .context("parallel split event has no paired model")?;
            ensure!(pair.kind == GatewayKind::Parallel,
                "parallel split event refers to another gateway kind");
            let expected = pair.branch_to_incoming_edge.keys().map(String::as_str).collect::<HashSet<_>>();
            let frames = plan.create_tokens.iter().filter(|token| token.scope_id == event.scope_id)
                .flat_map(|token| token.fork_stack.iter().filter(|frame|
                    frame.activation_id == activation && frame.split_node_id == node_id))
                .collect::<Vec<_>>();
            ensure!(!frames.is_empty() && frames.iter().all(|frame|
                frame.gateway_kind == GatewayKind::Parallel
                    && frame.join_node_id == pair.join_node_id
                    && frame.selected_branch_edge_ids.iter().map(String::as_str).collect::<HashSet<_>>() == expected),
                "parallel split event has a mismatched planned fork frame");
            let created = frames.iter().map(|frame| frame.branch_edge_id.as_str()).collect::<HashSet<_>>();
            ensure!(created == expected, "parallel split did not create every paired branch");
        }
        if event.kind == "inclusive_split" {
            let node_id = event.node_id.as_deref().context("inclusive split node missing")?;
            let activation = event.data["activation_id"].as_str().context("inclusive split activation missing")?;
            let selected = event.data["selected_branch_edge_ids"].as_array()
                .context("inclusive split selected edges missing")?
                .iter().map(|edge| edge.as_str().map(str::to_owned)
                    .context("inclusive split selected edge is not a string"))
                .collect::<Result<Vec<_>>>()?;
            ensure!(!selected.is_empty() && selected.windows(2).all(|window| window[0] < window[1])
                && event.data["default_selected"].is_boolean(),
                "inclusive split event has invalid selected set");
            let node = scope_node(&model, &scopes, instance_id, &event.scope_id, node_id)?;
            let ProcessNodeKind::InclusiveGateway { default_flow_id } = &node.kind else {
                bail!("inclusive split event refers to another node kind")
            };
            ensure!(event.data["default_selected"].as_bool() == Some(default_flow_id.as_ref().is_some_and(|edge| selected.len() == 1 && &selected[0] == edge)),
                "inclusive split default fact differs from selected edges");
            let created_frames = plan.create_tokens.iter().filter(|token| token.scope_id == event.scope_id)
                .flat_map(|token| token.fork_stack.iter().filter(|frame|
                    frame.activation_id == activation && frame.split_node_id == node_id))
                .collect::<Vec<_>>();
            ensure!(!created_frames.is_empty() && created_frames.iter().all(|frame|
                frame.gateway_kind == GatewayKind::Inclusive
                    && frame.selected_branch_edge_ids == selected
                    && selected.binary_search(&frame.branch_edge_id).is_ok()),
                "inclusive split event has a mismatched planned fork frame");
            let created = created_frames.iter().map(|frame| frame.branch_edge_id.as_str())
                .collect::<HashSet<_>>();
            ensure!(created.len() == selected.len() && selected.iter().all(|edge| created.contains(edge.as_str())),
                "inclusive split event lacks its actual selected branch tokens");
        }
        if event.kind == "incident" && event.data["code"] == "INCLUSIVE_GATEWAY_ERROR" {
            let source = event.data["source_token_id"].as_str().context("inclusive incident source token missing")?;
            let waiting = event.data["waiting_token_id"].as_str().context("inclusive incident waiting token missing")?;
            ensure!(matches!(event.data["reason"].as_str(), Some("no_matching_flow" | "condition_evaluation_failed" | "non_boolean_condition"))
                && plan.consume_token_ids.iter().any(|id| id == source)
                && plan.create_tokens.iter().any(|token| token.token_id == waiting
                    && token.scope_id == event.scope_id && token.node_id == event.node_id.as_deref().unwrap_or_default()
                    && token.status == "waiting" && token.fork_stack.iter().all(|frame| frame.gateway_kind != GatewayKind::Inclusive || frame.split_node_id != token.node_id))
                && plan.add_incidents.iter().any(|incident| incident.code == "INCLUSIVE_GATEWAY_ERROR"
                    && incident.scope_id == event.scope_id && incident.node_id == event.node_id
                    && incident.job_id.is_none() && !incident.can_retry),
                "inclusive incident lacks its actual waiting token and nonretryable incident");
        }
        if event.kind == "error_end_reached" {
            let node_id = event
                .node_id
                .as_ref()
                .context("ErrorEnd source node missing")?;
            let node = scope_node(&model, &scopes, instance_id, &event.scope_id, node_id)?;
            let ProcessNodeKind::ErrorEnd { error_ref } = &node.kind else {
                bail!("ErrorEnd facts reference another activity")
            };
            let declaration = model
                .errors
                .iter()
                .find(|error| &error.error_id == error_ref)
                .context("ErrorEnd declaration missing")?;
            let source_token = event.data["source_token_id"]
                .as_str()
                .context("ErrorEnd source token missing")?;
            let source_event = event.data["source_event_id"]
                .as_str()
                .context("ErrorEnd source event identity missing")?;
            let index = plan
                .events
                .iter()
                .position(|candidate| std::ptr::eq(candidate, event))
                .context("ErrorEnd event index missing")?;
            let retained:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM bpmn_tokens WHERE instance_id=?1 AND scope_id=?2 AND token_id=?3 AND node_id=?4 AND status='ready')",params![instance_id,event.scope_id,source_token,node.id],|row|row.get(0))?;
            ensure!(
                plan.event_ids.get(&index).map(String::as_str) == Some(source_event)
                    && Uuid::parse_str(source_event).is_ok()
                    && event.data["source_instance_id"].as_str() == Some(instance_id)
                    && event.data["source_scope_id"].as_str() == Some(event.scope_id.as_str())
                    && event.data["source_node_id"].as_str() == Some(node.id.as_str())
                    && event.data["error_ref"].as_str() == Some(error_ref.as_str())
                    && event.data["error_code"].as_str() == Some(declaration.error_code.as_str())
                    && plan.consume_token_ids.iter().any(|id| id == source_token)
                    && (retained
                        || plan
                            .create_tokens
                            .iter()
                            .any(|token| token.token_id == source_token
                                && token.scope_id == event.scope_id
                                && token.node_id == node.id
                                && token.status == "ready")),
                "ErrorEnd history differs from actual pinned consumed source"
            );
            validate_variables(&event.data["outputs"])?;
        }
        if let Some(node_id) = &event.node_id {
            let node = scope_node(&model, &scopes, instance_id, &event.scope_id, node_id)?;
            if event.kind == "end_reached" {
                let mut statement=tx.prepare("SELECT token_id FROM bpmn_tokens WHERE instance_id=?1 AND scope_id=?2 AND node_id=?3 AND status='ready'")?;
                let existing = statement
                    .query_map(params![instance_id, event.scope_id, node_id], |row| {
                        row.get::<_, String>(0)
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                ensure!(
                    node.kind == ProcessNodeKind::End
                        && (plan
                            .create_tokens
                            .iter()
                            .any(|token| token.scope_id == event.scope_id
                                && token.node_id == *node_id
                                && plan.consume_token_ids.contains(&token.token_id))
                            || existing
                                .iter()
                                .any(|id| plan.consume_token_ids.contains(id))),
                    "End history lacks its actual consumed local End token"
                );
            }
        }
    }
    for request in &plan.call_requests {
        ensure!(
            Uuid::parse_str(&request.call_id).is_ok()
                && Uuid::parse_str(&request.child_instance_id).is_ok(),
            "call activation identities must be UUIDs"
        );
        let node = scope_node(
            &model,
            &scopes,
            instance_id,
            &request.parent_scope_id,
            &request.call_node_id,
        )?;
        ensure!(
            matches!(node.kind, ProcessNodeKind::CallActivity { .. })
                && plan
                    .create_tokens
                    .iter()
                    .any(|token| token.token_id == request.parent_token_id
                        && token.scope_id == request.parent_scope_id
                        && token.node_id == request.call_node_id
                        && token.status == "waiting")
                && plan
                    .events
                    .iter()
                    .any(|event| event.kind == "call_requested"
                        && event.scope_id == request.parent_scope_id
                        && event.node_id.as_deref() == Some(request.call_node_id.as_str())
                        && event.data["call_id"].as_str() == Some(request.call_id.as_str())
                        && event.data["parent_token_id"].as_str()
                            == Some(request.parent_token_id.as_str())),
            "call entry lacks its exact new parent waiting activation"
        );
        validate_variables(&request.variables)?;
    }
    for incident in &plan.add_incidents {
        ensure!(
            scopes
                .iter()
                .any(|scope| scope.scope_id == incident.scope_id),
            "incident scope is outside its actual instance"
        );
        if let Some(node_id) = &incident.node_id {
            scope_node(&model, &scopes, instance_id, &incident.scope_id, node_id)?;
        }
        if let Some(job_id) = &incident.job_id {
            let job = if let Some(job) = plan.create_jobs.iter().find(|job| &job.job_id == job_id) {
                (job.scope_id.clone(), job.node_id.clone())
            } else {
                tx.query_row(
                    "SELECT scope_id,node_id FROM bpmn_jobs WHERE instance_id=?1 AND job_id=?2",
                    params![instance_id, job_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .context("incident job is outside its actual instance")?
            };
            ensure!(
                job.0 == incident.scope_id && incident.node_id.as_deref() == Some(job.1.as_str()),
                "incident job differs from its actual scope and activity"
            );
        }
    }
    Ok(scopes)
}

fn insert_scoped_tokens_on(
    tx: &Transaction<'_>,
    instance_id: &str,
    plan: &RuntimePlan,
    at_ms: i64,
) -> Result<()> {
    let mut inserted = HashSet::from([instance_id.to_owned()]);
    let mut statement = tx.prepare("SELECT scope_id FROM bpmn_scopes WHERE instance_id=?1")?;
    inserted.extend(
        statement
            .query_map([instance_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    );
    drop(statement);
    let mut tokens = HashSet::new();
    loop {
        let before = (inserted.len(), tokens.len());
        for token in &plan.create_tokens {
            if inserted.contains(&token.scope_id) && tokens.insert(token.token_id.clone()) {
                ensure!(
                    matches!(token.status.as_str(), "ready" | "waiting" | "joining"),
                    "invalid new token status"
                );
                tx.execute("INSERT INTO bpmn_tokens(token_id,instance_id,scope_id,node_id,arrival_edge_id,fork_stack_json,status,created_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![token.token_id,instance_id,token.scope_id,token.node_id,token.arrival_edge_id,json(&token.fork_stack)?,token.status,at_ms])?;
            }
        }
        for scope in &plan.create_scopes {
            if !inserted.contains(&scope.scope_id)
                && inserted.contains(&scope.parent_scope_id)
                && tokens.contains(&scope.parent_token_id)
            {
                tx.execute("INSERT INTO bpmn_scopes(scope_id,instance_id,parent_scope_id,subprocess_node_id,parent_token_id,revision,status,local_variables_json,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,1,'running',?6,?7,?7)",params![scope.scope_id,instance_id,scope.parent_scope_id,scope.subprocess_node_id,scope.parent_token_id,json(&scope.variables)?,at_ms])?;
                inserted.insert(scope.scope_id.clone());
            }
        }
        if before == (inserted.len(), tokens.len()) {
            break;
        }
    }
    ensure!(
        tokens.len() == plan.create_tokens.len()
            && plan
                .create_scopes
                .iter()
                .all(|scope| inserted.contains(&scope.scope_id)),
        "new scopes/tokens cannot be inserted in actual parent order"
    );
    Ok(())
}

#[derive(Debug)]
struct CallTransitionRejected(&'static str);
impl std::fmt::Display for CallTransitionRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for CallTransitionRejected {}

fn reaches_error_end(plan: &RuntimePlan) -> bool {
    plan.events
        .iter()
        .any(|event| event.kind == "error_end_reached")
}

fn validate_call_tree_budget_on(conn: &Connection, instance_id: &str) -> Result<()> {
    let mut scopes = 0u64;
    let mut active = 0u64;
    for id in call_tree_ids_on(conn, instance_id)? {
        let (count, bytes): (u64, u64) = conn.query_row(
            "SELECT (SELECT COUNT(*) FROM bpmn_scopes WHERE instance_id=i.instance_id),CASE WHEN i.status NOT IN ('completed','cancelled','error') THEN length(CAST(i.variables_json AS BLOB)) ELSE 0 END+COALESCE((SELECT SUM(length(CAST(local_variables_json AS BLOB))) FROM bpmn_scopes WHERE instance_id=i.instance_id AND parent_scope_id IS NOT NULL AND status NOT IN ('completed','cancelled','error')),0) FROM bpmn_instances i WHERE i.instance_id=?1",
            [&id],
            |row| Ok((row_u64(row, 0)?, row_u64(row, 1)?)),
        )?;
        scopes = scopes
            .checked_add(count)
            .context("call tree scope count overflow")?;
        active = active
            .checked_add(bytes)
            .context("call tree variable size overflow")?;
    }
    if scopes > 129 {
        return Err(
            CallTransitionRejected("process call tree exceeds 129 retained scope rows").into(),
        );
    }
    if active > 1024 * 1024 {
        return Err(
            CallTransitionRejected("process call tree active variables exceed 1 MiB").into(),
        );
    }
    Ok(())
}

fn call_incident_on(
    tx: &Transaction<'_>,
    call: &CallActivation,
    actor_id: &str,
    code: &str,
    message: &str,
    at_ms: i64,
) -> Result<()> {
    let incident_id = Uuid::new_v4().to_string();
    let full = bounded_failure_message(message);
    tx.execute("INSERT INTO bpmn_incidents(incident_id,instance_id,scope_id,node_id,job_id,code,message,at_ms,resolved_at_ms) VALUES(?1,?2,?3,?4,NULL,?5,?6,?7,NULL)",params![incident_id,call.parent_instance_id,call.parent_scope_id,call.call_node_id,code,incident_message(&full),at_ms])?;
    let seq = tx.query_row(
        "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
        [&call.parent_instance_id],
        |r| row_u64(r, 0),
    )?;
    insert_event_on(
        tx,
        &call.parent_instance_id,
        seq,
        Some(actor_id),
        &PlannedEvent {
            scope_id: call.parent_scope_id.clone(),
            kind: "incident".into(),
            node_id: Some(call.call_node_id.clone()),
            data: serde_json::json!({"incident_id":incident_id,"code":code,"message":full,"call_id":call.call_id,"parent_token_id":call.parent_token_id}),
        },
        at_ms,
        None,
    )?;
    check_active_incident_budget_on(tx, &call.parent_instance_id)?;
    tx.execute("UPDATE bpmn_instances SET status='incident',revision=revision+1,updated_at_ms=?1 WHERE instance_id=?2 AND status NOT IN ('completed','cancelled','error')",params![at_ms,call.parent_instance_id])?;
    if call.parent_scope_id != call.parent_instance_id {
        tx.execute("UPDATE bpmn_scopes SET status='incident',revision=revision+1,updated_at_ms=?1 WHERE scope_id=?2 AND instance_id=?3 AND status NOT IN ('completed','cancelled','error')",params![at_ms,call.parent_scope_id,call.parent_instance_id])?;
    }
    Ok(())
}

fn call_subtree_ids_on(conn: &Connection, root: &str) -> Result<Vec<String>> {
    let mut ids = vec![root.to_owned()];
    let mut index = 0;
    while index < ids.len() {
        let parent_id = ids[index].clone();
        for call in calls_on(conn, &parent_id)?
            .into_iter()
            .filter(|c| c.parent_instance_id == parent_id)
        {
            ensure!(
                !ids.contains(&call.child_instance_id),
                "cyclic call subtree"
            );
            ids.push(call.child_instance_id);
            ensure!(ids.len() <= 129, "call subtree exceeds lifetime bound");
        }
        index += 1;
    }
    Ok(ids)
}

fn retract_call_outbox_on(
    tx: &Transaction<'_>,
    instance_id: &str,
    reason: &str,
    at_ms: i64,
) -> Result<()> {
    let mut q=tx.prepare("SELECT org_id,sender_user_id,message_id FROM bpmn_messages WHERE source_instance_id=?1 AND status IN ('pending','blocked','ambiguous') ORDER BY sender_user_id,message_id")?;
    let keys = q
        .query_map([instance_id], |r| {
            Ok(MessageKey {
                org_id: r.get(0)?,
                sender_user_id: r.get(1)?,
                message_id: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(q);
    for key in keys {
        let message = message_on(tx, &key, false)?;
        ensure!(
            update_message_state_on(
                tx,
                &message,
                ProcessMessageStatus::Cancelled,
                Some(reason),
                at_ms,
                at_ms
            )?,
            "call outgoing receipt changed during closure"
        );
    }
    Ok(())
}

fn cancel_call_children_on(
    tx: &Transaction<'_>,
    instance_id: &str,
    token_ids: Option<&[String]>,
    scope_roots: &[String],
    actor_id: &str,
    reason: &str,
    at_ms: i64,
) -> Result<Vec<CancelledJobClaim>> {
    let (definition, version): (String, u32) = tx.query_row(
        "SELECT definition_id,version FROM bpmn_instances WHERE instance_id=?1",
        [instance_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let model = current_version_model_on(tx, &definition, version)?;
    let scopes = scopes_on(tx, instance_id, &model)?;
    let mut closure = HashSet::new();
    for root in scope_roots {
        closure.extend(descendant_scope_ids(&scopes, root)?);
    }
    let calls = calls_on(tx, instance_id)?
        .into_iter()
        .filter(|c| {
            c.parent_instance_id == instance_id
                && (token_ids.is_none()
                    || token_ids.is_some_and(|ids| ids.contains(&c.parent_token_id))
                    || closure.contains(&c.parent_scope_id))
        })
        .collect::<Vec<_>>();
    let mut claims = Vec::new();
    for call in calls {
        let was_active = matches!(
            call.status,
            ProcessCallStatus::Waiting | ProcessCallStatus::ReturnIncident
        );
        if matches!(
            call.status,
            ProcessCallStatus::Waiting | ProcessCallStatus::ReturnIncident
        ) {
            tx.execute("UPDATE bpmn_calls SET status='cancelled',revision=revision+1,updated_at_ms=?1 WHERE call_id=?2 AND revision=?3",params![at_ms,call.call_id,sql_incrementable(call.revision)?])?;
        }
        let child_id = &call.child_instance_id;
        let (revision, status): (u64, String) = tx.query_row(
            "SELECT revision,status FROM bpmn_instances WHERE instance_id=?1",
            [child_id],
            |r| Ok((row_u64(r, 0)?, r.get(1)?)),
        )?;
        retract_call_outbox_on(tx, child_id, reason, at_ms)?;
        if !matches!(status.as_str(), "completed" | "cancelled" | "error") {
            let actor = call_initiator_on(tx, child_id)?;
            claims.extend(cancel_instance_on(tx, &actor, child_id, revision, at_ms)?);
        } else {
            claims.extend(cancel_call_children_on(
                tx,
                child_id,
                None,
                &[],
                actor_id,
                reason,
                at_ms,
            )?);
        }
        if was_active {
            let seq = tx.query_row(
                "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
                [instance_id],
                |r| row_u64(r, 0),
            )?;
            insert_event_on(
                tx,
                instance_id,
                seq,
                Some(actor_id),
                &PlannedEvent {
                    scope_id: call.parent_scope_id.clone(),
                    kind: "call_cancelled".into(),
                    node_id: Some(call.call_node_id.clone()),
                    data: serde_json::json!({"call_id":call.call_id,"child_instance_id":call.child_instance_id,"reason":reason}),
                },
                at_ms,
                None,
            )?;
        }
    }
    Ok(claims)
}

fn validate_business_source_on(
    conn: &Connection,
    source: &BusinessErrorSource,
    target: &CallActivation,
) -> Result<()> {
    let source_id = match source {
        BusinessErrorSource::ErrorEnd { instance_id, .. }
        | BusinessErrorSource::ServiceContract { instance_id, .. } => instance_id,
    };
    ensure!(
        call_subtree_ids_on(conn, &target.child_instance_id)?.contains(source_id),
        "business error source is outside the exact called subtree"
    );
    match source {
        BusinessErrorSource::ErrorEnd {
            instance_id,
            fact,
            outputs,
            ..
        } => {
            validate_terminal_error_on(conn, instance_id, fact)?;
            let actual:Option<String>=conn.query_row("SELECT terminal_error_json FROM bpmn_instances WHERE instance_id=?1 AND status='error'",[instance_id],|r|r.get(0)).optional()?.flatten();
            ensure!(
                actual.as_deref() == Some(json(fact)?.as_str()),
                "outward ErrorEnd has no matching terminal source outcome"
            );
            validate_variables(outputs)?;
            let event: Value = parse(conn.query_row(
                "SELECT data_json FROM bpmn_events WHERE event_id=?1",
                [&fact.source_event_id],
                |r| r.get::<_, String>(0),
            )?)?;
            ensure!(
                event["outputs"] == *outputs,
                "ErrorEnd outward outputs differ from factual source variables"
            );
        }
        BusinessErrorSource::ServiceContract {
            instance_id,
            scope_id,
            node_id,
            token_id,
            job_id,
            attempt,
            fence,
            result_event_id,
            result,
        } => {
            ensure!(
                result.outcome == tentaflow_protocol::processes::ActivityOutcome::Error,
                "only a true business Error may propagate"
            );
            let matching:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM bpmn_jobs WHERE job_id=?1 AND instance_id=?2 AND scope_id=?3 AND node_id=?4 AND token_id=?5 AND attempt=?6 AND fence=?7 AND result_origin='contract' AND result_json=?8)",params![job_id,instance_id,scope_id,node_id,token_id,attempt,sql_integer(*fence)?,json(result)?],|r|r.get(0))?;
            ensure!(
                matching,
                "outward error is not the actual accepted fenced Contract result"
            );
            let data:Value=parse(conn.query_row("SELECT data_json FROM bpmn_events WHERE event_id=?1 AND instance_id=?2 AND scope_id=?3 AND node_id=?4 AND kind='service_result'",params![result_event_id,instance_id,scope_id,node_id],|r|r.get::<_,String>(0))?)?;
            let persisted: ActivityResult = serde_json::from_value(data.clone())?;
            ensure!(
                persisted == *result && data["result_origin"].as_str() == Some("contract"),
                "outward service result event differs from original Contract facts"
            );
        }
    }
    Ok(())
}

fn apply_call_steps_on(
    tx: &Transaction<'_>,
    source_instance_id: &str,
    actor_id: &str,
    plan: &RuntimePlan,
    at_ms: i64,
) -> Result<Vec<CancelledJobClaim>> {
    let mut claims = Vec::new();
    let mut skipped = HashSet::new();
    for (index, step) in plan.call_steps.iter().enumerate() {
        let step_instance = match step {
            CallStep::Start { call, .. } => &call.call.child_instance_id,
            CallStep::Return { call, .. } => &call.parent_instance_id,
            CallStep::Advance { instance_id, .. } => instance_id,
        };
        let dependency = match step {
            CallStep::Start { call, .. } => &call.call.parent_instance_id,
            CallStep::Return { call, .. } => &call.child_instance_id,
            CallStep::Advance { instance_id, .. } => instance_id,
        };
        if skipped.contains(step_instance) || skipped.contains(dependency) {
            if let CallStep::Start { call, .. } = step {
                skipped.insert(call.call.child_instance_id.clone());
            }
            continue;
        }
        let savepoint = format!("process_call_{index}");
        tx.execute_batch(&format!("SAVEPOINT {savepoint}"))?;
        let applied = (|| -> Result<Vec<CancelledJobClaim>> {
            match step {
                CallStep::Start { call, plan } => {
                    let actual = &call.call;
                    let actor = call_initiator_on(tx, &actual.parent_instance_id)?;
                    require_call_control_authority_on(tx, &actor, &actual.parent_instance_id)?;
                    let identity: (String, u32) = tx.query_row(
                        "SELECT definition_id,version FROM bpmn_instances WHERE instance_id=?1",
                        [&actual.parent_instance_id],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )?;
                    ensure!(
                        identity == (actual.definition_id.clone(), actual.version),
                        "call source version differs from actual parent instance"
                    );
                    let parent_model =
                        current_version_model_on(tx, &actual.definition_id, actual.version)?;
                    let node = scoped_node_on(
                        tx,
                        &actual.parent_instance_id,
                        &actual.parent_scope_id,
                        &parent_model,
                        &actual.call_node_id,
                    )?;
                    let pin = call_pins_on(tx, &actual.definition_id, actual.version)?
                        .into_iter()
                        .find(|p| p.node_id == actual.call_node_id)
                        .context("called target pin missing")?;
                    ensure!(
                        matches!(&node.kind,ProcessNodeKind::CallActivity {called_definition_id,called_version,called_element,..} if called_definition_id==&actual.called_definition_id && *called_version==actual.called_version && called_element==&pin.called_element)
                            && pin.model_sha256 == actual.model_sha256,
                        "new call differs from actual immutable parent pin"
                    );
                    let target =
                        version_on(tx, &actual.called_definition_id, actual.called_version)?;
                    require_call_version_on(tx, &actor, &target, true)?;
                    ensure!(
                        target.model_sha256 == actual.model_sha256,
                        "call target model hash changed"
                    );
                    let waiting:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM bpmn_tokens WHERE instance_id=?1 AND scope_id=?2 AND token_id=?3 AND node_id=?4 AND status='waiting')",params![actual.parent_instance_id,actual.parent_scope_id,actual.parent_token_id,actual.call_node_id],|r|r.get(0))?;
                    ensure!(waiting, "call parent NEW waiting control is missing");
                    tx.execute("INSERT INTO bpmn_calls(call_id,parent_instance_id,parent_scope_id,parent_token_id,call_node_id,child_instance_id,definition_id,version,called_definition_id,called_version,model_sha256,revision,status,created_at_ms,updated_at_ms) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,1,'waiting',?12,?12)",params![actual.call_id,actual.parent_instance_id,actual.parent_scope_id,actual.parent_token_id,actual.call_node_id,actual.child_instance_id,actual.definition_id,actual.version,actual.called_definition_id,actual.called_version,actual.model_sha256,at_ms])?;
                    start_instance_on(
                        tx,
                        &actor,
                        &actual.child_instance_id,
                        &actual.called_definition_id,
                        actual.called_version,
                        &call.variables,
                        plan,
                        at_ms,
                        None,
                        None,
                    )?;
                    validate_call_tree_budget_on(tx, &actual.parent_instance_id)?;
                    let seq = tx.query_row(
                        "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
                        [&actual.parent_instance_id],
                        |r| row_u64(r, 0),
                    )?;
                    insert_event_on(
                        tx,
                        &actual.parent_instance_id,
                        seq,
                        Some(actor_id),
                        &PlannedEvent {
                            scope_id: actual.parent_scope_id.clone(),
                            kind: "call_entered".into(),
                            node_id: Some(actual.call_node_id.clone()),
                            data: serde_json::json!({"call_id":actual.call_id,"child_instance_id":actual.child_instance_id,"called_definition_id":actual.called_definition_id,"called_version":actual.called_version,"parent_token_id":actual.parent_token_id}),
                        },
                        at_ms,
                        None,
                    )?;
                    Ok(Vec::new())
                }
                CallStep::Return {
                    call,
                    expected_revision,
                    status,
                    source,
                    plan,
                } => {
                    let actual = calls_on(tx, &call.parent_instance_id)?
                        .into_iter()
                        .find(|c| c.call_id == call.call_id)
                        .context("call return identity missing")?;
                    ensure!(
                        actual.revision == call.revision
                            && actual.status == ProcessCallStatus::Waiting
                            && actual.child_instance_id == call.child_instance_id,
                        "call return revision conflict"
                    );
                    if let Some(source) = source {
                        validate_business_source_on(tx, source, &actual)?;
                        let (source_id, source_scope, source_event, source_kind, code) =
                            match source {
                                BusinessErrorSource::ErrorEnd {
                                    instance_id, fact, ..
                                } => (
                                    instance_id.as_str(),
                                    fact.source_scope_id.as_str(),
                                    fact.source_event_id.as_str(),
                                    "error_end",
                                    Some(fact.error_code.as_str()),
                                ),
                                BusinessErrorSource::ServiceContract {
                                    instance_id,
                                    scope_id,
                                    result_event_id,
                                    result,
                                    ..
                                } => (
                                    instance_id.as_str(),
                                    scope_id.as_str(),
                                    result_event_id.as_str(),
                                    "contract",
                                    result.code.as_deref(),
                                ),
                            };
                        let mut hop = source_id.to_owned();
                        while hop != actual.child_instance_id {
                            let incoming = calls_on(tx, &hop)?
                                .into_iter()
                                .find(|call| call.child_instance_id == hop)
                                .context("business source call ancestry missing")?;
                            if source_kind == "error_end" && hop == source_id {
                                tx.execute("UPDATE bpmn_calls SET status='error',revision=revision+1,updated_at_ms=?1 WHERE call_id=?2 AND revision=?3 AND status='waiting'",params![at_ms,incoming.call_id,sql_incrementable(incoming.revision)?])?;
                            }
                            let seq=tx.query_row("SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",[&incoming.parent_instance_id],|row|row_u64(row,0))?;
                            insert_event_on(
                                tx,
                                &incoming.parent_instance_id,
                                seq,
                                Some(actor_id),
                                &PlannedEvent {
                                    scope_id: incoming.parent_scope_id.clone(),
                                    kind: "call_error_propagated".into(),
                                    node_id: Some(incoming.call_node_id.clone()),
                                    data: serde_json::json!({"call_id":incoming.call_id,"source_instance_id":source_id,"source_scope_id":source_scope,"source_event_id":source_event,"source_kind":source_kind,"attached_token_id":incoming.parent_token_id,"error_code":code}),
                                },
                                at_ms,
                                None,
                            )?;
                            hop = incoming.parent_instance_id;
                        }
                    } else {
                        let complete:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM bpmn_instances WHERE instance_id=?1 AND status='completed' AND definition_id=?2 AND version=?3)",params![actual.child_instance_id,actual.called_definition_id,actual.called_version],|r|r.get(0))?;
                        ensure!(complete, "call return child is not factually Completed");
                    }
                    let parent_revision: u64 = tx.query_row(
                        "SELECT revision FROM bpmn_instances WHERE instance_id=?1",
                        [&actual.parent_instance_id],
                        |row| row_u64(row, 0),
                    )?;
                    ensure!(
                        parent_revision == *expected_revision,
                        "call return parent revision changed within the prepared transaction"
                    );
                    let actor = call_initiator_on(tx, &actual.parent_instance_id)?;
                    if *status != ProcessCallStatus::ReturnIncident
                        && !plan.events.iter().any(|e| {
                            e.kind == "incident"
                                && e.data["code"].as_str() == Some("CALL_CHILD_ERROR")
                        })
                    {
                        require_call_control_authority_on(tx, &actor, &actual.parent_instance_id)?;
                    }
                    tx.execute("UPDATE bpmn_calls SET status=?1,revision=revision+1,updated_at_ms=?2 WHERE call_id=?3 AND revision=?4",params![call_status_text(status),at_ms,actual.call_id,sql_incrementable(actual.revision)?])?;
                    let claims = apply_plan_on(
                        tx,
                        &actual.parent_instance_id,
                        actor_id,
                        *expected_revision,
                        plan,
                        at_ms,
                    )?;
                    validate_call_tree_budget_on(tx, &actual.parent_instance_id)?;
                    Ok(claims)
                }
                CallStep::Advance {
                    instance_id,
                    expected_revision,
                    plan,
                } => apply_plan_on(tx, instance_id, actor_id, *expected_revision, plan, at_ms),
            }
        })();
        match applied {
            Ok(values) => {
                tx.execute_batch(&format!("RELEASE {savepoint}"))?;
                claims.extend(values);
            }
            Err(error)
                if error.downcast_ref::<ProcessAuthorityDenied>().is_some()
                    || error.downcast_ref::<CallTransitionRejected>().is_some() =>
            {
                tx.execute_batch(&format!("ROLLBACK TO {savepoint}; RELEASE {savepoint}"))?;
                let (call, code) = match step {
                    CallStep::Start { call, .. } => (&call.call, "CALL_ADMISSION_ERROR"),
                    CallStep::Return { call, .. } => (call, "CALL_RETURN_ERROR"),
                    CallStep::Advance { .. } => return Err(error),
                };
                call_incident_on(tx, call, actor_id, code, &format!("{error:#}"), at_ms)?;
                if matches!(step, CallStep::Return { .. }) {
                    tx.execute("UPDATE bpmn_calls SET status='return_incident',revision=revision+1,updated_at_ms=?1 WHERE call_id=?2 AND revision=?3",params![at_ms,call.call_id,sql_incrementable(call.revision)?])?;
                }
                skipped.insert(step_instance.to_owned());
                if matches!(step, CallStep::Start { .. }) {
                    skipped.insert(call.child_instance_id.clone());
                }
            }
            Err(error) => {
                tx.execute_batch(&format!("ROLLBACK TO {savepoint}; RELEASE {savepoint}"))?;
                return Err(error);
            }
        }
    }
    validate_call_tree_budget_on(tx, source_instance_id)?;
    Ok(claims)
}

fn validate_gateway_join_plan_on(
    tx: &Transaction<'_>,
    instance_id: &str,
    model: &ProcessModel,
    scopes: &[ProcessScopeSummary],
    plan: &RuntimePlan,
) -> Result<()> {
    let mut affected: HashMap<(String, String, String), (ForkFrame, usize)> = HashMap::new();
    let mut stmt = tx.prepare("SELECT token_id,scope_id,fork_stack_json FROM bpmn_tokens WHERE instance_id=?1 AND status IN ('ready','waiting','joining')")?;
    let prior = stmt.query_map([instance_id], |row| Ok((row.get::<_, String>(0)?,
        row.get::<_, String>(1)?, row.get::<_, String>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    for (token_id, scope_id, stack_json) in prior {
        for frame in parse::<Vec<ForkFrame>>(stack_json)? {
            let key = (scope_id.clone(), frame.join_node_id.clone(), frame.activation_id.clone());
            let survives = !plan.consume_token_ids.contains(&token_id)
                && !plan.cancel_token_ids.contains(&token_id);
            match affected.get_mut(&key) {
                Some((original, count)) => {
                    ensure!(original.split_node_id == frame.split_node_id
                        && original.gateway_kind == frame.gateway_kind
                        && original.selected_branch_edge_ids == frame.selected_branch_edge_ids,
                        "gateway activation has conflicting original frames");
                    *count += if survives { 1 } else { 0 };
                }
                None => {
                    affected.insert(key, (frame, if survives { 1 } else { 0 }));
                }
            }
        }
    }
    let prior_keys = affected.keys().cloned().collect::<HashSet<_>>();
    for token in &plan.create_tokens {
        for frame in &token.fork_stack {
            let key = (token.scope_id.clone(), frame.join_node_id.clone(), frame.activation_id.clone());
            if !prior_keys.contains(&key) {
                let split_kind = if frame.gateway_kind == GatewayKind::Inclusive {
                    "inclusive_split"
                } else {
                    "parallel_split"
                };
                ensure!(plan.events.iter().any(|event| event.kind == split_kind
                    && event.scope_id == token.scope_id
                    && event.node_id.as_deref() == Some(frame.split_node_id.as_str())
                    && event.data["activation_id"].as_str() == Some(frame.activation_id.as_str())),
                    "new gateway activation has no factual split");
            }
            let survives = matches!(token.status.as_str(), "ready" | "waiting" | "joining")
                && !plan.consume_token_ids.contains(&token.token_id)
                && !plan.cancel_token_ids.contains(&token.token_id);
            match affected.get_mut(&key) {
                Some((original, count)) => {
                    ensure!(original.split_node_id == frame.split_node_id
                        && original.gateway_kind == frame.gateway_kind
                        && original.selected_branch_edge_ids == frame.selected_branch_edge_ids,
                        "planned gateway frame changed its original activation");
                    *count += if survives { 1 } else { 0 };
                }
                None => {
                    affected.insert(key, (frame.clone(), if survives { 1 } else { 0 }));
                }
            }
        }
    }
    let mut joined = HashSet::new();
    for event in plan.events.iter().filter(|event| {
        matches!(event.kind.as_str(), "parallel_joined" | "inclusive_joined")
    }) {
        let join_id = event.node_id.as_deref().context("gateway join event has no node")?;
        let activation_id = event.data["activation_id"].as_str()
            .context("gateway join event has no activation")?;
        let key = (event.scope_id.clone(), join_id.to_owned(), activation_id.to_owned());
        ensure!(joined.insert(key.clone()), "gateway activation joined more than once");
        let path = scope_path(scopes, instance_id, &event.scope_id)?;
        let (nodes, flows, _) = super::model::scope_body(model, &path)?;
        let pair = super::model::gateway_pairs(nodes, flows)?
            .into_values().find(|pair| pair.join_node_id == join_id)
            .context("gateway join event has no paired model")?;
        let kind = if event.kind == "inclusive_joined" {
            GatewayKind::Inclusive
        } else {
            GatewayKind::Parallel
        };
        ensure!(pair.kind == kind, "gateway join event kind differs from pinned pair");
        let mut selected_from_state = None;
        let mut stmt = tx.prepare("SELECT fork_stack_json FROM bpmn_tokens WHERE instance_id=?1 AND scope_id=?2 AND status IN ('ready','waiting','joining')")?;
        let stacks = stmt.query_map(params![instance_id,event.scope_id], |row| row.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for stack in stacks {
            for frame in parse::<Vec<ForkFrame>>(stack)? {
                if frame.join_node_id == join_id && frame.activation_id == activation_id {
                    ensure!(frame.split_node_id == pair.split_node_id && frame.gateway_kind == kind,
                        "gateway join activation differs from its pinned pair");
                    if let Some(previous) = &selected_from_state {
                        ensure!(previous == &frame.selected_branch_edge_ids,
                            "gateway join activation has conflicting original selections");
                    } else {
                        selected_from_state = Some(frame.selected_branch_edge_ids);
                    }
                }
            }
        }
        let selected = if let Some(selected) = selected_from_state {
            selected
        } else {
            let split_kind = if kind == GatewayKind::Inclusive {
                "inclusive_split"
            } else {
                "parallel_split"
            };
            let split = plan.events.iter().find(|candidate| candidate.kind == split_kind
                && candidate.scope_id == event.scope_id
                && candidate.node_id.as_deref() == Some(pair.split_node_id.as_str())
                && candidate.data["activation_id"].as_str() == Some(activation_id))
                .context("gateway join has no original activation or same-plan split")?;
            if kind == GatewayKind::Inclusive {
                serde_json::from_value::<Vec<String>>(split.data["selected_branch_edge_ids"].clone())?
            } else {
                pair.branch_to_incoming_edge.keys().cloned().collect()
            }
        };
        ensure!(!selected.is_empty()
            && selected.windows(2).all(|window| window[0] < window[1])
            && selected.iter().all(|edge| pair.branch_to_incoming_edge.contains_key(edge))
            && (kind == GatewayKind::Inclusive
                || selected.iter().map(String::as_str)
                    .eq(pair.branch_to_incoming_edge.keys().map(String::as_str))),
            "gateway join selection differs from pinned pair");
        if kind == GatewayKind::Inclusive {
            ensure!(event.data["selected_branch_edge_ids"] == serde_json::to_value(&selected)?,
                "inclusive join event differs from original selected branches");
        }
        let mut arrivals: HashMap<String, (String, Vec<ForkFrame>, Option<String>)> = HashMap::new();
        let mut persisted = HashSet::new();
        let mut stmt = tx.prepare("SELECT r.branch_edge_id,r.token_id,r.gateway_kind,t.node_id,t.arrival_edge_id,t.status,t.fork_stack_json FROM bpmn_gateway_receipts r JOIN bpmn_tokens t ON t.instance_id=r.instance_id AND t.scope_id=r.scope_id AND t.token_id=r.token_id WHERE r.instance_id=?1 AND r.scope_id=?2 AND r.join_node_id=?3 AND r.activation_id=?4")?;
        let rows = stmt.query_map(params![instance_id,event.scope_id,join_id,activation_id], |row| {
            Ok((row.get::<_, String>(0)?,row.get::<_, String>(1)?,row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,row.get::<_, Option<String>>(4)?,row.get::<_, String>(5)?,
                row.get::<_, String>(6)?))
        })?.collect::<rusqlite::Result<Vec<_>>>()?;
        for (branch, token_id, receipt_kind, node_id, arrival, status, stack_json) in rows {
            ensure!(node_id == join_id && status == "joining"
                && receipt_kind == (if kind == GatewayKind::Inclusive { "inclusive" } else { "parallel" }),
                "gateway join prior receipt differs from its joining token");
            let stack: Vec<ForkFrame> = parse(stack_json)?;
            ensure!(arrivals.insert(branch.clone(), (token_id.clone(), stack, arrival)).is_none(),
                "gateway join has duplicate prior branch receipts");
            persisted.insert((branch.clone(), token_id.clone()));
            ensure!(plan.remove_gateway_receipts.iter().filter(|receipt| receipt.scope_id == event.scope_id
                && receipt.join_node_id == join_id && receipt.activation_id == activation_id
                && receipt.branch_edge_id == branch && receipt.token_id == token_id
                && receipt.gateway_kind == kind).count() == 1,
                "gateway join did not remove its exact prior receipt");
        }
        for token in plan.create_tokens.iter().filter(|token| token.scope_id == event.scope_id
            && token.node_id == join_id && token.status == "joining"
            && token.fork_stack.last().is_some_and(|frame| frame.activation_id == activation_id)) {
            let frame = token.fork_stack.last().context("new join arrival has no frame")?;
            ensure!(arrivals.insert(frame.branch_edge_id.clone(),
                (token.token_id.clone(), token.fork_stack.clone(), token.arrival_edge_id.clone())).is_none(),
                "gateway join has duplicate newly arrived branch");
            ensure!(!plan.add_gateway_receipts.iter().any(|receipt| receipt.token_id == token.token_id),
                "completed gateway join retained a new receipt");
        }
        ensure!(arrivals.len() == selected.len()
            && selected.iter().all(|branch| arrivals.contains_key(branch)),
            "gateway join did not collect exactly its selected branches");
        let mut outer_stack = None;
        let mut consumed = HashSet::new();
        for (branch, (token_id, stack, arrival)) in arrivals {
            let frame = stack.last().context("gateway arrival has no top frame")?;
            ensure!(frame.gateway_kind == kind && frame.split_node_id == pair.split_node_id
                && frame.join_node_id == join_id && frame.activation_id == activation_id
                && frame.branch_edge_id == branch && frame.selected_branch_edge_ids == selected
                && pair.branch_to_incoming_edge.get(&branch) == arrival.as_ref()
                && consumed.insert(token_id.clone())
                && plan.consume_token_ids.iter().filter(|id| *id == &token_id).count() == 1,
                "gateway join consumed a mismatched or unselected arrival");
            let parent = stack[..stack.len() - 1].to_vec();
            if let Some(previous) = &outer_stack {
                ensure!(previous == &parent, "gateway join branch frames disagree below the activation");
            } else {
                outer_stack = Some(parent);
            }
        }
        ensure!(plan.remove_gateway_receipts.iter().filter(|receipt| receipt.scope_id == event.scope_id
            && receipt.join_node_id == join_id && receipt.activation_id == activation_id).count()
            == persisted.len(), "gateway join removed a receipt outside its prior arrivals");
        let outgoing = flows.iter().filter(|flow| flow.source_id == join_id).collect::<Vec<_>>();
        ensure!(outgoing.len() == 1, "gateway join has no unique continuation edge");
        let edge = outgoing[0];
        let outer_stack = outer_stack.context("gateway join has no selected arrival frame")?;
        ensure!(plan.create_tokens.iter().filter(|token| token.scope_id == event.scope_id
            && token.status == "ready" && token.node_id == edge.target_id
            && token.arrival_edge_id.as_deref() == Some(edge.id.as_str())
            && token.fork_stack == outer_stack)
            .count() == 1, "gateway join lacks exactly one popped-frame continuation");
    }
    if plan.cancel_scope_roots.is_empty() && !reaches_error_end(plan) {
        ensure!(plan.remove_gateway_receipts.iter().all(|receipt| joined.contains(&(
            receipt.scope_id.clone(), receipt.join_node_id.clone(), receipt.activation_id.clone()))),
            "ordinary transition removed a gateway receipt without a join");
    }
    let mut cancelled = HashSet::new();
    for root in &plan.cancel_scope_roots {
        cancelled.extend(descendant_scope_ids(scopes, root)?);
    }
    for ((scope_id, join_id, activation_id), (original, survivors)) in affected {
        if joined.contains(&(scope_id.clone(), join_id.clone(), activation_id.clone())) {
            ensure!(survivors == 0, "joined gateway activation still has an active frame");
            continue;
        }
        if survivors != 0 {
            continue;
        }
        let factual_error = (plan.terminal_error.as_ref().is_some_and(|fact| fact.source_scope_id == scope_id)
            || plan.scope_terminal_errors.contains_key(&scope_id))
            && plan.events.iter().any(|event| event.kind == "error_end_reached"
            && event.scope_id == scope_id
            && event.data["source_token_id"].as_str().is_some_and(|source| {
                plan.create_tokens.iter().find(|token| token.scope_id == scope_id
                    && token.token_id == source).map(|token| token.fork_stack.clone())
                    .or_else(|| tx.query_row("SELECT fork_stack_json FROM bpmn_tokens WHERE instance_id=?1 AND scope_id=?2 AND token_id=?3",
                        params![instance_id,scope_id.as_str(),source], |row| row.get::<_, String>(0))
                        .ok().and_then(|json| parse::<Vec<ForkFrame>>(json).ok()))
                    .is_some_and(|frames| frames.iter().any(|frame|
                        frame.join_node_id == join_id && frame.activation_id == activation_id
                        && frame.split_node_id == original.split_node_id))
            }));
        ensure!(cancelled.contains(&scope_id) || factual_error,
            "gateway activation disappeared without its factual join");
    }
    Ok(())
}

fn validate_gateway_state_on(
    tx: &Transaction<'_>, instance_id: &str, model: &ProcessModel,
    scopes: &[ProcessScopeSummary],
) -> Result<()> {
    let mut groups: HashMap<(String, String, String), (GatewayKind, Vec<String>)> = HashMap::new();
    let mut tokens: HashMap<String, (String, String, Option<String>, String, Vec<ForkFrame>)> = HashMap::new();
    let mut stmt = tx.prepare("SELECT token_id,scope_id,node_id,arrival_edge_id,status,fork_stack_json FROM bpmn_tokens WHERE instance_id=?1 AND status IN ('ready','waiting','joining')")?;
    let rows = stmt.query_map([instance_id], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, Option<String>>(3)?, row.get::<_, String>(4)?, row.get::<_, String>(5)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (token_id, scope_id, node_id, arrival_edge_id, status, stack_json) in rows {
        let frames: Vec<ForkFrame> = parse(stack_json)?;
        let path = scope_path(scopes, instance_id, &scope_id)?;
        let (nodes, flows, _) = super::model::scope_body(model, &path)?;
        let pairs = super::model::gateway_pairs(nodes, flows)?;
        for frame in &frames {
            let pair = pairs.get(&frame.split_node_id).context("active fork pair missing from pinned scope")?;
            let selected = &frame.selected_branch_edge_ids;
            ensure!(pair.join_node_id == frame.join_node_id && pair.kind == frame.gateway_kind
                && !selected.is_empty() && selected.windows(2).all(|window| window[0] < window[1])
                && selected.binary_search(&frame.branch_edge_id).is_ok()
                && selected.iter().all(|edge| pair.branch_to_incoming_edge.contains_key(edge))
                && (frame.gateway_kind == GatewayKind::Inclusive
                    || selected.iter().map(String::as_str).eq(pair.branch_to_incoming_edge.keys().map(String::as_str))),
                "active fork frame differs from pinned gateway pair");
            let key = (scope_id.clone(), frame.join_node_id.clone(), frame.activation_id.clone());
            if let Some((kind, branches)) = groups.get(&key) {
                ensure!(*kind == frame.gateway_kind && branches == selected,
                    "gateway activation has conflicting selected branches");
            } else {
                groups.insert(key, (frame.gateway_kind, selected.clone()));
            }
        }
        tokens.insert(token_id, (scope_id, node_id, arrival_edge_id, status, frames));
    }
    let mut stmt = tx.prepare("SELECT scope_id,join_node_id,activation_id,branch_edge_id,token_id,gateway_kind FROM bpmn_gateway_receipts WHERE instance_id=?1")?;
    let rows = stmt.query_map([instance_id], |row| Ok((row.get::<_, String>(0)?,row.get::<_, String>(1)?,row.get::<_, String>(2)?,row.get::<_, String>(3)?,row.get::<_, String>(4)?,row.get::<_, String>(5)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut receipt_tokens = HashSet::new();
    for (scope_id, join_id, activation_id, branch_id, token_id, kind_text) in rows {
        ensure!(receipt_tokens.insert(token_id.clone()), "joining token has multiple gateway receipts");
        let (token_scope, token_node, arrival, status, frames) = tokens.get(&token_id)
            .context("gateway receipt refers to an inactive token")?;
        let frame = frames.last().context("gateway receipt token lacks top fork frame")?;
        let kind = match kind_text.as_str() { "parallel" => GatewayKind::Parallel,
            "inclusive" => GatewayKind::Inclusive, _ => bail!("invalid gateway receipt kind") };
        let key = (scope_id.clone(), join_id.clone(), activation_id.clone());
        let (group_kind, selected) = groups.get(&key).context("gateway receipt has no active activation")?;
        ensure!(token_scope == &scope_id && token_node == &join_id && status == "joining"
            && frame.join_node_id == join_id && frame.activation_id == activation_id
            && frame.branch_edge_id == branch_id && frame.gateway_kind == kind
            && group_kind == &kind && &frame.selected_branch_edge_ids == selected
            && selected.binary_search(&branch_id).is_ok(),
            "gateway receipt differs from its active selected joining token");
        let path = scope_path(scopes, instance_id, &scope_id)?;
        let (nodes, flows, _) = super::model::scope_body(model, &path)?;
        let pairs = super::model::gateway_pairs(nodes, flows)?;
        let pair = pairs.get(&frame.split_node_id).context("gateway receipt pair missing")?;
        ensure!(pair.kind == kind && pair.join_node_id == join_id
            && pair.branch_to_incoming_edge.get(&branch_id) == arrival.as_ref(),
            "gateway receipt arrived through another paired edge");
    }
    ensure!(tokens.iter().filter(|(_, (_, _, _, status, _))| status.as_str() == "joining")
        .all(|(token_id, _)| receipt_tokens.contains(token_id)),
        "joining token lacks a gateway receipt");
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
    let (current_revision, current_status): (u64, String) = tx.query_row(
        "SELECT revision,status FROM bpmn_instances WHERE instance_id=?1",
        [instance_id],
        |row| Ok((row_u64(row, 0)?, row.get(1)?)),
    )?;
    ensure!(
        current_revision == expected_revision
            && !matches!(current_status.as_str(), "completed" | "cancelled" | "error"),
        "process instance revision conflict or closed instance"
    );
    sql_incrementable(expected_revision)?;
    let proposed_scopes = validate_scope_plan_on(tx, instance_id, plan, at_ms)?;
    let (definition_id,version,org_id,initiator):(String,u32,String,String)=tx.query_row("SELECT definition_id,version,org_id,initiator_user_id FROM bpmn_instances WHERE instance_id=?1",[instance_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?;
    let model = current_version_model_on(tx, &definition_id, version)?;
    validate_gateway_state_on(tx, instance_id, &model, &proposed_scopes)?;
    validate_gateway_join_plan_on(tx, instance_id, &model, &proposed_scopes, plan)?;

    let mut cancelled_claims = Vec::new();
    insert_scoped_tokens_on(tx, instance_id, plan, at_ms)?;
    apply_event_plan_on(tx, instance_id, plan, at_ms)?;
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
                    || (matches!(reason,"sibling_interrupted"|"scope_cancelled") && plan.cancel_token_ids.iter().any(|id|id == token_id)),
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
    for receipt in &plan.remove_gateway_receipts {
        let affected=tx.execute("DELETE FROM bpmn_gateway_receipts WHERE instance_id=?1 AND join_node_id=?2 AND activation_id=?3 AND branch_edge_id=?4 AND token_id=?5 AND scope_id=?6 AND gateway_kind=?7",params![instance_id,receipt.join_node_id,receipt.activation_id,receipt.branch_edge_id,receipt.token_id,receipt.scope_id,match receipt.gateway_kind { GatewayKind::Parallel => "parallel", GatewayKind::Inclusive => "inclusive" }])?;
        ensure!(
            affected == 1,
            "gateway join receipt changed before transition"
        );
    }
    for receipt in &plan.add_gateway_receipts {
        let (node,stack,status,scope):(String,String,String,String)=tx.query_row("SELECT node_id,fork_stack_json,status,scope_id FROM bpmn_tokens WHERE instance_id=?1 AND token_id=?2",params![instance_id,receipt.token_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)))?;
        let stack: Vec<ForkFrame> = parse(stack)?;
        let frame = stack.last().context("joining receipt lacks a fork frame")?;
        ensure!(
            status == "joining"
                && scope == receipt.scope_id
                && node == receipt.join_node_id
                && frame.join_node_id == receipt.join_node_id
                && frame.activation_id == receipt.activation_id
                && frame.branch_edge_id == receipt.branch_edge_id
                && frame.gateway_kind == receipt.gateway_kind,
            "join receipt differs from its actual scoped joining token"
        );
        tx.execute("INSERT INTO bpmn_gateway_receipts(instance_id,join_node_id,activation_id,branch_edge_id,token_id,created_at_ms,scope_id,gateway_kind) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![instance_id,receipt.join_node_id,receipt.activation_id,receipt.branch_edge_id,receipt.token_id,at_ms,receipt.scope_id,match receipt.gateway_kind { GatewayKind::Parallel => "parallel", GatewayKind::Inclusive => "inclusive" }])?;
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
        let waiting: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM bpmn_tokens WHERE token_id=?1 AND instance_id=?2 AND node_id=?3 AND scope_id=?4 AND status='waiting')",params![token_id,instance_id,task.node_id,task.scope_id],|row|row.get(0))?;
        ensure!(
            waiting,
            "new user task does not match its waiting activation"
        );
        let node = scope_node(
            &model,
            &proposed_scopes,
            instance_id,
            &task.scope_id,
            &task.node_id,
        )?;
        let expected_assignee = match (&node.kind, &task.kind) {
            (
                ProcessNodeKind::UserTask {
                    assignee_user_id, ..
                },
                ProcessUserTaskKind::Work,
            ) => assignee_user_id.as_deref().unwrap_or(&initiator),
            (ProcessNodeKind::ServiceTask { .. }, ProcessUserTaskKind::Verification) => {
                initiator.as_str()
            }
            _ => bail!("new user task kind differs from its exact scoped activity"),
        };
        ensure!(
            task.assignee_user_id == expected_assignee && task.name == node.name,
            "new work identity differs from its pinned scoped activity"
        );
        tx.execute("INSERT INTO bpmn_user_tasks(user_task_id,instance_id,node_id,name,assignee_user_id,kind,status,outputs_json,revision,created_at_ms,updated_at_ms,token_id,scope_id) VALUES(?1,?2,?3,?4,?5,?6,'open',?7,1,?8,?8,?9,?10)",params![task.user_task_id,instance_id,task.node_id,task.name,task.assignee_user_id,task_kind_text(&task.kind),json(&task.outputs)?,at_ms,token_id,task.scope_id])?;
    }
    for job in &plan.create_jobs {
        ensure!(
            job.instance_id == instance_id
                && job.status == "queued"
                && job.attempt == 0
                && job.fence == 0,
            "invalid queued service job"
        );
        let node = scope_node(
            &model,
            &proposed_scopes,
            instance_id,
            &job.scope_id,
            &job.node_id,
        )?;
        let ProcessNodeKind::ServiceTask { flow_id, .. } = &node.kind else {
            bail!("queued job node is not a service in its scope body")
        };
        require_flow_current(
            tx,
            &ProcessActor {
                org_id: org_id.clone(),
                user_id: initiator.clone(),
            },
            flow_id,
            None,
        )?;
        validate_output(&job.input)?;
        let waiting:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM bpmn_tokens WHERE instance_id=?1 AND scope_id=?2 AND token_id=?3 AND node_id=?4 AND status='waiting')",params![instance_id,job.scope_id,job.token_id,job.node_id],|row|row.get(0))?;
        ensure!(
            waiting,
            "new job differs from its actual scoped service wait"
        );
        tx.execute("INSERT INTO bpmn_jobs(job_id,instance_id,node_id,token_id,input_json,status,created_at_ms,updated_at_ms,scope_id) VALUES(?1,?2,?3,?4,?5,'queued',?6,?6,?7)",params![job.job_id,instance_id,job.node_id,job.token_id,json(&job.input)?,at_ms,job.scope_id])?;
    }
    for token_id in &plan.consume_token_ids {
        let mut statement=tx.prepare("SELECT user_task_id FROM bpmn_user_tasks WHERE instance_id=?1 AND token_id=?2 AND status='open'")?;
        ensure!(
            statement
                .query_map(params![instance_id, token_id], |row| row
                    .get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .iter()
                .all(|id| plan.complete_user_task_ids.contains(id)),
            "token consumption omits its actual open work"
        );
        let mut statement=tx.prepare("SELECT job_id FROM bpmn_jobs WHERE instance_id=?1 AND token_id=?2 AND status IN ('queued','running')")?;
        ensure!(
            statement
                .query_map(params![instance_id, token_id], |row| row
                    .get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?
                .iter()
                .all(|id| plan.complete_job_ids.contains(id)),
            "token consumption omits its actual active service job"
        );
    }
    for token_id in &plan.consume_token_ids {
        let affected = tx.execute("UPDATE bpmn_tokens SET status='consumed' WHERE token_id=?1 AND instance_id=?2 AND status IN ('ready','waiting','joining')",params![token_id,instance_id])?;
        ensure!(affected == 1, "process token changed before transition");
    }
    for token_id in &plan.cancel_token_ids {
        let affected = tx.execute("UPDATE bpmn_tokens SET status='cancelled' WHERE token_id=?1 AND instance_id=?2 AND status IN ('ready','waiting','joining')",params![token_id,instance_id])?;
        ensure!(
            affected == 1,
            "interrupted activity token changed before transition"
        );
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
    for incident in &plan.add_incidents {
        ensure!(
            incident.message.len() <= 32 * 1024,
            "process incident message exceeds 32 KiB"
        );
        tx.execute("INSERT INTO bpmn_incidents(incident_id,instance_id,node_id,job_id,code,message,at_ms,scope_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![incident.incident_id,instance_id,incident.node_id,incident.job_id,incident.code,incident_message(&incident.message),at_ms,incident.scope_id])?;
    }
    for incident_id in &plan.resolve_incident_ids {
        let affected = tx.execute("UPDATE bpmn_incidents SET resolved_at_ms=?1 WHERE incident_id=?2 AND instance_id=?3 AND resolved_at_ms IS NULL",params![at_ms,incident_id,instance_id])?;
        ensure!(affected == 1, "process incident changed before transition");
    }
    check_active_incident_budget_on(tx, instance_id)?;
    let next_seq: u64 = tx.query_row(
        "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
        [instance_id],
        |row| row_u64(row, 0),
    )?;
    let mut event_ids = Vec::new();
    for (index, planned) in plan.events.iter().enumerate() {
        let mut event = planned.clone();
        if event.kind == "scope_error_propagated" {
            let source_index = usize::try_from(
                event.data["source_result_index"]
                    .as_u64()
                    .context("propagated error source index missing")?,
            )?;
            let source = plan
                .events
                .get(source_index)
                .context("propagated error source event missing")?;
            ensure!(
                source_index < index
                    && source.kind == "service_result"
                    && source.data["result_origin"].as_str() == Some("contract")
                    && source.scope_id
                        == event.data["source_scope_id"]
                            .as_str()
                            .context("propagated error source scope missing")?,
                "propagated error must reference an earlier factual contract result"
            );
            event
                .data
                .as_object_mut()
                .context("propagation facts must be an object")?
                .remove("source_result_index");
            event.data["result_event_id"] = serde_json::json!(event_ids
                .get(source_index)
                .context("accepted result event not committed in this plan")?);
        }
        event_ids.push(insert_event_on(
            tx,
            instance_id,
            next_seq
                .checked_add(u64::try_from(index)?)
                .context("process event sequence overflow")?,
            Some(actor_id),
            &event,
            at_ms,
            plan.event_ids.get(&index).map(String::as_str),
        )?);
    }
    let (org_id, initiator): (String, String) = tx.query_row(
        "SELECT org_id,initiator_user_id FROM bpmn_instances WHERE instance_id=?1",
        [instance_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let sender = ProcessActor {
        org_id,
        user_id: initiator,
    };
    for message in &plan.create_messages {
        let event = plan
            .events
            .get(message.source_event_index)
            .context("outbox event index is absent")?;
        ensure!(
            event.kind == "message_queued"
                && event.scope_id == message.source_scope_id
                && event.node_id.as_deref() == Some(message.source_node_id.as_str())
                && event.data["message_id"].as_str() == Some(message.message.message_id.as_str())
                && event.data["source_activation_id"].as_str()
                    == Some(message.source_activation_id.as_str()),
            "outbox identity differs from its actual queued event"
        );
        insert_message_on(
            tx,
            &sender,
            &message.message,
            Some((
                instance_id,
                &message.source_scope_id,
                &message.source_node_id,
                &message.source_activation_id,
                &event_ids[message.source_event_index],
            )),
            at_ms,
        )?;
    }
    cancelled_claims.extend(cancel_call_children_on(
        tx,
        instance_id,
        Some(&plan.cancel_token_ids),
        &plan.cancel_scope_roots,
        actor_id,
        if plan.terminal_error.is_some() {
            "error_end"
        } else {
            "call_interrupted"
        },
        at_ms,
    )?);
    for update in &plan.scope_updates {
        if matches!(
            update.status,
            ProcessInstanceStatus::Completed
                | ProcessInstanceStatus::Cancelled
                | ProcessInstanceStatus::Error
        ) {
            let controls:bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM bpmn_tokens WHERE instance_id=?1 AND scope_id=?2 AND status IN ('ready','waiting','joining')) OR EXISTS(SELECT 1 FROM bpmn_gateway_receipts WHERE instance_id=?1 AND scope_id=?2) OR EXISTS(SELECT 1 FROM bpmn_user_tasks WHERE instance_id=?1 AND scope_id=?2 AND status='open') OR EXISTS(SELECT 1 FROM bpmn_jobs j JOIN bpmn_tokens t ON t.token_id=j.token_id AND t.scope_id=j.scope_id AND t.instance_id=j.instance_id WHERE j.instance_id=?1 AND j.scope_id=?2 AND j.status IN ('queued','running','error') AND t.status IN ('ready','waiting','joining')) OR EXISTS(SELECT 1 FROM bpmn_event_subscriptions WHERE instance_id=?1 AND scope_id=?2 AND status='open') OR EXISTS(SELECT 1 FROM bpmn_timers WHERE instance_id=?1 AND scope_id=?2 AND status IN ('pending','blocked')) OR EXISTS(SELECT 1 FROM bpmn_event_races WHERE instance_id=?1 AND scope_id=?2 AND status='open') OR EXISTS(SELECT 1 FROM bpmn_incidents WHERE instance_id=?1 AND scope_id=?2 AND resolved_at_ms IS NULL) OR EXISTS(SELECT 1 FROM bpmn_calls WHERE parent_instance_id=?1 AND parent_scope_id=?2 AND status IN ('waiting','return_incident'))",
                params![instance_id,update.scope_id],|row|row.get(0))?;
            ensure!(
                !controls,
                "terminal child scope retains active control state or incidents"
            );
            let scope = proposed_scopes
                .iter()
                .find(|scope| scope.scope_id == update.scope_id)
                .context("terminal child missing")?;
            if update.status == ProcessInstanceStatus::Completed {
                let ended:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM bpmn_events WHERE instance_id=?1 AND scope_id=?2 AND kind='end_reached')",params![instance_id,scope.scope_id],|row|row.get(0))?;
                ensure!(
                    ended,
                    "child completion has not reached its actual local End"
                );
                let parent_token = scope
                    .parent_token_id
                    .as_deref()
                    .context("completed scope parent wait missing")?;
                ensure!(
                    plan.consume_token_ids.iter().any(|id| id == parent_token)
                        && plan
                            .events
                            .iter()
                            .any(|event| event.kind == "scope_completed"
                                && event.scope_id == scope.scope_id
                                && event.data["parent_token_id"].as_str() == Some(parent_token)),
                    "scope completion does not consume its exact parent waiting activation"
                );
            } else {
                ensure!(
                    plan.cancel_scope_roots
                        .iter()
                        .any(|root| descendant_scope_ids(&proposed_scopes, root)
                            .is_ok_and(|ids| ids.contains(&scope.scope_id))),
                    "scope cancellation lacks its validated closure"
                );
            }
        }
        let error = plan.scope_terminal_errors.get(&update.scope_id);
        ensure!(
            (update.status == ProcessInstanceStatus::Error) == error.is_some(),
            "Error scope requires its actual terminal outcome"
        );
        if let Some(fact) = error {
            validate_terminal_error_on(tx, instance_id, fact)?;
        }
        let count = tx.execute("UPDATE bpmn_scopes SET status=?1,local_variables_json=COALESCE(?2,local_variables_json),revision=revision+1,updated_at_ms=?3,terminal_error_json=?7,error_event_id=?8,error_scope_id=?9 WHERE scope_id=?4 AND instance_id=?5 AND revision=?6 AND parent_scope_id IS NOT NULL AND status NOT IN ('completed','cancelled','error')",
            params![status_text(&update.status),update.variables.as_ref().map(json).transpose()?,at_ms,update.scope_id,instance_id,sql_incrementable(update.expected_revision)?,error.map(json).transpose()?,error.map(|e|&e.source_event_id),error.map(|e|&e.source_scope_id)])?;
        ensure!(
            count == 1,
            "process scope revision conflict or terminal scope"
        );
    }
    let mut closure = HashSet::new();
    for root in &plan.cancel_scope_roots {
        closure.extend(descendant_scope_ids(&proposed_scopes, root)?);
    }
    if !closure.is_empty() {
        let mut query = tx.prepare("SELECT org_id,sender_user_id,message_id FROM bpmn_messages WHERE source_instance_id=?1 AND status IN ('pending','blocked','ambiguous') ORDER BY sender_user_id,message_id")?;
        let keys = query
            .query_map([instance_id], |row| {
                Ok(MessageKey {
                    org_id: row.get(0)?,
                    sender_user_id: row.get(1)?,
                    message_id: row.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(query);
        for key in keys {
            let message = message_on(tx, &key, false)?;
            if message
                .source_scope_id
                .as_ref()
                .is_some_and(|id| closure.contains(id))
            {
                ensure!(
                    update_message_state_on(
                        tx,
                        &message,
                        ProcessMessageStatus::Cancelled,
                        Some("scope_cancelled"),
                        at_ms,
                        at_ms
                    )?,
                    "outgoing scope receipt changed before closure"
                );
            }
        }
    }
    validate_gateway_state_on(tx, instance_id, &model, &proposed_scopes)?;
    let unresolved:bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM bpmn_incidents WHERE instance_id=?1 AND resolved_at_ms IS NULL) OR EXISTS(SELECT 1 FROM bpmn_timers x JOIN bpmn_tokens t ON t.token_id=x.token_id AND t.instance_id=x.instance_id AND t.scope_id=x.scope_id WHERE x.instance_id=?1 AND x.kind='catch' AND x.status='error' AND t.status='waiting')",[instance_id],|row|row.get(0))?;
    let status = if unresolved {
        ProcessInstanceStatus::Incident
    } else {
        plan.status.clone()
    };
    if status == ProcessInstanceStatus::Completed || status == ProcessInstanceStatus::Error {
        let ended:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM bpmn_events WHERE instance_id=?1 AND scope_id=?1 AND kind='end_reached')",[instance_id],|row|row.get(0))?;
        ensure!(
            status == ProcessInstanceStatus::Error || ended,
            "root completion has not reached its actual End"
        );
        let active:bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM bpmn_tokens WHERE instance_id=?1 AND status IN ('ready','waiting','joining')) OR EXISTS(SELECT 1 FROM bpmn_scopes WHERE instance_id=?1 AND parent_scope_id IS NOT NULL AND status NOT IN ('completed','cancelled','error')) OR EXISTS(SELECT 1 FROM bpmn_calls WHERE parent_instance_id=?1 AND status IN ('waiting','return_incident')) OR EXISTS(SELECT 1 FROM bpmn_user_tasks WHERE instance_id=?1 AND status='open') OR EXISTS(SELECT 1 FROM bpmn_jobs j JOIN bpmn_tokens t ON t.token_id=j.token_id WHERE j.instance_id=?1 AND j.status IN ('queued','running','error') AND t.status IN ('ready','waiting','joining')) OR EXISTS(SELECT 1 FROM bpmn_gateway_receipts WHERE instance_id=?1) OR EXISTS(SELECT 1 FROM bpmn_event_subscriptions WHERE instance_id=?1 AND status='open') OR EXISTS(SELECT 1 FROM bpmn_timers WHERE instance_id=?1 AND status IN ('pending','blocked')) OR EXISTS(SELECT 1 FROM bpmn_event_races WHERE instance_id=?1 AND status='open') OR EXISTS(SELECT 1 FROM bpmn_incidents WHERE instance_id=?1 AND resolved_at_ms IS NULL)",[instance_id],|row|row.get(0))?;
        ensure!(
            !active,
            "root completion retains active tokens or child scopes"
        );
    }
    ensure!(
        (status == ProcessInstanceStatus::Error) == plan.terminal_error.is_some(),
        "terminal instance Error requires its actual ErrorEnd outcome"
    );
    if let Some(fact) = &plan.terminal_error {
        validate_terminal_error_on(tx, instance_id, fact)?;
    }
    let affected = tx.execute("UPDATE bpmn_instances SET status=?1,variables_json=?2,revision=revision+1,updated_at_ms=?3,terminal_error_json=?6,error_event_id=?7,error_scope_id=?8 WHERE instance_id=?4 AND revision=?5 AND status NOT IN ('completed','cancelled','error')",
        params![status_text(&status),json(&plan.variables)?,at_ms,instance_id,sql_incrementable(expected_revision)?,plan.terminal_error.as_ref().map(json).transpose()?,plan.terminal_error.as_ref().map(|e|&e.source_event_id),plan.terminal_error.as_ref().map(|e|&e.source_scope_id)])?;
    ensure!(
        affected == 1,
        "process instance revision conflict or closed instance"
    );
    cancelled_claims.extend(apply_call_steps_on(tx, instance_id, actor_id, plan, at_ms)?);
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
            let prior: StoredInstanceIdentity = parse(result)?;
            require_instance_reader(conn, actor, &prior.instance_id)?;
            return Ok(Some(reproject_instance_on(
                conn,
                actor,
                &prior.instance_id,
            )?));
        }
        Ok(None)
    })
}

fn call_tree_ids_on(conn: &Connection, instance_id: &str) -> Result<Vec<String>> {
    let mut root = instance_id.to_owned();
    let mut seen = HashSet::new();
    loop {
        ensure!(
            seen.insert(root.clone()),
            "cyclic process call instance ancestry"
        );
        let parent: Option<String> = conn
            .query_row(
                "SELECT parent_instance_id FROM bpmn_calls WHERE child_instance_id=?1",
                [&root],
                |r| r.get(0),
            )
            .optional()?;
        match parent {
            Some(value) => root = value,
            None => break,
        }
        ensure!(seen.len() <= 4, "process call instance depth exceeds three");
    }
    let mut ids = vec![root];
    let mut index = 0;
    while index < ids.len() {
        let mut query = conn.prepare(
            "SELECT child_instance_id FROM bpmn_calls WHERE parent_instance_id=?1 ORDER BY call_id",
        )?;
        for child in query
            .query_map([&ids[index]], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?
        {
            ensure!(
                !ids.contains(&child),
                "cyclic or shared process call instance"
            );
            ids.push(child);
            ensure!(
                ids.len() <= 129,
                "process call tree lifetime capacity exceeded"
            );
        }
        index += 1;
    }
    Ok(ids)
}

fn call_initiator_on(conn: &Connection, instance_id: &str) -> Result<ProcessActor> {
    let (org_id, user_id): (String, String) = conn.query_row(
        "SELECT org_id,initiator_user_id FROM bpmn_instances WHERE instance_id=?1",
        [instance_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(ProcessActor { org_id, user_id })
}

fn call_control_live_on(conn: &Connection, instance_id: &str) -> Result<bool> {
    let mut id = instance_id.to_owned();
    let actor = call_initiator_on(conn, instance_id)?;
    let mut depth = 0;
    loop {
        let incoming = calls_on(conn, &id)?
            .into_iter()
            .find(|c| c.child_instance_id == id);
        let Some(call) = incoming else {
            return Ok(true);
        };
        depth += 1;
        ensure!(depth <= 3, "process call instance depth exceeds three");
        let live:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM bpmn_tokens t JOIN bpmn_instances i ON i.instance_id=t.instance_id JOIN bpmn_scopes s ON s.instance_id=t.instance_id AND s.scope_id=t.scope_id WHERE t.instance_id=?1 AND t.scope_id=?2 AND t.token_id=?3 AND t.node_id=?4 AND t.status='waiting' AND i.status NOT IN ('completed','cancelled','error') AND (s.parent_scope_id IS NULL OR s.status NOT IN ('completed','cancelled','error')) AND i.org_id=?5 AND i.initiator_user_id=?6)",params![call.parent_instance_id,call.parent_scope_id,call.parent_token_id,call.call_node_id,actor.org_id,actor.user_id],|r|r.get(0))?;
        if call.status != ProcessCallStatus::Waiting || !live {
            return Ok(false);
        }
        id = call.parent_instance_id;
    }
}

fn require_call_control_authority_on(
    conn: &Connection,
    actor: &ProcessActor,
    instance_id: &str,
) -> Result<()> {
    ensure!(
        call_control_live_on(conn, instance_id)?,
        "called process control ancestry is closed"
    );
    let mut id = instance_id.to_owned();
    loop {
        let (definition,version,org,initiator):(String,u32,String,String)=conn.query_row("SELECT definition_id,version,org_id,initiator_user_id FROM bpmn_instances WHERE instance_id=?1",[&id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        ensure!(
            org == actor.org_id && initiator == actor.user_id,
            "called process actor differs from actual initiator"
        );
        require_owner(conn, actor, &definition)?;
        require_version_execution_on(conn, actor, &definition, version)?;
        let Some(call) = calls_on(conn, &id)?
            .into_iter()
            .find(|c| c.child_instance_id == id)
        else {
            return Ok(());
        };
        id = call.parent_instance_id;
    }
}

fn initial_call_snapshot(
    actor: &ProcessActor,
    instance_id: &str,
    version: &PinnedCallVersion,
    variables: &Value,
    all_versions: &[PinnedCallVersion],
    at_ms: i64,
) -> Result<RuntimeSnapshot> {
    let mut vars = serde_json::to_value(&version.version.model.variables)?;
    for (key, value) in variables
        .as_object()
        .context("called inputs are not an object")?
    {
        vars.as_object_mut()
            .context("published default variables are not an object")?
            .insert(key.clone(), value.clone());
    }
    validate_variables(&vars)?;
    let instance = ProcessInstance {
        instance_id: instance_id.to_owned(),
        definition_id: version.version.definition_id.clone(),
        definition_name: version.name.clone(),
        initiator_user_id: actor.user_id.clone(),
        version: version.version.version,
        revision: 1,
        status: ProcessInstanceStatus::Running,
        variables: vars,
        active_node_ids: Vec::new(),
        user_tasks: Vec::new(),
        incidents: Vec::new(),
        created_at_ms: at_ms,
        updated_at_ms: at_ms,
        can_cancel: true,
        can_retry: false,
        timers: Vec::new(),
        subscriptions: Vec::new(),
        event_races: Vec::new(),
        outgoing_messages: Vec::new(),
        message_names: Vec::new(),
        can_send_message: Some(true),
        pages: None,
        selected_user_task: None,
        selected_incident: None,
        scopes: Vec::new(),
        calls: Vec::new(),
        terminal_error: None,
    };
    let root = ProcessScopeSummary {
        scope_id: instance_id.to_owned(),
        parent_scope_id: None,
        subprocess_node_id: None,
        subprocess_node_name: None,
        parent_token_id: None,
        revision: 1,
        status: ProcessInstanceStatus::Running,
        depth: 0,
        created_at_ms: at_ms,
        updated_at_ms: at_ms,
        terminal_error: None,
    };
    Ok(RuntimeSnapshot {
        org_id: actor.org_id.clone(),
        instance,
        model: version.version.model.clone(),
        user_tasks: Vec::new(),
        tokens: Vec::new(),
        jobs: Vec::new(),
        receipts: Vec::new(),
        service_snapshots: version.services.clone(),
        timers: Vec::new(),
        boundary_incidents: Vec::new(),
        subscriptions: Vec::new(),
        event_races: Vec::new(),
        incidents: Vec::new(),
        scopes: vec![root],
        scope_variables: BTreeMap::new(),
        calls: Vec::new(),
        call_versions: all_versions.to_vec(),
        retained_scope_count: 1,
    })
}

fn projected_call_budget(snapshots: &BTreeMap<String, RuntimeSnapshot>) -> Result<()> {
    let mut rows = 0usize;
    let mut active = 0usize;
    for snapshot in snapshots.values() {
        rows = rows
            .checked_add(snapshot.scopes.len())
            .context("process call lifetime count overflow")?;
        ensure!(
            rows <= 129,
            "process call tree exceeds 129 retained scope rows"
        );
        if !matches!(
            snapshot.instance.status,
            ProcessInstanceStatus::Completed
                | ProcessInstanceStatus::Cancelled
                | ProcessInstanceStatus::Error
        ) {
            active = active
                .checked_add(json(&snapshot.instance.variables)?.len())
                .context("process active variable size overflow")?;
        }
        for scope in &snapshot.scopes {
            if scope.parent_scope_id.is_some()
                && !matches!(
                    scope.status,
                    ProcessInstanceStatus::Completed
                        | ProcessInstanceStatus::Cancelled
                        | ProcessInstanceStatus::Error
                )
            {
                active = active
                    .checked_add(
                        json(
                            snapshot
                                .scope_variables
                                .get(&scope.scope_id)
                                .context("active child variables missing")?,
                        )?
                        .len(),
                    )
                    .context("process active variable size overflow")?;
            }
        }
        ensure!(
            active <= 1024 * 1024,
            "process call tree active variables exceed 1 MiB"
        );
    }
    Ok(())
}

fn append_call_plan(
    snapshots: &mut BTreeMap<String, RuntimeSnapshot>,
    instance_id: &str,
    plan: &RuntimePlan,
    at_ms: i64,
) -> Result<()> {
    let current = snapshots
        .get(instance_id)
        .context("composite process state missing")?;
    let next = super::runtime::project_snapshot(current, plan, at_ms)?;
    snapshots.insert(instance_id.to_owned(), next);
    let retained = snapshots
        .values()
        .map(|snapshot| snapshot.scopes.len())
        .sum();
    for snapshot in snapshots.values_mut() {
        snapshot.retained_scope_count = retained;
    }
    Ok(())
}

fn prepare_call_steps(
    instance_id: &str,
    local_plan: &RuntimePlan,
    snapshots: &mut BTreeMap<String, RuntimeSnapshot>,
    authority: &BTreeMap<String, String>,
    steps: &mut Vec<CallStep>,
    at_ms: i64,
) -> Result<()> {
    let mut pending = vec![(instance_id.to_owned(), Box::new(local_plan.clone()), 0usize)];
    'pending: while let Some((instance_id, local_plan, next_request)) = pending.pop() {
        if let Some(request) = local_plan.call_requests.get(next_request).cloned() {
            pending.push((instance_id.clone(), local_plan, next_request + 1));
            let parent = snapshots
                .get(&instance_id)
                .context("planned call parent state missing")?
                .clone();
            if !parent
                .tokens
                .iter()
                .any(|t| t.token_id == request.parent_token_id && t.status == "waiting")
            {
                continue;
            }
            let node = scope_node(
                &parent.model,
                &parent.scopes,
                &instance_id,
                &request.parent_scope_id,
                &request.call_node_id,
            )?;
            let ProcessNodeKind::CallActivity {
                called_definition_id,
                called_version,
                ..
            } = &node.kind
            else {
                bail!("planned call node is not CallActivity")
            };
            let target = parent
                .call_versions
                .iter()
                .find(|v| {
                    v.version.definition_id == *called_definition_id
                        && v.version.version == *called_version
                })
                .context("immutable called process version missing")?
                .clone();
            let mut depth = 1;
            let mut current = instance_id.to_owned();
            while let Some(call) = snapshots
                .get(&current)
                .and_then(|s| s.calls.iter().find(|c| c.child_instance_id == current))
            {
                depth += 1;
                current = call.parent_instance_id.clone();
            }
            let actor = ProcessActor {
                org_id: parent.org_id.clone(),
                user_id: parent.instance.initiator_user_id.clone(),
            };
            let prepared = (|| -> Result<_> {
                let child = initial_call_snapshot(
                    &actor,
                    &request.child_instance_id,
                    &target,
                    &request.variables,
                    &parent.call_versions,
                    at_ms,
                )?;
                let plan = super::runtime::plan_start(
                    &target.version.model,
                    &request.child_instance_id,
                    &actor,
                    called_definition_id,
                    *called_version,
                    child.instance.variables.clone(),
                    super::runtime::StartCause::Manual,
                    at_ms,
                )?;
                Ok((child, plan))
            })();
            let (child, plan) = match prepared {
                Ok(pair) => pair,
                Err(error) => {
                    let incident = super::runtime::plan_call_incident(
                        &parent,
                        &node.id,
                        &request.parent_scope_id,
                        "CALL_ADMISSION_ERROR",
                        &format!("{error:#}"),
                        at_ms,
                    )?;
                    steps.push(CallStep::Advance {
                        instance_id: instance_id.to_owned(),
                        expected_revision: parent.instance.revision,
                        plan: Box::new(incident.clone()),
                    });
                    append_call_plan(snapshots, &instance_id, &incident, at_ms)?;
                    continue;
                }
            };
            let mut proposed = snapshots.clone();
            proposed.insert(
                request.child_instance_id.clone(),
                super::runtime::project_snapshot(&child, &plan, at_ms)?,
            );
            let error = target
                .admission_error
                .clone()
                .or_else(|| authority.get(&instance_id).cloned())
                .or_else(|| (depth > 3).then(|| "process call depth exceeds three".into()))
                .or_else(|| {
                    projected_call_budget(&proposed)
                        .err()
                        .map(|e| format!("{e:#}"))
                });
            if let Some(reason) = error {
                let incident = super::runtime::plan_call_incident(
                    &parent,
                    &node.id,
                    &request.parent_scope_id,
                    "CALL_ADMISSION_ERROR",
                    &reason,
                    at_ms,
                )?;
                steps.push(CallStep::Advance {
                    instance_id: instance_id.to_owned(),
                    expected_revision: parent.instance.revision,
                    plan: Box::new(incident.clone()),
                });
                append_call_plan(snapshots, &instance_id, &incident, at_ms)?;
                continue;
            }
            let call = CallActivation {
                call_id: request.call_id.clone(),
                parent_instance_id: instance_id.to_owned(),
                parent_scope_id: request.parent_scope_id.clone(),
                parent_token_id: request.parent_token_id.clone(),
                call_node_id: node.id.clone(),
                child_instance_id: request.child_instance_id.clone(),
                definition_id: parent.instance.definition_id.clone(),
                version: parent.instance.version,
                called_definition_id: called_definition_id.clone(),
                called_version: *called_version,
                model_sha256: target.version.model_sha256.clone(),
                revision: 1,
                status: ProcessCallStatus::Waiting,
                created_at_ms: at_ms,
                updated_at_ms: at_ms,
            };
            snapshots
                .get_mut(&instance_id)
                .context("call parent state missing")?
                .calls
                .push(call.clone());
            let mut child = child;
            child.calls.push(call.clone());
            snapshots.insert(request.child_instance_id.clone(), child);
            steps.push(CallStep::Start {
                call: PlannedCall {
                    call: call.clone(),
                    variables: request.variables.clone(),
                },
                plan: Box::new(plan.clone()),
            });
            append_call_plan(snapshots, &request.child_instance_id, &plan, at_ms)?;
            pending.push((request.child_instance_id.clone(), Box::new(plan), 0));
            continue;
        }
        if let Some(source) = &local_plan.business_error {
            let mut child = instance_id.to_owned();
            let mut direct = None;
            while let Some(call) = snapshots
                .get(&child)
                .and_then(|s| {
                    s.calls.iter().find(|c| {
                        c.child_instance_id == child && c.status == ProcessCallStatus::Waiting
                    })
                })
                .cloned()
            {
                if direct.is_none() {
                    direct = Some(call.clone());
                }
                let parent = snapshots
                    .get(&call.parent_instance_id)
                    .context("error caller state missing")?
                    .clone();
                if authority.contains_key(&call.parent_instance_id) {
                    break;
                }
                if let Some(plan) = super::runtime::plan_call_error(&parent, &call, source, at_ms)?
                {
                    let status = if matches!(source,BusinessErrorSource::ErrorEnd {instance_id,..} if instance_id==&call.child_instance_id)
                    {
                        ProcessCallStatus::Error
                    } else {
                        ProcessCallStatus::Cancelled
                    };
                    steps.push(CallStep::Return {
                        call: call.clone(),
                        expected_revision: parent.instance.revision,
                        status,
                        source: Some(source.clone()),
                        plan: Box::new(plan.clone()),
                    });
                    append_call_plan(snapshots, &call.parent_instance_id, &plan, at_ms)?;
                    pending.push((call.parent_instance_id.clone(), Box::new(plan), 0));
                    continue 'pending;
                }
                child = call.parent_instance_id;
            }
            if let (Some(call), BusinessErrorSource::ErrorEnd { .. }) = (direct, source) {
                let parent = snapshots
                    .get(&call.parent_instance_id)
                    .context("error caller state missing")?
                    .clone();
                let plan = super::runtime::plan_call_incident(
                    &parent,
                    &call.call_node_id,
                    &call.parent_scope_id,
                    "CALL_CHILD_ERROR",
                    "the called process ended with an uncaught business error",
                    at_ms,
                )?;
                steps.push(CallStep::Return {
                    call: call.clone(),
                    expected_revision: parent.instance.revision,
                    status: ProcessCallStatus::Error,
                    source: Some(source.clone()),
                    plan: Box::new(plan.clone()),
                });
                append_call_plan(snapshots, &call.parent_instance_id, &plan, at_ms)?;
            }
            continue 'pending;
        }
        let child = snapshots
            .get(&instance_id)
            .context("completed call child state missing")?
            .clone();
        if child.instance.status == ProcessInstanceStatus::Completed {
            if let Some(call) = child
                .calls
                .iter()
                .find(|c| {
                    c.child_instance_id == instance_id && c.status == ProcessCallStatus::Waiting
                })
                .cloned()
            {
                let parent = snapshots
                    .get(&call.parent_instance_id)
                    .context("call return parent state missing")?
                    .clone();
                let candidate = match authority.get(&call.parent_instance_id) {
                    Some(reason) => Err(anyhow::anyhow!(reason.clone())),
                    None => super::runtime::plan_call_return(
                        &parent,
                        &call,
                        &child.instance.variables,
                        at_ms,
                    ),
                };
                let (plan, status) = match candidate {
                    Ok(plan) => {
                        let mut proposed = snapshots.clone();
                        let checked = super::runtime::project_snapshot(&parent, &plan, at_ms)
                            .and_then(|projected| {
                                proposed.insert(call.parent_instance_id.clone(), projected);
                                projected_call_budget(&proposed)
                            });
                        if let Err(error) = checked {
                            (
                                super::runtime::plan_call_incident(
                                    &parent,
                                    &call.call_node_id,
                                    &call.parent_scope_id,
                                    "CALL_RETURN_ERROR",
                                    &format!("{error:#}"),
                                    at_ms,
                                )?,
                                ProcessCallStatus::ReturnIncident,
                            )
                        } else {
                            (plan, ProcessCallStatus::Returned)
                        }
                    }
                    Err(error) => (
                        super::runtime::plan_call_incident(
                            &parent,
                            &call.call_node_id,
                            &call.parent_scope_id,
                            "CALL_RETURN_ERROR",
                            &format!("{error:#}"),
                            at_ms,
                        )?,
                        ProcessCallStatus::ReturnIncident,
                    ),
                };
                steps.push(CallStep::Return {
                    call: call.clone(),
                    expected_revision: parent.instance.revision,
                    status: status.clone(),
                    source: None,
                    plan: Box::new(plan.clone()),
                });
                for snapshot in snapshots.values_mut() {
                    for actual in &mut snapshot.calls {
                        if actual.call_id == call.call_id {
                            actual.status = status.clone();
                            actual.revision += 1;
                        }
                    }
                }
                append_call_plan(snapshots, &call.parent_instance_id, &plan, at_ms)?;
                pending.push((call.parent_instance_id.clone(), Box::new(plan), 0));
            }
        }
    }
    Ok(())
}

fn prepare_call_plan<'pool>(
    pool: &'pool DbPool,
    actor: &ProcessActor,
    instance_id: &str,
    initial: Option<(&str, u32, &Value)>,
    plan: &RuntimePlan,
    at_ms: i64,
) -> Result<(RuntimePlan, parking_lot::MutexGuard<'pool, Connection>)> {
    ensure!(
        plan.call_steps.is_empty(),
        "composite plans are server-minted by the canonical repository"
    );
    for _ in 0..8 {
        let (mut snapshots, authority) = read_snapshot(pool, |conn| {
            let mut snapshots = BTreeMap::new();
            let mut authority = BTreeMap::new();
            if let Some((definition_id, version, variables)) = initial {
                require_actor(conn, actor)?;
                require_owner(conn, actor, definition_id)?;
                let ver = version_on(conn, definition_id, version)?;
                let services=parse(conn.query_row("SELECT service_snapshots_json FROM bpmn_versions WHERE definition_id=?1 AND version=?2",params![definition_id,version],|r|r.get::<_,String>(0))?)?;
                let versions =
                    pinned_call_versions_on(conn, actor, definition_id, version, &ver.model)?;
                let pin = PinnedCallVersion {
                    version: ver,
                    services,
                    name: definition_on(conn, definition_id)?.name,
                    admission_error: None,
                };
                snapshots.insert(
                    instance_id.to_owned(),
                    initial_call_snapshot(actor, instance_id, &pin, variables, &versions, at_ms)?,
                );
            } else {
                require_actor(conn, actor)?;
                require_instance_reader(conn, actor, instance_id)?;
                let ids = call_tree_ids_on(conn, instance_id)?;
                let source = call_initiator_on(conn, instance_id)?;
                for id in ids {
                    let initiator = call_initiator_on(conn, &id)?;
                    ensure!(
                        initiator.org_id == source.org_id && initiator.user_id == source.user_id,
                        "call tree organization or initiator mismatch"
                    );
                    let snapshot = runtime_snapshot_on(conn, &initiator, &id)?;
                    if let Err(error) = require_call_control_authority_on(conn, &initiator, &id) {
                        authority.insert(id.clone(), format!("{error:#}"));
                    }
                    snapshots.insert(id, snapshot);
                }
            }
            Ok((snapshots, authority))
        })?;
        let related_revisions = snapshots
            .values()
            .filter(|snapshot| snapshot.instance.instance_id != instance_id)
            .map(|snapshot| {
                (
                    snapshot.instance.instance_id.clone(),
                    snapshot.instance.revision,
                )
            })
            .collect::<Vec<_>>();
        let source = snapshots
            .get(instance_id)
            .context("source composite state missing")?;
        if matches!(
            source.instance.status,
            ProcessInstanceStatus::Completed
                | ProcessInstanceStatus::Cancelled
                | ProcessInstanceStatus::Error
        ) {
            return Ok((plan.clone(), pool.write()?));
        }
        let mut composite = plan.clone();
        append_call_plan(&mut snapshots, instance_id, plan, at_ms)?;
        let mut steps = Vec::new();
        prepare_call_steps(
            instance_id,
            plan,
            &mut snapshots,
            &authority,
            &mut steps,
            at_ms,
        )?;
        composite.call_steps = steps;
        #[cfg(test)]
        CALL_TRANSITION_PREFLIGHT.with(|gate| {
            if let Some((ready, resume)) = gate.borrow_mut().take() {
                ready.send(()).expect("call transition preflight barrier");
                resume.recv().expect("call transition writer barrier");
            }
        });
        let conn = pool.write()?;
        let mut current = true;
        for (id, revision) in related_revisions {
            let actual: Option<u64> = conn
                .query_row(
                    "SELECT revision FROM bpmn_instances WHERE instance_id=?1",
                    [&id],
                    |row| row_u64(row, 0),
                )
                .optional()?;
            if actual != Some(revision) {
                current = false;
                break;
            }
        }
        if current {
            return Ok((composite, conn));
        }
        drop(conn);
    }
    bail!("process call transition revision conflict after bounded replanning")
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
    if let Some(prior) = replay_instance_command(pool, actor, stamp, None)? {
        return read_snapshot(pool, |conn| {
            require_actor(conn, actor)?;
            require_owner(conn, actor, definition_id)?;
            reproject_instance_on(conn, actor, &prior.instance_id)
        });
    }
    let (composite, mut conn) = prepare_call_plan(
        pool,
        actor,
        instance_id,
        Some((definition_id, version, initial_variables)),
        plan,
        at_ms,
    )?;
    let plan = &composite;
    let tx = conn.transaction()?;
    require_actor(&tx, actor)?;
    require_owner(&tx, actor, definition_id)?;
    if let Some(prior) = command_replay::<StoredInstanceIdentity>(&tx, actor, stamp)? {
        require_instance_reader(&tx, actor, &prior.instance_id)?;
        return reproject_instance_on(&tx, actor, &prior.instance_id);
    }
    let result = start_instance_on(
        &tx,
        actor,
        instance_id,
        definition_id,
        version,
        initial_variables,
        plan,
        at_ms,
        None,
        None,
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
    message_identity: Option<&MessageKey>,
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
    ensure!(
        reaches_error_end(plan)
            || (plan.cancel_scope_roots.is_empty()
                && plan.cancel_token_ids.is_empty()
                && plan.cancel_user_task_ids.is_empty()
                && plan.cancel_job_ids.is_empty()),
        "instance start cannot interrupt existing work"
    );
    let model = current_version_model_on(tx, definition_id, version)?;
    let start = model
        .nodes
        .iter()
        .find(|n| {
            matches!(
                n.kind,
                ProcessNodeKind::Start
                    | ProcessNodeKind::TimerStart { .. }
                    | ProcessNodeKind::MessageStart { .. }
            )
        })
        .context("process start missing")?;
    ensure!(
        matches!(start.kind, ProcessNodeKind::TimerStart { .. }) == start_identity.is_some()
            && matches!(start.kind, ProcessNodeKind::MessageStart { .. })
                == message_identity.is_some(),
        "start requires its exact persisted timer/message identity"
    );
    if let Some(key) = message_identity {
        let message = message_on(tx, key, true)?;
        ensure!(
            key.org_id == actor.org_id
                && key.sender_user_id == actor.user_id
                && matches!(&message.target,ProcessMessageTarget::Start{definition_id:target}if target==definition_id)
                && definition.published_version == Some(version)
                && matches!(
                    message.status,
                    ProcessMessageStatus::Pending | ProcessMessageStatus::Blocked
                )
                && plan.events.iter().any(|e| e.kind == "instance_started"
                    && e.data["start_message_id"].as_str() == Some(key.message_id.as_str())),
            "message start identity differs from its actual current envelope"
        );
    }
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
        plan.start_variables.as_ref() == Some(&vars),
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
    tx.execute(
        "INSERT INTO bpmn_scopes(scope_id,instance_id) VALUES(?1,?1)",
        [instance_id],
    )?;
    apply_plan_on(tx, instance_id, &actor.user_id, 1, plan, at_ms)?;
    instance_on(tx, actor, instance_id, None)
}

pub fn apply_transition(
    pool: &DbPool,
    actor: &ProcessActor,
    instance_id: &str,
    expected_revision: u64,
    plan: &RuntimePlan,
    at_ms: i64,
) -> Result<ProcessTransitionOutcome> {
    ensure!(
        reaches_error_end(plan)
            || (plan.cancel_scope_roots.is_empty()
                && plan.cancel_token_ids.is_empty()
                && plan.cancel_job_ids.is_empty()
                && plan.cancel_user_task_ids.is_empty()),
        "ordinary advancement cannot authorize interruption"
    );
    let (composite, mut conn) = prepare_call_plan(pool, actor, instance_id, None, plan, at_ms)?;
    let plan = &composite;
    let tx = conn.transaction()?;
    require_actor(&tx, actor)?;
    let initiator = require_instance_reader(&tx, actor, instance_id)?;
    ensure!(initiator, "only process initiator can advance the process");
    require_call_control_authority_on(&tx, actor, instance_id)?;
    let instance = instance_on(&tx, actor, instance_id, None)?;
    let snapshots_json: String = tx.query_row(
        "SELECT service_snapshots_json FROM bpmn_versions WHERE definition_id=?1 AND version=?2",
        params![instance.definition_id, instance.version],
        |row| row.get(0),
    )?;
    let snapshots: Vec<PinnedServiceSnapshot> = parse(snapshots_json)?;
    for snapshot in &snapshots {
        require_flow_current(&tx, actor, &snapshot.info.flow_id, None)?;
    }
    let cancelled_claims = apply_plan_on(
        &tx,
        instance_id,
        &actor.user_id,
        expected_revision,
        plan,
        at_ms,
    )?;
    let result = instance_on(&tx, actor, instance_id, None)?;
    tx.commit()?;
    Ok(ProcessTransitionOutcome {
        instance: result,
        cancelled_claims,
    })
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
) -> Result<ProcessTransitionOutcome> {
    validate_output(outputs)?;
    ensure!(
        reaches_error_end(plan)
            || (plan.cancel_scope_roots.is_empty()
                && plan.cancel_token_ids.is_empty()
                && plan.cancel_job_ids.is_empty()
                && plan.cancel_user_task_ids.is_empty()),
        "ordinary advancement cannot authorize interruption"
    );
    let (composite, mut conn) = prepare_call_plan(pool, actor, instance_id, None, plan, at_ms)?;
    let plan = &composite;
    let tx = conn.transaction()?;
    require_actor(&tx, actor)?;
    require_instance_reader(&tx, actor, instance_id)?;
    if let Some(prior) = command_replay::<StoredInstanceIdentity>(&tx, actor, stamp)? {
        require_instance_reader(&tx, actor, &prior.instance_id)?;
        return Ok(ProcessTransitionOutcome {
            instance: reproject_instance_on(&tx, actor, &prior.instance_id)?,
            cancelled_claims: Vec::new(),
        });
    }
    let (assignee,kind,status,token_id,node_id):(String,String,String,Option<String>,String)=tx.query_row("SELECT assignee_user_id,kind,status,token_id,node_id FROM bpmn_user_tasks WHERE user_task_id=?1 AND instance_id=?2",params![user_task_id,instance_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).context("user task not found")?;
    let token_id = token_id.context("open user task lacks its waiting activation")?;
    let activation_live: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM bpmn_tokens t JOIN bpmn_instances i ON i.instance_id=t.instance_id JOIN bpmn_scopes s ON s.instance_id=t.instance_id AND s.scope_id=t.scope_id WHERE t.token_id=?1 AND t.instance_id=?2 AND t.node_id=?3 AND t.status='waiting' AND (s.parent_scope_id IS NULL OR s.status NOT IN ('completed','cancelled','error')) AND i.status NOT IN ('completed','cancelled','error'))",params![token_id,instance_id,node_id],|row|row.get(0))?;
    ensure!(
        activation_live && call_control_live_on(&tx, instance_id)?,
        "user task activation is no longer waiting"
    );
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
    let cancelled_claims = apply_plan_on(
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
    let result = instance_on(&tx, actor, instance_id, None)?;
    store_command(&tx, actor, stamp, &result, at_ms)?;
    tx.commit()?;
    Ok(ProcessTransitionOutcome {
        instance: result,
        cancelled_claims,
    })
}

pub fn cancel_instance(
    pool: &DbPool,
    actor: &ProcessActor,
    stamp: &CommandStamp,
    instance_id: &str,
    expected_revision: u64,
) -> Result<ProcessTransitionOutcome> {
    let at_ms = now_ms()?;
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    require_actor(&tx, actor)?;
    let initiator = require_instance_reader(&tx, actor, instance_id)?;
    ensure!(initiator, "only process initiator may cancel");
    if let Some(prior) = command_replay::<StoredInstanceIdentity>(&tx, actor, stamp)? {
        return Ok(ProcessTransitionOutcome {
            instance: reproject_instance_on(&tx, actor, &prior.instance_id)?,
            cancelled_claims: Vec::new(),
        });
    }
    let cancelled_claims = cancel_instance_on(&tx, actor, instance_id, expected_revision, at_ms)?;
    if let Some(call) = calls_on(&tx, instance_id)?
        .into_iter()
        .find(|c| c.child_instance_id == instance_id && c.status == ProcessCallStatus::Waiting)
    {
        tx.execute("UPDATE bpmn_calls SET status='cancelled',revision=revision+1,updated_at_ms=?1 WHERE call_id=?2 AND revision=?3",params![at_ms,call.call_id,sql_incrementable(call.revision)?])?;
        call_incident_on(
            &tx,
            &call,
            &actor.user_id,
            "CALL_CHILD_CANCELLED",
            "the called process was cancelled by its initiator",
            at_ms,
        )?;
    }
    let result = instance_on(&tx, actor, instance_id, None)?;
    store_command(&tx, actor, stamp, &result, at_ms)?;
    tx.commit()?;
    Ok(ProcessTransitionOutcome {
        instance: result,
        cancelled_claims,
    })
}

fn cancel_instance_on(
    tx: &Transaction<'_>,
    actor: &ProcessActor,
    instance_id: &str,
    expected_revision: u64,
    at_ms: i64,
) -> Result<Vec<CancelledJobClaim>> {
    let mut claims = tx.prepare("SELECT job_id,attempt,fence,worker_id FROM bpmn_jobs WHERE instance_id=?1 AND status='running' ORDER BY job_id")?;
    let mut cancelled_claims = claims
        .query_map([instance_id], |row| {
            Ok(CancelledJobClaim {
                job_id: row.get(0)?,
                attempt: row.get(1)?,
                fence: row_u64(row, 2)?,
                worker_id: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(claims);
    for claim in &cancelled_claims {
        sql_incrementable(claim.fence)?;
    }
    retract_call_outbox_on(tx, instance_id, "instance_cancelled", at_ms)?;
    let changed=tx.execute("UPDATE bpmn_instances SET status='cancelled',revision=revision+1,updated_at_ms=?1 WHERE instance_id=?2 AND revision=?3 AND status NOT IN ('completed','cancelled','error')",params![at_ms,instance_id,sql_incrementable(expected_revision)?])?;
    ensure!(
        changed == 1,
        "process instance revision conflict or already closed"
    );
    let current = instance_state_on(tx, actor, instance_id, None, false)?;
    let model = current_version_model_on(tx, &current.definition_id, current.version)?;
    for link in boundary_incidents_on(tx, instance_id)? {
        tx.execute("UPDATE bpmn_incidents SET resolved_at_ms=?1 WHERE incident_id=?2 AND instance_id=?3 AND resolved_at_ms IS NULL",params![at_ms,link.incident_id,instance_id])?;
    }
    for id in timer_ids_on(tx, instance_id)? {
        let timer = timer_on(tx, &id)?;
        if !matches!(
            timer.status,
            ProcessTimerStatus::Pending | ProcessTimerStatus::Blocked
        ) {
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
        insert_event_on(
            tx,
            instance_id,
            seq,
            Some(&actor.user_id),
            &PlannedEvent {
                scope_id: timer
                    .scope_id
                    .clone()
                    .context("instance timer has no scope")?,
                kind: "timer_cancelled".into(),
                node_id: Some(timer.node_id.clone()),
                data: serde_json::json!({"timer_id":id,"kind":timer.kind,"reason":"instance_cancelled","attached_to_id":if timer.kind == ProcessTimerKind::Boundary { timer_activation_node(tx,&model,&timer)? } else { None },"attached_token_id":timer.token_id}),
            },
            at_ms,
            None,
        )?;
    }
    for subscription in subscriptions_on(tx, instance_id)? {
        if subscription.status != ProcessSubscriptionStatus::Open {
            continue;
        }
        tx.execute("UPDATE bpmn_event_subscriptions SET status='cancelled',last_reason='instance_cancelled',revision=revision+1,updated_at_ms=?1 WHERE subscription_id=?2 AND revision=?3 AND status='open'",params![at_ms,subscription.subscription_id,sql_incrementable(subscription.revision)?])?;
        let seq: u64 = tx.query_row(
            "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
            [instance_id],
            |row| row_u64(row, 0),
        )?;
        insert_event_on(
            tx,
            instance_id,
            seq,
            Some(&actor.user_id),
            &PlannedEvent {
                scope_id: subscription.scope_id.clone(),
                kind: "subscription_cancelled".into(),
                node_id: Some(subscription.node_id),
                data: serde_json::json!({"subscription_id":subscription.subscription_id,"attached_token_id":subscription.token_id,"reason":"instance_cancelled"}),
            },
            at_ms,
            None,
        )?;
    }
    for race in races_on(tx, instance_id)? {
        if race.status != ProcessEventRaceStatus::Open {
            continue;
        }
        tx.execute("UPDATE bpmn_event_races SET status='cancelled',revision=revision+1,updated_at_ms=?1 WHERE race_id=?2 AND revision=?3 AND status='open'",params![at_ms,race.race_id,sql_incrementable(race.revision)?])?;
        let seq: u64 = tx.query_row(
            "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
            [instance_id],
            |row| row_u64(row, 0),
        )?;
        insert_event_on(
            tx,
            instance_id,
            seq,
            Some(&actor.user_id),
            &PlannedEvent {
                scope_id: race.scope_id.clone(),
                kind: "event_race_cancelled".into(),
                node_id: Some(race.gateway_node_id),
                data: serde_json::json!({"race_id":race.race_id,"reason":"instance_cancelled"}),
            },
            at_ms,
            None,
        )?;
    }
    for key in message_keys_on(tx, &actor.org_id, Some(instance_id), None, None, None)? {
        let message = message_on(tx, &key, true)?;
        if matches!(
            message.status,
            ProcessMessageStatus::Pending
                | ProcessMessageStatus::Blocked
                | ProcessMessageStatus::Ambiguous
        ) {
            update_message_state_on(
                tx,
                &message,
                ProcessMessageStatus::Cancelled,
                Some("instance_cancelled"),
                at_ms,
                at_ms,
            )?;
        }
    }
    tx.execute(
        "DELETE FROM bpmn_gateway_receipts WHERE instance_id=?1",
        [instance_id],
    )?;
    tx.execute("UPDATE bpmn_tokens SET status='cancelled' WHERE instance_id=?1 AND status IN ('ready','waiting','joining')",[instance_id])?;
    tx.execute("UPDATE bpmn_user_tasks SET status='cancelled',revision=revision+1,updated_at_ms=?2 WHERE instance_id=?1 AND status='open'",params![instance_id,at_ms])?;
    tx.execute("UPDATE bpmn_jobs SET status='cancelled',fence=fence+1,worker_id=NULL,lease_until_ms=NULL,updated_at_ms=?2 WHERE instance_id=?1 AND status IN ('queued','running','error')",params![instance_id,at_ms])?;
    tx.execute("UPDATE bpmn_incidents SET resolved_at_ms=?1 WHERE instance_id=?2 AND resolved_at_ms IS NULL",params![at_ms,instance_id])?;
    let children = scopes_on(tx, instance_id, &model)?;
    for child in children.into_iter().filter(|scope| {
        scope.parent_scope_id.is_some()
            && !matches!(
                scope.status,
                ProcessInstanceStatus::Completed
                    | ProcessInstanceStatus::Cancelled
                    | ProcessInstanceStatus::Error
            )
    }) {
        tx.execute("UPDATE bpmn_scopes SET status='cancelled',revision=revision+1,updated_at_ms=?1 WHERE scope_id=?2 AND instance_id=?3 AND revision=?4 AND status NOT IN ('completed','cancelled','error')",params![at_ms,child.scope_id,instance_id,sql_incrementable(child.revision)?])?;
        let seq: u64 = tx.query_row(
            "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
            [instance_id],
            |row| row_u64(row, 0),
        )?;
        insert_event_on(
            tx,
            instance_id,
            seq,
            Some(&actor.user_id),
            &PlannedEvent {
                scope_id: child.scope_id.clone(),
                kind: "scope_cancelled".into(),
                node_id: None,
                data: serde_json::json!({"scope_id":child.scope_id,"parent_scope_id":child.parent_scope_id,"parent_token_id":child.parent_token_id,"subprocess_node_id":child.subprocess_node_id,"reason":"instance_cancelled"}),
            },
            at_ms,
            None,
        )?;
    }
    let next_seq: u64 = tx.query_row(
        "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
        [instance_id],
        |row| row_u64(row, 0),
    )?;
    insert_event_on(
        tx,
        instance_id,
        next_seq,
        Some(&actor.user_id),
        &PlannedEvent {
            scope_id: instance_id.to_owned(),
            kind: "cancelled".into(),
            node_id: None,
            data: Value::Null,
        },
        at_ms,
        None,
    )?;
    cancelled_claims.extend(cancel_call_children_on(
        tx,
        instance_id,
        None,
        &[],
        &actor.user_id,
        "instance_cancelled",
        at_ms,
    )?);
    Ok(cancelled_claims)
}

fn job_flow_id_on(conn: &Connection, job_id: &str) -> Result<String> {
    let (instance,scope,definition,version,node):(String,String,String,u32,String)=conn.query_row(
        "SELECT j.instance_id,j.scope_id,i.definition_id,i.version,j.node_id FROM bpmn_jobs j JOIN bpmn_instances i ON i.instance_id=j.instance_id WHERE j.job_id=?1",[job_id],
        |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)))?;
    let model = current_version_model_on(conn, &definition, version)?;
    match &scoped_node_on(conn, &instance, &scope, &model, &node)?.kind {
        ProcessNodeKind::ServiceTask { flow_id, .. } => Ok(flow_id.clone()),
        _ => bail!("service job references a non-service node in its exact scope"),
    }
}

fn job_activation_live_on(conn: &Connection, job_id: &str) -> Result<bool> {
    let instance_id: Option<String> = conn
        .query_row(
            "SELECT instance_id FROM bpmn_jobs WHERE job_id=?1",
            [job_id],
            |row| row.get(0),
        )
        .optional()?;
    if !instance_id
        .as_deref()
        .map(|id| call_control_live_on(conn, id))
        .transpose()?
        .unwrap_or(false)
    {
        return Ok(false);
    }
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM bpmn_jobs j JOIN bpmn_tokens t ON t.token_id=j.token_id AND t.instance_id=j.instance_id AND t.node_id=j.node_id AND t.scope_id=j.scope_id JOIN bpmn_scopes s ON s.scope_id=j.scope_id AND s.instance_id=j.instance_id JOIN bpmn_instances i ON i.instance_id=j.instance_id WHERE j.job_id=?1 AND t.status='waiting' AND (s.parent_scope_id IS NULL OR s.status NOT IN ('completed','cancelled','error')) AND i.status NOT IN ('completed','cancelled','error'))",
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
    let queued:Option<(String,String,String,String,String,String)>=tx.query_row(
        "SELECT j.job_id,j.instance_id,j.scope_id,j.node_id,i.org_id,i.initiator_user_id FROM bpmn_jobs j JOIN bpmn_instances i ON i.instance_id=j.instance_id JOIN bpmn_tokens t ON t.token_id=j.token_id AND t.instance_id=j.instance_id AND t.scope_id=j.scope_id AND t.node_id=j.node_id JOIN bpmn_scopes s ON s.instance_id=j.instance_id AND s.scope_id=j.scope_id WHERE j.status='queued' AND i.status IN ('running','waiting','incident') AND t.status='waiting' AND (s.parent_scope_id IS NULL OR s.status NOT IN ('completed','cancelled','error')) ORDER BY j.created_at_ms,j.job_id LIMIT 1",
        [],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
    ).optional()?;
    let Some((job_id, instance_id, scope_id, node_id, org_id, user_id)) = queued else {
        return Ok(None);
    };
    let actor = ProcessActor { org_id, user_id };
    let flow_id = job_flow_id_on(&tx, &job_id)?;
    if let Err(error) = require_actor(&tx, &actor)
        .and_then(|_| require_call_control_authority_on(&tx, &actor, &instance_id))
        .and_then(|_| require_flow_current(&tx, &actor, &flow_id, None))
    {
        let reason = bounded_failure_message(&error.to_string());
        tx.execute("UPDATE bpmn_jobs SET status='error',updated_at_ms=?1 WHERE job_id=?2 AND status='queued'",params![now_ms,job_id])?;
        tx.execute("UPDATE bpmn_instances SET status='incident',revision=revision+1,updated_at_ms=?1 WHERE instance_id=?2",params![now_ms,instance_id])?;
        tx.execute("UPDATE bpmn_scopes SET status='incident',revision=revision+1,updated_at_ms=?1 WHERE instance_id=?2 AND scope_id=?3 AND parent_scope_id IS NOT NULL AND status NOT IN ('completed','cancelled','error')",params![now_ms,instance_id,scope_id])?;
        let incident_id = Uuid::new_v4().to_string();
        tx.execute("INSERT INTO bpmn_incidents(incident_id,instance_id,node_id,job_id,code,message,at_ms,scope_id) VALUES(?1,?2,?3,?4,'SOURCE_ACCESS_REVOKED',?5,?6,?7)",params![incident_id,instance_id,node_id,job_id,incident_message(&reason),now_ms,scope_id])?;
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
                scope_id: scope_id.clone(),
                kind: "job_denied".into(),
                node_id: Some(node_id),
                data: serde_json::json!({"job_id":job_id,"incident_id":incident_id,"reason":reason}),
            },
            now_ms,
            None,
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
    let claimed_before_event = runtime_snapshot_on(&tx, &actor, &instance_id)?;
    let claimed_node = scope_node(
        &claimed_before_event.model,
        &claimed_before_event.scopes,
        &instance_id,
        &scope_id,
        &node_id,
    )?;
    let has_expression = matches!(
        &claimed_node.kind,
        ProcessNodeKind::ServiceTask {
            result_expression: Some(_),
            ..
        }
    );
    let body_path = scope_path(&claimed_before_event.scopes, &instance_id, &scope_id)?;
    let has_escalation = super::model::scope_body(&claimed_before_event.model, &body_path)?.0
        .iter().any(|node| matches!(&node.kind,
            ProcessNodeKind::BoundaryEscalation { attached_to_id, .. } if attached_to_id == &node_id));
    let expression_variables_sha256 = if has_expression && has_escalation {
        let effective = effective_scope_variables(
            &claimed_before_event.scopes,
            &claimed_before_event.scope_variables,
            &instance_id,
            &claimed_before_event.instance.variables,
            &scope_id,
        )?;
        Some(expression_variables_hash(&effective)?)
    } else {
        None
    };
    let next_seq: u64 = tx.query_row(
        "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
        [&instance_id],
        |row| row_u64(row, 0),
    )?;
    let mut claim_data = serde_json::json!({"job_id":job_id,"attempt":attempt,"fence":fence});
    if let Some(hash) = &expression_variables_sha256 {
        claim_data["expression_variables_sha256"] = serde_json::json!(hash);
    }
    insert_event_on(
        &tx,
        &instance_id,
        next_seq,
        Some(&actor.user_id),
        &PlannedEvent {
            scope_id: scope_id.clone(),
            kind: "service_claimed".into(),
            node_id: Some(node_id),
            data: claim_data,
        },
        now_ms,
        None,
    )?;
    let snapshot = runtime_snapshot_on(&tx, &actor, &instance_id)?;
    let job = snapshot
        .jobs
        .iter()
        .find(|job| job.job_id == job_id)
        .cloned()
        .context("claimed process job disappeared")?;
    if let Some(hash) = &expression_variables_sha256 {
        let effective = effective_scope_variables(
            &snapshot.scopes,
            &snapshot.scope_variables,
            &instance_id,
            &snapshot.instance.variables,
            &scope_id,
        )?;
        ensure!(
            expression_variables_hash(&effective)? == *hash,
            "claimed process variables changed before claim commit"
        );
    }
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
    let owner:Option<(String,String)>=tx.query_row("SELECT i.org_id,i.initiator_user_id FROM bpmn_jobs j JOIN bpmn_instances i ON i.instance_id=j.instance_id WHERE j.job_id=?1",[job_id],|row|Ok((row.get(0)?,row.get(1)?))).optional()?;
    let Some((org_id, user_id)) = owner else {
        return Ok(false);
    };
    let actor = ProcessActor { org_id, user_id };
    require_actor(&tx, &actor)?;
    let instance_id: String = tx.query_row(
        "SELECT instance_id FROM bpmn_jobs WHERE job_id=?1",
        [job_id],
        |row| row.get(0),
    )?;
    require_call_control_authority_on(&tx, &actor, &instance_id)?;
    let flow_id = job_flow_id_on(&tx, &job_id)?;
    require_flow_current(&tx, &actor, &flow_id, None)?;
    let changed=tx.execute("UPDATE bpmn_jobs SET lease_until_ms=?1,updated_at_ms=?2 WHERE job_id=?3 AND worker_id=?4 AND attempt=?5 AND fence=?6 AND status='running' AND lease_until_ms>=?2",params![now_ms+30_000,now_ms,job_id,worker_id,attempt,sql_integer(fence)?])?;
    tx.commit()?;
    Ok(changed == 1)
}

fn validate_escalation_successor_on(
    plan: &RuntimePlan,
    scope_id: &str,
    edge_id: &str,
    target: &ProcessNode,
    expected_stack: &[ForkFrame],
) -> Result<()> {
    let successors = plan.create_tokens.iter()
        .filter(|token| token.scope_id == scope_id
            && token.arrival_edge_id.as_deref() == Some(edge_id))
        .collect::<Vec<_>>();
    let ready = successors.iter().filter(|token| token.status == "ready").collect::<Vec<_>>();
    ensure!(ready.len() == 1
        && ready[0].node_id == target.id
        && ready[0].fork_stack.as_slice() == expected_stack
        && plan.consume_token_ids.iter().filter(|id| *id == &ready[0].token_id).count() == 1
        && !plan.cancel_token_ids.contains(&ready[0].token_id),
        "escalation continuation lacks one consumed exact ready successor");
    let mut waiting = 0;
    let mut joining = 0;
    for token in &successors {
        ensure!(token.node_id == target.id && token.fork_stack.as_slice() == expected_stack,
            "escalation continuation successor changed its target or fork activation");
        match token.status.as_str() {
            "ready" => ensure!(token.token_id == ready[0].token_id,
                "escalation continuation created another ready successor"),
            "waiting" => {
                waiting += 1;
                ensure!(waiting == 1
                    && !plan.consume_token_ids.contains(&token.token_id)
                    && !plan.cancel_token_ids.contains(&token.token_id),
                    "escalation continuation created an extra or closed waiting successor");
                let work = match &target.kind {
                    ProcessNodeKind::UserTask { .. } => plan.create_user_tasks.iter()
                        .filter(|task| task.scope_id == scope_id && task.node_id == target.id
                            && task.token_id.as_deref() == Some(token.token_id.as_str())
                            && task.status == ProcessUserTaskStatus::Open).count(),
                    ProcessNodeKind::ServiceTask { .. } => plan.create_jobs.iter()
                        .filter(|job| job.scope_id == scope_id && job.node_id == target.id
                            && job.token_id == token.token_id && job.status == "queued").count(),
                    ProcessNodeKind::TimerCatch { .. } => plan.create_timers.iter()
                        .filter(|timer| timer.scope_id.as_deref() == Some(scope_id)
                            && timer.node_id == target.id
                            && timer.token_id.as_deref() == Some(token.token_id.as_str())
                            && timer.kind == ProcessTimerKind::Catch
                            && matches!(&timer.status, ProcessTimerStatus::Pending | ProcessTimerStatus::Blocked)).count(),
                    ProcessNodeKind::MessageCatch { .. } => plan.create_subscriptions.iter()
                        .filter(|subscription| subscription.scope_id == scope_id
                            && subscription.node_id == target.id
                            && subscription.token_id == token.token_id
                            && subscription.kind == ProcessSubscriptionKind::MessageCatch
                            && subscription.status == ProcessSubscriptionStatus::Open).count(),
                    _ => 0,
                };
                ensure!(work == 1,
                    "escalation waiting successor lacks its exact durable work");
            }
            "joining" => {
                joining += 1;
                let frame = expected_stack.last().context("joining successor lacks a fork frame")?;
                let retained = plan.add_gateway_receipts.iter().filter(|receipt|
                    receipt.scope_id == scope_id && receipt.join_node_id == target.id
                        && receipt.activation_id == frame.activation_id
                        && receipt.branch_edge_id == frame.branch_edge_id
                        && receipt.gateway_kind == frame.gateway_kind
                        && receipt.token_id == token.token_id).count() == 1;
                let joined_kind = match frame.gateway_kind {
                    GatewayKind::Parallel => "parallel_joined",
                    GatewayKind::Inclusive => "inclusive_joined",
                };
                let completed = plan.events.iter().any(|event|
                    event.scope_id == scope_id && event.node_id.as_deref() == Some(target.id.as_str())
                        && event.kind == joined_kind
                        && event.data["activation_id"].as_str() == Some(frame.activation_id.as_str()));
                ensure!(joining == 1 && frame.join_node_id == target.id
                    && matches!(&target.kind, ProcessNodeKind::ParallelGateway | ProcessNodeKind::InclusiveGateway { .. })
                    && !plan.cancel_token_ids.contains(&token.token_id)
                    && ((retained && !plan.consume_token_ids.contains(&token.token_id))
                        || (completed && plan.consume_token_ids.iter().filter(|id| *id == &token.token_id).count() == 1)),
                    "escalation joining successor lacks its exact gateway arrival");
            }
            _ => bail!("escalation continuation created an invalid successor status"),
        }
    }
    let durable_target = matches!(&target.kind,
        ProcessNodeKind::UserTask { .. } | ProcessNodeKind::ServiceTask { .. }
            | ProcessNodeKind::TimerCatch { .. } | ProcessNodeKind::MessageCatch { .. });
    let join_target = expected_stack.last()
        .is_some_and(|frame| frame.join_node_id == target.id);
    ensure!(waiting == usize::from(durable_target) && joining == usize::from(join_target),
        "escalation continuation did not create its exact durable wait or join arrival");
    Ok(())
}

fn validate_escalation_choices_on(
    tx: &Transaction<'_>,
    instance: &ProcessInstance,
    model: &ProcessModel,
    scopes: &[ProcessScopeSummary],
    source_scope: &str,
    mapped_effective: &Value,
    plan: &RuntimePlan,
) -> Result<()> {
    let path = scope_path(scopes, &instance.instance_id, source_scope)?;
    let (nodes, flows, _) = super::model::scope_body(model, &path)?;
    let pairs = super::model::gateway_pairs(nodes, flows)?;
    let choices = plan
        .events
        .iter()
        .filter(|event| {
            matches!(
                event.kind.as_str(),
                "exclusive_selected" | "inclusive_split"
            )
        })
        .collect::<Vec<_>>();
    for event in &choices {
        ensure!(
            event.scope_id == source_scope,
            "escalation continuation selected a gateway outside its source body"
        );
        let source_id = event.data["source_token_id"]
            .as_str()
            .context("escalation gateway choice lacks its source token")?;
        let stored: Option<(String,String,String,String,Option<String>)> = tx.query_row(
            "SELECT scope_id,node_id,status,fork_stack_json,arrival_edge_id FROM bpmn_tokens WHERE instance_id=?1 AND token_id=?2",
            params![instance.instance_id,source_id],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
        ).optional()?;
        let source = if let Some((scope_id, node_id, status, stack, arrival)) = stored {
            ProcessToken {
                token_id: source_id.to_owned(),
                scope_id,
                node_id,
                status,
                fork_stack: parse(stack)?,
                arrival_edge_id: arrival,
            }
        } else {
            plan.create_tokens
                .iter()
                .find(|token| token.token_id == source_id)
                .cloned()
                .context("escalation choice source token is not planned or persisted")?
        };
        ensure!(
            source.scope_id == source_scope
                && source.status == "ready"
                && plan
                    .consume_token_ids
                    .iter()
                    .filter(|id| id.as_str() == source_id)
                    .count()
                    == 1
                && choices
                    .iter()
                    .filter(|other| other.data["source_token_id"].as_str() == Some(source_id))
                    .count()
                    == 1,
            "escalation choice is not linked to one consumed ready token"
        );
        let node = nodes
            .iter()
            .find(|node| node.id == source.node_id)
            .context("escalation choice source node is absent from its pinned body")?;
        let outgoing = flows
            .iter()
            .filter(|flow| flow.source_id == node.id)
            .collect::<Vec<_>>();
        match (&node.kind, event.kind.as_str()) {
            (ProcessNodeKind::ExclusiveGateway { default_flow_id }, "exclusive_selected") => {
                ensure!(
                    event.node_id.as_deref() == Some(node.id.as_str())
                        && event.data["activation_id"].as_str() == Some(source_id),
                    "exclusive choice activation differs from consumed token"
                );
                let mut matches = Vec::new();
                for flow in &outgoing {
                    if default_flow_id.as_ref() == Some(&flow.id) {
                        continue;
                    }
                    let selected = flow.condition.as_ref().map_or(Ok(true), |expression| {
                        super::runtime::evaluate(expression, mapped_effective, &Value::Null, &[])?
                            .as_bool()
                            .context("exclusive condition is not boolean")
                    })?;
                    if selected {
                        matches.push(flow.id.as_str());
                    }
                }
                let edge = match matches.as_slice() {
                    [only] => *only,
                    [] => default_flow_id
                        .as_deref()
                        .context("exclusive choice has no matching flow")?,
                    _ => bail!("exclusive choice is ambiguous under mapped variables"),
                };
                ensure!(
                    event.data["sequence_flow_id"].as_str() == Some(edge),
                    "exclusive choice differs from pinned CEL/default evaluation"
                );
                let target = outgoing
                    .iter()
                    .find(|flow| flow.id == edge)
                    .context("exclusive selected flow is missing")?
                    .target_id
                    .as_str();
                let target = nodes.iter().find(|node| node.id == target)
                    .context("exclusive selected target is absent")?;
                validate_escalation_successor_on(plan, source_scope, edge, target, &source.fork_stack)?;
                ensure!(
                    plan.create_tokens
                        .iter()
                        .filter(|token| token.scope_id == source_scope
                            && outgoing
                                .iter()
                                .any(|flow| token.arrival_edge_id.as_deref()
                                    == Some(flow.id.as_str())))
                        .all(|token| token.arrival_edge_id.as_deref() == Some(edge)),
                    "exclusive choice created an unselected successor"
                );
            }
            (ProcessNodeKind::InclusiveGateway { default_flow_id }, "inclusive_split") => {
                let pair = pairs
                    .get(&node.id)
                    .context("inclusive choice is not a paired split")?;
                let activation = event.data["activation_id"]
                    .as_str()
                    .context("inclusive choice activation is missing")?;
                ensure!(
                    event.node_id.as_deref() == Some(node.id.as_str())
                        && Uuid::parse_str(activation).is_ok(),
                    "inclusive choice has invalid split activation"
                );
                let mut selected = Vec::new();
                for edge in pair.branch_to_incoming_edge.keys() {
                    if default_flow_id.as_ref() == Some(edge) {
                        continue;
                    }
                    let flow = outgoing
                        .iter()
                        .find(|flow| &flow.id == edge)
                        .context("inclusive candidate flow is missing")?;
                    let expression = flow
                        .condition
                        .as_deref()
                        .context("inclusive candidate condition is missing")?;
                    let value =
                        super::runtime::evaluate(expression, mapped_effective, &Value::Null, &[])?;
                    if value
                        .as_bool()
                        .context("inclusive condition is not boolean")?
                    {
                        selected.push(edge.clone());
                    }
                }
                let default_selected = selected.is_empty() && default_flow_id.is_some();
                if default_selected {
                    selected.push(
                        default_flow_id
                            .as_ref()
                            .context("inclusive default disappeared")?
                            .clone(),
                    );
                }
                ensure!(
                    !selected.is_empty()
                        && event.data["selected_branch_edge_ids"]
                            == serde_json::to_value(&selected)?
                        && event.data["default_selected"].as_bool() == Some(default_selected),
                    "inclusive choice differs from pinned CEL/default evaluation"
                );
                let successors = plan
                    .create_tokens
                    .iter()
                    .filter(|token| {
                        token.scope_id == source_scope
                            && outgoing.iter().any(|flow| {
                                token.arrival_edge_id.as_deref() == Some(flow.id.as_str())
                            })
                    })
                    .collect::<Vec<_>>();
                ensure!(
                    successors.iter().all(|token| selected.iter().any(|edge|
                        token.arrival_edge_id.as_deref() == Some(edge.as_str()))),
                    "inclusive choice created an unselected successor"
                );
                for edge in &selected {
                    let target = outgoing
                        .iter()
                        .find(|flow| &flow.id == edge)
                        .context("inclusive selected flow is missing")?
                        .target_id
                        .as_str();
                    let target = nodes.iter().find(|node| node.id == target)
                        .context("inclusive selected target is absent")?;
                    let mut expected_stack = source.fork_stack.clone();
                    expected_stack.push(ForkFrame {
                        activation_id: activation.to_owned(),
                        split_node_id: node.id.clone(),
                        join_node_id: pair.join_node_id.clone(),
                        branch_edge_id: edge.clone(),
                        gateway_kind: GatewayKind::Inclusive,
                        selected_branch_edge_ids: selected.clone(),
                    });
                    validate_escalation_successor_on(plan, source_scope, edge, target, &expected_stack)?;
                }
            }
            _ => bail!("escalation choice kind differs from its consumed gateway token"),
        }
    }
    for token in plan.create_tokens.iter().filter(|token| {
        token.scope_id == source_scope
            && token.status == "ready"
            && plan.consume_token_ids.contains(&token.token_id)
    }) {
        let node = nodes
            .iter()
            .find(|node| node.id == token.node_id)
            .context("consumed continuation token node is absent")?;
        if matches!(node.kind, ProcessNodeKind::ExclusiveGateway { .. })
            || (matches!(node.kind, ProcessNodeKind::InclusiveGateway { .. })
                && pairs.contains_key(&node.id))
        {
            ensure!(
                choices
                    .iter()
                    .any(|event| event.data["source_token_id"].as_str()
                        == Some(token.token_id.as_str())),
                "consumed escalation gateway token lacks its choice fact"
            );
        }
    }
    for source_id in &plan.consume_token_ids {
        let stored: Option<(String,String,String)> = tx.query_row(
            "SELECT scope_id,node_id,status FROM bpmn_tokens WHERE instance_id=?1 AND token_id=?2",
            params![instance.instance_id,source_id],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).optional()?;
        let Some((scope_id, node_id, status)) = stored else {
            continue;
        };
        if scope_id != source_scope || status != "ready" {
            continue;
        }
        let node = nodes
            .iter()
            .find(|node| node.id == node_id)
            .context("consumed persisted gateway token node is absent")?;
        if matches!(node.kind, ProcessNodeKind::ExclusiveGateway { .. })
            || (matches!(node.kind, ProcessNodeKind::InclusiveGateway { .. })
                && pairs.contains_key(&node.id))
        {
            ensure!(choices.iter().any(|event|
                event.data["source_token_id"].as_str() == Some(source_id.as_str())),
                "consumed persisted escalation gateway token lacks its choice fact");
        }
    }
    Ok(())
}

fn validate_escalation_provenance_on(
    plan: &RuntimePlan,
    model: &ProcessModel,
    mapped_effective: &Value,
    accepted_result: &Value,
    scope_id: &str,
    source_token_id: &str,
    source_node_id: &str,
    boundary_id: &str,
    boundary_edge_id: &str,
    cancel_activity: bool,
    nodes: &[ProcessNode],
    flows: &[tentaflow_protocol::processes::ProcessSequenceFlow],
) -> Result<Vec<PlannedEvent>> {
    let pairs = super::model::gateway_pairs(nodes, flows)?;
    let mut traced = HashSet::new();
    let mut visited_ready = HashSet::new();
    let mut completed_joins = HashSet::new();
    let mut frontier = Vec::new();
    let mut expected_events = Vec::new();
    let trace_edge = |source: &str, edge_id: &str, stack: &[ForkFrame],
                      traced: &mut HashSet<String>, frontier: &mut Vec<ProcessToken>| -> Result<()> {
        let edge = flows.iter().find(|edge| edge.id == edge_id && edge.source_id == source)
            .context("escalation prefix left its pinned sequence flow")?;
        let target = nodes.iter().find(|node| node.id == edge.target_id)
            .context("escalation prefix target is outside its pinned body")?;
        validate_escalation_successor_on(plan, scope_id, edge_id, target, stack)?;
        for token in plan.create_tokens.iter().filter(|token|
            token.scope_id == scope_id && token.arrival_edge_id.as_deref() == Some(edge_id)) {
            ensure!(traced.insert(token.token_id.clone()),
                "escalation prefix token has two unrelated predecessors");
            if token.status == "ready" { frontier.push(token.clone()); }
        }
        Ok(())
    };
    trace_edge(boundary_id, boundary_edge_id, &[], &mut traced, &mut frontier)?;
    while let Some(token) = frontier.pop() {
        ensure!(visited_ready.insert(token.token_id.clone()),
            "escalation prefix revisited a ready activation");
        let node = nodes.iter().find(|node| node.id == token.node_id)
            .context("escalation prefix token node is outside its pinned body")?;
        let outgoing = flows.iter().filter(|edge| edge.source_id == node.id).collect::<Vec<_>>();
        match &node.kind {
            ProcessNodeKind::UserTask { .. } | ProcessNodeKind::ServiceTask { .. }
            | ProcessNodeKind::TimerCatch { .. } | ProcessNodeKind::MessageCatch { .. } => {}
            ProcessNodeKind::ExclusiveGateway { .. } => {
                let selected = plan.events.iter().filter(|event|
                    event.kind == "exclusive_selected" && event.scope_id == scope_id
                        && event.node_id.as_deref() == Some(node.id.as_str())
                        && event.data["source_token_id"].as_str() == Some(token.token_id.as_str()))
                    .collect::<Vec<_>>();
                ensure!(selected.len() == 1, "escalation XOR lacks its traced choice fact");
                let edge = selected[0].data["sequence_flow_id"].as_str()
                    .context("escalation XOR choice edge is missing")?;
                expected_events.push(PlannedEvent { scope_id: scope_id.to_owned(),
                    kind: "exclusive_selected".into(), node_id: Some(node.id.clone()),
                    data: serde_json::json!({"sequence_flow_id":edge,
                        "source_token_id":token.token_id,"activation_id":token.token_id}) });
                trace_edge(&node.id, edge, &token.fork_stack, &mut traced, &mut frontier)?;
            }
            ProcessNodeKind::InclusiveGateway { .. } if pairs.contains_key(&node.id) => {
                let pair = &pairs[&node.id];
                let selected = plan.events.iter().filter(|event|
                    event.kind == "inclusive_split" && event.scope_id == scope_id
                        && event.node_id.as_deref() == Some(node.id.as_str())
                        && event.data["source_token_id"].as_str() == Some(token.token_id.as_str()))
                    .collect::<Vec<_>>();
                ensure!(selected.len() == 1, "escalation OR lacks its traced split fact");
                let activation = selected[0].data["activation_id"].as_str()
                    .context("escalation OR activation is missing")?;
                let branches = selected[0].data["selected_branch_edge_ids"].as_array()
                    .context("escalation OR selected branches are missing")?
                    .iter().map(|edge| edge.as_str().map(str::to_owned)
                        .context("escalation OR branch ID is invalid"))
                    .collect::<Result<Vec<_>>>()?;
                expected_events.push(PlannedEvent { scope_id: scope_id.to_owned(),
                    kind: "inclusive_split".into(), node_id: Some(node.id.clone()),
                    data: serde_json::json!({"activation_id":activation,
                        "selected_branch_edge_ids":branches,
                        "default_selected":selected[0].data["default_selected"],
                        "source_token_id":token.token_id}) });
                for edge in &branches {
                    let mut stack = token.fork_stack.clone();
                    stack.push(ForkFrame { activation_id: activation.to_owned(),
                        split_node_id: node.id.clone(), join_node_id: pair.join_node_id.clone(),
                        branch_edge_id: edge.clone(), gateway_kind: GatewayKind::Inclusive,
                        selected_branch_edge_ids: branches.clone() });
                    trace_edge(&node.id, edge, &stack, &mut traced, &mut frontier)?;
                }
            }
            ProcessNodeKind::ParallelGateway if pairs.contains_key(&node.id) => {
                let pair = &pairs[&node.id];
                let split = plan.events.iter().filter(|event|
                    event.kind == "parallel_split" && event.scope_id == scope_id
                        && event.node_id.as_deref() == Some(node.id.as_str()))
                    .collect::<Vec<_>>();
                ensure!(split.len() == 1, "escalation AND lacks its one traced split fact");
                let activation = split[0].data["activation_id"].as_str()
                    .context("escalation AND activation is missing")?;
                let branches = pair.branch_to_incoming_edge.keys().cloned().collect::<Vec<_>>();
                expected_events.push(PlannedEvent { scope_id: scope_id.to_owned(),
                    kind: "parallel_split".into(), node_id: Some(node.id.clone()),
                    data: serde_json::json!({"activation_id":activation}) });
                for edge in &branches {
                    let mut stack = token.fork_stack.clone();
                    stack.push(ForkFrame { activation_id: activation.to_owned(),
                        split_node_id: node.id.clone(), join_node_id: pair.join_node_id.clone(),
                        branch_edge_id: edge.clone(), gateway_kind: GatewayKind::Parallel,
                        selected_branch_edge_ids: branches.clone() });
                    trace_edge(&node.id, edge, &stack, &mut traced, &mut frontier)?;
                }
            }
            ProcessNodeKind::ParallelGateway | ProcessNodeKind::InclusiveGateway { .. } => {
                let frame = token.fork_stack.last()
                    .context("escalation join lacks its traced fork frame")?;
                ensure!(frame.join_node_id == node.id,
                    "escalation join is outside its traced activation");
                let kind = match frame.gateway_kind {
                    GatewayKind::Parallel => "parallel_joined",
                    GatewayKind::Inclusive => "inclusive_joined",
                };
                let joined = plan.events.iter().filter(|event| event.kind == kind
                    && event.scope_id == scope_id
                    && event.node_id.as_deref() == Some(node.id.as_str())
                    && event.data["activation_id"].as_str() == Some(frame.activation_id.as_str()))
                    .collect::<Vec<_>>();
                ensure!(joined.len() <= 1, "escalation join has duplicate completion facts");
                if !joined.is_empty() && completed_joins.insert((node.id.clone(), frame.activation_id.clone())) {
                    let data = match frame.gateway_kind {
                        GatewayKind::Parallel => serde_json::json!({"activation_id":frame.activation_id}),
                        GatewayKind::Inclusive => serde_json::json!({"activation_id":frame.activation_id,
                            "selected_branch_edge_ids":frame.selected_branch_edge_ids}),
                    };
                    expected_events.push(PlannedEvent { scope_id: scope_id.to_owned(),
                        kind: kind.into(), node_id: Some(node.id.clone()), data });
                    ensure!(outgoing.len() == 1, "escalation join lacks its unique continuation");
                    let outer = &token.fork_stack[..token.fork_stack.len() - 1];
                    trace_edge(&node.id, &outgoing[0].id, outer, &mut traced, &mut frontier)?;
                }
            }
            ProcessNodeKind::MessageThrow { .. } => {
                let messages = plan.create_messages.iter().filter(|message|
                    message.source_scope_id == scope_id && message.source_node_id == node.id
                        && message.source_activation_id == token.token_id).collect::<Vec<_>>();
                ensure!(messages.len() == 1,
                    "escalation MessageThrow lacks its traced queued message");
                let mut expected = super::messages::prepare_throw(model, node, mapped_effective)?;
                expected.message_id = messages[0].message.message_id.clone();
                ensure!(serde_json::to_value(expected)? == serde_json::to_value(&messages[0].message)?,
                    "escalation queued message differs from its pinned expression inputs");
                let message = &messages[0].message;
                expected_events.push(PlannedEvent { scope_id: scope_id.to_owned(),
                    kind: "message_queued".into(), node_id: Some(node.id.clone()),
                    data: serde_json::json!({"message_id":message.message_id,
                        "source_activation_id":token.token_id,"target":message.target,
                        "message_name":message.message_name,"correlation_key":message.correlation_key}) });
                for edge in outgoing {
                    trace_edge(&node.id, &edge.id, &token.fork_stack, &mut traced, &mut frontier)?;
                }
            }
            ProcessNodeKind::EventBasedGateway => {
                let races = plan.create_event_races.iter().filter(|race|
                    race.scope_id == scope_id && race.gateway_node_id == node.id
                        && race.activation_id == token.token_id).collect::<Vec<_>>();
                ensure!(races.len() == 1, "escalation race lacks its traced activation");
                expected_events.push(PlannedEvent { scope_id: scope_id.to_owned(),
                    kind: "event_race_armed".into(), node_id: Some(node.id.clone()),
                    data: serde_json::json!({"race_id":races[0].race_id,
                        "activation_id":token.token_id}) });
                for edge in outgoing {
                    let branches = plan.create_tokens.iter().filter(|candidate|
                        candidate.scope_id == scope_id
                            && candidate.arrival_edge_id.as_deref() == Some(edge.id.as_str()))
                        .collect::<Vec<_>>();
                    ensure!(branches.len() == 1 && branches[0].status == "waiting"
                        && branches[0].node_id == edge.target_id
                        && branches[0].fork_stack == token.fork_stack,
                        "escalation race created an unrelated branch token");
                    let waiting = branches[0];
                    let subscription = plan.create_subscriptions.iter().filter(|sub|
                        sub.scope_id == scope_id && sub.node_id == waiting.node_id
                            && sub.token_id == waiting.token_id
                            && sub.status == ProcessSubscriptionStatus::Open
                            && sub.race_id.as_deref() == Some(races[0].race_id.as_str())).count();
                    let timer = plan.create_timers.iter().filter(|timer|
                        timer.scope_id.as_deref() == Some(scope_id)
                            && timer.node_id == waiting.node_id
                            && timer.token_id.as_deref() == Some(waiting.token_id.as_str())
                            && timer.status == ProcessTimerStatus::Pending
                            && timer.race_id.as_deref() == Some(races[0].race_id.as_str())).count();
                    ensure!(subscription + timer == 1 && traced.insert(waiting.token_id.clone()),
                        "escalation race branch lacks its exact one-shot wait");
                }
            }
            _ => bail!("escalation prefix reached a node without a permitted durable wait"),
        }
    }
    ensure!(plan.create_tokens.len() == traced.len()
        && plan.create_tokens.iter().all(|token| token.scope_id == scope_id
            && traced.contains(&token.token_id)),
        "escalation plan created a token outside its traced same-body prefix");
    ensure!(plan.consume_token_ids.len() == plan.consume_token_ids.iter()
        .collect::<HashSet<_>>().len()
        && plan.consume_token_ids.iter().all(|id| traced.contains(id)
            && plan.create_tokens.iter().any(|token| token.token_id.as_str() == id.as_str()
                && matches!(token.status.as_str(), "ready" | "joining"))),
        "escalation plan consumed an untraced or persisted token");
    ensure!(if cancel_activity { plan.cancel_token_ids == [source_token_id.to_owned()] }
        else { plan.cancel_token_ids.is_empty() },
        "escalation plan cancelled a token outside its source activity");
    let waiting = plan.create_tokens.iter().filter(|token|
        traced.contains(&token.token_id) && token.status == "waiting")
        .map(|token| token.token_id.as_str()).collect::<HashSet<_>>();
    let verification = plan.create_user_tasks.iter().filter(|task|
        task.kind == ProcessUserTaskKind::Verification).collect::<Vec<_>>();
    ensure!(verification.len() == usize::from(!cancel_activity)
        && verification.iter().all(|task| task.scope_id == scope_id
            && task.node_id == source_node_id
            && task.token_id.as_deref() == Some(source_token_id)
            && task.status == ProcessUserTaskStatus::Open
            && &task.outputs == accepted_result),
        "escalation source Verification differs from its accepted job");
    ensure!(plan.create_user_tasks.iter().filter(|task|
        task.kind != ProcessUserTaskKind::Verification).all(|task|
        task.kind == ProcessUserTaskKind::Work && task.scope_id == scope_id
            && task.token_id.as_deref().is_some_and(|id| waiting.contains(id)
                && plan.create_tokens.iter().any(|token| token.token_id == id
                    && token.node_id == task.node_id && token.status == "waiting"))
            && nodes.iter().any(|node| node.id == task.node_id
                && matches!(&node.kind, ProcessNodeKind::UserTask { .. }))),
        "escalation plan created work outside its traced waits");
    ensure!(plan.create_user_tasks.iter().filter(|task|
        task.kind == ProcessUserTaskKind::Work).all(|task| task.outputs.is_null()),
        "escalation continuation opened work with fabricated outputs");
    for task in &plan.create_user_tasks {
        expected_events.push(PlannedEvent { scope_id: scope_id.to_owned(),
            kind: "user_task_opened".into(), node_id: Some(task.node_id.clone()),
            data: serde_json::json!({"user_task_id":task.user_task_id,
                "assignee_user_id":task.assignee_user_id,"kind":task.kind}) });
    }
    ensure!(plan.create_jobs.iter().all(|job| job.scope_id == scope_id
        && waiting.contains(job.token_id.as_str())
        && plan.create_tokens.iter().any(|token| token.token_id == job.token_id
            && token.node_id == job.node_id && token.status == "waiting")
        && nodes.iter().any(|node| node.id == job.node_id
            && matches!(&node.kind, ProcessNodeKind::ServiceTask { .. }))),
        "escalation plan queued a job outside its traced waits");
    for job in &plan.create_jobs {
        let node = nodes.iter().find(|node| node.id == job.node_id)
            .context("escalation queued service is outside its pinned body")?;
        let ProcessNodeKind::ServiceTask { input_mapping, .. } = &node.kind else {
            bail!("escalation queued job node is not a pinned service task");
        };
        ensure!(job.input == super::runtime::prepare_service_input(input_mapping, mapped_effective)?,
            "escalation queued service input differs from its pinned mapping");
        expected_events.push(PlannedEvent { scope_id: scope_id.to_owned(),
            kind: "service_queued".into(), node_id: Some(job.node_id.clone()),
            data: serde_json::json!({"job_id":job.job_id}) });
    }
    ensure!(plan.create_timers.iter().all(|timer|
        timer.scope_id.as_deref() == Some(scope_id)
            && timer.token_id.as_deref().is_some_and(|id| waiting.contains(id)
                && plan.create_tokens.iter().any(|token| token.token_id == id
                    && match &timer.kind {
                        ProcessTimerKind::Catch => token.node_id == timer.node_id,
                        ProcessTimerKind::Boundary => nodes.iter().any(|node| node.id == timer.node_id
                            && matches!(&node.kind, ProcessNodeKind::BoundaryTimer { attached_to_id, .. }
                                if attached_to_id == &token.node_id)),
                        ProcessTimerKind::Start => false,
                    }))),
        "escalation plan armed a timer outside its traced waits");
    for timer in &plan.create_timers {
        let node = nodes.iter().find(|node| node.id == timer.node_id)
            .context("escalation timer node is outside its pinned body")?;
        let rule = match &node.kind {
            ProcessNodeKind::TimerCatch { timer } => timer,
            ProcessNodeKind::BoundaryTimer { timer, .. } => timer,
            _ => bail!("escalation timer node has no pinned timer rule"),
        };
        let due = super::timers::resolve_timer_due(rule, &timer.timezone,
            timer.kind.clone(), timer.anchor_at_ms, model.calendar_pin.as_ref())?;
        ensure!(timer.rule == *rule && timer.status == ProcessTimerStatus::Pending
            && timer.due_at_ms == Some(due) && timer.next_check_at_ms == due
            && timer.occurrence == 1 && timer.revision == 1
            && timer.last_reason.is_none(),
            "escalation timer differs from its pinned due slot");
        let attached_to_id = match &node.kind {
            ProcessNodeKind::BoundaryTimer { attached_to_id, .. } => Some(attached_to_id.as_str()),
            _ => None,
        };
        let mut data = serde_json::json!({"kind":timer.kind,"timer_id":timer.timer_id,
            "attached_to_id":attached_to_id,
            "attached_token_id":if timer.kind == ProcessTimerKind::Boundary {
                timer.token_id.as_deref() } else { None },
            "due_at_ms":timer.due_at_ms,"timezone":timer.timezone,"occurrence":1});
        if let Some(working_time) = super::calendar::working_time_summary(
            rule, model.calendar_pin.as_ref(), timer.due_at_ms)? {
            data["working_time"] = serde_json::to_value(working_time)?;
            data["timezone"] = serde_json::json!(timer.timezone);
        }
        expected_events.push(PlannedEvent { scope_id: scope_id.to_owned(),
            kind: "timer_armed".into(), node_id: Some(timer.node_id.clone()), data });
    }
    ensure!(plan.create_subscriptions.iter().all(|subscription|
        subscription.scope_id == scope_id && waiting.contains(subscription.token_id.as_str())
            && plan.create_tokens.iter().any(|token| token.token_id == subscription.token_id
                && nodes.iter().any(|node| node.id == subscription.node_id
                    && match (&subscription.kind, &node.kind) {
                        (ProcessSubscriptionKind::MessageCatch, ProcessNodeKind::MessageCatch { .. }) =>
                            token.node_id == node.id,
                        (ProcessSubscriptionKind::BoundaryMessage,
                            ProcessNodeKind::BoundaryMessage { attached_to_id, .. })
                        | (ProcessSubscriptionKind::BoundaryError,
                            ProcessNodeKind::BoundaryError { attached_to_id, .. })
                        | (ProcessSubscriptionKind::BoundaryEscalation,
                            ProcessNodeKind::BoundaryEscalation { attached_to_id, .. }) =>
                            attached_to_id == &token.node_id,
                        _ => false,
                    }))),
        "escalation plan armed a subscription outside its traced waits");
    for subscription in &plan.create_subscriptions {
        ensure!(subscription.status == ProcessSubscriptionStatus::Open
            && subscription.last_reason.is_none() && subscription.revision == 1,
            "escalation continuation created a failed subscription wait");
        let node = nodes.iter().find(|node| node.id == subscription.node_id)
            .context("escalation subscription node is outside its pinned body")?;
        let correlation = match &node.kind {
            ProcessNodeKind::MessageCatch { correlation_expression, .. }
            | ProcessNodeKind::BoundaryMessage { correlation_expression, .. } =>
                Some(correlation_expression.as_str()),
            _ => None,
        };
        if let Some(expression) = correlation {
            let expected_key = super::messages::evaluate_key(expression, mapped_effective)?;
            ensure!(subscription.correlation_key.as_deref() == Some(expected_key.as_str()),
                "escalation subscription correlation differs from its pinned expression");
        }
        let attached_to_id = match &node.kind {
            ProcessNodeKind::BoundaryMessage { attached_to_id, .. }
            | ProcessNodeKind::BoundaryError { attached_to_id, .. }
            | ProcessNodeKind::BoundaryEscalation { attached_to_id, .. } => Some(attached_to_id.as_str()),
            _ => None,
        };
        let (kind, data) = match &node.kind {
            ProcessNodeKind::BoundaryEscalation { cancel_activity, .. } => (
                "escalation_boundary_armed",
                serde_json::json!({"subscription_id":subscription.subscription_id,
                    "attached_token_id":subscription.token_id,"attached_to_id":attached_to_id,
                    "escalation_code":subscription.escalation_code,"cancel_activity":cancel_activity})),
            _ => (if subscription.kind == ProcessSubscriptionKind::BoundaryError {
                "error_boundary_armed" } else { "message_armed" },
                serde_json::json!({"subscription_id":subscription.subscription_id,
                    "token_id":subscription.token_id,"attached_to_id":attached_to_id,
                    "message_name":subscription.message_name,
                    "correlation_key":subscription.correlation_key,
                    "error_code":subscription.error_code,"race_id":subscription.race_id,
                    "kind":subscription.kind})),
        };
        expected_events.push(PlannedEvent { scope_id: scope_id.to_owned(),
            kind: kind.into(), node_id: Some(subscription.node_id.clone()), data });
    }
    for token in plan.create_tokens.iter().filter(|token|
        token.scope_id == scope_id && token.status == "waiting"
            && nodes.iter().any(|node| node.id == token.node_id
                && matches!(&node.kind, ProcessNodeKind::UserTask { .. } | ProcessNodeKind::ServiceTask { .. }))) {
        for boundary in nodes.iter().filter(|node| match &node.kind {
            ProcessNodeKind::BoundaryTimer { attached_to_id, .. }
            | ProcessNodeKind::BoundaryMessage { attached_to_id, .. }
            | ProcessNodeKind::BoundaryError { attached_to_id, .. }
            | ProcessNodeKind::BoundaryEscalation { attached_to_id, .. } => attached_to_id == &token.node_id,
            _ => false,
        }) {
            let armed = match &boundary.kind {
                ProcessNodeKind::BoundaryTimer { .. } => plan.create_timers.iter().filter(|timer|
                    timer.node_id == boundary.id && timer.token_id.as_deref() == Some(token.token_id.as_str())
                        && timer.kind == ProcessTimerKind::Boundary
                        && matches!(&timer.status, ProcessTimerStatus::Pending | ProcessTimerStatus::Blocked)).count(),
                _ => plan.create_subscriptions.iter().filter(|subscription|
                    subscription.node_id == boundary.id && subscription.token_id == token.token_id
                        && subscription.status == ProcessSubscriptionStatus::Open).count(),
            };
            ensure!(armed == 1, "escalation traced wait did not arm its exact boundary");
        }
    }
    ensure!(plan.create_event_races.iter().all(|race|
        race.scope_id == scope_id && visited_ready.contains(&race.activation_id)),
        "escalation plan armed a race outside its traced prefix");
    ensure!(plan.create_messages.iter().all(|message|
        message.source_scope_id == scope_id && visited_ready.contains(&message.source_activation_id)
            && plan.create_tokens.iter().any(|token| token.token_id == message.source_activation_id
                && token.node_id == message.source_node_id
                && nodes.iter().any(|node| node.id == token.node_id
                    && matches!(&node.kind, ProcessNodeKind::MessageThrow { .. })))),
        "escalation plan queued a message outside its traced prefix");
    ensure!(plan.add_gateway_receipts.iter().all(|receipt|
        traced.contains(&receipt.token_id)
            && plan.create_tokens.iter().any(|token| token.token_id == receipt.token_id
                && token.status == "joining"))
        && plan.remove_gateway_receipts.is_empty(),
        "escalation plan changed a receipt outside its traced gateway");
    Ok(expected_events)
}

fn validate_escalation_result_plan_on(
    tx: &Transaction<'_>,
    instance: &ProcessInstance,
    model: &ProcessModel,
    scopes: &[ProcessScopeSummary],
    source_scope: &str,
    source_token_id: &str,
    source_node_id: &str,
    job_id: &str,
    observed: &ObservedActivityResult,
    plan: &RuntimePlan,
    has_escalation: bool,
) -> Result<()> {
    let caught = plan
        .events
        .iter()
        .filter(|event| event.kind == "escalation_caught")
        .collect::<Vec<_>>();
    let selected = if has_escalation
        && observed.origin == ActivityResultOrigin::Contract
        && observed.result.outcome == tentaflow_protocol::processes::ActivityOutcome::NeedsHuman
    {
        let open = subscriptions_on(tx, &instance.instance_id)?
            .into_iter()
            .filter(|sub| {
                sub.kind == ProcessSubscriptionKind::BoundaryEscalation
                    && sub.status == ProcessSubscriptionStatus::Open
                    && sub.scope_id == source_scope
                    && sub.token_id == source_token_id
            })
            .collect::<Vec<_>>();
        open.iter()
            .find(|sub| {
                sub.escalation_code.is_some() && sub.escalation_code == observed.result.code
            })
            .or_else(|| open.iter().find(|sub| sub.escalation_code.is_none()))
            .cloned()
    } else {
        None
    };
    let Some(subscription) = selected else {
        ensure!(
            caught.is_empty(),
            "unselected escalation handler cannot catch a result"
        );
        for update in plan
            .subscription_updates
            .iter()
            .filter(|update| update.status == ProcessSubscriptionStatus::Consumed)
        {
            ensure!(
                subscription_on(tx, &update.subscription_id)?.kind
                    != ProcessSubscriptionKind::BoundaryEscalation,
                "unselected escalation handler cannot consume a result"
            );
        }
        return Ok(());
    };
    let handler = scope_node(
        model,
        scopes,
        &instance.instance_id,
        source_scope,
        &subscription.node_id,
    )?;
    let ProcessNodeKind::BoundaryEscalation {
        output_mapping,
        cancel_activity,
        ..
    } = &handler.kind
    else {
        bail!("selected escalation subscription refers to a different node kind");
    };
    let result_index = plan
        .events
        .iter()
        .position(|event| event.kind == "service_result")
        .context("accepted escalation has no factual result event")?;
    ensure!(result_index == 0, "accepted service result must precede its continuation");
    let result_event_id = plan.event_ids.get(&result_index);
    if caught.is_empty() {
        ensure!(
            plan.complete_job_ids == [job_id.to_owned()]
                && plan.cancel_job_ids.is_empty()
                && plan.consume_token_ids.is_empty()
                && plan.cancel_token_ids.is_empty()
                && plan.create_tokens.is_empty()
                && plan.add_gateway_receipts.is_empty()
                && plan.remove_gateway_receipts.is_empty()
                && plan.resolve_incident_ids.is_empty()
                && plan.create_user_tasks.is_empty()
                && plan.complete_user_task_ids.is_empty()
                && plan.cancel_user_task_ids.is_empty()
                && plan.create_jobs.is_empty()
                && plan.create_timers.is_empty()
                && plan.timer_updates.is_empty()
                && plan.create_subscriptions.is_empty()
                && plan.subscription_updates.is_empty()
                && plan.create_event_races.is_empty()
                && plan.race_updates.is_empty()
                && plan.create_messages.is_empty()
                && plan.create_scopes.is_empty()
                && plan.cancel_scope_roots.is_empty()
                && plan.call_requests.is_empty()
                && plan.call_steps.is_empty()
                && plan.scope_terminal_errors.is_empty()
                && plan.terminal_error.is_none()
                && plan.variables == instance.variables
                && plan.status == ProcessInstanceStatus::Incident,
            "retained escalation failure changed boundary or source work"
        );
        ensure!(
            plan.scope_updates
                .iter()
                .all(|update| update.scope_id == source_scope
                    && update.variables.is_none()
                    && update.status == ProcessInstanceStatus::Incident),
            "retained escalation failure changed another scope"
        );
        ensure!(
            plan.add_incidents.len() == 1
                && plan.add_incidents[0].scope_id == source_scope
                && plan.add_incidents[0].node_id.as_deref() == Some(source_node_id)
                && plan.add_incidents[0].job_id.as_deref() == Some(job_id)
                && plan.add_incidents[0].code == "ESCALATION_HANDLER_FAILED"
                && !plan.add_incidents[0].can_retry
                && plan.event_ids.is_empty()
                && plan.events.len() == 2
                && plan.events[result_index].kind == "service_result"
                && plan.events.iter().any(|event| event.kind == "incident"
                    && event.scope_id == source_scope
                    && event.node_id.as_deref() == Some(source_node_id)
                    && event.data == serde_json::json!({"code":"ESCALATION_HANDLER_FAILED",
                        "message":plan.add_incidents[0].message})),
            "retained escalation failure lacks its factual source incident"
        );
        return Ok(());
    }
    ensure!(
        caught.len() == 1 && result_event_id.is_some(),
        "escalation catch requires one factual result event ID"
    );
    ensure!(plan.event_ids.len() == 1,
        "escalation catch assigned an unrelated event identity");
    let event = caught[0];
    let expected = serde_json::json!({
        "subscription_id": subscription.subscription_id,
        "boundary_id": subscription.node_id,
        "attached_token_id": source_token_id,
        "source_token_id": source_token_id,
        "source_scope_id": source_scope,
        "job_id": job_id,
        "attempt": tx.query_row("SELECT attempt FROM bpmn_jobs WHERE job_id=?1", [job_id], |row| row.get::<_,u32>(0))?,
        "fence": tx.query_row("SELECT fence FROM bpmn_jobs WHERE job_id=?1", [job_id], |row| row_u64(row,0))?,
        "result_origin": "contract",
        "cancel_activity": cancel_activity,
        "result_event_id": result_event_id,
        "code": observed.result.code,
        "matched_escalation_code": subscription.escalation_code,
    });
    ensure!(
        event.scope_id == source_scope
            && event.node_id.as_deref() == Some(subscription.node_id.as_str())
            && event.data == expected,
        "escalation catch differs from the accepted result and selected handler"
    );
    ensure!(
        plan.complete_job_ids == [job_id.to_owned()]
            && !plan.cancel_job_ids.iter().any(|id| id == job_id)
            && plan.complete_user_task_ids.is_empty()
            && plan.add_incidents.is_empty()
            && plan.terminal_error.is_none()
            && !reaches_error_end(plan),
        "caught escalation cannot cancel its accepted job or introduce a failure incident"
    );
    let consumed = plan
        .subscription_updates
        .iter()
        .filter(|update| update.status == ProcessSubscriptionStatus::Consumed)
        .collect::<Vec<_>>();
    ensure!(
        consumed.len() == 1
            && consumed[0].subscription_id == subscription.subscription_id
            && consumed[0].expected_revision == subscription.revision,
        "escalation must consume only its selected one-shot handler"
    );
    validate_boundary_plan_on(
        tx,
        &BoundaryActivationId::Subscription(subscription.subscription_id.clone()),
        Some(job_id),
        plan,
    )?;
    let path = scope_path(scopes, &instance.instance_id, source_scope)?;
    let (body_nodes, body_flows, _) = super::model::scope_body(model, &path)?;
    let boundary_flows = body_flows
        .iter()
        .filter(|flow| flow.source_id == subscription.node_id)
        .collect::<Vec<_>>();
    ensure!(boundary_flows.len() == 1,
        "escalation catch does not start its exact boundary continuation"
    );
    let target = body_nodes.iter()
        .find(|node| node.id == boundary_flows[0].target_id)
        .context("escalation boundary target is absent from its source body")?;
    validate_escalation_successor_on(plan, source_scope, &boundary_flows[0].id, target, &[])?;
    let source_local = scope_variables_on(tx, &instance.instance_id, source_scope)?;
    let mut locals = BTreeMap::new();
    for scope in scopes
        .iter()
        .filter(|scope| scope.scope_id != instance.instance_id)
    {
        if !matches!(
            scope.status,
            ProcessInstanceStatus::Completed
                | ProcessInstanceStatus::Cancelled
                | ProcessInstanceStatus::Error
        ) {
            locals.insert(
                scope.scope_id.clone(),
                scope_variables_on(tx, &instance.instance_id, &scope.scope_id)?,
            );
        }
    }
    let effective = effective_scope_variables(
        scopes,
        &locals,
        &instance.instance_id,
        &instance.variables,
        source_scope,
    )?;
    let canonical_source = super::runtime::patch_variables(
        output_mapping,
        &source_local,
        &effective,
        &observed.result.outputs,
        &[(
            "activity_result".into(),
            serde_json::to_value(&observed.result)?,
        )],
    )?;
    let canonical_root = if source_scope == instance.instance_id {
        ensure!(
            plan.variables == canonical_source
                && plan
                    .scope_updates
                    .iter()
                    .all(|update| update.variables.is_none()),
            "escalation plan changed variables outside its root mapping"
        );
        canonical_source.clone()
    } else {
        ensure!(
            plan.variables == instance.variables
                && plan
                    .scope_updates
                    .iter()
                    .filter(|update| update.variables.is_some())
                    .count()
                    == 1
                && plan
                    .scope_updates
                    .iter()
                    .any(|update| update.scope_id == source_scope
                        && update.variables.as_ref() == Some(&canonical_source)),
            "escalation plan changed variables outside its source scope mapping"
        );
        instance.variables.clone()
    };
    let mut incident_stmt = tx.prepare("SELECT incident_id,scope_id FROM bpmn_incidents WHERE instance_id=?1 AND resolved_at_ms IS NULL")?;
    let remaining_incidents = incident_stmt
        .query_map([&instance.instance_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|(id, _)| !plan.resolve_incident_ids.contains(id))
        .collect::<Vec<_>>();
    let mut ready_stmt = tx.prepare("SELECT scope_id,token_id FROM bpmn_tokens WHERE instance_id=?1 AND status='ready'")?;
    let ready = ready_stmt.query_map([&instance.instance_id], |row| {
        Ok((row.get::<_,String>(0)?, row.get::<_,String>(1)?))
    })?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut job_stmt = tx.prepare("SELECT scope_id,job_id FROM bpmn_jobs WHERE instance_id=?1 AND status IN ('queued','running')")?;
    let active_jobs = job_stmt.query_map([&instance.instance_id], |row| {
        Ok((row.get::<_,String>(0)?, row.get::<_,String>(1)?))
    })?.collect::<rusqlite::Result<Vec<_>>>()?;
    let expected_status = |scope: Option<&str>| {
        if remaining_incidents.iter().any(|(_, incident_scope)|
            scope.is_none() || scope == Some(incident_scope.as_str())) {
            ProcessInstanceStatus::Incident
        } else if ready.iter().any(|(token_scope, id)|
            (scope.is_none() || scope == Some(token_scope.as_str()))
                && !plan.consume_token_ids.contains(id) && !plan.cancel_token_ids.contains(id))
            || plan.create_tokens.iter().any(|token|
                (scope.is_none() || scope == Some(token.scope_id.as_str()))
                    && token.status == "ready" && !plan.consume_token_ids.contains(&token.token_id)
                    && !plan.cancel_token_ids.contains(&token.token_id))
            || active_jobs.iter().any(|(job_scope, id)|
                (scope.is_none() || scope == Some(job_scope.as_str()))
                    && !plan.complete_job_ids.contains(id) && !plan.cancel_job_ids.contains(id))
            || plan.create_jobs.iter().any(|job|
                scope.is_none() || scope == Some(job.scope_id.as_str())) {
            ProcessInstanceStatus::Running
        } else {
            ProcessInstanceStatus::Waiting
        }
    };
    ensure!(
        plan.create_scopes.is_empty()
            && plan.cancel_scope_roots.is_empty()
            && plan.call_requests.is_empty()
            && plan.call_steps.is_empty()
            && plan.scope_terminal_errors.is_empty()
            && plan.status == expected_status(None)
            && plan
                .scope_updates
                .iter()
                .all(|update| update.scope_id == source_scope
                    && update.status == expected_status(Some(source_scope))),
        "escalation continuation changed another scope or call activation"
    );
    if source_scope != instance.instance_id && !plan.scope_updates.iter().any(|update|
        update.scope_id == source_scope) {
        ensure!(scopes.iter().any(|scope| scope.scope_id == source_scope
            && scope.status == expected_status(Some(source_scope))),
            "escalation source scope status differs from its factual active work");
    }
    let mapped_effective = if source_scope == instance.instance_id {
        canonical_root.clone()
    } else {
        locals.insert(source_scope.to_owned(), canonical_source);
        effective_scope_variables(
            scopes,
            &locals,
            &instance.instance_id,
            &canonical_root,
            source_scope,
        )?
    };
    validate_escalation_choices_on(
        tx,
        instance,
        model,
        scopes,
        source_scope,
        &mapped_effective,
        plan,
    )?;
    let mut expected_events = validate_escalation_provenance_on(plan, model, &mapped_effective,
        &serde_json::to_value(&observed.result)?,
        source_scope, source_token_id, source_node_id,
        &subscription.node_id, &boundary_flows[0].id, *cancel_activity,
        body_nodes, body_flows)?;
    expected_events.push(plan.events[result_index].clone());
    expected_events.push(event.clone());
    for update in plan.subscription_updates.iter()
        .filter(|update| update.status == ProcessSubscriptionStatus::Cancelled) {
        let actual = subscription_on(tx, &update.subscription_id)?;
        ensure!(*cancel_activity && actual.scope_id == source_scope
            && actual.token_id == source_token_id
            && update.last_reason.as_deref() == Some("sibling_interrupted"),
            "escalation cancelled a subscription outside its source activity");
        expected_events.push(PlannedEvent { scope_id: source_scope.to_owned(),
            kind: "subscription_cancelled".into(), node_id: Some(actual.node_id),
            data: serde_json::json!({"subscription_id":actual.subscription_id,
                "attached_token_id":actual.token_id,"reason":update.last_reason}) });
    }
    for update in plan.timer_updates.iter()
        .filter(|update| update.status == ProcessTimerStatus::Cancelled) {
        let actual = timer_on(tx, &update.timer_id)?;
        ensure!(*cancel_activity && actual.scope_id.as_deref() == Some(source_scope)
            && actual.token_id.as_deref() == Some(source_token_id)
            && update.last_reason.as_deref() == Some("sibling_interrupted"),
            "escalation cancelled a timer outside its source activity");
        let node = body_nodes.iter().find(|node| node.id == actual.node_id)
            .context("cancelled escalation timer is outside its pinned body")?;
        let ProcessNodeKind::BoundaryTimer { attached_to_id, .. } = &node.kind else {
            bail!("cancelled escalation timer is not a boundary timer");
        };
        expected_events.push(PlannedEvent { scope_id: source_scope.to_owned(),
            kind: "timer_cancelled".into(), node_id: Some(actual.node_id),
            data: serde_json::json!({"kind":"Boundary","timer_id":actual.timer_id,
                "attached_to_id":attached_to_id,"attached_token_id":actual.token_id,
                "reason":update.last_reason,"winning_timer_id":subscription.subscription_id}) });
    }
    let mut expected_history = expected_events.iter().map(json)
        .collect::<Result<Vec<_>>>()?;
    let mut planned_history = plan.events.iter().map(json)
        .collect::<Result<Vec<_>>>()?;
    expected_history.sort();
    planned_history.sort();
    ensure!(planned_history == expected_history,
        "escalation history differs from its factual selected continuation");
    ensure!(
        plan.create_tokens
            .iter()
            .any(|token| token.scope_id == source_scope
                && token.status == "waiting"
                && !plan.consume_token_ids.contains(&token.token_id)
                && !plan.cancel_token_ids.contains(&token.token_id)),
        "escalation continuation did not reach a durable wait"
    );
    Ok(())
}

fn validate_job_result_plan_on(
    tx: &Transaction<'_>,
    instance: &ProcessInstance,
    node_id: &str,
    job_id: &str,
    observed: &ObservedActivityResult,
    plan: &RuntimePlan,
) -> Result<()> {
    let model = current_version_model_on(tx, &instance.definition_id, instance.version)?;
    let (source_scope, token_id): (String, String) = tx.query_row(
        "SELECT scope_id,token_id FROM bpmn_jobs WHERE job_id=?1 AND instance_id=?2",
        params![job_id, instance.instance_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let scopes = scopes_on(tx, &instance.instance_id, &model)?;
    let node = scope_node(
        &model,
        &scopes,
        &instance.instance_id,
        &source_scope,
        node_id,
    )?;
    let ProcessNodeKind::ServiceTask {
        result_expression, ..
    } = &node.kind
    else {
        bail!("result node is not a service activity")
    };
    if observed.origin == ActivityResultOrigin::Contract {
        ensure!(
            result_expression.is_some(),
            "business result requires an explicit pinned output contract"
        );
        super::jobs::parse_contract_result(serde_json::to_value(&observed.result)?)?;
    }
    let body_path = scope_path(&scopes, &instance.instance_id, &source_scope)?;
    let has_escalation = super::model::scope_body(&model, &body_path)?
        .0
        .iter()
        .any(|candidate| {
            matches!(&candidate.kind,
            ProcessNodeKind::BoundaryEscalation { attached_to_id, .. } if attached_to_id == node_id)
        });
    if has_escalation && observed.origin == ActivityResultOrigin::Contract {
        let observation = observed
            .expression_observation
            .as_ref()
            .context("accepted escalation result lacks its original expression observation")?;
        validate_output(&observation.normalized_outputs)?;
        validate_variables(&observation.evaluation_variables)?;
        let claim_data: String = tx.query_row(
            "SELECT data_json FROM bpmn_events WHERE instance_id=?1 AND scope_id=?2 AND kind='service_claimed' AND json_extract(data_json,'$.job_id')=?3 AND json_extract(data_json,'$.attempt')=?4 AND json_extract(data_json,'$.fence')=?5 ORDER BY seq DESC LIMIT 1",
            params![instance.instance_id,source_scope,job_id,
                tx.query_row("SELECT attempt FROM bpmn_jobs WHERE job_id=?1",[job_id],|row|row.get::<_,u32>(0))?,
                sql_integer(tx.query_row("SELECT fence FROM bpmn_jobs WHERE job_id=?1",[job_id],|row|row_u64(row,0))?)?],
            |row| row.get(0),
        ).context("fenced service claim event is missing")?;
        let claim: Value = parse(claim_data)?;
        ensure!(
            claim["expression_variables_sha256"].as_str()
                == Some(expression_variables_hash(&observation.evaluation_variables)?.as_str()),
            "accepted escalation expression variables differ from the fenced claim"
        );
        let expression = result_expression
            .as_deref()
            .context("accepted escalation has no pinned result expression")?;
        let actual = super::jobs::parse_contract_result(super::runtime::evaluate(
            expression,
            &observation.evaluation_variables,
            &observation.normalized_outputs,
            &[],
        )?)?;
        ensure!(
            actual == observed.result,
            "accepted escalation result differs from its pinned expression observation"
        );
    }
    let mut expected = serde_json::to_value(&observed.result)?;
    expected["result_origin"] = serde_json::json!(result_origin_text(&observed.origin));
    ensure!(
        plan.events
            .iter()
            .filter(|event| event.kind == "service_result")
            .count()
            == 1
            && plan
                .events
                .iter()
                .any(|event| event.kind == "service_result"
                    && event.scope_id == source_scope
                    && event.node_id.as_deref() == Some(node_id)
                    && event.data == expected),
        "service result plan does not retain its actual result and origin"
    );
    let open = subscriptions_on(tx, &instance.instance_id)?
        .into_iter()
        .filter(|sub| {
            sub.kind == ProcessSubscriptionKind::BoundaryError
                && sub.status == ProcessSubscriptionStatus::Open
        })
        .collect::<Vec<_>>();
    let mut attached_token = token_id.clone();
    let mut scope_id = source_scope.clone();
    let mut hops = Vec::new();
    let mut handler = None;
    if observed.origin == ActivityResultOrigin::Contract
        && observed.result.outcome == tentaflow_protocol::processes::ActivityOutcome::Error
    {
        loop {
            let local = open
                .iter()
                .filter(|sub| sub.scope_id == scope_id && sub.token_id == attached_token)
                .collect::<Vec<_>>();
            if let Some(sub) = local
                .iter()
                .find(|sub| sub.error_code.is_some() && sub.error_code == observed.result.code)
                .or_else(|| local.iter().find(|sub| sub.error_code.is_none()))
            {
                handler = Some(*sub);
                break;
            }
            let scope = scopes
                .iter()
                .find(|scope| scope.scope_id == scope_id)
                .context("error source scope is absent")?;
            let Some(parent) = &scope.parent_scope_id else {
                break;
            };
            hops.push((scope_id.clone(), parent.clone()));
            attached_token = scope
                .parent_token_id
                .clone()
                .context("enclosing subprocess wait is absent")?;
            scope_id = parent.clone();
            let live:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM bpmn_tokens WHERE instance_id=?1 AND scope_id=?2 AND token_id=?3 AND status='waiting')",params![instance.instance_id,scope_id,attached_token],|row|row.get(0))?;
            ensure!(live, "error propagation lost an actual enclosing wait");
        }
    }
    ensure!(
        !reaches_error_end(plan)
            || observed.result.outcome == tentaflow_protocol::processes::ActivityOutcome::Completed
            || handler.is_some(),
        "only a completed activity or handled Contract Error can reach downstream ErrorEnd"
    );
    let caught = plan
        .events
        .iter()
        .filter(|event| {
            event.kind == "business_error_caught"
                && event.data["source_kind"].as_str() != Some("error_end")
        })
        .collect::<Vec<_>>();
    let handling_escalation = plan
        .events
        .iter()
        .any(|event| event.kind == "escalation_caught");
    match handler {
        Some(sub) => {
            ensure!(
                caught.len() == 1
                    && caught[0].scope_id == sub.scope_id
                    && caught[0].node_id.as_deref() == Some(sub.node_id.as_str())
                    && caught[0].data["subscription_id"].as_str()
                        == Some(sub.subscription_id.as_str())
                    && caught[0].data["attached_token_id"].as_str()
                        == Some(attached_token.as_str())
                    && caught[0].data["job_id"].as_str() == Some(job_id)
                    && caught[0].data["error_code"] == serde_json::to_value(&observed.result.code)?
                    && caught[0].data["result_origin"].as_str() == Some("contract"),
                "business error handler does not match its accepted contract result"
            );
            let propagation = plan
                .events
                .iter()
                .filter(|event| event.kind == "scope_error_propagated")
                .collect::<Vec<_>>();
            ensure!(
                propagation.len() == hops.len(),
                "business error propagation omits an actual enclosing scope"
            );
            let result_index = plan
                .events
                .iter()
                .position(|event| event.kind == "service_result")
                .context("accepted result event missing")?;
            for (event, (from, to)) in propagation.iter().zip(&hops) {
                ensure!(event.scope_id==sub.scope_id && event.node_id.as_deref()==Some(sub.node_id.as_str())
                    && event.data["source_scope_id"].as_str()==Some(source_scope.as_str())
                    && event.data["source_job_id"].as_str()==Some(job_id)
                    && event.data["source_token_id"].as_str()==Some(token_id.as_str())
                    && event.data["from_scope_id"].as_str()==Some(from.as_str())
                    && event.data["to_scope_id"].as_str()==Some(to.as_str())
                    && event.data["handler_node_id"].as_str()==Some(sub.node_id.as_str())
                    && event.data["code"]==serde_json::to_value(&observed.result.code)?
                    && event.data["source_result_index"].as_u64()==Some(u64::try_from(result_index)?),
                    "business error propagation differs from accepted result and actual scope ancestry");
            }
            validate_boundary_plan_on(
                tx,
                &BoundaryActivationId::Subscription(sub.subscription_id.clone()),
                Some(job_id),
                plan,
            )?;
        }
        None => {
            ensure!(
                (reaches_error_end(plan)
                    || handling_escalation
                    || (plan.cancel_scope_roots.is_empty()
                        && plan.cancel_token_ids.is_empty()
                        && plan.cancel_job_ids.is_empty()
                        && plan.cancel_user_task_ids.is_empty()
                        && !plan
                            .events
                            .iter()
                            .any(|event| event.kind == "scope_error_propagated"))),
                "uncaught or platform result cannot interrupt another scope"
            );
            ensure!(
                (reaches_error_end(plan)
                    || handling_escalation
                    || (caught.is_empty()
                        && !plan.subscription_updates.iter().any(|update| update.status
                            == ProcessSubscriptionStatus::Consumed
                            && open
                                .iter()
                                .any(|sub| sub.subscription_id == update.subscription_id)))),
                "nonbusiness error cannot consume an error handler"
            );
        }
    }
    validate_escalation_result_plan_on(
        tx,
        instance,
        &model,
        &scopes,
        &source_scope,
        &token_id,
        node_id,
        job_id,
        observed,
        plan,
        has_escalation,
    )?;
    Ok(())
}

pub fn accept_job_result(
    pool: &DbPool,
    actor: &ProcessActor,
    job_id: &str,
    attempt: u32,
    fence: u64,
    worker_id: &str,
    observed: &ObservedActivityResult,
    expected_revision: u64,
    plan: &RuntimePlan,
    at_ms: i64,
) -> Result<ProcessTransitionOutcome> {
    let result = &observed.result;
    validate_output(&result.outputs)?;
    ensure!(
        json(result)?.len() <= 384 * 1024 - 4096,
        "activity result exceeds the process history budget"
    );
    let source_id: String = {
        let conn = pool.read()?;
        conn.query_row(
            "SELECT instance_id FROM bpmn_jobs WHERE job_id=?1",
            [job_id],
            |row| row.get(0),
        )?
    };
    let (composite, mut conn) = prepare_call_plan(pool, actor, &source_id, None, plan, at_ms)?;
    let plan = &composite;
    let tx = conn.transaction()?;
    require_actor(&tx, actor)?;
    let (instance_id,node_id,status,current_attempt,current_fence,current_worker,lease_until_ms,stored_result):(String,String,String,u32,u64,Option<String>,Option<i64>,Option<String>)=tx.query_row("SELECT instance_id,node_id,status,attempt,fence,worker_id,lease_until_ms,result_json FROM bpmn_jobs WHERE job_id=?1",[job_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row_u64(row,4)?,row.get(5)?,row.get(6)?,row.get(7)?))).context("process job not found")?;
    let instance = instance_on(&tx, actor, &instance_id, None)?;
    ensure!(
        instance.initiator_user_id == actor.user_id,
        "process job actor is not the initiator"
    );
    if status == "completed"
        || status == "error"
        || (status == "cancelled" && stored_result.is_some())
    {
        ensure!(
            current_attempt == attempt
                && current_fence == fence
                && stored_result.as_deref() == Some(json(result)?.as_str())
                && tx
                    .query_row(
                        "SELECT result_origin FROM bpmn_jobs WHERE job_id=?1",
                        [job_id],
                        |row| row.get::<_, Option<String>>(0)
                    )?
                    .as_deref()
                    == Some(result_origin_text(&observed.origin)),
            "service job result conflicts with accepted result"
        );
        return Ok(ProcessTransitionOutcome {
            instance,
            cancelled_claims: Vec::new(),
        });
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
    require_call_control_authority_on(&tx, actor, &instance_id)?;
    let flow_id = job_flow_id_on(&tx, job_id)?;
    require_flow_current(&tx, actor, &flow_id, None)?;
    ensure!(
        plan.complete_job_ids.iter().any(|id| id == job_id),
        "result plan does not consume service job"
    );
    validate_job_result_plan_on(&tx, &instance, &node_id, job_id, observed, plan)?;
    tx.execute(
        "UPDATE bpmn_jobs SET result_json=?1,result_origin=?2 WHERE job_id=?3",
        params![json(result)?, result_origin_text(&observed.origin), job_id],
    )?;
    let cancelled_claims = apply_plan_on(
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
    tx.execute("UPDATE bpmn_jobs SET status=?1,result_json=?2,lease_until_ms=NULL,updated_at_ms=?3,result_origin=?5 WHERE job_id=?4",params![terminal,json(result)?,at_ms,job_id,result_origin_text(&observed.origin)])?;
    let result_instance = instance_on(&tx, actor, &instance_id, None)?;
    tx.commit()?;
    Ok(ProcessTransitionOutcome {
        instance: result_instance,
        cancelled_claims,
    })
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
    let instance = instance_on(&tx, actor, instance_id, None)?;
    ensure!(
        instance.initiator_user_id == actor.user_id,
        "only process initiator may retry"
    );
    if let Some(prior) = command_replay::<StoredInstanceIdentity>(&tx, actor, stamp)? {
        return reproject_instance_on(&tx, actor, &prior.instance_id);
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
    let accepted_needs_human: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM bpmn_jobs WHERE job_id=?1 AND instance_id=?2 AND status='completed' AND result_origin='contract' AND json_extract(result_json,'$.outcome')='NeedsHuman')",
        params![job_id,instance_id], |row| row.get(0),
    )?;
    ensure!(
        !accepted_needs_human,
        "accepted Contract NeedsHuman service job cannot be retried"
    );
    let (node_id,scope_id):(String,String)=tx.query_row("SELECT j.node_id,j.scope_id FROM bpmn_jobs j JOIN bpmn_incidents x ON x.job_id=j.job_id AND x.instance_id=j.instance_id AND x.resolved_at_ms IS NULL WHERE j.job_id=?1 AND j.instance_id=?2 AND j.status IN ('error','completed')",params![job_id,instance_id],|row|Ok((row.get(0)?,row.get(1)?))).context("retryable job not found")?;
    require_call_control_authority_on(&tx, actor, instance_id)?;
    let flow_id = job_flow_id_on(&tx, job_id)?;
    require_flow_current(&tx, actor, &flow_id, None)?;
    let affected=tx.execute("UPDATE bpmn_jobs SET status='queued',fence=fence+1,worker_id=NULL,lease_until_ms=NULL,result_json=NULL,result_origin=NULL,updated_at_ms=?1 WHERE job_id=?2 AND instance_id=?3 AND status IN ('error','completed')",params![at_ms,job_id,instance_id])?;
    ensure!(affected == 1, "retryable job changed");
    tx.execute("UPDATE bpmn_user_tasks SET status='cancelled',revision=revision+1,updated_at_ms=?1 WHERE instance_id=?2 AND token_id=(SELECT token_id FROM bpmn_jobs WHERE job_id=?3) AND kind='verification' AND status='open'",params![at_ms,instance_id,job_id])?;
    tx.execute("UPDATE bpmn_incidents SET resolved_at_ms=?1 WHERE instance_id=?2 AND job_id=?3 AND resolved_at_ms IS NULL",params![at_ms,instance_id,job_id])?;
    tx.execute("UPDATE bpmn_scopes SET status=CASE WHEN EXISTS(SELECT 1 FROM bpmn_incidents WHERE instance_id=?2 AND scope_id=?3 AND resolved_at_ms IS NULL) THEN 'incident' ELSE 'running' END,revision=revision+1,updated_at_ms=?1 WHERE instance_id=?2 AND scope_id=?3 AND parent_scope_id IS NOT NULL AND status NOT IN ('completed','cancelled','error')",params![at_ms,instance_id,scope_id])?;
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
            scope_id: scope_id.clone(),
            kind: "job_retried".into(),
            node_id: Some(node_id),
            data: serde_json::json!({"job_id":job_id}),
        },
        at_ms,
        None,
    )?;
    let result = instance_on(&tx, actor, instance_id, None)?;
    store_command(&tx, actor, stamp, &result, at_ms)?;
    tx.commit()?;
    Ok(result)
}

pub fn recover_jobs(pool: &DbPool, worker_id: Option<&str>, now_ms: i64) -> Result<u32> {
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    let mut stmt=tx.prepare("SELECT j.job_id,j.instance_id,j.node_id,j.scope_id FROM bpmn_jobs j WHERE j.status='running' AND (?1 IS NULL OR j.worker_id=?1) ORDER BY j.created_at_ms,j.job_id")?;
    let rows = stmt
        .query_map([worker_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(stmt);
    for (job_id, instance_id, node_id, scope_id) in &rows {
        ensure!(
            job_activation_live_on(&tx, job_id)?,
            "running service job no longer has a waiting activation"
        );
        tx.execute("UPDATE bpmn_jobs SET status='error',fence=fence+1,worker_id=NULL,lease_until_ms=NULL,updated_at_ms=?1 WHERE job_id=?2 AND status='running'",params![now_ms,job_id])?;
        tx.execute("UPDATE bpmn_instances SET status='incident',revision=revision+1,updated_at_ms=?1 WHERE instance_id=?2 AND status NOT IN ('completed','cancelled','error')",params![now_ms,instance_id])?;
        tx.execute("UPDATE bpmn_scopes SET status='incident',revision=revision+1,updated_at_ms=?1 WHERE instance_id=?2 AND scope_id=?3 AND parent_scope_id IS NOT NULL AND status NOT IN ('completed','cancelled','error')",params![now_ms,instance_id,scope_id])?;
        let incident_id = Uuid::new_v4().to_string();
        tx.execute("INSERT INTO bpmn_incidents(incident_id,instance_id,node_id,job_id,code,message,at_ms,scope_id) VALUES(?1,?2,?3,?4,'INTERRUPTED','The worker stopped before the external effect was confirmed',?5,?6)",params![incident_id,instance_id,node_id,job_id,now_ms,scope_id])?;
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
                scope_id: scope_id.clone(),
                kind: "job_interrupted".into(),
                node_id: Some(node_id.clone()),
                data: serde_json::json!({"job_id":job_id,"incident_id":incident_id}),
            },
            now_ms,
            None,
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
    observed: Option<&ObservedActivityResult>,
) -> Result<bool> {
    ensure!(
        !code.is_empty() && code.len() <= 128,
        "invalid process job failure"
    );
    let message = bounded_failure_message(message);
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    let row:Option<(String,String,String)>=tx.query_row("SELECT instance_id,node_id,scope_id FROM bpmn_jobs WHERE job_id=?1 AND status='running' AND attempt=?2 AND fence=?3 AND worker_id=?4",params![job_id,attempt,sql_integer(fence)?,worker_id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).optional()?;
    let Some((instance_id, node_id, scope_id)) = row else {
        return Ok(false);
    };
    if !job_activation_live_on(&tx, job_id)? {
        return Ok(false);
    }
    if let Some(observed) = observed {
        validate_output(&observed.result.outputs)?;
        ensure!(
            json(&observed.result)?.len() <= 384 * 1024 - 4096,
            "observed result exceeds history budget"
        );
        if observed.origin == ActivityResultOrigin::Contract {
            super::jobs::parse_contract_result(serde_json::to_value(&observed.result)?)?;
            let (definition, version): (String, u32) = tx.query_row(
                "SELECT definition_id,version FROM bpmn_instances WHERE instance_id=?1",
                [&instance_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            let model = current_version_model_on(&tx, &definition, version)?;
            ensure!(
                matches!(
                    &scoped_node_on(&tx, &instance_id, &scope_id, &model, &node_id)?.kind,
                    ProcessNodeKind::ServiceTask {
                        result_expression: Some(_),
                        ..
                    }
                ),
                "contract provenance has no pinned result expression"
            );
        }
        tx.execute(
            "UPDATE bpmn_jobs SET result_json=?1,result_origin=?2 WHERE job_id=?3",
            params![
                json(&observed.result)?,
                result_origin_text(&observed.origin),
                job_id
            ],
        )?;
        let seq: u64 = tx.query_row(
            "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
            [&instance_id],
            |row| row_u64(row, 0),
        )?;
        let mut data = serde_json::to_value(&observed.result)?;
        data["result_origin"] = serde_json::json!(result_origin_text(&observed.origin));
        insert_event_on(
            &tx,
            &instance_id,
            seq,
            None,
            &PlannedEvent {
                scope_id: scope_id.clone(),
                kind: "service_result".into(),
                node_id: Some(node_id.clone()),
                data,
            },
            now_ms,
            None,
        )?;
    }
    let changed=tx.execute("UPDATE bpmn_jobs SET status='error',fence=fence+1,worker_id=NULL,lease_until_ms=NULL,updated_at_ms=?1 WHERE job_id=?2 AND status='running' AND attempt=?3 AND fence=?4 AND worker_id=?5",params![now_ms,job_id,attempt,sql_integer(fence)?,worker_id])?;
    if changed == 0 {
        return Ok(false);
    }
    let active=tx.execute("UPDATE bpmn_instances SET status='incident',revision=revision+1,updated_at_ms=?1 WHERE instance_id=?2 AND status NOT IN ('completed','cancelled','error')",params![now_ms,instance_id])?;
    if active == 1 {
        tx.execute("UPDATE bpmn_scopes SET status='incident',revision=revision+1,updated_at_ms=?1 WHERE instance_id=?2 AND scope_id=?3 AND parent_scope_id IS NOT NULL AND status NOT IN ('completed','cancelled','error')",params![now_ms,instance_id,scope_id])?;
        let incident_id = Uuid::new_v4().to_string();
        tx.execute("INSERT INTO bpmn_incidents(incident_id,instance_id,node_id,job_id,code,message,at_ms,scope_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![incident_id,instance_id,node_id,job_id,code,incident_message(&message),now_ms,scope_id])?;
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
                scope_id: scope_id.clone(),
                kind: "job_failed".into(),
                node_id: Some(node_id),
                data: serde_json::json!({"job_id":job_id,"incident_id":incident_id,"code":code,"message":message}),
            },
            now_ms,
            None,
        )?;
    }
    tx.commit()?;
    Ok(true)
}

fn subscription_kind_text(kind: &ProcessSubscriptionKind) -> &'static str {
    match kind {
        ProcessSubscriptionKind::MessageCatch => "message_catch",
        ProcessSubscriptionKind::BoundaryMessage => "boundary_message",
        ProcessSubscriptionKind::BoundaryError => "boundary_error",
        ProcessSubscriptionKind::BoundaryEscalation => "boundary_escalation",
    }
}
fn subscription_kind(value: &str) -> Result<ProcessSubscriptionKind> {
    match value {
        "message_catch" => Ok(ProcessSubscriptionKind::MessageCatch),
        "boundary_message" => Ok(ProcessSubscriptionKind::BoundaryMessage),
        "boundary_error" => Ok(ProcessSubscriptionKind::BoundaryError),
        "boundary_escalation" => Ok(ProcessSubscriptionKind::BoundaryEscalation),
        _ => bail!("unknown subscription kind"),
    }
}
fn subscription_status_text(status: &ProcessSubscriptionStatus) -> &'static str {
    match status {
        ProcessSubscriptionStatus::Open => "open",
        ProcessSubscriptionStatus::Consumed => "consumed",
        ProcessSubscriptionStatus::Cancelled => "cancelled",
        ProcessSubscriptionStatus::Error => "error",
    }
}
fn subscription_status(value: &str) -> Result<ProcessSubscriptionStatus> {
    match value {
        "open" => Ok(ProcessSubscriptionStatus::Open),
        "consumed" => Ok(ProcessSubscriptionStatus::Consumed),
        "cancelled" => Ok(ProcessSubscriptionStatus::Cancelled),
        "error" => Ok(ProcessSubscriptionStatus::Error),
        _ => bail!("unknown subscription status"),
    }
}
fn race_status_text(status: &ProcessEventRaceStatus) -> &'static str {
    match status {
        ProcessEventRaceStatus::Open => "open",
        ProcessEventRaceStatus::Won => "won",
        ProcessEventRaceStatus::Cancelled => "cancelled",
    }
}
fn race_status(value: &str) -> Result<ProcessEventRaceStatus> {
    match value {
        "open" => Ok(ProcessEventRaceStatus::Open),
        "won" => Ok(ProcessEventRaceStatus::Won),
        "cancelled" => Ok(ProcessEventRaceStatus::Cancelled),
        _ => bail!("unknown event race status"),
    }
}
fn subscription_on(conn: &Connection, id: &str) -> Result<EventSubscription> {
    let row = conn.query_row("SELECT instance_id,org_id,definition_id,version,node_id,token_id,kind,message_name,correlation_key,error_code,escalation_code,race_id,revision,status,last_reason,created_at_ms,updated_at_ms FROM bpmn_event_subscriptions WHERE subscription_id=?1",[id],|r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,u32>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,Option<String>>(7)?,r.get::<_,Option<String>>(8)?,r.get::<_,Option<String>>(9)?,r.get::<_,Option<String>>(10)?,r.get::<_,Option<String>>(11)?,row_u64(r,12)?,r.get::<_,String>(13)?,r.get::<_,Option<String>>(14)?,r.get::<_,i64>(15)?,r.get::<_,i64>(16)?))).context("subscription not found")?;
    Ok(EventSubscription {
        scope_id: conn.query_row(
            "SELECT scope_id FROM bpmn_event_subscriptions WHERE subscription_id=?1",
            [id],
            |row| row.get(0),
        )?,
        subscription_id: id.to_owned(),
        instance_id: row.0,
        org_id: row.1,
        definition_id: row.2,
        version: row.3,
        node_id: row.4,
        token_id: row.5,
        kind: subscription_kind(&row.6)?,
        message_name: row.7,
        correlation_key: row.8,
        error_code: row.9,
        escalation_code: row.10,
        race_id: row.11,
        revision: row.12,
        status: subscription_status(&row.13)?,
        last_reason: row.14,
        created_at_ms: row.15,
        updated_at_ms: row.16,
    })
}
fn subscriptions_on(conn: &Connection, instance_id: &str) -> Result<Vec<EventSubscription>> {
    let mut q=conn.prepare("SELECT subscription_id FROM bpmn_event_subscriptions WHERE instance_id=?1 ORDER BY created_at_ms,subscription_id")?;
    let ids = q
        .query_map([instance_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ids.iter().map(|id| subscription_on(conn, id)).collect()
}
fn race_on(conn: &Connection, id: &str) -> Result<EventRace> {
    let r=conn.query_row("SELECT instance_id,gateway_node_id,activation_id,revision,status,winner_node_id,winner_subscription_id,winner_timer_id,won_at_ms,created_at_ms,updated_at_ms FROM bpmn_event_races WHERE race_id=?1",[id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,row_u64(r,3)?,r.get::<_,String>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,Option<String>>(6)?,r.get::<_,Option<String>>(7)?,r.get::<_,Option<i64>>(8)?,r.get::<_,i64>(9)?,r.get::<_,i64>(10)?))).context("event race not found")?;
    Ok(EventRace {
        scope_id: conn.query_row(
            "SELECT scope_id FROM bpmn_event_races WHERE race_id=?1",
            [id],
            |row| row.get(0),
        )?,
        race_id: id.to_owned(),
        instance_id: r.0,
        gateway_node_id: r.1,
        activation_id: r.2,
        revision: r.3,
        status: race_status(&r.4)?,
        winner_node_id: r.5,
        winner_subscription_id: r.6,
        winner_timer_id: r.7,
        won_at_ms: r.8,
        created_at_ms: r.9,
        updated_at_ms: r.10,
    })
}
fn races_on(conn: &Connection, instance_id: &str) -> Result<Vec<EventRace>> {
    let mut q = conn.prepare(
        "SELECT race_id FROM bpmn_event_races WHERE instance_id=?1 ORDER BY created_at_ms,race_id",
    )?;
    let ids = q
        .query_map([instance_id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ids.iter().map(|id| race_on(conn, id)).collect()
}
fn subscription_activation<'a>(
    conn: &Connection,
    model: &'a ProcessModel,
    s: &EventSubscription,
) -> Result<&'a str> {
    let node = scoped_node_on(conn, &s.instance_id, &s.scope_id, model, &s.node_id)?;
    let (attachment, message_ref, error_ref, escalation_ref) = match (&s.kind, &node.kind) {
        (
            ProcessSubscriptionKind::MessageCatch,
            ProcessNodeKind::MessageCatch { message_ref, .. },
        ) => (node.id.as_str(), Some(message_ref), None, None),
        (
            ProcessSubscriptionKind::BoundaryMessage,
            ProcessNodeKind::BoundaryMessage {
                attached_to_id,
                message_ref,
                ..
            },
        ) => (attached_to_id.as_str(), Some(message_ref), None, None),
        (
            ProcessSubscriptionKind::BoundaryError,
            ProcessNodeKind::BoundaryError {
                attached_to_id,
                error_ref,
                ..
            },
        ) => (attached_to_id.as_str(), None, Some(error_ref), None),
        (
            ProcessSubscriptionKind::BoundaryEscalation,
            ProcessNodeKind::BoundaryEscalation {
                attached_to_id,
                escalation_ref,
                ..
            },
        ) => (attached_to_id.as_str(), None, None, Some(escalation_ref)),
        _ => bail!("subscription kind differs from pinned node"),
    };
    if let Some(id) = message_ref {
        let declaration = model
            .messages
            .iter()
            .find(|m| &m.message_id == id)
            .context("subscription message declaration is missing")?;
        ensure!(
            s.message_name.as_deref() == Some(declaration.name.as_str()),
            "subscription name differs from pinned declaration"
        );
        if s.status != ProcessSubscriptionStatus::Error {
            super::messages::validate_key(
                s.correlation_key
                    .as_deref()
                    .context("subscription correlation is missing")?,
            )?;
        }
    }
    if let Some(id) = error_ref {
        let expected = id
            .as_ref()
            .map(|id| {
                model
                    .errors
                    .iter()
                    .find(|e| &e.error_id == id)
                    .map(|e| e.error_code.as_str())
                    .context("error declaration is missing")
            })
            .transpose()?;
        ensure!(
            s.error_code.as_deref() == expected,
            "subscription error code differs from pinned declaration"
        );
    }
    if let Some(id) = escalation_ref {
        let expected = id
            .as_ref()
            .map(|id| {
                model
                    .escalations
                    .iter()
                    .find(|e| &e.escalation_id == id)
                    .map(|e| e.escalation_code.as_str())
                    .context("escalation declaration is missing")
            })
            .transpose()?;
        ensure!(
            s.escalation_code.as_deref() == expected,
            "subscription escalation code differs from pinned declaration"
        );
    } else {
        ensure!(
            s.escalation_code.is_none(),
            "non-escalation subscription has escalation code"
        );
    }
    Ok(attachment)
}
fn subscription_live_on(conn: &Connection, s: &EventSubscription) -> Result<bool> {
    if !call_control_live_on(conn, &s.instance_id)? {
        return Ok(false);
    }

    let model = current_version_model_on(conn, &s.definition_id, s.version)?;
    let node = subscription_activation(conn, &model, s)?;
    let live:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM bpmn_tokens t JOIN bpmn_instances i ON i.instance_id=t.instance_id JOIN bpmn_scopes s ON s.instance_id=t.instance_id AND s.scope_id=t.scope_id WHERE t.token_id=?1 AND t.instance_id=?2 AND t.node_id=?3 AND t.status='waiting' AND t.scope_id=?7 AND EXISTS(SELECT 1 FROM bpmn_scopes x WHERE x.instance_id=t.instance_id AND x.scope_id=t.scope_id AND (x.parent_scope_id IS NULL OR x.status NOT IN ('completed','cancelled','error'))) AND i.org_id=?4 AND i.definition_id=?5 AND i.version=?6 AND i.status NOT IN ('completed','cancelled','error'))",params![s.token_id,s.instance_id,node,s.org_id,s.definition_id,s.version,s.scope_id],|r|r.get(0))?;
    if !live {
        return Ok(false);
    }
    if let Some(id) = &s.race_id {
        let race = race_on(conn, id)?;
        return Ok(race.instance_id == s.instance_id
            && race.scope_id == s.scope_id
            && race.status == ProcessEventRaceStatus::Open);
    }
    Ok(true)
}
fn insert_subscription_on(tx: &Transaction<'_>, s: &EventSubscription) -> Result<()> {
    ensure!(
        s.revision == 1
            && matches!(
                s.status,
                ProcessSubscriptionStatus::Open | ProcessSubscriptionStatus::Error
            ),
        "new subscription state is invalid"
    );
    ensure!(
        Uuid::parse_str(&s.subscription_id).is_ok() && subscription_live_on(tx, s)?,
        "subscription is not bound to its exact waiting activation"
    );
    let open: u32 = tx.query_row(
        "SELECT COUNT(*) FROM bpmn_event_subscriptions WHERE instance_id=?1 AND status='open'",
        [&s.instance_id],
        |r| r.get(0),
    )?;
    ensure!(
        s.status != ProcessSubscriptionStatus::Open || open < 128,
        "live subscription capacity exceeded"
    );
    if let Some(id) = &s.race_id {
        let race = race_on(tx, id)?;
        let model = current_version_model_on(tx, &s.definition_id, s.version)?;
        let scopes = scopes_on(tx, &s.instance_id, &model)?;
        let path = scope_path(&scopes, &s.instance_id, &s.scope_id)?;
        let flows = super::model::scope_body(&model, &path)?.1;
        ensure!(
            s.kind == ProcessSubscriptionKind::MessageCatch
                && race.instance_id == s.instance_id
                && race.scope_id == s.scope_id
                && race.status == ProcessEventRaceStatus::Open
                && flows
                    .iter()
                    .any(|e| e.source_id == race.gateway_node_id && e.target_id == s.node_id),
            "subscription is not a branch of its event race"
        );
    }
    tx.execute("INSERT INTO bpmn_event_subscriptions(subscription_id,instance_id,org_id,definition_id,version,node_id,token_id,kind,message_name,correlation_key,error_code,escalation_code,race_id,revision,status,last_reason,created_at_ms,updated_at_ms,scope_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,1,?14,?15,?16,?17,?18)",params![s.subscription_id,s.instance_id,s.org_id,s.definition_id,s.version,s.node_id,s.token_id,subscription_kind_text(&s.kind),s.message_name,s.correlation_key,s.error_code,s.escalation_code,s.race_id,subscription_status_text(&s.status),s.last_reason,s.created_at_ms,s.updated_at_ms,s.scope_id])?;
    Ok(())
}
fn apply_event_plan_on(
    tx: &Transaction<'_>,
    instance_id: &str,
    plan: &RuntimePlan,
    at_ms: i64,
) -> Result<()> {
    let (definition_id, version, org_id): (String, u32, String) = tx.query_row(
        "SELECT definition_id,version,org_id FROM bpmn_instances WHERE instance_id=?1",
        [instance_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let model = current_version_model_on(tx, &definition_id, version)?;
    for race in &plan.create_event_races {
        let scopes = scopes_on(tx, instance_id, &model)?;
        let path = scope_path(&scopes, instance_id, &race.scope_id)?;
        let (nodes, flows, _) = super::model::scope_body(&model, &path)?;
        ensure!(
            race.instance_id == instance_id
                && race.revision == 1
                && race.status == ProcessEventRaceStatus::Open
                && race.winner_node_id.is_none()
                && race.winner_subscription_id.is_none()
                && race.winner_timer_id.is_none()
                && race.won_at_ms.is_none()
                && race.created_at_ms == at_ms
                && race.updated_at_ms == at_ms,
            "new event race is invalid"
        );
        ensure!(
            nodes.iter().any(|n| n.id == race.gateway_node_id
                && matches!(n.kind, ProcessNodeKind::EventBasedGateway))
                && plan.events.iter().any(|e| e.kind == "event_race_armed"
                    && e.scope_id == race.scope_id
                    && e.data["race_id"].as_str() == Some(race.race_id.as_str())
                    && e.data["activation_id"].as_str() == Some(race.activation_id.as_str())),
            "event race lacks its logical gateway activation"
        );
        ensure!(
            Uuid::parse_str(&race.race_id).is_ok() && Uuid::parse_str(&race.activation_id).is_ok(),
            "event race identity must be an actual UUID"
        );
        let branches = flows
            .iter()
            .filter(|edge| edge.source_id == race.gateway_node_id)
            .collect::<Vec<_>>();
        ensure!(
            (2..=8).contains(&branches.len()),
            "event race needs all of its published branches"
        );
        let mut actual_nodes = Vec::new();
        for edge in &branches {
            let node = nodes
                .iter()
                .find(|node| node.id == edge.target_id)
                .context("race branch missing")?;
            let token_id = match &node.kind {
                ProcessNodeKind::MessageCatch { .. } => {
                    let rows = plan
                        .create_subscriptions
                        .iter()
                        .filter(|sub| {
                            sub.race_id.as_deref() == Some(race.race_id.as_str())
                                && sub.scope_id == race.scope_id
                                && sub.node_id == node.id
                        })
                        .collect::<Vec<_>>();
                    ensure!(
                        rows.len() == 1,
                        "race must arm exactly one subscription for each message branch"
                    );
                    &rows[0].token_id
                }
                ProcessNodeKind::TimerCatch { .. } => {
                    let rows = plan
                        .create_timers
                        .iter()
                        .filter(|timer| {
                            timer.race_id.as_deref() == Some(race.race_id.as_str())
                                && timer.scope_id.as_deref() == Some(race.scope_id.as_str())
                                && timer.node_id == node.id
                        })
                        .collect::<Vec<_>>();
                    ensure!(
                        rows.len() == 1,
                        "race must arm exactly one timer for each timer branch"
                    );
                    rows[0]
                        .token_id
                        .as_ref()
                        .context("race timer lacks waiting UUID")?
                }
                _ => bail!("race branch is not a published one-shot catch"),
            };
            ensure!(
                plan.create_tokens
                    .iter()
                    .any(|token| &token.token_id == token_id
                        && token.scope_id == race.scope_id
                        && token.node_id == node.id
                        && token.status == "waiting"
                        && token.arrival_edge_id.as_deref() == Some(edge.id.as_str())),
                "race branch lacks its exact waiting token and edge"
            );
            actual_nodes.push(node.id.as_str());
        }
        ensure!(
            plan.create_subscriptions
                .iter()
                .filter(|sub| sub.race_id.as_deref() == Some(race.race_id.as_str()))
                .all(|sub| actual_nodes.contains(&sub.node_id.as_str()))
                && plan
                    .create_timers
                    .iter()
                    .filter(|timer| timer.race_id.as_deref() == Some(race.race_id.as_str()))
                    .all(|timer| actual_nodes.contains(&timer.node_id.as_str())),
            "race includes a foreign branch"
        );
        tx.execute("INSERT INTO bpmn_event_races(race_id,instance_id,gateway_node_id,activation_id,revision,status,created_at_ms,updated_at_ms,scope_id) VALUES(?1,?2,?3,?4,1,'open',?5,?5,?6)",params![race.race_id,instance_id,race.gateway_node_id,race.activation_id,at_ms,race.scope_id])?;
    }
    for s in &plan.create_subscriptions {
        ensure!(
            s.instance_id == instance_id
                && s.definition_id == definition_id
                && s.version == version
                && s.org_id == org_id
                && s.created_at_ms == at_ms
                && s.updated_at_ms == at_ms,
            "new subscription belongs to another transition"
        );
        if s.kind == ProcessSubscriptionKind::BoundaryEscalation {
            let node = scoped_node_on(tx, instance_id, &s.scope_id, &model, &s.node_id)?;
            let ProcessNodeKind::BoundaryEscalation {
                attached_to_id,
                cancel_activity,
                ..
            } = &node.kind
            else {
                bail!("escalation subscription references another node kind");
            };
            ensure!(
                plan.events
                    .iter()
                    .any(|event| event.kind == "escalation_boundary_armed"
                        && event.scope_id == s.scope_id
                        && event.node_id.as_deref() == Some(s.node_id.as_str())
                        && event.data
                            == serde_json::json!({
                                "subscription_id":s.subscription_id,
                                "attached_token_id":s.token_id,
                                "attached_to_id":attached_to_id,
                                "escalation_code":s.escalation_code,
                                "cancel_activity":cancel_activity,
                            })),
                "escalation subscription lacks its exact armed event"
            );
        }
        insert_subscription_on(tx, s)?;
    }
    for update in &plan.race_updates {
        let actual = race_on(tx, &update.race_id)?;
        ensure!(
            actual.instance_id == instance_id && actual.status == ProcessEventRaceStatus::Open,
            "event race already settled"
        );
        ensure!(
            matches!(
                update.status,
                ProcessEventRaceStatus::Won | ProcessEventRaceStatus::Cancelled
            ),
            "event race cannot reopen"
        );
        if update.status == ProcessEventRaceStatus::Won {
            let winner = update
                .winner_node_id
                .as_deref()
                .context("event winner node is missing")?;
            match (&update.winner_subscription_id, &update.winner_timer_id) {
                (Some(id), None) => {
                    let s = subscription_on(tx, id)?;
                    ensure!(
                        s.race_id.as_deref() == Some(actual.race_id.as_str())
                            && s.scope_id == actual.scope_id
                            && s.instance_id == instance_id
                            && s.node_id == winner
                            && s.status == ProcessSubscriptionStatus::Open,
                        "race winning subscription changed"
                    );
                }
                (None, Some(id)) => {
                    let t = timer_on(tx, id)?;
                    ensure!(
                        t.race_id.as_deref() == Some(actual.race_id.as_str())
                            && t.scope_id.as_deref() == Some(actual.scope_id.as_str())
                            && t.instance_id.as_deref() == Some(instance_id)
                            && t.node_id == winner
                            && matches!(
                                t.status,
                                ProcessTimerStatus::Pending | ProcessTimerStatus::Blocked
                            ),
                        "race winning timer changed"
                    );
                }
                _ => bail!("event race needs exactly one winning branch"),
            }
            let mut losers = subscriptions_on(tx, instance_id)?
                .into_iter()
                .filter(|sub| {
                    sub.race_id.as_deref() == Some(actual.race_id.as_str())
                        && Some(&sub.subscription_id) != update.winner_subscription_id.as_ref()
                })
                .map(|sub| sub.token_id)
                .collect::<Vec<_>>();
            losers.extend(
                timer_ids_on(tx, instance_id)?
                    .iter()
                    .map(|id| timer_on(tx, id))
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .filter(|timer| {
                        timer.race_id.as_deref() == Some(actual.race_id.as_str())
                            && Some(&timer.timer_id) != update.winner_timer_id.as_ref()
                    })
                    .map(|timer| timer.token_id.context("race timer lacks activation"))
                    .collect::<Result<Vec<_>>>()?,
            );
            ensure!(
                losers.len() == plan.cancel_token_ids.len()
                    && losers.iter().all(|id| plan.cancel_token_ids.contains(id))
                    && plan.cancel_token_ids.iter().collect::<HashSet<_>>().len() == losers.len(),
                "race cancels a foreign activation or omits a losing branch"
            );
            for s in subscriptions_on(tx, instance_id)?
                .iter()
                .filter(|s| s.race_id.as_deref() == Some(actual.race_id.as_str()))
            {
                ensure!(
                    if Some(&s.subscription_id) == update.winner_subscription_id.as_ref() {
                        plan.consume_token_ids.contains(&s.token_id)
                    } else {
                        plan.cancel_token_ids.contains(&s.token_id)
                    },
                    "race must close each exact branch activation"
                );
                if s.status != ProcessSubscriptionStatus::Open {
                    continue;
                }
                let expected = if Some(&s.subscription_id) == update.winner_subscription_id.as_ref()
                {
                    ProcessSubscriptionStatus::Consumed
                } else {
                    ProcessSubscriptionStatus::Cancelled
                };
                ensure!(
                    plan.subscription_updates
                        .iter()
                        .any(|u| u.subscription_id == s.subscription_id && u.status == expected),
                    "race must settle every subscription branch"
                );
            }
            for t in timer_ids_on(tx, instance_id)?
                .iter()
                .map(|id| timer_on(tx, id))
                .collect::<Result<Vec<_>>>()?
                .iter()
                .filter(|t| t.race_id.as_deref() == Some(actual.race_id.as_str()))
            {
                let id = t.token_id.as_ref().context("race timer lacks activation")?;
                ensure!(
                    if Some(&t.timer_id) == update.winner_timer_id.as_ref() {
                        plan.consume_token_ids.contains(id)
                    } else {
                        plan.cancel_token_ids.contains(id)
                    },
                    "race must close each exact timer activation"
                );
                if !matches!(
                    t.status,
                    ProcessTimerStatus::Pending | ProcessTimerStatus::Blocked
                ) {
                    continue;
                }
                let expected = if Some(&t.timer_id) == update.winner_timer_id.as_ref() {
                    ProcessTimerStatus::Fired
                } else {
                    ProcessTimerStatus::Cancelled
                };
                ensure!(
                    plan.timer_updates
                        .iter()
                        .any(|u| u.timer_id == t.timer_id && u.status == expected),
                    "race must settle every timer branch"
                );
            }
        }
        ensure!(
            plan.events.iter().any(|event| event.kind
                == if update.status == ProcessEventRaceStatus::Won {
                    "event_race_won"
                } else {
                    "event_race_cancelled"
                }
                && event.data["race_id"].as_str() == Some(actual.race_id.as_str())),
            "event race transition lacks its actual history"
        );
        let count=tx.execute("UPDATE bpmn_event_races SET revision=revision+1,status=?1,winner_node_id=?2,winner_subscription_id=?3,winner_timer_id=?4,won_at_ms=?5,updated_at_ms=?6 WHERE race_id=?7 AND revision=?8 AND status='open'",params![race_status_text(&update.status),update.winner_node_id,update.winner_subscription_id,update.winner_timer_id,if update.status==ProcessEventRaceStatus::Won{Some(at_ms)}else{None},at_ms,update.race_id,sql_incrementable(update.expected_revision)?])?;
        ensure!(count == 1, "event race revision conflict");
    }
    for update in &plan.subscription_updates {
        let actual = subscription_on(tx, &update.subscription_id)?;
        ensure!(
            actual.instance_id == instance_id
                && actual.status == ProcessSubscriptionStatus::Open
                && matches!(
                    update.status,
                    ProcessSubscriptionStatus::Consumed | ProcessSubscriptionStatus::Cancelled
                ),
            "subscription cannot reopen or cross instance"
        );
        if update.status == ProcessSubscriptionStatus::Consumed {
            let token_advanced = plan.consume_token_ids.contains(&actual.token_id)
                || plan.cancel_token_ids.contains(&actual.token_id);
            let noninterrupt = matches!(
                &scoped_node_on(tx, instance_id, &actual.scope_id, &model, &actual.node_id)?.kind,
                ProcessNodeKind::BoundaryMessage {
                    cancel_activity: false,
                    ..
                } | ProcessNodeKind::BoundaryEscalation {
                    cancel_activity: false,
                    ..
                }
            );
            ensure!(
                token_advanced || noninterrupt,
                "consumed subscription does not advance its activation"
            );
        }
        if update.status == ProcessSubscriptionStatus::Cancelled {
            let reason = update
                .last_reason
                .as_deref()
                .context("subscription cancellation lacks reason")?;
            ensure!(
                (reason == "activity_completed"
                    && plan.consume_token_ids.contains(&actual.token_id))
                    || (matches!(
                        reason,
                        "sibling_interrupted" | "event_race_lost" | "scope_cancelled"
                    ) && plan.cancel_token_ids.contains(&actual.token_id)),
                "subscription cancellation is unrelated to its exact activation"
            );
            if actual.kind == ProcessSubscriptionKind::BoundaryEscalation {
                ensure!(
                    plan.events
                        .iter()
                        .any(|event| event.kind == "subscription_cancelled"
                            && event.scope_id == actual.scope_id
                            && event.node_id.as_deref() == Some(actual.node_id.as_str())
                            && event.data["subscription_id"].as_str()
                                == Some(actual.subscription_id.as_str())
                            && event.data["attached_token_id"].as_str()
                                == Some(actual.token_id.as_str())
                            && event.data["reason"].as_str() == Some(reason)),
                    "escalation disarm lacks its actual subscription history"
                );
            }
        }
        let count=tx.execute("UPDATE bpmn_event_subscriptions SET status=?1,last_reason=?2,revision=revision+1,updated_at_ms=?3 WHERE subscription_id=?4 AND instance_id=?5 AND revision=?6 AND status='open'",params![subscription_status_text(&update.status),update.last_reason,at_ms,update.subscription_id,instance_id,sql_incrementable(update.expected_revision)?])?;
        ensure!(count == 1, "subscription revision conflict");
    }
    for id in plan.consume_token_ids.iter().chain(&plan.cancel_token_ids) {
        let pending:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM bpmn_event_subscriptions WHERE instance_id=?1 AND token_id=?2 AND status='open')",params![instance_id,id],|r|r.get(0))?;
        ensure!(
            !pending,
            "activity transition left a live subscription on a closed token"
        );
    }
    Ok(())
}
pub(super) fn result_origin_text(origin: &ActivityResultOrigin) -> &'static str {
    match origin {
        ActivityResultOrigin::Envelope => "envelope",
        ActivityResultOrigin::Contract => "contract",
        ActivityResultOrigin::Platform => "platform",
    }
}
fn result_origin(value: &str) -> Result<ActivityResultOrigin> {
    match value {
        "envelope" => Ok(ActivityResultOrigin::Envelope),
        "contract" => Ok(ActivityResultOrigin::Contract),
        "platform" => Ok(ActivityResultOrigin::Platform),
        _ => bail!("unknown accepted result provenance"),
    }
}
fn message_status_text(status: &ProcessMessageStatus) -> &'static str {
    match status {
        ProcessMessageStatus::Pending => "pending",
        ProcessMessageStatus::Blocked => "blocked",
        ProcessMessageStatus::Ambiguous => "ambiguous",
        ProcessMessageStatus::Delivered => "delivered",
        ProcessMessageStatus::Expired => "expired",
        ProcessMessageStatus::Cancelled => "cancelled",
        ProcessMessageStatus::Error => "error",
    }
}
fn message_status(value: &str) -> Result<ProcessMessageStatus> {
    match value {
        "pending" => Ok(ProcessMessageStatus::Pending),
        "blocked" => Ok(ProcessMessageStatus::Blocked),
        "ambiguous" => Ok(ProcessMessageStatus::Ambiguous),
        "delivered" => Ok(ProcessMessageStatus::Delivered),
        "expired" => Ok(ProcessMessageStatus::Expired),
        "cancelled" => Ok(ProcessMessageStatus::Cancelled),
        "error" => Ok(ProcessMessageStatus::Error),
        _ => bail!("unknown message status"),
    }
}
fn message_on(conn: &Connection, key: &MessageKey, load_payload: bool) -> Result<MessageRecord> {
    let r=conn.query_row("SELECT request_hash,origin,target_kind,definition_id,target_instance_id,target_subscription_id,message_name,correlation_key,CASE WHEN ?4 THEN payload_json ELSE NULL END,payload_sha256,payload_bytes,ttl_seconds,received_at_ms,expires_at_ms,revision,status,last_reason,next_check_at_ms,updated_at_ms,resolved_instance_id,resolved_subscription_id,resolved_token_id,matched_instance_id,matched_version,matched_node_id,matched_subscription_id,delivered_at_ms,source_instance_id,source_node_id,payload_json IS NOT NULL FROM bpmn_messages WHERE org_id=?1 AND sender_user_id=?2 AND message_id=?3",params![key.org_id,key.sender_user_id,key.message_id,load_payload],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?,r.get::<_,Option<String>>(8)?,r.get::<_,String>(9)?,r.get::<_,u32>(10)?,r.get::<_,u32>(11)?,r.get::<_,i64>(12)?,r.get::<_,i64>(13)?,row_u64(r,14)?,r.get::<_,String>(15)?,r.get::<_,Option<String>>(16)?,r.get::<_,i64>(17)?,r.get::<_,i64>(18)?,r.get::<_,Option<String>>(19)?,r.get::<_,Option<String>>(20)?,r.get::<_,Option<String>>(21)?,r.get::<_,Option<String>>(22)?,r.get::<_,Option<u32>>(23)?,r.get::<_,Option<String>>(24)?,r.get::<_,Option<String>>(25)?,r.get::<_,Option<i64>>(26)?,r.get::<_,Option<String>>(27)?,r.get::<_,Option<String>>(28)?,r.get::<_,bool>(29)?))).context("message not found")?;
    let target = match r.2.as_str() {
        "start" => ProcessMessageTarget::Start { definition_id: r.3 },
        "catch" => ProcessMessageTarget::Catch {
            definition_id: r.3,
            instance_id: r.4,
            subscription_id: r.5,
        },
        _ => bail!("unknown message target"),
    };
    Ok(MessageRecord {
        key: key.clone(),
        request_hash: r.0,
        origin: match r.1.as_str() {
            "api" => ProcessMessageOrigin::Api,
            "process" => ProcessMessageOrigin::Process,
            _ => bail!("unknown message origin"),
        },
        target,
        message_name: r.6,
        correlation_key: r.7,
        payload: r.8.map(parse).transpose()?,
        payload_sha256: r.9,
        payload_bytes: r.10,
        ttl_seconds: r.11,
        received_at_ms: r.12,
        expires_at_ms: r.13,
        revision: r.14,
        status: message_status(&r.15)?,
        last_reason: r.16,
        next_check_at_ms: r.17,
        updated_at_ms: r.18,
        resolved_instance_id: r.19,
        resolved_subscription_id: r.20,
        resolved_token_id: r.21,
        matched_instance_id: r.22,
        matched_version: r.23,
        matched_node_id: r.24,
        matched_subscription_id: r.25,
        delivered_at_ms: r.26,
        source_scope_id: conn.query_row("SELECT source_scope_id FROM bpmn_messages WHERE org_id=?1 AND sender_user_id=?2 AND message_id=?3", params![key.org_id,key.sender_user_id,key.message_id], |row| row.get(0))?,
        source_instance_id: r.27,
        source_node_id: r.28,
        payload_available: r.29,
    })
}
fn require_message_target_on(
    conn: &Connection,
    actor: &ProcessActor,
    target: &ProcessMessageTarget,
) -> Result<()> {
    require_actor(conn, actor)?;
    match target {
        ProcessMessageTarget::Start { definition_id }
        | ProcessMessageTarget::Catch {
            definition_id,
            instance_id: None,
            ..
        } => require_owner(conn, actor, definition_id),
        ProcessMessageTarget::Catch {
            definition_id,
            instance_id: Some(id),
            subscription_id,
        } => {
            require_instance_reader(conn, actor, id)?;
            let actual: String = conn.query_row(
                "SELECT definition_id FROM bpmn_instances WHERE instance_id=?1 AND org_id=?2",
                params![id, actor.org_id],
                |r| r.get(0),
            )?;
            ensure!(&actual == definition_id, "message target not found");
            if let Some(s) = subscription_id {
                ensure!(
                    subscription_on(conn, s)?.instance_id == *id,
                    "message subscription not found"
                );
            }
            Ok(())
        }
    }
}
fn require_version_execution_on(
    conn: &Connection,
    actor: &ProcessActor,
    definition_id: &str,
    version: u32,
) -> Result<()> {
    require_actor(conn, actor)?;
    let v = version_on(conn, definition_id, version)?;
    for pin in v.service_flows {
        require_flow_current(conn, actor, &pin.flow_id, None)?;
    }
    for node in super::model::all_nodes(&v.model) {
        if let ProcessNodeKind::UserTask {
            assignee_user_id: Some(id),
            ..
        } = &node.kind
        {
            require_actor(
                conn,
                &ProcessActor {
                    org_id: actor.org_id.clone(),
                    user_id: id.clone(),
                },
            )?;
        }
    }
    Ok(())
}
fn message_summary_on(
    conn: &Connection,
    actor: &ProcessActor,
    m: &MessageRecord,
) -> Result<ProcessMessageSummary> {
    require_message_target_on(conn, actor, &m.target)?;
    let owns_namespace = actor.user_id == m.key.sender_user_id;
    let mutable = matches!(
        m.status,
        ProcessMessageStatus::Pending
            | ProcessMessageStatus::Blocked
            | ProcessMessageStatus::Ambiguous
    ) && m.expires_at_ms > now_ms()?;
    let source_readable = match &m.source_instance_id {
        Some(id) => match require_instance_reader(conn, actor, id) {
            Ok(_) => true,
            Err(error) if error.downcast_ref::<ProcessAuthorityDenied>().is_some() => false,
            Err(error) => return Err(error),
        },
        None => false,
    };
    Ok(ProcessMessageSummary {
        message_id: m.key.message_id.clone(),
        sender_user_id: m.key.sender_user_id.clone(),
        origin: m.origin.clone(),
        target: m.target.clone(),
        message_name: m.message_name.clone(),
        correlation_key: m.correlation_key.clone(),
        revision: m.revision,
        status: m.status.clone(),
        received_at_ms: m.received_at_ms,
        expires_at_ms: m.expires_at_ms,
        updated_at_ms: m.updated_at_ms,
        delivered_at_ms: m.delivered_at_ms,
        matched_instance_id: m.matched_instance_id.clone(),
        matched_version: m.matched_version,
        matched_subscription_id: m.matched_subscription_id.clone(),
        source_scope_id: if source_readable {
            m.source_scope_id.clone()
        } else {
            None
        },
        source_instance_id: if source_readable {
            m.source_instance_id.clone()
        } else {
            None
        },
        source_node_id: if source_readable {
            m.source_node_id.clone()
        } else {
            None
        },
        last_reason: m.last_reason.clone(),
        payload_sha256: m.payload_sha256.clone(),
        payload_bytes: m.payload_bytes,
        payload_available: m.payload_available,
        can_resolve: owns_namespace && mutable && m.status == ProcessMessageStatus::Ambiguous,
        can_cancel: owns_namespace && mutable,
    })
}
fn require_message_source_on(
    conn: &Connection,
    actor: &ProcessActor,
    message_id: &str,
    message_name: &str,
    correlation_key: &str,
    target: &ProcessMessageTarget,
    source: (&str, &str, &str, &str, &str),
) -> Result<(String, u32)> {
    let (instance_id, scope_id, node_id, activation_id, event_id) = source;
    require_actor(conn, actor)?;
    let (definition_id, version, status): (String, u32, String) = conn.query_row(
        "SELECT definition_id,version,status FROM bpmn_instances WHERE instance_id=?1 AND org_id=?2 AND initiator_user_id=?3",
        params![instance_id,actor.org_id,actor.user_id],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
    ).context("message source instance does not match its sender")?;
    if matches!(status.as_str(), "cancelled" | "error") {
        return Err(MessageClosed::Source.into());
    }
    let mut source_ancestor = instance_id.to_owned();
    while let Some(call) = calls_on(conn, &source_ancestor)?
        .into_iter()
        .find(|call| call.child_instance_id == source_ancestor)
    {
        let parent_status: String = conn.query_row(
            "SELECT status FROM bpmn_instances WHERE instance_id=?1",
            [&call.parent_instance_id],
            |row| row.get(0),
        )?;
        let allowed = (call.status == ProcessCallStatus::Waiting
            && call_control_live_on(conn, &source_ancestor)?)
            || call.status == ProcessCallStatus::Returned;
        if !allowed || matches!(parent_status.as_str(), "cancelled" | "error") {
            return Err(MessageClosed::Source.into());
        }
        let parent_actor = call_initiator_on(conn, &call.parent_instance_id)?;
        require_actor(conn, &parent_actor)?;
        require_owner(conn, &parent_actor, &call.definition_id)?;
        require_version_execution_on(conn, &parent_actor, &call.definition_id, call.version)?;
        source_ancestor = call.parent_instance_id;
    }
    require_owner(conn, actor, &definition_id)?;
    require_version_execution_on(conn, actor, &definition_id, version)?;
    let model = current_version_model_on(conn, &definition_id, version)?;
    let scope = scopes_on(conn, instance_id, &model)?
        .into_iter()
        .find(|scope| scope.scope_id == scope_id)
        .context("message source scope is missing")?;
    if scope.status == ProcessInstanceStatus::Cancelled {
        return Err(MessageClosed::Source.into());
    }
    let node = scoped_node_on(conn, instance_id, scope_id, &model, node_id)?;
    let ProcessNodeKind::MessageThrow {
        message_ref,
        target: declared_target,
        ..
    } = &node.kind
    else {
        bail!("message source is not its pinned Throw node");
    };
    let declared_name = model
        .messages
        .iter()
        .find(|declaration| &declaration.message_id == message_ref)
        .context("message source declaration missing")?;
    ensure!(
        declared_name.name == message_name && Uuid::parse_str(activation_id).is_ok(),
        "message source declaration or logical activation changed"
    );
    let declared_definition = match declared_target {
        tentaflow_protocol::processes::ProcessMessageTargetSpec::Start { definition_id }
        | tentaflow_protocol::processes::ProcessMessageTargetSpec::Catch {
            definition_id, ..
        } => definition_id,
    };
    let actual_definition = match target {
        ProcessMessageTarget::Start { definition_id }
        | ProcessMessageTarget::Catch { definition_id, .. } => definition_id,
    };
    ensure!(
        declared_definition == actual_definition
            && matches!(
                (declared_target, target),
                (
                    tentaflow_protocol::processes::ProcessMessageTargetSpec::Start { .. },
                    ProcessMessageTarget::Start { .. }
                ) | (
                    tentaflow_protocol::processes::ProcessMessageTargetSpec::Catch { .. },
                    ProcessMessageTarget::Catch { .. }
                )
            ),
        "message source target differs from its pinned Throw contract"
    );
    let (kind, event_node, data): (String, Option<String>, String) = conn
        .query_row(
            "SELECT kind,node_id,data_json FROM bpmn_events WHERE event_id=?1 AND instance_id=?2 AND scope_id=?3",
            params![event_id,instance_id,scope_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .context("message source event is missing")?;
    let data: Value = parse(data)?;
    ensure!(
        kind == "message_queued"
            && event_node.as_deref() == Some(node_id)
            && data["message_id"].as_str() == Some(message_id)
            && data["source_activation_id"].as_str() == Some(activation_id)
            && data["message_name"].as_str() == Some(message_name)
            && data["correlation_key"].as_str() == Some(correlation_key)
            && data["target"] == serde_json::to_value(target)?,
        "message source differs from its actual queued event"
    );
    Ok((definition_id, version))
}

fn insert_message_on(
    tx: &Transaction<'_>,
    actor: &ProcessActor,
    prepared: &PreparedMessage,
    source: Option<(&str, &str, &str, &str, &str)>,
    at_ms: i64,
) -> Result<MessageRecord> {
    super::messages::validate_message(prepared)?;
    require_message_target_on(tx, actor, &prepared.target)?;
    let (kind, definition_id, instance_id, subscription_id) = match &prepared.target {
        ProcessMessageTarget::Start { definition_id } => ("start", definition_id, None, None),
        ProcessMessageTarget::Catch {
            definition_id,
            instance_id,
            subscription_id,
        } => (
            "catch",
            definition_id,
            instance_id.as_deref(),
            subscription_id.as_deref(),
        ),
    };
    let hash = request_hash(&(prepared, source))?;
    let key = MessageKey {
        org_id: actor.org_id.clone(),
        sender_user_id: actor.user_id.clone(),
        message_id: prepared.message_id.clone(),
    };
    let existing:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM bpmn_messages WHERE org_id=?1 AND sender_user_id=?2 AND message_id=?3)",params![key.org_id,key.sender_user_id,key.message_id],|r|r.get(0))?;
    if existing {
        let old = message_on(tx, &key, true)?;
        ensure!(
            old.request_hash == hash,
            "message identity conflicts with immutable envelope"
        );
        return Ok(old);
    }
    if let Some(id) = instance_id {
        let closed: bool = tx.query_row(
            "SELECT status IN ('completed','cancelled','error') FROM bpmn_instances WHERE instance_id=?1",
            [id],
            |r| r.get(0),
        )?;
        ensure!(!closed, "target instance is closed");
    }
    if kind == "start" {
        let definition = definition_on(tx, definition_id)?;
        ensure!(!definition.archived, "definition_archived");
        let version = version_on(
            tx,
            definition_id,
            definition
                .published_version
                .context("message start is not published")?,
        )?;
        ensure!(version.model.nodes.iter().any(|n|matches!(&n.kind,ProcessNodeKind::MessageStart{message_ref,..}if version.model.messages.iter().any(|d|&d.message_id==message_ref&&d.name==prepared.message_name))),"message start name is not declared by the current published version");
    }
    let payload = json(&prepared.payload)?;
    let (sender_count,sender_bytes,org_count,org_bytes):(u32,u64,u32,u64)=tx.query_row("SELECT COALESCE(SUM(sender_user_id=?2),0),COALESCE(SUM(CASE WHEN sender_user_id=?2 THEN payload_bytes ELSE 0 END),0),COUNT(*),COALESCE(SUM(payload_bytes),0) FROM bpmn_messages WHERE org_id=?1 AND status IN ('pending','blocked','ambiguous')",params![actor.org_id,actor.user_id],|r|Ok((r.get(0)?,row_u64(r,1)?,r.get(2)?,row_u64(r,3)?)))?;
    ensure!(
        sender_count < 1024
            && org_count < 4096
            && sender_bytes
                .checked_add(u64::try_from(payload.len())?)
                .is_some_and(|n| n <= 64 * 1024 * 1024)
            && org_bytes
                .checked_add(u64::try_from(payload.len())?)
                .is_some_and(|n| n <= 256 * 1024 * 1024),
        "pending message capacity exceeded"
    );
    let expires = at_ms
        .checked_add(i64::from(prepared.ttl_seconds) * 1000)
        .context("message TTL overflow")?;
    let (source_instance, source_scope, source_node, source_activation, source_event) = match source
    {
        Some((i, s, n, a, e)) => (Some(i), Some(s), Some(n), Some(a), Some(e)),
        None => (None, None, None, None, None),
    };
    let source_identity = source
        .map(|facts| {
            require_message_source_on(
                tx,
                actor,
                &prepared.message_id,
                &prepared.message_name,
                &prepared.correlation_key,
                &prepared.target,
                facts,
            )
        })
        .transpose()?;
    tx.execute("INSERT INTO bpmn_messages(org_id,sender_user_id,message_id,request_hash,origin,target_kind,definition_id,target_instance_id,target_subscription_id,message_name,correlation_key,payload_json,payload_sha256,payload_bytes,ttl_seconds,received_at_ms,expires_at_ms,revision,status,next_check_at_ms,updated_at_ms,source_instance_id,source_definition_id,source_version,source_node_id,source_activation_id,source_event_id,source_scope_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,1,'pending',?16,?16,?18,?19,?20,?21,?22,?23,?24)",params![actor.org_id,actor.user_id,prepared.message_id,hash,if source.is_some(){"process"}else{"api"},kind,definition_id,instance_id,subscription_id,prepared.message_name,prepared.correlation_key,payload,hex::encode(Sha256::digest(payload.as_bytes())),u32::try_from(payload.len())?,prepared.ttl_seconds,at_ms,expires,source_instance,source_identity.as_ref().map(|r|r.0.as_str()),source_identity.as_ref().map(|r|r.1),source_node,source_activation,source_event,source_scope])?;
    message_on(tx, &key, true)
}
pub fn send_message(
    pool: &DbPool,
    actor: &ProcessActor,
    stamp: &CommandStamp,
    message: &PreparedMessage,
    at_ms: i64,
) -> Result<ProcessMessageSummary> {
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    require_message_target_on(&tx, actor, &message.target)?;
    if let Some(prior) = command_replay::<ProcessMessageSummary>(&tx, actor, stamp)? {
        let key = MessageKey {
            org_id: actor.org_id.clone(),
            sender_user_id: actor.user_id.clone(),
            message_id: prior.message_id,
        };
        return message_summary_on(&tx, actor, &message_on(&tx, &key, true)?);
    }
    let m = insert_message_on(&tx, actor, message, None, at_ms)?;
    let result = message_summary_on(&tx, actor, &m)?;
    store_command(&tx, actor, stamp, &result, at_ms)?;
    tx.commit()?;
    Ok(result)
}
pub fn get_message(
    pool: &DbPool,
    actor: &ProcessActor,
    sender_user_id: &str,
    message_id: &str,
) -> Result<ProcessMessageDetail> {
    read_snapshot(pool, |conn| {
        require_actor(conn, actor)?;
        let m = message_on(
            conn,
            &MessageKey {
                org_id: actor.org_id.clone(),
                sender_user_id: sender_user_id.to_owned(),
                message_id: message_id.to_owned(),
            },
            true,
        )?;
        Ok(ProcessMessageDetail {
            message: message_summary_on(conn, actor, &m)?,
            payload: m.payload,
        })
    })
}
const MESSAGE_KEYS_SQL: &str = "SELECT sender_user_id,message_id FROM bpmn_messages WHERE org_id=?1 AND (?2 IS NULL OR source_instance_id=?2) AND (?3 IS NULL OR sender_user_id=?3) AND (?4 IS NULL OR definition_id=?4) AND (?5 IS NULL OR COALESCE(matched_instance_id,target_instance_id)=?5) ORDER BY CASE WHEN status IN ('pending','blocked','ambiguous') THEN 0 ELSE 1 END,received_at_ms DESC,sender_user_id,message_id";
fn message_keys_on(
    conn: &Connection,
    org_id: &str,
    source_instance_id: Option<&str>,
    sender_id: Option<&str>,
    definition_id: Option<&str>,
    target_instance_id: Option<&str>,
) -> Result<Vec<MessageKey>> {
    let mut q = conn.prepare(MESSAGE_KEYS_SQL)?;
    let rows = q
        .query_map(
            params![
                org_id,
                source_instance_id,
                sender_id,
                definition_id,
                target_instance_id
            ],
            |r| {
                Ok(MessageKey {
                    org_id: org_id.to_owned(),
                    sender_user_id: r.get(0)?,
                    message_id: r.get(1)?,
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}
pub fn list_messages(
    pool: &DbPool,
    actor: &ProcessActor,
    definition_id: Option<&str>,
    instance_id: Option<&str>,
    offset: u32,
    limit: u32,
) -> Result<(Vec<ProcessMessageSummary>, u32, bool)> {
    page(offset, limit)?;
    read_snapshot(pool, |conn| {
        require_actor(conn, actor)?;
        let mut total = 0u32;
        let mut rows = Vec::new();
        let mut statement = conn.prepare(MESSAGE_KEYS_SQL)?;
        let mut keys = statement.query(params![
            actor.org_id,
            None::<&str>,
            actor.user_id,
            definition_id,
            instance_id
        ])?;
        while let Some(row) = keys.next()? {
            let key = MessageKey {
                org_id: actor.org_id.clone(),
                sender_user_id: row.get(0)?,
                message_id: row.get(1)?,
            };
            let m = message_on(conn, &key, false)?;
            let summary = match message_summary_on(conn, actor, &m) {
                Ok(summary) => summary,
                Err(error) if error.downcast_ref::<ProcessAuthorityDenied>().is_some() => continue,
                Err(error) => return Err(error),
            };
            if total >= offset && rows.len() < usize::try_from(limit)? {
                rows.push(summary);
            }
            total = total.checked_add(1).context("message count overflow")?;
        }
        let more = offset
            .checked_add(u32::try_from(rows.len())?)
            .context("message page overflow")?
            < total;
        Ok((rows, total, more))
    })
}
fn message_candidate_matches(m: &MessageRecord, c: &MessageCandidate) -> bool {
    m.key == c.key
        && m.revision == c.revision
        && m.next_check_at_ms == c.next_check_at_ms
        && matches!(
            m.status,
            ProcessMessageStatus::Pending | ProcessMessageStatus::Blocked
        )
}
fn message_selection_on(conn: &Connection, c: &MessageCandidate) -> Result<MessageSelection> {
    let m = message_on(conn, &c.key, true)?;
    if !message_candidate_matches(&m, c) {
        return Ok(MessageSelection::Stale);
    }
    let sender = ProcessActor {
        org_id: m.key.org_id.clone(),
        user_id: m.key.sender_user_id.clone(),
    };
    require_message_target_on(conn, &sender, &m.target)?;
    if m.origin == ProcessMessageOrigin::Process {
        let (instance, definition, version, node, activation, event): (String, String, u32, String, String, String) = conn.query_row(
            "SELECT source_instance_id,source_definition_id,source_version,source_node_id,source_activation_id,source_event_id FROM bpmn_messages WHERE org_id=?1 AND sender_user_id=?2 AND message_id=?3",
            params![m.key.org_id,m.key.sender_user_id,m.key.message_id],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
        )?;
        ensure!(
            require_message_source_on(
                conn,
                &sender,
                &m.key.message_id,
                &m.message_name,
                &m.correlation_key,
                &m.target,
                (
                    &instance,
                    m.source_scope_id
                        .as_deref()
                        .context("process message source scope missing")?,
                    &node,
                    &activation,
                    &event
                )
            )? == (definition, version),
            "message source identity differs from its immutable instance"
        );
    }
    let (definition_id, instance_id, subscription_id) = match &m.target {
        ProcessMessageTarget::Start { definition_id } => {
            let d = definition_on(conn, definition_id)?;
            ensure!(!d.archived, "definition_archived");
            let version = version_on(
                conn,
                definition_id,
                d.published_version.context("start_definition_changed")?,
            )?;
            ensure!(
                version.model.nodes.iter().any(|n| match &n.kind {
                    ProcessNodeKind::MessageStart { message_ref, .. } => version
                        .model
                        .messages
                        .iter()
                        .any(|decl| &decl.message_id == message_ref && decl.name == m.message_name),
                    _ => false,
                }),
                "start_definition_changed"
            );
            require_version_execution_on(conn, &sender, definition_id, version.version)?;
            return Ok(MessageSelection::Ready(MessageSnapshot {
                candidate: c.clone(),
                message: m,
                target: MessageDeliveryTarget::Start {
                    actor: sender,
                    version,
                    instance_id: Uuid::new_v4().to_string(),
                },
            }));
        }
        ProcessMessageTarget::Catch {
            definition_id,
            instance_id,
            subscription_id,
        } => (
            definition_id,
            instance_id.as_deref(),
            subscription_id.as_deref(),
        ),
    };
    let exact_instance = m.resolved_instance_id.as_deref().or(instance_id);
    let exact_subscription = m.resolved_subscription_id.as_deref().or(subscription_id);
    ensure!(
        instance_id.is_none_or(|id| m
            .resolved_instance_id
            .as_deref()
            .is_none_or(|resolved| resolved == id))
            && subscription_id.is_none_or(|id| m
                .resolved_subscription_id
                .as_deref()
                .is_none_or(|resolved| resolved == id)),
        "message resolution differs from its immutable address"
    );
    if let Some(id) = exact_instance {
        let status:String=conn.query_row("SELECT status FROM bpmn_instances WHERE instance_id=?1 AND definition_id=?2 AND org_id=?3",params![id,definition_id,m.key.org_id],|r|r.get(0)).context("target instance not found")?;
        if matches!(status.as_str(), "completed" | "cancelled" | "error") {
            return Err(
                if m.resolved_token_id.is_some() || exact_subscription.is_some() {
                    MessageClosed::Activation
                } else {
                    MessageClosed::Instance
                }
                .into(),
            );
        }
    }
    let mut q=conn.prepare("SELECT s.subscription_id FROM bpmn_event_subscriptions s JOIN bpmn_tokens t ON t.token_id=s.token_id AND t.instance_id=s.instance_id AND t.status='waiting' JOIN bpmn_instances i ON i.instance_id=s.instance_id AND i.status NOT IN ('completed','cancelled','error') LEFT JOIN bpmn_event_races r ON r.race_id=s.race_id WHERE s.org_id=?1 AND s.definition_id=?2 AND s.kind IN ('message_catch','boundary_message') AND s.message_name=?3 AND s.correlation_key=?4 AND s.status='open' AND (s.race_id IS NULL OR r.status='open') AND (?5 IS NULL OR s.instance_id=?5) AND (?6 IS NULL OR s.subscription_id=?6) ORDER BY s.subscription_id LIMIT 2")?;
    let ids = q
        .query_map(
            params![
                m.key.org_id,
                definition_id,
                m.message_name,
                m.correlation_key,
                exact_instance,
                exact_subscription
            ],
            |r| r.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut eligible = Vec::new();
    for id in ids {
        let s = subscription_on(conn, &id)?;
        if subscription_live_on(conn, &s)? {
            eligible.push(s);
        }
    }
    if eligible.is_empty() {
        if exact_subscription.is_some() {
            return Err(MessageClosed::Activation.into());
        }
        return Ok(MessageSelection::NoMatch);
    }
    if eligible.len() > 1 {
        return Ok(MessageSelection::Ambiguous);
    }
    let s = eligible.remove(0);
    if !m
        .resolved_token_id
        .as_ref()
        .is_none_or(|id| id == &s.token_id)
    {
        return Err(MessageClosed::Activation.into());
    }
    let recipient: String = conn.query_row(
        "SELECT initiator_user_id FROM bpmn_instances WHERE instance_id=?1",
        [&s.instance_id],
        |r| r.get(0),
    )?;
    let actor = ProcessActor {
        org_id: m.key.org_id.clone(),
        user_id: recipient,
    };
    require_call_control_authority_on(conn, &actor, &s.instance_id)?;
    require_version_execution_on(conn, &actor, &s.definition_id, s.version)?;
    let snapshot = runtime_snapshot_on(conn, &actor, &s.instance_id)?;
    Ok(MessageSelection::Ready(MessageSnapshot {
        candidate: c.clone(),
        message: m,
        target: MessageDeliveryTarget::Catch {
            actor,
            subscription: s,
            snapshot,
        },
    }))
}
pub fn due_messages(pool: &DbPool, at_ms: i64, limit: u32) -> Result<Vec<MessageCandidate>> {
    ensure!(
        (1..=32).contains(&limit),
        "message drain batch must be 1..32"
    );
    read_snapshot(pool, |conn| {
        let mut q=conn.prepare("SELECT org_id,sender_user_id,message_id,revision,next_check_at_ms FROM bpmn_messages WHERE status IN ('pending','blocked') AND next_check_at_ms<=?1 AND expires_at_ms>?1 ORDER BY received_at_ms,sender_user_id,message_id LIMIT ?2")?;
        let rows = q
            .query_map(params![at_ms, limit], |r| {
                Ok(MessageCandidate {
                    key: MessageKey {
                        org_id: r.get(0)?,
                        sender_user_id: r.get(1)?,
                        message_id: r.get(2)?,
                    },
                    revision: row_u64(r, 3)?,
                    next_check_at_ms: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    })
}
pub fn message_snapshot(pool: &DbPool, candidate: &MessageCandidate) -> Result<MessageSelection> {
    read_snapshot(pool, |conn| message_selection_on(conn, candidate))
}
fn message_receipt_on(
    tx: &Transaction<'_>,
    m: &MessageRecord,
    kind: &str,
    data: Value,
    at_ms: i64,
) -> Result<()> {
    if let Some(id) = &m.source_instance_id {
        let seq: u64 = tx.query_row(
            "SELECT COALESCE(MAX(seq),0)+1 FROM bpmn_events WHERE instance_id=?1",
            [id],
            |r| row_u64(r, 0),
        )?;
        insert_event_on(
            tx,
            id,
            seq,
            Some(&m.key.sender_user_id),
            &PlannedEvent {
                scope_id: m
                    .source_scope_id
                    .clone()
                    .context("process receipt source scope missing")?,
                kind: kind.to_owned(),
                node_id: m.source_node_id.clone(),
                data,
            },
            at_ms,
            None,
        )?;
    } else {
        crate::db::repository::log_audit_scoped_tx(
            tx,
            Some(&m.key.sender_user_id),
            &format!("process.{kind}"),
            &m.key.message_id,
            "bpmn_message",
            &m.key.message_id,
            Some(&json(&data)?),
            "info",
            "unclassified",
            Some(&m.key.org_id),
            None,
        )?;
    }
    Ok(())
}
fn update_message_state_on(
    tx: &Transaction<'_>,
    m: &MessageRecord,
    status: ProcessMessageStatus,
    reason: Option<&str>,
    next: i64,
    at_ms: i64,
) -> Result<bool> {
    let reason = reason.map(timer_reason).transpose()?;
    let changed = m.status != status || m.last_reason != reason;
    let count = if changed {
        tx.execute("UPDATE bpmn_messages SET status=?1,last_reason=?2,next_check_at_ms=?3,revision=revision+1,updated_at_ms=?4 WHERE org_id=?5 AND sender_user_id=?6 AND message_id=?7 AND revision=?8 AND next_check_at_ms=?9 AND status IN ('pending','blocked','ambiguous')",params![message_status_text(&status),reason,next,at_ms,m.key.org_id,m.key.sender_user_id,m.key.message_id,sql_incrementable(m.revision)?,m.next_check_at_ms])?
    } else {
        tx.execute(
            "UPDATE bpmn_messages SET next_check_at_ms=?1 WHERE org_id=?2 AND sender_user_id=?3 AND message_id=?4 AND revision=?5 AND next_check_at_ms=?6 AND status IN ('pending','blocked','ambiguous')",
            params![next,m.key.org_id,m.key.sender_user_id,m.key.message_id,sql_integer(m.revision)?,m.next_check_at_ms],
        )?
    };
    if count == 1 && changed {
        let kind = match status {
            ProcessMessageStatus::Pending => return Ok(true),
            ProcessMessageStatus::Blocked => "message_blocked",
            ProcessMessageStatus::Ambiguous => "message_ambiguous",
            ProcessMessageStatus::Expired => "message_expired",
            ProcessMessageStatus::Cancelled => "message_cancelled",
            ProcessMessageStatus::Error => "message_error",
            ProcessMessageStatus::Delivered => bail!("delivery needs its matched identity"),
        };
        message_receipt_on(
            tx,
            m,
            kind,
            serde_json::json!({"message_id":m.key.message_id,"sender_user_id":m.key.sender_user_id,"reason":reason}),
            at_ms,
        )?;
    }
    Ok(count == 1)
}
pub fn resolve_message(
    pool: &DbPool,
    actor: &ProcessActor,
    stamp: &CommandStamp,
    message_id: &str,
    expected_revision: u64,
    instance_id: &str,
    subscription_id: &str,
    at_ms: i64,
) -> Result<ProcessMessageSummary> {
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    let key = MessageKey {
        org_id: actor.org_id.clone(),
        sender_user_id: actor.user_id.clone(),
        message_id: message_id.to_owned(),
    };
    let m = message_on(&tx, &key, true)?;
    require_message_target_on(&tx, actor, &m.target)?;
    if command_replay::<ProcessMessageSummary>(&tx, actor, stamp)?.is_some() {
        return message_summary_on(&tx, actor, &m);
    }
    ensure!(
        m.revision == expected_revision
            && m.status == ProcessMessageStatus::Ambiguous
            && m.expires_at_ms > now_ms()?,
        "message revision conflict or no longer ambiguous"
    );
    let s = subscription_on(&tx, subscription_id)?;
    require_instance_reader(&tx, actor, instance_id)?;
    let def = match &m.target {
        ProcessMessageTarget::Catch {
            definition_id,
            instance_id: original_instance,
            subscription_id: original_subscription,
        } => {
            ensure!(
                original_instance
                    .as_deref()
                    .is_none_or(|id| id == instance_id)
                    && original_subscription
                        .as_deref()
                        .is_none_or(|id| id == subscription_id),
                "chosen subscription differs from the immutable message address"
            );
            definition_id
        }
        _ => bail!("only Catch messages can resolve ambiguity"),
    };
    ensure!(
        s.instance_id == instance_id
            && s.org_id == actor.org_id
            && s.definition_id == *def
            && s.status == ProcessSubscriptionStatus::Open
            && s.message_name.as_deref() == Some(m.message_name.as_str())
            && s.correlation_key.as_deref() == Some(m.correlation_key.as_str())
            && subscription_live_on(&tx, &s)?,
        "chosen subscription is no longer an exact match"
    );
    tx.execute("UPDATE bpmn_messages SET resolved_instance_id=?1,resolved_subscription_id=?2,resolved_token_id=?3,status='pending',last_reason=NULL,next_check_at_ms=?4,revision=revision+1,updated_at_ms=?4 WHERE org_id=?5 AND sender_user_id=?6 AND message_id=?7 AND revision=?8",params![instance_id,subscription_id,s.token_id,at_ms,key.org_id,key.sender_user_id,key.message_id,sql_incrementable(expected_revision)?])?;
    message_receipt_on(
        &tx,
        &m,
        "message_resolved",
        serde_json::json!({"message_id":message_id,"instance_id":instance_id,"subscription_id":subscription_id}),
        at_ms,
    )?;
    let result = message_summary_on(&tx, actor, &message_on(&tx, &key, true)?)?;
    store_command(&tx, actor, stamp, &result, at_ms)?;
    tx.commit()?;
    Ok(result)
}
pub fn cancel_message(
    pool: &DbPool,
    actor: &ProcessActor,
    stamp: &CommandStamp,
    message_id: &str,
    expected_revision: u64,
    at_ms: i64,
) -> Result<ProcessMessageSummary> {
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    let key = MessageKey {
        org_id: actor.org_id.clone(),
        sender_user_id: actor.user_id.clone(),
        message_id: message_id.to_owned(),
    };
    let m = message_on(&tx, &key, true)?;
    require_message_target_on(&tx, actor, &m.target)?;
    if command_replay::<ProcessMessageSummary>(&tx, actor, stamp)?.is_some() {
        return message_summary_on(&tx, actor, &m);
    }
    ensure!(
        m.revision == expected_revision
            && matches!(
                m.status,
                ProcessMessageStatus::Pending
                    | ProcessMessageStatus::Blocked
                    | ProcessMessageStatus::Ambiguous
            )
            && m.expires_at_ms > now_ms()?,
        "message revision conflict or terminal message"
    );
    ensure!(
        update_message_state_on(
            &tx,
            &m,
            ProcessMessageStatus::Cancelled,
            Some("sender_cancelled"),
            at_ms,
            at_ms
        )?,
        "message revision conflict"
    );
    let result = message_summary_on(&tx, actor, &message_on(&tx, &key, true)?)?;
    store_command(&tx, actor, stamp, &result, at_ms)?;
    tx.commit()?;
    Ok(result)
}
pub fn record_message_waiting(pool: &DbPool, c: &MessageCandidate, at_ms: i64) -> Result<bool> {
    record_message_selection(pool, c, None, at_ms)
}
pub fn record_message_ambiguous(pool: &DbPool, c: &MessageCandidate, at_ms: i64) -> Result<bool> {
    record_message_selection(pool, c, Some(ProcessMessageStatus::Ambiguous), at_ms)
}
fn record_message_selection(
    pool: &DbPool,
    c: &MessageCandidate,
    state: Option<ProcessMessageStatus>,
    at_ms: i64,
) -> Result<bool> {
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    let selection = message_selection_on(&tx, c)?;
    let valid = match (&selection, &state) {
        (MessageSelection::NoMatch, None)
        | (MessageSelection::Ambiguous, Some(ProcessMessageStatus::Ambiguous)) => true,
        _ => false,
    };
    if !valid {
        return Ok(false);
    }
    let m = message_on(&tx, &c.key, true)?;
    if m.expires_at_ms <= now_ms()? {
        return Ok(false);
    }
    let next = at_ms.checked_add(1000).context("message wait overflow")?;
    let result = update_message_state_on(
        &tx,
        &m,
        state.unwrap_or(ProcessMessageStatus::Pending),
        None,
        next,
        at_ms,
    )?;
    tx.commit()?;
    Ok(result)
}
pub fn record_message_blocked(
    pool: &DbPool,
    c: &MessageCandidate,
    reason: &str,
    at_ms: i64,
) -> Result<bool> {
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    let m = message_on(&tx, &c.key, true)?;
    if !message_candidate_matches(&m, c) || m.expires_at_ms <= now_ms()? {
        return Ok(false);
    }
    match message_selection_on(&tx, c) {
        Err(e) if e.downcast_ref::<ProcessAuthorityDenied>().is_some() => {}
        Err(e) => return Err(e),
        Ok(_) => return Ok(false),
    }
    let next = at_ms
        .checked_add(60_000)
        .context("message authority retry overflow")?;
    let result = update_message_state_on(
        &tx,
        &m,
        ProcessMessageStatus::Blocked,
        Some(reason),
        next,
        at_ms,
    )?;
    tx.commit()?;
    Ok(result)
}
pub fn record_message_failed(
    pool: &DbPool,
    c: &MessageCandidate,
    reason: &str,
    at_ms: i64,
) -> Result<bool> {
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    let m = message_on(&tx, &c.key, true)?;
    if !message_candidate_matches(&m, c) || m.expires_at_ms <= now_ms()? {
        return Ok(false);
    }
    let ready = match message_selection_on(&tx, c) {
        Ok(MessageSelection::Ready(snapshot)) => Some(snapshot),
        Ok(MessageSelection::Stale) => return Ok(false),
        Err(e) if e.downcast_ref::<ProcessAuthorityDenied>().is_some() => return Ok(false),
        Err(e) if e.downcast_ref::<rusqlite::Error>().is_some() => return Err(e),
        Err(e) if e.downcast_ref::<MessageClosed>().is_some() => {
            let reason = e
                .downcast_ref::<MessageClosed>()
                .context("message closure reason is missing")?
                .reason();
            let result = update_message_state_on(
                &tx,
                &m,
                ProcessMessageStatus::Cancelled,
                Some(reason),
                at_ms,
                at_ms,
            )?;
            tx.commit()?;
            return Ok(result);
        }
        _ => None,
    };
    if let Some(MessageSnapshot {
        target:
            MessageDeliveryTarget::Catch {
                actor,
                subscription,
                snapshot,
            },
        ..
    }) = ready
    {
        let incident = ProcessIncident {
            scope_id: subscription.scope_id.clone(),
            incident_id: Uuid::new_v4().to_string(),
            node_id: Some(subscription.node_id.clone()),
            node_name: None,
            job_id: None,
            code: "MESSAGE_MAPPING_ERROR".to_owned(),
            message: bounded_failure_message(reason),
            at_ms,
            can_retry: false,
        };
        let mut plan = RuntimePlan::initial(snapshot.instance.variables.clone());
        plan.status = ProcessInstanceStatus::Incident;
        plan.events.push(PlannedEvent{scope_id:subscription.scope_id.clone(),kind:"message_error".into(),node_id:Some(subscription.node_id.clone()),data:serde_json::json!({"message_id":m.key.message_id,"subscription_id":subscription.subscription_id,"attached_token_id":subscription.token_id,"incident_id":incident.incident_id,"reason":bounded_failure_message(reason)})});
        plan.add_incidents.push(incident);
        apply_plan_on(
            &tx,
            &snapshot.instance.instance_id,
            &actor.user_id,
            snapshot.instance.revision,
            &plan,
            at_ms,
        )?;
    }
    let result = update_message_state_on(
        &tx,
        &m,
        ProcessMessageStatus::Error,
        Some(reason),
        at_ms,
        at_ms,
    )?;
    tx.commit()?;
    Ok(result)
}
pub fn deliver_message(
    pool: &DbPool,
    prepared: &MessageSnapshot,
    plan: &RuntimePlan,
    at_ms: i64,
) -> Result<Option<MessageDeliveryOutcome>> {
    let (actor, source_id, initial) = match &prepared.target {
        MessageDeliveryTarget::Start {
            actor,
            version,
            instance_id,
        } => (
            actor,
            instance_id.as_str(),
            Some((
                version.definition_id.as_str(),
                version.version,
                plan.start_variables
                    .as_ref()
                    .context("message start variables missing")?,
            )),
        ),
        MessageDeliveryTarget::Catch {
            actor, snapshot, ..
        } => (actor, snapshot.instance.instance_id.as_str(), None),
    };
    let (composite, mut conn) = prepare_call_plan(pool, actor, source_id, initial, plan, at_ms)?;
    let plan = &composite;
    let tx = conn.transaction()?;
    let fresh = match message_selection_on(&tx, &prepared.candidate)? {
        MessageSelection::Ready(s) => s,
        _ => return Ok(None),
    };
    if fresh.message.expires_at_ms <= now_ms()? {
        return Ok(None);
    }
    let delivered = plan
        .events
        .iter()
        .filter(|event| event.kind == "message_delivered")
        .collect::<Vec<_>>();
    let m = &fresh.message;
    let metadata = serde_json::json!({"message_id":m.key.message_id,"sender_user_id":m.key.sender_user_id,"message_name":m.message_name,"correlation_key":m.correlation_key,"received_at_ms":m.received_at_ms,"expires_at_ms":m.expires_at_ms,"payload_sha256":m.payload_sha256});
    ensure!(
        delivered.len() == 1
            && delivered[0].data["message_id"].as_str() == Some(m.key.message_id.as_str())
            && delivered[0].data["message"] == metadata
            && Some(&delivered[0].data["payload"]) == m.payload.as_ref(),
        "message delivery lacks its immutable envelope facts"
    );
    let (actor, instance, version, node_id, subscription_id, claims) = match (
        &prepared.target,
        &fresh.target,
    ) {
        (
            MessageDeliveryTarget::Start {
                version: old,
                instance_id,
                ..
            },
            MessageDeliveryTarget::Start { actor, version, .. },
        ) => {
            if old.version != version.version || old.model_sha256 != version.model_sha256 {
                return Ok(None);
            }
            ensure!(
                plan.start_instance_id.as_deref() == Some(instance_id.as_str()),
                "message start plan identity is wrong"
            );
            let node=version.model.nodes.iter().find(|n|matches!(&n.kind,ProcessNodeKind::MessageStart{message_ref,..}if version.model.messages.iter().any(|d|&d.message_id==message_ref&&d.name==fresh.message.message_name))).context("message start node missing")?;
            ensure!(
                delivered[0].node_id.as_deref() == Some(node.id.as_str()),
                "message start event uses another node"
            );
            let instance = start_instance_on(
                &tx,
                actor,
                instance_id,
                &version.definition_id,
                version.version,
                plan.start_variables
                    .as_ref()
                    .context("message start inputs missing")?,
                plan,
                at_ms,
                None,
                Some(&fresh.message.key),
            )?;
            (
                actor.clone(),
                instance,
                version.version,
                node.id.clone(),
                None,
                Vec::new(),
            )
        }
        (
            MessageDeliveryTarget::Catch {
                subscription: old,
                snapshot: old_snapshot,
                ..
            },
            MessageDeliveryTarget::Catch {
                actor,
                subscription,
                snapshot,
            },
        ) => {
            if old.subscription_id != subscription.subscription_id
                || old.token_id != subscription.token_id
                || old.revision != subscription.revision
                || old_snapshot.instance.revision != snapshot.instance.revision
            {
                return Ok(None);
            }
            ensure!(
                (subscription.kind == ProcessSubscriptionKind::MessageCatch
                    || subscription.kind == ProcessSubscriptionKind::BoundaryMessage)
                    && delivered[0].node_id.as_deref() == Some(subscription.node_id.as_str())
                    && delivered[0].data["subscription_id"].as_str()
                        == Some(subscription.subscription_id.as_str())
                    && delivered[0].data["attached_token_id"].as_str()
                        == Some(subscription.token_id.as_str()),
                "message delivery event uses another activation"
            );
            ensure!(
                plan.subscription_updates
                    .iter()
                    .any(|u| u.subscription_id == subscription.subscription_id
                        && u.status == ProcessSubscriptionStatus::Consumed),
                "message plan does not consume its exact subscription"
            );
            if subscription.kind == ProcessSubscriptionKind::BoundaryMessage {
                validate_boundary_plan_on(
                    &tx,
                    &BoundaryActivationId::Subscription(subscription.subscription_id.clone()),
                    None,
                    plan,
                )?;
            } else {
                ensure!(
                    (reaches_error_end(plan)
                        || (plan.cancel_user_task_ids.is_empty()
                            && plan.cancel_job_ids.is_empty()
                            && plan.cancel_scope_roots.is_empty())),
                    "message catch cannot cancel an activity"
                );
                if let Some(id) = &subscription.race_id {
                    ensure!(
                        plan.race_updates.len() == 1
                            && plan.race_updates[0].race_id == *id
                            && plan.race_updates[0].status == ProcessEventRaceStatus::Won
                            && plan.race_updates[0].winner_subscription_id.as_deref()
                                == Some(subscription.subscription_id.as_str())
                            && plan.race_updates[0].winner_timer_id.is_none(),
                        "message delivery does not win its exact race"
                    );
                }
                ensure!(
                    reaches_error_end(plan)
                        || subscription.race_id.is_some()
                        || (plan.cancel_token_ids.is_empty()
                            && plan.race_updates.is_empty()
                            && plan.cancel_scope_roots.is_empty()),
                    "ordinary catch cannot cancel another activation"
                );
            }
            let claims = apply_plan_on(
                &tx,
                &snapshot.instance.instance_id,
                &actor.user_id,
                snapshot.instance.revision,
                plan,
                at_ms,
            )?;
            (
                actor.clone(),
                instance_on(&tx, actor, &snapshot.instance.instance_id, None)?,
                snapshot.instance.version,
                subscription.node_id.clone(),
                Some(subscription.subscription_id.clone()),
                claims,
            )
        }
        _ => return Ok(None),
    };
    let m = &fresh.message;
    let count=tx.execute("UPDATE bpmn_messages SET status='delivered',revision=revision+1,last_reason=NULL,updated_at_ms=?1,matched_instance_id=?2,matched_version=?3,matched_node_id=?4,matched_subscription_id=?5,delivered_at_ms=?1 WHERE org_id=?6 AND sender_user_id=?7 AND message_id=?8 AND revision=?9 AND status IN ('pending','blocked')",params![at_ms,instance.instance_id,version,node_id,subscription_id,m.key.org_id,m.key.sender_user_id,m.key.message_id,sql_incrementable(m.revision)?])?;
    ensure!(count == 1, "message delivery revision conflict");
    message_receipt_on(
        &tx,
        m,
        "message_delivered",
        serde_json::json!({"message_id":m.key.message_id,"sender_user_id":m.key.sender_user_id,"instance_id":instance.instance_id,"subscription_id":subscription_id,"version":version}),
        at_ms,
    )?;
    let sender = ProcessActor {
        org_id: m.key.org_id.clone(),
        user_id: m.key.sender_user_id.clone(),
    };
    let message = message_summary_on(&tx, &sender, &message_on(&tx, &m.key, true)?)?;
    let instance = instance_on(&tx, &actor, &instance.instance_id, None)?;
    tx.commit()?;
    Ok(Some(MessageDeliveryOutcome {
        message,
        transition: ProcessTransitionOutcome {
            instance,
            cancelled_claims: claims,
        },
    }))
}
pub fn expire_messages(pool: &DbPool, at_ms: i64, limit: u32) -> Result<u32> {
    ensure!(
        (1..=32).contains(&limit),
        "message expiry batch must be 1..32"
    );
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    let mut q=tx.prepare("SELECT org_id,sender_user_id,message_id FROM bpmn_messages WHERE status IN ('pending','blocked','ambiguous') AND expires_at_ms<=?1 ORDER BY expires_at_ms,message_id LIMIT ?2")?;
    let keys = q
        .query_map(params![at_ms, limit], |r| {
            Ok(MessageKey {
                org_id: r.get(0)?,
                sender_user_id: r.get(1)?,
                message_id: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(q);
    let mut count = 0;
    for key in keys {
        let m = message_on(&tx, &key, true)?;
        count += u32::from(update_message_state_on(
            &tx,
            &m,
            ProcessMessageStatus::Expired,
            Some("ttl_expired"),
            at_ms,
            at_ms,
        )?);
    }
    tx.commit()?;
    Ok(count)
}
pub fn prune_message_payloads(pool: &DbPool, at_ms: i64, limit: u32) -> Result<u32> {
    ensure!(
        (1..=32).contains(&limit),
        "message prune batch must be 1..32"
    );
    let cutoff = at_ms
        .checked_sub(7 * 86400 * 1000)
        .context("message retention overflow")?;
    let mut conn = pool.write()?;
    let tx = conn.transaction()?;
    let mut q=tx.prepare("SELECT org_id,sender_user_id,message_id FROM bpmn_messages WHERE status IN ('delivered','expired','cancelled','error') AND payload_pruned_at_ms IS NULL AND updated_at_ms<=?1 ORDER BY updated_at_ms,message_id LIMIT ?2")?;
    let keys = q
        .query_map(params![cutoff, limit], |r| {
            Ok(MessageKey {
                org_id: r.get(0)?,
                sender_user_id: r.get(1)?,
                message_id: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(q);
    let count = u32::try_from(keys.len())?;
    for key in keys {
        let m = message_on(&tx, &key, true)?;
        tx.execute("UPDATE bpmn_messages SET payload_json=NULL,payload_pruned_at_ms=?1,revision=revision+1 WHERE org_id=?2 AND sender_user_id=?3 AND message_id=?4 AND revision=?5",params![at_ms,key.org_id,key.sender_user_id,key.message_id,sql_incrementable(m.revision)?])?;
        message_receipt_on(
            &tx,
            &m,
            "message_payload_pruned",
            serde_json::json!({"message_id":key.message_id,"reason":"payload_retention_elapsed"}),
            at_ms,
        )?;
    }
    tx.commit()?;
    Ok(count)
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
    fn claim_variables_hash_has_fixed_utf8_recursive_key_order() {
        let value = json!({"z":[{"b":2,"a":1}],"a":"ä"});
        assert_eq!(
            expression_variables_hash(&value).unwrap(),
            "734ab3481265d16f414104b6962401ea55b0e5d42a70da42f3d5a65d0d9b198f"
        );
    }

    #[test]
    fn complete_detail_frame_with_eight_maximum_pages_utf8_floats_and_selected_rows_fits_wire_budget(
    ) {
        use tentaflow_protocol::processes::HolidayPolicy;
        let fixture = super::super::runtime::test_support::Fixture::new();
        let mut instance = super::super::runtime::test_support::start_model(
            &fixture,
            &super::super::runtime::test_support::user_model(None),
        );
        let variables = serde_json::json!({"v": vec![0.1_f64; 65_534]});
        validate_variables(&variables).unwrap();
        assert_eq!(serde_json::to_vec(&variables).unwrap().len(), 262_143);
        instance.variables = variables;
        let name = "界".repeat(85) + "x";
        let reason = "界".repeat(170) + "xx";
        let identifier = "i".repeat(128);
        let uuid = "ffffffff-ffff-4fff-8fff-ffffffffffff".to_owned();
        instance.definition_name = name.clone();
        instance.active_node_ids = (0..128)
            .map(|i| format!("N{i:03}{}", "i".repeat(124)))
            .collect();
        instance.message_names = (0..32)
            .map(|i| format!("{i:02}{}xx", "界".repeat(84)))
            .collect();
        let mut task = instance.user_tasks[0].clone();
        task.node_id = identifier.clone();
        task.name = name.clone();
        task.assignee_user_id = "u".repeat(128);
        task.revision = i64::MAX as u64;
        instance.user_tasks = vec![task.clone(); 20];
        let incident = ProcessIncident {
            scope_id: instance.instance_id.clone(),
            incident_id: uuid.clone(),
            node_id: Some(identifier.clone()),
            node_name: Some(name.clone()),
            job_id: Some(uuid.clone()),
            code: "C".repeat(128),
            message: reason.clone(),
            at_ms: i64::MAX,
            can_retry: true,
        };
        instance.incidents = vec![incident.clone(); 20];
        let timer = ProcessTimerSummary {
            scope_id: Some(instance.instance_id.clone()),
            timer_id: uuid.clone(),
            node_id: identifier.clone(),
            node_name: name.clone(),
            kind: ProcessTimerKind::Boundary,
            status: ProcessTimerStatus::Blocked,
            due_at_ms: Some(i64::MAX),
            timezone: "z".repeat(128),
            occurrence: i64::MAX as u64,
            total_firings: Some(u32::MAX),
            last_reason: Some(reason.clone()),
            attached_to_id: Some(identifier.clone()),
            working_time: Some(tentaflow_protocol::processes::ProcessWorkingTimeSummary {
                calendar_name: name.clone(),
                holiday_policy: HolidayPolicy::PolandStatutory,
                pin_sha256: "f".repeat(64),
                legal_release_id: identifier.clone(),
                legal_as_of_date: "2026-10-02".into(),
                tzdb_release_id: identifier.clone(),
                due_offset_seconds: Some(i32::MIN),
            }),
        };
        instance.timers = vec![timer; 20];
        instance.subscriptions = vec![
            ProcessSubscriptionSummary {
                scope_id: instance.instance_id.clone(),
                subscription_id: uuid.clone(),
                node_id: identifier.clone(),
                node_name: name.clone(),
                token_id: uuid.clone(),
                kind: ProcessSubscriptionKind::BoundaryMessage,
                status: ProcessSubscriptionStatus::Error,
                revision: i64::MAX as u64,
                message_name: Some(name.clone()),
                correlation_key: Some(name.clone()),
                error_code: Some("E".repeat(64)),
                escalation_code: None,
                attached_to_id: Some(identifier.clone()),
                race_id: Some(uuid.clone()),
                last_reason: Some(reason.clone())
            };
            20
        ];
        // Charging both full branch arrays exceeds the combined eight-branch graph bound.
        instance.event_races = vec![
            ProcessEventRaceSummary {
                scope_id: instance.instance_id.clone(),
                race_id: uuid.clone(),
                gateway_node_id: identifier.clone(),
                gateway_name: name.clone(),
                status: ProcessEventRaceStatus::Cancelled,
                revision: i64::MAX as u64,
                winner_node_id: Some(identifier.clone()),
                branch_subscription_ids: vec![uuid.clone(); 8],
                branch_timer_ids: vec![uuid.clone(); 8]
            };
            20
        ];
        let outgoing = ProcessMessageSummary {
            source_scope_id: Some(instance.instance_id.clone()),
            message_id: uuid.clone(),
            sender_user_id: "u".repeat(128),
            origin: ProcessMessageOrigin::Process,
            target: ProcessMessageTarget::Catch {
                definition_id: uuid.clone(),
                instance_id: Some(uuid.clone()),
                subscription_id: Some(uuid.clone()),
            },
            message_name: name.clone(),
            correlation_key: name,
            revision: i64::MAX as u64,
            status: ProcessMessageStatus::Ambiguous,
            received_at_ms: i64::MAX,
            expires_at_ms: i64::MAX,
            updated_at_ms: i64::MAX,
            delivered_at_ms: Some(i64::MAX),
            matched_instance_id: Some(uuid.clone()),
            matched_version: Some(u32::MAX),
            matched_subscription_id: Some(uuid.clone()),
            source_instance_id: Some(uuid),
            source_node_id: Some(identifier),
            last_reason: Some(reason),
            payload_sha256: "f".repeat(64),
            payload_bytes: 262_144,
            payload_available: true,
            can_resolve: true,
            can_cancel: true,
        };
        instance.outgoing_messages = vec![outgoing; 20];
        let mut scope = instance.scopes[0].clone();
        scope.parent_scope_id = Some(instance.instance_id.clone());
        scope.subprocess_node_id = Some("s".repeat(128));
        scope.subprocess_node_name = Some("界".repeat(85) + "x");
        scope.parent_token_id = Some(uuid::Uuid::new_v4().to_string());
        scope.scope_id = uuid::Uuid::new_v4().to_string();
        scope.revision = i64::MAX as u64;
        scope.created_at_ms = i64::MAX;
        scope.updated_at_ms = i64::MAX;
        instance.scopes = vec![scope; 20];
        instance.calls = vec![
            ProcessCallSummary::Outgoing {
                call_node_id: "c".repeat(128),
                call_node_name: "界".repeat(85) + "x",
                status: ProcessCallStatus::ReturnIncident,
                child: Some(ProcessRelatedInstance {
                    instance_id: "ffffffff-ffff-4fff-8fff-ffffffffffff".into(),
                    definition_name: "界".repeat(85) + "x",
                    version: u32::MAX,
                    status: ProcessInstanceStatus::Error,
                    can_open: true,
                }),
            };
            20
        ];
        instance.terminal_error = Some(ProcessTerminalError {
            error_ref: "e".repeat(128),
            error_code: "E".repeat(64),
            source_event_id: "ffffffff-ffff-4fff-8fff-ffffffffffff".into(),
            source_node_id: "n".repeat(128),
            source_scope_id: "ffffffff-ffff-4fff-8fff-ffffffffffff".into(),
        });
        let info = ProcessPageInfo {
            offset: u32::MAX - 20,
            total: u32::MAX,
            next_offset: None,
            has_more: false,
        };
        instance.pages = Some(ProcessInstancePageInfo {
            user_tasks: info.clone(),
            incidents: info.clone(),
            timers: info.clone(),
            subscriptions: info.clone(),
            event_races: info.clone(),
            outgoing_messages: info.clone(),
            calls: info.clone(),
            scopes: info,
        });
        instance.selected_user_task = Some(task);
        instance.selected_incident = Some(ProcessIncidentSelection {
            incident,
            resolved_at_ms: Some(i64::MAX),
        });
        for payload in [
            ProcessPayload::InstanceGetResponse {
                instance: instance.clone(),
            },
            ProcessPayload::UserTaskCompleteResponse {
                instance: instance.clone(),
            },
            ProcessPayload::InstanceCancelResponse {
                instance: instance.clone(),
            },
            ProcessPayload::JobRetryResponse {
                instance: instance.clone(),
            },
        ] {
            let body = tentaflow_protocol::message_body::MessageBody::ProcessBody(payload);
            let frame = tentaflow_protocol::cbor::encode(&body).unwrap();
            assert!(
                frame.len() > 580_000,
                "the floating-point CBOR expansion must be measured"
            );
            assert!(
                frame.len() < 900 * 1024,
                "complete typed frame was {} bytes",
                frame.len()
            );
            let decoded: tentaflow_protocol::message_body::MessageBody =
                tentaflow_protocol::cbor::decode(&frame).unwrap();
            assert_eq!(decoded, body);
        }
        ensure_instance_wire_budget(&instance).unwrap();
    }

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
        let (_, timer, _) = get_definition(&db, &actor, &draft.definition_id).unwrap();
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
        let (_, timer, _) = get_definition(&db, &actor, &draft.definition_id).unwrap();
        let due_at = timer.unwrap().due_at_ms.unwrap();
        let candidate = due_timers(&db, due_at, 32).unwrap().remove(0);
        db.write().unwrap().execute(
            "UPDATE user_accounts SET is_active=0 WHERE id=?1", [&actor.user_id],
        ).unwrap();
        let reason = "Owner account in Łódź 🧪 was revoked ".repeat(24);
        assert!(reason.len() > 512 && reason.len() < 32 * 1024);
        assert!(record_timer_blocked(&db, &candidate, &reason, due_at).unwrap());
        db.write()
            .unwrap()
            .execute(
                "UPDATE user_accounts SET is_active=1 WHERE id=?1",
                [&actor.user_id],
            )
            .unwrap();
        let (_, timer, _) = get_definition(&db, &actor, &draft.definition_id).unwrap();
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
        let after = get_instance(&db, &actor, &instance_id, None).unwrap();
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
            &task_snapshot,
            &task_id,
            &json!({}),
            None,
            1_500,
        )
        .unwrap();
        let waiting = complete_user_task(
            &db,
            &participant,
            &stamp("complete before timer"),
            &instance_id,
            &task_id,
            started.revision,
            &json!({}),
            None,
            &completed_plan,
            1_500,
        )
        .unwrap()
        .instance;
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
        let unchanged = get_instance(&db, &actor, &instance_id, None).unwrap();
        assert_eq!(unchanged.revision, waiting.revision);
        assert_eq!(unchanged.timers[0].status, ProcessTimerStatus::Pending);
        let full_reason = "Assignee revoked after Łódź 🧪 snapshot ".repeat(24);
        assert!(full_reason.len() > 512);
        assert!(record_timer_blocked(&db, &due[0], &full_reason, 2_500).unwrap());
        let blocked = get_instance(&db, &actor, &instance_id, None).unwrap();
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
        assert!(get_instance(&db, &unrelated, &instance_id, None).is_err());
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
            get_instance(&db, &participant, &instance_id, None)
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
        .unwrap()
        .instance;
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
        .unwrap()
        .instance;
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
            get_instance(&fixture.db, &fixture.owner, &instance.instance_id, None)
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
            get_instance(&fixture.db, &fixture.owner, &instance.instance_id, None)
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
        let after = get_instance(&fixture.db, &fixture.owner, &instance.instance_id, None).unwrap();
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
    use super::super::runtime::test_support::{stamp, Fixture};
    use super::*;
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
        let (definition, timer, _) =
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
            (stale, old_timer, None)
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
                (expected, original.1, original.2)
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
