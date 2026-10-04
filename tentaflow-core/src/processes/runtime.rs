// ============ File: runtime.rs — durable process token transitions and worker lifetime ============

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Weak};
use std::time::Duration;

use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use tentaflow_protocol::processes::{
    ActivityOutcome, ActivityResult, ActivityVerification, ProcessIncident, ProcessInstanceStatus,
    ProcessModel, ProcessNode, ProcessNodeKind, ProcessScopeSummary, ProcessTimerKind,
    ProcessTimerStatus, ProcessUserTask, ProcessUserTaskKind, ProcessUserTaskStatus,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::model::{gateway_pairs, validate_variables, GatewayKind, MAX_VARIABLE_BYTES, MAX_VARIABLE_KEYS};
use super::repository::{
    GatewayReceipt, BoundaryEventIncident, CancelledJobClaim, ForkFrame, PlannedEvent, PlannedScope,
    ProcessActor, ProcessJob, ProcessTimer, ProcessToken, RuntimePlan, RuntimeSnapshot,
    ScopeUpdate,
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

pub(super) fn evaluate(
    expression: &str,
    variables: &Value,
    outputs: &Value,
    extra: &[(String, Value)],
) -> Result<Value> {
    let vars = variables
        .as_object()
        .context("process variables must be an object")?
        .iter()
        .map(|(key, value)| (key.clone(), FlowValue::Json(value.clone())))
        .collect::<BTreeMap<_, _>>();
    let payload = FlowValue::Json(variables.clone());
    let mut extras = Vec::with_capacity(extra.len() + 1);
    extras.push(("outputs", outputs.clone()));
    extras.extend(
        extra
            .iter()
            .map(|(name, value)| (name.as_str(), value.clone())),
    );
    Ok(expr::evaluate(
        expression,
        &ExprScope {
            vars: &vars,
            payload: &payload,
            artifacts: &HashMap::new(),
            meta: &BTreeMap::new(),
            extras: &extras,
        },
        None,
    )?)
}

fn condition(expression: &str, variables: &Value, outputs: &Value) -> Result<bool> {
    evaluate(expression, variables, outputs, &[])?
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
        let value = evaluate(expression, variables, &Value::Null, &[])?;
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

pub(super) fn patch_variables(
    mapping: &BTreeMap<String, String>,
    local: &Value,
    effective: &Value,
    outputs: &Value,
    extra: &[(String, Value)],
) -> Result<Value> {
    let mut patched = local
        .as_object()
        .context("process variables must be an object")?
        .clone();
    for (key, expression) in mapping {
        patched.insert(
            key.clone(),
            evaluate(expression, effective, outputs, extra)?,
        );
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
    current_scope: String,
    scopes: Vec<ProcessScopeSummary>,
    retained_scope_count: usize,
    scope_variables: BTreeMap<String, Value>,
    tokens: Vec<ProcessToken>,
    jobs: Vec<ProcessJob>,
    tasks: Vec<ProcessUserTask>,
    receipts: Vec<GatewayReceipt>,
    incidents: Vec<ProcessIncident>,
    timers: Vec<ProcessTimer>,
    boundary_incidents: Vec<BoundaryEventIncident>,
    subscriptions: Vec<super::repository::EventSubscription>,
    event_races: Vec<super::repository::EventRace>,
    escalation_continuation: bool,
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
            current_scope: instance_id.to_owned(),
            scopes: vec![ProcessScopeSummary {
                scope_id: instance_id.to_owned(),
                parent_scope_id: None,
                subprocess_node_id: None,
                subprocess_node_name: None,
                terminal_error: None,
                parent_token_id: None,
                revision: 1,
                status: ProcessInstanceStatus::Running,
                depth: 0,
                created_at_ms: now_ms,
                updated_at_ms: now_ms,
            }],
            retained_scope_count: 1,
            scope_variables: BTreeMap::new(),
            tokens: Vec::new(),
            jobs: Vec::new(),
            tasks: Vec::new(),
            receipts: Vec::new(),
            incidents: Vec::new(),
            timers: Vec::new(),
            boundary_incidents: Vec::new(),
            subscriptions: Vec::new(),
            event_races: Vec::new(),
            escalation_continuation: false,
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
        transition.scopes = snapshot.scopes.clone();
        transition.retained_scope_count = snapshot.retained_scope_count;
        transition.scope_variables = snapshot.scope_variables.clone();
        transition.jobs = snapshot.jobs.clone();
        transition.tasks = snapshot.user_tasks.clone();
        transition.receipts = snapshot.receipts.clone();
        transition.incidents = snapshot.incidents.clone();
        transition.timers = snapshot.timers.clone();
        transition.boundary_incidents = snapshot.boundary_incidents.clone();
        transition.subscriptions = snapshot.subscriptions.clone();
        transition.event_races = snapshot.event_races.clone();
        Ok(transition)
    }

    fn node(&self, id: &str) -> Result<&ProcessNode> {
        super::repository::scope_node(
            self.model,
            &self.scopes,
            self.instance_id,
            &self.current_scope,
            id,
        )
    }

    fn body(
        &self,
    ) -> Result<(
        &[ProcessNode],
        &[tentaflow_protocol::processes::ProcessSequenceFlow],
    )> {
        let path =
            super::repository::scope_path(&self.scopes, self.instance_id, &self.current_scope)?;
        let (nodes, flows, _) = super::model::scope_body(self.model, &path)?;
        Ok((nodes, flows))
    }

    fn local(&self) -> Result<&Value> {
        if self.current_scope == self.instance_id {
            Ok(&self.plan.variables)
        } else {
            self.scope_variables
                .get(&self.current_scope)
                .context("active scope local variables are missing")
        }
    }

    fn effective(&self) -> Result<Value> {
        super::repository::effective_scope_variables(
            &self.scopes,
            &self.scope_variables,
            self.instance_id,
            &self.plan.variables,
            &self.current_scope,
        )
    }

    fn set_variables(&mut self, value: Value) -> Result<()> {
        validate_variables(&value)?;
        if self.current_scope == self.instance_id {
            self.plan.variables = value;
        } else {
            self.scope_variables
                .insert(self.current_scope.clone(), value.clone());
            let scope = self
                .scopes
                .iter()
                .find(|scope| scope.scope_id == self.current_scope)
                .context("current child scope is missing")?
                .clone();
            self.update_scope(&scope.scope_id, scope.status, Some(value))?;
        }
        Ok(())
    }

    fn update_scope(
        &mut self,
        id: &str,
        status: ProcessInstanceStatus,
        variables: Option<Value>,
    ) -> Result<()> {
        let scope = self
            .scopes
            .iter_mut()
            .find(|scope| scope.scope_id == id && scope.parent_scope_id.is_some())
            .context("child scope update references a missing child")?;
        scope.status = status.clone();
        if let Some(update) = self
            .plan
            .scope_updates
            .iter_mut()
            .find(|update| update.scope_id == id)
        {
            update.status = status;
            if variables.is_some() {
                update.variables = variables;
            }
        } else {
            self.plan.scope_updates.push(ScopeUpdate {
                scope_id: id.to_owned(),
                expected_revision: scope.revision,
                status,
                variables,
            });
        }
        Ok(())
    }

    fn map_outputs(
        &mut self,
        mapping: &BTreeMap<String, String>,
        outputs: &Value,
        extra: &[(String, Value)],
    ) -> Result<()> {
        self.set_variables(patch_variables(
            mapping,
            self.local()?,
            &self.effective()?,
            outputs,
            extra,
        )?)
    }

    fn event(&mut self, kind: &str, node_id: Option<String>, data: Value) {
        self.plan.events.push(PlannedEvent {
            scope_id: self.current_scope.clone(),
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
        if !self.plan.consume_token_ids.iter().any(|id| id == token_id) {
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
            .body()?
            .1
            .iter()
            .find(|flow| flow.id == edge_id)
            .context("sequence flow is missing")?;
        self.create_token(ProcessToken {
            token_id: String::new(),
            scope_id: self.current_scope.clone(),
            node_id: edge.target_id.clone(),
            arrival_edge_id: Some(edge.id.clone()),
            fork_stack: token.fork_stack.clone(),
            status: "ready".into(),
        });
        Ok(())
    }

    fn outgoing(&self, node_id: &str) -> Vec<String> {
        self.body()
            .expect("current scope body was validated")
            .1
            .iter()
            .filter(|flow| flow.source_id == node_id)
            .map(|flow| flow.id.clone())
            .collect()
    }

    fn incident(&mut self, node_id: &str, job_id: Option<String>, code: &str, message: String) {
        let node_name = self.node(node_id).ok().map(|node| node.name.clone());
        self.plan.add_incidents.push(ProcessIncident {
            scope_id: self.current_scope.clone(),
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
            scope_id: self.current_scope.clone(),
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
        let due = super::timers::resolve_timer_due(
            rule,
            &zone,
            kind.clone(),
            self.now_ms,
            self.model.calendar_pin.as_ref(),
        );
        let (due_at_ms, status, last_reason, error_reason) = match due {
            Ok(due) => (Some(due), ProcessTimerStatus::Pending, None, None),
            Err(error)
                if kind == ProcessTimerKind::Boundary
                    || matches!(
                        rule,
                        tentaflow_protocol::processes::ProcessTimerSpec::WorkingDuration { .. }
                    ) =>
            {
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
            scope_id: Some(self.current_scope.clone()),
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
            race_id: None,
        };
        let attached_to_id = match &node.kind {
            ProcessNodeKind::BoundaryTimer { attached_to_id, .. } => Some(attached_to_id.as_str()),
            _ => None,
        };
        if status == ProcessTimerStatus::Error {
            let reason = error_reason.context("failed timer has no reason")?;
            self.incident(&node.id, None, "TIMER_ERROR", reason.clone());
            let incident_id = self
                .plan
                .add_incidents
                .last()
                .context("timer failure incident was not created")?
                .incident_id
                .clone();
            self.event("timer_error", Some(node.id.clone()), json!({"kind":kind,"timer_id":timer.timer_id,"attached_to_id":attached_to_id,"attached_token_id":token_id,"incident_id":incident_id,"reason":reason,"due_at_ms":due_at_ms}));
        } else {
            self.event("timer_armed", Some(node.id.clone()), json!({"kind":kind,"timer_id":timer.timer_id,"attached_to_id":attached_to_id,"attached_token_id":if kind == ProcessTimerKind::Boundary {Some(token_id)} else {None},"due_at_ms":due_at_ms,"timezone":zone,"occurrence":1}));
        }
        if let Some(working_time) = super::calendar::working_time_summary(
            rule,
            self.model.calendar_pin.as_ref(),
            due_at_ms,
        )? {
            let event = self
                .plan
                .events
                .last_mut()
                .context("armed timer lacks its event")?;
            event.data["working_time"] = serde_json::to_value(working_time)?;
            event.data["timezone"] = json!(zone);
        }
        self.timers.push(timer.clone());
        self.plan.create_timers.push(timer);
        Ok(())
    }

    fn arm_boundaries(&mut self, activity: &ProcessNode, token_id: &str) -> Result<()> {
        let nodes = self
            .body()?
            .0
            .iter()
            .filter(|n| match &n.kind {
                ProcessNodeKind::BoundaryTimer { attached_to_id, .. }
                | ProcessNodeKind::BoundaryMessage { attached_to_id, .. }
                | ProcessNodeKind::BoundaryError { attached_to_id, .. }
                | ProcessNodeKind::BoundaryEscalation { attached_to_id, .. } => {
                    attached_to_id == &activity.id
                }
                _ => false,
            })
            .cloned()
            .collect::<Vec<_>>();
        for node in nodes {
            match &node.kind {
                ProcessNodeKind::BoundaryTimer { timer, .. } => {
                    self.arm_timer(&node, token_id, ProcessTimerKind::Boundary, timer)?
                }
                _ => self.arm_subscription(&node, token_id, None)?,
            }
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
        for subscription in self
            .subscriptions
            .iter()
            .filter(|s| {
                s.token_id == token_id
                    && s.kind
                        != tentaflow_protocol::processes::ProcessSubscriptionKind::MessageCatch
                    && s.status == tentaflow_protocol::processes::ProcessSubscriptionStatus::Open
                    && Some(s.subscription_id.as_str()) != winning_timer_id
            })
            .cloned()
            .collect::<Vec<_>>()
        {
            self.settle_subscription(
                &subscription,
                tentaflow_protocol::processes::ProcessSubscriptionStatus::Cancelled,
                Some(reason),
            );
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
        if let Some(child) = self
            .scopes
            .iter()
            .find(|scope| scope.parent_token_id.as_deref() == Some(token.token_id.as_str()))
            .cloned()
        {
            self.cancel_scope(&child.scope_id, winning_timer_id)?;
        }
        let job_ids = self
            .jobs
            .iter()
            .filter(|job| job.token_id == token.token_id)
            .map(|job| job.job_id.clone())
            .collect::<Vec<_>>();
        let cancelled_jobs = self
            .jobs
            .iter()
            .filter(|job| {
                job.token_id == token.token_id
                    && matches!(job.status.as_str(), "queued" | "running" | "error")
            })
            .map(|job| job.job_id.clone())
            .collect::<Vec<_>>();
        self.plan.cancel_job_ids.extend(cancelled_jobs);
        self.jobs
            .retain(|job| !self.plan.cancel_job_ids.contains(&job.job_id));
        let cancelled_tasks = self
            .tasks
            .iter()
            .filter(|task| {
                task.token_id.as_deref() == Some(token.token_id.as_str())
                    && task.status == ProcessUserTaskStatus::Open
            })
            .map(|task| task.user_task_id.clone())
            .collect::<Vec<_>>();
        self.plan.cancel_user_task_ids.extend(cancelled_tasks);
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

    fn cancel_scope(&mut self, root: &str, boundary_id: &str) -> Result<()> {
        let closure = super::repository::descendant_scope_ids(&self.scopes, root)?;
        let active = self
            .scopes
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
            .cloned()
            .collect::<Vec<_>>();
        let active_ids = active
            .iter()
            .map(|scope| scope.scope_id.clone())
            .collect::<std::collections::HashSet<_>>();
        self.plan.cancel_scope_roots.push(root.to_owned());
        for token in self
            .tokens
            .iter()
            .filter(|token| active_ids.contains(&token.scope_id))
            .cloned()
            .collect::<Vec<_>>()
        {
            self.plan.cancel_token_ids.push(token.token_id.clone());
            self.tokens
                .retain(|actual| actual.token_id != token.token_id);
        }
        for receipt in self
            .receipts
            .iter()
            .filter(|receipt| active_ids.contains(&receipt.scope_id))
            .cloned()
            .collect::<Vec<_>>()
        {
            self.receipts
                .retain(|actual| actual.token_id != receipt.token_id);
            if self
                .plan
                .add_gateway_receipts
                .iter()
                .any(|actual| actual.token_id == receipt.token_id)
            {
                self.plan
                    .add_gateway_receipts
                    .retain(|actual| actual.token_id != receipt.token_id);
            } else {
                self.plan.remove_gateway_receipts.push(receipt);
            }
        }
        self.plan.cancel_user_task_ids.extend(
            self.tasks
                .iter()
                .filter(|task| {
                    active_ids.contains(&task.scope_id)
                        && task.status == ProcessUserTaskStatus::Open
                })
                .map(|task| task.user_task_id.clone()),
        );
        self.tasks
            .retain(|task| !self.plan.cancel_user_task_ids.contains(&task.user_task_id));
        self.plan.cancel_job_ids.extend(
            self.jobs
                .iter()
                .filter(|job| {
                    active_ids.contains(&job.scope_id)
                        && matches!(job.status.as_str(), "queued" | "running" | "error")
                })
                .map(|job| job.job_id.clone()),
        );
        self.jobs
            .retain(|job| !self.plan.cancel_job_ids.contains(&job.job_id));
        let previous = self.current_scope.clone();
        for timer in self
            .timers
            .iter()
            .filter(|timer| {
                timer
                    .scope_id
                    .as_ref()
                    .is_some_and(|id| active_ids.contains(id))
                    && matches!(
                        timer.status,
                        ProcessTimerStatus::Pending | ProcessTimerStatus::Blocked
                    )
            })
            .cloned()
            .collect::<Vec<_>>()
        {
            self.current_scope = timer
                .scope_id
                .clone()
                .context("instance timer lacks scope")?;
            self.plan
                .timer_updates
                .push(super::repository::TimerUpdate {
                    timer_id: timer.timer_id.clone(),
                    expected_revision: timer.revision,
                    fired_occurrence: None,
                    occurrence: timer.occurrence,
                    due_at_ms: None,
                    status: ProcessTimerStatus::Cancelled,
                    last_reason: Some("scope_cancelled".into()),
                    next_check_at_ms: self.now_ms,
                });
            self.event("timer_cancelled", Some(timer.node_id.clone()), json!({"timer_id":timer.timer_id,"kind":timer.kind,"attached_token_id":timer.token_id,"reason":"scope_cancelled"}));
        }
        for subscription in self
            .subscriptions
            .iter()
            .filter(|subscription| {
                active_ids.contains(&subscription.scope_id)
                    && subscription.status
                        == tentaflow_protocol::processes::ProcessSubscriptionStatus::Open
            })
            .cloned()
            .collect::<Vec<_>>()
        {
            self.current_scope = subscription.scope_id.clone();
            self.settle_subscription(
                &subscription,
                tentaflow_protocol::processes::ProcessSubscriptionStatus::Cancelled,
                Some("scope_cancelled"),
            );
        }
        for race in self
            .event_races
            .iter()
            .filter(|race| {
                active_ids.contains(&race.scope_id)
                    && race.status == tentaflow_protocol::processes::ProcessEventRaceStatus::Open
            })
            .cloned()
            .collect::<Vec<_>>()
        {
            self.current_scope = race.scope_id.clone();
            self.plan
                .race_updates
                .push(super::repository::EventRaceUpdate {
                    race_id: race.race_id.clone(),
                    expected_revision: race.revision,
                    status: tentaflow_protocol::processes::ProcessEventRaceStatus::Cancelled,
                    winner_node_id: None,
                    winner_subscription_id: None,
                    winner_timer_id: None,
                });
            self.event(
                "event_race_cancelled",
                Some(race.gateway_node_id.clone()),
                json!({"race_id":race.race_id,"reason":"scope_cancelled"}),
            );
        }
        for id in self
            .incidents
            .iter()
            .chain(self.plan.add_incidents.iter())
            .filter(|incident| active_ids.contains(&incident.scope_id))
            .map(|incident| incident.incident_id.clone())
            .collect::<Vec<_>>()
        {
            self.resolve_incident(&id);
        }
        for scope in active {
            self.current_scope = scope.scope_id.clone();
            if scope.parent_scope_id.is_none() {
                continue;
            }
            self.update_scope(&scope.scope_id, ProcessInstanceStatus::Cancelled, None)?;
            self.event("scope_cancelled", None,
                json!({"scope_id":scope.scope_id,"parent_scope_id":scope.parent_scope_id,"parent_token_id":scope.parent_token_id,"subprocess_node_id":scope.subprocess_node_id,"reason":"scope_cancelled","boundary_id":boundary_id}));
        }
        self.current_scope = previous;
        Ok(())
    }

    fn enter_call(&mut self, node: &ProcessNode, token: &ProcessToken) -> Result<()> {
        let ProcessNodeKind::CallActivity { input_mapping, .. } = &node.kind else {
            anyhow::bail!("call entry requires a CallActivity");
        };
        let waiting = self.wait(token, "waiting");
        self.arm_boundaries(node, &waiting)?;
        match patch_variables(
            input_mapping,
            &json!({}),
            &self.effective()?,
            &Value::Null,
            &[],
        ) {
            Ok(variables) => {
                let request = super::repository::CallRequest {
                    call_id: Uuid::new_v4().to_string(),
                    parent_scope_id: self.current_scope.clone(),
                    parent_token_id: waiting,
                    call_node_id: node.id.clone(),
                    child_instance_id: Uuid::new_v4().to_string(),
                    variables,
                };
                self.event(
                    "call_requested",
                    Some(node.id.clone()),
                    json!({"call_id":request.call_id,"parent_token_id":request.parent_token_id}),
                );
                self.plan.call_requests.push(request);
            }
            Err(error) => self.incident(
                &node.id,
                None,
                "CALL_ADMISSION_ERROR",
                super::repository::bounded_failure_message(&format!("{error:#}")),
            ),
        }
        Ok(())
    }

    fn error_end(
        &mut self,
        node: &ProcessNode,
        token: &ProcessToken,
        error_ref: &str,
    ) -> Result<()> {
        let declaration = self
            .model
            .errors
            .iter()
            .find(|e| e.error_id == error_ref)
            .context("ErrorEnd declaration missing")?;
        let code = declaration.error_code.clone();
        ensure!(!code.is_empty(), "ErrorEnd needs a declared error code");
        let source_scope = self.current_scope.clone();
        let outputs = self.effective()?;
        let event_id = Uuid::new_v4().to_string();
        let fact = tentaflow_protocol::processes::ProcessTerminalError {
            error_ref: error_ref.to_owned(),
            error_code: code.clone(),
            source_event_id: event_id.clone(),
            source_node_id: node.id.clone(),
            source_scope_id: source_scope.clone(),
        };
        let envelope = json!({"kind":"ErrorEnd","error_ref":error_ref,"error_code":code,"source_instance_id":self.instance_id,"source_scope_id":source_scope,"source_node_id":node.id,"source_event_id":event_id,"source_token_id":token.token_id});
        self.plan.event_ids.insert(self.plan.events.len(), event_id);
        let mut factual_envelope = envelope.clone();
        factual_envelope["outputs"] = outputs.clone();
        self.event("error_end_reached", Some(node.id.clone()), factual_envelope);
        self.consume(&token.token_id);
        if let Some((subscription, attached, _hops)) = self.error_handler(token, Some(&code))? {
            self.current_scope = subscription.scope_id.clone();
            let handler = self.node(&subscription.node_id)?.clone();
            let ProcessNodeKind::BoundaryError { output_mapping, .. } = &handler.kind else {
                anyhow::bail!("error handler is not a BoundaryError")
            };
            self.map_outputs(
                output_mapping,
                &outputs,
                &[("activity_result".into(), envelope)],
            )?;
            self.settle_subscription(
                &subscription,
                tentaflow_protocol::processes::ProcessSubscriptionStatus::Consumed,
                None,
            );
            self.interrupt_activity(&attached, &subscription.subscription_id)?;
            self.plan
                .scope_terminal_errors
                .insert(source_scope.clone(), fact.clone());
            if source_scope != self.instance_id {
                self.update_scope(&source_scope, ProcessInstanceStatus::Error, None)?;
            }
            self.event("business_error_caught",Some(handler.id.clone()),json!({"subscription_id":subscription.subscription_id,"attached_token_id":attached.token_id,"error_code":code,"source_instance_id":self.instance_id,"source_event_id":fact.source_event_id,"source_scope_id":source_scope,"source_kind":"error_end"}));
            for edge in self.outgoing(&handler.id) {
                self.follow(&attached, &edge)?;
            }
        } else {
            self.current_scope = self.instance_id.to_owned();
            self.cancel_scope(self.instance_id, "error_end")?;
            for id in self
                .incidents
                .iter()
                .chain(self.plan.add_incidents.iter())
                .map(|i| i.incident_id.clone())
                .collect::<Vec<_>>()
            {
                self.resolve_incident(&id);
            }
            let source_scopes = self
                .scopes
                .clone()
                .into_iter()
                .filter(|s| {
                    s.parent_scope_id.is_some()
                        && super::repository::descendant_scope_ids(&self.scopes, &source_scope)
                            .is_ok_and(|ids| ids.contains(&s.scope_id))
                })
                .collect::<Vec<_>>();
            for scope in source_scopes {
                if scope.scope_id == source_scope {
                    self.plan
                        .scope_terminal_errors
                        .insert(scope.scope_id.clone(), fact.clone());
                    self.update_scope(&scope.scope_id, ProcessInstanceStatus::Error, None)?;
                }
            }
            self.plan.terminal_error = Some(fact.clone());
            self.plan.business_error = Some(super::repository::BusinessErrorSource::ErrorEnd {
                instance_id: self.instance_id.to_owned(),
                token_id: token.token_id.clone(),
                fact,
                outputs,
            });
            self.current_scope = self.instance_id.to_owned();
            self.event(
                "instance_error",
                None,
                json!({"error_ref":error_ref,"error_code":code}),
            );
        }
        Ok(())
    }

    fn enter_scope(&mut self, node: &ProcessNode, token: &ProcessToken) -> Result<()> {
        let ProcessNodeKind::SubProcess {
            body,
            input_mapping,
            ..
        } = &node.kind
        else {
            anyhow::bail!("scope entry requires an embedded subprocess");
        };
        let parent_scope = self.current_scope.clone();
        let waiting = self.wait(token, "waiting");
        self.arm_boundaries(node, &waiting)?;
        if self.retained_scope_count >= 129 {
            self.incident(
                &node.id,
                None,
                "SCOPE_LIMIT",
                "process instance reached its 129-scope lifetime limit".into(),
            );
            let incident_id = self
                .plan
                .add_incidents
                .last()
                .context("scope limit incident missing")?
                .incident_id
                .clone();
            self.event("scope_entry_failed", Some(node.id.clone()), json!({"parent_token_id":waiting,"scope_id":parent_scope,"subprocess_node_id":node.id,"incident_id":incident_id,"code":"SCOPE_LIMIT","reason":"scope_limit"}));
            return Ok(());
        }
        let local = patch_variables(
            input_mapping,
            &serde_json::to_value(&body.variables)?,
            &self.effective()?,
            &Value::Null,
            &[],
        )?;
        let start = body
            .nodes
            .iter()
            .find(|node| node.kind == ProcessNodeKind::Start)
            .context("embedded subprocess start missing")?;
        let scope_id = Uuid::new_v4().to_string();
        let depth = self
            .scopes
            .iter()
            .find(|scope| scope.scope_id == parent_scope)
            .context("parent scope missing")?
            .depth
            .checked_add(1)
            .context("scope depth overflow")?;
        ensure!(depth <= 3, "embedded subprocess depth exceeds three");
        self.plan.create_scopes.push(PlannedScope {
            scope_id: scope_id.clone(),
            parent_scope_id: parent_scope.clone(),
            parent_token_id: waiting.clone(),
            subprocess_node_id: node.id.clone(),
            variables: local.clone(),
        });
        self.retained_scope_count += 1;
        self.scopes.push(ProcessScopeSummary {
            scope_id: scope_id.clone(),
            parent_scope_id: Some(parent_scope.clone()),
            subprocess_node_id: Some(node.id.clone()),
            subprocess_node_name: Some(node.name.clone()),
            terminal_error: None,
            parent_token_id: Some(waiting.clone()),
            revision: 1,
            status: ProcessInstanceStatus::Running,
            depth,
            created_at_ms: self.now_ms,
            updated_at_ms: self.now_ms,
        });
        self.scope_variables.insert(scope_id.clone(), local);
        self.current_scope = scope_id.clone();
        self.event("scope_entered", None, json!({"scope_id":scope_id,"parent_scope_id":parent_scope,"parent_token_id":waiting,"subprocess_node_id":node.id}));
        self.create_token(ProcessToken {
            token_id: String::new(),
            scope_id: scope_id.clone(),
            node_id: start.id.clone(),
            arrival_edge_id: None,
            fork_stack: Vec::new(),
            status: "ready".into(),
        });
        self.current_scope = parent_scope;
        Ok(())
    }

    fn arm_subscription(
        &mut self,
        node: &ProcessNode,
        token_id: &str,
        race_id: Option<String>,
    ) -> Result<()> {
        use tentaflow_protocol::processes::{
            ProcessSubscriptionKind as K, ProcessSubscriptionStatus as S,
        };
        let (kind, message_ref, correlation, error_ref, escalation_ref, attachment) =
            match &node.kind {
                ProcessNodeKind::MessageCatch {
                    message_ref,
                    correlation_expression,
                    ..
                } => (
                    K::MessageCatch,
                    Some(message_ref),
                    Some(correlation_expression),
                    None,
                    None,
                    None,
                ),
                ProcessNodeKind::BoundaryMessage {
                    attached_to_id,
                    message_ref,
                    correlation_expression,
                    ..
                } => (
                    K::BoundaryMessage,
                    Some(message_ref),
                    Some(correlation_expression),
                    None,
                    None,
                    Some(attached_to_id),
                ),
                ProcessNodeKind::BoundaryError {
                    attached_to_id,
                    error_ref,
                    ..
                } => (
                    K::BoundaryError,
                    None,
                    None,
                    Some(error_ref),
                    None,
                    Some(attached_to_id),
                ),
                ProcessNodeKind::BoundaryEscalation {
                    attached_to_id,
                    escalation_ref,
                    ..
                } => (
                    K::BoundaryEscalation,
                    None,
                    None,
                    None,
                    Some(escalation_ref),
                    Some(attached_to_id),
                ),
                _ => anyhow::bail!("node does not arm a subscription"),
            };
        let name = message_ref
            .map(|id| {
                self.model
                    .messages
                    .iter()
                    .find(|d| &d.message_id == id)
                    .map(|d| d.name.clone())
                    .context("receiving message declaration missing")
            })
            .transpose()?;
        let code = error_ref
            .and_then(|id| id.as_ref())
            .map(|id| {
                self.model
                    .errors
                    .iter()
                    .find(|d| &d.error_id == id)
                    .map(|d| d.error_code.clone())
                    .context("boundary error declaration missing")
            })
            .transpose()?;
        let escalation_code = escalation_ref
            .and_then(|id| id.as_ref())
            .map(|id| {
                self.model
                    .escalations
                    .iter()
                    .find(|d| &d.escalation_id == id)
                    .map(|d| d.escalation_code.clone())
                    .context("boundary escalation declaration missing")
            })
            .transpose()?;
        let effective = self.effective()?;
        let predicate = correlation
            .map(|expression| super::messages::evaluate_key(expression, &effective))
            .transpose();
        let (key, status, reason) = match predicate {
            Ok(key) => (key, S::Open, None),
            Err(e) => (
                None,
                S::Error,
                Some(super::repository::timer_reason(&format!("{e:#}"))?),
            ),
        };
        let s = super::repository::EventSubscription {
            subscription_id: Uuid::new_v4().to_string(),
            scope_id: self.current_scope.clone(),
            instance_id: self.instance_id.to_owned(),
            org_id: self.org_id.to_owned(),
            definition_id: self.definition_id.to_owned(),
            version: self.version,
            node_id: node.id.clone(),
            token_id: token_id.to_owned(),
            kind: kind.clone(),
            message_name: name,
            correlation_key: key,
            error_code: code,
            escalation_code,
            race_id,
            revision: 1,
            status: status.clone(),
            last_reason: reason.clone(),
            created_at_ms: self.now_ms,
            updated_at_ms: self.now_ms,
        };
        if status == S::Error {
            self.incident(
                &node.id,
                None,
                "MESSAGE_PREDICATE_ERROR",
                reason
                    .clone()
                    .context("subscription error reason missing")?,
            );
            let incident_id = self
                .plan
                .add_incidents
                .last()
                .context("predicate incident missing")?
                .incident_id
                .clone();
            self.event("message_error",Some(node.id.clone()),json!({"subscription_id":s.subscription_id,"attached_token_id":token_id,"incident_id":incident_id,"reason":reason}));
        } else if kind == K::BoundaryEscalation {
            let ProcessNodeKind::BoundaryEscalation {
                cancel_activity, ..
            } = &node.kind
            else {
                anyhow::bail!("escalation subscription has a different node kind");
            };
            self.event("escalation_boundary_armed",Some(node.id.clone()),json!({"subscription_id":s.subscription_id,"attached_token_id":token_id,"attached_to_id":attachment,"escalation_code":s.escalation_code,"cancel_activity":cancel_activity}));
        } else {
            self.event(if kind==K::BoundaryError{"error_boundary_armed"}else{"message_armed"},Some(node.id.clone()),json!({"subscription_id":s.subscription_id,"token_id":token_id,"attached_to_id":attachment,"message_name":s.message_name,"correlation_key":s.correlation_key,"error_code":s.error_code,"race_id":s.race_id,"kind":kind}));
        }
        self.subscriptions.push(s.clone());
        self.plan.create_subscriptions.push(s);
        Ok(())
    }
    fn settle_subscription(
        &mut self,
        s: &super::repository::EventSubscription,
        status: tentaflow_protocol::processes::ProcessSubscriptionStatus,
        reason: Option<&str>,
    ) {
        if let Some(current) = self
            .subscriptions
            .iter_mut()
            .find(|current| current.subscription_id == s.subscription_id)
        {
            current.status = status.clone();
        }
        self.plan
            .subscription_updates
            .push(super::repository::SubscriptionUpdate {
                subscription_id: s.subscription_id.clone(),
                expected_revision: s.revision,
                status: status.clone(),
                last_reason: reason.map(str::to_owned),
            });
        if status == tentaflow_protocol::processes::ProcessSubscriptionStatus::Cancelled {
            self.event("subscription_cancelled",Some(s.node_id.clone()),json!({"subscription_id":s.subscription_id,"attached_token_id":s.token_id,"reason":reason}));
        }
    }
    fn win_race(
        &mut self,
        race_id: &str,
        node_id: &str,
        subscription_id: Option<&str>,
        timer_id: Option<&str>,
    ) -> Result<()> {
        use tentaflow_protocol::processes::{
            ProcessEventRaceStatus as R, ProcessSubscriptionStatus as S,
        };
        let race = self
            .event_races
            .iter()
            .find(|r| r.race_id == race_id && r.status == R::Open)
            .context("event race is already settled")?
            .clone();
        self.plan
            .race_updates
            .push(super::repository::EventRaceUpdate {
                race_id: race.race_id.clone(),
                expected_revision: race.revision,
                status: R::Won,
                winner_node_id: Some(node_id.to_owned()),
                winner_subscription_id: subscription_id.map(str::to_owned),
                winner_timer_id: timer_id.map(str::to_owned),
            });
        for s in self
            .subscriptions
            .iter()
            .filter(|s| {
                s.race_id.as_deref() == Some(race_id) && matches!(s.status, S::Open | S::Error)
            })
            .cloned()
            .collect::<Vec<_>>()
        {
            if Some(s.subscription_id.as_str()) == subscription_id {
                continue;
            }
            if s.status == S::Open {
                self.settle_subscription(&s, S::Cancelled, Some("event_race_lost"));
            }
            self.resolve_boundary_incidents(&s.token_id);
            self.tokens.retain(|t| t.token_id != s.token_id);
            self.plan.cancel_token_ids.push(s.token_id);
        }
        for t in self
            .timers
            .iter()
            .filter(|t| {
                t.race_id.as_deref() == Some(race_id)
                    && Some(t.timer_id.as_str()) != timer_id
                    && matches!(
                        t.status,
                        ProcessTimerStatus::Pending
                            | ProcessTimerStatus::Blocked
                            | ProcessTimerStatus::Error
                    )
            })
            .cloned()
            .collect::<Vec<_>>()
        {
            if t.status != ProcessTimerStatus::Error {
                self.plan
                    .timer_updates
                    .push(super::repository::TimerUpdate {
                        timer_id: t.timer_id.clone(),
                        expected_revision: t.revision,
                        fired_occurrence: None,
                        occurrence: t.occurrence,
                        due_at_ms: None,
                        status: ProcessTimerStatus::Cancelled,
                        last_reason: Some("event_race_lost".into()),
                        next_check_at_ms: self.now_ms,
                    });
            }
            let id = t.token_id.context("race timer lacks activation")?;
            self.resolve_boundary_incidents(&id);
            self.tokens.retain(|token| token.token_id != id);
            self.plan.cancel_token_ids.push(id);
            if t.status != ProcessTimerStatus::Error {
                self.event(
                    "timer_cancelled",
                    Some(t.node_id),
                    json!({"timer_id":t.timer_id,"race_id":race_id,"reason":"event_race_lost"}),
                );
            }
        }
        self.event("event_race_won",Some(race.gateway_node_id),json!({"race_id":race_id,"winner_node_id":node_id,"subscription_id":subscription_id,"timer_id":timer_id}));
        Ok(())
    }
    fn enter_event_race(&mut self, node: &ProcessNode, token: &ProcessToken) -> Result<()> {
        let id = Uuid::new_v4().to_string();
        let race = super::repository::EventRace {
            race_id: id.clone(),
            scope_id: self.current_scope.clone(),
            instance_id: self.instance_id.to_owned(),
            gateway_node_id: node.id.clone(),
            activation_id: token.token_id.clone(),
            revision: 1,
            status: tentaflow_protocol::processes::ProcessEventRaceStatus::Open,
            winner_node_id: None,
            winner_subscription_id: None,
            winner_timer_id: None,
            won_at_ms: None,
            created_at_ms: self.now_ms,
            updated_at_ms: self.now_ms,
        };
        self.consume(&token.token_id);
        self.plan.create_event_races.push(race.clone());
        self.event_races.push(race);
        self.event(
            "event_race_armed",
            Some(node.id.clone()),
            json!({"race_id":id,"activation_id":token.token_id}),
        );
        for edge in self.outgoing(&node.id) {
            let branch = self
                .body()?
                .1
                .iter()
                .find(|f| f.id == edge)
                .context("race branch edge missing")?;
            let child = self.node(&branch.target_id)?.clone();
            let waiting = self.create_token(ProcessToken {
                token_id: String::new(),
                scope_id: self.current_scope.clone(),
                node_id: child.id.clone(),
                arrival_edge_id: Some(edge),
                fork_stack: token.fork_stack.clone(),
                status: "waiting".into(),
            });
            match &child.kind {
                ProcessNodeKind::MessageCatch { .. } => {
                    self.arm_subscription(&child, &waiting, Some(id.clone()))?
                }
                ProcessNodeKind::TimerCatch { timer } => {
                    self.arm_timer(&child, &waiting, ProcessTimerKind::Catch, timer)?;
                    self.timers
                        .last_mut()
                        .context("race timer missing")?
                        .race_id = Some(id.clone());
                    self.plan
                        .create_timers
                        .last_mut()
                        .context("planned race timer missing")?
                        .race_id = Some(id.clone());
                }
                _ => anyhow::bail!("event race branch is not a one-shot catch"),
            }
        }
        Ok(())
    }
    fn throw_message(&mut self, node: &ProcessNode, token: &ProcessToken) -> Result<()> {
        let prepared = super::messages::prepare_throw(self.model, node, &self.effective()?)?;
        let event_index = self.plan.events.len();
        self.event("message_queued",Some(node.id.clone()),json!({"message_id":prepared.message_id,"source_activation_id":token.token_id,"target":prepared.target,"message_name":prepared.message_name,"correlation_key":prepared.correlation_key}));
        self.plan
            .create_messages
            .push(super::repository::PlannedMessage {
                message: prepared,
                source_scope_id: self.current_scope.clone(),
                source_node_id: node.id.clone(),
                source_activation_id: token.token_id.clone(),
                source_event_index: event_index,
            });
        self.consume(&token.token_id);
        for edge in self.outgoing(&node.id) {
            self.follow(token, &edge)?;
        }
        Ok(())
    }

    fn join(&mut self, token: &ProcessToken) -> Result<()> {
        let frame = token.fork_stack.last().context("gateway join has no fork activation")?.clone();
        ensure!(frame.join_node_id == token.node_id, "gateway join differs from its fork activation");
        let pair = gateway_pairs(self.body()?.0, self.body()?.1)?
            .remove(&frame.split_node_id)
            .context("gateway fork pair is missing")?;
        ensure!(pair.kind == frame.gateway_kind && pair.join_node_id == token.node_id,
            "gateway join differs from the paired model");
        ensure!(pair.branch_to_incoming_edge.get(&frame.branch_edge_id) == token.arrival_edge_id.as_ref(),
            "gateway branch arrived through another join edge");
        ensure!(frame.selected_branch_edge_ids.binary_search(&frame.branch_edge_id).is_ok(),
            "gateway branch was not selected");
        ensure!(!self.receipts.iter().any(|receipt| receipt.scope_id == self.current_scope
            && receipt.join_node_id == frame.join_node_id
            && receipt.activation_id == frame.activation_id
            && receipt.branch_edge_id == frame.branch_edge_id), "gateway branch arrived twice");
        let token_id = self.wait(token, "joining");
        let receipt = GatewayReceipt {
            scope_id: self.current_scope.clone(), gateway_kind: frame.gateway_kind,
            join_node_id: token.node_id.clone(), activation_id: frame.activation_id.clone(),
            branch_edge_id: frame.branch_edge_id.clone(), token_id,
        };
        self.receipts.push(receipt.clone());
        self.plan.add_gateway_receipts.push(receipt);
        let arrivals = self.receipts.iter().filter(|receipt| receipt.scope_id == self.current_scope
            && receipt.join_node_id == frame.join_node_id && receipt.activation_id == frame.activation_id)
            .cloned().collect::<Vec<_>>();
        ensure!(arrivals.iter().all(|receipt| {
            let Some(joining) = self.tokens.iter().find(|candidate| candidate.token_id == receipt.token_id) else { return false; };
            receipt.gateway_kind == frame.gateway_kind
                && frame.selected_branch_edge_ids.binary_search(&receipt.branch_edge_id).is_ok()
                && joining.status == "joining" && joining.scope_id == self.current_scope
                && joining.node_id == frame.join_node_id
                && joining.fork_stack.last().is_some_and(|sibling| sibling.gateway_kind == frame.gateway_kind
                    && sibling.activation_id == frame.activation_id
                    && sibling.join_node_id == frame.join_node_id
                    && sibling.branch_edge_id == receipt.branch_edge_id
                    && sibling.selected_branch_edge_ids == frame.selected_branch_edge_ids
                    && pair.branch_to_incoming_edge.get(&sibling.branch_edge_id) == joining.arrival_edge_id.as_ref())
        }), "gateway receipts differ from their selected joining tokens");
        if arrivals.len() != frame.selected_branch_edge_ids.len() { return Ok(()); }
        ensure!(frame.selected_branch_edge_ids.iter().all(|edge| arrivals.iter().any(|receipt| &receipt.branch_edge_id == edge)),
            "gateway receipts do not match selected branches");
        for receipt in arrivals {
            self.consume(&receipt.token_id);
            self.receipts.retain(|existing| existing.token_id != receipt.token_id);
            if self.plan.add_gateway_receipts.iter().any(|existing| existing.token_id == receipt.token_id) {
                self.plan.add_gateway_receipts.retain(|existing| existing.token_id != receipt.token_id);
            } else {
                self.plan.remove_gateway_receipts.push(receipt);
            }
        }
        let mut next = token.clone();
        next.fork_stack.pop();
        let kind = match frame.gateway_kind { GatewayKind::Parallel => "parallel_joined", GatewayKind::Inclusive => "inclusive_joined" };
        let data = match frame.gateway_kind {
            GatewayKind::Parallel => json!({"activation_id": frame.activation_id}),
            GatewayKind::Inclusive => json!({"activation_id": frame.activation_id,
                "selected_branch_edge_ids": frame.selected_branch_edge_ids}),
        };
        self.event(kind, Some(token.node_id.clone()), data);
        for edge in self.outgoing(&token.node_id) { self.follow(&next, &edge)?; }
        Ok(())
    }

    fn advance(&mut self) -> Result<()> {
        while let Some(token) = self
            .tokens
            .iter()
            .find(|token| token.status == "ready")
            .cloned()
        {
            self.current_scope = token.scope_id.clone();
            let id = token.token_id.clone();
            let node = self.node(&token.node_id)?.clone();
            let outgoing = self.outgoing(&node.id);
            match node.kind.clone() {
                ProcessNodeKind::Start
                | ProcessNodeKind::TimerStart { .. }
                | ProcessNodeKind::MessageStart { .. } => {
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
                ProcessNodeKind::CallActivity { .. } => self.enter_call(&node, &token)?,
                ProcessNodeKind::ErrorEnd { error_ref } => {
                    self.error_end(&node, &token, &error_ref)?
                }
                ProcessNodeKind::SubProcess { .. } => self.enter_scope(&node, &token)?,
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
                ProcessNodeKind::MessageCatch { .. } => {
                    let waiting = self.wait(&token, "waiting");
                    self.arm_subscription(&node, &waiting, None)?;
                }
                ProcessNodeKind::MessageThrow { .. } => {
                    if let Err(error) = self.throw_message(&node, &token) {
                        self.wait(&token, "waiting");
                        self.incident(
                            &node.id,
                            None,
                            "MESSAGE_EXPRESSION_ERROR",
                            super::repository::bounded_failure_message(&error.to_string()),
                        );
                    }
                }
                ProcessNodeKind::EventBasedGateway => {
                    self.enter_event_race(&node, &token)?;
                }
                ProcessNodeKind::BoundaryTimer { .. }
                | ProcessNodeKind::BoundaryMessage { .. }
                | ProcessNodeKind::BoundaryError { .. }
                | ProcessNodeKind::BoundaryEscalation { .. } => {
                    anyhow::bail!("boundary events are entered only by their attached timer")
                }
                ProcessNodeKind::ServiceTask { input_mapping, .. } => {
                    match prepare_service_input(&input_mapping, &self.effective()?) {
                        Ok(input) => {
                            let token_id = self.wait(&token, "waiting");
                            let job = ProcessJob {
                                job_id: Uuid::new_v4().to_string(),
                                instance_id: self.instance_id.to_owned(),
                                scope_id: self.current_scope.clone(),
                                node_id: node.id.clone(),
                                token_id: token_id.clone(),
                                input,
                                status: "queued".into(),
                                attempt: 0,
                                fence: 0,
                                worker_id: None,
                                lease_until_ms: None,
                                result: None,

                                result_origin: None,
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
                            .body()?
                            .1
                            .iter()
                            .find(|flow| flow.id == *edge_id)
                            .context("sequence flow missing")?;
                        match edge.condition.as_ref().map_or(Ok(true), |expression| {
                            condition(expression, &self.effective()?, &Value::Null)
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
                    let mut data = json!({"sequence_flow_id": edge});
                    if self.escalation_continuation {
                        data["source_token_id"] = json!(id);
                        data["activation_id"] = json!(id);
                    }
                    self.event("exclusive_selected", Some(node.id), data);
                    self.follow(&token, &edge)?;
                }
                ProcessNodeKind::ParallelGateway | ProcessNodeKind::InclusiveGateway { .. } => {
                    let gateway_kind = match &node.kind {
                        ProcessNodeKind::ParallelGateway => GatewayKind::Parallel,
                        ProcessNodeKind::InclusiveGateway { .. } => GatewayKind::Inclusive,
                        _ => unreachable!(),
                    };
                    let pairs = gateway_pairs(self.body()?.0, self.body()?.1)?;
                    if let Some(pair) = pairs.get(&node.id) {
                        ensure!(pair.kind == gateway_kind, "gateway kind differs from paired body");
                        let all_edges = pair.branch_to_incoming_edge.keys().cloned().collect::<Vec<_>>();
                        let (selected, default_selected) = match &node.kind {
                            ProcessNodeKind::ParallelGateway => (all_edges.clone(), false),
                            ProcessNodeKind::InclusiveGateway { default_flow_id } => {
                                let effective = self.effective()?;
                                let mut selected = Vec::new();
                                let mut failure = None;
                                for edge_id in &all_edges {
                                    if default_flow_id.as_ref() == Some(edge_id) { continue; }
                                    let edge = self.body()?.1.iter().find(|flow| flow.id == *edge_id)
                                        .context("inclusive sequence flow missing")?;
                                    let expression = edge.condition.as_deref().context("inclusive condition missing")?;
                                    match evaluate(expression, &effective, &Value::Null, &[]) {
                                        Ok(Value::Bool(true)) => selected.push(edge_id.clone()),
                                        Ok(Value::Bool(false)) => {},
                                        Ok(_) => { if failure.is_none() { failure = Some(("non_boolean_condition", Some(edge_id.clone()), format!("condition on flow {edge_id} is not boolean"))); } },
                                        Err(error) => { if failure.is_none() { failure = Some(("condition_evaluation_failed", Some(edge_id.clone()), format!("condition on flow {edge_id}: {error}"))); } },
                                    }
                                }
                                let default_selected = selected.is_empty() && failure.is_none() && default_flow_id.is_some();
                                if let Some(default_edge) = default_flow_id.as_ref().filter(|_| default_selected) {
                                    selected.push(default_edge.clone());
                                }
                                if failure.is_none() && selected.is_empty() {
                                    failure = Some(("no_matching_flow", None, format!("inclusive gateway {} has no matching flow", node.id)));
                                }
                                if let Some((reason, condition_edge_id, message)) = failure {
                                    let waiting_token_id = self.wait(&token, "waiting");
                                    let message = message.chars().take(512).collect::<String>();
                                    self.incident(&node.id, None, "INCLUSIVE_GATEWAY_ERROR", message);
                                    if let Some(event) = self.plan.events.last_mut() {
                                        event.data["reason"] = json!(reason);
                                        event.data["condition_edge_id"] = json!(condition_edge_id);
                                        event.data["source_token_id"] = json!(id);
                                        event.data["waiting_token_id"] = json!(waiting_token_id);
                                    }
                                    break;
                                }
                                (selected, default_selected)
                            }
                            _ => unreachable!(),
                        };
                        let activation_id = Uuid::new_v4().to_string();
                        self.consume(&id);
                        let kind = match gateway_kind {
                            GatewayKind::Parallel => "parallel_split",
                            GatewayKind::Inclusive => "inclusive_split",
                        };
                        let mut data = match gateway_kind {
                            GatewayKind::Parallel => json!({"activation_id": activation_id}),
                            GatewayKind::Inclusive => json!({"activation_id": activation_id,
                                "selected_branch_edge_ids": selected, "default_selected": default_selected}),
                        };
                        if self.escalation_continuation && gateway_kind == GatewayKind::Inclusive {
                            data["source_token_id"] = json!(id);
                        }
                        self.event(kind, Some(node.id.clone()), data);
                        for edge in &selected {
                            let mut branch = token.clone();
                            branch.fork_stack.push(ForkFrame {
                                activation_id: activation_id.clone(), split_node_id: node.id.clone(),
                                join_node_id: pair.join_node_id.clone(), branch_edge_id: edge.clone(),
                                gateway_kind, selected_branch_edge_ids: selected.clone(),
                            });
                            self.follow(&branch, edge)?;
                        }
                    } else {
                        self.join(&token)?;
                    }
                }
            }
        }
        if self.complete_scopes()? {
            self.advance()?;
        }
        Ok(())
    }

    fn complete_scopes(&mut self) -> Result<bool> {
        let mut scopes = self
            .scopes
            .iter()
            .filter(|scope| {
                scope.parent_scope_id.is_some()
                    && !matches!(
                        scope.status,
                        ProcessInstanceStatus::Completed
                            | ProcessInstanceStatus::Cancelled
                            | ProcessInstanceStatus::Error
                    )
            })
            .cloned()
            .collect::<Vec<_>>();
        scopes.sort_by_key(|scope| std::cmp::Reverse(scope.depth));
        let mut progressed = false;
        for scope in scopes {
            if self
                .tokens
                .iter()
                .any(|token| token.scope_id == scope.scope_id)
                || self
                    .receipts
                    .iter()
                    .any(|receipt| receipt.scope_id == scope.scope_id)
                || self.jobs.iter().any(|job| {
                    job.scope_id == scope.scope_id
                        && matches!(job.status.as_str(), "queued" | "running" | "error")
                        && self
                            .tokens
                            .iter()
                            .any(|token| token.token_id == job.token_id)
                })
                || self.tasks.iter().any(|task| {
                    task.scope_id == scope.scope_id && task.status == ProcessUserTaskStatus::Open
                })
                || self
                    .incidents
                    .iter()
                    .chain(self.plan.add_incidents.iter())
                    .any(|incident| {
                        incident.scope_id == scope.scope_id
                            && !self
                                .plan
                                .resolve_incident_ids
                                .contains(&incident.incident_id)
                    })
                || self.scopes.iter().any(|child| {
                    child.parent_scope_id.as_deref() == Some(scope.scope_id.as_str())
                        && !matches!(
                            child.status,
                            ProcessInstanceStatus::Completed
                                | ProcessInstanceStatus::Cancelled
                                | ProcessInstanceStatus::Error
                        )
                })
            {
                continue;
            }
            let parent = scope
                .parent_scope_id
                .clone()
                .context("child parent scope missing")?;
            let waiting = scope
                .parent_token_id
                .as_deref()
                .context("child parent wait missing")?;
            let token = self
                .tokens
                .iter()
                .find(|token| {
                    token.token_id == waiting
                        && token.scope_id == parent
                        && token.status == "waiting"
                })
                .context("completing child lost its parent waiting activation")?
                .clone();
            self.current_scope = parent;
            let node = self.node(&token.node_id)?.clone();
            let ProcessNodeKind::SubProcess { output_mapping, .. } = &node.kind else {
                anyhow::bail!("child completion parent activation is not a subprocess");
            };
            let outputs = self
                .scope_variables
                .get(&scope.scope_id)
                .context("completing child local variables missing")?
                .clone();
            if let Err(error) = self.map_outputs(output_mapping, &outputs, &[]) {
                self.current_scope = scope.scope_id.clone();
                let incident_id = Uuid::new_v4().to_string();
                let message = super::repository::bounded_failure_message(&format!("{error:#}"));
                self.plan.add_incidents.push(ProcessIncident {
                    incident_id: incident_id.clone(),
                    scope_id: scope.scope_id.clone(),
                    node_id: None,
                    node_name: None,
                    job_id: None,
                    code: "SCOPE_RETURN_ERROR".into(),
                    message: message.clone(),
                    at_ms: self.now_ms,
                    can_retry: false,
                });
                self.event("incident", None, json!({"incident_id":incident_id,"code":"SCOPE_RETURN_ERROR","message":message,"parent_token_id":waiting}));
                self.update_scope(&scope.scope_id, ProcessInstanceStatus::Incident, None)?;
                continue;
            }
            self.update_scope(
                &scope.scope_id,
                ProcessInstanceStatus::Completed,
                Some(outputs),
            )?;
            let parent_scope = self.current_scope.clone();
            self.current_scope = scope.scope_id.clone();
            self.event("scope_completed", None, json!({"scope_id":scope.scope_id,"parent_scope_id":parent_scope,"parent_token_id":waiting,"subprocess_node_id":node.id}));
            self.current_scope = parent_scope;
            self.disarm_boundaries(waiting, "activity_completed", None)?;
            self.resolve_boundary_incidents(waiting);
            self.consume(waiting);
            for edge in self.outgoing(&node.id) {
                self.follow(&token, &edge)?;
            }
            progressed = true;
        }
        Ok(progressed)
    }

    fn error_handler(
        &self,
        token: &ProcessToken,
        code: Option<&str>,
    ) -> Result<
        Option<(
            super::repository::EventSubscription,
            ProcessToken,
            Vec<String>,
        )>,
    > {
        let mut attached = token.clone();
        let mut hops = Vec::new();
        loop {
            let open = self
                .subscriptions
                .iter()
                .filter(|subscription| {
                    subscription.scope_id == attached.scope_id
                        && subscription.token_id == attached.token_id
                        && subscription.kind
                            == tentaflow_protocol::processes::ProcessSubscriptionKind::BoundaryError
                        && subscription.status
                            == tentaflow_protocol::processes::ProcessSubscriptionStatus::Open
                })
                .collect::<Vec<_>>();
            if let Some(handler) = open
                .iter()
                .find(|subscription| {
                    subscription.error_code.is_some() && subscription.error_code.as_deref() == code
                })
                .or_else(|| {
                    open.iter()
                        .find(|subscription| subscription.error_code.is_none())
                })
            {
                return Ok(Some(((*handler).clone(), attached, hops)));
            }
            let scope = self
                .scopes
                .iter()
                .find(|scope| scope.scope_id == attached.scope_id)
                .context("error source scope is missing")?;
            let Some(parent) = &scope.parent_scope_id else {
                return Ok(None);
            };
            hops.push(scope.scope_id.clone());
            attached = self
                .tokens
                .iter()
                .find(|candidate| {
                    candidate.scope_id == *parent
                        && Some(candidate.token_id.as_str()) == scope.parent_token_id.as_deref()
                        && candidate.status == "waiting"
                })
                .context("error propagation lost enclosing subprocess activation")?
                .clone();
        }
    }

    fn finish(mut self) -> Result<RuntimePlan> {
        self.plan.status = if self.plan.terminal_error.is_some() {
            ProcessInstanceStatus::Error
        } else if self
            .incidents
            .iter()
            .chain(self.plan.add_incidents.iter())
            .any(|incident| {
                !self
                    .plan
                    .resolve_incident_ids
                    .contains(&incident.incident_id)
            }) {
            ProcessInstanceStatus::Incident
        } else if self.tokens.is_empty()
            && !self.scopes.iter().any(|scope| {
                scope.parent_scope_id.is_some()
                    && !matches!(
                        scope.status,
                        ProcessInstanceStatus::Completed
                            | ProcessInstanceStatus::Cancelled
                            | ProcessInstanceStatus::Error
                    )
            })
        {
            self.current_scope = self.instance_id.to_owned();
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
        for scope in self.scopes.clone().into_iter().filter(|scope| {
            scope.parent_scope_id.is_some()
                && !matches!(
                    scope.status,
                    ProcessInstanceStatus::Completed
                        | ProcessInstanceStatus::Cancelled
                        | ProcessInstanceStatus::Error
                )
        }) {
            let status = if self
                .incidents
                .iter()
                .chain(self.plan.add_incidents.iter())
                .any(|incident| {
                    incident.scope_id == scope.scope_id
                        && !self
                            .plan
                            .resolve_incident_ids
                            .contains(&incident.incident_id)
                }) {
                ProcessInstanceStatus::Incident
            } else if self.jobs.iter().any(|job| {
                job.scope_id == scope.scope_id
                    && matches!(job.status.as_str(), "queued" | "running")
            }) || self
                .tokens
                .iter()
                .any(|token| token.scope_id == scope.scope_id && token.status == "ready")
            {
                ProcessInstanceStatus::Running
            } else {
                ProcessInstanceStatus::Waiting
            };
            if scope.status != status {
                self.update_scope(&scope.scope_id, status, None)?;
            }
        }
        Ok(self.plan)
    }
}

pub(super) fn project_snapshot(
    snapshot: &RuntimeSnapshot,
    plan: &RuntimePlan,
    now_ms: i64,
) -> Result<RuntimeSnapshot> {
    let mut next = snapshot.clone();
    next.instance.revision = next
        .instance
        .revision
        .checked_add(1)
        .context("process revision overflow")?;
    next.instance.variables = plan.variables.clone();
    next.instance.status = plan.status.clone();
    next.instance.terminal_error = plan.terminal_error.clone();
    next.instance.updated_at_ms = now_ms;
    next.tokens.extend(plan.create_tokens.clone());
    next.tokens.retain(|t| {
        !plan.consume_token_ids.contains(&t.token_id)
            && !plan.cancel_token_ids.contains(&t.token_id)
    });
    next.receipts.extend(plan.add_gateway_receipts.clone());
    next.receipts.retain(|r| {
        !plan.remove_gateway_receipts.iter().any(|v| {
            v.scope_id == r.scope_id
                && v.activation_id == r.activation_id
                && v.branch_edge_id == r.branch_edge_id
        })
    });
    next.user_tasks.extend(plan.create_user_tasks.clone());
    for task in &mut next.user_tasks {
        if plan.complete_user_task_ids.contains(&task.user_task_id) {
            task.status = ProcessUserTaskStatus::Completed;
        }
        if plan.cancel_user_task_ids.contains(&task.user_task_id) {
            task.status = ProcessUserTaskStatus::Cancelled;
        }
    }
    next.jobs.extend(plan.create_jobs.clone());
    for job in &mut next.jobs {
        if plan.complete_job_ids.contains(&job.job_id) {
            job.status = "completed".into();
        }
        if plan.cancel_job_ids.contains(&job.job_id) {
            job.status = "cancelled".into();
        }
    }
    next.incidents
        .retain(|i| !plan.resolve_incident_ids.contains(&i.incident_id));
    next.incidents.extend(plan.add_incidents.clone());
    for child in &plan.create_scopes {
        let node = super::repository::scope_node(
            &snapshot.model,
            &next.scopes,
            &snapshot.instance.instance_id,
            &child.parent_scope_id,
            &child.subprocess_node_id,
        )?;
        let depth = next
            .scopes
            .iter()
            .find(|s| s.scope_id == child.parent_scope_id)
            .context("planned child parent missing")?
            .depth
            + 1;
        next.scopes.push(ProcessScopeSummary {
            scope_id: child.scope_id.clone(),
            parent_scope_id: Some(child.parent_scope_id.clone()),
            subprocess_node_id: Some(child.subprocess_node_id.clone()),
            subprocess_node_name: Some(node.name.clone()),
            parent_token_id: Some(child.parent_token_id.clone()),
            revision: 1,
            status: ProcessInstanceStatus::Running,
            depth,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            terminal_error: None,
        });
        next.scope_variables
            .insert(child.scope_id.clone(), child.variables.clone());
    }
    for update in &plan.scope_updates {
        let scope = next
            .scopes
            .iter_mut()
            .find(|s| s.scope_id == update.scope_id)
            .context("updated child missing")?;
        scope.status = update.status.clone();
        scope.revision += 1;
        scope.updated_at_ms = now_ms;
        scope.terminal_error = plan.scope_terminal_errors.get(&update.scope_id).cloned();
        if let Some(vars) = &update.variables {
            next.scope_variables
                .insert(update.scope_id.clone(), vars.clone());
        }
    }
    next.timers.extend(plan.create_timers.clone());
    for update in &plan.timer_updates {
        let timer = next
            .timers
            .iter_mut()
            .find(|t| t.timer_id == update.timer_id)
            .context("planned timer update missing")?;
        timer.status = update.status.clone();
        timer.revision += 1;
        timer.occurrence = update.occurrence;
        timer.due_at_ms = update.due_at_ms;
        timer.last_reason = update.last_reason.clone();
        timer.next_check_at_ms = update.next_check_at_ms;
    }
    next.subscriptions.extend(plan.create_subscriptions.clone());
    for update in &plan.subscription_updates {
        let sub = next
            .subscriptions
            .iter_mut()
            .find(|s| s.subscription_id == update.subscription_id)
            .context("planned subscription missing")?;
        sub.status = update.status.clone();
        sub.revision += 1;
        sub.last_reason = update.last_reason.clone();
    }
    next.event_races.extend(plan.create_event_races.clone());
    for update in &plan.race_updates {
        let race = next
            .event_races
            .iter_mut()
            .find(|r| r.race_id == update.race_id)
            .context("planned race missing")?;
        race.status = update.status.clone();
        race.revision += 1;
        race.winner_node_id = update.winner_node_id.clone();
        race.winner_timer_id = update.winner_timer_id.clone();
        race.winner_subscription_id = update.winner_subscription_id.clone();
    }
    if let Some(root) = next
        .scopes
        .iter_mut()
        .find(|s| s.scope_id == snapshot.instance.instance_id)
    {
        root.status = next.instance.status.clone();
        root.revision = next.instance.revision;
        root.terminal_error = next.instance.terminal_error.clone();
    }
    validate_variables(&next.instance.variables)?;
    for scope in next.scopes.iter().filter(|scope| {
        scope.parent_scope_id.is_some()
            && !matches!(
                scope.status,
                ProcessInstanceStatus::Completed
                    | ProcessInstanceStatus::Cancelled
                    | ProcessInstanceStatus::Error
            )
    }) {
        super::repository::effective_scope_variables(
            &next.scopes,
            &next.scope_variables,
            &next.instance.instance_id,
            &next.instance.variables,
            &scope.scope_id,
        )?;
    }
    next.instance.active_node_ids = next.tokens.iter().map(|t| t.node_id.clone()).collect();
    next.instance.active_node_ids.sort();
    next.instance.active_node_ids.dedup();
    Ok(next)
}

pub(super) fn plan_call_incident(
    snapshot: &RuntimeSnapshot,
    call_node_id: &str,
    scope_id: &str,
    code: &str,
    message: &str,
    now_ms: i64,
) -> Result<RuntimePlan> {
    let mut transition = Transition::from_snapshot(snapshot, now_ms)?;
    transition.current_scope = scope_id.to_owned();
    transition.node(call_node_id)?;
    transition.incident(
        call_node_id,
        None,
        code,
        super::repository::bounded_failure_message(message),
    );
    transition.finish()
}

pub(super) fn plan_call_return(
    snapshot: &RuntimeSnapshot,
    call: &super::repository::CallActivation,
    outputs: &Value,
    now_ms: i64,
) -> Result<RuntimePlan> {
    let mut transition = Transition::from_snapshot(snapshot, now_ms)?;
    transition.current_scope = call.parent_scope_id.clone();
    let token = transition
        .tokens
        .iter()
        .find(|t| {
            t.token_id == call.parent_token_id
                && t.status == "waiting"
                && t.scope_id == call.parent_scope_id
                && t.node_id == call.call_node_id
        })
        .context("call return lost its exact waiting token")?
        .clone();
    let node = transition.node(&call.call_node_id)?.clone();
    let ProcessNodeKind::CallActivity { output_mapping, .. } = &node.kind else {
        anyhow::bail!("call return node is not CallActivity")
    };
    transition.map_outputs(output_mapping, outputs, &[])?;
    transition.disarm_boundaries(&token.token_id, "activity_completed", None)?;
    transition.resolve_boundary_incidents(&token.token_id);
    transition.consume(&token.token_id);
    transition.event("call_returned",Some(node.id.clone()),json!({"call_id":call.call_id,"child_instance_id":call.child_instance_id,"parent_token_id":call.parent_token_id}));
    for edge in transition.outgoing(&node.id) {
        transition.follow(&token, &edge)?;
    }
    transition.advance()?;
    transition.finish()
}

pub(super) fn plan_call_error(
    snapshot: &RuntimeSnapshot,
    call: &super::repository::CallActivation,
    source: &super::repository::BusinessErrorSource,
    now_ms: i64,
) -> Result<Option<RuntimePlan>> {
    let mut transition = Transition::from_snapshot(snapshot, now_ms)?;
    transition.current_scope = call.parent_scope_id.clone();
    let token = transition
        .tokens
        .iter()
        .find(|t| {
            t.token_id == call.parent_token_id
                && t.status == "waiting"
                && t.scope_id == call.parent_scope_id
        })
        .context("error propagation lost exact call activation")?
        .clone();
    let (outputs, activity_result, code, source_instance, source_scope, source_event, source_kind) =
        match source {
            super::repository::BusinessErrorSource::ErrorEnd {
                instance_id,
                fact,
                outputs,
                ..
            } => (
                outputs.clone(),
                json!({"kind":"ErrorEnd","error_ref":fact.error_ref,"error_code":fact.error_code,"source_instance_id":instance_id,"source_scope_id":fact.source_scope_id,"source_node_id":fact.source_node_id,"source_event_id":fact.source_event_id}),
                Some(fact.error_code.as_str()),
                instance_id.as_str(),
                fact.source_scope_id.as_str(),
                fact.source_event_id.as_str(),
                "error_end",
            ),
            super::repository::BusinessErrorSource::ServiceContract {
                instance_id,
                scope_id,
                result_event_id,
                result,
                ..
            } => (
                result.outputs.clone(),
                serde_json::to_value(result)?,
                result.code.as_deref(),
                instance_id.as_str(),
                scope_id.as_str(),
                result_event_id.as_str(),
                "contract",
            ),
        };
    let Some((subscription, attached, _hops)) = transition.error_handler(&token, code)? else {
        return Ok(None);
    };
    transition.current_scope = subscription.scope_id.clone();
    let handler = transition.node(&subscription.node_id)?.clone();
    let ProcessNodeKind::BoundaryError { output_mapping, .. } = &handler.kind else {
        anyhow::bail!("call error handler is not BoundaryError")
    };
    transition.map_outputs(
        output_mapping,
        &outputs,
        &[("activity_result".into(), activity_result)],
    )?;
    transition.settle_subscription(
        &subscription,
        tentaflow_protocol::processes::ProcessSubscriptionStatus::Consumed,
        None,
    );
    transition.interrupt_activity(&attached, &subscription.subscription_id)?;
    transition.event("call_error_propagated",Some(handler.id.clone()),json!({"call_id":call.call_id,"source_instance_id":source_instance,"source_scope_id":source_scope,"source_event_id":source_event,"source_kind":source_kind,"handler_node_id":handler.id,"attached_token_id":attached.token_id,"error_code":code}));
    transition.event("business_error_caught",Some(handler.id.clone()),json!({"subscription_id":subscription.subscription_id,"attached_token_id":attached.token_id,"error_code":code,"source_event_id":source_event,"source_instance_id":source_instance,"source_kind":source_kind}));
    for edge in transition.outgoing(&handler.id) {
        transition.follow(&attached, &edge)?;
    }
    transition.advance()?;
    Ok(Some(transition.finish()?))
}

pub enum StartCause {
    Manual,
    Message { message_id: String },
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
                ProcessNodeKind::Start
                    | ProcessNodeKind::TimerStart { .. }
                    | ProcessNodeKind::MessageStart { .. }
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
        (ProcessNodeKind::MessageStart { .. }, StartCause::Message { message_id }) => {
            json!({"initiator_user_id":actor.user_id,"start_message_id":message_id})
        }
        (
            ProcessNodeKind::TimerStart { .. } | ProcessNodeKind::MessageStart { .. },
            StartCause::Manual,
        ) => {
            anyhow::bail!("a timer-start process cannot be started manually")
        }
        _ => anyhow::bail!("timer firing does not match the process start event"),
    };
    transition.plan.start_instance_id = Some(instance_id.to_owned());
    transition.plan.start_variables = Some(transition.plan.variables.clone());
    transition.create_token(ProcessToken {
        token_id: String::new(),
        scope_id: instance_id.to_owned(),
        node_id: start.id.clone(),
        arrival_edge_id: None,
        fork_stack: Vec::new(),
        status: "ready".into(),
    });
    transition.event("instance_started", None, facts);
    transition.advance()?;
    transition.finish()
}

pub fn plan_advance(snapshot: &RuntimeSnapshot, now_ms: i64) -> Result<RuntimePlan> {
    let mut transition = Transition::from_snapshot(snapshot, now_ms)?;
    transition.advance()?;
    transition.finish()
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
    transition.current_scope = task.scope_id.clone();
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
                return transition.finish();
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
    transition.map_outputs(mapping, &effective_outputs, &[])?;
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
    transition.finish()
}

pub fn plan_job_result(
    snapshot: &RuntimeSnapshot,
    job: &ProcessJob,
    observed: &super::repository::ObservedActivityResult,
    now_ms: i64,
) -> Result<RuntimePlan> {
    let result = &observed.result;
    validate_output(&result.outputs)?;
    let mut transition = Transition::from_snapshot(snapshot, now_ms)?;
    transition.current_scope = job.scope_id.clone();
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
    transition.event("service_result", Some(node.id.clone()), {
        let mut data = serde_json::to_value(result)?;
        data["result_origin"] = json!(super::repository::result_origin_text(&observed.origin));
        data
    });
    if observed.origin == super::repository::ActivityResultOrigin::Contract
        && result.outcome == ActivityOutcome::NeedsHuman
    {
        use tentaflow_protocol::processes::{
            ProcessSubscriptionKind as K, ProcessSubscriptionStatus as S,
        };
        let local = transition
            .subscriptions
            .iter()
            .filter(|subscription| {
                subscription.kind == K::BoundaryEscalation
                    && subscription.status == S::Open
                    && subscription.scope_id == job.scope_id
                    && subscription.token_id == token.token_id
            })
            .cloned()
            .collect::<Vec<_>>();
        let selected = local
            .iter()
            .find(|subscription| {
                subscription.escalation_code.is_some()
                    && subscription.escalation_code == result.code
            })
            .or_else(|| {
                local
                    .iter()
                    .find(|subscription| subscription.escalation_code.is_none())
            })
            .cloned();
        if let Some(subscription) = selected {
            let boundary_id = subscription.node_id.clone();
            let planned = (|| -> Result<RuntimePlan> {
                let handler = transition.node(&boundary_id)?.clone();
                let ProcessNodeKind::BoundaryEscalation {
                    output_mapping,
                    cancel_activity,
                    ..
                } = &handler.kind
                else {
                    anyhow::bail!("selected escalation subscription is not a boundary");
                };
                transition.map_outputs(
                    output_mapping,
                    &result.outputs,
                    &[("activity_result".into(), serde_json::to_value(result)?)],
                )?;
                let result_event_id = Uuid::new_v4().to_string();
                transition.plan.event_ids.insert(0, result_event_id.clone());
                transition.settle_subscription(&subscription, S::Consumed, None);
                if *cancel_activity {
                    transition.interrupt_activity(&token, &subscription.subscription_id)?;
                    transition
                        .plan
                        .cancel_job_ids
                        .retain(|id| id != &job.job_id);
                } else {
                    transition.user_task(
                        &node,
                        ProcessUserTaskKind::Verification,
                        transition.initiator.to_owned(),
                        serde_json::to_value(result)?,
                        &token.token_id,
                    );
                }
                transition.event(
                    "escalation_caught",
                    Some(handler.id.clone()),
                    json!({
                        "subscription_id": subscription.subscription_id,
                        "boundary_id": handler.id,
                        "attached_token_id": token.token_id,
                        "source_token_id": token.token_id,
                        "source_scope_id": job.scope_id,
                        "job_id": job.job_id,
                        "attempt": job.attempt,
                        "fence": job.fence,
                        "result_origin": "contract",
                        "cancel_activity": cancel_activity,
                        "result_event_id": result_event_id,
                        "code": result.code,
                        "matched_escalation_code": subscription.escalation_code,
                    }),
                );
                for edge in transition.outgoing(&handler.id) {
                    transition.follow(&token, &edge)?;
                }
                transition.escalation_continuation = true;
                transition.advance()?;
                ensure!(
                    transition.plan.add_incidents.is_empty(),
                    "escalation continuation created a failure wait"
                );
                ensure!(
                    transition.plan.terminal_error.is_none(),
                    "escalation continuation reached a terminal error"
                );
                transition.finish()
            })();
            return match planned {
                Ok(plan) => Ok(plan),
                Err(error) => plan_retained_escalation_incident(
                    snapshot,
                    job,
                    observed,
                    &boundary_id,
                    &error.to_string(),
                    now_ms,
                ),
            };
        }
    }
    if observed.origin == super::repository::ActivityResultOrigin::Contract
        && result.outcome == ActivityOutcome::Error
    {
        if let Some((subscription, attached, hops)) =
            transition.error_handler(&token, result.code.as_deref())?
        {
            let source_scope = transition.current_scope.clone();
            let result_index = transition.plan.events.len() - 1;
            transition.current_scope = subscription.scope_id.clone();
            let handler = transition.node(&subscription.node_id)?.clone();
            let ProcessNodeKind::BoundaryError { output_mapping, .. } = &handler.kind else {
                anyhow::bail!("error subscription is not a boundary");
            };
            transition.map_outputs(
                output_mapping,
                &result.outputs,
                &[("activity_result".into(), serde_json::to_value(result)?)],
            )?;
            transition.settle_subscription(
                &subscription,
                tentaflow_protocol::processes::ProcessSubscriptionStatus::Consumed,
                None,
            );
            transition.interrupt_activity(&attached, &subscription.subscription_id)?;
            transition
                .plan
                .cancel_job_ids
                .retain(|id| id != &job.job_id);
            for (index, from) in hops.iter().enumerate() {
                let to = hops.get(index + 1).unwrap_or(&subscription.scope_id);
                transition.event("scope_error_propagated", Some(handler.id.clone()), json!({
                    "source_scope_id":source_scope,"source_job_id":job.job_id,"source_token_id":token.token_id,
                    "from_scope_id":from,"to_scope_id":to,"handler_node_id":handler.id,"code":result.code,
                    "source_result_index":result_index,
                }));
            }
            transition.event("business_error_caught",Some(handler.id.clone()),json!({"subscription_id":subscription.subscription_id,"attached_token_id":attached.token_id,"job_id":job.job_id,"error_code":result.code,"result_origin":super::repository::result_origin_text(&observed.origin)}));
            for edge in transition.outgoing(&handler.id) {
                transition.follow(&attached, &edge)?;
            }
            transition.advance()?;
            return transition.finish();
        }
    }
    if observed.origin == super::repository::ActivityResultOrigin::Contract
        && result.outcome == ActivityOutcome::Error
    {
        let result_event_id = Uuid::new_v4().to_string();
        transition.plan.event_ids.insert(0, result_event_id.clone());
        transition.plan.business_error =
            Some(super::repository::BusinessErrorSource::ServiceContract {
                instance_id: snapshot.instance.instance_id.clone(),
                scope_id: job.scope_id.clone(),
                node_id: job.node_id.clone(),
                token_id: job.token_id.clone(),
                job_id: job.job_id.clone(),
                attempt: job.attempt,
                fence: job.fence,
                result_event_id,
                result: result.clone(),
            });
    }
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
        return transition.finish();
    }
    match verification {
        _ if result.outcome == ActivityOutcome::NeedsHuman => {
            transition.user_task(
                &node,
                ProcessUserTaskKind::Verification,
                transition.initiator.to_owned(),
                serde_json::to_value(result)?,
                &token.token_id,
            );
        }
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
            match condition(expression, &transition.effective()?, &result.outputs) {
                Ok(true) => match patch_variables(
                    output_mapping,
                    transition.local()?,
                    &transition.effective()?,
                    &result.outputs,
                    &[],
                ) {
                    Ok(variables) => {
                        transition.set_variables(variables)?;
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
    transition.finish()
}

pub(super) fn plan_retained_escalation_incident(
    snapshot: &RuntimeSnapshot,
    job: &ProcessJob,
    observed: &super::repository::ObservedActivityResult,
    boundary_id: &str,
    reason: &str,
    now_ms: i64,
) -> Result<RuntimePlan> {
    ensure!(
        observed.origin == super::repository::ActivityResultOrigin::Contract
            && observed.result.outcome == ActivityOutcome::NeedsHuman,
        "retained escalation incident requires an accepted Contract NeedsHuman result"
    );
    let mut transition = Transition::from_snapshot(snapshot, now_ms)?;
    transition.current_scope = job.scope_id.clone();
    let node = transition.node(&job.node_id)?.clone();
    ensure!(
        matches!(node.kind, ProcessNodeKind::ServiceTask { .. })
            && transition
                .tokens
                .iter()
                .any(|token| token.token_id == job.token_id
                    && token.scope_id == job.scope_id
                    && token.status == "waiting"),
        "retained escalation incident lost its factual service wait"
    );
    transition.plan.complete_job_ids.push(job.job_id.clone());
    transition
        .jobs
        .retain(|existing| existing.job_id != job.job_id);
    transition.event("service_result", Some(node.id.clone()), {
        let mut data = serde_json::to_value(&observed.result)?;
        data["result_origin"] = json!("contract");
        data
    });
    transition.incident(
        &node.id,
        Some(job.job_id.clone()),
        "ESCALATION_HANDLER_FAILED",
        super::repository::bounded_failure_message(&format!("boundary {boundary_id}: {reason}")),
    );
    transition.finish()
}

pub(super) fn plan_message_catch(
    snapshot: &RuntimeSnapshot,
    subscription: &super::repository::EventSubscription,
    payload: &Value,
    metadata: Value,
    now_ms: i64,
) -> Result<RuntimePlan> {
    let mut transition = Transition::from_snapshot(snapshot, now_ms)?;
    transition.current_scope = subscription.scope_id.clone();
    let node = transition.node(&subscription.node_id)?.clone();
    let (mapping, interrupt) = match &node.kind {
        ProcessNodeKind::MessageCatch { output_mapping, .. } => (output_mapping, None),
        ProcessNodeKind::BoundaryMessage {
            output_mapping,
            cancel_activity,
            ..
        } => (output_mapping, Some(*cancel_activity)),
        _ => anyhow::bail!("message target is not a catch"),
    };
    let token = transition
        .tokens
        .iter()
        .find(|t| t.token_id == subscription.token_id && t.status == "waiting")
        .context("subscription activation is closed")?
        .clone();
    transition.map_outputs(mapping, payload, &[("message".into(), metadata.clone())])?;
    transition.settle_subscription(
        subscription,
        tentaflow_protocol::processes::ProcessSubscriptionStatus::Consumed,
        None,
    );
    for link in snapshot.boundary_incidents.iter().filter(|link|matches!(&link.activation,super::repository::BoundaryActivationId::Subscription(id)if id==&subscription.subscription_id)) {transition.resolve_incident(&link.incident_id);}
    if let Some(race) = &subscription.race_id {
        transition.win_race(race, &node.id, Some(&subscription.subscription_id), None)?;
    }
    match interrupt {
        Some(true) => transition.interrupt_activity(&token, &subscription.subscription_id)?,
        Some(false) => {}
        None => transition.consume(&token.token_id),
    }
    transition.event("message_delivered",Some(node.id.clone()),json!({"subscription_id":subscription.subscription_id,"attached_token_id":subscription.token_id,"message_id":metadata["message_id"],"message":metadata,"payload":payload}));
    for edge in transition.outgoing(&node.id) {
        transition.follow(&token, &edge)?;
    }
    transition.advance()?;
    transition.finish()
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
    transition.current_scope = timer.scope_id.clone().context("catch timer lacks scope")?;
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
    if let Some(race_id) = &timer.race_id {
        transition.win_race(race_id, &timer.node_id, None, Some(&timer.timer_id))?;
    }
    transition.consume(token_id);
    for edge in transition.outgoing(&timer.node_id) {
        transition.follow(&token, &edge)?;
    }
    transition.advance()?;
    transition.finish()
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
    transition.current_scope = timer
        .scope_id
        .clone()
        .context("boundary timer lacks scope")?;
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
    transition.finish()
}

struct RunningJob {
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

pub fn signal_cancelled_claims(dispatcher: &FlowDispatcher, claims: &[CancelledJobClaim]) {
    if let Some(runtime) = dispatcher.process_runtime.lock().as_ref() {
        runtime.signal_cancelled_claims(claims);
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
            let messages =
                super::messages::drain_pending(&self.db, chrono::Utc::now().timestamp_millis());
            self.signal_cancelled_claims(&messages.cancelled_claims);
            if let Err(error) = messages.completion {
                tracing::error!(error=%error,"process message drain failed");
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
                        if let Err(error) = super::repository::fail_job(&worker.db, &job_id, attempt, fence, &worker.worker_id, "WORKER_ERROR", &error, chrono::Utc::now().timestamp_millis(), None) {
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

                    result_expression: None,
                },
            },
        );
        model.sequence_flows = vec![
            edge("ToService", "Start_1", "Service"),
            edge("ToEnd", "Service", "End_1"),
        ];
        model
    }

    pub fn embedded_model(mut model: ProcessModel, scope_id: &str) -> ProcessModel {
        let body = tentaflow_protocol::processes::ProcessSubProcess {
            nodes: std::mem::take(&mut model.nodes),
            sequence_flows: std::mem::take(&mut model.sequence_flows),
            variables: std::mem::take(&mut model.variables),
            diagram: std::mem::take(&mut model.diagram),
        };
        let start_id = format!("RootStart_{scope_id}");
        let end_id = format!("RootEnd_{scope_id}");
        model.nodes = vec![
            ProcessNode {
                id: start_id.clone(),
                name: "Enter embedded work".into(),
                kind: ProcessNodeKind::Start,
            },
            ProcessNode {
                id: scope_id.into(),
                name: format!("Embedded {scope_id}"),
                kind: ProcessNodeKind::SubProcess {
                    body,
                    input_mapping: BTreeMap::new(),
                    output_mapping: BTreeMap::new(),
                },
            },
            ProcessNode {
                id: end_id.clone(),
                name: "Finish embedded work".into(),
                kind: ProcessNodeKind::End,
            },
        ];
        model.sequence_flows = vec![
            edge(&format!("Enter_{scope_id}"), &start_id, scope_id),
            edge(&format!("Leave_{scope_id}"), scope_id, &end_id),
        ];
        model
    }

    pub fn with_boundaries(
        mut model: ProcessModel,
        activity: &str,
        boundaries: &[(&str, bool, u32)],
    ) -> ProcessModel {
        model.timer_timezone = Some("UTC".into());
        let end_id = model
            .nodes
            .iter()
            .find(|node| node.kind == ProcessNodeKind::End)
            .unwrap()
            .id
            .clone();
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
                .push(edge(&format!("From_{id}"), id, &end_id));
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
        for node in super::super::model::all_nodes(model) {
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
            None,
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
            &version.model,
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
                let instance =
                    super::super::repository::get_instance(pool, actor, instance_id, None)
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
        .unwrap()
        .instance;
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
        .unwrap()
        .instance;
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
        .unwrap()
        .instance;
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
        .unwrap()
        .instance;
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
        .unwrap()
        .instance;
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

    fn inclusive_model(default: Option<&str>, a: bool, b: bool) -> ProcessModel {
        let mut model = super::super::model::starter_model();
        model.variables.insert("a".into(), json!(a));
        model.variables.insert("b".into(), json!(b));
        model.nodes.splice(1..1, [
            ProcessNode { id: "Split".into(), name: "Choose reviews".into(),
                kind: ProcessNodeKind::InclusiveGateway { default_flow_id: default.map(str::to_owned) } },
            ProcessNode { id: "A".into(), name: "Review A".into(),
                kind: ProcessNodeKind::UserTask { assignee_user_id: None, output_mapping: BTreeMap::new() } },
            ProcessNode { id: "B".into(), name: "Review B".into(),
                kind: ProcessNodeKind::UserTask { assignee_user_id: None, output_mapping: BTreeMap::new() } },
            ProcessNode { id: "C".into(), name: "Review C".into(),
                kind: ProcessNodeKind::UserTask { assignee_user_id: None, output_mapping: BTreeMap::new() } },
            ProcessNode { id: "Join".into(), name: "Selected reviews".into(),
                kind: ProcessNodeKind::InclusiveGateway { default_flow_id: None } },
        ]);
        model.sequence_flows = vec![
            edge("StartSplit", "Start_1", "Split"), edge("To_A", "Split", "A"),
            edge("To_B", "Split", "B"), edge("To_C", "Split", "C"),
            edge("From_A", "A", "Join"), edge("From_B", "B", "Join"),
            edge("From_C", "C", "Join"), edge("JoinEnd", "Join", "End_1"),
        ];
        model.sequence_flows[1].condition = Some("vars.a == true".into());
        model.sequence_flows[2].condition = Some("vars.b == true".into());
        if default.is_none() { model.sequence_flows[3].condition = Some("false".into()); }
        model
    }

    #[tokio::test]
    async fn inclusive_selected_pair_reopens_after_one_arrival_and_joins_once() {
        let fixture = Fixture::new();
        let waiting = start_model(&fixture, &inclusive_model(Some("To_C"), true, true));
        assert_eq!(waiting.status, ProcessInstanceStatus::Waiting);
        assert_eq!(waiting.user_tasks.len(), 2);
        assert!(waiting.user_tasks.iter().all(|task| task.node_id != "C"));
        let first = waiting.user_tasks.iter().find(|task| task.node_id == "B").unwrap();
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id).unwrap();
        let at_ms = chrono::Utc::now().timestamp_millis();
        let plan = plan_user_completion(&snapshot, &first.user_task_id, &Value::Null, None, at_ms).unwrap();
        let partial = repository::complete_user_task(&fixture.db, &fixture.owner, &stamp("inclusive first"),
            &waiting.instance_id, &first.user_task_id, waiting.revision, &Value::Null, None, &plan, at_ms).unwrap().instance;
        assert_eq!(partial.status, ProcessInstanceStatus::Waiting);
        let path = fixture.directory.path().join("processes.db");
        let owner = fixture.owner.clone();
        let Fixture { directory, db, router, .. } = fixture;
        drop(router); drop(db);
        let reopened = crate::db::init(&path).unwrap();
        let snapshot = repository::runtime_snapshot(&reopened, &owner, &waiting.instance_id).unwrap();
        assert_eq!(snapshot.receipts.len(), 1);
        assert_eq!(snapshot.receipts[0].branch_edge_id, "To_B");
        assert_eq!(snapshot.receipts[0].gateway_kind, GatewayKind::Inclusive);
        let second = snapshot.instance.user_tasks.iter().find(|task| task.node_id == "A").unwrap();
        let plan = plan_user_completion(&snapshot, &second.user_task_id, &Value::Null, None, at_ms).unwrap();
        let command = stamp("inclusive second");
        let completed = repository::complete_user_task(&reopened, &owner, &command,
            &waiting.instance_id, &second.user_task_id, partial.revision, &Value::Null, None, &plan, at_ms).unwrap().instance;
        assert_eq!(completed.status, ProcessInstanceStatus::Completed);
        let replay = repository::complete_user_task(&reopened, &owner, &command,
            &waiting.instance_id, &second.user_task_id, partial.revision, &Value::Null, None, &plan, at_ms).unwrap().instance;
        assert_eq!(replay.revision, completed.revision);
        let events = repository::list_events(&reopened, &owner, &waiting.instance_id, 0, 200).unwrap().0;
        let split = events.iter().find(|event| event.kind == "inclusive_split").unwrap();
        assert_eq!(split.data["selected_branch_edge_ids"], json!(["To_A", "To_B"]));
        assert_eq!(split.data["default_selected"], false);
        assert_eq!(events.iter().filter(|event| event.kind == "inclusive_joined").count(), 1);
        assert_eq!(events.iter().filter(|event| event.kind == "end_reached").count(), 1);
        drop(reopened); drop(directory);
    }

    #[tokio::test]
    async fn nested_gateway_pairs_preserve_each_selected_activation() {
        for (outer_inclusive, inner_inclusive, inner_b, completion_order) in [
            (true, false, true, &["C", "A", "B"][..]),
            (true, true, false, &["A", "C"][..]),
            (false, true, true, &["C", "A", "B"][..]),
        ] {
            let fixture = Fixture::new();
            let mut model = super::super::model::starter_model();
            model.nodes.insert(1, ProcessNode { id: "OuterSplit".into(), name: "Choose".into(),
                kind: if outer_inclusive { ProcessNodeKind::InclusiveGateway { default_flow_id: None } }
                    else { ProcessNodeKind::ParallelGateway } });
            model.nodes.insert(2, ProcessNode { id: "InnerSplit".into(), name: "Nested choice".into(),
                kind: if inner_inclusive { ProcessNodeKind::InclusiveGateway { default_flow_id: None } }
                    else { ProcessNodeKind::ParallelGateway } });
            model.nodes.insert(3, ProcessNode { id: "InnerJoin".into(), name: "Nested done".into(),
                kind: if inner_inclusive { ProcessNodeKind::InclusiveGateway { default_flow_id: None } }
                    else { ProcessNodeKind::ParallelGateway } });
            model.nodes.insert(4, ProcessNode { id: "OuterJoin".into(), name: "Selected done".into(),
                kind: if outer_inclusive { ProcessNodeKind::InclusiveGateway { default_flow_id: None } }
                    else { ProcessNodeKind::ParallelGateway } });
            for id in ["A", "B", "C"] {
                model.nodes.push(ProcessNode { id: id.into(), name: id.into(),
                    kind: ProcessNodeKind::UserTask { assignee_user_id: None, output_mapping: BTreeMap::new() } });
            }
            model.sequence_flows = vec![
                edge("StartOuter", "Start_1", "OuterSplit"),
                edge("OuterInner", "OuterSplit", "InnerSplit"),
                edge("OuterC", "OuterSplit", "C"),
                edge("InnerA", "InnerSplit", "A"),
                edge("InnerB", "InnerSplit", "B"),
                edge("AInner", "A", "InnerJoin"),
                edge("BInner", "B", "InnerJoin"),
                edge("InnerOuter", "InnerJoin", "OuterJoin"),
                edge("COuter", "C", "OuterJoin"),
                edge("OuterEnd", "OuterJoin", "End_1"),
            ];
            if outer_inclusive {
                model.sequence_flows[1].condition = Some("true".into());
                model.sequence_flows[2].condition = Some("true".into());
            }
            if inner_inclusive {
                model.sequence_flows[3].condition = Some("true".into());
                model.sequence_flows[4].condition = Some(if inner_b { "true" } else { "false" }.into());
            }
            let mut instance = start_model(&fixture, &model);
            assert_eq!(instance.user_tasks.len(), completion_order.len());
            for node_id in completion_order {
                let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &instance.instance_id).unwrap();
                let task = snapshot.instance.user_tasks.iter().find(|task| task.node_id == *node_id).unwrap();
                let at_ms = chrono::Utc::now().timestamp_millis();
                let plan = plan_user_completion(&snapshot, &task.user_task_id, &Value::Null, None, at_ms).unwrap();
                instance = repository::complete_user_task(&fixture.db, &fixture.owner,
                    &stamp(&format!("nested {node_id}")), &instance.instance_id, &task.user_task_id,
                    instance.revision, &Value::Null, None, &plan, at_ms).unwrap().instance;
            }
            assert_eq!(instance.status, ProcessInstanceStatus::Completed);
            let events = repository::list_events(&fixture.db, &fixture.owner, &instance.instance_id, 0, 200).unwrap().0;
            let outer_join_kind = if outer_inclusive { "inclusive_joined" } else { "parallel_joined" };
            let inner_join_kind = if inner_inclusive { "inclusive_joined" } else { "parallel_joined" };
            assert_eq!(events.iter().filter(|event| event.kind == outer_join_kind && event.node_id.as_deref() == Some("OuterJoin")).count(), 1);
            assert_eq!(events.iter().filter(|event| event.kind == inner_join_kind && event.node_id.as_deref() == Some("InnerJoin")).count(), 1);
            if inner_inclusive {
                let selected = events.iter().find(|event| event.kind == "inclusive_split" && event.node_id.as_deref() == Some("InnerSplit")).unwrap();
                assert_eq!(selected.data["selected_branch_edge_ids"], if inner_b { json!(["InnerA", "InnerB"]) } else { json!(["InnerA"]) });
            }
            if outer_inclusive {
                let selected = events.iter().find(|event| event.kind == "inclusive_split" && event.node_id.as_deref() == Some("OuterSplit")).unwrap();
                assert_eq!(selected.data["selected_branch_edge_ids"], json!(["OuterC", "OuterInner"]));
            }
        }
    }

    #[tokio::test]
    async fn parallel_nine_branch_join_keeps_all_selected_edges() {
        let fixture = Fixture::new();
        let mut model = super::super::model::starter_model();
        model.nodes.insert(1, ProcessNode { id: "Split".into(), name: "Parallel".into(),
            kind: ProcessNodeKind::ParallelGateway });
        model.nodes.insert(2, ProcessNode { id: "Join".into(), name: "All branches".into(),
            kind: ProcessNodeKind::ParallelGateway });
        model.sequence_flows = vec![edge("StartSplit", "Start_1", "Split")];
        for index in 0..9 {
            let node_id = format!("Branch_{index}");
            model.nodes.push(ProcessNode { id: node_id.clone(), name: node_id.clone(),
                kind: ProcessNodeKind::UserTask { assignee_user_id: None, output_mapping: BTreeMap::new() } });
            model.sequence_flows.push(edge(&format!("To_{index}"), "Split", &node_id));
            model.sequence_flows.push(edge(&format!("From_{index}"), &node_id, "Join"));
        }
        model.sequence_flows.push(edge("JoinEnd", "Join", "End_1"));
        let mut instance = start_model(&fixture, &model);
        assert_eq!(instance.user_tasks.len(), 9);
        for index in 0..9 {
            let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &instance.instance_id).unwrap();
            let task = snapshot.instance.user_tasks.iter().find(|task| task.status == ProcessUserTaskStatus::Open).unwrap();
            let at_ms = chrono::Utc::now().timestamp_millis();
            let plan = plan_user_completion(&snapshot, &task.user_task_id, &Value::Null, None, at_ms).unwrap();
            instance = repository::complete_user_task(&fixture.db, &fixture.owner,
                &stamp(&format!("parallel nine branch {index}")), &instance.instance_id,
                &task.user_task_id, instance.revision, &Value::Null, None, &plan, at_ms).unwrap().instance;
        }
        assert_eq!(instance.status, ProcessInstanceStatus::Completed);
        let events = repository::list_events(&fixture.db, &fixture.owner, &instance.instance_id, 0, 200).unwrap().0;
        assert_eq!(events.iter().filter(|event| event.kind == "parallel_joined").count(), 1);
    }

    #[tokio::test]
    async fn inclusive_writer_rejects_conflicting_selected_set_without_partial_receipt() {
        let fixture = Fixture::new();
        let waiting = start_model(&fixture, &inclusive_model(None, true, true));
        let task = waiting.user_tasks.iter().find(|task| task.node_id == "B").unwrap();
        let before = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id).unwrap();
        let at_ms = chrono::Utc::now().timestamp_millis();
        let mut plan = plan_user_completion(&before, &task.user_task_id, &Value::Null, None, at_ms).unwrap();
        let joining = plan.create_tokens.iter_mut().find(|token| token.status == "joining").unwrap();
        joining.fork_stack.last_mut().unwrap().selected_branch_edge_ids = vec!["To_B".into()];
        assert!(repository::complete_user_task(&fixture.db, &fixture.owner,
            &stamp("forged selected set"), &waiting.instance_id, &task.user_task_id,
            waiting.revision, &Value::Null, None, &plan, at_ms).is_err());
        let after = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id).unwrap();
        assert_eq!(after.instance.revision, before.instance.revision);
        assert_eq!(after.tokens.len(), before.tokens.len());
        assert_eq!(after.receipts.len(), before.receipts.len());
        assert_eq!(after.instance.user_tasks.len(), before.instance.user_tasks.len());
        let events = repository::list_events(&fixture.db, &fixture.owner, &waiting.instance_id, 0, 200).unwrap().0;
        assert!(events.iter().all(|event| event.kind != "inclusive_joined"));
        let mut wrong_arrival = plan_user_completion(&before, &task.user_task_id, &Value::Null, None, at_ms).unwrap();
        let joining = wrong_arrival.create_tokens.iter_mut().find(|token| token.status == "joining").unwrap();
        joining.arrival_edge_id = Some("From_A".into());
        assert!(repository::complete_user_task(&fixture.db, &fixture.owner,
            &stamp("wrong join incoming edge"), &waiting.instance_id, &task.user_task_id,
            waiting.revision, &Value::Null, None, &wrong_arrival, at_ms).is_err());
        let after = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id).unwrap();
        assert_eq!(after.instance.revision, before.instance.revision);
        assert!(after.receipts.is_empty());
    }

    #[tokio::test]
    async fn inclusive_last_arrival_rejects_forgery_atomically_then_joins_once() {
        let fixture = Fixture::new();
        let waiting = start_model(&fixture, &inclusive_model(None, true, true));
        let first = waiting.user_tasks.iter().find(|task| task.node_id == "B").unwrap();
        let at_ms = chrono::Utc::now().timestamp_millis();
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id).unwrap();
        let first_plan = plan_user_completion(&snapshot, &first.user_task_id, &Value::Null, None, at_ms).unwrap();
        let partial = repository::complete_user_task(&fixture.db, &fixture.owner,
            &stamp("first selected branch"), &waiting.instance_id, &first.user_task_id,
            waiting.revision, &Value::Null, None, &first_plan, at_ms).unwrap().instance;
        assert_eq!(partial.status, ProcessInstanceStatus::Waiting);
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id).unwrap();
        assert_eq!(snapshot.receipts.len(), 1);
        let second = snapshot.instance.user_tasks.iter().find(|task| task.node_id == "A").unwrap();
        let valid = plan_user_completion(&snapshot, &second.user_task_id, &Value::Null, None, at_ms).unwrap();
        assert_eq!(valid.remove_gateway_receipts.len(), 1);
        assert_eq!(valid.events.iter().filter(|event| event.kind == "inclusive_joined").count(), 1);
        let rows = || {
            let conn = fixture.db.read().unwrap();
            ["bpmn_instances", "bpmn_scopes", "bpmn_tokens", "bpmn_gateway_receipts",
                "bpmn_user_tasks", "bpmn_jobs", "bpmn_incidents", "bpmn_timers",
                "bpmn_event_subscriptions", "bpmn_event_races", "bpmn_messages", "bpmn_calls",
                "bpmn_events", "bpmn_commands"]
                .iter().map(|table| super::super::call_pin_tests::table_rows(&conn, table, "rowid"))
                .collect::<Vec<_>>()
        };
        let before = rows();
        let mut forged_frame = valid.clone();
        let joining = forged_frame.create_tokens.iter_mut().find(|token| token.status == "joining").unwrap();
        joining.fork_stack.last_mut().unwrap().selected_branch_edge_ids = vec!["To_A".into()];
        assert!(repository::complete_user_task(&fixture.db, &fixture.owner,
            &stamp("forged final selected set"), &waiting.instance_id, &second.user_task_id,
            partial.revision, &Value::Null, None, &forged_frame, at_ms).is_err());
        assert_eq!(rows(), before);
        let mut forged_event = valid.clone();
        forged_event.events.iter_mut().find(|event| event.kind == "inclusive_joined").unwrap()
            .data["selected_branch_edge_ids"] = json!(["To_A"]);
        assert!(repository::complete_user_task(&fixture.db, &fixture.owner,
            &stamp("forged final join event"), &waiting.instance_id, &second.user_task_id,
            partial.revision, &Value::Null, None, &forged_event, at_ms).is_err());
        assert_eq!(rows(), before);
        let command = stamp("valid final selected join");
        let complete = repository::complete_user_task(&fixture.db, &fixture.owner,
            &command, &waiting.instance_id, &second.user_task_id, partial.revision,
            &Value::Null, None, &valid, at_ms).unwrap().instance;
        assert_eq!(complete.status, ProcessInstanceStatus::Completed);
        let completed_rows = rows();
        let replay = repository::complete_user_task(&fixture.db, &fixture.owner,
            &command, &waiting.instance_id, &second.user_task_id, partial.revision,
            &Value::Null, None, &valid, at_ms).unwrap().instance;
        assert_eq!(replay.revision, complete.revision);
        assert_eq!(rows(), completed_rows);
        let events = repository::list_events(&fixture.db, &fixture.owner, &waiting.instance_id, 0, 200).unwrap().0;
        assert_eq!(events.iter().filter(|event| event.kind == "inclusive_joined").count(), 1);
        assert_eq!(events.iter().filter(|event| event.kind == "end_reached").count(), 1);
    }

    #[tokio::test]
    async fn inclusive_single_selected_activation_requires_factual_join_before_disappearing() {
        let fixture = Fixture::new();
        let waiting = start_model(&fixture, &inclusive_model(None, true, false));
        assert_eq!(waiting.user_tasks.len(), 1);
        let task = &waiting.user_tasks[0];
        assert_eq!(task.node_id, "A");
        let at_ms = chrono::Utc::now().timestamp_millis();
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id).unwrap();
        let valid = plan_user_completion(&snapshot, &task.user_task_id, &Value::Null, None, at_ms).unwrap();
        assert!(valid.remove_gateway_receipts.is_empty());
        assert_eq!(valid.events.iter().filter(|event| event.kind == "inclusive_joined").count(), 1);
        let rows = || {
            let conn = fixture.db.read().unwrap();
            ["bpmn_instances", "bpmn_scopes", "bpmn_tokens", "bpmn_gateway_receipts",
                "bpmn_user_tasks", "bpmn_jobs", "bpmn_incidents", "bpmn_timers",
                "bpmn_event_subscriptions", "bpmn_event_races", "bpmn_messages", "bpmn_calls",
                "bpmn_events", "bpmn_commands"]
                .iter().map(|table| super::super::call_pin_tests::table_rows(&conn, table, "rowid"))
                .collect::<Vec<_>>()
        };
        let before = rows();
        let mut missing_event = valid.clone();
        missing_event.events.retain(|event| event.kind != "inclusive_joined");
        let error = repository::complete_user_task(&fixture.db, &fixture.owner,
            &stamp("singleton omitted join event"), &waiting.instance_id, &task.user_task_id,
            waiting.revision, &Value::Null, None, &missing_event, at_ms).err().unwrap();
        assert!(format!("{error:#}").contains("gateway activation disappeared"));
        assert_eq!(rows(), before);
        let mut missing_arrival = missing_event.clone();
        let joining = missing_arrival.create_tokens.iter().find(|token| token.status == "joining")
            .unwrap().token_id.clone();
        missing_arrival.create_tokens.retain(|token| token.token_id != joining);
        missing_arrival.consume_token_ids.retain(|id| id != &joining);
        let error = repository::complete_user_task(&fixture.db, &fixture.owner,
            &stamp("singleton omitted join arrival and event"), &waiting.instance_id,
            &task.user_task_id, waiting.revision, &Value::Null, None, &missing_arrival, at_ms)
            .err().unwrap();
        assert!(format!("{error:#}").contains("gateway activation disappeared"));
        assert_eq!(rows(), before);
        let mut changed_replacement = valid.clone();
        let replacement = changed_replacement.create_tokens.iter_mut()
            .find(|token| token.status == "joining").unwrap();
        replacement.fork_stack.last_mut().unwrap().selected_branch_edge_ids =
            vec!["To_A".into(), "To_B".into()];
        let replacement_id = replacement.token_id.clone();
        changed_replacement.consume_token_ids.retain(|id| id != &replacement_id);
        let error = repository::complete_user_task(&fixture.db, &fixture.owner,
            &stamp("singleton changed replacement selection"), &waiting.instance_id,
            &task.user_task_id, waiting.revision, &Value::Null, None, &changed_replacement,
            at_ms).err().unwrap();
        assert!(format!("{error:#}").contains("planned gateway frame changed"));
        assert_eq!(rows(), before);
        let command = stamp("singleton factual join");
        let completed = repository::complete_user_task(&fixture.db, &fixture.owner,
            &command, &waiting.instance_id, &task.user_task_id, waiting.revision,
            &Value::Null, None, &valid, at_ms).unwrap().instance;
        assert_eq!(completed.status, ProcessInstanceStatus::Completed);
        let committed_rows = rows();
        let replay = repository::complete_user_task(&fixture.db, &fixture.owner,
            &command, &waiting.instance_id, &task.user_task_id, waiting.revision,
            &Value::Null, None, &valid, at_ms).unwrap().instance;
        assert_eq!(replay.revision, completed.revision);
        assert_eq!(rows(), committed_rows);
        let events = repository::list_events(&fixture.db, &fixture.owner, &waiting.instance_id, 0, 200).unwrap().0;
        assert_eq!(events.iter().filter(|event| event.kind == "inclusive_joined").count(), 1);
        assert_eq!(events.iter().filter(|event| event.kind == "end_reached").count(), 1);
    }

    #[test]
    fn runtime_fork_frame_rejects_unknown_fields_and_rolls_back_transition() {
        let fixture = Fixture::new();
        let waiting = start_model(&fixture, &inclusive_model(None, true, true));
        let task = waiting.user_tasks.iter().find(|task| task.node_id == "A").unwrap();
        let at_ms = chrono::Utc::now().timestamp_millis();
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id).unwrap();
        let plan = plan_user_completion(&snapshot, &task.user_task_id, &Value::Null, None, at_ms).unwrap();
        let token_id = snapshot.tokens.iter().find(|token| token.node_id == "A").unwrap().token_id.clone();
        let original: String = fixture.db.read().unwrap().query_row(
            "SELECT fork_stack_json FROM bpmn_tokens WHERE token_id=?1", [&token_id], |row| row.get(0)).unwrap();
        let mut malformed: Value = serde_json::from_str(&original).unwrap();
        malformed[0].as_object_mut().unwrap().insert("unexpected".into(), json!(1));
        assert!(serde_json::from_value::<ForkFrame>(malformed[0].clone()).is_err());
        fixture.db.write().unwrap().execute(
            "UPDATE bpmn_tokens SET fork_stack_json=?1 WHERE token_id=?2",
            rusqlite::params![malformed.to_string(), token_id]).unwrap();
        let before = {
            let conn = fixture.db.read().unwrap();
            ["bpmn_instances", "bpmn_tokens", "bpmn_gateway_receipts", "bpmn_user_tasks",
                "bpmn_events", "bpmn_commands"].iter()
                .map(|table| super::super::call_pin_tests::table_rows(&conn, table, "rowid"))
                .collect::<Vec<_>>()
        };
        assert!(repository::complete_user_task(&fixture.db, &fixture.owner,
            &stamp("malformed frame"), &waiting.instance_id, &task.user_task_id,
            waiting.revision, &Value::Null, None, &plan, at_ms).is_err());
        let after = {
            let conn = fixture.db.read().unwrap();
            ["bpmn_instances", "bpmn_tokens", "bpmn_gateway_receipts", "bpmn_user_tasks",
                "bpmn_events", "bpmn_commands"].iter()
                .map(|table| super::super::call_pin_tests::table_rows(&conn, table, "rowid"))
                .collect::<Vec<_>>()
        };
        assert_eq!(after, before);
    }

    #[tokio::test]
    async fn inclusive_default_and_nonretryable_failures_use_factual_waiting_tokens() {
        let fixture = Fixture::new();
        let default = start_model(&fixture, &inclusive_model(Some("To_C"), false, false));
        assert_eq!(default.user_tasks.len(), 1);
        assert_eq!(default.user_tasks[0].node_id, "C");
        let events = repository::list_events(&fixture.db, &fixture.owner, &default.instance_id, 0, 200).unwrap().0;
        let split = events.iter().find(|event| event.kind == "inclusive_split").unwrap();
        assert_eq!(split.data["selected_branch_edge_ids"], json!(["To_C"]));
        assert_eq!(split.data["default_selected"], true);
        for (condition, reason) in [("false", "no_matching_flow"), ("1", "non_boolean_condition"), ("1 / 0 == 1", "condition_evaluation_failed")] {
            let mut model = inclusive_model(None, false, false);
            model.sequence_flows[1].condition = Some(condition.into());
            let blocked = start_model(&fixture, &model);
            assert_eq!(blocked.status, ProcessInstanceStatus::Incident);
            assert_eq!(blocked.incidents.len(), 1);
            assert_eq!(blocked.incidents[0].code, "INCLUSIVE_GATEWAY_ERROR");
            assert!(!blocked.incidents[0].can_retry);
            assert!(blocked.incidents[0].job_id.is_none());
            let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &blocked.instance_id).unwrap();
            assert!(snapshot.receipts.is_empty());
            assert_eq!(snapshot.tokens.len(), 1);
            assert_eq!(snapshot.tokens[0].status, "waiting");
            assert!(snapshot.tokens[0].fork_stack.is_empty());
            let events = repository::list_events(&fixture.db, &fixture.owner, &blocked.instance_id, 0, 200).unwrap().0;
            let incident = events.iter().find(|event| event.kind == "incident").unwrap();
            assert_eq!(incident.data["reason"], reason);
            if reason == "no_matching_flow" { assert!(incident.data["condition_edge_id"].is_null()); }
            else { assert_eq!(incident.data["condition_edge_id"], "To_A"); }
            assert_eq!(incident.data["waiting_token_id"], snapshot.tokens[0].token_id);
            assert!(events.iter().all(|event| event.kind != "inclusive_split"));
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
                attempt: claim.job.attempt,
                fence: claim.job.fence,
                worker_id: claim.job.worker_id.clone().unwrap(),
                cancel: cancel.clone(),
            },
        );
    }

    #[tokio::test]
    async fn boundary_claim_registration_race_never_enters_the_real_flow_executor() {
        for called in [false, true] {
            let fixture = Fixture::new();
            let flow_id = flow(
                &fixture.db,
                &fixture.owner,
                &graph("must not execute", None),
            );
            let service = service_model(
                &flow_id,
                ActivityVerification::Condition {
                    expression: "true".into(),
                },
            );
            let (activity, model) = if called {
                let target = publish_model(&fixture, &service);
                (
                    "Call_1",
                    super::super::call_tests::caller(&target, BTreeMap::new()),
                )
            } else {
                ("Scope", embedded_model(service, "Scope"))
            };
            let model = with_boundaries(model, activity, &[("Limit", true, 1)]);
            let started = start_model(&fixture, &model);
            let claim = repository::claim_job(
                &fixture.db,
                "registration-window",
                chrono::Utc::now().timestamp_millis(),
            )
            .unwrap()
            .unwrap();
            let due =
                repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                    .unwrap()
                    .timers[0]
                    .due_at_ms
                    .unwrap();
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
                repository::runtime_snapshot(&fixture.db, &fixture.owner, &claim.job.instance_id)
                    .unwrap();
            assert_eq!(snapshot.jobs[0].status, "cancelled");
            assert_eq!(snapshot.jobs[0].fence, claim.job.fence + 1);
            assert!(snapshot.jobs[0].result.is_none());
            assert!(!snapshot.instance.can_retry);
            if called {
                assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Cancelled);
                assert_eq!(
                    repository::get_instance(
                        &fixture.db,
                        &fixture.owner,
                        &started.instance_id,
                        None
                    )
                    .unwrap()
                    .status,
                    ProcessInstanceStatus::Completed
                );
            }
        }
    }

    #[tokio::test]
    async fn cancelled_generation_signal_and_old_cleanup_cannot_touch_an_actual_retried_claim() {
        for called in [false, true] {
            let fixture = Fixture::new();
            let flow_id = flow(&fixture.db, &fixture.owner, &graph("new generation", None));
            let service = service_model(
                &flow_id,
                ActivityVerification::Condition {
                    expression: "true".into(),
                },
            );
            let model = if called {
                let version = publish_model(&fixture, &service);
                super::super::call_tests::caller(&version, BTreeMap::new())
            } else {
                embedded_model(service, "Scope")
            };
            let started = start_model(&fixture, &model);
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
                chrono::Utc::now().timestamp_millis(),
                None
            )
            .unwrap());
            let incident =
                repository::get_instance(&fixture.db, &fixture.owner, &old.job.instance_id, None)
                    .unwrap();
            repository::retry_job(
                &fixture.db,
                &fixture.owner,
                &stamp("retry real generation"),
                &old.job.instance_id,
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
                repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None)
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
            assert!(repository::runtime_snapshot(
                &fixture.db,
                &fixture.owner,
                &started.instance_id
            )
            .unwrap()
            .scopes
            .iter()
            .all(|scope| scope.status == ProcessInstanceStatus::Completed));
            if called {
                assert_eq!(
                    repository::get_instance(
                        &fixture.db,
                        &fixture.owner,
                        &new.job.instance_id,
                        None
                    )
                    .unwrap()
                    .status,
                    ProcessInstanceStatus::Completed
                );
            }
        }
    }

    #[tokio::test]
    async fn midbatch_sqlite_failure_keeps_prior_committed_cancellation_and_signals_exact_worker() {
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("queued effects", None));
        let mut instances = Vec::new();
        for seconds in [1, 2] {
            let model = with_boundaries(
                embedded_model(
                    service_model(
                        &flow_id,
                        ActivityVerification::Condition {
                            expression: "true".into(),
                        },
                    ),
                    "Scope",
                ),
                "Scope",
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
        assert!(committed
            .scopes
            .iter()
            .filter(|scope| scope.parent_scope_id.is_some())
            .all(|scope| scope.status == ProcessInstanceStatus::Cancelled));
        assert!(rolled_back
            .scopes
            .iter()
            .filter(|scope| scope.parent_scope_id.is_some())
            .all(|scope| scope.status == ProcessInstanceStatus::Running));
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
    #[tokio::test]
    async fn message_boundary_claim_registration_window_keeps_actual_flow_effect_count_zero() {
        use super::super::messages::{
            self,
            test_support::{
                boundary_messages, catch_target, envelope, published, send, start_version,
            },
        };
        let fixture = Fixture::new();
        let flow_id = flow(
            &fixture.db,
            &fixture.owner,
            &graph("must not execute after message", None),
        );
        let model = boundary_messages(
            service_model(
                &flow_id,
                ActivityVerification::Condition {
                    expression: "true".into(),
                },
            ),
            "Service",
            &[("Stop", true, "EvidenceReady")],
        );
        let version = published(&fixture, &model);
        let started = start_version(&fixture, &version);
        let claimed = repository::claim_job(
            &fixture.db,
            "message-registration",
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap()
        .unwrap();
        let runtime = registry_runtime(&fixture, "message-registration");
        let cancel = CancellationToken::new();
        let message = envelope(
            catch_target(&version, Some(&started.instance_id), None),
            Value::Null,
        );
        send(&fixture, &message);
        let before = tokio::sync::Barrier::new(2);
        let committed = tokio::sync::Barrier::new(2);
        let deliver = async {
            before.wait().await;
            assert!(runtime.running.is_empty());
            let drained =
                messages::drain_pending(&fixture.db, chrono::Utc::now().timestamp_millis());
            drained.completion.unwrap();
            assert_eq!(drained.delivered, 1);
            assert_eq!(drained.cancelled_claims.len(), 1);
            runtime.signal_cancelled_claims(&drained.cancelled_claims);
            committed.wait().await;
        };
        let register = async {
            before.wait().await;
            committed.wait().await;
            register_claim(&runtime, &claimed, &cancel);
            assert!(!cancel.is_cancelled());
            super::super::jobs::execute_claimed(
                &fixture.db,
                fixture.dispatcher(),
                "message-registration",
                claimed.clone(),
                cancel.clone(),
            )
            .await
            .unwrap();
            runtime.remove_running_claim(
                &claimed.job.job_id,
                claimed.job.attempt,
                claimed.job.fence,
                "message-registration",
            );
        };
        tokio::join!(deliver, register);
        assert!(runtime.running.is_empty());
        assert!(
            crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 10)
                .unwrap()
                .is_empty()
        );
        let actual =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        assert_eq!(actual.jobs[0].status, "cancelled");
        assert_eq!(actual.jobs[0].fence, claimed.job.fence + 1);
        assert!(actual.jobs[0].result.is_none());
    }

    #[tokio::test]
    async fn message_midbatch_sqlite_failure_returns_earlier_committed_claim_and_signals_only_that_generation(
    ) {
        use super::super::messages::{
            self,
            test_support::{boundary_messages, catch_target, envelope, published, start_version},
        };
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("batch effect", None));
        let model = boundary_messages(
            embedded_model(
                service_model(&flow_id, ActivityVerification::Human),
                "Scope",
            ),
            "Scope",
            &[("Stop", true, "EvidenceReady")],
        );
        let version = published(&fixture, &model);
        let first = start_version(&fixture, &version);
        let second = start_version(&fixture, &version);
        let at = chrono::Utc::now().timestamp_millis();
        let runtime = registry_runtime(&fixture, "message-batch");
        let mut claims = Vec::new();
        for _ in 0..2 {
            let claim = repository::claim_job(&fixture.db, "message-batch", at)
                .unwrap()
                .unwrap();
            let token = CancellationToken::new();
            register_claim(&runtime, &claim, &token);
            claims.push((claim, token));
        }
        let first_claim = claims
            .iter()
            .find(|(claim, _)| claim.job.instance_id == first.instance_id)
            .unwrap();
        let second_claim = claims
            .iter()
            .find(|(claim, _)| claim.job.instance_id == second.instance_id)
            .unwrap();
        let first_msg = envelope(
            catch_target(&version, Some(&first.instance_id), None),
            Value::Null,
        );
        let second_msg = envelope(
            catch_target(&version, Some(&second.instance_id), None),
            Value::Null,
        );
        repository::send_message(
            &fixture.db,
            &fixture.owner,
            &stamp("first batch message"),
            &first_msg,
            at,
        )
        .unwrap();
        repository::send_message(
            &fixture.db,
            &fixture.owner,
            &stamp("second batch message"),
            &second_msg,
            at + 1,
        )
        .unwrap();
        fixture.db.write().unwrap().execute_batch(&format!("CREATE TRIGGER fail_later_message BEFORE UPDATE ON bpmn_messages WHEN OLD.message_id='{}' AND NEW.status='delivered' BEGIN SELECT RAISE(ABORT,'controlled second message SQLite failure'); END;",second_msg.message_id)).unwrap();
        let drained = messages::drain_pending(&fixture.db, at + 2);
        assert_eq!(drained.delivered, 1);
        assert_eq!(
            drained.cancelled_claims,
            vec![CancelledJobClaim {
                job_id: first_claim.0.job.job_id.clone(),
                attempt: first_claim.0.job.attempt,
                fence: first_claim.0.job.fence,
                worker_id: "message-batch".into()
            }]
        );
        assert!(drained
            .completion
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("controlled second message SQLite failure"));
        runtime.signal_cancelled_claims(&drained.cancelled_claims);
        assert!(first_claim.1.is_cancelled());
        assert!(!second_claim.1.is_cancelled());
        let first_actual =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &first.instance_id).unwrap();
        let second_actual =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &second.instance_id).unwrap();
        assert_eq!(first_actual.jobs[0].status, "cancelled");
        assert!(first_actual
            .scopes
            .iter()
            .filter(|scope| scope.parent_scope_id.is_some())
            .all(|scope| scope.status == ProcessInstanceStatus::Cancelled));
        assert!(second_actual
            .scopes
            .iter()
            .filter(|scope| scope.parent_scope_id.is_some())
            .all(|scope| scope.status == ProcessInstanceStatus::Running));
        assert_eq!(second_actual.jobs[0].status, "running");
        assert_eq!(second_actual.jobs[0].fence, second_claim.0.job.fence);
        assert_eq!(
            second_actual.subscriptions[0].status,
            tentaflow_protocol::processes::ProcessSubscriptionStatus::Open
        );
        assert!(
            crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 10)
                .unwrap()
                .is_empty()
        );
    }
    #[tokio::test]
    async fn subprocess_parent_and_local_join_receipts_remain_isolated_after_reopen() {
        use super::super::messages::test_support::complete;
        for peer_first in [false, true] {
            let fixture = Fixture::new();
            let mut child = super::super::model::starter_model();
            child.nodes.splice(
                1..1,
                [
                    ProcessNode {
                        id: "ChildSplit".into(),
                        name: "Start local parallel work".into(),
                        kind: ProcessNodeKind::ParallelGateway,
                    },
                    ProcessNode {
                        id: "ChildA".into(),
                        name: "First child review".into(),
                        kind: ProcessNodeKind::UserTask {
                            assignee_user_id: None,
                            output_mapping: BTreeMap::new(),
                        },
                    },
                    ProcessNode {
                        id: "ChildB".into(),
                        name: "Second child review".into(),
                        kind: ProcessNodeKind::UserTask {
                            assignee_user_id: None,
                            output_mapping: BTreeMap::new(),
                        },
                    },
                    ProcessNode {
                        id: "ChildJoin".into(),
                        name: "Finish local parallel work".into(),
                        kind: ProcessNodeKind::ParallelGateway,
                    },
                ],
            );
            child.sequence_flows = vec![
                edge("ChildEnter", "Start_1", "ChildSplit"),
                edge("ChildLeft", "ChildSplit", "ChildA"),
                edge("ChildRight", "ChildSplit", "ChildB"),
                edge("ChildAJoin", "ChildA", "ChildJoin"),
                edge("ChildBJoin", "ChildB", "ChildJoin"),
                edge("ChildLeave", "ChildJoin", "End_1"),
            ];
            let mut model = embedded_model(child, "Scope");
            model.nodes.extend([
                ProcessNode {
                    id: "ParentSplit".into(),
                    name: "Start parent parallel work".into(),
                    kind: ProcessNodeKind::ParallelGateway,
                },
                ProcessNode {
                    id: "Peer".into(),
                    name: "Independent parent work".into(),
                    kind: ProcessNodeKind::UserTask {
                        assignee_user_id: None,
                        output_mapping: BTreeMap::new(),
                    },
                },
                ProcessNode {
                    id: "ParentJoin".into(),
                    name: "Finish parent parallel work".into(),
                    kind: ProcessNodeKind::ParallelGateway,
                },
            ]);
            model.sequence_flows = vec![
                edge("ParentEnter", "RootStart_Scope", "ParentSplit"),
                edge("ParentChild", "ParentSplit", "Scope"),
                edge("ParentPeer", "ParentSplit", "Peer"),
                edge("ScopeJoin", "Scope", "ParentJoin"),
                edge("PeerJoin", "Peer", "ParentJoin"),
                edge("ParentLeave", "ParentJoin", "RootEnd_Scope"),
            ];
            let started = start_model(&fixture, &model);
            let child_id = started
                .scopes
                .iter()
                .find(|scope| scope.subprocess_node_id.as_deref() == Some("Scope"))
                .unwrap()
                .scope_id
                .clone();
            let task_id = |node: &str| {
                started
                    .user_tasks
                    .iter()
                    .find(|task| task.node_id == node)
                    .unwrap()
                    .user_task_id
                    .clone()
            };
            let first = if peer_first { "Peer" } else { "ChildA" };
            let second = if peer_first { "ChildA" } else { "Peer" };
            complete(&fixture, &started.instance_id, &task_id(first));
            complete(&fixture, &started.instance_id, &task_id(second));
            let partial =
                repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                    .unwrap();
            assert_eq!(partial.receipts.len(), 2);
            assert!(
                partial
                    .receipts
                    .iter()
                    .any(|receipt| receipt.scope_id == child_id
                        && receipt.join_node_id == "ChildJoin")
            );
            assert!(partial
                .receipts
                .iter()
                .any(|receipt| receipt.scope_id == started.instance_id
                    && receipt.join_node_id == "ParentJoin"));
            let parent_wait = partial
                .tokens
                .iter()
                .find(|token| token.node_id == "Scope")
                .unwrap();
            assert_eq!(parent_wait.fork_stack[0].split_node_id, "ParentSplit");
            let child_wait = partial
                .tokens
                .iter()
                .find(|token| token.node_id == "ChildB")
                .unwrap();
            assert_eq!(child_wait.fork_stack[0].split_node_id, "ChildSplit");
            assert_ne!(
                parent_wait.fork_stack[0].activation_id,
                child_wait.fork_stack[0].activation_id
            );
            let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
            let snapshot =
                repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id)
                    .unwrap();
            let at = chrono::Utc::now().timestamp_millis();
            let command = stamp("finish scoped and joins");
            let plan =
                plan_user_completion(&snapshot, &task_id("ChildB"), &json!({}), None, at).unwrap();
            let outcome = repository::complete_user_task(
                &reopened,
                &fixture.owner,
                &command,
                &started.instance_id,
                &task_id("ChildB"),
                snapshot.instance.revision,
                &json!({}),
                None,
                &plan,
                at,
            )
            .unwrap();
            assert!(outcome.cancelled_claims.is_empty());
            assert_eq!(outcome.instance.status, ProcessInstanceStatus::Completed);
            let finished =
                repository::runtime_snapshot(&reopened, &fixture.owner, &started.instance_id)
                    .unwrap();
            assert!(finished.tokens.is_empty() && finished.receipts.is_empty());
            assert!(finished
                .scopes
                .iter()
                .all(|scope| scope.status == ProcessInstanceStatus::Completed));
            let events =
                repository::list_events(&reopened, &fixture.owner, &started.instance_id, 0, 200)
                    .unwrap()
                    .0;
            let joined = events
                .iter()
                .filter(|event| event.kind == "parallel_joined")
                .collect::<Vec<_>>();
            assert_eq!(joined.len(), 2);
            assert!(joined.iter().any(|event| event.scope_id == child_id));
            assert!(joined
                .iter()
                .any(|event| event.scope_id == started.instance_id));
            let replay = repository::complete_user_task(
                &reopened,
                &fixture.owner,
                &command,
                &started.instance_id,
                &task_id("ChildB"),
                snapshot.instance.revision,
                &json!({}),
                None,
                &plan,
                at,
            )
            .unwrap();
            assert!(replay.cancelled_claims.is_empty());
            assert_eq!(replay.instance.revision, outcome.instance.revision);
            assert_eq!(
                repository::list_events(&reopened, &fixture.owner, &started.instance_id, 0, 200)
                    .unwrap()
                    .0,
                events
            );
        }
    }

    #[tokio::test]
    async fn subprocess_boundaries_compete_on_exact_child_and_preserve_earlier_parent_side_work() {
        use super::super::messages::{
            self,
            test_support::{
                boundary_messages, catch_target, complete, envelope, published, send, start_version,
            },
        };
        use tentaflow_protocol::processes::{ProcessMessageStatus, ProcessTimerStatus};
        for winner in ["message", "timer", "completion"] {
            let fixture = Fixture::new();
            let mut child = user_model(None);
            if let ProcessNodeKind::UserTask { output_mapping, .. } = &mut child.nodes[1].kind {
                output_mapping.clear();
            }
            child
                .variables
                .insert("retained".into(), json!("actual child local"));
            let model = boundary_messages(
                with_boundaries(
                    embedded_model(child, "Scope"),
                    "Scope",
                    &[("Limit", true, 5)],
                ),
                "Scope",
                &[
                    ("Note", false, "NoteReady"),
                    ("Stop", true, "EvidenceReady"),
                ],
            );
            let version = published(&fixture, &model);
            let started = start_version(&fixture, &version);
            let child_id = started
                .scopes
                .iter()
                .find(|scope| scope.subprocess_node_id.as_deref() == Some("Scope"))
                .unwrap()
                .scope_id
                .clone();
            let snap =
                repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                    .unwrap();
            let note = snap
                .subscriptions
                .iter()
                .find(|sub| sub.node_id == "Note")
                .unwrap();
            let stop = snap
                .subscriptions
                .iter()
                .find(|sub| sub.node_id == "Stop")
                .unwrap()
                .clone();
            let due = snap
                .timers
                .iter()
                .find(|timer| timer.node_id == "Limit")
                .unwrap()
                .due_at_ms
                .unwrap();
            let mut message = envelope(
                catch_target(
                    &version,
                    Some(&started.instance_id),
                    Some(&note.subscription_id),
                ),
                Value::Null,
            );
            message.message_name = "NoteReady".into();
            let receipt = send(&fixture, &message);
            let drained = messages::drain_pending(&fixture.db, receipt.received_at_ms);
            assert!(drained.completion.is_ok());
            assert_eq!(drained.delivered, 1);
            let before =
                repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                    .unwrap();
            let side = before
                .user_tasks
                .iter()
                .find(|task| task.node_id == "Side_Note")
                .unwrap()
                .user_task_id
                .clone();
            let work = before
                .user_tasks
                .iter()
                .find(|task| task.node_id == "Work")
                .unwrap()
                .clone();
            if winner == "message" {
                let stop_message = envelope(
                    catch_target(
                        &version,
                        Some(&started.instance_id),
                        Some(&stop.subscription_id),
                    ),
                    json!({"stop":true}),
                );
                let receipt = send(&fixture, &stop_message);
                let drained = messages::drain_pending(&fixture.db, receipt.received_at_ms);
                assert!(drained.completion.is_ok());
                assert_eq!(drained.delivered, 1);
                assert_eq!(
                    repository::get_message(
                        &fixture.db,
                        &fixture.owner,
                        &fixture.owner.user_id,
                        &stop_message.message_id
                    )
                    .unwrap()
                    .message
                    .status,
                    ProcessMessageStatus::Delivered
                );
            } else if winner == "timer" {
                let drained = super::super::timers::drain_due(&fixture.db, due);
                assert!(drained.completion.is_ok());
                assert_eq!(drained.fired, 1);
            } else {
                complete(&fixture, &started.instance_id, &work.user_task_id);
            }
            let actual =
                repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                    .unwrap();
            let expected = if winner == "completion" {
                ProcessInstanceStatus::Completed
            } else {
                ProcessInstanceStatus::Cancelled
            };
            assert_eq!(
                actual
                    .scopes
                    .iter()
                    .find(|scope| scope.scope_id == child_id)
                    .unwrap()
                    .status,
                expected
            );
            assert_eq!(
                actual
                    .user_tasks
                    .iter()
                    .find(|task| task.user_task_id == side)
                    .unwrap()
                    .status,
                ProcessUserTaskStatus::Open
            );
            assert_eq!(
                actual
                    .user_tasks
                    .iter()
                    .find(|task| task.user_task_id == work.user_task_id)
                    .unwrap()
                    .status,
                if winner == "completion" {
                    ProcessUserTaskStatus::Completed
                } else {
                    ProcessUserTaskStatus::Cancelled
                }
            );
            assert_eq!(
                repository::get_scope(&fixture.db, &fixture.owner, &started.instance_id, &child_id)
                    .unwrap()
                    .1["retained"],
                "actual child local"
            );
            assert_eq!(
                actual
                    .timers
                    .iter()
                    .find(|timer| timer.node_id == "Limit")
                    .unwrap()
                    .status,
                if winner == "timer" {
                    ProcessTimerStatus::Fired
                } else {
                    ProcessTimerStatus::Cancelled
                }
            );
            let later = super::super::timers::drain_due(&fixture.db, due);
            assert!(later.completion.is_ok());
            assert_eq!(later.fired, 0);
            if winner != "message" {
                let late = envelope(
                    catch_target(
                        &version,
                        Some(&started.instance_id),
                        Some(&stop.subscription_id),
                    ),
                    Value::Null,
                );
                let receipt = send(&fixture, &late);
                let drained = messages::drain_pending(&fixture.db, receipt.received_at_ms);
                assert!(drained.completion.is_ok());
                assert_eq!(drained.delivered, 0);
                let terminal = repository::get_message(
                    &fixture.db,
                    &fixture.owner,
                    &fixture.owner.user_id,
                    &late.message_id,
                )
                .unwrap();
                assert_eq!(terminal.message.status, ProcessMessageStatus::Cancelled);
                assert_eq!(
                    terminal.message.last_reason.as_deref(),
                    Some("activation_closed")
                );
            }
            let stale =
                plan_user_completion(&before, &work.user_task_id, &json!({}), None, due).unwrap();
            assert!(repository::complete_user_task(
                &fixture.db,
                &fixture.owner,
                &stamp("stale child completion"),
                &started.instance_id,
                &work.user_task_id,
                before.instance.revision,
                &json!({}),
                None,
                &stale,
                due
            )
            .is_err());
        }
    }

    #[tokio::test]
    async fn subprocess_nearest_contract_error_preserves_result_and_cancels_only_committed_child_generation(
    ) {
        use super::super::repository::{ActivityResultOrigin, ObservedActivityResult};
        use tentaflow_protocol::processes::{ProcessErrorDeclaration, ProcessSubscriptionStatus};
        let fixture = Fixture::new();
        let business = json!({"outcome":"Error","code":"REJECTED","summary":"A real pinned business rejection","outputs":{"customer_ID":42},"evidence":["actual scoped execution"]});
        let business_graph=json!({"nodes":[
            {"id":"trigger","type":"trigger","config":{"output_mapping":{"actual_result":serde_json::to_string(&business).unwrap()}}},
            {"id":"output","type":"output","config":{}}
        ],"edges":[{"from":"trigger","to":"output","from_port":"text","to_port":"text"}],
            "variables":[{"name":"actual_result","type":"json"}]}).to_string();
        let source_flow = flow(&fixture.db, &fixture.owner, &business_graph);
        let other_flow = flow(
            &fixture.db,
            &fixture.owner,
            &graph("must not execute child sibling", None),
        );
        let mut child = service_model(
            &source_flow,
            ActivityVerification::Condition {
                expression: "true".into(),
            },
        );
        if let ProcessNodeKind::ServiceTask {
            result_expression,
            output_mapping,
            ..
        } = &mut child.nodes[1].kind
        {
            *result_expression = Some("outputs.variables.actual_result".into());
            output_mapping.clear();
        }
        let mut other = service_model(
            &other_flow,
            ActivityVerification::Condition {
                expression: "true".into(),
            },
        )
        .nodes
        .remove(1);
        other.id = "OtherService".into();
        other.name = "Still running inside the interrupted child".into();
        child.nodes.extend([
            ProcessNode {
                id: "LocalSplit".into(),
                name: "Start child branches".into(),
                kind: ProcessNodeKind::ParallelGateway,
            },
            other,
            ProcessNode {
                id: "LocalJoin".into(),
                name: "Finish child branches".into(),
                kind: ProcessNodeKind::ParallelGateway,
            },
        ]);
        child.sequence_flows = vec![
            edge("LocalEnter", "Start_1", "LocalSplit"),
            edge("LocalSource", "LocalSplit", "Service"),
            edge("LocalOther", "LocalSplit", "OtherService"),
            edge("SourceJoin", "Service", "LocalJoin"),
            edge("OtherJoin", "OtherService", "LocalJoin"),
            edge("LocalLeave", "LocalJoin", "End_1"),
        ];
        let mut inner = embedded_model(child, "Inner");
        inner.errors.push(ProcessErrorDeclaration {
            error_id: "BusinessError".into(),
            name: "A real business error".into(),
            error_code: "REJECTED".into(),
        });
        inner.nodes.extend([
            ProcessNode {
                id: "Near".into(),
                name: "Nearest enclosing handler".into(),
                kind: ProcessNodeKind::BoundaryError {
                    attached_to_id: "Inner".into(),
                    error_ref: Some("BusinessError".into()),
                    output_mapping: BTreeMap::from([
                        ("original_result".into(), "activity_result".into()),
                        ("customer".into(), "outputs.customer_ID".into()),
                    ]),
                },
            },
            ProcessNode {
                id: "NearWork".into(),
                name: "Review the child error".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: BTreeMap::new(),
                },
            },
        ]);
        inner.sequence_flows.extend([
            edge("NearPath", "Near", "NearWork"),
            edge("NearEnd", "NearWork", "RootEnd_Inner"),
        ]);
        let mut model = embedded_model(inner, "Outer");
        model.nodes.extend([
            ProcessNode {
                id: "Far".into(),
                name: "Outer catch all".into(),
                kind: ProcessNodeKind::BoundaryError {
                    attached_to_id: "Outer".into(),
                    error_ref: None,
                    output_mapping: BTreeMap::new(),
                },
            },
            ProcessNode {
                id: "FarWork".into(),
                name: "Review outer error".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: BTreeMap::new(),
                },
            },
        ]);
        model.sequence_flows.extend([
            edge("FarPath", "Far", "FarWork"),
            edge("FarEnd", "FarWork", "RootEnd_Outer"),
        ]);
        let started = start_model(&fixture, &model);
        let runtime = Arc::new(registry_runtime(&fixture, "scoped-error-worker"));
        *fixture.dispatcher().process_runtime.lock() = Some(runtime.clone());
        let mut claims = Vec::new();
        for _ in 0..2 {
            claims.push(
                repository::claim_job(
                    &fixture.db,
                    "scoped-error-worker",
                    chrono::Utc::now().timestamp_millis(),
                )
                .unwrap()
                .unwrap(),
            );
        }
        let source = claims
            .iter()
            .find(|claim| claim.job.node_id == "Service")
            .unwrap()
            .clone();
        let sibling = claims
            .iter()
            .find(|claim| claim.job.node_id == "OtherService")
            .unwrap()
            .clone();
        let source_cancel = CancellationToken::new();
        let sibling_cancel = CancellationToken::new();
        register_claim(&runtime, &source, &source_cancel);
        register_claim(&runtime, &sibling, &sibling_cancel);
        let snapshot =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        let observed = ObservedActivityResult {
            result: serde_json::from_value(business.clone()).unwrap(),
            origin: ActivityResultOrigin::Contract,
            expression_observation: None,
        };
        let mut forged = plan_job_result(
            &snapshot,
            &source.job,
            &observed,
            chrono::Utc::now().timestamp_millis(),
        )
        .unwrap();
        forged.cancel_job_ids.clear();
        let events_before =
            repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                .unwrap()
                .0;
        assert!(repository::accept_job_result(
            &fixture.db,
            &fixture.owner,
            &source.job.job_id,
            source.job.attempt,
            source.job.fence,
            "scoped-error-worker",
            &observed,
            snapshot.instance.revision,
            &forged,
            chrono::Utc::now().timestamp_millis()
        )
        .is_err());
        assert_eq!(
            repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                .unwrap()
                .0,
            events_before
        );
        assert!(!sibling_cancel.is_cancelled());
        super::super::jobs::execute_claimed(
            &fixture.db,
            fixture.dispatcher(),
            "scoped-error-worker",
            source.clone(),
            source_cancel.clone(),
        )
        .await
        .unwrap();
        assert!(sibling_cancel.is_cancelled());
        assert!(!source_cancel.is_cancelled());
        let actual =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        let inner_scope = actual
            .scopes
            .iter()
            .find(|scope| scope.subprocess_node_id.as_deref() == Some("Inner"))
            .unwrap();
        let outer_scope = actual
            .scopes
            .iter()
            .find(|scope| scope.subprocess_node_id.as_deref() == Some("Outer"))
            .unwrap();
        assert_eq!(inner_scope.status, ProcessInstanceStatus::Cancelled);
        assert_eq!(outer_scope.status, ProcessInstanceStatus::Waiting);
        assert!(actual
            .user_tasks
            .iter()
            .any(|task| task.node_id == "NearWork" && task.scope_id == outer_scope.scope_id));
        assert!(!actual
            .user_tasks
            .iter()
            .any(|task| task.node_id == "FarWork"));
        assert_eq!(
            actual
                .subscriptions
                .iter()
                .find(|sub| sub.node_id == "Near")
                .unwrap()
                .status,
            ProcessSubscriptionStatus::Consumed
        );
        assert_eq!(
            actual
                .subscriptions
                .iter()
                .find(|sub| sub.node_id == "Far")
                .unwrap()
                .status,
            ProcessSubscriptionStatus::Open
        );
        let retained = actual
            .jobs
            .iter()
            .find(|job| job.job_id == source.job.job_id)
            .unwrap();
        assert_eq!(retained.result.as_ref().unwrap(), &observed.result);
        assert_eq!(retained.result_origin, Some(ActivityResultOrigin::Contract));
        assert_eq!(retained.status, "error");
        let cancelled = actual
            .jobs
            .iter()
            .find(|job| job.job_id == sibling.job.job_id)
            .unwrap();
        assert_eq!(cancelled.status, "cancelled");
        assert_eq!(cancelled.fence, sibling.job.fence + 1);
        assert_eq!(
            repository::get_scope(
                &fixture.db,
                &fixture.owner,
                &started.instance_id,
                &outer_scope.scope_id
            )
            .unwrap()
            .1["original_result"],
            business
        );
        let events =
            repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                .unwrap()
                .0;
        let result_event = events
            .iter()
            .find(|event| event.kind == "service_result")
            .unwrap();
        assert_eq!(result_event.scope_id, inner_scope.scope_id);
        let propagation = events
            .iter()
            .filter(|event| event.kind == "scope_error_propagated")
            .collect::<Vec<_>>();
        assert_eq!(propagation.len(), 1);
        assert_eq!(
            propagation[0].data["result_event_id"],
            result_event.event_id
        );
        assert_eq!(propagation[0].data["from_scope_id"], inner_scope.scope_id);
        assert_eq!(propagation[0].data["to_scope_id"], outer_scope.scope_id);
        assert!(propagation[0].data.get("source_result_index").is_none());
        assert!(!repository::renew_job_lease(
            &fixture.db,
            &sibling.job.job_id,
            sibling.job.attempt,
            sibling.job.fence,
            "scoped-error-worker",
            chrono::Utc::now().timestamp_millis()
        )
        .unwrap());
        super::super::jobs::execute_claimed(
            &fixture.db,
            fixture.dispatcher(),
            "scoped-error-worker",
            sibling.clone(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert!(repository::retry_job(
            &fixture.db,
            &fixture.owner,
            &stamp("closed child cannot retry"),
            &started.instance_id,
            &source.job.job_id,
            actual.instance.revision
        )
        .is_err());
        assert!(
            crate::db::repository::list_flow_executions_for_flow(&fixture.db, &other_flow, 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            crate::db::repository::list_flow_executions_for_flow(&fixture.db, &source_flow, 10)
                .unwrap()
                .len(),
            1
        );
        assert!(!actual.instance.can_retry);
        *fixture.dispatcher().process_runtime.lock() = None;
    }
    #[tokio::test]
    async fn called_child_midbatch_storage_failure_preserves_committed_registry_cancellation() {
        let fixture = Fixture::new();
        let flow_id = flow(
            &fixture.db,
            &fixture.owner,
            &graph("call batch must not execute", None),
        );
        let target = publish_model(
            &fixture,
            &service_model(
                &flow_id,
                ActivityVerification::Condition {
                    expression: "true".into(),
                },
            ),
        );
        let mut parents = Vec::new();
        let mut timers = Vec::new();
        let mut children = Vec::new();
        for seconds in [1, 2] {
            let model = with_boundaries(
                super::super::call_tests::caller(&target, BTreeMap::new()),
                "Call_1",
                &[("Limit", true, seconds)],
            );
            let parent = start_model(&fixture, &model);
            let snapshot =
                repository::runtime_snapshot(&fixture.db, &fixture.owner, &parent.instance_id)
                    .unwrap();
            children.push(snapshot.calls[0].child_instance_id.clone());
            timers.push(snapshot.timers[0].clone());
            parents.push(parent);
        }
        let runtime = registry_runtime(&fixture, "called-batch");
        let mut running = Vec::new();
        for _ in 0..2 {
            let claim = repository::claim_job(
                &fixture.db,
                "called-batch",
                chrono::Utc::now().timestamp_millis(),
            )
            .unwrap()
            .unwrap();
            let cancellation = CancellationToken::new();
            register_claim(&runtime, &claim, &cancellation);
            running.push((claim, cancellation));
        }
        let first = running
            .iter()
            .find(|(claim, _)| claim.job.instance_id == children[0])
            .unwrap();
        let second = running
            .iter()
            .find(|(claim, _)| claim.job.instance_id == children[1])
            .unwrap();
        fixture.db.write().unwrap().execute_batch(&format!(
            "CREATE TRIGGER fail_call_batch BEFORE UPDATE ON bpmn_timers WHEN OLD.timer_id='{}' AND NEW.status IN ('fired','error') BEGIN SELECT RAISE(ABORT,'controlled called midbatch storage failure'); END;",
            timers[1].timer_id)).unwrap();
        let drained = super::super::timers::drain_due(&fixture.db, timers[1].due_at_ms.unwrap());
        assert_eq!(drained.fired, 1);
        assert!(format!("{:#}", drained.completion.as_ref().unwrap_err())
            .contains("controlled called midbatch storage failure"));
        assert_eq!(
            drained.cancelled_claims,
            vec![CancelledJobClaim {
                job_id: first.0.job.job_id.clone(),
                attempt: first.0.job.attempt,
                fence: first.0.job.fence,
                worker_id: "called-batch".into(),
            }]
        );
        runtime.handle_timer_drain(drained);
        assert!(first.1.is_cancelled());
        assert!(!second.1.is_cancelled());
        let first_child =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &children[0]).unwrap();
        let second_child =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &children[1]).unwrap();
        assert_eq!(
            first_child.instance.status,
            ProcessInstanceStatus::Cancelled
        );
        assert_eq!(first_child.jobs[0].status, "cancelled");
        assert_eq!(first_child.jobs[0].fence, first.0.job.fence + 1);
        assert_eq!(second_child.instance.status, ProcessInstanceStatus::Running);
        assert_eq!(second_child.jobs[0].status, "running");
        assert_eq!(second_child.jobs[0].fence, second.0.job.fence);
        assert_eq!(
            repository::get_instance(&fixture.db, &fixture.owner, &parents[0].instance_id, None)
                .unwrap()
                .status,
            ProcessInstanceStatus::Completed
        );
        assert_eq!(
            repository::get_instance(&fixture.db, &fixture.owner, &parents[1].instance_id, None)
                .unwrap()
                .status,
            ProcessInstanceStatus::Waiting
        );
        for (claim, cancellation) in &running {
            if cancellation.is_cancelled() {
                runtime.remove_running_claim(
                    &claim.job.job_id,
                    claim.job.attempt,
                    claim.job.fence,
                    "called-batch",
                );
            }
        }
        assert_eq!(runtime.running.len(), 1);
        assert!(
            crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 10)
                .unwrap()
                .is_empty()
        );
    }
}
