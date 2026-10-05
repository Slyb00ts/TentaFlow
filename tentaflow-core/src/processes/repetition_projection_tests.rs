// ============ File: repetition_projection_tests.rs — real repetition reader pages and native CBOR frame bounds ============

use std::collections::BTreeMap;

use serde_json::{json, Value};
use tentaflow_protocol::processes::{
    ProcessInstancePageRequest, ProcessMultiInstanceInput, ProcessMultiInstanceMode, ProcessNode,
    ProcessNodeKind, ProcessPageSpec, ProcessPayload, ProcessRepeatSpec,
    ProcessRepetitionValueKind, ProcessUserTaskStatus,
};
use tentaflow_protocol::{cbor, MessageBody};

use super::model::starter_model;
use super::repository;
use super::runtime::{
    self,
    test_support::{actor, edge, stamp, start_model, Fixture},
};

#[test]
fn real_twenty_group_and_occurrence_pages_preserve_selected_aggregate_within_native_frame_budget() {
    let fixture = Fixture::new();
    assert!(fixture.directory.path().join("processes.db").is_file());
    let mut model = starter_model();
    model
        .variables
        .insert("large_text".into(), json!("x".repeat(163_840)));
    model
        .variables
        .insert("root_fractions".into(), json!(vec![0.1_f64; 16_000]));
    model.nodes.insert(
        1,
        ProcessNode {
            id: "Split".into(),
            name: "Independent repeated activities".into(),
            kind: ProcessNodeKind::ParallelGateway,
            repeat: None,
        },
    );
    model.nodes.insert(
        2,
        ProcessNode {
            id: "Join".into(),
            name: "One factual parent join".into(),
            kind: ProcessNodeKind::ParallelGateway,
            repeat: None,
        },
    );
    model.sequence_flows = vec![
        edge("Entry", "Start_1", "Split"),
        edge("Exit", "Join", "End_1"),
    ];
    for index in 0..20 {
        let id = format!("Work_{index}");
        let output = format!("results_{index}");
        model.variables.insert(output.clone(), json!([]));
        model.nodes.push(ProcessNode {
            id: id.clone(),
            name: format!("Actual repeated work {index}"),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: Some(fixture.owner.user_id.clone()),
                output_mapping: BTreeMap::new(),
            },
            repeat: Some(if index == 0 {
                ProcessRepeatSpec::StructuredLoop {
                    condition: "true".into(),
                    test_before: false,
                    max_iterations: 20,
                    output_collection_variable: output,
                }
            } else {
                ProcessRepeatSpec::MultiInstance {
                    mode: ProcessMultiInstanceMode::Sequential,
                    input: ProcessMultiInstanceInput::Cardinality { count: 1 },
                    output_collection_variable: output,
                }
            }),
        });
        model
            .sequence_flows
            .push(edge(&format!("Branch_{index}"), "Split", &id));
        model
            .sequence_flows
            .push(edge(&format!("Return_{index}"), &id, "Join"));
    }
    let started = start_model(&fixture, &model);
    let accepted = json!({"Customer_ID": vec![0.1_f64; 60_000]});
    assert!(serde_json::to_vec(&accepted).unwrap().len() <= 256 * 1024);
    for ordinal in 0..19 {
        let snapshot =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        let task = snapshot
            .user_tasks
            .iter()
            .find(|row| row.node_id == "Work_0" && row.status == ProcessUserTaskStatus::Open)
            .unwrap();
        let outputs = if ordinal == 0 {
            accepted.clone()
        } else {
            Value::Null
        };
        let command = stamp("accept exact repeated projection source");
        let at_ms = chrono::Utc::now().timestamp_millis();
        let plan = runtime::plan_user_completion(
            &snapshot,
            &task.user_task_id,
            &outputs,
            None,
            at_ms,
            runtime::test_support::human_input(&snapshot, &task.user_task_id, &command),
        None)
        .unwrap();
        repository::complete_user_task(
            &fixture.db,
            &fixture.owner,
            &command,
            &started.instance_id,
            &task.user_task_id,
            snapshot.instance.revision,
            &outputs,
            None,
            repository::ProcessPlanInput::Supplied(&plan),
            at_ms,
        )
        .unwrap();
    }
    let mut pages = ProcessInstancePageRequest {
        user_tasks: None,
        incidents: None,
        timers: None,
        subscriptions: None,
        event_races: None,
        outgoing_messages: None,
        selected_user_task_id: None,
        selected_incident_id: None,
        scopes: None,
        calls: None,
        repetition_groups: Some(ProcessPageSpec {
            offset: 0,
            limit: 20,
        }),
        repetition_occurrences: Some(ProcessPageSpec {
            offset: 0,
            limit: 20,
        }),
        selected_repetition_group_id: None,
        selected_repetition_occurrence_id: None,
        selected_repetition_value: None,
    };
    let summary = repository::get_instance(
        &fixture.db,
        &fixture.owner,
        &started.instance_id,
        Some(&pages),
    )
    .unwrap();
    assert_eq!(summary.repetition_groups.len(), 20);
    assert_eq!(
        summary
            .pages
            .as_ref()
            .expect("explicit summary page metadata")
            .repetition_groups
            .total,
        20
    );
    assert!(summary.selected_repetition_occurrence.is_none());
    let group = summary
        .repetition_groups
        .iter()
        .find(|row| row.node_id == "Work_0")
        .unwrap();
    pages.selected_repetition_group_id = Some(group.group_id.clone());
    let paged = repository::get_instance(
        &fixture.db,
        &fixture.owner,
        &started.instance_id,
        Some(&pages),
    )
    .unwrap();
    assert_eq!(paged.repetition_occurrences.len(), 20);
    assert_eq!(
        paged
            .pages
            .as_ref()
            .expect("explicit occurrence page metadata")
            .repetition_occurrences
            .total,
        20
    );
    assert_eq!(
        paged
            .pages
            .as_ref()
            .expect("explicit occurrence page metadata")
            .repetition_occurrences
            .next_offset,
        None
    );
    assert!(
        !paged
            .pages
            .as_ref()
            .expect("explicit occurrence page metadata")
            .repetition_occurrences
            .has_more
    );
    assert_eq!(
        paged
            .repetition_occurrences
            .iter()
            .map(|row| row.ordinal)
            .collect::<Vec<_>>(),
        (0..20).collect::<Vec<_>>()
    );
    let first_id = paged.repetition_occurrences[0].occurrence_id.clone();
    pages.selected_repetition_occurrence_id = Some(first_id.clone());
    pages.selected_repetition_value = Some(ProcessRepetitionValueKind::Aggregate);
    let selected = repository::get_instance(
        &fixture.db,
        &fixture.owner,
        &started.instance_id,
        Some(&pages),
    )
    .unwrap();
    let detail = selected.selected_repetition_occurrence.as_ref().unwrap();
    assert_eq!(detail.summary.occurrence_id, first_id);
    assert!(detail.value_available);
    assert_eq!(detail.value, accepted);
    assert_eq!(selected.variables["results_0"], json!([]));
    let body = MessageBody::ProcessBody(ProcessPayload::InstanceGetResponse { instance: selected });
    let encoded = cbor::encode(&body).unwrap();
    assert!(
        encoded.len() >= 800 * 1024,
        "The factual CBOR fixture must exercise the upper frame range: {}",
        encoded.len()
    );
    assert!(
        encoded.len() <= 921_600,
        "The actual reader frame exceeded the frozen budget: {}",
        encoded.len()
    );
    let decoded: MessageBody = cbor::decode(&encoded).unwrap();
    assert_eq!(decoded, body);
    let outsider = actor(&fixture.db, "projection-outsider");
    let error =
        repository::get_instance(&fixture.db, &outsider, &started.instance_id, Some(&pages))
            .unwrap_err();
    assert!(!error.to_string().contains(&first_id));
    pages.selected_repetition_value = Some(ProcessRepetitionValueKind::Item);
    let item = repository::get_instance(
        &fixture.db,
        &fixture.owner,
        &started.instance_id,
        Some(&pages),
    )
    .unwrap();
    let detail = item.selected_repetition_occurrence.unwrap();
    assert!(detail.value_available);
    assert_eq!(detail.value, Value::Null);
    pages.selected_repetition_occurrence_id =
        Some(paged.repetition_occurrences[19].occurrence_id.clone());
    pages.selected_repetition_value = Some(ProcessRepetitionValueKind::Aggregate);
    let pending = repository::get_instance(
        &fixture.db,
        &fixture.owner,
        &started.instance_id,
        Some(&pages),
    )
    .unwrap();
    let unavailable = pending.selected_repetition_occurrence.unwrap();
    assert!(!unavailable.value_available);
    assert_eq!(unavailable.value, Value::Null);
    pages.selected_repetition_occurrence_id = None;
    assert!(repository::get_instance(
        &fixture.db,
        &fixture.owner,
        &started.instance_id,
        Some(&pages)
    )
    .is_err());
    pages.selected_repetition_value = None;
    pages.repetition_occurrences = Some(ProcessPageSpec {
        offset: 19,
        limit: 20,
    });
    let last_page = repository::get_instance(
        &fixture.db,
        &fixture.owner,
        &started.instance_id,
        Some(&pages),
    )
    .unwrap();
    assert_eq!(last_page.repetition_occurrences.len(), 1);
    assert_eq!(last_page.repetition_occurrences[0].ordinal, 19);
    assert_eq!(
        last_page
            .pages
            .as_ref()
            .expect("explicit final page metadata")
            .repetition_occurrences
            .offset,
        19
    );
    assert_eq!(
        last_page
            .pages
            .as_ref()
            .expect("explicit final page metadata")
            .repetition_occurrences
            .total,
        20
    );
    assert!(
        !last_page
            .pages
            .as_ref()
            .expect("explicit final page metadata")
            .repetition_occurrences
            .has_more
    );
}
