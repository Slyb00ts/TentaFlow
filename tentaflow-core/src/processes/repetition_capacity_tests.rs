// ============ File: repetition_capacity_tests.rs — file-backed repetition byte admission regressions ============

use std::collections::BTreeMap;

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
use super::runtime::{self, test_support::{edge, flow, graph, human_input, service_model,
    stamp, start_model, Fixture}};

const RETAINED_LIMIT: u64 = 64 * 1024 * 1024;

fn physical_retained_bytes(fixture: &Fixture, group_id: &str) -> u64 {
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
            +COALESCE((SELECT SUM(length(CAST(t.outputs_json AS BLOB))) FROM bpmn_user_tasks t
                WHERE t.instance_id=g.instance_id AND t.user_task_id IN
                (SELECT o.user_task_id FROM bpmn_repetition_occurrences o WHERE o.group_id=g.group_id
                 UNION SELECT o.verification_user_task_id FROM bpmn_repetition_occurrences o WHERE o.group_id=g.group_id)),0)
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
        kind: ProcessNodeKind::ParallelGateway, repeat: None });
    model.nodes.push(ProcessNode { id: "Join".into(), name: "Join".into(),
        kind: ProcessNodeKind::ParallelGateway, repeat: None });
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
            kind: ProcessNodeKind::ParallelGateway, repeat: None },
        ProcessNode { id: "OuterJoin".into(), name: "Join after verification".into(),
            kind: ProcessNodeKind::ParallelGateway, repeat: None },
        ProcessNode { id: "GateHuman".into(), name: "Release the work fanout".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: Some(owner.into()), output_mapping: BTreeMap::new(),
            }, repeat: None },
        ProcessNode { id: "InnerSplit".into(), name: "Start four real repetitions".into(),
            kind: ProcessNodeKind::ParallelGateway, repeat: None },
        ProcessNode { id: "InnerJoin".into(), name: "Join four repetitions".into(),
            kind: ProcessNodeKind::ParallelGateway, repeat: None },
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
        .filter(|row| row.status == ProcessRepetitionOccurrenceStatus::Active)
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
        human_input(snapshot, task_id, command)).unwrap()
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
        instance_id, task_id, snapshot.instance.revision, outputs, None, plan, at_ms).is_err());
    assert_eq!(super::call_tests::transition_rows(fixture), before);
}

fn assert_capacity_forgeries(fixture: &Fixture, instance_id: &str,
    snapshot: &repository::RuntimeSnapshot, task_id: &str,
    command: &repository::CommandStamp, outputs: &Value, canonical: &RuntimePlan, at_ms: i64) {
    let before = super::call_tests::transition_rows(fixture);
    assert_eq!(before.len(), 16);
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
                selected.0, None, selected.1, at_ms).unwrap();
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
                selected.0, None, selected.1, at_ms).unwrap();
            assert_eq!(super::call_tests::transition_rows(&fixture), after);
            assert!(repository::cancel_instance(&reopened, &fixture.owner,
                &stamp("cancel already latched repetition"), &started.instance_id,
                persisted.instance.revision).is_err());
            assert_eq!(super::call_tests::transition_rows(&fixture), after);
            return;
        }
        repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &started.instance_id, &task_id, snapshot.instance.revision,
            &small, None, &small_plan, at_ms).unwrap();
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
            assert_eq!(before.len(), 16);
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
            &outputs, None, &canonical, at_ms).unwrap();
        if ordinal == 1 {
            let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
            let committed_rows = super::call_tests::transition_rows(&fixture);
            repository::complete_user_task(&reopened, &fixture.owner, &command,
                &started.instance_id, &task_id, snapshot.instance.revision,
                &outputs, None, &canonical, at_ms).unwrap();
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
        assert_eq!(before.len(), 16);
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
            &outputs, None, &canonical, at_ms).unwrap();
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let persisted = repository::runtime_snapshot(&reopened, &fixture.owner,
            &started.instance_id).unwrap();
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
            &outputs, None, &canonical, at_ms).unwrap();
        assert_eq!(super::call_tests::transition_rows(&fixture), after);
        let approve = stamp("approval after verification cancellation must fail");
        assert!(repository::complete_user_task(&reopened, &fixture.owner, &approve,
            &started.instance_id, &verification_id, persisted.instance.revision,
            &Value::Null, Some(true), &canonical, at_ms).is_err());
        assert_eq!(super::call_tests::transition_rows(&fixture), after);
    }
}
