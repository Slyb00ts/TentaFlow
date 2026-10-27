// ===== File: agent_sink.rs — the `core.task_complete` sink of agent steps in a process =====
//
// An agent step ends when the agent reports through `core.task_complete`, never
// by what its last message says. The report is written to
// `bpmn_agent_completions`, one final row per invocation. Which invocation a
// call belongs to is not a tool argument: the process layer mints a binding into
// `envelope.meta["process_activity"]` when it spawns the run (as Project Studio
// does with `ps_generation`), so a model cannot report for another activity.
//
// Meta is writable by every node of a flow (an addon block deserializes the whole
// envelope), so the binding only says "this run claims a step". What the sink
// believes comes from the database: the reporting run is the one the flow engine
// is executing (`ExecutionContext::request_id`, never meta), and its `agent_runs`
// row must be the running, top-level run of the step's pinned agent that was
// started for this very invocation.

use std::collections::BTreeMap;

use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tentaflow_protocol::processes::{parse_agent_source_ref, ActivityOutcome, ActivityResult};
use uuid::Uuid;

use super::jobs::parse_contract_result;
use super::repository::job_activation_live_on;
use crate::bus::schema_registry::SchemaType;
use crate::db::DbPool;

/// Key of the binding in `FlowEnvelope::meta`.
pub const PROCESS_ACTIVITY_META_KEY: &str = "process_activity";

/// Key of the narrowing-only marker a nested agent inherits from a run bound to
/// a process step: the step's tool list and nothing else. Unlike the binding it
/// carries no claim, so it narrows the nested run's tools and delegation without
/// opening `core.task_complete`.
pub const PROCESS_STEP_TOOLS_META_KEY: &str = "process_step_tools";

/// What the process model says about the step an agent run was started for. It
/// is read from the database by run id, never from run meta: meta is writable by
/// every node of the harness, so a step's tool list, iteration budget and output
/// schema must not depend on it surviving.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentStepContract {
    pub binding: ProcessActivityBinding,
    pub max_iterations: u32,
}

/// The contract of the step `run_id` was spawned for, `None` when the run is not
/// one (no row, not top-level, not the run id derived from its invocation, or
/// not the pinned agent of that invocation).
pub fn step_contract(pool: &DbPool, run_id: &str) -> Result<Option<AgentStepContract>> {
    let conn = pool.read()?;
    super::repository::agent_step_contract_for_run_on(&conn, run_id)
}

/// The iteration budget the process model gives the step `run_id` belongs to.
/// A budget in run meta would be one `meta.remove` away from the agent's own,
/// larger one.
pub fn step_iteration_cap(pool: &DbPool, run_id: &str) -> Result<Option<i64>> {
    Ok(step_contract(pool, run_id)?.map(|contract| i64::from(contract.max_iterations)))
}

/// Run meta as the process layer vouches for it: for a run that really belongs to
/// a step, the step's tool list comes from the process model whatever meta says;
/// any other run keeps its meta.
///
/// `nested` is `ExecutionContext::nested_agent`. A nested agent shares the step
/// run's id, so the id alone would hand it the step's binding (and with it
/// `core.task_complete`); it gets the narrowing marker instead, never the claim.
///
/// A step whose contract cannot be read gets an unreadable entry, which admits
/// no tool at all. The read can fail for reasons that have nothing to do with a
/// step, so only a run whose meta claims one fails closed; any other run keeps
/// its meta rather than losing its tools to a transient database error.
pub fn sealed_meta(
    pool: &DbPool,
    run_id: &str,
    nested: bool,
    meta: &BTreeMap<String, Value>,
) -> BTreeMap<String, Value> {
    let mut sealed = meta.clone();
    match step_contract(pool, run_id) {
        Ok(None) => {}
        Ok(Some(contract)) if nested => {
            sealed.remove(PROCESS_ACTIVITY_META_KEY);
            sealed.insert(
                PROCESS_STEP_TOOLS_META_KEY.to_owned(),
                json!(contract.binding.allowed_tools),
            );
        }
        Ok(Some(contract)) => {
            let (key, value) = contract.binding.meta_entry();
            sealed.insert(key.to_owned(), value);
        }
        Err(error) => {
            tracing::error!(run_id, error = %error, "cannot read the contract of an agent step");
            if meta.contains_key(PROCESS_ACTIVITY_META_KEY) && !nested {
                sealed.insert(PROCESS_ACTIVITY_META_KEY.to_owned(), Value::Null);
            } else if meta.contains_key(PROCESS_ACTIVITY_META_KEY)
                || meta.contains_key(PROCESS_STEP_TOOLS_META_KEY)
            {
                sealed.remove(PROCESS_ACTIVITY_META_KEY);
                sealed.insert(PROCESS_STEP_TOOLS_META_KEY.to_owned(), Value::Null);
            }
        }
    }
    sealed
}

/// Largest output schema an agent step may carry; larger ones are refused at
/// publication so the sink never compiles an unbounded document per call.
pub const MAX_OUTPUT_SCHEMA_BYTES: usize = 64 * 1024;

const NOT_THIS_RUN: &str = "this run was not started for the step it reports on";

const REPORT_KEYS: [&str; 5] = ["outcome", "code", "summary", "outputs", "evidence"];

/// What a run of an agent step is bound to. Everything but `allowed_tools` and
/// `output_schema` identifies the invocation and the attempt the run was
/// spawned for. Those two travel with it as a convenience for the spawn; every
/// reader that acts on them takes them from the process model by run id
/// ([`sealed_meta`], [`step_contract`]), because meta can be rewritten.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcessActivityBinding {
    pub invocation_id: String,
    pub instance_id: String,
    pub node_id: String,
    pub attempt: u32,
    pub fence: u64,
    pub allowed_tools: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
}

impl ProcessActivityBinding {
    /// The `extra_meta` entry to hand to `AgentRunManager::spawn_with_run_id`.
    pub fn meta_entry(&self) -> (&'static str, Value) {
        (
            PROCESS_ACTIVITY_META_KEY,
            serde_json::to_value(self).expect("a binding is plain JSON"),
        )
    }

    /// The run id this invocation is allowed to report from.
    pub fn agent_run_id(&self) -> String {
        agent_run_id(&self.invocation_id)
    }

    /// The agent's own allow-list shaped as a node allow-list, for the
    /// intersection the tool catalog and the dispatch gate apply.
    pub fn allowed_tools_json(&self) -> String {
        allow_list_json(&self.allowed_tools)
    }
}

/// A tool list shaped as a node allow-list.
pub fn allow_list_json(tools: &[String]) -> String {
    serde_json::to_string(tools).expect("tool names are plain strings")
}

/// Run id of the agent run an invocation spawns. Derived from the invocation
/// identity (instance, activity and attempt are already part of it), so a
/// re-spawn of the same invocation cannot mint a second run and the sink can
/// tell the run that reports from a run it never created.
pub fn agent_run_id(invocation_id: &str) -> String {
    const NAMESPACE: Uuid = Uuid::from_bytes([
        0x7b, 0x3a, 0x51, 0xd0, 0x2c, 0x84, 0x4f, 0x6e, 0x9a, 0x17, 0xe5, 0x08, 0xc3, 0x61, 0x4d,
        0x92,
    ]);
    Uuid::new_v5(&NAMESPACE, invocation_id.as_bytes()).to_string()
}

/// What run meta says about a process step. A malformed entry is its own case:
/// it must narrow a run to nothing, never read as "no step" and hand back the
/// agent's full tool list.
#[derive(Debug, Clone, PartialEq)]
pub enum MetaBinding {
    Absent,
    Valid(ProcessActivityBinding),
    Malformed,
}

pub fn binding_in_meta(meta: &BTreeMap<String, Value>) -> MetaBinding {
    match meta.get(PROCESS_ACTIVITY_META_KEY) {
        None => MetaBinding::Absent,
        Some(value) => match serde_json::from_value(value.clone()) {
            Ok(binding) => MetaBinding::Valid(binding),
            Err(_) => MetaBinding::Malformed,
        },
    }
}

/// Reads the binding from run meta. Absent or malformed means the run was not
/// spawned for a process step, and the sink must refuse.
pub fn binding_from_meta(meta: &BTreeMap<String, Value>) -> Option<ProcessActivityBinding> {
    match binding_in_meta(meta) {
        MetaBinding::Valid(binding) => Some(binding),
        MetaBinding::Absent | MetaBinding::Malformed => None,
    }
}

/// The tool list a nested agent inherits from a run bound to a process step, as
/// the marker that narrows without claiming the step.
pub fn step_tools_marker(meta: &BTreeMap<String, Value>) -> Option<(&'static str, Value)> {
    step_tool_list(meta).map(|tools| (PROCESS_STEP_TOOLS_META_KEY, json!(tools)))
}

/// The tools a run may hand on to a sub-agent besides the agent's own limit:
/// `None` outside a process step, the step's list inside one (its own binding or
/// the marker inherited from the run that owns the step), nothing at all for an
/// entry that cannot be read.
pub fn step_tool_list(meta: &BTreeMap<String, Value>) -> Option<Vec<String>> {
    match binding_in_meta(meta) {
        MetaBinding::Valid(binding) => Some(binding.allowed_tools),
        MetaBinding::Malformed => Some(Vec::new()),
        MetaBinding::Absent => meta
            .get(PROCESS_STEP_TOOLS_META_KEY)
            .map(|value| serde_json::from_value::<Vec<String>>(value.clone()).unwrap_or_default()),
    }
}

/// Why a report could not be recorded.
#[derive(Debug)]
pub enum CompletionError {
    /// The invocation is no longer waiting for this attempt: its phase moved,
    /// the instance or token closed, or a newer attempt owns the job.
    NotLive,
    /// The invocation already has its final report.
    AlreadyCompleted,
    /// The calling run is not the running, top-level run that was started for
    /// this invocation of its pinned agent.
    NotThisRun,
    /// The report does not fit the step's output schema, as the process model
    /// (not run meta) states it.
    Invalid(String),
    Storage(anyhow::Error),
}

impl From<rusqlite::Error> for CompletionError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error.into())
    }
}

impl From<anyhow::Error> for CompletionError {
    fn from(error: anyhow::Error) -> Self {
        Self::Storage(error)
    }
}

/// Writes the final report of an invocation. The live check, the check of the
/// reporting run, the check of the outputs and the insert are one transaction: a
/// report cannot be accepted for a step that closed between them, and of two
/// simultaneous reports only one gets the row. Nothing the run claims in meta is
/// taken on trust here: the run row must exist, be running and top-level, belong
/// to the agent the invocation pinned, and carry this invocation as its
/// correlation id, and the output schema is the one of the pinned process model.
fn record_completion(
    pool: &DbPool,
    binding: &ProcessActivityBinding,
    run_id: &str,
    result: &ActivityResult,
    at_ms: i64,
) -> std::result::Result<(), CompletionError> {
    let mut conn = pool.write().map_err(|error| anyhow::anyhow!(error))?;
    let tx = conn.transaction()?;
    let live: Option<(String, String)> = tx
        .query_row(
            "SELECT v.job_id, v.flow_id FROM bpmn_service_invocations v \
             JOIN bpmn_jobs j ON j.job_id=v.job_id \
             WHERE v.invocation_id=?1 AND v.instance_id=?2 AND v.node_id=?3 \
               AND v.implementation_kind='agent' AND v.phase='may_have_executed' \
               AND v.dispatch_evidence='committed_boundary' \
               AND v.dispatch_attempt=?4 AND v.dispatch_fence=?5 \
               AND j.status='running' AND j.attempt=?4 AND j.fence=?5",
            params![
                binding.invocation_id,
                binding.instance_id,
                binding.node_id,
                binding.attempt,
                i64::try_from(binding.fence).map_err(|error| anyhow::anyhow!(error))?
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((job_id, source_ref)) = live else {
        return Err(CompletionError::NotLive);
    };
    if !job_activation_live_on(&tx, &job_id)? {
        return Err(CompletionError::NotLive);
    }
    let pinned_agent = parse_agent_source_ref(&source_ref);
    let run_matches: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_runs WHERE id=?1 AND agent_id=?2 \
         AND status='running' AND parent_run_id IS NULL AND correlation_id=?3)",
        params![run_id, pinned_agent, binding.invocation_id],
        |row| row.get(0),
    )?;
    if !run_matches {
        return Err(CompletionError::NotThisRun);
    }
    let taken: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM bpmn_agent_completions WHERE invocation_id=?1)",
        [&binding.invocation_id],
        |row| row.get(0),
    )?;
    if taken {
        return Err(CompletionError::AlreadyCompleted);
    }
    let contract = super::repository::agent_step_contract_for_run_on(&tx, run_id)?
        .ok_or(CompletionError::NotThisRun)?;
    check_report_outputs(result, contract.binding.output_schema.as_ref())
        .map_err(CompletionError::Invalid)?;
    tx.execute(
        "INSERT INTO bpmn_agent_completions(invocation_id,run_id,result_json,created_at_ms) \
         VALUES(?1,?2,?3,?4)",
        params![
            binding.invocation_id,
            run_id,
            serde_json::to_string(result).map_err(|error| anyhow::anyhow!(error))?,
            at_ms
        ],
    )?;
    tx.commit()?;
    Ok(())
}

/// The final report an invocation recorded, if any. This row, never the run's
/// last message, is what an agent step ended with: a run can finish with a
/// confident answer and no report.
pub fn recorded_completion(
    conn: &rusqlite::Connection,
    invocation_id: &str,
) -> Result<Option<ActivityResult>> {
    let stored: Option<String> = conn
        .query_row(
            "SELECT result_json FROM bpmn_agent_completions WHERE invocation_id=?1",
            [invocation_id],
            |row| row.get(0),
        )
        .optional()?;
    stored
        .map(|json| Ok(serde_json::from_str(&json)?))
        .transpose()
}

fn parse_outcome(value: &Value) -> Option<ActivityOutcome> {
    match value.as_str()? {
        "completed" => Some(ActivityOutcome::Completed),
        "error" => Some(ActivityOutcome::Error),
        "needs_human" => Some(ActivityOutcome::NeedsHuman),
        _ => None,
    }
}

/// Turns the tool arguments into a bounded `ActivityResult`: exactly the five
/// report keys of a business result, with the same size and code limits as a
/// `result_expression`. Whether `outputs` fit the step's schema is checked
/// against the process model in the recording transaction.
fn report_from_arguments(arguments: &Value) -> std::result::Result<ActivityResult, String> {
    let object = arguments
        .as_object()
        .ok_or("the report must be a JSON object")?;
    if let Some(unknown) = object
        .keys()
        .find(|key| !REPORT_KEYS.contains(&key.as_str()))
    {
        return Err(format!(
            "unknown report field '{unknown}' (allowed: outcome, code, summary, outputs, evidence)"
        ));
    }
    let outcome = object
        .get("outcome")
        .and_then(parse_outcome)
        .ok_or("'outcome' must be one of completed, error, needs_human")?;
    if !object.get("summary").is_some_and(Value::is_string) {
        return Err("'summary' is required and must be a string".into());
    }
    let mut report = Map::new();
    report.insert(
        "outcome".into(),
        serde_json::to_value(&outcome).map_err(|error| error.to_string())?,
    );
    report.insert(
        "code".into(),
        object.get("code").cloned().unwrap_or(Value::Null),
    );
    report.insert("summary".into(), object["summary"].clone());
    report.insert(
        "outputs".into(),
        object.get("outputs").cloned().unwrap_or_else(|| json!({})),
    );
    report.insert(
        "evidence".into(),
        object.get("evidence").cloned().unwrap_or_else(|| json!([])),
    );
    parse_contract_result(Value::Object(report))
        .map_err(|error| format!("invalid report: {error:#}"))
}

/// The `outputs` of a `completed` report against the step's output schema.
pub(super) fn check_report_outputs(
    result: &ActivityResult,
    output_schema: Option<&Value>,
) -> std::result::Result<(), String> {
    match output_schema {
        Some(schema) if result.outcome == ActivityOutcome::Completed => {
            check_outputs(schema, &result.outputs)
        }
        _ => Ok(()),
    }
}

fn check_outputs(schema: &Value, outputs: &Value) -> std::result::Result<(), String> {
    let ops = SchemaType::JsonSchema.ops();
    let compiled = ops
        .compile(&schema.to_string())
        .map_err(|error| format!("the step's output schema cannot be applied: {error}"))?;
    let bytes = serde_json::to_vec(outputs).map_err(|error| error.to_string())?;
    ops.validate(&compiled, &bytes)
        .map_err(|error| format!("'outputs' do not match the output schema: {error}"))
}

/// Checks that an output schema can be applied, so a step that could never
/// report `completed` is refused at publication instead of at the end of a run.
pub fn validate_output_schema(schema: &Value) -> Result<()> {
    anyhow::ensure!(
        schema.is_object(),
        "the output schema must be a JSON object"
    );
    anyhow::ensure!(
        schema.to_string().len() <= MAX_OUTPUT_SCHEMA_BYTES,
        "the output schema exceeds {MAX_OUTPUT_SCHEMA_BYTES} bytes"
    );
    SchemaType::JsonSchema
        .ops()
        .compile(&schema.to_string())
        .map(|_| ())
        .map_err(|error| anyhow::anyhow!("the output schema is not supported: {error}"))
}

/// Executes one `core.task_complete` call. `run_id` is the id of the run the
/// flow engine is executing (`ExecutionContext::request_id`; never meta and never
/// a model argument). An `Err` is the text of a
/// `[TOOL_ERROR]` the model can act on; nothing about the process leaks into it
/// beyond what the model itself sent.
pub fn complete_task(
    pool: &DbPool,
    binding: &ProcessActivityBinding,
    run_id: &str,
    arguments: &Value,
    at_ms: i64,
) -> std::result::Result<Value, String> {
    if run_id != binding.agent_run_id() {
        return Err(NOT_THIS_RUN.into());
    }
    let result = report_from_arguments(arguments)?;
    match record_completion(pool, binding, run_id, &result, at_ms) {
        Ok(()) => Ok(json!({ "recorded": true })),
        Err(CompletionError::NotThisRun) => Err(NOT_THIS_RUN.into()),
        Err(CompletionError::NotLive) => {
            Err("this step is no longer running, so the report was not recorded".into())
        }
        Err(CompletionError::AlreadyCompleted) => Err(
            "already completed: this step already has its final report; do not call \
             core.task_complete again"
                .into(),
        ),
        Err(CompletionError::Invalid(reason)) => Err(reason),
        Err(CompletionError::Storage(error)) => {
            tracing::error!(invocation_id = %binding.invocation_id, error = %error, "agent completion storage failed");
            Err("the report could not be stored; try once more".into())
        }
    }
}
