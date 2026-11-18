// ============ File: simulation_scope_tests.rs — embedded sub-processes in a private simulation ============

use std::collections::BTreeMap;

use serde_json::{json, Value};
use tentaflow_protocol::processes::{
    ProcessDiagram, ProcessInstanceStatus, ProcessModel, ProcessNode, ProcessNodeKind,
    ProcessSubProcess, ProcessUserTaskKind, ProcessUserTaskStatus,
};

use super::model::starter_model;
use super::runtime::test_support::edge;
use super::simulation::{
    SimulationServiceError, SimulationStore, SimulationView, MAX_SIMULATION_SCOPES,
    SIMULATION_SCOPE_LIMIT,
};
use super::simulation_service_tests::normalize_events;
use super::simulation_tests::{create_store, source_input_for, test_authorization};

pub(crate) fn node(id: &str, kind: ProcessNodeKind) -> ProcessNode {
    ProcessNode {
        id: id.into(),
        name: format!("Step {id}"),
        kind,
        repeat: None,
        activity_io: None,
    }
}

pub(crate) fn user_task(assignee: Option<&str>) -> ProcessNodeKind {
    ProcessNodeKind::UserTask {
        assignee_user_id: assignee.map(str::to_owned),
        output_mapping: BTreeMap::from([("answer".into(), "outputs.answer".into())]),
    }
}

pub(crate) fn script(source: &str, output: &str) -> ProcessNodeKind {
    ProcessNodeKind::ScriptTask {
        script: source.into(),
        output_mapping: BTreeMap::from([(output.into(), "outputs".into())]),
    }
}

pub(crate) fn body(
    nodes: Vec<ProcessNode>,
    flows: Vec<tentaflow_protocol::processes::ProcessSequenceFlow>,
) -> ProcessSubProcess {
    ProcessSubProcess {
        nodes,
        sequence_flows: flows,
        variables: BTreeMap::new(),
        diagram: ProcessDiagram::default(),
        modeling: None,
    }
}

pub(crate) fn sub_process(
    inner: ProcessSubProcess,
    input: &[(&str, &str)],
    output: &[(&str, &str)],
) -> ProcessNodeKind {
    let map = |pairs: &[(&str, &str)]| {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    };
    ProcessNodeKind::SubProcess {
        body: inner,
        input_mapping: map(input),
        output_mapping: map(output),
    }
}

/// Start, a script, a sub-process whose body asks a person, and End: the smallest
/// model in which a scope is opened, works and hands a value back.
pub(crate) fn scope_model(assignee: Option<&str>) -> ProcessModel {
    let mut model = starter_model();
    model.variables = BTreeMap::from([("value".into(), json!(1))]);
    let inner = body(
        vec![
            node("InnerStart", ProcessNodeKind::Start),
            node("InnerWork", user_task(assignee)),
            node("InnerEnd", ProcessNodeKind::End),
        ],
        vec![
            edge("InnerFlow1", "InnerStart", "InnerWork"),
            edge("InnerFlow2", "InnerWork", "InnerEnd"),
        ],
    );
    model.nodes = vec![
        model.nodes[0].clone(),
        node("Script_1", script("vars.value + 1", "base")),
        node(
            "Scope_1",
            sub_process(
                inner,
                &[("seed", "vars.base")],
                &[("result", "outputs.answer")],
            ),
        ),
        model.nodes.last().expect("starter End").clone(),
    ];
    model.sequence_flows = vec![
        edge("ToScript", "Start_1", "Script_1"),
        edge("ToScope", "Script_1", "Scope_1"),
        edge("ScopeEnd", "Scope_1", "End_1"),
    ];
    model
}

fn authorize(store: &SimulationStore) -> super::simulation::AuthenticatedSimulationAction {
    test_authorization(store)
}

pub(crate) fn started(model: ProcessModel) -> SimulationStore {
    // The scenario supplies the whole initial scope, so a run starts from the
    // model's own defaults only when the operator hands them over.
    let variables = serde_json::to_value(&model.variables).expect("model variables");
    let mut store = create_store(source_input_for(model)).expect("capture the source");
    store
        .start(authorize(&store), variables)
        .expect("start the run");
    store
}

/// Advances until the predicate holds, or panics when the run never gets there.
pub(crate) fn advance_until(
    store: &mut SimulationStore,
    what: &str,
    done: impl Fn(&SimulationView) -> bool,
) -> SimulationView {
    let mut view = store.view(authorize(store)).expect("read the run");
    for _ in 0..60 {
        if done(&view) {
            return view;
        }
        view = store.advance(authorize(store)).expect("advance the run");
    }
    panic!("the run never reached: {what}");
}

pub(crate) fn open_work(
    view: &SimulationView,
) -> Vec<&tentaflow_protocol::processes::ProcessUserTask> {
    view.user_tasks
        .iter()
        .filter(|task| {
            task.status == ProcessUserTaskStatus::Open && task.kind == ProcessUserTaskKind::Work
        })
        .collect()
}

pub(crate) fn complete_open_work(store: &mut SimulationStore, outputs: Value) -> SimulationView {
    let view = store.view(authorize(store)).expect("read the run");
    let task_id = open_work(&view)
        .first()
        .expect("an open task")
        .user_task_id
        .clone();
    store
        .complete_user_task(authorize(store), &task_id, outputs, None)
        .expect("complete the task")
}

#[test]
fn a_sub_process_opens_a_scope_runs_its_body_and_hands_the_result_back() {
    let mut store = started(scope_model(Some("user-simulation")));
    let view = advance_until(&mut store, "the task inside the scope", |view| {
        !open_work(view).is_empty()
    });

    let child = view
        .scopes
        .iter()
        .find(|scope| scope.parent_scope_id.is_some())
        .expect("the sub-process opened a scope");
    assert_eq!(child.subprocess_node_id.as_deref(), Some("Scope_1"));
    assert_eq!(child.subprocess_node_name.as_deref(), Some("Step Scope_1"));
    assert_eq!(child.status, ProcessInstanceStatus::Waiting);
    assert_eq!(child.depth, 1);
    let root = view
        .scopes
        .iter()
        .find(|scope| scope.parent_scope_id.is_none())
        .expect("root scope");
    assert_eq!(
        child.parent_scope_id.as_deref(),
        Some(root.scope_id.as_str())
    );
    let task = open_work(&view)[0];
    assert_eq!(
        task.scope_id, child.scope_id,
        "the task belongs to the scope, not to the root"
    );
    assert!(view
        .events
        .iter()
        .any(|event| event.kind == "scope_entered"));

    let done = complete_open_work(&mut store, json!({"answer": "yes"}));
    let instance = done.instance.as_ref().expect("instance");
    assert_eq!(instance.status, ProcessInstanceStatus::Completed);
    assert_eq!(
        instance.variables["result"],
        json!("yes"),
        "copy-out through the output mapping"
    );
    assert!(done
        .scopes
        .iter()
        .filter(|scope| scope.parent_scope_id.is_some())
        .all(|scope| scope.status == ProcessInstanceStatus::Completed));
    assert!(done
        .events
        .iter()
        .any(|event| event.kind == "scope_completed"));
}

#[test]
fn a_scope_run_repeats_with_the_same_ids_and_trace() {
    let run = || {
        let mut store = started(scope_model(Some("user-simulation")));
        advance_until(&mut store, "the task inside the scope", |view| {
            !open_work(view).is_empty()
        });
        let view = complete_open_work(&mut store, json!({"answer": "yes"}));
        (
            view.scopes
                .iter()
                .map(|scope| scope.scope_id.clone())
                .collect::<Vec<_>>(),
            view.events
                .iter()
                .map(|event| {
                    (
                        event.event_id.clone(),
                        event.kind.clone(),
                        event.data.clone(),
                    )
                })
                .collect::<Vec<_>>(),
            view.trace_steps
                .iter()
                .map(|step| (step.trace_step_id.clone(), step.result_sha256.clone()))
                .collect::<Vec<_>>(),
        )
    };
    let (first, second) = (run(), run());
    assert_eq!(first.0.len(), 2);
    assert_eq!(first, second);
}

#[tokio::test]
async fn the_simulated_scope_trace_equals_the_production_execution() {
    use super::repository;
    use super::runtime::test_support::{stamp, start_model, Fixture};

    let mut model = scope_model(None);
    let production = Fixture::new();
    let instance = start_model(&production, &model);
    let waiting =
        repository::runtime_snapshot(&production.db, &production.owner, &instance.instance_id)
            .expect("production snapshot");
    let task = waiting
        .user_tasks
        .iter()
        .find(|task| task.kind == ProcessUserTaskKind::Work)
        .expect("production opens the task inside the scope");
    repository::complete_user_task(
        &production.db,
        &production.owner,
        &stamp("complete"),
        &instance.instance_id,
        &task.user_task_id,
        waiting.instance.revision,
        &json!({"answer": "yes"}),
        None,
        repository::ProcessPlanInput::Canonical,
        chrono::Utc::now().timestamp_millis(),
    )
    .expect("complete the production task");
    let production_events = normalize_events(
        repository::list_events(
            &production.db,
            &production.owner,
            &instance.instance_id,
            0,
            200,
        )
        .expect("production history")
        .0
        .into_iter()
        .map(|event| (event.kind, event.node_id, event.data))
        .collect(),
    );
    let production_variables = repository::get_instance(
        &production.db,
        &production.owner,
        &instance.instance_id,
        None,
    )
    .expect("production instance")
    .variables;

    for node in &mut model.nodes {
        if let ProcessNodeKind::SubProcess { body, .. } = &mut node.kind {
            for inner in &mut body.nodes {
                if let ProcessNodeKind::UserTask {
                    assignee_user_id, ..
                } = &mut inner.kind
                {
                    *assignee_user_id = Some("user-simulation".into());
                }
            }
        }
    }
    let mut store = started(model);
    advance_until(&mut store, "the task inside the scope", |view| {
        !open_work(view).is_empty()
    });
    let done = complete_open_work(&mut store, json!({"answer": "yes"}));
    let simulated_events = normalize_events(
        done.events
            .iter()
            .map(|event| {
                (
                    event.kind.clone(),
                    event.node_id.clone(),
                    event.data.clone(),
                )
            })
            .collect(),
    );

    assert_eq!(simulated_events, production_events);
    assert_eq!(
        done.instance.as_ref().expect("instance").variables,
        production_variables
    );
}

/// A plan that opens `count` scopes, as the writer would be handed it.
fn plan_opening(count: usize) -> super::repository::RuntimePlan {
    let mut plan = super::repository::RuntimePlan::initial(json!({}));
    plan.create_scopes = (0..count)
        .map(|index| super::repository::PlannedScope {
            scope_id: uuid::Uuid::from_u128(index as u128 + 1).to_string(),
            parent_scope_id: uuid::Uuid::nil().to_string(),
            parent_token_id: uuid::Uuid::nil().to_string(),
            subprocess_node_id: "Scope_1".into(),
            variables: json!({}),
        })
        .collect();
    plan
}

pub(crate) fn refusal_code(error: &anyhow::Error) -> &'static str {
    error
        .downcast_ref::<SimulationServiceError>()
        .unwrap_or_else(|| panic!("a stable refusal, got {error:#}"))
        .code
}

#[test]
fn a_transition_cannot_open_more_scopes_than_the_simulation_limit() {
    use super::simulation::{check_simulation_capacity, SimulationUsage};

    check_simulation_capacity(
        &plan_opening(MAX_SIMULATION_SCOPES),
        &SimulationUsage::default(),
    )
    .expect("a run may open exactly the limit");
    // Scopes the run opened earlier count, closed ones included.
    let usage = SimulationUsage {
        scopes: MAX_SIMULATION_SCOPES - 1,
        ..SimulationUsage::default()
    };
    check_simulation_capacity(&plan_opening(1), &usage).expect("the last scope fits");
    for (opened, used) in [
        (MAX_SIMULATION_SCOPES + 1, 0),
        (2, MAX_SIMULATION_SCOPES - 1),
    ] {
        let usage = SimulationUsage {
            scopes: used,
            ..SimulationUsage::default()
        };
        let error =
            check_simulation_capacity(&plan_opening(opened), &usage).expect_err("past the limit");
        assert_eq!(refusal_code(&error), SIMULATION_SCOPE_LIMIT);
    }
}

/// Start, a sub-process around one person's task, and a handler for whatever the
/// boundary event on the sub-process catches.
fn guarded_scope_model(boundary: ProcessNodeKind) -> ProcessModel {
    let mut model = starter_model();
    let inner = body(
        vec![
            node("InnerStart", ProcessNodeKind::Start),
            node("InnerWork", user_task(Some("user-simulation"))),
            node("InnerEnd", ProcessNodeKind::End),
        ],
        vec![
            edge("InnerFlow1", "InnerStart", "InnerWork"),
            edge("InnerFlow2", "InnerWork", "InnerEnd"),
        ],
    );
    model.nodes = vec![
        model.nodes[0].clone(),
        node(
            "Scope_1",
            sub_process(inner, &[], &[("result", "outputs.answer")]),
        ),
        node("Guard_1", boundary),
        node("Handler_1", user_task(Some("user-simulation"))),
        node("End_H", ProcessNodeKind::End),
        model.nodes.last().expect("starter End").clone(),
    ];
    model.sequence_flows = vec![
        edge("ToScope", "Start_1", "Scope_1"),
        edge("ScopeEnd", "Scope_1", "End_1"),
        edge("ToHandler", "Guard_1", "Handler_1"),
        edge("HandlerEnd", "Handler_1", "End_H"),
    ];
    model
}

fn child_scopes(view: &SimulationView) -> Vec<&tentaflow_protocol::processes::ProcessScopeSummary> {
    view.scopes
        .iter()
        .filter(|scope| scope.parent_scope_id.is_some())
        .collect()
}

#[test]
fn a_boundary_timer_on_a_sub_process_closes_the_scope_and_its_work() {
    let mut model = guarded_scope_model(ProcessNodeKind::BoundaryTimer {
        attached_to_id: "Scope_1".into(),
        cancel_activity: true,
        timer: tentaflow_protocol::processes::ProcessTimerSpec::Duration { seconds: 1 },
    });
    model.timer_timezone = Some("UTC".into());
    let mut store = started(model);
    let view = advance_until(&mut store, "the scope's task", |view| {
        !open_work(view).is_empty()
    });
    assert_eq!(open_work(&view)[0].node_id, "InnerWork");

    let view = advance_until(&mut store, "the boundary timer", |view| {
        view.events.iter().any(|event| event.kind == "timer_fired")
    });
    assert_eq!(
        view.clock.now_ms - view.clock.start_ms,
        1_000,
        "one second of virtual time"
    );
    let scope = child_scopes(&view)[0];
    assert_eq!(scope.status, ProcessInstanceStatus::Cancelled);
    assert!(
        open_work(&view)
            .iter()
            .all(|task| task.node_id != "InnerWork"),
        "the task of the closed scope is no longer open"
    );
    let handler = advance_until(&mut store, "the handler's task", |view| {
        open_work(view)
            .iter()
            .any(|task| task.node_id == "Handler_1")
    });
    assert_eq!(open_work(&handler).len(), 1);
}

#[test]
fn an_error_end_inside_a_scope_is_caught_on_the_sub_process() {
    let mut model = guarded_scope_model(ProcessNodeKind::BoundaryError {
        attached_to_id: "Scope_1".into(),
        error_ref: Some("Rejected".into()),
        output_mapping: BTreeMap::from([("caught".into(), "outputs.answer".into())]),
    });
    model
        .errors
        .push(tentaflow_protocol::processes::ProcessErrorDeclaration {
            error_id: "Rejected".into(),
            name: "Rejected".into(),
            error_code: "REJECTED".into(),
        });
    for node in &mut model.nodes {
        if let ProcessNodeKind::SubProcess { body, .. } = &mut node.kind {
            for inner in &mut body.nodes {
                if inner.id == "InnerEnd" {
                    inner.kind = ProcessNodeKind::ErrorEnd {
                        error_ref: "Rejected".into(),
                    };
                }
            }
        }
    }
    let mut store = started(model);
    advance_until(&mut store, "the scope's task", |view| {
        !open_work(view).is_empty()
    });
    let view = complete_open_work(&mut store, json!({"answer": "no"}));
    let view = if open_work(&view)
        .iter()
        .any(|task| task.node_id == "Handler_1")
    {
        view
    } else {
        advance_until(&mut store, "the handler's task", |view| {
            open_work(view)
                .iter()
                .any(|task| task.node_id == "Handler_1")
        })
    };

    assert!(view
        .events
        .iter()
        .any(|event| event.kind == "error_end_reached"));
    assert!(view
        .events
        .iter()
        .any(|event| event.kind == "business_error_caught"));
    assert_eq!(
        child_scopes(&view)[0].status,
        ProcessInstanceStatus::Error,
        "the scope that ended in the error is closed as failed"
    );
    assert_eq!(
        view.instance.as_ref().expect("instance").variables["caught"],
        json!("no")
    );
}

#[test]
fn a_service_step_inside_a_scope_waits_for_its_answer_and_hands_it_out() {
    use tentaflow_protocol::processes::{
        ActivityOutcome, ActivityResult, ActivityVerification, ServiceImplementation,
    };
    let mut model = starter_model();
    let inner = body(
        vec![
            node("InnerStart", ProcessNodeKind::Start),
            node(
                "Service_1",
                ProcessNodeKind::ServiceTask {
                    retry: None,
                    flow_id: "flow-simulation-service".into(),
                    input_mapping: BTreeMap::new(),
                    output_mapping: BTreeMap::from([("got".into(), "outputs.answer".into())]),
                    verification: ActivityVerification::Condition {
                        expression: "true".into(),
                    },
                    timeout_seconds: 30,
                    result_expression: None,
                    implementation: ServiceImplementation::default(),
                },
            ),
            node("InnerEnd", ProcessNodeKind::End),
        ],
        vec![
            edge("InnerFlow1", "InnerStart", "Service_1"),
            edge("InnerFlow2", "Service_1", "InnerEnd"),
        ],
    );
    model.nodes = vec![
        model.nodes[0].clone(),
        node(
            "Scope_1",
            sub_process(inner, &[], &[("result", "outputs.got")]),
        ),
        model.nodes.last().expect("starter End").clone(),
    ];
    model.sequence_flows = vec![
        edge("ToScope", "Start_1", "Scope_1"),
        edge("ScopeEnd", "Scope_1", "End_1"),
    ];
    let mut store = started(model);
    let view = advance_until(&mut store, "the Service wait", |view| {
        !view.service_waits.is_empty()
    });
    assert_eq!(view.service_waits[0].node_id, "Service_1");
    let answer = ActivityResult {
        outcome: ActivityOutcome::Completed,
        code: None,
        summary: "answered".into(),
        outputs: json!({"answer": 7}),
        evidence: Vec::new(),
    };
    let done = store
        .resolve_service(authorize(&store), "Service_1", None, answer, None)
        .expect("answer the step inside the scope");
    let instance = done.instance.as_ref().expect("instance");
    assert_eq!(instance.status, ProcessInstanceStatus::Completed);
    assert_eq!(instance.variables["result"], json!(7));
}

#[test]
fn a_plan_that_opens_a_scope_the_allocator_never_issued_is_refused() {
    use super::repository::{PlannedScope, RuntimePlan};
    use super::simulation::{validate_plan_ids, SimulationIdSource};
    use std::collections::HashSet;

    let mut ids = SimulationIdSource::new(&"a".repeat(64)).expect("allocator");
    let scope_id = super::runtime::RuntimeIdSource::next_id(&mut ids, "runtime");
    let planned = |scope_id: &str| {
        let mut plan = RuntimePlan::initial(json!({}));
        plan.create_scopes.push(PlannedScope {
            scope_id: scope_id.into(),
            parent_scope_id: uuid::Uuid::nil().to_string(),
            parent_token_id: uuid::Uuid::nil().to_string(),
            subprocess_node_id: "Scope_1".into(),
            variables: json!({}),
        });
        plan
    };
    validate_plan_ids(&planned(&scope_id), &ids, &HashSet::new()).expect("an issued scope id");
    let forged = uuid::Uuid::new_v4().to_string();
    let error = validate_plan_ids(&planned(&forged), &ids, &HashSet::new())
        .expect_err("a scope id from nowhere");
    assert!(
        format!("{error:#}").contains("scope identifier not issued"),
        "{error:#}"
    );
}

#[test]
fn an_escalation_raised_by_a_step_inside_a_scope_is_caught_by_its_boundary_inside_the_scope() {
    use tentaflow_protocol::processes::{
        ActivityOutcome, ActivityResult, ActivityVerification, ProcessEscalationDeclaration,
        ServiceImplementation,
    };
    let escalate =
        r#"{"outcome":"NeedsHuman","code":"ESC","summary":"decided","outputs":{},"evidence":[]}"#;
    let inner = body(
        vec![
            node("InnerStart", ProcessNodeKind::Start),
            node(
                "InnerWork",
                ProcessNodeKind::ServiceTask {
                    retry: None,
                    flow_id: "flow-simulation-service".into(),
                    input_mapping: BTreeMap::new(),
                    output_mapping: BTreeMap::new(),
                    verification: ActivityVerification::Condition {
                        expression: "true".into(),
                    },
                    timeout_seconds: 30,
                    result_expression: Some(escalate.into()),
                    implementation: ServiceImplementation::default(),
                },
            ),
            node("InnerEnd", ProcessNodeKind::End),
            node(
                "Guard_E",
                ProcessNodeKind::BoundaryEscalation {
                    attached_to_id: "InnerWork".into(),
                    escalation_ref: Some("Esc_1".into()),
                    cancel_activity: false,
                    output_mapping: BTreeMap::new(),
                },
            ),
            node("EscUser", user_task(Some("user-simulation"))),
            node("EscEnd", ProcessNodeKind::End),
        ],
        vec![
            edge("InnerFlow1", "InnerStart", "InnerWork"),
            edge("InnerFlow2", "InnerWork", "InnerEnd"),
            edge("EscFlow1", "Guard_E", "EscUser"),
            edge("EscFlow2", "EscUser", "EscEnd"),
        ],
    );
    let mut model = starter_model();
    model.escalations.push(ProcessEscalationDeclaration {
        escalation_id: "Esc_1".into(),
        name: "Help".into(),
        escalation_code: "ESC".into(),
    });
    model.nodes = vec![
        model.nodes[0].clone(),
        node("Scope_1", sub_process(inner, &[], &[])),
        model.nodes.last().expect("starter End").clone(),
    ];
    model.sequence_flows = vec![
        edge("ToScope", "Start_1", "Scope_1"),
        edge("ScopeEnd", "Scope_1", "End_1"),
    ];
    let mut store = started(model);
    advance_until(&mut store, "the Service wait", |view| {
        !view.service_waits.is_empty()
    });
    let answer = ActivityResult {
        outcome: ActivityOutcome::Completed,
        code: None,
        summary: "answered".into(),
        outputs: json!({}),
        evidence: Vec::new(),
    };
    store
        .resolve_service(authorize(&store), "InnerWork", None, answer, None)
        .expect("answer the step inside the scope");
    let view = advance_until(&mut store, "the handler's task", |view| {
        open_work(view).iter().any(|task| task.node_id == "EscUser")
    });
    assert!(view.events.iter().any(|event| {
        event.kind == "escalation_caught" && event.node_id.as_deref() == Some("Guard_E")
    }));
    let handler = open_work(&view)[0];
    assert_eq!(handler.scope_id, child_scopes(&view)[0].scope_id);
}

#[test]
fn an_interrupting_timer_on_a_sub_process_cancels_a_waiting_service_job_inside() {
    use tentaflow_protocol::processes::{
        ActivityVerification, ProcessTimerSpec, ServiceImplementation,
    };
    let mut model = guarded_scope_model(ProcessNodeKind::BoundaryTimer {
        attached_to_id: "Scope_1".into(),
        cancel_activity: true,
        timer: ProcessTimerSpec::Duration { seconds: 1 },
    });
    model.timer_timezone = Some("UTC".into());
    for step in &mut model.nodes {
        if let ProcessNodeKind::SubProcess { body, .. } = &mut step.kind {
            for inner in &mut body.nodes {
                if inner.id == "InnerWork" {
                    inner.kind = ProcessNodeKind::ServiceTask {
                        retry: None,
                        flow_id: "flow-simulation-service".into(),
                        input_mapping: BTreeMap::new(),
                        output_mapping: BTreeMap::new(),
                        verification: ActivityVerification::Condition {
                            expression: "true".into(),
                        },
                        timeout_seconds: 30,
                        result_expression: None,
                        implementation: ServiceImplementation::default(),
                    };
                }
            }
        }
    }
    let mut store = started(model);
    let waiting = advance_until(&mut store, "the Service wait", |view| {
        !view.service_waits.is_empty()
    });
    assert_eq!(
        waiting.service_waits[0].instance_id,
        waiting.instance.as_ref().expect("instance").instance_id
    );
    let view = advance_until(&mut store, "the handler's task", |view| {
        open_work(view)
            .iter()
            .any(|task| task.node_id == "Handler_1")
    });
    assert!(
        view.service_waits.is_empty(),
        "the job closed with its scope"
    );
    assert_eq!(
        child_scopes(&view)[0].status,
        ProcessInstanceStatus::Cancelled
    );
    assert!(view
        .events
        .iter()
        .any(|event| event.kind == "simulation_service_cancelled"
            && event.node_id.as_deref() == Some("InnerWork")));
}
