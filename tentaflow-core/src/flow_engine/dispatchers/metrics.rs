// =============================================================================
// Plik: flow_engine/dispatchers/metrics.rs
// Opis: MetricsSink — narrow trait dla metrics emit z adapter pipeline.
//       Stage 1: tylko trait + NoopMetrics. Realna integracja z
//       metrics-prometheus dochodzi jak będziemy mierzyć per-node latency.
// =============================================================================

use std::time::Duration;

pub trait MetricsSink: Send + Sync {
    fn record_node_duration(&self, node_type: &str, duration: Duration);
    fn record_node_error(&self, node_type: &str, error_kind: &str);
    fn increment_counter(&self, name: &str, labels: &[(&str, &str)]);
}

pub struct NoopMetrics;

impl MetricsSink for NoopMetrics {
    fn record_node_duration(&self, _node_type: &str, _duration: Duration) {}
    fn record_node_error(&self, _node_type: &str, _error_kind: &str) {}
    fn increment_counter(&self, _name: &str, _labels: &[(&str, &str)]) {}
}
