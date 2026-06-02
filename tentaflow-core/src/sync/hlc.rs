// =============================================================================
// Plik: sync/hlc.rs
// Opis: Zegar Hybrid Logical Clock generujacy monotoniczne znaczniki HLC dla
//       operacji Sync Ledgera i przyjmujacy znaczniki zdalne (observe).
// =============================================================================

use parking_lot::Mutex;

use super::ledger::HybridLogicalTimestamp;

/// Stan wewnetrzny zegara HLC: ostatni wyemitowany / zaobserwowany czas
/// scienny (ms) oraz licznik logiczny w obrebie tego samego ms.
#[derive(Debug, Clone, Copy)]
struct HlcState {
    wall_time_ms: i64,
    logical: u32,
}

/// Monotoniczny generator znacznikow HLC dla pojedynczego noda.
///
/// `now()` nigdy nie cofa znacznika nawet gdy fizyczny zegar systemowy
/// przeskoczy wstecz (NTP), a `observe()` podbija stan ponad znacznik
/// odebrany od peera, zeby kolejne lokalne operacje byly zawsze pozniejsze
/// niz to, co juz widzielismy w sieci.
pub struct HlcClock {
    node_id: String,
    state: Mutex<HlcState>,
}

impl HlcClock {
    /// Tworzy zegar dla danego noda. `initial` pozwala wznowic stan po
    /// restarcie (faza B podepnie persystencje); `None` startuje od zera.
    pub fn new(node_id: impl Into<String>, initial: Option<HybridLogicalTimestamp>) -> Self {
        let state = initial.map_or(
            HlcState {
                wall_time_ms: 0,
                logical: 0,
            },
            |ts| HlcState {
                wall_time_ms: ts.wall_time_ms,
                logical: ts.logical,
            },
        );
        Self {
            node_id: node_id.into(),
            state: Mutex::new(state),
        }
    }

    /// Identyfikator noda przypisany do tego zegara.
    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    /// Zwraca kolejny, scisle rosnacy znacznik HLC.
    pub fn now(&self) -> HybridLogicalTimestamp {
        let mut state = self.state.lock();
        let physical = super::runtime::now_ms();
        if physical > state.wall_time_ms {
            state.wall_time_ms = physical;
            state.logical = 0;
        } else {
            // Same millisecond or a backwards NTP jump: keep the wall time and
            // bump the logical counter, carrying into the wall time on overflow
            // so the timestamp stays strictly greater (a saturating bump would
            // stall at u32::MAX and break monotonicity).
            let (wall, logical) = bump_logical(state.wall_time_ms, state.logical);
            state.wall_time_ms = wall;
            state.logical = logical;
        }
        HybridLogicalTimestamp {
            wall_time_ms: state.wall_time_ms,
            logical: state.logical,
            node_id: self.node_id.clone(),
        }
    }

    /// Wlacza zdalny znacznik do lokalnego stanu, tak by nastepne `now()`
    /// bylo pozniejsze zarowno niz dotychczasowy stan, jak i niz `remote`.
    /// Nie emituje znacznika.
    pub fn observe(&self, remote: &HybridLogicalTimestamp) {
        let mut state = self.state.lock();
        let physical = super::runtime::now_ms();
        let wall = state
            .wall_time_ms
            .max(remote.wall_time_ms)
            .max(physical);

        // Pick a logical counter strictly greater than every source that shares
        // the chosen wall time. When the wall time advances past a source, that
        // source no longer constrains the counter, so it resets to 0.
        let base = match (wall == state.wall_time_ms, wall == remote.wall_time_ms) {
            (true, true) => Some(state.logical.max(remote.logical)),
            (true, false) => Some(state.logical),
            (false, true) => Some(remote.logical),
            (false, false) => None,
        };
        let (wall, logical) = match base {
            Some(value) => bump_logical(wall, value),
            None => (wall, 0),
        };
        state.wall_time_ms = wall;
        state.logical = logical;
    }
}

/// Zwraca `(wall, base + 1)`, a przy przepelnieniu u32 przenosi nadmiar do
/// czasu sciennego: `(wall + 1, 0)`. Dzieki temu kolejny znacznik jest zawsze
/// scisle wiekszy nawet gdy licznik logiczny osiagnal `u32::MAX`.
fn bump_logical(wall: i64, base: u32) -> (i64, u32) {
    match base.checked_add(1) {
        Some(next) => (wall, next),
        // Logical overflowed; carry into the wall time. When the wall time is
        // already at i64::MAX the carry is impossible, so resetting logical to 0
        // would move the timestamp *backwards* (i64::MAX, 0) < (i64::MAX, MAX).
        // At the absolute ceiling we saturate in place and keep (i64::MAX, MAX):
        // strict-greater is unrepresentable there, so non-decreasing is the
        // strongest invariant we can hold.
        None => match wall.checked_add(1) {
            Some(carried) => (carried, 0),
            None => (i64::MAX, u32::MAX),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_is_strictly_monotonic_across_calls() {
        let clock = HlcClock::new("node_a", None);
        let mut previous = clock.now();
        for _ in 0..1_000 {
            let next = clock.now();
            assert!(
                next > previous,
                "HLC must be strictly increasing: {previous:?} -> {next:?}"
            );
            previous = next;
        }
    }

    #[test]
    fn now_bumps_logical_within_same_millisecond() {
        let clock = HlcClock::new("node_a", None);
        // Two calls in immediate succession land in the same millisecond often
        // enough that at least one logical bump is guaranteed over many calls.
        let first = clock.now();
        let mut saw_same_wall_bump = false;
        let mut previous = first;
        for _ in 0..10_000 {
            let next = clock.now();
            if next.wall_time_ms == previous.wall_time_ms {
                assert!(next.logical > previous.logical);
                saw_same_wall_bump = true;
            }
            previous = next;
        }
        assert!(saw_same_wall_bump, "expected at least one same-ms logical bump");
    }

    #[test]
    fn observe_pushes_local_state_above_remote() {
        let clock = HlcClock::new("node_a", None);
        let remote = HybridLogicalTimestamp {
            wall_time_ms: i64::MAX - 10,
            logical: 5,
            node_id: "node_b".to_string(),
        };
        clock.observe(&remote);
        let next = clock.now();
        assert_eq!(next.wall_time_ms, remote.wall_time_ms);
        assert!(next.logical > remote.logical);
        assert_eq!(next.node_id, "node_a");
    }

    #[test]
    fn observe_ignores_older_remote() {
        let clock = HlcClock::new("node_a", None);
        let ahead = clock.now();
        let stale = HybridLogicalTimestamp {
            wall_time_ms: ahead.wall_time_ms - 1_000,
            logical: 99,
            node_id: "node_b".to_string(),
        };
        clock.observe(&stale);
        let next = clock.now();
        assert!(next >= ahead);
    }

    #[test]
    fn node_id_is_preserved_from_constructor() {
        let clock = HlcClock::new("desktop-7", None);
        assert_eq!(clock.node_id(), "desktop-7");
        assert_eq!(clock.now().node_id, "desktop-7");
    }

    #[test]
    fn now_stays_monotonic_across_logical_overflow() {
        // Far-future wall time forces the same-ms branch on every call, and the
        // logical counter starts one below u32::MAX so the overflow carry path
        // is exercised. The sequence must remain strictly increasing.
        let initial = HybridLogicalTimestamp {
            wall_time_ms: i64::MAX - 5,
            logical: u32::MAX - 1,
            node_id: "node_a".to_string(),
        };
        let clock = HlcClock::new("node_a", Some(initial));
        let first = clock.now();
        assert_eq!(first.logical, u32::MAX);
        let second = clock.now();
        // Logical wrapped, so the wall time must have carried forward by 1.
        assert!(second > first, "{first:?} -> {second:?}");
        assert_eq!(second.wall_time_ms, first.wall_time_ms + 1);
        assert_eq!(second.logical, 0);
    }

    #[test]
    fn observe_remote_with_max_logical_keeps_next_now_greater() {
        // A backwards/stalled physical clock plus a remote whose logical is at
        // u32::MAX must still yield a strictly greater local timestamp.
        let initial = HybridLogicalTimestamp {
            wall_time_ms: i64::MAX - 5,
            logical: 0,
            node_id: "node_a".to_string(),
        };
        let clock = HlcClock::new("node_a", Some(initial.clone()));
        let remote = HybridLogicalTimestamp {
            wall_time_ms: initial.wall_time_ms,
            logical: u32::MAX,
            node_id: "node_b".to_string(),
        };
        clock.observe(&remote);
        let next = clock.now();
        assert!(
            (next.wall_time_ms, next.logical) > (remote.wall_time_ms, remote.logical),
            "next {next:?} must exceed remote {remote:?}"
        );
    }

    #[test]
    fn now_saturates_at_absolute_ceiling_without_going_backwards() {
        // Start one logical tick below the absolute ceiling. The far-future wall
        // time forces the same-ms branch, so the first now() reaches the ceiling
        // and every subsequent call must clamp there without ever decreasing.
        let initial = HybridLogicalTimestamp {
            wall_time_ms: i64::MAX,
            logical: u32::MAX - 1,
            node_id: "node_a".to_string(),
        };
        let clock = HlcClock::new("node_a", Some(initial));
        let first = clock.now();
        assert_eq!((first.wall_time_ms, first.logical), (i64::MAX, u32::MAX));
        let mut previous = first;
        for _ in 0..100 {
            let next = clock.now();
            assert!(
                (next.wall_time_ms, next.logical) >= (previous.wall_time_ms, previous.logical),
                "HLC must never go backwards at the ceiling: {previous:?} -> {next:?}"
            );
            assert_eq!((next.wall_time_ms, next.logical), (i64::MAX, u32::MAX));
            previous = next;
        }
    }

    #[test]
    fn initial_state_resumes_logical_counter() {
        let initial = HybridLogicalTimestamp {
            wall_time_ms: i64::MAX - 10,
            logical: 3,
            node_id: "node_a".to_string(),
        };
        let clock = HlcClock::new("node_a", Some(initial.clone()));
        let next = clock.now();
        // The far-future initial wall time forces the same-ms branch, so the
        // logical counter must continue from the resumed value, not restart.
        assert_eq!(next.wall_time_ms, initial.wall_time_ms);
        assert_eq!(next.logical, 4);
    }
}
