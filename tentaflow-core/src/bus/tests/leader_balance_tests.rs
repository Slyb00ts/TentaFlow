//! `leader_balance`: the planning and the moves of leaders on a `BusService`
//! whose coordinator is scripted, so each failure can be put exactly where a
//! scenario needs it. The in-process cluster tests are in
//! `tests/bus_replication_three_node.rs`.

use std::collections::{BTreeMap, HashMap, HashSet};

use parking_lot::Mutex;

use super::super::leader_balance::{
    LeaderBalanceError, MoveStatus, RequestedMove, RunContext, SpreadStatus,
};
use super::super::replication::assignment::PartitionAssignment;
use super::*;

pub(super) const ORG: &str = "org-1";

pub(super) fn row(
    topic: &str,
    partition: u32,
    leader: &str,
    replicas: &[&str],
) -> PartitionAssignment {
    PartitionAssignment {
        instance_id: "tentabus-00000001".to_string(),
        org_id: ORG.to_string(),
        topic: topic.to_string(),
        partition,
        leader_node_id: leader.to_string(),
        replicas: replicas.iter().map(|s| s.to_string()).collect(),
        isr: replicas.iter().map(|s| s.to_string()).collect(),
        leader_epoch: 1,
        updated_at_ms: 0,
        topic_generation: 1,
        epoch_slots: Default::default(),
    }
}

/// A coordinator holding the assignments itself: a transfer rewrites its row
/// the way the ledger would, and the ones named in `failing` are refused.
pub(super) struct ScriptedCoordinator {
    local: String,
    pub(super) rows: Mutex<Vec<PartitionAssignment>>,
    pub(super) forwarding: Mutex<HashMap<String, NodeForwarding>>,
    /// `(topic, partition)` whose real transfer fails (after the precheck).
    pub(super) failing: Mutex<HashSet<(String, u32)>>,
    /// `(topic, partition)` the dry run refuses as a paused group.
    refusing: Mutex<HashSet<(String, u32)>>,
    /// `(topic, partition)` the dry run refuses as a target that is behind.
    pub(super) behind: Mutex<HashSet<(String, u32)>>,
    /// Nodes the dry run refuses as a target that is behind, for any partition.
    pub(super) behind_nodes: Mutex<HashSet<String>>,
    /// `(topic, partition)` whose first real transfer gets no answer; the
    /// flag says whether the move was made all the same.
    unknown: Mutex<HashMap<(String, u32), bool>>,
    /// Every transfer made, as `topic/partition->target`, in order.
    pub(super) transfers: Mutex<Vec<String>>,
    /// The live state of the partitions this node leads, as the snapshot
    /// reports it: `(topic, partition)` to the in-sync set. A partition with
    /// no entry is not in the snapshot.
    pub(super) live: Mutex<HashMap<(String, u32), Vec<String>>>,
    /// How many records the preferred node is behind, per partition (0 when
    /// not listed): `replica_record_gap`.
    pub(super) gaps: Mutex<HashMap<(String, u32), u64>>,
    /// Who led each partition when the coordinator was made: a transfer back
    /// to it is a rollback, which `failing` and `unknown` do not touch.
    first_leader: HashMap<(String, u32), String>,
    /// Runs before every checked transfer, with no lock held: a stand-in for
    /// someone else moving a partition in the meantime.
    pub(super) before_transfer:
        Mutex<Option<Box<dyn FnMut(&ScriptedCoordinator, &str, u32) + Send>>>,
}

impl ScriptedCoordinator {
    fn new(local: &str, rows: Vec<PartitionAssignment>) -> Arc<Self> {
        let first_leader = rows
            .iter()
            .map(|r| ((r.topic.clone(), r.partition), r.leader_node_id.clone()))
            .collect();
        Arc::new(Self {
            first_leader,
            gaps: Mutex::new(HashMap::new()),
            before_transfer: Mutex::new(None),
            local: local.to_string(),
            rows: Mutex::new(rows),
            forwarding: Mutex::new(HashMap::new()),
            failing: Mutex::new(HashSet::new()),
            refusing: Mutex::new(HashSet::new()),
            behind: Mutex::new(HashSet::new()),
            behind_nodes: Mutex::new(HashSet::new()),
            unknown: Mutex::new(HashMap::new()),
            transfers: Mutex::new(Vec::new()),
            live: Mutex::new(HashMap::new()),
        })
    }

    pub(super) fn leaders_of(&self, topic: &str) -> Vec<String> {
        let mut rows: Vec<_> = self
            .rows
            .lock()
            .iter()
            .filter(|r| r.topic == topic)
            .cloned()
            .collect();
        rows.sort_by_key(|r| r.partition);
        rows.into_iter().map(|r| r.leader_node_id).collect()
    }

    pub(super) fn led_by(&self, node: &str) -> usize {
        self.rows
            .lock()
            .iter()
            .filter(|r| r.leader_node_id == node)
            .count()
    }

    fn move_row(
        &self,
        topic: &str,
        partition: u32,
        target: &str,
        replicas: Result<&str, &[String]>,
        forward: bool,
    ) -> Result<u32, ReplError> {
        if self
            .failing
            .lock()
            .contains(&(topic.to_string(), partition))
            && forward
        {
            return Err(ReplError::TransferTargetBehind {
                topic: topic.to_string(),
                partition,
                node_id: target.to_string(),
            });
        }
        let silent = if forward {
            self.unknown.lock().remove(&(topic.to_string(), partition))
        } else {
            None
        };
        if silent == Some(false) {
            return Err(ReplError::TransferOutcomeUnknown {
                topic: topic.to_string(),
                partition,
            });
        }
        let mut rows = self.rows.lock();
        let row = rows
            .iter_mut()
            .find(|r| r.topic == topic && r.partition == partition)
            .ok_or_else(|| ReplError::NoAssignment {
                topic: topic.to_string(),
                partition,
            })?;
        row.leader_node_id = target.to_string();
        match replicas {
            Ok(first) => {
                row.replicas.retain(|n| n != first);
                row.replicas.insert(0, first.to_string());
            }
            Err(exact) => row.replicas = exact.to_vec(),
        }
        row.leader_epoch += 1;
        self.transfers
            .lock()
            .push(format!("{topic}/{partition}->{target}"));
        if silent == Some(true) {
            return Err(ReplError::TransferOutcomeUnknown {
                topic: topic.to_string(),
                partition,
            });
        }
        Ok(row.leader_epoch)
    }
}

impl ReplicationCoordinator for ScriptedCoordinator {
    fn role(&self, _: &str, _: &str, _: u32) -> PartitionRole {
        PartitionRole::Unavailable {
            reason: UnavailableReason::NoAssignment,
        }
    }
    fn preflight(&self, _: &str, _: &str, _: u32, _: topics::Acks) -> Result<u32, ReplError> {
        Ok(1)
    }
    fn await_acks(
        &self,
        _: &str,
        _: &str,
        _: u32,
        _: u64,
        _: topics::Acks,
        _: Duration,
    ) -> Result<AckOutcome, ReplError> {
        Ok(AckOutcome {
            acked_nodes: 1,
            required: 1,
            hw: 0,
        })
    }
    fn note_offset_commit(&self, _: &str, _: &str, _: &str, _: u32, _: u64, _: u32) {}
    fn note_dlq_handled(&self, _: &str, _: &str, _: u32, _: u64) {}
    fn evict_node_from_replica_sets(&self, _: &str, _: &'static str) -> Result<u32, ReplError> {
        Ok(0)
    }
    fn transfer_leader(
        &self,
        _org: &str,
        topic: &str,
        partition: u32,
        target: &str,
    ) -> Result<u32, ReplError> {
        // A move to a node that is not the preferred one makes it preferred.
        self.move_row(topic, partition, target, Ok(target), true)
    }
    fn transfer_leader_restoring(
        &self,
        _org: &str,
        topic: &str,
        partition: u32,
        target: &str,
        replicas: &[String],
    ) -> Result<u32, ReplError> {
        self.move_row(topic, partition, target, Err(replicas), false)
    }
    fn transfer_leader_expecting(
        &self,
        _org: &str,
        topic: &str,
        partition: u32,
        target: &str,
        replicas: &[String],
        expected: &ExpectedLeader,
    ) -> Result<u32, ReplError> {
        if let Some(hook) = self.before_transfer.lock().as_mut() {
            hook(self, topic, partition);
        }
        let key = (topic.to_string(), partition);
        {
            let rows = self.rows.lock();
            let row = rows
                .iter()
                .find(|r| r.topic == topic && r.partition == partition)
                .ok_or_else(|| ReplError::NoAssignment {
                    topic: topic.to_string(),
                    partition,
                })?;
            if row.leader_node_id != expected.node_id || row.leader_epoch != expected.epoch {
                return Err(ReplError::NotPartitionLeader {
                    topic: topic.to_string(),
                    partition,
                    leader: row.leader_node_id.clone(),
                });
            }
        }
        let forward = self.first_leader.get(&key).map(String::as_str) != Some(target);
        if replicas.is_empty() {
            self.move_row(topic, partition, target, Ok(target), forward)
        } else {
            self.move_row(topic, partition, target, Err(replicas), forward)
        }
    }
    fn replica_record_gap(&self, _: &str, topic: &str, partition: u32, _: &str) -> Option<u64> {
        let led_here = self.rows.lock().iter().any(|r| {
            r.topic == topic && r.partition == partition && r.leader_node_id == self.local
        });
        led_here.then(|| {
            self.gaps
                .lock()
                .get(&(topic.to_string(), partition))
                .copied()
                .unwrap_or(0)
        })
    }
    fn check_transfer(
        &self,
        _org: &str,
        topic: &str,
        partition: u32,
        target: &str,
    ) -> Result<(), ReplError> {
        if self.behind.lock().contains(&(topic.to_string(), partition))
            || self.behind_nodes.lock().contains(target)
        {
            return Err(ReplError::TransferTargetBehind {
                topic: topic.to_string(),
                partition,
                node_id: target.to_string(),
            });
        }
        if self
            .refusing
            .lock()
            .contains(&(topic.to_string(), partition))
        {
            return Err(ReplError::TransferBlocked {
                topic: topic.to_string(),
                reason: "consumer_group_paused".to_string(),
            });
        }
        Ok(())
    }
    fn reassign(&self, _: &str, _: &str, _: Option<u32>, _: &[String]) -> Result<u32, ReplError> {
        Ok(0)
    }
    fn snapshot(&self, _: &str, _: Option<&str>) -> ReplicationSnapshot {
        let live = self.live.lock();
        let partitions = self
            .rows
            .lock()
            .iter()
            .filter(|r| r.leader_node_id == self.local)
            .filter_map(|r| {
                let isr = live.get(&(r.topic.clone(), r.partition))?;
                Some(PartitionReplicaInfo {
                    topic: r.topic.clone(),
                    partition: r.partition,
                    leader_node_id: Some(r.leader_node_id.clone()),
                    leader_epoch: r.leader_epoch,
                    replicas: r.replicas.clone(),
                    isr: isr.clone(),
                    lagging: Vec::new(),
                    high_watermark: 0,
                    log_end_offset: 0,
                    unavailable_reason: None,
                })
            })
            .collect();
        ReplicationSnapshot {
            nodes: Vec::new(),
            partitions,
            failovers: Vec::new(),
        }
    }
    fn local_node_id(&self) -> String {
        self.local.clone()
    }
    fn node_forwarding(&self, nodes: &[String], _: Duration) -> Vec<NodeForwarding> {
        let known = self.forwarding.lock();
        nodes
            .iter()
            .map(|n| known.get(n).copied().unwrap_or(NodeForwarding::Capable))
            .collect()
    }
    fn instance_assignments(&self) -> Result<Vec<PartitionAssignment>, ReplError> {
        let mut rows = self.rows.lock().clone();
        rows.sort_by(|a, b| (&a.topic, a.partition).cmp(&(&b.topic, b.partition)));
        Ok(rows)
    }
}

pub(super) fn service_with(
    local: &str,
    rows: Vec<PartitionAssignment>,
) -> (tempfile::TempDir, BusService, Arc<ScriptedCoordinator>) {
    let (dir, svc) = test_service();
    let coordinator = ScriptedCoordinator::new(local, rows);
    svc.set_replication(Arc::clone(&coordinator) as Arc<dyn ReplicationCoordinator>);
    (dir, svc, coordinator)
}

pub(super) fn audit_details(svc: &BusService, action: &str) -> Vec<String> {
    let conn = svc.db.read().expect("db read");
    let mut stmt = conn
        .prepare("SELECT COALESCE(details, '') FROM audit_log WHERE action = ?1 ORDER BY rowid")
        .expect("prepare");
    stmt.query_map([action], |r| r.get::<_, String>(0))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect()
}

fn four_topics_on(node: &str, other: &str) -> Vec<PartitionAssignment> {
    ["a", "b", "c", "d"]
        .iter()
        .map(|t| row(t, 0, node, &[node, other]))
        .collect()
}

fn manual() -> RunContext<'static> {
    RunContext {
        actor: Some("admin"),
        reason: "manual",
    }
}

#[test]
fn a_whole_topic_move_that_fails_midway_hands_every_partition_back_and_audits_it() {
    let mut rows = vec![
        row("orders", 0, "n1", &["n1", "n2"]),
        row("orders", 1, "n1", &["n1", "n2"]),
        row("__dlq.orders", 0, "n1", &["n1", "n2"]),
    ];
    rows.push(row("other", 0, "n1", &["n1", "n2"]));
    let (_dir, svc, coordinator) = service_with("n1", rows);
    coordinator.failing.lock().insert(("orders".to_string(), 1));

    let failure = svc
        .move_topic_leadership(manual(), ORG, "orders", "n2")
        .expect_err("the last partition cannot be moved");
    assert_eq!(failure.status, MoveStatus::FailedRolledBack);
    assert!(matches!(
        failure.cause,
        ReplError::TransferTargetBehind { .. }
    ));

    for topic in ["orders", "__dlq.orders", "other"] {
        assert!(
            coordinator.leaders_of(topic).iter().all(|l| l == "n1"),
            "{topic} must be back on its node"
        );
    }
    for r in coordinator.rows.lock().iter() {
        assert_eq!(r.replicas[0], "n1", "{} keeps n1 as preferred", r.topic);
    }
    let transfers = audit_details(&svc, "bus.leader.transfer");
    assert_eq!(
        transfers.len(),
        4,
        "two moves and their two rollbacks: {transfers:?}"
    );
    assert_eq!(
        transfers
            .iter()
            .filter(|d| d.contains("reason=rollback"))
            .count(),
        2
    );
    assert_eq!(
        transfers
            .iter()
            .filter(|d| d.contains("reason=manual"))
            .count(),
        2
    );
}

#[test]
fn a_precheck_refusal_moves_nothing_and_needs_no_rollback() {
    let rows = vec![
        row("orders", 0, "n1", &["n1", "n2"]),
        row("orders", 1, "n1", &["n1", "n2"]),
    ];
    let (_dir, svc, coordinator) = service_with("n1", rows);
    coordinator
        .refusing
        .lock()
        .insert(("orders".to_string(), 1));

    let failure = svc
        .move_topic_leadership(manual(), ORG, "orders", "n2")
        .expect_err("refused");
    assert_eq!(failure.status, MoveStatus::Failed);
    assert!(matches!(failure.cause, ReplError::TransferBlocked { .. }));
    assert!(coordinator.transfers.lock().is_empty());
    assert!(audit_details(&svc, "bus.leader.transfer").is_empty());
}

#[test]
fn a_plan_spreads_four_topics_two_and_two_and_the_apply_does_it() {
    let (_dir, svc, coordinator) = service_with("n1", four_topics_on("n1", "n2"));

    let plan = svc
        .leader_balance_plan(Some(ORG), None, &|_, _| true)
        .expect("plan");
    assert_eq!(plan.moves.len(), 2, "the fewest moves that even it out");
    assert!(plan.blockers.is_empty());
    let requested: Vec<RequestedMove> = plan
        .moves
        .iter()
        .map(|m| RequestedMove {
            topic: m.topic.clone(),
            partitions: m.partitions.clone(),
            from_node_id: m.from.clone(),
            to_node_id: m.to.clone(),
        })
        .collect();

    let run = RunContext {
        actor: Some("admin"),
        reason: "rebalance",
    };
    let results = svc
        .leader_balance_apply(ORG, run, &requested, &|_, _| true)
        .expect("apply");
    assert!(results.iter().all(|r| r.status == MoveStatus::Moved));
    assert_eq!(coordinator.led_by("n1"), 2);
    assert_eq!(coordinator.led_by("n2"), 2);

    let summary = audit_details(&svc, "bus.leader.rebalance");
    assert_eq!(summary.len(), 1);
    assert!(
        summary[0].contains("moves=2 moved=2 result=ok"),
        "{summary:?}"
    );
    assert!(summary[0].contains("actor=admin reason=rebalance"));
    assert_eq!(audit_details(&svc, "bus.leader.transfer").len(), 2);
}

#[test]
fn an_apply_of_a_plan_that_no_longer_holds_is_refused_and_moves_nothing() {
    let (_dir, svc, coordinator) = service_with("n1", four_topics_on("n1", "n2"));
    let plan = svc
        .leader_balance_plan(Some(ORG), None, &|_, _| true)
        .expect("plan");
    let requested: Vec<RequestedMove> = plan
        .moves
        .iter()
        .map(|m| RequestedMove {
            topic: m.topic.clone(),
            partitions: m.partitions.clone(),
            from_node_id: m.from.clone(),
            to_node_id: m.to.clone(),
        })
        .collect();
    // Somebody moves a topic in between.
    svc.move_topic_leadership(manual(), ORG, "a", "n2")
        .expect("manual move");
    let before = coordinator.transfers.lock().len();

    let err = svc
        .leader_balance_apply(ORG, manual(), &requested, &|_, _| true)
        .expect_err("stale plan");
    assert!(matches!(err, LeaderBalanceError::PlanChanged), "{err:?}");
    assert_eq!(coordinator.transfers.lock().len(), before);
}

#[test]
fn an_apply_without_the_admin_right_on_a_topic_is_refused_before_anything_moves() {
    let (_dir, svc, coordinator) = service_with("n1", four_topics_on("n1", "n2"));
    let requested = vec![RequestedMove {
        topic: "a".to_string(),
        partitions: vec![0],
        from_node_id: "n1".to_string(),
        to_node_id: "n2".to_string(),
    }];
    let err = svc
        .leader_balance_apply(ORG, manual(), &requested, &|_, topic| topic != "a")
        .expect_err("no right");
    assert!(
        matches!(&err, LeaderBalanceError::PermissionDenied(t) if t == "a"),
        "{err:?}"
    );
    assert!(coordinator.transfers.lock().is_empty());
}

#[test]
fn a_topic_the_caller_cannot_administer_is_a_blocker_and_stays_put() {
    let (_dir, svc, coordinator) = service_with("n1", four_topics_on("n1", "n2"));
    let plan = svc
        .leader_balance_plan(Some(ORG), None, &|_, topic| topic == "d")
        .expect("plan");
    // Only `d` may move, which leaves a gap of two: the others are named.
    assert!(plan.moves.iter().all(|m| m.topic == "d"));
    assert!(plan
        .blockers
        .iter()
        .any(|b| b.topic == "a" && b.reason == "no_admin_right"));
    assert_eq!(coordinator.led_by("n1"), 4, "a plan changes nothing");
}

#[test]
fn a_topic_a_leader_refuses_in_the_dry_run_is_left_out_with_its_reason() {
    let (_dir, svc, coordinator) = service_with("n1", four_topics_on("n1", "n2"));
    for topic in ["a", "b", "c"] {
        coordinator.refusing.lock().insert((topic.to_string(), 0));
    }
    let plan = svc
        .leader_balance_plan(Some(ORG), None, &|_, _| true)
        .expect("plan");
    assert!(plan.moves.iter().all(|m| m.topic == "d"));
    let blocked: BTreeMap<&str, &str> = plan
        .blockers
        .iter()
        .map(|b| (b.topic.as_str(), b.reason.as_str()))
        .collect();
    assert_eq!(blocked.get("a"), Some(&"consumer_group_paused"));
    assert_eq!(blocked.get("b"), Some(&"consumer_group_paused"));
    assert_eq!(blocked.get("c"), Some(&"consumer_group_paused"));
}

// ---- The spread after an upgrade -------------------------------------------

fn spread_marker(svc: &BusService) -> Option<String> {
    crate::db::repository::get_setting(&svc.db, "bus.leader_spread:tentabus-00000001")
        .expect("read marker")
}

#[test]
fn the_spread_after_an_upgrade_runs_once_and_is_audited_as_the_system() {
    let (_dir, svc, coordinator) = service_with("n1", four_topics_on("n1", "n2"));

    assert_eq!(svc.run_post_upgrade_spread(), SpreadStatus::Done);
    assert_eq!(coordinator.led_by("n1"), 2);
    assert_eq!(coordinator.led_by("n2"), 2);
    assert_eq!(spread_marker(&svc).as_deref(), Some("done"));
    let summary = audit_details(&svc, "bus.leader.rebalance");
    assert_eq!(summary.len(), 1);
    assert!(
        summary[0].contains("actor=system reason=post_upgrade_spread"),
        "{summary:?}"
    );

    // Skew it again: the marker keeps the spread from running a second time.
    coordinator
        .move_row("a", 0, "n1", Ok("n1"), true)
        .expect("skew");
    coordinator
        .move_row("b", 0, "n1", Ok("n1"), true)
        .expect("skew");
    let transfers = coordinator.transfers.lock().len();
    assert_eq!(svc.run_post_upgrade_spread(), SpreadStatus::Done);
    assert_eq!(coordinator.transfers.lock().len(), transfers);
    assert_eq!(audit_details(&svc, "bus.leader.rebalance").len(), 1);
}

#[test]
fn the_spread_waits_while_a_peer_does_not_advertise_the_feature() {
    let (_dir, svc, coordinator) = service_with("n1", four_topics_on("n1", "n2"));
    coordinator
        .forwarding
        .lock()
        .insert("n2".to_string(), NodeForwarding::Lacking);

    assert_eq!(
        svc.run_post_upgrade_spread(),
        SpreadStatus::NotYet("a_node_lacks_the_feature")
    );
    assert!(coordinator.transfers.lock().is_empty());
    assert_eq!(spread_marker(&svc), None, "it is still due");

    coordinator
        .forwarding
        .lock()
        .insert("n2".to_string(), NodeForwarding::Capable);
    assert_eq!(svc.run_post_upgrade_spread(), SpreadStatus::Done);
    assert_eq!(coordinator.led_by("n2"), 2);
}

#[test]
fn only_the_node_with_the_lowest_id_runs_the_spread() {
    let (_dir, svc, coordinator) = service_with("n2", four_topics_on("n1", "n2"));
    assert_eq!(svc.run_post_upgrade_spread(), SpreadStatus::NotTheRunner);
    assert!(coordinator.transfers.lock().is_empty());
    assert_eq!(spread_marker(&svc), None);
    assert!(audit_details(&svc, "bus.leader.rebalance").is_empty());
}

#[test]
fn the_spread_skips_a_topic_it_cannot_move_and_says_why() {
    let (_dir, svc, coordinator) = service_with("n1", four_topics_on("n1", "n2"));
    for topic in ["a", "b", "c"] {
        coordinator.refusing.lock().insert((topic.to_string(), 0));
    }
    assert_eq!(svc.run_post_upgrade_spread(), SpreadStatus::Done);
    assert_eq!(coordinator.leaders_of("a"), vec!["n1"]);
    let skipped = audit_details(&svc, "bus.leader.rebalance_skipped");
    assert!(
        skipped
            .iter()
            .any(|d| d.contains("reason=consumer_group_paused")
                && d.contains("source=post_upgrade_spread")),
        "{skipped:?}"
    );
}

// ---- Review findings --------------------------------------------------------

fn planned_topics(svc: &BusService) -> Vec<String> {
    svc.leader_balance_plan(None, None, &|_, _| true)
        .expect("plan")
        .moves
        .into_iter()
        .map(|m| m.topic)
        .collect()
}

/// An answer that never came after partitions already moved: the move that got
/// no answer did not show in the rows, so where it is led is not known. The
/// partitions that moved are handed back, the topic is reported partial (not
/// "nothing moved"), and the cause is the stable unknown-outcome code.
#[test]
fn a_move_with_no_answer_after_partial_success_is_reported_unresolved_not_as_nothing_moved() {
    let rows = vec![
        row("orders", 0, "n1", &["n1", "n2"]),
        row("orders", 1, "n1", &["n1", "n2"]),
    ];
    let (_dir, svc, coordinator) = service_with("n1", rows);
    coordinator
        .unknown
        .lock()
        .insert(("orders".to_string(), 1), false);

    let failure = svc
        .move_topic_leadership(manual(), ORG, "orders", "n2")
        .expect_err("the second partition gives no answer");
    assert!(matches!(
        failure.cause,
        ReplError::TransferOutcomeUnknown { partition: 1, .. }
    ));
    assert_eq!(failure.status, MoveStatus::FailedPartial);
    assert_eq!(failure.unresolved, vec![("orders".to_string(), 1)]);
    assert!(failure.stranded.is_empty());
    assert_eq!(
        coordinator.leaders_of("orders")[0],
        "n1",
        "the partition that did move went back"
    );
}

/// No answer, but the row shows the move: it counts as made and the topic
/// completes.
#[test]
fn a_move_with_no_answer_whose_row_shows_it_counts_as_made() {
    let rows = vec![
        row("orders", 0, "n1", &["n1", "n2"]),
        row("orders", 1, "n1", &["n1", "n2"]),
    ];
    let (_dir, svc, coordinator) = service_with("n1", rows);
    coordinator
        .unknown
        .lock()
        .insert(("orders".to_string(), 1), true);

    let moved = svc
        .move_topic_leadership(manual(), ORG, "orders", "n2")
        .expect("the rows show both moved");
    assert_eq!(moved.moved.len(), 2);
    assert_eq!(coordinator.leaders_of("orders"), vec!["n2", "n2"]);
}

/// The apply reports the unknown outcome in the result of the move.
#[test]
fn an_apply_reports_the_unknown_outcome_with_its_code_and_where_it_is_not_known() {
    let rows = vec![
        row("orders", 0, "n1", &["n1", "n2"]),
        row("orders", 1, "n1", &["n1", "n2"]),
        row("other", 0, "n1", &["n1", "n2"]),
        row("other2", 0, "n1", &["n1", "n2"]),
    ];
    let (_dir, svc, coordinator) = service_with("n1", rows);
    let plan = svc
        .leader_balance_plan(Some(ORG), None, &|_, _| true)
        .expect("plan");
    let first = plan.moves[0].clone();
    coordinator
        .unknown
        .lock()
        .insert((first.topic.clone(), 0), false);
    let requested: Vec<RequestedMove> = plan
        .moves
        .iter()
        .map(|m| RequestedMove {
            topic: m.topic.clone(),
            partitions: m.partitions.clone(),
            from_node_id: m.from.clone(),
            to_node_id: m.to.clone(),
        })
        .collect();
    let results = svc
        .leader_balance_apply(ORG, manual(), &requested, &|_, _| true)
        .expect("apply");
    assert_eq!(results[0].status, MoveStatus::FailedPartial);
    assert_eq!(
        results[0].error_code.as_deref(),
        Some("bus.transfer_outcome_unknown")
    );
    assert!(results[0]
        .detail
        .as_deref()
        .is_some_and(|d| d.contains("is not known")));
}

#[test]
fn a_move_to_a_node_that_does_not_take_forwarded_requests_is_refused_with_its_own_code() {
    for state in [NodeForwarding::Lacking, NodeForwarding::Unknown] {
        let (_dir, svc, coordinator) = service_with("n1", four_topics_on("n1", "n2"));
        coordinator
            .forwarding
            .lock()
            .insert("n2".to_string(), state);

        let failure = svc
            .move_topic_leadership(manual(), ORG, "a", "n2")
            .expect_err("the target cannot take forwarded requests");
        assert!(
            matches!(failure.cause, ReplError::NodeWithoutForwarding { ref node_id } if node_id == "n2"),
            "{state:?}: {:?}",
            failure.cause
        );
        assert_eq!(failure.status, MoveStatus::Failed);
        assert!(coordinator.transfers.lock().is_empty());
        assert_eq!(
            super::super::leader_balance::error_code(&failure.cause),
            "bus.node_without_forwarding"
        );
    }
}

#[test]
fn a_second_rebalance_while_one_runs_is_refused_as_busy() {
    let (_dir, svc, coordinator) = service_with("n1", four_topics_on("n1", "n2"));
    let plan = svc
        .leader_balance_plan(Some(ORG), None, &|_, _| true)
        .expect("plan");
    let requested: Vec<RequestedMove> = plan
        .moves
        .iter()
        .map(|m| RequestedMove {
            topic: m.topic.clone(),
            partitions: m.partitions.clone(),
            from_node_id: m.from.clone(),
            to_node_id: m.to.clone(),
        })
        .collect();
    {
        let _running = svc.leader_balance_run.lock();
        let refused = svc.leader_balance_apply(ORG, manual(), &requested, &|_, _| true);
        assert!(matches!(refused, Err(LeaderBalanceError::Busy)));
        assert!(coordinator.transfers.lock().is_empty());
        assert_eq!(svc.run_post_upgrade_spread(), SpreadStatus::NotYet("busy"));
    }
    svc.leader_balance_apply(ORG, manual(), &requested, &|_, _| true)
        .expect("it runs once the other is done");
    assert_eq!(coordinator.led_by("n2"), 2);
}

/// One topic that fails does not stop the spread: the others still move, the
/// failure is audited as skipped, no marker is written, and the run is to be
/// repeated.
#[test]
fn the_spread_goes_on_past_a_failed_topic_and_keeps_the_marker_back_for_a_transient_reason() {
    let (_dir, svc, coordinator) = service_with("n1", four_topics_on("n1", "n2"));
    let planned = planned_topics(&svc);
    assert_eq!(planned.len(), 2);
    coordinator.failing.lock().insert((planned[0].clone(), 0));

    assert_eq!(svc.run_post_upgrade_spread(), SpreadStatus::Retry);
    assert_eq!(coordinator.leaders_of(&planned[0]), vec!["n1"]);
    assert_eq!(coordinator.leaders_of(&planned[1]), vec!["n2"]);
    assert_eq!(spread_marker(&svc), None);
    let skipped = audit_details(&svc, "bus.leader.rebalance_skipped");
    assert!(
        skipped
            .iter()
            .any(|d| d.contains("reason=bus.transfer_target_behind")),
        "{skipped:?}"
    );

    // The next run, with the target caught up, finishes and writes the marker.
    coordinator.failing.lock().clear();
    assert_eq!(svc.run_post_upgrade_spread(), SpreadStatus::Done);
    assert_eq!(coordinator.led_by("n2"), 2);
    assert_eq!(spread_marker(&svc).as_deref(), Some("done"));
}

/// A topic that is not moved for a reason that will be the same next time (a
/// paused group) does not hold the marker back, but is still audited.
#[test]
fn a_permanent_reason_does_not_hold_the_spread_marker_back() {
    let (_dir, svc, coordinator) = service_with("n1", four_topics_on("n1", "n2"));
    for topic in ["a", "b", "c", "d"] {
        coordinator.refusing.lock().insert((topic.to_string(), 0));
    }
    assert_eq!(svc.run_post_upgrade_spread(), SpreadStatus::Done);
    assert_eq!(spread_marker(&svc).as_deref(), Some("done"));
    assert!(audit_details(&svc, "bus.leader.rebalance_skipped")
        .iter()
        .any(|d| d.contains("reason=consumer_group_paused")));
}

/// A target that is behind in the dry run is a transient blocker: audited as
/// skipped, no marker.
#[test]
fn a_target_that_is_behind_in_the_dry_run_keeps_the_marker_back() {
    let (_dir, svc, coordinator) = service_with("n1", four_topics_on("n1", "n2"));
    coordinator.behind.lock().insert(("a".to_string(), 0));
    coordinator.behind.lock().insert(("b".to_string(), 0));
    coordinator.behind.lock().insert(("c".to_string(), 0));
    coordinator.behind.lock().insert(("d".to_string(), 0));
    assert_eq!(svc.run_post_upgrade_spread(), SpreadStatus::Retry);
    assert_eq!(spread_marker(&svc), None);
    assert!(audit_details(&svc, "bus.leader.rebalance_skipped")
        .iter()
        .any(|d| d.contains("reason=target_behind")));
}
