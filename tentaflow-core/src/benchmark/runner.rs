// ===== File: benchmark/runner.rs — executes a benchmark run: targets × scenarios, aggregates and persists results, emits progress =====

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use tracing::warn;

use crate::crypto::SettingsCipher;
use crate::db::{repository, DbPool};

use super::client::BenchClient;
use super::scenarios::{self, VariantOutcome};
use super::stats;
use super::types::{
    ApiType, BenchEvent, BenchmarkConfig, BenchmarkResultRecord, BenchmarkTargetRecord,
    RequestSample, TargetSpec,
};

/// Progress sink; Chunk 2 bridges it onto the log bus / binary protocol.
pub type ProgressFn = Arc<dyn Fn(BenchEvent) + Send + Sync>;

/// Runs one benchmark run to completion. Targets execute strictly one after
/// another (never in parallel with each other) — concurrent targets would
/// contend for the same GPUs/network and skew every number.
/// Cancellation: `cancel` is checked between requests; a cancelled run keeps
/// the results persisted so far and finishes with status 'cancelled'.
pub async fn run_benchmark(
    db: DbPool,
    org_id: &str,
    benchmark_id: &str,
    run_id: &str,
    cipher: &SettingsCipher,
    cancel: Arc<AtomicBool>,
    progress: ProgressFn,
) -> Result<()> {
    match execute(
        &db,
        org_id,
        benchmark_id,
        run_id,
        cipher,
        &cancel,
        &progress,
    )
    .await
    {
        Ok(()) => {
            let status = if cancel.load(Ordering::Relaxed) {
                "cancelled"
            } else {
                "success"
            };
            repository::finish_benchmark_run(&db, run_id, status, None)?;
            progress(BenchEvent::Done);
            Ok(())
        }
        Err(e) => {
            let message = format!("{e:#}");
            if let Err(db_err) =
                repository::finish_benchmark_run(&db, run_id, "failed", Some(&message))
            {
                warn!("benchmark: failed to persist run failure: {db_err}");
            }
            progress(BenchEvent::Error {
                message: message.clone(),
            });
            Err(e)
        }
    }
}

async fn execute(
    db: &DbPool,
    org_id: &str,
    benchmark_id: &str,
    run_id: &str,
    cipher: &SettingsCipher,
    cancel: &Arc<AtomicBool>,
    progress: &ProgressFn,
) -> Result<()> {
    let (benchmark, targets) =
        repository::get_benchmark(db, org_id, benchmark_id)?.context("benchmark not found")?;
    let config: BenchmarkConfig =
        serde_json::from_str(&benchmark.config_json).context("invalid benchmark config_json")?;
    anyhow::ensure!(!targets.is_empty(), "benchmark has no targets");

    let client = BenchClient::new()?;
    let timeout = Duration::from_secs(config.request_timeout_secs.max(1));

    for target_rec in &targets {
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }
        let target = resolve_target(target_rec, cipher)?;
        progress(BenchEvent::Log {
            line: format!("target '{}' ({}) start", target.label, target.model),
        });

        if let Some(cfg) = &config.latency {
            run_scenario_variants(
                db,
                run_id,
                &target,
                "latency",
                progress,
                vec![
                    scenarios::run_latency(
                        &client,
                        &target,
                        cfg,
                        config.prompt_tokens,
                        config.gen_tokens,
                        timeout,
                        cancel,
                    )
                    .await,
                ],
            )?;
        }
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }

        if let Some(cfg) = &config.throughput {
            let outcomes = scenarios::run_throughput(
                &client,
                &target,
                cfg,
                config.prompt_tokens,
                config.gen_tokens,
                timeout,
                cancel,
            )
            .await;
            run_scenario_variants(db, run_id, &target, "throughput", progress, outcomes)?;
        }
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }

        if let Some(cfg) = &config.context {
            let outcomes =
                scenarios::run_context(&client, &target, cfg, config.gen_tokens, timeout, cancel)
                    .await;
            run_scenario_variants(db, run_id, &target, "context", progress, outcomes)?;
        }
        if cancel.load(Ordering::Relaxed) {
            return Ok(());
        }

        if let Some(cfg) = &config.sustained {
            let outcomes = scenarios::run_sustained(
                &client,
                &target,
                cfg,
                config.prompt_tokens,
                config.gen_tokens,
                timeout,
                cancel,
            )
            .await;
            run_scenario_variants(db, run_id, &target, "sustained", progress, outcomes)?;
        }
    }
    Ok(())
}

/// Persists each variant outcome as one result row and emits progress events.
fn run_scenario_variants(
    db: &DbPool,
    run_id: &str,
    target: &TargetSpec,
    scenario: &'static str,
    progress: &ProgressFn,
    outcomes: Vec<VariantOutcome>,
) -> Result<()> {
    progress(BenchEvent::Phase {
        target_id: target.id.clone(),
        target_label: target.label.clone(),
        scenario,
        message: format!("{} variants collected", outcomes.len()),
    });
    for outcome in outcomes {
        // A cancelled variant may come back with zero samples — nothing to store.
        if outcome.samples.is_empty() {
            continue;
        }
        let record = build_result_record(run_id, target, scenario, &outcome);
        repository::insert_benchmark_result(db, &record)?;
        progress(BenchEvent::PartialResult { result: record });
    }
    Ok(())
}

/// Aggregates raw samples into a result row. Error samples count toward
/// `errors` and the error rate but are excluded from timing aggregates.
fn build_result_record(
    run_id: &str,
    target: &TargetSpec,
    scenario: &str,
    outcome: &VariantOutcome,
) -> BenchmarkResultRecord {
    let ok: Vec<&RequestSample> = outcome
        .samples
        .iter()
        .filter(|s| s.error.is_none())
        .collect();
    let errors = (outcome.samples.len() - ok.len()) as u32;

    let ttft: Vec<f64> = ok.iter().map(|s| s.ttft_ms).collect();
    let total: Vec<f64> = ok.iter().map(|s| s.total_ms).collect();
    let prefill: Vec<f64> = ok.iter().filter_map(|s| s.prefill_tps).collect();
    let decode: Vec<f64> = ok.iter().filter_map(|s| s.decode_tps).collect();

    let ttft_agg = stats::aggregate(&ttft);
    let total_agg = stats::aggregate(&total);
    let (prefill_mean, prefill_sigma) = mean_sigma_opt(&prefill);
    let (decode_mean, decode_sigma) = mean_sigma_opt(&decode);

    BenchmarkResultRecord {
        id: uuid::Uuid::new_v4().to_string(),
        run_id: run_id.to_string(),
        target_id: target.id.clone(),
        target_label: target.label.clone(),
        scenario: scenario.to_string(),
        variant_json: outcome.variant.to_string(),
        ttft_ms_mean: ttft_agg.as_ref().map(|a| a.mean),
        ttft_ms_sigma: ttft_agg.as_ref().map(|a| a.sigma),
        prefill_tps_mean: prefill_mean,
        prefill_tps_sigma: prefill_sigma,
        decode_tps_mean: decode_mean,
        decode_tps_sigma: decode_sigma,
        total_ms_mean: total_agg.as_ref().map(|a| a.mean),
        total_ms_sigma: total_agg.as_ref().map(|a| a.sigma),
        p50_ms: total_agg.as_ref().map(|a| a.p50),
        p90_ms: total_agg.as_ref().map(|a| a.p90),
        p99_ms: total_agg.as_ref().map(|a| a.p99),
        requests: outcome.samples.len() as u32,
        errors,
        samples_json: serde_json::to_string(&outcome.samples).unwrap_or_else(|_| "[]".into()),
    }
}

fn mean_sigma_opt(values: &[f64]) -> (Option<f64>, Option<f64>) {
    if values.is_empty() {
        (None, None)
    } else {
        let (mean, sigma) = stats::mean_sigma(values);
        (Some(mean), Some(sigma))
    }
}

/// Turns a stored target row into a runtime spec: parses the API type and
/// decrypts the API key with the settings cipher (secrets never leave the DB
/// in plaintext outside a run).
fn resolve_target(rec: &BenchmarkTargetRecord, cipher: &SettingsCipher) -> Result<TargetSpec> {
    let api = ApiType::parse(&rec.api_type)
        .with_context(|| format!("unknown api_type '{}'", rec.api_type))?;
    let api_key = match &rec.api_key_enc {
        Some(enc) => Some(
            cipher
                .decrypt(enc)
                .with_context(|| format!("decrypt api key for target '{}'", rec.label))?,
        ),
        None => None,
    };
    let spec = TargetSpec {
        id: rec.id.clone(),
        label: rec.label.clone(),
        api,
        host: rec.host.clone(),
        port: rec.port,
        api_key,
        model: rec.model.clone(),
    };
    spec.validate_host()
        .with_context(|| format!("target '{}'", rec.label))?;
    Ok(spec)
}
