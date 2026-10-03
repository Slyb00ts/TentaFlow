// ============ File: jobs.rs — fenced process service jobs using the existing flow executor ============

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use tentaflow_protocol::processes::{ActivityOutcome, ActivityResult, ProcessNodeKind};
use tokio_util::sync::CancellationToken;

use super::repository::{self, ActivityResultOrigin, ClaimedProcessJob, ObservedActivityResult};
use super::runtime::{plan_job_result, validate_output};
use crate::db::DbPool;
use crate::flow_engine::dispatcher::{FlowDispatcher, PinnedFlowSnapshot};
use crate::flow_engine::envelope::{FlowEnvelope, FlowExecutionOutcome, FlowValue};
use crate::flow_engine::expr::flow_value_to_json;

pub const LEASE_RENEWAL: Duration = Duration::from_secs(10);

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn failure(code: &str, message: impl Into<String>) -> ActivityResult {
    let message = message.into();
    let summary = repository::bounded_failure_message(&message);
    if summary != message {
        tracing::error!(code, reason = %message, "full process service failure exceeds its history summary budget");
    }
    ActivityResult {
        outcome: ActivityOutcome::Error,
        code: Some(code.into()),
        summary,
        outputs: Value::Null,
        evidence: Vec::new(),
    }
}

fn normalize_outcome(outcome: FlowExecutionOutcome) -> Result<ActivityResult> {
    if let Some(error) = outcome.error {
        return Ok(failure("FLOW_ERROR", error));
    }
    let envelope = outcome.final_envelope;
    let variables = envelope
        .variables
        .iter()
        .map(|(key, value)| (key.clone(), flow_value_to_json(value)))
        .collect::<BTreeMap<_, _>>();
    let artifacts = envelope
        .artifacts
        .iter()
        .map(|(key, value)| (key.clone(), flow_value_to_json(value)))
        .collect::<BTreeMap<_, _>>();
    let outputs = json!({
        "payload": flow_value_to_json(&envelope.payload),
        "variables": variables,
        "artifacts": artifacts,
    });
    validate_output(&outputs)?;
    Ok(ActivityResult {
        outcome: ActivityOutcome::Completed,
        code: None,
        summary: "The pinned flow returned an activity result".into(),
        outputs,
        evidence: vec![format!(
            "flow_execution_latency_ms:{}",
            outcome.total_latency_ms
        )],
    })
}

fn observed_result(
    outcome: FlowExecutionOutcome,
    expression: Option<&str>,
    variables: &Value,
) -> ObservedActivityResult {
    let platform_error = outcome.error.is_some();
    match normalize_outcome(outcome) {
        Err(error) => ObservedActivityResult {
            result: failure("OUTPUT_LIMIT", error.to_string()),
            origin: ActivityResultOrigin::Platform,
        },
        Ok(result) if platform_error => ObservedActivityResult {
            result,
            origin: ActivityResultOrigin::Platform,
        },
        Ok(result) => match expression {
            None => ObservedActivityResult {
                result,
                origin: ActivityResultOrigin::Envelope,
            },
            Some(expression) => {
                match super::runtime::evaluate(expression, variables, &result.outputs, &[])
                    .and_then(parse_contract_result)
                {
                    Ok(result) => ObservedActivityResult {
                        result,
                        origin: ActivityResultOrigin::Contract,
                    },
                    Err(error) => ObservedActivityResult {
                        result: failure("RESULT_EXPRESSION_ERROR", format!("{error:#}")),
                        origin: ActivityResultOrigin::Platform,
                    },
                }
            }
        },
    }
}
pub(super) fn parse_contract_result(value: Value) -> Result<ActivityResult> {
    let object = value
        .as_object()
        .context("service result expression must return a complete ActivityResult object")?;
    ensure!(
        object.len() == 5
            && ["outcome", "code", "summary", "outputs", "evidence"]
                .iter()
                .all(|key| object.contains_key(*key)),
        "service result expression must return exactly outcome/code/summary/outputs/evidence"
    );
    let result: ActivityResult = serde_json::from_value(value)?;
    validate_output(&result.outputs)?;
    ensure!(
        result.summary.len() <= 32 * 1024
            && result.evidence.len() <= 64
            && result.evidence.iter().all(|s| s.len() <= 2048),
        "service business result metadata exceeds its budget"
    );
    if let Some(code) = &result.code {
        ensure!(
            !code.is_empty()
                && code.len() <= 64
                && code
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b)),
            "service business error code is invalid"
        );
    }
    ensure!(
        serde_json::to_vec(&result)?.len() <= 384 * 1024 - 4096,
        "service business result exceeds history/detail budget"
    );
    Ok(result)
}

fn fail_claim(
    pool: &DbPool,
    worker_id: &str,
    claimed: &ClaimedProcessJob,
    code: &str,
    error: impl std::fmt::Display,
    observed: Option<&ObservedActivityResult>,
) -> Result<()> {
    let message = error.to_string();
    tracing::error!(job_id = %claimed.job.job_id, code, reason = %message, "process service claim failed");
    repository::fail_job(
        pool,
        &claimed.job.job_id,
        claimed.job.attempt,
        claimed.job.fence,
        worker_id,
        code,
        &message,
        now_ms(),
        observed,
    )?;
    Ok(())
}

pub async fn execute_claimed(
    pool: &DbPool,
    dispatcher: &Arc<FlowDispatcher>,
    worker_id: &str,
    claimed: ClaimedProcessJob,
    cancel: CancellationToken,
) -> Result<()> {
    let node = repository::scope_node(
        &claimed.snapshot.model,
        &claimed.snapshot.scopes,
        &claimed.job.instance_id,
        &claimed.job.scope_id,
        &claimed.job.node_id,
    )?;
    let effective = repository::effective_scope_variables(
        &claimed.snapshot.scopes,
        &claimed.snapshot.scope_variables,
        &claimed.job.instance_id,
        &claimed.snapshot.instance.variables,
        &claimed.job.scope_id,
    )?;
    let ProcessNodeKind::ServiceTask {
        flow_id,
        timeout_seconds,
        result_expression,
        ..
    } = &node.kind
    else {
        return fail_claim(
            pool,
            worker_id,
            &claimed,
            "INVALID_SERVICE_JOB",
            "job node is not a service task",
            None,
        );
    };
    let snapshot = claimed
        .snapshot
        .service_snapshots
        .iter()
        .find(|snapshot| snapshot.info.node_id == node.id && snapshot.info.flow_id == *flow_id)
        .context("pinned service graph missing")?;
    let pinned = PinnedFlowSnapshot {
        flow_id: snapshot.info.flow_id.clone(),
        source_version: snapshot.info.source_version,
        graph_json: snapshot.graph_json.clone(),
        graph_sha256: snapshot.info.graph_sha256.clone(),
    };
    let mut meta = match dispatcher.authorize_process_flow(
        flow_id,
        &claimed.actor.user_id,
        &claimed.actor.org_id,
    ) {
        Ok(meta) => meta,
        Err(error) => {
            return fail_claim(
                pool,
                worker_id,
                &claimed,
                "SOURCE_ACCESS_REVOKED",
                error,
                None,
            )
        }
    };
    meta.request_id = format!(
        "{}:{}:{}",
        claimed.job.job_id, claimed.job.attempt, claimed.job.fence
    );
    meta.correlation_id = Some(claimed.job.instance_id.clone());
    meta.cancel_token = cancel.clone();
    meta.deadline = Some(Instant::now() + Duration::from_secs(u64::from(*timeout_seconds)));
    let mut envelope = FlowEnvelope::empty();
    envelope.payload = FlowValue::Json(
        claimed
            .job
            .input
            .get("payload")
            .context("service input payload missing")?
            .clone(),
    );
    envelope.variables = claimed
        .job
        .input
        .get("variables")
        .and_then(Value::as_object)
        .context("service input variables missing")?
        .iter()
        .map(|(key, value)| (key.clone(), FlowValue::Json(value.clone())))
        .collect();

    // Cancellation may commit between the claim and running-handle registration.
    // Check the durable claim after registration, before polling the executor.
    if cancel.is_cancelled() {
        return fail_claim(
            pool,
            worker_id,
            &claimed,
            "INTERRUPTED",
            "the worker was cancelled before executing the service task",
            None,
        );
    }
    match repository::renew_job_lease(
        pool,
        &claimed.job.job_id,
        claimed.job.attempt,
        claimed.job.fence,
        worker_id,
        now_ms(),
    ) {
        Ok(true) => {}
        Ok(false) => {
            cancel.cancel();
            return fail_claim(
                pool,
                worker_id,
                &claimed,
                "LEASE_LOST",
                "service job no longer owns its execution lease",
                None,
            );
        }
        Err(error) => {
            cancel.cancel();
            return fail_claim(
                pool,
                worker_id,
                &claimed,
                "SOURCE_ACCESS_REVOKED",
                error,
                None,
            );
        }
    }

    let mut renewal =
        tokio::time::interval_at(tokio::time::Instant::now() + LEASE_RENEWAL, LEASE_RENEWAL);
    renewal.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let execution = dispatcher.dispatch_pinned_flow(&pinned, envelope, meta);
    tokio::pin!(execution);
    let timeout = tokio::time::sleep(Duration::from_secs(u64::from(*timeout_seconds)));
    tokio::pin!(timeout);
    let result = loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                return fail_claim(pool, worker_id, &claimed, "INTERRUPTED", "the worker was cancelled before its external effect was confirmed", None);
            }
            _ = renewal.tick() => {
                if let Err(error) = dispatcher.authorize_process_flow(flow_id, &claimed.actor.user_id, &claimed.actor.org_id) {
                    cancel.cancel();
                    return fail_claim(pool, worker_id, &claimed, "SOURCE_ACCESS_REVOKED", error, None);
                }
                match repository::renew_job_lease(pool, &claimed.job.job_id, claimed.job.attempt, claimed.job.fence, worker_id, now_ms()) {
                    Ok(true) => {},
                    Ok(false) => {
                        cancel.cancel();
                        return fail_claim(pool, worker_id, &claimed, "LEASE_LOST", "service job no longer owns its execution lease", None);
                    }
                    Err(error) => {
                        cancel.cancel();
                        return fail_claim(pool, worker_id, &claimed, "SOURCE_ACCESS_REVOKED", error, None);
                    }
                }
            }
            _ = &mut timeout => {
                cancel.cancel();
                break ObservedActivityResult{result:failure("SERVICE_TIMEOUT", "the service task exceeded its configured timeout"),origin:ActivityResultOrigin::Platform};
            }
            outcome = &mut execution => {
                break match outcome {
                    Ok(outcome) => observed_result(outcome,result_expression.as_deref(),&effective),
                    Err(error) => ObservedActivityResult{result:failure("FLOW_ERROR", error.to_string()),origin:ActivityResultOrigin::Platform},
                };
            }
        }
    };

    // A CAS retry only recalculates transitions from the already observed result.
    // It never repeats an external flow execution.
    for _ in 0..8 {
        let current =
            match repository::runtime_snapshot(pool, &claimed.actor, &claimed.job.instance_id) {
                Ok(current) => current,
                Err(error) => {
                    return fail_claim(
                        pool,
                        worker_id,
                        &claimed,
                        "SOURCE_ACCESS_REVOKED",
                        error,
                        Some(&result),
                    )
                }
            };
        let Some(job) = current
            .jobs
            .iter()
            .find(|job| job.job_id == claimed.job.job_id)
        else {
            return Ok(());
        };
        if job.status != "running"
            || job.attempt != claimed.job.attempt
            || job.fence != claimed.job.fence
        {
            return Ok(());
        }
        if let Err(error) = dispatcher.authorize_process_flow(
            flow_id,
            &claimed.actor.user_id,
            &claimed.actor.org_id,
        ) {
            return fail_claim(
                pool,
                worker_id,
                &claimed,
                "SOURCE_ACCESS_REVOKED",
                error,
                Some(&result),
            );
        }
        let at_ms = now_ms();
        let plan = match plan_job_result(&current, job, &result, at_ms) {
            Ok(plan) => plan,
            Err(error) => {
                return fail_claim(
                    pool,
                    worker_id,
                    &claimed,
                    "TRANSITION_ERROR",
                    error,
                    Some(&result),
                )
            }
        };
        match repository::accept_job_result(
            pool,
            &claimed.actor,
            &job.job_id,
            job.attempt,
            job.fence,
            worker_id,
            &result,
            current.instance.revision,
            &plan,
            at_ms,
        ) {
            Ok(committed) => {
                super::runtime::signal_cancelled_claims(dispatcher, &committed.cancelled_claims);
                return Ok(());
            }
            Err(error) if error.to_string().contains("revision conflict") => continue,
            Err(error) => {
                return fail_claim(
                    pool,
                    worker_id,
                    &claimed,
                    "RESULT_REJECTED",
                    error,
                    Some(&result),
                )
            }
        }
    }
    fail_claim(
        pool,
        worker_id,
        &claimed,
        "REVISION_CONFLICT",
        "concurrent process transitions prevented result publication",
        Some(&result),
    )
}

#[cfg(test)]
mod tests {
    use super::super::runtime::{self, test_support::*};
    use super::*;
    use tentaflow_protocol::processes::{
        ActivityVerification, ProcessInstanceStatus, ProcessUserTaskStatus,
    };

    async fn execute(fixture: &Fixture, worker: &str) -> ClaimedProcessJob {
        let claimed = repository::claim_job(&fixture.db, worker, now_ms())
            .unwrap()
            .expect("claim persisted service");
        execute_claimed(
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

    async fn observe_effect(fixture: &Fixture, claimed: &ClaimedProcessJob) -> ActivityResult {
        let source = &claimed.snapshot.service_snapshots[0];
        let pinned = PinnedFlowSnapshot {
            flow_id: source.info.flow_id.clone(),
            source_version: source.info.source_version,
            graph_json: source.graph_json.clone(),
            graph_sha256: source.info.graph_sha256.clone(),
        };
        let meta = fixture
            .dispatcher()
            .authorize_process_flow(
                &pinned.flow_id,
                &claimed.actor.user_id,
                &claimed.actor.org_id,
            )
            .unwrap();
        let envelope =
            FlowEnvelope::with_payload(FlowValue::Json(claimed.job.input["payload"].clone()));
        normalize_outcome(
            fixture
                .dispatcher()
                .dispatch_pinned_flow(&pinned, envelope, meta)
                .await
                .unwrap(),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn published_service_executes_pinned_graph_after_live_source_edit() {
        let at_ms = chrono::Utc::now().timestamp_millis();
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("published", None));
        let started = start_model(
            &fixture,
            &service_model(
                &flow_id,
                ActivityVerification::Condition {
                    expression: "outputs.variables.marker == 'published'".into(),
                },
            ),
        );
        update_flow(
            &fixture.db,
            &fixture.owner,
            &flow_id,
            &graph("edited", None),
        );
        let current = crate::db::repository::get_flow(&fixture.db, &flow_id)
            .unwrap()
            .unwrap();
        let claim = execute(&fixture, "pinned-worker").await;
        assert!(
            current.version > i64::from(claim.snapshot.service_snapshots[0].info.source_version)
        );
        let completed =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        assert_eq!(completed.instance.status, ProcessInstanceStatus::Completed);
        assert_eq!(completed.instance.variables["answer"], "published");
        let job = completed
            .jobs
            .iter()
            .find(|job| job.job_id == claim.job.job_id)
            .unwrap();
        assert_eq!(
            job.result.as_ref().unwrap().outputs["variables"]["marker"],
            "published"
        );
        let history =
            repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                .unwrap()
                .0;
        assert_eq!(
            history
                .iter()
                .filter(|event| event.kind == "service_result")
                .count(),
            1
        );
        let result = job.result.as_ref().unwrap();
        let plan = runtime::plan_job_result(
            &claim.snapshot,
            &claim.job,
            &crate::processes::repository::ObservedActivityResult {
                result: (result).clone(),
                origin: crate::processes::repository::ActivityResultOrigin::Envelope,
            },
            at_ms,
        )
        .unwrap();
        repository::accept_job_result(
            &fixture.db,
            &fixture.owner,
            &job.job_id,
            claim.job.attempt,
            claim.job.fence,
            "pinned-worker",
            &crate::processes::repository::ObservedActivityResult {
                result: (result).clone(),
                origin: crate::processes::repository::ActivityResultOrigin::Envelope,
            },
            claim.snapshot.instance.revision,
            &plan,
            at_ms,
        )
        .unwrap()
        .instance;
        assert_eq!(
            repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                .unwrap()
                .0,
            history
        );
    }

    #[tokio::test]
    async fn human_verification_uses_persisted_result_and_explicit_retry_after_rejection() {
        let at_ms = chrono::Utc::now().timestamp_millis();
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("evidence", None));
        let started = start_model(
            &fixture,
            &service_model(&flow_id, ActivityVerification::Human),
        );
        let first = execute(&fixture, "human-worker").await;
        let waiting =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        assert_eq!(waiting.instance.status, ProcessInstanceStatus::Waiting);
        assert!(waiting.instance.variables.get("answer").is_none());
        let task = waiting
            .user_tasks
            .iter()
            .find(|task| task.status == ProcessUserTaskStatus::Open)
            .unwrap();
        let detail = repository::get_user_task(
            &fixture.db,
            &fixture.owner,
            &started.instance_id,
            &task.user_task_id,
        )
        .unwrap();
        assert_eq!(detail.outputs["outputs"]["variables"]["marker"], "evidence");
        assert!(runtime::plan_user_completion(
            &waiting,
            &task.user_task_id,
            &Value::Null,
            None,
            at_ms
        )
        .is_err());
        let rejection = runtime::plan_user_completion(
            &waiting,
            &task.user_task_id,
            &json!("insufficient"),
            Some(false),
            at_ms,
        )
        .unwrap();
        let rejected = repository::complete_user_task(
            &fixture.db,
            &fixture.owner,
            &stamp("reject result"),
            &started.instance_id,
            &task.user_task_id,
            waiting.instance.revision,
            &json!("insufficient"),
            Some(false),
            &rejection,
            at_ms,
        )
        .unwrap()
        .instance;
        assert_eq!(rejected.status, ProcessInstanceStatus::Incident);
        assert_eq!(rejected.incidents[0].code, "HUMAN_REJECTED");
        assert_eq!(
            rejected.incidents[0].job_id.as_deref(),
            Some(first.job.job_id.as_str())
        );
        repository::retry_job(
            &fixture.db,
            &fixture.owner,
            &stamp("retry rejected service"),
            &started.instance_id,
            &first.job.job_id,
            rejected.revision,
        )
        .unwrap();
        let second = execute(&fixture, "human-worker").await;
        assert!(second.job.attempt > first.job.attempt);
        assert!(second.job.fence > first.job.fence);
        let waiting =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        let task = waiting
            .user_tasks
            .iter()
            .find(|task| task.status == ProcessUserTaskStatus::Open)
            .unwrap();
        let approval = runtime::plan_user_completion(
            &waiting,
            &task.user_task_id,
            &json!({"answer":"client cannot replace service output"}),
            Some(true),
            at_ms,
        )
        .unwrap();
        let completed = repository::complete_user_task(
            &fixture.db,
            &fixture.owner,
            &stamp("approve observed result"),
            &started.instance_id,
            &task.user_task_id,
            waiting.instance.revision,
            &json!({"answer":"client cannot replace service output"}),
            Some(true),
            &approval,
            at_ms,
        )
        .unwrap()
        .instance;
        assert_eq!(completed.status, ProcessInstanceStatus::Completed);
        assert_eq!(completed.variables["answer"], "evidence");
        let executions =
            crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 100)
                .unwrap();
        assert_eq!(
            executions.len(),
            2,
            "only the explicit retry executes the service again"
        );
    }

    #[tokio::test]
    async fn observed_effect_before_crash_is_not_reexecuted_without_retry_and_old_fence_is_rejected(
    ) {
        let at_ms = chrono::Utc::now().timestamp_millis();
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("observed", None));
        let started = start_model(
            &fixture,
            &service_model(
                &flow_id,
                ActivityVerification::Condition {
                    expression: "true".into(),
                },
            ),
        );
        let claim = repository::claim_job(&fixture.db, "lost-worker", now_ms())
            .unwrap()
            .unwrap();
        let observed = observe_effect(&fixture, &claim).await;
        let late_plan = runtime::plan_job_result(
            &claim.snapshot,
            &claim.job,
            &crate::processes::repository::ObservedActivityResult {
                result: (observed).clone(),
                origin: crate::processes::repository::ActivityResultOrigin::Envelope,
            },
            at_ms,
        )
        .unwrap();
        let path = fixture.directory.path().join("processes.db");
        let actor = fixture.owner.clone();
        let Fixture {
            directory,
            db,
            router,
            participant,
            ..
        } = fixture;
        drop(router);
        drop(db);
        let db = crate::db::init(&path).unwrap();
        assert_eq!(repository::recover_jobs(&db, None, now_ms()).unwrap(), 1);
        assert_eq!(repository::recover_jobs(&db, None, now_ms()).unwrap(), 0);
        assert!(repository::claim_job(&db, "replacement", now_ms())
            .unwrap()
            .is_none());
        let interrupted =
            repository::get_instance(&db, &actor, &started.instance_id, None).unwrap();
        assert_eq!(interrupted.status, ProcessInstanceStatus::Incident);
        assert_eq!(interrupted.incidents[0].code, "INTERRUPTED");
        assert_eq!(
            crate::db::repository::list_flow_executions_for_flow(&db, &flow_id, 100)
                .unwrap()
                .len(),
            1
        );
        assert!(repository::accept_job_result(
            &db,
            &actor,
            &claim.job.job_id,
            claim.job.attempt,
            claim.job.fence,
            "lost-worker",
            &crate::processes::repository::ObservedActivityResult {
                result: (observed).clone(),
                origin: crate::processes::repository::ActivityResultOrigin::Envelope
            },
            claim.snapshot.instance.revision,
            &late_plan,
            at_ms
        )
        .is_err());
        repository::retry_job(
            &db,
            &actor,
            &stamp("retry uncertain effect"),
            &started.instance_id,
            &claim.job.job_id,
            interrupted.revision,
        )
        .unwrap();
        let router = Arc::new(
            crate::routing::Router::new(crate::config::RouterConfig::default(), Some(db.clone()))
                .unwrap(),
        );
        let fixture = Fixture {
            directory,
            db,
            router,
            owner: actor,
            participant,
        };
        execute(&fixture, "replacement").await;
        assert_eq!(
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None)
                .unwrap()
                .status,
            ProcessInstanceStatus::Completed
        );
        assert_eq!(
            crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 100)
                .unwrap()
                .len(),
            2
        );
    }

    #[tokio::test]
    async fn queued_revocation_and_cancelled_claim_never_publish_a_late_result() {
        let at_ms = chrono::Utc::now().timestamp_millis();
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("authorized", None));
        let started = start_model(
            &fixture,
            &service_model(
                &flow_id,
                ActivityVerification::Condition {
                    expression: "true".into(),
                },
            ),
        );
        crate::db::repository::resource_permissions::set(
            &fixture.db,
            "flow",
            &flow_id,
            "user",
            &fixture.owner.user_id,
            "deny",
        )
        .unwrap();
        assert!(
            repository::claim_job(&fixture.db, "revoked-worker", now_ms())
                .unwrap()
                .is_none()
        );
        let denied =
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None)
                .unwrap();
        assert_eq!(denied.status, ProcessInstanceStatus::Incident);
        assert_eq!(denied.incidents[0].code, "SOURCE_ACCESS_REVOKED");
        assert!(
            crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 10)
                .unwrap()
                .is_empty()
        );
        crate::db::repository::resource_permissions::set(
            &fixture.db,
            "flow",
            &flow_id,
            "user",
            &fixture.owner.user_id,
            "allow",
        )
        .unwrap();
        let job_id = denied.incidents[0].job_id.as_ref().unwrap();
        repository::retry_job(
            &fixture.db,
            &fixture.owner,
            &stamp("restore source and retry"),
            &started.instance_id,
            job_id,
            denied.revision,
        )
        .unwrap();
        let claim = repository::claim_job(&fixture.db, "cancelled-worker", now_ms())
            .unwrap()
            .unwrap();
        let observed = observe_effect(&fixture, &claim).await;
        let plan = runtime::plan_job_result(
            &claim.snapshot,
            &claim.job,
            &crate::processes::repository::ObservedActivityResult {
                result: (observed).clone(),
                origin: crate::processes::repository::ActivityResultOrigin::Envelope,
            },
            at_ms,
        )
        .unwrap();
        let current =
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None)
                .unwrap();
        repository::cancel_instance(
            &fixture.db,
            &fixture.owner,
            &stamp("cancel before result"),
            &started.instance_id,
            current.revision,
        )
        .unwrap()
        .instance;
        let events =
            repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                .unwrap()
                .0;
        assert!(!repository::renew_job_lease(
            &fixture.db,
            &claim.job.job_id,
            claim.job.attempt,
            claim.job.fence,
            "cancelled-worker",
            now_ms()
        )
        .unwrap());
        assert!(repository::accept_job_result(
            &fixture.db,
            &fixture.owner,
            &claim.job.job_id,
            claim.job.attempt,
            claim.job.fence,
            "cancelled-worker",
            &crate::processes::repository::ObservedActivityResult {
                result: (observed).clone(),
                origin: crate::processes::repository::ActivityResultOrigin::Envelope
            },
            current.revision,
            &plan,
            at_ms
        )
        .is_err());
        assert!(!repository::fail_job(
            &fixture.db,
            &claim.job.job_id,
            claim.job.attempt,
            claim.job.fence,
            "cancelled-worker",
            "INTERRUPTED",
            "late worker",
            now_ms(),
            None
        )
        .unwrap());
        assert_eq!(
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None)
                .unwrap()
                .status,
            ProcessInstanceStatus::Cancelled
        );
        assert_eq!(
            repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                .unwrap()
                .0,
            events
        );
    }

    #[tokio::test]
    async fn cancelled_or_expired_claim_never_enters_the_flow_executor() {
        for scenario in ["cancelled", "expired", "worker_stopped"] {
            let fixture = Fixture::new();
            let flow_id = flow(
                &fixture.db,
                &fixture.owner,
                &graph("must not execute", None),
            );
            let started = start_model(
                &fixture,
                &service_model(
                    &flow_id,
                    ActivityVerification::Condition {
                        expression: "true".into(),
                    },
                ),
            );
            let claimed_at = if scenario == "expired" {
                now_ms() - 31_000
            } else {
                now_ms()
            };
            let claim = repository::claim_job(&fixture.db, scenario, claimed_at)
                .unwrap()
                .expect("claim the actual persisted job before its execution gate");
            let cancel = CancellationToken::new();
            if scenario == "cancelled" {
                repository::cancel_instance(
                    &fixture.db,
                    &fixture.owner,
                    &stamp("cancel in the claim registration window"),
                    &started.instance_id,
                    claim.snapshot.instance.revision,
                )
                .unwrap()
                .instance;
                assert!(!cancel.is_cancelled());
            } else if scenario == "worker_stopped" {
                cancel.cancel();
            }
            let before =
                repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                    .unwrap()
                    .0;
            execute_claimed(&fixture.db, fixture.dispatcher(), scenario, claim, cancel)
                .await
                .unwrap();
            assert!(
                crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 10)
                    .unwrap()
                    .is_empty(),
                "{scenario} claim entered the actual flow executor"
            );
            let current =
                repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None)
                    .unwrap();
            let events =
                repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                    .unwrap()
                    .0;
            assert!(current.variables.get("answer").is_none());
            assert!(events.iter().all(|event| event.kind != "service_result"));
            if scenario == "cancelled" {
                assert_eq!(current.status, ProcessInstanceStatus::Cancelled);
                assert_eq!(events, before);
            } else {
                assert_eq!(current.status, ProcessInstanceStatus::Incident);
                assert_eq!(
                    current.incidents[0].code,
                    if scenario == "expired" {
                        "LEASE_LOST"
                    } else {
                        "INTERRUPTED"
                    }
                );
            }
        }
    }

    #[tokio::test]
    async fn current_acl_is_rechecked_after_actual_service_execution_has_started() {
        let fixture = Fixture::new();
        let flow_id = flow(
            &fixture.db,
            &fixture.owner,
            &graph("not publishable", Some(500)),
        );
        let started = start_model(
            &fixture,
            &service_model(
                &flow_id,
                ActivityVerification::Condition {
                    expression: "true".into(),
                },
            ),
        );
        let claim = repository::claim_job(&fixture.db, "running-worker", now_ms())
            .unwrap()
            .unwrap();
        let claimed_job = claim.job.clone();
        let db = fixture.db.clone();
        let dispatcher = fixture.dispatcher().clone();
        let task = tokio::spawn(async move {
            execute_claimed(
                &db,
                &dispatcher,
                "running-worker",
                claim,
                CancellationToken::new(),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if !crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 10)
                    .unwrap()
                    .is_empty()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("observe actual flow execution row before revoking access");
        crate::db::repository::resource_permissions::set(
            &fixture.db,
            "flow",
            &flow_id,
            "user",
            &fixture.owner.user_id,
            "deny",
        )
        .unwrap();
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let current =
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None)
                .unwrap();
        assert_eq!(current.status, ProcessInstanceStatus::Incident);
        assert_eq!(current.incidents[0].code, "SOURCE_ACCESS_REVOKED");
        assert_eq!(
            current.incidents[0].job_id.as_deref(),
            Some(claimed_job.job_id.as_str())
        );
        assert!(current.variables.get("answer").is_none());
        assert_eq!(current.variables, started.variables);
        let snapshot =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        let retained_job = snapshot
            .jobs
            .iter()
            .find(|job| job.job_id == claimed_job.job_id)
            .unwrap();
        assert_eq!(retained_job.status, "error");
        assert_eq!(retained_job.attempt, claimed_job.attempt);
        assert_eq!(retained_job.fence, claimed_job.fence + 1);
        assert!(retained_job.worker_id.is_none() && retained_job.lease_until_ms.is_none());
        assert_eq!(
            retained_job.result_origin,
            Some(ActivityResultOrigin::Platform)
        );
        let denied_reason = fixture
            .dispatcher()
            .authorize_process_flow(&flow_id, &fixture.owner.user_id, &fixture.owner.org_id)
            .err()
            .expect("actual source grant remains denied")
            .to_string();
        let result = retained_job.result.as_ref().unwrap();
        assert_eq!(result.outcome, ActivityOutcome::Error);
        assert_eq!(result.code.as_deref(), Some("FLOW_ERROR"));
        assert_eq!(
            result.summary,
            repository::bounded_failure_message(&denied_reason)
        );
        assert_eq!(result.outputs, Value::Null);
        assert!(result.evidence.is_empty());
        assert!(snapshot
            .tokens
            .iter()
            .any(|token| { token.token_id == claimed_job.token_id && token.status == "waiting" }));
        assert!(snapshot.user_tasks.is_empty());
        let executions =
            crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 10)
                .unwrap();
        assert_eq!(executions.len(), 1);
        assert_eq!(executions[0].status.as_deref(), Some("completed"));
        assert!(executions[0].total_latency_ms.unwrap() >= 0);
        let history =
            repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                .unwrap()
                .0;
        let observed = history
            .iter()
            .filter(|event| event.kind == "service_result")
            .collect::<Vec<_>>();
        assert_eq!(observed.len(), 1);
        let mut expected = serde_json::to_value(result).unwrap();
        expected["result_origin"] = json!("platform");
        assert_eq!(observed[0].data, expected);
        assert_eq!(observed[0].scope_id, started.instance_id);
        let failed = history
            .iter()
            .filter(|event| event.kind == "job_failed")
            .collect::<Vec<_>>();
        assert_eq!(failed.len(), 1);
        assert_eq!(failed[0].data["job_id"], claimed_job.job_id);
        assert_eq!(failed[0].data["code"], "SOURCE_ACCESS_REVOKED");
        assert_eq!(
            failed[0].data["message"],
            repository::bounded_failure_message(&denied_reason)
        );
        assert!(history.iter().all(|event| !matches!(
            event.kind.as_str(),
            "verification_passed"
                | "verification_created"
                | "verification_approved"
                | "job_success"
                | "scope_completed"
                | "end_reached"
                | "instance_completed"
                | "business_error_caught"
        )));
    }

    #[tokio::test]
    async fn false_condition_and_oversized_output_record_incidents_instead_of_success() {
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("evidence", None));
        let started = start_model(
            &fixture,
            &service_model(
                &flow_id,
                ActivityVerification::Condition {
                    expression: "outputs.variables.marker == 'unproved'".into(),
                },
            ),
        );
        let first = execute(&fixture, "condition-worker").await;
        let current =
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None)
                .unwrap();
        assert_eq!(current.status, ProcessInstanceStatus::Incident);
        assert_eq!(current.incidents[0].code, "VERIFICATION_FAILED");
        assert!(current.variables.get("answer").is_none());
        assert!(current.can_retry);
        repository::retry_job(
            &fixture.db,
            &fixture.owner,
            &stamp("explicit condition retry"),
            &started.instance_id,
            &first.job.job_id,
            current.revision,
        )
        .unwrap();
        execute(&fixture, "condition-worker").await;
        let current =
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None)
                .unwrap();
        assert_eq!(current.status, ProcessInstanceStatus::Incident);
        assert_eq!(
            current.incidents.len(),
            1,
            "prior retry incident is resolved, not silently accepted"
        );
        assert_eq!(
            crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 100)
                .unwrap()
                .len(),
            2
        );

        let mut large_graph: Value = serde_json::from_str(&graph("unused", None)).unwrap();
        large_graph["nodes"][0]["config"]["output_mapping"]["marker"] = json!("payload.large");
        let large_flow = flow(&fixture.db, &fixture.owner, &large_graph.to_string());
        let mut large_model = service_model(&large_flow, ActivityVerification::Human);
        large_model
            .variables
            .insert("large".into(), json!("e".repeat(160 * 1024)));
        let started = start_model(&fixture, &large_model);
        execute(&fixture, "bounded-output-worker").await;
        let current =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        assert_eq!(current.instance.status, ProcessInstanceStatus::Incident);
        assert_eq!(current.instance.incidents[0].code, "OUTPUT_LIMIT");
        assert_eq!(
            current.jobs[0].status, "error",
            "a real post-effect output error must terminate the claim"
        );
        assert!(current.instance.can_retry);
        assert!(
            current.instance.user_tasks.is_empty(),
            "oversized output is not a fictitious verification success"
        );
    }

    #[tokio::test]
    async fn expired_lease_rejects_observed_result_and_long_failure_keeps_explicit_provenance() {
        let fixture = Fixture::new();
        let flow_id = flow(&fixture.db, &fixture.owner, &graph("late", None));
        let started = start_model(
            &fixture,
            &service_model(
                &flow_id,
                ActivityVerification::Condition {
                    expression: "true".into(),
                },
            ),
        );
        let at_ms = now_ms() - 31_000;
        let claim = repository::claim_job(&fixture.db, "overdue-worker", at_ms)
            .unwrap()
            .unwrap();
        assert!(at_ms < claim.job.lease_until_ms.unwrap());
        assert!(claim.job.lease_until_ms.unwrap() < now_ms());
        let observed = observe_effect(&fixture, &claim).await;
        let plan = runtime::plan_job_result(
            &claim.snapshot,
            &claim.job,
            &crate::processes::repository::ObservedActivityResult {
                result: (observed).clone(),
                origin: crate::processes::repository::ActivityResultOrigin::Envelope,
            },
            at_ms,
        )
        .unwrap();
        assert!(repository::accept_job_result(
            &fixture.db,
            &fixture.owner,
            &claim.job.job_id,
            claim.job.attempt,
            claim.job.fence,
            "overdue-worker",
            &crate::processes::repository::ObservedActivityResult {
                result: (observed).clone(),
                origin: crate::processes::repository::ActivityResultOrigin::Envelope
            },
            claim.snapshot.instance.revision,
            &plan,
            at_ms
        )
        .is_err());
        let reason = "A real service failure with Unicode detail: 🧪".repeat(1200);
        let result = failure("LARGE_REASON", reason.clone());
        assert!(result.summary.contains("original bytes="));
        assert!(result.summary.contains("sha256="));
        fail_claim(
            &fixture.db,
            "overdue-worker",
            &claim,
            "LEASE_LOST",
            &reason,
            Some(&ObservedActivityResult {
                result: observed.clone(),
                origin: ActivityResultOrigin::Envelope,
            }),
        )
        .unwrap();
        let current =
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None)
                .unwrap();
        assert_eq!(current.status, ProcessInstanceStatus::Incident);
        assert_eq!(current.incidents[0].code, "LEASE_LOST");
        let history =
            repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                .unwrap()
                .0;
        let failure = history
            .iter()
            .find(|event| event.kind == "job_failed")
            .unwrap();
        assert!(failure.data["message"]
            .as_str()
            .unwrap()
            .contains("original bytes="));
        assert!(failure.data["message"]
            .as_str()
            .unwrap()
            .contains("sha256="));
        let retained = history
            .iter()
            .filter(|event| event.kind == "service_result")
            .collect::<Vec<_>>();
        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].data["outputs"], observed.outputs);
        assert_eq!(retained[0].data["result_origin"], "envelope");
    }

    #[tokio::test]
    async fn boundary_queued_claimed_and_observed_jobs_cannot_reappear_through_retry_or_late_result(
    ) {
        for stage in ["queued", "claimed", "observed"] {
            let fixture = Fixture::new();
            let flow_id = flow(
                &fixture.db,
                &fixture.owner,
                &graph("factual external effect", None),
            );
            let model = with_boundaries(
                service_model(&flow_id, ActivityVerification::Human),
                "Service",
                &[("Limit", true, 1)],
            );
            let started = start_model(&fixture, &model);
            let initial =
                repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                    .unwrap();
            let claim = if stage == "queued" {
                None
            } else {
                repository::claim_job(&fixture.db, "late-worker", now_ms()).unwrap()
            };
            let observed = if stage == "observed" {
                Some(observe_effect(&fixture, claim.as_ref().unwrap()).await)
            } else {
                None
            };
            let late_plan = observed.as_ref().map(|result| {
                plan_job_result(
                    &claim.as_ref().unwrap().snapshot,
                    &claim.as_ref().unwrap().job,
                    &crate::processes::repository::ObservedActivityResult {
                        result: (result).clone(),
                        origin: crate::processes::repository::ActivityResultOrigin::Envelope,
                    },
                    now_ms(),
                )
                .unwrap()
            });
            let drained =
                super::super::timers::drain_due(&fixture.db, initial.timers[0].due_at_ms.unwrap());
            drained.completion.unwrap();
            assert_eq!(drained.fired, 1);
            assert_eq!(
                drained.cancelled_claims.len(),
                usize::from(stage != "queued")
            );
            let current =
                repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                    .unwrap();
            assert_eq!(current.jobs[0].status, "cancelled");
            assert!(current.jobs[0].result.is_none());
            assert_eq!(current.instance.status, ProcessInstanceStatus::Completed);
            assert!(!current.instance.can_retry);
            assert!(repository::retry_job(
                &fixture.db,
                &fixture.owner,
                &stamp("retry interrupted activation"),
                &started.instance_id,
                &current.jobs[0].job_id,
                current.instance.revision
            )
            .is_err());
            if let Some(claim) = &claim {
                assert!(!repository::renew_job_lease(
                    &fixture.db,
                    &claim.job.job_id,
                    claim.job.attempt,
                    claim.job.fence,
                    "late-worker",
                    now_ms()
                )
                .unwrap());
                assert!(!repository::fail_job(
                    &fixture.db,
                    &claim.job.job_id,
                    claim.job.attempt,
                    claim.job.fence,
                    "late-worker",
                    "LATE_ERROR",
                    "must not revive interrupted activation",
                    now_ms(),
                    None
                )
                .unwrap());
                if let Some(result) = &observed {
                    assert!(repository::accept_job_result(
                        &fixture.db,
                        &fixture.owner,
                        &claim.job.job_id,
                        claim.job.attempt,
                        claim.job.fence,
                        "late-worker",
                        &crate::processes::repository::ObservedActivityResult {
                            result: (result).clone(),
                            origin: crate::processes::repository::ActivityResultOrigin::Envelope
                        },
                        claim.snapshot.instance.revision,
                        late_plan.as_ref().unwrap(),
                        now_ms()
                    )
                    .is_err());
                } else {
                    execute_claimed(
                        &fixture.db,
                        fixture.dispatcher(),
                        "late-worker",
                        claim.clone(),
                        CancellationToken::new(),
                    )
                    .await
                    .unwrap();
                }
            }
            assert!(repository::claim_job(&fixture.db, "replacement", now_ms())
                .unwrap()
                .is_none());
            let actual_effect_count =
                crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 10)
                    .unwrap()
                    .len();
            assert_eq!(actual_effect_count, usize::from(stage == "observed"));
            let history =
                repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                    .unwrap()
                    .0;
            assert!(history
                .iter()
                .all(|event| event.kind != "service_result" && event.kind != "job_failed"));
        }
    }
    fn business_graph(result: Value) -> String {
        json!({"nodes":[{"id":"trigger","type":"trigger","config":{"output_mapping":{"actual_result":result.to_string()}}},{"id":"output","type":"output","config":{}}],"edges":[{"from":"trigger","to":"output","from_port":"text","to_port":"text"}],"variables":[{"name":"actual_result","type":"json"}]}).to_string()
    }
    fn error_handlers(
        mut model: tentaflow_protocol::processes::ProcessModel,
    ) -> tentaflow_protocol::processes::ProcessModel {
        use tentaflow_protocol::processes::{ProcessErrorDeclaration, ProcessNode};
        model.errors.push(ProcessErrorDeclaration {
            error_id: "ErrorDecl".into(),
            name: "Business rejection".into(),
            error_code: "REJECTED".into(),
        });
        for (id, reference) in [("Exact", Some("ErrorDecl")), ("Any", None)] {
            model.nodes.push(ProcessNode {
                id: id.into(),
                name: format!("Handle {id}"),
                kind: ProcessNodeKind::BoundaryError {
                    attached_to_id: "Service".into(),
                    error_ref: reference.map(str::to_owned),
                    output_mapping: BTreeMap::from([(
                        "business_evidence".into(),
                        "activity_result.evidence".into(),
                    )]),
                },
            });
            model.nodes.push(ProcessNode {
                id: format!("Work_{id}"),
                name: format!("Review {id}"),
                kind: ProcessNodeKind::UserTask {
                    assignee_user_id: None,
                    output_mapping: BTreeMap::new(),
                },
            });
            model.sequence_flows.extend([
                edge(&format!("ErrorPath_{id}"), id, &format!("Work_{id}")),
                edge(&format!("ErrorEnd_{id}"), &format!("Work_{id}"), "End_1"),
            ]);
        }
        model
    }

    #[tokio::test]
    async fn real_pinned_result_contract_routes_exact_error_before_catch_all_and_preserves_factual_evidence(
    ) {
        for code in [Some("REJECTED"), Some("OTHER"), None] {
            let fixture = Fixture::new();
            let body = json!({"outcome":"Error","code":code,"summary":"A real business rejection","outputs":{"customer_ID":17},"evidence":["actual_public_flow_result"]});
            let flow_id = flow(&fixture.db, &fixture.owner, &business_graph(body.clone()));
            let mut model = error_handlers(service_model(
                &flow_id,
                ActivityVerification::Condition {
                    expression: "true".into(),
                },
            ));
            if let ProcessNodeKind::ServiceTask {
                result_expression, ..
            } = &mut model.nodes[1].kind
            {
                *result_expression = Some("outputs.variables.actual_result".into());
            }
            for node in &mut model.nodes {
                if let ProcessNodeKind::BoundaryError { output_mapping, .. } = &mut node.kind {
                    output_mapping.extend([
                        ("business_result".into(), "activity_result".into()),
                        ("business_code".into(), "activity_result.code".into()),
                        ("business_customer".into(), "outputs.customer_ID".into()),
                    ]);
                }
            }
            let started = start_model(&fixture, &model);
            let claim = execute(&fixture, "contract-worker").await;
            let state =
                repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                    .unwrap();
            let expected = if code == Some("REJECTED") {
                "Exact"
            } else {
                "Any"
            };
            assert_eq!(state.instance.status, ProcessInstanceStatus::Waiting);
            assert!(state
                .user_tasks
                .iter()
                .any(|task| task.node_id == format!("Work_{expected}")));
            assert_eq!(
                state.jobs[0].result_origin,
                Some(ActivityResultOrigin::Contract)
            );
            assert_eq!(
                state.jobs[0].result.as_ref().unwrap().outputs["customer_ID"],
                17
            );
            assert_eq!(state.jobs[0].status, "error");
            assert!(!state.instance.can_retry);
            assert_eq!(
                state.instance.variables["business_evidence"],
                json!(["actual_public_flow_result"])
            );
            assert_eq!(state.instance.variables["business_result"], body);
            assert_eq!(state.instance.variables["business_code"], json!(code));
            assert_eq!(state.instance.variables["business_customer"], 17);
            assert!(repository::retry_job(
                &fixture.db,
                &fixture.owner,
                &stamp("handled error cannot retry"),
                &started.instance_id,
                &claim.job.job_id,
                state.instance.revision
            )
            .is_err());
            let history =
                repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                    .unwrap()
                    .0;
            let result = history
                .iter()
                .find(|event| event.kind == "service_result")
                .unwrap();
            assert_eq!(result.data["result_origin"], "contract");
            assert_eq!(
                result.data["evidence"],
                json!(["actual_public_flow_result"])
            );
            assert_eq!(
                history
                    .iter()
                    .find(|event| event.kind == "business_error_caught")
                    .unwrap()
                    .node_id
                    .as_deref(),
                Some(expected)
            );
        }
    }

    #[tokio::test]
    async fn invalid_contract_and_forged_provenance_cannot_consume_business_error_boundary() {
        for expression in ["{'outcome':'Error'}", "1 / 0"] {
            let fixture = Fixture::new();
            let flow_id = flow(
                &fixture.db,
                &fixture.owner,
                &graph("actual execution", None),
            );
            let mut model = error_handlers(service_model(&flow_id, ActivityVerification::Human));
            if let ProcessNodeKind::ServiceTask {
                result_expression, ..
            } = &mut model.nodes[1].kind
            {
                *result_expression = Some(expression.into());
            }
            let started = start_model(&fixture, &model);
            execute(&fixture, "invalid-contract").await;
            let state =
                repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                    .unwrap();
            assert_eq!(
                state.jobs[0].result_origin,
                Some(ActivityResultOrigin::Platform)
            );
            assert_eq!(state.instance.status, ProcessInstanceStatus::Incident);
            assert!(state
                .subscriptions
                .iter()
                .all(|sub| sub.status
                    == tentaflow_protocol::processes::ProcessSubscriptionStatus::Open));
            assert!(state.user_tasks.is_empty());
            assert!(repository::list_events(
                &fixture.db,
                &fixture.owner,
                &started.instance_id,
                0,
                200
            )
            .unwrap()
            .0
            .iter()
            .all(|event| event.kind != "business_error_caught"));
        }
        let fixture = Fixture::new();
        let flow_id = flow(
            &fixture.db,
            &fixture.owner,
            &graph("forged origin denied", None),
        );
        let started = start_model(
            &fixture,
            &error_handlers(service_model(&flow_id, ActivityVerification::Human)),
        );
        let claim = repository::claim_job(&fixture.db, "forged-result", now_ms())
            .unwrap()
            .unwrap();
        let result = ObservedActivityResult {
            origin: ActivityResultOrigin::Contract,
            result: ActivityResult {
                outcome: ActivityOutcome::Error,
                code: Some("REJECTED".into()),
                summary: "Untrusted origin must not route".into(),
                outputs: Value::Null,
                evidence: Vec::new(),
            },
        };
        let at = now_ms();
        let plan = plan_job_result(&claim.snapshot, &claim.job, &result, at).unwrap();
        assert!(repository::accept_job_result(
            &fixture.db,
            &fixture.owner,
            &claim.job.job_id,
            claim.job.attempt,
            claim.job.fence,
            "forged-result",
            &result,
            claim.snapshot.instance.revision,
            &plan,
            at
        )
        .is_err());
        let actual =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        assert_eq!(actual.jobs[0].status, "running");
        assert!(actual.jobs[0].result.is_none());
        assert!(actual
            .subscriptions
            .iter()
            .all(|s| s.status == tentaflow_protocol::processes::ProcessSubscriptionStatus::Open));
    }

    #[tokio::test]
    async fn explicit_needs_human_keeps_message_boundary_live_until_actual_verification_completion()
    {
        use crate::processes::messages::test_support::{
            boundary_messages, catch_target, envelope, published, send, start_version,
        };
        let fixture = Fixture::new();
        let flow_id = flow(
            &fixture.db,
            &fixture.owner,
            &business_graph(
                json!({"outcome":"NeedsHuman","code":null,"summary":"Review actual result","outputs":{"answer":17},"evidence":["observed"]}),
            ),
        );
        let mut model = service_model(
            &flow_id,
            ActivityVerification::Condition {
                expression: "true".into(),
            },
        );
        if let ProcessNodeKind::ServiceTask {
            result_expression,
            output_mapping,
            ..
        } = &mut model.nodes[1].kind
        {
            *result_expression = Some("outputs.variables.actual_result".into());
            *output_mapping = BTreeMap::from([("answer".into(), "outputs.answer".into())]);
        }
        let mut model = embedded_model(model, "Scope");
        if let ProcessNodeKind::SubProcess { output_mapping, .. } = &mut model.nodes[1].kind {
            output_mapping.insert("answer".into(), "outputs.answer".into());
        }
        let model = boundary_messages(model, "Scope", &[("Notify", false, "EvidenceReady")]);
        let version = published(&fixture, &model);
        let started = start_version(&fixture, &version);
        execute(&fixture, "human-contract").await;
        let state = repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
            .unwrap();
        let task = state
            .user_tasks
            .iter()
            .find(|t| t.kind == tentaflow_protocol::processes::ProcessUserTaskKind::Verification)
            .unwrap();
        assert_eq!(
            state.subscriptions[0].status,
            tentaflow_protocol::processes::ProcessSubscriptionStatus::Open
        );
        let message = envelope(
            catch_target(&version, Some(&started.instance_id), None),
            Value::Null,
        );
        send(&fixture, &message);
        crate::processes::messages::drain_pending(&fixture.db, now_ms())
            .completion
            .unwrap();
        let current =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        assert!(current.user_tasks.iter().any(
            |t| t.user_task_id == task.user_task_id && t.status == ProcessUserTaskStatus::Open
        ));
        let at = now_ms();
        let plan = crate::processes::runtime::plan_user_completion(
            &current,
            &task.user_task_id,
            &json!({}),
            Some(true),
            at,
        )
        .unwrap();
        repository::complete_user_task(
            &fixture.db,
            &fixture.owner,
            &stamp("approve actual persisted result"),
            &started.instance_id,
            &task.user_task_id,
            current.instance.revision,
            &json!({}),
            Some(true),
            &plan,
            at,
        )
        .unwrap()
        .instance;
        let final_state =
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None)
                .unwrap();
        assert_eq!(final_state.variables["answer"], 17);
        assert_eq!(
            final_state
                .scopes
                .iter()
                .find(|scope| scope.parent_scope_id.is_some())
                .unwrap()
                .status,
            ProcessInstanceStatus::Completed
        );
        assert_eq!(
            final_state.subscriptions[0].status,
            tentaflow_protocol::processes::ProcessSubscriptionStatus::Consumed
        );
    }

    #[tokio::test]
    async fn actual_verification_retries_page_retained_work_and_selected_resolved_incidents_without_hiding_runtime_state(
    ) {
        let fixture = Fixture::new();
        let flow_id = flow(
            &fixture.db,
            &fixture.owner,
            &graph("actual retry result", None),
        );
        let started = start_model(
            &fixture,
            &service_model(&flow_id, ActivityVerification::Human),
        );
        let mut first_task = None;
        let mut first_incident = None;
        for index in 0..21 {
            let claim = execute(&fixture, "paged-retries").await;
            let snapshot =
                repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                    .unwrap();
            let task = snapshot
                .user_tasks
                .iter()
                .find(|task| task.status == ProcessUserTaskStatus::Open)
                .unwrap();
            first_task.get_or_insert_with(|| task.user_task_id.clone());
            let at = now_ms();
            let plan = crate::processes::runtime::plan_user_completion(
                &snapshot,
                &task.user_task_id,
                &json!({}),
                Some(false),
                at,
            )
            .unwrap();
            let rejected = repository::complete_user_task(
                &fixture.db,
                &fixture.owner,
                &stamp("reject persisted verification"),
                &started.instance_id,
                &task.user_task_id,
                snapshot.instance.revision,
                &json!({}),
                Some(false),
                &plan,
                at,
            )
            .unwrap()
            .instance;
            first_incident.get_or_insert_with(|| rejected.incidents[0].incident_id.clone());
            if index < 20 {
                repository::retry_job(
                    &fixture.db,
                    &fixture.owner,
                    &stamp("explicit actual retry"),
                    &started.instance_id,
                    &claim.job.job_id,
                    rejected.revision,
                )
                .unwrap();
            }
        }
        use tentaflow_protocol::processes::{ProcessInstancePageRequest, ProcessPageSpec};
        let first =
            repository::get_instance(&fixture.db, &fixture.owner, &started.instance_id, None)
                .unwrap();
        assert_eq!(first.user_tasks.len(), 20);
        assert_eq!(first.pages.as_ref().unwrap().user_tasks.total, 21);
        assert_eq!(
            first.pages.as_ref().unwrap().user_tasks.next_offset,
            Some(20)
        );
        assert!(first.can_retry);
        let pages = ProcessInstancePageRequest {
            user_tasks: Some(ProcessPageSpec {
                offset: 20,
                limit: 20,
            }),
            incidents: Some(ProcessPageSpec {
                offset: 100,
                limit: 20,
            }),
            timers: None,
            subscriptions: None,
            event_races: None,
            outgoing_messages: None,
            scopes: None,
            calls: None,
            selected_user_task_id: first_task.clone(),
            selected_incident_id: first_incident.clone(),
        };
        let second = repository::get_instance(
            &fixture.db,
            &fixture.owner,
            &started.instance_id,
            Some(&pages),
        )
        .unwrap();
        assert_eq!(second.user_tasks.len(), 1);
        assert_eq!(second.pages.as_ref().unwrap().user_tasks.next_offset, None);
        assert!(second.incidents.is_empty());
        assert!(
            second.can_retry,
            "hidden current incident still governs retry"
        );
        assert_eq!(
            second.selected_user_task.as_ref().unwrap().user_task_id,
            first_task.unwrap()
        );
        assert!(second
            .selected_incident
            .as_ref()
            .unwrap()
            .resolved_at_ms
            .is_some());
        assert!(
            !second
                .selected_incident
                .as_ref()
                .unwrap()
                .incident
                .can_retry
        );
        assert_eq!(
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap()
                .user_tasks
                .len(),
            21
        );
        assert_eq!(
            crate::db::repository::list_flow_executions_for_flow(&fixture.db, &flow_id, 100)
                .unwrap()
                .len(),
            21
        );
    }
    #[tokio::test]
    async fn rejected_scoped_result_budget_preserves_observed_effect_without_parent_return_or_rerun(
    ) {
        let fixture = Fixture::new();
        let source=flow(&fixture.db,&fixture.owner,&json!({"nodes":[
            {"id":"trigger","type":"trigger","config":{}},{"id":"output","type":"output","config":{}}
        ],"edges":[{"from":"trigger","to":"output","from_port":"text","to_port":"text"}]}).to_string());
        let mut body = service_model(
            &source,
            ActivityVerification::Condition {
                expression: "true".into(),
            },
        );
        if let ProcessNodeKind::ServiceTask {
            input_mapping,
            output_mapping,
            ..
        } = &mut body.nodes[1].kind
        {
            *input_mapping = BTreeMap::from([("payload".into(), "vars.large_parent".into())]);
            *output_mapping = BTreeMap::from([("child_copy".into(), "outputs.payload".into())]);
        }
        let mut model = embedded_model(body, "Scope");
        model
            .variables
            .insert("large_parent".into(), json!("x".repeat(180 * 1024)));
        if let ProcessNodeKind::SubProcess { output_mapping, .. } = &mut model.nodes[1].kind {
            *output_mapping =
                BTreeMap::from([("returned_copy".into(), "outputs.child_copy".into())]);
        }
        let started = start_model(&fixture, &model);
        let scope = started
            .scopes
            .iter()
            .find(|scope| scope.parent_scope_id.is_some())
            .unwrap()
            .clone();
        let claimed = execute(&fixture, "scope-budget-worker").await;
        let actual =
            repository::runtime_snapshot(&fixture.db, &fixture.owner, &started.instance_id)
                .unwrap();
        assert_eq!(actual.instance.status, ProcessInstanceStatus::Incident);
        assert_eq!(actual.instance.variables, started.variables);
        let retained = actual
            .jobs
            .iter()
            .find(|job| job.job_id == claimed.job.job_id)
            .unwrap();
        assert_eq!(retained.status, "error");
        assert_eq!(retained.result_origin, Some(ActivityResultOrigin::Envelope));
        assert_eq!(
            retained.result.as_ref().unwrap().outputs["payload"],
            started.variables["large_parent"]
        );
        assert_eq!(actual.scope_variables[&scope.scope_id], json!({}));
        assert!(actual
            .tokens
            .iter()
            .any(
                |token| Some(token.token_id.as_str()) == scope.parent_token_id.as_deref()
                    && token.status == "waiting"
            ));
        assert!(actual
            .tokens
            .iter()
            .any(|token| token.token_id == claimed.job.token_id
                && token.scope_id == scope.scope_id
                && token.status == "waiting"));
        assert!(actual
            .instance
            .incidents
            .iter()
            .any(|incident| incident.scope_id == scope.scope_id
                && incident.code == "RESULT_REJECTED"));
        let events =
            repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                .unwrap()
                .0;
        let observed = events
            .iter()
            .filter(|event| event.kind == "service_result")
            .collect::<Vec<_>>();
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].scope_id, scope.scope_id);
        assert_eq!(
            observed[0].data["outputs"],
            retained.result.as_ref().unwrap().outputs
        );
        assert!(events
            .iter()
            .all(|event| event.kind != "scope_completed" && event.kind != "business_error_caught"));
        assert_eq!(
            crate::db::repository::list_flow_executions_for_flow(&fixture.db, &source, 10)
                .unwrap()
                .len(),
            1
        );
        execute_claimed(
            &fixture.db,
            fixture.dispatcher(),
            "scope-budget-worker",
            claimed,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            crate::db::repository::list_flow_executions_for_flow(&fixture.db, &source, 10)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            repository::list_events(&fixture.db, &fixture.owner, &started.instance_id, 0, 200)
                .unwrap()
                .0,
            events
        );
    }
}
