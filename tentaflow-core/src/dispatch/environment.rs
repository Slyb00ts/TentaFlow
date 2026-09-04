// =============================================================================
// File: dispatch/environment.rs — node environment identity + manual
//       config-bundle pull (ROADMAP Z12)
// =============================================================================
//
// Two families of operation behind `MessageBody::EnvironmentPromotionBody`:
//
//   1. The node's OWN environment identity (`GetKind`/`SetKind`,
//      `SetStrictIsolation`) — `SetKind` toward Prod is fail-closed on
//      `confirm_environment_name == "PROD"`, validated HERE, not merely by a
//      disabled button in the UI (ZADANIA.md Z12 pitfall #8).
//   2. The manual config-bundle pull wizard (`ExportBundle`/
//      `ImportFromFile`/`PullDonorList`/`PullStart`/`PullStatus`/
//      `ImportPreviewDiff`/`ImportApply`), modeled on `mesh-baseline-adopt.js`
//      (`PHASE_ORDER` Elected -> Receiving -> Importing -> Imported ->
//      Completed). `ImportApply` toward a HIGHER-ranked environment (in
//      particular Prod) is fail-closed on `confirm_environment_name`
//      matching the target environment's name, independently of `SetKind`'s
//      own gate (pitfall #6) — two separate fields on two separate requests,
//      never one reused for both.
//
// Pending pulls are held in-process (a fetched-but-not-yet-applied bundle is
// not worth persisting across a restart) keyed by a random `pull_id`.
// =============================================================================

use std::sync::OnceLock;

use dashmap::DashMap;
use serde_json::json;
use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::environment::{
    EnvironmentDiffEntry, EnvironmentExportBundleResponse, EnvironmentGetKindResponse,
    EnvironmentImportApplyResponse, EnvironmentImportPreviewDiffResponse,
    EnvironmentPromotionPayload, EnvironmentPullDonorInfo, EnvironmentPullDonorListResponse,
    EnvironmentPullStartResponse, EnvironmentPullStatusResponse, EnvironmentSetKindResponse,
    EnvironmentSetStrictIsolationResponse, NodeEnvironment,
};
use tentaflow_protocol::mesh::MeshCommandType;
use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode, SessionAuth};

use super::HandlerContext;
use crate::db::repository;
use crate::mesh::peer_registry::TrustStateTag;
use crate::services::config_bundle::{self, ConfigBundle};
use crate::services::environment as env_settings;

fn db_err(e: impl std::fmt::Display) -> ProtocolError {
    ProtocolError::internal(format!("environment database error: {e}"))
}

fn user_uuid(ctx: &HandlerContext) -> Option<String> {
    match &ctx.session {
        SessionAuth::UserSession { user_id, .. } => {
            Some(uuid::Uuid::from_bytes(*user_id).to_string())
        }
        _ => None,
    }
}

struct PendingPull {
    donor_node_id: String,
    bundle: ConfigBundle,
    // File-transport import carries a `source_environment` the CLIENT wrote
    // into the archive it uploaded — nothing here vouches for it the way a
    // trust-paired QUIC donor's own `ConfigBundleExport` response does
    // (P2-4). `true` for `ImportFromFileRequest`, `false` for `PullStart`.
    from_file: bool,
    created_at_ms: u64,
}

/// Held only for the wizard's own request/preview/apply round-trip, so an
/// abandoned pull (browser closed mid-wizard, admin never applies) must not
/// accumulate forever: bundles can carry MB-scale flow JSON, and each pull
/// holds one in full (P2-5). Both bounds are enforced together on every
/// insert; the TTL alone also self-heals a slow trickle of abandoned pulls
/// that never hits the entry cap.
const PENDING_PULL_TTL_MS: u64 = 15 * 60 * 1000;
const PENDING_PULL_MAX_ENTRIES: usize = 64;

fn pending_pulls() -> &'static DashMap<String, PendingPull> {
    static PENDING: OnceLock<DashMap<String, PendingPull>> = OnceLock::new();
    PENDING.get_or_init(DashMap::new)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Sweeps TTL-expired pulls, then — if still at/over capacity — evicts the
/// single oldest surviving entry to make room. Called on every insert so the
/// map never grows past `PENDING_PULL_MAX_ENTRIES` regardless of poll
/// traffic on `PullStatus`/`ImportPreviewDiff` (which only read, never sweep).
fn insert_pending_pull(pull_id: String, pending: PendingPull) {
    let map = pending_pulls();
    let now = now_ms();
    map.retain(|_, p| now.saturating_sub(p.created_at_ms) < PENDING_PULL_TTL_MS);
    if map.len() >= PENDING_PULL_MAX_ENTRIES {
        if let Some(oldest_id) = map
            .iter()
            .min_by_key(|entry| entry.value().created_at_ms)
            .map(|entry| entry.key().clone())
        {
            map.remove(&oldest_id);
        }
    }
    map.insert(pull_id, pending);
}

// =============================================================================
// Own environment identity — GetKind / SetKind / SetStrictIsolation
// =============================================================================

/// PLAN-M2 §1b fencing point 4 (`SUM/tentabus/PLAN-M2.md`): a `SetKind`
/// environment switch must evict this node from every bus replica set it
/// belongs to under its OLD environment identity — the same fencing
/// principle `invalidate_environment_cache` already applies to publish/
/// consume (a stale cached identity must not keep serving decisions for an
/// environment the node no longer declares), here applied to replication.
///
/// `coordinator` is threaded in rather than read from `bus::global()`
/// directly: `BusService` (`bus/mod.rs`) has no public getter for its
/// private `replication` field yet (`set_replication`'s own doc: "nothing
/// in this file's publish/open_consumer/fetch/commit reads `self.
/// replication` yet — that wiring is wave-2, agent S"), and `bus/mod.rs` is
/// PLAN-M2 §3's "jedyny właściciel" file for that wave — out of scope
/// here. The call site below passes `None` until that getter lands, at
/// which point it becomes
/// `crate::bus::global().and_then(|s| s.replication_coordinator())`; this
/// function's own logic (eviction + one audit entry) is complete and
/// tested independently of that missing wire.
///
/// `None` (no coordinator wired — M1 behavior, or wave-2 wiring not landed
/// yet) is a no-op, matching every `ReplicationCoordinator` call site's
/// "`None` means unchanged M1 behavior" convention (`bus/mod.rs`'s doc on
/// `set_replication`). A coordinator error is logged and swallowed — a
/// `SetKind` must not fail because bus replication eviction failed, the
/// same "best effort, never block the primary action" posture
/// `invalidate_environment_cache`'s own call site takes.
///
/// One audit entry per call, not per partition (PLAN-M2 §1b pt.4,
/// explicit): a single environment switch may evict this node from many
/// partitions' replica sets at once; the operator needs "this switch
/// evicted N partitions", not N individual rows.
pub(crate) fn evict_node_from_replica_sets_on_environment_change(
    coordinator: Option<std::sync::Arc<dyn crate::bus::ReplicationCoordinator>>,
    db: &crate::db::DbPool,
    local_node_id: &str,
    from: NodeEnvironment,
    to: NodeEnvironment,
) {
    let Some(coordinator) = coordinator else {
        return;
    };
    let evicted = match coordinator.evict_node_from_replica_sets(local_node_id, "env_change") {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(
                error = %e,
                node_id = local_node_id,
                "environment switch: bus replica-set eviction failed"
            );
            return;
        }
    };
    let _ = repository::log_audit(
        db,
        None,
        None,
        "bus.replica.evicted_env_change",
        None,
        Some(
            &json!({
                "node_id": local_node_id,
                "from": from.as_str(),
                "to": to.as_str(),
                "partitions_evicted": evicted,
            })
            .to_string(),
        ),
        None,
        Some(local_node_id),
    );
}

#[handler(variant = "EnvironmentGetKindRequest", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub fn environment_get_kind(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let kind = env_settings::get_node_environment(&ctx.state.db);
    let isolation_strict = env_settings::is_isolation_strict(&ctx.state.db);
    Ok(MessageBody::EnvironmentPromotionBody(
        EnvironmentPromotionPayload::GetKindResponse(EnvironmentGetKindResponse {
            kind,
            isolation_strict,
        }),
    ))
}

#[handler(variant = "EnvironmentSetKindRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn environment_set_kind(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::EnvironmentPromotionBody(EnvironmentPromotionPayload::SetKindRequest(p)) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected EnvironmentSetKindRequest",
            ))
        }
    };

    // Fail-closed server-side gate (pitfall #8) — a disabled UI button is UX
    // only, never the actual guarantee. `SetKind` carries its OWN
    // `confirm_environment_name`, independent of `ImportApply`'s.
    if payload.new_kind == NodeEnvironment::Prod
        && payload.confirm_environment_name.as_deref() != Some("PROD")
    {
        return Err(ProtocolError::bad_request(
            "switching to Prod requires confirm_environment_name == \"PROD\"",
        ));
    }

    // Ledger FIRST, settings SECOND (P1-5): `settings.node_environment` is the
    // canonical source of truth, so it must only ever advance to a value the
    // ledger has ALREADY committed to — never the other way around. Flipping
    // settings first (the previous order) left a window where a
    // `switch_node_environment` failure stranded settings pointing at the new
    // environment while the ledger (and every admission/outbox/resolver
    // decision keyed off it) stayed on the old one, with no rollback.
    let from = env_settings::get_node_environment(&ctx.state.db);
    let reseeded = crate::sync::runtime::switch_node_environment(payload.new_kind)
        .map_err(|e| ProtocolError::internal(format!("environment switch failed: {e}")))?;
    env_settings::set_node_environment(&ctx.state.db, payload.new_kind).map_err(db_err)?;

    // TentaBus Z12 fencing (SUM/tentabus/PLAN.md §4.4 pt.1): `BusService`
    // caches the node's environment (`publish`/`open_consumer`'s hot paths
    // must not re-read `settings.node_environment` from SQLite on every
    // call) — a `SetKind` that lands after the cache was primed must
    // invalidate it immediately, or every open `ConsumerHandle` and the
    // service itself keep enforcing fencing against the STALE environment
    // until this node restarts. plan-app-platform §7 W4 finding 6:
    // iterates EVERY running instance (`bus::running_instances()`), not
    // `bus::global()` — with two instances enabled, `global()` returns
    // `None` (its own §7 W4 finding 3 fix: it only ever resolves the
    // single-instance shim), so a node-environment change would silently
    // stop invalidating either engine's cache the moment a second instance
    // is enabled. Zero running instances is a no-op, not an error.
    for bus_service in crate::bus::running_instances() {
        bus_service.invalidate_environment_cache();
    }

    // PLAN-M2 §1b fencing point 4 (`SUM/tentabus/PLAN-M2.md`): a `SetKind`
    // must also evict this node from every bus replica set it belongs to
    // under its OLD environment identity, for EVERY running instance (same
    // finding 6 fix as above — was `bus::global()`, single-instance only).
    // `replication()` returning `None` (no coordinator ever wired, i.e.
    // RF=1/M1 behavior) is a no-op inside
    // `evict_node_from_replica_sets_on_environment_change`.
    for bus_service in crate::bus::running_instances() {
        evict_node_from_replica_sets_on_environment_change(
            bus_service.replication(),
            &ctx.state.db,
            &ctx.state.local_node_id,
            from,
            payload.new_kind,
        );
    }

    // Immediate rebuild (P2-8) — the catalog stamps `trusted_nodes.environment`
    // vs the LOCAL environment at snapshot-build time (`build_service_model_
    // entries`), so a stale snapshot after a `SetKind` would keep serving the
    // pre-switch instance set until the next unrelated rebuild trigger fired
    // (a full window of either wrongly-hidden or wrongly-exposed instances).
    ctx.state.router.rebuild_catalog();

    let _ = repository::log_audit(
        &ctx.state.db,
        user_uuid(ctx).as_deref(),
        None,
        "environment.changed",
        None,
        Some(
            &json!({
                "from": from.as_str(),
                "to": payload.new_kind.as_str(),
                "reseeded_operations": reseeded,
            })
            .to_string(),
        ),
        None,
        Some(&ctx.state.local_node_id),
    );

    Ok(MessageBody::EnvironmentPromotionBody(
        EnvironmentPromotionPayload::SetKindResponse(EnvironmentSetKindResponse {
            kind: payload.new_kind,
            reseeded_operations: reseeded as u64,
        }),
    ))
}

#[handler(variant = "EnvironmentSetStrictIsolationRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn environment_set_strict_isolation(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::EnvironmentPromotionBody(
            EnvironmentPromotionPayload::SetStrictIsolationRequest(p),
        ) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected EnvironmentSetStrictIsolationRequest",
            ))
        }
    };
    env_settings::set_isolation_strict(&ctx.state.db, payload.strict).map_err(db_err)?;
    let _ = repository::log_audit(
        &ctx.state.db,
        user_uuid(ctx).as_deref(),
        None,
        "environment.isolation_strict_toggled",
        None,
        Some(&json!({ "strict": payload.strict }).to_string()),
        None,
        Some(&ctx.state.local_node_id),
    );
    Ok(MessageBody::EnvironmentPromotionBody(
        EnvironmentPromotionPayload::SetStrictIsolationResponse(
            EnvironmentSetStrictIsolationResponse {
                strict: payload.strict,
            },
        ),
    ))
}

// =============================================================================
// Config bundle — file transport (export local, import a previously
// downloaded/received archive).
// =============================================================================

#[handler(variant = "EnvironmentExportBundleRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn environment_export_bundle(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let exported = config_bundle::export_bundle(&ctx.state.db, &ctx.state.local_node_id)
        .map_err(|e| ProtocolError::internal(format!("export failed: {e}")))?;
    let table_counts =
        exported
            .table_counts
            .into_iter()
            .map(|(table, row_count)| {
                tentaflow_protocol::environment::EnvironmentBundleTableCount { table, row_count }
            })
            .collect();
    Ok(MessageBody::EnvironmentPromotionBody(
        EnvironmentPromotionPayload::ExportBundleResponse(EnvironmentExportBundleResponse {
            filename: exported.filename,
            archive_bytes: exported.archive_bytes,
            manifest_sha256: exported.manifest_sha256,
            source_environment: exported.bundle.source_environment,
            table_counts,
        }),
    ))
}

/// File-transport import: stores the uploaded bundle as a pending pull (the
/// local node as its own "donor" label — file transport carries no live
/// donor node id), ready for `ImportPreviewDiff`/`ImportApply` exactly like a
/// QUIC pull.
#[handler(variant = "EnvironmentImportFromFileRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn environment_import_from_file(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::EnvironmentPromotionBody(
            EnvironmentPromotionPayload::ImportFromFileRequest(p),
        ) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected EnvironmentImportFromFileRequest",
            ))
        }
    };
    let bundle = config_bundle::parse_bundle(&payload.archive_bytes)
        .map_err(|e| ProtocolError::bad_request(format!("invalid config bundle file: {e}")))?;
    let pull_id = uuid::Uuid::new_v4().to_string();
    let donor_node_id = bundle.source_node_id.clone();
    insert_pending_pull(
        pull_id.clone(),
        PendingPull {
            donor_node_id,
            bundle,
            from_file: true,
            created_at_ms: now_ms(),
        },
    );
    Ok(MessageBody::EnvironmentPromotionBody(
        EnvironmentPromotionPayload::PullStartResponse(EnvironmentPullStartResponse {
            pull_id,
            phase: "imported".to_string(),
            error: None,
        }),
    ))
}

// =============================================================================
// Config bundle — QUIC pull wizard (donor select -> start -> poll).
// =============================================================================

#[handler(variant = "EnvironmentPullDonorListRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn environment_pull_donor_list(
    _req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let local_node_id = ctx.state.local_node_id.as_ref();
    let donors: Vec<EnvironmentPullDonorInfo> = ctx
        .state
        .mesh_peer_store
        .registry()
        .map(|reg| {
            reg.snapshot_summary()
                .into_iter()
                .filter(|s| matches!(s.trust, TrustStateTag::Trusted))
                .filter_map(|s| {
                    let node_id = hex::encode(s.node_id);
                    if node_id == local_node_id {
                        return None;
                    }
                    let hostname = if s.hostname.is_empty() {
                        node_id.clone()
                    } else {
                        (*s.hostname).to_string()
                    };
                    // Fail-closed (P1-2): a peer whose environment is unknown
                    // (no active `trusted_nodes` row, or a NULL pre-Z12 row)
                    // never appears as a pull donor candidate — showing it as
                    // "prod" by default would let an operator pull config from
                    // a peer whose real environment nobody actually confirmed.
                    let environment =
                        repository::get_trusted_node_environment(&ctx.state.db, &node_id)
                            .ok()
                            .flatten()?;
                    Some(EnvironmentPullDonorInfo {
                        node_id,
                        hostname,
                        environment,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Ok(MessageBody::EnvironmentPromotionBody(
        EnvironmentPromotionPayload::PullDonorListResponse(EnvironmentPullDonorListResponse {
            donors,
        }),
    ))
}

#[handler(variant = "EnvironmentPullStartRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub async fn environment_pull_start(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::EnvironmentPromotionBody(EnvironmentPromotionPayload::PullStartRequest(p)) => {
            p
        }
        _ => {
            return Err(ProtocolError::bad_request(
                "expected EnvironmentPullStartRequest",
            ))
        }
    };

    let is_trusted = ctx
        .state
        .mesh_security
        .as_ref()
        .map_or(false, |s| s.is_trusted(&payload.donor_node_id));
    if !is_trusted {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "donor node is not a trusted paired peer",
        ));
    }
    let qm = ctx
        .state
        .quic_mesh
        .clone()
        .ok_or_else(|| ProtocolError::internal("mesh manager unavailable"))?;

    let response = qm
        .send_command(&payload.donor_node_id, MeshCommandType::ConfigBundleExport)
        .await
        .map_err(|e| {
            ProtocolError::new(
                ProtocolErrorCode::Internal,
                format!("config bundle pull from donor failed: {e}"),
            )
        })?;

    if !response.ok {
        return Ok(MessageBody::EnvironmentPromotionBody(
            EnvironmentPromotionPayload::PullStartResponse(EnvironmentPullStartResponse {
                pull_id: String::new(),
                phase: "failed".to_string(),
                error: response.error,
            }),
        ));
    }

    let archive_bytes = match response.payload {
        tentaflow_protocol::mesh::MeshCommandResponsePayload::ConfigBundleExport {
            archive_bytes,
            manifest_sha256,
            ..
        } => {
            use sha2::{Digest, Sha256};
            let mut hasher = Sha256::new();
            hasher.update(&archive_bytes);
            let actual = hex::encode(hasher.finalize());
            if actual != manifest_sha256 {
                return Err(ProtocolError::new(
                    ProtocolErrorCode::Internal,
                    "config bundle integrity check failed (sha256 mismatch)",
                ));
            }
            archive_bytes
        }
        _ => {
            return Err(ProtocolError::new(
                ProtocolErrorCode::Internal,
                "donor returned an unexpected response payload",
            ))
        }
    };

    let bundle = config_bundle::parse_bundle(&archive_bytes)
        .map_err(|e| ProtocolError::internal(format!("invalid config bundle from donor: {e}")))?;
    let pull_id = uuid::Uuid::new_v4().to_string();
    insert_pending_pull(
        pull_id.clone(),
        PendingPull {
            donor_node_id: payload.donor_node_id.clone(),
            bundle,
            from_file: false,
            created_at_ms: now_ms(),
        },
    );

    Ok(MessageBody::EnvironmentPromotionBody(
        EnvironmentPromotionPayload::PullStartResponse(EnvironmentPullStartResponse {
            pull_id,
            phase: "imported".to_string(),
            error: None,
        }),
    ))
}

#[handler(variant = "EnvironmentPullStatusRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn environment_pull_status(
    req: &MessageBody,
    _ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::EnvironmentPromotionBody(EnvironmentPromotionPayload::PullStatusRequest(
            p,
        )) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected EnvironmentPullStatusRequest",
            ))
        }
    };
    let phase = if pending_pulls().contains_key(&payload.pull_id) {
        "imported".to_string()
    } else {
        "failed".to_string()
    };
    Ok(MessageBody::EnvironmentPromotionBody(
        EnvironmentPromotionPayload::PullStatusResponse(EnvironmentPullStatusResponse {
            pull_id: payload.pull_id.clone(),
            phase,
            error: None,
        }),
    ))
}

#[handler(variant = "EnvironmentImportPreviewDiffRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn environment_import_preview_diff(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::EnvironmentPromotionBody(
            EnvironmentPromotionPayload::ImportPreviewDiffRequest(p),
        ) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected EnvironmentImportPreviewDiffRequest",
            ))
        }
    };
    let pending = pending_pulls()
        .get(&payload.pull_id)
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "unknown pull_id"))?;
    let diff = config_bundle::diff_bundle(&ctx.state.db, &pending.bundle)
        .map_err(|e| ProtocolError::internal(format!("diff failed: {e}")))?;

    let _ = repository::log_audit(
        &ctx.state.db,
        user_uuid(ctx).as_deref(),
        None,
        "environment.pull_previewed",
        Some(&format!("node:{}", pending.donor_node_id)),
        Some(
            &json!({
                "from_environment": diff.from_environment.as_str(),
                "to_environment": diff.to_environment.as_str(),
                "added": diff.added.len(),
                "changed": diff.changed.len(),
                "skipped": diff.skipped.len(),
            })
            .to_string(),
        ),
        None,
        Some(&ctx.state.local_node_id),
    );

    Ok(MessageBody::EnvironmentPromotionBody(
        EnvironmentPromotionPayload::ImportPreviewDiffResponse(
            EnvironmentImportPreviewDiffResponse {
                pull_id: payload.pull_id.clone(),
                from_environment: diff.from_environment,
                to_environment: diff.to_environment,
                added: diff.added.into_iter().map(to_wire_entry).collect(),
                changed: diff.changed.into_iter().map(to_wire_entry).collect(),
                skipped: diff.skipped.into_iter().map(to_wire_entry).collect(),
                flows_count: diff.flows_count,
                settings_count: diff.settings_count,
                aliases_count: diff.aliases_count,
            },
        ),
    ))
}

fn to_wire_entry(e: config_bundle::DiffEntry) -> EnvironmentDiffEntry {
    EnvironmentDiffEntry {
        table: e.table,
        resource_id: e.resource_id,
        label: e.label,
    }
}

#[handler(variant = "EnvironmentImportApplyRequest", since = (1, 0))]
#[policy(Admin)]
#[observed]
pub fn environment_import_apply(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::EnvironmentPromotionBody(EnvironmentPromotionPayload::ImportApplyRequest(
            p,
        )) => p,
        _ => {
            return Err(ProtocolError::bad_request(
                "expected EnvironmentImportApplyRequest",
            ))
        }
    };
    let pending = pending_pulls()
        .get(&payload.pull_id)
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "unknown pull_id"))?;

    let from_environment = pending.bundle.source_environment;
    let to_environment = env_settings::get_node_environment(&ctx.state.db);

    // Fail-closed server-side gate (D-Z12.8 / pitfall #6) — a promotion
    // upward (in particular anything landing on Prod) requires the target
    // environment's name typed EXACTLY, independently of whether the client
    // ever showed the warning modal. Own field, own gate — NOT the same one
    // `SetKind` uses (pitfall #6).
    //
    // File-transport import is additionally gated whenever the LOCAL
    // (target) environment is Prod, regardless of rank (P2-4): `from_
    // environment` for a file bundle is whatever the uploaded archive
    // itself claims — nobody vouches for it the way a trust-paired QUIC
    // donor's own signed `ConfigBundleExport` response does, so a bundle
    // that lies about being "prod" (same rank as the local node, so the
    // rank check above would not fire) must not bypass confirmation.
    let requires_confirmation = to_environment > from_environment
        || (pending.from_file && to_environment == NodeEnvironment::Prod);
    if requires_confirmation {
        let expected = to_environment.as_str().to_uppercase();
        if payload.confirm_environment_name.as_deref() != Some(expected.as_str()) {
            return Err(ProtocolError::bad_request(format!(
                "promoting from {} to {} requires confirm_environment_name == \"{}\"",
                from_environment, to_environment, expected
            )));
        }
    }

    let registry = ctx
        .state
        .router
        .flow_dispatcher()
        .map(|d| d.registry().as_ref());
    let result = config_bundle::apply_bundle(
        &ctx.state.db,
        &pending.bundle,
        &payload.selected_resource_keys,
        registry,
    )
    .map_err(|e| ProtocolError::bad_request(format!("apply failed: {e}")))?;

    let donor_node_id = pending.donor_node_id.clone();
    let from_file = pending.from_file;
    drop(pending);
    pending_pulls().remove(&payload.pull_id);

    let _ = repository::log_audit(
        &ctx.state.db,
        user_uuid(ctx).as_deref(),
        None,
        "environment.pull_imported",
        Some(&format!("node:{donor_node_id}")),
        Some(
            &json!({
                "from_environment": from_environment.as_str(),
                // File-transport `from_environment` is a self-reported claim
                // in the uploaded archive (P2-4) — flagged so an auditor
                // never conflates it with the QUIC path's donor-attested
                // value.
                "from_environment_unverified": from_file,
                "to_environment": to_environment.as_str(),
                "imported_count": result.imported_count,
                "confirm_environment_name": payload.confirm_environment_name,
            })
            .to_string(),
        ),
        None,
        Some(&ctx.state.local_node_id),
    );

    Ok(MessageBody::EnvironmentPromotionBody(
        EnvironmentPromotionPayload::ImportApplyResponse(EnvironmentImportApplyResponse {
            applied: true,
            imported_count: result.imported_count,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tentaflow_protocol::environment::{
        EnvironmentImportApplyRequest, EnvironmentImportFromFileRequest,
        EnvironmentImportPreviewDiffRequest, EnvironmentSetKindRequest,
        EnvironmentSetStrictIsolationRequest,
    };

    /// `environment_set_kind` calls `sync::runtime::switch_node_environment`,
    /// which needs the process-global `SYNC_RUNTIME` singleton
    /// (`sync::runtime::init`) — production always has one (booted at
    /// startup), but a bare `HandlerContext` fixture does not. `init` is
    /// idempotent (a second call in the same process returns the existing
    /// runtime), so this is safe to call from every test in this module;
    /// only the FIRST call actually opens the Fjall ledger.
    ///
    /// Environment (unlike `AppState::for_test()`'s own fresh per-test
    /// SQLite pool) lives in ONE Fjall ledger shared by the WHOLE test
    /// binary — `resolver.rs`'s environment-fencing tests read the exact
    /// same `sync::runtime::core_environment()` (ROADMAP Z12, P2-11).
    /// Without serialization, two tests in different modules that both flip
    /// it (this module's `SetKind` tests do, on purpose) can interleave and
    /// make each other's assertions depend on execution order. Held for the
    /// FULL test body (the caller keeps the returned guard alive, not just
    /// during setup) and always resets to `Prod` before returning, so every
    /// test that uses this fixture observes the SAME starting point
    /// regardless of what ran before it in the same process.
    fn locked_env_fixture() -> std::sync::MutexGuard<'static, ()> {
        let guard = crate::addon::fs_sandbox::test_home_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // `OnceLock`, not `std::sync::Once`: a `Once` poisons forever the
        // moment its closure panics, so a transient `Fjall(Locked)` (another
        // test in the binary still holding the ledger under the shared HOME
        // at the exact instant this fixture races to open it first) would
        // brick every later test that shares this fixture. `OnceLock` is
        // only marked done AFTER a successful `init`, so a failed attempt
        // lets the next test retry cleanly instead of inheriting a poison.
        static INITIALIZED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        if INITIALIZED.get().is_none() {
            let tmp = tempfile::tempdir().expect("tempdir");
            std::env::set_var("HOME", tmp.path());
            std::env::set_var("TENTAFLOW_HOME", tmp.path());

            let conn = rusqlite::Connection::open_in_memory().expect("open db");
            crate::db::migrations::run(&conn).expect("run migrations");
            let db: crate::db::DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
            let cipher = std::sync::Arc::new(crate::crypto::SettingsCipher::new(&[7u8; 32]));
            let security = std::sync::Arc::new(
                crate::mesh::security::MeshSecurity::new(db.clone(), cipher.clone())
                    .expect("mesh security"),
            );
            // Retry with backoff on a transient lock instead of failing
            // outright — up to ~30s, matching the other Fjall-init fixtures
            // in the test binary (`resolver.rs`, `dispatch/mod.rs`).
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
            // The ledger stays open for the rest of the process; the tempdir
            // handle is intentionally leaked (not dropped) so its path stays
            // valid for the runtime's lifetime.
            std::mem::forget(tmp);
            let _ = INITIALIZED.set(());
        }
        if crate::sync::runtime::core_environment() != NodeEnvironment::Prod {
            crate::sync::runtime::switch_node_environment(NodeEnvironment::Prod)
                .expect("reset environment to prod baseline");
        }
        guard
    }

    fn ctx_admin() -> (HandlerContext, std::sync::MutexGuard<'static, ()>) {
        let guard = locked_env_fixture();
        let ctx = HandlerContext {
            session: SessionAuth::UserSession {
                user_id: *uuid::Uuid::new_v4().as_bytes(),
                role: Some("admin".to_string()),
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state: crate::dispatch::state::AppState::for_test(),
            org_context: None,
        };
        (ctx, guard)
    }

    fn ctx_power_user() -> (HandlerContext, std::sync::MutexGuard<'static, ()>) {
        let (mut ctx, guard) = ctx_admin();
        ctx.session = SessionAuth::UserSession {
            user_id: *uuid::Uuid::new_v4().as_bytes(),
            role: Some("power_user".to_string()),
        };
        (ctx, guard)
    }

    fn set_kind_body(new_kind: NodeEnvironment, confirm: Option<&str>) -> MessageBody {
        MessageBody::EnvironmentPromotionBody(EnvironmentPromotionPayload::SetKindRequest(
            EnvironmentSetKindRequest {
                new_kind,
                confirm_environment_name: confirm.map(str::to_string),
            },
        ))
    }

    // -------------------------------------------------------------------
    // SetKind — server-side confirmation gate (D-Z12.9, pitfall #8).
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn set_kind_to_prod_without_confirmation_is_rejected() {
        let (ctx, _env_lock) = ctx_admin();
        let (body, is_err) =
            crate::dispatch::dispatch(&set_kind_body(NodeEnvironment::Prod, None), &ctx).await;
        assert!(is_err, "SetKind to Prod without confirmation must fail");
        match body {
            MessageBody::Error(e) => assert_eq!(e.code, ProtocolErrorCode::BadRequest),
            other => panic!("expected BadRequest error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_kind_to_prod_with_wrong_case_is_rejected() {
        let (ctx, _env_lock) = ctx_admin();
        let (body, is_err) =
            crate::dispatch::dispatch(&set_kind_body(NodeEnvironment::Prod, Some("prod")), &ctx)
                .await;
        assert!(is_err, "SetKind confirmation is case-sensitive and exact");
        match body {
            MessageBody::Error(e) => assert_eq!(e.code, ProtocolErrorCode::BadRequest),
            other => panic!("expected BadRequest error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_kind_to_prod_with_exact_confirmation_succeeds() {
        let (ctx, _env_lock) = ctx_admin();
        let (body, is_err) =
            crate::dispatch::dispatch(&set_kind_body(NodeEnvironment::Prod, Some("PROD")), &ctx)
                .await;
        assert!(!is_err, "unexpected error body: {body:?}");
        match body {
            MessageBody::EnvironmentPromotionBody(
                EnvironmentPromotionPayload::SetKindResponse(r),
            ) => assert_eq!(r.kind, NodeEnvironment::Prod),
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn set_kind_to_test_does_not_require_confirmation() {
        let (ctx, _env_lock) = ctx_admin();
        let (body, is_err) =
            crate::dispatch::dispatch(&set_kind_body(NodeEnvironment::Test, None), &ctx).await;
        assert!(!is_err, "unexpected error body: {body:?}");
        match body {
            MessageBody::EnvironmentPromotionBody(
                EnvironmentPromotionPayload::SetKindResponse(r),
            ) => assert_eq!(r.kind, NodeEnvironment::Test),
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_non_admin_session_is_denied_for_set_kind() {
        let (ctx, _env_lock) = ctx_power_user();
        let (body, is_err) =
            crate::dispatch::dispatch(&set_kind_body(NodeEnvironment::Test, None), &ctx).await;
        assert!(is_err, "power_user must not pass an Admin gate");
        match body {
            MessageBody::Error(e) => assert_eq!(e.code, ProtocolErrorCode::PolicyDenied),
            other => panic!("expected PolicyDenied error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_kind_is_readable_by_power_user() {
        let (ctx, _env_lock) = ctx_power_user();
        let (body, is_err) = crate::dispatch::dispatch(
            &MessageBody::EnvironmentPromotionBody(EnvironmentPromotionPayload::GetKindRequest(
                tentaflow_protocol::environment::EnvironmentGetKindRequest {},
            )),
            &ctx,
        )
        .await;
        assert!(!is_err, "unexpected error body: {body:?}");
    }

    #[tokio::test]
    async fn set_strict_isolation_round_trips() {
        let (ctx, _env_lock) = ctx_admin();
        let (body, is_err) = crate::dispatch::dispatch(
            &MessageBody::EnvironmentPromotionBody(
                EnvironmentPromotionPayload::SetStrictIsolationRequest(
                    EnvironmentSetStrictIsolationRequest { strict: true },
                ),
            ),
            &ctx,
        )
        .await;
        assert!(!is_err, "unexpected error body: {body:?}");
        match body {
            MessageBody::EnvironmentPromotionBody(
                EnvironmentPromotionPayload::SetStrictIsolationResponse(r),
            ) => assert!(r.strict),
            other => panic!("unexpected response: {other:?}"),
        }
    }

    // -------------------------------------------------------------------
    // ImportApply — server-side confirmation gate for upward promotion
    // (D-Z12.8, pitfall #6), independent of `SetKind`'s own gate.
    // -------------------------------------------------------------------

    /// Starts a pending pull via the file-transport path (no live mesh
    /// connection needed in tests) and returns its `pull_id`. The donor
    /// bundle is exported from a SEPARATE seeded DB standing in for the
    /// remote node, matching the real cross-node shape of a pull.
    /// `donor_environment` sets the DONOR's declared environment (defaults to
    /// `Prod` when left unset by the caller's own DB) — the caller controls
    /// it explicitly so tests can construct a genuine upward-vs-downward
    /// promotion relative to the RECEIVER's environment.
    async fn start_file_pull_from(
        receiver_ctx: &HandlerContext,
        donor_flow_id: &str,
        donor_environment: NodeEnvironment,
    ) -> String {
        let donor_pool = {
            let conn = rusqlite::Connection::open_in_memory().unwrap();
            crate::db::migrations::run(&conn).unwrap();
            std::sync::Arc::new(crate::db::Db::from_connection(conn))
        };
        env_settings::set_node_environment(&donor_pool, donor_environment).unwrap();
        {
            // A structurally valid minimal flow (one trigger -> one output) —
            // `apply_bundle` runs it through the SAME R1-R8 validation a
            // manual flow save does, so an empty `{}` body would be
            // (correctly) rejected before ever reaching the diff/apply
            // assertions this fixture exists for.
            const MINIMAL_VALID_FLOW_JSON: &str = r#"{"nodes":[{"id":"t","type":"trigger","config":{}},{"id":"o","type":"output","config":{}}],"edges":[{"from":"t","to":"o","from_port":"text","to_port":"text"}]}"#;
            let conn = donor_pool.write().unwrap();
            conn.execute(
                "INSERT INTO flows (id, name, flow_json, status) VALUES (?1, 'donor flow', ?2, 'active')",
                rusqlite::params![donor_flow_id, MINIMAL_VALID_FLOW_JSON],
            )
            .unwrap();
        }
        let exported = crate::services::config_bundle::export_bundle(&donor_pool, "donor-node")
            .expect("export donor bundle");

        let (body, is_err) = crate::dispatch::dispatch(
            &MessageBody::EnvironmentPromotionBody(
                EnvironmentPromotionPayload::ImportFromFileRequest(
                    EnvironmentImportFromFileRequest {
                        archive_bytes: exported.archive_bytes,
                    },
                ),
            ),
            receiver_ctx,
        )
        .await;
        assert!(!is_err, "unexpected error body: {body:?}");
        match body {
            MessageBody::EnvironmentPromotionBody(
                EnvironmentPromotionPayload::PullStartResponse(r),
            ) => r.pull_id,
            other => panic!("unexpected response: {other:?}"),
        }
    }

    #[tokio::test]
    async fn import_apply_promotion_without_confirmation_is_rejected() {
        let (ctx, _env_lock) = ctx_admin();
        // Receiver stays at the default Prod; donor is Dev, so importing
        // INTO this receiver is a genuine Dev -> Prod upward promotion.
        let pull_id = start_file_pull_from(&ctx, "donor-flow-1", NodeEnvironment::Dev).await;

        let (body, is_err) = crate::dispatch::dispatch(
            &MessageBody::EnvironmentPromotionBody(
                EnvironmentPromotionPayload::ImportApplyRequest(EnvironmentImportApplyRequest {
                    pull_id,
                    confirm_environment_name: None,
                    selected_resource_keys: vec!["flows:donor-flow-1".to_string()],
                }),
            ),
            &ctx,
        )
        .await;
        assert!(
            is_err,
            "an upward promotion (Dev donor -> Prod receiver) without confirmation must fail"
        );
        match body {
            MessageBody::Error(e) => assert_eq!(e.code, ProtocolErrorCode::BadRequest),
            other => panic!("expected BadRequest error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn import_apply_promotion_with_correct_confirmation_succeeds() {
        let (ctx, _env_lock) = ctx_admin();
        let pull_id = start_file_pull_from(&ctx, "donor-flow-2", NodeEnvironment::Dev).await;

        let (body, is_err) = crate::dispatch::dispatch(
            &MessageBody::EnvironmentPromotionBody(
                EnvironmentPromotionPayload::ImportApplyRequest(EnvironmentImportApplyRequest {
                    pull_id,
                    confirm_environment_name: Some("PROD".to_string()),
                    selected_resource_keys: vec!["flows:donor-flow-2".to_string()],
                }),
            ),
            &ctx,
        )
        .await;
        assert!(!is_err, "unexpected error body: {body:?}");
        match body {
            MessageBody::EnvironmentPromotionBody(
                EnvironmentPromotionPayload::ImportApplyResponse(r),
            ) => {
                assert!(r.applied);
                assert_eq!(r.imported_count, 1);
            }
            other => panic!("unexpected response: {other:?}"),
        }
    }

    /// Same-rank pull (donor Dev -> receiver Dev — NOT an upward promotion,
    /// and NOT landing on Prod) must apply without any confirmation field.
    #[tokio::test]
    async fn import_apply_same_rank_does_not_require_confirmation() {
        let (ctx, _env_lock) = ctx_admin();
        env_settings::set_node_environment(&ctx.state.db, NodeEnvironment::Dev).unwrap();
        let pull_id = start_file_pull_from(&ctx, "donor-flow-3", NodeEnvironment::Dev).await;

        let (body, is_err) = crate::dispatch::dispatch(
            &MessageBody::EnvironmentPromotionBody(
                EnvironmentPromotionPayload::ImportApplyRequest(EnvironmentImportApplyRequest {
                    pull_id,
                    confirm_environment_name: None,
                    selected_resource_keys: vec!["flows:donor-flow-3".to_string()],
                }),
            ),
            &ctx,
        )
        .await;
        assert!(!is_err, "unexpected error body: {body:?}");
    }

    /// P2-4: a file-transport bundle's `from_environment` is a self-reported
    /// claim in the uploaded archive, never attested by a trust-paired peer —
    /// so importing INTO Prod always requires confirmation, even when the
    /// bundle claims to already be "prod" itself (same rank, which the plain
    /// upward-promotion check above would NOT catch on its own).
    #[tokio::test]
    async fn file_import_into_prod_always_requires_confirmation_even_at_same_rank() {
        let (ctx, _env_lock) = ctx_admin();
        // Receiver defaults to Prod (fresh test DB, no `SetKind` performed).
        let pull_id = start_file_pull_from(&ctx, "donor-flow-3b", NodeEnvironment::Prod).await;

        let (body, is_err) = crate::dispatch::dispatch(
            &MessageBody::EnvironmentPromotionBody(
                EnvironmentPromotionPayload::ImportApplyRequest(EnvironmentImportApplyRequest {
                    pull_id,
                    confirm_environment_name: None,
                    selected_resource_keys: vec!["flows:donor-flow-3b".to_string()],
                }),
            ),
            &ctx,
        )
        .await;
        assert!(
            is_err,
            "a file-transport bundle claiming 'prod' must still require confirmation on a Prod receiver"
        );
        match body {
            MessageBody::Error(e) => assert_eq!(e.code, ProtocolErrorCode::BadRequest),
            other => panic!("expected BadRequest error, got {other:?}"),
        }
    }

    /// `ImportPreviewDiffResponse` counts must be consistent with what
    /// `ImportApply` actually imports for the SAME selection — the UI's
    /// warning-modal numbers must never diverge from the operation it gates.
    #[tokio::test]
    async fn preview_diff_counts_match_apply_result() {
        let (ctx, _env_lock) = ctx_admin();
        // Same-rank, non-Prod on both ends so the P2-4 Prod-always-confirm
        // gate does not interfere with what this test actually asserts
        // (diff counts vs. apply counts).
        env_settings::set_node_environment(&ctx.state.db, NodeEnvironment::Dev).unwrap();
        let pull_id = start_file_pull_from(&ctx, "donor-flow-4", NodeEnvironment::Dev).await;

        let (diff_body, is_err) = crate::dispatch::dispatch(
            &MessageBody::EnvironmentPromotionBody(
                EnvironmentPromotionPayload::ImportPreviewDiffRequest(
                    EnvironmentImportPreviewDiffRequest {
                        pull_id: pull_id.clone(),
                    },
                ),
            ),
            &ctx,
        )
        .await;
        assert!(!is_err, "unexpected error body: {diff_body:?}");
        let diff = match diff_body {
            MessageBody::EnvironmentPromotionBody(
                EnvironmentPromotionPayload::ImportPreviewDiffResponse(r),
            ) => r,
            other => panic!("unexpected response: {other:?}"),
        };
        assert_eq!(diff.flows_count, 1);
        assert_eq!(diff.added.len(), 1);

        let selected: Vec<String> = diff
            .added
            .iter()
            .chain(diff.changed.iter())
            .map(|e| format!("{}:{}", e.table, e.resource_id))
            .collect();
        let (apply_body, is_err) = crate::dispatch::dispatch(
            &MessageBody::EnvironmentPromotionBody(
                EnvironmentPromotionPayload::ImportApplyRequest(EnvironmentImportApplyRequest {
                    pull_id,
                    confirm_environment_name: None,
                    selected_resource_keys: selected,
                }),
            ),
            &ctx,
        )
        .await;
        assert!(!is_err, "unexpected error body: {apply_body:?}");
        match apply_body {
            MessageBody::EnvironmentPromotionBody(
                EnvironmentPromotionPayload::ImportApplyResponse(r),
            ) => assert_eq!(
                r.imported_count as usize,
                diff.added.len() + diff.changed.len(),
                "apply must import exactly the entries the preview diff reported"
            ),
            other => panic!("unexpected response: {other:?}"),
        }
    }

    // -------------------------------------------------------------------
    // TentaBus M2 (SUM/tentabus/PLAN-M2.md §1b pt.4) —
    // `evict_node_from_replica_sets_on_environment_change`. No dispatch
    // harness needed for this one: it is a plain function taking an
    // `Option<Arc<dyn ReplicationCoordinator>>`, so a mock coordinator
    // exercises it directly without going through `SetKind`'s full
    // handler (which cannot supply a real coordinator yet — see the
    // function's own doc for why).
    // -------------------------------------------------------------------

    struct MockCoordinator {
        evict_result: std::sync::Mutex<Option<Result<u32, crate::bus::ReplError>>>,
    }

    impl crate::bus::ReplicationCoordinator for MockCoordinator {
        fn role(&self, _org: &str, _topic: &str, _partition: u32) -> crate::bus::PartitionRole {
            unimplemented!("not exercised by the environment-switch eviction hook")
        }
        fn preflight(
            &self,
            _org: &str,
            _topic: &str,
            _partition: u32,
            _acks: crate::bus::topics::Acks,
        ) -> Result<u32, crate::bus::ReplError> {
            unimplemented!("not exercised by the environment-switch eviction hook")
        }
        fn await_acks(
            &self,
            _org: &str,
            _topic: &str,
            _partition: u32,
            _next_offset: u64,
            _acks: crate::bus::topics::Acks,
            _timeout: std::time::Duration,
        ) -> Result<crate::bus::AckOutcome, crate::bus::ReplError> {
            unimplemented!("not exercised by the environment-switch eviction hook")
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
            unimplemented!("not exercised by the environment-switch eviction hook")
        }
        fn evict_node_from_replica_sets(
            &self,
            _node_id: &str,
            _reason: &'static str,
        ) -> Result<u32, crate::bus::ReplError> {
            self.evict_result
                .lock()
                .unwrap()
                .take()
                .expect("evict_result configured once per test")
        }
        fn transfer_leader(
            &self,
            _org: &str,
            _topic: &str,
            _partition: u32,
            _target: &str,
        ) -> Result<u32, crate::bus::ReplError> {
            unimplemented!("not exercised by the environment-switch eviction hook")
        }
        fn reassign(
            &self,
            _org: &str,
            _topic: &str,
            _partition: Option<u32>,
            _replicas: &[String],
        ) -> Result<u32, crate::bus::ReplError> {
            unimplemented!("not exercised by the environment-switch eviction hook")
        }
        fn snapshot(&self, _org: &str, _topic: Option<&str>) -> crate::bus::ReplicationSnapshot {
            unimplemented!("not exercised by the environment-switch eviction hook")
        }
    }

    fn audit_count(db: &crate::db::DbPool, action: &str) -> i64 {
        let conn = db.read().expect("db lock");
        conn.query_row(
            "SELECT COUNT(*) FROM audit_log WHERE action = ?1",
            rusqlite::params![action],
            |row| row.get(0),
        )
        .expect("count audit_log rows")
    }

    #[test]
    fn evict_hook_is_noop_when_no_coordinator_is_wired() {
        let state = crate::dispatch::state::AppState::for_test();
        evict_node_from_replica_sets_on_environment_change(
            None,
            &state.db,
            "node-1",
            NodeEnvironment::Test,
            NodeEnvironment::Prod,
        );
        assert_eq!(audit_count(&state.db, "bus.replica.evicted_env_change"), 0);
    }

    #[test]
    fn evict_hook_writes_one_audit_entry_carrying_the_eviction_count() {
        let state = crate::dispatch::state::AppState::for_test();
        let coordinator: std::sync::Arc<dyn crate::bus::ReplicationCoordinator> =
            std::sync::Arc::new(MockCoordinator {
                evict_result: std::sync::Mutex::new(Some(Ok(3))),
            });
        evict_node_from_replica_sets_on_environment_change(
            Some(coordinator),
            &state.db,
            "node-1",
            NodeEnvironment::Test,
            NodeEnvironment::Prod,
        );
        assert_eq!(audit_count(&state.db, "bus.replica.evicted_env_change"), 1);
    }

    #[test]
    fn evict_hook_writes_no_audit_entry_when_the_coordinator_errors() {
        let state = crate::dispatch::state::AppState::for_test();
        let coordinator: std::sync::Arc<dyn crate::bus::ReplicationCoordinator> =
            std::sync::Arc::new(MockCoordinator {
                evict_result: std::sync::Mutex::new(Some(Err(
                    crate::bus::ReplError::NotAReplica {
                        topic: "orders.created".to_string(),
                        partition: 0,
                        node_id: "node-1".to_string(),
                    },
                ))),
            });
        evict_node_from_replica_sets_on_environment_change(
            Some(coordinator),
            &state.db,
            "node-1",
            NodeEnvironment::Test,
            NodeEnvironment::Prod,
        );
        assert_eq!(audit_count(&state.db, "bus.replica.evicted_env_change"), 0);
    }
}
