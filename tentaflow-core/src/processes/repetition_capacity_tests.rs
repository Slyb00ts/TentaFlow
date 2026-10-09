// ============ File: repetition_capacity_tests.rs — file-backed repetition byte admission regressions ============

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use tentaflow_protocol::processes::{
    ActivityOutcome, ActivityVerification, ProcessInstanceStatus, ProcessMultiInstanceInput,
    ProcessMultiInstanceMode, ProcessNode, ProcessNodeKind,
    ProcessRepeatSpec, ProcessRepetitionGroupStatus, ProcessRepetitionOccurrenceStatus,
    ProcessUserTaskKind, ProcessUserTaskStatus,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::jobs;
use super::repetition_tests::sequential_human_model;
use super::repository::{self, RepetitionDeniedBytes, RuntimePlan};
use super::runtime::{self, test_support::{edge, flow, graph, human_input, manual_input,
    publish_model, service_model, stamp, start_model, Fixture}};

const RETAINED_LIMIT: u64 = 64 * 1024 * 1024;

pub(super) fn physical_retained_bytes(fixture: &Fixture, group_id: &str) -> u64 {
    let conn = fixture.db.read().unwrap();
    let bytes: i64 = conn.query_row(
        "SELECT length(CAST(g.entry_variables_json AS BLOB))
            +COALESCE(length(CAST(g.frozen_collection_json AS BLOB)),0)
            +COALESCE(length(CAST(g.loop_state_json AS BLOB)),0)
            +COALESCE((SELECT SUM(length(CAST(o.item_json AS BLOB))
                +length(CAST(o.input_variables_json AS BLOB))
                +COALESCE(length(CAST(o.aggregate_item_json AS BLOB)),0)
                +COALESCE(length(CAST(o.state_patch_json AS BLOB)),0)
                +COALESCE(length(CAST(o.state_after_json AS BLOB)),0))
                FROM bpmn_repetition_occurrences o WHERE o.group_id=g.group_id),0)
            +COALESCE((SELECT SUM(length(CAST(j.input_json AS BLOB))
                +COALESCE(length(CAST(j.result_json AS BLOB)),0)) FROM bpmn_jobs j
                WHERE j.instance_id=g.instance_id AND j.job_id IN
                (SELECT o.job_id FROM bpmn_repetition_occurrences o WHERE o.group_id=g.group_id)),0)
            +COALESCE((SELECT SUM(length(CAST(v.observed_result_json AS BLOB)))
                FROM bpmn_service_invocations v
                WHERE v.instance_id=g.instance_id AND v.job_id IN
                (SELECT o.job_id FROM bpmn_repetition_occurrences o WHERE o.group_id=g.group_id)),0)
            +COALESCE((SELECT SUM(length(CAST(t.outputs_json AS BLOB))) FROM bpmn_user_tasks t
                WHERE t.instance_id=g.instance_id AND t.user_task_id IN
                (SELECT o.user_task_id FROM bpmn_repetition_occurrences o WHERE o.group_id=g.group_id
                 UNION SELECT o.verification_user_task_id FROM bpmn_repetition_occurrences o WHERE o.group_id=g.group_id)),0)
            +COALESCE((SELECT SUM(COALESCE(length(CAST(m.payload_json AS BLOB)),0))
                FROM bpmn_messages m WHERE m.source_instance_id=g.instance_id
                AND m.source_activation_id IN (SELECT o.token_id FROM bpmn_repetition_occurrences o
                    WHERE o.group_id=g.group_id)),0)
            +COALESCE((SELECT SUM(COALESCE(length(CAST(s.message_name AS BLOB)),0)
                +COALESCE(length(CAST(s.correlation_key AS BLOB)),0))
                FROM bpmn_event_subscriptions s WHERE s.instance_id=g.instance_id
                AND s.token_id IN (SELECT o.token_id FROM bpmn_repetition_occurrences o
                    WHERE o.group_id=g.group_id)),0)
            +COALESCE((SELECT SUM(length(CAST(s.local_variables_json AS BLOB)))
                FROM bpmn_scopes s WHERE s.instance_id=g.instance_id
                AND s.parent_token_id IN (SELECT o.token_id FROM bpmn_repetition_occurrences o
                    WHERE o.group_id=g.group_id)),0)
            +COALESCE((SELECT SUM(length(CAST(i.variables_json AS BLOB)))
                FROM bpmn_calls c JOIN bpmn_instances i ON i.instance_id=c.child_instance_id
                WHERE c.parent_instance_id=g.instance_id
                AND c.parent_token_id IN (SELECT o.token_id FROM bpmn_repetition_occurrences o
                    WHERE o.group_id=g.group_id)),0)
            +COALESCE((SELECT SUM(length(CAST(e.data_json AS BLOB))) FROM bpmn_events e
                WHERE e.instance_id=g.instance_id AND (
                    json_extract(e.data_json,'$.group_id')=g.group_id
                    OR e.event_id IN (SELECT o.accepted_source_event_id FROM bpmn_repetition_occurrences o WHERE o.group_id=g.group_id
                        UNION SELECT o.approval_event_id FROM bpmn_repetition_occurrences o WHERE o.group_id=g.group_id)
                    OR (e.kind='service_claimed' AND json_extract(e.data_json,'$.job_id') IN
                        (SELECT o.job_id FROM bpmn_repetition_occurrences o WHERE o.group_id=g.group_id))
                )),0)
            +COALESCE((SELECT SUM(length(CAST(i.message AS BLOB))) FROM bpmn_incidents i
                WHERE i.instance_id=g.instance_id AND (i.incident_id=g.terminal_incident_id
                    OR i.job_id IN (SELECT o.job_id FROM bpmn_repetition_occurrences o WHERE o.group_id=g.group_id))),0)
            FROM bpmn_repetition_groups g WHERE g.group_id=?1",
        [group_id], |row| row.get(0),
    ).unwrap();
    u64::try_from(bytes).unwrap()
}

fn measured_retained_bytes(fixture: &Fixture, instance_id: &str) -> u64 {
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, instance_id).unwrap();
    snapshot.repetition_groups.iter().map(|group| {
        let physical = physical_retained_bytes(fixture, &group.group_id);
        assert_eq!(group.retained_bytes, physical, "retained counter differs from UTF-8 SQL bytes");
        physical
    }).sum()
}

fn event_data(fixture: &Fixture, instance_id: &str, event_id: &str) -> (String, Value) {
    let conn = fixture.db.read().unwrap();
    let (kind, raw): (String, String) = conn.query_row(
        "SELECT kind,data_json FROM bpmn_events WHERE instance_id=?1 AND event_id=?2",
        rusqlite::params![instance_id, event_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    (kind, serde_json::from_str(&raw).unwrap())
}

fn large_loop_model(owner: &str, padding_bytes: usize) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = sequential_human_model(owner, 0);
    model.variables.insert("padding".into(), json!("é".repeat(padding_bytes / 2)));
    model.variables.insert("step".into(), json!(0));
    model.nodes.push(ProcessNode { id: "Split".into(), name: "Split".into(),
        kind: ProcessNodeKind::ParallelGateway, repeat: None, activity_io: None,});
    model.nodes.push(ProcessNode { id: "Join".into(), name: "Join".into(),
        kind: ProcessNodeKind::ParallelGateway, repeat: None, activity_io: None,});
    let prototype = model.nodes.iter().find(|node| node.id == "RepeatedWork").unwrap().clone();
    model.nodes.retain(|node| node.id != "RepeatedWork");
    model.sequence_flows = vec![edge("EnterSplit", "Start_1", "Split")];
    for branch in 0..5 {
        let id = format!("RepeatedWork_{branch}");
        let output = format!("results_{branch}");
        model.variables.insert(output.clone(), json!([]));
        let mut node = prototype.clone();
        node.id.clone_from(&id);
        node.name = format!("Review branch {branch}");
        node.repeat = Some(ProcessRepeatSpec::StructuredLoop {
            condition: "vars.step < 32".into(), test_before: false,
            max_iterations: 32, output_collection_variable: output,
        });
        if let ProcessNodeKind::UserTask { output_mapping, .. } = &mut node.kind {
            *output_mapping = BTreeMap::from([("step".into(), "outputs.next_step".into())]);
        } else { panic!("fixture Work node is not a UserTask") }
        model.nodes.push(node);
        model.sequence_flows.push(edge(&format!("Branch_{branch}"), "Split", &id));
        model.sequence_flows.push(edge(&format!("Return_{branch}"), &id, "Join"));
    }
    model.sequence_flows.push(edge("LeaveJoin", "Join", "End_1"));
    model
}

fn large_loop_with_repeated_service(
    owner: &str, flow_id: &str,
) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = large_loop_model(owner, 80_000);
    model.variables.insert("service_items".into(), json!(["observed"]));
    model.variables.insert("service_results".into(), json!([]));
    let mut service = service_model(flow_id, ActivityVerification::Condition {
        expression: "true".into(),
    }).nodes.into_iter().find(|node| node.id == "Service").unwrap();
    service.id = "RepeatedService".into();
    let ProcessNodeKind::ServiceTask { output_mapping, result_expression, .. } =
        &mut service.kind else { panic!("fixture Service changed kind") };
    output_mapping.clear();
    *result_expression = Some("outputs.variables.actual_result".into());
    service.repeat = Some(ProcessRepeatSpec::MultiInstance {
        mode: ProcessMultiInstanceMode::Sequential,
        input: ProcessMultiInstanceInput::CollectionExpression {
            expression: "vars.service_items".into(),
        },
        output_collection_variable: "service_results".into(),
    });
    model.nodes.push(service);
    model.sequence_flows.push(edge("ServiceBranch", "Split", "RepeatedService"));
    model.sequence_flows.push(edge("ServiceJoin", "RepeatedService", "Join"));
    model
}

fn parallel_manual_parent_aggregate_model(owner: &str, flow_id: &str)
    -> tentaflow_protocol::processes::ProcessModel {
    let mut model = sequential_human_model(owner, 0);
    model.nodes.retain(|node| node.id != "RepeatedWork");
    model.variables.remove("results");
    model.variables.insert("padding".into(), json!("é".repeat(123_844)));
    model.variables.insert("service_marker".into(), json!(""));
    model.variables.insert("fine".into(), json!(""));
    let mut service = service_model(flow_id, ActivityVerification::Condition {
        expression: "true".into(),
    }).nodes.into_iter().find(|node| node.id == "Service").unwrap();
    let ProcessNodeKind::ServiceTask { output_mapping, .. } = &mut service.kind else {
        panic!("actual Service fixture changed kind")
    };
    *output_mapping = BTreeMap::from([(
        "service_marker".into(), "outputs.variables.marker".into(),
    )]);
    model.nodes.push(service);
    model.nodes.push(ProcessNode {
        id: "FineTune".into(), name: "Tune actual accepted input".into(),
        kind: ProcessNodeKind::UserTask {
            assignee_user_id: Some(owner.into()),
            output_mapping: BTreeMap::from([("fine".into(), "outputs.fine".into())]),
        }, repeat: None,
        activity_io: None,
    });
    for index in 0..4 {
        let output = format!("manual_results_{index}");
        model.variables.insert(output.clone(), json!([]));
        model.nodes.push(ProcessNode {
            id: format!("RepeatedManual_{index}"),
            name: format!("Acknowledge actual ordinal {index}"),
            kind: ProcessNodeKind::ManualTask {
                assignee_user_id: Some(owner.into()),
                instructions: "Acknowledge the physical work item.".into(),
            },
            repeat: Some(ProcessRepeatSpec::StructuredLoop {
                condition: "true".into(), test_before: false,
                max_iterations: 32, output_collection_variable: output,
            }),
            activity_io: None,
        });
    }
    for index in 0..2 {
        let output = format!("parallel_results_{index}");
        model.variables.insert(output.clone(), json!([]));
        model.nodes.push(ProcessNode {
            id: format!("ParallelManual_{index}"),
            name: format!("Acknowledge parallel ordinals {index}"),
            kind: ProcessNodeKind::ManualTask {
                assignee_user_id: Some(owner.into()),
                instructions: "Acknowledge the selected physical work item.".into(),
            },
            repeat: Some(ProcessRepeatSpec::MultiInstance {
                mode: ProcessMultiInstanceMode::Parallel,
                input: ProcessMultiInstanceInput::Cardinality { count: 2 },
                output_collection_variable: output,
            }),
            activity_io: None,
        });
    }
    model.sequence_flows = vec![
        edge("EnterManual_0", "Start_1", "RepeatedManual_0"),
        edge("Manual_0ToManual_1", "RepeatedManual_0", "RepeatedManual_1"),
        edge("Manual_1ToManual_2", "RepeatedManual_1", "RepeatedManual_2"),
        edge("Manual_2ToManual_3", "RepeatedManual_2", "RepeatedManual_3"),
        edge("Manual_3ToParallel_0", "RepeatedManual_3", "ParallelManual_0"),
        edge("Parallel_0ToService", "ParallelManual_0", "Service"),
        edge("ServiceToFineTune", "Service", "FineTune"),
        edge("FineTuneToParallel_1", "FineTune", "ParallelManual_1"),
        edge("Parallel_1ToEnd", "ParallelManual_1", "End_1"),
    ];
    model
}

fn verification_capacity_model(owner: &str, flow_id: &str, needs_human: bool)
    -> tentaflow_protocol::processes::ProcessModel {
    let mut model = service_model(flow_id, ActivityVerification::Human);
    model.variables.insert("service_results".into(), json!([]));
    let service = model.nodes.iter_mut().find(|node| node.id == "Service").unwrap();
    service.repeat = Some(ProcessRepeatSpec::MultiInstance {
        mode: ProcessMultiInstanceMode::Sequential,
        input: ProcessMultiInstanceInput::Cardinality { count: 1 },
        output_collection_variable: "service_results".into(),
    });
    if needs_human {
        let ProcessNodeKind::ServiceTask { result_expression, .. } = &mut service.kind else {
            panic!("fixture Service node changed kind")
        };
        *result_expression = Some("outputs.variables.actual_result".into());
    }
    model.nodes.extend([
        ProcessNode { id: "OuterSplit".into(), name: "Parallel source and gate".into(),
            kind: ProcessNodeKind::ParallelGateway, repeat: None, activity_io: None,},
        ProcessNode { id: "OuterJoin".into(), name: "Join after verification".into(),
            kind: ProcessNodeKind::ParallelGateway, repeat: None, activity_io: None,},
        ProcessNode { id: "GateHuman".into(), name: "Release the work fanout".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: Some(owner.into()), output_mapping: BTreeMap::new(),
            }, repeat: None, activity_io: None,},
        ProcessNode { id: "InnerSplit".into(), name: "Start four real repetitions".into(),
            kind: ProcessNodeKind::ParallelGateway, repeat: None, activity_io: None,},
        ProcessNode { id: "InnerJoin".into(), name: "Join four repetitions".into(),
            kind: ProcessNodeKind::ParallelGateway, repeat: None, activity_io: None,},
    ]);
    let prototype = sequential_human_model(owner, 16).nodes.into_iter()
        .find(|node| node.id == "RepeatedWork").unwrap();
    model.sequence_flows = vec![
        edge("RootToOuter", "Start_1", "OuterSplit"),
        edge("OuterToService", "OuterSplit", "Service"),
        edge("ServiceToOuterJoin", "Service", "OuterJoin"),
        edge("OuterToGate", "OuterSplit", "GateHuman"),
        edge("GateToInner", "GateHuman", "InnerSplit"),
        edge("InnerToOuter", "InnerJoin", "OuterJoin"),
        edge("OuterToEnd", "OuterJoin", "End_1"),
    ];
    for branch in 0..4 {
        let id = format!("HumanRepeat_{branch}");
        let output = format!("human_results_{branch}");
        model.variables.insert(output.clone(), json!([]));
        let mut node = prototype.clone();
        node.id.clone_from(&id);
        node.name = format!("Review capacity branch {branch}");
        node.repeat = Some(ProcessRepeatSpec::MultiInstance {
            mode: ProcessMultiInstanceMode::Parallel,
            input: ProcessMultiInstanceInput::Cardinality { count: 16 },
            output_collection_variable: output,
        });
        model.nodes.push(node);
        model.sequence_flows.push(edge(&format!("InnerBranch_{branch}"), "InnerSplit", &id));
        model.sequence_flows.push(edge(&format!("InnerReturn_{branch}"), &id, "InnerJoin"));
    }
    model
}

fn open_work(snapshot: &repository::RuntimeSnapshot) -> (String, u32, String) {
    let occurrence = snapshot.repetition_occurrences.iter()
        .filter(|row| row.status == ProcessRepetitionOccurrenceStatus::Active
            && row.user_task_id.as_ref().is_some_and(|task_id|
                snapshot.user_tasks.iter().any(|task|
                    task.user_task_id == *task_id
                        && task.status == ProcessUserTaskStatus::Open)))
        .min_by_key(|row| (row.ordinal, row.group_id.as_str()))
        .expect("actual open repetition occurrence");
    let task_id = occurrence.user_task_id.as_ref().expect("actual repeated Work task");
    assert!(snapshot.user_tasks.iter().any(|task|
        task.user_task_id == *task_id && task.status == ProcessUserTaskStatus::Open));
    (occurrence.group_id.clone(), occurrence.ordinal, task_id.clone())
}

fn planned_completion(snapshot: &repository::RuntimeSnapshot,
    task_id: &str, command: &repository::CommandStamp, outputs: &Value, at_ms: i64) -> RuntimePlan {
    runtime::plan_user_completion(snapshot, task_id, outputs, None, at_ms,
        human_input(snapshot, task_id, command), None).unwrap()
}

fn accepted_prefix_capacity(plan: &RuntimePlan) -> bool {
    plan.repetition_capacity.as_ref().is_some_and(|source|
        source.reason == "repetition_bytes"
            && !matches!(source.denied_candidate.as_ref(),
                Some(RepetitionDeniedBytes::Occurrence { .. })))
}

fn assert_rejected_unchanged(fixture: &Fixture, instance_id: &str,
    snapshot: &repository::RuntimeSnapshot, task_id: &str,
    command: &repository::CommandStamp, outputs: &Value, plan: &RuntimePlan, at_ms: i64,
    before: &[Vec<Vec<rusqlite::types::Value>>]) {
    assert!(repository::complete_user_task(&fixture.db, &fixture.owner, command,
        instance_id, task_id, snapshot.instance.revision, outputs, None, repository::ProcessPlanInput::Supplied(plan), at_ms).is_err());
    assert_eq!(super::call_tests::transition_rows(fixture), before);
}

fn assert_capacity_forgeries(fixture: &Fixture, instance_id: &str,
    snapshot: &repository::RuntimeSnapshot, task_id: &str,
    command: &repository::CommandStamp, outputs: &Value, canonical: &RuntimePlan, at_ms: i64) {
    let before = super::call_tests::transition_rows(fixture);
    let capacity = canonical.repetition_capacity.as_ref().expect("canonical capacity latch");
    assert_eq!(capacity.reason, "repetition_bytes");
    assert!(canonical.events.iter().any(|event| event.kind == "user_task_completed"));

    let mut wrong_input = canonical.clone();
    wrong_input.repetition_capacity.as_mut().unwrap().accepted_input =
        repository::AcceptedInputRef::PersistedReady {
            token_id: capacity.source_token_id.clone(),
            expected_instance_revision: snapshot.instance.revision,
        };
    let mut wrong_retained = canonical.clone();
    wrong_retained.repetition_capacity.as_mut().unwrap().retained_before_closure_bytes += 1;
    let mut wrong_denial = canonical.clone();
    wrong_denial.repetition_capacity.as_mut().unwrap().denied_candidate =
        Some(RepetitionDeniedBytes::Occurrence { ordinal: u32::MAX });
    let mut wrong_candidate_bytes = canonical.clone();
    wrong_candidate_bytes.repetition_capacity.as_mut().unwrap().denied_candidate_bytes =
        Some(capacity.denied_candidate_bytes.unwrap_or(RETAINED_LIMIT) + 1);
    let mut duplicate_blocked = canonical.clone();
    let blocked = duplicate_blocked.events.iter().find(|event|
        event.kind == "repetition_group_blocked").unwrap().clone();
    duplicate_blocked.events.push(blocked);
    let mut wrong_incident = canonical.clone();
    wrong_incident.repetition_capacity.as_mut().unwrap().incident_id = Uuid::new_v4().to_string();
    let mut wrong_event = canonical.clone();
    wrong_event.repetition_capacity.as_mut().unwrap().event_id = Uuid::new_v4().to_string();
    let mut missing_completion = canonical.clone();
    missing_completion.complete_user_task_ids.retain(|id| id != task_id);
    let mut foreign_closure = canonical.clone();
    foreign_closure.cancel_scope_roots.push(Uuid::new_v4().to_string());
    let mut foreign_cancellation = canonical.clone();
    foreign_cancellation.cancel_token_ids.push(Uuid::new_v4().to_string());
    let mut extra_occurrence = canonical.clone();
    let mut occurrence = extra_occurrence.repetition_occurrences.iter()
        .find(|row| row.group_id == capacity.group_id).unwrap().clone();
    occurrence.occurrence_id = Uuid::new_v4().to_string();
    extra_occurrence.repetition_occurrences.push(occurrence);
    let mut extra_task = canonical.clone();
    let mut task = snapshot.user_tasks.iter().find(|task|
        task.user_task_id == task_id).unwrap().clone();
    task.user_task_id = Uuid::new_v4().to_string();
    extra_task.create_user_tasks.push(task);
    let mut forgeries = vec![wrong_input, wrong_retained, wrong_denial, wrong_candidate_bytes,
        duplicate_blocked, wrong_incident, wrong_event, missing_completion,
        foreign_closure, foreign_cancellation, extra_occurrence, extra_task];
    if let Some(RepetitionDeniedBytes::Occurrence { ordinal }) = &capacity.denied_candidate {
        let mut adjacent_ordinal = canonical.clone();
        adjacent_ordinal.repetition_capacity.as_mut().unwrap().denied_candidate =
            Some(RepetitionDeniedBytes::Occurrence { ordinal: ordinal + 1 });
        forgeries.push(adjacent_ordinal);
    }
    if let Some((condition_index, condition)) = canonical.events.iter().enumerate()
        .find(|(_, event)| event.kind == "repetition_condition_checked"
            && event.data["group_id"] == capacity.group_id
            && event.data["phase"] == "after_accepted") {
        assert_eq!(condition.data["matched"], true);
        let completion_index = canonical.events.iter().position(|event|
            event.kind == "repetition_occurrence_completed"
                && event.data["group_id"] == capacity.group_id).unwrap();
        assert!(completion_index < condition_index && condition_index < capacity.event_index);
        assert!(!canonical.event_ids.contains_key(&completion_index)
            && !canonical.event_ids.contains_key(&condition_index)
            && !canonical.event_sources.contains_key(&completion_index)
            && !canonical.event_sources.contains_key(&condition_index));
        let mut duplicate_condition = canonical.clone();
        duplicate_condition.events.push(condition.clone());
        forgeries.push(duplicate_condition);
        let source_id = condition.data["previous_source_event_id"].as_str().unwrap();
        let earlier = repository::list_events(&fixture.db, &fixture.owner, instance_id, 0, 200)
            .unwrap().0.into_iter().find(|event|
                event.kind == "user_task_completed" && event.event_id != source_id).unwrap();
        assert_eq!(earlier.scope_id, condition.scope_id);
        let mut wrong_source = canonical.clone();
        wrong_source.events[condition_index].data["previous_source_event_id"] =
            json!(earlier.event_id);
        forgeries.push(wrong_source);
        let mut reordered = canonical.clone();
        reordered.events.swap(completion_index, condition_index);
        forgeries.push(reordered);
    }
    for forged in forgeries {
        assert_rejected_unchanged(fixture, instance_id, snapshot, task_id,
            command, outputs, &forged, at_ms, &before);
    }
}

fn complete_capacity_control(expect_accepted_prefix: bool) {
    let fixture = Fixture::new();
    let model = large_loop_model(&fixture.owner.user_id, 236_000);
    let started = start_model(&fixture, &model);
    assert_eq!(started.status, ProcessInstanceStatus::Waiting);
    let mut accepted = 0_u32;
    loop {
        assert!(accepted < 160, "64 MiB limit was not reached by bounded real Work completions");
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let (_, ordinal, task_id) = open_work(&snapshot);
        let measured_before = measured_retained_bytes(&fixture, &started.instance_id);
        assert!(measured_before <= RETAINED_LIMIT);
        let command = stamp("accept factual Work output at the repetition byte boundary");
        let at_ms = chrono::Utc::now().timestamp_millis();
        let small = json!({"next_step":ordinal + 1});
        let small_plan = planned_completion(&snapshot, &task_id, &command, &small, at_ms);
        let mut large = json!({"next_step":ordinal + 1,"evidence":"x".repeat(240_000)});
        assert!(serde_json::to_vec(&large).unwrap().len() < 256 * 1024);
        let mut large_plan = Some(planned_completion(&snapshot, &task_id, &command, &large, at_ms));
        if expect_accepted_prefix && large_plan.as_ref().is_some_and(accepted_prefix_capacity)
            && !accepted_prefix_capacity(&small_plan) {
            let empty = json!({"next_step":ordinal + 1,"evidence":""});
            let empty_plan = planned_completion(&snapshot, &task_id, &command, &empty, at_ms);
            if accepted_prefix_capacity(&empty_plan) {
                large = empty;
                large_plan = Some(empty_plan);
            } else {
                let mut below = 0_usize;
                let mut above = 240_000_usize;
                while below + 1 < above {
                    let middle = below + (above - below) / 2;
                    let output = json!({"next_step":ordinal + 1,"evidence":"x".repeat(middle)});
                    let probe = planned_completion(&snapshot, &task_id, &command, &output, at_ms);
                    if accepted_prefix_capacity(&probe) { above = middle; }
                    else { below = middle; }
                }
                large = json!({"next_step":ordinal + 1,"evidence":"x".repeat(above)});
                large_plan = Some(planned_completion(&snapshot, &task_id, &command, &large, at_ms));
                let shorter = json!({"next_step":ordinal + 1,"evidence":"x".repeat(above - 1)});
                assert!(!accepted_prefix_capacity(&planned_completion(&snapshot,
                    &task_id, &command, &shorter, at_ms)),
                    "one fewer UTF-8 output byte must not cross the accepted-prefix boundary");
            }
        }
        if !expect_accepted_prefix {
            assert!(!small_plan.repetition_capacity.as_ref().is_some_and(|source|
                !matches!(source.denied_candidate.as_ref(),
                    Some(RepetitionDeniedBytes::Occurrence { .. }))),
                "a factual next-ordinal candidate must be selected before a coarse small-output prefix latch");
            if small_plan.repetition_capacity.is_none()
                && large_plan.as_ref().unwrap().repetition_capacity.is_some() {
                let empty = json!({"next_step":ordinal + 1,"evidence":""});
                let empty_plan = planned_completion(&snapshot, &task_id, &command, &empty, at_ms);
                if empty_plan.repetition_capacity.is_some() {
                    large = empty;
                    large_plan = Some(empty_plan);
                } else {
                    let mut below = 0_usize;
                    let mut above = 240_000_usize;
                    while below + 1 < above {
                        let middle = below + (above - below) / 2;
                        let output = json!({"next_step":ordinal + 1,"evidence":"x".repeat(middle)});
                        let probe = planned_completion(&snapshot, &task_id, &command, &output, at_ms);
                        if probe.repetition_capacity.is_some() { above = middle; }
                        else { below = middle; }
                    }
                    large = json!({"next_step":ordinal + 1,"evidence":"x".repeat(above)});
                    large_plan = Some(planned_completion(&snapshot,
                        &task_id, &command, &large, at_ms));
                    let shorter = json!({"next_step":ordinal + 1,"evidence":"x".repeat(above - 1)});
                    assert!(planned_completion(&snapshot, &task_id, &command,
                        &shorter, at_ms).repetition_capacity.is_none(),
                        "one fewer UTF-8 output byte must leave the real next-ordinal candidate admissible");
                }
                assert!(large_plan.as_ref().unwrap().repetition_capacity.as_ref().is_some_and(|source|
                    matches!(source.denied_candidate.as_ref(),
                        Some(RepetitionDeniedBytes::Occurrence { .. }))),
                    "the first physical byte boundary must deny the next ordinal after accepting Work");
            }
        }
        let selected = large_plan.as_ref().and_then(|plan| plan.repetition_capacity.as_ref())
            .filter(|source| source.reason == "repetition_bytes"
                && (if expect_accepted_prefix {
                    !matches!(source.denied_candidate.as_ref(),
                        Some(RepetitionDeniedBytes::Occurrence { .. }))
                } else {
                    matches!(source.denied_candidate.as_ref(),
                        Some(RepetitionDeniedBytes::Occurrence { .. }))
                }))
            .map(|_| (&large, large_plan.as_ref().unwrap()))
            .unwrap_or((&small, &small_plan));
        if let Some(source) = &selected.1.repetition_capacity {
            assert_eq!(source.reason, "repetition_bytes");
            assert_eq!(source.before_retained_bytes, measured_before);
            assert!(source.denied_candidate_bytes.unwrap_or(source.retained_before_closure_bytes)
                > RETAINED_LIMIT);
            if expect_accepted_prefix {
                assert!(!matches!(source.denied_candidate.as_ref(),
                    Some(RepetitionDeniedBytes::Occurrence { .. })),
                    "pre-spawn capacity closed before the accepted-prefix boundary");
            } else {
                assert!(matches!(source.denied_candidate.as_ref(),
                    Some(RepetitionDeniedBytes::Occurrence { .. })),
                    "accepted-prefix capacity closed before the denied next ordinal");
            }
            assert_capacity_forgeries(&fixture, &started.instance_id, &snapshot,
                &task_id, &command, selected.0, selected.1, at_ms);
            let committed = repository::complete_user_task(&fixture.db, &fixture.owner,
                &command, &started.instance_id, &task_id, snapshot.instance.revision,
                selected.0, None, repository::ProcessPlanInput::Supplied(selected.1), at_ms).unwrap();
            assert_eq!(committed.instance.status, ProcessInstanceStatus::Cancelled);
            let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
            let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
                &started.instance_id).unwrap();
            let group = persisted.repetition_groups.iter().find(|row|
                row.group_id == source.group_id).unwrap();
            assert_eq!(group.status, ProcessRepetitionGroupStatus::Incident);
            assert!(group.terminal_capacity);
            assert_eq!(group.terminal_incident_id.as_deref(), Some(source.incident_id.as_str()));
            assert_eq!(group.terminal_event_id.as_deref(), Some(source.event_id.as_str()));
            assert!(persisted.incidents.iter().any(|incident|
                incident.incident_id == source.incident_id && incident.code == "REPETITION_LIMIT"));
            let conn = fixture.db.read().unwrap();
            let (task_status, task_outputs): (String, String) = conn.query_row(
                "SELECT status,outputs_json FROM bpmn_user_tasks WHERE instance_id=?1 AND user_task_id=?2",
                rusqlite::params![started.instance_id, task_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).unwrap();
            assert_eq!(task_status, "completed");
            assert_eq!(serde_json::from_str::<Value>(&task_outputs).unwrap(), *selected.0);
            let accepted_events: i64 = conn.query_row(
                "SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1 AND kind='user_task_completed' AND json_extract(data_json,'$.user_task_id')=?2",
                rusqlite::params![started.instance_id, task_id], |row| row.get(0),
            ).unwrap();
            assert_eq!(accepted_events, 1);
            drop(conn);
            if let Some(RepetitionDeniedBytes::Occurrence { ordinal }) = &source.denied_candidate {
                assert!(!persisted.repetition_occurrences.iter().any(|row|
                    row.group_id == source.group_id && row.ordinal == *ordinal));
            }
            assert!(!persisted.tokens.iter().any(|token| token.node_id == "End_1"));
            let (kind, data) = event_data(&fixture, &started.instance_id, &source.event_id);
            assert_eq!(kind, "repetition_group_blocked");
            assert_eq!(data["reason"], "repetition_bytes");
            assert_eq!(data["incident_id"], source.incident_id);
            assert_eq!(measured_retained_bytes(&fixture, &started.instance_id),
                persisted.repetition_groups.iter().map(|row| row.retained_bytes).sum::<u64>());
            let after = super::call_tests::transition_rows(&fixture);
            repository::complete_user_task(&reopened, &fixture.owner, &command,
                &started.instance_id, &task_id, snapshot.instance.revision,
                selected.0, None, repository::ProcessPlanInput::Supplied(selected.1), at_ms).unwrap();
            assert_eq!(super::call_tests::transition_rows(&fixture), after);
            assert!(repository::cancel_instance(&reopened, &fixture.owner,
                &stamp("cancel already latched repetition"), &started.instance_id,
                persisted.instance.revision).is_err());
            assert_eq!(super::call_tests::transition_rows(&fixture), after);
            return;
        }
        repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &started.instance_id, &task_id, snapshot.instance.revision,
            &small, None, repository::ProcessPlanInput::Supplied(&small_plan), at_ms).unwrap();
        accepted += 1;
    }
}

#[test]
fn physical_utf8_capacity_latches_after_a_real_accepted_work_source() {
    complete_capacity_control(true);
}

#[test]
fn physical_utf8_capacity_latches_before_denied_next_ordinal_and_owner_cancel_keeps_history() {
    complete_capacity_control(false);
}

#[tokio::test]
async fn final_parallel_manual_parent_aggregate_keeps_highest_ordinal_source() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph(&"s".repeat(128), None));
    let model = parallel_manual_parent_aggregate_model(&fixture.owner.user_id, &flow_id);
    let started = start_model(&fixture, &model);
    assert_eq!(started.status, ProcessInstanceStatus::Waiting);

    for group_index in 0..4 {
        for ordinal in 0..32 {
            let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
                &started.instance_id).unwrap();
            let node_id = format!("RepeatedManual_{group_index}");
            let task = snapshot.user_tasks.iter().find(|task|
                task.node_id == node_id && task.kind == ProcessUserTaskKind::Manual
                    && task.status == ProcessUserTaskStatus::Open).unwrap();
            let occurrence = snapshot.repetition_occurrences.iter().find(|row|
                row.user_task_id.as_deref() == Some(task.user_task_id.as_str())).unwrap();
            assert_eq!(occurrence.ordinal, ordinal);
            let command = stamp("accept real Manual byte-prefix ordinal");
            let at_ms = chrono::Utc::now().timestamp_millis();
            let plan = runtime::plan_manual_acknowledgment(&snapshot, &task.user_task_id,
                &fixture.owner.user_id, at_ms,
                super::manual_tests::manual_entry(&snapshot, &task.user_task_id, &command),
                None).unwrap();
            assert!(plan.repetition_capacity.is_none(),
                "the factual prefix must remain below the capacity limit");
            repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
                &command, &started.instance_id, &task.user_task_id,
                snapshot.instance.revision, repository::ProcessPlanInput::Supplied(&plan),
                at_ms).unwrap();
        }
        let after_group = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let node_id = format!("RepeatedManual_{group_index}");
        assert!(after_group.repetition_groups.iter().any(|group|
            group.node_id == node_id && group.status == ProcessRepetitionGroupStatus::Completed
                && group.completed_count == 32));
        assert!(measured_retained_bytes(&fixture, &started.instance_id) <= RETAINED_LIMIT,
            "each real completed UTF-8 prefix must stay under the physical limit");
    }
    let before_calibration = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let root_variables_before_calibration =
        serde_json::to_vec(&before_calibration.instance.variables).unwrap().len();
    let calibration_group = before_calibration.repetition_groups.iter().find(|group|
        group.node_id == "ParallelManual_0").unwrap();
    assert_eq!(calibration_group.mode,
        tentaflow_protocol::processes::ProcessRepetitionGroupMode::MultiInstanceParallel);
    assert_eq!(calibration_group.created_count, 2);
    for ordinal in [1, 0] {
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let occurrence = snapshot.repetition_occurrences.iter().find(|row|
            row.group_id == calibration_group.group_id && row.ordinal == ordinal).unwrap();
        let task_id = occurrence.user_task_id.as_ref().unwrap();
        let command = stamp("accept real calibration parallel Manual ordinal");
        let at_ms = chrono::Utc::now().timestamp_millis();
        let plan = runtime::plan_manual_acknowledgment(&snapshot, task_id,
            &fixture.owner.user_id, at_ms,
            super::manual_tests::manual_entry(&snapshot, task_id, &command), None).unwrap();
        assert!(plan.repetition_capacity.is_none());
        repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
            &command, &started.instance_id, task_id, snapshot.instance.revision,
            repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    }
    let after_calibration = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let calibration_group = after_calibration.repetition_groups.iter().find(|group|
        group.node_id == "ParallelManual_0").unwrap();
    assert_eq!(calibration_group.status, ProcessRepetitionGroupStatus::Completed);
    assert_eq!(calibration_group.completed_count, 2);
    let calibration_bytes = physical_retained_bytes(&fixture, &calibration_group.group_id);
    let retained_before_service = measured_retained_bytes(&fixture, &started.instance_id);
    assert!(retained_before_service <= RETAINED_LIMIT);
    let completed_event_bytes: i64 = fixture.db.read().unwrap().query_row(
        "SELECT length(CAST(data_json AS BLOB)) FROM bpmn_events \
         WHERE instance_id=?1 AND kind='repetition_completed' \
         AND json_extract(data_json,'$.group_id')=?2",
        rusqlite::params![started.instance_id, calibration_group.group_id],
        |row| row.get(0),
    ).unwrap();
    let completed_event_bytes = u64::try_from(completed_event_bytes).unwrap();

    let worker = "parallel-parent-aggregate-source-worker";
    let claim = repository::claim_job(&fixture.db, worker,
        chrono::Utc::now().timestamp_millis()).unwrap().expect("real Service claim");
    assert_eq!(claim.job.node_id, "Service");
    jobs::execute_claimed(&fixture.db, fixture.dispatcher(), worker, claim.clone(),
        CancellationToken::new()).await.unwrap();
    let after_service = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(after_service.instance.variables["service_marker"], json!("s".repeat(128)));
    assert_eq!(measured_retained_bytes(&fixture, &started.instance_id),
        retained_before_service, "ordinary Service cannot synthesize repetition retention");
    let service_event_id: String = fixture.db.read().unwrap().query_row(
        "SELECT event_id FROM bpmn_events WHERE instance_id=?1 \
         AND kind='service_result' AND node_id='Service'",
        [&started.instance_id], |row| row.get(0),
    ).unwrap();
    let root_variables_after_service =
        serde_json::to_vec(&after_service.instance.variables).unwrap().len();
    let delta_without_fine = i128::try_from(root_variables_after_service).unwrap()
        - i128::try_from(root_variables_before_calibration).unwrap();
    assert!(delta_without_fine > 128,
        "the actual prior parallel aggregate and Service mapping must enlarge root variables");
    let root_copies = 3_i128;
    let baseline_before_completion = i128::from(retained_before_service)
        + i128::from(calibration_bytes) + root_copies * delta_without_fine
        - i128::from(completed_event_bytes);
    let smallest_fine = (i128::from(RETAINED_LIMIT) - baseline_before_completion
        - i128::from(completed_event_bytes)).div_euclid(root_copies) + 1;
    let largest_fine = (i128::from(RETAINED_LIMIT) - baseline_before_completion)
        .div_euclid(root_copies);
    assert!(smallest_fine >= 0 && smallest_fine <= largest_fine,
        "real parallel group must have a reachable UTF-8 completion window: \
         baseline={baseline_before_completion}, event={completed_event_bytes}, \
         range={smallest_fine}..={largest_fine}");
    let fine_bytes = usize::try_from(smallest_fine).unwrap();
    let fine_outputs = json!({"fine":"f".repeat(fine_bytes)});
    let mut projected_root = after_service.instance.variables.clone();
    projected_root.as_object_mut().unwrap().insert("fine".into(),
        fine_outputs["fine"].clone());
    assert!(serde_json::to_vec(&projected_root).unwrap().len()
        <= super::model::MAX_VARIABLE_BYTES,
        "the physical calibration must keep actual parent variables inside 256 KiB");
    assert!(serde_json::to_vec(&fine_outputs).unwrap().len()
        <= super::model::MAX_VARIABLE_BYTES,
        "the real Work output must remain within its published limit");
    let task = after_service.user_tasks.iter().find(|task|
        task.node_id == "FineTune" && task.status == ProcessUserTaskStatus::Open).unwrap();
    let fine_command = stamp("accept real fine-tuning Work source");
    let fine_ms = chrono::Utc::now().timestamp_millis();
    let fine_plan = planned_completion(&after_service, &task.user_task_id,
        &fine_command, &fine_outputs, fine_ms);
    assert!(fine_plan.repetition_capacity.is_none());
    repository::complete_user_task(&fixture.db, &fixture.owner, &fine_command,
        &started.instance_id, &task.user_task_id, after_service.instance.revision,
        &fine_outputs, None, repository::ProcessPlanInput::Supplied(&fine_plan),
        fine_ms).unwrap();
    let after_fine = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(after_fine.instance.variables["fine"], fine_outputs["fine"]);
    let after_fine_retained = measured_retained_bytes(&fixture, &started.instance_id);
    assert!(after_fine_retained <= RETAINED_LIMIT,
        "the actual target group must open below the physical limit");
    let target_group = after_fine.repetition_groups.iter().find(|group|
        group.node_id == "ParallelManual_1").unwrap();
    let target_open_bytes = physical_retained_bytes(&fixture, &target_group.group_id);
    assert!(target_open_bytes > 0);
    for prior in &after_service.repetition_groups {
        let persisted = after_fine.repetition_groups.iter().find(|group|
            group.group_id == prior.group_id).unwrap();
        assert_eq!(persisted.retained_bytes, prior.retained_bytes,
            "FineTune cannot change an already retained repetition group");
        assert_eq!(physical_retained_bytes(&fixture, &prior.group_id), prior.retained_bytes);
    }
    assert_eq!(after_fine_retained, retained_before_service + target_open_bytes,
        "the new parallel group alone must account for the physical retention increase");
    assert_eq!(target_group.created_count, 2);
    assert_eq!(target_group.mode,
        tentaflow_protocol::processes::ProcessRepetitionGroupMode::MultiInstanceParallel);
    let target_group_id = target_group.group_id.clone();
    let first = after_fine.repetition_occurrences.iter().find(|row|
        row.group_id == target_group_id && row.ordinal == 1).unwrap();
    let first_task_id = first.user_task_id.as_ref().unwrap();
    let first_command = stamp("accept higher parallel Manual ordinal first");
    let first_ms = chrono::Utc::now().timestamp_millis();
    let first_entry = super::manual_tests::manual_entry(&after_fine,
        first_task_id, &first_command);
    let first_plan = runtime::plan_manual_acknowledgment(&after_fine, first_task_id,
        &fixture.owner.user_id, first_ms, first_entry.clone(), None).unwrap();
    assert!(first_plan.repetition_capacity.is_none());
    repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
        &first_command, &started.instance_id, first_task_id,
        after_fine.instance.revision, repository::ProcessPlanInput::Supplied(&first_plan),
        first_ms).unwrap();
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let after_higher_retained = measured_retained_bytes(&fixture, &started.instance_id);
    assert!(after_higher_retained <= RETAINED_LIMIT,
        "the actual first accepted target ordinal must remain below the physical limit");
    assert!(physical_retained_bytes(&fixture, &target_group_id) > target_open_bytes,
        "the first accepted target ordinal must add factual retained evidence");
    let higher = snapshot.repetition_occurrences.iter().find(|row|
        row.group_id == target_group_id && row.ordinal == 1).unwrap();
    assert_eq!(higher.status, ProcessRepetitionOccurrenceStatus::Completed);
    let higher_source = higher.accepted_source_event_id.as_ref().unwrap().clone();
    let lower = snapshot.repetition_occurrences.iter().find(|row|
        row.group_id == target_group_id && row.ordinal == 0).unwrap();
    assert_eq!(lower.status, ProcessRepetitionOccurrenceStatus::Active);
    let lower_task_id = lower.user_task_id.as_ref().unwrap();
    let command = stamp("accept lower Manual last and deny parallel parent aggregate");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let canonical = runtime::plan_manual_acknowledgment(&snapshot, lower_task_id,
        &fixture.owner.user_id, at_ms,
        super::manual_tests::manual_entry(&snapshot, lower_task_id, &command),
        None).unwrap();
    let capacity = canonical.repetition_capacity.as_ref().expect("real parent aggregate denial");
    assert_eq!(capacity.reason, "repetition_bytes");
    assert_eq!(capacity.group_id, target_group_id);
    assert!(matches!(&capacity.accepted_input,
        repository::AcceptedInputRef::ManualAcknowledgment { task_id, .. }
            if task_id == lower_task_id));
    let lower_index = canonical.events.iter().position(|event|
        event.kind == "manual_task_acknowledged").unwrap();
    let lower_source = canonical.event_ids.get(&lower_index).unwrap();
    assert_ne!(lower_source, &higher_source);
    assert!(matches!(&capacity.denied_candidate,
        Some(RepetitionDeniedBytes::ParentAggregate {
            final_ordinal: Some(1), source_event_id: Some(source),
        }) if source == &higher_source));
    assert!(capacity.retained_before_closure_bytes <= RETAINED_LIMIT);
    assert!(capacity.denied_candidate_bytes.unwrap() > RETAINED_LIMIT);
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    assert_eq!(before.len(), super::call_tests::TRANSITION_TABLES.len() + 2);
    let mut wrong_ordinal = canonical.clone();
    wrong_ordinal.repetition_capacity.as_mut().unwrap().denied_candidate =
        Some(RepetitionDeniedBytes::ParentAggregate {
            final_ordinal: Some(0), source_event_id: Some(higher_source.clone()),
        });
    let mut wrong_highest_source = canonical.clone();
    wrong_highest_source.repetition_capacity.as_mut().unwrap().denied_candidate =
        Some(RepetitionDeniedBytes::ParentAggregate {
            final_ordinal: Some(1), source_event_id: Some(lower_source.clone()),
        });
    let mut wrong_accepted_input = canonical.clone();
    wrong_accepted_input.repetition_capacity.as_mut().unwrap().accepted_input = first_entry;
    let mut wrong_service_source = canonical.clone();
    wrong_service_source.repetition_capacity.as_mut().unwrap().accepted_input =
        repository::AcceptedInputRef::Service {
            job_id: claim.job.job_id.clone(), attempt: claim.job.attempt,
            fence: claim.job.fence, result_event_id: service_event_id.clone(),
        };
    for forged in [wrong_ordinal, wrong_highest_source, wrong_accepted_input,
        wrong_service_source] {
        assert!(repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
            &command, &started.instance_id, lower_task_id,
            snapshot.instance.revision, repository::ProcessPlanInput::Supplied(&forged),
            at_ms).is_err());
        assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
    }
    repository::acknowledge_manual_task(&fixture.db, &fixture.owner,
        &command, &started.instance_id, lower_task_id,
        snapshot.instance.revision, repository::ProcessPlanInput::Supplied(&canonical),
        at_ms).unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    let final_group = persisted.repetition_groups.iter().find(|group|
        group.group_id == target_group_id).unwrap();
    assert_eq!(persisted.instance.status, ProcessInstanceStatus::Cancelled);
    assert_eq!(final_group.status, ProcessRepetitionGroupStatus::Incident);
    assert!(final_group.terminal_capacity);
    assert_eq!(final_group.completed_count, 2);
    assert!(persisted.repetition_occurrences.iter().any(|row|
        row.group_id == target_group_id && row.ordinal == 0
            && row.status == ProcessRepetitionOccurrenceStatus::Completed
            && row.accepted_source_event_id.as_deref() == Some(lower_source.as_str())));
    assert!(persisted.repetition_occurrences.iter().any(|row|
        row.group_id == target_group_id && row.ordinal == 1
            && row.status == ProcessRepetitionOccurrenceStatus::Completed
            && row.accepted_source_event_id.as_deref() == Some(higher_source.as_str())));
    assert!(persisted.user_tasks.iter().any(|row|
        row.user_task_id == *lower_task_id && row.status == ProcessUserTaskStatus::Completed));
    assert_eq!(event_data(&fixture, &started.instance_id, lower_source).0,
        "manual_task_acknowledged");
    assert_eq!(event_data(&fixture, &started.instance_id, &higher_source).0,
        "manual_task_acknowledged");
    assert_eq!(event_data(&fixture, &started.instance_id, &service_event_id).0,
        "service_result");
    assert!(!persisted.tokens.iter().any(|token| token.node_id == "End_1"));
    let after = super::signal_proof_tests::all_transition_rows(&fixture);
    repository::acknowledge_manual_task(&reopened, &fixture.owner,
        &command, &started.instance_id, lower_task_id,
        snapshot.instance.revision, repository::ProcessPlanInput::Supplied(&canonical),
        at_ms).unwrap();
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), after);
}

#[test]
fn aggregate_limit_retains_two_accepted_sources_and_rejects_forged_finite_codes() {
    let fixture = Fixture::new();
    let started = start_model(&fixture,
        &sequential_human_model(&fixture.owner.user_id, 2));
    let evidence = "é".repeat(72_500);
    for ordinal in 0..2 {
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let (_, _, task_id) = open_work(&snapshot);
        let outputs = json!({"answer":ordinal,"evidence":evidence});
        assert!(serde_json::to_vec(&outputs).unwrap().len() < 256 * 1024);
        let command = stamp("accept genuine large Work output");
        let at_ms = chrono::Utc::now().timestamp_millis();
        let canonical = planned_completion(&snapshot, &task_id,
            &command, &outputs, at_ms);
        if ordinal == 1 {
            let before = super::call_tests::transition_rows(&fixture);
            for both_codes in [false, true] {
                let mut forged = canonical.clone();
                let blocked = forged.events.iter_mut().find(|event|
                    event.kind == "repetition_group_blocked").unwrap();
                assert_eq!(blocked.data["code"], "REPETITION_AGGREGATE_LIMIT");
                blocked.data["code"] = json!("REPETITION_MAPPING_FAILED");
                if both_codes {
                    forged.add_incidents.iter_mut().find(|incident|
                        incident.code == "REPETITION_AGGREGATE_LIMIT").unwrap().code =
                        "REPETITION_MAPPING_FAILED".into();
                }
                assert_rejected_unchanged(&fixture, &started.instance_id, &snapshot,
                    &task_id, &command, &outputs, &forged, at_ms, &before);
            }
        }
        repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &started.instance_id, &task_id, snapshot.instance.revision,
            &outputs, None, repository::ProcessPlanInput::Supplied(&canonical), at_ms).unwrap();
        if ordinal == 1 {
            let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
            let committed_rows = super::call_tests::transition_rows(&fixture);
            repository::complete_user_task(&reopened, &fixture.owner, &command,
                &started.instance_id, &task_id, snapshot.instance.revision,
                &outputs, None, repository::ProcessPlanInput::Supplied(&canonical), at_ms).unwrap();
            assert_eq!(super::call_tests::transition_rows(&fixture), committed_rows);
        }
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(persisted.instance.status, ProcessInstanceStatus::Incident);
    assert_eq!(persisted.repetition_groups[0].status, ProcessRepetitionGroupStatus::Incident);
    assert_eq!(persisted.repetition_groups[0].completed_count, 2);
    assert_eq!(persisted.repetition_occurrences.iter().filter(|row|
        row.status == ProcessRepetitionOccurrenceStatus::Completed).count(), 2);
    assert_eq!(persisted.instance.variables["results"], json!([]));
    assert!(!persisted.tokens.iter().any(|token| token.node_id == "End_1"));
    assert!(persisted.user_tasks.iter().all(|task|
        task.status != ProcessUserTaskStatus::Open));
    assert!(persisted.incidents.iter().any(|incident|
        incident.code == "REPETITION_AGGREGATE_LIMIT"));
    let (events, _, _) = repository::list_events(&reopened, &fixture.owner,
        &started.instance_id, 0, 100).unwrap();
    assert_eq!(events.iter().filter(|event| event.kind == "user_task_completed").count(), 2);
    assert_eq!(events.iter().filter(|event| event.kind == "repetition_group_blocked"
        && event.data["code"] == "REPETITION_AGGREGATE_LIMIT").count(), 1);
    measured_retained_bytes(&fixture, &started.instance_id);
}

#[tokio::test]
async fn capacity_latch_preserves_real_completed_service_evidence_and_cancels_open_verification() {
    for needs_human in [false, true] {
        let fixture = Fixture::new();
        let graph_json = if needs_human {
            let result = json!({"outcome":"NeedsHuman","code":"REVIEW",
                "summary":"Review the actual pinned result","outputs":{"answer":17},
                "evidence":["registered_flow_result"]});
            json!({"nodes":[{"id":"trigger","type":"trigger","config":{
                "output_mapping":{"actual_result":result.to_string()}}},
                {"id":"output","type":"output","config":{}}],
                "edges":[{"from":"trigger","to":"output",
                    "from_port":"text","to_port":"text"}],
                "variables":[{"name":"actual_result","type":"json"}]}).to_string()
        } else {
            graph("completed result requiring Human verification", None)
        };
        let flow_id = flow(&fixture.db, &fixture.owner, &graph_json);
        let model = verification_capacity_model(&fixture.owner.user_id, &flow_id, needs_human);
        let started = start_model(&fixture, &model);
        let worker = if needs_human { "needs-human-capacity-worker" }
            else { "completed-human-capacity-worker" };
        let claim = repository::claim_job(&fixture.db, worker,
            chrono::Utc::now().timestamp_millis()).unwrap().expect("real repeated Service claim");
        jobs::execute_claimed(&fixture.db, fixture.dispatcher(), worker, claim.clone(),
            CancellationToken::new()).await.unwrap();
        let waiting = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let service_occurrence = waiting.repetition_occurrences.iter().find(|row|
            row.job_id.as_deref() == Some(claim.job.job_id.as_str())).unwrap();
        let service_job = waiting.jobs.iter().find(|job|
            job.job_id == claim.job.job_id).unwrap();
        let observed_bytes: i64 = fixture.db.read().unwrap().query_row(
            "SELECT length(CAST(observed_result_json AS BLOB)) FROM bpmn_service_invocations WHERE job_id=?1 AND phase='accepted'",
            [&claim.job.job_id], |row| row.get(0)).unwrap();
        assert!(observed_bytes > 0);
        let service_group = waiting.repetition_groups.iter().find(|group|
            group.group_id == service_occurrence.group_id).unwrap();
        assert_eq!(service_group.retained_bytes,
            physical_retained_bytes(&fixture, &service_group.group_id));
        assert_eq!(service_job.status, "completed");
        assert_eq!(service_occurrence.status,
            ProcessRepetitionOccurrenceStatus::AwaitingVerification);
        assert_eq!(service_job.result.as_ref().unwrap().outcome,
            if needs_human { ActivityOutcome::NeedsHuman } else { ActivityOutcome::Completed });
        let verification = waiting.user_tasks.iter().find(|task|
            task.user_task_id == *service_occurrence.verification_user_task_id.as_ref().unwrap()
                && task.kind == ProcessUserTaskKind::Verification
                && task.status == ProcessUserTaskStatus::Open).unwrap();
        let verification_id = verification.user_task_id.clone();
        let verification_token_id = verification.token_id.as_ref().unwrap().clone();
        let service_event = repository::list_events(&fixture.db, &fixture.owner,
            &started.instance_id, 0, 100).unwrap().0.into_iter().find(|event|
            event.kind == "service_result" && event.node_id.as_deref() == Some("Service")).unwrap();
        let conn = fixture.db.read().unwrap();
        let (job_status_before, result_before, origin_before): (String, String, String) =
            conn.query_row("SELECT status,result_json,result_origin FROM bpmn_jobs WHERE job_id=?1 AND instance_id=?2",
                rusqlite::params![claim.job.job_id, started.instance_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
        drop(conn);
        assert_eq!(job_status_before, "completed");
        let gate = waiting.user_tasks.iter().find(|task|
            task.node_id == "GateHuman" && task.status == ProcessUserTaskStatus::Open).unwrap();
        let command = stamp("release 64 actual repeated Work ordinals");
        let at_ms = chrono::Utc::now().timestamp_millis();
        let outputs = json!({"gate":"approved"});
        let canonical = planned_completion(&waiting, &gate.user_task_id,
            &command, &outputs, at_ms);
        let capacity = canonical.repetition_capacity.as_ref().expect("real 65th active ordinal denial");
        assert_eq!(capacity.reason, "active_occurrences");
        assert!(matches!(&capacity.accepted_input,
            repository::AcceptedInputRef::Human { .. }));
        let before = super::call_tests::transition_rows(&fixture);
        let mut unrelated_verification = canonical.clone();
        unrelated_verification.complete_user_task_ids.push(verification_id.clone());
        assert_rejected_unchanged(&fixture, &started.instance_id, &waiting,
            &gate.user_task_id, &command, &outputs, &unrelated_verification, at_ms, &before);
        let mut foreign_service_source = canonical.clone();
        foreign_service_source.repetition_capacity.as_mut().unwrap().accepted_input =
            repository::AcceptedInputRef::Service {
                job_id: claim.job.job_id.clone(), attempt: claim.job.attempt,
                fence: claim.job.fence, result_event_id: service_event.event_id.clone(),
            };
        assert_rejected_unchanged(&fixture, &started.instance_id, &waiting,
            &gate.user_task_id, &command, &outputs, &foreign_service_source, at_ms, &before);
        repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &started.instance_id, &gate.user_task_id, waiting.instance.revision,
            &outputs, None, repository::ProcessPlanInput::Supplied(&canonical), at_ms).unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
            &started.instance_id).unwrap();
        let retained_observation: String = reopened.read().unwrap().query_row(
            "SELECT observed_result_json FROM bpmn_service_invocations WHERE job_id=?1 AND phase='accepted'",
            [&claim.job.job_id], |row| row.get(0)).unwrap();
        assert_eq!(i64::try_from(retained_observation.as_bytes().len()).unwrap(),
            observed_bytes);
        assert_eq!(persisted.repetition_groups.iter().find(|group|
            group.group_id == service_group.group_id).unwrap().retained_bytes,
            physical_retained_bytes(&fixture, &service_group.group_id));
        assert_eq!(persisted.instance.status, ProcessInstanceStatus::Cancelled);
        assert!(persisted.repetition_groups.iter().any(|group|
            group.group_id == capacity.group_id
                && group.status == ProcessRepetitionGroupStatus::Incident
                && group.terminal_capacity));
        assert!(persisted.user_tasks.iter().any(|task|
            task.user_task_id == verification_id
                && task.status == ProcessUserTaskStatus::Cancelled));
        let conn = reopened.read().unwrap();
        let verification_token_status: String = conn.query_row(
            "SELECT status FROM bpmn_tokens WHERE instance_id=?1 AND token_id=?2",
            rusqlite::params![started.instance_id, verification_token_id],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(verification_token_status, "cancelled");
        let (job_status_after, result_after, origin_after): (String, String, String) =
            conn.query_row("SELECT status,result_json,result_origin FROM bpmn_jobs WHERE job_id=?1 AND instance_id=?2",
                rusqlite::params![claim.job.job_id, started.instance_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
        drop(conn);
        assert_eq!((job_status_after, result_after, origin_after),
            (job_status_before, result_before, origin_before));
        assert_eq!(event_data(&fixture, &started.instance_id, &service_event.event_id),
            (service_event.kind, service_event.data));
        let after = super::call_tests::transition_rows(&fixture);
        repository::complete_user_task(&reopened, &fixture.owner, &command,
            &started.instance_id, &gate.user_task_id, waiting.instance.revision,
            &outputs, None, repository::ProcessPlanInput::Supplied(&canonical), at_ms).unwrap();
        assert_eq!(super::call_tests::transition_rows(&fixture), after);
        let approve = stamp("approval after verification cancellation must fail");
        assert!(repository::complete_user_task(&reopened, &fixture.owner, &approve,
            &started.instance_id, &verification_id, persisted.instance.revision,
            &Value::Null, Some(true), repository::ProcessPlanInput::Supplied(&canonical), at_ms).is_err());
        assert_eq!(super::call_tests::transition_rows(&fixture), after);
    }
}

fn repeated_service_observation_graph(large_answer: bool) -> String {
    let actual_result = if large_answer {
        r#"{"outcome":"Completed","code":null,"summary":"accepted","outputs":{"answer":payload.padding},"evidence":[]}"#
    } else {
        r#"{"outcome":"Completed","code":null,"summary":"late original","outputs":{"answer":17},"evidence":[]}"#
    };
    let noise = if large_answer {
        "[]"
    } else {
        "[payload.padding,payload.padding]"
    };
    assert!(actual_result.len() <= crate::flow_engine::expr::MAX_EXPR_CHARS);
    assert!(noise.len() <= crate::flow_engine::expr::MAX_EXPR_CHARS);
    json!({
        "nodes":[{"id":"trigger","type":"trigger","config":{"output_mapping":{
            "actual_result":actual_result,
            "noise":noise
        }}},{"id":"output","type":"output","config":{}}],
        "edges":[{"from":"trigger","to":"output","from_port":"text","to_port":"text"}],
        "variables":[{"name":"actual_result","type":"json"},
            {"name":"noise","type":"json"}]
    })
    .to_string()
}

#[tokio::test]
async fn repeated_service_large_observation_uses_real_pinned_flow_input() {
    for large_answer in [true, false] {
        let fixture = Fixture::new();
        let graph_json = repeated_service_observation_graph(large_answer);
        let flow_id = flow(&fixture.db, &fixture.owner, &graph_json);
        let model = large_loop_with_repeated_service(&fixture.owner.user_id, &flow_id);
        let started = start_model(&fixture, &model);
        let worker = "large-observation-fixture-worker";
        let claim =
            repository::claim_job(&fixture.db, worker, chrono::Utc::now().timestamp_millis())
                .unwrap()
                .expect("real Service claim");
        assert_eq!(claim.job.node_id, "RepeatedService");
        assert_eq!(
            claim.job.input["payload"]["padding"]
                .as_str()
                .unwrap()
                .as_bytes()
                .len(),
            80_000
        );
        let observed =
            super::repetition_service_tests::observed_flow_result(&fixture, &claim).await;
        let bytes = serde_json::to_vec(&observed).unwrap().len();
        assert!(
            bytes > 300_000
                && u64::try_from(bytes).unwrap() <= runtime::MAX_OBSERVED_SERVICE_RESULT_BYTES,
            "real pinned Flow observation has {bytes} bytes"
        );
        let executions: i64 = fixture
            .db
            .read()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM flow_executions WHERE request_id=?1",
                [&claim.request_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(executions, 1);
        let persisted =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        assert!(persisted
            .service_dispatches
            .iter()
            .any(|dispatch| dispatch.job_id == claim.job.job_id && dispatch.phase == "observed"));
        let at_ms = chrono::Utc::now().timestamp_millis();
        let plan = runtime::test_support::plan_recorded_result(
            &fixture, &claim, &observed, at_ms,
        )
        .unwrap();
        assert!(plan.repetition_capacity.is_none());
        repository::accept_job_result(
            &fixture.db,
            &fixture.owner,
            &claim.job.job_id,
            claim.job.attempt,
            claim.job.fence,
            worker,
            &observed,
            persisted.instance.revision,
            repository::ProcessPlanInput::Supplied(&plan),
            at_ms,
        )
        .unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let accepted = repository::runtime_snapshot(
            &reopened,
            &fixture.owner,
            &started.instance_id,
        )
        .unwrap();
        let group = accepted
            .repetition_groups
            .iter()
            .find(|group| group.node_id == "RepeatedService")
            .unwrap();
        assert_eq!(group.completed_count, 1);
        assert_eq!(group.status, ProcessRepetitionGroupStatus::Completed);
        assert_eq!(group.retained_bytes, physical_retained_bytes(&fixture, &group.group_id));
        assert!(accepted.repetition_occurrences.iter().any(|occurrence|
            occurrence.group_id == group.group_id
                && occurrence.ordinal == 0
                && occurrence.status == ProcessRepetitionOccurrenceStatus::Completed
                && occurrence.accepted_source_event_id.is_some()));
        let executions: i64 = reopened.read().unwrap().query_row(
            "SELECT COUNT(*) FROM flow_executions WHERE request_id=?1",
            [&claim.request_id],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(executions, 1);
    }
}

#[tokio::test]
async fn near_limit_repeated_service_retains_observation_through_accepted_capacity_latch() {
    let fixture = Fixture::new();
    let graph_json = repeated_service_observation_graph(true);
    let flow_id = flow(&fixture.db, &fixture.owner, &graph_json);
    let mut model = large_loop_with_repeated_service(&fixture.owner.user_id, &flow_id);
    model.variables.insert("service_items".into(), json!(["observed", "next"]));
    let started = start_model(&fixture, &model);
    let mut work_completions = 0;
    loop {
        let retained = measured_retained_bytes(&fixture, &started.instance_id);
        let slack = RETAINED_LIMIT.checked_sub(retained).unwrap();
        if slack <= 550_000 { break; }
        assert!(work_completions < 160, "real Work history did not approach the byte boundary");
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let (_, ordinal, task_id) = open_work(&snapshot);
        let command = stamp("retain real Work history before Service dispatch");
        let at_ms = chrono::Utc::now().timestamp_millis();
        let wanted = usize::try_from(slack.saturating_sub(500_000)).unwrap()
            .saturating_sub(150_000).min(240_000);
        let large = json!({"next_step":ordinal + 1,"evidence":"x".repeat(wanted)});
        let large_plan = planned_completion(&snapshot, &task_id, &command, &large, at_ms);
        let small = json!({"next_step":ordinal + 1});
        let small_plan = planned_completion(&snapshot, &task_id, &command, &small, at_ms);
        let (outputs, plan) = if large_plan.repetition_capacity.is_none() {
            (&large, &large_plan)
        } else {
            assert!(small_plan.repetition_capacity.is_none(),
                "Service reservation must not strand a factual small Work completion");
            (&small, &small_plan)
        };
        repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &started.instance_id, &task_id, snapshot.instance.revision,
            outputs, None, repository::ProcessPlanInput::Supplied(plan), at_ms).unwrap();
        work_completions += 1;
    }
    let retained_before = measured_retained_bytes(&fixture, &started.instance_id);
    assert!(RETAINED_LIMIT - retained_before <= 550_000);
    let worker = "near-limit-repeated-service-worker";
    let claim = repository::claim_job(&fixture.db, worker,
        chrono::Utc::now().timestamp_millis()).unwrap().expect("factual Service claim");
    assert_eq!(claim.job.node_id, "RepeatedService");
    let observed = super::repetition_service_tests::observed_flow_result(&fixture, &claim).await;
    let observed_bytes = u64::try_from(serde_json::to_vec(&observed).unwrap().len()).unwrap();
    assert!(observed_bytes > 300_000
        && observed_bytes <= runtime::MAX_OBSERVED_SERVICE_RESULT_BYTES);
    let after_observation = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let service_group = after_observation.repetition_groups.iter().find(|group|
        group.node_id == "RepeatedService").unwrap();
    assert_eq!(service_group.total_count, Some(2));
    assert_eq!(service_group.retained_bytes,
        physical_retained_bytes(&fixture, &service_group.group_id));
    let at_ms = chrono::Utc::now().timestamp_millis();
    let canonical = runtime::test_support::plan_recorded_result(&fixture, &claim,
        &observed, at_ms).unwrap();
    let capacity = canonical.repetition_capacity.as_ref()
        .expect("accepted result must close the measured near-limit repetition");
    assert_eq!(capacity.reason, "repetition_bytes");
    assert_eq!(capacity.group_id, service_group.group_id);
    let source_index = canonical.events.iter().position(|event|
        event.kind == "service_result" && event.node_id.as_deref() == Some("RepeatedService"))
        .expect("accepted Service result event");
    let source_id = canonical.event_ids.get(&source_index).unwrap();
    let occurrence = after_observation.repetition_occurrences.iter().find(|row|
        row.group_id == service_group.group_id && row.ordinal == 0).unwrap();
    if capacity.retained_before_closure_bytes > RETAINED_LIMIT {
        assert!(capacity.denied_candidate.is_none());
        assert!(capacity.denied_candidate_bytes.is_none());
    } else {
        assert!(matches!(&capacity.denied_candidate,
            Some(RepetitionDeniedBytes::AcceptedMapping {
                occurrence_id, accepted_source_event_id,
            }) if occurrence_id == &occurrence.occurrence_id
                && accepted_source_event_id == source_id));
        assert!(capacity.denied_candidate_bytes.unwrap() > RETAINED_LIMIT);
    }
    let before = super::call_tests::transition_rows(&fixture);
    let mut forged = canonical.clone();
    forged.repetition_capacity.as_mut().unwrap().retained_before_closure_bytes += 1;
    assert!(repository::accept_job_result(&fixture.db, &fixture.owner,
        &claim.job.job_id, claim.job.attempt, claim.job.fence, worker,
        &observed, after_observation.instance.revision,
        repository::ProcessPlanInput::Supplied(&forged), at_ms).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), before);
    repository::accept_job_result(&fixture.db, &fixture.owner,
        &claim.job.job_id, claim.job.attempt, claim.job.fence, worker,
        &observed, after_observation.instance.revision,
        repository::ProcessPlanInput::Supplied(&canonical), at_ms).unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    let group = persisted.repetition_groups.iter().find(|group|
        group.group_id == service_group.group_id).unwrap();
    assert_eq!(group.status, ProcessRepetitionGroupStatus::Incident);
    assert!(group.terminal_capacity);
    assert_eq!(group.completed_count, 0);
    assert_eq!(group.retained_bytes, physical_retained_bytes(&fixture, &group.group_id));
    let closure_bytes = u64::try_from(serde_json::to_vec(
        &canonical.events[capacity.event_index].data).unwrap().len()).unwrap()
        + u64::try_from(canonical.add_incidents.iter().find(|incident|
            incident.incident_id == capacity.incident_id).unwrap().message.len()).unwrap();
    assert_eq!(measured_retained_bytes(&fixture, &started.instance_id)
        .checked_sub(closure_bytes), Some(capacity.retained_before_closure_bytes));
    let accepted = persisted.repetition_occurrences.iter().find(|row|
        row.group_id == group.group_id && row.ordinal == 0).unwrap();
    assert_eq!(accepted.status, ProcessRepetitionOccurrenceStatus::AcceptedBlocked);
    assert_eq!(accepted.accepted_source_event_id.as_deref(), Some(source_id.as_str()));
    assert!(accepted.accepted_origin.is_some());
    assert!(accepted.aggregate_item.is_none());
    let (service_results, occurrence_completions): (i64, i64) = reopened.read().unwrap()
        .query_row("SELECT
            (SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1 AND kind='service_result'
                AND node_id='RepeatedService' AND event_id=?2),
            (SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1
                AND kind='repetition_occurrence_completed' AND node_id='RepeatedService')",
            rusqlite::params![started.instance_id, source_id],
            |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert_eq!(service_results, 1);
    assert_eq!(occurrence_completions, 0);
    let raw: String = reopened.read().unwrap().query_row(
        "SELECT observed_result_json FROM bpmn_service_invocations WHERE job_id=?1 AND phase='accepted'",
        [&claim.job.job_id], |row| row.get(0)).unwrap();
    assert_eq!(u64::try_from(raw.as_bytes().len()).unwrap(), observed_bytes);
    assert!(!persisted.repetition_occurrences.iter().any(|row|
        row.group_id == group.group_id && row.ordinal > 0));
    assert_eq!(persisted.repetition_occurrences.iter().filter(|row|
        row.group_id == group.group_id).count(), 1);
    let service_jobs: i64 = reopened.read().unwrap().query_row(
        "SELECT COUNT(*) FROM bpmn_jobs WHERE instance_id=?1 AND node_id='RepeatedService'",
        [&started.instance_id], |row| row.get(0)).unwrap();
    assert_eq!(service_jobs, 1);
    assert!(repository::claim_job(&reopened, "after-service-capacity-latch",
        chrono::Utc::now().timestamp_millis()).unwrap().is_none());
    let committed = super::call_tests::transition_rows(&fixture);
    repository::accept_job_result(&reopened, &fixture.owner,
        &claim.job.job_id, claim.job.attempt, claim.job.fence, worker,
        &observed, after_observation.instance.revision,
        repository::ProcessPlanInput::Supplied(&canonical), at_ms).unwrap();
    assert_eq!(super::call_tests::transition_rows(&fixture), committed);
    let executions: i64 = reopened.read().unwrap().query_row(
        "SELECT COUNT(*) FROM flow_executions WHERE flow_id=?1",
        [&flow_id], |row| row.get(0)).unwrap();
    assert_eq!(executions, 1);
}

#[tokio::test]
async fn near_limit_completed_service_denies_only_the_next_factual_ordinal() {
    let fixture = Fixture::new();
    let graph_json = repeated_service_observation_graph(true);
    let flow_id = flow(&fixture.db, &fixture.owner, &graph_json);
    let mut model = large_loop_with_repeated_service(&fixture.owner.user_id, &flow_id);
    model.variables.insert("service_items".into(), json!(["observed", "next"]));
    let started = start_model(&fixture, &model);
    let mut work_completions = 0;
    loop {
        let slack = RETAINED_LIMIT - measured_retained_bytes(&fixture, &started.instance_id);
        if slack <= 1_100_000 { break; }
        assert!(work_completions < 140, "real Work history did not approach Service capacity");
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let (_, ordinal, task_id) = open_work(&snapshot);
        let command = stamp("retain Work before completed Service capacity probe");
        let at_ms = chrono::Utc::now().timestamp_millis();
        let wanted = usize::try_from(slack.saturating_sub(1_050_000)).unwrap()
            .saturating_sub(150_000).min(240_000);
        let large = json!({"next_step":ordinal + 1,"evidence":"x".repeat(wanted)});
        let large_plan = planned_completion(&snapshot, &task_id, &command, &large, at_ms);
        let small = json!({"next_step":ordinal + 1});
        let small_plan = planned_completion(&snapshot, &task_id, &command, &small, at_ms);
        let (outputs, plan) = if large_plan.repetition_capacity.is_none() {
            (&large, &large_plan)
        } else {
            assert!(small_plan.repetition_capacity.is_none());
            (&small, &small_plan)
        };
        repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &started.instance_id, &task_id, snapshot.instance.revision,
            outputs, None, repository::ProcessPlanInput::Supplied(plan), at_ms).unwrap();
        work_completions += 1;
    }
    let worker = "completed-service-capacity-worker";
    let claim = repository::claim_job(&fixture.db, worker,
        chrono::Utc::now().timestamp_millis()).unwrap().expect("factual Service claim");
    assert_eq!(claim.job.node_id, "RepeatedService");
    let observed = super::repetition_service_tests::observed_flow_result(&fixture, &claim).await;
    let observed_bytes = u64::try_from(serde_json::to_vec(&observed).unwrap().len()).unwrap();
    assert!(observed_bytes > 300_000
        && observed_bytes <= runtime::MAX_OBSERVED_SERVICE_RESULT_BYTES);

    let mut canonical;
    let mut canonical_at_ms;
    loop {
        let at_ms = chrono::Utc::now().timestamp_millis();
        assert!(repository::renew_job_lease(&fixture.db, &claim.job.job_id,
            claim.job.attempt, claim.job.fence, worker, at_ms).unwrap(),
            "the original observed Service lease must remain fenced during calibration");
        canonical = runtime::test_support::plan_recorded_result(&fixture, &claim,
            &observed, at_ms).unwrap();
        canonical_at_ms = at_ms;
        if let Some(capacity) = &canonical.repetition_capacity {
            assert!(matches!(&capacity.denied_candidate,
                Some(RepetitionDeniedBytes::Occurrence { ordinal: 1 })),
                "real calibration passed the completed first ordinal's next-work window: {:?}",
                capacity.denied_candidate);
            break;
        }
        assert!(work_completions < 160,
            "factual Work completions did not reach the next-Service admission boundary");
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let (_, ordinal, task_id) = open_work(&snapshot);
        let command = stamp("measure completed Service next-ordinal admission");
        let outputs = json!({"next_step":ordinal + 1,"evidence":"x".repeat(4_096)});
        let work_plan = planned_completion(&snapshot, &task_id, &command, &outputs, at_ms);
        assert!(work_plan.repetition_capacity.is_none(),
            "Work completion must not own the Service capacity latch");
        repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &started.instance_id, &task_id, snapshot.instance.revision,
            &outputs, None, repository::ProcessPlanInput::Supplied(&work_plan), at_ms).unwrap();
        work_completions += 1;
    }
    let after_observation = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let service_group = after_observation.repetition_groups.iter().find(|group|
        group.node_id == "RepeatedService").unwrap();
    let capacity = canonical.repetition_capacity.as_ref().unwrap();
    assert_eq!(capacity.reason, "repetition_bytes");
    assert_eq!(capacity.group_id, service_group.group_id);
    assert!(capacity.retained_before_closure_bytes <= RETAINED_LIMIT);
    assert!(capacity.denied_candidate_bytes.unwrap() > RETAINED_LIMIT);
    assert_eq!(service_group.retained_bytes,
        physical_retained_bytes(&fixture, &service_group.group_id));
    let source_index = canonical.events.iter().position(|event|
        event.kind == "service_result" && event.node_id.as_deref() == Some("RepeatedService"))
        .unwrap();
    let source_id = canonical.event_ids.get(&source_index).unwrap();
    let before = super::call_tests::transition_rows(&fixture);
    let mut forged = canonical.clone();
    forged.repetition_capacity.as_mut().unwrap().denied_candidate =
        Some(RepetitionDeniedBytes::Occurrence { ordinal: 0 });
    assert!(repository::accept_job_result(&fixture.db, &fixture.owner,
        &claim.job.job_id, claim.job.attempt, claim.job.fence, worker,
        &observed, after_observation.instance.revision,
        repository::ProcessPlanInput::Supplied(&forged), canonical_at_ms).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), before);
    repository::accept_job_result(&fixture.db, &fixture.owner,
        &claim.job.job_id, claim.job.attempt, claim.job.fence, worker,
        &observed, after_observation.instance.revision,
        repository::ProcessPlanInput::Supplied(&canonical), canonical_at_ms).unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    let group = persisted.repetition_groups.iter().find(|group|
        group.group_id == service_group.group_id).unwrap();
    assert_eq!(group.status, ProcessRepetitionGroupStatus::Incident);
    assert!(group.terminal_capacity);
    assert_eq!(group.completed_count, 1);
    assert_eq!(group.retained_bytes, physical_retained_bytes(&fixture, &group.group_id));
    let closure_bytes = u64::try_from(serde_json::to_vec(
        &canonical.events[capacity.event_index].data).unwrap().len()).unwrap()
        + u64::try_from(canonical.add_incidents.iter().find(|incident|
            incident.incident_id == capacity.incident_id).unwrap().message.len()).unwrap();
    assert_eq!(measured_retained_bytes(&fixture, &started.instance_id)
        .checked_sub(closure_bytes), Some(capacity.retained_before_closure_bytes));
    let first = persisted.repetition_occurrences.iter().find(|row|
        row.group_id == group.group_id && row.ordinal == 0).unwrap();
    assert_eq!(first.status, ProcessRepetitionOccurrenceStatus::Completed);
    assert_eq!(first.accepted_source_event_id.as_deref(), Some(source_id.as_str()));
    assert!(!persisted.repetition_occurrences.iter().any(|row|
        row.group_id == group.group_id && row.ordinal > 0));
    let raw: String = reopened.read().unwrap().query_row(
        "SELECT observed_result_json FROM bpmn_service_invocations WHERE job_id=?1 AND phase='accepted'",
        [&claim.job.job_id], |row| row.get(0)).unwrap();
    assert_eq!(u64::try_from(raw.as_bytes().len()).unwrap(), observed_bytes);
    let jobs: i64 = reopened.read().unwrap().query_row(
        "SELECT COUNT(*) FROM bpmn_jobs WHERE instance_id=?1 AND node_id='RepeatedService'",
        [&started.instance_id], |row| row.get(0)).unwrap();
    assert_eq!(jobs, 1);
    assert!(repository::claim_job(&reopened, "after-next-Service-denial",
        chrono::Utc::now().timestamp_millis()).unwrap().is_none());
    let executions: i64 = reopened.read().unwrap().query_row(
        "SELECT COUNT(*) FROM flow_executions WHERE request_id=?1",
        [&claim.request_id], |row| row.get(0)).unwrap();
    assert_eq!(executions, 1);
    let committed = super::call_tests::transition_rows(&fixture);
    repository::accept_job_result(&reopened, &fixture.owner,
        &claim.job.job_id, claim.job.attempt, claim.job.fence, worker,
        &observed, after_observation.instance.revision,
        repository::ProcessPlanInput::Supplied(&canonical), canonical_at_ms).unwrap();
    assert_eq!(super::call_tests::transition_rows(&fixture), committed);
}

#[tokio::test]
async fn near_limit_cancelled_repeated_service_retains_late_original_observation() {
    let fixture = Fixture::new();
    let graph_json = repeated_service_observation_graph(false);
    let flow_id = flow(&fixture.db, &fixture.owner, &graph_json);
    let model = large_loop_with_repeated_service(&fixture.owner.user_id, &flow_id);
    let started = start_model(&fixture, &model);
    let mut work_completions = 0;
    loop {
        let retained = measured_retained_bytes(&fixture, &started.instance_id);
        let slack = RETAINED_LIMIT.checked_sub(retained).unwrap();
        if slack <= 550_000 { break; }
        assert!(work_completions < 160, "real Work history did not approach the byte boundary");
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
            &started.instance_id).unwrap();
        let (_, ordinal, task_id) = open_work(&snapshot);
        let command = stamp("retain Work before late original Service observation");
        let at_ms = chrono::Utc::now().timestamp_millis();
        let wanted = usize::try_from(slack.saturating_sub(500_000)).unwrap()
            .saturating_sub(150_000).min(240_000);
        let large = json!({"next_step":ordinal + 1,"evidence":"x".repeat(wanted)});
        let large_plan = planned_completion(&snapshot, &task_id, &command, &large, at_ms);
        let small = json!({"next_step":ordinal + 1});
        let small_plan = planned_completion(&snapshot, &task_id, &command, &small, at_ms);
        let (outputs, plan) = if large_plan.repetition_capacity.is_none() {
            (&large, &large_plan)
        } else {
            assert!(small_plan.repetition_capacity.is_none(),
                "the original observation reservation must preserve a small Work continuation");
            (&small, &small_plan)
        };
        repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &started.instance_id, &task_id, snapshot.instance.revision,
            outputs, None, repository::ProcessPlanInput::Supplied(plan), at_ms).unwrap();
        work_completions += 1;
    }
    let retained_before = measured_retained_bytes(&fixture, &started.instance_id);
    assert!(RETAINED_LIMIT - retained_before <= 550_000);
    let worker = "late-original-near-limit-worker";
    let claim = repository::claim_job(&fixture.db, worker,
        chrono::Utc::now().timestamp_millis()).unwrap().expect("factual Service claim");
    assert_eq!(claim.job.node_id, "RepeatedService");
    let original = super::repetition_service_tests::original_flow_result(&fixture, &claim).await;
    let original_bytes = u64::try_from(serde_json::to_vec(&original).unwrap().len()).unwrap();
    assert!(original_bytes > 300_000
        && original_bytes <= runtime::MAX_OBSERVED_SERVICE_RESULT_BYTES);
    let before_cancel = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    repository::cancel_instance(&fixture.db, &fixture.owner,
        &stamp("close committed near-limit Service activation"),
        &started.instance_id, before_cancel.instance.revision).unwrap();
    let cancelled = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(cancelled.instance.status, ProcessInstanceStatus::Cancelled);
    assert_eq!(repository::record_job_observation(&fixture.db, &claim, worker,
        &original, chrono::Utc::now().timestamp_millis()).unwrap(),
        repository::JobObservationState::Blocked);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(persisted.instance.status, ProcessInstanceStatus::Cancelled);
    let group = persisted.repetition_groups.iter().find(|group|
        group.node_id == "RepeatedService").unwrap();
    assert_eq!(group.retained_bytes, physical_retained_bytes(&fixture, &group.group_id));
    assert!(measured_retained_bytes(&fixture, &started.instance_id) <= RETAINED_LIMIT);
    let raw: String = reopened.read().unwrap().query_row(
        "SELECT observed_result_json FROM bpmn_service_invocations WHERE job_id=?1 AND phase='observed_blocked'",
        [&claim.job.job_id], |row| row.get(0)).unwrap();
    assert_eq!(raw.as_bytes(), serde_json::to_string(&original).unwrap().as_bytes());
    assert_eq!(repository::record_job_observation(&reopened, &claim, worker,
        &original, chrono::Utc::now().timestamp_millis()).unwrap(),
        repository::JobObservationState::Blocked);
    assert_eq!(group.retained_bytes, physical_retained_bytes(&fixture, &group.group_id));
    let service_results: i64 = reopened.read().unwrap().query_row(
        "SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1 AND kind='service_result' AND node_id='RepeatedService'",
        [&started.instance_id], |row| row.get(0)).unwrap();
    assert_eq!(service_results, 0);
    let executions: i64 = reopened.read().unwrap().query_row(
        "SELECT COUNT(*) FROM flow_executions WHERE flow_id=?1",
        [&flow_id], |row| row.get(0)).unwrap();
    assert_eq!(executions, 1);
}

#[test]
fn capacity_latch_cancels_an_unrelated_manual_wait_without_acknowledgment() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner,
        &graph("manual sibling capacity", None));
    let mut model = verification_capacity_model(&fixture.owner.user_id, &flow_id, false);
    model.nodes.push(ProcessNode { id: "Manual".into(), name: "Inspect external work".into(),
        kind: ProcessNodeKind::ManualTask { assignee_user_id: None,
            instructions: "Inspect the physical item before acknowledging it.".into() },
        repeat: None, activity_io: None,});
    model.sequence_flows.push(edge("OuterToManual", "OuterSplit", "Manual"));
    model.sequence_flows.push(edge("ManualToOuterJoin", "Manual", "OuterJoin"));
    let version = publish_model(&fixture, &model);
    let instance_id = Uuid::new_v4().to_string();
    let variables = serde_json::to_value(&version.model.variables).unwrap();
    let start_command = stamp("start manual sibling capacity");
    let start_ms = chrono::Utc::now().timestamp_millis();
    let start_plan = runtime::plan_start(&version.model, &version.model.process_id, crate::processes::runtime::test_support::ordinary_start_id(&version.model), &instance_id, &fixture.owner,
        &version.definition_id, version.version, variables.clone(), runtime::StartCause::Manual,
        start_ms, manual_input(&start_command), None).unwrap();
    let occurrence_event = start_plan.events.iter().find(|event|
        event.kind == "repetition_occurrence_started").unwrap();
    let occurrence_token_id = occurrence_event.data["token_id"].as_str().unwrap().to_owned();
    let before_start = super::call_tests::transition_rows(&fixture);
    let mut wrong_start_source = start_plan.clone();
    wrong_start_source.events.iter_mut().find(|event|
        event.kind == "repetition_occurrence_started").unwrap().data["token_id"] =
            json!(Uuid::new_v4().to_string());
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &start_command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&wrong_start_source), start_ms).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), before_start);
    let manual_wait_id = start_plan.create_user_tasks.iter().find(|task|
        task.kind == ProcessUserTaskKind::Manual).unwrap().token_id.as_ref().unwrap().clone();
    let mut wrong_parent = start_plan.clone();
    wrong_parent.token_sources.insert(occurrence_token_id.clone(), manual_wait_id);
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &start_command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&wrong_parent), start_ms).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), before_start);
    let mut wrong_ready = start_plan.clone();
    wrong_ready.create_tokens.iter_mut().find(|token|
        token.token_id == occurrence_token_id).unwrap().status = "waiting".into();
    assert!(repository::start_instance(&fixture.db, &fixture.owner, &start_command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&wrong_ready), start_ms).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), before_start);
    let started = repository::start_instance(&fixture.db, &fixture.owner, &start_command,
        &instance_id, &version.definition_id, version.version, &variables, None, None,
        repository::ProcessPlanInput::Supplied(&start_plan), start_ms).unwrap();
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let manual = snapshot.user_tasks.iter().find(|task|
        task.node_id == "Manual" && task.kind == ProcessUserTaskKind::Manual
            && task.status == ProcessUserTaskStatus::Open).unwrap();
    let manual_id = manual.user_task_id.clone();
    let manual_token_id = manual.token_id.as_ref().unwrap().clone();
    let manual_command = stamp("late capacity manual acknowledgment");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let manual_plan = runtime::plan_manual_acknowledgment(&snapshot, &manual_id,
        &fixture.owner.user_id, at_ms,
        super::manual_tests::manual_entry(&snapshot, &manual_id, &manual_command), None).unwrap();
    let gate = snapshot.user_tasks.iter().find(|task|
        task.node_id == "GateHuman" && task.status == ProcessUserTaskStatus::Open).unwrap();
    let gate_command = stamp("release capacity against manual sibling");
    let gate_outputs = json!({"gate":"approved"});
    let gate_plan = planned_completion(&snapshot, &gate.user_task_id,
        &gate_command, &gate_outputs, at_ms);
    let capacity = gate_plan.repetition_capacity.as_ref().expect("real 65th ordinal denial");
    assert_eq!(capacity.reason, "active_occurrences");
    let committed = repository::complete_user_task(&fixture.db, &fixture.owner,
        &gate_command, &started.instance_id, &gate.user_task_id, snapshot.instance.revision,
        &gate_outputs, None, repository::ProcessPlanInput::Supplied(&gate_plan), at_ms).unwrap();
    assert_eq!(committed.instance.status, ProcessInstanceStatus::Cancelled);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    assert!(persisted.repetition_groups.iter().any(|group|
        group.group_id == capacity.group_id
            && group.status == ProcessRepetitionGroupStatus::Incident
            && group.terminal_capacity));
    assert!(persisted.user_tasks.iter().any(|task|
        task.user_task_id == manual_id && task.status == ProcessUserTaskStatus::Cancelled));
    assert_eq!(reopened.read().unwrap().query_row(
        "SELECT status FROM bpmn_tokens WHERE instance_id=?1 AND token_id=?2",
        rusqlite::params![started.instance_id, manual_token_id],
        |row| row.get::<_,String>(0)).unwrap(), "cancelled");
    let conn = reopened.read().unwrap();
    let (opened, acknowledged): (i64,i64) = conn.query_row(
        "SELECT SUM(kind='manual_task_opened'),SUM(kind='manual_task_acknowledged') FROM bpmn_events WHERE instance_id=?1",
        [&started.instance_id], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
    assert_eq!((opened, acknowledged), (1,0));
    drop(conn);
    let before = super::call_tests::transition_rows(&fixture);
    assert!(repository::acknowledge_manual_task(&reopened, &fixture.owner,
        &manual_command, &started.instance_id, &manual_id, snapshot.instance.revision,
        repository::ProcessPlanInput::Supplied(&manual_plan), at_ms).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), before);
    repository::complete_user_task(&reopened, &fixture.owner,
        &gate_command, &started.instance_id, &gate.user_task_id, snapshot.instance.revision,
        &gate_outputs, None, repository::ProcessPlanInput::Supplied(&gate_plan), at_ms).unwrap();
    assert_eq!(super::call_tests::transition_rows(&fixture), before);
}

#[test]
fn active_occurrence_capacity_closes_repeated_manual_tasks_without_acknowledgment() {
    let fixture = Fixture::new();
    let flow_id = flow(&fixture.db, &fixture.owner, &graph("repeated manual capacity", None));
    let mut model = verification_capacity_model(&fixture.owner.user_id, &flow_id, false);
    for node in model.nodes.iter_mut().filter(|node| node.id.starts_with("HumanRepeat_")) {
        node.kind = ProcessNodeKind::ManualTask {
            assignee_user_id: Some(fixture.owner.user_id.clone()),
            instructions: "Inspect each physical item before acknowledging it.".into(),
        };
    }
    model.sequence_flows.retain(|flow|
        flow.id != "InnerBranch_0" && flow.id != "InnerReturn_0");
    model.sequence_flows.push(edge("OuterToManual_0", "OuterSplit", "HumanRepeat_0"));
    model.sequence_flows.push(edge("Manual_0ToOuter", "HumanRepeat_0", "OuterJoin"));
    let started = start_model(&fixture, &model);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner,
        &started.instance_id).unwrap();
    let manual_ids: BTreeSet<String> = snapshot.user_tasks.iter()
        .filter(|task| task.kind == ProcessUserTaskKind::Manual
            && task.node_id == "HumanRepeat_0"
            && task.status == ProcessUserTaskStatus::Open)
        .map(|task| task.user_task_id.clone()).collect();
    assert_eq!(manual_ids.len(), 16, "the direct branch must open 16 distinct Manual waits");
    assert_eq!(snapshot.user_tasks.iter().filter(|task|
        task.kind == ProcessUserTaskKind::Manual).count(), 16);
    let gate = snapshot.user_tasks.iter().find(|task|
        task.node_id == "GateHuman" && task.status == ProcessUserTaskStatus::Open).unwrap();
    let command = stamp("release actual repeated Manual capacity");
    let outputs = json!({"gate":"approved"});
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = planned_completion(&snapshot, &gate.user_task_id, &command, &outputs, at_ms);
    let capacity = plan.repetition_capacity.as_ref().expect("real 65th active ordinal denial");
    assert_eq!(capacity.reason, "active_occurrences");
    let before = super::signal_proof_tests::all_transition_rows(&fixture);
    let mut forged = plan.clone();
    forged.repetition_capacity.as_mut().unwrap().group_id = Uuid::new_v4().to_string();
    assert!(repository::complete_user_task(&fixture.db, &fixture.owner,
        &command, &started.instance_id, &gate.user_task_id, snapshot.instance.revision,
        &outputs, None, repository::ProcessPlanInput::Supplied(&forged), at_ms).is_err());
    assert_eq!(super::signal_proof_tests::all_transition_rows(&fixture), before);
    repository::complete_user_task(&fixture.db, &fixture.owner,
        &command, &started.instance_id, &gate.user_task_id, snapshot.instance.revision,
        &outputs, None, repository::ProcessPlanInput::Supplied(&plan), at_ms).unwrap();
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
        &started.instance_id).unwrap();
    assert_eq!(persisted.instance.status, ProcessInstanceStatus::Cancelled);
    assert!(persisted.repetition_groups.iter().any(|group|
        group.group_id == capacity.group_id && group.terminal_capacity));
    let persisted_manual: Vec<_> = persisted.user_tasks.iter().filter(|task|
        task.kind == ProcessUserTaskKind::Manual).collect();
    assert_eq!(persisted_manual.len(), manual_ids.len());
    assert_eq!(persisted_manual.iter().map(|task| task.user_task_id.clone())
        .collect::<BTreeSet<_>>(), manual_ids);
    assert!(persisted_manual.iter().all(|task|
        task.status == ProcessUserTaskStatus::Cancelled));
    let (opened, acknowledged): (i64, i64) = reopened.read().unwrap().query_row(
        "SELECT SUM(kind='manual_task_opened'),SUM(kind='manual_task_acknowledged') \
         FROM bpmn_events WHERE instance_id=?1",
        [&started.instance_id], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    assert!(opened > 0);
    assert_eq!(opened, 16);
    assert_eq!(acknowledged, 0);
}
