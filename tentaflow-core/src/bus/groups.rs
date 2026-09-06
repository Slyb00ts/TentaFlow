// =============================================================================
// File: bus/groups.rs — TentaBus M1: consumer group offsets (fjall)
// =============================================================================
//
// PLAN.md §3.2: offset per (group_id, topic, partition) in a fjall keyspace
// (`offsets`, under `<bus_dir>/_meta` — the same `fjall::Database` producer
// idempotency uses, see `bus/producer.rs`). Delivery-attempt bookkeeping
// (PLAN §3.3) lives here too, keyed per (group, topic, partition, OFFSET) —
// not just per partition — because PLAN §3.3 counts attempts "per (grupa,
// offset)": a batch fetch (up to 1000 records, PLAN K4) can fail on one
// record while others succeed, and a single shared counter would send the
// FIRST failing record to the DLQ using a count already inflated by
// unrelated offsets' failures.
//
// DURABILITY: `commit`/`force_commit` call `Database::persist`
// with an EXPLICIT `PersistMode` rather than relying on `Keyspace::insert`'s
// default (buffered, not fsynced) behavior — an offset commit is the one
// write in this file whose loss actually matters (a lost commit means
// re-delivery, which is only safe because TentaBus is at-least-once, never
// silent loss). This does NOT give the atomicity PLAN §3.2 originally
// described ("zapis w jednej `Batch` fjall razem z aktualizacją dedup —
// atomowość ACK-a i deduplikacji"): dedup lives in its own mmap store
// (`dedup.rs`, plan B — a dedicated fixed-size key store rather than a
// second fjall keyspace, chosen for its per-record throughput), so the
// commit (fjall) and the dedup key (mmap) live in two different files with
// no shared transaction, so a crash between "record processed" and
// "commit persisted" can redeliver
// a record whose dedup key is already marked seen (if the redelivery
// resends the exact same wire bytes/key) or genuinely reprocess it (if the
// caller's dedup key covers something other than raw bytes). Either way the
// system stays at-least-once, never exactly-once — this is a known,
// accepted consequence of plan B, not a regression to fix later.

use fjall::{Database, Keyspace, KeyspaceCreateOptions, PersistMode};
use serde::{Deserialize, Serialize};

use super::codec::{decode, encode};
use super::BusServiceError;

pub const OFFSETS_KEYSPACE: &str = "offsets";
const SEP: u8 = 0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitMode {
    /// Default for `bus_consume`: caller commits after the flow/handler
    /// returns success.
    AutoAfterSuccess,
    /// Caller calls `commit()` explicitly (addons, external integrations).
    Explicit,
    /// Commits before delivery — may lose records on crash between commit
    /// and processing; opt-in only (PLAN §3.2).
    AtMostOnce,
}

impl CommitMode {
    pub fn as_str(self) -> &'static str {
        match self {
            CommitMode::AutoAfterSuccess => "auto_after_success",
            CommitMode::Explicit => "explicit",
            CommitMode::AtMostOnce => "at_most_once",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "auto_after_success" => Some(CommitMode::AutoAfterSuccess),
            "explicit" => Some(CommitMode::Explicit),
            "at_most_once" => Some(CommitMode::AtMostOnce),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct OffsetRecord {
    committed_offset: u64,
    ts_ms: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct AttemptRecord {
    attempts: u32,
    first_failed_at_ms: i64,
    last_failed_at_ms: i64,
}

#[derive(Debug, Clone, Copy)]
pub struct DeliveryAttemptInfo {
    pub attempts: u32,
    pub first_failed_at_ms: i64,
    pub last_failed_at_ms: i64,
}

pub struct GroupOffsetStore {
    db: Database,
    keyspace: Keyspace,
}

impl GroupOffsetStore {
    pub fn open(db: &Database) -> Result<Self, BusServiceError> {
        Ok(Self {
            db: db.clone(),
            keyspace: db.keyspace(OFFSETS_KEYSPACE, KeyspaceCreateOptions::default)?,
        })
    }

    fn key(org_id: &str, group: &str, topic: &str, partition: u32) -> Vec<u8> {
        let mut k = Vec::with_capacity(org_id.len() + group.len() + topic.len() + 10);
        k.extend_from_slice(org_id.as_bytes());
        k.push(SEP);
        k.extend_from_slice(group.as_bytes());
        k.push(SEP);
        k.extend_from_slice(topic.as_bytes());
        k.push(SEP);
        k.extend_from_slice(&partition.to_be_bytes());
        k
    }

    /// One entry per (group, topic, partition, OFFSET) — a strict extension
    /// of `key` (same prefix plus a separator and the big-endian offset), so
    /// a byte-range scan of `attempt_key(from)..attempt_key(to)` visits
    /// exactly the attempt entries for offsets in `[from, to)` without ever
    /// matching the bare commit record itself.
    fn attempt_key(org_id: &str, group: &str, topic: &str, partition: u32, offset: u64) -> Vec<u8> {
        let mut k = Self::key(org_id, group, topic, partition);
        k.push(SEP);
        k.extend_from_slice(&offset.to_be_bytes());
        k
    }

    /// Prefix shared by every attempt key for (group, topic, partition),
    /// regardless of offset — used to wipe attempt history wholesale on an
    /// admin offset reset (`force_commit`), where there is no well-defined
    /// "already consumed" range to bound a precise delete.
    fn attempt_prefix(org_id: &str, group: &str, topic: &str, partition: u32) -> Vec<u8> {
        let mut k = Self::key(org_id, group, topic, partition);
        k.push(SEP);
        k
    }

    fn load(&self, key: &[u8]) -> Result<Option<OffsetRecord>, BusServiceError> {
        match self.keyspace.get(key)? {
            Some(bytes) => Ok(Some(decode(bytes.as_ref())?)),
            None => Ok(None),
        }
    }

    fn load_attempt(&self, key: &[u8]) -> Result<Option<AttemptRecord>, BusServiceError> {
        match self.keyspace.get(key)? {
            Some(bytes) => Ok(Some(decode(bytes.as_ref())?)),
            None => Ok(None),
        }
    }

    /// Committed offset for (group, topic, partition); `0` (earliest) if the
    /// group has never committed here.
    pub fn committed_offset(
        &self,
        org_id: &str,
        group: &str,
        topic: &str,
        partition: u32,
    ) -> Result<u64, BusServiceError> {
        let key = Self::key(org_id, group, topic, partition);
        Ok(self.load(&key)?.map(|r| r.committed_offset).unwrap_or(0))
    }

    /// Lag = high_watermark - committed_offset (PLAN §3.2), no log scan.
    pub fn lag(
        &self,
        org_id: &str,
        group: &str,
        topic: &str,
        partition: u32,
        high_watermark: u64,
    ) -> Result<u64, BusServiceError> {
        let committed = self.committed_offset(org_id, group, topic, partition)?;
        Ok(high_watermark.saturating_sub(committed))
    }

    /// Deletes every attempt entry for offsets in `[from, to)` — the range
    /// that just moved behind the group's committed offset and can never be
    /// redelivered again, so its failure bookkeeping is dead weight.
    /// A best-effort cleanup: offsets in the range with no attempt entry
    /// (the common case — most records never fail) simply are not found by
    /// the range scan and cost nothing extra.
    fn clear_attempts_in_range(
        &self,
        org_id: &str,
        group: &str,
        topic: &str,
        partition: u32,
        from: u64,
        to: u64,
    ) -> Result<(), BusServiceError> {
        if to <= from {
            return Ok(());
        }
        let start = Self::attempt_key(org_id, group, topic, partition, from);
        let end = Self::attempt_key(org_id, group, topic, partition, to);
        let keys: Vec<Vec<u8>> = self
            .keyspace
            .range(start..end)
            .filter_map(|guard| guard.key().ok())
            .map(|k| k.to_vec())
            .collect();
        for k in keys {
            self.keyspace.remove(k)?;
        }
        Ok(())
    }

    fn clear_all_attempts(
        &self,
        org_id: &str,
        group: &str,
        topic: &str,
        partition: u32,
    ) -> Result<(), BusServiceError> {
        let prefix = Self::attempt_prefix(org_id, group, topic, partition);
        let keys: Vec<Vec<u8>> = self
            .keyspace
            .prefix(&prefix)
            .filter_map(|guard| guard.key().ok())
            .map(|k| k.to_vec())
            .collect();
        for k in keys {
            self.keyspace.remove(k)?;
        }
        Ok(())
    }

    /// Advances the committed offset for (group, topic, partition) and
    /// sweeps the attempt entries the advance just left behind.
    ///
    /// Rejects `offset < committed` (defense in depth — the primary
    /// guard is `ConsumerHandle::commit`'s own check against its
    /// subscription set, which runs before this is ever called): a
    /// downward move here would silently redeliver already-acknowledged
    /// records with no audit trail, exactly the access-control gap PLAN
    /// §3.2 reserves for `BusService::reset_offset` (`bus.admin` +
    /// `bus.offset.reset` audit). Legitimate downward resets must go
    /// through `force_commit` instead.
    pub fn commit(
        &self,
        org_id: &str,
        group: &str,
        topic: &str,
        partition: u32,
        offset: u64,
        now_ms: i64,
    ) -> Result<(), BusServiceError> {
        let key = Self::key(org_id, group, topic, partition);
        let previous = self.load(&key)?.map(|r| r.committed_offset).unwrap_or(0);
        if offset < previous {
            return Err(BusServiceError::OffsetRegression {
                topic: topic.to_string(),
                partition,
                requested: offset,
                committed: previous,
            });
        }
        let record = OffsetRecord {
            committed_offset: offset,
            ts_ms: now_ms,
        };
        self.keyspace.insert(&key, encode(&record)?)?;
        // Explicit persist mode: fsync the offset commit rather than
        // trusting `insert`'s default buffered write — see the module doc
        // for why this still is not full ACK/dedup atomicity.
        self.db.persist(PersistMode::SyncData)?;
        self.clear_attempts_in_range(org_id, group, topic, partition, previous, offset)?;
        Ok(())
    }

    /// Unconditionally sets the committed offset, bypassing `commit`'s
    /// monotonicity guard — the only legitimate caller is
    /// `BusService::reset_offset` (`bus.admin` + `bus.offset.reset` audit,
    /// PLAN §3.2). A reset invalidates ALL prior delivery-attempt history
    /// for this (group, topic, partition), forward or backward, so every
    /// attempt entry is cleared rather than just a bounded range.
    pub fn force_commit(
        &self,
        org_id: &str,
        group: &str,
        topic: &str,
        partition: u32,
        offset: u64,
        now_ms: i64,
    ) -> Result<(), BusServiceError> {
        let key = Self::key(org_id, group, topic, partition);
        let record = OffsetRecord {
            committed_offset: offset,
            ts_ms: now_ms,
        };
        self.keyspace.insert(&key, encode(&record)?)?;
        self.db.persist(PersistMode::SyncData)?;
        self.clear_all_attempts(org_id, group, topic, partition)?;
        Ok(())
    }

    /// Records one more failed delivery attempt for the record at
    /// `offset` specifically (PLAN §3.3: "odliczanie jest per (grupa,
    /// offset)") and returns the running count plus the first/last failure
    /// timestamps for the DLQ envelope. Does NOT move `committed_offset` —
    /// that only happens via `commit`/`force_commit`.
    pub fn record_delivery_attempt(
        &self,
        org_id: &str,
        group: &str,
        topic: &str,
        partition: u32,
        offset: u64,
        now_ms: i64,
    ) -> Result<DeliveryAttemptInfo, BusServiceError> {
        let key = Self::attempt_key(org_id, group, topic, partition, offset);
        let mut record = self.load_attempt(&key)?.unwrap_or(AttemptRecord {
            attempts: 0,
            first_failed_at_ms: 0,
            last_failed_at_ms: 0,
        });
        record.attempts += 1;
        if record.first_failed_at_ms == 0 {
            record.first_failed_at_ms = now_ms;
        }
        record.last_failed_at_ms = now_ms;
        self.keyspace.insert(&key, encode(&record)?)?;
        Ok(DeliveryAttemptInfo {
            attempts: record.attempts,
            first_failed_at_ms: record.first_failed_at_ms,
            last_failed_at_ms: record.last_failed_at_ms,
        })
    }

    /// M2 (PLAN-M2 §1e/K-M2-5): absolute set of a delivery-attempt counter,
    /// as opposed to `record_delivery_attempt`'s increment-by-one. The
    /// follower side of `ReplOffsets` applies a leader-computed `attempts`
    /// value directly — it must never go through `record_delivery_attempt`'s
    /// "load current, add one" path, because that would double-count: the
    /// leader has ALREADY incremented and is shipping the resulting total,
    /// not a delta. `first_failed_at_ms` is `None` when the leader has no
    /// failure yet to report (an offset just committed to `attempts == 0`
    /// has never failed) — `Some(ts)` overwrites the local timestamp
    /// unconditionally rather than only-if-unset, again because the leader
    /// is the source of truth this follower must converge to, not merely a
    /// hint. `last_failed_at_ms` is set to the same `first_failed_at_ms`
    /// timestamp when provided (the wire frame carries no separate "last"
    /// field, PLAN-M2 §1b `ReplOffsets`) — a caller that ever needs the true
    /// last-failure time on a follower should read it straight from the
    /// leader's own `DeliveryAttemptInfo` instead of trusting this replica.
    #[allow(clippy::too_many_arguments)]
    pub fn set_delivery_attempts(
        &self,
        org_id: &str,
        group: &str,
        topic: &str,
        partition: u32,
        offset: u64,
        attempts: u32,
        first_failed_at_ms: Option<i64>,
    ) -> Result<(), BusServiceError> {
        let key = Self::attempt_key(org_id, group, topic, partition, offset);
        if attempts == 0 {
            // Nothing to record — and if a stale entry exists from before a
            // truncate/re-election, an incoming `attempts == 0` means the
            // leader considers this offset's failure history gone, so drop
            // it rather than leave a stale non-zero count behind.
            self.keyspace.remove(&key)?;
            return Ok(());
        }
        let ts = first_failed_at_ms.unwrap_or(0);
        let record = AttemptRecord {
            attempts,
            first_failed_at_ms: ts,
            last_failed_at_ms: ts,
        };
        self.keyspace.insert(&key, encode(&record)?)?;
        Ok(())
    }

    /// Deletes every commit and attempt key for `(org_id, topic)` across
    /// EVERY group — called by `BusService::delete_topic` so a later
    /// `create_topic` of the SAME name does not inherit a stale committed
    /// offset (or attempt count) from a group that consumed the deleted
    /// topic's previous incarnation. Unlike `purge_org` (whose key layout
    /// puts `org_id` first, letting a single prefix scan cover everything),
    /// `topic` is the THIRD component of `key` (`org SEP group SEP topic
    /// SEP partition[SEP offset]`) — a group name could coincidentally
    /// share a prefix with a topic name, so this scans every key under the
    /// org and inspects its actual `topic` component via `splitn` rather
    /// than attempting a direct byte-range/prefix scan. Group and topic
    /// names cannot themselves contain `SEP` (both go through
    /// `topics::validate_user_topic_name`'s charset, which excludes NUL),
    /// so `splitn(3, ..)` on the bytes after the org prefix reliably yields
    /// `[group, topic, rest]`. Returns the number of keys removed.
    pub fn purge_topic(&self, org_id: &str, topic: &str) -> Result<usize, BusServiceError> {
        let mut org_prefix = org_id.as_bytes().to_vec();
        org_prefix.push(SEP);
        let topic_bytes = topic.as_bytes();
        let keys: Vec<Vec<u8>> = self
            .keyspace
            .prefix(&org_prefix)
            .filter_map(|guard| guard.key().ok())
            .filter(|k| {
                let rest = &k[org_prefix.len()..];
                let mut parts = rest.splitn(3, |&b| b == SEP);
                let _group = parts.next();
                matches!(parts.next(), Some(t) if t == topic_bytes)
            })
            .map(|k| k.to_vec())
            .collect();
        let n = keys.len();
        for k in keys {
            self.keyspace.remove(k)?;
        }
        if n > 0 {
            self.db.persist(PersistMode::SyncData)?;
        }
        Ok(n)
    }

    /// Deletes every key belonging to `org_id` — both commit records
    /// (`key`) and delivery-attempt counters (`attempt_key`) alike, since
    /// `attempt_key` is a strict extension of `key`'s own prefix and both
    /// start with `org_id` followed by `SEP` (GDPR/RODO org purge,
    /// `BusService::purge_org`). Returns the number of keys removed.
    pub fn purge_org(&self, org_id: &str) -> Result<usize, BusServiceError> {
        let mut prefix = org_id.as_bytes().to_vec();
        prefix.push(SEP);
        let keys: Vec<Vec<u8>> = self
            .keyspace
            .prefix(&prefix)
            .filter_map(|guard| guard.key().ok())
            .map(|k| k.to_vec())
            .collect();
        let n = keys.len();
        for k in keys {
            self.keyspace.remove(k)?;
        }
        if n > 0 {
            self.db.persist(PersistMode::SyncData)?;
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Opens a fresh fjall `Database` in its own temp directory, which is
    /// removed when the returned `TempDir` is dropped. Callers must keep
    /// the `TempDir` alive for as long as they use the `Database`.
    fn temp_db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db = Database::builder(dir.path()).open().expect("open fjall db");
        (dir, db)
    }

    #[test]
    fn committed_offset_defaults_to_zero_then_reflects_commit() {
        let (_dir, db) = temp_db();
        let store = GroupOffsetStore::open(&db).unwrap();
        assert_eq!(
            store.committed_offset("org-1", "g1", "orders", 0).unwrap(),
            0
        );
        store.commit("org-1", "g1", "orders", 0, 42, 1_000).unwrap();
        assert_eq!(
            store.committed_offset("org-1", "g1", "orders", 0).unwrap(),
            42
        );
    }

    #[test]
    fn lag_is_high_watermark_minus_committed() {
        let (_dir, db) = temp_db();
        let store = GroupOffsetStore::open(&db).unwrap();
        store.commit("org-1", "g1", "orders", 0, 10, 1_000).unwrap();
        assert_eq!(store.lag("org-1", "g1", "orders", 0, 25).unwrap(), 15);
        assert_eq!(store.lag("org-1", "g1", "orders", 0, 10).unwrap(), 0);
    }

    #[test]
    fn commit_rejects_offset_regression() {
        let (_dir, db) = temp_db();
        let store = GroupOffsetStore::open(&db).unwrap();
        store.commit("org-1", "g1", "orders", 0, 10, 1_000).unwrap();
        let err = store
            .commit("org-1", "g1", "orders", 0, 5, 1_100)
            .unwrap_err();
        assert!(matches!(err, BusServiceError::OffsetRegression { .. }));
        // The rejected regression must not have moved the stored offset.
        assert_eq!(
            store.committed_offset("org-1", "g1", "orders", 0).unwrap(),
            10
        );
    }

    #[test]
    fn force_commit_allows_downward_move_and_clears_attempts() {
        let (_dir, db) = temp_db();
        let store = GroupOffsetStore::open(&db).unwrap();
        store.commit("org-1", "g1", "orders", 0, 10, 1_000).unwrap();
        store
            .record_delivery_attempt("org-1", "g1", "orders", 0, 3, 1_000)
            .unwrap();
        store
            .force_commit("org-1", "g1", "orders", 0, 3, 1_100)
            .unwrap();
        assert_eq!(
            store.committed_offset("org-1", "g1", "orders", 0).unwrap(),
            3
        );
        let a = store
            .record_delivery_attempt("org-1", "g1", "orders", 0, 3, 1_200)
            .unwrap();
        assert_eq!(a.attempts, 1, "reset wiped the earlier attempt entry");
    }

    #[test]
    fn delivery_attempts_are_per_offset_not_per_partition() {
        let (_dir, db) = temp_db();
        let store = GroupOffsetStore::open(&db).unwrap();
        let a1 = store
            .record_delivery_attempt("org-1", "g1", "orders", 0, 5, 1_000)
            .unwrap();
        assert_eq!(a1.attempts, 1);
        assert_eq!(a1.first_failed_at_ms, 1_000);

        let a2 = store
            .record_delivery_attempt("org-1", "g1", "orders", 0, 5, 1_500)
            .unwrap();
        assert_eq!(a2.attempts, 2);
        assert_eq!(
            a2.first_failed_at_ms, 1_000,
            "first failure timestamp is sticky"
        );
        assert_eq!(a2.last_failed_at_ms, 1_500);

        // A DIFFERENT offset on the same partition has its own independent
        // counter — this is the point: attempts must not sum across
        // unrelated records in the same batch.
        let other = store
            .record_delivery_attempt("org-1", "g1", "orders", 0, 6, 1_600)
            .unwrap();
        assert_eq!(
            other.attempts, 1,
            "offset 6 must not inherit offset 5's count"
        );

        // Committing past offset 5 clears its attempt entry but must not
        // touch offset 6's, which is still ahead of the new committed
        // offset.
        store.commit("org-1", "g1", "orders", 0, 6, 2_000).unwrap();
        let a3 = store
            .record_delivery_attempt("org-1", "g1", "orders", 0, 5, 2_500)
            .unwrap();
        assert_eq!(a3.attempts, 1, "attempts for offset 5 reset after commit");
        let other2 = store
            .record_delivery_attempt("org-1", "g1", "orders", 0, 6, 2_600)
            .unwrap();
        assert_eq!(
            other2.attempts, 2,
            "offset 6's own count must survive a commit that only passed offset 5"
        );
    }

    #[test]
    fn set_delivery_attempts_is_an_absolute_set_not_an_increment() {
        let (_dir, db) = temp_db();
        let store = GroupOffsetStore::open(&db).unwrap();

        // Sets a fresh entry outright.
        store
            .set_delivery_attempts("org-1", "g1", "orders", 0, 7, 3, Some(1_000))
            .unwrap();
        let a = store
            .record_delivery_attempt("org-1", "g1", "orders", 0, 7, 1_500)
            .unwrap();
        assert_eq!(a.attempts, 4, "increment continues from the set value");
        assert_eq!(a.first_failed_at_ms, 1_000, "first-failed ts preserved");

        // Overwrites an existing entry with a leader-supplied absolute
        // value rather than adding to it.
        store
            .set_delivery_attempts("org-1", "g1", "orders", 0, 7, 9, Some(2_000))
            .unwrap();
        let a2 = store
            .record_delivery_attempt("org-1", "g1", "orders", 0, 7, 2_500)
            .unwrap();
        assert_eq!(a2.attempts, 10, "overwrote, did not add to, the old 4");
        assert_eq!(
            a2.first_failed_at_ms, 2_000,
            "ts overwritten unconditionally"
        );

        // A different offset on the same partition is untouched.
        assert_eq!(
            store
                .record_delivery_attempt("org-1", "g1", "orders", 0, 8, 1_000)
                .unwrap()
                .attempts,
            1
        );

        // attempts == 0 clears any existing entry rather than storing a
        // zero count.
        store
            .set_delivery_attempts("org-1", "g1", "orders", 0, 7, 0, None)
            .unwrap();
        let a3 = store
            .record_delivery_attempt("org-1", "g1", "orders", 0, 7, 3_000)
            .unwrap();
        assert_eq!(a3.attempts, 1, "cleared entry starts counting from zero");
    }

    #[test]
    fn groups_and_partitions_are_independent() {
        let (_dir, db) = temp_db();
        let store = GroupOffsetStore::open(&db).unwrap();
        store.commit("org-1", "g1", "orders", 0, 5, 1_000).unwrap();
        store.commit("org-1", "g2", "orders", 0, 9, 1_000).unwrap();
        store.commit("org-1", "g1", "orders", 1, 3, 1_000).unwrap();
        assert_eq!(
            store.committed_offset("org-1", "g1", "orders", 0).unwrap(),
            5
        );
        assert_eq!(
            store.committed_offset("org-1", "g2", "orders", 0).unwrap(),
            9
        );
        assert_eq!(
            store.committed_offset("org-1", "g1", "orders", 1).unwrap(),
            3
        );
    }

    #[test]
    fn purge_org_removes_only_that_orgs_commit_and_attempt_keys() {
        let (_dir, db) = temp_db();
        let store = GroupOffsetStore::open(&db).unwrap();
        store.commit("org-1", "g1", "orders", 0, 10, 1_000).unwrap();
        store
            .record_delivery_attempt("org-1", "g1", "orders", 0, 12, 1_000)
            .unwrap();
        store.commit("org-2", "g1", "orders", 0, 5, 1_000).unwrap();

        let deleted = store.purge_org("org-1").unwrap();
        assert!(deleted >= 2, "commit + attempt key both removed");

        assert_eq!(
            store.committed_offset("org-1", "g1", "orders", 0).unwrap(),
            0,
            "org-1's commit is gone"
        );
        let a = store
            .record_delivery_attempt("org-1", "g1", "orders", 0, 12, 2_000)
            .unwrap();
        assert_eq!(a.attempts, 1, "org-1's attempt history is gone");

        // org-2 is completely untouched.
        assert_eq!(
            store.committed_offset("org-2", "g1", "orders", 0).unwrap(),
            5
        );
    }

    #[test]
    fn purge_topic_removes_every_groups_offset_and_attempt_for_that_topic_only() {
        let (_dir, db) = temp_db();
        let store = GroupOffsetStore::open(&db).unwrap();
        store
            .commit("org-1", "g1", "orders", 0, 500, 1_000)
            .unwrap();
        store
            .record_delivery_attempt("org-1", "g1", "orders", 0, 3, 1_000)
            .unwrap();
        // A second, independent group on the same topic must also be wiped.
        store
            .commit("org-1", "g2", "orders", 0, 200, 1_000)
            .unwrap();
        // A different topic in the same org must survive.
        store
            .commit("org-1", "g1", "shipments", 0, 77, 1_000)
            .unwrap();
        // A different org's identically named topic must survive.
        store.commit("org-2", "g1", "orders", 0, 42, 1_000).unwrap();

        let deleted = store.purge_topic("org-1", "orders").unwrap();
        assert!(deleted >= 3, "commit x2 + attempt for the purged topic");

        assert_eq!(
            store.committed_offset("org-1", "g1", "orders", 0).unwrap(),
            0
        );
        assert_eq!(
            store.committed_offset("org-1", "g2", "orders", 0).unwrap(),
            0
        );
        let a = store
            .record_delivery_attempt("org-1", "g1", "orders", 0, 3, 2_000)
            .unwrap();
        assert_eq!(a.attempts, 1, "orders' attempt history is gone");
        // The attempt entry just recorded above is itself under "orders" —
        // purge it again so the no-op assertion below reflects a genuinely
        // empty topic, not leftover state this test itself just created.
        store.purge_topic("org-1", "orders").unwrap();

        assert_eq!(
            store
                .committed_offset("org-1", "g1", "shipments", 0)
                .unwrap(),
            77,
            "a different topic in the same org must survive"
        );
        assert_eq!(
            store.committed_offset("org-2", "g1", "orders", 0).unwrap(),
            42,
            "a different org's identically named topic must survive"
        );

        assert_eq!(
            store.purge_topic("org-1", "orders").unwrap(),
            0,
            "purging an already-empty topic is a no-op, not an error"
        );
    }
}
