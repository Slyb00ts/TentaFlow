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
//           read). `Truncate` cuts a replica back only to what it keeps
//           under the new leader (`LogPosition::kept_end`): a tail past the
//           leader's `leo` in the leader's own epoch, everything above its
//           own `hw` in any other (in practice: the old leader, rejoining
//           with an un-replicated tail).
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
// `admitted_by_quorum` (a majority of the REPLICA set must have
// acknowledged the ledger operation, PLAN-M2 §1c) plus the materializer's
// epoch-monotonic admission gate (agent L, `core_materializer.rs`, out of
// this file's scope). LeoQuery/tie-break only improve which node wins and
// how fast — never whether an unsafe promotion can succeed.
//
// A ledger ack proves delivery, not a vote: two candidacies can both collect
// a majority of acks, and at RF ≤ 2 one replica alone admits. So an epoch is
// made a Raft term by two rules of its own. It names one leader: epochs are
// dealt round-robin over fixed per-node slots that never change hands
// (`epoch_for`, `PartitionAssignment::epoch_slots`), so concurrent
// candidacies never mint the same number, whatever replica set each stands
// on. And a replica that answers a candidacy promises its term
// (`candidacy_epoch`, `ReplLeoQuery::candidate_epoch`) — durably, on its
// partition — and follows no leader of an earlier term from then on; a
// candidate hearing a newer promise stands down
// (`AbandonReason::NewerTerm`). In the round that asks for promises only a
// reply that carries one counts toward the majority. The logs an election
// compared then stay the logs its majority holds: no earlier term can commit
// through them after they answered. Promises are asked for only in a second round
// (`ReplicationManager::run_election`), after a first that asks for none
// shows the candidacy would propose — Raft's pre-vote, so a candidate that
// defers or loses fences nobody out.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::bus::replication::assignment::PartitionAssignment;
use crate::sync::ledger::OperationId;

/// K-M2-3: candidate's LeoQuery round trip budget.
pub const LEO_QUERY_TIMEOUT: Duration = Duration::from_millis(300);

/// How long a candidate waits for `admitted_by_quorum` after proposing,
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
/// (`floor(rf/2)+1`) is also the majority threshold `admitted_by_quorum`
/// applies to the replica set for promotion admission (PLAN-M2 §1c).
pub fn min_isr_required(replication_factor: usize) -> usize {
    replication_factor / 2 + 1
}

/// The smallest group of replicas that may act for a partition without the
/// rest: elect a leader (`TooFewReplies`, `admitted_by_quorum`), keep one
/// leading (`PartitionLeader::has_quorum_lease`), and accept an
/// `acks=leader`/`acks=all` write.
///
/// A majority of the replica set for RF ≥ 3: two disjoint groups can never
/// both act, so a network split never produces two leaders and a leader
/// chosen from a majority of logs holds every committed record.
///
/// For RF ≤ 2 a majority is the whole set, and requiring it would let any
/// single node failure take the partition down. The owner's decision is
/// availability, Kafka-like: one in-sync replica may act alone. Accepted
/// risk — a write acknowledged only by that replica is lost if it is lost
/// too, and a plain network split between the two replicas (no failure at
/// all) lets both lead at once until they reach each other again and a
/// `Hello` fences one: the newer leadership wins, and the losing side's
/// writes that never reached the winner are dropped. `acks=quorum` keeps
/// its own contract at every RF (`min_isr_required`).
pub fn availability_quorum(replication_factor: usize) -> usize {
    if replication_factor >= 3 {
        min_isr_required(replication_factor)
    } else {
        1
    }
}

/// How many epoch slots a partition has (`PartitionAssignment::epoch_slots`)
/// — the most distinct nodes that may ever lead one topic incarnation's
/// partition, far above the largest replication factor (7). Fixed, never
/// derived from a replica set: two rows of different sets must deal epochs
/// by the same modulus, or their slots overlap.
pub const EPOCH_SLOTS: u32 = 64;

/// The first epoch after `current` in epoch slot `slot`: epochs are dealt
/// round-robin over `EPOCH_SLOTS`, and a node only ever mints in its own
/// slot (`PartitionAssignment::epoch_slot`). Two nodes minting from the same
/// term — two self-elections that did not hear each other, which at RF ≤ 2
/// need no one else's consent (`availability_quorum`), or a candidacy on an
/// old row racing a reassignment minted on a new one — therefore never land
/// on the same number, and an epoch names exactly one leader's chain.
/// Everything that compares epochs relies on that: a log written in the
/// leader's own epoch is taken for a prefix of its chain
/// (`LogPosition::kept_end`), a follower confirms a leader's term by its
/// number alone, and the newer of two leaderships is simply the higher
/// number.
///
/// The one exception is saturation: an epoch never wraps, so once no epoch
/// of `slot` is left below `u32::MAX` every slot mints `u32::MAX` itself.
/// That takes tens of millions of elections on one partition and signals
/// something else badly wrong; wrapping instead would let a long-dead
/// leader's stale epoch look current again.
pub fn epoch_for(current: u32, slot: u32) -> u32 {
    let n = u64::from(EPOCH_SLOTS);
    let slot = u64::from(slot % EPOCH_SLOTS);
    let first = u64::from(current) + 1;
    let epoch = first + (slot + n - first % n) % n;
    u32::try_from(epoch).unwrap_or(u32::MAX)
}

/// The epoch a candidacy in slot `slot` stands for: the first of its own
/// after both the term its assignment names (`current`) and the newest term
/// its partition was promised (`promised`) — unless that promise is its own
/// earlier candidacy, which nobody has outbid since. Standing again for that
/// same term keeps every promise already made to it valid, and a candidate
/// that cannot reach a majority does not climb one term per lease tick.
/// Epochs being unique per slot (`epoch_for`), a promise in the
/// candidate's own slot can only be its own.
pub fn candidacy_epoch(current: u32, promised: u32, slot: u32) -> u32 {
    if promised > current && epoch_for(promised - 1, slot) == promised {
        return promised;
    }
    epoch_for(current.max(promised), slot)
}

/// Where one replica's local log stands, as an election compares it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogPosition {
    /// The leader epoch the log was last written under
    /// (`Partition::log_epoch`): the epoch of its last batch, or of the last
    /// leader-authority truncate onto that leader's chain. Never the term a
    /// `Hello` stamped without shipping anything.
    pub epoch: u32,
    pub leo: u64,
    /// Every record below this offset is on a majority of the replica set
    /// (`Partition::committed_offset`) — majority-derived whatever the
    /// topic's `acks`, unlike `hw`.
    pub committed: u64,
}

impl LogPosition {
    /// Raft's "at least as up to date" order: the later epoch first, the
    /// longer log only within one epoch. Offset alone is not an order at
    /// all across terms — an old leader's unreplicated tail can outgrow a
    /// newer term's committed records at the same offsets.
    fn rank(&self) -> (u32, u64) {
        (self.epoch, self.leo)
    }

    /// How much of this replica's log a new leader at `leader` keeps. A log
    /// last written in the leader's own epoch is a prefix or an extension of
    /// the leader's chain, so only a tail past the leader's `leo` goes. A log
    /// of another epoch may diverge from the leader's anywhere above the
    /// offset it knew committed, so it goes back to its own committed
    /// offset: every record below it is on a majority, and a leader chosen
    /// from a majority of replies that ranks at least as high as each of
    /// them holds all of those (Raft's election restriction).
    ///
    /// Never below `self.committed` while `leader.leo` reaches it — which is
    /// what an election guarantees (`PromotionState::step` abandons against
    /// any reply committed past the winner's `leo`) and what a replica
    /// enforces (`BusError::TruncateBelowCommitted`).
    pub fn kept_end(&self, leader: &LogPosition) -> u64 {
        if self.epoch == leader.epoch {
            self.leo.min(leader.leo)
        } else {
            self.committed.min(self.leo).min(leader.leo)
        }
    }
}

/// K-M2-3: picks the promotion candidate from `logs` (one entry per replica
/// that answered a `LeoQuery`), restricted to members of `isr` (the last
/// assignment's ISR — a replica outside it may be badly behind and must
/// never win). The most up-to-date log wins (`LogPosition::rank`: epoch,
/// then `leo`); ties break on the lowest `node_id`.
///
/// `self_id` is the deterministic fallback when `logs` carries no entries
/// at all (nobody answered, or the ISR is just `[self]` and there was
/// nobody to ask) — a candidate always knows its own log without a
/// network round trip, so an empty `logs` must not make an otherwise
/// eligible sole-ISR-member candidate return `None`. When `logs` DOES carry
/// entries, callers that want `self` considered against them include
/// `(self_id, own)` in `logs` themselves — `self_id` alone never wins a
/// non-empty comparison it did not enter.
pub fn choose_candidate(
    isr: &[String],
    logs: &[(String, LogPosition)],
    self_id: &str,
) -> Option<String> {
    let mut best: Option<(&str, (u32, u64))> = None;
    for (node_id, log) in logs {
        if !isr.iter().any(|m| m == node_id) {
            continue;
        }
        let rank = log.rank();
        best = Some(match best {
            None => (node_id.as_str(), rank),
            Some((best_id, best_rank)) => {
                if rank > best_rank || (rank == best_rank && node_id.as_str() < best_id) {
                    (node_id.as_str(), rank)
                } else {
                    (best_id, best_rank)
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

/// PLAN-M2 §1c's admission rule — "liczba wpisów `acknowledged == true` dla
/// targetów z `replicas` ≥ `floor(|replicas|/2)+1` (licząc siebie)" — with
/// the threshold now `availability_quorum`: that majority at RF ≥ 3, one
/// replica at RF ≤ 2 (see there for the accepted risk). `acked` is the set of node ids the ledger reports as
/// having acknowledged the op (`LedgerAdmission::admitted_by`, outbox
/// targets only — never includes `self_id`, since the op is local); self is
/// counted whenever it is actually a member of `replicas` (a proposing node
/// that is not even a replica is a caller bug, not a reason to inflate the
/// count).
pub fn admitted_by_quorum(acked: &[String], replicas: &[String], self_id: &str) -> bool {
    let required = availability_quorum(replicas.len());
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
    /// Fewer than a majority of the replica set (this candidate counted)
    /// answered its `LeoQuery`. Ledger acks prove delivery of the proposal,
    /// not what the acking replicas hold: only a majority of LOGS compared
    /// intersects every majority that acknowledged a write, so only then
    /// does winning the comparison mean holding every committed record.
    TooFewReplies { answered: usize, required: usize },
    /// A replier that will not stand — a leader, a log awaiting
    /// reconciliation — ranks at least as high as this candidate, or has
    /// records committed past its `leo`. Winning would cut committed records
    /// out of that replier; it steps down and stands itself instead
    /// (`manager.rs`'s quorum-lease step-down).
    Outranked { by: String },
    /// A replier had already promised a newer term than this candidacy's
    /// (`ReplLeoReply::promised_epoch`): another candidate stands above it,
    /// and a replica that promised that term refuses every leader below it.
    /// The next attempt stands above `epoch`.
    NewerTerm { epoch: u32 },
}

/// One action `PromotionState::step` asks the caller (`manager.rs`) to
/// execute. Kept data-only and `PartialEq` so tests can assert on the
/// returned `Vec<PromotionAction>` directly instead of re-deriving intent
/// from the resulting state.
#[derive(Debug, Clone, PartialEq)]
pub enum PromotionAction {
    /// Query every OTHER replica's `leo` (K-M2-3) on behalf of a candidacy
    /// for `epoch`; a replier that promises it follows no leader of an
    /// earlier term from then on. `to` never includes `self_id`.
    SendLeoQuery { to: Vec<String>, epoch: u32 },
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
        topic_generation: u64,
        self_id: String,
        /// The epoch this candidacy stands for (`candidacy_epoch`), fixed when it
        /// starts: repliers promise exactly this term.
        epoch: u32,
        /// The round asks every replier to promise `epoch`
        /// (`PromotionEvent::LeaseExpired::vote`).
        vote: bool,
        own: LogPosition,
        isr: Vec<String>,
        replicas: Vec<String>,
        /// `LeoReply`s accumulated so far; never contains `self_id`.
        leos: Vec<(String, LogPosition)>,
        /// Repliers that said they will not stand (`LeoReply::can_stand`):
        /// their logs still decide truncation and the new ISR, but electing
        /// one of them would elect nobody.
        ineligible: Vec<String>,
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
        /// The topic incarnation the partition belongs to
        /// (`PartitionAssignment::topic_generation`), carried onto the
        /// proposal so replicas admit it into the same incarnation.
        topic_generation: u64,
        self_id: String,
        /// The epoch this candidacy stands for (`candidacy_epoch`). The
        /// caller computes it: its own partition must promise it before the
        /// log in `own` is read, so nothing an earlier term appends after
        /// that read can land.
        epoch: u32,
        /// Whether this round asks every replier to promise `epoch` (the
        /// vote) or asks for nothing (the pre-vote). In a vote only a reply
        /// that promised exactly `epoch` counts: an answer that promised
        /// nothing describes a log an earlier term may still append to.
        vote: bool,
        /// This node's own local log.
        own: LogPosition,
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
        log: LogPosition,
        in_isr: bool,
        /// Whether the replying node would stand for this partition itself
        /// (`ReplLeoReply::ineligible`, inverted). A leader that lost its
        /// quorum lease, or a node whose log awaits reconciliation, never
        /// runs an election: picking it as the winner would leave every
        /// candidate deferring to a node that never proposes.
        can_stand: bool,
        /// The newest term the replier promised
        /// (`ReplLeoReply::promised_epoch`); `None` from a replier that
        /// promised nothing.
        promised: Option<u32>,
        /// The replier understands promises (`ReplLeoReply::promises`). One
        /// that predates them is counted as before in a vote, since it
        /// could not have promised anything; see the CHANGELOG upgrade note.
        promises: bool,
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
                    topic_generation,
                    self_id,
                    epoch,
                    vote,
                    own,
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
                        topic_generation,
                        self_id,
                        epoch,
                        vote,
                        own,
                        isr,
                        replicas,
                        leos: Vec::new(),
                        ineligible: Vec::new(),
                        deadline,
                    },
                    vec![PromotionAction::SendLeoQuery { to, epoch }],
                )
            }

            (
                PromotionState::Querying {
                    instance_id,
                    org_id,
                    topic,
                    partition,
                    topic_generation,
                    self_id,
                    epoch,
                    vote,
                    own,
                    isr,
                    replicas,
                    mut leos,
                    mut ineligible,
                    deadline,
                },
                PromotionEvent::LeoReply {
                    node_id,
                    log,
                    in_isr: _,
                    can_stand,
                    promised,
                    promises,
                },
            ) => {
                // Epochs are unique per leader (`epoch_for`), so a promise
                // of exactly this epoch is this candidacy's own.
                if let Some(newer) = promised.filter(|p| *p > epoch) {
                    return (
                        PromotionState::Abandoned {
                            reason: AbandonReason::NewerTerm { epoch: newer },
                        },
                        Vec::new(),
                    );
                }
                // A vote counts only a replier bound by it: one that
                // promised nothing (a restart that does not know the
                // partition yet, a verdict that changed while it answered)
                // may still follow a leader of an earlier term, which could
                // then commit through it past the log it reported here.
                let unbound = vote && promises && promised != Some(epoch);
                // Recorded regardless of the replying node's own `in_isr`
                // self-report: candidacy safety comes entirely from
                // `choose_candidate` filtering against THIS node's own
                // `isr` (the last assignment's, K-M2-3) — a stale or
                // over-cautious self-report from the replying node must
                // not hide a replica that is genuinely ahead of us and
                // therefore needs a `Truncate` (K-M2-1), nor one that has
                // caught back up to `own.committed` and belongs in `new_isr`.
                if !unbound && replicas.iter().any(|r| r == &node_id) {
                    ineligible.retain(|id| *id != node_id);
                    if !can_stand {
                        ineligible.push(node_id.clone());
                    }
                    match leos.iter_mut().find(|(id, _)| *id == node_id) {
                        Some(existing) => existing.1 = log,
                        None => leos.push((node_id, log)),
                    }
                }
                (
                    PromotionState::Querying {
                        instance_id,
                        org_id,
                        topic,
                        partition,
                        topic_generation,
                        self_id,
                        epoch,
                        vote,
                        own,
                        isr,
                        replicas,
                        leos,
                        ineligible,
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
                    topic_generation,
                    self_id,
                    epoch,
                    vote,
                    own,
                    isr,
                    replicas,
                    leos,
                    ineligible,
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
                            topic_generation,
                            self_id,
                            epoch,
                            vote,
                            own,
                            isr,
                            replicas,
                            leos,
                            ineligible,
                            deadline,
                        },
                        Vec::new(),
                    );
                }
                let required = availability_quorum(replicas.len());
                let answered = leos.len() + 1;
                if answered < required {
                    return (
                        PromotionState::Abandoned {
                            reason: AbandonReason::TooFewReplies { answered, required },
                        },
                        Vec::new(),
                    );
                }
                let mut candidates: Vec<(String, LogPosition)> = leos
                    .iter()
                    .filter(|(id, _)| !ineligible.contains(id))
                    .cloned()
                    .collect();
                candidates.push((self_id.clone(), own));
                let outranking = leos.iter().find(|(id, log)| {
                    (ineligible.contains(id) && log.rank() >= own.rank()) || log.committed > own.leo
                });
                match choose_candidate(&isr, &candidates, &self_id) {
                    Some(winner) if winner == self_id && outranking.is_some() => (
                        PromotionState::Abandoned {
                            reason: AbandonReason::Outranked {
                                by: outranking.map(|(id, _)| id.clone()).unwrap_or_default(),
                            },
                        },
                        Vec::new(),
                    ),
                    Some(winner) if winner == self_id => {
                        // ISR for the new assignment is self plus every
                        // replica whose log, once reconciled with ours
                        // (`LogPosition::kept_end`), still reaches our own
                        // committed offset — one behind it was never in sync.
                        let mut new_isr: Vec<String> = leos
                            .iter()
                            .filter(|(_, log)| log.kept_end(&own) >= own.committed)
                            .map(|(id, _)| id.clone())
                            .collect();
                        new_isr.push(self_id.clone());
                        new_isr.sort();
                        new_isr.dedup();

                        // K-M2-1: a replica is cut back to what it keeps of
                        // its log under this leader — a tail past our `leo`
                        // in our own epoch, everything above its committed
                        // offset in any other. Never a replica already behind that point;
                        // the target is always derived from OUR log, since a
                        // peer told to truncate to its own `leo` is a silent
                        // no-op (`Partition::truncate_to_offset` returns the
                        // unchanged `leo` for any request at or above it).
                        let truncate_targets: Vec<(String, u64)> = leos
                            .iter()
                            .filter_map(|(node, log)| {
                                let kept = log.kept_end(&own);
                                (kept < log.leo).then(|| (node.clone(), kept))
                            })
                            .collect();

                        let assignment = PartitionAssignment {
                            instance_id,
                            org_id,
                            topic,
                            partition,
                            leader_node_id: self_id.clone(),
                            replicas: replicas.clone(),
                            isr: new_isr,
                            leader_epoch: epoch,
                            updated_at_ms: now_ms,
                            topic_generation,
                            epoch_slots: Default::default(),
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
                if admitted_by_quorum(&acked, &replicas, &self_id) {
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

    /// A replica's slot as a row written before slots existed derives it:
    /// its position in the sorted set.
    fn slot_in(replicas: &[&str], node: &str) -> u32 {
        let mut sorted = replicas.to_vec();
        sorted.sort_unstable();
        sorted.iter().position(|r| *r == node).expect("a member") as u32
    }

    #[test]
    fn epoch_for_deals_epochs_round_robin_over_fixed_slots() {
        assert_eq!(epoch_for(0, 0), 64);
        assert_eq!(epoch_for(0, 1), 1);
        assert_eq!(epoch_for(0, 2), 2);
        assert_eq!(epoch_for(64, 0), 128, "strictly after `current`");
        assert_eq!(epoch_for(65, 2), 66);
        assert_eq!(epoch_for(66, 2), 130);
        assert_eq!(epoch_for(7, 63), 63);
        // Saturation, the one documented exception: every slot mints the
        // last epoch once none of its own is left below it.
        assert_eq!(epoch_for(u32::MAX - 1, 0), u32::MAX);
        assert_eq!(epoch_for(u32::MAX - 1, 5), u32::MAX);
        assert_eq!(epoch_for(u32::MAX, 5), u32::MAX);
    }

    #[test]
    fn a_candidacy_stands_above_every_promise_but_its_own() {
        assert_eq!(candidacy_epoch(1, 0, 0), 64);
        assert_eq!(
            candidacy_epoch(1, 64, 0),
            64,
            "its own earlier candidacy: the same term again"
        );
        assert_eq!(
            candidacy_epoch(1, 65, 0),
            128,
            "another candidate's promise: above it"
        );
        assert_eq!(
            candidacy_epoch(70, 64, 0),
            128,
            "a promise the ledger has moved past is no candidacy any more"
        );
        assert_eq!(candidacy_epoch(65, 65, 1), 129);
    }

    /// Nodes that stand from the same term without hearing each other — the
    /// RF <= 2 split, where neither needs the other's consent — never claim
    /// the same epoch number, from any term, whatever their slots.
    #[test]
    fn nodes_standing_from_one_term_never_mint_the_same_epoch() {
        for current in (0..200u32).chain(u32::MAX - 200..u32::MAX - 64) {
            let minted: HashSet<u32> = (0..EPOCH_SLOTS)
                .map(|slot| epoch_for(current, slot))
                .collect();
            assert_eq!(minted.len(), EPOCH_SLOTS as usize, "current={current}");
            assert!(minted.iter().all(|e| *e > current));
        }
    }

    // ---- choose_candidate --------------------------------------------------

    #[test]
    fn choose_candidate_picks_max_leo_in_isr() {
        let isr = ss(&["a", "b", "c"]);
        let leos = vec![
            (s("a"), log(1, 10, 0)),
            (s("b"), log(1, 30, 0)),
            (s("c"), log(1, 20, 0)),
        ];
        assert_eq!(choose_candidate(&isr, &leos, "a"), Some(s("b")));
    }

    #[test]
    fn choose_candidate_ignores_non_isr_members() {
        let isr = ss(&["a", "b"]);
        // "c" has the highest leo but is not in the ISR (K-M2-3).
        let leos = vec![
            (s("a"), log(1, 10, 0)),
            (s("b"), log(1, 20, 0)),
            (s("c"), log(1, 99, 0)),
        ];
        assert_eq!(choose_candidate(&isr, &leos, "a"), Some(s("b")));
    }

    #[test]
    fn choose_candidate_breaks_ties_on_lowest_node_id() {
        let isr = ss(&["node-b", "node-a", "node-c"]);
        let leos = vec![
            (s("node-b"), log(1, 50, 0)),
            (s("node-a"), log(1, 50, 0)),
            (s("node-c"), log(1, 10, 0)),
        ];
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
        let leos = vec![(s("c"), log(1, 100, 0))];
        assert_eq!(choose_candidate(&isr, &leos, "z"), None);
    }

    // ---- admitted_by_quorum ----------------------------------------------

    #[test]
    fn admitted_by_quorum_counts_self_plus_acked_replicas() {
        let replicas = ss(&["l", "f1", "f2"]);
        assert!(!admitted_by_quorum(&[], &replicas, "l")); // self only: 1 < 2
        assert!(admitted_by_quorum(&ss(&["f1"]), &replicas, "l")); // self+f1: 2 >= 2
    }

    #[test]
    fn admitted_by_quorum_ignores_acks_from_non_replicas() {
        let replicas = ss(&["l", "f1", "f2"]);
        assert!(!admitted_by_quorum(&ss(&["stranger"]), &replicas, "l"));
    }

    #[test]
    fn admitted_by_quorum_true_for_rf1_counting_only_self() {
        let replicas = ss(&["solo"]);
        assert!(admitted_by_quorum(&[], &replicas, "solo"));
    }

    #[test]
    fn admitted_by_quorum_false_when_self_is_not_a_replica() {
        // Caller bug guard: self isn't even in `replicas`, so it must not
        // get an extra free count on top of the genuine replica acks —
        // only "a" (one real ack, required is 2 for 3 replicas) is
        // counted, which is NOT yet a majority.
        let replicas = ss(&["a", "b", "c"]);
        assert!(!admitted_by_quorum(&ss(&["a"]), &replicas, "z"));
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

    /// A log last written under `epoch`.
    fn log(epoch: u32, leo: u64, committed: u64) -> LogPosition {
        LogPosition {
            epoch,
            leo,
            committed,
        }
    }

    /// Builds a `LeaseExpired` whose own log was written under the current
    /// epoch — the ordinary follower of the last leader.
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
            topic_generation: 7,
            self_id: s(self_id),
            epoch: epoch_for(epoch, slot_in(replicas, self_id)),
            vote: false,
            own: log(epoch, own_leo, own_hw),
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
                to: ss(&["l", "f2"]),
                epoch: 64
            }]
        );
        assert!(matches!(state, PromotionState::Querying { .. }));

        // 2. f2 answers with a lower leo than us; the (crashed) old leader
        // never answers.
        let (state, actions) = state.step(PromotionEvent::LeoReply {
            node_id: s("f2"),
            log: log(5, 80, 80),
            in_isr: true,
            can_stand: true,
            promised: None,
            promises: true,
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
        assert_eq!(assignment.leader_epoch, 64);
        assert_eq!(assignment.updated_at_ms, 1_000);
        assert_eq!(
            assignment.topic_generation, 7,
            "the proposal stays in the partition's topic incarnation"
        );
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
                PromotionAction::SetLeaderEpoch(64),
                PromotionAction::StartFeeders,
            ]
        );
        assert!(matches!(state, PromotionState::Promoted { epoch: 64 }));
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
            // Its tail past 40 never reached a majority.
            log: log(1, 70, 40),
            in_isr: false,
            can_stand: true,
            promised: None,
            promises: true,
        });
        let (state, actions) = state.step(PromotionEvent::LeoReply {
            node_id: s("c"),
            log: log(1, 45, 45),
            in_isr: true,
            can_stand: true,
            promised: None,
            promises: true,
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
                PromotionAction::SetLeaderEpoch(64),
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
            log: log(1, 99, 99),
            in_isr: true,
            can_stand: true,
            promised: None,
            promises: true,
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
        let (state, _) = state.step(PromotionEvent::LeoReply {
            node_id: s("b"),
            log: log(0, 5, 5),
            in_isr: true,
            can_stand: true,
            promised: None,
            promises: true,
        });
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
                log: log(4, 1, 1),
                in_isr: true,
                can_stand: true,
                promised: None,
                promises: true,
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

    // ---- log epochs: an unreconciled tail never wins on offset alone ------

    #[test]
    fn choose_candidate_prefers_a_later_epoch_over_a_longer_log() {
        let isr = ss(&["x", "y", "z"]);
        let logs = vec![
            (s("x"), log(1, 120, 90)),
            (s("y"), log(2, 100, 100)),
            (s("z"), log(2, 100, 100)),
        ];
        assert_eq!(choose_candidate(&isr, &logs, "x"), Some(s("y")));
    }

    /// The crashed old leader `x` rejoined with an unreplicated tail (leo
    /// 120 under epoch 1) past the newer term's committed records (leo 100
    /// under epoch 2). Its election must lose to the newer term.
    #[test]
    fn an_old_term_tail_loses_the_election_to_a_newer_term() {
        let t0 = Instant::now();
        let (state, _) = PromotionState::Idle.step(PromotionEvent::LeaseExpired {
            instance_id: s("tentabus-00000001"),
            org_id: s("org-1"),
            topic: s("orders"),
            partition: 0,
            topic_generation: 7,
            self_id: s("x"),
            epoch: epoch_for(2, slot_in(&["x", "y", "z"], "x")),
            vote: false,
            own: log(1, 120, 90),
            isr: ss(&["x", "y", "z"]),
            replicas: ss(&["x", "y", "z"]),
            leo_query_deadline: t0 + LEO_QUERY_TIMEOUT,
        });
        let (state, _) = state.step(PromotionEvent::LeoReply {
            node_id: s("z"),
            log: log(2, 100, 100),
            in_isr: true,
            can_stand: true,
            promised: None,
            promises: true,
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
            } if w == "z"
        ));
    }

    /// The newer term wins, and the old-term replica is cut back to its own
    /// `hw` — not merely to the winner's `leo`, which would keep its
    /// divergent records below that offset.
    #[test]
    fn a_replica_of_another_epoch_is_truncated_to_its_own_hw() {
        let t0 = Instant::now();
        let (state, _) = PromotionState::Idle.step(PromotionEvent::LeaseExpired {
            instance_id: s("tentabus-00000001"),
            org_id: s("org-1"),
            topic: s("orders"),
            partition: 0,
            topic_generation: 7,
            self_id: s("z"),
            epoch: epoch_for(2, slot_in(&["x", "y", "z"], "z")),
            vote: false,
            own: log(2, 100, 100),
            isr: ss(&["x", "z"]),
            replicas: ss(&["x", "y", "z"]),
            leo_query_deadline: t0 + LEO_QUERY_TIMEOUT,
        });
        let (state, _) = state.step(PromotionEvent::LeoReply {
            node_id: s("x"),
            log: log(1, 95, 90),
            in_isr: true,
            can_stand: true,
            promised: None,
            promises: true,
        });
        let (state, actions) = state.step(PromotionEvent::Timeout {
            now: t0 + LEO_QUERY_TIMEOUT,
            now_ms: 1,
        });
        let assignment = match actions.as_slice() {
            [PromotionAction::ProposeAssignment(a)] => a.clone(),
            other => panic!("unexpected actions: {other:?}"),
        };
        assert_eq!(assignment.leader_node_id, "z");
        assert_eq!(
            assignment.isr,
            vec![s("z")],
            "a replica cut back below the leader's hw is not in sync"
        );
        let (state, _) = state.step(PromotionEvent::Proposed {
            op_id: op_id(4),
            deadline: t0 + Duration::from_secs(1),
        });
        let (_, actions) = state.step(PromotionEvent::AckObserved {
            acked: vec![s("x")],
        });
        assert_eq!(
            actions,
            vec![
                PromotionAction::SetLeaderEpoch(66),
                PromotionAction::StartFeeders,
                PromotionAction::SendTruncate {
                    node: s("x"),
                    to: 90
                },
            ]
        );
    }

    /// A replier that will not stand (a leader, a log awaiting
    /// reconciliation) is never the winner — every candidate would defer to
    /// a node that never proposes — yet its log is still reconciled with the
    /// winner's: of another epoch, it goes back to its committed offset.
    #[test]
    fn a_replier_that_will_not_stand_is_truncated_but_never_chosen() {
        let t0 = Instant::now();
        let (state, _) = PromotionState::Idle.step(lease_expired(
            &["f1", "f2", "l"],
            &["l", "f1", "f2"],
            "f1",
            2,
            50,
            50,
            t0,
        ));
        let (state, _) = state.step(PromotionEvent::LeoReply {
            node_id: s("l"),
            log: log(1, 48, 40),
            in_isr: true,
            can_stand: false,
            promised: None,
            promises: true,
        });
        let (state, actions) = state.step(PromotionEvent::Timeout {
            now: t0 + LEO_QUERY_TIMEOUT,
            now_ms: 1,
        });
        let assignment = match actions.as_slice() {
            [PromotionAction::ProposeAssignment(a)] => a.clone(),
            other => panic!("unexpected actions: {other:?}"),
        };
        assert_eq!(assignment.leader_node_id, "f1");
        let (state, _) = state.step(PromotionEvent::Proposed {
            op_id: op_id(5),
            deadline: t0 + Duration::from_secs(1),
        });
        let (_, actions) = state.step(PromotionEvent::AckObserved {
            acked: vec![s("f2")],
        });
        assert!(actions.contains(&PromotionAction::SendTruncate {
            node: s("l"),
            to: 40
        }));
    }

    /// RF=3, acks=quorum: `l` and `f2` hold records to 100 (committed); `f1`
    /// is at 90; `f2` is down and `l` lost its quorum lease. `l` will not
    /// stand, and electing `f1` — its ledger ack is a majority — would cut
    /// `l` below its committed offset and lose those records everywhere.
    #[test]
    fn a_shorter_candidate_never_wins_over_committed_records_of_a_replier_that_will_not_stand() {
        let t0 = Instant::now();
        let (state, _) = PromotionState::Idle.step(lease_expired(
            &["f1", "f2", "l"],
            &["l", "f1", "f2"],
            "f1",
            2,
            90,
            90,
            t0,
        ));
        let (state, _) = state.step(PromotionEvent::LeoReply {
            node_id: s("l"),
            log: log(2, 100, 100),
            in_isr: true,
            can_stand: false,
            promised: None,
            promises: true,
        });
        let (state, actions) = state.step(PromotionEvent::Timeout {
            now: t0 + LEO_QUERY_TIMEOUT,
            now_ms: 1,
        });
        assert!(actions.is_empty());
        assert!(matches!(
            state,
            PromotionState::Abandoned {
                reason: AbandonReason::Outranked { ref by }
            } if by == "l"
        ));
    }

    /// Records committed past the candidate's `leo` on any replier — even
    /// one of an older log epoch, which the rank comparison would place
    /// below — stop the candidacy: winning would cut them.
    #[test]
    fn a_reply_committed_past_the_candidates_log_stops_the_candidacy() {
        let t0 = Instant::now();
        let (state, _) = PromotionState::Idle.step(lease_expired(
            &["a", "b"],
            &["a", "b", "c"],
            "a",
            3,
            10,
            10,
            t0,
        ));
        let (state, _) = state.step(PromotionEvent::LeoReply {
            node_id: s("b"),
            log: log(1, 30, 20),
            in_isr: true,
            can_stand: true,
            promised: None,
            promises: true,
        });
        let (state, _) = state.step(PromotionEvent::Timeout {
            now: t0 + LEO_QUERY_TIMEOUT,
            now_ms: 1,
        });
        assert!(matches!(
            state,
            PromotionState::Abandoned {
                reason: AbandonReason::Outranked { ref by }
            } if by == "b"
        ));
    }

    /// Only a majority of logs compared intersects every majority that
    /// acknowledged a write: with fewer replies the candidacy stops before
    /// proposing, however the ledger would ack.
    #[test]
    fn fewer_than_a_majority_of_replies_never_proposes() {
        let t0 = Instant::now();
        let (state, _) = PromotionState::Idle.step(lease_expired(
            &["a", "b", "c"],
            &["a", "b", "c"],
            "a",
            1,
            5,
            5,
            t0,
        ));
        let (state, actions) = state.step(PromotionEvent::Timeout {
            now: t0 + LEO_QUERY_TIMEOUT,
            now_ms: 1,
        });
        assert!(actions.is_empty());
        assert!(matches!(
            state,
            PromotionState::Abandoned {
                reason: AbandonReason::TooFewReplies {
                    answered: 1,
                    required: 2
                }
            }
        ));
    }

    /// Owner decision: majority at RF ≥ 3 (unchanged), one in-sync replica
    /// at RF ≤ 2.
    #[test]
    fn the_availability_quorum_is_a_majority_from_three_replicas_and_one_below() {
        assert_eq!(availability_quorum(1), 1);
        assert_eq!(availability_quorum(2), 1);
        assert_eq!(availability_quorum(3), 2);
        assert_eq!(availability_quorum(5), 3);
        let rf2 = ss(&["a", "b"]);
        assert!(admitted_by_quorum(&[], &rf2, "b"), "a lone RF=2 survivor");
        let rf3 = ss(&["a", "b", "c"]);
        assert!(!admitted_by_quorum(&[], &rf3, "b"), "RF=3 still needs two");
    }

    /// RF=2: the leader is gone and nobody else answers; the surviving
    /// in-sync replica stands alone.
    #[test]
    fn a_lone_rf2_survivor_proposes_without_any_reply() {
        let t0 = Instant::now();
        let (state, _) =
            PromotionState::Idle.step(lease_expired(&["a", "b"], &["a", "b"], "b", 4, 10, 10, t0));
        let (state, actions) = state.step(PromotionEvent::Timeout {
            now: t0 + LEO_QUERY_TIMEOUT,
            now_ms: 1,
        });
        assert!(matches!(state, PromotionState::Proposing { .. }));
        assert!(matches!(
            actions.as_slice(),
            [PromotionAction::ProposeAssignment(a)] if a.leader_node_id == "b" && a.leader_epoch == 65
        ));
    }

    /// A replier that already promised a newer term than this candidacy's
    /// stops it before it proposes: that replier refuses every leader below
    /// the promised term, so winning here would elect a leader it never
    /// follows. A promise of exactly this candidacy's epoch is its own.
    #[test]
    fn a_reply_promising_a_newer_term_abandons_the_candidacy() {
        let t0 = Instant::now();
        let (state, actions) = PromotionState::Idle.step(lease_expired(
            &["a", "b", "c"],
            &["a", "b", "c"],
            "a",
            1,
            5,
            5,
            t0,
        ));
        assert_eq!(
            actions,
            vec![PromotionAction::SendLeoQuery {
                to: ss(&["b", "c"]),
                epoch: 64
            }]
        );
        let (state, _) = state.step(PromotionEvent::LeoReply {
            node_id: s("b"),
            log: log(1, 5, 5),
            in_isr: true,
            can_stand: true,
            promised: Some(64),
            promises: true,
        });
        assert!(
            matches!(state, PromotionState::Querying { .. }),
            "its own promise"
        );
        let (state, _) = state.step(PromotionEvent::LeoReply {
            node_id: s("c"),
            log: log(1, 5, 5),
            in_isr: true,
            can_stand: true,
            promised: Some(65),
            promises: true,
        });
        assert!(matches!(
            state,
            PromotionState::Abandoned {
                reason: AbandonReason::NewerTerm { epoch: 65 }
            }
        ));
        let (_, actions) = state.step(PromotionEvent::Timeout {
            now: t0 + LEO_QUERY_TIMEOUT,
            now_ms: 1,
        });
        assert!(actions.is_empty(), "an abandoned candidacy never proposes");
    }

    /// The restarted-replica case: R answers a vote without knowing the
    /// partition (or its verdict changed mid-answer), so it promised
    /// nothing. Counted, it made a majority with the candidate alone, and a
    /// leader of an earlier term could still commit through R past the log
    /// R reported. In a vote only a promise of exactly this epoch counts; a
    /// replier that predates promises is counted as before.
    #[test]
    fn a_vote_counts_only_repliers_bound_by_it() {
        let t0 = Instant::now();
        let vote = |own_leo| {
            let mut event =
                lease_expired(&["l", "r", "x"], &["l", "r", "x"], "x", 5, own_leo, 0, t0);
            if let PromotionEvent::LeaseExpired { vote, .. } = &mut event {
                *vote = true;
            }
            event
        };
        let epoch = epoch_for(5, slot_in(&["l", "r", "x"], "x"));
        let reply = |promised, promises| PromotionEvent::LeoReply {
            node_id: s("r"),
            log: log(5, 0, 0),
            in_isr: true,
            can_stand: true,
            promised,
            promises,
        };
        let timeout = PromotionEvent::Timeout {
            now: t0 + LEO_QUERY_TIMEOUT,
            now_ms: 1,
        };

        let (state, _) = PromotionState::Idle.step(vote(10));
        let (state, _) = state.step(reply(None, true));
        let (state, actions) = state.step(timeout.clone());
        assert!(actions.is_empty(), "{actions:?}");
        assert!(matches!(
            state,
            PromotionState::Abandoned {
                reason: AbandonReason::TooFewReplies {
                    answered: 1,
                    required: 2
                }
            }
        ));

        let (state, _) = PromotionState::Idle.step(vote(10));
        let (state, _) = state.step(reply(Some(epoch), true));
        let (_, actions) = state.step(timeout.clone());
        assert!(matches!(
            actions.as_slice(),
            [PromotionAction::ProposeAssignment(a)] if a.leader_epoch == epoch
        ));

        let (state, _) = PromotionState::Idle.step(vote(10));
        let (state, _) = state.step(reply(None, false));
        let (_, actions) = state.step(timeout.clone());
        assert!(
            matches!(actions.as_slice(), [PromotionAction::ProposeAssignment(_)]),
            "a replier that predates promises counts as before"
        );

        // The pre-vote asks for nothing and counts every answer.
        let (state, _) = PromotionState::Idle.step(lease_expired(
            &["l", "r", "x"],
            &["l", "r", "x"],
            "x",
            5,
            10,
            0,
            t0,
        ));
        let (state, _) = state.step(reply(None, true));
        let (_, actions) = state.step(timeout);
        assert!(matches!(
            actions.as_slice(),
            [PromotionAction::ProposeAssignment(_)]
        ));
    }
}
