//! `leader_return`: a topic goes back to its preferred leader once that node
//! has stayed in the live in-sync set, and a split topic is gathered. The
//! clock is injected: a tick is made "at" an instant a number of seconds
//! after the start, so a stable five minutes costs no time.

use std::time::{Duration, Instant};

use super::super::replication::assignment::PartitionAssignment;
use super::leader_balance_tests::{audit_details, row, service_with, ScriptedCoordinator, ORG};
use super::*;
use crate::bus::leader_balance::MoveStatus;
use crate::bus::leader_return::{
    leader_return_setting_key, validate_switch_setting, LeaderReturnTick, LeaderReturnTiming,
    PREFERRED_MAX_LAG_RECORDS, PREFERRED_RESTORE_REASON, SPLIT_GATHER_REASON,
};

const SAMPLE: u64 = 5;

/// `orders` (two partitions) and its DLQ led by `n2` after a failover, `n1`
/// being the preferred leader of both; this node is `n2`.
fn failed_over_rows() -> Vec<PartitionAssignment> {
    vec![
        row("orders", 0, "n2", &["n1", "n2", "n3"]),
        row("orders", 1, "n2", &["n1", "n2", "n3"]),
        row("__dlq.orders", 0, "n2", &["n1", "n2", "n3"]),
    ]
}

/// The same topic after the partitions elected their leaders one by one: the
/// survivors `n2` and `n3` share it.
fn split_rows() -> Vec<PartitionAssignment> {
    vec![
        row("orders", 0, "n2", &["n1", "n2", "n3"]),
        row("orders", 1, "n3", &["n1", "n2", "n3"]),
        row("__dlq.orders", 0, "n2", &["n1", "n2", "n3"]),
    ]
}

fn everything_in_sync(coordinator: &ScriptedCoordinator) {
    let rows = coordinator.rows.lock().clone();
    for r in rows {
        set_in_sync(coordinator, &r.topic, r.partition, true);
    }
}

/// The preferred node `n1` is in (or out of) the live in-sync set of the
/// partition, as the leader sees it.
fn set_in_sync(coordinator: &ScriptedCoordinator, topic: &str, partition: u32, in_sync: bool) {
    let isr = if in_sync {
        vec!["n2".to_string(), "n1".to_string()]
    } else {
        vec!["n2".to_string()]
    };
    coordinator
        .live
        .lock()
        .insert((topic.to_string(), partition), isr);
}

struct Clock(Instant);

impl Clock {
    fn at(&self, secs: u64) -> Instant {
        self.0 + Duration::from_secs(secs)
    }
}

fn tick(svc: &BusService, clock: &Clock, secs: u64) -> LeaderReturnTick {
    svc.leader_return_tick_at(clock.at(secs), &|| false)
}

/// Ticks every `SAMPLE` seconds from `from` (exclusive) to `to` (inclusive)
/// and returns what the ticks that moved or failed to move something
/// answered, with the second.
fn run(svc: &BusService, clock: &Clock, from: u64, to: u64) -> Vec<(u64, LeaderReturnTick)> {
    let mut out = Vec::new();
    let mut at = from + SAMPLE;
    while at <= to {
        let answer = tick(svc, clock, at);
        if matches!(
            answer,
            LeaderReturnTick::Returned { .. }
                | LeaderReturnTick::Gathered { .. }
                | LeaderReturnTick::Failed { .. }
        ) {
            out.push((at, answer));
        }
        at += SAMPLE;
    }
    out
}

fn returned(topic: &str) -> LeaderReturnTick {
    LeaderReturnTick::Returned {
        org: ORG.to_string(),
        topic: topic.to_string(),
        to: "n1".to_string(),
    }
}

fn gathered(topic: &str, to: &str) -> LeaderReturnTick {
    LeaderReturnTick::Gathered {
        org: ORG.to_string(),
        topic: topic.to_string(),
        to: to.to_string(),
    }
}

type Started = (
    tempfile::TempDir,
    BusService,
    Arc<ScriptedCoordinator>,
    Clock,
);

/// A service on `local` with `rows`, everything in sync, and nothing ticked.
fn start_on(local: &str, rows: Vec<PartitionAssignment>) -> Started {
    let (dir, svc, coordinator) = service_with(local, rows);
    everything_in_sync(&coordinator);
    (dir, svc, coordinator, Clock(Instant::now()))
}

/// `start_on` the node `n2`, after the first sampling (the start of every
/// clock).
fn start(rows: Vec<PartitionAssignment>) -> Started {
    let started = start_on("n2", rows);
    // The first sampling is the start of every clock.
    assert_eq!(tick(&started.1, &started.3, 0), LeaderReturnTick::Idle);
    started
}

#[test]
fn a_topic_goes_back_after_a_stable_five_minutes_with_its_dlq_and_its_replica_order() {
    let (_dir, svc, coordinator, clock) = start(failed_over_rows());

    assert!(
        run(&svc, &clock, 0, 295).is_empty(),
        "not before five minutes"
    );
    assert!(coordinator.transfers.lock().is_empty());

    let done = run(&svc, &clock, 295, 330);
    assert_eq!(done, vec![(300, returned("orders"))], "{done:?}");
    for topic in ["orders", "__dlq.orders"] {
        assert!(
            coordinator.leaders_of(topic).iter().all(|l| l == "n1"),
            "{topic} is led by the preferred node"
        );
    }
    for r in coordinator.rows.lock().iter() {
        assert_eq!(
            r.replicas,
            vec!["n1", "n2", "n3"],
            "a return leaves the replica order alone: {}",
            r.topic
        );
    }

    let transfers = audit_details(&svc, "bus.leader.transfer");
    assert_eq!(transfers.len(), 3, "{transfers:?}");
    assert!(transfers.iter().all(
        |d| d.contains(&format!("reason={PREFERRED_RESTORE_REASON}"))
            && d.contains("target_node_id=n1")
    ));
    let summary = audit_details(&svc, "bus.leader.rebalance");
    assert_eq!(summary.len(), 1, "{summary:?}");
    assert!(
        summary[0].contains("actor=system reason=preferred_restore moves=1 moved=1 result=ok"),
        "{summary:?}"
    );
}

#[test]
fn a_node_that_drops_out_of_the_in_sync_set_restarts_the_clock_and_nothing_bounces() {
    let (_dir, svc, coordinator, clock) = start(failed_over_rows());
    assert!(run(&svc, &clock, 0, 120).is_empty());

    // Out for one sample, then back: the five minutes count from the return.
    set_in_sync(&coordinator, "orders", 1, false);
    assert!(run(&svc, &clock, 120, 125).is_empty());
    set_in_sync(&coordinator, "orders", 1, true);

    assert!(
        run(&svc, &clock, 125, 420).is_empty(),
        "300 s since the start is not 300 s since the flap"
    );
    let done = run(&svc, &clock, 420, 450);
    assert_eq!(done, vec![(450, returned("orders"))], "{done:?}");
    assert_eq!(
        coordinator.transfers.lock().len(),
        3,
        "one move of the unit"
    );
}

#[test]
fn a_node_that_keeps_flapping_never_gets_the_topic() {
    let (_dir, svc, coordinator, clock) = start(failed_over_rows());
    for round in 0..8u64 {
        let base = round * 60;
        let moved = run(&svc, &clock, base, base + 40);
        assert!(moved.is_empty());
        set_in_sync(&coordinator, "orders", 0, false);
        run(&svc, &clock, base + 40, base + 45);
        set_in_sync(&coordinator, "orders", 0, true);
        run(&svc, &clock, base + 45, base + 60);
    }
    assert!(coordinator.transfers.lock().is_empty());
}

#[test]
fn a_partition_of_the_dlq_that_is_out_of_sync_holds_the_whole_topic() {
    let (_dir, svc, coordinator, clock) = start(failed_over_rows());
    set_in_sync(&coordinator, "__dlq.orders", 0, false);
    assert!(run(&svc, &clock, 0, 900).is_empty());
    assert!(coordinator.transfers.lock().is_empty());
}

/// "No lag" is the record gap of the replica to the leader's log end, which
/// the real leader handle reports (`replica_progress`); a replica that is in
/// the in-sync set but further behind than the tolerance is not in sync for
/// the clock, and one exactly at it is.
#[test]
fn a_preferred_node_further_behind_than_the_tolerance_is_not_in_sync_for_the_clock() {
    let (_dir, svc, coordinator, clock) = start(failed_over_rows());
    coordinator
        .gaps
        .lock()
        .insert(("orders".to_string(), 1), PREFERRED_MAX_LAG_RECORDS + 1);
    assert!(run(&svc, &clock, 0, 900).is_empty());
    assert!(coordinator.transfers.lock().is_empty());

    coordinator
        .gaps
        .lock()
        .insert(("orders".to_string(), 1), PREFERRED_MAX_LAG_RECORDS);
    // The clock starts at the first sample that is in sync.
    let done = run(&svc, &clock, 900, 1260);
    assert_eq!(done, vec![(1230, returned("orders"))], "{done:?}");
}

/// The clock belongs to a leader epoch: a node that lost the partition and
/// won it back starts again, whatever the in-sync set did in between.
#[test]
fn a_new_leader_epoch_restarts_the_clock() {
    let (_dir, svc, coordinator, clock) = start(failed_over_rows());
    assert!(run(&svc, &clock, 0, 200).is_empty());
    for r in coordinator.rows.lock().iter_mut() {
        r.leader_epoch += 1;
    }
    assert!(run(&svc, &clock, 200, 495).is_empty());
    let done = run(&svc, &clock, 495, 540);
    assert_eq!(done, vec![(510, returned("orders"))], "{done:?}");
}

/// ... and to the generation of the topic: one that was deleted and created
/// again is a new topic.
#[test]
fn a_new_topic_generation_restarts_the_clock() {
    let (_dir, svc, coordinator, clock) = start(failed_over_rows());
    assert!(run(&svc, &clock, 0, 200).is_empty());
    for r in coordinator.rows.lock().iter_mut() {
        r.topic_generation += 1;
    }
    assert!(run(&svc, &clock, 200, 495).is_empty());
    let done = run(&svc, &clock, 495, 540);
    assert_eq!(done, vec![(510, returned("orders"))], "{done:?}");
}

#[test]
fn at_most_one_topic_goes_back_per_thirty_seconds() {
    let mut rows = failed_over_rows();
    rows.push(row("invoices", 0, "n2", &["n1", "n2", "n3"]));
    rows.push(row("alpha", 0, "n2", &["n1", "n2", "n3"]));
    let (_dir, svc, coordinator, clock) = start(rows);

    let done = run(&svc, &clock, 0, 400);
    assert_eq!(
        done,
        vec![
            (300, returned("alpha")),
            (330, returned("invoices")),
            (360, returned("orders")),
        ],
        "one per look, thirty seconds apart"
    );
    assert_eq!(coordinator.led_by("n1"), 5);
}

#[test]
fn the_switch_off_returns_nothing_and_turning_it_on_starts_the_clocks_over() {
    let (_dir, svc, coordinator, clock) = start(failed_over_rows());
    let key = leader_return_setting_key(&svc.instance_id);
    crate::db::repository::set_setting(&svc.db, &key, "off").expect("set");

    assert_eq!(tick(&svc, &clock, 400), LeaderReturnTick::Disabled);
    assert!(coordinator.transfers.lock().is_empty());

    crate::db::repository::set_setting(&svc.db, &key, "on").expect("set");
    assert!(
        run(&svc, &clock, 400, 690).is_empty(),
        "the time spent off does not count"
    );
    let done = run(&svc, &clock, 690, 720);
    assert_eq!(done.len(), 1, "{done:?}");
}

#[test]
fn a_switch_value_that_does_not_read_is_off() {
    let (_dir, svc, coordinator, clock) = start(failed_over_rows());
    let key = leader_return_setting_key(&svc.instance_id);
    crate::db::repository::set_setting(&svc.db, &key, "maybe").expect("set");
    assert!(!svc.leader_return_enabled());
    assert!(run(&svc, &clock, 0, 900).is_empty());
    assert!(coordinator.transfers.lock().is_empty());
}

#[test]
fn the_switch_is_on_by_default() {
    let (_dir, svc, _coordinator, _clock) = start(failed_over_rows());
    assert!(svc.leader_return_enabled());
}

/// The switch replicates, so what is stored is checked: a hosted instance, a
/// value that reads, and no secret.
#[test]
fn a_switch_setting_is_refused_for_an_unknown_instance_a_bad_value_or_a_secret() {
    let hosted = |instance: &str| instance == "tentabus-00000001";
    let check =
        |key: &str, value: &str, secret: bool| validate_switch_setting(key, value, secret, &hosted);
    assert!(check("bus.leader_return:tentabus-00000001", "off", false).is_ok());
    assert!(check("bus.leader_return:tentabus-00000001", "TRUE", false).is_ok());
    assert!(check("bus.leader_return:tentabus-00000002", "off", false).is_err());
    assert!(check("bus.leader_return:", "off", false).is_err());
    assert!(check("bus.leader_return:tentabus-00000001", "maybe", false).is_err());
    assert!(check("bus.leader_return:tentabus-00000001", "off", true).is_err());
    assert!(
        check("jwt_expiry_hours", "maybe", false).is_ok(),
        "other keys are not this check's business"
    );
}

#[test]
fn an_unreadable_switch_is_complained_about_at_most_once_in_a_while() {
    let (_dir, svc, _coordinator, _clock) = start(failed_over_rows());
    assert!(svc.switch_warning_due());
    assert!(!svc.switch_warning_due());
}

fn pause_group_of(svc: &BusService, topic: &str) {
    crate::db::repository::bus_group_upsert(
        &svc.local_db,
        &crate::db::repository::DbBusGroup {
            org_id: ORG.to_string(),
            group_id: "workers".to_string(),
            topic: topic.to_string(),
            commit_mode: "auto".to_string(),
            paused: true,
            created_at_ms: 0,
            updated_at_ms: 0,
        },
    )
    .expect("pause a group");
}

#[test]
fn a_paused_group_of_the_topic_or_of_its_dlq_keeps_it_where_it_is() {
    for paused in ["orders", "__dlq.orders"] {
        let mut rows = failed_over_rows();
        rows.push(row("other", 0, "n2", &["n1", "n2", "n3"]));
        let (_dir, svc, coordinator, clock) = start(rows);
        pause_group_of(&svc, paused);

        let done = run(&svc, &clock, 0, 900);
        assert_eq!(
            done,
            vec![(300, returned("other"))],
            "only the topic that is not paused goes back ({paused})"
        );
        assert!(coordinator.leaders_of("orders").iter().all(|l| l == "n2"));
        assert!(coordinator
            .leaders_of("__dlq.orders")
            .iter()
            .all(|l| l == "n2"));
    }
}

#[test]
fn a_preferred_node_that_does_not_take_forwarded_requests_gets_nothing_back() {
    let (_dir, svc, coordinator, clock) = start(failed_over_rows());
    coordinator
        .forwarding
        .lock()
        .insert("n1".to_string(), NodeForwarding::Lacking);
    assert!(run(&svc, &clock, 0, 900).is_empty());
    assert!(coordinator.transfers.lock().is_empty());

    // The node is upgraded: the topic goes back at the next look.
    coordinator.forwarding.lock().remove("n1");
    let done = run(&svc, &clock, 900, 930);
    assert_eq!(done, vec![(930, returned("orders"))]);
}

#[test]
fn a_look_while_a_rebalance_runs_is_skipped_and_made_again_at_the_next_sample() {
    let (_dir, svc, coordinator, clock) = start(failed_over_rows());
    assert!(run(&svc, &clock, 0, 295).is_empty());
    {
        let _running = svc.leader_balance_run.lock();
        assert_eq!(tick(&svc, &clock, 300), LeaderReturnTick::Busy);
    }
    assert!(coordinator.transfers.lock().is_empty());
    assert_eq!(
        tick(&svc, &clock, 305),
        returned("orders"),
        "the busy look did not use up the interval"
    );
}

#[test]
fn a_node_that_is_shutting_down_starts_no_move() {
    let (_dir, svc, coordinator, clock) = start(failed_over_rows());
    assert!(run(&svc, &clock, 0, 295).is_empty());
    assert_eq!(
        svc.leader_return_tick_at(clock.at(300), &|| true),
        LeaderReturnTick::Cancelled
    );
    assert!(coordinator.transfers.lock().is_empty());
}

#[test]
fn a_failed_return_changes_nothing_and_the_topic_waits_another_five_minutes() {
    let (_dir, svc, coordinator, clock) = start(failed_over_rows());
    coordinator.failing.lock().insert(("orders".to_string(), 1));

    let done = run(&svc, &clock, 0, 300);
    assert!(
        matches!(&done[..], [(300, LeaderReturnTick::Failed { topic, partial: false, .. })] if topic == "orders"),
        "{done:?}"
    );
    for topic in ["orders", "__dlq.orders"] {
        assert!(coordinator.leaders_of(topic).iter().all(|l| l == "n2"));
    }

    assert!(
        run(&svc, &clock, 300, 595).is_empty(),
        "not tried again at once"
    );
    coordinator.failing.lock().clear();
    let done = run(&svc, &clock, 595, 630);
    assert_eq!(done, vec![(630, returned("orders"))], "{done:?}");
}

#[test]
fn the_internal_topics_and_a_topic_this_node_prefers_itself_are_left_alone() {
    let rows = vec![
        row("mine", 0, "n2", &["n2", "n1"]),
        row("__bus.metrics", 0, "n2", &["n1", "n2"]),
    ];
    let (_dir, svc, coordinator, clock) = start(rows);
    assert!(run(&svc, &clock, 0, 900).is_empty());
    assert!(coordinator.transfers.lock().is_empty());
}

#[test]
fn an_instance_of_one_node_has_nothing_to_return_to() {
    let rows = vec![row("orders", 0, "n2", &["n2"])];
    let (_dir, svc, _coordinator, clock) = start_on("n2", rows);
    assert_eq!(tick(&svc, &clock, 0), LeaderReturnTick::SingleNode);
}

#[test]
fn rows_that_disagree_on_the_preferred_node_leave_the_topic_where_it_is() {
    let rows = vec![
        row("orders", 0, "n2", &["n1", "n2", "n3"]),
        row("orders", 1, "n2", &["n3", "n2", "n1"]),
    ];
    let (_dir, svc, coordinator, clock) = start(rows);
    assert!(run(&svc, &clock, 0, 900).is_empty());
    assert!(coordinator.transfers.lock().is_empty());
}

/// For a split topic the disagreement matters: with no node every row names
/// first, there is no preferred node to gather on, so the strays go to the
/// node that leads most of the topic.
#[test]
fn a_split_topic_whose_rows_disagree_on_the_preferred_node_is_gathered_on_the_majority() {
    let rows = vec![
        row("orders", 0, "n2", &["n1", "n2", "n3"]),
        row("orders", 1, "n3", &["n3", "n2", "n1"]),
        row("__dlq.orders", 0, "n2", &["n1", "n2", "n3"]),
    ];
    let (_dir, svc, coordinator, clock) = start_on("n2", rows);
    assert_eq!(tick(&svc, &clock, 0), gathered("orders", "n2"));
    for topic in ["orders", "__dlq.orders"] {
        assert!(coordinator.leaders_of(topic).iter().all(|l| l == "n2"));
    }
}

#[test]
fn the_timings_can_be_shortened_and_default_to_five_minutes_and_thirty_seconds() {
    let (_dir, svc, _coordinator, clock) = start(failed_over_rows());
    let default = svc.leader_return_timing();
    assert_eq!(default.stable_for, Duration::from_secs(300));
    assert_eq!(default.decision_interval, Duration::from_secs(30));

    svc.set_leader_return_timing(LeaderReturnTiming {
        stable_for: Duration::from_secs(20),
        decision_interval: Duration::from_secs(10),
        sample_interval: Duration::from_secs(5),
        idle_interval: Duration::from_secs(60),
    });
    let done = run(&svc, &clock, 0, 60);
    assert_eq!(done, vec![(20, returned("orders"))], "{done:?}");
}

// ---- A topic split between nodes -------------------------------------------

/// The lowest id among the nodes that lead the topic drives the move, with no
/// five minutes: a topic split between nodes is read by neither. The leaders'
/// own checks say the preferred node can take it, so it goes there, and the
/// order of the replicas is what it was.
#[test]
fn a_split_topic_is_gathered_on_the_preferred_node_at_the_first_look() {
    let (_dir, svc, coordinator, clock) = start_on("n2", split_rows());
    assert_eq!(tick(&svc, &clock, 0), gathered("orders", "n1"));
    for topic in ["orders", "__dlq.orders"] {
        assert!(coordinator.leaders_of(topic).iter().all(|l| l == "n1"));
    }
    for r in coordinator.rows.lock().iter() {
        assert_eq!(r.replicas, vec!["n1", "n2", "n3"]);
    }
    let transfers = audit_details(&svc, "bus.leader.transfer");
    assert_eq!(transfers.len(), 3, "{transfers:?}");
    assert!(transfers
        .iter()
        .all(|d| d.contains(&format!("reason={PREFERRED_RESTORE_REASON}"))));
}

/// Only the driver moves it: a node with a higher id than another leader of
/// the topic does nothing, so two nodes never gather one topic at once.
#[test]
fn only_the_lowest_leader_of_a_split_topic_drives_it() {
    let (_dir, svc, coordinator, clock) = start_on("n3", split_rows());
    // The in-sync entries are for the partitions this node leads.
    assert_eq!(tick(&svc, &clock, 0), LeaderReturnTick::Idle);
    assert!(run(&svc, &clock, 0, 900).is_empty());
    assert!(coordinator.transfers.lock().is_empty());
}

/// The preferred node cannot take the strays (a leader says it is behind): the
/// topic is gathered onto the node that leads most of it, never left split,
/// and the replica order is not rewritten, so the later return still goes to
/// the true preferred node.
#[test]
fn a_split_topic_whose_preferred_node_is_behind_is_gathered_on_the_majority_and_returned_later() {
    let (_dir, svc, coordinator, clock) = start_on("n2", split_rows());
    coordinator.behind_nodes.lock().insert("n1".to_string());

    assert_eq!(tick(&svc, &clock, 0), gathered("orders", "n2"));
    for topic in ["orders", "__dlq.orders"] {
        assert!(coordinator.leaders_of(topic).iter().all(|l| l == "n2"));
    }
    for r in coordinator.rows.lock().iter() {
        assert_eq!(
            r.replicas,
            vec!["n1", "n2", "n3"],
            "a gather does not choose a new preferred node: {}",
            r.topic
        );
    }
    let transfers = audit_details(&svc, "bus.leader.transfer");
    assert_eq!(transfers.len(), 1, "only the stray moved: {transfers:?}");
    assert!(transfers[0].contains(&format!("reason={SPLIT_GATHER_REASON}")));

    // The preferred node catches up and stays in sync: the return follows.
    coordinator.behind_nodes.lock().clear();
    let done = run(&svc, &clock, 0, 360);
    assert_eq!(done, vec![(330, returned("orders"))], "{done:?}");
    assert!(coordinator.leaders_of("orders").iter().all(|l| l == "n1"));
}

#[test]
fn a_split_topic_whose_gather_failed_waits_two_looks_before_it_is_tried_again() {
    let (_dir, svc, coordinator, clock) = start_on("n2", split_rows());
    coordinator.behind_nodes.lock().insert("n1".to_string());
    coordinator.failing.lock().insert(("orders".to_string(), 1));

    let first = tick(&svc, &clock, 0);
    assert!(
        matches!(&first, LeaderReturnTick::Failed { partial: false, .. }),
        "{first:?}"
    );
    assert!(run(&svc, &clock, 0, 55).is_empty(), "backing off");
    coordinator.failing.lock().clear();
    let done = run(&svc, &clock, 55, 90);
    assert_eq!(done, vec![(60, gathered("orders", "n2"))], "{done:?}");
}

// ---- Moves that overlap -----------------------------------------------------

/// Somebody else (another node's manual move) hands the whole topic to `n3`
/// while the automatic return is half way. The return finds its second
/// partition has moved (the leader refuses a transfer from a row that is no
/// longer the one read), and does not take the first one back from `n3`: the
/// topic ends on one node, and the result says that part of it was not handed
/// back.
#[test]
fn a_manual_move_that_overtakes_the_return_leaves_the_topic_whole_and_the_result_honest() {
    let (_dir, svc, coordinator, clock) = start(failed_over_rows());
    assert!(run(&svc, &clock, 0, 295).is_empty());
    let calls = std::sync::atomic::AtomicU32::new(0);
    *coordinator.before_transfer.lock() = Some(Box::new(move |c, _, _| {
        // Before the return's second transfer, the other mover takes every
        // partition of the unit to n3 (each at a new epoch).
        if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
            for r in c.rows.lock().iter_mut() {
                r.leader_node_id = "n3".to_string();
                r.leader_epoch += 10;
            }
        }
    }));

    let answer = tick(&svc, &clock, 300);
    assert!(
        matches!(&answer, LeaderReturnTick::Failed { partial: true, code, .. } if code == "bus.not_leader"),
        "{answer:?}"
    );
    for topic in ["orders", "__dlq.orders"] {
        assert!(
            coordinator.leaders_of(topic).iter().all(|l| l == "n3"),
            "{topic} is whole, on the node the other mover chose"
        );
    }
    assert_eq!(
        coordinator.transfers.lock().len(),
        1,
        "the first partition's move only: nothing was taken back from n3"
    );
    let summary = audit_details(&svc, "bus.leader.rebalance");
    assert!(
        summary
            .iter()
            .any(|d| d.contains("result=failed_partial") && d.contains("reason=preferred_restore")),
        "{summary:?}"
    );
}

/// A partition the other mover did not touch is handed back as usual.
#[test]
fn a_return_that_fails_after_its_first_partition_hands_it_back_when_it_is_still_the_returns() {
    let (_dir, svc, coordinator, clock) = start(failed_over_rows());
    assert!(run(&svc, &clock, 0, 295).is_empty());
    let calls = std::sync::atomic::AtomicU32::new(0);
    *coordinator.before_transfer.lock() = Some(Box::new(move |c, topic, partition| {
        // Before the second transfer someone moves that partition on.
        if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
            let mut rows = c.rows.lock();
            let r = rows
                .iter_mut()
                .find(|r| r.topic == topic && r.partition == partition)
                .expect("row");
            r.leader_node_id = "n3".to_string();
            r.leader_epoch += 10;
        }
    }));
    let answer = tick(&svc, &clock, 300);
    assert!(
        matches!(&answer, LeaderReturnTick::Failed { partial: false, .. }),
        "{answer:?}"
    );
    // The first partition of the unit, moved by the return, is back; the one
    // the other mover took stays where that mover put it.
    let led: Vec<String> = coordinator
        .rows
        .lock()
        .iter()
        .map(|r| format!("{}/{}={}", r.topic, r.partition, r.leader_node_id))
        .collect();
    assert_eq!(
        led.iter().filter(|l| l.ends_with("=n1")).count(),
        0,
        "nothing is left on the node the return aimed at: {led:?}"
    );
}

/// A move that left the topic partly moved is gathered at the next look, not
/// after five minutes: the other mover takes the first partition on while the
/// second one fails, so the rollback has nothing to hand back and the topic
/// is split between `n2` and `n3`.
#[test]
fn a_partial_move_is_gathered_at_the_next_look() {
    let (_dir, svc, coordinator, clock) = start(failed_over_rows());
    assert!(run(&svc, &clock, 0, 295).is_empty());
    coordinator.failing.lock().insert(("orders".to_string(), 1));
    let calls = std::sync::atomic::AtomicU32::new(0);
    *coordinator.before_transfer.lock() = Some(Box::new(move |c, topic, partition| {
        // Before the second transfer the other mover takes the partition the
        // return moved first on to n3.
        if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 1 {
            let _ = (topic, partition);
            let mut rows = c.rows.lock();
            for r in rows.iter_mut().filter(|r| r.leader_node_id == "n1") {
                r.leader_node_id = "n3".to_string();
                r.leader_epoch += 10;
            }
        }
    }));
    let answer = tick(&svc, &clock, 300);
    assert!(
        matches!(&answer, LeaderReturnTick::Failed { partial: true, .. }),
        "{answer:?}"
    );
    let split: std::collections::BTreeSet<String> = ["orders", "__dlq.orders"]
        .iter()
        .flat_map(|t| coordinator.leaders_of(t))
        .collect();
    assert!(split.len() > 1, "the topic is split now: {split:?}");

    *coordinator.before_transfer.lock() = None;
    coordinator.failing.lock().clear();
    let done = run(&svc, &clock, 300, 330);
    assert!(
        matches!(&done[..], [(330, LeaderReturnTick::Gathered { .. })]),
        "gathered at the next look: {done:?}"
    );
    for topic in ["orders", "__dlq.orders"] {
        let leaders: std::collections::BTreeSet<String> =
            coordinator.leaders_of(topic).into_iter().collect();
        assert_eq!(leaders.len(), 1, "{topic} is whole again: {leaders:?}");
    }
}

/// The admin's manual move takes the same lock as the plans and the return
/// and is refused as busy while another run holds it.
#[test]
fn a_manual_move_while_a_run_is_in_progress_is_refused_as_busy() {
    let (_dir, svc, coordinator, _clock) = start(failed_over_rows());
    let _running = svc.leader_balance_run.lock();
    let failure = svc
        .move_topic_leadership(
            crate::bus::leader_balance::RunContext {
                actor: Some("admin"),
                reason: "manual",
            },
            ORG,
            "orders",
            "n3",
        )
        .expect_err("another run holds the executor");
    assert!(failure.busy);
    assert_eq!(failure.status, MoveStatus::Failed);
    assert!(coordinator.transfers.lock().is_empty());
}

/// The plan counts a return that waits for its node as already made only
/// while the node that makes the plan returns topics by itself.
#[test]
fn the_plan_counts_a_waiting_return_where_the_topic_is_when_the_switch_is_off() {
    let rows = vec![
        row("t1", 0, "n2", &["n1", "n2"]),
        row("t2", 0, "n2", &["n2", "n1"]),
        row("t3", 0, "n1", &["n1", "n2"]),
    ];
    let (_dir, svc, coordinator) = service_with("n2", rows);
    coordinator.behind.lock().insert(("t1".to_string(), 0));
    let after = |plan: &crate::bus::leader_balance::LeaderPlan, node: &str| {
        plan.per_node
            .iter()
            .find(|n| n.node_id == node)
            .map_or(0, |n| n.after)
    };

    let plan = svc
        .leader_balance_plan(Some(ORG), None, &|_, _| true)
        .expect("plan");
    assert_eq!((after(&plan, "n1"), after(&plan, "n2")), (2, 1), "{plan:?}");

    let key = leader_return_setting_key(&svc.instance_id);
    crate::db::repository::set_setting(&svc.db, &key, "off").expect("set");
    let plan = svc
        .leader_balance_plan(Some(ORG), None, &|_, _| true)
        .expect("plan");
    assert_eq!((after(&plan, "n1"), after(&plan, "n2")), (1, 2), "{plan:?}");
}
