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
    // The revocation is honoured at once: the observed result is blocked, so a
    // later recovery has nothing left to apply.
    assert_eq!(
        invocation(&fixture, &claimed.job.job_id).2,
        "observed_blocked"
    );
    assert_ne!(current.status, ProcessInstanceStatus::Completed);
    repository::recover_jobs(
        &fixture.db,
        None,
        now_ms(),
        &super::runtime::tool_authority(fixture.dispatcher()),
    )
    .unwrap();
    let after =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert!(after.variables.get("answer").is_none());
    assert_ne!(after.status, ProcessInstanceStatus::Completed);
    assert_eq!(
        invocation(&fixture, &claimed.job.job_id).2,
        "observed_blocked"
    );
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
        repository::recover_jobs(&fixture.db, None, now_ms(), &repository::no_tool_authority)
            .unwrap(),
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

/// A tool result durably observed, then the worker dies before accepting it.
async fn observed_but_unaccepted(
    fixture: &Fixture,
    host: &Arc<ScriptedToolHost>,
) -> (
    tentaflow_protocol::processes::ProcessInstance,
    repository::ClaimedProcessJob,
) {
    let started = start_model(fixture, &tool_model(10, None));
    let claimed = repository::claim_job(&fixture.db, "dying-worker", now_ms())
        .unwrap()
        .unwrap();
    assert!(repository::commit_job_dispatch_boundary(
        &fixture.db,
        &claimed,
        "dying-worker",
        now_ms()
    )
    .unwrap());
    let observed = repository::ObservedActivityResult {
        result: tentaflow_protocol::processes::ActivityResult {
            outcome: ActivityOutcome::Completed,
            code: None,
            summary: "The addon tool returned a result".into(),
            outputs: json!({"result": {"hits": 7}}),
            evidence: Vec::new(),
        },
        origin: ActivityResultOrigin::Envelope,
        expression_observation: None,
    };
    assert_eq!(
        repository::record_job_observation(
            &fixture.db,
            &claimed,
            "dying-worker",
            &observed,
            now_ms()
        )
        .unwrap(),
        repository::JobObservationState::Observed
    );
    assert!(host.calls.lock().unwrap().is_empty());
    (started, claimed)
}

#[tokio::test]
async fn recovery_applies_an_observed_tool_result_only_while_the_initiator_may_still_call_the_tool()
{
    let host = ScriptedToolHost::new(json!(null));
    let fixture = fixture_with(&host);
    let (started, claimed) = observed_but_unaccepted(&fixture, &host).await;
    *host.allow.lock().unwrap() = Err(ToolAccessError::PermissionDenied);
    let authority = super::runtime::tool_authority(fixture.dispatcher());
    assert_eq!(
        repository::recover_jobs(&fixture.db, None, now_ms(), &authority).unwrap(),
        1
    );
    assert_eq!(
        invocation(&fixture, &claimed.job.job_id).2,
        "observed_blocked"
    );
    let blocked =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert_ne!(blocked.status, ProcessInstanceStatus::Completed);
    assert!(blocked.variables.get("answer").is_none());

    let allowed = ScriptedToolHost::new(json!(null));
    let fixture = fixture_with(&allowed);
    let (started, claimed) = observed_but_unaccepted(&fixture, &allowed).await;
    let authority = super::runtime::tool_authority(fixture.dispatcher());
    assert_eq!(
        repository::recover_jobs(&fixture.db, None, now_ms(), &authority).unwrap(),
        1
    );
    assert_eq!(invocation(&fixture, &claimed.job.job_id).2, "accepted");
    let done =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert_eq!(done.status, ProcessInstanceStatus::Completed);
    assert_eq!(done.variables["answer"], 7);
}

#[tokio::test]
async fn recovery_without_any_tool_authority_blocks_instead_of_applying() {
    let host = ScriptedToolHost::new(json!(null));
    let fixture = fixture_with(&host);
    let (_, claimed) = observed_but_unaccepted(&fixture, &host).await;
    assert_eq!(
        repository::recover_jobs(&fixture.db, None, now_ms(), &repository::no_tool_authority)
            .unwrap(),
        1
    );
    assert_eq!(
        invocation(&fixture, &claimed.job.job_id).2,
        "observed_blocked"
    );
}

#[tokio::test]
async fn publication_and_claim_deny_other_organizations_without_revealing_the_addon() {
    let host = ScriptedToolHost::new(json!(null));
    let fixture = fixture_with(&host);
    let org_b = crate::services::org::repo::create_organization(
        &fixture.db,
        "Org B",
        "org-b",
        None,
        None,
        None,
        None,
    )
    .unwrap();
    crate::services::org::repo::add_membership(
        &fixture.db,
        &org_b.org_id,
        &fixture.participant.user_id,
        "role-org-viewer",
        &fixture.participant.user_id,
    )
    .unwrap();
    let member_of_b = repository::ProcessActor {
        org_id: org_b.org_id.clone(),
        user_id: fixture.participant.user_id.clone(),
    };
    // The addon runs in the default organization (org A); an org B member names it.
    let denied = fixture
        .dispatcher()
        .snapshot_process_tool(ADDON, TOOL, &member_of_b.user_id, &member_of_b.org_id)
        .unwrap_err();
    assert_eq!(denied, ToolAccessError::OtherOrganization);
    // The publication text is the same as for an addon that does not exist.
    let mut gone = ScriptedToolHost::new(json!(null));
    {
        let gone = Arc::get_mut(&mut gone).unwrap();
        gone.org = org_b.org_id.clone();
        *gone.allow.get_mut().unwrap() = Err(ToolAccessError::Unavailable);
    }
    fixture.dispatcher().set_process_tool_host(gone);
    let missing = fixture
        .dispatcher()
        .snapshot_process_tool(ADDON, TOOL, &member_of_b.user_id, &member_of_b.org_id)
        .unwrap_err();
    assert_eq!(missing, ToolAccessError::Unavailable);
    assert_eq!(denied.to_string(), missing.to_string());
    fixture.dispatcher().set_process_tool_host(host);
    // And through the real handler function: a PolicyDenied with that text.
    let model = tool_model(10, None);
    let error = crate::dispatch::processes::pin_service_snapshots(
        fixture.dispatcher(),
        &member_of_b,
        &model,
    )
    .unwrap_err();
    assert_eq!(error.message, ToolAccessError::Unavailable.to_string());
}

#[tokio::test]
async fn abandoned_calls_hold_the_gate_until_the_guest_returns_and_refuse_new_ones() {
    let host = ScriptedToolHost::new(json!({"hits": 1}));
    *host.delay.lock().unwrap() = Duration::from_millis(5000);
    let fixture = fixture_with(&host);
    fixture.dispatcher().process_tool_gate().set_limit(2);
    let model = tool_model(1, None);
    let mut jobs = Vec::new();
    for _ in 0..3 {
        start_model(&fixture, &model);
    }
    for round in 0..3 {
        let claimed = run(&fixture, &format!("gate-worker-{round}")).await;
        jobs.push(claimed.job.job_id);
    }
    // Two calls were dispatched and abandoned by their timeout; the third was refused.
    assert_eq!(host.calls.lock().unwrap().len(), 2);
    assert_eq!(invocation(&fixture, &jobs[0]).2, "uncertain");
    assert_eq!(invocation(&fixture, &jobs[1]).2, "uncertain");
    assert_eq!(invocation(&fixture, &jobs[2]).2, "proved_no_effect");
    assert_eq!(fixture.dispatcher().process_tool_gate().in_flight(ADDON), 2);
    let refused = fixture
        .db
        .read()
        .unwrap()
        .query_row(
            "SELECT code FROM bpmn_incidents WHERE job_id=?1",
            [&jobs[2]],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    assert_eq!(refused, "TOOL_BUSY");
    // The permits come back only when the guest really returns.
    tokio::time::timeout(Duration::from_secs(10), async {
        while fixture.dispatcher().process_tool_gate().in_flight(ADDON) > 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("permits are released when the abandoned calls finish");
}

#[tokio::test]
async fn a_changed_tool_is_used_and_the_drift_is_recorded() {
    let host = ScriptedToolHost::new(json!({"hits": 4}));
    let fixture = fixture_with(&host);
    let started = start_model(&fixture, &tool_model(10, None));
    *host.bundle.lock().unwrap() = "c".repeat(64);
    let claimed = run(&fixture, "drift-worker").await;
    assert_eq!(
        host.calls.lock().unwrap().len(),
        1,
        "the changed tool still runs"
    );
    let history =
        repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 100)
            .unwrap()
            .0;
    let drift = history
        .iter()
        .find(|event| event.kind == "service_tool_drift")
        .expect("drift event");
    assert_eq!(drift.data["pinned_bundle_hash"], "b".repeat(64));
    assert_eq!(drift.data["current_bundle_hash"], "c".repeat(64));
    assert_eq!(drift.data["job_id"], claimed.job.job_id);
    assert_eq!(drift.data["fence"], claimed.job.fence);
    // Same schema, so its digest did not change.
    assert_eq!(
        drift.data["pinned_input_schema_digest"],
        drift.data["current_input_schema_digest"]
    );
    let finished =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert_eq!(finished.status, ProcessInstanceStatus::Completed);
}

#[tokio::test]
async fn an_unchanged_tool_records_no_drift_and_a_changed_schema_is_the_one_checked() {
    let host = ScriptedToolHost::new(json!({"hits": 4}));
    let fixture = fixture_with(&host);
    let started = start_model(&fixture, &tool_model(10, None));
    run(&fixture, "plain-worker").await;
    let history =
        repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 100)
            .unwrap()
            .0;
    assert!(history
        .iter()
        .all(|event| event.kind != "service_tool_drift"));

    // A schema that gained a required argument after publication is enforced.
    let published_with = ScriptedToolHost::new(json!({"hits": 4}));
    let fixture = fixture_with(&published_with);
    let started = start_model(&fixture, &tool_model(10, None));
    let mut stricter = ScriptedToolHost::new(json!({"hits": 4}));
    Arc::get_mut(&mut stricter).unwrap().schema =
        json!({"type": "object", "required": ["query", "limit"]});
    fixture.dispatcher().set_process_tool_host(stricter.clone());
    run(&fixture, "strict-worker").await;
    assert!(stricter.calls.lock().unwrap().is_empty());
    let current =
        repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None).unwrap();
    assert_eq!(current.incidents[0].code, "TOOL_ARGUMENTS_INVALID");
}

#[tokio::test]
async fn a_denial_is_recorded_atomically_with_its_boundary() {
    let host = ScriptedToolHost::new(json!({"hits": 1}));
    let fixture = fixture_with(&host);
    let started = start_model(&fixture, &tool_model(10, None));
    *host.allow.lock().unwrap() = Err(ToolAccessError::PermissionDenied);
    fixture
        .db
        .write()
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER refuse_observation BEFORE UPDATE ON bpmn_service_invocations \
             WHEN NEW.phase='observed' BEGIN SELECT RAISE(ABORT,'observation refused'); END;",
        )
        .unwrap();
    let claimed = repository::claim_job(&fixture.db, "atomic-worker", now_ms())
        .unwrap()
        .unwrap();
    let error = super::jobs::execute_claimed(
        &fixture.db,
        fixture.dispatcher(),
        "atomic-worker",
        claimed.clone(),
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("observation refused"),
        "{error:#}"
    );
    // The boundary was rolled back with the failed observation: a crash here
    // would leave a plain prepared job, not an "uncertain" external effect.
    assert_eq!(invocation(&fixture, &claimed.job.job_id).2, "prepared");
    assert!(host.calls.lock().unwrap().is_empty());
    let _ = started;
}

#[test]
fn model_validation_reserves_the_tool_prefix_for_tools() {
    let error = super::model::validate_model(&with_implementation(
        "tool:notes-1/search",
        ServiceImplementation::Flow,
    ))
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("reserved tool: prefix"),
        "{error:#}"
    );
}

#[test]
fn invocation_rows_cannot_pair_a_kind_with_the_wrong_reference() {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::db::migrations::run(&conn).unwrap();
    conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    let digest = "a".repeat(64);
    let insert = |id: &str, flow_id: &str, kind: &str| {
        conn.execute(
            "INSERT INTO bpmn_service_invocations(invocation_id,job_id,instance_id,scope_id,\
             node_id,token_id,definition_id,version,selected_process_id,whole_document_sha256,\
             flow_id,pinned_flow_digest,input_sha256,stable_request_id,phase,dispatch_evidence,\
             created_at_ms,updated_at_ms,implementation_kind) \
             VALUES(?1,?1,'i','s','n','t','d',1,'p',?4,?2,?4,?4,?1,'prepared','no_boundary',1,1,?3)",
            rusqlite::params![id, flow_id, kind, digest],
        )
    };
    insert("a", "flow-1", "flow").unwrap();
    insert("b", "tool:notes-1/search", "addon_tool").unwrap();
    assert!(insert("c", "flow-1", "addon_tool").is_err());
    assert!(insert("d", "tool:notes-1/search", "flow").is_err());
    assert!(insert("e", "flow-1", "agent").is_err());
}

#[test]
fn oversized_failure_text_is_logged_by_length_only() {
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    #[derive(Clone, Default)]
    struct Sink(Arc<Mutex<Vec<u8>>>);
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let sink = Sink::default();
    let writer = sink.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .finish();
    let secret = format!("customer-secret-{}", "x".repeat(200_000));
    tracing::subscriber::with_default(subscriber, || {
        super::jobs::failure_for_test("TOOL_ERROR", secret.clone());
    });
    let logged = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
    assert!(logged.contains("message_len"), "{logged}");
    assert!(
        !logged.contains("customer-secret"),
        "addon text must not be logged"
    );
}
