// === File: peer_registry/tests.rs — pure state machine + registry unit tests ===

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::mesh::peer_registry::delta::{ConnectionStateTag, PeerDelta, PeerOutcome};
use crate::mesh::peer_registry::entry::{NodeId, RetryState, TransportHints};
use crate::mesh::peer_registry::shard::{shard_for, NUM_SHARDS};
use crate::mesh::peer_registry::state::{
    backoff, transition, ActivePath, ConnectionState, DialPath, StateTrigger, TransitionSideEffect,
};
use crate::mesh::peer_registry::PeerRegistry;

fn t0() -> Instant {
    Instant::now()
}

fn err(s: &str) -> Arc<str> {
    Arc::<str>::from(s)
}

fn dummy_path() -> ActivePath {
    ActivePath::Direct {
        addr: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 1), 9000)),
    }
}

#[test]
fn transition_disconnected_to_connecting_on_dial_started() {
    let now = t0();
    let r = transition(
        &ConnectionState::Disconnected,
        &RetryState::default(),
        &StateTrigger::DialStarted { via: DialPath::Direct },
        now,
    );
    assert!(matches!(r.new_state, Some(ConnectionState::Connecting { .. })));
}

#[test]
fn transition_connecting_to_connected_on_dial_ok() {
    let now = t0();
    let cur = ConnectionState::Connecting { since: now, via: DialPath::Direct };
    let r = transition(
        &cur,
        &RetryState::default(),
        &StateTrigger::DialOk { conn_id: 1, path: dummy_path() },
        now,
    );
    assert!(matches!(r.new_state, Some(ConnectionState::Connected { conn_id: 1, .. })));
    assert!(matches!(r.side_effect, Some(TransitionSideEffect::EmitOnline)));
}

#[test]
fn transition_connecting_to_reconnecting_on_dial_fail() {
    let now = t0();
    let cur = ConnectionState::Connecting { since: now, via: DialPath::Direct };
    let r = transition(
        &cur,
        &RetryState::default(),
        &StateTrigger::DialFail { err: err("e1") },
        now,
    );
    match r.new_state {
        Some(ConnectionState::Reconnecting { attempt, backoff_until, .. }) => {
            assert_eq!(attempt, 1);
            // 1s backoff for attempt=1.
            let dt = backoff_until.saturating_duration_since(now);
            assert_eq!(dt, Duration::from_millis(1000));
        }
        other => panic!("unexpected state: {other:?}"),
    }
    assert!(matches!(r.side_effect, Some(TransitionSideEffect::BumpRetry { .. })));
}

#[test]
fn transition_reconnecting_increments_attempt() {
    let now = t0();
    let cur = ConnectionState::Reconnecting {
        since: now,
        attempt: 1,
        backoff_until: now + Duration::from_millis(1000),
        last_err: err("e1"),
    };
    let r = transition(
        &cur,
        &RetryState { attempts: 1, ..Default::default() },
        &StateTrigger::DialFail { err: err("e2") },
        now,
    );
    match r.new_state {
        Some(ConnectionState::Reconnecting { attempt, backoff_until, .. }) => {
            assert_eq!(attempt, 2);
            assert_eq!(backoff_until.saturating_duration_since(now), Duration::from_millis(2000));
        }
        other => panic!("unexpected state: {other:?}"),
    }
}

#[test]
fn transition_connected_to_degraded_on_liveness_15s_no_hb() {
    let now = t0();
    let cur = ConnectionState::Connected { since: now, path: dummy_path(), conn_id: 7 };
    let later = now + Duration::from_secs(16);
    let r = transition(
        &cur,
        &RetryState::default(),
        &StateTrigger::LivenessTick { now: later },
        later,
    );
    assert!(matches!(r.new_state, Some(ConnectionState::Degraded { conn_id: 7, .. })));
}

#[test]
fn transition_degraded_to_reconnecting_on_liveness_45s_no_hb() {
    let now = t0();
    let cur = ConnectionState::Degraded {
        since: now,
        path: dummy_path(),
        conn_id: 7,
        missed_heartbeats: 2,
    };
    let later = now + Duration::from_secs(50);
    let r = transition(
        &cur,
        &RetryState::default(),
        &StateTrigger::LivenessTick { now: later },
        later,
    );
    assert!(matches!(r.new_state, Some(ConnectionState::Reconnecting { .. })));
    assert!(matches!(r.side_effect, Some(TransitionSideEffect::ScheduleDial { .. })));
}

#[test]
fn transition_reconnecting_to_offline_after_5min() {
    let now = t0();
    let cur = ConnectionState::Reconnecting {
        since: now,
        attempt: 3,
        backoff_until: now + Duration::from_millis(4000),
        last_err: err("e"),
    };
    let later = now + Duration::from_secs(5 * 60 + 1);
    let r = transition(
        &cur,
        &RetryState { attempts: 3, ..Default::default() },
        &StateTrigger::LivenessTick { now: later },
        later,
    );
    assert!(matches!(r.new_state, Some(ConnectionState::Offline { .. })));
    assert!(matches!(r.side_effect, Some(TransitionSideEffect::EmitOffline)));
}

#[test]
fn transition_reconnecting_to_offline_after_12_attempts() {
    let now = t0();
    let cur = ConnectionState::Reconnecting {
        since: now,
        attempt: 13,
        backoff_until: now + Duration::from_millis(1000),
        last_err: err("e"),
    };
    let r = transition(
        &cur,
        &RetryState { attempts: 13, ..Default::default() },
        &StateTrigger::LivenessTick { now: now + Duration::from_secs(1) },
        now + Duration::from_secs(1),
    );
    assert!(matches!(r.new_state, Some(ConnectionState::Offline { .. })));
}

#[test]
fn transition_heartbeat_recovers_degraded_to_connected() {
    let now = t0();
    let cur = ConnectionState::Degraded {
        since: now,
        path: dummy_path(),
        conn_id: 9,
        missed_heartbeats: 3,
    };
    let later = now + Duration::from_secs(20);
    let r = transition(
        &cur,
        &RetryState::default(),
        &StateTrigger::Heartbeat { at: later },
        later,
    );
    assert!(matches!(r.new_state, Some(ConnectionState::Connected { conn_id: 9, .. })));
}

#[test]
fn transition_transport_closed_with_stale_conn_id_ignored() {
    let now = t0();
    let cur = ConnectionState::Connected { since: now, path: dummy_path(), conn_id: 100 };
    let r = transition(
        &cur,
        &RetryState::default(),
        &StateTrigger::TransportClosed { conn_id: 42 },
        now,
    );
    assert!(r.new_state.is_none());
    assert!(r.side_effect.is_none());
}

#[test]
fn transition_transport_closed_with_current_conn_id_to_reconnecting() {
    let now = t0();
    let cur = ConnectionState::Connected { since: now, path: dummy_path(), conn_id: 100 };
    let r = transition(
        &cur,
        &RetryState::default(),
        &StateTrigger::TransportClosed { conn_id: 100 },
        now,
    );
    assert!(matches!(r.new_state, Some(ConnectionState::Reconnecting { attempt: 1, .. })));
    assert!(matches!(r.side_effect, Some(TransitionSideEffect::ScheduleDial { .. })));
}

#[test]
fn transition_discovered_on_offline_returns_to_disconnected_with_schedule() {
    let now = t0();
    let cur = ConnectionState::Offline { since: now };
    let r = transition(
        &cur,
        &RetryState::default(),
        &StateTrigger::Discovered,
        now,
    );
    assert!(matches!(r.new_state, Some(ConnectionState::Disconnected)));
    assert!(matches!(r.side_effect, Some(TransitionSideEffect::ScheduleDial { .. })));
}

#[test]
fn transition_discovered_on_connected_no_change() {
    let now = t0();
    let cur = ConnectionState::Connected { since: now, path: dummy_path(), conn_id: 1 };
    let r = transition(
        &cur,
        &RetryState::default(),
        &StateTrigger::Discovered,
        now,
    );
    assert!(r.new_state.is_none());
    assert!(r.side_effect.is_none());
}

#[test]
fn transition_forget_emits_forget_side_effect() {
    let now = t0();
    let cur = ConnectionState::Connected { since: now, path: dummy_path(), conn_id: 1 };
    let r = transition(
        &cur,
        &RetryState::default(),
        &StateTrigger::Forget,
        now,
    );
    assert!(matches!(r.side_effect, Some(TransitionSideEffect::EmitForget)));
}

#[test]
fn backoff_progression() {
    assert_eq!(backoff(1), Duration::from_millis(1000));
    assert_eq!(backoff(2), Duration::from_millis(2000));
    assert_eq!(backoff(3), Duration::from_millis(4000));
    assert_eq!(backoff(4), Duration::from_millis(8000));
    // Cap at 5min kicks in once 2^(attempt-1)*1s exceeds 300s — i.e. for attempt>=10.
    assert_eq!(backoff(9), Duration::from_millis(256_000));
    assert_eq!(backoff(10), Duration::from_millis(300_000)); // cap
    assert_eq!(backoff(11), Duration::from_millis(300_000)); // cap
    assert_eq!(backoff(20), Duration::from_millis(300_000)); // cap
}

#[test]
fn shard_distribution_uniform() {
    use std::collections::HashMap;
    // Deterministic pseudo-random: linear congruential filler.
    let mut counts: HashMap<usize, u32> = HashMap::new();
    let mut seed: u64 = 0xDEAD_BEEF_CAFE_F00D;
    for _ in 0..10_000 {
        let mut id: NodeId = [0u8; 32];
        for byte in id.iter_mut() {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            *byte = (seed >> 56) as u8;
        }
        let idx = shard_for(&id, NUM_SHARDS);
        *counts.entry(idx).or_insert(0) += 1;
    }
    let expected = 10_000.0 / NUM_SHARDS as f64;
    let lo = (expected * 0.7) as u32;
    let hi = (expected * 1.3) as u32;
    for s in 0..NUM_SHARDS {
        let c = *counts.get(&s).unwrap_or(&0);
        assert!(
            c >= lo && c <= hi,
            "shard {s} got {c}, expected within [{lo}, {hi}]"
        );
    }
}

#[test]
fn peer_registry_upsert_discovered_creates_entry_and_emits_created() {
    let reg = PeerRegistry::new(16);
    let id: NodeId = [1u8; 32];
    let hints = TransportHints::default();
    let out = reg.upsert_discovered(id, hints);
    assert!(matches!(out, PeerOutcome::Created { .. }));
    assert_eq!(reg.len(), 1);
    let detail = reg.snapshot_detail(&id).expect("entry present");
    assert_eq!(detail.summary.conn_tag, ConnectionStateTag::Disconnected);
}

#[test]
fn peer_registry_upsert_existing_emits_no_change_for_same_hints() {
    let reg = PeerRegistry::new(16);
    let id: NodeId = [2u8; 32];
    let hints = TransportHints::default();
    let _ = reg.upsert_discovered(id, hints.clone());
    let out = reg.upsert_discovered(id, hints);
    assert!(matches!(out, PeerOutcome::NoChange));
}

#[test]
fn peer_registry_subscribe_receives_delta_after_mutation() {
    let reg = PeerRegistry::new(16);
    let mut rx = reg.subscribe();
    let id: NodeId = [3u8; 32];
    let _ = reg.upsert_discovered(id, TransportHints::default());
    // Drain — at least one delta must be present.
    let got = rx.try_recv().expect("delta received");
    match got {
        PeerDelta::Discovered { node_id } => assert_eq!(node_id, id),
        other => panic!("unexpected: {other:?}"),
    }
}

// =============================================================================
// PR5: PersistenceWriter wiring + hydrate_from_db
// =============================================================================

use crate::mesh::peer_registry::persistence::{PersistOp, PersistSink, PendingWriteSnapshot};
use crate::mesh::peer_registry::HintKind;

#[derive(Default)]
struct CountSink {
    inner: std::sync::Mutex<Vec<Vec<(NodeId, PendingWriteSnapshot)>>>,
}

impl PersistSink for CountSink {
    fn write_peer_batch(
        &self,
        ops: &[(NodeId, PendingWriteSnapshot)],
    ) -> anyhow::Result<()> {
        self.inner.lock().unwrap().push(ops.to_vec());
        Ok(())
    }
}

fn install_test_writer(reg: &PeerRegistry) -> Arc<std::sync::Mutex<Vec<PersistOp>>> {
    use tokio::sync::mpsc;
    let collected: Arc<std::sync::Mutex<Vec<PersistOp>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (tx, mut rx) = mpsc::channel::<PersistOp>(1024);
    let collected_clone = collected.clone();
    // Drain the channel synchronously into a vector for inspection. Spawn a
    // background task on the current (test) tokio runtime.
    tokio::spawn(async move {
        while let Some(op) = rx.recv().await {
            collected_clone.lock().unwrap().push(op);
        }
    });
    reg.set_persistence(tx);
    collected
}

#[tokio::test(flavor = "current_thread")]
async fn peer_registry_record_heartbeat_no_persist_within_30s_bucket() {
    let reg = PeerRegistry::new(16);
    let collected = install_test_writer(&reg);
    let id: NodeId = [11u8; 32];
    let _ = reg.upsert_discovered(id, TransportHints::default());
    // Yield so the writer task can drain the discovery upsert ops.
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let baseline = collected.lock().unwrap().len();

    // 5 heartbeats spaced 20ms apart — all in the same wall-clock 30s bucket.
    let now = Instant::now();
    for i in 0..5 {
        let _ = reg.record_heartbeat(&id, now + Duration::from_millis(i * 20));
    }
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    // The first record_heartbeat sees prev_hb = None and persists once
    // (bucket_advanced=true). Subsequent calls within the same bucket must
    // NOT persist. Therefore we expect at most 1 new op vs baseline.
    let after = collected.lock().unwrap().len();
    assert!(
        after - baseline <= 1,
        "expected ≤1 persist op for 5 same-bucket heartbeats, got {}",
        after - baseline
    );
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn peer_registry_record_heartbeat_persists_on_bucket_change() {
    let reg = PeerRegistry::new(16);
    let collected = install_test_writer(&reg);
    let id: NodeId = [12u8; 32];
    let _ = reg.upsert_discovered(id, TransportHints::default());
    // Heartbeat persistence requires a known pubkey (peer_persisted.pubkey
    // is NOT NULL). Install one before exercising the bucket-advance path.
    let _ = reg.set_pubkey(&id, Arc::<[u8]>::from(&[12u8; 32][..]));
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let now = Instant::now();
    let _ = reg.record_heartbeat(&id, now);
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let after_first = collected.lock().unwrap().len();

    // Advance virtual time past a 30s bucket boundary and record another HB.
    tokio::time::advance(Duration::from_secs(31)).await;
    let _ = reg.record_heartbeat(&id, now + Duration::from_secs(31));
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let after_second = collected.lock().unwrap().len();
    assert!(
        after_second > after_first,
        "expected ≥1 additional persist op when bucket advances (got {} → {})",
        after_first, after_second
    );
}

#[tokio::test(flavor = "current_thread")]
async fn peer_registry_hydrate_from_db_loads_trusted_with_hints() {
    use crate::db::repository::{
        replace_peer_hints, upsert_peer_persisted_batch, PeerHintRow, PeerPersistedRow,
        HINT_KIND_DIRECT_ADDR, HINT_KIND_HOSTNAME, HINT_KIND_RELAY_URL, ROLE_NODE, TRUST_TRUSTED,
    };
    use std::path::Path;

    // Spin up an in-memory DB by running migrations against a temp file.
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("test.db");
    let pool = crate::db::init(Path::new(&db_path)).expect("db init");

    let id_a: NodeId = [0xAA; 32];
    let id_b: NodeId = [0xBB; 32];

    let now_ms: i64 = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    upsert_peer_persisted_batch(
        &pool,
        &[
            PeerPersistedRow {
                node_id: id_a,
                pubkey: vec![0xAA; 32],
                trust_state: TRUST_TRUSTED,
                hostname: Some("host-a".into()),
                platform: Some("linux".into()),
                role: ROLE_NODE,
                last_seen_ms: 0,
                persisted_ver: 1,
                updated_at_ms: now_ms,
            },
            PeerPersistedRow {
                node_id: id_b,
                pubkey: vec![0xBB; 32],
                trust_state: TRUST_TRUSTED,
                hostname: Some("host-b".into()),
                platform: None,
                role: ROLE_NODE,
                last_seen_ms: 0,
                persisted_ver: 1,
                updated_at_ms: now_ms,
            },
        ],
    )
    .expect("upsert_peer_persisted_batch");

    replace_peer_hints(
        &pool,
        &id_a,
        &[
            PeerHintRow {
                node_id: id_a,
                hint_kind: HINT_KIND_DIRECT_ADDR,
                payload: "10.0.0.1:9000".into(),
                last_ok_ms: None,
                fail_count: 0,
            },
            PeerHintRow {
                node_id: id_a,
                hint_kind: HINT_KIND_RELAY_URL,
                payload: "https://relay.example/".into(),
                last_ok_ms: None,
                fail_count: 0,
            },
            PeerHintRow {
                node_id: id_a,
                hint_kind: HINT_KIND_HOSTNAME,
                payload: "host-a.local".into(),
                last_ok_ms: None,
                fail_count: 0,
            },
        ],
    )
    .expect("replace_peer_hints");

    let reg = PeerRegistry::new(16);
    let n = reg.hydrate_from_db(&pool).expect("hydrate");
    assert_eq!(n, 2);
    assert_eq!(reg.snapshot_summary().len(), 2);

    let detail_a = reg.snapshot_detail(&id_a).expect("detail A");
    assert_eq!(detail_a.summary.hostname.as_ref(), "host-a");
    assert_eq!(detail_a.hints.addresses.len(), 1);
    assert!(detail_a.hints.relay_url.is_some());
    assert!(detail_a.hints.hostname_dns.is_some());

    let detail_b = reg.snapshot_detail(&id_b).expect("detail B");
    assert_eq!(detail_b.summary.hostname.as_ref(), "host-b");
    assert_eq!(detail_b.hints.addresses.len(), 0);

    // Suppress unused-import warning when this test happens to be filtered out.
    let _ = HintKind::DirectAddr;
}

// =============================================================================
// PR6a: pubkey field is required before peer_persisted UpsertEntry
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn peer_registry_set_pubkey_persists_when_provided() {
    let reg = PeerRegistry::new(16);
    let collected = install_test_writer(&reg);
    let id: NodeId = [42u8; 32];

    let _ = reg.upsert_discovered(id, TransportHints::default());
    // No pubkey yet — so far, only hint ops can have flowed.
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let pubkey = Arc::<[u8]>::from(&[7u8; 32][..]);
    let outcome = reg.set_pubkey(&id, pubkey.clone());
    assert!(matches!(outcome, PeerOutcome::Changed { .. }));
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let ops = collected.lock().unwrap();
    let upsert_with_pubkey = ops.iter().rev().find_map(|op| match op {
        PersistOp::UpsertEntry { snapshot, node_id, .. } if node_id == &id => {
            Some(snapshot.pubkey.clone())
        }
        _ => None,
    });
    assert_eq!(
        upsert_with_pubkey.as_deref(),
        Some(&pubkey[..]),
        "set_pubkey must produce an UpsertEntry whose snapshot carries the bytes"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn peer_registry_no_persist_when_pubkey_missing() {
    let reg = PeerRegistry::new(16);
    let collected = install_test_writer(&reg);
    let id: NodeId = [43u8; 32];

    let _ = reg.upsert_discovered(id, TransportHints::default());
    let _ = reg.set_hostname(&id, Arc::<str>::from("host-no-pk"));
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    let ops = collected.lock().unwrap();
    let upserts: Vec<_> = ops
        .iter()
        .filter(|op| matches!(op, PersistOp::UpsertEntry { .. }))
        .collect();
    assert!(
        upserts.is_empty(),
        "no UpsertEntry op should be emitted while pubkey is None (got {})",
        upserts.len()
    );
    // Hints, however, may flow — peer_hints does not strictly require a parent
    // row in this test sink (the real DB sink would FK-reject; that is fine
    // because the next set_pubkey call re-emits both ops together).
}
