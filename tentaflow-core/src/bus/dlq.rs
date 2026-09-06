// =============================================================================
// File: bus/dlq.rs — TentaBus M1: dead-letter queue as a topic (PLAN §3.3)
// =============================================================================
//
// DLQ is a topic (`__dlq.<topic>`), not a separate table (SPEC correction
// K5): same engine, same retention, same UI. This module builds the error
// envelope and the retry backoff schedule; the actual "is this record's
// delivery attempt count over the limit" decision and the counter itself
// live in `groups.rs` (`record_delivery_attempt`), because that counter is
// keyed identically to the commit record and must reset atomically with a
// successful commit.
//
// SUM/tentabus/PLAN-F3.md §4.4: `build_publish_violation_record` is the
// PUBLISH-time counterpart to `build_dlq_record` — a `validation = dlq`
// topic quarantines a record that failed schema validation the moment
// `BusService::publish` sees it, before it was ever appended anywhere, so
// there is no `source_partition`/`source_offset`/`group_id` to carry (those
// three headers are CONSUME-time-failure-specific and are deliberately
// absent here). KNOWN LIMITATION (R9): `dlq_retry` (`bus/mod.rs`)
// republishes a DLQ record's payload verbatim to its source topic — if that
// payload still fails the topic's bound schema, it lands right back in the
// DLQ. This is one hop under explicit admin action (an operator chose to
// retry), not a loop this module guards against; PLAN-F3 treats it as an
// accepted limitation, same class as `dlq.rs`'s pre-existing consume-side
// retry limitation this doc comment sits next to. Concretely, for a
// PUBLISH-time schema violation specifically: `dlq_retry`'s republish goes
// through the normal `BusService::publish` schema-validation block again,
// which quarantines it AGAIN via `build_publish_violation_record` — the
// ORIGINAL DLQ record is never removed or marked handled by this path (only
// `dlq_discard` does that), so the source topic's DLQ ends up holding TWO
// records for what is, from an operator's point of view, one violation: the
// original and this new one. `dlq_retry` itself reports `accepted: 0` for
// this case (the republish never lands on the SOURCE topic — it is diverted
// to the DLQ again before ever reaching it), which is the caller's only
// signal that the retry did not actually clear the violation.

use bytes::Bytes;
use fjall::{Database, Keyspace, KeyspaceCreateOptions, PersistMode};
use serde::{Deserialize, Serialize};

use super::codec::encode;
use super::topics::{TopicConfig, TopicOptions};
use super::{BusCallContext, BusServiceError, PublishRecord};

pub const DLQ_TOPIC_PREFIX: &str = "__dlq.";

/// `__dlq.<source_topic>`.
///
/// INVARIANT: the result must always pass
/// `topics::validate_internal_topic_name`, i.e. never exceed
/// `topics::MAX_TOPIC_NAME_LEN` bytes. This holds only because
/// `topics::validate_user_topic_name` independently enforces
/// `source_topic.len() + DLQ_TOPIC_PREFIX.len() <= MAX_TOPIC_NAME_LEN` at
/// topic-creation time — every `source_topic` reaching this function has
/// already gone through that check. This function does not re-check the
/// length itself: it has no way to reject a name at the point
/// `note_delivery_failure` calls it (attempts are already exhausted), so
/// the guarantee has to come from the source topic never having been
/// creatable with a name too long for this prefix in the first place.
pub fn dlq_topic_name(source_topic: &str) -> String {
    format!("{DLQ_TOPIC_PREFIX}{source_topic}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DlqReason {
    SchemaViolation,
    ConsumerError,
    ConsumerTimeout,
    PermissionDenied,
    PayloadTooLarge,
    BlobMissing,
}

impl DlqReason {
    pub fn as_str(self) -> &'static str {
        match self {
            DlqReason::SchemaViolation => "schema_violation",
            DlqReason::ConsumerError => "consumer_error",
            DlqReason::ConsumerTimeout => "consumer_timeout",
            DlqReason::PermissionDenied => "permission_denied",
            DlqReason::PayloadTooLarge => "payload_too_large",
            DlqReason::BlobMissing => "blob_missing",
        }
    }
}

/// Longest `dlq.error_message` header value kept, IN BYTES (PLAN §3.3:
/// "przycięty do 4 KiB"): the previous `.chars().take(4096)` counted
/// Unicode scalar values, not bytes, so a message made entirely of
/// multi-byte UTF-8 characters could reach up to 16 KiB on the wire, 4x the
/// documented cap. Redaction itself is `events/store.rs` territory (a
/// different subsystem's PII-scrubbing pattern) — out of this file's
/// scope; callers are expected to already have passed the message through
/// that redaction before it reaches here.
pub const MAX_ERROR_MESSAGE_BYTES: usize = 4096;

/// Truncates `s` to at most `max_bytes` BYTES, backing off to the nearest
/// preceding UTF-8 character boundary so the result is always valid `str`
/// (never splits a multi-byte codepoint in half).
fn truncate_to_byte_budget(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// What a delivery-failure call (`bus::note_delivery_failure`) reports back
/// to the caller so it knows whether to keep retrying or stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DlqOutcome {
    /// Still under `max_delivery_attempts` — caller should back off and
    /// retry the same offset.
    Retry { attempts: u32, backoff_ms: u32 },
    /// Attempts exhausted: a copy was published to the DLQ topic and the
    /// group's committed offset was advanced past the poison record.
    SentToDlq { attempts: u32 },
    /// Attempts exhausted and a DLQ copy of THIS record was published, but
    /// the group's committed offset was NOT advanced because
    /// `offset` was not the group's current committed offset —
    /// `committed_offset` reports what it actually is. Advancing anyway
    /// would silently skip every record between the true committed offset
    /// and this one without ever giving any of them a DLQ entry. The
    /// caller must resolve the earlier offset(s) first (or `reset_offset`
    /// via `bus.admin`, audited) before this one's advance can happen.
    SentToDlqOffsetMismatch {
        attempts: u32,
        committed_offset: u64,
    },
}

/// PLAN §3.3: `retry_backoff_ms` (default 1000) x2 per attempt, capped at
/// `cap_ms` (default 60000), +/-20% jitter. Jitter is a deterministic
/// function of `jitter_seed` (caller supplies e.g. a wall-clock-derived
/// value) rather than a global RNG, so this stays a pure function and is
/// exactly reproducible in tests.
pub fn compute_backoff_ms(attempts: u32, base_ms: u32, cap_ms: u32, jitter_seed: u64) -> u32 {
    // `attempts` is 1-based (the first failure reports `attempts ==
    // 1`, see `groups.rs::record_delivery_attempt`), but the first retry
    // must back off by `base_ms` (2^0 * base), not `2 * base_ms` — PLAN
    // §3.3 says the schedule STARTS at `base_ms`. `saturating_sub(1)`
    // turns attempt 1 into shift 0, attempt 2 into shift 1, and so on.
    let shift = attempts.saturating_sub(1).min(16); // 2^16 * base already dwarfs any realistic cap
    let exp = (base_ms as u64).saturating_mul(1u64 << shift);
    let capped = exp.min(cap_ms as u64) as i64;

    // xorshift64 for a cheap, deterministic, well-mixed pseudo-random value.
    let mut x = jitter_seed ^ 0x9E37_79B9_7F4A_7C15;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    // Map to a per-mille offset in [-200, 200] (i.e. +/-20.0%).
    let per_mille = (x % 401) as i64 - 200;
    let delta = capped * per_mille / 1000;
    (capped + delta).clamp(0, cap_ms as i64) as u32
}

/// Builds the DLQ copy of a failed record: same key/payload, the union of
/// its original headers plus the `dlq.*` error envelope (PLAN §3.3).
///
/// strips any `tf.*`-prefixed header from `original` itself, rather
/// than trusting `publish` to strip it again later — `dlq_retry` copies a
/// PREVIOUSLY PUBLISHED record's headers (`tf.*` included) straight into
/// this function's `original` before `publish` ever sees them again, so
/// this module needs to be self-consistent about the same provenance
/// boundary `bus/mod.rs`'s `RESERVED_HEADER_PREFIX` enforces, not rely on
/// the caller re-deriving it correctly every time.
#[allow(clippy::too_many_arguments)]
pub fn build_dlq_record(
    source_topic: &str,
    source_partition: u32,
    source_offset: u64,
    group_id: &str,
    attempts: u32,
    first_failed_at_ms: i64,
    last_failed_at_ms: i64,
    reason: DlqReason,
    error_message: &str,
    original: &PublishRecord,
) -> PublishRecord {
    let truncated = truncate_to_byte_budget(error_message, MAX_ERROR_MESSAGE_BYTES);
    let mut headers: Vec<(String, Bytes)> = original
        .headers
        .iter()
        .filter(|(k, _)| !k.starts_with("tf."))
        .cloned()
        .collect();
    headers.push((
        "dlq.source_topic".to_string(),
        Bytes::from(source_topic.to_string()),
    ));
    headers.push((
        "dlq.source_partition".to_string(),
        Bytes::from(source_partition.to_string()),
    ));
    headers.push((
        "dlq.source_offset".to_string(),
        Bytes::from(source_offset.to_string()),
    ));
    headers.push((
        "dlq.group_id".to_string(),
        Bytes::from(group_id.to_string()),
    ));
    headers.push((
        "dlq.attempts".to_string(),
        Bytes::from(attempts.to_string()),
    ));
    headers.push((
        "dlq.first_failed_at_ms".to_string(),
        Bytes::from(first_failed_at_ms.to_string()),
    ));
    headers.push((
        "dlq.last_failed_at_ms".to_string(),
        Bytes::from(last_failed_at_ms.to_string()),
    ));
    headers.push(("dlq.reason".to_string(), Bytes::from(reason.as_str())));
    headers.push(("dlq.error_message".to_string(), Bytes::from(truncated)));

    PublishRecord {
        key: original.key.clone(),
        headers,
        payload: original.payload.clone(),
        timestamp_ms: original.timestamp_ms,
        schema_id: original.schema_id,
    }
}

/// PLAN-F3 §4.4: publish-time schema-violation quarantine envelope for
/// `validation = dlq`. Unlike `build_dlq_record` (a CONSUME-time delivery
/// failure, with a real source offset/partition/group to record), this
/// record never made it into the log at all — there is no
/// `dlq.source_partition`/`dlq.source_offset`/`dlq.group_id` to stamp, only
/// `dlq.source_topic` (which topic it was rejected from), `dlq.reason`
/// (e.g. `"schema_violation"`), the truncated `dlq.error_message`, and
/// `dlq.rejected_at_ms`. Same `tf.*` header-stripping rationale as
/// `build_dlq_record`'s doc: `original` may itself be a re-threaded record
/// (a caller that copies a previously seen record's headers into a fresh
/// `PublishRecord` before calling `publish` again), so this module stays
/// self-consistent about the same provenance boundary
/// `bus/mod.rs::RESERVED_HEADER_PREFIX` enforces rather than trusting
/// `publish` to strip it a second time.
///
/// Review finding #5: `publish`'s quarantine write runs under a broker
/// `SYSTEM_ACTOR` context (`bus/mod.rs`'s `quarantine_ctx`, needed so a
/// write-only producer's own permissions do not gate the nested DLQ write)
/// — without `producer_ctx`, the resulting DLQ record carried no trace of
/// WHO actually produced the rejected record at all, only that the broker
/// itself wrote the quarantine copy. `producer_ctx` is the ORIGINAL
/// producer's own context (`publish`'s `ctx`, not `quarantine_ctx`), so
/// `dlq.producer_actor`/`dlq.correlation_id` restore that attribution for
/// an operator triaging the DLQ. Both are best-effort: a caller that never
/// set `actor`/`correlation_id` on its `BusCallContext` simply gets no
/// header for that field, same as a legacy caller of `build_dlq_record`.
pub fn build_publish_violation_record(
    source_topic: &str,
    reason: &str,
    error_message: &str,
    producer_ctx: &BusCallContext,
    original: &PublishRecord,
) -> PublishRecord {
    let truncated = truncate_to_byte_budget(error_message, MAX_ERROR_MESSAGE_BYTES);
    let mut headers: Vec<(String, Bytes)> = original
        .headers
        .iter()
        .filter(|(k, _)| !k.starts_with("tf."))
        .cloned()
        .collect();
    headers.push((
        "dlq.source_topic".to_string(),
        Bytes::from(source_topic.to_string()),
    ));
    headers.push(("dlq.reason".to_string(), Bytes::from(reason.to_string())));
    headers.push(("dlq.error_message".to_string(), Bytes::from(truncated)));
    headers.push((
        "dlq.rejected_at_ms".to_string(),
        Bytes::from(super::now_ms().to_string()),
    ));
    if let Some(actor) = producer_ctx.actor.as_deref() {
        headers.push((
            "dlq.producer_actor".to_string(),
            Bytes::from(actor.to_string()),
        ));
    }
    if let Some(correlation_id) = producer_ctx.correlation_id.as_deref() {
        headers.push((
            "dlq.correlation_id".to_string(),
            Bytes::from(correlation_id.to_string()),
        ));
    }

    PublishRecord {
        key: original.key.clone(),
        headers,
        payload: original.payload.clone(),
        timestamp_ms: original.timestamp_ms,
        schema_id: original.schema_id,
    }
}

/// PLAN §3.3 retry action: republish to the source topic with
/// `dlq.retry_of` set to the DLQ record's own coordinates, attempts reset —
/// a fresh delivery attempt against the ORIGINAL topic, not the DLQ.
pub fn build_retry_record(
    dlq_record: &PublishRecord,
    dlq_topic: &str,
    dlq_offset: u64,
) -> PublishRecord {
    let mut headers: Vec<(String, Bytes)> = dlq_record
        .headers
        .iter()
        .filter(|(k, _)| !k.starts_with("dlq."))
        .cloned()
        .collect();
    headers.push((
        "dlq.retry_of".to_string(),
        Bytes::from(format!("{dlq_topic}#{dlq_offset}")),
    ));
    PublishRecord {
        key: dlq_record.key.clone(),
        headers,
        payload: dlq_record.payload.clone(),
        timestamp_ms: dlq_record.timestamp_ms,
        schema_id: dlq_record.schema_id,
    }
}

/// Config a `__dlq.<topic>` gets when auto-created (PLAN §3.3: "RF i
/// środowisko dziedziczone, retencja domyślnie 30 dni, ACL dziedziczone").
/// ACL inheritance is an RBAC-layer concern (`resource_permissions`, tor D);
/// this function only carries the engine-level config forward.
///
/// Durability is the one setting deliberately NOT inherited (owner decision
/// B): every DLQ topic is pinned to `DurabilityClass::Standard`
/// (class-derived, NOT an explicit override — v143,
/// `SUM/tentabus/KRYTYK-M1-R5.md` R5-1/R5-7: this must round-trip through
/// `durability_explicit() == false` like any other class-driven topic, not
/// look like an admin manually picked a policy) regardless of the source
/// topic's own `durability`/`durability_class`, `Critical` sources
/// included. A DLQ copy only ever exists because delivery attempts against
/// the ALREADY-durable source record were exhausted — the source's own
/// append is what carried the strong guarantee, so the DLQ copy only needs
/// whatever `Standard` means on this node, not a second copy of whatever
/// stronger barrier the source topic pays for on every append.
///
/// This resolves through the SAME per-environment table as every other
/// `Standard` topic (`DurabilityClass::resolve`): `FsyncInterval{ms:
/// topics::STANDARD_FSYNC_INTERVAL_MS}` in Test/Prod, `Os` in Dev — DLQ is
/// no longer a special-cased environment-independent policy, it is simply
/// "a Standard-class topic like any other". `STANDARD_FSYNC_INTERVAL_MS`'s
/// own doc names this function as one of the two places that value is
/// load-bearing; it still is, just reached via class resolution rather
/// than a hardcoded literal here.
///
/// NOT retroactive by itself: `ensure_dlq_topic`'s get-or-create only calls
/// this on first creation, so a `__dlq.<topic>` created before this
/// decision existed would otherwise keep whatever durability it inherited
/// from its source at creation time forever — `BusService::new`'s
/// `migrate_legacy_dlq_durability` one-time startup sweep is what actually
/// repairs those rows (R5-8).
/// Review finding #6: `max_inline_bytes` is inherited from `source` rather
/// than left at `TopicOptions::default()`'s implicit
/// `topics::DEFAULT_MAX_INLINE_BYTES` — the most likely way a quarantine
/// write itself fails is the DLQ envelope (the full original payload plus
/// `dlq.*` headers) exceeding the DLQ topic's OWN limit, which used to
/// default independently of the source topic's own (possibly larger)
/// limit. No other per-record/per-batch size limit exists on `TopicConfig`
/// to inherit alongside it (`max_inline_bytes` is the only field `publish`
/// checks a record's size against, `bus/mod.rs`'s entry check).
///
/// NOT retroactive: this only takes effect at a `__dlq.<topic>` topic's
/// FIRST creation (`ensure_dlq_topic`'s get-or-create only calls this
/// function on a miss) — a DLQ topic that already exists keeps whatever
/// `max_inline_bytes` it was created with, even if its source topic's own
/// limit is later raised past it. An operator can still raise it explicitly
/// via `update_topic` on the `__dlq.<topic>` topic itself.
pub fn dlq_topic_options(source: &TopicConfig) -> TopicOptions {
    TopicOptions {
        partitions: Some(source.partitions),
        retention_ms: Some(30 * 24 * 3_600_000),
        replication_factor: Some(source.replication_factor),
        durability_class: Some(super::topics::DurabilityClass::Standard),
        compression: Some(source.compression),
        max_inline_bytes: Some(source.max_inline_bytes),
        ..Default::default()
    }
}

// =============================================================================
// Real discard (M1-R2 review N-5, coordinator decision 2)
// =============================================================================
//
// `BusService::dlq_discard`'s doc used to say, correctly at the time, "THIS
// DOES NOT DELETE OR TOMBSTONE ANYTHING" — M1's log engine has no
// per-record delete, so the old implementation was audit-only: the record
// stayed fully visible in `DlqList`/`peek`, `DlqRetryAll` would happily
// republish it, and `dlq_depth` never dropped. The UI's "Odrzuć" confirm
// dialog, meanwhile, told the operator the record was "trwale odrzucony
// (tombstone) … nie można cofnąć" — a flatly false claim for data that can
// (and, in the review's reproduction, DID) come right back via "Ponów
// wszystkie" minutes later.
//
// `DiscardStore` is what makes "Odrzuć" true instead: a durable, fjall-
// backed SET of `(org, dlq_topic, partition, offset)` coordinates the
// caller wants excluded from every DLQ read/retry surface. It does not
// change what `dlq_discard`'s doc still correctly says about the log
// engine — no bytes move, no tombstone is written to the log itself — but
// every caller-facing DLQ surface (`DlqList`/`peek`'s dispatch-layer
// wrapper, `DlqRetryAll`, `dlq_depth`) now treats a discarded record as
// gone, which is the actual UI-visible contract "Odrzuć" needs to honor.
pub const DISCARDED_KEYSPACE: &str = "dlq_discarded";
const DISCARD_SEP: u8 = 0;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct DiscardRecord {
    discarded_at_ms: i64,
}

/// Durable set of DLQ records an admin has marked "handled" via
/// `BusService::dlq_discard` — see this module's section doc. Lives in its
/// own fjall keyspace inside the SAME `Database` (`<bus_dir>/_meta`)
/// `groups::GroupOffsetStore`'s `offsets` keyspace already uses: no new
/// file, no new lock, same `groups.rs`/`producer.rs` key-encoding style
/// (raw byte-concatenated key, `DISCARD_SEP`-separated components, CBOR
/// value via `codec::encode`/`decode`).
pub struct DiscardStore {
    db: Database,
    keyspace: Keyspace,
}

impl DiscardStore {
    pub fn open(db: &Database) -> Result<Self, BusServiceError> {
        Ok(Self {
            db: db.clone(),
            keyspace: db.keyspace(DISCARDED_KEYSPACE, KeyspaceCreateOptions::default)?,
        })
    }

    /// `org_id SEP dlq_topic SEP partition(be) SEP offset(be)` — `dlq_topic`
    /// is the SECOND key component (unlike `GroupOffsetStore`'s layout,
    /// where a group name can precede a topic name and `purge_topic` has to
    /// scan-and-inspect for that reason), so a direct byte-range prefix scan
    /// on `org_id_and_topic_prefix` unambiguously matches only that
    /// `(org, dlq_topic)` pair: DLQ topic names go through the same
    /// `validate_user_topic_name` charset as everything else, which
    /// excludes the separator byte (NUL), so no topic name can itself
    /// contain `DISCARD_SEP` and shift the boundary.
    fn key(org_id: &str, dlq_topic: &str, partition: u32, offset: u64) -> Vec<u8> {
        let mut k = Self::partition_prefix(org_id, dlq_topic, partition);
        k.extend_from_slice(&offset.to_be_bytes());
        k
    }

    fn partition_prefix(org_id: &str, dlq_topic: &str, partition: u32) -> Vec<u8> {
        let mut k = Self::topic_prefix(org_id, dlq_topic);
        k.extend_from_slice(&partition.to_be_bytes());
        k.push(DISCARD_SEP);
        k
    }

    fn topic_prefix(org_id: &str, dlq_topic: &str) -> Vec<u8> {
        let mut k = Vec::with_capacity(org_id.len() + dlq_topic.len() + 2);
        k.extend_from_slice(org_id.as_bytes());
        k.push(DISCARD_SEP);
        k.extend_from_slice(dlq_topic.as_bytes());
        k.push(DISCARD_SEP);
        k
    }

    fn org_prefix(org_id: &str) -> Vec<u8> {
        let mut k = org_id.as_bytes().to_vec();
        k.push(DISCARD_SEP);
        k
    }

    /// Marks `(dlq_topic, partition, offset)` discarded — `BusService::
    /// dlq_discard`'s only durable side effect. Idempotent: discarding an
    /// already-discarded offset just overwrites the marker's timestamp.
    pub fn mark(
        &self,
        org_id: &str,
        dlq_topic: &str,
        partition: u32,
        offset: u64,
        now_ms: i64,
    ) -> Result<(), BusServiceError> {
        let key = Self::key(org_id, dlq_topic, partition, offset);
        self.keyspace.insert(
            &key,
            encode(&DiscardRecord {
                discarded_at_ms: now_ms,
            })?,
        )?;
        self.db.persist(PersistMode::SyncData)?;
        Ok(())
    }

    pub fn is_discarded(
        &self,
        org_id: &str,
        dlq_topic: &str,
        partition: u32,
        offset: u64,
    ) -> Result<bool, BusServiceError> {
        let key = Self::key(org_id, dlq_topic, partition, offset);
        Ok(self.keyspace.get(&key)?.is_some())
    }

    /// Every offset currently marked discarded for `(dlq_topic, partition)`
    /// at or above `earliest_offset`. Entries BELOW `earliest_offset` name a
    /// DLQ record retention has already physically removed (the whole
    /// segment holding it is gone) — dead weight with nothing left to
    /// filter out of any read, so this opportunistically deletes them
    /// during the same scan rather than requiring a dedicated sweep (PLAN's
    /// retention scan itself never visits this keyspace).
    pub fn discarded_offsets(
        &self,
        org_id: &str,
        dlq_topic: &str,
        partition: u32,
        earliest_offset: u64,
    ) -> Result<std::collections::HashSet<u64>, BusServiceError> {
        let prefix = Self::partition_prefix(org_id, dlq_topic, partition);
        let mut live = std::collections::HashSet::new();
        let mut stale: Vec<Vec<u8>> = Vec::new();
        for guard in self.keyspace.prefix(&prefix) {
            let key = guard.key()?;
            let offset_bytes = &key[prefix.len()..];
            let Ok(offset_arr) = <[u8; 8]>::try_from(offset_bytes) else {
                continue;
            };
            let offset = u64::from_be_bytes(offset_arr);
            if offset < earliest_offset {
                stale.push(key.to_vec());
            } else {
                live.insert(offset);
            }
        }
        for k in stale {
            self.keyspace.remove(k)?;
        }
        Ok(live)
    }

    /// Deletes every discard marker for `(org_id, dlq_topic)` — called by
    /// `BusService::delete_topic` when the topic being deleted IS (or has)
    /// a DLQ topic, mirroring `GroupOffsetStore::purge_topic`/
    /// `ProducerSeqStore::purge_topic`. Returns the number of keys removed.
    pub fn purge_topic(&self, org_id: &str, dlq_topic: &str) -> Result<usize, BusServiceError> {
        let prefix = Self::topic_prefix(org_id, dlq_topic);
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

    /// Deletes every discard marker for `org_id` — GDPR/RODO org purge,
    /// mirroring `GroupOffsetStore::purge_org`/`ProducerSeqStore::purge_org`.
    /// Returns the number of keys removed.
    pub fn purge_org(&self, org_id: &str) -> Result<usize, BusServiceError> {
        let prefix = Self::org_prefix(org_id);
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

    #[test]
    fn dlq_topic_name_uses_double_underscore_prefix() {
        assert_eq!(dlq_topic_name("orders.created"), "__dlq.orders.created");
    }

    /// Owner decision B (v143): `dlq_topic_options` always pins
    /// `durability_class` to `Standard` and leaves `durability` itself
    /// unset (class-derived, not an explicit override), regardless of the
    /// source topic's own durability — including a `Critical` source
    /// (`FsyncBatchFull`) — see this function's own doc for why.
    #[test]
    fn dlq_topic_options_always_pins_standard_class_regardless_of_source() {
        use super::super::topics::{
            Acks, CleanupPolicy, CompressionPolicy, DeliveryMode, DurabilityClass,
            DurabilityPolicy, ValidationMode,
        };
        use tentaflow_protocol::environment::NodeEnvironment;

        fn fixture_source(durability: DurabilityPolicy) -> TopicConfig {
            TopicConfig {
                instance_id: crate::bus::instance::LEGACY_SINGLE_INSTANCE.to_string(),
                name: "orders.created".to_string(),
                org_id: "org-1".to_string(),
                partitions: 4,
                retention_ms: 3_600_000,
                retention_bytes_per_partition: 64 * 1024 * 1024,
                cleanup_policy: CleanupPolicy::Delete,
                delivery: DeliveryMode::AtLeastOnce,
                idempotency_key: None,
                dedup_window_ms: 3_600_000,
                max_delivery_attempts: 5,
                retry_backoff_ms: 1_000,
                schema_id: None,
                validation: ValidationMode::Off,
                content_type: "application/octet-stream".to_string(),
                replication_factor: 1,
                acks: Acks::Leader,
                durability,
                durability_class: None,
                max_inline_bytes: 1024 * 1024,
                compression: CompressionPolicy::Lz4,
                environment: NodeEnvironment::Prod,
                created_at_ms: 0,
                updated_at_ms: 0,
            }
        }

        // Critical source (FsyncBatchFull): the DLQ options still pin
        // Standard, NOT a copy of the source's stronger barrier — and
        // `durability` itself is left unset (class wins, no explicit
        // override reaches `TopicConfig::from_options`).
        let critical_source = fixture_source(DurabilityPolicy::FsyncBatchFull);
        let critical_opts = dlq_topic_options(&critical_source);
        assert_eq!(
            critical_opts.durability_class,
            Some(DurabilityClass::Standard)
        );
        assert_eq!(critical_opts.durability, None);

        // Standard/Dev source (Os): same pinned class either way — the
        // DLQ's class never tracks the source's own class/policy at all.
        let dev_source = fixture_source(DurabilityPolicy::Os);
        let dev_opts = dlq_topic_options(&dev_source);
        assert_eq!(dev_opts.durability_class, Some(DurabilityClass::Standard));
        assert_eq!(dev_opts.durability, None);
    }

    // The pinned `Standard` class's actual per-environment resolution
    // through `TopicConfig::from_options` (Test/Prod get the fixed interval
    // policy, Dev gets `Os`, both stay class-derived) is exercised by
    // `topics::tests::dlq_topic_options_resolves_per_environment_and_stays_class_derived`
    // — `from_options` is private to `topics.rs`, so that integration test
    // lives there instead of here.

    /// Review finding #6: the most likely way a schema-violation quarantine
    /// write fails is the DLQ envelope (full original payload + `dlq.*`
    /// headers) exceeding the DLQ topic's OWN `max_inline_bytes` — which
    /// used to default independently of the source topic's own, possibly
    /// much larger, limit. A freshly created DLQ topic must inherit it.
    #[test]
    fn dlq_topic_options_inherits_max_inline_bytes_from_the_source() {
        use super::super::topics::{
            Acks, CleanupPolicy, CompressionPolicy, DeliveryMode, DurabilityPolicy, ValidationMode,
        };
        use tentaflow_protocol::environment::NodeEnvironment;

        let source = TopicConfig {
            instance_id: crate::bus::instance::LEGACY_SINGLE_INSTANCE.to_string(),
            name: "orders.created".to_string(),
            org_id: "org-1".to_string(),
            partitions: 4,
            retention_ms: 3_600_000,
            retention_bytes_per_partition: 64 * 1024 * 1024,
            cleanup_policy: CleanupPolicy::Delete,
            delivery: DeliveryMode::AtLeastOnce,
            idempotency_key: None,
            dedup_window_ms: 3_600_000,
            max_delivery_attempts: 5,
            retry_backoff_ms: 1_000,
            schema_id: None,
            validation: ValidationMode::Off,
            content_type: "application/octet-stream".to_string(),
            replication_factor: 1,
            acks: Acks::Leader,
            durability: DurabilityPolicy::Os,
            durability_class: None,
            max_inline_bytes: 8 * 1024 * 1024,
            compression: CompressionPolicy::Lz4,
            environment: NodeEnvironment::Prod,
            created_at_ms: 0,
            updated_at_ms: 0,
        };
        let opts = dlq_topic_options(&source);
        assert_eq!(opts.max_inline_bytes, Some(8 * 1024 * 1024));
    }

    #[test]
    fn backoff_doubles_and_caps() {
        // `attempts` is 1-based (the first failed delivery reports
        // `attempts == 1`, see `groups.rs::record_delivery_attempt`) and
        // the FIRST retry must back off by `base_ms` itself, not `2 *
        // base_ms` — an `attempts = 0` case can never actually occur, so
        // asserting against it (as this test previously did) verified a
        // value the real call path never produces.
        let b1 = compute_backoff_ms(1, 1000, 60_000, 1);
        let b2 = compute_backoff_ms(2, 1000, 60_000, 1);
        let b3 = compute_backoff_ms(3, 1000, 60_000, 1);
        // Roughly doubling each attempt, even with jitter (+/-20% leaves
        // headroom).
        assert!(b1 >= 800 && b1 <= 1200, "b1={b1}");
        assert!(b2 >= 1600 && b2 <= 2400, "b2={b2}");
        assert!(b3 >= 3200 && b3 <= 4800, "b3={b3}");

        let high = compute_backoff_ms(20, 1000, 60_000, 1);
        assert!(high <= 60_000, "capped at 60s, got {high}");
        assert!(
            high as f64 >= 60_000.0 * 0.8,
            "cap respects jitter floor too"
        );
    }

    #[test]
    fn backoff_is_deterministic_for_a_given_seed() {
        let a = compute_backoff_ms(3, 1000, 60_000, 42);
        let b = compute_backoff_ms(3, 1000, 60_000, 42);
        assert_eq!(a, b);
        let c = compute_backoff_ms(3, 1000, 60_000, 43);
        assert_ne!(
            a, c,
            "different seeds should (almost always) jitter differently"
        );
    }

    #[test]
    fn build_dlq_record_carries_envelope_headers_and_original_payload() {
        let original = PublishRecord {
            key: Some(Bytes::from_static(b"order-1")),
            headers: vec![
                ("tf.org".to_string(), Bytes::from_static(b"org-1")),
                ("app.correlation".to_string(), Bytes::from_static(b"c-1")),
            ],
            payload: Bytes::from_static(b"payload-bytes"),
            timestamp_ms: 1_000,
            schema_id: 0,
        };
        let dlq_rec = build_dlq_record(
            "orders.created",
            2,
            42,
            "billing-group",
            5,
            10_000,
            15_000,
            DlqReason::ConsumerError,
            "boom",
            &original,
        );
        assert_eq!(dlq_rec.payload, original.payload);
        assert_eq!(dlq_rec.key, original.key);
        let find = |name: &str| {
            dlq_rec
                .headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        };
        // a stale `tf.*` copy from the original publish is stripped
        // here — `bus::mod`'s `publish` (which `note_delivery_failure`
        // calls to actually send this record) re-stamps fresh `tf.*`
        // headers reflecting the DLQ send itself, so keeping the old copy
        // around would just be dead weight `publish` throws away anyway
        // (the provenance boundary). A non-`tf.` original header must
        // still survive untouched.
        assert!(find("tf.org").is_none(), "stale tf.* must be stripped");
        assert_eq!(find("app.correlation").unwrap(), Bytes::from_static(b"c-1"));
        assert_eq!(
            find("dlq.source_topic").unwrap(),
            Bytes::from_static(b"orders.created")
        );
        assert_eq!(
            find("dlq.source_partition").unwrap(),
            Bytes::from_static(b"2")
        );
        assert_eq!(
            find("dlq.source_offset").unwrap(),
            Bytes::from_static(b"42")
        );
        assert_eq!(
            find("dlq.group_id").unwrap(),
            Bytes::from_static(b"billing-group")
        );
        assert_eq!(find("dlq.attempts").unwrap(), Bytes::from_static(b"5"));
        assert_eq!(
            find("dlq.reason").unwrap(),
            Bytes::from_static(b"consumer_error")
        );
        assert_eq!(
            find("dlq.error_message").unwrap(),
            Bytes::from_static(b"boom")
        );
    }

    fn test_producer_ctx(actor: Option<&str>, correlation_id: Option<&str>) -> BusCallContext {
        BusCallContext {
            org_id: "org-1".to_string(),
            actor: actor.map(str::to_string),
            correlation_id: correlation_id.map(str::to_string),
            origin: "test".to_string(),
        }
    }

    #[test]
    fn build_publish_violation_record_carries_no_source_offset_or_partition_header() {
        let original = PublishRecord {
            key: Some(Bytes::from_static(b"order-1")),
            headers: vec![
                ("tf.org".to_string(), Bytes::from_static(b"org-1")),
                ("app.correlation".to_string(), Bytes::from_static(b"c-1")),
            ],
            payload: Bytes::from_static(b"payload-bytes"),
            timestamp_ms: 1_000,
            schema_id: 0,
        };
        let rec = build_publish_violation_record(
            "orders.created",
            "schema_violation",
            "field 'x' is required",
            &test_producer_ctx(None, None),
            &original,
        );
        assert_eq!(rec.payload, original.payload);
        assert_eq!(rec.key, original.key);
        let find = |name: &str| {
            rec.headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        };
        assert!(find("tf.org").is_none(), "stale tf.* must be stripped");
        assert_eq!(find("app.correlation").unwrap(), Bytes::from_static(b"c-1"));
        assert_eq!(
            find("dlq.source_topic").unwrap(),
            Bytes::from_static(b"orders.created")
        );
        assert_eq!(
            find("dlq.reason").unwrap(),
            Bytes::from_static(b"schema_violation")
        );
        assert_eq!(
            find("dlq.error_message").unwrap(),
            Bytes::from_static(b"field 'x' is required")
        );
        assert!(find("dlq.rejected_at_ms").is_some());
        assert!(
            find("dlq.source_partition").is_none(),
            "publish-time violation never had an offset/partition to record"
        );
        assert!(find("dlq.source_offset").is_none());
        assert!(find("dlq.group_id").is_none());
        // No actor/correlation_id on the producer ctx: neither header is
        // added at all, rather than an empty-string placeholder.
        assert!(find("dlq.producer_actor").is_none());
        assert!(find("dlq.correlation_id").is_none());
    }

    /// Review finding #5: the quarantine write itself runs under a broker
    /// `SYSTEM_ACTOR` context (`bus/mod.rs`'s `quarantine_ctx`), so without
    /// carrying the ORIGINAL producer's own actor/correlation id forward
    /// into the DLQ record, an operator triaging the DLQ could never tell
    /// who actually produced a quarantined record.
    #[test]
    fn build_publish_violation_record_carries_the_producers_actor_and_correlation_id() {
        let original = PublishRecord {
            key: None,
            headers: vec![],
            payload: Bytes::from_static(b"payload-bytes"),
            timestamp_ms: 1_000,
            schema_id: 0,
        };
        let rec = build_publish_violation_record(
            "orders.created",
            "schema_violation",
            "field 'x' is required",
            &test_producer_ctx(Some("user-42"), Some("corr-9")),
            &original,
        );
        let find = |name: &str| {
            rec.headers
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(
            find("dlq.producer_actor").unwrap(),
            Bytes::from_static(b"user-42")
        );
        assert_eq!(
            find("dlq.correlation_id").unwrap(),
            Bytes::from_static(b"corr-9")
        );
    }

    #[test]
    fn error_message_is_truncated() {
        let original = PublishRecord {
            key: None,
            headers: vec![],
            payload: Bytes::from_static(b"x"),
            timestamp_ms: 0,
            schema_id: 0,
        };
        let huge = "a".repeat(MAX_ERROR_MESSAGE_BYTES + 500);
        let dlq_rec = build_dlq_record(
            "t",
            0,
            0,
            "g",
            1,
            0,
            0,
            DlqReason::ConsumerError,
            &huge,
            &original,
        );
        let msg = dlq_rec
            .headers
            .iter()
            .find(|(k, _)| k == "dlq.error_message")
            .unwrap()
            .1
            .clone();
        assert_eq!(msg.len(), MAX_ERROR_MESSAGE_BYTES);
    }

    #[test]
    fn build_retry_record_replaces_dlq_headers_with_retry_of() {
        let dlq_rec = PublishRecord {
            key: Some(Bytes::from_static(b"k")),
            headers: vec![
                ("tf.org".to_string(), Bytes::from_static(b"org-1")),
                (
                    "dlq.reason".to_string(),
                    Bytes::from_static(b"consumer_error"),
                ),
            ],
            payload: Bytes::from_static(b"payload"),
            timestamp_ms: 1_000,
            schema_id: 0,
        };
        let retry = build_retry_record(&dlq_rec, "__dlq.orders", 7);
        assert!(retry.headers.iter().all(|(k, _)| k != "dlq.reason"));
        let retry_of = retry
            .headers
            .iter()
            .find(|(k, _)| k == "dlq.retry_of")
            .unwrap()
            .1
            .clone();
        assert_eq!(retry_of, Bytes::from_static(b"__dlq.orders#7"));
        assert_eq!(retry.payload, dlq_rec.payload);
    }

    // ---- DiscardStore (M1-R2 review N-5, coordinator decision 2) ------

    fn temp_db() -> (tempfile::TempDir, Database) {
        let dir = tempfile::tempdir().expect("create temp dir");
        let db = Database::builder(dir.path()).open().expect("open fjall db");
        (dir, db)
    }

    #[test]
    fn mark_and_is_discarded_round_trip() {
        let (_dir, db) = temp_db();
        let store = DiscardStore::open(&db).unwrap();
        assert!(!store.is_discarded("org-1", "__dlq.orders", 0, 2).unwrap());
        store.mark("org-1", "__dlq.orders", 0, 2, 1_000).unwrap();
        assert!(store.is_discarded("org-1", "__dlq.orders", 0, 2).unwrap());
        // A neighboring offset/partition/org/topic is untouched.
        assert!(!store.is_discarded("org-1", "__dlq.orders", 0, 3).unwrap());
        assert!(!store.is_discarded("org-1", "__dlq.orders", 1, 2).unwrap());
        assert!(!store.is_discarded("org-2", "__dlq.orders", 0, 2).unwrap());
        assert!(!store.is_discarded("org-1", "__dlq.other", 0, 2).unwrap());
    }

    #[test]
    fn discarded_offsets_prunes_entries_below_earliest_offset() {
        let (_dir, db) = temp_db();
        let store = DiscardStore::open(&db).unwrap();
        store.mark("org-1", "__dlq.orders", 0, 1, 1_000).unwrap();
        store.mark("org-1", "__dlq.orders", 0, 5, 1_000).unwrap();
        store.mark("org-1", "__dlq.orders", 0, 9, 1_000).unwrap();

        // Retention has moved earliest_offset to 5: offset 1 is dead weight.
        let live = store
            .discarded_offsets("org-1", "__dlq.orders", 0, 5)
            .unwrap();
        assert_eq!(live, std::collections::HashSet::from([5, 9]));

        // The stale entry was actually removed, not just filtered out of
        // this one call's result.
        assert!(!store.is_discarded("org-1", "__dlq.orders", 0, 1).unwrap());
        assert!(store.is_discarded("org-1", "__dlq.orders", 0, 5).unwrap());
    }

    #[test]
    fn purge_topic_removes_only_that_dlq_topics_markers() {
        let (_dir, db) = temp_db();
        let store = DiscardStore::open(&db).unwrap();
        store.mark("org-1", "__dlq.orders", 0, 1, 1_000).unwrap();
        store.mark("org-1", "__dlq.orders", 1, 2, 1_000).unwrap();
        store.mark("org-1", "__dlq.other", 0, 1, 1_000).unwrap();
        store.mark("org-2", "__dlq.orders", 0, 1, 1_000).unwrap();

        let deleted = store.purge_topic("org-1", "__dlq.orders").unwrap();
        assert_eq!(deleted, 2);
        assert!(!store.is_discarded("org-1", "__dlq.orders", 0, 1).unwrap());
        assert!(!store.is_discarded("org-1", "__dlq.orders", 1, 2).unwrap());
        assert!(store.is_discarded("org-1", "__dlq.other", 0, 1).unwrap());
        assert!(store.is_discarded("org-2", "__dlq.orders", 0, 1).unwrap());
    }

    #[test]
    fn purge_org_removes_only_that_orgs_markers() {
        let (_dir, db) = temp_db();
        let store = DiscardStore::open(&db).unwrap();
        store.mark("org-1", "__dlq.orders", 0, 1, 1_000).unwrap();
        store.mark("org-1", "__dlq.other", 0, 1, 1_000).unwrap();
        store.mark("org-2", "__dlq.orders", 0, 1, 1_000).unwrap();

        let deleted = store.purge_org("org-1").unwrap();
        assert_eq!(deleted, 2);
        assert!(!store.is_discarded("org-1", "__dlq.orders", 0, 1).unwrap());
        assert!(!store.is_discarded("org-1", "__dlq.other", 0, 1).unwrap());
        assert!(store.is_discarded("org-2", "__dlq.orders", 0, 1).unwrap());
    }
}
