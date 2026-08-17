// ===== File: metrics.rs — engine observability counters/gauges/histograms (SPEC §8.3) =====
// Real state the scheduler already computes, exported as atomics so the HTTP
// layer can render a Prometheus exposition without touching the worker thread.
// Every value here is written by the engine worker from genuine events (token
// emission timing, admission, KV free-page count, speculation acceptance); no
// value is synthesized. The worker holds one `Arc<EngineMetrics>` and updates it
// in place; readers (the /metrics handler) only load.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// A fixed-bucket cumulative histogram (Prometheus semantics): `buckets[i]`
/// counts observations ≤ `BOUNDS[i]`, plus a running sum and count. Lock-free;
/// each observation touches only the matching bucket, the sum and the count.
pub struct AtomicHistogram {
    bounds: &'static [f64],
    /// One counter per upper bound; the implicit `+Inf` bucket equals `count`.
    buckets: Vec<AtomicU64>,
    /// Sum of observed values, scaled by `SUM_SCALE` and stored as an integer so
    /// the whole histogram stays lock-free (no float CAS loop).
    sum_scaled: AtomicU64,
    count: AtomicU64,
}

/// Fixed-point scale for histogram sums (µs precision on second-valued
/// observations; the exposition divides back out).
const SUM_SCALE: f64 = 1_000_000.0;

impl AtomicHistogram {
    fn new(bounds: &'static [f64]) -> Self {
        Self {
            bounds,
            buckets: (0..bounds.len()).map(|_| AtomicU64::new(0)).collect(),
            sum_scaled: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    /// Record one observation into every bucket whose bound it falls under
    /// (cumulative "le" semantics are reconstructed at render time from the
    /// per-bound counts).
    pub fn observe(&self, value: f64) {
        for (i, &b) in self.bounds.iter().enumerate() {
            if value <= b {
                self.buckets[i].fetch_add(1, Ordering::Relaxed);
            }
        }
        self.sum_scaled
            .fetch_add((value * SUM_SCALE).max(0.0) as u64, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    /// Snapshot for rendering: cumulative `(le_bound, count_le)` pairs, the total
    /// count and the total sum (unscaled).
    pub fn snapshot(&self) -> HistogramSnapshot {
        let cumulative = self
            .bounds
            .iter()
            .zip(&self.buckets)
            .map(|(&b, c)| (b, c.load(Ordering::Relaxed)))
            .collect();
        HistogramSnapshot {
            cumulative,
            count: self.count.load(Ordering::Relaxed),
            sum: self.sum_scaled.load(Ordering::Relaxed) as f64 / SUM_SCALE,
        }
    }
}

pub struct HistogramSnapshot {
    /// `(upper_bound, count ≤ bound)` in ascending bound order.
    pub cumulative: Vec<(f64, u64)>,
    pub count: u64,
    pub sum: f64,
}

// Latency buckets in seconds: sub-millisecond to ~1 min, spanning TTFT and ITL.
static LATENCY_BOUNDS: &[f64] = &[
    0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0,
];
// Decode throughput buckets in tokens/second (per completed request).
static TPS_BOUNDS: &[f64] = &[
    5.0, 10.0, 20.0, 40.0, 60.0, 80.0, 120.0, 160.0, 240.0, 320.0, 480.0, 640.0, 1000.0,
];

/// All engine metrics. Counters only grow; gauges are overwritten each scheduler
/// iteration with the worker's current view.
pub struct EngineMetrics {
    /// Requests admitted into an active slot (started decoding/prefilling).
    pub requests_started: AtomicU64,
    /// Requests that reached a terminal `Done`.
    pub requests_finished: AtomicU64,
    /// Requests that ended with an engine `Error` (admission rejection, OOM,
    /// kernel failure). Client hang-ups also route here.
    pub requests_errored: AtomicU64,
    /// Prompt tokens across all finished requests (usage.prompt_tokens).
    pub prompt_tokens_total: AtomicU64,
    /// Generated (completion) tokens across all finished requests.
    pub generated_tokens_total: AtomicU64,
    /// Prompt tokens served from the radix prefix cache (SPEC §5.2 hit).
    pub cache_read_tokens_total: AtomicU64,
    /// Speculative verify forwards run (SPEC §6); 0 when speculation is off.
    pub spec_forwards_total: AtomicU64,
    /// Speculative draft tokens accepted across those forwards.
    pub spec_accepted_total: AtomicU64,
    /// Wspólne kroki dwóch sekwencji wykonane przez natywną ścieżkę MTP B2.
    pub native_mtp_b2_steps_total: AtomicU64,
    /// Wspólne chunki targetowego prefill B2 T32.
    pub hybrid_prefill_b2_steps_total: AtomicU64,
    /// Tokeny promptu wykonane przez targetowy prefill B2 T32.
    pub hybrid_prefill_b2_tokens_total: AtomicU64,
    /// Pary prefill odrzucone przez capability i wykonane serialnie.
    pub hybrid_prefill_b2_fallbacks_total: AtomicU64,
    /// Gauge: logiczny rozmiar zaalokowanego dedykowanego scratchu prefill B2.
    pub hybrid_prefill_b2_scratch_bytes: AtomicU64,
    /// Kroki dekodowania wykonane wspólnym forwardem hybrydy.
    pub hybrid_decode_batch_steps_total: AtomicU64,
    /// Linie obsłużone przez te kroki — iloraz daje realną szerokość grupy.
    pub hybrid_decode_batch_lanes_total: AtomicU64,
    /// Wspólne weryfikacje dwóch pełnych draftów routera MTP+n-gram.
    pub mtp_ngram_b2_steps_total: AtomicU64,
    /// Wspólne weryfikacje routed B2 dla par N/N.
    pub mtp_routed_nn_b2_steps_total: AtomicU64,
    /// Wspólne weryfikacje routed B2 dla par N/M lub M/N.
    pub mtp_routed_nm_b2_steps_total: AtomicU64,
    /// Wspólne weryfikacje routed B2 dla par M/M.
    pub mtp_routed_mm_b2_steps_total: AtomicU64,
    /// Gauge: sequences currently decoding/prefilling.
    pub active_sequences: AtomicU64,
    /// Gauge: submissions admitted-but-waiting behind KV pressure (queue depth).
    pub queued_sequences: AtomicU64,
    /// Gauge: total KV pages in the VRAM pool (set once at startup).
    pub kv_pages_total: AtomicU64,
    /// Gauge: KV pages not on the free stack (held by active seqs + prefix tree).
    pub kv_pages_used: AtomicU64,
    /// Time-to-first-token per request, seconds.
    pub ttft_seconds: AtomicHistogram,
    /// Inter-token latency per decode step, seconds.
    pub inter_token_seconds: AtomicHistogram,
    /// Per-request decode throughput, tokens/second.
    pub decode_tps: AtomicHistogram,
}

impl Default for EngineMetrics {
    fn default() -> Self {
        Self {
            requests_started: AtomicU64::new(0),
            requests_finished: AtomicU64::new(0),
            requests_errored: AtomicU64::new(0),
            prompt_tokens_total: AtomicU64::new(0),
            generated_tokens_total: AtomicU64::new(0),
            cache_read_tokens_total: AtomicU64::new(0),
            spec_forwards_total: AtomicU64::new(0),
            spec_accepted_total: AtomicU64::new(0),
            native_mtp_b2_steps_total: AtomicU64::new(0),
            hybrid_prefill_b2_steps_total: AtomicU64::new(0),
            hybrid_prefill_b2_tokens_total: AtomicU64::new(0),
            hybrid_prefill_b2_fallbacks_total: AtomicU64::new(0),
            hybrid_prefill_b2_scratch_bytes: AtomicU64::new(0),
            hybrid_decode_batch_steps_total: AtomicU64::new(0),
            hybrid_decode_batch_lanes_total: AtomicU64::new(0),
            mtp_ngram_b2_steps_total: AtomicU64::new(0),
            mtp_routed_nn_b2_steps_total: AtomicU64::new(0),
            mtp_routed_nm_b2_steps_total: AtomicU64::new(0),
            mtp_routed_mm_b2_steps_total: AtomicU64::new(0),
            active_sequences: AtomicU64::new(0),
            queued_sequences: AtomicU64::new(0),
            kv_pages_total: AtomicU64::new(0),
            kv_pages_used: AtomicU64::new(0),
            ttft_seconds: AtomicHistogram::new(LATENCY_BOUNDS),
            // Inter-token latency shares the seconds-valued latency buckets.
            inter_token_seconds: AtomicHistogram::new(LATENCY_BOUNDS),
            decode_tps: AtomicHistogram::new(TPS_BOUNDS),
        }
    }
}

impl EngineMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn inc(counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    pub fn add(counter: &AtomicU64, n: u64) {
        counter.fetch_add(n, Ordering::Relaxed);
    }

    #[inline]
    pub fn set(gauge: &AtomicU64, v: u64) {
        gauge.store(v, Ordering::Relaxed);
    }
}

/// Per-sequence timing the worker threads through an `ActiveSeq` to feed the
/// TTFT / inter-token / decode-tps histograms without any allocation.
pub struct SeqTiming {
    admitted_at: Instant,
    first_token_at: Option<Instant>,
    last_token_at: Option<Instant>,
    tokens: u64,
}

impl SeqTiming {
    pub fn new() -> Self {
        Self {
            admitted_at: Instant::now(),
            first_token_at: None,
            last_token_at: None,
            tokens: 0,
        }
    }

    /// Record one produced token. The first call measures TTFT (admission →
    /// first token); later calls measure the inter-token gap.
    pub fn record_token(&mut self, m: &EngineMetrics) {
        let now = Instant::now();
        match self.last_token_at {
            None => {
                self.first_token_at = Some(now);
                m.ttft_seconds
                    .observe(now.duration_since(self.admitted_at).as_secs_f64());
            }
            Some(prev) => {
                m.inter_token_seconds
                    .observe(now.duration_since(prev).as_secs_f64());
            }
        }
        self.last_token_at = Some(now);
        self.tokens += 1;
    }

    /// At sequence teardown, record the decode throughput (tokens after the
    /// first, over the decode span). Needs ≥2 tokens for a meaningful rate.
    pub fn record_decode_tps(&self, m: &EngineMetrics) {
        if let (Some(first), Some(last)) = (self.first_token_at, self.last_token_at) {
            let span = last.duration_since(first).as_secs_f64();
            if self.tokens >= 2 && span > 0.0 {
                m.decode_tps.observe((self.tokens - 1) as f64 / span);
            }
        }
    }
}

impl Default for SeqTiming {
    fn default() -> Self {
        Self::new()
    }
}
