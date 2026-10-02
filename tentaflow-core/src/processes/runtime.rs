// ============ File: runtime.rs — durable B1 token transition planning and worker lifetime ============

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use tentaflow_protocol::processes::{
    ActivityOutcome, ActivityResult, ActivityVerification, ProcessIncident, ProcessInstanceStatus,
    ProcessModel, ProcessNode, ProcessNodeKind, ProcessUserTask, ProcessUserTaskKind,
    ProcessUserTaskStatus,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::model::{and_pairs, validate_variables, MAX_VARIABLE_BYTES, MAX_VARIABLE_KEYS};
use super::repository::{
    AndReceipt, ForkFrame, PlannedEvent, ProcessJob, ProcessToken, RuntimePlan, RuntimeSnapshot,
};
use crate::db::DbPool;
use crate::flow_engine::dispatcher::FlowDispatcher;
use crate::flow_engine::envelope::FlowValue;
use crate::flow_engine::expr::{self, ExprScope};

pub fn validate_output(value: &Value) -> Result<()> {
    ensure!(
        serde_json::to_vec(value)?.len() <= MAX_VARIABLE_BYTES,
        "activity output exceeds the process byte limit"
    );
    if let Some(object) = value.as_object() {
        ensure!(
            object.len() <= MAX_VARIABLE_KEYS,
            "activity output exceeds 128 keys"
        );
    }
    Ok(())
}

fn evaluate(expression: &str, variables: &Value, outputs: &Value) -> Result<Value> {
    let vars = variables
        .as_object()
        .context("process variables must be an object")?
        .iter()
        .map(|(key, value)| (key.clone(), FlowValue::Json(value.clone())))
        .collect::<BTreeMap<_, _>>();
    let payload = FlowValue::Json(variables.clone());
    Ok(expr::evaluate(
        expression,
        &ExprScope {
            vars: &vars,
            payload: &payload,
            artifacts: &HashMap::new(),
            meta: &BTreeMap::new(),
            extras: &[("outputs", outputs.clone())],
        },
        None,
    )?)
}

fn condition(expression: &str, variables: &Value, outputs: &Value) -> Result<bool> {
    evaluate(expression, variables, outputs)?
        .as_bool()
        .context("process condition must evaluate to a boolean")
}

pub fn prepare_service_input(
    mapping: &BTreeMap<String, String>,
    variables: &Value,
) -> Result<Value> {
    let mut payload = variables.clone();
    let mut activity_vars = serde_json::Map::new();
    for (key, expression) in mapping {
        let value = evaluate(expression, variables, &Value::Null)?;
        if key == "payload" {
            payload = value;
        } else {
            activity_vars.insert(key.clone(), value);
        }
    }
    let input = json!({"payload": payload, "variables": activity_vars});
    validate_output(&input)?;
    Ok(input)
}

fn patch_variables(
    mapping: &BTreeMap<String, String>,
    variables: &Value,
    outputs: &Value,
) -> Result<Value> {
    let mut patched = variables
        .as_object()
        .context("process variables must be an object")?
        .clone();
    for (key, expression) in mapping {
        patched.insert(key.clone(), evaluate(expression, variables, outputs)?);
    }
    let value = Value::Object(patched);
    validate_variables(&value)?;
    Ok(value)
}

struct Transition<'a> {
    model: &'a ProcessModel,
    instance_id: &'a str,
    initiator: &'a str,
    pairs: HashMap<String, String>,
    tokens: Vec<ProcessToken>,
    jobs: Vec<ProcessJob>,
    tasks: Vec<ProcessUserTask>,
    receipts: Vec<AndReceipt>,
    existing_incidents: bool,
    plan: RuntimePlan,
}

impl<'a> Transition<'a> {
    fn new(
        model: &'a ProcessModel,
        instance_id: &'a str,
        initiator: &'a str,
        variables: Value,
    ) -> Result<Self> {
        validate_variables(&variables)?;
        Ok(Self {
            model,
            instance_id,
            initiator,
            pairs: and_pairs(model)?,
            tokens: Vec::new(),
            jobs: Vec::new(),
            tasks: Vec::new(),
            receipts: Vec::new(),
            existing_incidents: false,
            plan: RuntimePlan::initial(variables),
        })
    }

    fn from_snapshot(snapshot: &'a RuntimeSnapshot) -> Result<Self> {
        let mut transition = Self::new(
            &snapshot.model,
            &snapshot.instance.instance_id,
            &snapshot.instance.initiator_user_id,
            snapshot.instance.variables.clone(),
        )?;
        transition.tokens = snapshot.tokens.clone();
        transition.jobs = snapshot.jobs.clone();
        transition.tasks = snapshot.user_tasks.clone();
        transition.receipts = snapshot.receipts.clone();
        transition.existing_incidents = !snapshot.instance.incidents.is_empty();
        Ok(transition)
    }

    fn node(&self, id: &str) -> Result<&ProcessNode> {
        self.model
            .nodes
            .iter()
            .find(|node| node.id == id)
            .context("process token references a missing node")
    }

    fn event(&mut self, kind: &str, node_id: Option<String>, data: Value) {
        self.plan.events.push(PlannedEvent {
            kind: kind.into(),
            node_id,
            data,
        });
    }

    fn create_token(&mut self, mut token: ProcessToken) -> String {
        token.token_id = Uuid::new_v4().to_string();
        let id = token.token_id.clone();
        self.tokens.push(token.clone());
        self.plan.create_tokens.push(token);
        id
    }

    fn consume(&mut self, token_id: &str) {
        self.tokens.retain(|token| token.token_id != token_id);
        if self
            .plan
            .create_tokens
            .iter()
            .any(|token| token.token_id == token_id)
        {
            self.plan
                .create_tokens
                .retain(|token| token.token_id != token_id);
        } else if !self.plan.consume_token_ids.iter().any(|id| id == token_id) {
            self.plan.consume_token_ids.push(token_id.to_owned());
        }
    }

    fn wait(&mut self, token: &ProcessToken, status: &str) -> String {
        self.consume(&token.token_id);
        self.create_token(ProcessToken {
            status: status.into(),
            ..token.clone()
        })
    }

    fn follow(&mut self, token: &ProcessToken, edge_id: &str) -> Result<()> {
        let edge = self
            .model
            .sequence_flows
            .iter()
            .find(|flow| flow.id == edge_id)
            .context("sequence flow is missing")?;
        self.create_token(ProcessToken {
            token_id: String::new(),
            node_id: edge.target_id.clone(),
            arrival_edge_id: Some(edge.id.clone()),
            fork_stack: token.fork_stack.clone(),
            status: "ready".into(),
        });
        Ok(())
    }

    fn outgoing(&self, node_id: &str) -> Vec<String> {
        self.model
            .sequence_flows
            .iter()
            .filter(|flow| flow.source_id == node_id)
            .map(|flow| flow.id.clone())
            .collect()
    }

    fn incident(&mut self, node_id: &str, job_id: Option<String>, code: &str, message: String) {
        let node_name = self
            .model
            .nodes
            .iter()
            .find(|node| node.id == node_id)
            .map(|node| node.name.clone());
        self.plan.add_incidents.push(ProcessIncident {
            incident_id: Uuid::new_v4().to_string(),
            node_id: Some(node_id.to_owned()),
            node_name,
            job_id,
            code: code.into(),
            message: message.clone(),
            at_ms: chrono::Utc::now().timestamp_millis(),
            can_retry: false,
        });
        self.event(
            "incident",
            Some(node_id.to_owned()),
            json!({"code": code, "message": message}),
        );
    }

    fn user_task(
        &mut self,
        node: &ProcessNode,
        kind: ProcessUserTaskKind,
        assignee: String,
        outputs: Value,
    ) {
        let task = ProcessUserTask {
            user_task_id: Uuid::new_v4().to_string(),
            node_id: node.id.clone(),
            name: node.name.clone(),
            assignee_user_id: assignee.clone(),
            kind,
            status: ProcessUserTaskStatus::Open,
            outputs,
            revision: 1,
            can_complete: false,
        };
        self.event("user_task_opened", Some(node.id.clone()), json!({"user_task_id": task.user_task_id, "assignee_user_id": assignee, "kind": task.kind}));
        self.tasks.push(task.clone());
        self.plan.create_user_tasks.push(task);
    }

    fn join(&mut self, token: &ProcessToken) -> Result<()> {
        let frame = token
            .fork_stack
            .last()
            .context("parallel join has no fork activation")?
            .clone();
        ensure!(
            frame.join_node_id == token.node_id,
            "parallel join does not match the current fork activation"
        );
        ensure!(
            !self
                .receipts
                .iter()
                .any(|receipt| receipt.activation_id == frame.activation_id
                    && receipt.branch_edge_id == frame.branch_edge_id),
            "parallel branch arrived twice"
        );
        let token_id = self.wait(token, "joining");
        let receipt = AndReceipt {
            join_node_id: token.node_id.clone(),
            activation_id: frame.activation_id.clone(),
            branch_edge_id: frame.branch_edge_id.clone(),
            token_id,
        };
        self.receipts.push(receipt.clone());
        self.plan.add_receipts.push(receipt);
        let arrivals = self
            .receipts
            .iter()
            .filter(|receipt| {
                receipt.activation_id == frame.activation_id
                    && receipt.join_node_id == frame.join_node_id
            })
            .cloned()
            .collect::<Vec<_>>();
        let branches = self.outgoing(&frame.split_node_id);
        if arrivals.len() != branches.len() {
            return Ok(());
        }
        ensure!(
            branches.iter().all(|branch| arrivals
                .iter()
                .any(|receipt| receipt.branch_edge_id == *branch)),
            "parallel receipts do not match the fork branches"
        );
        for receipt in arrivals {
            self.consume(&receipt.token_id);
            self.receipts
                .retain(|existing| existing.token_id != receipt.token_id);
            if self
                .plan
                .add_receipts
                .iter()
                .any(|existing| existing.token_id == receipt.token_id)
            {
                self.plan
                    .add_receipts
                    .retain(|existing| existing.token_id != receipt.token_id);
            } else {
                self.plan.remove_receipts.push(receipt);
            }
        }
        let mut next = token.clone();
        next.fork_stack.pop();
        self.event(
            "parallel_joined",
            Some(token.node_id.clone()),
            json!({"activation_id": frame.activation_id}),
        );
        for edge in self.outgoing(&token.node_id) {
            self.follow(&next, &edge)?;
        }
        Ok(())
    }

    fn advance(&mut self) -> Result<()> {
        while let Some(token) = self
            .tokens
            .iter()
            .find(|token| token.status == "ready")
            .cloned()
        {
            let id = token.token_id.clone();
            let node = self.node(&token.node_id)?.clone();
            let outgoing = self.outgoing(&node.id);
            match node.kind.clone() {
                ProcessNodeKind::Start => {
                    self.consume(&id);
                    self.event("node_completed", Some(node.id.clone()), Value::Null);
                    for edge in outgoing {
                        self.follow(&token, &edge)?;
                    }
                }
                ProcessNodeKind::End => {
                    self.consume(&id);
                    self.event("end_reached", Some(node.id), Value::Null);
                }
                ProcessNodeKind::UserTask {
                    assignee_user_id, ..
                } => {
                    self.wait(&token, "waiting");
                    self.user_task(
                        &node,
                        ProcessUserTaskKind::Work,
                        assignee_user_id.unwrap_or_else(|| self.initiator.to_owned()),
                        Value::Null,
                    );
                }
                ProcessNodeKind::ServiceTask { input_mapping, .. } => {
                    match prepare_service_input(&input_mapping, &self.plan.variables) {
                        Ok(input) => {
                            let token_id = self.wait(&token, "waiting");
                            let job = ProcessJob {
                                job_id: Uuid::new_v4().to_string(),
                                instance_id: self.instance_id.to_owned(),
                                node_id: node.id.clone(),
                                token_id,
                                input,
                                status: "queued".into(),
                                attempt: 0,
                                fence: 0,
                                worker_id: None,
                                lease_until_ms: None,
                                result: None,
                            };
                            self.event(
                                "service_queued",
                                Some(node.id),
                                json!({"job_id": job.job_id}),
                            );
                            self.jobs.push(job.clone());
                            self.plan.create_jobs.push(job);
                        }
                        Err(error) => {
                            self.wait(&token, "waiting");
                            self.incident(&node.id, None, "EXPRESSION_ERROR", error.to_string());
                            break;
                        }
                    }
                }
                ProcessNodeKind::ExclusiveGateway { default_flow_id } => {
                    let mut matches = Vec::new();
                    let mut error = None;
                    for edge_id in &outgoing {
                        if default_flow_id.as_ref() == Some(edge_id) {
                            continue;
                        }
                        let edge = self
                            .model
                            .sequence_flows
                            .iter()
                            .find(|flow| flow.id == *edge_id)
                            .context("sequence flow missing")?;
                        match edge.condition.as_ref().map_or(Ok(true), |expression| {
                            condition(expression, &self.plan.variables, &Value::Null)
                        }) {
                            Ok(true) => matches.push(edge_id.clone()),
                            Ok(false) => {}
                            Err(failure) => {
                                error = Some(failure);
                                break;
                            }
                        }
                    }
                    if let Some(error) = error {
                        self.wait(&token, "waiting");
                        self.incident(&node.id, None, "EXPRESSION_ERROR", error.to_string());
                        break;
                    }
                    let selected = match matches.len() {
                        1 => matches.pop(),
                        0 => default_flow_id,
                        _ => None,
                    };
                    let Some(edge) = selected else {
                        self.wait(&token, "waiting");
                        self.incident(
                            &node.id,
                            None,
                            if matches.len() > 1 {
                                "AMBIGUOUS_GATEWAY"
                            } else {
                                "NO_MATCHING_FLOW"
                            },
                            "exclusive gateway has no unique matching sequence flow".into(),
                        );
                        break;
                    };
                    self.consume(&id);
                    self.event(
                        "exclusive_selected",
                        Some(node.id),
                        json!({"sequence_flow_id": edge}),
                    );
                    self.follow(&token, &edge)?;
                }
                ProcessNodeKind::ParallelGateway => {
                    if let Some(join_node_id) = self.pairs.get(&node.id).cloned() {
                        self.consume(&id);
                        let activation_id = Uuid::new_v4().to_string();
                        self.event(
                            "parallel_split",
                            Some(node.id.clone()),
                            json!({"activation_id": activation_id}),
                        );
                        for edge in outgoing {
                            let mut branch = token.clone();
                            branch.fork_stack.push(ForkFrame {
                                activation_id: activation_id.clone(),
                                split_node_id: node.id.clone(),
                                join_node_id: join_node_id.clone(),
                                branch_edge_id: edge.clone(),
                            });
                            self.follow(&branch, &edge)?;
                        }
                    } else {
                        self.join(&token)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn finish(mut self) -> RuntimePlan {
        self.plan.status = if self.existing_incidents || !self.plan.add_incidents.is_empty() {
            ProcessInstanceStatus::Incident
        } else if self.tokens.is_empty() {
            self.event("instance_completed", None, Value::Null);
            ProcessInstanceStatus::Completed
        } else if self
            .jobs
            .iter()
            .any(|job| matches!(job.status.as_str(), "queued" | "running"))
            || self.tokens.iter().any(|token| token.status == "ready")
        {
            ProcessInstanceStatus::Running
        } else {
            ProcessInstanceStatus::Waiting
        };
        self.plan
    }
}

pub fn plan_start(
    model: &ProcessModel,
    instance_id: &str,
    initiator: &str,
    variables: Value,
) -> Result<RuntimePlan> {
    let mut transition = Transition::new(model, instance_id, initiator, variables)?;
    let start = model
        .nodes
        .iter()
        .find(|node| matches!(node.kind, ProcessNodeKind::Start))
        .context("process start event missing")?;
    transition.create_token(ProcessToken {
        token_id: String::new(),
        node_id: start.id.clone(),
        arrival_edge_id: None,
        fork_stack: Vec::new(),
        status: "ready".into(),
    });
    transition.event(
        "instance_started",
        None,
        json!({"initiator_user_id": initiator}),
    );
    transition.advance()?;
    Ok(transition.finish())
}

pub fn plan_advance(snapshot: &RuntimeSnapshot) -> Result<RuntimePlan> {
    let mut transition = Transition::from_snapshot(snapshot)?;
    transition.advance()?;
    Ok(transition.finish())
}

pub fn plan_user_completion(
    snapshot: &RuntimeSnapshot,
    task_id: &str,
    outputs: &Value,
    approved: Option<bool>,
) -> Result<RuntimePlan> {
    validate_output(outputs)?;
    let mut transition = Transition::from_snapshot(snapshot)?;
    let task = snapshot
        .user_tasks
        .iter()
        .find(|task| task.user_task_id == task_id && task.status == ProcessUserTaskStatus::Open)
        .context("open process user task not found")?;
    let node = transition.node(&task.node_id)?.clone();
    let token = transition
        .tokens
        .iter()
        .find(|token| token.node_id == node.id && token.status == "waiting")
        .context("user task waiting token missing")?
        .clone();
    let effective_outputs = match task.kind {
        ProcessUserTaskKind::Work => {
            ensure!(
                approved.is_none(),
                "approved is only valid for verification tasks"
            );
            outputs.clone()
        }
        ProcessUserTaskKind::Verification => {
            let approve = approved.context("verification requires approved")?;
            if !approve {
                transition
                    .plan
                    .complete_user_task_ids
                    .push(task_id.to_owned());
                transition.tasks.retain(|task| task.user_task_id != task_id);
                let job = transition
                    .jobs
                    .iter()
                    .find(|job| job.token_id == token.token_id)
                    .map(|job| job.job_id.clone());
                transition.incident(
                    &node.id,
                    job,
                    "HUMAN_REJECTED",
                    "the activity result was rejected by its verifier".into(),
                );
                transition.event(
                    "verification_rejected",
                    Some(node.id),
                    json!({"user_task_id": task_id, "outputs": outputs}),
                );
                return Ok(transition.finish());
            }
            let result: ActivityResult = serde_json::from_value(task.outputs.clone())
                .context("verification has no persisted activity result")?;
            result.outputs
        }
    };
    let mapping = match &node.kind {
        ProcessNodeKind::UserTask { output_mapping, .. }
        | ProcessNodeKind::ServiceTask { output_mapping, .. } => output_mapping,
        _ => anyhow::bail!("user task node is not an activity"),
    };
    transition.plan.variables =
        patch_variables(mapping, &transition.plan.variables, &effective_outputs)?;
    transition
        .plan
        .complete_user_task_ids
        .push(task_id.to_owned());
    transition.tasks.retain(|task| task.user_task_id != task_id);
    transition.consume(&token.token_id);
    transition.event(
        if task.kind == ProcessUserTaskKind::Work {
            "user_task_completed"
        } else {
            "verification_approved"
        },
        Some(node.id.clone()),
        json!({"user_task_id": task_id, "outputs": outputs}),
    );
    for edge in transition.outgoing(&node.id) {
        transition.follow(&token, &edge)?;
    }
    transition.advance()?;
    Ok(transition.finish())
}

pub fn plan_job_result(
    snapshot: &RuntimeSnapshot,
    job: &ProcessJob,
    result: &ActivityResult,
) -> Result<RuntimePlan> {
    validate_output(&result.outputs)?;
    let mut transition = Transition::from_snapshot(snapshot)?;
    let node = transition.node(&job.node_id)?.clone();
    let token = transition
        .tokens
        .iter()
        .find(|token| token.token_id == job.token_id && token.status == "waiting")
        .context("service task waiting token missing")?
        .clone();
    let ProcessNodeKind::ServiceTask {
        output_mapping,
        verification,
        ..
    } = &node.kind
    else {
        anyhow::bail!("job node is not a service task");
    };
    transition.plan.complete_job_ids.push(job.job_id.clone());
    transition
        .jobs
        .retain(|existing| existing.job_id != job.job_id);
    transition.event(
        "service_result",
        Some(node.id.clone()),
        serde_json::to_value(result)?,
    );
    if matches!(
        result.outcome,
        ActivityOutcome::Error | ActivityOutcome::Cancelled
    ) {
        transition.incident(
            &node.id,
            Some(job.job_id.clone()),
            result.code.as_deref().unwrap_or("SERVICE_ERROR"),
            result.summary.clone(),
        );
        return Ok(transition.finish());
    }
    match verification {
        ActivityVerification::Human => {
            transition.user_task(
                &node,
                ProcessUserTaskKind::Verification,
                transition.initiator.to_owned(),
                serde_json::to_value(result)?,
            );
        }
        ActivityVerification::Condition { expression } => {
            match condition(expression, &transition.plan.variables, &result.outputs) {
                Ok(true) => match patch_variables(
                    output_mapping,
                    &transition.plan.variables,
                    &result.outputs,
                ) {
                    Ok(variables) => {
                        transition.plan.variables = variables;
                        transition.consume(&token.token_id);
                        transition.event(
                            "verification_passed",
                            Some(node.id.clone()),
                            json!({"expression": expression}),
                        );
                        for edge in transition.outgoing(&node.id) {
                            transition.follow(&token, &edge)?;
                        }
                        transition.advance()?;
                    }
                    Err(error) => transition.incident(
                        &node.id,
                        Some(job.job_id.clone()),
                        "EXPRESSION_ERROR",
                        error.to_string(),
                    ),
                },
                Ok(false) => transition.incident(
                    &node.id,
                    Some(job.job_id.clone()),
                    "VERIFICATION_FAILED",
                    "the configured condition did not accept the activity result".into(),
                ),
                Err(error) => transition.incident(
                    &node.id,
                    Some(job.job_id.clone()),
                    "EXPRESSION_ERROR",
                    error.to_string(),
                ),
            }
        }
    }
    Ok(transition.finish())
}

struct RunningJob {
    instance_id: String,
    cancel: CancellationToken,
}

pub struct ProcessRuntime {
    db: DbPool,
    dispatcher: Weak<FlowDispatcher>,
    worker_id: String,
    stop: CancellationToken,
    wake: tokio::sync::Notify,
    running: dashmap::DashMap<String, RunningJob>,
    handle: parking_lot::Mutex<Option<tokio::task::JoinHandle<Result<()>>>>,
}

pub fn start(db: &DbPool, dispatcher: &Arc<FlowDispatcher>) -> Result<Arc<ProcessRuntime>> {
    ensure!(
        Arc::ptr_eq(db, dispatcher.process_database()),
        "process worker database does not match its flow dispatcher"
    );
    tokio::runtime::Handle::try_current()
        .context("process worker requires the server async runtime")?;
    let mut slot = dispatcher.process_runtime.lock();
    if let Some(runtime) = slot.as_ref() {
        ensure!(
            !runtime.stop.is_cancelled(),
            "process worker was already stopped"
        );
        return Ok(runtime.clone());
    }
    super::repository::recover_jobs(db, None, chrono::Utc::now().timestamp_millis())?;
    let runtime = Arc::new(ProcessRuntime {
        db: db.clone(),
        dispatcher: Arc::downgrade(dispatcher),
        worker_id: Uuid::new_v4().to_string(),
        stop: CancellationToken::new(),
        wake: tokio::sync::Notify::new(),
        running: dashmap::DashMap::new(),
        handle: parking_lot::Mutex::new(None),
    });
    let worker = runtime.clone();
    *runtime.handle.lock() = Some(tokio::spawn(async move { worker.run().await }));
    *slot = Some(runtime.clone());
    Ok(runtime)
}

pub fn wake(dispatcher: &FlowDispatcher) {
    if let Some(runtime) = dispatcher.process_runtime.lock().as_ref() {
        runtime.wake.notify_one();
    }
}

pub fn cancel_instance(dispatcher: &FlowDispatcher, instance_id: &str) {
    if let Some(runtime) = dispatcher.process_runtime.lock().as_ref() {
        for job in runtime.running.iter() {
            if job.instance_id == instance_id {
                job.cancel.cancel();
            }
        }
        runtime.wake.notify_one();
    }
}

pub async fn stop(dispatcher: &FlowDispatcher) -> Result<()> {
    let runtime = dispatcher.process_runtime.lock().clone();
    if let Some(runtime) = runtime {
        runtime.shutdown().await?;
    }
    Ok(())
}

impl ProcessRuntime {
    async fn run(self: Arc<Self>) -> Result<()> {
        let mut jobs = tokio::task::JoinSet::new();
        loop {
            if self.stop.is_cancelled() {
                break;
            }
            while jobs.len() < 4 && !self.stop.is_cancelled() {
                let Some(dispatcher) = self.dispatcher.upgrade() else {
                    self.stop.cancel();
                    break;
                };
                let claimed = match super::repository::claim_job(
                    &self.db,
                    &self.worker_id,
                    chrono::Utc::now().timestamp_millis(),
                ) {
                    Ok(Some(claimed)) => claimed,
                    Ok(None) => break,
                    Err(error) => {
                        tracing::error!(error = %error, "process job claim failed");
                        break;
                    }
                };
                let job_id = claimed.job.job_id.clone();
                let cancel = self.stop.child_token();
                self.running.insert(
                    job_id.clone(),
                    RunningJob {
                        instance_id: claimed.job.instance_id.clone(),
                        cancel: cancel.clone(),
                    },
                );
                let worker = self.clone();
                jobs.spawn(async move {
                    use futures::FutureExt;

                    let attempt = claimed.job.attempt;
                    let fence = claimed.job.fence;
                    let result = std::panic::AssertUnwindSafe(super::jobs::execute_claimed(
                        &worker.db,
                        &dispatcher,
                        &worker.worker_id,
                        claimed,
                        cancel,
                    )).catch_unwind().await;
                    worker.running.remove(&job_id);
                    let failure = match result {
                        Ok(Ok(())) => None,
                        Ok(Err(error)) => Some(error.to_string()),
                        Err(_) => Some("the service executor panicked before confirming its external effect".into()),
                    };
                    if let Some(error) = failure {
                        tracing::error!(job_id, error, "process job execution failed");
                        if let Err(error) = super::repository::fail_job(&worker.db, &job_id, attempt, fence, &worker.worker_id, "WORKER_ERROR", &error, chrono::Utc::now().timestamp_millis()) {
                            tracing::error!(job_id, error = %error, "process worker could not persist its incident");
                        }
                    }
                });
            }
            tokio::select! {
                _ = self.stop.cancelled() => break,
                joined = jobs.join_next(), if !jobs.is_empty() => {
                    if let Some(Err(error)) = joined { tracing::error!(error = %error, "process job worker panicked"); }
                }
                _ = self.wake.notified() => {},
                _ = tokio::time::sleep(Duration::from_millis(250)) => {
                    if self.dispatcher.upgrade().is_none() { break; }
                }
            }
        }
        self.stop.cancel();
        for job in self.running.iter() {
            job.cancel.cancel();
        }
        let drain = async { while jobs.join_next().await.is_some() {} };
        if tokio::time::timeout(Duration::from_secs(5), drain)
            .await
            .is_err()
        {
            jobs.abort_all();
            while jobs.join_next().await.is_some() {}
        }
        super::repository::recover_jobs(
            &self.db,
            Some(&self.worker_id),
            chrono::Utc::now().timestamp_millis(),
        )?;
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.stop.cancel();
        self.wake.notify_one();
        let handle = self.handle.lock().take();
        if let Some(handle) = handle {
            handle.await.context("process worker shutdown failed")??;
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::config::RouterConfig;
    use crate::db::{self, models::FlowParams};
    use crate::routing::Router;
    use tentaflow_protocol::processes::{PinnedFlowInfo, ProcessInstance, ProcessSequenceFlow};

    pub struct Fixture {
        pub directory: tempfile::TempDir,
        pub db: DbPool,
        pub router: Arc<Router>,
        pub owner: super::super::repository::ProcessActor,
        pub participant: super::super::repository::ProcessActor,
    }

    pub fn actor(pool: &DbPool, name: &str) -> super::super::repository::ProcessActor {
        let id = db::repository::create_user_account(
            pool,
            name,
            "test-password-hash",
            name,
            &format!("{name}@example.test"),
        )
        .expect("create active process user");
        crate::services::org::repo::add_membership(
            pool,
            crate::services::org::DEFAULT_ORG_ID,
            &id,
            "role-org-viewer",
            &id,
        )
        .expect("grant actual organization membership");
        super::super::repository::ProcessActor {
            org_id: crate::services::org::DEFAULT_ORG_ID.into(),
            user_id: id,
        }
    }

    impl Fixture {
        pub fn new() -> Self {
            let directory = tempfile::tempdir().expect("process test directory");
            let db = db::init(&directory.path().join("processes.db")).expect("migrate process DB");
            let owner = actor(&db, "process-owner");
            let participant = actor(&db, "process-participant");
            let router = Arc::new(
                Router::new(RouterConfig::default(), Some(db.clone())).expect("real flow router"),
            );
            Self {
                directory,
                db,
                router,
                owner,
                participant,
            }
        }

        pub fn dispatcher(&self) -> &Arc<FlowDispatcher> {
            self.router
                .flow_dispatcher()
                .expect("actual flow dispatcher")
        }
    }

    pub fn stamp(label: &str) -> super::super::repository::CommandStamp {
        super::super::repository::CommandStamp {
            command_id: Uuid::new_v4().to_string(),
            request_hash: super::super::repository::request_hash(&label).expect("hash command"),
        }
    }

    pub fn edge(id: &str, source: &str, target: &str) -> ProcessSequenceFlow {
        ProcessSequenceFlow {
            id: id.into(),
            source_id: source.into(),
            target_id: target.into(),
            condition: None,
        }
    }

    pub fn user_model(assignee: Option<&str>) -> ProcessModel {
        let mut model = super::super::model::starter_model();
        model.nodes.insert(
            1,
            ProcessNode {
                id: "Work".into(),
                name: "Review the evidence".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: assignee.map(str::to_owned),
                    output_mapping: BTreeMap::from([("answer".into(), "outputs.answer".into())]),
                },
            },
        );
        model.sequence_flows = vec![
            edge("ToWork", "Start_1", "Work"),
            edge("ToEnd", "Work", "End_1"),
        ];
        model
    }

    pub fn service_model(flow_id: &str, verification: ActivityVerification) -> ProcessModel {
        let mut model = super::super::model::starter_model();
        model.nodes.insert(
            1,
            ProcessNode {
                id: "Service".into(),
                name: "Process evidence".into(),
                kind: ProcessNodeKind::ServiceTask {
                    flow_id: flow_id.into(),
                    input_mapping: BTreeMap::new(),
                    output_mapping: BTreeMap::from([(
                        "answer".into(),
                        "outputs.variables.marker".into(),
                    )]),
                    verification,
                    timeout_seconds: 10,
                },
            },
        );
        model.sequence_flows = vec![
            edge("ToService", "Start_1", "Service"),
            edge("ToEnd", "Service", "End_1"),
        ];
        model
    }

    pub fn graph(marker: &str, delay_ms: Option<u64>) -> String {
        let mut nodes = vec![
            json!({"id":"trigger","type":"trigger","config":{"output_mapping":{"marker":serde_json::to_string(marker).unwrap()}}}),
        ];
        let edges = if let Some(delay) = delay_ms {
            nodes.push(
                json!({"id":"delay","type":"interval","config":{"seconds":delay as f64 / 1000.0}}),
            );
            vec![
                json!({"from":"trigger","to":"delay","from_port":"text","to_port":"in"}),
                json!({"from":"delay","to":"output","from_port":"full","to_port":"text"}),
            ]
        } else {
            vec![json!({"from":"trigger","to":"output","from_port":"text","to_port":"text"})]
        };
        nodes.push(json!({"id":"output","type":"output","config":{}}));
        json!({"nodes":nodes,"edges":edges,"variables":[{"name":"marker","type":"text"}]})
            .to_string()
    }

    pub fn flow(
        pool: &DbPool,
        actor: &super::super::repository::ProcessActor,
        graph: &str,
    ) -> String {
        db::repository::create_flow(
            pool,
            &FlowParams {
                name: "Process service",
                description: None,
                is_default: false,
                service_type: None,
                flow_json: graph,
                status: "active",
                published_model_name: None,
                actor_user_id: Some(&actor.user_id),
            },
        )
        .expect("persist actual service flow")
    }

    pub fn update_flow(
        pool: &DbPool,
        actor: &super::super::repository::ProcessActor,
        flow_id: &str,
        graph: &str,
    ) {
        let version = db::repository::get_flow(pool, flow_id)
            .unwrap()
            .unwrap()
            .version;
        db::repository::update_flow(
            pool,
            flow_id,
            version,
            &FlowParams {
                name: "Process service edited",
                description: None,
                is_default: false,
                service_type: None,
                flow_json: graph,
                status: "active",
                published_model_name: None,
                actor_user_id: Some(&actor.user_id),
            },
        )
        .expect("edit actual service graph");
    }

    pub fn start_model(fixture: &Fixture, model: &ProcessModel) -> ProcessInstance {
        let actor = &fixture.owner;
        let definition = super::super::repository::save_definition(
            &fixture.db,
            actor,
            &stamp("create"),
            None,
            0,
            "Evidence process",
            "",
            model,
        )
        .expect("save real process definition");
        let mut snapshots = Vec::new();
        for node in &model.nodes {
            if let ProcessNodeKind::ServiceTask { flow_id, .. } = &node.kind {
                let dispatcher = fixture.dispatcher();
                let meta = dispatcher
                    .authorize_process_flow(flow_id, &actor.user_id, &actor.org_id)
                    .expect("current source authorization");
                let pinned = dispatcher
                    .snapshot_flow(flow_id, &meta)
                    .expect("validate real service graph");
                snapshots.push(super::super::repository::PinnedServiceSnapshot {
                    info: PinnedFlowInfo {
                        node_id: node.id.clone(),
                        flow_id: pinned.flow_id,
                        source_version: pinned.source_version,
                        graph_sha256: pinned.graph_sha256,
                    },
                    graph_json: pinned.graph_json,
                });
            }
        }
        super::super::repository::publish_definition(
            &fixture.db,
            actor,
            &stamp("publish"),
            &definition.definition_id,
            definition.draft_revision,
            &snapshots,
        )
        .expect("publish immutable process");
        let id = Uuid::new_v4().to_string();
        let variables = serde_json::to_value(&model.variables).unwrap();
        let plan = super::plan_start(model, &id, &actor.user_id, variables.clone())
            .expect("plan actual start");
        super::super::repository::start_instance(
            &fixture.db,
            actor,
            &stamp("start"),
            &id,
            &definition.definition_id,
            1,
            &variables,
            &plan,
        )
        .expect("commit process start")
    }

    pub async fn wait_for_status(
        pool: &DbPool,
        actor: &super::super::repository::ProcessActor,
        instance_id: &str,
        status: ProcessInstanceStatus,
    ) -> ProcessInstance {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let instance = super::super::repository::get_instance(pool, actor, instance_id)
                    .expect("read current process");
                if instance.status == status {
                    return instance;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("actual worker reaches expected status")
    }
}

#[cfg(test)]
mod tests {
    use super::super::repository;
    use super::*;
    use test_support::*;

    #[tokio::test]
    async fn user_wait_survives_reopen_and_completion_is_idempotent() {
        let fixture = Fixture::new();
        let waiting = start_model(&fixture, &user_model(Some(&fixture.participant.user_id)));
        assert_eq!(waiting.status, ProcessInstanceStatus::Waiting);
        let task_id = waiting.user_tasks[0].user_task_id.clone();
        let participant = fixture.participant.clone();
        let owner = fixture.owner.clone();
        let path = fixture.directory.path().join("processes.db");
        let Fixture {
            directory,
            db,
            router,
            ..
        } = fixture;
        drop(router);
        drop(db);
        let reopened = crate::db::init(&path).expect("reopen durable process DB");
        assert_eq!(
            repository::recover_jobs(&reopened, None, chrono::Utc::now().timestamp_millis())
                .unwrap(),
            0
        );
        let snapshot =
            repository::runtime_snapshot(&reopened, &participant, &waiting.instance_id).unwrap();
        assert_eq!(snapshot.instance.user_tasks[0].user_task_id, task_id);
        assert!(snapshot.instance.user_tasks[0].can_complete);
        assert!(!snapshot.instance.can_cancel);
        let output = json!({"answer":"reviewed"});
        let plan = plan_user_completion(&snapshot, &task_id, &output, None).unwrap();
        let completion = stamp("complete evidence");
        let completed = repository::complete_user_task(
            &reopened,
            &participant,
            &completion,
            &waiting.instance_id,
            &task_id,
            waiting.revision,
            &output,
            None,
            &plan,
        )
        .unwrap();
        assert_eq!(completed.status, ProcessInstanceStatus::Completed);
        assert_eq!(completed.variables["answer"], "reviewed");
        let events = repository::list_events(&reopened, &owner, &waiting.instance_id, 0, 200)
            .unwrap()
            .0;
        let replay = repository::complete_user_task(
            &reopened,
            &participant,
            &completion,
            &waiting.instance_id,
            &task_id,
            waiting.revision,
            &output,
            None,
            &plan,
        )
        .unwrap();
        assert_eq!(replay.instance_id, completed.instance_id);
        assert_eq!(
            repository::list_events(&reopened, &owner, &waiting.instance_id, 0, 200)
                .unwrap()
                .0,
            events
        );
        let conflicting = repository::CommandStamp {
            request_hash: "different payload".into(),
            ..completion
        };
        assert!(repository::complete_user_task(
            &reopened,
            &participant,
            &conflicting,
            &waiting.instance_id,
            &task_id,
            waiting.revision,
            &output,
            None,
            &plan
        )
        .is_err());
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "instance_completed")
                .count(),
            1
        );
        drop(reopened);
        drop(directory);
    }

    #[tokio::test]
    async fn parallel_join_retains_activation_across_reopen_and_advances_once() {
        let fixture = Fixture::new();
        let mut model = super::super::model::starter_model();
        model.nodes.splice(
            1..1,
            [
                ProcessNode {
                    id: "Split".into(),
                    name: "Parallel".into(),
                    kind: ProcessNodeKind::ParallelGateway,
                },
                ProcessNode {
                    id: "Left".into(),
                    name: "First review".into(),
                    kind: ProcessNodeKind::UserTask {
                        assignee_user_id: None,
                        output_mapping: BTreeMap::new(),
                    },
                },
                ProcessNode {
                    id: "Right".into(),
                    name: "Second review".into(),
                    kind: ProcessNodeKind::UserTask {
                        assignee_user_id: Some(fixture.participant.user_id.clone()),
                        output_mapping: BTreeMap::new(),
                    },
                },
                ProcessNode {
                    id: "Join".into(),
                    name: "All reviews".into(),
                    kind: ProcessNodeKind::ParallelGateway,
                },
            ],
        );
        model.sequence_flows = vec![
            edge("StartSplit", "Start_1", "Split"),
            edge("SplitLeft", "Split", "Left"),
            edge("SplitRight", "Split", "Right"),
            edge("LeftJoin", "Left", "Join"),
            edge("RightJoin", "Right", "Join"),
            edge("JoinEnd", "Join", "End_1"),
        ];
        let waiting = start_model(&fixture, &model);
        assert_eq!(waiting.user_tasks.len(), 2);
        let left = waiting
            .user_tasks
            .iter()
            .find(|task| task.node_id == "Left")
            .unwrap();
        let snapshot =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
                .unwrap();
        let plan = plan_user_completion(&snapshot, &left.user_task_id, &Value::Null, None).unwrap();
        let partial = repository::complete_user_task(
            &fixture.db,
            &fixture.owner,
            &stamp("left done"),
            &waiting.instance_id,
            &left.user_task_id,
            waiting.revision,
            &Value::Null,
            None,
            &plan,
        )
        .unwrap();
        assert_eq!(partial.status, ProcessInstanceStatus::Waiting);
        let path = fixture.directory.path().join("processes.db");
        let owner = fixture.owner.clone();
        let participant = fixture.participant.clone();
        let Fixture {
            directory,
            db,
            router,
            ..
        } = fixture;
        drop(router);
        drop(db);
        let reopened = crate::db::init(&path).unwrap();
        let snapshot =
            repository::runtime_snapshot(&reopened, &participant, &waiting.instance_id).unwrap();
        assert_eq!(snapshot.receipts.len(), 1);
        assert_eq!(snapshot.receipts[0].branch_edge_id, "SplitLeft");
        let right = snapshot
            .instance
            .user_tasks
            .iter()
            .find(|task| task.node_id == "Right" && task.status == ProcessUserTaskStatus::Open)
            .unwrap();
        let plan = plan_user_completion(&snapshot, &right.user_task_id, &json!(["checked"]), None)
            .unwrap();
        let completion = stamp("right done");
        let completed = repository::complete_user_task(
            &reopened,
            &participant,
            &completion,
            &waiting.instance_id,
            &right.user_task_id,
            partial.revision,
            &json!(["checked"]),
            None,
            &plan,
        )
        .unwrap();
        assert_eq!(completed.status, ProcessInstanceStatus::Completed);
        repository::complete_user_task(
            &reopened,
            &participant,
            &completion,
            &waiting.instance_id,
            &right.user_task_id,
            partial.revision,
            &json!(["checked"]),
            None,
            &plan,
        )
        .unwrap();
        let snapshot =
            repository::runtime_snapshot(&reopened, &owner, &waiting.instance_id).unwrap();
        assert!(snapshot.tokens.is_empty());
        assert!(snapshot.receipts.is_empty());
        let events = repository::list_events(&reopened, &owner, &waiting.instance_id, 0, 200)
            .unwrap()
            .0;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "parallel_joined")
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "end_reached")
                .count(),
            1
        );
        drop(reopened);
        drop(directory);
    }

    #[tokio::test]
    async fn xor_uses_real_cel_and_records_ambiguity_and_non_boolean_incidents() {
        let fixture = Fixture::new();
        for (first, second, default, expected) in [
            ("false", "false", Some("Fallback"), None),
            ("true", "true", None, Some("AMBIGUOUS_GATEWAY")),
            ("1", "false", None, Some("EXPRESSION_ERROR")),
            ("false", "false", None, Some("NO_MATCHING_FLOW")),
        ] {
            let mut model = super::super::model::starter_model();
            model.nodes.insert(
                1,
                ProcessNode {
                    id: "Choice".into(),
                    name: "Route evidence".into(),
                    kind: ProcessNodeKind::ExclusiveGateway {
                        default_flow_id: default.map(str::to_owned),
                    },
                },
            );
            model.nodes.push(ProcessNode {
                id: "OtherEnd".into(),
                name: "Alternative".into(),
                kind: ProcessNodeKind::End,
            });
            model.sequence_flows = vec![
                edge("ToChoice", "Start_1", "Choice"),
                edge("Primary", "Choice", "End_1"),
                edge("Fallback", "Choice", "OtherEnd"),
            ];
            model.sequence_flows[1].condition = Some(first.into());
            model.sequence_flows[2].condition = if default == Some("Fallback") {
                None
            } else {
                Some(second.into())
            };
            let instance = start_model(&fixture, &model);
            if let Some(code) = expected {
                assert_eq!(instance.status, ProcessInstanceStatus::Incident);
                assert_eq!(instance.incidents[0].code, code);
                assert_eq!(instance.active_node_ids, vec!["Choice"]);
            } else {
                assert_eq!(instance.status, ProcessInstanceStatus::Completed);
                let events = repository::list_events(
                    &fixture.db,
                    &fixture.owner,
                    &instance.instance_id,
                    0,
                    200,
                )
                .unwrap()
                .0;
                assert!(events.iter().any(|event| event.kind == "exclusive_selected"
                    && event.data["sequence_flow_id"] == "Fallback"));
            }
        }
    }

    #[tokio::test]
    async fn boot_claims_real_queued_service_once_and_shutdown_is_dispatcher_scoped() {
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("pinned", None));
        let queued = start_model(
            &fixture,
            &service_model(
                &flow_id,
                ActivityVerification::Condition {
                    expression: "outputs.variables.marker == 'pinned'".into(),
                },
            ),
        );
        let worker = start(&fixture.db, fixture.dispatcher()).unwrap();
        assert!(Arc::ptr_eq(
            &worker,
            &start(&fixture.db, fixture.dispatcher()).unwrap()
        ));
        let completed = wait_for_status(
            &fixture.db,
            &fixture.owner,
            &queued.instance_id,
            ProcessInstanceStatus::Completed,
        )
        .await;
        assert_eq!(completed.variables["answer"], "pinned");
        stop(fixture.dispatcher()).await.unwrap();
        let events =
            repository::list_events(&fixture.db, &fixture.owner, &queued.instance_id, 0, 200)
                .unwrap()
                .0;
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "service_claimed")
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "service_result")
                .count(),
            1
        );
        assert!(start(&fixture.db, fixture.dispatcher()).is_err());
        let other = Fixture::new();
        let other_worker = start(&other.db, other.dispatcher()).unwrap();
        assert!(!Arc::ptr_eq(&worker, &other_worker));
        other_worker.shutdown().await.unwrap();
    }
}
