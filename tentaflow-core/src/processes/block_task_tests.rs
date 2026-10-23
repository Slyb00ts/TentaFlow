// ===== File: block_task_tests.rs — Service tasks that run one Flow Builder block =====

use std::collections::BTreeMap;

use serde_json::{json, Value};
use tentaflow_protocol::processes::{
    ActivityOutcome, ActivityVerification, ProcessInstanceStatus, ProcessModel, ProcessNodeKind,
    ServiceImplementation, ServiceImplementationKind,
};
use tokio_util::sync::CancellationToken;

use super::blocks::{block_pin, BlockAccessError};
use super::repository::{self, ActivityResultOrigin};
use super::runtime::test_support::*;
use super::tool_task_tests::{invocation, job_of, now_ms, run};

fn block(node_type: &str, config: Value) -> ServiceImplementation {
    ServiceImplementation::Block {
        node_type: node_type.into(),
        config,
    }
}

fn block_model(
    implementation: ServiceImplementation,
    payload: &str,
    result_expression: Option<&str>,
) -> ProcessModel {
    let mut model = service_model(
        "",
        ActivityVerification::Condition {
            expression: "true".into(),
        },
    );
    let node = model
        .nodes
        .iter_mut()
        .find(|node| node.id == "Service")
        .unwrap();
    node.kind = ProcessNodeKind::ServiceTask {
        flow_id: String::new(),
        input_mapping: BTreeMap::from([("payload".into(), payload.into())]),
        output_mapping: BTreeMap::from([("answer".into(), "outputs.payload".into())]),
        verification: ActivityVerification::Condition {
            expression: "true".into(),
        },
        timeout_seconds: 10,
        result_expression: result_expression.map(str::to_owned),
        implementation,
    };
    model
}

fn pii_model(payload: &str) -> ProcessModel {
    block_model(block("pii_filter", json!({})), payload, None)
}

fn execution_rows(fixture: &Fixture) -> i64 {
    fixture
        .db
        .read()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM flow_executions", [], |row| row.get(0))
        .unwrap()
}

fn deactivate(fixture: &Fixture, user_id: &str) {
    fixture
        .db
        .write()
        .unwrap()
        .execute(
            "UPDATE user_accounts SET is_active=0 WHERE id=?1",
            [user_id],
        )
        .unwrap();
}

#[tokio::test]
async fn block_result_flows_through_the_shared_machinery_as_an_envelope_result() {
    let fixture = Fixture::new();
    let started = start_model(&fixture, &pii_model("\"call me later\""));
    let claimed = run(&fixture, "block-worker").await;

    let finished =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert_eq!(finished.status, ProcessInstanceStatus::Completed);
    assert_eq!(finished.variables["answer"], "call me later");
    let job = job_of(&fixture, &started.instance_id, &claimed.job.job_id);
    assert_eq!(job.result_origin, Some(ActivityResultOrigin::Envelope));
    let result = job.result.unwrap();
    assert_eq!(result.outcome, ActivityOutcome::Completed);
    assert_eq!(result.outputs["payload"], "call me later");
    assert_eq!(
        invocation(&fixture, &claimed.job.job_id),
        ("block".into(), "block:pii_filter".into(), "accepted".into())
    );
    // A block has no `flows` row, so no execution record may be written for it.
    assert_eq!(execution_rows(&fixture), 0);
}

#[tokio::test]
async fn result_expression_makes_a_block_result_a_contract_result() {
    let fixture = Fixture::new();
    let contract = r#"{"outcome":"Completed","code":null,"summary":"filtered","outputs":{"text":outputs.payload},"evidence":[]}"#;
    let started = start_model(
        &fixture,
        &block_model(block("pii_filter", json!({})), "\"hello\"", Some(contract)),
    );
    let claimed = run(&fixture, "contract-worker").await;
    let job = job_of(&fixture, &started.instance_id, &claimed.job.job_id);
    assert_eq!(job.result_origin, Some(ActivityResultOrigin::Contract));
    assert_eq!(job.result.unwrap().outputs, json!({"text": "hello"}));
}

#[tokio::test]
async fn failing_block_leaves_an_uncertain_incident_and_no_business_result() {
    // No embeddings service exists in the fixture, so the block really fails.
    // Past the dispatch boundary an unconfirmed effect is `uncertain`, exactly
    // as for a failing flow; nothing a boundary event could catch is produced.
    let fixture = Fixture::new();
    let started = start_model(
        &fixture,
        &block_model(
            block("embeddings", json!({"model": "absent"})),
            "\"hello\"",
            None,
        ),
    );
    run(&fixture, "failing-worker").await;
    let current =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert_eq!(current.status, ProcessInstanceStatus::Incident);
    assert_eq!(current.incidents[0].code, "EXTERNAL_OUTCOME_UNCERTAIN");
    assert!(current.variables.get("answer").is_none());
}

#[tokio::test]
async fn non_text_payload_is_refused_before_the_dispatch_boundary() {
    let fixture = Fixture::new();
    let started = start_model(&fixture, &pii_model("7"));
    let claimed = run(&fixture, "payload-worker").await;
    let current =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert_eq!(current.incidents[0].code, "BLOCK_INPUT_INVALID");
    assert_eq!(
        invocation(&fixture, &claimed.job.job_id).2,
        "proved_no_effect"
    );
}

#[test]
fn publication_pins_the_one_block_graph_of_every_allowed_block() {
    let fixture = Fixture::new();
    for (node_type, config) in [
        ("llm", json!({"model": "m", "max_tokens": 32})),
        ("embeddings", json!({"model": "e"})),
        ("pii_filter", json!({})),
    ] {
        let model = block_model(block(node_type, config.clone()), "\"x\"", None);
        let snapshots = crate::dispatch::processes::pin_service_snapshots(
            fixture.dispatcher(),
            &fixture.owner,
            &model,
        )
        .unwrap_or_else(|error| panic!("{node_type} must pin: {}", error.message));
        let (graph, sha256) = block_pin(node_type, &config).unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].info.flow_id, format!("block:{node_type}"));
        assert_eq!(snapshots[0].info.source_version, 0);
        assert_eq!(snapshots[0].info.graph_sha256, sha256);
        assert_eq!(snapshots[0].graph_json, graph);
        assert_eq!(
            snapshots[0].implementation_kind,
            ServiceImplementationKind::Block
        );
    }
}

#[test]
fn the_published_version_stores_the_pin_beside_the_model() {
    let fixture = Fixture::new();
    let version = publish_model(&fixture, &pii_model("\"x\""));
    let stored: String = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT service_snapshots_json FROM bpmn_versions WHERE definition_id=?1 AND version=?2",
            rusqlite::params![version.definition_id, version.version],
            |row| row.get(0),
        )
        .unwrap();
    let pins: Vec<repository::PinnedServiceSnapshot> = serde_json::from_str(&stored).unwrap();
    assert_eq!(pins.len(), 1);
    assert_eq!(
        pins[0].graph_json,
        block_pin("pii_filter", &json!({})).unwrap().0
    );
    assert_eq!(pins[0].info.flow_id, "block:pii_filter");
}

#[test]
fn publication_refuses_a_pin_built_for_another_block_configuration() {
    let fixture = Fixture::new();
    let authored = block_model(block("llm", json!({"model": "authored"})), "\"x\"", None);
    let other = block_model(block("llm", json!({"model": "other"})), "\"x\"", None);
    let definition = repository::save_definition(
        &fixture.db,
        &fixture.owner,
        &stamp("create"),
        None,
        0,
        "Block process",
        "",
        &authored,
    )
    .unwrap();
    let forged = crate::dispatch::processes::pin_service_snapshots(
        fixture.dispatcher(),
        &fixture.owner,
        &other,
    )
    .unwrap();
    let error = repository::publish_definition(
        &fixture.db,
        &fixture.owner,
        &stamp("publish"),
        &definition.definition_id,
        definition.draft_revision,
        &forged,
        None,
    )
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("block snapshot does not match service task"),
        "{error:#}"
    );
}

#[test]
fn blocks_outside_the_allow_list_are_refused_when_saving_and_when_pinning() {
    let fixture = Fixture::new();
    for node_type in ["exec_command", "agent", "subflow", "stt", "bus_publish"] {
        let model = block_model(block(node_type, json!({})), "\"x\"", None);
        let saved = super::model::validate_model(&model).unwrap_err();
        assert!(
            format!("{saved:#}").contains("cannot be run as a service task"),
            "{node_type}: {saved:#}"
        );
        // Pinning does not trust that the model was validated.
        let pinned = crate::dispatch::processes::pin_service_snapshots(
            fixture.dispatcher(),
            &fixture.owner,
            &model,
        );
        assert!(pinned.is_err(), "{node_type} must not pin");
    }
    let tools = block_model(
        block("llm", json!({"model": "m", "tools": ["x"]})),
        "\"x\"",
        None,
    );
    assert!(super::model::validate_model(&tools).is_err());
}

#[test]
fn a_block_task_must_map_its_payload_and_name_no_flow() {
    let mut unmapped = pii_model("\"x\"");
    let ProcessNodeKind::ServiceTask { input_mapping, .. } = &mut unmapped.nodes[1].kind else {
        unreachable!()
    };
    input_mapping.clear();
    assert!(
        format!("{:#}", super::model::validate_model(&unmapped).unwrap_err())
            .contains("must map its payload")
    );

    let mut named = pii_model("\"x\"");
    let ProcessNodeKind::ServiceTask { flow_id, .. } = &mut named.nodes[1].kind else {
        unreachable!()
    };
    *flow_id = "flow-1".into();
    assert!(
        format!("{:#}", super::model::validate_model(&named).unwrap_err())
            .contains("must not name a flow")
    );

    let reserved = service_model("block:llm", ActivityVerification::Human);
    assert!(
        format!("{:#}", super::model::validate_model(&reserved).unwrap_err())
            .contains("reserved block: prefix")
    );
}

#[test]
fn every_refusal_to_pin_reads_the_same_whatever_the_reason() {
    let fixture = Fixture::new();
    let model = pii_model("\"x\"");
    let lost = actor(&fixture.db, "lost-initiator");
    deactivate(&fixture, &lost.user_id);
    let elsewhere = repository::ProcessActor {
        org_id: "organization-that-does-not-exist".into(),
        user_id: fixture.owner.user_id.clone(),
    };
    let messages = [lost, elsewhere].map(|actor| {
        let refused =
            crate::dispatch::processes::pin_service_snapshots(fixture.dispatcher(), &actor, &model)
                .unwrap_err();
        assert_eq!(
            refused.code,
            tentaflow_protocol::ProtocolErrorCode::PolicyDenied
        );
        refused.message
    });
    assert_eq!(messages[0], messages[1]);
}

#[test]
fn a_failed_read_is_unavailable_and_never_a_verdict_on_the_initiator() {
    let fixture = Fixture::new();
    let dispatcher = fixture.dispatcher();
    let owner = &fixture.owner;
    assert!(dispatcher
        .authorize_process_block("pii_filter", &json!({}), &owner.user_id, &owner.org_id)
        .is_ok());
    assert_eq!(
        dispatcher
            .authorize_process_block("exec_command", &json!({}), &owner.user_id, &owner.org_id)
            .err()
            .unwrap(),
        BlockAccessError::NotAllowed
    );
    assert_eq!(
        dispatcher
            .authorize_process_block("pii_filter", &json!({}), "no-such-user", &owner.org_id)
            .err()
            .unwrap(),
        BlockAccessError::Initiator
    );
    let original = corrupt_accounts(&fixture);
    assert_eq!(
        dispatcher
            .authorize_process_block("pii_filter", &json!({}), &owner.user_id, &owner.org_id)
            .err()
            .unwrap(),
        BlockAccessError::Unavailable
    );
    restore_accounts(&fixture, original);
    assert!(dispatcher
        .authorize_process_block("pii_filter", &json!({}), &owner.user_id, &owner.org_id)
        .is_ok());
}

/// Makes every `user_accounts` row unreadable (a column no longer decodes) and
/// returns what [`restore_accounts`] needs to undo it.
fn corrupt_accounts(fixture: &Fixture) -> Vec<(String, String)> {
    let original: Vec<(String, String)> = {
        let conn = fixture.db.read().unwrap();
        let mut statement = conn.prepare("SELECT id,email FROM user_accounts").unwrap();
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    fixture
        .db
        .write()
        .unwrap()
        .execute("UPDATE user_accounts SET email=X'00'", [])
        .unwrap();
    original
}

fn restore_accounts(fixture: &Fixture, original: Vec<(String, String)>) {
    let conn = fixture.db.write().unwrap();
    for (id, email) in original {
        conn.execute("UPDATE user_accounts SET email=?1 WHERE id=?2", [email, id])
            .unwrap();
    }
}

#[tokio::test]
async fn an_unreadable_account_defers_the_claim_instead_of_revoking_the_source() {
    let fixture = Fixture::new();
    let started = start_model(&fixture, &pii_model("\"x\""));
    let claimed = repository::claim_job(&fixture.db, "deferring-worker", now_ms())
        .unwrap()
        .expect("claim the queued block task");
    let original = corrupt_accounts(&fixture);
    super::jobs::execute_claimed(
        &fixture.db,
        fixture.dispatcher(),
        "deferring-worker",
        claimed.clone(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    restore_accounts(&fixture, original);
    let current =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert_ne!(current.status, ProcessInstanceStatus::Incident);
    assert!(current.incidents.is_empty());
    let job = job_of(&fixture, &started.instance_id, &claimed.job.job_id);
    assert_eq!(job.status, "queued");
    assert_eq!(invocation(&fixture, &claimed.job.job_id).2, "prepared");
    let history =
        repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
            .unwrap()
            .0;
    let deferred = history
        .iter()
        .find(|event| event.kind == "service_deferred")
        .unwrap();
    assert_eq!(deferred.data["code"], "BLOCK_UNAVAILABLE");
    // The account is readable again, so the same job now runs to completion.
    let retried = repository::claim_job(&fixture.db, "retry-worker", now_ms() + 60_000)
        .unwrap()
        .expect("deferred job is claimable after its backoff");
    super::jobs::execute_claimed(
        &fixture.db,
        fixture.dispatcher(),
        "retry-worker",
        retried,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None)
            .unwrap()
            .status,
        ProcessInstanceStatus::Completed
    );
}

#[tokio::test]
async fn a_lost_initiator_stops_the_block_before_it_runs() {
    let fixture = Fixture::new();
    let started = start_model(&fixture, &pii_model("\"x\""));
    let claimed = repository::claim_job(&fixture.db, "revoked-worker", now_ms())
        .unwrap()
        .expect("claim the queued block task");
    deactivate(&fixture, &fixture.owner.user_id);
    super::jobs::execute_claimed(
        &fixture.db,
        fixture.dispatcher(),
        "revoked-worker",
        claimed.clone(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let phase = invocation(&fixture, &claimed.job.job_id).2;
    assert_eq!(phase, "proved_no_effect");
    let incident_code: String = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT code FROM bpmn_incidents WHERE instance_id=?1",
            [&started.instance_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(incident_code, "SOURCE_ACCESS_REVOKED");
}

#[tokio::test]
async fn the_dispatcher_runs_only_the_exact_pin_of_an_allowed_block() {
    use crate::flow_engine::dispatcher::{DispatchError, PinnedFlowSnapshot};
    use crate::flow_engine::envelope::{FlowEnvelope, FlowValue};
    use sha2::{Digest, Sha256};

    let fixture = Fixture::new();
    let dispatcher = fixture.dispatcher();
    let owner = &fixture.owner;
    let (graph, sha256) = block_pin("pii_filter", &json!({})).unwrap();
    let genuine = PinnedFlowSnapshot {
        flow_id: "block:pii_filter".into(),
        source_version: 0,
        graph_json: graph.clone(),
        graph_sha256: sha256,
    };
    let meta = || {
        dispatcher
            .authorize_process_block("pii_filter", &json!({}), &owner.user_id, &owner.org_id)
            .unwrap()
    };
    let seed = || FlowEnvelope::with_payload(FlowValue::Text("hello".into()));

    let outcome = dispatcher
        .dispatch_pinned_synthetic(&genuine, seed(), meta())
        .await
        .unwrap();
    assert_eq!(outcome.final_envelope.payload.as_text(), Some("hello"));

    // A graph that names a block outside the allow-list, with a consistent hash.
    let swapped = graph.replace("\"type\":\"pii_filter\"", "\"type\":\"exec_command\"");
    assert_ne!(swapped, graph);
    let forged = PinnedFlowSnapshot {
        flow_id: "block:exec_command".into(),
        source_version: 0,
        graph_sha256: hex::encode(Sha256::digest(swapped.as_bytes())),
        graph_json: swapped,
    };
    assert!(matches!(
        dispatcher
            .dispatch_pinned_synthetic(&forged, seed(), meta())
            .await,
        Err(DispatchError::Denied { .. })
    ));
    // A graph that was edited without recomputing its hash.
    let tampered = PinnedFlowSnapshot {
        graph_sha256: "0".repeat(64),
        ..genuine.clone()
    };
    assert!(matches!(
        dispatcher
            .dispatch_pinned_synthetic(&tampered, seed(), meta())
            .await,
        Err(DispatchError::Denied { .. })
    ));
    // A reference that does not name the block the graph runs.
    let renamed = PinnedFlowSnapshot {
        flow_id: "block:llm".into(),
        ..genuine.clone()
    };
    assert!(matches!(
        dispatcher
            .dispatch_pinned_synthetic(&renamed, seed(), meta())
            .await,
        Err(DispatchError::Denied { .. })
    ));
    // A different person than the one the run was authorized for.
    let mut other = meta();
    other.actor_user_id = Some("someone-else".into());
    assert!(matches!(
        dispatcher
            .dispatch_pinned_synthetic(&genuine, seed(), other)
            .await,
        Err(DispatchError::Denied { .. })
    ));
}

#[test]
fn xml_round_trips_a_block_task_and_keeps_the_other_implementations_apart() {
    use super::bpmn::{export_xml, import_xml};
    let model = block_model(
        block(
            "llm",
            json!({"model": "m", "max_tokens": 64, "stop": ["\n"]}),
        ),
        "\"x\"",
        None,
    );
    let xml = export_xml(&model).unwrap();
    assert!(xml.contains("implementation=\"block\" nodeType=\"llm\" timeoutSeconds=\"10\""));
    assert!(xml.contains(
        "<tentaflow:blockConfig>{&quot;max_tokens&quot;:64,&quot;model&quot;:&quot;m&quot;,&quot;stop&quot;:[&quot;\\n&quot;]}</tentaflow:blockConfig>"
    ));
    assert!(!xml.contains("flowId"));
    assert_eq!(import_xml(&xml).0, Some(model));

    // A block service names neither a flow nor an addon tool, and the other
    // implementations name no block.
    for (replacement, reason) in [
        (
            "implementation=\"block\" flowId=\"flow-1\" nodeType=",
            "flow on a block",
        ),
        (
            "implementation=\"block\" addonId=\"a\" nodeType=",
            "addon on a block",
        ),
        (
            "implementation=\"block\" toolName=\"t\" nodeType=",
            "tool on a block",
        ),
    ] {
        let mixed = xml.replacen("implementation=\"block\" nodeType=", replacement, 1);
        assert!(import_xml(&mixed).0.is_none(), "{reason}");
    }
    let blockless = xml.replacen("nodeType=\"llm\" ", "", 1);
    assert!(import_xml(&blockless).0.is_none(), "a block needs its type");
    let not_json = xml.replacen("<tentaflow:blockConfig>", "<tentaflow:blockConfig>{", 1);
    assert!(
        import_xml(&not_json).0.is_none(),
        "configuration must be JSON"
    );
    let as_tool = xml.replacen("implementation=\"block\"", "implementation=\"tool\"", 1);
    assert!(import_xml(&as_tool).0.is_none(), "a tool carries no block");
    let flow_xml = export_xml(&service_model("flow-1", ActivityVerification::Human)).unwrap();
    let flow_with_block =
        flow_xml.replacen("flowId=\"flow-1\"", "flowId=\"flow-1\" nodeType=\"llm\"", 1);
    assert!(
        import_xml(&flow_with_block).0.is_none(),
        "a flow carries no block"
    );
}

#[test]
fn block_pin_snapshots_name_their_kind_and_old_pins_keep_their_bytes() {
    let (graph, sha256) = block_pin("pii_filter", &json!({})).unwrap();
    let pinned = repository::PinnedServiceSnapshot {
        info: tentaflow_protocol::processes::PinnedFlowInfo {
            node_id: "Service".into(),
            flow_id: "block:pii_filter".into(),
            source_version: 0,
            graph_sha256: sha256,
        },
        graph_json: graph,
        role: Default::default(),
        implementation_kind: ServiceImplementationKind::Block,
    };
    let value = serde_json::to_value(&pinned).unwrap();
    assert_eq!(value["implementation_kind"], "block");
    assert_eq!(
        serde_json::from_value::<repository::PinnedServiceSnapshot>(value)
            .unwrap()
            .implementation_kind,
        ServiceImplementationKind::Block
    );
    // The pins B3.1 published still read and write exactly as they did.
    let tool = json!({
        "info": {"node_id": "Service", "flow_id": "tool:a-1/t", "source_version": 0,
                 "graph_sha256": "a".repeat(64)},
        "graph_json": "{}", "implementation_kind": "addon_tool"
    });
    let parsed: repository::PinnedServiceSnapshot = serde_json::from_value(tool.clone()).unwrap();
    assert_eq!(
        parsed.implementation_kind,
        ServiceImplementationKind::AddonTool
    );
    assert_eq!(serde_json::to_value(&parsed).unwrap(), tool);
}

fn llm_task(model: &str) -> ProcessModel {
    block_model(block("llm", json!({ "model": model })), "\"hi\"", None)
}

fn ai_event_rows(fixture: &Fixture) -> i64 {
    fixture
        .db
        .read()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM compliance_ai_events", [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn deny_model(fixture: &Fixture, model: &str, user_id: &str) {
    crate::db::repository::resource_permissions::set(
        &fixture.db,
        "model",
        model,
        "user",
        user_id,
        "deny",
    )
    .unwrap();
}

#[test]
fn a_model_the_initiator_may_not_use_cannot_be_pinned() {
    let fixture = Fixture::new();
    deny_model(&fixture, "secret-model", &fixture.owner.user_id);
    let refused = crate::dispatch::processes::pin_service_snapshots(
        fixture.dispatcher(),
        &fixture.owner,
        &llm_task("secret-model"),
    )
    .unwrap_err();
    assert_eq!(
        refused.code,
        tentaflow_protocol::ProtocolErrorCode::PolicyDenied
    );
    // The refusal reads like every other one: it does not say the model exists.
    let missing_org = repository::ProcessActor {
        org_id: "organization-that-does-not-exist".into(),
        user_id: fixture.owner.user_id.clone(),
    };
    let other = crate::dispatch::processes::pin_service_snapshots(
        fixture.dispatcher(),
        &missing_org,
        &llm_task("anything"),
    )
    .unwrap_err();
    assert_eq!(refused.message, other.message);
    // Another person, and another model, are not affected.
    for (actor, model) in [
        (&fixture.participant, "secret-model"),
        (&fixture.owner, "open-model"),
    ] {
        crate::dispatch::processes::pin_service_snapshots(
            fixture.dispatcher(),
            actor,
            &llm_task(model),
        )
        .unwrap_or_else(|error| panic!("{model} must pin: {}", error.message));
    }
}

#[tokio::test]
async fn model_access_withdrawn_after_publication_stops_the_job_before_the_boundary() {
    let fixture = Fixture::new();
    let started = start_model(&fixture, &llm_task("secret-model"));
    deny_model(&fixture, "secret-model", &fixture.owner.user_id);
    let claimed = run(&fixture, "model-worker").await;
    let current =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert_eq!(current.incidents[0].code, "SOURCE_ACCESS_REVOKED");
    assert_eq!(
        invocation(&fixture, &claimed.job.job_id).2,
        "proved_no_effect"
    );
    assert_eq!(ai_event_rows(&fixture), 0, "no backend call was prepared");
}

#[tokio::test]
async fn a_spent_token_quota_stops_the_job_before_the_boundary() {
    let fixture = Fixture::new();
    let started = start_model(&fixture, &llm_task("quota-model"));
    crate::db::repository::create_token_quota(
        &fixture.db,
        &crate::db::models::NewTokenQuota {
            org_id: crate::services::org::DEFAULT_ORG_ID,
            scope_type: "org",
            subject_id: None,
            model_id: None,
            period: "daily",
            max_total_tokens: 0,
            is_active: true,
        },
    )
    .unwrap();
    let claimed = run(&fixture, "quota-worker").await;
    let current =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert_eq!(current.incidents[0].code, "TOKEN_QUOTA_EXCEEDED");
    assert_eq!(
        invocation(&fixture, &claimed.job.job_id).2,
        "proved_no_effect"
    );
    assert_eq!(ai_event_rows(&fixture), 0);
}

#[tokio::test]
async fn an_unreadable_token_quota_defers_the_job_instead_of_running_it() {
    let fixture = Fixture::new();
    let started = start_model(&fixture, &llm_task("quota-model"));
    crate::db::repository::create_token_quota(
        &fixture.db,
        &crate::db::models::NewTokenQuota {
            org_id: crate::services::org::DEFAULT_ORG_ID,
            scope_type: "org",
            subject_id: None,
            model_id: None,
            period: "daily",
            max_total_tokens: 1_000_000,
            is_active: true,
        },
    )
    .unwrap();
    fixture
        .db
        .write()
        .unwrap()
        .execute("UPDATE token_quota SET max_total_tokens = X'00'", [])
        .unwrap();
    let claimed = run(&fixture, "unreadable-quota-worker").await;
    let job = job_of(&fixture, &started.instance_id, &claimed.job.job_id);
    assert_eq!(job.status, "queued");
    assert_eq!(invocation(&fixture, &claimed.job.job_id).2, "prepared");
    assert_eq!(ai_event_rows(&fixture), 0);
    let current =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert!(current.incidents.is_empty());
    let history =
        repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
            .unwrap()
            .0;
    let deferred = history
        .iter()
        .find(|event| event.kind == "service_deferred")
        .expect("the job is deferred");
    assert_eq!(deferred.data["code"], "BLOCK_UNAVAILABLE");

    // Readable again: the same job now passes the quota check and is claimable.
    fixture
        .db
        .write()
        .unwrap()
        .execute("UPDATE token_quota SET max_total_tokens = 1000000", [])
        .unwrap();
    let retried = repository::claim_job(&fixture.db, "retry-worker", now_ms() + 60_000)
        .unwrap()
        .expect("deferred job is claimable after its backoff");
    super::jobs::execute_claimed(
        &fixture.db,
        fixture.dispatcher(),
        "retry-worker",
        retried,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let job = job_of(&fixture, &started.instance_id, &claimed.job.job_id);
    assert_ne!(job.status, "queued", "the quota is readable, so the job runs");
    assert_eq!(ai_event_rows(&fixture), 1);
}

#[tokio::test]
async fn an_unreadable_account_does_not_turn_an_authorized_run_into_an_uncertain_one() {
    use crate::flow_engine::dispatcher::PinnedFlowSnapshot;
    use crate::flow_engine::envelope::{FlowEnvelope, FlowValue};

    let fixture = Fixture::new();
    let dispatcher = fixture.dispatcher();
    let owner = &fixture.owner;
    let (graph, sha256) = block_pin("pii_filter", &json!({})).unwrap();
    let pinned = PinnedFlowSnapshot {
        flow_id: "block:pii_filter".into(),
        source_version: 0,
        graph_json: graph,
        graph_sha256: sha256,
    };
    let meta = dispatcher
        .authorize_process_block("pii_filter", &json!({}), &owner.user_id, &owner.org_id)
        .ok()
        .unwrap();
    let original = corrupt_accounts(&fixture);
    let outcome = dispatcher
        .dispatch_pinned_synthetic(
            &pinned,
            FlowEnvelope::with_payload(FlowValue::Text("hi".into())),
            meta,
        )
        .await;
    restore_accounts(&fixture, original);
    assert_eq!(
        outcome.unwrap().final_envelope.payload.as_text(),
        Some("hi")
    );
}

#[tokio::test]
async fn a_model_block_whose_pin_names_no_model_is_refused_not_given_a_fallback() {
    use crate::flow_engine::dispatcher::{DispatchError, PinnedFlowSnapshot};
    use crate::flow_engine::envelope::{FlowEnvelope, FlowValue};
    use sha2::{Digest, Sha256};

    let fixture = Fixture::new();
    let dispatcher = fixture.dispatcher();
    let owner = &fixture.owner;
    let (graph, _) = block_pin("llm", &json!({"model": "m"})).unwrap();
    for settings in [
        r#"{}"#,
        r#"{"model":""}"#,
        r#"{"model":"  "}"#,
        r#"{"model":7}"#,
    ] {
        let unnamed = graph.replace(r#"{"model":"m"}"#, settings);
        assert_ne!(unnamed, graph);
        let pinned = PinnedFlowSnapshot {
            flow_id: "block:llm".into(),
            source_version: 0,
            graph_sha256: hex::encode(Sha256::digest(unnamed.as_bytes())),
            graph_json: unnamed,
        };
        let config: Value = serde_json::from_str(settings).unwrap();
        assert_eq!(
            dispatcher
                .authorize_process_block("llm", &config, &owner.user_id, &owner.org_id)
                .err()
                .unwrap(),
            BlockAccessError::ModelDenied,
            "{settings}"
        );
        let meta = dispatcher
            .authorize_process_block("pii_filter", &json!({}), &owner.user_id, &owner.org_id)
            .ok()
            .unwrap();
        let outcome = dispatcher
            .dispatch_pinned_synthetic(
                &pinned,
                FlowEnvelope::with_payload(FlowValue::Text("hi".into())),
                meta,
            )
            .await;
        assert!(
            matches!(outcome, Err(DispatchError::Denied { .. })),
            "{settings}"
        );
    }
}
