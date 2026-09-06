// =============================================================================
// File: bus/replication/election.rs — M2 promotion state machine (PLAN-M2 §1b)
// =============================================================================
//
// Pure functions + a pure state machine, table-tested (PLAN-M2 §1g). Nothing
// in this file performs I/O, reads the wall clock, or spawns a task — every
// timestamp a caller needs (`Instant` for deadlines, `i64` ms for the
// resulting `PartitionAssignment.updated_at_ms`) is threaded through as
// event data, and `PromotionState::step` returns the actions the manager
// (`manager.rs`) must execute; it never executes them itself.
//
// K-M2-1..K-M2-3 (PLAN-M2 §0), decided by the coordinator before this wave
// started:
//   K-M2-1: `hw` is monotonic/durable per partition. A newly promoted leader
//           starts from ITS OWN persisted `hw`, never `min(leo of ISR)`
//           (that would move `hw` backwards past records a consumer already
//           read). `Truncate` targets only replicas whose `leo` is AHEAD of
//           the new leader's own `leo` (in practice: the old leader,
//           rejoining with an un-replicated tail).
//   K-M2-2: `min_isr_required(rf) = floor(rf/2)+1`, computed from the
//           REPLICA SET, not the (fast-shrinking) ISR — so `acks=quorum`
//           never silently degrades to `acks=leader` as ISR shrinks.
//   K-M2-3: a follower cannot see another follower's `leo` (it only talks
//           to the leader), so a candidate queries the other replicas
//           directly (`ReplLeoQuery`/`ReplLeoReply`, 300 ms timeout) before
//           proposing. Only a node in the LAST assignment's ISR may become
//           a candidate; ties break on the lowest `node_id` (mirrors
//           `sync/core_baseline.rs::decide_roles`'s tie-break pattern).
//
// Split-brain safety (M2-R2, PLAN-M2 §4.2) does NOT depend on any of the
// above being followed correctly — it comes entirely from
// `admitted_by_majority` (a majority of the REPLICA set must have
// acknowledged the ledger operation, PLAN-M2 §1c) plus the materializer's
// epoch-monotonic admission gate (agent L, `core_materializer.rs`, out of
// this file's scope). LeoQuery/tie-break only improve which node wins and
// how fast — never whether an unsafe promotion can succeed.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::bus::replication::assignment::PartitionAssignment;
use crate::sync::ledger::OperationId;

/// K-M2-3: candidate's LeoQuery round trip budget.
pub const LEO_QUERY_TIMEOUT: Duration = Duration::from_millis(300);

/// How long a candidate waits for `admitted_by_majority` after proposing,
/// before giving up and letting the caller retry later. PLAN-M2 does not
/// pin an exact number for this step; 1.5 s is chosen so a majority that is
/// merely slow to pull/ack the op — not genuinely unreachable — still has a
/// realistic chance within one attempt, while a hung attempt does not block
/// the follower's retry loop for long. Callers may use a different deadline
/// (it is threaded through `PromotionEvent::Proposed`/`Timeout`, never read
/// from this constant by `step` itself).
pub const MAJORITY_AWAIT_TIMEOUT: Duration = Duration::from_millis(1500);

/// K-M2-2: minimum ISR size required to accept writes at `acks=quorum`,
/// computed from the REPLICA SET (`replication_factor`), never from the
/// current ISR size — see this module's header for why. The same formula
/// (`floor(rf/2)+1`) is also the majority threshold `admitted_by_majority`
/// applies to the replica set for promotion admission (PLAN-M2 §1c).
pub fn min_isr_required(replication_factor: usize) -> usize {
    replication_factor / 2 + 1
}

/// Next leader epoch. Saturates instead of wrapping: an epoch that has
/// climbed to `u32::MAX` (billions of failovers on one partition) already
/// signals something else is badly wrong, and wrapping back to 0 would let
/// a long-dead old leader's stale epoch look current again.
pub fn next_epoch(current: u32) -> u32 {
    current.saturating_add(1)
}

/// K-M2-3: picks the promotion candidate from `leos` (one entry per replica
/// that answered a `LeoQuery`, `(node_id, leo)`), restricted to members of
/// `isr` (the last assignment's ISR — a replica outside it may be badly
/// behind and must never win). Ties break on the lowest `node_id`.
///
/// `self_id` is the deterministic fallback when `leos` carries no entries
/// at all (nobody answered, or the ISR is just `[self]` and there was
/// nobody to ask) — a candidate always knows its own `leo` without a
/// network round trip, so an empty `leos` must not make an otherwise
/// eligible sole-ISR-member candidate return `None`. When `leos` DOES carry
/// entries, callers that want `self` considered against them include
/// `(self_id, own_leo)` in `leos` themselves — `self_id` alone never wins a
/// non-empty comparison it did not enter.
pub fn choose_candidate(isr: &[String], leos: &[(String, u64)], self_id: &str) -> Option<String> {
    let mut best: Option<(&str, u64)> = None;
    for (node_id, leo) in leos {
        if !isr.iter().any(|m| m == node_id) {
            continue;
        }
        best = Some(match best {
            None => (node_id.as_str(), *leo),
            Some((best_id, best_leo)) => {
                if *leo > best_leo || (*leo == best_leo && node_id.as_str() < best_id) {
                    (node_id.as_str(), *leo)
                } else {
                    (best_id, best_leo)
                }
            }
        });
    }
    match best {
        Some((node_id, _)) => Some(node_id.to_string()),
        None if isr.iter().any(|m| m == self_id) => Some(self_id.to_string()),
        None => None,
    }
}

/// PLAN-M2 §1c: "`admitted_by_majority(op_id, replicas)` = liczba wpisów
/// `acknowledged == true` dla targetów z `replicas` ≥ `floor(|replicas|/2)+1`
/// (licząc siebie)". `acked` is the set of node ids the ledger reports as
/// having acknowledged the op (`LedgerAdmission::admitted_by`, outbox
/// targets only — never includes `self_id`, since the op is local); self is
/// counted whenever it is actually a member of `replicas` (a proposing node
/// that is not even a replica is a caller bug, not a reason to inflate the
/// count).
pub fn admitted_by_majority(acked: &[String], replicas: &[String], self_id: &str) -> bool {
    let required = min_isr_required(replicas.len());
    let mut admitted: HashSet<&str> = HashSet::new();
    if replicas.iter().any(|r| r == self_id) {
        admitted.insert(self_id);
    }
    for node_id in acked {
        if replicas.iter().any(|r| r == node_id) {
            admitted.insert(node_id.as_str());
        }
    }
    admitted.len() >= required
}

/// This node's locally-tracked role for a partition, as `manager.rs`'s
/// registry sees it right now — distinct from `bus::PartitionRole` (the
/// coordinator-facing, epoch-carrying view): this is only the input
/// `should_start_election` needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalRole {
    Leader,
    Follower,
    NotReplica,
}

/// Gate before a follower spends a `LEO_QUERY_TIMEOUT` round trip on an
/// election it cannot win or should not attempt: only a `Follower` (a
/// `Leader` has nothing to promote itself to; a `NotReplica` node has no
/// business electing itself) whose lease has actually expired AND who is a
/// member of the last known ISR (K-M2-3 — an out-of-ISR follower would
/// immediately lose `choose_candidate` anyway) should start one.
pub fn should_start_election(lease_expired: bool, in_isr: bool, role: LocalRole) -> bool {
    role == LocalRole::Follower && lease_expired && in_isr
}

/// Why a promotion attempt stopped without reaching `Promoted`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbandonReason {
    /// Defensive: `step` was driven with a `LeaseExpired` event whose `isr`
    /// does not contain `self_id` — should never happen if the caller
    /// already applied `should_start_election`, but the state machine
    /// checks it anyway rather than trusting the caller silently.
    NotInIsr,
    /// `choose_candidate` picked a different node — this attempt stops so
    /// the winner's own election (or, if it never proposes, the next lease
    /// expiry) can proceed instead of two candidates racing.
    LostElection { winner: Option<String> },
    /// `AssignmentStore::propose` returned an error (ledger unavailable,
    /// local write failure, …).
    ProposeFailed,
    /// The proposed operation never reached a majority of `replicas`
    /// before its deadline — M2-R2's split-brain guard working as
    /// intended, not a bug: this candidate stays a follower and (per
    /// `manager.rs`) retries after its own lease expires again.
    NoMajority,
}

/// One action `PromotionState::step` asks the caller (`manager.rs`) to
/// execute. Kept data-only and `PartialEq` so tests can assert on the
/// returned `Vec<PromotionAction>` directly instead of re-deriving intent
/// from the resulting state.
#[derive(Debug, Clone, PartialEq)]
pub enum PromotionAction {
    /// Query every OTHER replica's `leo` (K-M2-3). `to` never includes
    /// `self_id`.
    SendLeoQuery { to: Vec<String> },
    /// Submit this assignment as a ledger operation
    /// (`AssignmentStore::propose`).
    ProposeAssignment(PartitionAssignment),
    /// Stamp the local partition's `leader_epoch` (`Partition::
    /// set_leader_epoch`, via whatever `LeaderHandle` the manager opens).
    SetLeaderEpoch(u32),
    /// Open/refresh the per-replica feeders (`LeaderHandle`).
    StartFeeders,
    /// K-M2-1: sent to a replica whose `leo` is ahead of the new leader's
    /// own `leo` — never to a replica behind it.
    SendTruncate { node: String, to: u64 },
}

/// Promotion state machine (PLAN-M2 §1b): `Idle -> Querying -> Proposing ->
/// AwaitingMajority -> Promoted | Abandoned`. Every variant beyond `Idle`
/// carries the full context `step` needs for its next transition — this
/// file has no side state anywhere else, so the SAME `(state, event)` pair
/// always produces the SAME `(next_state, actions)` pair (the property the
/// table tests below rely on).
#[derive(Debug, Clone)]
pub enum PromotionState {
    Idle,
    Querying {
        /// plan-app-platform §7 W4: the TentaBus instance this election is
        /// for — carried through unchanged so the eventual `PartitionAssignment`
        /// this election proposes names the right instance instead of the
        /// single-instance placeholder.
        instance_id: String,
        org_id: String,
        topic: String,
        partition: u32,
        self_id: String,
        current_epoch: u32,
        own_leo: u64,
        own_hw: u64,
        isr: Vec<String>,
        replicas: Vec<String>,
        /// `LeoReply`s accumulated so far; never contains `self_id`.
        leos: Vec<(String, u64)>,
        deadline: Instant,
    },
    Proposing {
        assignment: PartitionAssignment,
        replicas: Vec<String>,
        self_id: String,
        truncate_targets: Vec<(String, u64)>,
    },
    AwaitingMajority {
        op_id: OperationId,
        epoch: u32,
        replicas: Vec<String>,
        self_id: String,
        truncate_targets: Vec<(String, u64)>,
        deadline: Instant,
    },
    Promoted {
        epoch: u32,
    },
    Abandoned {
        reason: AbandonReason,
    },
}

/// Input to `PromotionState::step`. Every field a transition needs is
/// carried by the event itself (including wall-clock reads) so `step`
/// stays pure — `manager.rs` is the only place that ever calls
/// `Instant::now()`, reads the ledger, or dials a peer.
#[derive(Debug, Clone)]
pub enum PromotionEvent {
    /// The follower lease watchdog fired and `should_start_election`
    /// already returned `true` for this partition.
    LeaseExpired {
        /// plan-app-platform §7 W4: which TentaBus instance owns this
        /// partition — see `PromotionState::Querying`'s field of the same
        /// name.
        instance_id: String,
        org_id: String,
        topic: String,
        partition: u32,
        self_id: String,
        current_epoch: u32,
        own_leo: u64,
        own_hw: u64,
        isr: Vec<String>,
        replicas: Vec<String>,
        /// When the resulting `Querying` state's own `LeoQuery` round trip
        /// gives up — caller-supplied (normally `now + LEO_QUERY_TIMEOUT`,
        /// but NOT hardcoded here) so a caller driving its own wait loop on
        /// a different budget (e.g. a shorter one in tests) can never end
        /// up racing against a `deadline` this module invented on its own
        /// from a constant the caller has no way to override.
        leo_query_deadline: Instant,
    },
    /// One replica answered a `LeoQuery` (`ReplLeoReply`). `in_isr` is the
    /// REPLYING node's own belief about its ISR membership — carried for
    /// wire-shape completeness (mirrors `ReplLeoReply.in_isr`) but not
    /// consulted by `step`: candidacy safety comes entirely from
    /// `choose_candidate` filtering against the CANDIDATE's own `isr`
    /// (K-M2-3), so a stale self-report from the replying node cannot hide
    /// a replica that must still be truncated (K-M2-1) or re-admitted to
    /// `new_isr` once caught up.
    LeoReply {
        node_id: String,
        leo: u64,
        in_isr: bool,
    },
    /// A previously-scheduled deadline elapsed (or a poll tick fired before
    /// it — `step` re-checks `now` against the state's own deadline either
    /// way). `now_ms` is only consumed by the `Querying -> Proposing`
    /// transition (building `PartitionAssignment.updated_at_ms`); ignored
    /// otherwise.
    Timeout { now: Instant, now_ms: i64 },
    /// `AssignmentStore::propose` returned successfully.
    Proposed {
        op_id: OperationId,
        deadline: Instant,
    },
    /// `AssignmentStore::propose` failed.
    ProposeFailed,
    /// A poll of `LedgerAdmission::admitted_by(op_id)` — `manager.rs`
    /// polls this on its own cadence while `AwaitingMajority`.
    AckObserved { acked: Vec<String> },
    /// External reset (assignment deleted/changed under us, role no longer
    /// `Follower`, …) — returns to `Idle` from any state.
    Reset,
}

impl PromotionState {
    /// Pure transition: the same `(self, event)` always yields the same
    /// `(next_state, actions)`. `manager.rs` executes `actions` in order
    /// and drives the next event from their outcome.
    pub fn step(self, event: PromotionEvent) -> (PromotionState, Vec<PromotionAction>) {
        if matches!(event, PromotionEvent::Reset) {
            return (PromotionState::Idle, Vec::new());
        }
        match (self, event) {
            (
                PromotionState::Idle,
                PromotionEvent::LeaseExpired {
                    instance_id,
                    org_id,
                    topic,
                    partition,
                    self_id,
                    current_epoch,
                    own_leo,
                    own_hw,
                    isr,
                    replicas,
                    leo_query_deadline,
                },
            ) => {
                if !isr.iter().any(|m| m == &self_id) {
                    return (
                        PromotionState::Abandoned {
                            reason: AbandonReason::NotInIsr,
                        },
                        Vec::new(),
                    );
                }
                let to: Vec<String> = replicas
                    .iter()
                    .filter(|r| *r != &self_id)
                    .cloned()
                    .collect();
                let deadline = leo_query_deadline;
                (
                    PromotionState::Querying {
                        instance_id,
                        org_id,
                        topic,
                        partition,
                        self_id,
                        current_epoch,
                        own_leo,
                        own_hw,
                        isr,
                        replicas,
                        leos: Vec::new(),
                        deadline,
                    },
                    vec![PromotionAction::SendLeoQuery { to }],
                )
            }

            (
                PromotionState::Querying {
                    instance_id,
                    org_id,
                    topic,
                    partition,
                    self_id,
                    current_epoch,
                    own_leo,
                    own_hw,
                    isr,
                    replicas,
                    mut leos,
                    deadline,
                },
                PromotionEvent::LeoReply {
                    node_id,
                    leo,
                    in_isr: _,
                },
            ) => {
                // Recorded regardless of the replying node's own `in_isr`
                // self-report: candidacy safety comes entirely from
                // `choose_candidate` filtering against THIS node's own
                // `isr` (the last assignment's, K-M2-3) — a stale or
                // over-cautious self-report from the replying node must
                // not hide a replica that is genuinely ahead of us and
                // therefore needs a `Truncate` (K-M2-1), nor one that has
                // caught back up to `own_hw` and belongs in `new_isr`.
                if replicas.iter().any(|r| r == &node_id) {
                    match leos.iter_mut().find(|(id, _)| *id == node_id) {
                        Some(existing) => existing.1 = leo,
                        None => leos.push((node_id, leo)),
                    }
                }
                (
                    PromotionState::Querying {
                        instance_id,
                        org_id,
                        topic,
                        partition,
                        self_id,
                        current_epoch,
                        own_leo,
                        own_hw,
                        isr,
                        replicas,
                        leos,
                        deadline,
                    },
                    Vec::new(),
                )
            }

            (
                PromotionState::Querying {
                    instance_id,
                    org_id,
                    topic,
                    partition,
                    self_id,
                    current_epoch,
                    own_leo,
                    own_hw,
                    isr,
                    replicas,
                    leos,
                    deadline,
                },
                PromotionEvent::Timeout { now, now_ms },
            ) => {
                if now < deadline {
                    return (
                        PromotionState::Querying {
                            instance_id,
                            org_id,
                            topic,
                            partition,
                            self_id,
                            current_epoch,
                            own_leo,
                            own_hw,
                            isr,
                            replicas,
                            leos,
                            deadline,
                        },
                        Vec::new(),
                    );
                }
                let mut all_leos = leos.clone();
                all_leos.push((self_id.clone(), own_leo));
                match choose_candidate(&isr, &all_leos, &self_id) {
                    Some(winner) if winner == self_id => {
                        // K-M2-1: ISR for the new assignment is self plus
                        // every replica whose leo is caught up to (not
                        // necessarily equal to) our own persisted hw — a
                        // replica behind hw was never actually in sync.
                        let mut new_isr: Vec<String> = leos
                            .iter()
                            .filter(|(_, leo)| *leo >= own_hw)
                            .map(|(id, _)| id.clone())
                            .collect();
                        new_isr.push(self_id.clone());
                        new_isr.sort();
                        new_isr.dedup();

                        // K-M2-1: truncate only replicas AHEAD of our own
                        // leo (the old leader, rejoining with a longer
                        // un-replicated tail) — never one behind it. The
                        // target is OUR OWN `leo`, not the replying peer's:
                        // a peer told to truncate to its own `leo` is a
                        // silent no-op (`Partition::truncate_to_offset`
                        // returns the unchanged `leo` for any request at or
                        // above it, `truncate()`'s "nothing to discard"
                        // early return), so carrying the reply's value
                        // would leave the divergence exactly where it was
                        // instead of cutting the replica back to the new
                        // leader's authority.
                        let truncate_targets: Vec<(String, u64)> = leos
                            .iter()
                            .filter(|(_, leo)| *leo > own_leo)
                            .map(|(node, _)| (node.clone(), own_leo))
                            .collect();

                        let assignment = PartitionAssignment {
                            instance_id,
                            org_id,
                            topic,
                            partition,
                            leader_node_id: self_id.clone(),
                            replicas: replicas.clone(),
                            isr: new_isr,
                            leader_epoch: next_epoch(current_epoch),
                            updated_at_ms: now_ms,
                        };
                        let action = PromotionAction::ProposeAssignment(assignment.clone());
                        (
                            PromotionState::Proposing {
                                assignment,
                                replicas,
                                self_id,
                                truncate_targets,
                            },
                            vec![action],
                        )
                    }
                    winner => (
                        PromotionState::Abandoned {
                            reason: AbandonReason::LostElection { winner },
                        },
                        Vec::new(),
                    ),
                }
            }

            (
                PromotionState::Proposing {
                    assignment,
                    replicas,
                    self_id,
                    truncate_targets,
                },
                PromotionEvent::Proposed { op_id, deadline },
            ) => (
                PromotionState::AwaitingMajority {
                    op_id,
                    epoch: assignment.leader_epoch,
                    replicas,
                    self_id,
                    truncate_targets,
                    deadline,
                },
                Vec::new(),
            ),

            (PromotionState::Proposing { .. }, PromotionEvent::ProposeFailed) => (
                PromotionState::Abandoned {
                    reason: AbandonReason::ProposeFailed,
                },
                Vec::new(),
            ),

            (
                PromotionState::AwaitingMajority {
                    op_id,
                    epoch,
                    replicas,
                    self_id,
                    truncate_targets,
                    deadline,
                },
                PromotionEvent::AckObserved { acked },
            ) => {
                if admitted_by_majority(&acked, &replicas, &self_id) {
                    let mut actions = vec![
                        PromotionAction::SetLeaderEpoch(epoch),
                        PromotionAction::StartFeeders,
                    ];
                    actions.extend(truncate_targets.iter().map(|(node, leo)| {
                        PromotionAction::SendTruncate {
                            node: node.clone(),
                            to: *leo,
                        }
                    }));
                    (PromotionState::Promoted { epoch }, actions)
                } else {
                    (
                        PromotionState::AwaitingMajority {
                            op_id,
                            epoch,
                            replicas,
                            self_id,
                            truncate_targets,
                            deadline,
                        },
                        Vec::new(),
                    )
                }
            }

            (
                PromotionState::AwaitingMajority {
                    op_id,
                    epoch,
                    replicas,
                    self_id,
                    truncate_targets,
                    deadline,
                },
                PromotionEvent::Timeout { now, .. },
            ) => {
                if now < deadline {
                    (
                        PromotionState::AwaitingMajority {
                            op_id,
                            epoch,
                            replicas,
                            self_id,
                            truncate_targets,
                            deadline,
                        },
                        Vec::new(),
                    )
                } else {
                    (
                        PromotionState::Abandoned {
                            reason: AbandonReason::NoMajority,
                        },
                        Vec::new(),
                    )
                }
            }

            // Terminal states (`Promoted`/`Abandoned`) and mismatched
            // (state, event) pairs (e.g. a stray `LeoReply` after
            // `Idle`/`Proposing`): no transition, no actions.
            (state, _) => (state, Vec::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> String {
        v.to_string()
    }

    fn ss(vs: &[&str]) -> Vec<String> {
        vs.iter().map(|v| s(v)).collect()
    }

    fn op_id(byte: u8) -> OperationId {
        OperationId::from_hash([byte; 32])
    }

    // ---- min_isr_required / next_epoch -----------------------------------

    #[test]
    fn min_isr_required_is_floor_rf_over_2_plus_1() {
        assert_eq!(min_isr_required(1), 1);
        assert_eq!(min_isr_required(2), 2);
        assert_eq!(min_isr_required(3), 2);
        assert_eq!(min_isr_required(4), 3);
        assert_eq!(min_isr_required(5), 3);
    }

    #[test]
    fn next_epoch_increments_and_saturates() {
        assert_eq!(next_epoch(0), 1);
        assert_eq!(next_epoch(41), 42);
        assert_eq!(next_epoch(u32::MAX), u32::MAX);
    }

    // ---- choose_candidate --------------------------------------------------

    #[test]
    fn choose_candidate_picks_max_leo_in_isr() {
        let isr = ss(&["a", "b", "c"]);
        let leos = vec![(s("a"), 10), (s("b"), 30), (s("c"), 20)];
        assert_eq!(choose_candidate(&isr, &leos, "a"), Some(s("b")));
    }

    #[test]
    fn choose_candidate_ignores_non_isr_members() {
        let isr = ss(&["a", "b"]);
        // "c" has the highest leo but is not in the ISR (K-M2-3).
        let leos = vec![(s("a"), 10), (s("b"), 20), (s("c"), 99)];
        assert_eq!(choose_candidate(&isr, &leos, "a"), Some(s("b")));
    }

    #[test]
    fn choose_candidate_breaks_ties_on_lowest_node_id() {
        let isr = ss(&["node-b", "node-a", "node-c"]);
        let leos = vec![(s("node-b"), 50), (s("node-a"), 50), (s("node-c"), 10)];
        assert_eq!(choose_candidate(&isr, &leos, "node-a"), Some(s("node-a")));
    }

    #[test]
    fn choose_candidate_falls_back_to_self_when_no_replies() {
        let isr = ss(&["a"]);
        assert_eq!(choose_candidate(&isr, &[], "a"), Some(s("a")));
    }

    #[test]
    fn choose_candidate_returns_none_when_self_not_in_isr_and_no_replies() {
        let isr = ss(&["a"]);
        assert_eq!(choose_candidate(&isr, &[], "b"), None);
    }

    #[test]
    fn choose_candidate_returns_none_when_no_isr_member_replied() {
        let isr = ss(&["a", "b"]);
        let leos = vec![(s("c"), 100)];
        assert_eq!(choose_candidate(&isr, &leos, "z"), None);
    }

    // ---- admitted_by_majority ----------------------------------------------

    #[test]
    fn admitted_by_majority_counts_self_plus_acked_replicas() {
        let replicas = ss(&["l", "f1", "f2"]);
        assert!(!admitted_by_majority(&[], &replicas, "l")); // self only: 1 < 2
        assert!(admitted_by_majority(&ss(&["f1"]), &replicas, "l")); // self+f1: 2 >= 2
    }

    #[test]
    fn admitted_by_majority_ignores_acks_from_non_replicas() {
        let replicas = ss(&["l", "f1", "f2"]);
        assert!(!admitted_by_majority(&ss(&["stranger"]), &replicas, "l"));
    }

    #[test]
    fn admitted_by_majority_true_for_rf1_counting_only_self() {
        let replicas = ss(&["solo"]);
        assert!(admitted_by_majority(&[], &replicas, "solo"));
    }

    #[test]
    fn admitted_by_majority_false_when_self_is_not_a_replica() {
        // Caller bug guard: self isn't even in `replicas`, so it must not
        // get an extra free count on top of the genuine replica acks —
        // only "a" (one real ack, required is 2 for 3 replicas) is
        // counted, which is NOT yet a majority.
        let replicas = ss(&["a", "b", "c"]);
        assert!(!admitted_by_majority(&ss(&["a"]), &replicas, "z"));
    }

    // ---- should_start_election ----------------------------------------------

    #[test]
    fn should_start_election_only_for_in_isr_follower_with_expired_lease() {
        assert!(should_start_election(true, true, LocalRole::Follower));
        assert!(!should_start_election(false, true, LocalRole::Follower));
        assert!(!should_start_election(true, false, LocalRole::Follower));
        assert!(!should_start_election(true, true, LocalRole::Leader));
        assert!(!should_start_election(true, true, LocalRole::NotReplica));
    }

    // ---- PromotionState::step: happy path -----------------------------------

    fn lease_expired(
        isr: &[&str],
        replicas: &[&str],
        self_id: &str,
        epoch: u32,
        own_leo: u64,
        own_hw: u64,
        now: Instant,
    ) -> PromotionEvent {
        PromotionEvent::LeaseExpired {
            instance_id: s("tentabus-00000001"),
            org_id: s("org-1"),
            topic: s("orders"),
            partition: 0,
            self_id: s(self_id),
            current_epoch: epoch,
            own_leo,
            own_hw,
            isr: ss(isr),
            replicas: ss(replicas),
            leo_query_deadline: now + LEO_QUERY_TIMEOUT,
        }
    }

    #[test]
    fn full_promotion_happy_path_reaches_promoted() {
        let t0 = Instant::now();
        let state = PromotionState::Idle;

        // 1. lease expiry -> Querying, sends LeoQuery to the other replicas.
        let (state, actions) = state.step(lease_expired(
            &["f1", "f2", "l"],
            &["l", "f1", "f2"],
            "f1",
            5,
            100,
            90,
            t0,
        ));
        assert_eq!(
            actions,
            vec![PromotionAction::SendLeoQuery {
                to: ss(&["l", "f2"])
            }]
        );
        assert!(matches!(state, PromotionState::Querying { .. }));

        // 2. f2 answers with a lower leo than us; the (crashed) old leader
        // never answers.
        let (state, actions) = state.step(PromotionEvent::LeoReply {
            node_id: s("f2"),
            leo: 80,
            in_isr: true,
        });
        assert!(actions.is_empty());

        // 3. LeoQuery deadline elapses: we (f1, leo=100) have the highest
        // leo among ISR members who answered (+ourselves) -> Proposing.
        let (state, actions) = state.step(PromotionEvent::Timeout {
            now: t0 + LEO_QUERY_TIMEOUT,
            now_ms: 1_000,
        });
        let assignment = match actions.as_slice() {
            [PromotionAction::ProposeAssignment(a)] => a.clone(),
            other => panic!("expected exactly one ProposeAssignment, got {other:?}"),
        };
        assert_eq!(assignment.leader_node_id, "f1");
        assert_eq!(assignment.leader_epoch, 6);
        assert_eq!(assignment.updated_at_ms, 1_000);
        // f2's leo (80) < own_hw (90): not caught up, excluded from the new ISR.
        assert_eq!(assignment.isr, vec![s("f1")]);
        assert!(matches!(state, PromotionState::Proposing { .. }));

        // 4. propose succeeds.
        let (state, actions) = state.step(PromotionEvent::Proposed {
            op_id: op_id(1),
            deadline: t0 + LEO_QUERY_TIMEOUT + MAJORITY_AWAIT_TIMEOUT,
        });
        assert!(actions.is_empty());
        assert!(matches!(state, PromotionState::AwaitingMajority { .. }));

        // 5. majority acks -> Promoted, with SetLeaderEpoch + StartFeeders.
        let (state, actions) = state.step(PromotionEvent::AckObserved {
            acked: vec![s("f2")],
        });
        assert_eq!(
            actions,
            vec![
                PromotionAction::SetLeaderEpoch(6),
                PromotionAction::StartFeeders,
            ]
        );
        assert!(matches!(state, PromotionState::Promoted { epoch: 6 }));
    }

    #[test]
    fn promotion_sends_truncate_to_replicas_ahead_of_own_leo() {
        let t0 = Instant::now();
        let state = PromotionState::Idle;
        // "b" is a REPLICA but not in the last assignment's ISR (it fell
        // behind earlier) — otherwise its higher leo below would make it
        // WIN the candidacy outright (K-M2-3), which would defeat this
        // test's premise that "a" (self) is the one promoted.
        let (state, _) = state.step(lease_expired(
            &["a", "c"],
            &["a", "b", "c"],
            "a",
            1,
            /* own_leo */ 50,
            /* own_hw */ 40,
            t0,
        ));
        // "b" is ahead of us (old leader rejoining with an un-replicated
        // tail) — must be truncated even though it is not a candidate.
        // "c" is behind and must not be truncated.
        let (state, _) = state.step(PromotionEvent::LeoReply {
            node_id: s("b"),
            leo: 70,
            in_isr: false,
        });
        let (state, actions) = state.step(PromotionEvent::LeoReply {
            node_id: s("c"),
            leo: 45,
            in_isr: true,
        });
        assert!(actions.is_empty());
        let (state, actions) = state.step(PromotionEvent::Timeout {
            now: t0 + LEO_QUERY_TIMEOUT,
            now_ms: 1,
        });
        assert!(matches!(state, PromotionState::Proposing { .. }));
        let assignment = match actions.as_slice() {
            [PromotionAction::ProposeAssignment(a)] => a.clone(),
            other => panic!("unexpected actions: {other:?}"),
        };
        assert_eq!(assignment.leader_node_id, "a");
        let (state, _) = state.step(PromotionEvent::Proposed {
            op_id: op_id(2),
            deadline: t0 + Duration::from_secs(1),
        });
        let (_state, actions) = state.step(PromotionEvent::AckObserved {
            acked: vec![s("b"), s("c")],
        });
        assert_eq!(
            actions,
            vec![
                PromotionAction::SetLeaderEpoch(2),
                PromotionAction::StartFeeders,
                // Truncated back to OUR leo (50), not to "b"'s own 70 —
                // see `Querying -> Proposing`'s `truncate_targets` doc.
                PromotionAction::SendTruncate {
                    node: s("b"),
                    to: 50
                },
            ]
        );
    }

    // ---- PromotionState::step: candidate not in ISR never proposes ----------

    #[test]
    fn candidate_not_in_isr_is_abandoned_without_querying() {
        let t0 = Instant::now();
        let state = PromotionState::Idle;
        let (state, actions) = state.step(lease_expired(
            &["other-1", "other-2"], // self ("f9") is NOT in the ISR
            &["other-1", "other-2", "f9"],
            "f9",
            3,
            10,
            10,
            t0,
        ));
        assert!(actions.is_empty());
        assert!(matches!(
            state,
            PromotionState::Abandoned {
                reason: AbandonReason::NotInIsr
            }
        ));
    }

    #[test]
    fn losing_the_leo_comparison_abandons_without_proposing() {
        let t0 = Instant::now();
        let state = PromotionState::Idle;
        let (state, _) = state.step(lease_expired(
            &["a", "b"],
            &["a", "b"],
            "a",
            1,
            /* own_leo */ 10,
            /* own_hw */ 10,
            t0,
        ));
        let (state, _) = state.step(PromotionEvent::LeoReply {
            node_id: s("b"),
            leo: 99,
            in_isr: true,
        });
        let (state, actions) = state.step(PromotionEvent::Timeout {
            now: t0 + LEO_QUERY_TIMEOUT,
            now_ms: 1,
        });
        assert!(actions.is_empty());
        assert!(matches!(
            state,
            PromotionState::Abandoned {
                reason: AbandonReason::LostElection {
                    winner: Some(ref w)
                }
            } if w == "b"
        ));
    }

    // ---- PromotionState::step: no majority -> abandon, then retryable -------

    #[test]
    fn no_majority_before_deadline_abandons_and_can_be_reset_for_retry() {
        let t0 = Instant::now();
        let state = PromotionState::Idle;
        let (state, _) = state.step(lease_expired(
            &["a", "b", "c"],
            &["a", "b", "c"],
            "a",
            0,
            5,
            5,
            t0,
        ));
        let (state, _) = state.step(PromotionEvent::Timeout {
            now: t0 + LEO_QUERY_TIMEOUT,
            now_ms: 1,
        });
        let deadline = t0 + LEO_QUERY_TIMEOUT + MAJORITY_AWAIT_TIMEOUT;
        let (state, _) = state.step(PromotionEvent::Proposed {
            op_id: op_id(3),
            deadline,
        });
        // Nobody else acked before the deadline.
        let (state, actions) = state.step(PromotionEvent::AckObserved { acked: vec![] });
        assert!(actions.is_empty());
        let (state, actions) = state.step(PromotionEvent::Timeout {
            now: deadline,
            now_ms: 2,
        });
        assert!(actions.is_empty());
        assert!(matches!(
            state,
            PromotionState::Abandoned {
                reason: AbandonReason::NoMajority
            }
        ));
        // The manager resets before the next lease-expiry retry.
        let (state, actions) = state.step(PromotionEvent::Reset);
        assert!(actions.is_empty());
        assert!(matches!(state, PromotionState::Idle));
    }

    #[test]
    fn propose_failure_abandons_immediately() {
        let t0 = Instant::now();
        let state = PromotionState::Idle;
        let (state, _) = state.step(lease_expired(&["a"], &["a"], "a", 0, 1, 1, t0));
        let (state, _) = state.step(PromotionEvent::Timeout {
            now: t0 + LEO_QUERY_TIMEOUT,
            now_ms: 1,
        });
        let (state, actions) = state.step(PromotionEvent::ProposeFailed);
        assert!(actions.is_empty());
        assert!(matches!(
            state,
            PromotionState::Abandoned {
                reason: AbandonReason::ProposeFailed
            }
        ));
    }

    // ---- Reset from every state returns to Idle without actions -------------

    #[test]
    fn reset_from_any_state_returns_to_idle() {
        assert!(matches!(
            PromotionState::Idle.step(PromotionEvent::Reset).0,
            PromotionState::Idle
        ));
        assert!(matches!(
            PromotionState::Promoted { epoch: 9 }
                .step(PromotionEvent::Reset)
                .0,
            PromotionState::Idle
        ));
        assert!(matches!(
            PromotionState::Abandoned {
                reason: AbandonReason::NoMajority
            }
            .step(PromotionEvent::Reset)
            .0,
            PromotionState::Idle
        ));
    }

    #[test]
    fn spurious_events_in_terminal_states_are_no_ops() {
        let (state, actions) =
            PromotionState::Promoted { epoch: 4 }.step(PromotionEvent::LeoReply {
                node_id: s("x"),
                leo: 1,
                in_isr: true,
            });
        assert!(actions.is_empty());
        assert!(matches!(state, PromotionState::Promoted { epoch: 4 }));
    }

    #[test]
    fn early_timeout_tick_before_leo_query_deadline_is_a_no_op() {
        let t0 = Instant::now();
        let state = PromotionState::Idle;
        let (state, _) = state.step(lease_expired(&["a", "b"], &["a", "b"], "a", 0, 1, 1, t0));
        let (state, actions) = state.step(PromotionEvent::Timeout {
            now: t0, // deadline is t0 + LEO_QUERY_TIMEOUT, not yet reached
            now_ms: 0,
        });
        assert!(actions.is_empty());
        assert!(matches!(state, PromotionState::Querying { .. }));
    }
}
