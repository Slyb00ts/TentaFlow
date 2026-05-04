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
/// carried separately so the writer can replace them atomically per node.
#[derive(Debug, Clone)]
pub struct PeerPersistSnapshot {
    pub pubkey: Vec<u8>,
    pub trust_state: TrustState,
    pub hostname: Option<String>,
    pub platform: Option<String>,
    pub role: PeerRole,
    pub last_seen_ms: i64,
}

/// Hint payload for the writer. The writer rewrites the entire hint set per
/// node atomically (delete + insert in one tx); the registry must therefore
/// always send the full current set, not deltas.
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
    },
    UpsertHints {
        node_id: NodeId,
        hints: Vec<PersistedHint>,
    },
    Delete {
        node_id: NodeId,
    },
}

/// Coalesced pending state per node — the writer collapses N requests for the
/// same node into a single transaction. Newest snapshot wins; hints are
/// replace-style (latest set is the truth).
#[derive(Debug, Default)]
struct PendingWrite {
    snapshot: Option<(PeerPersistSnapshot, u64)>,
    hints: Option<Vec<PersistedHint>>,
    delete: bool,
}

impl PendingWrite {
    fn merge(&mut self, op: PersistOp) {
        match op {
            PersistOp::UpsertEntry {
                snapshot, version, ..
            } => {
                // Latest version wins; out-of-order writes are also rejected
                // by the SQL ON CONFLICT WHERE clause.
                let is_newer = self
                    .snapshot
                    .as_ref()
                    .map(|(_, v)| version >= *v)
                    .unwrap_or(true);
                if is_newer {
                    self.snapshot = Some((snapshot, version));
                }
                self.delete = false;
            }
            PersistOp::UpsertHints { hints, .. } => {
                self.hints = Some(hints);
                self.delete = false;
            }
            PersistOp::Delete { .. } => {
                // A pending delete supersedes pending upserts — drop them.
                self.snapshot = None;
                self.hints = None;
                self.delete = true;
            }
        }
    }
}

fn node_of(op: &PersistOp) -> NodeId {
    match op {
        PersistOp::UpsertEntry { node_id, .. }
        | PersistOp::UpsertHints { node_id, .. }
        | PersistOp::Delete { node_id } => *node_id,
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
    pub delete: bool,
}

impl From<PendingWrite> for PendingWriteSnapshot {
    fn from(p: PendingWrite) -> Self {
        Self {
            snapshot: p.snapshot,
            hints: p.hints,
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
        let mut hint_writes: Vec<(NodeId, Vec<PeerHintRow>)> = Vec::new();
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
                hint_writes.push((*node_id, rows));
            }
        }

        // Single-writer SQLite mutex serializes these; logical ordering is
        // entries first (FK target), then hints, then deletes.
        if !entry_rows.is_empty() {
            repository::upsert_peer_persisted_batch(&self.pool, &entry_rows)?;
        }
        for (node_id, rows) in &hint_writes {
            repository::replace_peer_hints(&self.pool, node_id, rows)?;
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
