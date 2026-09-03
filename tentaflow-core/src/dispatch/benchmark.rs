// =============================================================================
// Plik: dispatch/benchmark.rs
// Opis: Handlery binarnego API Benchmark Studio — definicje benchmarków, targety,
//       start/anulowanie runów (nieblokujące), historia i wyniki. Sekrety
//       (api_key) nigdy nie wracają w wire; run leci w tokio::spawn, a jego
//       progres jest re-emitowany przez współdzieloną szynę logów (log_bus),
//       którą subskrybuje streaming handler `BenchmarkRunStreamRequest`.
// Przykład: BenchmarkPayload::StartRunRequest → StartRunResponse { run_id }.
// =============================================================================

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    BenchmarkPayload, BenchmarkSummaryWire, BenchmarkWire, MessageBody, ProtocolError,
    ProtocolErrorCode, ResultRowWire, RunSummaryWire, TargetInputWire, TargetWire,
};

use super::HandlerContext;
use crate::benchmark::db as store;
use crate::benchmark::types::{
    BenchEvent, BenchmarkConfig, BenchmarkListItem, BenchmarkRecord, BenchmarkResultRecord,
    BenchmarkRunRecord, BenchmarkTargetRecord, BenchmarkTargetUpsert,
};
use crate::db::DbPool;
use crate::deploy::log_bus::{self, BusMessage, LogLine};
use crate::services::rbac::OrgContext;

/// Package id of the native Benchmark Studio app (app-platform pilot).
const PACKAGE_ID: &str = "benchmark-studio";
const PERM_READ: &str = "benchmark.read";
const PERM_WRITE: &str = "benchmark.write";
const RECENT_RUNS_LIMIT: u32 = 50;

/// Anulowanie biezacych zadan tego procesu. Wspolny typ z
/// `services::cancel_registry` — kazda z trzech kopii tej mapy miala wlasna
/// implementacje, a ta w Project Studio wprost nazywala sie lustrem benchmarku.
static BENCH_CANCEL: crate::services::cancel_registry::CancelRegistry =
    crate::services::cancel_registry::CancelRegistry::new();

fn register_cancel(run_id: &str) -> Arc<AtomicBool> {
    BENCH_CANCEL.register(run_id)
}

fn unregister_cancel(run_id: &str) {
    BENCH_CANCEL.unregister(run_id)
}

/// Flags a live run for cancellation. `false` = this process does not own it.
fn signal_cancel(run_id: &str) -> bool {
    BENCH_CANCEL.signal(run_id)
}

fn require_org(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    ctx.org_context
        .as_ref()
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required"))
}

// App-platform gate: availability (installed + enabled instance) and access
// come from the addon permission matrix, no longer from org-RBAC role strings.
// The instance the gate resolved is also the owner of the content database,
// so the pool comes out of the same check — no second lookup path to drift.
fn require_access<'a>(
    ctx: &'a HandlerContext,
    permission: &str,
) -> Result<(&'a OrgContext, DbPool), ProtocolError> {
    let org = require_org(ctx)?;
    let addon_id = super::app_gate::require_app_permission(ctx, PACKAGE_ID, permission)?;
    let pool = store::pool(&ctx.state.db, &addon_id).map_err(|e| db_error("open", e))?;
    Ok((org, pool))
}

/// Read gate shared with the run-progress stream handler, which must bind a
/// subscriber to the same instance database the run was written to.
pub(super) fn require_read(ctx: &HandlerContext) -> Result<(&OrgContext, DbPool), ProtocolError> {
    require_access(ctx, PERM_READ)
}

fn require_write(ctx: &HandlerContext) -> Result<(&OrgContext, DbPool), ProtocolError> {
    require_access(ctx, PERM_WRITE)
}

fn db_error(scope: &str, error: anyhow::Error) -> ProtocolError {
    tracing::warn!(scope, error = %error, "benchmark studio database error");
    ProtocolError::internal("benchmark database error")
}

/// Liczba włączonych scenariuszy w configu — do kolumny „testy" w przeglądzie.
/// Nieparsowalny config traktujemy jako zero scenariuszy (edytor i tak pokaże
/// pełny błąd przy Get).
fn count_enabled_scenarios(config_json: &str) -> u32 {
    match serde_json::from_str::<BenchmarkConfig>(config_json) {
        Ok(cfg) => {
            let mut n = 0;
            if cfg.latency.is_some() {
                n += 1;
            }
            if cfg.throughput.is_some() {
                n += 1;
            }
            if cfg.context.is_some() {
                n += 1;
            }
            if cfg.sustained.is_some() {
                n += 1;
            }
            n
        }
        Err(_) => 0,
    }
}

fn run_to_wire(run: &BenchmarkRunRecord, benchmark_name: Option<String>) -> RunSummaryWire {
    RunSummaryWire {
        id: run.id.clone(),
        benchmark_id: run.benchmark_id.clone(),
        benchmark_name,
        started_at: run.started_at.clone(),
        finished_at: run.finished_at.clone(),
        status: run.status.clone(),
        error: run.error.clone(),
    }
}

fn item_to_wire(item: BenchmarkListItem) -> BenchmarkSummaryWire {
    let test_count = count_enabled_scenarios(&item.record.config_json);
    let last_run = item.last_run.as_ref().map(|r| run_to_wire(r, None));
    BenchmarkSummaryWire {
        id: item.record.id,
        name: item.record.name,
        target_count: item.target_count,
        test_count,
        models: item.models,
        last_run,
    }
}

fn target_to_wire(rec: BenchmarkTargetRecord) -> TargetWire {
    TargetWire {
        id: rec.id,
        kind: rec.kind,
        service_ref: rec.service_ref,
        api_type: rec.api_type,
        host: rec.host,
        port: rec.port,
        has_key: rec.api_key_enc.is_some(),
        model: rec.model,
        label: rec.label,
    }
}

fn benchmark_to_wire(
    record: BenchmarkRecord,
    targets: Vec<BenchmarkTargetRecord>,
) -> BenchmarkWire {
    BenchmarkWire {
        id: record.id,
        name: record.name,
        config_json: record.config_json,
        targets: targets.into_iter().map(target_to_wire).collect(),
        created_at: record.created_at,
        updated_at: record.updated_at,
    }
}

fn input_to_upsert(t: TargetInputWire) -> BenchmarkTargetUpsert {
    BenchmarkTargetUpsert {
        id: t.id,
        kind: t.kind,
        service_ref: t.service_ref,
        api_type: t.api_type,
        host: t.host,
        port: t.port,
        api_key: t.api_key,
        model: t.model,
        label: t.label,
    }
}

fn result_to_wire(r: BenchmarkResultRecord) -> ResultRowWire {
    ResultRowWire {
        target_id: r.target_id,
        target_label: r.target_label,
        scenario: r.scenario,
        variant_json: r.variant_json,
        ttft_ms_mean: r.ttft_ms_mean,
        ttft_ms_sigma: r.ttft_ms_sigma,
        prefill_tps_mean: r.prefill_tps_mean,
        prefill_tps_sigma: r.prefill_tps_sigma,
        decode_tps_mean: r.decode_tps_mean,
        decode_tps_sigma: r.decode_tps_sigma,
        total_ms_mean: r.total_ms_mean,
        total_ms_sigma: r.total_ms_sigma,
        p50_ms: r.p50_ms,
        p90_ms: r.p90_ms,
        p99_ms: r.p99_ms,
        requests: r.requests,
        errors: r.errors,
        samples_json: r.samples_json,
    }
}

#[handler(variant = "BenchmarkBody", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn benchmark_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::BenchmarkBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected BenchmarkBody")),
    };

    match payload {
        BenchmarkPayload::ListRequest => list_v1(ctx),
        BenchmarkPayload::GetRequest { id } => get_v1(ctx, id),
        BenchmarkPayload::SaveRequest {
            id,
            name,
            config_json,
            targets,
        } => save_v1(ctx, id.clone(), name, config_json, targets.clone()),
        BenchmarkPayload::DeleteRequest { id } => delete_v1(ctx, id),
        BenchmarkPayload::StartRunRequest { benchmark_id } => start_run_v1(ctx, benchmark_id),
        BenchmarkPayload::RunStatusRequest { run_id } => run_status_v1(ctx, run_id),
        BenchmarkPayload::RunResultsRequest { run_id } => run_results_v1(ctx, run_id),
        BenchmarkPayload::ListRunsRequest { benchmark_id } => list_runs_v1(ctx, benchmark_id),
        BenchmarkPayload::RecentRunsRequest => recent_runs_v1(ctx),
        BenchmarkPayload::CancelRunRequest { run_id } => cancel_run_v1(ctx, run_id),
        BenchmarkPayload::ListResponse { .. }
        | BenchmarkPayload::GetResponse { .. }
        | BenchmarkPayload::SaveResponse { .. }
        | BenchmarkPayload::DeleteResult { .. }
        | BenchmarkPayload::StartRunResponse { .. }
        | BenchmarkPayload::RunStatusResponse { .. }
        | BenchmarkPayload::RunResultsResponse { .. }
        | BenchmarkPayload::ListRunsResponse { .. }
        | BenchmarkPayload::RecentRunsResponse { .. }
        | BenchmarkPayload::CancelRunResult { .. }
        | BenchmarkPayload::RunStreamRequest { .. }
        | BenchmarkPayload::RunStreamChunk { .. }
        | BenchmarkPayload::RunStreamEnd { .. } => Err(ProtocolError::bad_request(
            "variant is not a valid benchmark request",
        )),
    }
}

macro_rules! register_benchmark_variant {
    ($variant:literal, $metric:literal) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: crate::dispatch::SessionAuthKind::UserSession,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_benchmark_dispatch,
            }
        }
    };
}

register_benchmark_variant!(
    "BenchmarkListRequest",
    "tentaflow_ws_handler_benchmark_list"
);
register_benchmark_variant!("BenchmarkGetRequest", "tentaflow_ws_handler_benchmark_get");
register_benchmark_variant!(
    "BenchmarkSaveRequest",
    "tentaflow_ws_handler_benchmark_save"
);
register_benchmark_variant!(
    "BenchmarkDeleteRequest",
    "tentaflow_ws_handler_benchmark_delete"
);
register_benchmark_variant!(
    "BenchmarkStartRunRequest",
    "tentaflow_ws_handler_benchmark_start_run"
);
register_benchmark_variant!(
    "BenchmarkRunStatusRequest",
    "tentaflow_ws_handler_benchmark_run_status"
);
register_benchmark_variant!(
    "BenchmarkRunResultsRequest",
    "tentaflow_ws_handler_benchmark_run_results"
);
register_benchmark_variant!(
    "BenchmarkListRunsRequest",
    "tentaflow_ws_handler_benchmark_list_runs"
);
register_benchmark_variant!(
    "BenchmarkRecentRunsRequest",
    "tentaflow_ws_handler_benchmark_recent_runs"
);
register_benchmark_variant!(
    "BenchmarkCancelRunRequest",
    "tentaflow_ws_handler_benchmark_cancel_run"
);

fn list_v1(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let (org, db) = require_read(ctx)?;
    let items = store::list_benchmarks(&db, &org.org_id).map_err(|e| db_error("list", e))?;
    let benchmarks = items.into_iter().map(item_to_wire).collect();
    Ok(MessageBody::BenchmarkBody(BenchmarkPayload::ListResponse {
        benchmarks,
    }))
}

fn get_v1(ctx: &HandlerContext, id: &str) -> Result<MessageBody, ProtocolError> {
    let (org, db) = require_read(ctx)?;
    let (record, targets) = store::get_benchmark(&db, &org.org_id, id)
        .map_err(|e| db_error("get", e))?
        .ok_or_else(|| ProtocolError::not_found("benchmark not found"))?;
    Ok(MessageBody::BenchmarkBody(BenchmarkPayload::GetResponse {
        benchmark: benchmark_to_wire(record, targets),
    }))
}

fn save_v1(
    ctx: &HandlerContext,
    id: Option<String>,
    name: &str,
    config_json: &str,
    targets: Vec<TargetInputWire>,
) -> Result<MessageBody, ProtocolError> {
    let (org, db) = require_write(ctx)?;
    if name.trim().is_empty() {
        return Err(ProtocolError::bad_request("benchmark name is required"));
    }
    // Odrzuć niepoprawny config wcześnie — inaczej run wywali się dopiero przy starcie.
    serde_json::from_str::<BenchmarkConfig>(config_json)
        .map_err(|e| ProtocolError::bad_request(format!("invalid config_json: {e}")))?;

    let id = id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let upserts: Vec<BenchmarkTargetUpsert> = targets.into_iter().map(input_to_upsert).collect();
    store::upsert_benchmark(
        &db,
        &org.org_id,
        &id,
        name,
        config_json,
        &upserts,
        &ctx.state.settings_cipher,
    )
    .map_err(|e| db_error("save", e))?;
    Ok(MessageBody::BenchmarkBody(BenchmarkPayload::SaveResponse {
        id,
    }))
}

fn delete_v1(ctx: &HandlerContext, id: &str) -> Result<MessageBody, ProtocolError> {
    let (org, db) = require_write(ctx)?;
    let ok = store::delete_benchmark(&db, &org.org_id, id)
        .map_err(|e| db_error("delete", e))?;
    Ok(MessageBody::BenchmarkBody(BenchmarkPayload::DeleteResult {
        ok,
    }))
}

/// Nieblokujący start runu: waliduje istnienie benchmarku, otwiera rekord runu,
/// rejestruje token anulowania i spawnuje `run_benchmark`, po czym natychmiast
/// zwraca `run_id`. Progres leci przez log_bus (klucz = run_id); front subskrybuje
/// go osobnym streamem `BenchmarkRunStreamRequest`.
fn start_run_v1(ctx: &HandlerContext, benchmark_id: &str) -> Result<MessageBody, ProtocolError> {
    let (org, db) = require_write(ctx)?;
    // Zapewnij, że benchmark istnieje w tej org i ma targety, zanim otworzymy run.
    let (_record, targets) = store::get_benchmark(&db, &org.org_id, benchmark_id)
        .map_err(|e| db_error("start_run.get", e))?
        .ok_or_else(|| ProtocolError::not_found("benchmark not found"))?;
    if targets.is_empty() {
        return Err(ProtocolError::bad_request("benchmark has no targets"));
    }

    let engine_meta = serde_json::json!({
        "node_id": ctx.state.local_node_id.as_ref(),
        "core_version": env!("CARGO_PKG_VERSION"),
    })
    .to_string();
    let run_id = store::create_benchmark_run(&db, benchmark_id, &engine_meta)
        .map_err(|e| db_error("start_run.create", e))?;

    let cancel = register_cancel(&run_id);
    let cipher = ctx.state.settings_cipher.clone();
    // Sciezka in-process: KAZDY model z katalogu (embedded llama.cpp/MLX, QUIC
    // sidecar, most coding-agenta, model na innym wezle) jest mierzalny przez
    // executor, bez wlasnego endpointu HTTP. Tozsamosc operatora jedzie razem z
    // nim, wiec ACL modelu obowiazuje tak samo jak przy zwyklym requescie.
    let local = ctx.state.router.executor().map(|executor| {
        crate::benchmark::LocalRunner::new(
            executor,
            Some(crate::auth::acl::UserContext::new(
                org.user_id.clone(),
                org.role_id.clone(),
            )),
        )
    });
    let org_id = org.org_id.clone();
    let benchmark_id = benchmark_id.to_string();
    let run_id_task = run_id.clone();
    // Utwórz kanał log_bus SYNCHRONICZNIE, przed spawnem: StartRunResponse może
    // wrócić do frontu zanim task ruszy, a natychmiastowy BenchmarkRunStreamRequest
    // musi trafić w istniejący kanał (inaczej subscribe(None) → pusty End).
    let tx = log_bus::sender_for(&run_id);

    tokio::spawn(async move {
        let progress: crate::benchmark::ProgressFn = {
            let tx = tx.clone();
            let run_id = run_id_task.clone();
            Arc::new(move |event: BenchEvent| {
                // Terminal (Done/Error) obsługujemy poniżej z realnym statusem
                // z DB — cancelled też idzie przez Done, więc nie da się go tu
                // rozróżnić. Tu re-emitujemy wyłącznie postęp.
                let line = match event {
                    BenchEvent::Phase {
                        target_label,
                        scenario,
                        message,
                        ..
                    } => LogLine {
                        deploy_id: run_id.clone(),
                        kind: "phase".to_string(),
                        line: format!("{target_label}: {message}"),
                        phase: scenario.to_string(),
                        progress_pct: 0,
                        ts_ms: log_bus::now_ms(),
                    },
                    BenchEvent::Log { line } => LogLine {
                        deploy_id: run_id.clone(),
                        kind: "log".to_string(),
                        line,
                        phase: String::new(),
                        progress_pct: 0,
                        ts_ms: log_bus::now_ms(),
                    },
                    BenchEvent::PartialResult { result } => LogLine {
                        deploy_id: run_id.clone(),
                        kind: "result".to_string(),
                        line: format!(
                            "{} / {}: {} req, {} err",
                            result.target_label, result.scenario, result.requests, result.errors
                        ),
                        phase: result.scenario.clone(),
                        progress_pct: 0,
                        ts_ms: log_bus::now_ms(),
                    },
                    BenchEvent::Done | BenchEvent::Error { .. } => return,
                };
                let _ = tx.send(BusMessage::Line(line));
            })
        };

        // Uruchom run w osobnym tasku i poczekaj na JoinHandle: panika w
        // run_benchmark albo w callbacku postępu NIE może ubić procesu ani
        // pominąć finalizerów poniżej (status DB, End, close, unregister). Panika
        // → JoinError::is_panic → traktujemy jak porażkę i wymuszamy status.
        let run = {
            let db = db.clone();
            let org_id = org_id.clone();
            let benchmark_id = benchmark_id.clone();
            let run_id_task = run_id_task.clone();
            let cipher = cipher.clone();
            tokio::spawn(async move {
                crate::benchmark::run_benchmark(
                    db,
                    &org_id,
                    &benchmark_id,
                    &run_id_task,
                    &cipher,
                    local,
                    cancel,
                    progress,
                )
                .await
            })
        };
        if let Err(join_err) = run.await {
            if join_err.is_panic() {
                let _ = store::finish_benchmark_run(
                    &db,
                    &run_id_task,
                    "failed",
                    Some("benchmark task panicked"),
                );
            }
        }

        // Terminal z realnym statusem/błędem z DB (success | failed | cancelled).
        let (status, error) = match store::get_benchmark_run(&db, &org_id, &run_id_task) {
            Ok(Some(run)) => (run.status, run.error.unwrap_or_default()),
            _ => ("failed".to_string(), "run record missing".to_string()),
        };
        let _ = tx.send(BusMessage::End {
            deploy_id: run_id_task.clone(),
            final_status: status,
            image_tag: String::new(),
            container_name: String::new(),
            error_message: error,
            duration_ms: 0,
        });
        // Daj subskrybentom chwilę na odbiór End, potem zamknij kanał i zwolnij token.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        log_bus::close(&run_id_task);
        unregister_cancel(&run_id_task);
    });

    Ok(MessageBody::BenchmarkBody(
        BenchmarkPayload::StartRunResponse { run_id },
    ))
}

fn run_status_v1(ctx: &HandlerContext, run_id: &str) -> Result<MessageBody, ProtocolError> {
    let (org, db) = require_read(ctx)?;
    let run = store::get_benchmark_run(&db, &org.org_id, run_id)
        .map_err(|e| db_error("run_status", e))?
        .ok_or_else(|| ProtocolError::not_found("run not found"))?;
    Ok(MessageBody::BenchmarkBody(
        BenchmarkPayload::RunStatusResponse {
            run_id: run.id,
            status: run.status,
            error: run.error,
            started_at: run.started_at,
            finished_at: run.finished_at,
        },
    ))
}

fn run_results_v1(ctx: &HandlerContext, run_id: &str) -> Result<MessageBody, ProtocolError> {
    let (org, db) = require_read(ctx)?;
    let rows = store::get_benchmark_run_results(&db, &org.org_id, run_id)
        .map_err(|e| db_error("run_results", e))?;
    let results = rows.into_iter().map(result_to_wire).collect();
    Ok(MessageBody::BenchmarkBody(
        BenchmarkPayload::RunResultsResponse { results },
    ))
}

fn list_runs_v1(ctx: &HandlerContext, benchmark_id: &str) -> Result<MessageBody, ProtocolError> {
    let (org, db) = require_read(ctx)?;
    let runs = store::list_benchmark_runs(&db, &org.org_id, benchmark_id)
        .map_err(|e| db_error("list_runs", e))?
        .iter()
        .map(|r| run_to_wire(r, None))
        .collect();
    Ok(MessageBody::BenchmarkBody(
        BenchmarkPayload::ListRunsResponse { runs },
    ))
}

fn recent_runs_v1(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let (org, db) = require_read(ctx)?;
    let runs = store::list_recent_benchmark_runs(&db, &org.org_id, RECENT_RUNS_LIMIT)
        .map_err(|e| db_error("recent_runs", e))?
        .iter()
        .map(|(run, name)| run_to_wire(run, Some(name.clone())))
        .collect();
    Ok(MessageBody::BenchmarkBody(
        BenchmarkPayload::RecentRunsResponse { runs },
    ))
}

fn cancel_run_v1(ctx: &HandlerContext, run_id: &str) -> Result<MessageBody, ProtocolError> {
    let (org, db) = require_write(ctx)?;
    // Autoryzacja + istnienie runu w org: nie pozwól sygnalizować cudzych runów.
    let run = store::get_benchmark_run(&db, &org.org_id, run_id)
        .map_err(|e| db_error("cancel_run", e))?
        .ok_or_else(|| ProtocolError::not_found("run not found"))?;
    let ok = if run.finished_at.is_some() {
        false
    } else {
        signal_cancel(run_id)
    };
    Ok(MessageBody::BenchmarkBody(
        BenchmarkPayload::CancelRunResult { ok },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_protocol::SessionAuth;

    const USER: &str = "bench-user";

    fn ctx_for(state: &Arc<crate::dispatch::state::AppState>) -> HandlerContext {
        HandlerContext {
            session: SessionAuth::UserSession {
                user_id: [0x42u8; 16],
                role: None,
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state: state.clone(),
            org_context: Some(OrgContext {
                user_id: USER.to_string(),
                org_id: crate::services::org::DEFAULT_ORG_ID.to_string(),
                role_id: "role-x".to_string(),
                permissions: Default::default(),
            }),
        }
    }

    fn target(id: &str) -> TargetInputWire {
        TargetInputWire {
            id: id.to_string(),
            kind: "external".to_string(),
            service_ref: None,
            api_type: "openai".to_string(),
            host: "127.0.0.1".to_string(),
            port: 8080,
            api_key: Some("sk-secret".to_string()),
            model: "gpt-x".to_string(),
            label: "external".to_string(),
        }
    }

    /// The handlers persist through the instance database the app gate
    /// resolved: the main DB has no benchmark tables any more, and the file
    /// the manifest names (`benchmark.db`) appears in the instance data dir.
    #[test]
    fn handlers_write_to_the_instance_database_not_the_main_one() {
        crate::addon::fs_sandbox::with_tmp_home(|| {
            let state = crate::dispatch::state::AppState::for_test();
            let instance = super::super::app_gate::test_support::install_app(
                &state,
                PACKAGE_ID,
                &[PERM_READ],
            );
            super::super::app_gate::test_support::grant(&state, &instance, USER, PERM_WRITE);
            let ctx = ctx_for(&state);

            let MessageBody::BenchmarkBody(BenchmarkPayload::SaveResponse { id }) =
                save_v1(&ctx, None, "latency sweep", "{}", vec![target("t1")]).expect("save")
            else {
                panic!("unexpected save response");
            };

            let MessageBody::BenchmarkBody(BenchmarkPayload::GetResponse { benchmark }) =
                get_v1(&ctx, &id).expect("get")
            else {
                panic!("unexpected get response");
            };
            assert_eq!(benchmark.name, "latency sweep");
            assert_eq!(benchmark.targets.len(), 1);
            assert!(benchmark.targets[0].has_key, "key stored, never echoed");

            let MessageBody::BenchmarkBody(BenchmarkPayload::ListResponse { benchmarks }) =
                list_v1(&ctx).expect("list")
            else {
                panic!("unexpected list response");
            };
            assert_eq!(benchmarks.len(), 1);
            assert_eq!(benchmarks[0].id, id);
            assert_eq!(benchmarks[0].target_count, 1);

            let db_file = crate::addon::fs_sandbox::addon_data_dir(
                crate::services::org::DEFAULT_ORG_ID,
                &instance,
            )
            .expect("data dir")
            .join("benchmark.db");
            assert!(db_file.is_file(), "content db at {db_file:?}");

            let main_has_table: bool = state
                .db
                .write()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) > 0 FROM sqlite_master \
                     WHERE type = 'table' AND name = 'benchmarks'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(!main_has_table, "main db keeps no benchmark content");

            crate::addon::app_db::close(&instance);
        });
    }

    /// `benchmark.write` is deny-by-default in the manifest: a reader can
    /// list but not save, and the denial comes from the matrix, not RBAC.
    #[test]
    fn write_gate_denies_readers() {
        crate::addon::fs_sandbox::with_tmp_home(|| {
            let state = crate::dispatch::state::AppState::for_test();
            let instance = super::super::app_gate::test_support::install_app(
                &state,
                PACKAGE_ID,
                &[PERM_READ],
            );
            let ctx = ctx_for(&state);

            list_v1(&ctx).expect("read default allows list");
            let err = save_v1(&ctx, None, "x", "{}", vec![]).expect_err("write denied");
            assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);

            crate::addon::app_db::close(&instance);
        });
    }
}
