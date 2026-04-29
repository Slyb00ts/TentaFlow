// === File: peer_registry/mod.rs — sharded peer registry, public API and re-exports ===

pub mod delta;
pub mod entry;
pub mod persistence;
pub mod shard;
pub mod state;

#[cfg(test)]
mod tests;

pub use delta::{ConnectionStateTag, PathKind, PeerDelta, PeerDetail, PeerOutcome, PeerSummary};
pub use entry::{
    ArcStr, GpuInfo, NodeId, NodeInfoSnapshot, PeerContainerInfo, PeerEntry, PeerModelInfo,
    PeerRole, RetryState, TransportHints, TrustState, TrustStateTag,
};
pub use shard::{shard_for, Shard, NUM_SHARDS};
pub use state::{
    backoff, transition, ActivePath, ConnectionState, DialPath, StateTrigger, TransitionResult,
    TransitionSideEffect,
};

use parking_lot::RwLock;
use std::sync::{Arc, OnceLock};
use std::time::Instant;
use tokio::sync::{broadcast, mpsc};

use crate::mesh::peer_registry::persistence::{
    bucketize_30s, try_schedule, HintKindWire, PeerPersistSnapshot, PersistOp, PersistedHint,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintKind {
    DirectAddr,
    RelayUrl,
    Hostname,
}

/// Sharded, in-memory peer registry. Authoritative for transient peer state
/// (connection, hints, last heartbeat). Persistence is wired in PR5 via
/// `set_persistence` — every dirtying mutator schedules a debounced
/// PersistOp on the channel, the writer task batches and commits.
pub struct PeerRegistry {
    shards: Box<[Shard]>,
    bus: broadcast::Sender<PeerDelta>,
    persist_tx: OnceLock<mpsc::Sender<PersistOp>>,
}

impl std::fmt::Debug for PeerRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerRegistry")
            .field("shards", &self.shards.len())
            .field("entries", &self.len())
            .finish()
    }
}

impl PeerRegistry {
    pub fn new(bus_capacity: usize) -> Arc<Self> {
        let mut v = Vec::with_capacity(NUM_SHARDS);
        for _ in 0..NUM_SHARDS {
            v.push(Shard::new());
        }
        let (bus, _rx) = broadcast::channel(bus_capacity.max(1));
        Arc::new(Self {
            shards: v.into_boxed_slice(),
            bus,
            persist_tx: OnceLock::new(),
        })
    }

    /// Install the PersistenceWriter channel exactly once. Called from main /
    /// runtime startup right after `new()`. Every subsequent mutator that
    /// flips `dirty` will fire-and-forget a PersistOp into `tx`.
    pub fn set_persistence(&self, tx: mpsc::Sender<PersistOp>) {
        // Ignore if already set — runtimes that re-init keep the first sender.
        let _ = self.persist_tx.set(tx);
    }

    fn schedule_persist(&self, op: PersistOp) {
        if let Some(tx) = self.persist_tx.get() {
            try_schedule(tx, op);
        }
    }

    /// Build the rkyv-free snapshot used by the persistence writer. Reads the
    /// current entry under a short read lock; callers must already hold an
    /// arc. Returns `None` when `pubkey` is unknown — peer_persisted.pubkey
    /// is NOT NULL, so we skip the UpsertEntry op until pairing/hello fills
    /// it in. Hints are persisted independently via `UpsertHints`.
    fn snapshot_for_persist(g: &PeerEntry) -> Option<PeerPersistSnapshot> {
        let pubkey = g.pubkey.as_ref()?;
        let last_seen_ms = g
            .last_app_heartbeat
            .map(|t| {
                // Map Instant → unix millis: we only have a monotonic clock on
                // last_app_heartbeat, so for persistence we substitute "now"
                // as the bucketed timestamp at write time. The bucket is
                // derived in record_heartbeat where wall time is available.
                let elapsed = Instant::now().saturating_duration_since(t).as_millis() as i64;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                bucketize_30s(now.saturating_sub(elapsed))
            })
            .unwrap_or(0);
        Some(PeerPersistSnapshot {
            pubkey: pubkey.to_vec(),
            trust_state: g.trust.clone(),
            hostname: if g.hostname.is_empty() {
                None
            } else {
                Some((*g.hostname).to_string())
            },
            platform: if g.platform.is_empty() {
                None
            } else {
                Some((*g.platform).to_string())
            },
            role: g.role,
            last_seen_ms,
        })
    }

    fn hints_for_persist(g: &PeerEntry) -> Vec<PersistedHint> {
        let mut out: Vec<PersistedHint> = Vec::new();
        for addr in g.hints.addresses.iter() {
            out.push(PersistedHint {
                kind: HintKindWire::DirectAddr,
                payload: addr.to_string(),
            });
        }
        if let Some(relay) = g.hints.relay_url.as_ref() {
            out.push(PersistedHint {
                kind: HintKindWire::RelayUrl,
                payload: (**relay).to_string(),
            });
        }
        if let Some(host) = g.hints.hostname_dns.as_ref() {
            out.push(PersistedHint {
                kind: HintKindWire::Hostname,
                payload: (**host).to_string(),
            });
        }
        out
    }

    /// Persist a freshly-mutated entry. Versions are derived from the bumping
    /// counter on the entry, monotonically increasing per peer. Skips the
    /// UpsertEntry op when pubkey is missing (snapshot returns None) but
    /// still emits hints since peer_hints has no FK constraint requiring
    /// pubkey on the parent — wait, it does (FK on node_id). The hint
    /// upsert is therefore also gated until at least one UpsertEntry has
    /// landed. We emit hints unconditionally because the writer applies
    /// entries first within a flush; if no entry exists, the hint insert
    /// will FK-fail and be dropped, which is the desired conservative
    /// behavior for not-yet-known pubkeys.
    fn persist_entry(&self, g: &mut PeerEntry, persist_hints: bool) {
        if self.persist_tx.get().is_none() {
            return;
        }
        g.persisted_version = g.persisted_version.saturating_add(1);
        if let Some(snap) = Self::snapshot_for_persist(g) {
            self.schedule_persist(PersistOp::UpsertEntry {
                node_id: g.node_id,
                snapshot: snap,
                version: g.persisted_version,
            });
        } else {
            tracing::debug!(
                node_id = %hex::encode(g.node_id),
                "peer_registry: skipping UpsertEntry — pubkey not yet known"
            );
        }
        if persist_hints {
            let hints = Self::hints_for_persist(g);
            self.schedule_persist(PersistOp::UpsertHints {
                node_id: g.node_id,
                hints,
            });
        }
    }

    fn shard(&self, id: &NodeId) -> &Shard {
        &self.shards[shard_for(id, self.shards.len())]
    }

    fn get_arc(&self, id: &NodeId) -> Option<Arc<RwLock<PeerEntry>>> {
        let s = self.shard(id);
        s.map.read().get(id).cloned()
    }

    fn emit(&self, delta: PeerDelta) {
        // broadcast::send fails only when there are zero receivers — that is
        // fine, we drop the event silently.
        let _ = self.bus.send(delta);
    }

    /// Insert a freshly discovered peer or merge transport hints into an
    /// existing entry. Returns Created on first insert, Changed if the hints
    /// were updated, NoChange if everything was already up to date.
    pub fn upsert_discovered(&self, id: NodeId, hints: TransportHints) -> PeerOutcome {
        let shard = self.shard(&id);
        let now = Instant::now();
        // Fast path: already exists, just merge hints.
        if let Some(arc) = shard.map.read().get(&id).cloned() {
            let mut g = arc.write();
            if g.hints == hints {
                return PeerOutcome::NoChange;
            }
            g.hints = hints;
            g.dirty = true;
            self.persist_entry(&mut g, true);
            let delta = PeerDelta::Discovered { node_id: id };
            drop(g);
            self.emit(delta.clone());
            return PeerOutcome::Changed { delta };
        }
        // Slow path: insert.
        let mut w = shard.map.write();
        if let Some(arc) = w.get(&id).cloned() {
            // Lost the race — fall back to merge.
            drop(w);
            let mut g = arc.write();
            if g.hints == hints {
                return PeerOutcome::NoChange;
            }
            g.hints = hints;
            g.dirty = true;
            self.persist_entry(&mut g, true);
            let delta = PeerDelta::Discovered { node_id: id };
            drop(g);
            self.emit(delta.clone());
            return PeerOutcome::Changed { delta };
        }
        let mut entry = PeerEntry::new_discovered(id, hints, now);
        // First persist: write entry + hints under one logical commit. The
        // UpsertEntry is only emitted once we know the pubkey; hints flow
        // independently and will be reconciled on the next flush after
        // pubkey arrives via set_pubkey.
        if self.persist_tx.get().is_some() {
            entry.persisted_version = entry.persisted_version.saturating_add(1);
            if let Some(snap) = Self::snapshot_for_persist(&entry) {
                self.schedule_persist(PersistOp::UpsertEntry {
                    node_id: id,
                    snapshot: snap,
                    version: entry.persisted_version,
                });
            }
            let hints = Self::hints_for_persist(&entry);
            self.schedule_persist(PersistOp::UpsertHints { node_id: id, hints });
        }
        w.insert(id, Arc::new(RwLock::new(entry)));
        drop(w);
        let delta = PeerDelta::Discovered { node_id: id };
        self.emit(delta.clone());
        PeerOutcome::Created { delta }
    }

    pub fn record_heartbeat(&self, id: &NodeId, at: Instant) -> PeerOutcome {
        let Some(arc) = self.get_arc(id) else {
            return PeerOutcome::NoChange;
        };
        let mut g = arc.write();
        let prev_tag = ConnectionStateTag::from(&g.conn);
        let prev_hb = g.last_app_heartbeat;
        g.last_app_heartbeat = Some(at);
        // Run the pure transition; Heartbeat in Connected is a no-op for
        // state but Degraded -> Connected is real.
        let res = transition(&g.conn, &g.retry, &StateTrigger::Heartbeat { at }, at);
        let mut state_changed = false;
        if let Some(new) = res.new_state {
            g.conn = new;
            state_changed = true;
        }
        let new_tag = ConnectionStateTag::from(&g.conn);

        // Bucket heartbeats to 30s wall-clock windows for persistence — at most
        // one DB write per peer per bucket, keeping the SQLite WAL quiet.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let new_bucket = bucketize_30s(now_ms);
        let prev_bucket = prev_hb.map(|prev| {
            let elapsed = at.saturating_duration_since(prev).as_millis() as i64;
            bucketize_30s(now_ms.saturating_sub(elapsed))
        });
        let bucket_advanced = prev_bucket.map(|b| b != new_bucket).unwrap_or(true);
        if bucket_advanced || state_changed {
            // last_seen_ms is the bucketed wall clock; persist via the writer.
            // We bypass `persist_entry`'s snapshot helper here so we can use
            // the freshly-computed bucket without reading Instant→wall again.
            // Skip the write entirely when pubkey is unknown — see snapshot_for_persist.
            if self.persist_tx.get().is_some() {
                let pubkey_bytes: Option<Vec<u8>> = g.pubkey.as_ref().map(|pk| pk.to_vec());
                if let Some(pubkey_vec) = pubkey_bytes {
                    g.persisted_version = g.persisted_version.saturating_add(1);
                    let snap = PeerPersistSnapshot {
                        pubkey: pubkey_vec,
                        trust_state: g.trust.clone(),
                        hostname: if g.hostname.is_empty() {
                            None
                        } else {
                            Some((*g.hostname).to_string())
                        },
                        platform: if g.platform.is_empty() {
                            None
                        } else {
                            Some((*g.platform).to_string())
                        },
                        role: g.role,
                        last_seen_ms: new_bucket,
                    };
                    self.schedule_persist(PersistOp::UpsertEntry {
                        node_id: g.node_id,
                        snapshot: snap,
                        version: g.persisted_version,
                    });
                }
            }
        }

        let node_id = g.node_id;
        drop(g);

        let hb = PeerDelta::Heartbeat { node_id, at };
        self.emit(hb.clone());
        if state_changed && prev_tag != new_tag {
            self.emit(PeerDelta::StateChanged {
                node_id,
                from: prev_tag,
                to: new_tag,
                at,
            });
        }
        PeerOutcome::Changed { delta: hb }
    }

    pub fn apply_node_info(&self, id: &NodeId, snap: NodeInfoSnapshot) -> PeerOutcome {
        let Some(arc) = self.get_arc(id) else {
            return PeerOutcome::NoChange;
        };
        let mut g = arc.write();
        g.hostname = snap.hostname.clone();
        g.platform = snap.platform.clone();
        g.node_info = Some(Arc::new(snap));
        g.dirty = true;
        self.persist_entry(&mut g, false);
        let node_id = g.node_id;
        drop(g);
        let delta = PeerDelta::NodeInfoUpdated { node_id };
        self.emit(delta.clone());
        PeerOutcome::Changed { delta }
    }

    pub fn transition_state(&self, id: &NodeId, trigger: StateTrigger) -> PeerOutcome {
        let Some(arc) = self.get_arc(id) else {
            return PeerOutcome::NoChange;
        };
        let now = Instant::now();
        let mut g = arc.write();
        let prev_tag = ConnectionStateTag::from(&g.conn);
        let res = transition(&g.conn, &g.retry, &trigger, now);

        if let Some(eff) = res.side_effect.as_ref() {
            match eff {
                TransitionSideEffect::ResetRetry => {
                    g.retry = RetryState::default();
                }
                TransitionSideEffect::BumpRetry { err } => {
                    g.retry.attempts = g.retry.attempts.saturating_add(1);
                    g.retry.last_err = Some(err.clone());
                    g.retry.next_attempt = Some(now);
                }
                TransitionSideEffect::ScheduleDial { at } => {
                    g.retry.next_attempt = Some(*at);
                }
                TransitionSideEffect::InvalidateHint
                | TransitionSideEffect::EmitOnline
                | TransitionSideEffect::EmitOffline
                | TransitionSideEffect::EmitForget => {}
            }
        }

        let mut new_tag = prev_tag;
        if let Some(ns) = res.new_state {
            g.conn = ns;
            new_tag = ConnectionStateTag::from(&g.conn);
            g.last_transport_event = now;
            g.dirty = true;
        }
        let node_id = g.node_id;
        let is_forget = matches!(res.side_effect, Some(TransitionSideEffect::EmitForget));
        drop(g);

        if is_forget {
            self.shard(id).map.write().remove(id);
            let delta = PeerDelta::Forgotten { node_id };
            self.emit(delta.clone());
            return PeerOutcome::Changed { delta };
        }

        if prev_tag != new_tag {
            let delta = PeerDelta::StateChanged {
                node_id,
                from: prev_tag,
                to: new_tag,
                at: now,
            };
            self.emit(delta.clone());
            return PeerOutcome::Changed { delta };
        }
        PeerOutcome::NoChange
    }

    pub fn set_trust(&self, id: &NodeId, trust: TrustState) -> PeerOutcome {
        let Some(arc) = self.get_arc(id) else {
            return PeerOutcome::NoChange;
        };
        let mut g = arc.write();
        if g.trust == trust {
            return PeerOutcome::NoChange;
        }
        g.trust = trust;
        g.dirty = true;
        self.persist_entry(&mut g, false);
        let node_id = g.node_id;
        drop(g);
        let delta = PeerDelta::TrustChanged { node_id };
        self.emit(delta.clone());
        PeerOutcome::Changed { delta }
    }

    pub fn invalidate_hint(&self, id: &NodeId, kind: HintKind) -> PeerOutcome {
        let Some(arc) = self.get_arc(id) else {
            return PeerOutcome::NoChange;
        };
        let mut g = arc.write();
        let before = g.hints.clone();
        match kind {
            HintKind::DirectAddr => g.hints.addresses.clear(),
            HintKind::RelayUrl => g.hints.relay_url = None,
            HintKind::Hostname => g.hints.hostname_dns = None,
        }
        if before == g.hints {
            return PeerOutcome::NoChange;
        }
        g.dirty = true;
        self.persist_entry(&mut g, true);
        let node_id = g.node_id;
        drop(g);
        let delta = PeerDelta::Discovered { node_id };
        self.emit(delta.clone());
        PeerOutcome::Changed { delta }
    }

    pub fn forget(&self, id: &NodeId) -> PeerOutcome {
        let removed = self.shard(id).map.write().remove(id);
        if removed.is_none() {
            return PeerOutcome::NoChange;
        }
        self.schedule_persist(PersistOp::Delete { node_id: *id });
        let delta = PeerDelta::Forgotten { node_id: *id };
        self.emit(delta.clone());
        PeerOutcome::Changed { delta }
    }

    /// Install or replace the long-term identity public key for a peer.
    /// First successful call unlocks the persistence writer (see
    /// `snapshot_for_persist`); subsequent calls only persist when the
    /// bytes actually change. No-op if the entry does not exist — callers
    /// must `upsert_discovered` first.
    pub fn set_pubkey(&self, id: &NodeId, pubkey: Arc<[u8]>) -> PeerOutcome {
        let Some(arc) = self.get_arc(id) else {
            return PeerOutcome::NoChange;
        };
        let mut g = arc.write();
        let unchanged = matches!(g.pubkey.as_ref(), Some(prev) if prev.as_ref() == pubkey.as_ref());
        if unchanged {
            return PeerOutcome::NoChange;
        }
        g.pubkey = Some(pubkey);
        g.dirty = true;
        // Re-emit hints alongside the entry so a freshly-pubkeyed peer gets
        // its first complete UpsertEntry + UpsertHints pair in one flush.
        self.persist_entry(&mut g, true);
        let node_id = g.node_id;
        drop(g);
        let delta = PeerDelta::NodeInfoUpdated { node_id };
        self.emit(delta.clone());
        PeerOutcome::Changed { delta }
    }

    /// Patch hostname on an existing entry. Used by the PR2 shadow when
    /// peer_store learns a hostname out-of-band (Hello payload, mDNS TXT).
    /// No-op if the entry does not exist (use upsert_discovered first).
    pub fn set_hostname(&self, id: &NodeId, hostname: ArcStr) -> PeerOutcome {
        let Some(arc) = self.get_arc(id) else {
            return PeerOutcome::NoChange;
        };
        let mut g = arc.write();
        if g.hostname == hostname {
            return PeerOutcome::NoChange;
        }
        g.hostname = hostname;
        g.dirty = true;
        self.persist_entry(&mut g, false);
        let node_id = g.node_id;
        drop(g);
        let delta = PeerDelta::NodeInfoUpdated { node_id };
        self.emit(delta.clone());
        PeerOutcome::Changed { delta }
    }

    /// Patch platform on an existing entry.
    pub fn set_platform(&self, id: &NodeId, platform: ArcStr) -> PeerOutcome {
        let Some(arc) = self.get_arc(id) else {
            return PeerOutcome::NoChange;
        };
        let mut g = arc.write();
        if g.platform == platform {
            return PeerOutcome::NoChange;
        }
        g.platform = platform;
        g.dirty = true;
        self.persist_entry(&mut g, false);
        let node_id = g.node_id;
        drop(g);
        let delta = PeerDelta::NodeInfoUpdated { node_id };
        self.emit(delta.clone());
        PeerOutcome::Changed { delta }
    }

    /// Replace the model list for an entry. peer_store treats every
    /// ModelsSync as authoritative (full overwrite), the registry mirrors
    /// that semantic.
    pub fn set_models(&self, id: &NodeId, models: Arc<[PeerModelInfo]>) -> PeerOutcome {
        let Some(arc) = self.get_arc(id) else {
            return PeerOutcome::NoChange;
        };
        let mut g = arc.write();
        g.models = models;
        g.dirty = true;
        let node_id = g.node_id;
        drop(g);
        let delta = PeerDelta::NodeInfoUpdated { node_id };
        self.emit(delta.clone());
        PeerOutcome::Changed { delta }
    }

    /// Replace the container list for an entry.
    pub fn set_containers(
        &self,
        id: &NodeId,
        containers: Arc<[PeerContainerInfo]>,
    ) -> PeerOutcome {
        let Some(arc) = self.get_arc(id) else {
            return PeerOutcome::NoChange;
        };
        let mut g = arc.write();
        g.containers = containers;
        g.dirty = true;
        let node_id = g.node_id;
        drop(g);
        let delta = PeerDelta::NodeInfoUpdated { node_id };
        self.emit(delta.clone());
        PeerOutcome::Changed { delta }
    }

    /// Read the current connection conn_id for an entry, if any. Used by the
    /// PR2 shadow to issue a matching TransportClosed when peer_store flips
    /// quic_connected from true to false.
    pub fn current_conn_id(&self, id: &NodeId) -> Option<u64> {
        let arc = self.get_arc(id)?;
        let g = arc.read();
        match &g.conn {
            ConnectionState::Connected { conn_id, .. }
            | ConnectionState::Degraded { conn_id, .. } => Some(*conn_id),
            _ => None,
        }
    }

    /// Whether the entry is currently in Connected state.
    pub fn is_connected(&self, id: &NodeId) -> bool {
        let Some(arc) = self.get_arc(id) else {
            return false;
        };
        let g = arc.read();
        matches!(g.conn, ConnectionState::Connected { .. })
    }

    pub fn snapshot_summary(&self) -> Vec<PeerSummary> {
        let mut out = Vec::new();
        let now = Instant::now();
        for shard in self.shards.iter() {
            let entries: Vec<Arc<RwLock<PeerEntry>>> =
                shard.map.read().values().cloned().collect();
            for arc in entries {
                let g = arc.read();
                out.push(summary_from(&g, now));
            }
        }
        out
    }

    pub fn snapshot_detail(&self, id: &NodeId) -> Option<PeerDetail> {
        let arc = self.get_arc(id)?;
        let g = arc.read();
        let now = Instant::now();
        Some(PeerDetail {
            summary: summary_from(&g, now),
            node_info: g.node_info.clone(),
            models: g.models.clone(),
            containers: g.containers.clone(),
            hints: g.hints.clone(),
            retry: g.retry.clone(),
        })
    }

    pub fn for_each_connected<F: FnMut(&PeerEntry)>(&self, mut f: F) {
        for shard in self.shards.iter() {
            let entries: Vec<Arc<RwLock<PeerEntry>>> =
                shard.map.read().values().cloned().collect();
            for arc in entries {
                let g = arc.read();
                if matches!(g.conn, ConnectionState::Connected { .. }) {
                    f(&g);
                }
            }
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<PeerDelta> {
        self.bus.subscribe()
    }

    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }

    pub fn shard_iter(&self, shard_idx: usize) -> Vec<Arc<RwLock<PeerEntry>>> {
        self.shards
            .get(shard_idx)
            .map(|s| s.map.read().values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.map.read().len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Load every persisted peer (with hints) from the SQLite repository
    /// into the in-memory registry. Returns the number of peers hydrated.
    /// Must be called BEFORE `set_persistence` so the hydrate path does not
    /// echo writes back through the writer.
    pub fn hydrate_from_db(&self, db: &crate::db::DbPool) -> anyhow::Result<usize> {
        let rows = crate::db::repository::load_peer_persisted_all(db)?;
        let hints_by_node = crate::db::repository::load_peer_hints_all(db)?;
        let mut count = 0usize;

        for row in rows {
            let mut hints = TransportHints::default();
            if let Some(node_hints) = hints_by_node.get(&row.node_id) {
                for h in node_hints {
                    match h.hint_kind {
                        v if v == crate::db::repository::HINT_KIND_DIRECT_ADDR => {
                            if let Ok(addr) = h.payload.parse() {
                                hints.addresses.push(addr);
                            }
                        }
                        v if v == crate::db::repository::HINT_KIND_RELAY_URL => {
                            hints.relay_url = Some(Arc::<str>::from(h.payload.as_str()));
                        }
                        v if v == crate::db::repository::HINT_KIND_HOSTNAME => {
                            hints.hostname_dns = Some(Arc::<str>::from(h.payload.as_str()));
                        }
                        _ => {}
                    }
                }
            }

            // Insert directly without going through `upsert_discovered` so we
            // preserve persisted_ver and don't fire persist events back at
            // the writer (which is not installed yet at hydrate time).
            let mut entry =
                PeerEntry::new_discovered(row.node_id, hints, std::time::Instant::now());
            entry.persisted_version = row.persisted_ver.max(0) as u64;
            entry.dirty = false;
            if !row.pubkey.is_empty() {
                entry.pubkey = Some(Arc::<[u8]>::from(row.pubkey.as_slice()));
            }
            entry.trust = match row.trust_state {
                v if v == crate::db::repository::TRUST_TRUSTED => TrustState::Trusted,
                v if v == crate::db::repository::TRUST_PENDING_PAIRING => {
                    // pin_hash is unknown here — pending pairings live in a
                    // separate table; fall back to Discovered for safety.
                    TrustState::Discovered
                }
                _ => TrustState::Discovered,
            };
            if let Some(host) = row.hostname.filter(|s| !s.is_empty()) {
                entry.hostname = Arc::<str>::from(host.as_str());
            }
            if let Some(plat) = row.platform.filter(|s| !s.is_empty()) {
                entry.platform = Arc::<str>::from(plat.as_str());
            }
            entry.role = match row.role {
                v if v == crate::db::repository::ROLE_EDGE => PeerRole::Edge,
                v if v == crate::db::repository::ROLE_RELAY => PeerRole::Relay,
                _ => PeerRole::Node,
            };

            let shard = self.shard(&row.node_id);
            shard
                .map
                .write()
                .insert(row.node_id, Arc::new(RwLock::new(entry)));
            count += 1;
        }
        Ok(count)
    }
}

fn summary_from(g: &PeerEntry, now: Instant) -> PeerSummary {
    let conn_tag = ConnectionStateTag::from(&g.conn);
    let conn_path_kind = match &g.conn {
        ConnectionState::Connected { path, .. } | ConnectionState::Degraded { path, .. } => {
            Some(PathKind::from(path))
        }
        _ => None,
    };
    let since_ms: i64 = match &g.conn {
        ConnectionState::Connecting { since, .. }
        | ConnectionState::Connected { since, .. }
        | ConnectionState::Degraded { since, .. }
        | ConnectionState::Reconnecting { since, .. }
        | ConnectionState::Offline { since } => {
            now.saturating_duration_since(*since).as_millis() as i64
        }
        ConnectionState::Disconnected => 0,
    };
    let last_app_heartbeat_ms = g
        .last_app_heartbeat
        .map(|t| now.saturating_duration_since(t).as_millis() as i64);
    PeerSummary {
        node_id: g.node_id,
        trust: TrustStateTag::from(&g.trust),
        conn_tag,
        conn_path_kind,
        since_ms,
        hostname: g.hostname.clone(),
        platform: g.platform.clone(),
        role: g.role,
        last_app_heartbeat_ms,
    }
}
