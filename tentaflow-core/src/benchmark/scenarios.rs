// ===== File: benchmark/scenarios.rs — the four benchmark scenarios: latency, throughput sweep, context sweep, sustained load =====

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;

use super::client::BenchClient;
use super::prompt::synthetic_prompt;
use super::types::{
    ContextConfig, LatencyConfig, RequestSample, SustainedConfig, TargetSpec, ThroughputConfig,
};

/// Process-global salt handed to `synthetic_prompt` so every single benchmark
/// request (warmups included) gets a unique prompt. WHY: a repeated prompt lets
/// the LLM server restore its prefill KV-cache checkpoint, which would make TTFT
/// and prefill_tps report a cached prefill instead of the real one.
static PROMPT_SALT: AtomicU64 = AtomicU64::new(0);

fn next_salt() -> u64 {
    PROMPT_SALT.fetch_add(1, Ordering::Relaxed)
}

/// One variant's raw outcome; the runner aggregates and persists it.
pub struct VariantOutcome {
    pub variant: serde_json::Value,
    pub samples: Vec<RequestSample>,
}

fn cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Relaxed)
}

/// Czeka na workery, ale ABORTUJE je gdy tylko `cancel` sie zapali — inaczej
/// wiszacy in-flight request (np. c=64 przy nasyconym GPU) trzyma Stop przez
/// caly `request_timeout` (do 120 s * per_worker). Watcher-task pollowa cancel co
/// 150 ms i ubija wszystkie handle, wiec Stop jest responsywny nawet w srodku
/// requestu. Zwraca surowe `Result` per worker — kazdy scenariusz sam decyduje
/// co zrobic z aborted (JoinError::is_cancelled) vs realnym panic.
async fn join_workers_cancellable<T: Send + 'static>(
    handles: Vec<tokio::task::JoinHandle<T>>,
    cancel: &Arc<AtomicBool>,
) -> Vec<Result<T, tokio::task::JoinError>> {
    let aborts: Vec<tokio::task::AbortHandle> = handles.iter().map(|h| h.abort_handle()).collect();
    let watcher = {
        let cancel = Arc::clone(cancel);
        tokio::spawn(async move {
            while !cancelled(&cancel) {
                tokio::time::sleep(Duration::from_millis(150)).await;
            }
            for a in &aborts {
                a.abort();
            }
        })
    };
    let mut out = Vec::with_capacity(handles.len());
    for handle in handles {
        out.push(handle.await);
    }
    watcher.abort();
    out
}

/// Single-request latency: one uncounted warmup, then N sequential repeats.
/// Sequential on purpose — this scenario measures uncontended latency.
pub async fn run_latency(
    client: &BenchClient,
    target: &TargetSpec,
    cfg: &LatencyConfig,
    prompt_tokens: u32,
    gen_tokens: u32,
    timeout: Duration,
    cancel: &AtomicBool,
) -> VariantOutcome {
    let mut samples = Vec::with_capacity(cfg.repeats as usize);
    if !cancelled(cancel) {
        let prompt = synthetic_prompt(prompt_tokens, next_salt());
        let _warmup = client.execute(target, &prompt, gen_tokens, timeout).await;
    }
    for _ in 0..cfg.repeats {
        if cancelled(cancel) {
            break;
        }
        let prompt = synthetic_prompt(prompt_tokens, next_salt());
        samples.push(client.execute(target, &prompt, gen_tokens, timeout).await);
    }
    VariantOutcome {
        variant: json!({"prompt_tokens": prompt_tokens, "gen_tokens": gen_tokens}),
        samples,
    }
}

/// Concurrency sweep: for each level C, C parallel workers each run M requests
/// (total C×M). Aggregate throughput = Σ completion_tokens / wall time and is
/// carried in variant_json (per-request stats live in the sample aggregates).
pub async fn run_throughput(
    client: &BenchClient,
    target: &TargetSpec,
    cfg: &ThroughputConfig,
    prompt_tokens: u32,
    gen_tokens: u32,
    timeout: Duration,
    cancel: &Arc<AtomicBool>,
) -> Vec<VariantOutcome> {
    let mut outcomes = Vec::with_capacity(cfg.levels.len());
    for &level in &cfg.levels {
        if cancelled(cancel) {
            break;
        }
        let level = level.max(1);
        let wall_start = Instant::now();
        let mut handles = Vec::with_capacity(level as usize);
        for _ in 0..level {
            let client = client.clone();
            let target = target.clone();
            let cancel = Arc::clone(cancel);
            let per_worker = cfg.requests_per_worker.max(1);
            handles.push(tokio::spawn(async move {
                let mut out = Vec::with_capacity(per_worker as usize);
                for _ in 0..per_worker {
                    if cancelled(&cancel) {
                        break;
                    }
                    let prompt = synthetic_prompt(prompt_tokens, next_salt());
                    out.push(client.execute(&target, &prompt, gen_tokens, timeout).await);
                }
                out
            }));
        }
        let mut samples = Vec::new();
        for r in join_workers_cancellable(handles, cancel).await {
            match r {
                Ok(worker_samples) => samples.extend(worker_samples),
                // Aborted przez Stop — pomijamy (nie liczymy jako blad).
                Err(e) if e.is_cancelled() => {}
                Err(e) => samples.push(RequestSample::failed(0.0, format!("worker join: {e}"))),
            }
        }
        let wall_secs = wall_start.elapsed().as_secs_f64();
        let total_completion: u64 = samples.iter().map(|s| s.completion_tokens as u64).sum();
        let throughput_tps = if wall_secs > 0.0 && total_completion > 0 {
            Some(total_completion as f64 / wall_secs)
        } else {
            None
        };
        outcomes.push(VariantOutcome {
            variant: json!({
                "concurrency": level,
                "prompt_tokens": prompt_tokens,
                "gen_tokens": gen_tokens,
                "throughput_tps": throughput_tps,
                "wall_secs": wall_secs,
            }),
            samples,
        });
    }
    outcomes
}

/// Context sweep: for each prompt length P, one uncounted warmup + N repeats.
/// Measures how TTFT/decode degrade as the prompt grows.
pub async fn run_context(
    client: &BenchClient,
    target: &TargetSpec,
    cfg: &ContextConfig,
    gen_tokens: u32,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Vec<VariantOutcome> {
    let mut outcomes = Vec::with_capacity(cfg.prompt_lengths.len());
    for &prompt_len in &cfg.prompt_lengths {
        if cancelled(cancel) {
            break;
        }
        let mut samples = Vec::with_capacity(cfg.repeats as usize);
        let warmup_prompt = synthetic_prompt(prompt_len, next_salt());
        let _warmup = client
            .execute(target, &warmup_prompt, gen_tokens, timeout)
            .await;
        for _ in 0..cfg.repeats {
            if cancelled(cancel) {
                break;
            }
            let prompt = synthetic_prompt(prompt_len, next_salt());
            samples.push(client.execute(target, &prompt, gen_tokens, timeout).await);
        }
        outcomes.push(VariantOutcome {
            variant: json!({"prompt_tokens": prompt_len, "gen_tokens": gen_tokens}),
            samples,
        });
    }
    outcomes
}

/// Sustained load: C workers loop for T minutes; each sample is tagged with the
/// minute (at request start) so aggregates form a stability timeline.
pub async fn run_sustained(
    client: &BenchClient,
    target: &TargetSpec,
    cfg: &SustainedConfig,
    prompt_tokens: u32,
    gen_tokens: u32,
    timeout: Duration,
    cancel: &Arc<AtomicBool>,
) -> Vec<VariantOutcome> {
    let concurrency = cfg.concurrency.max(1);
    let planned_secs = u64::from(cfg.minutes.max(1)) * 60;
    let run_start = Instant::now();
    let deadline = run_start + Duration::from_secs(planned_secs);

    let mut handles = Vec::with_capacity(concurrency as usize);
    for _ in 0..concurrency {
        let client = client.clone();
        let target = target.clone();
        let cancel = Arc::clone(cancel);
        handles.push(tokio::spawn(async move {
            let mut out: Vec<(u32, RequestSample)> = Vec::new();
            loop {
                let now = Instant::now();
                if now >= deadline || cancelled(&cancel) {
                    break;
                }
                // Clip the per-request deadline to the time left in the window so
                // an in-flight request cannot overrun the sustained duration by up
                // to a full request_timeout.
                let remaining = deadline.saturating_duration_since(now);
                let req_timeout = timeout.min(remaining);
                let minute = (now.duration_since(run_start).as_secs() / 60) as u32;
                let prompt = synthetic_prompt(prompt_tokens, next_salt());
                out.push((
                    minute,
                    client
                        .execute(&target, &prompt, gen_tokens, req_timeout)
                        .await,
                ));
            }
            out
        }));
    }

    let mut per_minute: std::collections::BTreeMap<u32, Vec<RequestSample>> = Default::default();
    for r in join_workers_cancellable(handles, cancel).await {
        match r {
            Ok(tagged) => {
                for (minute, sample) in tagged {
                    per_minute.entry(minute).or_default().push(sample);
                }
            }
            // Aborted przez Stop — pomijamy.
            Err(e) if e.is_cancelled() => {}
            Err(e) => {
                per_minute
                    .entry(0)
                    .or_default()
                    .push(RequestSample::failed(0.0, format!("worker join: {e}")));
            }
        }
    }
    let actual_secs = run_start.elapsed().as_secs_f64();

    per_minute
        .into_iter()
        .map(|(minute, samples)| VariantOutcome {
            variant: json!({
                "minute": minute,
                "concurrency": concurrency,
                "prompt_tokens": prompt_tokens,
                "gen_tokens": gen_tokens,
                "planned_duration_secs": planned_secs,
                "actual_duration_secs": actual_secs,
            }),
            samples,
        })
        .collect()
}
