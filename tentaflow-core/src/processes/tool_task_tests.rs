// ===== File: tool_task_tests.rs — Service tasks that run an addon tool =====

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tentaflow_protocol::processes::{
    ActivityOutcome, ActivityVerification, ProcessInstanceStatus, ProcessModel, ProcessNodeKind,
    ServiceImplementation,
};
use tokio_util::sync::CancellationToken;

use super::repository::{self, ActivityResultOrigin, PinnedServiceSnapshot};
use super::runtime::test_support::*;
use super::tool_host::test_support::ScriptedToolHost;
use super::tool_host::ToolAccessError;

const ADDON: &str = "notes-1a2b3c4d";
const TOOL: &str = "search";

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn tool_model(timeout_seconds: u32, result_expression: Option<&str>) -> ProcessModel {
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
        input_mapping: BTreeMap::from([("query".into(), "\"cats\"".into())]),
        output_mapping: BTreeMap::from([("answer".into(), "outputs.result.hits".into())]),
        verification: ActivityVerification::Condition {
            expression: "true".into(),
        },
        timeout_seconds,
        result_expression: result_expression.map(str::to_owned),
        implementation: ServiceImplementation::AddonTool {
            addon_id: ADDON.into(),
            tool_name: TOOL.into(),
        },
    };
    model
}

fn fixture_with(host: &Arc<ScriptedToolHost>) -> Fixture {
    let fixture = Fixture::new();
    fixture.dispatcher().set_process_tool_host(host.clone());
    // The database side of the pin: the enabled addon row and its approved bundle.
    fixture
        .db
        .write()
        .unwrap()
        .execute(
            "INSERT INTO addons(addon_id,name,version,installed_bundle_hash) VALUES(?1,'Notes','1',?2)",
            rusqlite::params![ADDON, "b".repeat(64)],
        )
        .unwrap();
    fixture
}

fn job_of(fixture: &Fixture, instance_id: &str, job_id: &str) -> repository::ProcessJob {
    repository::runtime_snapshot(&fixture.db, &fixture.owner, instance_id)
        .unwrap()
        .jobs
        .into_iter()
        .find(|job| job.job_id == job_id)
        .expect("job is part of the instance")
}

async fn run(fixture: &Fixture, worker: &str) -> repository::ClaimedProcessJob {
    let claimed = repository::claim_job(&fixture.db, worker, now_ms())
        .unwrap()
        .expect("claim the queued tool task");
    super::jobs::execute_claimed(
        &fixture.db,
        fixture.dispatcher(),
        worker,
        claimed.clone(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    claimed
}

fn invocation(fixture: &Fixture, job_id: &str) -> (String, String, String) {
    fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT implementation_kind,flow_id,phase FROM bpmn_service_invocations WHERE job_id=?1",
            [job_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap()
}

#[tokio::test]
async fn tool_result_flows_through_the_shared_machinery_as_an_envelope_result() {
    let host = ScriptedToolHost::new(json!({"hits": 3}));
    let fixture = fixture_with(&host);
    let started = start_model(&fixture, &tool_model(10, None));
    let claimed = run(&fixture, "tool-worker").await;

    let finished =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert_eq!(finished.status, ProcessInstanceStatus::Completed);
    assert_eq!(finished.variables["answer"], 3);
    let job = job_of(&fixture, &started.instance_id, &claimed.job.job_id);
    assert_eq!(job.result_origin, Some(ActivityResultOrigin::Envelope));
    let result = job.result.unwrap();
    assert_eq!(result.outcome, ActivityOutcome::Completed);
    assert_eq!(result.outputs, json!({"result": {"hits": 3}}));
    assert_eq!(
        invocation(&fixture, &claimed.job.job_id),
        (
            "addon_tool".into(),
            format!("tool:{ADDON}/{TOOL}"),
            "accepted".into()
        )
    );
    let calls = host.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0],
        (
            ADDON.to_owned(),
            TOOL.to_owned(),
            json!({"query": "cats"}),
            fixture.owner.user_id.clone(),
            claimed.request_id.clone(),
        )
    );
}

#[tokio::test]
async fn result_expression_makes_a_tool_result_a_contract_result() {
    let host = ScriptedToolHost::new(json!({
        "outcome": "Completed", "code": null, "summary": "searched",
        "outputs": {"hits": 0}, "evidence": []
    }));
    let fixture = fixture_with(&host);
    let started = start_model(&fixture, &tool_model(10, Some("outputs.result")));
    let claimed = run(&fixture, "contract-worker").await;
    let job = job_of(&fixture, &started.instance_id, &claimed.job.job_id);
    assert_eq!(job.result_origin, Some(ActivityResultOrigin::Contract));
    assert_eq!(job.result.unwrap().summary, "searched");
}

#[tokio::test]
async fn tool_error_is_a_platform_error_result_not_a_job_failure() {
    let host = ScriptedToolHost::new(json!(null));
    *host.reply.lock().unwrap() = Err("index unavailable".into());
    let fixture = fixture_with(&host);
    let started = start_model(&fixture, &tool_model(10, None));
    let claimed = run(&fixture, "error-worker").await;
    let job = job_of(&fixture, &started.instance_id, &claimed.job.job_id);
    assert_eq!(job.result_origin, Some(ActivityResultOrigin::Platform));
    let result = job.result.unwrap();
    assert_eq!(result.outcome, ActivityOutcome::Error);
    assert_eq!(result.code.as_deref(), Some("TOOL_ERROR"));
    assert!(result.summary.contains("index unavailable"));
}

#[tokio::test]
async fn denied_permission_at_claim_becomes_human_work_without_calling_the_tool() {
    let host = ScriptedToolHost::new(json!({"hits": 1}));
    let fixture = fixture_with(&host);
    let started = start_model(&fixture, &tool_model(10, None));
    *host.allow.lock().unwrap() = Err(ToolAccessError::PermissionDenied);
    let claimed = run(&fixture, "denied-worker").await;
    assert!(host.calls.lock().unwrap().is_empty());
    let job = job_of(&fixture, &started.instance_id, &claimed.job.job_id);
    let result = job.result.unwrap();
    assert_eq!(result.outcome, ActivityOutcome::NeedsHuman);
    assert_eq!(result.code.as_deref(), Some("TOOL_PERMISSION_DENIED"));
    let snapshot =
        repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id).unwrap();
    assert!(!snapshot.user_tasks.is_empty(), "a person must decide");
}

#[tokio::test]
async fn a_vanished_tool_fails_the_claim_as_revoked_source() {
    let host = ScriptedToolHost::new(json!({"hits": 1}));
    let fixture = fixture_with(&host);
    let started = start_model(&fixture, &tool_model(10, None));
    *host.allow.lock().unwrap() = Err(ToolAccessError::Unavailable);
    run(&fixture, "gone-worker").await;
    assert!(host.calls.lock().unwrap().is_empty());
    let current =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert_eq!(current.status, ProcessInstanceStatus::Incident);
    assert_eq!(current.incidents[0].code, "SOURCE_ACCESS_REVOKED");
}

#[tokio::test]
async fn missing_required_argument_fails_before_the_dispatch_boundary() {
    let mut host = ScriptedToolHost::new(json!({"hits": 1}));
    Arc::get_mut(&mut host).unwrap().schema =
        json!({"type": "object", "required": ["query", "limit"]});
    let fixture = fixture_with(&host);
    let started = start_model(&fixture, &tool_model(10, None));
    let claimed = run(&fixture, "arguments-worker").await;
    assert!(host.calls.lock().unwrap().is_empty());
    let current =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert_eq!(current.incidents[0].code, "TOOL_ARGUMENTS_INVALID");
    assert_eq!(
        invocation(&fixture, &claimed.job.job_id).2,
        "proved_no_effect"
    );
}

#[tokio::test]
async fn permission_revoked_during_the_call_never_publishes_the_result() {
    let mut host = ScriptedToolHost::new(json!({"hits": 9}));
    Arc::get_mut(&mut host).unwrap().revoke_during_call = true;
    let fixture = fixture_with(&host);
    let started = start_model(&fixture, &tool_model(10, None));
    let claimed = run(&fixture, "revoked-worker").await;
    let current =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert!(current.variables.get("answer").is_none());
    // The result stays recorded but unapplied; recovery decides later.
    assert_eq!(invocation(&fixture, &claimed.job.job_id).2, "observed");
    assert_ne!(current.status, ProcessInstanceStatus::Completed);
}

#[tokio::test]
async fn failing_call_after_revocation_is_reported_as_revoked_and_uncertain() {
    let mut host = ScriptedToolHost::new(json!(null));
    {
        let host = Arc::get_mut(&mut host).unwrap();
        host.revoke_during_call = true;
        *host.reply.lock().unwrap() = Err("denied".into());
    }
    let fixture = fixture_with(&host);
    let started = start_model(&fixture, &tool_model(10, None));
    let claimed = run(&fixture, "revoked-error-worker").await;
    assert_eq!(invocation(&fixture, &claimed.job.job_id).2, "uncertain");
    let history =
        repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 100)
            .unwrap()
            .0;
    assert!(history
        .iter()
        .any(|event| event.kind == "incident" && event.data["reason"] == "SOURCE_ACCESS_REVOKED"));
}

#[tokio::test]
async fn timeout_abandons_the_wait_and_leaves_the_outcome_uncertain() {
    let host = ScriptedToolHost::new(json!({"hits": 1}));
    *host.delay.lock().unwrap() = Duration::from_millis(2500);
    let fixture = fixture_with(&host);
    let started = start_model(&fixture, &tool_model(1, None));
    let claimed = run(&fixture, "timeout-worker").await;
    assert_eq!(invocation(&fixture, &claimed.job.job_id).2, "uncertain");
    let history =
        repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 100)
            .unwrap()
            .0;
    assert!(history
        .iter()
        .any(|event| event.kind == "incident" && event.data["reason"] == "SERVICE_TIMEOUT"));
    assert!(history.iter().all(|event| event.kind != "service_result"));
}

#[tokio::test]
async fn cancelling_the_worker_leaves_the_outcome_uncertain() {
    let host = ScriptedToolHost::new(json!({"hits": 1}));
    *host.delay.lock().unwrap() = Duration::from_millis(1500);
    let fixture = fixture_with(&host);
    let started = start_model(&fixture, &tool_model(10, None));
    let claimed = repository::claim_job(&fixture.db, "cancel-worker", now_ms())
        .unwrap()
        .unwrap();
    let cancel = CancellationToken::new();
    let task = {
        let (db, dispatcher, claim, cancel) = (
            fixture.db.clone(),
            fixture.dispatcher().clone(),
            claimed.clone(),
            cancel.clone(),
        );
        tokio::spawn(async move {
            super::jobs::execute_claimed(&db, &dispatcher, "cancel-worker", claim, cancel).await
        })
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        while host.calls.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the tool call starts");
    cancel.cancel();
    task.await.unwrap().unwrap();
    assert_eq!(invocation(&fixture, &claimed.job.job_id).2, "uncertain");
    let current =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert_eq!(current.status, ProcessInstanceStatus::Incident);
}

#[tokio::test]
async fn restart_between_dispatch_and_result_never_calls_the_tool_again() {
    let host = ScriptedToolHost::new(json!({"hits": 1}));
    let fixture = fixture_with(&host);
    let started = start_model(&fixture, &tool_model(10, None));
    let claimed = repository::claim_job(&fixture.db, "dead-worker", now_ms())
        .unwrap()
        .unwrap();
    assert!(repository::commit_job_dispatch_boundary(
        &fixture.db,
        &claimed,
        "dead-worker",
        now_ms()
    )
    .unwrap());
    assert_eq!(
        repository::recover_jobs(&fixture.db, None, now_ms()).unwrap(),
        1
    );
    assert!(repository::claim_job(&fixture.db, "next-worker", now_ms())
        .unwrap()
        .is_none());
    assert!(host.calls.lock().unwrap().is_empty());
    let current =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert!(current
        .incidents
        .iter()
        .any(|incident| incident.code == "EXTERNAL_OUTCOME_UNCERTAIN"));
}

#[test]
fn tool_pin_is_canonical_and_bound_to_bundle_and_schema() {
    use super::tool_host::{tool_pin, ProcessToolDescriptor};
    let descriptor = |schema: Value, bundle: &str| ProcessToolDescriptor {
        input_schema: schema,
        bundle_hash: bundle.into(),
    };
    let (pin, sha) = tool_pin(ADDON, TOOL, &descriptor(json!({"b": 1, "a": 2}), "h1")).unwrap();
    let parsed: Value = serde_json::from_str(&pin).unwrap();
    assert_eq!(parsed["addon_id"], ADDON);
    assert_eq!(parsed["tool_name"], TOOL);
    assert_eq!(parsed["bundle_hash"], "h1");
    assert_eq!(parsed["input_schema_digest"].as_str().unwrap().len(), 64);
    assert_eq!(sha.len(), 64);
    // Key order of the schema does not matter; its content and the bundle do.
    let (_, same) = tool_pin(ADDON, TOOL, &descriptor(json!({"a": 2, "b": 1}), "h1")).unwrap();
    assert_eq!(sha, same);
    let (_, other_schema) = tool_pin(ADDON, TOOL, &descriptor(json!({"a": 3}), "h1")).unwrap();
    let (_, other_bundle) =
        tool_pin(ADDON, TOOL, &descriptor(json!({"b": 1, "a": 2}), "h2")).unwrap();
    assert!(sha != other_schema && sha != other_bundle);
}

#[test]
fn publication_pins_the_tool_and_rejects_a_pin_of_the_wrong_kind() {
    let host = ScriptedToolHost::new(json!({"hits": 1}));
    let fixture = fixture_with(&host);
    let model = tool_model(10, None);
    let version = publish_model(&fixture, &model);
    assert_eq!(version.service_flows.len(), 1);
    assert_eq!(
        version.service_flows[0].flow_id,
        format!("tool:{ADDON}/{TOOL}")
    );
    assert_eq!(version.service_flows[0].source_version, 0);

    let definition = repository::save_definition(
        &fixture.db,
        &fixture.owner,
        &stamp("save"),
        None,
        0,
        "Tool process",
        "",
        &model,
    )
    .unwrap();
    let pinned = fixture
        .dispatcher()
        .snapshot_process_tool(ADDON, TOOL, &fixture.owner.user_id, &fixture.owner.org_id)
        .unwrap();
    let wrong_kind = PinnedServiceSnapshot {
        info: tentaflow_protocol::processes::PinnedFlowInfo {
            node_id: "Service".into(),
            flow_id: pinned.flow_id,
            source_version: 0,
            graph_sha256: pinned.graph_sha256,
        },
        graph_json: pinned.graph_json,
        role: Default::default(),
        implementation_kind: tentaflow_protocol::processes::ServiceImplementationKind::Flow,
    };
    let error = repository::publish_definition(
        &fixture.db,
        &fixture.owner,
        &stamp("publish"),
        &definition.definition_id,
        definition.draft_revision,
        &[wrong_kind],
        None,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("does not match service task"));
}

#[test]
fn publication_refuses_a_tool_of_another_organization_or_without_permission() {
    let host = ScriptedToolHost::new(json!({"hits": 1}));
    let fixture = fixture_with(&host);
    let owner = &fixture.owner;
    // An initiator outside the addon's organization (org B pointing at org A's tool).
    assert_eq!(
        fixture
            .dispatcher()
            .snapshot_process_tool(ADDON, TOOL, &owner.user_id, "org-b")
            .unwrap_err(),
        ToolAccessError::Initiator
    );
    let mut foreign = ScriptedToolHost::new(json!(null));
    Arc::get_mut(&mut foreign).unwrap().org = "org-a".into();
    fixture.dispatcher().set_process_tool_host(foreign);
    assert_eq!(
        fixture
            .dispatcher()
            .authorize_process_tool(ADDON, TOOL, &owner.user_id, &owner.org_id)
            .unwrap_err(),
        ToolAccessError::OtherOrganization
    );
    *host.allow.lock().unwrap() = Err(ToolAccessError::PermissionDenied);
    fixture.dispatcher().set_process_tool_host(host);
    assert_eq!(
        fixture
            .dispatcher()
            .snapshot_process_tool(ADDON, TOOL, &owner.user_id, &owner.org_id)
            .unwrap_err(),
        ToolAccessError::PermissionDenied
    );
}

#[tokio::test]
async fn disabling_the_addon_before_the_claim_stops_the_task_before_any_call() {
    let host = ScriptedToolHost::new(json!({"hits": 1}));
    let fixture = fixture_with(&host);
    let started = start_model(&fixture, &tool_model(10, None));
    fixture
        .db
        .write()
        .unwrap()
        .execute("UPDATE addons SET is_enabled=0 WHERE addon_id=?1", [ADDON])
        .unwrap();
    assert!(
        repository::claim_job(&fixture.db, "disabled-worker", now_ms())
            .unwrap()
            .is_none()
    );
    assert!(host.calls.lock().unwrap().is_empty());
    let current =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert_eq!(current.incidents[0].code, "SOURCE_ACCESS_REVOKED");
}

fn with_implementation(flow_id: &str, implementation: ServiceImplementation) -> ProcessModel {
    let mut model = tool_model(10, None);
    let node = model
        .nodes
        .iter_mut()
        .find(|node| node.id == "Service")
        .unwrap();
    let ProcessNodeKind::ServiceTask {
        flow_id: slot,
        implementation: kind,
        ..
    } = &mut node.kind
    else {
        unreachable!()
    };
    *slot = flow_id.into();
    *kind = implementation;
    model
}

fn tool(addon: &str, name: &str) -> ServiceImplementation {
    ServiceImplementation::AddonTool {
        addon_id: addon.into(),
        tool_name: name.into(),
    }
}

#[test]
fn model_validation_ties_the_flow_id_to_the_implementation() {
    use super::model::validate_model;
    validate_model(&with_implementation("", tool(ADDON, TOOL))).unwrap();
    validate_model(&with_implementation("flow-1", ServiceImplementation::Flow)).unwrap();
    for (flow_id, implementation, reason) in [
        ("", ServiceImplementation::Flow, "lacks a flow"),
        ("flow-1", tool(ADDON, TOOL), "must not name a flow"),
        ("", tool("", TOOL), "invalid addon id"),
        ("", tool(ADDON, ""), "invalid tool name"),
        ("", tool("a/b", TOOL), "invalid addon id"),
        ("", tool(ADDON, "x:y"), "invalid tool name"),
        ("", tool(ADDON, &"t".repeat(129)), "invalid tool name"),
    ] {
        let error = validate_model(&with_implementation(flow_id, implementation)).unwrap_err();
        assert!(format!("{error:#}").contains(reason), "{reason}: {error:#}");
    }
}

#[test]
fn xml_round_trips_a_tool_task_and_keeps_flow_task_bytes() {
    use super::bpmn::{export_xml, import_xml};
    let tool_model = with_implementation("", tool(ADDON, TOOL));
    let xml = export_xml(&tool_model).unwrap();
    assert!(xml.contains(&format!(
        "implementation=\"tool\" addonId=\"{ADDON}\" toolName=\"{TOOL}\" timeoutSeconds=\"10\""
    )));
    assert!(!xml.contains("flowId"));
    assert_eq!(import_xml(&xml).0, Some(tool_model));

    let flow_model = with_implementation("flow-1", ServiceImplementation::Flow);
    let xml = export_xml(&flow_model).unwrap();
    assert!(xml.contains(
        "<tentaflow:service flowId=\"flow-1\" timeoutSeconds=\"10\" verification=\"condition\">"
    ));
    assert!(!xml.contains("implementation"));
    assert_eq!(import_xml(&xml).0, Some(flow_model));

    // A tool service may not also name a flow, and a flow service may not name a tool.
    let mixed = xml.replacen("flowId=\"flow-1\"", "flowId=\"flow-1\" addonId=\"a\"", 1);
    assert!(import_xml(&mixed).0.is_none());
    let both = export_xml(&with_implementation("", tool(ADDON, TOOL)))
        .unwrap()
        .replacen(
            "implementation=\"tool\"",
            "implementation=\"tool\" flowId=\"flow-1\"",
            1,
        );
    assert!(import_xml(&both).0.is_none());
    let unknown = export_xml(&with_implementation("", tool(ADDON, TOOL)))
        .unwrap()
        .replacen("implementation=\"tool\"", "implementation=\"agent\"", 1);
    assert!(import_xml(&unknown).0.is_none());
}

#[test]
fn legacy_pin_snapshots_keep_their_bytes() {
    let legacy = json!({
        "info": {"node_id": "Service", "flow_id": "flow-1", "source_version": 3,
                 "graph_sha256": "a".repeat(64)},
        "graph_json": "{}"
    });
    let parsed: PinnedServiceSnapshot = serde_json::from_value(legacy.clone()).unwrap();
    assert_eq!(parsed.role, repository::PinnedServiceRole::Implementation);
    assert!(parsed.implementation_kind.is_flow());
    assert_eq!(serde_json::to_value(&parsed).unwrap(), legacy);

    let mut pinned = parsed;
    pinned.implementation_kind =
        tentaflow_protocol::processes::ServiceImplementationKind::AddonTool;
    let value = serde_json::to_value(&pinned).unwrap();
    assert_eq!(value["implementation_kind"], "addon_tool");
    assert!(value.get("role").is_none());
}
