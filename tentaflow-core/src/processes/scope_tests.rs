// ============ File: scope_tests.rs — Persisted embedded process scope behavior ============

use std::collections::BTreeMap;

use serde_json::{json, Value};
use tentaflow_protocol::processes::{
    ProcessDiagram, ProcessErrorDeclaration, ProcessInstanceStatus, ProcessMessageDeclaration,
    ProcessMessageStatus,
    ProcessMessageTargetSpec, ProcessNode, ProcessNodeKind, ProcessSubProcess, ProcessTimerSpec,
    ProcessSubscriptionStatus, ProcessTimerStatus, ProcessUserTaskStatus,
};
use uuid::Uuid;

use super::messages::{self, test_support as message_support};
use super::model::starter_model;
use super::repository::{self, PlannedEvent, PlannedScope, ProcessToken, RuntimePlan, ScopeUpdate};
use super::runtime::{
    self,
    test_support::{edge, embedded_model, publish_model, stamp, start_model, Fixture},
    StartCause,
};

fn fast_model() -> tentaflow_protocol::processes::ProcessModel {
    let mut model = starter_model();
    let body = ProcessSubProcess {
        nodes: vec![
            ProcessNode {
                id: "LocalStart".into(),
                name: "Start locally".into(),
                kind: ProcessNodeKind::Start,
            },
            ProcessNode {
                id: "LocalEnd".into(),
                name: "End locally".into(),
                kind: ProcessNodeKind::End,
            },
        ],
        sequence_flows: vec![edge("LocalFlow", "LocalStart", "LocalEnd")],
        variables: BTreeMap::from([("local_ID".into(), json!("kept"))]),
        diagram: ProcessDiagram::default(),
    };
    model.nodes.insert(
        1,
        ProcessNode {
            id: "Scope".into(),
            name: "Fast embedded scope".into(),
            kind: ProcessNodeKind::SubProcess {
                body,
                input_mapping: BTreeMap::new(),
                output_mapping: BTreeMap::new(),
            },
        },
    );
    model.sequence_flows = vec![
        edge("IntoScope", "Start_1", "Scope"),
        edge("AfterScope", "Scope", "End_1"),
    ];
    model
}

fn terminating_scope_model(
    owner: &str,
    input: &str,
    fail_return: bool,
) -> tentaflow_protocol::processes::ProcessModel {
    let mut child = starter_model();
    child.variables.insert("child_result".into(), json!(41));
    child.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    let kind = match input {
        "human" => ProcessNodeKind::UserTask {
            assignee_user_id: Some(owner.into()),
            output_mapping: BTreeMap::new(),
        },
        "timer" => ProcessNodeKind::TimerCatch {
            timer: ProcessTimerSpec::Duration { seconds: 1 },
        },
        "message" => {
            child.messages.push(ProcessMessageDeclaration {
                message_id: "ReviewMessage".into(),
                name: "EvidenceReady".into(),
            });
            ProcessNodeKind::MessageCatch {
                message_ref: "ReviewMessage".into(),
                correlation_expression: "'case-1'".into(),
                output_mapping: BTreeMap::new(),
            }
        }
        _ => panic!("unsupported factual input"),
    };
    child.nodes.insert(1, ProcessNode {
        id: "ChildInput".into(),
        name: "Accept actual child input".into(),
        kind,
    });
    child.sequence_flows = vec![
        edge("ChildEntry", "Start_1", "ChildInput"),
        edge("ChildTerminate", "ChildInput", "End_1"),
    ];
    let mut model = embedded_model(child, "Scope");
    model.variables.insert("parent_marker".into(), json!("unchanged"));
    if input == "timer" {
        model.timer_timezone = Some("UTC".into());
    }
    let scope = model.nodes.iter_mut().find(|node| node.id == "Scope").unwrap();
    let ProcessNodeKind::SubProcess { output_mapping, .. } = &mut scope.kind else {
        panic!("actual subprocess");
    };
    output_mapping.insert(
        "mapped_result".into(),
        if fail_return { "outputs.missing.required" } else { "outputs.child_result" }.into(),
    );
    model
}

fn immediate_terminal_body(
    start_id: &str,
    end_id: &str,
    flow_id: &str,
    local_key: &str,
    local_value: i64,
) -> ProcessSubProcess {
    ProcessSubProcess {
        nodes: vec![
            ProcessNode { id: start_id.into(), name: "Enter child".into(), kind: ProcessNodeKind::Start },
            ProcessNode { id: end_id.into(), name: "Terminate child".into(), kind: ProcessNodeKind::TerminateEnd },
        ],
        sequence_flows: vec![edge(flow_id, start_id, end_id)],
        variables: BTreeMap::from([(local_key.into(), json!(local_value))]),
        diagram: ProcessDiagram::default(),
    }
}

fn nested_model(
    owner: &str,
    outer_large: &str,
    inner_large: &str,
) -> tentaflow_protocol::processes::ProcessModel {
    let mut model = starter_model();
    model.variables = BTreeMap::from([
        ("source_ID".into(), json!("origin")),
        ("shadow".into(), json!("root")),
    ]);
    let inner = ProcessSubProcess {
        nodes: vec![
            ProcessNode {
                id: "InnerStart".into(),
                name: "Start inner".into(),
                kind: ProcessNodeKind::Start,
            },
            ProcessNode {
                id: "InnerWork".into(),
                name: "Approve inner".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: Some(owner.into()),
                    output_mapping: BTreeMap::from([("answer".into(), "outputs.answer".into())]),
                },
            },
            ProcessNode {
                id: "InnerEnd".into(),
                name: "End inner".into(),
                kind: ProcessNodeKind::End,
            },
        ],
        sequence_flows: vec![
            edge("InnerFlow1", "InnerStart", "InnerWork"),
            edge("InnerFlow2", "InnerWork", "InnerEnd"),
        ],
        variables: BTreeMap::from([
            ("innerBig".into(), json!(inner_large)),
            ("inner_ID".into(), json!("original")),
        ]),
        diagram: ProcessDiagram::default(),
    };
    let outer = ProcessSubProcess {
        nodes: vec![
            ProcessNode {
                id: "OuterStart".into(),
                name: "Start outer".into(),
                kind: ProcessNodeKind::Start,
            },
            ProcessNode {
                id: "Inner".into(),
                name: "Inner scope".into(),
                kind: ProcessNodeKind::SubProcess {
                    body: inner,
                    input_mapping: BTreeMap::from([(
                        "inner_from_outer".into(),
                        "vars.outer_from_root".into(),
                    )]),
                    output_mapping: BTreeMap::from([(
                        "inner_answer".into(),
                        "outputs.answer".into(),
                    )]),
                },
            },
            ProcessNode {
                id: "OuterEnd".into(),
                name: "End outer".into(),
                kind: ProcessNodeKind::End,
            },
        ],
        sequence_flows: vec![
            edge("OuterFlow1", "OuterStart", "Inner"),
            edge("OuterFlow2", "Inner", "OuterEnd"),
        ],
        variables: BTreeMap::from([
            ("shadow".into(), json!("local")),
            ("outerBig".into(), json!(outer_large)),
        ]),
        diagram: ProcessDiagram::default(),
    };
    model.nodes.insert(
        1,
        ProcessNode {
            id: "Outer".into(),
            name: "Outer scope".into(),
            kind: ProcessNodeKind::SubProcess {
                body: outer,
                input_mapping: BTreeMap::from([(
                    "outer_from_root".into(),
                    "vars.source_ID".into(),
                )]),
                output_mapping: BTreeMap::from([(
                    "result_ID".into(),
                    "outputs.inner_answer".into(),
                )]),
            },
        },
    );
    model.sequence_flows = vec![
        edge("RootFlow1", "Start_1", "Outer"),
        edge("RootFlow2", "Outer", "End_1"),
    ];
    model
}

fn event_count(fixture: &Fixture, instance_id: &str) -> i64 {
    fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bpmn_events WHERE instance_id=?1",
            [instance_id],
            |row| row.get(0),
        )
        .unwrap()
}

fn command_count(fixture: &Fixture) -> i64 {
    fixture
        .db
        .read()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM bpmn_commands", [], |row| row.get(0))
        .unwrap()
}

fn outbox_model(
    receiver_id: &str,
    catch_instance_id: Option<&str>,
    pending_receiver_id: Option<&str>,
    owner: &str,
    interrupt: bool,
) -> tentaflow_protocol::processes::ProcessModel {
    let first_target = match catch_instance_id {
        Some(instance_id) => ProcessMessageTargetSpec::Catch {
            definition_id: receiver_id.into(),
            instance_id_expression: Some(serde_json::to_string(instance_id).unwrap()),
            subscription_id_expression: None,
        },
        None => ProcessMessageTargetSpec::Start {
            definition_id: receiver_id.into(),
        },
    };
    let mut inner = starter_model();
    inner.messages.push(ProcessMessageDeclaration {
        message_id: "EvidenceDeclaration".into(),
        name: "EvidenceReady".into(),
    });
    inner.nodes.insert(
        1,
        ProcessNode {
            id: "ThrowEvidence".into(),
            name: "Send evidence".into(),
            kind: ProcessNodeKind::MessageThrow {
                message_ref: "EvidenceDeclaration".into(),
                target: first_target,
                correlation_expression: "'case-1'".into(),
                payload_expression: "{'customer_ID': 23}".into(),
                ttl_seconds: 120,
            },
        },
    );
    inner.sequence_flows = vec![
        edge("InnerToThrow", "Start_1", "ThrowEvidence"),
        edge("InnerThrowEnd", "ThrowEvidence", "End_1"),
    ];
    if let Some(pending_receiver_id) = pending_receiver_id {
        inner.messages.push(ProcessMessageDeclaration {
            message_id: "PendingDeclaration".into(),
            name: "EvidencePending".into(),
        });
        inner.nodes.insert(
            2,
            ProcessNode {
                id: "ThrowPending".into(),
                name: "Queue later evidence".into(),
                kind: ProcessNodeKind::MessageThrow {
                    message_ref: "PendingDeclaration".into(),
                    target: ProcessMessageTargetSpec::Start {
                        definition_id: pending_receiver_id.into(),
                    },
                    correlation_expression: "'case-1'".into(),
                    payload_expression: "{'customer_ID': 24}".into(),
                    ttl_seconds: 120,
                },
            },
        );
        inner.sequence_flows = vec![
            edge("InnerToThrow", "Start_1", "ThrowEvidence"),
            edge("ToPending", "ThrowEvidence", "ThrowPending"),
            edge("PendingEnd", "ThrowPending", "End_1"),
        ];
    }
    let mut outer = embedded_model(inner, "Inner");
    outer.nodes.insert(
        2,
        ProcessNode {
            id: "OuterWork".into(),
            name: "Finish outer work".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: Some(owner.into()),
                output_mapping: BTreeMap::new(),
            },
        },
    );
    outer.sequence_flows = vec![
        edge("OuterToInner", "RootStart_Inner", "Inner"),
        edge("InnerToWork", "Inner", "OuterWork"),
        edge("WorkToEnd", "OuterWork", "RootEnd_Inner"),
    ];
    let mut model = embedded_model(outer, "Outer");
    if interrupt {
        model.messages.push(ProcessMessageDeclaration {
            message_id: "StopDeclaration".into(),
            name: "StopWork".into(),
        });
        model.nodes.push(ProcessNode {
            id: "StopOuter".into(),
            name: "Interrupt outer scope".into(),
            kind: ProcessNodeKind::BoundaryMessage {
                attached_to_id: "Outer".into(),
                cancel_activity: true,
                message_ref: "StopDeclaration".into(),
                correlation_expression: "'case-1'".into(),
                output_mapping: BTreeMap::new(),
            },
        });
        model
            .sequence_flows
            .push(edge("StopToEnd", "StopOuter", "RootEnd_Outer"));
    }
    model
}

#[test]
fn nested_fast_scope_persists_root_child_and_replays_one_start() {
    let fixture = Fixture::new();
    let model = fast_model();
    let version = publish_model(&fixture, &model);
    let first_id = Uuid::new_v4().to_string();
    let variables = serde_json::to_value(&model.variables).unwrap();
    let actor = &fixture.owner;
    let command = stamp("embedded-fast-start");
    let first_plan = runtime::plan_start(
        &version.model,
        &first_id,
        actor,
        &version.definition_id,
        version.version,
        variables.clone(),
        StartCause::Manual,
        1_000,
        runtime::test_support::manual_input(&command),
    )
    .unwrap();
    let completed = repository::start_instance(
        &fixture.db,
        actor,
        &command,
        &first_id,
        &version.definition_id,
        version.version,
        &variables,
        &first_plan,
        1_000,
    )
    .unwrap();
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_eq!(completed.scopes.len(), 2);
    assert_eq!(
        completed
            .scopes
            .iter()
            .find(|scope| scope.parent_scope_id.is_none())
            .unwrap()
            .scope_id,
        first_id
    );
    let child = completed
        .scopes
        .iter()
        .find(|scope| scope.parent_scope_id.as_deref() == Some(first_id.as_str()))
        .unwrap();
    assert_eq!(child.subprocess_node_id.as_deref(), Some("Scope"));
    assert_eq!(child.status, ProcessInstanceStatus::Completed);
    let persisted: (i64, i64) = fixture.db.read().unwrap().query_row(
        "SELECT COUNT(*),COUNT(CASE WHEN parent_scope_id IS NULL AND local_variables_json IS NULL THEN 1 END) FROM bpmn_scopes WHERE instance_id=?1",
        [&first_id], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(persisted, (2, 1));
    let events_before = event_count(&fixture, &first_id);
    assert!(
        events_before >= 4,
        "start, entry, child completion and instance completion are persisted"
    );

    let replacement_id = Uuid::new_v4().to_string();
    let replacement_plan = runtime::plan_start(
        &version.model,
        &replacement_id,
        actor,
        &version.definition_id,
        version.version,
        variables.clone(),
        StartCause::Manual,
        2_000,
        runtime::test_support::manual_input(&command),
    )
    .unwrap();
    let replay = repository::start_instance(
        &fixture.db,
        actor,
        &command,
        &replacement_id,
        &version.definition_id,
        version.version,
        &variables,
        &replacement_plan,
        2_000,
    )
    .unwrap();
    assert_eq!(replay.instance_id, first_id);
    assert_eq!(replay.scopes, completed.scopes);
    assert_eq!(event_count(&fixture, &first_id), events_before);
    assert!(repository::get_instance(&fixture.db, actor, &replacement_id, None).is_err());
}

#[test]
fn scoped_work_uses_lexical_inputs_outputs_and_authorized_scope_detail() {
    let fixture = Fixture::new();
    let model = nested_model(&fixture.owner.user_id, "", "");
    let waiting = start_model(&fixture, &model);
    assert_eq!(waiting.status, ProcessInstanceStatus::Waiting);
    assert_eq!(waiting.scopes.len(), 3);
    let root = waiting
        .scopes
        .iter()
        .find(|scope| scope.parent_scope_id.is_none())
        .unwrap();
    let outer = waiting
        .scopes
        .iter()
        .find(|scope| scope.subprocess_node_id.as_deref() == Some("Outer"))
        .unwrap();
    let inner = waiting
        .scopes
        .iter()
        .find(|scope| scope.subprocess_node_id.as_deref() == Some("Inner"))
        .unwrap();
    assert_eq!(root.scope_id, waiting.instance_id);
    assert_eq!(
        outer.parent_scope_id.as_deref(),
        Some(root.scope_id.as_str())
    );
    assert_eq!(
        inner.parent_scope_id.as_deref(),
        Some(outer.scope_id.as_str())
    );
    let (detail, local, active) = repository::get_scope(
        &fixture.db,
        &fixture.owner,
        &waiting.instance_id,
        &inner.scope_id,
    )
    .unwrap();
    assert_eq!(detail.subprocess_node_name.as_deref(), Some("Inner scope"));
    assert_eq!(local["inner_from_outer"], "origin");
    assert_eq!(active, vec!["InnerWork"]);
    assert!(repository::get_scope(
        &fixture.db,
        &fixture.participant,
        &waiting.instance_id,
        &inner.scope_id
    )
    .is_err());
    assert!(repository::get_scope(
        &fixture.db,
        &fixture.owner,
        &waiting.instance_id,
        "forged-scope"
    )
    .is_err());
    let other_instance = start_model(&fixture, &model);
    assert!(repository::get_scope(
        &fixture.db,
        &fixture.owner,
        &other_instance.instance_id,
        &inner.scope_id
    )
    .is_err());

    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id).unwrap();
    let task_id = snapshot
        .user_tasks
        .iter()
        .find(|task| task.scope_id == inner.scope_id)
        .unwrap()
        .user_task_id
        .clone();
    let outputs = json!({"answer":"approved"});
    let command = stamp("scoped-complete");
    let plan = runtime::plan_user_completion(&snapshot, &task_id, &outputs, None, 2_000,
        runtime::test_support::human_input(&snapshot, &task_id, &command)).unwrap();
    let completed = repository::complete_user_task(
        &fixture.db,
        &fixture.owner,
        &command,
        &waiting.instance_id,
        &task_id,
        waiting.revision,
        &outputs,
        None,
        &plan,
        2_000,
    )
    .unwrap()
    .instance;
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_eq!(completed.variables["result_ID"], "approved");
    assert_eq!(completed.scopes.len(), 3);
    assert!(completed
        .scopes
        .iter()
        .all(|scope| scope.status == ProcessInstanceStatus::Completed));
}

#[test]
fn forged_scope_parent_or_cross_body_node_cannot_publish_a_partial_transition() {
    let fixture = Fixture::new();
    let model = nested_model(&fixture.owner.user_id, "", "");
    let waiting = start_model(&fixture, &model);
    let outer = waiting
        .scopes
        .iter()
        .find(|scope| scope.subprocess_node_id.as_deref() == Some("Outer"))
        .unwrap();
    let events_before = event_count(&fixture, &waiting.instance_id);
    for (parent_scope_id, node_id) in [
        (Uuid::new_v4().to_string(), "Outer"),
        (outer.scope_id.clone(), "Outer"),
    ] {
        let scope_id = Uuid::new_v4().to_string();
        let parent_token_id = Uuid::new_v4().to_string();
        let mut forged = RuntimePlan::initial(waiting.variables.clone());
        forged.status = waiting.status.clone();
        forged.create_tokens.push(ProcessToken {
            token_id: parent_token_id.clone(),
            scope_id: parent_scope_id.clone(),
            node_id: node_id.into(),
            arrival_edge_id: None,
            fork_stack: Vec::new(),
            status: "waiting".into(),
        });
        forged.create_scopes.push(PlannedScope {
            scope_id: scope_id.clone(),
            parent_scope_id: parent_scope_id.clone(),
            parent_token_id: parent_token_id.clone(),
            subprocess_node_id: node_id.into(),
            variables: json!({}),
        });
        forged.events.push(PlannedEvent {
            scope_id: scope_id.clone(),
            kind: "scope_entered".into(),
            node_id: None,
            data: json!({"scope_id":scope_id,"parent_scope_id":parent_scope_id,
                "parent_token_id":parent_token_id,"subprocess_node_id":node_id}),
        });
        assert!(repository::apply_transition(
            &fixture.db,
            &fixture.owner,
            &waiting.instance_id,
            waiting.revision,
            &forged,
            2_000
        )
        .is_err());
        assert!(repository::get_scope(
            &fixture.db,
            &fixture.owner,
            &waiting.instance_id,
            &scope_id
        )
        .is_err());
        assert_eq!(
            repository::get_instance(&fixture.db, &fixture.owner, &waiting.instance_id, None)
                .unwrap()
                .revision,
            waiting.revision
        );
        assert_eq!(event_count(&fixture, &waiting.instance_id), events_before);
    }
}

#[test]
fn ancestor_variable_change_revalidates_all_active_descendants_atomically() {
    let fixture = Fixture::new();
    let model = nested_model(
        &fixture.owner.user_id,
        &"o".repeat(70_000),
        &"i".repeat(20_000),
    );
    let waiting = start_model(&fixture, &model);
    assert_eq!(waiting.status, ProcessInstanceStatus::Waiting);
    let outer = waiting
        .scopes
        .iter()
        .find(|scope| scope.subprocess_node_id.as_deref() == Some("Outer"))
        .unwrap()
        .clone();
    let inner = waiting
        .scopes
        .iter()
        .find(|scope| scope.subprocess_node_id.as_deref() == Some("Inner"))
        .unwrap()
        .clone();
    let events_before = event_count(&fixture, &waiting.instance_id);
    let commands_before = command_count(&fixture);
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id).unwrap();
    let task_before = snapshot
        .user_tasks
        .iter()
        .find(|task| task.scope_id == inner.scope_id)
        .unwrap()
        .clone();

    let mut invalid = RuntimePlan::initial(waiting.variables.clone());
    invalid.status = waiting.status.clone();
    invalid.variables["newLarge"] = Value::String("x".repeat(180_000));
    assert!(
        repository::apply_transition(
            &fixture.db,
            &fixture.owner,
            &waiting.instance_id,
            waiting.revision,
            &invalid,
            2_000
        )
        .is_err(),
        "unchanged grandchild effective variables exceed 256 KiB"
    );
    let after_rejection =
        repository::get_instance(&fixture.db, &fixture.owner, &waiting.instance_id, None).unwrap();
    assert_eq!(after_rejection.revision, waiting.revision);
    assert_eq!(after_rejection.variables, waiting.variables);
    assert_eq!(event_count(&fixture, &waiting.instance_id), events_before);
    assert_eq!(command_count(&fixture), commands_before);
    let snapshot_after_rejection =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id).unwrap();
    assert_eq!(
        snapshot_after_rejection.scope_variables,
        snapshot.scope_variables
    );
    assert_eq!(
        serde_json::to_value(&snapshot_after_rejection.tokens).unwrap(),
        serde_json::to_value(&snapshot.tokens).unwrap()
    );
    assert_eq!(
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
            .unwrap()
            .user_tasks
            .iter()
            .find(|task| task.scope_id == inner.scope_id)
            .unwrap()
            .status,
        task_before.status
    );

    let mut shadowed = RuntimePlan::initial(waiting.variables.clone());
    shadowed.status = waiting.status.clone();
    shadowed.variables["shadow"] = Value::String("s".repeat(180_000));
    let legal = repository::apply_transition(
        &fixture.db,
        &fixture.owner,
        &waiting.instance_id,
        waiting.revision,
        &shadowed,
        3_000,
    )
    .unwrap()
    .instance;
    assert_eq!(legal.variables["shadow"].as_str().unwrap().len(), 180_000);
    assert_eq!(legal.status, ProcessInstanceStatus::Waiting);
    let (outer_after, local, _) = repository::get_scope(
        &fixture.db,
        &fixture.owner,
        &waiting.instance_id,
        &outer.scope_id,
    )
    .unwrap();
    assert_eq!(local["shadow"], "local");

    let mut removed_shadow = RuntimePlan::initial(legal.variables.clone());
    removed_shadow.status = legal.status.clone();
    let mut exposed = local.as_object().unwrap().clone();
    exposed.remove("shadow");
    removed_shadow.scope_updates.push(ScopeUpdate {
        scope_id: outer.scope_id.clone(),
        expected_revision: outer_after.revision,
        status: outer_after.status.clone(),
        variables: Some(Value::Object(exposed)),
    });
    assert!(
        repository::apply_transition(
            &fixture.db,
            &fixture.owner,
            &waiting.instance_id,
            legal.revision,
            &removed_shadow,
            4_000
        )
        .is_err(),
        "removing a shadow exposes oversized ancestor value to unchanged descendants"
    );
    let unchanged =
        repository::get_instance(&fixture.db, &fixture.owner, &waiting.instance_id, None).unwrap();
    assert_eq!(unchanged.revision, legal.revision);
    assert_eq!(event_count(&fixture, &waiting.instance_id), events_before);
    assert_eq!(command_count(&fixture), commands_before);
    let (_, local_after, _) = repository::get_scope(
        &fixture.db,
        &fixture.owner,
        &waiting.instance_id,
        &outer.scope_id,
    )
    .unwrap();
    assert_eq!(local_after["shadow"], "local");
    assert_eq!(
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
            .unwrap()
            .user_tasks
            .iter()
            .find(|task| task.scope_id == inner.scope_id)
            .unwrap()
            .status,
        task_before.status
    );

    let (inner_after, inner_local, _) = repository::get_scope(
        &fixture.db,
        &fixture.owner,
        &waiting.instance_id,
        &inner.scope_id,
    )
    .unwrap();
    let mut legal_outer_local = local.as_object().unwrap().clone();
    legal_outer_local.remove("shadow");
    let mut legal_inner_local = inner_local.as_object().unwrap().clone();
    legal_inner_local.insert("shadow".into(), json!("nested shadow"));
    let mut simultaneous = RuntimePlan::initial(legal.variables.clone());
    simultaneous.status = legal.status.clone();
    simultaneous.scope_updates.extend([
        ScopeUpdate {
            scope_id: outer.scope_id.clone(),
            expected_revision: outer_after.revision,
            status: outer_after.status,
            variables: Some(Value::Object(legal_outer_local)),
        },
        ScopeUpdate {
            scope_id: inner.scope_id.clone(),
            expected_revision: inner_after.revision,
            status: inner_after.status,
            variables: Some(Value::Object(legal_inner_local)),
        },
    ]);
    let accepted = repository::apply_transition(
        &fixture.db,
        &fixture.owner,
        &waiting.instance_id,
        legal.revision,
        &simultaneous,
        5_000,
    )
    .unwrap()
    .instance;
    assert_eq!(accepted.revision, legal.revision + 1);
    assert_eq!(event_count(&fixture, &waiting.instance_id), events_before);
    assert_eq!(
        repository::get_scope(
            &fixture.db,
            &fixture.owner,
            &waiting.instance_id,
            &outer.scope_id
        )
        .unwrap()
        .1
        .get("shadow"),
        None
    );
    assert_eq!(
        repository::get_scope(
            &fixture.db,
            &fixture.owner,
            &waiting.instance_id,
            &inner.scope_id
        )
        .unwrap()
        .1["shadow"],
        "nested shadow"
    );

    let mut terminal_model = nested_model(&fixture.owner.user_id, "", &"i".repeat(20_000));
    let ProcessNodeKind::SubProcess {
        body: outer_body, ..
    } = &mut terminal_model.nodes[1].kind
    else {
        panic!("the fixture must retain its outer embedded body");
    };
    let ProcessNodeKind::SubProcess {
        body: inner_body,
        output_mapping: inner_output,
        ..
    } = &mut outer_body.nodes[1].kind
    else {
        panic!("the fixture must retain its inner embedded body");
    };
    inner_output.clear();
    inner_body.nodes.retain(|node| node.id != "InnerWork");
    inner_body.sequence_flows = vec![edge("InnerDirectEnd", "InnerStart", "InnerEnd")];
    outer_body.nodes.insert(
        2,
        ProcessNode {
            id: "OuterWait".into(),
            name: "Keep the ancestor active".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: Some(fixture.owner.user_id.clone()),
                output_mapping: BTreeMap::new(),
            },
        },
    );
    outer_body.sequence_flows = vec![
        edge("OuterToInner", "OuterStart", "Inner"),
        edge("InnerToWait", "Inner", "OuterWait"),
        edge("WaitToEnd", "OuterWait", "OuterEnd"),
    ];
    let terminal_waiting = start_model(&fixture, &terminal_model);
    let finished_inner = terminal_waiting
        .scopes
        .iter()
        .find(|scope| scope.subprocess_node_id.as_deref() == Some("Inner"))
        .unwrap();
    assert_eq!(finished_inner.status, ProcessInstanceStatus::Completed);
    let terminal_inner_id = finished_inner.scope_id.clone();
    let terminal_local = repository::get_scope(
        &fixture.db,
        &fixture.owner,
        &terminal_waiting.instance_id,
        &terminal_inner_id,
    )
    .unwrap()
    .1;
    let terminal_events = repository::list_events(
        &fixture.db,
        &fixture.owner,
        &terminal_waiting.instance_id,
        0,
        100,
    )
    .unwrap()
    .0;
    let mut root_change = RuntimePlan::initial(terminal_waiting.variables.clone());
    root_change.status = terminal_waiting.status.clone();
    root_change.variables["terminal_only_large"] = Value::String("x".repeat(250_000));
    let terminal_after = repository::apply_transition(
        &fixture.db,
        &fixture.owner,
        &terminal_waiting.instance_id,
        terminal_waiting.revision,
        &root_change,
        6_000,
    )
    .unwrap()
    .instance;
    assert_eq!(terminal_after.revision, terminal_waiting.revision + 1);
    assert_eq!(terminal_after.status, ProcessInstanceStatus::Waiting);
    assert_eq!(
        repository::get_scope(
            &fixture.db,
            &fixture.owner,
            &terminal_waiting.instance_id,
            &terminal_inner_id
        )
        .unwrap()
        .1,
        terminal_local
    );
    assert_eq!(
        terminal_after
            .scopes
            .iter()
            .find(|scope| scope.scope_id == terminal_inner_id)
            .unwrap()
            .status,
        ProcessInstanceStatus::Completed
    );
    assert_eq!(
        repository::list_events(
            &fixture.db,
            &fixture.owner,
            &terminal_waiting.instance_id,
            0,
            100
        )
        .unwrap()
        .0,
        terminal_events
    );
    assert!(terminal_after
        .user_tasks
        .iter()
        .any(|task| task.node_id == "OuterWait"
            && task.status == tentaflow_protocol::processes::ProcessUserTaskStatus::Open));
}

#[test]
fn scope_lifetime_limit_records_an_incident_and_retains_the_waiting_parent_activation() {
    let fixture = Fixture::new();
    let model = nested_model(&fixture.owner.user_id, "", "");
    let initial = start_model(&fixture, &model);
    assert_eq!(initial.scopes.len(), 3);
    let original =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &initial.instance_id).unwrap();
    let outer_scope_id = original
        .scopes
        .iter()
        .find(|scope| scope.subprocess_node_id.as_deref() == Some("Outer"))
        .unwrap()
        .scope_id
        .clone();
    let mut outer_variables = original.scope_variables[&outer_scope_id].clone();
    outer_variables["inner_answer"] = json!("completed");
    let mut expansion = RuntimePlan::initial(initial.variables.clone());
    expansion.status = initial.status.clone();
    for _ in 0..126 {
        let scope_id = Uuid::new_v4().to_string();
        let parent_token_id = Uuid::new_v4().to_string();
        let end_token_id = Uuid::new_v4().to_string();
        expansion.create_tokens.push(ProcessToken {
            token_id: parent_token_id.clone(),
            scope_id: initial.instance_id.clone(),
            node_id: "Outer".into(),
            arrival_edge_id: None,
            fork_stack: Vec::new(),
            status: "waiting".into(),
        });
        expansion.create_tokens.push(ProcessToken {
            token_id: end_token_id.clone(),
            scope_id: scope_id.clone(),
            node_id: "OuterEnd".into(),
            arrival_edge_id: Some("OuterFlow2".into()),
            fork_stack: Vec::new(),
            status: "ready".into(),
        });
        expansion.create_scopes.push(PlannedScope {
            scope_id: scope_id.clone(),
            parent_scope_id: initial.instance_id.clone(),
            parent_token_id: parent_token_id.clone(),
            subprocess_node_id: "Outer".into(),
            variables: outer_variables.clone(),
        });
        expansion.events.push(PlannedEvent {
            scope_id: scope_id.clone(),
            kind: "scope_entered".into(),
            node_id: None,
            data: json!({"scope_id":scope_id,"parent_scope_id":initial.instance_id.clone(),
                "parent_token_id":parent_token_id,"subprocess_node_id":"Outer"}),
        });
        expansion.events.push(PlannedEvent {
            scope_id: scope_id.clone(),
            kind: "end_reached".into(),
            node_id: Some("OuterEnd".into()),
            data: Value::Null,
        });
        expansion.events.push(PlannedEvent {
            scope_id: scope_id.clone(),
            kind: "scope_completed".into(),
            node_id: None,
            data: json!({"scope_id":scope_id,"parent_scope_id":initial.instance_id.clone(),
                "parent_token_id":parent_token_id,"subprocess_node_id":"Outer"}),
        });
        expansion.consume_token_ids.push(parent_token_id);
        expansion.consume_token_ids.push(end_token_id);
        expansion.scope_updates.push(ScopeUpdate {
            scope_id,
            expected_revision: 1,
            status: ProcessInstanceStatus::Completed,
            variables: None,
        });
    }
    expansion.create_tokens.push(ProcessToken {
        token_id: Uuid::new_v4().to_string(),
        scope_id: initial.instance_id.clone(),
        node_id: "Outer".into(),
        arrival_edge_id: None,
        fork_stack: Vec::new(),
        status: "ready".into(),
    });
    let expanded = repository::apply_transition(
        &fixture.db,
        &fixture.owner,
        &initial.instance_id,
        initial.revision,
        &expansion,
        2_000,
    )
    .unwrap()
    .instance;
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &initial.instance_id).unwrap();
    assert_eq!(snapshot.scopes.len(), 129);
    let plan = runtime::plan_advance(&snapshot, 3_000).unwrap();
    assert!(plan
        .add_incidents
        .iter()
        .any(|incident| incident.code == "SCOPE_LIMIT"));
    let failed = plan
        .events
        .iter()
        .find(|event| event.kind == "scope_entry_failed")
        .unwrap();
    let retained_token = failed.data["parent_token_id"].as_str().unwrap();
    assert!(plan
        .create_tokens
        .iter()
        .any(|token| token.token_id == retained_token
            && token.scope_id == initial.instance_id
            && token.node_id == "Outer"
            && token.status == "waiting"));
    let result = repository::apply_transition(
        &fixture.db,
        &fixture.owner,
        &initial.instance_id,
        expanded.revision,
        &plan,
        3_000,
    )
    .unwrap()
    .instance;
    assert!(result
        .incidents
        .iter()
        .any(|incident| incident.code == "SCOPE_LIMIT"));
    let persisted: (String, String) = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT scope_id,status FROM bpmn_tokens WHERE instance_id=?1 AND token_id=?2",
            rusqlite::params![initial.instance_id, retained_token],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(persisted, (initial.instance_id, "waiting".into()));
    assert!(event_count(&fixture, &result.instance_id) > 0);
}

#[test]
fn interrupted_ancestor_retracts_completed_descendant_outbox_without_erasing_facts() {
    let fixture = Fixture::new();
    let receiver = publish_model(&fixture, &message_support::receiving_model(true, false));
    let mut pending_receiver_model = message_support::receiving_model(true, false);
    pending_receiver_model.messages[0].name = "EvidencePending".into();
    let pending_receiver = publish_model(&fixture, &pending_receiver_model);
    let model = outbox_model(
        &receiver.definition_id,
        None,
        Some(&pending_receiver.definition_id),
        &fixture.owner.user_id,
        true,
    );
    let source_version = publish_model(&fixture, &model);
    let waiting = message_support::start_version(&fixture, &source_version);
    assert_eq!(waiting.status, ProcessInstanceStatus::Waiting);
    let inner = waiting
        .scopes
        .iter()
        .find(|scope| scope.subprocess_node_id.as_deref() == Some("Inner"))
        .unwrap();
    let inner_id = inner.scope_id.clone();
    assert_eq!(inner.status, ProcessInstanceStatus::Completed);
    let before_detail =
        repository::get_scope(&fixture.db, &fixture.owner, &waiting.instance_id, &inner_id)
            .unwrap();
    assert_eq!(waiting.outgoing_messages.len(), 2);
    let deliver_id = waiting
        .outgoing_messages
        .iter()
        .find(|message| message.message_name == "EvidenceReady")
        .unwrap()
        .message_id
        .clone();
    let pending = waiting
        .outgoing_messages
        .iter()
        .find(|message| message.message_name == "EvidencePending")
        .unwrap();
    assert_eq!(pending.status, ProcessMessageStatus::Pending);
    assert_eq!(pending.source_scope_id.as_deref(), Some(inner_id.as_str()));
    let pending_id = pending.message_id.clone();
    let first_at_ms = chrono::Utc::now().timestamp_millis();
    let deliver_candidate = repository::due_messages(&fixture.db, first_at_ms, 32)
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.key.message_id == deliver_id)
        .unwrap();
    let repository::MessageSelection::Ready(deliver_prepared) =
        repository::message_snapshot(&fixture.db, &deliver_candidate).unwrap()
    else {
        panic!("the completed descendant's first message must have a real matching start");
    };
    let deliver_plan = messages::plan_message_delivery(&deliver_prepared, first_at_ms).unwrap();
    let first_delivery =
        repository::deliver_message(&fixture.db, &deliver_prepared, &deliver_plan, first_at_ms)
            .unwrap()
            .unwrap();
    assert_eq!(
        first_delivery.message.status,
        ProcessMessageStatus::Delivered
    );
    let before_events =
        repository::list_events(&fixture.db, &fixture.owner, &waiting.instance_id, 0, 100)
            .unwrap()
            .0;

    let mut stop = message_support::envelope(
        message_support::catch_target(&source_version, Some(&waiting.instance_id), None),
        Value::Null,
    );
    stop.message_name = "StopWork".into();
    let stop_receipt = message_support::send(&fixture, &stop);
    let at_ms = chrono::Utc::now().timestamp_millis();
    let candidate = repository::due_messages(&fixture.db, at_ms, 32)
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.key.message_id == stop_receipt.message_id)
        .unwrap();
    let repository::MessageSelection::Ready(prepared) =
        repository::message_snapshot(&fixture.db, &candidate).unwrap()
    else {
        panic!("the actual boundary subscription must accept the stop message");
    };
    let plan = messages::plan_message_delivery(&prepared, at_ms).unwrap();
    let delivered = repository::deliver_message(&fixture.db, &prepared, &plan, at_ms)
        .unwrap()
        .unwrap();
    assert_eq!(delivered.message.status, ProcessMessageStatus::Delivered);

    let after =
        repository::get_instance(&fixture.db, &fixture.owner, &waiting.instance_id, None).unwrap();
    let inner_after = after
        .scopes
        .iter()
        .find(|scope| scope.scope_id == inner_id)
        .unwrap();
    assert_eq!(inner_after.status, ProcessInstanceStatus::Completed);
    assert_eq!(
        repository::get_scope(&fixture.db, &fixture.owner, &waiting.instance_id, &inner_id)
            .unwrap()
            .1,
        before_detail.1
    );
    let cancelled = repository::get_message(
        &fixture.db,
        &fixture.owner,
        &fixture.owner.user_id,
        &pending_id,
    )
    .unwrap();
    assert_eq!(cancelled.message.status, ProcessMessageStatus::Cancelled);
    assert_eq!(
        cancelled.message.last_reason.as_deref(),
        Some("scope_cancelled")
    );
    assert_eq!(
        cancelled.message.source_scope_id.as_deref(),
        Some(inner_id.as_str())
    );
    let after_events =
        repository::list_events(&fixture.db, &fixture.owner, &waiting.instance_id, 0, 100)
            .unwrap()
            .0;
    assert!(before_events.iter().all(|event| after_events
        .iter()
        .any(|kept| kept.seq == event.seq && kept.kind == event.kind && kept.data == event.data)));
    assert_eq!(
        after_events
            .iter()
            .filter(|event| event.kind == "message_queued")
            .count(),
        2
    );
    assert_eq!(
        repository::get_message(
            &fixture.db,
            &fixture.owner,
            &fixture.owner.user_id,
            &deliver_id
        )
        .unwrap()
        .message
        .status,
        ProcessMessageStatus::Delivered
    );
    assert_eq!(
        after_events
            .iter()
            .filter(|event| event.kind == "message_delivered" && event.scope_id == inner_id)
            .count(),
        1
    );
    assert_eq!(
        after_events
            .iter()
            .filter(|event| event.kind == "message_cancelled" && event.scope_id == inner_id)
            .count(),
        1
    );
    assert_eq!(
        after_events
            .iter()
            .filter(|event| event.kind == "scope_completed" && event.scope_id == inner_id)
            .count(),
        1
    );
}

#[test]
fn normal_scope_completion_keeps_pending_outbox_until_real_catch_delivery_once() {
    let fixture = Fixture::new();
    let receiver_version = publish_model(&fixture, &message_support::receiving_model(false, true));
    let receiver = message_support::start_version(&fixture, &receiver_version);
    let source_version = publish_model(
        &fixture,
        &outbox_model(
            &receiver_version.definition_id,
            Some(&receiver.instance_id),
            None,
            &fixture.owner.user_id,
            false,
        ),
    );
    let waiting = message_support::start_version(&fixture, &source_version);
    let inner_id = waiting
        .scopes
        .iter()
        .find(|scope| scope.subprocess_node_id.as_deref() == Some("Inner"))
        .unwrap()
        .scope_id
        .clone();
    let work_id = waiting
        .user_tasks
        .iter()
        .find(|task| task.node_id == "OuterWork")
        .unwrap()
        .user_task_id
        .clone();
    let message_id = waiting.outgoing_messages[0].message_id.clone();
    assert_eq!(
        waiting.outgoing_messages[0].status,
        ProcessMessageStatus::Pending
    );
    let completed = message_support::complete(&fixture, &waiting.instance_id, &work_id);
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert!(completed
        .scopes
        .iter()
        .all(|scope| scope.status == ProcessInstanceStatus::Completed));
    assert_eq!(
        repository::get_message(
            &fixture.db,
            &fixture.owner,
            &fixture.owner.user_id,
            &message_id
        )
        .unwrap()
        .message
        .status,
        ProcessMessageStatus::Pending
    );

    let gate_id = receiver
        .user_tasks
        .iter()
        .find(|task| task.node_id == "Gate_1")
        .unwrap()
        .user_task_id
        .clone();
    let armed = message_support::complete(&fixture, &receiver.instance_id, &gate_id);
    assert_eq!(
        armed
            .subscriptions
            .iter()
            .filter(|subscription| subscription.node_id == "Catch_1"
                && subscription.status
                    == tentaflow_protocol::processes::ProcessSubscriptionStatus::Open)
            .count(),
        1
    );
    let at_ms = chrono::Utc::now().timestamp_millis();
    let first = messages::drain_pending(&fixture.db, at_ms);
    first.completion.unwrap();
    assert_eq!(first.delivered, 1);
    let second = messages::drain_pending(&fixture.db, at_ms + 1);
    second.completion.unwrap();
    assert_eq!(second.delivered, 0);
    let receipt = repository::get_message(
        &fixture.db,
        &fixture.owner,
        &fixture.owner.user_id,
        &message_id,
    )
    .unwrap();
    assert_eq!(receipt.message.status, ProcessMessageStatus::Delivered);
    assert_eq!(
        receipt.message.source_scope_id.as_deref(),
        Some(inner_id.as_str())
    );
    assert_eq!(
        receipt.message.matched_instance_id.as_deref(),
        Some(receiver.instance_id.as_str())
    );
    let source_events =
        repository::list_events(&fixture.db, &fixture.owner, &waiting.instance_id, 0, 100)
            .unwrap()
            .0;
    assert_eq!(
        source_events
            .iter()
            .filter(|event| event.kind == "message_queued" && event.scope_id == inner_id)
            .count(),
        1
    );
    assert_eq!(
        source_events
            .iter()
            .filter(|event| event.kind == "message_delivered" && event.scope_id == inner_id)
            .count(),
        1
    );
    assert_eq!(
        source_events
            .iter()
            .filter(|event| event.kind == "scope_completed" && event.scope_id == inner_id)
            .count(),
        1
    );
}

#[test]
fn embedded_terminate_returns_mapped_locals_once_from_a_real_human_completion() {
    let fixture = Fixture::new();
    let model = terminating_scope_model(&fixture.owner.user_id, "human", false);
    let waiting = start_model(&fixture, &model);
    let before = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
        .unwrap();
    let child = before.scopes.iter().find(|scope| scope.subprocess_node_id.as_deref() == Some("Scope"))
        .unwrap();
    let child_id = child.scope_id.clone();
    let parent_wait = before.tokens.iter().find(|token| token.node_id == "Scope" && token.status == "waiting")
        .unwrap().clone();
    let task = before.user_tasks.iter().find(|task| task.node_id == "ChildInput").unwrap();
    let command = stamp("complete factual embedded terminal input");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let outputs = json!({"decision":"accepted"});
    let plan = runtime::plan_user_completion(&before, &task.user_task_id, &outputs, None, at_ms,
        runtime::test_support::human_input(&before, &task.user_task_id, &command)).unwrap();
    assert_eq!(plan.termination_attempts.len(), 1);
    assert!(matches!(&plan.termination_attempts[0], repository::TerminationAttempt::Success(_)));
    let completed = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &waiting.instance_id, &task.user_task_id, waiting.revision, &outputs, None, &plan, at_ms)
        .unwrap().instance;
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_eq!(completed.variables["mapped_result"], 41);
    assert_eq!(completed.variables["parent_marker"], "unchanged");
    let actual = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
        .unwrap();
    assert_eq!(actual.scopes.iter().find(|scope| scope.scope_id == child_id).unwrap().status,
        ProcessInstanceStatus::Completed);
    assert_eq!(repository::get_scope(&fixture.db, &fixture.owner, &waiting.instance_id, &child_id)
        .unwrap().1["child_result"], 41);
    let parent_status: String = fixture.db.read().unwrap().query_row(
        "SELECT status FROM bpmn_tokens WHERE instance_id=?1 AND scope_id=?2 AND token_id=?3",
        rusqlite::params![waiting.instance_id, parent_wait.scope_id, parent_wait.token_id],
        |row| row.get(0)).unwrap();
    assert_eq!(parent_status, "consumed");
    let events = repository::list_events(&fixture.db, &fixture.owner, &waiting.instance_id, 0, 200)
        .unwrap().0;
    let sources = events.iter().filter(|event| event.kind == "terminate_end_reached").collect::<Vec<_>>();
    assert_eq!(sources.len(), 1);
    let source = sources[0];
    assert_eq!(source.scope_id, child_id);
    assert_eq!(source.node_id.as_deref(), Some("End_1"));
    assert_eq!(source.data["source_event_id"].as_str(), Some(source.event_id.as_str()));
    assert_eq!(source.data["source_instance_id"].as_str(), Some(waiting.instance_id.as_str()));
    assert_eq!(events.iter().filter(|event| event.kind == "scope_completed"
        && event.data["reason"] == "terminate_end").count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "instance_completed").count(), 1);
    let rows = super::call_tests::transition_rows(&fixture);
    let replay = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &waiting.instance_id, &task.user_task_id, waiting.revision, &outputs, None, &plan, at_ms)
        .unwrap().instance;
    assert_eq!(replay, completed);
    assert_eq!(super::call_tests::transition_rows(&fixture), rows);
}

#[test]
fn embedded_human_error_end_catch_then_root_terminate_preserves_ordered_values_and_facts() {
    let fixture = Fixture::new();
    let mut model = terminating_scope_model(&fixture.owner.user_id, "human", false);
    model.errors.push(ProcessErrorDeclaration {
        error_id: "Rejected".into(),
        name: "Child business rejection".into(),
        error_code: "REJECTED".into(),
    });
    model.nodes.iter_mut().find(|node| node.id == "RootEnd_Scope").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    let scope = model.nodes.iter_mut().find(|node| node.id == "Scope").unwrap();
    let ProcessNodeKind::SubProcess { body, .. } = &mut scope.kind else {
        panic!("actual embedded body")
    };
    body.variables.insert("answer".into(), json!("pending"));
    let work = body.nodes.iter_mut().find(|node| node.id == "ChildInput").unwrap();
    let ProcessNodeKind::UserTask { output_mapping, .. } = &mut work.kind else {
        panic!("actual child human task")
    };
    output_mapping.insert("answer".into(), "outputs.answer".into());
    body.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::ErrorEnd { error_ref: "Rejected".into() };
    model.nodes.push(ProcessNode {
        id: "CatchChildError".into(),
        name: "Catch the child's factual error".into(),
        kind: ProcessNodeKind::BoundaryError {
            attached_to_id: "Scope".into(),
            error_ref: Some("Rejected".into()),
            output_mapping: BTreeMap::from([
                ("accepted_error_value".into(), "outputs.answer".into()),
                ("error_source".into(), "activity_result".into()),
            ]),
        },
    });
    model.sequence_flows.push(edge("CaughtToTerminate", "CatchChildError", "RootEnd_Scope"));

    let version = publish_model(&fixture, &model);
    let waiting = message_support::start_version(&fixture, &version);
    let before = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
        .unwrap();
    let child = before.scopes.iter().find(|scope| scope.subprocess_node_id.as_deref() == Some("Scope"))
        .unwrap();
    let child_id = child.scope_id.clone();
    let parent_wait = before.tokens.iter().find(|token| token.node_id == "Scope" && token.status == "waiting")
        .unwrap().clone();
    let task = before.user_tasks.iter().find(|task| task.node_id == "ChildInput").unwrap();
    let command = stamp("complete real human input into child ErrorEnd and root TerminateEnd");
    let outputs = json!({"answer":"approved after review"});
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_user_completion(&before, &task.user_task_id, &outputs, None, at_ms,
        runtime::test_support::human_input(&before, &task.user_task_id, &command)).unwrap();
    assert_eq!(plan.termination_attempts.len(), 1);
    assert!(matches!(&plan.termination_attempts[0], repository::TerminationAttempt::Success(_)));
    let before_rows = super::call_tests::transition_rows(&fixture);
    let mut missing_error = plan.clone();
    missing_error.events.iter_mut().find(|event| event.kind == "error_end_reached")
        .unwrap().kind = "node_completed".into();
    let mut foreign_error = plan.clone();
    foreign_error.events.iter_mut().find(|event| event.kind == "error_end_reached")
        .unwrap().data["source_event_id"] = json!(Uuid::new_v4().to_string());
    let child_token_id = task.token_id.as_ref().unwrap();
    assert_ne!(child_token_id, &parent_wait.token_id);
    let mut wrong_boundary_source = plan.clone();
    wrong_boundary_source.events.iter_mut().find(|event| event.kind == "business_error_caught")
        .unwrap().data["attached_token_id"] = json!(child_token_id);
    for (case, forged) in [
        ("missing factual child ErrorEnd", missing_error),
        ("foreign factual child ErrorEnd", foreign_error),
        ("wrong selected BoundaryError attachment", wrong_boundary_source),
    ] {
        assert!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &waiting.instance_id, &task.user_task_id, waiting.revision, &outputs, None,
            &forged, at_ms).is_err(), "{case} was accepted");
        assert_eq!(super::call_tests::transition_rows(&fixture), before_rows,
            "{case} changed persisted process rows");
    }
    let completed = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &waiting.instance_id, &task.user_task_id, waiting.revision, &outputs, None, &plan, at_ms)
        .unwrap().instance;
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_eq!(completed.variables["accepted_error_value"], "approved after review");
    assert_eq!(completed.variables["error_source"]["error_code"], "REJECTED");
    assert_eq!(completed.variables["parent_marker"], "unchanged");
    assert!(completed.variables.get("mapped_result").is_none());

    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let actual = repository::runtime_snapshot(&reopened, &fixture.owner, &waiting.instance_id)
        .unwrap();
    assert_eq!(actual.scopes.iter().find(|scope| scope.scope_id == child_id).unwrap().status,
        ProcessInstanceStatus::Error);
    assert_eq!(repository::get_scope(&reopened, &fixture.owner, &waiting.instance_id, &child_id)
        .unwrap().1["answer"], "approved after review");
    let parent_status: String = reopened.read().unwrap().query_row(
        "SELECT status FROM bpmn_tokens WHERE instance_id=?1 AND scope_id=?2 AND token_id=?3",
        rusqlite::params![waiting.instance_id, parent_wait.scope_id, parent_wait.token_id],
        |row| row.get(0)).unwrap();
    assert_eq!(parent_status, "cancelled");
    assert_eq!(actual.user_tasks.iter().find(|row| row.user_task_id == task.user_task_id).unwrap().status,
        ProcessUserTaskStatus::Completed);
    let events = repository::list_events(&reopened, &fixture.owner, &waiting.instance_id, 0, 200)
        .unwrap().0;
    let errors = events.iter().filter(|event| event.kind == "error_end_reached").collect::<Vec<_>>();
    let catches = events.iter().filter(|event| event.kind == "business_error_caught").collect::<Vec<_>>();
    let terminations = events.iter().filter(|event| event.kind == "terminate_end_reached").collect::<Vec<_>>();
    assert_eq!((errors.len(), catches.len(), terminations.len()), (1, 1, 1));
    assert_eq!(errors[0].scope_id, child_id);
    assert_eq!(errors[0].data["outputs"]["answer"], "approved after review");
    assert_eq!(catches[0].data["source_event_id"].as_str(), Some(errors[0].event_id.as_str()));
    assert_eq!(catches[0].data["source_kind"], "error_end");
    assert_eq!(terminations[0].scope_id, waiting.instance_id);
    assert_eq!(terminations[0].data["source_event_id"].as_str(), Some(terminations[0].event_id.as_str()));
    assert!(errors[0].seq < catches[0].seq && catches[0].seq < terminations[0].seq);
    assert_eq!(events.iter().filter(|event| event.kind == "instance_completed").count(), 1);
    let rows = super::call_tests::transition_rows(&fixture);
    let replay = repository::complete_user_task(&reopened, &fixture.owner, &command,
        &waiting.instance_id, &task.user_task_id, waiting.revision, &outputs, None, &plan, at_ms)
        .unwrap().instance;
    assert_eq!(replay, completed);
    assert_eq!(super::call_tests::transition_rows(&fixture), rows);
}

#[test]
fn embedded_terminate_keeps_a_foreign_sibling_incident_and_parent_join_activation() {
    let fixture = Fixture::new();
    let mut model = terminating_scope_model(&fixture.owner.user_id, "human", false);
    model.nodes.extend([
        ProcessNode { id: "RootSplit".into(), name: "Run both branches".into(),
            kind: ProcessNodeKind::ParallelGateway },
        ProcessNode { id: "PeerWork".into(), name: "Review sibling".into(),
            kind: ProcessNodeKind::UserTask { assignee_user_id: Some(fixture.owner.user_id.clone()),
                output_mapping: BTreeMap::new() } },
        ProcessNode { id: "PeerChoice".into(), name: "Evaluate sibling".into(),
            kind: ProcessNodeKind::ExclusiveGateway { default_flow_id: Some("PeerDefault".into()) } },
        ProcessNode { id: "PeerConditionalWork".into(), name: "Conditional sibling work".into(),
            kind: ProcessNodeKind::UserTask { assignee_user_id: Some(fixture.owner.user_id.clone()),
                output_mapping: BTreeMap::new() } },
        ProcessNode { id: "PeerDefaultWork".into(), name: "Default sibling work".into(),
            kind: ProcessNodeKind::UserTask { assignee_user_id: Some(fixture.owner.user_id.clone()),
                output_mapping: BTreeMap::new() } },
        ProcessNode { id: "PeerMerge".into(), name: "Merge sibling choice".into(),
            kind: ProcessNodeKind::ExclusiveGateway { default_flow_id: None } },
        ProcessNode { id: "RootJoin".into(), name: "Wait for both".into(),
            kind: ProcessNodeKind::ParallelGateway },
    ]);
    let mut invalid_condition = edge("PeerConditional", "PeerChoice", "PeerConditionalWork");
    invalid_condition.condition = Some("1".into());
    model.sequence_flows = vec![
        edge("RootEnterSplit", "RootStart_Scope", "RootSplit"),
        edge("SplitScope", "RootSplit", "Scope"),
        edge("SplitPeer", "RootSplit", "PeerWork"),
        edge("ScopeJoin", "Scope", "RootJoin"),
        edge("PeerChoiceEntry", "PeerWork", "PeerChoice"),
        invalid_condition,
        edge("PeerDefault", "PeerChoice", "PeerDefaultWork"),
        edge("PeerConditionalMerge", "PeerConditionalWork", "PeerMerge"),
        edge("PeerDefaultMerge", "PeerDefaultWork", "PeerMerge"),
        edge("PeerJoin", "PeerMerge", "RootJoin"),
        edge("AfterJoin", "RootJoin", "RootEnd_Scope"),
    ];
    let waiting = start_model(&fixture, &model);
    let first = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
        .unwrap();
    let peer_task = first.user_tasks.iter().find(|task| task.node_id == "PeerWork").unwrap();
    let peer_command = stamp("accept real sibling input with nonboolean choice");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let peer_plan = runtime::plan_user_completion(&first, &peer_task.user_task_id, &json!({}), None,
        at_ms, runtime::test_support::human_input(&first, &peer_task.user_task_id, &peer_command)).unwrap();
    let peer = repository::complete_user_task(&fixture.db, &fixture.owner, &peer_command,
        &waiting.instance_id, &peer_task.user_task_id, first.instance.revision, &json!({}), None,
        &peer_plan, at_ms).unwrap().instance;
    assert_eq!(peer.status, ProcessInstanceStatus::Incident);
    let peer_incident = peer.incidents.iter().find(|incident| incident.node_id.as_deref() == Some("PeerChoice"))
        .expect("the accepted sibling input produced its own gateway incident").clone();
    let second = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
        .unwrap();
    let child_id = second.scopes.iter().find(|scope| scope.subprocess_node_id.as_deref() == Some("Scope"))
        .unwrap().scope_id.clone();
    let child_task = second.user_tasks.iter().find(|task| task.node_id == "ChildInput").unwrap();
    let child_command = stamp("complete child without erasing sibling incident");
    let child_plan = runtime::plan_user_completion(&second, &child_task.user_task_id, &json!({}), None,
        at_ms + 1, runtime::test_support::human_input(&second, &child_task.user_task_id, &child_command)).unwrap();
    let continued = repository::complete_user_task(&fixture.db, &fixture.owner, &child_command,
        &waiting.instance_id, &child_task.user_task_id, second.instance.revision, &json!({}), None,
        &child_plan, at_ms + 1).unwrap().instance;
    assert_eq!(continued.status, ProcessInstanceStatus::Incident);
    assert!(continued.incidents.iter().any(|incident| incident == &peer_incident));
    assert_eq!(continued.variables["mapped_result"], 41);
    let actual = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
        .unwrap();
    assert_eq!(actual.scopes.iter().find(|scope| scope.scope_id == child_id).unwrap().status,
        ProcessInstanceStatus::Completed);
    assert_eq!(actual.receipts.iter().filter(|receipt| receipt.join_node_id == "RootJoin").count(), 1);
    assert!(actual.tokens.iter().any(|token| token.node_id == "RootJoin" && token.status == "joining"));
    assert_eq!(repository::list_events(&fixture.db, &fixture.owner, &waiting.instance_id, 0, 200)
        .unwrap().0.iter().filter(|event| event.kind == "terminate_end_reached"
            && event.scope_id == child_id).count(), 1);
}

#[test]
fn embedded_terminate_closes_a_real_open_fork_without_a_join_receipt() {
    let fixture = Fixture::new();
    let mut child = starter_model();
    child.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    child.nodes.extend([
        ProcessNode { id: "ChildSplit".into(), name: "Run both child branches".into(),
            kind: ProcessNodeKind::ParallelGateway },
        ProcessNode { id: "TerminatingWork".into(), name: "Complete and stop the scope".into(),
            kind: ProcessNodeKind::UserTask { assignee_user_id: Some(fixture.owner.user_id.clone()),
                output_mapping: BTreeMap::new() } },
        ProcessNode { id: "SiblingWork".into(), name: "Outstanding sibling work".into(),
            kind: ProcessNodeKind::UserTask { assignee_user_id: Some(fixture.owner.user_id.clone()),
                output_mapping: BTreeMap::new() } },
    ]);
    child.sequence_flows = vec![
        edge("ChildSplitEntry", "Start_1", "ChildSplit"),
        edge("ChildTerminateBranch", "ChildSplit", "TerminatingWork"),
        edge("ChildSiblingBranch", "ChildSplit", "SiblingWork"),
        edge("WorkTerminates", "TerminatingWork", "End_1"),
        edge("SiblingMayTerminate", "SiblingWork", "End_1"),
    ];
    let model = embedded_model(child, "Scope");
    let waiting = start_model(&fixture, &model);
    let before = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
        .unwrap();
    let child_id = before.scopes.iter().find(|scope| scope.subprocess_node_id.as_deref() == Some("Scope"))
        .unwrap().scope_id.clone();
    let terminating = before.user_tasks.iter().find(|task| task.node_id == "TerminatingWork").unwrap();
    let sibling = before.user_tasks.iter().find(|task| task.node_id == "SiblingWork").unwrap();
    let source = before.tokens.iter().find(|token| Some(token.token_id.as_str()) == terminating.token_id.as_deref()).unwrap();
    assert_eq!(source.fork_stack.len(), 1);
    assert_eq!(source.fork_stack[0].split_node_id, "ChildSplit");
    assert_eq!(source.fork_stack[0].join_node_id, None);
    assert_eq!(source.fork_stack[0].selected_branch_edge_ids,
        vec!["ChildSiblingBranch", "ChildTerminateBranch"]);
    assert!(before.receipts.is_empty());
    let command = stamp("complete one real branch and terminate its scope");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_user_completion(&before, &terminating.user_task_id, &json!({}), None,
        at_ms, runtime::test_support::human_input(&before, &terminating.user_task_id, &command)).unwrap();
    let completed = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &waiting.instance_id, &terminating.user_task_id, waiting.revision, &json!({}), None,
        &plan, at_ms).unwrap().instance;
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    let after = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
        .unwrap();
    assert_eq!(after.scopes.iter().find(|scope| scope.scope_id == child_id).unwrap().status,
        ProcessInstanceStatus::Completed);
    assert_eq!(after.user_tasks.iter().find(|task| task.user_task_id == terminating.user_task_id)
        .unwrap().status, ProcessUserTaskStatus::Completed);
    assert_eq!(after.user_tasks.iter().find(|task| task.user_task_id == sibling.user_task_id)
        .unwrap().status, ProcessUserTaskStatus::Cancelled);
    assert!(after.receipts.is_empty());
    assert_eq!(repository::list_events(&fixture.db, &fixture.owner, &waiting.instance_id, 0, 200)
        .unwrap().0.iter().filter(|event| event.kind == "terminate_end_reached"
            && event.scope_id == child_id).count(), 1);
}

#[test]
fn embedded_terminate_return_failure_parks_only_real_human_timer_or_message_input() {
    for input in ["human", "timer", "message"] {
        let fixture = Fixture::new();
        let model = runtime::test_support::with_boundaries(
            terminating_scope_model(&fixture.owner.user_id, input, true),
            "Scope", &[ ("ParentBoundary", true, 3600) ]);
        let version = publish_model(&fixture, &model);
        let waiting = message_support::start_version(&fixture, &version);
        let before = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
            .unwrap();
        let child_id = before.scopes.iter().find(|scope| scope.subprocess_node_id.as_deref() == Some("Scope"))
            .unwrap().scope_id.clone();
        let parent_wait = before.tokens.iter().find(|token| token.node_id == "Scope" && token.status == "waiting")
            .unwrap().clone();
        let child_wait = before.tokens.iter().find(|token| token.node_id == "ChildInput" && token.status == "waiting")
            .unwrap().clone();
        let boundary_id = before.timers.iter().find(|timer| timer.node_id == "ParentBoundary")
            .unwrap().timer_id.clone();
        match input {
            "human" => {
                let task = before.user_tasks.iter().find(|task| task.node_id == "ChildInput").unwrap();
                let command = stamp("accept actual human termination input");
                let at_ms = chrono::Utc::now().timestamp_millis();
                let outputs = json!({"accepted":"human"});
                let plan = runtime::plan_user_completion(&before, &task.user_task_id, &outputs, None, at_ms,
                    runtime::test_support::human_input(&before, &task.user_task_id, &command)).unwrap();
                assert!(matches!(&plan.termination_attempts[0], repository::TerminationAttempt::ReturnFailure(_)));
                repository::complete_user_task(&fixture.db, &fixture.owner, &command,
                    &waiting.instance_id, &task.user_task_id, waiting.revision, &outputs, None, &plan, at_ms).unwrap();
            }
            "timer" => {
                let due = before.timers.iter().find(|timer| timer.node_id == "ChildInput")
                    .unwrap().due_at_ms.unwrap();
                let fired = super::timers::drain_due(&fixture.db, due);
                fired.completion.unwrap();
                assert_eq!(fired.fired, 1);
            }
            "message" => {
                let subscription = before.subscriptions.iter().find(|subscription|
                    subscription.node_id == "ChildInput").unwrap();
                let envelope = message_support::envelope(
                    message_support::catch_target(&version, Some(&waiting.instance_id),
                        Some(&subscription.subscription_id)), json!({"accepted":"message"}));
                let receipt = message_support::send(&fixture, &envelope);
                let delivered = messages::drain_pending(&fixture.db, receipt.received_at_ms);
                delivered.completion.unwrap();
                let current = message_support::current(&fixture, &envelope);
                assert_eq!(delivered.delivered, 1,
                    "actual message status: {:?}, reason: {:?}",
                    current.message.status, current.message.last_reason);
                assert_eq!(current.message.status, ProcessMessageStatus::Delivered);
            }
            _ => unreachable!(),
        }
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let actual = repository::runtime_snapshot(&reopened, &fixture.owner, &waiting.instance_id)
            .unwrap();
        assert_eq!(actual.instance.status, ProcessInstanceStatus::Incident);
        assert_eq!(actual.instance.variables["parent_marker"], "unchanged");
        assert!(actual.instance.variables.get("mapped_result").is_none());
        assert_eq!(actual.scopes.iter().find(|scope| scope.scope_id == child_id).unwrap().status,
            ProcessInstanceStatus::Incident);
        assert!(actual.tokens.iter().any(|token| token.token_id == parent_wait.token_id
            && token.status == "waiting" && token.fork_stack == parent_wait.fork_stack));
        let parked = actual.tokens.iter().filter(|token| token.node_id == "End_1"
            && token.scope_id == child_id && token.status == "waiting").collect::<Vec<_>>();
        assert_eq!(parked.len(), 1);
        let consumed_terminal_count: i64 = reopened.read().unwrap().query_row(
            "SELECT COUNT(*) FROM bpmn_tokens WHERE instance_id=?1 AND scope_id=?2 AND node_id='End_1' AND status='consumed'",
            rusqlite::params![waiting.instance_id, child_id], |row| row.get(0)).unwrap();
        assert_eq!(consumed_terminal_count, 1);
        assert_eq!(parked[0].arrival_edge_id.as_deref(), Some("ChildTerminate"));
        assert_eq!(parked[0].fork_stack, child_wait.fork_stack);
        assert_eq!(actual.timers.iter().find(|timer| timer.timer_id == boundary_id).unwrap().status,
            ProcessTimerStatus::Pending);
        match input {
            "human" => assert_eq!(actual.user_tasks.iter().find(|task| task.node_id == "ChildInput")
                .unwrap().status, ProcessUserTaskStatus::Completed),
            "timer" => assert_eq!(actual.timers.iter().find(|timer| timer.node_id == "ChildInput")
                .unwrap().status, ProcessTimerStatus::Fired),
            "message" => assert_eq!(actual.subscriptions.iter().find(|subscription|
                subscription.node_id == "ChildInput").unwrap().status,
                ProcessSubscriptionStatus::Consumed),
            _ => unreachable!(),
        }
        let incident = actual.incidents.iter().find(|incident| incident.code == "SCOPE_RETURN_ERROR")
            .unwrap();
        assert!(incident.job_id.is_none() && !incident.can_retry);
        assert_eq!(incident.scope_id, child_id);
        let events = repository::list_events(&reopened, &fixture.owner, &waiting.instance_id, 0, 200)
            .unwrap().0;
        assert!(!events.iter().any(|event| event.kind == "terminate_end_reached"));
        let sources = events.iter().filter(|event| event.kind == "incident"
            && event.data["source_kind"] == "terminate_end_return_failure"
            && event.data["parent_token_id"] == parent_wait.token_id).collect::<Vec<_>>();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].data["source_event_id"].as_str(), Some(sources[0].event_id.as_str()));
        assert!(Uuid::parse_str(&sources[0].event_id).is_ok());
        assert!(!events.iter().any(|event| event.kind == "scope_completed" && event.scope_id == child_id));
        let rows = super::call_tests::transition_rows(&fixture);
        let later = repository::runtime_snapshot(&reopened, &fixture.owner, &waiting.instance_id)
            .unwrap();
        assert_eq!(later.incidents, actual.incidents);
        assert_eq!(super::call_tests::transition_rows(&fixture), rows);
    }
}

#[test]
fn embedded_termination_rejects_forged_source_prefix_and_closure_without_any_row_change() {
    let fixture = Fixture::new();
    let model = terminating_scope_model(&fixture.owner.user_id, "human", false);
    let waiting = start_model(&fixture, &model);
    let foreign_instance = start_model(&fixture, &model);
    assert_ne!(foreign_instance.instance_id, waiting.instance_id);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
        .unwrap();
    let task = snapshot.user_tasks.iter().find(|task| task.node_id == "ChildInput").unwrap();
    let command = stamp("genuine human input before forged termination plans");
    let outputs = json!({"decision":"accepted"});
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_user_completion(&snapshot, &task.user_task_id, &outputs, None, at_ms,
        runtime::test_support::human_input(&snapshot, &task.user_task_id, &command)).unwrap();
    let repository::TerminationAttempt::Success(source) = &plan.termination_attempts[0] else {
        panic!("actual human completion must produce a successful local terminal source");
    };
    let child_scope_id = source.source_scope_id.clone();
    let mut wrong_token = plan.clone();
    let repository::TerminationAttempt::Success(source) = &mut wrong_token.termination_attempts[0] else { unreachable!() };
    source.source_token_id = Uuid::new_v4().to_string();
    let mut wrong_event = plan.clone();
    let repository::TerminationAttempt::Success(source) = &mut wrong_event.termination_attempts[0] else { unreachable!() };
    source.source_event_id = Uuid::new_v4().to_string();
    let mut wrong_parent = plan.clone();
    let repository::TerminationAttempt::Success(source) = &mut wrong_parent.termination_attempts[0] else { unreachable!() };
    source.parent_token_id = Some(Uuid::new_v4().to_string());
    let mut wrong_arrival = plan.clone();
    let repository::TerminationAttempt::Success(source) = &mut wrong_arrival.termination_attempts[0] else { unreachable!() };
    source.source_arrival_edge_id = Some("an edge outside the child body".into());
    let mut wrong_input = plan.clone();
    let repository::TerminationAttempt::Success(source) = &mut wrong_input.termination_attempts[0] else { unreachable!() };
    let repository::AcceptedInputRef::Human { command_id, .. } = &mut source.accepted_input else { unreachable!() };
    *command_id = Uuid::new_v4().to_string();
    let mut wrong_variables = plan.clone();
    wrong_variables.variables["parent_marker"] = json!("forged root replacement");
    let mut extra_event = plan.clone();
    extra_event.events.push(PlannedEvent {
        scope_id: child_scope_id,
        kind: "terminate_end_reached".into(),
        node_id: Some("End_1".into()),
        data: json!({"source_event_id":Uuid::new_v4().to_string()}),
    });
    let mut missing_closure = plan.clone();
    missing_closure.cancel_scope_roots.clear();
    let before = super::call_tests::transition_rows(&fixture);
    for (case, forged) in [
        ("wrong factual source token", wrong_token),
        ("wrong source event identity", wrong_event),
        ("wrong enclosing wait", wrong_parent),
        ("wrong source predecessor", wrong_arrival),
        ("wrong accepted input", wrong_input),
        ("unmapped root replacement", wrong_variables),
        ("extra terminal effect", extra_event),
        ("missing closure", missing_closure),
    ] {
        let error = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &waiting.instance_id, &task.user_task_id, snapshot.instance.revision, &outputs, None,
            &forged, at_ms);
        assert!(error.is_err(), "{case} must be rejected before commit");
        assert_eq!(super::call_tests::transition_rows(&fixture), before, "{case} changed persisted rows");
    }
    let accepted = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &waiting.instance_id, &task.user_task_id, snapshot.instance.revision, &outputs, None,
        &plan, at_ms).unwrap();
    assert_eq!(accepted.instance.status, ProcessInstanceStatus::Completed);
    assert_eq!(accepted.instance.variables["mapped_result"], 41);
}

#[test]
fn embedded_termination_return_failure_rejects_false_mapping_proof_and_keeps_all_rows() {
    let fixture = Fixture::new();
    let model = terminating_scope_model(&fixture.owner.user_id, "human", true);
    let waiting = start_model(&fixture, &model);
    let foreign_instance = start_model(&fixture, &model);
    assert_ne!(foreign_instance.instance_id, waiting.instance_id);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
        .unwrap();
    let task = snapshot.user_tasks.iter().find(|task| task.node_id == "ChildInput").unwrap();
    let command = stamp("genuine human input before false return proof");
    let outputs = json!({"decision":"accepted"});
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_user_completion(&snapshot, &task.user_task_id, &outputs, None, at_ms,
        runtime::test_support::human_input(&snapshot, &task.user_task_id, &command)).unwrap();
    assert!(matches!(&plan.termination_attempts[0], repository::TerminationAttempt::ReturnFailure(_)));
    let mut wrong_pretext = plan.clone();
    let repository::TerminationAttempt::ReturnFailure(failure) = &mut wrong_pretext.termination_attempts[0] else { unreachable!() };
    failure.pre_return_child_locals["child_result"] = json!(99);
    let mut wrong_parent = plan.clone();
    let repository::TerminationAttempt::ReturnFailure(failure) = &mut wrong_parent.termination_attempts[0] else { unreachable!() };
    failure.pre_return_parent_effective["parent_marker"] = json!("forged");
    let mut wrong_wait = plan.clone();
    let repository::TerminationAttempt::ReturnFailure(failure) = &mut wrong_wait.termination_attempts[0] else { unreachable!() };
    failure.waiting_token_id = Uuid::new_v4().to_string();
    let before = super::call_tests::transition_rows(&fixture);
    for (case, forged) in [
        ("changed child prefix", wrong_pretext),
        ("changed parent effective variables", wrong_parent),
        ("changed parked arrival", wrong_wait),
    ] {
        assert!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &waiting.instance_id, &task.user_task_id, snapshot.instance.revision, &outputs, None,
            &forged, at_ms).is_err(), "{case} must fail closed");
        assert_eq!(super::call_tests::transition_rows(&fixture), before, "{case} changed persisted rows");
    }
    let accepted = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &waiting.instance_id, &task.user_task_id, snapshot.instance.revision, &outputs, None,
        &plan, at_ms).unwrap();
    assert_eq!(accepted.instance.status, ProcessInstanceStatus::Incident);
    assert!(accepted.instance.incidents.iter().any(|incident| incident.code == "SCOPE_RETURN_ERROR"
        && !incident.can_retry && incident.job_id.is_none()));
}

#[test]
fn two_disjoint_embedded_terminations_map_distinct_parent_keys_in_one_start_reduction() {
    let fixture = Fixture::new();
    let mut model = starter_model();
    model.variables.insert("parent_marker".into(), json!("kept"));
    model.nodes.extend([
        ProcessNode { id: "Split".into(), name: "Start two children".into(),
            kind: ProcessNodeKind::ParallelGateway },
        ProcessNode { id: "ScopeA".into(), name: "First child".into(),
            kind: ProcessNodeKind::SubProcess {
                body: immediate_terminal_body("StartA", "TerminateA", "FlowA", "local_a", 11),
                input_mapping: BTreeMap::new(),
                output_mapping: BTreeMap::from([("parent_a".into(), "outputs.local_a".into())]),
            } },
        ProcessNode { id: "ScopeB".into(), name: "Second child".into(),
            kind: ProcessNodeKind::SubProcess {
                body: immediate_terminal_body("StartB", "TerminateB", "FlowB", "local_b", 22),
                input_mapping: BTreeMap::new(),
                output_mapping: BTreeMap::from([("parent_b".into(), "outputs.local_b".into())]),
            } },
        ProcessNode { id: "Join".into(), name: "Join both children".into(),
            kind: ProcessNodeKind::ParallelGateway },
    ]);
    model.sequence_flows = vec![
        edge("StartSplit", "Start_1", "Split"),
        edge("SplitA", "Split", "ScopeA"),
        edge("SplitB", "Split", "ScopeB"),
        edge("AJoin", "ScopeA", "Join"),
        edge("BJoin", "ScopeB", "Join"),
        edge("JoinEnd", "Join", "End_1"),
    ];
    let version = publish_model(&fixture, &model);
    let instance_id = Uuid::new_v4().to_string();
    let variables = serde_json::to_value(&model.variables).unwrap();
    let command = stamp("start two factual immediate embedded terminal children");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&version.model, &instance_id, &fixture.owner,
        &version.definition_id, version.version, variables.clone(), StartCause::Manual,
        at_ms, runtime::test_support::manual_input(&command)).unwrap();
    let sources = plan.termination_attempts.iter().filter_map(|attempt| match attempt {
        repository::TerminationAttempt::Success(source) => Some(source),
        repository::TerminationAttempt::ReturnFailure(_) => None,
    }).collect::<Vec<_>>();
    assert_eq!(sources.len(), 2);
    assert_ne!(sources[0].source_scope_id, sources[1].source_scope_id);
    assert_ne!(sources[0].parent_token_id, sources[1].parent_token_id);
    assert_ne!(sources[0].source_event_id, sources[1].source_event_id);
    let completed = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, &plan, at_ms).unwrap();
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_eq!(completed.variables, json!({
        "parent_marker":"kept","parent_a":11,"parent_b":22,
    }));
    assert_eq!(completed.scopes.iter().filter(|scope| scope.parent_scope_id.as_deref()
        == Some(instance_id.as_str()) && scope.status == ProcessInstanceStatus::Completed).count(), 2);
    let actual = repository::runtime_snapshot(&fixture.db, &fixture.owner, &instance_id).unwrap();
    assert!(actual.receipts.is_empty());
    let events = repository::list_events(&fixture.db, &fixture.owner, &instance_id, 0, 200).unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "terminate_end_reached").count(), 2);
    assert_eq!(events.iter().filter(|event| event.kind == "scope_completed"
        && event.data["reason"] == "terminate_end").count(), 2);
    assert_eq!(events.iter().filter(|event| event.kind == "parallel_joined"
        && event.node_id.as_deref() == Some("Join")).count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "instance_completed").count(), 1);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::get_instance(&reopened, &fixture.owner, &instance_id, None).unwrap();
    assert_eq!(persisted.status, ProcessInstanceStatus::Completed);
    assert_eq!(persisted.variables, completed.variables);
    assert_eq!(persisted.scopes, completed.scopes);
    let rows = super::call_tests::transition_rows(&fixture);
    let replay = repository::start_instance(&reopened, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, &plan, at_ms).unwrap();
    assert_eq!(replay, completed);
    assert_eq!(super::call_tests::transition_rows(&fixture), rows);
}

#[test]
fn child_termination_then_root_termination_in_one_start_retains_both_factual_sources() {
    let fixture = Fixture::new();
    let mut model = starter_model();
    model.variables.insert("parent_marker".into(), json!("kept"));
    model.nodes.retain(|node| node.id != "End_1");
    model.nodes.extend([
        ProcessNode { id: "Scope".into(), name: "Child before root termination".into(),
            kind: ProcessNodeKind::SubProcess {
                body: immediate_terminal_body("ChildStart", "ChildTerminate", "ChildFlow", "child_value", 41),
                input_mapping: BTreeMap::new(),
                output_mapping: BTreeMap::from([("received".into(), "outputs.child_value".into())]),
            } },
        ProcessNode { id: "RootTerminate".into(), name: "Terminate parent after return".into(),
            kind: ProcessNodeKind::TerminateEnd },
    ]);
    model.sequence_flows = vec![
        edge("RootChild", "Start_1", "Scope"),
        edge("ChildRootTerminate", "Scope", "RootTerminate"),
    ];
    let version = publish_model(&fixture, &model);
    let instance_id = Uuid::new_v4().to_string();
    let variables = serde_json::to_value(&model.variables).unwrap();
    let command = stamp("start child then terminate its ancestor in one reduction");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&version.model, &instance_id, &fixture.owner,
        &version.definition_id, version.version, variables.clone(), StartCause::Manual,
        at_ms, runtime::test_support::manual_input(&command)).unwrap();
    let sources = plan.termination_attempts.iter().filter_map(|attempt| match attempt {
        repository::TerminationAttempt::Success(source) => Some(source),
        repository::TerminationAttempt::ReturnFailure(_) => None,
    }).collect::<Vec<_>>();
    assert_eq!(sources.len(), 2);
    assert_ne!(sources[0].source_scope_id, sources[1].source_scope_id);
    assert_eq!(sources[1].source_scope_id, instance_id);
    assert_ne!(sources[0].source_event_id, sources[1].source_event_id);
    let completed = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, &plan, at_ms).unwrap();
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_eq!(completed.variables, json!({"parent_marker":"kept","received":41}));
    let child = completed.scopes.iter().find(|scope| scope.subprocess_node_id.as_deref() == Some("Scope"))
        .unwrap();
    assert_eq!(child.status, ProcessInstanceStatus::Completed);
    assert_eq!(repository::get_scope(&fixture.db, &fixture.owner, &instance_id, &child.scope_id)
        .unwrap().1["child_value"], 41);
    let events = repository::list_events(&fixture.db, &fixture.owner, &instance_id, 0, 200).unwrap().0;
    let terminals = events.iter().filter(|event| event.kind == "terminate_end_reached").collect::<Vec<_>>();
    assert_eq!(terminals.len(), 2);
    assert_eq!(terminals[0].scope_id, child.scope_id);
    assert_eq!(terminals[1].scope_id, instance_id);
    assert_ne!(terminals[0].event_id, terminals[1].event_id);
    assert_eq!(events.iter().filter(|event| event.kind == "scope_completed"
        && event.scope_id == child.scope_id).count(), 1);
    let root_completion = events.iter().find(|event| event.kind == "instance_completed").unwrap();
    assert_eq!(root_completion.data["reason"], "terminate_end");
    assert_eq!(root_completion.data["source_event_id"].as_str(), Some(terminals[1].event_id.as_str()));
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let persisted = repository::get_instance(&reopened, &fixture.owner, &instance_id, None).unwrap();
    assert_eq!(persisted.status, ProcessInstanceStatus::Completed);
    assert_eq!(persisted.variables, completed.variables);
    assert_eq!(persisted.scopes, completed.scopes);
    assert_eq!(repository::list_events(&reopened, &fixture.owner, &instance_id, 0, 200).unwrap().0,
        events);
}

#[test]
fn embedded_termination_cannot_close_live_sibling_call_outbox_or_invent_sibling_completion() {
    let fixture = Fixture::new();
    let receiver = publish_model(&fixture, &message_support::receiving_model(true, false));
    let called = publish_model(&fixture, &runtime::test_support::user_model(Some(&fixture.owner.user_id)));
    let call_kind = super::call_tests::caller(&called, BTreeMap::new()).nodes.into_iter()
        .find(|node| node.id == "Call_1").unwrap().kind;
    let peer = ProcessSubProcess {
        nodes: vec![
            ProcessNode { id: "PeerStart".into(), name: "Enter peer".into(), kind: ProcessNodeKind::Start },
            ProcessNode { id: "PeerThrow".into(), name: "Queue peer evidence".into(),
                kind: ProcessNodeKind::MessageThrow {
                    message_ref: "EvidenceDeclaration".into(),
                    target: ProcessMessageTargetSpec::Start { definition_id: receiver.definition_id.clone() },
                    correlation_expression: "'case-1'".into(),
                    payload_expression: "{'customer_ID': 23}".into(), ttl_seconds: 120,
                } },
            ProcessNode { id: "PeerSplit".into(), name: "Keep two peer controls open".into(),
                kind: ProcessNodeKind::ParallelGateway },
            ProcessNode { id: "PeerCall".into(), name: "Call pinned child".into(), kind: call_kind },
            ProcessNode { id: "PeerHuman".into(), name: "Keep peer human open".into(),
                kind: ProcessNodeKind::UserTask { assignee_user_id: Some(fixture.owner.user_id.clone()),
                    output_mapping: BTreeMap::new() } },
            ProcessNode { id: "PeerJoin".into(), name: "Join peer controls".into(),
                kind: ProcessNodeKind::ParallelGateway },
            ProcessNode { id: "PeerEnd".into(), name: "Finish peer".into(), kind: ProcessNodeKind::End },
        ],
        sequence_flows: vec![
            edge("PeerEntry", "PeerStart", "PeerThrow"),
            edge("PeerToSplit", "PeerThrow", "PeerSplit"),
            edge("PeerToCall", "PeerSplit", "PeerCall"),
            edge("PeerToHuman", "PeerSplit", "PeerHuman"),
            edge("CallToPeerJoin", "PeerCall", "PeerJoin"),
            edge("HumanToPeerJoin", "PeerHuman", "PeerJoin"),
            edge("PeerJoinEnd", "PeerJoin", "PeerEnd"),
        ],
        variables: BTreeMap::new(), diagram: ProcessDiagram::default(),
    };
    let mut model = terminating_scope_model(&fixture.owner.user_id, "human", false);
    model.messages.push(ProcessMessageDeclaration {
        message_id: "EvidenceDeclaration".into(), name: "EvidenceReady".into(),
    });
    model.nodes.extend([
        ProcessNode { id: "RootSplit".into(), name: "Open both embedded branches".into(),
            kind: ProcessNodeKind::ParallelGateway },
        ProcessNode { id: "PeerScope".into(), name: "Retain actual peer work".into(),
            kind: ProcessNodeKind::SubProcess { body: peer,
                input_mapping: BTreeMap::new(), output_mapping: BTreeMap::new() } },
        ProcessNode { id: "RootJoin".into(), name: "Join both embedded branches".into(),
            kind: ProcessNodeKind::ParallelGateway },
    ]);
    model.sequence_flows = vec![
        edge("RootStartSplit", "RootStart_Scope", "RootSplit"),
        edge("RootToTerminating", "RootSplit", "Scope"),
        edge("RootToPeer", "RootSplit", "PeerScope"),
        edge("TerminatingToJoin", "Scope", "RootJoin"),
        edge("PeerToRootJoin", "PeerScope", "RootJoin"),
        edge("RootJoinEnd", "RootJoin", "RootEnd_Scope"),
    ];
    let version = publish_model(&fixture, &model);
    let waiting = message_support::start_version(&fixture, &version);
    let before = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
        .unwrap();
    assert_eq!(before.instance.status, ProcessInstanceStatus::Waiting);
    let terminating_scope = before.scopes.iter().find(|scope| scope.subprocess_node_id.as_deref() == Some("Scope"))
        .unwrap();
    let peer_scope = before.scopes.iter().find(|scope| scope.subprocess_node_id.as_deref() == Some("PeerScope"))
        .unwrap();
    let peer_call = before.calls.iter().find(|call| call.parent_scope_id == peer_scope.scope_id)
        .unwrap();
    assert_eq!(peer_call.status, tentaflow_protocol::processes::ProcessCallStatus::Waiting);
    let called_child_id = peer_call.child_instance_id.clone();
    assert_eq!(repository::get_instance(&fixture.db, &fixture.owner, &called_child_id, None)
        .unwrap().status, ProcessInstanceStatus::Waiting);
    let message = before.instance.outgoing_messages.iter().find(|message|
        message.source_scope_id.as_deref() == Some(peer_scope.scope_id.as_str())).unwrap();
    assert_eq!(message.status, ProcessMessageStatus::Pending);
    let message_id = message.message_id.clone();
    let peer_human = before.user_tasks.iter().find(|task| task.node_id == "PeerHuman").unwrap();
    assert_eq!(peer_human.status, ProcessUserTaskStatus::Open);
    let peer_tokens = before.tokens.iter().filter(|token| token.scope_id == peer_scope.scope_id
        && token.status == "waiting").collect::<Vec<_>>();
    assert_eq!(peer_tokens.len(), 2);
    let terminating_task = before.user_tasks.iter().find(|task| task.node_id == "ChildInput").unwrap();
    let command = stamp("complete only the factual terminating child human task");
    let outputs = json!({"decision":"accepted"});
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_user_completion(&before, &terminating_task.user_task_id, &outputs, None,
        at_ms, runtime::test_support::human_input(&before, &terminating_task.user_task_id, &command))
        .unwrap();
    assert_eq!(plan.cancel_scope_roots, vec![terminating_scope.scope_id.clone()]);
    let source = match &plan.termination_attempts[0] {
        repository::TerminationAttempt::Success(source) => source,
        repository::TerminationAttempt::ReturnFailure(_) => panic!("factual child return succeeded"),
    };
    let mut extra_closure = plan.clone();
    extra_closure.cancel_scope_roots.push(peer_scope.scope_id.clone());
    extra_closure.cancel_token_ids.extend(peer_tokens.iter().map(|token| token.token_id.clone()));
    extra_closure.cancel_user_task_ids.push(peer_human.user_task_id.clone());
    extra_closure.scope_updates.push(ScopeUpdate { scope_id: peer_scope.scope_id.clone(),
        expected_revision: peer_scope.revision, status: ProcessInstanceStatus::Cancelled,
        variables: None });
    extra_closure.events.push(PlannedEvent { scope_id: peer_scope.scope_id.clone(),
        kind: "scope_cancelled".into(), node_id: None,
        data: json!({"scope_id":peer_scope.scope_id,"parent_scope_id":peer_scope.parent_scope_id,
            "parent_token_id":peer_scope.parent_token_id,"subprocess_node_id":"PeerScope",
            "reason":"terminate_end","source_instance_id":waiting.instance_id,
            "source_event_id":source.source_event_id}),
    });
    let prior_events = repository::list_events(&fixture.db, &fixture.owner, &waiting.instance_id,
        0, 200).unwrap().0;
    let prior_start = prior_events.iter().find(|event| event.kind == "node_completed"
        && event.scope_id == peer_scope.scope_id && event.node_id.as_deref() == Some("PeerStart"))
        .unwrap();
    let historical_start_token: String = fixture.db.read().unwrap().query_row(
        "SELECT token_id FROM bpmn_tokens WHERE instance_id=?1 AND scope_id=?2 AND node_id='PeerStart' AND status='consumed'",
        rusqlite::params![waiting.instance_id, peer_scope.scope_id], |row| row.get(0)).unwrap();
    let prior_throw = prior_events.iter().find(|event| event.kind == "message_queued"
        && event.scope_id == peer_scope.scope_id && event.node_id.as_deref() == Some("PeerThrow"))
        .unwrap();
    let historical_throw_token = prior_throw.data["source_activation_id"].as_str().unwrap();
    assert!(before.tokens.iter().all(|token| token.token_id != historical_start_token
        && token.token_id != historical_throw_token));
    assert!(plan.consume_token_ids.iter().all(|id| id != &historical_start_token
        && id != historical_throw_token));
    let mut unrelated_event = plan.clone();
    let start_event_index = unrelated_event.events.len();
    unrelated_event.events.push(PlannedEvent { scope_id: peer_scope.scope_id.clone(),
        kind: "node_completed".into(), node_id: Some("PeerStart".into()),
        data: prior_start.data.clone() });
    unrelated_event.event_sources.insert(start_event_index, historical_start_token);
    let mut duplicate_message = plan.clone();
    let throw_event_index = duplicate_message.events.len();
    duplicate_message.events.push(PlannedEvent { scope_id: peer_scope.scope_id.clone(),
        kind: "message_queued".into(), node_id: Some("PeerThrow".into()),
        data: prior_throw.data.clone() });
    duplicate_message.event_sources.insert(throw_event_index, historical_throw_token.into());
    let before_rows = super::call_tests::transition_rows(&fixture);
    for (case, forged) in [("extra live sibling closure", extra_closure),
        ("historical sibling start completion", unrelated_event),
        ("historical sibling message queue", duplicate_message)] {
        assert!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &waiting.instance_id, &terminating_task.user_task_id, waiting.revision, &outputs,
            None, &forged, at_ms).is_err(), "{case} must fail closed");
        assert_eq!(super::call_tests::transition_rows(&fixture), before_rows,
            "{case} changed one of the 14 durable process tables");
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(super::call_tests::transition_rows(&fixture), before_rows);
    let unchanged = repository::runtime_snapshot(&reopened, &fixture.owner, &waiting.instance_id)
        .unwrap();
    assert_eq!(unchanged.calls.iter().find(|call| call.call_id == peer_call.call_id).unwrap().status,
        tentaflow_protocol::processes::ProcessCallStatus::Waiting);
    assert_eq!(repository::get_instance(&reopened, &fixture.owner, &called_child_id, None)
        .unwrap().status, ProcessInstanceStatus::Waiting);
    assert_eq!(repository::get_message(&reopened, &fixture.owner, &fixture.owner.user_id,
        &message_id).unwrap().message.status, ProcessMessageStatus::Pending);
    let committed = repository::complete_user_task(&reopened, &fixture.owner, &command,
        &waiting.instance_id, &terminating_task.user_task_id, waiting.revision, &outputs,
        None, &plan, at_ms).unwrap().instance;
    assert_eq!(committed.status, ProcessInstanceStatus::Waiting);
    assert_eq!(committed.variables["mapped_result"], 41);
    let actual = repository::runtime_snapshot(&reopened, &fixture.owner, &waiting.instance_id)
        .unwrap();
    assert_eq!(actual.scopes.iter().find(|scope| scope.scope_id == terminating_scope.scope_id)
        .unwrap().status, ProcessInstanceStatus::Completed);
    assert_eq!(actual.scopes.iter().find(|scope| scope.scope_id == peer_scope.scope_id)
        .unwrap().status, ProcessInstanceStatus::Waiting);
    assert_eq!(actual.calls.iter().find(|call| call.call_id == peer_call.call_id).unwrap().status,
        tentaflow_protocol::processes::ProcessCallStatus::Waiting);
    assert_eq!(repository::get_instance(&reopened, &fixture.owner, &called_child_id, None)
        .unwrap().status, ProcessInstanceStatus::Waiting);
    assert_eq!(repository::get_message(&reopened, &fixture.owner, &fixture.owner.user_id,
        &message_id).unwrap().message.status, ProcessMessageStatus::Pending);
    assert_eq!(actual.user_tasks.iter().find(|task| task.user_task_id == peer_human.user_task_id)
        .unwrap().status, ProcessUserTaskStatus::Open);
    let events = repository::list_events(&reopened, &fixture.owner, &waiting.instance_id, 0, 200)
        .unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "terminate_end_reached"
        && event.scope_id == terminating_scope.scope_id).count(), 1);
    assert!(!events.iter().any(|event| event.kind == "scope_cancelled"
        && event.scope_id == peer_scope.scope_id));
    assert!(!events.iter().any(|event| event.kind == "message_cancelled"
        && event.scope_id == peer_scope.scope_id));
    let committed_rows = super::call_tests::transition_rows(&fixture);
    let replay = repository::complete_user_task(&reopened, &fixture.owner, &command,
        &waiting.instance_id, &terminating_task.user_task_id, waiting.revision, &outputs,
        None, &plan, at_ms).unwrap().instance;
    assert_eq!(replay, committed);
    assert_eq!(super::call_tests::transition_rows(&fixture), committed_rows);
}

#[test]
fn waiting_human_activation_cannot_be_recast_as_ready_termination_input() {
    let fixture = Fixture::new();
    let mut model = starter_model();
    model.nodes.extend([
        ProcessNode { id: "HumanEntry".into(), name: "Accept human input".into(),
            kind: ProcessNodeKind::UserTask { assignee_user_id: Some(fixture.owner.user_id.clone()),
                output_mapping: BTreeMap::new() } },
        ProcessNode { id: "Split".into(), name: "Open two actual child scopes".into(),
            kind: ProcessNodeKind::ParallelGateway },
        ProcessNode { id: "ScopeA".into(), name: "First terminating child".into(),
            kind: ProcessNodeKind::SubProcess {
                body: immediate_terminal_body("StartA", "TerminateA", "FlowA", "local_a", 11),
                input_mapping: BTreeMap::new(),
                output_mapping: BTreeMap::from([("parent_a".into(), "outputs.local_a".into())]),
            } },
        ProcessNode { id: "ScopeB".into(), name: "Second terminating child".into(),
            kind: ProcessNodeKind::SubProcess {
                body: immediate_terminal_body("StartB", "TerminateB", "FlowB", "local_b", 22),
                input_mapping: BTreeMap::new(),
                output_mapping: BTreeMap::from([("parent_b".into(), "outputs.local_b".into())]),
            } },
        ProcessNode { id: "Join".into(), name: "Join both child returns".into(),
            kind: ProcessNodeKind::ParallelGateway },
    ]);
    model.sequence_flows = vec![
        edge("StartHuman", "Start_1", "HumanEntry"),
        edge("HumanSplit", "HumanEntry", "Split"),
        edge("SplitA", "Split", "ScopeA"),
        edge("SplitB", "Split", "ScopeB"),
        edge("AJoin", "ScopeA", "Join"),
        edge("BJoin", "ScopeB", "Join"),
        edge("JoinEnd", "Join", "End_1"),
    ];
    let waiting = start_model(&fixture, &model);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
        .unwrap();
    let task = snapshot.user_tasks.iter().find(|task| task.node_id == "HumanEntry").unwrap();
    let waiting_token_id = task.token_id.as_ref().unwrap();
    assert!(snapshot.tokens.iter().any(|token| token.token_id == *waiting_token_id
        && token.status == "waiting"));
    assert!(runtime::plan_advance(&snapshot, chrono::Utc::now().timestamp_millis()).unwrap()
        .termination_attempts.is_empty());
    let command = stamp("complete the actual human activation after rejected ready bypass");
    let outputs = json!({"decision":"accepted"});
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_user_completion(&snapshot, &task.user_task_id, &outputs, None, at_ms,
        runtime::test_support::human_input(&snapshot, &task.user_task_id, &command)).unwrap();
    assert_eq!(plan.termination_attempts.len(), 2);
    assert!(plan.termination_attempts.iter().all(|attempt| matches!(attempt,
        repository::TerminationAttempt::Success(source)
            if matches!(&source.accepted_input, repository::AcceptedInputRef::Human { .. }))));
    let mut forged = plan.clone();
    let repository::TerminationAttempt::Success(source) = &mut forged.termination_attempts[1] else {
        panic!("the second factual child must reach TerminateEnd");
    };
    source.accepted_input = repository::AcceptedInputRef::PersistedReady {
        token_id: waiting_token_id.clone(),
        expected_instance_revision: snapshot.instance.revision,
    };
    assert_eq!(forged.event_sources, plan.event_sources);
    assert_eq!(forged.token_sources, plan.token_sources);
    let before = super::call_tests::transition_rows(&fixture);
    assert!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
        &waiting.instance_id, &task.user_task_id, snapshot.instance.revision, &outputs, None,
        &forged, at_ms).is_err());
    assert_eq!(super::call_tests::transition_rows(&fixture), before);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(repository::get_instance(&reopened, &fixture.owner, &waiting.instance_id, None)
        .unwrap().status, ProcessInstanceStatus::Waiting);
    let completed = repository::complete_user_task(&reopened, &fixture.owner, &command,
        &waiting.instance_id, &task.user_task_id, snapshot.instance.revision, &outputs, None,
        &plan, at_ms).unwrap().instance;
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_eq!(completed.variables["parent_a"], 11);
    assert_eq!(completed.variables["parent_b"], 22);
    let committed_rows = super::call_tests::transition_rows(&fixture);
    let replay = repository::complete_user_task(&reopened, &fixture.owner, &command,
        &waiting.instance_id, &task.user_task_id, snapshot.instance.revision, &outputs, None,
        &plan, at_ms).unwrap().instance;
    assert_eq!(replay, completed);
    assert_eq!(super::call_tests::transition_rows(&fixture), committed_rows);
}

#[test]
fn terminating_xor_cannot_select_the_false_or_unselected_default_edge() {
    for take_true in [false, true] {
        let fixture = Fixture::new();
        let mut model = starter_model();
        model.variables.insert("choose_true".into(), json!(take_true));
        model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
            ProcessNodeKind::TerminateEnd;
        model.nodes.extend([
            ProcessNode { id: "HumanChoice".into(), name: "Choose after human input".into(),
                kind: ProcessNodeKind::UserTask { assignee_user_id: Some(fixture.owner.user_id.clone()),
                    output_mapping: BTreeMap::new() } },
            ProcessNode { id: "Choice".into(), name: "Choose the pinned branch".into(),
                kind: ProcessNodeKind::ExclusiveGateway { default_flow_id: Some("ChoiceDefault".into()) } },
        ]);
        let mut true_edge = edge("ChoiceTrue", "Choice", "End_1");
        true_edge.condition = Some("vars.choose_true == true".into());
        model.sequence_flows = vec![
            edge("StartHuman", "Start_1", "HumanChoice"),
            edge("HumanChoiceGateway", "HumanChoice", "Choice"),
            true_edge,
            edge("ChoiceDefault", "Choice", "End_1"),
        ];
        let waiting = start_model(&fixture, &model);
        let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
            .unwrap();
        let task = snapshot.user_tasks.iter().find(|task| task.node_id == "HumanChoice").unwrap();
        let command = stamp("complete the factual human choice");
        let outputs = json!({});
        let at_ms = chrono::Utc::now().timestamp_millis();
        let plan = runtime::plan_user_completion(&snapshot, &task.user_task_id, &outputs, None,
            at_ms, runtime::test_support::human_input(&snapshot, &task.user_task_id, &command))
            .unwrap();
        assert_eq!(plan.termination_attempts.len(), 1);
        let selected = if take_true { "ChoiceTrue" } else { "ChoiceDefault" };
        let forged_edge = if take_true { "ChoiceDefault" } else { "ChoiceTrue" };
        let actual_choice = plan.events.iter().enumerate().find(|(_, event)|
            event.kind == "exclusive_selected" && event.node_id.as_deref() == Some("Choice"))
            .unwrap();
        assert_eq!(actual_choice.1.data["sequence_flow_id"], selected);
        let repository::TerminationAttempt::Success(actual_source) = &plan.termination_attempts[0] else {
            panic!("the selected branch must reach TerminateEnd");
        };
        assert_eq!(actual_source.source_arrival_edge_id.as_deref(), Some(selected));
        let mut forged = plan.clone();
        forged.events[actual_choice.0].data["sequence_flow_id"] = json!(forged_edge);
        let terminal = forged.create_tokens.iter_mut().find(|token| token.token_id == actual_source.source_token_id)
            .unwrap();
        assert_eq!(terminal.arrival_edge_id.as_deref(), Some(selected));
        terminal.arrival_edge_id = Some(forged_edge.into());
        let repository::TerminationAttempt::Success(source) = &mut forged.termination_attempts[0] else {
            unreachable!();
        };
        source.source_arrival_edge_id = Some(forged_edge.into());
        assert_eq!(forged.event_sources, plan.event_sources);
        assert_eq!(forged.token_sources, plan.token_sources);
        let xor_token = plan.create_tokens.iter().find(|token|
            token.node_id == "Choice" && token.status == "ready"
                && plan.consume_token_ids.contains(&token.token_id)).unwrap();
        let mut extra_wait = plan.clone();
        let mut ghost = xor_token.clone();
        ghost.token_id = Uuid::new_v4().to_string();
        ghost.status = "waiting".into();
        extra_wait.token_sources.insert(ghost.token_id.clone(), xor_token.token_id.clone());
        extra_wait.cancel_token_ids.push(ghost.token_id.clone());
        extra_wait.create_tokens.push(ghost);
        assert_eq!(extra_wait.event_sources, plan.event_sources);
        let before = super::call_tests::transition_rows(&fixture);
        assert!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &waiting.instance_id, &task.user_task_id, snapshot.instance.revision, &outputs, None,
            &extra_wait, at_ms).is_err(), "the selected XOR cannot create a same-node wait");
        assert_eq!(super::call_tests::transition_rows(&fixture), before);
        let error = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &waiting.instance_id, &task.user_task_id, snapshot.instance.revision, &outputs, None,
            &forged, at_ms).unwrap_err();
        assert!(format!("{error:#}").contains("exclusive choice differs from pinned source-time CEL/default"),
            "unselected edge {forged_edge} was rejected for another reason: {error:#}");
        assert_eq!(super::call_tests::transition_rows(&fixture), before);
        let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
        let committed = repository::complete_user_task(&reopened, &fixture.owner, &command,
            &waiting.instance_id, &task.user_task_id, snapshot.instance.revision, &outputs, None,
            &plan, at_ms).unwrap().instance;
        assert_eq!(committed.status, ProcessInstanceStatus::Completed);
        let history = repository::list_events(&reopened, &fixture.owner, &waiting.instance_id,
            0, 200).unwrap().0;
        assert_eq!(history.iter().filter(|event| event.kind == "exclusive_selected"
            && event.node_id.as_deref() == Some("Choice")
            && event.data["sequence_flow_id"] == selected).count(), 1);
        assert_eq!(history.iter().filter(|event| event.kind == "terminate_end_reached").count(), 1);
        let committed_rows = super::call_tests::transition_rows(&fixture);
        let replay = repository::complete_user_task(&reopened, &fixture.owner, &command,
            &waiting.instance_id, &task.user_task_id, snapshot.instance.revision, &outputs, None,
            &plan, at_ms).unwrap().instance;
        assert_eq!(replay, committed);
        assert_eq!(super::call_tests::transition_rows(&fixture), committed_rows);
    }
}

#[test]
fn terminating_root_requires_exact_factual_descendant_cancellation_event() {
    let fixture = Fixture::new();
    let mut model = starter_model();
    model.timer_timezone = Some("UTC".into());
    model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    model.nodes.extend([
        ProcessNode { id: "Split".into(), name: "Open root and child work".into(),
            kind: ProcessNodeKind::ParallelGateway },
        ProcessNode { id: "RootHuman".into(), name: "Choose root termination".into(),
            kind: ProcessNodeKind::UserTask { assignee_user_id: Some(fixture.owner.user_id.clone()),
                output_mapping: BTreeMap::new() } },
        ProcessNode { id: "ChildScope".into(), name: "Retain active child work".into(),
            kind: ProcessNodeKind::SubProcess {
                body: ProcessSubProcess {
                    nodes: vec![
                        ProcessNode { id: "ChildStart".into(), name: "Enter child".into(),
                            kind: ProcessNodeKind::Start },
                        ProcessNode { id: "ChildRace".into(), name: "Wait for the first child event".into(),
                            kind: ProcessNodeKind::EventBasedGateway },
                        ProcessNode { id: "ChildTimerA".into(), name: "First child timer".into(),
                            kind: ProcessNodeKind::TimerCatch {
                                timer: ProcessTimerSpec::Duration { seconds: 10 },
                            } },
                        ProcessNode { id: "ChildTimerB".into(), name: "Second child timer".into(),
                            kind: ProcessNodeKind::TimerCatch {
                                timer: ProcessTimerSpec::Duration { seconds: 20 },
                            } },
                        ProcessNode { id: "ChildEnd".into(), name: "Complete child".into(),
                            kind: ProcessNodeKind::End },
                    ],
                    sequence_flows: vec![
                        edge("ChildEntry", "ChildStart", "ChildRace"),
                        edge("ChildToTimerA", "ChildRace", "ChildTimerA"),
                        edge("ChildToTimerB", "ChildRace", "ChildTimerB"),
                        edge("TimerAEnd", "ChildTimerA", "ChildEnd"),
                        edge("TimerBEnd", "ChildTimerB", "ChildEnd"),
                    ],
                    variables: BTreeMap::new(), diagram: ProcessDiagram::default(),
                },
                input_mapping: BTreeMap::new(), output_mapping: BTreeMap::new(),
            } },
    ]);
    model.sequence_flows = vec![
        edge("StartSplit", "Start_1", "Split"),
        edge("SplitHuman", "Split", "RootHuman"),
        edge("SplitChild", "Split", "ChildScope"),
        edge("HumanTerminate", "RootHuman", "End_1"),
        edge("ChildTerminate", "ChildScope", "End_1"),
    ];
    let waiting = start_model(&fixture, &model);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
        .unwrap();
    let child = snapshot.scopes.iter().find(|scope|
        scope.subprocess_node_id.as_deref() == Some("ChildScope")).unwrap();
    assert_eq!(child.status, ProcessInstanceStatus::Waiting);
    let race = snapshot.event_races.iter().find(|race| race.scope_id == child.scope_id).unwrap();
    assert_eq!(race.status, tentaflow_protocol::processes::ProcessEventRaceStatus::Open);
    let child_timers = snapshot.timers.iter().filter(|timer| timer.scope_id.as_deref()
        == Some(child.scope_id.as_str())).collect::<Vec<_>>();
    assert_eq!(child_timers.len(), 2);
    assert!(child_timers.iter().all(|timer| timer.status == ProcessTimerStatus::Pending));
    let root_task = snapshot.user_tasks.iter().find(|task| task.node_id == "RootHuman").unwrap();
    let command = stamp("terminate root with a genuinely active embedded descendant");
    let outputs = json!({});
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_user_completion(&snapshot, &root_task.user_task_id, &outputs, None,
        at_ms, runtime::test_support::human_input(&snapshot, &root_task.user_task_id, &command))
        .unwrap();
    assert_eq!(plan.termination_attempts.len(), 1);
    assert!(matches!(&plan.termination_attempts[0], repository::TerminationAttempt::Success(_)));
    let cancelled_index = plan.events.iter().position(|event|
        event.kind == "scope_cancelled" && event.scope_id == child.scope_id).unwrap();
    assert_eq!(plan.events[cancelled_index].data["reason"], "terminate_end");
    assert_eq!(plan.events[cancelled_index].data["parent_token_id"],
        child.parent_token_id.as_ref().unwrap().as_str());
    assert!(plan.events[cancelled_index].data["source_event_id"].as_str().is_some());
    assert!(plan.scope_updates.iter().any(|update| update.scope_id == child.scope_id
        && update.status == ProcessInstanceStatus::Cancelled));
    let race_index = plan.events.iter().position(|event| event.kind == "event_race_cancelled"
        && event.data["race_id"] == race.race_id).unwrap();
    assert_eq!(plan.events[race_index].data["reason"], "terminate_end");
    let timer = child_timers[0];
    let timer_index = plan.events.iter().position(|event| event.kind == "timer_cancelled"
        && event.data["timer_id"] == timer.timer_id).unwrap();
    assert_eq!(plan.events[timer_index].data["reason"], "terminate_end");
    let mut wrong_reason = plan.clone();
    wrong_reason.events[cancelled_index].data["reason"] = json!("scope_cancelled");
    let mut missing_source = plan.clone();
    missing_source.events[cancelled_index].data.as_object_mut().unwrap()
        .remove("source_event_id");
    let mut wrong_source = plan.clone();
    wrong_source.events[cancelled_index].data["source_event_id"] = json!(Uuid::new_v4().to_string());
    let mut wrong_parent = plan.clone();
    wrong_parent.events[cancelled_index].data["parent_token_id"] = json!(Uuid::new_v4().to_string());
    let mut wrong_race_reason = plan.clone();
    wrong_race_reason.events[race_index].data["reason"] = json!("scope_cancelled");
    let mut wrong_race_source = plan.clone();
    wrong_race_source.events[race_index].data["source_event_id"] = json!(Uuid::new_v4().to_string());
    let mut wrong_timer_reason = plan.clone();
    wrong_timer_reason.events[timer_index].data["reason"] = json!("scope_cancelled");
    wrong_timer_reason.timer_updates.iter_mut().find(|update| update.timer_id == timer.timer_id)
        .unwrap().last_reason = Some("scope_cancelled".into());
    let before = super::call_tests::transition_rows(&fixture);
    for (case, forged) in [
        ("ordinary cancellation reason", wrong_reason),
        ("missing termination source", missing_source),
        ("wrong termination source", wrong_source),
        ("wrong child parent wait", wrong_parent),
        ("ordinary event race cancellation reason", wrong_race_reason),
        ("wrong event race termination source", wrong_race_source),
        ("ordinary timer cancellation reason and update", wrong_timer_reason),
    ] {
        assert!(repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &waiting.instance_id, &root_task.user_task_id, waiting.revision, &outputs, None,
            &forged, at_ms).is_err(), "{case} must fail closed");
        assert_eq!(super::call_tests::transition_rows(&fixture), before,
            "{case} changed durable process rows");
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    assert_eq!(super::call_tests::transition_rows(&fixture), before);
    let completed = repository::complete_user_task(&reopened, &fixture.owner, &command,
        &waiting.instance_id, &root_task.user_task_id, waiting.revision, &outputs, None,
        &plan, at_ms).unwrap().instance;
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    let actual = repository::runtime_snapshot(&reopened, &fixture.owner, &waiting.instance_id)
        .unwrap();
    assert_eq!(actual.scopes.iter().find(|scope| scope.scope_id == child.scope_id)
        .unwrap().status, ProcessInstanceStatus::Cancelled);
    assert_eq!(actual.event_races.iter().find(|row| row.race_id == race.race_id)
        .unwrap().status, tentaflow_protocol::processes::ProcessEventRaceStatus::Cancelled);
    assert!(child_timers.iter().all(|timer| actual.timers.iter().find(|row|
        row.timer_id == timer.timer_id).unwrap().status == ProcessTimerStatus::Cancelled));
    let events = repository::list_events(&reopened, &fixture.owner, &waiting.instance_id, 0, 200)
        .unwrap().0;
    let terminal = events.iter().find(|event| event.kind == "terminate_end_reached").unwrap();
    let cancelled = events.iter().find(|event| event.kind == "scope_cancelled"
        && event.scope_id == child.scope_id).unwrap();
    assert_eq!(cancelled.data["reason"], "terminate_end");
    assert_eq!(cancelled.data["source_event_id"], terminal.event_id);
    assert_eq!(cancelled.data["parent_token_id"], child.parent_token_id.as_ref().unwrap().as_str());
    let race_event = events.iter().find(|event| event.kind == "event_race_cancelled"
        && event.data["race_id"] == race.race_id).unwrap();
    assert_eq!(race_event.data["reason"], "terminate_end");
    assert_eq!(race_event.data["source_event_id"], terminal.event_id);
    assert_eq!(events.iter().filter(|event| event.kind == "timer_cancelled"
        && event.scope_id == child.scope_id && event.data["reason"] == "terminate_end"
        && event.data["source_event_id"] == terminal.event_id).count(), 2);
    let committed_rows = super::call_tests::transition_rows(&fixture);
    let replay = repository::complete_user_task(&reopened, &fixture.owner, &command,
        &waiting.instance_id, &root_task.user_task_id, waiting.revision, &outputs, None,
        &plan, at_ms).unwrap().instance;
    assert_eq!(replay, completed);
    assert_eq!(super::call_tests::transition_rows(&fixture), committed_rows);
}

#[test]
fn same_start_embedded_race_arms_its_timer_before_terminal_closure() {
    let fixture = Fixture::new();
    let foreign = start_model(&fixture, &runtime::test_support::user_model(Some(&fixture.owner.user_id)));
    let foreign_token = repository::runtime_snapshot(&fixture.db, &fixture.owner, &foreign.instance_id)
        .unwrap().tokens.into_iter().find(|token| token.status == "waiting").unwrap().token_id;
    let mut model = starter_model();
    model.timer_timezone = Some("UTC".into());
    model.messages.push(ProcessMessageDeclaration {
        message_id: "RaceMessage".into(), name: "RaceEvidence".into(),
    });
    model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    model.nodes.extend([
        ProcessNode { id: "RootSplit".into(), name: "Arm child before terminating".into(),
            kind: ProcessNodeKind::ParallelGateway },
        ProcessNode { id: "RaceScope".into(), name: "Arm an embedded event race".into(),
            kind: ProcessNodeKind::SubProcess {
                body: ProcessSubProcess {
                    nodes: vec![
                        ProcessNode { id: "ChildStart".into(), name: "Enter child".into(),
                            kind: ProcessNodeKind::Start },
                        ProcessNode { id: "ChildRace".into(), name: "Wait for first input".into(),
                            kind: ProcessNodeKind::EventBasedGateway },
                        ProcessNode { id: "ChildTimer".into(), name: "Timer branch".into(),
                            kind: ProcessNodeKind::TimerCatch {
                                timer: ProcessTimerSpec::Duration { seconds: 600 },
                            } },
                        ProcessNode { id: "ChildMessage".into(), name: "Message branch".into(),
                            kind: ProcessNodeKind::MessageCatch {
                                message_ref: "RaceMessage".into(),
                                correlation_expression: "'case-1'".into(),
                                output_mapping: BTreeMap::new(),
                            } },
                        ProcessNode { id: "ChildEnd".into(), name: "Finish child".into(),
                            kind: ProcessNodeKind::End },
                    ],
                    sequence_flows: vec![
                        edge("ChildEntry", "ChildStart", "ChildRace"),
                        edge("RaceToTimer", "ChildRace", "ChildTimer"),
                        edge("RaceToMessage", "ChildRace", "ChildMessage"),
                        edge("TimerToEnd", "ChildTimer", "ChildEnd"),
                        edge("MessageToEnd", "ChildMessage", "ChildEnd"),
                    ],
                    variables: BTreeMap::new(), diagram: ProcessDiagram::default(),
                },
                input_mapping: BTreeMap::new(), output_mapping: BTreeMap::new(),
            } },
        ProcessNode { id: "SourceDelay".into(), name: "Complete a factual sibling scope".into(),
            kind: ProcessNodeKind::SubProcess {
                body: ProcessSubProcess {
                    nodes: vec![
                        ProcessNode { id: "DelayStart".into(), name: "Enter sibling".into(),
                            kind: ProcessNodeKind::Start },
                        ProcessNode { id: "DelayEnd".into(), name: "Finish sibling".into(),
                            kind: ProcessNodeKind::End },
                    ],
                    sequence_flows: vec![edge("DelayFlow", "DelayStart", "DelayEnd")],
                    variables: BTreeMap::new(), diagram: ProcessDiagram::default(),
                },
                input_mapping: BTreeMap::new(), output_mapping: BTreeMap::new(),
            } },
    ]);
    model.sequence_flows = vec![
        edge("RootEntry", "Start_1", "RootSplit"),
        edge("ArmChild", "RootSplit", "RaceScope"),
        edge("CompleteSibling", "RootSplit", "SourceDelay"),
        edge("TerminateRoot", "SourceDelay", "End_1"),
        edge("ChildToEnd", "RaceScope", "End_1"),
    ];
    let version = publish_model(&fixture, &model);
    let instance_id = Uuid::new_v4().to_string();
    let variables = serde_json::to_value(&model.variables).unwrap();
    let command = stamp("start and close a newly armed embedded race");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&version.model, &instance_id, &fixture.owner,
        &version.definition_id, version.version, variables.clone(), StartCause::Manual,
        at_ms, runtime::test_support::manual_input(&command)).unwrap();
    let timer = plan.create_timers.iter().find(|timer| timer.node_id == "ChildTimer").unwrap();
    let race = plan.create_event_races.iter().find(|race| race.gateway_node_id == "ChildRace").unwrap();
    assert_eq!(timer.race_id.as_deref(), Some(race.race_id.as_str()));
    assert!(plan.race_updates.iter().any(|update| update.race_id == race.race_id
        && update.status == tentaflow_protocol::processes::ProcessEventRaceStatus::Cancelled));
    assert!(plan.timer_updates.iter().any(|update| update.timer_id == timer.timer_id
        && update.status == ProcessTimerStatus::Cancelled));
    let mut wrong_race = plan.clone();
    wrong_race.create_timers.iter_mut().find(|row| row.timer_id == timer.timer_id).unwrap()
        .race_id = Some(Uuid::new_v4().to_string());
    let mut wrong_scope = plan.clone();
    wrong_scope.create_timers.iter_mut().find(|row| row.timer_id == timer.timer_id).unwrap()
        .scope_id = Some(instance_id.clone());
    let mut wrong_source = plan.clone();
    wrong_source.events.iter_mut().find(|event| event.kind == "timer_cancelled"
        && event.data["timer_id"] == timer.timer_id).unwrap().data["source_event_id"] =
        json!(Uuid::new_v4().to_string());
    let mut missing_timer = plan.clone();
    missing_timer.create_timers.retain(|row| row.timer_id != timer.timer_id);
    let mut foreign_cancel = plan.clone();
    foreign_cancel.cancel_token_ids.push(foreign_token);
    let before = super::call_tests::transition_rows(&fixture);
    for (case, forged) in [
        ("wrong race", wrong_race), ("wrong scope", wrong_scope),
        ("wrong termination source", wrong_source), ("missing timer", missing_timer),
        ("foreign activation cancellation", foreign_cancel),
    ] {
        assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
            &instance_id, &version.definition_id, version.version, &variables, &forged,
            at_ms).is_err(), "{case} must reject the whole start");
        assert_eq!(super::call_tests::transition_rows(&fixture), before,
            "{case} changed durable process rows");
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let committed = repository::start_instance(&reopened, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, &plan,
        at_ms).unwrap();
    assert_eq!(committed.status, ProcessInstanceStatus::Completed);
    let actual = repository::runtime_snapshot(&reopened, &fixture.owner, &instance_id).unwrap();
    assert_eq!(actual.timers.iter().find(|row| row.timer_id == timer.timer_id).unwrap().status,
        ProcessTimerStatus::Cancelled);
    assert_eq!(actual.event_races.iter().find(|row| row.race_id == race.race_id).unwrap().status,
        tentaflow_protocol::processes::ProcessEventRaceStatus::Cancelled);
    let history = repository::list_events(&reopened, &fixture.owner, &instance_id, 0, 200).unwrap().0;
    let source = history.iter().find(|event| event.kind == "terminate_end_reached").unwrap();
    assert_eq!(history.iter().filter(|event| event.kind == "timer_cancelled"
        && event.data["timer_id"] == timer.timer_id
        && event.data["source_event_id"] == source.event_id).count(), 1);
    let committed_rows = super::call_tests::transition_rows(&fixture);
    assert_eq!(repository::start_instance(&reopened, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, &plan,
        at_ms).unwrap(), committed);
    assert_eq!(super::call_tests::transition_rows(&fixture), committed_rows);
}

#[test]
fn completing_child_work_disarms_its_boundaries_once_before_two_terminations() {
    let fixture = Fixture::new();
    let mut model = terminating_scope_model(&fixture.owner.user_id, "human", false);
    model.nodes.iter_mut().find(|node| node.id == "RootEnd_Scope").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    model.timer_timezone = Some("UTC".into());
    model.messages.push(ProcessMessageDeclaration {
        message_id: "BoundaryReview".into(), name: "boundary.review".into(),
    });
    let scope = model.nodes.iter_mut().find(|node| node.id == "Scope").unwrap();
    let ProcessNodeKind::SubProcess { body, .. } = &mut scope.kind else { panic!("embedded scope"); };
    body.nodes.extend([
        ProcessNode { id: "AttachedTimer".into(), name: "Work timer".into(),
            kind: ProcessNodeKind::BoundaryTimer {
                attached_to_id: "ChildInput".into(), cancel_activity: true,
                timer: ProcessTimerSpec::Duration { seconds: 600 },
            } },
        ProcessNode { id: "AttachedMessage".into(), name: "Work message".into(),
            kind: ProcessNodeKind::BoundaryMessage {
                attached_to_id: "ChildInput".into(), cancel_activity: false,
                message_ref: "BoundaryReview".into(),
                correlation_expression: "'case-1'".into(),
                output_mapping: BTreeMap::new(),
            } },
        ProcessNode { id: "TimerWork".into(), name: "Timer continuation".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: Some(fixture.owner.user_id.clone()),
                output_mapping: BTreeMap::new(),
            } },
        ProcessNode { id: "MessageWork".into(), name: "Message continuation".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: Some(fixture.owner.user_id.clone()),
                output_mapping: BTreeMap::new(),
            } },
        ProcessNode { id: "BoundaryEnd".into(), name: "Other boundary end".into(),
            kind: ProcessNodeKind::End },
    ]);
    body.sequence_flows.extend([
        edge("TimerContinuation", "AttachedTimer", "TimerWork"),
        edge("MessageContinuation", "AttachedMessage", "MessageWork"),
        edge("TimerFinish", "TimerWork", "BoundaryEnd"),
        edge("MessageFinish", "MessageWork", "BoundaryEnd"),
    ]);
    let waiting = start_model(&fixture, &model);
    let snapshot = repository::runtime_snapshot(&fixture.db, &fixture.owner, &waiting.instance_id)
        .unwrap();
    let task = snapshot.user_tasks.iter().find(|task| task.node_id == "ChildInput").unwrap();
    let timer = snapshot.timers.iter().find(|timer| timer.node_id == "AttachedTimer").unwrap();
    let subscription = snapshot.subscriptions.iter().find(|subscription|
        subscription.node_id == "AttachedMessage").unwrap();
    let child_scope = snapshot.scopes.iter().find(|scope|
        scope.subprocess_node_id.as_deref() == Some("Scope")).unwrap();
    let foreign = start_model(&fixture, &runtime::test_support::user_model(Some(&fixture.owner.user_id)));
    let foreign_token = repository::runtime_snapshot(&fixture.db, &fixture.owner, &foreign.instance_id)
        .unwrap().tokens.into_iter().find(|token| token.status == "waiting").unwrap().token_id;
    let command = stamp("complete attached work before child and ancestor termination");
    let outputs = json!({"accepted":"work"});
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_user_completion(&snapshot, &task.user_task_id, &outputs, None,
        at_ms, runtime::test_support::human_input(&snapshot, &task.user_task_id, &command)).unwrap();
    assert_eq!(plan.termination_attempts.len(), 2);
    let timer_events = plan.events.iter().enumerate().filter(|(_,event)|
        event.kind == "timer_cancelled" && event.data["timer_id"] == timer.timer_id)
        .collect::<Vec<_>>();
    let subscription_events = plan.events.iter().enumerate().filter(|(_,event)|
        event.kind == "subscription_cancelled"
            && event.data["subscription_id"] == subscription.subscription_id)
        .collect::<Vec<_>>();
    assert_eq!(timer_events.len(), 1);
    assert_eq!(subscription_events.len(), 1);
    assert_eq!(plan.timer_updates.iter().filter(|update|
        update.timer_id == timer.timer_id).count(), 1);
    assert_eq!(plan.subscription_updates.iter().filter(|update|
        update.subscription_id == subscription.subscription_id).count(), 1);
    let timer_index = timer_events[0].0;
    let subscription_index = subscription_events[0].0;
    let work_index = plan.events.iter().position(|event|
        event.kind == "user_task_completed" && event.node_id.as_deref() == Some("ChildInput")).unwrap();
    let child_source_index = plan.events.iter().position(|event|
        event.kind == "terminate_end_reached" && event.scope_id == child_scope.scope_id).unwrap();
    let parent_source_index = plan.events.iter().position(|event|
        event.kind == "terminate_end_reached" && event.scope_id == waiting.instance_id).unwrap();
    assert!(timer_index < work_index && subscription_index < work_index
        && work_index < child_source_index && child_source_index < parent_source_index);
    assert_eq!(plan.events[timer_index].data, json!({
        "kind":"Boundary","timer_id":timer.timer_id,"attached_to_id":"ChildInput",
        "attached_token_id":task.token_id,"reason":"activity_completed",
        "winning_timer_id":null,
    }));
    assert_eq!(plan.events[subscription_index].data, json!({
        "subscription_id":subscription.subscription_id,
        "attached_token_id":task.token_id,"reason":"activity_completed",
    }));
    let mut wrong_timer_id = plan.clone();
    wrong_timer_id.events[timer_index].data["timer_id"] = json!(Uuid::new_v4().to_string());
    let mut wrong_timer_revision = plan.clone();
    wrong_timer_revision.timer_updates.iter_mut().find(|update|
        update.timer_id == timer.timer_id).unwrap().expected_revision += 1;
    let mut duplicate_timer = plan.clone();
    duplicate_timer.events.push(plan.events[timer_index].clone());
    let mut wrong_reason = plan.clone();
    wrong_reason.events[timer_index].data["reason"] = json!("terminate_end");
    wrong_reason.timer_updates.iter_mut().find(|update|
        update.timer_id == timer.timer_id).unwrap().last_reason = Some("terminate_end".into());
    let mut wrong_source = plan.clone();
    wrong_source.events[timer_index].data["source_event_id"] = json!(Uuid::new_v4().to_string());
    let mut wrong_subscription = plan.clone();
    wrong_subscription.events[subscription_index].data["subscription_id"] =
        json!(Uuid::new_v4().to_string());
    let mut wrong_subscription_revision = plan.clone();
    wrong_subscription_revision.subscription_updates.iter_mut().find(|update|
        update.subscription_id == subscription.subscription_id).unwrap().expected_revision += 1;
    let mut wrong_subscription_source = plan.clone();
    wrong_subscription_source.events[subscription_index].data["source_event_id"] =
        json!(Uuid::new_v4().to_string());
    let mut foreign_activation = plan.clone();
    foreign_activation.events[timer_index].data["attached_token_id"] = json!(foreign_token);
    let completed_index = plan.events.iter().position(|event|
        event.kind == "scope_completed" && event.scope_id == child_scope.scope_id
            && event.data["reason"] == "terminate_end").unwrap();
    assert!(child_source_index < completed_index && completed_index < parent_source_index);
    let mut wrong_return_child = plan.clone();
    wrong_return_child.events[completed_index].data["scope_id"] = json!(foreign.instance_id);
    let mut wrong_return_source = plan.clone();
    wrong_return_source.events[completed_index].data["source_event_id"] =
        json!(Uuid::new_v4().to_string());
    let mut wrong_return_parent = plan.clone();
    wrong_return_parent.events[completed_index].data["parent_token_id"] =
        json!(foreign_token);
    let mut early_return = plan.clone();
    early_return.events.swap(child_source_index, completed_index);
    early_return.event_sources = plan.event_sources.iter().map(|(index, source)|
        (if *index == child_source_index { completed_index }
            else if *index == completed_index { child_source_index }
            else { *index }, source.clone())).collect();
    early_return.event_ids = plan.event_ids.iter().map(|(index, id)|
        (if *index == child_source_index { completed_index }
            else if *index == completed_index { child_source_index }
            else { *index }, id.clone())).collect();
    for attempt in &mut early_return.termination_attempts {
        match attempt {
            repository::TerminationAttempt::Success(source)
                if source.source_event_index == child_source_index =>
                source.source_event_index = completed_index,
            repository::TerminationAttempt::ReturnFailure(failure)
                if failure.source_event_index == child_source_index =>
                failure.source_event_index = completed_index,
            _ => {},
        }
    }
    let mut late_completion = plan.clone();
    let completion = late_completion.events.remove(work_index);
    late_completion.events.insert(parent_source_index, completion);
    let remap_fact = |index: usize| {
        if index == work_index { parent_source_index }
        else if index > work_index && index <= parent_source_index { index - 1 }
        else { index }
    };
    let remap_effect = |index: usize| {
        if index > work_index && index <= parent_source_index { index - 1 }
        else { index }
    };
    late_completion.event_sources = plan.event_sources.iter().map(|(index, source)|
        (remap_fact(*index), source.clone())).collect();
    late_completion.event_ids = plan.event_ids.iter().map(|(index, id)|
        (remap_fact(*index), id.clone())).collect();
    for effect in &mut late_completion.variable_effects {
        match effect {
            repository::VariableEffect::Mapped { event_index, .. }
            | repository::VariableEffect::ScopeEntry { event_index, .. } =>
                *event_index = remap_effect(*event_index),
        }
    }
    for attempt in &mut late_completion.termination_attempts {
        match attempt {
            repository::TerminationAttempt::Success(source) =>
                source.source_event_index = remap_fact(source.source_event_index),
            repository::TerminationAttempt::ReturnFailure(failure) =>
                failure.source_event_index = remap_fact(failure.source_event_index),
        }
    }
    for message in &mut late_completion.create_messages {
        message.source_event_index = remap_fact(message.source_event_index);
    }
    assert!(late_completion.variable_effects.iter().any(|effect| match effect {
        repository::VariableEffect::Mapped { event_index, .. }
        | repository::VariableEffect::ScopeEntry { event_index, .. } =>
            *event_index <= work_index,
    }));
    assert!(late_completion.events.iter().position(|event|
        event.kind == "user_task_completed" && event.node_id.as_deref() == Some("ChildInput"))
        .unwrap() > late_completion.termination_attempts.iter().map(|attempt| match attempt {
            repository::TerminationAttempt::Success(source) => source.source_event_index,
            repository::TerminationAttempt::ReturnFailure(failure) => failure.source_event_index,
        }).max().unwrap());
    let before = super::call_tests::transition_rows(&fixture);
    for (case, forged) in [
        ("wrong boundary timer", wrong_timer_id),
        ("stale timer revision", wrong_timer_revision),
        ("duplicate boundary cancellation", duplicate_timer),
        ("false termination reason", wrong_reason),
        ("foreign cancellation source", wrong_source),
        ("wrong boundary subscription", wrong_subscription),
        ("stale subscription revision", wrong_subscription_revision),
        ("foreign subscription source", wrong_subscription_source),
        ("foreign attached activation", foreign_activation),
        ("foreign completed child", wrong_return_child),
        ("foreign completed child source", wrong_return_source),
        ("foreign completed child parent", wrong_return_parent),
        ("completed child before its source", early_return),
        ("completion after both factual termination sources", late_completion),
    ] {
        let rejected = repository::complete_user_task(&fixture.db, &fixture.owner, &command,
            &waiting.instance_id, &task.user_task_id, waiting.revision, &outputs, None,
            &forged, at_ms).unwrap_err();
        if case == "completion after both factual termination sources" {
            assert!(format!("{rejected:#}").contains(
                "termination history has an unlinked or duplicate effect fact"),
                "late completion must fail its factual boundary history: {rejected:#}");
        }
        assert_eq!(super::call_tests::transition_rows(&fixture), before,
            "{case} changed durable process rows");
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let completed = repository::complete_user_task(&reopened, &fixture.owner, &command,
        &waiting.instance_id, &task.user_task_id, waiting.revision, &outputs, None,
        &plan, at_ms).unwrap().instance;
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    assert_eq!(completed.variables["mapped_result"], 41);
    let actual = repository::runtime_snapshot(&reopened, &fixture.owner, &waiting.instance_id).unwrap();
    assert_eq!(actual.scopes.iter().find(|scope| scope.scope_id == child_scope.scope_id)
        .unwrap().status, ProcessInstanceStatus::Completed);
    assert_eq!(actual.timers.iter().find(|row| row.timer_id == timer.timer_id)
        .unwrap().status, ProcessTimerStatus::Cancelled);
    assert_eq!(actual.subscriptions.iter().find(|row| row.subscription_id == subscription.subscription_id)
        .unwrap().status, ProcessSubscriptionStatus::Cancelled);
    let history = repository::list_events(&reopened, &fixture.owner, &waiting.instance_id, 0, 200).unwrap().0;
    assert_eq!(history.iter().filter(|event| event.kind == "timer_cancelled"
        && event.data["timer_id"] == timer.timer_id).count(), 1);
    assert_eq!(history.iter().filter(|event| event.kind == "subscription_cancelled"
        && event.data["subscription_id"] == subscription.subscription_id).count(), 1);
    assert_eq!(history.iter().find(|event| event.kind == "timer_cancelled"
        && event.data["timer_id"] == timer.timer_id).unwrap().data,
        plan.events[timer_index].data);
    assert_eq!(history.iter().find(|event| event.kind == "subscription_cancelled"
        && event.data["subscription_id"] == subscription.subscription_id).unwrap().data,
        plan.events[subscription_index].data);
    let sources = history.iter().filter(|event| event.kind == "terminate_end_reached")
        .collect::<Vec<_>>();
    assert_eq!(sources.len(), 2);
    assert_eq!(sources[0].scope_id, child_scope.scope_id);
    assert_eq!(sources[1].scope_id, waiting.instance_id);
    let committed_rows = super::call_tests::transition_rows(&fixture);
    assert_eq!(repository::complete_user_task(&reopened, &fixture.owner, &command,
        &waiting.instance_id, &task.user_task_id, waiting.revision, &outputs, None,
        &plan, at_ms).unwrap().instance, completed);
    assert_eq!(super::call_tests::transition_rows(&fixture), committed_rows);
}

#[test]
fn same_start_closes_real_boundary_catch_race_and_outbox_producers() {
    let fixture = Fixture::new();
    let receiver = publish_model(&fixture, &message_support::receiving_model(true, false));
    let foreign = start_model(&fixture, &runtime::test_support::user_model(Some(&fixture.owner.user_id)));
    let foreign_token = repository::runtime_snapshot(&fixture.db, &fixture.owner, &foreign.instance_id)
        .unwrap().tokens.into_iter().find(|token| token.status == "waiting").unwrap().token_id;
    let child = ProcessSubProcess {
        nodes: vec![
            ProcessNode { id: "ChildStart".into(), name: "Enter producer scope".into(),
                kind: ProcessNodeKind::Start },
            ProcessNode { id: "AttachedWork".into(), name: "Keep attached work open".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: Some(fixture.owner.user_id.clone()),
                    output_mapping: BTreeMap::new(),
                } },
            ProcessNode { id: "AttachedTimer".into(), name: "Arm attached timer".into(),
                kind: ProcessNodeKind::BoundaryTimer {
                    attached_to_id: "AttachedWork".into(), cancel_activity: true,
                    timer: ProcessTimerSpec::Duration { seconds: 600 },
                } },
            ProcessNode { id: "AttachedMessage".into(), name: "Arm attached message".into(),
                kind: ProcessNodeKind::BoundaryMessage {
                    attached_to_id: "AttachedWork".into(), cancel_activity: false,
                    message_ref: "ClosureMessage".into(),
                    correlation_expression: "vars.case_key".into(),
                    output_mapping: BTreeMap::new(),
                } },
            ProcessNode { id: "AttachedTimerWork".into(), name: "Timer continuation".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: Some(fixture.owner.user_id.clone()),
                    output_mapping: BTreeMap::new(),
                } },
            ProcessNode { id: "AttachedMessageWork".into(), name: "Message continuation".into(),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: Some(fixture.owner.user_id.clone()),
                    output_mapping: BTreeMap::new(),
                } },
            ProcessNode { id: "ChildEnd".into(), name: "Finish producer scope".into(),
                kind: ProcessNodeKind::End },
        ],
        sequence_flows: vec![
            edge("ChildEntry", "ChildStart", "AttachedWork"),
            edge("AttachedWorkEnd", "AttachedWork", "ChildEnd"),
            edge("TimerWork", "AttachedTimer", "AttachedTimerWork"),
            edge("MessageWork", "AttachedMessage", "AttachedMessageWork"),
            edge("TimerContinuationEnd", "AttachedTimerWork", "ChildEnd"),
            edge("MessageContinuationEnd", "AttachedMessageWork", "ChildEnd"),
        ],
        variables: BTreeMap::new(), diagram: ProcessDiagram::default(),
    };
    let mut model = starter_model();
    model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
        ProcessNodeKind::TerminateEnd;
    model.timer_timezone = Some("UTC".into());
    model.variables.insert("case_key".into(), json!("case-1"));
    model.variables.insert("opaque_business_key".into(), json!("opaque-73"));
    model.messages.push(ProcessMessageDeclaration {
        message_id: "ClosureMessage".into(), name: "EvidenceReady".into(),
    });
    model.nodes.extend([
        ProcessNode { id: "RootSplit".into(), name: "Arm all real producers".into(),
            kind: ProcessNodeKind::ParallelGateway },
        ProcessNode { id: "ProducerScope".into(), name: "Arm attached controls".into(),
            kind: ProcessNodeKind::SubProcess {
                body: child, input_mapping: BTreeMap::new(), output_mapping: BTreeMap::new(),
            } },
        ProcessNode { id: "RootTimer".into(), name: "Arm root timer".into(),
            kind: ProcessNodeKind::TimerCatch {
                timer: ProcessTimerSpec::Duration { seconds: 600 },
            } },
        ProcessNode { id: "RootMessage".into(), name: "Arm root message".into(),
            kind: ProcessNodeKind::MessageCatch {
                message_ref: "ClosureMessage".into(),
                correlation_expression: "vars.case_key".into(),
                output_mapping: BTreeMap::new(),
            } },
        ProcessNode { id: "RootRace".into(), name: "Arm root event race".into(),
            kind: ProcessNodeKind::EventBasedGateway },
        ProcessNode { id: "RaceTimer".into(), name: "Arm race timer".into(),
            kind: ProcessNodeKind::TimerCatch {
                timer: ProcessTimerSpec::Duration { seconds: 600 },
            } },
        ProcessNode { id: "RaceMessage".into(), name: "Arm race message".into(),
            kind: ProcessNodeKind::MessageCatch {
                message_ref: "ClosureMessage".into(),
                correlation_expression: "vars.case_key".into(),
                output_mapping: BTreeMap::new(),
            } },
        ProcessNode { id: "RootThrow".into(), name: "Queue a real addressed message".into(),
            kind: ProcessNodeKind::MessageThrow {
                message_ref: "ClosureMessage".into(),
                target: ProcessMessageTargetSpec::Start {
                    definition_id: receiver.definition_id.clone(),
                },
                correlation_expression: "vars.case_key".into(),
                payload_expression: "vars.opaque_business_key".into(),
                ttl_seconds: 300,
            } },
        ProcessNode { id: "ThrowHold".into(), name: "Retain the throw branch until closure".into(),
            kind: ProcessNodeKind::UserTask {
                assignee_user_id: Some(fixture.owner.user_id.clone()),
                output_mapping: BTreeMap::new(),
            } },
        ProcessNode { id: "SourceDelay".into(), name: "Complete a factual sibling scope".into(),
            kind: ProcessNodeKind::SubProcess {
                body: ProcessSubProcess {
                    nodes: vec![
                        ProcessNode { id: "DelayStart".into(), name: "Enter sibling".into(),
                            kind: ProcessNodeKind::Start },
                        ProcessNode { id: "DelayEnd".into(), name: "Finish sibling".into(),
                            kind: ProcessNodeKind::End },
                    ],
                    sequence_flows: vec![edge("DelayFlow", "DelayStart", "DelayEnd")],
                    variables: BTreeMap::new(), diagram: ProcessDiagram::default(),
                },
                input_mapping: BTreeMap::new(), output_mapping: BTreeMap::new(),
            } },
    ]);
    model.sequence_flows = vec![
        edge("RootEntry", "Start_1", "RootSplit"),
        edge("OpenProducerScope", "RootSplit", "ProducerScope"),
        edge("OpenRootTimer", "RootSplit", "RootTimer"),
        edge("OpenRootMessage", "RootSplit", "RootMessage"),
        edge("OpenRootRace", "RootSplit", "RootRace"),
        edge("QueueRootThrow", "RootSplit", "RootThrow"),
        edge("SourceToSibling", "RootSplit", "SourceDelay"),
        edge("SourceTerminate", "SourceDelay", "End_1"),
        edge("ScopeToTerminate", "ProducerScope", "End_1"),
        edge("TimerToTerminate", "RootTimer", "End_1"),
        edge("MessageToTerminate", "RootMessage", "End_1"),
        edge("RaceToTimer", "RootRace", "RaceTimer"),
        edge("RaceToMessage", "RootRace", "RaceMessage"),
        edge("RaceTimerTerminate", "RaceTimer", "End_1"),
        edge("RaceMessageTerminate", "RaceMessage", "End_1"),
        edge("ThrowToHold", "RootThrow", "ThrowHold"),
        edge("ThrowHoldTerminate", "ThrowHold", "End_1"),
    ];
    let version = publish_model(&fixture, &model);
    let instance_id = Uuid::new_v4().to_string();
    let variables = serde_json::to_value(&model.variables).unwrap();
    let command = stamp("start then close every actual same-plan producer");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&version.model, &instance_id, &fixture.owner,
        &version.definition_id, version.version, variables.clone(), StartCause::Manual,
        at_ms, runtime::test_support::manual_input(&command)).unwrap();
    assert_eq!(plan.termination_attempts.len(), 1);
    assert_eq!(plan.create_timers.len(), 3);
    assert_eq!(plan.create_subscriptions.len(), 3);
    assert_eq!(plan.create_event_races.len(), 1);
    assert_eq!(plan.create_user_tasks.len(), 2);
    assert_eq!(plan.create_messages.len(), 1);
    let race_timer = plan.create_timers.iter().find(|timer| timer.node_id == "RaceTimer").unwrap();
    let root_timer = plan.create_timers.iter().find(|timer| timer.node_id == "RootTimer").unwrap();
    let mut wrong_race = plan.clone();
    wrong_race.create_timers.iter_mut().find(|timer| timer.timer_id == race_timer.timer_id).unwrap()
        .race_id = Some(Uuid::new_v4().to_string());
    let mut wrong_scope = plan.clone();
    wrong_scope.create_timers.iter_mut().find(|timer| timer.timer_id == root_timer.timer_id).unwrap()
        .scope_id = Some(Uuid::new_v4().to_string());
    let mut missing_timer = plan.clone();
    missing_timer.create_timers.retain(|timer| timer.timer_id != race_timer.timer_id);
    let mut wrong_source = plan.clone();
    wrong_source.events.iter_mut().find(|event| event.kind == "timer_cancelled"
        && event.data["timer_id"] == race_timer.timer_id).unwrap().data["source_event_id"] =
        json!(Uuid::new_v4().to_string());
    let mut foreign_cancel = plan.clone();
    foreign_cancel.cancel_token_ids.push(foreign_token);
    let before = super::call_tests::transition_rows(&fixture);
    for (case, forged) in [
        ("wrong race", wrong_race), ("wrong scope", wrong_scope),
        ("missing race timer", missing_timer), ("wrong closure source", wrong_source),
        ("foreign activation cancellation", foreign_cancel),
    ] {
        assert!(repository::start_instance(&fixture.db, &fixture.owner, &command,
            &instance_id, &version.definition_id, version.version, &variables, &forged,
            at_ms).is_err(), "{case} must reject the entire source plan");
        assert_eq!(super::call_tests::transition_rows(&fixture), before,
            "{case} changed durable process rows");
    }
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let committed = repository::start_instance(&reopened, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, &plan,
        at_ms).unwrap();
    assert_eq!(committed.status, ProcessInstanceStatus::Completed);
    let actual = repository::runtime_snapshot(&reopened, &fixture.owner, &instance_id).unwrap();
    assert!(actual.timers.iter().all(|timer| timer.status == ProcessTimerStatus::Cancelled));
    assert!(actual.subscriptions.iter().all(|sub| sub.status == ProcessSubscriptionStatus::Cancelled));
    assert!(actual.event_races.iter().all(|race|
        race.status == tentaflow_protocol::processes::ProcessEventRaceStatus::Cancelled));
    assert!(actual.user_tasks.iter().all(|task| task.status == ProcessUserTaskStatus::Cancelled));
    let queued = actual.instance.outgoing_messages.iter().find(|message|
        message.source_node_id.as_deref() == Some("RootThrow")).unwrap();
    assert_eq!(queued.status, ProcessMessageStatus::Cancelled);
    let history = repository::list_events(&reopened, &fixture.owner, &instance_id, 0, 200).unwrap().0;
    let source = history.iter().find(|event| event.kind == "terminate_end_reached").unwrap();
    assert_eq!(history.iter().filter(|event| event.kind == "timer_cancelled"
        && event.data["reason"] == "terminate_end"
        && event.data["source_event_id"] == source.event_id).count(), 3);
    assert_eq!(history.iter().filter(|event| event.kind == "subscription_cancelled"
        && event.data["source_event_id"] == source.event_id).count(), 3);
    assert_eq!(history.iter().filter(|event| event.kind == "event_race_cancelled"
        && event.data["source_event_id"] == source.event_id).count(), 1);
    assert_eq!(history.iter().filter(|event| event.kind == "message_queued"
        && event.node_id.as_deref() == Some("RootThrow")).count(), 1);
    assert_eq!(history.iter().filter(|event| event.kind == "message_cancelled"
        && event.data["source_event_id"] == source.event_id).count(), 1);
    assert!(repository::list_instances(&reopened, &fixture.owner, Some(&receiver.definition_id),
        0, 10).unwrap().0.is_empty());
    let committed_rows = super::call_tests::transition_rows(&fixture);
    assert_eq!(repository::start_instance(&reopened, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, &plan,
        at_ms).unwrap(), committed);
    assert_eq!(super::call_tests::transition_rows(&fixture), committed_rows);
}

#[test]
fn terminating_parallel_sibling_rejects_an_invented_xor_failure_wait() {
    let make_model = |condition: &str| {
        let mut model = starter_model();
        model.nodes.iter_mut().find(|node| node.id == "End_1").unwrap().kind =
            ProcessNodeKind::TerminateEnd;
        model.nodes.extend([
            ProcessNode { id: "Split".into(), name: "Open two terminal paths".into(),
                kind: ProcessNodeKind::ParallelGateway },
            ProcessNode { id: "Choice".into(), name: "Select a terminal path".into(),
                kind: ProcessNodeKind::ExclusiveGateway { default_flow_id: None } },
            ProcessNode { id: "ChoiceTerminate".into(), name: "End selected path".into(),
                kind: ProcessNodeKind::TerminateEnd },
        ]);
        let mut selected = edge("ChoiceTrue", "Choice", "ChoiceTerminate");
        selected.condition = Some(condition.into());
        let mut other = edge("ChoiceOther", "Choice", "ChoiceTerminate");
        other.condition = Some("false".into());
        model.sequence_flows = vec![
            edge("StartSplit", "Start_1", "Split"),
            edge("SplitChoice", "Split", "Choice"),
            edge("SplitDirect", "Split", "End_1"),
            selected, other,
        ];
        model
    };

    let fixture = Fixture::new();
    let model = make_model("true");
    let version = publish_model(&fixture, &model);
    let instance_id = Uuid::new_v4().to_string();
    let variables = serde_json::to_value(&model.variables).unwrap();
    let command = stamp("start a factual XOR selection beside a terminating branch");
    let at_ms = chrono::Utc::now().timestamp_millis();
    let plan = runtime::plan_start(&version.model, &instance_id, &fixture.owner,
        &version.definition_id, version.version, variables.clone(), StartCause::Manual,
        at_ms, runtime::test_support::manual_input(&command)).unwrap();
    assert_eq!(plan.termination_attempts.len(), 1);
    let selected_index = plan.events.iter().position(|event|
        event.kind == "exclusive_selected" && event.node_id.as_deref() == Some("Choice")).unwrap();
    assert_eq!(plan.events[selected_index].data["sequence_flow_id"], "ChoiceTrue");
    assert!(!plan.event_ids.contains_key(&selected_index));
    let choice = plan.create_tokens.iter().find(|token| token.node_id == "Choice"
        && token.status == "ready" && plan.consume_token_ids.contains(&token.token_id)).unwrap();
    let successor = plan.create_tokens.iter().find(|token| token.node_id == "ChoiceTerminate"
        && token.arrival_edge_id.as_deref() == Some("ChoiceTrue")).unwrap();
    assert!(plan.cancel_token_ids.contains(&successor.token_id));
    let mut forged = plan.clone();
    let message = "exclusive gateway has no unique matching sequence flow";
    forged.events[selected_index].kind = "incident".into();
    forged.events[selected_index].data = json!({"code":"NO_MATCHING_FLOW","message":message});
    forged.event_sources.remove(&selected_index);
    forged.create_tokens.retain(|token| token.token_id != successor.token_id);
    forged.cancel_token_ids.retain(|id| id != &successor.token_id);
    forged.token_sources.remove(&successor.token_id);
    let mut waiting = choice.clone();
    waiting.token_id = Uuid::new_v4().to_string();
    waiting.status = "waiting".into();
    forged.token_sources.insert(waiting.token_id.clone(), choice.token_id.clone());
    forged.cancel_token_ids.push(waiting.token_id.clone());
    forged.create_tokens.push(waiting);
    let incident_id = Uuid::new_v4().to_string();
    forged.add_incidents.push(tentaflow_protocol::processes::ProcessIncident {
        incident_id: incident_id.clone(), scope_id: instance_id.clone(),
        node_id: Some("Choice".into()), node_name: Some("Select a terminal path".into()),
        job_id: None, code: "NO_MATCHING_FLOW".into(), message: message.into(), at_ms,
        can_retry: false,
    });
    forged.resolve_incident_ids.push(incident_id);
    assert_eq!(forged.events.len(), plan.events.len());
    let before = super::call_tests::transition_rows(&fixture);
    let error = repository::start_instance(&fixture.db, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, &forged, at_ms)
        .unwrap_err();
    assert!(format!("{error:#}").contains(
        "termination XOR has a selected pinned flow, not a failure wait"),
        "the invented fault was rejected for another reason: {error:#}");
    assert_eq!(super::call_tests::transition_rows(&fixture), before);
    let reopened = crate::db::init(&fixture.directory.path().join("processes.db")).unwrap();
    let completed = repository::start_instance(&reopened, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, &plan, at_ms).unwrap();
    assert_eq!(completed.status, ProcessInstanceStatus::Completed);
    let events = repository::list_events(&reopened, &fixture.owner, &instance_id, 0, 200).unwrap().0;
    assert_eq!(events.iter().filter(|event| event.kind == "exclusive_selected"
        && event.data["sequence_flow_id"] == "ChoiceTrue").count(), 1);
    assert_eq!(events.iter().filter(|event| event.kind == "terminate_end_reached").count(), 1);
    assert!(events.iter().all(|event| event.kind != "incident"));
    let committed_rows = super::call_tests::transition_rows(&fixture);
    let replay = repository::start_instance(&reopened, &fixture.owner, &command,
        &instance_id, &version.definition_id, version.version, &variables, &plan, at_ms).unwrap();
    assert_eq!(replay, completed);
    assert_eq!(super::call_tests::transition_rows(&fixture), committed_rows);

    let fault_fixture = Fixture::new();
    let mut fault_model = make_model("false");
    fault_model.nodes.push(ProcessNode {
        id: "HumanTerminate".into(), name: "Complete the other selected branch".into(),
        kind: ProcessNodeKind::UserTask {
            assignee_user_id: Some(fault_fixture.owner.user_id.clone()),
            output_mapping: BTreeMap::new(),
        },
    });
    fault_model.nodes.push(ProcessNode {
        id: "FaultGateHuman".into(), name: "Enter the factual XOR branch".into(),
        kind: ProcessNodeKind::UserTask {
            assignee_user_id: Some(fault_fixture.owner.user_id.clone()),
            output_mapping: BTreeMap::new(),
        },
    });
    fault_model.sequence_flows[1] = edge("SplitFaultGate", "Split", "FaultGateHuman");
    fault_model.sequence_flows[2] = edge("SplitHuman", "Split", "HumanTerminate");
    fault_model.sequence_flows.push(edge("FaultGateChoice", "FaultGateHuman", "Choice"));
    fault_model.sequence_flows.push(edge("HumanToTerminate", "HumanTerminate", "End_1"));
    let fault_version = publish_model(&fault_fixture, &fault_model);
    let fault_instance_id = Uuid::new_v4().to_string();
    let fault_variables = serde_json::to_value(&fault_model.variables).unwrap();
    let fault_command = stamp("start two factual human branches before evaluating the XOR");
    let fault_at_ms = chrono::Utc::now().timestamp_millis();
    let fault_plan = runtime::plan_start(&fault_version.model, &fault_instance_id,
        &fault_fixture.owner, &fault_version.definition_id, fault_version.version,
        fault_variables.clone(), StartCause::Manual, fault_at_ms,
        runtime::test_support::manual_input(&fault_command)).unwrap();
    assert!(fault_plan.termination_attempts.is_empty());
    assert!(fault_plan.add_incidents.is_empty());
    let fault_started = repository::start_instance(&fault_fixture.db, &fault_fixture.owner,
        &fault_command, &fault_instance_id, &fault_version.definition_id, fault_version.version,
        &fault_variables, &fault_plan, fault_at_ms).unwrap();
    assert_eq!(fault_started.status, ProcessInstanceStatus::Waiting);
    let fault_reopened = crate::db::init(&fault_fixture.directory.path().join("processes.db")).unwrap();
    let start_snapshot = repository::runtime_snapshot(&fault_reopened, &fault_fixture.owner,
        &fault_instance_id).unwrap();
    assert_eq!(start_snapshot.user_tasks.iter().filter(|task|
        task.status == ProcessUserTaskStatus::Open).count(), 2);
    let fault_gate = start_snapshot.user_tasks.iter().find(|task|
        task.node_id == "FaultGateHuman" && task.status == ProcessUserTaskStatus::Open).unwrap();
    let fault_gate_command = stamp("complete the factual human gate into no-match XOR");
    let fault_gate_outputs = json!({});
    let fault_gate_at_ms = fault_at_ms + 1;
    let fault_gate_plan = runtime::plan_user_completion(&start_snapshot, &fault_gate.user_task_id,
        &fault_gate_outputs, None, fault_gate_at_ms,
        runtime::test_support::human_input(&start_snapshot, &fault_gate.user_task_id,
            &fault_gate_command)).unwrap();
    assert!(fault_gate_plan.termination_attempts.is_empty());
    assert!(fault_gate_plan.events.iter().any(|event| event.kind == "incident"
        && event.node_id.as_deref() == Some("Choice")
        && event.data["code"] == "NO_MATCHING_FLOW"));
    assert!(fault_gate_plan.events.iter().all(|event| event.kind != "exclusive_selected"));
    let factual_wait = fault_gate_plan.create_tokens.iter().find(|token| token.node_id == "Choice"
        && token.status == "waiting" && !fault_gate_plan.cancel_token_ids.contains(&token.token_id))
        .unwrap();
    assert_eq!(fault_gate_plan.add_incidents.len(), 1);
    assert!(!fault_gate_plan.add_incidents[0].can_retry);
    assert!(!fault_gate_plan.resolve_incident_ids.contains(&fault_gate_plan.add_incidents[0].incident_id));
    let fault_incident = repository::complete_user_task(&fault_reopened, &fault_fixture.owner,
        &fault_gate_command, &fault_instance_id, &fault_gate.user_task_id,
        start_snapshot.instance.revision, &fault_gate_outputs, None, &fault_gate_plan,
        fault_gate_at_ms).unwrap().instance;
    assert_eq!(fault_incident.status, ProcessInstanceStatus::Incident);
    let persisted_wait_status: String = fault_reopened.read().unwrap().query_row(
        "SELECT status FROM bpmn_tokens WHERE instance_id=?1 AND token_id=?2",
        rusqlite::params![fault_instance_id, factual_wait.token_id], |row| row.get(0)).unwrap();
    assert_eq!(persisted_wait_status, "waiting");
    let initial_resolved_at_ms: Option<i64> = fault_reopened.read().unwrap().query_row(
        "SELECT resolved_at_ms FROM bpmn_incidents WHERE instance_id=?1 AND incident_id=?2",
        rusqlite::params![fault_instance_id, fault_gate_plan.add_incidents[0].incident_id],
        |row| row.get(0)).unwrap();
    assert!(initial_resolved_at_ms.is_none());
    let fault_snapshot = repository::runtime_snapshot(&fault_reopened, &fault_fixture.owner,
        &fault_instance_id).unwrap();
    let human = fault_snapshot.user_tasks.iter().find(|task|
        task.node_id == "HumanTerminate" && task.status == ProcessUserTaskStatus::Open).unwrap();
    let completion_command = stamp("complete actual human work after the factual XOR incident");
    let completion_outputs = json!({});
    let completion_at_ms = fault_at_ms + 2;
    let completion_plan = runtime::plan_user_completion(&fault_snapshot, &human.user_task_id,
        &completion_outputs, None, completion_at_ms,
        runtime::test_support::human_input(&fault_snapshot, &human.user_task_id,
            &completion_command)).unwrap();
    assert_eq!(completion_plan.termination_attempts.len(), 1);
    assert!(completion_plan.cancel_token_ids.contains(&factual_wait.token_id));
    assert!(completion_plan.resolve_incident_ids.contains(&fault_gate_plan.add_incidents[0].incident_id));
    let fault_completed = repository::complete_user_task(&fault_reopened, &fault_fixture.owner,
        &completion_command, &fault_instance_id, &human.user_task_id,
        fault_snapshot.instance.revision, &completion_outputs, None, &completion_plan,
        completion_at_ms).unwrap().instance;
    assert_eq!(fault_completed.status, ProcessInstanceStatus::Completed);
    let cancelled_wait_status: String = fault_reopened.read().unwrap().query_row(
        "SELECT status FROM bpmn_tokens WHERE instance_id=?1 AND token_id=?2",
        rusqlite::params![fault_instance_id, factual_wait.token_id], |row| row.get(0)).unwrap();
    assert_eq!(cancelled_wait_status, "cancelled");
    let resolved_at_ms: Option<i64> = fault_reopened.read().unwrap().query_row(
        "SELECT resolved_at_ms FROM bpmn_incidents WHERE instance_id=?1 AND incident_id=?2",
        rusqlite::params![fault_instance_id, fault_gate_plan.add_incidents[0].incident_id],
        |row| row.get(0)).unwrap();
    assert!(resolved_at_ms.is_some());
    let fault_events = repository::list_events(&fault_reopened, &fault_fixture.owner,
        &fault_instance_id, 0, 200).unwrap().0;
    let fault_index = fault_events.iter().position(|event| event.kind == "incident"
        && event.node_id.as_deref() == Some("Choice")).unwrap();
    let terminal_index = fault_events.iter().position(|event| event.kind == "terminate_end_reached")
        .unwrap();
    assert!(fault_index < terminal_index);
    let fault_rows = super::call_tests::transition_rows(&fault_fixture);
    let fault_gate_replay = repository::complete_user_task(&fault_reopened, &fault_fixture.owner,
        &fault_gate_command, &fault_instance_id, &fault_gate.user_task_id,
        start_snapshot.instance.revision, &fault_gate_outputs, None, &fault_gate_plan,
        fault_gate_at_ms).unwrap().instance;
    assert_eq!(fault_gate_replay.status, ProcessInstanceStatus::Completed);
    assert_eq!(super::call_tests::transition_rows(&fault_fixture), fault_rows);
    let fault_replay = repository::complete_user_task(&fault_reopened, &fault_fixture.owner,
        &completion_command, &fault_instance_id, &human.user_task_id,
        fault_snapshot.instance.revision, &completion_outputs, None, &completion_plan,
        completion_at_ms).unwrap().instance;
    assert_eq!(fault_replay, fault_completed);
    assert_eq!(super::call_tests::transition_rows(&fault_fixture), fault_rows);
}
