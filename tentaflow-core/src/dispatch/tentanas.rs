// =============================================================================
// File: dispatch/tentanas.rs — the TentaNas request family (plan-02 §8).
//       Every request runs on the node the dashboard selected: the client
//       sets the forward target and `app_route` moves the whole body there
//       before this handler sees it, so everything below acts on THIS
//       node's disks, channel and database. Only `NodesListRequest` is
//       answered wherever it lands.
//
//       Gate: instance enabled + permission matrix (`app_gate`), plus the
//       org Admin role for the privilege channel, package installation and
//       (later) destructive pool operations — the matrix can delegate
//       `nas.admin`, the role check cannot be delegated.
// =============================================================================

use std::sync::Arc;
use std::time::Duration;

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::tentanas::{SudoSecret, TentaNasPayload as P};
use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode};
use tentanas_helper::{HelperCommand, PackageManager, SelfTestKind};

use super::HandlerContext;
use crate::db::DbPool;
use crate::profiling::collectors::elevation::ElevationToken;
use crate::tentanas::{self, broker::BrokerError, db as store};

const PERM_READ: &str = "nas.read";
const PERM_POOLS: &str = "nas.pools.manage";
const PERM_ADMIN: &str = "nas.admin";

fn tn(body: P) -> MessageBody {
    MessageBody::TentaNasBody(body)
}

fn internal(scope: &str, error: impl std::fmt::Display) -> ProtocolError {
    tracing::warn!(scope, error = %error, "tentanas error");
    ProtocolError::internal(format!("tentanas {scope} failed"))
}

fn broker_error(scope: &str, error: BrokerError) -> ProtocolError {
    match error {
        BrokerError::Unarmed(why) => ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            format!("privilege channel not available: {why}"),
        ),
        BrokerError::ToolMissing(tool) => {
            ProtocolError::new(ProtocolErrorCode::NotAvailable, format!("{tool} is not installed"))
        }
        BrokerError::InvalidArgument(d) => ProtocolError::bad_request(d),
        other => internal(scope, other),
    }
}

/// The caller's instance + database after the matrix check.
struct Gate {
    addon_id: String,
    org_id: String,
    user_id: String,
    db: DbPool,
}

fn gate(ctx: &HandlerContext, permission: &str) -> Result<Gate, ProtocolError> {
    let org = ctx
        .org_context
        .as_ref()
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required"))?;
    let addon_id = super::app_gate::require_app_permission(ctx, tentanas::PACKAGE_ID, permission)?;
    let db = tentanas::open_db(&ctx.state.db, &org.org_id, &addon_id)
        .map_err(|e| internal("database", e))?;
    Ok(Gate {
        addon_id,
        org_id: org.org_id.clone(),
        user_id: org.user_id.clone(),
        db,
    })
}

/// `nas.admin` AND the org Admin role (§4 table: the privilege channel and
/// system packages are the operator's, not delegable through the matrix).
fn gate_admin(ctx: &HandlerContext) -> Result<Gate, ProtocolError> {
    let g = gate(ctx, PERM_ADMIN)?;
    let is_org_admin = ctx.org_context.as_ref().is_some_and(|o| o.has("org.admin"));
    if !is_org_admin {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "org Admin role required",
        ));
    }
    Ok(g)
}

fn token(secret: &SudoSecret) -> Arc<ElevationToken> {
    Arc::new(ElevationToken::new_sudo(secret.0.clone()))
}

fn staging_dir(g: &Gate) -> Result<std::path::PathBuf, ProtocolError> {
    crate::addon::fs_sandbox::addon_data_dir(&g.org_id, &g.addon_id)
        .map_err(|e| internal("data dir", format!("{e:?}")))
}

fn job_response(job: tentaflow_protocol::tentanas::NasJob) -> MessageBody {
    tn(P::JobResponse { job })
}

// ----- handlers ---------------------------------------------------------------------

async fn environment(ctx: &HandlerContext, refresh: bool) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    let env = if refresh {
        tentanas::environment::refresh(&g.db).await
    } else {
        tentanas::environment::cached_or_probe(&g.db).await
    }
    .map_err(|e| internal("environment probe", e))?;
    Ok(tn(P::EnvironmentResponse { environment: env }))
}

async fn elevation_plan(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let g = gate_admin(ctx)?;
    let plan = tentanas::elevation::plan(&staging_dir(&g)?).await;
    Ok(tn(P::ElevationPlanResponse { plan }))
}

async fn elevation_provision(ctx: &HandlerContext, secret: &SudoSecret) -> Result<MessageBody, ProtocolError> {
    let g = gate_admin(ctx)?;
    let token = token(secret);
    let staging = staging_dir(&g)?;
    let job = tentanas::jobs::spawn(&g.db, "elevation.provision", "helper", &g.user_id, move |h| {
        tentanas::jobs::provision_helper(h, token, staging)
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response(job))
}

async fn elevation_arm(ctx: &HandlerContext, secret: &SudoSecret, ttl_secs: u32) -> Result<MessageBody, ProtocolError> {
    let g = gate_admin(ctx)?;
    let elevation = tentanas::elevation::arm(&g.db, secret.0.clone(), ttl_secs)
        .await
        .map_err(|e| ProtocolError::new(ProtocolErrorCode::PolicyDenied, e.to_string()))?;
    if tentanas::elevation::mode(&g.db) != tentanas::elevation::Mode::Helper {
        tentanas::elevation::set_mode(&g.db, tentanas::elevation::Mode::Interactive)
            .map_err(|e| internal("settings", e))?;
    }
    if ttl_secs > 0 {
        store::set_setting(&g.db, tentanas::elevation::SETTING_TTL, &ttl_secs.to_string())
            .map_err(|e| internal("settings", e))?;
    }
    tentanas::disks::request_smart_refresh();
    Ok(tn(P::ElevationResponse { elevation }))
}

async fn elevation_disarm(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let g = gate_admin(ctx)?;
    tentanas::elevation::disarm();
    Ok(tn(P::ElevationResponse {
        elevation: tentanas::elevation::status(&g.db).await,
    }))
}

async fn elevation_remove(ctx: &HandlerContext, secret: &SudoSecret) -> Result<MessageBody, ProtocolError> {
    let g = gate_admin(ctx)?;
    let token = token(secret);
    let job = tentanas::jobs::spawn(&g.db, "elevation.remove", "helper", &g.user_id, move |h| {
        tentanas::jobs::remove_helper(h, token)
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response(job))
}

async fn packages_install(
    ctx: &HandlerContext,
    feature_id: &str,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate_admin(ctx)?;
    let Some(manager) = tentanas::environment::detect_package_manager() else {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "no supported package manager on this node",
        ));
    };
    let Some(packages) = tentanas::environment::packages_for(feature_id, manager) else {
        return Err(ProtocolError::bad_request(format!("unknown feature '{feature_id}'")));
    };
    let explicit = secret.map(token);
    let manager: PackageManager = manager;
    let job = tentanas::jobs::spawn(&g.db, "packages.install", feature_id, &g.user_id, move |h| {
        tentanas::jobs::install_packages(h, manager, packages, explicit)
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response(job))
}

fn jobs_list(ctx: &HandlerContext, limit: u32) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    let jobs = store::list_jobs(&g.db, if limit == 0 { 50 } else { limit })
        .map_err(|e| internal("jobs", e))?;
    Ok(tn(P::JobsListResponse { jobs }))
}

fn job_get(ctx: &HandlerContext, job_id: &str) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    let job = store::job(&g.db, job_id)
        .map_err(|e| internal("jobs", e))?
        .ok_or_else(|| ProtocolError::not_found("job not found"))?;
    Ok(job_response(job))
}

fn job_cancel(ctx: &HandlerContext, job_id: &str) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_ADMIN)?;
    if !tentanas::jobs::cancel(job_id) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "job is not running on this node",
        ));
    }
    let job = store::job(&g.db, job_id)
        .map_err(|e| internal("jobs", e))?
        .ok_or_else(|| ProtocolError::not_found("job not found"))?;
    Ok(job_response(job))
}

async fn disks_list(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    let (mut disks, telemetry) = tentanas::disks::snapshot();
    if disks.is_empty() && telemetry.sampled_at.is_none() {
        // First request before the sampler's first tick (or on a node where
        // the sampler could not start): answer from a fresh scan.
        if let Err(e) = tentanas::disks::refresh_inventory(&g.db).await {
            tracing::warn!("tentanas: on-demand inventory failed: {e}");
        }
        disks = tentanas::disks::snapshot().0;
    }
    Ok(tn(P::DisksListResponse { disks, telemetry }))
}

fn disk_get(ctx: &HandlerContext, disk_id: &str) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    let disk = tentanas::disks::disk(disk_id).ok_or_else(|| ProtocolError::not_found("disk not found"))?;
    let row = store::disk_row(&g.db, disk_id).map_err(|e| internal("disk", e))?;
    let (mut attributes, self_tests) = row
        .as_ref()
        .and_then(|r| r.smart_json.as_deref())
        .and_then(|j| serde_json::from_str::<serde_json::Value>(j).ok())
        .map(|doc| (tentanas::disks::smart_attributes(&doc), tentanas::disks::smart_self_tests(&doc)))
        .unwrap_or_default();
    // Trend column: the week-old raw value for the counters the app samples.
    for a in attributes.iter_mut() {
        let column = match a.id {
            5 => "reallocated",
            197 => "pending",
            199 => "crc_errors",
            187 | 198 => "media_errors",
            _ => continue,
        };
        a.raw_week_ago = store::attribute_week_ago(&g.db, disk_id, column).unwrap_or(None);
    }
    let since = (chrono::Utc::now() - chrono::Duration::hours(24))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let history = store::samples_since(&g.db, disk_id, &since).map_err(|e| internal("samples", e))?;
    let alerts = store::alerts_for_subject(&g.db, "disk", disk_id).map_err(|e| internal("alerts", e))?;
    let telemetry = tentanas::disks::snapshot().1;
    Ok(tn(P::DiskGetResponse {
        disk,
        attributes,
        self_tests,
        history,
        alerts,
        telemetry,
    }))
}

async fn disk_smart_test(
    ctx: &HandlerContext,
    disk_id: &str,
    kind: &str,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_POOLS)?;
    let kind = match kind {
        "short" => SelfTestKind::Short,
        "long" => SelfTestKind::Long,
        other => return Err(ProtocolError::bad_request(format!("unknown self-test kind '{other}'"))),
    };
    let device = tentanas::disks::device_path(disk_id)
        .ok_or_else(|| ProtocolError::not_found("disk not found"))?;
    let explicit = secret.map(token);
    let job = tentanas::jobs::spawn(&g.db, "smart.test", disk_id, &g.user_id, move |h| {
        tentanas::jobs::smart_self_test(h, device, kind, explicit)
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response(job))
}

async fn disk_locate(ctx: &HandlerContext, disk_id: &str, enable: bool) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_POOLS)?;
    let disk = tentanas::disks::disk(disk_id).ok_or_else(|| ProtocolError::not_found("disk not found"))?;
    let command = HelperCommand::Locate {
        device: disk.path.clone(),
        enable,
    };
    match tentanas::broker::run_privileged(&g.db, &command, None, Duration::from_secs(20)).await {
        Ok((out, _)) if out.success() => Ok(tn(P::DiskLocateResponse {
            method: "ledctl".to_string(),
            active: enable,
            detail: String::new(),
        })),
        Ok((out, _)) => Ok(tn(P::DiskLocateResponse {
            method: "ledctl".to_string(),
            active: false,
            detail: out.stderr.trim().lines().next().unwrap_or("ledctl failed").to_string(),
        })),
        // No enclosure LED path: the UI shows serial/WWN large instead.
        Err(BrokerError::ToolMissing(_)) => Ok(tn(P::DiskLocateResponse {
            method: "none".to_string(),
            active: false,
            detail: format!("S/N {} · WWN {}", disk.serial, disk.wwn.unwrap_or_default()),
        })),
        Err(e) => Err(broker_error("locate", e)),
    }
}

fn alerts_list(ctx: &HandlerContext, include_acked: bool) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    let alerts = store::list_alerts(&g.db, include_acked).map_err(|e| internal("alerts", e))?;
    Ok(tn(P::AlertsListResponse { alerts }))
}

fn alert_ack(ctx: &HandlerContext, alert_id: &str) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    if !store::ack_alert(&g.db, alert_id).map_err(|e| internal("alerts", e))? {
        return Err(ProtocolError::not_found("alert not found or already acknowledged"));
    }
    alerts_list(ctx, false)
}

// ----- dispatcher -------------------------------------------------------------------

#[handler(variant = "TentaNasBody", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn tentanas_dispatch(req: &MessageBody, ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::TentaNasBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected TentaNasBody")),
    };
    match payload {
        P::NodesListRequest {} => {
            let g = gate(ctx, PERM_READ)?;
            Ok(tn(P::NodesListResponse {
                local_node_id: ctx.state.local_node_id.to_string(),
                nodes: tentanas::fleet::nodes(ctx, &g.addon_id),
            }))
        }
        P::EnvironmentRequest { refresh } => environment(ctx, *refresh).await,
        P::ElevationPlanRequest {} => elevation_plan(ctx).await,
        P::ElevationProvisionRequest { sudo_password } => elevation_provision(ctx, sudo_password).await,
        P::ElevationArmRequest { sudo_password, ttl_secs } => elevation_arm(ctx, sudo_password, *ttl_secs).await,
        P::ElevationDisarmRequest {} => elevation_disarm(ctx).await,
        P::ElevationRemoveRequest { sudo_password } => elevation_remove(ctx, sudo_password).await,
        P::PackagesInstallRequest { feature_id, sudo_password } => {
            packages_install(ctx, feature_id, sudo_password.as_ref()).await
        }
        P::JobsListRequest { limit } => jobs_list(ctx, *limit),
        P::JobGetRequest { job_id } => job_get(ctx, job_id),
        P::JobCancelRequest { job_id } => job_cancel(ctx, job_id),
        P::DisksListRequest {} => disks_list(ctx).await,
        P::DiskGetRequest { disk_id } => disk_get(ctx, disk_id),
        P::DiskSmartTestRequest { disk_id, kind, sudo_password } => {
            disk_smart_test(ctx, disk_id, kind, sudo_password.as_ref()).await
        }
        P::DiskLocateRequest { disk_id, enable } => disk_locate(ctx, disk_id, *enable).await,
        P::AlertsListRequest { include_acked } => alerts_list(ctx, *include_acked),
        P::AlertAckRequest { alert_id } => alert_ack(ctx, alert_id),
        P::NodesListResponse { .. }
        | P::EnvironmentResponse { .. }
        | P::ElevationPlanResponse { .. }
        | P::ElevationResponse { .. }
        | P::JobsListResponse { .. }
        | P::JobResponse { .. }
        | P::DisksListResponse { .. }
        | P::DiskGetResponse { .. }
        | P::DiskLocateResponse { .. }
        | P::AlertsListResponse { .. } => Err(ProtocolError::bad_request("response variant sent as request")),
    }
}
