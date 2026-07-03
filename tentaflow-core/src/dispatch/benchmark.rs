// =============================================================================
// Plik: dispatch/benchmark.rs
// Opis: Handlery binarnego API Benchmark Studio — definicje benchmarków, targety,
//       start/anulowanie runów (nieblokujące), historia i wyniki. Sekrety
//       (api_key) nigdy nie wracają w wire; run leci w tokio::spawn, a jego
//       progres jest re-emitowany przez współdzieloną szynę logów (log_bus),
//       którą subskrybuje streaming handler `BenchmarkRunStreamRequest`.
// Przykład: BenchmarkPayload::StartRunRequest → StartRunResponse { run_id }.
// =============================================================================

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use parking_lot::RwLock;
use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    BenchmarkPayload, BenchmarkSummaryWire, BenchmarkWire, MessageBody, ProtocolError,
    ProtocolErrorCode, ResultRowWire, RunSummaryWire, TargetInputWire, TargetWire,
};

use super::HandlerContext;
use crate::benchmark::types::{
    BenchEvent, BenchmarkConfig, BenchmarkListItem, BenchmarkRecord, BenchmarkResultRecord,
    BenchmarkRunRecord, BenchmarkTargetRecord, BenchmarkTargetUpsert,
};
use crate::db::repository;
use crate::deploy::log_bus::{self, BusMessage, LogLine};
use crate::services::rbac::OrgContext;

const PERM_READ: &str = "benchmark.read";
const PERM_WRITE: &str = "benchmark.write";
const RECENT_RUNS_LIMIT: u32 = 50;

/// Rejestr aktywnych runów: run_id → flaga anulowania. Proces-globalny (wzorzec
/// jak `log_bus::BUS`), więc StartRun nie musi dotykać AppState. Wpis powstaje
/// przy starcie runu i znika, gdy zadanie się kończy; Cancel ustawia flagę.
fn cancel_registry() -> &'static RwLock<HashMap<String, Arc<AtomicBool>>> {
    static REG: OnceLock<RwLock<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    REG.get_or_init(|| RwLock::new(HashMap::new()))
}

fn register_cancel(run_id: &str) -> Arc<AtomicBool> {
    let token = Arc::new(AtomicBool::new(false));
    cancel_registry()
        .write()
        .insert(run_id.to_string(), token.clone());
    token
}

fn unregister_cancel(run_id: &str) {
    cancel_registry().write().remove(run_id);
}

fn signal_cancel(run_id: &str) -> bool {
    match cancel_registry().read().get(run_id) {
        Some(token) => {
            token.store(true, Ordering::Relaxed);
            true
        }
        None => false,
    }
}

fn require_org(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    ctx.org_context
        .as_ref()
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required"))
}

fn require_read(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    let org = require_org(ctx)?;
    if !org.has(PERM_READ) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "benchmark.read permission required",
        ));
    }
    Ok(org)
}

fn require_write(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    let org = require_org(ctx)?;
    if !org.has(PERM_WRITE) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "benchmark.write permission required",
        ));
    }
    Ok(org)
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

fn benchmark_to_wire(record: BenchmarkRecord, targets: Vec<BenchmarkTargetRecord>) -> BenchmarkWire {
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

register_benchmark_variant!("BenchmarkListRequest", "tentaflow_ws_handler_benchmark_list");
register_benchmark_variant!("BenchmarkGetRequest", "tentaflow_ws_handler_benchmark_get");
register_benchmark_variant!("BenchmarkSaveRequest", "tentaflow_ws_handler_benchmark_save");
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
    let org = require_read(ctx)?;
    let items = repository::list_benchmarks(&ctx.state.db, &org.org_id)
        .map_err(|e| db_error("list", e))?;
    let benchmarks = items.into_iter().map(item_to_wire).collect();
    Ok(MessageBody::BenchmarkBody(BenchmarkPayload::ListResponse {
        benchmarks,
    }))
}

fn get_v1(ctx: &HandlerContext, id: &str) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, targets) = repository::get_benchmark(&ctx.state.db, &org.org_id, id)
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
    let org = require_write(ctx)?;
    if name.trim().is_empty() {
        return Err(ProtocolError::bad_request("benchmark name is required"));
    }
    // Odrzuć niepoprawny config wcześnie — inaczej run wywali się dopiero przy starcie.
    serde_json::from_str::<BenchmarkConfig>(config_json)
        .map_err(|e| ProtocolError::bad_request(format!("invalid config_json: {e}")))?;

    let id = id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let upserts: Vec<BenchmarkTargetUpsert> = targets.into_iter().map(input_to_upsert).collect();
    repository::upsert_benchmark(
        &ctx.state.db,
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
    let org = require_write(ctx)?;
    let ok = repository::delete_benchmark(&ctx.state.db, &org.org_id, id)
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
    let org = require_write(ctx)?;
    // Zapewnij, że benchmark istnieje w tej org i ma targety, zanim otworzymy run.
    let (_record, targets) = repository::get_benchmark(&ctx.state.db, &org.org_id, benchmark_id)
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
    let run_id = repository::create_benchmark_run(&ctx.state.db, benchmark_id, &engine_meta)
        .map_err(|e| db_error("start_run.create", e))?;

    let cancel = register_cancel(&run_id);
    let db = ctx.state.db.clone();
    let cipher = ctx.state.settings_cipher.clone();
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
                    cancel,
                    progress,
                )
                .await
            })
        };
        if let Err(join_err) = run.await {
            if join_err.is_panic() {
                let _ = repository::finish_benchmark_run(
                    &db,
                    &run_id_task,
                    "failed",
                    Some("benchmark task panicked"),
                );
            }
        }

        // Terminal z realnym statusem/błędem z DB (success | failed | cancelled).
        let (status, error) = match repository::get_benchmark_run(&db, &org_id, &run_id_task) {
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
    let org = require_read(ctx)?;
    let run = repository::get_benchmark_run(&ctx.state.db, &org.org_id, run_id)
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
    let org = require_read(ctx)?;
    let rows = repository::get_benchmark_run_results(&ctx.state.db, &org.org_id, run_id)
        .map_err(|e| db_error("run_results", e))?;
    let results = rows.into_iter().map(result_to_wire).collect();
    Ok(MessageBody::BenchmarkBody(
        BenchmarkPayload::RunResultsResponse { results },
    ))
}

fn list_runs_v1(ctx: &HandlerContext, benchmark_id: &str) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let runs = repository::list_benchmark_runs(&ctx.state.db, &org.org_id, benchmark_id)
        .map_err(|e| db_error("list_runs", e))?
        .iter()
        .map(|r| run_to_wire(r, None))
        .collect();
    Ok(MessageBody::BenchmarkBody(
        BenchmarkPayload::ListRunsResponse { runs },
    ))
}

fn recent_runs_v1(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let runs = repository::list_recent_benchmark_runs(&ctx.state.db, &org.org_id, RECENT_RUNS_LIMIT)
        .map_err(|e| db_error("recent_runs", e))?
        .iter()
        .map(|(run, name)| run_to_wire(run, Some(name.clone())))
        .collect();
    Ok(MessageBody::BenchmarkBody(
        BenchmarkPayload::RecentRunsResponse { runs },
    ))
}

fn cancel_run_v1(ctx: &HandlerContext, run_id: &str) -> Result<MessageBody, ProtocolError> {
    let org = require_write(ctx)?;
    // Autoryzacja + istnienie runu w org: nie pozwól sygnalizować cudzych runów.
    let run = repository::get_benchmark_run(&ctx.state.db, &org.org_id, run_id)
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
