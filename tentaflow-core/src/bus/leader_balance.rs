// =============================================================================
// File: bus/leader_balance.rs — moving topic leaders between nodes (TB-04 K5)
// =============================================================================
//
// The service half of "spread the leaders": it gathers what the planner
// (`leader_balance_plan`) needs, checks the plan against the nodes that lead
// the topics (a dry-run transfer, `ReplicationCoordinator::check_transfer`),
// and executes it. The same machinery serves the admin's "Przenieś
// prowadzenie" (one topic to one node), "Rozłóż prowadzenie" (a plan, shown
// first and applied only if it is still the plan) and the one-off spread
// after an upgrade (`run_post_upgrade_spread`).
//
// A TOPIC MOVES WHOLE. Its partitions and those of its `__dlq.*` topic go to
// the target together or not at all (`move_unit`): every partition is checked
// first (target in the leader's live ISR, close enough to catch up), then the
// partitions are moved one by one, and when one of them fails the ones that
// had moved go back to their previous leaders. A topic split between two
// nodes is read by neither (`open_consumer` wants every partition), so a half
// move is the one result that must not stay.
//
// SEQUENCE. A plan runs topic by topic; the first topic that fails stops the
// rest, which are reported as not attempted. Writes that reach a partition
// while it moves are refused or held by the old leader and go on to the new
// one by forwarding (`forward.rs`); the leaders' own transfer keeps the move
// from losing a write that was acknowledged (`ReplicationManager::
// transfer_leader_here`).
//
// AUDIT. Every partition that changes leader (and every rollback of one)
// writes `bus.leader.transfer`, as the manual transfer always did; a run
// writes one `bus.leader.rebalance` per organisation it touched with the
// actor, the number of moves, the result and the reason.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::leader_balance_plan::{self as planner, reason, PartitionFacts, PlanInput};
pub use super::leader_balance_plan::{LeaderPlan, NodeLoad, PlanBlocker, PlannedMove};
use super::replication::assignment::PartitionAssignment;
use super::{dlq, BusService, ExpectedLeader, NodeForwarding, ReplError, ReplicationCoordinator};

/// Audit action of a partition that changed leader through an admin action or
/// a rebalance. Details: `instance_id=<id> org_id=<org> partition=<u32>
/// from_node=<id|-> from_epoch=<u32> target_node_id=<id> leader_epoch=<u32>
/// duration_ms=<u64> reason=<manual|rebalance|post_upgrade_spread|preferred_restore|
/// rollback>`.
pub const LEADER_TRANSFER_AUDIT_ACTION: &str = "bus.leader.transfer";

/// Audit action summing up one run of moves for one organisation. The
/// automatic return of a topic to its preferred leader (`leader_return`) writes
/// one for its single move too, with `reason=preferred_restore`.
pub const LEADER_REBALANCE_AUDIT_ACTION: &str = "bus.leader.rebalance";

/// Audit action of a topic a one-off spread left where it was, with the reason.
pub const LEADER_REBALANCE_SKIPPED_AUDIT_ACTION: &str = "bus.leader.rebalance_skipped";

/// `reason` of the aggregate audit row of the spread that follows an upgrade
/// (shown to the admin as "rozkład po aktualizacji").
pub const POST_UPGRADE_SPREAD_REASON: &str = "post_upgrade_spread";

/// How long the nodes are asked what they accept when a plan is made.
pub(super) const PLAN_PROBE_BUDGET: Duration = Duration::from_secs(1);

/// How long a rollback keeps trying to hand a partition back: the node that
/// just took it over has to publish the move before the previous leader is
/// in its ISR again.
const ROLLBACK_WINDOW: Duration = Duration::from_secs(15);

/// How long planning asks the leaders before it leaves the moves not yet
/// checked out of the plan.
const PLAN_VERIFY_MAX: Duration = Duration::from_secs(20);

/// How long, after a move got no answer, the rows are read for what became of
/// the partition, and how often.
const OUTCOME_RESOLVE_WINDOW: Duration = if cfg!(test) {
    Duration::from_millis(300)
} else {
    Duration::from_secs(10)
};
const OUTCOME_RESOLVE_POLL: Duration = if cfg!(test) {
    Duration::from_millis(20)
} else {
    Duration::from_millis(250)
};

/// The reason a move is refused when the paused groups cannot be read.
const PAUSE_STATE_UNREADABLE: &str = "pause_state_unreadable";

/// The wait between two attempts of a rollback.
const ROLLBACK_RETRY: Duration = Duration::from_millis(250);

/// `settings` key (per instance) of the one-off spread's marker. A key of
/// this node's own `settings` table: only an allowlisted key replicates
/// (`db::repository::SHARED_SETTING_KEYS`), and an op for a key an older
/// build does not know is refused by its materializer.
fn spread_marker_key(instance_id: &str) -> String {
    format!("bus.leader_spread:{instance_id}")
}

#[derive(Debug, thiserror::Error)]
pub enum LeaderBalanceError {
    #[error("no replication coordinator is installed on this node")]
    ReplicationDisabled,
    /// The moves asked for are not what the plan is now.
    #[error("the plan has changed since it was shown")]
    PlanChanged,
    #[error("no admin right on '{0}'")]
    PermissionDenied(String),
    /// Another rebalance (or the spread after an upgrade) is running on this
    /// node.
    #[error("another leader rebalance is running on this node")]
    Busy,
    #[error("{0}")]
    Failed(String),
}

/// One move as a client asks for it (`LeaderBalanceApplyRequest`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestedMove {
    pub topic: String,
    pub partitions: Vec<u32>,
    pub from_node_id: String,
    pub to_node_id: String,
}

/// How one move of a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveStatus {
    /// Every partition of the topic and its DLQ is led by the target.
    Moved,
    /// Refused before anything moved, or failed with nothing moved: no
    /// partition changed node.
    Failed,
    /// Failed after some partitions moved, which went back to where they
    /// were: no partition changed node.
    FailedRolledBack,
    /// Failed and the partitions that had moved could not all be handed back:
    /// the topic is split and needs the admin.
    FailedPartial,
    /// An earlier move failed and stopped the run.
    NotAttempted,
}

impl MoveStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            MoveStatus::Moved => "moved",
            MoveStatus::Failed => "failed",
            MoveStatus::FailedRolledBack => "failed_rolled_back",
            MoveStatus::FailedPartial => "failed_partial",
            MoveStatus::NotAttempted => "not_attempted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveResult {
    pub org_id: String,
    pub topic: String,
    pub partitions: Vec<u32>,
    pub from_node_id: String,
    pub to_node_id: String,
    pub status: MoveStatus,
    /// The stable `bus.*` code of what failed (`bus.transfer_target_behind`).
    pub error_code: Option<String>,
    pub detail: Option<String>,
}

/// A partition that changed leader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionMoved {
    pub topic: String,
    pub partition: u32,
    pub from_node_id: String,
    pub from_epoch: u32,
    pub leader_epoch: u32,
    pub duration_ms: u64,
}

/// What a whole-topic move did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TopicMoved {
    pub moved: Vec<PartitionMoved>,
    /// Partitions the target already led: `(topic, partition, epoch)`.
    pub unchanged: Vec<(String, u32, u32)>,
}

/// Why a whole-topic move did not happen, and what became of the partitions.
#[derive(Debug, Clone)]
pub struct TopicMoveFailure {
    pub cause: ReplError,
    pub status: MoveStatus,
    /// Partitions left on the target when the rollback could not finish.
    pub stranded: Vec<(String, u32)>,
    /// Partitions whose move got no answer and whose row did not show it
    /// either in time: they may be led by the target or still by the old
    /// leader (which stays stopped until the ledger says).
    pub unresolved: Vec<(String, u32)>,
    /// Another run holds this node's executor: nothing was tried.
    pub busy: bool,
}

/// Who a run acts for, and why (the audit's `actor`/`reason`).
#[derive(Debug, Clone, Copy)]
pub struct RunContext<'a> {
    /// The admin's user id; `None` for the system.
    pub actor: Option<&'a str>,
    pub reason: &'static str,
}

pub(super) fn error_code(e: &ReplError) -> &'static str {
    match e {
        ReplError::NoAssignment { .. } => "bus.partition_unavailable",
        ReplError::NotEnoughReplicas { .. } => "bus.not_enough_replicas",
        ReplError::NotAReplica { .. } => "bus.not_a_replica",
        ReplError::EpochFenced { .. } | ReplError::NotPartitionLeader { .. } => "bus.not_leader",
        ReplError::TransferTargetBehind { .. } => "bus.transfer_target_behind",
        ReplError::TransferBlocked { .. } => "bus.transfer_blocked",
        ReplError::TransferOutcomeUnknown { .. } => "bus.transfer_outcome_unknown",
        ReplError::TransferRefused { .. } => "bus.transfer_refused",
        ReplError::NodeWithoutForwarding { .. } => "bus.node_without_forwarding",
        ReplError::Internal(_) => "bus.replication_internal_error",
    }
}

/// Whether a failure or a blocker will be the same on the next try (a paused
/// group, a node that cannot take forwarded requests) and so is no reason to
/// run the spread after an upgrade again.
fn is_permanent_code(code: &str) -> bool {
    matches!(code, "bus.transfer_blocked" | "bus.node_without_forwarding")
}

fn is_permanent_reason(why: &str) -> bool {
    matches!(
        why,
        reason::CONSUMER_GROUP_PAUSED
            | reason::NODE_WITHOUT_FORWARDING
            | reason::NO_ADMIN_RIGHT
            | reason::WOULD_NOT_IMPROVE
    )
}

/// How a leader's refusal of a dry-run transfer reads as a plan blocker.
fn veto_of(e: &ReplError, target: &str, leader: &str) -> (String, Option<String>) {
    match e {
        ReplError::NotAReplica { .. } => (
            reason::TARGET_OUTSIDE_ISR.to_string(),
            Some(target.to_string()),
        ),
        ReplError::TransferTargetBehind { .. } => {
            (reason::TARGET_BEHIND.to_string(), Some(target.to_string()))
        }
        ReplError::TransferBlocked { reason, .. } => (reason.clone(), None),
        ReplError::NodeWithoutForwarding { .. } => (
            reason::NODE_WITHOUT_FORWARDING.to_string(),
            Some(target.to_string()),
        ),
        _ => (
            reason::LEADER_UNAVAILABLE.to_string(),
            Some(leader.to_string()),
        ),
    }
}

/// The topic a (possibly dead-letter) topic belongs to.
pub(super) fn source_of(topic: &str) -> &str {
    topic.strip_prefix(dlq::DLQ_TOPIC_PREFIX).unwrap_or(topic)
}

impl BusService {
    fn balance_coordinator(&self) -> Result<Arc<dyn ReplicationCoordinator>, LeaderBalanceError> {
        self.replication()
            .ok_or(LeaderBalanceError::ReplicationDisabled)
    }

    /// Every partition row of the instance, from the ledger's materialized view.
    fn instance_rows(&self) -> Result<Vec<PartitionAssignment>, LeaderBalanceError> {
        let coordinator = self.balance_coordinator()?;
        coordinator
            .instance_assignments()
            .map_err(|e| LeaderBalanceError::Failed(format!("cannot read the assignments: {e}")))
    }

    /// The facts of one plan. Where this node leads a partition its own live
    /// ISR replaces the ledger's; for the others only the leader knows, and
    /// the dry-run check asks it.
    fn gather_plan_input(
        &self,
        coordinator: &Arc<dyn ReplicationCoordinator>,
        rows: &[PartitionAssignment],
    ) -> Result<PlanInput, LeaderBalanceError> {
        let local = coordinator.local_node_id();
        let mut live: BTreeMap<(String, String, u32), Vec<String>> = BTreeMap::new();
        let orgs: BTreeSet<&str> = rows.iter().map(|r| r.org_id.as_str()).collect();
        for org in orgs {
            for p in coordinator.snapshot(org, None).partitions {
                if p.leader_node_id.as_deref() == Some(local.as_str()) {
                    live.insert((org.to_string(), p.topic, p.partition), p.isr);
                }
            }
        }
        let partitions: Vec<PartitionFacts> = rows
            .iter()
            .map(|r| {
                let live_isr = live.get(&(r.org_id.clone(), r.topic.clone(), r.partition));
                PartitionFacts {
                    org_id: r.org_id.clone(),
                    topic: r.topic.clone(),
                    partition: r.partition,
                    leader: r.leader_node_id.clone(),
                    replicas: r.replicas.clone(),
                    isr: live_isr.cloned().unwrap_or_else(|| r.isr.clone()),
                    isr_live: live_isr.is_some(),
                }
            })
            .collect();

        let nodes: BTreeSet<String> = rows
            .iter()
            .flat_map(|r| r.replicas.iter().chain(std::iter::once(&r.leader_node_id)))
            .filter(|n| !n.is_empty())
            .cloned()
            .collect();
        let node_list: Vec<String> = nodes.iter().cloned().collect();
        let forwarding = coordinator.node_forwarding(&node_list, PLAN_PROBE_BUDGET);
        let forward_capable = node_list
            .iter()
            .zip(&forwarding)
            .filter(|(_, state)| **state == NodeForwarding::Capable)
            .map(|(node, _)| node.clone())
            .chain(std::iter::once(local.clone()))
            .collect();
        let reachable = nodes
            .iter()
            .filter(|n| **n == local || coordinator.node_reachable(n))
            .cloned()
            .chain(std::iter::once(local.clone()))
            .collect();
        // A pause is written by the node that leads the topic and sent to
        // its replicas; the ones this node knows are listed here, the rest
        // come back as the leaders' refusals (`TransferBlocked`).
        let paused = crate::db::repository::bus_group_list_all(&self.local_db)
            .map_err(|e| {
                LeaderBalanceError::Failed(format!("cannot read the consumer groups: {e}"))
            })?
            .into_iter()
            .filter(|g| g.paused)
            .map(|g| (g.org_id, source_of(&g.topic).to_string()))
            .collect();

        Ok(PlanInput {
            partitions,
            reachable,
            forward_capable,
            paused,
            vetoed: BTreeMap::new(),
            scope_org: None,
            scope_topic: None,
            automatic_return_off: !self.leader_return_enabled(),
        })
    }

    /// The rows of the topic `unit` (source and DLQ), by `(org, source)`.
    pub(super) fn unit_rows<'a>(
        rows: &'a [PartitionAssignment],
        org: &str,
        unit: &str,
    ) -> Vec<&'a PartitionAssignment> {
        let mut found: Vec<&PartitionAssignment> = rows
            .iter()
            .filter(|r| r.org_id == org && source_of(&r.topic) == unit)
            .collect();
        found.sort_by(|a, b| (&a.topic, a.partition).cmp(&(&b.topic, b.partition)));
        found
    }

    /// The plan for the instance, checked with the leaders. `can_admin` says
    /// which topics the caller may move; the others are blockers
    /// (`no_admin_right`). A move a leader refuses in a dry run is not in the
    /// plan: its topic becomes a blocker and the plan is made again without
    /// it, until every move in it has been accepted by its leaders. A move a
    /// leader accepted is not asked again in the passes that follow, and the
    /// asking stops after `PLAN_VERIFY_MAX`: the moves not yet checked by then
    /// are left out as `leader_unavailable`.
    pub fn leader_balance_plan(
        &self,
        scope_org: Option<&str>,
        scope_topic: Option<&str>,
        can_admin: &dyn Fn(&str, &str) -> bool,
    ) -> Result<LeaderPlan, LeaderBalanceError> {
        let coordinator = self.balance_coordinator()?;
        let rows = self.instance_rows()?;
        let mut input = self.gather_plan_input(&coordinator, &rows)?;
        input.scope_org = scope_org.map(str::to_string);
        input.scope_topic = scope_topic.map(str::to_string);
        for p in &input.partitions {
            let unit = source_of(&p.topic).to_string();
            if !can_admin(&p.org_id, &unit) {
                input.vetoed.insert(
                    (p.org_id.clone(), unit),
                    (reason::NO_ADMIN_RIGHT.to_string(), None),
                );
            }
        }
        let started = Instant::now();
        let mut accepted: BTreeSet<(String, String, String)> = BTreeSet::new();
        // Every pass that vetoes something settles at least one more topic,
        // so the loop ends within the number of topics.
        let passes = rows.len() + 1;
        for _ in 0..passes {
            let plan = planner::plan(&input);
            let mut refused = false;
            for mv in &plan.moves {
                let key = (mv.org_id.clone(), mv.topic.clone());
                if input.vetoed.contains_key(&key) {
                    continue;
                }
                let checked = (mv.org_id.clone(), mv.topic.clone(), mv.to.clone());
                if accepted.contains(&checked) {
                    continue;
                }
                if started.elapsed() >= PLAN_VERIFY_MAX {
                    input
                        .vetoed
                        .insert(key, (reason::LEADER_UNAVAILABLE.to_string(), None));
                    refused = true;
                    continue;
                }
                match self.check_unit_move(&coordinator, &rows, &mv.org_id, &mv.topic, &mv.to) {
                    Some(veto) => {
                        input.vetoed.insert(key, veto);
                        refused = true;
                    }
                    None => {
                        accepted.insert(checked);
                    }
                }
            }
            if !refused {
                return Ok(plan);
            }
        }
        Ok(planner::plan(&input))
    }

    /// Asks the leaders whether every partition of the unit that `to` does
    /// not lead yet could be handed to it, without changing anything. `None`
    /// when they all could.
    fn check_unit_move(
        &self,
        coordinator: &Arc<dyn ReplicationCoordinator>,
        rows: &[PartitionAssignment],
        org: &str,
        unit: &str,
        to: &str,
    ) -> Option<(String, Option<String>)> {
        for row in Self::unit_rows(rows, org, unit) {
            if row.leader_node_id == to {
                continue;
            }
            if let Err(e) = coordinator.check_transfer(org, &row.topic, row.partition, to) {
                return Some(veto_of(&e, to, &row.leader_node_id));
            }
        }
        None
    }

    /// Applies `requested`, if it is still the plan. The caller's admin right
    /// is required on every topic it names (`PermissionDenied` otherwise);
    /// the plan is made again (for the whole organisation, or for the one
    /// topic when every move names it) and a difference is `PlanChanged`. The
    /// moves run in order and the first failure stops the rest.
    ///
    /// One run at a time on this node (`Busy` otherwise). Another node may run
    /// one too: each move reads the rows again and the leaders refuse a
    /// transfer of a partition they no longer lead, so two runs cannot
    /// interleave into a split topic, but the second may find the plan changed
    /// under it, which is `PlanChanged` on its next apply.
    pub fn leader_balance_apply(
        &self,
        org: &str,
        run: RunContext<'_>,
        requested: &[RequestedMove],
        can_admin: &dyn Fn(&str, &str) -> bool,
    ) -> Result<Vec<MoveResult>, LeaderBalanceError> {
        for mv in requested {
            if !can_admin(org, &mv.topic) {
                return Err(LeaderBalanceError::PermissionDenied(mv.topic.clone()));
            }
        }
        let _running = self
            .leader_balance_run
            .try_lock()
            .ok_or(LeaderBalanceError::Busy)?;
        let as_requested = |plan: &LeaderPlan| -> Vec<RequestedMove> {
            plan.moves
                .iter()
                .map(|m| RequestedMove {
                    topic: m.topic.clone(),
                    partitions: m.partitions.clone(),
                    from_node_id: m.from.clone(),
                    to_node_id: m.to.clone(),
                })
                .collect()
        };
        let mut plan = self.leader_balance_plan(Some(org), None, can_admin)?;
        if as_requested(&plan) != requested {
            let single = requested
                .first()
                .map(|m| m.topic.as_str())
                .filter(|t| requested.iter().all(|m| m.topic == *t));
            let Some(topic) = single else {
                return Err(LeaderBalanceError::PlanChanged);
            };
            plan = self.leader_balance_plan(Some(org), Some(topic), can_admin)?;
            if as_requested(&plan) != requested {
                return Err(LeaderBalanceError::PlanChanged);
            }
        }
        let coordinator = self.balance_coordinator()?;
        Ok(self.execute_plan(&coordinator, run, plan.moves, true, false))
    }

    /// Runs `moves` one after the other; with `stop_on_failure` the first
    /// failure stops the rest (reported as not attempted), otherwise the
    /// following moves still run. One `bus.leader.rebalance` audit row per
    /// organisation touched.
    pub(super) fn execute_plan(
        &self,
        coordinator: &Arc<dyn ReplicationCoordinator>,
        run: RunContext<'_>,
        moves: Vec<PlannedMove>,
        stop_on_failure: bool,
        keep_order: bool,
    ) -> Vec<MoveResult> {
        let rows = match self.instance_rows() {
            Ok(rows) => rows,
            Err(e) => {
                return moves
                    .into_iter()
                    .map(|m| unattempted(m, Some(e.to_string())))
                    .collect()
            }
        };
        let mut results: Vec<MoveResult> = Vec::new();
        let mut stopped = false;
        for mv in moves {
            if stopped {
                results.push(unattempted(mv, None));
                continue;
            }
            let outcome = self.move_unit(
                coordinator,
                &rows,
                run,
                &mv.org_id,
                &mv.topic,
                &mv.to,
                keep_order,
            );
            let (status, error_code, detail) = match outcome {
                Ok(_) => (MoveStatus::Moved, None, None),
                Err(f) => {
                    stopped = stop_on_failure;
                    let listed = |list: &[(String, u32)]| {
                        list.iter()
                            .map(|(t, p)| format!("{t}/{p}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    };
                    let mut detail = f.cause.to_string();
                    if !f.stranded.is_empty() {
                        detail.push_str(&format!("; left on {}: {}", mv.to, listed(&f.stranded)));
                    }
                    if !f.unresolved.is_empty() {
                        detail.push_str(&format!(
                            "; where these are led is not known: {}",
                            listed(&f.unresolved)
                        ));
                    }
                    (
                        f.status,
                        Some(error_code(&f.cause).to_string()),
                        Some(detail),
                    )
                }
            };
            results.push(MoveResult {
                org_id: mv.org_id,
                topic: mv.topic,
                partitions: mv.partitions,
                from_node_id: mv.from,
                to_node_id: mv.to,
                status,
                error_code,
                detail,
            });
        }
        self.audit_rebalance(coordinator, run, &results);
        results
    }

    fn audit_rebalance(
        &self,
        coordinator: &Arc<dyn ReplicationCoordinator>,
        run: RunContext<'_>,
        results: &[MoveResult],
    ) {
        let local = coordinator.local_node_id();
        let orgs: BTreeSet<&str> = results.iter().map(|r| r.org_id.as_str()).collect();
        for org in orgs {
            let ours: Vec<&MoveResult> = results.iter().filter(|r| r.org_id == org).collect();
            let moved = ours
                .iter()
                .filter(|r| r.status == MoveStatus::Moved)
                .count();
            let failed = ours
                .iter()
                .find(|r| !matches!(r.status, MoveStatus::Moved | MoveStatus::NotAttempted));
            let result = match failed {
                None => "ok",
                Some(r) => r.status.as_str(),
            };
            let mut details = format!(
                "instance_id={} org_id={org} actor={} reason={} moves={} moved={moved} result={result}",
                self.instance_id,
                run.actor.unwrap_or("system"),
                run.reason,
                ours.len(),
            );
            if let Some(r) = failed {
                details.push_str(&format!(
                    " failed_topic={} error={}",
                    r.topic,
                    r.error_code.as_deref().unwrap_or("-")
                ));
            }
            let _ = crate::db::repository::log_audit(
                &self.db,
                run.actor,
                None,
                LEADER_REBALANCE_AUDIT_ACTION,
                None,
                Some(&details),
                None,
                Some(&local),
            );
        }
    }

    /// Moves the leadership of a whole topic, its dead-letter topic included,
    /// to `to`: the admin's "Przenieś prowadzenie".
    pub fn move_topic_leadership(
        &self,
        run: RunContext<'_>,
        org: &str,
        topic: &str,
        to: &str,
    ) -> Result<TopicMoved, TopicMoveFailure> {
        let refused = |cause: ReplError| TopicMoveFailure {
            cause,
            status: MoveStatus::Failed,
            stranded: Vec::new(),
            unresolved: Vec::new(),
            busy: false,
        };
        // The same lock as a plan's run: a manual move and a rebalance, the
        // spread or the automatic return never move one topic at once on this
        // node. (Another node's run is caught by the epoch each transfer
        // expects.)
        let Some(_running) = self.leader_balance_run.try_lock() else {
            return Err(TopicMoveFailure {
                busy: true,
                ..refused(ReplError::TransferBlocked {
                    topic: topic.to_string(),
                    reason: "rebalance_busy".to_string(),
                })
            });
        };
        let coordinator = self.replication().ok_or_else(|| {
            refused(ReplError::Internal(
                "no replication coordinator is installed on this node".into(),
            ))
        })?;
        let rows = self
            .instance_rows()
            .map_err(|e| refused(ReplError::Internal(e.to_string())))?;
        self.move_unit(&coordinator, &rows, run, org, source_of(topic), to, false)
    }

    /// The whole-topic move (see the module doc). `rows` are the instance's
    /// assignments as read for this run. The target must take forwarded
    /// requests (`node_without_forwarding` otherwise): once it leads, every
    /// other node reaches the topic through it, and one that cannot would
    /// lose its writes.
    fn move_unit(
        &self,
        coordinator: &Arc<dyn ReplicationCoordinator>,
        rows: &[PartitionAssignment],
        run: RunContext<'_>,
        org: &str,
        unit: &str,
        to: &str,
        keep_order: bool,
    ) -> Result<TopicMoved, TopicMoveFailure> {
        let local = coordinator.local_node_id();
        let all = Self::unit_rows(rows, org, unit);
        let refused = |cause: ReplError| TopicMoveFailure {
            cause,
            status: MoveStatus::Failed,
            stranded: Vec::new(),
            unresolved: Vec::new(),
            busy: false,
        };
        if all.is_empty() {
            return Err(refused(ReplError::NoAssignment {
                topic: unit.to_string(),
                partition: 0,
            }));
        }
        if to != local {
            let capable = coordinator
                .node_forwarding(&[to.to_string()], PLAN_PROBE_BUDGET)
                .first()
                .is_some_and(|state| *state == NodeForwarding::Capable);
            if !capable {
                return Err(refused(ReplError::NodeWithoutForwarding {
                    node_id: to.to_string(),
                }));
            }
        }
        let mut outcome = TopicMoved::default();
        let mut todo: Vec<&PartitionAssignment> = Vec::new();
        for row in all {
            if row.leader_node_id == to {
                outcome
                    .unchanged
                    .push((row.topic.clone(), row.partition, row.leader_epoch));
            } else {
                todo.push(row);
            }
        }
        // Every partition is checked with its leader before any is moved.
        for row in &todo {
            coordinator
                .check_transfer(org, &row.topic, row.partition, to)
                .map_err(refused)?;
        }
        let mut moved: Vec<(&PartitionAssignment, u32)> = Vec::new();
        for row in &todo {
            let started = Instant::now();
            // The rows were read for this run: a partition that has been moved
            // since (by another node's run, an election) is refused by its
            // leader. `keep_order` leaves the replica order as it is (a gather
            // is not a choice of a new preferred node).
            let expected = ExpectedLeader {
                node_id: row.leader_node_id.clone(),
                epoch: row.leader_epoch,
            };
            let order: &[String] = if keep_order { &row.replicas } else { &[] };
            let result = match coordinator.transfer_leader_expecting(
                org,
                &row.topic,
                row.partition,
                to,
                order,
                &expected,
            ) {
                Ok(epoch) => Ok(epoch),
                // No answer: the partition may have moved all the same. The
                // row says which; only a row that does not say leaves the
                // partition unresolved.
                Err(cause @ ReplError::TransferOutcomeUnknown { .. }) => {
                    match self.row_led_by(coordinator, org, &row.topic, row.partition, to) {
                        Some(epoch) => Ok(epoch),
                        None => Err((cause, true)),
                    }
                }
                Err(cause) => Err((cause, false)),
            };
            match result {
                Ok(epoch) => {
                    let record = PartitionMoved {
                        topic: row.topic.clone(),
                        partition: row.partition,
                        from_node_id: row.leader_node_id.clone(),
                        from_epoch: row.leader_epoch,
                        leader_epoch: epoch,
                        duration_ms: u64::try_from(started.elapsed().as_millis())
                            .unwrap_or(u64::MAX),
                    };
                    self.audit_transfer(&local, run, org, &record, to, run.reason);
                    outcome.moved.push(record);
                    moved.push((row, epoch));
                }
                Err((cause, unknown)) => {
                    let unresolved = if unknown {
                        vec![(row.topic.clone(), row.partition)]
                    } else {
                        Vec::new()
                    };
                    return Err(self.roll_back(
                        coordinator,
                        &local,
                        run,
                        org,
                        to,
                        &moved,
                        cause,
                        unresolved,
                    ));
                }
            }
        }
        Ok(outcome)
    }

    /// The epoch at which `to` leads the partition, read from the rows, once
    /// it does: waits for the row to materialize for at most
    /// `OUTCOME_RESOLVE_WINDOW`.
    fn row_led_by(
        &self,
        coordinator: &Arc<dyn ReplicationCoordinator>,
        org: &str,
        topic: &str,
        partition: u32,
        to: &str,
    ) -> Option<u32> {
        let until = Instant::now() + OUTCOME_RESOLVE_WINDOW;
        loop {
            if let Ok(rows) = coordinator.instance_assignments() {
                let led = rows
                    .iter()
                    .find(|r| r.org_id == org && r.topic == topic && r.partition == partition);
                if let Some(row) = led.filter(|r| r.leader_node_id == to) {
                    return Some(row.leader_epoch);
                }
            }
            if Instant::now() >= until {
                return None;
            }
            std::thread::sleep(OUTCOME_RESOLVE_POLL);
        }
    }

    /// Hands every partition in `moved` (taken by `taken_by`) back to the
    /// leader it had, with the replica order it had, in the reverse order, but
    /// only while `taken_by` still leads it at the epoch the move gave it: a
    /// partition somebody moved on meanwhile is not taken back from where
    /// they put it, and is reported with `unresolved`, as are the partitions
    /// of the failed move whose fate is not known. Either makes the result
    /// partial, since the topic may be split.
    #[allow(clippy::too_many_arguments)]
    fn roll_back(
        &self,
        coordinator: &Arc<dyn ReplicationCoordinator>,
        local: &str,
        run: RunContext<'_>,
        org: &str,
        taken_by: &str,
        moved: &[(&PartitionAssignment, u32)],
        cause: ReplError,
        mut unresolved: Vec<(String, u32)>,
    ) -> TopicMoveFailure {
        let mut stranded = Vec::new();
        for (row, taken_at_epoch) in moved.iter().rev() {
            let started = Instant::now();
            let deadline = started + ROLLBACK_WINDOW;
            let still_taken = ExpectedLeader {
                node_id: taken_by.to_string(),
                epoch: *taken_at_epoch,
            };
            let mut moved_on = false;
            let restored = loop {
                match coordinator.transfer_leader_expecting(
                    org,
                    &row.topic,
                    row.partition,
                    &row.leader_node_id,
                    &row.replicas,
                    &still_taken,
                ) {
                    Ok(epoch) => break Some(epoch),
                    Err(e @ ReplError::NotPartitionLeader { .. }) => {
                        tracing::warn!(
                            org, topic = %row.topic, partition = row.partition, error = %e,
                            "leader move: a partition was moved on by someone else and is not handed back"
                        );
                        moved_on = true;
                        break None;
                    }
                    Err(e) if Instant::now() >= deadline => {
                        tracing::warn!(
                            org, topic = %row.topic, partition = row.partition, error = %e,
                            "leader move: a partition could not be handed back after a failed move"
                        );
                        break None;
                    }
                    Err(_) => std::thread::sleep(ROLLBACK_RETRY),
                }
            };
            match restored {
                Some(epoch) => {
                    let record = PartitionMoved {
                        topic: row.topic.clone(),
                        partition: row.partition,
                        from_node_id: taken_by.to_string(),
                        from_epoch: *taken_at_epoch,
                        leader_epoch: epoch,
                        duration_ms: u64::try_from(started.elapsed().as_millis())
                            .unwrap_or(u64::MAX),
                    };
                    self.audit_transfer(local, run, org, &record, &row.leader_node_id, "rollback");
                }
                None if moved_on => unresolved.push((row.topic.clone(), row.partition)),
                None => stranded.push((row.topic.clone(), row.partition)),
            }
        }
        let status = if !stranded.is_empty() || !unresolved.is_empty() {
            MoveStatus::FailedPartial
        } else if moved.is_empty() {
            MoveStatus::Failed
        } else {
            MoveStatus::FailedRolledBack
        };
        TopicMoveFailure {
            cause,
            status,
            stranded,
            unresolved,
            busy: false,
        }
    }

    fn audit_transfer(
        &self,
        local: &str,
        run: RunContext<'_>,
        org: &str,
        moved: &PartitionMoved,
        target: &str,
        reason: &str,
    ) {
        let _ = crate::db::repository::log_audit(
            &self.db,
            run.actor,
            None,
            LEADER_TRANSFER_AUDIT_ACTION,
            Some(&moved.topic),
            Some(&format!(
                "instance_id={} org_id={org} partition={} from_node={} from_epoch={} \
                 target_node_id={target} leader_epoch={} duration_ms={} reason={reason}",
                self.instance_id,
                moved.partition,
                moved.from_node_id,
                moved.from_epoch,
                moved.leader_epoch,
                moved.duration_ms,
            )),
            None,
            Some(local),
        );
    }

    /// Why leadership of `topic` of `org` must not be moved from this node
    /// now: a consumer group of it (or of its dead-letter topic) is paused.
    /// A pause not yet sent to the replicas (one coalescing window) would be
    /// lost with the old leader. A pause that cannot be read is a refusal too:
    /// the move is not made on a guess.
    pub(super) fn leader_move_blocker(&self, org: &str, topic: &str) -> Option<String> {
        let unit = source_of(topic);
        match crate::db::repository::bus_group_list_all(&self.local_db) {
            Ok(groups) => groups
                .into_iter()
                .any(|g| g.paused && g.org_id == org && source_of(&g.topic) == unit)
                .then(|| reason::CONSUMER_GROUP_PAUSED.to_string()),
            Err(e) => {
                tracing::warn!(error = %e, "leader move: the consumer groups cannot be read");
                Some(PAUSE_STATE_UNREADABLE.to_string())
            }
        }
    }

    // ---- The spread after an upgrade ---------------------------------------

    /// One look at whether the one-off spread of existing topics is due, and
    /// if it is, the spread (TB-04 P4). Called repeatedly by the node's
    /// background loop until it reports `Done`.
    ///
    /// It runs once per instance: when every node of the instance advertises
    /// what moving leaders needs, on the node with the lowest id among them.
    /// A topic that fails does not stop the others. The durable marker (a
    /// `settings` row of this node) is written only when nothing was left for
    /// a transient reason (a target that is behind or outside the ISR, a node
    /// that does not answer, an outcome not known): then `Retry` is answered
    /// and the loop asks again later, with a growing pause. A reason that will
    /// be the same next time (a paused group, a node without forwarding) does
    /// not hold the marker back. Every topic left where it was, whatever the
    /// reason, is in the audit as `bus.leader.rebalance_skipped`; the admin's
    /// "Rozłóż prowadzenie" is for the ones that stay.
    ///
    /// The marker is this node's own: only the node with the lowest id runs
    /// the spread, so one marker is enough while the membership holds. A node
    /// with a lower id that joins later finds no marker and may run it again;
    /// the plan then starts from what is there, which is empty when the
    /// topics are balanced.
    pub fn run_post_upgrade_spread(&self) -> SpreadStatus {
        let key = spread_marker_key(&self.instance_id);
        if matches!(crate::db::repository::get_setting(&self.db, &key), Ok(Some(v)) if v == "done")
        {
            return SpreadStatus::Done;
        }
        let Some(coordinator) = self.replication() else {
            return SpreadStatus::NotYet("no_replication");
        };
        let rows = match self.instance_rows() {
            Ok(rows) => rows,
            Err(_) => return SpreadStatus::NotYet("no_assignments"),
        };
        let local = coordinator.local_node_id();
        let members: BTreeSet<String> = rows
            .iter()
            .flat_map(|r| r.replicas.iter().chain(std::iter::once(&r.leader_node_id)))
            .filter(|n| !n.is_empty())
            .cloned()
            .chain(std::iter::once(local.clone()))
            .collect();
        if members.len() < 2 {
            return SpreadStatus::NotYet("single_node");
        }
        let peers: Vec<String> = members.iter().filter(|n| **n != local).cloned().collect();
        let mut capable = vec![local.clone()];
        for (node, state) in peers
            .iter()
            .zip(coordinator.node_forwarding(&peers, PLAN_PROBE_BUDGET))
        {
            match state {
                NodeForwarding::Capable => capable.push(node.clone()),
                NodeForwarding::NotAMember => {}
                NodeForwarding::Lacking | NodeForwarding::Unknown => {
                    return SpreadStatus::NotYet("a_node_lacks_the_feature")
                }
            }
        }
        capable.sort();
        if capable.first() != Some(&local) {
            return SpreadStatus::NotTheRunner;
        }
        let Some(_running) = self.leader_balance_run.try_lock() else {
            return SpreadStatus::NotYet("busy");
        };
        let run = RunContext {
            actor: None,
            reason: POST_UPGRADE_SPREAD_REASON,
        };
        let plan = match self.leader_balance_plan(None, None, &|_, _| true) {
            Ok(plan) => plan,
            Err(_) => return SpreadStatus::NotYet("no_plan"),
        };
        let mut transient = false;
        let skip = |topic: &str, org: &str, why: &str, node: Option<&str>| {
            let _ = crate::db::repository::log_audit(
                &self.db,
                None,
                None,
                LEADER_REBALANCE_SKIPPED_AUDIT_ACTION,
                Some(topic),
                Some(&format!(
                    "instance_id={} org_id={org} reason={why} node_id={} source={POST_UPGRADE_SPREAD_REASON}",
                    self.instance_id,
                    node.unwrap_or("-"),
                )),
                None,
                Some(&local),
            );
        };
        for b in &plan.blockers {
            skip(&b.topic, &b.org_id, &b.reason, b.node_id.as_deref());
            transient |= !is_permanent_reason(&b.reason);
        }
        let results = self.execute_plan(&coordinator, run, plan.moves, false, false);
        for r in results.iter().filter(|r| r.status != MoveStatus::Moved) {
            let code = r.error_code.as_deref().unwrap_or("not_attempted");
            skip(&r.topic, &r.org_id, code, Some(&r.to_node_id));
            transient |= !is_permanent_code(code);
        }
        tracing::info!(
            moves = results.len(),
            skipped = plan.blockers.len(),
            transient,
            "leader spread after the upgrade ran"
        );
        if transient {
            return SpreadStatus::Retry;
        }
        if let Err(e) = crate::db::repository::set_setting(&self.db, &key, "done") {
            tracing::warn!(error = %e, "leader spread: cannot write the marker; it may run again");
        }
        SpreadStatus::Done
    }
}

fn unattempted(mv: PlannedMove, detail: Option<String>) -> MoveResult {
    MoveResult {
        org_id: mv.org_id,
        topic: mv.topic,
        partitions: mv.partitions,
        from_node_id: mv.from,
        to_node_id: mv.to,
        status: MoveStatus::NotAttempted,
        error_code: None,
        detail,
    }
}

/// How one look at the spread after an upgrade ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpreadStatus {
    /// Done now or before; nothing is left to look at.
    Done,
    /// Not due yet; the reason is a stable machine value.
    NotYet(&'static str),
    /// Another node runs it.
    NotTheRunner,
    /// It ran and left something that may work next time: no marker is
    /// written, and the caller asks again later.
    Retry,
}
