// ===== File: speculation/stats.rs — per-proposer acceptance stats + adaptive disable =====
//! Bookkeeping per proposer per sequence: lifetime proposed/accepted totals,
//! a sliding window of recent propose calls, and the adaptive-disable rule
//! from SPEC §6: when the acceptance-adjusted speedup estimate over the
//! window drops below 1.05, the proposer sleeps for a fixed token count and
//! then retries with a fresh window.

use std::collections::VecDeque;

/// Sliding-window size in recorded (non-empty) propose calls.
pub const WINDOW_CALLS: usize = 32;
/// Sleep duration after a disable, measured in committed sequence tokens.
pub const SLEEP_TOKENS: u64 = 256;
/// Minimum estimated speedup to stay enabled.
pub const MIN_SPEEDUP: f64 = 1.05;
/// Linear-verification cost model: verifying one extra draft token costs this
/// fraction of a plain decode step. With it, an always-rejected drafter scores
/// below 1.0 and gets disabled, while a mostly-accepted one scores well above.
pub const VERIFY_COST_PER_DRAFT_TOKEN: f64 = 0.1;

#[derive(Default)]
pub struct ProposerStats {
    total_proposed: u64,
    total_accepted: u64,
    window: VecDeque<(u32, u32)>,
    sleep_remaining: u64,
}

/// Point-in-time copy for monitoring / scheduler introspection.
#[derive(Debug, Clone)]
pub struct ProposerStatsSnapshot {
    pub name: String,
    pub proposed: u64,
    pub accepted: u64,
    pub acceptance_rate: f64,
    pub sleeping: bool,
}

impl ProposerStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the outcome of one propose call. Empty proposals cost nothing
    /// and carry no signal, so they neither fill the window nor wake/penalize
    /// anything — an idle proposer must not get disabled for finding no match.
    pub fn record(&mut self, proposed: usize, accepted: usize) {
        if proposed == 0 {
            return;
        }
        self.total_proposed += proposed as u64;
        self.total_accepted += accepted as u64;
        if self.window.len() == WINDOW_CALLS {
            self.window.pop_front();
        }
        self.window.push_back((proposed as u32, accepted as u32));
        if self.window.len() == WINDOW_CALLS && self.window_speedup() < MIN_SPEEDUP {
            self.sleep_remaining = SLEEP_TOKENS;
            // Fresh window after waking, so one bad stretch is not held
            // against the proposer forever.
            self.window.clear();
        }
    }

    /// Estimated speedup of linear speculation over plain decode across the
    /// window: one verify step yields `1 + accepted` tokens and costs
    /// `1 + c * proposed` plain steps.
    fn window_speedup(&self) -> f64 {
        let calls = self.window.len() as f64;
        let proposed: u64 = self.window.iter().map(|&(p, _)| u64::from(p)).sum();
        let accepted: u64 = self.window.iter().map(|&(_, a)| u64::from(a)).sum();
        let gain = 1.0 + accepted as f64 / calls;
        let cost = 1.0 + VERIFY_COST_PER_DRAFT_TOKEN * (proposed as f64 / calls);
        gain / cost
    }

    /// Count committed sequence tokens; wakes the proposer once the sleep
    /// budget is spent.
    pub fn note_tokens(&mut self, n: usize) {
        self.sleep_remaining = self.sleep_remaining.saturating_sub(n as u64);
    }

    pub fn is_sleeping(&self) -> bool {
        self.sleep_remaining > 0
    }

    pub fn proposed(&self) -> u64 {
        self.total_proposed
    }

    pub fn accepted(&self) -> u64 {
        self.total_accepted
    }

    pub fn acceptance_rate(&self) -> f64 {
        if self.total_proposed == 0 {
            0.0
        } else {
            self.total_accepted as f64 / self.total_proposed as f64
        }
    }

    pub fn snapshot(&self, name: &str) -> ProposerStatsSnapshot {
        ProposerStatsSnapshot {
            name: name.to_string(),
            proposed: self.total_proposed,
            accepted: self.total_accepted,
            acceptance_rate: self.acceptance_rate(),
            sleeping: self.is_sleeping(),
        }
    }
}
