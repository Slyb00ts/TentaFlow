// ============ File: call_pin_tests.rs — real publication pins and legacy CallActivity migration proofs ============

use super::{model, repository, runtime::test_support};
use rusqlite::{params, types::Value, Connection};
use std::collections::BTreeMap;
use tentaflow_protocol::processes::{
    ActivityVerification, HolidayPolicy, ProcessCallableReference, ProcessDefinition, ProcessModel,
    ProcessNode, ProcessNodeKind, ProcessSequenceFlow, ProcessVersion, ProcessWorkCalendar,
    WorkWindow,
};

fn call_node(id: &str, target: &ProcessVersion) -> ProcessNode {
    ProcessNode {
        id: id.into(),
        name: format!("Call {id}"),
        kind: ProcessNodeKind::CallActivity {
            called_definition_id: target.definition_id.clone(),
            called_version: target.version,
            called_element: ProcessCallableReference {
                namespace_uri: target
                    .model
                    .target_namespace
                    .clone()
                    .unwrap_or_else(|| "https://tentaflow.app/bpmn/1".into()),
                process_id: target.model.process_id.clone(),
            },
            input_mapping: BTreeMap::new(),
            output_mapping: BTreeMap::new(),
        },
    }
}

fn sequence_model(targets: &[ProcessVersion]) -> ProcessModel {
    let mut model = model::starter_model();
    model.sequence_flows.clear();
    let mut previous = model.nodes[0].id.clone();
    for (index, target) in targets.iter().enumerate() {
        let id = format!("Call_{index}");
        model
            .nodes
            .insert(model.nodes.len() - 1, call_node(&id, target));
        model.sequence_flows.push(ProcessSequenceFlow {
            id: format!("Flow_{index}"),
            source_id: previous,
            target_id: id.clone(),
            condition: None,
        });
        previous = id;
    }
    model.sequence_flows.push(ProcessSequenceFlow {
        id: "Flow_End".into(),
        source_id: previous,
        target_id: model.nodes.last().unwrap().id.clone(),
        condition: None,
    });
    model
}

fn save_draft(fixture: &test_support::Fixture, model: &ProcessModel) -> ProcessDefinition {
    repository::save_definition(
        &fixture.db,
        &fixture.owner,
        &test_support::stamp("save call draft"),
        None,
        0,
        "Caller",
        "",
        model,
    )
    .expect("save actual call draft")
}

fn publish_draft(
    fixture: &test_support::Fixture,
    draft: &ProcessDefinition,
) -> anyhow::Result<(ProcessDefinition, ProcessVersion)> {
    repository::publish_definition(
        &fixture.db,
        &fixture.owner,
        &test_support::stamp("publish call draft"),
        &draft.definition_id,
        draft.draft_revision,
        &[],
        None,
    )
}

#[test]
fn call_publication_pins_exact_target_model_service_and_calendar() {
    let fixture = test_support::Fixture::new();
    let flow_id = test_support::flow(
        &fixture.db,
        &fixture.owner,
        &test_support::graph("original", None),
    );
    let mut target_model = test_support::service_model(&flow_id, ActivityVerification::Human);
    target_model.timer_timezone = Some("UTC".into());
    target_model.work_calendar = Some(ProcessWorkCalendar {
        name: "Review office".into(),
        weekly_windows: vec![WorkWindow {
            weekday: 1,
            start_minute: 540,
            end_minute: 1020,
        }],
        manual_days_off: Vec::new(),
        holiday_policy: HolidayPolicy::None,
    });
    let target = test_support::publish_model(&fixture, &target_model);
    assert_eq!(target.service_flows.len(), 1);
    assert!(target.model.calendar_pin.is_some());

    let caller = test_support::publish_model(&fixture, &sequence_model(&[target.clone()]));
    assert_eq!(caller.call_activities.len(), 1);
    let pin = &caller.call_activities[0];
    assert_eq!(pin.called_definition_id, target.definition_id);
    assert_eq!(pin.called_version, target.version);
    assert_eq!(pin.model_sha256, target.model_sha256);
    let dependency: (String, u32, String) = fixture.db.read().unwrap().query_row(
        "SELECT called_definition_id,called_version,model_sha256 FROM bpmn_call_dependencies WHERE definition_id=?1 AND version=?2",
        params![caller.definition_id, caller.version],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).unwrap();
    assert_eq!(
        dependency,
        (
            target.definition_id.clone(),
            target.version,
            target.model_sha256.clone()
        )
    );

    test_support::update_flow(
        &fixture.db,
        &fixture.owner,
        &flow_id,
        &test_support::graph("edited", None),
    );
    let retained = repository::get_version(
        &fixture.db,
        &fixture.owner,
        &target.definition_id,
        target.version,
    )
    .unwrap();
    assert_eq!(
        retained, target,
        "published model, calendar and service flow snapshot remain pinned"
    );
    assert_eq!(
        repository::get_version(
            &fixture.db,
            &fixture.owner,
            &caller.definition_id,
            caller.version
        )
        .unwrap(),
        caller
    );
}

#[test]
fn version_keys_allow_finite_return_chain_and_reject_fourth_call_depth() {
    let fixture = test_support::Fixture::new();
    let a1 = test_support::publish_model(&fixture, &model::starter_model());
    let b1 = test_support::publish_model(&fixture, &sequence_model(&[a1.clone()]));
    let current_a = repository::get_definition(&fixture.db, &fixture.owner, &a1.definition_id)
        .unwrap()
        .0;
    let a2_model = sequence_model(&[b1.clone()]);
    let a2_draft = repository::save_definition(
        &fixture.db,
        &fixture.owner,
        &test_support::stamp("save A2"),
        Some(&a1.definition_id),
        current_a.draft_revision,
        &current_a.name,
        "",
        &a2_model,
    )
    .unwrap();
    let (_, a2) = publish_draft(&fixture, &a2_draft).unwrap();
    assert_eq!(a2.version, 2);
    assert_eq!(a2.definition_id, a1.definition_id);
    let c1 = test_support::publish_model(&fixture, &sequence_model(&[a2.clone()]));
    let dependencies: Vec<(String, u32)> = {
        let conn = fixture.db.read().unwrap();
        let mut query = conn.prepare("SELECT called_definition_id,called_version FROM bpmn_call_dependencies WHERE definition_id=?1 AND version=?2 ORDER BY called_definition_id,called_version").unwrap();
        query
            .query_map(params![c1.definition_id, c1.version], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    assert_eq!(dependencies.len(), 3);
    assert!(dependencies.contains(&(a1.definition_id.clone(), 1)));
    assert!(dependencies.contains(&(a1.definition_id.clone(), 2)));
    assert!(dependencies.contains(&(b1.definition_id.clone(), 1)));

    let deep = save_draft(&fixture, &sequence_model(&[c1]));
    let error = publish_draft(&fixture, &deep).unwrap_err();
    assert!(format!("{error:#}").contains("depth"));
    let versions: i64 = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bpmn_versions WHERE definition_id=?1",
            [&deep.definition_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        versions, 0,
        "failed publication leaves no partial version or call pins"
    );
}

#[test]
fn repeated_call_diamond_reads_each_distinct_published_version_once() {
    let fixture = test_support::Fixture::new();
    let leaf = test_support::publish_model(&fixture, &model::starter_model());
    let shared = test_support::publish_model(&fixture, &sequence_model(&[leaf]));
    let left = test_support::publish_model(&fixture, &sequence_model(&[shared.clone()]));
    let right = test_support::publish_model(&fixture, &sequence_model(&[shared]));
    let caller = save_draft(&fixture, &sequence_model(&[left, right]));
    repository::CALL_VERSION_READS.with(|count| count.set(0));
    let (_, version) = publish_draft(&fixture, &caller).unwrap();
    assert_eq!(version.call_activities.len(), 2);
    assert_eq!(
        repository::CALL_VERSION_READS.with(|count| count.get()),
        4,
        "each distinct published dependency is read once despite the diamond"
    );
    let dependencies: i64 = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bpmn_call_dependencies WHERE definition_id=?1 AND version=?2",
            params![version.definition_id, version.version],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(dependencies, 4);
}

#[test]
fn repeated_call_target_still_checks_every_edge_qname() {
    let fixture = test_support::Fixture::new();
    let target = test_support::publish_model(&fixture, &model::starter_model());
    let mut model = sequence_model(&[target.clone(), target]);
    let ProcessNodeKind::CallActivity { called_element, .. } = &mut model.nodes[2].kind else {
        panic!("second call is present");
    };
    called_element.process_id = "Wrong_Process".into();
    let caller = save_draft(&fixture, &model);
    let error = publish_draft(&fixture, &caller).unwrap_err();
    assert!(format!("{error:#}").contains("QName"));
    let conn = fixture.db.read().unwrap();
    assert!(table_rows(&conn, "bpmn_call_pins", "definition_id,version,node_id").is_empty());
    assert!(table_rows(
        &conn,
        "bpmn_call_dependencies",
        "definition_id,version,called_definition_id"
    )
    .is_empty());
}

#[test]
fn cached_short_diamond_path_cannot_hide_a_fourth_call_depth() {
    let fixture = test_support::Fixture::new();
    let leaf = test_support::publish_model(&fixture, &model::starter_model());
    let shared = test_support::publish_model(&fixture, &sequence_model(&[leaf]));
    let middle = test_support::publish_model(&fixture, &sequence_model(&[shared.clone()]));
    let deep = test_support::publish_model(&fixture, &sequence_model(&[middle]));
    let caller = save_draft(&fixture, &sequence_model(&[shared, deep]));
    let error = publish_draft(&fixture, &caller).unwrap_err();
    assert!(format!("{error:#}").contains("depth"));
    let conn = fixture.db.read().unwrap();
    let versions: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM bpmn_versions WHERE definition_id=?1",
            [&caller.definition_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(versions, 0);
    let caller_pins: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM bpmn_call_pins WHERE definition_id=?1",
            [&caller.definition_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(caller_pins, 0);
}

#[test]
fn call_publication_enforces_distinct_dependency_and_transitive_byte_limits() {
    let fixture = test_support::Fixture::new();
    let small = (0..33)
        .map(|_| test_support::publish_model(&fixture, &model::starter_model()))
        .collect::<Vec<_>>();
    let wide = save_draft(&fixture, &sequence_model(&small));
    assert!(format!("{:#}", publish_draft(&fixture, &wide).unwrap_err())
        .contains("32 distinct versions"));

    let large = (0..18)
        .map(|_| {
            let mut target = model::starter_model();
            target.variables.insert(
                "payload".into(),
                serde_json::Value::String("x".repeat(245_000)),
            );
            test_support::publish_model(&fixture, &target)
        })
        .collect::<Vec<_>>();
    let over_budget = save_draft(&fixture, &sequence_model(&large));
    assert!(format!("{:#}", publish_draft(&fixture, &over_budget).unwrap_err()).contains("4 MiB"));
    let partial: i64 = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bpmn_call_dependencies WHERE definition_id=?1",
            [&over_budget.definition_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        partial, 0,
        "transaction rollback removes earlier dependency writes"
    );
}

#[test]
fn archived_or_revoked_target_denies_current_call_admission_without_writing_a_version() {
    let fixture = test_support::Fixture::new();
    let target = test_support::publish_model(&fixture, &model::starter_model());
    let caller = save_draft(&fixture, &sequence_model(&[target.clone()]));
    let target_draft =
        repository::get_definition(&fixture.db, &fixture.owner, &target.definition_id)
            .unwrap()
            .0;
    repository::archive_definition(
        &fixture.db,
        &fixture.owner,
        &test_support::stamp("archive target"),
        &target.definition_id,
        target_draft.draft_revision,
        true,
    )
    .unwrap();
    assert!(publish_draft(&fixture, &caller).is_err());
    assert_eq!(
        repository::get_definition(&fixture.db, &fixture.owner, &caller.definition_id)
            .unwrap()
            .0,
        caller
    );

    let restored = repository::get_definition(&fixture.db, &fixture.owner, &target.definition_id)
        .unwrap()
        .0;
    repository::archive_definition(
        &fixture.db,
        &fixture.owner,
        &test_support::stamp("restore target"),
        &target.definition_id,
        restored.draft_revision,
        false,
    )
    .unwrap();
    assert!(crate::services::org::repo::remove_membership(
        &fixture.db,
        &fixture.owner.org_id,
        &fixture.owner.user_id
    )
    .unwrap());
    assert!(
        publish_draft(&fixture, &caller).is_err(),
        "current actor membership is rechecked inside publication"
    );
    let rows: i64 = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM bpmn_versions WHERE definition_id=?1",
            [&caller.definition_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0);
}

#[test]
fn call_publication_rechecks_actor_and_target_after_preflight_before_writer() {
    for mutation in 0..2 {
        let fixture = test_support::Fixture::new();
        let target = test_support::publish_model(&fixture, &model::starter_model());
        let caller = save_draft(&fixture, &sequence_model(&[target.clone()]));
        let attempt = test_support::stamp("publish after call authority change");
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(0);
        let (resume_tx, resume_rx) = std::sync::mpsc::sync_channel(0);
        let (result, before) = std::thread::scope(|scope| {
            let f = &fixture;
            let draft = &caller;
            let request = &attempt;
            let publishing = scope.spawn(move || {
                repository::PUBLICATION_PREFLIGHT
                    .with(|gate| *gate.borrow_mut() = Some((ready_tx, resume_rx)));
                let result = repository::publish_definition(
                    &f.db,
                    &f.owner,
                    request,
                    &draft.definition_id,
                    draft.draft_revision,
                    &[],
                    None,
                );
                repository::PUBLICATION_PREFLIGHT.with(|gate| *gate.borrow_mut() = None);
                result
            });
            ready_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("actual call publication reached preflight before writer");
            if mutation == 0 {
                assert!(crate::services::org::repo::remove_membership(
                    &fixture.db,
                    &fixture.owner.org_id,
                    &fixture.owner.user_id,
                )
                .unwrap());
            } else {
                let current =
                    repository::get_definition(&fixture.db, &fixture.owner, &target.definition_id)
                        .unwrap()
                        .0;
                repository::archive_definition(
                    &fixture.db,
                    &fixture.owner,
                    &test_support::stamp("archive prepared call target"),
                    &target.definition_id,
                    current.draft_revision,
                    true,
                )
                .unwrap();
            }
            let before = {
                let conn = fixture.db.read().unwrap();
                [
                    table_rows(&conn, "bpmn_definitions", "definition_id"),
                    table_rows(&conn, "bpmn_versions", "definition_id,version"),
                    table_rows(&conn, "bpmn_call_pins", "definition_id,version,node_id"),
                    table_rows(
                        &conn,
                        "bpmn_call_dependencies",
                        "definition_id,version,called_definition_id",
                    ),
                    table_rows(&conn, "bpmn_commands", "command_id"),
                    table_rows(&conn, "audit_log", "id"),
                ]
            };
            resume_tx.send(()).unwrap();
            (publishing.join().unwrap(), before)
        });
        assert!(
            result.is_err(),
            "current mutation {mutation} denies publication"
        );
        let after = {
            let conn = fixture.db.read().unwrap();
            [
                table_rows(&conn, "bpmn_definitions", "definition_id"),
                table_rows(&conn, "bpmn_versions", "definition_id,version"),
                table_rows(&conn, "bpmn_call_pins", "definition_id,version,node_id"),
                table_rows(
                    &conn,
                    "bpmn_call_dependencies",
                    "definition_id,version,called_definition_id",
                ),
                table_rows(&conn, "bpmn_commands", "command_id"),
                table_rows(&conn, "audit_log", "id"),
            ]
        };
        assert_eq!(
            after, before,
            "denied publication leaves no version, pin, command or audit"
        );
        let conn = fixture.db.read().unwrap();
        let caller_versions: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM bpmn_versions WHERE definition_id=?1",
                [&caller.definition_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(caller_versions, 0);
    }
}

#[test]
fn call_free_published_version_keeps_its_original_model_json_and_no_pin_rows() {
    let fixture = test_support::Fixture::new();
    let model = model::starter_model();
    let version = test_support::publish_model(&fixture, &model);
    assert!(version.call_activities.is_empty());
    let (stored, pin_count, dependency_count): (String, i64, i64) = fixture.db.read().unwrap().query_row(
        "SELECT model_json,(SELECT COUNT(*) FROM bpmn_call_pins WHERE definition_id=?1),(SELECT COUNT(*) FROM bpmn_call_dependencies WHERE definition_id=?1) FROM bpmn_versions WHERE definition_id=?1 AND version=?2",
        params![version.definition_id, version.version],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
    ).unwrap();
    assert_eq!(stored, serde_json::to_string(&model).unwrap());
    assert_eq!((pin_count, dependency_count), (0, 0));
}

pub(super) fn table_rows(conn: &Connection, table: &str, ordering: &str) -> Vec<Vec<Value>> {
    let mut query = conn
        .prepare(&format!("SELECT * FROM {table} ORDER BY {ordering}"))
        .unwrap();
    let columns = query.column_count();
    query
        .query_map([], |row| {
            (0..columns)
                .map(|index| row.get::<_, Value>(index))
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

fn legacy_database() -> (tempfile::TempDir, Connection, String, String) {
    let directory = tempfile::tempdir().unwrap();
    let conn = Connection::open(directory.path().join("process-v185.db")).unwrap();
    let (model_json, model_sha256) = crate::db::migrations::bpmn_boundary_migration_fixture(&conn);
    crate::db::migrations::run_ladder_up_to(&conn, 185);
    let payload = r#"{"customer_ID":"é"}"#;
    let payload_sha256 = repository::request_hash(&serde_json::json!({"customer_ID":"é"})).unwrap();
    let payload_bytes = i64::try_from(payload.len()).unwrap();
    for (id, origin, status, content, pruned, source, activation) in [
        (
            "api-message",
            "api",
            "pending",
            Some(payload),
            None,
            None,
            None,
        ),
        ("api-pruned", "api", "expired", None, Some(10), None, None),
        (
            "process-message",
            "process",
            "pending",
            Some(payload),
            None,
            Some("boundary-instance"),
            Some("activation-1"),
        ),
        (
            "process-pruned",
            "process",
            "cancelled",
            None,
            Some(10),
            Some("boundary-instance"),
            Some("activation-2"),
        ),
    ] {
        conn.execute("INSERT INTO bpmn_messages(org_id,sender_user_id,message_id,request_hash,origin,target_kind,definition_id,message_name,correlation_key,payload_json,payload_sha256,payload_bytes,ttl_seconds,received_at_ms,expires_at_ms,revision,status,next_check_at_ms,updated_at_ms,payload_pruned_at_ms,source_instance_id,source_scope_id,source_definition_id,source_version,source_node_id,source_activation_id,source_event_id) VALUES('org-default','boundary-owner',?1,?1,?2,'start','boundary-process','signal','customer_ID',?3,?4,?5,60,1,60001,1,?6,1,1,?7,?8,CASE WHEN ?2='process' THEN 'boundary-instance' ELSE NULL END,CASE WHEN ?2='process' THEN 'boundary-process' ELSE NULL END,CASE WHEN ?2='process' THEN 1 ELSE NULL END,CASE WHEN ?2='process' THEN 'Service_1' ELSE NULL END,?9,CASE WHEN ?2='process' THEN 'boundary-event' ELSE NULL END)",
            params![id,origin,content,&payload_sha256,payload_bytes,status,pruned,source,activation]).unwrap();
    }
    conn.execute("INSERT INTO bpmn_commands(org_id,actor_user_id,command_id,request_hash,result_json,created_at_ms) VALUES('org-default','boundary-owner','old-command','old-hash','{\"instance_id\":\"boundary-instance\",\"opaque_ID\":true}',1)", []).unwrap();
    (directory, conn, model_json, model_sha256)
}

#[test]
fn migration_preserves_populated_legacy_call_free_rows_and_foreign_keys() {
    let (_directory, conn, model_json, model_sha256) = legacy_database();
    let before = [
        table_rows(&conn, "bpmn_versions", "definition_id,version"),
        table_rows(&conn, "bpmn_events", "instance_id,seq"),
        table_rows(&conn, "bpmn_commands", "command_id"),
        table_rows(&conn, "bpmn_messages", "message_id"),
    ];
    crate::db::migrations::run(&conn).unwrap();
    assert_eq!(
        conn.query_row("SELECT MAX(version) FROM _migrations", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        187
    );
    let after = [
        table_rows(&conn, "bpmn_versions", "definition_id,version"),
        table_rows(&conn, "bpmn_events", "instance_id,seq"),
        table_rows(&conn, "bpmn_commands", "command_id"),
        table_rows(&conn, "bpmn_messages", "message_id"),
    ];
    assert_eq!(before, after);
    assert_eq!(
        after[3].len(),
        4,
        "API and process messages include retained and pruned payloads"
    );
    let retained: (String, String) = conn.query_row(
        "SELECT model_json,model_sha256 FROM bpmn_versions WHERE definition_id='boundary-process'", [],
        |row| Ok((row.get(0)?,row.get(1)?)),
    ).unwrap();
    assert_eq!(retained, (model_json, model_sha256));
    let roots: (i64, i64) = conn.query_row(
        "SELECT COUNT(*),COUNT(terminal_error_json) FROM bpmn_scopes WHERE instance_id='boundary-instance' AND scope_id='boundary-instance' AND parent_scope_id IS NULL",
        [], |row| Ok((row.get(0)?,row.get(1)?)),
    ).unwrap();
    assert_eq!(roots, (1, 0));
    assert_eq!(
        conn.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(table_rows(&conn, "bpmn_call_pins", "definition_id,version,node_id").is_empty());
    assert!(table_rows(
        &conn,
        "bpmn_call_dependencies",
        "definition_id,version,called_definition_id"
    )
    .is_empty());
}

#[test]
fn migration_rejects_malformed_legacy_reference_atomically_and_restores_foreign_keys() {
    let (_directory, conn, model_json, _) = legacy_database();
    let before = table_rows(&conn, "bpmn_messages", "message_id");
    conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    conn.execute("UPDATE bpmn_instances SET definition_id='missing-definition' WHERE instance_id='boundary-instance'", []).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    assert!(crate::db::migrations::run(&conn).is_err());
    assert_eq!(
        conn.query_row("SELECT MAX(version) FROM _migrations", [], |row| row
            .get::<_, i64>(0))
            .unwrap(),
        185
    );
    assert_eq!(table_rows(&conn, "bpmn_messages", "message_id"), before);
    assert_eq!(
        conn.query_row(
            "SELECT model_json FROM bpmn_versions WHERE definition_id='boundary-process'",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        model_json
    );
    assert_eq!(
        conn.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    let terminal_column: i64 = conn.query_row("SELECT COUNT(*) FROM pragma_table_info('bpmn_instances') WHERE name='terminal_error_json'", [], |row| row.get(0)).unwrap();
    assert_eq!(terminal_column, 0, "failed schema rebuild is rolled back");
}
