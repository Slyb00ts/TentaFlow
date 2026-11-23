//! TB-04 K7: the read-only views (`peek`, `peek_runs`, `group_lag`, the lag
//! sampler) are served by a replica, bounded by its own high watermark, while
//! everything that changes state still answers `NotLeader` naming the leader.
//! The in-process cluster tests are in `tests/bus_replication_three_node.rs`.

use super::*;

const TOPIC: &str = "orders.replica-read";

/// A one-partition topic whose five records were written as ONE batch and
/// whose high watermark stops at 3, as a replica's does while the leader has
/// committed only that far: the watermark lies inside a fetched batch.
fn topic_with_hw_inside_a_batch(svc: &BusService, ctx: &BusCallContext) {
    topic_with_watermarks(svc, ctx, 3, 3);
}

/// The same five records with the high watermark at `hw` and the committed
/// offset (what a replica is never cut below) at `committed`: `acks=leader`
/// lets the first run ahead of the second.
fn topic_with_watermarks(svc: &BusService, ctx: &BusCallContext, hw: u64, committed: u64) {
    one_partition_topic(svc, ctx, TOPIC);
    let part = svc.local_partition(ctx, TOPIC, 0).unwrap();
    part.set_hw_tracking(tentaflow_bus::HwTracking::Manual);
    let records = (0..5).map(|i| record(&format!("r{i}"))).collect();
    // The broker stamps the correlation id itself; a caller's `tf.*` header
    // is stripped.
    let stamped = BusCallContext {
        correlation_id: Some("chain-1".to_string()),
        ..ctx.clone()
    };
    svc.publish(
        &stamped,
        TOPIC,
        PublishBatch {
            partition: Some(0),
            producer: None,
            records,
        },
    )
    .unwrap();
    assert_eq!(part.log_end_offset(), 5);
    assert_eq!(part.set_high_watermark(hw), hw);
    assert_eq!(part.set_committed_offset(committed), committed);
}

fn offsets(result: &PeekResult) -> Vec<u64> {
    result.records.iter().map(|r| r.offset).collect()
}

#[test]
fn a_replica_peek_returns_what_the_leader_returns_and_stops_at_its_high_watermark() {
    let (_tmp, svc) = test_service();
    let ctx = test_ctx("org-1");
    topic_with_hw_inside_a_batch(&svc, &ctx);
    let coord = FakeCoordinator::leader(1);
    svc.set_replication(coord.clone());

    let on_leader = svc.peek(&ctx, TOPIC, 0, 0, 100, PEEK_MAX_BYTES).unwrap();

    coord.set_role(follower_of("node-a", 1));
    let on_replica = svc.peek(&ctx, TOPIC, 0, 0, 100, PEEK_MAX_BYTES).unwrap();
    assert_eq!(offsets(&on_replica), vec![0, 1, 2], "nothing past hw 3");
    assert_eq!(offsets(&on_replica), offsets(&on_leader));
    assert_eq!(on_replica.high_watermark, 3);
    let payloads =
        |r: &PeekResult| -> Vec<Bytes> { r.records.iter().map(|r| r.payload.clone()).collect() };
    assert_eq!(payloads(&on_replica), payloads(&on_leader));

    let at_hw = svc.peek(&ctx, TOPIC, 0, 3, 100, PEEK_MAX_BYTES).unwrap();
    assert!(
        at_hw.records.is_empty(),
        "a read at hw is empty, not an error"
    );

    let runs = svc.peek_runs(&ctx, TOPIC, 0, &[0..5], 16).unwrap();
    assert_eq!(offsets(&runs), vec![0, 1, 2]);
}

#[test]
fn a_peek_says_whether_a_replica_or_the_leader_answered() {
    let (_tmp, svc) = test_service();
    let ctx = test_ctx("org-1");
    topic_with_hw_inside_a_batch(&svc, &ctx);
    let coord = FakeCoordinator::leader(1);
    svc.set_replication(coord.clone());
    let from_replica = || {
        svc.peek(&ctx, TOPIC, 0, 0, 100, PEEK_MAX_BYTES)
            .unwrap()
            .from_replica
    };

    assert!(!from_replica());
    coord.set_role(follower_of("node-a", 1));
    assert!(from_replica());
}

#[test]
fn a_peek_of_ranges_says_whether_a_replica_or_the_leader_answered() {
    let (_tmp, svc) = test_service();
    let ctx = test_ctx("org-1");
    topic_with_hw_inside_a_batch(&svc, &ctx);
    let coord = FakeCoordinator::leader(1);
    svc.set_replication(coord.clone());
    let from_replica = || {
        svc.peek_runs(&ctx, TOPIC, 0, &[0..3], 16)
            .unwrap()
            .from_replica
    };

    assert!(!from_replica());
    coord.set_role(follower_of("node-a", 1));
    assert!(from_replica());
}

#[test]
fn a_replica_filtered_scan_stops_at_its_high_watermark() {
    let (_tmp, svc) = test_service();
    let ctx = test_ctx("org-1");
    topic_with_hw_inside_a_batch(&svc, &ctx);
    let coord = FakeCoordinator::leader(1);
    coord.set_role(follower_of("node-a", 1));
    svc.set_replication(coord);

    let filtered = svc
        .peek_filtered(&ctx, TOPIC, 0, 0, 100, PEEK_MAX_BYTES, Some("chain-1"))
        .unwrap();
    assert_eq!(offsets(&filtered), vec![0, 1, 2]);
    assert_eq!(filtered.next_offset, Some(3), "the scan resumes at hw");
}

#[test]
fn a_node_that_holds_no_replica_still_refuses_a_read() {
    let (_tmp, svc) = test_service();
    let ctx = test_ctx("org-1");
    topic_with_hw_inside_a_batch(&svc, &ctx);
    let coord = FakeCoordinator::leader(1);
    coord.set_role(PartitionRole::Unavailable {
        reason: UnavailableReason::NoAssignment,
    });
    svc.set_replication(coord);

    assert!(matches!(
        svc.peek(&ctx, TOPIC, 0, 0, 10, PEEK_MAX_BYTES),
        Err(BusServiceError::NotLeader {
            leader_node_id: None,
            ..
        })
    ));
    assert!(matches!(
        svc.peek_runs(&ctx, TOPIC, 0, &[0..2], 16),
        Err(BusServiceError::NotLeader { .. })
    ));
    assert!(matches!(
        svc.group_lag(&ctx, "g1", TOPIC),
        Err(BusServiceError::NotLeader { .. })
    ));
}

#[test]
fn group_lag_on_a_replica_is_its_high_watermark_minus_the_offset_that_reached_it() {
    let (_tmp, svc) = test_service();
    let ctx = test_ctx("org-1");
    topic_with_hw_inside_a_batch(&svc, &ctx);
    let coord = FakeCoordinator::leader(1);
    coord.set_role(follower_of("node-a", 1));
    svc.set_replication(coord.clone());

    // What `follower::apply_offsets` does with a `ReplOffsets` commit.
    svc.offsets
        .commit("org-1", "g1", TOPIC, 0, 1, now_ms())
        .unwrap();
    let lag = svc.group_lag(&ctx, "g1", TOPIC).unwrap();
    assert_eq!(
        (lag[0].committed_offset, lag[0].high_watermark, lag[0].lag),
        (1, 3, 2),
        "lag counts to hw (3), not to the log end (5)"
    );

    coord.set_role(PartitionRole::Leader { epoch: 2 });
    assert_eq!(svc.group_lag(&ctx, "g1", TOPIC).unwrap(), lag);
}

#[test]
fn the_lag_sampler_samples_a_group_whose_topic_this_node_replicates() {
    let (_tmp, svc) = test_service();
    let ctx = test_ctx("org-1");
    one_partition_topic(&svc, &ctx, TOPIC);
    insert_group_row(&svc, "org-1", "g1", TOPIC);
    publish_to(&svc, &ctx, TOPIC, vec![record("a"), record("b")]);
    svc.offsets
        .commit("org-1", "g1", TOPIC, 0, 1, now_ms())
        .unwrap();
    let coord = FakeCoordinator::leader(1);
    coord.set_role(follower_of("node-a", 1));
    svc.set_replication(coord.clone());

    svc.sample_lag_history(1_800_000_000_000);
    let series = lag_history::group_series(&svc.local_db, "org-1", 0, None, None).unwrap();
    assert_eq!(series.len(), 1);
    assert_eq!(series[0].points[0].lag_total, 1);

    // A node with no replica has nothing to sample.
    coord.set_role(PartitionRole::Unavailable {
        reason: UnavailableReason::NoAssignment,
    });
    svc.sample_lag_history(1_800_000_060_000);
    let series = lag_history::group_series(&svc.local_db, "org-1", 0, None, None).unwrap();
    assert_eq!(series[0].points.len(), 1, "no second sample");
}

/// Every state change reachable from a read-only screen is refused on a
/// replica with `NotLeader` naming the leader and its epoch, and changes
/// nothing.
#[test]
fn every_mutation_on_a_replica_is_refused_naming_the_leader() {
    let (_tmp, svc) = test_service();
    let ctx = test_ctx("org-1");
    one_partition_topic(&svc, &ctx, TOPIC);
    svc.ensure_dlq_topic(&ctx, TOPIC, &svc.topic_config("org-1", TOPIC).unwrap())
        .unwrap();
    insert_group_row(&svc, "org-1", "g1", TOPIC);
    publish_to(&svc, &ctx, TOPIC, vec![record("a"), record("b")]);
    let dlq_topic = dlq::dlq_topic_name(TOPIC);
    let coord = FakeCoordinator::leader(1);
    coord.set_role(follower_of("node-b", 7));
    svc.set_replication(coord.clone());

    let refused: Vec<(&str, Result<(), BusServiceError>)> = vec![
        ("pause", svc.pause_group(&ctx, "g1", TOPIC)),
        ("resume", svc.resume_group(&ctx, "g1", TOPIC)),
        ("reset", svc.reset_offset(&ctx, "g1", TOPIC, 0, 1)),
        (
            "reset of a group this node never served",
            svc.reset_offset(&ctx, "never-opened-here", TOPIC, 0, 1),
        ),
        ("discard", svc.dlq_discard(&ctx, &dlq_topic, 0, 0)),
        ("retry", svc.dlq_retry(&ctx, &dlq_topic, 0, 0).map(|_| ())),
        (
            "retry all (leader check before the scan)",
            svc.require_partition_leader(&ctx, &dlq_topic, 0),
        ),
    ];
    for (what, outcome) in refused {
        match outcome {
            Err(BusServiceError::NotLeader {
                leader_node_id,
                leader_epoch,
            }) => assert_eq!(
                (leader_node_id.as_deref(), leader_epoch),
                (Some("node-b"), 7),
                "{what}"
            ),
            other => panic!("{what}: expected NotLeader naming node-b, got {other:?}"),
        }
    }
    assert!(!svc.is_group_paused("org-1", "g1", TOPIC).unwrap());
    assert_eq!(
        svc.offsets
            .committed_offset("org-1", "g1", TOPIC, 0)
            .unwrap(),
        0
    );
    assert!(coord.note_offset_commit_calls.lock().is_empty());
    assert!(coord.note_dlq_handled_calls.lock().is_empty());
}

/// Under `acks=leader` the leader's high watermark runs ahead of any
/// majority, and a replica can be cut back to its committed offset, so it
/// shows nothing past that.
#[test]
fn a_replica_read_stops_at_the_committed_offset_when_the_high_watermark_runs_ahead() {
    let (_tmp, svc) = test_service();
    let ctx = test_ctx("org-1");
    topic_with_watermarks(&svc, &ctx, 3, 2);
    let coord = FakeCoordinator::leader(1);
    svc.set_replication(coord.clone());

    let on_leader = svc.peek(&ctx, TOPIC, 0, 0, 100, PEEK_MAX_BYTES).unwrap();
    assert_eq!(offsets(&on_leader), vec![0, 1, 2]);

    coord.set_role(follower_of("node-a", 1));
    let on_replica = svc.peek(&ctx, TOPIC, 0, 0, 100, PEEK_MAX_BYTES).unwrap();
    assert_eq!(offsets(&on_replica), vec![0, 1]);
    assert_eq!(
        on_replica.high_watermark, 2,
        "the page reports its own bound"
    );
    assert_eq!(
        offsets(&svc.peek_runs(&ctx, TOPIC, 0, &[0..5], 16).unwrap()),
        vec![0, 1]
    );
    let filtered = svc
        .peek_filtered(&ctx, TOPIC, 0, 0, 100, PEEK_MAX_BYTES, Some("chain-1"))
        .unwrap();
    assert_eq!(offsets(&filtered), vec![0, 1]);
    assert_eq!(filtered.next_offset, Some(2));
    assert_eq!(svc.read_window(&ctx, TOPIC, 0).unwrap().high_watermark, 2);

    svc.offsets
        .commit("org-1", "g1", TOPIC, 0, 1, now_ms())
        .unwrap();
    let lag = svc.group_lag(&ctx, "g1", TOPIC).unwrap();
    assert_eq!((lag[0].high_watermark, lag[0].lag), (2, 1));
    coord.set_role(PartitionRole::Leader { epoch: 2 });
    let lag = svc.group_lag(&ctx, "g1", TOPIC).unwrap();
    assert_eq!((lag[0].high_watermark, lag[0].lag), (3, 2));
}

/// A stale ex-leader that stepped down is a follower with no leader to name;
/// what it holds may be a branch no leader has, so it serves no read.
#[test]
fn a_follower_without_a_known_leader_is_not_readable() {
    let (_tmp, svc) = test_service();
    let ctx = test_ctx("org-1");
    topic_with_hw_inside_a_batch(&svc, &ctx);
    let coord = FakeCoordinator::leader(1);
    coord.set_role(follower_of("", 4));
    svc.set_replication(coord);

    let expect_refused = |what: &str, outcome: Result<(), BusServiceError>| match outcome {
        Err(BusServiceError::NotLeader {
            leader_node_id: None,
            leader_epoch: 4,
        }) => {}
        other => panic!("{what}: expected NotLeader naming nobody, got {other:?}"),
    };
    expect_refused(
        "peek",
        svc.peek(&ctx, TOPIC, 0, 0, 10, PEEK_MAX_BYTES).map(|_| ()),
    );
    expect_refused(
        "peek_runs",
        svc.peek_runs(&ctx, TOPIC, 0, &[0..2], 16).map(|_| ()),
    );
    expect_refused("group_lag", svc.group_lag(&ctx, "g1", TOPIC).map(|_| ()));
    expect_refused("read_window", svc.read_window(&ctx, TOPIC, 0).map(|_| ()));
    assert!(!svc.reads_from_replica("org-1", TOPIC));
}

/// A read that a truncation cut across is read again once; a second cut
/// refuses it instead of returning a mixed page.
#[test]
fn a_replica_read_cut_by_a_truncation_is_retried_once_then_refused() {
    let (_tmp, svc) = test_service();
    let ctx = test_ctx("org-1");
    one_partition_topic(&svc, &ctx, TOPIC);
    let part = svc.local_partition(&ctx, TOPIC, 0).unwrap();
    // As on a replica: nothing is committed, so the log can be cut.
    part.set_hw_tracking(tentaflow_bus::HwTracking::Manual);
    for i in 0..6 {
        publish_to(&svc, &ctx, TOPIC, vec![record(&format!("r{i}"))]);
    }

    let mut calls = 0;
    let out = read_stable(&part, ReadServedBy::Replica, || {
        calls += 1;
        if calls == 1 {
            part.truncate_to_offset(5).unwrap();
        }
        Ok(calls)
    });
    assert_eq!(
        out.unwrap(),
        2,
        "the page that straddled the cut is read again"
    );

    let mut calls = 0;
    let out = read_stable(&part, ReadServedBy::Replica, || {
        calls += 1;
        let end = part.log_end_offset();
        part.truncate_to_offset(end - 1).unwrap();
        Ok(calls)
    });
    assert!(matches!(
        out,
        Err(BusServiceError::PartitionUnavailable { .. })
    ));
    assert_eq!(calls, 2, "retried once, not forever");

    let mut calls = 0;
    let leader_out = read_stable(&part, ReadServedBy::Leader, || {
        calls += 1;
        let end = part.log_end_offset();
        part.truncate_to_offset(end - 1).unwrap();
        Ok(calls)
    });
    assert_eq!(leader_out.unwrap(), 1, "a leader's read is not retried");
}

/// A leader on a replicated topic serves a consumer no record its replicas
/// may still lose: a fetch returns whole batches, so the cut is per record.
#[test]
fn a_consumer_fetch_stops_at_the_high_watermark_inside_a_batch() {
    let (_tmp, svc) = test_service();
    let ctx = test_ctx("org-1");
    topic_with_hw_inside_a_batch(&svc, &ctx);
    svc.set_replication(FakeCoordinator::leader(1));

    let handle = svc
        .open_consumer(
            &ctx,
            "g1",
            &[TOPIC.to_string()],
            ConsumerConfig {
                commit_mode: groups::CommitMode::Explicit,
            },
        )
        .unwrap();
    let batch = handle.fetch(1024 * 1024, 20).unwrap();
    let fetched: Vec<u64> = batch.records.iter().map(|r| r.offset).collect();
    assert_eq!(fetched, vec![0, 1, 2], "nothing past hw 3");
    assert!(handle.fetch(1024 * 1024, 20).unwrap().records.is_empty());

    let part = svc.local_partition(&ctx, TOPIC, 0).unwrap();
    part.set_high_watermark(5);
    let later = handle.fetch(1024 * 1024, 20).unwrap();
    let later: Vec<u64> = later.records.iter().map(|r| r.offset).collect();
    assert_eq!(later, vec![3, 4], "the rest once hw covers it");
}

#[test]
fn a_topic_led_in_parts_names_the_node_that_leads_most_of_it() {
    let follower = |node: &str| follower_of(node, 1);
    assert_eq!(majority_leader([]), None);
    assert_eq!(
        majority_leader([PartitionRole::Leader { epoch: 1 }]),
        None,
        "this node's own partitions name nobody"
    );
    assert_eq!(
        majority_leader([follower("node-c"), follower("node-b"), follower("node-b")]),
        Some("node-b".to_string())
    );
    assert_eq!(
        majority_leader([follower("node-c"), follower("node-b")]),
        Some("node-b".to_string()),
        "a tie goes to the smaller id"
    );
    assert_eq!(
        majority_leader([follower(""), follower("node-c")]),
        Some("node-c".to_string()),
        "an unknown leader does not count"
    );
}

#[test]
fn an_empty_leader_id_is_no_leader_in_a_refusal() {
    assert!(matches!(
        role_not_leader_error(follower_of("", 9)),
        BusServiceError::NotLeader {
            leader_node_id: None,
            leader_epoch: 9
        }
    ));
}

/// `reads_from_replica` is what the group and lag wires say about themselves.
#[test]
fn a_topic_is_read_from_a_replica_only_where_this_node_follows() {
    let (_tmp, svc) = test_service();
    let ctx = test_ctx("org-1");
    one_partition_topic(&svc, &ctx, TOPIC);
    assert!(!svc.reads_from_replica("org-1", TOPIC), "no coordinator");
    let coord = FakeCoordinator::leader(1);
    svc.set_replication(coord.clone());
    assert!(!svc.reads_from_replica("org-1", TOPIC));
    coord.set_role(follower_of("node-a", 1));
    assert!(svc.reads_from_replica("org-1", TOPIC));
    coord.set_role(PartitionRole::Unavailable {
        reason: UnavailableReason::NoAssignment,
    });
    assert!(!svc.reads_from_replica("org-1", TOPIC), "no replica here");
    assert!(!svc.reads_from_replica("org-1", "no.such.topic"));
}

fn replicated(group: &str, paused: bool, updated_at_ms: i64) -> replication::frames::ReplGroup {
    replication::frames::ReplGroup {
        group: group.to_string(),
        commit_mode: groups::CommitMode::Explicit.as_str().to_string(),
        paused,
        created_at_ms: 1,
        updated_at_ms,
    }
}

/// A leader's group rows are last-writer-wins by `(leader epoch, updated at)`,
/// and what `fetch` cached about the group is dropped when one is written.
#[test]
fn a_replicated_group_row_is_applied_by_epoch_then_time_and_drops_the_cache() {
    let (_tmp, svc) = test_service();
    let ctx = test_ctx("org-1");
    one_partition_topic(&svc, &ctx, TOPIC);
    let sink = replication::glue::PartitionProvider::follower_stores(&svc)
        .groups
        .expect("a group row sink");
    let paused = |svc: &BusService| svc.group_state.paused(&svc.local_db, "org-1", "g1", TOPIC);

    assert!(
        !paused(&svc).unwrap(),
        "primes the cache: no row, not paused"
    );
    sink.apply("org-1", TOPIC, 2, &[replicated("g1", true, 100)])
        .unwrap();
    assert!(paused(&svc).unwrap(), "the cached answer was dropped");
    let row = crate::db::repository::bus_group_get(&svc.local_db, "org-1", "g1", TOPIC)
        .unwrap()
        .expect("the row was created");
    assert_eq!(
        (
            row.commit_mode.as_str(),
            row.created_at_ms,
            row.updated_at_ms
        ),
        ("explicit", 1, 100)
    );

    sink.apply("org-1", TOPIC, 2, &[replicated("g1", false, 90)])
        .unwrap();
    assert!(paused(&svc).unwrap(), "older in the same epoch: kept");
    sink.apply("org-1", TOPIC, 1, &[replicated("g1", false, 500)])
        .unwrap();
    assert!(
        paused(&svc).unwrap(),
        "an older epoch loses whatever its clock says"
    );
    sink.apply("org-1", TOPIC, 2, &[replicated("g1", false, 100)])
        .unwrap();
    assert!(
        !paused(&svc).unwrap(),
        "same epoch, same time: the frame applies again"
    );
    sink.apply("org-1", TOPIC, 3, &[replicated("g1", true, 50)])
        .unwrap();
    assert!(
        paused(&svc).unwrap(),
        "a newer epoch wins with an earlier clock"
    );

    assert!(
        sink.apply("org-1", TOPIC, 3, &[replicated("bad group", false, 1)])
            .is_err(),
        "a group name that is no key is refused like a local one"
    );
}

/// The leader tells its replicas whenever it writes a group row, and the
/// snapshot a stream sends holds every row of the topic.
#[test]
fn opening_and_pausing_a_group_tell_the_replicas_and_the_rows_are_listed() {
    let (_tmp, svc) = test_service();
    let ctx = test_ctx("org-1");
    one_partition_topic(&svc, &ctx, TOPIC);
    let coord = FakeCoordinator::leader(1);
    svc.set_replication(coord.clone());

    let _consumer = svc
        .open_consumer(
            &ctx,
            "g1",
            &[TOPIC.to_string()],
            ConsumerConfig {
                commit_mode: groups::CommitMode::Explicit,
            },
        )
        .unwrap();
    assert_eq!(
        *coord.note_group_rows_calls.lock(),
        vec![("org-1".to_string(), TOPIC.to_string())]
    );
    svc.pause_group(&ctx, "g1", TOPIC).unwrap();
    assert_eq!(coord.note_group_rows_calls.lock().len(), 2);

    let rows = replication::glue::PartitionProvider::group_rows(&svc, "org-1", TOPIC);
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0].group.as_str(), rows[0].paused), ("g1", true));
    assert!(
        replication::glue::PartitionProvider::group_rows(&svc, "org-1", "other.topic").is_empty()
    );
}

/// A replica samples lag only up to what it cannot be cut below.
#[test]
fn the_lag_sampler_of_a_replica_stops_at_the_committed_offset() {
    let (_tmp, svc) = test_service();
    let ctx = test_ctx("org-1");
    topic_with_watermarks(&svc, &ctx, 4, 2);
    insert_group_row(&svc, "org-1", "g1", TOPIC);
    let coord = FakeCoordinator::leader(1);
    svc.set_replication(coord.clone());

    svc.sample_lag_history(1_800_000_000_000);
    coord.set_role(follower_of("node-a", 1));
    svc.sample_lag_history(1_800_000_060_000);
    let series = lag_history::group_series(&svc.local_db, "org-1", 0, None, None).unwrap();
    let lag: Vec<u64> = series[0].points.iter().map(|p| p.lag_total).collect();
    assert_eq!(
        lag,
        vec![4, 2],
        "the leader samples to hw, the replica to committed"
    );
}
