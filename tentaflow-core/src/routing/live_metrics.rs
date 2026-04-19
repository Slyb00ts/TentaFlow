// =============================================================================
// Plik: routing/live_metrics.rs
// Opis: Globalny snapshot live-metrics routera — liczba trwajacych requestow
//       i aktualny throughput tokenow/s (sliding window 10s). Uzywane przez
//       mesh heartbeat do propagacji statystyk po calym mesh.
// =============================================================================

use parking_lot::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

static ACTIVE_REQUESTS: AtomicU32 = AtomicU32::new(0);
static TOKENS_WINDOW: OnceLock<Mutex<TokensWindow>> = OnceLock::new();

const WINDOW_SECONDS: f32 = 10.0;

struct TokensWindow {
    samples: Vec<(Instant, u64)>,
}

fn window() -> &'static Mutex<TokensWindow> {
    TOKENS_WINDOW.get_or_init(|| Mutex::new(TokensWindow { samples: Vec::new() }))
}

/// Guard RAII — zwieksza licznik active przy tworzeniu, zmniejsza przy drop.
pub struct ActiveRequestGuard;

impl ActiveRequestGuard {
    pub fn new() -> Self {
        ACTIVE_REQUESTS.fetch_add(1, Ordering::Relaxed);
        Self
    }
}

impl Default for ActiveRequestGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        ACTIVE_REQUESTS.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Rejestruje N wygenerowanych tokenow (LLM completion) z aktualnym timestampem.
/// Stare probki (>10s) sa usuwane przy kazdym push.
pub fn record_tokens(n: u64) {
    if n == 0 {
        return;
    }
    let now = Instant::now();
    let mut w = window().lock();
    w.samples.push((now, n));
    let cutoff = now
        .checked_sub(std::time::Duration::from_secs_f32(WINDOW_SECONDS))
        .unwrap_or(now);
    w.samples.retain(|(ts, _)| *ts >= cutoff);
}

/// Zwraca `(active_requests, tokens_per_sec)` obliczone ze sliding window.
pub fn snapshot() -> (u32, f32) {
    let active = ACTIVE_REQUESTS.load(Ordering::Relaxed);
    let now = Instant::now();
    let mut w = window().lock();
    let cutoff = now
        .checked_sub(std::time::Duration::from_secs_f32(WINDOW_SECONDS))
        .unwrap_or(now);
    w.samples.retain(|(ts, _)| *ts >= cutoff);
    let sum: u64 = w.samples.iter().map(|(_, n)| *n).sum();
    let tps = sum as f32 / WINDOW_SECONDS;
    (active, tps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_guard_roundtrip() {
        let (before, _) = snapshot();
        let _g1 = ActiveRequestGuard::new();
        let _g2 = ActiveRequestGuard::new();
        let (during, _) = snapshot();
        assert_eq!(during, before + 2);
        drop(_g1);
        drop(_g2);
        let (after, _) = snapshot();
        assert_eq!(after, before);
    }

    #[test]
    fn tokens_window_decays() {
        record_tokens(100);
        let (_, tps) = snapshot();
        assert!(tps > 0.0);
    }
}
