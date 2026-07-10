// =============================================================================
// File: protocol/frame/replay.rs — UFP/2 replay protection (§9)
// Purpose: detect and reject replayed envelopes. Two independent gates:
//   1. Time skew window: reject if |now - created_at| > 30 s (per-channel
//      override allowed; see §9).
//   2. Per-source dedup LRU keyed on (message_id, fragment_index). Each
//      source identity has its own capacity-bounded cache so a noisy peer
//      cannot evict legitimate dedup entries for other peers (§9 phrase
//      "per-source, configurable"). For non-fragmented envelopes,
//      `fragment_index` is the sentinel 0xFFFF.
//
// Atomic primitive (`try_observe`): combines check_skew + is_replay +
// caller-supplied processing + commit under the per-source mutex so the
// §9 step ordering is enforced atomically. Low-level `is_replay` and
// `commit` are also exposed for advanced callers that need to interleave
// processing across multiple modules; those callers are responsible for
// serializing the check→process→commit sequence per dedup key.
//
// Spec ref: docs/UNIFIED_FRAME_PROTOCOL_v2.md §9.
// =============================================================================

use std::collections::{HashMap, HashSet, VecDeque};
use std::num::NonZeroUsize;
use std::sync::Mutex;

use super::envelope::{Envelope, MessageId, NODE_ID_LEN};
use super::error::{FrameError, FrameErrorCode};

/// Default time skew window (§9). Receivers reject envelopes whose
/// `created_at_ms` is more than this many milliseconds away from `now_ms`.
pub const DEFAULT_CLOCK_SKEW_MS: u64 = 30_000;

/// Sentinel `fragment_index` value used in the dedup key when the envelope
/// is NOT a fragment. Safe because legitimate fragment indices satisfy
/// `fragment_index < fragment_count <= u16::MAX`, so the maximum valid
/// index is `65534` — `0xFFFF` cannot collide.
pub const NON_FRAGMENT_INDEX_SENTINEL: u16 = 0xFFFF;

/// Default capacity of the per-source dedup cache (§9).
pub const DEFAULT_DEDUP_CAPACITY_PER_SOURCE: usize = 10_000;

/// Per-source dedup key: `message_id` plus `fragment_index` (sentinel for
/// non-fragment envelopes). The source identity is the HashMap key in the
/// outer per-source structure.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DedupKey {
    pub message_id: MessageId,
    pub fragment_index: u16,
}

impl DedupKey {
    pub fn from_envelope(envelope: &Envelope) -> Self {
        Self {
            message_id: envelope.message_id,
            fragment_index: envelope
                .fragment_index
                .unwrap_or(NON_FRAGMENT_INDEX_SENTINEL),
        }
    }
}

/// Replay protection state. Maintains a per-source bounded dedup cache and
/// the configured time-skew tolerance. Thread-safe via a single outer mutex.
pub struct ReplayGuard {
    capacity_per_source: NonZeroUsize,
    skew_ms: u64,
    state: Mutex<GuardState>,
}

struct GuardState {
    per_source: HashMap<[u8; NODE_ID_LEN], SourceCache>,
}

struct SourceCache {
    /// Already-committed dedup keys for this source.
    committed: HashMap<DedupKey, ()>,
    /// Insertion order over `committed` — used for capacity eviction.
    order: VecDeque<DedupKey>,
    /// Keys currently being processed (reserved but not yet committed).
    /// A second concurrent arrival with a matching key sees the reservation
    /// and is rejected as a replay; this gives `try_observe` atomicity for
    /// concurrent same-key processing.
    in_flight: HashSet<DedupKey>,
}

impl SourceCache {
    fn new() -> Self {
        Self {
            committed: HashMap::new(),
            order: VecDeque::new(),
            in_flight: HashSet::new(),
        }
    }
}

impl ReplayGuard {
    /// Construct a fresh guard with the given per-source LRU capacity and
    /// time-skew window (in milliseconds).
    pub fn new(capacity_per_source: NonZeroUsize, skew_ms: u64) -> Self {
        Self {
            capacity_per_source,
            skew_ms,
            state: Mutex::new(GuardState {
                per_source: HashMap::new(),
            }),
        }
    }

    /// Default guard: 10 000 entries per source, 30 s skew window (§9 defaults).
    pub fn with_defaults() -> Self {
        Self::new(
            NonZeroUsize::new(DEFAULT_DEDUP_CAPACITY_PER_SOURCE)
                .expect("default capacity is non-zero"),
            DEFAULT_CLOCK_SKEW_MS,
        )
    }

    pub fn skew_ms(&self) -> u64 {
        self.skew_ms
    }

    pub fn capacity_per_source(&self) -> usize {
        self.capacity_per_source.get()
    }

    /// Step 5 of the §9 receive pipeline. Returns `ClockSkewExceeded` if
    /// `|now_ms - envelope.created_at_ms| > skew_ms`. Per-channel callers
    /// MAY pass an alternate window via `check_clock_skew_with`.
    pub fn check_clock_skew(&self, envelope: &Envelope, now_ms: u64) -> Result<(), FrameError> {
        check_clock_skew_with(envelope, now_ms, self.skew_ms)
    }

    /// Step 6 of the §9 receive pipeline (low-level). Returns
    /// `ReplayDetected` if the envelope's dedup key is already in the
    /// committed LRU OR currently in-flight. Does NOT insert — the caller
    /// MUST call `commit` only after processing succeeds (§9 step 8).
    ///
    /// For concurrent processing of the same dedup key, prefer
    /// `try_observe` which atomically reserves, processes, and commits.
    /// Standalone `is_replay` is correct only when the caller serialises
    /// `is_replay → processing → commit` per dedup key.
    pub fn is_replay(&self, envelope: &Envelope) -> Result<(), FrameError> {
        let key = DedupKey::from_envelope(envelope);
        let state = self.state.lock().expect("replay state mutex poisoned");
        if let Some(cache) = state.per_source.get(&envelope.source.id) {
            if cache.committed.contains_key(&key) || cache.in_flight.contains(&key) {
                return Err(FrameError::new(
                    FrameErrorCode::ReplayDetected,
                    "is_replay: (source.id, message_id, fragment_index) already seen in dedup window",
                ));
            }
        }
        Ok(())
    }

    /// Step 8 of the §9 receive pipeline (low-level). Commit the envelope's
    /// dedup key to the per-source LRU. Evicts the oldest entry for this
    /// source when capacity is reached. Idempotent: re-committing the same
    /// key is a no-op.
    pub fn commit(&self, envelope: &Envelope) -> Result<(), FrameError> {
        let key = DedupKey::from_envelope(envelope);
        let mut state = self.state.lock().expect("replay state mutex poisoned");
        let cache = state
            .per_source
            .entry(envelope.source.id)
            .or_insert_with(SourceCache::new);
        // Even if low-level callers misuse the API and call commit without a
        // prior reservation, clear the in_flight marker for this key (which
        // is harmless if absent) so try_observe-style concurrent guards
        // cannot leak.
        cache.in_flight.remove(&key);
        if cache.committed.contains_key(&key) {
            return Ok(());
        }
        if cache.order.len() >= self.capacity_per_source.get() {
            if let Some(evicted) = cache.order.pop_front() {
                cache.committed.remove(&evicted);
            }
        }
        cache.committed.insert(key.clone(), ());
        cache.order.push_back(key);
        Ok(())
    }

    /// Atomically: check clock skew, check + reserve dedup key, run the
    /// caller-supplied processing closure, then commit-on-success OR
    /// release-reservation-on-failure. This is the recommended public API
    /// for §9 step-ordered receive paths.
    ///
    /// The closure receives the envelope unchanged and returns its own
    /// `Result<R, FrameError>`. The outer mutex is held only across check
    /// + reservation + commit; the closure runs WITHOUT holding the mutex,
    /// so multiple distinct dedup keys can process in parallel. Concurrent
    /// processing of the SAME dedup key is rejected on the second arrival
    /// because the first has an active reservation.
    pub fn try_observe<F, R>(
        &self,
        envelope: &Envelope,
        now_ms: u64,
        processing: F,
    ) -> Result<R, FrameError>
    where
        F: FnOnce(&Envelope) -> Result<R, FrameError>,
    {
        self.check_clock_skew(envelope, now_ms)?;
        let key = DedupKey::from_envelope(envelope);
        {
            let mut state = self.state.lock().expect("replay state mutex poisoned");
            let cache = state
                .per_source
                .entry(envelope.source.id)
                .or_insert_with(SourceCache::new);
            if cache.committed.contains_key(&key) || cache.in_flight.contains(&key) {
                return Err(FrameError::new(
                    FrameErrorCode::ReplayDetected,
                    "try_observe: (source.id, message_id, fragment_index) already seen in dedup window",
                ));
            }
            cache.in_flight.insert(key.clone());
        }

        // RAII reservation: even if `processing` panics and the panic is
        // caught upstream (e.g. by a thread pool worker), Drop releases the
        // in_flight slot so the key does not leak.
        let reservation = Reservation {
            guard: self,
            source_id: envelope.source.id,
            key: key.clone(),
            committed: false,
        };

        let outcome = processing(envelope);

        match outcome {
            Ok(value) => {
                let mut state = self.state.lock().expect("replay state mutex poisoned");
                let cache = state
                    .per_source
                    .get_mut(&envelope.source.id)
                    .expect("source cache present after reservation");
                cache.in_flight.remove(&key);
                if !cache.committed.contains_key(&key) {
                    if cache.order.len() >= self.capacity_per_source.get() {
                        if let Some(evicted) = cache.order.pop_front() {
                            cache.committed.remove(&evicted);
                        }
                    }
                    cache.committed.insert(key.clone(), ());
                    cache.order.push_back(key);
                }
                // Mark committed so Drop becomes a no-op for the success path
                // (success cleanup already ran above under the same lock).
                core::mem::forget(reservation);
                Ok(value)
            }
            Err(e) => {
                // Drop releases the reservation as part of normal stack unwind.
                drop(reservation);
                Err(e)
            }
        }
    }

    /// Total committed entries across all sources (test/diagnostic helper).
    pub fn committed_count(&self) -> usize {
        self.state
            .lock()
            .map(|s| s.per_source.values().map(|c| c.committed.len()).sum())
            .unwrap_or(0)
    }

    /// Number of source-specific caches currently held.
    pub fn source_count(&self) -> usize {
        self.state.lock().map(|s| s.per_source.len()).unwrap_or(0)
    }
}

/// RAII guard that releases an in-flight reservation if dropped without
/// being explicitly forgotten. Used internally by `try_observe` to keep
/// the in_flight set clean even if `processing` panics and the panic is
/// caught upstream.
struct Reservation<'a> {
    guard: &'a ReplayGuard,
    source_id: [u8; NODE_ID_LEN],
    key: DedupKey,
    #[allow(dead_code)]
    committed: bool,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.guard.state.lock() {
            if let Some(cache) = state.per_source.get_mut(&self.source_id) {
                cache.in_flight.remove(&self.key);
            }
        }
    }
}

/// Stateless time-skew helper. Computed in `i128` to handle the `u64` /
/// `i64` mix without overflow and to support arbitrary clock offsets.
pub fn check_clock_skew_with(
    envelope: &Envelope,
    now_ms: u64,
    skew_ms: u64,
) -> Result<(), FrameError> {
    let created = envelope.created_at_ms;
    let delta_ms = if (now_ms as i128) >= (created as i128) {
        (now_ms as i128) - (created as i128)
    } else {
        (created as i128) - (now_ms as i128)
    };
    if delta_ms > skew_ms as i128 {
        return Err(FrameError::new(
            FrameErrorCode::ClockSkewExceeded,
            format!(
                "check_clock_skew: |now({}) - created({})| = {}ms > skew_ms({})",
                now_ms, created, delta_ms, skew_ms
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::frame::address::NodeAddress;
    use crate::protocol::frame::channel::{channels, Kind};
    use crate::protocol::frame::envelope::{MessageId, Priority, MESSAGE_ID_LEN};
    use crate::protocol::frame::flags::Flags;

    fn sample_envelope_with(message_id_byte0: u8, fragment_index: Option<u16>) -> Envelope {
        let mut mid = [0u8; MESSAGE_ID_LEN];
        mid[0] = message_id_byte0;
        let mut env = Envelope::minimal(
            NodeAddress::node([0x11u8; NODE_ID_LEN]),
            NodeAddress::node([0x22u8; NODE_ID_LEN]),
            channels::FRONTEND,
            Kind(0x0001),
            Priority::Normal,
            Flags::NONE,
            MessageId(mid),
            1_700_000_000_000,
        );
        if let Some(idx) = fragment_index {
            env.flags = env.flags.with(Flags::IS_FRAGMENT);
            env.fragment_index = Some(idx);
            env.fragment_count = Some(idx + 1);
            if idx == 0 {
                env.flags = env.flags.with(Flags::IS_LAST_FRAGMENT);
            }
        }
        env
    }

    #[test]
    fn clock_skew_inside_window_passes() {
        let g = ReplayGuard::with_defaults();
        let env = sample_envelope_with(1, None);
        g.check_clock_skew(&env, 1_700_000_000_000).unwrap();
        g.check_clock_skew(&env, 1_700_000_029_999).unwrap();
        g.check_clock_skew(&env, 1_699_999_970_001).unwrap();
    }

    #[test]
    fn clock_skew_past_window_rejects() {
        let g = ReplayGuard::with_defaults();
        let env = sample_envelope_with(1, None);
        let r = g.check_clock_skew(&env, 1_700_000_031_000);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::ClockSkewExceeded);
    }

    #[test]
    fn clock_skew_future_past_window_rejects() {
        let g = ReplayGuard::with_defaults();
        let env = sample_envelope_with(1, None);
        let r = g.check_clock_skew(&env, 1_699_999_960_000);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::ClockSkewExceeded);
    }

    #[test]
    fn per_channel_skew_override_uses_caller_value() {
        let env = sample_envelope_with(1, None);
        check_clock_skew_with(&env, 1_700_000_004_999, 5_000).unwrap();
        let r = check_clock_skew_with(&env, 1_700_000_005_001, 5_000);
        assert!(r.is_err());
    }

    #[test]
    fn first_arrival_is_not_replay() {
        let g = ReplayGuard::with_defaults();
        let env = sample_envelope_with(1, None);
        g.is_replay(&env).unwrap();
    }

    #[test]
    fn second_arrival_after_commit_is_replay() {
        let g = ReplayGuard::with_defaults();
        let env = sample_envelope_with(1, None);
        g.is_replay(&env).unwrap();
        g.commit(&env).unwrap();
        let r = g.is_replay(&env);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::ReplayDetected);
    }

    #[test]
    fn pre_commit_arrival_is_not_replay_yet() {
        let g = ReplayGuard::with_defaults();
        let env = sample_envelope_with(1, None);
        g.is_replay(&env).unwrap();
        g.is_replay(&env).unwrap();
        assert_eq!(g.committed_count(), 0);
    }

    #[test]
    fn different_message_ids_do_not_collide() {
        let g = ReplayGuard::with_defaults();
        let env_a = sample_envelope_with(1, None);
        let env_b = sample_envelope_with(2, None);
        g.commit(&env_a).unwrap();
        g.is_replay(&env_b).unwrap();
    }

    #[test]
    fn different_fragment_indices_do_not_collide() {
        let g = ReplayGuard::with_defaults();
        let mut env_a = sample_envelope_with(1, Some(0));
        env_a.fragment_count = Some(3);
        env_a.flags = env_a.flags.without(Flags::IS_LAST_FRAGMENT);
        let mut env_b = sample_envelope_with(1, Some(1));
        env_b.fragment_count = Some(3);
        env_b.flags = env_b.flags.without(Flags::IS_LAST_FRAGMENT);
        g.commit(&env_a).unwrap();
        g.is_replay(&env_b).unwrap();
        g.commit(&env_b).unwrap();
        let r = g.is_replay(&env_a);
        assert!(r.is_err());
    }

    #[test]
    fn non_fragment_uses_sentinel_in_dedup_key() {
        let env = sample_envelope_with(1, None);
        let key = DedupKey::from_envelope(&env);
        assert_eq!(key.fragment_index, NON_FRAGMENT_INDEX_SENTINEL);
    }

    #[test]
    fn fragment_zero_does_not_collide_with_non_fragment() {
        let g = ReplayGuard::with_defaults();
        let env_non_frag = sample_envelope_with(1, None);
        let env_frag0 = sample_envelope_with(1, Some(0));
        g.commit(&env_non_frag).unwrap();
        g.is_replay(&env_frag0).unwrap();
    }

    #[test]
    fn lru_capacity_evicts_oldest_when_full() {
        let g = ReplayGuard::new(NonZeroUsize::new(3).unwrap(), DEFAULT_CLOCK_SKEW_MS);
        let envs: Vec<_> = (1..=4)
            .map(|b| sample_envelope_with(b as u8, None))
            .collect();
        for e in &envs[..3] {
            g.commit(e).unwrap();
        }
        assert_eq!(g.committed_count(), 3);
        g.commit(&envs[3]).unwrap();
        assert_eq!(g.committed_count(), 3);
        g.is_replay(&envs[0]).unwrap();
        let r = g.is_replay(&envs[3]);
        assert!(r.is_err());
    }

    #[test]
    fn commit_is_idempotent() {
        let g = ReplayGuard::with_defaults();
        let env = sample_envelope_with(1, None);
        g.commit(&env).unwrap();
        g.commit(&env).unwrap();
        g.commit(&env).unwrap();
        assert_eq!(g.committed_count(), 1);
    }

    #[test]
    fn different_sources_do_not_collide() {
        let g = ReplayGuard::with_defaults();
        let env_a = sample_envelope_with(1, None);
        let mut env_b = sample_envelope_with(1, None);
        env_b.source = NodeAddress::node([0xAAu8; NODE_ID_LEN]);
        g.commit(&env_a).unwrap();
        g.is_replay(&env_b).unwrap();
    }

    #[test]
    fn per_source_capacity_does_not_evict_other_sources() {
        // Capacity 2 per source. Source A fills its cap; source B's entries
        // remain intact.
        let g = ReplayGuard::new(NonZeroUsize::new(2).unwrap(), DEFAULT_CLOCK_SKEW_MS);
        let mut env_a1 = sample_envelope_with(1, None);
        let mut env_a2 = sample_envelope_with(2, None);
        let mut env_a3 = sample_envelope_with(3, None);
        let mut env_b1 = sample_envelope_with(1, None);
        env_b1.source = NodeAddress::node([0xBBu8; NODE_ID_LEN]);
        env_a1.source = NodeAddress::node([0xAAu8; NODE_ID_LEN]);
        env_a2.source = env_a1.source.clone();
        env_a3.source = env_a1.source.clone();

        g.commit(&env_b1).unwrap();
        g.commit(&env_a1).unwrap();
        g.commit(&env_a2).unwrap();
        g.commit(&env_a3).unwrap(); // evicts env_a1 from source A's cache only

        // env_b1 still flagged (source B's cache untouched).
        assert!(g.is_replay(&env_b1).is_err());
        // env_a1 has been evicted from source A's cache.
        g.is_replay(&env_a1).unwrap();
    }

    #[test]
    fn try_observe_commits_on_success() {
        let g = ReplayGuard::with_defaults();
        let env = sample_envelope_with(1, None);
        let result = g
            .try_observe(&env, 1_700_000_000_000, |_| Ok(42u32))
            .unwrap();
        assert_eq!(result, 42);
        // Second arrival is a replay.
        let r = g.try_observe(&env, 1_700_000_000_000, |_| Ok(0u32));
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::ReplayDetected);
    }

    #[test]
    fn try_observe_releases_reservation_on_failure() {
        let g = ReplayGuard::with_defaults();
        let env = sample_envelope_with(1, None);
        let r = g.try_observe(&env, 1_700_000_000_000, |_| {
            Err(FrameError::new(
                FrameErrorCode::DecryptionFailed,
                "simulated processing failure",
            )) as Result<u32, FrameError>
        });
        assert!(r.is_err());
        // Reservation released → retry can succeed.
        g.try_observe(&env, 1_700_000_000_000, |_| Ok(7u32))
            .unwrap();
    }

    #[test]
    fn try_observe_rejects_concurrent_same_key_via_reservation() {
        let g = ReplayGuard::with_defaults();
        let env = sample_envelope_with(1, None);
        // Simulate a long-running processing closure that reserves the key.
        // We can't truly run two threads in a doc-style unit test, but we
        // can observe the reservation effect by detecting another arrival
        // inside the closure.
        let r = g.try_observe(&env, 1_700_000_000_000, |inside_env| {
            let nested = g.is_replay(inside_env);
            assert!(
                nested.is_err(),
                "in-flight reservation MUST block concurrent same-key"
            );
            Ok(0u32)
        });
        r.unwrap();
    }

    #[test]
    fn try_observe_releases_reservation_on_panic() {
        use std::panic::AssertUnwindSafe;
        let g = ReplayGuard::with_defaults();
        let env = sample_envelope_with(1, None);
        let r = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _ = g.try_observe(&env, 1_700_000_000_000, |_| -> Result<u32, FrameError> {
                panic!("simulated processing panic");
            });
        }));
        assert!(r.is_err());
        // After the panic, in_flight set is clean (Drop ran), so a retry
        // succeeds.
        g.try_observe(&env, 1_700_000_000_000, |_| Ok(0u32))
            .unwrap();
    }

    #[test]
    fn try_observe_rejects_clock_skew_before_reservation() {
        let g = ReplayGuard::with_defaults();
        let env = sample_envelope_with(1, None);
        let r = g.try_observe(&env, 1_700_000_999_999, |_| Ok(0u32));
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, FrameErrorCode::ClockSkewExceeded);
        // No reservation leaked → retry within window works.
        g.try_observe(&env, 1_700_000_000_000, |_| Ok(0u32))
            .unwrap();
    }
}
