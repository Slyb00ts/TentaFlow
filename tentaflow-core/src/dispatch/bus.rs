// =============================================================================
// Plik: dispatch/bus.rs — TentaBus M1 dispatch handlers (SUM/tentabus/PLAN.md §6.2)
// =============================================================================
//
// plan-app-platform §4.2/§7 W7: ONE dispatch function, `bus_dispatch`, at
// session tier `UserSession`. PLAN §6.2's original two-tier split
// (`#[policy(UserSession)]`/`#[policy(Admin)]` — a global site-admin
// session for `OffsetReset`/`AclSet`/`QuotaSet`/etc. via a separate
// `bus_dispatch_admin` fn) is RETIRED: every request now names its
// instance on the wire (`BusEnvelope`, §3.1), and authority is the
// addressed instance's addon permission matrix (`bus.read`/`bus.write`/
// `bus.admin`) — for admin variants, the DOUBLE LOCK of `gate_admin`
// (matrix `bus.admin` AND the caller's org Admin role, see `gate_admin`'s
// own doc). Every request/response variant is registered via
// `inventory::submit!` (wzór `dispatch/benchmark.rs:248-298`), all pointing
// at `bus_dispatch`'s own macro-generated dispatch wrapper.
//
// BLOCKING: `BusService::publish`/`open_consumer`/`ConsumerHandle::fetch`
// are synchronous and may block on disk I/O or a bounded sleep (see
// `bus/mod.rs`'s own module-level BLOCKING warning). Every call into them
// from this file goes through `run_blocking`, which wraps
// `tokio::task::spawn_blocking` — never called directly from an async body.
//
// MessagesBrowse/DlqList (follow-up toru P task 1): both now call
// `BusService::peek` — a stateless, one-shot, no-`bus_groups`-row read added
// specifically for this UI surface — instead of opening a throwaway
// consumer under a random group name the way M1's first cut did. `peek`
// audits `bus.messages.browse` itself (once per partition it reads that
// actually returns >= 1 record — P3-5 follow-up, `KRYTYK-M1-R3.md`: an
// empty read of a partition is not a data access), so this file no longer
// writes that row a second time. See `peek_topic`'s doc for how a
// multi-partition browse's `limit`/`from_offset(s)` map onto per-partition
// `peek` calls.
//
// `TopicDetail`'s per-partition figures (`log_end_offset`/`earliest_offset`/
// `size_bytes`/`segments`) similarly come from `BusService::partition_stats`
// (also follow-up toru P task 1/3) — another no-consumer-session read.
//
// M1-R2 review N-1/N-7, coordinator decisions 1/3: this file used to keep a
// fixed, reused `tf-system-probe` ("PROBE_GROUP") consumer group alive
// purely to read a partition's high watermark via `lag()` on a group that
// never actually consumes anything — `GroupDetail`'s per-partition high
// watermark and `OffsetReset::Latest` both used it. Both now read
// `BusService::partition_stats` instead (a no-consumer-session,
// no-`bus_groups`-row read), so nothing in this file opens that probe
// consumer anymore. `OffsetReset::Earliest` also used to go through a REAL
// consumer's `seek_to_earliest` (which internally commits via the same
// monotonicity-guarded path a normal consumer's `commit` uses) — it now
// goes through the same `svc.reset_offset` admin path
// (`force_commit`/`bus.offset.reset` audit) every other reset mode already
// used, since `Earliest` moving a commit BACKWARD is exactly `reset_offset`'s
// job, not a real consumer's. `GROUP_ID_HIDDEN_PREFIX` below is defense in
// depth against a leftover `tf-*`-named row (the old probe's, or a future
// one) ever surfacing in `GroupList`/KPI counts/lag totals — `bus::init`
// also deletes any leftover legacy `tf-system-probe` row outright.
//
// ACL/RBAC split: see `services::bus_authorizer`'s module doc for exactly
// what the per-topic ACL layer does and does NOT model (no produce/consume/
// admin action column on `resource_permissions`).

use bytes::Bytes;
use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::{
    BusAclEntryWire, BusBrowsePartitionInfoWire, BusCapabilitiesWire, BusDlqListResultWire,
    BusDlqRecordWire, BusFailoverEventWire, BusFieldPolicyWire, BusGroupDetailWire,
    BusGroupLagSummaryWire, BusGroupPartitionDetailWire, BusGroupSummaryWire, BusHeaderWire,
    BusMessagePreviewWire, BusMessagesBrowseResultWire, BusOffsetResetMode, BusPartitionInfoWire,
    BusPartitionOffsetWire, BusPartitionReplicaWire, BusPayload, BusQuotaWire, BusReplicaLagWire,
    BusReplicaNodeWire, BusSchemaSubjectWire, BusSchemaVersionWire, BusStatsSnapshotWire,
    BusTopicConfigWire, BusTopicOptionsWire, BusTopicStatsWire, BusTopicSummaryWire, MessageBody,
    ProtocolError, ProtocolErrorCode,
};

use super::HandlerContext;
use crate::bus::{
    self, dlq, field_policies, groups, instance::BusInstanceId, quota, schema_registry, topics,
    BusCallContext, BusServiceError, PartitionReplicaInfo, ReplError, ReplicaLagInfo,
    ReplicaNodeInfo, UnavailableReason,
};
use crate::db::repository;
use crate::dispatch::app_gate;
use crate::dispatch::SessionAuthKind;
use crate::services::rbac::OrgContext;

const PERM_READ: &str = "bus.read";
const PERM_WRITE: &str = "bus.write";
const PERM_ADMIN: &str = "bus.admin";

/// Any group whose id starts with this prefix is internal/ephemeral
/// tooling, never a real consumer — hidden from `GroupList`, group KPI
/// counts (`StatsSnapshot.group_count`/`paused_group_count`), and lag
/// totals (M1-R2 review N-7, coordinator decision 3). Currently the only
/// group ever created under it was the now-retired `tf-system-probe`
/// (`bus::LEGACY_PROBE_GROUP_ID`, deleted outright at `bus::init`) — this
/// filter stays even though nothing in this file creates a `tf-`-prefixed
/// group anymore, as defense in depth against a leftover row (or a future
/// internal tool reusing the convention) ever leaking into the UI.
const GROUP_ID_HIDDEN_PREFIX: &str = "tf-";

fn is_hidden_group(group_id: &str) -> bool {
    group_id.starts_with(GROUP_ID_HIDDEN_PREFIX)
}

/// Per-record payload preview budget (PLAN §6.2's "podgląd... zredagowany" —
/// this is the per-row cut, independent of the 1 MiB TOTAL response budget
/// `fetch`'s own `max_bytes` argument enforces at the source).
const PREVIEW_MAX_BYTES: usize = 4096;

/// Server-side ceiling for `MessagesBrowseRequest`/`DlqListRequest.limit`
/// (PLAN §6.2: "≤ 100 rekordów").
const BROWSE_MAX_RECORDS: u32 = 100;

/// Server-side ceiling for `DlqRetryAllRequest.max_records` (PLAN §6.2:
/// "Ponów wszystkie" — bounded batch, never "retry the entire DLQ" in one
/// call).
const DLQ_RETRY_ALL_MAX: u32 = 500;

fn require_org(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    ctx.org_context
        .as_ref()
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required"))
}

/// plan-app-platform §4.2: one gated call's resolved context — the
/// instance, its running engine, and the caller. Replaces the W4 stopgap
/// (`resolve_instance_addon_id`/`resolve_instance_id` via `app_gate::
/// sole_enabled_instance`, finding 2): the instance now comes from the
/// `BusEnvelope` the caller addressed on the wire, never a guess, so two
/// open instance screens can never cross-talk through an ambiguous
/// default.
struct Gate {
    instance: BusInstanceId,
    svc: std::sync::Arc<bus::BusService>,
    org_id: String,
    user_id: String,
}

/// `BusInstanceId::parse` (shape only, before any DB read — the trust
/// boundary for an externally supplied id) → the addon permission matrix of
/// THAT instance (`app_gate::require_instance_permission`: existence,
/// enabled, and the caller's grant — `AppUnavailable`/`PolicyDenied`) → the
/// running engine (`bus::instance`, `None` only when the DB says enabled
/// but nothing actually started it on this node yet — a narrower race than
/// the app gate's own check, same `AppUnavailable` code either way).
fn gate(ctx: &HandlerContext, instance_id: &str, permission: &str) -> Result<Gate, ProtocolError> {
    let id = BusInstanceId::parse(instance_id)
        .map_err(|e| ProtocolError::bad_request(format!("bus.invalid_instance_id: {e}")))?;
    app_gate::require_instance_permission(ctx, BusInstanceId::PACKAGE_ID, instance_id, permission)?;
    let svc = bus::instance(&id).ok_or_else(|| {
        ProtocolError::new(
            ProtocolErrorCode::AppUnavailable,
            "the bus is not running on this node",
        )
    })?;
    let org = require_org(ctx)?;
    Ok(Gate {
        instance: id,
        svc,
        org_id: org.org_id.clone(),
        user_id: org.user_id.clone(),
    })
}

fn gate_read(ctx: &HandlerContext, instance_id: &str) -> Result<Gate, ProtocolError> {
    gate(ctx, instance_id, PERM_READ)
}

fn gate_write(ctx: &HandlerContext, instance_id: &str) -> Result<Gate, ProtocolError> {
    gate(ctx, instance_id, PERM_WRITE)
}

/// DOUBLE LOCK (plan-app-platform §4.2, owner decision): `bus.admin` in the
/// instance matrix AND the caller's org Admin role. The matrix can delegate
/// `bus.admin` to a non-admin operator; the org role cannot be delegated —
/// every destructive/admin action needs both. `bus_dispatch_admin` (the
/// separate site-Admin-tier dispatch fn + `register_bus_admin_variant!`)
/// is retired: every admin variant now runs at `UserSession` tier through
/// this gate instead of a site-admin session tier, which is at least as
/// strong for a delegated operator and strictly more auditable per
/// instance (the matrix grant + org role are BOTH instance/org scoped;
/// `SessionAuthKind::Admin` was neither).
fn gate_admin(ctx: &HandlerContext, instance_id: &str) -> Result<Gate, ProtocolError> {
    let g = gate(ctx, instance_id, PERM_ADMIN)?;
    if !ctx.org_context.as_ref().is_some_and(|o| o.has("org.admin")) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "org Admin role required",
        ));
    }
    Ok(g)
}

/// Builds a `BusCallContext` from an already-resolved `Gate` — `actor` is
/// the session's `user_id` (same string `PermissionMatrix`/`log_audit` key
/// against), `origin` is always `"ui"` (this dispatch surface is the ONLY
/// caller of `bus::*` that speaks for the dashboard, as opposed to flow/
/// addon/mesh origins PLAN §6.1 also describes but which land through
/// different call sites entirely).
fn bus_ctx(ctx: &HandlerContext, g: &Gate) -> BusCallContext {
    BusCallContext {
        instance_id: g.instance.clone(),
        org_id: g.org_id.clone(),
        actor: Some(g.user_id.clone()),
        correlation_id: Some(ctx.correlation_id.to_string()),
        origin: "ui".to_string(),
    }
}

/// Resolves the engine for a `BusCallContext` that already names its
/// instance — used by helpers below the gate (`peek_topic`/
/// `filter_out_discarded`) that only carry a `bctx`, not the whole `Gate`.
/// Same `AppUnavailable` shape `gate` itself uses; this is what `service()`/
/// `bus::global()` (the W4 single-instance shim) used to paper over here —
/// those helpers previously resolved the shim's "the one running engine"
/// instead of `bctx.instance_id`, silently correct only because exactly one
/// instance ever ran on this node.
fn instance_service(id: &BusInstanceId) -> Result<std::sync::Arc<bus::BusService>, ProtocolError> {
    bus::instance(id).ok_or_else(|| {
        ProtocolError::new(
            ProtocolErrorCode::AppUnavailable,
            "the bus is not running on this node",
        )
    })
}

/// Runs a blocking `bus::*` call off the async runtime's worker thread (see
/// this file's module-level BLOCKING doc). The closure returns a
/// `ProtocolError` directly so every call site maps its own domain error
/// (`map_bus_error`/`db_err`) INSIDE the closure, not after the join.
async fn run_blocking<T, F>(f: F) -> Result<T, ProtocolError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ProtocolError> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ProtocolError::internal(format!("bus.blocking_task_failed: {e}")))?
}

fn db_err(scope: &str, error: anyhow::Error) -> ProtocolError {
    tracing::warn!(scope, error = %error, "tentabus database error");
    ProtocolError::internal(format!("bus.db_error: {scope}"))
}

/// Maps every `BusServiceError` variant to a `ProtocolError` carrying a
/// stable `bus.<snake_case>` code as the FIRST token of `message` (PLAN
/// §6.2: "błędy mapowane... ze stabilnymi kodami stringowymi") — this
/// crate's `ProtocolError` has no separate string-code field, only the
/// coarse `ProtocolErrorCode` enum, so the stable code rides in `message`
/// exactly like every other domain-specific error prefix already used
/// throughout `dispatch/`.
fn map_bus_error(e: BusServiceError) -> ProtocolError {
    match e {
        BusServiceError::NotInitialized => {
            ProtocolError::internal("bus.not_initialized: TentaBus is not running")
        }
        BusServiceError::Db(m) => ProtocolError::internal(format!("bus.db_error: {m}")),
        BusServiceError::Fjall(m) => ProtocolError::internal(format!("bus.fjall_error: {m}")),
        BusServiceError::Codec(m) => ProtocolError::internal(format!("bus.codec_error: {m}")),
        BusServiceError::Io(m) => ProtocolError::internal(format!("bus.io_error: {m}")),
        BusServiceError::Engine(err) => {
            ProtocolError::internal(format!("bus.engine_error: {err}"))
        }
        BusServiceError::InvalidTopicName { name, reason } => ProtocolError::bad_request(
            format!("bus.invalid_topic_name: '{name}' {reason}"),
        ),
        BusServiceError::InvalidTopicConfig { reason } => {
            ProtocolError::bad_request(format!("bus.invalid_topic_config: {reason}"))
        }
        BusServiceError::CorruptTopicRow { name, field, value } => ProtocolError::internal(
            format!("bus.corrupt_topic_row: {name}.{field}='{value}'"),
        ),
        BusServiceError::TopicAlreadyExists { name } => ProtocolError::new(
            ProtocolErrorCode::Conflict,
            format!("bus.topic_already_exists: '{name}'"),
        ),
        BusServiceError::TopicNotFound { name } => {
            ProtocolError::not_found(format!("bus.topic_not_found: '{name}'"))
        }
        BusServiceError::PermissionDenied { action, topic } => ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            format!("bus.permission_denied: {action} on '{topic}'"),
        ),
        BusServiceError::QuotaExceeded { retry_after_ms } => ProtocolError::new(
            ProtocolErrorCode::RateLimited,
            format!("bus.quota_exceeded: retry after {retry_after_ms} ms"),
        ),
        BusServiceError::QuotaRequestTooLarge {
            unit,
            amount,
            capacity,
        } => ProtocolError::bad_request(format!(
            "bus.quota_request_too_large: {amount} {unit} exceeds capacity {capacity} {unit}/s"
        )),
        BusServiceError::MaxTopicsExceeded {
            org_id: _,
            max,
            current,
        } => ProtocolError::bad_request(format!(
            "bus.max_topics_exceeded: {current}/{max}"
        )),
        BusServiceError::MaxPartitionsExceeded {
            org_id: _,
            max,
            current,
            requested,
        } => ProtocolError::bad_request(format!(
            "bus.max_partitions_exceeded: {current} existing + {requested} requested > {max}"
        )),
        BusServiceError::Throttled { retry_after_ms } => ProtocolError::new(
            ProtocolErrorCode::RateLimited,
            format!("bus.throttled: retry after {retry_after_ms} ms"),
        ),
        BusServiceError::PayloadTooLarge {
            len,
            max_inline_bytes,
        } => ProtocolError::bad_request(format!(
            "bus.payload_too_large: {len} bytes exceeds max_inline_bytes {max_inline_bytes}"
        )),
        BusServiceError::DedupKeyRequired { topic } => ProtocolError::bad_request(format!(
            "bus.dedup_key_required: topic '{topic}'"
        )),
        BusServiceError::ProducerFenced { current_epoch } => ProtocolError::new(
            ProtocolErrorCode::Conflict,
            format!("bus.producer_fenced: current epoch {current_epoch}"),
        ),
        BusServiceError::EnvironmentMismatch {
            topic_env,
            node_env,
        } => ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            format!(
                "bus.environment_mismatch: topic={} node={}",
                topic_env.as_str(),
                node_env.as_str()
            ),
        ),
        BusServiceError::InvalidArgument(msg) => {
            ProtocolError::bad_request(format!("bus.invalid_argument: {msg}"))
        }
        BusServiceError::NotSubscribed { topic, partition } => ProtocolError::bad_request(
            format!("bus.not_subscribed: '{topic}'/{partition}"),
        ),
        BusServiceError::OffsetRegression {
            topic,
            partition,
            requested,
            committed,
        } => ProtocolError::bad_request(format!(
            "bus.offset_regression: '{topic}'/{partition} requested={requested} < committed={committed}"
        )),
        BusServiceError::OffsetOutOfRange {
            topic,
            partition,
            requested,
            earliest,
            latest,
        } => ProtocolError::bad_request(format!(
            "bus.offset_out_of_range: '{topic}'/{partition} requested={requested} earliest={earliest} latest={latest}"
        )),
        BusServiceError::GroupPaused { group, topic } => ProtocolError::new(
            ProtocolErrorCode::Conflict,
            format!("bus.group_paused: '{group}' on '{topic}'"),
        ),
        BusServiceError::DlqOfDlqNotAllowed { topic } => {
            ProtocolError::bad_request(format!("bus.dlq_of_dlq_not_allowed: '{topic}'"))
        }
        BusServiceError::PartitionPoisoned { topic, partition } => ProtocolError::internal(
            format!("bus.partition_poisoned: '{topic}'/{partition}"),
        ),
        BusServiceError::PartialPublish { acked, source } => ProtocolError::internal(format!(
            "bus.partial_publish: {} partition(s) already applied before failing: {source}",
            acked.len()
        )),
        // WHY: dedicated variant (follow-up toru P, task 6) replacing the
        // earlier `InvalidArgument(MAX_GROUPS_EXCEEDED_PREFIX)` workaround —
        // reuses the SAME stable code string via `bus::MAX_GROUPS_EXCEEDED_
        // PREFIX` so no client-visible error code changes.
        BusServiceError::MaxGroupsExceeded {
            org_id: _,
            max,
            current,
        } => ProtocolError::bad_request(format!(
            "{}: {current}/{max}",
            bus::MAX_GROUPS_EXCEEDED_PREFIX
        )),
        // M2 (PLAN-M2 §1e): `Conflict` for `NotLeader` — same "retry
        // against a different node/target" shape as `TopicAlreadyExists`/
        // `ProducerFenced` above, not a client input error. `NotAvailable`
        // for the other three: the partition/replica set genuinely cannot
        // serve the request right now, not a bad request or a stale write.
        BusServiceError::NotLeader {
            leader_node_id,
            leader_epoch,
        } => ProtocolError::new(
            ProtocolErrorCode::Conflict,
            format!(
                "bus.not_leader: leader_node_id={:?} leader_epoch={leader_epoch}",
                leader_node_id
            ),
        ),
        BusServiceError::NotEnoughReplicas { isr, required } => ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            format!("bus.not_enough_replicas: isr={isr} required={required}"),
        ),
        BusServiceError::AckTimeout { acked, required } => ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            format!("bus.ack_timeout: acked={acked} required={required}"),
        ),
        BusServiceError::PartitionUnavailable { reason } => ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            format!("bus.partition_unavailable: {reason}"),
        ),
        // SUM/tentabus/POLITYKI-POL.md: a field policy rejected this
        // request/payload — a client input error, same shape as
        // `InvalidArgument` above.
        BusServiceError::FieldNotAllowed { topic, fields } => ProtocolError::bad_request(format!(
            "bus.field_not_allowed: topic '{topic}' fields={fields:?}"
        )),
        BusServiceError::RequiredFieldMissing { topic, fields } => ProtocolError::bad_request(
            format!("bus.required_field_missing: topic '{topic}' fields={fields:?}"),
        ),
        BusServiceError::FieldPolicyPayloadMalformed { topic, format } => ProtocolError::bad_request(
            format!("bus.field_policy_payload_malformed: topic '{topic}' expected {format}"),
        ),
        // SUM/tentabus/PLAN-F3.md §4.7/§6 — schema registry errors.
        // `SchemaViolation`/`SchemaIncompatible`/`SchemaTypeUnsupported`/
        // `SchemaRefIdCollision` are all caller input problems (a bad
        // payload, an incompatible new version, an operation this build's
        // schema kind cannot perform, an astronomically unlikely hash
        // collision the caller can retry past) — `bad_request`, same shape
        // as the field-policy errors above. `SchemaNotFound`/
        // `SchemaVersionNotFound` name a missing resource, like
        // `TopicNotFound` above.
        BusServiceError::SchemaViolation {
            topic,
            subject,
            version,
            detail,
        } => ProtocolError::bad_request(format!(
            "bus.schema_violation: topic '{topic}' subject '{subject}' version {version}: {detail}"
        )),
        BusServiceError::SchemaNotFound { subject } => {
            ProtocolError::not_found(format!("bus.schema_not_found: '{subject}'"))
        }
        BusServiceError::SchemaVersionNotFound { subject, version } => ProtocolError::not_found(
            format!("bus.schema_version_not_found: '{subject}' version {version}"),
        ),
        BusServiceError::SchemaIncompatible {
            subject,
            mode,
            detail,
        } => ProtocolError::bad_request(format!(
            "bus.schema_incompatible: '{subject}' mode={mode}: {detail}"
        )),
        BusServiceError::SchemaTypeUnsupported {
            schema_type,
            operation,
        } => ProtocolError::bad_request(format!(
            "bus.schema_type_unsupported: '{}' does not support {operation}",
            schema_type.as_str()
        )),
        BusServiceError::SchemaRefIdCollision { subject, version } => ProtocolError::bad_request(
            format!("bus.schema_ref_id_collision: '{subject}' version {version}"),
        ),
        // plan-app-platform §7 W4 findings 1/4: these two variants are the
        // engine's own last-line defence against a mis-resolved instance —
        // they should never actually surface through a correctly-wired
        // dispatch gate (which already resolved `bctx.instance_id` from the
        // same lookup as `service()`/`require_read`/`require_admin`), but if
        // one ever does, it means an internal wiring bug rather than caller
        // input, so `internal` rather than `bad_request`.
        BusServiceError::InstanceMismatch {
            engine_instance,
            ctx_instance,
        } => ProtocolError::internal(format!(
            "bus.instance_mismatch: request addressed '{ctx_instance}' but resolved engine '{engine_instance}'"
        )),
        BusServiceError::AuthorizerInstanceMismatch {
            engine_instance,
            authorizer_instance,
        } => ProtocolError::internal(format!(
            "bus.authorizer_instance_mismatch: authorizer wired for '{authorizer_instance}' but engine is '{engine_instance}'"
        )),
    }
}

/// Maps `ReplError` (PLAN-M2 §1e) — the narrower error surface
/// `ReplicationCoordinator::{reassign,transfer_leader}` return directly,
/// separate from `BusServiceError`/`map_bus_error` above — to a stable
/// `bus.<snake_case>` code, same convention as `map_bus_error`. Only
/// `Reassign`/`LeaderTransfer` (`bus_dispatch_admin`) call into this: every
/// other M2 write path (`publish`/`open_consumer`/`fetch`/`commit`, agent
/// S's territory) maps its own `ReplError` into `BusServiceError` BEFORE it
/// ever reaches this dispatch layer, so `map_bus_error` alone covers it.
fn map_repl_error(e: ReplError) -> ProtocolError {
    match e {
        ReplError::NoAssignment { topic, partition } => ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            format!("bus.partition_unavailable: no partition assignment for '{topic}'/{partition}"),
        ),
        ReplError::NotEnoughReplicas {
            topic,
            partition,
            isr,
            required,
        } => ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            format!("bus.not_enough_replicas: '{topic}'/{partition} isr={isr} required={required}"),
        ),
        ReplError::NotAReplica {
            topic,
            partition,
            node_id,
        } => ProtocolError::bad_request(format!(
            "bus.not_a_replica: '{node_id}' is not a replica of '{topic}'/{partition}"
        )),
        ReplError::EpochFenced { have, requested } => ProtocolError::new(
            ProtocolErrorCode::Conflict,
            format!("bus.not_leader: epoch fenced have={have} requested={requested}"),
        ),
        ReplError::Internal(msg) => {
            ProtocolError::internal(format!("bus.replication_internal_error: {msg}"))
        }
    }
}

fn parse_opt<T>(
    field: &'static str,
    value: Option<String>,
    parse: impl Fn(&str) -> Option<T>,
) -> Result<Option<T>, ProtocolError> {
    match value {
        None => Ok(None),
        Some(s) => parse(&s).map(Some).ok_or_else(|| {
            ProtocolError::bad_request(format!("bus.invalid_field: {field} = '{s}'"))
        }),
    }
}

/// Wire sentinel for `BusTopicOptionsWire.durability` (see that field's own
/// doc): never a valid `DurabilityPolicy` string, stripped out here into
/// `TopicOptions::durability_reset_to_class` instead of being parsed.
const DURABILITY_AUTO_SENTINEL: &str = "auto";

fn topic_options_from_wire(w: BusTopicOptionsWire) -> Result<topics::TopicOptions, ProtocolError> {
    let durability_reset_to_class = w.durability.as_deref() == Some(DURABILITY_AUTO_SENTINEL);
    let durability_wire = if durability_reset_to_class {
        None
    } else {
        w.durability
    };
    Ok(topics::TopicOptions {
        partitions: w.partitions,
        retention_ms: w.retention_ms,
        retention_bytes_per_partition: w.retention_bytes_per_partition,
        cleanup_policy: parse_opt(
            "cleanup_policy",
            w.cleanup_policy,
            topics::CleanupPolicy::parse,
        )?,
        delivery: parse_opt("delivery", w.delivery, topics::DeliveryMode::parse)?,
        idempotency_key: w.idempotency_key,
        dedup_window_ms: w.dedup_window_ms,
        max_delivery_attempts: w.max_delivery_attempts,
        retry_backoff_ms: w.retry_backoff_ms,
        schema_id: w.schema_id,
        validation: parse_opt("validation", w.validation, topics::ValidationMode::parse)?,
        content_type: w.content_type,
        replication_factor: w.replication_factor,
        acks: parse_opt("acks", w.acks, topics::Acks::parse)?,
        durability: parse_opt(
            "durability",
            durability_wire,
            topics::DurabilityPolicy::parse,
        )?,
        durability_class: parse_opt(
            "durability_class",
            w.durability_class,
            topics::DurabilityClass::parse,
        )?,
        durability_reset_to_class,
        max_inline_bytes: w.max_inline_bytes.map(|v| v as usize),
        compression: parse_opt(
            "compression",
            w.compression,
            topics::CompressionPolicy::parse,
        )?,
    })
}

fn topic_config_to_wire(cfg: &topics::TopicConfig) -> BusTopicConfigWire {
    BusTopicConfigWire {
        name: cfg.name.clone(),
        partitions: cfg.partitions,
        retention_ms: cfg.retention_ms,
        retention_bytes_per_partition: cfg.retention_bytes_per_partition,
        cleanup_policy: cfg.cleanup_policy.as_str().to_string(),
        delivery: cfg.delivery.as_str().to_string(),
        idempotency_key: cfg.idempotency_key.clone(),
        dedup_window_ms: cfg.dedup_window_ms,
        max_delivery_attempts: cfg.max_delivery_attempts,
        retry_backoff_ms: cfg.retry_backoff_ms,
        schema_id: cfg.schema_id.clone(),
        validation: cfg.validation.as_str().to_string(),
        content_type: cfg.content_type.clone(),
        replication_factor: cfg.replication_factor,
        acks: cfg.acks.as_str().to_string(),
        durability: cfg.durability.to_wire_string(),
        durability_class: cfg.durability_class().as_str().to_string(),
        durability_explicit: cfg.durability_explicit(),
        max_inline_bytes: cfg.max_inline_bytes as u64,
        compression: cfg.compression.as_str().to_string(),
        environment: cfg.environment.as_str().to_string(),
        created_at_ms: cfg.created_at_ms,
        updated_at_ms: cfg.updated_at_ms,
    }
}

fn topic_config_to_summary_wire(cfg: &topics::TopicConfig) -> BusTopicSummaryWire {
    BusTopicSummaryWire {
        name: cfg.name.clone(),
        partitions: cfg.partitions,
        retention_ms: cfg.retention_ms,
        replication_factor: cfg.replication_factor,
        acks: cfg.acks.as_str().to_string(),
        environment: cfg.environment.as_str().to_string(),
        cleanup_policy: cfg.cleanup_policy.as_str().to_string(),
        created_at_ms: cfg.created_at_ms,
        updated_at_ms: cfg.updated_at_ms,
        is_dlq: cfg.name.starts_with(dlq::DLQ_TOPIC_PREFIX),
        durability: cfg.durability.to_wire_string(),
        durability_class: cfg.durability_class().as_str().to_string(),
        durability_explicit: cfg.durability_explicit(),
    }
}

/// Heuristic BlobRef detector (PLAN §2.4/§6.2 D11): `true` iff `payload`
/// parses as a JSON object carrying (at least) the four
/// `flow_engine::blob_store::BlobRef` fields. No M1 producer in this repo
/// actually emits one yet (that is M3a's `bus_publish` flow block) — this
/// exists so a preview never re-inlines a large blob's bytes SHOULD a
/// future producer (an addon via M3b host functions, or a hand-rolled
/// in-process publisher) already write one, rather than silently treating
/// it like any other payload.
fn looks_like_blob_ref(payload: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(payload) else {
        return false;
    };
    let Some(obj) = value.as_object() else {
        return false;
    };
    obj.get("id").is_some_and(|v| v.is_string())
        && obj.get("size_bytes").is_some_and(|v| v.is_u64())
        && obj.get("mime").is_some_and(|v| v.is_string())
        && obj.get("sha256").is_some_and(|v| v.is_string())
}

fn headers_to_wire(headers: &[(Bytes, Bytes)]) -> Vec<BusHeaderWire> {
    headers
        .iter()
        .map(|(k, v)| BusHeaderWire {
            key: String::from_utf8_lossy(k).into_owned(),
            value: v.to_vec(),
        })
        .collect()
}

fn record_to_preview(rv: &bus::FetchedRecordMeta) -> BusMessagePreviewWire {
    let is_blob_ref = looks_like_blob_ref(&rv.payload);
    let (payload_preview, truncated) = if is_blob_ref {
        (rv.payload.to_vec(), false)
    } else if rv.payload.len() > PREVIEW_MAX_BYTES {
        (rv.payload[..PREVIEW_MAX_BYTES].to_vec(), true)
    } else {
        (rv.payload.to_vec(), false)
    };
    BusMessagePreviewWire {
        partition: rv.partition,
        offset: rv.offset,
        timestamp_ms: rv.timestamp_ms,
        key: rv.key.as_deref().map(|k| k.to_vec()).unwrap_or_default(),
        headers: headers_to_wire(&rv.headers),
        payload_preview,
        is_blob_ref,
        truncated,
    }
}

fn record_to_dlq_wire(rv: &bus::FetchedRecordMeta) -> BusDlqRecordWire {
    let is_blob_ref = looks_like_blob_ref(&rv.payload);
    let (payload_preview, truncated) = if is_blob_ref {
        (rv.payload.to_vec(), false)
    } else if rv.payload.len() > PREVIEW_MAX_BYTES {
        (rv.payload[..PREVIEW_MAX_BYTES].to_vec(), true)
    } else {
        (rv.payload.to_vec(), false)
    };
    BusDlqRecordWire {
        partition: rv.partition,
        offset: rv.offset,
        timestamp_ms: rv.timestamp_ms,
        key: rv.key.as_deref().map(|k| k.to_vec()).unwrap_or_default(),
        headers: headers_to_wire(&rv.headers),
        payload_preview,
        is_blob_ref,
        truncated,
    }
}

/// Builds the per-partition starting-offset map `peek_topic` reads from a
/// `MessagesBrowseRequest`/`DlqListRequest` (follow-up toru P task 1):
/// `from_offsets` wins whenever it is non-empty (a partition it does not
/// list still starts at its own earliest retained offset, NOT at the legacy
/// scalar); otherwise the legacy scalar `from_offset`, if present, applies
/// to every partition (the exact behavior a client built before
/// `from_offsets` existed already gets); an empty map (both absent) means
/// "every partition from its own earliest retained offset".
fn resolve_partition_starts(
    from_offset: Option<u64>,
    from_offsets: &[BusPartitionOffsetWire],
    partitions: u32,
) -> std::collections::HashMap<u32, u64> {
    if !from_offsets.is_empty() {
        return from_offsets
            .iter()
            .map(|o| (o.partition, o.offset))
            .collect();
    }
    match from_offset {
        Some(start) => (0..partitions).map(|p| (p, start)).collect(),
        None => std::collections::HashMap::new(),
    }
}

/// Server-side partition filter for `MessagesBrowseRequest`/`DlqListRequest`
/// (R3-2 follow-up, `KRYTYK-M1-R3.md`): `Some(p)` must already have been
/// checked against the topic's partition count by
/// `validate_partition_filter` before this runs, so `peek_topic` itself
/// never rejects `p` — it only decides which partition(s) to iterate.
///
/// Validates `partition` (if present) against `partitions` (the topic's
/// actual partition count) and returns the STABLE wire error code
/// `bus.partition_out_of_range` on a miss — kept as its own code (rather
/// than folded into the generic `bus.invalid_argument` `BusService::peek`
/// itself would raise for the same condition) because the UI needs to
/// distinguish "you asked for a partition that does not exist" from every
/// other `InvalidArgument` this family of requests can produce.
fn validate_partition_filter(
    partition: Option<u32>,
    partitions: u32,
    topic: &str,
) -> Result<(), ProtocolError> {
    if let Some(p) = partition {
        if p >= partitions {
            return Err(ProtocolError::bad_request(format!(
                "bus.partition_out_of_range: partition {p} out of range for topic '{topic}' ({partitions} partition(s))"
            )));
        }
    }
    Ok(())
}

/// Shared read-only fetch behind `MessagesBrowse`/`DlqList` (follow-up toru
/// P task 1): calls `BusService::peek` once per partition — no throwaway
/// consumer, no `bus_groups` row, no per-message audit duplication (`peek`
/// audits `bus.messages.browse` itself) — and merges the results in
/// partition order, spending `limit`'s total record budget partition by
/// partition until it runs out. A partition absent from `starts` reads from
/// its OWN earliest retained offset: `peek` at offset 0 is retried at the
/// engine-reported `earliest` on `OffsetOutOfRange` (retention has moved
/// the floor past 0), but only when the caller did not explicitly ask for
/// offset 0 on that partition — an EXPLICIT out-of-range request still
/// fails loudly, exactly like `peek` already does for a caller that opens a
/// real consumer.
///
/// `partition_filter` (R3-2 follow-up): `Some(p)` restricts the whole read
/// to that single partition — the caller MUST have already validated `p`
/// against `partitions` via `validate_partition_filter`. `None` keeps the
/// original "walk every partition in order" behavior unchanged.
fn peek_topic(
    bctx: &BusCallContext,
    topic: &str,
    partitions: u32,
    starts: &std::collections::HashMap<u32, u64>,
    limit: u32,
    partition_filter: Option<u32>,
) -> Result<
    (
        Vec<bus::FetchedRecordMeta>,
        bool,
        u64,
        Vec<BusBrowsePartitionInfoWire>,
    ),
    ProtocolError,
> {
    let mut limit_left = limit.clamp(1, BROWSE_MAX_RECORDS) as usize;
    let svc = instance_service(&bctx.instance_id)?;
    let mut all_records = Vec::new();
    let partition_range: Vec<u32> = match partition_filter {
        Some(p) => vec![p],
        None => (0..partitions).collect(),
    };
    let mut partitions_wire = Vec::with_capacity(partition_range.len());
    let mut any_has_more = false;
    let mut max_next_offset = 0u64;

    for partition in partition_range {
        if limit_left == 0 {
            break;
        }
        let requested_start = starts.get(&partition).copied();
        let start = requested_start.unwrap_or(0);
        let result = match svc.peek(
            bctx,
            topic,
            partition,
            start,
            limit_left,
            bus::PEEK_MAX_BYTES,
        ) {
            Ok(r) => r,
            Err(BusServiceError::Engine(tentaflow_bus::BusError::OffsetOutOfRange {
                earliest,
                ..
            })) if requested_start.is_none() => svc
                .peek(
                    bctx,
                    topic,
                    partition,
                    earliest,
                    limit_left,
                    bus::PEEK_MAX_BYTES,
                )
                .map_err(map_bus_error)?,
            Err(e) => return Err(map_bus_error(e)),
        };
        let next_offset = result
            .records
            .iter()
            .map(|r| r.offset + 1)
            .max()
            .unwrap_or_else(|| start.max(result.earliest_offset));
        let has_more = next_offset < result.high_watermark;
        any_has_more |= has_more;
        max_next_offset = max_next_offset.max(next_offset);
        limit_left = limit_left.saturating_sub(result.records.len());
        partitions_wire.push(BusBrowsePartitionInfoWire {
            partition,
            earliest_offset: result.earliest_offset,
            high_watermark: result.high_watermark,
            next_offset,
            has_more,
        });
        all_records.extend(result.records);
    }
    Ok((all_records, any_has_more, max_next_offset, partitions_wire))
}

/// Drops every record in `records` (already fetched from `dlq_topic` via
/// `peek_topic`) that `BusService::dlq_discard` has marked handled — the
/// `DlqList`/`DlqRetryAll` half of M1-R2 review N-5, coordinator decision 2.
/// `BusService::peek` itself deliberately stays unaware of discard state
/// (`dlq_discard`'s own doc): it also backs `MessagesBrowse`, which never
/// reads a DLQ topic through this path, so discard-filtering belongs here,
/// one layer up, not inside `peek`. Calls `BusService::dlq_discarded_offsets`
/// once per DISTINCT partition present in `records`, not once per record.
fn filter_out_discarded(
    bctx: &BusCallContext,
    dlq_topic: &str,
    records: Vec<bus::FetchedRecordMeta>,
) -> Result<Vec<bus::FetchedRecordMeta>, ProtocolError> {
    let svc = instance_service(&bctx.instance_id)?;
    let mut discarded_by_partition: std::collections::HashMap<u32, std::collections::HashSet<u64>> =
        std::collections::HashMap::new();
    let mut out = Vec::with_capacity(records.len());
    for rv in records {
        if let std::collections::hash_map::Entry::Vacant(e) =
            discarded_by_partition.entry(rv.partition)
        {
            let discarded = svc
                .dlq_discarded_offsets(bctx, dlq_topic, rv.partition)
                .map_err(map_bus_error)?;
            e.insert(discarded);
        }
        if !discarded_by_partition[&rv.partition].contains(&rv.offset) {
            out.push(rv);
        }
    }
    Ok(out)
}

// =============================================================================
// bus_dispatch — #[policy(UserSession)]
//
// plan-app-platform §4.2/§7 W7: the former `bus_dispatch_admin` (site-Admin
// session tier, `#[policy(Admin)]`) is GONE — every admin variant listed in
// the old `register_bus_admin_variant!` block below now routes through
// THIS function at `UserSession` tier and is gated by `gate_admin` in its
// own handler body (the double lock: instance-matrix `bus.admin` AND the
// caller's org Admin role). This is a real session-tier change, not just a
// file move: an admin action no longer requires a site-admin SESSION, it
// requires the matrix grant + the org role, both scoped to the addressed
// instance/org — see `gate_admin`'s doc for why that is at least as strong
// and strictly more auditable per instance.
// =============================================================================

#[handler(variant = "BusBody", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn bus_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let envelope = match req {
        MessageBody::BusBody(e) => e,
        _ => return Err(ProtocolError::bad_request("expected BusBody")),
    };
    let instance_id = envelope.instance_id.as_str();
    let payload: BusPayload = match &envelope.payload {
        BusPayload::TopicListRequest => topic_list_v1(ctx, instance_id).await?,
        BusPayload::TopicCreateRequest { name, options } => {
            topic_create_v1(ctx, instance_id, name.clone(), options.clone()).await?
        }
        BusPayload::TopicUpdateRequest { name, options } => {
            topic_update_v1(ctx, instance_id, name.clone(), options.clone()).await?
        }
        BusPayload::TopicDeleteRequest { name } => {
            topic_delete_v1(ctx, instance_id, name.clone()).await?
        }
        BusPayload::TopicDetailRequest { name } => {
            topic_detail_v1(ctx, instance_id, name.clone()).await?
        }
        BusPayload::GroupListRequest => group_list_v1(ctx, instance_id).await?,
        BusPayload::GroupDetailRequest { group, topic } => {
            group_detail_v1(ctx, instance_id, group.clone(), topic.clone()).await?
        }
        BusPayload::GroupPauseRequest { group, topic } => {
            group_pause_v1(ctx, instance_id, group.clone(), topic.clone()).await?
        }
        BusPayload::GroupResumeRequest { group, topic } => {
            group_resume_v1(ctx, instance_id, group.clone(), topic.clone()).await?
        }
        BusPayload::OffsetResetRequest {
            group,
            topic,
            partition,
            mode,
        } => {
            offset_reset_v1(
                ctx,
                instance_id,
                group.clone(),
                topic.clone(),
                *partition,
                *mode,
            )
            .await?
        }
        BusPayload::MessagesBrowseRequest {
            topic,
            from_offset,
            from_offsets,
            limit,
            partition,
        } => {
            messages_browse_v1(
                ctx,
                instance_id,
                topic.clone(),
                *from_offset,
                from_offsets.clone(),
                *limit,
                *partition,
            )
            .await?
        }
        BusPayload::DlqListRequest {
            source_topic,
            from_offset,
            from_offsets,
            limit,
            partition,
        } => {
            dlq_list_v1(
                ctx,
                instance_id,
                source_topic.clone(),
                *from_offset,
                from_offsets.clone(),
                *limit,
                *partition,
            )
            .await?
        }
        BusPayload::DlqRetryRequest {
            source_topic,
            partition,
            offset,
        } => dlq_retry_v1(ctx, instance_id, source_topic.clone(), *partition, *offset).await?,
        BusPayload::DlqDiscardRequest {
            source_topic,
            partition,
            offset,
        } => dlq_discard_v1(ctx, instance_id, source_topic.clone(), *partition, *offset).await?,
        BusPayload::DlqRetryAllRequest {
            source_topic,
            max_records,
        } => dlq_retry_all_v1(ctx, instance_id, source_topic.clone(), *max_records).await?,
        BusPayload::AclListRequest { topic } => {
            acl_list_v1(ctx, instance_id, topic.clone()).await?
        }
        BusPayload::AclSetRequest {
            topic,
            subject_type,
            subject_id,
            access_level,
        } => {
            acl_set_v1(
                ctx,
                instance_id,
                topic.clone(),
                subject_type.clone(),
                subject_id.clone(),
                access_level.clone(),
            )
            .await?
        }
        BusPayload::FieldPolicyListRequest { topic } => {
            field_policy_list_v1(ctx, instance_id, topic.clone()).await?
        }
        BusPayload::FieldPolicySetRequest {
            topic,
            subject_type,
            subject_id,
            direction,
            fields,
            required_fields,
        } => {
            field_policy_set_v1(
                ctx,
                instance_id,
                topic.clone(),
                subject_type.clone(),
                subject_id.clone(),
                direction.clone(),
                fields.clone(),
                required_fields.clone(),
            )
            .await?
        }
        BusPayload::FieldPolicyDeleteRequest {
            topic,
            subject_type,
            subject_id,
            direction,
        } => {
            field_policy_delete_v1(
                ctx,
                instance_id,
                topic.clone(),
                subject_type.clone(),
                subject_id.clone(),
                direction.clone(),
            )
            .await?
        }
        BusPayload::StatsSnapshotRequest => stats_snapshot_v1(ctx, instance_id).await?,
        BusPayload::CapabilitiesRequest => capabilities_v1(ctx, instance_id).await?,
        BusPayload::QuotaGetRequest => quota_get_v1(ctx, instance_id)?,
        BusPayload::QuotaSetRequest {
            max_topics,
            max_partitions,
            max_bytes_total,
            produce_msgs_per_sec,
            produce_bytes_per_sec,
            max_groups,
        } => quota_set_v1(
            ctx,
            instance_id,
            *max_topics,
            *max_partitions,
            *max_bytes_total,
            *produce_msgs_per_sec,
            *produce_bytes_per_sec,
            *max_groups,
        )?,
        BusPayload::ReplicaListRequest { topic } => {
            replica_list_v1(ctx, instance_id, topic.clone()).await?
        }
        BusPayload::ReassignRequest {
            topic,
            partition,
            replicas,
        } => {
            replica_reassign_v1(
                ctx,
                instance_id,
                topic.clone(),
                *partition,
                replicas.clone(),
            )
            .await?
        }
        BusPayload::LeaderTransferRequest {
            topic,
            partition,
            target_node_id,
        } => {
            leader_transfer_v1(
                ctx,
                instance_id,
                topic.clone(),
                *partition,
                target_node_id.clone(),
            )
            .await?
        }
        BusPayload::SchemaSubjectListRequest {} => schema_subject_list_v1(ctx, instance_id).await?,
        BusPayload::SchemaVersionListRequest { subject } => {
            schema_version_list_v1(ctx, instance_id, subject.clone()).await?
        }
        BusPayload::SchemaGetRequest { subject, version } => {
            schema_get_v1(ctx, instance_id, subject.clone(), *version).await?
        }
        BusPayload::SchemaDerivedGetRequest {
            subject,
            version,
            topic,
            subject_type,
            subject_id,
            direction,
        } => {
            schema_derived_get_v1(
                ctx,
                instance_id,
                subject.clone(),
                *version,
                topic.clone(),
                subject_type.clone(),
                subject_id.clone(),
                direction.clone(),
            )
            .await?
        }
        BusPayload::SchemaRegisterRequest {
            subject,
            schema_type,
            schema_text,
            compatibility,
        } => {
            schema_register_v1(
                ctx,
                instance_id,
                subject.clone(),
                schema_type.clone(),
                schema_text.clone(),
                compatibility.clone(),
            )
            .await?
        }
        BusPayload::SchemaCompatibilitySetRequest {
            subject,
            compatibility,
        } => {
            schema_compatibility_set_v1(ctx, instance_id, subject.clone(), compatibility.clone())
                .await?
        }
        BusPayload::SchemaDeleteRequest {
            subject,
            version,
            deprecate_only,
        } => schema_delete_v1(ctx, instance_id, subject.clone(), *version, *deprecate_only).await?,

        BusPayload::TopicListResponse { .. }
        | BusPayload::TopicCreateResponse { .. }
        | BusPayload::TopicUpdateResponse { .. }
        | BusPayload::TopicDeleteResponse
        | BusPayload::TopicDetailResponse { .. }
        | BusPayload::GroupListResponse { .. }
        | BusPayload::GroupDetailResponse { .. }
        | BusPayload::GroupPauseResponse
        | BusPayload::GroupResumeResponse
        | BusPayload::MessagesBrowseResponse { .. }
        | BusPayload::DlqListResponse { .. }
        | BusPayload::DlqRetryResponse { .. }
        | BusPayload::DlqDiscardResponse
        | BusPayload::DlqRetryAllResponse { .. }
        | BusPayload::AclListResponse { .. }
        | BusPayload::StatsSnapshotResponse { .. }
        | BusPayload::CapabilitiesResponse { .. }
        | BusPayload::ReplicaListResponse { .. }
        | BusPayload::OffsetResetResponse { .. }
        | BusPayload::AclSetResponse
        | BusPayload::FieldPolicyListResponse { .. }
        | BusPayload::FieldPolicySetResponse
        | BusPayload::FieldPolicyDeleteResponse
        | BusPayload::QuotaGetResponse { .. }
        | BusPayload::QuotaSetResponse { .. }
        | BusPayload::ReassignResponse { .. }
        | BusPayload::LeaderTransferResponse { .. }
        | BusPayload::SchemaSubjectListResponse { .. }
        | BusPayload::SchemaVersionListResponse { .. }
        | BusPayload::SchemaGetResponse { .. }
        | BusPayload::SchemaDerivedGetResponse { .. }
        | BusPayload::SchemaRegisterResponse { .. }
        | BusPayload::SchemaCompatibilitySetResponse
        | BusPayload::SchemaDeleteResponse { .. } => {
            return Err(ProtocolError::bad_request(
                "variant is not routed through bus_dispatch (UserSession tier)",
            ))
        }
    };
    // Echo the SAME instance_id the request named (§3.1) — a client with
    // two open instance screens can attribute this response to the right
    // one without a second round trip.
    Ok(MessageBody::BusBody(tentaflow_protocol::BusEnvelope {
        instance_id: envelope.instance_id.clone(),
        payload,
    }))
}

macro_rules! register_bus_variant {
    ($variant:literal, $metric:literal) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: crate::dispatch::SessionAuthKind::UserSession,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_bus_dispatch,
            }
        }
    };
}

register_bus_variant!("BusTopicListRequest", "tentaflow_ws_handler_bus_topic_list");
register_bus_variant!(
    "BusTopicCreateRequest",
    "tentaflow_ws_handler_bus_topic_create"
);
register_bus_variant!(
    "BusTopicUpdateRequest",
    "tentaflow_ws_handler_bus_topic_update"
);
register_bus_variant!(
    "BusTopicDeleteRequest",
    "tentaflow_ws_handler_bus_topic_delete"
);
register_bus_variant!(
    "BusTopicDetailRequest",
    "tentaflow_ws_handler_bus_topic_detail"
);
register_bus_variant!("BusGroupListRequest", "tentaflow_ws_handler_bus_group_list");
register_bus_variant!(
    "BusGroupDetailRequest",
    "tentaflow_ws_handler_bus_group_detail"
);
register_bus_variant!(
    "BusGroupPauseRequest",
    "tentaflow_ws_handler_bus_group_pause"
);
register_bus_variant!(
    "BusGroupResumeRequest",
    "tentaflow_ws_handler_bus_group_resume"
);
register_bus_variant!(
    "BusMessagesBrowseRequest",
    "tentaflow_ws_handler_bus_messages_browse"
);
register_bus_variant!("BusDlqListRequest", "tentaflow_ws_handler_bus_dlq_list");
register_bus_variant!("BusDlqRetryRequest", "tentaflow_ws_handler_bus_dlq_retry");
register_bus_variant!(
    "BusDlqDiscardRequest",
    "tentaflow_ws_handler_bus_dlq_discard"
);
register_bus_variant!(
    "BusDlqRetryAllRequest",
    "tentaflow_ws_handler_bus_dlq_retry_all"
);
register_bus_variant!("BusAclListRequest", "tentaflow_ws_handler_bus_acl_list");
register_bus_variant!(
    "BusFieldPolicyListRequest",
    "tentaflow_ws_handler_bus_field_policy_list"
);
register_bus_variant!(
    "BusStatsSnapshotRequest",
    "tentaflow_ws_handler_bus_stats_snapshot"
);
register_bus_variant!(
    "BusCapabilitiesRequest",
    "tentaflow_ws_handler_bus_capabilities"
);
register_bus_variant!(
    "BusReplicaListRequest",
    "tentaflow_ws_handler_bus_replica_list"
);
register_bus_variant!(
    "BusSchemaSubjectListRequest",
    "tentaflow_ws_handler_bus_schema_subject_list"
);
register_bus_variant!(
    "BusSchemaVersionListRequest",
    "tentaflow_ws_handler_bus_schema_version_list"
);
register_bus_variant!("BusSchemaGetRequest", "tentaflow_ws_handler_bus_schema_get");
register_bus_variant!(
    "BusSchemaDerivedGetRequest",
    "tentaflow_ws_handler_bus_schema_derived_get"
);

// plan-app-platform §4.2/§7 W7: the 11 variants formerly routed through
// `bus_dispatch_admin` (`#[policy(Admin)]`, site-Admin session tier) via
// `register_bus_admin_variant!` — that dispatch fn and macro are deleted;
// every one of these now runs at `UserSession` tier through `bus_dispatch`
// above and is gated by `gate_admin` inside its own handler body (§4.3
// table). Every caller already had a live route here BEFORE the admin fn
// was removed (`bus_dispatch`'s match arms above call each `_v1` handler
// directly), so this is a rename of the registration tier, not a new path.
register_bus_variant!(
    "BusOffsetResetRequest",
    "tentaflow_ws_handler_bus_offset_reset"
);
register_bus_variant!("BusAclSetRequest", "tentaflow_ws_handler_bus_acl_set");
register_bus_variant!(
    "BusFieldPolicySetRequest",
    "tentaflow_ws_handler_bus_field_policy_set"
);
register_bus_variant!(
    "BusFieldPolicyDeleteRequest",
    "tentaflow_ws_handler_bus_field_policy_delete"
);
register_bus_variant!("BusQuotaGetRequest", "tentaflow_ws_handler_bus_quota_get");
register_bus_variant!("BusQuotaSetRequest", "tentaflow_ws_handler_bus_quota_set");
register_bus_variant!(
    "BusReassignRequest",
    "tentaflow_ws_handler_bus_replica_reassign"
);
register_bus_variant!(
    "BusLeaderTransferRequest",
    "tentaflow_ws_handler_bus_leader_transfer"
);
register_bus_variant!(
    "BusSchemaRegisterRequest",
    "tentaflow_ws_handler_bus_schema_register"
);
register_bus_variant!(
    "BusSchemaCompatibilitySetRequest",
    "tentaflow_ws_handler_bus_schema_compatibility_set"
);
register_bus_variant!(
    "BusSchemaDeleteRequest",
    "tentaflow_ws_handler_bus_schema_delete"
);

// =============================================================================
// Topics
// =============================================================================

async fn topic_list_v1(
    ctx: &HandlerContext,
    instance_id: &str,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_read(ctx, instance_id)?;
    let org_id = g.org_id.clone();
    let db = ctx.state.db.clone();
    let instance = g.instance.as_str().to_string();
    let topics =
        run_blocking(move || topics::list_topics(&db, &instance, &org_id).map_err(map_bus_error))
            .await?;
    let wire: Vec<BusTopicSummaryWire> = topics.iter().map(topic_config_to_summary_wire).collect();
    Ok(BusPayload::TopicListResponse { topics: wire })
}

async fn topic_create_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    name: String,
    options: BusTopicOptionsWire,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_admin(ctx, instance_id)?;
    let bctx = bus_ctx(ctx, &g);
    let opts = topic_options_from_wire(options)?;
    let svc = g.svc.clone();
    let cfg =
        run_blocking(move || svc.create_topic(&bctx, &name, opts).map_err(map_bus_error)).await?;
    Ok(BusPayload::TopicCreateResponse {
        topic: topic_config_to_wire(&cfg),
    })
}

async fn topic_update_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    name: String,
    options: BusTopicOptionsWire,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_admin(ctx, instance_id)?;
    let bctx = bus_ctx(ctx, &g);
    let opts = topic_options_from_wire(options)?;
    let svc = g.svc.clone();
    let cfg =
        run_blocking(move || svc.update_topic(&bctx, &name, opts).map_err(map_bus_error)).await?;
    Ok(BusPayload::TopicUpdateResponse {
        topic: topic_config_to_wire(&cfg),
    })
}

async fn topic_delete_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    name: String,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_admin(ctx, instance_id)?;
    let bctx = bus_ctx(ctx, &g);
    let svc = g.svc.clone();
    run_blocking(move || svc.delete_topic(&bctx, &name).map_err(map_bus_error)).await?;
    Ok(BusPayload::TopicDeleteResponse)
}

/// Per-partition `log_end_offset` and per-group lag summary (this file's
/// module doc, point 2) — both derived by opening consumers, never from a
/// dedicated stats getter (none exists on `BusService`'s public surface).
async fn topic_detail_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    name: String,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_read(ctx, instance_id)?;
    let bctx = bus_ctx(ctx, &g);
    let org_id = g.org_id.clone();
    let db = ctx.state.db.clone();
    let instance = g.instance.as_str().to_string();
    let name_for_cfg = name.clone();
    let cfg = run_blocking(move || {
        topics::get_topic(&db, &instance, &org_id, &name_for_cfg)
            .map_err(map_bus_error)?
            .ok_or_else(|| {
                ProtocolError::not_found(format!("bus.topic_not_found: '{name_for_cfg}'"))
            })
    })
    .await?;

    let local_db = g.svc.local_db().clone();
    let org_id2 = g.org_id.clone();
    let name2 = name.clone();
    let partitions_n = cfg.partitions;
    let group_rows = run_blocking(move || {
        repository::bus_group_list(&local_db, &org_id2)
            .map(|rows| {
                rows.into_iter()
                    .filter(|g| g.topic == name2 && !is_hidden_group(&g.group_id))
                    .collect::<Vec<_>>()
            })
            .map_err(|e| db_err("bus_group_list", e))
    })
    .await?;

    let name3 = name.clone();
    let bctx2 = bctx.clone();
    let org_id3 = g.org_id.clone();
    let local_node_id = ctx.state.local_node_id.to_string();
    let svc = g.svc.clone();
    // Follow-up toru P task 3: per-partition figures now come from
    // `BusService::partition_stats` — a no-consumer-session, no-`bus_groups`
    // -row read (unlike the earlier PROBE_GROUP-backed `open_consumer` this
    // replaced), which also supplies `earliest_offset`/`size_bytes`/
    // `segments` for free.
    //
    // M2 (PLAN-M2 §1f), additive: `leader_node_id`/`leader_epoch`/
    // `isr_count`/`replica_count`/`high_watermark` come from ONE
    // `coordinator.snapshot()` call for the whole topic (not one per
    // partition — the coordinator itself decides what a whole-topic
    // snapshot costs) when a `ReplicationCoordinator` is installed; the
    // honest RF=1 fallback otherwise mirrors `replica_list_v1`'s own
    // (this node is the sole replica, `leader_epoch=0`).
    let partitions_wire = run_blocking(
        move || -> Result<Vec<BusPartitionInfoWire>, ProtocolError> {
            let snapshot = svc
                .replication()
                .map(|c| c.snapshot(&org_id3, Some(&name3)));
            (0..partitions_n)
                .map(|partition| {
                    let stats = svc
                        .partition_stats(&bctx2, &name3, partition)
                        .map_err(map_bus_error)?;
                    let (leader_node_id, leader_epoch, isr_count, replica_count, high_watermark) =
                        match snapshot
                            .as_ref()
                            .and_then(|s| s.partitions.iter().find(|p| p.partition == partition))
                        {
                            Some(p) => (
                                p.leader_node_id.clone(),
                                p.leader_epoch,
                                p.isr.len() as u32,
                                p.replicas.len() as u32,
                                p.high_watermark,
                            ),
                            None => (Some(local_node_id.clone()), 0, 1, 1, stats.high_watermark),
                        };
                    Ok(BusPartitionInfoWire {
                        partition,
                        log_end_offset: stats.high_watermark,
                        earliest_offset: stats.earliest_offset,
                        size_bytes: stats.size_bytes,
                        segments: stats.segments,
                        leader_node_id,
                        leader_epoch,
                        isr_count,
                        replica_count,
                        high_watermark,
                    })
                })
                .collect()
        },
    )
    .await?;

    let bctx3 = bctx.clone();
    let name4 = name.clone();
    let svc = g.svc.clone();
    let groups_wire = run_blocking(
        move || -> Result<Vec<BusGroupLagSummaryWire>, ProtocolError> {
            let mut out = Vec::with_capacity(group_rows.len());
            for row in group_rows {
                let handle = match svc.open_consumer(
                    &bctx3,
                    &row.group_id,
                    std::slice::from_ref(&name4),
                    bus::ConsumerConfig {
                        commit_mode: groups::CommitMode::Explicit,
                    },
                ) {
                    Ok(h) => h,
                    // A group this admin cannot re-authorize for (revoked ACL
                    // since it last consumed) is skipped rather than failing the
                    // whole detail view.
                    Err(_) => continue,
                };
                let lag_total: u64 = handle
                    .lag()
                    .map_err(map_bus_error)?
                    .into_iter()
                    .map(|(_, l)| l)
                    .sum();
                out.push(BusGroupLagSummaryWire {
                    group: row.group_id,
                    lag_total,
                });
            }
            Ok(out)
        },
    )
    .await?;

    Ok(BusPayload::TopicDetailResponse {
        topic: topic_config_to_wire(&cfg),
        partitions: partitions_wire,
        groups: groups_wire,
    })
}

// =============================================================================
// Consumer groups
// =============================================================================

async fn group_list_v1(
    ctx: &HandlerContext,
    instance_id: &str,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_read(ctx, instance_id)?;
    let org_id = g.org_id.clone();
    let local_db = g.svc.local_db().clone();
    let rows = run_blocking(move || {
        repository::bus_group_list(&local_db, &org_id).map_err(|e| db_err("bus_group_list", e))
    })
    .await?;
    let groups = rows
        .into_iter()
        .filter(|g| !is_hidden_group(&g.group_id))
        .map(|g| BusGroupSummaryWire {
            group: g.group_id,
            topic: g.topic,
            commit_mode: g.commit_mode,
            paused: g.paused,
            created_at_ms: g.created_at_ms,
            updated_at_ms: g.updated_at_ms,
        })
        .collect();
    Ok(BusPayload::GroupListResponse { groups })
}

async fn group_detail_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    group: String,
    topic: String,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_read(ctx, instance_id)?;
    let bctx = bus_ctx(ctx, &g);
    let org_id = g.org_id.clone();
    let local_db = g.svc.local_db().clone();
    let (group_clone, topic_clone) = (group.clone(), topic.clone());
    let row = run_blocking(move || {
        repository::bus_group_get(&local_db, &org_id, &group_clone, &topic_clone)
            .map_err(|e| db_err("bus_group_get", e))?
            .ok_or_else(|| {
                ProtocolError::not_found(format!(
                    "bus.group_not_found: '{group_clone}' on '{topic_clone}'"
                ))
            })
    })
    .await?;

    // Follow-up toru P/M1-R2 decision 3: high watermark comes from
    // `BusService::partition_stats` (a no-consumer-session read) rather than
    // the old `PROBE_GROUP`-backed second `open_consumer`/`lag()` call —
    // see this file's module doc. Only the group's OWN lag still needs a
    // real `open_consumer`, since lag is inherently a property of the
    // (group, topic) pair.
    let group2 = group.clone();
    let topic2 = topic.clone();
    let bctx2 = bctx.clone();
    let svc = g.svc.clone();
    let partitions = run_blocking(
        move || -> Result<Vec<BusGroupPartitionDetailWire>, ProtocolError> {
            let handle = svc
                .open_consumer(
                    &bctx,
                    &group2,
                    std::slice::from_ref(&topic2),
                    bus::ConsumerConfig {
                        commit_mode: groups::CommitMode::Explicit,
                    },
                )
                .map_err(map_bus_error)?;
            handle
                .lag()
                .map_err(map_bus_error)?
                .into_iter()
                .map(|(tp, lag)| {
                    let hw = svc
                        .partition_stats(&bctx2, &topic2, tp.partition)
                        .map_err(map_bus_error)?
                        .high_watermark;
                    Ok(BusGroupPartitionDetailWire {
                        partition: tp.partition,
                        committed_offset: hw.saturating_sub(lag),
                        lag,
                    })
                })
                .collect()
        },
    )
    .await?;

    Ok(BusPayload::GroupDetailResponse {
        detail: BusGroupDetailWire {
            group: row.group_id,
            topic: row.topic,
            commit_mode: row.commit_mode,
            paused: row.paused,
            partitions,
        },
    })
}

async fn group_pause_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    group: String,
    topic: String,
) -> Result<BusPayload, ProtocolError> {
    // §4.3: operational, reversible, does not destroy data — write tier.
    let g = gate_write(ctx, instance_id)?;
    let bctx = bus_ctx(ctx, &g);
    let svc = g.svc.clone();
    run_blocking(move || {
        svc.pause_group(&bctx, &group, &topic)
            .map_err(map_bus_error)
    })
    .await?;
    Ok(BusPayload::GroupPauseResponse)
}

async fn group_resume_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    group: String,
    topic: String,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_write(ctx, instance_id)?;
    let bctx = bus_ctx(ctx, &g);
    let svc = g.svc.clone();
    run_blocking(move || {
        svc.resume_group(&bctx, &group, &topic)
            .map_err(map_bus_error)
    })
    .await?;
    Ok(BusPayload::GroupResumeResponse)
}

async fn offset_reset_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    group: String,
    topic: String,
    partition: u32,
    mode: BusOffsetResetMode,
) -> Result<BusPayload, ProtocolError> {
    // §4.3: rewrites committed consumer state — admin tier.
    let g = gate_admin(ctx, instance_id)?;
    let bctx = bus_ctx(ctx, &g);
    let svc = g.svc.clone();
    let new_offset = match mode {
        BusOffsetResetMode::Explicit { offset } => {
            run_blocking(move || {
                svc.reset_offset(&bctx, &group, &topic, partition, offset)
                    .map_err(map_bus_error)?;
                Ok(offset)
            })
            .await?
        }
        // Follow-up toru P/M1-R2 decision 1 (review N-1): goes through the
        // exact same admin path (`svc.reset_offset`, `force_commit`,
        // `bus.offset.reset` audit) as `Explicit`/`Latest`/`Timestamp`
        // below, unlike the OLD implementation's `open_consumer` +
        // `seek_to_earliest` — which internally calls `GroupOffsetStore::
        // commit` (the CONSUMER path, whose monotonicity guard rejects any
        // move backward with `OffsetRegression`). That made "Najwcześniejszy"
        // the one reset mode that could never actually move a commit
        // backward for a group that had committed anything at all — exactly
        // backward, the single most common reason an operator resets a
        // group in the first place ("replay the whole backlog").
        BusOffsetResetMode::Earliest => {
            run_blocking(move || -> Result<u64, ProtocolError> {
                let earliest = svc
                    .partition_stats(&bctx, &topic, partition)
                    .map_err(map_bus_error)?
                    .earliest_offset;
                svc.reset_offset(&bctx, &group, &topic, partition, earliest)
                    .map_err(map_bus_error)?;
                Ok(earliest)
            })
            .await?
        }
        // Follow-up toru P/M1-R2 decision 3: `partition_stats` (a
        // no-consumer-session read) replaces the old `PROBE_GROUP`-backed
        // `open_consumer`/`lag()` probe for "what is the high watermark" —
        // see this file's module doc.
        BusOffsetResetMode::Latest => {
            run_blocking(move || -> Result<u64, ProtocolError> {
                let latest = svc
                    .partition_stats(&bctx, &topic, partition)
                    .map_err(map_bus_error)?
                    .high_watermark;
                svc.reset_offset(&bctx, &group, &topic, partition, latest)
                    .map_err(map_bus_error)?;
                Ok(latest)
            })
            .await?
        }
        // Follow-up toru P task 4: PLAN M04's 4th reset mode, resolved via
        // `BusService::resolve_offset_for_timestamp` (wraps the engine's
        // `PartitionReader::fetch_from_timestamp`) rather than requiring a
        // real consumer session — this is a read-only lookup, not a commit,
        // so it needs no `open_consumer`/`bus_groups` row of its own.
        BusOffsetResetMode::Timestamp { ts_ms } => {
            run_blocking(move || -> Result<u64, ProtocolError> {
                let resolved = svc
                    .resolve_offset_for_timestamp(&bctx, &topic, partition, ts_ms)
                    .map_err(map_bus_error)?;
                svc.reset_offset(&bctx, &group, &topic, partition, resolved)
                    .map_err(map_bus_error)?;
                Ok(resolved)
            })
            .await?
        }
    };
    Ok(BusPayload::OffsetResetResponse { new_offset })
}

// =============================================================================
// Message preview (audited `bus.messages.browse`) and DLQ
// =============================================================================

async fn messages_browse_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    topic: String,
    from_offset: Option<u64>,
    from_offsets: Vec<BusPartitionOffsetWire>,
    limit: u32,
    partition: Option<u32>,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_read(ctx, instance_id)?;
    let bctx = bus_ctx(ctx, &g);
    let org_id = g.org_id.clone();
    let db = ctx.state.db.clone();
    let instance = g.instance.as_str().to_string();
    let topic_for_cfg = topic.clone();
    let partitions = run_blocking(move || {
        topics::get_topic(&db, &instance, &org_id, &topic_for_cfg)
            .map_err(map_bus_error)?
            .map(|c| c.partitions)
            .ok_or_else(|| {
                ProtocolError::not_found(format!("bus.topic_not_found: '{topic_for_cfg}'"))
            })
    })
    .await?;
    validate_partition_filter(partition, partitions, &topic)?;

    let starts = resolve_partition_starts(from_offset, &from_offsets, partitions);
    let topic_for_fetch = topic.clone();
    // `BusService::peek` audits `bus.messages.browse` itself (once per
    // partition it actually reads AND returns >= 1 record — P3-5 follow-up,
    // `KRYTYK-M1-R3.md`) — this handler no longer writes its own row, which
    // used to duplicate that audit under the OLD throwaway-consumer
    // implementation (follow-up toru P task 1).
    let (records, has_more, next_offset, partitions_wire) = run_blocking(move || {
        peek_topic(
            &bctx,
            &topic_for_fetch,
            partitions,
            &starts,
            limit,
            partition,
        )
    })
    .await?;

    let records = records.iter().map(record_to_preview).collect();
    Ok(BusPayload::MessagesBrowseResponse {
        result: BusMessagesBrowseResultWire {
            records,
            has_more,
            next_offset,
            partitions: partitions_wire,
        },
    })
}

async fn dlq_list_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    source_topic: String,
    from_offset: Option<u64>,
    from_offsets: Vec<BusPartitionOffsetWire>,
    limit: u32,
    partition: Option<u32>,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_read(ctx, instance_id)?;
    let bctx = bus_ctx(ctx, &g);
    let org_id = g.org_id.clone();
    let db = ctx.state.db.clone();
    let instance = g.instance.as_str().to_string();
    let dlq_topic = dlq::dlq_topic_name(&source_topic);
    let dlq_topic_for_cfg = dlq_topic.clone();
    let partitions = run_blocking(move || {
        topics::get_topic(&db, &instance, &org_id, &dlq_topic_for_cfg)
            .map_err(map_bus_error)?
            .map(|c| c.partitions)
            .ok_or_else(|| {
                ProtocolError::not_found(format!(
                    "bus.topic_not_found: '{dlq_topic_for_cfg}' (DLQ never used yet)"
                ))
            })
    })
    .await?;
    validate_partition_filter(partition, partitions, &dlq_topic)?;

    let starts = resolve_partition_starts(from_offset, &from_offsets, partitions);
    let dlq_topic_for_fetch = dlq_topic.clone();
    let dlq_topic_for_filter = dlq_topic.clone();
    let (records, has_more, next_offset, partitions_wire) = run_blocking(move || {
        let (records, has_more, next_offset, partitions_wire) = peek_topic(
            &bctx,
            &dlq_topic_for_fetch,
            partitions,
            &starts,
            limit,
            partition,
        )?;
        // M1-R2 review N-5, coordinator decision 2: `peek` itself stays
        // discard-unaware (it also backs `MessagesBrowse`, which never
        // reads a DLQ topic) — filtering a discarded record out of what
        // `DlqList` shows happens here, at this dispatch-layer wrapper.
        let records = filter_out_discarded(&bctx, &dlq_topic_for_filter, records)?;
        Ok((records, has_more, next_offset, partitions_wire))
    })
    .await?;
    let records = records.iter().map(record_to_dlq_wire).collect();
    Ok(BusPayload::DlqListResponse {
        result: BusDlqListResultWire {
            records,
            has_more,
            next_offset,
            partitions: partitions_wire,
        },
    })
}

async fn dlq_retry_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    source_topic: String,
    partition: u32,
    offset: u64,
) -> Result<BusPayload, ProtocolError> {
    // §4.3: republishes an existing record — write tier.
    let g = gate_write(ctx, instance_id)?;
    let bctx = bus_ctx(ctx, &g);
    let svc = g.svc.clone();
    let dlq_topic = dlq::dlq_topic_name(&source_topic);
    let result = run_blocking(move || {
        svc.dlq_retry(&bctx, &dlq_topic, partition, offset)
            .map_err(map_bus_error)
    })
    .await?;
    Ok(BusPayload::DlqRetryResponse {
        accepted: result.accepted,
    })
}

async fn dlq_discard_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    source_topic: String,
    partition: u32,
    offset: u64,
) -> Result<BusPayload, ProtocolError> {
    // §4.3: destroys the only remaining copy of a failed record — admin tier.
    let g = gate_admin(ctx, instance_id)?;
    let bctx = bus_ctx(ctx, &g);
    let svc = g.svc.clone();
    let dlq_topic = dlq::dlq_topic_name(&source_topic);
    run_blocking(move || {
        svc.dlq_discard(&bctx, &dlq_topic, partition, offset)
            .map_err(map_bus_error)
    })
    .await?;
    Ok(BusPayload::DlqDiscardResponse)
}

async fn dlq_retry_all_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    source_topic: String,
    max_records: u32,
) -> Result<BusPayload, ProtocolError> {
    // §4.3: bounded (DLQ_RETRY_ALL_MAX), same tier as the single retry.
    let g = gate_write(ctx, instance_id)?;
    let bctx = bus_ctx(ctx, &g);
    let org_id = g.org_id.clone();
    let db = ctx.state.db.clone();
    let instance = g.instance.as_str().to_string();
    let dlq_topic = dlq::dlq_topic_name(&source_topic);
    let max_records = max_records.clamp(1, DLQ_RETRY_ALL_MAX);

    let dlq_topic_for_cfg = dlq_topic.clone();
    let partitions = run_blocking(move || {
        topics::get_topic(&db, &instance, &org_id, &dlq_topic_for_cfg)
            .map_err(map_bus_error)?
            .map(|c| c.partitions)
            .ok_or_else(|| {
                ProtocolError::not_found(format!(
                    "bus.topic_not_found: '{dlq_topic_for_cfg}' (DLQ never used yet)"
                ))
            })
    })
    .await?;

    let bctx2 = bctx.clone();
    let dlq_topic2 = dlq_topic.clone();
    let dlq_topic2b = dlq_topic.clone();
    let empty_starts = std::collections::HashMap::new();
    // M1-R2 review N-5, coordinator decision 2: a discarded record must
    // never come back via "Ponów wszystkie" (the exact failure mode the
    // review reproduced — a record "discarded" minutes earlier reappeared
    // in the source topic through this exact call). Filtered out of the
    // page BEFORE any retry runs, same helper `DlqList` uses. This can
    // retry fewer than `max_records` non-discarded records in one call if
    // the page itself contained discarded ones — the same "one page, not a
    // guaranteed exact-N batch" shape `peek`-based paging already has.
    let records = run_blocking(move || {
        let (records, _, _, _) = peek_topic(
            &bctx2,
            &dlq_topic2,
            partitions,
            &empty_starts,
            max_records,
            None,
        )?;
        filter_out_discarded(&bctx2, &dlq_topic2b, records)
    })
    .await?;

    let (mut retried, mut failed) = (0u32, 0u32);
    for rv in records {
        let bctx3 = bctx.clone();
        let dlq_topic3 = dlq_topic.clone();
        let svc = g.svc.clone();
        let outcome = run_blocking(move || {
            svc.dlq_retry(&bctx3, &dlq_topic3, rv.partition, rv.offset)
                .map_err(map_bus_error)
        })
        .await;
        match outcome {
            Ok(_) => retried += 1,
            Err(_) => failed += 1,
        }
    }
    Ok(BusPayload::DlqRetryAllResponse { retried, failed })
}

// =============================================================================
// ACL (thin wrapper over `resource_permissions`, resource_type = "topic" —
// see `services::bus_authorizer`'s doc for what this does NOT model)
// =============================================================================

async fn acl_list_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    topic: String,
) -> Result<BusPayload, ProtocolError> {
    // §4.3: reading who may touch a topic is not itself privileged.
    let g = gate_read(ctx, instance_id)?;
    let db = ctx.state.db.clone();
    let instance = g.instance.as_str().to_string();
    let org_id = g.org_id.clone();
    let rows = run_blocking(move || {
        let resource_id =
            crate::services::bus_authorizer::topic_acl_resource_id(&instance, &org_id, &topic);
        repository::resource_permissions::list_for_resource(&db, "topic", &resource_id)
            .map_err(|e| db_err("resource_permissions::list_for_resource", e))
    })
    .await?;
    let entries = rows
        .into_iter()
        .map(|r| BusAclEntryWire {
            subject_type: r.subject_type,
            subject_id: r.subject_id,
            access_level: r.access_level,
        })
        .collect();
    Ok(BusPayload::AclListResponse { entries })
}

async fn acl_set_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    topic: String,
    subject_type: String,
    subject_id: String,
    access_level: String,
) -> Result<BusPayload, ProtocolError> {
    // §4.3: authorization surface — admin tier.
    let g = gate_admin(ctx, instance_id)?;
    // plan-app-platform §7 W4 finding 8: validation only guards the SET
    // path. A `clear` request must always be able to remove an ACL row,
    // even one keyed by a topic name that a LATER rule change made invalid
    // or reserved (e.g. a name that would fail `validate_user_topic_name`
    // today but did not when the row was created) — otherwise such a row
    // can never be removed through this API at all.
    if access_level != "clear" {
        crate::bus::topics::validate_user_topic_name(&topic).map_err(map_bus_error)?;
    }
    let db = ctx.state.db.clone();
    let instance = g.instance.as_str().to_string();
    let (org_id, topic2, subject_type2, subject_id2, access_level2) = (
        g.org_id.clone(),
        topic.clone(),
        subject_type.clone(),
        subject_id.clone(),
        access_level.clone(),
    );
    run_blocking(move || {
        let resource_id =
            crate::services::bus_authorizer::topic_acl_resource_id(&instance, &org_id, &topic2);
        if access_level2 == "clear" {
            repository::resource_permissions::clear(
                &db,
                "topic",
                &resource_id,
                &subject_type2,
                &subject_id2,
            )
            .map_err(|e| db_err("resource_permissions::clear", e))
        } else {
            if !matches!(access_level2.as_str(), "allow" | "deny") {
                return Err(ProtocolError::bad_request(
                    "bus.invalid_argument: access_level must be 'allow', 'deny' or 'clear'",
                ));
            }
            repository::resource_permissions::set(
                &db,
                "topic",
                &resource_id,
                &subject_type2,
                &subject_id2,
                &access_level2,
            )
            .map_err(|e| db_err("resource_permissions::set", e))
        }
    })
    .await?;
    // Any consumer holding a `ConsumerHandle` re-checks its permission
    // generation on the very next `fetch`/`commit` (PLAN §8.1); this is
    // what makes that generation counter actually move for an ACL edit —
    // see `services::bus_authorizer::bump_acl_generation`'s doc.
    crate::services::bus_authorizer::bump_acl_generation();
    let _ = repository::log_audit(
        &ctx.state.db,
        Some(&g.user_id),
        None,
        "bus.acl.set",
        Some(&topic),
        Some(&format!(
            "subject_type={subject_type} subject_id={subject_id} access_level={access_level}"
        )),
        None,
        Some(&ctx.state.local_node_id),
    );
    Ok(BusPayload::AclSetResponse)
}

// =============================================================================
// Field policies (SUM/tentabus/POLITYKI-POL.md, F0 follow-up
// SUM/tentabus/POLITYKI-POL-FORMATY.md — per-field access control, distinct
// from the coarse per-topic ACL above). §4.3: List is `gate_read` (reading
// policy shape is not itself privileged); Set/Delete are `gate_admin`
// (authorization surface — a field policy gates exactly what PII a subject
// can see or write).
// =============================================================================

async fn field_policy_list_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    topic: String,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_read(ctx, instance_id)?;
    let org_id = g.org_id.clone();
    let db = ctx.state.db.clone();
    let instance = g.instance.as_str().to_string();
    let policies = run_blocking(move || {
        field_policies::list_policies(&db, &instance, &org_id, &topic)
            .map_err(map_bus_error)?
            .into_iter()
            .map(|row| {
                let subject_type = row.subject_type.clone();
                let subject_id = row.subject_id.clone();
                let direction = row.direction.clone();
                let created_at_ms = row.created_at_ms;
                let updated_at_ms = row.updated_at_ms;
                let row_topic = row.topic.clone();
                field_policies::decode(row, &row_topic)
                    .map(|p| BusFieldPolicyWire {
                        subject_type,
                        subject_id,
                        direction,
                        fields: p.fields.into_iter().collect(),
                        required_fields: p.required_fields.into_iter().collect(),
                        created_at_ms,
                        updated_at_ms,
                    })
                    .map_err(map_bus_error)
            })
            .collect::<Result<Vec<_>, _>>()
    })
    .await?;
    Ok(BusPayload::FieldPolicyListResponse { policies })
}

#[allow(clippy::too_many_arguments)]
async fn field_policy_set_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    topic: String,
    subject_type: String,
    subject_id: String,
    direction: String,
    fields: Vec<String>,
    required_fields: Vec<String>,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_admin(ctx, instance_id)?;
    let dir = field_policies::Direction::parse(&direction).ok_or_else(|| {
        ProtocolError::bad_request("bus.invalid_argument: direction must be 'write' or 'read'")
    })?;
    let org_id = g.org_id.clone();
    let actor = g.user_id.clone();
    let db = ctx.state.db.clone();
    let instance = g.instance.as_str().to_string();
    let (topic2, subject_type2, subject_id2) =
        (topic.clone(), subject_type.clone(), subject_id.clone());
    let fields_set: std::collections::BTreeSet<String> = fields.into_iter().collect();
    let required_set: std::collections::BTreeSet<String> = required_fields.into_iter().collect();
    run_blocking(move || {
        field_policies::set_policy(
            &db,
            &instance,
            &org_id,
            &topic2,
            &subject_type2,
            &subject_id2,
            dir,
            &fields_set,
            &required_set,
        )
        .map_err(map_bus_error)
    })
    .await?;
    let _ = repository::log_audit(
        &ctx.state.db,
        Some(&actor),
        None,
        "bus.field_policy.set",
        Some(&topic),
        Some(&format!(
            "subject_type={subject_type} subject_id={subject_id} direction={direction}"
        )),
        None,
        Some(&ctx.state.local_node_id),
    );
    Ok(BusPayload::FieldPolicySetResponse)
}

async fn field_policy_delete_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    topic: String,
    subject_type: String,
    subject_id: String,
    direction: String,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_admin(ctx, instance_id)?;
    let dir = field_policies::Direction::parse(&direction).ok_or_else(|| {
        ProtocolError::bad_request("bus.invalid_argument: direction must be 'write' or 'read'")
    })?;
    let org_id = g.org_id.clone();
    let actor = g.user_id.clone();
    let db = ctx.state.db.clone();
    let instance = g.instance.as_str().to_string();
    let (topic2, subject_type2, subject_id2) =
        (topic.clone(), subject_type.clone(), subject_id.clone());
    run_blocking(move || {
        field_policies::delete_policy(
            &db,
            &instance,
            &org_id,
            &topic2,
            &subject_type2,
            &subject_id2,
            dir,
        )
        .map_err(map_bus_error)
    })
    .await?;
    let _ = repository::log_audit(
        &ctx.state.db,
        Some(&actor),
        None,
        "bus.field_policy.delete",
        Some(&topic),
        Some(&format!(
            "subject_type={subject_type} subject_id={subject_id} direction={direction}"
        )),
        None,
        Some(&ctx.state.local_node_id),
    );
    Ok(BusPayload::FieldPolicyDeleteResponse)
}

// =============================================================================
// Schema registry (SUM/tentabus/PLAN-F3.md §6). §4.3: List/Get/DerivedGet
// are `gate_read` (reading a schema is not itself privileged — a bound
// schema affects writers, not readers); Register/CompatibilitySet/Delete
// are `gate_admin` (a bound schema can start DLQ-diverting live traffic).
// =============================================================================

fn schema_subject_to_wire(info: schema_registry::registry::SubjectInfo) -> BusSchemaSubjectWire {
    BusSchemaSubjectWire {
        subject: info.subject,
        schema_type: info.schema_type.as_str().to_string(),
        compatibility: info.compatibility.as_str().to_string(),
        deprecated_at_ms: info.deprecated_at_ms,
        latest_version: info.latest_version,
        created_by: info.created_by,
        created_at_ms: info.created_at_ms,
        updated_at_ms: info.updated_at_ms,
    }
}

fn schema_version_to_wire(info: schema_registry::registry::VersionInfo) -> BusSchemaVersionWire {
    BusSchemaVersionWire {
        subject: info.subject,
        version: info.version,
        schema_ref_id: info.schema_ref_id,
        content_hash: info.content_hash,
        created_by: info.created_by,
        created_at_ms: info.created_at_ms,
    }
}

async fn schema_subject_list_v1(
    ctx: &HandlerContext,
    instance_id: &str,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_read(ctx, instance_id)?;
    let org_id = g.org_id.clone();
    let db = ctx.state.db.clone();
    let instance = g.instance.as_str().to_string();
    let subjects = run_blocking(move || {
        schema_registry::registry::list_subjects(&db, &instance, &org_id).map_err(map_bus_error)
    })
    .await?;
    let subjects = subjects.into_iter().map(schema_subject_to_wire).collect();
    Ok(BusPayload::SchemaSubjectListResponse { subjects })
}

async fn schema_version_list_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    subject: String,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_read(ctx, instance_id)?;
    let org_id = g.org_id.clone();
    let db = ctx.state.db.clone();
    let instance = g.instance.as_str().to_string();
    let versions = run_blocking(move || {
        schema_registry::registry::list_versions(&db, &instance, &org_id, &subject)
            .map_err(map_bus_error)
    })
    .await?;
    let versions = versions.into_iter().map(schema_version_to_wire).collect();
    Ok(BusPayload::SchemaVersionListResponse { versions })
}

async fn schema_get_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    subject: String,
    version: Option<u32>,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_read(ctx, instance_id)?;
    let org_id = g.org_id.clone();
    let db = ctx.state.db.clone();
    let instance = g.instance.as_str().to_string();
    let (info, schema_text) = run_blocking(move || {
        schema_registry::registry::get(&db, &instance, &org_id, &subject, version)
            .map_err(map_bus_error)
    })
    .await?;
    Ok(BusPayload::SchemaGetResponse {
        schema: schema_version_to_wire(info),
        schema_text,
    })
}

async fn schema_derived_get_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    subject: String,
    version: Option<u32>,
    topic: String,
    subject_type: String,
    subject_id: String,
    direction: String,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_read(ctx, instance_id)?;
    let dir = field_policies::Direction::parse(&direction).ok_or_else(|| {
        ProtocolError::bad_request("bus.invalid_argument: direction must be 'write' or 'read'")
    })?;
    let bctx = bus_ctx(ctx, &g);
    let svc = g.svc.clone();
    let schema_text = run_blocking(move || {
        svc.schema_derived_get(
            &bctx,
            &subject,
            version,
            &topic,
            &subject_type,
            &subject_id,
            dir,
        )
        .map_err(map_bus_error)
    })
    .await?;
    Ok(BusPayload::SchemaDerivedGetResponse { schema_text })
}

async fn schema_register_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    subject: String,
    schema_type: String,
    schema_text: String,
    compatibility: Option<String>,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_admin(ctx, instance_id)?;
    let kind = schema_registry::SchemaType::parse(&schema_type).ok_or_else(|| {
        ProtocolError::bad_request(
            "bus.invalid_argument: schema_type must be one of json_schema|avro|protobuf|thrift",
        )
    })?;
    let compat = compatibility
        .as_deref()
        .map(|s| {
            schema_registry::Compatibility::parse(s).ok_or_else(|| {
                ProtocolError::bad_request(
                    "bus.invalid_argument: compatibility must be one of none|backward|forward|full",
                )
            })
        })
        .transpose()?;
    let org_id = g.org_id.clone();
    let actor = g.user_id.clone();
    let actor_for_call = actor.clone();
    let db = ctx.state.db.clone();
    let instance = g.instance.as_str().to_string();
    let subject2 = subject.clone();
    let schema_type_for_audit = schema_type.clone();
    let outcome = run_blocking(move || {
        schema_registry::registry::register(
            &db,
            &instance,
            &org_id,
            &subject2,
            kind,
            &schema_text,
            compat,
            Some(&actor_for_call),
        )
        .map_err(map_bus_error)
    })
    .await?;
    let _ = repository::log_audit(
        &ctx.state.db,
        Some(&actor),
        None,
        "bus.schema.register",
        Some(&subject),
        Some(&format!(
            "schema_type={schema_type_for_audit} version={} deduplicated={}",
            outcome.version, outcome.deduplicated
        )),
        None,
        Some(&ctx.state.local_node_id),
    );
    Ok(BusPayload::SchemaRegisterResponse {
        version: outcome.version,
        schema_ref_id: outcome.schema_ref_id,
        deduplicated: outcome.deduplicated,
    })
}

async fn schema_compatibility_set_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    subject: String,
    compatibility: String,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_admin(ctx, instance_id)?;
    let compat = schema_registry::Compatibility::parse(&compatibility).ok_or_else(|| {
        ProtocolError::bad_request(
            "bus.invalid_argument: compatibility must be one of none|backward|forward|full",
        )
    })?;
    let org_id = g.org_id.clone();
    let actor = g.user_id.clone();
    let db = ctx.state.db.clone();
    let instance = g.instance.as_str().to_string();
    let subject2 = subject.clone();
    run_blocking(move || {
        schema_registry::registry::set_compatibility(&db, &instance, &org_id, &subject2, compat)
            .map_err(map_bus_error)
    })
    .await?;
    let _ = repository::log_audit(
        &ctx.state.db,
        Some(&actor),
        None,
        "bus.schema.compatibility.set",
        Some(&subject),
        Some(&format!("compatibility={compatibility}")),
        None,
        Some(&ctx.state.local_node_id),
    );
    Ok(BusPayload::SchemaCompatibilitySetResponse)
}

async fn schema_delete_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    subject: String,
    version: Option<u32>,
    deprecate_only: bool,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_admin(ctx, instance_id)?;
    let org_id = g.org_id.clone();
    let actor = g.user_id.clone();
    let db = ctx.state.db.clone();
    let instance = g.instance.as_str().to_string();
    let subject2 = subject.clone();
    let removed_versions = run_blocking(move || {
        schema_registry::registry::delete(
            &db,
            &instance,
            &org_id,
            &subject2,
            version,
            deprecate_only,
        )
        .map_err(map_bus_error)
    })
    .await?;
    // A deprecation is a soft, reversible-in-spirit action (PLAN-F3 owner
    // decision 3); a real delete of one or more immutable versions is not
    // — distinct audit actions so an operator reviewing the log does not
    // have to inspect `details` to tell them apart.
    let action = if deprecate_only {
        "bus.schema.deprecate"
    } else {
        "bus.schema.delete"
    };
    let _ = repository::log_audit(
        &ctx.state.db,
        Some(&actor),
        None,
        action,
        Some(&subject),
        Some(&format!("versions={removed_versions:?}")),
        None,
        Some(&ctx.state.local_node_id),
    );
    Ok(BusPayload::SchemaDeleteResponse { removed_versions })
}

// =============================================================================
// Stats snapshot (PLAN §6.2 StatsSubscribe/StatsEvent, delivered as polling
// instead — see this file's module doc)
// =============================================================================

/// Sum of `high_watermark - earliest_offset` across every partition of
/// `source_topic`'s derived `__dlq.<source_topic>` topic, minus every
/// offset in that range marked discarded (`BusService::dlq_discard`, M1-R2
/// review N-5, coordinator decision 2) — `0` when the DLQ topic does not
/// exist yet (the source topic has never needed its DLQ), a normal
/// outcome, not an error (follow-up toru P task 3).
fn dlq_depth_for(
    svc: &bus::BusService,
    bctx: &BusCallContext,
    db: &crate::db::DbPool,
    org_id: &str,
    source_topic: &str,
) -> u64 {
    let dlq_topic = dlq::dlq_topic_name(source_topic);
    let Ok(Some(dlq_cfg)) = topics::get_topic(db, svc.instance_id(), org_id, &dlq_topic) else {
        return 0;
    };
    (0..dlq_cfg.partitions)
        .filter_map(|p| {
            let stats = svc.partition_stats(bctx, &dlq_topic, p).ok()?;
            let raw = stats.high_watermark.saturating_sub(stats.earliest_offset);
            let discarded = svc
                .dlq_discarded_offsets(bctx, &dlq_topic, p)
                .map(|offsets| offsets.len() as u64)
                .unwrap_or(0);
            Some(raw.saturating_sub(discarded))
        })
        .sum()
}

async fn stats_snapshot_v1(
    ctx: &HandlerContext,
    instance_id: &str,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_read(ctx, instance_id)?;
    let bctx = bus_ctx(ctx, &g);
    let org_id = g.org_id.clone();
    let db = ctx.state.db.clone();
    let svc = g.svc.clone();
    let instance_id = svc.instance_id().to_string();
    let local_db = svc.local_db().clone();
    let (topics, groups) = run_blocking(move || {
        let topics = topics::list_topics(&db, &instance_id, &org_id).map_err(map_bus_error)?;
        // Hidden (`tf-`-prefixed) groups are dropped here, once, so every
        // KPI/lag figure below derived from `groups` — `group_count`,
        // `paused_group_count`, and the per-topic lag loop — agrees with
        // what `GroupList` itself shows (M1-R2 review N-2/N-7, coordinator
        // decisions 3/7).
        let groups = repository::bus_group_list(&local_db, &org_id)
            .map_err(|e| db_err("bus_group_list", e))?
            .into_iter()
            .filter(|g| !is_hidden_group(&g.group_id))
            .collect::<Vec<_>>();
        Ok::<_, ProtocolError>((topics, groups))
    })
    .await?;
    let topic_count = topics.len() as u32;
    let dlq_topic_count = topics
        .iter()
        .filter(|t| t.name.starts_with(dlq::DLQ_TOPIC_PREFIX))
        .count() as u32;
    let partition_count_total: u32 = topics.iter().map(|t| t.partitions).sum();
    let group_count = groups.len() as u32;
    let paused_group_count = groups.iter().filter(|g| g.paused).count() as u32;

    // Per-topic KPI breakdown (follow-up toru P task 3). Lag is summed once
    // per GROUP (not per topic × group, `groups` is read once above) to
    // stay O(groups), not O(topics × groups); rates/disk-bytes/dlq_depth
    // are cheap per-topic reads (`topic_rates` is an in-memory lookup,
    // `partition_stats` opens an already-cached `Partition` handle).
    let db2 = ctx.state.db.clone();
    let org_id2 = g.org_id.clone();
    let bctx2 = bctx.clone();
    let svc2 = g.svc.clone();
    let (topics_wire, total_lag, total_dlq_depth, total_bytes_on_disk) = run_blocking(
        move || -> Result<(Vec<BusTopicStatsWire>, u64, u64, u64), ProtocolError> {
            let svc = svc2;
            let mut lag_by_topic: std::collections::HashMap<String, u64> =
                std::collections::HashMap::new();
            for row in &groups {
                let handle = match svc.open_consumer(
                    &bctx2,
                    &row.group_id,
                    std::slice::from_ref(&row.topic),
                    bus::ConsumerConfig {
                        commit_mode: groups::CommitMode::Explicit,
                    },
                ) {
                    Ok(h) => h,
                    // A group this session cannot re-authorize for (revoked
                    // ACL) is skipped rather than failing the whole
                    // snapshot — same tolerance `topic_detail_v1` already
                    // has for its own per-group lag loop.
                    Err(_) => continue,
                };
                if let Ok(lag) = handle.lag() {
                    let sum: u64 = lag.into_iter().map(|(_, l)| l).sum();
                    *lag_by_topic.entry(row.topic.clone()).or_insert(0) += sum;
                }
            }

            let mut topics_wire = Vec::new();
            let mut total_lag = 0u64;
            let mut total_dlq_depth = 0u64;
            let mut total_bytes_on_disk = 0u64;
            // DLQ topics are listed as their own rows too: their bytes are
            // real disk usage and the UI reconciles the per-topic column with
            // the org-wide total. A DLQ topic has no DLQ of its own, so its
            // `dlq_depth` is 0 and it never feeds `total_dlq_depth`.
            for cfg in topics.iter() {
                let is_dlq = cfg.name.starts_with(dlq::DLQ_TOPIC_PREFIX);
                let (msgs_in_per_sec, bytes_in_per_sec) = svc.topic_rates(&org_id2, &cfg.name);
                let topic_bytes_on_disk: u64 = (0..cfg.partitions)
                    .filter_map(|p| svc.partition_stats(&bctx2, &cfg.name, p).ok())
                    .map(|s| s.size_bytes)
                    .sum();
                let topic_lag = lag_by_topic.get(&cfg.name).copied().unwrap_or(0);
                let dlq_depth = if is_dlq {
                    0
                } else {
                    dlq_depth_for(&svc, &bctx2, &db2, &org_id2, &cfg.name)
                };

                total_lag += topic_lag;
                total_dlq_depth += dlq_depth;
                total_bytes_on_disk += topic_bytes_on_disk;

                topics_wire.push(BusTopicStatsWire {
                    topic: cfg.name.clone(),
                    msgs_in_per_sec: msgs_in_per_sec.min(u32::MAX as u64) as u32,
                    bytes_in_per_sec,
                    total_bytes_on_disk: topic_bytes_on_disk,
                    total_lag: topic_lag,
                    dlq_depth,
                });
            }
            Ok((topics_wire, total_lag, total_dlq_depth, total_bytes_on_disk))
        },
    )
    .await?;

    let total_msgs_in_per_sec: u32 = topics_wire
        .iter()
        .map(|t| t.msgs_in_per_sec)
        .fold(0u32, |acc, v| acc.saturating_add(v));
    let total_bytes_in_per_sec: u64 = topics_wire.iter().map(|t| t.bytes_in_per_sec).sum();

    Ok(BusPayload::StatsSnapshotResponse {
        snapshot: BusStatsSnapshotWire {
            topic_count,
            dlq_topic_count,
            partition_count_total,
            group_count,
            paused_group_count,
            total_msgs_in_per_sec,
            total_bytes_in_per_sec,
            total_bytes_on_disk,
            total_lag,
            total_dlq_depth,
            topics: topics_wire,
        },
    })
}

// =============================================================================
// Capabilities (permission introspection for the UI, PLAN §8.1 — follow-up
// toru P task 5)
// =============================================================================

/// plan-app-platform §4.4: unlike the rest of this table, `read` here is a
/// HARD gate, not a graceful degrade to "nothing" — the request now names a
/// concrete `instance_id` (no more `resolve_instance_addon_id` guess), so a
/// caller with no read grant on THAT instance, or an instance that does not
/// exist/is disabled, gets the same `PolicyDenied`/`AppUnavailable` every
/// other `gate_read` caller gets. `can_read` itself is therefore always
/// `true` in a successful response: reaching this line already proved it.
async fn capabilities_v1(
    ctx: &HandlerContext,
    instance_id: &str,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_read(ctx, instance_id)?;
    let checker = ctx
        .state
        .permission_checker
        .as_ref()
        .ok_or_else(|| ProtocolError::internal("bus.no_permission_checker"))?;
    let can = |p: &str| {
        checker
            .check(g.instance.as_str(), &g.user_id, p, None)
            .is_granted()
    };
    let is_org_admin = ctx.org_context.as_ref().is_some_and(|o| o.has("org.admin"));
    let capabilities = BusCapabilitiesWire {
        can_read: true,
        can_write: can(PERM_WRITE),
        // The UI must reflect the DOUBLE LOCK, not just the matrix half, or
        // an operator with delegated bus.admin sees buttons that always 403.
        can_admin: can(PERM_ADMIN) && is_org_admin,
        is_site_admin: SessionAuthKind::Admin.session_satisfies(&ctx.session),
    };
    Ok(BusPayload::CapabilitiesResponse { capabilities })
}

// =============================================================================
// M2 replication (SUM/tentabus/PLAN-M2.md §1e/§1f) — node cards, the
// per-partition role matrix, and failover history for the M06 "Partycje i
// repliki" view, plus the two admin mutations (`Reassign`/`LeaderTransfer`).
// =============================================================================

fn replica_node_wire(n: &ReplicaNodeInfo) -> BusReplicaNodeWire {
    BusReplicaNodeWire {
        node_id: n.node_id.clone(),
        label: n.label.clone(),
        environment: n.environment.as_str().to_string(),
        is_local: n.is_local,
        reachable: n.reachable,
        last_heartbeat_ms_ago: n.last_heartbeat_ms_ago,
        leader_count: n.leader_count,
        follower_count: n.follower_count,
        isr_count: n.isr_count,
    }
}

fn replica_lag_wire(l: &ReplicaLagInfo) -> BusReplicaLagWire {
    BusReplicaLagWire {
        node_id: l.node_id.clone(),
        lag_bytes: l.lag_bytes,
        lag_ms: l.lag_ms,
        reason: l.reason.clone(),
    }
}

/// 'no_assignment' | 'epoch_fenced' | 'no_isr' — the wire tag for
/// `bus::UnavailableReason` (see `BusPartitionReplicaWire::unavailable_
/// reason`'s doc for why this crosses as a string, not the Rust enum).
fn unavailable_reason_str(r: UnavailableReason) -> &'static str {
    match r {
        UnavailableReason::NoAssignment => "no_assignment",
        UnavailableReason::EpochFenced => "epoch_fenced",
        UnavailableReason::NoIsr => "no_isr",
    }
}

fn partition_replica_wire(p: &PartitionReplicaInfo) -> BusPartitionReplicaWire {
    BusPartitionReplicaWire {
        partition: p.partition,
        leader_node_id: p.leader_node_id.clone(),
        leader_epoch: p.leader_epoch,
        replicas: p.replicas.clone(),
        isr: p.isr.clone(),
        lagging: p.lagging.iter().map(replica_lag_wire).collect(),
        high_watermark: p.high_watermark,
        log_end_offset: p.log_end_offset,
        unavailable_reason: p
            .unavailable_reason
            .map(|r| unavailable_reason_str(r).to_string()),
    }
}

/// Audit action a leader promotion/failover (agent S/EL, wave 2 — nothing
/// in THIS build writes it yet) MUST log under, for `replica_list_v1`'s
/// M06 timeline below to find it. Contract (matches `bus::mod`'s own
/// `audit_details(org_id, extra)` helper's output shape exactly, since
/// that code lives in `bus/mod.rs` and already has it in scope):
///
/// - `resource` = the topic name.
/// - `node_id` = the NEW leader (`to_node`), same "author of the row"
///     convention `dispatch/bus.rs`'s own `ctx.state.local_node_id` audit
///     calls use.
/// - `details` = `"org_id=<org> partition=<u32> from_node=<id|-> \
///     from_epoch=<u32> to_epoch=<u32> duration_ms=<u64> reason=<token>"`
///     (space-separated `key=value`, `from_node` literal `-` when there
///     was no prior leader for this partition).
const BUS_FAILOVER_AUDIT_ACTION: &str = "bus.leader.failover";

/// Upper bound on `audit_log` rows scanned per `ReplicaList` call — `audit_
/// log` has no `org_id`/tenant column (PLAN-M2 §1f's own note: sourced from
/// the existing table, no new one), so this handler filters by `org_id=`
/// inside `details` in Rust rather than in SQL. A large-but-bounded scan
/// window, not a full-table scan, and never more than one query per
/// request.
const BUS_FAILOVER_AUDIT_SCAN_LIMIT: i64 = 1000;

/// Failover events actually returned to the UI, after org/topic filtering
/// — the M06 timeline is a recent-history strip, not a full audit export
/// (that already exists as the separate, generic audit log viewer).
const BUS_FAILOVER_HISTORY_MAX: usize = 50;

fn parse_audit_kv(details: &str) -> std::collections::HashMap<&str, &str> {
    details
        .split_whitespace()
        .filter_map(|tok| tok.split_once('='))
        .collect()
}

/// Builds the M06 failover timeline directly from `audit_log` (PLAN-M2 §1f:
/// "no dedicated table") — deliberately independent of whatever a
/// `ReplicationCoordinator::snapshot`'s own `failovers` field returns
/// (wave 1, agent EL owns that implementor), so this list is correct even
/// before/without a coordinator wired up, and does not depend on EL having
/// mirrored the SAME audit rows into its own snapshot type.
fn failover_events_from_audit(
    db: &crate::db::DbPool,
    org_id: &str,
    topic: Option<&str>,
) -> Result<Vec<BusFailoverEventWire>, ProtocolError> {
    let rows = repository::list_audit_logs(
        db,
        &crate::db::models::AuditLogFilters {
            action: Some(BUS_FAILOVER_AUDIT_ACTION.to_string()),
            ..Default::default()
        },
        0,
        BUS_FAILOVER_AUDIT_SCAN_LIMIT,
    )
    .map_err(|e| db_err("list_audit_logs(bus.leader.failover)", e))?;

    let mut out = Vec::new();
    for row in rows {
        if out.len() >= BUS_FAILOVER_HISTORY_MAX {
            break;
        }
        let Some(row_topic) = row.resource.clone() else {
            continue;
        };
        if let Some(want_topic) = topic {
            if row_topic != want_topic {
                continue;
            }
        }
        let details = row.details.unwrap_or_default();
        let kv = parse_audit_kv(&details);
        if kv.get("org_id").copied() != Some(org_id) {
            continue;
        }
        let Some(to_node) = row.node_id.clone() else {
            continue;
        };
        let partition: u32 = kv
            .get("partition")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let from_node = match kv.get("from_node").copied() {
            Some("-") | None => None,
            Some(v) => Some(v.to_string()),
        };
        let from_epoch: u32 = kv
            .get("from_epoch")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let to_epoch: u32 = kv.get("to_epoch").and_then(|v| v.parse().ok()).unwrap_or(0);
        let duration_ms: u64 = kv
            .get("duration_ms")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let reason = kv.get("reason").copied().unwrap_or("").to_string();
        // `audit_log.timestamp` is `%Y-%m-%d %H:%M:%S` UTC, second precision
        // (`log_audit_conn`'s own format) — the only precision this table
        // has, not a limitation introduced here.
        let at_ms = chrono::NaiveDateTime::parse_from_str(&row.timestamp, "%Y-%m-%d %H:%M:%S")
            .map(|dt| dt.and_utc().timestamp_millis())
            .unwrap_or(0);
        out.push(BusFailoverEventWire {
            at_ms,
            topic: row_topic,
            partition,
            from_node,
            to_node,
            from_epoch,
            to_epoch,
            duration_ms,
            reason,
        });
    }
    Ok(out)
}

/// M06 "Partycje i repliki" view: node cards + per-partition role matrix +
/// failover history. `bus.read`, org-scoped.
///
/// With a `ReplicationCoordinator` installed (`BusService::replication()`
/// is `Some`, wave 2, agent EL's `ReplicationManager`), `nodes`/`partitions`
/// come straight from `coordinator.snapshot(org, topic)`. With NONE
/// installed — every build until wave 2 wires one up, and any RF=1
/// deployment forever after — this returns an HONEST single-node snapshot
/// instead of an empty one: this node is the sole replica of every
/// partition it owns (`leader_epoch=0`, `isr=[this node]`), which is
/// exactly M1's real, unreplicated behavior, so the M06 screen has
/// something truthful to show on a single node. `failovers` is ALWAYS
/// read straight from `audit_log` (see `failover_events_from_audit`'s
/// doc), independent of whether a coordinator is installed.
async fn replica_list_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    topic: Option<String>,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_read(ctx, instance_id)?;
    let bctx = bus_ctx(ctx, &g);
    let org_id = g.org_id.clone();
    let db = ctx.state.db.clone();
    let local_node_id = ctx.state.local_node_id.to_string();
    let local_label = ctx
        .state
        .mesh_peer_store
        .get_hostname(&local_node_id)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| local_node_id.clone());

    let topic_for_snapshot = topic.clone();
    let org_id_for_snapshot = org_id.clone();
    let db_for_snapshot = db.clone();
    let bctx_for_snapshot = bctx.clone();
    let local_node_id_for_snapshot = local_node_id.clone();
    let local_label_for_snapshot = local_label.clone();
    let svc = g.svc.clone();
    let (nodes, partitions) = run_blocking(move || -> Result<_, ProtocolError> {
        let svc = svc;
        if let Some(coordinator) = svc.replication() {
            let snapshot =
                coordinator.snapshot(&org_id_for_snapshot, topic_for_snapshot.as_deref());
            let nodes: Vec<BusReplicaNodeWire> =
                snapshot.nodes.iter().map(replica_node_wire).collect();
            let partitions: Vec<BusPartitionReplicaWire> = snapshot
                .partitions
                .iter()
                .map(partition_replica_wire)
                .collect();
            return Ok((nodes, partitions));
        }
        // Honest RF=1 fallback (module doc above): this node is the sole
        // replica for every partition of every topic in scope.
        let env = crate::services::environment::get_node_environment(&db_for_snapshot);
        let topics_in_scope: Vec<topics::TopicConfig> = match &topic_for_snapshot {
            Some(name) => topics::get_topic(
                &db_for_snapshot,
                svc.instance_id(),
                &org_id_for_snapshot,
                name,
            )
            .map_err(map_bus_error)?
            .into_iter()
            .collect(),
            None => topics::list_topics(&db_for_snapshot, svc.instance_id(), &org_id_for_snapshot)
                .map_err(map_bus_error)?,
        };
        let mut partition_count = 0u32;
        let mut partitions_wire = Vec::new();
        for cfg in &topics_in_scope {
            for partition in 0..cfg.partitions {
                partition_count += 1;
                let stats = svc
                    .partition_stats(&bctx_for_snapshot, &cfg.name, partition)
                    .map_err(map_bus_error)?;
                partitions_wire.push(BusPartitionReplicaWire {
                    partition,
                    leader_node_id: Some(local_node_id_for_snapshot.clone()),
                    leader_epoch: 0,
                    replicas: vec![local_node_id_for_snapshot.clone()],
                    isr: vec![local_node_id_for_snapshot.clone()],
                    lagging: Vec::new(),
                    high_watermark: stats.high_watermark,
                    log_end_offset: stats.high_watermark,
                    unavailable_reason: None,
                });
            }
        }
        let node = BusReplicaNodeWire {
            node_id: local_node_id_for_snapshot.clone(),
            label: local_label_for_snapshot,
            environment: env.as_str().to_string(),
            is_local: true,
            reachable: true,
            last_heartbeat_ms_ago: Some(0),
            leader_count: partition_count,
            follower_count: 0,
            isr_count: partition_count,
        };
        Ok((vec![node], partitions_wire))
    })
    .await?;

    let failovers =
        run_blocking(move || failover_events_from_audit(&db, &org_id, topic.as_deref())).await?;

    Ok(BusPayload::ReplicaListResponse {
        nodes,
        partitions,
        failovers,
    })
}

/// Admin-triggered replica-set change for one partition, or (`partition:
/// None`) every partition of the topic. `gate_admin` (§4.3: "moves data
/// between nodes") — same double-lock tier every other destructive bus
/// mutation runs at, and PLAN-M2 §1f's own `// Admin` annotation on this
/// wire request.
async fn replica_reassign_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    topic: String,
    partition: Option<u32>,
    replicas: Vec<String>,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_admin(ctx, instance_id)?;
    let org_id = g.org_id.clone();
    let user_id = g.user_id.clone();
    let db = ctx.state.db.clone();
    let local_node_id = ctx.state.local_node_id.to_string();

    if replicas.is_empty() {
        return Err(ProtocolError::bad_request(
            "bus.invalid_argument: replicas must not be empty",
        ));
    }

    let topic_for_check = topic.clone();
    let org_id_for_check = org_id.clone();
    let db_for_check = db.clone();
    let instance_for_check = g.instance.as_str().to_string();
    run_blocking(move || {
        topics::get_topic(
            &db_for_check,
            &instance_for_check,
            &org_id_for_check,
            &topic_for_check,
        )
        .map_err(map_bus_error)?
        .ok_or_else(|| {
            ProtocolError::not_found(format!("bus.topic_not_found: '{topic_for_check}'"))
        })
        .map(|_| ())
    })
    .await?;

    let topic_for_call = topic.clone();
    let org_id_for_call = org_id.clone();
    let replicas_for_call = replicas.clone();
    let svc = g.svc.clone();
    let applied = run_blocking(move || -> Result<u32, ProtocolError> {
        let svc = svc;
        let coordinator = svc.replication().ok_or_else(|| {
            ProtocolError::new(
                ProtocolErrorCode::NotAvailable,
                "bus.replication_disabled: no replication coordinator installed on this node",
            )
        })?;
        coordinator
            .reassign(
                &org_id_for_call,
                &topic_for_call,
                partition,
                &replicas_for_call,
            )
            .map_err(map_repl_error)
    })
    .await?;

    let _ = repository::log_audit(
        &db,
        Some(&user_id),
        None,
        "bus.replica.reassign",
        Some(&topic),
        Some(&format!(
            "partition={} replicas={} applied={applied}",
            partition
                .map(|p| p.to_string())
                .unwrap_or_else(|| "*".to_string()),
            replicas.join(","),
        )),
        None,
        Some(&local_node_id),
    );

    Ok(BusPayload::ReassignResponse { applied })
}

/// Admin-triggered leader transfer for one partition (M03/M06 "Przenieś
/// lidera"). Same tier as `replica_reassign_v1`.
async fn leader_transfer_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    topic: String,
    partition: u32,
    target_node_id: String,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_admin(ctx, instance_id)?;
    let org_id = g.org_id.clone();
    let user_id = g.user_id.clone();
    let db = ctx.state.db.clone();
    let local_node_id = ctx.state.local_node_id.to_string();

    if target_node_id.trim().is_empty() {
        return Err(ProtocolError::bad_request(
            "bus.invalid_argument: target_node_id must not be empty",
        ));
    }

    let topic_for_check = topic.clone();
    let org_id_for_check = org_id.clone();
    let db_for_check = db.clone();
    let instance_for_check = g.instance.as_str().to_string();
    run_blocking(move || {
        topics::get_topic(
            &db_for_check,
            &instance_for_check,
            &org_id_for_check,
            &topic_for_check,
        )
        .map_err(map_bus_error)?
        .ok_or_else(|| {
            ProtocolError::not_found(format!("bus.topic_not_found: '{topic_for_check}'"))
        })
        .map(|_| ())
    })
    .await?;

    let topic_for_call = topic.clone();
    let org_id_for_call = org_id.clone();
    let target_for_call = target_node_id.clone();
    let svc = g.svc.clone();
    let leader_epoch = run_blocking(move || -> Result<u32, ProtocolError> {
        let svc = svc;
        let coordinator = svc.replication().ok_or_else(|| {
            ProtocolError::new(
                ProtocolErrorCode::NotAvailable,
                "bus.replication_disabled: no replication coordinator installed on this node",
            )
        })?;
        coordinator
            .transfer_leader(
                &org_id_for_call,
                &topic_for_call,
                partition,
                &target_for_call,
            )
            .map_err(map_repl_error)
    })
    .await?;

    let _ = repository::log_audit(
        &db,
        Some(&user_id),
        None,
        "bus.leader.transfer",
        Some(&topic),
        Some(&format!(
            "partition={partition} target_node_id={target_node_id} leader_epoch={leader_epoch}"
        )),
        None,
        Some(&local_node_id),
    );

    Ok(BusPayload::LeaderTransferResponse { leader_epoch })
}

// =============================================================================
// Quotas (Admin, per org — full replace; `QuotaGet` reports the real
// configured rates/`max_groups` via `QuotaManager`'s getters, follow-up toru
// P task 7)
// =============================================================================

fn quota_get_v1(ctx: &HandlerContext, instance_id: &str) -> Result<BusPayload, ProtocolError> {
    let g = gate_admin(ctx, instance_id)?;
    let q = g.svc.quota();
    Ok(BusPayload::QuotaGetResponse {
        quota: BusQuotaWire {
            max_topics: q.max_topics(&g.org_id),
            max_partitions: q.max_partitions(&g.org_id),
            max_bytes_total: q.max_bytes_total(&g.org_id),
            // Follow-up toru P task 7: `QuotaManager` now has getters for
            // both rates, so `QuotaGet` reports the REAL configured value
            // instead of an unconditional `None`.
            produce_msgs_per_sec: Some(q.produce_msgs_per_sec(&g.org_id)),
            produce_bytes_per_sec: Some(q.produce_bytes_per_sec(&g.org_id)),
            max_groups: q.max_groups(&g.org_id),
        },
    })
}

#[allow(clippy::too_many_arguments)]
fn quota_set_v1(
    ctx: &HandlerContext,
    instance_id: &str,
    max_topics: u32,
    max_partitions: u32,
    max_bytes_total: u64,
    produce_msgs_per_sec: u32,
    produce_bytes_per_sec: u64,
    // `None` = leave `max_groups` unchanged (follow-up toru P task 6/7) —
    // the same "omitted means unchanged" convention `TopicUpdateRequest`
    // already uses, so a client not yet aware of this field cannot
    // silently reset an org's group ceiling to some arbitrary default.
    max_groups: Option<u32>,
) -> Result<BusPayload, ProtocolError> {
    let g = gate_admin(ctx, instance_id)?;
    let max_groups = max_groups.unwrap_or_else(|| g.svc.quota().max_groups(&g.org_id));
    g.svc.quota().set_org_quota(
        &g.org_id,
        quota::QuotaConfig {
            max_topics,
            max_partitions,
            max_bytes_total,
            produce_msgs_per_sec,
            produce_bytes_per_sec,
            max_groups,
        },
    );
    // Not in PLAN §8.2's literal audit-action list (quotas are absent from
    // it entirely) — added anyway because every OTHER admin mutation in
    // this file has an audit row, and a silent quota change would be the
    // one exception to that pattern rather than a deliberate omission.
    let _ = repository::log_audit(
        &ctx.state.db,
        Some(&g.user_id),
        None,
        "bus.quota.set",
        None,
        Some(&format!(
            "max_topics={max_topics} max_partitions={max_partitions} max_bytes_total={max_bytes_total} \
             produce_msgs_per_sec={produce_msgs_per_sec} produce_bytes_per_sec={produce_bytes_per_sec} \
             max_groups={max_groups}"
        )),
        None,
        Some(&ctx.state.local_node_id),
    );
    Ok(BusPayload::QuotaSetResponse {
        quota: BusQuotaWire {
            max_topics,
            max_partitions,
            max_bytes_total,
            produce_msgs_per_sec: Some(produce_msgs_per_sec),
            produce_bytes_per_sec: Some(produce_bytes_per_sec),
            max_groups,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::models::AuditLogFilters;
    use crate::db::DbPool;
    use std::sync::{Mutex, OnceLock};
    use tentaflow_protocol::SessionAuth;

    /// The `BusInstanceId` the whole test binary's shared `bus::global()`
    /// singleton runs as — see `bus_fixture`'s doc for why one `bus::init`
    /// call is shared process-wide.
    fn fixture_instance_id() -> bus::instance::BusInstanceId {
        bus::instance::BusInstanceId::parse("tentabus-00000001").expect("valid instance id")
    }

    /// The `PermissionChecker` backing the shared `bus::global()`
    /// singleton's `InstanceBusAuthorizer` — kept in its own `OnceLock` (not
    /// just built inside `bus_fixture` and dropped) so `seed_membership`
    /// can reach the SAME instance to grant a permission and refresh it;
    /// building a second `PermissionChecker` over the same `db` would grant
    /// into the right table but never invalidate the cache the live
    /// authorizer actually reads from.
    fn shared_checker(db: &DbPool) -> std::sync::Arc<crate::addon::permissions::PermissionChecker> {
        static CHECKER: OnceLock<std::sync::Arc<crate::addon::permissions::PermissionChecker>> =
            OnceLock::new();
        CHECKER
            .get_or_init(|| {
                std::sync::Arc::new(crate::addon::permissions::PermissionChecker::new(
                    db.clone(),
                ))
            })
            .clone()
    }

    /// Builds an `Arc<AppState>` sharing `db` and its `shared_checker` — the
    /// same wiring `handler_ctx` needs for a request `HandlerContext` and
    /// `bus_fixture` needs for `app_gate::test_support::install_app_instance`
    /// (which takes `&Arc<AppState>`, not a bare `DbPool`). Factored out so
    /// both call sites build the identical state instead of drifting apart.
    fn test_state(db: &DbPool) -> std::sync::Arc<crate::dispatch::state::AppState> {
        let mut state = crate::dispatch::state::AppState::for_test();
        let checker = shared_checker(db);
        let state_mut =
            std::sync::Arc::get_mut(&mut state).expect("sole owner right after for_test()");
        state_mut.db = db.clone();
        state_mut.permission_checker = Some(checker);
        state
    }

    /// One shared, migrated in-memory DB for every test in this module and
    /// ONE `bus::init` call for the whole test binary — `bus::init`'s
    /// `OnceLock` (like `sync::runtime::init`'s, see `dispatch/environment.
    /// rs`'s `locked_env_fixture`) only honors its FIRST caller's config, so
    /// every test that needs a live `BusService` must share the exact same
    /// `DbPool` a real request's `ctx.state.db` also points at (otherwise
    /// `topics::list_topics(ctx.state.db, ..)` and the service's own writes
    /// would silently disagree about which database is the source of
    /// truth). Serialized with a `Mutex` because several tests here mutate
    /// shared state (topic creation, ACL rows, audit log) that must not
    /// interleave.
    fn bus_fixture() -> (std::sync::MutexGuard<'static, ()>, DbPool) {
        static LOCK: Mutex<()> = Mutex::new(());
        static DB: OnceLock<DbPool> = OnceLock::new();
        let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let db = DB
            .get_or_init(|| {
                let conn = rusqlite::Connection::open_in_memory().expect("open db");
                crate::db::migrations::run(&conn).expect("run migrations");
                std::sync::Arc::new(crate::db::Db::from_connection(conn))
            })
            .clone();
        if bus::global().is_none() {
            let dir = tempfile::tempdir().expect("bus_dir tempdir");
            let local_conn = rusqlite::Connection::open_in_memory().expect("open local db");
            crate::bus::db::migrate(&local_conn).expect("migrate local db");
            let local_db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(local_conn));
            let checker = shared_checker(&db);
            let authorizer =
                std::sync::Arc::new(crate::services::bus_authorizer::InstanceBusAuthorizer::new(
                    db.clone(),
                    fixture_instance_id(),
                    checker,
                ));
            bus::init(bus::BusInitConfig {
                instance_id: fixture_instance_id(),
                local_db,
                bus_dir: dir.path().to_path_buf(),
                db: db.clone(),
                authorizer,
                retention_interval: None,
                dedup_expected_rate_per_sec: 10_000,
                partition_handle_lru: None,
                publish_ack_timeout: bus::DEFAULT_PUBLISH_ACK_TIMEOUT,
            })
            .expect("bus init");
            // Kept alive for the process (mirrors `locked_env_fixture`'s own
            // `mem::forget` of its tempdir) — `BusService` holds file
            // handles under this path for as long as the singleton lives.
            std::mem::forget(dir);

            // plan-app-platform §7 W7: the dispatch gate now resolves the
            // instance through `gate`/`app_gate::require_instance_permission`
            // + the addon permission matrix, exactly like a real request —
            // so the fixture needs a real, ENABLED `addons` row for
            // `fixture_instance_id()`, not just a live engine. Routed through
            // the shared `app_gate::test_support::install_app_instance`
            // helper (suffix `"00000001"` so the resulting addon_id matches
            // `fixture_instance_id()` exactly) instead of a hand-rolled
            // manifest rewrite + INSERT, now that `test_state` gives this
            // fixture the `Arc<AppState>` that helper needs.
            let state = test_state(&db);
            app_gate::test_support::install_app_instance(
                &state,
                bus::instance::BusInstanceId::PACKAGE_ID,
                "00000001",
                &[],
            );
        }
        (guard, db)
    }

    /// Builds an `OrgContext` carrying only ORG-RBAC flags — as of
    /// plan-app-platform §7 W4 finding 2, `bus.read`/`bus.write`/`bus.admin`
    /// are addon-matrix permissions, not org-RBAC ones, so `perms` here must
    /// never carry them (a test that does would be asserting against a flag
    /// `require_read`/`require_admin` no longer reads at all). The one flag
    /// still meaningful here is `"org.admin"` — the org-role half of
    /// `require_admin`'s double lock; the matrix half is granted separately
    /// through `seed_membership`/`seed_bus_permissions`.
    fn org_context(
        org_id: &str,
        user_id: &str,
        perms: &[&str],
    ) -> crate::services::rbac::OrgContext {
        crate::services::rbac::OrgContext {
            user_id: user_id.to_string(),
            org_id: org_id.to_string(),
            role_id: "test-role".to_string(),
            permissions: perms.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// plan-app-platform §7 W4: grants exactly `perms` through the addon
    /// permission matrix on the shared `bus_fixture` instance for `user_id`
    /// and refreshes the live checker — the real authority `require_read`/
    /// `require_admin`/`InstanceBusAuthorizer` all consult. `seed_membership`
    /// (below) is the common "grant everything" case; this is the partial-
    /// grant primitive it (and `capabilities_v1`'s tests, which need
    /// read-without-admin) build on.
    fn seed_bus_permissions(db: &DbPool, user_id: &str, perms: &[&str]) -> String {
        let org_id = format!("org-{}", uuid::Uuid::new_v4());
        let instance_id = fixture_instance_id();
        for perm in perms {
            crate::db::repository::upsert_permission(
                db,
                instance_id.as_str(),
                "user",
                user_id,
                perm,
                "allow",
                None,
            )
            .unwrap();
        }
        shared_checker(db).refresh_addon(instance_id.as_str());
        org_id
    }

    /// plan-app-platform §7 W4: TentaBus authorization comes from the addon
    /// permission matrix now (`InstanceBusAuthorizer`), not org-RBAC — grants
    /// the full `bus.read`/`bus.write`/`bus.admin` set on the shared
    /// `bus_fixture` instance for `user_id`, instead of seeding an org-RBAC
    /// role/membership row. `role` is accepted but unused — kept so every
    /// one of this file's existing call sites needs no change. Callers that
    /// also exercise `require_admin`'s double lock must additionally pass
    /// `"org.admin"` to `org_context` — this function only grants the
    /// MATRIX half.
    fn seed_membership(db: &DbPool, user_id: &str, _role: &str) -> String {
        seed_bus_permissions(db, user_id, &["bus.read", "bus.write", "bus.admin"])
    }

    fn handler_ctx(db: DbPool, org: crate::services::rbac::OrgContext) -> HandlerContext {
        // `test_state` points `ctx.state.db`/`ctx.state.permission_checker`
        // at the SAME pool `bus::init`/`seed_membership` were wired to (see
        // `bus_fixture`'s doc), otherwise the addon-matrix gate (`gate_read`/
        // `gate_write`/`gate_admin`) reads a checker over an empty,
        // unrelated database and denies everyone.
        let state = test_state(&db);
        HandlerContext {
            session: SessionAuth::UserSession {
                user_id: *uuid::Uuid::new_v4().as_bytes(),
                role: Some("admin".to_string()),
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state,
            org_context: Some(org),
        }
    }

    /// Test-only helper: opens a FRESH consumer handle for `(group, topic)`
    /// off the async runtime (`tokio::task::spawn_blocking` — `bus::*`
    /// calls block synchronously, this file's own module-level BLOCKING
    /// doc) and returns its total lag across every subscribed partition.
    /// Opening a new handle each call (rather than keeping one alive
    /// across `.await` points) is deliberate: `lag()` reads the durable
    /// committed offset straight from `GroupOffsetStore`, so a fresh handle
    /// reports the exact same value an existing one would, without the
    /// `Send`-across-await-points friction of holding a live
    /// `ConsumerHandle` in an async test body.
    async fn group_total_lag(
        svc: std::sync::Arc<bus::BusService>,
        bctx: BusCallContext,
        group: String,
        topic: String,
    ) -> u64 {
        tokio::task::spawn_blocking(move || {
            let handle = svc
                .open_consumer(
                    &bctx,
                    &group,
                    std::slice::from_ref(&topic),
                    bus::ConsumerConfig {
                        commit_mode: groups::CommitMode::Explicit,
                    },
                )
                .expect("open consumer for lag check");
            handle.lag().expect("lag").into_iter().map(|(_, l)| l).sum()
        })
        .await
        .expect("blocking lag check task panicked")
    }

    /// Test-only helper: the sorted list of offsets `DlqList` currently
    /// returns for `topic` — used by the discard tests below to assert on
    /// what a discard did/did not remove from the page.
    async fn dlq_list_offsets(ctx: &HandlerContext, topic: String) -> Vec<u64> {
        match dlq_list_v1(
            ctx,
            fixture_instance_id().as_str(),
            topic,
            None,
            vec![],
            10,
            None,
        )
        .await
        .expect("dlq list")
        {
            BusPayload::DlqListResponse { result } => {
                result.records.iter().map(|r| r.offset).collect()
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn topic_create_denied_without_bus_admin_permission() {
        let (_guard, db) = bus_fixture();
        let org = org_context("org-x", "u-no-perm", &[]);
        let ctx = handler_ctx(db, org);
        let err = topic_create_v1(
            &ctx,
            fixture_instance_id().as_str(),
            "orders.created".to_string(),
            BusTopicOptionsWire::default(),
        )
        .await
        .expect_err("must be denied without bus.admin");
        assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
    }

    #[tokio::test]
    async fn topic_create_and_list_round_trip_with_admin_permission() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-admin-{}", uuid::Uuid::new_v4());
        let org_id = seed_membership(&db, &user_id, "org_admin");
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let topic_name = format!("orders.{}", uuid::Uuid::new_v4().simple());
        let ctx = handler_ctx(db, org.clone());

        let created = topic_create_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            BusTopicOptionsWire::default(),
        )
        .await
        .expect("topic create must succeed for an org_admin");
        match created {
            BusPayload::TopicCreateResponse { topic } => {
                assert_eq!(topic.name, topic_name);
            }
            other => panic!("unexpected response: {other:?}"),
        }

        let listed = topic_list_v1(&ctx, fixture_instance_id().as_str())
            .await
            .expect("topic list must succeed");
        match listed {
            BusPayload::TopicListResponse { topics } => {
                assert!(
                    topics.iter().any(|t| t.name == topic_name),
                    "created topic must appear in the list"
                );
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    /// Owner decision B: `durability_class: "critical"` in a create request
    /// resolves to `FsyncBatchFull` server-side (Critical resolves the same
    /// way in every `NodeEnvironment`), and `TopicDetailResponse` reports
    /// both the resolved policy AND the class it derives from — the class
    /// is read back out of the resolved policy, not stored separately, so
    /// this exercises `TopicConfig::durability_class` end to end through
    /// the dispatch layer.
    #[tokio::test]
    async fn topic_create_with_critical_durability_class_shows_up_in_detail() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-admin-{}", uuid::Uuid::new_v4());
        let org_id = seed_membership(&db, &user_id, "org_admin");
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let topic_name = format!("critical.{}", uuid::Uuid::new_v4().simple());
        let ctx = handler_ctx(db, org.clone());

        let created = topic_create_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            BusTopicOptionsWire {
                durability_class: Some("critical".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("topic create must succeed for an org_admin");
        match created {
            BusPayload::TopicCreateResponse { topic } => {
                assert_eq!(topic.durability_class, "critical");
                assert_eq!(topic.durability, "fsync_batch_full");
            }
            other => panic!("unexpected response: {other:?}"),
        }

        let detail = topic_detail_v1(&ctx, fixture_instance_id().as_str(), topic_name.clone())
            .await
            .expect("topic detail must succeed for bus.read");
        match detail {
            BusPayload::TopicDetailResponse { topic, .. } => {
                assert_eq!(topic.durability_class, "critical");
                assert_eq!(topic.durability, "fsync_batch_full");
                assert!(!topic.durability_explicit);
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    /// v143 (`SUM/tentabus/KRYTYK-M1-R5.md` R5-1): `TopicListResponse` rows
    /// carry the same durability/class/explicit trio as `TopicDetail` —
    /// previously the list had none of this at all, so the M01 table could
    /// never show a topic's real durability tier.
    #[tokio::test]
    async fn topic_list_rows_carry_durability_class_and_explicit() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-admin-{}", uuid::Uuid::new_v4());
        let org_id = seed_membership(&db, &user_id, "org_admin");
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let critical_name = format!("krytyk.crit.{}", uuid::Uuid::new_v4().simple());
        let explicit_name = format!("krytyk.explicit.{}", uuid::Uuid::new_v4().simple());
        let ctx = handler_ctx(db, org.clone());

        topic_create_v1(
            &ctx,
            fixture_instance_id().as_str(),
            critical_name.clone(),
            BusTopicOptionsWire {
                durability_class: Some("critical".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("create critical topic");
        topic_create_v1(
            &ctx,
            fixture_instance_id().as_str(),
            explicit_name.clone(),
            BusTopicOptionsWire {
                durability: Some("os".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("create explicit-override topic");

        let listed = topic_list_v1(&ctx, fixture_instance_id().as_str())
            .await
            .expect("topic list must succeed");
        match listed {
            BusPayload::TopicListResponse { topics } => {
                let critical = topics
                    .iter()
                    .find(|t| t.name == critical_name)
                    .expect("critical topic must be listed");
                assert_eq!(critical.durability, "fsync_batch_full");
                assert_eq!(critical.durability_class, "critical");
                assert!(!critical.durability_explicit);

                let explicit = topics
                    .iter()
                    .find(|t| t.name == explicit_name)
                    .expect("explicit topic must be listed");
                assert_eq!(explicit.durability, "os");
                assert!(explicit.durability_explicit);
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    /// The R5 P1-2 case (`SUM/tentabus/KRYTYK-M1-R5.md` b.2): an update
    /// that sends ONLY `durability_class` (no explicit `durability`) must
    /// actually downgrade a Critical topic to Standard, through the full
    /// dispatch path — not silently no-op the way the UI's stale-prefill
    /// bug made it look in the critique.
    #[tokio::test]
    async fn topic_update_with_class_only_downgrades_critical_to_standard() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-admin-{}", uuid::Uuid::new_v4());
        let org_id = seed_membership(&db, &user_id, "org_admin");
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let topic_name = format!("krytyk.std.{}", uuid::Uuid::new_v4().simple());
        let ctx = handler_ctx(db, org.clone());

        topic_create_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            BusTopicOptionsWire {
                durability_class: Some("critical".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("create critical topic");

        let updated = topic_update_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            BusTopicOptionsWire {
                durability_class: Some("standard".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("update to standard class must succeed");
        match updated {
            BusPayload::TopicUpdateResponse { topic } => {
                assert_eq!(
                    topic.durability, "fsync_interval:50",
                    "class-only update must actually change the resolved policy"
                );
                assert_eq!(topic.durability_class, "standard");
                assert!(!topic.durability_explicit);
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    /// `durability: "auto"` on update clears an explicit override and
    /// re-resolves from the topic's current effective class.
    #[tokio::test]
    async fn topic_update_with_auto_durability_clears_explicit_override() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-admin-{}", uuid::Uuid::new_v4());
        let org_id = seed_membership(&db, &user_id, "org_admin");
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let topic_name = format!("orders.auto.{}", uuid::Uuid::new_v4().simple());
        let ctx = handler_ctx(db, org.clone());

        topic_create_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            BusTopicOptionsWire {
                durability: Some("fsync_batch_full".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("create explicit-override topic");

        let after_create =
            topic_detail_v1(&ctx, fixture_instance_id().as_str(), topic_name.clone())
                .await
                .expect("detail after create");
        match after_create {
            BusPayload::TopicDetailResponse { topic, .. } => {
                assert!(topic.durability_explicit);
            }
            other => panic!("unexpected response: {other:?}"),
        }

        let updated = topic_update_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            BusTopicOptionsWire {
                durability: Some("auto".to_string()),
                ..Default::default()
            },
        )
        .await
        .expect("auto-reset update must succeed");
        match updated {
            BusPayload::TopicUpdateResponse { topic } => {
                assert!(!topic.durability_explicit);
                // Critical resolves to the same FsyncBatchFull policy in
                // every environment, so the wire policy string is
                // unchanged — only `durability_explicit` flips.
                assert_eq!(topic.durability, "fsync_batch_full");
                assert_eq!(topic.durability_class, "critical");
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn messages_browse_writes_one_audit_row_per_partition_peeked_with_records() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-viewer-{}", uuid::Uuid::new_v4());
        let org_id = seed_membership(&db, &user_id, "org_admin");
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let topic_name = format!("lab.{}", uuid::Uuid::new_v4().simple());
        let ctx = handler_ctx(db.clone(), org.clone());

        // Single partition so this browse call maps to EXACTLY one
        // `BusService::peek` call — `peek` (not this handler) audits
        // `bus.messages.browse` once per partition it reads AND actually
        // returns records (follow-up toru P task 1 + P3-5 follow-up,
        // `KRYTYK-M1-R3.md`: an empty read is not a data access, so this
        // test publishes a record first — see
        // `messages_browse_of_an_empty_topic_writes_no_audit_row` for the
        // empty-partition counterpart).
        topic_create_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            BusTopicOptionsWire {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .await
        .expect("topic create");

        let g = gate_read(&ctx, fixture_instance_id().as_str())
            .expect("gate resolves the fixture's enabled instance");
        let svc = g.svc.clone();
        let bctx = bus_ctx(&ctx, &g);
        {
            let svc = svc.clone();
            let bctx = bctx.clone();
            let topic_name = topic_name.clone();
            tokio::task::spawn_blocking(move || {
                svc.publish(
                    &bctx,
                    &topic_name,
                    bus::PublishBatch {
                        partition: Some(0),
                        producer: None,
                        records: vec![bus::PublishRecord {
                            key: None,
                            headers: vec![],
                            payload: Bytes::from_static(b"r-0"),
                            timestamp_ms: 0,
                            schema_id: 0,
                        }],
                    },
                )
                .expect("publish 1 record");
            })
            .await
            .expect("publish task panicked");
        }

        let before = repository::list_audit_logs(
            &db,
            &AuditLogFilters {
                action: Some("bus.messages.browse".to_string()),
                ..Default::default()
            },
            0,
            100,
        )
        .expect("list audit logs");

        messages_browse_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            None,
            vec![],
            10,
            None,
        )
        .await
        .expect("messages browse must succeed for bus.read");

        let after = repository::list_audit_logs(
            &db,
            &AuditLogFilters {
                action: Some("bus.messages.browse".to_string()),
                ..Default::default()
            },
            0,
            100,
        )
        .expect("list audit logs");

        assert_eq!(
            after.len(),
            before.len() + 1,
            "exactly one bus.messages.browse row per partition peeked WITH records (one partition here)"
        );
    }

    /// P3-5 follow-up (`KRYTYK-M1-R3.md`, coordinator decision "Decyzje po
    /// R3"): browsing a topic with no records yet must not write a
    /// `bus.messages.browse` row — the concrete regression this closes is 8
    /// audit rows (7 of them `count=0`) for one M08/M05 open on a topic with
    /// data on only one of several partitions.
    #[tokio::test]
    async fn messages_browse_of_an_empty_topic_writes_no_audit_row() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-viewer-{}", uuid::Uuid::new_v4());
        let org_id = seed_membership(&db, &user_id, "org_admin");
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let topic_name = format!("lab.{}", uuid::Uuid::new_v4().simple());
        let ctx = handler_ctx(db.clone(), org.clone());

        topic_create_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            BusTopicOptionsWire {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .await
        .expect("topic create");

        // `bus_fixture` shares ONE db (behind the module-level `LOCK`) across
        // every test in this file — count before/after rather than asserting
        // the whole log is empty, same pattern as
        // `messages_browse_writes_one_audit_row_per_partition_peeked_with_records`.
        let before = repository::list_audit_logs(
            &db,
            &AuditLogFilters {
                action: Some("bus.messages.browse".to_string()),
                ..Default::default()
            },
            0,
            100,
        )
        .expect("list audit logs");

        messages_browse_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            None,
            vec![],
            10,
            None,
        )
        .await
        .expect("messages browse must succeed for bus.read even with nothing to show");

        let after = repository::list_audit_logs(
            &db,
            &AuditLogFilters {
                action: Some("bus.messages.browse".to_string()),
                ..Default::default()
            },
            0,
            100,
        )
        .expect("list audit logs");

        assert_eq!(
            after.len(),
            before.len(),
            "browsing an empty topic must not write a bus.messages.browse row"
        );
    }

    /// R3-2 follow-up (`KRYTYK-M1-R3.md`, coordinator decision "Decyzje po
    /// R3"): `partition: Some(3)` must peek ONLY partition 3 — a topic with
    /// records on partitions 0 AND 3 must come back with partition-3
    /// records exclusively and a single `partitions[]` entry, not the old
    /// client-side-filter dead end where the server always answered from
    /// partition 0.
    #[tokio::test]
    async fn messages_browse_with_partition_filter_returns_only_that_partition() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-viewer-{}", uuid::Uuid::new_v4());
        let org_id = seed_membership(&db, &user_id, "org_admin");
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let topic_name = format!("lab.{}", uuid::Uuid::new_v4().simple());
        let ctx = handler_ctx(db.clone(), org.clone());

        topic_create_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            BusTopicOptionsWire {
                partitions: Some(4),
                ..Default::default()
            },
        )
        .await
        .expect("topic create");

        let g = gate_read(&ctx, fixture_instance_id().as_str())
            .expect("gate resolves the fixture's enabled instance");
        let svc = g.svc.clone();
        let bctx = bus_ctx(&ctx, &g);
        for (partition, count) in [(0u32, 5u32), (3u32, 3u32)] {
            let svc = svc.clone();
            let bctx = bctx.clone();
            let topic_name = topic_name.clone();
            tokio::task::spawn_blocking(move || {
                svc.publish(
                    &bctx,
                    &topic_name,
                    bus::PublishBatch {
                        partition: Some(partition),
                        producer: None,
                        records: (0..count)
                            .map(|i| bus::PublishRecord {
                                key: None,
                                headers: vec![],
                                payload: Bytes::from(format!("p{partition}-r{i}")),
                                timestamp_ms: 0,
                                schema_id: 0,
                            })
                            .collect(),
                    },
                )
                .expect("publish batch")
            })
            .await
            .expect("publish task panicked");
        }

        let resp = messages_browse_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            None,
            vec![],
            10,
            Some(3),
        )
        .await
        .expect("messages browse with a valid partition filter must succeed");

        match resp {
            BusPayload::MessagesBrowseResponse { result } => {
                assert!(
                    !result.records.is_empty(),
                    "partition 3 has records and must not come back empty"
                );
                assert!(
                    result.records.iter().all(|r| r.partition == 3),
                    "partition filter must exclude every other partition's records: {:?}",
                    result
                        .records
                        .iter()
                        .map(|r| r.partition)
                        .collect::<Vec<_>>()
                );
                assert_eq!(
                    result.partitions.len(),
                    1,
                    "partition filter must report exactly the one requested partition, not all 4"
                );
                assert_eq!(result.partitions[0].partition, 3);
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    /// R3-2 follow-up: a partition filter past the topic's actual partition
    /// count must fail loudly with the stable `bus.partition_out_of_range`
    /// wire error code, not silently fall back to "every partition" or a
    /// misleading empty result.
    #[tokio::test]
    async fn messages_browse_with_out_of_range_partition_filter_is_rejected() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-viewer-{}", uuid::Uuid::new_v4());
        let org_id = seed_membership(&db, &user_id, "org_admin");
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let topic_name = format!("lab.{}", uuid::Uuid::new_v4().simple());
        let ctx = handler_ctx(db.clone(), org.clone());

        topic_create_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            BusTopicOptionsWire {
                partitions: Some(2),
                ..Default::default()
            },
        )
        .await
        .expect("topic create");

        let err = messages_browse_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            None,
            vec![],
            10,
            Some(2),
        )
        .await
        .expect_err("partition 2 is out of range for a 2-partition topic (0, 1)");
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
        assert!(
            err.message.contains("bus.partition_out_of_range"),
            "error message must carry the stable bus.partition_out_of_range code: {}",
            err.message
        );
    }

    /// Follow-up toru P task 1: `MessagesBrowse` uses `BusService::peek`, a
    /// stateless read that creates no `bus_groups` row — unlike the earlier
    /// throwaway-consumer implementation, which left one behind per call.
    #[tokio::test]
    async fn messages_browse_leaves_no_bus_groups_row_behind() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-viewer-{}", uuid::Uuid::new_v4());
        let org_id = seed_membership(&db, &user_id, "org_admin");
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let topic_name = format!("lab.{}", uuid::Uuid::new_v4().simple());
        let ctx = handler_ctx(db.clone(), org.clone());

        topic_create_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            BusTopicOptionsWire {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .await
        .expect("topic create");

        let local_db = bus::global()
            .expect("bus_fixture initialized")
            .local_db()
            .clone();
        let before = repository::bus_group_list(&local_db, &org_id).expect("list groups");

        messages_browse_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            None,
            vec![],
            10,
            None,
        )
        .await
        .expect("messages browse must succeed for bus.read");

        let after = repository::bus_group_list(&local_db, &org_id).expect("list groups");
        assert_eq!(
            after.len(),
            before.len(),
            "a browse must not create any bus_groups row"
        );
    }

    #[tokio::test]
    async fn messages_browse_denied_without_bus_read_permission() {
        let (_guard, db) = bus_fixture();
        let org = org_context("org-y", "u-no-read", &[]);
        let ctx = handler_ctx(db, org);
        let err = messages_browse_v1(
            &ctx,
            fixture_instance_id().as_str(),
            "some.topic".to_string(),
            None,
            vec![],
            10,
            None,
        )
        .await
        .expect_err("must be denied without bus.read");
        assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
    }

    // ---- Capabilities (follow-up toru P task 5) -----------------------

    #[tokio::test]
    async fn capabilities_reports_org_permissions_and_site_admin_tier() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-caps-{}", uuid::Uuid::new_v4());
        let org_id = seed_bus_permissions(&db, &user_id, &["bus.read", "bus.write"]);
        let org = org_context(&org_id, &user_id, &[]);
        let ctx = handler_ctx(db, org);
        let resp = capabilities_v1(&ctx, fixture_instance_id().as_str())
            .await
            .expect("capabilities must succeed for any authenticated org session");
        match resp {
            BusPayload::CapabilitiesResponse { capabilities } => {
                assert!(capabilities.can_read);
                assert!(capabilities.can_write);
                assert!(!capabilities.can_admin, "org has no bus.admin permission");
                // `handler_ctx` always mints an "admin"-role `UserSession`
                // (see its own doc) — this is the SEPARATE
                // `SessionAuthKind::Admin` site-admin tier, independent of
                // `bus.admin`; both must be readable independently of the
                // other (see `capabilities_is_site_admin_false_for_a_non_
                // admin_role_session` for the opposite case).
                assert!(capabilities.is_site_admin);
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn capabilities_is_site_admin_false_for_a_non_admin_role_session() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-caps2-{}", uuid::Uuid::new_v4());
        let org_id = seed_bus_permissions(&db, &user_id, &["bus.read"]);
        let org = org_context(&org_id, &user_id, &[]);
        let mut state = crate::dispatch::state::AppState::for_test();
        let checker = shared_checker(&db);
        {
            let state_mut =
                std::sync::Arc::get_mut(&mut state).expect("sole owner right after for_test()");
            state_mut.db = db;
            state_mut.permission_checker = Some(checker);
        }
        let ctx = HandlerContext {
            session: SessionAuth::UserSession {
                user_id: *uuid::Uuid::new_v4().as_bytes(),
                role: Some("user".to_string()),
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state,
            org_context: Some(org),
        };
        let resp = capabilities_v1(&ctx, fixture_instance_id().as_str())
            .await
            .expect("capabilities must succeed");
        match resp {
            BusPayload::CapabilitiesResponse { capabilities } => {
                assert!(capabilities.can_read);
                assert!(
                    !capabilities.is_site_admin,
                    "role 'user' is not a site admin"
                );
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    /// plan-app-platform §4.4: `capabilities_v1` is now a hard `gate_read`
    /// gate — it resolves the ADDRESSED INSTANCE before it ever looks at
    /// `org_context` (`gate` → `app_gate::require_instance_permission`
    /// checks the instance exists/is enabled first, the caller's org
    /// context second). A fully instance-less DB (this test's setup before
    /// this rewrite) now fails on `AppUnavailable` from that instance
    /// lookup, never reaching the `AuthRequired` check this test means to
    /// exercise. SETUP adapted to `bus_fixture()` (a real installed,
    /// ENABLED instance) so the instance lookup succeeds and the missing
    /// `org_context` is what actually fails the call — the ASSERTION
    /// itself (`AuthRequired`) is unchanged.
    #[tokio::test]
    async fn capabilities_requires_an_org_context() {
        let (_guard, db) = bus_fixture();
        let ctx = HandlerContext {
            session: SessionAuth::UserSession {
                user_id: *uuid::Uuid::new_v4().as_bytes(),
                role: None,
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state: test_state(&db),
            org_context: None,
        };
        let err = capabilities_v1(&ctx, fixture_instance_id().as_str())
            .await
            .expect_err("must fail without an org context");
        assert_eq!(err.code, ProtocolErrorCode::AuthRequired);
    }

    // ---- OffsetReset::Earliest (M1-R2 review N-1, coordinator decision 1) --

    /// N-1: `Earliest` used to route through `ConsumerHandle::
    /// seek_to_earliest`, which commits via the ordinary CONSUMER path
    /// (`GroupOffsetStore::commit`) — that path's own monotonicity guard
    /// rejects any move backward with `OffsetRegression`, so resetting to
    /// earliest was IMPOSSIBLE for any group that had ever committed past
    /// offset 0 (i.e. every group an operator would actually want to reset).
    /// It must now go through the same admin `reset_offset` path
    /// (`force_commit` + `bus.offset.reset` audit) every other mode uses.
    #[tokio::test]
    async fn offset_reset_earliest_moves_a_higher_commit_backward_and_lag_grows() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-admin-{}", uuid::Uuid::new_v4());
        let org_id = seed_membership(&db, &user_id, "org_admin");
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let topic_name = format!("orders.{}", uuid::Uuid::new_v4().simple());
        let group_name = "notifier".to_string();
        let ctx = handler_ctx(db.clone(), org.clone());

        topic_create_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            BusTopicOptionsWire {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .await
        .expect("topic create");

        let g = gate_read(&ctx, fixture_instance_id().as_str())
            .expect("gate resolves the fixture's enabled instance");
        let svc = g.svc.clone();
        let bctx = bus_ctx(&ctx, &g);

        // `BusService::publish`/`open_consumer`/`ConsumerHandle::fetch`/
        // `commit` are synchronous and BLOCK (this file's own module-level
        // BLOCKING doc) — calling them directly from a `#[tokio::test]`
        // body panics ("cannot block the current thread from within a
        // runtime"), so every direct engine call below goes through
        // `tokio::task::spawn_blocking`, exactly like the dispatch
        // functions under test already do internally via `run_blocking`.
        {
            let svc = svc.clone();
            let bctx = bctx.clone();
            let topic_name = topic_name.clone();
            let group_name = group_name.clone();
            tokio::task::spawn_blocking(move || {
                svc.publish(
                    &bctx,
                    &topic_name,
                    bus::PublishBatch {
                        partition: Some(0),
                        producer: None,
                        records: (0..5u32)
                            .map(|i| bus::PublishRecord {
                                key: None,
                                headers: vec![],
                                payload: Bytes::from(format!("r-{i}")),
                                timestamp_ms: 0,
                                schema_id: 0,
                            })
                            .collect(),
                    },
                )
                .expect("publish 5 records");

                let handle = svc
                    .open_consumer(
                        &bctx,
                        &group_name,
                        std::slice::from_ref(&topic_name),
                        bus::ConsumerConfig {
                            commit_mode: groups::CommitMode::Explicit,
                        },
                    )
                    .expect("open consumer");
                handle.fetch(1024 * 1024, 10).expect("fetch");
                handle
                    .commit(&[(
                        bus::TopicPartition {
                            topic: topic_name.clone(),
                            partition: 0,
                        },
                        5,
                    )])
                    .expect("commit fully caught up");
            })
            .await
            .expect("blocking setup task panicked");
        }

        let lag_before = group_total_lag(
            svc.clone(),
            bctx.clone(),
            group_name.clone(),
            topic_name.clone(),
        )
        .await;
        assert_eq!(lag_before, 0, "fully committed before the reset");

        let resp = offset_reset_v1(
            &ctx,
            fixture_instance_id().as_str(),
            group_name.clone(),
            topic_name.clone(),
            0,
            BusOffsetResetMode::Earliest,
        )
        .await
        .expect(
            "Earliest reset must succeed via the admin reset_offset path, \
             not fail with OffsetRegression",
        );
        match resp {
            BusPayload::OffsetResetResponse { new_offset } => {
                assert_eq!(new_offset, 0);
            }
            other => panic!("unexpected response: {other:?}"),
        }

        let lag_after = group_total_lag(svc, bctx, group_name, topic_name).await;
        assert_eq!(
            lag_after, 5,
            "lag must grow back to the full backlog after resetting to earliest"
        );

        let audit = repository::list_audit_logs(
            &db,
            &AuditLogFilters {
                action: Some("bus.offset.reset".to_string()),
                ..Default::default()
            },
            0,
            10,
        )
        .expect("list audit logs");
        assert!(
            !audit.is_empty(),
            "Earliest reset must go through the audited admin path, not a bare commit"
        );
    }

    // ---- Hidden `tf-`-prefixed groups (M1-R2 review N-7, decisions 3/7) ----

    /// `GroupList` and `StatsSnapshot`'s group KPI/lag figures must never
    /// surface a `tf-`-prefixed group, even if a `bus_groups` row for one
    /// exists (a leftover from before this filter existed, or any future
    /// internal tool reusing the convention) — defense in depth independent
    /// of `bus::init`'s own leftover-row cleanup.
    #[tokio::test]
    async fn group_list_and_stats_snapshot_hide_tf_prefixed_groups() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-admin-{}", uuid::Uuid::new_v4());
        let org_id = seed_membership(&db, &user_id, "org_admin");
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let topic_name = format!("orders.{}", uuid::Uuid::new_v4().simple());
        let ctx = handler_ctx(db.clone(), org.clone());

        topic_create_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            BusTopicOptionsWire {
                partitions: Some(1),
                ..Default::default()
            },
        )
        .await
        .expect("topic create");

        let g = gate_read(&ctx, fixture_instance_id().as_str())
            .expect("gate resolves the fixture's enabled instance");
        let svc = g.svc.clone();
        let bctx = bus_ctx(&ctx, &g);

        // A real, visible group.
        svc.open_consumer(
            &bctx,
            "billing",
            std::slice::from_ref(&topic_name),
            bus::ConsumerConfig {
                commit_mode: groups::CommitMode::Explicit,
            },
        )
        .expect("open real consumer");

        // A leftover/rogue `tf-`-prefixed row — simulates the retired probe
        // (or any future internal tool) rather than going through
        // `open_consumer` under that name, which nothing in this codebase
        // does anymore.
        repository::bus_group_upsert(
            svc.local_db(),
            &repository::DbBusGroup {
                org_id: org_id.clone(),
                group_id: "tf-system-probe".to_string(),
                topic: topic_name.clone(),
                commit_mode: groups::CommitMode::Explicit.as_str().to_string(),
                paused: false,
                created_at_ms: 0,
                updated_at_ms: 0,
            },
        )
        .expect("insert rogue tf- row");

        let listed = group_list_v1(&ctx, fixture_instance_id().as_str())
            .await
            .expect("group list");
        let group_ids: Vec<String> = match listed {
            BusPayload::GroupListResponse { groups } => {
                groups.into_iter().map(|g| g.group).collect()
            }
            other => panic!("unexpected response: {other:?}"),
        };
        assert!(
            group_ids.iter().any(|g| g == "billing"),
            "the real group must still be listed"
        );
        assert!(
            group_ids.iter().all(|g| !g.starts_with("tf-")),
            "no tf-prefixed group may ever appear in GroupList, got {group_ids:?}"
        );

        let snapshot = stats_snapshot_v1(&ctx, fixture_instance_id().as_str())
            .await
            .expect("stats snapshot");
        match snapshot {
            BusPayload::StatsSnapshotResponse { snapshot } => {
                assert_eq!(
                    snapshot.group_count as usize,
                    group_ids.len(),
                    "StatsSnapshot.group_count must equal the filtered GroupList length"
                );
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    // ---- DLQ discard (M1-R2 review N-5, coordinator decision 2) -----------

    /// End-to-end: discard hides a record from `DlqList`, decrements
    /// `StatsSnapshot`'s `dlq_depth`, and `DlqRetryAll` skips it — while the
    /// record's bytes remain physically in the DLQ log (M1's engine still
    /// has no per-record delete; only these caller-facing surfaces change).
    #[tokio::test]
    async fn dlq_discard_hides_the_record_retry_all_skips_it_and_depth_drops() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-admin-{}", uuid::Uuid::new_v4());
        let org_id = seed_membership(&db, &user_id, "org_admin");
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let source_topic = format!("orders.{}", uuid::Uuid::new_v4().simple());
        let ctx = handler_ctx(db.clone(), org.clone());

        topic_create_v1(
            &ctx,
            fixture_instance_id().as_str(),
            source_topic.clone(),
            BusTopicOptionsWire {
                partitions: Some(1),
                max_delivery_attempts: Some(1),
                ..Default::default()
            },
        )
        .await
        .expect("topic create");

        let g = gate_read(&ctx, fixture_instance_id().as_str())
            .expect("gate resolves the fixture's enabled instance");
        let svc = g.svc.clone();
        let bctx = bus_ctx(&ctx, &g);

        // 3 records, each exhausting its single allowed delivery attempt —
        // the REAL failure path, not a raw publish into the DLQ topic.
        // `publish`/`note_delivery_failure` block synchronously (this
        // file's own module-level BLOCKING doc), so this whole setup runs
        // off the async runtime via `spawn_blocking`, exactly like the
        // dispatch functions under test already do internally.
        {
            let svc = svc.clone();
            let bctx = bctx.clone();
            let source_topic = source_topic.clone();
            tokio::task::spawn_blocking(move || {
                for i in 0..3u64 {
                    svc.publish(
                        &bctx,
                        &source_topic,
                        bus::PublishBatch {
                            partition: Some(0),
                            producer: None,
                            records: vec![bus::PublishRecord {
                                key: None,
                                headers: vec![],
                                payload: Bytes::from(format!("payload-{i}")),
                                timestamp_ms: 0,
                                schema_id: 0,
                            }],
                        },
                    )
                    .expect("publish source record");
                    let fetched = bus::FetchedRecordMeta {
                        topic: source_topic.clone(),
                        partition: 0,
                        offset: i,
                        timestamp_ms: 0,
                        key: None,
                        headers: vec![],
                        payload: Bytes::from(format!("payload-{i}")),
                        schema_id: 0,
                    };
                    svc.note_delivery_failure(
                        &bctx,
                        "g",
                        &source_topic,
                        0,
                        i,
                        &fetched,
                        dlq::DlqReason::ConsumerError,
                        "boom",
                    )
                    .expect("note delivery failure sends to dlq");
                }
            })
            .await
            .expect("blocking setup task panicked");
        }

        assert_eq!(
            dlq_list_offsets(&ctx, source_topic.clone()).await,
            vec![0, 1, 2],
            "all 3 records visible before any discard"
        );

        dlq_discard_v1(
            &ctx,
            fixture_instance_id().as_str(),
            source_topic.clone(),
            0,
            1,
        )
        .await
        .expect("discard must succeed");

        assert_eq!(
            dlq_list_offsets(&ctx, source_topic.clone()).await,
            vec![0, 2],
            "a discarded record must not appear in DlqList"
        );

        let snapshot = stats_snapshot_v1(&ctx, fixture_instance_id().as_str())
            .await
            .expect("stats snapshot");
        let dlq_depth = match snapshot {
            BusPayload::StatsSnapshotResponse { snapshot } => snapshot
                .topics
                .iter()
                .find(|t| t.topic == source_topic)
                .map(|t| t.dlq_depth),
            other => panic!("unexpected response: {other:?}"),
        };
        assert_eq!(
            dlq_depth,
            Some(2),
            "dlq_depth must subtract the discarded offset (3 total - 1 discarded)"
        );

        let retry_all = dlq_retry_all_v1(
            &ctx,
            fixture_instance_id().as_str(),
            source_topic.clone(),
            10,
        )
        .await
        .expect("retry all");
        match retry_all {
            BusPayload::DlqRetryAllResponse { retried, failed } => {
                assert_eq!(
                    retried, 2,
                    "the discarded record must be skipped, not retried"
                );
                assert_eq!(failed, 0);
            }
            other => panic!("unexpected response: {other:?}"),
        }

        // The record's bytes are still physically present in the DLQ log
        // (M1's engine has no per-record delete) — only the surfaces above
        // treat it as gone.
        let dlq_topic = dlq::dlq_topic_name(&source_topic);
        assert!(
            svc.dlq_discarded_offsets(&bctx, &dlq_topic, 0)
                .unwrap()
                .contains(&1),
            "the discard marker itself must still exist"
        );
    }

    // =========================================================================
    // M2 replication (PLAN-M2 §1e/§1f)
    // =========================================================================

    /// Deterministic stand-in for `bus::replication::ReplicationManager`
    /// (wave 1, agent EL — not built yet). Every method NOT exercised by
    /// `replica_list_v1`/`replica_reassign_v1`/`leader_transfer_v1` returns
    /// a harmless value rather than `unimplemented!()`: nothing in this
    /// build calls `role`/`preflight`/`await_acks`/`note_offset_commit`/
    /// `evict_node_from_replica_sets` yet (wave 2, agent S), so a panic
    /// there would fail a test for a code path this fake was never meant
    /// to cover, not a real regression.
    struct FakeCoordinator {
        reassign_applied: u32,
        transfer_epoch: u32,
        snapshot_org: String,
        snapshot_topic: String,
        snapshot: bus::ReplicationSnapshot,
    }

    impl bus::ReplicationCoordinator for FakeCoordinator {
        fn role(&self, _org: &str, _topic: &str, _partition: u32) -> bus::PartitionRole {
            bus::PartitionRole::Unavailable {
                reason: bus::UnavailableReason::NoAssignment,
            }
        }
        fn preflight(
            &self,
            _org: &str,
            _topic: &str,
            _partition: u32,
            _acks: topics::Acks,
        ) -> Result<u32, ReplError> {
            Ok(0)
        }
        fn await_acks(
            &self,
            _org: &str,
            _topic: &str,
            _partition: u32,
            _next_offset: u64,
            _acks: topics::Acks,
            _timeout: std::time::Duration,
        ) -> Result<bus::AckOutcome, ReplError> {
            Ok(bus::AckOutcome {
                acked_nodes: 1,
                required: 1,
                hw: 0,
            })
        }
        fn note_offset_commit(
            &self,
            _org: &str,
            _group: &str,
            _topic: &str,
            _partition: u32,
            _offset: u64,
            _attempts: u32,
        ) {
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
            Ok(self.transfer_epoch)
        }
        fn reassign(
            &self,
            _org: &str,
            _topic: &str,
            _partition: Option<u32>,
            _replicas: &[String],
        ) -> Result<u32, ReplError> {
            Ok(self.reassign_applied)
        }
        fn snapshot(&self, org: &str, topic: Option<&str>) -> bus::ReplicationSnapshot {
            if org == self.snapshot_org && topic == Some(self.snapshot_topic.as_str()) {
                self.snapshot.clone()
            } else {
                bus::ReplicationSnapshot::default()
            }
        }
    }

    /// Pure-function coverage for `map_repl_error` (PLAN-M2 §1e) — every
    /// `ReplError` variant must map to the stable `bus.<snake_case>` code
    /// the coordinator report promises (`bus.partition_unavailable`,
    /// `bus.not_enough_replicas`, `bus.not_a_replica`, `bus.not_leader`),
    /// as the FIRST token of `message`, same convention `map_bus_error`
    /// already uses for `BusServiceError`.
    #[test]
    fn map_repl_error_produces_stable_codes() {
        let cases: Vec<(ReplError, ProtocolErrorCode, &str)> = vec![
            (
                ReplError::NoAssignment {
                    topic: "t".to_string(),
                    partition: 0,
                },
                ProtocolErrorCode::NotAvailable,
                "bus.partition_unavailable:",
            ),
            (
                ReplError::NotEnoughReplicas {
                    topic: "t".to_string(),
                    partition: 0,
                    isr: 1,
                    required: 2,
                },
                ProtocolErrorCode::NotAvailable,
                "bus.not_enough_replicas:",
            ),
            (
                ReplError::NotAReplica {
                    topic: "t".to_string(),
                    partition: 0,
                    node_id: "n1".to_string(),
                },
                ProtocolErrorCode::BadRequest,
                "bus.not_a_replica:",
            ),
            (
                ReplError::EpochFenced {
                    have: 3,
                    requested: 2,
                },
                ProtocolErrorCode::Conflict,
                "bus.not_leader:",
            ),
            (
                ReplError::Internal("boom".to_string()),
                ProtocolErrorCode::Internal,
                "bus.replication_internal_error:",
            ),
        ];
        for (err, expected_code, expected_prefix) in cases {
            let mapped = map_repl_error(err);
            assert_eq!(mapped.code, expected_code, "code for {expected_prefix}");
            assert!(
                mapped.message.starts_with(expected_prefix),
                "message '{}' must start with '{expected_prefix}'",
                mapped.message
            );
        }
    }

    /// plan-app-platform §7 W7: the two-tier split this test used to
    /// verify (`bus_dispatch` UserSession-only vs. `bus_dispatch_admin`
    /// Admin-only, mirror-image routing tables) no longer exists —
    /// `bus_dispatch_admin` and its separate site-Admin session tier are
    /// deleted (§4.2). Every `BusPayload` variant, including the two admin
    /// mutations, now routes through this ONE `bus_dispatch` and is gated
    /// inside its own handler body (`gate_admin`). Rewritten (not deleted)
    /// to keep covering what is still a real regression risk: that
    /// `ReplicaListRequest`/`ReassignRequest`/`LeaderTransferRequest` are
    /// each wired to their correct handler in `bus_dispatch`'s match table
    /// — exercised through the real wire path (`MessageBody::BusBody` +
    /// `BusEnvelope`), which the direct `_v1` fn calls elsewhere in this
    /// module do not cover.
    #[tokio::test]
    async fn replica_variants_route_through_bus_dispatch() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-router-{}", uuid::Uuid::new_v4());
        let org_id = seed_membership(&db, &user_id, "org_admin");
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let ctx = handler_ctx(db.clone(), org.clone());
        let instance_id = fixture_instance_id().as_str().to_string();

        let envelope = |payload: BusPayload| {
            MessageBody::BusBody(tentaflow_protocol::BusEnvelope {
                instance_id: instance_id.clone(),
                payload,
            })
        };

        let replica_list_req = envelope(BusPayload::ReplicaListRequest { topic: None });
        let reassign_req = envelope(BusPayload::ReassignRequest {
            topic: "does.not.exist".to_string(),
            partition: None,
            replicas: vec!["n1".to_string()],
        });
        let transfer_req = envelope(BusPayload::LeaderTransferRequest {
            topic: "does.not.exist".to_string(),
            partition: 0,
            target_node_id: "n1".to_string(),
        });

        assert!(bus_dispatch(&replica_list_req, &ctx).await.is_ok());
        let err = bus_dispatch(&reassign_req, &ctx).await.unwrap_err();
        assert!(
            err.message.contains("bus.topic_not_found"),
            "must reach replica_reassign_v1 (gate_admin passes, a DOMAIN error follows): {}",
            err.message
        );
        let err = bus_dispatch(&transfer_req, &ctx).await.unwrap_err();
        assert!(
            err.message.contains("bus.topic_not_found"),
            "must reach leader_transfer_v1 (gate_admin passes, a DOMAIN error follows): {}",
            err.message
        );
    }

    /// Full lifecycle in ONE test, in a fixed order, deliberately: `bus::
    /// global()` is a process-wide singleton (`bus_fixture`'s own doc) and
    /// `BusService::set_replication` has no unset — installing a
    /// coordinator is a one-way door for the rest of this test binary's
    /// process. Every assertion that depends on NO coordinator being
    /// installed (the RF=1 fallback, `bus.replication_disabled`) MUST run
    /// before `svc.set_replication` is ever called, so it cannot race
    /// against a different test doing the same thing in the other order.
    #[tokio::test]
    async fn replica_dispatch_rf1_fallback_then_coordinator_path() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-repl-{}", uuid::Uuid::new_v4());
        let org_id = seed_membership(&db, &user_id, "org_admin");
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let ctx = handler_ctx(db.clone(), org.clone());
        let topic_name = format!("lab.{}", uuid::Uuid::new_v4().simple());

        topic_create_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            BusTopicOptionsWire {
                partitions: Some(3),
                ..Default::default()
            },
        )
        .await
        .expect("topic create");

        // ---- 1. No coordinator installed: honest RF=1 snapshot. ----
        let resp = replica_list_v1(
            &ctx,
            fixture_instance_id().as_str(),
            Some(topic_name.clone()),
        )
        .await
        .expect("replica list (rf1)");
        let (nodes, partitions, failovers) = match resp {
            BusPayload::ReplicaListResponse {
                nodes,
                partitions,
                failovers,
            } => (nodes, partitions, failovers),
            other => panic!("unexpected response: {other:?}"),
        };
        assert_eq!(nodes.len(), 1, "RF=1: this node only");
        assert!(nodes[0].is_local);
        assert_eq!(nodes[0].node_id, "test-node");
        assert_eq!(nodes[0].leader_count, 3);
        assert_eq!(nodes[0].isr_count, 3);
        assert_eq!(nodes[0].follower_count, 0);
        assert_eq!(partitions.len(), 3, "one row per partition");
        for (i, p) in partitions.iter().enumerate() {
            assert_eq!(p.partition, i as u32);
            assert_eq!(p.leader_node_id.as_deref(), Some("test-node"));
            assert_eq!(p.leader_epoch, 0);
            assert_eq!(p.replicas, vec!["test-node".to_string()]);
            assert_eq!(p.isr, vec!["test-node".to_string()]);
            assert!(p.lagging.is_empty());
            assert!(p.unavailable_reason.is_none());
        }
        assert!(
            failovers.is_empty(),
            "no bus.leader.failover audit rows exist for this fresh topic"
        );

        // ---- 2. No coordinator: admin mutations refuse cleanly. ----
        let err = match replica_reassign_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            None,
            vec!["n1".to_string()],
        )
        .await
        {
            Ok(_) => panic!("reassign must fail with no coordinator installed"),
            Err(e) => e,
        };
        assert!(err.message.starts_with("bus.replication_disabled:"));
        assert_eq!(err.code, ProtocolErrorCode::NotAvailable);

        let err = match leader_transfer_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            0,
            "n1".to_string(),
        )
        .await
        {
            Ok(_) => panic!("leader transfer must fail with no coordinator installed"),
            Err(e) => e,
        };
        assert!(err.message.starts_with("bus.replication_disabled:"));

        // ---- 3. Input validation, independent of coordinator presence. ----
        let err = replica_reassign_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            None,
            vec![],
        )
        .await
        .unwrap_err();
        assert!(err.message.starts_with("bus.invalid_argument:"));

        let err = leader_transfer_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            0,
            String::new(),
        )
        .await
        .unwrap_err();
        assert!(err.message.starts_with("bus.invalid_argument:"));

        let missing_topic = format!("lab.missing.{}", uuid::Uuid::new_v4().simple());
        let err = replica_reassign_v1(
            &ctx,
            fixture_instance_id().as_str(),
            missing_topic.clone(),
            None,
            vec!["n1".to_string()],
        )
        .await
        .unwrap_err();
        assert!(err.message.contains("bus.topic_not_found"));
        let err = leader_transfer_v1(
            &ctx,
            fixture_instance_id().as_str(),
            missing_topic,
            0,
            "n1".to_string(),
        )
        .await
        .unwrap_err();
        assert!(err.message.contains("bus.topic_not_found"));

        // ---- 4. Install the coordinator — every assertion from here on
        // depends on it, and nothing above this line may run again in this
        // process. ----
        let fake_snapshot = bus::ReplicationSnapshot {
            nodes: vec![bus::ReplicaNodeInfo {
                node_id: "gcm-core-01".to_string(),
                label: "gcm-core-01".to_string(),
                environment: tentaflow_protocol::NodeEnvironment::Prod,
                is_local: true,
                reachable: true,
                last_heartbeat_ms_ago: Some(50),
                leader_count: 3,
                follower_count: 0,
                isr_count: 3,
            }],
            partitions: vec![bus::PartitionReplicaInfo {
                topic: topic_name.clone(),
                partition: 0,
                leader_node_id: Some("gcm-core-01".to_string()),
                leader_epoch: 5,
                replicas: vec!["gcm-core-01".to_string(), "gczd-edge-02".to_string()],
                isr: vec!["gcm-core-01".to_string()],
                lagging: vec![bus::ReplicaLagInfo {
                    node_id: "gczd-edge-02".to_string(),
                    lag_bytes: 100,
                    lag_ms: 10,
                    reason: "lag 100 B > 64 MiB".to_string(),
                }],
                high_watermark: 42,
                log_end_offset: 44,
                unavailable_reason: None,
            }],
            failovers: vec![],
        };
        let coordinator = std::sync::Arc::new(FakeCoordinator {
            reassign_applied: 3,
            transfer_epoch: 7,
            snapshot_org: org_id.clone(),
            snapshot_topic: topic_name.clone(),
            snapshot: fake_snapshot,
        });
        let svc = bus::global().expect("bus service running");
        svc.set_replication(coordinator);

        let applied = match replica_reassign_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            Some(0),
            vec!["gcm-core-01".to_string()],
        )
        .await
        .expect("reassign via coordinator")
        {
            BusPayload::ReassignResponse { applied } => applied,
            other => panic!("unexpected response: {other:?}"),
        };
        assert_eq!(applied, 3, "must be the coordinator's own return value");

        let leader_epoch = match leader_transfer_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            0,
            "gczd-edge-02".to_string(),
        )
        .await
        .expect("transfer via coordinator")
        {
            BusPayload::LeaderTransferResponse { leader_epoch } => leader_epoch,
            other => panic!("unexpected response: {other:?}"),
        };
        assert_eq!(
            leader_epoch, 7,
            "must be the coordinator's own return value"
        );

        let (nodes, partitions) = match replica_list_v1(
            &ctx,
            fixture_instance_id().as_str(),
            Some(topic_name.clone()),
        )
        .await
        .expect("replica list via coordinator")
        {
            BusPayload::ReplicaListResponse {
                nodes, partitions, ..
            } => (nodes, partitions),
            other => panic!("unexpected response: {other:?}"),
        };
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0].node_id, "gcm-core-01");
        assert_eq!(nodes[0].environment, "prod");
        assert_eq!(partitions.len(), 1);
        assert_eq!(partitions[0].leader_epoch, 5);
        assert_eq!(partitions[0].high_watermark, 42);
        assert_eq!(partitions[0].lagging.len(), 1);
        assert_eq!(partitions[0].lagging[0].node_id, "gczd-edge-02");

        // A different (org, topic) the fake does not recognize falls back
        // to the coordinator's own EMPTY default, not RF=1 — confirms
        // `replica_list_v1` never re-derives an RF=1 answer once a
        // coordinator is installed, even for data it knows nothing about.
        let (other_nodes, other_partitions) =
            match replica_list_v1(&ctx, fixture_instance_id().as_str(), None)
                .await
                .expect("replica list, no topic filter")
            {
                BusPayload::ReplicaListResponse {
                    nodes, partitions, ..
                } => (nodes, partitions),
                other => panic!("unexpected response: {other:?}"),
            };
        assert!(other_nodes.is_empty());
        assert!(other_partitions.is_empty());
    }

    // ---- SUM/tentabus/POLITYKI-POL-FORMATY.md (F0): field policy CRUD ----

    /// plan-app-platform §4.3: `FieldPolicyListRequest` is `gate_read`, not
    /// `gate_admin` — "reading who may touch a topic is not itself
    /// privileged" (same rationale as `AclListRequest`). This test used to
    /// grant `bus.read` and assert DENIAL, i.e. it asserted the OLD,
    /// admin-only boundary; the plan moves that boundary down to `bus.
    /// read`, so the ASSERTION (not just the setup) changes to match — a
    /// caller with NO grant at all is denied, one with only `bus.read`
    /// (`field_policy_list_a_read_only_caller_is_allowed` below) succeeds.
    #[tokio::test]
    async fn field_policy_list_denied_without_bus_read_permission() {
        let (_guard, db) = bus_fixture();
        let user_id = "u-no-perm-field-policy".to_string();
        let org_id = seed_bus_permissions(&db, &user_id, &[]);
        let org = org_context(&org_id, &user_id, &[]);
        let ctx = handler_ctx(db, org);
        let err = field_policy_list_v1(
            &ctx,
            fixture_instance_id().as_str(),
            "patients.updated".to_string(),
        )
        .await
        .expect_err("must be denied without any bus grant");
        assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
    }

    #[tokio::test]
    async fn field_policy_list_a_read_only_caller_is_allowed() {
        let (_guard, db) = bus_fixture();
        let user_id = "u-read-only-field-policy".to_string();
        let org_id = seed_bus_permissions(&db, &user_id, &["bus.read"]);
        let org = org_context(&org_id, &user_id, &[]);
        let ctx = handler_ctx(db, org);
        field_policy_list_v1(
            &ctx,
            fixture_instance_id().as_str(),
            "patients.updated".to_string(),
        )
        .await
        .expect("bus.read alone must be enough to list field policies");
    }

    #[tokio::test]
    async fn field_policy_set_list_delete_round_trip() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-admin-{}", uuid::Uuid::new_v4());
        let org_id = seed_membership(&db, &user_id, "org_admin");
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let topic_name = format!("patients.{}", uuid::Uuid::new_v4().simple());
        let ctx = handler_ctx(db, org.clone());

        topic_create_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            BusTopicOptionsWire::default(),
        )
        .await
        .expect("topic create must succeed for an org_admin");

        let set = field_policy_set_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            "any".to_string(),
            "*".to_string(),
            "write".to_string(),
            vec!["patient_id".to_string(), "status".to_string()],
            vec!["patient_id".to_string()],
        )
        .await
        .expect("field policy set must succeed");
        assert!(matches!(set, BusPayload::FieldPolicySetResponse));

        let listed = field_policy_list_v1(&ctx, fixture_instance_id().as_str(), topic_name.clone())
            .await
            .expect("field policy list must succeed");
        let policies = match listed {
            BusPayload::FieldPolicyListResponse { policies } => policies,
            other => panic!("unexpected response: {other:?}"),
        };
        assert_eq!(policies.len(), 1);
        assert_eq!(policies[0].subject_type, "any");
        assert_eq!(policies[0].subject_id, "*");
        assert_eq!(policies[0].direction, "write");
        assert_eq!(
            policies[0].fields,
            vec!["patient_id".to_string(), "status".to_string()]
        );
        assert_eq!(policies[0].required_fields, vec!["patient_id".to_string()]);

        let deleted = field_policy_delete_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            "any".to_string(),
            "*".to_string(),
            "write".to_string(),
        )
        .await
        .expect("field policy delete must succeed");
        assert!(matches!(deleted, BusPayload::FieldPolicyDeleteResponse));

        let listed_after_delete =
            field_policy_list_v1(&ctx, fixture_instance_id().as_str(), topic_name.clone())
                .await
                .expect("field policy list must succeed");
        match listed_after_delete {
            BusPayload::FieldPolicyListResponse { policies } => {
                assert!(
                    policies.is_empty(),
                    "deleted policy must no longer be listed"
                );
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn field_policy_set_rejects_invalid_direction() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-admin-{}", uuid::Uuid::new_v4());
        let org_id = seed_membership(&db, &user_id, "org_admin");
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let topic_name = format!("patients.{}", uuid::Uuid::new_v4().simple());
        let ctx = handler_ctx(db, org.clone());

        topic_create_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            BusTopicOptionsWire::default(),
        )
        .await
        .expect("topic create must succeed for an org_admin");

        let err = field_policy_set_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            "any".to_string(),
            "*".to_string(),
            "sideways".to_string(),
            vec!["patient_id".to_string()],
            vec![],
        )
        .await
        .expect_err("invalid direction must be rejected");
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    }

    #[tokio::test]
    async fn field_policy_set_rejects_unknown_topic() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-admin-{}", uuid::Uuid::new_v4());
        let org_id = seed_membership(&db, &user_id, "org_admin");
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let ctx = handler_ctx(db, org.clone());

        let err = field_policy_set_v1(
            &ctx,
            fixture_instance_id().as_str(),
            "no.such.topic".to_string(),
            "any".to_string(),
            "*".to_string(),
            "write".to_string(),
            vec!["patient_id".to_string()],
            vec![],
        )
        .await
        .expect_err("field policy on a nonexistent topic must be rejected");
        assert_eq!(err.code, ProtocolErrorCode::NotFound);
    }

    // ---- SUM/tentabus/PLAN-F3.md (F3): schema registry CRUD ----

    /// plan-app-platform §4.3: `SchemaSubjectListRequest` is `gate_read`,
    /// not `gate_admin` — same rationale/shape as `field_policy_list_
    /// denied_without_bus_read_permission` above: the ASSERTION changes
    /// (not just the setup) because the plan moves the boundary itself, a
    /// caller with only `bus.read` now succeeds
    /// (`schema_subject_list_a_read_only_caller_is_allowed` below).
    #[tokio::test]
    async fn schema_subject_list_denied_without_bus_read_permission() {
        let (_guard, db) = bus_fixture();
        let user_id = "u-no-perm-schema-subject".to_string();
        let org_id = seed_bus_permissions(&db, &user_id, &[]);
        let org = org_context(&org_id, &user_id, &[]);
        let ctx = handler_ctx(db, org);
        let err = schema_subject_list_v1(&ctx, fixture_instance_id().as_str())
            .await
            .expect_err("must be denied without any bus grant");
        assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
    }

    #[tokio::test]
    async fn schema_subject_list_a_read_only_caller_is_allowed() {
        let (_guard, db) = bus_fixture();
        let user_id = "u-read-only-schema-subject".to_string();
        let org_id = seed_bus_permissions(&db, &user_id, &["bus.read"]);
        let org = org_context(&org_id, &user_id, &[]);
        let ctx = handler_ctx(db, org);
        schema_subject_list_v1(&ctx, fixture_instance_id().as_str())
            .await
            .expect("bus.read alone must be enough to list schema subjects");
    }

    #[tokio::test]
    async fn schema_register_get_delete_round_trip() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-admin-{}", uuid::Uuid::new_v4());
        let org_id = seed_membership(&db, &user_id, "org_admin");
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let subject = format!("orders.{}", uuid::Uuid::new_v4().simple());
        let ctx = handler_ctx(db, org.clone());
        let schema_text = r#"{"type":"object","properties":{"id":{"type":"string"}}}"#.to_string();

        let registered = schema_register_v1(
            &ctx,
            fixture_instance_id().as_str(),
            subject.clone(),
            "json_schema".to_string(),
            schema_text.clone(),
            None,
        )
        .await
        .expect("register must succeed");
        let (version, schema_ref_id, deduplicated) = match registered {
            BusPayload::SchemaRegisterResponse {
                version,
                schema_ref_id,
                deduplicated,
            } => (version, schema_ref_id, deduplicated),
            other => panic!("unexpected response: {other:?}"),
        };
        assert_eq!(version, 1);
        assert!(!deduplicated);
        assert_ne!(
            schema_ref_id, 0,
            "schema_ref_id must never be 0 (reserved = no schema)"
        );

        let got = schema_get_v1(&ctx, fixture_instance_id().as_str(), subject.clone(), None)
            .await
            .expect("get latest must succeed");
        match got {
            BusPayload::SchemaGetResponse {
                schema,
                schema_text: text,
            } => {
                assert_eq!(schema.version, 1);
                assert_eq!(text, schema_text);
            }
            other => panic!("unexpected response: {other:?}"),
        }

        let listed = schema_version_list_v1(&ctx, fixture_instance_id().as_str(), subject.clone())
            .await
            .expect("version list must succeed");
        match listed {
            BusPayload::SchemaVersionListResponse { versions } => {
                assert_eq!(versions.len(), 1);
            }
            other => panic!("unexpected response: {other:?}"),
        }

        let deleted = schema_delete_v1(
            &ctx,
            fixture_instance_id().as_str(),
            subject.clone(),
            None,
            false,
        )
        .await
        .expect("delete must succeed");
        match deleted {
            BusPayload::SchemaDeleteResponse { removed_versions } => {
                assert_eq!(removed_versions, vec![1]);
            }
            other => panic!("unexpected response: {other:?}"),
        }

        // Deleting the ONLY version removes the subject row itself, not
        // just the version — there is no "subject with zero versions"
        // state (`SubjectInfo::latest_version` is only ever `Some`).
        let err = schema_version_list_v1(&ctx, fixture_instance_id().as_str(), subject.clone())
            .await
            .expect_err("a fully deleted subject must no longer be listed");
        assert_eq!(err.code, ProtocolErrorCode::NotFound);
    }

    #[tokio::test]
    async fn schema_register_identical_text_is_deduplicated() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-admin-{}", uuid::Uuid::new_v4());
        let org_id = seed_membership(&db, &user_id, "org_admin");
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let subject = format!("orders.{}", uuid::Uuid::new_v4().simple());
        let ctx = handler_ctx(db, org.clone());
        let schema_text = r#"{"type":"object"}"#.to_string();

        schema_register_v1(
            &ctx,
            fixture_instance_id().as_str(),
            subject.clone(),
            "json_schema".to_string(),
            schema_text.clone(),
            None,
        )
        .await
        .expect("first register must succeed");

        let second = schema_register_v1(
            &ctx,
            fixture_instance_id().as_str(),
            subject.clone(),
            "json_schema".to_string(),
            schema_text,
            None,
        )
        .await
        .expect("second register with identical text must succeed");
        match second {
            BusPayload::SchemaRegisterResponse {
                version,
                deduplicated,
                ..
            } => {
                assert_eq!(version, 1, "identical content must not mint a new version");
                assert!(deduplicated);
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn schema_register_avro_with_non_none_compatibility_is_rejected() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-admin-{}", uuid::Uuid::new_v4());
        let org_id = seed_membership(&db, &user_id, "org_admin");
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let subject = format!("orders.{}", uuid::Uuid::new_v4().simple());
        let ctx = handler_ctx(db, org.clone());

        let err = schema_register_v1(
            &ctx,
            fixture_instance_id().as_str(),
            subject,
            "avro".to_string(),
            r#"{"type":"record","name":"x","fields":[]}"#.to_string(),
            Some("backward".to_string()),
        )
        .await
        .expect_err("avro with a non-none compatibility mode must be rejected in F3");
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
    }

    #[tokio::test]
    async fn schema_delete_is_rejected_while_a_topic_binds_the_subject() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-admin-{}", uuid::Uuid::new_v4());
        let org_id = seed_membership(&db, &user_id, "org_admin");
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let subject = format!("orders.{}", uuid::Uuid::new_v4().simple());
        let topic_name = format!("orders.evt.{}", uuid::Uuid::new_v4().simple());
        let ctx = handler_ctx(db, org.clone());

        schema_register_v1(
            &ctx,
            fixture_instance_id().as_str(),
            subject.clone(),
            "json_schema".to_string(),
            r#"{"type":"object"}"#.to_string(),
            None,
        )
        .await
        .expect("register must succeed");

        topic_create_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            BusTopicOptionsWire {
                schema_id: Some(subject.clone()),
                ..Default::default()
            },
        )
        .await
        .expect("topic bound to the registered subject must be created");

        let err = schema_delete_v1(
            &ctx,
            fixture_instance_id().as_str(),
            subject.clone(),
            None,
            false,
        )
        .await
        .expect_err("delete must be rejected while a topic still binds the subject");
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
        assert!(
            err.message.contains(&topic_name),
            "error must name the referencing topic: {}",
            err.message
        );

        topic_update_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            BusTopicOptionsWire {
                schema_id: Some(String::new()),
                ..Default::default()
            },
        )
        .await
        .expect("unbinding the schema from the topic must succeed");

        schema_delete_v1(
            &ctx,
            fixture_instance_id().as_str(),
            subject.clone(),
            None,
            false,
        )
        .await
        .expect("delete must succeed once no topic references the subject");
    }

    #[tokio::test]
    async fn schema_derived_get_returns_the_projection_for_a_stored_read_policy() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-admin-{}", uuid::Uuid::new_v4());
        let org_id = seed_membership(&db, &user_id, "org_admin");
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let subject = format!("patients.{}", uuid::Uuid::new_v4().simple());
        let topic_name = format!("patients.{}", uuid::Uuid::new_v4().simple());
        let ctx = handler_ctx(db, org.clone());

        topic_create_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            BusTopicOptionsWire::default(),
        )
        .await
        .expect("topic create must succeed for an org_admin");

        field_policy_set_v1(
            &ctx,
            fixture_instance_id().as_str(),
            topic_name.clone(),
            "any".to_string(),
            "*".to_string(),
            "read".to_string(),
            vec!["patient_id".to_string(), "status".to_string()],
            vec![],
        )
        .await
        .expect("field policy set must succeed");

        let schema_text = r#"{"type":"object","properties":{"patient_id":{"type":"string"},"status":{"type":"string"},"ssn":{"type":"string"}}}"#.to_string();
        schema_register_v1(
            &ctx,
            fixture_instance_id().as_str(),
            subject.clone(),
            "json_schema".to_string(),
            schema_text,
            None,
        )
        .await
        .expect("register must succeed");

        let derived = schema_derived_get_v1(
            &ctx,
            fixture_instance_id().as_str(),
            subject.clone(),
            None,
            topic_name.clone(),
            "any".to_string(),
            "*".to_string(),
            "read".to_string(),
        )
        .await
        .expect("derived get must succeed");
        let derived_text = match derived {
            BusPayload::SchemaDerivedGetResponse { schema_text } => schema_text,
            other => panic!("unexpected response: {other:?}"),
        };
        assert!(derived_text.contains("patient_id"));
        assert!(derived_text.contains("status"));
        assert!(
            !derived_text.contains("ssn"),
            "a field outside the read policy's allow-list must not survive derivation: {derived_text}"
        );
        assert!(
            derived_text.contains("\"additionalProperties\":false"),
            "derivation must force additionalProperties:false: {derived_text}"
        );
    }

    // =========================================================================
    // plan-app-platform §7 W7: the double-lock (`gate_admin`) and per-
    // instance isolation (`gate`) this rewrite introduced.
    // =========================================================================

    /// Half of the double lock is not the double lock: a matrix `bus.admin`
    /// grant WITHOUT the org Admin role must still be denied.
    #[tokio::test]
    async fn admin_variant_denied_without_org_admin_role_even_with_bus_admin_grant() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-matrix-only-{}", uuid::Uuid::new_v4());
        let org_id = seed_bus_permissions(&db, &user_id, &["bus.admin"]);
        // No "org.admin" among `perms` — the org-role half of the lock.
        let org = org_context(&org_id, &user_id, &[]);
        let ctx = handler_ctx(db, org);
        let err = topic_create_v1(
            &ctx,
            fixture_instance_id().as_str(),
            "orders.matrix-only".to_string(),
            BusTopicOptionsWire::default(),
        )
        .await
        .expect_err("bus.admin in the matrix alone must not be enough");
        assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
    }

    /// The other half: the org Admin role WITHOUT a matrix `bus.admin`
    /// grant must also be denied — the role cannot be delegated, but it
    /// alone is not authority over any one instance either.
    #[tokio::test]
    async fn admin_variant_denied_with_org_admin_role_but_no_bus_admin_grant() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-role-only-{}", uuid::Uuid::new_v4());
        // No matrix grant at all for this user on the fixture instance.
        let org_id = seed_bus_permissions(&db, &user_id, &[]);
        let org = org_context(&org_id, &user_id, &["org.admin"]);
        let ctx = handler_ctx(db, org);
        let err = topic_create_v1(
            &ctx,
            fixture_instance_id().as_str(),
            "orders.role-only".to_string(),
            BusTopicOptionsWire::default(),
        )
        .await
        .expect_err("org.admin alone, without the matrix grant, must not be enough");
        assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
    }

    /// A grant on one instance must not carry over to a second, unrelated
    /// instance of the same package — the whole point of naming the
    /// instance on the wire (§3.1) instead of resolving a process-global
    /// default. No live `bus::init_instance` for instance B: the denial
    /// this test exercises happens entirely inside `app_gate::
    /// require_instance_permission`, before `gate` ever tries to resolve a
    /// running engine, so a second live `BusService` is not needed —
    /// starting one would permanently break `bus::global()`'s "exactly one
    /// running instance" assumption for the rest of this test binary (see
    /// `bus_fixture`'s own doc).
    #[tokio::test]
    async fn read_variant_on_instance_b_denied_when_granted_only_on_a() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-cross-instance-{}", uuid::Uuid::new_v4());
        // Granted on the fixture's instance (A) only.
        let org_id = seed_bus_permissions(&db, &user_id, &["bus.read"]);
        let org = org_context(&org_id, &user_id, &[]);
        let ctx = handler_ctx(db.clone(), org);

        let state = test_state(&db);
        let instance_b = app_gate::test_support::install_app_instance(
            &state,
            bus::instance::BusInstanceId::PACKAGE_ID,
            "0000000b",
            &[],
        );

        let err = topic_list_v1(&ctx, &instance_b)
            .await
            .expect_err("bus.read on instance A must not carry over to instance B");
        assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);

        // Sanity: the SAME session still succeeds against the instance it
        // actually has the grant on.
        topic_list_v1(&ctx, fixture_instance_id().as_str())
            .await
            .expect("bus.read on instance A must still work");
    }

    /// A disabled instance rejects even a caller with a real matrix grant —
    /// `gate`'s instance-existence/enabled check runs before the
    /// permission check.
    #[tokio::test]
    async fn request_for_a_disabled_instance_is_app_unavailable() {
        let (_guard, db) = bus_fixture();
        let user_id = format!("u-disabled-{}", uuid::Uuid::new_v4());
        let org_id = seed_bus_permissions(&db, &user_id, &["bus.read"]);
        let org = org_context(&org_id, &user_id, &[]);
        let ctx = handler_ctx(db.clone(), org);

        crate::db::repository::set_addon_enabled(&db, fixture_instance_id().as_str(), false)
            .expect("disable the fixture instance");
        let err = topic_list_v1(&ctx, fixture_instance_id().as_str()).await;
        // Restore FIRST, before asserting — `fixture_instance_id()` is the
        // shared, process-wide `bus::global()` singleton's row; every OTHER
        // test in this module needs it ENABLED, and `bus_fixture`'s
        // `OnceLock` never re-inserts it once created.
        crate::db::repository::set_addon_enabled(&db, fixture_instance_id().as_str(), true)
            .expect("re-enable the fixture instance for the rest of the suite");
        let err = err.expect_err("a disabled instance must reject even a caller with a real grant");
        assert_eq!(err.code, ProtocolErrorCode::AppUnavailable);
    }

    /// Locates the next `fn <name>_v1(` (sync or async) definition at or
    /// after `search_from`, returning its name, its body (the balanced
    /// `{ ... }` after the signature), and the offset right after that
    /// body — or `None` once the source is exhausted. A hand-rolled scan
    /// (no regex) mirrors `ml_studio.rs::every_ml_handler_body_calls_a_
    /// matrix_gate`'s style: precise enough for this file's own syntax,
    /// without pulling a parser dependency into a test.
    fn scan_next_v1_handler(source: &str, search_from: usize) -> Option<(&str, &str, usize)> {
        let mut search_from = search_from;
        loop {
            let kw_start = search_from + source[search_from..].find("fn ")?;
            let name_start = kw_start + 3;
            let name_end = name_start
                + source[name_start..].find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))?;
            let name = &source[name_start..name_end];
            if name.ends_with("_v1") && source[name_end..].starts_with('(') {
                let body_start = name_end + source[name_end..].find('{')?;
                let bytes = source.as_bytes();
                let mut depth = 1i32;
                let mut i = body_start + 1;
                while depth > 0 {
                    match bytes[i] {
                        b'{' => depth += 1,
                        b'}' => depth -= 1,
                        _ => {}
                    }
                    i += 1;
                }
                return Some((name, &source[body_start..i], i));
            }
            search_from = name_end.max(kw_start + 3);
        }
    }

    /// plan-app-platform §4.3's enforcement test (modelled on `ml_studio.
    /// rs::every_ml_handler_body_calls_a_matrix_gate`): every `BusPayload::
    /// …Request` variant `bus_dispatch`'s match table routes to a `_v1`
    /// handler whose OWN body calls `gate_read`/`gate_write`/`gate_admin`
    /// — checked against the SOURCE, not a hand-maintained parallel table
    /// (the §4.3 table in the plan is documentation, not what this test
    /// reads), so a new variant/handler added later without a gate call
    /// fails this test instead of silently shipping ungated.
    #[test]
    fn every_bus_variant_arm_calls_a_gate() {
        let source = include_str!("bus.rs");

        // Part 1: every `_v1` fn definition's own body must contain at
        // least one of the three gate calls.
        let mut gated_handlers = std::collections::HashSet::new();
        let mut ungated_handlers = Vec::new();
        let mut pos = 0usize;
        let mut total = 0u32;
        while let Some((name, body, end)) = scan_next_v1_handler(source, pos) {
            total += 1;
            if ["gate_read(", "gate_write(", "gate_admin("]
                .iter()
                .any(|g| body.contains(g))
            {
                gated_handlers.insert(name.to_string());
            } else {
                ungated_handlers.push(name.to_string());
            }
            pos = end;
        }
        assert!(
            ungated_handlers.is_empty(),
            "_v1 handlers with no gate_read/gate_write/gate_admin call in their own body: \
             {ungated_handlers:?}"
        );
        assert!(
            total >= 30,
            "suspiciously few _v1 handlers found ({total}) — the scan itself may be broken"
        );

        // Part 2: every `_v1(` CALL inside `bus_dispatch`'s own match body
        // must name a handler Part 1 found gated — proves the match table
        // does not route a variant to something ungated or nonexistent.
        let dispatch_start = source
            .find("pub async fn bus_dispatch(")
            .expect("bus_dispatch must exist");
        let dispatch_rest = &source[dispatch_start..];
        let dispatch_body = &dispatch_rest[..dispatch_rest
            .find("\nmacro_rules! register_bus_variant")
            .expect("bus_dispatch must be followed by register_bus_variant!")];

        let mut called = std::collections::HashSet::new();
        let mut cur = 0usize;
        while let Some(rel) = dispatch_body[cur..].find("_v1(") {
            let call_end = cur + rel + "_v1(".len() - 1; // index of the '('
            let mut start = call_end;
            let bytes = dispatch_body.as_bytes();
            while start > 0
                && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_')
            {
                start -= 1;
            }
            called.insert(dispatch_body[start..call_end].to_string());
            cur = call_end + 1;
        }
        assert!(
            !called.is_empty(),
            "scan found no `_v1(` calls inside bus_dispatch — the scan itself may be broken"
        );
        for name in &called {
            assert!(
                gated_handlers.contains(name),
                "bus_dispatch calls '{name}', which either is not a _v1 handler this scan \
                 found, or has no gate call in its own body"
            );
        }
    }
}
