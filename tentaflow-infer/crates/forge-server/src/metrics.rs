// ===== File: metrics.rs — Prometheus text exposition for the FORGE server (SPEC §8.3) =====
// Renders the engine's live counters/gauges/histograms plus HTTP-level request
// counts as a Prometheus 0.0.4 text exposition. Every value comes from real
// engine/HTTP state (forge_engine::metrics + HttpMetrics); nothing is synthetic.
// The /metrics route is served outside the API-key gate (like /healthz) so a
// standard scraper reaches it without a bearer token.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use forge_engine::metrics::{EngineMetrics, HistogramSnapshot};

/// HTTP-facing request counts, keyed by matched route + status code. A single
/// mutex guards the map; it is touched once per request (negligible next to a
/// forward pass), which keeps the reader (`render`) a simple consistent snapshot.
#[derive(Default)]
pub struct HttpMetrics {
    counts: Mutex<BTreeMap<(String, u16), u64>>,
}

impl HttpMetrics {
    /// Record one completed HTTP request against its matched route template
    /// (e.g. `/v1/chat/completions`) and final status code.
    pub fn record(&self, route: &str, status: u16) {
        let mut map = self.counts.lock().expect("http metrics mutex");
        *map.entry((route.to_string(), status)).or_insert(0) += 1;
    }

    fn snapshot(&self) -> Vec<((String, u16), u64)> {
        self.counts
            .lock()
            .expect("http metrics mutex")
            .iter()
            .map(|(k, &v)| (k.clone(), v))
            .collect()
    }
}

fn counter(out: &mut String, name: &str, help: &str, value: u64) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} counter");
    let _ = writeln!(out, "{name} {value}");
}

fn gauge(out: &mut String, name: &str, help: &str, value: u64) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} gauge");
    let _ = writeln!(out, "{name} {value}");
}

/// Render one histogram in Prometheus cumulative-bucket form: a `_bucket{le=...}`
/// line per bound, a `+Inf` bucket equal to the count, plus `_sum` and `_count`.
fn histogram(out: &mut String, name: &str, help: &str, snap: &HistogramSnapshot) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} histogram");
    for (bound, count) in &snap.cumulative {
        let _ = writeln!(out, "{name}_bucket{{le=\"{bound}\"}} {count}");
    }
    let _ = writeln!(out, "{name}_bucket{{le=\"+Inf\"}} {}", snap.count);
    let _ = writeln!(out, "{name}_sum {}", fmt_f64(snap.sum));
    let _ = writeln!(out, "{name}_count {}", snap.count);
}

/// Prometheus wants a plain decimal (no scientific notation for these ranges).
fn fmt_f64(v: f64) -> String {
    format!("{v:.6}")
}

/// Escape a label value per the exposition format (backslash, quote, newline).
fn escape_label(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

/// Build the full exposition body from the engine metrics and HTTP counts.
pub fn render(engine: &EngineMetrics, http: &HttpMetrics, model_id: &str) -> String {
    let mut out = String::with_capacity(4096);
    let load = |c: &AtomicU64| c.load(Ordering::Relaxed);

    // ---- HTTP request counts by route + status ----
    let http_counts = http.snapshot();
    if !http_counts.is_empty() {
        out.push_str("# HELP forge_http_requests_total HTTP requests by route and status code.\n");
        out.push_str("# TYPE forge_http_requests_total counter\n");
        for ((route, status), value) in http_counts {
            let _ = writeln!(
                out,
                "forge_http_requests_total{{route=\"{}\",status=\"{status}\"}} {value}",
                escape_label(&route)
            );
        }
    }

    // ---- Request lifecycle counters ----
    counter(
        &mut out,
        "forge_engine_requests_started_total",
        "Requests admitted into an active engine slot.",
        load(&engine.requests_started),
    );
    counter(
        &mut out,
        "forge_engine_requests_finished_total",
        "Requests that completed with a terminal Done.",
        load(&engine.requests_finished),
    );
    counter(
        &mut out,
        "forge_engine_requests_errored_total",
        "Requests that ended in an engine error, rejection or client hang-up.",
        load(&engine.requests_errored),
    );

    // ---- Token counters ----
    counter(
        &mut out,
        "forge_engine_prompt_tokens_total",
        "Prompt tokens across all finished requests.",
        load(&engine.prompt_tokens_total),
    );
    counter(
        &mut out,
        "forge_engine_generated_tokens_total",
        "Generated completion tokens across all finished requests.",
        load(&engine.generated_tokens_total),
    );
    counter(
        &mut out,
        "forge_engine_cache_read_tokens_total",
        "Prompt tokens served from the radix prefix cache (SPEC §5.2).",
        load(&engine.cache_read_tokens_total),
    );

    // ---- Speculation counters (SPEC §6) ----
    counter(
        &mut out,
        "forge_engine_speculative_forwards_total",
        "Speculative verify forwards run (0 when speculation is off).",
        load(&engine.spec_forwards_total),
    );
    counter(
        &mut out,
        "forge_engine_speculative_accepted_total",
        "Speculative draft tokens accepted across verify forwards.",
        load(&engine.spec_accepted_total),
    );
    counter(
        &mut out,
        "forge_engine_native_mtp_b2_steps_total",
        "Native MTP steps executed by the paired B2 fast path.",
        load(&engine.native_mtp_b2_steps_total),
    );
    counter(
        &mut out,
        "forge_engine_mtp_ngram_b2_steps_total",
        "MTP+n-gram verifies executed by the paired N/N B2 fast path.",
        load(&engine.mtp_ngram_b2_steps_total),
    );
    counter(
        &mut out,
        "forge_engine_mtp_routed_nn_b2_steps_total",
        "Routed MTP B2 verifies for N/N source pairs.",
        load(&engine.mtp_routed_nn_b2_steps_total),
    );
    counter(
        &mut out,
        "forge_engine_mtp_routed_nm_b2_steps_total",
        "Routed MTP B2 verifies for N/M or M/N source pairs.",
        load(&engine.mtp_routed_nm_b2_steps_total),
    );
    counter(
        &mut out,
        "forge_engine_mtp_routed_mm_b2_steps_total",
        "Routed MTP B2 verifies for M/M source pairs.",
        load(&engine.mtp_routed_mm_b2_steps_total),
    );

    // ---- Gauges ----
    gauge(
        &mut out,
        "forge_engine_active_sequences",
        "Sequences currently decoding or prefilling.",
        load(&engine.active_sequences),
    );
    gauge(
        &mut out,
        "forge_engine_queued_sequences",
        "Admitted-but-waiting submissions behind KV pressure.",
        load(&engine.queued_sequences),
    );
    gauge(
        &mut out,
        "forge_engine_kv_pages_total",
        "Total KV pages in the VRAM pool.",
        load(&engine.kv_pages_total),
    );
    gauge(
        &mut out,
        "forge_engine_kv_pages_used",
        "KV pages held by active sequences and the prefix tree.",
        load(&engine.kv_pages_used),
    );

    // ---- Histograms ----
    histogram(
        &mut out,
        "forge_engine_ttft_seconds",
        "Time-to-first-token per request, seconds.",
        &engine.ttft_seconds.snapshot(),
    );
    histogram(
        &mut out,
        "forge_engine_inter_token_seconds",
        "Inter-token latency per decode step, seconds.",
        &engine.inter_token_seconds.snapshot(),
    );
    histogram(
        &mut out,
        "forge_engine_decode_tokens_per_second",
        "Per-request decode throughput, tokens/second.",
        &engine.decode_tps.snapshot(),
    );

    // Served model id as a labeled info gauge, so a scrape self-identifies.
    let _ = writeln!(out, "# HELP forge_build_info Served model identity.");
    let _ = writeln!(out, "# TYPE forge_build_info gauge");
    let _ = writeln!(
        out,
        "forge_build_info{{model=\"{}\"}} 1",
        escape_label(model_id)
    );

    out
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::{render, HttpMetrics};
    use forge_engine::metrics::EngineMetrics;

    #[test]
    fn prometheus_eksponuje_licznik_mtp_ngram_b2() {
        let engine = EngineMetrics::new();
        engine.mtp_ngram_b2_steps_total.store(7, Ordering::Relaxed);
        engine.mtp_routed_nn_b2_steps_total.store(3, Ordering::Relaxed);
        engine.mtp_routed_nm_b2_steps_total.store(2, Ordering::Relaxed);
        engine.mtp_routed_mm_b2_steps_total.store(1, Ordering::Relaxed);
        let output = render(&engine, &HttpMetrics::default(), "test");
        assert!(output.contains("# TYPE forge_engine_mtp_ngram_b2_steps_total counter"));
        assert!(output.contains("forge_engine_mtp_ngram_b2_steps_total 7"));
        assert!(output.contains("forge_engine_mtp_routed_nn_b2_steps_total 3"));
        assert!(output.contains("forge_engine_mtp_routed_nm_b2_steps_total 2"));
        assert!(output.contains("forge_engine_mtp_routed_mm_b2_steps_total 1"));
    }
}
