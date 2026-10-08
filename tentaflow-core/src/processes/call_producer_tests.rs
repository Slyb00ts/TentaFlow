// ============ File: processes/call_producer_tests.rs — preserve called-process return and termination guarantees across durable producer transitions ============

use super::{call_tests, jobs, messages, repository, runtime, timers};
use runtime::test_support::{edge, flow, graph, publish_model, service_model, Fixture};
use serde_json::json;
use std::collections::BTreeMap;
use tentaflow_protocol::processes::{
    ActivityVerification, ProcessActivityIo, ProcessCallActivity, ProcessErrorDeclaration,
    ProcessInputAssociation, ProcessInstanceStatus, ProcessIoDataInput, ProcessMessageStatus,
    ProcessNode, ProcessNodeKind, ProcessTimerSpec, ProcessUserTaskKind, ProcessUserTaskStatus,
};

fn return_count(fixture: &Fixture, instance_id: &str) -> usize {
    repository::list_events(&fixture.db, &fixture.owner, instance_id, 0, 200)
        .unwrap()
        .0
        .iter()
        .filter(|event| event.kind == "call_returned")
        .count()
}

#[test]
fn configured_activity_io_publishes_with_its_mapping_writer() {
    let fixture = Fixture::new();
    let mut model = runtime::test_support::user_model(None);
    model.nodes[1].activity_io = Some(ProcessActivityIo {
        data_inputs: vec![ProcessIoDataInput { id: "Input_1".into(), name: None }],
        data_outputs: vec![],
        input_set_id: "InputSet_1".into(),
        input_set: vec!["Input_1".into()],
        output_set_id: "OutputSet_1".into(),
        output_set: vec![],
        input_associations: vec![ProcessInputAssociation::CelAssignment {
            id: "InputAssociation_1".into(),
            from_expression: "1".into(),
            target_input_id: "Input_1".into(),
        }],
        output_associations: vec![],
        coordinator_output: None,
    });
    let definition = repository::save_definition(
        &fixture.db, &fixture.owner, &runtime::test_support::stamp("save configured IO"),
        None, 0, "Configured IO", "", &model,
    ).unwrap();
    let (_, version) = repository::publish_definition(
        &fixture.db, &fixture.owner, &runtime::test_support::stamp("publish configured IO"),
        &definition.definition_id, definition.draft_revision, &[], None,
    ).unwrap();
    assert!(version.model.nodes[1].activity_io.is_some());
    assert_eq!(repository::list_versions(&fixture.db, &fixture.owner,
        &definition.definition_id, 0, 20).unwrap().0.len(), 1);
}

#[test]
fn child_timer_catch_fires_and_returns_parent_once() {
    let fixture = Fixture::new();
    let mut child_model = super::model::starter_model();
    child_model.timer_timezone = Some("UTC".into());
    child_model.nodes.insert(
        1,
        ProcessNode {
            id: "Wait".into(),
            name: "Wait for evidence deadline".into(),
            kind: ProcessNodeKind::TimerCatch {
                timer: ProcessTimerSpec::Duration { seconds: 1 },
            },
            repeat: None,
            activity_io: None,
        },
    );
    child_model.sequence_flows = vec![
        edge("ToWait", "Start_1", "Wait"),
        edge("WaitEnd", "Wait", "End_1"),
    ];
    let target = publish_model(&fixture, &child_model);
    let caller = publish_model(&fixture, &call_tests::caller(&target, BTreeMap::new()));
    let parent = messages::test_support::start_version(&fixture, &caller);
    let child_id = call_tests::child_id(&fixture, &parent.instance_id);
    let child = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    assert_eq!(child.instance.status, ProcessInstanceStatus::Waiting);
    assert_eq!(parent.status, ProcessInstanceStatus::Waiting);
    assert_eq!(child.timers.len(), 1);
    let due = child.timers[0].due_at_ms.unwrap();
    let drained = timers::drain_due(&fixture.db, due);
    drained.completion.unwrap();
    assert_eq!(drained.fired, 1);
    assert!(drained.cancelled_claims.is_empty());
    assert_eq!(
        repository::get_instance(&fixture.db, &fixture.owner, &child_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Completed
    );
    assert_eq!(
        repository::get_instance(&fixture.db, &fixture.owner, &parent.instance_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Completed
    );
    assert_eq!(return_count(&fixture, &parent.instance_id), 1);
    let repeated = timers::drain_due(&fixture.db, due + 1);
    repeated.completion.unwrap();
    assert_eq!(repeated.fired, 0);
    assert_eq!(return_count(&fixture, &parent.instance_id), 1);
}

#[test]
fn child_message_catch_delivers_to_exact_subscription_and_returns_parent_once() {
    let fixture = Fixture::new();
    let child_model = messages::test_support::receiving_model(false, false);
    let target = publish_model(&fixture, &child_model);
    let caller = publish_model(&fixture, &call_tests::caller(&target, BTreeMap::new()));
    let parent = messages::test_support::start_version(&fixture, &caller);
    let child_id = call_tests::child_id(&fixture, &parent.instance_id);
    let child = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    assert_eq!(child.instance.status, ProcessInstanceStatus::Waiting);
    assert_eq!(parent.status, ProcessInstanceStatus::Waiting);
    assert_eq!(child.subscriptions.len(), 1);
    let message = messages::test_support::envelope(
        messages::test_support::catch_target(
            &target,
            Some(&child_id),
            Some(&child.subscriptions[0].subscription_id),
        ),
        json!({"actual_evidence_ID": "case-1"}),
    );
    let admitted = messages::test_support::send(&fixture, &message);
    assert_eq!(admitted.status, ProcessMessageStatus::Pending);
    let drained = messages::drain_pending(&fixture.db, admitted.received_at_ms);
    drained.completion.unwrap();
    assert_eq!(drained.delivered, 1);
    assert_eq!(
        messages::test_support::current(&fixture, &message)
            .message
            .status,
        ProcessMessageStatus::Delivered
    );
    assert_eq!(
        repository::get_instance(&fixture.db, &fixture.owner, &child_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Completed
    );
    assert_eq!(
        repository::get_instance(&fixture.db, &fixture.owner, &parent.instance_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Completed
    );
    assert_eq!(return_count(&fixture, &parent.instance_id), 1);
    let repeated = messages::drain_pending(&fixture.db, admitted.received_at_ms + 1);
    repeated.completion.unwrap();
    assert_eq!(repeated.delivered, 0);
    assert_eq!(return_count(&fixture, &parent.instance_id), 1);
}

#[tokio::test]
async fn accepted_child_service_result_waits_for_human_verification_before_single_parent_return() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("accepted result", None));
    let child_model = service_model(&flow_id, ActivityVerification::Human);
    let target = publish_model(&fixture, &child_model);
    let caller = publish_model(&fixture, &call_tests::caller(&target, BTreeMap::new()));
    let parent = messages::test_support::start_version(&fixture, &caller);
    let child_id = call_tests::child_id(&fixture, &parent.instance_id);
    let claim = repository::claim_job(
        &fixture.db,
        "call-human-verification",
        chrono::Utc::now().timestamp_millis(),
    )
    .unwrap()
    .unwrap();
    jobs::execute_claimed(
        &fixture.db,
        fixture.dispatcher(),
        "call-human-verification",
        claim,
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .unwrap();
    let child = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    let task = child
        .user_tasks
        .iter()
        .find(|task| task.kind == ProcessUserTaskKind::Verification)
        .expect("real human verification task");
    assert_eq!(task.status, ProcessUserTaskStatus::Open);
    assert_eq!(child.jobs.len(), 1);
    assert_eq!(child.jobs[0].status, "completed");
    assert!(child.jobs[0].result.is_some());
    assert_eq!(child.instance.status, ProcessInstanceStatus::Waiting);
    assert_eq!(
        repository::get_instance(&fixture.db, &fixture.owner, &parent.instance_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Waiting
    );
    assert_eq!(return_count(&fixture, &parent.instance_id), 0);
    let at = chrono::Utc::now().timestamp_millis();
    let output = json!({});
    let command = runtime::test_support::stamp("approve called service result");
    let plan =
        runtime::plan_user_completion(&child, &task.user_task_id, &output, Some(true), at,
            runtime::test_support::human_input(&child, &task.user_task_id, &command), None).unwrap();
    let approved = repository::complete_user_task(
        &fixture.db,
        &fixture.owner,
        &command,
        &child_id,
        &task.user_task_id,
        child.instance.revision,
        &output,
        Some(true),
        repository::ProcessPlanInput::Supplied(&plan),
        at,
    )
    .unwrap();
    assert_eq!(approved.instance.status, ProcessInstanceStatus::Completed);
    assert_eq!(
        repository::get_instance(&fixture.db, &fixture.owner, &parent.instance_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Completed
    );
    assert_eq!(return_count(&fixture, &parent.instance_id), 1);
    let replay = repository::complete_user_task(
        &fixture.db,
        &fixture.owner,
        &command,
        &child_id,
        &task.user_task_id,
        child.instance.revision,
        &output,
        Some(true),
        repository::ProcessPlanInput::Supplied(&plan),
        at,
    )
    .unwrap();
    assert_eq!(replay.instance, approved.instance);
    assert_eq!(return_count(&fixture, &parent.instance_id), 1);
}

#[test]
fn human_error_end_crosses_pinned_call_boundary_before_root_termination_once() {
    let fixture = Fixture::new();
    let mut child_model = runtime::test_support::user_model(Some(&fixture.owner.user_id));
    child_model.variables.insert("answer".into(), json!("pending"));
    child_model.errors.push(ProcessErrorDeclaration {
        error_id: "Rejected".into(),
        name: "Rejected after human review".into(),
        error_code: "REJECTED".into(),
    });
    child_model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::ErrorEnd { error_ref: "Rejected".into() };
    let target = publish_model(&fixture, &child_model);
    let mut parent_model = call_tests::caller(&target, BTreeMap::new());
    parent_model.errors.push(ProcessErrorDeclaration {
        error_id: "Rejected".into(),
        name: "Catch the child's rejection".into(),
        error_code: "REJECTED".into(),
    });
    parent_model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    parent_model.nodes.push(ProcessNode {
        id: "CatchCalledError".into(),
        name: "Map the accepted child error".into(),
        kind: ProcessNodeKind::BoundaryError {
            attached_to_id: "Call_1".into(),
            error_ref: Some("Rejected".into()),
            output_mapping: BTreeMap::from([
                ("accepted_error_value".into(), "outputs.answer".into()),
                ("error_source".into(), "activity_result".into()),
            ]),
        },
        repeat: None,
        activity_io: None,
    });
    parent_model.sequence_flows.push(edge("CaughtToTerminate", "CatchCalledError", "End_1"));
    let caller = publish_model(&fixture, &parent_model);
    let parent = messages::test_support::start_version(&fixture, &caller);
    let child_id = call_tests::child_id(&fixture, &parent.instance_id);
    assert_eq!(parent.status, ProcessInstanceStatus::Waiting);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &child_id).unwrap();
    assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Waiting);
    let task = snapshot.user_tasks.iter().find(|task| task.node_id == "Work").unwrap();
    let outputs = json!({"answer":"approved review evidence"});
    let command = runtime::test_support::stamp("complete pinned child's real human error input");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_user_completion(&snapshot, &task.user_task_id, &outputs, None, at_ms,
        runtime::test_support::human_input(&snapshot, &task.user_task_id, &command), None).unwrap();
    let accepted = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &child_id, &task.user_task_id, snapshot.instance.revision, &outputs, None, repository::ProcessPlanInput::Supplied(&plan), at_ms)
        .unwrap().instance;
    assert_eq!(accepted.status, ProcessInstanceStatus::Error);
    assert_eq!(accepted.variables["answer"], "approved review evidence");
    let completed = repository::get_instance(&fixture.db, &fixture.owner, &parent.instance_id, None)
        .unwrap();
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_eq!(completed.variables["accepted_error_value"], "approved review evidence");
    assert_eq!(completed.variables["error_source"]["error_code"], "REJECTED");
    let source = repository::list_events(&fixture.db, &fixture.owner, &child_id, 0, 200).unwrap().0;
    let error = source.iter().find(|event| event.kind == "error_end_reached").unwrap();
    assert_eq!(error.data["outputs"]["answer"], "approved review evidence");
    assert_eq!(accepted.terminal_error.as_ref().unwrap().source_event_id, error.event_id);
    let parent_events = repository::list_events(&fixture.db, &fixture.owner,
        &parent.instance_id, 0, 200).unwrap().0;
    let propagated = parent_events.iter().filter(|event| event.kind == "call_error_propagated")
        .collect::<Vec<_>>();
    let caught = parent_events.iter().filter(|event| event.kind == "business_error_caught")
        .collect::<Vec<_>>();
    let termination = parent_events.iter().filter(|event| event.kind == "terminate_end_reached")
        .collect::<Vec<_>>();
    assert_eq!((propagated.len(), caught.len(), termination.len()), (1, 1, 1));
    assert_eq!(propagated[0].data["source_instance_id"], child_id);
    assert_eq!(propagated[0].data["source_event_id"].as_str(), Some(error.event_id.as_str()));
    assert_eq!(caught[0].data["source_event_id"].as_str(), Some(error.event_id.as_str()));
    assert_eq!(caught[0].data["source_kind"], "error_end");
    assert_eq!(termination[0].scope_id, parent.instance_id);
    assert!(propagated[0].seq < caught[0].seq && caught[0].seq < termination[0].seq);
    assert_eq!(parent_events.iter().filter(|event| event.kind == "call_returned").count(), 0);
    assert_eq!(parent_events.iter().filter(|event| event.kind == "instance_completed").count(), 1);
    let rows = call_tests::transition_rows(&fixture);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(repository::get_instance(&reopened, &fixture.owner, &child_id, None).unwrap(), accepted);
    assert_eq!(repository::get_instance(&reopened, &fixture.owner, &parent.instance_id, None).unwrap(), completed);
    let replay = repository::complete_user_task(&reopened, &fixture.owner, &command,
        &child_id, &task.user_task_id, snapshot.instance.revision, &outputs, None, repository::ProcessPlanInput::Supplied(&plan), at_ms)
        .unwrap().instance;
    assert_eq!(replay, accepted);
    assert_eq!(call_tests::transition_rows(&fixture), rows);
    assert_eq!(repository::list_events(&reopened, &fixture.owner, &parent.instance_id, 0, 200)
        .unwrap().0, parent_events);
}

fn terminate_child(wait: bool) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = if wait {
        runtime::test_support::user_model(None)
    } else {
        super::model::starter_model()
    };
    model.variables.insert("answer".into(), json!(42));
    model.variables.insert("seed".into(), json!(7));
    for node in &mut model.nodes {
        if node.id == "End_1" {
            node.kind = ProcessNodeKind::TerminateEnd;
        }
        if let ProcessNodeKind::UserTask { output_mapping, .. } = &mut node.kind {
            output_mapping.clear();
        }
    }
    model
}

fn terminate_caller(
    target: &tentaflow_protocol::processes::ProcessVersion,
    failed_return: bool,
) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = call_tests::caller(target, BTreeMap::from([(
        "received".into(),
        if failed_return { "outputs.missing.required" } else { "outputs.answer" }.into(),
    )]));
    model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    for (id, kind) in [
        ("Split", ProcessNodeKind::ParallelGateway),
        ("ParentWork", ProcessNodeKind::UserTask {
            assignee_user_id: None,
            output_mapping: BTreeMap::new(),
        }),
        ("AfterCall", ProcessNodeKind::UserTask {
            assignee_user_id: None,
            output_mapping: BTreeMap::new(),
        }),
    ] {
        model.nodes.push(ProcessNode { id: id.into(), name: id.into(), kind,
            repeat: None, activity_io: None });
    }
    model.sequence_flows = vec![
        edge("StartSplit", "Start_1", "Split"),
        edge("SplitCall", "Split", "Call_1"),
        edge("SplitWork", "Split", "ParentWork"),
        edge("CallAfter", "Call_1", "AfterCall"),
        edge("AfterTerminate", "AfterCall", "End_1"),
        edge("WorkTerminate", "ParentWork", "End_1"),
    ];
    model
}

fn termination_event(fixture: &Fixture, instance_id: &str) -> tentaflow_protocol::processes::ProcessEvent {
    let events = repository::list_events(&fixture.db, &fixture.owner, instance_id, 0, 200).unwrap().0;
    let sources = events.iter().filter(|event| event.kind == "terminate_end_reached").collect::<Vec<_>>();
    assert_eq!(sources.len(), 1);
    let source = sources[0];
    assert_eq!(source.scope_id, instance_id);
    assert_eq!(source.node_id.as_deref(), Some("End_1"));
    uuid::Uuid::parse_str(&source.event_id).unwrap();
    assert_eq!(source.data, json!({
        "source_instance_id":instance_id,"source_event_id":source.event_id,
        "source_token_id":source.data["source_token_id"],"source_scope_id":instance_id,
        "source_node_id":"End_1","terminated_scope_id":instance_id,
    }));
    let completion = events.iter().filter(|event| event.kind == "instance_completed").collect::<Vec<_>>();
    assert_eq!(completion.len(), 1);
    assert_eq!(completion[0].data, json!({
        "reason":"terminate_end","source_instance_id":instance_id,"source_event_id":source.event_id,
        "source_scope_id":instance_id,"source_node_id":"End_1","source_token_id":source.data["source_token_id"],
    }));
    assert!(!events.iter().any(|event| event.kind == "end_reached"));
    let conn = fixture.db.read().unwrap();
    let state: String = conn.query_row("SELECT status FROM bpmn_tokens WHERE token_id=?1 AND instance_id=?2",
        rusqlite::params![source.data["source_token_id"].as_str().unwrap(),instance_id], |row| row.get(0)).unwrap();
    assert_eq!(state,"consumed");
    source.clone()
}

#[test]
fn immediate_pinned_terminate_child_has_no_manual_command_and_reopens_exactly() {
    let fixture = Fixture::new();
    let target = publish_model(&fixture, &terminate_child(false));
    let mut caller = call_tests::caller(&target, BTreeMap::from([("received".into(),"outputs.answer".into())]));
    caller.variables.insert("parent_seed".into(),json!(9));
    let ProcessNodeKind::CallActivity(ProcessCallActivity { input_mapping, .. }) = &mut caller.nodes.iter_mut()
        .find(|node| node.id == "Call_1").unwrap().kind else { panic!("actual pinned call"); };
    input_mapping.insert("seed".into(),"vars.parent_seed".into());
    let version = publish_model(&fixture, &caller);
    let instance_id = uuid::Uuid::new_v4().to_string();
    let at = chrono::Utc::now().timestamp_millis();
    let command = runtime::test_support::stamp("immediate called termination");
    let variables = serde_json::to_value(&version.model.variables).unwrap();
    let plan = runtime::plan_start(&version.model, &version.model.process_id, crate::processes::runtime::test_support::ordinary_start_id(&version.model),&instance_id,&fixture.owner,&version.definition_id,
        version.version,variables.clone(),runtime::StartCause::Manual,at,runtime::test_support::manual_input(&command), None).unwrap();
    let parent = repository::start_instance(&fixture.db,&fixture.owner,&command,&instance_id,
        &version.definition_id,version.version,&variables, None, None,repository::ProcessPlanInput::Supplied(&plan),at).unwrap();
    assert_eq!(parent.status,ProcessInstanceStatus::Completed);
    assert_eq!(parent.variables["received"],json!(42));
    let child_id = call_tests::child_id(&fixture,&instance_id);
    let child = repository::runtime_snapshot(&fixture.db,&fixture.owner,&child_id).unwrap();
    assert_eq!(child.instance.status,ProcessInstanceStatus::Completed);
    assert_eq!(child.instance.version,target.version);
    assert_eq!(child.instance.variables["seed"],json!(9));
    assert!(child.tokens.is_empty());
    assert!(child.receipts.is_empty());
    assert_eq!(return_count(&fixture,&instance_id),1);
    let source = termination_event(&fixture,&child_id);
    let events = repository::list_events(&fixture.db,&fixture.owner,&child_id,0,200).unwrap().0;
    assert_eq!(events.iter().find(|event| event.kind == "instance_started").unwrap().data,
        json!({"initiator_user_id":fixture.owner.user_id}));
    let parent_events = repository::list_events(&fixture.db,&fixture.owner,&instance_id,0,200).unwrap().0;
    assert_eq!(parent_events.iter().find(|event| event.kind == "instance_completed").unwrap().data,json!(null));
    assert_eq!(parent_events.iter().find(|event| event.kind == "end_reached").unwrap().data,json!(null));
    {
        let conn = fixture.db.read().unwrap();
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM bpmn_commands WHERE json_extract(result_json,'$.instance_id')=?1",
            [&child_id],|row| row.get::<_,i64>(0)).unwrap(),0);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM bpmn_calls WHERE child_instance_id=?1 AND status='returned'",
            [&child_id],|row| row.get::<_,i64>(0)).unwrap(),1);
    }
    let before = call_tests::transition_rows(&fixture);
    let path = fixture.directory.path().join("processes.db");
    let Fixture {directory,db,router,owner,participant} = fixture;
    drop(router); drop(db);
    let reopened = crate::db::init(&path).unwrap();
    {
        let conn = reopened.read().unwrap();
        for (table,rows) in ["bpmn_instances","bpmn_scopes","bpmn_tokens","bpmn_gateway_receipts","bpmn_user_tasks","bpmn_jobs",
            "bpmn_incidents","bpmn_timers","bpmn_event_subscriptions","bpmn_event_races","bpmn_messages","bpmn_calls","bpmn_events","bpmn_commands"].iter().zip(&before) {
            assert_eq!(&super::call_pin_tests::table_rows(&conn,table,"rowid"),rows,"{table}");
        }
    }
    let replay = repository::start_instance(&reopened,&owner,&command,&uuid::Uuid::new_v4().to_string(),
        &version.definition_id,version.version,&variables, None, None,repository::ProcessPlanInput::Supplied(&plan),at).unwrap();
    assert_eq!(replay,parent);
    assert_eq!(repository::list_events(&reopened,&owner,&child_id,0,200).unwrap().0,events);
    assert_eq!(repository::list_events(&reopened,&owner,&instance_id,0,200).unwrap().0,parent_events);
    assert_eq!(source.data["source_instance_id"],json!(child_id));
    {
        let conn = reopened.read().unwrap();
        for (table,rows) in ["bpmn_instances","bpmn_scopes","bpmn_tokens","bpmn_gateway_receipts","bpmn_user_tasks","bpmn_jobs",
            "bpmn_incidents","bpmn_timers","bpmn_event_subscriptions","bpmn_event_races","bpmn_messages","bpmn_calls","bpmn_events","bpmn_commands"].iter().zip(&before) {
            assert_eq!(&super::call_pin_tests::table_rows(&conn,table,"rowid"),rows,"replay {table}");
        }
    }
    drop(reopened); drop(participant); drop(directory);
}

#[test]
fn nested_called_termination_keeps_boxed_local_indices_and_own_event_uuids() {
    let fixture = Fixture::new();
    let leaf = publish_model(&fixture,&terminate_child(false));
    let mut middle_model = call_tests::caller(&leaf,BTreeMap::new());
    middle_model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind = ProcessNodeKind::TerminateEnd;
    let middle = publish_model(&fixture,&middle_model);
    let outer = publish_model(&fixture,&call_tests::caller(&middle,BTreeMap::new()));
    let captured = std::sync::Arc::new(std::sync::Mutex::new(None::<repository::RuntimePlan>));
    let observed = captured.clone();
    repository::CALL_PLAN_TEST_MUTATOR.with(|slot| *slot.borrow_mut() = Some(Box::new(move |composite| {
        *observed.lock().unwrap() = Some(composite.clone());
    })));
    let parent = messages::test_support::start_version(&fixture,&outer);
    repository::CALL_PLAN_TEST_MUTATOR.with(|slot| *slot.borrow_mut() = None);
    assert_eq!(parent.status,ProcessInstanceStatus::Completed);
    let composite = captured.lock().unwrap().take().unwrap();
    let mut sources = Vec::new();
    for (step_index,step) in composite.call_steps.iter().enumerate() {
        let (instance_id,plan) = match step {
            repository::CallStep::Start {call,plan} => (&call.call.child_instance_id,plan),
            repository::CallStep::Return {call,plan,..} => (&call.parent_instance_id,plan),
            repository::CallStep::Advance {instance_id,plan,..} => (instance_id,plan),
        };
        for attempt in &plan.termination_attempts {
            let repository::TerminationAttempt::Success(source) = attempt else { panic!("successful terminal source"); };
            assert_eq!(&source.source_instance_id,instance_id);
            assert!(source.source_event_index < plan.events.len());
            assert_eq!(plan.event_ids.get(&source.source_event_index),Some(&source.source_event_id));
            let event = &plan.events[source.source_event_index];
            assert_eq!(event.kind,"terminate_end_reached");
            assert_eq!(event.scope_id,source.source_scope_id);
            assert_eq!(event.data["source_event_id"],json!(source.source_event_id));
            if let repository::AcceptedInputRef::Start {cause:repository::StartInputRef::CallStart {call_step_index,..},..} = &source.accepted_input {
                assert_eq!(*call_step_index,step_index);
            }
            sources.push((instance_id.clone(),source.source_event_id.clone()));
        }
    }
    assert_eq!(sources.len(),2);
    assert_ne!(sources[0].0,sources[1].0);
    assert_ne!(sources[0].1,sources[1].1);
    for (id,event_id) in sources {
        assert_eq!(termination_event(&fixture,&id).event_id,event_id);
        assert_eq!(return_count(&fixture,&id),if id == call_tests::child_id(&fixture,&parent.instance_id) {1} else {0});
        assert_eq!(fixture.db.read().unwrap().query_row("SELECT COUNT(*) FROM bpmn_commands WHERE json_extract(result_json,'$.instance_id')=?1",
            [&id],|row| row.get::<_,i64>(0)).unwrap(),0);
    }
    assert_eq!(return_count(&fixture,&parent.instance_id),1);
}

#[test]
fn parent_termination_closes_active_or_completed_child_in_both_return_orders() {
    for (child_first,failed_return) in [(false,false),(true,false),(true,true)] {
        let fixture = Fixture::new();
        let target = publish_model(&fixture,&terminate_child(true));
        let version = publish_model(&fixture,&terminate_caller(&target,failed_return));
        let parent = messages::test_support::start_version(&fixture,&version);
        let child_id = call_tests::child_id(&fixture,&parent.instance_id);
        let child_before = repository::runtime_snapshot(&fixture.db,&fixture.owner,&child_id).unwrap();
        let child_task = child_before.user_tasks.iter().find(|task| task.status == ProcessUserTaskStatus::Open).unwrap();
        let stale_command = runtime::test_support::stamp("stale child termination after parent closure");
        let at = chrono::Utc::now().timestamp_millis();
        let stale_plan = runtime::plan_user_completion(&child_before,&child_task.user_task_id,&json!({}),None,at,
            runtime::test_support::human_input(&child_before,&child_task.user_task_id,&stale_command), None).unwrap();
        if child_first {
            let child_done = messages::test_support::complete(&fixture,&child_id,&child_task.user_task_id);
            assert_eq!(child_done.status,ProcessInstanceStatus::Completed);
            if failed_return {
                let parent_wait = repository::runtime_snapshot(&fixture.db,&fixture.owner,&parent.instance_id).unwrap();
                assert_eq!(parent_wait.instance.status,ProcessInstanceStatus::Incident);
                assert!(parent_wait.calls.iter().any(|call| call.status == tentaflow_protocol::processes::ProcessCallStatus::ReturnIncident));
            }
        }
        let child_events_before = repository::list_events(&fixture.db,&fixture.owner,&child_id,0,200).unwrap().0;
        let child_rows_before = repository::runtime_snapshot(&fixture.db,&fixture.owner,&child_id).unwrap();
        let snapshot = repository::runtime_snapshot(&fixture.db,&fixture.owner,&parent.instance_id).unwrap();
        let task = snapshot.user_tasks.iter().find(|task| task.node_id == "ParentWork" && task.status == ProcessUserTaskStatus::Open).unwrap();
        let command = runtime::test_support::stamp("accepted parent TerminateEnd input");
        let now = chrono::Utc::now().timestamp_millis();
        let plan = runtime::plan_user_completion(&snapshot,&task.user_task_id,&json!({}),None,now,
            runtime::test_support::human_input(&snapshot,&task.user_task_id,&command), None).unwrap();
        let terminated = repository::complete_user_task(&fixture.db,&fixture.owner,&command,&parent.instance_id,
            &task.user_task_id,snapshot.instance.revision,&json!({}),None,repository::ProcessPlanInput::Supplied(&plan),now).unwrap();
        assert_eq!(terminated.instance.status,ProcessInstanceStatus::Completed);
        assert!(terminated.cancelled_claims.is_empty());
        let source = termination_event(&fixture,&parent.instance_id);
        let child_after = repository::runtime_snapshot(&fixture.db,&fixture.owner,&child_id).unwrap();
        let child_events = repository::list_events(&fixture.db,&fixture.owner,&child_id,0,200).unwrap().0;
        assert_eq!(child_after.instance.status,if child_first {ProcessInstanceStatus::Completed} else {ProcessInstanceStatus::Cancelled});
        assert!(child_after.tokens.is_empty());
        assert!(child_after.receipts.is_empty());
        assert!(child_after.incidents.is_empty());
        assert!(child_after.user_tasks.iter().all(|task| task.status != ProcessUserTaskStatus::Open));
        assert_eq!(return_count(&fixture,&parent.instance_id),usize::from(child_first && !failed_return));
        if child_first {
            assert_eq!(child_after.instance.revision,child_rows_before.instance.revision);
            assert_eq!(child_after.instance.variables,child_rows_before.instance.variables);
            assert_eq!(child_events,child_events_before);
            assert_eq!(serde_json::to_value(&child_after.scopes).unwrap(),serde_json::to_value(&child_rows_before.scopes).unwrap());
            assert_eq!(serde_json::to_value(&child_after.scope_variables).unwrap(),serde_json::to_value(&child_rows_before.scope_variables).unwrap());
            assert_eq!(child_after.user_tasks,child_rows_before.user_tasks);
            termination_event(&fixture,&child_id);
        } else {
            let cancellations = child_events.iter().filter(|event| event.kind == "cancelled").collect::<Vec<_>>();
            assert_eq!(cancellations.len(),1);
            assert_eq!(cancellations[0].scope_id,child_id);
            assert_eq!(cancellations[0].node_id,None);
            assert_eq!(cancellations[0].data,json!({"reason":"terminate_end",
                "source_instance_id":parent.instance_id,"source_event_id":source.event_id}));
            assert!(!child_events.iter().any(|event| event.kind == "terminate_end_reached" || event.kind == "instance_completed"));
        }
        {
            let conn = fixture.db.read().unwrap();
            for query in [
                "SELECT COUNT(*) FROM bpmn_tokens WHERE instance_id IN (?1,?2) AND status IN ('ready','waiting','joining')",
                "SELECT COUNT(*) FROM bpmn_user_tasks WHERE instance_id IN (?1,?2) AND status='open'",
                "SELECT COUNT(*) FROM bpmn_gateway_receipts WHERE instance_id IN (?1,?2)",
                "SELECT COUNT(*) FROM bpmn_jobs WHERE instance_id IN (?1,?2) AND status IN ('queued','running','error')",
                "SELECT COUNT(*) FROM bpmn_timers WHERE instance_id IN (?1,?2) AND status IN ('pending','blocked')",
                "SELECT COUNT(*) FROM bpmn_event_subscriptions WHERE instance_id IN (?1,?2) AND status='open'",
                "SELECT COUNT(*) FROM bpmn_event_races WHERE instance_id IN (?1,?2) AND status='open'",
                "SELECT COUNT(*) FROM bpmn_calls WHERE parent_instance_id IN (?1,?2) AND status IN ('waiting','return_incident')",
                "SELECT COUNT(*) FROM bpmn_incidents WHERE instance_id IN (?1,?2) AND resolved_at_ms IS NULL",
                "SELECT COUNT(*) FROM bpmn_messages WHERE source_instance_id IN (?1,?2) AND status IN ('pending','blocked','ambiguous')",
            ] {
                assert_eq!(conn.query_row(query,rusqlite::params![parent.instance_id,child_id],|row| row.get::<_,i64>(0)).unwrap(),0,"{query}");
            }
        }
        let before_replay = call_tests::transition_rows(&fixture);
        let replay = repository::complete_user_task(&fixture.db,&fixture.owner,&command,&parent.instance_id,
            &task.user_task_id,snapshot.instance.revision,&json!({}),None,repository::ProcessPlanInput::Supplied(&plan),now).unwrap();
        assert_eq!(replay.instance,terminated.instance);
        assert!(replay.cancelled_claims.is_empty());
        assert_eq!(call_tests::transition_rows(&fixture),before_replay);
        if !child_first {
            assert!(repository::complete_user_task(&fixture.db,&fixture.owner,&stale_command,&child_id,
                &child_task.user_task_id,child_before.instance.revision,&json!({}),None,repository::ProcessPlanInput::Supplied(&stale_plan),at).is_err());
            assert_eq!(call_tests::transition_rows(&fixture),before_replay);
        }
        let path = fixture.directory.path().join("processes.db");
        let Fixture {directory,db,router,owner,participant} = fixture;
        drop(router); drop(db);
        let reopened = crate::db::init(&path).unwrap();
        assert_eq!(repository::get_instance(&reopened,&owner,&parent.instance_id,None).unwrap().status,ProcessInstanceStatus::Completed);
        assert_eq!(repository::list_events(&reopened,&owner,&child_id,0,200).unwrap().0,child_events);
        {
            let conn = reopened.read().unwrap();
            for (table,rows) in ["bpmn_instances","bpmn_scopes","bpmn_tokens","bpmn_gateway_receipts","bpmn_user_tasks","bpmn_jobs",
                "bpmn_incidents","bpmn_timers","bpmn_event_subscriptions","bpmn_event_races","bpmn_messages","bpmn_calls","bpmn_events","bpmn_commands"].iter().zip(&before_replay) {
                assert_eq!(&super::call_pin_tests::table_rows(&conn,table,"rowid"),rows,"reopen {table}");
            }
        }
        drop(reopened); drop(participant); drop(directory);
    }
}

#[test]
fn canonical_call_start_forgeries_roll_back_all_fourteen_tables_then_valid_plan_commits() {
    let fixture = Fixture::new();
    let target = publish_model(&fixture,&terminate_child(false));
    let mut caller = call_tests::caller(&target,BTreeMap::new());
    caller.variables.insert("parent_seed".into(),json!(9));
    let ProcessNodeKind::CallActivity(ProcessCallActivity { input_mapping, .. }) = &mut caller.nodes.iter_mut()
        .find(|node| node.id == "Call_1").unwrap().kind else { panic!("real call"); };
    input_mapping.insert("seed".into(),"vars.parent_seed".into());
    let version = publish_model(&fixture,&caller);
    let foreign_version = publish_model(&fixture,&terminate_child(false));
    let foreign = messages::test_support::start_version(&fixture,&foreign_version);
    let foreign_source = termination_event(&fixture,&foreign.instance_id);
    let instance_id = uuid::Uuid::new_v4().to_string();
    let command = runtime::test_support::stamp("canonical child start proof");
    let at = chrono::Utc::now().timestamp_millis();
    let variables = serde_json::to_value(&version.model.variables).unwrap();
    let plan = runtime::plan_start(&version.model, &version.model.process_id, crate::processes::runtime::test_support::ordinary_start_id(&version.model),&instance_id,&fixture.owner,&version.definition_id,
        version.version,variables.clone(),runtime::StartCause::Manual,at,runtime::test_support::manual_input(&command), None).unwrap();
    let controls = ["call-id","called-definition","called-version","model-pin","parent-revision","step-index",
        "mapped-child-input","foreign-source-instance","foreign-source-uuid","extra-ready-token","omitted-closure","extra-variable-effect"];
    let before = call_tests::transition_rows(&fixture);
    for (mutation,label) in controls.iter().enumerate() {
        let foreign_instance_id = foreign.instance_id.clone();
        let foreign_event_id = foreign_source.event_id.clone();
        let inspected = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = inspected.clone();
        repository::CALL_PLAN_TEST_MUTATOR.with(|slot| *slot.borrow_mut() = Some(Box::new(move |composite| {
            let (step_index,step) = composite.call_steps.iter_mut().enumerate()
                .find(|(_,step)| matches!(step,repository::CallStep::Start {..})).unwrap();
            let repository::CallStep::Start {call,plan} = step else { panic!("actual Start step"); };
            let child_plan = plan.as_mut();
            let repository::TerminationAttempt::Success(source) = &mut child_plan.termination_attempts[0] else { panic!("real child termination"); };
            assert_eq!(source.source_instance_id,call.call.child_instance_id);
            assert_eq!(source.source_scope_id,call.call.child_instance_id);
            assert_eq!(child_plan.event_ids.get(&source.source_event_index),Some(&source.source_event_id));
            assert_eq!(child_plan.events[source.source_event_index].kind,"terminate_end_reached");
            let repository::AcceptedInputRef::Start {instance_id,cause:repository::StartInputRef::CallStart {
                call_id,child_definition_id,child_version,child_model_sha256,parent_instance_revision_at_step,call_step_index,..}} = &mut source.accepted_input
                else { panic!("canonical CallStart authority"); };
            assert_eq!(*instance_id,call.call.child_instance_id);
            assert_eq!(*call_id,call.call.call_id);
            assert_eq!(*child_definition_id,call.call.called_definition_id);
            assert_eq!(*child_version,call.call.called_version);
            assert_eq!(*child_model_sha256,call.call.model_sha256);
            assert_eq!(*call_step_index,step_index);
            assert_eq!(call.call.revision,1);
            assert_eq!(call.variables["seed"],json!(9));
            observed.store(true,std::sync::atomic::Ordering::SeqCst);
            match mutation {
                0 => *call_id = uuid::Uuid::new_v4().to_string(),
                1 => *child_definition_id = uuid::Uuid::new_v4().to_string(),
                2 => *child_version += 1,
                3 => *child_model_sha256 = "0".repeat(64),
                4 => *parent_instance_revision_at_step += 1,
                5 => *call_step_index += 1,
                6 => {
                    call.variables["seed"] = json!(10);
                    child_plan.start_variables.as_mut().unwrap()["seed"] = json!(10);
                    child_plan.variables["seed"] = json!(10);
                }
                7 => source.source_instance_id = foreign_instance_id,
                8 => {
                    let original_event_id = source.source_event_id.clone();
                    source.source_event_id = foreign_event_id.clone();
                    child_plan.event_ids.insert(source.source_event_index,foreign_event_id.clone());
                    for event in &mut child_plan.events {
                        if event.data.get("source_event_id").and_then(serde_json::Value::as_str) == Some(original_event_id.as_str()) {
                            event.data["source_event_id"] = json!(foreign_event_id);
                        }
                    }
                }
                9 => {
                    let mut extra = child_plan.create_tokens.iter().find(|token| token.token_id == source.source_token_id).unwrap().clone();
                    extra.token_id = uuid::Uuid::new_v4().to_string(); extra.status = "ready".into();
                    child_plan.create_tokens.push(extra);
                }
                10 => child_plan.cancel_scope_roots.clear(),
                11 => { child_plan.variables["unmapped_extra"] = json!(true); }
                _ => unreachable!("bounded forgery control"),
            }
        })));
        let attempted = repository::start_instance(&fixture.db,&fixture.owner,&command,&instance_id,
            &version.definition_id,version.version,&variables, None, None,repository::ProcessPlanInput::Supplied(&plan),at);
        repository::CALL_PLAN_TEST_MUTATOR.with(|slot| *slot.borrow_mut() = None);
        assert!(inspected.load(std::sync::atomic::Ordering::SeqCst),"{label} inspected real canonical child plan");
        assert!(attempted.is_err(),"{label} must reject atomically");
        assert_eq!(call_tests::transition_rows(&fixture),before,"{label}: every column of all fourteen tables");
    }
    let accepted = repository::start_instance(&fixture.db,&fixture.owner,&command,&instance_id,
        &version.definition_id,version.version,&variables, None, None,repository::ProcessPlanInput::Supplied(&plan),at).unwrap();
    assert_eq!(accepted.status,ProcessInstanceStatus::Completed);
    let child_id = call_tests::child_id(&fixture,&instance_id);
    assert_eq!(repository::get_instance(&fixture.db,&fixture.owner,&child_id,None).unwrap().variables["seed"],json!(9));
    termination_event(&fixture,&child_id);
    assert_eq!(return_count(&fixture,&instance_id),1);
    let committed = call_tests::transition_rows(&fixture);
    assert_ne!(committed,before);
    let replay = repository::start_instance(&fixture.db,&fixture.owner,&command,&uuid::Uuid::new_v4().to_string(),
        &version.definition_id,version.version,&variables, None, None,repository::ProcessPlanInput::Supplied(&plan),at).unwrap();
    assert_eq!(replay,accepted);
    assert_eq!(call_tests::transition_rows(&fixture),committed);
}

#[test]
fn parent_termination_fences_real_child_service_in_both_commit_orders_without_redispatch() {
    for child_first in [false,true] {
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db,&fixture.owner,&graph("observed before termination",None));
        let mut child_model = service_model(&flow_id,ActivityVerification::Condition {expression:"true".into()});
        child_model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind = ProcessNodeKind::TerminateEnd;
        let target = publish_model(&fixture,&child_model);
        let version = publish_model(&fixture,&terminate_caller(&target,false));
        let parent = messages::test_support::start_version(&fixture,&version);
        let child_id = call_tests::child_id(&fixture,&parent.instance_id);
        let worker_id = "terminate-called-service";
        let claim = repository::claim_job(&fixture.db,worker_id,chrono::Utc::now().timestamp_millis()).unwrap().unwrap();
        assert_eq!(claim.job.instance_id,child_id);
        let replay_claim = claim.clone();
        let complete_parent = || {
            let snapshot = repository::runtime_snapshot(&fixture.db,&fixture.owner,&parent.instance_id).unwrap();
            let task = snapshot.user_tasks.iter().find(|task| task.node_id == "ParentWork" && task.status == ProcessUserTaskStatus::Open).unwrap();
            let command = runtime::test_support::stamp("terminate real called worker");
            let now = chrono::Utc::now().timestamp_millis();
            let plan = runtime::plan_user_completion(&snapshot,&task.user_task_id,&json!({}),None,now,
                runtime::test_support::human_input(&snapshot,&task.user_task_id,&command), None).unwrap();
            repository::complete_user_task(&fixture.db,&fixture.owner,&command,&parent.instance_id,
                &task.user_task_id,snapshot.instance.revision,&json!({}),None,repository::ProcessPlanInput::Supplied(&plan),now)
        };
        let outcome;
        let completed_jobs;
        if child_first {
            let worker = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            worker.block_on(jobs::execute_claimed(&fixture.db,fixture.dispatcher(),worker_id,claim,
                tokio_util::sync::CancellationToken::new())).unwrap();
            termination_event(&fixture,&child_id);
            completed_jobs = Some(super::call_pin_tests::table_rows(&fixture.db.read().unwrap(),"bpmn_jobs","rowid"));
            outcome = complete_parent().unwrap();
            assert!(outcome.cancelled_claims.is_empty());
        } else {
            completed_jobs = None;
            let (ready_tx,ready_rx) = std::sync::mpsc::sync_channel(0);
            let (resume_tx,resume_rx) = std::sync::mpsc::sync_channel(0);
            outcome = std::thread::scope(|scope| {
                let resume_tx = resume_tx;
                let fixture = &fixture;
                let pending = scope.spawn(move || {
                    repository::CALL_TRANSITION_PREFLIGHT.with(|gate| *gate.borrow_mut() = Some((ready_tx,resume_rx)));
                    let worker = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                    let result = worker.block_on(jobs::execute_claimed(&fixture.db,fixture.dispatcher(),worker_id,claim,
                        tokio_util::sync::CancellationToken::new()));
                    repository::CALL_TRANSITION_PREFLIGHT.with(|gate| *gate.borrow_mut() = None);
                    result
                });
                ready_rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
                let actual_executions = crate::db::repository::list_flow_executions_for_flow(&fixture.db,&flow_id,10).unwrap().len();
                let committed = complete_parent();
                let committed_rows = call_tests::transition_rows(fixture);
                resume_tx.send(()).unwrap();
                pending.join().unwrap().unwrap();
                let committed = committed.unwrap();
                assert_eq!(actual_executions,1);
                assert_eq!(call_tests::transition_rows(fixture),committed_rows,"stale external result cannot alter any transition table");
                committed
            });
            assert_eq!(outcome.cancelled_claims,vec![repository::CancelledJobClaim {
                job_id:replay_claim.job.job_id.clone(),attempt:replay_claim.job.attempt,
                fence:replay_claim.job.fence,worker_id:worker_id.into(),
            }]);
        }
        assert_eq!(outcome.instance.status,ProcessInstanceStatus::Completed);
        let source = termination_event(&fixture,&parent.instance_id);
        let child_after = repository::runtime_snapshot(&fixture.db,&fixture.owner,&child_id).unwrap();
        assert_eq!(child_after.instance.status,if child_first {ProcessInstanceStatus::Completed} else {ProcessInstanceStatus::Cancelled});
        assert_eq!(child_after.jobs.len(),1);
        let job = &child_after.jobs[0];
        assert_eq!(job.status,if child_first {"completed"} else {"cancelled"});
        assert_eq!(job.fence,replay_claim.job.fence + u64::from(!child_first));
        assert_eq!(return_count(&fixture,&parent.instance_id),usize::from(child_first));
        if child_first {
            assert!(job.result.is_some());
            assert_eq!(super::call_pin_tests::table_rows(&fixture.db.read().unwrap(),"bpmn_jobs","rowid"),completed_jobs.unwrap());
        } else {
            assert_eq!(job.result,None);
            let child_events = repository::list_events(&fixture.db,&fixture.owner,&child_id,0,200).unwrap().0;
            assert_eq!(child_events.iter().filter(|event| event.kind == "service_result").count(),0);
            let cancelled = child_events.iter().find(|event| event.kind == "cancelled").unwrap();
            assert_eq!(cancelled.data,json!({"reason":"terminate_end","source_instance_id":parent.instance_id,"source_event_id":source.event_id}));
        }
        assert!(child_after.tokens.is_empty());
        assert!(child_after.receipts.is_empty());
        assert!(child_after.incidents.is_empty());
        let closed_rows = call_tests::transition_rows(&fixture);
        let worker = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        worker.block_on(jobs::execute_claimed(&fixture.db,fixture.dispatcher(),worker_id,replay_claim,
            tokio_util::sync::CancellationToken::new())).unwrap();
        assert_eq!(call_tests::transition_rows(&fixture),closed_rows);
        assert_eq!(crate::db::repository::list_flow_executions_for_flow(&fixture.db,&flow_id,10).unwrap().len(),1);
        let child_events = repository::list_events(&fixture.db,&fixture.owner,&child_id,0,200).unwrap().0;
        let parent_events = repository::list_events(&fixture.db,&fixture.owner,&parent.instance_id,0,200).unwrap().0;
        let path = fixture.directory.path().join("processes.db");
        let Fixture {directory,db,router,owner,participant} = fixture;
        drop(router); drop(db);
        let reopened = crate::db::init(&path).unwrap();
        assert_eq!(repository::list_events(&reopened,&owner,&child_id,0,200).unwrap().0,child_events);
        assert_eq!(repository::list_events(&reopened,&owner,&parent.instance_id,0,200).unwrap().0,parent_events);
        {
            let conn = reopened.read().unwrap();
            for (table,rows) in ["bpmn_instances","bpmn_scopes","bpmn_tokens","bpmn_gateway_receipts","bpmn_user_tasks","bpmn_jobs",
                "bpmn_incidents","bpmn_timers","bpmn_event_subscriptions","bpmn_event_races","bpmn_messages","bpmn_calls","bpmn_events","bpmn_commands"].iter().zip(&closed_rows) {
                assert_eq!(&super::call_pin_tests::table_rows(&conn,table,"rowid"),rows,"worker reopen {table}");
            }
        }
        drop(reopened); drop(participant); drop(directory);
    }
}
