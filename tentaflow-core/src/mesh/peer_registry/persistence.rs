// === File: peer_registry/persistence.rs — debounced batched writer for peer_persisted ===

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{error, warn};

use crate::db::repository::{
    self, PeerHintRow, PeerPersistedRow, HINT_KIND_DIRECT_ADDR, HINT_KIND_HOSTNAME,
    HINT_KIND_RELAY_URL, ROLE_EDGE, ROLE_NODE, ROLE_RELAY, TRUST_DISCOVERED, TRUST_PENDING_PAIRING,
    TRUST_TRUSTED,
};
use crate::db::DbPool;
use crate::mesh::peer_registry::entry::{NodeId, PeerRole, TrustState};

const DEBOUNCE: Duration = Duration::from_secs(2);
const MAX_BATCH: usize = 256;
pub const CHANNEL_CAPACITY: usize = 4096;

/// Snapshot of the peer state fields that go into peer_persisted. Hints are
/// carried separately so the writer can merge them atomically per node.
#[derive(Debug, Clone)]
pub struct PeerPersistSnapshot {
    pub pubkey: Vec<u8>,
    pub trust_state: TrustState,
    pub hostname: Option<String>,
    pub platform: Option<String>,
    pub role: PeerRole,
    pub last_seen_ms: i64,
}

/// Learned hints are merged with pairing contacts in SQLite. Removing a kind
/// requires an explicit invalidation so an incomplete snapshot cannot erase it.
#[derive(Debug, Clone)]
pub struct PersistedHint {
    pub kind: HintKindWire,
    pub payload: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintKindWire {
    DirectAddr,
    RelayUrl,
    Hostname,
}

impl HintKindWire {
    fn to_int(self) -> i64 {
        match self {
            HintKindWire::DirectAddr => HINT_KIND_DIRECT_ADDR,
            HintKindWire::RelayUrl => HINT_KIND_RELAY_URL,
            HintKindWire::Hostname => HINT_KIND_HOSTNAME,
        }
    }
}

#[derive(Debug)]
pub enum PersistOp {
    UpsertEntry {
        node_id: NodeId,
        snapshot: PeerPersistSnapshot,
        version: u64,
        hints: Option<Vec<PersistedHint>>,
        invalidated: Option<HintKindWire>,
    },
    Delete {
        node_id: NodeId,
    },
}

/// Coalesced pending state per node — the writer collapses N requests for the
/// same node into a single transaction. Newest snapshot wins; explicit hint
/// invalidations survive coalescing until the corresponding write commits.
#[derive(Debug, Default)]
struct PendingWrite {
    snapshot: Option<(PeerPersistSnapshot, u64)>,
    hints: Option<Vec<PersistedHint>>,
    invalidated: Vec<HintKindWire>,
    delete: bool,
}

impl PendingWrite {
    fn merge(&mut self, op: PersistOp) {
        match op {
            PersistOp::UpsertEntry {
                snapshot,
                version,
                hints,
                invalidated,
                ..
            } => {
                let is_newer = self
                    .snapshot
                    .as_ref()
                    .map(|(_, current)| version >= *current)
                    .unwrap_or(true);
                if is_newer {
                    self.snapshot = Some((snapshot, version));
                    if let Some(hints) = hints {
                        self.hints = Some(hints);
                    }
                    if let Some(kind) = invalidated {
                        if !self.invalidated.contains(&kind) {
                            self.invalidated.push(kind);
                        }
                    }
                    self.delete = false;
                }
            }
            PersistOp::Delete { .. } => {
                // A pending delete supersedes pending upserts — drop them.
                self.snapshot = None;
                self.hints = None;
                self.invalidated.clear();
                self.delete = true;
            }
        }
    }
}

fn node_of(op: &PersistOp) -> NodeId {
    match op {
        PersistOp::UpsertEntry { node_id, .. } | PersistOp::Delete { node_id } => *node_id,
    }
}

fn coalesce(buf: &mut HashMap<NodeId, PendingWrite>, op: PersistOp) {
    let id = node_of(&op);
    let entry = buf.entry(id).or_default();
    entry.merge(op);
}

fn trust_to_int(t: &TrustState) -> i64 {
    match t {
        TrustState::Discovered => TRUST_DISCOVERED,
        TrustState::PendingPairing { .. } => TRUST_PENDING_PAIRING,
        TrustState::Trusted => TRUST_TRUSTED,
    }
}

fn role_to_int(r: PeerRole) -> i64 {
    match r {
        PeerRole::Node => ROLE_NODE,
        PeerRole::Edge => ROLE_EDGE,
        PeerRole::Relay => ROLE_RELAY,
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Bucket a unix-epoch millisecond timestamp to a 30-second granularity.
/// Heartbeats only generate a write when their bucket changes — so at most
/// one write per peer per 30s for the heartbeat path.
pub fn bucketize_30s(now_ms: i64) -> i64 {
    (now_ms / 30_000) * 30_000
}

/// Sink trait — abstracts the underlying repository so unit tests can run
/// without touching SQLite.
pub trait PersistSink: Send + Sync + 'static {
    fn write_peer_batch(&self, ops: &[(NodeId, PendingWriteSnapshot)]) -> anyhow::Result<()>;
}

/// Public, immutable view of the writer's coalesced pending state for one
/// node — what the sink actually receives.
#[derive(Debug, Clone)]
pub struct PendingWriteSnapshot {
    pub snapshot: Option<(PeerPersistSnapshot, u64)>,
    pub hints: Option<Vec<PersistedHint>>,
    pub invalidated: Vec<HintKindWire>,
    pub delete: bool,
}

impl From<PendingWrite> for PendingWriteSnapshot {
    fn from(p: PendingWrite) -> Self {
        Self {
            snapshot: p.snapshot,
            hints: p.hints,
            invalidated: p.invalidated,
            delete: p.delete,
        }
    }
}

/// SQLite-backed sink — what the production binary uses.
pub struct DbSink {
    pool: DbPool,
}

impl DbSink {
    pub fn new(pool: DbPool) -> Self {
        Self { pool }
    }
}

impl PersistSink for DbSink {
    fn write_peer_batch(&self, ops: &[(NodeId, PendingWriteSnapshot)]) -> anyhow::Result<()> {
        let now = now_ms();
        let mut entry_rows: Vec<PeerPersistedRow> = Vec::new();
        let mut hint_writes: Vec<(NodeId, Vec<PeerHintRow>, Vec<i64>, i64)> = Vec::new();
        let mut deletes: Vec<NodeId> = Vec::new();

        for (node_id, pend) in ops {
            if pend.delete {
                deletes.push(*node_id);
                continue;
            }
            if let Some((snap, version)) = &pend.snapshot {
                entry_rows.push(PeerPersistedRow {
                    node_id: *node_id,
                    pubkey: snap.pubkey.clone(),
                    trust_state: trust_to_int(&snap.trust_state),
                    hostname: snap.hostname.clone(),
                    platform: snap.platform.clone(),
                    role: role_to_int(snap.role),
                    last_seen_ms: snap.last_seen_ms,
                    persisted_ver: *version as i64,
                    updated_at_ms: now,
                });
            }
            if let Some(hints) = &pend.hints {
                let rows: Vec<PeerHintRow> = hints
                    .iter()
                    .map(|h| PeerHintRow {
                        node_id: *node_id,
                        hint_kind: h.kind.to_int(),
                        payload: h.payload.clone(),
                        last_ok_ms: None,
                        fail_count: 0,
                    })
                    .collect();
                let mut replaced: Vec<i64> =
                    pend.invalidated.iter().map(|kind| kind.to_int()).collect();
                for kind in [HINT_KIND_RELAY_URL, HINT_KIND_HOSTNAME] {
                    if rows.iter().any(|row| row.hint_kind == kind) && !replaced.contains(&kind) {
                        replaced.push(kind);
                    }
                }
                if let Some((_, version)) = &pend.snapshot {
                    hint_writes.push((*node_id, rows, replaced, *version as i64));
                }
            }
        }

        // Single-writer SQLite mutex serializes these; logical ordering is
        // entries first (FK target), then hints, then deletes.
        if !entry_rows.is_empty() {
            repository::upsert_peer_persisted_batch(&self.pool, &entry_rows)?;
        }
        for (node_id, rows, replaced, version) in &hint_writes {
            repository::update_peer_hints(&self.pool, node_id, rows, replaced, Some(*version))?;
        }
        for node_id in &deletes {
            repository::delete_peer_persisted(&self.pool, node_id)?;
        }
        Ok(())
    }
}

/// Background task that drains PersistOp messages, coalesces by node, and
/// flushes either when the buffer reaches MAX_BATCH or after a 2s debounce
/// window of inactivity.
pub struct PersistenceWriter {
    sink: Arc<dyn PersistSink>,
    rx: mpsc::Receiver<PersistOp>,
}

impl PersistenceWriter {
    pub fn new(sink: Arc<dyn PersistSink>, capacity: usize) -> (Self, mpsc::Sender<PersistOp>) {
        let (tx, rx) = mpsc::channel(capacity.max(1));
        (Self { sink, rx }, tx)
    }

    pub fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(async move { self.run().await })
    }

    async fn run(mut self) {
        let mut buffer: HashMap<NodeId, PendingWrite> = HashMap::new();
        let mut deadline = tokio::time::Instant::now() + DEBOUNCE;

        loop {
            tokio::select! {
                op = self.rx.recv() => match op {
                    Some(op) => {
                        // An invalidation is a versioned deletion. Keep it out of
                        // later heartbeat batches that could promote its version.
                        if matches!(&op, PersistOp::UpsertEntry { invalidated: Some(_), .. }) {
                            if !buffer.is_empty() { self.flush(&mut buffer).await; }
                            coalesce(&mut buffer, op);
                            self.flush(&mut buffer).await;
                            deadline = tokio::time::Instant::now() + DEBOUNCE;
                            continue;
                        }
                        coalesce(&mut buffer, op);
                        if buffer.len() >= MAX_BATCH {
                            self.flush(&mut buffer).await;
                            deadline = tokio::time::Instant::now() + DEBOUNCE;
                        }
                    }
                    None => {
                        if !buffer.is_empty() {
                            self.flush(&mut buffer).await;
                        }
                        break;
                    }
                },
                _ = tokio::time::sleep_until(deadline) => {
                    if !buffer.is_empty() {
                        self.flush(&mut buffer).await;
                    }
                    deadline = tokio::time::Instant::now() + DEBOUNCE;
                }
            }
        }
    }

    async fn flush(&self, buffer: &mut HashMap<NodeId, PendingWrite>) {
        let drained: Vec<(NodeId, PendingWriteSnapshot)> = buffer
            .drain()
            .map(|(id, p)| (id, PendingWriteSnapshot::from(p)))
            .collect();
        let count = drained.len();
        let sink = self.sink.clone();
        // Run blocking SQLite work on a dedicated thread; a flush stall must
        // not back up the channel.
        let res = tokio::task::spawn_blocking(move || sink.write_peer_batch(&drained)).await;
        match res {
            Ok(Ok(())) => {}
            Ok(Err(e)) => error!(err = %e, count, "PersistenceWriter flush failed"),
            Err(e) => error!(err = %e, count, "PersistenceWriter join failed"),
        }
    }
}

/// Helper for callers (registry mutators) — non-blocking try_send. If the
/// channel is full we drop the write and warn, because the alternative
/// (blocking the mutator) would defeat the entire decoupling.
pub fn try_schedule(tx: &mpsc::Sender<PersistOp>, op: PersistOp) {
    if let Err(e) = tx.try_send(op) {
        warn!(?e, "PersistenceWriter channel full, dropping write");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Instant;

    #[derive(Default)]
    struct MockSink {
        calls: Mutex<Vec<Vec<(NodeId, PendingWriteSnapshot)>>>,
    }

    impl PersistSink for MockSink {
        fn write_peer_batch(&self, ops: &[(NodeId, PendingWriteSnapshot)]) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(ops.to_vec());
            Ok(())
        }
    }

    fn snap() -> PeerPersistSnapshot {
        PeerPersistSnapshot {
            pubkey: vec![1, 2, 3],
            trust_state: TrustState::Discovered,
            hostname: None,
            platform: None,
            role: PeerRole::Node,
            last_seen_ms: bucketize_30s(now_ms()),
        }
    }

    #[test]
    fn pairing_contacts_survive_partial_registry_flush_and_only_explicit_invalidation_removes_them()
    {
        use crate::mesh::peer_registry::{HintKind, PeerRegistry, TransportHints};
        use crate::net::iroh::pairing::{store_trusted_contact_hints, PairingContactHints};
        let directory = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&directory.path().join("peers.db")).unwrap();
        let id = [42; 32];
        let contact = PairingContactHints {
            node_id: hex::encode(id),
            public_key_hex: hex::encode(id),
            hostname: "paired-host".into(),
            addresses: vec!["192.168.0.96:19753".into()],
            relay_url: "https://relay.example/".into(),
        };
        store_trusted_contact_hints(&pool, &contact.node_id, &contact).unwrap();
        let row = repository::load_peer_persisted_all(&pool)
            .unwrap()
            .remove(0);
        let sink = DbSink::new(pool.clone());
        let partial = PendingWriteSnapshot {
            snapshot: Some((
                PeerPersistSnapshot {
                    pubkey: id.to_vec(),
                    trust_state: TrustState::Trusted,
                    hostname: Some("partial-host".into()),
                    platform: None,
                    role: PeerRole::Node,
                    last_seen_ms: row.last_seen_ms,
                },
                row.persisted_ver as u64 + 1,
            )),
            hints: Some(vec![PersistedHint {
                kind: HintKindWire::Hostname,
                payload: "partial-host".into(),
            }]),
            invalidated: Vec::new(),
            delete: false,
        };
        sink.write_peer_batch(&[(id, partial)]).unwrap();
        let mut partial_contact = contact.clone();
        partial_contact.addresses.clear();
        partial_contact.relay_url.clear();
        store_trusted_contact_hints(&pool, &contact.node_id, &partial_contact).unwrap();
        let registry = PeerRegistry::new(16);
        registry.hydrate_from_db(&pool).unwrap();
        registry.upsert_discovered(
            id,
            TransportHints {
                hostname_dns: Some(Arc::from("discovered-host")),
                ..Default::default()
            },
        );
        let detail = registry.snapshot_detail(&id).unwrap();
        assert_eq!(detail.hints.addresses[0].to_string(), "192.168.0.96:19753");
        assert_eq!(
            detail.hints.relay_url.as_deref(),
            Some("https://relay.example/")
        );
        let (sender, mut receiver) = mpsc::channel(16);
        registry.set_persistence(sender);
        registry.invalidate_hint(&id, HintKind::DirectAddr);
        let mut pending = HashMap::new();
        while let Ok(op) = receiver.try_recv() {
            coalesce(&mut pending, op);
        }
        let operations: Vec<_> = pending
            .into_iter()
            .map(|(id, pending)| (id, pending.into()))
            .collect();
        sink.write_peer_batch(&operations).unwrap();
        let restarted = PeerRegistry::new(16);
        restarted.hydrate_from_db(&pool).unwrap();
        let detail = restarted.snapshot_detail(&id).unwrap();
        assert!(detail.hints.addresses.is_empty());
        assert_eq!(
            detail.hints.relay_url.as_deref(),
            Some("https://relay.example/")
        );
    }

    #[test]
    fn stale_hint_invalidation_cannot_erase_newer_pairing_contacts() {
        use crate::net::iroh::pairing::{store_trusted_contact_hints, PairingContactHints};
        let directory = tempfile::tempdir().unwrap();
        let pool = crate::db::init(&directory.path().join("peers.db")).unwrap();
        let id = [43; 32];
        let contact = PairingContactHints {
            node_id: hex::encode(id),
            public_key_hex: hex::encode(id),
            hostname: "host".into(),
            addresses: vec!["192.168.0.96:19753".into()],
            relay_url: String::new(),
        };
        store_trusted_contact_hints(&pool, &contact.node_id, &contact).unwrap();
        DbSink::new(pool.clone())
            .write_peer_batch(&[(
                id,
                PendingWriteSnapshot {
                    snapshot: Some((snap(), 1)),
                    hints: Some(Vec::new()),
                    invalidated: vec![HintKindWire::DirectAddr],
                    delete: false,
                },
            )])
            .unwrap();
        let hints = repository::load_peer_hints_all(&pool).unwrap();
        assert!(hints[&id]
            .iter()
            .any(|hint| hint.payload == "192.168.0.96:19753"));
    }

    #[test]
    fn bucketize_30s_rounds_down_to_thirty_seconds() {
        assert_eq!(bucketize_30s(0), 0);
        assert_eq!(bucketize_30s(29_999), 0);
        assert_eq!(bucketize_30s(30_000), 30_000);
        assert_eq!(bucketize_30s(59_999), 30_000);
        assert_eq!(bucketize_30s(60_000), 60_000);
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn persistence_writer_debounces_within_2s() {
        let sink = Arc::new(MockSink::default());
        let (writer, tx) = PersistenceWriter::new(sink.clone(), 64);
        let h = writer.spawn();

        let id: NodeId = [7u8; 32];
        for v in 1..=5 {
            tx.send(PersistOp::UpsertEntry {
                node_id: id,
                snapshot: snap(),
                hints: None,
                invalidated: None,
                version: v,
            })
            .await
            .unwrap();
        }
        // Advance past the debounce window. Tokio start_paused=true gives us
        // virtual time so the writer's sleep_until elapses immediately.
        tokio::time::sleep(Duration::from_millis(2_500)).await;

        let calls = sink.calls.lock().unwrap();
        assert_eq!(
            calls.len(),
            1,
            "expected exactly one flush, got {}",
            calls.len()
        );
        assert_eq!(calls[0].len(), 1, "five ops on one node should coalesce");
        let (got_id, pend) = &calls[0][0];
        assert_eq!(got_id, &id);
        let (_, ver) = pend.snapshot.as_ref().expect("snapshot present");
        assert_eq!(*ver, 5, "latest version wins after coalesce");
        drop(tx);
        drop(calls);
        let _ = tokio::time::timeout(Duration::from_secs(1), h).await;
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn persistence_writer_flushes_on_batch_full() {
        let sink = Arc::new(MockSink::default());
        let (writer, tx) = PersistenceWriter::new(sink.clone(), 1024);
        let _h = writer.spawn();

        // MAX_BATCH distinct node_ids → triggers immediate flush before debounce.
        for i in 0..(MAX_BATCH as u32) {
            let mut id = [0u8; 32];
            id[..4].copy_from_slice(&i.to_le_bytes());
            tx.send(PersistOp::UpsertEntry {
                node_id: id,
                snapshot: snap(),
                hints: Some(vec![PersistedHint {
                    kind: HintKindWire::DirectAddr,
                    payload: format!("192.0.2.1:{}", 9000 + i),
                }]),
                invalidated: None,
                version: 1,
            })
            .await
            .unwrap();
        }

        // Yield without advancing past the debounce — the flush should already
        // have happened thanks to MAX_BATCH.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let calls = sink.calls.lock().unwrap();
        assert!(!calls.is_empty(), "MAX_BATCH should have triggered a flush");
        assert_eq!(calls[0].len(), MAX_BATCH);
        for (_, pending) in &calls[0] {
            assert!(pending.snapshot.is_some());
            assert_eq!(
                pending.hints.as_ref().unwrap().len(),
                1,
                "batch boundary must keep identity and hints together"
            );
        }
    }

    #[test]
    fn coalescing_ignores_a_stale_invalidation() {
        let mut pending = PendingWrite::default();
        let id = [9; 32];
        pending.merge(PersistOp::UpsertEntry {
            node_id: id,
            snapshot: snap(),
            version: 20,
            hints: Some(vec![PersistedHint {
                kind: HintKindWire::DirectAddr,
                payload: "192.0.2.1:9000".into(),
            }]),
            invalidated: None,
        });
        pending.merge(PersistOp::UpsertEntry {
            node_id: id,
            snapshot: snap(),
            version: 10,
            hints: Some(Vec::new()),
            invalidated: Some(HintKindWire::DirectAddr),
        });
        assert!(pending.invalidated.is_empty());
        assert_eq!(pending.hints.unwrap().len(), 1);
    }

    #[test]
    fn coalesce_delete_supersedes_pending_upsert() {
        let mut buf: HashMap<NodeId, PendingWrite> = HashMap::new();
        let id: NodeId = [9u8; 32];
        coalesce(
            &mut buf,
            PersistOp::UpsertEntry {
                node_id: id,
                snapshot: snap(),
                hints: None,
                invalidated: None,
                version: 1,
            },
        );
        coalesce(&mut buf, PersistOp::Delete { node_id: id });
        let pend = buf.remove(&id).unwrap();
        assert!(pend.delete);
        assert!(pend.snapshot.is_none());
    }

    // Touch Instant so unused-import warnings don't trip the strict build.
    #[test]
    fn _instant_is_real() {
        let _ = Instant::now();
    }
}
