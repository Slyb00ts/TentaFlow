// ============ File: simulation_service_tests.rs — simulated Service steps, verification and production parity ============

use std::collections::BTreeMap;

use serde_json::{json, Value};
use tentaflow_protocol::processes::{
    ActivityOutcome, ActivityResult, ActivityVerification, ProcessInstanceStatus, ProcessModel,
    ProcessNode, ProcessNodeKind, ProcessSimulationServiceFixture, ProcessSimulationServiceMode,
    ProcessSimulationVerdict, ProcessUserTaskKind, ServiceImplementation, VerificationOnFailure,
};

use super::model::starter_model;
use super::runtime::test_support::edge;
use super::simulation::{
    SimulationServiceError, SimulationStore, SimulationView, SERVICE_FIXTURE_INVALID,
    SERVICE_NOT_WAITING, SERVICE_REAL_EXECUTION_UNSUPPORTED, SERVICE_SIDE_EFFECTS_NOT_CONFIRMED,
    SERVICE_VERDICT_REQUIRED,
};
use super::simulation_tests::{create_store, source_input_for, test_authorization};

const SERVICE: &str = "Service_1";

fn node(id: &str, kind: ProcessNodeKind) -> ProcessNode {
    ProcessNode {
        id: id.into(),
        name: format!("Step {id}"),
        kind,
        repeat: None,
        activity_io: None,
    }
}

fn service_kind(
    verification: ActivityVerification,
    result_expression: Option<&str>,
) -> ProcessNodeKind {
    ProcessNodeKind::ServiceTask {
        flow_id: "flow-simulation-service".into(),
        input_mapping: BTreeMap::from([("seed".into(), "vars.answer".into())]),
        output_mapping: BTreeMap::from([("service_answer".into(), "outputs.answer".into())]),
        verification,
        timeout_seconds: 30,
        result_expression: result_expression.map(str::to_owned),
        implementation: ServiceImplementation::default(),
    }
}

/// Start, a script, a Service task, a user task and End: the path a process
/// takes when a step is handed to a flow in the middle of human work.
fn chain_model(
    verification: ActivityVerification,
    result_expression: Option<&str>,
    implementation: ServiceImplementation,
) -> ProcessModel {
    let mut model = starter_model();
    let mut service = service_kind(verification, result_expression);
    if let ProcessNodeKind::ServiceTask { implementation: slot, flow_id, .. } = &mut service {
        // Only a flow step names a flow; a tool or an unbound step must not.
        if !matches!(implementation, ServiceImplementation::Flow) {
            flow_id.clear();
        }
        *slot = implementation;
    }
    model.nodes = vec![
        model.nodes[0].clone(),
        node(
            "Script_1",
            ProcessNodeKind::ScriptTask {
                script: "vars.value + 1".into(),
                output_mapping: BTreeMap::from([("answer".into(), "outputs".into())]),
            },
        ),
        node(SERVICE, service),
        node(
            "User_1",
            ProcessNodeKind::UserTask {
                assignee_user_id: Some("user-simulation".into()),
                output_mapping: BTreeMap::new(),
            },
        ),
        model.nodes.last().expect("starter End").clone(),
    ];
    model.sequence_flows = vec![
        edge("ToScript", "Start_1", "Script_1"),
        edge("ToService", "Script_1", SERVICE),
        edge("ToUser", SERVICE, "User_1"),
        edge("ToEnd", "User_1", "End_1"),
    ];
    model
}

fn human_chain() -> ProcessModel {
    chain_model(ActivityVerification::Human, None, ServiceImplementation::default())
}

fn completed(outputs: Value) -> ActivityResult {
    ActivityResult {
        outcome: ActivityOutcome::Completed,
        code: None,
        summary: "The pinned flow returned an activity result".into(),
        outputs,
        evidence: Vec::new(),
    }
}

fn authorize(store: &SimulationStore) -> super::simulation::AuthenticatedSimulationAction {
    test_authorization(store)
}

fn started(model: ProcessModel) -> SimulationStore {
    let mut store = create_store(source_input_for(model)).expect("capture the source");
    store
        .start(authorize(&store), json!({"value": 1}))
        .expect("start the run");
    store
}

/// Advances until the first Service step waits, or the run stops moving.
fn drive_to_service(store: &mut SimulationStore) -> SimulationView {
    let mut view = store.view(authorize(store)).expect("read the run");
    for _ in 0..8 {
        if !view.service_waits.is_empty() {
            break;
        }
        view = store.advance(authorize(store)).expect("advance the run");
    }
    view
}

fn kinds(view: &SimulationView) -> Vec<String> {
    view.events
        .iter()
        .map(|event| format!("{}@{}", event.kind, event.node_id.as_deref().unwrap_or("-")))
        .collect()
}

fn service_error(error: &anyhow::Error) -> &'static str {
    error
        .downcast_ref::<SimulationServiceError>()
        .unwrap_or_else(|| panic!("a typed Service refusal was expected, got {error:#}"))
        .code
}

fn fixture(node_id: &str, mode: ProcessSimulationServiceMode) -> ProcessSimulationServiceFixture {
    ProcessSimulationServiceFixture {
        node_id: node_id.into(),
        mode,
        result: None,
        verdict: None,
        confirm_side_effects: false,
    }
}

#[test]
fn a_service_step_waits_and_the_operator_answer_continues_the_run_like_production() {
    let mut store = started(human_chain());
    let waiting = drive_to_service(&mut store);
    assert_eq!(waiting.service_waits.len(), 1);
    let wait = &waiting.service_waits[0];
    assert_eq!(wait.node_id, SERVICE);
    assert_eq!(wait.mode, ProcessSimulationServiceMode::Result);
    assert!(!wait.verdict_applicable);
    assert_eq!(wait.input["variables"], json!({"seed": 2}));
    assert_eq!(
        waiting.instance.as_ref().expect("instance").status,
        ProcessInstanceStatus::Running
    );

    let resolved = store
        .resolve_service(authorize(&store), SERVICE, completed(json!({"answer": 42})), None)
        .expect("resolve the waiting step");
    assert!(resolved.service_waits.is_empty());
    let trail = kinds(&resolved);
    for expected in ["service_queued@Service_1", "service_claimed@Service_1", "service_result@Service_1"] {
        assert!(trail.iter().any(|kind| kind == expected), "{expected} in {trail:?}");
    }
    // Human verification: the completed result goes to a person before it counts.
    let verification = resolved
        .user_tasks
        .iter()
        .find(|task| task.kind == ProcessUserTaskKind::Verification)
        .expect("a human verification task opens");
    let verification_id = verification.user_task_id.clone();
    let approved = store
        .complete_user_task(authorize(&store), &verification_id, json!({}), Some(true))
        .expect("approve the verification");
    assert_eq!(approved.instance.as_ref().expect("instance").variables["service_answer"], 42);
    let work = approved
        .user_tasks
        .iter()
        .find(|task| task.kind == ProcessUserTaskKind::Work && task.status == tentaflow_protocol::processes::ProcessUserTaskStatus::Open)
        .expect("the user task after the Service step opens");
    let done = store
        .complete_user_task(authorize(&store), &work.user_task_id.clone(), json!({"ok": true}), None)
        .expect("complete the user task");
    assert_eq!(
        done.instance.as_ref().expect("instance").status,
        ProcessInstanceStatus::Completed
    );
}

#[test]
fn a_repeated_run_has_the_same_ids_and_trace() {
    let run = || {
        let mut store = started(human_chain());
        drive_to_service(&mut store);
        let view = store
            .resolve_service(authorize(&store), SERVICE, completed(json!({"answer": 7})), None)
            .expect("resolve");
        let events = view
            .events
            .iter()
            .map(|event| (event.event_id.clone(), event.kind.clone(), event.data.clone()))
            .collect::<Vec<_>>();
        let trace = view
            .trace_steps
            .iter()
            .map(|step| {
                (
                    step.trace_step_id.clone(),
                    step.request_sha256.clone(),
                    step.result_sha256.clone(),
                )
            })
            .collect::<Vec<_>>();
        (events, trace)
    };
    let (first_events, first_trace) = run();
    let (second_events, second_trace) = run();
    assert_eq!(first_events, second_events);
    assert_eq!(first_trace, second_trace);
}

#[test]
fn a_prepared_fixture_answers_on_advance_without_the_operator() {
    let mut store = create_store(source_input_for(human_chain())).expect("capture");
    store
        .set_service_fixtures(&[ProcessSimulationServiceFixture {
            result: Some(completed(json!({"answer": 9}))),
            ..fixture(SERVICE, ProcessSimulationServiceMode::Fixture)
        }])
        .expect("store the fixture");
    store.start(authorize(&store), json!({"value": 1})).expect("start");
    let waiting = drive_to_service(&mut store);
    assert_eq!(waiting.service_waits[0].mode, ProcessSimulationServiceMode::Fixture);
    let answered = store.advance(authorize(&store)).expect("the fixture is applied");
    assert!(answered.service_waits.is_empty());
    assert!(kinds(&answered).contains(&"service_result@Service_1".to_owned()));
}

#[test]
fn fixtures_are_refused_with_stable_codes_and_real_execution_never_runs() {
    let cases: Vec<(Vec<ProcessSimulationServiceFixture>, &str)> = vec![
        (vec![fixture("Nope", ProcessSimulationServiceMode::Fixture)], SERVICE_FIXTURE_INVALID),
        (vec![fixture(SERVICE, ProcessSimulationServiceMode::Fixture)], SERVICE_FIXTURE_INVALID),
        (
            vec![ProcessSimulationServiceFixture {
                result: Some(completed(json!({}))),
                ..fixture(SERVICE, ProcessSimulationServiceMode::Result)
            }],
            SERVICE_FIXTURE_INVALID,
        ),
        (
            vec![
                fixture(SERVICE, ProcessSimulationServiceMode::Result),
                fixture(SERVICE, ProcessSimulationServiceMode::Result),
            ],
            SERVICE_FIXTURE_INVALID,
        ),
        (
            vec![fixture(SERVICE, ProcessSimulationServiceMode::RealExecution)],
            SERVICE_SIDE_EFFECTS_NOT_CONFIRMED,
        ),
        (
            vec![ProcessSimulationServiceFixture {
                confirm_side_effects: true,
                ..fixture(SERVICE, ProcessSimulationServiceMode::RealExecution)
            }],
            SERVICE_REAL_EXECUTION_UNSUPPORTED,
        ),
    ];
    for (fixtures, code) in cases {
        let mut store = create_store(source_input_for(human_chain())).expect("capture");
        let error = store
            .set_service_fixtures(&fixtures)
            .expect_err("the fixture set is refused");
        assert_eq!(service_error(&error), code, "{error:#}");
        assert!(format!("{error:#}").starts_with(code));
        // Nothing of a refused set is kept.
        let waiting = {
            store.start(authorize(&store), json!({"value": 1})).expect("start");
            drive_to_service(&mut store)
        };
        assert_eq!(waiting.service_waits[0].mode, ProcessSimulationServiceMode::Result);
    }
}

#[test]
fn resolving_a_step_that_does_not_wait_is_a_typed_refusal_and_changes_nothing() {
    let mut store = started(human_chain());
    drive_to_service(&mut store);
    let before = store.view(authorize(&store)).expect("view");
    let error = store
        .resolve_service(authorize(&store), "User_1", completed(json!({})), None)
        .expect_err("a user task is not a Service step");
    assert_eq!(service_error(&error), SERVICE_NOT_WAITING);
    let after = store.view(authorize(&store)).expect("view");
    assert_eq!(before.clock, after.clock);
    assert_eq!(before.events.len(), after.events.len());
}

#[test]
fn a_test_verification_needs_the_operators_verdict_and_follows_its_failure_policy() {
    let verification = |on_failure| ActivityVerification::Test {
        flow_id: "flow-verifier".into(),
        on_failure,
    };
    let mut store = started(chain_model(
        verification(VerificationOnFailure::Error),
        None,
        ServiceImplementation::default(),
    ));
    let waiting = drive_to_service(&mut store);
    assert!(waiting.service_waits[0].verdict_applicable);
    let error = store
        .resolve_service(authorize(&store), SERVICE, completed(json!({"answer": 1})), None)
        .expect_err("a completed result under a test needs a verdict");
    assert_eq!(service_error(&error), SERVICE_VERDICT_REQUIRED);
    assert_eq!(
        store.view(authorize(&store)).expect("view").service_waits.len(),
        1,
        "the step still waits after the refusal"
    );
    let approved = store
        .resolve_service(
            authorize(&store),
            SERVICE,
            completed(json!({"answer": 1})),
            Some(ProcessSimulationVerdict::Approved),
        )
        .expect("approved");
    assert!(kinds(&approved).contains(&"verification_passed@Service_1".to_owned()));
    assert!(approved
        .user_tasks
        .iter()
        .all(|task| task.kind != ProcessUserTaskKind::Verification));

    // A rejection under `NeedsHuman` hands the result to a person.
    let mut store = started(chain_model(
        verification(VerificationOnFailure::NeedsHuman),
        None,
        ServiceImplementation::default(),
    ));
    drive_to_service(&mut store);
    let rejected = store
        .resolve_service(
            authorize(&store),
            SERVICE,
            completed(json!({"answer": 1})),
            Some(ProcessSimulationVerdict::Rejected),
        )
        .expect("rejected");
    assert!(rejected
        .user_tasks
        .iter()
        .any(|task| task.kind == ProcessUserTaskKind::Verification));

    // A verdict for a result that is not verified at all is ignored: an error
    // result never reaches the verifier.
    let mut store = started(chain_model(
        verification(VerificationOnFailure::Error),
        None,
        ServiceImplementation::default(),
    ));
    drive_to_service(&mut store);
    let failed = store
        .resolve_service(
            authorize(&store),
            SERVICE,
            ActivityResult {
                outcome: ActivityOutcome::Error,
                code: Some("FLOW_ERROR".into()),
                summary: "the flow failed".into(),
                outputs: Value::Null,
                evidence: Vec::new(),
            },
            None,
        )
        .expect("an error result needs no verdict");
    assert_eq!(
        failed.instance.as_ref().expect("instance").status,
        ProcessInstanceStatus::Incident
    );
}

#[test]
fn a_rejected_human_verification_raises_the_production_incident() {
    let mut store = started(human_chain());
    drive_to_service(&mut store);
    let resolved = store
        .resolve_service(authorize(&store), SERVICE, completed(json!({"answer": 3})), None)
        .expect("resolve");
    let task_id = resolved
        .user_tasks
        .iter()
        .find(|task| task.kind == ProcessUserTaskKind::Verification)
        .expect("verification")
        .user_task_id
        .clone();
    let rejected = store
        .complete_user_task(authorize(&store), &task_id, json!({}), Some(false))
        .expect("reject");
    assert!(rejected.incidents.iter().any(|incident| incident.code == "HUMAN_REJECTED"));
    // A Work task takes no verdict and a verification needs one.
    let mut other = started(human_chain());
    drive_to_service(&mut other);
    let resolved = other
        .resolve_service(authorize(&other), SERVICE, completed(json!({"answer": 3})), None)
        .expect("resolve");
    let id = resolved.user_tasks[0].user_task_id.clone();
    let error = other
        .complete_user_task(authorize(&other), &id, json!({}), None)
        .expect_err("a verification needs approved");
    assert!(format!("{error:#}").contains("verification requires approved"));
}

#[test]
fn a_business_error_is_caught_by_the_error_boundary_of_the_step() {
    let mut model = chain_model(
        ActivityVerification::Condition { expression: "true".into() },
        Some(
            r#"{"outcome":"Error","code":"OUT_OF_STOCK","summary":"none left","outputs":{},"evidence":[]}"#,
        ),
        ServiceImplementation::default(),
    );
    model.nodes.push(node(
        "Boundary_1",
        ProcessNodeKind::BoundaryError {
            attached_to_id: SERVICE.into(),
            error_ref: None,
            output_mapping: BTreeMap::new(),
        },
    ));
    model.nodes.push(node("End_Caught", ProcessNodeKind::End));
    model.sequence_flows.push(edge("ToCaught", "Boundary_1", "End_Caught"));
    let mut store = started(model);
    drive_to_service(&mut store);
    let caught = store
        .resolve_service(authorize(&store), SERVICE, completed(json!({"answer": 1})), None)
        .expect("resolve");
    assert!(
        kinds(&caught).contains(&"business_error_caught@Boundary_1".to_owned()),
        "{:?}",
        kinds(&caught)
    );
    assert_eq!(
        caught.instance.as_ref().expect("instance").status,
        ProcessInstanceStatus::Completed
    );
}

#[test]
fn an_uncaught_business_error_ends_the_run_in_error_like_production() {
    let model = chain_model(
        ActivityVerification::Condition { expression: "true".into() },
        Some(
            r#"{"outcome":"Error","code":"OUT_OF_STOCK","summary":"none left","outputs":{},"evidence":[]}"#,
        ),
        ServiceImplementation::default(),
    );
    let mut store = started(model);
    drive_to_service(&mut store);
    let ended = store
        .resolve_service(authorize(&store), SERVICE, completed(json!({"answer": 1})), None)
        .expect("resolve");
    let status = ended.instance.as_ref().expect("instance").status.clone();
    assert!(
        matches!(status, ProcessInstanceStatus::Error | ProcessInstanceStatus::Incident),
        "{status:?} {:?}",
        kinds(&ended)
    );
}

#[test]
fn an_unbound_step_is_refused_before_the_run_exists() {
    let model = chain_model(
        ActivityVerification::Human,
        None,
        ServiceImplementation::Unbound { origin: "delegate:${bean}".into() },
    );
    let error = create_store(source_input_for(model)).expect_err("nothing to simulate");
    assert!(format!("{error:#}").contains("UNBOUND_IMPLEMENTATION"));
}

#[test]
fn the_plan_profile_refuses_started_jobs_foreign_nodes_and_unreserved_invocations() {
    use super::repository::{ProcessJob, RuntimePlan};
    use super::simulation::validate_supported_plan;

    let model = human_chain();
    let queued = |node_id: &str| ProcessJob {
        job_id: "10000000-0000-4000-8000-000000000001".into(),
        instance_id: "20000000-0000-4000-8000-000000000001".into(),
        scope_id: "20000000-0000-4000-8000-000000000001".into(),
        node_id: node_id.into(),
        token_id: "30000000-0000-4000-8000-000000000001".into(),
        input: json!({}),
        status: "queued".into(),
        attempt: 0,
        fence: 0,
        worker_id: None,
        lease_until_ms: None,
        result: None,
        result_origin: None,
        observed_result_event_id: None,
    };
    let plan_with = |job: ProcessJob, reserve: bool| {
        let mut plan = RuntimePlan::initial(Value::Null);
        if reserve {
            plan.service_invocation_ids
                .insert(job.job_id.clone(), "40000000-0000-4000-8000-000000000001".into());
        }
        plan.create_jobs.push(job);
        plan
    };
    validate_supported_plan(&plan_with(queued(SERVICE), true), &model.nodes)
        .expect("a queued job of a bound Service step is the waiting state");

    // Each sign that a job has been taken is enough on its own.
    let started: [fn(&mut ProcessJob); 5] = [
        |job| job.status = "running".into(),
        |job| job.attempt = 1,
        |job| job.fence = 1,
        |job| job.worker_id = Some("production-worker".into()),
        |job| job.lease_until_ms = Some(30_000),
    ];
    for mark in started {
        let mut job = queued(SERVICE);
        mark(&mut job);
        let error = validate_supported_plan(&plan_with(job, true), &model.nodes)
            .expect_err("a claimed job is an external dispatch");
        assert!(format!("{error:#}").contains("started Service job"), "{error:#}");
    }

    let error = validate_supported_plan(&plan_with(queued("Script_1"), true), &model.nodes)
        .expect_err("a job for a script is not a Service wait");
    assert!(format!("{error:#}").contains("not a bound Service task"), "{error:#}");

    let error = validate_supported_plan(&plan_with(queued(SERVICE), false), &model.nodes)
        .expect_err("a job without its reserved invocation");
    assert!(format!("{error:#}").contains("reserves Service invocations"), "{error:#}");
}

#[tokio::test]
async fn the_simulated_trace_equals_the_production_execution_on_the_same_answer() {
    use super::repository;
    use super::runtime::test_support::{start_model, stamp};
    use super::tool_host::test_support::ScriptedToolHost;
    use super::tool_task_tests::{fixture_with, now_ms, run};

    const ADDON: &str = "notes-1a2b3c4d";
    const TOOL: &str = "search";

    let mut model = chain_model(
        ActivityVerification::Condition { expression: "true".into() },
        None,
        ServiceImplementation::AddonTool { addon_id: ADDON.into(), tool_name: TOOL.into() },
    );
    // Production starts from the model's own defaults; the simulation is given the same.
    model.variables.insert("value".into(), json!(1));
    for node in &mut model.nodes {
        match &mut node.kind {
            ProcessNodeKind::ServiceTask { output_mapping, input_mapping, .. } => {
                // The scripted tool declares a `query` argument.
                *input_mapping = BTreeMap::from([("query".into(), "\"cats\"".into())]);
                *output_mapping = BTreeMap::from([("service_answer".into(), "outputs.result.hits".into())]);
            }
            ProcessNodeKind::UserTask { assignee_user_id, .. } => *assignee_user_id = None,
            _ => {}
        }
    }

    let normalized = |events: Vec<(String, Option<String>, Value)>| {
        fn mask(value: &Value) -> Value {
            match value {
                Value::String(text) if uuid::Uuid::parse_str(text).is_ok() => json!("<uuid>"),
                Value::Array(items) => Value::Array(items.iter().map(mask).collect()),
                Value::Object(map) => Value::Object(
                    map.iter()
                        .map(|(key, value)| {
                            // Account identities differ between a live organization and a private one.
                            let masked = if key.ends_with("_user_id") { json!("<user>") } else { mask(value) };
                            (key.clone(), masked)
                        })
                        .collect(),
                ),
                other => other.clone(),
            }
        }
        events
            .into_iter()
            .map(|(kind, node, data)| (kind, node, mask(&data)))
            .collect::<Vec<_>>()
    };

    // Production: the real worker pipeline around a scripted addon tool.
    let host = ScriptedToolHost::new(json!({"hits": 3}));
    let production = fixture_with(&host);
    let instance = start_model(&production, &model);
    // The runtime's own tick advances ready tokens; here the same canonical
    // transition is applied by hand until the Service step is queued.
    for _ in 0..4 {
        let current = repository::runtime_snapshot(&production.db, &production.owner, &instance.instance_id)
            .expect("production snapshot");
        if current.jobs.iter().any(|job| job.status == "queued") {
            break;
        }
        repository::apply_transition(
            &production.db,
            &production.owner,
            &instance.instance_id,
            current.instance.revision,
            repository::ProcessPlanInput::Canonical,
            now_ms(),
        )
        .expect("advance the production run");
    }
    let queued = repository::runtime_snapshot(&production.db, &production.owner, &instance.instance_id)
        .expect("production snapshot");
    assert!(
        queued.jobs.iter().any(|job| job.status == "queued"),
        "production queued no job: {:?} {:?}",
        queued.instance.status,
        repository::list_events(&production.db, &production.owner, &instance.instance_id, 0, 50)
            .expect("history")
            .0
            .iter()
            .map(|event| event.kind.clone())
            .collect::<Vec<_>>()
    );
    run(&production, "parity-worker").await;
    let snapshot = repository::runtime_snapshot(&production.db, &production.owner, &instance.instance_id)
        .expect("production snapshot");
    let task = snapshot.user_tasks.iter().find(|task| task.kind == ProcessUserTaskKind::Work)
        .unwrap_or_else(|| panic!(
            "production opens the user task: {:?} {:?} {:?}",
            snapshot.instance.status,
            snapshot.tokens.iter().map(|token| format!("{}:{}", token.node_id, token.status)).collect::<Vec<_>>(),
            repository::list_events(&production.db, &production.owner, &instance.instance_id, 0, 50)
                .expect("history").0.iter().map(|event| event.kind.clone()).collect::<Vec<_>>()
        ));
    repository::complete_user_task(
        &production.db,
        &production.owner,
        &stamp("complete"),
        &instance.instance_id,
        &task.user_task_id,
        snapshot.instance.revision,
        &json!({"ok": true}),
        None,
        repository::ProcessPlanInput::Canonical,
        now_ms(),
    )
    .expect("complete the production user task");
    let production_events = normalized(
        repository::list_events(&production.db, &production.owner, &instance.instance_id, 0, 200)
            .expect("production history")
            .0
            .into_iter()
            .map(|event| (event.kind, event.node_id, event.data))
            .collect(),
    );

    // Simulation: the answer the scripted tool produced is given by the operator.
    let mut store = started(model);
    drive_to_service(&mut store);
    let answer = ActivityResult {
        outcome: ActivityOutcome::Completed,
        code: None,
        summary: "The addon tool returned a result".into(),
        outputs: json!({"result": {"hits": 3}}),
        evidence: vec![format!("addon_tool:{}", tentaflow_protocol::processes::tool_source_ref(ADDON, TOOL))],
    };
    let waiting = store
        .resolve_service(authorize(&store), SERVICE, answer, None)
        .expect("resolve the simulated step");
    let task_id = waiting.user_tasks.iter().find(|task| task.kind == ProcessUserTaskKind::Work)
        .expect("the simulation opens the user task")
        .user_task_id
        .clone();
    let done = store
        .complete_user_task(authorize(&store), &task_id, json!({"ok": true}), None)
        .expect("complete the simulated user task");
    let simulated_events = normalized(
        done.events
            .iter()
            .map(|event| (event.kind.clone(), event.node_id.clone(), event.data.clone()))
            .collect(),
    );

    assert_eq!(simulated_events, production_events);
    assert_eq!(
        done.instance.as_ref().expect("instance").variables,
        repository::get_instance(&production.db, &production.owner, &instance.instance_id, None)
            .expect("production instance")
            .variables
    );
}
