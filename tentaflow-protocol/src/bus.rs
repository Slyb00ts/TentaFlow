// =============================================================================
// File: bus.rs — TentaBus M1 wire protocol (SUM/tentabus/PLAN.md §6.2)
// =============================================================================
//
// One `MessageBody::BusBody(BusPayload)` variant carries the whole family
// (topics, groups, DLQ, ACL, quotas, message preview, stats) — same pack
// pattern as `BenchmarkPayload`/`EventsPayload`: `MessageBody` is tagged by
// variant NAME, not index, so this crate is free to add/append `BusPayload`
// variants without touching `SCHEMA_VERSION` again as long as they land at
// the END of this enum (ciborium re-derives the tag from the Rust variant
// name at both ends, so an insertion in the middle is still wire-safe, but
// append-only keeps every payload family in this crate consistent and easy
// to diff against PLAN.md).
//
// Every enum-ish topic/group/ACL field crosses the wire as a plain `String`
// (never a shared Rust enum) — `tentaflow-protocol` has no dependency on
// `tentaflow-core::bus`, and the strings already match the `as_str()`/
// `parse()` convention `tentaflow-core/src/bus/topics.rs` uses to persist
// the same fields to SQLite, so the dispatch layer's translation is a
// direct `parse()` call, not a bespoke mapping table.
//
// `Vec<u8>` fields use `#[serde(with = "serde_bytes")]` (CBOR byte string,
// one bulk copy) per this crate's existing convention (`project_studio.rs`,
// `mesh.rs`). An OPTIONAL byte field (record key) is modeled as a plain,
// possibly-empty `Vec<u8>` rather than `Option<Vec<u8>>` — matching every
// other byte field in this file rather than introducing a second pattern
// (`serde_bytes` has no direct `Option<Vec<u8>>` support without a custom
// shim) — an empty key is otherwise meaningless for `PublishRecord` anyway
// (PLAN §3.1 dedup/partitioning both need a real key to do anything).
//
// New struct fields MUST carry `#[serde(default)]` (PLAN §6.2) so a peer
// built before a field existed still decodes a message that omits it.

use serde::{Deserialize as SerdeDeserialize, Serialize as SerdeSerialize};

// =============================================================================
// Topic config (mirrors `bus::topics::{TopicConfig, TopicOptions}` 1:1,
// field-for-field, as plain strings/numbers)
// =============================================================================

/// Partial topic settings for `TopicCreateRequest`/`TopicUpdateRequest`.
/// Every field absent (`None`) means "use the server default" on create, or
/// "leave unchanged" on update — same semantics as `bus::topics::
/// TopicOptions`, which this maps to 1:1 in the dispatch handler.
#[derive(Debug, Clone, Default, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusTopicOptionsWire {
    #[serde(default)]
    pub partitions: Option<u32>,
    #[serde(default)]
    pub retention_ms: Option<i64>,
    #[serde(default)]
    pub retention_bytes_per_partition: Option<i64>,
    /// 'delete' | 'compact'.
    #[serde(default)]
    pub cleanup_policy: Option<String>,
    /// 'at_least_once' | 'fire_and_forget'.
    #[serde(default)]
    pub delivery: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub dedup_window_ms: Option<i64>,
    #[serde(default)]
    pub max_delivery_attempts: Option<u32>,
    #[serde(default)]
    pub retry_backoff_ms: Option<u32>,
    #[serde(default)]
    pub schema_id: Option<String>,
    /// 'off' | 'warn' | 'dlq'.
    #[serde(default)]
    pub validation: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub replication_factor: Option<u32>,
    /// 'leader' | 'quorum' | 'all'.
    #[serde(default)]
    pub acks: Option<String>,
    /// Advanced override, wins over `durability_class` below when both are
    /// set. 'os' | 'fsync_batch' | 'fsync_batch_full' | 'fsync_interval:<ms>'
    /// (`<ms>` is 1-1000, e.g. 'fsync_interval:50' — owner decision B), OR
    /// the sentinel `'auto'`. `'auto'` is never a stored/valid
    /// `DurabilityPolicy` and never round-trips back out on a
    /// `BusTopicConfigWire`/`BusTopicSummaryWire` — on `TopicUpdateRequest`
    /// it means "stop overriding, clear any explicit policy and go back to
    /// whatever `durability_class` says" (resolved from `durability_class`
    /// below IF also given in the SAME request, otherwise from this
    /// topic's current effective class). Distinct from omitting this field
    /// entirely, which means "leave unchanged" like every other field
    /// here. A no-op on `TopicCreateRequest` (nothing to clear yet).
    #[serde(default)]
    pub durability: Option<String>,
    /// Friendly durability tier (owner decision B), resolved to a concrete
    /// `durability` policy server-side per node environment when
    /// `durability` itself is unset: 'standard' | 'critical'. Absent means
    /// "use the server default" on create ('standard'), or "leave
    /// unchanged" on update — same `Option` semantics as every other field
    /// here.
    #[serde(default)]
    pub durability_class: Option<String>,
    #[serde(default)]
    pub max_inline_bytes: Option<u64>,
    /// 'lz4' | 'none'.
    #[serde(default)]
    pub compression: Option<String>,
}

/// Full topic config as read back after create/update/detail — mirrors
/// `bus::topics::TopicConfig` field-for-field. `org_id` is deliberately
/// absent: it is always the caller's own org (single-tenant per session),
/// never a cross-org value a client could spoof into a create/update.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusTopicConfigWire {
    pub name: String,
    pub partitions: u32,
    pub retention_ms: i64,
    pub retention_bytes_per_partition: i64,
    pub cleanup_policy: String,
    pub delivery: String,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    pub dedup_window_ms: i64,
    pub max_delivery_attempts: u32,
    pub retry_backoff_ms: u32,
    #[serde(default)]
    pub schema_id: Option<String>,
    pub validation: String,
    pub content_type: String,
    pub replication_factor: u32,
    pub acks: String,
    /// Resolved policy string, e.g. 'os' | 'fsync_batch' | 'fsync_batch_full'
    /// | 'fsync_interval:50' (owner decision B).
    pub durability: String,
    /// Coarse class the resolved `durability` policy above corresponds to
    /// (owner decision B, `topics::TopicConfig::durability_class()`) —
    /// 'standard' | 'critical'. v143: the STORED class when one is
    /// persisted, else derived from `durability`'s policy family — see
    /// `durability_explicit` below to tell which case this is.
    #[serde(default)]
    pub durability_class: String,
    /// v143 (`SUM/tentabus/KRYTYK-M1-R5.md` R5-1/R5-7): `true` iff
    /// `durability` is an explicit override, i.e. no class is currently
    /// stored for this topic (`topics::TopicConfig::durability_explicit()`)
    /// — the honest signal an "(explicit policy)" UI label needs, which
    /// `durability_class` alone cannot provide (it always has SOME value,
    /// stored or derived, and cannot tell the two apart on its own).
    #[serde(default)]
    pub durability_explicit: bool,
    pub max_inline_bytes: u64,
    pub compression: String,
    /// 'dev' | 'test' | 'prod' (`NodeEnvironment::as_str()` convention).
    pub environment: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// One row of `TopicListResponse`. Deliberately narrower than
/// `BusTopicConfigWire` — a list view needs enough to render PLAN's M01
/// table (name, partitions, retention, replication, environment) without
/// forcing every row through the full config struct; `TopicDetailRequest`
/// returns the full config for one topic.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusTopicSummaryWire {
    pub name: String,
    pub partitions: u32,
    pub retention_ms: i64,
    pub replication_factor: u32,
    pub acks: String,
    pub environment: String,
    pub cleanup_policy: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    /// `true` for a `__dlq.*` topic — the UI filters/badges these instead
    /// of hiding them, per PLAN §3.3 ("DLQ jest topikiem, nie osobnym
    /// ekranem").
    pub is_dlq: bool,
    /// Resolved policy string (`BusTopicConfigWire::durability`'s doc) —
    /// v143 follow-up to `SUM/tentabus/KRYTYK-M1-R5.md` R5-1: the M01
    /// topic list previously had no durability information at all, so
    /// every row silently fell back to a UI-side "standard" guess
    /// regardless of the real policy. `#[serde(default)]` so a peer built
    /// before this field existed deserializes an empty string rather than
    /// failing.
    #[serde(default)]
    pub durability: String,
    /// `BusTopicConfigWire::durability_class`'s doc — 'standard' |
    /// 'critical'.
    #[serde(default)]
    pub durability_class: String,
    /// `BusTopicConfigWire::durability_explicit`'s doc.
    #[serde(default)]
    pub durability_explicit: bool,
}

/// Per-partition size proxy for `TopicDetailResponse` (PLAN §6.2 "per-
/// partition offsets/sizes"). `log_end_offset` (the partition's next-write
/// offset / total records ever appended and still retained) is read from
/// `BusService::partition_stats` (follow-up toru P, task 3), the same
/// no-consumer-session read-only introspection call `earliest_offset`/
/// `size_bytes`/`segments` below now come from too — `TopicDetail` no
/// longer needs to open a throwaway probe consumer per partition to report
/// this.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusPartitionInfoWire {
    pub partition: u32,
    pub log_end_offset: u64,
    /// Oldest offset still retained on this partition (PLAN §6.2 "wykryj
    /// lukę retencji"). `0` on a peer built before this field existed
    /// (`#[serde(default)]`) — indistinguishable from a genuinely empty
    /// partition, which is the same ambiguity `PeekResult`'s callers already
    /// accept.
    #[serde(default)]
    pub earliest_offset: u64,
    /// Sum of this partition's SEALED segments only (`PartitionStats`'
    /// doc — the still-growing active segment has no byte-length getter on
    /// the engine's public surface yet) — a lower bound on real disk usage,
    /// not an exact figure.
    #[serde(default)]
    pub size_bytes: u64,
    /// Number of sealed (rolled, immutable) segments backing this
    /// partition — lets the UI distinguish "one small active segment" from
    /// "many rolled segments retention has not caught up with yet".
    #[serde(default)]
    pub segments: u32,
    /// M2 (PLAN-M2 §1f), additive: this partition's current leader, `None`
    /// only when the coordinator itself does not know it yet (mid
    /// election) — never `None` on a peer built before this field existed
    /// (`#[serde(default)]` decodes that as `None` too, indistinguishable
    /// from "unknown"; the M03 UI must treat both the same way).
    #[serde(default)]
    pub leader_node_id: Option<String>,
    /// `0` on a peer built before this field existed OR on an RF=1 topic
    /// that has never had a leader election.
    #[serde(default)]
    pub leader_epoch: u32,
    /// `len(isr)` — `1` for an RF=1 topic (this node only).
    #[serde(default)]
    pub isr_count: u32,
    /// `len(replicas)` — `1` for an RF=1 topic.
    #[serde(default)]
    pub replica_count: u32,
    /// M2 (PLAN-M2 §1f), additive: the REAL high watermark, now that
    /// `hw` and `leo` can diverge under RF>=3 (K-M2-1) — `log_end_offset`
    /// above keeps its M1 meaning (this node's own log tail) so no
    /// existing caller's semantics silently change; this field is the new,
    /// separately-tracked one the M06/M03 UI reads instead.
    #[serde(default)]
    pub high_watermark: u64,
}

/// One row of `TopicDetailResponse.groups` — a consumer group's summed lag
/// across every partition of the ONE topic being detailed (PLAN M03's
/// "Partycje i repliki"/overview KPI), not the group's lag on every topic
/// it subscribes to (see `GroupDetailResponse` for the per-partition,
/// single-group breakdown).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusGroupLagSummaryWire {
    pub group: String,
    pub lag_total: u64,
}

// =============================================================================
// Consumer groups (mirrors `bus::groups` + `db::repository::DbBusGroup`)
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusGroupSummaryWire {
    pub group: String,
    pub topic: String,
    /// 'auto_after_success' | 'explicit' | 'at_most_once'.
    pub commit_mode: String,
    pub paused: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// One partition's state for `GroupDetailResponse` — `committed_offset` is
/// DERIVED (`high_watermark - lag`, both read live off the engine), not
/// stored directly, because `bus::mod`'s public surface exposes lag but not
/// a raw committed-offset getter outside a `ConsumerHandle`'s own owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusGroupPartitionDetailWire {
    pub partition: u32,
    pub committed_offset: u64,
    pub lag: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusGroupDetailWire {
    pub group: String,
    pub topic: String,
    pub commit_mode: String,
    pub paused: bool,
    pub partitions: Vec<BusGroupPartitionDetailWire>,
}

/// `OffsetResetRequest.mode` (PLAN M04's "4 tryby" reset-offset modal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum BusOffsetResetMode {
    Earliest,
    Latest,
    Explicit {
        offset: u64,
    },
    /// Resets to the first offset whose record timestamp is `>= ts_ms`
    /// (PLAN M04's 4th mode, follow-up toru P task 4) — resolved via
    /// `BusService::resolve_offset_for_timestamp`, which wraps the engine's
    /// `PartitionReader::fetch_from_timestamp`. Appended as this enum's last
    /// variant (this crate's append-only convention) rather than inserted
    /// alongside `Explicit`, so a peer built before it existed still decodes
    /// every OTHER mode unchanged.
    Timestamp {
        ts_ms: i64,
    },
}

// =============================================================================
// Message preview (PLAN §6.2 `MessagesBrowse`: <=100 records, <=1 MiB,
// audited `bus.messages.browse`) and DLQ records (PLAN §3.3)
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusHeaderWire {
    pub key: String,
    #[serde(with = "serde_bytes")]
    pub value: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusMessagePreviewWire {
    pub partition: u32,
    pub offset: u64,
    pub timestamp_ms: i64,
    /// Empty = keyless record (see this file's module doc for why this is
    /// not `Option<Vec<u8>>`).
    #[serde(with = "serde_bytes", default)]
    pub key: Vec<u8>,
    pub headers: Vec<BusHeaderWire>,
    /// Truncated to the per-record preview budget the dispatch handler
    /// enforces (PLAN §6.2) — never the full payload for a `BlobRef`-backed
    /// record (see `is_blob_ref`).
    #[serde(with = "serde_bytes")]
    pub payload_preview: Vec<u8>,
    /// `true` when `payload_preview` is a JSON-encoded `BlobRef` (PLAN
    /// §2.4/§6.2 D11: a record whose real payload lives in the blob store,
    /// never re-inlined into a preview) rather than a raw payload prefix.
    pub is_blob_ref: bool,
    /// `true` when `payload_preview` was cut short of the record's real
    /// payload length (never true when `is_blob_ref`).
    pub truncated: bool,
}

/// Per-partition `MessagesBrowse`/`DlqList` metadata (follow-up toru P,
/// task 1) — one entry per partition the request actually read, sourced
/// from `BusService::peek`'s `PeekResult.high_watermark`/`earliest_offset`
/// (the SAME partition snapshot `records` came from, no second round trip).
/// `has_more`/`next_offset` here are the PER-PARTITION equivalents of this
/// result's own top-level `has_more`/`next_offset` (kept for the existing
/// UI, see that field's doc) — a client that wants to keep paging one
/// partition at a time (rather than restarting the whole multi-partition
/// browse) uses these instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusBrowsePartitionInfoWire {
    pub partition: u32,
    pub earliest_offset: u64,
    pub high_watermark: u64,
    pub next_offset: u64,
    pub has_more: bool,
}

/// One `(partition, offset)` starting point for a `MessagesBrowseRequest`/
/// `DlqListRequest.from_offsets` (follow-up toru P task 1) — lets a client
/// resume EACH partition from wherever it last left off, instead of the
/// single scalar `from_offset` applied identically to every partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusPartitionOffsetWire {
    pub partition: u32,
    pub offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusMessagesBrowseResultWire {
    pub records: Vec<BusMessagePreviewWire>,
    /// `true` when more records exist past `records.last()` (the request's
    /// `limit`/1 MiB budget was hit before the partition ran out) — the UI's
    /// cue to offer "load more" starting at `next_offset`. Aggregate across
    /// every partition the request read (`true` iff ANY partition has more)
    /// — kept exactly as before for the existing UI; see `partitions` for
    /// the per-partition breakdown.
    pub has_more: bool,
    /// Aggregate "resume here" scalar — kept exactly as before (the highest
    /// `next_offset` across every partition read) for the existing UI's
    /// scalar `from_offset` follow-up request; see `partitions` for the
    /// per-partition equivalents a client should prefer once it understands
    /// them.
    pub next_offset: u64,
    /// Per-partition breakdown (follow-up toru P task 1) — empty on a peer
    /// built before this field existed (`#[serde(default)]`).
    #[serde(default)]
    pub partitions: Vec<BusBrowsePartitionInfoWire>,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusDlqRecordWire {
    pub partition: u32,
    pub offset: u64,
    pub timestamp_ms: i64,
    #[serde(with = "serde_bytes", default)]
    pub key: Vec<u8>,
    /// `dlq.*` headers only (`dlq.source_topic`, `dlq.source_partition`,
    /// `dlq.source_offset`, `dlq.group_id`, `dlq.attempts`,
    /// `dlq.first_failed_at_ms`, `dlq.last_failed_at_ms`, `dlq.reason`,
    /// `dlq.error_message`) plus whatever non-`tf.*` headers the original
    /// record carried (`bus::dlq::build_dlq_record`'s doc) — never `tf.*`.
    pub headers: Vec<BusHeaderWire>,
    #[serde(with = "serde_bytes")]
    pub payload_preview: Vec<u8>,
    pub is_blob_ref: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusDlqListResultWire {
    pub records: Vec<BusDlqRecordWire>,
    pub has_more: bool,
    pub next_offset: u64,
    /// See `BusMessagesBrowseResultWire::partitions`'s doc — same shape,
    /// same follow-up.
    #[serde(default)]
    pub partitions: Vec<BusBrowsePartitionInfoWire>,
}

// =============================================================================
// ACL (PLAN §8.1 decision D6: `resource_permissions` with
// `resource_type = "topic"`, actions folded into the table's existing
// allow/deny-per-subject shape — see the authorizer's doc for what this
// does NOT model)
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusAclEntryWire {
    /// 'user' | 'group' | 'api_key'.
    pub subject_type: String,
    pub subject_id: String,
    /// 'allow' | 'deny'.
    pub access_level: String,
}

// =============================================================================
// Field policies (SUM/tentabus/POLITYKI-POL.md — per-field access control,
// distinct from the coarse per-topic ACL above: a subject can be ALLOWED on
// a topic's ACL yet still be restricted to a subset of its fields by a
// field policy). One `BusFieldPolicyWire` mirrors one `bus_field_policies`
// row, keyed `(topic, subject_type, subject_id, direction)`.
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusFieldPolicyWire {
    /// 'user' | 'any'.
    pub subject_type: String,
    /// The wildcard sentinel is `"*"` when `subject_type == "any"`.
    pub subject_id: String,
    /// 'write' | 'read'.
    pub direction: String,
    pub fields: Vec<String>,
    pub required_fields: Vec<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

// =============================================================================
// Schema registry (SUM/tentabus/PLAN-F3.md §2 — a versioned, org-scoped
// schema subject/version pair. `BusSchemaSubjectWire` mirrors one
// `bus_schema_subjects` row (`schema_registry::registry::SubjectInfo`),
// `BusSchemaVersionWire` mirrors one immutable `bus_schema_versions` row
// (`registry::VersionInfo`) minus its `schema_text`, which the two
// `*GetResponse` variants below carry separately since most callers
// (subject/version listing) never need the schema body itself.
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusSchemaSubjectWire {
    pub subject: String,
    /// 'json_schema' | 'avro' | 'protobuf' | 'thrift'.
    pub schema_type: String,
    /// 'none' | 'backward' | 'forward' | 'full'.
    pub compatibility: String,
    #[serde(default)]
    pub deprecated_at_ms: Option<i64>,
    #[serde(default)]
    pub latest_version: Option<u32>,
    #[serde(default)]
    pub created_by: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusSchemaVersionWire {
    pub subject: String,
    pub version: u32,
    pub schema_ref_id: u32,
    pub content_hash: String,
    #[serde(default)]
    pub created_by: Option<String>,
    pub created_at_ms: i64,
}

// =============================================================================
// Quotas (PLAN §7.1 per-org quotas)
// =============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusQuotaWire {
    pub max_topics: u32,
    pub max_partitions: u32,
    pub max_bytes_total: u64,
    /// Follow-up toru P task 7: `QuotaManager::produce_msgs_per_sec`/
    /// `produce_bytes_per_sec` getters now exist, so `QuotaGet` reports the
    /// REAL configured rate instead of an unconditional `None`. Kept as
    /// `Option` (rather than promoted to a plain `u32`/`u64`) so a client
    /// that round-trips a value read from an OLDER server (which really did
    /// always send `None` here) does not have to invent a fake number —
    /// `None` still means "unknown", `Some(0)` still means "unlimited" per
    /// `bus::quota`'s token-bucket convention.
    #[serde(default)]
    pub produce_msgs_per_sec: Option<u32>,
    #[serde(default)]
    pub produce_bytes_per_sec: Option<u64>,
    /// `QuotaManager::max_groups` (follow-up toru P task 6 — promoted into
    /// `bus::quota::QuotaConfig` itself). `0` on a peer built before this
    /// field existed; a real `max_groups` of `0` is not otherwise a valid
    /// configured value in practice (it would refuse every `open_consumer`
    /// for a brand-new group), so this is an acceptable "unknown" default
    /// for the same reason `BusPartitionInfoWire::earliest_offset` accepts
    /// one.
    #[serde(default)]
    pub max_groups: u32,
}

// =============================================================================
// Stats (PLAN §6.2 `StatsSubscribe`/`StatsEvent` — delivered here as a
// polling snapshot instead; see the dispatch handler's doc for why the
// `SubscriptionRegistry` push path was not wired for M1)
// =============================================================================

/// Per-topic breakdown for `BusStatsSnapshotWire.topics` (follow-up toru P
/// task 3 — M01's KPI cards/per-topic charts, previously impossible with
/// only the org-wide counts above). Rates come from `BusService::
/// topic_rates` (an in-memory 1s window, PLAN §6.2's polling cadence is 3s
/// so this is always at least one full window old); `total_bytes_on_disk`
/// sums `BusService::partition_stats` across every partition (SEALED
/// segments only, see that struct's doc); `total_lag` sums every group
/// subscribed to this topic; `dlq_depth` is `high_watermark - earliest`
/// summed across the derived `__dlq.<topic>` topic's partitions (`0` when
/// that topic does not exist yet, i.e. it has never needed its DLQ).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusTopicStatsWire {
    pub topic: String,
    pub msgs_in_per_sec: u32,
    pub bytes_in_per_sec: u64,
    pub total_bytes_on_disk: u64,
    pub total_lag: u64,
    pub dlq_depth: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusStatsSnapshotWire {
    pub topic_count: u32,
    pub dlq_topic_count: u32,
    pub partition_count_total: u32,
    pub group_count: u32,
    pub paused_group_count: u32,
    /// Org-wide sum of every non-DLQ topic's `msgs_in_per_sec` (follow-up
    /// toru P task 3). `0` on a peer built before this field existed.
    #[serde(default)]
    pub total_msgs_in_per_sec: u32,
    #[serde(default)]
    pub total_bytes_in_per_sec: u64,
    /// Org-wide sum of `BusTopicStatsWire::total_bytes_on_disk` across
    /// every topic (DLQ topics included, since they occupy real disk too).
    #[serde(default)]
    pub total_bytes_on_disk: u64,
    /// Org-wide sum of `BusTopicStatsWire::total_lag`.
    #[serde(default)]
    pub total_lag: u64,
    /// Org-wide sum of `BusTopicStatsWire::dlq_depth` — the exact figure
    /// the M01 DLQ badge previously had to approximate as "number of topics
    /// with a non-empty DLQ" (POSTEP.md's "Tor U" gap).
    #[serde(default)]
    pub total_dlq_depth: u64,
    /// Per-topic breakdown, non-DLQ topics only (a `__dlq.*` topic's own
    /// figures are folded into its SOURCE topic's `dlq_depth`, not listed
    /// again as its own row) — empty on a peer built before this field
    /// existed.
    #[serde(default)]
    pub topics: Vec<BusTopicStatsWire>,
}

// =============================================================================
// Capabilities (PLAN §8.1 introspection — follow-up toru P task 5)
// =============================================================================

/// The calling session's own effective bus permissions for its current org
/// — `can_read`/`can_write`/`can_admin` mirror `bus.read`/`bus.write`/
/// `bus.admin` exactly as `dispatch/bus.rs`'s `require_read`/`require_admin`
/// check them; `is_site_admin` mirrors the SEPARATE, coarser `#[policy
/// (Admin)]` tier `OffsetReset`/`AclSet`/`Quota*` require (a site admin, not
/// just an org admin with `bus.admin`). A UI gates a control on whichever of
/// these actually matches the request it is about to send, rather than
/// collapsing everything onto `is_site_admin` the way M1's first cut did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusCapabilitiesWire {
    pub can_read: bool,
    pub can_write: bool,
    pub can_admin: bool,
    pub is_site_admin: bool,
}

// =============================================================================
// M2 replication wire types (SUM/tentabus/PLAN-M2.md §1f) — node cards, the
// per-partition role matrix, and failover history for the M06 "Partycje i
// repliki"/replication view. `SCHEMA_VERSION` stays 27 (§1f: `MessageBody`
// tags by variant name, so appending to `BusPayload` alone never forces a
// bump).
// =============================================================================

/// One node's replication-relevant state (mirrors `bus::ReplicaNodeInfo`
/// 1:1) — the M06 node cards (role counts, reachability, last heartbeat).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusReplicaNodeWire {
    pub node_id: String,
    pub label: String,
    /// 'dev' | 'test' | 'prod' (`NodeEnvironment::as_str()`, this crate has
    /// no dependency on `tentaflow-core::bus` so the enum never crosses the
    /// wire directly — same convention as `BusTopicConfigWire.environment`).
    pub environment: String,
    pub is_local: bool,
    pub reachable: bool,
    #[serde(default)]
    pub last_heartbeat_ms_ago: Option<u64>,
    pub leader_count: u32,
    pub follower_count: u32,
    pub isr_count: u32,
}

/// One replica's lag behind the leader (mirrors `bus::ReplicaLagInfo`) —
/// e.g. "lag 87 MiB > 64 MiB" in the M06 role matrix.
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusReplicaLagWire {
    pub node_id: String,
    pub lag_bytes: u64,
    pub lag_ms: u64,
    pub reason: String,
}

/// One partition's replica/role snapshot (mirrors `bus::PartitionReplicaInfo`
/// 1:1, except `unavailable_reason` crosses as a plain string tag —
/// 'no_assignment' | 'epoch_fenced' | 'no_isr' — same string-not-enum
/// convention as every other enum-ish field in this file).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusPartitionReplicaWire {
    pub partition: u32,
    pub leader_node_id: Option<String>,
    pub leader_epoch: u32,
    pub replicas: Vec<String>,
    pub isr: Vec<String>,
    pub lagging: Vec<BusReplicaLagWire>,
    pub high_watermark: u64,
    pub log_end_offset: u64,
    pub unavailable_reason: Option<String>,
}

/// One failover event (M06 timeline) — sourced from `audit_log`'s
/// `bus.leader.failover` entries (PLAN-M2 §1f: no dedicated table).
#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub struct BusFailoverEventWire {
    pub at_ms: i64,
    pub topic: String,
    pub partition: u32,
    pub from_node: Option<String>,
    pub to_node: String,
    pub from_epoch: u32,
    pub to_epoch: u32,
    pub duration_ms: u64,
    pub reason: String,
}

// =============================================================================
// BusPayload — one MessageBody variant for the whole family
// =============================================================================

#[derive(Debug, Clone, PartialEq, Eq, SerdeSerialize, SerdeDeserialize)]
pub enum BusPayload {
    TopicListRequest,
    TopicListResponse {
        topics: Vec<BusTopicSummaryWire>,
    },
    TopicCreateRequest {
        name: String,
        options: BusTopicOptionsWire,
    },
    TopicCreateResponse {
        topic: BusTopicConfigWire,
    },
    TopicUpdateRequest {
        name: String,
        options: BusTopicOptionsWire,
    },
    TopicUpdateResponse {
        topic: BusTopicConfigWire,
    },
    TopicDeleteRequest {
        name: String,
    },
    TopicDeleteResponse,
    TopicDetailRequest {
        name: String,
    },
    TopicDetailResponse {
        topic: BusTopicConfigWire,
        partitions: Vec<BusPartitionInfoWire>,
        groups: Vec<BusGroupLagSummaryWire>,
    },

    GroupListRequest,
    GroupListResponse {
        groups: Vec<BusGroupSummaryWire>,
    },
    GroupDetailRequest {
        group: String,
        topic: String,
    },
    GroupDetailResponse {
        detail: BusGroupDetailWire,
    },
    GroupPauseRequest {
        group: String,
        topic: String,
    },
    GroupPauseResponse,
    GroupResumeRequest {
        group: String,
        topic: String,
    },
    GroupResumeResponse,
    OffsetResetRequest {
        group: String,
        topic: String,
        partition: u32,
        mode: BusOffsetResetMode,
    },
    OffsetResetResponse {
        new_offset: u64,
    },

    MessagesBrowseRequest {
        topic: String,
        /// `None` = start at the earliest retained offset on every
        /// partition. Superseded by `from_offsets` (follow-up toru P task 1)
        /// when that is non-empty; kept as the fallback so a client not yet
        /// aware of `from_offsets` still gets correct (if coarser) behavior.
        #[serde(default)]
        from_offset: Option<u64>,
        /// Per-partition starting offsets (follow-up toru P task 1) — a
        /// partition not listed here starts at its own earliest retained
        /// offset (NOT at `from_offset`, which only applies when this whole
        /// list is empty). Empty by default (`#[serde(default)]`) so a peer
        /// built before this field existed still decodes.
        #[serde(default)]
        from_offsets: Vec<BusPartitionOffsetWire>,
        /// Clamped server-side to 100 (PLAN §6.2) — applies to the TOTAL
        /// record count across every partition read, same as before.
        limit: u32,
        /// Server-side partition filter (R3-2 follow-up, `KRYTYK-M1-R3.md`):
        /// `Some(p)` restricts the peek to partition `p` alone (validated
        /// against the topic's partition count, `bus.partition_out_of_range`
        /// on a miss) instead of walking every partition. `None` keeps the
        /// unchanged multi-partition behavior. `#[serde(default)]` so a peer
        /// built before this field existed still decodes.
        #[serde(default)]
        partition: Option<u32>,
    },
    MessagesBrowseResponse {
        result: BusMessagesBrowseResultWire,
    },

    DlqListRequest {
        /// The SOURCE topic (e.g. `orders.created`) — the handler derives
        /// `__dlq.orders.created` itself, matching every other DLQ request
        /// below, so the UI never has to know/construct the `__dlq.`
        /// prefix.
        source_topic: String,
        #[serde(default)]
        from_offset: Option<u64>,
        /// See `MessagesBrowseRequest::from_offsets`'s doc — same
        /// semantics, applied to the derived `__dlq.<source_topic>` topic.
        #[serde(default)]
        from_offsets: Vec<BusPartitionOffsetWire>,
        limit: u32,
        /// See `MessagesBrowseRequest::partition`'s doc — same semantics,
        /// applied to the derived `__dlq.<source_topic>` topic.
        #[serde(default)]
        partition: Option<u32>,
    },
    DlqListResponse {
        result: BusDlqListResultWire,
    },
    DlqRetryRequest {
        source_topic: String,
        partition: u32,
        offset: u64,
    },
    DlqRetryResponse {
        accepted: u32,
    },
    DlqDiscardRequest {
        source_topic: String,
        partition: u32,
        offset: u64,
    },
    DlqDiscardResponse,
    DlqRetryAllRequest {
        source_topic: String,
        /// Bounded batch (PLAN §6.2) — clamped server-side to 500.
        max_records: u32,
    },
    DlqRetryAllResponse {
        retried: u32,
        failed: u32,
    },

    AclListRequest {
        topic: String,
    },
    AclListResponse {
        entries: Vec<BusAclEntryWire>,
    },
    AclSetRequest {
        topic: String,
        subject_type: String,
        subject_id: String,
        /// 'allow' | 'deny' | 'clear' ('clear' removes the row entirely,
        /// reverting the subject to default-allow).
        access_level: String,
    },
    AclSetResponse,

    StatsSnapshotRequest,
    StatsSnapshotResponse {
        snapshot: BusStatsSnapshotWire,
    },

    QuotaGetRequest,
    QuotaGetResponse {
        quota: BusQuotaWire,
    },
    QuotaSetRequest {
        max_topics: u32,
        max_partitions: u32,
        max_bytes_total: u64,
        produce_msgs_per_sec: u32,
        produce_bytes_per_sec: u64,
        /// `None` = leave `max_groups` unchanged (follow-up toru P task
        /// 6/7) — `#[serde(default)]` so a peer built before this field
        /// existed decodes as `None`, i.e. "unchanged", never a silent
        /// reset to some arbitrary default.
        #[serde(default)]
        max_groups: Option<u32>,
    },
    QuotaSetResponse {
        quota: BusQuotaWire,
    },

    /// Permission introspection for the UI (follow-up toru P task 5, PLAN's
    /// "brak introspekcji uprawnień w UI" gap) — one round trip on module
    /// mount so the dashboard can show/hide admin-only controls for an org
    /// admin (not just a site admin) instead of gating everything on
    /// `is_site_admin` alone. Computed with the SAME `PermissionMatrix`/
    /// `BusAuthorizer` the mutating handlers already enforce, so this is a
    /// read of the real answer, never a client-trusted guess.
    CapabilitiesRequest,
    CapabilitiesResponse {
        capabilities: BusCapabilitiesWire,
    },

    // ===== M2 replication (PLAN-M2 §1f) — `#[policy(UserSession)]`,
    // `bus.read` (`bus_dispatch`) =====
    /// Node cards + partition role matrix + failover history for the M06
    /// "Partycje i repliki" view. `topic: None` means every topic in the
    /// caller's org; `Some(name)` narrows `partitions` (and, for the
    /// RF=1-no-coordinator fallback, `nodes`' role counts too) to that one
    /// topic — see `dispatch::bus::replica_list_v1`'s doc for exactly what
    /// an absent `ReplicationCoordinator` returns.
    ReplicaListRequest {
        #[serde(default)]
        topic: Option<String>,
    },
    ReplicaListResponse {
        nodes: Vec<BusReplicaNodeWire>,
        partitions: Vec<BusPartitionReplicaWire>,
        failovers: Vec<BusFailoverEventWire>,
    },

    // ===== M2 replication (PLAN-M2 §1f) — `#[policy(Admin)]` (site admin
    // tier, `bus_dispatch_admin`) =====
    /// Admin-triggered replica-set change: one partition (`partition:
    /// Some(n)`) or the whole topic (`partition: None`).
    ReassignRequest {
        topic: String,
        #[serde(default)]
        partition: Option<u32>,
        replicas: Vec<String>,
    },
    ReassignResponse {
        /// Number of partitions whose replica set was actually changed.
        applied: u32,
    },
    /// Admin-triggered leader transfer for one partition.
    LeaderTransferRequest {
        topic: String,
        partition: u32,
        target_node_id: String,
    },
    LeaderTransferResponse {
        /// The new leader epoch after the transfer.
        leader_epoch: u32,
    },

    // ===== SUM/tentabus/POLITYKI-POL.md / POLITYKI-POL-FORMATY.md (F0) —
    // field-level access policies. List follows the ACL precedent above
    // (`bus_dispatch`, UserSession transport tier, org-admin checked inside
    // the handler); Set/Delete follow AclSetRequest's precedent (routed
    // through `bus_dispatch_admin`, site-Admin transport tier) since a
    // field policy gates exactly what PII a subject can see or write. =====
    FieldPolicyListRequest {
        topic: String,
    },
    FieldPolicyListResponse {
        policies: Vec<BusFieldPolicyWire>,
    },
    FieldPolicySetRequest {
        topic: String,
        subject_type: String,
        subject_id: String,
        /// 'write' | 'read'.
        direction: String,
        fields: Vec<String>,
        #[serde(default)]
        required_fields: Vec<String>,
    },
    FieldPolicySetResponse,
    FieldPolicyDeleteRequest {
        topic: String,
        subject_type: String,
        subject_id: String,
        direction: String,
    },
    FieldPolicyDeleteResponse,

    // ===== SUM/tentabus/PLAN-F3.md §6 — schema registry. Reads (List
    // subjects/versions, Get, DerivedGet) follow the field-policy list
    // precedent above (`bus_dispatch`, UserSession transport tier,
    // `bus.admin` checked inside the handler); writes (Register,
    // CompatibilitySet, Delete) follow FieldPolicySetRequest's precedent
    // (routed through `bus_dispatch_admin`, site-Admin transport tier). =====
    SchemaSubjectListRequest {},
    SchemaSubjectListResponse {
        subjects: Vec<BusSchemaSubjectWire>,
    },
    SchemaVersionListRequest {
        subject: String,
    },
    SchemaVersionListResponse {
        versions: Vec<BusSchemaVersionWire>,
    },
    SchemaGetRequest {
        subject: String,
        #[serde(default)]
        version: Option<u32>,
    },
    SchemaGetResponse {
        schema: BusSchemaVersionWire,
        schema_text: String,
    },
    /// Derives a read-projected sub-schema from a stored field policy —
    /// does NOT accept a raw `allowed_fields` list from the wire (PLAN-F3
    /// §5.4: a caller enumerating fields via this endpoint's response shape
    /// would let anyone with read access binary-search the full schema
    /// structure).
    SchemaDerivedGetRequest {
        subject: String,
        #[serde(default)]
        version: Option<u32>,
        topic: String,
        /// 'user' | 'any'.
        subject_type: String,
        subject_id: String,
        /// 'write' | 'read'.
        direction: String,
    },
    SchemaDerivedGetResponse {
        schema_text: String,
    },
    SchemaRegisterRequest {
        subject: String,
        /// 'json_schema' | 'avro' | 'protobuf' | 'thrift'.
        schema_type: String,
        schema_text: String,
        /// 'none' | 'backward' | 'forward' | 'full'; `None` = leave/default
        /// to the subject's existing (or 'none' on first registration)
        /// compatibility mode.
        #[serde(default)]
        compatibility: Option<String>,
    },
    SchemaRegisterResponse {
        version: u32,
        schema_ref_id: u32,
        deduplicated: bool,
    },
    SchemaCompatibilitySetRequest {
        subject: String,
        /// 'none' | 'backward' | 'forward' | 'full'.
        compatibility: String,
    },
    SchemaCompatibilitySetResponse,
    SchemaDeleteRequest {
        subject: String,
        #[serde(default)]
        version: Option<u32>,
        /// `#[serde(default)]` (review finding #10) so a legacy encoder
        /// that predates this field — or any encoder that only ever sends
        /// a hard delete — decodes it as `false`, not a decode error.
        #[serde(default)]
        deprecate_only: bool,
    },
    SchemaDeleteResponse {
        removed_versions: Vec<u32>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MessageBody;

    /// Round-trips one `BusPayload` variant through `MessageBody::BusBody` +
    /// CBOR encode/decode, asserting the decoded value matches the original —
    /// same shape as every other payload family's
    /// `message_body_*_round_trip` test in this crate.
    fn round_trip(payload: BusPayload) {
        let body = MessageBody::BusBody(payload.clone());
        let bytes = crate::cbor::encode(&body).expect("encode");
        let decoded = crate::cbor::decode::<MessageBody>(&bytes).expect("decode");
        match decoded {
            MessageBody::BusBody(p) => assert_eq!(p, payload),
            other => panic!("expected BusBody, got {other:?}"),
        }
    }

    fn sample_topic_options() -> BusTopicOptionsWire {
        BusTopicOptionsWire {
            partitions: Some(8),
            retention_ms: Some(604_800_000),
            retention_bytes_per_partition: Some(10 * 1024 * 1024 * 1024),
            cleanup_policy: Some("delete".to_string()),
            delivery: Some("at_least_once".to_string()),
            idempotency_key: None,
            dedup_window_ms: Some(86_400_000),
            max_delivery_attempts: Some(5),
            retry_backoff_ms: Some(1000),
            schema_id: None,
            validation: Some("off".to_string()),
            content_type: Some("application/octet-stream".to_string()),
            replication_factor: Some(3),
            acks: Some("quorum".to_string()),
            durability: Some("fsync_batch_full".to_string()),
            durability_class: Some("critical".to_string()),
            max_inline_bytes: Some(1024 * 1024),
            compression: Some("lz4".to_string()),
        }
    }

    fn sample_topic_config() -> BusTopicConfigWire {
        BusTopicConfigWire {
            name: "pacs.badania.nowe".to_string(),
            partitions: 8,
            retention_ms: 604_800_000,
            retention_bytes_per_partition: 10 * 1024 * 1024 * 1024,
            cleanup_policy: "delete".to_string(),
            delivery: "at_least_once".to_string(),
            idempotency_key: None,
            dedup_window_ms: 86_400_000,
            max_delivery_attempts: 5,
            retry_backoff_ms: 1000,
            schema_id: None,
            validation: "off".to_string(),
            content_type: "application/octet-stream".to_string(),
            replication_factor: 3,
            acks: "quorum".to_string(),
            durability: "fsync_batch_full".to_string(),
            durability_class: "critical".to_string(),
            durability_explicit: false,
            max_inline_bytes: 1024 * 1024,
            compression: "lz4".to_string(),
            environment: "prod".to_string(),
            created_at_ms: 1,
            updated_at_ms: 2,
        }
    }

    #[test]
    fn topic_list_round_trip() {
        round_trip(BusPayload::TopicListRequest);
        round_trip(BusPayload::TopicListResponse {
            topics: vec![BusTopicSummaryWire {
                name: "pacs.badania.nowe".to_string(),
                partitions: 8,
                retention_ms: 604_800_000,
                replication_factor: 3,
                acks: "quorum".to_string(),
                environment: "prod".to_string(),
                cleanup_policy: "delete".to_string(),
                created_at_ms: 1,
                updated_at_ms: 2,
                is_dlq: false,
                durability: "fsync_interval:50".to_string(),
                durability_class: "standard".to_string(),
                durability_explicit: false,
            }],
        });
    }

    #[test]
    fn topic_create_round_trip() {
        round_trip(BusPayload::TopicCreateRequest {
            name: "pacs.badania.nowe".to_string(),
            options: sample_topic_options(),
        });
        round_trip(BusPayload::TopicCreateResponse {
            topic: sample_topic_config(),
        });
    }

    #[test]
    fn topic_update_round_trip() {
        round_trip(BusPayload::TopicUpdateRequest {
            name: "pacs.badania.nowe".to_string(),
            options: sample_topic_options(),
        });
        round_trip(BusPayload::TopicUpdateResponse {
            topic: sample_topic_config(),
        });
    }

    /// Owner decision B: `fsync_interval:<ms>` (`DurabilityClass::Standard`'s
    /// Prod/Test policy) round-trips through CBOR like every other wire
    /// string, and `durability_class` travels alongside it.
    #[test]
    fn topic_create_round_trip_with_fsync_interval_durability() {
        let mut options = sample_topic_options();
        options.durability = Some("fsync_interval:50".to_string());
        options.durability_class = Some("standard".to_string());
        round_trip(BusPayload::TopicCreateRequest {
            name: "pacs.badania.nowe".to_string(),
            options,
        });

        let mut topic = sample_topic_config();
        topic.durability = "fsync_interval:50".to_string();
        topic.durability_class = "standard".to_string();
        round_trip(BusPayload::TopicCreateResponse { topic });
    }

    /// v143: `durability_explicit` on `BusTopicConfigWire` and the three
    /// durability fields on `BusTopicSummaryWire` all round-trip, and the
    /// `durability: "auto"` update sentinel is just a plain string on the
    /// wire (`TopicUpdateRequest` never itself validates it — that is
    /// `dispatch/bus.rs`'s job).
    #[test]
    fn topic_config_durability_explicit_round_trips() {
        let mut topic = sample_topic_config();
        topic.durability = "os".to_string();
        topic.durability_class = "standard".to_string();
        topic.durability_explicit = true;
        round_trip(BusPayload::TopicUpdateResponse { topic });
    }

    #[test]
    fn topic_summary_durability_fields_round_trip() {
        round_trip(BusPayload::TopicListResponse {
            topics: vec![BusTopicSummaryWire {
                name: "krytyk.crit".to_string(),
                partitions: 8,
                retention_ms: 604_800_000,
                replication_factor: 1,
                acks: "leader".to_string(),
                environment: "prod".to_string(),
                cleanup_policy: "delete".to_string(),
                created_at_ms: 1,
                updated_at_ms: 2,
                is_dlq: false,
                durability: "fsync_batch_full".to_string(),
                durability_class: "critical".to_string(),
                durability_explicit: false,
            }],
        });
    }

    #[test]
    fn topic_update_request_with_auto_durability_sentinel_round_trips() {
        let mut options = sample_topic_options();
        options.durability = Some("auto".to_string());
        options.durability_class = None;
        round_trip(BusPayload::TopicUpdateRequest {
            name: "pacs.badania.nowe".to_string(),
            options,
        });
    }

    #[test]
    fn topic_delete_round_trip() {
        round_trip(BusPayload::TopicDeleteRequest {
            name: "pacs.badania.nowe".to_string(),
        });
        round_trip(BusPayload::TopicDeleteResponse);
    }

    #[test]
    fn topic_detail_round_trip() {
        round_trip(BusPayload::TopicDetailRequest {
            name: "pacs.badania.nowe".to_string(),
        });
        round_trip(BusPayload::TopicDetailResponse {
            topic: sample_topic_config(),
            partitions: vec![BusPartitionInfoWire {
                partition: 0,
                log_end_offset: 1234,
                earliest_offset: 0,
                size_bytes: 4096,
                segments: 2,
                leader_node_id: Some("gcm-core-01".to_string()),
                leader_epoch: 3,
                isr_count: 2,
                replica_count: 3,
                high_watermark: 1234,
            }],
            groups: vec![BusGroupLagSummaryWire {
                group: "radiologia-worker".to_string(),
                lag_total: 87,
            }],
        });
    }

    #[test]
    fn group_list_round_trip() {
        round_trip(BusPayload::GroupListRequest);
        round_trip(BusPayload::GroupListResponse {
            groups: vec![BusGroupSummaryWire {
                group: "radiologia-worker".to_string(),
                topic: "pacs.badania.nowe".to_string(),
                commit_mode: "auto_after_success".to_string(),
                paused: false,
                created_at_ms: 1,
                updated_at_ms: 2,
            }],
        });
    }

    #[test]
    fn group_detail_round_trip() {
        round_trip(BusPayload::GroupDetailRequest {
            group: "radiologia-worker".to_string(),
            topic: "pacs.badania.nowe".to_string(),
        });
        round_trip(BusPayload::GroupDetailResponse {
            detail: BusGroupDetailWire {
                group: "radiologia-worker".to_string(),
                topic: "pacs.badania.nowe".to_string(),
                commit_mode: "auto_after_success".to_string(),
                paused: false,
                partitions: vec![BusGroupPartitionDetailWire {
                    partition: 0,
                    committed_offset: 100,
                    lag: 5,
                }],
            },
        });
    }

    #[test]
    fn group_pause_resume_round_trip() {
        round_trip(BusPayload::GroupPauseRequest {
            group: "radiologia-worker".to_string(),
            topic: "pacs.badania.nowe".to_string(),
        });
        round_trip(BusPayload::GroupPauseResponse);
        round_trip(BusPayload::GroupResumeRequest {
            group: "radiologia-worker".to_string(),
            topic: "pacs.badania.nowe".to_string(),
        });
        round_trip(BusPayload::GroupResumeResponse);
    }

    #[test]
    fn offset_reset_round_trip() {
        round_trip(BusPayload::OffsetResetRequest {
            group: "radiologia-worker".to_string(),
            topic: "pacs.badania.nowe".to_string(),
            partition: 0,
            mode: BusOffsetResetMode::Earliest,
        });
        round_trip(BusPayload::OffsetResetRequest {
            group: "radiologia-worker".to_string(),
            topic: "pacs.badania.nowe".to_string(),
            partition: 0,
            mode: BusOffsetResetMode::Latest,
        });
        round_trip(BusPayload::OffsetResetRequest {
            group: "radiologia-worker".to_string(),
            topic: "pacs.badania.nowe".to_string(),
            partition: 0,
            mode: BusOffsetResetMode::Explicit { offset: 42 },
        });
        round_trip(BusPayload::OffsetResetRequest {
            group: "radiologia-worker".to_string(),
            topic: "pacs.badania.nowe".to_string(),
            partition: 0,
            mode: BusOffsetResetMode::Timestamp {
                ts_ms: 1_700_000_000_000,
            },
        });
        round_trip(BusPayload::OffsetResetResponse { new_offset: 42 });
    }

    #[test]
    fn messages_browse_round_trip() {
        round_trip(BusPayload::MessagesBrowseRequest {
            topic: "pacs.badania.nowe".to_string(),
            from_offset: Some(10),
            from_offsets: vec![
                BusPartitionOffsetWire {
                    partition: 0,
                    offset: 10,
                },
                BusPartitionOffsetWire {
                    partition: 1,
                    offset: 0,
                },
            ],
            limit: 50,
            partition: Some(1),
        });
        round_trip(BusPayload::MessagesBrowseResponse {
            result: BusMessagesBrowseResultWire {
                records: vec![BusMessagePreviewWire {
                    partition: 0,
                    offset: 10,
                    timestamp_ms: 1000,
                    key: vec![1, 2, 3],
                    headers: vec![BusHeaderWire {
                        key: "tf.org".to_string(),
                        value: b"org-1".to_vec(),
                    }],
                    payload_preview: vec![0xde, 0xad, 0xbe, 0xef],
                    is_blob_ref: false,
                    truncated: false,
                }],
                has_more: true,
                next_offset: 11,
                partitions: vec![BusBrowsePartitionInfoWire {
                    partition: 0,
                    earliest_offset: 0,
                    high_watermark: 100,
                    next_offset: 11,
                    has_more: true,
                }],
            },
        });
    }

    #[test]
    fn dlq_list_round_trip() {
        round_trip(BusPayload::DlqListRequest {
            source_topic: "lab.wyniki.scchs".to_string(),
            from_offset: None,
            from_offsets: vec![],
            limit: 46,
            partition: None,
        });
        round_trip(BusPayload::DlqListResponse {
            result: BusDlqListResultWire {
                records: vec![BusDlqRecordWire {
                    partition: 0,
                    offset: 5,
                    timestamp_ms: 2000,
                    key: vec![],
                    headers: vec![BusHeaderWire {
                        key: "dlq.reason".to_string(),
                        value: b"consumer_error".to_vec(),
                    }],
                    payload_preview: vec![1, 2, 3],
                    is_blob_ref: false,
                    truncated: true,
                }],
                has_more: false,
                next_offset: 6,
                partitions: vec![BusBrowsePartitionInfoWire {
                    partition: 0,
                    earliest_offset: 0,
                    high_watermark: 6,
                    next_offset: 6,
                    has_more: false,
                }],
            },
        });
    }

    #[test]
    fn dlq_retry_and_discard_round_trip() {
        round_trip(BusPayload::DlqRetryRequest {
            source_topic: "lab.wyniki.scchs".to_string(),
            partition: 0,
            offset: 5,
        });
        round_trip(BusPayload::DlqRetryResponse { accepted: 1 });
        round_trip(BusPayload::DlqDiscardRequest {
            source_topic: "lab.wyniki.scchs".to_string(),
            partition: 0,
            offset: 5,
        });
        round_trip(BusPayload::DlqDiscardResponse);
        round_trip(BusPayload::DlqRetryAllRequest {
            source_topic: "lab.wyniki.scchs".to_string(),
            max_records: 46,
        });
        round_trip(BusPayload::DlqRetryAllResponse {
            retried: 40,
            failed: 6,
        });
    }

    #[test]
    fn acl_round_trip() {
        round_trip(BusPayload::AclListRequest {
            topic: "pacs.badania.nowe".to_string(),
        });
        round_trip(BusPayload::AclListResponse {
            entries: vec![BusAclEntryWire {
                subject_type: "user".to_string(),
                subject_id: "u-1".to_string(),
                access_level: "allow".to_string(),
            }],
        });
        round_trip(BusPayload::AclSetRequest {
            topic: "pacs.badania.nowe".to_string(),
            subject_type: "user".to_string(),
            subject_id: "u-1".to_string(),
            access_level: "deny".to_string(),
        });
        round_trip(BusPayload::AclSetResponse);
    }

    #[test]
    fn stats_snapshot_round_trip() {
        round_trip(BusPayload::StatsSnapshotRequest);
        round_trip(BusPayload::StatsSnapshotResponse {
            snapshot: BusStatsSnapshotWire {
                topic_count: 6,
                dlq_topic_count: 3,
                partition_count_total: 48,
                group_count: 9,
                paused_group_count: 1,
                total_msgs_in_per_sec: 120,
                total_bytes_in_per_sec: 4096,
                total_bytes_on_disk: 1024 * 1024,
                total_lag: 42,
                total_dlq_depth: 3,
                topics: vec![BusTopicStatsWire {
                    topic: "pacs.badania.nowe".to_string(),
                    msgs_in_per_sec: 120,
                    bytes_in_per_sec: 4096,
                    total_bytes_on_disk: 1024 * 1024,
                    total_lag: 42,
                    dlq_depth: 3,
                }],
            },
        });
    }

    /// `#[serde(default)]` on every new `BusStatsSnapshotWire` field means a
    /// peer built before they existed still decodes — regression guard for
    /// follow-up toru P task 3.
    #[test]
    fn stats_snapshot_new_fields_default_when_absent() {
        #[derive(SerdeSerialize)]
        struct LegacySnapshot {
            topic_count: u32,
            dlq_topic_count: u32,
            partition_count_total: u32,
            group_count: u32,
            paused_group_count: u32,
        }
        #[derive(SerdeSerialize)]
        enum LegacyBusPayload {
            StatsSnapshotResponse { snapshot: LegacySnapshot },
        }

        let legacy = LegacyBusPayload::StatsSnapshotResponse {
            snapshot: LegacySnapshot {
                topic_count: 6,
                dlq_topic_count: 3,
                partition_count_total: 48,
                group_count: 9,
                paused_group_count: 1,
            },
        };
        let bytes = crate::cbor::encode(&legacy).expect("encode");
        let decoded = crate::cbor::decode::<BusPayload>(&bytes).expect("decode");
        assert_eq!(
            decoded,
            BusPayload::StatsSnapshotResponse {
                snapshot: BusStatsSnapshotWire {
                    topic_count: 6,
                    dlq_topic_count: 3,
                    partition_count_total: 48,
                    group_count: 9,
                    paused_group_count: 1,
                    total_msgs_in_per_sec: 0,
                    total_bytes_in_per_sec: 0,
                    total_bytes_on_disk: 0,
                    total_lag: 0,
                    total_dlq_depth: 0,
                    topics: vec![],
                }
            }
        );
    }

    #[test]
    fn quota_round_trip() {
        round_trip(BusPayload::QuotaGetRequest);
        round_trip(BusPayload::QuotaGetResponse {
            quota: BusQuotaWire {
                max_topics: 100,
                max_partitions: 1024,
                max_bytes_total: 1024 * 1024 * 1024 * 1024,
                produce_msgs_per_sec: None,
                produce_bytes_per_sec: None,
                max_groups: 1000,
            },
        });
        round_trip(BusPayload::QuotaSetRequest {
            max_topics: 200,
            max_partitions: 2048,
            max_bytes_total: 2 * 1024 * 1024 * 1024 * 1024,
            produce_msgs_per_sec: 200_000,
            produce_bytes_per_sec: 2 * 1024 * 1024 * 1024,
            max_groups: Some(500),
        });
        round_trip(BusPayload::QuotaSetRequest {
            max_topics: 200,
            max_partitions: 2048,
            max_bytes_total: 2 * 1024 * 1024 * 1024 * 1024,
            produce_msgs_per_sec: 200_000,
            produce_bytes_per_sec: 2 * 1024 * 1024 * 1024,
            max_groups: None,
        });
        round_trip(BusPayload::QuotaSetResponse {
            quota: BusQuotaWire {
                max_topics: 200,
                max_partitions: 2048,
                max_bytes_total: 2 * 1024 * 1024 * 1024 * 1024,
                produce_msgs_per_sec: Some(200_000),
                produce_bytes_per_sec: Some(2 * 1024 * 1024 * 1024),
                max_groups: 500,
            },
        });
    }

    /// `#[serde(default)]` on `QuotaSetRequest.max_groups` means a peer
    /// built before that field existed still decodes as `None` ("leave
    /// unchanged") — regression guard for follow-up toru P task 6/7.
    #[test]
    fn quota_set_request_max_groups_defaults_to_none_when_absent() {
        #[derive(SerdeSerialize)]
        enum LegacyBusPayload {
            QuotaSetRequest {
                max_topics: u32,
                max_partitions: u32,
                max_bytes_total: u64,
                produce_msgs_per_sec: u32,
                produce_bytes_per_sec: u64,
            },
        }

        let legacy = LegacyBusPayload::QuotaSetRequest {
            max_topics: 10,
            max_partitions: 20,
            max_bytes_total: 30,
            produce_msgs_per_sec: 40,
            produce_bytes_per_sec: 50,
        };
        let bytes = crate::cbor::encode(&legacy).expect("encode");
        let decoded = crate::cbor::decode::<BusPayload>(&bytes).expect("decode");
        assert_eq!(
            decoded,
            BusPayload::QuotaSetRequest {
                max_topics: 10,
                max_partitions: 20,
                max_bytes_total: 30,
                produce_msgs_per_sec: 40,
                produce_bytes_per_sec: 50,
                max_groups: None,
            }
        );
    }

    /// `#[serde(default)]` on `from_offset` means a peer built before that
    /// field existed still decodes — regression guard for the M08 "podgląd
    /// komunikatów" preview's optional starting offset. Mirrors
    /// `message_body.rs`'s `robot_entry_legacy_decode_without_actions_meta_
    /// defaults_empty` pattern: a hand-rolled enum with the OLD field set
    /// reproduces exactly the CBOR shape a pre-`from_offset` sender would
    /// have emitted.
    #[test]
    fn messages_browse_from_offset_defaults_when_absent() {
        #[derive(SerdeSerialize)]
        enum LegacyBusPayload {
            MessagesBrowseRequest { topic: String, limit: u32 },
        }

        let legacy = LegacyBusPayload::MessagesBrowseRequest {
            topic: "pacs.badania.nowe".to_string(),
            limit: 10,
        };
        let bytes = crate::cbor::encode(&legacy).expect("encode");
        let decoded = crate::cbor::decode::<BusPayload>(&bytes).expect("decode");
        assert_eq!(
            decoded,
            BusPayload::MessagesBrowseRequest {
                topic: "pacs.badania.nowe".to_string(),
                from_offset: None,
                from_offsets: vec![],
                limit: 10,
                partition: None,
            }
        );
    }

    /// `#[serde(default)]` on `partition` means a peer built before the
    /// R3-2 server-side partition filter existed still decodes — regression
    /// guard for `KRYTYK-M1-R3.md` R3-2 (`SUM/tentabus/POSTEP.md` "Decyzje
    /// po R3"). Mirrors `messages_browse_from_offset_defaults_when_absent`.
    #[test]
    fn messages_browse_partition_defaults_to_none_when_absent() {
        #[derive(SerdeSerialize)]
        enum LegacyBusPayload {
            MessagesBrowseRequest {
                topic: String,
                from_offset: Option<u64>,
                from_offsets: Vec<BusPartitionOffsetWire>,
                limit: u32,
            },
        }

        let legacy = LegacyBusPayload::MessagesBrowseRequest {
            topic: "pacs.badania.nowe".to_string(),
            from_offset: None,
            from_offsets: vec![],
            limit: 10,
        };
        let bytes = crate::cbor::encode(&legacy).expect("encode");
        let decoded = crate::cbor::decode::<BusPayload>(&bytes).expect("decode");
        assert_eq!(
            decoded,
            BusPayload::MessagesBrowseRequest {
                topic: "pacs.badania.nowe".to_string(),
                from_offset: None,
                from_offsets: vec![],
                limit: 10,
                partition: None,
            }
        );
    }

    /// Same guard as `messages_browse_partition_defaults_to_none_when_absent`,
    /// for `DlqListRequest`.
    #[test]
    fn dlq_list_partition_defaults_to_none_when_absent() {
        #[derive(SerdeSerialize)]
        enum LegacyBusPayload {
            DlqListRequest {
                source_topic: String,
                from_offset: Option<u64>,
                from_offsets: Vec<BusPartitionOffsetWire>,
                limit: u32,
            },
        }

        let legacy = LegacyBusPayload::DlqListRequest {
            source_topic: "lab.wyniki.scchs".to_string(),
            from_offset: None,
            from_offsets: vec![],
            limit: 46,
        };
        let bytes = crate::cbor::encode(&legacy).expect("encode");
        let decoded = crate::cbor::decode::<BusPayload>(&bytes).expect("decode");
        assert_eq!(
            decoded,
            BusPayload::DlqListRequest {
                source_topic: "lab.wyniki.scchs".to_string(),
                from_offset: None,
                from_offsets: vec![],
                limit: 46,
                partition: None,
            }
        );
    }

    #[test]
    fn capabilities_round_trip() {
        round_trip(BusPayload::CapabilitiesRequest);
        round_trip(BusPayload::CapabilitiesResponse {
            capabilities: BusCapabilitiesWire {
                can_read: true,
                can_write: true,
                can_admin: false,
                is_site_admin: false,
            },
        });
    }

    // =========================================================================
    // M2 replication (PLAN-M2 §1f)
    // =========================================================================

    fn sample_replica_node() -> BusReplicaNodeWire {
        BusReplicaNodeWire {
            node_id: "gcm-core-01".to_string(),
            label: "gcm-core-01".to_string(),
            environment: "prod".to_string(),
            is_local: true,
            reachable: true,
            last_heartbeat_ms_ago: Some(120),
            leader_count: 5,
            follower_count: 3,
            isr_count: 8,
        }
    }

    fn sample_partition_replica() -> BusPartitionReplicaWire {
        BusPartitionReplicaWire {
            partition: 5,
            leader_node_id: Some("gcm-core-01".to_string()),
            leader_epoch: 3,
            replicas: vec![
                "gcm-core-01".to_string(),
                "gczd-edge-02".to_string(),
                "scchs-edge-03".to_string(),
            ],
            isr: vec!["gcm-core-01".to_string(), "gczd-edge-02".to_string()],
            lagging: vec![BusReplicaLagWire {
                node_id: "scchs-edge-03".to_string(),
                lag_bytes: 91_226_112,
                lag_ms: 4_200,
                reason: "lag 87 MiB > 64 MiB".to_string(),
            }],
            high_watermark: 100_000,
            log_end_offset: 100_120,
            unavailable_reason: None,
        }
    }

    fn sample_failover_event() -> BusFailoverEventWire {
        BusFailoverEventWire {
            at_ms: 1_756_000_000_000,
            topic: "pacs.badania.nowe".to_string(),
            partition: 5,
            from_node: Some("gczd-edge-02".to_string()),
            to_node: "gcm-core-01".to_string(),
            from_epoch: 2,
            to_epoch: 3,
            duration_ms: 840,
            reason: "lease_expired".to_string(),
        }
    }

    #[test]
    fn replica_list_round_trip() {
        round_trip(BusPayload::ReplicaListRequest { topic: None });
        round_trip(BusPayload::ReplicaListRequest {
            topic: Some("pacs.badania.nowe".to_string()),
        });
        round_trip(BusPayload::ReplicaListResponse {
            nodes: vec![sample_replica_node()],
            partitions: vec![sample_partition_replica()],
            failovers: vec![sample_failover_event()],
        });
        // Empty response (RF=1, no coordinator, no failovers yet) must also
        // round-trip cleanly — the single-node fallback shape.
        round_trip(BusPayload::ReplicaListResponse {
            nodes: vec![],
            partitions: vec![],
            failovers: vec![],
        });
    }

    #[test]
    fn replica_list_request_topic_defaults_to_none_when_absent() {
        #[derive(SerdeSerialize)]
        enum LegacyBusPayload {
            ReplicaListRequest {},
        }

        let legacy = LegacyBusPayload::ReplicaListRequest {};
        let bytes = crate::cbor::encode(&legacy).expect("encode");
        let decoded = crate::cbor::decode::<BusPayload>(&bytes).expect("decode");
        assert_eq!(decoded, BusPayload::ReplicaListRequest { topic: None });
    }

    #[test]
    fn partition_replica_wire_unavailable_reason_round_trips() {
        let mut p = sample_partition_replica();
        p.unavailable_reason = Some("no_isr".to_string());
        round_trip(BusPayload::ReplicaListResponse {
            nodes: vec![],
            partitions: vec![p],
            failovers: vec![],
        });
    }

    #[test]
    fn reassign_round_trip() {
        round_trip(BusPayload::ReassignRequest {
            topic: "pacs.badania.nowe".to_string(),
            partition: Some(5),
            replicas: vec!["gcm-core-01".to_string(), "gczd-edge-02".to_string()],
        });
        round_trip(BusPayload::ReassignRequest {
            topic: "pacs.badania.nowe".to_string(),
            partition: None,
            replicas: vec!["gcm-core-01".to_string()],
        });
        round_trip(BusPayload::ReassignResponse { applied: 8 });
    }

    #[test]
    fn reassign_request_partition_defaults_to_none_when_absent() {
        #[derive(SerdeSerialize)]
        enum LegacyBusPayload {
            ReassignRequest {
                topic: String,
                replicas: Vec<String>,
            },
        }

        let legacy = LegacyBusPayload::ReassignRequest {
            topic: "pacs.badania.nowe".to_string(),
            replicas: vec!["gcm-core-01".to_string()],
        };
        let bytes = crate::cbor::encode(&legacy).expect("encode");
        let decoded = crate::cbor::decode::<BusPayload>(&bytes).expect("decode");
        assert_eq!(
            decoded,
            BusPayload::ReassignRequest {
                topic: "pacs.badania.nowe".to_string(),
                partition: None,
                replicas: vec!["gcm-core-01".to_string()],
            }
        );
    }

    #[test]
    fn leader_transfer_round_trip() {
        round_trip(BusPayload::LeaderTransferRequest {
            topic: "pacs.badania.nowe".to_string(),
            partition: 5,
            target_node_id: "gczd-edge-02".to_string(),
        });
        round_trip(BusPayload::LeaderTransferResponse { leader_epoch: 4 });
    }

    #[test]
    fn field_policy_round_trip() {
        round_trip(BusPayload::FieldPolicyListRequest {
            topic: "patients.updated".to_string(),
        });
        round_trip(BusPayload::FieldPolicyListResponse {
            policies: vec![BusFieldPolicyWire {
                subject_type: "any".to_string(),
                subject_id: "*".to_string(),
                direction: "write".to_string(),
                fields: vec!["patient_id".to_string(), "status".to_string()],
                required_fields: vec!["patient_id".to_string()],
                created_at_ms: 1000,
                updated_at_ms: 2000,
            }],
        });
        round_trip(BusPayload::FieldPolicySetRequest {
            topic: "patients.updated".to_string(),
            subject_type: "user".to_string(),
            subject_id: "u-1".to_string(),
            direction: "read".to_string(),
            fields: vec!["patient_id".to_string()],
            required_fields: vec![],
        });
        round_trip(BusPayload::FieldPolicySetResponse);
        round_trip(BusPayload::FieldPolicyDeleteRequest {
            topic: "patients.updated".to_string(),
            subject_type: "user".to_string(),
            subject_id: "u-1".to_string(),
            direction: "read".to_string(),
        });
        round_trip(BusPayload::FieldPolicyDeleteResponse);
    }

    #[test]
    fn schema_registry_round_trip() {
        let subject_wire = BusSchemaSubjectWire {
            subject: "patients.updated".to_string(),
            schema_type: "json_schema".to_string(),
            compatibility: "backward".to_string(),
            deprecated_at_ms: None,
            latest_version: Some(2),
            created_by: Some("u-admin".to_string()),
            created_at_ms: 1000,
            updated_at_ms: 2000,
        };
        let version_wire = BusSchemaVersionWire {
            subject: "patients.updated".to_string(),
            version: 2,
            schema_ref_id: 12345,
            content_hash: "blake3:deadbeef".to_string(),
            created_by: Some("u-admin".to_string()),
            created_at_ms: 1500,
        };

        round_trip(BusPayload::SchemaSubjectListRequest {});
        round_trip(BusPayload::SchemaSubjectListResponse {
            subjects: vec![subject_wire],
        });
        round_trip(BusPayload::SchemaVersionListRequest {
            subject: "patients.updated".to_string(),
        });
        round_trip(BusPayload::SchemaVersionListResponse {
            versions: vec![version_wire.clone()],
        });
        round_trip(BusPayload::SchemaGetRequest {
            subject: "patients.updated".to_string(),
            version: Some(2),
        });
        round_trip(BusPayload::SchemaGetRequest {
            subject: "patients.updated".to_string(),
            version: None,
        });
        round_trip(BusPayload::SchemaGetResponse {
            schema: version_wire.clone(),
            schema_text: "{\"type\":\"object\"}".to_string(),
        });
        round_trip(BusPayload::SchemaDerivedGetRequest {
            subject: "patients.updated".to_string(),
            version: Some(2),
            topic: "patients.updated".to_string(),
            subject_type: "user".to_string(),
            subject_id: "u-1".to_string(),
            direction: "read".to_string(),
        });
        round_trip(BusPayload::SchemaDerivedGetResponse {
            schema_text: "{\"type\":\"object\",\"additionalProperties\":false}".to_string(),
        });
        round_trip(BusPayload::SchemaRegisterRequest {
            subject: "patients.updated".to_string(),
            schema_type: "json_schema".to_string(),
            schema_text: "{\"type\":\"object\"}".to_string(),
            compatibility: Some("backward".to_string()),
        });
        round_trip(BusPayload::SchemaRegisterRequest {
            subject: "patients.updated".to_string(),
            schema_type: "avro".to_string(),
            schema_text: "{\"type\":\"record\"}".to_string(),
            compatibility: None,
        });
        round_trip(BusPayload::SchemaRegisterResponse {
            version: 2,
            schema_ref_id: 12345,
            deduplicated: false,
        });
        round_trip(BusPayload::SchemaCompatibilitySetRequest {
            subject: "patients.updated".to_string(),
            compatibility: "full".to_string(),
        });
        round_trip(BusPayload::SchemaCompatibilitySetResponse);
        round_trip(BusPayload::SchemaDeleteRequest {
            subject: "patients.updated".to_string(),
            version: Some(1),
            deprecate_only: false,
        });
        round_trip(BusPayload::SchemaDeleteRequest {
            subject: "patients.updated".to_string(),
            version: None,
            deprecate_only: true,
        });
        round_trip(BusPayload::SchemaDeleteResponse {
            removed_versions: vec![1, 2],
        });
    }

    /// A peer built before `FieldPolicySetRequest.required_fields` existed
    /// must still decode a message that omits it, as `"leave/omit ==
    /// empty"` rather than a hard decode failure — same additive-field
    /// contract as every other `#[serde(default)]` field in this crate.
    #[test]
    fn field_policy_set_required_fields_defaults_when_absent() {
        #[derive(SerdeSerialize)]
        enum LegacyBusPayload {
            FieldPolicySetRequest {
                topic: String,
                subject_type: String,
                subject_id: String,
                direction: String,
                fields: Vec<String>,
            },
        }
        let legacy = LegacyBusPayload::FieldPolicySetRequest {
            topic: "patients.updated".to_string(),
            subject_type: "any".to_string(),
            subject_id: "*".to_string(),
            direction: "write".to_string(),
            fields: vec!["patient_id".to_string()],
        };
        let bytes = crate::cbor::encode(&legacy).expect("encode");
        let decoded: BusPayload = crate::cbor::decode(&bytes).expect("decode");
        match decoded {
            BusPayload::FieldPolicySetRequest {
                required_fields, ..
            } => assert!(required_fields.is_empty()),
            other => panic!("expected FieldPolicySetRequest, got {other:?}"),
        }
    }

    /// Review finding #10: `SchemaDeleteRequest.deprecate_only` must decode
    /// as `false` (a hard delete, the pre-existing behavior) when a legacy
    /// encoder omits it entirely — same additive-field contract as
    /// `field_policy_set_required_fields_defaults_when_absent` above.
    #[test]
    fn schema_delete_request_deprecate_only_defaults_when_absent() {
        #[derive(SerdeSerialize)]
        enum LegacyBusPayload {
            SchemaDeleteRequest {
                subject: String,
                version: Option<u32>,
            },
        }
        let legacy = LegacyBusPayload::SchemaDeleteRequest {
            subject: "patients.updated".to_string(),
            version: Some(3),
        };
        let bytes = crate::cbor::encode(&legacy).expect("encode");
        let decoded: BusPayload = crate::cbor::decode(&bytes).expect("decode");
        match decoded {
            BusPayload::SchemaDeleteRequest {
                subject,
                version,
                deprecate_only,
            } => {
                assert_eq!(subject, "patients.updated");
                assert_eq!(version, Some(3));
                assert!(!deprecate_only);
            }
            other => panic!("expected SchemaDeleteRequest, got {other:?}"),
        }
    }

    /// `BusPartitionInfoWire`'s M2 additive fields (PLAN-M2 §1f) must default
    /// to the same "unknown"/RF=1-neutral values a pre-M2 peer's encoding
    /// implies — same guard shape as `messages_browse_partition_defaults_
    /// to_none_when_absent` above, applied to `TopicDetailResponse`'s
    /// per-partition wire instead of a request field.
    #[test]
    fn partition_info_wire_m2_fields_default_when_absent() {
        #[derive(SerdeSerialize)]
        struct LegacyBusPartitionInfoWire {
            partition: u32,
            log_end_offset: u64,
            earliest_offset: u64,
            size_bytes: u64,
            segments: u32,
        }
        #[derive(SerdeSerialize)]
        enum LegacyBusPayload {
            TopicDetailResponsePartitionsOnly {
                partitions: Vec<LegacyBusPartitionInfoWire>,
            },
        }
        #[derive(Debug, Clone, PartialEq, Eq, SerdeDeserialize)]
        enum CurrentPartitionsOnly {
            TopicDetailResponsePartitionsOnly {
                partitions: Vec<BusPartitionInfoWire>,
            },
        }

        let legacy = LegacyBusPayload::TopicDetailResponsePartitionsOnly {
            partitions: vec![LegacyBusPartitionInfoWire {
                partition: 0,
                log_end_offset: 42,
                earliest_offset: 0,
                size_bytes: 4096,
                segments: 1,
            }],
        };
        let bytes = crate::cbor::encode(&legacy).expect("encode");
        let decoded: CurrentPartitionsOnly = crate::cbor::decode(&bytes).expect("decode");
        let CurrentPartitionsOnly::TopicDetailResponsePartitionsOnly { partitions } = decoded;
        assert_eq!(
            partitions,
            vec![BusPartitionInfoWire {
                partition: 0,
                log_end_offset: 42,
                earliest_offset: 0,
                size_bytes: 4096,
                segments: 1,
                leader_node_id: None,
                leader_epoch: 0,
                isr_count: 0,
                replica_count: 0,
                high_watermark: 0,
            }]
        );
    }
}
