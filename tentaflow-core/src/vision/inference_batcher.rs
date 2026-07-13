// =============================================================================
// File: vision/inference_batcher.rs — cross-camera dynamic-batching front-end
// =============================================================================
//
// A Triton-style dynamic batcher. Every camera's cold-path enrichment submits
// its crops here instead of calling a model's batched API directly, so crops
// from ALL cameras aggregate into ONE big batched forward per model. This turns
// "thousands of tiny per-camera batch-1 forwards" (kernel-launch + CPU<->GPU
// transfer bound, GPU idle) into a few large forwards that saturate the GPU.
//
// One worker thread per model owns the queue: it blocks for the first job, then
// drains up to `max_batch` jobs OR until `window` elapses, runs the model's
// existing batched API ONCE on the whole collected batch, and routes each result
// back to the exact job that submitted it via that job's private response
// channel — never by position across cameras. On a batch error every waiter in
// the batch gets the error; no submitter is ever dropped or left hanging.
//
// Scoped to the ort/TensorRT path (`inference-supertonic`): the session pools
// are `&self` + Send+Sync and take a batch dim (TRT profile 1..=16), so the
// worker can call `classify_batch`/`read_batch` straight off its own thread. The
// Burn/wgpu path must serialize forwards on the single wgpu thread and is left on
// its existing `classify_batch_local`/`read_batch_local` route.

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use crossbeam_channel::{bounded, unbounded, Receiver, RecvTimeoutError, Sender};
use tokio::sync::OnceCell;
use tracing::warn;

use super::classifier_stan::StateClassifier;
use super::ocr_plate::PlateOcr;
use crate::services::camera_ingest::vision_analysis::{get_classifier, get_ocr};

/// Max jobs per batched forward. Matches the models' TensorRT dynamic profile
/// upper bound (1..=16) — a bigger batch would fall out of the profile and force
/// a slow rebuild, so this is the natural aggregation cap.
pub const MAX_BATCH: usize = 16;

/// Longest a partially filled batch waits for more jobs before it is flushed.
/// Tunable via `[vision] batch_window_us` (microseconds). A wider window
/// aggregates more crops per forward (higher GPU efficiency) at the cost of more
/// per-crop latency; the default keeps latency low while still coalescing the
/// bursts that many cameras produce within the same millisecond.
pub fn batch_window() -> Duration {
    Duration::from_micros(crate::vision::settings::get().batch_window_us)
}

/// One submitted crop plus the private channel its result must return on. The
/// per-job channel is the correctness anchor: a batched result is routed back by
/// this sender, never by position across a cross-camera batch.
struct Job<R> {
    crop: Arc<[u8]>,
    w: u32,
    h: u32,
    respond: Sender<Result<R>>,
}

/// Generic cross-camera dynamic batcher. `R` is the per-crop result type
/// (`Vec<String>` for state labels, `Option<String>` for a plate read). Cheap to
/// share: `submit`/`submit_all` take `&self`, so a single instance fronts every
/// camera through an `Arc`/`&'static`.
pub struct InferenceBatcher<R> {
    job_tx: Sender<Job<R>>,
    // Kept so the batcher owns its workers; on drop the sender closes and the
    // workers' `recv` returns `Err`, so the threads exit instead of leaking.
    _workers: Vec<JoinHandle<()>>,
}

impl<R> InferenceBatcher<R>
where
    R: Send + 'static,
{
    /// Builds a batcher and spawns its single worker thread. `run_batch` calls
    /// the model's existing batched API on the whole collected batch and must
    /// return exactly one result per input crop, in order.
    pub fn new(
        max_batch: usize,
        window: Duration,
        run_batch: Arc<dyn Fn(&[(Arc<[u8]>, u32, u32)]) -> Result<Vec<R>> + Send + Sync>,
    ) -> Self {
        Self::with_workers(max_batch, window, 1, run_batch)
    }

    /// Like [`new`], but with N workers draining ONE shared queue: while worker A's
    /// forward runs on the GPU, worker B collects and launches the next batch, so
    /// consecutive batched forwards PIPELINE across the model's session pool
    /// instead of serializing on a single thread. Only useful when the session
    /// pool has ≥ `workers` sessions — otherwise the extra workers just queue on
    /// the pool. Result routing is unchanged (per-job channels).
    pub fn with_workers(
        max_batch: usize,
        window: Duration,
        workers: usize,
        run_batch: Arc<dyn Fn(&[(Arc<[u8]>, u32, u32)]) -> Result<Vec<R>> + Send + Sync>,
    ) -> Self {
        let (job_tx, job_rx) = unbounded::<Job<R>>();
        let max_batch = max_batch.max(1);
        let workers = workers.max(1);
        let handles = (0..workers)
            .map(|i| {
                let rx = job_rx.clone();
                let rb = run_batch.clone();
                std::thread::Builder::new()
                    .name(format!("vision-inference-batcher-{i}"))
                    .spawn(move || worker_loop(rx, max_batch, window, rb))
                    .expect("spawn vision inference batcher worker")
            })
            .collect();
        Self {
            job_tx,
            _workers: handles,
        }
    }

    /// Submits one crop and blocks until its batched result returns. Call from a
    /// blocking context (e.g. inside `spawn_blocking`), never on a tokio worker.
    pub fn submit(&self, crop: Arc<[u8]>, w: u32, h: u32) -> Result<R> {
        let (tx, rx) = bounded::<Result<R>>(1);
        self.job_tx
            .send(Job {
                crop,
                w,
                h,
                respond: tx,
            })
            .map_err(|_| anyhow!("inference batcher worker gone"))?;
        rx.recv()
            .map_err(|_| anyhow!("inference batcher dropped response"))?
    }

    /// Enqueues every crop FIRST, then blocks collecting all results in order.
    /// Enqueuing before blocking is what lets a single caller's crops (and, when
    /// callers run concurrently, crops from every camera) land in the queue
    /// together and coalesce into one forward — a submit-one-block-one loop would
    /// instead flush each crop as its own batch. Result `i` corresponds to crop
    /// `i`; a per-crop error is returned in that slot without affecting the rest.
    pub fn submit_all(&self, crops: &[(Arc<[u8]>, u32, u32)]) -> Vec<Result<R>> {
        if crops.is_empty() {
            return Vec::new();
        }
        let mut receivers = Vec::with_capacity(crops.len());
        for (crop, w, h) in crops {
            let (tx, rx) = bounded::<Result<R>>(1);
            let sent = self
                .job_tx
                .send(Job {
                    crop: crop.clone(),
                    w: *w,
                    h: *h,
                    respond: tx,
                })
                .is_ok();
            receivers.push(sent.then_some(rx));
        }
        receivers
            .into_iter()
            .map(|rx| match rx {
                Some(rx) => rx
                    .recv()
                    .unwrap_or_else(|_| Err(anyhow!("inference batcher dropped response"))),
                None => Err(anyhow!("inference batcher worker gone")),
            })
            .collect()
    }
}

/// Worker loop: block for the first job, drain up to `max_batch` OR until the
/// window (measured from the first job) elapses, run `run_batch` ONCE, then route
/// each result back by index. Jobs are owned locally for the whole run — no lock
/// is held across `run_batch`, so a slow forward cannot block submitters or
/// deadlock. A `run_batch` error (or a length-mismatch contract break) errors
/// EVERY waiter in the batch; a waiter is never silently dropped.
fn worker_loop<R>(
    job_rx: Receiver<Job<R>>,
    max_batch: usize,
    window: Duration,
    run_batch: Arc<dyn Fn(&[(Arc<[u8]>, u32, u32)]) -> Result<Vec<R>> + Send + Sync>,
) where
    R: Send + 'static,
{
    while let Ok(first) = job_rx.recv() {
        let deadline = Instant::now() + window;
        let mut batch: Vec<Job<R>> = Vec::with_capacity(max_batch);
        batch.push(first);
        while batch.len() < max_batch {
            match job_rx.recv_deadline(deadline) {
                Ok(job) => batch.push(job),
                // Window elapsed (flush what we have) or the sender is gone
                // (drain this batch, then the outer `recv` ends the loop).
                Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        let crops: Vec<(Arc<[u8]>, u32, u32)> =
            batch.iter().map(|j| (j.crop.clone(), j.w, j.h)).collect();

        match run_batch(&crops) {
            Ok(results) if results.len() == batch.len() => {
                for (job, r) in batch.into_iter().zip(results) {
                    let _ = job.respond.send(Ok(r));
                }
            }
            Ok(results) => {
                let n = results.len();
                let m = batch.len();
                warn!(
                    "[inference_batcher] run_batch returned {n} results for {m} jobs (contract broken)"
                );
                for job in batch {
                    let _ = job.respond.send(Err(anyhow!(
                        "inference batcher: {n} results for {m} jobs (contract broken)"
                    )));
                }
            }
            Err(e) => {
                let msg = format!("{e:#}");
                warn!("[inference_batcher] batched forward failed: {msg}");
                for job in batch {
                    let _ = job
                        .respond
                        .send(Err(anyhow!("inference batcher forward failed: {msg}")));
                }
            }
        }
    }
}

/// Process-wide state-classifier batcher, lazily built on first use over the
/// process classifier singleton. `None` when the classifier failed to load —
/// callers then take their per-crop fallback (never lose enrichment).
pub(crate) async fn state_batcher() -> Option<&'static InferenceBatcher<Vec<String>>> {
    static B: OnceCell<Option<InferenceBatcher<Vec<String>>>> = OnceCell::const_new();
    B.get_or_init(|| async {
        let classifier: Arc<StateClassifier> = get_classifier().await?;
        // GPU-resident preprocess (fused resize+normalize on GPU + device tensor,
        // parity-verified, ~5.5x faster than the CPU preprocess). Falls back to the
        // CPU classify_batch on error so a preprocess/CUDA failure never drops a
        // batch — same runtime-fallback contract as the zero-copy paths.
        let run_batch: Arc<
            dyn Fn(&[(Arc<[u8]>, u32, u32)]) -> Result<Vec<Vec<String>>> + Send + Sync,
        > = Arc::new(move |crops| {
            let refs: Vec<(&[u8], u32, u32)> =
                crops.iter().map(|(c, w, h)| (c.as_ref(), *w, *h)).collect();
            classifier.classify_batch_gpu(&refs).or_else(|e| {
                tracing::warn!("state batcher: GPU preprocess failed ({e}); CPU fallback");
                classifier.classify_batch(crops)
            })
        });
        Some(InferenceBatcher::new(MAX_BATCH, batch_window(), run_batch))
    })
    .await
    .as_ref()
}

/// Process-wide plate-OCR batcher, lazily built on first use over the process
/// OCR singleton. `None` when the OCR engine failed to load — callers then take
/// their per-crop fallback.
pub(crate) async fn plate_batcher() -> Option<&'static InferenceBatcher<(Option<String>, f32)>> {
    static B: OnceCell<Option<InferenceBatcher<(Option<String>, f32)>>> = OnceCell::const_new();
    B.get_or_init(|| async {
        let ocr: Arc<PlateOcr> = get_ocr().await?;
        let run_batch: Arc<
            dyn Fn(&[(Arc<[u8]>, u32, u32)]) -> Result<Vec<(Option<String>, f32)>> + Send + Sync,
        > = Arc::new(move |crops| ocr.read_batch(crops));
        Some(InferenceBatcher::new(MAX_BATCH, batch_window(), run_batch))
    })
    .await
    .as_ref()
}

/// Process-wide ADR-OCR batcher, lazily built on first use over the process ADR
/// singleton. Every submitted placard crop coalesces with placards from all
/// other cameras into ONE cross-placard `read_adr_batch` forward (all rows of
/// all placards in a single `[total_rows,1,32,128]` tensor), instead of one
/// small 2-row forward per placard. `R` is the raw `(kemler, un, confidence)`
/// read (confidence 0..1) — the caller applies the same `snap_adr` catalog snap
/// as the per-crop path and keeps the confidence for the vote gate. `None`
/// when the ADR model is unavailable — callers then take the per-crop executor
/// path (which keeps the PP-OCRv5 fallback).
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
pub(crate) async fn adr_batcher() -> Option<&'static InferenceBatcher<Option<(String, String, f32)>>>
{
    static B: OnceCell<Option<InferenceBatcher<Option<(String, String, f32)>>>> =
        OnceCell::const_new();
    B.get_or_init(|| async {
        // `adr_ocr::get` builds the ort/TRT session pool on first call — keep
        // that heavy load off the async worker.
        let adr: Arc<crate::vision::adr_ocr::AdrOcr> =
            tokio::task::spawn_blocking(crate::vision::adr_ocr::get)
                .await
                .ok()
                .flatten()?;
        let run_batch: Arc<
            dyn Fn(&[(Arc<[u8]>, u32, u32)]) -> Result<Vec<Option<(String, String, f32)>>>
                + Send
                + Sync,
        > = Arc::new(move |crops| Ok(adr.read_adr_batch(crops)));
        Some(InferenceBatcher::new(MAX_BATCH, batch_window(), run_batch))
    })
    .await
    .as_ref()
}
