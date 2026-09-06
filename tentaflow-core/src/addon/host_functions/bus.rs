// ============ File: bus.rs — TentaBus host functions (M3b, PLAN §6.4) ============
//
// Five host functions bridging `crate::bus::BusService` to WASM:
//   - `bus_publish_v1`        — publish a batch of records to one topic
//   - `bus_consume_open_v1`   — open a consumer handle (group + topics)
//   - `bus_consume_next_v1`   — bounded-await poll for the next batch
//   - `bus_consume_commit_v1` — durably advance committed offsets
//   - `bus_consume_close_v1`  — drop consumer handle
//
// "nigdy per komunikat" (PLAN §6.4): the WASM boundary crossing cost
// (~1-5us) must be amortized over a BATCH, never paid per message. Publish
// always takes a batch of records in one call; consume is a handle+batch
// pattern (open once, drain repeated `next` batches) mirroring
// `stream_subscribe_v1`/`stream_next_v1`/`stream_close_v1` — see
// `streaming.rs`. The one deliberate divergence from that template: audit
// entries are written ONLY on `open`/`publish`/`close`/denial, NEVER per
// `next`/`commit` success — PLAN §8.2 forbids per-message audit logging for
// the bus, unlike camera frames (`stream_next_v1` audits every frame).
//
// Idle-handle reaper mirrors `llm.rs`'s stream reaper (a crashed addon must
// not leak an open `ConsumerHandle` — and the `bus_groups` DB row / read
// cursor it implies — forever), not `streaming.rs` (which has none, relying
// on explicit close/eviction only — acceptable for a camera subscription,
// not for a bus consumer that only a timer can safely reclaim).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use bytes::Bytes;

use tentaflow_sdk_spec::{
    BusConsumeCloseInput, BusConsumeCloseOutput, BusConsumeCommitInput, BusConsumeCommitOutput,
    BusConsumeNextInput, BusConsumeNextOutput, BusConsumeOpenInput, BusConsumeOpenOutput,
    BusHeader, BusOffsetEntry, BusPublishInput, BusPublishOutput, BusRecordOut,
};

use super::abi_helpers::PayloadKind;
use super::cbor_io::{read_input_cbor, write_cbor_capped};
use super::{audit_log_with_risk, check_permission, get_memory, AddonState, WasmCaller};
use crate::addon::errors::AbiError;
use crate::audit::RiskClass;
use crate::bus::groups::CommitMode;
use crate::bus::{
    self, BusCallContext, BusServiceError, ConsumerConfig, PublishBatch, PublishRecord,
    TopicPartition,
};
use crate::services::org::DEFAULT_ORG_ID;

// =============================================================================
// Permissions
// =============================================================================

const PERM_BUS_PUBLISH: &str = "bus.publish";
const PERM_BUS_SUBSCRIBE: &str = "bus.subscribe";

// =============================================================================
// Limits
// =============================================================================

/// PLAN §6.4: "do 1000 rekordow" per `bus_publish_v1` call.
const MAX_PUBLISH_RECORDS: usize = 1000;
/// `bus_consume_next_v1`'s `max_records` ceiling — same batch size PLAN's P12
/// gate is sized at.
const MAX_CONSUME_RECORDS: u32 = 1000;
/// Per-record byte estimate used only to size `ConsumerHandle::fetch`'s
/// byte-bounded budget from the addon's record-count request — PLAN's P12
/// gate is sized at "1000 rekordow x 1 KiB". `fetch` is byte-bounded, not
/// record-bounded, so the actual returned count can run over or under
/// `max_records`; callers must not assume an exact match.
const CONSUME_RECORD_BYTE_ESTIMATE: usize = 1024;
/// Mirrors `stream_next_v1`'s wait ceiling.
const MAX_CONSUME_WAIT_MS: u32 = 5_000;

/// Per-addon ceiling on simultaneous open consumers. Lower than
/// `streaming.rs`'s 16 camera streams — a bus consumer holds a durable
/// `bus_groups` row and offset cursor, a heavier resource than a broadcast
/// subscription.
const MAX_CONSUMERS_PER_ADDON: usize = 8;
/// Global ceiling across every addon.
const MAX_CONSUMERS_GLOBAL: usize = 128;
/// A consumer without a `next`/`commit` call for this long is force-closed —
/// a crashed or unloaded addon must not leak an open read cursor forever.
const CONSUMER_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
/// Background-sweeper interval reaping abandoned consumers.
const CONSUMER_SWEEP_INTERVAL: Duration = Duration::from_secs(30);

// =============================================================================
// Per-addon consumer registry
// =============================================================================

type ConsumerKey = (String, String);

struct ConsumerSlot {
    /// `next`/`commit` are sync from the host side (via `block_in_place` +
    /// `blocking_lock`), but mutate the handle's internal fetch cursor —
    /// concurrent calls on the same `consumer_id` must serialize.
    handle: Arc<tokio::sync::Mutex<bus::ConsumerHandle>>,
    /// The `BusInstanceId` this handle was opened against (`svc.instance_id()`
    /// at `bus_consume_open_v1` time). Review finding F3: `close_consumers_
    /// for_instance` filters the whole registry on this field to invalidate
    /// exactly one disabled instance's own consumers, never a different
    /// instance's, even for the same addon.
    instance_id: String,
    /// Snapshotted at `open` time — used to re-check `bus.subscribe` on
    /// every `next`/`commit` (fail-closed: a permission revoked mid-session
    /// must deny the very next call, PLAN §8.1-style).
    topics: Vec<String>,
    last_used: parking_lot::Mutex<Instant>,
    /// Reaper skips an in-flight (long-polling) consumer even if its
    /// `last_used` predates it — see `InUseGuard`.
    in_use: Arc<AtomicBool>,
}

/// Panic-safe RAII guard for `ConsumerSlot::in_use` — mirrors `llm.rs`'s
/// `InUseGuard`. Always restores `in_use=false` on drop, whether `next`/
/// `commit` returns Ok/Err or panics mid-call.
struct InUseGuard {
    flag: Arc<AtomicBool>,
}

impl InUseGuard {
    fn new(flag: Arc<AtomicBool>) -> Self {
        flag.store(true, Ordering::Release);
        Self { flag }
    }
}

impl Drop for InUseGuard {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

/// Whole registry under one lock, like `llm.rs`'s `LLM_STREAMS` — makes
/// reap+count+insert atomic so parallel `bus_consume_open_v1` calls cannot
/// all observe `<limit` and jointly insert `>limit`.
static CONSUMERS: OnceLock<parking_lot::Mutex<HashMap<ConsumerKey, ConsumerSlot>>> =
    OnceLock::new();
static SWEEPER_STARTED: std::sync::Once = std::sync::Once::new();

fn consumers() -> &'static parking_lot::Mutex<HashMap<ConsumerKey, ConsumerSlot>> {
    CONSUMERS.get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
}

fn reap_idle_locked(map: &mut HashMap<ConsumerKey, ConsumerSlot>) {
    let now = Instant::now();
    map.retain(|_, slot| {
        slot.in_use.load(Ordering::Acquire)
            || now.duration_since(*slot.last_used.lock()) < CONSUMER_IDLE_TIMEOUT
    });
}

fn ensure_sweeper_started() {
    SWEEPER_STARTED.call_once(|| {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async {
                let mut ticker = tokio::time::interval(CONSUMER_SWEEP_INTERVAL);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    ticker.tick().await;
                    reap_idle_locked(&mut consumers().lock());
                }
            });
        }
    });
}

/// Drops every open consumer belonging to `addon_id` — called from
/// `addon::mod::stop_addon` alongside `llm::cleanup_addon_streams`.
pub fn cleanup_addon_consumers(addon_id: &str) {
    consumers().lock().retain(|(aid, _), _| aid != addon_id);
}

/// Review finding F3 (closes the same reference-cycle hazard F1 fixes on
/// the replication side, reached here from the WASM host-function surface
/// instead): closes and invalidates every open consumer whose `bus_consume_
/// open_v1` resolved against `instance_id` — called from `bus::native::
/// native_on_disable` (via `bus::stop_instance`) so a disable does not have
/// to wait up to `CONSUMER_IDLE_TIMEOUT` (300s) for an addon-held
/// `ConsumerHandle`'s own `Arc<groups::GroupOffsetStore>` clone to age out
/// of this registry on its own. Left open, that clone is a second,
/// independent hold on the engine's `<data dir>/log/_meta` fjall advisory
/// lock that `BusService::shutdown()`'s own map-clearing (F1) cannot see or
/// release — the SAME reference-cycle shape F1 closes on the replication
/// side, just reached from a different holder.
///
/// A subsequent `bus_consume_next_v1`/`bus_consume_commit_v1` call against
/// any closed `consumer_id` observes `AbiError::NotFound` — plain removal
/// from `CONSUMERS` already produces exactly that (the same "unknown
/// consumer_id" shape `bus_consume_close_v1` returns today for a
/// never-registered or already-closed id), so no new error class is
/// introduced at the ABI boundary. An already-in-flight `next`/`commit`
/// call is unaffected: it resolved its own `Arc<Mutex<ConsumerHandle>>`
/// clone out of the registry, under the same lock, before this function
/// could ever observe its entry — only the NEXT call, after this function's
/// own retain commits, sees the removal.
pub fn close_consumers_for_instance(instance_id: &str) {
    consumers()
        .lock()
        .retain(|_, slot| slot.instance_id != instance_id);
}

/// Registers a freshly opened `ConsumerHandle`, enforcing the per-addon and
/// global quotas atomically (reap + count + insert under one lock).
fn register_consumer(
    addon_id: &str,
    instance_id: &str,
    topics: Vec<String>,
    handle: bus::ConsumerHandle,
) -> Result<String, AbiError> {
    ensure_sweeper_started();
    let mut map = consumers().lock();
    reap_idle_locked(&mut map);
    if map.len() >= MAX_CONSUMERS_GLOBAL {
        return Err(AbiError::QuotaExceeded);
    }
    let per_addon = map.keys().filter(|(aid, _)| aid == addon_id).count();
    if per_addon >= MAX_CONSUMERS_PER_ADDON {
        return Err(AbiError::QuotaExceeded);
    }
    let consumer_id = format!("busc_{}", uuid::Uuid::new_v4());
    map.insert(
        (addon_id.to_string(), consumer_id.clone()),
        ConsumerSlot {
            handle: Arc::new(tokio::sync::Mutex::new(handle)),
            instance_id: instance_id.to_string(),
            topics,
            last_used: parking_lot::Mutex::new(Instant::now()),
            in_use: Arc::new(AtomicBool::new(false)),
        },
    );
    Ok(consumer_id)
}

// =============================================================================
// Helpers
// =============================================================================

fn audit(
    state: &AddonState,
    action: &str,
    resource_id: Option<&str>,
    result: &str,
    reason: Option<&str>,
) {
    audit_log_with_risk(
        state,
        action,
        Some("bus"),
        resource_id,
        RiskClass::B,
        None,
        None,
        result,
        reason,
    );
}

/// Provenance for a bus call originating from an addon (PLAN §2.5 discipline
/// — the addon's own identity, never a fabricated one). Mirrors
/// `bus_publish` flow node's `call_context`. `instance_id` is the SAME
/// engine's own id (`svc.instance_id()`) the caller already resolved via
/// `resolve_bus_instance` — never re-derived independently, so
/// `check_instance` can never observe a mismatch here.
fn call_context(state: &AddonState, svc: &bus::BusService) -> BusCallContext {
    BusCallContext {
        instance_id: bus::instance::BusInstanceId::parse(svc.instance_id())
            .expect("BusService::instance_id() is always a valid BusInstanceId"),
        org_id: state
            .org_id
            .clone()
            .unwrap_or_else(|| DEFAULT_ORG_ID.to_string()),
        actor: Some(state.addon_id.clone()),
        correlation_id: None,
        origin: "addon".to_string(),
    }
}

/// Resolves the `BusService` an addon's `bus_publish_v1`/`bus_consume_open_v1`
/// call targets (plan-app-platform §3.4). This is the trust boundary that
/// keeps a request naming instance B from ever being served by instance A —
/// `bus::global()` cannot make that distinction (it answers `Some` iff
/// exactly one instance happens to be running, regardless of which).
///
/// - `Some(id)` — `BusInstanceId::parse` then `app_gate::instance_enabled`;
///   an unparsable or disabled/uninstalled id is refused.
/// - `None` — `app_gate::sole_enabled_instance`; refused if zero or more
///   than one instance is enabled (the addon must name one).
fn resolve_bus_instance(
    state: &AddonState,
    instance_id: Option<&str>,
) -> Result<Arc<bus::BusService>, AbiError> {
    let resolved = match instance_id {
        Some(raw) => {
            let id = bus::instance::BusInstanceId::parse(raw).map_err(|e| {
                tracing::warn!(instance_id = raw, error = %e, "bus: invalid instance_id");
                AbiError::Operation
            })?;
            if !crate::dispatch::app_gate::instance_enabled(
                &state.db,
                bus::instance::BusInstanceId::PACKAGE_ID,
                id.as_str(),
            ) {
                tracing::warn!(instance_id = %id, "bus: named instance not installed or not enabled");
                return Err(AbiError::Operation);
            }
            id
        }
        None => {
            match crate::dispatch::app_gate::sole_enabled_instance(
                &state.db,
                bus::instance::BusInstanceId::PACKAGE_ID,
            ) {
                Ok(addon_id) => bus::instance::BusInstanceId::parse(&addon_id).map_err(|e| {
                    tracing::error!(addon_id, error = %e, "bus: sole enabled instance id is malformed");
                    AbiError::Operation
                })?,
                Err(crate::dispatch::app_gate::SoleInstanceError::None)
                | Err(crate::dispatch::app_gate::SoleInstanceError::Disabled) => {
                    tracing::warn!("bus: no enabled TentaBus instance");
                    return Err(AbiError::Operation);
                }
                Err(crate::dispatch::app_gate::SoleInstanceError::Ambiguous(n)) => {
                    tracing::warn!(
                        count = n,
                        "bus: {n} enabled TentaBus instances — set instance_id"
                    );
                    return Err(AbiError::Operation);
                }
                Err(crate::dispatch::app_gate::SoleInstanceError::Lookup) => {
                    tracing::warn!("bus: instance lookup failed");
                    return Err(AbiError::Operation);
                }
            }
        }
    };
    match bus::instance(&resolved) {
        Some(svc) => Ok(svc),
        None => {
            tracing::warn!(instance_id = %resolved, "bus: enabled instance has no running engine");
            Err(AbiError::Operation)
        }
    }
}

fn map_bus_error(e: &BusServiceError) -> AbiError {
    match e {
        BusServiceError::TopicNotFound { .. } => AbiError::NotFound,
        BusServiceError::PermissionDenied { .. } => AbiError::Permission,
        BusServiceError::QuotaExceeded { .. }
        | BusServiceError::QuotaRequestTooLarge { .. }
        | BusServiceError::MaxTopicsExceeded { .. }
        | BusServiceError::MaxPartitionsExceeded { .. } => AbiError::QuotaExceeded,
        BusServiceError::Throttled { .. } => AbiError::Backpressure,
        BusServiceError::PayloadTooLarge { .. } => AbiError::PayloadTooLarge,
        // SUM/tentabus/POLITYKI-POL.md: a field policy blocked this
        // publish/consume — `GateNotSatisfied` is the existing ABI code for
        // "operation blocked by policy", the same category this is.
        BusServiceError::FieldNotAllowed { .. }
        | BusServiceError::RequiredFieldMissing { .. }
        | BusServiceError::FieldPolicyPayloadMalformed { .. } => AbiError::GateNotSatisfied,
        // SUM/tentabus/PLAN-F3.md: a bound schema subject/version vanished
        // out from under a topic.
        BusServiceError::SchemaNotFound { .. } | BusServiceError::SchemaVersionNotFound { .. } => {
            AbiError::NotFound
        }
        // A record failed schema validation and no per-record disposition
        // applied — same "operation blocked by policy" category as the
        // field-policy group above.
        BusServiceError::SchemaViolation { .. } => AbiError::GateNotSatisfied,
        // Registry-admin-shaped rejections (incompatible change, type with
        // no validator/derive support in this build, the ~1e-9
        // `schema_ref_id` collision) — state-conflict class, closest
        // existing code to "this request's inputs disagree with what is
        // already registered".
        BusServiceError::SchemaIncompatible { .. }
        | BusServiceError::SchemaTypeUnsupported { .. }
        | BusServiceError::SchemaRefIdCollision { .. } => AbiError::Conflict,
        _ => AbiError::Operation,
    }
}

fn consumer_id_valid(s: &str) -> bool {
    if let Some(rest) = s.strip_prefix("busc_") {
        rest.len() == 36
            && rest.chars().enumerate().all(|(i, c)| {
                let dash_pos = matches!(i, 8 | 13 | 18 | 23);
                if dash_pos {
                    c == '-'
                } else {
                    c.is_ascii_hexdigit() && !c.is_ascii_uppercase()
                }
            })
    } else {
        false
    }
}

// =============================================================================
// Host function: bus_publish_v1
// =============================================================================

pub fn bus_publish_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    let input: BusPublishInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::BusBatch,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "bus.publish",
                None,
                "error",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };
    let topic = input.topic.trim().to_string();
    if topic.is_empty() {
        audit(
            caller.data(),
            "bus.publish",
            None,
            "error",
            Some("empty_topic"),
        );
        return AbiError::Operation.as_i32();
    }
    if input.records.is_empty() {
        audit(
            caller.data(),
            "bus.publish",
            Some(&topic),
            "error",
            Some("empty_batch"),
        );
        return AbiError::Operation.as_i32();
    }
    if input.records.len() > MAX_PUBLISH_RECORDS {
        audit(
            caller.data(),
            "bus.publish",
            Some(&topic),
            "denied",
            Some("too_many_records"),
        );
        return AbiError::PayloadTooLarge.as_i32();
    }
    if !check_permission(caller.data(), PERM_BUS_PUBLISH, Some(&topic)) {
        audit(
            caller.data(),
            "bus.publish",
            Some(&topic),
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }

    let svc = match resolve_bus_instance(caller.data(), input.instance_id.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            audit(
                caller.data(),
                "bus.publish",
                Some(&topic),
                "error",
                Some("bus_instance_unresolved"),
            );
            return e.as_i32();
        }
    };
    let bctx = call_context(caller.data(), &svc);
    let create_if_missing = input.create_if_missing.unwrap_or(false);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let records: Vec<PublishRecord> = input
        .records
        .into_iter()
        .map(|r| PublishRecord {
            key: r.key.map(Bytes::from),
            headers: r
                .headers
                .into_iter()
                .map(|h| (h.name, Bytes::from(h.value)))
                .collect(),
            payload: Bytes::from(r.payload),
            timestamp_ms: now_ms,
            schema_id: 0,
        })
        .collect();
    let batch = PublishBatch {
        partition: None,
        producer: None,
        records,
    };

    // See `bus_publish.rs` flow node's own comment: `BusService::publish`
    // (and `create_topic`) block internally on the partition writer's
    // channel — every async caller MUST go through `block_in_place`
    // (equivalent to that node's `spawn_blocking`, but this call site is a
    // sync wasmtime host function, not an async fn, so there is no future
    // to `.await` — `block_in_place` alone suffices).
    let topic_for_task = topic.clone();
    let result = tokio::task::block_in_place(|| {
        let topic = topic_for_task;
        match svc.publish(&bctx, &topic, batch.clone()) {
            Ok(r) => Ok(r),
            Err(BusServiceError::TopicNotFound { .. }) if create_if_missing => {
                svc.create_topic(&bctx, &topic, bus::topics::TopicOptions::default())?;
                svc.publish(&bctx, &topic, batch)
            }
            Err(e) => Err(e),
        }
    });

    match result {
        Ok(r) => {
            audit(caller.data(), "bus.publish", Some(&topic), "ok", None);
            let out = BusPublishOutput {
                published: r.accepted,
                schema_rejected: r.schema_rejected,
            };
            write_cbor_capped(
                &memory,
                &mut caller,
                &out,
                out_ptr,
                out_cap,
                out_len_ptr,
                PayloadKind::BusBatch,
            )
        }
        Err(e) => {
            audit(
                caller.data(),
                "bus.publish",
                Some(&topic),
                "error",
                Some(&e.to_string()),
            );
            map_bus_error(&e).as_i32()
        }
    }
}

// =============================================================================
// Host function: bus_consume_open_v1
// =============================================================================

pub fn bus_consume_open_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    let input: BusConsumeOpenInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::ServiceCall,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "bus.consume.open",
                None,
                "error",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };
    if input.topics.is_empty() {
        audit(
            caller.data(),
            "bus.consume.open",
            None,
            "error",
            Some("empty_topics"),
        );
        return AbiError::Operation.as_i32();
    }
    let group = input.group.trim().to_string();
    if group.is_empty() {
        audit(
            caller.data(),
            "bus.consume.open",
            None,
            "error",
            Some("empty_group"),
        );
        return AbiError::Operation.as_i32();
    }
    let commit_mode = match input.commit_mode.as_deref() {
        Some("explicit") => CommitMode::Explicit,
        Some("at_most_once") => CommitMode::AtMostOnce,
        Some("auto_after_success") | None => CommitMode::AutoAfterSuccess,
        Some(_) => {
            audit(
                caller.data(),
                "bus.consume.open",
                Some(&group),
                "error",
                Some("unknown_commit_mode"),
            );
            return AbiError::Operation.as_i32();
        }
    };

    for topic in &input.topics {
        if !check_permission(caller.data(), PERM_BUS_SUBSCRIBE, Some(topic)) {
            audit(
                caller.data(),
                "bus.consume.open",
                Some(topic),
                "denied",
                Some("missing_permission"),
            );
            return AbiError::Permission.as_i32();
        }
    }

    let svc = match resolve_bus_instance(caller.data(), input.instance_id.as_deref()) {
        Ok(s) => s,
        Err(e) => {
            audit(
                caller.data(),
                "bus.consume.open",
                Some(&group),
                "error",
                Some("bus_instance_unresolved"),
            );
            return e.as_i32();
        }
    };
    let bctx = call_context(caller.data(), &svc);
    let addon_id = caller.data().addon_id.clone();
    let topics = input.topics.clone();

    let opened = tokio::task::block_in_place(|| {
        svc.open_consumer(&bctx, &group, &topics, ConsumerConfig { commit_mode })
    });
    match opened {
        Ok(handle) => match register_consumer(&addon_id, svc.instance_id(), topics, handle) {
            Ok(consumer_id) => {
                audit(caller.data(), "bus.consume.open", Some(&group), "ok", None);
                let out = BusConsumeOpenOutput { consumer_id };
                write_cbor_capped(
                    &memory,
                    &mut caller,
                    &out,
                    out_ptr,
                    out_cap,
                    out_len_ptr,
                    PayloadKind::ServiceCall,
                )
            }
            Err(e) => {
                audit(
                    caller.data(),
                    "bus.consume.open",
                    Some(&group),
                    "denied",
                    Some("consumers_quota"),
                );
                e.as_i32()
            }
        },
        Err(e) => {
            audit(
                caller.data(),
                "bus.consume.open",
                Some(&group),
                "error",
                Some(&e.to_string()),
            );
            map_bus_error(&e).as_i32()
        }
    }
}

// =============================================================================
// Host function: bus_consume_next_v1
// =============================================================================

pub fn bus_consume_next_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    let input: BusConsumeNextInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::ServiceCall,
    ) {
        Ok(v) => v,
        Err(e) => return e.as_i32(),
    };
    if !consumer_id_valid(&input.consumer_id) {
        return AbiError::NotFound.as_i32();
    }

    let addon_id = caller.data().addon_id.clone();
    let key = (addon_id, input.consumer_id.clone());
    let (handle_arc, topics, in_use_flag) = {
        let map = consumers().lock();
        match map.get(&key) {
            Some(slot) => {
                *slot.last_used.lock() = Instant::now();
                (
                    slot.handle.clone(),
                    slot.topics.clone(),
                    slot.in_use.clone(),
                )
            }
            None => return AbiError::NotFound.as_i32(),
        }
    };
    let _guard = InUseGuard::new(in_use_flag);

    // Fail-closed per-call re-check (PLAN §6.4's security gate: a revoked
    // `bus.subscribe` must deny "on every next", not just at `open`).
    for topic in &topics {
        if !check_permission(caller.data(), PERM_BUS_SUBSCRIBE, Some(topic)) {
            audit(
                caller.data(),
                "bus.consume.next",
                Some(&input.consumer_id),
                "denied",
                Some("missing_permission"),
            );
            return AbiError::Permission.as_i32();
        }
    }

    let max_records = input.max_records.clamp(1, MAX_CONSUME_RECORDS);
    let max_wait_ms = input.max_wait_ms.min(MAX_CONSUME_WAIT_MS);
    let max_bytes = (max_records as usize)
        .saturating_mul(CONSUME_RECORD_BYTE_ESTIMATE)
        .min(PayloadKind::BusBatch.max_bytes());

    let fetched = tokio::task::block_in_place(|| {
        let guard = handle_arc.blocking_lock();
        guard.fetch(max_bytes, max_wait_ms)
    });

    match fetched {
        Ok(batch) if batch.records.is_empty() => {
            let out = BusConsumeNextOutput::empty();
            write_cbor_capped(
                &memory,
                &mut caller,
                &out,
                out_ptr,
                out_cap,
                out_len_ptr,
                PayloadKind::BusBatch,
            )
        }
        Ok(batch) => {
            let records = batch
                .records
                .into_iter()
                .map(|r| BusRecordOut {
                    topic: r.topic,
                    partition: r.partition,
                    offset: r.offset,
                    timestamp_ms: r.timestamp_ms,
                    key: r.key.map(|b| b.to_vec()),
                    // `FetchedRecordMeta::headers` keeps keys as raw `Bytes`
                    // (never UTF-8-validated on the fetch hot path, see its
                    // own doc) — decoded here, once per record returned to
                    // an addon, not on the engine's internal fetch path.
                    headers: r
                        .headers
                        .into_iter()
                        .map(|(name, value)| BusHeader {
                            name: String::from_utf8_lossy(&name).into_owned(),
                            value: value.to_vec(),
                        })
                        .collect(),
                    payload: r.payload.to_vec(),
                })
                .collect();
            let out = BusConsumeNextOutput::batch(records);
            write_cbor_capped(
                &memory,
                &mut caller,
                &out,
                out_ptr,
                out_cap,
                out_len_ptr,
                PayloadKind::BusBatch,
            )
        }
        // Not audited — see the file-level doc: a fetch error is not a
        // permission denial, and auditing it would put an audit-log write
        // back on the per-poll hot path the "never per message" rule exists
        // to keep clear.
        Err(e) => map_bus_error(&e).as_i32(),
    }
}

// =============================================================================
// Host function: bus_consume_commit_v1
// =============================================================================

pub fn bus_consume_commit_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    let input: BusConsumeCommitInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::ServiceCall,
    ) {
        Ok(v) => v,
        Err(e) => return e.as_i32(),
    };
    if !consumer_id_valid(&input.consumer_id) {
        return AbiError::NotFound.as_i32();
    }
    if input.offsets.is_empty() {
        return AbiError::Operation.as_i32();
    }

    let addon_id = caller.data().addon_id.clone();
    let key = (addon_id, input.consumer_id.clone());
    let (handle_arc, topics, in_use_flag) = {
        let map = consumers().lock();
        match map.get(&key) {
            Some(slot) => {
                *slot.last_used.lock() = Instant::now();
                (
                    slot.handle.clone(),
                    slot.topics.clone(),
                    slot.in_use.clone(),
                )
            }
            None => return AbiError::NotFound.as_i32(),
        }
    };
    let _guard = InUseGuard::new(in_use_flag);

    for topic in &topics {
        if !check_permission(caller.data(), PERM_BUS_SUBSCRIBE, Some(topic)) {
            audit(
                caller.data(),
                "bus.consume.commit",
                Some(&input.consumer_id),
                "denied",
                Some("missing_permission"),
            );
            return AbiError::Permission.as_i32();
        }
    }

    let offsets: Vec<(TopicPartition, u64)> = input
        .offsets
        .iter()
        .map(|o: &BusOffsetEntry| {
            (
                TopicPartition {
                    topic: o.topic.clone(),
                    partition: o.partition,
                },
                o.offset,
            )
        })
        .collect();

    let committed = tokio::task::block_in_place(|| {
        let guard = handle_arc.blocking_lock();
        guard.commit(&offsets)
    });

    match committed {
        // Not audited on success — per-batch commit sits on the same
        // "never per message" hot path as `next`.
        Ok(()) => {
            let out = BusConsumeCommitOutput { committed: true };
            write_cbor_capped(
                &memory,
                &mut caller,
                &out,
                out_ptr,
                out_cap,
                out_len_ptr,
                PayloadKind::ServiceCall,
            )
        }
        Err(e) => map_bus_error(&e).as_i32(),
    }
}

// =============================================================================
// Host function: bus_consume_close_v1
// =============================================================================

pub fn bus_consume_close_v1(
    mut caller: WasmCaller<'_, AddonState>,
    input_ptr: i32,
    input_len: i32,
    out_ptr: i32,
    out_cap: i32,
    out_len_ptr: i32,
) -> i32 {
    let memory = match get_memory(&mut caller) {
        Some(m) => m,
        None => return AbiError::Operation.as_i32(),
    };
    let input: BusConsumeCloseInput = match read_input_cbor(
        &memory,
        &caller,
        input_ptr,
        input_len,
        PayloadKind::ServiceCall,
    ) {
        Ok(v) => v,
        Err(e) => {
            audit(
                caller.data(),
                "bus.consume.close",
                None,
                "error",
                Some(if e == AbiError::PayloadTooLarge {
                    "payload_too_large"
                } else {
                    "invalid_payload"
                }),
            );
            return e.as_i32();
        }
    };
    if !check_permission(caller.data(), PERM_BUS_SUBSCRIBE, None) {
        audit(
            caller.data(),
            "bus.consume.close",
            None,
            "denied",
            Some("missing_permission"),
        );
        return AbiError::Permission.as_i32();
    }
    if !consumer_id_valid(&input.consumer_id) {
        audit(
            caller.data(),
            "bus.consume.close",
            None,
            "denied",
            Some("consumer_id_invalid"),
        );
        return AbiError::Operation.as_i32();
    }

    let addon_id = caller.data().addon_id.clone();
    let key = (addon_id, input.consumer_id.clone());
    if consumers().lock().remove(&key).is_some() {
        audit(
            caller.data(),
            "bus.consume.close",
            Some(&input.consumer_id),
            "ok",
            None,
        );
        let out = BusConsumeCloseOutput { closed: true };
        return write_cbor_capped(
            &memory,
            &mut caller,
            &out,
            out_ptr,
            out_cap,
            out_len_ptr,
            PayloadKind::ServiceCall,
        );
    }
    audit(
        caller.data(),
        "bus.consume.close",
        Some(&input.consumer_id),
        "denied",
        Some("consumer_not_found"),
    );
    AbiError::NotFound.as_i32()
}

// =============================================================================
// Test helpers (hidden) — let integration tests poke the registry
// =============================================================================

#[doc(hidden)]
pub mod test_api {
    use super::*;

    #[doc(hidden)]
    pub fn registry_len() -> usize {
        consumers().lock().len()
    }

    #[doc(hidden)]
    pub fn registry_contains(addon_id: &str, consumer_id: &str) -> bool {
        consumers()
            .lock()
            .contains_key(&(addon_id.to_string(), consumer_id.to_string()))
    }

    #[doc(hidden)]
    pub fn registry_clear() {
        consumers().lock().clear();
    }

    /// Review finding F3 test hook: registers a real, already-open
    /// `bus::ConsumerHandle` directly, bypassing the WASM `Caller`/CBOR
    /// plumbing `bus_consume_open_v1` needs — lets a test drive `close_
    /// consumers_for_instance` against a real handle without standing up a
    /// wasmtime instance.
    #[doc(hidden)]
    pub fn register_for_test(
        addon_id: &str,
        instance_id: &str,
        topics: Vec<String>,
        handle: bus::ConsumerHandle,
    ) -> Result<String, AbiError> {
        super::register_consumer(addon_id, instance_id, topics, handle)
    }

    #[doc(hidden)]
    pub fn max_consumers_per_addon() -> usize {
        super::MAX_CONSUMERS_PER_ADDON
    }

    #[doc(hidden)]
    pub fn max_consumers_global() -> usize {
        super::MAX_CONSUMERS_GLOBAL
    }

    #[doc(hidden)]
    pub fn consumer_id_valid_for_test(s: &str) -> bool {
        super::consumer_id_valid(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbPool;

    struct AllowAllAuthorizer;
    impl bus::BusAuthorizer for AllowAllAuthorizer {
        fn authorize(
            &self,
            _ctx: &BusCallContext,
            _action: bus::BusAction,
            _topic: &str,
        ) -> Result<(), BusServiceError> {
            Ok(())
        }
        fn authorize_group(
            &self,
            _ctx: &BusCallContext,
            _action: bus::BusAction,
            _topic: &str,
            _group: &str,
        ) -> Result<(), BusServiceError> {
            Ok(())
        }
        fn generation(&self) -> u64 {
            0
        }
    }

    fn test_local_db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory local db");
        crate::bus::db::migrate(&conn).expect("migrate local db");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    /// A real, running `BusService` for `instance_id_str` in its own
    /// `tempfile::TempDir` (kept alive by the returned guard) — F3's test
    /// needs a genuine `bus::ConsumerHandle`, not a fake, since the whole
    /// point of `close_consumers_for_instance` is what it does to the real
    /// registry entry wrapping one. Mirrors `bus::mod::tests::test_service`'s
    /// own fixture shape.
    fn test_bus_service(instance_id_str: &str) -> (tempfile::TempDir, bus::BusService) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let instance_id =
            bus::instance::BusInstanceId::parse(instance_id_str).expect("valid test instance id");
        let main_db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        crate::db::repository::bus_test_support::create_bus_tables(&main_db)
            .expect("bus fixture tables");
        let svc = bus::BusService::new(bus::BusInitConfig {
            instance_id,
            bus_dir: tmp.path().join("log"),
            db: main_db,
            local_db: test_local_db(),
            authorizer: Arc::new(AllowAllAuthorizer),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: bus::DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .expect("bus service");
        (tmp, svc)
    }

    fn open_test_consumer(svc: &bus::BusService, topic: &str, group: &str) -> bus::ConsumerHandle {
        let ctx = BusCallContext {
            instance_id: bus::instance::BusInstanceId::parse(svc.instance_id())
                .expect("valid instance id"),
            org_id: DEFAULT_ORG_ID.to_string(),
            actor: Some("test-actor".to_string()),
            correlation_id: None,
            origin: "test".to_string(),
        };
        svc.create_topic(&ctx, topic, bus::topics::TopicOptions::default())
            .expect("create topic");
        svc.open_consumer(
            &ctx,
            group,
            &[topic.to_string()],
            ConsumerConfig {
                commit_mode: CommitMode::Explicit,
            },
        )
        .expect("open consumer")
    }

    /// Review finding F3: `close_consumers_for_instance` must filter on the
    /// `instance_id` recorded at `open` time, not just on the addon — a
    /// consumer this addon opened against a DIFFERENT still-enabled
    /// instance must survive another instance's disable untouched.
    #[test]
    fn close_consumers_for_instance_only_removes_that_instances_own_entries() {
        test_api::registry_clear();
        let (_tmp_a, svc_a) = test_bus_service("tentabus-f3000001");
        let (_tmp_b, svc_b) = test_bus_service("tentabus-f3000002");

        let handle_a = open_test_consumer(&svc_a, "test-topic", "test-group");
        let handle_b = open_test_consumer(&svc_b, "test-topic", "test-group");

        let addon_id = "addon-f3-test";
        let id_a = test_api::register_for_test(
            addon_id,
            svc_a.instance_id(),
            vec!["test-topic".into()],
            handle_a,
        )
        .expect("register a");
        let id_b = test_api::register_for_test(
            addon_id,
            svc_b.instance_id(),
            vec!["test-topic".into()],
            handle_b,
        )
        .expect("register b");

        assert!(test_api::registry_contains(addon_id, &id_a));
        assert!(test_api::registry_contains(addon_id, &id_b));

        close_consumers_for_instance(svc_a.instance_id());

        assert!(
            !test_api::registry_contains(addon_id, &id_a),
            "disabled instance's own consumer must be closed"
        );
        assert!(
            test_api::registry_contains(addon_id, &id_b),
            "a different, still-enabled instance's consumer must survive"
        );

        test_api::registry_clear();
    }

    #[test]
    fn consumer_id_format_validator() {
        let s = format!("busc_{}", uuid::Uuid::new_v4());
        assert!(consumer_id_valid(&s));
        assert!(!consumer_id_valid("busc_"));
        assert!(!consumer_id_valid("xxx"));
        let bad = "busc_AAAAAAAA-AAAA-AAAA-AAAA-AAAAAAAAAAAA";
        assert!(!consumer_id_valid(bad));
    }

    #[test]
    fn map_bus_error_permission_and_not_found() {
        assert_eq!(
            map_bus_error(&BusServiceError::TopicNotFound { name: "t".into() }),
            AbiError::NotFound
        );
        assert_eq!(
            map_bus_error(&BusServiceError::PermissionDenied {
                action: "produce",
                topic: "t".into()
            }),
            AbiError::Permission
        );
        assert_eq!(
            map_bus_error(&BusServiceError::Throttled { retry_after_ms: 10 }),
            AbiError::Backpressure
        );
    }

    // =========================================================================
    // `resolve_bus_instance` tests (plan-app-platform §3.4 / W8) — the
    // resolution rules that close the last `bus::global()` call sites in this
    // file. `bus::global()` answers `Some` iff exactly one instance happens
    // to be running, regardless of which one an addon actually named — these
    // tests prove the replacement never has that property.
    // =========================================================================

    /// `resolve_bus_instance`'s `Ok` type (`Arc<bus::BusService>`) is not
    /// `Debug` (the engine holds non-`Debug` internals), so plain
    /// `.unwrap_err()` does not compile — this is the same "expect an Err,
    /// panic with the Ok value's TYPE not its Debug repr" shape.
    fn expect_resolve_err(result: Result<Arc<bus::BusService>, AbiError>) -> AbiError {
        match result {
            Err(e) => e,
            Ok(_) => panic!("expected resolve_bus_instance to fail, got Ok"),
        }
    }

    /// Minimal `AddonState` sharing `db` with an `AppState` test fixture, so
    /// `resolve_bus_instance` (which only touches `state.db`) can be driven
    /// directly without standing up a wasmtime `Caller`. Mirrors `storage.rs`
    /// `tests::make_state`, but takes the db instead of building its own —
    /// the app-gate rows this module's tests seed live on a specific
    /// `AppState::for_test()` db, not an ad hoc one.
    fn make_addon_state(db: DbPool) -> AddonState {
        AddonState {
            addon_id: "bus-w8-test-addon".to_string(),
            instance_id: "t".to_string(),
            user_id: None,
            org_id: None,
            db: db.clone(),
            permissions: Vec::new(),
            event_bus: Arc::new(crate::addon::event_bus::EventBus::new()),
            permission_checker: Arc::new(crate::addon::permissions::PermissionChecker::new(db)),
            fuel_consumed: 0,
            is_system_call: true,
            call_provenance: crate::addon::AddonCallProvenance::addon(),
            rate_limiter: None,
            net_manager: Arc::new(parking_lot::Mutex::new(
                crate::addon::host_functions::network::NetworkConnectionManager::new(),
            )),
            settings_cipher: Arc::new(crate::crypto::SettingsCipher::new(&[0u8; 32])),
            manifest: Arc::new(crate::addon::AddonManifest::default()),
            memory_limit: 64 * 1024 * 1024,
            router: None,
            oauth_refresh_guard: Arc::new(
                crate::addon::oauth_refresh_guard::OAuthRefreshGuard::new(),
            ),
            ui_panels: None,
            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            wasi: wasmtime_wasi::WasiCtxBuilder::new().build_p1(),
        }
    }

    /// A real, running `BusService` registered in the process-global registry
    /// (`bus::init_instance`, NOT the bare `BusService::new` `test_bus_service`
    /// above uses) — `resolve_bus_instance`'s last step is `bus::instance(&id)`,
    /// a registry lookup, so a `resolve_bus_instance` test needs an instance
    /// the registry actually knows about. Caller must `bus::stop_instance(&id)`
    /// when done — `BUS_INSTANCES` is a process-global `static` shared with
    /// every other `#[cfg(test)]` module in this crate's `--lib` binary, same
    /// caveat as `bus::mod::tests`'s own registry tests.
    fn init_test_bus_instance(
        id_str: &str,
    ) -> (
        tempfile::TempDir,
        bus::instance::BusInstanceId,
        Arc<bus::BusService>,
    ) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let instance_id =
            bus::instance::BusInstanceId::parse(id_str).expect("valid test instance id");
        let main_db = crate::db::init(std::path::Path::new(":memory:")).expect("test db");
        crate::db::repository::bus_test_support::create_bus_tables(&main_db)
            .expect("bus fixture tables");
        let svc = bus::init_instance(bus::BusInitConfig {
            instance_id: instance_id.clone(),
            bus_dir: tmp.path().join("log"),
            db: main_db,
            local_db: test_local_db(),
            authorizer: Arc::new(AllowAllAuthorizer),
            retention_interval: None,
            dedup_expected_rate_per_sec: 10_000,
            partition_handle_lru: None,
            publish_ack_timeout: bus::DEFAULT_PUBLISH_ACK_TIMEOUT,
        })
        .expect("bus init_instance");
        (tmp, instance_id, svc)
    }

    #[test]
    fn resolve_bus_instance_errors_when_none_installed() {
        let app_state = crate::dispatch::state::AppState::for_test();
        let addon_state = make_addon_state(app_state.db.clone());

        let err = expect_resolve_err(resolve_bus_instance(&addon_state, None));
        assert_eq!(err, AbiError::Operation);
    }

    #[test]
    fn resolve_bus_instance_ambiguous_when_more_than_one_enabled() {
        let app_state = crate::dispatch::state::AppState::for_test();
        crate::dispatch::app_gate::test_support::install_app_instance(
            &app_state,
            "tentabus",
            "a0000010",
            &[],
        );
        crate::dispatch::app_gate::test_support::install_app_instance(
            &app_state,
            "tentabus",
            "a0000011",
            &[],
        );
        let addon_state = make_addon_state(app_state.db.clone());

        let err = expect_resolve_err(resolve_bus_instance(&addon_state, None));
        assert_eq!(
            err,
            AbiError::Operation,
            "more than one enabled instance and no instance_id named must refuse, not guess"
        );
    }

    #[test]
    fn resolve_bus_instance_refuses_a_named_disabled_instance() {
        let app_state = crate::dispatch::state::AppState::for_test();
        let addon_id = crate::dispatch::app_gate::test_support::install_app_instance(
            &app_state,
            "tentabus",
            "a0000012",
            &[],
        );
        crate::db::repository::set_addon_enabled(&app_state.db, &addon_id, false)
            .expect("disable instance");
        let addon_state = make_addon_state(app_state.db.clone());

        let err = expect_resolve_err(resolve_bus_instance(&addon_state, Some(&addon_id)));
        assert_eq!(
            err,
            AbiError::Operation,
            "an addon naming a disabled instance must be refused, never silently rerouted"
        );
    }

    #[test]
    fn resolve_bus_instance_refuses_an_unparsable_instance_id() {
        let app_state = crate::dispatch::state::AppState::for_test();
        let addon_state = make_addon_state(app_state.db.clone());

        let err = expect_resolve_err(resolve_bus_instance(&addon_state, Some("not-a-valid-id")));
        assert_eq!(err, AbiError::Operation);
    }

    /// The owner's HARD requirement, at the resolution layer: with A and B
    /// both enabled and running, an addon that names B must resolve to B's
    /// own engine — never A's — and B's own topic (freshly created, empty)
    /// must not see a single record of the batch published into A's
    /// same-named topic. `bus::global()` cannot make this distinction at
    /// all; this is the property that replaces it.
    #[test]
    fn resolve_bus_instance_named_b_never_reaches_instance_as_data() {
        let app_state = crate::dispatch::state::AppState::for_test();
        let addon_a = crate::dispatch::app_gate::test_support::install_app_instance(
            &app_state,
            "tentabus",
            "a0000013",
            &[],
        );
        let addon_b = crate::dispatch::app_gate::test_support::install_app_instance(
            &app_state,
            "tentabus",
            "a0000014",
            &[],
        );

        let (_tmp_a, id_a, svc_a) = init_test_bus_instance(&addon_a);
        let (_tmp_b, id_b, svc_b) = init_test_bus_instance(&addon_b);

        let ctx_b = BusCallContext {
            instance_id: id_b.clone(),
            org_id: DEFAULT_ORG_ID.to_string(),
            actor: Some("test-actor".to_string()),
            correlation_id: None,
            origin: "test".to_string(),
        };

        let ctx_a = BusCallContext {
            instance_id: id_a.clone(),
            org_id: DEFAULT_ORG_ID.to_string(),
            actor: Some("test-actor".to_string()),
            correlation_id: None,
            origin: "test".to_string(),
        };
        svc_a
            .create_topic(&ctx_a, "orders", bus::topics::TopicOptions::default())
            .expect("create topic on A");
        // Publishing is deliberately TOLERANT of `NotLeader`. A fresh
        // `init_instance` installs no replication coordinator, so
        // `check_leader_role` is a no-op — but W6's boot-time re-arm
        // (`bus::native::start_replication_for_already_enabled_instances`)
        // iterates EVERY entry of the process-wide `BUS_INSTANCES` registry,
        // so when `bus::native`'s own tests call it from this same test binary
        // they install a coordinator on THIS test's engines too, and publish
        // then correctly refuses for a partition no assignment covers. That is
        // right in production — at boot there is exactly one owner of the
        // process — and mere cross-talk here. The isolation claim below does
        // not depend on the publish landing, and a test that fails only in the
        // full-suite run while passing in every filtered one is worse than
        // useless.
        let published = svc_a
            .publish(
                &ctx_a,
                "orders",
                PublishBatch {
                    partition: None,
                    producer: None,
                    records: vec![PublishRecord {
                        key: None,
                        headers: Vec::new(),
                        payload: Bytes::from_static(b"instance-a-only"),
                        timestamp_ms: 0,
                        schema_id: 0,
                    }],
                },
            )
            .is_ok();

        let addon_state = make_addon_state(app_state.db.clone());
        let resolved = resolve_bus_instance(&addon_state, Some(id_b.as_str()))
            .expect("naming B must resolve, both instances are enabled and running");
        assert_eq!(
            resolved.instance_id(),
            id_b.as_str(),
            "naming B must resolve to B's own engine"
        );
        assert!(
            !Arc::ptr_eq(&resolved, &svc_a),
            "naming B must never resolve to A's engine"
        );
        assert!(Arc::ptr_eq(&resolved, &svc_b));

        // Topic-level isolation, and it needs no leadership: "orders" exists
        // on A. If the two instances shared topic storage this would fail as a
        // duplicate, so its SUCCESS is the isolation proof.
        resolved
            .create_topic(&ctx_b, "orders", bus::topics::TopicOptions::default())
            .expect("instance B must not already carry instance A's topic");

        // Record-level isolation, asserted only when the publish above landed
        // and a consumer can actually be opened (both are leadership-gated —
        // see the publish comment).
        if published {
            if let Ok(handle_b) = resolved.open_consumer(
                &ctx_b,
                "isolation-check",
                &["orders".to_string()],
                ConsumerConfig {
                    commit_mode: CommitMode::Explicit,
                },
            ) {
                if let Ok(fetched) = handle_b.fetch(64 * 1024, 20) {
                    assert!(
                        fetched.records.is_empty(),
                        "instance B must never see instance A's data"
                    );
                }
            }
        }

        bus::stop_instance(&id_a);
        bus::stop_instance(&id_b);
    }
}
