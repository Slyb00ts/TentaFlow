// ============ File: runtime.rs — durable process token transitions and worker lifetime ============

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use tentaflow_protocol::processes::{
    ActivityOutcome, ActivityResult, ActivityVerification, ProcessIncident, ProcessInstanceStatus,
    ProcessModel, ProcessNode, ProcessNodeKind, ProcessTimerKind, ProcessTimerStatus,
    ProcessUserTask, ProcessUserTaskKind, ProcessUserTaskStatus,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::model::{and_pairs, validate_variables, MAX_VARIABLE_BYTES, MAX_VARIABLE_KEYS};
use super::repository::{
    AndReceipt, BoundaryTimerIncident, CancelledJobClaim, ForkFrame, PlannedEvent, ProcessActor,
    ProcessJob, ProcessTimer, ProcessToken, RuntimePlan, RuntimeSnapshot,
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
    org_id: &'a str,
    initiator: &'a str,
    definition_id: &'a str,
    version: u32,
    now_ms: i64,
    pairs: HashMap<String, String>,
    tokens: Vec<ProcessToken>,
    jobs: Vec<ProcessJob>,
    tasks: Vec<ProcessUserTask>,
    receipts: Vec<AndReceipt>,
    incidents: Vec<ProcessIncident>,
    timers: Vec<ProcessTimer>,
    boundary_incidents: Vec<BoundaryTimerIncident>,
    plan: RuntimePlan,
}

impl<'a> Transition<'a> {
    fn new(
        model: &'a ProcessModel,
        instance_id: &'a str,
        org_id: &'a str,
        initiator: &'a str,
        definition_id: &'a str,
        version: u32,
        variables: Value,
        now_ms: i64,
    ) -> Result<Self> {
        validate_variables(&variables)?;
        Ok(Self {
            model,
            instance_id,
            org_id,
            initiator,
            definition_id,
            version,
            now_ms,
            pairs: and_pairs(model)?,
            tokens: Vec::new(),
            jobs: Vec::new(),
            tasks: Vec::new(),
            receipts: Vec::new(),
            incidents: Vec::new(),
            timers: Vec::new(),
            boundary_incidents: Vec::new(),
            plan: RuntimePlan::initial(variables),
        })
    }

    fn from_snapshot(snapshot: &'a RuntimeSnapshot, now_ms: i64) -> Result<Self> {
        let mut transition = Self::new(
            &snapshot.model,
            &snapshot.instance.instance_id,
            &snapshot.org_id,
            &snapshot.instance.initiator_user_id,
            &snapshot.instance.definition_id,
            snapshot.instance.version,
            snapshot.instance.variables.clone(),
            now_ms,
        )?;
        transition.tokens = snapshot.tokens.clone();
        transition.jobs = snapshot.jobs.clone();
        transition.tasks = snapshot.user_tasks.clone();
        transition.receipts = snapshot.receipts.clone();
        transition.incidents = snapshot.instance.incidents.clone();
        transition.timers = snapshot.timers.clone();
        transition.boundary_incidents = snapshot.boundary_incidents.clone();
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
            at_ms: self.now_ms,
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
        token_id: &str,
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
            token_id: Some(token_id.to_owned()),
        };
        self.event("user_task_opened", Some(node.id.clone()), json!({"user_task_id": task.user_task_id, "assignee_user_id": assignee, "kind": task.kind}));
        self.tasks.push(task.clone());
        self.plan.create_user_tasks.push(task);
    }

    fn arm_timer(
        &mut self,
        node: &ProcessNode,
        token_id: &str,
        kind: ProcessTimerKind,
        rule: &tentaflow_protocol::processes::ProcessTimerSpec,
    ) -> Result<()> {
        let zone = self
            .model
            .timer_timezone
            .as_deref()
            .context("process timer timezone is missing")?
            .to_owned();
        let due = super::timers::resolve_timer_due(rule, &zone, kind.clone(), self.now_ms);
        let (due_at_ms, status, last_reason, error_reason) = match due {
            Ok(due) => (Some(due), ProcessTimerStatus::Pending, None, None),
            Err(error) if kind == ProcessTimerKind::Boundary => {
                let reason = format!("{error:#}");
                (
                    None,
                    ProcessTimerStatus::Error,
                    Some(super::repository::timer_reason(&reason)?),
                    Some(super::repository::bounded_failure_message(&reason)),
                )
            }
            Err(error) => {
                self.incident(&node.id, None, "TIMER_ERROR", error.to_string());
                return Ok(());
            }
        };
        let timer = ProcessTimer {
            timer_id: Uuid::new_v4().to_string(),
            org_id: self.org_id.to_owned(),
            definition_id: self.definition_id.to_owned(),
            version: self.version,
            node_id: node.id.clone(),
            kind: kind.clone(),
            instance_id: Some(self.instance_id.to_owned()),
            token_id: Some(token_id.to_owned()),
            rule: rule.clone(),
            timezone: zone.to_owned(),
            anchor_at_ms: self.now_ms,
            due_at_ms,
            occurrence: 1,
            total_firings: Some(1),
            revision: 1,
            status: status.clone(),
            last_reason: last_reason.clone(),
            next_check_at_ms: due_at_ms.unwrap_or(self.now_ms),
            created_at_ms: self.now_ms,
            updated_at_ms: self.now_ms,
        };
        let attached_to_id = match &node.kind {
            ProcessNodeKind::BoundaryTimer { attached_to_id, .. } => Some(attached_to_id.as_str()),
            _ => None,
        };
        if status == ProcessTimerStatus::Error {
            let reason = error_reason.context("failed boundary timer has no reason")?;
            self.incident(&node.id, None, "TIMER_ERROR", reason.clone());
            let incident_id = self
                .plan
                .add_incidents
                .last()
                .context("boundary timer failure incident was not created")?
                .incident_id
                .clone();
            self.event("timer_error", Some(node.id.clone()), json!({"kind":kind,"timer_id":timer.timer_id,"attached_to_id":attached_to_id,"attached_token_id":token_id,"incident_id":incident_id,"reason":reason,"due_at_ms":due_at_ms}));
        } else {
            self.event("timer_armed", Some(node.id.clone()), json!({"kind":kind,"timer_id":timer.timer_id,"attached_to_id":attached_to_id,"attached_token_id":if kind == ProcessTimerKind::Boundary {Some(token_id)} else {None},"due_at_ms":due_at_ms,"timezone":zone,"occurrence":1}));
        }
        self.timers.push(timer.clone());
        self.plan.create_timers.push(timer);
        Ok(())
    }

    fn arm_boundaries(&mut self, activity: &ProcessNode, token_id: &str) -> Result<()> {
        let boundaries = self.model.nodes.iter().filter(|node| matches!(&node.kind, ProcessNodeKind::BoundaryTimer { attached_to_id, .. } if attached_to_id == &activity.id)).cloned().collect::<Vec<_>>();
        for node in boundaries {
            let ProcessNodeKind::BoundaryTimer { timer, .. } = &node.kind else {
                anyhow::bail!("boundary catalogue contains another node kind");
            };
            self.arm_timer(&node, token_id, ProcessTimerKind::Boundary, timer)?;
        }
        Ok(())
    }

    fn disarm_boundaries(
        &mut self,
        token_id: &str,
        reason: &str,
        winning_timer_id: Option<&str>,
    ) -> Result<()> {
        let timers = self
            .timers
            .iter()
            .filter(|timer| {
                timer.kind == ProcessTimerKind::Boundary
                    && timer.token_id.as_deref() == Some(token_id)
                    && matches!(
                        timer.status,
                        ProcessTimerStatus::Pending | ProcessTimerStatus::Blocked
                    )
                    && Some(timer.timer_id.as_str()) != winning_timer_id
            })
            .cloned()
            .collect::<Vec<_>>();
        for timer in timers {
            let node = self.node(&timer.node_id)?.clone();
            let ProcessNodeKind::BoundaryTimer { attached_to_id, .. } = &node.kind else {
                anyhow::bail!("boundary timer references a different node kind");
            };
            self.plan
                .timer_updates
                .push(super::repository::TimerUpdate {
                    timer_id: timer.timer_id.clone(),
                    expected_revision: timer.revision,
                    fired_occurrence: None,
                    occurrence: timer.occurrence,
                    due_at_ms: None,
                    status: ProcessTimerStatus::Cancelled,
                    last_reason: Some(reason.to_owned()),
                    next_check_at_ms: self.now_ms,
                });
            self.event("timer_cancelled",Some(node.id.clone()),json!({"kind":"Boundary","timer_id":timer.timer_id,"attached_to_id":attached_to_id,"attached_token_id":token_id,"reason":reason,"winning_timer_id":winning_timer_id}));
        }
        Ok(())
    }

    fn resolve_boundary_incidents(&mut self, token_id: &str) {
        let ids = self
            .boundary_incidents
            .iter()
            .filter(|link| link.token_id == token_id)
            .map(|link| link.incident_id.clone())
            .collect::<Vec<_>>();
        for id in ids {
            self.resolve_incident(&id);
        }
    }

    fn resolve_incident(&mut self, id: &str) {
        self.incidents.retain(|incident| incident.incident_id != id);
        if !self
            .plan
            .resolve_incident_ids
            .iter()
            .any(|existing| existing == id)
        {
            self.plan.resolve_incident_ids.push(id.to_owned());
        }
    }

    fn interrupt_activity(&mut self, token: &ProcessToken, winning_timer_id: &str) -> Result<()> {
        ensure!(
            token.fork_stack.is_empty(),
            "boundary interruption requires an empty parallel activation stack"
        );
        let job_ids = self
            .jobs
            .iter()
            .filter(|job| job.token_id == token.token_id)
            .map(|job| job.job_id.clone())
            .collect::<Vec<_>>();
        self.plan.cancel_job_ids = self
            .jobs
            .iter()
            .filter(|job| {
                job.token_id == token.token_id
                    && matches!(job.status.as_str(), "queued" | "running" | "error")
            })
            .map(|job| job.job_id.clone())
            .collect();
        self.jobs
            .retain(|job| !self.plan.cancel_job_ids.contains(&job.job_id));
        self.plan.cancel_user_task_ids = self
            .tasks
            .iter()
            .filter(|task| {
                task.token_id.as_deref() == Some(token.token_id.as_str())
                    && task.status == ProcessUserTaskStatus::Open
            })
            .map(|task| task.user_task_id.clone())
            .collect();
        self.tasks
            .retain(|task| !self.plan.cancel_user_task_ids.contains(&task.user_task_id));
        self.tokens
            .retain(|existing| existing.token_id != token.token_id);
        self.plan.cancel_token_ids.push(token.token_id.clone());
        self.disarm_boundaries(
            &token.token_id,
            "sibling_interrupted",
            Some(winning_timer_id),
        )?;
        self.resolve_boundary_incidents(&token.token_id);
        let incidents = self
            .incidents
            .iter()
            .filter(|incident| {
                incident
                    .job_id
                    .as_ref()
                    .is_some_and(|job_id| job_ids.contains(job_id))
            })
            .map(|incident| incident.incident_id.clone())
            .collect::<Vec<_>>();
        for id in incidents {
            self.resolve_incident(&id);
        }
        Ok(())
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
                ProcessNodeKind::Start | ProcessNodeKind::TimerStart { .. } => {
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
                    let token_id = self.wait(&token, "waiting");
                    self.user_task(
                        &node,
                        ProcessUserTaskKind::Work,
                        assignee_user_id.unwrap_or_else(|| self.initiator.to_owned()),
                        Value::Null,
                        &token_id,
                    );
                    self.arm_boundaries(&node, &token_id)?;
                }
                ProcessNodeKind::TimerCatch { timer } => {
                    let token_id = self.wait(&token, "waiting");
                    self.arm_timer(&node, &token_id, ProcessTimerKind::Catch, &timer)?;
                }
                ProcessNodeKind::BoundaryTimer { .. } => {
                    anyhow::bail!("boundary events are entered only by their attached timer")
                }
                ProcessNodeKind::ServiceTask { input_mapping, .. } => {
                    match prepare_service_input(&input_mapping, &self.plan.variables) {
                        Ok(input) => {
                            let token_id = self.wait(&token, "waiting");
                            let job = ProcessJob {
                                job_id: Uuid::new_v4().to_string(),
                                instance_id: self.instance_id.to_owned(),
                                node_id: node.id.clone(),
                                token_id: token_id.clone(),
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
                                Some(node.id.clone()),
                                json!({"job_id": job.job_id}),
                            );
                            self.jobs.push(job.clone());
                            self.plan.create_jobs.push(job);
                            self.arm_boundaries(&node, &token_id)?;
                        }
                        Err(error) => {
                            let token_id = self.wait(&token, "waiting");
                            self.arm_boundaries(&node, &token_id)?;
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
        self.plan.status = if !self.incidents.is_empty() || !self.plan.add_incidents.is_empty() {
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

pub enum StartCause {
    Manual,
    Timer { timer_id: String, occurrence: u64 },
}

pub fn plan_start(
    model: &ProcessModel,
    instance_id: &str,
    actor: &ProcessActor,
    definition_id: &str,
    version: u32,
    variables: Value,
    cause: StartCause,
    now_ms: i64,
) -> Result<RuntimePlan> {
    let mut transition = Transition::new(
        model,
        instance_id,
        &actor.org_id,
        &actor.user_id,
        definition_id,
        version,
        variables,
        now_ms,
    )?;
    let start = model
        .nodes
        .iter()
        .find(|node| {
            matches!(
                node.kind,
                ProcessNodeKind::Start | ProcessNodeKind::TimerStart { .. }
            )
        })
        .context("process start event missing")?;
    let facts = match (&start.kind, cause) {
        (ProcessNodeKind::Start, StartCause::Manual) => json!({"initiator_user_id":actor.user_id}),
        (
            ProcessNodeKind::TimerStart { .. },
            StartCause::Timer {
                timer_id,
                occurrence,
            },
        ) => {
            ensure!(
                Uuid::parse_str(&timer_id).is_ok()
                    && occurrence > 0
                    && occurrence <= i64::MAX as u64,
                "scheduled process start identity is invalid"
            );
            json!({"initiator_user_id":actor.user_id,"start_timer_id":timer_id,"start_occurrence":occurrence})
        }
        (ProcessNodeKind::TimerStart { .. }, StartCause::Manual) => {
            anyhow::bail!("a timer-start process cannot be started manually")
        }
        _ => anyhow::bail!("timer firing does not match the process start event"),
    };
    transition.plan.start_instance_id = Some(instance_id.to_owned());
    transition.create_token(ProcessToken {
        token_id: String::new(),
        node_id: start.id.clone(),
        arrival_edge_id: None,
        fork_stack: Vec::new(),
        status: "ready".into(),
    });
    transition.event("instance_started", None, facts);
    transition.advance()?;
    Ok(transition.finish())
}

pub fn plan_advance(snapshot: &RuntimeSnapshot, now_ms: i64) -> Result<RuntimePlan> {
    let mut transition = Transition::from_snapshot(snapshot, now_ms)?;
    transition.advance()?;
    Ok(transition.finish())
}

pub fn plan_user_completion(
    snapshot: &RuntimeSnapshot,
    task_id: &str,
    outputs: &Value,
    approved: Option<bool>,
    now_ms: i64,
) -> Result<RuntimePlan> {
    validate_output(outputs)?;
    let mut transition = Transition::from_snapshot(snapshot, now_ms)?;
    let task = snapshot
        .user_tasks
        .iter()
        .find(|task| task.user_task_id == task_id && task.status == ProcessUserTaskStatus::Open)
        .context("open process user task not found")?;
    let node = transition.node(&task.node_id)?.clone();
    let token = transition
        .tokens
        .iter()
        .find(|token| {
            task.token_id.as_deref() == Some(token.token_id.as_str())
                && token.node_id == node.id
                && token.status == "waiting"
        })
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
    transition.disarm_boundaries(&token.token_id, "activity_completed", None)?;
    transition.resolve_boundary_incidents(&token.token_id);
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
    now_ms: i64,
) -> Result<RuntimePlan> {
    validate_output(&result.outputs)?;
    let mut transition = Transition::from_snapshot(snapshot, now_ms)?;
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
                &token.token_id,
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
                        transition.disarm_boundaries(
                            &token.token_id,
                            "activity_completed",
                            None,
                        )?;
                        transition.resolve_boundary_incidents(&token.token_id);
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

pub(super) fn plan_timer_catch(
    snapshot: &RuntimeSnapshot,
    timer: &ProcessTimer,
    now_ms: i64,
) -> Result<RuntimePlan> {
    ensure!(
        timer.kind == ProcessTimerKind::Catch
            && timer.instance_id.as_deref() == Some(snapshot.instance.instance_id.as_str())
            && timer.definition_id == snapshot.instance.definition_id
            && timer.org_id == snapshot.org_id
            && timer.version == snapshot.instance.version,
        "catch timer does not match its pinned instance"
    );
    let mut transition = Transition::from_snapshot(snapshot, now_ms)?;
    let token_id = timer
        .token_id
        .as_deref()
        .context("catch timer has no waiting token")?;
    let token = transition
        .tokens
        .iter()
        .find(|token| {
            token.token_id == token_id
                && token.node_id == timer.node_id
                && token.status == "waiting"
        })
        .context("catch timer waiting token is missing")?
        .clone();
    ensure!(
        matches!(&transition.node(&timer.node_id)?.kind, ProcessNodeKind::TimerCatch { timer: rule } if *rule == timer.rule),
        "catch timer rule does not match its pinned node"
    );
    transition.consume(token_id);
    for edge in transition.outgoing(&timer.node_id) {
        transition.follow(&token, &edge)?;
    }
    transition.advance()?;
    Ok(transition.finish())
}

pub(super) fn plan_timer_boundary(
    snapshot: &RuntimeSnapshot,
    timer: &ProcessTimer,
    now_ms: i64,
) -> Result<RuntimePlan> {
    ensure!(
        timer.kind == ProcessTimerKind::Boundary
            && timer.instance_id.as_deref() == Some(snapshot.instance.instance_id.as_str())
            && timer.definition_id == snapshot.instance.definition_id
            && timer.org_id == snapshot.org_id
            && timer.version == snapshot.instance.version,
        "boundary timer does not match its pinned instance"
    );
    let mut transition = Transition::from_snapshot(snapshot, now_ms)?;
    let node = transition.node(&timer.node_id)?.clone();
    let ProcessNodeKind::BoundaryTimer {
        attached_to_id,
        cancel_activity,
        timer: rule,
    } = &node.kind
    else {
        anyhow::bail!("timer does not reference a boundary node");
    };
    ensure!(
        rule == &timer.rule,
        "boundary timer rule differs from pinned model"
    );
    let token = transition
        .tokens
        .iter()
        .find(|token| {
            timer.token_id.as_deref() == Some(token.token_id.as_str())
                && token.node_id == *attached_to_id
                && token.status == "waiting"
        })
        .context("boundary attached waiting activation is missing")?
        .clone();
    ensure!(
        token.fork_stack.is_empty(),
        "boundary attachment has an active parallel fork frame"
    );
    if *cancel_activity {
        transition.interrupt_activity(&token, &timer.timer_id)?;
    }
    for edge in transition.outgoing(&node.id) {
        transition.follow(&token, &edge)?;
    }
    transition.advance()?;
    Ok(transition.finish())
}

struct RunningJob {
    instance_id: String,
    attempt: u32,
    fence: u64,
    worker_id: String,
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
    fn signal_cancelled_claims(&self, claims: &[CancelledJobClaim]) {
        for claim in claims {
            if let Some(job) = self.running.get(&claim.job_id) {
                if job.attempt == claim.attempt
                    && job.fence == claim.fence
                    && job.worker_id == claim.worker_id
                {
                    job.cancel.cancel();
                }
            }
        }
    }

    fn remove_running_claim(&self, job_id: &str, attempt: u32, fence: u64, worker_id: &str) {
        self.running.remove_if(job_id, |_, job| {
            job.attempt == attempt && job.fence == fence && job.worker_id == worker_id
        });
    }

    fn handle_timer_drain(&self, drained: super::timers::TimerDrainOutcome) {
        self.signal_cancelled_claims(&drained.cancelled_claims);
        if let Err(error) = drained.completion {
            tracing::error!(error = %error, "process timer drain failed");
        }
    }

    async fn run(self: Arc<Self>) -> Result<()> {
        let mut jobs = tokio::task::JoinSet::new();
        loop {
            if self.stop.is_cancelled() {
                break;
            }
            let drained = super::timers::drain_due(&self.db, chrono::Utc::now().timestamp_millis());
            self.handle_timer_drain(drained);
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
                        attempt: claimed.job.attempt,
                        fence: claimed.job.fence,
                        worker_id: self.worker_id.clone(),
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
                    worker.remove_running_claim(&job_id, attempt, fence, &worker.worker_id);
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
    use tentaflow_protocol::processes::{
        PinnedFlowInfo, ProcessInstance, ProcessSequenceFlow, ProcessVersion,
    };

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

    pub fn with_boundaries(
        mut model: ProcessModel,
        activity: &str,
        boundaries: &[(&str, bool, u32)],
    ) -> ProcessModel {
        model.timer_timezone = Some("UTC".into());
        for (id, interrupt, seconds) in boundaries {
            model.nodes.push(ProcessNode {
                id: (*id).into(),
                name: format!("Boundary {id}"),
                kind: ProcessNodeKind::BoundaryTimer {
                    attached_to_id: activity.into(),
                    cancel_activity: *interrupt,
                    timer: tentaflow_protocol::processes::ProcessTimerSpec::Duration {
                        seconds: *seconds,
                    },
                },
            });
            model
                .sequence_flows
                .push(edge(&format!("From_{id}"), id, "End_1"));
        }
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

    pub fn publish_model(fixture: &Fixture, model: &ProcessModel) -> ProcessVersion {
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
        .expect("publish immutable process")
        .1
    }

    pub fn start_model(fixture: &Fixture, model: &ProcessModel) -> ProcessInstance {
        let version = publish_model(fixture, model);
        let actor = &fixture.owner;
        let id = Uuid::new_v4().to_string();
        let variables = serde_json::to_value(&model.variables).unwrap();
        let at_ms = chrono::Utc::now().timestamp_millis();
        let plan = super::plan_start(
            model,
            &id,
            actor,
            &version.definition_id,
            version.version,
            variables.clone(),
            StartCause::Manual,
            at_ms,
        )
        .expect("plan actual start");
        super::super::repository::start_instance(
            &fixture.db,
            actor,
            &stamp("start"),
            &id,
            &version.definition_id,
            version.version,
            &variables,
            &plan,
            at_ms,
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
        let at_ms = chrono::Utc::now().timestamp_millis();
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
        let plan = plan_user_completion(&snapshot, &task_id, &output, None, at_ms).unwrap();
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
            at_ms,
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
            at_ms,
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
            &plan,
            at_ms
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
        let at_ms = chrono::Utc::now().timestamp_millis();
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
        let plan =
            plan_user_completion(&snapshot, &left.user_task_id, &Value::Null, None, at_ms).unwrap();
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
            at_ms,
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
        let plan = plan_user_completion(
            &snapshot,
            &right.user_task_id,
            &json!(["checked"]),
            None,
            at_ms,
        )
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
            at_ms,
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
            at_ms,
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

    fn registry_runtime(fixture: &Fixture, worker_id: &str) -> ProcessRuntime {
        ProcessRuntime {
            db: fixture.db.clone(),
            dispatcher: Arc::downgrade(fixture.dispatcher()),
            worker_id: worker_id.into(),
            stop: CancellationToken::new(),
            wake: tokio::sync::Notify::new(),
            running: dashmap::DashMap::new(),
            handle: parking_lot::Mutex::new(None),
        }
    }

    fn register_claim(
        runtime: &ProcessRuntime,
        claim: &super::super::repository::ClaimedProcessJob,
        cancel: &CancellationToken,
    ) {
        runtime.running.insert(
            claim.job.job_id.clone(),
            RunningJob {
                instance_id: claim.job.instance_id.clone(),
                attempt: claim.job.attempt,
                fence: claim.job.fence,
                worker_id: claim.job.worker_id.clone().unwrap(),
                cancel: cancel.clone(),
            },
        );
    }

    #[tokio::test]
    async fn boundary_claim_registration_race_never_enters_the_real_flow_executor() {
        let fixture = Fixture::new();
        let flow_id = flow(
            &fixture.db,
            &fixture.owner,
            &graph("must not execute", None),
        );
        let model = with_boundaries(
            service_model(
                &flow_id,
                ActivityVerification::Condition {
                    expression: "true".into(),
                },
            ),
            "Service",
            &[("Limit", true, 1)],
        );
        let started = start_model(&fixture, &model);
        let claim = repository::claim_job(
            &fixture.db,
            "registration-window",
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap()
        .unwrap();
        let due = claim.snapshot.timers[0].due_at_ms.unwrap();
        let runtime = registry_runtime(&fixture, "registration-window");
        let cancel = CancellationToken::new();
        let before_registration = tokio::sync::Barrier::new(2);
        let committed = tokio::sync::Barrier::new(2);
        let firing = async {
            before_registration.wait().await;
            assert!(runtime.running.is_empty());
            let drained = super::super::timers::drain_due(&fixture.db, due);
            assert_eq!(drained.fired, 1);
            assert!(drained.completion.is_ok());
            assert_eq!(drained.cancelled_claims.len(), 1);
            runtime.handle_timer_drain(drained);
            committed.wait().await;
        };
        let registering = async {
            before_registration.wait().await;
            committed.wait().await;
            register_claim(&runtime, &claim, &cancel);
            assert!(!cancel.is_cancelled());
            super::super::jobs::execute_claimed(
                &fixture.db,
                fixture.dispatcher(),
                "registration-window",
                claim.clone(),
                cancel.clone(),
            )
            .await
            .unwrap();
            runtime.remove_running_claim(
                &claim.job.job_id,
                claim.job.attempt,
                claim.job.fence,
                "registration-window",
            );
        };
        tokio::join!(firing, registering);
        assert!(runtime.running.is_empty());
        let actual_effect_count =
            crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 10)
                .unwrap()
                .len();
        assert_eq!(actual_effect_count, 0);
        let snapshot =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        assert_eq!(snapshot.jobs[0].status, "cancelled");
        assert_eq!(snapshot.jobs[0].fence, claim.job.fence + 1);
        assert!(snapshot.jobs[0].result.is_none());
        assert!(!snapshot.instance.can_retry);
    }

    #[tokio::test]
    async fn cancelled_generation_signal_and_old_cleanup_cannot_touch_an_actual_retried_claim() {
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("new generation", None));
        let started = start_model(
            &fixture,
            &service_model(
                &flow_id,
                ActivityVerification::Condition {
                    expression: "true".into(),
                },
            ),
        );
        let old = repository::claim_job(
            &fixture.db,
            "old-worker",
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap()
        .unwrap();
        assert!(repository::fail_job(
            &fixture.db,
            &old.job.job_id,
            old.job.attempt,
            old.job.fence,
            "old-worker",
            "INTERRUPTED",
            "controlled worker interruption",
            chrono::Utc::now().timestamp_millis()
        )
        .unwrap());
        let incident =
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
        repository::retry_job(
            &fixture.db,
            &fixture.owner,
            &stamp("retry real generation"),
            &started.instance_id,
            &old.job.job_id,
            incident.revision,
        )
        .unwrap();
        let new = repository::claim_job(
            &fixture.db,
            "new-worker",
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(old.job.job_id, new.job.job_id);
        assert!(new.job.attempt > old.job.attempt && new.job.fence > old.job.fence);
        let runtime = registry_runtime(&fixture, "new-worker");
        let cancel = CancellationToken::new();
        register_claim(&runtime, &new, &cancel);
        runtime.signal_cancelled_claims(&[CancelledJobClaim {
            job_id: old.job.job_id.clone(),
            attempt: old.job.attempt,
            fence: old.job.fence,
            worker_id: "old-worker".into(),
        }]);
        assert!(!cancel.is_cancelled());
        runtime.remove_running_claim(
            &old.job.job_id,
            old.job.attempt,
            old.job.fence,
            "old-worker",
        );
        assert_eq!(runtime.running.len(), 1);
        super::super::jobs::execute_claimed(
            &fixture.db,
            fixture.dispatcher(),
            "new-worker",
            new.clone(),
            cancel.clone(),
        )
        .await
        .unwrap();
        assert_eq!(
            crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap()
                .status,
            ProcessInstanceStatus::Completed
        );
        runtime.remove_running_claim(
            &new.job.job_id,
            new.job.attempt,
            new.job.fence,
            "new-worker",
        );
        assert!(runtime.running.is_empty());
    }

    #[tokio::test]
    async fn midbatch_sqlite_failure_keeps_prior_committed_cancellation_and_signals_exact_worker() {
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("queued effects", None));
        let mut instances = Vec::new();
        for seconds in [1, 2] {
            let model = with_boundaries(
                service_model(
                    &flow_id,
                    ActivityVerification::Condition {
                        expression: "true".into(),
                    },
                ),
                "Service",
                &[("Limit", true, seconds)],
            );
            instances.push(start_model(&fixture, &model));
        }
        let runtime = registry_runtime(&fixture, "batch-worker");
        let mut claims = Vec::new();
        for _ in 0..2 {
            let claim = repository::claim_job(
                &fixture.db,
                "batch-worker",
                chrono::Utc::now().timestamp_millis(),
            )
            .unwrap()
            .unwrap();
            let cancel = CancellationToken::new();
            register_claim(&runtime, &claim, &cancel);
            claims.push((claim, cancel));
        }
        let first = claims
            .iter()
            .find(|(claim, _)| claim.job.instance_id == instances[0].instance_id)
            .unwrap();
        let second = claims
            .iter()
            .find(|(claim, _)| claim.job.instance_id == instances[1].instance_id)
            .unwrap();
        let fault_id = second.0.snapshot.timers[0].timer_id.clone();
        fixture.db.write().unwrap().execute_batch(&format!("CREATE TRIGGER fail_later_timer BEFORE UPDATE ON bpmn_timers WHEN OLD.timer_id='{}' AND NEW.status IN ('fired','error') BEGIN SELECT RAISE(ABORT,'controlled midbatch SQLite failure'); END;",fault_id)).unwrap();
        let due = second.0.snapshot.timers[0].due_at_ms.unwrap();
        let drained = super::super::timers::drain_due(&fixture.db, due);
        assert_eq!(drained.fired, 1);
        assert_eq!(drained.cancelled_claims.len(), 1);
        assert!(drained
            .completion
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("controlled midbatch SQLite failure"));
        assert_eq!(
            drained.cancelled_claims[0],
            CancelledJobClaim {
                job_id: first.0.job.job_id.clone(),
                attempt: first.0.job.attempt,
                fence: first.0.job.fence,
                worker_id: "batch-worker".into()
            }
        );
        runtime.handle_timer_drain(drained);
        assert!(first.1.is_cancelled());
        assert!(!second.1.is_cancelled());
        let committed =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &first.0.job.instance_id)
                .unwrap();
        let rolled_back =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &second.0.job.instance_id)
                .unwrap();
        assert_eq!(committed.jobs[0].status, "cancelled");
        assert_eq!(committed.jobs[0].fence, first.0.job.fence + 1);
        assert_eq!(rolled_back.jobs[0].status, "running");
        assert_eq!(rolled_back.jobs[0].fence, second.0.job.fence);
        assert_eq!(
            rolled_back.timers[0].status,
            tentaflow_protocol::processes::ProcessTimerStatus::Pending
        );
        assert!(rolled_back.instance.incidents.is_empty());
        assert!(
            crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 10)
                .unwrap()
                .is_empty()
        );
        fixture
            .db
            .write()
            .unwrap()
            .execute_batch("DROP TRIGGER fail_later_timer")
            .unwrap();
        let retried = super::super::timers::drain_due(&fixture.db, due);
        assert_eq!(retried.fired, 1);
        assert!(retried.completion.is_ok());
        runtime.handle_timer_drain(retried);
        assert!(second.1.is_cancelled());
    }

    #[tokio::test]
    async fn boundary_running_executor_receives_only_its_committed_generation_signal() {
        let fixture = Fixture::new();
        let flow_id = flow(
            &fixture.db,
            &fixture.owner,
            &graph("late effect result", Some(2_000)),
        );
        let model = with_boundaries(
            service_model(&flow_id, ActivityVerification::Human),
            "Service",
            &[("Limit", true, 1)],
        );
        let started = start_model(&fixture, &model);
        let claim = repository::claim_job(
            &fixture.db,
            "live-generation",
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap()
        .unwrap();
        let runtime = registry_runtime(&fixture, "live-generation");
        let cancel = CancellationToken::new();
        register_claim(&runtime, &claim, &cancel);
        let execution = super::super::jobs::execute_claimed(
            &fixture.db,
            fixture.dispatcher(),
            "live-generation",
            claim.clone(),
            cancel.clone(),
        );
        let interrupted = async {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if !crate::db::repository::list_flow_executions_for_flow(
                        &fixture.db,
                        &flow_id,
                        10,
                    )
                    .unwrap()
                    .is_empty()
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("the real pinned flow executor started");
            let drained = super::super::timers::drain_due(
                &fixture.db,
                claim.snapshot.timers[0].due_at_ms.unwrap(),
            );
            assert_eq!(drained.fired, 1);
            assert!(drained.completion.is_ok());
            assert_eq!(drained.cancelled_claims.len(), 1);
            runtime.handle_timer_drain(drained);
            assert!(cancel.is_cancelled());
        };
        let (result, ()) = tokio::join!(execution, interrupted);
        result.unwrap();
        let snapshot =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        assert_eq!(snapshot.jobs[0].status, "cancelled");
        assert!(snapshot.jobs[0].result.is_none());
        assert!(snapshot.user_tasks.is_empty() && snapshot.instance.incidents.is_empty());
        assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Completed);
        assert!(!repository::renew_job_lease(
            &fixture.db,
            &claim.job.job_id,
            claim.job.attempt,
            claim.job.fence,
            "live-generation",
            chrono::Utc::now().timestamp_millis()
        )
        .unwrap());
    }
}
