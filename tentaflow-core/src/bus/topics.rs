// =============================================================================
// File: bus/topics.rs — TentaBus M1: topic lifecycle, config, on-disk layout
// =============================================================================
//
// Config surface and defaults follow PLAN.md §7.1 verbatim. Persistence is
// SQLite (`bus_topics`, migration v141 — owned by the parallel DB/RBAC
// agent, tor D per PLAN.md §9.1). COORDINATION: this file assumes that
// table exists with the shape `DbBusTopic`/the SQL in
// `db/repository.rs`'s `bus_topic_*` functions describe; it does not create
// it (`db/migrations.rs` is off-limits per this task's file ownership).
// Until v141 lands, tests here build their own fixture connection with an
// identical `CREATE TABLE` (see `db/repository.rs` bus tests) instead of
// going through `crate::db::migrations::run`.

use tentaflow_protocol::environment::NodeEnvironment;

use crate::db::repository::{self, DbBusTopic};
use crate::db::DbPool;

use super::payload_format::PayloadFormat;
use super::schema_registry::SchemaType;
use super::BusServiceError;

/// PLAN §7.1 `name`: `^[a-z0-9]([a-z0-9.\-]{1,126})$`. Internal topics
/// (`__dlq.<topic>`, `__bus.metrics`) deliberately fall outside this and use
/// `validate_internal_topic_name` instead — the leading `__` is a reserved
/// namespace normal `create_topic` callers can never reach.
pub const RESERVED_PREFIX: &str = "__";

/// PLAN §8.4/M4: broker-owned internal topic carrying 1-second
/// `BusMetricsRollup` snapshots — dogfooding source for a future ClickHouse
/// sink. Created lazily by `BusService::publish_metrics_rollup` the same way
/// `dlq::dlq_topic_name` topics are, via `create_internal_topic`.
pub const METRICS_TOPIC_NAME: &str = "__bus.metrics";

/// Longest a topic name (user or internal, `__` prefix included) may be.
/// PLAN §7.1's regex caps a user name at 127 bytes total (1 mandatory
/// leading char + up to 126 more); internal names share that same ceiling
/// so `<bus_dir>/<org_id>/<topic>/pNNNN` component lengths stay uniform
/// regardless of which validator accepted the name.
pub const MAX_TOPIC_NAME_LEN: usize = 127;

fn is_name_char(b: u8) -> bool {
    matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-')
}

/// Length of the `__dlq.` prefix `dlq_topic_name` (`dlq.rs`) prepends
/// unconditionally to build a source topic's DLQ topic name. Kept as its
/// own constant (rather than importing `dlq::DLQ_TOPIC_PREFIX` and taking
/// its `.len()`) so this file's length budget below does not depend on
/// `dlq.rs` staying internally consistent with itself — if that prefix
/// ever changes, `dlq_topic_name`'s own invariant doc comment is the place
/// that must change together with this literal.
const DLQ_PREFIX_LEN: usize = 6; // "__dlq.".len()

/// Validates a user-supplied topic name against PLAN §7.1's regex and
/// refuses the `__` namespace reserved for broker-internal topics (DLQ,
/// `__bus.metrics`).
///
/// Also rejects a name that would leave no room for the
/// `__dlq.` prefix its own DLQ topic needs — without this, a 122+ byte
/// name passes here but `dlq_topic_name(name)` produces a >127-byte
/// internal name that `validate_internal_topic_name` then rejects, so
/// `note_delivery_failure` would fail with `InvalidTopicName` at the exact
/// moment delivery attempts are exhausted, instead of at topic creation.
pub fn validate_user_topic_name(name: &str) -> Result<(), BusServiceError> {
    if name.starts_with(RESERVED_PREFIX) {
        return Err(BusServiceError::InvalidTopicName {
            name: name.to_string(),
            reason: "the '__' prefix is reserved for internal topics",
        });
    }
    validate_name_shape(name)?;
    if name.len() + DLQ_PREFIX_LEN > MAX_TOPIC_NAME_LEN {
        return Err(BusServiceError::InvalidTopicName {
            name: name.to_string(),
            reason: "name is too long to leave room for the '__dlq.' prefix its DLQ topic needs",
        });
    }
    Ok(())
}

/// Validates an internal (`__`-prefixed) topic name, used only by
/// `dlq.rs`'s `__dlq.<topic>` construction and any future `__bus.*` topic.
/// Same character class as the user regex, minus the "no leading
/// underscore" restriction.
pub fn validate_internal_topic_name(name: &str) -> Result<(), BusServiceError> {
    let Some(rest) = name.strip_prefix(RESERVED_PREFIX) else {
        return Err(BusServiceError::InvalidTopicName {
            name: name.to_string(),
            reason: "internal topic names must start with '__'",
        });
    };
    if rest.is_empty() || name.len() > MAX_TOPIC_NAME_LEN {
        return Err(BusServiceError::InvalidTopicName {
            name: name.to_string(),
            reason: "length must be 3-127 bytes",
        });
    }
    if rest.as_bytes().iter().any(|&b| !is_name_char(b)) {
        return Err(BusServiceError::InvalidTopicName {
            name: name.to_string(),
            reason: "only [a-z0-9.-] allowed after the '__' prefix",
        });
    }
    Ok(())
}

fn validate_name_shape(name: &str) -> Result<(), BusServiceError> {
    let bytes = name.as_bytes();
    // Regex `^[a-z0-9]([a-z0-9.\-]{1,126})$`: 1 mandatory leading char + 1-126
    // more, so total length 2..=127.
    if bytes.len() < 2 || bytes.len() > MAX_TOPIC_NAME_LEN {
        return Err(BusServiceError::InvalidTopicName {
            name: name.to_string(),
            reason: "length must be 2-127 bytes",
        });
    }
    let first = bytes[0];
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return Err(BusServiceError::InvalidTopicName {
            name: name.to_string(),
            reason: "must start with a lowercase letter or digit",
        });
    }
    if bytes[1..].iter().any(|&b| !is_name_char(b)) {
        return Err(BusServiceError::InvalidTopicName {
            name: name.to_string(),
            reason: "only [a-z0-9.-] allowed after the first character",
        });
    }
    Ok(())
}

/// Longest an `org_id` may be. Not from PLAN §7.1's topic table (`org_id`
/// is not a topic setting) — chosen to match the topic-name ceiling since
/// `org_id` is just another path-segment identifier flowing through the
/// same on-disk layout (`topic_dir`, `partition_dir`).
pub const MAX_ORG_ID_LEN: usize = 64;

/// `org_id` is the one identifier in this module that flows straight into
/// filesystem paths (`topic_dir`/`partition_dir`, joined with no further
/// checks) and into a
/// destructive `std::fs::remove_dir_all` (`BusService::purge_org`). Without
/// this, an `org_id` containing `..` or an absolute path could make
/// `purge_org` delete a directory outside `bus_dir` entirely, and
/// `org_id == "_meta"` collides with the reserved directory name fjall's
/// own keyspace (offsets, producer sequences) lives under, letting
/// `purge_org("_meta")` erase every organization's state at once.
///
/// Same character class and length bound as a topic name
/// (`is_name_char`/`MAX_TOPIC_NAME_LEN`, tightened to `MAX_ORG_ID_LEN`
/// here), plus an explicit ban on a leading `_` — not just `_meta` itself,
/// so any future `_`-prefixed reserved directory is covered too — even
/// though that charset already excludes `_` outright (this is the
/// specific-reason check a caller sees instead of the generic charset one).
/// The charset itself already rules out `/` (path separators) entirely;
/// `..` is additionally checked explicitly because it is expressible using
/// only allowed characters (two literal dots) and, as a whole path
/// segment, is exactly what would let `bus_dir.join(org_id)` escape
/// `bus_dir`.
pub fn validate_org_id(org_id: &str) -> Result<(), BusServiceError> {
    let bytes = org_id.as_bytes();
    if bytes.is_empty() || bytes.len() > MAX_ORG_ID_LEN {
        return Err(BusServiceError::InvalidArgument(format!(
            "invalid org_id '{org_id}': length must be 1-{MAX_ORG_ID_LEN} bytes"
        )));
    }
    if org_id.starts_with('_') {
        return Err(BusServiceError::InvalidArgument(format!(
            "invalid org_id '{org_id}': must not start with '_' (reserved for internal \
             directories such as '_meta')"
        )));
    }
    if org_id == ".." || org_id.contains("..") {
        return Err(BusServiceError::InvalidArgument(format!(
            "invalid org_id '{org_id}': must not contain '..'"
        )));
    }
    if bytes.iter().any(|&b| !is_name_char(b)) {
        return Err(BusServiceError::InvalidArgument(format!(
            "invalid org_id '{org_id}': only [a-z0-9.-] allowed"
        )));
    }
    Ok(())
}

// ---- PLAN §7.1 defaults -----------------------------------------------

pub const DEFAULT_PARTITIONS: u32 = 8;
pub const MIN_PARTITIONS: u32 = 1;
pub const MAX_PARTITIONS: u32 = 256;
pub const DEFAULT_RETENTION_MS: i64 = 7 * 24 * 3_600_000;
pub const DEFAULT_RETENTION_BYTES_PER_PARTITION: i64 = 10 * 1024 * 1024 * 1024;
pub const DEFAULT_DEDUP_WINDOW_MS: i64 = 24 * 3_600_000;
pub const DEFAULT_MAX_DELIVERY_ATTEMPTS: u32 = 5;
pub const DEFAULT_RETRY_BACKOFF_MS: u32 = 1_000;
pub const DEFAULT_RETRY_BACKOFF_CAP_MS: u32 = 60_000;
pub const DEFAULT_MAX_INLINE_BYTES: usize = 1024 * 1024;
pub const DEFAULT_CONTENT_TYPE: &str = "application/octet-stream";

// ---- PLAN §7.1 range table --------------------------------------
//
// `partitions` is validated separately (at construction, its rule differs
// on update — grow-only) and `replication_factor` is silently clamped
// rather than rejected; every other numeric setting below is rejected
// outright when out of range. This matters beyond input hygiene:
// `max_delivery_attempts = 0` would send every failure straight to the
// DLQ on the very first attempt, `retention_ms = 0` would delete every
// sealed segment on the very next retention sweep, and a negative
// `dedup_window_ms` would make every record look `Fresh` forever (dedup
// silently disabled). All six ranges below are PLAN §7.1's table verbatim.

pub const MIN_RETENTION_MS: i64 = 3_600_000; // 1 hour
pub const MAX_RETENTION_MS: i64 = 10 * 365 * 24 * 3_600_000; // 10 years (365-day)
pub const MIN_RETENTION_BYTES_PER_PARTITION: i64 = 64 * 1024 * 1024; // 64 MiB
pub const MAX_RETENTION_BYTES_PER_PARTITION: i64 = 10 * 1024 * 1024 * 1024 * 1024; // 10 TiB
pub const MIN_DEDUP_WINDOW_MS: i64 = 3_600_000; // 1 hour
pub const MAX_DEDUP_WINDOW_MS: i64 = 30 * 24 * 3_600_000; // 30 days
pub const MIN_MAX_DELIVERY_ATTEMPTS: u32 = 1;
pub const MAX_MAX_DELIVERY_ATTEMPTS: u32 = 100;
pub const MIN_RETRY_BACKOFF_MS: u32 = 100;
pub const MAX_RETRY_BACKOFF_MS: u32 = 3_600_000; // 1 hour
pub const MIN_MAX_INLINE_BYTES: usize = 4 * 1024; // 4 KiB
pub const MAX_MAX_INLINE_BYTES: usize = 8 * 1024 * 1024; // 8 MiB

/// Validates every numeric setting in PLAN §7.1's range table except
/// `partitions` (checked separately, at construction, because its rule
/// differs on update — grow-only) and `replication_factor` (silently
/// clamped rather than rejected — decision predates this task, left
/// unchanged). Called after defaulting/merging in both `from_options` and
/// `apply_options`, so an out-of-range value is rejected identically
/// whether it arrived at creation or via a later update.
fn validate_ranges(cfg: &TopicConfig) -> Result<(), BusServiceError> {
    fn out_of_range(
        field: &str,
        min: impl std::fmt::Display,
        max: impl std::fmt::Display,
        got: impl std::fmt::Display,
    ) -> BusServiceError {
        BusServiceError::InvalidTopicConfig {
            reason: format!("{field} must be {min}-{max}, got {got}"),
        }
    }
    if !(MIN_RETENTION_MS..=MAX_RETENTION_MS).contains(&cfg.retention_ms) {
        return Err(out_of_range(
            "retention_ms",
            MIN_RETENTION_MS,
            MAX_RETENTION_MS,
            cfg.retention_ms,
        ));
    }
    if !(MIN_RETENTION_BYTES_PER_PARTITION..=MAX_RETENTION_BYTES_PER_PARTITION)
        .contains(&cfg.retention_bytes_per_partition)
    {
        return Err(out_of_range(
            "retention_bytes",
            MIN_RETENTION_BYTES_PER_PARTITION,
            MAX_RETENTION_BYTES_PER_PARTITION,
            cfg.retention_bytes_per_partition,
        ));
    }
    if !(MIN_DEDUP_WINDOW_MS..=MAX_DEDUP_WINDOW_MS).contains(&cfg.dedup_window_ms) {
        return Err(out_of_range(
            "dedup_window_ms",
            MIN_DEDUP_WINDOW_MS,
            MAX_DEDUP_WINDOW_MS,
            cfg.dedup_window_ms,
        ));
    }
    if !(MIN_MAX_DELIVERY_ATTEMPTS..=MAX_MAX_DELIVERY_ATTEMPTS).contains(&cfg.max_delivery_attempts)
    {
        return Err(out_of_range(
            "max_delivery_attempts",
            MIN_MAX_DELIVERY_ATTEMPTS,
            MAX_MAX_DELIVERY_ATTEMPTS,
            cfg.max_delivery_attempts,
        ));
    }
    if !(MIN_RETRY_BACKOFF_MS..=MAX_RETRY_BACKOFF_MS).contains(&cfg.retry_backoff_ms) {
        return Err(out_of_range(
            "retry_backoff_ms",
            MIN_RETRY_BACKOFF_MS,
            MAX_RETRY_BACKOFF_MS,
            cfg.retry_backoff_ms,
        ));
    }
    if !(MIN_MAX_INLINE_BYTES..=MAX_MAX_INLINE_BYTES).contains(&cfg.max_inline_bytes) {
        return Err(out_of_range(
            "max_inline_bytes",
            MIN_MAX_INLINE_BYTES,
            MAX_MAX_INLINE_BYTES,
            cfg.max_inline_bytes,
        ));
    }
    if let DurabilityPolicy::FsyncInterval { ms } = cfg.durability {
        if !(MIN_FSYNC_INTERVAL_MS..=MAX_FSYNC_INTERVAL_MS).contains(&ms) {
            return Err(out_of_range(
                "durability (fsync_interval ms)",
                MIN_FSYNC_INTERVAL_MS,
                MAX_FSYNC_INTERVAL_MS,
                ms,
            ));
        }
    }
    Ok(())
}

/// Fail-closed rejection of `idempotency_key`: the field
/// is a CEL expression per PLAN §3.1, evaluated against a record body —
/// that evaluator (`flow_engine/expr.rs` integration) does not exist yet.
/// M1 initially substituted the record's routing `key` bytes for it
/// instead, which is a silent semantics change: a topic keyed by e.g.
/// `patient_id` (a reasonable partitioning choice) with `idempotency_key`
/// enabled would silently drop every record after the first one for that
/// patient within the dedup window. Rejecting outright until the real CEL
/// evaluator lands is the safe direction — a caller gets a loud error
/// instead of silent data loss.
fn reject_idempotency_key(opts: &TopicOptions) -> Result<(), BusServiceError> {
    if opts.idempotency_key.is_some() {
        return Err(BusServiceError::InvalidTopicConfig {
            reason: "idempotency_key requires the CEL evaluator, not available yet".to_string(),
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryMode {
    AtLeastOnce,
    FireAndForget,
}

impl DeliveryMode {
    pub fn as_str(self) -> &'static str {
        match self {
            DeliveryMode::AtLeastOnce => "at_least_once",
            DeliveryMode::FireAndForget => "fire_and_forget",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "at_least_once" => Some(DeliveryMode::AtLeastOnce),
            "fire_and_forget" => Some(DeliveryMode::FireAndForget),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanupPolicy {
    Delete,
    /// M5 per PLAN §2.5 — accepted as a config value now (so a topic never
    /// needs a config migration to opt in later) but not implemented by
    /// `retention.rs`, which only ever deletes whole segments.
    Compact,
}

impl CleanupPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            CleanupPolicy::Delete => "delete",
            CleanupPolicy::Compact => "compact",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "delete" => Some(CleanupPolicy::Delete),
            "compact" => Some(CleanupPolicy::Compact),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidationMode {
    Off,
    Warn,
    Dlq,
}

impl ValidationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ValidationMode::Off => "off",
            ValidationMode::Warn => "warn",
            ValidationMode::Dlq => "dlq",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "off" => Some(ValidationMode::Off),
            "warn" => Some(ValidationMode::Warn),
            "dlq" => Some(ValidationMode::Dlq),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acks {
    Leader,
    Quorum,
    All,
}

impl Acks {
    pub fn as_str(self) -> &'static str {
        match self {
            Acks::Leader => "leader",
            Acks::Quorum => "quorum",
            Acks::All => "all",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "leader" => Some(Acks::Leader),
            "quorum" => Some(Acks::Quorum),
            "all" => Some(Acks::All),
            _ => None,
        }
    }
    /// PLAN §7.1: `quorum` when `replication_factor >= 3`, else `leader`
    /// (the only sensible value for RF<3 — replication itself is M2 scope,
    /// but the default is decided here so a topic created in M1 already
    /// carries the right value for when M2 wires ack-gating).
    pub fn default_for_rf(replication_factor: u32) -> Self {
        if replication_factor >= 3 {
            Acks::Quorum
        } else {
            Acks::Leader
        }
    }
}

/// Shortest/longest `ms` accepted by `DurabilityPolicy::FsyncInterval`
/// (owner decision B): the writer thread
/// fsyncs at most once per interval and an ACK returns after the write
/// lands, not after that fsync, so an interval longer than ~1s would widen
/// the crash-loss window past what "durable-ish" should mean, while `0`
/// would just be `FsyncBatch` under a different name.
pub const MIN_FSYNC_INTERVAL_MS: u32 = 1;
pub const MAX_FSYNC_INTERVAL_MS: u32 = 1_000;

/// Interval `DurabilityClass::Standard` resolves to in Prod/Test (owner
/// decision B) — also what every DLQ topic resolves to in Test/Prod, since
/// `dlq.rs::dlq_topic_options` pins every DLQ to this same `Standard` class
/// regardless of its source topic's own class (see that function's doc for
/// why).
pub const STANDARD_FSYNC_INTERVAL_MS: u32 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurabilityPolicy {
    Os,
    FsyncBatch,
    /// macOS `F_FULLFSYNC`: measured cost is approximately equal to
    /// `FsyncBatch`'s `sync_data` on Apple Silicon, so this is the
    /// `DurabilityClass::Critical` policy in every environment.
    FsyncBatchFull,
    /// Writer thread fsyncs at most once per `ms` (`tentaflow_bus::
    /// Durability::FsyncInterval`, see `to_engine`) instead of after every
    /// group — an ACK still returns as soon as the write itself lands, not
    /// after that fsync. Owner decision B's `DurabilityClass::Standard`
    /// policy in Prod/Test (`STANDARD_FSYNC_INTERVAL_MS` = 50) and the
    /// fixed policy every DLQ topic gets (`dlq.rs`). Wire form
    /// `fsync_interval:<ms>`; `ms` is range-checked against
    /// `MIN_FSYNC_INTERVAL_MS..=MAX_FSYNC_INTERVAL_MS` by `validate_ranges`.
    FsyncInterval {
        ms: u32,
    },
}

impl DurabilityPolicy {
    /// Wire/DB string form. Owned `String` (not `&'static str`) because
    /// `FsyncInterval`'s `ms` is dynamic — every other variant still
    /// allocates nothing but a fixed literal.
    pub fn to_wire_string(self) -> String {
        match self {
            DurabilityPolicy::Os => "os".to_string(),
            DurabilityPolicy::FsyncBatch => "fsync_batch".to_string(),
            DurabilityPolicy::FsyncBatchFull => "fsync_batch_full".to_string(),
            DurabilityPolicy::FsyncInterval { ms } => format!("fsync_interval:{ms}"),
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "os" => Some(DurabilityPolicy::Os),
            "fsync_batch" => Some(DurabilityPolicy::FsyncBatch),
            "fsync_batch_full" => Some(DurabilityPolicy::FsyncBatchFull),
            _ => s
                .strip_prefix("fsync_interval:")
                .and_then(|ms| ms.parse::<u32>().ok())
                .map(|ms| DurabilityPolicy::FsyncInterval { ms }),
        }
    }
    pub fn to_engine(self) -> tentaflow_bus::Durability {
        match self {
            DurabilityPolicy::Os => tentaflow_bus::Durability::Os,
            DurabilityPolicy::FsyncBatch => tentaflow_bus::Durability::FsyncBatch,
            DurabilityPolicy::FsyncBatchFull => tentaflow_bus::Durability::FsyncBatchFull,
            DurabilityPolicy::FsyncInterval { ms } => tentaflow_bus::Durability::FsyncInterval(
                std::time::Duration::from_millis(ms as u64),
            ),
        }
    }
}

/// Coarse, environment-independent durability tier a topic asks for
/// (owner decision B). `TopicOptions::durability_class` is the friendly
/// knob most callers should set; `TopicOptions::durability` remains the
/// advanced escape hatch and, when both are given, wins outright (see
/// `TopicConfig::from_options`/`apply_options`) — a caller who names an
/// exact policy presumably means it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurabilityClass {
    /// Interval fsync (Prod/Test) or OS-flush (Dev) — the default.
    Standard,
    /// The strongest policy this node can offer in every environment
    /// (`FsyncBatchFull`), for topics whose data cannot tolerate the
    /// `FsyncInterval` crash-loss window at all.
    Critical,
}

impl DurabilityClass {
    pub fn as_str(self) -> &'static str {
        match self {
            DurabilityClass::Standard => "standard",
            DurabilityClass::Critical => "critical",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "standard" => Some(DurabilityClass::Standard),
            "critical" => Some(DurabilityClass::Critical),
            _ => None,
        }
    }
    /// Owner decision B's resolution table:
    ///
    /// | environment  | Standard                          | Critical         |
    /// |--------------|------------------------------------|-------------------|
    /// | Dev          | `Os`                                | `FsyncBatchFull`  |
    /// | Test / Prod  | `FsyncInterval{ms:50}`              | `FsyncBatchFull`  |
    pub fn resolve(self, env: NodeEnvironment) -> DurabilityPolicy {
        match (env, self) {
            (NodeEnvironment::Dev, DurabilityClass::Standard) => DurabilityPolicy::Os,
            (NodeEnvironment::Dev, DurabilityClass::Critical) => DurabilityPolicy::FsyncBatchFull,
            (NodeEnvironment::Test | NodeEnvironment::Prod, DurabilityClass::Standard) => {
                DurabilityPolicy::FsyncInterval {
                    ms: STANDARD_FSYNC_INTERVAL_MS,
                }
            }
            (NodeEnvironment::Test | NodeEnvironment::Prod, DurabilityClass::Critical) => {
                DurabilityPolicy::FsyncBatchFull
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionPolicy {
    Lz4,
    None,
}

impl CompressionPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            CompressionPolicy::Lz4 => "lz4",
            CompressionPolicy::None => "none",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "lz4" => Some(CompressionPolicy::Lz4),
            "none" => Some(CompressionPolicy::None),
            _ => None,
        }
    }
}

/// Full per-topic configuration (PLAN §7.1's table, one field each). `name`
/// and `org_id` are immutable after creation; everything else can be
/// changed via `update_topic` (partitions may only increase — enforced by
/// `TopicConfig::apply_options`).
#[derive(Debug, Clone)]
pub struct TopicConfig {
    pub name: String,
    pub org_id: String,
    pub partitions: u32,
    pub retention_ms: i64,
    pub retention_bytes_per_partition: i64,
    /// Read starting M5's compaction engine (`CleanupPolicy::Compact`'s own
    /// doc) — M1's `retention.rs` only ever deletes whole sealed segments
    /// regardless of this value; `compact` is accepted and persisted today
    /// so a topic never needs a config migration to opt in once M5 lands.
    pub cleanup_policy: CleanupPolicy,
    /// Read starting M3a's `bus_consume` flow block: `fire_and_forget` is
    /// meant to skip offset tracking, ACKs, and DLQ entirely (PLAN §3.1). M1
    /// always treats every topic as `at_least_once` end-to-end regardless
    /// of this value — `publish`/`open_consumer`/`fetch` never read it.
    pub delivery: DeliveryMode,
    /// CEL expression selecting the per-record idempotency key (PLAN §3.1
    /// layer 2). Evaluating CEL against a record is `flow_engine/expr.rs`
    /// territory (out of this task's file ownership), so `create_topic`/
    /// `update_topic` fail closed and REJECT any attempt to set this field
    /// (`reject_idempotency_key`) until that evaluator
    /// exists — no real caller can produce a `Some` here today. `bus::mod`'s
    /// `publish()` still HAS a working dedup path keyed on this field
    /// (using each record's `key` bytes directly, a documented placeholder
    /// for the eventual CEL result), reachable only by constructing a
    /// `TopicConfig` directly, e.g. via the `#[cfg(test)]` helper below —
    /// that is deliberate: it lets the dedup mechanics be exercised now
    /// without the semantics-changing bug the placeholder would cause if
    /// it were reachable through the public admin API — substituting the
    /// record's PARTITIONING key for the idempotency key would silently
    /// drop every record after the first one sharing that key within the
    /// dedup window.
    pub idempotency_key: Option<String>,
    pub dedup_window_ms: i64,
    pub max_delivery_attempts: u32,
    pub retry_backoff_ms: u32,
    /// F3 (SUM/tentabus/PLAN-F3.md §3): the SUBJECT NAME of a registered
    /// `bus::schema_registry` entry. `Some(non-empty)` after `create_topic`/
    /// `update_topic` only when `apply_schema_binding_guard` accepted it —
    /// subject must exist, not be deprecated, and its type must match this
    /// topic's resolved `PayloadFormat`. `Some("")`/`None` means "no schema
    /// bound"; a call that clears it also forces `validation` to `Off` (see
    /// `apply_schema_binding_guard`'s doc). Pre-F3 rows may carry arbitrary
    /// free text here (validation was never wired, so nothing ever checked
    /// it) — the guard only fires on a call that EXPLICITLY touches
    /// `schema_id`/`validation`, so such a row is left alone by an unrelated
    /// update (legacy tolerance, PLAN-F3 §3 rule 5).
    pub schema_id: Option<String>,
    /// F3: `warn`/`dlq` gate `BusService::publish` on the bound
    /// `schema_id` subject's compiled validator (`bus::schema_registry`,
    /// `publish`'s schema-validation block). `Off` (the default, and every
    /// pre-F3 row's value) is not evaluated at all — zero cost beyond the
    /// enum comparison. A binary schema type (`avro`/`protobuf`/`thrift`,
    /// no validator until F4) FORCES this back to `Off` regardless of what
    /// was requested when its subject is bound (`apply_schema_binding_guard`).
    pub validation: ValidationMode,
    pub content_type: String,
    /// PLAN §7.1 default is `min(3, healthy nodes in the same environment)`
    /// — meaningless in M1, which has no mesh/replication at all (M2
    /// scope). `from_options` defaults this to a hard `1` instead: M2 must
    /// treat that as the correct floor for a single-node deployment and
    /// wire the real `min(3, healthy nodes)` logic in ADDITION to it, not
    /// "fix" this back down to 1 thinking it was a placeholder bug.
    pub replication_factor: u32,
    /// Read starting M2's replication (`acks` gates when `high_watermark`
    /// advances past a leader-only append, PLAN §4.2) — M1 has no
    /// replication at all, so every batch's `high_watermark` already equals
    /// `log_end_offset` regardless of this value.
    pub acks: Acks,
    pub durability: DurabilityPolicy,
    /// v143 (`SUM/tentabus/KRYTYK-M1-R5.md` R5-1/R5-7): `Some(class)` when
    /// `durability` was last set by resolving that class
    /// (`DurabilityClass::resolve`); `None` when `durability` is an
    /// explicit override that bypassed class resolution entirely. Read via
    /// `durability_class()`/`durability_explicit()` below — those two
    /// accessors are what every caller outside this struct's own
    /// `from_options`/`apply_options` should use, not this field directly,
    /// since a pre-v143 row (backfilled by `db::migrations::
    /// bus_topics_add_durability_class_column`) always carries `Some`, but
    /// that migration explicitly could not tell a genuinely pre-existing
    /// explicit override apart from a class-derived one (documented in its
    /// own doc comment).
    pub durability_class: Option<DurabilityClass>,
    pub max_inline_bytes: usize,
    pub compression: CompressionPolicy,
    /// Z12 fencing (PLAN §4.4 item 1, task item 3): stamped at creation from
    /// `services::environment::get_node_environment`. M1 only carries the
    /// stamp; replication fencing that actually enforces it is M2.
    pub environment: NodeEnvironment,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Partial overrides for `create_topic`/`update_topic`; unset fields fall
/// back to PLAN §7.1 defaults on create, or are left unchanged on update.
#[derive(Debug, Clone, Default)]
pub struct TopicOptions {
    pub partitions: Option<u32>,
    pub retention_ms: Option<i64>,
    pub retention_bytes_per_partition: Option<i64>,
    pub cleanup_policy: Option<CleanupPolicy>,
    pub delivery: Option<DeliveryMode>,
    pub idempotency_key: Option<String>,
    pub dedup_window_ms: Option<i64>,
    pub max_delivery_attempts: Option<u32>,
    pub retry_backoff_ms: Option<u32>,
    pub schema_id: Option<String>,
    pub validation: Option<ValidationMode>,
    pub content_type: Option<String>,
    pub replication_factor: Option<u32>,
    pub acks: Option<Acks>,
    /// Advanced override: an explicit policy always wins over
    /// `durability_class` below when both are set (owner decision B).
    pub durability: Option<DurabilityPolicy>,
    /// Friendly durability tier; resolved to a concrete `DurabilityPolicy`
    /// per `DurabilityClass::resolve` when `durability` itself is unset.
    /// Defaults to `Standard` (owner decision B).
    pub durability_class: Option<DurabilityClass>,
    /// Wire sentinel `durability: "auto"` (`dispatch/bus.rs`'s
    /// `topic_options_from_wire` — never itself a valid `DurabilityPolicy`
    /// string, so it is stripped out of `durability` above and turned into
    /// this flag instead). On `update_topic`, clears whatever explicit
    /// override is currently stored and resolves the policy from
    /// `durability_class` above (if also given in the SAME call) or
    /// otherwise from this topic's current EFFECTIVE class
    /// (`TopicConfig::durability_class()` — stored if present, else
    /// derived). A no-op on `create_topic`: a brand-new topic has no prior
    /// explicit override to clear, so it behaves exactly like leaving both
    /// `durability`/`durability_class` unset (falls through to the
    /// `Standard` default). Distinct from leaving every durability field
    /// unset, which is a true no-op update — this is the caller
    /// explicitly asking "stop overriding, go back to whatever my class
    /// says", which needs its own signal since `Option::None` already
    /// means "leave unchanged" everywhere else in this struct.
    pub durability_reset_to_class: bool,
    pub max_inline_bytes: Option<usize>,
    pub compression: Option<CompressionPolicy>,
}

impl TopicConfig {
    fn from_options(
        org_id: &str,
        name: &str,
        opts: TopicOptions,
        environment: NodeEnvironment,
        now_ms: i64,
    ) -> Result<Self, BusServiceError> {
        let partitions = opts.partitions.unwrap_or(DEFAULT_PARTITIONS);
        if !(MIN_PARTITIONS..=MAX_PARTITIONS).contains(&partitions) {
            return Err(BusServiceError::InvalidTopicConfig {
                reason: format!(
                    "partitions must be {MIN_PARTITIONS}-{MAX_PARTITIONS}, got {partitions}"
                ),
            });
        }
        let replication_factor = opts.replication_factor.unwrap_or(1).clamp(1, 7);
        // Owner decision B: an explicit `durability` always wins (and, v143,
        // leaves `durability_class` unset — no class was resolved to reach
        // it); otherwise resolve `durability_class` (defaulting to
        // `Standard`) against `environment` via `DurabilityClass::resolve`'s
        // table and persist which class produced it.
        let (durability, durability_class) = match opts.durability {
            Some(policy) => (policy, None),
            None => {
                let class = opts.durability_class.unwrap_or(DurabilityClass::Standard);
                (class.resolve(environment), Some(class))
            }
        };
        let cfg = Self {
            name: name.to_string(),
            org_id: org_id.to_string(),
            partitions,
            retention_ms: opts.retention_ms.unwrap_or(DEFAULT_RETENTION_MS),
            retention_bytes_per_partition: opts
                .retention_bytes_per_partition
                .unwrap_or(DEFAULT_RETENTION_BYTES_PER_PARTITION),
            cleanup_policy: opts.cleanup_policy.unwrap_or(CleanupPolicy::Delete),
            delivery: opts.delivery.unwrap_or(DeliveryMode::AtLeastOnce),
            idempotency_key: opts.idempotency_key,
            dedup_window_ms: opts.dedup_window_ms.unwrap_or(DEFAULT_DEDUP_WINDOW_MS),
            max_delivery_attempts: opts
                .max_delivery_attempts
                .unwrap_or(DEFAULT_MAX_DELIVERY_ATTEMPTS),
            retry_backoff_ms: opts.retry_backoff_ms.unwrap_or(DEFAULT_RETRY_BACKOFF_MS),
            schema_id: opts.schema_id,
            validation: opts.validation.unwrap_or(ValidationMode::Off),
            content_type: opts
                .content_type
                .unwrap_or_else(|| DEFAULT_CONTENT_TYPE.to_string()),
            replication_factor,
            acks: opts
                .acks
                .unwrap_or(Acks::default_for_rf(replication_factor)),
            durability,
            durability_class,
            max_inline_bytes: opts.max_inline_bytes.unwrap_or(DEFAULT_MAX_INLINE_BYTES),
            compression: opts.compression.unwrap_or(CompressionPolicy::Lz4),
            environment,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        };
        validate_ranges(&cfg)?;
        Ok(cfg)
    }

    /// Applies an update, enforcing PLAN §7.1's "partitions may grow, never
    /// shrink" rule (increasing partition count changes the topic's
    /// distribution but not its identity; shrinking would strand keys
    /// already routed to the removed partitions).
    fn apply_options(&mut self, opts: TopicOptions) -> Result<(), BusServiceError> {
        if let Some(p) = opts.partitions {
            if p < self.partitions {
                return Err(BusServiceError::InvalidTopicConfig {
                    reason: format!(
                        "partitions can only increase (current {}, requested {p})",
                        self.partitions
                    ),
                });
            }
            if p > MAX_PARTITIONS {
                return Err(BusServiceError::InvalidTopicConfig {
                    reason: format!("partitions must be <= {MAX_PARTITIONS}, got {p}"),
                });
            }
            self.partitions = p;
        }
        if let Some(v) = opts.retention_ms {
            self.retention_ms = v;
        }
        if let Some(v) = opts.retention_bytes_per_partition {
            self.retention_bytes_per_partition = v;
        }
        if let Some(v) = opts.cleanup_policy {
            self.cleanup_policy = v;
        }
        if let Some(v) = opts.delivery {
            self.delivery = v;
        }
        if opts.idempotency_key.is_some() {
            self.idempotency_key = opts.idempotency_key;
        }
        if let Some(v) = opts.dedup_window_ms {
            self.dedup_window_ms = v;
        }
        if let Some(v) = opts.max_delivery_attempts {
            self.max_delivery_attempts = v;
        }
        if let Some(v) = opts.retry_backoff_ms {
            self.retry_backoff_ms = v;
        }
        if opts.schema_id.is_some() {
            self.schema_id = opts.schema_id;
        }
        if let Some(v) = opts.validation {
            self.validation = v;
        }
        if let Some(v) = opts.content_type {
            self.content_type = v;
        }
        if let Some(v) = opts.replication_factor {
            self.replication_factor = v.clamp(1, 7);
        }
        if let Some(v) = opts.acks {
            self.acks = v;
        }
        // Owner decision B (v143): an explicit `durability` wins outright
        // and clears the stored class — this update is no longer
        // class-derived. Otherwise, EITHER a `durability_class` given in
        // the SAME call OR the `durability_reset_to_class` wire sentinel
        // (`durability: "auto"`) resolves against this topic's own
        // `environment` and replaces the current policy: an update that
        // only names a class (or explicitly asks to go back to one) is
        // meant to move the topic to whatever that class currently
        // resolves to, not leave a stale explicit policy from an earlier
        // update in place. When only the reset sentinel is given (no class
        // in this same call), the EFFECTIVE class already in force
        // (`durability_class()` — stored if present, else derived from the
        // current policy family) is what gets re-resolved and persisted.
        if let Some(v) = opts.durability {
            self.durability = v;
            self.durability_class = None;
        } else if opts.durability_class.is_some() || opts.durability_reset_to_class {
            let class = opts
                .durability_class
                .unwrap_or_else(|| self.durability_class());
            self.durability = class.resolve(self.environment);
            self.durability_class = Some(class);
        }
        if let Some(v) = opts.max_inline_bytes {
            self.max_inline_bytes = v;
        }
        if let Some(v) = opts.compression {
            self.compression = v;
        }
        validate_ranges(self)?;
        Ok(())
    }

    /// Coarse durability class this topic's `durability` currently reflects
    /// (owner decision B). v143 persists this directly (`durability_class`
    /// column) whenever `durability` was set by resolving a class rather
    /// than by an explicit override — this accessor returns that STORED
    /// class when present. For a `None` (explicit-override) row, or a
    /// pre-v143 row the migration's own backfill could not confidently tell
    /// apart from a genuine explicit override, it falls back to the same
    /// policy-family derivation the pre-v143 code used: `Os`/`FsyncInterval`
    /// map to `Standard`, `FsyncBatch`/`FsyncBatchFull` map to `Critical`.
    /// `FsyncBatch` itself is never produced by `DurabilityClass::resolve`
    /// (only the advanced `TopicOptions::durability` override can select
    /// it), but it shares `FsyncBatchFull`'s "stronger than the Standard
    /// default" intent, so it is grouped with `Critical` in that fallback
    /// too. Use `durability_explicit()` to tell whether THIS particular
    /// value came from the stored column at all (i.e. whether an "(explicit
    /// policy)" UI label would be honest) rather than re-deriving that from
    /// this method's return value, which cannot distinguish "stored
    /// Standard" from "derived Standard".
    pub fn durability_class(&self) -> DurabilityClass {
        let derived = match self.durability {
            DurabilityPolicy::Os | DurabilityPolicy::FsyncInterval { .. } => {
                DurabilityClass::Standard
            }
            DurabilityPolicy::FsyncBatch | DurabilityPolicy::FsyncBatchFull => {
                DurabilityClass::Critical
            }
        };
        self.durability_class.unwrap_or(derived)
    }

    /// `true` iff `durability` is an explicit override — i.e. no class is
    /// currently STORED for this topic (`durability_class` field is
    /// `None`). A pre-v143 row is never `true` here: the v143 backfill
    /// (`db::migrations::bus_topics_add_durability_class_column`) always
    /// stamps `Some` for a row that predates the column, even for the
    /// handful that really were an intentional explicit override before
    /// v143 existed — that migration's own doc comment documents this as
    /// the accepted, undetectable gap.
    pub fn durability_explicit(&self) -> bool {
        self.durability_class.is_none()
    }
}

impl From<&TopicConfig> for DbBusTopic {
    fn from(c: &TopicConfig) -> Self {
        DbBusTopic {
            org_id: c.org_id.clone(),
            name: c.name.clone(),
            partitions: c.partitions,
            retention_ms: c.retention_ms,
            retention_bytes: c.retention_bytes_per_partition,
            cleanup_policy: c.cleanup_policy.as_str().to_string(),
            delivery: c.delivery.as_str().to_string(),
            idempotency_key: c.idempotency_key.clone(),
            dedup_window_ms: c.dedup_window_ms,
            max_delivery_attempts: c.max_delivery_attempts,
            retry_backoff_ms: c.retry_backoff_ms,
            schema_id: c.schema_id.clone(),
            validation: c.validation.as_str().to_string(),
            content_type: c.content_type.clone(),
            replication_factor: c.replication_factor,
            acks: c.acks.as_str().to_string(),
            durability: c.durability.to_wire_string(),
            durability_class: c.durability_class.map(|cl| cl.as_str().to_string()),
            max_inline_bytes: c.max_inline_bytes as i64,
            compression: c.compression.as_str().to_string(),
            environment: c.environment.as_str().to_string(),
            created_at_ms: c.created_at_ms,
            updated_at_ms: c.updated_at_ms,
        }
    }
}

impl TryFrom<DbBusTopic> for TopicConfig {
    type Error = BusServiceError;

    fn try_from(row: DbBusTopic) -> Result<Self, Self::Error> {
        let bad = |field: &'static str, value: &str| BusServiceError::CorruptTopicRow {
            name: row.name.clone(),
            field,
            value: value.to_string(),
        };
        Ok(TopicConfig {
            name: row.name.clone(),
            org_id: row.org_id,
            partitions: row.partitions,
            retention_ms: row.retention_ms,
            retention_bytes_per_partition: row.retention_bytes,
            cleanup_policy: CleanupPolicy::parse(&row.cleanup_policy)
                .ok_or_else(|| bad("cleanup_policy", &row.cleanup_policy))?,
            delivery: DeliveryMode::parse(&row.delivery)
                .ok_or_else(|| bad("delivery", &row.delivery))?,
            idempotency_key: row.idempotency_key,
            dedup_window_ms: row.dedup_window_ms,
            max_delivery_attempts: row.max_delivery_attempts,
            retry_backoff_ms: row.retry_backoff_ms,
            schema_id: row.schema_id,
            validation: ValidationMode::parse(&row.validation)
                .ok_or_else(|| bad("validation", &row.validation))?,
            content_type: row.content_type,
            replication_factor: row.replication_factor,
            acks: Acks::parse(&row.acks).ok_or_else(|| bad("acks", &row.acks))?,
            durability: DurabilityPolicy::parse(&row.durability)
                .ok_or_else(|| bad("durability", &row.durability))?,
            durability_class: row
                .durability_class
                .as_deref()
                .map(|s| DurabilityClass::parse(s).ok_or_else(|| bad("durability_class", s)))
                .transpose()?,
            max_inline_bytes: row.max_inline_bytes.max(0) as usize,
            compression: CompressionPolicy::parse(&row.compression)
                .ok_or_else(|| bad("compression", &row.compression))?,
            environment: NodeEnvironment::parse(&row.environment)
                .ok_or_else(|| bad("environment", &row.environment))?,
            created_at_ms: row.created_at_ms,
            updated_at_ms: row.updated_at_ms,
        })
    }
}

// ---- On-disk layout (PLAN §2.2) ----------------------------------------

/// `<bus_dir>/<org_id>/<topic>/`
pub fn topic_dir(bus_dir: &std::path::Path, org_id: &str, topic: &str) -> std::path::PathBuf {
    bus_dir.join(org_id).join(topic)
}

/// `<bus_dir>/<org_id>/<topic>/pNNNN/`
pub fn partition_dir(
    bus_dir: &std::path::Path,
    org_id: &str,
    topic: &str,
    partition: u32,
) -> std::path::PathBuf {
    topic_dir(bus_dir, org_id, topic).join(format!("p{partition:04}"))
}

// ---- SUM/tentabus/PLAN-F3.md §3: schema binding guards -----------------

/// Fires ONLY when this call explicitly sets `schema_id` or `validation`
/// (`schema_id_touched`/`validation_touched` — i.e. `TopicOptions.schema_id`/
/// `.validation` was `Some(_)`), matching PLAN-F3 §3 rule 5's legacy
/// tolerance: an update that leaves both untouched — including every pre-F3
/// row carrying free-text `schema_id` with `validation=off` — is completely
/// unaffected, no lookup, no cost.
///
/// `old_schema_id` is the topic's `schema_id` BEFORE this call's options
/// were merged into `cfg` — `None` for `create_topic` (no prior row).
///
/// Rules enforced once triggered (PLAN-F3 §3 rules 1-3; rule 4, the
/// content_type-while-bound guard, is unconditional and lives directly in
/// `update_topic` next to the pre-existing field-policy content_type guard
/// it mirrors — it is not gated by "touched" at all, same as that one):
///   1. An explicit non-empty `schema_id` must name a subject that exists
///      for this org and is not deprecated — but ONLY when the binding is
///      actually CHANGING (`cfg.schema_id` after merge differs from
///      `old_schema_id`) or enforcement is actually being requested
///      (`cfg.validation != Off`). A UI that always echoes back the
///      currently-selected `validation` value on every unrelated edit
///      (e.g. a retention change) must not turn "schema_id unchanged,
///      validation staying Off" into a hard failure just because a pre-F3
///      topic's free-text `schema_id` was never a registered subject —
///      that combination is exactly the legacy tolerance this guard exists
///      to preserve, and it can only be told apart from a real re-binding
///      by comparing against the OLD value, not by "was the field present
///      on the wire".
///   2. The subject's `schema_type` must match this topic's resolved
///      `PayloadFormat`: `json_schema` only on `PayloadFormat::Json`.
///      `avro`/`protobuf`/`thrift` bind on ANY format (no format resolves
///      to them yet) but FORCE `cfg.validation` back to `Off` — silently,
///      not rejected, so an integrator can stage a schema ahead of F4.
///   3. `validation != Off` requires a bound, non-empty `schema_id` whose
///      type actually has a validator in this build
///      (`SchemaType::has_validator`) — rejected otherwise. An explicit
///      clear (`schema_id: Some("")`) instead SILENTLY forces `validation`
///      back to `Off` rather than erroring, even if this same call did not
///      touch `validation` at all: leaving a stale non-`Off` value with no
///      bound subject would be a dangling, unenforceable config.
fn apply_schema_binding_guard(
    db: &DbPool,
    org_id: &str,
    cfg: &mut TopicConfig,
    schema_id_touched: bool,
    validation_touched: bool,
    old_schema_id: Option<&str>,
) -> Result<(), BusServiceError> {
    if !schema_id_touched && !validation_touched {
        return Ok(());
    }
    let effective_subject = cfg.schema_id.as_deref().filter(|s| !s.is_empty());
    let Some(subject_name) = effective_subject else {
        if schema_id_touched {
            // Explicit clear: no bound subject left, so no validation can
            // ever run against it — force, don't reject.
            cfg.validation = ValidationMode::Off;
            return Ok(());
        }
        if cfg.validation != ValidationMode::Off {
            return Err(BusServiceError::InvalidTopicConfig {
                reason: "validation cannot be enabled without a bound schema subject \
                         (set schema_id first)"
                    .to_string(),
            });
        }
        return Ok(());
    };

    // Review finding #6: a broker-internal `__`-reserved topic (in
    // practice, always `__dlq.<topic>` — nothing else auto-creates one)
    // must never carry a schema binding. `validate_user_topic_name`
    // already keeps `create_topic` from ever reaching a `__`-prefixed name,
    // but `update_topic` operates on topics that already exist, including
    // internal ones `ensure_dlq_topic` created — without this check an
    // admin could bind a schema straight onto a DLQ topic, and a
    // subsequent `dlq`-mode violation on that DLQ's OWN source topic would
    // recurse into `__dlq.__dlq.<topic>` the moment this same guard's
    // enforcement kicked in for the DLQ topic itself.
    if cfg.name.starts_with(RESERVED_PREFIX) {
        return Err(BusServiceError::InvalidTopicConfig {
            reason: format!(
                "topic '{}' is a broker-internal reserved topic and cannot have a schema bound \
                 to it",
                cfg.name
            ),
        });
    }

    let old_subject = old_schema_id.filter(|s| !s.is_empty());
    let binding_changed = Some(subject_name) != old_subject;
    let enforcement_requested = cfg.validation != ValidationMode::Off;
    if !binding_changed && !enforcement_requested {
        // Same binding as before (or none at all pre-existed on create,
        // which is impossible here since `subject_name` is `Some`), and
        // validation stays Off — a legacy free-text `schema_id` this build
        // cannot resolve is tolerated rather than rejected.
        return Ok(());
    }

    let row = repository::bus_schema_subject_get(db, org_id, subject_name)?.ok_or_else(|| {
        BusServiceError::InvalidTopicConfig {
            reason: format!("schema subject '{subject_name}' is not registered for this org"),
        }
    })?;
    if row.deprecated_at_ms.is_some() {
        return Err(BusServiceError::InvalidTopicConfig {
            reason: format!("schema subject '{subject_name}' is deprecated"),
        });
    }
    let schema_type =
        SchemaType::parse(&row.schema_type).ok_or_else(|| BusServiceError::InvalidTopicConfig {
            reason: format!(
                "schema subject '{subject_name}' has an unrecognized schema_type '{}'",
                row.schema_type
            ),
        })?;
    let format = PayloadFormat::from_content_type(&cfg.content_type);
    if schema_type == SchemaType::JsonSchema && format != PayloadFormat::Json {
        return Err(BusServiceError::InvalidTopicConfig {
            reason: format!(
                "schema subject '{subject_name}' is json_schema but this topic's content_type \
                 resolves to {}",
                format.as_str()
            ),
        });
    }
    if !schema_type.has_validator() {
        // avro/protobuf/thrift: binding is allowed, validation is not — but
        // whether that is a silent downgrade or a hard rejection depends on
        // WHAT this call actually asked for (review finding #5):
        //   - the call explicitly turned validation ON (or to any non-`Off`
        //     mode) for a type with no validator in this build: reject, the
        //     caller asked for enforcement that can never happen and
        //     silently ignoring that is a worse trap than an error;
        //   - only `schema_id` was set (validation untouched, or touched
        //     but explicitly `Off` already): force `Off` as before — an
        //     integrator staging a schema ahead of F4 is exactly the
        //     intended use, and a stale non-`Off` value carried over from a
        //     previous binding must not linger unenforceable.
        if validation_touched && cfg.validation != ValidationMode::Off {
            return Err(BusServiceError::InvalidTopicConfig {
                reason: format!(
                    "schema subject '{subject_name}' is {} which has no validator in this \
                     build; validation cannot be enabled for it",
                    schema_type.as_str()
                ),
            });
        }
        cfg.validation = ValidationMode::Off;
    }
    Ok(())
}

// ---- Lifecycle (SQLite via db/repository.rs bus_topic_* functions) -----

pub fn create_topic(
    db: &DbPool,
    org_id: &str,
    name: &str,
    opts: TopicOptions,
    environment: NodeEnvironment,
    now_ms: i64,
) -> Result<TopicConfig, BusServiceError> {
    validate_user_topic_name(name)?;
    reject_idempotency_key(&opts)?;
    if repository::bus_topic_get(db, org_id, name)?.is_some() {
        return Err(BusServiceError::TopicAlreadyExists {
            name: name.to_string(),
        });
    }
    let schema_id_touched = opts.schema_id.is_some();
    let validation_touched = opts.validation.is_some();
    let mut cfg = TopicConfig::from_options(org_id, name, opts, environment, now_ms)?;
    apply_schema_binding_guard(
        db,
        org_id,
        &mut cfg,
        schema_id_touched,
        validation_touched,
        None,
    )?;
    repository::bus_topic_create(db, &DbBusTopic::from(&cfg))?;
    Ok(cfg)
}

/// Internal variant for broker-owned topics (`__dlq.<topic>`), bypassing the
/// user regex but still going through the same defaulting/persistence path.
pub fn create_internal_topic(
    db: &DbPool,
    org_id: &str,
    name: &str,
    opts: TopicOptions,
    environment: NodeEnvironment,
    now_ms: i64,
) -> Result<TopicConfig, BusServiceError> {
    validate_internal_topic_name(name)?;
    if let Some(existing) = repository::bus_topic_get(db, org_id, name)? {
        return TopicConfig::try_from(existing);
    }
    let cfg = TopicConfig::from_options(org_id, name, opts, environment, now_ms)?;
    repository::bus_topic_create(db, &DbBusTopic::from(&cfg))?;
    Ok(cfg)
}

pub fn get_topic(
    db: &DbPool,
    org_id: &str,
    name: &str,
) -> Result<Option<TopicConfig>, BusServiceError> {
    match repository::bus_topic_get(db, org_id, name)? {
        Some(row) => Ok(Some(TopicConfig::try_from(row)?)),
        None => Ok(None),
    }
}

pub fn update_topic(
    db: &DbPool,
    org_id: &str,
    name: &str,
    opts: TopicOptions,
    now_ms: i64,
) -> Result<TopicConfig, BusServiceError> {
    reject_idempotency_key(&opts)?;
    let row = repository::bus_topic_get(db, org_id, name)?.ok_or_else(|| {
        BusServiceError::TopicNotFound {
            name: name.to_string(),
        }
    })?;
    let mut cfg = TopicConfig::try_from(row)?;
    let old_content_type = cfg.content_type.clone();
    let old_schema_id = cfg.schema_id.clone();
    let schema_id_touched = opts.schema_id.is_some();
    let validation_touched = opts.validation.is_some();
    cfg.apply_options(opts)?;
    // SUM/tentabus/POLITYKI-POL-FORMATY.md (F0): a field policy is
    // validated/projected against the payload format `content_type`
    // resolves to (`bus::field_policies`/`bus::payload_format`) — changing
    // it out from under an existing policy would silently start
    // interpreting the SAME policy against a different wire format
    // without anyone re-reviewing it. Reject rather than reinterpret;
    // deleting the topic's policies first is the explicit, auditable path.
    if cfg.content_type != old_content_type
        && !repository::bus_field_policy_list_for_topic(db, org_id, name)?.is_empty()
    {
        return Err(BusServiceError::InvalidTopicConfig {
            reason: format!(
                "cannot change content_type on topic '{name}' while field policies exist \
                 for it (would silently reinterpret them against a different payload \
                 format); delete its field policies first"
            ),
        });
    }
    // SUM/tentabus/PLAN-F3.md §3 rule 4: same "reinterpret without review"
    // hazard as the field-policy guard just above, for a bound schema
    // subject instead of a field policy — a `json_schema` validator (and,
    // once F4 lands, a binary codec) is compiled/interpreted against a
    // SPECIFIC wire format. Unconditional (not gated by
    // `schema_id_touched`/`validation_touched`): unbinding the schema first
    // is the explicit, auditable path, exactly like field policies.
    // Review finding #3: this guard used to fire on ANY non-empty
    // `schema_id`, including a pre-F3 topic's free-text value that was
    // never a registered subject (nothing ever checked it — validation was
    // never wired). That made `content_type` permanently unchangeable on
    // every legacy row carrying such a string, with no way to "unbind" a
    // binding that was never real. Gate on the subject actually
    // RESOLVING — only a genuinely registered subject can be
    // "reinterpreted against a different payload format" by a content_type
    // change; an unresolved free-text string has nothing to reinterpret.
    if cfg.content_type != old_content_type {
        if let Some(subject) = cfg.schema_id.as_deref().filter(|s| !s.is_empty()) {
            if repository::bus_schema_subject_get(db, org_id, subject)?.is_some() {
                return Err(BusServiceError::InvalidTopicConfig {
                    reason: format!(
                        "cannot change content_type on topic '{name}' while schema subject \
                         '{subject}' is bound to it; unbind the schema first"
                    ),
                });
            }
        }
    }
    apply_schema_binding_guard(
        db,
        org_id,
        &mut cfg,
        schema_id_touched,
        validation_touched,
        old_schema_id.as_deref(),
    )?;
    cfg.updated_at_ms = now_ms;
    repository::bus_topic_update(db, &DbBusTopic::from(&cfg))?;
    Ok(cfg)
}

pub fn delete_topic(db: &DbPool, org_id: &str, name: &str) -> Result<(), BusServiceError> {
    if repository::bus_topic_get(db, org_id, name)?.is_none() {
        return Err(BusServiceError::TopicNotFound {
            name: name.to_string(),
        });
    }
    repository::bus_topic_delete(db, org_id, name)?;
    Ok(())
}

pub fn list_topics(db: &DbPool, org_id: &str) -> Result<Vec<TopicConfig>, BusServiceError> {
    repository::bus_topic_list(db, org_id)?
        .into_iter()
        .map(TopicConfig::try_from)
        .collect()
}

/// TEST-ONLY escape hatch: builds and persists a `TopicConfig` carrying
/// `idempotency_key`, bypassing `create_topic`'s fail-closed rejection of
/// that field . `bus::mod`'s publish() dedup path (layer
/// 2, `dedup.rs`) is real and load-bearing even though no production admin
/// call can reach it yet — this is how `bus::mod`'s tests get a topic
/// config into that state to exercise it, standing in for the eventual
/// CEL-backed caller.
#[cfg(test)]
pub(crate) fn create_topic_for_dedup_test(
    db: &DbPool,
    org_id: &str,
    name: &str,
    opts: TopicOptions,
    environment: NodeEnvironment,
    now_ms: i64,
) -> Result<TopicConfig, BusServiceError> {
    validate_user_topic_name(name)?;
    let cfg = TopicConfig::from_options(org_id, name, opts, environment, now_ms)?;
    repository::bus_topic_create(db, &DbBusTopic::from(&cfg))?;
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_topic_name_validation_matches_plan_regex() {
        assert!(validate_user_topic_name("orders.created").is_ok());
        assert!(validate_user_topic_name("a1").is_ok());
        assert!(validate_user_topic_name("cmc-wynik.v2").is_ok());

        assert!(
            validate_user_topic_name("a").is_err(),
            "too short (needs 2+)"
        );
        assert!(validate_user_topic_name("Orders").is_err(), "uppercase");
        assert!(
            validate_user_topic_name("_orders").is_err(),
            "leading underscore"
        );
        assert!(
            validate_user_topic_name("__dlq.orders").is_err(),
            "reserved prefix"
        );
        assert!(
            validate_user_topic_name("orders_created").is_err(),
            "underscore not in charset"
        );
        assert!(validate_user_topic_name("orders created").is_err(), "space");
    }

    #[test]
    fn internal_topic_name_requires_reserved_prefix() {
        assert!(validate_internal_topic_name("__dlq.orders.created").is_ok());
        assert!(validate_internal_topic_name("orders.created").is_err());
    }

    /// A user topic name must leave room for `dlq_topic_name`'s
    /// `__dlq.` prefix, or `note_delivery_failure` would fail with
    /// `InvalidTopicName` exactly when delivery attempts are exhausted.
    /// `MAX_TOPIC_NAME_LEN` is 127 and the prefix is 6 bytes, so 121 is the
    /// longest name that still leaves its DLQ topic at exactly 127 bytes.
    #[test]
    fn user_topic_name_leaves_room_for_the_dlq_prefix() {
        let at_boundary = "a".repeat(MAX_TOPIC_NAME_LEN - 6);
        assert_eq!(at_boundary.len(), 121);
        assert!(
            validate_user_topic_name(&at_boundary).is_ok(),
            "121 bytes: '__dlq.' + name is exactly 127 bytes, must be accepted"
        );
        assert_eq!(
            format!("__dlq.{at_boundary}").len(),
            MAX_TOPIC_NAME_LEN,
            "sanity: the resulting DLQ name is exactly at the internal-name ceiling"
        );

        let one_over = "a".repeat(MAX_TOPIC_NAME_LEN - 5);
        assert_eq!(one_over.len(), 122);
        let err = validate_user_topic_name(&one_over)
            .expect_err("122 bytes: '__dlq.' + name would be 128 bytes, must be rejected");
        assert!(matches!(err, BusServiceError::InvalidTopicName { .. }));
    }

    #[test]
    fn defaults_match_plan_7_1() {
        let cfg = TopicConfig::from_options(
            "org-1",
            "orders.created",
            TopicOptions::default(),
            NodeEnvironment::Prod,
            1_000,
        )
        .unwrap();
        assert_eq!(cfg.partitions, DEFAULT_PARTITIONS);
        assert_eq!(cfg.retention_ms, DEFAULT_RETENTION_MS);
        assert_eq!(
            cfg.retention_bytes_per_partition,
            DEFAULT_RETENTION_BYTES_PER_PARTITION
        );
        assert_eq!(cfg.cleanup_policy, CleanupPolicy::Delete);
        assert_eq!(cfg.delivery, DeliveryMode::AtLeastOnce);
        assert_eq!(cfg.max_delivery_attempts, DEFAULT_MAX_DELIVERY_ATTEMPTS);
        assert_eq!(cfg.validation, ValidationMode::Off);
        assert_eq!(cfg.replication_factor, 1);
        assert_eq!(cfg.acks, Acks::Leader, "RF=1 defaults to leader acks");
        // Owner decision B changed the Prod/Test default: an unset
        // `durability`/`durability_class` now resolves through
        // `DurabilityClass::Standard` (the default class) rather than
        // going straight to `FsyncBatchFull` — Prod gets
        // `FsyncInterval{ms: 50}` instead, trading the strongest
        // per-group barrier for a bounded-staleness interval fsync that
        // still never blocks an ACK on the fsync itself. `FsyncBatchFull`
        // is now reserved for `DurabilityClass::Critical`.
        assert_eq!(
            cfg.durability,
            DurabilityPolicy::FsyncInterval {
                ms: STANDARD_FSYNC_INTERVAL_MS
            },
            "Prod default is now DurabilityClass::Standard resolved (owner decision B)"
        );
        assert_eq!(cfg.durability_class(), DurabilityClass::Standard);
        assert_eq!(cfg.compression, CompressionPolicy::Lz4);
        assert_eq!(cfg.environment, NodeEnvironment::Prod);
    }

    #[test]
    fn dev_environment_defaults_to_os_durability() {
        let cfg = TopicConfig::from_options(
            "org-1",
            "telemetry.raw",
            TopicOptions::default(),
            NodeEnvironment::Dev,
            1_000,
        )
        .unwrap();
        assert_eq!(cfg.durability, DurabilityPolicy::Os);
        assert_eq!(cfg.durability_class(), DurabilityClass::Standard);
    }

    // ---- Owner decision B: DurabilityPolicy / DurabilityClass ---------

    #[test]
    fn durability_policy_string_round_trip() {
        let cases = [
            (DurabilityPolicy::Os, "os"),
            (DurabilityPolicy::FsyncBatch, "fsync_batch"),
            (DurabilityPolicy::FsyncBatchFull, "fsync_batch_full"),
            (
                DurabilityPolicy::FsyncInterval { ms: 50 },
                "fsync_interval:50",
            ),
            (
                DurabilityPolicy::FsyncInterval { ms: 1 },
                "fsync_interval:1",
            ),
            (
                DurabilityPolicy::FsyncInterval { ms: 1000 },
                "fsync_interval:1000",
            ),
        ];
        for (policy, wire) in cases {
            assert_eq!(policy.to_wire_string(), wire, "policy={policy:?}");
            assert_eq!(DurabilityPolicy::parse(wire), Some(policy), "wire={wire}");
        }
        assert_eq!(DurabilityPolicy::parse("bogus"), None);
        assert_eq!(DurabilityPolicy::parse("fsync_interval:"), None);
        assert_eq!(DurabilityPolicy::parse("fsync_interval:abc"), None);
    }

    #[test]
    fn durability_class_resolves_per_environment_table() {
        assert_eq!(
            DurabilityClass::Standard.resolve(NodeEnvironment::Dev),
            DurabilityPolicy::Os
        );
        assert_eq!(
            DurabilityClass::Critical.resolve(NodeEnvironment::Dev),
            DurabilityPolicy::FsyncBatchFull
        );
        for env in [NodeEnvironment::Test, NodeEnvironment::Prod] {
            assert_eq!(
                DurabilityClass::Standard.resolve(env),
                DurabilityPolicy::FsyncInterval {
                    ms: STANDARD_FSYNC_INTERVAL_MS
                },
                "env={env:?}"
            );
            assert_eq!(
                DurabilityClass::Critical.resolve(env),
                DurabilityPolicy::FsyncBatchFull,
                "env={env:?}"
            );
        }
    }

    #[test]
    fn explicit_durability_wins_over_durability_class_on_create() {
        let cfg = TopicConfig::from_options(
            "org-1",
            "orders.created",
            TopicOptions {
                durability: Some(DurabilityPolicy::Os),
                durability_class: Some(DurabilityClass::Critical),
                ..Default::default()
            },
            NodeEnvironment::Prod,
            1_000,
        )
        .unwrap();
        assert_eq!(
            cfg.durability,
            DurabilityPolicy::Os,
            "explicit durability must win over durability_class"
        );
    }

    #[test]
    fn explicit_durability_wins_over_durability_class_on_update() {
        let mut cfg = TopicConfig::from_options(
            "org-1",
            "orders.created",
            TopicOptions::default(),
            NodeEnvironment::Prod,
            1_000,
        )
        .unwrap();
        cfg.apply_options(TopicOptions {
            durability: Some(DurabilityPolicy::Os),
            durability_class: Some(DurabilityClass::Critical),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(cfg.durability, DurabilityPolicy::Os);
    }

    #[test]
    fn durability_class_alone_resolves_and_replaces_current_policy_on_update() {
        let mut cfg = TopicConfig::from_options(
            "org-1",
            "orders.created",
            TopicOptions {
                durability: Some(DurabilityPolicy::Os),
                ..Default::default()
            },
            NodeEnvironment::Prod,
            1_000,
        )
        .unwrap();
        assert_eq!(cfg.durability, DurabilityPolicy::Os);

        cfg.apply_options(TopicOptions {
            durability_class: Some(DurabilityClass::Critical),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(cfg.durability, DurabilityPolicy::FsyncBatchFull);
        assert_eq!(cfg.durability_class(), DurabilityClass::Critical);
    }

    // ---- v143: persisted `durability_class` / `durability_explicit` ----

    /// An explicit `durability` at creation stores NO class — the topic is
    /// explicit from the start.
    #[test]
    fn explicit_durability_on_create_leaves_no_stored_class() {
        let cfg = TopicConfig::from_options(
            "org-1",
            "orders.created",
            TopicOptions {
                durability: Some(DurabilityPolicy::Os),
                ..Default::default()
            },
            NodeEnvironment::Prod,
            1_000,
        )
        .unwrap();
        assert!(cfg.durability_explicit());
        assert_eq!(
            cfg.durability_class(),
            DurabilityClass::Standard,
            "still derivable for display"
        );
    }

    /// A class-only create stores that class and is NOT explicit.
    #[test]
    fn class_only_on_create_stores_the_class_and_is_not_explicit() {
        let cfg = TopicConfig::from_options(
            "org-1",
            "orders.created",
            TopicOptions {
                durability_class: Some(DurabilityClass::Critical),
                ..Default::default()
            },
            NodeEnvironment::Prod,
            1_000,
        )
        .unwrap();
        assert!(!cfg.durability_explicit());
        assert_eq!(cfg.durability_class(), DurabilityClass::Critical);
        assert_eq!(cfg.durability, DurabilityPolicy::FsyncBatchFull);
    }

    /// The R5 P1-2 case from the linked critique: editing a Critical topic
    /// down to Standard via the class radio ALONE (no explicit
    /// `durability` in the same call) must actually change the persisted
    /// policy, not silently no-op. Previously reproducible only because the
    /// UI's advanced field happened to prefill an explicit `durability`
    /// that then "won" and blocked the downgrade — this test exercises the
    /// backend contract directly, independent of that UI bug.
    #[test]
    fn class_only_update_actually_downgrades_critical_to_standard() {
        let mut cfg = TopicConfig::from_options(
            "org-1",
            "krytyk.std",
            TopicOptions {
                durability_class: Some(DurabilityClass::Critical),
                ..Default::default()
            },
            NodeEnvironment::Prod,
            1_000,
        )
        .unwrap();
        assert_eq!(cfg.durability, DurabilityPolicy::FsyncBatchFull);

        cfg.apply_options(TopicOptions {
            durability_class: Some(DurabilityClass::Standard),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(
            cfg.durability,
            DurabilityPolicy::FsyncInterval {
                ms: STANDARD_FSYNC_INTERVAL_MS
            },
            "class-only update must actually change the resolved policy"
        );
        assert_eq!(cfg.durability_class(), DurabilityClass::Standard);
        assert!(!cfg.durability_explicit());
    }

    /// Explicit `durability` on update clears any previously stored class.
    #[test]
    fn explicit_durability_on_update_clears_stored_class() {
        let mut cfg = TopicConfig::from_options(
            "org-1",
            "orders.created",
            TopicOptions {
                durability_class: Some(DurabilityClass::Critical),
                ..Default::default()
            },
            NodeEnvironment::Prod,
            1_000,
        )
        .unwrap();
        assert!(!cfg.durability_explicit());

        cfg.apply_options(TopicOptions {
            durability: Some(DurabilityPolicy::Os),
            ..Default::default()
        })
        .unwrap();
        assert!(cfg.durability_explicit());
        assert_eq!(cfg.durability, DurabilityPolicy::Os);
        // Display-only derivation still resolves a class from the family,
        // even though it is no longer the STORED source of truth.
        assert_eq!(cfg.durability_class(), DurabilityClass::Standard);
    }

    /// `durability_reset_to_class` (wire `durability: "auto"`) alone, with
    /// no `durability_class` in the same call, clears an explicit override
    /// and re-resolves from the topic's current EFFECTIVE class (derived
    /// from the explicit policy's own family, since nothing was stored).
    #[test]
    fn reset_to_class_alone_clears_explicit_and_resolves_current_effective_class() {
        let mut cfg = TopicConfig::from_options(
            "org-1",
            "orders.created",
            TopicOptions {
                durability: Some(DurabilityPolicy::FsyncBatchFull),
                ..Default::default()
            },
            NodeEnvironment::Prod,
            1_000,
        )
        .unwrap();
        assert!(cfg.durability_explicit());
        assert_eq!(cfg.durability_class(), DurabilityClass::Critical);

        cfg.apply_options(TopicOptions {
            durability_reset_to_class: true,
            ..Default::default()
        })
        .unwrap();
        assert!(!cfg.durability_explicit());
        assert_eq!(cfg.durability_class(), DurabilityClass::Critical);
        // Critical resolves to FsyncBatchFull in every environment, so the
        // wire value is unchanged here but is now class-derived, not
        // explicit — verified by `durability_explicit()` above, not by a
        // policy change (see the next test for a case where the policy
        // itself also changes).
        assert_eq!(cfg.durability, DurabilityPolicy::FsyncBatchFull);
    }

    /// `durability_reset_to_class` combined with a `durability_class` in
    /// the SAME call resolves against the GIVEN class, not the topic's
    /// prior one.
    #[test]
    fn reset_to_class_with_explicit_class_resolves_the_given_class() {
        let mut cfg = TopicConfig::from_options(
            "org-1",
            "orders.created",
            TopicOptions {
                durability: Some(DurabilityPolicy::FsyncBatchFull),
                ..Default::default()
            },
            NodeEnvironment::Prod,
            1_000,
        )
        .unwrap();

        cfg.apply_options(TopicOptions {
            durability_reset_to_class: true,
            durability_class: Some(DurabilityClass::Standard),
            ..Default::default()
        })
        .unwrap();
        assert!(!cfg.durability_explicit());
        assert_eq!(cfg.durability_class(), DurabilityClass::Standard);
        assert_eq!(
            cfg.durability,
            DurabilityPolicy::FsyncInterval {
                ms: STANDARD_FSYNC_INTERVAL_MS
            }
        );
    }

    /// Leaving every durability field unset on update is a true no-op —
    /// `durability_reset_to_class: false` (the default) must not itself
    /// trigger any resolution.
    #[test]
    fn no_durability_fields_on_update_is_a_true_no_op() {
        let mut cfg = TopicConfig::from_options(
            "org-1",
            "orders.created",
            TopicOptions {
                durability: Some(DurabilityPolicy::Os),
                ..Default::default()
            },
            NodeEnvironment::Prod,
            1_000,
        )
        .unwrap();
        cfg.apply_options(TopicOptions {
            partitions: Some(16),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(cfg.durability, DurabilityPolicy::Os);
        assert!(cfg.durability_explicit());
    }

    /// `dlq::dlq_topic_options`'s pinned `Standard` class resolves through
    /// the SAME per-environment table as any other Standard topic once it
    /// reaches `TopicConfig::from_options` — Test/Prod get the fixed
    /// interval policy, Dev gets `Os` — and stays class-derived
    /// (`durability_explicit() == false`), not an explicit override. Lives
    /// here (not `dlq.rs`) because `from_options` is private to this
    /// module.
    #[test]
    fn dlq_topic_options_resolves_per_environment_and_stays_class_derived() {
        fn resolved(env: NodeEnvironment) -> TopicConfig {
            let source =
                TopicConfig::from_options("org-1", "orders.created", Default::default(), env, 0)
                    .unwrap();
            let opts = super::super::dlq::dlq_topic_options(&source);
            TopicConfig::from_options("org-1", "__dlq.orders.created", opts, env, 0).unwrap()
        }

        let prod = resolved(NodeEnvironment::Prod);
        assert_eq!(
            prod.durability,
            DurabilityPolicy::FsyncInterval {
                ms: STANDARD_FSYNC_INTERVAL_MS
            }
        );
        assert_eq!(prod.durability_class(), DurabilityClass::Standard);
        assert!(!prod.durability_explicit());

        let dev = resolved(NodeEnvironment::Dev);
        assert_eq!(dev.durability, DurabilityPolicy::Os);
        assert_eq!(dev.durability_class(), DurabilityClass::Standard);
        assert!(!dev.durability_explicit());
    }

    #[test]
    fn fsync_interval_ms_out_of_range_rejected_boundary_accepted() {
        let build_with = |ms: u32| {
            TopicConfig::from_options(
                "org-1",
                "t.interval",
                TopicOptions {
                    durability: Some(DurabilityPolicy::FsyncInterval { ms }),
                    ..Default::default()
                },
                NodeEnvironment::Prod,
                1_000,
            )
        };
        assert!(build_with(0).is_err(), "0ms below MIN_FSYNC_INTERVAL_MS");
        assert!(
            build_with(MAX_FSYNC_INTERVAL_MS + 1).is_err(),
            "above MAX_FSYNC_INTERVAL_MS"
        );
        assert!(build_with(MIN_FSYNC_INTERVAL_MS).is_ok());
        assert!(build_with(MAX_FSYNC_INTERVAL_MS).is_ok());
    }

    #[test]
    fn rf3_defaults_to_quorum_acks() {
        let cfg = TopicConfig::from_options(
            "org-1",
            "results.final",
            TopicOptions {
                replication_factor: Some(3),
                ..Default::default()
            },
            NodeEnvironment::Prod,
            1_000,
        )
        .unwrap();
        assert_eq!(cfg.acks, Acks::Quorum);
    }

    #[test]
    fn partitions_can_grow_but_not_shrink() {
        let mut cfg = TopicConfig::from_options(
            "org-1",
            "orders.created",
            TopicOptions {
                partitions: Some(8),
                ..Default::default()
            },
            NodeEnvironment::Prod,
            1_000,
        )
        .unwrap();
        cfg.apply_options(TopicOptions {
            partitions: Some(16),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(cfg.partitions, 16);

        let err = cfg
            .apply_options(TopicOptions {
                partitions: Some(4),
                ..Default::default()
            })
            .unwrap_err();
        assert!(matches!(err, BusServiceError::InvalidTopicConfig { .. }));
    }

    #[test]
    fn partition_dir_layout_matches_plan_2_2() {
        let bus_dir = std::path::Path::new("/var/tentaflow/bus");
        assert_eq!(
            partition_dir(bus_dir, "org-1", "orders.created", 3),
            std::path::PathBuf::from("/var/tentaflow/bus/org-1/orders.created/p0003")
        );
    }

    fn build(opts: TopicOptions) -> Result<TopicConfig, BusServiceError> {
        TopicConfig::from_options("org-1", "t.range", opts, NodeEnvironment::Prod, 1_000)
    }

    // ---- PLAN §7.1 numeric ranges -------------------------------

    #[test]
    fn retention_ms_out_of_range_rejected_boundary_accepted() {
        assert!(build(TopicOptions {
            retention_ms: Some(MIN_RETENTION_MS - 1),
            ..Default::default()
        })
        .is_err());
        assert!(build(TopicOptions {
            retention_ms: Some(MAX_RETENTION_MS + 1),
            ..Default::default()
        })
        .is_err());
        assert!(build(TopicOptions {
            retention_ms: Some(MIN_RETENTION_MS),
            ..Default::default()
        })
        .is_ok());
        assert!(build(TopicOptions {
            retention_ms: Some(MAX_RETENTION_MS),
            ..Default::default()
        })
        .is_ok());
    }

    #[test]
    fn retention_bytes_out_of_range_rejected_boundary_accepted() {
        assert!(build(TopicOptions {
            retention_bytes_per_partition: Some(MIN_RETENTION_BYTES_PER_PARTITION - 1),
            ..Default::default()
        })
        .is_err());
        assert!(build(TopicOptions {
            retention_bytes_per_partition: Some(MAX_RETENTION_BYTES_PER_PARTITION + 1),
            ..Default::default()
        })
        .is_err());
        assert!(build(TopicOptions {
            retention_bytes_per_partition: Some(MIN_RETENTION_BYTES_PER_PARTITION),
            ..Default::default()
        })
        .is_ok());
        assert!(build(TopicOptions {
            retention_bytes_per_partition: Some(MAX_RETENTION_BYTES_PER_PARTITION),
            ..Default::default()
        })
        .is_ok());
    }

    #[test]
    fn dedup_window_out_of_range_rejected_boundary_accepted() {
        // Negative/zero would otherwise make every record look `Fresh`
        // forever (dedup silently disabled) — the specific example.
        assert!(build(TopicOptions {
            dedup_window_ms: Some(-1),
            ..Default::default()
        })
        .is_err());
        assert!(build(TopicOptions {
            dedup_window_ms: Some(MIN_DEDUP_WINDOW_MS - 1),
            ..Default::default()
        })
        .is_err());
        assert!(build(TopicOptions {
            dedup_window_ms: Some(MAX_DEDUP_WINDOW_MS + 1),
            ..Default::default()
        })
        .is_err());
        assert!(build(TopicOptions {
            dedup_window_ms: Some(MIN_DEDUP_WINDOW_MS),
            ..Default::default()
        })
        .is_ok());
        assert!(build(TopicOptions {
            dedup_window_ms: Some(MAX_DEDUP_WINDOW_MS),
            ..Default::default()
        })
        .is_ok());
    }

    #[test]
    fn max_delivery_attempts_out_of_range_rejected_boundary_accepted() {
        // 0 would otherwise send every failure straight to the DLQ on the
        // very first delivery attempt — the specific example.
        assert!(build(TopicOptions {
            max_delivery_attempts: Some(0),
            ..Default::default()
        })
        .is_err());
        assert!(build(TopicOptions {
            max_delivery_attempts: Some(MAX_MAX_DELIVERY_ATTEMPTS + 1),
            ..Default::default()
        })
        .is_err());
        assert!(build(TopicOptions {
            max_delivery_attempts: Some(MIN_MAX_DELIVERY_ATTEMPTS),
            ..Default::default()
        })
        .is_ok());
        assert!(build(TopicOptions {
            max_delivery_attempts: Some(MAX_MAX_DELIVERY_ATTEMPTS),
            ..Default::default()
        })
        .is_ok());
    }

    #[test]
    fn retry_backoff_out_of_range_rejected_boundary_accepted() {
        assert!(build(TopicOptions {
            retry_backoff_ms: Some(MIN_RETRY_BACKOFF_MS - 1),
            ..Default::default()
        })
        .is_err());
        assert!(build(TopicOptions {
            retry_backoff_ms: Some(MAX_RETRY_BACKOFF_MS + 1),
            ..Default::default()
        })
        .is_err());
        assert!(build(TopicOptions {
            retry_backoff_ms: Some(MIN_RETRY_BACKOFF_MS),
            ..Default::default()
        })
        .is_ok());
        assert!(build(TopicOptions {
            retry_backoff_ms: Some(MAX_RETRY_BACKOFF_MS),
            ..Default::default()
        })
        .is_ok());
    }

    #[test]
    fn max_inline_bytes_out_of_range_rejected_boundary_accepted() {
        assert!(build(TopicOptions {
            max_inline_bytes: Some(MIN_MAX_INLINE_BYTES - 1),
            ..Default::default()
        })
        .is_err());
        assert!(build(TopicOptions {
            max_inline_bytes: Some(MAX_MAX_INLINE_BYTES + 1),
            ..Default::default()
        })
        .is_err());
        assert!(build(TopicOptions {
            max_inline_bytes: Some(MIN_MAX_INLINE_BYTES),
            ..Default::default()
        })
        .is_ok());
        assert!(build(TopicOptions {
            max_inline_bytes: Some(MAX_MAX_INLINE_BYTES),
            ..Default::default()
        })
        .is_ok());
    }

    #[test]
    fn apply_options_also_enforces_ranges() {
        let mut cfg = build(TopicOptions::default()).unwrap();
        let err = cfg
            .apply_options(TopicOptions {
                max_delivery_attempts: Some(0),
                ..Default::default()
            })
            .unwrap_err();
        assert!(matches!(err, BusServiceError::InvalidTopicConfig { .. }));
    }

    // ---- idempotency_key fail-closed ------------------

    #[test]
    fn create_topic_rejects_idempotency_key() {
        let err = reject_idempotency_key(&TopicOptions {
            idempotency_key: Some("msg.run_id".to_string()),
            ..Default::default()
        })
        .unwrap_err();
        assert!(matches!(err, BusServiceError::InvalidTopicConfig { .. }));
    }

    #[test]
    fn update_topic_rejects_idempotency_key_but_none_is_a_no_op() {
        assert!(reject_idempotency_key(&TopicOptions {
            idempotency_key: Some("msg.run_id".to_string()),
            ..Default::default()
        })
        .is_err());
        // `None` means "leave unchanged" on update — must NOT be rejected.
        assert!(reject_idempotency_key(&TopicOptions::default()).is_ok());
    }

    // ---- validate_org_id --------------------

    #[test]
    fn validate_org_id_accepts_a_normal_id() {
        assert!(validate_org_id("org-1").is_ok());
        assert!(validate_org_id("acme.corp").is_ok());
        assert!(validate_org_id("a").is_ok());
    }

    #[test]
    fn validate_org_id_rejects_empty_and_too_long() {
        assert!(validate_org_id("").is_err());
        assert!(validate_org_id(&"a".repeat(MAX_ORG_ID_LEN)).is_ok());
        assert!(validate_org_id(&"a".repeat(MAX_ORG_ID_LEN + 1)).is_err());
    }

    #[test]
    fn validate_org_id_rejects_meta_and_any_leading_underscore() {
        let err = validate_org_id("_meta").unwrap_err();
        assert!(matches!(err, BusServiceError::InvalidArgument(_)));
        assert!(validate_org_id("_anything").is_err());
    }

    /// `bus_dir.join(org_id)` must never be able to escape `bus_dir` —
    /// neither via a path separator nor via a bare `..` component, which
    /// is expressible using only characters `is_name_char` otherwise
    /// allows (two literal dots).
    #[test]
    fn validate_org_id_rejects_path_traversal_and_absolute_paths() {
        assert!(validate_org_id("../x").is_err(), "traversal via '/'");
        assert!(validate_org_id("/abs").is_err(), "absolute path");
        assert!(validate_org_id("..").is_err(), "bare parent-dir component");
        assert!(
            validate_org_id("org/../other").is_err(),
            "traversal embedded past the first segment"
        );
    }

    #[test]
    fn validate_org_id_rejects_charset_violations() {
        assert!(validate_org_id("Org-1").is_err(), "uppercase");
        assert!(validate_org_id("org_1").is_err(), "underscore mid-string");
        assert!(validate_org_id("org 1").is_err(), "space");
    }
}
