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
use tentaflow_protocol::tentanas::{
    NasDataset, NasDisk, NasPropertyChange, NasSchedule, NasScheduleRow, SudoSecret,
    TentaNasPayload as P,
};
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

/// Operations that destroy data or move a pool between nodes: the pool
/// permission AND the org Admin role (§4 red path). The matrix can delegate
/// `nas.pools.manage`, it cannot delegate the role.
fn gate_destructive(ctx: &HandlerContext) -> Result<Gate, ProtocolError> {
    super::app_gate::require_app_permission(ctx, tentanas::PACKAGE_ID, PERM_POOLS)?;
    gate_admin(ctx)
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
    let job = tentanas::jobs::spawn(&g.db, "elevation_provision", "helper", &g.user_id, move |h| {
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
    let job = tentanas::jobs::spawn(&g.db, "elevation_remove", "helper", &g.user_id, move |h| {
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
    let job = tentanas::jobs::spawn(&g.db, "packages_install", feature_id, &g.user_id, move |h| {
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
    let job = tentanas::jobs::spawn(&g.db, "smart_test", disk_id, &g.user_id, move |h| {
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

// ----- pools ------------------------------------------------------------------------

/// The 24 h window every history answer covers.
fn since_24h() -> String {
    (chrono::Utc::now() - chrono::Duration::hours(24))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Disks by id, refusing anything this node does not have. `require_free`
/// guards the destructive paths: a pool is never built on a disk that already
/// belongs to a pool, an array or the running system.
fn disks_by_id(disk_ids: &[String], require_free: bool) -> Result<Vec<NasDisk>, ProtocolError> {
    let mut out = Vec::with_capacity(disk_ids.len());
    for id in disk_ids {
        let disk = tentanas::disks::disk(id)
            .ok_or_else(|| ProtocolError::not_found(format!("disk '{id}' not found on this node")))?;
        if require_free && disk.role != "free" {
            return Err(ProtocolError::bad_request(format!(
                "disk {} is not free: {}",
                disk.name, disk.role
            )));
        }
        out.push(disk);
    }
    if out.is_empty() {
        return Err(ProtocolError::bad_request("no disks selected"));
    }
    Ok(out)
}

/// Stable `/dev/disk/by-id` paths of the picked disks — what `zpool create`,
/// `add` and `replace` receive so a kernel rename cannot scramble the pool.
fn device_paths(disks: &[NasDisk]) -> Vec<String> {
    disks
        .iter()
        .map(|d| tentanas::zfs::stable_device_path(&d.name))
        .collect()
}

/// Datasets of a pool (empty = the whole node) with their snapshot totals and
/// the automatic-snapshot schedule the Tasks tab configured.
async fn datasets_view(g: &Gate, pool: &str) -> Result<Vec<NasDataset>, ProtocolError> {
    let mut datasets = tentanas::datasets::list(pool)
        .await
        .map_err(|e| broker_error("datasets", e))?;
    let snapshots = tentanas::snapshots::list(pool, "", false).await.unwrap_or_default();
    let schedules = store::list_snapshot_schedules(&g.db).unwrap_or_default();
    for d in datasets.iter_mut() {
        let mine = snapshots.iter().filter(|s| s.dataset == d.name);
        let (count, used) = mine.fold((0u32, 0u64), |(c, u), s| (c + 1, u + s.used_bytes));
        d.snapshot_count = count;
        d.snapshot_used_bytes = used;
        d.snapshot_schedule = schedules.iter().find(|s| s.dataset == d.name).cloned().map(|mut s| {
            s.snapshot_count = count;
            s
        });
    }
    Ok(datasets)
}

async fn pool_view(g: &Gate, name: &str) -> Result<MessageBody, ProtocolError> {
    let pool = tentanas::pools::one(&g.db, name)
        .await
        .map_err(|e| broker_error("pool", e))?
        .ok_or_else(|| ProtocolError::not_found("pool not found on this node"))?;
    let properties = tentanas::datasets::pool_properties(name)
        .await
        .map_err(|e| broker_error("pool properties", e))?;
    let datasets = datasets_view(g, name).await?;
    let alerts = store::alerts_for_subject(&g.db, "pool", name).map_err(|e| internal("alerts", e))?;
    let history =
        store::pool_samples_since(&g.db, name, &since_24h()).map_err(|e| internal("samples", e))?;
    Ok(tn(P::PoolGetResponse {
        pool,
        properties,
        datasets,
        alerts,
        history,
    }))
}

async fn pools_list(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    let pools = tentanas::pools::collect(&g.db)
        .await
        .map_err(|e| broker_error("pools", e))?;
    let free_disks = tentanas::disks::snapshot()
        .0
        .into_iter()
        .filter(|d| d.role == "free")
        .collect();
    Ok(tn(P::PoolsListResponse { pools, free_disks }))
}

fn pool_plan(ctx: &HandlerContext, disk_ids: &[String]) -> Result<MessageBody, ProtocolError> {
    gate(ctx, PERM_READ)?;
    let disks = disks_by_id(disk_ids, false)?;
    let (options, warnings, smallest_disk_bytes) = tentanas::pools::plan(&disks);
    Ok(tn(P::PoolPlanResponse {
        options,
        warnings,
        smallest_disk_bytes,
    }))
}

#[allow(clippy::too_many_arguments)]
async fn pool_create(
    ctx: &HandlerContext,
    req: PoolCreateArgs<'_>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_POOLS)?;
    // The name is checked before anything is spawned: a job that fails on its
    // first argument is a worse answer than a refused request.
    tentanas_helper::validate_pool_name(req.name)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let disks = disks_by_id(req.disk_ids, true)?;
    let vdevs = tentanas::pools::vdev_groups(
        tentanas_helper::VdevRole::Data,
        req.layout,
        &device_paths(&disks),
    )
    .map_err(|e| broker_error("layout", e))?;
    let command = HelperCommand::ZpoolCreate {
        pool: req.name.to_string(),
        vdevs,
        ashift: req.ashift,
        autotrim: req.autotrim,
        compression: req.compression.to_string(),
        encryption: req.encryption,
        mountpoint: format!("/mnt/{}", req.name),
    };
    // Resolve once here so a bad property or device is a bad_request, not a
    // job that dies on its first line.
    command
        .resolve()
        .map_err(|e| broker_error("zpool create", catalog_error(e)))?;
    let key = req
        .encryption
        .then(|| tentanas::pools::KeyForNewRoot {
            cipher: ctx.state.settings_cipher.clone(),
            addon_id: g.addon_id.clone(),
            dataset: req.name.to_string(),
            material: tentanas::keystore::generate(),
        });
    let explicit = req.sudo_password.map(token);
    let job = tentanas::jobs::spawn(&g.db, "pool_create", req.name, &g.user_id, move |h| {
        tentanas::pools::create_job(h, command, key, explicit)
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response(job))
}

/// `PoolCreateRequest` without the protocol's borrow shape, so the handler
/// signature stays readable.
struct PoolCreateArgs<'a> {
    name: &'a str,
    layout: &'a str,
    disk_ids: &'a [String],
    compression: &'a str,
    encryption: bool,
    ashift: u32,
    autotrim: bool,
    sudo_password: Option<&'a SudoSecret>,
}

fn catalog_error(e: tentanas_helper::CatalogError) -> BrokerError {
    match e {
        tentanas_helper::CatalogError::InvalidArgument(d) => BrokerError::InvalidArgument(d),
        tentanas_helper::CatalogError::ToolMissing(t) => BrokerError::ToolMissing(t),
    }
}

/// Spawns a one-command pool job and answers with it.
fn spawn_pool_job(
    g: &Gate,
    kind: &str,
    subject: &str,
    command: HelperCommand,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    command
        .resolve()
        .map_err(|e| broker_error(kind, catalog_error(e)))?;
    let explicit = secret.map(token);
    let job = tentanas::jobs::spawn(&g.db, kind, subject, &g.user_id, move |h| {
        tentanas::pools::command_job(h, command, explicit)
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response(job))
}

/// A destroy job: the command, then the encryption keys of what it removed.
fn spawn_destroy_job(
    g: &Gate,
    kind: &str,
    subject: &str,
    command: HelperCommand,
    subtree: bool,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    command
        .resolve()
        .map_err(|e| broker_error(kind, catalog_error(e)))?;
    let explicit = secret.map(token);
    let addon_id = g.addon_id.clone();
    let name = subject.to_string();
    let job = tentanas::jobs::spawn(&g.db, kind, subject, &g.user_id, move |h| {
        tentanas::datasets::destroy_job(h, command, addon_id, name, subtree, explicit)
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response(job))
}

/// Runs one catalog command right now and maps its exit into a protocol
/// error. For the operations the UI expects to complete before the answer.
async fn run_now(
    g: &Gate,
    scope: &str,
    command: &HelperCommand,
    secret: Option<&SudoSecret>,
) -> Result<(), ProtocolError> {
    let explicit = secret.map(token);
    let (out, _) = tentanas::broker::run_privileged(
        &g.db,
        command,
        explicit.as_deref(),
        Duration::from_secs(120),
    )
    .await
    .map_err(|e| broker_error(scope, e))?;
    if out.success() {
        Ok(())
    } else {
        Err(ProtocolError::bad_request(
            out.stderr
                .trim()
                .lines()
                .next()
                .unwrap_or("the command failed")
                .to_string(),
        ))
    }
}

/// Retype gate: the backend re-checks what the dialog made the admin type.
fn require_confirm(name: &str, confirm_name: &str) -> Result<(), ProtocolError> {
    if name == confirm_name {
        Ok(())
    } else {
        Err(ProtocolError::bad_request(
            "the typed confirmation does not match the name",
        ))
    }
}

async fn pool_destroy(
    ctx: &HandlerContext,
    name: &str,
    confirm_name: &str,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate_destructive(ctx)?;
    require_confirm(name, confirm_name)?;
    let answer = spawn_destroy_job(
        &g,
        "pool_destroy",
        name,
        HelperCommand::ZpoolDestroy {
            pool: name.to_string(),
        },
        true,
        secret,
    )?;
    // The schedule of a pool that no longer exists would keep firing.
    let _ = store::delete_scrub_schedule(&g.db, name);
    Ok(answer)
}

async fn pool_scrub(
    ctx: &HandlerContext,
    name: &str,
    action: &str,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_POOLS)?;
    tentanas_helper::validate_pool_name(name)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    if action == "start" {
        // The job follows the scrub to its end; cancelling it stops the scrub.
        let pool = name.to_string();
        let explicit = secret.map(token);
        let job = tentanas::jobs::spawn(&g.db, "pool_scrub", name, &g.user_id, move |h| {
            tentanas::pools::scrub_job(h, pool, explicit)
        })
        .map_err(|e| internal("job", e))?;
        return Ok(job_response(job));
    }
    let scrub_action = match action {
        "pause" => tentanas_helper::ScrubAction::Pause,
        "resume" => tentanas_helper::ScrubAction::Resume,
        "stop" => tentanas_helper::ScrubAction::Stop,
        other => {
            return Err(ProtocolError::bad_request(format!(
                "unknown scrub action '{other}'"
            )))
        }
    };
    run_now(
        &g,
        "scrub",
        &HelperCommand::ZpoolScrub {
            pool: name.to_string(),
            action: scrub_action,
        },
        secret,
    )
    .await?;
    pool_view(&g, name).await
}

async fn pool_import_scan(
    ctx: &HandlerContext,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate_destructive(ctx)?;
    // The scan opens every disk on the host, so unlike the other pool reads it
    // needs root.
    let explicit = secret.map(token);
    let (out, _) = tentanas::broker::run_privileged(
        &g.db,
        &HelperCommand::ZpoolImportScan {},
        explicit.as_deref(),
        Duration::from_secs(120),
    )
    .await
    .map_err(|e| broker_error("import scan", e))?;
    // `zpool import` exits 1 when it finds nothing at all — an empty list, not
    // a failure.
    Ok(tn(P::PoolImportScanResponse {
        pools: tentanas::pools::parse_import_scan(&out.stdout),
    }))
}

async fn pool_add_vdev(
    ctx: &HandlerContext,
    name: &str,
    role: &str,
    layout: &str,
    disk_ids: &[String],
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_POOLS)?;
    let vdev_role = tentanas_helper::VdevRole::parse(role)
        .ok_or_else(|| ProtocolError::bad_request(format!("unknown vdev role '{role}'")))?;
    if vdev_role == tentanas_helper::VdevRole::Data && layout.is_empty() {
        return Err(ProtocolError::bad_request("a data vdev needs a layout"));
    }
    let disks = disks_by_id(disk_ids, true)?;
    let devices = device_paths(&disks);
    // Cache and spare groups are always bare leaves; a SLOG mirrors when the
    // caller picked two disks and named no layout of its own.
    let layout = if layout.is_empty() {
        match vdev_role {
            tentanas_helper::VdevRole::Log if devices.len() == 2 => "mirror",
            _ => "stripe",
        }
    } else {
        layout
    };
    let groups = tentanas::pools::vdev_groups(vdev_role, layout, &devices)
        .map_err(|e| broker_error("layout", e))?;
    // A mirror layout becomes one `zpool add` per pair, but the admin asked
    // for one growth, so it is one job with one log.
    let commands: Vec<HelperCommand> = groups
        .into_iter()
        .map(|vdev| HelperCommand::ZpoolAdd {
            pool: name.to_string(),
            vdev,
        })
        .collect();
    for command in &commands {
        command
            .resolve()
            .map_err(|e| broker_error("zpool add", catalog_error(e)))?;
    }
    let explicit = secret.map(token);
    let job = tentanas::jobs::spawn(&g.db, "pool_add_vdev", name, &g.user_id, move |h| async move {
        for command in commands {
            tentanas::jobs::run_step(&h, &command, explicit.as_deref(), Duration::from_secs(600))
                .await?;
        }
        drop(explicit);
        h.progress(100);
        Ok(())
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response(job))
}

async fn pool_device_state(
    ctx: &HandlerContext,
    name: &str,
    device: &str,
    action: &str,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_POOLS)?;
    let command = match action {
        "offline" => HelperCommand::ZpoolOffline {
            pool: name.to_string(),
            device: device.to_string(),
        },
        "online" => HelperCommand::ZpoolOnline {
            pool: name.to_string(),
            device: device.to_string(),
        },
        "clear" => HelperCommand::ZpoolClear {
            pool: name.to_string(),
            device: device.to_string(),
        },
        other => {
            return Err(ProtocolError::bad_request(format!(
                "unknown device action '{other}'"
            )))
        }
    };
    run_now(&g, "device state", &command, secret).await?;
    pool_view(&g, name).await
}

async fn pool_set_properties(
    ctx: &HandlerContext,
    name: &str,
    changes: &[NasPropertyChange],
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_POOLS)?;
    for change in changes {
        if change.inherit {
            return Err(ProtocolError::bad_request(
                "pool properties have no parent to inherit from",
            ));
        }
        run_now(
            &g,
            "pool property",
            &HelperCommand::ZpoolSet {
                pool: name.to_string(),
                property: change.name.clone(),
                value: change.value.clone(),
            },
            secret,
        )
        .await?;
    }
    pool_view(&g, name).await
}

async fn scrub_schedule_set(
    ctx: &HandlerContext,
    name: &str,
    enabled: bool,
    schedule: &NasSchedule,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_POOLS)?;
    tentanas_helper::validate_pool_name(name)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let next = enabled
        .then(|| tentanas::scheduler::next_run_utc(schedule, chrono::Local::now()))
        .flatten();
    if enabled && next.is_none() {
        return Err(ProtocolError::bad_request(format!(
            "unknown schedule cadence '{}'",
            schedule.every
        )));
    }
    store::set_scrub_schedule(&g.db, name, enabled, schedule, next.as_deref())
        .map_err(|e| internal("schedules", e))?;
    pool_view(&g, name).await
}

// ----- datasets ---------------------------------------------------------------------

async fn dataset_view(g: &Gate, name: &str) -> Result<MessageBody, ProtocolError> {
    let mut dataset = tentanas::datasets::get(name)
        .await
        .map_err(|e| broker_error("dataset", e))?
        .ok_or_else(|| ProtocolError::not_found("dataset not found on this node"))?;
    let snapshots = tentanas::snapshots::list("", name, false)
        .await
        .map_err(|e| broker_error("snapshots", e))?;
    dataset.snapshot_count = snapshots.len() as u32;
    dataset.snapshot_used_bytes = snapshots.iter().map(|s| s.used_bytes).sum();
    dataset.snapshot_schedule = store::list_snapshot_schedules(&g.db)
        .unwrap_or_default()
        .into_iter()
        .find(|s| s.dataset == name)
        .map(|mut s| {
            s.snapshot_count = snapshots.len() as u32;
            s
        });
    let properties = tentanas::datasets::properties(name)
        .await
        .map_err(|e| broker_error("dataset properties", e))?;
    Ok(tn(P::DatasetGetResponse {
        dataset,
        properties,
        snapshots,
    }))
}

async fn dataset_create(
    ctx: &HandlerContext,
    req: &P,
) -> Result<MessageBody, ProtocolError> {
    let P::DatasetCreateRequest {
        name,
        kind,
        compression,
        block_size,
        quota_bytes,
        volsize_bytes,
        thin,
        atime,
        sync,
        encryption,
        mountpoint,
        sudo_password,
    } = req
    else {
        return Err(ProtocolError::bad_request("expected DatasetCreateRequest"));
    };
    let g = gate(ctx, PERM_POOLS)?;
    tentanas_helper::validate_dataset_name(name)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let dataset_kind = match kind.as_str() {
        "filesystem" => tentanas_helper::DatasetKind::Filesystem,
        "volume" => tentanas_helper::DatasetKind::Volume,
        other => {
            return Err(ProtocolError::bad_request(format!(
                "unknown dataset kind '{other}'"
            )))
        }
    };
    let is_volume = dataset_kind == tentanas_helper::DatasetKind::Volume;
    let mut properties = Vec::new();
    let mut push = |k: &str, v: String| properties.push((k.to_string(), v));
    if !compression.is_empty() {
        push("compression", compression.clone());
    }
    if !block_size.is_empty() {
        push(
            if is_volume { "volblocksize" } else { "recordsize" },
            block_size.clone(),
        );
    }
    if *quota_bytes > 0 && !is_volume {
        push("quota", quota_bytes.to_string());
    }
    if !atime.is_empty() {
        push("atime", atime.clone());
    }
    if !sync.is_empty() {
        push("sync", sync.clone());
    }
    if !mountpoint.is_empty() {
        push("mountpoint", mountpoint.clone());
    }
    let command = HelperCommand::ZfsCreate {
        name: name.clone(),
        kind: dataset_kind,
        volsize: if is_volume {
            if *volsize_bytes == 0 {
                return Err(ProtocolError::bad_request("a zvol needs a volsize"));
            }
            volsize_bytes.to_string()
        } else {
            String::new()
        },
        sparse: is_volume && *thin,
        properties,
        encryption: *encryption,
    };
    let explicit = sudo_password.as_ref().map(token);
    if *encryption {
        let key = tentanas::keystore::generate();
        let (out, _) = tentanas::broker::run_privileged_with_key(
            &g.db,
            &command,
            &key,
            explicit.as_deref(),
            Duration::from_secs(300),
        )
        .await
        .map_err(|e| broker_error("dataset create", e))?;
        if !out.success() {
            return Err(ProtocolError::bad_request(
                out.stderr.trim().lines().next().unwrap_or("zfs create failed").to_string(),
            ));
        }
        // Only a dataset that exists gets a key: a stored key for a dataset
        // that was never created is indistinguishable from a real one.
        tentanas::keystore::put(&ctx.state.settings_cipher, &g.addon_id, name, &key)
            .map_err(|e| internal("keystore", e))?;
    } else {
        run_now(&g, "dataset create", &command, sudo_password.as_ref()).await?;
    }
    dataset_view(&g, name).await
}

async fn dataset_set_properties(
    ctx: &HandlerContext,
    name: &str,
    changes: &[NasPropertyChange],
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_POOLS)?;
    for change in changes {
        let command = if change.inherit {
            HelperCommand::ZfsInherit {
                name: name.to_string(),
                property: change.name.clone(),
            }
        } else {
            HelperCommand::ZfsSet {
                name: name.to_string(),
                property: change.name.clone(),
                value: change.value.clone(),
            }
        };
        run_now(&g, "dataset property", &command, secret).await?;
    }
    dataset_view(&g, name).await
}

async fn dataset_destroy(
    ctx: &HandlerContext,
    name: &str,
    confirm_name: &str,
    recursive: bool,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate_destructive(ctx)?;
    require_confirm(name, confirm_name)?;
    let answer = spawn_destroy_job(
        &g,
        "dataset_destroy",
        name,
        HelperCommand::ZfsDestroy {
            name: name.to_string(),
            recursive,
        },
        recursive,
        secret,
    )?;
    // A schedule that snapshots a dataset that is gone would fail every tick.
    if let Some(schedule) = store::list_snapshot_schedules(&g.db)
        .unwrap_or_default()
        .into_iter()
        .find(|s| s.dataset == name)
    {
        let _ = store::delete_snapshot_schedule(&g.db, &schedule.schedule_id);
    }
    Ok(answer)
}

async fn dataset_key(
    ctx: &HandlerContext,
    name: &str,
    action: &str,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_POOLS)?;
    tentanas_helper::validate_dataset_name(name)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    match action {
        "load" => {
            let key = tentanas::keystore::get(&ctx.state.settings_cipher, &g.addon_id, name)
                .map_err(|e| internal("keystore", e))?
                .ok_or_else(|| {
                    ProtocolError::not_found(format!("no key for '{name}' in this node's keystore"))
                })?;
            let explicit = secret.map(token);
            let (out, _) = tentanas::broker::run_privileged_with_key(
                &g.db,
                &HelperCommand::ZfsLoadKey {
                    dataset: name.to_string(),
                },
                &key,
                explicit.as_deref(),
                Duration::from_secs(120),
            )
            .await
            .map_err(|e| broker_error("load-key", e))?;
            if !out.success() {
                return Err(ProtocolError::bad_request(
                    out.stderr.trim().lines().next().unwrap_or("zfs load-key failed").to_string(),
                ));
            }
        }
        "unload" => {
            run_now(
                &g,
                "unload-key",
                &HelperCommand::ZfsUnloadKey {
                    dataset: name.to_string(),
                },
                secret,
            )
            .await?;
        }
        other => {
            return Err(ProtocolError::bad_request(format!(
                "unknown key action '{other}'"
            )))
        }
    }
    dataset_view(&g, name).await
}

async fn dataset_mount(
    ctx: &HandlerContext,
    name: &str,
    action: &str,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_POOLS)?;
    let command = match action {
        "mount" => HelperCommand::ZfsMount {
            dataset: name.to_string(),
        },
        "unmount" => HelperCommand::ZfsUnmount {
            dataset: name.to_string(),
        },
        other => {
            return Err(ProtocolError::bad_request(format!(
                "unknown mount action '{other}'"
            )))
        }
    };
    run_now(&g, "mount", &command, secret).await?;
    dataset_view(&g, name).await
}

// ----- snapshots --------------------------------------------------------------------

async fn snapshots_list(
    ctx: &HandlerContext,
    pool: &str,
    dataset: &str,
    recursive: bool,
    origin: &str,
    limit: u32,
) -> Result<MessageBody, ProtocolError> {
    gate(ctx, PERM_READ)?;
    let all = tentanas::snapshots::list(pool, dataset, recursive)
        .await
        .map_err(|e| broker_error("snapshots", e))?;
    let filtered: Vec<_> = all
        .into_iter()
        .filter(|s| origin.is_empty() || s.origin == origin)
        .collect();
    let total = filtered.len() as u32;
    let total_used_bytes = filtered.iter().map(|s| s.used_bytes).sum();
    let limit = if limit == 0 { 500 } else { limit } as usize;
    Ok(tn(P::SnapshotsListResponse {
        snapshots: filtered.into_iter().take(limit).collect(),
        total,
        total_used_bytes,
    }))
}

async fn snapshot_create(
    ctx: &HandlerContext,
    dataset: &str,
    short_name: &str,
    recursive: bool,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_POOLS)?;
    let short_name = if short_name.is_empty() {
        chrono::Local::now().format("%Y%m%d-%H%M%S").to_string()
    } else {
        short_name.to_string()
    };
    run_now(
        &g,
        "snapshot",
        &HelperCommand::ZfsSnapshot {
            snapshot: format!("{dataset}@{short_name}"),
            recursive,
        },
        secret,
    )
    .await?;
    snapshots_list(ctx, "", dataset, recursive, "", 0).await
}

async fn snapshot_destroy(
    ctx: &HandlerContext,
    names: &[String],
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_POOLS)?;
    if names.is_empty() {
        return Err(ProtocolError::bad_request("no snapshots given"));
    }
    for name in names {
        tentanas_helper::validate_snapshot_name(name)
            .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    }
    let subject = names.first().cloned().unwrap_or_default();
    let list = names.to_vec();
    let explicit = secret.map(token);
    let job = tentanas::jobs::spawn(&g.db, "snapshot_destroy", &subject, &g.user_id, move |h| {
        async move {
            for name in list {
                let command = HelperCommand::ZfsDestroy {
                    name,
                    recursive: false,
                };
                tentanas::jobs::run_step(&h, &command, explicit.as_deref(), Duration::from_secs(300))
                    .await?;
            }
            drop(explicit);
            h.progress(100);
            Ok(())
        }
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response(job))
}

async fn snapshot_rollback(
    ctx: &HandlerContext,
    name: &str,
    confirm_name: &str,
    destroy_newer: bool,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_POOLS)?;
    require_confirm(name, confirm_name)?;
    spawn_pool_job(
        &g,
        "snapshot_rollback",
        name,
        HelperCommand::ZfsRollback {
            snapshot: name.to_string(),
            destroy_newer,
        },
        secret,
    )
}

async fn snapshot_clone(
    ctx: &HandlerContext,
    name: &str,
    target: &str,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_POOLS)?;
    run_now(
        &g,
        "clone",
        &HelperCommand::ZfsClone {
            snapshot: name.to_string(),
            target: target.to_string(),
        },
        secret,
    )
    .await?;
    dataset_view(&g, target).await
}

async fn snapshot_schedule_set(
    ctx: &HandlerContext,
    req: &P,
) -> Result<MessageBody, ProtocolError> {
    let P::SnapshotScheduleSetRequest {
        schedule_id,
        dataset,
        enabled,
        recursive,
        schedule,
        keep_frequent,
        keep_hourly,
        keep_daily,
        keep_weekly,
        keep_monthly,
    } = req
    else {
        return Err(ProtocolError::bad_request("expected SnapshotScheduleSetRequest"));
    };
    let g = gate(ctx, PERM_POOLS)?;
    tentanas_helper::validate_dataset_name(dataset)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let next = enabled
        .then(|| tentanas::scheduler::next_run_utc(schedule, chrono::Local::now()))
        .flatten();
    if *enabled && next.is_none() {
        return Err(ProtocolError::bad_request(format!(
            "unknown schedule cadence '{}'",
            schedule.every
        )));
    }
    let mut row = tentaflow_protocol::tentanas::NasSnapshotSchedule {
        schedule_id: if schedule_id.is_empty() {
            uuid::Uuid::now_v7().to_string()
        } else {
            schedule_id.clone()
        },
        dataset: dataset.clone(),
        enabled: *enabled,
        recursive: *recursive,
        schedule: schedule.clone(),
        keep_frequent: *keep_frequent,
        keep_hourly: *keep_hourly,
        keep_daily: *keep_daily,
        keep_weekly: *keep_weekly,
        keep_monthly: *keep_monthly,
        last_run_at: None,
        next_run_at: next.clone(),
        snapshot_count: 0,
    };
    store::upsert_snapshot_schedule(&g.db, &row, next.as_deref())
        .map_err(|e| internal("schedules", e))?;
    // Read back so an existing schedule keeps its id and its last run.
    if let Some(stored) = store::list_snapshot_schedules(&g.db)
        .unwrap_or_default()
        .into_iter()
        .find(|s| s.dataset == *dataset)
    {
        row = stored;
    }
    row.snapshot_count = tentanas::snapshots::list("", dataset, *recursive)
        .await
        .map(|s| s.len() as u32)
        .unwrap_or(0);
    Ok(tn(P::SnapshotScheduleResponse { schedule: row }))
}

fn snapshot_schedule_delete(
    ctx: &HandlerContext,
    schedule_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_POOLS)?;
    let existing = store::snapshot_schedule(&g.db, schedule_id)
        .map_err(|e| internal("schedules", e))?
        .ok_or_else(|| ProtocolError::not_found("snapshot schedule not found"))?;
    store::delete_snapshot_schedule(&g.db, schedule_id).map_err(|e| internal("schedules", e))?;
    // The deleted schedule is echoed back disabled, so the UI can show what
    // it just removed without a second round trip.
    Ok(tn(P::SnapshotScheduleResponse {
        schedule: tentaflow_protocol::tentanas::NasSnapshotSchedule {
            enabled: false,
            next_run_at: None,
            ..existing
        },
    }))
}

async fn snapshot_schedules_list(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    let mut schedules =
        store::list_snapshot_schedules(&g.db).map_err(|e| internal("schedules", e))?;
    let snapshots = tentanas::snapshots::list("", "", false).await.unwrap_or_default();
    for s in schedules.iter_mut() {
        s.snapshot_count = snapshots.iter().filter(|x| x.dataset == s.dataset).count() as u32;
    }
    Ok(tn(P::SnapshotSchedulesListResponse { schedules }))
}

// ----- schedules (Tasks tab) ---------------------------------------------------------

fn schedules_list(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    let mut rows = Vec::new();
    for row in store::list_scrub_schedules(&g.db).map_err(|e| internal("schedules", e))? {
        rows.push(NasScheduleRow {
            kind: "scrub".to_string(),
            subject: row.pool,
            enabled: row.enabled,
            schedule: row.schedule,
            last_run_at: row.last_run_at,
            last_result: row.last_result,
            next_run_at: row.next_run_at,
        });
    }
    for s in store::list_snapshot_schedules(&g.db).map_err(|e| internal("schedules", e))? {
        let last_result = store::snapshot_schedule_result(&g.db, &s.schedule_id).unwrap_or_default();
        rows.push(NasScheduleRow {
            kind: "snapshot".to_string(),
            subject: s.dataset,
            enabled: s.enabled,
            schedule: s.schedule,
            last_run_at: s.last_run_at,
            last_result,
            next_run_at: s.next_run_at,
        });
    }
    let smart = store::smart_schedule(&g.db).map_err(|e| internal("schedules", e))?;
    for (kind, schedule, last, next) in [
        ("smart_short", &smart.short, &smart.last_short_at, &smart.next_short_at),
        ("smart_long", &smart.long, &smart.last_long_at, &smart.next_long_at),
    ] {
        rows.push(NasScheduleRow {
            kind: kind.to_string(),
            subject: "all disks".to_string(),
            enabled: smart.enabled,
            schedule: schedule.clone(),
            last_run_at: last.clone(),
            last_result: String::new(),
            next_run_at: next.clone(),
        });
    }
    Ok(tn(P::SchedulesListResponse { rows, smart }))
}

fn smart_schedule_set(
    ctx: &HandlerContext,
    enabled: bool,
    short: &NasSchedule,
    long: &NasSchedule,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_POOLS)?;
    let now = chrono::Local::now();
    let mut smart = store::smart_schedule(&g.db).map_err(|e| internal("schedules", e))?;
    let next_short = tentanas::scheduler::next_run_utc(short, now);
    let next_long = tentanas::scheduler::next_run_utc(long, now);
    if enabled && (next_short.is_none() || next_long.is_none()) {
        return Err(ProtocolError::bad_request(
            "unknown schedule cadence for the SMART tests",
        ));
    }
    smart.enabled = enabled;
    smart.short = short.clone();
    smart.long = long.clone();
    smart.next_short_at = enabled.then_some(next_short).flatten();
    smart.next_long_at = enabled.then_some(next_long).flatten();
    store::set_smart_schedule(&g.db, &smart).map_err(|e| internal("schedules", e))?;
    Ok(tn(P::SmartScheduleResponse { smart }))
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

        // ----- pools -----
        P::PoolsListRequest {} => pools_list(ctx).await,
        P::PoolGetRequest { name } => {
            let g = gate(ctx, PERM_READ)?;
            pool_view(&g, name).await
        }
        P::PoolPlanRequest { disk_ids } => pool_plan(ctx, disk_ids),
        P::PoolCreateRequest {
            name,
            layout,
            disk_ids,
            compression,
            encryption,
            ashift,
            autotrim,
            sudo_password,
        } => {
            pool_create(
                ctx,
                PoolCreateArgs {
                    name,
                    layout,
                    disk_ids,
                    compression,
                    encryption: *encryption,
                    ashift: *ashift,
                    autotrim: *autotrim,
                    sudo_password: sudo_password.as_ref(),
                },
            )
            .await
        }
        P::PoolDestroyRequest {
            name,
            confirm_name,
            sudo_password,
        } => pool_destroy(ctx, name, confirm_name, sudo_password.as_ref()).await,
        P::PoolScrubRequest {
            name,
            action,
            sudo_password,
        } => pool_scrub(ctx, name, action, sudo_password.as_ref()).await,
        P::PoolExportRequest {
            name,
            force,
            sudo_password,
        } => {
            let g = gate_destructive(ctx)?;
            let answer = spawn_pool_job(
                &g,
                "pool_export",
                name,
                HelperCommand::ZpoolExport {
                    pool: name.clone(),
                    force: *force,
                },
                sudo_password.as_ref(),
            )?;
            let _ = store::delete_scrub_schedule(&g.db, name);
            Ok(answer)
        }
        P::PoolImportScanRequest { sudo_password } => {
            pool_import_scan(ctx, sudo_password.as_ref()).await
        }
        P::PoolImportRequest {
            guid,
            new_name,
            force,
            sudo_password,
        } => {
            let g = gate_destructive(ctx)?;
            spawn_pool_job(
                &g,
                "pool_import",
                if new_name.is_empty() { guid } else { new_name },
                HelperCommand::ZpoolImport {
                    guid: guid.clone(),
                    new_name: new_name.clone(),
                    force: *force,
                },
                sudo_password.as_ref(),
            )
        }
        P::PoolAddVdevRequest {
            name,
            role,
            layout,
            disk_ids,
            sudo_password,
        } => pool_add_vdev(ctx, name, role, layout, disk_ids, sudo_password.as_ref()).await,
        P::PoolExpandVdevRequest {
            name,
            vdev_id,
            disk_id,
            sudo_password,
        } => {
            let g = gate(ctx, PERM_POOLS)?;
            let disks = disks_by_id(std::slice::from_ref(disk_id), true)?;
            spawn_pool_job(
                &g,
                "pool_expand_vdev",
                name,
                HelperCommand::ZpoolAttach {
                    pool: name.clone(),
                    vdev: vdev_id.clone(),
                    device: device_paths(&disks).remove(0),
                },
                sudo_password.as_ref(),
            )
        }
        P::PoolRemoveVdevRequest {
            name,
            vdev_id,
            sudo_password,
        } => {
            let g = gate(ctx, PERM_POOLS)?;
            spawn_pool_job(
                &g,
                "pool_remove_vdev",
                name,
                HelperCommand::ZpoolRemove {
                    pool: name.clone(),
                    device: vdev_id.clone(),
                },
                sudo_password.as_ref(),
            )
        }
        P::PoolReplaceDiskRequest {
            name,
            old,
            disk_id,
            sudo_password,
        } => {
            let g = gate(ctx, PERM_POOLS)?;
            // The replacement may be a hot spare of this very pool, so it is
            // not required to be free.
            let disks = disks_by_id(std::slice::from_ref(disk_id), false)?;
            let command = HelperCommand::ZpoolReplace {
                pool: name.clone(),
                old: old.clone(),
                new: device_paths(&disks).remove(0),
            };
            command
                .resolve()
                .map_err(|e| broker_error("zpool replace", catalog_error(e)))?;
            let explicit = sudo_password.as_ref().map(token);
            let pool = name.clone();
            let job = tentanas::jobs::spawn(&g.db, "pool_replace", name, &g.user_id, move |h| {
                tentanas::pools::replace_job(h, pool, command, explicit)
            })
            .map_err(|e| internal("job", e))?;
            Ok(job_response(job))
        }
        P::PoolDeviceStateRequest {
            name,
            device,
            action,
            sudo_password,
        } => pool_device_state(ctx, name, device, action, sudo_password.as_ref()).await,
        P::PoolSetPropertiesRequest {
            name,
            changes,
            sudo_password,
        } => pool_set_properties(ctx, name, changes, sudo_password.as_ref()).await,
        P::ScrubScheduleSetRequest {
            name,
            enabled,
            schedule,
        } => scrub_schedule_set(ctx, name, *enabled, schedule).await,

        // ----- datasets -----
        P::DatasetsListRequest { pool } => {
            let g = gate(ctx, PERM_READ)?;
            Ok(tn(P::DatasetsListResponse {
                datasets: datasets_view(&g, pool).await?,
            }))
        }
        P::DatasetGetRequest { name } => {
            let g = gate(ctx, PERM_READ)?;
            dataset_view(&g, name).await
        }
        P::DatasetCreateRequest { .. } => dataset_create(ctx, payload).await,
        P::DatasetSetPropertiesRequest {
            name,
            changes,
            sudo_password,
        } => dataset_set_properties(ctx, name, changes, sudo_password.as_ref()).await,
        P::DatasetDestroyRequest {
            name,
            confirm_name,
            recursive,
            sudo_password,
        } => dataset_destroy(ctx, name, confirm_name, *recursive, sudo_password.as_ref()).await,
        P::DatasetKeyRequest {
            name,
            action,
            sudo_password,
        } => dataset_key(ctx, name, action, sudo_password.as_ref()).await,
        P::DatasetMountRequest {
            name,
            action,
            sudo_password,
        } => dataset_mount(ctx, name, action, sudo_password.as_ref()).await,

        // ----- snapshots -----
        P::SnapshotsListRequest {
            pool,
            dataset,
            recursive,
            origin,
            limit,
        } => snapshots_list(ctx, pool, dataset, *recursive, origin, *limit).await,
        P::SnapshotCreateRequest {
            dataset,
            short_name,
            recursive,
            sudo_password,
        } => snapshot_create(ctx, dataset, short_name, *recursive, sudo_password.as_ref()).await,
        P::SnapshotDestroyRequest {
            names,
            sudo_password,
        } => snapshot_destroy(ctx, names, sudo_password.as_ref()).await,
        P::SnapshotRollbackRequest {
            name,
            confirm_name,
            destroy_newer,
            sudo_password,
        } => snapshot_rollback(ctx, name, confirm_name, *destroy_newer, sudo_password.as_ref()).await,
        P::SnapshotCloneRequest {
            name,
            target,
            sudo_password,
        } => snapshot_clone(ctx, name, target, sudo_password.as_ref()).await,
        P::SnapshotScheduleSetRequest { .. } => snapshot_schedule_set(ctx, payload).await,
        P::SnapshotScheduleDeleteRequest { schedule_id } => {
            snapshot_schedule_delete(ctx, schedule_id)
        }
        P::SnapshotSchedulesListRequest {} => snapshot_schedules_list(ctx).await,

        // ----- schedules -----
        P::SchedulesListRequest {} => schedules_list(ctx),
        P::SmartScheduleSetRequest {
            enabled,
            short,
            long,
        } => smart_schedule_set(ctx, *enabled, short, long),

        P::NodesListResponse { .. }
        | P::EnvironmentResponse { .. }
        | P::ElevationPlanResponse { .. }
        | P::ElevationResponse { .. }
        | P::JobsListResponse { .. }
        | P::JobResponse { .. }
        | P::DisksListResponse { .. }
        | P::DiskGetResponse { .. }
        | P::DiskLocateResponse { .. }
        | P::AlertsListResponse { .. }
        | P::PoolsListResponse { .. }
        | P::PoolGetResponse { .. }
        | P::PoolPlanResponse { .. }
        | P::PoolImportScanResponse { .. }
        | P::DatasetsListResponse { .. }
        | P::DatasetGetResponse { .. }
        | P::SnapshotsListResponse { .. }
        | P::SnapshotScheduleResponse { .. }
        | P::SnapshotSchedulesListResponse { .. }
        | P::SchedulesListResponse { .. }
        | P::SmartScheduleResponse { .. } => {
            Err(ProtocolError::bad_request("response variant sent as request"))
        }
    }
}
