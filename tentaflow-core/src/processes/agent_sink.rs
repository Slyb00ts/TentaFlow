// ===== File: agent_sink.rs — the `core.task_complete` sink of agent steps in a process =====
//
// An agent step ends when the agent reports through `core.task_complete`, never
// by what its last message says. The report is written to
// `bpmn_agent_completions`, one final row per invocation. Which invocation a
// call belongs to is not a tool argument: the process layer mints a binding into
// `envelope.meta["process_activity"]` when it spawns the run (as Project Studio
// does with `ps_generation`), so a model cannot report for another activity.

use std::collections::BTreeMap;

use anyhow::Result;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tentaflow_protocol::processes::{ActivityOutcome, ActivityResult};
use uuid::Uuid;

use super::jobs::parse_contract_result;
use super::repository::job_activation_live_on;
use crate::bus::schema_registry::SchemaType;
use crate::db::DbPool;

/// Key of the binding in `FlowEnvelope::meta`.
pub const PROCESS_ACTIVITY_META_KEY: &str = "process_activity";

/// Largest output schema an agent step may carry; larger ones are refused at
/// publication so the sink never compiles an unbounded document per call.
pub const MAX_OUTPUT_SCHEMA_BYTES: usize = 64 * 1024;

const REPORT_KEYS: [&str; 5] = ["outcome", "code", "summary", "outputs", "evidence"];

/// What a run of an agent step is bound to. Everything but `allowed_tools` and
/// `output_schema` identifies the invocation and the attempt the run was
/// spawned for; those two travel with it so the run narrows its tools and
/// checks its outputs without reading the process model again.
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
        serde_json::to_string(&self.allowed_tools).expect("tool names are plain strings")
    }
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

/// Reads the binding from run meta. Absent or malformed means the run was not
/// spawned for a process step, and the sink must refuse.
pub fn binding_from_meta(meta: &BTreeMap<String, Value>) -> Option<ProcessActivityBinding> {
    serde_json::from_value(meta.get(PROCESS_ACTIVITY_META_KEY)?.clone()).ok()
}

/// Why a report could not be recorded.
#[derive(Debug)]
pub enum CompletionError {
    /// The invocation is no longer waiting for this attempt: its phase moved,
    /// the instance or token closed, or a newer attempt owns the job.
    NotLive,
    /// The invocation already has its final report.
    AlreadyCompleted,
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

/// Writes the final report of an invocation. The live check and the insert are
/// one transaction: a report cannot be accepted for a step that closed between
/// the two, and of two simultaneous reports only one gets the row.
fn record_completion(
    pool: &DbPool,
    binding: &ProcessActivityBinding,
    run_id: &str,
    result: &ActivityResult,
    at_ms: i64,
) -> std::result::Result<(), CompletionError> {
    let mut conn = pool.write().map_err(|error| anyhow::anyhow!(error))?;
    let tx = conn.transaction()?;
    let job_id: Option<String> = tx
        .query_row(
            "SELECT v.job_id FROM bpmn_service_invocations v \
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
            |row| row.get(0),
        )
        .optional()?;
    let Some(job_id) = job_id else {
        return Err(CompletionError::NotLive);
    };
    if !job_activation_live_on(&tx, &job_id)? {
        return Err(CompletionError::NotLive);
    }
    let taken: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM bpmn_agent_completions WHERE invocation_id=?1)",
        [&binding.invocation_id],
        |row| row.get(0),
    )?;
    if taken {
        return Err(CompletionError::AlreadyCompleted);
    }
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
/// `result_expression`, and `outputs` of a `completed` report checked against
/// the step's output schema.
fn report_from_arguments(
    arguments: &Value,
    output_schema: Option<&Value>,
) -> std::result::Result<ActivityResult, String> {
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
    let result = parse_contract_result(Value::Object(report))
        .map_err(|error| format!("invalid report: {error:#}"))?;
    if outcome == ActivityOutcome::Completed {
        if let Some(schema) = output_schema {
            check_outputs(schema, &result.outputs)?;
        }
    }
    Ok(result)
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

/// Executes one `core.task_complete` call. `run_id` is the id of the calling
/// run (server-owned meta, never a model argument). An `Err` is the text of a
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
        return Err("this run was not started for the step it reports on".into());
    }
    let result = report_from_arguments(arguments, binding.output_schema.as_ref())?;
    match record_completion(pool, binding, run_id, &result, at_ms) {
        Ok(()) => Ok(json!({ "recorded": true })),
        Err(CompletionError::NotLive) => {
            Err("this step is no longer running, so the report was not recorded".into())
        }
        Err(CompletionError::AlreadyCompleted) => Err(
            "already completed: this step already has its final report; do not call \
             core.task_complete again"
                .into(),
        ),
        Err(CompletionError::Storage(error)) => {
            tracing::error!(invocation_id = %binding.invocation_id, error = %error, "agent completion storage failed");
            Err("the report could not be stored; try once more".into())
        }
    }
}
