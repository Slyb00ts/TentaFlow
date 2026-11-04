// ============ File: simulation_event_tests.rs — boundary timers on the virtual clock ============

use std::collections::BTreeMap;

use serde_json::{json, Value};
use tentaflow_protocol::processes::{
    ProcessModel, ProcessNode, ProcessNodeKind, ProcessTimerSpec, ProcessUserTaskKind,
    ProcessUserTaskStatus,
};

use super::model::starter_model;
use super::runtime::test_support::edge;
use super::simulation::{AuthenticatedSimulationAction, SimulationStore, SimulationView};
use super::simulation_service_tests::normalize_events;
use super::simulation_tests::{create_store, source_input_for, test_authorization};

fn node(id: &str, kind: ProcessNodeKind) -> ProcessNode {
    ProcessNode { id: id.into(), name: format!("Step {id}"), kind, repeat: None, activity_io: None }
}

fn user_task(assignee: Option<&str>) -> ProcessNodeKind {
    ProcessNodeKind::UserTask {
        assignee_user_id: assignee.map(str::to_owned),
        output_mapping: BTreeMap::new(),
    }
}

/// Start, a user task with a boundary timer, and a handler that waits for a
/// person before ending: the shape every boundary path needs to be valid.
pub(crate) fn boundary_model(rule: ProcessTimerSpec, cancel_activity: bool, assignee: Option<&str>) -> ProcessModel {
    let mut model = starter_model();
    model.timer_timezone = Some("UTC".into());
    model.nodes = vec![
        model.nodes[0].clone(),
        node("User_1", user_task(assignee)),
        node(
            "Boundary_T",
            ProcessNodeKind::BoundaryTimer {
                attached_to_id: "User_1".into(),
                cancel_activity,
                timer: rule,
            },
        ),
        node("Handler_1", user_task(assignee)),
        node("End_T", ProcessNodeKind::End),
        model.nodes.last().expect("starter End").clone(),
    ];
    model.sequence_flows = vec![
        edge("ToUser", "Start_1", "User_1"),
        edge("UserEnd", "User_1", "End_1"),
        edge("ToHandler", "Boundary_T", "Handler_1"),
        edge("HandlerEnd", "Handler_1", "End_T"),
    ];
    model
}

fn authorize(store: &SimulationStore) -> AuthenticatedSimulationAction {
    test_authorization(store)
}

fn started(model: ProcessModel) -> SimulationStore {
    let mut store = create_store(source_input_for(model)).expect("capture the source");
    store.start(authorize(&store), json!({})).expect("start the run");
    store
}

fn fired(view: &SimulationView) -> usize {
    view.events.iter().filter(|event| event.kind == "timer_fired").count()
}

/// Advances until `count` timers have fired, or the horizon ends.
fn advance_until_fired(store: &mut SimulationStore, count: usize) -> SimulationView {
    let mut view = store.view(authorize(store)).expect("read the run");
    for _ in 0..80 {
        if fired(&view) >= count {
            return view;
        }
        view = store.advance(authorize(store)).expect("advance the run");
    }
    panic!("the timer never fired on the virtual clock");
}

fn open_tasks(view: &SimulationView, node_id: &str) -> usize {
    view.user_tasks
        .iter()
        .filter(|task| {
            task.node_id == node_id
                && task.status == ProcessUserTaskStatus::Open
                && task.kind == ProcessUserTaskKind::Work
        })
        .count()
}

#[test]
fn an_interrupting_boundary_timer_fires_on_the_virtual_clock_and_cancels_its_activity() {
    let mut store = started(boundary_model(
        ProcessTimerSpec::Duration { seconds: 1 },
        true,
        Some("user-simulation"),
    ));
    let view = advance_until_fired(&mut store, 1);
    // One second of virtual time after the start, not of wall time.
    assert_eq!(view.clock.now_ms - view.clock.start_ms, 1_000);
    assert_eq!(open_tasks(&view, "User_1"), 0, "the attached task was interrupted");
    assert_eq!(open_tasks(&view, "Handler_1"), 1);
    assert!(view.events.iter().any(|event| event.kind == "timer_fired"
        && event.node_id.as_deref() == Some("Boundary_T")));
}

#[test]
fn a_non_interrupting_boundary_timer_leaves_its_activity_open() {
    let mut store = started(boundary_model(
        ProcessTimerSpec::Duration { seconds: 1 },
        false,
        Some("user-simulation"),
    ));
    let view = advance_until_fired(&mut store, 1);
    assert_eq!(view.clock.now_ms - view.clock.start_ms, 1_000);
    assert_eq!(open_tasks(&view, "User_1"), 1, "a non-interrupting timer keeps the task");
    assert_eq!(open_tasks(&view, "Handler_1"), 1);
}

#[test]
fn a_boundary_timer_run_repeats_with_the_same_ids_and_trace() {
    let run = || {
        let mut store = started(boundary_model(
            ProcessTimerSpec::Duration { seconds: 1 },
            true,
            Some("user-simulation"),
        ));
        let view = advance_until_fired(&mut store, 1);
        (
            view.events
                .iter()
                .map(|event| (event.event_id.clone(), event.kind.clone(), event.data.clone()))
                .collect::<Vec<_>>(),
            view.trace_steps
                .iter()
                .map(|step| (step.trace_step_id.clone(), step.result_sha256.clone()))
                .collect::<Vec<_>>(),
        )
    };
    assert_eq!(run(), run());
}

#[test]
fn a_repeating_boundary_timer_is_refused_by_the_model_before_any_run_exists() {
    for rule in [
        ProcessTimerSpec::Cycle { seconds: 1, total_firings: Some(2) },
        ProcessTimerSpec::Daily { hour: 8, minute: 0, total_firings: None },
    ] {
        let model = boundary_model(rule, false, Some("user-simulation"));
        let message = format!("{:#}", create_store(source_input_for(model)).expect_err("repeat"));
        assert!(message.contains("repeating timer is only supported as a start event"), "{message}");
    }
}

#[test]
fn a_simulated_boundary_timer_path_equals_the_production_execution() {
    use super::repository;
    use super::runtime::test_support::{start_model, Fixture};
    use super::timers::drain_due;

    let production = Fixture::new();
    let model = boundary_model(ProcessTimerSpec::Duration { seconds: 1 }, true, None);
    let instance = start_model(&production, &model);
    let now = chrono::Utc::now().timestamp_millis();
    let drained = drain_due(&production.db, now + 5_000);
    drained.completion.expect("the production drain succeeds");
    assert_eq!(drained.fired, 1);
    let production_events = normalize_events(
        repository::list_events(&production.db, &production.owner, &instance.instance_id, 0, 100)
            .expect("production history")
            .0
            .into_iter()
            .map(|event| (event.kind, event.node_id, event.data))
            .collect(),
    );

    let mut store = started(model);
    let view = advance_until_fired(&mut store, 1);
    let simulated_events = normalize_events(
        view.events
            .iter()
            // The simulation also names the work an interrupting timer cancelled.
            .filter(|event| !event.kind.starts_with("simulation_"))
            .map(|event| (event.kind.clone(), event.node_id.clone(), event.data.clone()))
            .collect::<Vec<(String, Option<String>, Value)>>(),
    );
    assert_eq!(simulated_events, production_events);
}

#[test]
fn an_interrupting_timer_on_a_waiting_service_step_cancels_its_job() {
    use tentaflow_protocol::processes::{ActivityVerification, ServiceImplementation};

    let mut model = boundary_model(
        ProcessTimerSpec::Duration { seconds: 1 },
        true,
        Some("user-simulation"),
    );
    model.nodes[1] = node(
        "User_1",
        ProcessNodeKind::ServiceTask {
            flow_id: "flow-simulation-service".into(),
            input_mapping: BTreeMap::new(),
            output_mapping: BTreeMap::new(),
            verification: ActivityVerification::Human,
            timeout_seconds: 30,
            result_expression: None,
            implementation: ServiceImplementation::default(),
        },
    );
    let mut store = started(model);
    let before = store.view(authorize(&store)).expect("view");
    assert_eq!(before.service_waits.len(), 1);
    let view = advance_until_fired(&mut store, 1);
    assert!(view.service_waits.is_empty(), "the timer cancelled the waiting job");
    assert_eq!(open_tasks(&view, "Handler_1"), 1);
}
