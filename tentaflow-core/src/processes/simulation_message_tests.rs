// ============ File: simulation_message_tests.rs — messages, signals and event races on the virtual clock ============

use std::collections::BTreeMap;

use serde_json::{json, Value};
use tentaflow_protocol::processes::{
    ProcessInstanceStatus, ProcessMessageDeclaration, ProcessMessageStatus,
    ProcessMessageTargetSpec, ProcessModel, ProcessNode, ProcessNodeKind, ProcessSubscriptionKind,
    ProcessTimerSpec, ProcessUserTaskKind, ProcessUserTaskStatus,
};

use super::messages::test_support::receiving_model;
use super::runtime::test_support::edge;
use super::signal_tests::{signal_catch_model, signal_timer_race_model};
use super::simulation::{
    AuthenticatedSimulationAction, SimulationServiceError, SimulationStore, SimulationView,
    SIMULATION_EVENT_DELIVERY_UNSUPPORTED, SIMULATION_MESSAGE_INVALID,
    SIMULATION_MESSAGE_SUBSCRIPTION_UNKNOWN, SIMULATION_MESSAGE_TARGET_UNSUPPORTED,
};
use super::simulation_service_tests::normalize_events;
use super::simulation_tests::{create_store, source_input_for, test_authorization};

/// A published definition id is a UUID, and so is the address of a message.
const DEFINITION: &str = "5f1c2a40-0000-4000-8000-000000000001";
const OWNER: &str = "user-simulation";

fn node(id: &str, kind: ProcessNodeKind) -> ProcessNode {
    ProcessNode {
        id: id.into(),
        name: format!("Step {id}"),
        kind,
        repeat: None,
        activity_io: None,
    }
}

fn input_for(model: ProcessModel) -> super::simulation::SimulationSourceInput {
    let mut input = source_input_for(model);
    input.definition_id = DEFINITION.into();
    input
}

fn started_with_tick(model: ProcessModel, tick_duration_ms: i64) -> SimulationStore {
    let mut input = input_for(model);
    input.tick_duration_ms = tick_duration_ms;
    let mut store = create_store(input).expect("capture the source");
    store
        .start(authorize(&store), json!({}))
        .expect("start the run");
    store
}

fn authorize(store: &SimulationStore) -> AuthenticatedSimulationAction {
    test_authorization(store)
}

fn started(model: ProcessModel) -> SimulationStore {
    let mut store = create_store(input_for(model)).expect("capture the source");
    store
        .start(authorize(&store), json!({}))
        .expect("start the run");
    store
}

fn view(store: &SimulationStore) -> SimulationView {
    store.view(authorize(store)).expect("read the run")
}

fn advance(store: &mut SimulationStore) -> SimulationView {
    store.advance(authorize(store)).expect("advance the run")
}

/// Advances up to `times` ticks; a finished run accepts no more.
fn advance_times(store: &mut SimulationStore, times: usize) -> SimulationView {
    let mut latest = view(store);
    for _ in 0..times {
        if status(&latest) == ProcessInstanceStatus::Completed {
            break;
        }
        latest = advance(store);
    }
    latest
}

fn send(store: &mut SimulationStore, name: &str, key: &str) -> anyhow::Result<SimulationView> {
    store.send_message(
        authorize(store),
        name,
        key,
        json!({"value": 42}),
        None,
        None,
    )
}

fn count(view: &SimulationView, kind: &str) -> usize {
    view.events
        .iter()
        .filter(|event| event.kind == kind)
        .count()
}

fn status(view: &SimulationView) -> ProcessInstanceStatus {
    view.instance
        .as_ref()
        .expect("a started run has an instance")
        .status
        .clone()
}

fn code_of(error: &anyhow::Error) -> &'static str {
    error
        .downcast_ref::<SimulationServiceError>()
        .expect("a coded refusal")
        .code
}

fn message_catch(id: &str, key: &str) -> ProcessNode {
    node(
        id,
        ProcessNodeKind::MessageCatch {
            message_ref: "Message_1".into(),
            correlation_expression: format!("'{key}'"),
            output_mapping: BTreeMap::from([("received".into(), "outputs".into())]),
        },
    )
}

fn message_throw(id: &str, send_task: bool, key: &str) -> ProcessNode {
    let target = ProcessMessageTargetSpec::Catch {
        definition_id: DEFINITION.into(),
        instance_id_expression: None,
        subscription_id_expression: None,
    };
    let (message_ref, correlation_expression, payload_expression) = (
        "Message_1".to_owned(),
        format!("'{key}'"),
        "{'from': 'model'}".to_owned(),
    );
    node(
        id,
        if send_task {
            ProcessNodeKind::SendTask {
                message_ref,
                target,
                correlation_expression,
                payload_expression,
                ttl_seconds: 120,
            }
        } else {
            ProcessNodeKind::MessageThrow {
                message_ref,
                target,
                correlation_expression,
                payload_expression,
                ttl_seconds: 120,
            }
        },
    )
}

fn user_task(id: &str) -> ProcessNode {
    node(
        id,
        ProcessNodeKind::UserTask {
            assignee_user_id: Some(OWNER.into()),
            output_mapping: BTreeMap::new(),
        },
    )
}

/// Start, a fork, a catching branch and a throwing branch behind a person (so
/// the wait is armed before anything is thrown), and the join that ends them.
fn fork_model(catch: ProcessNode, throw_chain: Vec<ProcessNode>) -> ProcessModel {
    let mut model = receiving_model(false, false);
    model.signals = signal_catch_model().signals;
    model.target_namespace = signal_catch_model().target_namespace;
    let catch_id = catch.id.clone();
    let first = throw_chain.first().expect("a throwing branch").id.clone();
    let last = throw_chain.last().expect("a throwing branch").id.clone();
    let chain_ids = throw_chain
        .iter()
        .map(|step| step.id.clone())
        .collect::<Vec<_>>();
    model.nodes = vec![
        model.nodes[0].clone(),
        node("Split", ProcessNodeKind::ParallelGateway),
        catch,
    ];
    model.nodes.extend(throw_chain);
    model
        .nodes
        .push(node("Join", ProcessNodeKind::ParallelGateway));
    model.nodes.push(node("End_1", ProcessNodeKind::End));
    // A declaration nobody references is a model error.
    let message_refs = model
        .nodes
        .iter()
        .filter_map(|step| match &step.kind {
            ProcessNodeKind::MessageCatch { message_ref, .. }
            | ProcessNodeKind::MessageThrow { message_ref, .. }
            | ProcessNodeKind::SendTask { message_ref, .. }
            | ProcessNodeKind::ReceiveTask { message_ref, .. } => Some(message_ref.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    model
        .messages
        .retain(|declaration| message_refs.contains(&declaration.message_id));
    let signal_refs = model
        .nodes
        .iter()
        .filter_map(|step| match &step.kind {
            ProcessNodeKind::SignalCatch { signal_ref, .. }
            | ProcessNodeKind::SignalThrow { signal_ref, .. } => Some(signal_ref.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    model
        .signals
        .retain(|declaration| signal_refs.contains(&declaration.signal_id));
    model.sequence_flows = vec![
        edge("ToSplit", "Start_1", "Split"),
        edge("ToCatch", "Split", &catch_id),
        edge("ToThrow", "Split", &first),
        edge("CatchJoin", &catch_id, "Join"),
        edge("ThrowJoin", &last, "Join"),
        edge("JoinEnd", "Join", "End_1"),
    ];
    for pair in chain_ids.windows(2) {
        model.sequence_flows.push(edge(
            &format!("{}-{}", pair[0], pair[1]),
            &pair[0],
            &pair[1],
        ));
    }
    model
}

fn message_race_model() -> ProcessModel {
    let mut model = receiving_model(false, false);
    model.timer_timezone = Some("UTC".into());
    model.nodes = vec![
        model.nodes[0].clone(),
        node("Race_1", ProcessNodeKind::EventBasedGateway),
        message_catch("Catch_1", "case-1"),
        node(
            "Timer_1",
            ProcessNodeKind::TimerCatch {
                timer: ProcessTimerSpec::Duration { seconds: 2 },
            },
        ),
        node("End_1", ProcessNodeKind::End),
    ];
    model.sequence_flows = vec![
        edge("ToRace", "Start_1", "Race_1"),
        edge("ToMessage", "Race_1", "Catch_1"),
        edge("ToTimer", "Race_1", "Timer_1"),
        edge("MessageEnd", "Catch_1", "End_1"),
        edge("TimerEnd", "Timer_1", "End_1"),
    ];
    model
}

/// The rows a run keeps for its messages, signals and waits: their identities
/// come from the allocator or are derived, never random.
fn event_rows(store: &SimulationStore) -> Vec<Vec<Vec<String>>> {
    let rows = store.private_row_snapshot().expect("snapshot");
    [
        "bpmn_messages",
        "bpmn_signal_emissions",
        "bpmn_signal_receipts",
        "bpmn_event_subscriptions",
        "audit_log",
    ]
    .iter()
    .map(|table| rows[*table].clone())
    .collect()
}

fn completed_message(
    view: &SimulationView,
) -> Option<&tentaflow_protocol::processes::ProcessMessageSummary> {
    view.messages
        .iter()
        .find(|message| message.status == ProcessMessageStatus::Delivered)
}

#[test]
fn an_open_message_wait_is_listed_for_the_operator_to_answer() {
    let store = started(receiving_model(false, false));
    let waits = view(&store).subscriptions;
    assert_eq!(waits.len(), 1);
    let wait = &waits[0];
    assert_eq!(wait.kind, ProcessSubscriptionKind::MessageCatch);
    assert_eq!(wait.node_id, "Catch_1");
    assert_eq!(wait.message_name.as_deref(), Some("EvidenceReady"));
    assert_eq!(wait.correlation_key.as_deref(), Some("case-1"));
    assert_eq!((wait.race_id.clone(), wait.due_at_ms), (None, None));
}

#[test]
fn an_operator_message_is_queued_then_delivered_by_the_next_advance() {
    let mut store = started(receiving_model(false, false));
    let sent = send(&mut store, "EvidenceReady", "case-1").expect("queue the message");
    assert_eq!(sent.messages.len(), 1);
    assert_eq!(sent.messages[0].status, ProcessMessageStatus::Pending);
    assert_eq!(count(&sent, "simulation_message_sent"), 1);
    assert_eq!(
        status(&sent),
        ProcessInstanceStatus::Waiting,
        "queued, not yet delivered"
    );

    let delivered = advance(&mut store);
    assert_eq!(count(&delivered, "message_delivered"), 1);
    assert!(completed_message(&delivered).is_some());
    assert!(delivered.subscriptions.is_empty(), "the wait was consumed");
    let received = delivered
        .events
        .iter()
        .find(|event| event.kind == "message_delivered")
        .expect("the delivery is recorded");
    assert_eq!(received.data["payload"], json!({"value": 42}));
    let finished = advance_times(&mut store, 3);
    assert_eq!(status(&finished), ProcessInstanceStatus::Completed);
}

#[test]
fn a_message_with_another_correlation_key_never_reaches_the_wait() {
    let mut store = started(receiving_model(false, false));
    send(&mut store, "EvidenceReady", "case-2").expect("queue the message");
    let waiting = advance_times(&mut store, 5);
    assert_eq!(count(&waiting, "message_delivered"), 0);
    assert_eq!(waiting.subscriptions.len(), 1, "the wait is still open");
    assert_eq!(waiting.messages[0].status, ProcessMessageStatus::Pending);
    assert_eq!(status(&waiting), ProcessInstanceStatus::Waiting);
}

#[test]
fn a_message_expires_on_the_simulation_clock_not_the_wall_clock() {
    let mut store = started(receiving_model(false, false));
    let sent = store
        .send_message(
            authorize(&store),
            "EvidenceReady",
            "case-2",
            json!({}),
            Some(1),
            None,
        )
        .expect("queue the message with a one second lifetime");
    let queued_at = sent.messages[0].received_at_ms;
    assert_eq!(sent.messages[0].expires_at_ms - queued_at, 1_000);
    // Five ticks of 100 ms are half the lifetime: still alive, however long it
    // took to run them.
    let alive = advance_times(&mut store, 5);
    assert_eq!(alive.messages[0].status, ProcessMessageStatus::Pending);
    assert!(
        alive.messages[0].can_cancel,
        "the lifetime is judged on the virtual clock"
    );
    let expired = advance_times(&mut store, 6);
    assert_eq!(expired.messages[0].status, ProcessMessageStatus::Expired);
    assert!(expired.clock.now_ms >= queued_at + 1_000);
    assert!(!expired.messages[0].can_cancel);
    assert_eq!(count(&expired, "message_delivered"), 0);
    let trace = expired
        .trace_steps
        .iter()
        .filter(|step| step.data["result"].get("expired_messages").is_some())
        .count();
    assert_eq!(trace, 1, "the expiry is part of exactly one traced step");
}

#[test]
fn a_second_message_for_a_consumed_wait_is_not_delivered_twice() {
    // A person step after the catch keeps the run open, so the duplicate is
    // examined against a wait that is gone rather than against a finished run.
    let mut model = receiving_model(false, false);
    model.nodes.insert(2, user_task("Work_1"));
    model.sequence_flows = vec![
        edge("ToCatch", "Start_1", "Catch_1"),
        edge("CatchWork", "Catch_1", "Work_1"),
        edge("WorkEnd", "Work_1", "End_1"),
    ];
    let mut store = started(model);
    send(&mut store, "EvidenceReady", "case-1").expect("first message");
    send(&mut store, "EvidenceReady", "case-1").expect("duplicate message");
    let settled = advance_times(&mut store, 8);
    assert_eq!(status(&settled), ProcessInstanceStatus::Waiting);
    assert_eq!(count(&settled, "message_delivered"), 1);
    let delivered = settled
        .messages
        .iter()
        .filter(|message| message.status == ProcessMessageStatus::Delivered)
        .count();
    let pending = settled
        .messages
        .iter()
        .filter(|message| message.status == ProcessMessageStatus::Pending)
        .count();
    assert_eq!(
        (delivered, pending),
        (1, 1),
        "the duplicate has no wait left to reach"
    );
}

#[test]
fn an_unknown_subscription_and_a_malformed_message_are_refused_with_stable_codes() {
    let mut store = started(receiving_model(false, false));
    let unknown = store
        .send_message(
            authorize(&store),
            "EvidenceReady",
            "case-1",
            json!({}),
            None,
            Some("00000000-0000-4000-8000-000000000000"),
        )
        .expect_err("no such wait");
    assert_eq!(code_of(&unknown), SIMULATION_MESSAGE_SUBSCRIPTION_UNKNOWN);
    let wrong_key = store
        .send_message(
            authorize(&store),
            "EvidenceReady",
            "case-9",
            json!({}),
            None,
            Some(&view(&store).subscriptions[0].subscription_id),
        )
        .expect_err("the wait has another key");
    assert_eq!(code_of(&wrong_key), SIMULATION_MESSAGE_SUBSCRIPTION_UNKNOWN);
    for (name, key, ttl) in [
        ("", "case-1", None),
        ("EvidenceReady", "", None),
        ("EvidenceReady", "case-1", Some(0)),
    ] {
        let error = store
            .send_message(authorize(&store), name, key, json!({}), ttl, None)
            .expect_err("malformed message");
        assert_eq!(
            code_of(&error),
            SIMULATION_MESSAGE_INVALID,
            "{name:?} {key:?} {ttl:?}"
        );
    }
    assert!(
        view(&store).messages.is_empty(),
        "a refused message leaves no trace in the run"
    );
}

#[test]
fn a_message_that_matches_two_waits_is_recorded_ambiguous_and_never_delivered_by_itself() {
    let mut model = receiving_model(false, false);
    model.nodes = vec![
        model.nodes[0].clone(),
        node("Split", ProcessNodeKind::ParallelGateway),
        message_catch("Catch_1", "case-1"),
        message_catch("Catch_2", "case-1"),
        node("Join", ProcessNodeKind::ParallelGateway),
        node("End_1", ProcessNodeKind::End),
    ];
    model.sequence_flows = vec![
        edge("ToSplit", "Start_1", "Split"),
        edge("ToOne", "Split", "Catch_1"),
        edge("ToTwo", "Split", "Catch_2"),
        edge("OneJoin", "Catch_1", "Join"),
        edge("TwoJoin", "Catch_2", "Join"),
        edge("JoinEnd", "Join", "End_1"),
    ];
    let mut store = started(model);
    let waits = advance_times(&mut store, 1).subscriptions;
    assert_eq!(waits.len(), 2, "both branches are waiting");
    send(&mut store, "EvidenceReady", "case-1").expect("ambiguous message");
    let ambiguous = advance_times(&mut store, 2);
    assert_eq!(
        count(&ambiguous, "message_delivered"),
        0,
        "an ambiguous address is never guessed"
    );
    assert_eq!(
        ambiguous.messages[0].status,
        ProcessMessageStatus::Ambiguous
    );
    assert!(ambiguous.messages[0].can_resolve && ambiguous.messages[0].can_cancel);

    // The operator answers one wait exactly with a message of its own.
    let second = waits
        .iter()
        .find(|wait| wait.node_id == "Catch_2")
        .expect("second wait");
    store
        .send_message(
            authorize(&store),
            "EvidenceReady",
            "case-1",
            json!({"value": "exact"}),
            None,
            Some(&second.subscription_id),
        )
        .expect("an exact address");
    let delivered = advance_times(&mut store, 4);
    let deliveries = delivered
        .events
        .iter()
        .filter(|event| event.kind == "message_delivered")
        .map(|event| {
            (
                event.node_id.clone().unwrap_or_default(),
                event.data["payload"].clone(),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        deliveries,
        vec![("Catch_2".to_owned(), json!({"value": "exact"}))]
    );
    // With one wait gone the first message is still ambiguous in production's
    // sense: only an explicit resolution delivers it.
    let statuses = delivered
        .messages
        .iter()
        .map(|message| message.status.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        statuses,
        vec![
            ProcessMessageStatus::Ambiguous,
            ProcessMessageStatus::Delivered
        ]
    );
    assert_eq!(delivered.subscriptions.len(), 1);
}

#[test]
fn a_message_thrown_inside_the_model_reaches_the_catch_of_the_same_run() {
    for send_task in [false, true] {
        let model = fork_model(
            message_catch("Catch_1", "case-1"),
            vec![
                user_task("Work_1"),
                message_throw("Throw_1", send_task, "case-1"),
            ],
        );
        let mut store = started(model);
        let mut latest = advance_times(&mut store, 1);
        let task = latest
            .user_tasks
            .iter()
            .find(|task| {
                task.status == ProcessUserTaskStatus::Open && task.kind == ProcessUserTaskKind::Work
            })
            .expect("the person step opens")
            .user_task_id
            .clone();
        store
            .complete_user_task(authorize(&store), &task, json!({}), None)
            .expect("complete the person step");
        for _ in 0..12 {
            latest = advance(&mut store);
            if status(&latest) == ProcessInstanceStatus::Completed {
                break;
            }
        }
        assert_eq!(
            status(&latest),
            ProcessInstanceStatus::Completed,
            "send_task={send_task}"
        );
        // The recipient's delivery and the receipt the sender's history keeps.
        assert_eq!(count(&latest, "message_delivered"), 2);
        let delivered = latest
            .events
            .iter()
            .find(|event| event.kind == "message_delivered")
            .expect("the throw is delivered");
        assert_eq!(delivered.data["payload"], json!({"from": "model"}));
        assert!(completed_message(&latest).is_some());
    }
}

#[test]
fn a_signal_thrown_inside_the_model_reaches_the_signal_catch_of_the_same_run() {
    let signal_catch = signal_catch_model().nodes[1].clone();
    let throw = node(
        "Throw_1",
        ProcessNodeKind::SignalThrow {
            signal_ref: "Signal_1".into(),
            payload_expression: "{'business_key': 'PO-7'}".into(),
            ttl_seconds: 120,
        },
    );
    let mut store = started(fork_model(signal_catch, vec![user_task("Work_1"), throw]));
    let waiting = advance_times(&mut store, 1);
    assert_eq!(waiting.subscriptions.len(), 1);
    assert_eq!(
        waiting.subscriptions[0].kind,
        ProcessSubscriptionKind::SignalCatch
    );
    assert_eq!(
        waiting.subscriptions[0].signal_declaration_id.as_deref(),
        Some("Signal_1"),
        "a signal wait is listed, but only the model's own throw can release it"
    );
    let task = waiting
        .user_tasks
        .iter()
        .find(|task| {
            task.status == ProcessUserTaskStatus::Open && task.kind == ProcessUserTaskKind::Work
        })
        .expect("the person step opens")
        .user_task_id
        .clone();
    store
        .complete_user_task(authorize(&store), &task, json!({}), None)
        .expect("complete the person step");
    let mut latest = view(&store);
    for _ in 0..12 {
        latest = advance(&mut store);
        if status(&latest) == ProcessInstanceStatus::Completed {
            break;
        }
    }
    assert_eq!(status(&latest), ProcessInstanceStatus::Completed);
    assert_eq!(count(&latest, "signal_admitted"), 1);
    assert_eq!(count(&latest, "signal_received"), 1);
}

#[test]
fn a_receive_task_waits_for_its_message() {
    let mut model = receiving_model(false, false);
    model.nodes[1] = node(
        "Catch_1",
        ProcessNodeKind::ReceiveTask {
            message_ref: "Message_1".into(),
            correlation_expression: "'case-1'".into(),
            output_mapping: BTreeMap::from([("received".into(), "outputs".into())]),
        },
    );
    let mut store = started(model);
    assert_eq!(
        view(&store).subscriptions[0].kind,
        ProcessSubscriptionKind::ReceiveTask
    );
    send(&mut store, "EvidenceReady", "case-1").expect("queue the message");
    let latest = advance_times(&mut store, 4);
    assert_eq!(count(&latest, "receive_task_completed"), 1);
    assert_eq!(status(&latest), ProcessInstanceStatus::Completed);
}

#[test]
fn an_interrupting_boundary_message_cancels_the_task_it_is_attached_to() {
    let mut model = super::simulation_event_tests::boundary_model(
        ProcessTimerSpec::Duration { seconds: 60 },
        true,
        Some(OWNER),
    );
    model.timer_timezone = None;
    model.messages.push(ProcessMessageDeclaration {
        message_id: "Message_1".into(),
        name: "EvidenceReady".into(),
    });
    model.nodes[2] = node(
        "Boundary_T",
        ProcessNodeKind::BoundaryMessage {
            attached_to_id: "User_1".into(),
            cancel_activity: true,
            message_ref: "Message_1".into(),
            correlation_expression: "'case-1'".into(),
            output_mapping: BTreeMap::new(),
        },
    );
    let mut store = started(model);
    let waiting = view(&store);
    assert_eq!(
        waiting.subscriptions[0].kind,
        ProcessSubscriptionKind::BoundaryMessage
    );
    send(&mut store, "EvidenceReady", "case-1").expect("queue the message");
    let latest = advance_times(&mut store, 2);
    let open = |node_id: &str| {
        latest
            .user_tasks
            .iter()
            .filter(|task| task.node_id == node_id && task.status == ProcessUserTaskStatus::Open)
            .count()
    };
    assert_eq!(open("User_1"), 0, "the attached task was interrupted");
    assert_eq!(open("Handler_1"), 1, "the handler opened");
    assert_eq!(count(&latest, "message_delivered"), 1);
}

#[test]
fn a_message_wins_the_event_race_against_the_timer_and_the_timer_is_disarmed() {
    let mut store = started(message_race_model());
    let racing = view(&store);
    assert_eq!(racing.subscriptions.len(), 1);
    let wait = &racing.subscriptions[0];
    assert!(wait.race_id.is_some());
    assert_eq!(
        wait.due_at_ms,
        Some(racing.clock.start_ms + 2_000),
        "the wait names when its rival timer fires"
    );
    send(&mut store, "EvidenceReady", "case-1").expect("queue the message");
    let latest = advance_times(&mut store, 4);
    assert_eq!(count(&latest, "event_race_won"), 1);
    assert_eq!(count(&latest, "message_delivered"), 1);
    assert_eq!(
        count(&latest, "timer_fired"),
        0,
        "the lost timer never fires"
    );
    assert_eq!(status(&latest), ProcessInstanceStatus::Completed);
    assert!(latest
        .timers
        .iter()
        .all(|timer| timer.last_reason.as_deref() != Some("never")));
}

#[test]
fn the_timer_wins_the_event_race_and_a_late_message_is_refused() {
    let mut store = started(message_race_model());
    let mut latest = view(&store);
    for _ in 0..40 {
        if count(&latest, "timer_fired") > 0 {
            break;
        }
        latest = advance(&mut store);
    }
    assert_eq!(count(&latest, "timer_fired"), 1);
    assert_eq!(latest.clock.now_ms - latest.clock.start_ms, 2_000);
    assert!(
        latest.subscriptions.is_empty(),
        "the message wait lost the race"
    );
    assert_eq!(status(&latest), ProcessInstanceStatus::Completed);
    let late = send(&mut store, "EvidenceReady", "case-1").expect_err("the run is over");
    assert!(format!("{late:#}").contains("closed"), "{late:#}");
}

#[test]
fn a_signal_race_with_a_timer_resolves_on_the_virtual_clock() {
    let mut store = started(signal_timer_race_model());
    let racing = view(&store);
    assert_eq!(racing.subscriptions.len(), 1);
    assert_eq!(
        racing.subscriptions[0].due_at_ms,
        Some(racing.clock.start_ms + 2_000)
    );
    let mut latest = racing;
    for _ in 0..40 {
        if status(&latest) == ProcessInstanceStatus::Completed {
            break;
        }
        latest = advance(&mut store);
    }
    assert_eq!(
        count(&latest, "timer_fired"),
        1,
        "only the timer can release this race"
    );
    assert_eq!(status(&latest), ProcessInstanceStatus::Completed);
}

#[test]
fn a_run_with_messages_repeats_with_the_same_ids_and_trace() {
    let run = || {
        let mut store = started(fork_model(
            message_catch("Catch_1", "case-1"),
            vec![
                user_task("Work_1"),
                message_throw("Throw_1", false, "case-1"),
            ],
        ));
        let waiting = advance_times(&mut store, 1);
        let task = waiting
            .user_tasks
            .iter()
            .find(|task| {
                task.status == ProcessUserTaskStatus::Open && task.kind == ProcessUserTaskKind::Work
            })
            .expect("the person step opens")
            .user_task_id
            .clone();
        store
            .complete_user_task(authorize(&store), &task, json!({}), None)
            .expect("complete the person step");
        // A stray message with another key stays queued beside the thrown one.
        send(&mut store, "EvidenceReady", "case-9").expect("a stray message");
        let latest_with_stray = advance_times(&mut store, 12);
        assert_eq!(status(&latest_with_stray), ProcessInstanceStatus::Completed);
        assert_eq!(latest_with_stray.messages.len(), 2);
        (
            latest_with_stray
                .events
                .iter()
                .map(|event| {
                    (
                        event.event_id.clone(),
                        event.kind.clone(),
                        event.data.clone(),
                    )
                })
                .collect::<Vec<_>>(),
            latest_with_stray
                .trace_steps
                .iter()
                .map(|step| (step.trace_step_id.clone(), step.result_sha256.clone()))
                .collect::<Vec<_>>(),
            latest_with_stray
                .messages
                .iter()
                .map(|message| (message.message_id.clone(), message.status.clone()))
                .collect::<Vec<_>>(),
            event_rows(&store),
        )
    };
    assert_eq!(run(), run());
}

#[test]
fn a_signal_run_repeats_with_the_same_ids_and_trace() {
    let run = || {
        let throw = node(
            "Throw_1",
            ProcessNodeKind::SignalThrow {
                signal_ref: "Signal_1".into(),
                payload_expression: "{'business_key': 'PO-7'}".into(),
                ttl_seconds: 120,
            },
        );
        let mut store = started(fork_model(
            signal_catch_model().nodes[1].clone(),
            vec![user_task("Work_1"), throw],
        ));
        let waiting = advance_times(&mut store, 1);
        let task = waiting
            .user_tasks
            .iter()
            .find(|task| {
                task.status == ProcessUserTaskStatus::Open && task.kind == ProcessUserTaskKind::Work
            })
            .expect("the person step opens")
            .user_task_id
            .clone();
        store
            .complete_user_task(authorize(&store), &task, json!({}), None)
            .expect("complete the person step");
        let latest = advance_times(&mut store, 10);
        assert_eq!(status(&latest), ProcessInstanceStatus::Completed);
        (
            latest
                .events
                .iter()
                .map(|event| {
                    (
                        event.event_id.clone(),
                        event.kind.clone(),
                        event.data.clone(),
                    )
                })
                .collect::<Vec<_>>(),
            latest
                .trace_steps
                .iter()
                .map(|step| (step.trace_step_id.clone(), step.result_sha256.clone()))
                .collect::<Vec<_>>(),
            event_rows(&store),
        )
    };
    assert_eq!(run(), run());
}

#[test]
fn nothing_a_message_run_does_leaves_the_private_database() {
    let mut store = started(message_race_model());
    let before = store.private_row_snapshot().expect("snapshot");
    assert_eq!(before["bpmn_messages"].len(), 0);
    send(&mut store, "EvidenceReady", "case-1").expect("queue the message");
    advance_times(&mut store, 4);
    let after = store.private_row_snapshot().expect("snapshot");
    assert_eq!(
        after["bpmn_messages"].len(),
        1,
        "the run queued its message in its own database"
    );
    assert_eq!(after["bpmn_signal_emissions"].len(), 0);
    assert!(
        after["audit_log"].is_empty(),
        "a private run keeps no audit row (it would carry the wall clock)"
    );
}

#[test]
fn refused_event_nodes_keep_their_stable_codes() {
    // A message start would create another instance.
    let mut start = receiving_model(false, false);
    start.nodes.push(node(
        "MessageStart_1",
        ProcessNodeKind::MessageStart {
            message_ref: "Message_1".into(),
            output_mapping: BTreeMap::new(),
        },
    ));
    start
        .sequence_flows
        .push(edge("MessageStartEnd", "MessageStart_1", "End_1"));
    let error = create_store(input_for(start)).expect_err("message start");
    let message = format!("{error:#}");
    assert!(
        message.starts_with(SIMULATION_EVENT_DELIVERY_UNSUPPORTED),
        "{message}"
    );
    assert!(message.contains("MessageStart"), "{message}");

    // Another process, an address expression, a start target: nothing a single
    // private run can reach.
    for target in [
        ProcessMessageTargetSpec::Catch {
            definition_id: "5f1c2a40-0000-4000-8000-000000000002".into(),
            instance_id_expression: None,
            subscription_id_expression: None,
        },
        ProcessMessageTargetSpec::Catch {
            definition_id: DEFINITION.into(),
            instance_id_expression: Some("'00000000-0000-4000-8000-000000000000'".into()),
            subscription_id_expression: None,
        },
        ProcessMessageTargetSpec::Start {
            definition_id: DEFINITION.into(),
            process_id: None,
            start_node_id: None,
        },
    ] {
        let mut model = receiving_model(false, false);
        let mut throw = message_throw("Throw_1", false, "case-1");
        let ProcessNodeKind::MessageThrow { target: slot, .. } = &mut throw.kind else {
            unreachable!("a throw")
        };
        *slot = target.clone();
        model.nodes[1] = throw;
        model.sequence_flows = vec![
            edge("ToThrow", "Start_1", "Throw_1"),
            edge("ThrowEnd", "Throw_1", "End_1"),
        ];
        let error = create_store(input_for(model)).expect_err("target beyond the run");
        let message = format!("{error:#}");
        assert!(
            message.starts_with(SIMULATION_MESSAGE_TARGET_UNSUPPORTED),
            "{target:?} {message}"
        );
        assert!(message.contains("Throw_1"), "{message}");
    }

    // Authored activity IO continues through a deferred external step.
    let mut io_catch = message_catch("Catch_1", "case-1");
    io_catch.activity_io = Some(tentaflow_protocol::processes::ProcessActivityIo {
        data_inputs: Vec::new(),
        data_outputs: Vec::new(),
        input_set_id: "InputSet_1".into(),
        input_set: Vec::new(),
        output_set_id: "OutputSet_1".into(),
        output_set: Vec::new(),
        input_associations: Vec::new(),
        output_associations: Vec::new(),
        coordinator_output: None,
    });
    let error = super::simulation::refuse_event_activity_io(&io_catch).expect_err("activity IO");
    let message = format!("{error:#}");
    assert!(
        message.starts_with(SIMULATION_EVENT_DELIVERY_UNSUPPORTED)
            && message.contains("MessageCatch with activity IO")
            && message.contains("Catch_1"),
        "{message}"
    );
}

#[test]
fn a_run_that_has_not_started_or_has_ended_accepts_no_message() {
    let mut store = create_store(input_for(receiving_model(false, false))).expect("capture");
    assert!(
        send(&mut store, "EvidenceReady", "case-1").is_err(),
        "no instance yet"
    );
    store.start(authorize(&store), json!({})).expect("start");
    send(&mut store, "EvidenceReady", "case-1").expect("queue");
    advance_times(&mut store, 5);
    assert!(
        send(&mut store, "EvidenceReady", "case-1").is_err(),
        "the run is over"
    );
}

/// Compares the history of a simulated and a production run of one model with
/// ids relabelled by first appearance and times masked.
fn comparable(
    events: Vec<(String, Option<String>, Value)>,
) -> Vec<(String, Option<String>, Value)> {
    normalize_events(
        events
            .into_iter()
            .filter(|(kind, _, _)| kind != "simulation_message_sent")
            .collect(),
    )
}

fn production_message(
    version: &tentaflow_protocol::processes::ProcessVersion,
    instance_id: &str,
) -> super::repository::PreparedMessage {
    super::repository::PreparedMessage {
        message_id: uuid::Uuid::new_v4().to_string(),
        target: tentaflow_protocol::processes::ProcessMessageTarget::Catch {
            definition_id: version.definition_id.clone(),
            instance_id: Some(instance_id.to_owned()),
            subscription_id: None,
        },
        message_name: "EvidenceReady".into(),
        correlation_key: "case-1".into(),
        payload: json!({"value": 42}),
        ttl_seconds: 120,
    }
}

#[test]
fn a_simulated_message_catch_path_equals_the_production_execution() {
    use super::messages::drain_pending;
    use super::repository;
    use super::runtime::test_support::{publish_model, stamp, Fixture};

    let model = receiving_model(false, false);
    let production = Fixture::new();
    let version = publish_model(&production, &model);
    let instance = super::messages::test_support::start_version(&production, &version);
    let now = chrono::Utc::now().timestamp_millis();
    repository::send_message(
        &production.db,
        &production.owner,
        &stamp("production message"),
        &production_message(&version, &instance.instance_id),
        now,
    )
    .expect("production send");
    drain_pending(&production.db, now + 1)
        .completion
        .expect("production drain");
    let production_events = comparable(
        repository::list_events(
            &production.db,
            &production.owner,
            &instance.instance_id,
            0,
            100,
        )
        .expect("production history")
        .0
        .into_iter()
        .map(|event| (event.kind, event.node_id, event.data))
        .collect(),
    );

    let mut store = started(model);
    send(&mut store, "EvidenceReady", "case-1").expect("queue the message");
    let latest = advance_times(&mut store, 6);
    let simulated = comparable(
        latest
            .events
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
    assert_eq!(simulated, production_events);
    assert!(production_events
        .iter()
        .any(|(kind, _, _)| kind == "message_delivered"));
}

fn race_history(model: &ProcessModel, message_first: bool) -> Vec<(String, Option<String>, Value)> {
    use super::messages::drain_pending;
    use super::repository;
    use super::runtime::test_support::{publish_model, stamp, Fixture};
    use super::timers::drain_due;

    let production = Fixture::new();
    let version = publish_model(&production, model);
    let instance = super::messages::test_support::start_version(&production, &version);
    let now = chrono::Utc::now().timestamp_millis();
    if message_first {
        repository::send_message(
            &production.db,
            &production.owner,
            &stamp("race message"),
            &production_message(&version, &instance.instance_id),
            now,
        )
        .expect("production send");
        drain_pending(&production.db, now + 1)
            .completion
            .expect("production drain");
    } else {
        drain_due(&production.db, now + 5_000)
            .completion
            .expect("production timers");
    }
    comparable(
        repository::list_events(
            &production.db,
            &production.owner,
            &instance.instance_id,
            0,
            100,
        )
        .expect("production history")
        .0
        .into_iter()
        .map(|event| (event.kind, event.node_id, event.data))
        .collect(),
    )
}

#[test]
fn a_simulated_event_race_equals_the_production_execution_for_either_winner() {
    let model = message_race_model();

    let mut store = started(model.clone());
    send(&mut store, "EvidenceReady", "case-1").expect("queue the message");
    let message_wins = advance_times(&mut store, 5);
    assert_eq!(
        comparable(
            message_wins
                .events
                .iter()
                .map(|event| (
                    event.kind.clone(),
                    event.node_id.clone(),
                    event.data.clone()
                ))
                .collect()
        ),
        race_history(&model, true),
        "message wins"
    );

    let mut store = started(model.clone());
    let mut latest = view(&store);
    for _ in 0..40 {
        if status(&latest) == ProcessInstanceStatus::Completed {
            break;
        }
        latest = advance(&mut store);
    }
    assert_eq!(
        comparable(
            latest
                .events
                .iter()
                .map(|event| (
                    event.kind.clone(),
                    event.node_id.clone(),
                    event.data.clone()
                ))
                .collect()
        ),
        race_history(&model, false),
        "timer wins"
    );
}

#[test]
fn a_plan_that_sends_beyond_the_run_is_refused_before_it_is_written() {
    use super::repository::{PlannedMessage, PreparedMessage, RuntimePlan};
    use tentaflow_protocol::processes::ProcessMessageTarget;

    let mut plan = RuntimePlan::initial(json!({}));
    plan.create_messages.push(PlannedMessage {
        message: PreparedMessage {
            message_id: uuid::Uuid::new_v4().to_string(),
            target: ProcessMessageTarget::Catch {
                definition_id: DEFINITION.into(),
                instance_id: Some(uuid::Uuid::new_v4().to_string()),
                subscription_id: None,
            },
            message_name: "EvidenceReady".into(),
            correlation_key: "case-1".into(),
            payload: json!({}),
            ttl_seconds: 60,
        },
        source_scope_id: "scope".into(),
        source_node_id: "Throw_1".into(),
        source_activation_id: "token".into(),
        source_event_index: 0,
    });
    let error = super::simulation::validate_supported_plan(&plan, &[]).expect_err("beyond the run");
    assert!(format!("{error:#}").contains("beyond the simulated process"));
    let mut start_target = plan.clone();
    start_target.create_messages[0].message.target = ProcessMessageTarget::Start {
        definition_id: DEFINITION.into(),
        process_id: None,
        start_node_id: None,
    };
    assert!(super::simulation::validate_supported_plan(&start_target, &[]).is_err());
}

fn mapping_failure_model() -> ProcessModel {
    let mut model = receiving_model(false, false);
    model.variables.insert("amount".into(), Value::Null);
    model.nodes[1] = node(
        "Catch_1",
        ProcessNodeKind::MessageCatch {
            message_ref: "Message_1".into(),
            correlation_expression: "'case-1'".into(),
            output_mapping: BTreeMap::from([("amount".into(), "outputs.amount + 1".into())]),
        },
    );
    model
}

#[test]
fn a_message_whose_output_mapping_fails_ends_in_an_incident_and_the_run_goes_on() {
    let mut store = started(mapping_failure_model());
    store
        .send_message(
            authorize(&store),
            "EvidenceReady",
            "case-1",
            json!({}),
            None,
            None,
        )
        .expect("queue the message");
    let failed = advance(&mut store);
    assert_eq!(failed.messages[0].status, ProcessMessageStatus::Error);
    assert_eq!(status(&failed), ProcessInstanceStatus::Incident);
    assert_eq!(
        failed
            .incidents
            .iter()
            .map(|incident| incident.code.as_str())
            .collect::<Vec<_>>(),
        vec!["MESSAGE_MAPPING_ERROR"]
    );
    assert_eq!(count(&failed, "message_error"), 1);
    // The run is not wedged on the same delivery: the clock keeps moving.
    let later = advance(&mut store);
    assert!(later.clock.now_ms > failed.clock.now_ms);
}

#[test]
fn a_simulated_mapping_failure_equals_the_production_outcome() {
    use super::messages::drain_pending;
    use super::repository;
    use super::runtime::test_support::{publish_model, stamp, Fixture};

    let model = mapping_failure_model();
    let production = Fixture::new();
    let version = publish_model(&production, &model);
    let instance = super::messages::test_support::start_version(&production, &version);
    let now = chrono::Utc::now().timestamp_millis();
    let mut message = production_message(&version, &instance.instance_id);
    message.payload = json!({});
    repository::send_message(
        &production.db,
        &production.owner,
        &stamp("mapping"),
        &message,
        now,
    )
    .expect("production send");
    drain_pending(&production.db, now + 1)
        .completion
        .expect("production drain");
    let production_events = comparable(
        repository::list_events(
            &production.db,
            &production.owner,
            &instance.instance_id,
            0,
            100,
        )
        .expect("production history")
        .0
        .into_iter()
        .map(|event| (event.kind, event.node_id, event.data))
        .collect(),
    );
    let production_status = repository::get_message(
        &production.db,
        &production.owner,
        &production.owner.user_id,
        &message.message_id,
    )
    .expect("production message")
    .message
    .status;

    let mut store = started(model);
    store
        .send_message(
            authorize(&store),
            "EvidenceReady",
            "case-1",
            json!({}),
            None,
            None,
        )
        .expect("queue the message");
    let latest = advance(&mut store);
    let simulated = comparable(
        latest
            .events
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
    assert_eq!(simulated, production_events);
    assert_eq!(latest.messages[0].status, production_status);
}

fn timer_then_work_race_model() -> ProcessModel {
    let mut model = message_race_model();
    model.nodes.insert(4, user_task("Work_1"));
    model.sequence_flows = vec![
        edge("ToRace", "Start_1", "Race_1"),
        edge("ToMessage", "Race_1", "Catch_1"),
        edge("ToTimer", "Race_1", "Timer_1"),
        edge("MessageEnd", "Catch_1", "End_1"),
        edge("TimerWork", "Timer_1", "Work_1"),
        edge("WorkEnd", "Work_1", "End_1"),
    ];
    model
}

#[test]
fn a_message_that_arrived_before_a_due_timer_is_delivered_before_it() {
    // The message is queued at +1.5 s and the two second timer falls due at +2 s;
    // the next advance (+3 s) finds both due, and the message arrived first.
    let model = message_race_model();
    let mut store = started_with_tick(model.clone(), 1_500);
    send(&mut store, "EvidenceReady", "case-1").expect("queue the message");
    let latest = advance_times(&mut store, 2);
    assert_eq!(count(&latest, "message_delivered"), 1);
    assert_eq!(
        count(&latest, "timer_fired"),
        0,
        "the earlier message won the race"
    );
    assert_eq!(
        comparable(
            latest
                .events
                .iter()
                .map(|event| (
                    event.kind.clone(),
                    event.node_id.clone(),
                    event.data.clone()
                ))
                .collect()
        ),
        race_history(&model, true)
    );
}

#[test]
fn a_timer_and_a_message_due_at_the_same_instant_go_timer_first() {
    let mut store = started_with_tick(timer_then_work_race_model(), 1_000);
    advance(&mut store);
    // The message is queued exactly when the two second timer falls due.
    let queued = send(&mut store, "EvidenceReady", "case-1").expect("queue the message");
    assert_eq!(
        queued.messages[0].received_at_ms,
        queued.timers[0].due_at_ms.expect("due")
    );
    let latest = advance(&mut store);
    assert_eq!(count(&latest, "timer_fired"), 1);
    assert_eq!(count(&latest, "message_delivered"), 0);
}

#[test]
fn a_message_addressed_to_a_wait_that_lost_its_race_is_cancelled_like_production() {
    let mut store = started_with_tick(timer_then_work_race_model(), 1_000);
    advance(&mut store);
    let wait = view(&store).subscriptions[0].subscription_id.clone();
    store
        .send_message(
            authorize(&store),
            "EvidenceReady",
            "case-1",
            json!({}),
            None,
            Some(&wait),
        )
        .expect("addressed while the wait is still open");
    // The timer fires first and the activation the message names is gone.
    let after_timer = advance(&mut store);
    assert_eq!(count(&after_timer, "timer_fired"), 1);
    let cancelled = advance(&mut store);
    assert_eq!(
        cancelled.messages[0].status,
        ProcessMessageStatus::Cancelled
    );
    assert_eq!(count(&cancelled, "message_delivered"), 0);
    assert_eq!(status(&cancelled), ProcessInstanceStatus::Waiting);
}

fn boundary_signal_model() -> ProcessModel {
    use super::simulation_event_tests::boundary_model;
    let mut model = boundary_model(ProcessTimerSpec::Duration { seconds: 1 }, false, None);
    model.signals = signal_catch_model().signals;
    model.target_namespace = signal_catch_model().target_namespace;
    model.nodes[3] = node(
        "Handler_1",
        ProcessNodeKind::SignalThrow {
            signal_ref: "Signal_1".into(),
            payload_expression: "{'k': 1}".into(),
            ttl_seconds: 120,
        },
    );
    model
}

#[test]
fn a_non_interrupting_boundary_timer_may_throw_a_signal_in_production() {
    use super::repository;
    use super::runtime::test_support::{start_model, Fixture};
    use super::timers::drain_due;

    let production = Fixture::new();
    let instance = start_model(&production, &boundary_signal_model());
    let now = chrono::Utc::now().timestamp_millis();
    let drained = drain_due(&production.db, now + 5_000);
    drained
        .completion
        .expect("the boundary path with a Signal throw drains");
    assert_eq!(drained.fired, 1);
    let events = repository::list_events(
        &production.db,
        &production.owner,
        &instance.instance_id,
        0,
        100,
    )
    .expect("history")
    .0;
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "signal_admitted")
            .count(),
        1
    );
    assert_eq!(
        events[events
            .iter()
            .position(|event| event.kind == "timer_fired")
            .unwrap()
            + 1]
        .kind,
        "signal_admitted"
    );
}

#[test]
fn a_non_interrupting_boundary_timer_may_throw_a_signal_in_a_simulation() {
    let mut store = started(boundary_signal_model());
    let mut latest = view(&store);
    for _ in 0..40 {
        if count(&latest, "signal_admitted") > 0 {
            break;
        }
        latest = advance(&mut store);
    }
    assert_eq!(count(&latest, "timer_fired"), 1);
    assert_eq!(count(&latest, "signal_admitted"), 1);
}

#[test]
fn production_delivery_keeps_random_receipt_ids_and_claim_fences() {
    use super::messages::test_support::start_version;
    use super::repository;
    use super::runtime::test_support::{publish_model, start_model, Fixture};
    use super::signal_tests::signal_throw_model;

    let production = Fixture::new();
    let recipient = start_version(
        &production,
        &publish_model(&production, &signal_catch_model()),
    );
    start_model(&production, &signal_throw_model());
    let now = chrono::Utc::now().timestamp_millis();
    let receipts = repository::due_signal_receipts(&production.db, now + 1).expect("due receipts");
    assert_eq!(receipts.len(), 1, "{recipient:?}");
    let version_of = |id: &str| uuid::Uuid::parse_str(id).expect("a UUID").get_version_num();
    assert_eq!(
        version_of(&receipts[0]),
        4,
        "a production receipt id is random"
    );
    let claim = repository::claim_signal_receipt(&production.db, &receipts[0], now + 1)
        .expect("claim")
        .expect("a pending receipt is claimable");
    assert_eq!(
        version_of(&claim.fence),
        4,
        "a production claim fence is random"
    );
}

#[test]
fn a_simulation_proof_does_not_open_a_production_transaction() {
    use super::messages::test_support::start_version;
    use super::repository::{self, MessageSelection, SimulatedDelivery};
    use super::runtime::test_support::{publish_model, stamp, Fixture};
    use super::runtime::RuntimeIdSourceHandle;
    use super::simulation::{PrivateSimulationMode, SimulationDatabase, SimulationIdSource};

    // A proof minted on a private database…
    let private = SimulationDatabase::open()
        .expect("private database")
        .into_connection_for_test();
    let mode =
        PrivateSimulationMode::prove_for_test(&private).expect("a private database proves itself");
    let ids: RuntimeIdSourceHandle = std::rc::Rc::new(std::cell::RefCell::new(
        SimulationIdSource::new(&"a".repeat(64)).unwrap(),
    ));
    let finish = |_: &mut repository::RuntimePlan| -> anyhow::Result<()> { Ok(()) };
    let simulated = SimulatedDelivery {
        mode,
        ids,
        finish_plan: &finish,
        receipt_event_id: uuid::Uuid::new_v4().to_string(),
    };

    // …is refused by every writer when it meets a production transaction.
    let model = receiving_model(false, false);
    let production = Fixture::new();
    let version = publish_model(&production, &model);
    let instance = start_version(&production, &version);
    let now = chrono::Utc::now().timestamp_millis();
    repository::send_message(
        &production.db,
        &production.owner,
        &stamp("proof"),
        &production_message(&version, &instance.instance_id),
        now,
    )
    .expect("production send");
    let candidate = repository::due_messages(&production.db, now + 1, 32)
        .unwrap()
        .remove(0);
    let MessageSelection::Ready(snapshot) =
        repository::message_snapshot(&production.db, &candidate).unwrap()
    else {
        panic!("a deliverable production message")
    };
    let mut connection = production.db.write().unwrap();
    let tx = connection.transaction().unwrap();
    let refusal = |error: anyhow::Error| format!("{error:#}");
    assert!(refusal(
        repository::deliver_message_in_tx(&tx, &snapshot, None, now, &|| Ok(now), Some(&simulated))
            .expect_err("delivery")
    )
    .contains("private simulation database"));
    assert!(refusal(
        repository::record_message_failed_in_tx(
            &tx,
            &candidate,
            "x",
            now,
            &|| Ok(now),
            Some(&simulated)
        )
        .err()
        .expect("failure record")
    )
    .contains("private simulation database"));
    assert!(refusal(
        repository::simulation_deliver_signal_on(&tx, &simulated, "receipt", now)
            .err()
            .expect("signal")
    )
    .contains("private simulation database"));
}
