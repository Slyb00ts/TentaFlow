// ============ File: simulation_call_tests.rs — process calls in a private simulation ============

use std::collections::BTreeMap;

use serde_json::{json, Value};
use tentaflow_protocol::processes::{
    ProcessCallActivity, ProcessCallStatus, ProcessCallTarget, ProcessCallableReference,
    ProcessErrorDeclaration, ProcessExecutableProcess, ProcessInstanceStatus, ProcessModel,
    ProcessMultiInstanceInput, ProcessMultiInstanceMode, ProcessNodeKind, ProcessRepeatSpec,
};

use super::model::starter_model;
use super::repository;
use super::runtime::test_support::{edge, publish_model, stamp, user_model, Fixture};
use super::simulation::{
    SimulationStore, SimulationView, MAX_SIMULATION_INSTANCES, SIMULATION_CALL_LIMIT,
    SIMULATION_CALL_PIN_UNAVAILABLE, SIMULATION_CALL_UNSUPPORTED,
};
use super::simulation_scope_tests::{
    advance_until, complete_open_work, node, open_work, refusal_code, script, user_task,
};
use super::simulation_service_tests::normalize_events;
use super::simulation_tests::test_authorization;

const NAMESPACE: &str = "https://tentaflow.app/bpmn/1";

fn reference(process_id: &str) -> ProcessCallableReference {
    ProcessCallableReference {
        namespace_uri: NAMESPACE.into(),
        process_id: process_id.into(),
    }
}

fn call_node(
    id: &str,
    target: ProcessCallTarget,
    output: &[(&str, &str)],
) -> tentaflow_protocol::processes::ProcessNode {
    node(
        id,
        ProcessNodeKind::CallActivity(ProcessCallActivity {
            target,
            input_mapping: BTreeMap::new(),
            output_mapping: output
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect(),
        }),
    )
}

/// Start, one call to the published callee, End.
fn caller_of(callee: &tentaflow_protocol::processes::ProcessVersion) -> ProcessModel {
    let mut model = starter_model();
    model.nodes = vec![
        model.nodes[0].clone(),
        call_node(
            "Call_1",
            ProcessCallTarget::PublishedBody {
                definition_id: callee.definition_id.clone(),
                version: callee.version,
                called_element: reference(&callee.model.process_id),
            },
            &[("result", "outputs.answer")],
        ),
        model.nodes.last().expect("starter End").clone(),
    ];
    model.sequence_flows = vec![
        edge("ToCall", "Start_1", "Call_1"),
        edge("CallEnd", "Call_1", "End_1"),
    ];
    model
}

/// Captures the published caller from production exactly as a Start request does
/// and opens the private run on it.
fn started_from(
    fixture: &Fixture,
    caller: &tentaflow_protocol::processes::ProcessVersion,
) -> anyhow::Result<SimulationStore> {
    // The scenario supplies the whole initial scope: the model's defaults are handed over.
    let variables = serde_json::to_value(&caller.model.variables)?;
    let source = repository::capture_simulation_source(
        &fixture.db,
        &fixture.owner,
        &caller.definition_id,
        caller.version,
        &caller.model.process_id,
        "Start_1",
        &variables,
        1_000,
        20_000,
        100,
    )?;
    let mut store = SimulationStore::create(source)?;
    let authorization = test_authorization(&store);
    store.start(authorization, variables)?;
    Ok(store)
}

/// The contents of every process table of production plus the audit trail, authoring,
/// call-pin and signal tables included: a private run may read them, never write them.
fn all_production_rows(fixture: &Fixture) -> Vec<(String, Vec<Vec<rusqlite::types::Value>>)> {
    let conn = fixture.db.read().expect("production connection");
    let tables = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND (name LIKE 'bpmn\\_%' ESCAPE '\\' OR name='audit_log') ORDER BY name")
        .expect("list tables")
        .query_map([], |row| row.get::<_, String>(0))
        .expect("tables")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("table names");
    assert!(tables.iter().any(|name| name == "audit_log"));
    assert!(tables.iter().any(|name| name == "bpmn_signal_emissions"));
    tables
        .into_iter()
        .map(|table| {
            let rows = super::call_pin_tests::table_rows(&conn, &table, "rowid");
            (table, rows)
        })
        .collect()
}

fn call_of(view: &SimulationView) -> &tentaflow_protocol::processes::ProcessSimulationCall {
    view.calls.first().expect("the run made a call")
}

#[test]
fn a_published_call_pins_the_callee_into_the_private_run_and_returns_its_result() {
    let fixture = Fixture::new();
    let callee = publish_model(&fixture, &user_model(Some(&fixture.owner.user_id)));
    let caller = publish_model(&fixture, &caller_of(&callee));
    let mut store = started_from(&fixture, &caller).expect("start the run");

    let view = advance_until(&mut store, "the callee's task", |view| {
        !open_work(view).is_empty()
    });
    assert_eq!(view.instances.len(), 2);
    assert_eq!(view.instances[0].depth, 0);
    assert_eq!(view.instances[1].depth, 1);
    assert_eq!(view.instances[1].definition_id, callee.definition_id);
    assert_eq!(
        view.instances[1].parent_instance_id.as_deref(),
        Some(view.instances[0].instance_id.as_str())
    );
    let call = call_of(&view);
    assert_eq!(call.call_node_id, "Call_1");
    assert_eq!(call.called_definition_id, callee.definition_id);
    assert_eq!(call.called_version, callee.version);
    assert!(!call.local);
    assert_eq!(call.status, ProcessCallStatus::Waiting);
    assert_eq!(call.child_instance_id, view.instances[1].instance_id);
    // The callee's task belongs to the child instance's scope, not the caller's.
    assert_eq!(open_work(&view)[0].scope_id, call.child_instance_id);
    assert_eq!(
        view.instance.as_ref().expect("instance").status,
        ProcessInstanceStatus::Waiting
    );

    let done = complete_open_work(&mut store, json!({"answer": "from the callee"}));
    let instance = done.instance.as_ref().expect("instance");
    assert_eq!(instance.status, ProcessInstanceStatus::Completed);
    assert_eq!(instance.variables["result"], json!("from the callee"));
    assert_eq!(call_of(&done).status, ProcessCallStatus::Returned);
    assert_eq!(done.instances[1].status, ProcessInstanceStatus::Completed);
    assert!(done
        .events
        .iter()
        .any(|event| event.kind == "call_entered"
            && event.instance_id == done.instances[0].instance_id));
    assert!(done
        .events
        .iter()
        .any(|event| event.kind == "instance_completed"
            && event.instance_id == done.instances[1].instance_id));
}

#[test]
fn a_call_run_repeats_with_the_same_ids_and_trace() {
    let fixture = Fixture::new();
    let callee = publish_model(&fixture, &user_model(Some(&fixture.owner.user_id)));
    let caller = publish_model(&fixture, &caller_of(&callee));
    let run = || {
        let mut store = started_from(&fixture, &caller).expect("start the run");
        advance_until(&mut store, "the callee's task", |view| {
            !open_work(view).is_empty()
        });
        let view = complete_open_work(&mut store, json!({"answer": "x"}));
        (
            view.instances
                .iter()
                .map(|instance| instance.instance_id.clone())
                .collect::<Vec<_>>(),
            view.calls
                .iter()
                .map(|call| call.call_id.clone())
                .collect::<Vec<_>>(),
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
                        event.instance_id.clone(),
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
    assert_eq!((first.0.len(), first.1.len()), (2, 1));
    assert!(first.3.len() > 8, "the run left a real history");
    assert_eq!(first, second);
}

fn events_of(view: &SimulationView, instance_id: &str) -> Vec<(String, Option<String>, Value)> {
    view.events
        .iter()
        .filter(|event| event.instance_id == instance_id)
        .map(|event| {
            (
                event.kind.clone(),
                event.node_id.clone(),
                event.data.clone(),
            )
        })
        .collect()
}

#[tokio::test]
async fn the_simulated_call_trace_equals_the_production_execution() {
    use super::runtime::test_support::{stamp, start_model};

    let production = Fixture::new();
    let callee = publish_model(&production, &user_model(None));
    let caller_model = caller_of(&callee);
    let started = start_model(&production, &caller_model);
    let parent_id = started.instance_id.clone();
    let child_id = super::call_tests::child_id(&production, &parent_id);
    let snapshot = repository::runtime_snapshot(&production.db, &production.owner, &child_id)
        .expect("production child snapshot");
    let task = snapshot
        .user_tasks
        .iter()
        .find(|task| task.status == tentaflow_protocol::processes::ProcessUserTaskStatus::Open)
        .expect("production opens the callee's task");
    repository::complete_user_task(
        &production.db,
        &production.owner,
        &stamp("complete"),
        &child_id,
        &task.user_task_id,
        snapshot.instance.revision,
        &json!({"answer": "from the callee"}),
        None,
        repository::ProcessPlanInput::Canonical,
        chrono::Utc::now().timestamp_millis(),
    )
    .expect("complete the production callee task");
    let history = |id: &str| -> Vec<(String, Option<String>, Value)> {
        repository::list_events(&production.db, &production.owner, id, 0, 200)
            .expect("production history")
            .0
            .into_iter()
            .map(|event| (event.kind, event.node_id, event.data))
            .collect()
    };
    // Parent history first, then the child's: identifiers become the ordinal of their
    // first appearance across the pair, so the link between them is compared too.
    let production_events = normalize_events(
        history(&parent_id)
            .into_iter()
            .chain(history(&child_id))
            .collect(),
    );
    let production_parent =
        repository::get_instance(&production.db, &production.owner, &parent_id, None)
            .expect("production parent")
            .variables;

    // The simulation of the same published pair, with the task assigned to the actor.
    let simulation = Fixture::new();
    let sim_callee = publish_model(&simulation, &user_model(Some(&simulation.owner.user_id)));
    let sim_caller = publish_model(&simulation, &caller_of(&sim_callee));
    let mut store = started_from(&simulation, &sim_caller).expect("start the run");
    advance_until(&mut store, "the callee's task", |view| {
        !open_work(view).is_empty()
    });
    let done = complete_open_work(&mut store, json!({"answer": "from the callee"}));
    let (parent, child) = (
        &done.instances[0].instance_id,
        &done.instances[1].instance_id,
    );
    let simulated_events = normalize_events(
        events_of(&done, parent)
            .into_iter()
            .chain(events_of(&done, child))
            .collect(),
    );

    assert_eq!(simulated_events, production_events);
    assert_eq!(
        done.instance.as_ref().expect("instance").variables,
        production_parent
    );
}

#[test]
fn a_simulated_call_writes_nothing_to_production_and_pins_only_by_reading() {
    let fixture = Fixture::new();
    let callee = publish_model(&fixture, &user_model(Some(&fixture.owner.user_id)));
    let caller = publish_model(&fixture, &caller_of(&callee));
    let before = all_production_rows(&fixture);

    let mut store = started_from(&fixture, &caller).expect("start the run");
    advance_until(&mut store, "the callee's task", |view| {
        !open_work(view).is_empty()
    });
    complete_open_work(&mut store, json!({"answer": "x"}));

    // Production holds no instance, call, event or audit row of the run.
    assert_eq!(all_production_rows(&fixture), before);
    let rows: (i64, i64) = fixture
        .db
        .read()
        .expect("production connection")
        .query_row(
            "SELECT (SELECT COUNT(*) FROM bpmn_instances),(SELECT COUNT(*) FROM bpmn_calls)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("production counts");
    assert_eq!(rows, (0, 0));
    // The pinned callee is a copy inside the private database.
    let private = store.private_row_snapshot().expect("private rows");
    let versions = &private["bpmn_versions"];
    assert_eq!(
        versions.len(),
        2,
        "the simulated version and its pinned callee"
    );
    assert!(private["bpmn_call_pins"].len() == 1 && private["bpmn_calls"].len() == 1);
}

#[test]
fn a_call_whose_target_production_would_refuse_is_refused_at_start_with_a_named_code() {
    let fixture = Fixture::new();
    let callee = publish_model(&fixture, &user_model(Some(&fixture.owner.user_id)));
    let caller = publish_model(&fixture, &caller_of(&callee));
    // Archiving the callee after the caller was published: production would raise
    // CALL_ADMISSION_ERROR at the call, so the simulation says so before it starts.
    let revision = repository::get_definition(&fixture.db, &fixture.owner, &callee.definition_id)
        .expect("callee definition")
        .0
        .draft_revision;
    repository::archive_definition(
        &fixture.db,
        &fixture.owner,
        &super::runtime::test_support::stamp("archive"),
        &callee.definition_id,
        revision,
        true,
    )
    .expect("archive the callee");

    let error = started_from(&fixture, &caller).expect_err("the start is refused");
    assert_eq!(
        refusal_code(&error),
        SIMULATION_CALL_PIN_UNAVAILABLE,
        "{error:#}"
    );
}

fn local_call_model() -> ProcessModel {
    let mut model = starter_model();
    model.variables = BTreeMap::from([("value".into(), json!(1))]);
    model.additional_processes.push(ProcessExecutableProcess {
        process_id: "Child_Process".into(),
        process_name: None,
        nodes: vec![
            node("Child_Start", ProcessNodeKind::Start),
            node("Child_Script", script("vars.value + 41", "answer")),
            node("Child_End", ProcessNodeKind::End),
        ],
        sequence_flows: vec![
            edge("Child_Flow1", "Child_Start", "Child_Script"),
            edge("Child_Flow2", "Child_Script", "Child_End"),
        ],
        variables: BTreeMap::new(),
        diagram: Default::default(),
        timer_timezone: None,
        work_calendar: None,
        calendar_pin: None,
        modeling: None,
    });
    let mut call = call_node(
        "Call_Local",
        ProcessCallTarget::LocalBody {
            called_element: reference("Child_Process"),
        },
        &[("result", "outputs.answer")],
    );
    if let ProcessNodeKind::CallActivity(activity) = &mut call.kind {
        activity
            .input_mapping
            .insert("value".into(), "vars.value".into());
    }
    model.nodes = vec![
        model.nodes[0].clone(),
        call,
        model.nodes.last().expect("starter End").clone(),
    ];
    model.sequence_flows = vec![
        edge("ToCall", "Start_1", "Call_Local"),
        edge("CallEnd", "Call_Local", "End_1"),
    ];
    model
}

#[test]
fn a_local_body_call_runs_a_body_of_the_same_document() {
    let fixture = Fixture::new();
    let caller = publish_model(&fixture, &local_call_model());
    let source = repository::capture_simulation_source(
        &fixture.db,
        &fixture.owner,
        &caller.definition_id,
        caller.version,
        &caller.model.process_id,
        "Start_1",
        &json!({"value": 1}),
        1_000,
        20_000,
        100,
    )
    .expect("capture");
    let mut store = SimulationStore::create(source).expect("create");
    let authorization = test_authorization(&store);
    store
        .start(authorization, json!({"value": 1}))
        .expect("start");
    let done = advance_until(&mut store, "the run to finish", |view| {
        view.instance
            .as_ref()
            .is_some_and(|instance| instance.status == ProcessInstanceStatus::Completed)
    });
    assert!(call_of(&done).local);
    assert_eq!(call_of(&done).called_process_id, "Child_Process");
    assert_eq!(
        done.instance.as_ref().expect("instance").variables["result"],
        json!(42)
    );
}

#[test]
fn an_error_end_in_the_callee_is_caught_on_the_call_activity() {
    let fixture = Fixture::new();
    let mut callee_model = user_model(Some(&fixture.owner.user_id));
    callee_model.errors.push(ProcessErrorDeclaration {
        error_id: "Rejected".into(),
        name: "Rejected".into(),
        error_code: "REJECTED".into(),
    });
    callee_model.nodes.last_mut().expect("callee End").kind = ProcessNodeKind::ErrorEnd {
        error_ref: "Rejected".into(),
    };
    let callee = publish_model(&fixture, &callee_model);
    let mut caller_model = caller_of(&callee);
    caller_model.errors.push(ProcessErrorDeclaration {
        error_id: "Rejected".into(),
        name: "Rejected".into(),
        error_code: "REJECTED".into(),
    });
    caller_model.nodes.push(node(
        "Guard_1",
        ProcessNodeKind::BoundaryError {
            attached_to_id: "Call_1".into(),
            error_ref: Some("Rejected".into()),
            output_mapping: BTreeMap::from([("caught".into(), "outputs.answer".into())]),
        },
    ));
    caller_model.nodes.push(node("End_H", ProcessNodeKind::End));
    caller_model
        .sequence_flows
        .push(edge("ToHandlerEnd", "Guard_1", "End_H"));
    let caller = publish_model(&fixture, &caller_model);
    let mut store = started_from(&fixture, &caller).expect("start the run");
    advance_until(&mut store, "the callee's task", |view| {
        !open_work(view).is_empty()
    });
    complete_open_work(&mut store, json!({"answer": "no"}));
    let done = advance_until(&mut store, "the caller to finish", |view| {
        view.instance
            .as_ref()
            .is_some_and(|instance| instance.status == ProcessInstanceStatus::Completed)
    });
    assert!(done
        .events
        .iter()
        .any(|event| event.kind == "business_error_caught"));
    assert_eq!(call_of(&done).status, ProcessCallStatus::Error);
    assert_eq!(done.instances[1].status, ProcessInstanceStatus::Error);
    assert_eq!(
        done.instance.as_ref().expect("instance").variables["caught"],
        json!("no")
    );
}

#[test]
fn a_called_process_that_waits_for_a_message_is_refused_with_a_named_code() {
    let fixture = Fixture::new();
    let mut callee_model = starter_model();
    callee_model
        .messages
        .push(tentaflow_protocol::processes::ProcessMessageDeclaration {
            message_id: "Evidence".into(),
            name: "EvidenceReady".into(),
        });
    callee_model.nodes = vec![
        callee_model.nodes[0].clone(),
        node(
            "Catch_1",
            ProcessNodeKind::MessageCatch {
                message_ref: "Evidence".into(),
                correlation_expression: "\"case-1\"".into(),
                output_mapping: BTreeMap::new(),
            },
        ),
        callee_model.nodes.last().expect("End").clone(),
    ];
    callee_model.sequence_flows = vec![
        edge("A", "Start_1", "Catch_1"),
        edge("B", "Catch_1", "End_1"),
    ];
    let callee = publish_model(&fixture, &callee_model);
    let caller = publish_model(&fixture, &caller_of(&callee));
    let error = started_from(&fixture, &caller).expect_err("the start is refused");
    assert!(
        format!("{error:#}").starts_with(SIMULATION_CALL_UNSUPPORTED),
        "{error:#}"
    );
}

#[test]
fn a_run_cannot_start_more_instances_than_the_simulation_limit() {
    let fixture = Fixture::new();
    let callee = publish_model(&fixture, &user_model(Some(&fixture.owner.user_id)));
    let mut caller_model = caller_of(&callee);
    caller_model.variables = BTreeMap::from([("results".into(), json!([]))]);
    // Sixteen children where eight instances are the most a run holds.
    caller_model.nodes[1].repeat = Some(ProcessRepeatSpec::MultiInstance {
        mode: ProcessMultiInstanceMode::Parallel,
        input: ProcessMultiInstanceInput::Cardinality { count: 16 },
        output_collection_variable: "results".into(),
    });
    // A person's task ahead of the call keeps the start apart from the call, so the
    // refusal falls on a step of the live run.
    caller_model
        .nodes
        .insert(1, node("Ask_1", user_task(Some(&fixture.owner.user_id))));
    caller_model.sequence_flows = vec![
        edge("ToAsk", "Start_1", "Ask_1"),
        edge("ToCall", "Ask_1", "Call_1"),
        edge("CallEnd", "Call_1", "End_1"),
    ];
    let caller = publish_model(&fixture, &caller_model);
    let complete_ask = |store: &mut SimulationStore| {
        let view = store.view(test_authorization(store)).expect("read the run");
        let task_id = open_work(&view)[0].user_task_id.clone();
        store.complete_user_task(
            test_authorization(store),
            &task_id,
            json!({"answer": 1}),
            None,
        )
    };

    let mut refused = started_from(&fixture, &caller).expect("start the run");
    advance_until(&mut refused, "the first task", |view| {
        !open_work(view).is_empty()
    });
    let error = complete_ask(&mut refused).expect_err("the calls are refused");
    assert_eq!(refusal_code(&error), SIMULATION_CALL_LIMIT, "{error:#}");
    // Refused before the ninth child was started, not after all sixteen were built.
    assert!(format!("{error:#}").contains("instance 9,"), "{error:#}");
    let view = refused
        .view(test_authorization(&refused))
        .expect("read the run");
    assert_eq!(
        view.instances.len(),
        1,
        "the refused transition started no instance"
    );
    assert!(view.calls.is_empty());
    assert_eq!(
        open_work(&view).len(),
        1,
        "the task is still open: nothing was applied"
    );

    // The refusal left no trace: the next step mints exactly what it would have
    // minted had the refused one never been asked for.
    let after_refusal = refused
        .advance(test_authorization(&refused))
        .expect("the run is usable");
    let mut untouched = started_from(&fixture, &caller).expect("start the run");
    advance_until(&mut untouched, "the first task", |view| {
        !open_work(view).is_empty()
    });
    let direct = untouched
        .advance(test_authorization(&untouched))
        .expect("advance");
    let last = |view: &SimulationView| {
        (
            view.trace_steps
                .last()
                .map(|step| (step.trace_step_id.clone(), step.result_sha256.clone())),
            view.clock.step_index,
            view.events
                .iter()
                .map(|event| event.event_id.clone())
                .collect::<Vec<_>>(),
        )
    };
    assert_eq!(last(&after_refusal), last(&direct));
}

#[test]
fn a_call_is_pinned_to_the_callee_version_its_caller_was_published_against() {
    let fixture = Fixture::new();
    let callee = publish_model(&fixture, &user_model(Some(&fixture.owner.user_id)));
    let caller = publish_model(&fixture, &caller_of(&callee));
    // The callee moves on: a newer version is published after the caller.
    let mut newer = user_model(Some(&fixture.owner.user_id));
    for step in &mut newer.nodes {
        if let ProcessNodeKind::UserTask { output_mapping, .. } = &mut step.kind {
            *output_mapping = BTreeMap::from([("answer".into(), "outputs.other".into())]);
        }
    }
    let draft = repository::get_definition(&fixture.db, &fixture.owner, &callee.definition_id)
        .expect("callee definition")
        .0;
    let saved = repository::save_definition(
        &fixture.db,
        &fixture.owner,
        &stamp("update callee"),
        Some(&callee.definition_id),
        draft.draft_revision,
        "Evidence process",
        "",
        &newer,
    )
    .expect("save the newer callee");
    let snapshots = crate::dispatch::processes::pin_service_snapshots(
        fixture.dispatcher(),
        &fixture.owner,
        &newer,
    )
    .expect("pin the newer callee");
    let (_, second_version) = repository::publish_definition(
        &fixture.db,
        &fixture.owner,
        &stamp("publish callee 2"),
        &callee.definition_id,
        saved.draft_revision,
        &snapshots,
        None,
    )
    .expect("publish the newer callee");
    assert_eq!(second_version.version, callee.version + 1);

    let mut store = started_from(&fixture, &caller).expect("start the run");
    let view = advance_until(&mut store, "the callee's task", |view| {
        !open_work(view).is_empty()
    });
    assert_eq!(call_of(&view).called_version, callee.version);
    let done = complete_open_work(&mut store, json!({"answer": "from version 1"}));
    assert_eq!(
        done.instance.as_ref().expect("instance").variables["result"],
        json!("from version 1"),
        "the older callee ran: the newer one maps its answer elsewhere"
    );
    let private = store.private_row_snapshot().expect("private rows");
    assert_eq!(
        private["bpmn_versions"].len(),
        2,
        "only the pinned version was copied"
    );
}

#[test]
fn a_repeated_call_starts_one_child_per_occurrence() {
    let fixture = Fixture::new();
    let callee = publish_model(&fixture, &user_model(Some(&fixture.owner.user_id)));
    let mut caller_model = caller_of(&callee);
    caller_model.variables = BTreeMap::from([
        ("results".into(), json!([])),
        ("result".into(), json!(null)),
    ]);
    caller_model.nodes[1].repeat = Some(ProcessRepeatSpec::MultiInstance {
        mode: ProcessMultiInstanceMode::Parallel,
        input: ProcessMultiInstanceInput::Cardinality { count: 2 },
        output_collection_variable: "results".into(),
    });
    let caller = publish_model(&fixture, &caller_model);
    let mut store = started_from(&fixture, &caller).expect("start the run");
    let view = advance_until(&mut store, "two callee tasks", |view| {
        open_work(view).len() == 2
    });
    assert_eq!(view.instances.len(), 3);
    assert_eq!(view.calls.len(), 2);
    let mut view = view;
    for _ in 0..2 {
        view = complete_open_work(&mut store, json!({"answer": "x"}));
    }
    let instance = view.instance.as_ref().expect("instance");
    assert_eq!(
        instance.status,
        ProcessInstanceStatus::Completed,
        "{:?}",
        view.incidents
    );
    assert_eq!(
        instance.variables["results"].as_array().map(Vec::len),
        Some(2)
    );
    assert!(view
        .calls
        .iter()
        .all(|call| call.status == ProcessCallStatus::Returned));
}

#[test]
fn a_callee_that_calls_another_process_nests_two_children() {
    let fixture = Fixture::new();
    let inner = publish_model(&fixture, &user_model(Some(&fixture.owner.user_id)));
    let middle = publish_model(&fixture, &caller_of(&inner));
    let mut outer_model = caller_of(&middle);
    if let ProcessNodeKind::CallActivity(call) = &mut outer_model.nodes[1].kind {
        // The middle process hands its answer back as `result`.
        call.output_mapping = BTreeMap::from([("result".into(), "outputs.result".into())]);
    }
    let outer = publish_model(&fixture, &outer_model);
    let mut store = started_from(&fixture, &outer).expect("start the run");
    let view = advance_until(&mut store, "the innermost task", |view| {
        !open_work(view).is_empty()
    });
    assert_eq!(
        view.instances
            .iter()
            .map(|instance| instance.depth)
            .collect::<Vec<_>>(),
        [0, 1, 2]
    );
    assert_eq!(view.calls.len(), 2);
    let done = complete_open_work(&mut store, json!({"answer": "deep"}));
    let instance = done.instance.as_ref().expect("instance");
    assert_eq!(instance.status, ProcessInstanceStatus::Completed);
    assert_eq!(instance.variables["result"], json!("deep"));
}

#[test]
fn a_boundary_timer_on_a_call_activity_cancels_the_child() {
    let fixture = Fixture::new();
    let callee = publish_model(&fixture, &user_model(Some(&fixture.owner.user_id)));
    let mut caller_model = caller_of(&callee);
    caller_model.timer_timezone = Some("UTC".into());
    caller_model.nodes.push(node(
        "Guard_1",
        ProcessNodeKind::BoundaryTimer {
            attached_to_id: "Call_1".into(),
            cancel_activity: true,
            timer: tentaflow_protocol::processes::ProcessTimerSpec::Duration { seconds: 1 },
        },
    ));
    caller_model.nodes.push(node("End_H", ProcessNodeKind::End));
    caller_model
        .sequence_flows
        .push(edge("ToHandlerEnd", "Guard_1", "End_H"));
    let caller = publish_model(&fixture, &caller_model);
    let mut store = started_from(&fixture, &caller).expect("start the run");
    advance_until(&mut store, "the callee's task", |view| {
        !open_work(view).is_empty()
    });
    let done = advance_until(&mut store, "the timer to end the caller", |view| {
        view.instance
            .as_ref()
            .is_some_and(|instance| instance.status == ProcessInstanceStatus::Completed)
    });
    assert_eq!(done.clock.now_ms - done.clock.start_ms, 1_000);
    assert_eq!(call_of(&done).status, ProcessCallStatus::Cancelled);
    assert_eq!(done.instances[1].status, ProcessInstanceStatus::Cancelled);
    assert!(
        open_work(&done).is_empty(),
        "the child's task closed with the child"
    );
}

#[test]
fn a_service_step_in_a_called_process_waits_for_the_operator() {
    use super::runtime::test_support::{flow, graph, service_model};
    use tentaflow_protocol::processes::{ActivityOutcome, ActivityResult, ActivityVerification};

    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("marker", None));
    let callee = publish_model(
        &fixture,
        &service_model(
            &flow_id,
            ActivityVerification::Condition {
                expression: "true".into(),
            },
        ),
    );
    let caller = publish_model(&fixture, &caller_of(&callee));
    let mut store = started_from(&fixture, &caller).expect("start the run");
    let view = advance_until(&mut store, "the Service wait", |view| {
        !view.service_waits.is_empty()
    });
    assert_eq!(view.service_waits[0].node_id, "Service");
    let answer = ActivityResult {
        outcome: ActivityOutcome::Completed,
        code: None,
        summary: "answered".into(),
        outputs: json!({"variables": {"marker": "m"}}),
        evidence: Vec::new(),
    };
    store
        .resolve_service(test_authorization(&store), "Service", None, answer, None)
        .expect("answer the called step");
    let done = advance_until(&mut store, "the caller to finish", |view| {
        view.instance
            .as_ref()
            .is_some_and(|instance| instance.status == ProcessInstanceStatus::Completed)
    });
    let instance = done.instance.as_ref().expect("instance");
    assert_eq!(instance.status, ProcessInstanceStatus::Completed);
    assert_eq!(instance.variables["result"], json!("m"));
}

#[test]
fn a_call_run_continues_after_the_store_is_reopened_between_steps() {
    let fixture = Fixture::new();
    let callee = publish_model(&fixture, &user_model(Some(&fixture.owner.user_id)));
    let caller = publish_model(&fixture, &caller_of(&callee));
    let mut store = started_from(&fixture, &caller).expect("start the run");
    let view = advance_until(&mut store, "the callee's task", |view| {
        !open_work(view).is_empty()
    });
    let task_id = open_work(&view)[0].user_task_id.clone();
    // The retained run is reopened from its private database with a fresh allocator.
    let simulation_id = store.simulation_id().to_owned();
    let actor = repository::ProcessActor {
        org_id: view.source.org_id.clone(),
        user_id: view.source.actor_user_id.clone(),
    };
    let mut store =
        super::simulation_tests::reopen_store(store.into_database(), &simulation_id, actor)
            .expect("reopen the private run");
    let done = store
        .complete_user_task(
            test_authorization(&store),
            &task_id,
            json!({"answer": "again"}),
            None,
        )
        .expect("complete the callee's task after reopening");
    assert_eq!(
        done.instance.as_ref().expect("instance").status,
        ProcessInstanceStatus::Completed
    );
    assert_eq!(
        done.instance.as_ref().expect("instance").variables["result"],
        json!("again")
    );
    assert_eq!(call_of(&done).status, ProcessCallStatus::Returned);
}

#[test]
fn a_plan_that_arrives_with_call_steps_is_refused() {
    use super::repository::{CallRequest, CallStep, RuntimePlan};
    use super::simulation::validate_supported_plan;

    let mut plan = RuntimePlan::initial(json!({}));
    plan.call_steps.push(CallStep::Advance {
        instance_id: "instance".into(),
        expected_revision: 1,
        request: CallRequest {
            call_id: "call".into(),
            parent_scope_id: "scope".into(),
            parent_token_id: "token".into(),
            parent_arrival_edge_id: "edge".into(),
            call_node_id: "Call_1".into(),
            child_instance_id: "child".into(),
            variables: json!({}),
        },
        plan: Box::new(RuntimePlan::initial(json!({}))),
    });
    let error =
        validate_supported_plan(&plan, &[]).expect_err("steps the repository did not derive");
    assert!(
        format!("{error:#}").contains("derives its call steps itself"),
        "{error:#}"
    );
}

#[test]
fn a_boundary_timer_inside_a_called_process_fires_on_the_virtual_clock() {
    use super::simulation_event_tests::boundary_model;
    use tentaflow_protocol::processes::ProcessTimerSpec;

    let fixture = Fixture::new();
    let callee = publish_model(
        &fixture,
        &boundary_model(
            ProcessTimerSpec::Duration { seconds: 1 },
            true,
            Some(&fixture.owner.user_id),
        ),
    );
    // The callee's handler maps no answer, so the caller maps nothing back.
    let mut caller_model = caller_of(&callee);
    if let ProcessNodeKind::CallActivity(call) = &mut caller_model.nodes[1].kind {
        call.output_mapping.clear();
    }
    let caller = publish_model(&fixture, &caller_model);
    let mut store = started_from(&fixture, &caller).expect("start the run");
    advance_until(&mut store, "the callee's task", |view| {
        !open_work(view).is_empty()
    });
    // The callee's own timer fires in the child instance, one second of virtual time on.
    let view = advance_until(&mut store, "the callee's handler", |view| {
        open_work(view)
            .iter()
            .any(|task| task.node_id == "Handler_1")
    });
    assert_eq!(view.clock.now_ms - view.clock.start_ms, 1_000);
    assert!(view
        .events
        .iter()
        .any(|event| event.kind == "timer_fired"
            && event.instance_id == view.instances[1].instance_id));
    assert_eq!(view.instances[0].status, ProcessInstanceStatus::Waiting);
    let done = complete_open_work(&mut store, json!({"answer": "late"}));
    assert_eq!(done.instances[1].status, ProcessInstanceStatus::Completed);
    assert_eq!(call_of(&done).status, ProcessCallStatus::Returned);
}

/// A document that calls its own additional process `Child_Process`, whose body is
/// `nodes` and `flows`; `decorate` declares whatever the body needs.
fn local_call_with_body(
    nodes: Vec<tentaflow_protocol::processes::ProcessNode>,
    flows: Vec<tentaflow_protocol::processes::ProcessSequenceFlow>,
    decorate: impl FnOnce(&mut ProcessModel),
) -> ProcessModel {
    let mut model = local_call_model();
    model.additional_processes[0].nodes = nodes;
    model.additional_processes[0].sequence_flows = flows;
    decorate(&mut model);
    model
}

#[test]
fn a_local_body_that_waits_for_an_event_is_refused_like_a_published_one() {
    use super::simulation_tests::{create_store, source_input_for};
    use tentaflow_protocol::processes::{
        ProcessMessageDeclaration, ProcessSignalDeclaration, ProcessTimerSpec,
    };
    let message_catch = || {
        node(
            "Child_Catch",
            ProcessNodeKind::MessageCatch {
                message_ref: "Message_1".into(),
                correlation_expression: "'case-1'".into(),
                output_mapping: BTreeMap::new(),
            },
        )
    };
    let declare_message = |model: &mut ProcessModel| {
        model.messages.push(ProcessMessageDeclaration {
            message_id: "Message_1".into(),
            name: "EvidenceReady".into(),
        });
    };
    let cases: Vec<(&str, ProcessModel)> = vec![
        (
            "MessageCatch",
            local_call_with_body(
                vec![
                    node("Child_Start", ProcessNodeKind::Start),
                    message_catch(),
                    node("Child_End", ProcessNodeKind::End),
                ],
                vec![
                    edge("Child_Flow1", "Child_Start", "Child_Catch"),
                    edge("Child_Flow2", "Child_Catch", "Child_End"),
                ],
                declare_message,
            ),
        ),
        (
            "SignalCatch",
            local_call_with_body(
                vec![
                    node("Child_Start", ProcessNodeKind::Start),
                    node(
                        "Child_Catch",
                        ProcessNodeKind::SignalCatch {
                            signal_ref: "Signal_1".into(),
                            output_mapping: BTreeMap::new(),
                        },
                    ),
                    node("Child_End", ProcessNodeKind::End),
                ],
                vec![
                    edge("Child_Flow1", "Child_Start", "Child_Catch"),
                    edge("Child_Flow2", "Child_Catch", "Child_End"),
                ],
                |model| {
                    model.target_namespace = Some(NAMESPACE.into());
                    model.signals.push(ProcessSignalDeclaration {
                        signal_id: "Signal_1".into(),
                        namespace_uri: NAMESPACE.into(),
                        name: "Evidence".into(),
                    });
                },
            ),
        ),
        (
            "EventBasedGateway",
            local_call_with_body(
                vec![
                    node("Child_Start", ProcessNodeKind::Start),
                    node("Child_Race", ProcessNodeKind::EventBasedGateway),
                    message_catch(),
                    node(
                        "Child_Timer",
                        ProcessNodeKind::TimerCatch {
                            timer: ProcessTimerSpec::Duration { seconds: 2 },
                        },
                    ),
                    node("Child_End", ProcessNodeKind::End),
                ],
                vec![
                    edge("Child_Flow1", "Child_Start", "Child_Race"),
                    edge("Child_Flow2", "Child_Race", "Child_Catch"),
                    edge("Child_Flow3", "Child_Race", "Child_Timer"),
                    edge("Child_Flow4", "Child_Catch", "Child_End"),
                    edge("Child_Flow5", "Child_Timer", "Child_End"),
                ],
                |model| {
                    declare_message(model);
                    model.additional_processes[0].timer_timezone = Some("UTC".into());
                },
            ),
        ),
    ];
    for (name, model) in cases {
        let error = create_store(source_input_for(model))
            .err()
            .unwrap_or_else(|| panic!("{name} in a local body must be refused"));
        assert!(
            format!("{error:#}").starts_with(SIMULATION_CALL_UNSUPPORTED),
            "{name}: {error:#}"
        );
    }
}

#[test]
fn a_callee_document_whose_other_process_calls_a_third_version_still_starts() {
    let fixture = Fixture::new();
    let third = publish_model(&fixture, &user_model(Some(&fixture.owner.user_id)));
    // D2's main process is the one D1 calls; its additional process Q2 calls D3,
    // which D1 never reaches but production recorded as a dependency of D2.
    let mut second_model = user_model(Some(&fixture.owner.user_id));
    second_model
        .additional_processes
        .push(ProcessExecutableProcess {
            process_id: "Q2".into(),
            process_name: None,
            nodes: vec![
                node("Q2_Start", ProcessNodeKind::Start),
                call_node(
                    "Q2_Call",
                    ProcessCallTarget::PublishedBody {
                        definition_id: third.definition_id.clone(),
                        version: third.version,
                        called_element: reference(&third.model.process_id),
                    },
                    &[],
                ),
                node("Q2_End", ProcessNodeKind::End),
            ],
            sequence_flows: vec![
                edge("Q2_Flow1", "Q2_Start", "Q2_Call"),
                edge("Q2_Flow2", "Q2_Call", "Q2_End"),
            ],
            variables: BTreeMap::new(),
            diagram: Default::default(),
            timer_timezone: None,
            work_calendar: None,
            calendar_pin: None,
            modeling: None,
        });
    let second = publish_model(&fixture, &second_model);
    let first = publish_model(&fixture, &caller_of(&second));
    let mut store = started_from(&fixture, &first).expect("the run starts");
    let view = advance_until(&mut store, "the callee's task", |view| {
        !open_work(view).is_empty()
    });
    assert_eq!(view.instances.len(), 2, "D3 is pinned but never entered");
    let private = store.private_row_snapshot().expect("private rows");
    assert_eq!(
        private["bpmn_versions"].len(),
        3,
        "D1, D2 and the dependency D3"
    );
    let done = complete_open_work(&mut store, json!({"answer": "x"}));
    assert_eq!(
        done.instance.as_ref().expect("instance").status,
        ProcessInstanceStatus::Completed
    );
}

#[test]
fn a_callee_task_for_another_member_cannot_be_completed_by_the_simulating_owner() {
    use super::runtime::test_support::actor;
    let fixture = Fixture::new();
    let other = actor(&fixture.db, "other-assignee");
    let callee = publish_model(&fixture, &user_model(Some(&other.user_id)));
    let caller = publish_model(&fixture, &caller_of(&callee));
    let mut store = started_from(&fixture, &caller).expect("the run starts");
    let view = advance_until(&mut store, "the callee's task", |view| {
        view.user_tasks.iter().any(|task| task.node_id == "Work")
    });
    let task = view
        .user_tasks
        .iter()
        .find(|task| task.node_id == "Work")
        .expect("task");
    assert_eq!(task.assignee_user_id, other.user_id);
    // Only the assignee completes a task, in a call exactly as in a root step: the
    // simulation does not impersonate another member, so such a task waits. The
    // private organisation making every installed identity an admin gives no
    // shortcut either: completion is decided by the assignee, not by role.
    assert!(!task.can_complete);
    let error = store
        .complete_user_task(
            test_authorization(&store),
            &task.user_task_id,
            json!({"answer": "x"}),
            None,
        )
        .expect_err("the owner is not the assignee");
    assert!(
        format!("{error:#}").contains("assigned to another actor"),
        "{error:#}"
    );
    store
        .advance(test_authorization(&store))
        .expect("the run is not wedged");
    let root = super::simulation_tests::create_store(super::simulation_tests::source_input_for(
        user_model(Some("user-other")),
    ))
    .and_then(|mut root| {
        let authorization = test_authorization(&root);
        root.start(authorization, json!({}))?;
        Ok(root)
    })
    .expect("a root step for another member");
    let view = root.view(test_authorization(&root)).expect("view");
    assert!(!open_work(&view).is_empty() && !view.user_tasks[0].can_complete);
}

#[test]
fn a_childs_events_come_between_the_calls_entry_and_its_return() {
    let fixture = Fixture::new();
    let caller = publish_model(&fixture, &local_call_model());
    let variables = json!({"value": 1});
    let source = repository::capture_simulation_source(
        &fixture.db,
        &fixture.owner,
        &caller.definition_id,
        caller.version,
        &caller.model.process_id,
        "Start_1",
        &variables,
        1_000,
        20_000,
        100,
    )
    .expect("capture");
    let mut store = SimulationStore::create(source).expect("create");
    store
        .start(test_authorization(&store), variables)
        .expect("start");
    let done = advance_until(&mut store, "the run to finish", |view| {
        view.instance
            .as_ref()
            .is_some_and(|instance| instance.status == ProcessInstanceStatus::Completed)
    });
    let (parent, child) = (
        &done.instances[0].instance_id,
        &done.instances[1].instance_id,
    );
    let position = |kind: &str, instance: &str| {
        done.events
            .iter()
            .position(|event| event.kind == kind && event.instance_id == instance)
            .unwrap_or_else(|| panic!("no {kind} of {instance}"))
    };
    let entered = position("call_entered", parent);
    let started = position("instance_started", child);
    let completed = position("instance_completed", child);
    let returned = position("call_returned", parent);
    // The writer records the parent's `call_entered` after the child it started
    // (which here runs to its end within the same step); what must never happen is
    // the return shown before the child it returns from.
    assert!(
        started < completed && completed < returned && entered < returned,
        "the history is in the order the run wrote it: {entered} {started} {completed} {returned}"
    );
}

#[test]
fn an_interrupting_boundary_on_a_sub_process_with_a_call_cancels_the_whole_call_tree() {
    use super::simulation_scope_tests::{body, sub_process};
    use tentaflow_protocol::processes::ProcessTimerSpec;

    let fixture = Fixture::new();
    let callee = publish_model(&fixture, &user_model(Some(&fixture.owner.user_id)));
    let inner = body(
        vec![
            node("InnerStart", ProcessNodeKind::Start),
            call_node(
                "Call_1",
                ProcessCallTarget::PublishedBody {
                    definition_id: callee.definition_id.clone(),
                    version: callee.version,
                    called_element: reference(&callee.model.process_id),
                },
                &[],
            ),
            node("InnerEnd", ProcessNodeKind::End),
        ],
        vec![
            edge("InnerFlow1", "InnerStart", "Call_1"),
            edge("InnerFlow2", "Call_1", "InnerEnd"),
        ],
    );
    let mut model = starter_model();
    model.timer_timezone = Some("UTC".into());
    model.nodes = vec![
        model.nodes[0].clone(),
        node("Scope_1", sub_process(inner, &[], &[])),
        node(
            "Guard_1",
            ProcessNodeKind::BoundaryTimer {
                attached_to_id: "Scope_1".into(),
                cancel_activity: true,
                timer: ProcessTimerSpec::Duration { seconds: 1 },
            },
        ),
        node("End_H", ProcessNodeKind::End),
        model.nodes.last().expect("starter End").clone(),
    ];
    model.sequence_flows = vec![
        edge("ToScope", "Start_1", "Scope_1"),
        edge("ScopeEnd", "Scope_1", "End_1"),
        edge("ToHandler", "Guard_1", "End_H"),
    ];
    let caller = publish_model(&fixture, &model);
    let mut store = started_from(&fixture, &caller).expect("start the run");
    advance_until(&mut store, "the callee's task", |view| {
        !open_work(view).is_empty()
    });
    let done = advance_until(&mut store, "the timer to end the run", |view| {
        view.instance
            .as_ref()
            .is_some_and(|instance| instance.status == ProcessInstanceStatus::Completed)
    });
    assert_eq!(call_of(&done).status, ProcessCallStatus::Cancelled);
    assert_eq!(done.instances[1].status, ProcessInstanceStatus::Cancelled);
    assert!(
        open_work(&done).is_empty(),
        "nothing of the cancelled tree stays open"
    );
    assert!(done
        .scopes
        .iter()
        .filter(|scope| scope.parent_scope_id.is_some())
        .all(|scope| scope.status == ProcessInstanceStatus::Cancelled));
}

#[test]
fn an_error_crossing_two_call_levels_is_caught_by_the_outermost_boundary() {
    let fixture = Fixture::new();
    let rejected = || ProcessErrorDeclaration {
        error_id: "Rejected".into(),
        name: "Rejected".into(),
        error_code: "REJECTED".into(),
    };
    let mut inner_model = user_model(Some(&fixture.owner.user_id));
    inner_model.errors.push(rejected());
    inner_model.nodes.last_mut().expect("End").kind = ProcessNodeKind::ErrorEnd {
        error_ref: "Rejected".into(),
    };
    let inner = publish_model(&fixture, &inner_model);
    let middle = publish_model(&fixture, &caller_of(&inner));
    let mut outer_model = caller_of(&middle);
    outer_model.errors.push(rejected());
    outer_model.nodes.push(node(
        "Guard_1",
        ProcessNodeKind::BoundaryError {
            attached_to_id: "Call_1".into(),
            error_ref: Some("Rejected".into()),
            output_mapping: BTreeMap::from([("caught".into(), "outputs.answer".into())]),
        },
    ));
    outer_model.nodes.push(node("End_H", ProcessNodeKind::End));
    outer_model
        .sequence_flows
        .push(edge("ToHandlerEnd", "Guard_1", "End_H"));
    let outer = publish_model(&fixture, &outer_model);
    let mut store = started_from(&fixture, &outer).expect("start the run");
    advance_until(&mut store, "the innermost task", |view| {
        !open_work(view).is_empty()
    });
    complete_open_work(&mut store, json!({"answer": "no"}));
    let done = advance_until(&mut store, "the outer run to finish", |view| {
        view.instance
            .as_ref()
            .is_some_and(|instance| instance.status == ProcessInstanceStatus::Completed)
    });
    assert_eq!(done.instances.len(), 3);
    assert!(done
        .events
        .iter()
        .any(|event| event.kind == "business_error_caught"));
    assert_eq!(
        done.instance.as_ref().expect("instance").variables["caught"],
        json!("no")
    );
    assert_eq!(done.instances[2].status, ProcessInstanceStatus::Error);
}

#[test]
fn a_repetition_inside_a_called_process_runs_in_the_child() {
    use super::simulation_repetition_tests::repeated_task_model;
    let fixture = Fixture::new();
    let callee = publish_model(
        &fixture,
        &repeated_task_model(
            Some(&fixture.owner.user_id),
            ProcessRepeatSpec::MultiInstance {
                mode: ProcessMultiInstanceMode::Parallel,
                input: ProcessMultiInstanceInput::Cardinality { count: 2 },
                output_collection_variable: "results".into(),
            },
        ),
    );
    let mut caller_model = caller_of(&callee);
    if let ProcessNodeKind::CallActivity(call) = &mut caller_model.nodes[1].kind {
        call.output_mapping = BTreeMap::from([("collected".into(), "outputs.results".into())]);
    }
    let caller = publish_model(&fixture, &caller_model);
    let mut store = started_from(&fixture, &caller).expect("start the run");
    let view = advance_until(&mut store, "two tasks of the child", |view| {
        open_work(view).len() == 2
    });
    assert_eq!(view.repetition_groups.len(), 1);
    assert_eq!(view.repetition_occurrences.len(), 2);
    assert_eq!(view.instances.len(), 2);
    let mut view = view;
    for _ in 0..2 {
        view = complete_open_work(&mut store, json!({"answer": "x"}));
    }
    let instance = view.instance.as_ref().expect("instance");
    assert_eq!(
        instance.status,
        ProcessInstanceStatus::Completed,
        "{:?}",
        view.incidents
    );
    assert_eq!(
        instance.variables["collected"].as_array().map(Vec::len),
        Some(2)
    );
}

#[test]
fn the_simulated_and_the_called_process_carry_the_names_of_their_definitions() {
    let fixture = Fixture::new();
    let callee = publish_model(&fixture, &user_model(Some(&fixture.owner.user_id)));
    let caller = publish_model(&fixture, &caller_of(&callee));
    let mut store = started_from(&fixture, &caller).expect("start the run");
    let view = advance_until(&mut store, "the child to start", |view| view.instances.len() == 2);
    assert!(
        view.instances.iter().all(|instance| instance.definition_name == "Evidence process"),
        "{:?}",
        view.instances
    );
}

#[test]
fn a_called_step_with_a_result_expression_says_so_on_its_own_wait() {
    use super::runtime::test_support::{flow, graph, service_model};
    use tentaflow_protocol::processes::ActivityVerification;

    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("marker", None));
    let mut callee_model = service_model(&flow_id, ActivityVerification::Human);
    if let ProcessNodeKind::ServiceTask { result_expression, .. } = &mut callee_model.nodes[1].kind {
        *result_expression = Some("outputs.result".into());
    }
    let callee = publish_model(&fixture, &callee_model);
    let caller = publish_model(&fixture, &caller_of(&callee));
    let mut store = started_from(&fixture, &caller).expect("start the run");
    let view = advance_until(&mut store, "the Service wait", |view| {
        !view.service_waits.is_empty()
    });
    assert!(
        view.service_waits[0].has_result_expression,
        "the caller has no step with a result expression, so only the wait can say it"
    );
}

#[test]
fn the_same_service_node_waiting_in_two_instances_is_told_apart_by_job() {
    use super::runtime::test_support::{flow, graph, service_model};
    use super::simulation::SERVICE_AMBIGUOUS;
    use tentaflow_protocol::processes::{ActivityOutcome, ActivityResult, ActivityVerification};

    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("marker", None));
    let verification = || ActivityVerification::Condition {
        expression: "true".into(),
    };
    let mut callee_model = service_model(&flow_id, verification());
    callee_model.nodes[1].name = "Child service".into();
    let callee = publish_model(&fixture, &callee_model);
    // The caller runs the same node id ("Service") beside its call.
    let mut model = service_model(&flow_id, verification());
    let mut service = model.nodes[1].clone();
    service.name = "Parent service".into();
    model.nodes = vec![
        model.nodes[0].clone(),
        node("Split", ProcessNodeKind::ParallelGateway),
        service,
        call_node(
            "Call_1",
            ProcessCallTarget::PublishedBody {
                definition_id: callee.definition_id.clone(),
                version: callee.version,
                called_element: reference(&callee.model.process_id),
            },
            &[("result", "outputs.answer")],
        ),
        node("Join", ProcessNodeKind::ParallelGateway),
        model.nodes.last().expect("End").clone(),
    ];
    model.sequence_flows = vec![
        edge("A", "Start_1", "Split"),
        edge("B", "Split", "Service"),
        edge("C", "Split", "Call_1"),
        edge("D", "Service", "Join"),
        edge("E", "Call_1", "Join"),
        edge("F", "Join", "End_1"),
    ];
    let caller = publish_model(&fixture, &model);
    let mut store = started_from(&fixture, &caller).expect("start the run");
    let view = advance_until(&mut store, "two Service waits", |view| {
        view.service_waits.len() == 2
    });
    assert_eq!(view.service_waits[0].node_id, view.service_waits[1].node_id);
    assert_ne!(
        view.service_waits[0].instance_id,
        view.service_waits[1].instance_id
    );
    let answer = || ActivityResult {
        outcome: ActivityOutcome::Completed,
        code: None,
        summary: "answered".into(),
        outputs: json!({"variables": {"marker": "m"}}),
        evidence: Vec::new(),
    };
    let error = store
        .resolve_service(test_authorization(&store), "Service", None, answer(), None)
        .expect_err("two jobs wait on that node id");
    assert_eq!(refusal_code(&error), SERVICE_AMBIGUOUS, "{error:#}");
    let child = view.instances[1].instance_id.clone();
    for wait in &view.service_waits {
        let expected = if wait.instance_id == child { "Child service" } else { "Parent service" };
        assert_eq!(
            wait.node_name.as_deref(),
            Some(expected),
            "a wait is named from the model of the instance that owns it"
        );
    }
    let child_event_names = view
        .events
        .iter()
        .filter(|event| event.instance_id == child && event.node_id.as_deref() == Some("Service"))
        .map(|event| event.node_name.clone())
        .collect::<Vec<_>>();
    assert!(!child_event_names.is_empty());
    assert!(
        child_event_names.iter().all(|name| name.as_deref() == Some("Child service")),
        "{child_event_names:?}"
    );
    for wait in [&view.service_waits[0], &view.service_waits[1]] {
        store
            .resolve_service(
                test_authorization(&store),
                "Service",
                Some(&wait.job_id),
                answer(),
                None,
            )
            .expect("resolve by job id");
    }
    let done = advance_until(&mut store, "the run to finish", |view| {
        view.instance
            .as_ref()
            .is_some_and(|instance| instance.status == ProcessInstanceStatus::Completed)
    });
    assert!(done.service_waits.is_empty());
}

#[test]
fn a_process_that_calls_itself_cannot_be_published_so_the_simulation_never_sees_it() {
    let fixture = Fixture::new();
    // Two local bodies that call each other.
    let mut model = local_call_model();
    model.additional_processes[0].nodes = vec![
        node("Child_Start", ProcessNodeKind::Start),
        call_node(
            "Back_Call",
            ProcessCallTarget::LocalBody {
                called_element: reference(&model.process_id),
            },
            &[],
        ),
        node("Child_End", ProcessNodeKind::End),
    ];
    model.additional_processes[0].sequence_flows = vec![
        edge("Child_Flow1", "Child_Start", "Back_Call"),
        edge("Child_Flow2", "Back_Call", "Child_End"),
    ];
    let definition = repository::save_definition(
        &fixture.db,
        &fixture.owner,
        &stamp("create"),
        None,
        0,
        "Recursive",
        "",
        &model,
    );
    let published = definition.and_then(|definition| {
        let snapshots = crate::dispatch::processes::pin_service_snapshots(
            fixture.dispatcher(),
            &fixture.owner,
            &model,
        )?;
        repository::publish_definition(
            &fixture.db,
            &fixture.owner,
            &stamp("publish"),
            &definition.definition_id,
            definition.draft_revision,
            &snapshots,
            None,
        )
    });
    assert!(
        published.is_err(),
        "a cyclic local call must not be publishable"
    );
    let listed = repository::list_definitions(&fixture.db, &fixture.owner, 0, 50)
        .expect("definitions")
        .0;
    for summary in listed {
        let error = repository::capture_simulation_source(
            &fixture.db,
            &fixture.owner,
            &summary.definition_id,
            1,
            &model.process_id,
            "Start_1",
            &json!({}),
            1_000,
            20_000,
            100,
        )
        .err()
        .expect("an unpublished process cannot be simulated");
        let _ = error;
    }
}

#[test]
fn a_plan_naming_a_call_or_child_the_allocator_never_issued_is_refused() {
    use super::repository::{CallRequest, RuntimePlan};
    use super::simulation::{validate_plan_ids, vet_call_step_plan, SimulationIdSource};
    use std::collections::HashSet;

    let mut ids = SimulationIdSource::new(&"c".repeat(64)).expect("allocator");
    let mut issue = || super::runtime::RuntimeIdSource::next_id(&mut ids, "runtime");
    let (call_id, child_id) = (issue(), issue());
    let request = |call: &str, child: &str| CallRequest {
        call_id: call.into(),
        parent_scope_id: "scope".into(),
        parent_token_id: "token".into(),
        parent_arrival_edge_id: "edge".into(),
        call_node_id: "Call_1".into(),
        child_instance_id: child.into(),
        variables: json!({}),
    };
    let planned = |call: &str, child: &str| {
        let mut plan = RuntimePlan::initial(json!({}));
        plan.call_requests.push(request(call, child));
        plan
    };
    validate_plan_ids(&planned(&call_id, &child_id), &ids, &HashSet::new()).expect("issued ids");
    let random = uuid::Uuid::new_v4().to_string();
    for (case, plan) in [
        ("call", planned(&random, &child_id)),
        ("called instance", planned(&call_id, &random)),
    ] {
        let error = validate_plan_ids(&plan, &ids, &HashSet::new()).expect_err(case);
        assert!(
            format!("{error:#}").contains(&format!("{case} identifier not issued")),
            "{error:#}"
        );
    }
    // A plan the repository made itself must carry derived (v5) identifiers only.
    vet_call_step_plan(&planned(&call_id, &child_id), &[]).expect("derived ids");
    let error = vet_call_step_plan(&planned(&call_id, &random), &[]).expect_err("a random id");
    assert!(format!("{error:#}").contains("not derived"), "{error:#}");
}
