// =============================================================================
// File: bus/producer.rs — TentaBus M1: producer idempotency (PLAN §3.1 layer 1)
// =============================================================================
//
// "Sesja producenta ma (producer_id, producer_epoch); każdy batch niesie
// monotoniczny base_seq. Broker trzyma w fjall (producer_seq) ostatni
// przyjęty seq per (topik, partycja, producent) i odrzuca duplikat,
// zwracając oryginalny offset." — one fjall lookup+write PER BATCH (not per
// record). fjall comfortably measures in the hundreds of thousands of
// ops/s for this kind of single-key lookup+write, well inside the layer-1
// budget of a few hundred/s at realistic batch sizes — unlike layer 2
// (`dedup.rs`, a per-RECORD workload that needed a dedicated store
// instead), this stays on fjall.
//
// The key is actually `(org_id, topic, partition, producer_id)`, one
// component more than the PLAN quote above: topics are scoped per org
// (`bus_topics` PK `(org_id, name)`), so two organizations can both have a
// topic named e.g. `orders.created`; without `org_id` in the key, a
// producer_id/seq collision across orgs would report a `Duplicate` with
// the WRONG org's offset (see `key`'s doc for the failure mode this
// avoids).

use fjall::{Database, Keyspace, KeyspaceCreateOptions, PersistMode};
use serde::{Deserialize, Serialize};

use super::codec::{decode, encode};
use super::BusServiceError;

pub const PRODUCER_SEQ_KEYSPACE: &str = "producer_seq";
const SEP: u8 = 0;

/// One producer session's identity, carried by `PublishBatch` (PLAN §6.1).
#[derive(Debug, Clone)]
pub struct ProducerIdentity {
    pub producer_id: String,
    /// Fencing epoch (PLAN §2.3's wire `producer_epoch`, PLAN §4's leader
    /// fencing) — M1 has no leader yet, but a producer that restarts and
    /// bumps its epoch still fences out a zombie instance of itself running
    /// under the old epoch.
    pub epoch: u32,
    /// Monotonic sequence number for this batch, assigned by the producer.
    pub base_seq: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct SeqRecord {
    epoch: u32,
    last_seq: u64,
    offset: u64,
}

pub struct ProducerSeqStore {
    db: Database,
    keyspace: Keyspace,
    /// Test-only failure injection for `record` — see
    /// `force_next_record_failure`'s doc. A plain field (not behind
    /// `#[cfg(test)]`) so `BusService::publish` never needs its own
    /// `cfg(test)` branch to check it; the flag itself is always `false`
    /// outside tests, so the check on `record`'s hot path is one relaxed
    /// atomic load that never fires in production.
    force_next_record_failure: std::sync::atomic::AtomicBool,
}

impl ProducerSeqStore {
    pub fn open(db: &Database) -> Result<Self, BusServiceError> {
        Ok(Self {
            db: db.clone(),
            keyspace: db.keyspace(PRODUCER_SEQ_KEYSPACE, KeyspaceCreateOptions::default)?,
            force_next_record_failure: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// Test-only hook: makes the NEXT `record` call return `Err` instead of
    /// actually recording anything, then resets itself — simulates a
    /// `record` failure AFTER a successful `append_batch` (the fjall write
    /// itself has no reachable failure mode through this store's public
    /// API alone), which is exactly the ordering `BusService::publish`'s
    /// `PartialPublish.acked` needs to stay correct across.
    #[cfg(test)]
    pub(crate) fn force_next_record_failure(&self) {
        self.force_next_record_failure
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// `org_id` is the FIRST key component: topics are
    /// per-org (`bus_topics` PK `(org_id, name)`), so without it two
    /// organizations sharing a topic name AND a `producer_id` would
    /// collide — a producer in org B replaying a `base_seq` already seen
    /// for org A's producer would be told its record is a `Duplicate`
    /// carrying org A's offset, silently dropping org B's data. This is an
    /// on-disk key format change; there is no migration because nothing
    /// has shipped with the old format yet.
    fn key(org_id: &str, topic: &str, partition: u32, producer_id: &str) -> Vec<u8> {
        let mut k = Vec::with_capacity(org_id.len() + topic.len() + producer_id.len() + 7);
        k.extend_from_slice(org_id.as_bytes());
        k.push(SEP);
        k.extend_from_slice(topic.as_bytes());
        k.push(SEP);
        k.extend_from_slice(&partition.to_be_bytes());
        k.push(SEP);
        k.extend_from_slice(producer_id.as_bytes());
        k
    }

    /// Checks `identity` against the last accepted `(epoch, seq)` for this
    /// producer on this (topic, partition):
    /// - a strictly older epoch is `Fenced` (a zombie instance of a
    ///   producer that has since restarted under a newer epoch) — the
    ///   batch must be rejected, never silently accepted or deduped;
    /// - `base_seq` at or behind the last accepted seq of the SAME epoch is
    ///   `Duplicate { original_offset }` — the batch must not be written
    ///   again;
    /// - otherwise `Fresh`: the caller proceeds to append the batch, then
    ///   MUST call `record` with the real offset the engine assigned it
    ///   (`check`/`record` are split because that offset is only known
    ///   after the append — see `record`'s doc).
    ///
    /// LIMITATION (PLAN §3.1 says "ostatni przyjęty seq", singular): only
    /// the single most recent `(seq, offset)` pair is remembered per
    /// producer, not a window of several like Kafka's idempotent producer.
    /// Replaying a seq that has since been superseded by a newer one is
    /// still safely reported as `Duplicate` (no re-append happens), but
    /// `original_offset` reflects the LATEST recorded batch, not
    /// necessarily the exact stale one being replayed. A well-behaved
    /// producer only ever retries its most recent unacknowledged batch, so
    /// this only matters for out-of-order replays, which should not occur
    /// in practice.
    pub fn check(
        &self,
        org_id: &str,
        topic: &str,
        partition: u32,
        identity: &ProducerIdentity,
    ) -> Result<CheckOutcome, BusServiceError> {
        let key = Self::key(org_id, topic, partition, &identity.producer_id);
        let Some(bytes) = self.keyspace.get(&key)? else {
            return Ok(CheckOutcome::Fresh);
        };
        let existing: SeqRecord = decode(bytes.as_ref())?;
        if identity.epoch < existing.epoch {
            return Ok(CheckOutcome::Fenced {
                current_epoch: existing.epoch,
            });
        }
        if identity.epoch == existing.epoch && identity.base_seq <= existing.last_seq {
            return Ok(CheckOutcome::Duplicate {
                original_offset: existing.offset,
            });
        }
        Ok(CheckOutcome::Fresh)
    }

    /// Records the `(epoch, seq) -> offset` mapping after a batch was
    /// actually appended to the engine. Split from `check` because the
    /// offset is only known once `Partition::append_batch` returns.
    pub fn record(
        &self,
        org_id: &str,
        topic: &str,
        partition: u32,
        identity: &ProducerIdentity,
        offset: u64,
    ) -> Result<(), BusServiceError> {
        if self
            .force_next_record_failure
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(BusServiceError::Db(
                "forced test failure (force_next_record_failure)".to_string(),
            ));
        }
        let key = Self::key(org_id, topic, partition, &identity.producer_id);
        let record = SeqRecord {
            epoch: identity.epoch,
            last_seq: identity.base_seq,
            offset,
        };
        self.keyspace.insert(&key, encode(&record)?)?;
        Ok(())
    }

    /// Deletes every producer-sequence key for `(org_id, topic)`, across
    /// every partition/producer — called by `BusService::delete_topic` so a
    /// later `create_topic` of the SAME name starts every producer's
    /// sequence fresh, rather than treating its first batch as a
    /// `Duplicate` of a sequence recorded against the deleted topic's
    /// previous incarnation. `topic` is the SECOND key component (`org SEP
    /// topic SEP partition SEP producer_id`), directly after `org_id`, so
    /// this is a plain prefix scan, unlike `groups::GroupOffsetStore::
    /// purge_topic`'s `splitn` (there, `group` sits between `org` and
    /// `topic`). Returns the number of keys removed.
    pub fn purge_topic(&self, org_id: &str, topic: &str) -> Result<usize, BusServiceError> {
        let mut prefix = org_id.as_bytes().to_vec();
        prefix.push(SEP);
        prefix.extend_from_slice(topic.as_bytes());
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

    /// Deletes every producer-sequence key belonging to `org_id` (GDPR/RODO
    /// org purge, `BusService::purge_org`) — `key`'s first component is
    /// `org_id`, so a single prefix scan covers every topic/partition/
    /// producer this org ever produced under. Returns the number of keys
    /// removed.
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckOutcome {
    Fresh,
    Duplicate { original_offset: u64 },
    Fenced { current_epoch: u32 },
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

    fn identity(producer_id: &str, epoch: u32, seq: u64) -> ProducerIdentity {
        ProducerIdentity {
            producer_id: producer_id.to_string(),
            epoch,
            base_seq: seq,
        }
    }

    #[test]
    fn first_batch_is_fresh_then_retry_is_a_duplicate_returning_original_offset() {
        let (_dir, db) = temp_db();
        let store = ProducerSeqStore::open(&db).unwrap();
        let id = identity("producer-a", 1, 0);

        assert_eq!(
            store.check("org-1", "orders", 0, &id).unwrap(),
            CheckOutcome::Fresh
        );
        store.record("org-1", "orders", 0, &id, 100).unwrap();

        // Retry of the exact same batch (same epoch, same seq): duplicate,
        // must report the ORIGINAL offset, not append again.
        assert_eq!(
            store.check("org-1", "orders", 0, &id).unwrap(),
            CheckOutcome::Duplicate {
                original_offset: 100
            }
        );
    }

    #[test]
    fn monotonic_seq_within_same_epoch_is_fresh() {
        let (_dir, db) = temp_db();
        let store = ProducerSeqStore::open(&db).unwrap();
        let id0 = identity("producer-a", 1, 0);
        store.record("org-1", "orders", 0, &id0, 100).unwrap();

        let id1 = identity("producer-a", 1, 1);
        assert_eq!(
            store.check("org-1", "orders", 0, &id1).unwrap(),
            CheckOutcome::Fresh
        );
        store.record("org-1", "orders", 0, &id1, 101).unwrap();

        // Replaying the now-superseded seq=0 is still safely recognized as
        // "do not append again" (seq <= last_seq) — but the store only
        // remembers the single most recent (seq, offset) pair (PLAN §3.1:
        // "ostatni przyjęty seq"), so the reported offset reflects that
        // latest batch (101), not the exact stale one being replayed. A
        // well-behaved producer only ever retries its most recent unacked
        // batch, so `first_batch_is_fresh_then_retry_is_a_duplicate_*`
        // above covers the case that actually matters; this test only
        // proves the safety property (no re-append) holds even outside it.
        assert_eq!(
            store.check("org-1", "orders", 0, &id0).unwrap(),
            CheckOutcome::Duplicate {
                original_offset: 101
            }
        );
    }

    #[test]
    fn newer_epoch_fences_out_the_older_one() {
        let (_dir, db) = temp_db();
        let store = ProducerSeqStore::open(&db).unwrap();
        let id_epoch2 = identity("producer-a", 2, 5);
        store.record("org-1", "orders", 0, &id_epoch2, 200).unwrap();

        // A batch from the OLD epoch (a zombie producer instance) is fenced,
        // not treated as fresh or as a duplicate.
        let id_epoch1 = identity("producer-a", 1, 6);
        assert_eq!(
            store.check("org-1", "orders", 0, &id_epoch1).unwrap(),
            CheckOutcome::Fenced { current_epoch: 2 }
        );
    }

    #[test]
    fn different_producers_partitions_and_topics_are_independent() {
        let (_dir, db) = temp_db();
        let store = ProducerSeqStore::open(&db).unwrap();
        let a = identity("producer-a", 1, 0);
        let b = identity("producer-b", 1, 0);
        store.record("org-1", "orders", 0, &a, 10).unwrap();
        assert_eq!(
            store.check("org-1", "orders", 0, &b).unwrap(),
            CheckOutcome::Fresh
        );
        assert_eq!(
            store.check("org-1", "orders", 1, &a).unwrap(),
            CheckOutcome::Fresh
        );
        assert_eq!(
            store.check("org-1", "payments", 0, &a).unwrap(),
            CheckOutcome::Fresh
        );
    }

    /// two different organizations sharing the same topic name AND
    /// the same `producer_id`/`base_seq` must never collide — before
    /// `org_id` joined the key, org B's producer here would have been told
    /// its fresh batch was a `Duplicate` carrying org A's offset (100),
    /// silently discarding org B's data.
    #[test]
    fn same_producer_id_and_seq_in_different_orgs_do_not_collide() {
        let (_dir, db) = temp_db();
        let store = ProducerSeqStore::open(&db).unwrap();
        let id = identity("shared-producer-id", 1, 0);

        store.record("org-a", "orders", 0, &id, 100).unwrap();

        // Same topic, partition, producer_id AND base_seq — but a
        // different org: must be Fresh, not a Duplicate of org A's offset.
        assert_eq!(
            store.check("org-b", "orders", 0, &id).unwrap(),
            CheckOutcome::Fresh
        );
        store.record("org-b", "orders", 0, &id, 500).unwrap();

        // Both orgs keep their own independent offset for the identical
        // (topic, partition, producer_id, seq) tuple.
        assert_eq!(
            store.check("org-a", "orders", 0, &id).unwrap(),
            CheckOutcome::Duplicate {
                original_offset: 100
            }
        );
        assert_eq!(
            store.check("org-b", "orders", 0, &id).unwrap(),
            CheckOutcome::Duplicate {
                original_offset: 500
            }
        );
    }

    #[test]
    fn purge_topic_removes_only_that_topics_sequence_keys() {
        let (_dir, db) = temp_db();
        let store = ProducerSeqStore::open(&db).unwrap();
        let id = identity("producer-a", 1, 0);
        store.record("org-1", "orders", 0, &id, 100).unwrap();
        store.record("org-1", "orders", 1, &id, 101).unwrap();
        store.record("org-1", "shipments", 0, &id, 5).unwrap();
        store.record("org-2", "orders", 0, &id, 200).unwrap();

        let deleted = store.purge_topic("org-1", "orders").unwrap();
        assert_eq!(deleted, 2);

        assert_eq!(
            store.check("org-1", "orders", 0, &id).unwrap(),
            CheckOutcome::Fresh,
            "org-1's orders sequence is gone"
        );
        assert_eq!(
            store.check("org-1", "orders", 1, &id).unwrap(),
            CheckOutcome::Fresh
        );
        assert_eq!(
            store.check("org-1", "shipments", 0, &id).unwrap(),
            CheckOutcome::Duplicate { original_offset: 5 },
            "a different topic in the same org must survive"
        );
        assert_eq!(
            store.check("org-2", "orders", 0, &id).unwrap(),
            CheckOutcome::Duplicate {
                original_offset: 200
            },
            "a different org's identically named topic must survive"
        );
    }

    #[test]
    fn purge_org_removes_only_that_orgs_sequence_keys() {
        let (_dir, db) = temp_db();
        let store = ProducerSeqStore::open(&db).unwrap();
        let id = identity("producer-a", 1, 0);
        store.record("org-1", "orders", 0, &id, 100).unwrap();
        store.record("org-2", "orders", 0, &id, 200).unwrap();

        let deleted = store.purge_org("org-1").unwrap();
        assert_eq!(deleted, 1);

        assert_eq!(
            store.check("org-1", "orders", 0, &id).unwrap(),
            CheckOutcome::Fresh,
            "org-1's sequence record is gone"
        );
        assert_eq!(
            store.check("org-2", "orders", 0, &id).unwrap(),
            CheckOutcome::Duplicate {
                original_offset: 200
            },
            "org-2 is untouched"
        );
    }
}
