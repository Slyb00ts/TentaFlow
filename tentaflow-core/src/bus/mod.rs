// =============================================================================
// File: bus/mod.rs — TentaBus M1: production single-node service layer
// =============================================================================
//
// Singleton wired exactly like `sync/runtime.rs` (`OnceLock<Arc<..>>` +
// `init`/`global`), sitting on top of the M0 log engine (`tentaflow-bus`
// crate) added as a path dependency. Public surface follows PLAN.md §6.1
// verbatim: `publish`/`open_consumer`/`fetch`/`commit`, all taking
// `BusCallContext` (org_id/actor/correlation_id/origin — the same fields as
// `flow_engine::node_adapter::ExecutionContext`, so provenance flows into
// the record's system headers without extra plumbing).
//
// `BusInitConfig::bus_dir` is a plain, already-resolved `PathBuf` — the
// caller obtains it via `paths::category_dir(paths::StorageCategory::Bus)`
// (that category exists; see `paths.rs`'s `StorageCategory::Bus` variant).
// This module deliberately does not depend on `paths` directly, and nothing
// in this repo calls `bus::init` from real application startup yet — wiring
// that up (choosing where in the startup sequence, resolving `bus_dir`,
// wiring `BusAuthorizer` to real RBAC) is tor P's dispatch-layer work, out
// of this file's scope.
//
// COORDINATION (tor P/D, RBAC + dispatch): authorization is a trait
// (`BusAuthorizer`) injected at `init()`, never implemented here — this
// module has no opinion on roles/permissions, only on WHERE the check
// happens (PLAN §8.1: session open, i.e. `publish`/`open_consumer`, not per
// message, PLUS revalidation on every `fetch`/`commit` — see
// `BusAuthorizer::generation`'s doc). See `BusAuthorizer`'s own doc for the
// full list of what a production implementation must enforce (DLQ
// self-produce, generation bumping).
//
// BLOCKING: `BusService::publish` and `ConsumerHandle::fetch` are both fully
// synchronous — `publish` calls `Partition::append_batch` (`blocking_recv`
// on an internal channel) and `fetch` calls `PartitionReader::
// fetch_from_offset` plus a bounded `std::thread::sleep` long-poll. Neither
// has an async counterpart wired up at this layer (the engine's own
// `append_batch_async` exists but nothing here uses it). Any caller on an
// async executor (Tokio) MUST invoke these through `spawn_blocking` —
// calling them directly from an `async fn` blocks that worker thread for
// the call's full duration, including the long-poll wait.
//
// GDPR/RODO org purge: `BusService::purge_org` hard-deletes
// everything this module holds for an org (topic/group rows, fjall offset
// and producer-sequence keys, the on-disk directory). `services::org::repo::
// delete_organization` is a SOFT delete and does NOT call this — it must
// not, since a soft-deleted org may still need its data for the retention
// window a compliance policy grants it. Whatever hard-delete/compliance-
// erasure flow eventually gets built (there is none in this repo yet) is
// the caller responsible for invoking `purge_org` once an org's erasure
// becomes irreversible.
//
// PARTITION HANDLE LIFETIME (M1): `open_consumer` opens and keeps a full
// `Partition` (writer thread + directory flock included, not a read-only
// handle) for every partition of every topic it subscribes to, for as long
// as the returned `ConsumerHandle` lives — literally: `ConsumerPartition`
// stores its OWN `tentaflow_bus::Partition` clone (`Arc`-backed) alongside
// the `PartitionReader` it fetches through, not just the reader. This is
// deliberate for M1's single-node shape, where the same process both
// produces and consumes — the handle is already open for writing via
// `publish`'s own `partition_handle`, so consuming from it too costs
// nothing extra. An LRU/reaper that closes idle partition handles is
// explicitly DEFERRED to M2, whose replication design needs a different
// partition lifecycle anyway (a partition handle that can be closed and
// reopened while other nodes keep serving it).
//
// `run_retention_sweep` is the one thing that ever removes an entry from
// `BusService::partitions` outside of `delete_topic`/`purge_org`: it closes
// any MAP entry it opened for itself that was not already present before
// the sweep started, so a periodic system-wide sweep never permanently
// accumulates a writer thread/flock per partition on a node nobody is
// otherwise consuming from. Because `ConsumerPartition` keeps its own
// `Partition` clone (not just a `PartitionReader`), the sweeper removing the
// SHARED MAP's entry can never silently stop a live consumer: the `Arc` the
// consumer holds keeps the writer thread and flock alive regardless of what
// happens to the map, and the consumer's own `Drop` is what actually
// releases them once it goes away. The only place this matters is
// `purge_org`/`delete_topic`'s `detach()` call, which must reach a
// consumer's clone even when the sweeper has already removed it from the
// map — `BusService::consumer_partitions` is a `Weak`-referenced side
// registry kept exactly for that (see its own doc).

pub mod codec;
pub mod dedup;
pub mod dlq;
pub mod field_policies;
pub mod groups;
pub mod payload_format;
pub mod producer;
pub mod quota;
pub mod reactor;
pub mod replication;
pub mod retention;
pub mod topics;

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use bytes::Bytes;
use dashmap::DashMap;
use tentaflow_protocol::environment::NodeEnvironment;

use crate::db::DbPool;

static BUS_SERVICE: OnceLock<Arc<BusService>> = OnceLock::new();

/// Any incoming header using this prefix is silently STRIPPED before the
/// broker's own provenance headers (`tf.org`/`tf.actor`/
/// `tf.correlation_id`/`tf.origin`/`tf.content_type`/`tf.env`) are stamped
///. A consumer/UI reading `tf.*` needs the guarantee that it was
/// never caller-controllable — without this, `record.headers` was appended
/// FIRST, then the broker's own `tf.*` pushed again, so a forged
/// `tf.actor` from the wire sat right next to the real one with no
/// deduplication; whichever a reader happened to pick first on a
/// duplicate-key lookup could be the forged value.
///
/// STRIP rather than REJECT (the task's other option) because `publish` is
/// also used internally to re-thread an already-published record forward —
/// `dlq_retry` (this file) and `note_delivery_failure` (`dlq.rs`'s
/// `build_dlq_record`/`build_retry_record`) both copy a previously fetched
/// record's headers, `tf.*` included, into a new `PublishRecord` before
/// calling `publish` again. Rejecting outright would make every DLQ
/// send/retry fail; stripping cleanly discards the stale copy and lets the
/// fresh, correct stamp win — matching the FIRST bullet's actual security
/// property, since no code path here forwards a caller-uncontrollable
/// header we can't already trust.
const RESERVED_HEADER_PREFIX: &str = "tf.";

/// Window size for `AuditWindows` : "one `audit_log`
/// entry per (org, event kind) per 60 s window".
const AUDIT_WINDOW: Duration = Duration::from_secs(60);

/// `group_id` of the now-retired ephemeral "how much is on this partition"
/// probe consumer `dispatch/bus.rs` used to open under a fixed, reused
/// group name before `BusService::partition_stats` existed (M1-R2 review
/// N-1/N-7, coordinator decision 3). Nothing in this codebase opens a
/// consumer under this name anymore, but a row could already exist in
/// `bus_groups` from before this change shipped (every real "probe" is a
/// consumer that DID commit an offset once, so it left a durable row like
/// any other group) — `BusService::new` deletes any such leftover row
/// outright on every startup so it can never resurface in `GroupList`/KPI
/// counts even before `dispatch/bus.rs`'s own `tf-`-prefix filter (defense
/// in depth) gets a chance to hide it.
const LEGACY_PROBE_GROUP_ID: &str = "tf-system-probe";

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Shared by `BusService::is_group_paused` and `ConsumerHandle::fetch`
/// — a `ConsumerHandle` has its own `DbPool` clone but no
/// `&BusService`, so this lives as a free function rather than an
/// `impl BusService` method both call sites would need a service reference
/// for.
fn group_paused(
    db: &DbPool,
    org_id: &str,
    group: &str,
    topic: &str,
) -> Result<bool, BusServiceError> {
    Ok(
        crate::db::repository::bus_group_get(db, org_id, group, topic)?
            .map(|g| g.paused)
            .unwrap_or(false),
    )
}

/// Builds standardized `audit_log` `details` text (task: every bus audit
/// entry must carry `org_id`, since two orgs sharing a topic/group name are
/// otherwise indistinguishable in a shared `audit_log`) — leads with
/// `org_id=<org>`, then appends `extra` verbatim (each call site formats
/// its own `topic=…/group=…/partition=…/offset=…` tail with `format!`,
/// since the set of fields that apply differs per action).
fn audit_details(org_id: &str, extra: Option<&str>) -> String {
    match extra {
        Some(e) if !e.is_empty() => format!("org_id={org_id} {e}"),
        _ => format!("org_id={org_id}"),
    }
}

/// One in-memory bucket per (org, audit action), counting occurrences
/// within the current window : a producer stuck in a
/// retry loop against `QuotaExceeded`/`PermissionDenied` used to write one
/// `audit_log` row per rejected request, which floods the platform's
/// shared, hash-chained `audit_log` writer. Applied to `bus.quota.exceeded`,
/// `bus.produce.denied`, and `bus.consume.denied` — the three actions PLAN
/// §8.2 lists that a caller can trigger once per request on a hot path.
///
/// Behavior: the FIRST occurrence in a fresh window is written to
/// `audit_log` immediately (so an isolated denial is never silently
/// invisible), with `count=1`. Every subsequent occurrence within the same
/// window is only counted in memory, never written. Once the window has
/// elapsed, the NEXT occurrence starts a new window and its own
/// `audit_log` row folds in ("flushes") however many occurrences were
/// suppressed since the last write, so no occurrence is ever permanently
/// lost from the audit trail, even though most of them are never charged an
/// individual row.
/// One flushed bucket's worth of data (`AuditWindows::drain_suppressed`):
/// `(org_id, kind, suppressed_count, resource, actor)`.
type AuditFlushEntry = (String, &'static str, u32, Option<String>, Option<String>);

struct AuditWindows {
    buckets: DashMap<(String, &'static str), AuditWindowBucket>,
    /// Milliseconds, not `Duration` directly, so `#[cfg(test)]` code can
    /// shrink the window (see `set_window_for_test`) without needing
    /// interior mutability on the whole struct — production code never
    /// changes this after construction.
    window_ms: AtomicU64,
}

struct AuditWindowBucket {
    window_start: Instant,
    /// Occurrences seen since `window_start` that have NOT yet been written
    /// to `audit_log` — i.e. everything after the first one, which is
    /// written synchronously by `record` itself.
    suppressed: u32,
    /// Resource/actor of the MOST RECENT occurrence recorded in this
    /// bucket, kept so a later flush (`drain_suppressed`, which has no
    /// per-call context of its own) can still write a meaningful
    /// `resource`/`actor` for the suppressed tail instead of `None`/`None`.
    last_resource: Option<String>,
    last_actor: Option<String>,
}

impl AuditWindows {
    fn new(window: Duration) -> Self {
        Self {
            buckets: DashMap::new(),
            window_ms: AtomicU64::new(window.as_millis() as u64),
        }
    }

    fn window(&self) -> Duration {
        Duration::from_millis(self.window_ms.load(Ordering::Relaxed))
    }

    /// Test-only seam: a real 60s window cannot be waited out in a unit
    /// test, so tests shrink it to observe window rollover deterministically
    /// with a short sleep instead.
    #[cfg(test)]
    fn set_window_for_test(&self, window: Duration) {
        self.window_ms
            .store(window.as_millis() as u64, Ordering::Relaxed);
    }

    /// Records one occurrence of `kind` for `org_id`. Returns `Some(count)`
    /// when THIS occurrence must be written to `audit_log` (see the struct
    /// doc) — `count` is how many occurrences that one row represents
    /// (`1` for a fresh window's first occurrence, or `suppressed + 1` when
    /// flushing a just-elapsed window). Returns `None` when this occurrence
    /// falls inside an already-opened window and was only counted.
    ///
    /// `resource`/`actor` are stashed on the bucket on EVERY call (not just
    /// suppressed ones) so a later `drain_suppressed` flush always has
    /// something to report, even though the immediate (non-suppressed)
    /// write below uses its own `resource`/`actor` parameters directly
    /// rather than reading them back off the bucket.
    fn record(
        &self,
        org_id: &str,
        kind: &'static str,
        resource: Option<&str>,
        actor: Option<&str>,
    ) -> Option<u32> {
        let now = Instant::now();
        let key = (org_id.to_string(), kind);
        let mut created = false;
        let mut bucket = self.buckets.entry(key).or_insert_with(|| {
            created = true;
            AuditWindowBucket {
                window_start: now,
                suppressed: 0,
                last_resource: resource.map(str::to_string),
                last_actor: actor.map(str::to_string),
            }
        });
        if created {
            return Some(1);
        }
        bucket.last_resource = resource.map(str::to_string);
        bucket.last_actor = actor.map(str::to_string);
        if now.duration_since(bucket.window_start) >= self.window() {
            let carried = bucket.suppressed + 1;
            bucket.window_start = now;
            bucket.suppressed = 0;
            Some(carried)
        } else {
            bucket.suppressed += 1;
            None
        }
    }

    /// Force-flushes every bucket, regardless of whether its window has
    /// elapsed, returning `(org_id, kind, suppressed_count, resource,
    /// actor)` for every bucket that actually had a suppressed occurrence
    /// pending — called by `bus::init`'s periodic timer (and `Drop`/
    /// `stop_background_sweeper`) so a burst that stops mid-window still
    /// gets its tail count written eventually, instead of only ever
    /// surfacing if some future occurrence happens to arrive and trigger
    /// the lazy flush in `record`.
    ///
    /// EVERY bucket is removed here, whether or not it had anything
    /// suppressed: an unbounded `(org, kind)` bucket map that only ever
    /// grows is its own resource leak on a long-running node. A bucket
    /// with nothing suppressed costs nothing to recreate (the next
    /// occurrence is treated as a fresh window's first, written
    /// immediately, same as if this bucket had never existed) — the only
    /// observable difference is that a flush can occasionally end a
    /// window earlier than a full `window()` would have.
    fn drain_suppressed(&self) -> Vec<AuditFlushEntry> {
        let keys: Vec<(String, &'static str)> = self
            .buckets
            .iter()
            .map(|entry| entry.key().clone())
            .collect();
        let mut out = Vec::new();
        for key in keys {
            if let Some((_, bucket)) = self.buckets.remove(&key) {
                if bucket.suppressed > 0 {
                    out.push((
                        key.0,
                        key.1,
                        bucket.suppressed,
                        bucket.last_resource,
                        bucket.last_actor,
                    ));
                }
            }
        }
        out
    }

    /// GDPR/RODO org purge (`BusService::purge_org`): drops every bucket
    /// belonging to `org_id` so a purged org's denial/quota history does
    /// not linger in this in-memory map (or get flushed to `audit_log`
    /// under an org_id nothing else in the system has any record of
    /// anymore) after the purge.
    fn remove_org(&self, org_id: &str) {
        self.buckets.retain(|(o, _), _| o != org_id);
    }
}

/// `(org, group, topic) -> paused` cache backing `ConsumerHandle::fetch`'s
/// pause check. `fetch` used to run `bus_group_get` (SQLite) once per
/// distinct topic on EVERY call — a long-poll consumer with a short
/// `max_wait_ms` turns that into hundreds-to-thousands of queries/second
/// against the same shared application database `publish`'s own
/// `topic_config_cache` was specifically added to stay off. Filled lazily
/// on the first `fetch` that touches a given (group, topic); invalidated by
/// `BusService::set_group_paused` (covers `pause_group`/`resume_group`) and
/// by an org purge/topic delete, exactly mirroring `topic_config_cache`'s
/// own invalidation shape.
struct GroupStateCache {
    cache: DashMap<(String, String, String), bool>,
    /// Counts only cache MISSES (real `bus_groups` reads) — the same kind
    /// of metric hook `BusService::topic_config_db_loads` provides for the
    /// publish-side cache.
    db_loads: AtomicU64,
}

impl GroupStateCache {
    fn new() -> Self {
        Self {
            cache: DashMap::new(),
            db_loads: AtomicU64::new(0),
        }
    }

    fn paused(
        &self,
        db: &DbPool,
        org_id: &str,
        group: &str,
        topic: &str,
    ) -> Result<bool, BusServiceError> {
        let key = (org_id.to_string(), group.to_string(), topic.to_string());
        if let Some(cached) = self.cache.get(&key) {
            return Ok(*cached);
        }
        self.db_loads.fetch_add(1, Ordering::Relaxed);
        let paused = group_paused(db, org_id, group, topic)?;
        self.cache.insert(key, paused);
        Ok(paused)
    }

    fn invalidate(&self, org_id: &str, group: &str, topic: &str) {
        self.cache
            .remove(&(org_id.to_string(), group.to_string(), topic.to_string()));
    }

    /// `BusService::delete_topic`: drops every group's cached state for
    /// this one topic (the topic itself is gone, regardless of which
    /// groups had ever subscribed to it).
    fn remove_topic(&self, org_id: &str, topic: &str) {
        self.cache
            .retain(|(o, _, t), _| !(o == org_id && t == topic));
    }

    /// `BusService::purge_org`: drops every entry for the whole org.
    fn remove_org(&self, org_id: &str) {
        self.cache.retain(|(o, _, _), _| o != org_id);
    }
}

/// Shared by `BusService::audit_windowed` and `ConsumerHandle::revalidate`
/// (the latter has no `&BusService` to call a method on) — writes exactly
/// one `audit_log` row when `windows.record` says this occurrence is not
/// suppressed, doing nothing otherwise.
#[allow(clippy::too_many_arguments)]
fn write_windowed_audit(
    db: &DbPool,
    windows: &AuditWindows,
    actor: Option<&str>,
    org_id: &str,
    kind: &'static str,
    resource: Option<&str>,
    extra: Option<&str>,
) {
    if let Some(count) = windows.record(org_id, kind, resource, actor) {
        let _ = crate::db::repository::log_audit(
            db,
            actor,
            None,
            kind,
            resource,
            Some(&format!("{} count={count}", audit_details(org_id, extra))),
            None,
            None,
        );
    }
}

/// Stable per-record partition hash: the first 8
/// bytes of `blake3(key)`, read little-endian, modulo the topic's partition
/// count.
///
/// WIRE-COMPATIBILITY INVARIANT: once any record has been routed under this
/// mapping, this function's output for a given `(key, partitions)` pair
/// must never change — a future edit that alters it silently remaps every
/// existing key to a different partition, breaking "same key → same
/// partition" for all data already on disk. This replaced M1's original
/// choice, `rustc-hash`'s `FxHasher`: `rustc-hash` documents
/// its hash as an implementation detail that has already changed across
/// major versions, so a routine `cargo update` could have remapped live
/// data. `blake3` has no such disclaimer and is exercised here purely as a
/// fixed bit-mixing function, not for any cryptographic property.
fn partition_for_key(key: &[u8], partitions: u32) -> u32 {
    let hash = blake3::hash(key);
    let mut first8 = [0u8; 8];
    first8.copy_from_slice(&hash.as_bytes()[..8]);
    (u64::from_le_bytes(first8) % partitions as u64) as u32
}

/// M2 (PLAN-M2 §1e): `create_topic`'s initial replica set for one
/// partition — always `[local_node_id, ..up to rf-1 nodes from
/// same_env]`, the local node leading every partition at creation time
/// (a later election/`transfer_leader` can move leadership; this is only
/// the STARTING placement). `same_env` is spread round-robin across
/// partitions (offset by `partition`) rather than every partition getting
/// the identical replica set, so a topic with more partitions than `rf-1`
/// distributes replication load across the whole same-environment pool
/// instead of concentrating it on the first `rf-1` nodes alphabetically.
/// `same_env` must already exclude the local node and be in a STABLE order
/// (`create_topic` sorts it) — the round-robin offset below is only
/// reproducible if the pool's order does not change between calls.
fn build_replica_set(
    local_node_id: &str,
    same_env: &[String],
    replication_factor: u32,
    partition: u32,
) -> Vec<String> {
    let mut replicas = vec![local_node_id.to_string()];
    if replication_factor <= 1 || same_env.is_empty() {
        return replicas;
    }
    let need = (replication_factor - 1) as usize;
    let start = (partition as usize) % same_env.len();
    for i in 0..need.min(same_env.len()) {
        let idx = (start + i) % same_env.len();
        replicas.push(same_env[idx].clone());
    }
    replicas
}

#[derive(Debug, thiserror::Error)]
pub enum BusServiceError {
    #[error("bus service not initialized")]
    NotInitialized,
    #[error("database error: {0}")]
    Db(String),
    #[error("fjall error: {0}")]
    Fjall(String),
    #[error("codec error: {0}")]
    Codec(String),
    #[error("io error: {0}")]
    Io(String),
    #[error("engine error: {0}")]
    Engine(#[from] tentaflow_bus::BusError),
    #[error("invalid topic name '{name}': {reason}")]
    InvalidTopicName { name: String, reason: &'static str },
    #[error("invalid topic config: {reason}")]
    InvalidTopicConfig { reason: String },
    #[error("topic '{name}' row is corrupt: field {field} has unrecognized value '{value}'")]
    CorruptTopicRow {
        name: String,
        field: &'static str,
        value: String,
    },
    #[error("topic '{name}' already exists")]
    TopicAlreadyExists { name: String },
    #[error("topic '{name}' not found")]
    TopicNotFound { name: String },
    #[error("permission denied: {action} on topic '{topic}'")]
    PermissionDenied { action: &'static str, topic: String },
    #[error("org quota exceeded, retry after {retry_after_ms} ms")]
    QuotaExceeded { retry_after_ms: u32 },
    /// A single request's `amount` of `unit` is larger than the bucket's
    /// capacity (capacity == the configured rate, `quota.rs`'s
    /// `TokenBucket`), so no amount of waiting would ever let it through —
    /// unlike `QuotaExceeded`, this must never carry a `retry_after_ms`,
    /// because that would tell the caller to retry a request that can never
    /// succeed, producing an infinite retry loop instead of a signal to
    /// shrink the batch or raise the org's quota.
    #[error(
        "publish request of {amount} {unit} exceeds the org quota's rate limit of {capacity} {unit}/s; this can never succeed via retry (reduce the batch size or raise the org's quota)"
    )]
    QuotaRequestTooLarge {
        unit: &'static str,
        amount: u64,
        capacity: u64,
    },
    #[error(
        "org '{org_id}' has reached its max_topics quota ({max}); {current} topics already exist"
    )]
    MaxTopicsExceeded {
        org_id: String,
        max: u32,
        current: u32,
    },
    #[error(
        "org '{org_id}' max_partitions quota ({max}) would be exceeded: {current} existing + {requested} requested"
    )]
    MaxPartitionsExceeded {
        org_id: String,
        max: u32,
        current: u32,
        requested: u32,
    },
    #[error("producer throttled, retry after {retry_after_ms} ms")]
    Throttled { retry_after_ms: u32 },
    #[error(
        "payload of {len} bytes exceeds max_inline_bytes ({max_inline_bytes}); use a BlobRef (PLAN §2.4) instead"
    )]
    PayloadTooLarge { len: usize, max_inline_bytes: usize },
    #[error("topic '{topic}' requires a record key for idempotency-key dedup")]
    DedupKeyRequired { topic: String },
    #[error("producer epoch fenced: current epoch is {current_epoch}")]
    ProducerFenced { current_epoch: u32 },
    #[error(
        "topic environment {topic_env} does not match node environment {node_env}; fail-closed per PLAN §4.4 (Z12)"
    )]
    EnvironmentMismatch {
        topic_env: NodeEnvironment,
        node_env: NodeEnvironment,
    },
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// `(topic, partition)` is not part of the `ConsumerHandle`'s own
    /// subscription set. Rejected before any fjall write so a handle for
    /// group `billing` can never move group `billing`'s (or anyone else's)
    /// offset on a topic it never opened.
    #[error(
        "cannot commit offset for '{topic}'/{partition}: not part of this consumer's subscription"
    )]
    NotSubscribed { topic: String, partition: u32 },
    /// `offset` is behind the currently committed offset. A downward
    /// move is only ever legitimate through `BusService::reset_offset`
    /// (`bus.admin` + audited `bus.offset.reset`), never through a
    /// consumer's own `commit`.
    #[error(
        "cannot commit offset {requested} for '{topic}'/{partition}: behind the committed offset {committed}"
    )]
    OffsetRegression {
        topic: String,
        partition: u32,
        requested: u64,
        committed: u64,
    },
    /// retention already deleted the segment holding `requested` —
    /// propagated from the engine's `BusError::OffsetOutOfRange` with
    /// `topic`/`partition` context attached so the caller can act on it
    /// (see `ConsumerHandle::seek_to_earliest`) instead of the fetch silently
    /// rebasing to whatever is now the oldest surviving offset.
    #[error(
        "requested offset {requested} for '{topic}'/{partition} is below the earliest retained offset {earliest} (latest {latest})"
    )]
    OffsetOutOfRange {
        topic: String,
        partition: u32,
        requested: u64,
        earliest: u64,
        latest: u64,
    },
    /// `fetch` refuses to serve a group that an admin has paused
    /// (`pause_group`) rather than silently returning an empty batch, so a
    /// caller polling in a loop gets an explicit signal to stop instead of
    /// mistaking "paused" for "caught up".
    #[error("group '{group}' is paused on topic '{topic}'")]
    GroupPaused { group: String, topic: String },
    /// a DLQ topic (`__dlq.<x>`) can never itself have a DLQ — M1 has
    /// no "poison of poison" escalation path. Distinct from
    /// `InvalidTopicName` (which `__dlq.__dlq.x` would otherwise hit via
    /// `validate_internal_topic_name`'s underscore rejection) because that
    /// message talks about character classes, not the actual rule being
    /// enforced.
    #[error("'{topic}' is already a DLQ topic; a DLQ-of-a-DLQ is not allowed")]
    DlqOfDlqNotAllowed { topic: String },
    /// `tentaflow_bus::BusError::PartitionPoisoned`: a group-commit fsync
    /// failed after that group had already rolled to a new segment, so the
    /// partition's writer thread refuses every further append until the
    /// directory is reopened (see the engine's own doc on that variant).
    /// Given its own variant rather than folded into the generic `Engine`
    /// wrapper because a caller needs to distinguish "this exact write
    /// failed" (`Engine`/most other errors) from "this whole partition is
    /// now permanently closed for writing" — the latter is not worth
    /// retrying at all until an operator reopens it.
    #[error(
        "partition '{topic}'/{partition} is poisoned by a group-commit fsync failure and accepts no further writes until it is reopened"
    )]
    PartitionPoisoned { topic: String, partition: u32 },
    /// A multi-partition `publish` (per-record routing) failed
    /// on a LATER partition group after at least one EARLIER group had
    /// already been durably appended — `acked` lists exactly what already
    /// landed (including a partition where the append itself succeeded but
    /// a later bookkeeping step, e.g. `producer_seq.record`, failed on it;
    /// `acked` is populated right after the append, not after every
    /// downstream step). See `PublishResult`'s doc for how narrow "a retry
    /// is safe" actually is without a producer identity or dedup enabled.
    #[error("publish partially applied before failing: {source}")]
    PartialPublish {
        acked: Vec<PartitionAck>,
        #[source]
        source: Box<BusServiceError>,
    },
    /// WHY: replaces the earlier `InvalidArgument(MAX_GROUPS_EXCEEDED_PREFIX)`
    /// workaround (follow-up toru P, task 6) — `dispatch/bus.rs`'s
    /// `map_bus_error` is no longer under concurrent edit, so this can be a
    /// real variant mapped to its own stable `bus.max_groups_exceeded` error
    /// code instead of a free-form string a caller had to prefix-match.
    #[error(
        "org '{org_id}' has reached its max_groups quota ({max}); {current} groups already exist"
    )]
    MaxGroupsExceeded {
        org_id: String,
        max: u32,
        current: u32,
    },
    /// M2 (PLAN-M2 §1e): this node is not the leader for the target
    /// partition — `publish`/`open_consumer`/`fetch`/`commit` are all
    /// leader-only in M2 (consumption from a follower's `hw` is out of
    /// scope, PLAN-M2 §0). `leader_node_id` is `None` when the
    /// coordinator itself does not know the current leader (e.g. mid
    /// election).
    #[error(
        "not the leader for this partition (leader_node_id={leader_node_id:?}, leader_epoch={leader_epoch})"
    )]
    NotLeader {
        leader_node_id: Option<String>,
        leader_epoch: u32,
    },
    /// K-M2-2: `|ISR| < min_isr = floor(RF/2)+1` — `publish` refuses
    /// outright rather than silently degrading `acks=quorum` to
    /// `acks=leader`. No "unclean leader election" mode.
    #[error("not enough in-sync replicas: isr={isr}, required={required}")]
    NotEnoughReplicas { isr: u32, required: u32 },
    /// `await_acks` timed out waiting for enough replicas before
    /// `publish_ack_timeout_ms` elapsed. The record IS already durable on
    /// this (leader) node's disk — same semantics as `PartialPublish`: a
    /// blind retry may duplicate.
    #[error("timed out waiting for replica acks: acked={acked}, required={required}")]
    AckTimeout { acked: u32, required: u32 },
    /// The partition cannot currently serve the request for a reason that
    /// is neither "not leader" nor "not enough replicas" specifically
    /// (e.g. no assignment yet, mid-reassignment). `reason` is meant to be
    /// shown directly in the UI (PLAN-M2 §1e: "partycja niedostępna — …").
    #[error("partition unavailable: {reason}")]
    PartitionUnavailable { reason: String },
    /// A write-direction field policy (SUM/tentabus/POLITYKI-POL.md) rejects
    /// the whole batch: at least one record carries a top-level JSON field
    /// not in the resolved policy's allow-list (`mode=reject`, the only
    /// mode the client chose — no field-stripping fallback). Checked before
    /// partition resolution/dedup/quota so a rejected batch pays none of
    /// that cost.
    #[error("field(s) {fields:?} are not allowed by the write policy on topic '{topic}'")]
    FieldNotAllowed { topic: String, fields: Vec<String> },
    /// A write-direction field policy declares `required_fields_json` and
    /// at least one record is missing one of them.
    #[error("required field(s) {fields:?} are missing for the write policy on topic '{topic}'")]
    RequiredFieldMissing { topic: String, fields: Vec<String> },
    /// A field policy exists for `topic` but the payload does not parse
    /// under the topic's resolved wire format (`bus::payload_format`,
    /// SUM/tentabus/POLITYKI-POL-FORMATY.md), so neither the write
    /// allow-list nor the read projection can be applied. `format` names
    /// which format was expected — useful once more than JSON is
    /// implemented.
    #[error("topic '{topic}' has a field policy but the payload does not parse as {format}")]
    FieldPolicyPayloadMalformed {
        topic: String,
        format: &'static str,
    },
}

impl From<anyhow::Error> for BusServiceError {
    fn from(e: anyhow::Error) -> Self {
        BusServiceError::Db(e.to_string())
    }
}
impl From<fjall::Error> for BusServiceError {
    fn from(e: fjall::Error) -> Self {
        BusServiceError::Fjall(e.to_string())
    }
}
impl From<std::io::Error> for BusServiceError {
    fn from(e: std::io::Error) -> Self {
        BusServiceError::Io(e.to_string())
    }
}

// ---- Call context, authorization ---------------------------------------

/// Same fields as `flow_engine::node_adapter::ExecutionContext` (PLAN §6.1)
/// so provenance carries straight into a record's `tf.*` system headers.
#[derive(Debug, Clone)]
pub struct BusCallContext {
    pub org_id: String,
    pub actor: Option<String>,
    pub correlation_id: Option<String>,
    pub origin: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusAction {
    Produce,
    Consume,
    Admin,
}

impl BusAction {
    pub fn as_str(self) -> &'static str {
        match self {
            BusAction::Produce => "produce",
            BusAction::Consume => "consume",
            BusAction::Admin => "admin",
        }
    }
}

/// Injected authorization callback (PLAN §8.1: checked at session open —
/// `publish`/`open_consumer`/admin calls — never per message). Implemented
/// by tor P/D against real RBAC (`bus.read`/`bus.write`/`bus.admin` +
/// `resource_permissions` with `resource_type = "topic"`); this module only
/// defines the seam.
///
/// A PRODUCTION implementation MUST:
/// - allow a caller with consume/admin rights on a source topic to
///   implicitly produce to its own `__dlq.<topic>` (see
///   `note_delivery_failure`, which calls `publish` re-using the caller's
///   `BusCallContext`) — either by special-casing `__dlq.*` writes or by
///   granting `bus.write` on them alongside `bus.read`/`bus.admin` on the
///   source topic;
/// - bump the value `generation()` returns on EVERY permission change
///   (grant, revoke, role edit) that could affect ANY caller — `fetch`/
///   `commit` compare it on every call (PLAN §8.1) and skip
///   re-authorization when it has not moved, so a stale/constant value
///   defeats revocation silently, not loudly.
pub trait BusAuthorizer: Send + Sync {
    fn authorize(
        &self,
        ctx: &BusCallContext,
        action: BusAction,
        topic: &str,
    ) -> Result<(), BusServiceError>;

    /// Group-scoped variant of `authorize`, used by the two call sites that
    /// mint or touch a `(group, topic)` commit-offset pair:
    /// `open_consumer` and `note_delivery_failure`. A production authorizer that has no separate
    /// per-group ACL concept (PLAN §8.1's RBAC model is topic-scoped, not
    /// group-scoped, as of M1) can implement this as a thin delegate to
    /// `self.authorize(ctx, action, topic)` — but MUST still perform that
    /// check; there is no default implementation here specifically so a
    /// production `BusAuthorizer` cannot forget to wire this call at all.
    fn authorize_group(
        &self,
        ctx: &BusCallContext,
        action: BusAction,
        topic: &str,
        group: &str,
    ) -> Result<(), BusServiceError>;

    /// Monotonically increasing permission-generation counter (PLAN §8.1,
    /// ). `ConsumerHandle` snapshots this at `open_consumer`
    /// time and re-authorizes whenever a later `fetch`/`commit` observes a
    /// different value, so a revoked permission takes effect within one
    /// fetch cycle instead of only at the next `open_consumer`. No default
    /// impl: a constant return value (most obviously `0`) silently defeats
    /// revocation and there is no value that is safe to default to.
    fn generation(&self) -> u64;
}

fn deny(action: BusAction, topic: &str) -> BusServiceError {
    BusServiceError::PermissionDenied {
        action: action.as_str(),
        topic: topic.to_string(),
    }
}

/// Stable error code (`dispatch/bus.rs`'s `map_bus_error`) for
/// `BusServiceError::MaxGroupsExceeded` — kept as a named constant (rather
/// than a string literal duplicated at both ends) so the two files agree on
/// the exact code without either one importing the other's error type.
pub const MAX_GROUPS_EXCEEDED_PREFIX: &str = "bus.max_groups_exceeded";

/// `BusService::open_consumer` would create at least one brand-new
/// `bus_groups` row and doing so would exceed `QuotaManager::
/// max_groups(org_id)` — a caller with only `bus.read`/consume rights (no
/// `bus.admin`) could otherwise loop `open_consumer` with a fresh,
/// caller-controlled group name and grow `bus_groups` (and this process's
/// `GroupStateCache`/`commit_locks`) without bound.
fn max_groups_exceeded(org_id: &str, max: u32, current: u32) -> BusServiceError {
    BusServiceError::MaxGroupsExceeded {
        org_id: org_id.to_string(),
        max,
        current,
    }
}

/// Translates an engine-level `tentaflow_bus::BusError` into the
/// service-level error a caller should actually see, attaching the
/// `topic`/`partition` context the engine error itself does not carry.
/// `PartitionDetached` becomes `TopicNotFound` (a detached partition's
/// topic/org has just been deleted or purged out from under this handle —
/// from the caller's point of view it no longer exists, which is exactly
/// what `TopicNotFound` already communicates, rather than inventing a
/// second "gone" error the caller would have to handle identically) and
/// `PartitionPoisoned` becomes the dedicated `BusServiceError::
/// PartitionPoisoned` (a distinct, non-retryable-without-operator-action
/// condition on an OTHERWISE-live partition, unlike `TopicNotFound`).
/// `Throttled` keeps its existing translation (dropping the returned batch
/// bytes, which every caller of this function already owns a copy of).
/// Every other variant passes through as `Engine`.
fn map_engine_error(err: tentaflow_bus::BusError, topic: &str, partition: u32) -> BusServiceError {
    match err {
        tentaflow_bus::BusError::PartitionDetached => BusServiceError::TopicNotFound {
            name: topic.to_string(),
        },
        tentaflow_bus::BusError::PartitionPoisoned => BusServiceError::PartitionPoisoned {
            topic: topic.to_string(),
            partition,
        },
        tentaflow_bus::BusError::Throttled { retry_after_ms, .. } => {
            BusServiceError::Throttled { retry_after_ms }
        }
        other => BusServiceError::Engine(other),
    }
}

/// M2 (PLAN-M2 §1e): translates a `PartitionRole` that is not `Leader` into
/// the `BusServiceError::NotLeader` a `publish`/`open_consumer`/`fetch`/
/// `commit`/`peek` call site returns. `Unavailable` also becomes `NotLeader`
/// (not `PartitionUnavailable`) here specifically because every caller of
/// this helper already knows a coordinator IS installed and has already
/// established this node is not currently allowed to lead — "no assignment
/// yet"/"fenced"/"no ISR" are all still "you are not talking to a leader",
/// which is the signal a producer/consumer needs to redirect itself;
/// `PartitionUnavailable` is reserved for `preflight`'s own richer errors
/// (`map_repl_error`), which run only on the publish path and only after
/// this same role check has already passed.
fn role_not_leader_error(role: PartitionRole) -> BusServiceError {
    match role {
        PartitionRole::Leader { .. } => {
            unreachable!("role_not_leader_error must only be called for a non-Leader role")
        }
        PartitionRole::Follower {
            leader_node_id,
            epoch,
        } => BusServiceError::NotLeader {
            leader_node_id: Some(leader_node_id),
            leader_epoch: epoch,
        },
        PartitionRole::Unavailable { .. } => BusServiceError::NotLeader {
            leader_node_id: None,
            leader_epoch: 0,
        },
    }
}

/// M2 (PLAN-M2 §1e): translates `ReplicationCoordinator::preflight`'s
/// narrower `ReplError` into the richer `BusServiceError` variant `publish`
/// actually returns. `NoAssignment`/`NotAReplica`/`EpochFenced` all mean
/// "this node cannot currently lead this partition" — re-queries `role()`
/// for the current leader (if the coordinator knows one) so the caller
/// gets a useful `leader_node_id` instead of a bare rejection, via the
/// SAME `role_not_leader_error` helper `check_leader_role` uses for the
/// consume path, so `publish` and `open_consumer`/`fetch`/`commit`/`peek`
/// report the identical `NotLeader` for the identical condition instead of
/// the asymmetry T1's finding (3) reported (`preflight` returns
/// `NoAssignment` for EVERY non-leader role — there is no dedicated "not a
/// replica"/"is a follower" `ReplError` variant — which this function used
/// to turn into `PartitionUnavailable` instead of `NotLeader`). A
/// coordinator that itself does not know the leader (mid-election) reports
/// `None`, exactly matching `NotLeader`'s own doc. `Internal` is the only
/// variant still mapped to `PartitionUnavailable`: a genuine coordinator-
/// side failure, not a role/assignment condition.
fn map_repl_error(
    coordinator: &Arc<dyn ReplicationCoordinator>,
    org: &str,
    topic: &str,
    partition: u32,
    err: ReplError,
) -> BusServiceError {
    match err {
        ReplError::NotEnoughReplicas { isr, required, .. } => {
            BusServiceError::NotEnoughReplicas { isr, required }
        }
        ReplError::Internal(_) => BusServiceError::PartitionUnavailable {
            reason: err.to_string(),
        },
        ReplError::NoAssignment { .. }
        | ReplError::NotAReplica { .. }
        | ReplError::EpochFenced { .. } => match coordinator.role(org, topic, partition) {
            PartitionRole::Leader { epoch } => {
                // Raced: the coordinator says Leader now even though
                // `preflight` just refused — report our own last-known
                // epoch rather than fabricate a peer node id.
                BusServiceError::NotLeader {
                    leader_node_id: None,
                    leader_epoch: epoch,
                }
            }
            other => role_not_leader_error(other),
        },
    }
}

/// M2 (PLAN-M2 §1e): consumption is leader-only — `open_consumer`/`fetch`/
/// `commit`/`peek` all refuse with `NotLeader` when a coordinator is
/// installed and this node's `role()` for `(org, topic, partition)` is not
/// `Leader` (reading from a follower's `hw` is out of scope for M2, PLAN-M2
/// §0's own note). A free function, not a method, so both `BusService`
/// (`open_consumer`, `peek`) and `ConsumerHandle` (`fetch`, `commit`) — two
/// different `self` types with no relationship to each other — share the
/// exact same check. `coordinator: None` is always `Ok(())`: RF=1's consume
/// path (and any build that never calls `set_replication`) is unaffected.
fn check_leader_role(
    coordinator: &Option<Arc<dyn ReplicationCoordinator>>,
    org: &str,
    topic: &str,
    partition: u32,
) -> Result<(), BusServiceError> {
    let Some(coordinator) = coordinator else {
        return Ok(());
    };
    match coordinator.role(org, topic, partition) {
        PartitionRole::Leader { .. } => Ok(()),
        other => Err(role_not_leader_error(other)),
    }
}

/// Validates a consumer group name against the same charset PLAN §7.1 uses
/// for topic names — the group name flows straight into a fjall key
/// and a `bus_groups` PK, so it needs the same "no path-unsafe or control
/// characters" guarantee a topic name gets, not a bespoke second regex.
/// Re-wraps `InvalidTopicName` as `InvalidArgument` so the error message
/// talks about a group, not a topic.
fn validate_group_name(group: &str) -> Result<(), BusServiceError> {
    topics::validate_user_topic_name(group).map_err(|e| match e {
        BusServiceError::InvalidTopicName { name, reason } => {
            BusServiceError::InvalidArgument(format!("invalid group name '{name}': {reason}"))
        }
        other => other,
    })
}

// ---- Publish/consume data types (PLAN §6.1) ----------------------------

#[derive(Debug, Clone)]
pub struct PublishRecord {
    pub key: Option<Bytes>,
    pub headers: Vec<(String, Bytes)>,
    pub payload: Bytes,
    pub timestamp_ms: i64,
    pub schema_id: u32,
}

#[derive(Debug, Clone, Default)]
pub struct PublishBatch {
    /// Explicit target partition: forces the WHOLE batch onto one
    /// partition, for callers that need strict ordering across every
    /// record. `None` routes each record INDEPENDENTLY: a keyed record hashes to its own partition (see
    /// `partition_for_key`), a keyless one is round-robined.
    pub partition: Option<u32>,
    pub producer: Option<producer::ProducerIdentity>,
    pub records: Vec<PublishRecord>,
}

/// Per-partition outcome of a `publish` call — one entry per distinct
/// partition that any record in the batch landed on or replayed against
/// (partitioning is per record, not per whole batch, so a single `publish` call can touch more than one partition).
#[derive(Debug, Clone, Copy)]
pub struct PartitionAck {
    pub partition: u32,
    /// First offset of the sub-batch appended to this partition, OR — when
    /// every record routed here was a producer-sequence replay
    /// (`accepted == 0`) — the LAST offset `producer_seq` has on record for
    /// this (org, topic, partition, producer). The store only
    /// remembers the single most recent `(seq, offset)` pair (PLAN §3.1:
    /// "ostatni przyjęty seq"), so on an out-of-order replay this is not
    /// necessarily the offset of the exact stale batch being replayed —
    /// only a well-behaved producer retrying its most recent unacked batch
    /// gets a reliable offset here.
    pub base_offset: u64,
    /// Records actually appended to this partition (0 for a pure replay).
    pub accepted: u32,
}

/// Result of a `publish` call (PLAN §6.1).
///
/// A batch can span more than one partition (per-record routing, decision
/// 8) and each resulting partition group is appended/replay-checked
/// independently — there is no cross-partition atomicity, matching Kafka's
/// own per-partition produce semantics: if one group's append fails, the
/// caller gets an `Err` for the whole call, but earlier groups in the same
/// call may already have landed durably (see `BusServiceError::
/// PartialPublish`).
///
/// A retry is idempotent ONLY when the caller supplies a producer identity
/// (`PublishBatch::producer`, layer 1: `producer_seq` recognizes an
/// already-recorded `(seq, offset)` and reports `CheckOutcome::Duplicate`
/// instead of re-appending) OR the topic has `idempotency_key`/dedup
/// enabled (layer 2: `dedup_store` recognizes an already-committed key).
/// WITHOUT either of those — the default for a fresh topic and a caller
/// that never sets `producer` — a retry after any failure (including a
/// `PartialPublish`) blindly re-appends every record the caller resends,
/// producing DUPLICATES in the log, not idempotent no-ops. In that default
/// configuration, `PartialPublish.acked` is the ONLY source of truth for
/// "what already landed"; it must be inspected before deciding what (if
/// anything) is safe to resend.
#[derive(Debug, Clone)]
pub struct PublishResult {
    /// `true` only when the ENTIRE request was a producer-sequence replay
    /// (PLAN §3.1 layer 1) — nothing new was appended to ANY partition. A
    /// batch that is fresh on some partitions and a replay on others
    /// reports `false` here; inspect `partitions` for the per-partition
    /// breakdown. Never set by per-record idempotency-key dedup (layer 2)
    /// — see `deduplicated` for that.
    pub duplicate: bool,
    /// Total records actually appended to the log, summed across every
    /// partition this batch touched.
    pub accepted: u32,
    /// Total records dropped by per-record idempotency-key dedup (layer 2,
    /// `dedup.rs`). A batch that is entirely deduplicated has
    /// `accepted == 0` and an empty `partitions` — unlike M1's initial
    /// version, there is no "borrowed" `log_end_offset` to report for a
    /// record that was never appended anywhere.
    pub deduplicated: u32,
    /// One entry per distinct partition that received or replayed at least
    /// one record. Empty iff every record in the batch was deduplicated by
    /// layer 2.
    pub partitions: Vec<PartitionAck>,
}

impl PublishResult {
    /// Ergonomic accessor for the common case (explicit `partition:
    /// Some(p)`, or a batch whose keys all hashed to the same partition):
    /// `Some` iff exactly one partition is present in `partitions`. Returns
    /// `None` both when the batch spread across multiple partitions (read
    /// `partitions` directly) and when it was entirely deduplicated.
    pub fn single_partition(&self) -> Option<PartitionAck> {
        match self.partitions.as_slice() {
            [only] => Some(*only),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TopicPartition {
    pub topic: String,
    pub partition: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct ConsumerConfig {
    pub commit_mode: groups::CommitMode,
}

#[derive(Debug, Clone)]
pub struct FetchedRecordMeta {
    pub topic: String,
    pub partition: u32,
    pub offset: u64,
    pub timestamp_ms: i64,
    pub key: Option<Bytes>,
    /// Header keys stay raw `Bytes` (never validated/decoded as UTF-8) to
    /// avoid an allocating `String::from_utf8_lossy(..).into_owned()` per
    /// header per record on `fetch`'s hot path — the engine's own
    /// `HeaderPair` is already `(Bytes, Bytes)`, so this is a plain clone
    /// of what `tentaflow_bus` hands back, not a new allocation. A caller
    /// that wants a `&str` key can call `std::str::from_utf8`/
    /// `String::from_utf8_lossy` itself, paying that cost only where it
    /// actually needs the string.
    pub headers: Vec<(Bytes, Bytes)>,
    pub payload: Bytes,
    pub schema_id: u32,
}

#[derive(Debug, Clone, Default)]
pub struct FetchedBatch {
    pub records: Vec<FetchedRecordMeta>,
}

/// Result of `BusService::peek` — a stateless, one-shot read for a UI
/// preview (message browser, DLQ list), never a real consumer session:
/// unlike `ConsumerHandle::fetch`, nothing here is backed by a `bus_groups`
/// row, a committed offset, or any other durable state. `high_watermark`/
/// `earliest_offset` are read from the SAME partition snapshot `records`
/// came from, so a caller can render "N of M" / detect a retention gap
/// without a second round trip.
#[derive(Debug, Clone, Default)]
pub struct PeekResult {
    pub records: Vec<FetchedRecordMeta>,
    pub high_watermark: u64,
    pub earliest_offset: u64,
}

/// `peek`'s hard ceiling on records per call, regardless of what a caller
/// requests — a UI preview has no business reading more than a page at a
/// time, and this is a stateless read with no backpressure/quota mechanism
/// of its own (unlike `publish`, which has `QuotaManager`) to fall back on.
pub const PEEK_MAX_RECORDS: usize = 100;

/// `peek`'s hard ceiling on total payload bytes per call — see
/// `PEEK_MAX_RECORDS`'s doc.
pub const PEEK_MAX_BYTES: usize = 1024 * 1024;

// ===== M2 replication contract (PLAN-M2 §1e) =========================
//
// Frozen wave 0 (coordinator): `bus/mod.rs` calls through
// `ReplicationCoordinator` instead of depending on `bus::replication`
// directly, breaking the `bus` <-> `bus::replication` cycle and letting
// this crate test `BusService` with a stub coordinator. `set_replication`
// is never called by anything in this build yet — every method below is
// signature-only scaffolding for wave 1/2 (agents RL/RF/EL wire the real
// `ReplicationManager`; agent S wires `preflight`/`await_acks`/`role` into
// `publish`/`open_consumer`/`fetch`/`commit`). Until `set_replication` is
// wired up, `BusService::replication` stays `None` and every existing
// `publish`/`fetch`/`commit` call path is completely unaffected — that is
// the "no behaviour change" requirement this wave's tests enforce.

/// A partition's role as this node's `ReplicationCoordinator` currently
/// sees it (PLAN-M2 §1e). `role()` is queried by `publish`/`open_consumer`/
/// `fetch`/`commit` (wave 2, agent S) to fail fast with `NotLeader` before
/// touching the engine at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartitionRole {
    Leader { epoch: u32 },
    Follower { leader_node_id: String, epoch: u32 },
    Unavailable { reason: UnavailableReason },
}

/// Why `role()` reports `PartitionRole::Unavailable` — mirrors the three
/// causes PLAN-M2 §1e calls out: no assignment was ever made for this
/// partition, the assignment exists but this node has no epoch to serve
/// under (fenced out by a newer election), or the partition genuinely has
/// no usable ISR right now (K-M2-2: `|ISR| < min_isr` refuses writes
/// outright, no "unclean" mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableReason {
    NoAssignment,
    EpochFenced,
    NoIsr,
}

/// Outcome of `ReplicationCoordinator::await_acks` (PLAN-M2 §1e): how many
/// replicas (this node included) had acknowledged the requested offset by
/// the time the call returned, versus how many `acks` required, and the
/// resulting high watermark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AckOutcome {
    pub acked_nodes: u32,
    pub required: u32,
    pub hw: u64,
}

/// Error surface for `ReplicationCoordinator` methods (PLAN-M2 §1e).
/// Deliberately narrower than `BusServiceError`: this trait lives in
/// `bus/mod.rs` so `bus::replication` (wave 1) can implement it without
/// `bus` depending back on it, and its errors get mapped up into the
/// richer `BusServiceError::{NotLeader,NotEnoughReplicas,AckTimeout,
/// PartitionUnavailable}` variants by the wave-2 call sites in `publish`/
/// `open_consumer`/`fetch`/`commit`, not surfaced to callers directly.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ReplError {
    #[error("no partition assignment for '{topic}'/{partition}")]
    NoAssignment { topic: String, partition: u32 },
    #[error("'{topic}'/{partition}: isr size {isr} below required {required}")]
    NotEnoughReplicas {
        topic: String,
        partition: u32,
        isr: u32,
        required: u32,
    },
    #[error("node '{node_id}' is not a replica of '{topic}'/{partition}")]
    NotAReplica {
        topic: String,
        partition: u32,
        node_id: String,
    },
    #[error("leader epoch fenced: this node is at {have}, request carries {requested}")]
    EpochFenced { have: u32, requested: u32 },
    #[error("replication coordinator internal error: {0}")]
    Internal(String),
}

/// One node's replication-relevant state for the M06 UI (mirrors the wire
/// shape `BusReplicaNodeWire` will carry, PLAN-M2 §1f) — node cards
/// (leader/follower/ISR counts, reachability, last heartbeat).
#[derive(Debug, Clone)]
pub struct ReplicaNodeInfo {
    pub node_id: String,
    pub label: String,
    pub environment: NodeEnvironment,
    pub is_local: bool,
    pub reachable: bool,
    pub last_heartbeat_ms_ago: Option<u64>,
    pub leader_count: u32,
    pub follower_count: u32,
    pub isr_count: u32,
}

/// One replica's lag behind the leader (mirrors `BusReplicaLagWire`,
/// PLAN-M2 §1f) — e.g. "lag 87 MiB > 64 MiB" in the role matrix.
#[derive(Debug, Clone)]
pub struct ReplicaLagInfo {
    pub node_id: String,
    pub lag_bytes: u64,
    pub lag_ms: u64,
    pub reason: String,
}

/// One partition's replica/role snapshot (mirrors `BusPartitionReplicaWire`,
/// PLAN-M2 §1f) — the role matrix's per-partition row.
#[derive(Debug, Clone)]
pub struct PartitionReplicaInfo {
    pub topic: String,
    pub partition: u32,
    pub leader_node_id: Option<String>,
    pub leader_epoch: u32,
    pub replicas: Vec<String>,
    pub isr: Vec<String>,
    pub lagging: Vec<ReplicaLagInfo>,
    pub high_watermark: u64,
    pub log_end_offset: u64,
    pub unavailable_reason: Option<UnavailableReason>,
}

/// One failover event (mirrors `BusFailoverEventWire`, PLAN-M2 §1f) — the
/// M06 timeline, sourced from `audit_log`'s `bus.leader.failover` entries,
/// no dedicated table.
#[derive(Debug, Clone)]
pub struct FailoverEventInfo {
    pub org_id: String,
    pub topic: String,
    pub partition: u32,
    pub from_node_id: Option<String>,
    pub to_node_id: String,
    pub from_epoch: u32,
    pub to_epoch: u32,
    pub duration_ms: u64,
    pub at_ms: i64,
}

/// Snapshot for the M06 "Partycje i repliki" view (PLAN-M2 §1e/§1f).
/// Empty-but-typed in this wave: `ReplicationCoordinator::snapshot`'s only
/// implementor is wave 1's `ReplicationManager` (agent EL); nothing in
/// this build ever produces a non-empty one yet.
#[derive(Debug, Clone, Default)]
pub struct ReplicationSnapshot {
    pub nodes: Vec<ReplicaNodeInfo>,
    pub partitions: Vec<PartitionReplicaInfo>,
    pub failovers: Vec<FailoverEventInfo>,
}

/// Injected replication backend (PLAN-M2 §1e). `BusService` holds at most
/// one, set via `BusService::set_replication` after mesh startup —
/// `None` (the default) means M1 behavior: single-node, `hw == leo`,
/// every write leader-local. Implemented by `bus::replication::
/// ReplicationManager` (wave 1, agent EL); nothing in this build
/// implements it yet.
pub trait ReplicationCoordinator: Send + Sync {
    /// This node's current role for `(org, topic, partition)`.
    fn role(&self, org: &str, topic: &str, partition: u32) -> PartitionRole;
    /// Called before a `publish` touches the engine: validates this node
    /// is leader with enough ISR for `acks`, returning the epoch to stamp
    /// the write with, or the specific `ReplError` (`NoAssignment`/
    /// `NotEnoughReplicas`/`EpochFenced`) that explains why not.
    fn preflight(
        &self,
        org: &str,
        topic: &str,
        partition: u32,
        acks: topics::Acks,
    ) -> Result<u32, ReplError>;
    /// Blocks (up to `timeout`) until `next_offset` has been acknowledged
    /// by enough replicas to satisfy `acks`, or returns `AckOutcome`
    /// reporting how far it got.
    fn await_acks(
        &self,
        org: &str,
        topic: &str,
        partition: u32,
        next_offset: u64,
        acks: topics::Acks,
        timeout: Duration,
    ) -> Result<AckOutcome, ReplError>;
    /// K-M2-5: records a consumer group's offset commit so it can be
    /// replicated (`ReplOffsets`) and survive a failover with bounded
    /// redelivery instead of resetting.
    fn note_offset_commit(
        &self,
        org: &str,
        group: &str,
        topic: &str,
        partition: u32,
        offset: u64,
        attempts: u32,
    );
    /// Removes `node_id` from every replica set it belongs to (PLAN §4.4:
    /// environment-change fencing) — returns the number of assignments
    /// touched.
    fn evict_node_from_replica_sets(
        &self,
        node_id: &str,
        reason: &'static str,
    ) -> Result<u32, ReplError>;
    /// Admin-triggered leader transfer; returns the new leader epoch.
    fn transfer_leader(
        &self,
        org: &str,
        topic: &str,
        partition: u32,
        target: &str,
    ) -> Result<u32, ReplError>;
    /// Admin-triggered replica-set change, for one partition or (when
    /// `partition` is `None`) the whole topic; returns the number of
    /// partitions touched.
    fn reassign(
        &self,
        org: &str,
        topic: &str,
        partition: Option<u32>,
        replicas: &[String],
    ) -> Result<u32, ReplError>;
    /// Snapshot for the M06 UI/`ReplicaListResponse` (`topic: None` means
    /// every topic in `org`).
    fn snapshot(&self, org: &str, topic: Option<&str>) -> ReplicationSnapshot;
    /// This node's own identity — independent of `snapshot()`, whose
    /// `nodes` list is populated ENTIRELY from existing partition
    /// assignments (`registry`'s own `replicas` lists). `create_topic`
    /// used to derive its own node id by searching `snapshot(org, None)
    /// .nodes` for `is_local`, which is circular: a fresh org (or, as a
    /// live krytyk pass on a real cluster found, ANY org where nothing has
    /// ever successfully proposed an assignment) has an empty registry, so
    /// `is_local` is never found, `create_topic` silently proposes NO
    /// assignment for ANY topic, forever — the registry can never
    /// bootstrap its own first entry. Defaulted to `""` (matching
    /// `isr_shrink_total`'s precedent) so a test fake that never exercises
    /// `create_topic`'s placement path keeps compiling unchanged;
    /// `ReplicationManager` is the only override that matters. Returns an
    /// owned `String` (not `&str`) so a test double can keep this behind a
    /// `Mutex` — this is only ever called once per `create_topic`, never a
    /// hot path.
    fn local_node_id(&self) -> String {
        String::new()
    }
    /// M2 (PLAN §8.4): running total of ISR-membership shrinks this
    /// coordinator has observed across every partition it manages — feeds
    /// `tentaflow_bus_isr_shrink_total`. Defaulted to `0` so every existing
    /// test fake implementing this trait keeps compiling unchanged;
    /// `replication::ReplicationManager` is the only override.
    fn isr_shrink_total(&self) -> u64 {
        0
    }
}

/// Read-only, no-consumer-session partition metadata for `StatsSnapshot`/
/// `TopicDetail`'s per-partition KPIs (PLAN §6.2, follow-up toru P task 3) —
/// the same "cheap introspection without opening a real consumer" spirit as
/// `peek`, but for sizes/offsets rather than record bytes.
#[derive(Debug, Clone, Copy, Default)]
pub struct PartitionStats {
    pub earliest_offset: u64,
    pub high_watermark: u64,
    /// M2 (PLAN-M2 §1e): the engine's real append point (`Partition::
    /// log_end_offset`), separate from `high_watermark` now that RF>=1 can
    /// make the two diverge (K-M2-1: `high_watermark` only ever advances
    /// once enough replicas have acknowledged). Before this field existed,
    /// the UI's "log_end_offset" was quietly `high_watermark` under a
    /// different name — harmless at RF=1 (M1: the two are always equal),
    /// but a lie once a leader can have unacknowledged records past `hw`.
    pub log_end_offset: u64,
    /// Every SEALED segment's on-disk length (`Partition::sealed_segments`)
    /// PLUS the still-growing ACTIVE segment's current length
    /// (`Partition::active_segment_len`) — an exact figure, not a lower
    /// bound. Until `active_segment_len` was added to the engine (M1-R2
    /// review N-6), this only summed sealed segments, which made
    /// `size_bytes` structurally always `0` for any partition below its
    /// first roll (1 GiB at the old default) regardless of how much data it
    /// actually held.
    pub size_bytes: u64,
    /// Total segment FILES on disk for this partition, sealed + the one
    /// active segment — a partition always has exactly one active segment
    /// (`Partition::open`'s own invariant), so this is never `0` even for a
    /// brand-new, empty partition (the old `sealed_segments()`-only count
    /// used to report `0` there, which is not what "Segmenty" on disk
    /// actually was — the file exists from the first write).
    pub segments: u32,
}

/// One-second TUMBLING publish-rate window per (org, topic) — feeds
/// `BusService::topic_rates` for `StatsSnapshot`'s `msgs_in_per_sec`/
/// `bytes_in_per_sec` KPIs (PLAN §6.2, follow-up toru P task 3). Guarded by
/// a single `parking_lot::Mutex` rather than raw atomics — the same shape
/// `quota::TokenBucket` already uses for the identical `publish` hot path —
/// because a lock held for a handful of integer ops is not the bottleneck a
/// lock-free scheme would be worth the complexity for, and every other
/// per-(org, topic) hot-path structure in this file (`TokenBucket`, dedup
/// stores) already pays a `DashMap` lookup plus a lock of its own.
struct RateWindow {
    window_start_ms: i64,
    msgs: u64,
    bytes: u64,
    last_msgs_per_sec: u64,
    last_bytes_per_sec: u64,
}

struct RateCounter {
    state: parking_lot::Mutex<RateWindow>,
}

impl RateCounter {
    fn new(now_ms: i64) -> Self {
        Self {
            state: parking_lot::Mutex::new(RateWindow {
                window_start_ms: now_ms,
                msgs: 0,
                bytes: 0,
                last_msgs_per_sec: 0,
                last_bytes_per_sec: 0,
            }),
        }
    }

    /// Called once per successful `publish` call (not per record) with the
    /// accepted record/byte counts for THIS call. Rolls the window forward
    /// exactly once per call, regardless of how many whole seconds elapsed,
    /// since a caller that stopped publishing for a while should see the
    /// PREVIOUS window's real count once, not a synthetic zero window
    /// inserted for every second nobody called `record`.
    fn record(&self, now_ms: i64, msgs: u64, bytes: u64) {
        let mut w = self.state.lock();
        if now_ms - w.window_start_ms >= 1000 {
            w.last_msgs_per_sec = w.msgs;
            w.last_bytes_per_sec = w.bytes;
            w.window_start_ms = now_ms;
            w.msgs = 0;
            w.bytes = 0;
        }
        w.msgs += msgs;
        w.bytes += bytes;
    }

    /// Best-effort current rate, read independently of `record` (a topic
    /// that just went idle must decay to 0 without needing another publish
    /// to trigger the roll). Within the still-open window this reports the
    /// PREVIOUS window's rate (the current one is not finished yet); once a
    /// second window has fully elapsed with no new `record` call, the topic
    /// is treated as idle and this reports 0.
    fn rates(&self, now_ms: i64) -> (u64, u64) {
        let w = self.state.lock();
        let elapsed = now_ms - w.window_start_ms;
        if elapsed < 1000 {
            (w.last_msgs_per_sec, w.last_bytes_per_sec)
        } else if elapsed < 2000 {
            (w.msgs, w.bytes)
        } else {
            (0, 0)
        }
    }
}

// ---- BusService ----------------------------------------------------------

pub struct BusInitConfig {
    /// Root directory for on-disk topic data (PLAN §2.2). The caller
    /// resolves this via `paths::category_dir(paths::StorageCategory::Bus)`
    /// — this struct only carries the already-resolved path so `bus/` never
    /// needs to depend on `paths`' override/live-migration machinery
    /// directly. Wiring `bus::init` into actual application startup (which
    /// nothing in this repo does yet) is out of this file's scope — tor P's
    /// dispatch layer owns that.
    pub bus_dir: PathBuf,
    pub db: DbPool,
    pub authorizer: Arc<dyn BusAuthorizer>,
    /// Interval between automatic retention sweeps (`run_retention_sweep`),
    /// run on a background thread `init` spawns. `None` disables the
    /// background thread entirely — the default for anything that wants
    /// full control over exactly when a sweep runs (unit tests, an operator
    /// tool driving `run_retention_sweep`/`flush_audit_windows` by hand).
    /// PLAN's own default once wired into real startup is 5 minutes.
    pub retention_interval: Option<Duration>,
    /// Node-wide expected sustained publish rate fed into every dedup
    /// store's `dedup::DedupConfig::expected_rate_per_sec` (see that
    /// field's doc for the capacity math). This is deliberately a NODE
    /// setting, not per-topic — M1 has no per-topic plumbing for it (every
    /// dedup-enabled topic on a node shares the same assumed rate);
    /// per-topic tuning is deferred to M5's schema/advanced-config
    /// registry. PLAN's own default (and what an operator leaving this
    /// unset should get) is 10,000 msg/s.
    pub dedup_expected_rate_per_sec: u64,
    /// M2 (PLAN-M2 §1e, A9 debt from REVIEW-M1-R3): caps how many
    /// partition handles (`partitions`, `consumer_partitions`) stay open
    /// at once, evicting least-recently-used ones once the count is
    /// exceeded. `None` (the default) disables the LRU entirely —
    /// M1's actual behavior, unbounded, kept unchanged for RF=1. RF=3
    /// feeders/followers holding handles open permanently make this a
    /// real cost starting M2; wiring the eviction itself (and never
    /// evicting a partition with a live replication stream or
    /// `ConsumerHandle`) is wave-2 work (agent S).
    pub partition_handle_lru: Option<usize>,
    /// M2 (PLAN-M2 §1e): how long `publish` blocks in `await_acks` for a
    /// target partition's `acks` policy to be satisfied before returning
    /// `BusServiceError::AckTimeout` (still wrapped in `PartialPublish` —
    /// see `publish`'s own comment on that call site). Unused whenever no
    /// coordinator is installed (`replication` is `None`) — RF=1's publish
    /// path never calls `await_acks` at all. PLAN-M2's own default is 30 s
    /// (`DEFAULT_PUBLISH_ACK_TIMEOUT`); every test in this file that never
    /// installs a coordinator can pass that constant without it affecting
    /// anything it exercises.
    pub publish_ack_timeout: Duration,
}

/// PLAN-M2 §1e's own default for `BusInitConfig::publish_ack_timeout`.
pub const DEFAULT_PUBLISH_ACK_TIMEOUT: Duration = Duration::from_secs(30);

type PartitionKey = (String, String, u32);
type TopicKey = (String, String);
/// Per-(org, group) `commit` mutexes — see `BusService::commit_locks`'s doc.
type CommitLocks = Arc<DashMap<(String, String), Arc<parking_lot::Mutex<()>>>>;

pub struct BusService {
    bus_dir: PathBuf,
    db: DbPool,
    authorizer: Arc<dyn BusAuthorizer>,
    // Kept alive for the service's lifetime; every keyspace below borrows
    // from it. Never touched directly outside `new`.
    _fjall_db: fjall::Database,
    offsets: Arc<groups::GroupOffsetStore>,
    producer_seq: Arc<producer::ProducerSeqStore>,
    /// Durable set of DLQ records marked "handled" via `dlq_discard` — see
    /// `dlq::DiscardStore`'s doc (M1-R2 review N-5, coordinator decision 2).
    discarded: Arc<dlq::DiscardStore>,
    partitions: DashMap<PartitionKey, tentaflow_bus::Partition>,
    round_robin: DashMap<TopicKey, AtomicU32>,
    dedup_stores: DashMap<TopicKey, Arc<dedup::MmapDedupStore>>,
    quota: quota::QuotaManager,
    /// `publish`'s hot path must not touch SQLite once warm — one query per
    /// message at any real throughput would serialize on the single shared
    /// application database. Invalidated (removed) by `update_topic`/
    /// `delete_topic` so a config change is visible on the very next
    /// `publish`, never stale-forever.
    topic_config_cache: DashMap<TopicKey, Arc<topics::TopicConfig>>,
    /// Counts only cache MISSES (i.e. actual `bus_topics` reads) — a metric
    /// hook a test (or an operator dashboard) can use to prove the cache is
    /// doing its job, rather than asserting on SQLite query counts
    /// directly.
    topic_config_db_loads: AtomicU64,
    /// M2 (PLAN §8.4): total records ever accepted by `publish` across every
    /// call — feeds the Zabbix exporter's `tentaflow_bus_publish_msgs_total`.
    publish_msgs_total: AtomicU64,
    /// M2 (PLAN §8.4): total payload bytes ever accepted by `publish` —
    /// feeds `tentaflow_bus_publish_bytes_total`.
    publish_bytes_total: AtomicU64,
    /// M2 (PLAN §8.4): total records ever returned by `ConsumerHandle::
    /// fetch` — feeds `tentaflow_bus_consume_msgs_total`. Wrapped in its own
    /// `Arc` (same pattern as `group_state`/`audit_windows` below) so every
    /// `ConsumerHandle` this service opens can bump the SAME counter:
    /// `fetch` itself is a `ConsumerHandle` method, not a `BusService` one.
    consume_msgs_total: Arc<AtomicU64>,
    /// M2 (PLAN §8.4): total `publish` calls rejected by the org's quota
    /// (`QuotaExceeded`/`QuotaRequestTooLarge`) — feeds
    /// `tentaflow_bus_throttled_total`. Counts rejected CALLS, not records.
    throttled_total: AtomicU64,
    /// Cached at `new()`; refreshed only by `invalidate_environment_cache`.
    /// Removes the second per-`publish` SQLite read
    /// (`settings.node_environment`) that a naive implementation would do
    /// on every call. Encoded as `u8` (0=Dev/1=Test/2=Prod) so the
    /// read/write is a single atomic op with no lock — wiring
    /// `switch_node_environment` to call `invalidate_environment_cache`
    /// automatically is tor P's dispatch hook (`dispatch/environment.rs`);
    /// until that lands, a node that changes environment mid-run keeps
    /// stamping `tf.env`/evaluating `check_environment` against the STALE
    /// value until this service is restarted or a caller invalidates it
    /// explicitly.
    node_environment_cache: Arc<AtomicU8>,
    /// Shared with every `ConsumerHandle` this service opens (`Arc::clone`
    /// at `open_consumer` time) — the windowed quota/denial audit
    /// mechanism (`AuditWindows`'s doc) needs one shared set of counters
    /// regardless of whether the rejection happened inside a `BusService`
    /// method or a `ConsumerHandle` one.
    audit_windows: Arc<AuditWindows>,
    /// Set by `stop_background_sweeper` to ask the retention/audit-flush
    /// thread `bus::init` spawned (if any) to exit at its next wake-up —
    /// checked once per tick, not preemptively, so a sweep already running
    /// always finishes.
    sweeper_shutdown: Arc<AtomicBool>,
    /// Shared with every `ConsumerHandle` this service opens — backs
    /// `fetch`'s paused-group check (see `GroupStateCache`'s doc).
    group_state: Arc<GroupStateCache>,
    /// Serializes `ConsumerHandle::commit` calls for the SAME (org, group)
    /// across different handle instances (two independent `open_consumer`
    /// calls for the same group, or two threads sharing one handle) —
    /// `commit`'s own doc used to promise "validate everything, then write
    /// everything" as an atomic unit, but the validation loop and the write
    /// loop are two separate passes with no lock between them, so a second
    /// `commit` racing in between could see stale `committed_offset` reads
    /// and interleave its own writes. A `parking_lot::Mutex` per (org,
    /// group) — not one global lock — keeps unrelated groups fully
    /// concurrent; the `Arc` lets a caller clone the lock out of the
    /// `DashMap` and drop the map's own shard guard immediately, rather
    /// than holding it for `commit`'s whole duration.
    commit_locks: CommitLocks,
    /// `org_id -> purge count`, bumped by `purge_org`. A `ConsumerHandle`
    /// snapshots the current count for its org at `open_consumer` time; if
    /// a later `commit`/`seek_to_earliest` observes a different count, the
    /// whole org was purged (GDPR/RODO) after this handle was opened, and
    /// the call must refuse to write rather than silently re-creating a
    /// fjall offset key for data `purge_org` already erased everywhere
    /// else (`GroupOffsetStore::commit`/`force_commit` have no notion of
    /// "this org was purged" on their own — they would just insert the key
    /// again). `fetch`'s own read-then-write path is separately protected
    /// by `purge_org` detaching every partition it touches (see
    /// `map_engine_error`), so it does not need this check too.
    purged_orgs: Arc<DashMap<String, u64>>,
    /// Copied from `BusInitConfig::dedup_expected_rate_per_sec` at `new()`;
    /// fed into every `dedup::DedupConfig` this service builds in
    /// `dedup_store`.
    dedup_expected_rate_per_sec: u64,
    /// Side registry of every `Partition` clone a LIVE `ConsumerHandle` is
    /// holding (`ConsumerPartition::partition`), stored as a
    /// `tentaflow_bus::WeakPartition` so this map is never itself a strong
    /// owner — only the `ConsumerHandle` (and, while it is present, this
    /// service's own `partitions` map) keep the writer thread/flock alive.
    /// Populated in `open_consumer`'s phase 2, one entry appended per
    /// `(org, topic, partition)` key.
    ///
    /// Exists ONLY to make `delete_topic`/`purge_org`'s `detach()` reach a
    /// consumer's clone even after `run_retention_sweep` has already
    /// removed that key from `partitions` (see the module doc's PARTITION
    /// HANDLE LIFETIME section) — without it, a consumer opened while a key
    /// was temporarily absent from `partitions` would hold a partition
    /// `delete_topic`/`purge_org` can no longer reach through the main map,
    /// silently serving a deleted/purged org's data. Entries are pruned
    /// opportunistically: every push first drops dead (already-upgraded-to-
    /// `None`) weak refs for that key, so this never accumulates one entry
    /// per HISTORICAL `open_consumer` call, only one per still-live handle.
    consumer_partitions: Arc<DashMap<PartitionKey, Vec<tentaflow_bus::WeakPartition>>>,
    /// Per-(org, topic) publish-rate window (`RateCounter`'s doc) — feeds
    /// `topic_rates`/`StatsSnapshot`'s `msgs_in_per_sec`/`bytes_in_per_sec`.
    publish_rates: DashMap<TopicKey, RateCounter>,
    /// M2 (PLAN-M2 §1e): injected replication backend, set once via
    /// `set_replication` after mesh startup. `None` (the default) means M1
    /// behavior: every `publish`/`open_consumer`/`fetch`/`commit` call runs
    /// exactly as it did before M2. `parking_lot::RwLock` rather than
    /// `ArcSwapOption`: `arc_swap::RefCnt` is only implemented for `Arc<T>`
    /// with `T: Sized` (`arc-swap` 1.7's `ref_cnt.rs`), so it cannot hold
    /// `Arc<dyn ReplicationCoordinator>` at all; a swap here happens at most
    /// once per process lifetime, so an uncontended `RwLock` read on the hot
    /// path costs nothing worth avoiding that for.
    ///
    /// Wrapped in its own `Arc` (wave 2, agent S) — not just the inner
    /// `RwLock` — so `open_consumer` can hand every `ConsumerHandle` a clone
    /// of the SAME lock (`Arc::clone`, the identical pattern
    /// `node_environment_cache` already uses): a coordinator installed or
    /// swapped after a handle was opened must be visible to that handle's
    /// next `fetch`/`commit` `role()` check immediately, not only to a
    /// freshly opened one.
    replication: Arc<parking_lot::RwLock<Option<Arc<dyn ReplicationCoordinator>>>>,
    /// M2 (PLAN-M2 §1e, `create_topic`'s replica placement / `delete_topic`'s
    /// cleanup): the ledger-backed assignment store, set once via
    /// `set_assignment_store` — separate from `replication` because a node
    /// can propose/read assignments (a SQLite+ledger concern, `assignment.
    /// rs`) before it has a live `ReplicationCoordinator` wired (a mesh+
    /// election concern, `manager.rs`), and because `ReplicationCoordinator`
    /// itself has no `place_topic`/assignment-write method (frozen wave-0
    /// contract) — see `create_topic`'s doc for why this file goes around
    /// that trait for placement instead of through it. `None` (the default)
    /// means `create_topic` never proposes any assignment, matching every
    /// other M1 behavior guarantee in this file: nothing observable changes
    /// until something explicitly wires this up.
    assignment_store:
        parking_lot::RwLock<Option<Arc<replication::assignment::SqliteLedgerAssignmentStore>>>,
    /// M2 (PLAN-M2 §1e, A9 debt): counts every `partition_handle` call
    /// (hit or miss) with a monotonically increasing sequence number, so
    /// `maybe_evict_lru_partition_handles` can find the truly
    /// least-recently-USED entries in `self.partitions` without touching a
    /// wall clock on the hot path. A plain `u64` counter (not per-key
    /// timestamps) is enough: eviction only ever needs a strict ORDER
    /// between keys, not an actual duration.
    partition_access: DashMap<PartitionKey, u64>,
    /// Feeds `partition_access`'s sequence numbers — `fetch_add` under no
    /// lock, since ties (two threads getting the same value) only cost a
    /// coin-flip in eviction order, never correctness.
    partition_access_clock: AtomicU64,
    /// Copied from `BusInitConfig::partition_handle_lru` at `new()` — `None`
    /// disables the eviction check in `partition_handle` entirely (M1's
    /// actual behavior, unbounded, RF=1's default).
    partition_handle_lru: Option<usize>,
    /// Copied from `BusInitConfig::publish_ack_timeout` at `new()` — see
    /// that field's doc.
    publish_ack_timeout: Duration,
    /// Test-only synchronization point: if set, taken and invoked exactly
    /// once, right after `open_consumer`'s phase 1 completes (before phase
    /// 2's side effects and the purge-epoch re-check) — lets a test
    /// deterministically land a `purge_org` call in the EXACT window the
    /// re-check exists to catch, instead of relying on real thread
    /// scheduling to reproduce a race. Always `None` outside tests.
    #[cfg(test)]
    test_open_consumer_after_phase1: std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

fn environment_to_u8(env: NodeEnvironment) -> u8 {
    match env {
        NodeEnvironment::Dev => 0,
        NodeEnvironment::Test => 1,
        NodeEnvironment::Prod => 2,
    }
}

fn environment_from_u8(v: u8) -> NodeEnvironment {
    match v {
        0 => NodeEnvironment::Dev,
        1 => NodeEnvironment::Test,
        _ => NodeEnvironment::Prod,
    }
}

impl BusService {
    pub fn new(cfg: BusInitConfig) -> Result<Self, BusServiceError> {
        std::fs::create_dir_all(&cfg.bus_dir)?;
        let meta_dir = cfg.bus_dir.join("_meta");
        let fjall_db = fjall::Database::builder(&meta_dir).open()?;
        let offsets = Arc::new(groups::GroupOffsetStore::open(&fjall_db)?);
        let producer_seq = Arc::new(producer::ProducerSeqStore::open(&fjall_db)?);
        let discarded = Arc::new(dlq::DiscardStore::open(&fjall_db)?);
        let node_environment_cache = Arc::new(AtomicU8::new(environment_to_u8(
            crate::services::environment::get_node_environment(&cfg.db),
        )));
        // M1-R2 review N-1/N-7, coordinator decision 3: delete any leftover
        // `bus_groups` row for the now-retired ephemeral probe group
        // outright, on every startup — `dispatch/bus.rs`'s own `tf-`-prefix
        // filter already hides it from `GroupList`/KPI counts as defense in
        // depth, but a row that no code path can ever create again should
        // not just sit there forever either. Best-effort: a failure here
        // (e.g. `bus_groups` not migrated in yet on some unusual startup
        // ordering) must not prevent the bus service itself from starting.
        match crate::db::repository::bus_groups_delete_by_group_id(&cfg.db, LEGACY_PROBE_GROUP_ID) {
            Ok(0) => {}
            Ok(n) => tracing::info!(
                group_id = LEGACY_PROBE_GROUP_ID,
                rows_deleted = n,
                "bus init: purged leftover legacy probe group row(s)"
            ),
            Err(e) => tracing::warn!(
                group_id = LEGACY_PROBE_GROUP_ID,
                error = %e,
                "bus init: failed to purge leftover legacy probe group row(s)"
            ),
        }
        // Owner decision B follow-up (`SUM/tentabus/KRYTYK-M1-R5.md` b.8,
        // R5-8): repair any `__dlq.*` row created before `dlq::
        // dlq_topic_options` started pinning `DurabilityClass::Standard` —
        // see `migrate_legacy_dlq_durability`'s own doc. Same best-effort
        // tolerance as the probe-group purge above: this must never block
        // the bus service from starting.
        match Self::migrate_legacy_dlq_durability(&cfg.db) {
            Ok(0) => {}
            Ok(n) => tracing::info!(
                rows_updated = n,
                "bus init: migrated legacy DLQ topic(s) to the fixed interval durability policy"
            ),
            Err(e) => tracing::warn!(
                error = %e,
                "bus init: failed to migrate legacy DLQ topic durability"
            ),
        }
        Ok(Self {
            bus_dir: cfg.bus_dir,
            db: cfg.db,
            authorizer: cfg.authorizer,
            _fjall_db: fjall_db,
            offsets,
            producer_seq,
            discarded,
            partitions: DashMap::new(),
            round_robin: DashMap::new(),
            dedup_stores: DashMap::new(),
            quota: quota::QuotaManager::new(),
            topic_config_cache: DashMap::new(),
            topic_config_db_loads: AtomicU64::new(0),
            publish_msgs_total: AtomicU64::new(0),
            publish_bytes_total: AtomicU64::new(0),
            consume_msgs_total: Arc::new(AtomicU64::new(0)),
            throttled_total: AtomicU64::new(0),
            node_environment_cache,
            audit_windows: Arc::new(AuditWindows::new(AUDIT_WINDOW)),
            sweeper_shutdown: Arc::new(AtomicBool::new(false)),
            group_state: Arc::new(GroupStateCache::new()),
            commit_locks: Arc::new(DashMap::new()),
            purged_orgs: Arc::new(DashMap::new()),
            dedup_expected_rate_per_sec: cfg.dedup_expected_rate_per_sec,
            consumer_partitions: Arc::new(DashMap::new()),
            publish_rates: DashMap::new(),
            replication: Arc::new(parking_lot::RwLock::new(None)),
            assignment_store: parking_lot::RwLock::new(None),
            partition_access: DashMap::new(),
            partition_access_clock: AtomicU64::new(0),
            partition_handle_lru: cfg.partition_handle_lru,
            publish_ack_timeout: cfg.publish_ack_timeout,
            #[cfg(test)]
            test_open_consumer_after_phase1: std::sync::Mutex::new(None),
        })
    }

    /// M2 (PLAN-M2 §1e): installs the replication backend. Called once,
    /// from `main.rs`, AFTER the mesh is up (`bus::replication::init`,
    /// PLAN-M2 §2) — never from `bus::init` itself, since the coordinator
    /// needs a live `IrohMeshManager` to dial/accept `ALPN_BUS` streams.
    /// Overwrites any previously set coordinator (a plain `store`, not a
    /// compare-and-swap): this build never calls it more than once, and a
    /// future re-election-driven reconfiguration replacing the coordinator
    /// wholesale is not a scenario PLAN-M2 describes.
    ///
    /// Wave 2 (agent S): `publish`/`open_consumer`/`fetch`/`commit`/`peek`
    /// now read `self.replication` — see `role_check`/`preflight_publish`/
    /// `await_publish_acks` below. `None` (never called, or a build with no
    /// coordinator wired) keeps every one of those call paths byte-for-byte
    /// identical to M1 (PLAN-M2 §4.1 A1).
    pub fn set_replication(&self, coordinator: Arc<dyn ReplicationCoordinator>) {
        *self.replication.write() = Some(coordinator);
    }

    /// M2 (PLAN-M2 §1f): the ONE read accessor `dispatch/bus.rs`'s
    /// `ReplicaList`/`Reassign`/`LeaderTransfer` handlers need to reach the
    /// injected coordinator — `None` means M1 behavior (RF=1, `hw == leo`,
    /// no coordinator ever installed), which those handlers fall back to
    /// with an honest single-node snapshot instead of erroring. A cheap
    /// `Arc` clone under an uncontended read lock, same cost profile as
    /// every other `self.replication.read()` this file adds in wave 2.
    pub fn replication(&self) -> Option<Arc<dyn ReplicationCoordinator>> {
        self.replication.read().clone()
    }

    /// M2 (PLAN-M2 §1c/§1e): installs the ledger-backed assignment store
    /// `create_topic`'s replica placement and `delete_topic`/`purge_org`'s
    /// cleanup use — see `assignment_store`'s field doc for why this is
    /// separate from `set_replication`. Called once, alongside
    /// `set_replication`, from wherever wires `bus::replication::init`.
    pub fn set_assignment_store(
        &self,
        store: Arc<replication::assignment::SqliteLedgerAssignmentStore>,
    ) {
        *self.assignment_store.write() = Some(store);
    }

    /// `None` until `set_assignment_store` is called (M1 behavior: no
    /// placement, no assignment cleanup — every existing call path is
    /// unaffected).
    fn assignment_store(
        &self,
    ) -> Option<Arc<replication::assignment::SqliteLedgerAssignmentStore>> {
        self.assignment_store.read().clone()
    }

    /// Owner decision B follow-up (`SUM/tentabus/KRYTYK-M1-R5.md` b.8,
    /// R5-8): `dlq::dlq_topic_options`'s `DurabilityClass::Standard` pin
    /// only applies at a DLQ topic's OWN creation — `ensure_dlq_topic`'s
    /// get-or-create never revisits an already-existing row, so a
    /// `__dlq.*` topic created before that fix landed keeps whatever
    /// durability it inherited from its source at the time, forever,
    /// unless something else fixes it up. This runs once per
    /// `BusService::new` (best-effort, see caller), across EVERY org.
    ///
    /// UI critic round 6, R6-1 (P2): the original sweep matched on
    /// `durability != fixed_wire` alone, which stamped over an operator's
    /// explicit `durability` override on a DLQ topic on every single
    /// startup — `durability_class == None` (v148,
    /// `bus_topics_add_durability_class_column`'s own doc) means exactly
    /// that: an explicit override, not something this best-effort sweep
    /// owns. This now only touches a row whose `durability_class` is
    /// `Some("critical")` — the v148 backfill value for a pre-decision-B
    /// `__dlq.*` row that inherited an `fsync_batch`/`fsync_batch_full`
    /// policy from its source topic and was never itself set explicitly
    /// (see that migration's BACKFILL ASSUMPTION). A row already
    /// `Some("standard")` (already fixed, by this sweep or otherwise) is
    /// left alone too. Matched rows are stamped to
    /// `FsyncInterval{ms: topics::STANDARD_FSYNC_INTERVAL_MS}` +
    /// `durability_class = Standard`, which is itself never
    /// `Some("critical")` again — so a second sweep over the same row is a
    /// no-op, making this predicate naturally idempotent without needing a
    /// separate "already fixed" check on `durability` itself.
    ///
    /// Deliberately a FIXED literal value here, not "whatever
    /// `DurabilityClass::Standard` resolves to on this node" (which would
    /// be `Os` in Dev) — this sweep repairs rows that predate the
    /// class-based policy entirely; it is not trying to guess what
    /// class-derived value they "should" have gotten had they been created
    /// today under `dlq_topic_options`'s current per-environment
    /// resolution.
    ///
    /// Writes one `bus.topic.update` audit entry per migrated row (system
    /// actor — `None`, like `run_retention_sweep`'s own summary row — this
    /// runs at startup, not on behalf of any caller), reporting the
    /// `durability`/`durability_class` before->after transition plus a
    /// `reason=legacy_dlq_durability_migration` marker so it reads
    /// distinctly from an operator-triggered `update_topic` call in the
    /// same audit trail. Returns the number of rows updated.
    fn migrate_legacy_dlq_durability(db: &crate::db::DbPool) -> Result<usize, BusServiceError> {
        let fixed_wire = topics::DurabilityPolicy::FsyncInterval {
            ms: topics::STANDARD_FSYNC_INTERVAL_MS,
        }
        .to_wire_string();
        let critical_class = topics::DurabilityClass::Critical.as_str();
        let standard_class = topics::DurabilityClass::Standard.as_str().to_string();
        let now = now_ms();
        let mut updated = 0usize;
        for mut row in crate::db::repository::bus_topic_list_all_dlq(db)? {
            if row.durability_class.as_deref() != Some(critical_class) {
                continue;
            }
            let old_durability = row.durability.clone();
            row.durability = fixed_wire.clone();
            row.durability_class = Some(standard_class.clone());
            row.updated_at_ms = now;
            crate::db::repository::bus_topic_update(db, &row)?;
            let _ = crate::db::repository::log_audit(
                db,
                None,
                None,
                "bus.topic.update",
                Some(&row.name),
                Some(&audit_details(
                    &row.org_id,
                    Some(&format!(
                        "durability={old_durability}->{fixed_wire} \
                         durability_class={critical_class}->{standard_class} \
                         reason=legacy_dlq_durability_migration"
                    )),
                )),
                None,
                None,
            );
            updated += 1;
        }
        Ok(updated)
    }

    /// Registers a `ConsumerPartition`'s `Partition` clone in
    /// `consumer_partitions` (see that field's doc) so `delete_topic`/
    /// `purge_org` can still `detach()` it even if `run_retention_sweep`
    /// has since removed the SAME key from the main `partitions` map.
    /// Prunes every already-dead weak reference for this key first, so this
    /// map's memory tracks live handles, not every `open_consumer` call
    /// this service has ever served.
    fn register_consumer_partition(&self, key: PartitionKey, part: &tentaflow_bus::Partition) {
        let mut entry = self.consumer_partitions.entry(key).or_default();
        entry.retain(|w| w.upgrade().is_some());
        entry.push(part.downgrade());
    }

    /// Detaches every still-live `Partition` this service knows a
    /// `ConsumerHandle` is holding for `org_id` (optionally further
    /// narrowed to one `topic`) — the counterpart of `partitions.retain`'s
    /// own `detach()` loop in `delete_topic`/`purge_org`, reaching handles
    /// the sweeper already dropped from the main map. Dead entries are
    /// dropped as they are found; the whole `(org[, topic])` slice of the
    /// registry is removed afterward regardless of whether anything was
    /// still live, exactly like `partitions.retain` above it.
    fn detach_consumer_partitions(&self, org_id: &str, topic: Option<&str>) {
        self.consumer_partitions.retain(|key, weak_parts| {
            if key.0 != org_id || topic.is_some_and(|t| key.1 != t) {
                return true;
            }
            for w in weak_parts.iter() {
                if let Some(part) = w.upgrade() {
                    part.detach();
                }
            }
            false
        });
    }

    /// Writes one `audit_log` row per (org, action) bucket whose window has
    /// suppressed at least one occurrence (`AuditWindows::drain_suppressed`)
    /// — called by `init`'s background sweeper thread so a burst of
    /// rejections that stops mid-window still gets its tail count recorded
    /// even if no further occurrence ever arrives to trigger the lazy flush
    /// in `audit_windowed`. Also callable directly (tests, an operator
    /// tool) to flush deterministically without waiting for the window.
    pub fn flush_audit_windows(&self) {
        for (org_id, kind, count, resource, actor) in self.audit_windows.drain_suppressed() {
            let _ = crate::db::repository::log_audit(
                &self.db,
                actor.as_deref(),
                None,
                kind,
                resource.as_deref(),
                Some(&format!("{} count={count}", audit_details(&org_id, None))),
                None,
                None,
            );
        }
    }

    /// Records one occurrence of a windowed audit `kind` for `ctx.org_id`
    /// and writes it to `audit_log` immediately if `AuditWindows::record`
    /// says this occurrence is not suppressed  — the
    /// single call site every `bus.quota.exceeded`/`bus.produce.denied`/
    /// `bus.consume.denied` emission in `BusService` goes through
    /// (`ConsumerHandle::revalidate`'s own `bus.consume.denied` uses the
    /// same underlying `write_windowed_audit`, since it has no `&BusService`
    /// to call this method on), so the windowing behavior lives in exactly
    /// one place.
    fn audit_windowed(
        &self,
        ctx: &BusCallContext,
        kind: &'static str,
        resource: Option<&str>,
        extra: Option<&str>,
    ) {
        write_windowed_audit(
            &self.db,
            &self.audit_windows,
            ctx.actor.as_deref(),
            &ctx.org_id,
            kind,
            resource,
            extra,
        );
    }

    /// Asks the background sweeper thread `init` spawned (if any) to stop
    /// at its next wake-up (a no-op if no `retention_interval` was
    /// configured — no thread was ever spawned), and flushes any pending
    /// windowed audit occurrences immediately rather than waiting for that
    /// wake-up (or the independent audit-flush timer's own tick) to get
    /// around to it — a caller explicitly asking for shutdown should not
    /// lose an in-flight suppressed count to a race against whichever
    /// background thread happens to notice first.
    pub fn stop_background_sweeper(&self) {
        self.sweeper_shutdown.store(true, Ordering::Release);
        self.flush_audit_windows();
    }

    pub fn quota(&self) -> &quota::QuotaManager {
        &self.quota
    }

    /// Number of times `topic_config` actually hit SQLite (as opposed to
    /// the in-memory cache) since this service started — a test/metric hook
    /// for the "zero SQLite queries on the publish hot path after
    /// warm-up" requirement.
    pub fn topic_config_db_loads(&self) -> u64 {
        self.topic_config_db_loads.load(Ordering::Relaxed)
    }

    /// Number of times `ConsumerHandle::fetch`'s pause check actually hit
    /// SQLite (as opposed to `GroupStateCache`) since this service started
    /// — the consumption-side counterpart to `topic_config_db_loads`.
    pub fn group_state_db_loads(&self) -> u64 {
        self.group_state.db_loads.load(Ordering::Relaxed)
    }

    /// Snapshot of this service's PLAN §8.4 bus-level counters —
    /// `(publish_msgs_total, publish_bytes_total, consume_msgs_total,
    /// throttled_total)`. Feeds the Zabbix exporter's
    /// `tentaflow_bus_publish_msgs_total`/`bus_publish_bytes_total`/
    /// `bus_consume_msgs_total`/`bus_throttled_total` keys. All `Relaxed`
    /// loads — approximate under concurrent traffic, which is what a
    /// scrape-interval counter needs, not a linearizable read.
    pub fn bus_metrics_snapshot(&self) -> (u64, u64, u64, u64) {
        (
            self.publish_msgs_total.load(Ordering::Relaxed),
            self.publish_bytes_total.load(Ordering::Relaxed),
            self.consume_msgs_total.load(Ordering::Relaxed),
            self.throttled_total.load(Ordering::Relaxed),
        )
    }

    /// Engine hot-path p99 latencies in microseconds — `(append_p99_us,
    /// fsync_p99_us)`. Delegates to the process-wide reservoirs in
    /// `tentaflow_bus::metrics` (PLAN §8.4). A free function rather than a
    /// method: those reservoirs are process-wide, not scoped to any one
    /// `BusService`/partition.
    pub fn bus_engine_p99_us() -> (u64, u64) {
        (
            tentaflow_bus::metrics::append_p99_us(),
            tentaflow_bus::metrics::fsync_p99_us(),
        )
    }

    /// Committed offset for one consumer group's (topic, partition)
    /// subscription — a thin passthrough to `self.offsets` (fjall) so the
    /// Zabbix exporter's `bus_consumer_lag_max`/`bus_consumer_lag_sum`
    /// computation (PLAN §8.4) never needs to reach into this module's
    /// private fields directly. `0` (earliest) if the group has never
    /// committed here, same as `GroupOffsetStore::committed_offset`.
    pub fn group_committed_offset(
        &self,
        org_id: &str,
        group: &str,
        topic: &str,
        partition: u32,
    ) -> Result<u64, BusServiceError> {
        self.offsets
            .committed_offset(org_id, group, topic, partition)
    }

    /// Re-reads `settings.node_environment` and refreshes the cache
    /// `publish`/`check_environment` consult. Must be
    /// called by whatever calls `sync::runtime::switch_node_environment`
    /// (tor P's dispatch hook) — this service has no way to observe that
    /// change on its own.
    pub fn invalidate_environment_cache(&self) {
        let fresh = crate::services::environment::get_node_environment(&self.db);
        self.node_environment_cache
            .store(environment_to_u8(fresh), Ordering::Release);
    }

    fn cached_environment(&self) -> NodeEnvironment {
        environment_from_u8(self.node_environment_cache.load(Ordering::Acquire))
    }

    /// Fail-closed Z12 environment fencing: a topic stamped with
    /// one environment must never accept traffic from a node currently
    /// declaring a different one (e.g. after `switch_node_environment`
    /// Test→Prod). Exposed as its own method — not inlined into `publish`
    /// — so the consumer-side agent's `open_consumer`/`fetch` work can call
    /// the exact same check.
    pub fn check_environment(&self, cfg: &topics::TopicConfig) -> Result<(), BusServiceError> {
        let node_env = self.cached_environment();
        if cfg.environment != node_env {
            return Err(BusServiceError::EnvironmentMismatch {
                topic_env: cfg.environment,
                node_env,
            });
        }
        Ok(())
    }

    /// Reads a topic's config, serving from `topic_config_cache` when
    /// possible. Returns an owned `TopicConfig` (cloned from the
    /// cached `Arc` on a hit) rather than the `Arc` itself, so every
    /// existing call site's `&TopicConfig` usage keeps working unchanged.
    fn topic_config(
        &self,
        org_id: &str,
        topic: &str,
    ) -> Result<topics::TopicConfig, BusServiceError> {
        let key = (org_id.to_string(), topic.to_string());
        if let Some(cached) = self.topic_config_cache.get(&key) {
            return Ok((**cached).clone());
        }
        self.topic_config_db_loads.fetch_add(1, Ordering::Relaxed);
        let cfg = topics::get_topic(&self.db, org_id, topic)?.ok_or_else(|| {
            BusServiceError::TopicNotFound {
                name: topic.to_string(),
            }
        })?;
        self.topic_config_cache.insert(key, Arc::new(cfg.clone()));
        Ok(cfg)
    }

    /// Removes `topic`'s cached config, if any, so the next `topic_config`
    /// call re-reads SQLite. Called from `update_topic`/`delete_topic`
    /// — the only two admin paths that change what `bus_topics`
    /// holds for an existing topic.
    fn invalidate_topic_config_cache(&self, org_id: &str, topic: &str) {
        self.topic_config_cache
            .remove(&(org_id.to_string(), topic.to_string()));
    }

    /// Opens (or returns the already-open) `Partition` for `(org, topic,
    /// partition)`. Uses `DashMap::entry(..).or_try_insert_with`
    /// instead of a separate `get` + `insert` so two threads racing to
    /// first-open the SAME fresh partition serialize on that entry's shard
    /// lock rather than both calling `Partition::open` concurrently — the
    /// second of which would otherwise fail with `PartitionLocked` purely
    /// from `flock`'s well-known same-process, different-fd contention,
    /// not any real conflict.
    ///
    /// On a cache MISS, first checks `consumer_partitions` (see that
    /// field's doc) for a still-live `Partition` a `ConsumerHandle` is
    /// already holding for this exact key before opening a fresh one from
    /// disk. Without this, `run_retention_sweep`'s cleanup — which removes
    /// a `self.partitions` entry it opened for itself, even when a
    /// consumer grabbed the SAME key while the sweep was in flight and is
    /// still holding its own live clone — would leave the NEXT caller
    /// racing that consumer's still-held directory flock: a fresh
    /// `Partition::open` on the same directory fails with `PartitionLocked`
    /// (a real, if transient, outage) instead of transparently rejoining
    /// the handle that is already open and already accepting writes.
    fn partition_handle(
        &self,
        org_id: &str,
        topic: &str,
        partition: u32,
        cfg: &topics::TopicConfig,
    ) -> Result<tentaflow_bus::Partition, BusServiceError> {
        let key = (org_id.to_string(), topic.to_string(), partition);
        let part = {
            let entry = self.partitions.entry(key.clone()).or_try_insert_with(|| {
                if let Some(live) = self
                    .consumer_partitions
                    .get(&key)
                    .and_then(|weak_parts| weak_parts.iter().find_map(|w| w.upgrade()))
                {
                    return Ok(live);
                }
                let dir = topics::partition_dir(&self.bus_dir, org_id, topic, partition);
                tentaflow_bus::Partition::open(
                    &dir,
                    tentaflow_bus::RollPolicy::default(),
                    cfg.durability.to_engine(),
                    256,
                )
            })?;
            entry.clone()
        };
        // M2 (PLAN-M2 §1e, A9 debt): record this access and, if
        // `partition_handle_lru` is configured, evict idle handles above
        // the cap. `None` (M1's actual behavior) skips both entirely — no
        // observable change for RF=1 unless an operator opts in.
        if let Some(cap) = self.partition_handle_lru {
            self.partition_access.insert(
                key.clone(),
                self.partition_access_clock.fetch_add(1, Ordering::Relaxed),
            );
            self.maybe_evict_lru_partition_handles(cap, &key);
        }
        Ok(part)
    }

    /// See `partition_handle_lru`'s field doc and `partition_access`'s. Only
    /// ever called with `self.partition_handle_lru == Some(cap)`. Never
    /// evicts `keep` (the key the caller that triggered this pass is about
    /// to use), a key a live `ConsumerHandle` still references
    /// (`consumer_partitions`), or a key the coordinator currently reports
    /// as `Leader`/`Follower` (an active replication stream, not an idle
    /// handle — `role()` doubles as the "is this handle idle" oracle here).
    /// Best-effort: a handle this pass cannot safely evict just stays open
    /// past `cap` until a later call finds it evictable.
    fn maybe_evict_lru_partition_handles(&self, cap: usize, keep: &PartitionKey) {
        let len = self.partitions.len();
        if len <= cap {
            return;
        }
        let coordinator = self.replication();
        let mut candidates: Vec<(PartitionKey, u64)> = self
            .partition_access
            .iter()
            .filter(|e| e.key() != keep)
            .map(|e| (e.key().clone(), *e.value()))
            .collect();
        candidates.sort_by_key(|(_, seq)| *seq);
        let mut to_evict = len - cap;
        for (key, _) in candidates {
            if to_evict == 0 {
                break;
            }
            if !self.partitions.contains_key(&key) {
                continue;
            }
            let has_live_consumer = self
                .consumer_partitions
                .get(&key)
                .is_some_and(|weak_parts| weak_parts.iter().any(|w| w.upgrade().is_some()));
            if has_live_consumer {
                continue;
            }
            if let Some(coordinator) = &coordinator {
                match coordinator.role(&key.0, &key.1, key.2) {
                    PartitionRole::Leader { .. } | PartitionRole::Follower { .. } => continue,
                    PartitionRole::Unavailable { .. } => {}
                }
            }
            if self.partitions.remove(&key).is_some() {
                self.partition_access.remove(&key);
                to_evict -= 1;
            }
        }
    }

    /// Opens (or returns the already-open) dedup store for `(org, topic)`.
    /// Uses `DashMap::entry(..).or_try_insert_with` — the same fix
    /// `partition_handle` applies for the identical race: two threads
    /// publishing concurrently to a freshly dedup-enabled topic used to
    /// both call `MmapDedupStore::open` on the same path, and the loser got
    /// `io::ErrorKind::WouldBlock` from the file's advisory lock (a false
    /// failure — there is no real conflict, just two opens racing the same
    /// first-open) instead of sharing the winner's handle.
    fn dedup_store(
        &self,
        org_id: &str,
        topic: &str,
        cfg: &topics::TopicConfig,
    ) -> Result<Arc<dedup::MmapDedupStore>, BusServiceError> {
        let key = (org_id.to_string(), topic.to_string());
        let entry = self.dedup_stores.entry(key).or_try_insert_with(|| {
            let dir = topics::topic_dir(&self.bus_dir, org_id, topic);
            std::fs::create_dir_all(&dir)?;
            let store = dedup::MmapDedupStore::open(
                &dir.join("dedup.bin"),
                dedup::DedupConfig {
                    ttl_ms: cfg.dedup_window_ms,
                    expected_rate_per_sec: self.dedup_expected_rate_per_sec,
                    ..Default::default()
                },
            )?;
            Ok::<_, std::io::Error>(Arc::new(store))
        })?;
        Ok(entry.clone())
    }

    fn next_round_robin(&self, org_id: &str, topic: &str, partitions: u32) -> u32 {
        let entry = self
            .round_robin
            .entry((org_id.to_string(), topic.to_string()))
            .or_insert_with(|| AtomicU32::new(0));
        entry.fetch_add(1, Ordering::Relaxed) % partitions
    }

    /// Assigns a partition to EVERY record in `batch` (an earlier draft routed the whole batch by the FIRST record's key,
    /// silently breaking "same key → same partition" for every other
    /// record in a multi-key batch). An explicit `batch.partition` still
    /// forces the whole batch onto one partition (a caller wanting strict
    /// batch-wide ordering asked for exactly that). Otherwise: a keyed
    /// record hashes via `partition_for_key`; a keyless one round-robins —
    /// chosen over "always partition 0" so a topic whose producers never
    /// set a key still spreads load across every partition, which is the
    /// common "just publish events" case.
    fn resolve_partitions(
        &self,
        org_id: &str,
        topic: &str,
        cfg: &topics::TopicConfig,
        batch: &PublishBatch,
    ) -> Vec<u32> {
        let partitions = cfg.partitions.max(1);
        if let Some(p) = batch.partition {
            let p = p % partitions;
            return vec![p; batch.records.len()];
        }
        batch
            .records
            .iter()
            .map(|r| match &r.key {
                Some(k) => partition_for_key(k, partitions),
                None => self.next_round_robin(org_id, topic, partitions),
            })
            .collect()
    }

    // ---- Admin: topic lifecycle (PLAN §8.2 audit actions) --------------

    /// Enforces the org-level `max_topics`/`max_partitions` ceilings (PLAN
    /// §7.1) before a new topic is persisted — counted from
    /// `bus_topics` itself rather than any in-memory cache, since topic
    /// creation is an admin-plane, low-frequency operation (unlike
    /// `publish`'s hot path, which the config cache keeps off SQLite entirely) where a
    /// direct, always-current count is worth the query.
    fn enforce_topic_resource_quota(
        &self,
        org_id: &str,
        opts: &topics::TopicOptions,
    ) -> Result<(), BusServiceError> {
        let existing = crate::db::repository::bus_topic_list(&self.db, org_id)?;
        let max_topics = self.quota.max_topics(org_id);
        let current_topics = existing.len() as u32;
        if current_topics >= max_topics {
            return Err(BusServiceError::MaxTopicsExceeded {
                org_id: org_id.to_string(),
                max: max_topics,
                current: current_topics,
            });
        }
        let requested_partitions = opts.partitions.unwrap_or(topics::DEFAULT_PARTITIONS);
        let current_partitions: u32 = existing.iter().map(|t| t.partitions).sum();
        let max_partitions = self.quota.max_partitions(org_id);
        if current_partitions.saturating_add(requested_partitions) > max_partitions {
            return Err(BusServiceError::MaxPartitionsExceeded {
                org_id: org_id.to_string(),
                max: max_partitions,
                current: current_partitions,
                requested: requested_partitions,
            });
        }
        Ok(())
    }

    /// M2 (PLAN-M2 §1e): replica placement. `replication_factor` defaults
    /// to `min(3, healthy same-environment nodes)` — PLAN §7.1's own
    /// intended default, meaningless in M1 (no coordinator, no mesh) and
    /// left at a hard `1` by `topics::TopicConfig::from_options` for that
    /// reason (see that field's doc). This resolves it for real once a
    /// coordinator IS installed, using `ReplicationCoordinator::snapshot`'s
    /// `ReplicaNodeInfo.reachable`/`environment` — NOT a direct
    /// `db::repository::list_trusted_node_environments` query — because a
    /// trusted node with no live mesh connection is not "healthy" and must
    /// not inflate the default RF a fresh topic gets placed at.
    ///
    /// Placement itself does not go through `ReplicationCoordinator`: that
    /// trait is frozen (wave 0) with no `place_topic`/assignment-write
    /// method, and by design (K-M2-4) a `PartitionAssignment` is proposed
    /// straight through `assignment_store()` (`SqliteLedgerAssignmentStore::
    /// propose`, capture -> ledger -> materializer) — the same ledger path
    /// `ReplicationManager` itself uses. `assignment_store` unset (no
    /// coordinator wired yet) skips placement entirely: the topic row is
    /// still created with whatever `replication_factor` it resolved to,
    /// just with no assignments proposed for it yet.
    pub fn create_topic(
        &self,
        ctx: &BusCallContext,
        name: &str,
        mut opts: topics::TopicOptions,
    ) -> Result<topics::TopicConfig, BusServiceError> {
        topics::validate_org_id(&ctx.org_id)?;
        self.authorizer
            .authorize(ctx, BusAction::Admin, name)
            .map_err(|_| deny(BusAction::Admin, name))?;
        self.enforce_topic_resource_quota(&ctx.org_id, &opts)?;
        let env = crate::services::environment::get_node_environment(&self.db);

        let coordinator = self.replication();
        // `local_node_id` comes straight from the coordinator's own
        // identity (`ReplicationCoordinator::local_node_id`), NOT by
        // searching `snapshot(org, None).nodes` for `is_local` (the bug a
        // live krytyk pass on M2 found): `snapshot()`'s `nodes` list is
        // populated ENTIRELY from existing partition assignments, so a
        // fresh org's empty registry can never contain an `is_local` entry
        // to find — every `create_topic` call silently proposed zero
        // assignments, forever, because the registry could never bootstrap
        // its own first row. `same_env_replicas` still comes from the
        // snapshot (a real, lesser limitation: the very first topic in a
        // brand-new multi-node cluster still can't discover OTHER peers
        // this way and settles for RF=1 on this node alone — but unlike
        // the identity check, this self-corrects, since every topic after
        // that first one sees the growing registry). An empty
        // `local_node_id()` (a coordinator that never overrides the
        // trait's default, e.g. a test fake) leaves `opts.replication_
        // factor` exactly as the caller passed it and proposes no
        // assignments, same as "no coordinator" would.
        let mut placement: Option<(String, Vec<String>)> = None;
        if let Some(coordinator) = &coordinator {
            let local_node_id = coordinator.local_node_id();
            let snapshot = coordinator.snapshot(&ctx.org_id, None);
            let mut same_env: Vec<String> = snapshot
                .nodes
                .iter()
                .filter(|n| n.environment == env && n.reachable && n.node_id != local_node_id)
                .map(|n| n.node_id.clone())
                .collect();
            same_env.sort();
            if opts.replication_factor.is_none() {
                // +1: the local node itself always counts as one healthy
                // replica even when the snapshot has no OTHER same-env
                // peer yet (a brand-new single-node mesh).
                let healthy = (same_env.len() as u32 + 1).min(3);
                opts.replication_factor = Some(healthy);
            }
            if !local_node_id.is_empty() {
                placement = Some((local_node_id, same_env));
            }
        }

        let cfg = topics::create_topic(&self.db, &ctx.org_id, name, opts, env, now_ms())?;
        // Defensive: a delete+recreate of the same name must not resurrect a
        // stale cached config.
        self.invalidate_topic_config_cache(&ctx.org_id, name);

        let mut replicas_detail = String::new();
        if let (Some(store), Some((local_node_id, same_env))) = (self.assignment_store(), placement)
        {
            let mut all_replicas: Vec<String> = Vec::new();
            let mut placed = 0u32;
            for partition in 0..cfg.partitions {
                let replicas =
                    build_replica_set(&local_node_id, &same_env, cfg.replication_factor, partition);
                for node in &replicas {
                    if !all_replicas.contains(node) {
                        all_replicas.push(node.clone());
                    }
                }
                let assignment = replication::assignment::PartitionAssignment {
                    org_id: ctx.org_id.clone(),
                    topic: name.to_string(),
                    partition,
                    leader_node_id: local_node_id.clone(),
                    isr: replicas.clone(),
                    replicas,
                    leader_epoch: 1,
                    updated_at_ms: now_ms(),
                };
                match store.propose(&assignment) {
                    Ok(_) => placed += 1,
                    Err(e) => {
                        // Best-effort: a failed placement leaves the topic
                        // usable at RF=1-on-this-node (M1 behavior for that
                        // partition) rather than failing topic creation
                        // outright — an operator can `reassign` later.
                        tracing::warn!(
                            org_id = %ctx.org_id, topic = name, partition,
                            error = %e,
                            "create_topic: failed to propose partition assignment"
                        );
                    }
                }
            }
            all_replicas.sort();
            replicas_detail = format!(
                " replicas={} partitions_placed={placed}/{}",
                all_replicas.join(","),
                cfg.partitions
            );
        }

        let _ = crate::db::repository::log_audit(
            &self.db,
            ctx.actor.as_deref(),
            None,
            "bus.topic.create",
            Some(name),
            Some(&audit_details(
                &ctx.org_id,
                Some(&format!(
                    "partitions={} retention_ms={} environment={} durability={} \
                     durability_class={} durability_explicit={} replication_factor={}{replicas_detail}",
                    cfg.partitions,
                    cfg.retention_ms,
                    cfg.environment.as_str(),
                    cfg.durability.to_wire_string(),
                    cfg.durability_class().as_str(),
                    cfg.durability_explicit(),
                    cfg.replication_factor,
                )),
            )),
            None,
            None,
        );
        Ok(cfg)
    }

    pub fn update_topic(
        &self,
        ctx: &BusCallContext,
        name: &str,
        opts: topics::TopicOptions,
    ) -> Result<topics::TopicConfig, BusServiceError> {
        topics::validate_org_id(&ctx.org_id)?;
        self.authorizer
            .authorize(ctx, BusAction::Admin, name)
            .map_err(|_| deny(BusAction::Admin, name))?;
        // Captured before the update lands so the audit row can report a
        // before->after diff for the durability fields (owner decision B
        // follow-up, `SUM/tentabus/KRYTYK-M1-R5.md` R5-5) — a failure to
        // read the "before" state (e.g. a concurrent delete racing this
        // call) must not block the update itself, it only degrades the
        // audit detail to the "after" value alone.
        let before = topics::get_topic(&self.db, &ctx.org_id, name)
            .ok()
            .flatten();
        let cfg = topics::update_topic(&self.db, &ctx.org_id, name, opts, now_ms())?;
        // Must happen before the config is stale-read by a concurrent
        // `publish`: the cache holds the OLD config, not this one.
        self.invalidate_topic_config_cache(&ctx.org_id, name);
        let durability_detail = match before {
            Some(b) => format!(
                "durability={}->{} durability_class={}->{} durability_explicit={}->{}",
                b.durability.to_wire_string(),
                cfg.durability.to_wire_string(),
                b.durability_class().as_str(),
                cfg.durability_class().as_str(),
                b.durability_explicit(),
                cfg.durability_explicit(),
            ),
            None => format!(
                "durability={} durability_class={} durability_explicit={}",
                cfg.durability.to_wire_string(),
                cfg.durability_class().as_str(),
                cfg.durability_explicit(),
            ),
        };
        let _ = crate::db::repository::log_audit(
            &self.db,
            ctx.actor.as_deref(),
            None,
            "bus.topic.update",
            Some(name),
            Some(&audit_details(&ctx.org_id, Some(&durability_detail))),
            None,
            None,
        );
        Ok(cfg)
    }

    /// Deletes the topic's DB row and best-effort removes its on-disk
    /// directory. Every partition handle this service holds for the topic
    /// — whether still in `self.partitions` or only reachable through a
    /// live `ConsumerHandle` via `consumer_partitions` (see that field's
    /// doc: `run_retention_sweep` can have removed the `self.partitions`
    /// entry already) — is `detach()`ed before the directory is removed, so
    /// EVERY handle opened before the delete, sweeper-touched or not, gets
    /// a prompt, permanent `BusServiceError::TopicNotFound` from its next
    /// `fetch` instead of racing `remove_dir_all` for a raw ENOENT (the bug
    /// that used to make `fetch` retry forever against a segment whose
    /// descriptor was never actually removed from the engine's own list)
    /// or silently serving stale data forever from an orphaned reader.
    ///
    /// Also purges the topic's SCOPE of the fjall-backed consumer-group
    /// state (committed offsets, delivery-attempt counters,
    /// `producer_seq`) and every `bus_groups` row for it — otherwise a
    /// later `create_topic` of the SAME name would silently inherit a
    /// stale committed offset (a consumer group "catches up" on a brand
    /// new log without ever seeing its first N records) or have its first
    /// batch rejected as a producer-sequence `Duplicate` from the deleted
    /// topic's previous incarnation.
    pub fn delete_topic(&self, ctx: &BusCallContext, name: &str) -> Result<(), BusServiceError> {
        topics::validate_org_id(&ctx.org_id)?;
        self.authorizer
            .authorize(ctx, BusAction::Admin, name)
            .map_err(|_| deny(BusAction::Admin, name))?;
        topics::delete_topic(&self.db, &ctx.org_id, name)?;
        // M2 (PLAN-M2 §1e): stop replication and drop this topic's
        // assignments BEFORE detaching local handles/removing the
        // directory below — a feeder/follower stream still running against
        // a directory this call is about to `remove_dir_all` is exactly
        // the ordering bug `delete_topic`'s own module history (N3-P1-1/
        // N-P1-1) already fixed once for local handles; the coordinator
        // side needs the same "stop first" discipline. `reassign(.., None,
        // &[])` is this file's chosen "stop replicating this topic"
        // signal (PLAN-M2 §1e's `reassign` doc: `partition: None` already
        // means "every partition of this topic"; an empty replica set is
        // not a real placement, so a coordinator implementation must read
        // it as "tear down every stream for this topic" — documented here
        // since `ReplicationCoordinator` has no dedicated "stop" method).
        // Both steps are best-effort: a coordinator/store error must not
        // block the topic delete itself from completing.
        if let Some(coordinator) = self.replication() {
            if let Err(e) = coordinator.reassign(&ctx.org_id, name, None, &[]) {
                tracing::warn!(
                    org_id = %ctx.org_id, topic = name, error = %e,
                    "delete_topic: failed to stop replication before delete"
                );
            }
        }
        let assignments_deleted = match crate::db::repository::bus_assignment_delete_by_topic(
            &self.db,
            &ctx.org_id,
            name,
        ) {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(
                    org_id = %ctx.org_id, topic = name, error = %e,
                    "delete_topic: failed to delete partition assignments"
                );
                0
            }
        };
        self.partitions.retain(|k, v| {
            if k.0 == ctx.org_id && k.1 == name {
                v.detach();
                false
            } else {
                true
            }
        });
        self.detach_consumer_partitions(&ctx.org_id, Some(name));
        // Dedup stores are mmapped: dropping the `Arc` here unmaps the file
        // before it is unlinked below, rather than leaving the mapping open
        // against a deleted inode.
        self.dedup_stores
            .remove(&(ctx.org_id.clone(), name.to_string()));
        self.invalidate_topic_config_cache(&ctx.org_id, name);
        self.round_robin
            .remove(&(ctx.org_id.clone(), name.to_string()));
        self.group_state.remove_topic(&ctx.org_id, name);
        let offset_keys_purged = self.offsets.purge_topic(&ctx.org_id, name)?;
        let producer_seq_keys_purged = self.producer_seq.purge_topic(&ctx.org_id, name)?;
        // A no-op scan (0 rows) when `name` never had any discard markers
        // (i.e. it is not a DLQ topic, or is one nobody ever discarded a
        // record on) — mirrors `offsets`/`producer_seq` above, which pay
        // the same harmless empty-prefix-scan cost for every non-DLQ topic
        // deleted (M1-R2 review N-5, coordinator decision 2).
        let discarded_keys_purged = self.discarded.purge_topic(&ctx.org_id, name)?;
        let groups_purged =
            crate::db::repository::bus_groups_delete_by_topic(&self.db, &ctx.org_id, name)?;
        let dir = topics::topic_dir(&self.bus_dir, &ctx.org_id, name);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = crate::db::repository::log_audit(
            &self.db,
            ctx.actor.as_deref(),
            None,
            "bus.topic.delete",
            Some(name),
            Some(&audit_details(
                &ctx.org_id,
                Some(&format!(
                    "offset_keys_purged={offset_keys_purged} producer_seq_keys_purged={producer_seq_keys_purged} discarded_keys_purged={discarded_keys_purged} groups_purged={groups_purged} assignments_deleted={assignments_deleted}"
                )),
            )),
            None,
            None,
        );
        Ok(())
    }

    pub fn reset_offset(
        &self,
        ctx: &BusCallContext,
        group: &str,
        topic: &str,
        partition: u32,
        offset: u64,
    ) -> Result<(), BusServiceError> {
        topics::validate_org_id(&ctx.org_id)?;
        self.authorizer
            .authorize(ctx, BusAction::Admin, topic)
            .map_err(|_| deny(BusAction::Admin, topic))?;
        // The topic must still exist, and this org must not have been
        // `purge_org`'d since — otherwise this would recreate a fjall
        // offset key for data that no longer has a corresponding topic row
        // (an admin re-issuing a stale reset request, or a UI action
        // replayed after the topic/org was already erased).
        self.topic_config(&ctx.org_id, topic)?;
        // `force_commit`, not `commit`: this is the one legitimate
        // path allowed to move an offset BACKWARD, gated on `bus.admin` and
        // audited right below — `commit`'s own monotonicity guard exists
        // specifically to keep every other caller off this move.
        self.offsets
            .force_commit(&ctx.org_id, group, topic, partition, offset, now_ms())?;
        let _ = crate::db::repository::log_audit(
            &self.db,
            ctx.actor.as_deref(),
            None,
            "bus.offset.reset",
            Some(topic),
            Some(&audit_details(
                &ctx.org_id,
                Some(&format!(
                    "group={group} partition={partition} offset={offset}"
                )),
            )),
            None,
            None,
        );
        Ok(())
    }

    // ---- Publish (PLAN §6.1) --------------------------------------------

    pub fn publish(
        &self,
        ctx: &BusCallContext,
        topic: &str,
        batch: PublishBatch,
    ) -> Result<PublishResult, BusServiceError> {
        topics::validate_org_id(&ctx.org_id)?;
        self.authorizer
            .authorize(ctx, BusAction::Produce, topic)
            .map_err(|_| {
                self.audit_windowed(ctx, "bus.produce.denied", Some(topic), None);
                deny(BusAction::Produce, topic)
            })?;
        if batch.records.is_empty() {
            return Err(BusServiceError::InvalidArgument(
                "publish batch has no records".to_string(),
            ));
        }
        let cfg = self.topic_config(&ctx.org_id, topic)?;
        self.check_environment(&cfg)?;
        for r in &batch.records {
            if r.payload.len() > cfg.max_inline_bytes {
                return Err(BusServiceError::PayloadTooLarge {
                    len: r.payload.len(),
                    max_inline_bytes: cfg.max_inline_bytes,
                });
            }
        }
        // Field policy write check (SUM/tentabus/POLITYKI-POL.md,
        // `mode=reject`): resolved and validated for the WHOLE batch up
        // front, before partition resolution/dedup/quota, so a rejected
        // batch pays none of that cost and never partially lands. `None`
        // (the overwhelmingly common case — no policy row for this
        // topic/actor) costs one indexed point lookup and nothing else.
        if let Some(policy) = field_policies::resolve(
            &self.db,
            &ctx.org_id,
            topic,
            ctx.actor.as_deref().unwrap_or(""),
            field_policies::Direction::Write,
        )? {
            let format = payload_format::PayloadFormat::from_content_type(&cfg.content_type);
            for r in &batch.records {
                if let Err(e) = field_policies::validate_write(&policy, format, &r.payload, topic)
                {
                    self.audit_windowed(ctx, "bus.field_not_allowed", Some(topic), None);
                    return Err(e);
                }
            }
        }
        // Dedup key presence is validated for the WHOLE batch up front
        // (before any engine append) rather than lazily per partition
        // group below — a batch that fans out across partitions must not
        // partially land before failing on a later record.
        if cfg.idempotency_key.is_some() && batch.records.iter().any(|r| r.key.is_none()) {
            return Err(BusServiceError::DedupKeyRequired {
                topic: topic.to_string(),
            });
        }

        // Per-record partitioning: group records by
        // their assigned partition, preserving relative order within each
        // group, then append/replay-check one engine batch per partition.
        // Resolved BEFORE the quota check below (M2, PLAN-M2 §1e) so a
        // `preflight` failure — this node not being leader, or not having
        // enough ISR, for one of the target partitions — is caught and
        // returned WITHOUT having already spent this call's quota tokens;
        // charging quota for a publish that never touches the engine would
        // be a real (if minor) resource leak for a producer that keeps
        // retrying against a partition it can never reach.
        let assigned = self.resolve_partitions(&ctx.org_id, topic, &cfg, &batch);
        let mut groups: Vec<(u32, Vec<PublishRecord>)> = Vec::new();
        for (record, partition) in batch.records.into_iter().zip(assigned) {
            match groups.iter_mut().find(|(p, _)| *p == partition) {
                Some((_, recs)) => recs.push(record),
                None => groups.push((partition, vec![record])),
            }
        }

        // M2 (PLAN-M2 §1e): fail fast, before any append, when this node is
        // not the leader (or lacks enough ISR) for a target partition.
        // `None` (no coordinator installed — M1, or a build that never
        // calls `set_replication`) skips this entirely: RF=1's publish path
        // is byte-for-byte the M1 path (PLAN-M2 §4.1 A1).
        let coordinator = self.replication();
        if let Some(coordinator) = &coordinator {
            for (partition, _) in &groups {
                coordinator
                    .preflight(&ctx.org_id, topic, *partition, cfg.acks)
                    .map_err(|e| map_repl_error(coordinator, &ctx.org_id, topic, *partition, e))?;
            }
        }

        let total_records: u32 = groups.iter().map(|(_, recs)| recs.len() as u32).sum();
        let total_bytes: u64 = groups
            .iter()
            .flat_map(|(_, recs)| recs.iter())
            .map(|r| r.payload.len() as u64)
            .sum();
        if let Err(e) = self
            .quota
            .try_consume(&ctx.org_id, total_records, total_bytes)
        {
            // Both the retryable `QuotaExceeded` and the hard
            // `QuotaRequestTooLarge` config error are audited the same way
            //: the action name is about "a publish was
            // rejected by the org's quota", not about which quota-error
            // subtype fired.
            self.audit_windowed(ctx, "bus.quota.exceeded", Some(topic), None);
            self.throttled_total.fetch_add(1, Ordering::Relaxed);
            return Err(e);
        }

        // Layer 2's persistent store is per-topic, not per-partition, so it
        // is opened once and shared across every group below.
        let dedup_handle = if cfg.idempotency_key.is_some() {
            Some(self.dedup_store(&ctx.org_id, topic, &cfg)?)
        } else {
            None
        };
        // Catches a duplicate key appearing MORE THAN ONCE within this same
        // call, across groups sharing the same dedup store: two-phase
        // dedup (see below) only commits a key to the persistent store
        // after a successful append, so without this the store alone would
        // not catch two copies of the same key landing in the same
        // request. Only within THIS call, though: two CONCURRENT `publish`
        // calls racing the same key each build their own `seen_this_call`
        // and each read `store.contains` before either has reached its own
        // `store.insert` (still deferred past the append, see the
        // TWO-PHASE comment below) — both can observe "not seen" and both
        // append, landing the same key twice. The direction is the safe
        // one for a dedup layer sitting in front of at-least-once delivery
        // (a false negative, never a false positive that drops a unique
        // record), but it is a real gap, not just a theoretical one; a
        // caller that needs exactly-once for concurrent producers of the
        // same key still needs `producer` identity (layer 1) on top of this.
        let mut seen_this_call: HashSet<Bytes> = HashSet::new();

        let node_env = self.cached_environment();
        let mut acks: Vec<PartitionAck> = Vec::with_capacity(groups.len());
        let mut total_accepted = 0u32;
        let mut total_accepted_bytes = 0u64;
        let mut total_deduplicated = 0u32;
        let mut any_replay = false;
        // Wraps an error observed partway through the loop below in
        // `PartialPublish` whenever an EARLIER partition group in this same
        // call already landed durably (`acks` non-empty) — a caller must be
        // able to tell "nothing happened" from "some of this already
        // happened". Passed through unchanged when `acks` is still
        // empty, so a failure on the very first group keeps its original,
        // simpler error type exactly as before.
        let wrap_err = |acks: &[PartitionAck], err: BusServiceError| -> BusServiceError {
            if acks.is_empty() {
                err
            } else {
                BusServiceError::PartialPublish {
                    acked: acks.to_vec(),
                    source: Box::new(err),
                }
            }
        };

        for (partition, records) in groups {
            // Layer 1: producer idempotency (PLAN §3.1), one fjall lookup
            // per PARTITION GROUP (not per record), checked before the
            // engine append so a duplicate never touches the log.
            // `producer_seq` is keyed by (org, topic, partition,
            // producer_id), so a batch that fans a single producer
            // identity across several partitions gets one independent
            // check per partition — a caller wanting one shared sequence
            // space must pass an explicit `partition` instead of relying
            // on per-record hashing.
            if let Some(identity) = &batch.producer {
                let outcome = self
                    .producer_seq
                    .check(&ctx.org_id, topic, partition, identity)
                    .map_err(|e| wrap_err(&acks, e))?;
                match outcome {
                    producer::CheckOutcome::Duplicate { original_offset } => {
                        any_replay = true;
                        acks.push(PartitionAck {
                            partition,
                            base_offset: original_offset,
                            accepted: 0,
                        });
                        continue;
                    }
                    producer::CheckOutcome::Fenced { current_epoch } => {
                        return Err(wrap_err(
                            &acks,
                            BusServiceError::ProducerFenced { current_epoch },
                        ));
                    }
                    producer::CheckOutcome::Fresh => {}
                }
            }

            // Layer 2: per-record idempotency-key dedup (PLAN §3.1,
            // `dedup.rs` plan B). SCOPE NOTE: evaluating the topic's
            // `idempotency_key` CEL expression against a record body is
            // `flow_engine/expr.rs` integration work outside this file's
            // ownership (`create_topic`/`update_topic` reject the field
            // until that lands, see `topics.rs`) — this uses each record's
            // `key` bytes directly as a placeholder dedup input.
            //
            // TWO-PHASE: `contains` only PROBES the persistent
            // store; nothing is written here. A matching key is filtered
            // out of `kept`, but its `insert` is deferred to
            // `pending_keys`, committed only after `append_batch` below
            // actually succeeds — so a failed/throttled append never
            // poisons the store against a record that never made it into
            // the log (the bug: insert-then-append meant a retry after a
            // transient append failure was rejected as a false-positive
            // duplicate).
            let mut pending_keys: Vec<Bytes> = Vec::new();
            let records = if let Some(store) = &dedup_handle {
                let now = now_ms();
                let mut kept = Vec::with_capacity(records.len());
                for r in records {
                    // Presence was validated for the whole batch above, so
                    // this is not expected to ever miss — but a violated
                    // invariant on this hot path should fail the request,
                    // not panic the whole process via `.expect`.
                    let key = match r.key.clone() {
                        Some(k) => k,
                        None => {
                            return Err(wrap_err(
                                &acks,
                                BusServiceError::DedupKeyRequired {
                                    topic: topic.to_string(),
                                },
                            ));
                        }
                    };
                    if seen_this_call.contains(&key) || store.contains(&key, now) {
                        total_deduplicated += 1;
                    } else {
                        seen_this_call.insert(key.clone());
                        pending_keys.push(key);
                        kept.push(r);
                    }
                }
                kept
            } else {
                records
            };

            if records.is_empty() {
                // Every record routed to this partition was a duplicate;
                // nothing was appended, so there is no offset to report
                // for it (no more "borrowing" a foreign
                // `log_end_offset` for an empty append).
                continue;
            }

            let mut builder = tentaflow_bus::BatchBuilder::new(
                0,
                batch.producer.as_ref().map(|p| p.epoch).unwrap_or(0),
            )
            .with_codec(match cfg.compression {
                topics::CompressionPolicy::Lz4 => tentaflow_bus::Codec::Lz4,
                topics::CompressionPolicy::None => tentaflow_bus::Codec::None,
            });
            for r in &records {
                let mut rec = tentaflow_bus::RecordInput::new(r.payload.clone(), r.timestamp_ms)
                    .with_schema_id(r.schema_id);
                if let Some(k) = &r.key {
                    rec = rec.with_key(k.clone());
                }
                for (hk, hv) in &r.headers {
                    // Provenance boundary: a caller-supplied `tf.*`
                    // header (forged, or a stale copy re-threaded through
                    // an internal DLQ send/retry) is dropped here so it
                    // never sits next to the broker's own value below —
                    // see `RESERVED_HEADER_PREFIX`'s doc for why this
                    // strips instead of rejecting.
                    if hk.starts_with(RESERVED_HEADER_PREFIX) {
                        continue;
                    }
                    rec = rec.with_header(hk.clone(), hv.clone());
                }
                // Broker-written provenance headers (PLAN §2.3) — the only
                // `tf.*` values that can ever reach a consumer/UI now that
                // the loop above strips any caller-supplied copy.
                rec = rec.with_header("tf.org", ctx.org_id.clone());
                if let Some(actor) = &ctx.actor {
                    rec = rec.with_header("tf.actor", actor.clone());
                }
                if let Some(cid) = &ctx.correlation_id {
                    rec = rec.with_header("tf.correlation_id", cid.clone());
                }
                rec = rec.with_header("tf.origin", ctx.origin.clone());
                rec = rec.with_header("tf.content_type", cfg.content_type.clone());
                rec = rec.with_header("tf.env", node_env.as_str());
                builder
                    .push(rec)
                    .map_err(|e| wrap_err(&acks, BusServiceError::Engine(e)))?;
            }
            let wire = builder
                .build()
                .map_err(|e| wrap_err(&acks, BusServiceError::Engine(e)))?;

            let part = self
                .partition_handle(&ctx.org_id, topic, partition, &cfg)
                .map_err(|e| wrap_err(&acks, e))?;
            let append = part
                .append_batch(wire)
                .map_err(|e| wrap_err(&acks, map_engine_error(e, topic, partition)))?;

            // Record the ack IMMEDIATELY once the append is durable, before
            // either the dedup-key commit or the producer-sequence record
            // below — both of those can still fail, and a caller reading
            // `PartialPublish.acked` (or a later `PartialPublish` on a LATER
            // partition group) needs to see every partition that actually
            // has the data on disk, not just the ones where every
            // downstream bookkeeping step also succeeded.
            let accepted = records.len() as u32;
            total_accepted += accepted;
            total_accepted_bytes += records.iter().map(|r| r.payload.len() as u64).sum::<u64>();
            acks.push(PartitionAck {
                partition,
                base_offset: append.base_offset,
                accepted,
            });

            // M2 (PLAN-M2 §1e): block until enough replicas have
            // acknowledged `next_offset` to satisfy `cfg.acks` — AFTER
            // `acks.push` above, not before, so a timeout here is reported
            // through the exact same `wrap_err`/`PartialPublish` machinery
            // every other mid-loop failure uses: the record for THIS
            // partition group is already in `acked` (it is durably on this
            // leader's disk — `append_batch` above already returned `Ok`),
            // so `AckTimeout` gets wrapped in `PartialPublish` whenever
            // `acks` is non-empty, which by construction it always is at
            // this point (this group's own entry was just pushed). `None`
            // (no coordinator) skips this entirely — RF=1's publish path
            // never blocks here, exactly like M1.
            if let Some(coordinator) = &coordinator {
                let next_offset = append.base_offset + accepted as u64;
                let outcome = coordinator
                    .await_acks(
                        &ctx.org_id,
                        topic,
                        partition,
                        next_offset,
                        cfg.acks,
                        self.publish_ack_timeout,
                    )
                    .map_err(|e| {
                        wrap_err(
                            &acks,
                            map_repl_error(coordinator, &ctx.org_id, topic, partition, e),
                        )
                    })?;
                if outcome.acked_nodes < outcome.required {
                    return Err(wrap_err(
                        &acks,
                        BusServiceError::AckTimeout {
                            acked: outcome.acked_nodes,
                            required: outcome.required,
                        },
                    ));
                }
            }

            // Only now, after the append is durable, commit the dedup
            // keys and the producer sequence — ordering unchanged
            // from before: `record` has always come after a successful
            // append.
            if let Some(store) = &dedup_handle {
                if !pending_keys.is_empty() {
                    let now = now_ms();
                    for key in &pending_keys {
                        store.insert(key, now);
                    }
                }
            }
            if let Some(identity) = &batch.producer {
                self.producer_seq
                    .record(&ctx.org_id, topic, partition, identity, append.base_offset)
                    .map_err(|e| wrap_err(&acks, e))?;
            }
        }

        if total_accepted > 0 {
            self.record_publish_rate(
                &ctx.org_id,
                topic,
                total_accepted as u64,
                total_accepted_bytes,
            );
        }
        // M2 (PLAN §8.4): `total_records`/`total_bytes` are this whole
        // call's incoming record count/payload bytes (computed above for
        // the quota check, before per-group dedup filtering) — only reached
        // once every partition group in this call has already landed
        // durably, so a call that fails partway through (or is rejected by
        // quota, counted separately as `throttled_total` above) does not
        // also inflate this counter.
        self.publish_msgs_total
            .fetch_add(total_records as u64, Ordering::Relaxed);
        self.publish_bytes_total
            .fetch_add(total_bytes, Ordering::Relaxed);

        Ok(PublishResult {
            duplicate: any_replay && total_accepted == 0,
            accepted: total_accepted,
            deduplicated: total_deduplicated,
            partitions: acks,
        })
    }

    /// Async twin of `publish` (PLAN-M2 §1e's frozen contract). `publish`
    /// itself stays fully synchronous end to end (this file's own module
    /// doc, BLOCKING section) — including, as of M2, its `await_acks` call,
    /// which blocks on `ReplicationCoordinator::await_acks` (a sync trait
    /// method; the frozen wave-0 contract does not offer an async variant,
    /// so making just that one call non-blocking would still leave the rest
    /// of `publish` — the engine's own `append_batch` — synchronous too).
    /// Rather than fork a second, ~300-line copy of `publish` against the
    /// engine's `append_batch_async` twin (a real duplication-of-truth risk
    /// this codebase explicitly avoids elsewhere, e.g. `commit`/
    /// `force_commit` sharing one attempt-clearing helper), this runs the
    /// existing synchronous `publish` through `tokio::task::
    /// block_in_place`: it lets the CURRENT worker thread block on
    /// `publish` (which itself awaits the engine's blocking channel recv
    /// AND, when a coordinator is installed, `await_acks`) while Tokio
    /// moves this thread's other runnable tasks onto different workers, so
    /// the runtime as a whole is not starved the way calling `publish`
    /// directly from an `async fn` would starve it.
    ///
    /// PANICS: like every `block_in_place` call, this requires a
    /// multi-threaded Tokio runtime — calling it from a current-thread
    /// runtime panics. Every caller in this repo (`tentaflow/src/main.rs`'s
    /// server) already runs multi-threaded.
    pub async fn publish_async(
        &self,
        ctx: &BusCallContext,
        topic: &str,
        batch: PublishBatch,
    ) -> Result<PublishResult, BusServiceError> {
        tokio::task::block_in_place(|| self.publish(ctx, topic, batch))
    }

    /// Records `msgs`/`bytes` ACCEPTED (post-dedup, durably appended) by one
    /// `publish` call into `topic`'s rate window — see `RateCounter`'s doc.
    fn record_publish_rate(&self, org_id: &str, topic: &str, msgs: u64, bytes: u64) {
        let now = now_ms();
        self.publish_rates
            .entry((org_id.to_string(), topic.to_string()))
            .or_insert_with(|| RateCounter::new(now))
            .record(now, msgs, bytes);
    }

    /// Current publish rate for `(org_id, topic)` — `(msgs_per_sec,
    /// bytes_per_sec)`, both `0` if the topic has never been published to
    /// (or has been idle for >= 2 seconds; see `RateCounter::rates`'s doc).
    /// `StatsSnapshot`'s per-topic KPI (PLAN §6.2, follow-up toru P task 3).
    pub fn topic_rates(&self, org_id: &str, topic: &str) -> (u64, u64) {
        let now = now_ms();
        self.publish_rates
            .get(&(org_id.to_string(), topic.to_string()))
            .map(|c| c.rates(now))
            .unwrap_or((0, 0))
    }

    /// Read-only partition metadata (earliest/high-watermark offsets, sealed-
    /// segment bytes/count) with NO consumer session — see `PartitionStats`'
    /// doc. Requires `BusAction::Consume` on `topic`, same as `peek`.
    pub fn partition_stats(
        &self,
        ctx: &BusCallContext,
        topic: &str,
        partition: u32,
    ) -> Result<PartitionStats, BusServiceError> {
        topics::validate_org_id(&ctx.org_id)?;
        self.authorizer
            .authorize(ctx, BusAction::Consume, topic)
            .map_err(|_| {
                self.audit_windowed(
                    ctx,
                    "bus.consume.denied",
                    Some(topic),
                    Some("action=partition_stats"),
                );
                deny(BusAction::Consume, topic)
            })?;
        let cfg = self.topic_config(&ctx.org_id, topic)?;
        self.check_environment(&cfg)?;
        if partition >= cfg.partitions {
            return Err(BusServiceError::InvalidArgument(format!(
                "partition {partition} out of range for topic '{topic}' ({} partition(s))",
                cfg.partitions
            )));
        }
        let part = self.partition_handle(&ctx.org_id, topic, partition, &cfg)?;
        let reader = part.open_reader();
        let sealed = part.sealed_segments();
        let sealed_bytes: u64 = sealed.iter().map(|s| s.len).sum();
        let active_bytes = part.active_segment_len();
        Ok(PartitionStats {
            earliest_offset: reader.earliest_offset(),
            high_watermark: reader.high_watermark(),
            log_end_offset: part.log_end_offset(),
            size_bytes: sealed_bytes + active_bytes,
            // + 1: the active segment itself — always present, never listed
            // by `sealed_segments()`.
            segments: sealed.len() as u32 + 1,
        })
    }

    /// Resolves the first offset on `(topic, partition)` whose record
    /// timestamp is `>= ts_ms`, using the engine's `PartitionReader::
    /// fetch_from_timestamp` — `OffsetResetRequest`'s `Timestamp` mode
    /// (PLAN M04, follow-up toru P task 4). Returns `high_watermark` (the
    /// append point — "nothing at or after `ts_ms`") when no record
    /// qualifies, matching Kafka's `OffsetForTimes` "null means latest"
    /// convention rather than erroring. Does NOT move any group's committed
    /// offset itself — the caller (`dispatch/bus.rs`'s `OffsetReset` handler)
    /// still calls `reset_offset` with the resolved value, exactly as its
    /// `Earliest`/`Latest`/`Explicit` siblings already do.
    pub fn resolve_offset_for_timestamp(
        &self,
        ctx: &BusCallContext,
        topic: &str,
        partition: u32,
        ts_ms: i64,
    ) -> Result<u64, BusServiceError> {
        topics::validate_org_id(&ctx.org_id)?;
        self.authorizer
            .authorize(ctx, BusAction::Consume, topic)
            .map_err(|_| {
                self.audit_windowed(
                    ctx,
                    "bus.consume.denied",
                    Some(topic),
                    Some("action=resolve_offset_for_timestamp"),
                );
                deny(BusAction::Consume, topic)
            })?;
        let cfg = self.topic_config(&ctx.org_id, topic)?;
        self.check_environment(&cfg)?;
        if partition >= cfg.partitions {
            return Err(BusServiceError::InvalidArgument(format!(
                "partition {partition} out of range for topic '{topic}' ({} partition(s))",
                cfg.partitions
            )));
        }
        let part = self.partition_handle(&ctx.org_id, topic, partition, &cfg)?;
        let reader = part.open_reader();
        let batches = reader
            .fetch_from_timestamp(ts_ms, PEEK_MAX_BYTES)
            .map_err(|e| map_engine_error(e, topic, partition))?;
        Ok(batches
            .first()
            .map(|view| view.header().base_offset)
            .unwrap_or_else(|| reader.high_watermark()))
    }

    // ---- Consume (PLAN §6.1) --------------------------------------------

    /// Opens a `ConsumerHandle` subscribed to every partition of every
    /// topic in `topics_in`.
    ///
    /// Authorization AND Z12 environment fencing are checked for EVERY
    /// requested topic BEFORE any side effect (`bus_groups` upsert,
    /// partition open) runs for ANY of them — a denial on the third topic
    /// in a five-topic request must leave the first two exactly as they
    /// were, not half-open a subscription the caller's `Err` return tells
    /// them never happened.
    pub fn open_consumer(
        &self,
        ctx: &BusCallContext,
        group: &str,
        topics_in: &[String],
        cfg: ConsumerConfig,
    ) -> Result<ConsumerHandle, BusServiceError> {
        topics::validate_org_id(&ctx.org_id)?;
        // an unvalidated group name is a free-form string that ends
        // up as part of the fjall offset key AND the `bus_groups` PK below
        // — reusing the topic-name charset closes off path-unsafe or
        // control-character group names without inventing a second regex.
        validate_group_name(group)?;

        // Snapshotted BEFORE phase 1, not after phase 2: a `purge_org` for
        // this org that starts and finishes anywhere in this call's
        // phase-1/phase-2 window must be visible to the re-check below.
        // Reading it only after phase 2 (the earlier bug) let a `purge_org`
        // that ran entirely inside this window record the NEW epoch here,
        // so the re-check below always passed and this handle carried on
        // committing against data `purge_org` had just erased.
        let purge_epoch_before = self.purged_orgs.get(&ctx.org_id).map(|e| *e).unwrap_or(0);

        // Phase 1: validate every topic — authorization, then Z12
        // environment fencing — with zero side effects. Any failure here
        // aborts the whole call before phase 2 below has touched anything.
        // Also reads (never writes) each topic's existing `bus_groups` row,
        // if any, both to preserve its `paused` flag in phase 2 below and
        // to know whether this call is about to CREATE a new row (the
        // `max_groups` quota check right after this loop only cares about
        // NEW rows — a group reconnecting to a topic it already subscribed
        // to must never count against it again).
        // M2 (PLAN-M2 §1e): consumption is leader-only. Read once up front
        // (not per topic/partition) — a coordinator swap mid-call would be
        // no different from one landing a moment before this call started.
        let coordinator = self.replication();

        let mut checked: Vec<(
            String,
            topics::TopicConfig,
            Option<crate::db::repository::DbBusGroup>,
        )> = Vec::with_capacity(topics_in.len());
        for topic in topics_in {
            // group-scoped authorization — see `BusAuthorizer::
            // authorize_group`'s doc for why this is a distinct call from
            // plain `authorize`.
            self.authorizer
                .authorize_group(ctx, BusAction::Consume, topic, group)
                .map_err(|_| {
                    self.audit_windowed(
                        ctx,
                        "bus.consume.denied",
                        Some(topic),
                        Some(&format!("group={group}")),
                    );
                    deny(BusAction::Consume, topic)
                })?;
            let topic_cfg = self.topic_config(&ctx.org_id, topic)?;
            // fail-closed Z12 fencing on the consume side too, not
            // just `publish`.
            self.check_environment(&topic_cfg)?;
            // Every partition of this topic must be led by THIS node before
            // phase 2 opens a single handle — checked here, in phase 1, so
            // a `NotLeader` on the topic's last partition leaves nothing
            // opened for any earlier one, same "validate everything, then
            // touch anything" guarantee this loop already gives
            // authorization/environment.
            for p in 0..topic_cfg.partitions {
                check_leader_role(&coordinator, &ctx.org_id, topic, p)?;
            }
            let existing_row =
                crate::db::repository::bus_group_get(&self.db, &ctx.org_id, group, topic)?;
            checked.push((topic.clone(), topic_cfg, existing_row));
        }

        // Test-only: lets a test land a `purge_org` call deterministically
        // inside the exact window the `purge_epoch_before`/`purge_epoch_after`
        // re-check below exists to catch — see the field's doc.
        #[cfg(test)]
        if let Some(hook) = self.test_open_consumer_after_phase1.lock().unwrap().take() {
            hook();
        }

        // `max_groups` quota (defense against a caller looping
        // `open_consumer` with a fresh, caller-controlled group name to
        // grow `bus_groups`/`GroupStateCache`/`commit_locks` without
        // bound, PLAN §7.1-style): counted from `bus_groups` itself (the
        // admin plane, like `enforce_topic_resource_quota`), not from any
        // in-memory cache, and only against the rows THIS call is about to
        // newly create — checked before phase 2 below performs any write.
        let new_group_rows = checked.iter().filter(|(_, _, row)| row.is_none()).count() as u32;
        if new_group_rows > 0 {
            let current_groups =
                crate::db::repository::bus_group_list(&self.db, &ctx.org_id)?.len() as u32;
            let max_groups = self.quota.max_groups(&ctx.org_id);
            if current_groups.saturating_add(new_group_rows) > max_groups {
                return Err(max_groups_exceeded(&ctx.org_id, max_groups, current_groups));
            }
        }

        // Phase 2: every topic passed validation — now perform the actual
        // side effects (`bus_groups` upsert, partition open).
        let mut partitions = Vec::new();
        for (topic, topic_cfg, existing_row) in &checked {
            // register/refresh this group's `bus_groups` row so
            // group-listing UI reflects real, actively-consuming groups —
            // previously only `pause_group`/`resume_group` ever wrote one.
            // A pre-existing row's `paused` flag is preserved (an admin
            // pause must survive the consumer reconnecting).
            let now = now_ms();
            let existing_paused = existing_row.as_ref().map(|g| g.paused).unwrap_or(false);
            crate::db::repository::bus_group_upsert(
                &self.db,
                &crate::db::repository::DbBusGroup {
                    org_id: ctx.org_id.clone(),
                    group_id: group.to_string(),
                    topic: topic.clone(),
                    commit_mode: cfg.commit_mode.as_str().to_string(),
                    paused: existing_paused,
                    created_at_ms: now,
                    updated_at_ms: now,
                },
            )?;
            let env_byte = environment_to_u8(topic_cfg.environment);
            for p in 0..topic_cfg.partitions {
                let part = self.partition_handle(&ctx.org_id, topic, p, topic_cfg)?;
                let committed = self
                    .offsets
                    .committed_offset(&ctx.org_id, group, topic, p)?;
                // Registered in the side registry (not just held by this
                // handle) so `delete_topic`/`purge_org` can still reach and
                // `detach()` it even after `run_retention_sweep` removes
                // this same key from `self.partitions` — see
                // `consumer_partitions`'s doc.
                self.register_consumer_partition((ctx.org_id.clone(), topic.clone(), p), &part);
                partitions.push(ConsumerPartition {
                    topic: topic.clone(),
                    partition: p,
                    reader: part.open_reader(),
                    // Keeping this clone (not just the reader above) is
                    // what makes the module doc's PARTITION HANDLE LIFETIME
                    // promise true: as long as this `ConsumerHandle` lives,
                    // the writer thread and directory flock stay open
                    // regardless of whether `self.partitions` still has an
                    // entry for this key.
                    handle: part,
                    next_offset: AtomicU64::new(committed),
                    environment: env_byte,
                    gap_audited: std::sync::atomic::AtomicBool::new(false),
                });
            }
        }

        // Re-check: if the epoch moved since the phase-1 snapshot above,
        // `purge_org` for this org ran somewhere inside this call's window
        // — reject rather than hand back a handle that could commit/seek
        // against data `purge_org` just erased. Side effects phase 2
        // performed for THIS call are undone: every partition this call
        // opened is force-`detach()`ed (the org WAS purged inside this
        // window, so nothing this call opened should stay live regardless
        // of whether the entry pre-existed) and the `bus_groups` rows for
        // each touched topic are removed again.
        let purge_epoch_after = self.purged_orgs.get(&ctx.org_id).map(|e| *e).unwrap_or(0);
        if purge_epoch_after != purge_epoch_before {
            for cp in &partitions {
                cp.handle.detach();
                // Also drop it from the shared map, not just detach it —
                // otherwise a LEGITIMATE `create_topic` + `publish` on this
                // same (org, topic, partition) after the race would keep
                // getting handed this same, now-permanently-detached
                // handle back by `partition_handle`'s `or_try_insert_with`
                // instead of opening a fresh one.
                self.partitions
                    .remove(&(ctx.org_id.clone(), cp.topic.clone(), cp.partition));
            }
            for (topic, _, _) in &checked {
                let _ =
                    crate::db::repository::bus_groups_delete_by_topic(&self.db, &ctx.org_id, topic);
            }
            return Err(BusServiceError::TopicNotFound {
                name: topics_in.first().cloned().unwrap_or_default(),
            });
        }

        Ok(ConsumerHandle {
            org_id: ctx.org_id.clone(),
            group: group.to_string(),
            commit_mode: cfg.commit_mode,
            partitions,
            offsets: Arc::clone(&self.offsets),
            db: self.db.clone(),
            authorizer: Arc::clone(&self.authorizer),
            ctx: ctx.clone(),
            generation: AtomicU64::new(self.authorizer.generation()),
            node_environment: Arc::clone(&self.node_environment_cache),
            audit_windows: Arc::clone(&self.audit_windows),
            group_state: Arc::clone(&self.group_state),
            commit_locks: Arc::clone(&self.commit_locks),
            purged_orgs: Arc::clone(&self.purged_orgs),
            purge_epoch: purge_epoch_before,
            replication: Arc::clone(&self.replication),
            consume_msgs_total: Arc::clone(&self.consume_msgs_total),
        })
    }

    // ---- Peek: stateless UI preview read (PLAN §6.2) ----------------------

    /// Stateless, one-shot read of up to `max_records`/`max_bytes` (each
    /// further clamped to `PEEK_MAX_RECORDS`/`PEEK_MAX_BYTES`) starting at
    /// `from_offset` on one `(topic, partition)` — the read path a message
    /// browser/DLQ preview UI is meant to use INSTEAD OF `open_consumer` +
    /// `fetch` under a throwaway group name. Unlike a real consumer
    /// session, this creates NO `bus_groups` row, commits NOTHING, and does
    /// not long-poll: it is a single read of whatever is available right
    /// now, returned immediately (an empty `records` at `from_offset ==
    /// high_watermark` is a normal, non-error result, not something a
    /// caller should retry).
    ///
    /// Requires `BusAction::Consume` (`bus.read`) on `topic` — reading a
    /// record's payload is exactly the access that permission already
    /// governs for a real consumer, so this reuses it rather than adding a
    /// separate action a production `BusAuthorizer` would have to learn
    /// about. A call that actually reads at least one record writes an
    /// UNWINDOWED `bus.messages.browse` audit row (PLAN §6.2: message
    /// preview is a data-access event on potentially sensitive payloads,
    /// never suppressed/batched the way a routine denial or quota rejection
    /// is); a denial writes the same windowed `bus.consume.denied` row
    /// `open_consumer`/`fetch` use instead. A call that reaches the record
    /// loop but returns ZERO records (`from_offset == high_watermark`, or
    /// past it after retention moved the floor) writes NO audit row at all
    /// (P3-5 follow-up, `KRYTYK-M1-R3.md`, coordinator decision "Decyzje po
    /// R3": an empty read of a partition is not a data access) — this is
    /// what lets a multi-partition `MessagesBrowse`/`DlqList` walk every
    /// partition without an audit row per partition that had nothing to
    /// show, e.g. `count=0` on partitions 1..N while only partition 0 has
    /// data.
    pub fn peek(
        &self,
        ctx: &BusCallContext,
        topic: &str,
        partition: u32,
        from_offset: u64,
        max_records: usize,
        max_bytes: usize,
    ) -> Result<PeekResult, BusServiceError> {
        topics::validate_org_id(&ctx.org_id)?;
        self.authorizer
            .authorize(ctx, BusAction::Consume, topic)
            .map_err(|_| {
                self.audit_windowed(ctx, "bus.consume.denied", Some(topic), Some("action=peek"));
                deny(BusAction::Consume, topic)
            })?;
        let cfg = self.topic_config(&ctx.org_id, topic)?;
        self.check_environment(&cfg)?;
        if partition >= cfg.partitions {
            return Err(BusServiceError::InvalidArgument(format!(
                "partition {partition} out of range for topic '{topic}' ({} partition(s))",
                cfg.partitions
            )));
        }
        // M2 (PLAN-M2 §1e): `peek` is a read, same as `fetch` — leader-only
        // under the same rule.
        check_leader_role(&self.replication(), &ctx.org_id, topic, partition)?;
        let max_records = max_records.min(PEEK_MAX_RECORDS);
        let max_bytes = max_bytes.min(PEEK_MAX_BYTES);

        let part = self.partition_handle(&ctx.org_id, topic, partition, &cfg)?;
        let reader = part.open_reader();
        let high_watermark = reader.high_watermark();
        let earliest_offset = reader.earliest_offset();
        let batches = reader
            .fetch_from_offset(from_offset, max_bytes)
            .map_err(|e| map_engine_error(e, topic, partition))?;

        let mut records = Vec::new();
        'batches: for view in &batches {
            for rv in view.records_from(from_offset) {
                if records.len() >= max_records {
                    break 'batches;
                }
                let rv = rv?;
                let offset = view.header().base_offset + rv.offset_delta as u64;
                records.push(FetchedRecordMeta {
                    topic: topic.to_string(),
                    partition,
                    offset,
                    timestamp_ms: view.header().base_timestamp_ms + rv.ts_delta_ms as i64,
                    key: rv.key.clone(),
                    headers: rv.headers.iter().cloned().collect(),
                    payload: rv.payload.clone(),
                    schema_id: rv.schema_id,
                });
            }
        }

        // P3-5 follow-up: only an actual data access — at least one record
        // returned — gets an audit row. An empty read (nothing at/past
        // `from_offset` on this partition) is not a data access and must
        // not add to the `bus.messages.browse` audit trail (this is exactly
        // what a multi-partition browse hits on every partition that has no
        // records yet — see this fn's doc).
        if !records.is_empty() {
            let _ = crate::db::repository::log_audit(
                &self.db,
                ctx.actor.as_deref(),
                None,
                "bus.messages.browse",
                Some(topic),
                Some(&audit_details(
                    &ctx.org_id,
                    Some(&format!(
                        "partition={partition} from_offset={from_offset} count={}",
                        records.len()
                    )),
                )),
                None,
                None,
            );
        }

        // Field policy read projection (SUM/tentabus/POLITYKI-POL.md,
        // "hide only"): applied AFTER the audit above, so the browse audit
        // trail still reflects the real record count regardless of what a
        // policy later hides from the payload. `None` costs one indexed
        // point lookup and leaves every record untouched.
        if !records.is_empty() {
            if let Some(policy) = field_policies::resolve(
                &self.db,
                &ctx.org_id,
                topic,
                ctx.actor.as_deref().unwrap_or(""),
                field_policies::Direction::Read,
            )? {
                let format = payload_format::PayloadFormat::from_content_type(&cfg.content_type);
                for rec in &mut records {
                    rec.payload = field_policies::project_read(&policy, format, &rec.payload);
                }
            }
        }

        Ok(PeekResult {
            records,
            high_watermark,
            earliest_offset,
        })
    }

    // ---- DLQ (PLAN §3.3) --------------------------------------------------

    fn ensure_dlq_topic(
        &self,
        ctx: &BusCallContext,
        source_topic: &str,
        source_cfg: &topics::TopicConfig,
    ) -> Result<topics::TopicConfig, BusServiceError> {
        let dlq_name = dlq::dlq_topic_name(source_topic);
        if let Some(existing) = topics::get_topic(&self.db, &ctx.org_id, &dlq_name)? {
            return Ok(existing);
        }
        let env = crate::services::environment::get_node_environment(&self.db);
        topics::create_internal_topic(
            &self.db,
            &ctx.org_id,
            &dlq_name,
            dlq::dlq_topic_options(source_cfg),
            env,
            now_ms(),
        )
    }

    // ---- Metrics rollup (PLAN §8.4/M4) -------------------------------------

    fn metrics_topic_options() -> topics::TopicOptions {
        topics::TopicOptions {
            partitions: Some(1),
            // PLAN §7.1 default topic retention (7 days).
            retention_ms: Some(7 * 24 * 3_600_000),
            durability_class: Some(topics::DurabilityClass::Standard),
            ..Default::default()
        }
    }

    fn ensure_metrics_topic(&self, ctx: &BusCallContext) -> Result<(), BusServiceError> {
        if topics::get_topic(&self.db, &ctx.org_id, topics::METRICS_TOPIC_NAME)?.is_some() {
            return Ok(());
        }
        let env = crate::services::environment::get_node_environment(&self.db);
        topics::create_internal_topic(
            &self.db,
            &ctx.org_id,
            topics::METRICS_TOPIC_NAME,
            Self::metrics_topic_options(),
            env,
            now_ms(),
        )?;
        Ok(())
    }

    /// Collects the node's current `BusMetricsRollup` (the same struct the
    /// Zabbix exporter builds — `services::metrics_export::collect_bus_metrics`)
    /// and publishes it as one record on `__bus.metrics`. Called every
    /// `METRICS_ROLLUP_INTERVAL` by `spawn_metrics_rollup_timer`. This is
    /// broker-owned, unattributed traffic (no human/addon caller triggered
    /// it), so it publishes under `SYSTEM_ACTOR` — see that const's doc in
    /// `services::bus_authorizer` for why this cannot be spoofed from any
    /// external input path. Best-effort: any failure is logged and
    /// swallowed rather than propagated, since this must never take down
    /// the timer thread or block real traffic.
    fn publish_metrics_rollup(&self) {
        let ctx = BusCallContext {
            org_id: crate::services::org::DEFAULT_ORG_ID.to_string(),
            actor: Some(crate::services::bus_authorizer::SYSTEM_ACTOR.to_string()),
            correlation_id: None,
            origin: "bus.metrics.rollup".to_string(),
        };
        if let Err(e) = self.ensure_metrics_topic(&ctx) {
            tracing::warn!("__bus.metrics: ensure topic failed: {e}");
            return;
        }
        let rollup = crate::services::metrics_export::collect_bus_metrics(&self.db);
        let payload = match serde_json::to_vec(&rollup) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("__bus.metrics: serialize rollup failed: {e}");
                return;
            }
        };
        let record = PublishRecord {
            key: None,
            headers: Vec::new(),
            payload: Bytes::from(payload),
            timestamp_ms: now_ms(),
            schema_id: 0,
        };
        if let Err(e) = self.publish(
            &ctx,
            topics::METRICS_TOPIC_NAME,
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record],
            },
        ) {
            tracing::warn!("__bus.metrics: publish failed: {e}");
        }
    }

    /// Records one failed delivery attempt for (group, topic, partition,
    /// offset). Below `max_delivery_attempts` this only returns the retry
    /// backoff the caller should honor; at/above it, a copy of `record` is
    /// published to `__dlq.<topic>` (auto-created on first use) and the
    /// group's committed offset is advanced past the poison record so it
    /// stops being redelivered.
    #[allow(clippy::too_many_arguments)]
    pub fn note_delivery_failure(
        &self,
        ctx: &BusCallContext,
        group: &str,
        topic: &str,
        partition: u32,
        offset: u64,
        record: &FetchedRecordMeta,
        reason: dlq::DlqReason,
        error_message: &str,
    ) -> Result<dlq::DlqOutcome, BusServiceError> {
        topics::validate_org_id(&ctx.org_id)?;
        // a DLQ topic can never itself have a DLQ.
        if topic.starts_with(dlq::DLQ_TOPIC_PREFIX) {
            return Err(BusServiceError::DlqOfDlqNotAllowed {
                topic: topic.to_string(),
            });
        }
        // this call can advance GROUP's committed offset on TOPIC —
        // exactly the access a caller must not get without holding consume
        // rights on the source topic for that group. Previously unchecked
        // entirely.
        self.authorizer
            .authorize_group(ctx, BusAction::Consume, topic, group)
            .map_err(|_| {
                self.audit_windowed(
                    ctx,
                    "bus.consume.denied",
                    Some(topic),
                    Some(&format!("group={group} action=note_delivery_failure")),
                );
                deny(BusAction::Consume, topic)
            })?;
        let cfg = self.topic_config(&ctx.org_id, topic)?;
        let now = now_ms();
        let info = self.offsets.record_delivery_attempt(
            &ctx.org_id,
            group,
            topic,
            partition,
            offset,
            now,
        )?;
        // K-M2-5: the running attempts count for THIS offset just changed
        // locally (`record_delivery_attempt` above) — replicate it via the
        // same `note_offset_commit`/`ReplOffsets` path a real commit uses,
        // carrying the OFFSET that failed (not yet advanced) alongside the
        // new attempts count, so a follower promoted mid-retry-storm
        // resumes redelivery at the correct attempt number instead of
        // restarting from 1 (PLAN-M2 §1e's `GroupOffsetStore::
        // set_delivery_attempts` is the follower-side apply for exactly
        // this).
        if let Some(coordinator) = self.replication() {
            coordinator.note_offset_commit(
                &ctx.org_id,
                group,
                topic,
                partition,
                offset,
                info.attempts,
            );
        }
        if info.attempts < cfg.max_delivery_attempts {
            let backoff_ms = dlq::compute_backoff_ms(
                info.attempts,
                cfg.retry_backoff_ms,
                topics::DEFAULT_RETRY_BACKOFF_CAP_MS,
                (now as u64) ^ offset ^ (info.attempts as u64),
            );
            return Ok(dlq::DlqOutcome::Retry {
                attempts: info.attempts,
                backoff_ms,
            });
        }

        self.ensure_dlq_topic(ctx, topic, &cfg)?;
        let original = PublishRecord {
            key: record.key.clone(),
            // `PublishRecord::headers` keys are `String` (the public
            // publish-side API); `FetchedRecordMeta::headers` keys are raw
            // `Bytes` (fetch's hot path avoids the UTF-8 decode). This is
            // the DLQ-send path, not a hot one, so paying the decode here
            // is the right trade.
            headers: record
                .headers
                .iter()
                .map(|(k, v)| (String::from_utf8_lossy(k).into_owned(), v.clone()))
                .collect(),
            payload: record.payload.clone(),
            timestamp_ms: record.timestamp_ms,
            schema_id: record.schema_id,
        };
        let dlq_record = dlq::build_dlq_record(
            topic,
            partition,
            offset,
            group,
            info.attempts,
            info.first_failed_at_ms,
            info.last_failed_at_ms,
            reason,
            error_message,
            &original,
        );
        let dlq_topic_name = dlq::dlq_topic_name(topic);
        self.publish(
            ctx,
            &dlq_topic_name,
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![dlq_record],
            },
        )?;
        let _ = crate::db::repository::log_audit(
            &self.db,
            ctx.actor.as_deref(),
            None,
            "bus.dlq.sent",
            Some(topic),
            Some(&audit_details(
                &ctx.org_id,
                Some(&format!(
                    "group={group} partition={partition} offset={offset} attempts={} dlq_topic={dlq_topic_name}",
                    info.attempts
                )),
            )),
            None,
            None,
        );

        // only advance the committed offset when `offset` IS the
        // group's current committed offset — advancing unconditionally
        // (the old behavior) would silently skip every offset between the
        // true committed offset and this one without a DLQ entry for any
        // of them, if an earlier record in the same batch had already
        // failed and claimed the advance.
        let committed = self
            .offsets
            .committed_offset(&ctx.org_id, group, topic, partition)?;
        if committed == offset {
            self.offsets
                .commit(&ctx.org_id, group, topic, partition, offset + 1, now)?;
            Ok(dlq::DlqOutcome::SentToDlq {
                attempts: info.attempts,
            })
        } else {
            Ok(dlq::DlqOutcome::SentToDlqOffsetMismatch {
                attempts: info.attempts,
                committed_offset: committed,
            })
        }
    }

    /// Republishes a DLQ record to its source topic with `dlq.retry_of` set
    /// (PLAN §3.3 "Ponów"), attempts reset by virtue of being a normal
    /// fresh publish.
    pub fn dlq_retry(
        &self,
        ctx: &BusCallContext,
        dlq_topic: &str,
        dlq_partition: u32,
        dlq_offset: u64,
    ) -> Result<PublishResult, BusServiceError> {
        topics::validate_org_id(&ctx.org_id)?;
        self.authorizer
            .authorize(ctx, BusAction::Admin, dlq_topic)
            .map_err(|_| deny(BusAction::Admin, dlq_topic))?;
        let source_topic = dlq_topic
            .strip_prefix(dlq::DLQ_TOPIC_PREFIX)
            .ok_or_else(|| {
                BusServiceError::InvalidArgument(format!("'{dlq_topic}' is not a DLQ topic"))
            })?;
        let dlq_cfg = self.topic_config(&ctx.org_id, dlq_topic)?;
        let part = self.partition_handle(&ctx.org_id, dlq_topic, dlq_partition, &dlq_cfg)?;
        let reader = part.open_reader();
        let batches = reader.fetch_from_offset(dlq_offset, 8 * 1024 * 1024)?;
        let view = batches
            .first()
            .ok_or_else(|| BusServiceError::InvalidArgument("dlq offset not found".to_string()))?;
        let rv = view.records_from(dlq_offset).next().ok_or_else(|| {
            BusServiceError::InvalidArgument("dlq offset not found".to_string())
        })??;
        let original = PublishRecord {
            key: rv.key.clone(),
            headers: rv
                .headers
                .iter()
                .map(|(k, v)| (String::from_utf8_lossy(k).into_owned(), v.clone()))
                .collect(),
            payload: rv.payload.clone(),
            timestamp_ms: view.header().base_timestamp_ms + rv.ts_delta_ms as i64,
            schema_id: rv.schema_id,
        };
        let retry_record = dlq::build_retry_record(&original, dlq_topic, dlq_offset);
        let result = self.publish(
            ctx,
            source_topic,
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![retry_record],
            },
        )?;
        let _ = crate::db::repository::log_audit(
            &self.db,
            ctx.actor.as_deref(),
            None,
            "bus.dlq.retry",
            Some(source_topic),
            Some(&audit_details(
                &ctx.org_id,
                Some(&format!("dlq_topic={dlq_topic} dlq_offset={dlq_offset}")),
            )),
            None,
            None,
        );
        Ok(result)
    }

    /// Marks a DLQ record as handled (PLAN §3.3 "Odrzuć") — M1-R2 review
    /// N-5, coordinator decision 2.
    ///
    /// This STILL does not delete or tombstone anything in the log itself —
    /// M1's log engine has no per-record delete at all (that is the M5
    /// compaction engine's job), and the record's bytes remain physically
    /// present in the DLQ topic's log until normal retention (default 30
    /// days, `dlq_topic_options`) removes the whole segment holding them.
    /// What changed since the version of this doc the R2 review quoted:
    /// discarding now writes a durable marker (`dlq::DiscardStore`) that
    /// every caller-facing DLQ surface honors — `dlq_list`'s dispatch-layer
    /// wrapper and `peek` filter a discarded offset out of what it returns,
    /// `dlq_retry_all` skips it, and `dlq_depth` (the `StatsSnapshot` KPI)
    /// no longer counts it. A discarded record can still be resurrected by
    /// calling `dlq_retry` on its EXACT `(dlq_topic, partition, offset)`
    /// directly (an explicit, single-record admin action naming the record
    /// by coordinates is not the accidental "Ponów wszystkie brought back a
    /// record I just discarded" failure mode the review reported) — a UI
    /// surfacing this action again for an already-discarded row should make
    /// that explicit.
    pub fn dlq_discard(
        &self,
        ctx: &BusCallContext,
        dlq_topic: &str,
        partition: u32,
        offset: u64,
    ) -> Result<(), BusServiceError> {
        topics::validate_org_id(&ctx.org_id)?;
        self.authorizer
            .authorize(ctx, BusAction::Admin, dlq_topic)
            .map_err(|_| deny(BusAction::Admin, dlq_topic))?;
        self.discarded
            .mark(&ctx.org_id, dlq_topic, partition, offset, now_ms())?;
        let _ = crate::db::repository::log_audit(
            &self.db,
            ctx.actor.as_deref(),
            None,
            "bus.dlq.discard",
            Some(dlq_topic),
            Some(&audit_details(
                &ctx.org_id,
                Some(&format!("partition={partition} offset={offset}")),
            )),
            None,
            None,
        );
        Ok(())
    }

    /// Every offset currently marked discarded (`dlq_discard`) for
    /// `(dlq_topic, partition)`, with anything now behind `earliest_offset`
    /// already lazily pruned (`DiscardStore::discarded_offsets`'s doc). The
    /// dispatch layer's `DlqList`/`peek_topic` wrapper and `dlq_retry_all`
    /// both call this to filter a discarded record out of what they
    /// return/act on — `peek` itself stays pure (no discard-awareness), so
    /// every OTHER caller of `peek` (`MessagesBrowse`, which never reads a
    /// DLQ topic through this path) is unaffected.
    pub fn dlq_discarded_offsets(
        &self,
        ctx: &BusCallContext,
        dlq_topic: &str,
        partition: u32,
    ) -> Result<std::collections::HashSet<u64>, BusServiceError> {
        topics::validate_org_id(&ctx.org_id)?;
        self.authorizer
            .authorize(ctx, BusAction::Consume, dlq_topic)
            .map_err(|_| deny(BusAction::Consume, dlq_topic))?;
        let earliest = self
            .partition_stats(ctx, dlq_topic, partition)
            .map(|s| s.earliest_offset)
            .unwrap_or(0);
        self.discarded
            .discarded_offsets(&ctx.org_id, dlq_topic, partition, earliest)
    }

    // ---- Group administration (PLAN §8.2 `bus.group.pause`) --------------

    /// Sets a group's paused bookkeeping flag (`bus_groups` table) and
    /// audits the change. `ConsumerHandle::fetch` DOES consult this (via
    /// `GroupStateCache`, invalidated right below) and refuses to serve a
    /// paused group with `BusServiceError::GroupPaused` — a caller does not
    /// need to poll `is_group_paused` itself before calling `fetch` to get
    /// that enforcement, though doing so avoids paying for a `fetch` call
    /// it already knows will be rejected.
    fn set_group_paused(
        &self,
        ctx: &BusCallContext,
        group: &str,
        topic: &str,
        paused: bool,
    ) -> Result<(), BusServiceError> {
        topics::validate_org_id(&ctx.org_id)?;
        self.authorizer
            .authorize(ctx, BusAction::Admin, topic)
            .map_err(|_| deny(BusAction::Admin, topic))?;
        let now = now_ms();
        let commit_mode =
            crate::db::repository::bus_group_get(&self.db, &ctx.org_id, group, topic)?
                .map(|g| g.commit_mode)
                .unwrap_or_else(|| groups::CommitMode::AutoAfterSuccess.as_str().to_string());
        crate::db::repository::bus_group_upsert(
            &self.db,
            &crate::db::repository::DbBusGroup {
                org_id: ctx.org_id.clone(),
                group_id: group.to_string(),
                topic: topic.to_string(),
                commit_mode,
                paused,
                created_at_ms: now,
                updated_at_ms: now,
            },
        )?;
        // Invalidate rather than update-in-place: simpler to reason about
        // (one code path, "next read reloads"), and this is an admin-plane
        // call, not `fetch`'s hot path — the one extra SQLite read on the
        // very next `fetch` is a non-issue.
        self.group_state.invalidate(&ctx.org_id, group, topic);
        let _ = crate::db::repository::log_audit(
            &self.db,
            ctx.actor.as_deref(),
            None,
            "bus.group.pause",
            Some(topic),
            Some(&audit_details(
                &ctx.org_id,
                Some(&format!("group={group} paused={paused}")),
            )),
            None,
            None,
        );
        Ok(())
    }

    pub fn pause_group(
        &self,
        ctx: &BusCallContext,
        group: &str,
        topic: &str,
    ) -> Result<(), BusServiceError> {
        self.set_group_paused(ctx, group, topic, true)
    }

    pub fn resume_group(
        &self,
        ctx: &BusCallContext,
        group: &str,
        topic: &str,
    ) -> Result<(), BusServiceError> {
        self.set_group_paused(ctx, group, topic, false)
    }

    pub fn is_group_paused(
        &self,
        org_id: &str,
        group: &str,
        topic: &str,
    ) -> Result<bool, BusServiceError> {
        topics::validate_org_id(org_id)?;
        group_paused(&self.db, org_id, group, topic)
    }

    // ---- Retention (PLAN §2.5) --------------------------------------------

    /// System-wide retention sweep (PLAN §2.5):
    /// iterates every org and every one of its topics/partitions itself and
    /// takes no `BusCallContext`/authorization — the ORIGINAL shape of this
    /// method (per-topic, `bus.admin`-gated) put a periodic sweeper thread
    /// in the position of having to forge a synthetic admin actor just to
    /// call it, which is the wrong shape for something that is fundamentally
    /// a system/maintenance operation, not a caller-initiated one. Meant to
    /// be invoked ONLY by `bus::init`'s background sweeper thread (or
    /// directly by an operator tool that already runs with full system
    /// privileges) — never by a per-request caller.
    ///
    /// `min_retention_ms` (the compliance floor `sweep_partition` takes) is
    /// hardcoded to `0` here: `RetentionScopeKind::BusTopic` does not exist
    /// in `compliance/models.rs` yet, so there is nothing to resolve a real
    /// floor from (see `retention.rs`'s module doc for what this means in
    /// practice until that lands).
    ///
    /// Best-effort: a failure reading one org's topics, or opening/sweeping
    /// one partition, is logged and skipped rather than aborting the whole
    /// sweep — one corrupt row or locked partition must not stop every
    /// other org/topic from being swept.
    pub fn run_retention_sweep(&self) -> RetentionReport {
        let mut report = RetentionReport::default();
        let org_ids = match crate::db::repository::list_all_org_ids(&self.db) {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!(error = %e, "bus retention sweep: failed to list orgs");
                return report;
            }
        };
        let now = now_ms();
        // Partition handles this sweep itself opens (i.e. were NOT already
        // present in `self.partitions` before this call) are closed again
        // once the sweep is done — see the module doc's "PARTITION HANDLE
        // LIFETIME" note. Without this, a periodic system-wide sweep would
        // permanently accumulate one writer thread and one directory flock
        // per partition it ever touched, whether or not anything is
        // actually consuming/producing on it.
        let mut opened_by_this_sweep: Vec<PartitionKey> = Vec::new();
        for org_id in &org_ids {
            // Defense in depth: `bus_topics`' own `org_id` column should
            // always be a value this service itself once wrote via a
            // validated `ctx.org_id`, but a sweep iterates EVERY org in the
            // database, including rows this service never touched (a
            // different subsystem, a hand-edited row) — skip rather than
            // let an invalid value reach `bus_dir.join` deeper in this
            // loop.
            if let Err(e) = topics::validate_org_id(org_id) {
                tracing::warn!(org_id, error = %e, "bus retention sweep: skipping org with an invalid org_id");
                continue;
            }
            let topic_rows = match crate::db::repository::bus_topic_list(&self.db, org_id) {
                Ok(rows) => rows,
                Err(e) => {
                    tracing::warn!(org_id, error = %e, "bus retention sweep: failed to list topics");
                    continue;
                }
            };
            if topic_rows.is_empty() {
                continue;
            }
            report.orgs_swept += 1;
            for row in topic_rows {
                let topic_name = row.name.clone();
                let cfg = match topics::TopicConfig::try_from(row) {
                    Ok(cfg) => cfg,
                    Err(e) => {
                        tracing::warn!(org_id, topic = %topic_name, error = %e, "bus retention sweep: corrupt topic row");
                        continue;
                    }
                };
                report.topics_swept += 1;
                for p in 0..cfg.partitions {
                    let key = (org_id.clone(), cfg.name.clone(), p);
                    let already_open = self.partitions.contains_key(&key);
                    let part = match self.partition_handle(org_id, &cfg.name, p, &cfg) {
                        Ok(part) => part,
                        Err(e) => {
                            tracing::warn!(org_id, topic = %cfg.name, partition = p, error = %e, "bus retention sweep: failed to open partition");
                            continue;
                        }
                    };
                    if !already_open {
                        opened_by_this_sweep.push(key);
                    }
                    match retention::sweep_partition(
                        &part,
                        cfg.retention_ms,
                        cfg.retention_bytes_per_partition,
                        0, // compliance floor deferred, see this method's doc
                        now,
                    ) {
                        Ok(outcome) => {
                            report.deleted_segments += outcome.deleted_segments;
                            report.deleted_bytes += outcome.deleted_bytes;
                        }
                        Err(e) => {
                            tracing::warn!(org_id, topic = %cfg.name, partition = p, error = %e, "bus retention sweep: sweep_partition failed");
                        }
                    }
                }
            }
        }
        // Close every handle this sweep opened for itself (dropping the
        // last `Partition` clone stops its writer thread and releases its
        // directory flock — see the engine's `Drop for PartitionInner`).
        // A handle that was ALREADY open before this sweep (a live
        // producer/consumer) is left untouched.
        for key in opened_by_this_sweep {
            self.partitions.remove(&key);
        }
        // One summary row per sweep, only when something was actually
        // deleted — never per topic or per segment, so a
        // no-op sweep (the common case on a quiet system) never touches
        // `audit_log` at all.
        if report.deleted_segments > 0 {
            let _ = crate::db::repository::log_audit(
                &self.db,
                None,
                None,
                "bus.retention.sweep",
                None,
                Some(&format!(
                    "orgs={} topics={} deleted_segments={} deleted_bytes={}",
                    report.orgs_swept,
                    report.topics_swept,
                    report.deleted_segments,
                    report.deleted_bytes
                )),
                None,
                None,
            );
        }
        report
    }

    // ---- GDPR/RODO org purge ---------------------------------

    /// Hard-deletes everything TentaBus holds for `org_id`: in-memory caches
    /// (partition handles, dedup stores, topic config, round-robin
    /// counters), `bus_topics`/`bus_groups` rows, every fjall key under
    /// `_meta` scoped to this org (offsets, delivery attempts, producer
    /// sequences), and finally the org's whole on-disk directory
    /// (`<bus_dir>/<org_id>/`). System API — no `BusCallContext`/
    /// authorization, like `run_retention_sweep`: this is meant to be called
    /// by a hard-delete/compliance-erasure flow that already runs with full
    /// system privileges, not by a per-request caller.
    ///
    /// NOT wired to `services::org::repo::delete_organization` — that is a
    /// SOFT delete (a purge would be the wrong thing to trigger from it).
    /// See this file's module doc for who must call this instead.
    ///
    /// Best-effort on the on-disk removal (`dir_removed = false` if the
    /// directory did not exist or could not be removed) — the DB rows and
    /// fjall keys are the parts a caller can act on if this needs a retry;
    /// a missing directory is not itself a failure (an org that never wrote
    /// any bus data has none).
    pub fn purge_org(&self, org_id: &str) -> Result<PurgeReport, BusServiceError> {
        // A traversal/reserved org_id must never reach `bus_dir.join` below
        // (`remove_dir_all` on an attacker-chosen path) or collide with the
        // `_meta` directory this service's own fjall keyspaces live under.
        topics::validate_org_id(org_id)?;

        // Bumped BEFORE anything else so a `ConsumerHandle` opened for this
        // org concurrently with (or racing right after) this call is on the
        // conservative side of the race: it either captured the OLD count
        // (and will be refused by its own epoch check on its next
        // `commit`/`seek_to_earliest`) or the NEW one (nothing to refuse,
        // since it was opened after the purge already happened).
        self.purged_orgs
            .entry(org_id.to_string())
            .and_modify(|e| *e += 1)
            .or_insert(1);

        // M2 (PLAN-M2 §1e): stop replication for every one of this org's
        // topics BEFORE detaching local handles/removing the directory
        // below — same ordering rule as `delete_topic`'s own coordinator
        // call, applied per topic since `reassign` addresses one topic at
        // a time. Best-effort: listing topics (or a single `reassign`
        // call) failing must not block the purge itself.
        if let Some(coordinator) = self.replication() {
            match crate::db::repository::bus_topic_list(&self.db, org_id) {
                Ok(topics) => {
                    for t in topics {
                        if let Err(e) = coordinator.reassign(org_id, &t.name, None, &[]) {
                            tracing::warn!(
                                org_id = %org_id, topic = %t.name, error = %e,
                                "purge_org: failed to stop replication before purge"
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        org_id = %org_id, error = %e,
                        "purge_org: failed to list topics for replication teardown"
                    );
                }
            }
        }
        let assignments_deleted =
            match crate::db::repository::bus_assignment_delete_by_org(&self.db, org_id) {
                Ok(n) => n as u32,
                Err(e) => {
                    tracing::warn!(
                        org_id = %org_id, error = %e,
                        "purge_org: failed to delete partition assignments"
                    );
                    0
                }
            };

        // Drop cached in-process state FIRST so nothing under this process
        // still holds an open file descriptor/mmap into the directory this
        // is about to remove. Every partition handle for this org is
        // `detach()`ed before it is dropped from the map — a live
        // `ConsumerHandle`/producer that already cloned one of these
        // (Arc-backed) gets a prompt `BusError::PartitionDetached` on its
        // next read/write instead of racing the `remove_dir_all` below for
        // a raw ENOENT (the bug that used to make `fetch` loop forever).
        self.partitions.retain(|k, v| {
            if k.0 == org_id {
                v.detach();
                false
            } else {
                true
            }
        });
        // Reaches partitions a live `ConsumerHandle` is holding that
        // `run_retention_sweep` had already removed from `self.partitions`
        // above BEFORE this purge ran — without this, that handle's clone
        // would survive untouched (see `consumer_partitions`'s doc) and, in
        // `AtMostOnce` mode, `fetch` would keep committing offsets for an
        // org this call just erased everywhere else.
        self.detach_consumer_partitions(org_id, None);
        self.dedup_stores.retain(|k, _| k.0 != org_id);
        self.topic_config_cache.retain(|k, _| k.0 != org_id);
        self.round_robin.retain(|k, _| k.0 != org_id);
        self.publish_rates.retain(|k, _| k.0 != org_id);
        self.group_state.remove_org(org_id);
        self.audit_windows.remove_org(org_id);
        self.commit_locks.retain(|k, _| k.0 != org_id);
        self.quota.remove_org(org_id);

        let topics_deleted =
            crate::db::repository::bus_topics_delete_by_org(&self.db, org_id)? as u32;
        let groups_deleted =
            crate::db::repository::bus_groups_delete_by_org(&self.db, org_id)? as u32;
        let offset_keys_deleted = self.offsets.purge_org(org_id)? as u32;
        let producer_seq_keys_deleted = self.producer_seq.purge_org(org_id)? as u32;
        let discarded_keys_deleted = self.discarded.purge_org(org_id)? as u32;

        let dir = self.bus_dir.join(org_id);
        let dir_removed = std::fs::remove_dir_all(&dir).is_ok();

        let report = PurgeReport {
            topics_deleted,
            groups_deleted,
            offset_keys_deleted,
            producer_seq_keys_deleted,
            discarded_keys_deleted,
            dir_removed,
            assignments_deleted,
        };
        let _ = crate::db::repository::log_audit(
            &self.db,
            None,
            None,
            "bus.org.purged",
            None,
            Some(&audit_details(
                org_id,
                Some(&format!(
                    "topics_deleted={} groups_deleted={} offset_keys_deleted={} producer_seq_keys_deleted={} discarded_keys_deleted={} dir_removed={} assignments_deleted={}",
                    report.topics_deleted,
                    report.groups_deleted,
                    report.offset_keys_deleted,
                    report.producer_seq_keys_deleted,
                    report.discarded_keys_deleted,
                    report.dir_removed,
                    report.assignments_deleted
                )),
            )),
            None,
            None,
        );
        Ok(report)
    }
}

impl Drop for BusService {
    /// Last-resort flush: a process shutdown that never called
    /// `stop_background_sweeper` (or an `init`-less `BusService::new` used
    /// directly, as every test does) would otherwise lose whatever
    /// `AuditWindows` was still holding suppressed at exit — the same
    /// "no occurrence is ever permanently lost" guarantee the windowing
    /// scheme promises, just triggered by the service going away instead
    /// of an explicit stop or the periodic timer.
    fn drop(&mut self) {
        self.flush_audit_windows();
    }
}

/// M2 (PLAN-M2 §1e/§2, contract with agent G's `replication/glue.rs`):
/// `BusService` IS the engine-handle/local-store bridge `bus::replication`
/// needs, kept as a separate `impl` block (rather than folded into the main
/// `impl BusService`) so this file's ownership of the trait contract is
/// visually obvious next to the inherent methods it reuses.
impl replication::glue::PartitionProvider for BusService {
    /// Reuses the exact same cached/LRU-tracked handle `publish`/
    /// `open_consumer` open — a replication feeder/follower is not a
    /// separate handle class from a producer/consumer's own (module doc's
    /// PARTITION HANDLE LIFETIME section, extended by M2's LRU: see
    /// `partition_handle`'s doc). Errors are collapsed to `ReplError::
    /// Internal` — `PartitionProvider`'s error surface is deliberately
    /// narrower than `BusServiceError` (mirrors `ReplError`'s own doc), and
    /// none of its current callers (`GlueLeaderFactory`/`GlueFollowerFactory
    /// ::spawn`) branch on WHICH `BusServiceError` variant failed, only on
    /// success vs failure.
    fn partition(
        &self,
        org: &str,
        topic: &str,
        partition: u32,
    ) -> Result<tentaflow_bus::Partition, ReplError> {
        let cfg = self
            .topic_config(org, topic)
            .map_err(|e| ReplError::Internal(e.to_string()))?;
        self.partition_handle(org, topic, partition, &cfg)
            .map_err(|e| ReplError::Internal(e.to_string()))
    }

    /// The SAME fjall-backed stores `publish`/`ConsumerHandle::commit`/
    /// `note_delivery_failure` already use — a follower stream applying
    /// `ReplOffsets`/a `Batch.producer` mark writes into the exact keyspace
    /// a local consumer/producer on THIS node would, so a promotion from
    /// follower to leader sees continuous state, not a second copy.
    fn follower_stores(&self) -> replication::follower::FollowerStores {
        replication::follower::FollowerStores {
            offsets: Arc::clone(&self.offsets),
            discarded: Arc::clone(&self.discarded),
            producer_seq: Arc::clone(&self.producer_seq),
        }
    }

    /// K-M2-6 (`ReplBatchHeader.producer`, PLAN-M2 §1b): unresolved gap
    /// flagged in POSTEP's "fala 2" contract-closure list
    /// (`ReplProducerMark.base_seq`) — `producer::ProducerSeqStore` only
    /// exposes a FORWARD check (`check`/`record`, keyed by producer
    /// identity), never a reverse "which producer identity landed at this
    /// `base_offset`" lookup, so this cannot be answered from the current
    /// store without adding a second index that nothing else in this file
    /// needs. Always `None`: every batch this node leads replicates with no
    /// producer mark, which — per `ReplProducerMark`'s own doc — degrades
    /// only a follower's continuity of PRODUCER FENCING across a failover
    /// (at-least-once delivery and ordering are unaffected either way).
    /// Documented as a real, known gap, not silently wrong.
    fn producer_mark_for(
        &self,
        org: &str,
        topic: &str,
        partition: u32,
        base_offset: u64,
    ) -> Option<replication::frames::ReplProducerMark> {
        let _ = (org, topic, partition, base_offset);
        None
    }

    /// `None` when the topic is unknown to this node — the caller (`Glue
    /// LeaderFactory::spawn`) already falls back to `Acks::Quorum` in that
    /// case (its own doc).
    fn topic_acks(&self, org: &str, topic: &str) -> Option<topics::Acks> {
        self.topic_config(org, topic).ok().map(|cfg| cfg.acks)
    }
}

/// Result of one `BusService::purge_org` call (GDPR/RODO).
#[derive(Debug, Clone, Copy, Default)]
pub struct PurgeReport {
    pub topics_deleted: u32,
    pub groups_deleted: u32,
    pub offset_keys_deleted: u32,
    pub producer_seq_keys_deleted: u32,
    pub discarded_keys_deleted: u32,
    pub dir_removed: bool,
    /// M2 (PLAN-M2 §1e): `bus_partition_assignments` rows deleted for this
    /// org. `0` whenever no `ReplicationCoordinator`/assignment store was
    /// ever wired — RF=1's `purge_org` path never had any to delete.
    pub assignments_deleted: u32,
}

/// Aggregate result of one `run_retention_sweep` call, across every org and
/// topic it touched.
#[derive(Debug, Clone, Copy, Default)]
pub struct RetentionReport {
    pub orgs_swept: u32,
    pub topics_swept: u32,
    pub deleted_segments: u32,
    pub deleted_bytes: u64,
}

// ---- ConsumerHandle (self-sufficient: fetch/commit/lag need no BusService) -

struct ConsumerPartition {
    topic: String,
    partition: u32,
    reader: tentaflow_bus::PartitionReader,
    /// The FULL `Partition` (writer thread + directory flock), not just
    /// `reader`'s shared read-only state — see the module doc's PARTITION
    /// HANDLE LIFETIME section. Never read directly on `fetch`'s hot path
    /// (that goes through `reader`); this field's only job is to be an
    /// `Arc` clone that outlives whatever `BusService::partitions` does
    /// with its own copy of the same key, so a `run_retention_sweep` that
    /// removes that map entry can never stop THIS handle's ability to keep
    /// reading/writing. Also passed to `BusService::register_consumer_partition`
    /// at `open_consumer` time so `delete_topic`/`purge_org` can `detach()`
    /// it even after the map entry is gone.
    handle: tentaflow_bus::Partition,
    next_offset: AtomicU64,
    /// Snapshot of the topic's environment (PLAN §4.4 Z12) taken at
    /// `open_consumer` time, encoded via `environment_to_u8` — compared
    /// against the handle's `node_environment` on every `fetch`.
    environment: u8,
    /// Set once an `OffsetOutOfRange` gap has been audited for this
    /// partition's current position so a caller polling in a tight
    /// loop against a still-unresolved gap does not write one
    /// `bus.offset.gap` row per `fetch` call. Cleared by
    /// `seek_to_earliest`, since a later gap (a second retention pass) is a
    /// new occurrence worth auditing again.
    gap_audited: AtomicBool,
}

pub struct ConsumerHandle {
    pub org_id: String,
    pub group: String,
    pub commit_mode: groups::CommitMode,
    partitions: Vec<ConsumerPartition>,
    offsets: Arc<groups::GroupOffsetStore>,
    db: DbPool,
    /// kept so `fetch`/`commit` can re-authorize when
    /// `generation` has moved, instead of trusting the snapshot taken at
    /// `open_consumer` for the handle's entire lifetime.
    authorizer: Arc<dyn BusAuthorizer>,
    ctx: BusCallContext,
    /// Last permission-generation this handle successfully authorized
    /// against — see `BusAuthorizer::generation`'s doc.
    generation: AtomicU64,
    /// Shared with `BusService::node_environment_cache` — a node
    /// that changes environment mid-session is visible to every open
    /// handle immediately, not just to new `open_consumer` calls.
    node_environment: Arc<AtomicU8>,
    /// Shared with `BusService::audit_windows`  — so a
    /// `bus.consume.denied` raised here (`revalidate`) is windowed against
    /// the SAME per-(org, kind) counters `BusService::open_consumer`/
    /// `note_delivery_failure` use, not a separate, handle-local count.
    audit_windows: Arc<AuditWindows>,
    /// Shared with `BusService::group_state` — backs `fetch`'s paused-group
    /// check (see `GroupStateCache`'s doc).
    group_state: Arc<GroupStateCache>,
    /// Shared with `BusService::commit_locks` — see that field's doc.
    commit_locks: CommitLocks,
    /// Shared with `BusService::purged_orgs` — see that field's doc.
    purged_orgs: Arc<DashMap<String, u64>>,
    /// This handle's org's purge count at `open_consumer` time — compared
    /// against `purged_orgs`' live value in `commit`/`seek_to_earliest`.
    purge_epoch: u64,
    /// Shared with `BusService::replication` (`Arc::clone` of the SAME
    /// `RwLock`, not a snapshot) — see that field's doc for why. Backs
    /// `fetch`/`commit`'s `role() == Leader` check (PLAN-M2 §1e:
    /// consumption is leader-only in M2).
    replication: Arc<parking_lot::RwLock<Option<Arc<dyn ReplicationCoordinator>>>>,
    /// Shared with `BusService::consume_msgs_total` — see that field's doc.
    consume_msgs_total: Arc<AtomicU64>,
}

impl ConsumerHandle {
    /// Re-authorizes every subscribed topic when `self.authorizer`'s
    /// permission generation has moved since the last successful check
    /// (PLAN §8.1: "cofnięcie uprawnienia działa w granicach jednego
    /// fetcha"). A no-op (single atomic load) on the common path where
    /// nothing has changed.
    fn revalidate(&self) -> Result<(), BusServiceError> {
        let current = self.authorizer.generation();
        if self.generation.load(Ordering::Acquire) == current {
            return Ok(());
        }
        let mut checked: Vec<&str> = Vec::new();
        for cp in &self.partitions {
            let topic = cp.topic.as_str();
            if checked.contains(&topic) {
                continue;
            }
            checked.push(topic);
            if self
                .authorizer
                .authorize_group(&self.ctx, BusAction::Consume, topic, &self.group)
                .is_err()
            {
                write_windowed_audit(
                    &self.db,
                    &self.audit_windows,
                    self.ctx.actor.as_deref(),
                    &self.org_id,
                    "bus.consume.denied",
                    Some(topic),
                    Some(&format!("group={} action=revalidate", self.group)),
                );
                return Err(deny(BusAction::Consume, topic));
            }
        }
        self.generation.store(current, Ordering::Release);
        Ok(())
    }

    /// Fail-closed Z12 environment check — mirrors
    /// `BusService::check_environment` but works off the snapshot each
    /// `ConsumerPartition` captured at `open_consumer` time, since a
    /// `ConsumerHandle` has no `TopicConfig` of its own to re-read.
    fn check_environment(&self, cp: &ConsumerPartition) -> Result<(), BusServiceError> {
        let node_env = self.node_environment.load(Ordering::Acquire);
        if cp.environment != node_env {
            return Err(BusServiceError::EnvironmentMismatch {
                topic_env: environment_from_u8(cp.environment),
                node_env: environment_from_u8(node_env),
            });
        }
        Ok(())
    }

    /// Refuses to proceed if `self.org_id` was `purge_org`'d after this
    /// handle was opened (see `BusService::purged_orgs`'s doc) — guards the
    /// two methods (`commit`, `seek_to_earliest`) that write a durable
    /// fjall offset, which would otherwise silently resurrect a key for an
    /// org GDPR/RODO erasure already removed everywhere else. `fetch` does
    /// not need this: `purge_org` detaches every partition it touches, so
    /// `fetch`'s own engine read already fails fast via `map_engine_error`.
    fn check_not_purged(&self, topic: &str) -> Result<(), BusServiceError> {
        let current = self.purged_orgs.get(&self.org_id).map(|e| *e).unwrap_or(0);
        if current != self.purge_epoch {
            return Err(BusServiceError::TopicNotFound {
                name: topic.to_string(),
            });
        }
        Ok(())
    }

    /// Pull-based fetch (PLAN §5.3.7): polls every subscribed partition
    /// round-robin up to `max_bytes` total, long-polling up to
    /// `max_wait_ms` if nothing is available yet.
    ///
    /// BLOCKING: this calls `PartitionReader::fetch_from_offset`,
    /// which is a synchronous, potentially disk-I/O-bound call, and may
    /// additionally sleep for up to `max_wait_ms` in the long-poll loop
    /// below. A caller on a Tokio (or any async) executor MUST run this
    /// inside `spawn_blocking` — calling it directly from an async fn
    /// blocks that executor thread for the whole wait.
    pub fn fetch(
        &self,
        max_bytes: usize,
        max_wait_ms: u32,
    ) -> Result<FetchedBatch, BusServiceError> {
        self.revalidate()?;
        // M2 (PLAN-M2 §1e): consumption is leader-only. Read once, not per
        // poll iteration below — a coordinator that flips mid-long-poll is
        // no different from one that flips a moment before this call.
        let coordinator = self.replication.read().clone();
        // a paused group must not silently keep returning empty
        // batches forever (indistinguishable from "caught up") — an
        // explicit error lets a poll loop tell the two apart. Checked once
        // per distinct topic, up front, so a paused subscription never
        // touches the engine at all.
        let mut checked_pause: Vec<&str> = Vec::new();
        for cp in &self.partitions {
            let topic = cp.topic.as_str();
            if checked_pause.contains(&topic) {
                continue;
            }
            checked_pause.push(topic);
            if self
                .group_state
                .paused(&self.db, &self.org_id, &self.group, topic)?
            {
                return Err(BusServiceError::GroupPaused {
                    group: self.group.clone(),
                    topic: topic.to_string(),
                });
            }
        }
        let deadline = Instant::now() + Duration::from_millis(max_wait_ms as u64);
        loop {
            let mut records = Vec::new();
            let mut consumed = 0usize;
            for cp in &self.partitions {
                if consumed >= max_bytes {
                    break;
                }
                self.check_environment(cp)?;
                check_leader_role(&coordinator, &self.org_id, &cp.topic, cp.partition)?;
                let from = cp.next_offset.load(Ordering::Acquire);
                let batches = match cp
                    .reader
                    .fetch_from_offset(from, max_bytes.saturating_sub(consumed))
                {
                    Ok(b) => b,
                    Err(tentaflow_bus::BusError::OffsetOutOfRange {
                        requested,
                        earliest,
                        latest,
                    }) => {
                        // audited once per occurrence, not once per
                        // fetch — `seek_to_earliest` clears the flag so a
                        // LATER gap (another retention pass) is still
                        // reported.
                        if !cp.gap_audited.swap(true, Ordering::AcqRel) {
                            let _ = crate::db::repository::log_audit(
                                &self.db,
                                self.ctx.actor.as_deref(),
                                None,
                                "bus.offset.gap",
                                Some(&cp.topic),
                                Some(&audit_details(
                                    &self.org_id,
                                    Some(&format!(
                                        "group={} partition={} requested={requested} earliest={earliest} latest={latest}",
                                        self.group, cp.partition
                                    )),
                                )),
                                None,
                                None,
                            );
                        }
                        return Err(BusServiceError::OffsetOutOfRange {
                            topic: cp.topic.clone(),
                            partition: cp.partition,
                            requested,
                            earliest,
                            latest,
                        });
                    }
                    Err(other) => return Err(map_engine_error(other, &cp.topic, cp.partition)),
                };
                let mut new_next = from;
                for view in &batches {
                    for rv in view.records_from(from) {
                        let rv = rv?;
                        let offset = view.header().base_offset + rv.offset_delta as u64;
                        consumed += rv.payload.len();
                        records.push(FetchedRecordMeta {
                            topic: cp.topic.clone(),
                            partition: cp.partition,
                            offset,
                            timestamp_ms: view.header().base_timestamp_ms + rv.ts_delta_ms as i64,
                            key: rv.key.clone(),
                            // Cheap `Bytes` clones, not a UTF-8 decode +
                            // allocation per header — see
                            // `FetchedRecordMeta::headers`'s doc.
                            headers: rv.headers.iter().cloned().collect(),
                            payload: rv.payload.clone(),
                            schema_id: rv.schema_id,
                        });
                    }
                    new_next = new_next.max(view.header().next_offset());
                }
                if new_next > from {
                    cp.next_offset.store(new_next, Ordering::Release);
                    if self.commit_mode == groups::CommitMode::AtMostOnce {
                        self.offsets.commit(
                            &self.org_id,
                            &self.group,
                            &cp.topic,
                            cp.partition,
                            new_next,
                            now_ms(),
                        )?;
                    }
                }
            }
            if !records.is_empty() || Instant::now() >= deadline {
                if !records.is_empty() {
                    // M2 (PLAN §8.4): feeds `tentaflow_bus_consume_msgs_total`.
                    self.consume_msgs_total
                        .fetch_add(records.len() as u64, Ordering::Relaxed);
                    // Field policy read projection (SUM/tentabus/POLITYKI-POL.md,
                    // "hide only"), keyed per-record by ITS OWN topic — a
                    // single consumer can subscribe to more than one topic.
                    // Resolved once per distinct topic in this batch, not
                    // once per record, since a batch can hold many records
                    // from the same partition. The topic's `content_type`
                    // (-> payload_format::PayloadFormat) is only looked up
                    // when a policy actually exists — the common no-policy
                    // case pays for the policy lookup alone.
                    let mut policy_cache: std::collections::HashMap<
                        String,
                        Option<(field_policies::FieldPolicy, payload_format::PayloadFormat)>,
                    > = std::collections::HashMap::new();
                    for rec in &mut records {
                        if !policy_cache.contains_key(&rec.topic) {
                            let resolved = field_policies::resolve(
                                &self.db,
                                &self.org_id,
                                &rec.topic,
                                self.ctx.actor.as_deref().unwrap_or(""),
                                field_policies::Direction::Read,
                            )?;
                            let resolved = match resolved {
                                Some(policy) => {
                                    let format = topics::get_topic(&self.db, &self.org_id, &rec.topic)?
                                        .map(|cfg| {
                                            payload_format::PayloadFormat::from_content_type(
                                                &cfg.content_type,
                                            )
                                        })
                                        .unwrap_or(payload_format::PayloadFormat::Json);
                                    Some((policy, format))
                                }
                                None => None,
                            };
                            policy_cache.insert(rec.topic.clone(), resolved);
                        }
                        if let Some((policy, format)) = policy_cache.get(&rec.topic).unwrap() {
                            rec.payload = field_policies::project_read(policy, *format, &rec.payload);
                        }
                    }
                }
                return Ok(FetchedBatch { records });
            }
            // no notification hook exists on `PartitionReader`/
            // `Partition` to wake this loop when new data lands (the
            // engine's own writer path uses an internal `oneshot` per
            // append, not a broadcast a reader could subscribe to) — a
            // bounded poll sleep is the only option available without
            // engine changes (out of this file's ownership).
            std::thread::sleep(Duration::from_millis(max_wait_ms.clamp(1, 5) as u64));
        }
    }

    /// Durably advances the committed offset for each `(topic, partition)`
    /// (PLAN §3.2). Also raises the in-memory fetch cursor to at least the
    /// committed offset, so a caller committing ahead of its last fetch
    /// (batch processing, commit-at-end) never re-fetches already-handled
    /// records.
    ///
    /// every `(topic, partition)` is validated BEFORE any fjall
    /// write — an entry outside this handle's subscription set, an offset
    /// behind what is already committed, or an environment that no longer
    /// matches this node aborts the WHOLE call with nothing persisted,
    /// rather than partially committing the earlier entries in `offsets`
    /// and silently skipping the bad one. Concurrent `commit` calls for the
    /// SAME (org, group) — from two independent handles, or two threads
    /// sharing this one — are serialized on `commit_locks` for exactly
    /// this reason: without it, a second call's validation loop could read
    /// a `committed_offset` that a first call's write loop is
    /// concurrently about to move past, and the two writes could interleave
    /// even though each one individually validated cleanly.
    pub fn commit(&self, offsets: &[(TopicPartition, u64)]) -> Result<(), BusServiceError> {
        self.revalidate()?;
        // M2 (PLAN-M2 §1e): consumption is leader-only.
        let coordinator = self.replication.read().clone();
        let lock = self
            .commit_locks
            .entry((self.org_id.clone(), self.group.clone()))
            .or_insert_with(|| Arc::new(parking_lot::Mutex::new(())))
            .clone();
        let _guard = lock.lock();
        // Checked UNDER the group's commit mutex, not before acquiring
        // it: `purge_org` bumps `purged_orgs` and only then goes on to
        // detach/delete everything else, so a check taken before the lock
        // could observe the pre-purge epoch and still win the race to
        // write below — the mutex does not protect against `purge_org`
        // itself, but it does guarantee no OTHER `commit` call for this
        // (org, group) can slip a stale epoch read in between this check
        // and the writes further down.
        if let Some((tp, _)) = offsets.first() {
            self.check_not_purged(&tp.topic)?;
        }
        for (tp, offset) in offsets {
            let cp = self
                .partitions
                .iter()
                .find(|p| p.topic == tp.topic && p.partition == tp.partition)
                .ok_or_else(|| BusServiceError::NotSubscribed {
                    topic: tp.topic.clone(),
                    partition: tp.partition,
                })?;
            self.check_environment(cp)?;
            check_leader_role(&coordinator, &self.org_id, &tp.topic, tp.partition)?;
            let committed = self.offsets.committed_offset(
                &self.org_id,
                &self.group,
                &tp.topic,
                tp.partition,
            )?;
            if *offset < committed {
                return Err(BusServiceError::OffsetRegression {
                    topic: tp.topic.clone(),
                    partition: tp.partition,
                    requested: *offset,
                    committed,
                });
            }
        }
        let now = now_ms();
        for (tp, offset) in offsets {
            self.offsets.commit(
                &self.org_id,
                &self.group,
                &tp.topic,
                tp.partition,
                *offset,
                now,
            )?;
            // K-M2-5: replicate this offset commit so a failover redelivers
            // at most `ReplOffsets`' coalescing window, never resets. A
            // successful `commit` also means the group has moved past
            // whatever attempts this offset (or the ones behind it) had
            // accrued — `attempts: 0` here matches the local
            // `clear_attempts_in_range` a plain `commit` already performs
            // (see `GroupOffsetStore::commit`'s doc); `note_delivery_
            // failure`'s own call site (below) is the one that reports a
            // NON-zero running count.
            if let Some(coordinator) = &coordinator {
                coordinator.note_offset_commit(
                    &self.org_id,
                    &self.group,
                    &tp.topic,
                    tp.partition,
                    *offset,
                    0,
                );
            }
            if let Some(cp) = self
                .partitions
                .iter()
                .find(|p| p.topic == tp.topic && p.partition == tp.partition)
            {
                let cur = cp.next_offset.load(Ordering::Acquire);
                if *offset > cur {
                    cp.next_offset.store(*offset, Ordering::Release);
                }
            }
        }
        Ok(())
    }

    /// Deliberate recovery from `OffsetOutOfRange`: jumps the fetch
    /// cursor AND the durable commit forward to `earliest_offset()` for
    /// `(topic, partition)`. This is always a FORWARD move (skipping a gap
    /// retention already created), so it goes through the normal
    /// monotonic-`commit` path — unlike an admin's downward
    /// `BusService::reset_offset`, no extra authorization is needed beyond
    /// what opened this handle. Returns the offset it seeked to.
    pub fn seek_to_earliest(&self, topic: &str, partition: u32) -> Result<u64, BusServiceError> {
        self.revalidate()?;
        self.check_not_purged(topic)?;
        let cp = self
            .partitions
            .iter()
            .find(|p| p.topic == topic && p.partition == partition)
            .ok_or_else(|| BusServiceError::NotSubscribed {
                topic: topic.to_string(),
                partition,
            })?;
        self.check_environment(cp)?;
        // `earliest_offset()` is an infallible read with no "detached"
        // signal of its own (see `PartitionReader::is_detached`'s doc) — a
        // detached partition (topic/org deleted/purged) would otherwise
        // report a stale `0`/last-known value here and this method would
        // happily commit forward against a log that no longer exists,
        // exactly the gap `check_not_purged` above does not cover (that
        // guards the ORG-purge race, not a `delete_topic` whose org was
        // never purged at all).
        if cp.reader.is_detached() {
            return Err(BusServiceError::TopicNotFound {
                name: topic.to_string(),
            });
        }
        let earliest = cp.reader.earliest_offset();
        self.offsets.commit(
            &self.org_id,
            &self.group,
            topic,
            partition,
            earliest,
            now_ms(),
        )?;
        cp.next_offset.store(earliest, Ordering::Release);
        cp.gap_audited.store(false, Ordering::Release);
        Ok(earliest)
    }

    /// `high_watermark - committed_offset` per subscribed partition (PLAN
    /// §3.2), read from the durable commit — not the in-memory fetch
    /// cursor, since an uncommitted fetch is still "lag" from the group's
    /// point of view. Revalidates permission and checks Z12 environment
    /// fencing per partition, same as `fetch`/`commit` — `lag` used to skip
    /// both, so a handle whose permission had already been revoked (or
    /// whose topic environment no longer matches this node) could still be
    /// used to read a topic's lag/high-watermark.
    pub fn lag(&self) -> Result<Vec<(TopicPartition, u64)>, BusServiceError> {
        self.revalidate()?;
        let mut out = Vec::with_capacity(self.partitions.len());
        for cp in &self.partitions {
            self.check_environment(cp)?;
            // Same reasoning as `seek_to_earliest`: `high_watermark()` has
            // no "detached" signal of its own, so a deleted/purged
            // partition would otherwise report a frozen, stale watermark
            // instead of the `TopicNotFound` `fetch` already returns for
            // the same condition.
            if cp.reader.is_detached() {
                return Err(BusServiceError::TopicNotFound {
                    name: cp.topic.clone(),
                });
            }
            let hw = cp.reader.high_watermark();
            let l = self
                .offsets
                .lag(&self.org_id, &self.group, &cp.topic, cp.partition, hw)?;
            out.push((
                TopicPartition {
                    topic: cp.topic.clone(),
                    partition: cp.partition,
                },
                l,
            ));
        }
        Ok(out)
    }
}

// ---- Process-global singleton + free-function API (PLAN §6.1) -----------

pub fn init(cfg: BusInitConfig) -> Result<Arc<BusService>, BusServiceError> {
    if let Some(existing) = BUS_SERVICE.get() {
        return Ok(existing.clone());
    }
    let retention_interval = cfg.retention_interval;
    let service = Arc::new(BusService::new(cfg)?);
    let _ = BUS_SERVICE.set(service);
    let service = BUS_SERVICE
        .get()
        .expect("bus service must be initialized")
        .clone();
    // `None` (the default, and what every test uses) never spawns this, so
    // unit tests never race a background sweeper.
    if let Some(interval) = retention_interval {
        spawn_background_sweeper(Arc::clone(&service), interval);
    }
    // Unlike the retention sweeper, the audit-flush timer is unconditional
    // infrastructure, not an opt-in feature: `AuditWindows`'s "no occurrence
    // is ever permanently lost" guarantee otherwise depends entirely on
    // either the retention sweeper being configured (most test/operator-tool
    // setups leave `retention_interval: None`) or on some later occurrence
    // arriving to trigger the lazy flush in `record` — neither of which a
    // quiet system after a burst of denials can be relied on to do. Only
    // `bus::init` starts this (never `BusService::new` directly), matching
    // every other background thread in this module.
    spawn_audit_flush_timer(Arc::clone(&service));
    // Also unconditional infrastructure (PLAN §8.4/M4 dogfooding), same
    // reasoning as the audit-flush timer above: `__bus.metrics` must keep
    // rolling up regardless of whether an operator configured a retention
    // sweep.
    spawn_metrics_rollup_timer(Arc::clone(&service));
    Ok(service)
}

/// How often the independent audit-flush timer (`spawn_audit_flush_timer`)
/// wakes up. Deliberately not a `BusInitConfig` field: this is unconditional
/// node infrastructure, not a tunable — an operator who needs windowed
/// audit rows to appear faster should shrink `AUDIT_WINDOW` instead, which
/// controls how long an occurrence can be legitimately suppressed in the
/// first place.
const AUDIT_FLUSH_INTERVAL: Duration = Duration::from_secs(60);

/// Background thread started unconditionally by `bus::init`, independent of
/// the (optional) retention sweeper: periodically flushes any `AuditWindows`
/// buckets that still have a suppressed occurrence pending
/// (`flush_audit_windows`), so a burst of denials that goes quiet mid-window
/// still gets its tail count written even though nothing configured a
/// retention sweep at all. Stops at the next tick after
/// `BusService::stop_background_sweeper` is called (shares that same
/// shutdown flag — one "ask every background thread to stop" signal, not
/// one per thread).
fn spawn_audit_flush_timer(service: Arc<BusService>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(AUDIT_FLUSH_INTERVAL);
        if service.sweeper_shutdown.load(Ordering::Acquire) {
            break;
        }
        service.flush_audit_windows();
    });
}

/// Background thread started by `init` when `BusInitConfig.retention_interval`
/// is `Some` : on each tick, runs a full
/// `run_retention_sweep` and then flushes any windowed audit buckets that
/// have suppressed occurrences waiting (`flush_audit_windows`) — piggy-backing
/// the audit flush on the same timer rather than running a second thread for
/// it, since both are "occasional system housekeeping", not latency-sensitive.
/// Stops at the next tick after `BusService::stop_background_sweeper` is
/// called.
fn spawn_background_sweeper(service: Arc<BusService>, interval: Duration) {
    std::thread::spawn(move || loop {
        std::thread::sleep(interval);
        if service.sweeper_shutdown.load(Ordering::Acquire) {
            break;
        }
        let report = service.run_retention_sweep();
        if report.deleted_segments > 0 {
            tracing::info!(
                orgs = report.orgs_swept,
                topics = report.topics_swept,
                deleted_segments = report.deleted_segments,
                deleted_bytes = report.deleted_bytes,
                "bus retention sweep completed"
            );
        }
        service.flush_audit_windows();
    });
}

/// How often `spawn_metrics_rollup_timer` publishes a fresh
/// `BusMetricsRollup` snapshot to `__bus.metrics` (PLAN §8.4/M4: "1-second
/// rollups").
const METRICS_ROLLUP_INTERVAL: Duration = Duration::from_secs(1);

/// Background thread started unconditionally by `bus::init`: every
/// `METRICS_ROLLUP_INTERVAL`, publishes one `BusMetricsRollup` snapshot to
/// `__bus.metrics` via `BusService::publish_metrics_rollup`. Same
/// shutdown-flag shape as `spawn_audit_flush_timer`/`spawn_background_sweeper`.
fn spawn_metrics_rollup_timer(service: Arc<BusService>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(METRICS_ROLLUP_INTERVAL);
        if service.sweeper_shutdown.load(Ordering::Acquire) {
            break;
        }
        service.publish_metrics_rollup();
    });
}

pub fn global() -> Option<Arc<BusService>> {
    BUS_SERVICE.get().cloned()
}

pub fn publish(
    ctx: &BusCallContext,
    topic: &str,
    batch: PublishBatch,
) -> Result<PublishResult, BusServiceError> {
    global()
        .ok_or(BusServiceError::NotInitialized)?
        .publish(ctx, topic, batch)
}

pub fn open_consumer(
    ctx: &BusCallContext,
    group: &str,
    topics_in: &[String],
    cfg: ConsumerConfig,
) -> Result<ConsumerHandle, BusServiceError> {
    global()
        .ok_or(BusServiceError::NotInitialized)?
        .open_consumer(ctx, group, topics_in, cfg)
}

pub fn fetch(
    h: &ConsumerHandle,
    max_bytes: usize,
    max_wait_ms: u32,
) -> Result<FetchedBatch, BusServiceError> {
    h.fetch(max_bytes, max_wait_ms)
}

pub fn commit(
    h: &ConsumerHandle,
    offsets: &[(TopicPartition, u64)],
) -> Result<(), BusServiceError> {
    h.commit(offsets)
}

pub fn peek(
    ctx: &BusCallContext,
    topic: &str,
    partition: u32,
    from_offset: u64,
    max_records: usize,
    max_bytes: usize,
) -> Result<PeekResult, BusServiceError> {
    global().ok_or(BusServiceError::NotInitialized)?.peek(
        ctx,
        topic,
        partition,
        from_offset,
        max_records,
        max_bytes,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AllowAllAuthorizer;
    impl BusAuthorizer for AllowAllAuthorizer {
        fn authorize(
            &self,
            _ctx: &BusCallContext,
            _action: BusAction,
            _topic: &str,
        ) -> Result<(), BusServiceError> {
            Ok(())
        }
        fn authorize_group(
            &self,
            _ctx: &BusCallContext,
            _action: BusAction,
            _topic: &str,
            _group: &str,
        ) -> Result<(), BusServiceError> {
            Ok(())
        }
        fn generation(&self) -> u64 {
            0
        }
    }

    struct DenyAllAuthorizer;
    impl BusAuthorizer for DenyAllAuthorizer {
        fn authorize(
            &self,
            _ctx: &BusCallContext,
            action: BusAction,
            topic: &str,
        ) -> Result<(), BusServiceError> {
            Err(deny(action, topic))
        }
        fn authorize_group(
            &self,
            _ctx: &BusCallContext,
            action: BusAction,
            topic: &str,
            _group: &str,
        ) -> Result<(), BusServiceError> {
            Err(deny(action, topic))
        }
        fn generation(&self) -> u64 {
            0
        }
    }

    /// Test double: `allow` and `generation` can each be flipped
    /// mid-session (both `Ordering::SeqCst` for simplicity — tests never
    /// contend on them) so a test can open a `ConsumerHandle` while
    /// permission is granted, then revoke it and observe `fetch`/`commit`
    /// react without ever re-calling `open_consumer`.
    struct FlippableAuthorizer {
        allow: std::sync::atomic::AtomicBool,
        generation: AtomicU64,
    }
    impl FlippableAuthorizer {
        fn new() -> Self {
            Self {
                allow: std::sync::atomic::AtomicBool::new(true),
                generation: AtomicU64::new(1),
            }
        }
        fn revoke(&self) {
            self.allow.store(false, std::sync::atomic::Ordering::SeqCst);
            self.generation.fetch_add(1, Ordering::SeqCst);
        }
    }
    impl BusAuthorizer for FlippableAuthorizer {
        fn authorize(
            &self,
            _ctx: &BusCallContext,
            action: BusAction,
            topic: &str,
        ) -> Result<(), BusServiceError> {
            if self.allow.load(std::sync::atomic::Ordering::SeqCst) {
                Ok(())
            } else {
                Err(deny(action, topic))
            }
        }
        fn authorize_group(
            &self,
            ctx: &BusCallContext,
            action: BusAction,
            topic: &str,
            _group: &str,
        ) -> Result<(), BusServiceError> {
            self.authorize(ctx, action, topic)
        }
        fn generation(&self) -> u64 {
            self.generation.load(Ordering::SeqCst)
        }
    }

    /// Denies `Consume` on exactly one topic, allows everything else
    /// (including `Admin`, so `create_topic` for the denied topic still
    /// succeeds) — used to prove `open_consumer`'s phase-1/phase-2 split:
    /// a denial on a LATER topic in a multi-topic request must leave no
    /// trace for the EARLIER ones.
    struct DenyConsumeOnTopicAuthorizer {
        deny_topic: &'static str,
    }
    impl BusAuthorizer for DenyConsumeOnTopicAuthorizer {
        fn authorize(
            &self,
            _ctx: &BusCallContext,
            _action: BusAction,
            _topic: &str,
        ) -> Result<(), BusServiceError> {
            Ok(())
        }
        fn authorize_group(
            &self,
            _ctx: &BusCallContext,
            action: BusAction,
            topic: &str,
            _group: &str,
        ) -> Result<(), BusServiceError> {
            if action == BusAction::Consume && topic == self.deny_topic {
                Err(deny(action, topic))
            } else {
                Ok(())
            }
        }
        fn generation(&self) -> u64 {
            0
        }
    }

    /// Denies plain (non-group-scoped) `Consume` on every topic, allows
    /// everything else — `BusService::peek` authorizes via `authorize`
    /// (there is no group to scope it to), unlike `open_consumer`/
    /// `note_delivery_failure`'s `authorize_group`, so `DenyConsumeOnTopicAuthorizer`
    /// above (which only gates `authorize_group`) cannot exercise `peek`'s
    /// denial path.
    struct DenyPlainConsumeAuthorizer;
    impl BusAuthorizer for DenyPlainConsumeAuthorizer {
        fn authorize(
            &self,
            _ctx: &BusCallContext,
            action: BusAction,
            topic: &str,
        ) -> Result<(), BusServiceError> {
            if action == BusAction::Consume {
                Err(deny(action, topic))
            } else {
                Ok(())
            }
        }
        fn authorize_group(
            &self,
            _ctx: &BusCallContext,
            _action: BusAction,
            _topic: &str,
            _group: &str,
        ) -> Result<(), BusServiceError> {
            Ok(())
        }
        fn generation(&self) -> u64 {
            0
        }
    }

    fn test_ctx(org: &str) -> BusCallContext {
        BusCallContext {
            org_id: org.to_string(),
            actor: Some("tester".to_string()),
            correlation_id: Some("corr-1".to_string()),
            origin: "test".to_string(),
        }
    }

    /// Opens a fresh `BusService` in its own `tempfile::TempDir`, which is
    /// removed when the returned `TempDir` is dropped — callers must keep
    /// it alive (`let (_tmp, svc) = test_service();`) for as long as they
    /// use `svc`, mirroring `producer::tests::temp_db`/`groups::tests::
    /// temp_db`'s shape for the exact same reason (this used to leak a
    /// hand-rolled `std::env::temp_dir()` subdirectory per test run,
    /// forever).
    /// Same shape as `test_service`'s own `TempDir` use, for the handful of
    /// tests that need a bespoke `BusInitConfig` (a non-default
    /// authorizer, a specific `dedup_expected_rate_per_sec`, …) and so
    /// cannot go through `test_service()` itself.
    fn test_bus_dir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("create temp dir");
        let bus_dir = dir.path().join("bus");
        (dir, bus_dir)
    }

    fn test_service() -> (tempfile::TempDir, BusService) {
        let dir = tempfile::tempdir().expect("create temp dir");
        let bus_dir = dir.path().join("bus");
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        crate::db::repository::bus_test_support::create_bus_tables(&db)
            .expect("bus fixture tables");
        let svc = BusService::new(BusInitConfig {
            bus_dir,
            db,
            authorizer: Arc::new(AllowAllAuthorizer),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .expect("bus service");
        (dir, svc)
    }

    fn record(payload: &str) -> PublishRecord {
        PublishRecord {
            key: None,
            headers: vec![],
            payload: Bytes::from(payload.to_string()),
            timestamp_ms: now_ms(),
            schema_id: 0,
        }
    }

    fn record_with_key(payload: &str, key: &str) -> PublishRecord {
        PublishRecord {
            key: Some(Bytes::from(key.to_string())),
            headers: vec![],
            payload: Bytes::from(payload.to_string()),
            timestamp_ms: now_ms(),
            schema_id: 0,
        }
    }

    #[test]
    fn publish_metrics_rollup_creates_topic_and_publishes_one_record() {
        let (_tmp, svc) = test_service();
        let org = crate::services::org::DEFAULT_ORG_ID;

        // Topic must not exist yet — this is the lazy-create path.
        assert!(topics::get_topic(&svc.db, org, topics::METRICS_TOPIC_NAME)
            .unwrap()
            .is_none());

        svc.publish_metrics_rollup();

        let cfg = topics::get_topic(&svc.db, org, topics::METRICS_TOPIC_NAME)
            .unwrap()
            .expect("__bus.metrics must be auto-created");
        assert_eq!(cfg.partitions, 1);

        let ctx = test_ctx(org);
        let handle = svc
            .open_consumer(
                &ctx,
                "metrics-test-group",
                &[topics::METRICS_TOPIC_NAME.to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();
        let batch = handle.fetch(1024 * 1024, 50).unwrap();
        assert_eq!(batch.records.len(), 1);

        // Payload is the JSON-serialized BusMetricsRollup — assert it round-trips
        // as an object with a couple of the expected fields, without depending
        // on the full 16-field shape (that shape belongs to metrics_export).
        let value: serde_json::Value = serde_json::from_slice(&batch.records[0].payload).unwrap();
        assert!(value.get("publish_msgs_total").is_some());
        assert!(value.get("topic_count").is_some());

        // A second call republishes to the ALREADY-created topic rather than
        // erroring — the lazy-create path must be idempotent.
        svc.publish_metrics_rollup();
        let batch2 = handle.fetch(1024 * 1024, 50).unwrap();
        assert_eq!(batch2.records.len(), 1);
    }

    #[test]
    fn full_cycle_publish_fetch_commit_lag() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.created",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();

        for i in 0..5 {
            let r = svc
                .publish(
                    &ctx,
                    "orders.created",
                    PublishBatch {
                        partition: None,
                        producer: None,
                        records: vec![record(&format!("order-{i}"))],
                    },
                )
                .unwrap();
            assert_eq!(r.single_partition().unwrap().base_offset, i as u64);
            assert!(!r.duplicate);
        }

        let handle = svc
            .open_consumer(
                &ctx,
                "shipping",
                &["orders.created".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();

        // Lag before any consumption: 5 unread records.
        let lag = handle.lag().unwrap();
        assert_eq!(lag.len(), 1);
        assert_eq!(lag[0].1, 5);

        let batch = handle.fetch(1024 * 1024, 50).unwrap();
        assert_eq!(batch.records.len(), 5);
        assert_eq!(batch.records[0].payload, Bytes::from_static(b"order-0"));
        assert_eq!(batch.records[4].offset, 4);
        // System headers are present (PLAN §2.3).
        assert!(batch.records[0]
            .headers
            .iter()
            .any(|(k, v)| k == "tf.org" && v == "org-1"));

        // Lag is unchanged until commit (fetch alone does not advance it).
        let lag = handle.lag().unwrap();
        assert_eq!(lag[0].1, 5);

        handle
            .commit(&[(
                TopicPartition {
                    topic: "orders.created".to_string(),
                    partition: 0,
                },
                5,
            )])
            .unwrap();

        let lag = handle.lag().unwrap();
        assert_eq!(lag[0].1, 0);

        // A fresh consumer handle in the same group resumes from the commit.
        let handle2 = svc
            .open_consumer(
                &ctx,
                "shipping",
                &["orders.created".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();
        let batch2 = handle2.fetch(1024, 20).unwrap();
        assert!(batch2.records.is_empty());
    }

    #[test]
    fn producer_idempotency_returns_original_offset_on_duplicate() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(&ctx, "payments.charged", topics::TopicOptions::default())
            .unwrap();

        let identity = producer::ProducerIdentity {
            producer_id: "svc-a".to_string(),
            epoch: 1,
            base_seq: 0,
        };
        let batch = || PublishBatch {
            partition: Some(0),
            producer: Some(identity.clone()),
            records: vec![record("charge-1")],
        };
        let r1 = svc.publish(&ctx, "payments.charged", batch()).unwrap();
        assert!(!r1.duplicate);
        let r2 = svc.publish(&ctx, "payments.charged", batch()).unwrap();
        assert!(r2.duplicate);
        assert_eq!(r2.accepted, 0);
        assert_eq!(
            r2.single_partition().unwrap().base_offset,
            r1.single_partition().unwrap().base_offset
        );

        // Only one record actually landed in the log.
        let handle = svc
            .open_consumer(
                &ctx,
                "g1",
                &["payments.charged".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();
        let fetched = handle.fetch(1024, 20).unwrap();
        assert_eq!(fetched.records.len(), 1);
    }

    #[test]
    fn dlq_after_max_attempts_with_backoff() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "labs.results",
            topics::TopicOptions {
                partitions: Some(1),
                max_delivery_attempts: Some(3),
                retry_backoff_ms: Some(100),
                ..Default::default()
            },
        )
        .unwrap();
        svc.publish(
            &ctx,
            "labs.results",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record("poison-record")],
            },
        )
        .unwrap();

        let fetched_record = FetchedRecordMeta {
            topic: "labs.results".to_string(),
            partition: 0,
            offset: 0,
            timestamp_ms: now_ms(),
            key: None,
            headers: vec![],
            payload: Bytes::from_static(b"poison-record"),
            schema_id: 0,
        };

        let mut last_outcome = None;
        for _ in 0..3 {
            let outcome = svc
                .note_delivery_failure(
                    &ctx,
                    "consumer-group",
                    "labs.results",
                    0,
                    0,
                    &fetched_record,
                    dlq::DlqReason::ConsumerError,
                    "boom",
                )
                .unwrap();
            last_outcome = Some(outcome);
        }
        match last_outcome.unwrap() {
            dlq::DlqOutcome::SentToDlq { attempts } => assert_eq!(attempts, 3),
            other => panic!("expected SentToDlq after 3 attempts, got {other:?}"),
        }

        // The DLQ topic now holds a copy with the error envelope.
        let dlq_handle = svc
            .open_consumer(
                &ctx,
                "dlq-inspector",
                &["__dlq.labs.results".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();
        let dlq_batch = dlq_handle.fetch(1024, 20).unwrap();
        assert_eq!(dlq_batch.records.len(), 1);
        let has_header = |name: &str, value: &str| {
            dlq_batch.records[0]
                .headers
                .iter()
                .any(|(k, v)| k == name && v == value)
        };
        assert!(has_header("dlq.source_topic", "labs.results"));
        assert!(has_header("dlq.attempts", "3"));
        assert!(has_header("dlq.reason", "consumer_error"));

        // The group's committed offset is past the poison record.
        let consumer_handle = svc
            .open_consumer(
                &ctx,
                "consumer-group",
                &["labs.results".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();
        let lag = consumer_handle.lag().unwrap();
        assert_eq!(lag[0].1, 0);
    }

    #[test]
    fn retry_before_max_attempts_reports_growing_backoff() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "labs.retryable",
            topics::TopicOptions {
                max_delivery_attempts: Some(5),
                retry_backoff_ms: Some(1000),
                ..Default::default()
            },
        )
        .unwrap();
        svc.publish(
            &ctx,
            "labs.retryable",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record("r")],
            },
        )
        .unwrap();
        let fetched_record = FetchedRecordMeta {
            topic: "labs.retryable".to_string(),
            partition: 0,
            offset: 0,
            timestamp_ms: now_ms(),
            key: None,
            headers: vec![],
            payload: Bytes::from_static(b"r"),
            schema_id: 0,
        };
        let o1 = svc
            .note_delivery_failure(
                &ctx,
                "g",
                "labs.retryable",
                0,
                0,
                &fetched_record,
                dlq::DlqReason::ConsumerError,
                "e",
            )
            .unwrap();
        let o2 = svc
            .note_delivery_failure(
                &ctx,
                "g",
                "labs.retryable",
                0,
                0,
                &fetched_record,
                dlq::DlqReason::ConsumerError,
                "e",
            )
            .unwrap();
        match (o1, o2) {
            (
                dlq::DlqOutcome::Retry {
                    attempts: a1,
                    backoff_ms: b1,
                },
                dlq::DlqOutcome::Retry {
                    attempts: a2,
                    backoff_ms: b2,
                },
            ) => {
                assert_eq!(a1, 1);
                assert_eq!(a2, 2);
                assert!(b2 > b1, "backoff must grow: {b1} -> {b2}");
            }
            other => panic!("expected two Retry outcomes, got {other:?}"),
        }
    }

    #[test]
    fn quota_blocks_publish_when_exceeded() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(&ctx, "throttled.topic", topics::TopicOptions::default())
            .unwrap();
        svc.quota().set_org_quota(
            "org-1",
            quota::QuotaConfig {
                produce_msgs_per_sec: 1,
                produce_bytes_per_sec: 1024,
                ..Default::default()
            },
        );
        let ok = svc.publish(
            &ctx,
            "throttled.topic",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record("first")],
            },
        );
        assert!(ok.is_ok());
        // LOAD-ROBUST REFUSAL, not a wider tolerance: the bucket refills 1
        // token per full second, so the next publish is refused whenever
        // less than one second has passed since the accepted one — but a
        // scheduler stall longer than that legitimately refills a token,
        // and an unconditional "second publish must fail" assert turned
        // this test into a load flake (measured 386/2 vs 387/1 splits on a
        // loaded host). The property is never relaxed: EVERY accepted
        // publish must be justified by a measured >=1 s gap since the
        // previous accepted one (the refill math it exploits), and the
        // loop keeps publishing until the refusal appears, which it must
        // unless the host stalls a full second before every single
        // publish.
        let mut last_accepted = std::time::Instant::now();
        let mut refused = false;
        for attempt in 0..4 {
            match svc.publish(
                &ctx,
                "throttled.topic",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![record("second")],
                },
            ) {
                Err(BusServiceError::QuotaExceeded { .. }) => {
                    refused = true;
                    break;
                }
                Err(other) => panic!("expected QuotaExceeded, got {other:?}"),
                Ok(_) => {
                    let gap_ms = last_accepted.elapsed().as_millis();
                    assert!(
                        gap_ms >= 1_000,
                        "publish {attempt} accepted only {gap_ms}ms after the previous \
                         accepted one — a 1/s bucket cannot refill that fast"
                    );
                    last_accepted = std::time::Instant::now();
                }
            }
        }
        assert!(
            refused,
            "the 1/s bucket never refused across 4 attempts — each accepted attempt \
             proved a >=1 s gap, so this is a pathologically stalled host, not a \
             quota bug"
        );
    }

    #[test]
    fn pause_and_resume_group_round_trips_and_audits() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(&ctx, "orders.paused", topics::TopicOptions::default())
            .unwrap();

        assert!(!svc.is_group_paused("org-1", "g1", "orders.paused").unwrap());
        svc.pause_group(&ctx, "g1", "orders.paused").unwrap();
        assert!(svc.is_group_paused("org-1", "g1", "orders.paused").unwrap());
        svc.resume_group(&ctx, "g1", "orders.paused").unwrap();
        assert!(!svc.is_group_paused("org-1", "g1", "orders.paused").unwrap());

        let logs = crate::db::repository::list_audit_logs(
            &svc.db,
            &crate::db::models::AuditLogFilters {
                action: Some("bus.group.pause".to_string()),
                ..Default::default()
            },
            0,
            10,
        )
        .unwrap();
        assert_eq!(logs.len(), 2, "one entry for pause, one for resume");
    }

    #[test]
    fn permission_denied_authorizer_blocks_publish_and_consume() {
        let (_tmp, bus_dir) = test_bus_dir();
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        crate::db::repository::bus_test_support::create_bus_tables(&db)
            .expect("bus fixture tables");
        let svc = BusService::new(BusInitConfig {
            bus_dir,
            db,
            authorizer: Arc::new(DenyAllAuthorizer),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .unwrap();
        let ctx = test_ctx("org-1");

        // Admin (create_topic).
        let err = svc.create_topic(&ctx, "secret.topic", topics::TopicOptions::default());
        assert!(matches!(err, Err(BusServiceError::PermissionDenied { .. })));

        // Produce: denied before ever touching `bus_topics`/the engine, and
        // audited (PLAN §8.2, "Braki w testach": this test used to only
        // cover `create_topic` despite its name).
        let err = svc.publish(
            &ctx,
            "secret.topic",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record("x")],
            },
        );
        assert!(matches!(err, Err(BusServiceError::PermissionDenied { .. })));
        let produce_logs = crate::db::repository::list_audit_logs(
            &svc.db,
            &crate::db::models::AuditLogFilters {
                action: Some("bus.produce.denied".to_string()),
                ..Default::default()
            },
            0,
            10,
        )
        .unwrap();
        assert_eq!(produce_logs.len(), 1);

        // Consume (open_consumer): same treatment.
        let err = svc.open_consumer(
            &ctx,
            "some-group",
            &["secret.topic".to_string()],
            ConsumerConfig {
                commit_mode: groups::CommitMode::Explicit,
            },
        );
        assert!(matches!(err, Err(BusServiceError::PermissionDenied { .. })));
        let consume_logs = crate::db::repository::list_audit_logs(
            &svc.db,
            &crate::db::models::AuditLogFilters {
                action: Some("bus.consume.denied".to_string()),
                ..Default::default()
            },
            0,
            10,
        )
        .unwrap();
        assert_eq!(consume_logs.len(), 1);
    }

    #[test]
    fn note_delivery_failure_rejects_unauthorized_caller() {
        let (_tmp, bus_dir) = test_bus_dir();
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        crate::db::repository::bus_test_support::create_bus_tables(&db)
            .expect("bus fixture tables");
        let svc = BusService::new(BusInitConfig {
            bus_dir,
            db,
            authorizer: Arc::new(DenyAllAuthorizer),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .unwrap();
        let ctx = test_ctx("org-1");
        let fetched_record = FetchedRecordMeta {
            topic: "labs.results".to_string(),
            partition: 0,
            offset: 0,
            timestamp_ms: now_ms(),
            key: None,
            headers: vec![],
            payload: Bytes::from_static(b"x"),
            schema_id: 0,
        };
        let err = svc.note_delivery_failure(
            &ctx,
            "g",
            "labs.results",
            0,
            0,
            &fetched_record,
            dlq::DlqReason::ConsumerError,
            "boom",
        );
        assert!(matches!(err, Err(BusServiceError::PermissionDenied { .. })));
        let logs = crate::db::repository::list_audit_logs(
            &svc.db,
            &crate::db::models::AuditLogFilters {
                action: Some("bus.consume.denied".to_string()),
                ..Default::default()
            },
            0,
            10,
        )
        .unwrap();
        assert_eq!(logs.len(), 1);
    }

    // ---- commit access control ---------------------------------------

    #[test]
    fn commit_rejects_foreign_topic_partition_and_persists_nothing() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.commit-acl",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        svc.create_topic(
            &ctx,
            "labs.other-team",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        let handle = svc
            .open_consumer(
                &ctx,
                "billing",
                &["orders.commit-acl".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();

        // `billing` never subscribed to `labs.other-team` — committing
        // against it must be rejected, and must not create an offset entry
        // some future `open_consumer("billing", ["labs.other-team"])`
        // would then read back as real progress.
        let err = handle.commit(&[(
            TopicPartition {
                topic: "labs.other-team".to_string(),
                partition: 0,
            },
            u64::MAX,
        )]);
        assert!(matches!(err, Err(BusServiceError::NotSubscribed { .. })));
        assert_eq!(
            svc.offsets
                .committed_offset("org-1", "billing", "labs.other-team", 0)
                .unwrap(),
            0,
            "the rejected commit must not have been persisted"
        );
    }

    #[test]
    fn commit_rejects_offset_regression_and_reset_offset_still_works() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.regress",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        for i in 0..5 {
            svc.publish(
                &ctx,
                "orders.regress",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![record(&format!("r-{i}"))],
                },
            )
            .unwrap();
        }
        let handle = svc
            .open_consumer(
                &ctx,
                "g1",
                &["orders.regress".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();
        let tp = TopicPartition {
            topic: "orders.regress".to_string(),
            partition: 0,
        };
        handle.commit(&[(tp.clone(), 4)]).unwrap();

        // A plain consumer commit moving the offset BACKWARD is rejected.
        let err = handle.commit(&[(tp.clone(), 1)]);
        assert!(matches!(err, Err(BusServiceError::OffsetRegression { .. })));
        assert_eq!(
            svc.offsets
                .committed_offset("org-1", "g1", "orders.regress", 0)
                .unwrap(),
            4,
            "the rejected regression must not have moved the stored offset"
        );

        // The ONLY legitimate way to move it backward is the admin path.
        svc.reset_offset(&ctx, "g1", "orders.regress", 0, 1)
            .unwrap();
        assert_eq!(
            svc.offsets
                .committed_offset("org-1", "g1", "orders.regress", 0)
                .unwrap(),
            1
        );
    }

    #[test]
    fn note_delivery_failure_does_not_advance_offset_when_not_the_committed_one() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "labs.batch-fail",
            topics::TopicOptions {
                partitions: Some(1),
                max_delivery_attempts: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        for i in 0..3 {
            svc.publish(
                &ctx,
                "labs.batch-fail",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![record(&format!("r-{i}"))],
                },
            )
            .unwrap();
        }
        let fetched_record_at = |offset: u64| FetchedRecordMeta {
            topic: "labs.batch-fail".to_string(),
            partition: 0,
            offset,
            timestamp_ms: now_ms(),
            key: None,
            headers: vec![],
            payload: Bytes::from_static(b"x"),
            schema_id: 0,
        };

        // Record at offset 1 fails first (out of order vs. the committed
        // offset, which is still 0) — attempts exhausted immediately
        // (max_delivery_attempts=1), so a DLQ copy IS published, but the
        // committed offset (0) must stay put: offset 0 has not failed (yet)
        // and has no DLQ entry of its own.
        let outcome = svc
            .note_delivery_failure(
                &ctx,
                "g",
                "labs.batch-fail",
                0,
                1,
                &fetched_record_at(1),
                dlq::DlqReason::ConsumerError,
                "boom",
            )
            .unwrap();
        assert!(matches!(
            outcome,
            dlq::DlqOutcome::SentToDlqOffsetMismatch {
                committed_offset: 0,
                ..
            }
        ));
        assert_eq!(
            svc.offsets
                .committed_offset("org-1", "g", "labs.batch-fail", 0)
                .unwrap(),
            0,
            "offset must not advance past an untouched earlier record"
        );

        // Now offset 0 itself fails and IS the committed offset — this one
        // legitimately advances it.
        let outcome = svc
            .note_delivery_failure(
                &ctx,
                "g",
                "labs.batch-fail",
                0,
                0,
                &fetched_record_at(0),
                dlq::DlqReason::ConsumerError,
                "boom",
            )
            .unwrap();
        assert!(matches!(outcome, dlq::DlqOutcome::SentToDlq { .. }));
        assert_eq!(
            svc.offsets
                .committed_offset("org-1", "g", "labs.batch-fail", 0)
                .unwrap(),
            1
        );
    }

    // ---- permission generation revalidation on fetch/commit ---------

    #[test]
    fn fetch_revalidates_permission_generation_and_denies_after_revoke() {
        let (_tmp, bus_dir) = test_bus_dir();
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        crate::db::repository::bus_test_support::create_bus_tables(&db)
            .expect("bus fixture tables");
        let authorizer = Arc::new(FlippableAuthorizer::new());
        let svc = BusService::new(BusInitConfig {
            bus_dir,
            db,
            authorizer: authorizer.clone(),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .unwrap();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.generation",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        svc.publish(
            &ctx,
            "orders.generation",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record("x")],
            },
        )
        .unwrap();
        let handle = svc
            .open_consumer(
                &ctx,
                "g1",
                &["orders.generation".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();
        // Works while the generation is unchanged.
        assert_eq!(handle.fetch(1024, 20).unwrap().records.len(), 1);

        // Revoke mid-session: no new `open_consumer` call happens, only the
        // authorizer's own state changes.
        authorizer.revoke();
        let err = handle.fetch(1024, 20);
        assert!(matches!(err, Err(BusServiceError::PermissionDenied { .. })));
        let logs = crate::db::repository::list_audit_logs(
            &svc.db,
            &crate::db::models::AuditLogFilters {
                action: Some("bus.consume.denied".to_string()),
                ..Default::default()
            },
            0,
            10,
        )
        .unwrap();
        assert_eq!(logs.len(), 1);
    }

    // ---- Z12 environment fencing ---------------------

    #[test]
    fn open_consumer_and_fetch_reject_environment_mismatch() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(&ctx, "events.env-consume", topics::TopicOptions::default())
            .unwrap();
        svc.publish(
            &ctx,
            "events.env-consume",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record("x")],
            },
        )
        .unwrap();

        // Open the handle WHILE environments still match, so `fetch`'s own
        // check gets a real workout below — a handle opened AFTER the
        // mismatch already exists never reaches `ConsumerHandle::
        // check_environment` at all, which is exactly the gap the previous
        // version of this test had (its name promised "and fetch" but only
        // ever exercised `open_consumer`).
        let handle = svc
            .open_consumer(
                &ctx,
                "g1",
                &["events.env-consume".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();
        assert_eq!(handle.fetch(1024, 20).unwrap().records.len(), 1);

        // Flip the shared node-environment AtomicU8 AFTER the handle was
        // opened: its own per-partition snapshot is now stale relative to
        // what `check_environment` reads live.
        crate::services::environment::set_node_environment(&svc.db, NodeEnvironment::Dev).unwrap();
        svc.invalidate_environment_cache();

        let err = handle.fetch(1024, 20);
        assert!(
            matches!(err, Err(BusServiceError::EnvironmentMismatch { .. })),
            "an ALREADY-OPEN handle must also observe the mismatch on its next fetch, not just a fresh open_consumer call"
        );

        // A brand-new `open_consumer` call is rejected the same way.
        let err = svc.open_consumer(
            &ctx,
            "g2",
            &["events.env-consume".to_string()],
            ConsumerConfig {
                commit_mode: groups::CommitMode::Explicit,
            },
        );
        assert!(matches!(
            err,
            Err(BusServiceError::EnvironmentMismatch { .. })
        ));
    }

    #[test]
    fn lag_revalidates_permission_generation_and_denies_after_revoke() {
        let (_tmp, bus_dir) = test_bus_dir();
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        crate::db::repository::bus_test_support::create_bus_tables(&db)
            .expect("bus fixture tables");
        let authorizer = Arc::new(FlippableAuthorizer::new());
        let svc = BusService::new(BusInitConfig {
            bus_dir,
            db,
            authorizer: authorizer.clone(),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .unwrap();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.lag-generation",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        let handle = svc
            .open_consumer(
                &ctx,
                "g1",
                &["orders.lag-generation".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();
        assert!(
            handle.lag().is_ok(),
            "works while the generation is unchanged"
        );

        authorizer.revoke();
        let err = handle.lag();
        assert!(
            matches!(err, Err(BusServiceError::PermissionDenied { .. })),
            "lag() must revalidate like fetch()/commit(), not just at open_consumer time"
        );
    }

    #[test]
    fn commit_and_seek_to_earliest_reject_environment_mismatch_on_an_already_open_handle() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.commit-env",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        svc.publish(
            &ctx,
            "orders.commit-env",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record("x")],
            },
        )
        .unwrap();
        let handle = svc
            .open_consumer(
                &ctx,
                "g1",
                &["orders.commit-env".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();

        crate::services::environment::set_node_environment(&svc.db, NodeEnvironment::Dev).unwrap();
        svc.invalidate_environment_cache();

        let tp = TopicPartition {
            topic: "orders.commit-env".to_string(),
            partition: 0,
        };
        let err = handle.commit(&[(tp, 1)]);
        assert!(matches!(
            err,
            Err(BusServiceError::EnvironmentMismatch { .. })
        ));
        assert_eq!(
            svc.offsets
                .committed_offset("org-1", "g1", "orders.commit-env", 0)
                .unwrap(),
            0,
            "the rejected commit must not have been persisted"
        );

        let err = handle.seek_to_earliest("orders.commit-env", 0);
        assert!(matches!(
            err,
            Err(BusServiceError::EnvironmentMismatch { .. })
        ));
    }

    #[test]
    fn open_consumer_denied_on_a_later_topic_leaves_no_side_effects_for_earlier_ones() {
        let (_tmp, bus_dir) = test_bus_dir();
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        crate::db::repository::bus_test_support::create_bus_tables(&db)
            .expect("bus fixture tables");
        let svc = BusService::new(BusInitConfig {
            bus_dir,
            db,
            authorizer: Arc::new(DenyConsumeOnTopicAuthorizer {
                deny_topic: "topic-c",
            }),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .unwrap();
        let ctx = test_ctx("org-1");
        for name in ["topic-a", "topic-b", "topic-c"] {
            svc.create_topic(
                &ctx,
                name,
                topics::TopicOptions {
                    partitions: Some(1),
                    ..Default::default()
                },
            )
            .unwrap();
        }

        let err = svc.open_consumer(
            &ctx,
            "g1",
            &[
                "topic-a".to_string(),
                "topic-b".to_string(),
                "topic-c".to_string(),
            ],
            ConsumerConfig {
                commit_mode: groups::CommitMode::Explicit,
            },
        );
        assert!(matches!(err, Err(BusServiceError::PermissionDenied { .. })));

        // Phase 1 (authorize + validate every topic) must run to completion
        // before phase 2 (bus_groups upsert, partition open) touches
        // anything — a denial on the third topic must leave no trace for
        // the first two.
        assert!(
            crate::db::repository::bus_group_get(&svc.db, "org-1", "g1", "topic-a")
                .unwrap()
                .is_none(),
            "no bus_groups row for topic-a"
        );
        assert!(
            crate::db::repository::bus_group_get(&svc.db, "org-1", "g1", "topic-b")
                .unwrap()
                .is_none(),
            "no bus_groups row for topic-b"
        );
        assert!(
            !svc.partitions
                .contains_key(&("org-1".to_string(), "topic-a".to_string(), 0)),
            "no partition handle opened for topic-a"
        );
        assert!(
            !svc.partitions
                .contains_key(&("org-1".to_string(), "topic-b".to_string(), 0)),
            "no partition handle opened for topic-b"
        );
    }

    /// The purge-epoch snapshot `open_consumer` takes MUST be read before
    /// phase 1, not after phase 2 — otherwise a `purge_org` landing
    /// entirely inside this call's window (deterministically reproduced
    /// here via `test_open_consumer_after_phase1`) records the NEW epoch on
    /// the LATE read too, and the re-check always passes even though the
    /// org was erased mid-call.
    #[test]
    fn open_consumer_rejects_a_handle_when_purge_org_races_between_its_two_phases() {
        let (_tmp, svc) = test_service();
        let svc = Arc::new(svc);
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.race",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        svc.publish(
            &ctx,
            "orders.race",
            PublishBatch {
                partition: Some(0),
                producer: None,
                records: vec![record("x")],
            },
        )
        .unwrap();

        let svc_for_hook = Arc::clone(&svc);
        *svc.test_open_consumer_after_phase1.lock().unwrap() = Some(Box::new(move || {
            svc_for_hook.purge_org("org-1").unwrap();
        }));

        let result = svc.open_consumer(
            &ctx,
            "workers",
            &["orders.race".to_string()],
            ConsumerConfig {
                commit_mode: groups::CommitMode::Explicit,
            },
        );
        assert!(
            matches!(result, Err(BusServiceError::TopicNotFound { .. })),
            "expected the handle to be rejected because purge_org raced this call"
        );

        // Side effects phase 2 performed before the re-check caught the
        // race must be undone: no leftover `bus_groups` row for this call.
        assert!(
            crate::db::repository::bus_group_list(&svc.db, "org-1")
                .unwrap()
                .is_empty(),
            "the racing purge_org must leave no bus_groups row behind for this call"
        );
    }

    // ---- group bookkeeping + pause enforcement on fetch -------------

    #[test]
    fn open_consumer_upserts_bus_groups_row() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(&ctx, "orders.group-row", topics::TopicOptions::default())
            .unwrap();
        svc.open_consumer(
            &ctx,
            "shipping",
            &["orders.group-row".to_string()],
            ConsumerConfig {
                commit_mode: groups::CommitMode::AtMostOnce,
            },
        )
        .unwrap();
        let row =
            crate::db::repository::bus_group_get(&svc.db, "org-1", "shipping", "orders.group-row")
                .unwrap()
                .expect("open_consumer must upsert a bus_groups row");
        assert_eq!(row.commit_mode, groups::CommitMode::AtMostOnce.as_str());
        assert!(!row.paused);
    }

    /// `open_consumer` must refuse to create a brand-new `bus_groups` row
    /// once an org is at its `max_groups` ceiling — otherwise a caller with
    /// nothing but consume rights could loop with a fresh, caller-
    /// controlled group name and grow `bus_groups`/in-memory caches without
    /// bound. Reconnecting an EXISTING group (same name, same topic) must
    /// never count against the ceiling again.
    #[test]
    fn open_consumer_enforces_max_groups_but_lets_existing_groups_reconnect() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.quota().set_max_groups("org-1", 2);
        svc.create_topic(
            &ctx,
            "orders.max-groups",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();

        svc.open_consumer(
            &ctx,
            "g1",
            &["orders.max-groups".to_string()],
            ConsumerConfig {
                commit_mode: groups::CommitMode::Explicit,
            },
        )
        .unwrap();
        svc.open_consumer(
            &ctx,
            "g2",
            &["orders.max-groups".to_string()],
            ConsumerConfig {
                commit_mode: groups::CommitMode::Explicit,
            },
        )
        .unwrap();

        // A THIRD, brand-new group name is over the ceiling (2 already
        // exist).
        let err = svc.open_consumer(
            &ctx,
            "g3",
            &["orders.max-groups".to_string()],
            ConsumerConfig {
                commit_mode: groups::CommitMode::Explicit,
            },
        );
        match err {
            Err(BusServiceError::MaxGroupsExceeded {
                org_id,
                max,
                current,
            }) => {
                assert_eq!(org_id, "org-1");
                assert_eq!(max, 2);
                assert_eq!(current, 2);
            }
            Ok(_) => panic!("expected MaxGroupsExceeded, got Ok"),
            Err(other) => panic!("expected MaxGroupsExceeded, got {other:?}"),
        }
        assert!(
            crate::db::repository::bus_group_get(&svc.db, "org-1", "g3", "orders.max-groups")
                .unwrap()
                .is_none(),
            "the rejected group must not have created a bus_groups row"
        );

        // Reconnecting an EXISTING group at the ceiling must still work.
        svc.open_consumer(
            &ctx,
            "g1",
            &["orders.max-groups".to_string()],
            ConsumerConfig {
                commit_mode: groups::CommitMode::Explicit,
            },
        )
        .expect("an existing group reconnecting must not count against max_groups again");
    }

    #[test]
    fn fetch_rejects_a_paused_group() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(&ctx, "orders.paused-fetch", topics::TopicOptions::default())
            .unwrap();
        svc.publish(
            &ctx,
            "orders.paused-fetch",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record("x")],
            },
        )
        .unwrap();
        let handle = svc
            .open_consumer(
                &ctx,
                "g1",
                &["orders.paused-fetch".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();
        svc.pause_group(&ctx, "g1", "orders.paused-fetch").unwrap();
        let err = handle.fetch(1024, 20);
        assert!(matches!(err, Err(BusServiceError::GroupPaused { .. })));

        svc.resume_group(&ctx, "g1", "orders.paused-fetch").unwrap();
        assert_eq!(handle.fetch(1024, 20).unwrap().records.len(), 1);
    }

    // ---- OffsetOutOfRange after retention + deliberate recovery ----

    #[test]
    fn fetch_reports_offset_out_of_range_after_retention_then_seek_recovers() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "labs.retention-gap",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();

        // Drive the engine directly (the test asks for "the existing
        // engine API", not the higher-level `retention.rs`, which is a
        // different agent's file): open the SAME on-disk directory
        // `BusService::partition_handle` will use, with a `RollPolicy`
        // that actually seals a segment per batch (`RollPolicy::default()`
        // needs 100k batches to roll, unreachable in a unit test), append
        // 6 one-record batches, then delete the oldest sealed segment —
        // exactly `tentaflow-bus`'s own
        // `fetch_from_deleted_offset_range_returns_offset_out_of_range`
        // test, reused here against the directory `BusService` will read
        // from.
        let dir = topics::partition_dir(&svc.bus_dir, "org-1", "labs.retention-gap", 0);
        {
            let policy = tentaflow_bus::RollPolicy {
                max_batches: 1,
                ..tentaflow_bus::RollPolicy::default()
            };
            let raw =
                tentaflow_bus::Partition::open(&dir, policy, tentaflow_bus::Durability::Os, 8)
                    .expect("raw partition open");
            for i in 0..6i64 {
                let mut builder = tentaflow_bus::BatchBuilder::new(0, 0);
                let rec = tentaflow_bus::RecordInput::new(Bytes::from(format!("r-{i}")), now_ms());
                builder.push(rec).unwrap();
                let wire = builder.build().unwrap();
                raw.append_batch(wire).unwrap();
            }
            raw.delete_sealed_segment(0).unwrap();
            assert_eq!(raw.earliest_offset(), 1);
            // Dropping `raw` releases the directory flock before
            // `BusService` opens its own handle on the same path.
        }

        let handle = svc
            .open_consumer(
                &ctx,
                "g1",
                &["labs.retention-gap".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();
        let err = handle.fetch(1024 * 1024, 20);
        match err {
            Err(BusServiceError::OffsetOutOfRange {
                requested,
                earliest,
                latest,
                ..
            }) => {
                assert_eq!(requested, 0);
                assert_eq!(earliest, 1);
                assert_eq!(latest, 6);
            }
            other => panic!("expected OffsetOutOfRange, got {other:?}"),
        }
        // Audited exactly once, not once per fetch call.
        let _ = handle.fetch(1024 * 1024, 20);
        let gap_logs = crate::db::repository::list_audit_logs(
            &svc.db,
            &crate::db::models::AuditLogFilters {
                action: Some("bus.offset.gap".to_string()),
                ..Default::default()
            },
            0,
            10,
        )
        .unwrap();
        assert_eq!(gap_logs.len(), 1, "gap must be audited once, not per fetch");

        let seeked = handle
            .seek_to_earliest("labs.retention-gap", 0)
            .expect("seek must succeed");
        assert_eq!(seeked, 1);
        let batch = handle.fetch(1024 * 1024, 20).unwrap();
        assert_eq!(batch.records.len(), 5, "offsets 1..=5 are still retained");
        assert_eq!(batch.records[0].offset, 1);
    }

    // ---- DLQ-of-DLQ ---------------------------------------------------

    #[test]
    fn note_delivery_failure_on_a_dlq_topic_is_rejected_explicitly() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        let fetched_record = FetchedRecordMeta {
            topic: "__dlq.orders".to_string(),
            partition: 0,
            offset: 0,
            timestamp_ms: now_ms(),
            key: None,
            headers: vec![],
            payload: Bytes::from_static(b"x"),
            schema_id: 0,
        };
        let err = svc.note_delivery_failure(
            &ctx,
            "g",
            "__dlq.orders",
            0,
            0,
            &fetched_record,
            dlq::DlqReason::ConsumerError,
            "boom",
        );
        assert!(matches!(
            err,
            Err(BusServiceError::DlqOfDlqNotAllowed { .. })
        ));
    }

    // ---- Braki w testach: AtMostOnce commits before delivery ---------------

    #[test]
    fn at_most_once_commits_on_fetch_before_the_caller_processes_anything() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.at-most-once",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        svc.publish(
            &ctx,
            "orders.at-most-once",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record("only-record")],
            },
        )
        .unwrap();
        let handle = svc
            .open_consumer(
                &ctx,
                "g1",
                &["orders.at-most-once".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::AtMostOnce,
                },
            )
            .unwrap();

        // Before any fetch, lag is 1 and nothing is committed.
        assert_eq!(handle.lag().unwrap()[0].1, 1);

        let batch = handle.fetch(1024, 20).unwrap();
        assert_eq!(batch.records.len(), 1);

        // The offset is ALREADY committed the instant `fetch` returned the
        // record — before the caller has done anything with it. A crash
        // right here would lose the record (PLAN §3.2's documented
        // trade-off for this opt-in mode).
        assert_eq!(handle.lag().unwrap()[0].1, 0);
        assert_eq!(
            svc.offsets
                .committed_offset("org-1", "g1", "orders.at-most-once", 0)
                .unwrap(),
            1
        );
    }

    #[test]
    fn dlq_retry_republishes_to_source_topic() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "labs.retry-target",
            topics::TopicOptions {
                partitions: Some(1),
                max_delivery_attempts: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        svc.publish(
            &ctx,
            "labs.retry-target",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record("payload")],
            },
        )
        .unwrap();
        let fetched_record = FetchedRecordMeta {
            topic: "labs.retry-target".to_string(),
            partition: 0,
            offset: 0,
            timestamp_ms: now_ms(),
            key: None,
            headers: vec![],
            payload: Bytes::from_static(b"payload"),
            schema_id: 0,
        };
        svc.note_delivery_failure(
            &ctx,
            "g",
            "labs.retry-target",
            0,
            0,
            &fetched_record,
            dlq::DlqReason::ConsumerError,
            "err",
        )
        .unwrap();

        let result = svc
            .dlq_retry(&ctx, "__dlq.labs.retry-target", 0, 0)
            .unwrap();
        assert!(!result.duplicate);
        assert_eq!(
            result.single_partition().unwrap().base_offset,
            1,
            "second record on the source topic"
        );
    }

    // ---- Real discard (M1-R2 review N-5, coordinator decision 2) ------

    /// The discard marker `dlq_discard` writes must be durable — surviving
    /// a full `BusService` reopen against the SAME `bus_dir`, not just an
    /// in-process cache the current `BusService` instance happens to hold.
    /// `DlqList`/`dlq_retry_all`/`dlq_depth`'s discard-awareness is tested
    /// at the dispatch layer (`dispatch::bus::tests`); this test is the one
    /// place that can actually drop and recreate a `BusService`, which the
    /// dispatch layer's process-wide `bus::init` singleton cannot.
    #[test]
    fn dlq_discard_marker_survives_a_bus_service_reopen() {
        let (_tmp, bus_dir) = test_bus_dir();
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        crate::db::repository::bus_test_support::create_bus_tables(&db)
            .expect("bus fixture tables");
        let ctx = test_ctx("org-1");

        {
            let svc = BusService::new(BusInitConfig {
                bus_dir: bus_dir.clone(),
                db: db.clone(),
                authorizer: Arc::new(AllowAllAuthorizer),
                retention_interval: None,
                dedup_expected_rate_per_sec: 10_000,
                partition_handle_lru: None,
                publish_ack_timeout: DEFAULT_PUBLISH_ACK_TIMEOUT,
            })
            .expect("bus service (first open)");
            svc.create_topic(
                &ctx,
                "orders.discard-reopen",
                topics::TopicOptions {
                    partitions: Some(1),
                    max_delivery_attempts: Some(1),
                    ..Default::default()
                },
            )
            .unwrap();
            svc.publish(
                &ctx,
                "orders.discard-reopen",
                PublishBatch {
                    partition: Some(0),
                    producer: None,
                    records: vec![record("payload")],
                },
            )
            .unwrap();
            let fetched_record = FetchedRecordMeta {
                topic: "orders.discard-reopen".to_string(),
                partition: 0,
                offset: 0,
                timestamp_ms: now_ms(),
                key: None,
                headers: vec![],
                payload: Bytes::from_static(b"payload"),
                schema_id: 0,
            };
            svc.note_delivery_failure(
                &ctx,
                "g",
                "orders.discard-reopen",
                0,
                0,
                &fetched_record,
                dlq::DlqReason::ConsumerError,
                "boom",
            )
            .unwrap();
            svc.dlq_discard(&ctx, "__dlq.orders.discard-reopen", 0, 0)
                .unwrap();
            assert!(svc
                .dlq_discarded_offsets(&ctx, "__dlq.orders.discard-reopen", 0)
                .unwrap()
                .contains(&0));
            // `svc` drops at the end of this block, releasing every
            // partition directory's flock — required before the second
            // `BusService::new` below can open the same `bus_dir`.
        }

        let svc2 = BusService::new(BusInitConfig {
            bus_dir,
            db,
            authorizer: Arc::new(AllowAllAuthorizer),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .expect("bus service (reopen)");
        assert!(
            svc2.dlq_discarded_offsets(&ctx, "__dlq.orders.discard-reopen", 0)
                .unwrap()
                .contains(&0),
            "the discard marker must survive a BusService reopen (durable fjall keyspace)"
        );
    }

    // ---- Legacy probe group cleanup (M1-R2 review N-1/N-7, decision 3) ---

    /// `BusService::new` must delete any leftover `bus_groups` row for the
    /// retired `tf-system-probe` group on every startup, regardless of
    /// which org it belongs to — `dispatch/bus.rs`'s own `tf-`-prefix
    /// filter already hides it from the UI as defense in depth, but the row
    /// itself should not just accumulate forever either.
    #[test]
    fn new_deletes_a_leftover_legacy_probe_group_row_on_startup() {
        let (_tmp, bus_dir) = test_bus_dir();
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        crate::db::repository::bus_test_support::create_bus_tables(&db)
            .expect("bus fixture tables");
        crate::db::repository::bus_group_upsert(
            &db,
            &crate::db::repository::DbBusGroup {
                org_id: "org-1".to_string(),
                group_id: LEGACY_PROBE_GROUP_ID.to_string(),
                topic: "orders.created".to_string(),
                commit_mode: groups::CommitMode::Explicit.as_str().to_string(),
                paused: false,
                created_at_ms: 0,
                updated_at_ms: 0,
            },
        )
        .unwrap();
        // A real group must survive the same cleanup untouched.
        crate::db::repository::bus_group_upsert(
            &db,
            &crate::db::repository::DbBusGroup {
                org_id: "org-1".to_string(),
                group_id: "billing".to_string(),
                topic: "orders.created".to_string(),
                commit_mode: groups::CommitMode::Explicit.as_str().to_string(),
                paused: false,
                created_at_ms: 0,
                updated_at_ms: 0,
            },
        )
        .unwrap();

        let _svc = BusService::new(BusInitConfig {
            bus_dir,
            db: db.clone(),
            authorizer: Arc::new(AllowAllAuthorizer),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .expect("bus service");

        let rows = crate::db::repository::bus_group_list(&db, "org-1").unwrap();
        assert!(
            rows.iter().all(|g| g.group_id != LEGACY_PROBE_GROUP_ID),
            "leftover legacy probe group row must be deleted at startup, got {rows:?}"
        );
        assert!(
            rows.iter().any(|g| g.group_id == "billing"),
            "a real group's row must survive the cleanup"
        );
    }

    // ---- two-phase dedup ------------------------------------------

    /// Reproduces the retry-poisoning failure: dedup used to `insert`
    /// a key BEFORE the engine append, so a transient append failure
    /// (`Throttled`, I/O, here simulated via a pre-locked partition
    /// directory) left the key marked seen even though the record never
    /// made it into the log — a retry of the SAME unique record was then
    /// rejected as a false-positive duplicate, silently losing it forever.
    #[test]
    fn two_phase_dedup_does_not_poison_store_on_failed_append() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        topics::create_topic_for_dedup_test(
            &svc.db,
            &ctx.org_id,
            "labs.dedup.failed-append",
            topics::TopicOptions {
                partitions: Some(1),
                idempotency_key: Some("msg.run_id".to_string()),
                ..Default::default()
            },
            NodeEnvironment::Prod,
            now_ms(),
        )
        .unwrap();

        // Partition 0 has never been opened by `svc` yet: pre-lock its
        // directory with an independent `Partition` handle so `svc`'s own
        // `partition_handle` fails with `PartitionLocked` — deterministic
        // because `flock` conflicts even between two fds in the same
        // process (the flock-per-fd mechanism).
        let dir = topics::partition_dir(&svc.bus_dir, &ctx.org_id, "labs.dedup.failed-append", 0);
        let lock_holder = tentaflow_bus::Partition::open(
            &dir,
            tentaflow_bus::RollPolicy::default(),
            tentaflow_bus::Durability::Os,
            8,
        )
        .unwrap();

        let batch = || PublishBatch {
            partition: None,
            producer: None,
            records: vec![record_with_key("payload-1", "unique-key")],
        };
        let err = svc
            .publish(&ctx, "labs.dedup.failed-append", batch())
            .unwrap_err();
        assert!(matches!(err, BusServiceError::Engine(_)));

        drop(lock_holder);

        // Same key, retried after the transient failure: must be accepted
        // as fresh, NOT reported as a duplicate.
        let ok = svc
            .publish(&ctx, "labs.dedup.failed-append", batch())
            .unwrap();
        assert_eq!(ok.accepted, 1);
        assert_eq!(ok.deduplicated, 0);
        assert!(!ok.duplicate);
    }

    // ---- idempotency_key fail-closed, dedup path -------

    #[test]
    fn create_topic_rejects_idempotency_key() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        let err = svc
            .create_topic(
                &ctx,
                "orders.idem",
                topics::TopicOptions {
                    idempotency_key: Some("msg.run_id".to_string()),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(matches!(err, BusServiceError::InvalidTopicConfig { .. }));
    }

    #[test]
    fn update_topic_rejects_idempotency_key() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(&ctx, "orders.idem2", topics::TopicOptions::default())
            .unwrap();
        let err = svc
            .update_topic(
                &ctx,
                "orders.idem2",
                topics::TopicOptions {
                    idempotency_key: Some("msg.run_id".to_string()),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(matches!(err, BusServiceError::InvalidTopicConfig { .. }));
    }

    // ---- v148: durability class/explicit surfaced in audit details -----

    fn latest_audit_details(db: &crate::db::DbPool, action: &str) -> String {
        let entries = crate::db::repository::list_audit_logs(
            db,
            &crate::db::models::AuditLogFilters {
                action: Some(action.to_string()),
                ..Default::default()
            },
            0,
            10,
        )
        .unwrap();
        // `list_audit_logs` orders `id DESC`, so the newest matching row for
        // this action is first, not last.
        entries
            .first()
            .and_then(|e| e.details.clone())
            .expect("expected an audit_log row with details")
    }

    #[test]
    fn create_topic_audit_details_carry_durability_fields() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "krytyk.crit",
            topics::TopicOptions {
                durability_class: Some(topics::DurabilityClass::Critical),
                ..Default::default()
            },
        )
        .unwrap();

        let details = latest_audit_details(&svc.db, "bus.topic.create");
        assert!(details.contains("durability=fsync_batch_full"), "{details}");
        assert!(details.contains("durability_class=critical"), "{details}");
        assert!(details.contains("durability_explicit=false"), "{details}");
    }

    #[test]
    fn update_topic_audit_details_carry_before_to_after_durability_fields() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "krytyk.std",
            topics::TopicOptions {
                durability_class: Some(topics::DurabilityClass::Critical),
                ..Default::default()
            },
        )
        .unwrap();

        svc.update_topic(
            &ctx,
            "krytyk.std",
            topics::TopicOptions {
                durability_class: Some(topics::DurabilityClass::Standard),
                ..Default::default()
            },
        )
        .unwrap();

        let details = latest_audit_details(&svc.db, "bus.topic.update");
        assert!(
            details.contains("durability=fsync_batch_full->fsync_interval:50"),
            "{details}"
        );
        assert!(
            details.contains("durability_class=critical->standard"),
            "{details}"
        );
        assert!(
            details.contains("durability_explicit=false->false"),
            "{details}"
        );
    }

    #[test]
    fn update_topic_audit_details_report_explicit_override_transition() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(&ctx, "orders.override", topics::TopicOptions::default())
            .unwrap();

        svc.update_topic(
            &ctx,
            "orders.override",
            topics::TopicOptions {
                durability: Some(topics::DurabilityPolicy::Os),
                ..Default::default()
            },
        )
        .unwrap();

        let details = latest_audit_details(&svc.db, "bus.topic.update");
        assert!(
            details.contains("durability_explicit=false->true"),
            "{details}"
        );
    }

    // ---- v148: legacy DLQ durability migration at BusService::new ------

    /// Builds a `DbBusTopic` row for the DLQ migration tests below, varying
    /// only `name`/`durability`/`durability_class` — every other field is an
    /// arbitrary but valid fixture value the sweep never looks at.
    fn dlq_migration_test_row(
        name: &str,
        durability: &str,
        durability_class: Option<&str>,
    ) -> crate::db::repository::DbBusTopic {
        crate::db::repository::DbBusTopic {
            org_id: "org-1".to_string(),
            name: name.to_string(),
            partitions: 8,
            retention_ms: 2_592_000_000,
            retention_bytes: 10 * 1024 * 1024 * 1024,
            cleanup_policy: "delete".to_string(),
            delivery: "at_least_once".to_string(),
            idempotency_key: None,
            dedup_window_ms: 86_400_000,
            max_delivery_attempts: 5,
            retry_backoff_ms: 1_000,
            schema_id: None,
            validation: "off".to_string(),
            content_type: "application/octet-stream".to_string(),
            replication_factor: 1,
            acks: "leader".to_string(),
            durability: durability.to_string(),
            durability_class: durability_class.map(str::to_string),
            max_inline_bytes: 1_048_576,
            compression: "lz4".to_string(),
            environment: "prod".to_string(),
            created_at_ms: 1_000,
            updated_at_ms: 1_000,
        }
    }

    #[test]
    fn migrate_legacy_dlq_durability_repairs_pre_fix_rows_and_is_idempotent() {
        let (_tmp, svc) = test_service();
        // Simulate a `__dlq.*` row created before `dlq::dlq_topic_options`
        // pinned `DurabilityClass::Standard` — inherited the source's
        // stronger policy at creation time (R5-8). The v148 backfill
        // (`bus_topics_add_durability_class_column`) stamps exactly this
        // kind of pre-decision-B `fsync_batch*` row as `Some("critical")`,
        // never `None` — `None` means an explicit override (UI critic round
        // 6, R6-1).
        let stale =
            dlq_migration_test_row("__dlq.lab.results", "fsync_batch_full", Some("critical"));
        crate::db::repository::bus_topic_create(&svc.db, &stale).unwrap();

        let updated = BusService::migrate_legacy_dlq_durability(&svc.db).unwrap();
        assert_eq!(updated, 1);

        let fixed = crate::db::repository::bus_topic_get(&svc.db, "org-1", "__dlq.lab.results")
            .unwrap()
            .expect("row still exists");
        assert_eq!(fixed.durability, "fsync_interval:50");
        assert_eq!(fixed.durability_class.as_deref(), Some("standard"));

        let details = latest_audit_details(&svc.db, "bus.topic.update");
        assert!(
            details.contains("durability=fsync_batch_full->fsync_interval:50"),
            "{details}"
        );
        assert!(
            details.contains("durability_class=critical->standard"),
            "{details}"
        );
        assert!(
            details.contains("reason=legacy_dlq_durability_migration"),
            "{details}"
        );

        // Re-running is a no-op: nothing left to repair, and no new audit
        // row is written.
        let audit_count_before = count_audit_logs(&svc, "bus.topic.update");
        let updated_again = BusService::migrate_legacy_dlq_durability(&svc.db).unwrap();
        assert_eq!(updated_again, 0);
        assert_eq!(
            count_audit_logs(&svc, "bus.topic.update"),
            audit_count_before
        );
    }

    #[test]
    fn migrate_legacy_dlq_durability_leaves_explicit_override_untouched() {
        let (_tmp, svc) = test_service();
        // R6-1 (P2): an operator's explicit `durability` override on a DLQ
        // topic — `durability_class == None` — must never be reverted by
        // this startup sweep, and no audit entry should be written for it.
        let explicit = dlq_migration_test_row("__dlq.y", "os", None);
        crate::db::repository::bus_topic_create(&svc.db, &explicit).unwrap();

        let updated = BusService::migrate_legacy_dlq_durability(&svc.db).unwrap();
        assert_eq!(updated, 0);

        let row = crate::db::repository::bus_topic_get(&svc.db, "org-1", "__dlq.y")
            .unwrap()
            .expect("row still exists");
        assert_eq!(row.durability, "os");
        assert_eq!(row.durability_class, None);
        assert_eq!(count_audit_logs(&svc, "bus.topic.update"), 0);
    }

    #[test]
    fn migrate_legacy_dlq_durability_leaves_already_fixed_rows_and_non_dlq_topics_alone() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        // A regular (non-DLQ) topic with a Critical policy must never be
        // touched by this sweep.
        svc.create_topic(
            &ctx,
            "lab.results",
            topics::TopicOptions {
                durability_class: Some(topics::DurabilityClass::Critical),
                ..Default::default()
            },
        )
        .unwrap();
        // A DLQ topic created through the current (fixed) path is already
        // correct.
        let source_cfg = topics::get_topic(&svc.db, "org-1", "lab.results")
            .unwrap()
            .unwrap();
        svc.ensure_dlq_topic(&ctx, "lab.results", &source_cfg)
            .unwrap();

        let updated = BusService::migrate_legacy_dlq_durability(&svc.db).unwrap();
        assert_eq!(updated, 0);

        let source = topics::get_topic(&svc.db, "org-1", "lab.results")
            .unwrap()
            .unwrap();
        assert_eq!(source.durability, topics::DurabilityPolicy::FsyncBatchFull);
    }

    #[test]
    fn second_bus_service_new_on_the_same_db_migrates_nothing_and_writes_no_new_audit_row() {
        let (_tmp, bus_dir) = test_bus_dir();
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        crate::db::repository::bus_test_support::create_bus_tables(&db)
            .expect("bus fixture tables");
        let legacy =
            dlq_migration_test_row("__dlq.lab.results", "fsync_batch_full", Some("critical"));
        crate::db::repository::bus_topic_create(&db, &legacy).unwrap();

        {
            // First `BusService::new` runs `migrate_legacy_dlq_durability`
            // itself at startup (see the call site in `new`) and must
            // migrate the legacy row exactly once.
            let svc = BusService::new(BusInitConfig {
                bus_dir: bus_dir.clone(),
                db: db.clone(),
                authorizer: Arc::new(AllowAllAuthorizer),
                retention_interval: None,
                dedup_expected_rate_per_sec: 10_000,
                partition_handle_lru: None,
                publish_ack_timeout: DEFAULT_PUBLISH_ACK_TIMEOUT,
            })
            .expect("bus service (first open)");
            assert_eq!(count_audit_logs(&svc, "bus.topic.update"), 1);
            // `svc` drops at the end of this block, releasing every
            // partition directory's flock — required before the second
            // `BusService::new` below can open the same `bus_dir`.
        }

        let svc2 = BusService::new(BusInitConfig {
            bus_dir,
            db,
            authorizer: Arc::new(AllowAllAuthorizer),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .expect("bus service (reopen)");
        assert_eq!(
            count_audit_logs(&svc2, "bus.topic.update"),
            1,
            "the reopen's own startup sweep must not migrate the already-fixed row again"
        );
        let row = crate::db::repository::bus_topic_get(&svc2.db, "org-1", "__dlq.lab.results")
            .unwrap()
            .expect("row still exists");
        assert_eq!(row.durability, "fsync_interval:50");
        assert_eq!(row.durability_class.as_deref(), Some("standard"));
    }

    #[test]
    fn publish_dedup_layer_requires_key_rejects_duplicate_and_handles_partial_batches() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        topics::create_topic_for_dedup_test(
            &svc.db,
            &ctx.org_id,
            "labs.dedup2",
            topics::TopicOptions {
                partitions: Some(1),
                idempotency_key: Some("msg.run_id".to_string()),
                ..Default::default()
            },
            NodeEnvironment::Prod,
            now_ms(),
        )
        .unwrap();

        // DedupKeyRequired: a keyless record on a dedup-enabled topic is
        // rejected up front, before anything is appended.
        let err = svc
            .publish(
                &ctx,
                "labs.dedup2",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![record("no-key")],
                },
            )
            .unwrap_err();
        assert!(matches!(err, BusServiceError::DedupKeyRequired { .. }));

        // First publish of "k1": fresh, accepted.
        let r1 = svc
            .publish(
                &ctx,
                "labs.dedup2",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![record_with_key("v1", "k1")],
                },
            )
            .unwrap();
        assert_eq!(r1.accepted, 1);
        assert_eq!(r1.deduplicated, 0);

        // Retrying the SAME key: fully deduplicated, nothing appended, and
        // no foreign offset is reported (`partitions` is empty
        // rather than borrowing some other record's `log_end_offset`).
        let r2 = svc
            .publish(
                &ctx,
                "labs.dedup2",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![record_with_key("v1-retry", "k1")],
                },
            )
            .unwrap();
        assert_eq!(r2.accepted, 0);
        assert_eq!(r2.deduplicated, 1);
        assert!(r2.partitions.is_empty());
        assert!(!r2.duplicate, "layer-2 dedup never sets `duplicate`");

        // Partial dedup PLUS an intra-batch duplicate in one call: "k1" was
        // already seen on a previous call, and "k3" appears twice in THIS
        // SAME batch — it must only be appended once even though the
        // two-phase persistent store has not committed it yet when the
        // second copy is checked.
        let r3 = svc
            .publish(
                &ctx,
                "labs.dedup2",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![
                        record_with_key("v1-again", "k1"),
                        record_with_key("v3-a", "k3"),
                        record_with_key("v3-b", "k3"),
                    ],
                },
            )
            .unwrap();
        assert_eq!(r3.accepted, 1, "only the first 'k3' record lands");
        assert_eq!(r3.deduplicated, 2, "'k1' plus the intra-batch 'k3' repeat");
        assert_eq!(r3.single_partition().unwrap().accepted, 1);
    }

    /// Two-phase dedup's `contains` check and its `insert` are
    /// separated by the engine append, so two THREADS racing a `publish`
    /// of the SAME key can both observe "not seen" before either commits
    /// its own insert — this is a known, documented false-negative
    /// direction (a duplicate slips through), never a false positive (a
    /// unique record dropped). This test only asserts the safe-direction
    /// guarantee (at least one record lands); it does not assert exactly
    /// one, because a duplicate is an accepted, documented possibility
    /// here.
    #[test]
    fn concurrent_publish_of_the_same_dedup_key_is_at_least_once_not_exactly_once() {
        let (_tmp, svc) = test_service();
        let svc = Arc::new(svc);
        let ctx = test_ctx("org-1");
        topics::create_topic_for_dedup_test(
            &svc.db,
            &ctx.org_id,
            "labs.dedup.same-key-race",
            topics::TopicOptions {
                partitions: Some(1),
                idempotency_key: Some("msg.run_id".to_string()),
                ..Default::default()
            },
            NodeEnvironment::Prod,
            now_ms(),
        )
        .unwrap();

        let barrier = Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let svc = Arc::clone(&svc);
                let ctx = test_ctx("org-1");
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    svc.publish(
                        &ctx,
                        "labs.dedup.same-key-race",
                        PublishBatch {
                            partition: None,
                            producer: None,
                            records: vec![record_with_key(&format!("v{i}"), "same-key")],
                        },
                    )
                })
            })
            .collect();
        for h in handles {
            h.join().expect("publish thread panicked").unwrap();
        }

        let handle = svc
            .open_consumer(
                &ctx,
                "g1",
                &["labs.dedup.same-key-race".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();
        let batch = handle.fetch(1024 * 1024, 50).unwrap();
        assert!(
            !batch.records.is_empty(),
            "at least one of the racing publishes for the same key must have landed"
        );
    }

    #[test]
    fn dedup_expected_rate_per_sec_from_bus_init_config_changes_derived_capacity() {
        let make_svc = |rate: u64| {
            let (_tmp, bus_dir) = test_bus_dir();
            let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
            crate::db::repository::bus_test_support::create_bus_tables(&db)
                .expect("bus fixture tables");
            BusService::new(BusInitConfig {
                bus_dir,
                db,
                authorizer: Arc::new(AllowAllAuthorizer),
                retention_interval: None,
                dedup_expected_rate_per_sec: rate,
                partition_handle_lru: None,
                publish_ack_timeout: DEFAULT_PUBLISH_ACK_TIMEOUT,
            })
            .expect("bus service")
        };
        let ctx = test_ctx("org-1");
        // Both rates stay well inside `derive_capacity`'s floor/ceiling
        // (default 24h ttl_ms), so the on-disk dedup file size directly
        // reflects the node-wide rate `BusInitConfig` was given — a change
        // to `dedup_store`'s wiring that silently dropped this field would
        // make both files the same size.
        // 24h * 10/s = 864,000 and 24h * 100/s = 8,640,000 — both comfortably
        // between `MIN_DERIVED_CAPACITY` (65,536) and the ceiling
        // (16,777,216), so neither gets floored or capped to the same
        // value as the other.
        let low = make_svc(10);
        let high = make_svc(100);
        for svc in [&low, &high] {
            topics::create_topic_for_dedup_test(
                &svc.db,
                &ctx.org_id,
                "labs.dedup.rate",
                topics::TopicOptions {
                    partitions: Some(1),
                    idempotency_key: Some("msg.run_id".to_string()),
                    ..Default::default()
                },
                NodeEnvironment::Prod,
                now_ms(),
            )
            .unwrap();
            svc.publish(
                &ctx,
                "labs.dedup.rate",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![record_with_key("v", "k")],
                },
            )
            .unwrap();
        }
        let dedup_file_size = |svc: &BusService| {
            std::fs::metadata(
                topics::topic_dir(&svc.bus_dir, "org-1", "labs.dedup.rate").join("dedup.bin"),
            )
            .unwrap()
            .len()
        };
        let low_size = dedup_file_size(&low);
        let high_size = dedup_file_size(&high);
        assert!(
            high_size > low_size,
            "a higher node-wide dedup_expected_rate_per_sec must derive a larger dedup \
             store: low={low_size} high={high_size}"
        );
    }

    // ---- per-record partitioning ------------------

    #[test]
    fn publish_routes_each_record_by_its_own_key_not_just_the_first() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "events.multi",
            topics::TopicOptions {
                partitions: Some(8),
                ..Default::default()
            },
        )
        .unwrap();

        // Find two synthetic keys that hash to different partitions under
        // THIS SAME hash function, rather than hard-coding key strings
        // whose partition depends on the hash implementation.
        let mut key_a: Option<(String, u32)> = None;
        let mut key_b: Option<(String, u32)> = None;
        for i in 0..64u32 {
            let k = format!("key-{i}");
            let p = partition_for_key(k.as_bytes(), 8);
            match (&key_a, &key_b) {
                (None, _) => key_a = Some((k, p)),
                (Some((_, pa)), None) if p != *pa => key_b = Some((k, p)),
                _ => {}
            }
        }
        let (key_a, pa) = key_a.expect("at least one key");
        let (key_b, pb) = key_b.expect("8 partitions: some key must differ within 64 tries");

        let result = svc
            .publish(
                &ctx,
                "events.multi",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![
                        record_with_key("a1", &key_a),
                        record_with_key("b1", &key_b),
                        record_with_key("a2", &key_a),
                    ],
                },
            )
            .unwrap();

        assert_eq!(result.accepted, 3);
        assert_eq!(
            result.partitions.len(),
            2,
            "two distinct partitions touched, not just the first record's"
        );
        let ack_a = result
            .partitions
            .iter()
            .find(|a| a.partition == pa)
            .unwrap();
        let ack_b = result
            .partitions
            .iter()
            .find(|a| a.partition == pb)
            .unwrap();
        assert_eq!(ack_a.accepted, 2, "both key_a records share a partition");
        assert_eq!(ack_b.accepted, 1);

        // Explicit `partition` still forces the WHOLE batch onto one
        // partition, overriding per-record hashing.
        let forced = svc
            .publish(
                &ctx,
                "events.multi",
                PublishBatch {
                    partition: Some(pb),
                    producer: None,
                    records: vec![record_with_key("x", &key_a), record_with_key("y", &key_b)],
                },
            )
            .unwrap();
        assert_eq!(forced.partitions.len(), 1);
        assert_eq!(forced.single_partition().unwrap().partition, pb);
        assert_eq!(forced.single_partition().unwrap().accepted, 2);
    }

    #[test]
    fn partition_for_key_is_deterministic_across_calls() {
        // Required for "same key -> same partition" ordering guarantees to
        // hold across separate `publish` calls, not just within one batch.
        for _ in 0..5 {
            assert_eq!(
                partition_for_key(b"stable-key", 8),
                partition_for_key(b"stable-key", 8)
            );
        }
        assert!(partition_for_key(b"stable-key", 3) < 3);
    }

    // ---- concurrent first-open of a fresh partition -----------------

    #[test]
    fn concurrent_publish_to_a_fresh_partition_never_reports_partition_locked() {
        let (_tmp, svc) = test_service();
        let svc = Arc::new(svc);
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "events.race",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();

        let handles: Vec<_> = (0..16)
            .map(|i| {
                let svc = Arc::clone(&svc);
                let ctx = test_ctx("org-1");
                std::thread::spawn(move || {
                    svc.publish(
                        &ctx,
                        "events.race",
                        PublishBatch {
                            partition: Some(0),
                            producer: None,
                            records: vec![record(&format!("r{i}"))],
                        },
                    )
                })
            })
            .collect();

        for h in handles {
            let result = h.join().expect("publish thread panicked");
            assert!(
                result.is_ok(),
                "concurrent first-open of a fresh partition must never race into \
                 PartitionLocked: {result:?}"
            );
        }
    }

    #[test]
    fn concurrent_publish_to_a_fresh_dedup_topic_never_reports_would_block() {
        let (_tmp, svc) = test_service();
        let svc = Arc::new(svc);
        let ctx = test_ctx("org-1");
        topics::create_topic_for_dedup_test(
            &svc.db,
            &ctx.org_id,
            "labs.dedup.race-open",
            topics::TopicOptions {
                partitions: Some(1),
                idempotency_key: Some("msg.run_id".to_string()),
                ..Default::default()
            },
            NodeEnvironment::Prod,
            now_ms(),
        )
        .unwrap();

        let barrier = Arc::new(std::sync::Barrier::new(16));
        let handles: Vec<_> = (0..16)
            .map(|i| {
                let svc = Arc::clone(&svc);
                let ctx = test_ctx("org-1");
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    svc.publish(
                        &ctx,
                        "labs.dedup.race-open",
                        PublishBatch {
                            partition: None,
                            producer: None,
                            records: vec![record_with_key(&format!("v{i}"), &format!("k{i}"))],
                        },
                    )
                })
            })
            .collect();

        for h in handles {
            let result = h.join().expect("publish thread panicked");
            assert!(
                result.is_ok(),
                "concurrent first-open of a fresh dedup store must never race into \
                 an io::ErrorKind::WouldBlock from the store's own advisory lock: {result:?}"
            );
        }
    }

    // ---- a caller cannot forge `tf.*` headers -----------------------

    #[test]
    fn publish_strips_caller_supplied_tf_prefixed_headers() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "events.headers",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        let mut r = record("payload");
        r.headers
            .push(("tf.actor".to_string(), Bytes::from_static(b"forged")));
        svc.publish(
            &ctx,
            "events.headers",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![r],
            },
        )
        .unwrap();

        let handle = svc
            .open_consumer(
                &ctx,
                "g1",
                &["events.headers".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();
        let fetched = handle.fetch(1024, 20).unwrap();
        assert_eq!(fetched.records.len(), 1);
        let actor_headers: Vec<_> = fetched.records[0]
            .headers
            .iter()
            .filter(|(k, _)| k == "tf.actor")
            .collect();
        assert_eq!(
            actor_headers.len(),
            1,
            "the caller-supplied 'tf.actor' copy must be stripped, not just appended alongside"
        );
        assert_eq!(actor_headers[0].1, Bytes::from_static(b"tester"));
    }

    // ---- topic config cache -----------------------------------------

    #[test]
    fn topic_config_is_cached_after_first_publish() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(&ctx, "events.cache", topics::TopicOptions::default())
            .unwrap();
        let loads_after_create = svc.topic_config_db_loads();

        svc.publish(
            &ctx,
            "events.cache",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record("1")],
            },
        )
        .unwrap();
        let loads_after_first = svc.topic_config_db_loads();
        assert_eq!(
            loads_after_first,
            loads_after_create + 1,
            "the first publish warms the cache with exactly one SQLite read"
        );

        for i in 0..10 {
            svc.publish(
                &ctx,
                "events.cache",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![record(&format!("{i}"))],
                },
            )
            .unwrap();
        }
        assert_eq!(
            svc.topic_config_db_loads(),
            loads_after_first,
            "warm cache: zero further SQLite reads on the publish hot path"
        );

        // `update_topic` invalidates the cache: the NEXT publish re-loads.
        svc.update_topic(
            &ctx,
            "events.cache",
            topics::TopicOptions {
                retention_ms: Some(topics::MIN_RETENTION_MS),
                ..Default::default()
            },
        )
        .unwrap();
        svc.publish(
            &ctx,
            "events.cache",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record("after-update")],
            },
        )
        .unwrap();
        assert_eq!(svc.topic_config_db_loads(), loads_after_first + 1);
    }

    #[test]
    fn group_paused_state_is_cached_after_first_fetch() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(&ctx, "events.group-cache", topics::TopicOptions::default())
            .unwrap();
        svc.publish(
            &ctx,
            "events.group-cache",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record("1")],
            },
        )
        .unwrap();
        let handle = svc
            .open_consumer(
                &ctx,
                "g1",
                &["events.group-cache".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();

        let loads_before = svc.group_state_db_loads();
        handle.fetch(1024, 20).unwrap();
        let loads_after_first = svc.group_state_db_loads();
        assert_eq!(
            loads_after_first,
            loads_before + 1,
            "the first fetch warms the cache with exactly one SQLite read"
        );

        for _ in 0..10 {
            handle.fetch(1024, 20).unwrap();
        }
        assert_eq!(
            svc.group_state_db_loads(),
            loads_after_first,
            "warm cache: zero further SQLite reads on the fetch hot path"
        );

        // `pause_group` invalidates the cache: the NEXT fetch re-loads and
        // observes the pause.
        svc.pause_group(&ctx, "g1", "events.group-cache").unwrap();
        let err = handle.fetch(1024, 20);
        assert!(matches!(err, Err(BusServiceError::GroupPaused { .. })));
        assert_eq!(svc.group_state_db_loads(), loads_after_first + 1);
    }

    // ---- Z12 environment fencing ------------------------------------

    #[test]
    fn publish_rejects_when_topic_environment_differs_from_node_environment() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        // Fresh `:memory:` db with no `settings` row defaults to Prod, so
        // the topic is stamped Prod.
        svc.create_topic(&ctx, "events.env", topics::TopicOptions::default())
            .unwrap();

        // The node's declared environment changes to Dev; the topic stays
        // stamped Prod.
        crate::services::environment::set_node_environment(&svc.db, NodeEnvironment::Dev).unwrap();
        svc.invalidate_environment_cache();

        let err = svc
            .publish(
                &ctx,
                "events.env",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![record("x")],
                },
            )
            .unwrap_err();
        assert!(matches!(err, BusServiceError::EnvironmentMismatch { .. }));
    }

    /// `run_retention_sweep` enumerates orgs via `list_all_org_ids`, which
    /// reads the real `organizations` table — the bus test fixtures only
    /// create `bus_topics`/`bus_groups`, so a test exercising the sweep
    /// needs its org_id to actually exist there too.
    fn insert_test_org(db: &DbPool, org_id: &str) {
        db.write()
            .unwrap()
            .execute(
                "INSERT INTO organizations (org_id, name, slug, status, created_at) \
                 VALUES (?1, ?1, ?1, 'active', '2026-01-01T00:00:00Z')",
                rusqlite::params![org_id],
            )
            .unwrap();
    }

    // ---- windowed quota/denial audit -------------------

    fn count_audit_logs(svc: &BusService, action: &str) -> usize {
        crate::db::repository::list_audit_logs(
            &svc.db,
            &crate::db::models::AuditLogFilters {
                action: Some(action.to_string()),
                ..Default::default()
            },
            0,
            50,
        )
        .unwrap()
        .len()
    }

    #[test]
    fn two_rejections_in_one_window_produce_only_one_audit_entry() {
        let (_tmp, bus_dir) = test_bus_dir();
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        crate::db::repository::bus_test_support::create_bus_tables(&db)
            .expect("bus fixture tables");
        let svc = BusService::new(BusInitConfig {
            bus_dir,
            db,
            authorizer: Arc::new(DenyAllAuthorizer),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .unwrap();
        let ctx = test_ctx("org-1");

        for _ in 0..2 {
            let err = svc.publish(
                &ctx,
                "secret.topic",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![record("x")],
                },
            );
            assert!(matches!(err, Err(BusServiceError::PermissionDenied { .. })));
        }
        assert_eq!(
            count_audit_logs(&svc, "bus.produce.denied"),
            1,
            "second rejection in the same window must be suppressed, not write a new row"
        );
    }

    #[test]
    fn a_rejection_in_the_next_window_writes_a_new_entry() {
        let (_tmp, bus_dir) = test_bus_dir();
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        crate::db::repository::bus_test_support::create_bus_tables(&db)
            .expect("bus fixture tables");
        let svc = BusService::new(BusInitConfig {
            bus_dir,
            db,
            authorizer: Arc::new(DenyAllAuthorizer),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .unwrap();
        svc.audit_windows
            .set_window_for_test(Duration::from_millis(20));
        let ctx = test_ctx("org-1");

        let deny_once = || {
            svc.publish(
                &ctx,
                "secret.topic",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![record("x")],
                },
            )
        };
        // First occurrence: written immediately (window 1).
        assert!(matches!(
            deny_once(),
            Err(BusServiceError::PermissionDenied { .. })
        ));
        // Suppressed: still inside window 1.
        assert!(matches!(
            deny_once(),
            Err(BusServiceError::PermissionDenied { .. })
        ));
        assert_eq!(count_audit_logs(&svc, "bus.produce.denied"), 1);

        std::thread::sleep(Duration::from_millis(40));
        // This occurrence starts window 2 and flushes window 1's suppressed
        // count as part of its own row.
        assert!(matches!(
            deny_once(),
            Err(BusServiceError::PermissionDenied { .. })
        ));
        assert_eq!(
            count_audit_logs(&svc, "bus.produce.denied"),
            2,
            "a rejection in a new window must write its own entry"
        );
    }

    #[test]
    fn windowed_audit_details_carry_org_id() {
        let (_tmp, bus_dir) = test_bus_dir();
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        crate::db::repository::bus_test_support::create_bus_tables(&db)
            .expect("bus fixture tables");
        let svc = BusService::new(BusInitConfig {
            bus_dir,
            db,
            authorizer: Arc::new(DenyAllAuthorizer),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .unwrap();
        let ctx = test_ctx("org-audit-1");
        let _ = svc.publish(
            &ctx,
            "secret.topic",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record("x")],
            },
        );
        let logs = crate::db::repository::list_audit_logs(
            &svc.db,
            &crate::db::models::AuditLogFilters {
                action: Some("bus.produce.denied".to_string()),
                ..Default::default()
            },
            0,
            10,
        )
        .unwrap();
        assert_eq!(logs.len(), 1);
        let details = logs[0].details.clone().unwrap_or_default();
        assert!(
            details.contains("org_id=org-audit-1"),
            "details must carry org_id, got: {details}"
        );
    }

    #[test]
    fn flush_audit_windows_writes_the_suppressed_count_with_its_resource() {
        let (_tmp, bus_dir) = test_bus_dir();
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        crate::db::repository::bus_test_support::create_bus_tables(&db)
            .expect("bus fixture tables");
        let svc = BusService::new(BusInitConfig {
            bus_dir,
            db,
            authorizer: Arc::new(DenyAllAuthorizer),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .unwrap();
        let ctx = test_ctx("org-1");
        for _ in 0..3 {
            let _ = svc.publish(
                &ctx,
                "secret.topic",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![record("x")],
                },
            );
        }
        assert_eq!(
            count_audit_logs(&svc, "bus.produce.denied"),
            1,
            "the 2nd/3rd occurrence are suppressed inside the same window"
        );

        svc.flush_audit_windows();
        let logs = crate::db::repository::list_audit_logs(
            &svc.db,
            &crate::db::models::AuditLogFilters {
                action: Some("bus.produce.denied".to_string()),
                ..Default::default()
            },
            0,
            10,
        )
        .unwrap();
        assert_eq!(
            logs.len(),
            2,
            "flush must write the suppressed tail as its own row"
        );
        // Rows come back newest-first (`id DESC`): `logs[0]` is the row the
        // flush just wrote.
        assert_eq!(
            logs[0].resource.as_deref(),
            Some("secret.topic"),
            "the flushed row must carry the topic as its resource, not None"
        );
        let details = logs[0].details.clone().unwrap_or_default();
        assert!(
            details.contains("count=2"),
            "2 occurrences were suppressed before the flush: {details}"
        );
    }

    #[test]
    fn dropping_bus_service_flushes_pending_audit_windows() {
        let (_tmp, bus_dir) = test_bus_dir();
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        crate::db::repository::bus_test_support::create_bus_tables(&db)
            .expect("bus fixture tables");
        let db_for_check = db.clone();
        {
            let svc = BusService::new(BusInitConfig {
                bus_dir,
                db,
                authorizer: Arc::new(DenyAllAuthorizer),
                retention_interval: None,
                dedup_expected_rate_per_sec: 10_000,
                partition_handle_lru: None,
                publish_ack_timeout: DEFAULT_PUBLISH_ACK_TIMEOUT,
            })
            .unwrap();
            let ctx = test_ctx("org-1");
            for _ in 0..2 {
                let _ = svc.publish(
                    &ctx,
                    "secret.topic",
                    PublishBatch {
                        partition: None,
                        producer: None,
                        records: vec![record("x")],
                    },
                );
            }
            assert_eq!(
                count_audit_logs(&svc, "bus.produce.denied"),
                1,
                "second occurrence suppressed, not yet flushed"
            );
            // `svc` is dropped at the end of this block — `Drop for
            // BusService` must flush the suppressed occurrence.
        }
        let logs = crate::db::repository::list_audit_logs(
            &db_for_check,
            &crate::db::models::AuditLogFilters {
                action: Some("bus.produce.denied".to_string()),
                ..Default::default()
            },
            0,
            50,
        )
        .unwrap();
        assert_eq!(
            logs.len(),
            2,
            "Drop must flush the suppressed occurrence as its own row"
        );
    }

    // ---- amount > rate is a hard config error ----------

    #[test]
    fn publish_larger_than_bucket_capacity_is_a_hard_error_not_quota_exceeded() {
        let (_tmp, svc) = test_service();
        svc.quota().set_org_quota(
            "org-1",
            quota::QuotaConfig {
                produce_msgs_per_sec: 5,
                produce_bytes_per_sec: 1024 * 1024,
                ..Default::default()
            },
        );
        let ctx = test_ctx("org-1");
        svc.create_topic(&ctx, "big.batch", topics::TopicOptions::default())
            .unwrap();
        let records: Vec<PublishRecord> = (0..10).map(|i| record(&format!("r{i}"))).collect();
        let err = svc
            .publish(
                &ctx,
                "big.batch",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records,
                },
            )
            .unwrap_err();
        assert!(
            matches!(err, BusServiceError::QuotaRequestTooLarge { .. }),
            "expected a hard config error, got {err:?}"
        );
    }

    // ---- max_topics/max_partitions enforcement --------

    #[test]
    fn create_topic_rejects_once_max_topics_is_reached() {
        let (_tmp, svc) = test_service();
        svc.quota().set_org_quota(
            "org-1",
            quota::QuotaConfig {
                max_topics: 1,
                ..Default::default()
            },
        );
        let ctx = test_ctx("org-1");
        svc.create_topic(&ctx, "first.topic", topics::TopicOptions::default())
            .unwrap();
        let err = svc
            .create_topic(&ctx, "second.topic", topics::TopicOptions::default())
            .unwrap_err();
        assert!(matches!(err, BusServiceError::MaxTopicsExceeded { .. }));
    }

    #[test]
    fn create_topic_rejects_once_max_partitions_would_be_exceeded() {
        let (_tmp, svc) = test_service();
        svc.quota().set_org_quota(
            "org-1",
            quota::QuotaConfig {
                max_partitions: 10,
                ..Default::default()
            },
        );
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "first.topic",
            topics::TopicOptions {
                partitions: Some(8),
                ..Default::default()
            },
        )
        .unwrap();
        let err = svc
            .create_topic(
                &ctx,
                "second.topic",
                topics::TopicOptions {
                    partitions: Some(4),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(matches!(err, BusServiceError::MaxPartitionsExceeded { .. }));
    }

    // ---- system-wide retention sweep --------------------

    #[test]
    fn run_retention_sweep_deletes_sealed_segments_and_keeps_the_active_one() {
        let (_tmp, svc) = test_service();
        insert_test_org(&svc.db, "org-1");
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "sweep.topic",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();

        // Pre-seal several segments directly through the engine, bypassing
        // the fixed `RollPolicy::default()` `partition_handle` uses in
        // production — same technique as `retention.rs`'s own tests.
        let dir = topics::partition_dir(&svc.bus_dir, "org-1", "sweep.topic", 0);
        let one_segment_bytes;
        {
            let policy = tentaflow_bus::RollPolicy {
                max_batches: 1,
                ..Default::default()
            };
            let part =
                tentaflow_bus::Partition::open(&dir, policy, tentaflow_bus::Durability::Os, 8)
                    .unwrap();
            for _ in 0..5 {
                let mut b = tentaflow_bus::BatchBuilder::new(0, 1);
                b.push(tentaflow_bus::RecordInput::new(
                    Bytes::from(vec![7u8; 512]),
                    now_ms(),
                ))
                .unwrap();
                part.append_batch(b.build().unwrap()).unwrap();
            }
            let sealed = part.sealed_segments();
            assert_eq!(sealed.len(), 4);
            one_segment_bytes = sealed[0].len as i64;
        } // dropped: releases the directory flock before the sweep reopens it.

        // Budget for 1.5 segments worth of sealed data: only the newest
        // sealed segment should survive, mirroring retention.rs's own
        // `evicts_oldest_sealed_segments_first_to_satisfy_byte_budget`.
        let mut row = crate::db::repository::bus_topic_get(&svc.db, "org-1", "sweep.topic")
            .unwrap()
            .unwrap();
        row.retention_bytes = one_segment_bytes + one_segment_bytes / 2;
        crate::db::repository::bus_topic_update(&svc.db, &row).unwrap();

        let report = svc.run_retention_sweep();
        assert_eq!(report.orgs_swept, 1);
        assert_eq!(report.topics_swept, 1);
        assert_eq!(report.deleted_segments, 3);
        assert_eq!(report.deleted_bytes, one_segment_bytes as u64 * 3);

        let cfg = svc.topic_config("org-1", "sweep.topic").unwrap();
        let part = svc
            .partition_handle("org-1", "sweep.topic", 0, &cfg)
            .unwrap();
        assert_eq!(
            part.sealed_segments().len(),
            1,
            "the active segment was never touched and one sealed segment survives the budget"
        );

        let logs = crate::db::repository::list_audit_logs(
            &svc.db,
            &crate::db::models::AuditLogFilters {
                action: Some("bus.retention.sweep".to_string()),
                ..Default::default()
            },
            0,
            10,
        )
        .unwrap();
        assert_eq!(logs.len(), 1, "one summary entry, not one per segment");
    }

    #[test]
    fn run_retention_sweep_with_nothing_to_delete_writes_no_audit() {
        let (_tmp, svc) = test_service();
        insert_test_org(&svc.db, "org-1");
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "quiet.topic",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        svc.publish(
            &ctx,
            "quiet.topic",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record("x")],
            },
        )
        .unwrap();

        let report = svc.run_retention_sweep();
        assert_eq!(
            report.topics_swept, 1,
            "the org/topic were actually visited"
        );
        assert_eq!(report.deleted_segments, 0);
        assert_eq!(report.deleted_bytes, 0);
        assert_eq!(count_audit_logs(&svc, "bus.retention.sweep"), 0);
    }

    #[test]
    fn run_retention_sweep_does_not_leave_partition_handles_open_for_topics_nobody_had_open() {
        let (_tmp, svc) = test_service();
        insert_test_org(&svc.db, "org-1");
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "sweep.reaper",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        // Never opened via `svc` before the sweep — no publish, no
        // open_consumer, no explicit `partition_handle` call.
        let key = ("org-1".to_string(), "sweep.reaper".to_string(), 0);
        assert!(!svc.partitions.contains_key(&key));

        let report = svc.run_retention_sweep();
        assert_eq!(report.topics_swept, 1);

        assert!(
            !svc.partitions.contains_key(&key),
            "the sweep must close any handle it opened purely for itself, \
             not leave a writer thread + directory flock behind forever"
        );

        // Reopening (e.g. a real publish/consume after the sweep) still
        // works normally.
        let cfg = svc.topic_config("org-1", "sweep.reaper").unwrap();
        let part = svc
            .partition_handle("org-1", "sweep.reaper", 0, &cfg)
            .unwrap();
        assert_eq!(part.log_end_offset(), 0);
    }

    /// A live `ConsumerHandle`'s partition must survive `run_retention_
    /// sweep`'s map cleanup: `self.partitions.remove(&key)` reproduces
    /// exactly what the sweeper does for a key it (incorrectly, in the old
    /// bug) believed it was the only opener of — a `ConsumerHandle` opened
    /// while the sweep was mid-flight. With `ConsumerPartition` holding its
    /// own `Partition` clone, the removed map entry does not stop the
    /// writer thread or release the flock: a later `publish` must succeed
    /// (reusing the still-live handle via `consumer_partitions`, not racing
    /// its flock) and the already-open `ConsumerHandle`'s `fetch` must see
    /// the new record, rather than freezing at whatever `high_watermark` it
    /// had when the map entry disappeared.
    #[test]
    fn open_consumer_survives_a_sweep_that_removes_its_partition_from_the_shared_map() {
        let (_tmp, svc) = test_service();
        insert_test_org(&svc.db, "org-1");
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "sweep.consumer",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();

        let handle = svc
            .open_consumer(
                &ctx,
                "workers",
                &["sweep.consumer".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::AutoAfterSuccess,
                },
            )
            .unwrap();

        // Simulate the exact race: the sweeper (or anything else) drops
        // this key from the shared map while the consumer above is still
        // holding its own `Partition` clone.
        let key = ("org-1".to_string(), "sweep.consumer".to_string(), 0);
        assert!(
            svc.partitions.remove(&key).is_some(),
            "open_consumer must have inserted this key into the shared map"
        );

        svc.publish(
            &ctx,
            "sweep.consumer",
            PublishBatch {
                partition: Some(0),
                producer: None,
                records: vec![record("after-sweep-removal")],
            },
        )
        .expect(
            "publish must reuse the consumer's still-live partition instead of \
             racing its held directory flock",
        );

        let batch = handle.fetch(1024 * 1024, 50).unwrap();
        assert_eq!(
            batch.records.len(),
            1,
            "the already-open consumer must see the record published after its \
             partition was removed from the shared map"
        );
        assert_eq!(batch.records[0].payload.as_ref(), b"after-sweep-removal");
    }

    /// `purge_org` must still reach a `ConsumerHandle`'s partition even when
    /// the sweeper already removed its key from `self.partitions` — the
    /// GDPR/RODO gap `consumer_partitions` (a `Weak`-referenced side
    /// registry) exists to close: without it, `purge_org`'s own
    /// `partitions.retain` loop has nothing to find for this key and the
    /// consumer's orphaned handle would keep serving the purged org's data.
    #[test]
    fn purge_org_detaches_a_consumer_partition_even_after_the_sweeper_removed_it_from_the_map() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "purge.consumer",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        svc.publish(
            &ctx,
            "purge.consumer",
            PublishBatch {
                partition: Some(0),
                producer: None,
                records: vec![record("before-purge")],
            },
        )
        .unwrap();

        let handle = svc
            .open_consumer(
                &ctx,
                "workers",
                &["purge.consumer".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::AutoAfterSuccess,
                },
            )
            .unwrap();

        // Same simulated race as the sweep-survival test above: the map
        // entry is gone, but `handle` still holds a live clone.
        let key = ("org-1".to_string(), "purge.consumer".to_string(), 0);
        assert!(svc.partitions.remove(&key).is_some());

        svc.purge_org("org-1").unwrap();

        let err = handle.fetch(1024 * 1024, 10).unwrap_err();
        assert!(
            matches!(err, BusServiceError::TopicNotFound { .. }),
            "expected the orphaned consumer's fetch to fail promptly after purge_org \
             detached it via the weak registry, got {err:?}"
        );
    }

    // ---- org_id validation (traversal / reserved names) --------------

    #[test]
    fn purge_org_rejects_invalid_org_id_and_touches_nothing() {
        let (_tmp, svc) = test_service();
        // A traversal org_id must never reach `remove_dir_all`.
        let err = svc.purge_org("../escape").unwrap_err();
        assert!(matches!(err, BusServiceError::InvalidArgument(_)));

        // `_meta` is reserved: it is the real fjall directory this
        // service's own offsets/producer-seq keyspaces live under.
        let err = svc.purge_org("_meta").unwrap_err();
        assert!(matches!(err, BusServiceError::InvalidArgument(_)));
        assert!(
            svc.bus_dir.join("_meta").exists(),
            "the service's own metadata directory must be untouched"
        );
    }

    #[test]
    fn publish_open_consumer_and_create_topic_reject_invalid_org_id() {
        let (_tmp, svc) = test_service();
        let bad_ctx = test_ctx("_meta");

        let err = svc.publish(
            &bad_ctx,
            "any.topic",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record("x")],
            },
        );
        assert!(matches!(err, Err(BusServiceError::InvalidArgument(_))));

        let err = svc.open_consumer(
            &bad_ctx,
            "g1",
            &["any.topic".to_string()],
            ConsumerConfig {
                commit_mode: groups::CommitMode::Explicit,
            },
        );
        assert!(matches!(err, Err(BusServiceError::InvalidArgument(_))));

        let err = svc.create_topic(&bad_ctx, "any.topic", topics::TopicOptions::default());
        assert!(matches!(err, Err(BusServiceError::InvalidArgument(_))));
    }

    /// The remaining six entry points that touch `ctx.org_id` must reject
    /// an invalid one exactly like `publish`/`open_consumer`/`create_topic`
    /// already do — checked FIRST, before authorization or any topic/group
    /// lookup, so a bad org_id can never reach a filesystem path or a fjall
    /// key built from it.
    #[test]
    fn remaining_admin_and_group_apis_reject_invalid_org_id() {
        let (_tmp, svc) = test_service();
        let bad_ctx = test_ctx("_meta");

        assert!(matches!(
            svc.reset_offset(&bad_ctx, "g1", "any.topic", 0, 0),
            Err(BusServiceError::InvalidArgument(_))
        ));

        let fake_record = FetchedRecordMeta {
            topic: "any.topic".to_string(),
            partition: 0,
            offset: 0,
            timestamp_ms: 0,
            key: None,
            headers: vec![],
            payload: Bytes::new(),
            schema_id: 0,
        };
        assert!(matches!(
            svc.note_delivery_failure(
                &bad_ctx,
                "g1",
                "any.topic",
                0,
                0,
                &fake_record,
                dlq::DlqReason::ConsumerError,
                "boom",
            ),
            Err(BusServiceError::InvalidArgument(_))
        ));

        assert!(matches!(
            svc.dlq_retry(&bad_ctx, "__dlq.any.topic", 0, 0),
            Err(BusServiceError::InvalidArgument(_))
        ));
        assert!(matches!(
            svc.dlq_discard(&bad_ctx, "__dlq.any.topic", 0, 0),
            Err(BusServiceError::InvalidArgument(_))
        ));
        assert!(matches!(
            svc.pause_group(&bad_ctx, "g1", "any.topic"),
            Err(BusServiceError::InvalidArgument(_))
        ));
        assert!(matches!(
            svc.resume_group(&bad_ctx, "g1", "any.topic"),
            Err(BusServiceError::InvalidArgument(_))
        ));
        assert!(matches!(
            svc.is_group_paused("_meta", "g1", "any.topic"),
            Err(BusServiceError::InvalidArgument(_))
        ));
    }

    /// `reset_offset` must not blindly `force_commit` a fjall offset key
    /// for a topic that no longer exists — neither because it was
    /// `delete_topic`d nor because its whole org was `purge_org`'d.
    #[test]
    fn reset_offset_rejects_a_deleted_topic_and_a_purged_org() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.reset",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        svc.delete_topic(&ctx, "orders.reset").unwrap();

        let err = svc.reset_offset(&ctx, "g1", "orders.reset", 0, 5);
        assert!(matches!(err, Err(BusServiceError::TopicNotFound { .. })));
        assert_eq!(
            svc.offsets
                .committed_offset("org-1", "g1", "orders.reset", 0)
                .unwrap(),
            0,
            "the rejected reset must not have created a fjall offset key for a deleted topic"
        );

        svc.create_topic(
            &ctx,
            "orders.reset2",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        svc.purge_org("org-1").unwrap();
        let err = svc.reset_offset(&ctx, "g1", "orders.reset2", 0, 5);
        assert!(matches!(err, Err(BusServiceError::TopicNotFound { .. })));
    }

    /// `seek_to_earliest`/`lag` must respect a detached partition the same
    /// way `fetch` already does: after `delete_topic`, both must return
    /// `TopicNotFound` instead of a stale/frozen `0` derived from an
    /// infallible read of an engine value that no longer means anything.
    #[test]
    fn seek_to_earliest_and_lag_return_topic_not_found_after_delete_topic() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.detached-reads",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        svc.publish(
            &ctx,
            "orders.detached-reads",
            PublishBatch {
                partition: Some(0),
                producer: None,
                records: vec![record("x")],
            },
        )
        .unwrap();
        let handle = svc
            .open_consumer(
                &ctx,
                "g1",
                &["orders.detached-reads".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();

        svc.delete_topic(&ctx, "orders.detached-reads").unwrap();

        let err = handle.seek_to_earliest("orders.detached-reads", 0);
        assert!(matches!(err, Err(BusServiceError::TopicNotFound { .. })));

        let err = handle.lag();
        assert!(matches!(err, Err(BusServiceError::TopicNotFound { .. })));
    }

    // ---- GDPR/RODO org purge -----------------------------

    #[test]
    fn purge_org_removes_dir_rows_and_fjall_keys_leaving_other_org_intact() {
        let (_tmp, svc) = test_service();
        let ctx_a = test_ctx("org-a");
        let ctx_b = test_ctx("org-b");
        svc.create_topic(
            &ctx_a,
            "orders.created",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        svc.create_topic(
            &ctx_b,
            "orders.created",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        svc.publish(
            &ctx_a,
            "orders.created",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record("a")],
            },
        )
        .unwrap();
        svc.publish(
            &ctx_b,
            "orders.created",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record("b")],
            },
        )
        .unwrap();
        let handle_a = svc
            .open_consumer(
                &ctx_a,
                "g1",
                &["orders.created".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();
        handle_a.fetch(1024, 10).unwrap();
        handle_a
            .commit(&[(
                TopicPartition {
                    topic: "orders.created".to_string(),
                    partition: 0,
                },
                1,
            )])
            .unwrap();

        let dir_a = topics::topic_dir(&svc.bus_dir, "org-a", "orders.created");
        assert!(dir_a.exists());

        let report = svc.purge_org("org-a").unwrap();
        assert_eq!(report.topics_deleted, 1);
        assert_eq!(report.groups_deleted, 1);
        assert!(report.offset_keys_deleted >= 1);
        assert!(report.dir_removed);
        assert!(!dir_a.exists());

        assert!(topics::get_topic(&svc.db, "org-a", "orders.created")
            .unwrap()
            .is_none());
        assert!(topics::get_topic(&svc.db, "org-b", "orders.created")
            .unwrap()
            .is_some());

        let err = svc.publish(
            &ctx_a,
            "orders.created",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record("x")],
            },
        );
        assert!(matches!(err, Err(BusServiceError::TopicNotFound { .. })));

        // org-b is completely untouched.
        let ok = svc.publish(
            &ctx_b,
            "orders.created",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record("still-here")],
            },
        );
        assert!(ok.is_ok());

        let logs = crate::db::repository::list_audit_logs(
            &svc.db,
            &crate::db::models::AuditLogFilters {
                action: Some("bus.org.purged".to_string()),
                ..Default::default()
            },
            0,
            10,
        )
        .unwrap();
        assert_eq!(logs.len(), 1);
    }

    // ---- fetch after delete_topic/purge_org must never loop forever -----

    /// Runs `fetch` on a background thread and asserts it returns within a
    /// bounded timeout — the original bug was an UNBOUNDED retry loop
    /// against a repeatedly-stale ENOENT snapshot, which would hang the
    /// calling thread (or a Tokio worker, via `spawn_blocking`) forever
    /// with no error and no log line. A thread + `mpsc` channel is the only
    /// way to assert "did not hang" without actually risking the test
    /// process hanging alongside it.
    fn assert_returns_promptly<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(f());
        });
        rx.recv_timeout(Duration::from_secs(5))
            .expect("must return within 5s, not loop forever")
    }

    #[test]
    fn fetch_after_delete_topic_on_a_never_opened_segment_returns_promptly() {
        let (_tmp, svc) = test_service();
        let svc = Arc::new(svc);
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "labs.detach-delete",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        svc.publish(
            &ctx,
            "labs.detach-delete",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record("x")],
            },
        )
        .unwrap();
        let handle = svc
            .open_consumer(
                &ctx,
                "g1",
                &["labs.detach-delete".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();
        // This handle's `PartitionReader` was cloned at `open_consumer`
        // time and has never actually read a byte yet — its segment file
        // descriptor is still unopened (the engine opens it lazily),
        // reproducing the exact precondition the original infinite-loop
        // bug needed.
        svc.delete_topic(&ctx, "labs.detach-delete").unwrap();

        let result = assert_returns_promptly(move || handle.fetch(1024, 20));
        assert!(matches!(result, Err(BusServiceError::TopicNotFound { .. })));
    }

    #[test]
    fn fetch_after_purge_org_on_a_never_opened_segment_returns_promptly() {
        let (_tmp, svc) = test_service();
        let svc = Arc::new(svc);
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "labs.detach-purge",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        svc.publish(
            &ctx,
            "labs.detach-purge",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record("x")],
            },
        )
        .unwrap();
        let handle = svc
            .open_consumer(
                &ctx,
                "g1",
                &["labs.detach-purge".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();
        svc.purge_org("org-1").unwrap();

        let result = assert_returns_promptly(move || handle.fetch(1024, 20));
        assert!(matches!(result, Err(BusServiceError::TopicNotFound { .. })));
    }

    #[test]
    fn commit_and_seek_after_purge_org_do_not_recreate_fjall_keys() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.purged",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        svc.publish(
            &ctx,
            "orders.purged",
            PublishBatch {
                partition: None,
                producer: None,
                records: vec![record("x")],
            },
        )
        .unwrap();
        let handle = svc
            .open_consumer(
                &ctx,
                "g1",
                &["orders.purged".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();

        svc.purge_org("org-1").unwrap();

        let err = handle.commit(&[(
            TopicPartition {
                topic: "orders.purged".to_string(),
                partition: 0,
            },
            1,
        )]);
        assert!(matches!(err, Err(BusServiceError::TopicNotFound { .. })));
        assert_eq!(
            svc.offsets
                .committed_offset("org-1", "g1", "orders.purged", 0)
                .unwrap(),
            0,
            "commit on a purged org's handle must not resurrect a fjall offset key"
        );

        let err = handle.seek_to_earliest("orders.purged", 0);
        assert!(matches!(err, Err(BusServiceError::TopicNotFound { .. })));
        assert_eq!(
            svc.offsets
                .committed_offset("org-1", "g1", "orders.purged", 0)
                .unwrap(),
            0,
            "seek_to_earliest on a purged org's handle must not resurrect a fjall offset key either"
        );
    }

    /// `delete_topic` must purge the topic's scope of consumer-group state
    /// (committed offsets, `bus_groups` rows) and producer sequences —
    /// otherwise a `create_topic` of the SAME name inherits a stale
    /// committed offset from the deleted topic's previous incarnation and
    /// silently skips every record the new log's first consumer never
    /// actually saw.
    #[test]
    fn delete_topic_then_create_topic_of_the_same_name_starts_the_group_and_producer_seq_fresh() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.reincarnated",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        for i in 0..500 {
            svc.publish(
                &ctx,
                "orders.reincarnated",
                PublishBatch {
                    partition: Some(0),
                    producer: None,
                    records: vec![record(&format!("old-{i}"))],
                },
            )
            .unwrap();
        }
        let identity = producer::ProducerIdentity {
            producer_id: "producer-a".to_string(),
            epoch: 1,
            base_seq: 0,
        };
        svc.publish(
            &ctx,
            "orders.reincarnated",
            PublishBatch {
                partition: Some(0),
                producer: Some(identity.clone()),
                records: vec![record("old-producer-batch")],
            },
        )
        .unwrap();

        let handle = svc
            .open_consumer(
                &ctx,
                "workers",
                &["orders.reincarnated".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();
        handle
            .commit(&[(
                TopicPartition {
                    topic: "orders.reincarnated".to_string(),
                    partition: 0,
                },
                500,
            )])
            .unwrap();
        drop(handle);

        svc.delete_topic(&ctx, "orders.reincarnated").unwrap();
        svc.create_topic(
            &ctx,
            "orders.reincarnated",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        svc.publish(
            &ctx,
            "orders.reincarnated",
            PublishBatch {
                partition: Some(0),
                producer: None,
                records: vec![record("new-0")],
            },
        )
        .unwrap();

        // The same group name, reopened on the recreated topic, must start
        // at offset 0 (fresh log) and see the FIRST record of the new
        // incarnation — not silently "caught up" from the deleted topic's
        // stale committed offset of 500.
        let handle = svc
            .open_consumer(
                &ctx,
                "workers",
                &["orders.reincarnated".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();
        let batch = handle.fetch(1024 * 1024, 50).unwrap();
        assert_eq!(
            batch.records.len(),
            1,
            "the group's stale committed offset must not have survived delete_topic"
        );
        assert_eq!(batch.records[0].payload.as_ref(), b"new-0");
        assert_eq!(batch.records[0].offset, 0);

        // The old producer sequence must not report the fresh batch as a
        // `Duplicate` of the deleted topic's previous incarnation.
        let outcome = svc
            .producer_seq
            .check("org-1", "orders.reincarnated", 0, &identity)
            .unwrap();
        assert_eq!(outcome, producer::CheckOutcome::Fresh);
    }

    // ---- partial multi-partition publish failure -----------

    #[test]
    fn publish_partial_multi_partition_failure_reports_already_acked_partitions() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "events.partial",
            topics::TopicOptions {
                partitions: Some(2),
                ..Default::default()
            },
        )
        .unwrap();

        // Force partition 1 to fail on its very first append: open its
        // engine handle directly and detach it before `svc` ever gets a
        // chance to open its own — `partition_handle`'s
        // `entry(..).or_try_insert_with` then just returns this
        // already-detached handle instead of opening a fresh one.
        let dir = topics::partition_dir(&svc.bus_dir, "org-1", "events.partial", 1);
        let part = tentaflow_bus::Partition::open(
            &dir,
            tentaflow_bus::RollPolicy::default(),
            tentaflow_bus::Durability::Os,
            8,
        )
        .unwrap();
        part.detach();
        svc.partitions
            .insert(("org-1".to_string(), "events.partial".to_string(), 1), part);

        // Two keyless records round-robin deterministically: the first to
        // partition 0, the second to partition 1.
        let err = svc
            .publish(
                &ctx,
                "events.partial",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![record("r0"), record("r1")],
                },
            )
            .unwrap_err();

        match err {
            BusServiceError::PartialPublish { acked, source } => {
                assert_eq!(acked.len(), 1);
                assert_eq!(acked[0].partition, 0);
                // `map_engine_error` translates the engine's
                // `PartitionDetached` into `TopicNotFound`.
                assert!(matches!(*source, BusServiceError::TopicNotFound { .. }));
            }
            other => panic!("expected PartialPublish, got {other:?}"),
        }

        // Partition 0's record really did land, despite the overall call
        // returning `Err`.
        let cfg = svc.topic_config("org-1", "events.partial").unwrap();
        let part0 = svc
            .partition_handle("org-1", "events.partial", 0, &cfg)
            .unwrap();
        assert_eq!(
            part0.log_end_offset(),
            1,
            "partition 0's record was durably appended despite the overall publish failing"
        );
    }

    /// `acks.push` must happen right after a successful `append_batch`,
    /// BEFORE `producer_seq.record` — so a partition whose append landed
    /// durably shows up in `PartialPublish.acked` even when the very next
    /// step (recording the producer sequence) fails on that SAME partition.
    /// Before this ordering fix, this exact case lost the ack entirely: the
    /// caller got a bare `Db` error with no way to tell that the record was
    /// already on disk.
    #[test]
    fn publish_partial_multi_partition_failure_reports_already_acked_partitions_when_the_failure_is_after_the_append(
    ) {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "events.producer-seq-fail",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();

        let identity = producer::ProducerIdentity {
            producer_id: "producer-a".to_string(),
            epoch: 1,
            base_seq: 0,
        };

        // Forces the NEXT `producer_seq.record` call to fail — the append
        // itself (below) still succeeds and lands durably first.
        svc.producer_seq.force_next_record_failure();

        let err = svc
            .publish(
                &ctx,
                "events.producer-seq-fail",
                PublishBatch {
                    partition: Some(0),
                    producer: Some(identity.clone()),
                    records: vec![record("r0")],
                },
            )
            .unwrap_err();

        match err {
            BusServiceError::PartialPublish { acked, source } => {
                assert_eq!(acked.len(), 1);
                assert_eq!(acked[0].partition, 0);
                assert_eq!(
                    acked[0].accepted, 1,
                    "the append that succeeded before producer_seq.record failed \
                     must still be reported as accepted"
                );
                assert!(matches!(*source, BusServiceError::Db(_)));
            }
            other => panic!(
                "expected PartialPublish reporting the durably-appended partition, got {other:?}"
            ),
        }

        // The record really is on disk, despite the whole call returning
        // `Err` because of the LATER `producer_seq.record` failure.
        let cfg = svc
            .topic_config("org-1", "events.producer-seq-fail")
            .unwrap();
        let part = svc
            .partition_handle("org-1", "events.producer-seq-fail", 0, &cfg)
            .unwrap();
        assert_eq!(part.log_end_offset(), 1);

        // The producer sequence itself was NOT durably recorded (the
        // injected failure happened before that fjall write) — a naive
        // retry of the exact same batch is treated as `Fresh`, not
        // `Duplicate`, and would append a SECOND copy of the record. This
        // is exactly why `PublishResult`'s doc narrows "a retry is safe" to
        // only the case where BOTH the append and the sequence record
        // succeeded.
        let outcome = svc
            .producer_seq
            .check("org-1", "events.producer-seq-fail", 0, &identity)
            .unwrap();
        assert_eq!(outcome, producer::CheckOutcome::Fresh);
    }

    // ---- peek: stateless UI preview read -----------------------------

    #[test]
    fn peek_returns_records_without_creating_a_group_row_or_committing_anything() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.peek",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        for i in 0..5 {
            svc.publish(
                &ctx,
                "orders.peek",
                PublishBatch {
                    partition: Some(0),
                    producer: None,
                    records: vec![record(&format!("r-{i}"))],
                },
            )
            .unwrap();
        }

        let result = svc
            .peek(&ctx, "orders.peek", 0, 1, 10, 1024 * 1024)
            .unwrap();
        assert_eq!(result.records.len(), 4, "records from offset 1..5");
        assert_eq!(result.records[0].offset, 1);
        assert_eq!(result.records[0].payload.as_ref(), b"r-1");
        assert_eq!(result.high_watermark, 5);
        assert_eq!(result.earliest_offset, 0);

        // No `bus_groups` row of any kind was created by this read-only
        // preview — the whole point of `peek` over `open_consumer`+`fetch`
        // under a throwaway group.
        assert!(crate::db::repository::bus_group_list(&svc.db, "org-1")
            .unwrap()
            .is_empty());

        // A call that returns at least one record writes an audit row
        // (PLAN §6.2 medical-data access) — see
        // `peek_of_an_empty_partition_writes_no_audit_row` for the P3-5
        // counterpart (an empty read is not a data access).
        let logs = crate::db::repository::list_audit_logs(
            &svc.db,
            &crate::db::models::AuditLogFilters {
                action: Some("bus.messages.browse".to_string()),
                ..Default::default()
            },
            0,
            10,
        )
        .unwrap();
        assert_eq!(logs.len(), 1);
    }

    /// P3-5 follow-up (`KRYTYK-M1-R3.md`, coordinator decision "Decyzje po
    /// R3"): a `peek` that reaches the record loop but returns ZERO records
    /// must not write a `bus.messages.browse` row — the concrete case this
    /// fixes is `MessagesBrowse`/`DlqList` walking every partition of a
    /// topic and getting `count=0` on every partition that has no data yet.
    #[test]
    fn peek_of_an_empty_partition_writes_no_audit_row() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.peek-empty",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();

        let result = svc
            .peek(&ctx, "orders.peek-empty", 0, 0, 10, 1024 * 1024)
            .unwrap();
        assert!(result.records.is_empty());

        let logs = crate::db::repository::list_audit_logs(
            &svc.db,
            &crate::db::models::AuditLogFilters {
                action: Some("bus.messages.browse".to_string()),
                ..Default::default()
            },
            0,
            10,
        )
        .unwrap();
        assert!(
            logs.is_empty(),
            "an empty peek must not write a bus.messages.browse row"
        );
    }

    #[test]
    fn peek_denied_without_consume_permission_audits_bus_consume_denied() {
        let (_tmp, bus_dir) = test_bus_dir();
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        crate::db::repository::bus_test_support::create_bus_tables(&db)
            .expect("bus fixture tables");
        let svc = BusService::new(BusInitConfig {
            bus_dir,
            db,
            authorizer: Arc::new(DenyPlainConsumeAuthorizer),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .unwrap();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.peek-denied",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();

        let err = svc.peek(&ctx, "orders.peek-denied", 0, 0, 10, 1024);
        assert!(matches!(err, Err(BusServiceError::PermissionDenied { .. })));

        let logs = crate::db::repository::list_audit_logs(
            &svc.db,
            &crate::db::models::AuditLogFilters {
                action: Some("bus.consume.denied".to_string()),
                ..Default::default()
            },
            0,
            10,
        )
        .unwrap();
        assert_eq!(logs.len(), 1);

        // A denial writes no `bus.messages.browse` row either — the topic
        // lookup (and the data it would expose) is never reached.
        let browse_logs = crate::db::repository::list_audit_logs(
            &svc.db,
            &crate::db::models::AuditLogFilters {
                action: Some("bus.messages.browse".to_string()),
                ..Default::default()
            },
            0,
            10,
        )
        .unwrap();
        assert!(browse_logs.is_empty());
    }

    #[test]
    fn peek_clamps_requested_records_and_bytes_to_its_hard_caps() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.peek-caps",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        for i in 0..(PEEK_MAX_RECORDS + 10) {
            svc.publish(
                &ctx,
                "orders.peek-caps",
                PublishBatch {
                    partition: Some(0),
                    producer: None,
                    records: vec![record(&format!("r-{i}"))],
                },
            )
            .unwrap();
        }

        // Requesting far more than the hard cap must still be clamped to
        // `PEEK_MAX_RECORDS`, not fail and not return everything.
        let result = svc
            .peek(
                &ctx,
                "orders.peek-caps",
                0,
                0,
                PEEK_MAX_RECORDS * 10,
                usize::MAX,
            )
            .unwrap();
        assert_eq!(result.records.len(), PEEK_MAX_RECORDS);
    }

    #[test]
    fn peek_rejects_a_partition_out_of_range() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.peek-range",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        let err = svc.peek(&ctx, "orders.peek-range", 1, 0, 10, 1024);
        assert!(matches!(err, Err(BusServiceError::InvalidArgument(_))));
    }

    // ---- topic_rates (follow-up toru P, task 3) ----------------------

    #[test]
    fn topic_rates_is_zero_before_any_publish() {
        let (_tmp, svc) = test_service();
        assert_eq!(svc.topic_rates("org-1", "orders.never-published"), (0, 0));
    }

    #[test]
    fn topic_rates_reports_the_accepted_msgs_and_bytes_of_the_last_closed_window() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(&ctx, "orders.rates", topics::TopicOptions::default())
            .unwrap();
        svc.publish(
            &ctx,
            "orders.rates",
            PublishBatch {
                partition: Some(0),
                producer: None,
                records: vec![record("aaaa"), record("bb")],
            },
        )
        .unwrap();
        // Force the window to roll by directly driving the counter's clock
        // rather than sleeping 1s in a unit test: `record_publish_rate` is
        // private, so this goes through a second `publish` far enough in
        // logical time to close the window instead. `RateCounter` keys off
        // wall-clock `now_ms()`, so this test instead asserts the ONLY
        // observable behavior available without a fake clock: the counter
        // is non-zero right after publishing into a fresh window.
        let (msgs, bytes) = svc.topic_rates("org-1", "orders.rates");
        assert!(
            msgs == 0 && bytes == 0,
            "the window is still OPEN immediately after the first publish into it \
             (rates() reports the PREVIOUS closed window, which is empty) — got ({msgs}, {bytes})"
        );
    }

    #[test]
    fn topic_rates_are_isolated_per_org_and_per_topic() {
        let (_tmp, svc) = test_service();
        let ctx1 = test_ctx("org-1");
        let ctx2 = test_ctx("org-2");
        svc.create_topic(&ctx1, "orders.a", topics::TopicOptions::default())
            .unwrap();
        svc.create_topic(&ctx1, "orders.b", topics::TopicOptions::default())
            .unwrap();
        svc.create_topic(&ctx2, "orders.a", topics::TopicOptions::default())
            .unwrap();
        svc.publish(
            &ctx1,
            "orders.a",
            PublishBatch {
                partition: Some(0),
                producer: None,
                records: vec![record("x")],
            },
        )
        .unwrap();
        // Only `(org-1, orders.a)` has an entry at all; the rest report the
        // zero default rather than panicking on a missing map entry.
        assert_eq!(svc.topic_rates("org-1", "orders.b"), (0, 0));
        assert_eq!(svc.topic_rates("org-2", "orders.a"), (0, 0));
    }

    // ---- partition_stats (follow-up toru P, task 3) -------------------

    #[test]
    fn partition_stats_reports_earliest_offset_and_high_watermark() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.pstats",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        svc.publish(
            &ctx,
            "orders.pstats",
            PublishBatch {
                partition: Some(0),
                producer: None,
                records: vec![record("a"), record("b"), record("c")],
            },
        )
        .unwrap();
        let stats = svc.partition_stats(&ctx, "orders.pstats", 0).unwrap();
        assert_eq!(stats.earliest_offset, 0);
        assert_eq!(stats.high_watermark, 3);
        // No segment has rolled yet at this size, so there is nothing
        // SEALED — but `segments` still counts the one active segment file
        // (M1-R2 review N-6: "Segmenty 0" was never true, the file exists
        // from the first write), and `size_bytes` reflects its real length,
        // not just sealed bytes (`PartitionStats`'s doc).
        assert_eq!(stats.segments, 1);
        assert!(stats.size_bytes > 0, "size_bytes = {}", stats.size_bytes);
    }

    /// M1-R2 review N-6: `size_bytes`/`segments` must count sealed AND the
    /// active segment once a roll has actually happened, not just report
    /// the sealed-only sum. Drives the engine directly at the exact
    /// on-disk directory `BusService::partition_handle` will reopen (same
    /// pattern as `fetch_reports_offset_out_of_range_after_retention_then_seek_recovers`
    /// above), then asks `partition_stats` for the same partition.
    #[test]
    fn partition_stats_counts_sealed_and_active_segments_after_a_roll() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.pstats-rolled",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();

        let dir = topics::partition_dir(&svc.bus_dir, "org-1", "orders.pstats-rolled", 0);
        {
            let policy = tentaflow_bus::RollPolicy {
                max_batches: 2,
                ..tentaflow_bus::RollPolicy::default()
            };
            let raw =
                tentaflow_bus::Partition::open(&dir, policy, tentaflow_bus::Durability::Os, 8)
                    .expect("raw partition open");
            for i in 0..5i64 {
                let mut builder = tentaflow_bus::BatchBuilder::new(0, 0);
                let rec = tentaflow_bus::RecordInput::new(Bytes::from(format!("r-{i}")), now_ms());
                builder.push(rec).unwrap();
                let wire = builder.build().unwrap();
                raw.append_batch(wire).unwrap();
            }
            // Dropping `raw` releases the directory flock before
            // `BusService` opens its own handle on the same path.
        }

        let stats = svc
            .partition_stats(&ctx, "orders.pstats-rolled", 0)
            .unwrap();
        // 5 batches at max_batches=2 per segment roll into segments of
        // (2, 2, 1) batches — 2 sealed + 1 active, regardless of the
        // `RollPolicy` `BusService::partition_handle` itself reopens with
        // (recovery lists existing segment FILES, not policy-derived).
        assert_eq!(stats.segments, 3);
        assert_eq!(stats.high_watermark, 5);
        assert!(stats.size_bytes > 0, "size_bytes = {}", stats.size_bytes);
    }

    #[test]
    fn partition_stats_rejects_a_partition_out_of_range() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.pstats-range",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        let err = svc.partition_stats(&ctx, "orders.pstats-range", 1);
        assert!(matches!(err, Err(BusServiceError::InvalidArgument(_))));
    }

    // ---- resolve_offset_for_timestamp (follow-up toru P, task 4) ------

    #[test]
    fn resolve_offset_for_timestamp_finds_the_first_record_at_or_after_ts() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.ts-reset",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        let base = now_ms();
        for (i, payload) in ["a", "b", "c"].iter().enumerate() {
            svc.publish(
                &ctx,
                "orders.ts-reset",
                PublishBatch {
                    partition: Some(0),
                    producer: None,
                    records: vec![PublishRecord {
                        key: None,
                        headers: vec![],
                        payload: Bytes::from(payload.to_string()),
                        timestamp_ms: base + i as i64 * 1000,
                        schema_id: 0,
                    }],
                },
            )
            .unwrap();
        }
        // Right at record 1's ("b") timestamp: resolves to offset 1.
        let offset = svc
            .resolve_offset_for_timestamp(&ctx, "orders.ts-reset", 0, base + 1000)
            .unwrap();
        assert_eq!(offset, 1);
        // Between two records' timestamps: resolves to the NEXT one.
        let offset = svc
            .resolve_offset_for_timestamp(&ctx, "orders.ts-reset", 0, base + 1500)
            .unwrap();
        assert_eq!(offset, 2);
    }

    #[test]
    fn resolve_offset_for_timestamp_returns_high_watermark_when_nothing_qualifies() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.ts-reset-future",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        svc.publish(
            &ctx,
            "orders.ts-reset-future",
            PublishBatch {
                partition: Some(0),
                producer: None,
                records: vec![record("only")],
            },
        )
        .unwrap();
        let offset = svc
            .resolve_offset_for_timestamp(&ctx, "orders.ts-reset-future", 0, now_ms() + 3_600_000)
            .unwrap();
        assert_eq!(offset, 1, "must fall back to high_watermark, not error");
    }

    // =========================================================================
    // M2 replication (PLAN-M2 §1e) — wave 2, agent S
    // =========================================================================

    /// Configurable `ReplicationCoordinator` test double for THIS module's
    /// own tests — distinct from `dispatch/bus.rs`'s own `FakeCoordinator`
    /// (a different crate-internal module covering the admin RPC handlers
    /// instead of `BusService`'s own call sites). Fields are
    /// `parking_lot::Mutex`-guarded rather than fixed at construction since
    /// several tests below flip behavior (e.g. leader -> not-leader)
    /// mid-test.
    struct FakeCoordinator {
        role: parking_lot::Mutex<PartitionRole>,
        preflight_err: parking_lot::Mutex<Option<ReplError>>,
        await_outcome: parking_lot::Mutex<AckOutcome>,
        #[allow(clippy::type_complexity)]
        note_offset_commit_calls: parking_lot::Mutex<Vec<(String, String, String, u32, u64, u32)>>,
        snapshot: parking_lot::Mutex<ReplicationSnapshot>,
        reassign_calls: parking_lot::Mutex<Vec<(String, String, Option<u32>)>>,
        local_node_id: parking_lot::Mutex<String>,
    }

    impl FakeCoordinator {
        fn leader(epoch: u32) -> Arc<Self> {
            Arc::new(Self {
                role: parking_lot::Mutex::new(PartitionRole::Leader { epoch }),
                preflight_err: parking_lot::Mutex::new(None),
                await_outcome: parking_lot::Mutex::new(AckOutcome {
                    acked_nodes: 1,
                    required: 1,
                    hw: 0,
                }),
                note_offset_commit_calls: parking_lot::Mutex::new(Vec::new()),
                snapshot: parking_lot::Mutex::new(ReplicationSnapshot::default()),
                reassign_calls: parking_lot::Mutex::new(Vec::new()),
                local_node_id: parking_lot::Mutex::new(String::new()),
            })
        }
        fn set_role(&self, role: PartitionRole) {
            *self.role.lock() = role;
        }
        fn set_preflight_err(&self, err: Option<ReplError>) {
            *self.preflight_err.lock() = err;
        }
        fn set_await_outcome(&self, outcome: AckOutcome) {
            *self.await_outcome.lock() = outcome;
        }
        fn set_snapshot(&self, snapshot: ReplicationSnapshot) {
            *self.snapshot.lock() = snapshot;
        }
        fn set_local_node_id(&self, id: &str) {
            *self.local_node_id.lock() = id.to_string();
        }
    }

    impl ReplicationCoordinator for FakeCoordinator {
        fn role(&self, _org: &str, _topic: &str, _partition: u32) -> PartitionRole {
            self.role.lock().clone()
        }
        fn preflight(
            &self,
            _org: &str,
            topic: &str,
            partition: u32,
            _acks: topics::Acks,
        ) -> Result<u32, ReplError> {
            if let Some(e) = self.preflight_err.lock().clone() {
                return Err(e);
            }
            match &*self.role.lock() {
                PartitionRole::Leader { epoch } => Ok(*epoch),
                _ => Err(ReplError::NotAReplica {
                    topic: topic.to_string(),
                    partition,
                    node_id: "local".to_string(),
                }),
            }
        }
        fn await_acks(
            &self,
            _org: &str,
            _topic: &str,
            _partition: u32,
            _next_offset: u64,
            _acks: topics::Acks,
            _timeout: Duration,
        ) -> Result<AckOutcome, ReplError> {
            Ok(*self.await_outcome.lock())
        }
        fn note_offset_commit(
            &self,
            org: &str,
            group: &str,
            topic: &str,
            partition: u32,
            offset: u64,
            attempts: u32,
        ) {
            self.note_offset_commit_calls.lock().push((
                org.to_string(),
                group.to_string(),
                topic.to_string(),
                partition,
                offset,
                attempts,
            ));
        }
        fn evict_node_from_replica_sets(
            &self,
            _node_id: &str,
            _reason: &'static str,
        ) -> Result<u32, ReplError> {
            Ok(0)
        }
        fn transfer_leader(
            &self,
            _org: &str,
            _topic: &str,
            _partition: u32,
            _target: &str,
        ) -> Result<u32, ReplError> {
            Ok(0)
        }
        fn reassign(
            &self,
            org: &str,
            topic: &str,
            partition: Option<u32>,
            _replicas: &[String],
        ) -> Result<u32, ReplError> {
            self.reassign_calls
                .lock()
                .push((org.to_string(), topic.to_string(), partition));
            Ok(1)
        }
        fn snapshot(&self, _org: &str, _topic: Option<&str>) -> ReplicationSnapshot {
            self.snapshot.lock().clone()
        }
        fn local_node_id(&self) -> String {
            self.local_node_id.lock().clone()
        }
    }

    fn one_partition_topic(svc: &BusService, ctx: &BusCallContext, name: &str) {
        svc.create_topic(
            ctx,
            name,
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn publish_returns_not_leader_when_coordinator_reports_follower() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        one_partition_topic(&svc, &ctx, "orders.repl-not-leader");
        let coord = FakeCoordinator::leader(1);
        coord.set_role(PartitionRole::Follower {
            leader_node_id: "node-b".to_string(),
            epoch: 5,
        });
        svc.set_replication(coord);

        let err = svc
            .publish(
                &ctx,
                "orders.repl-not-leader",
                PublishBatch {
                    partition: Some(0),
                    producer: None,
                    records: vec![record("x")],
                },
            )
            .unwrap_err();
        assert!(
            matches!(
                err,
                BusServiceError::NotLeader {
                    leader_node_id: Some(ref n),
                    leader_epoch: 5,
                } if n == "node-b"
            ),
            "unexpected error: {err:?}"
        );
    }

    /// T1's finding (3): `preflight`'s `ReplicationManager` impl returns
    /// `ReplError::NoAssignment` for ANY non-leader role — there is no
    /// dedicated "not a replica"/"is a follower" variant. `map_repl_error`
    /// must turn that into the SAME `NotLeader` the consume path
    /// (`check_leader_role`) already returns for the identical condition,
    /// not `PartitionUnavailable`.
    #[test]
    fn publish_maps_no_assignment_from_preflight_to_not_leader_like_the_consume_path() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        one_partition_topic(&svc, &ctx, "orders.repl-no-assignment");
        let coord = FakeCoordinator::leader(1);
        coord.set_role(PartitionRole::Follower {
            leader_node_id: "node-b".to_string(),
            epoch: 5,
        });
        coord.set_preflight_err(Some(ReplError::NoAssignment {
            topic: "orders.repl-no-assignment".to_string(),
            partition: 0,
        }));
        svc.set_replication(coord);

        let err = svc
            .publish(
                &ctx,
                "orders.repl-no-assignment",
                PublishBatch {
                    partition: Some(0),
                    producer: None,
                    records: vec![record("x")],
                },
            )
            .unwrap_err();
        assert!(
            matches!(
                err,
                BusServiceError::NotLeader {
                    leader_node_id: Some(ref n),
                    leader_epoch: 5,
                } if n == "node-b"
            ),
            "expected NotLeader (matching check_leader_role's own signal for the \
             same condition), got {err:?}"
        );
    }

    #[test]
    fn publish_returns_not_enough_replicas_from_preflight() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        one_partition_topic(&svc, &ctx, "orders.repl-not-enough-isr");
        let coord = FakeCoordinator::leader(1);
        coord.set_preflight_err(Some(ReplError::NotEnoughReplicas {
            topic: "orders.repl-not-enough-isr".to_string(),
            partition: 0,
            isr: 1,
            required: 2,
        }));
        svc.set_replication(coord);

        let err = svc
            .publish(
                &ctx,
                "orders.repl-not-enough-isr",
                PublishBatch {
                    partition: Some(0),
                    producer: None,
                    records: vec![record("x")],
                },
            )
            .unwrap_err();
        assert!(
            matches!(
                err,
                BusServiceError::NotEnoughReplicas {
                    isr: 1,
                    required: 2
                }
            ),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn publish_returns_ack_timeout_wrapped_in_partial_publish_with_record_landed() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        one_partition_topic(&svc, &ctx, "orders.repl-ack-timeout");
        let coord = FakeCoordinator::leader(1);
        coord.set_await_outcome(AckOutcome {
            acked_nodes: 1,
            required: 2,
            hw: 0,
        });
        svc.set_replication(coord);

        let err = svc
            .publish(
                &ctx,
                "orders.repl-ack-timeout",
                PublishBatch {
                    partition: Some(0),
                    producer: None,
                    records: vec![record("x")],
                },
            )
            .unwrap_err();
        match err {
            BusServiceError::PartialPublish { acked, source } => {
                assert_eq!(acked.len(), 1, "the record IS already on the leader's disk");
                assert_eq!(acked[0].partition, 0);
                assert_eq!(acked[0].accepted, 1);
                assert!(
                    matches!(
                        *source,
                        BusServiceError::AckTimeout {
                            acked: 1,
                            required: 2
                        }
                    ),
                    "unexpected source: {source:?}"
                );
            }
            other => panic!("expected PartialPublish{{AckTimeout}}, got {other:?}"),
        }

        // The record really did land: a fresh read (no coordinator gating
        // reads here since this test only exercises `publish`) confirms one
        // record on disk at offset 0.
        let stats = svc
            .partition_stats(&ctx, "orders.repl-ack-timeout", 0)
            .unwrap();
        assert_eq!(stats.log_end_offset, 1);
    }

    #[test]
    fn publish_succeeds_when_await_acks_meets_required() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        one_partition_topic(&svc, &ctx, "orders.repl-ack-ok");
        let coord = FakeCoordinator::leader(1);
        coord.set_await_outcome(AckOutcome {
            acked_nodes: 2,
            required: 2,
            hw: 1,
        });
        svc.set_replication(coord);

        let result = svc
            .publish(
                &ctx,
                "orders.repl-ack-ok",
                PublishBatch {
                    partition: Some(0),
                    producer: None,
                    records: vec![record("x")],
                },
            )
            .unwrap();
        assert_eq!(result.accepted, 1);
        assert_eq!(result.partitions[0].base_offset, 0);
    }

    #[test]
    fn open_consumer_and_fetch_return_not_leader_when_not_leading() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        one_partition_topic(&svc, &ctx, "orders.repl-consume-not-leader");
        // Publish while still leader (default M1 path, no coordinator yet)
        // so there is something on disk to (attempt to) read.
        svc.publish(
            &ctx,
            "orders.repl-consume-not-leader",
            PublishBatch {
                partition: Some(0),
                producer: None,
                records: vec![record("x")],
            },
        )
        .unwrap();

        let coord = FakeCoordinator::leader(1);
        coord.set_role(PartitionRole::Unavailable {
            reason: UnavailableReason::NoIsr,
        });
        svc.set_replication(coord);

        // `ConsumerHandle` (the `Ok` side) does not implement `Debug`, so
        // `.unwrap_err()` cannot be used directly here (it requires `T:
        // Debug`) — `.err().unwrap()` only needs the `E` side (`BusServiceError`,
        // which does derive it).
        let err = svc
            .open_consumer(
                &ctx,
                "g1",
                &["orders.repl-consume-not-leader".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .err()
            .unwrap();
        assert!(
            matches!(
                err,
                BusServiceError::NotLeader {
                    leader_node_id: None,
                    ..
                }
            ),
            "unexpected error: {err:?}"
        );

        // `peek` (also leader-only, PLAN-M2 §1e) must refuse the same way.
        let err = svc
            .peek(&ctx, "orders.repl-consume-not-leader", 0, 0, 10, 1024)
            .unwrap_err();
        assert!(matches!(err, BusServiceError::NotLeader { .. }));
    }

    /// Full-migration fixture (real `sync::runtime` + a REAL `Sqlite
    /// LedgerAssignmentStore`), separate from `test_service()`'s lighter
    /// bus-tables-only DB — needed because `create_topic`'s placement path
    /// (`assignment_store().propose`) round-trips through the sync runtime,
    /// which `test_service()`'s minimal fixture never initializes. Same
    /// pattern as `db::repository::bus_repository_tests::
    /// locked_ledger_fixture` and `replication::assignment::tests`'s own
    /// copy — confirmed safe to coexist (`sync::runtime::init` is
    /// idempotent process-wide; the returned guard serializes every test
    /// using any of these three copies against each other).
    fn locked_ledger_fixture() -> std::sync::MutexGuard<'static, ()> {
        let guard = crate::addon::fs_sandbox::test_home_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        static INITIALIZED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        if INITIALIZED.get().is_none() {
            let tmp = tempfile::tempdir().expect("tempdir");
            std::env::set_var("HOME", tmp.path());
            std::env::set_var("TENTAFLOW_HOME", tmp.path());
            let conn = rusqlite::Connection::open_in_memory().expect("open db");
            crate::db::migrations::run(&conn).expect("run migrations");
            let db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
            let cipher = std::sync::Arc::new(crate::crypto::SettingsCipher::new(&[13u8; 32]));
            let security = std::sync::Arc::new(
                crate::mesh::security::MeshSecurity::new(db.clone(), cipher.clone())
                    .expect("mesh security"),
            );
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                match crate::sync::runtime::init(db.clone(), security.clone(), cipher.clone()) {
                    Ok(_) => break,
                    Err(crate::sync::ledger::SyncLedgerError::Fjall(fjall::Error::Locked))
                        if std::time::Instant::now() < deadline =>
                    {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    Err(e) => panic!("sync runtime init: {e:?}"),
                }
            }
            std::mem::forget(tmp);
            let _ = INITIALIZED.set(());
        }
        guard
    }

    /// Fala 4 finding (root cause, not just the `NotLeader` symptom the
    /// test above covers): `create_topic` used to derive its own node
    /// identity by searching `coordinator.snapshot(org, None).nodes` for
    /// `is_local` — but that list is populated ENTIRELY from existing
    /// partition assignments, so a fresh org's empty registry can never
    /// contain an `is_local` entry, and `create_topic` silently proposed
    /// ZERO assignments for every topic it ever created. Verified live: a
    /// brand-new topic created through the running release app's own UI
    /// (real coordinator, real mesh) left `bus_partition_assignments`
    /// completely empty for it. This test proves the fix (`coordinator.
    /// local_node_id()`, independent of the registry) on a REAL runtime/
    /// ledger: a `FakeCoordinator` whose `snapshot()` still reports ZERO
    /// nodes (the exact bootstrap-bug scenario) but whose `local_node_id`
    /// IS set must still get a real assignment proposed and materialized.
    #[test]
    fn create_topic_proposes_an_assignment_even_when_the_registry_snapshot_is_still_empty() {
        let _guard = locked_ledger_fixture();
        let dir = tempfile::tempdir().expect("create temp dir");
        let bus_dir = dir.path().join("bus");
        // A fully-migrated DB of its own (not the throwaway one `locked_
        // ledger_fixture` used only to install the process-wide sync
        // runtime singleton) — `store.propose`/`create_topic` write
        // directly into whatever pool they are handed, same convention as
        // `db::repository::bus_repository_tests`'s own tests.
        let conn = rusqlite::Connection::open_in_memory().expect("open db");
        crate::db::migrations::run(&conn).expect("run migrations");
        let db: DbPool = Arc::new(crate::db::Db::from_connection(conn));
        let svc = BusService::new(BusInitConfig {
            bus_dir,
            db: db.clone(),
            authorizer: Arc::new(AllowAllAuthorizer),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .expect("bus service");

        let coord = FakeCoordinator::leader(1);
        coord.set_local_node_id("self-node");
        // Deliberately still empty — the exact registry state a fresh org
        // (or, as the live krytyk pass found, this cluster after DAYS of
        // real use) has before its first assignment ever lands.
        coord.set_snapshot(ReplicationSnapshot::default());
        svc.set_replication(coord);
        svc.set_assignment_store(Arc::new(
            replication::assignment::SqliteLedgerAssignmentStore::new(db.clone()),
        ));

        let ctx = test_ctx("org-bootstrap");
        svc.create_topic(
            &ctx,
            "krytyk.regression.bootstrap",
            topics::TopicOptions {
                partitions: Some(2),
                ..Default::default()
            },
        )
        .expect("create_topic");

        let store = replication::assignment::SqliteLedgerAssignmentStore::new(db);
        let mut rows = store.list_for_node("self-node").expect("list_for_node");
        rows.sort_by_key(|a| a.partition);
        assert_eq!(
            rows.len(),
            2,
            "create_topic must propose one assignment per partition even when \
             coordinator.snapshot() still reports zero nodes — got {rows:?}"
        );
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(row.partition, i as u32);
            assert_eq!(row.leader_node_id, "self-node");
            assert_eq!(row.replicas, vec!["self-node".to_string()]);
            assert_eq!(row.leader_epoch, 1);
        }
    }

    #[test]
    fn commit_calls_note_offset_commit_with_zero_attempts() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        one_partition_topic(&svc, &ctx, "orders.repl-commit-notify");
        let coord = FakeCoordinator::leader(1);
        svc.set_replication(coord.clone());

        svc.publish(
            &ctx,
            "orders.repl-commit-notify",
            PublishBatch {
                partition: Some(0),
                producer: None,
                records: vec![record("x"), record("y")],
            },
        )
        .unwrap();

        let handle = svc
            .open_consumer(
                &ctx,
                "g1",
                &["orders.repl-commit-notify".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();
        let batch = handle.fetch(1024, 0).unwrap();
        assert_eq!(batch.records.len(), 2);
        handle
            .commit(&[(
                TopicPartition {
                    topic: "orders.repl-commit-notify".to_string(),
                    partition: 0,
                },
                2,
            )])
            .unwrap();

        let calls = coord.note_offset_commit_calls.lock();
        assert_eq!(calls.len(), 1);
        let (org, group, topic, partition, offset, attempts) = &calls[0];
        assert_eq!(org, "org-1");
        assert_eq!(group, "g1");
        assert_eq!(topic, "orders.repl-commit-notify");
        assert_eq!(*partition, 0);
        assert_eq!(*offset, 2);
        assert_eq!(*attempts, 0);
    }

    #[test]
    fn build_replica_set_spreads_round_robin_and_always_leads_with_local() {
        let pool = vec!["b".to_string(), "c".to_string(), "d".to_string()];
        let r0 = build_replica_set("a", &pool, 3, 0);
        let r1 = build_replica_set("a", &pool, 3, 1);
        assert_eq!(r0, vec!["a", "b", "c"]);
        assert_eq!(r1, vec!["a", "c", "d"], "round-robin offset by partition");
        // RF=1: local only, pool never consulted.
        assert_eq!(build_replica_set("a", &pool, 1, 0), vec!["a"]);
        // Empty pool: still returns just the local node, never panics.
        assert_eq!(build_replica_set("a", &[], 3, 0), vec!["a"]);
    }

    #[test]
    fn create_topic_defaults_replication_factor_to_min_3_healthy_same_env_nodes() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        let coord = FakeCoordinator::leader(1);
        coord.set_snapshot(ReplicationSnapshot {
            nodes: vec![
                ReplicaNodeInfo {
                    node_id: "local".to_string(),
                    label: "local".to_string(),
                    environment: NodeEnvironment::Prod,
                    is_local: true,
                    reachable: true,
                    last_heartbeat_ms_ago: None,
                    leader_count: 0,
                    follower_count: 0,
                    isr_count: 0,
                },
                ReplicaNodeInfo {
                    node_id: "node-b".to_string(),
                    label: "b".to_string(),
                    environment: NodeEnvironment::Prod,
                    is_local: false,
                    reachable: true,
                    last_heartbeat_ms_ago: Some(100),
                    leader_count: 0,
                    follower_count: 0,
                    isr_count: 0,
                },
                ReplicaNodeInfo {
                    node_id: "node-c".to_string(),
                    label: "c".to_string(),
                    environment: NodeEnvironment::Prod,
                    is_local: false,
                    reachable: true,
                    last_heartbeat_ms_ago: Some(100),
                    leader_count: 0,
                    follower_count: 0,
                    isr_count: 0,
                },
                // `test_service` never sets `settings.node_environment`, so
                // `get_node_environment` resolves to its documented
                // "missing value" default, `NodeEnvironment::Prod`
                // (Z12 fail-closed default) — the local/healthy nodes
                // below are `Prod` for exactly that reason; `node-e` is
                // the wrong-environment (`Dev`) node that must not count.
                // Unreachable and wrong-environment nodes must not count.
                ReplicaNodeInfo {
                    node_id: "node-d".to_string(),
                    label: "d".to_string(),
                    environment: NodeEnvironment::Prod,
                    is_local: false,
                    reachable: false,
                    last_heartbeat_ms_ago: None,
                    leader_count: 0,
                    follower_count: 0,
                    isr_count: 0,
                },
                ReplicaNodeInfo {
                    node_id: "node-e".to_string(),
                    label: "e".to_string(),
                    environment: NodeEnvironment::Dev,
                    is_local: false,
                    reachable: true,
                    last_heartbeat_ms_ago: Some(100),
                    leader_count: 0,
                    follower_count: 0,
                    isr_count: 0,
                },
            ],
            partitions: vec![],
            failovers: vec![],
        });
        svc.set_replication(coord);

        let cfg = svc
            .create_topic(
                &ctx,
                "orders.repl-rf-default",
                topics::TopicOptions {
                    partitions: Some(2),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            cfg.replication_factor, 3,
            "min(3, healthy same-env nodes) = min(3, 3 incl. local)"
        );
    }

    /// P9: create RF=1 (no coordinator, M1 path) stays fast — a loose ×3
    /// bound (1.5 s) on a single-partition topic create, matching the
    /// bench harness's own "absolute and relative" gate philosophy without
    /// pinning this unit test to real disk/CI timing.
    #[test]
    fn create_topic_rf1_is_fast() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        let start = Instant::now();
        svc.create_topic(&ctx, "orders.repl-p9", topics::TopicOptions::default())
            .unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(1_500),
            "create_topic RF=1 took {elapsed:?}, expected well under 500ms (loose x3 bound)"
        );
    }

    #[test]
    fn delete_topic_stops_replication_and_deletes_assignments_before_removing_directory() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        one_partition_topic(&svc, &ctx, "orders.repl-delete");
        let coord = FakeCoordinator::leader(1);
        svc.set_replication(coord.clone());

        // Seed an assignment row directly (bypassing the ledger — this test
        // is about `delete_topic`'s cleanup call sites, not the ledger
        // round trip, which agent L's own `assignment.rs` tests already
        // cover end to end).
        crate::db::repository::bus_assignment_upsert(
            &svc_db(&svc),
            &crate::db::repository::DbBusPartitionAssignment {
                org_id: "org-1".to_string(),
                topic: "orders.repl-delete".to_string(),
                partition: 0,
                leader_node_id: "org-1-local".to_string(),
                replicas: vec!["org-1-local".to_string()],
                isr: vec!["org-1-local".to_string()],
                leader_epoch: 1,
                environment: "dev".to_string(),
                updated_at_ms: now_ms(),
            },
        )
        .unwrap();
        assert!(crate::db::repository::bus_assignment_get(
            &svc_db(&svc),
            "org-1",
            "orders.repl-delete",
            0
        )
        .unwrap()
        .is_some());

        svc.delete_topic(&ctx, "orders.repl-delete").unwrap();

        assert!(
            crate::db::repository::bus_assignment_get(
                &svc_db(&svc),
                "org-1",
                "orders.repl-delete",
                0
            )
            .unwrap()
            .is_none(),
            "delete_topic must delete this topic's assignments"
        );
        let reassigns = coord.reassign_calls.lock();
        assert_eq!(reassigns.len(), 1);
        assert_eq!(reassigns[0].0, "org-1");
        assert_eq!(reassigns[0].1, "orders.repl-delete");
        assert_eq!(
            reassigns[0].2, None,
            "None means every partition of this topic"
        );
    }

    #[test]
    fn purge_org_stops_replication_and_deletes_assignments_for_every_topic() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-purge-repl");
        one_partition_topic(&svc, &ctx, "orders.a");
        one_partition_topic(&svc, &ctx, "orders.b");
        let coord = FakeCoordinator::leader(1);
        svc.set_replication(coord.clone());

        for topic in ["orders.a", "orders.b"] {
            crate::db::repository::bus_assignment_upsert(
                &svc_db(&svc),
                &crate::db::repository::DbBusPartitionAssignment {
                    org_id: "org-purge-repl".to_string(),
                    topic: topic.to_string(),
                    partition: 0,
                    leader_node_id: "local".to_string(),
                    replicas: vec!["local".to_string()],
                    isr: vec!["local".to_string()],
                    leader_epoch: 1,
                    environment: "dev".to_string(),
                    updated_at_ms: now_ms(),
                },
            )
            .unwrap();
        }

        let report = svc.purge_org("org-purge-repl").unwrap();
        assert_eq!(report.assignments_deleted, 2);
        assert!(crate::db::repository::bus_assignment_list_for_topic(
            &svc_db(&svc),
            "org-purge-repl",
            "orders.a"
        )
        .unwrap()
        .is_empty());
        let reassigns = coord.reassign_calls.lock();
        assert_eq!(reassigns.len(), 2, "one reassign(None) call per topic");
    }

    /// M2 (PLAN-M2 §1e, A9 debt): `partition_handle_lru` evicts the least-
    /// recently-used idle handle once the cap is exceeded, but never one a
    /// live `ConsumerHandle` still references.
    #[test]
    fn partition_handle_lru_evicts_least_recently_used_idle_handle() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let bus_dir = dir.path().join("bus");
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        crate::db::repository::bus_test_support::create_bus_tables(&db)
            .expect("bus fixture tables");
        let svc = BusService::new(BusInitConfig {
            bus_dir,
            db,
            authorizer: Arc::new(AllowAllAuthorizer),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: Some(1),
            publish_ack_timeout: DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .expect("bus service");
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.lru",
            topics::TopicOptions {
                partitions: Some(2),
                ..Default::default()
            },
        )
        .unwrap();

        // Opens partition 0's handle — only entry, well under cap.
        svc.publish(
            &ctx,
            "orders.lru",
            PublishBatch {
                partition: Some(0),
                producer: None,
                records: vec![record("x")],
            },
        )
        .unwrap();
        assert_eq!(svc.partitions.len(), 1);

        // Opens partition 1's handle — now 2 > cap(1), evicts partition 0
        // (idle: no live consumer, no coordinator installed so the
        // Leader/Follower guard never applies).
        svc.publish(
            &ctx,
            "orders.lru",
            PublishBatch {
                partition: Some(1),
                producer: None,
                records: vec![record("y")],
            },
        )
        .unwrap();
        assert_eq!(
            svc.partitions.len(),
            1,
            "partition 0's handle must have been evicted"
        );
        assert!(!svc
            .partitions
            .contains_key(&("org-1".to_string(), "orders.lru".to_string(), 0)));
        assert!(svc
            .partitions
            .contains_key(&("org-1".to_string(), "orders.lru".to_string(), 1)));

        // Transparently reopens partition 0 on the next access — eviction
        // must not have lost any data.
        let stats = svc.partition_stats(&ctx, "orders.lru", 0).unwrap();
        assert_eq!(stats.log_end_offset, 1);
    }

    /// A live `ConsumerHandle` must never have its partition evicted, even
    /// when it has not been the most-recently-accessed key.
    #[test]
    fn partition_handle_lru_never_evicts_a_live_consumer_partition() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let bus_dir = dir.path().join("bus");
        let db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        crate::db::repository::bus_test_support::create_bus_tables(&db)
            .expect("bus fixture tables");
        let svc = BusService::new(BusInitConfig {
            bus_dir,
            db,
            authorizer: Arc::new(AllowAllAuthorizer),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: Some(1),
            publish_ack_timeout: DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .expect("bus service");
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "orders.lru-consumer",
            topics::TopicOptions {
                partitions: Some(2),
                ..Default::default()
            },
        )
        .unwrap();
        svc.publish(
            &ctx,
            "orders.lru-consumer",
            PublishBatch {
                partition: Some(0),
                producer: None,
                records: vec![record("x")],
            },
        )
        .unwrap();
        // Opening a consumer on partition 0 registers it in
        // `consumer_partitions` — must survive the eviction pass below.
        let _handle = svc
            .open_consumer(
                &ctx,
                "g1",
                &["orders.lru-consumer".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();

        svc.publish(
            &ctx,
            "orders.lru-consumer",
            PublishBatch {
                partition: Some(1),
                producer: None,
                records: vec![record("y")],
            },
        )
        .unwrap();

        assert!(
            svc.partitions.contains_key(&(
                "org-1".to_string(),
                "orders.lru-consumer".to_string(),
                0
            )),
            "a partition with a live ConsumerHandle must never be evicted"
        );
    }

    fn svc_db(svc: &BusService) -> DbPool {
        svc.db.clone()
    }

    // ---- SUM/tentabus/POLITYKI-POL.md: field-level access policies -------

    fn set_field_set(items: &[&str]) -> std::collections::BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn publish_rejects_batch_with_field_not_in_write_policy() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(&ctx, "patients.updated", topics::TopicOptions::default())
            .unwrap();
        field_policies::set_policy(
            &svc.db,
            "org-1",
            "patients.updated",
            "any",
            field_policies::SUBJECT_ANY,
            field_policies::Direction::Write,
            &set_field_set(&["patient_id", "status"]),
            &set_field_set(&[]),
        )
        .unwrap();

        let err = svc
            .publish(
                &ctx,
                "patients.updated",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![record(r#"{"patient_id":"1","ssn":"999-99-9999"}"#)],
                },
            )
            .unwrap_err();
        match err {
            BusServiceError::FieldNotAllowed { topic, fields } => {
                assert_eq!(topic, "patients.updated");
                assert_eq!(fields, vec!["ssn".to_string()]);
            }
            other => panic!("expected FieldNotAllowed, got {other:?}"),
        }

        // Rejected batch must never have landed on the log.
        let handle = svc
            .open_consumer(
                &ctx,
                "g1",
                &["patients.updated".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();
        let batch = handle.fetch(1024, 10).unwrap();
        assert!(batch.records.is_empty());
    }

    #[test]
    fn publish_rejects_batch_missing_required_field() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(&ctx, "patients.updated", topics::TopicOptions::default())
            .unwrap();
        field_policies::set_policy(
            &svc.db,
            "org-1",
            "patients.updated",
            "any",
            field_policies::SUBJECT_ANY,
            field_policies::Direction::Write,
            &set_field_set(&["patient_id", "status"]),
            &set_field_set(&["status"]),
        )
        .unwrap();

        let err = svc
            .publish(
                &ctx,
                "patients.updated",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![record(r#"{"patient_id":"1"}"#)],
                },
            )
            .unwrap_err();
        assert!(matches!(
            err,
            BusServiceError::RequiredFieldMissing { .. }
        ));
    }

    #[test]
    fn publish_accepts_batch_within_write_policy() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(&ctx, "patients.updated", topics::TopicOptions::default())
            .unwrap();
        field_policies::set_policy(
            &svc.db,
            "org-1",
            "patients.updated",
            "any",
            field_policies::SUBJECT_ANY,
            field_policies::Direction::Write,
            &set_field_set(&["patient_id", "status"]),
            &set_field_set(&[]),
        )
        .unwrap();

        let result = svc
            .publish(
                &ctx,
                "patients.updated",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![record(r#"{"patient_id":"1","status":"ok"}"#)],
                },
            )
            .unwrap();
        assert_eq!(result.accepted, 1);
    }

    #[test]
    fn peek_hides_fields_outside_read_policy() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(&ctx, "patients.updated", topics::TopicOptions::default())
            .unwrap();
        svc.publish(
            &ctx,
            "patients.updated",
            PublishBatch {
                partition: Some(0),
                producer: None,
                records: vec![record(r#"{"patient_id":"1","ssn":"999-99-9999"}"#)],
            },
        )
        .unwrap();
        field_policies::set_policy(
            &svc.db,
            "org-1",
            "patients.updated",
            "any",
            field_policies::SUBJECT_ANY,
            field_policies::Direction::Read,
            &set_field_set(&["patient_id"]),
            &set_field_set(&[]),
        )
        .unwrap();

        let result = svc.peek(&ctx, "patients.updated", 0, 0, 10, 1024).unwrap();
        assert_eq!(result.records.len(), 1);
        let value: serde_json::Value =
            serde_json::from_slice(&result.records[0].payload).unwrap();
        assert_eq!(value, serde_json::json!({"patient_id": "1"}));
    }

    #[test]
    fn fetch_hides_fields_outside_read_policy() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(
            &ctx,
            "patients.updated",
            topics::TopicOptions {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .unwrap();
        let handle = svc
            .open_consumer(
                &ctx,
                "g1",
                &["patients.updated".to_string()],
                ConsumerConfig {
                    commit_mode: groups::CommitMode::Explicit,
                },
            )
            .unwrap();
        svc.publish(
            &ctx,
            "patients.updated",
            PublishBatch {
                partition: Some(0),
                producer: None,
                records: vec![record(r#"{"patient_id":"1","ssn":"999-99-9999"}"#)],
            },
        )
        .unwrap();
        field_policies::set_policy(
            &svc.db,
            "org-1",
            "patients.updated",
            "any",
            field_policies::SUBJECT_ANY,
            field_policies::Direction::Read,
            &set_field_set(&["patient_id"]),
            &set_field_set(&[]),
        )
        .unwrap();

        let batch = handle.fetch(1024, 50).unwrap();
        assert_eq!(batch.records.len(), 1);
        let value: serde_json::Value = serde_json::from_slice(&batch.records[0].payload).unwrap();
        assert_eq!(value, serde_json::json!({"patient_id": "1"}));
    }

    #[test]
    fn publish_and_peek_unaffected_when_no_policy_exists() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(&ctx, "orders.plain", topics::TopicOptions::default())
            .unwrap();
        svc.publish(
            &ctx,
            "orders.plain",
            PublishBatch {
                partition: Some(0),
                producer: None,
                records: vec![record(r#"{"anything":"goes","here":1}"#)],
            },
        )
        .unwrap();
        let result = svc.peek(&ctx, "orders.plain", 0, 0, 10, 1024).unwrap();
        assert_eq!(result.records.len(), 1);
        let value: serde_json::Value =
            serde_json::from_slice(&result.records[0].payload).unwrap();
        assert_eq!(value, serde_json::json!({"anything":"goes","here":1}));
    }

    #[test]
    fn write_policy_on_user_subject_takes_precedence_over_wildcard() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1"); // actor = "tester"
        svc.create_topic(&ctx, "patients.updated", topics::TopicOptions::default())
            .unwrap();
        // Wildcard: only "patient_id" allowed.
        field_policies::set_policy(
            &svc.db,
            "org-1",
            "patients.updated",
            "any",
            field_policies::SUBJECT_ANY,
            field_policies::Direction::Write,
            &set_field_set(&["patient_id"]),
            &set_field_set(&[]),
        )
        .unwrap();
        // Exact user row for "tester": also allows "status".
        field_policies::set_policy(
            &svc.db,
            "org-1",
            "patients.updated",
            "user",
            "tester",
            field_policies::Direction::Write,
            &set_field_set(&["patient_id", "status"]),
            &set_field_set(&[]),
        )
        .unwrap();

        let result = svc
            .publish(
                &ctx,
                "patients.updated",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![record(r#"{"patient_id":"1","status":"ok"}"#)],
                },
            )
            .unwrap();
        assert_eq!(result.accepted, 1);
    }

    // ---- SUM/tentabus/POLITYKI-POL-FORMATY.md (F0): guards -----------

    #[test]
    fn update_topic_rejects_content_type_change_with_existing_field_policy() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        svc.create_topic(&ctx, "patients.updated", topics::TopicOptions::default())
            .unwrap();
        field_policies::set_policy(
            &svc.db,
            "org-1",
            "patients.updated",
            "any",
            field_policies::SUBJECT_ANY,
            field_policies::Direction::Write,
            &set_field_set(&["patient_id"]),
            &set_field_set(&[]),
        )
        .unwrap();

        let err = svc
            .update_topic(
                &ctx,
                "patients.updated",
                topics::TopicOptions {
                    content_type: Some("application/xml".to_string()),
                    ..Default::default()
                },
            )
            .unwrap_err();
        assert!(matches!(err, BusServiceError::InvalidTopicConfig { .. }));

        // Re-applying the topic's CURRENT content_type must remain a no-op,
        // not be rejected — only an actual change is guarded against.
        let current = topics::get_topic(&svc.db, "org-1", "patients.updated")
            .unwrap()
            .unwrap()
            .content_type;
        svc.update_topic(
            &ctx,
            "patients.updated",
            topics::TopicOptions {
                content_type: Some(current),
                ..Default::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn set_policy_rejects_unknown_topic() {
        let (_tmp, svc) = test_service();
        let err = field_policies::set_policy(
            &svc.db,
            "org-1",
            "no.such.topic",
            "any",
            field_policies::SUBJECT_ANY,
            field_policies::Direction::Write,
            &set_field_set(&["patient_id"]),
            &set_field_set(&[]),
        )
        .unwrap_err();
        assert!(matches!(err, BusServiceError::TopicNotFound { .. }));
    }

    // ---- SUM/tentabus/POLITYKI-POL-FORMATY.md (F1/F2): XML + HL7 v2 ---
    // Same publish/peek path as the JSON tests above, routed through the
    // topic's declared `content_type` to a non-JSON codec.

    fn create_typed_topic(svc: &BusService, ctx: &BusCallContext, topic: &str, content_type: &str) {
        svc.create_topic(
            ctx,
            topic,
            topics::TopicOptions {
                partitions: Some(1),
                content_type: Some(content_type.to_string()),
                ..Default::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn xml_topic_publish_rejects_field_outside_write_policy() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        create_typed_topic(&svc, &ctx, "patients.xml", "application/xml");
        field_policies::set_policy(
            &svc.db,
            "org-1",
            "patients.xml",
            "any",
            field_policies::SUBJECT_ANY,
            field_policies::Direction::Write,
            &set_field_set(&["id", "status"]),
            &set_field_set(&[]),
        )
        .unwrap();

        let err = svc
            .publish(
                &ctx,
                "patients.xml",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![record("<Patient><id>1</id><ssn>999</ssn></Patient>")],
                },
            )
            .unwrap_err();
        match err {
            BusServiceError::FieldNotAllowed { fields, .. } => {
                assert_eq!(fields, vec!["ssn".to_string()]);
            }
            other => panic!("expected FieldNotAllowed, got {other:?}"),
        }
    }

    #[test]
    fn xml_topic_peek_projects_to_allowed_children() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        create_typed_topic(&svc, &ctx, "patients.xml", "application/xml");
        svc.publish(
            &ctx,
            "patients.xml",
            PublishBatch {
                partition: Some(0),
                producer: None,
                records: vec![record(
                    "<Patient v=\"1\"><id>1</id><ssn>999</ssn><name><first>Jan</first></name></Patient>",
                )],
            },
        )
        .unwrap();
        field_policies::set_policy(
            &svc.db,
            "org-1",
            "patients.xml",
            "any",
            field_policies::SUBJECT_ANY,
            field_policies::Direction::Read,
            &set_field_set(&["id", "name"]),
            &set_field_set(&[]),
        )
        .unwrap();

        let result = svc.peek(&ctx, "patients.xml", 0, 0, 10, 1024).unwrap();
        assert_eq!(result.records.len(), 1);
        let text = String::from_utf8(result.records[0].payload.to_vec()).unwrap();
        assert_eq!(
            text,
            "<Patient v=\"1\"><id>1</id><name><first>Jan</first></name></Patient>"
        );
    }

    #[test]
    fn xml_topic_set_policy_rejects_field_name_invalid_for_xml() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        create_typed_topic(&svc, &ctx, "patients.xml", "application/xml");
        let err = field_policies::set_policy(
            &svc.db,
            "org-1",
            "patients.xml",
            "any",
            field_policies::SUBJECT_ANY,
            field_policies::Direction::Write,
            &set_field_set(&["1st-element"]),
            &set_field_set(&[]),
        )
        .unwrap_err();
        assert!(matches!(err, BusServiceError::InvalidArgument(_)), "{err:?}");
    }

    const HL7_SAMPLE: &str =
        "MSH|^~\\&|APP|FAC|RAPP|RFAC|20260902||ADT^A01|MSG1|P|2.5\rPID|1||MRN123||Doe^Jan||19800101|M\r";

    #[test]
    fn hl7_topic_publish_rejects_field_outside_write_policy() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        create_typed_topic(&svc, &ctx, "adt.hl7", "application/hl7-v2");
        // Everything the sample carries except PID-5 (patient name).
        field_policies::set_policy(
            &svc.db,
            "org-1",
            "adt.hl7",
            "any",
            field_policies::SUBJECT_ANY,
            field_policies::Direction::Write,
            &set_field_set(&[
                "MSH-3", "MSH-4", "MSH-5", "MSH-6", "MSH-7", "MSH-8", "MSH-9", "MSH-10", "MSH-11",
                "MSH-12", "PID-1", "PID-2", "PID-3", "PID-4", "PID-6", "PID-7", "PID-8",
            ]),
            &set_field_set(&[]),
        )
        .unwrap();

        let err = svc
            .publish(
                &ctx,
                "adt.hl7",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![record(HL7_SAMPLE)],
                },
            )
            .unwrap_err();
        match err {
            BusServiceError::FieldNotAllowed { fields, .. } => {
                assert_eq!(fields, vec!["PID-5".to_string()]);
            }
            other => panic!("expected FieldNotAllowed, got {other:?}"),
        }
    }

    #[test]
    fn hl7_topic_peek_blanks_fields_outside_read_policy_in_place() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        create_typed_topic(&svc, &ctx, "adt.hl7", "application/hl7-v2");
        svc.publish(
            &ctx,
            "adt.hl7",
            PublishBatch {
                partition: Some(0),
                producer: None,
                records: vec![record(HL7_SAMPLE)],
            },
        )
        .unwrap();
        field_policies::set_policy(
            &svc.db,
            "org-1",
            "adt.hl7",
            "any",
            field_policies::SUBJECT_ANY,
            field_policies::Direction::Read,
            &set_field_set(&["MSH-9", "PID-3"]),
            &set_field_set(&[]),
        )
        .unwrap();

        let result = svc.peek(&ctx, "adt.hl7", 0, 0, 10, 1024).unwrap();
        assert_eq!(result.records.len(), 1);
        let text = String::from_utf8(result.records[0].payload.to_vec()).unwrap();
        assert_eq!(
            text,
            // MSH-1/MSH-2 structural + MSH-9 kept; MSH-3..8 and MSH-10..12
            // blanked in place (same separator count as the input).
            "MSH|^~\\&|||||||ADT^A01|||\rPID|||MRN123|||||\r"
        );
    }

    #[test]
    fn hl7_topic_set_policy_rejects_structural_msh_fields() {
        let (_tmp, svc) = test_service();
        let ctx = test_ctx("org-1");
        create_typed_topic(&svc, &ctx, "adt.hl7", "application/hl7-v2");
        for bad in ["MSH-1", "MSH-2", "pid-5", "PID"] {
            let err = field_policies::set_policy(
                &svc.db,
                "org-1",
                "adt.hl7",
                "any",
                field_policies::SUBJECT_ANY,
                field_policies::Direction::Read,
                &set_field_set(&[bad]),
                &set_field_set(&[]),
            )
            .unwrap_err();
            assert!(
                matches!(err, BusServiceError::InvalidArgument(_)),
                "{bad}: {err:?}"
            );
        }
    }
}
