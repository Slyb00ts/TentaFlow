// ============ File: processes/call_tests.rs — File-backed call instance, return, error, and cancellation behavior ============

use super::{messages, repository, runtime, timers};
use repository::{ProcessActor, ProcessTransitionOutcome};
use runtime::test_support::*;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tentaflow_protocol::processes::*;
use uuid::Uuid;

pub(super) fn caller(
    target: &ProcessVersion,
    output_mapping: BTreeMap<String, String>,
) -> ProcessModel {
    let mut model = super::model::starter_model();
    model.nodes.insert(
        1,
        ProcessNode {
            id: "Call_1".into(),
            name: "Call the published evidence process".into(),
            kind: ProcessNodeKind::CallActivity(ProcessCallActivity {
                target: ProcessCallTarget::PublishedBody {
                    definition_id: target.definition_id.clone(),
                    version: target.version,
                    called_element: ProcessCallableReference {
                    namespace_uri: target
                        .model
                        .target_namespace
                        .clone()
                        .unwrap_or_else(|| "https://tentaflow.app/bpmn/1".into()),
                    process_id: target.model.process_id.clone(),
                },
                },
                input_mapping: BTreeMap::new(),
                output_mapping,
            }),
            repeat: None,
            activity_io: None,
        },
    );
    model.sequence_flows = vec![
        edge("StartCall", "Start_1", "Call_1"),
        edge("CallEnd", "Call_1", "End_1"),
    ];
    model
}

#[test]
fn configured_call_captures_input_before_legacy_mapping_failure_and_rejects_forged_owner() {
    let fixture = Fixture::new();
    let target = publish_model(&fixture, &user_model(None));
    let mut model = caller(&target, BTreeMap::new());
    model.modeling = Some(ProcessBodyModeling::default());
    model.nodes[1].activity_io = Some(ProcessActivityIo {
        data_inputs: Vec::new(), data_outputs: Vec::new(),
        input_set_id: "Call_Input_Set".into(), input_set: Vec::new(),
        output_set_id: "Call_Output_Set".into(), output_set: Vec::new(),
        input_associations: Vec::new(), output_associations: Vec::new(),
        coordinator_output: None,
    });
    let ProcessNodeKind::CallActivity(call) = &mut model.nodes[1].kind else {
        unreachable!()
    };
    call.input_mapping.insert("answer".into(), "1 / 0".into());
    let version = publish_model(&fixture, &model);
    let instance_id = Uuid::new_v4().to_string();
    let variables = serde_json::to_value(&version.model.variables).unwrap();
    let command = stamp("configured Call legacy mapping failure");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&version.model, &version.model.process_id,
        "Start_1", &instance_id, &fixture.owner, &version.definition_id,
        version.version, variables.clone(), runtime::StartCause::Manual,
        at_ms, runtime::test_support::manual_input(&command), None).unwrap();
    let capture_index = plan.events.iter().position(|event|
        event.kind == "activity_io_input_captured").unwrap();
    let incident_index = plan.events.iter().position(|event|
        event.kind == "incident" && event.data["code"] == "CALL_ADMISSION_ERROR").unwrap();
    assert!(capture_index < incident_index);
    assert!(plan.call_requests.is_empty());
    let waiting = plan.activity_io_inputs.iter().find_map(|fact| match fact {
        repository::ActivityIoInputFact::Captured { activation_token_id, .. } =>
            Some(activation_token_id.as_str()),
        _ => None,
    }).unwrap();
    assert_eq!(plan.events[incident_index].data["activation_token_id"], waiting);
    assert_eq!(plan.events[incident_index].data["incident_id"],
        plan.add_incidents[0].incident_id);

    let before = transition_rows(&fixture);
    let mut forged = plan.clone();
    forged.events[incident_index].data["activation_token_id"] =
        json!(Uuid::new_v4().to_string());
    let rejection = repository::start_instance(&fixture.db, &fixture.owner,
        &command, &instance_id, &version.definition_id, version.version,
        &variables, None, None, repository::ProcessPlanInput::Supplied(&forged),
        at_ms).unwrap_err();
    assert!(format!("{rejection:#}").contains("activity IO input capture lacks one later resource"));
    assert_eq!(transition_rows(&fixture), before);

    let actual = repository::start_instance(&fixture.db, &fixture.owner,
        &command, &instance_id, &version.definition_id, version.version,
        &variables, None, None, repository::ProcessPlanInput::Supplied(&plan),
        at_ms).unwrap();
    assert_eq!(actual.status, ProcessInstanceStatus::Incident);
    assert_eq!(actual.incidents.len(), 1);
    assert_eq!(actual.incidents[0].code, "CALL_ADMISSION_ERROR");
    let persisted = transition_rows(&fixture);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let replay = repository::get_instance(&reopened, &fixture.owner, &instance_id, None).unwrap();
    assert_eq!(replay, actual);
    assert_eq!(transition_rows(&fixture), persisted);
}

#[test]
fn local_body_call_uses_pinned_incoming_edge_and_child_start_after_reopen() {
    let fixture = Fixture::new();
    let mut model = super::model::starter_model();
    model.additional_processes.push(ProcessExecutableProcess {
        process_id: "Child_Process".into(),
        process_name: None,
        nodes: vec![
            ProcessNode { id: "Child_Start".into(), name: "Child start".into(), kind: ProcessNodeKind::Start, repeat: None, activity_io: None },
            ProcessNode { id: "Child_End".into(), name: "Child end".into(), kind: ProcessNodeKind::End, repeat: None, activity_io: None },
            ProcessNode { id: "Other_Start".into(), name: "Unselected start".into(), kind: ProcessNodeKind::Start, repeat: None, activity_io: None },
            ProcessNode { id: "Other_End".into(), name: "Unselected end".into(), kind: ProcessNodeKind::End, repeat: None, activity_io: None },
        ],
        sequence_flows: vec![
            edge("Child_Flow", "Child_Start", "Child_End"),
            edge("Other_Flow", "Other_Start", "Other_End"),
        ],
        variables: BTreeMap::new(),
        diagram: ProcessDiagram::default(),
        timer_timezone: None,
        work_calendar: None,
        calendar_pin: None,
        modeling: None,
    });
    model.nodes.insert(1, ProcessNode {
        id: "Call_Local".into(),
        name: "Call selected local body".into(),
        kind: ProcessNodeKind::CallActivity(ProcessCallActivity {
            target: ProcessCallTarget::LocalBody {
                called_element: ProcessCallableReference {
                    namespace_uri: "https://tentaflow.app/bpmn/1".into(),
                    process_id: "Child_Process".into(),
                },
            },
            input_mapping: BTreeMap::new(),
            output_mapping: BTreeMap::new(),
        }),
        repeat: None,
        activity_io: None,
    });
    model.sequence_flows = vec![
        ProcessSequenceFlow {
            call_start_node_id: Some("Child_Start".into()),
            ..edge("To_Local_Call", "Start_1", "Call_Local")
        },
        edge("Local_Return", "Call_Local", "End_1"),
    ];
    let mut ambiguous = model.clone();
    ambiguous.sequence_flows[0].call_start_node_id = None;
    let error = super::model::validate_model(&ambiguous).unwrap_err();
    assert!(format!("{error:#}").contains("requires an exact Start for each incoming flow"));
    let version = publish_model(&fixture, &model);
    let parent_id = Uuid::new_v4().to_string();
    let command = stamp("start selected local call");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let variables = serde_json::to_value(&version.model.variables).unwrap();
    let plan = runtime::plan_start(&version.model, &version.model.process_id, "Start_1",
        &parent_id, &fixture.owner, &version.definition_id, version.version,
        variables.clone(), runtime::StartCause::Manual, at_ms,
        runtime::test_support::manual_input(&command), None).unwrap();
    let parent = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &parent_id, &version.definition_id, version.version, &variables,
        Some(&version.model.process_id), Some("Start_1"),
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    assert_eq!(parent.status, ProcessInstanceStatus::Completed);
    let child_id = child_id(&fixture, &parent.instance_id);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    assert_eq!(snapshot.instance.process_id, "Child_Process");
    assert_eq!(snapshot.instance.start_node_id, "Child_Start");
    assert_eq!(snapshot.instance.version, version.version);
    assert_eq!(snapshot.calls[0].parent_arrival_edge_id, "To_Local_Call");
    assert_eq!(snapshot.calls[0].called_process_id, "Child_Process");
    assert_eq!(snapshot.calls[0].child_start_node_id, "Child_Start");
    let child_events = repository::list_events(&fixture.db, &fixture.owner, &child_id, 0, 100)
        .unwrap().0;
    assert!(child_events.iter().any(|event| event.kind == "end_reached"
        && event.node_id.as_deref() == Some("Child_End")));
    assert!(!child_events.iter().any(|event| event.node_id.as_deref()
        == Some("Other_End")));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let retained = repository::runtime_snapshot(&reopened, &fixture.owner, &child_id).unwrap();
    assert_eq!(retained.instance.process_id, snapshot.instance.process_id);
    assert_eq!(retained.instance.start_node_id, snapshot.instance.start_node_id);
    assert_eq!(serde_json::to_value(&retained.calls).unwrap(),
        serde_json::to_value(&snapshot.calls).unwrap());
}

pub(super) fn child_id(f: &Fixture, parent: &str) -> String {
    let snapshot = repository::runtime_snapshot(&f.db, &f.owner, parent).unwrap();
    snapshot
        .calls
        .iter()
        .find(|call| call.parent_instance_id == parent)
        .unwrap()
        .child_instance_id
        .clone()
}

fn complete(
    f: &Fixture,
    actor: &ProcessActor,
    id: &str,
    output: Value,
) -> ProcessTransitionOutcome {
    let snapshot = repository::runtime_snapshot(&f.db, actor, id).unwrap();
    let task = snapshot
        .user_tasks
        .iter()
        .find(|task| task.status == ProcessUserTaskStatus::Open)
        .unwrap();
    let at = chrono::Utc::now().timestamp_millis();
    let command = stamp("complete called work");
    let plan =
        runtime::plan_user_completion(&snapshot, &task.user_task_id, &output, None, at,
            human_input(&snapshot, &task.user_task_id, &command), None).unwrap();
    repository::complete_user_task(
        &f.db,
        actor,
        &command,
        id,
        &task.user_task_id,
        snapshot.instance.revision,
        &output,
        None,
        repository::ProcessPlanInput::Supplied(&plan),
        at,
    )
    .unwrap()
}

#[test]
fn returned_call_cannot_replay_its_factual_termination_as_a_standalone_entry() {
    let fixture = Fixture::new();
    let target = publish_model(&fixture, &user_model(None));
    let mut called_body = caller(&target, BTreeMap::new());
    called_body.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    let mut model = embedded_model(called_body, "Scope");
    model.nodes.iter_mut().find(|node| node.id == "RootEnd_Scope").unwrap().kind =
        ProcessNodeKind::UserTask { assignee_user_id: None, output_mapping: BTreeMap::new() };
    model.nodes.push(ProcessNode { id: "FinalEnd".into(), name: "Finish parent".into(),
        kind: ProcessNodeKind::End,
        repeat: None,   activity_io: None,});
    model.sequence_flows.push(edge("AfterScopeWork", "RootEnd_Scope", "FinalEnd"));
    let version = publish_model(&fixture, &model);
    let parent = messages::test_support::start_version(&fixture, &version);
    let child = child_id(&fixture, &parent.instance_id);
    let captured = std::rc::Rc::new(std::cell::RefCell::new(None));
    let capture = captured.clone();
    repository::CALL_PLAN_TEST_MUTATOR.with(|mutator| {
        *mutator.borrow_mut() = Some(Box::new(move |composite| {
            let returned = composite.call_steps.iter().find_map(|step| match step {
                repository::CallStep::Return { plan, .. } => Some((**plan).clone()),
                _ => None,
            }).expect("real completed child must produce one boxed return");
            *capture.borrow_mut() = Some(returned);
        }));
    });
    let outcome = complete(&fixture, &fixture.owner, &child, json!({"answer": 42}));
    repository::CALL_PLAN_TEST_MUTATOR.with(|mutator| *mutator.borrow_mut() = None);
    assert_eq!(outcome.instance.status, ProcessInstanceStatus::Completed);
    let returned_plan: repository::RuntimePlan = captured.borrow_mut().take().unwrap();
    assert!(returned_plan.termination_attempts.iter().any(|attempt| match attempt {
        repository::TerminationAttempt::Success(source) => matches!(
            &source.accepted_input, repository::AcceptedInputRef::CallReturn { .. }),
        repository::TerminationAttempt::ReturnFailure(_) => false,
    }));
    let before = transition_rows(&fixture);
    let current = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &parent.instance_id).unwrap();
    assert_eq!(current.instance.status, ProcessInstanceStatus::Waiting);
    assert_eq!(current.user_tasks.iter().filter(|task|
        task.status == ProcessUserTaskStatus::Open).count(), 1);
    let failure = repository::apply_transition(&fixture.db, &fixture.owner,
        &parent.instance_id, current.instance.revision, repository::ProcessPlanInput::Supplied(&returned_plan),
        chrono::Utc::now().timestamp_millis()).unwrap_err();
    assert!(format!("{failure:#}").contains("ordinary advancement cannot invent another terminating entry"));
    assert_eq!(transition_rows(&fixture), before);
    let finished = complete(&fixture, &fixture.owner, &parent.instance_id, json!({}));
    assert_eq!(finished.instance.status, ProcessInstanceStatus::Completed);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
        &parent.instance_id).unwrap();
    assert_eq!(persisted.instance.status, ProcessInstanceStatus::Completed);
    assert_eq!(persisted.instance.revision, finished.instance.revision);
}

fn events(f: &Fixture, id: &str) -> Vec<ProcessEvent> {
    repository::list_events(&f.db, &f.owner, id, 0, 200)
        .unwrap()
        .0
}

pub(super) const TRANSITION_TABLES: [&str; 18] = [
    "bpmn_instances",
    "bpmn_scopes",
    "bpmn_tokens",
    "bpmn_gateway_receipts",
    "bpmn_user_tasks",
    "bpmn_jobs",
    "bpmn_service_invocations",
    "bpmn_activity_io_witnesses",
    "bpmn_incidents",
    "bpmn_timers",
    "bpmn_event_subscriptions",
    "bpmn_event_races",
    "bpmn_messages",
    "bpmn_calls",
    "bpmn_events",
    "bpmn_commands",
    "bpmn_repetition_groups",
    "bpmn_repetition_occurrences",
];

pub(super) fn transition_rows(f: &Fixture) -> Vec<Vec<Vec<rusqlite::types::Value>>> {
    let conn = f.db.read().unwrap();
    TRANSITION_TABLES
        .iter()
        .map(|table| super::call_pin_tests::table_rows(&conn, table, "rowid"))
        .collect()
}

fn parallel_caller(target: &ProcessVersion) -> ProcessModel {
    let mut model = caller(
        target,
        BTreeMap::from([("received".into(), "outputs.answer".into())]),
    );
    for (id, kind) in [
        ("Split", ProcessNodeKind::ParallelGateway),
        ("Join", ProcessNodeKind::ParallelGateway),
        (
            "Sibling",
            ProcessNodeKind::UserTask {
                assignee_user_id: None,
                output_mapping: BTreeMap::from([("sibling".into(), "outputs.answer".into())]),
            },
        ),
    ] {
        model.nodes.push(ProcessNode {
            id: id.into(),
            name: id.into(),
            kind,
            repeat: None,
            activity_io: None,
        });
    }
    model.sequence_flows = vec![
        edge("StartSplit", "Start_1", "Split"),
        edge("SplitCall", "Split", "Call_1"),
        edge("SplitSibling", "Split", "Sibling"),
        edge("CallJoin", "Call_1", "Join"),
        edge("SiblingJoin", "Sibling", "Join"),
        edge("JoinEnd", "Join", "End_1"),
    ];
    model
}

fn error_end_model() -> ProcessModel {
    let mut model = super::model::starter_model();
    model
        .variables
        .insert("evidence".into(), json!({"original":42}));
    model.errors.push(ProcessErrorDeclaration {
        error_id: "Rejected".into(),
        name: "Evidence rejected".into(),
        error_code: "REJECTED".into(),
    });
    model.nodes[1].kind = ProcessNodeKind::ErrorEnd {
        error_ref: "Rejected".into(),
    };
    model
}

fn with_error_handler(mut model: ProcessModel, reference: Option<&str>) -> ProcessModel {
    if reference.is_some() {
        model.errors.push(ProcessErrorDeclaration {
            error_id: "Rejected".into(),
            name: "Evidence rejected".into(),
            error_code: "REJECTED".into(),
        });
    }
    model.nodes.push(ProcessNode {
        id: "Handle".into(),
        name: "Handle the actual child error".into(),
        kind: ProcessNodeKind::BoundaryError {
            attached_to_id: "Call_1".into(),
            error_ref: reference.map(str::to_owned),
            output_mapping: BTreeMap::from([
                ("source".into(), "activity_result".into()),
                ("evidence".into(), "outputs".into()),
            ]),
        },
        repeat: None,
        activity_io: None,
    });
    model
        .sequence_flows
        .push(edge("HandledEnd", "Handle", "End_1"));
    model
}

#[test]
fn fast_nested_calls_use_distinct_roots_and_replay_without_another_child() {
    let f = Fixture::new();
    let leaf = publish_model(&f, &super::model::starter_model());
    let middle = publish_model(&f, &caller(&leaf, BTreeMap::new()));
    let outer = publish_model(&f, &caller(&middle, BTreeMap::new()));
    let id = Uuid::new_v4().to_string();
    let at = chrono::Utc::now().timestamp_millis();
    let command = stamp("start exact call tree");
    let plan = runtime::plan_start(
        &outer.model, &outer.model.process_id, crate::processes::runtime::test_support::ordinary_start_id(&outer.model),
        &id,
        &f.owner,
        &outer.definition_id,
        outer.version,
        json!({}),
        runtime::StartCause::Manual,
        at,
        manual_input(&command),
    None)
    .unwrap();
    let first = repository::start_instance(
        &f.db,
        &f.owner,
        &command,
        &id,
        &outer.definition_id,
        outer.version,
        &json!({}), None, None,
        repository::ProcessPlanInput::Supplied(&plan),
        at,
    )
    .unwrap();
    assert_eq!(first.status, ProcessInstanceStatus::Completed);
    let a = repository::runtime_snapshot(&f.db, &f.owner, &id).unwrap();
    let b_id = a
        .calls
        .iter()
        .find(|call| call.parent_instance_id == id)
        .unwrap()
        .child_instance_id
        .clone();
    let b = repository::runtime_snapshot(&f.db, &f.owner, &b_id).unwrap();
    let c_id = b
        .calls
        .iter()
        .find(|call| call.parent_instance_id == b_id)
        .unwrap()
        .child_instance_id
        .clone();
    assert_ne!(id, b_id);
    assert_ne!(b_id, c_id);
    assert_ne!(id, c_id);
    for current in [&id, &b_id, &c_id] {
        let state = repository::runtime_snapshot(&f.db, &f.owner, current).unwrap();
        assert_eq!(state.instance.status, ProcessInstanceStatus::Completed);
        assert_eq!(state.scopes.len(), 1);
        assert_eq!(state.scopes[0].scope_id, *current);
        assert!(state.tokens.is_empty());
        assert!(state
            .calls
            .iter()
            .all(|call| call.status == ProcessCallStatus::Returned));
    }
    let before_replay = transition_rows(&f);
    let replay = repository::start_instance(
        &f.db,
        &f.owner,
        &command,
        &Uuid::new_v4().to_string(),
        &outer.definition_id,
        outer.version,
        &json!({}), None, None,
        repository::ProcessPlanInput::Supplied(&plan),
        at,
    )
    .unwrap();
    assert_eq!(replay, first);
    assert_eq!(transition_rows(&f), before_replay);
    let conn = f.db.read().unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM bpmn_instances", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        3
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM bpmn_calls", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM bpmn_scopes WHERE parent_scope_id IS NULL AND scope_id!=instance_id",[],|r|r.get::<_,i64>(0)).unwrap(),0);
}

#[test]
fn child_assignee_reopen_returns_without_parent_read_grant_or_private_relation() {
    let f = Fixture::new();
    let leaf = publish_model(&f, &user_model(Some(&f.participant.user_id)));
    let outer = publish_model(
        &f,
        &caller(
            &leaf,
            BTreeMap::from([("received".into(), "outputs.answer".into())]),
        ),
    );
    let parent = messages::test_support::start_version(&f, &outer);
    let child = child_id(&f, &parent.instance_id);
    let private = repository::get_instance(&f.db, &f.participant, &child, None).unwrap();
    assert!(matches!(
        &private.calls[0],
        ProcessCallSummary::Incoming { parent: None }
    ));
    assert!(repository::get_instance(&f.db, &f.participant, &parent.instance_id, None).is_err());
    let task = private.user_tasks[0].user_task_id.clone();
    let path = f.directory.path().join("processes.db");
    let Fixture {
        directory,
        db,
        router,
        owner,
        participant,
    } = f;
    drop(router);
    drop(db);
    let reopened = crate::db::init(&path).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &participant, &child).unwrap();
    let at = chrono::Utc::now().timestamp_millis();
    let output = json!({"answer":"verified"});
    let command = stamp("return after restart");
    let plan = runtime::plan_user_completion(&snapshot, &task, &output, None, at,
        human_input(&snapshot, &task, &command), None).unwrap();
    let result = repository::complete_user_task(
        &reopened,
        &participant,
        &command,
        &child,
        &task,
        snapshot.instance.revision,
        &output,
        None,
        repository::ProcessPlanInput::Supplied(&plan),
        at,
    )
    .unwrap();
    assert_eq!(result.instance.status, ProcessInstanceStatus::Completed);
    let returned = repository::get_instance(&reopened, &owner, &parent.instance_id, None).unwrap();
    assert_eq!(returned.status, ProcessInstanceStatus::Completed);
    assert_eq!(returned.variables["received"], "verified");
    let duplicate = repository::complete_user_task(
        &reopened,
        &participant,
        &command,
        &child,
        &task,
        snapshot.instance.revision,
        &output,
        None,
        repository::ProcessPlanInput::Supplied(&plan),
        at,
    )
    .unwrap();
    assert_eq!(duplicate.instance, result.instance);
    assert!(duplicate.cancelled_claims.is_empty());
    assert!(matches!(
        &duplicate.instance.calls[0],
        ProcessCallSummary::Incoming { parent: None }
    ));
    assert!(repository::get_instance(&reopened, &participant, &parent.instance_id, None).is_err());
    drop(reopened);
    drop(directory);
}

#[test]
fn return_mapping_failure_preserves_completed_child_and_exact_parent_wait() {
    let f = Fixture::new();
    let leaf = publish_model(&f, &user_model(None));
    let outer = publish_model(
        &f,
        &caller(
            &leaf,
            BTreeMap::from([("bad".into(), "outputs.absent.value".into())]),
        ),
    );
    let parent = messages::test_support::start_version(&f, &outer);
    let child = child_id(&f, &parent.instance_id);
    let before = repository::runtime_snapshot(&f.db, &f.owner, &parent.instance_id).unwrap();
    let result = complete(&f, &f.owner, &child, json!({"answer":42}));
    assert_eq!(result.instance.status, ProcessInstanceStatus::Completed);
    assert_eq!(result.instance.variables["answer"], 42);
    let after = repository::runtime_snapshot(&f.db, &f.owner, &parent.instance_id).unwrap();
    assert_eq!(after.instance.status, ProcessInstanceStatus::Incident);
    assert_eq!(after.instance.variables, before.instance.variables);
    assert_eq!(
        serde_json::to_value(&after.tokens).unwrap(),
        serde_json::to_value(&before.tokens).unwrap()
    );
    assert_eq!(after.calls[0].status, ProcessCallStatus::ReturnIncident);
    assert!(after
        .incidents
        .iter()
        .any(|i| i.code == "CALL_RETURN_ERROR"));
    assert!(events(&f, &parent.instance_id)
        .iter()
        .all(|event| event.kind != "call_returned"));
}

#[test]
fn inactive_initiator_keeps_factual_assignee_completion_and_waiting_parent_incident() {
    let f = Fixture::new();
    let leaf = publish_model(&f, &user_model(Some(&f.participant.user_id)));
    let outer = publish_model(
        &f,
        &caller(
            &leaf,
            BTreeMap::from([("received".into(), "outputs.answer".into())]),
        ),
    );
    let parent = messages::test_support::start_version(&f, &outer);
    let child = child_id(&f, &parent.instance_id);
    f.db.write()
        .unwrap()
        .execute(
            "UPDATE user_accounts SET is_active=0 WHERE id=?1",
            [&f.owner.user_id],
        )
        .unwrap();
    let completed = complete(&f, &f.participant, &child, json!({"answer":"factual"}));
    assert_eq!(completed.instance.status, ProcessInstanceStatus::Completed);
    let conn = f.db.read().unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT status FROM bpmn_instances WHERE instance_id=?1",
            [&parent.instance_id],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "incident"
    );
    assert_eq!(
        conn.query_row(
            "SELECT status FROM bpmn_calls WHERE child_instance_id=?1",
            [&child],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "return_incident"
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM bpmn_tokens WHERE instance_id=?1 AND status='waiting'",
            [&parent.instance_id],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM bpmn_incidents WHERE instance_id=?1 AND code='CALL_RETURN_ERROR' AND resolved_at_ms IS NULL",[&parent.instance_id],|r|r.get::<_,i64>(0)).unwrap(),1);
    assert!(!conn
        .query_row(
            "SELECT variables_json FROM bpmn_instances WHERE instance_id=?1",
            [&parent.instance_id],
            |r| r.get::<_, String>(0)
        )
        .unwrap()
        .contains("factual"));
}

#[test]
fn call_timer_both_commit_orders_disarm_or_cancel_only_actual_child_claim() {
    for completion_first in [true, false] {
        let f = Fixture::new();
        let leaf = publish_model(&f, &user_model(None));
        let outer = publish_model(
            &f,
            &with_boundaries(
                caller(&leaf, BTreeMap::new()),
                "Call_1",
                &[("Limit", true, 1)],
            ),
        );
        let parent = messages::test_support::start_version(&f, &outer);
        let child = child_id(&f, &parent.instance_id);
        let due = repository::runtime_snapshot(&f.db, &f.owner, &parent.instance_id)
            .unwrap()
            .timers[0]
            .due_at_ms
            .unwrap();
        if completion_first {
            complete(&f, &f.owner, &child, json!({"answer":1}));
        }
        let drained = timers::drain_due(&f.db, due);
        assert!(drained.completion.is_ok());
        assert_eq!(drained.fired, u32::from(!completion_first));
        let source = repository::get_instance(&f.db, &f.owner, &child, None).unwrap();
        assert_eq!(
            source.status,
            if completion_first {
                ProcessInstanceStatus::Completed
            } else {
                ProcessInstanceStatus::Cancelled
            }
        );
        let final_parent =
            repository::get_instance(&f.db, &f.owner, &parent.instance_id, None).unwrap();
        assert_eq!(final_parent.status, ProcessInstanceStatus::Completed);
        assert_eq!(
            final_parent.timers[0].status,
            if completion_first {
                ProcessTimerStatus::Cancelled
            } else {
                ProcessTimerStatus::Fired
            }
        );
    }
}

#[tokio::test]
async fn parent_boundary_commit_before_child_registration_fences_real_executor_and_late_result() {
    let f = Fixture::new();
    let flow_id = flow(&f.db, &f.owner, &graph("must not execute", None));
    let leaf = publish_model(
        &f,
        &service_model(
            &flow_id,
            ActivityVerification::Condition {
                expression: "true".into(),
            },
        ),
    );
    let outer = publish_model(
        &f,
        &with_boundaries(
            caller(&leaf, BTreeMap::new()),
            "Call_1",
            &[("Limit", true, 1)],
        ),
    );
    let parent = messages::test_support::start_version(&f, &outer);
    let child = child_id(&f, &parent.instance_id);
    let claim = repository::claim_job(
        &f.db,
        "call-registration",
        chrono::Utc::now().timestamp_millis(),
    )
    .unwrap()
    .unwrap();
    let due = repository::runtime_snapshot(&f.db, &f.owner, &parent.instance_id)
        .unwrap()
        .timers[0]
        .due_at_ms
        .unwrap();
    let drained = timers::drain_due(&f.db, due);
    assert!(drained.completion.is_ok());
    assert_eq!(drained.cancelled_claims.len(), 1);
    let tuple = &drained.cancelled_claims[0];
    assert_eq!(tuple.job_id, claim.job.job_id);
    assert_eq!(tuple.attempt, claim.job.attempt);
    assert_eq!(tuple.fence, claim.job.fence);
    assert_eq!(tuple.worker_id, "call-registration");
    super::jobs::execute_claimed(
        &f.db,
        f.dispatcher(),
        "call-registration",
        claim.clone(),
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(
        crate::db::repository::list_flow_executions_for_flow(&f.db, &flow_id, 10)
            .unwrap()
            .is_empty()
    );
    let actual = repository::runtime_snapshot(&f.db, &f.owner, &child).unwrap();
    assert_eq!(actual.jobs[0].fence, claim.job.fence + 1);
    assert_eq!(actual.jobs[0].status, "cancelled");
    assert!(actual.jobs[0].result.is_none());
    assert!(!repository::renew_job_lease(
        &f.db,
        &claim.job.job_id,
        claim.job.attempt,
        claim.job.fence,
        "call-registration",
        chrono::Utc::now().timestamp_millis()
    )
    .unwrap());
}

#[test]
fn direct_child_cancel_creates_parent_incident_and_parent_cancel_preserves_child_fact() {
    let f = Fixture::new();
    let leaf = publish_model(&f, &user_model(None));
    let outer = publish_model(&f, &caller(&leaf, BTreeMap::new()));
    let parent = messages::test_support::start_version(&f, &outer);
    let child = child_id(&f, &parent.instance_id);
    let state = repository::get_instance(&f.db, &f.owner, &child, None).unwrap();
    repository::cancel_instance(
        &f.db,
        &f.owner,
        &stamp("cancel child"),
        &child,
        state.revision,
    )
    .unwrap();
    let after = repository::get_instance(&f.db, &f.owner, &parent.instance_id, None).unwrap();
    assert_eq!(after.status, ProcessInstanceStatus::Incident);
    assert_eq!(after.active_node_ids, vec!["Call_1"]);
    assert_eq!(after.incidents[0].code, "CALL_CHILD_CANCELLED");
    let child_before = repository::get_instance(&f.db, &f.owner, &child, None).unwrap();
    let history_before = events(&f, &child);
    repository::cancel_instance(
        &f.db,
        &f.owner,
        &stamp("cancel parent"),
        &parent.instance_id,
        after.revision,
    )
    .unwrap();
    let child_after = repository::get_instance(&f.db, &f.owner, &child, None).unwrap();
    assert_eq!(child_after.revision, child_before.revision);
    assert_eq!(child_after.updated_at_ms, child_before.updated_at_ms);
    assert_eq!(child_after.variables, child_before.variables);
    assert_eq!(child_after.user_tasks, child_before.user_tasks);
    assert_eq!(events(&f, &child), history_before);
    assert_eq!(
        repository::get_instance(&f.db, &f.owner, &child, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Cancelled
    );
}

#[test]
fn uncaught_error_end_is_genuine_terminal_error_and_direct_parent_waits() {
    let f = Fixture::new();
    let leaf = publish_model(&f, &error_end_model());
    let outer = publish_model(&f, &caller(&leaf, BTreeMap::new()));
    let parent = messages::test_support::start_version(&f, &outer);
    let child = child_id(&f, &parent.instance_id);
    let source = repository::get_instance(&f.db, &f.owner, &child, None).unwrap();
    assert_eq!(source.status, ProcessInstanceStatus::Error);
    assert!(!source.can_cancel);
    assert!(!source.can_retry);
    let fact = source.terminal_error.as_ref().unwrap();
    let actual = events(&f, &child);
    let event = actual
        .iter()
        .find(|event| event.event_id == fact.source_event_id)
        .unwrap();
    assert_eq!(event.kind, "error_end_reached");
    assert_eq!(event.data["source_instance_id"], child);
    assert_eq!(event.data["outputs"]["evidence"]["original"], 42);
    let after = repository::get_instance(&f.db, &f.owner, &parent.instance_id, None).unwrap();
    assert_eq!(after.status, ProcessInstanceStatus::Incident);
    assert_eq!(after.incidents[0].code, "CALL_CHILD_ERROR");
    assert_eq!(after.active_node_ids, vec!["Call_1"]);
    assert!(after.terminal_error.is_none());
}

#[test]
fn multi_call_error_end_keeps_source_error_and_cancels_intermediate_without_foreign_terminal_fact()
{
    let f = Fixture::new();
    let c = publish_model(&f, &error_end_model());
    let b = publish_model(&f, &caller(&c, BTreeMap::new()));
    let a = publish_model(
        &f,
        &with_error_handler(caller(&b, BTreeMap::new()), Some("Rejected")),
    );
    let parent = messages::test_support::start_version(&f, &a);
    let middle = child_id(&f, &parent.instance_id);
    let source = child_id(&f, &middle);
    assert_eq!(parent.status, ProcessInstanceStatus::Completed);
    assert_eq!(parent.variables["source"]["source_instance_id"], source);
    assert_eq!(parent.variables["source"]["error_code"], "REJECTED");
    assert_eq!(parent.variables["evidence"]["evidence"]["original"], 42);
    let b_state = repository::get_instance(&f.db, &f.owner, &middle, None).unwrap();
    let c_state = repository::get_instance(&f.db, &f.owner, &source, None).unwrap();
    assert_eq!(b_state.status, ProcessInstanceStatus::Cancelled);
    assert!(b_state.terminal_error.is_none());
    assert_eq!(c_state.status, ProcessInstanceStatus::Error);
    let terminal = c_state.terminal_error.unwrap();
    assert_eq!(terminal.source_scope_id, source);
    let b_snapshot = repository::runtime_snapshot(&f.db, &f.owner, &middle).unwrap();
    assert!(b_snapshot
        .calls
        .iter()
        .any(|call| call.child_instance_id == source && call.status == ProcessCallStatus::Error));
    let propagated = events(&f, &middle);
    assert!(propagated
        .iter()
        .any(|event| event.kind == "call_error_propagated"
            && event.data["source_instance_id"] == source
            && event.data["source_event_id"] == terminal.source_event_id));
}

#[test]
fn nearer_call_catch_all_wins_over_outer_exact_error_handler() {
    let f = Fixture::new();
    let c = publish_model(&f, &error_end_model());
    let b = publish_model(&f, &with_error_handler(caller(&c, BTreeMap::new()), None));
    let a = publish_model(
        &f,
        &with_error_handler(
            caller(
                &b,
                BTreeMap::from([("inner".into(), "outputs.source".into())]),
            ),
            Some("Rejected"),
        ),
    );
    let parent = messages::test_support::start_version(&f, &a);
    assert_eq!(parent.status, ProcessInstanceStatus::Completed);
    assert_eq!(parent.variables["inner"]["error_code"], "REJECTED");
    assert!(parent.variables.get("source").is_none());
    assert!(events(&f, &parent.instance_id)
        .iter()
        .all(|event| event.kind != "business_error_caught"));
}

#[tokio::test]
async fn accepted_real_contract_error_crosses_calls_with_original_outputs_and_result_origin() {
    let f = Fixture::new();
    let result = ActivityResult {
        outcome: ActivityOutcome::Error,
        code: Some("REJECTED".into()),
        summary: "Actual business rejection".into(),
        outputs: json!({"actual":42}),
        evidence: vec!["Original evidence".into()],
    };
    let graph=json!({"nodes":[{"id":"trigger","type":"trigger","config":{"output_mapping":{"actual_result":serde_json::to_string(&result).unwrap()}}},{"id":"output","type":"output","config":{}}],"edges":[{"from":"trigger","to":"output","from_port":"text","to_port":"text"}],"variables":[{"name":"actual_result","type":"json"}]}).to_string();
    let flow_id = flow(&f.db, &f.owner, &graph);
    let mut service = service_model(
        &flow_id,
        ActivityVerification::Condition {
            expression: "true".into(),
        },
    );
    if let ProcessNodeKind::ServiceTask {
        result_expression, ..
    } = &mut service.nodes[1].kind
    {
        *result_expression = Some("outputs.variables.actual_result".into());
    }
    let c = publish_model(&f, &service);
    let b = publish_model(&f, &caller(&c, BTreeMap::new()));
    let a = publish_model(
        &f,
        &with_error_handler(caller(&b, BTreeMap::new()), Some("Rejected")),
    );
    let parent = messages::test_support::start_version(&f, &a);
    let middle = child_id(&f, &parent.instance_id);
    let source = child_id(&f, &middle);
    let claim = repository::claim_job(
        &f.db,
        "contract-call",
        chrono::Utc::now().timestamp_millis(),
    )
    .unwrap()
    .unwrap();
    super::jobs::execute_claimed(
        &f.db,
        f.dispatcher(),
        "contract-call",
        claim,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    let after = repository::get_instance(&f.db, &f.owner, &parent.instance_id, None).unwrap();
    assert_eq!(after.status, ProcessInstanceStatus::Completed);
    assert_eq!(
        after.variables["source"],
        serde_json::to_value(&result).unwrap()
    );
    assert_eq!(after.variables["evidence"], result.outputs);
    let source_state = repository::runtime_snapshot(&f.db, &f.owner, &source).unwrap();
    assert_eq!(source_state.jobs[0].result.as_ref(), Some(&result));
    assert!(source_state.instance.terminal_error.is_none());
    assert_eq!(
        source_state.instance.status,
        ProcessInstanceStatus::Cancelled
    );
    let factual = events(&f, &source);
    let retained = factual
        .iter()
        .find(|event| event.kind == "service_result")
        .unwrap();
    assert_eq!(retained.data["result_origin"], "contract");
    assert_eq!(retained.data["outputs"], result.outputs);
    assert_eq!(retained.data["evidence"], json!(result.evidence));
    assert_eq!(
        crate::db::repository::list_flow_executions_for_flow(&f.db, &flow_id, 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn forged_error_end_missing_node_or_wrong_source_rolls_back_every_transition_fact() {
    let f = Fixture::new();
    let mut model = user_model(None);
    let error_model = error_end_model();
    model.errors = error_model.errors;
    model
        .nodes
        .iter_mut()
        .find(|node| node.id == "End_1")
        .unwrap()
        .kind = ProcessNodeKind::ErrorEnd {
        error_ref: "Rejected".into(),
    };
    let target = publish_model(&f, &model);
    let parent_version = publish_model(
        &f,
        &with_error_handler(caller(&target, BTreeMap::new()), None),
    );
    let parent = messages::test_support::start_version(&f, &parent_version);
    let child = child_id(&f, &parent.instance_id);
    let snapshot = repository::runtime_snapshot(&f.db, &f.owner, &child).unwrap();
    let task = &snapshot.user_tasks[0];
    let at = chrono::Utc::now().timestamp_millis();
    let outputs = json!({"answer":42});
    let command = stamp("forged error source");
    let plan =
        runtime::plan_user_completion(&snapshot, &task.user_task_id, &outputs, None, at,
            human_input(&snapshot, &task.user_task_id, &command), None).unwrap();
    for mutation in 0..4 {
        let mut forged = plan.clone();
        let event = forged
            .events
            .iter_mut()
            .find(|event| event.kind == "error_end_reached")
            .unwrap();
        match mutation {
            0 => event.node_id = None,
            1 => event.data["source_instance_id"] = json!(parent.instance_id),
            2 => event.data["source_token_id"] = json!(task.token_id),
            3 => event.data["source_event_id"] = json!(Uuid::new_v4().to_string()),
            _ => unreachable!(),
        }
        let before = transition_rows(&f);
        assert!(repository::complete_user_task(
            &f.db,
            &f.owner,
            &command,
            &child,
            &task.user_task_id,
            snapshot.instance.revision,
            &outputs,
            None,
            repository::ProcessPlanInput::Supplied(&forged),
            at,
        )
        .is_err());
        assert_eq!(
            transition_rows(&f),
            before,
            "forged fact must not complete/cancel any local or called control"
        );
    }
    let committed = repository::complete_user_task(
        &f.db,
        &f.owner,
        &command,
        &child,
        &task.user_task_id,
        snapshot.instance.revision,
        &outputs,
        None,
        repository::ProcessPlanInput::Supplied(&plan),
        at,
    )
    .unwrap();
    assert_eq!(committed.instance.status, ProcessInstanceStatus::Error);
    let returned = repository::get_instance(&f.db, &f.owner, &parent.instance_id, None).unwrap();
    assert_eq!(returned.status, ProcessInstanceStatus::Completed);
    assert_eq!(returned.variables["source"]["source_instance_id"], child);
    assert_eq!(
        events(&f, &child)
            .iter()
            .filter(|event| event.kind == "error_end_reached")
            .count(),
        1
    );
}

#[test]
fn storage_failure_after_child_start_rolls_back_parent_child_link_and_command() {
    let f = Fixture::new();
    let target = publish_model(&f, &super::model::starter_model());
    let version = publish_model(&f, &caller(&target, BTreeMap::new()));
    let id = Uuid::new_v4().to_string();
    let at = chrono::Utc::now().timestamp_millis();
    let command = stamp("atomic fast child");
    let plan = runtime::plan_start(
        &version.model, &version.model.process_id, crate::processes::runtime::test_support::ordinary_start_id(&version.model),
        &id,
        &f.owner,
        &version.definition_id,
        version.version,
        json!({}),
        runtime::StartCause::Manual,
        at,
        manual_input(&command),
    None)
    .unwrap();
    f.db.write().unwrap().execute_batch("CREATE TRIGGER fail_call_entered BEFORE INSERT ON bpmn_events WHEN NEW.kind='call_entered' BEGIN SELECT RAISE(ABORT,'controlled call storage failure'); END;").unwrap();
    let before = transition_rows(&f);
    let failure = repository::start_instance(
        &f.db,
        &f.owner,
        &command,
        &id,
        &version.definition_id,
        version.version,
        &json!({}), None, None,
        repository::ProcessPlanInput::Supplied(&plan),
        at,
    )
    .unwrap_err();
    assert!(format!("{failure:#}").contains("controlled call storage failure"));
    assert_eq!(transition_rows(&f), before);
    f.db.write()
        .unwrap()
        .execute_batch("DROP TRIGGER fail_call_entered")
        .unwrap();
    let actual = repository::start_instance(
        &f.db,
        &f.owner,
        &command,
        &id,
        &version.definition_id,
        version.version,
        &json!({}), None, None,
        repository::ProcessPlanInput::Supplied(&plan),
        at,
    )
    .unwrap();
    assert_eq!(actual.status, ProcessInstanceStatus::Completed);
    let conn = f.db.read().unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM bpmn_instances", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM bpmn_calls", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM bpmn_commands WHERE command_id=?1",
            [&command.command_id],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
}

#[test]
fn parent_sibling_revision_race_replans_return_without_incident_and_interruption_rejects() {
    for interruption in [false, true] {
        let f = Fixture::new();
        let target = publish_model(&f, &user_model(None));
        let model = if interruption {
            with_boundaries(
                caller(&target, BTreeMap::new()),
                "Call_1",
                &[("Limit", true, 1)],
            )
        } else {
            parallel_caller(&target)
        };
        let version = publish_model(&f, &model);
        let parent = messages::test_support::start_version(&f, &version);
        let child = child_id(&f, &parent.instance_id);
        let snapshot = repository::runtime_snapshot(&f.db, &f.owner, &child).unwrap();
        let task = snapshot.user_tasks[0].user_task_id.clone();
        let at = chrono::Utc::now().timestamp_millis();
        let outputs = json!({"answer":42});
    let command = stamp("complete after parent changes");
    let plan = runtime::plan_user_completion(&snapshot, &task, &outputs, None, at,
        human_input(&snapshot, &task, &command), None).unwrap();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
        let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(0);
        let actual = std::thread::scope(|scope| {
            let resume_tx = resume_tx;
            let f = &f;
            let id = &child;
            let task = &task;
            let plan = &plan;
            let command = &command;
            let outputs = &outputs;
            let pending = scope.spawn(move || {
                repository::CALL_TRANSITION_PREFLIGHT
                    .with(|gate| *gate.borrow_mut() = Some((ready_tx, resume_rx)));
                let result = repository::complete_user_task(
                    &f.db,
                    &f.owner,
                    command,
                    id,
                    task,
                    snapshot.instance.revision,
                    outputs,
                    None,
                    repository::ProcessPlanInput::Supplied(plan),
                    at,
                );
                repository::CALL_TRANSITION_PREFLIGHT.with(|gate| *gate.borrow_mut() = None);
                result
            });
            ready_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .unwrap();
            if interruption {
                let due = repository::runtime_snapshot(&f.db, &f.owner, &parent.instance_id)
                    .unwrap()
                    .timers[0]
                    .due_at_ms
                    .unwrap();
                let drained = timers::drain_due(&f.db, due);
                assert!(drained.completion.is_ok());
                assert_eq!(drained.fired, 1);
            } else {
                complete(
                    &f,
                    &f.owner,
                    &parent.instance_id,
                    json!({"answer":"sibling fact"}),
                );
            }
            resume_tx.send(()).unwrap();
            pending.join().unwrap()
        });
        let parent_after =
            repository::get_instance(&f.db, &f.owner, &parent.instance_id, None).unwrap();
        let child_after = repository::get_instance(&f.db, &f.owner, &child, None).unwrap();
        if interruption {
            assert!(actual.is_err());
            assert_eq!(child_after.status, ProcessInstanceStatus::Cancelled);
            assert_eq!(
                child_after.user_tasks[0].status,
                ProcessUserTaskStatus::Cancelled
            );
            assert!(events(&f, &child)
                .iter()
                .all(|event| event.kind != "user_task_completed"));
        } else {
            assert_eq!(
                actual.unwrap().instance.status,
                ProcessInstanceStatus::Completed
            );
            assert_eq!(parent_after.status, ProcessInstanceStatus::Completed);
            assert_eq!(parent_after.variables["sibling"], "sibling fact");
            assert_eq!(parent_after.variables["received"], 42);
            assert!(parent_after.incidents.is_empty());
            assert_eq!(
                events(&f, &parent.instance_id)
                    .iter()
                    .filter(|event| event.kind == "call_returned")
                    .count(),
                1
            );
            assert_eq!(
                events(&f, &child)
                    .iter()
                    .filter(|event| event.kind == "user_task_completed")
                    .count(),
                1
            );
        }
        assert_eq!(parent_after.status, ProcessInstanceStatus::Completed);
    }
}

#[test]
fn parent_and_child_gateway_receipts_are_isolated_in_both_completion_orders() {
    for inclusive in [false, true] {
        for child_first in [false, true] {
            let f = Fixture::new();
            let mut inner = user_model(None);
            inner
                .nodes
                .iter_mut()
                .find(|node| node.id == "Work")
                .unwrap()
                .kind = ProcessNodeKind::UserTask {
                assignee_user_id: None,
                output_mapping: BTreeMap::new(),
            };
            inner.nodes.extend([
                ProcessNode {
                    id: "ChildSplit".into(),
                    name: "Split child".into(),
                    kind: if inclusive { ProcessNodeKind::InclusiveGateway { default_flow_id: None } }
                        else { ProcessNodeKind::ParallelGateway },
                    repeat: None,
                    activity_io: None,
                },
                ProcessNode {
                    id: "ChildJoin".into(),
                    name: "Join child".into(),
                    kind: if inclusive { ProcessNodeKind::InclusiveGateway { default_flow_id: None } }
                        else { ProcessNodeKind::ParallelGateway },
                    repeat: None,
                    activity_io: None,
                },
                ProcessNode {
                    id: "OtherWork".into(),
                    name: "Other child work".into(),
                    kind: ProcessNodeKind::UserTask {
                        assignee_user_id: None,
                        output_mapping: BTreeMap::from([("answer".into(), "outputs.answer".into())]),
                    },
                    repeat: None,
                    activity_io: None,
                },
            ]);
            inner.sequence_flows = vec![
                edge("ChildStartSplit", "Start_1", "ChildSplit"),
                edge("ChildSplitLeft", "ChildSplit", "Work"),
                edge("ChildSplitRight", "ChildSplit", "OtherWork"),
                edge("ChildLeftJoin", "Work", "ChildJoin"),
                edge("ChildRightJoin", "OtherWork", "ChildJoin"),
                edge("ChildJoinEnd", "ChildJoin", "End_1"),
            ];
            if inclusive {
                inner.sequence_flows[1].condition = Some("true".into());
                inner.sequence_flows[2].condition = Some("true".into());
            }
            let leaf = publish_model(&f, &inner);
            let mut outer_model = parallel_caller(&leaf);
            if inclusive {
                for node in &mut outer_model.nodes {
                    if node.id == "Split" || node.id == "Join" {
                        node.kind = ProcessNodeKind::InclusiveGateway { default_flow_id: None };
                    }
                }
                outer_model.sequence_flows[1].condition = Some("true".into());
                outer_model.sequence_flows[2].condition = Some("true".into());
            }
            let outer = publish_model(&f, &outer_model);
            let parent = messages::test_support::start_version(&f, &outer);
            let child = child_id(&f, &parent.instance_id);
            complete(&f, &f.owner, &child, json!({"answer":42}));
            let halfway = repository::runtime_snapshot(&f.db, &f.owner, &child).unwrap();
            assert_eq!(halfway.receipts.len(), 1);
            assert_eq!(halfway.receipts[0].scope_id, child);
            assert_eq!(halfway.receipts[0].join_node_id, "ChildJoin");
            assert!(
                repository::runtime_snapshot(&f.db, &f.owner, &parent.instance_id)
                    .unwrap()
                    .receipts
                    .is_empty()
            );
            let order = if child_first {
                [&child, &parent.instance_id]
            } else {
                [&parent.instance_id, &child]
            };
            complete(&f, &f.owner, order[0], json!({"answer":42}));
            let waiting_parent =
                repository::runtime_snapshot(&f.db, &f.owner, &parent.instance_id).unwrap();
            assert_eq!(
                waiting_parent.instance.status,
                ProcessInstanceStatus::Waiting
            );
            assert_eq!(waiting_parent.receipts.len(), 1);
            assert_eq!(waiting_parent.receipts[0].scope_id, parent.instance_id);
            assert_eq!(waiting_parent.receipts[0].join_node_id, "Join");
            complete(&f, &f.owner, order[1], json!({"answer":42}));
            for id in [&child, &parent.instance_id] {
                let final_state = repository::runtime_snapshot(&f.db, &f.owner, id).unwrap();
                assert_eq!(
                    final_state.instance.status,
                    ProcessInstanceStatus::Completed
                );
                assert!(final_state.receipts.is_empty());
                assert!(final_state.tokens.is_empty());
                assert_eq!(
                    events(&f, id)
                        .iter()
                        .filter(|event| event.kind == if inclusive { "inclusive_joined" } else { "parallel_joined" })
                        .count(),
                    1
                );
            }
            assert_eq!(
                events(&f, &parent.instance_id)
                    .iter()
                    .filter(|event| event.kind == "call_returned")
                    .count(),
                1
            );
        }
    }
}

#[test]
fn completed_called_descendant_outbox_survives_normal_return_and_parent_interrupt(
) {
    use messages::test_support as support;
    for interrupt in [false, true] {
        let f = Fixture::new();
        let receiver_version = publish_model(&f, &support::receiving_model(false, true));
        let receiver = support::start_version(&f, &receiver_version);
        let fast_receiver = publish_model(&f, &support::receiving_model(true, false));
        let mut source = super::model::starter_model();
        source.messages.push(ProcessMessageDeclaration {
            message_id: "Evidence".into(),
            name: "EvidenceReady".into(),
        });
        source.nodes.insert(
            1,
            ProcessNode {
                id: "ThrowPending".into(),
                name: "Send evidence before the receiver is ready".into(),
                kind: ProcessNodeKind::MessageThrow {
                    message_ref: "Evidence".into(),
                    target: ProcessMessageTargetSpec::Catch {
                        definition_id: receiver_version.definition_id.clone(),
                        instance_id_expression: Some(
                            serde_json::to_string(&receiver.instance_id).unwrap(),
                        ),
                        subscription_id_expression: None,
                    },
                    correlation_expression: "'case-1'".into(),
                    payload_expression: "{'real': 42}".into(),
                    ttl_seconds: 120,
                },
                repeat: None,
                activity_io: None,
            },
        );
        source.nodes.insert(
            2,
            ProcessNode {
                id: "ThrowDelivered".into(),
                name: "Send independently delivered evidence".into(),
                kind: ProcessNodeKind::MessageThrow {
                    message_ref: "Evidence".into(),
                    target: ProcessMessageTargetSpec::Start {
                        definition_id: fast_receiver.definition_id.clone(),
                        process_id: None,
                        start_node_id: None,
                    },
                    correlation_expression: "'case-1'".into(),
                    payload_expression: "{'delivered': 42}".into(),
                    ttl_seconds: 120,
                },
                repeat: None,
                activity_io: None,
            },
        );
        source.sequence_flows = vec![
            edge("StartPending", "Start_1", "ThrowPending"),
            edge("PendingDelivered", "ThrowPending", "ThrowDelivered"),
            edge("DeliveredEnd", "ThrowDelivered", "End_1"),
        ];
        let c = publish_model(&f, &source);
        let mut middle = caller(&c, BTreeMap::new());
        middle.nodes.insert(
            2,
            ProcessNode {
                id: "MiddleWait".into(),
                name: "Wait after the child returned".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: BTreeMap::new(),
                },
                repeat: None,
                activity_io: None,
            },
        );
        middle.sequence_flows = vec![
            edge("StartCall", "Start_1", "Call_1"),
            edge("CallWait", "Call_1", "MiddleWait"),
            edge("WaitEnd", "MiddleWait", "End_1"),
        ];
        let b = publish_model(&f, &middle);
        let a = publish_model(
            &f,
            &with_boundaries(caller(&b, BTreeMap::new()), "Call_1", &[("Limit", true, 1)]),
        );
        let parent = support::start_version(&f, &a);
        let middle_id = child_id(&f, &parent.instance_id);
        let source_id = child_id(&f, &middle_id);
        let before = repository::get_instance(&f.db, &f.owner, &source_id, None).unwrap();
        assert_eq!(before.status, ProcessInstanceStatus::Completed);
        assert_eq!(before.outgoing_messages.len(), 2);
        let pending = before.outgoing_messages.iter().find(|message|
            message.source_node_id.as_deref() == Some("ThrowPending")).unwrap();
        let pending_id = pending.message_id.clone();
        let pending_digest = pending.payload_sha256.clone();
        let pending_bytes = pending.payload_bytes;
        let source_fact = events(&f, &source_id).into_iter().find(|event|
            event.kind == "message_queued" && event.data["message_id"] == pending_id).unwrap();
        let source_event_id: String = f.db.read().unwrap().query_row(
            "SELECT source_event_id FROM bpmn_messages WHERE message_id=?1",
            [&pending_id], |row| row.get(0)).unwrap();
        assert_eq!(source_event_id, source_fact.event_id);
        let drain = messages::drain_pending(&f.db, chrono::Utc::now().timestamp_millis());
        assert!(drain.completion.is_ok());
        assert_eq!(drain.delivered, 1);
        let delivered = repository::get_instance(&f.db, &f.owner, &source_id, None).unwrap();
        assert_eq!(
            delivered
                .outgoing_messages
                .iter()
                .filter(|message| message.status == ProcessMessageStatus::Delivered)
                .count(),
            1
        );
        if interrupt {
            let due = repository::runtime_snapshot(&f.db, &f.owner, &parent.instance_id)
                .unwrap()
                .timers[0]
                .due_at_ms
                .unwrap();
            let drain = timers::drain_due(&f.db, due);
            assert!(drain.completion.is_ok());
            assert_eq!(drain.fired, 1);
            let terminal = repository::get_instance(&f.db, &f.owner, &source_id, None).unwrap();
            assert_eq!(terminal.status, ProcessInstanceStatus::Completed);
            assert_eq!(terminal.revision, delivered.revision);
            assert_eq!(terminal.variables, delivered.variables);
            assert_eq!(
                terminal
                    .outgoing_messages
                    .iter()
                    .filter(|message| message.status == ProcessMessageStatus::Pending)
                    .count(),
                1
            );
            assert_eq!(
                terminal
                    .outgoing_messages
                    .iter()
                    .filter(|message| message.status == ProcessMessageStatus::Delivered)
                    .count(),
                1
            );
            assert_eq!(
                repository::get_instance(&f.db, &f.owner, &middle_id, None)
                    .unwrap()
                    .status,
                ProcessInstanceStatus::Cancelled
            );
            let retained = terminal.outgoing_messages.iter().find(|message|
                message.message_id == pending_id).unwrap();
            assert_eq!(retained.last_reason, None);
            assert_eq!(retained.payload_sha256, pending_digest);
            assert_eq!(retained.payload_bytes, pending_bytes);
            assert_eq!(events(&f, &source_id).iter().filter(|event|
                event.kind == "message_cancelled"
                    && event.data["message_id"] == pending_id).count(), 0);
        } else {
            complete(&f, &f.owner, &middle_id, json!({}));
            assert_eq!(
                repository::get_instance(&f.db, &f.owner, &parent.instance_id, None)
                    .unwrap()
                    .status,
                ProcessInstanceStatus::Completed
            );
        }
        let waiting =
            repository::get_instance(&f.db, &f.owner, &receiver.instance_id, None).unwrap();
        support::complete(
            &f,
            &receiver.instance_id,
            &waiting.user_tasks[0].user_task_id,
        );
        let reopened = crate::db::init(&f.directory.path().join("processes.db")).unwrap();
        let still_pending = repository::get_message(&reopened, &f.owner,
            &f.owner.user_id, &pending_id).unwrap().message;
        assert_eq!(still_pending.status, ProcessMessageStatus::Pending);
        assert_eq!(still_pending.payload_sha256, pending_digest);
        assert_eq!(still_pending.payload_bytes, pending_bytes);
        assert_eq!(still_pending.target, pending.target);
        assert_eq!(still_pending.message_name, pending.message_name);
        assert_eq!(still_pending.correlation_key, pending.correlation_key);
        let pending_budget: (i64, i64) = reopened.read().unwrap().query_row(
            "SELECT COUNT(*),COALESCE(SUM(payload_bytes),0) FROM bpmn_messages
             WHERE org_id=?1 AND status IN ('pending','blocked','ambiguous')",
            [&f.owner.org_id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(pending_budget, (1, i64::from(pending_bytes)));
        let drain = messages::drain_pending(&reopened, chrono::Utc::now().timestamp_millis() + 2_000);
        assert!(drain.completion.is_ok());
        assert_eq!(drain.delivered, 1);
        let receiver_after =
            repository::get_instance(&reopened, &f.owner, &receiver.instance_id, None).unwrap();
        assert_eq!(receiver_after.status, ProcessInstanceStatus::Completed);
        assert_eq!(receiver_after.variables["received"]["real"], 42);
        let receipt = repository::get_message(&reopened, &f.owner,
            &f.owner.user_id, &pending_id).unwrap().message;
        assert_eq!(receipt.status, ProcessMessageStatus::Delivered);
        assert_eq!(receipt.matched_instance_id.as_deref(), Some(receiver.instance_id.as_str()));
        assert_eq!(receipt.payload_sha256, pending_digest);
        assert_eq!(receipt.payload_bytes, pending_bytes);
        assert_eq!(receipt.target, pending.target);
        assert_eq!(receipt.message_name, pending.message_name);
        assert_eq!(receipt.correlation_key, pending.correlation_key);
        let cleared_budget: (i64, i64) = reopened.read().unwrap().query_row(
            "SELECT COUNT(*),COALESCE(SUM(payload_bytes),0) FROM bpmn_messages
             WHERE org_id=?1 AND status IN ('pending','blocked','ambiguous')",
            [&f.owner.org_id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
        assert_eq!(cleared_budget, (0, 0));
        let source_history = repository::list_events(&reopened, &f.owner,
            &source_id, 0, 100).unwrap().0;
        assert_eq!(source_history.iter().filter(|event| event.kind == "message_queued"
            && event.event_id == source_event_id).count(), 1);
        assert_eq!(source_history.iter().filter(|event| event.kind == "message_delivered"
            && event.data["message_id"] == pending_id).count(), 1);
        assert_eq!(source_history.iter().filter(|event| event.kind == "message_cancelled"
            && event.data["message_id"] == pending_id).count(), 0);
        let second = messages::drain_pending(&reopened, chrono::Utc::now().timestamp_millis() + 2_001);
        second.completion.unwrap();
        assert_eq!(second.delivered, 0);
    }
}

#[test]
fn corrupt_persisted_version_cycle_is_rejected_without_partial_caller_publication() {
    use sha2::{Digest, Sha256};
    let f = Fixture::new();
    let a = publish_model(&f, &super::model::starter_model());
    let b = publish_model(&f, &super::model::starter_model());
    let a_model = caller(&b, BTreeMap::new());
    let b_model = caller(&a, BTreeMap::new());
    let a_json = serde_json::to_string(&a_model).unwrap();
    let b_json = serde_json::to_string(&b_model).unwrap();
    let a_hash = hex::encode(Sha256::digest(a_json.as_bytes()));
    let b_hash = hex::encode(Sha256::digest(b_json.as_bytes()));
    {
        let conn = f.db.write().unwrap();
        for (definition, json, hash) in [
            (&a.definition_id, &a_json, &a_hash),
            (&b.definition_id, &b_json, &b_hash),
        ] {
            conn.execute("UPDATE bpmn_versions SET model_json=?1,model_sha256=?2 WHERE definition_id=?3 AND version=1", rusqlite::params![json, hash, definition]).unwrap();
        }
        for (source, target, hash) in [(&a, &b, &b_hash), (&b, &a, &a_hash)] {
            let element = ProcessCallableReference {
                namespace_uri: "https://tentaflow.app/bpmn/1".into(),
                process_id: target.model.process_id.clone(),
            };
            conn.execute("INSERT INTO bpmn_call_pins(definition_id,version,node_id,called_definition_id,called_version,called_element_json,model_sha256) VALUES(?1,1,'Call_1',?2,1,?3,?4)", rusqlite::params![source.definition_id,target.definition_id,serde_json::to_string(&element).unwrap(),hash]).unwrap();
        }
    }
    let draft = repository::save_definition(
        &f.db,
        &f.owner,
        &stamp("prepare corrupt target call"),
        None,
        0,
        "Refuse cyclic retained facts",
        "",
        &caller(&a, BTreeMap::new()),
    )
    .unwrap();
    let before = transition_rows(&f);
    let failure = repository::publish_definition(
        &f.db,
        &f.owner,
        &stamp("refuse cyclic store"),
        &draft.definition_id,
        draft.draft_revision,
        &[],
        None,
    )
    .unwrap_err();
    assert!(format!("{failure:#}").contains("process call body dependency cycle"));
    assert_eq!(transition_rows(&f), before);
    assert!(
        repository::list_versions(&f.db, &f.owner, &draft.definition_id, 0, 20)
            .unwrap()
            .0
            .is_empty()
    );
}

#[test]
fn call_message_boundaries_preserve_noninterrupting_fact_and_both_terminal_commit_orders() {
    use messages::test_support as support;
    for completion_first in [false, true] {
        let f = Fixture::new();
        let leaf = publish_model(&f, &user_model(None));
        let mut model = support::boundary_messages(
            caller(&leaf, BTreeMap::new()),
            "Call_1",
            &[
                ("Note", false, "EvidenceReady"),
                ("Stop", true, "EvidenceReady"),
            ],
        );
        model
            .messages
            .retain(|declaration| declaration.message_id == "Declaration_Note");
        for node in &mut model.nodes {
            if let ProcessNodeKind::BoundaryMessage { message_ref, .. } = &mut node.kind {
                *message_ref = "Declaration_Note".into();
            }
        }
        assert_eq!(model.messages.len(), 1);
        let version = publish_model(&f, &model);
        let parent = support::start_version(&f, &version);
        let child = child_id(&f, &parent.instance_id);
        let snapshot = repository::runtime_snapshot(&f.db, &f.owner, &parent.instance_id).unwrap();
        let note = snapshot
            .subscriptions
            .iter()
            .find(|sub| sub.node_id == "Note")
            .unwrap();
        let note_receipt = support::send(
            &f,
            &support::envelope(
                support::catch_target(
                    &version,
                    Some(&parent.instance_id),
                    Some(&note.subscription_id),
                ),
                json!({"note":42}),
            ),
        );
        let note_drain = messages::drain_pending(&f.db, note_receipt.received_at_ms);
        assert!(note_drain.completion.is_ok());
        assert_eq!(note_drain.delivered, 1);
        assert!(note_drain.cancelled_claims.is_empty());
        assert_eq!(
            repository::get_instance(&f.db, &f.owner, &child, None)
                .unwrap()
                .status,
            ProcessInstanceStatus::Waiting
        );
        let snapshot = repository::runtime_snapshot(&f.db, &f.owner, &parent.instance_id).unwrap();
        assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Waiting);
        let note_task = snapshot
            .user_tasks
            .iter()
            .find(|task| task.node_id == "Side_Note")
            .unwrap();
        assert_eq!(note_task.status, ProcessUserTaskStatus::Open);
        let note_task_id = note_task.user_task_id.clone();
        let stop = snapshot
            .subscriptions
            .iter()
            .find(|sub| sub.node_id == "Stop")
            .unwrap();
        assert_ne!(stop.subscription_id, note.subscription_id);
        let stop_receipt = support::send(
            &f,
            &support::envelope(
                support::catch_target(
                    &version,
                    Some(&parent.instance_id),
                    Some(&stop.subscription_id),
                ),
                json!({"stop":true}),
            ),
        );
        if completion_first {
            complete(&f, &f.owner, &child, json!({"answer":42}));
        }
        let stop_drain = messages::drain_pending(&f.db, stop_receipt.received_at_ms);
        assert!(stop_drain.completion.is_ok());
        assert_eq!(stop_drain.delivered, u32::from(!completion_first));
        let child_after = repository::get_instance(&f.db, &f.owner, &child, None).unwrap();
        assert_eq!(
            child_after.status,
            if completion_first {
                ProcessInstanceStatus::Completed
            } else {
                ProcessInstanceStatus::Cancelled
            }
        );
        let parent_waiting =
            repository::get_instance(&f.db, &f.owner, &parent.instance_id, None).unwrap();
        assert_eq!(parent_waiting.status, ProcessInstanceStatus::Waiting);
        assert_eq!(
            parent_waiting
                .user_tasks
                .iter()
                .find(|task| task.user_task_id == note_task_id)
                .unwrap()
                .status,
            ProcessUserTaskStatus::Open
        );
        let stop_task = parent_waiting
            .user_tasks
            .iter()
            .find(|task| task.node_id == "Side_Stop");
        assert_eq!(stop_task.is_some(), !completion_first);
        if let Some(task) = stop_task {
            assert_eq!(task.status, ProcessUserTaskStatus::Open);
        }
        let after_note = support::complete(&f, &parent.instance_id, &note_task_id);
        assert_eq!(
            after_note.status,
            if completion_first {
                ProcessInstanceStatus::Completed
            } else {
                ProcessInstanceStatus::Waiting
            }
        );
        if let Some(task) = stop_task {
            support::complete(&f, &parent.instance_id, &task.user_task_id);
        }
        let parent_after =
            repository::get_instance(&f.db, &f.owner, &parent.instance_id, None).unwrap();
        assert_eq!(parent_after.status, ProcessInstanceStatus::Completed);
        assert!(parent_after
            .user_tasks
            .iter()
            .all(|task| task.status == ProcessUserTaskStatus::Completed));
        let note_after = parent_after
            .subscriptions
            .iter()
            .find(|sub| sub.node_id == "Note")
            .unwrap();
        assert_eq!(note_after.status, ProcessSubscriptionStatus::Consumed);
        let stop_after = parent_after
            .subscriptions
            .iter()
            .find(|sub| sub.node_id == "Stop")
            .unwrap();
        assert_eq!(
            stop_after.status,
            if completion_first {
                ProcessSubscriptionStatus::Cancelled
            } else {
                ProcessSubscriptionStatus::Consumed
            }
        );
        assert_eq!(
            events(&f, &parent.instance_id)
                .iter()
                .filter(|event| event.kind == "message_delivered"
                    && event.data["message_id"] == note_receipt.message_id)
                .count(),
            1
        );
        assert_eq!(
            events(&f, &parent.instance_id)
                .iter()
                .filter(|event| event.kind == "call_returned")
                .count(),
            usize::from(completion_first)
        );
    }
}

#[test]
fn actual_call_tree_lifetime_and_active_variable_caps_leave_only_exact_waiting_incidents() {
    let f = Fixture::new();
    let target = publish_model(&f, &embedded_model(super::model::starter_model(), "Local"));
    let prototype = caller(&target, BTreeMap::new()).nodes[1].clone();
    let mut chain = super::model::starter_model();
    let mut previous = "Start_1".to_owned();
    chain.sequence_flows.clear();
    for index in 0..126 {
        let mut node = prototype.clone();
        node.id = format!("Call_{index:03}");
        chain
            .sequence_flows
            .push(edge(&format!("Step_{index}"), &previous, &node.id));
        previous = node.id.clone();
        chain.nodes.insert(chain.nodes.len() - 1, node);
    }
    chain
        .sequence_flows
        .push(edge("LastEnd", &previous, "End_1"));
    let version = publish_model(&f, &chain);
    let actual = messages::test_support::start_version(&f, &version);
    assert_eq!(actual.status, ProcessInstanceStatus::Incident);
    assert_eq!(actual.active_node_ids, vec!["Call_064"]);
    assert_eq!(actual.incidents[0].code, "CALL_ADMISSION_ERROR");
    let conn = f.db.read().unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM bpmn_scopes", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        129
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM bpmn_calls WHERE status='returned'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        64
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM bpmn_calls WHERE status='waiting'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM bpmn_tokens WHERE instance_id=?1 AND status='waiting'",
            [&actual.instance_id],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    drop(conn);

    let f = Fixture::new();
    let mut large = user_model(None);
    large
        .variables
        .insert("payload".into(), json!("x".repeat(245_000)));
    let target = publish_model(&f, &large);
    let prototype = caller(&target, BTreeMap::new()).nodes[1].clone();
    let mut model = super::model::starter_model();
    model.nodes.extend([
        ProcessNode {
            id: "Split".into(),
            name: "Split independent calls".into(),
            kind: ProcessNodeKind::ParallelGateway,
            repeat: None,
            activity_io: None,
        },
        ProcessNode {
            id: "Join".into(),
            name: "Join independent calls".into(),
            kind: ProcessNodeKind::ParallelGateway,
            repeat: None,
            activity_io: None,
        },
    ]);
    model.sequence_flows = vec![
        edge("StartSplit", "Start_1", "Split"),
        edge("JoinEnd", "Join", "End_1"),
    ];
    for index in 0..5 {
        let mut node = prototype.clone();
        node.id = format!("Call_{index}");
        model
            .sequence_flows
            .push(edge(&format!("SplitCall_{index}"), "Split", &node.id));
        model
            .sequence_flows
            .push(edge(&format!("CallJoin_{index}"), &node.id, "Join"));
        model.nodes.push(node);
    }
    let version = publish_model(&f, &model);
    let actual = messages::test_support::start_version(&f, &version);
    assert_eq!(actual.status, ProcessInstanceStatus::Incident);
    assert_eq!(actual.incidents[0].code, "CALL_ADMISSION_ERROR");
    assert!(actual.incidents[0].message.contains("1 MiB"));
    let conn = f.db.read().unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM bpmn_instances", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        5
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM bpmn_calls WHERE status='waiting'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        4
    );
    assert_eq!(
        conn.query_row(
            "SELECT COUNT(*) FROM bpmn_user_tasks WHERE status='open'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        4
    );
    assert!(conn.query_row("SELECT SUM(length(CAST(variables_json AS BLOB))) FROM bpmn_instances WHERE status NOT IN ('completed','cancelled','error')", [], |row| row.get::<_,i64>(0)).unwrap() <= 1024 * 1024);
}

#[test]
fn return_variable_budget_failure_keeps_completed_child_and_original_parent_local_variables() {
    for key_count in [false, true] {
        let f = Fixture::new();
        let target = publish_model(&f, &user_model(None));
        let mut model = caller(
            &target,
            BTreeMap::from([("returned".into(), "outputs.answer".into())]),
        );
        if key_count {
            model.variables = (0..128)
                .map(|index| (format!("key_{index}"), json!(index)))
                .collect();
        } else {
            model
                .variables
                .insert("large".into(), json!("x".repeat(245_000)));
        }
        let version = publish_model(&f, &model);
        let parent = messages::test_support::start_version(&f, &version);
        let child = child_id(&f, &parent.instance_id);
        let actual_output = if key_count {
            json!({"answer":42})
        } else {
            json!({"answer":"y".repeat(20_000)})
        };
        let source = complete(&f, &f.owner, &child, actual_output.clone());
        assert_eq!(source.instance.status, ProcessInstanceStatus::Completed);
        assert_eq!(source.instance.variables["answer"], actual_output["answer"]);
        let after = repository::get_instance(&f.db, &f.owner, &parent.instance_id, None).unwrap();
        assert_eq!(after.status, ProcessInstanceStatus::Incident);
        assert_eq!(after.variables, parent.variables);
        assert_eq!(after.active_node_ids, vec!["Call_1"]);
        assert_eq!(after.incidents[0].code, "CALL_RETURN_ERROR");
        assert_eq!(
            after.calls[0],
            ProcessCallSummary::Outgoing {
                call_node_id: "Call_1".into(),
                call_node_name: model.nodes[1].name.clone(),
                status: ProcessCallStatus::ReturnIncident,
                child: Some(ProcessRelatedInstance {
                    instance_id: child,
                    definition_name: source.instance.definition_name,
                    version: target.version,
                    status: ProcessInstanceStatus::Completed,
                    can_open: true
                }),
            }
        );
        assert!(events(&f, &parent.instance_id)
            .iter()
            .all(|event| event.kind != "call_returned"));
    }
}

#[test]
fn called_input_defaults_and_explicit_mapping_cannot_admit_a_129_key_root() {
    let f = Fixture::new();
    let mut target = user_model(None);
    target.variables.insert("default_key".into(), json!(true));
    let target = publish_model(&f, &target);
    let mut model = caller(&target, BTreeMap::new());
    let ProcessNodeKind::CallActivity(ProcessCallActivity { input_mapping, .. }) = &mut model.nodes[1].kind else {
        unreachable!()
    };
    *input_mapping = (0..128)
        .map(|index| (format!("key_{index}"), "1".into()))
        .collect();
    let version = publish_model(&f, &model);
    let instance_id = uuid::Uuid::new_v4().to_string();
    let variables = serde_json::to_value(&version.model.variables).unwrap();
    let command = stamp("reject forged Call admission request closure");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&version.model, &version.model.process_id, crate::processes::runtime::test_support::ordinary_start_id(&version.model), &instance_id, &f.owner,
        &version.definition_id, version.version, variables.clone(),
        runtime::StartCause::Manual, at_ms, runtime::test_support::manual_input(&command),
        None).unwrap();
    let before = super::signal_proof_tests::all_transition_rows(&f);
    for mutation in 0..7 {
        let inspected = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = inspected.clone();
        repository::CALL_PLAN_TEST_MUTATOR.with(|mutator| {
            *mutator.borrow_mut() = Some(Box::new(move |composite| {
                let step = composite.call_steps.iter_mut().find(|step|
                    matches!(step, repository::CallStep::Advance { .. }))
                    .expect("real rejected child admission");
                let repository::CallStep::Advance { request, plan, .. } = step else {
                    unreachable!()
                };
                observed.store(true, std::sync::atomic::Ordering::SeqCst);
                match mutation {
                    0 => request.call_id = uuid::Uuid::new_v4().to_string(),
                    1 => request.parent_scope_id = uuid::Uuid::new_v4().to_string(),
                    2 => request.parent_token_id = uuid::Uuid::new_v4().to_string(),
                    3 => request.call_node_id = "UnrelatedCall".into(),
                    4 => request.child_instance_id = uuid::Uuid::new_v4().to_string(),
                    5 => request.variables["key_0"] = json!(2),
                    6 => plan.event_ids.clear(),
                    _ => unreachable!(),
                }
            }));
        });
        let forged = repository::start_instance(&f.db, &f.owner, &command,
            &instance_id, &version.definition_id, version.version, &variables, None, None,
            repository::ProcessPlanInput::Supplied(&plan), at_ms);
        repository::CALL_PLAN_TEST_MUTATOR.with(|mutator| *mutator.borrow_mut() = None);
        assert!(inspected.load(std::sync::atomic::Ordering::SeqCst));
        assert!(forged.is_err(), "Call admission mutation {mutation}");
        assert_eq!(super::signal_proof_tests::all_transition_rows(&f), before,
            "Call admission mutation {mutation} rolls back every transition fact");
    }
    let actual = messages::test_support::start_version(&f, &version);
    assert_eq!(actual.status, ProcessInstanceStatus::Incident);
    assert_eq!(actual.active_node_ids, vec!["Call_1"]);
    assert_eq!(actual.incidents[0].code, "CALL_ADMISSION_ERROR");
    let conn = f.db.read().unwrap();
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM bpmn_instances", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM bpmn_calls", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row("SELECT COUNT(*) FROM bpmn_user_tasks", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(conn);
    let canonical_id = uuid::Uuid::new_v4().to_string();
    let canonical_command = stamp("retain canonical rejected child admission");
    let canonical = repository::start_instance(&f.db, &f.owner, &canonical_command,
        &canonical_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Canonical, at_ms).unwrap();
    assert_eq!(canonical.status, ProcessInstanceStatus::Incident);
    assert_eq!(canonical.active_node_ids, vec!["Call_1"]);
    assert_eq!(canonical.incidents.len(), 1);
    assert_eq!(canonical.incidents[0].code, "CALL_ADMISSION_ERROR");
    let persisted = super::signal_proof_tests::all_transition_rows(&f);
    let reopened = crate::db::init(&f.directory.path().join("processes.db")).unwrap();
    let replay = repository::start_instance(&reopened, &f.owner, &canonical_command,
        &canonical_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Canonical, at_ms).unwrap();
    assert_eq!(replay, canonical);
    assert_eq!(super::signal_proof_tests::all_transition_rows(&f), persisted);
}

#[test]
fn observed_called_service_result_survives_parent_cas_replan_without_another_execution() {
    let f = Fixture::new();
    let flow_id = flow(&f.db, &f.owner, &graph("result-once", None));
    let target = publish_model(
        &f,
        &service_model(
            &flow_id,
            ActivityVerification::Condition {
                expression: "true".into(),
            },
        ),
    );
    let version = publish_model(&f, &parallel_caller(&target));
    let parent = messages::test_support::start_version(&f, &version);
    let child = child_id(&f, &parent.instance_id);
    let claim = repository::claim_job(&f.db, "result-cas", chrono::Utc::now().timestamp_millis())
        .unwrap()
        .unwrap();
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
    let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(0);
    std::thread::scope(|scope| {
        let resume_tx = resume_tx;
        let f = &f;
        let observing = scope.spawn(move || {
            repository::CALL_TRANSITION_PREFLIGHT
                .with(|gate| *gate.borrow_mut() = Some((ready_tx, resume_rx)));
            let worker = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let result = worker.block_on(super::jobs::execute_claimed(
                &f.db,
                f.dispatcher(),
                "result-cas",
                claim,
                tokio_util::sync::CancellationToken::new(),
            ));
            repository::CALL_TRANSITION_PREFLIGHT.with(|gate| *gate.borrow_mut() = None);
            result
        });
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        assert_eq!(
            crate::db::repository::list_flow_executions_for_flow(&f.db, &flow_id, 10)
                .unwrap()
                .len(),
            1
        );
        assert!(repository::runtime_snapshot(&f.db, &f.owner, &child)
            .unwrap()
            .jobs[0]
            .result
            .is_none());
        complete(
            &f,
            &f.owner,
            &parent.instance_id,
            json!({"answer":"concurrent sibling"}),
        );
        resume_tx.send(()).unwrap();
        observing.join().unwrap().unwrap();
    });
    let returned = repository::get_instance(&f.db, &f.owner, &parent.instance_id, None).unwrap();
    assert_eq!(returned.status, ProcessInstanceStatus::Completed);
    assert_eq!(returned.variables["received"], "result-once");
    assert_eq!(returned.variables["sibling"], "concurrent sibling");
    assert!(returned.incidents.is_empty());
    assert_eq!(
        events(&f, &child)
            .iter()
            .filter(|event| event.kind == "service_result")
            .count(),
        1
    );
    assert_eq!(
        events(&f, &parent.instance_id)
            .iter()
            .filter(|event| event.kind == "call_returned")
            .count(),
        1
    );
    assert_eq!(
        crate::db::repository::list_flow_executions_for_flow(&f.db, &flow_id, 10)
            .unwrap()
            .len(),
        1
    );
}
