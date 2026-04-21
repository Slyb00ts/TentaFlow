// =============================================================================
// Plik: metrics/collector.rs
// Opis: Background task zbierajacy metryki: tokens/s co sekunde, stats serwisow
//       co 5 sekund. Dziala w tle na tokio::spawn.
// =============================================================================

use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::time::{interval, Duration};
use tracing::debug;

use super::RouterMetrics;
use crate::db::DbPool;

/// Kolektor metryk dzialajacy w tle.
///
/// Periodycznie oblicza tokens/s i (w przyszlosci) odpytuje serwisy
/// o statystyki CPU/GPU/pamiec.
pub struct MetricsCollector {
    metrics: Arc<RouterMetrics>,
    db: Option<DbPool>,
}

impl MetricsCollector {
    pub fn new(metrics: Arc<RouterMetrics>, db: Option<DbPool>) -> Self {
        Self { metrics, db }
    }

    /// Uruchamia background task zbierajacy metryki.
    ///
    /// Spawnuje dwa taski tokio, oba respektuja shutdown_rx — bez tego
    /// loopy `tick.tick()` nigdy sie nie koncza i blokuja tokio runtime drop.
    pub async fn start(&self, mut shutdown_rx: tokio::sync::watch::Receiver<bool>) {
        let metrics_tps = Arc::clone(&self.metrics);
        let mut sh1 = shutdown_rx.clone();
        tokio::spawn(async move {
            let mut tick = interval(Duration::from_secs(1));
            let mut prev_output_tokens: u64 =
                metrics_tps.total_output_tokens.load(Ordering::Relaxed);
            let mut prev_input_tokens: u64 = metrics_tps.total_input_tokens.load(Ordering::Relaxed);

            loop {
                tokio::select! {
                    biased;
                    _ = sh1.changed() => {
                        if *sh1.borrow() {
                            debug!("MetricsCollector: tps task shutdown");
                            return;
                        }
                    }
                    _ = tick.tick() => {
                        let current = metrics_tps.total_output_tokens.load(Ordering::Relaxed);
                        let diff = current.saturating_sub(prev_output_tokens);
                        metrics_tps.tokens_last_second.store(diff, Ordering::Relaxed);
                        prev_output_tokens = current;

                        let current_input = metrics_tps.total_input_tokens.load(Ordering::Relaxed);
                        let input_diff = current_input.saturating_sub(prev_input_tokens);
                        metrics_tps.input_tokens_last_second.store(input_diff, Ordering::Relaxed);
                        prev_input_tokens = current_input;

                        debug!("tokens/s: out={}, in={}", diff, input_diff);
                    }
                }
            }
        });

        let metrics_stats = Arc::clone(&self.metrics);
        let db_for_stats = self.db.clone();
        tokio::spawn(async move {
            let mut tick = interval(Duration::from_secs(5));

            loop {
                tokio::select! {
                    biased;
                    _ = shutdown_rx.changed() => {
                        if *shutdown_rx.borrow() {
                            debug!("MetricsCollector: stats task shutdown");
                            return;
                        }
                    }
                    _ = tick.tick() => {
                        if let Some(ref db) = db_for_stats {
                            if let Ok(svcs) = crate::db::repository::list_services(db) {
                                let active = svcs.iter().filter(|s| s.status == "active").count();
                                metrics_stats.set_active_services(active as u64);
                            }
                        }

                        let stats = metrics_stats.service_stats.read();
                        debug!(
                            "metryki serwisow: {} serwisow zarejestrowanych",
                            stats.len()
                        );
                    }
                }
            }
        });
    }
}
