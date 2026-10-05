// ============ File: repetition_tests.rs — file-backed repeated activity state and writer regressions ============

use serde_json::json;
use uuid::Uuid;
use tentaflow_protocol::processes::{
    ActivityVerification, ProcessInstanceStatus, ProcessMultiInstanceInput, ProcessMultiInstanceMode,
    ProcessNode, ProcessNodeKind, ProcessRepeatSpec, ProcessUserTaskStatus,
};

use super::model::starter_model;
use super::repository;
use super::runtime::{self, StartCause,
    test_support::{edge, flow, graph, human_input, manual_input, publish_model, service_model,
        stamp, start_model, Fixture}};

pub(super) fn sequential_human_model(owner: &str, count: u8) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = starter_model();
    model.variables.insert("results".into(), json!([]));
    model.nodes.insert(1, ProcessNode {
        id: "RepeatedWork".into(),
        name: "Review every item".into(),
        kind: ProcessNodeKind::UserTask {
            assignee_user_id: Some(owner.into()),
            output_mapping: Default::default(),
        },
        repeat: Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Sequential,
            input: ProcessMultiInstanceInput::Cardinality { count },
            output_collection_variable: "results".into(),
        }),
    });
    model.sequence_flows = vec![
        edge("EnterRepeatedWork", "Start_1", "RepeatedWork"),
        edge("LeaveRepeatedWork", "RepeatedWork", "End_1"),
    ];
    model
}

#[test]
fn sequential_human_ordinals_commit_once_and_forged_item_rolls_back_sixteen_tables() {
    let fixture = Fixture::new();
    assert!(fixture.directory.path().join("processes.db").is_file());
    let model = sequential_human_model(&fixture.owner.user_id, 3);
    let started = start_model(&fixture, &model);
    assert_eq!(started.status, ProcessInstanceStatus::Waiting);
    for ordinal in 0_u32..3 {
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let task = snapshot.user_tasks.iter().find(|task|
            task.node_id == "RepeatedWork" && task.status == ProcessUserTaskStatus::Open).unwrap();
        let outputs = json!({"answer":ordinal});
        let command = stamp("accept the actual repeated Work ordinal");
        let at_ms = chrono::Utc::now().timestamp_millis();
        let plan = runtime::plan_user_completion(&snapshot, &task.user_task_id, &outputs,
            None, at_ms, human_input(&snapshot, &task.user_task_id, &command), None).unwrap();
        if ordinal == 0 {
            let before = super::call_tests::transition_rows(&fixture);
            let completed = plan.events.iter().find(|event|
                event.kind == "repetition_occurrence_completed").unwrap().clone();
            for (fabricated, expected) in [
                (completed.clone(), "completed occurrence lacks one source-linked aggregate fact"),
                (super::repository::PlannedEvent {
                    kind: "repetition_unknown".into(), ..completed.clone()
                }, "repetition history differs"),
            ] {
                let mut forged = plan.clone();
                forged.events.push(fabricated);
                let error = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
                    &started.instance_id, &task.user_task_id, snapshot.instance.revision,
                    &outputs, None, repository::ProcessPlanInput::Supplied(&forged), at_ms).unwrap_err();
                let message = format!("{error:#}");
                assert!(message.contains(expected), "{message}");
                assert_eq!(super::call_tests::transition_rows(&fixture), before);
            }
            let source_index = plan.events.iter().position(|event|
                event.kind == "user_task_completed").unwrap();
            let completed_index = plan.events.iter().position(|event|
                event.kind == "repetition_occurrence_completed").unwrap();
            assert!(source_index < completed_index);
            let remap = |index: usize| {
                if index == source_index { completed_index }
                else if index == completed_index { source_index }
                else { index }
            };
            let mut reordered = plan.clone();
            reordered.events.swap(source_index, completed_index);
            reordered.event_ids = plan.event_ids.iter().map(|(index, id)|
                (remap(*index), id.clone())).collect();
            reordered.event_sources = plan.event_sources.iter().map(|(index, source)|
                (remap(*index), source.clone())).collect();
            let error = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
                &started.instance_id, &task.user_task_id, snapshot.instance.revision,
                &outputs, None, repository::ProcessPlanInput::Supplied(&reordered), at_ms).unwrap_err();
            assert!(format!("{error:#}").contains("repetition completion precedes its accepted source"));
            assert_eq!(super::call_tests::transition_rows(&fixture), before);
            let mut foreign_source = plan.clone();
            foreign_source.repetition_occurrences.iter_mut()
                .find(|row| row.ordinal == ordinal).unwrap().accepted_source_event_id =
                Some(Uuid::new_v4().to_string());
            assert!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
                &started.instance_id, &task.user_task_id, snapshot.instance.revision,
                &outputs, None, repository::ProcessPlanInput::Supplied(&foreign_source), at_ms).is_err());
            assert_eq!(super::call_tests::transition_rows(&fixture), before);
            let mut extra_closure = plan.clone();
            extra_closure.cancel_scope_roots.push(started.instance_id.clone());
            assert!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
                &started.instance_id, &task.user_task_id, snapshot.instance.revision,
                &outputs, None, repository::ProcessPlanInput::Supplied(&extra_closure), at_ms).is_err());
            assert_eq!(super::call_tests::transition_rows(&fixture), before);
        }
        if ordinal < 2 {
            let before = super::call_tests::transition_rows(&fixture);
            assert_eq!(before.len(), 16);
            let mut forged = plan.clone();
            let next = forged.repetition_occurrences.iter_mut()
                .find(|row| row.ordinal == ordinal + 1).unwrap();
            next.item = json!("foreign item");
            let error = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
                &started.instance_id, &task.user_task_id, snapshot.instance.revision,
                &outputs, None, repository::ProcessPlanInput::Supplied(&forged), at_ms).unwrap_err();
            assert!(format!("{error:#}").contains("repetition item"));
            assert_eq!(super::call_tests::transition_rows(&fixture), before);
        }
        repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &started.instance_id, &task.user_task_id, snapshot.instance.revision,
            &outputs, None, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
            &started.instance_id).unwrap();
        assert_eq!(persisted.repetition_groups.len(), 1);
        assert_eq!(persisted.repetition_groups[0].completed_count, ordinal + 1);
    }
    let final_state = repository::get_instance(&fixture.db, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(final_state.status, ProcessInstanceStatus::Completed);
    assert_eq!(final_state.variables["results"], json!([
        {"answer":0},{"answer":1},{"answer":2}
    ]));
}

#[test]
fn empty_multi_instance_finishes_without_an_occurrence_and_reopens() {
    let fixture = Fixture::new();
    let started = start_model(&fixture, &sequential_human_model(&fixture.owner.user_id, 0));
    assert_eq!(started.status, ProcessInstanceStatus::Completed);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(snapshot.repetition_groups.len(), 1);
    assert!(snapshot.repetition_occurrences.is_empty());
    assert_eq!(snapshot.repetition_groups[0].total_count, Some(0));
    assert_eq!(snapshot.repetition_groups[0].completed_count, 0);
    assert_eq!(snapshot.instance.variables["results"], json!([]));
}

#[test]
fn empty_repetition_then_terminate_has_exact_history_and_atomic_forgery_rollback() {
    let fixture = Fixture::new();
    let mut model = sequential_human_model(&fixture.owner.user_id, 0);
    model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    let version = publish_model(&fixture, &model);
    let instance_id = Uuid::new_v4().to_string();
    let variables = serde_json::to_value(&model.variables).unwrap();
    let command = stamp("start an empty repetition then terminate");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&version.model, &instance_id, &fixture.owner,
        &version.definition_id, version.version, variables.clone(), StartCause::Manual,
        at_ms, manual_input(&command), None).unwrap();
    assert_eq!(plan.termination_attempts.len(), 1);
    assert!(plan.events.iter().any(|event| event.kind == "repetition_completed"));
    let before = super::call_tests::transition_rows(&fixture);
    assert_eq!(before.len(), 16);
    let completed = plan.events.iter().find(|event|
        event.kind == "repetition_completed").unwrap().clone();
    for fabricated in [completed.clone(), repository::PlannedEvent {
        kind: "repetition_unknown".into(), ..completed.clone()
    }] {
        let mut forged = plan.clone();
        forged.events.push(fabricated);
        repository::start_instance(&fixture.db, &fixture.owner, &command,
            &instance_id, &version.definition_id, version.version, &variables,
            repository::ProcessPlanInput::Supplied(&forged), at_ms).unwrap_err();
        assert_eq!(super::call_tests::transition_rows(&fixture), before);
    }
    let committed = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    assert_eq!(committed.status, ProcessInstanceStatus::Completed);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::runtime_snapshot(&reopened, &fixture.owner, &instance_id).unwrap();
    assert_eq!(persisted.repetition_groups.len(), 1);
    assert_eq!(persisted.repetition_groups[0].completed_count, 0);
    assert_eq!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().status, ProcessInstanceStatus::Completed);
}

#[test]
fn completed_repetition_returns_through_persisted_parent_before_terminate() {
    let fixture = Fixture::new();
    let mut model = sequential_human_model(&fixture.owner.user_id, 1);
    model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    let started = start_model(&fixture, &model);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let task = snapshot.user_tasks.iter().find(|task|
        task.node_id == "RepeatedWork" && task.status == ProcessUserTaskStatus::Open).unwrap();
    let outputs = json!({"answer":41});
    let command = stamp("complete a repeated Work item before termination");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_user_completion(&snapshot, &task.user_task_id, &outputs,
        None, at_ms, human_input(&snapshot, &task.user_task_id, &command), None).unwrap();
    assert_eq!(plan.termination_attempts.len(), 1);
    let accepted_index = plan.events.iter().position(|event|
        event.kind == "user_task_completed").unwrap();
    let completed_index = plan.events.iter().position(|event|
        event.kind == "repetition_completed").unwrap();
    let terminated_index = plan.events.iter().position(|event|
        event.kind == "terminate_end_reached").unwrap();
    assert!(accepted_index < completed_index && completed_index < terminated_index);
    let before = super::call_tests::transition_rows(&fixture);
    assert_eq!(before.len(), 16);
    let mut wrong_source = plan.clone();
    wrong_source.repetition_occurrences.iter_mut().find(|row|
        row.ordinal == 0).unwrap().accepted_source_event_id = Some(Uuid::new_v4().to_string());
    assert!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &task.user_task_id, snapshot.instance.revision,
        &outputs, None, repository::ProcessPlanInput::Supplied(&wrong_source), at_ms).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), before);
    let mut wrong_parent = plan.clone();
    wrong_parent.repetition_groups.iter_mut().find(|row|
        row.node_id == "RepeatedWork").unwrap().parent_token_id = Uuid::new_v4().to_string();
    assert!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &task.user_task_id, snapshot.instance.revision,
        &outputs, None, repository::ProcessPlanInput::Supplied(&wrong_parent), at_ms).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), before);
    let mut duplicate_completion = plan.clone();
    duplicate_completion.events.push(plan.events[completed_index].clone());
    assert!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &task.user_task_id, snapshot.instance.revision,
        &outputs, None, repository::ProcessPlanInput::Supplied(&duplicate_completion), at_ms).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), before);
    let committed = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &task.user_task_id, snapshot.instance.revision,
        &outputs, None, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().instance;
    assert_eq!(committed.status, ProcessInstanceStatus::Completed);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(persisted.repetition_groups[0].completed_count, 1);
    assert_eq!(persisted.instance.variables["results"], json!([{"answer":41}]));
    assert_eq!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &started.instance_id, &task.user_task_id, snapshot.instance.revision,
        &outputs, None, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().instance.status,
        ProcessInstanceStatus::Completed);
}

#[test]
fn parallel_human_occurrences_wait_independently_and_join_once() {
    let fixture = Fixture::new();
    let mut model = sequential_human_model(&fixture.owner.user_id, 3);
    let repeat = model.nodes.iter_mut().find(|node| node.id == "RepeatedWork").unwrap()
        .repeat.as_mut().unwrap();
    let ProcessRepeatSpec::MultiInstance { mode, .. } = repeat else {
        panic!("fixture repetition is not multi-instance")
    };
    *mode = ProcessMultiInstanceMode::Parallel;
    let started = start_model(&fixture, &model);
    assert_eq!(started.status, ProcessInstanceStatus::Waiting);
    let initial = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(initial.repetition_occurrences.len(), 3);
    assert_eq!(initial.user_tasks.iter().filter(|task|
        task.node_id == "RepeatedWork" && task.status == ProcessUserTaskStatus::Open).count(), 3);
    for (completed, ordinal) in [2_u32, 0, 1].into_iter().enumerate() {
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let occurrence = snapshot.repetition_occurrences.iter().find(|row| row.ordinal == ordinal).unwrap();
        let task = snapshot.user_tasks.iter().find(|task|
            task.user_task_id == *occurrence.user_task_id.as_ref().unwrap()).unwrap();
        let command = stamp("accept one parallel repeated Work item");
        let outputs = json!({"answer":ordinal});
        let at_ms = chrono::Utc::now().timestamp_millis();
        let plan = runtime::plan_user_completion(&snapshot, &task.user_task_id, &outputs,
            None, at_ms, human_input(&snapshot, &task.user_task_id, &command), None).unwrap();
        repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &started.instance_id, &task.user_task_id, snapshot.instance.revision,
            &outputs, None, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
            &started.instance_id).unwrap();
        assert_eq!(persisted.repetition_groups[0].completed_count, completed as u32 + 1);
    }
    let final_state = repository::get_instance(&fixture.db, &fixture.owner,
        &started.instance_id, None).unwrap();
    assert_eq!(final_state.status, ProcessInstanceStatus::Completed);
    assert_eq!(final_state.variables["results"], json!([
        {"answer":0},{"answer":1},{"answer":2}
    ]));
}

#[test]
fn active_occurrence_limit_latches_once_and_closes_every_parallel_wait() {
    let fixture = Fixture::new();
    let mut model = starter_model();
    model.nodes = vec![
        ProcessNode { id:"Start_1".into(), name:"Start".into(), kind:ProcessNodeKind::Start, repeat:None },
        ProcessNode { id:"Fanout".into(), name:"Fan out".into(), kind:ProcessNodeKind::ParallelGateway, repeat:None },
    ];
    model.sequence_flows = vec![edge("EnterFanout", "Start_1", "Fanout")];
    for index in 0..5 {
        let node_id = format!("RepeatedWork_{index}");
        let target = format!("results_{index}");
        model.variables.insert(target.clone(), json!([]));
        model.nodes.push(ProcessNode {
            id:node_id.clone(), name:format!("Review branch {index}"),
            kind:ProcessNodeKind::UserTask {
                assignee_user_id:Some(fixture.owner.user_id.clone()),
                output_mapping:Default::default(),
            },
            repeat:Some(ProcessRepeatSpec::MultiInstance {
                mode:ProcessMultiInstanceMode::Parallel,
                input:ProcessMultiInstanceInput::Cardinality { count:16 },
                output_collection_variable:target,
            }),
        });
        model.sequence_flows.push(edge(&format!("Branch_{index}"), "Fanout", &node_id));
        model.sequence_flows.push(edge(&format!("Join_{index}"), &node_id, "Join"));
    }
    model.nodes.push(ProcessNode { id:"Join".into(), name:"Join".into(), kind:ProcessNodeKind::ParallelGateway, repeat:None });
    model.nodes.push(ProcessNode { id:"End_1".into(), name:"End".into(), kind:ProcessNodeKind::End, repeat:None });
    model.sequence_flows.push(edge("LeaveJoin", "Join", "End_1"));
    let started = start_model(&fixture, &model);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let snapshot = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    let latched = snapshot.repetition_groups.iter().filter(|group| group.terminal_capacity)
        .collect::<Vec<_>>();
    assert_eq!(latched.len(), 1);
    assert_eq!(latched[0].status, tentaflow_protocol::processes::ProcessRepetitionGroupStatus::Incident);
    assert_eq!(snapshot.instance.status, ProcessInstanceStatus::Cancelled);
    assert!(snapshot.repetition_occurrences.iter().all(|row|
        row.status == tentaflow_protocol::processes::ProcessRepetitionOccurrenceStatus::Cancelled));
    assert!(snapshot.user_tasks.iter().all(|task| task.status != ProcessUserTaskStatus::Open));
    let (events, _, _) = repository::list_events(&reopened, &fixture.owner,
        &started.instance_id, 0, 200).unwrap();
    let blocked = events.iter().filter(|event| event.kind == "repetition_group_blocked")
        .collect::<Vec<_>>();
    assert_eq!(blocked.len(), 1);
    assert_eq!(blocked[0].data["phase"], "capacity");
    assert_eq!(blocked[0].data["reason"], "active_occurrences");
    assert_eq!(super::call_tests::transition_rows(&fixture).len(), 16);
}

#[test]
fn active_service_job_limit_latches_before_the_thirty_third_claimable_job() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("repeated service capacity", None));
    let service = service_model(&flow_id, ActivityVerification::Human).nodes
        .into_iter().find(|node| node.id == "Service").unwrap();
    let mut model = starter_model();
    model.nodes = vec![
        ProcessNode { id:"Start_1".into(), name:"Start".into(), kind:ProcessNodeKind::Start, repeat:None },
        ProcessNode { id:"Fanout".into(), name:"Fan out".into(), kind:ProcessNodeKind::ParallelGateway, repeat:None },
    ];
    model.sequence_flows = vec![edge("EnterFanout", "Start_1", "Fanout")];
    for branch in 0..3 {
        let node_id = format!("RepeatedService_{branch}");
        let target = format!("results_{branch}");
        model.variables.insert(target.clone(), json!([]));
        let mut node = service.clone();
        node.id.clone_from(&node_id);
        node.name = format!("Process branch {branch}");
        node.repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode:ProcessMultiInstanceMode::Parallel,
            input:ProcessMultiInstanceInput::Cardinality { count:16 },
            output_collection_variable:target,
        });
        model.nodes.push(node);
        model.sequence_flows.push(edge(&format!("Branch_{branch}"), "Fanout", &node_id));
        model.sequence_flows.push(edge(&format!("Join_{branch}"), &node_id, "Join"));
    }
    model.nodes.push(ProcessNode { id:"Join".into(), name:"Join".into(), kind:ProcessNodeKind::ParallelGateway, repeat:None });
    model.nodes.push(ProcessNode { id:"End_1".into(), name:"End".into(), kind:ProcessNodeKind::End, repeat:None });
    model.sequence_flows.push(edge("LeaveJoin", "Join", "End_1"));
    let version = publish_model(&fixture, &model);
    let instance_id = Uuid::new_v4().to_string();
    let variables = serde_json::to_value(&model.variables).unwrap();
    let command = stamp("start parallel repeated Service branches");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&version.model, &instance_id, &fixture.owner,
        &version.definition_id, version.version, variables.clone(), StartCause::Manual,
        at_ms, manual_input(&command), None).unwrap();
    assert_eq!(plan.repetition_capacity.as_ref().unwrap().reason, "active_service_jobs");
    assert_eq!(plan.repetition_occurrences.len(), 48);
    assert_eq!(plan.create_jobs.len(), 32);
    let before = super::call_tests::transition_rows(&fixture);
    assert_eq!(before.len(), 16);
    let mut forged = plan.clone();
    forged.repetition_capacity.as_mut().unwrap().reason = "active_occurrences".into();
    repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables,
        repository::ProcessPlanInput::Supplied(&forged), at_ms).unwrap_err();
    assert_eq!(super::call_tests::transition_rows(&fixture), before);
    let mut extra_job = plan.clone();
    let mut invented = extra_job.create_jobs[0].clone();
    invented.job_id = Uuid::new_v4().to_string();
    extra_job.create_jobs.push(invented);
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables,
        repository::ProcessPlanInput::Supplied(&extra_job), at_ms).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), before);
    let committed = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    assert_eq!(committed.status, ProcessInstanceStatus::Cancelled);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::runtime_snapshot(&reopened, &fixture.owner, &instance_id).unwrap();
    assert_eq!(persisted.repetition_occurrences.len(), 48);
    assert!(persisted.repetition_occurrences.iter().all(|row|
        row.status == tentaflow_protocol::processes::ProcessRepetitionOccurrenceStatus::Cancelled));
    assert_eq!(persisted.repetition_groups.iter().filter(|group|
        group.terminal_capacity
            && group.status == tentaflow_protocol::processes::ProcessRepetitionGroupStatus::Incident).count(), 1);
    assert!(persisted.repetition_groups.iter().filter(|group| !group.terminal_capacity)
        .all(|group| group.status == tentaflow_protocol::processes::ProcessRepetitionGroupStatus::Cancelled));
    assert_eq!(persisted.jobs.len(), 32);
    assert!(persisted.jobs.iter().all(|job| job.status == "cancelled"));
    assert_eq!(persisted.repetition_groups.iter().filter(|group| group.terminal_capacity).count(), 1);
    let mut after_seq = 0;
    let mut blocked = Vec::new();
    loop {
        let (events, next_seq, has_more) = repository::list_events(
            &reopened, &fixture.owner, &instance_id, after_seq, 200).unwrap();
        blocked.extend(events.into_iter().filter(|event|
            event.kind == "repetition_group_blocked"));
        if !has_more {
            break;
        }
        assert!(next_seq > after_seq);
        after_seq = next_seq;
    }
    assert_eq!(blocked.len(), 1);
    assert_eq!(blocked[0].data["reason"], "active_service_jobs");
    let after = super::call_tests::transition_rows(&fixture);
    assert_eq!(repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables,
        repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap().status, ProcessInstanceStatus::Cancelled);
    assert_eq!(super::call_tests::transition_rows(&fixture), after);
}
