// =============================================================================
// File: bus/leader_return.rs — a topic goes back to its preferred leader
//       by itself, and a split topic is gathered (TB-04 K6)
// =============================================================================
//
// After a failover the topic is led by a node that is not its preferred one
// (`replicas[0]`). When the preferred node is back and has stayed back, the
// node that leads the topic hands it over, with the same whole-topic move the
// admin's "Rozłóż prowadzenie" uses (`BusService::execute_plan`): the same
// precheck of every partition, the same rollback, the same one-run-at-a-time
// lock. It is a RETURN, so `replicas[0]` is not rewritten.
//
// TWO CASES, BOTH LOCAL, NO COORDINATOR.
//
// 1. A topic (with its `__dlq.*` topic) led from ONE node. That node decides:
//    only it knows its live in-sync set. The topic is returned when, for
//    every partition, the preferred node has been in this node's LIVE
//    in-sync set, within `PREFERRED_MAX_LAG_RECORDS` of the log end, without
//    a break seen by the sampling for `LeaderReturnTiming::stable_for`
//    (5 minutes), the preferred node takes forwarded requests, and no
//    consumer group of the topic is paused. The in-sync set is sampled every
//    `sample_interval`, much more often than the decision is made, so a node
//    that drops out and comes back inside one decision interval still
//    restarts the clock (a drop shorter than a sample may be missed). A clock
//    belongs to a leader epoch and a topic generation: leadership lost and
//    regained, or a topic created again, starts it over. A flapping node
//    never gets the topic, and since only the leader moves it and the clocks
//    restart at every attempt that fails, nothing bounces between two nodes.
//
// 2. A SPLIT topic: after a crash the partitions elect their leaders one by
//    one, and the survivors may share a topic between them. A topic split
//    between nodes is read by neither (`open_consumer` wants every
//    partition), so it must not stay so, and no single node has the sampling
//    of partitions led elsewhere. One node drives it: the lowest id among the
//    nodes that lead a partition of the unit. It moves the whole unit
//    (`move_unit` sends each partition's transfer to its leader, which checks
//    its own live ISR and the catch-up of the target): to the preferred node
//    when every stray can go there now (the leaders' dry-run checks stand in
//    for the 5 minutes), otherwise onto the node that leads most of the
//    unit. A gather onto a node that is not the preferred one does NOT
//    rewrite `replicas[0]` (`keep_order`): it is a repair of a split, not a
//    choice of a leader, and the return to the true preferred node follows
//    by case 1. A split is gathered at the next look, with no 5 minutes.
//
// HOW OFTEN. A look at the topics happens at most once per `decision_interval`
// (30 s) and moves at most one topic. A look that finds the lock of the
// rebalance executor held (a manual move, an Apply or the spread after an
// upgrade) is skipped and made again at the next sample. A return that failed
// with nothing moved starts the topic's clocks over; a gather that failed
// waits two intervals; a move that left the topic partly moved
// (`FailedPartial`) is retried at the next look, as a gather, because the
// topic is split.
//
// THE SWITCH is the `settings` key `bus.leader_return:<instance>`, shared by
// every node of the instance through the ledger (`SHARED_SETTING_KEY_PREFIXES`
// in `db::repository`). A node that has not been updated does not know the key
// and records the op as a conflict, which is harmless: it returns by its
// default (on). Off stops every return and gather this node would start.
//
// WHAT "BLOCKED ONLY TRANSIENTLY" MEANS (the same as the planner's, see
// `leader_balance_plan`). A return that is not possible for a reason that may
// pass (the preferred node is outside the in-sync set or behind, does not
// answer, or its forwarding is not known yet) is waited for: the clock does
// not run while the reason lasts, and the planner counts the topic as already
// at its preferred node, or, for a split topic, on the node that leads most
// of it (the rule gathers it there at once). A move blocked for a lasting
// reason (a paused consumer group, a node that cannot take forwarded
// requests) is never made here, and the planner counts the topic where it is.
// With the switch off nothing is moved automatically, so the planner stops
// counting on it (`PlanInput::automatic_return_off`). The preferred node of a
// unit is the one every row names first (`unanimous_preferred`); rows that
// disagree leave it with none, in the rule and in the planner alike.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::leader_balance::{source_of, MoveStatus, RunContext, PLAN_PROBE_BUDGET};
use super::leader_balance_plan::{is_internal, majority, PlannedMove};
use super::replication::assignment::PartitionAssignment;
use super::{parse_autocreate_setting, BusService, NodeForwarding, ReplicationCoordinator};

/// `reason` of the audit rows of a topic that went back to its preferred
/// node (`bus.leader.transfer` and the aggregate `bus.leader.rebalance`).
pub const PREFERRED_RESTORE_REASON: &str = "preferred_restore";

/// `reason` of the audit rows of a split topic gathered on the node that led
/// most of it.
pub const SPLIT_GATHER_REASON: &str = "split_gather";

/// How many records the preferred node may be behind the leader's log end and
/// still count as in sync for the clock. A busy topic is never exactly level.
pub const PREFERRED_MAX_LAG_RECORDS: u64 = 100;

/// How seldom a switch that cannot be read is complained about.
const SWITCH_WARNING_EVERY: Duration = Duration::from_secs(300);

/// Prefix of the per-instance `settings` key of the switch; the value is
/// `on`/`off` (also `true`/`false`, `1`/`0`). Absent means on. The key
/// replicates through the ledger (`db::repository::SHARED_SETTING_KEY_PREFIXES`).
pub const LEADER_RETURN_SETTING_PREFIX: &str = "bus.leader_return:";

/// The switch's `settings` key for `instance_id`.
pub fn leader_return_setting_key(instance_id: &str) -> String {
    format!("{LEADER_RETURN_SETTING_PREFIX}{instance_id}")
}

/// `Some(value)` for a value the switch understands; `None` for anything else
/// (which the settings handler refuses and a node reads as off).
pub fn parse_leader_return_setting(raw: &str) -> Option<bool> {
    parse_autocreate_setting(raw)
}

/// Checks an entry of a settings update that names the switch, before it is
/// stored and replicated: a hosted instance, a value the nodes read as on or
/// off (a typo would otherwise switch it off everywhere) and never a secret.
/// Entries of other keys pass.
pub fn validate_switch_setting(
    key: &str,
    value: &str,
    is_secret: bool,
    instance_hosted: &dyn Fn(&str) -> bool,
) -> Result<(), String> {
    let Some(instance) = key.strip_prefix(LEADER_RETURN_SETTING_PREFIX) else {
        return Ok(());
    };
    if !instance_hosted(instance) {
        return Err(format!("'{key}' names an instance that is not hosted here"));
    }
    if is_secret {
        return Err(format!("'{key}' is not a secret setting"));
    }
    if parse_leader_return_setting(value).is_none() {
        return Err(format!("'{key}' must be on or off"));
    }
    Ok(())
}

/// Every duration of the rule, in one place. `Default` is what runs; a test
/// builds shorter ones (`BusService::set_leader_return_timing`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaderReturnTiming {
    /// How long the preferred node must have been in the live in-sync set,
    /// without a break, before the topic is returned.
    pub stable_for: Duration,
    /// The least time between two looks at the topics, which move one topic
    /// at most.
    pub decision_interval: Duration,
    /// How often the in-sync set is sampled to keep the clocks.
    pub sample_interval: Duration,
    /// How long the loop sleeps when there is nothing to watch for (one
    /// node, no replication).
    pub idle_interval: Duration,
}

impl Default for LeaderReturnTiming {
    fn default() -> Self {
        Self {
            stable_for: Duration::from_secs(5 * 60),
            decision_interval: Duration::from_secs(30),
            sample_interval: Duration::from_secs(5),
            idle_interval: Duration::from_secs(5 * 60),
        }
    }
}

type PartitionKey = (String, String, u32);

/// What one sampling saw of a partition this node leads whose preferred node
/// is another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Observation {
    pub key: PartitionKey,
    pub preferred: String,
    pub leader_epoch: u32,
    pub generation: u64,
    /// The preferred node is in the live in-sync set and close to the log end.
    pub in_sync: bool,
}

/// What is done with a unit that is due.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DueKind {
    /// Led from this node alone, and its preferred node has been in sync for
    /// the stable time.
    Return { to: String },
    /// Led by several nodes, this one driving it: the lowest id among them.
    Split {
        preferred: Option<String>,
        /// The node that leads most of the unit (lowest id on a tie).
        home: String,
    },
}

/// A topic that may be moved now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DueUnit {
    pub org: String,
    pub topic: String,
    /// The partitions of the topic itself (its DLQ goes along).
    pub partitions: Vec<u32>,
    pub kind: DueKind,
}

#[derive(Debug, Clone)]
struct Clock {
    preferred: String,
    leader_epoch: u32,
    generation: u64,
    since: Instant,
}

/// The node every row of the unit names first: its preferred leader. Rows
/// that disagree, or name none, leave the unit with no preferred node. The
/// planner reads it the same way (`leader_balance_plan::build_units`).
pub(super) fn unanimous_preferred<'a>(rows: &[&'a PartitionAssignment]) -> Option<&'a str> {
    let first = rows.first()?.replicas.first()?;
    rows.iter()
        .all(|r| r.replicas.first() == Some(first))
        .then_some(first.as_str())
        .filter(|p| !p.is_empty())
}

/// Since when each partition's preferred node has been in sync, without a
/// break. A pure value: the clock is passed in.
#[derive(Debug, Default)]
pub(super) struct PreferredStability {
    since: HashMap<PartitionKey, Clock>,
}

impl PreferredStability {
    /// Takes one sampling. A partition that is not in `seen` (no longer led
    /// here, no longer displaced) or is seen out of sync loses its clock; one
    /// seen in sync keeps it, unless its preferred node, the epoch it is led
    /// at or the generation of its topic changed.
    pub fn observe(&mut self, now: Instant, seen: &[Observation]) {
        let mut next = HashMap::with_capacity(seen.len());
        for o in seen.iter().filter(|o| o.in_sync) {
            let since = match self.since.remove(&o.key) {
                Some(c)
                    if c.preferred == o.preferred
                        && c.leader_epoch == o.leader_epoch
                        && c.generation == o.generation =>
                {
                    c.since
                }
                _ => now,
            };
            next.insert(
                o.key.clone(),
                Clock {
                    preferred: o.preferred.clone(),
                    leader_epoch: o.leader_epoch,
                    generation: o.generation,
                    since,
                },
            );
        }
        self.since = next;
    }

    /// Starts every clock of the topic over (after a return that failed).
    pub fn restart_unit(&mut self, org: &str, unit: &str) {
        self.since
            .retain(|(o, topic, _), _| !(o == org && source_of(topic) == unit));
    }

    pub fn clear(&mut self) {
        self.since.clear();
    }

    fn stable(
        &self,
        row: &PartitionAssignment,
        preferred: &str,
        now: Instant,
        needed: Duration,
    ) -> bool {
        let key = (row.org_id.clone(), row.topic.clone(), row.partition);
        self.since.get(&key).is_some_and(|c| {
            c.preferred == preferred
                && c.leader_epoch == row.leader_epoch
                && c.generation == row.topic_generation
                && now.saturating_duration_since(c.since) >= needed
        })
    }

    /// The topics (in order of name) this node may move now.
    ///
    /// A topic led from this node alone goes back to its preferred node when
    /// every partition of it and its DLQ has had that node in sync for
    /// `needed`. A topic led by several nodes is driven by the lowest id among
    /// them, with no clock. Rows with no leader and internal topics are left
    /// alone, as is a topic with no partition of its own (a DLQ without a
    /// source).
    pub fn due_units(
        &self,
        rows: &[PartitionAssignment],
        local: &str,
        now: Instant,
        needed: Duration,
    ) -> Vec<DueUnit> {
        let mut units: BTreeMap<(&str, &str), Vec<&PartitionAssignment>> = BTreeMap::new();
        for row in rows {
            units
                .entry((row.org_id.as_str(), source_of(&row.topic)))
                .or_default()
                .push(row);
        }
        let mut due = Vec::new();
        for ((org, topic), members) in units {
            if is_internal(topic) || members.iter().any(|r| r.leader_node_id.is_empty()) {
                continue;
            }
            let mut partitions: Vec<u32> = members
                .iter()
                .filter(|r| r.topic == topic)
                .map(|r| r.partition)
                .collect();
            if partitions.is_empty() {
                continue;
            }
            partitions.sort_unstable();
            let mut led: BTreeMap<String, u32> = BTreeMap::new();
            for r in &members {
                *led.entry(r.leader_node_id.clone()).or_insert(0) += 1;
            }
            let preferred = unanimous_preferred(&members);
            let kind = if led.len() == 1 {
                if led.keys().next().map(String::as_str) != Some(local) {
                    continue;
                }
                let Some(to) = preferred.filter(|p| *p != local) else {
                    continue;
                };
                if !members.iter().all(|r| self.stable(r, to, now, needed)) {
                    continue;
                }
                DueKind::Return { to: to.to_string() }
            } else {
                if led.keys().next().map(String::as_str) != Some(local) {
                    continue;
                }
                DueKind::Split {
                    preferred: preferred.map(str::to_string),
                    home: majority(&led),
                }
            };
            due.push(DueUnit {
                org: org.to_string(),
                topic: topic.to_string(),
                partitions,
                kind,
            });
        }
        due
    }
}

/// The state of the rule on one `BusService`.
#[derive(Debug, Default)]
pub(super) struct LeaderReturnState {
    inner: parking_lot::Mutex<StateInner>,
}

#[derive(Debug, Default)]
struct StateInner {
    timing: LeaderReturnTiming,
    stability: PreferredStability,
    last_look: Option<Instant>,
    /// Units a gather failed for, not tried again before the instant.
    backoff: HashMap<(String, String), Instant>,
    last_switch_warning: Option<Instant>,
}

/// How one tick of the rule ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaderReturnTick {
    NoReplication,
    /// The instance has one node: nothing to return to, and the loop may
    /// sleep long.
    SingleNode,
    /// The switch is off (or cannot be read): nothing is moved and the
    /// clocks are dropped, so turning it on starts them from zero.
    Disabled,
    /// Sampled only: the last look was less than `decision_interval` ago.
    Sampled,
    /// A manual move, a rebalance or the spread runs: this look is skipped.
    Busy,
    /// Looked, and no topic is due and unblocked (or none is displaced).
    Idle,
    /// The node is shutting down: nothing was started.
    Cancelled,
    /// The topic went back to its preferred node.
    Returned {
        org: String,
        topic: String,
        to: String,
    },
    /// A split topic was gathered on `to`.
    Gathered {
        org: String,
        topic: String,
        to: String,
    },
    /// The move failed. With nothing moved (or everything handed back) no
    /// partition changed node; `partial` says some are stranded on the
    /// target or were moved on by someone else, so the topic may be split:
    /// it is gathered at the next look. A failed return starts the topic's
    /// clocks over; a failed gather waits two intervals.
    Failed {
        org: String,
        topic: String,
        to: String,
        code: String,
        partial: bool,
    },
}

impl BusService {
    /// Replaces the timings of the rule: for tests, which cannot wait minutes.
    pub fn set_leader_return_timing(&self, timing: LeaderReturnTiming) {
        self.leader_return.inner.lock().timing = timing;
    }

    pub fn leader_return_timing(&self) -> LeaderReturnTiming {
        self.leader_return.inner.lock().timing
    }

    /// Whether this node moves the topics it leads back to their preferred
    /// nodes and gathers the split ones. On unless the instance's key says
    /// otherwise; a value that does not read, or a key that cannot be read,
    /// is off: nothing moves on a guess.
    pub fn leader_return_enabled(&self) -> bool {
        let key = leader_return_setting_key(&self.instance_id);
        let answer = match crate::db::repository::get_setting(&self.db, &key) {
            Ok(None) => return true,
            Ok(Some(raw)) => parse_leader_return_setting(&raw).ok_or(format!("value '{raw}'")),
            Err(e) => Err(format!("error: {e}")),
        };
        answer.unwrap_or_else(|problem| {
            if self.switch_warning_due() {
                tracing::warn!(setting = %key, %problem, "the leader return switch cannot be read; topics are not moved automatically");
            }
            false
        })
    }

    pub(super) fn switch_warning_due(&self) -> bool {
        let mut state = self.leader_return.inner.lock();
        let due = state
            .last_switch_warning
            .is_none_or(|at| at.elapsed() >= SWITCH_WARNING_EVERY);
        if due {
            state.last_switch_warning = Some(Instant::now());
        }
        due
    }

    /// One tick of the rule with the real clock. Blocks on the ledger and on
    /// other nodes: call it from a blocking context.
    pub fn leader_return_tick(&self) -> LeaderReturnTick {
        self.leader_return_tick_at(Instant::now(), &|| false)
    }

    /// `leader_return_tick` at the instant `now`; `cancelled` is asked before
    /// a move is started (the node is shutting down).
    pub fn leader_return_tick_at(
        &self,
        now: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> LeaderReturnTick {
        let Some(coordinator) = self.replication() else {
            return LeaderReturnTick::NoReplication;
        };
        if !self.leader_return_enabled() {
            self.leader_return.inner.lock().stability.clear();
            return LeaderReturnTick::Disabled;
        }
        let Ok(rows) = coordinator.instance_assignments() else {
            return LeaderReturnTick::Sampled;
        };
        let local = coordinator.local_node_id();
        let nodes: HashSet<&str> = rows
            .iter()
            .flat_map(|r| r.replicas.iter().map(String::as_str))
            .chain(rows.iter().map(|r| r.leader_node_id.as_str()))
            .chain(std::iter::once(local.as_str()))
            .filter(|n| !n.is_empty())
            .collect();
        if nodes.len() < 2 {
            return LeaderReturnTick::SingleNode;
        }
        let displaced = |r: &&PartitionAssignment| {
            r.leader_node_id == local && r.replicas.first().is_some_and(|p| *p != local)
        };
        let has_split = self
            .leader_return
            .inner
            .lock()
            .stability
            .due_units(&rows, &local, now, Duration::MAX)
            .iter()
            .any(|u| matches!(u.kind, DueKind::Split { .. }));
        if !rows.iter().any(|r| displaced(&r)) && !has_split {
            self.leader_return.inner.lock().stability.clear();
            return LeaderReturnTick::Idle;
        }
        let seen = self.sample_preferred_nodes(&coordinator, &rows, &local);
        let (timing, look) = {
            let mut state = self.leader_return.inner.lock();
            state.stability.observe(now, &seen);
            let due = state.last_look.is_none_or(|at| {
                now.saturating_duration_since(at) >= state.timing.decision_interval
            });
            (state.timing, due)
        };
        if !look {
            return LeaderReturnTick::Sampled;
        }
        let Some(_running) = self.leader_balance_run.try_lock() else {
            return LeaderReturnTick::Busy;
        };
        {
            let mut state = self.leader_return.inner.lock();
            state.last_look = Some(now);
            state.backoff.retain(|_, until| *until > now);
        }
        // Read again under the lock: a run that has just ended may have moved
        // things.
        let Ok(rows) = coordinator.instance_assignments() else {
            return LeaderReturnTick::Idle;
        };
        // Read once for the whole look.
        let Some(paused) = self.paused_units() else {
            return LeaderReturnTick::Idle;
        };
        let due = self.leader_return.inner.lock().stability.due_units(
            &rows,
            &local,
            now,
            timing.stable_for,
        );
        for unit in due {
            if paused.contains(&(unit.org.clone(), unit.topic.clone()))
                || self
                    .leader_return
                    .inner
                    .lock()
                    .backoff
                    .contains_key(&(unit.org.clone(), unit.topic.clone()))
            {
                continue;
            }
            let Some((to, keep_order)) = self.target_of(&coordinator, &rows, &local, &unit) else {
                continue;
            };
            if cancelled() {
                return LeaderReturnTick::Cancelled;
            }
            return self.move_due_unit(&coordinator, timing, now, unit, to, keep_order);
        }
        LeaderReturnTick::Idle
    }

    /// The consumer groups that are paused, as `(organisation, source topic)`:
    /// a paused topic is not moved. `None` when they cannot be read.
    fn paused_units(&self) -> Option<HashSet<(String, String)>> {
        match crate::db::repository::bus_group_list_all(&self.local_db) {
            Ok(groups) => Some(
                groups
                    .into_iter()
                    .filter(|g| g.paused)
                    .map(|g| (g.org_id, source_of(&g.topic).to_string()))
                    .collect(),
            ),
            Err(e) => {
                tracing::warn!(error = %e, "leader return: the consumer groups cannot be read");
                None
            }
        }
    }

    /// Where the unit goes and whether the replica order is kept, or `None`
    /// when it cannot go anywhere now.
    ///
    /// A return goes to its preferred node when that node takes forwarded
    /// requests. A split unit goes to the preferred node when it does and the
    /// leader of every stray partition says it could hand it over now (live
    /// ISR, caught up); otherwise onto the node that leads most of it, with
    /// the replica order kept.
    fn target_of(
        &self,
        coordinator: &Arc<dyn ReplicationCoordinator>,
        rows: &[PartitionAssignment],
        local: &str,
        unit: &DueUnit,
    ) -> Option<(String, bool)> {
        let capable = |node: &str| {
            node == local
                || coordinator
                    .node_forwarding(&[node.to_string()], PLAN_PROBE_BUDGET)
                    .first()
                    .is_some_and(|state| *state == NodeForwarding::Capable)
        };
        match &unit.kind {
            DueKind::Return { to } => {
                if capable(to) {
                    Some((to.clone(), false))
                } else {
                    tracing::debug!(topic = %unit.topic, node = %to, "leader return: the preferred node does not take forwarded requests (yet)");
                    None
                }
            }
            DueKind::Split { preferred, home } => {
                let unit_rows = Self::unit_rows(rows, &unit.org, &unit.topic);
                if let Some(p) = preferred.as_deref().filter(|p| capable(p)) {
                    let all_can_go = unit_rows.iter().filter(|r| r.leader_node_id != p).all(|r| {
                        coordinator
                            .check_transfer(&unit.org, &r.topic, r.partition, p)
                            .is_ok()
                    });
                    if all_can_go {
                        return Some((p.to_string(), false));
                    }
                }
                // Gathering on the preferred node when it already leads most
                // of the unit and cannot take the rest would change nothing.
                (preferred.as_deref() != Some(home.as_str())).then(|| (home.clone(), true))
            }
        }
    }

    fn move_due_unit(
        &self,
        coordinator: &Arc<dyn ReplicationCoordinator>,
        timing: LeaderReturnTiming,
        now: Instant,
        unit: DueUnit,
        to: String,
        keep_order: bool,
    ) -> LeaderReturnTick {
        let is_return = matches!(unit.kind, DueKind::Return { .. });
        let preferred_restore = !keep_order;
        let run = RunContext {
            actor: None,
            reason: if preferred_restore {
                PREFERRED_RESTORE_REASON
            } else {
                SPLIT_GATHER_REASON
            },
        };
        let local = coordinator.local_node_id();
        let planned = PlannedMove {
            org_id: unit.org.clone(),
            topic: unit.topic.clone(),
            partitions: unit.partitions.clone(),
            from: local,
            to: to.clone(),
            returns_to_preferred: preferred_restore,
            isr_uncertain: false,
        };
        let results = self.execute_plan(coordinator, run, vec![planned], true, keep_order);
        let failed = results.iter().find(|r| r.status != MoveStatus::Moved);
        let org = unit.org;
        let topic = unit.topic;
        let Some(r) = failed else {
            tracing::info!(%org, %topic, %to, gather = !is_return, "leader return: the topic was moved");
            return if is_return {
                LeaderReturnTick::Returned { org, topic, to }
            } else {
                LeaderReturnTick::Gathered { org, topic, to }
            };
        };
        let code = r
            .error_code
            .clone()
            .unwrap_or_else(|| r.status.as_str().to_string());
        let partial = r.status == MoveStatus::FailedPartial;
        tracing::warn!(%org, %topic, %to, %code, partial, "leader return: the topic could not be moved");
        let mut state = self.leader_return.inner.lock();
        if partial {
            // Split now: gathered at the next look, with the clocks as they
            // are.
        } else if is_return {
            state.stability.restart_unit(&org, &topic);
        } else {
            let until = now + timing.decision_interval * 2;
            state.backoff.insert((org.clone(), topic.clone()), until);
        }
        LeaderReturnTick::Failed {
            org,
            topic,
            to,
            code,
            partial,
        }
    }

    /// The in-sync state of the preferred node of every partition this node
    /// leads and whose preferred node is another, off the live state of this
    /// node's own partition leaders. Only the organisations of those
    /// partitions are looked at.
    fn sample_preferred_nodes(
        &self,
        coordinator: &Arc<dyn ReplicationCoordinator>,
        rows: &[PartitionAssignment],
        local: &str,
    ) -> Vec<Observation> {
        let displaced: Vec<(&PartitionAssignment, &String)> = rows
            .iter()
            .filter(|r| r.leader_node_id == local)
            .filter_map(|r| {
                r.replicas
                    .first()
                    .filter(|p| p.as_str() != local && !p.is_empty())
                    .map(|p| (r, p))
            })
            .collect();
        let mut live: HashMap<PartitionKey, Vec<String>> = HashMap::new();
        let orgs: std::collections::BTreeSet<&str> =
            displaced.iter().map(|(r, _)| r.org_id.as_str()).collect();
        for org in orgs {
            for p in coordinator.snapshot(org, None).partitions {
                if p.leader_node_id.as_deref() == Some(local) {
                    live.insert((org.to_string(), p.topic, p.partition), p.isr);
                }
            }
        }
        displaced
            .into_iter()
            .map(|(r, preferred)| {
                let key = (r.org_id.clone(), r.topic.clone(), r.partition);
                let in_sync = live.get(&key).is_some_and(|isr| isr.contains(preferred))
                    && coordinator
                        .replica_record_gap(&r.org_id, &r.topic, r.partition, preferred)
                        .is_some_and(|gap| gap <= PREFERRED_MAX_LAG_RECORDS);
                Observation {
                    key,
                    preferred: preferred.clone(),
                    leader_epoch: r.leader_epoch,
                    generation: r.topic_generation,
                    in_sync,
                }
            })
            .collect()
    }
}
