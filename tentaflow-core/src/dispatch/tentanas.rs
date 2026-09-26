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
    NasDataset, NasDisk, NasDiskWipePlan, NasPropertyChange, NasSchedule, NasScheduleRow,
    SudoSecret, TentaNasPayload as P,
};
use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode};
use tentanas_helper::{HelperCommand, PackageManager, SelfTestKind};

use super::HandlerContext;
use crate::db::DbPool;
use crate::profiling::collectors::elevation::ElevationToken;
use crate::tentanas::{self, broker::BrokerError, db as store, CodedText};
use tentanas_helper::elastic::{ElasticOwner, ElasticCreateSpec, ElasticDiskSpec, ElasticFilesystem};

const PERM_READ: &str = "nas.read";
const PERM_POOLS: &str = "nas.pools.manage";
const PERM_SHARES: &str = "nas.shares.manage";
const PERM_TARGETS: &str = "nas.targets.manage";
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
        // The operator can act on this one, so it has to say what to do. The
        // raw reason stays in the sentence: "not configured" and "sudo
        // rejected the password" lead to the same screen but not to the same
        // fix, and only the node knows which of them happened.
        //
        // The remedy names no tab on purpose. A node that answers this cannot
        // run a privileged command, and the panel replaces ALL its tabs —
        // Environment included — with the channel setup step, so "open the
        // Environment tab and start the wizard" pointed at a card that is not
        // on screen in exactly the state that produces this error.
        //
        // Polish, like every other operator-facing refusal this file writes
        // ("Macierz nie istnieje w tej instancji", "Nieprawidłowy zestaw
        // dysków Elastic"). The two arms below read English because they
        // forward `BrokerError`'s own `#[error]` text verbatim from
        // broker.rs — a pre-existing split across one error surface. It is
        // real, and it is not silently converted here: those strings have
        // other callers, so flipping them is its own change.
        BrokerError::Unarmed(why) => ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            format!(
                "Kanał uprawnień systemowych nie jest dostępny ({why}) — \
                 otwórz TentaNas na tym nodzie i dokończ krok konfiguracji \
                 kanału, który pojawi się zamiast zakładek."
            ),
        ),
        BrokerError::ToolMissing(tool) => {
            ProtocolError::new(ProtocolErrorCode::NotAvailable, format!("{tool} is not installed"))
        }
        // The version gate. Its text already names the versions and the remedy
        // (it is written for the admin, in Polish, by `broker::version_gate`),
        // so it is forwarded whole rather than turned into "tentanas … failed"
        // by the fallthrough — this is the one refusal an admin fixes by
        // re-running provisioning, and they can only do that if they are told.
        BrokerError::HelperVersion(why) => {
            ProtocolError::new(ProtocolErrorCode::NotAvailable, why)
        }
        BrokerError::InvalidArgument(d) => ProtocolError::bad_request(d),
        other => internal(scope, other),
    }
}

/// A privileged read whose error crossed an `anyhow` boundary on the way up.
///
/// WHY this exists: `internal()` on such a path turns "the channel was never
/// configured" — the one cause an operator can fix — into `tentanas {scope}
/// failed`, which is what a freshly installed instance answered every Elastic
/// request with. `anyhow` keeps the concrete error in the chain, so the
/// actionable half is recoverable; anything that is not a `BrokerError` is a
/// genuine internal fault and stays one.
fn privileged_error(scope: &str, error: anyhow::Error) -> ProtocolError {
    match error.downcast::<BrokerError>() {
        Ok(broker) => broker_error(scope, broker),
        Err(other) => internal(scope, other),
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

/// Where a red-path request came from. `Direct` is the dashboard asking;
/// `Approved` is the SAME request replayed by the four-eyes approval that
/// released it, and it never parks again (§5.10).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Origin {
    Direct,
    Approved,
}

/// The caller as the approval flow sees them. The permission checker is the
/// one the gate already used, so "who could approve" can never disagree with
/// "who may approve".
fn actor<'a>(
    ctx: &'a HandlerContext,
    g: &'a Gate,
) -> Result<tentanas::approvals::Actor<'a>, ProtocolError> {
    let checker = ctx
        .state
        .permission_checker
        .as_deref()
        .ok_or_else(|| internal("approvals", "permission checker not wired"))?;
    Ok(tentanas::approvals::Actor {
        main_db: &ctx.state.db,
        nas_db: &g.db,
        checker,
        org_id: &g.org_id,
        addon_id: &g.addon_id,
        node_id: &ctx.state.local_node_id,
        user_id: &g.user_id,
    })
}

/// Parks one red-path request and answers with the row to watch. Nothing ran.
///
/// `detail` is what the approver decides on: a code with parameters the
/// approver's screen words (`approvals.detail_<code>`) and the node's English
/// sentence for the audit row, the forwarded alert and the tooltip.
fn park(
    ctx: &HandlerContext,
    g: &Gate,
    operation: &str,
    subject: &str,
    detail: CodedText,
    payload: &P,
) -> Result<MessageBody, ProtocolError> {
    park_shown(ctx, g, operation, subject, subject, detail, payload)
}

/// `park` for a stored subject that is not a name (a config import's node
/// id): the alert's English title names `shown_subject` instead
/// (`approvals::park_shown`).
fn park_shown(
    ctx: &HandlerContext,
    g: &Gate,
    operation: &str,
    subject: &str,
    shown_subject: &str,
    detail: CodedText,
    payload: &P,
) -> Result<MessageBody, ProtocolError> {
    let a = actor(ctx, g)?;
    let approval = tentanas::approvals::park_shown(&a, operation, subject, shown_subject, detail, payload)
        .map_err(|e| internal("approvals", e))?;
    Ok(tn(P::ApprovalPendingResponse { approval }))
}

fn approval_error(e: tentanas::approvals::ApprovalError) -> ProtocolError {
    use tentanas::approvals::ApprovalError as E;
    match e {
        E::NotFound => ProtocolError::not_found(e.to_string()),
        E::OwnRequest => ProtocolError::new(ProtocolErrorCode::PolicyDenied, e.to_string()),
        E::Closed(_) | E::Expired => ProtocolError::bad_request(e.to_string()),
    }
}

fn token(secret: &SudoSecret) -> Arc<ElevationToken> {
    Arc::new(ElevationToken::new_sudo(secret.0.clone()))
}

fn staging_dir(g: &Gate) -> Result<std::path::PathBuf, ProtocolError> {
    crate::addon::fs_sandbox::addon_data_dir(&g.org_id, &g.addon_id)
        .map_err(|e| internal("data dir", format!("{e:?}")))
}

/// A job's `started_by` and an approval's `requested_by` / `decided_by` hold a
/// USER UUID, and the record must keep it that way: an account can be renamed,
/// and an audit trail that moves with the name proves nothing about who acted.
/// It is not what belongs on screen — the mockups name a person ("Anna ·
/// Admin") and a bare `0191f2c0-…` is the one thing an admin cannot recognise.
/// So the id is resolved HERE, at the read boundary, for display only.
///
/// The map answers for every id it was given except a system author
/// (`is_system_author`), which passes through unchanged so the scheduler's own
/// `started_by = "scheduler"` stays readable. Everything else is decided by
/// `names_for_org`: a member of the asking organisation by name, anyone else
/// as an empty author.
fn display_names(
    ctx: &HandlerContext,
    ids: &[String],
) -> std::collections::HashMap<String, String> {
    // Distinct, and without the system authors: 50 jobs by one admin are one
    // lookup, and "scheduler" is nobody's account to look up.
    let authors: std::collections::BTreeSet<String> = ids
        .iter()
        .filter(|id| !is_system_author(id))
        .cloned()
        .collect();
    if authors.is_empty() {
        return std::collections::HashMap::new();
    }
    let asking_org = ctx.org_context.as_ref().map(|o| o.org_id.as_str()).unwrap_or("");
    let members = org_members_among(ctx, asking_org, &authors);
    // Only the members' accounts are read: nobody else's name is ever needed.
    let member_ids: Vec<String> = members.iter().cloned().collect();
    let accounts =
        crate::db::repository::lookup_user_names(&ctx.state.db, &member_ids).unwrap_or_default();
    names_for_org(&authors, accounts, &members)
}

/// An author that is the node itself rather than an account: the scheduler's
/// unattended runs (`scheduler::STARTED_BY`) and a boot-time Elastic Restore
/// (`elastic::STARTED_BY_STARTUP`). Every other author is the `user_id` of
/// the request's org context.
fn is_system_author(id: &str) -> bool {
    matches!(id, tentanas::scheduler::STARTED_BY | tentanas::elastic::STARTED_BY_STARTUP)
}

/// Which of `ids` belong to `org_id`, in ONE query over the distinct ids (per
/// chunk of the platform's name-lookup size; a job list holds a handful of
/// authors). A failed read answers "none of them": the cost is a blank
/// author, never another tenant's name.
///
/// A membership row is the ONLY way an account belongs to an organisation:
/// the org admin is a member whose `role_id` is the admin role, and an org
/// context is resolved from the memberships (`rbac::resolve_org_context`
/// refuses a user without one), so everyone who can start a job in this org
/// has the row this looks for.
fn org_members_among(
    ctx: &HandlerContext,
    org_id: &str,
    ids: &std::collections::BTreeSet<String>,
) -> std::collections::BTreeSet<String> {
    let mut members = std::collections::BTreeSet::new();
    if org_id.is_empty() {
        return members;
    }
    let Ok(conn) = ctx.state.db.read() else {
        return members;
    };
    let ids: Vec<&String> = ids.iter().collect();
    for chunk in ids.chunks(500) {
        let placeholders = (2..chunk.len() + 2).map(|i| format!("?{i}")).collect::<Vec<_>>().join(", ");
        let sql = format!(
            "SELECT user_id FROM org_memberships WHERE org_id = ?1 AND user_id IN ({placeholders})"
        );
        let mut params: Vec<&dyn rusqlite::ToSql> = vec![&org_id];
        params.extend(chunk.iter().map(|id| *id as &dyn rusqlite::ToSql));
        let found = conn.prepare(&sql).and_then(|mut stmt| {
            let rows = stmt
                .query_map(params.as_slice(), |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<String>>>();
            rows
        });
        match found {
            Ok(rows) => members.extend(rows),
            Err(e) => {
                tracing::warn!("tentanas: org membership lookup failed: {e}");
                return std::collections::BTreeSet::new();
            }
        }
    }
    members
}

/// The shown name of every author, under the tenant rule: a member of the
/// asking organisation by its name, and ANY other id — an account of another
/// organisation of this node, a deleted account, an id known only on another
/// node — as an empty author. The node is one TentaNas instance for every
/// tenant on it, so the job list, the job modal and the approvals list can
/// carry rows another tenant's user started; that user's name is not this
/// tenant's to read, and neither is its raw id (the screen would put it in
/// the author tooltip). Empty rather than the id, and the screen shows "—".
///
/// A member without a usable name (no account row any more, or an account
/// with neither a display name nor a username) is left out of the map, so
/// its id — this organisation's own identifier — passes through as before.
fn names_for_org(
    authors: &std::collections::BTreeSet<String>,
    mut accounts: std::collections::HashMap<String, crate::db::repository::UserNameRow>,
    members: &std::collections::BTreeSet<String>,
) -> std::collections::HashMap<String, String> {
    authors
        .iter()
        .filter_map(|id| {
            if !members.contains(id) {
                return Some((id.clone(), String::new()));
            }
            let row = accounts.remove(id)?;
            let name = if row.display_name.is_empty() {
                row.username
            } else {
                row.display_name
            };
            (!name.is_empty()).then(|| (id.clone(), name))
        })
        .collect()
}

fn name_jobs(
    ctx: &HandlerContext,
    db: Option<&DbPool>,
    jobs: &mut [tentaflow_protocol::tentanas::NasJob],
) {
    let ids: Vec<String> = jobs.iter().map(|j| j.started_by.clone()).collect();
    let names = display_names(ctx, &ids);
    for job in jobs.iter_mut() {
        if let Some(name) = names.get(&job.started_by) {
            job.started_by = name.clone();
        }
        // A job's `subject` is a NAME for every kind but one: a SMART test is
        // spawned on the `disk_id`, because that string is also the key the
        // spawn refuses a second job on — two call sites depend on it, so the
        // stored subject stays the id and only the shown one becomes `sdg`.
        // n15 reads "SMART long: sdd".
        //
        // Named by the one disk-naming rule (`disks::shown_disk_name`): live
        // first, then — for a disk that has since left the inventory — the
        // name it was last seen under, and never the id. A job row is a
        // record of what an operation ran on, and the last-known name is the
        // name that disk had — sent FLAGGED (`subject_last_known`), because
        // the naming rule shows a remembered name only marked as such. The id
        // is kept only when the node never knew any name at all, and every
        // job renderer passes the subject through `jobSubject`
        // (`www/js/modules/tentanas/tasks.js`), which drops a machine-id shape
        // (`wwn-…`, `sn-…`, `dev-…`) from the visible text and keeps it as the
        // tooltip.
        if job.kind == "config_import" {
            job.subject = config_import_subject(&job.subject, |id| tentanas::fleet::node_name(ctx, id));
        }
        // A multi-disk job's lines, each named by the same rule, and its
        // subject rebuilt from them — never the stored ids.
        if job.kind == tentanas::db::SMART_BATCH_KIND {
            if let Some(db) = db {
                job.disks = job_disk_lines(db, &job.job_id);
                // The test kind and "all" stay; a chosen set is named by its
                // lines as they are now.
                let (kind, rest) = job.subject.split_once('|').unwrap_or(("short", ""));
                if rest != "all" && !job.disks.is_empty() {
                    let names: Vec<String> = job.disks.iter().map(|d| d.name.clone()).collect();
                    job.subject = tentanas::db::smart_batch_subject(kind == "long", Some(&names));
                }
            }
        }
        if job.kind == "smart_test" {
            if let Some((name, last_known)) = smart_subject_name(&job.subject, |id| {
                match db {
                    Some(db) => tentanas::disks::shown_disk_name(db, id, None),
                    None => tentanas::disks::pick_shown_name(tentanas::disks::disk_name(id), None, || None),
                }
            }) {
                job.subject = name;
                job.subject_last_known = last_known;
            }
        }
    }
    // A job's error and log are an operation's own words, and an Elastic
    // operation's may carry a branch path whose slot is not a disk name
    // (R2-3). Loaded only when some line has one.
    if let Some(db) = db {
        let has_path = |t: &str| t.contains(tentanas_helper::elastic::BRANCH_ROOT);
        if jobs.iter().any(|j| j.error.as_deref().is_some_and(has_path) || j.log.iter().any(|l| has_path(l))) {
            let names = tentanas::elastic::BranchNames::load(db);
            for job in jobs.iter_mut() {
                if let Some(error) = job.error.as_mut() {
                    *error = names.name(error);
                }
                for line in job.log.iter_mut() {
                    *line = names.name(line);
                }
            }
        }
    }
}

/// Alert and approval sentences through the same branch-path naming as the
/// job lines (`name_jobs`): a stored sentence may carry a slot directory.
fn name_branch_paths<'t>(db: &DbPool, texts: impl IntoIterator<Item = &'t mut String>) {
    let mut texts: Vec<&mut String> = texts.into_iter().collect();
    if !texts.iter().any(|t| t.contains(tentanas_helper::elastic::BRANCH_ROOT)) {
        return;
    }
    let names = tentanas::elastic::BranchNames::load(db);
    for text in texts.iter_mut() {
        **text = names.name(text);
    }
}

/// The shown subject of a config import. An export with no `node_name` gives
/// the import its `node_id` as the subject — stored that way in the parked
/// request, its alert and the job, and kept so, like a job's author. It is
/// resolved HERE, at the read boundary, through the fleet's names
/// (`fleet::node_name`, via `name_of`). A subject in a node-id shape the
/// fleet has no name for is shown as nothing, never as the id: the screens
/// then name the operation alone (owner's rule: no ids on screen).
fn config_import_subject(subject: &str, name_of: impl FnOnce(&str) -> String) -> String {
    let name = name_of(subject);
    if !name.trim().is_empty() {
        return name;
    }
    let id_shaped = subject.len() >= 32 && subject.bytes().all(|b| b.is_ascii_hexdigit());
    if id_shaped {
        String::new()
    } else {
        subject.to_string()
    }
}

/// The shown subject of a SMART job spawned on `disk_id`, with whether it is
/// only REMEMBERED: `(live name, false)`, or `(last-seen name, true)` once the
/// disk has left the inventory. `None` keeps the stored subject (a disk the
/// node never named).
/// The lines of a multi-disk job for the screen: each disk by its live name,
/// else the name it was last seen under (flagged), else the name stored when
/// the job started — flagged too, it is no longer the device as it is now.
fn job_disk_lines(db: &DbPool, job_id: &str) -> Vec<tentaflow_protocol::tentanas::NasJobDisk> {
    tentanas::db::job_disks(db, job_id)
        .unwrap_or_default()
        .into_iter()
        .map(|row| {
            let (name, last_known) = match tentanas::disks::shown_disk_name(db, &row.disk_id, None) {
                tentanas::disks::ShownDiskName::Live(name) => (name, false),
                tentanas::disks::ShownDiskName::LastKnown(name) => (name, true),
                tentanas::disks::ShownDiskName::Unknown => (row.name.clone(), true),
            };
            tentaflow_protocol::tentanas::NasJobDisk {
                name,
                last_known,
                state: row.state,
                progress_pct: row.progress_pct,
                reasons: row.reasons,
            }
        })
        .collect()
}

fn smart_subject_name(
    disk_id: &str,
    shown: impl FnOnce(&str) -> tentanas::disks::ShownDiskName,
) -> Option<(String, bool)> {
    match shown(disk_id) {
        tentanas::disks::ShownDiskName::Live(name) => Some((name, false)),
        tentanas::disks::ShownDiskName::LastKnown(name) => Some((name, true)),
        tentanas::disks::ShownDiskName::Unknown => None,
    }
}

fn job_response(ctx: &HandlerContext, job: tentaflow_protocol::tentanas::NasJob) -> MessageBody {
    job_response_in(ctx, None, job)
}

/// `job_response` with this instance's database at hand, so a SMART job on a
/// disk that has left the inventory is titled by its last-known name.
fn job_response_in(
    ctx: &HandlerContext,
    db: Option<&DbPool>,
    job: tentaflow_protocol::tentanas::NasJob,
) -> MessageBody {
    let mut job = job;
    name_jobs(ctx, db, std::slice::from_mut(&mut job));
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

/// The name the Environment tab shows next to "provisioned by". The account's
/// display name when the platform knows one — the point is that an admin
/// reading the node months later can tell who armed it. Nothing when the
/// platform knows no name: an account id is not a name, and the tab and the
/// job log never show one (owner's rule).
fn admin_display_name(ctx: &HandlerContext, g: &Gate) -> String {
    crate::db::repository::lookup_user_names(&ctx.state.db, std::slice::from_ref(&g.user_id))
        .ok()
        .and_then(|m| m.get(&g.user_id).cloned())
        .map(|row| {
            if row.display_name.is_empty() {
                row.username
            } else {
                row.display_name
            }
        })
        .filter(|n| !n.is_empty())
        .unwrap_or_default()
}

async fn elevation_provision(ctx: &HandlerContext, secret: &SudoSecret) -> Result<MessageBody, ProtocolError> {
    let g = gate_admin(ctx)?;
    let token = token(secret);
    let staging = staging_dir(&g)?;
    let admin = admin_display_name(ctx, &g);
    let job = tentanas::jobs::spawn(&g.db, "elevation_provision", "helper", &g.user_id, None, None, move |h| {
        tentanas::jobs::provision_helper(h, token, staging, admin)
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response(ctx, job))
}

fn elevation_catalog(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    // Read permission, not admin: "what could this app do as root" is exactly
    // what an operator without the channel needs to see before granting it.
    gate(ctx, PERM_READ)?;
    let commands = tentanas_helper::catalog()
        .into_iter()
        .map(|c| tentaflow_protocol::tentanas::NasHelperCommand {
            name: c.name,
            description: c.description.to_string(),
            tool: c.tool.to_string(),
            builtin: c.builtin,
            needs_stdin: c.needs_stdin,
        })
        .collect();
    Ok(tn(P::ElevationCatalogResponse { commands }))
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
    tentanas::disks::request_summary_refresh();
    Ok(tn(P::ElevationResponse { elevation }))
}

async fn elevation_disarm(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let g = gate_admin(ctx)?;
    tentanas::elevation::disarm();
    tentanas::disks::request_summary_refresh();
    Ok(tn(P::ElevationResponse {
        elevation: tentanas::elevation::status(&g.db).await,
    }))
}

async fn elevation_remove(ctx: &HandlerContext, secret: &SudoSecret) -> Result<MessageBody, ProtocolError> {
    let g = gate_admin(ctx)?;
    let token = token(secret);
    let job = tentanas::jobs::spawn(&g.db, "elevation_remove", "helper", &g.user_id, None, None, move |h| {
        tentanas::jobs::remove_helper(h, token)
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response(ctx, job))
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
    let job = tentanas::jobs::spawn(&g.db, "packages_install", feature_id, &g.user_id, None, None, move |h| {
        tentanas::jobs::install_packages(h, manager, packages, explicit)
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response(ctx, job))
}

fn jobs_list(ctx: &HandlerContext, limit: u32) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    // Scoped to the caller's organisation: this database is the whole
    // node's, and another tenant's array jobs name that tenant's array.
    let mut jobs = store::list_jobs_for_org(&g.db, org_viewer(ctx, &g), if limit == 0 { 50 } else { limit })
        .map_err(|e| internal("jobs", e))?;
    name_jobs(ctx, Some(&g.db), &mut jobs);
    Ok(tn(P::JobsListResponse { jobs }))
}

fn job_get(ctx: &HandlerContext, job_id: &str) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    let job = store::job_for_org(&g.db, org_viewer(ctx, &g), job_id)
        .map_err(|e| internal("jobs", e))?
        .ok_or_else(|| ProtocolError::not_found("job not found"))?;
    Ok(job_response_in(ctx, Some(&g.db), job))
}

/// The refusal of a cancel request for a job kind whose cancel would not stop
/// its work; worded by the screen from `refusal.job_not_cancellable`.
const JOB_NOT_CANCELLABLE: &str = "refusal:job_not_cancellable";

fn job_cancel(ctx: &HandlerContext, job_id: &str) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_ADMIN)?;
    // Another tenant's job is "not found" here as in `job_get`: an admin of
    // one organisation must not stop another organisation's array work.
    let job = store::job_for_org(&g.db, org_viewer(ctx, &g), job_id)
        .map_err(|e| internal("jobs", e))?
        .ok_or_else(|| ProtocolError::not_found("job not found"))?;
    // Only a kind whose cancel really stops the work (`jobs::user_cancellable`):
    // anything else would read "cancelled" while its command runs on. An
    // accepted Elastic create or restore is one of those, refused with the
    // same code the screen words (critic wave 5, MINOR 13: it used to get a
    // Polish sentence of its own first).
    if !tentanas::jobs::user_cancellable(&job.kind) {
        return Err(ProtocolError::new(ProtocolErrorCode::NotAvailable, JOB_NOT_CANCELLABLE));
    }
    if !tentanas::jobs::cancel(job_id) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "job is not running on this node",
        ));
    }
    Ok(job_response(ctx, job))
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
    // BEFORE the advice: a replacement advice names the disk's `member_of`.
    let own_arrays = own_array_names(&g)?;
    for disk in disks.iter_mut() {
        tentanas::disks::hide_other_org_array(disk, &own_arrays);
    }
    let advice = tentanas::disks::advice(&g.db, &disks);
    Ok(tn(P::DisksListResponse {
        disks,
        telemetry,
        iops_hour_avg: tentanas::disks::iops_hour_avg(),
        advice,
    }))
}

fn disk_get(ctx: &HandlerContext, disk_id: &str) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    let mut disk = tentanas::disks::disk(disk_id).ok_or_else(|| ProtocolError::not_found("disk not found"))?;
    tentanas::disks::hide_other_org_array(&mut disk, &own_array_names(&g)?);
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
    // The disk charts are the node's health record, not a live view: they
    // cover the whole retention window (minutes for the last 48 h, hourly
    // rows before that) and say so, so the frontend labels the axis from the
    // answer instead of assuming a window.
    let since = (chrono::Utc::now() - chrono::Duration::days(i64::from(store::HISTORY_DAYS)))
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let history = store::history_since(&g.db, disk_id, &since).map_err(|e| internal("samples", e))?;
    let alerts = store::alerts_for_subject(&g.db, "disk", disk_id).map_err(|e| internal("alerts", e))?;
    let (all, telemetry) = tentanas::disks::snapshot();
    let spares: Vec<NasDisk> = all.into_iter().filter(|d| d.vdev_role == "spare").collect();
    let advice = tentanas::disks::advice_for(&g.db, &disk, &spares);
    Ok(tn(P::DiskGetResponse {
        disk,
        attributes,
        self_tests,
        history,
        alerts,
        telemetry,
        history_days: store::HISTORY_DAYS,
        advice,
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
    let job = tentanas::jobs::spawn(&g.db, "smart_test", disk_id, &g.user_id, None, None, move |h| {
        tentanas::jobs::smart_self_test(h, device, kind, explicit)
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response(ctx, job))
}

/// Most disks one batch may name — a bound on the request, far above any
/// shelf this product manages.
const SMART_BATCH_MAX_DISKS: usize = 256;

/// One SMART self-test job over several disks (`jobs::smart_self_test_batch`):
/// one request, one job row with a line per disk, one credential, and a stop
/// at the first privilege/credential error instead of one refusal per disk.
async fn disk_smart_test_batch(
    ctx: &HandlerContext,
    disk_ids: &[String],
    kind: &str,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_POOLS)?;
    let kind = match kind {
        "short" => SelfTestKind::Short,
        "long" => SelfTestKind::Long,
        other => return Err(ProtocolError::bad_request(format!("unknown self-test kind '{other}'"))),
    };
    let mut seen = std::collections::HashSet::new();
    let ids: Vec<&String> = disk_ids.iter().filter(|id| seen.insert(id.as_str())).collect();
    if ids.is_empty() {
        return Err(ProtocolError::bad_request("no disk to test"));
    }
    if ids.len() > SMART_BATCH_MAX_DISKS {
        return Err(ProtocolError::bad_request(format!(
            "at most {SMART_BATCH_MAX_DISKS} disks per self-test job"
        )));
    }
    // Every line is named when the job is written: the kernel name now, or the
    // name the node last saw the disk under. A disk the node never knew is
    // refused here rather than written as a line with no name to show.
    let mut disks = Vec::with_capacity(ids.len());
    for id in ids {
        let name = match tentanas::disks::shown_disk_name(&g.db, id, None) {
            tentanas::disks::ShownDiskName::Live(name) | tentanas::disks::ShownDiskName::LastKnown(name) => name,
            tentanas::disks::ShownDiskName::Unknown => return Err(ProtocolError::not_found("disk not found")),
        };
        disks.push((id.clone(), name));
    }
    let names: Vec<String> = disks.iter().map(|(_, name)| name.clone()).collect();
    let subject = tentanas::db::smart_batch_subject(kind == SelfTestKind::Long, Some(&names));
    let explicit = secret.map(token);
    let job = tentanas::jobs::spawn_smart_batch(&g.db, &subject, &g.user_id, &disks, move |h| {
        tentanas::jobs::smart_self_test_batch(h, kind, explicit)
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response_in(ctx, Some(&g.db), job))
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

// ----- clearing a disk --------------------------------------------------------------
//
// One plan, read by BOTH requests. The wipe does not take the plan the browser
// was shown — it reads its own, from a freshly refreshed inventory and the
// node's own journals, and refuses on it. A plan the client could carry back
// would be a decision made at dialog-open time about a device set that moves.

/// The disk, refreshed, with the Elastic journal that claims it. Read-only.
///
/// Privileged: the journals under `/var/lib/tentanas` are `0700 root`, and a
/// disk left over from a DISSOLVED array is claimed by one of them while
/// nothing in the database says so — which is the single most likely reason a
/// disk on a real node reads as `used` with a bare `xfs` signature.
///
/// `orgs_on_node` (from the function of that name) is what turns a disk of
/// another tenant of this node into a refusal that does not name its array.
async fn wipe_plan_of(
    g: &Gate,
    orgs_on_node: &std::collections::BTreeSet<String>,
    disk_id: &str,
    explicit: Option<&ElevationToken>,
) -> Result<NasDiskWipePlan, ProtocolError> {
    tentanas::disks::refresh_inventory(&g.db)
        .await
        .map_err(|e| internal("inventory", e))?;
    let mut disk = tentanas::disks::disk(disk_id).ok_or_else(|| {
        ProtocolError::not_found(disk_gone_message(tentanas::disks::shown_disk_name(
            &g.db, disk_id, None,
        )))
    })?;
    // A member of another organisation's LIVE array: the plan refuses it
    // without the array's name (`ROLE_OTHER_ORG_ARRAY`), where the member arm
    // would have printed it — and the name would reach the dialog and the
    // wipe's own refusal text.
    tentanas::disks::hide_other_org_array(&mut disk, &own_array_names(g)?);
    let journals = tentanas::elastic::journals(&g.db, explicit)
        .await
        .map_err(|e| privileged_error("elastic journals", e))?;
    let claim = tentanas::disks::journal_claim_of(&disk, &journals.arrays, &elastic_owner(g), orgs_on_node);
    Ok(tentanas::disks::plan_wipe(&disk, claim))
}

/// Why a wipe plan cannot be read for a disk the inventory no longer holds,
/// naming the disk by the one naming rule — the dialog toasts this, and the
/// disk id (`wwn-…`) is not a name. The disk is by definition not live here,
/// so a remembered name is said as last-seen.
fn disk_gone_message(shown: tentanas::disks::ShownDiskName) -> String {
    match shown {
        tentanas::disks::ShownDiskName::Live(name) => {
            format!("disk {name} is not in this node's inventory")
        }
        tentanas::disks::ShownDiskName::LastKnown(name) => {
            format!("the disk last seen as {name} is no longer on this node")
        }
        tentanas::disks::ShownDiskName::Unknown => "this disk is no longer on this node".to_string(),
    }
}

/// The display name of a journal owner's instance, for the ONE kind whose
/// name this caller may learn: another instance of the caller's own
/// organisation.
///
/// Everything else answers empty WITHOUT a lookup. The organisation's name is
/// never resolved at all — for this org the admin knows it, and for another
/// org it is another tenant's name, which a node shared by several tenants
/// must not hand to the admin of one of them (OWASP A01). `lookup` is the
/// platform database read, passed in so the rule is testable without one.
fn own_org_instance_name(kind: &str, lookup: impl FnOnce() -> Option<String>) -> String {
    if kind != tentanas::elastic::OWNER_KIND_THIS_ORG {
        return String::new();
    }
    lookup().map(|n| n.trim().to_string()).unwrap_or_default()
}

/// The platform database's display name for an addon instance, when it has one.
fn addon_display_name(ctx: &HandlerContext, addon_id: &str) -> Option<String> {
    crate::db::repository::get_addon(&ctx.state.db, addon_id)
        .ok()
        .flatten()
        .map(|addon| if addon.display_name.trim().is_empty() { addon.name } else { addon.display_name })
}

async fn disk_wipe_plan(
    ctx: &HandlerContext,
    disk_id: &str,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    // Gated exactly like the wipe it precedes: the refusals name this node's
    // pools, arrays and journal owners, and that is not a read for anyone who
    // could not perform the operation anyway.
    let g = gate_destructive(ctx)?;
    let explicit = secret.map(token);
    let mut plan = wipe_plan_of(&g, &orgs_on_node(ctx)?, disk_id, explicit.as_deref()).await?;
    if let Some(claim) = plan.journal_claim.as_mut() {
        claim.owner_instance_name =
            own_org_instance_name(&claim.owner_kind, || addon_display_name(ctx, &claim.owner_addon_id));
    }
    Ok(tn(P::DiskWipePlanResponse { plan }))
}

async fn disk_wipe(
    ctx: &HandlerContext,
    disk_id: &str,
    confirm_device: &str,
    release_journal_array: &str,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate_destructive(ctx)?;
    let explicit = secret.map(token);
    // The node's own plan, read under the same tenant rule the dialog was
    // shown: a disk of another organisation of this node refuses HERE, not
    // merely by the dialog having no claim to acknowledge.
    let plan = wipe_plan_of(&g, &orgs_on_node(ctx)?, disk_id, explicit.as_deref()).await?;
    let disk = tentanas::disks::disk(disk_id)
        .ok_or_else(|| ProtocolError::not_found("Dysk zniknął z inwentarza"))?;
    // Every gate between "an admin clicked" and "a device is opened" lives in
    // one pure function, over the plan this node just read for ITSELF: the
    // retyped device name, the plan's refusals and the separate journal
    // acknowledgement. Nothing a client carried back is consulted.
    let command = tentanas::disks::wipe_command(&plan, &disk, confirm_device, release_journal_array)
        .map_err(|e| match e {
            tentanas::disks::WipeRefusal::BadRequest(detail) => ProtocolError::bad_request(detail),
            tentanas::disks::WipeRefusal::NotAvailable(detail) => {
                ProtocolError::new(ProtocolErrorCode::NotAvailable, detail)
            }
        })?;
    // Validated against the catalog before it becomes a job: a disk with
    // neither a WWN nor a serial cannot be re-resolved after a rename, and the
    // catalog refuses such a command — which must read as a refusal here, not
    // as a job that fails later.
    command
        .plan()
        .map_err(|e| broker_error("disk_wipe", catalog_error(e)))?;
    // A wipe that releases a journal logs the array it released by name, and
    // that array is this organisation's (the plan refuses anyone else's), so
    // the job is this organisation's instead of the node's — from the INSERT
    // that creates the row (`spawn_owned`), never through a later write that
    // could fail and leave a node-wide row naming the array.
    let owner = plan.journal_claim.is_some().then_some(g.org_id.as_str());
    let job = tentanas::jobs::spawn_owned(
        &g.db,
        "disk_wipe",
        &disk.name,
        &g.user_id,
        owner,
        None,
        None,
        move |h| tentanas::disks::wipe_job(h, command, explicit),
    )
    .map_err(|e| internal("job", e))?;
    Ok(job_response(ctx, job))
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
    let mut pool = tentanas::pools::one(&g.db, name)
        .await
        .map_err(|e| broker_error("pool", e))?
        .ok_or_else(|| ProtocolError::not_found("pool not found on this node"))?;
    // The TRIM state costs one extra `zpool status -t`, so the DETAIL view
    // pays for it and the list view does not (§5.10).
    if let Ok(trim) = tentanas::pools::trim_status(name).await {
        pool.trim_state = trim.state;
        pool.trim_progress_pct = trim.pct;
    }
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
    let plan = tentanas::pools::plan(&disks);
    Ok(tn(P::PoolPlanResponse {
        options: plan.options,
        warnings: plan.warnings,
        smallest_disk_bytes: plan.smallest_disk_bytes,
        warning_codes: plan.warning_codes,
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
        .plan()
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
    let job = tentanas::jobs::spawn(&g.db, "pool_create", req.name, &g.user_id, None, None, move |h| {
        tentanas::pools::create_job(h, command, key, explicit)
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response(ctx, job))
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
    ctx: &HandlerContext,
    g: &Gate,
    kind: &str,
    subject: &str,
    command: HelperCommand,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    command
        .plan()
        .map_err(|e| broker_error(kind, catalog_error(e)))?;
    let explicit = secret.map(token);
    let job = tentanas::jobs::spawn(&g.db, kind, subject, &g.user_id, None, None, move |h| {
        tentanas::pools::command_job(h, command, explicit)
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response(ctx, job))
}

/// A destroy job: the command, then the encryption keys of what it removed.
fn spawn_destroy_job(
    ctx: &HandlerContext,
    g: &Gate,
    kind: &str,
    subject: &str,
    command: HelperCommand,
    subtree: bool,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    command
        .plan()
        .map_err(|e| broker_error(kind, catalog_error(e)))?;
    let explicit = secret.map(token);
    let addon_id = g.addon_id.clone();
    let name = subject.to_string();
    let job = tentanas::jobs::spawn(&g.db, kind, subject, &g.user_id, None, None, move |h| {
        tentanas::datasets::destroy_job(h, command, addon_id, name, subtree, explicit)
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response(ctx, job))
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

/// A pool that holds another organisation's shares or block targets is not
/// this organisation's to destroy (owner decision 2026-09-26). The refusal
/// names nobody: which tenant, and what, stays that tenant's business.
const POOL_DESTROY_FOREIGN: &str = "refusal:pool_destroy_foreign_resources";
/// The pool's datasets or the other tenants' rows could not be read, so the
/// check above could not be made — refused, never assumed clear.
const POOL_DESTROY_UNVERIFIED: &str = "refusal:pool_destroy_unverified";

/// The destroy guard: `Ok` when nothing of another organisation lives on
/// `pool`. `mountpoints` is `None` when the pool's datasets could not be
/// listed. Resources of the asking organisation are not looked at here —
/// they follow the existing flow (the dialog lists them, the destroy takes
/// them with it).
fn pool_destroy_guard(
    db: &DbPool,
    org_id: &str,
    pool: &str,
    mountpoints: Option<&[String]>,
) -> Result<(), ProtocolError> {
    let unverified = || ProtocolError::new(ProtocolErrorCode::NotAvailable, POOL_DESTROY_UNVERIFIED);
    let (shares, targets) = store::resources_of_other_orgs(db, org_id).map_err(|_| unverified())?;
    // By dataset and zvol name first: that answer needs no mountpoint, so a
    // pool whose datasets cannot be listed is still refused as FOREIGN when
    // the rows alone say so.
    if tentanas::pools::holds_resources(pool, mountpoints.unwrap_or_default(), &shares, &targets) {
        return Err(ProtocolError::new(ProtocolErrorCode::Conflict, POOL_DESTROY_FOREIGN));
    }
    if mountpoints.is_none() {
        return Err(unverified());
    }
    Ok(())
}

/// `pools::try_resources_lock`, or the coded refusal of a creation that met a
/// pool destroy in progress.
async fn resources_or_refuse(g: &Gate) -> Result<tokio::sync::OwnedMutexGuard<()>, ProtocolError> {
    tentanas::pools::try_resources_lock(&g.db)
        .await
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::Conflict, POOL_DESTROY_IN_PROGRESS))
}

/// See `pools::POOL_DESTROY_IN_PROGRESS` (the literal is here too, where the
/// screen's refusal scan reads the dispatcher's codes).
const POOL_DESTROY_IN_PROGRESS: &str = "refusal:pool_destroy_in_progress";

/// The pool's mountpoints for `pool_destroy_guard`, `None` when its datasets
/// cannot be listed.
async fn pool_mountpoints(pool: &str) -> Option<Vec<String>> {
    tentanas::datasets::list(pool)
        .await
        .ok()
        .map(|datasets| datasets.into_iter().filter_map(|d| d.mountpoint).collect())
}

async fn pool_destroy(
    ctx: &HandlerContext,
    name: &str,
    confirm_name: &str,
    secret: Option<&SudoSecret>,
    origin: Origin,
) -> Result<MessageBody, ProtocolError> {
    let g = gate_destructive(ctx)?;
    require_confirm(name, confirm_name)?;
    tentanas_helper::validate_pool_name(name).map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    // Checked when the request is made AND again when an approved one runs:
    // another tenant may have exported something from the pool in between.
    let mountpoints = pool_mountpoints(name).await;
    pool_destroy_guard(&g.db, &g.org_id, name, mountpoints.as_deref())?;
    if origin == Origin::Direct && tentanas::approvals::required(&actor(ctx, &g)?) {
        return park(
            ctx,
            &g,
            tentanas::approvals::OP_POOL_DESTROY,
            name,
            CodedText::new(
                "pool_destroy",
                &[("pool", name.to_string())],
                format!("destroys the pool '{name}' and every dataset and snapshot on it"),
            ),
            &P::PoolDestroyRequest {
                name: name.to_string(),
                confirm_name: confirm_name.to_string(),
                sudo_password: None,
            },
        );
    }
    // The check above answers the request at once; the job checks again
    // under `resources_lock` and holds it through `zpool destroy`, so no
    // share or target of another organisation can land on the pool between
    // the last check and the destroy.
    let command = HelperCommand::ZpoolDestroy { pool: name.to_string() };
    command.plan().map_err(|e| broker_error("pool_destroy", catalog_error(e)))?;
    let explicit = secret.map(token);
    let (db, org_id, addon_id, pool) = (g.db.clone(), g.org_id.clone(), g.addon_id.clone(), name.to_string());
    let job = tentanas::jobs::spawn(&g.db, "pool_destroy", name, &g.user_id, None, None, move |h| {
        pool_destroy_job(h, db, org_id, addon_id, pool, command, explicit)
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response(ctx, job))
}

/// The body of a pool destroy job: the last check of other organisations'
/// resources and `zpool destroy`, both under `resources_lock`.
async fn pool_destroy_job(
    h: tentanas::jobs::JobHandle,
    db: DbPool,
    org_id: String,
    addon_id: String,
    pool: String,
    command: HelperCommand,
    explicit: Option<Arc<ElevationToken>>,
) -> anyhow::Result<()> {
    let _serialised = tentanas::pools::resources_lock(&db).lock_owned().await;
    let mountpoints = pool_mountpoints(&pool).await;
    pool_destroy_guard(&db, &org_id, &pool, mountpoints.as_deref())
        .map_err(|refusal| anyhow::anyhow!(refusal.message))?;
    tentanas::datasets::destroy_job(h, command, addon_id, pool.clone(), true, explicit).await?;
    // Only now: a destroy the re-check refused, or one that failed, leaves a
    // pool that still needs its scrub and trim schedules (critic wave 9a,
    // R2-MINOR 2).
    let _ = store::delete_pool_schedules(&db, &pool);
    Ok(())
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
        let job = tentanas::jobs::spawn(&g.db, "pool_scrub", name, &g.user_id, None, None, move |h| {
            tentanas::pools::scrub_job(h, pool, explicit)
        })
        .map_err(|e| internal("job", e))?;
        return Ok(job_response(ctx, job));
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
    let pools = importable_pools(&g, secret).await?;
    remember_import_scan(&g.org_id, &pools, std::time::Instant::now());
    Ok(tn(P::PoolImportScanResponse { pools }))
}

/// What `zpool import` offers on this host.
async fn importable_pools(
    g: &Gate,
    secret: Option<&SudoSecret>,
) -> Result<Vec<tentaflow_protocol::tentanas::NasImportablePool>, ProtocolError> {
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
    Ok(tentanas::pools::parse_import_scan(&out.stdout))
}

/// How long the names of the last import scan title an import job.
///
/// A scan and the import it leads to are one sitting of one dialog: minutes,
/// not hours. Past this the name may belong to a pool that was renamed,
/// imported elsewhere or unplugged since, and a job titled by a stale name is
/// worse than one titled by its kind alone.
const IMPORT_SCAN_NAMES_TTL: Duration = Duration::from_secs(15 * 60);

/// guid → name of the importable pools the last scan saw, and when.
type ImportScanNames = (std::time::Instant, std::collections::HashMap<String, String>);

/// The last import scan's names, per organisation, in this process.
///
/// WHY a cache and not a second scan. The import request used to re-run the
/// privileged `zpool import` scan (up to 120 s) BEFORE answering, only to
/// learn the pool's name for the job title — inside the client's own 120 s
/// timeout. A slow scan then showed the admin a timeout while the node went
/// on to import anyway, and the retry failed against the imported pool. The
/// dialog always scans first (it cannot offer a GUID otherwise), so that
/// scan's answer is the name; nothing privileged runs before the reply.
///
/// Keyed by organisation: the scan is a node-wide fact, but what one tenant's
/// admin scanned is not a title for another tenant's jobs. Being in-process
/// is also what makes it per node — each node titles its own imports.
fn import_scan_names() -> &'static std::sync::Mutex<std::collections::HashMap<String, ImportScanNames>> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, ImportScanNames>>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Records what a scan found. The whole answer replaces the previous one: a
/// pool the new scan no longer lists must not keep titling jobs.
fn remember_import_scan(
    org_id: &str,
    pools: &[tentaflow_protocol::tentanas::NasImportablePool],
    at: std::time::Instant,
) {
    let names = pools
        .iter()
        .filter(|pool| !pool.name.is_empty())
        .map(|pool| (pool.guid.clone(), pool.name.clone()))
        .collect();
    if let Ok(mut cache) = import_scan_names().lock() {
        cache.insert(org_id.to_string(), (at, names));
    }
}

/// The subject of an import job: the new name when the admin gave one,
/// otherwise the NAME the last scan gave this GUID, and when neither is known
/// — no scan in this process, an expired one, or a GUID it did not list — an
/// empty subject. The job row then reads as its kind alone ("Import puli").
///
/// Never the GUID: a 20-digit number in the job list is what nobody can tell
/// from any other. A GUID the pool no longer answers to is not refused here
/// either — the helper refuses it when the job runs, and that refusal is the
/// job's own error rather than a guess made from a cache.
fn import_job_subject(org_id: &str, guid: &str, new_name: &str, now: std::time::Instant) -> String {
    if !new_name.is_empty() {
        return new_name.to_string();
    }
    let Ok(cache) = import_scan_names().lock() else {
        return String::new();
    };
    cache
        .get(org_id)
        .filter(|(at, _)| now.saturating_duration_since(*at) < IMPORT_SCAN_NAMES_TTL)
        .and_then(|(_, names)| names.get(guid).cloned())
        .unwrap_or_default()
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
            .plan()
            .map_err(|e| broker_error("zpool add", catalog_error(e)))?;
    }
    let explicit = secret.map(token);
    let job = tentanas::jobs::spawn(&g.db, "pool_add_vdev", name, &g.user_id, None, None, move |h| async move {
        for command in commands {
            tentanas::jobs::run_step(&h, &command, explicit.as_deref(), Duration::from_secs(600))
                .await?;
        }
        drop(explicit);
        h.progress(100);
        Ok(())
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response(ctx, job))
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

/// The refusal of a detach the node does not allow (`refusal:<code>`, worded
/// by the screen).
const POOL_DETACH_NOT_ALLOWED: &str = "refusal:pool_detach_not_allowed";

/// Whether `device` of this pool status may be detached: a leaf the node
/// itself marked `detachable` (the original disk of a `spare-N` group whose
/// hot spare is ONLINE, with no resilver running — `pools::mark_detachable`).
/// The screen's word is never enough: a detach of any other leaf would take
/// redundancy, or a disk, the pool still needs.
fn detach_allowed(status: &tentanas::pools::StatusReport, device: &str) -> bool {
    !device.is_empty()
        && status
            .vdevs
            .iter()
            .flat_map(|v| &v.disks)
            .any(|d| d.detachable && d.name == device)
}

/// Owner decision (wave 7): after a replace onto a hot spare the old disk
/// stays in the pool's `spare-N` group; this takes it out with `zpool detach`
/// once the node — reading `zpool status` again, now — agrees it may go.
async fn pool_detach(
    ctx: &HandlerContext,
    name: &str,
    device: &str,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_POOLS)?;
    let status = tentanas::pools::status(name).await.map_err(|e| broker_error("pool", e))?;
    if !detach_allowed(&status, device) {
        return Err(ProtocolError::new(ProtocolErrorCode::NotAvailable, POOL_DETACH_NOT_ALLOWED));
    }
    let command = HelperCommand::ZpoolDetach { pool: name.to_string(), device: device.to_string() };
    run_now(&g, "detach", &command, secret).await?;
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

/// The recurring scrub (§5.2) or the recurring TRIM (§5.10) of one pool: the
/// same row, the same validation, the same answer — only the table differs.
async fn pool_schedule_set(
    ctx: &HandlerContext,
    task: store::PoolTask,
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
    store::set_pool_schedule(&g.db, task, name, enabled, schedule, next.as_deref())
        .map_err(|e| internal("schedules", e))?;
    pool_view(&g, name).await
}

/// `zpool trim` as an action (§5.10, research R7). 'start' follows the trim to
/// its end as a job, like a scrub; the other three are one privileged step.
async fn pool_trim(
    ctx: &HandlerContext,
    name: &str,
    action: &str,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_POOLS)?;
    tentanas_helper::validate_pool_name(name)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    if action == "start" {
        let pool = name.to_string();
        let explicit = secret.map(token);
        let job = tentanas::jobs::spawn(&g.db, "pool_trim", name, &g.user_id, None, None, move |h| {
            tentanas::pools::trim_job(h, pool, explicit)
        })
        .map_err(|e| internal("job", e))?;
        return Ok(job_response(ctx, job));
    }
    let trim_action = match action {
        "suspend" => tentanas_helper::TrimAction::Suspend,
        "resume" => tentanas_helper::TrimAction::Resume,
        "cancel" => tentanas_helper::TrimAction::Cancel,
        other => {
            return Err(ProtocolError::bad_request(format!(
                "unknown trim action '{other}'"
            )))
        }
    };
    run_now(
        &g,
        "trim",
        &HelperCommand::ZpoolTrim {
            pool: name.to_string(),
            action: trim_action,
        },
        secret,
    )
    .await?;
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
        ctx,
        &g,
        "dataset_destroy",
        name,
        HelperCommand::ZfsDestroy {
            name: name.to_string(),
            recursive,
            // A dataset destroy is never deferred: `-d` is a snapshot flag,
            // and a dataset holding a protected snapshot must fail loudly
            // instead of taking the protection down with it.
            deferred: false,
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
    let g = gate(ctx, PERM_READ)?;
    let all = tentanas::snapshots::list(pool, dataset, recursive)
        .await
        .map_err(|e| broker_error("snapshots", e))?;
    // ZFS knows a snapshot is held; only the app knows how long the admin
    // asked that to last (§5.10), so the record is joined in here.
    let protection = store::snapshot_protection(&g.db).unwrap_or_default();
    let filtered: Vec<_> = all
        .into_iter()
        .filter(|s| origin.is_empty() || s.origin == origin)
        .map(|mut s| {
            if tentanas::snapshots::is_protected(&s) {
                s.protected_until = protection.get(&s.name).cloned();
            }
            s
        })
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
    protect_days: u32,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_POOLS)?;
    // `manual-<YYYYMMDD>-<HHMMSS>` in node local time: the same
    // `<prefix>-<timestamp>` shape the automatic snapshots use, which is what
    // lets ONE `shadow:format` in the generated smb.conf offer both kinds as
    // Windows "Previous Versions" (shares.rs). A name the admin types is left
    // alone and simply does not appear there.
    let short_name = if short_name.is_empty() {
        chrono::Local::now().format("manual-%Y%m%d-%H%M%S").to_string()
    } else {
        short_name.to_string()
    };
    let now = chrono::Local::now();
    let snapshot = format!("{dataset}@{short_name}");
    for command in tentanas::snapshots::create_commands(&snapshot, recursive, protect_days > 0) {
        run_now(&g, "snapshot", &command, secret).await?;
    }
    if protect_days > 0 {
        // Recorded only after the hold is really there: the record is what the
        // UI shows as "protected until", and it must never claim a protection
        // ZFS does not have.
        let until = tentanas::snapshots::protected_until(now, protect_days);
        store::record_snapshot_protection(
            &g.db,
            &snapshot,
            protect_days,
            &until,
            &g.user_id,
            recursive,
        )
        .map_err(|e| internal("snapshots", e))?;
    }
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
    // Which of them are protected decides the destroy shape, so it is read
    // from ZFS itself rather than from the app's records: a hold placed by
    // hand must count too, and a plain destroy would just fail on it.
    let protected: std::collections::HashSet<String> = tentanas::snapshots::list("", "", false)
        .await
        .map_err(|e| broker_error("snapshots", e))?
        .into_iter()
        .filter(tentanas::snapshots::is_protected)
        .map(|s| s.name)
        .collect();
    let subject = names.first().cloned().unwrap_or_default();
    let list = names.to_vec();
    let explicit = secret.map(token);
    let job = tentanas::jobs::spawn(&g.db, "snapshot_destroy", &subject, &g.user_id, None, None, move |h| {
        async move {
            for name in list {
                let is_protected = protected.contains(&name);
                if is_protected {
                    h.log(format!(
                        "{name} is protected — the destroy is deferred and takes effect when the protection is lifted"
                    ));
                }
                let command = tentanas::snapshots::destroy_command(&name, is_protected);
                tentanas::jobs::run_step(&h, &command, explicit.as_deref(), Duration::from_secs(300))
                    .await?;
            }
            drop(explicit);
            h.progress(100);
            Ok(())
        }
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response(ctx, job))
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
        ctx,
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
        protect_days,
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
        protect_days: *protect_days,
    };
    // §5.10: the retention of a COARSE tier may not fall below the protection
    // period. Such a tier would protect snapshots it then wants to prune, and
    // only a four-eyes approval could free them — so it is refused, not
    // silently kept. The fine tiers hold nothing and are not consulted.
    if let Some((tier, days)) = tentanas::snapshots::protection_shortfall(&row) {
        return Err(ProtocolError::bad_request(format!(
            "the '{tier}' retention keeps {days} days of snapshots, less than the {} days of protection this schedule hands out",
            row.protect_days
        )));
    }
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

/// A schedule row's last outcome as the wire carries it (B, wave 6): the
/// structured fields, with the job's status read here — the job id stays on
/// the node — and the older sentence for a screen that predates them. A
/// stored value this build does not read (an older bare word) travels as the
/// sentence alone.
fn with_outcome(db: &crate::db::DbPool, mut row: NasScheduleRow, stored: &str) -> NasScheduleRow {
    use tentanas::scheduler::ScheduleOutcome;
    let Some(outcome) = ScheduleOutcome::parse(stored) else {
        row.last_result = stored.to_string();
        return row;
    };
    row.last_result = outcome.legacy_sentence();
    row.last_outcome = outcome.wire_word().to_string();
    match outcome {
        ScheduleOutcome::Started { job_id } => {
            // A job that is gone (pruned) has no status: the row then says
            // only that it ran.
            row.last_job_status = store::job(db, &job_id).ok().flatten().map(|j| j.status).unwrap_or_default();
        }
        ScheduleOutcome::StartFailed { detail } => row.last_detail = detail,
        ScheduleOutcome::Skipped { reason, detail } => {
            row.last_reason = reason;
            row.last_detail = detail;
        }
    }
    row
}

fn schedules_list(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    let mut rows = Vec::new();
    for task in [store::PoolTask::Scrub, store::PoolTask::Trim] {
        for row in store::list_pool_schedules(&g.db, task).map_err(|e| internal("schedules", e))? {
            rows.push(with_outcome(&g.db, NasScheduleRow {
                kind: task.kind().to_string(),
                subject: row.pool,
                enabled: row.enabled,
                schedule: row.schedule,
                last_run_at: row.last_run_at,
                next_run_at: row.next_run_at,
                ..Default::default()
            }, &row.last_result));
        }
    }
    for s in store::list_snapshot_schedules(&g.db).map_err(|e| internal("schedules", e))? {
        let last_result = store::snapshot_schedule_result(&g.db, &s.schedule_id).unwrap_or_default();
        rows.push(with_outcome(&g.db, NasScheduleRow {
            kind: "snapshot".to_string(),
            subject: s.dataset,
            enabled: s.enabled,
            schedule: s.schedule,
            last_run_at: s.last_run_at,
            next_run_at: s.next_run_at,
            ..Default::default()
        }, &last_result));
    }
    // The Elastic cadences (§5.3, E2-10). The kind is PREFIXED: the Tasks tab
    // buckets a row called 'scrub' as a POOL scrub and offers to run `zpool
    // scrub <subject>` on it, so an array's scrub landing in that bucket would
    // put a button on screen whose only possible answer is "no such pool".
    let arrays =
        store::elastic_arrays(&g.db, &elastic_owner(&g)).map_err(|e| internal("schedules", e))?;
    for array in &arrays {
        let Some(array_id) = array.array_id() else {
            continue;
        };
        for task in store::ElasticTask::ALL {
            let Some(row) = store::elastic_schedule(&g.db, array_id, task)
                .map_err(|e| internal("schedules", e))?
            else {
                continue;
            };
            rows.push(with_outcome(&g.db, NasScheduleRow {
                kind: format!("elastic_{}", task.kind()),
                subject: array.name.clone(),
                enabled: row.enabled,
                schedule: row.schedule,
                last_run_at: row.last_run_at,
                next_run_at: row.next_run_at,
                ..Default::default()
            }, &row.last_result));
        }
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
            next_run_at: next.clone(),
            ..Default::default()
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
    let next_short = tentanas::scheduler::next_run_utc(short, now);
    let next_long = tentanas::scheduler::next_run_utc(long, now);
    if enabled && (next_short.is_none() || next_long.is_none()) {
        return Err(ProtocolError::bad_request(
            "unknown schedule cadence for the SMART tests",
        ));
    }
    // Not read-modify-write: a scheduler tick between a read here and the
    // write would lose its run stamps (`store::save_smart_schedule`).
    let smart = store::save_smart_schedule(
        &g.db,
        enabled,
        short,
        long,
        enabled.then_some(next_short).flatten(),
        enabled.then_some(next_long).flatten(),
    )
    .map_err(|e| internal("schedules", e))?;
    Ok(tn(P::SmartScheduleResponse { smart }))
}

// ----- shares (SMB / NFS) --------------------------------------------------------------

/// Share mutations are `nas.shares.manage`; the delete goes through the
/// destructive gate like every other operation that takes an export away from
/// clients that are using it.
fn gate_shares(ctx: &HandlerContext) -> Result<Gate, ProtocolError> {
    gate(ctx, PERM_SHARES)
}

/// The whole `NasShare` of one row: the stored share, the mount state every
/// node published for it, and how many clients are attached right now.
fn share_view(
    ctx: &HandlerContext,
    g: &Gate,
    row: &store::ShareRow,
    sessions: u32,
) -> tentaflow_protocol::tentanas::NasShare {
    let mut share = tentanas::shares::to_protocol(row);
    share.mounts = tentanas::fleet_mounts::mounts_for(
        ctx,
        &g.addon_id,
        &tentanas::fleet_mounts::local_node_id(),
        &row.share_id,
        row.fleet_mount,
    );
    share.sessions = sessions;
    share
}

async fn shares_list(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    // The asking organisation's shares and accounts only (migration 20): the
    // node's database holds every tenant's, and this list names them, their
    // paths and who may reach them.
    let rows = store::list_shares_of_org(&g.db, &g.org_id).map_err(|e| internal("shares", e))?;
    let counts = tentanas::shares::session_counts(&g.db, &rows).await;
    let shares = rows
        .iter()
        .map(|row| share_view(ctx, &g, row, counts.get(&row.name).copied().unwrap_or(0)))
        .collect();
    Ok(tn(P::SharesListResponse {
        shares,
        services: tentanas::shares::services(&g.db).await,
        users: store::list_share_users(&g.db, &g.org_id).map_err(|e| internal("share users", e))?,
        mount_root: tentanas::shares::MOUNT_ROOT.to_string(),
    }))
}

/// One share of the ASKING organisation. Another organisation's share gets
/// the answer an id that does not exist gets — every read, edit, delete and
/// mount refresh goes through here, so none of them can confirm it exists.
fn share_row(g: &Gate, share_id: &str) -> Result<store::ShareRow, ProtocolError> {
    store::share(&g.db, &g.org_id, share_id)
        .map_err(|e| internal("shares", e))?
        .ok_or_else(|| ProtocolError::not_found("share not found on this node"))
}

/// Every account an SMB share is about to GRANT must be a share account of
/// the asking organisation. A grant to another tenant's account would let
/// that tenant's user into this share, and the refusal is the same sentence
/// for an account that is someone else's and one that does not exist, so it
/// confirms nothing. `already` are the grants the row carries now: a grant
/// that is only being KEPT is not re-judged — a legacy grant (migration 20
/// gives an account shared by two organisations to the default one) must not
/// make every later edit of the share fail.
fn require_own_grantees(
    g: &Gate,
    smb: &Option<tentaflow_protocol::tentanas::NasSmbOptions>,
    already: &[tentaflow_protocol::tentanas::NasShareAccess],
) -> Result<(), ProtocolError> {
    let Some(smb) = smb else {
        return Ok(());
    };
    for grant in &smb.users {
        if already.iter().any(|kept| kept.user == grant.user) {
            continue;
        }
        if !store::share_user_exists(&g.db, &g.org_id, &grant.user).map_err(|e| internal("share users", e))? {
            return Err(ProtocolError::bad_request(format!(
                "'{}' is not a share user of this organisation",
                grant.user
            )));
        }
    }
    Ok(())
}

async fn share_get(ctx: &HandlerContext, share_id: &str) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    let row = share_row(&g, share_id)?;
    let sessions = tentanas::shares::sessions(&g.db, &row).await;
    let share = share_view(ctx, &g, &row, sessions.len() as u32);
    Ok(tn(P::ShareGetResponse { share, sessions }))
}

/// Spawns the job that rewrites both service configs and republishes the
/// fleet's desired state. Every share mutation ends here, so the generated
/// files always describe the whole node rather than the last change.
///
/// The job is the ASKING organisation's (`spawn_owned`, migration 20): its
/// subject is that tenant's share. Its log comes from a NODE-WIDE apply,
/// which speaks about every tenant's shares, so the lines naming another
/// organisation's share are dropped before they are written
/// (`shares::scope_log`).
fn spawn_apply_job(
    ctx: &HandlerContext,
    g: &Gate,
    kind: &str,
    subject: &str,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let explicit = secret.map(token);
    let main_db = ctx.state.db.clone();
    let addon_id = g.addon_id.clone();
    let org_id = g.org_id.clone();
    let job = tentanas::jobs::spawn_owned(&g.db, kind, subject, &g.user_id, Some(g.org_id.as_str()), None, None, move |h| async move {
        let db = h.db().clone();
        let lines = tentanas::shares::apply(&db, &main_db, &addon_id, explicit.as_deref(), tentanas::shares::ApplyTrigger::Change).await?;
        log_scoped_share_lines(&h, &db, &org_id, lines);
        drop(explicit);
        h.progress(100);
        Ok(())
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response(ctx, job))
}

/// Writes the lines of a node-wide share apply that `org_id` may read. When
/// the owners cannot be read, NO line is written: an unscoped log is exactly
/// what this exists to prevent.
fn log_scoped_share_lines(h: &tentanas::jobs::JobHandle, db: &DbPool, org_id: &str, lines: Vec<String>) {
    match tentanas::shares::foreign_share_markers(db, org_id) {
        Ok(markers) => {
            for line in tentanas::shares::scope_log(lines, &markers) {
                h.log(line);
            }
        }
        Err(e) => h.log(format!("the apply log is withheld: the share owners could not be read ({e})")),
    }
}

/// The two transport gates of a share, read from the CACHED environment the
/// wizard offered the options from, so both sides answer the same question:
/// the RDMA row (§5.5a, the NFS transport) and the ksmbd row (§5.4b, SMB
/// Direct — which also carries the exposure guard).
async fn transport_gates(g: &Gate) -> (bool, bool) {
    match tentanas::environment::cached_or_probe(&g.db).await {
        Ok(env) => (
            tentanas::rdma::available(&env.features),
            tentanas::ksmbd::available(&env.features),
        ),
        Err(_) => (false, false),
    }
}

async fn share_create(ctx: &HandlerContext, req: &P) -> Result<MessageBody, ProtocolError> {
    let P::ShareCreateRequest {
        name,
        protocol,
        source_path,
        smb,
        nfs,
        fleet_mount,
        enabled,
        sudo_password,
    } = req
    else {
        return Err(ProtocolError::bad_request("expected ShareCreateRequest"));
    };
    let g = gate_shares(ctx)?;
    tentanas_helper::validate_share_name(name)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    // From the read of the datasets to the row: a pool destroy cannot run in
    // between (`pools::resources_lock`) — and one that is running refuses
    // this after a short wait, never an unbounded one.
    let _serialised = resources_or_refuse(&g).await?;
    let (rdma_ok, smb_direct_ok) = transport_gates(&g).await;
    tentanas::shares::validate_options(protocol, smb, nfs, rdma_ok, smb_direct_ok)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    require_own_grantees(&g, smb, &[])?;
    // Node-wide on purpose: an SMB section and an export are named on the
    // NODE, so the name is taken whoever took it. The sentence names only
    // what the caller typed, never whose share holds it.
    if store::share_by_name(&g.db, name)
        .map_err(|e| internal("shares", e))?
        .is_some()
    {
        return Err(ProtocolError::bad_request(format!(
            "a share named '{name}' already exists on this node"
        )));
    }
    let datasets = tentanas::datasets::list("")
        .await
        .map_err(|e| broker_error("datasets", e))?;
    // Only the caller's own arrays can be a share's source. The node holds
    // every tenant's arrays; resolving against all of them let one
    // organisation publish another's union over SMB or NFS. The OTHER
    // tenants' unions are refused outright as well: on a node whose pool is
    // mounted at `/mnt`, such a union was otherwise accepted as a directory
    // of that pool.
    let owner = elastic_owner(&g);
    let arrays = store::elastic_arrays(&g.db, &owner).map_err(|e| internal("elastic arrays", e))?;
    let foreign = tentanas::shares::foreign_unions(&g.db, &owner).map_err(|e| internal("elastic arrays", e))?;
    let source = tentanas::shares::resolve_source(&datasets, &arrays, &foreign, source_path)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let now = store::now();
    let row = store::ShareRow {
        share_id: uuid::Uuid::now_v7().to_string(),
        name: name.clone(),
        protocol: protocol.clone(),
        source_path: source.path,
        dataset: source.dataset,
        enabled: *enabled,
        fleet_mount: *fleet_mount,
        smb: smb.clone(),
        nfs: nfs.clone(),
        // The apply job decides the real state; until it ran the share is not
        // in any config, and "disabled" is what that is.
        state: "disabled".to_string(),
        state_detail: String::new(),
        state_reasons: Vec::new(),
        created_at: now.clone(),
        updated_at: now,
    };
    // Stamped with the creating organisation — the only one that will ever
    // see or manage it (owner decision 2026-09-22).
    store::upsert_share(&g.db, &g.org_id, &row).map_err(|e| internal("shares", e))?;
    spawn_apply_job(ctx, &g, "share_create", name, sudo_password.as_ref())
}

async fn share_update(ctx: &HandlerContext, req: &P) -> Result<MessageBody, ProtocolError> {
    let P::ShareUpdateRequest {
        share_id,
        smb,
        nfs,
        fleet_mount,
        enabled,
        sudo_password,
    } = req
    else {
        return Err(ProtocolError::bad_request("expected ShareUpdateRequest"));
    };
    let g = gate_shares(ctx)?;
    let mut row = share_row(&g, share_id)?;
    // Turning a transport ON needs the probe; a share that already has one
    // keeps it without one, so pausing or editing a share cannot start failing
    // because a card went down. The apply degrades that share on its own — to
    // TCP for NFS, to Samba-only for SMB Direct, saying so in `state_detail`.
    let (rdma_probed, smb_direct_probed) = transport_gates(&g).await;
    let rdma_ok = row.nfs.as_ref().is_some_and(|n| n.rdma) || rdma_probed;
    let smb_direct_ok = row.smb.as_ref().is_some_and(|s| s.smb_direct) || smb_direct_probed;
    tentanas::shares::validate_options(&row.protocol, smb, nfs, rdma_ok, smb_direct_ok)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let kept = row.smb.as_ref().map(|o| o.users.clone()).unwrap_or_default();
    require_own_grantees(&g, smb, &kept)?;
    row.smb = smb.clone();
    row.nfs = nfs.clone();
    row.fleet_mount = *fleet_mount;
    row.enabled = *enabled;
    row.updated_at = store::now();
    let name = row.name.clone();
    store::upsert_share(&g.db, &g.org_id, &row).map_err(|e| internal("shares", e))?;
    spawn_apply_job(ctx, &g, "share_update", &name, sudo_password.as_ref())
}

async fn share_delete(
    ctx: &HandlerContext,
    share_id: &str,
    confirm_name: &str,
    secret: Option<&SudoSecret>,
    origin: Origin,
) -> Result<MessageBody, ProtocolError> {
    super::app_gate::require_app_permission(ctx, tentanas::PACKAGE_ID, PERM_SHARES)?;
    let g = gate_destructive(ctx)?;
    let row = share_row(&g, share_id)?;
    require_confirm(&row.name, confirm_name)?;
    // Only a share with data behind it goes through four eyes (§5.10): taking
    // an empty export away costs nobody anything.
    if origin == Origin::Direct
        && tentanas::shares::holds_data(&row.source_path)
        && tentanas::approvals::required(&actor(ctx, &g)?)
    {
        return park(
            ctx,
            &g,
            tentanas::approvals::OP_SHARE_DELETE,
            &row.name,
            CodedText::new(
                "share_delete",
                &[("share", row.name.clone()), ("path", row.source_path.clone())],
                format!(
                    "removes the share '{}' — the data under {} stays, the export does not",
                    row.name, row.source_path
                ),
            ),
            &P::ShareDeleteRequest {
                share_id: share_id.to_string(),
                confirm_name: confirm_name.to_string(),
                sudo_password: None,
            },
        );
    }
    if !store::delete_share(&g.db, &g.org_id, share_id).map_err(|e| internal("shares", e))? {
        return Err(ProtocolError::not_found("share not found on this node"));
    }
    // The desired-state row goes now, not when the job finishes: every other
    // node reconciles off it and must stop mounting a share that is gone.
    tentanas::fleet_mounts::purge_share(&ctx.state.db, &g.addon_id, share_id);
    spawn_apply_job(ctx, &g, "share_delete", &row.name, secret)
}

async fn share_browse(ctx: &HandlerContext, path: &str) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    // Pools for everybody, Elastic unions of the asking organisation only —
    // the same set `share_create` resolves a source against.
    let (path, entries) = tentanas::shares::browse(&g.db, &elastic_owner(&g), path)
        .await
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    Ok(tn(P::ShareBrowseResponse { path, entries }))
}

async fn share_mounts_refresh(
    ctx: &HandlerContext,
    share_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    let row = share_row(&g, share_id)?;
    tentanas::fleet_mounts::reconcile(&ctx.state.db, &g.addon_id, &g.db, Some(share_id)).await;
    let sessions = tentanas::shares::sessions(&g.db, &row).await;
    let share = share_view(ctx, &g, &row, sessions.len() as u32);
    Ok(tn(P::ShareGetResponse { share, sessions }))
}

fn share_users_response(g: &Gate) -> Result<MessageBody, ProtocolError> {
    Ok(tn(P::ShareUsersListResponse {
        users: store::list_share_users(&g.db, &g.org_id).map_err(|e| internal("share users", e))?,
    }))
}

async fn share_user_set(
    ctx: &HandlerContext,
    req: &P,
) -> Result<MessageBody, ProtocolError> {
    let P::ShareUserSetRequest {
        name,
        password,
        description,
        sudo_password,
    } = req
    else {
        return Err(ProtocolError::bad_request("expected ShareUserSetRequest"));
    };
    let g = gate_shares(ctx)?;
    tentanas_helper::validate_share_user(name)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    // A share account is a node account (Samba's passdb maps it to a POSIX
    // user), so its NAME is one namespace for every organisation. An account
    // another organisation owns is refused HERE, before any password reaches
    // `smbpasswd`: setting it would hand this tenant the other tenant's login
    // — and with it every share that grants it. That the name is taken is
    // the one thing the refusal has to say; it names nobody.
    let known = match store::share_user_owner(&g.db, name).map_err(|e| internal("share users", e))? {
        None => false,
        Some(owner) if !owner.is_empty() && owner == g.org_id => true,
        Some(_) => {
            return Err(ProtocolError::bad_request(
                "this account name is already in use on this node — choose another",
            ))
        }
    };
    if let Some(password) = password {
        if password.0.is_empty() {
            return Err(ProtocolError::bad_request("the password may not be empty"));
        }
        let explicit = sudo_password.as_ref().map(token);
        // The password reaches `smbpasswd` (and, on a node that serves SMB
        // Direct, `ksmbd.adduser`) through the helper's stdin and is never
        // stored: the core has no copy of it after this call. The two backends
        // keep separate password databases, so ONE share account means the
        // same secret written twice, in the same request (§5.4b).
        let mut commands = vec![(
            HelperCommand::SmbUserSet { user: name.clone() },
            "smbpasswd failed",
        )];
        if tentanas::ksmbd::has_user_database() {
            commands.push((
                HelperCommand::KsmbdUserSet { user: name.clone() },
                "ksmbd.adduser failed",
            ));
        }
        for (command, fallback) in commands {
            let (out, _) = tentanas::broker::run_privileged_with_key(
                &g.db,
                &command,
                password.0.as_bytes(),
                explicit.as_deref(),
                Duration::from_secs(60),
            )
            .await
            .map_err(|e| broker_error("share user", e))?;
            if !out.success() {
                return Err(ProtocolError::bad_request(
                    out.stderr
                        .trim()
                        .lines()
                        .next()
                        .unwrap_or(fallback)
                        .to_string(),
                ));
            }
        }
    } else if !known {
        return Err(ProtocolError::bad_request(
            "a new share user needs a password",
        ));
    }
    store::upsert_share_user(&g.db, &g.org_id, name, description).map_err(|e| internal("share users", e))?;
    share_users_response(&g)
}

async fn share_user_delete(
    ctx: &HandlerContext,
    name: &str,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate_shares(ctx)?;
    // Another organisation's account answers like a missing one, and is
    // refused before either password database is touched.
    if !store::share_user_exists(&g.db, &g.org_id, name).map_err(|e| internal("share users", e))? {
        return Err(ProtocolError::not_found("share user not found on this node"));
    }
    // A legacy account (migration 20 gave one granted in two organisations to
    // the default one) that a share of ANOTHER organisation still grants is
    // one node account serving that organisation's users: removing it would
    // cut them off their own share. Refused BEFORE either password database
    // is touched, and without naming the share or its organisation.
    if store::share_user_granted_elsewhere(&g.db, &g.org_id, name).map_err(|e| internal("share users", e))? {
        return Err(ProtocolError::bad_request(store::SHARE_USER_IN_USE_ELSEWHERE));
    }
    let explicit = secret.map(token);
    // ksmbd's database goes FIRST and only then the POSIX account Samba's
    // passdb maps to: dropping the account while the second database still
    // names it would leave an entry pointing at a user that no longer exists.
    if tentanas::ksmbd::has_user_database() {
        let (out, _) = tentanas::broker::run_privileged(
            &g.db,
            &HelperCommand::KsmbdUserDelete { user: name.to_string() },
            explicit.as_deref(),
            Duration::from_secs(60),
        )
        .await
        .map_err(|e| broker_error("share user", e))?;
        if !out.success() {
            return Err(ProtocolError::bad_request(
                out.stderr
                    .trim()
                    .lines()
                    .next()
                    .unwrap_or("the ksmbd account could not be removed")
                    .to_string(),
            ));
        }
    }
    let (out, _) = tentanas::broker::run_privileged(
        &g.db,
        &HelperCommand::SmbUserDelete { user: name.to_string() },
        explicit.as_deref(),
        Duration::from_secs(60),
    )
    .await
    .map_err(|e| broker_error("share user", e))?;
    if !out.success() {
        return Err(ProtocolError::bad_request(
            out.stderr
                .trim()
                .lines()
                .next()
                .unwrap_or("the account could not be removed")
                .to_string(),
        ));
    }
    store::delete_share_user(&g.db, &g.org_id, name).map_err(|e| internal("share users", e))?;
    // Dropping the user changed every share that granted it, so the generated
    // sections have to follow before smbd offers access to an account that no
    // longer exists.
    let main_db = ctx.state.db.clone();
    let addon_id = g.addon_id.clone();
    if let Err(e) = tentanas::shares::apply(&g.db, &main_db, &addon_id, explicit.as_deref(), tentanas::shares::ApplyTrigger::Change).await {
        tracing::warn!("tentanas: share config not rewritten after user delete: {e}");
    }
    share_users_response(&g)
}

// ----- block targets: iSCSI and NVMe-oF (§5.5) --------------------------------------

/// `nas.targets.manage` AND the org Admin role.
///
/// NOT `nas.shares.manage`: the manifest already declares a separate
/// `nas.targets.manage` at risk "high" with "Requires the Admin role", and the
/// two are not the same decision. A file share hands out paths behind file
/// ACLs; a block target hands out a RAW DISK with no ACLs at all, on which two
/// careless clients destroy each other's data. Delegating "make shares" must
/// not silently delegate that.
fn gate_targets(ctx: &HandlerContext) -> Result<Gate, ProtocolError> {
    super::app_gate::require_app_permission(ctx, tentanas::PACKAGE_ID, PERM_TARGETS)?;
    gate_admin(ctx)
}

/// One target of the ASKING organisation; another organisation's answers
/// like an id that does not exist (migration 20).
fn target_row(g: &Gate, target_id: &str) -> Result<store::TargetRow, ProtocolError> {
    store::target(&g.db, &g.org_id, target_id)
        .map_err(|e| internal("targets", e))?
        .ok_or_else(|| ProtocolError::not_found("target not found on this node"))
}

/// Every target of the node AS THE ASKING ORGANISATION MAY SEE IT
/// (`targets::seen_by`): another organisation's rows stay in — a zvol it
/// exports is still taken, a host NQN it allows still collides, a WWN it
/// holds is still a configfs object — but carry no name, so neither the
/// wizard's volume list nor a refusal can say whose they are.
fn targets_seen_by(g: &Gate) -> Result<Vec<store::TargetRow>, ProtocolError> {
    let rows = store::list_targets(&g.db).map_err(|e| internal("targets", e))?;
    let owners = store::target_owners(&g.db).map_err(|e| internal("targets", e))?;
    Ok(tentanas::targets::seen_by(rows, &owners, &g.org_id))
}

/// What this node can serve, plus the zvols and interfaces the wizard offers.
/// Read from the CACHED environment the wizard already showed, so the options
/// it offers and the ones the save re-checks cannot disagree.
async fn block_capabilities(
    g: &Gate,
    targets: &[store::TargetRow],
) -> tentaflow_protocol::tentanas::NasBlockCapabilities {
    let features = tentanas::environment::cached_or_probe(&g.db)
        .await
        .map(|e| e.features)
        .unwrap_or_default();
    let datasets = tentanas::datasets::list("").await.unwrap_or_default();
    tentanas::targets::capabilities(&features, &datasets, targets)
}

thread_local! {
    /// Capability computations asked for on this thread (each one an
    /// environment read and a `zfs list` unless the 5 s cache holds them) —
    /// the dispatch test's proof that the fleet's summary path never asks.
    /// Per thread, so parallel tests do not count each other.
    static CAPABILITY_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// How long the two EXPENSIVE inputs of `block_capabilities` are reused on the
/// polled list path.
const CAPABILITIES_CACHE: Duration = Duration::from_secs(5);

type CapabilityInputs = (
    Vec<tentaflow_protocol::features::FeatureState>,
    Vec<tentaflow_protocol::tentanas::NasDataset>,
);

fn capabilities_cache() -> &'static std::sync::Mutex<Option<(std::time::Instant, CapabilityInputs)>>
{
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<Option<(std::time::Instant, CapabilityInputs)>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(None))
}

/// The same capabilities, for the POLLED list only.
///
/// `datasets::list("")` is two or three `zfs` processes and
/// `environment::cached_or_probe` can fall through to a full probe — per
/// request, on a tab that polls. Neither answer changes second to second: a
/// pool's zvols and a kernel's modules are not that kind of fact.
///
/// Only the two expensive INPUTS are cached; the capability set itself is
/// recomputed every time, because it folds in the current target rows (a
/// volume's "already exported by" comes from them) and those change on a save.
/// And only this path uses it — `target_create` and `target_update` validate
/// against a freshly read node, so no mutation is ever judged against a
/// capability set that is five seconds old.
async fn block_capabilities_cached(
    g: &Gate,
    targets: &[store::TargetRow],
) -> tentaflow_protocol::tentanas::NasBlockCapabilities {
    CAPABILITY_READS.with(|n| n.set(n.get() + 1));
    let cached = capabilities_cache()
        .lock()
        .ok()
        .and_then(|c| c.as_ref().filter(|(at, _)| at.elapsed() < CAPABILITIES_CACHE).map(|(_, v)| v.clone()));
    let (features, datasets) = match cached {
        Some(inputs) => inputs,
        None => {
            let features = tentanas::environment::cached_or_probe(&g.db)
                .await
                .map(|e| e.features)
                .unwrap_or_default();
            let datasets = tentanas::datasets::list("").await.unwrap_or_default();
            if let Ok(mut cache) = capabilities_cache().lock() {
                *cache = Some((std::time::Instant::now(), (features.clone(), datasets.clone())));
            }
            (features, datasets)
        }
    };
    tentanas::targets::capabilities(&features, &datasets, targets)
}

async fn targets_list(ctx: &HandlerContext, summary: bool) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    let rows = store::list_targets_of_org(&g.db, &g.org_id).map_err(|e| internal("targets", e))?;
    // The fleet's 10 s poll (critic wave 7, MAJOR 2): it reads the targets and
    // the service rows, never the capabilities, so it pays for neither the
    // environment read nor the `zfs list` behind them, and it takes a session
    // reading up to `FLEET_SESSIONS_MAX_AGE` old instead of a sudo per tick.
    if summary {
        let nvmet = if rows.iter().any(|row| row.protocol == "nvmet") {
            tentanas::targets::nvmet_sessions_within(&g.db, tentanas::targets::FLEET_SESSIONS_MAX_AGE).await
        } else {
            Default::default()
        };
        return Ok(tn(P::TargetsListResponse {
            targets: rows
                .iter()
                .map(|row| {
                    let (sessions, known) = tentanas::targets::sessions_from(row, &nvmet);
                    tentanas::targets::to_protocol(row, sessions.len() as u32, known)
                })
                .collect(),
            services: tentanas::targets::services(),
            capabilities: Default::default(),
        }));
    }
    // The cached variant: this list is POLLED, and the uncached one spawns two
    // or three `zfs` processes and may fall through to a full environment
    // probe on every single request. Judged against EVERY target of the node
    // (a zvol another tenant exports is not free), named only for ours.
    let capabilities = block_capabilities_cached(&g, &targets_seen_by(&g)?).await;
    // ONE privileged read for the whole list, and only when the list has an
    // NVMe-oF row at all — the same deal `shares::session_counts` makes with
    // `smbstatus` rather than paying a sudo per row of a polled table.
    let nvmet = if rows.iter().any(|row| row.protocol == "nvmet") {
        tentanas::targets::nvmet_sessions(&g.db).await
    } else {
        Default::default()
    };
    let targets = rows
        .iter()
        .map(|row| {
            let (sessions, known) = tentanas::targets::sessions_from(row, &nvmet);
            tentanas::targets::to_protocol(row, sessions.len() as u32, known)
        })
        .collect();
    Ok(tn(P::TargetsListResponse {
        targets,
        services: tentanas::targets::services(),
        capabilities,
    }))
}

async fn target_get(ctx: &HandlerContext, target_id: &str) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    let row = target_row(&g, target_id)?;
    let nvmet = if row.protocol == "nvmet" {
        tentanas::targets::nvmet_sessions(&g.db).await
    } else {
        Default::default()
    };
    let (sessions, known) = tentanas::targets::sessions_from(&row, &nvmet);
    // The preview is rendered from placeholder credentials and redacted on top
    // of that, so it can travel and be logged. A row the catalog's own rules
    // refuse cannot be rendered — and that is a fact about this target the
    // admin needs, so it goes into the block instead of leaving it empty. The
    // read itself still succeeds: everything else in the window is valid.
    let config_preview = match tentanas::targets::preview(&row) {
        Ok(text) => text,
        Err(e) => format!("this target cannot be rendered into a configfs plan: {e}"),
    };
    Ok(tn(P::TargetGetResponse {
        target: tentanas::targets::to_protocol(&row, sessions.len() as u32, known),
        sessions,
        config_preview,
    }))
}

/// The job behind one target mutation: apply THAT target, then say whether the
/// change actually reached the kernel.
///
/// `target_id` scopes the apply. A save used to re-render every target on the
/// node, which put twenty full configfs plans in the log of a single edit and
/// gave every save the whole node's worth of chances at the nvmet port
/// collision. Nothing about re-applying a live target is unsafe — that is
/// measured — but a job log an admin cannot read is not an audit trail, and
/// the blast radius of an edit should be the thing that was edited.
fn spawn_target_job(
    ctx: &HandlerContext,
    g: &Gate,
    kind: &str,
    subject: &str,
    target_id: &str,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let explicit = secret.map(token);
    let cipher = ctx.state.settings_cipher.clone();
    let name = subject.to_string();
    let scope = target_id.to_string();
    let org_id = g.org_id.clone();
    // The asking organisation's job. The KERNEL half of the apply reaches
    // its target and only while this organisation owns it; the judging half
    // re-evaluates every target on the node, and only the lines of this
    // organisation's own targets reach the job log (`apply_for_org`), the
    // same rule the delete and the import follow.
    let job = tentanas::jobs::spawn_owned(&g.db, kind, subject, &g.user_id, Some(g.org_id.as_str()), None, None, move |h| async move {
        let db = h.db().clone();
        for line in
            tentanas::targets::apply_for_org(&db, &cipher, explicit.as_deref(), Some(&scope), &org_id).await?
        {
            h.log(line);
        }
        drop(explicit);
        h.progress(100);
        // Does the kernel now match what the node judged? Three ways it does
        // not, and every one of them used to end green:
        //   * FROZEN (portal drift, §5.5) — the row is deliberately not
        //     touched, and the most likely change on a drifted target is
        //     taking an initiator OFF its allowlist, where "saved" reads as
        //     "access revoked";
        //   * judged appliable and not in the kernel — a target created on a
        //     zvol whose udev link has not appeared yet;
        //   * judged removable and still in the kernel — "Stop target" where
        //     the removal failed, while the client keeps writing.
        // The reconcile on the next tick will pick the last two up; the job
        // that claimed to have done them must not claim it.
        if let Some(reason) = tentanas::targets::unapplied_reason(&db, &name)? {
            return Err(anyhow::anyhow!(
                "the change is saved, but this target was not applied to the kernel: {reason}"
            ));
        }
        Ok(())
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response(ctx, job))
}

/// Turns the wizard's authentication choice into the row's four columns,
/// encrypting the secrets. `previous` keeps a stored secret when the request
/// carries none — an edit of the portal must not clear the CHAP password.
fn target_auth_columns(
    ctx: &HandlerContext,
    target_id: &str,
    protocol: &str,
    auth: Option<&tentaflow_protocol::tentanas::NasTargetAuth>,
    previous: Option<&store::TargetRow>,
) -> Result<(String, String, String, String, String, String, String), ProtocolError> {
    let Some(auth) = auth else {
        return Ok((
            "none".to_string(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        ));
    };
    let encrypt = |field: &str, value: &str| {
        tentanas::targets::encrypt_secret(&ctx.state.settings_cipher, target_id, field, value)
            .map_err(|e| internal("target secret", e))
    };
    let keep = |field: &str, incoming: Option<&tentaflow_protocol::tentanas::NasSecret>| {
        match incoming {
            Some(secret) if !secret.0.is_empty() => encrypt(field, &secret.0),
            // A request that carries no secret keeps the stored one; only
            // 'none' clears it, and it clears it below.
            _ => Ok(previous
                .map(|p| {
                    if field == "secret" {
                        p.auth_secret.clone()
                    } else {
                        p.auth_mutual_secret.clone()
                    }
                })
                .unwrap_or_default()),
        }
    };
    if auth.method == "none" {
        return Ok((
            "none".to_string(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        ));
    }
    let secret = keep("secret", auth.secret.as_ref())?;
    let mutual_secret = if auth.method == "mutual-chap" || auth.method == "dhchap-bidi" {
        keep("mutual_secret", auth.mutual_secret.as_ref())?
    } else {
        String::new()
    };
    if secret.is_empty() {
        return Err(ProtocolError::bad_request(
            "this authentication method needs a secret",
        ));
    }
    if (auth.method == "mutual-chap" || auth.method == "dhchap-bidi") && mutual_secret.is_empty() {
        return Err(ProtocolError::bad_request(
            "mutual authentication needs the target's own secret too",
        ));
    }
    // The DH parameters only mean anything to nvmet; defaulting them here
    // keeps a wizard that did not send them from producing a subsystem the
    // kernel refuses.
    let (hash, dhgroup) = if protocol == "nvmet" {
        (
            if auth.dhchap_hash.is_empty() {
                "hmac(sha256)".to_string()
            } else {
                auth.dhchap_hash.clone()
            },
            if auth.dhchap_dhgroup.is_empty() {
                "ffdhe2048".to_string()
            } else {
                auth.dhchap_dhgroup.clone()
            },
        )
    } else {
        (String::new(), String::new())
    };
    Ok((
        auth.method.clone(),
        auth.username.clone(),
        secret,
        auth.mutual_username.clone(),
        mutual_secret,
        hash,
        dhgroup,
    ))
}

/// The hostname a new target's IQN / NQN is built from, or the refusal when it
/// would give the target an empty host segment.
///
/// The segment `wwn_for` uses is the SANITISED hostname, so that is what is
/// judged — a hostname of `___` is as unusable as an empty one. The wizard
/// shows a placeholder in that case (`wwn_host` is empty on the wire), and
/// this is the matching refusal rather than a malformed `iqn.…:.name`.
fn target_host_for_create(hostname: &str) -> Result<String, ProtocolError> {
    if tentanas::targets::wwn_host(hostname).is_empty() {
        return Err(ProtocolError::bad_request(
            "this node has no usable hostname, and a target's IQN / NQN is built from it — \
             set the node's hostname first",
        ));
    }
    Ok(hostname.to_string())
}

async fn target_create(ctx: &HandlerContext, req: &P) -> Result<MessageBody, ProtocolError> {
    let P::TargetCreateRequest {
        name,
        protocol,
        source,
        create_size_bytes,
        thin,
        portal_interface,
        transports,
        auth,
        initiators,
        confirm_all_interfaces,
        enabled,
        sudo_password,
    } = req
    else {
        return Err(ProtocolError::bad_request("expected TargetCreateRequest"));
    };
    let g = gate_targets(ctx)?;
    // From the read of the volumes to the row: a pool destroy cannot run in
    // between (`pools::resources_lock`), and a running one refuses this.
    let _serialised = resources_or_refuse(&g).await?;
    // Checked before anything else is read or written: the host segment is
    // part of the target's permanent identity, and a node whose hostname is
    // empty (or holds nothing an IQN may carry) would publish `iqn.…:.name`.
    let node = target_host_for_create(&tentanas::config_io::hostname())?;
    // Node-wide: the name is part of the node's IQN / NQN namespace, so it is
    // taken whoever took it; the sentence names only what the caller typed.
    if store::target_by_name(&g.db, name)
        .map_err(|e| internal("targets", e))?
        .is_some()
    {
        return Err(ProtocolError::bad_request(format!(
            "a target named '{name}' already exists on this node"
        )));
    }
    let existing = targets_seen_by(&g)?;
    // Two targets on one zvol is two clients writing one raw disk.
    if let Some(other) = existing
        .iter()
        .find(|t| t.luns.iter().any(|l| l.source == *source))
    {
        return Err(ProtocolError::bad_request(format!(
            "'{}' already exports {source}",
            other.name
        )));
    }
    let caps = block_capabilities(&g, &existing).await;

    // The address of the interface the admin picked, through the ONE
    // definition of that phrase (`targets::primary_address`) — the same one
    // the drift check and an explicit re-pick use. Two definitions is what let
    // an aliased interface pass the drift check and then be rewritten onto a
    // sibling address by the next save.
    let interface = caps
        .interfaces
        .iter()
        .find(|i| i.name == *portal_interface)
        .cloned();
    if !portal_interface.is_empty() && interface.is_none() {
        return Err(ProtocolError::bad_request(format!(
            "'{portal_interface}' is not an interface of this node"
        )));
    }
    let address = if portal_interface.is_empty() {
        String::new()
    } else {
        match tentanas::targets::primary_address(&caps.interfaces, portal_interface) {
            Some(address) => address,
            None => {
                // Present, but with nothing a portal can bind — which today
                // means IPv6-only, and the sentence says so rather than
                // producing a portal on an empty address.
                let addresses: Vec<&str> = caps
                    .interfaces
                    .iter()
                    .filter(|i| i.name == *portal_interface)
                    .map(|i| i.address.as_str())
                    .collect();
                return Err(ProtocolError::bad_request(format!(
                    "{portal_interface} has only an IPv6 address ({}) — IPv6 portals are not \
                     offered yet",
                    addresses.join(", ")
                )));
            }
        }
    };
    // The volume as it will be: either the one already on the pool, or the one
    // the request asks to create.
    let volume = caps
        .volumes
        .iter()
        .find(|v| v.name == *source)
        .cloned()
        .unwrap_or_else(|| tentaflow_protocol::tentanas::NasBlockVolume {
            name: source.clone(),
            size_bytes: *create_size_bytes,
            thin: *thin,
            device_path: tentanas::targets::device_path(source),
            ..Default::default()
        });
    let target_id = uuid::Uuid::now_v7().to_string();
    let (method, username, secret, mutual_username, mutual_secret, hash, dhgroup) =
        target_auth_columns(ctx, &target_id, protocol, auth.as_ref(), None)?;
    let now = store::now();
    let row = store::TargetRow {
        name: name.clone(),
        protocol: protocol.clone(),
        wwn: tentanas::targets::wwn_for(protocol, &node, name),
        enabled: *enabled,
        luns: vec![tentanas::targets::lun_for(
            protocol,
            source,
            volume.size_bytes,
            volume.thin,
            &target_id,
        )],
        portals: transports
            .iter()
            .map(|t| tentanas::targets::portal_for(protocol, portal_interface, &address, t))
            .collect(),
        port_groups: tentanas::targets::default_port_groups(),
        // nvmet keeps its DH-HMAC-CHAP keys on the HOST objects of the
        // allowlist, so an authenticated subsystem needs one from the start —
        // `validate_options` refuses it otherwise, which is why the wizard asks
        // for host NQNs on the NVMe-oF path (n14 leaves the iSCSI allowlist to
        // the target detail, and this stays empty there).
        initiators: initiators.clone(),
        auth_method: method,
        auth_username: username,
        auth_secret: secret,
        auth_mutual_username: mutual_username,
        auth_mutual_secret: mutual_secret,
        dhchap_hash: hash,
        dhchap_dhgroup: dhgroup,
        // The apply job decides the real state; until it ran the target is in
        // no kernel, and "disabled" is what that is.
        state: "disabled".to_string(),
        state_detail: String::new(),
        state_reasons: Vec::new(),
        created_at: now.clone(),
        updated_at: now,
        target_id,
    };
    // BEFORE the zvol exists. A validation failure after `ZfsCreate` would
    // leave an orphaned volume on the pool that nobody cleans up and that the
    // next wizard offers as "free".
    tentanas::targets::validate_options(&row, &existing, &caps, *confirm_all_interfaces)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;

    let explicit = sudo_password.as_ref().map(token);
    if *create_size_bytes > 0 {
        // The zvol goes through the SAME catalog entry the dataset wizard
        // uses — there is no second way to create a volume (§3.4).
        let command = HelperCommand::ZfsCreate {
            name: source.clone(),
            kind: tentanas_helper::DatasetKind::Volume,
            properties: Vec::new(),
            volsize: create_size_bytes.to_string(),
            sparse: *thin,
            encryption: false,
        };
        let (out, _) = tentanas::broker::run_privileged(
            &g.db,
            &command,
            explicit.as_deref(),
            Duration::from_secs(120),
        )
        .await
        .map_err(|e| broker_error("zvol", e))?;
        if !out.success() {
            return Err(ProtocolError::bad_request(
                out.stderr
                    .trim()
                    .lines()
                    .next()
                    .unwrap_or("the volume could not be created")
                    .to_string(),
            ));
        }
    }
    let row_target_id = row.target_id.clone();
    // Stamped with the creating organisation (owner decision 2026-09-22).
    store::upsert_target(&g.db, &g.org_id, &row).map_err(|e| internal("targets", e))?;
    spawn_target_job(ctx, &g, "target_create", name, &row_target_id, sudo_password.as_ref())
}

async fn target_update(ctx: &HandlerContext, req: &P) -> Result<MessageBody, ProtocolError> {
    let P::TargetUpdateRequest {
        target_id,
        portals,
        repick_portal,
        auth,
        initiators,
        port_groups,
        confirm_all_interfaces,
        enabled,
        sudo_password,
    } = req
    else {
        return Err(ProtocolError::bad_request("expected TargetUpdateRequest"));
    };
    let g = gate_targets(ctx)?;
    let mut row = target_row(&g, target_id)?;
    let existing = targets_seen_by(&g)?;
    let caps = block_capabilities(&g, &existing).await;
    let (method, username, secret, mutual_username, mutual_secret, hash, dhgroup) =
        target_auth_columns(ctx, target_id, &row.protocol, auth.as_ref(), Some(&row))?;
    // ---- the portal, and the owner's drift decision (2026-09-04, §5.5) ----
    //
    // A portal MOVES only when somebody asked for it to move. The rule itself
    // lives in `targets::portals_for_update`, where it can be tested — the
    // handler is a caller, not the definition.
    //
    // What it replaces: the address used to be re-read from the node on EVERY
    // save. Three separate damages followed, all reachable from one click.
    // (1) A drifted target healed itself the moment an admin did the most
    // likely thing — pause/resume, or dropping an initiator — and the alert
    // closed with nothing anywhere saying the export had moved to a different
    // network. (2) On an aliased interface the collapse to the FIRST address
    // rewrote a healthy portal onto a sibling address, and `prune_iscsi` then
    // `rmdir`-ed the live one, cutting off every initiator logged in on it.
    // (3) `unapplied_reason` became unreachable: the same handler lifted the
    // freeze it was there to report.
    row.portals = tentanas::targets::portals_for_update(
        &row.protocol,
        &row.portals,
        portals,
        *repick_portal,
        &caps.interfaces,
    )
    .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    row.initiators = initiators.clone();
    if !port_groups.is_empty() {
        row.port_groups = port_groups.clone();
    }
    // A LUN follows the group it was put in; the wizard has no per-LUN picker
    // yet and one group is what a single-path target has.
    if row.port_groups.len() == 1 {
        let only = row.port_groups[0].group_id;
        for lun in row.luns.iter_mut() {
            lun.group_id = only;
        }
    }
    row.auth_method = method;
    row.auth_username = username;
    row.auth_secret = secret;
    row.auth_mutual_username = mutual_username;
    row.auth_mutual_secret = mutual_secret;
    row.dhchap_hash = hash;
    row.dhchap_dhgroup = dhgroup;
    row.enabled = *enabled;
    row.updated_at = store::now();
    tentanas::targets::validate_options(&row, &existing, &caps, *confirm_all_interfaces)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let name = row.name.clone();
    store::upsert_target(&g.db, &g.org_id, &row).map_err(|e| internal("targets", e))?;
    spawn_target_job(ctx, &g, "target_update", &name, target_id, sudo_password.as_ref())
}

/// Deleting a target cuts a live client off from a raw disk mid-write, which
/// is the same blast radius as deleting a share with data on it — so it takes
/// the same road: the destructive gate, a retyped name, and four eyes when the
/// fleet has a second admin (§5.10). The zvol and its data are never touched.
async fn target_delete(
    ctx: &HandlerContext,
    target_id: &str,
    confirm_name: &str,
    secret: Option<&SudoSecret>,
    origin: Origin,
) -> Result<MessageBody, ProtocolError> {
    super::app_gate::require_app_permission(ctx, tentanas::PACKAGE_ID, PERM_TARGETS)?;
    let g = gate_destructive(ctx)?;
    let row = target_row(&g, target_id)?;
    require_confirm(&row.name, confirm_name)?;
    if origin == Origin::Direct && tentanas::approvals::required(&actor(ctx, &g)?) {
        let sources: Vec<&str> = row.luns.iter().map(|l| l.source.as_str()).collect();
        return park(
            ctx,
            &g,
            tentanas::approvals::OP_TARGET_DELETE,
            &row.name,
            CodedText::new(
                "target_delete",
                &[("target", row.name.clone()), ("sources", sources.join(", "))],
                format!(
                    "stops exporting '{}' ({}) — a client using it loses the disk; {} and its data stay",
                    row.name,
                    row.wwn,
                    sources.join(", ")
                ),
            ),
            &P::TargetDeleteRequest {
                target_id: target_id.to_string(),
                confirm_name: confirm_name.to_string(),
                sudo_password: None,
            },
        );
    }
    // A portal-drift alert outlives its target otherwise: `apply` only closes
    // the alerts of rows it can still see, and this row is about to be gone.
    // Closed BEFORE the row and never ignored: an alert that stays open points
    // at a target nobody can open, and a failure here that was swallowed would
    // leave exactly that, forever. Doing it first also makes the failure
    // harmless — the row still exists, so the next evaluation re-raises it.
    tentanas::targets::forget_alerts(&g.db, target_id).map_err(|e| internal("targets", e))?;
    if !store::delete_target(&g.db, &g.org_id, target_id).map_err(|e| internal("targets", e))? {
        return Err(ProtocolError::not_found("target not found on this node"));
    }
    // The kernel object goes with the row, in the same job: a target left in
    // configfs with no row behind it is exactly the orphan §5.8 forbids.
    let explicit = secret.map(token);
    let protocol = row.protocol.clone();
    let wwn = row.wwn.clone();
    let cipher = ctx.state.settings_cipher.clone();
    let restore = row.clone();
    let org_id = g.org_id.clone();
    let job = tentanas::jobs::spawn_owned(&g.db, "target_delete", &row.name, &g.user_id, Some(g.org_id.as_str()), None, None, move |h| async move {
        let db = h.db().clone();
        let (lines, removed) = tentanas::targets::remove(&db, &protocol, &wwn, explicit.as_deref()).await;
        for line in lines {
            h.log(line);
        }
        if !removed {
            // The row came back, DISABLED, and the job fails.
            //
            // The row is deleted before the helper runs so that a failure here
            // cannot leave an alert or a stale row behind — but that same
            // ordering turned a refused teardown into a live export with no
            // record: the client keeps its disk and the UI has nothing to
            // press. Restoring it is what keeps the two in step.
            //
            // `enabled = false` on purpose: that makes the row's verdict
            // `Remove`, so `sweep_removals` keeps trying to take it out of the
            // kernel on every tick instead of re-exporting it. The admin sees
            // a red target that says why, and pressing delete again is a
            // retry rather than a second orphan.
            let mut back = restore;
            back.enabled = false;
            back.state = "error".to_string();
            back.state_detail =
                "the kernel refused to remove this target — it is still exported. \
                 The node keeps trying; see the job log."
                    .to_string();
            back.updated_at = store::now();
            // Back under the organisation that owned it: a restored row with
            // no owner would be a live export nobody can see.
            if let Err(e) = store::upsert_target(&db, &org_id, &back) {
                h.log(format!("the target row could not be restored: {e}"));
            } else {
                h.log(format!(
                    "{}: still in the kernel, so the target was put back as disabled rather \
                     than left as an export nothing knows about",
                    back.name
                ));
            }
            return Err(anyhow::anyhow!(
                "the target is still in the kernel; it was not deleted"
            ));
        }
        // This organisation's rows only, plus the orphan sweep: the delete
        // is the path that produces orphans (a delete whose job then
        // failed), so it is where they are swept. It does NOT re-apply the
        // other organisations' targets. `remove` above took out one target
        // and nothing another target serves (a shared nvmet port or host
        // object is left while anybody still links it), so none of theirs
        // needs re-applying — and a node-wide apply's log is the helper's
        // plan of every tenant's targets (backstore names, client host NQNs,
        // portal addresses), which no filter of this job's log can be
        // trusted to cut out. Their rows are the node's own reconcile's
        // business, logged in the node log; this job mentions neither
        // them nor the orphans it sweeps (`apply_for_org`).
        for line in tentanas::targets::apply_for_org(&db, &cipher, explicit.as_deref(), None, &org_id).await? {
            h.log(line);
        }
        drop(explicit);
        h.progress(100);
        Ok(())
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response(ctx, job))
}

/// The asking organisation's own fleet shares only: the registry holds every
/// tenant's, and each line names a share (`fleet_mounts` module header).
fn fleet_mounts_list(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    Ok(tn(P::FleetMountsListResponse {
        mounts: tentanas::fleet_mounts::fleet_mounts(ctx, &g.addon_id, &g.org_id),
    }))
}

async fn fleet_mount_retry(
    ctx: &HandlerContext,
    share_id: &str,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate_shares(ctx)?;
    // Another organisation's share is refused with the answer an id that
    // exists nowhere gets, so the refusal confirms nothing — and before the
    // channel is armed, which a refused request has no use for. An empty id
    // is the whole pass the minute tick runs anyway; it reveals nothing, and
    // the list it answers with is scoped like any other.
    if !share_id.is_empty()
        && !tentanas::fleet_mounts::may_retry(&ctx.state.db, &g.addon_id, &g.db, &g.org_id, share_id)
    {
        return Err(ProtocolError::not_found("share not found"));
    }
    // A one-shot password arms the channel for the length of this pass, which
    // is exactly the mode B case the retry button exists for.
    if let Some(secret) = secret {
        tentanas::elevation::arm(&g.db, secret.0.clone(), 0)
            .await
            .map_err(|e| ProtocolError::new(ProtocolErrorCode::PolicyDenied, e.to_string()))?;
    }
    let only = (!share_id.is_empty()).then_some(share_id);
    tentanas::fleet_mounts::reconcile(&ctx.state.db, &g.addon_id, &g.db, only).await;
    fleet_mounts_list(ctx)
}

// ----- configuration export / import ----------------------------------------------------

async fn config_export(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    // The asking organisation's shares, accounts and targets only; the node's
    // whole state is what the uninstall backup writes, never a download.
    let document = tentanas::config_io::export_for_org(&g.db, &g.org_id)
        .await
        .map_err(|e| internal("config export", e))?;
    let json = serde_json::to_string_pretty(&document).map_err(|e| internal("config export", e))?;
    Ok(tn(P::ConfigExportResponse {
        filename: tentanas::config_io::filename(&document),
        json,
    }))
}

async fn config_import_plan(ctx: &HandlerContext, json: &str) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    let document =
        tentanas::config_io::parse(json).map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let live = tentanas::config_io::live_state(&g.db, &elastic_owner(&g))
        .await
        .map_err(|e| internal("config import", e))?;
    let (items, warnings) = tentanas::config_io::plan(&document, &live);
    Ok(tn(P::ConfigImportPlanResponse { items, warnings }))
}

async fn config_import_apply(
    ctx: &HandlerContext,
    json: &str,
    secret: Option<&SudoSecret>,
    origin: Origin,
) -> Result<MessageBody, ProtocolError> {
    super::app_gate::require_app_permission(ctx, tentanas::PACKAGE_ID, PERM_SHARES)?;
    let g = gate_destructive(ctx)?;
    let document =
        tentanas::config_io::parse(json).map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let subject = if document.node_name.is_empty() {
        document.node_id.clone()
    } else {
        document.node_name.clone()
    };
    // An import that only creates what is missing is not a red path; one that
    // replaces a schedule already running here is (§5.10).
    if origin == Origin::Direct {
        let live = tentanas::config_io::live_state(&g.db, &elastic_owner(&g))
            .await
            .map_err(|e| internal("config import", e))?;
        let (items, _) = tentanas::config_io::plan(&document, &live);
        let overwritten = tentanas::config_io::overwritten(&items);
        if !overwritten.is_empty() && tentanas::approvals::required(&actor(ctx, &g)?) {
            // The stored subject stays what the document says (resolved on
            // every read); the alert's English title is forwarded as written,
            // so it names the node now, or nothing — never its id.
            let shown = config_import_subject(&subject, |id| tentanas::fleet::node_name(ctx, id));
            return park_shown(
                ctx,
                &g,
                tentanas::approvals::OP_CONFIG_IMPORT,
                &subject,
                &shown,
                CodedText::new(
                    "config_import",
                    &[
                        ("count", overwritten.len().to_string()),
                        ("items", overwritten.join(", ")),
                        ("schedules", tentanas::config_io::overwritten_schedules_json(&items)),
                    ],
                    format!("overwrites {}: {}", overwritten.len(), overwritten.join(", ")),
                ),
                &P::ConfigImportApplyRequest {
                    json: json.to_string(),
                    sudo_password: None,
                },
            );
        }
    }
    let explicit = secret.map(token);
    let main_db = ctx.state.db.clone();
    // The importing organisation: only its own Elastic unions may receive a
    // share from the document (`config_io::live_state`).
    let owner = elastic_owner(&g);
    // The importing organisation's job: its log names the shares, accounts
    // and targets the import creates for it (migration 20).
    let job = tentanas::jobs::spawn_owned(&g.db, "config_import", &subject, &g.user_id, Some(g.org_id.as_str()), None, None, move |h| async move {
        let outcome =
            tentanas::config_io::apply(&h, &main_db, &owner, document, explicit.as_deref()).await;
        drop(explicit);
        outcome
    })
    .map_err(|e| internal("job", e))?;
    Ok(job_response(ctx, job))
}

// ----- ARC ---------------------------------------------------------------------------

/// The ARC card. The pool list is what tells a log vdev from a cache vdev, so
/// it is read here and handed to the parser rather than guessed from names.
async fn arc_response(g: &Gate) -> Result<MessageBody, ProtocolError> {
    if !tentanas::arc::present() {
        return Ok(tn(P::ArcStatsResponse { arc: None }));
    }
    let pools = tentanas::pools::collect(&g.db).await.unwrap_or_default();
    Ok(tn(P::ArcStatsResponse {
        arc: tentanas::arc::stats(&pools),
    }))
}

async fn arc_stats(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    arc_response(&g).await
}

async fn arc_limit_set(
    ctx: &HandlerContext,
    max_bytes: u64,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate_admin(ctx)?;
    if !tentanas::arc::present() {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "this node has no ZFS ARC to limit",
        ));
    }
    // The same rule the helper enforces on the root side, checked here so the
    // dialog gets a reason instead of a channel error.
    tentanas_helper::validate_arc_max(max_bytes, tentanas_helper::meminfo_total_bytes())
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let command = HelperCommand::ArcLimitSet { max_bytes };
    let explicit = secret.map(token);
    let (out, _) = tentanas::broker::run_privileged(
        &g.db,
        &command,
        explicit.as_deref(),
        Duration::from_secs(30),
    )
    .await
    .map_err(|e| broker_error("arc limit", e))?;
    drop(explicit);
    if !out.success() {
        return Err(ProtocolError::internal(format!(
            "setting the ARC limit failed: {}",
            out.stderr.trim().lines().next().unwrap_or("no output")
        )));
    }
    // Read back rather than echo the request: the module clamps what it
    // accepts, and the card must show what is actually in force.
    arc_response(&g).await
}

async fn snapshot_browse(
    ctx: &HandlerContext,
    snapshot: &str,
    path: &str,
) -> Result<MessageBody, ProtocolError> {
    gate(ctx, PERM_READ)?;
    let (path, entries) = tentanas::snapshots::browse(snapshot, path)
        .await
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    Ok(tn(P::SnapshotBrowseResponse { path, entries }))
}

/// A parked config import names the exporting node; see
/// `config_import_subject`. An unnamed one loses the parameter, and the
/// screen words the alert without a subject. The English title (the
/// tooltip) is rewritten from the same resolution: a row parked before the
/// title was resolved at raise time carries the node's id in it, and a node
/// named since then is named now.
fn resolve_import_alerts(alerts: &mut [tentaflow_protocol::tentanas::NasAlert], name_of: impl Fn(&str) -> String) {
    for alert in alerts.iter_mut() {
        let import = alert.code == "approval_pending"
            && alert.params.get("operation").map(String::as_str) == Some(tentanas::approvals::OP_CONFIG_IMPORT);
        if !import {
            continue;
        }
        let Some(subject) = alert.params.get("subject").cloned() else {
            continue;
        };
        let shown = config_import_subject(&subject, &name_of);
        alert.title = tentanas::approvals::approval_alert_title(&shown);
        if shown.is_empty() {
            alert.params.remove("subject");
        } else {
            alert.params.insert("subject".to_string(), shown);
        }
    }
}

fn alerts_list(ctx: &HandlerContext, include_acked: bool) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    // Scoped like the job list: shared hardware for everyone, an alert about
    // an array only for the organisation that owns it (migration 18).
    let mut alerts = store::list_alerts_for_org(&g.db, org_viewer(ctx, &g), include_acked)
        .map_err(|e| internal("alerts", e))?;
    resolve_import_alerts(&mut alerts, |id| tentanas::fleet::node_name(ctx, id));
    name_branch_paths(&g.db, alerts.iter_mut().flat_map(|a| [&mut a.title, &mut a.detail]));
    Ok(tn(P::AlertsListResponse { alerts }))
}

fn alert_ack(ctx: &HandlerContext, alert_id: &str) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    if !store::ack_alert_for_org(&g.db, org_viewer(ctx, &g), alert_id).map_err(|e| internal("alerts", e))? {
        return Err(ProtocolError::not_found("alert not found or already acknowledged"));
    }
    alerts_list(ctx, false)
}

// ----- four eyes (§5.10) --------------------------------------------------------------

/// Asks for one snapshot's protection to be lifted (§5.10).
///
/// With a second admin who could approve, this parks whatever the four-eyes
/// switch says: the approved release is the only way a hold comes off, and
/// that is what "protected" was promised to mean. With FEWER than two such
/// admins (owner ruling 2026-09-03) there is nobody to be the second pair, so
/// it runs here as an ordinary red path — the snapshot name retyped plus the
/// sudo password — because a protection nobody can ever lift would leave the
/// pool without a way out of the GUI. The count is taken from the node's own
/// membership data, so no client can choose which of the two happens.
async fn snapshot_protection_release(
    ctx: &HandlerContext,
    snapshot: &str,
    reason: &str,
    confirm_snapshot: &str,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate_destructive(ctx)?;
    tentanas_helper::validate_snapshot_name(snapshot)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let live = tentanas::snapshots::list("", "", false)
        .await
        .map_err(|e| broker_error("snapshots", e))?;
    let row = live
        .iter()
        .find(|s| s.name == snapshot)
        .ok_or_else(|| ProtocolError::not_found("the snapshot is not on this node"))?;
    if !tentanas::snapshots::is_protected(row) {
        return Err(ProtocolError::bad_request(
            "the snapshot carries no hold — there is no protection to lift",
        ));
    }
    if tentanas::approvals::second_pair_available(&actor(ctx, &g)?) {
        // The author's reason is data, carried as written.
        let mut params = vec![("snapshot", snapshot.to_string())];
        let text = if reason.trim().is_empty() {
            format!("lifts the protection of {snapshot}")
        } else {
            params.push(("reason", reason.trim().to_string()));
            format!("lifts the protection of {snapshot} — {}", reason.trim())
        };
        return park(
            ctx,
            &g,
            tentanas::approvals::OP_SNAPSHOT_RELEASE,
            snapshot,
            CodedText::new("snapshot_release", &params, text),
            &P::SnapshotProtectionReleaseRequest {
                snapshot: snapshot.to_string(),
                reason: reason.to_string(),
                confirm_snapshot: String::new(),
                sudo_password: None,
            },
        );
    }
    // The single-admin red path: the retype is the only gate left, so it is
    // enforced here rather than trusted to the dialog.
    require_confirm(snapshot, confirm_snapshot)?;
    let job = tentanas::snapshots::spawn_release(&g.db, snapshot, &g.user_id, secret.map(token))
        .map_err(|e| internal("snapshot release", e))?;
    Ok(job_response(ctx, job))
}

// ----- the file access audit (§5.10) --------------------------------------------------

/// One page of the "Dziennik dostępu" when the client asks for no size.
const ACCESS_LOG_PAGE: u32 = 500;

/// The access log with its filters. Reading the audit is `nas.read`: it is a
/// READ of this node's own log, and an operator who may see the disks and the
/// shares may see who touched them. Changing what is audited is a share
/// change, and that stays `nas.shares.manage`.
async fn access_log(
    ctx: &HandlerContext,
    filter: &store::AccessFilter<'_>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    // Opening the view collects, subject to the collector's own cadence. In
    // mode A the schedule loop already did it; in mode B, where there IS no
    // unattended loop, this is the only moment the log can fill at all — and
    // an armed session is exactly when the admin is looking.
    tentanas::access_log::collect_tick(&g.db).await;
    if !filter.result.is_empty() && filter.result != "ok" && filter.result != "fail" {
        return Err(ProtocolError::bad_request(
            "the result filter is 'ok', 'fail' or empty",
        ));
    }
    // The asking organisation's lines, facets and audit state only
    // (migration 20): the log names shares, accounts, client addresses and
    // files, and every tenant's are in this one node database.
    let (events, total) =
        store::access_events(&g.db, &g.org_id, filter).map_err(|e| internal("access log", e))?;
    let (shares, users, operations) =
        store::access_facets(&g.db, &g.org_id).map_err(|e| internal("access log", e))?;
    let viewer = forward_viewer(ctx);
    Ok(tn(P::AccessLogResponse {
        events,
        total,
        audit: tentanas::access_log::state_for_org(&g.db, &g.org_id),
        shares,
        users,
        operations,
        forward: tentanas::forward::settings(
            &ctx.state.db,
            &g.db,
            &g.addon_id,
            tentanas::forward::Target::Org(&g.org_id),
            viewer,
        ),
        forward_node: tentanas::forward::settings(
            &ctx.state.db,
            &g.db,
            &g.addon_id,
            tentanas::forward::Target::Node,
            viewer,
        ),
    }))
}

/// Who may see the forwarding targets' addresses: the caller's organisation's
/// admins (the same gate that sets them), and even they only masked; every
/// other reader of the log sees whether forwarding is on, never where to
/// (critic wave 9b, MAJOR 5: a webhook URL is a bearer secret).
fn forward_viewer(ctx: &HandlerContext) -> tentanas::forward::Viewer {
    if gate_admin(ctx).is_ok() {
        tentanas::forward::Viewer::Admin
    } else {
        tentanas::forward::Viewer::Reader
    }
}

/// Where the alert pipeline goes (§5.9): the asking organisation's own
/// target, or — `node_wide` — the deletion of the retired node-wide one.
/// Both are fleet-wide settings, so they need the same gate the four-eyes
/// switch does. The organisation is the caller's own, never one the request
/// names.
#[allow(clippy::too_many_arguments)]
async fn alert_forward_set(
    ctx: &HandlerContext,
    enabled: bool,
    syslog_target: &str,
    webhook_url: &str,
    include_access: bool,
    node_wide: bool,
) -> Result<MessageBody, ProtocolError> {
    let g = gate_admin(ctx)?;
    if node_wide {
        // Retired (owner decision 2026-09-26): deleting it is the one change
        // left — an "off, no address" request — and nothing edits or creates
        // one.
        if enabled || !syslog_target.trim().is_empty() || !webhook_url.trim().is_empty() {
            return Err(ProtocolError::new(ProtocolErrorCode::Conflict, tentanas::forward::FORWARD_NODE_RETIRED));
        }
        tentanas::forward::delete_node_target(&ctx.state.db, &g.db, &g.addon_id)
            .map_err(|e| internal("forwarding", e))?;
    } else {
        tentanas::forward::set_settings(
            &ctx.state.db,
            &g.db,
            &g.addon_id,
            &g.user_id,
            &g.org_id,
            enabled,
            syslog_target,
            webhook_url,
            include_access,
        )
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    }
    access_log(
        ctx,
        &store::AccessFilter {
            limit: ACCESS_LOG_PAGE,
            ..Default::default()
        },
    )
    .await
}

fn approvals_response(ctx: &HandlerContext, g: &Gate) -> Result<MessageBody, ProtocolError> {
    approvals_view(ctx, g, false)
}

fn approvals_view(
    ctx: &HandlerContext,
    g: &Gate,
    include_closed: bool,
) -> Result<MessageBody, ProtocolError> {
    let a = actor(ctx, g)?;
    let mut approvals =
        tentanas::approvals::list(&a, include_closed).map_err(|e| internal("approvals", e))?;
    let ids: Vec<String> = approvals
        .iter()
        .flat_map(|r| [Some(r.requested_by.clone()), r.decided_by.clone()])
        .flatten()
        .collect();
    let names = display_names(ctx, &ids);
    for row in approvals.iter_mut() {
        if let Some(name) = names.get(&row.requested_by) {
            row.requested_by = name.clone();
        }
        if row.operation == tentanas::approvals::OP_CONFIG_IMPORT {
            row.subject = config_import_subject(&row.subject, |id| tentanas::fleet::node_name(ctx, id));
        }
        if let Some(name) = row.decided_by.as_ref().and_then(|id| names.get(id)).cloned() {
            row.decided_by = Some(name);
        }
    }
    name_branch_paths(a.nas_db, approvals.iter_mut().map(|r| &mut r.detail));
    Ok(tn(P::ApprovalsListResponse {
        approvals,
        settings: tentanas::approvals::settings(a.main_db, a.checker, a.org_id, a.addon_id),
    }))
}

async fn approvals_list(
    ctx: &HandlerContext,
    include_closed: bool,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    approvals_view(ctx, &g, include_closed)
}

fn approval_settings_set(
    ctx: &HandlerContext,
    enabled: bool,
    ttl_hours: u32,
) -> Result<MessageBody, ProtocolError> {
    let g = gate_admin(ctx)?;
    tentanas::approvals::set_settings(&actor(ctx, &g)?, enabled, ttl_hours)
        .map_err(|e| internal("approvals", e))?;
    approvals_response(ctx, &g)
}

/// Approves or rejects one parked operation. An approval replays the stored
/// request with `Origin::Approved`, so it executes instead of parking again,
/// and the row records the job it started.
async fn approval_decide(
    ctx: &HandlerContext,
    request_id: &str,
    approve: bool,
    note: &str,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate_admin(ctx)?;
    if !approve {
        tentanas::approvals::reject(&actor(ctx, &g)?, request_id, note).map_err(approval_error)?;
        return approvals_response(ctx, &g);
    }
    let row = tentanas::approvals::claim(&actor(ctx, &g)?, request_id).map_err(approval_error)?;
    let payload =
        tentanas::approvals::stored_payload(&row).map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let outcome = execute_approved(ctx, &payload, secret).await;
    let job_id = match &outcome {
        Ok(MessageBody::TentaNasBody(P::JobResponse { job })) => Some(job.job_id.clone()),
        _ => None,
    };
    tentanas::approvals::finish(&actor(ctx, &g)?, request_id, job_id.as_deref());
    // A failed execution is reported as itself: the operation is closed
    // 'failed' and the admin sees why, rather than a list that silently
    // dropped the request.
    outcome?;
    approvals_response(ctx, &g)
}

/// Runs a released request. Exhaustive on purpose: only the operations listed
/// here are ever parked, and a new one must be added consciously — the match
/// below is what decides whether an approval an admin granted can actually be
/// carried out.
async fn execute_approved(
    ctx: &HandlerContext,
    payload: &P,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    match payload {
        // The acknowledgement the AUTHOR gave travels with the parked request:
        // the approver releases that decision, and the Sync over the fault is
        // judged again, with it, when it runs.
        P::ElasticArraySyncRequest {name,acknowledge_parity_fault,..} => elastic_snapraid(ctx,name,secret,Origin::Approved,
            tentanas_helper::elastic::ElasticSnapraidKind::Sync,acknowledge_parity_fault.clone()).await,
        P::ElasticArrayScrubRequest {name,..} => elastic_snapraid(ctx,name,secret,Origin::Approved,
            tentanas_helper::elastic::ElasticSnapraidKind::Scrub,None).await,
        P::ElasticArrayFixRequest {name,disk,confirm_disk,..} => {
            require_confirm(disk,confirm_disk)?;
            elastic_snapraid(ctx,name,secret,Origin::Approved,
                tentanas_helper::elastic::ElasticSnapraidKind::Fix { disk: disk.clone() },None).await
        }
        P::ElasticArrayAddDiskRequest {name,disk_id,confirm_name,..} =>
            elastic_add_disk(ctx,name,disk_id,confirm_name,secret,Origin::Approved).await,
        P::ElasticArrayAddDiskAbortRequest {name,disk_id,confirm_name,..} =>
            elastic_add_disk_abort(ctx,name,disk_id,confirm_name,secret,Origin::Approved).await,
        P::ElasticArrayReplaceDiskRequest {name,disk,confirm_disk,replacement_disk_id,accept_stale_parity,..} =>
            elastic_replace_disk(ctx,name,disk,confirm_disk,replacement_disk_id,*accept_stale_parity,
                secret,Origin::Approved).await,
        P::ElasticArrayDestroyRequest {name,confirm_name,..} =>
            elastic_destroy(ctx,name,confirm_name,secret,Origin::Approved).await,
        P::ElasticArrayMoverRequest {name,..} => elastic_mover(ctx,name,secret,Origin::Approved).await,
        P::ElasticMoverScheduleSetRequest {name,enabled,schedule,min_age_secs,cache_min_free_pct,coupled_sync} =>
            elastic_schedule_set(ctx,store::ElasticTask::Mover,name,*enabled,schedule,
                (*min_age_secs,*cache_min_free_pct,*coupled_sync),Origin::Approved).await,
        P::ElasticSyncScheduleSetRequest {name,enabled,schedule} =>
            elastic_schedule_set(ctx,store::ElasticTask::Sync,name,*enabled,schedule,(None,None,None),Origin::Approved).await,
        P::ElasticScrubScheduleSetRequest {name,enabled,schedule} =>
            elastic_schedule_set(ctx,store::ElasticTask::Scrub,name,*enabled,schedule,(None,None,None),Origin::Approved).await,
        P::ElasticArrayCreateRequest {name,filesystem,data_disk_ids,parity_disk_ids,cache_disk_ids,confirm_name,..} =>
            elastic_create(ctx,name,filesystem,data_disk_ids,parity_disk_ids,cache_disk_ids,confirm_name,secret,Origin::Approved).await,
        P::PoolDestroyRequest {
            name, confirm_name, ..
        } => pool_destroy(ctx, name, confirm_name, secret, Origin::Approved).await,
        P::ShareDeleteRequest {
            share_id,
            confirm_name,
            ..
        } => share_delete(ctx, share_id, confirm_name, secret, Origin::Approved).await,
        P::TargetDeleteRequest {
            target_id,
            confirm_name,
            ..
        } => target_delete(ctx, target_id, confirm_name, secret, Origin::Approved).await,
        P::ConfigImportApplyRequest { json, .. } => {
            config_import_apply(ctx, json, secret, Origin::Approved).await
        }
        P::SnapshotProtectionReleaseRequest { snapshot, .. } => {
            let g = gate_destructive(ctx)?;
            let explicit = secret.map(token);
            let job = tentanas::snapshots::spawn_release(&g.db, snapshot, &g.user_id, explicit)
                .map_err(|e| internal("snapshot release", e))?;
            Ok(job_response(ctx, job))
        }
        other => Err(ProtocolError::bad_request(format!(
            "'{}' is not an approvable operation",
            variant_of(other)
        ))),
    }
}

// ----- Elastic Array (§5.3) ---------------------------------------------------------

fn elastic_owner(g: &Gate) -> ElasticOwner {
    ElasticOwner { org_id: g.org_id.clone(), addon_id: g.addon_id.clone() }
}

/// The asking organisation's array names, for `disks::hide_other_org_array`.
/// An unreadable table is an error, not an empty set: an empty set would call
/// every one of this organisation's own arrays "another organisation's".
fn own_array_names(g: &Gate) -> Result<std::collections::BTreeSet<String>, ProtocolError> {
    store::elastic_array_names_of_org(&g.db, &g.org_id).map_err(|e| internal("elastic arrays", e))
}

/// Every organisation this node has a record of — the set an Elastic
/// journal's owner is judged against (`tentanas::elastic::owner_kind`).
///
/// WHY it matters: TentaNas is ONE package instance per node, shared by every
/// organisation on it, and a journal of another organisation that lives here
/// is that tenant's array — hidden from the import scan, refused for
/// adoption and refused for a wipe. A journal whose organisation this node
/// has never heard of came in on disks from another machine and stays
/// adoptable. This lookup is what tells the two apart.
///
/// Every row counts, whatever its status: a soft-deleted organisation
/// (`status = 'deleted'`, `services::org::delete_organization`) keeps its row
/// and its data under its retention policy, and letting another tenant adopt
/// its array would hand that tenant the deleted one's files. A failed read is
/// an error, never an empty set — an empty set would make every other tenant
/// "unknown", which is exactly the case that is shown and adoptable.
fn orgs_on_node(ctx: &HandlerContext) -> Result<std::collections::BTreeSet<String>, ProtocolError> {
    orgs_on_node_of(ctx)
}

/// Who reads the job and alert lists (`store::OrgViewer`): the caller's
/// organisation, and whether it is the sole organisation here — then the
/// rows whose owner is gone (a dissolved array's jobs and alerts, migration
/// 18) are its to see (owner decisions, wave 5 and 2026-09-26). A failed
/// read of either set answers "not the sole one": hidden, never leaked.
fn org_viewer<'a>(ctx: &HandlerContext, g: &'a Gate) -> store::OrgViewer<'a> {
    let sole_org = match (org_statuses_of(ctx), store::orgs_with_resources(&g.db)) {
        (Ok(statuses), Ok(owning)) => is_sole_org(&statuses, &owning, &g.org_id),
        _ => false,
    };
    store::OrgViewer { org_id: &g.org_id, sole_org }
}

/// Whether `org_id` is "the sole organisation": itself `active`, and no
/// OTHER organisation that still exists owns resources on this node
/// (`owning`). Only a soft-deleted organisation (`deleted`) is ignored; a
/// suspended one is still present — a suspension is reversible, and its
/// rows must not become another tenant's to see meanwhile (critic wave 9a) —
/// and so is an owner id no organisation row names (nothing says it is gone).
/// The viewer need not own anything here: an organisation that dissolved its
/// only array still sees that array's rows. `statuses` is org id → status.
fn is_sole_org(
    statuses: &std::collections::BTreeMap<String, String>,
    owning: &std::collections::BTreeSet<String>,
    org_id: &str,
) -> bool {
    !org_id.is_empty()
        && statuses.get(org_id).is_some_and(|status| status == "active")
        && !owning
            .iter()
            .any(|org| org != org_id && statuses.get(org).is_none_or(|status| status != "deleted"))
}

/// Every organisation's status, by id.
fn org_statuses_of(ctx: &HandlerContext) -> Result<std::collections::BTreeMap<String, String>, ProtocolError> {
    crate::services::org::list_organizations(&ctx.state.db, None)
        .map(|orgs| orgs.into_iter().map(|org| (org.org_id, org.status)).collect())
        .map_err(|e| internal("organisations", e))
}

fn orgs_on_node_of(ctx: &HandlerContext) -> Result<std::collections::BTreeSet<String>, ProtocolError> {
    crate::services::org::list_organizations(&ctx.state.db, None)
        .map(|orgs| orgs.into_iter().map(|org| org.org_id).collect())
        .map_err(|e| internal("organisations", e))
}

/// The one way this module reads the root's Elastic claims.
///
/// WHY it exists: three call sites — the capabilities read, the plan the
/// wizard asks for, and `elastic_create` just before it formats anything —
/// each unwrapped the `anyhow` erasure themselves, so each was its own chance
/// to reach for `internal()`. That is precisely what turned "this node has no
/// privilege channel" into `tentanas elastic namespace failed` on a fresh
/// install. Funnelling them through one function leaves one place to get the
/// classification right, and one place a test can hold it to.
async fn root_claims(
    g: &Gate,
    scope: &str,
    name: Option<&str>,
    explicit: Option<&ElevationToken>,
) -> Result<tentanas_helper::elastic::ElasticClaimsResult, ProtocolError> {
    tentanas::elastic::claims(&g.db, name, explicit)
        .await
        .map_err(|e| privileged_error(scope, e))
}

async fn arrays_claiming_disks(g: &Gate) -> Result<std::collections::BTreeSet<String>, ProtocolError> {
    let mut claims = store::elastic_claims(&g.db).map_err(|e| internal("elastic claims",e))?;
    claims.extend(root_claims(g,"elastic root claims",None,None).await?.disks);
    Ok(tentanas::elastic::claimed_disk_ids(&tentanas::disks::snapshot().0,&claims))
}

#[allow(clippy::too_many_arguments)]
async fn elastic_create(ctx: &HandlerContext,name: &str,filesystem: &str,
    data_disk_ids: &[String],parity_disk_ids: &[String],cache_disk_ids: &[String],confirm_name: &str,
    secret: Option<&SudoSecret>,origin: Origin) -> Result<MessageBody,ProtocolError> {
    let g = gate_destructive(ctx)?;
    require_confirm(name,confirm_name)?;
    tentanas_helper::elastic::validate_array_name(name).map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let filesystem_kind = match filesystem {
        "ext4" => ElasticFilesystem::Ext4,
        "xfs" => ElasticFilesystem::Xfs,
        _ => return Err(ProtocolError::bad_request("Wymagany filesystem xfs albo ext4")),
    };
    if data_disk_ids.is_empty() || parity_disk_ids.len() > 2 || cache_disk_ids.len() > 1
        || data_disk_ids.len() + parity_disk_ids.len() + cache_disk_ids.len() > 32
        || data_disk_ids.iter().chain(parity_disk_ids).chain(cache_disk_ids).any(|id| id.is_empty() || id.len() > 128) {
        return Err(ProtocolError::bad_request("Nieprawidłowy zestaw dysków Elastic"));
    }
    if origin == Origin::Direct && tentanas::approvals::required(&actor(ctx,&g)?) {
        return park(ctx,&g,tentanas::approvals::OP_ELASTIC_CREATE,name,
            CodedText::new("elastic_create", &[("array", name.to_string())],
                "formats the picked disks and creates the Elastic Array"),
            &P::ElasticArrayCreateRequest { name:name.to_string(),filesystem:filesystem.to_string(),
                data_disk_ids:data_disk_ids.to_vec(),parity_disk_ids:parity_disk_ids.to_vec(),cache_disk_ids:cache_disk_ids.to_vec(),
                confirm_name:confirm_name.to_string(),sudo_password:None });
    }
    tentanas::disks::refresh_inventory(&g.db).await.map_err(|e| internal("inventory",e))?;
    let data = disks_by_id(data_disk_ids,true)?;
    let parity = optional_disks(parity_disk_ids,&|ids| disks_by_id(ids,true))?;
    let cache = optional_disks(cache_disk_ids,&|ids| disks_by_id(ids,true))?;
    let explicit = secret.map(token);
    let global = root_claims(&g,"elastic root claims",Some(name),explicit.as_deref()).await?;
    if global.name_claimed != Some(false) || global.namespace_clear != Some(true) {
        return Err(ProtocolError::bad_request("Nazwa lub przestrzeń montowania jest zajęta albo niepotwierdzona"));
    }
    let mut claims = store::elastic_claims(&g.db).map_err(|e| internal("elastic claims",e))?;
    claims.extend(global.disks);
    let selected: Vec<NasDisk> = data.iter().chain(&parity).chain(&cache).cloned().collect();
    let taken = tentanas::elastic::claimed_disk_ids(&selected,&claims);
    let features = tentanas::environment::cached_or_probe(&g.db).await
        .map_err(|e| internal("elastic features",e))?.features;
    let capabilities = tentanas::elastic::capabilities(&features,&has_mkfs);
    if !capabilities.mergerfs || (!parity.is_empty() && !capabilities.snapraid)
        || !capabilities.filesystems.iter().any(|fs| fs == filesystem) {
        return Err(ProtocolError::new(ProtocolErrorCode::NotAvailable,"Brak działających narzędzi macierzy"));
    }
    // Pusty zbiór nazw wynika z pozytywnego odczytu namespace przez roota, nie błędu zpool.
    let plan = tentanas::elastic::plan_layout(name,filesystem,&data,&parity,&cache,&taken,
        &std::collections::BTreeSet::new(),&capabilities.filesystems,&tentanas_helper::elastic::Tools::for_preview());
    if !plan.refusals.is_empty() {
        return Err(ProtocolError::bad_request(plan.refusals.iter().map(|r| r.detail.as_str()).collect::<Vec<_>>().join("; ")));
    }
    let disk_spec = |disk: &NasDisk| ElasticDiskSpec {
        disk_id:disk.disk_id.clone(),wwn:disk.wwn.clone().filter(|w| !w.is_empty()),
        serial:(!disk.serial.is_empty()).then(||disk.serial.clone()),bytes:disk.size_bytes,
        expected_uuid:uuid::Uuid::new_v4().to_string(),
    };
    let spec = ElasticCreateSpec { array_id:uuid::Uuid::now_v7().to_string(),
        operation_id:uuid::Uuid::now_v7().to_string(),owner:elastic_owner(&g),name:name.to_string(),
        filesystem:filesystem_kind,data:data.iter().map(disk_spec).collect(),
        cache:cache.first().map(disk_spec),parity:parity.iter().map(disk_spec).collect() };
    spec.validate().map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let intent = tentanas::jobs::ElasticJobIntent::Create(spec.clone());
    let job = tentanas::jobs::spawn(&g.db,"elastic_create",name,&g.user_id,Some(intent),None,
        move |h| tentanas::elastic::create_job(h,spec,explicit)).map_err(|e| internal("elastic create",e))?;
    Ok(job_response(ctx, job))
}

async fn elastic_restore(ctx: &HandlerContext,name: &str,secret: Option<&SudoSecret>) -> Result<MessageBody,ProtocolError> {
    let g = gate_admin(ctx)?;
    let row = store::elastic_array(&g.db,&elastic_owner(&g),name)
        .map_err(|e| internal("elastic array",e))?
        .ok_or_else(||ProtocolError::not_found("Macierz nie istnieje w tej instancji"))?;
    let job = tentanas::elastic::spawn_restore(&g.db,&row,&g.user_id,secret.map(token),None)
        .map_err(|e| internal("elastic restore",e))?;
    Ok(job_response(ctx, job))
}

/// A refusal from the import path is an answer the admin has to read — "this
/// member no longer carries the UUID its journal recorded" — and not a server
/// fault. Only the privilege channel failing is, and that arrives as a
/// `BrokerError`, so it keeps `broker_error`'s classification.
fn import_error(scope: &str, error: anyhow::Error) -> ProtocolError {
    match error.downcast::<BrokerError>() {
        Ok(broker) => broker_error(scope, broker),
        Err(other) => ProtocolError::bad_request(other.to_string()),
    }
}

/// Arrays this node holds on disk but has no database record of. Read-only,
/// and privileged for the same reason `pool_import_scan` is: the journals live
/// under `/var/lib/tentanas`, which is `0700 root`, while the service runs
/// unprivileged — so there is no unprivileged way to learn that a lost array
/// exists at all.
async fn elastic_import_scan(
    ctx: &HandlerContext,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate_destructive(ctx)?;
    let explicit = secret.map(token);
    // Another tenant of this node never appears in this list at all
    // (`orgs_on_node`), so nothing below can name one.
    let orgs = orgs_on_node(ctx)?;
    let mut candidates =
        tentanas::elastic::import_scan(&g.db, &elastic_owner(&g), &orgs, explicit.as_deref())
            .await
            .map_err(|e| privileged_error("elastic import scan", e))?;
    for candidate in candidates.iter_mut() {
        candidate.owner_instance_name = own_org_instance_name(&candidate.owner_kind, || {
            addon_display_name(ctx, &candidate.owner_addon_id)
        });
    }
    Ok(tn(P::ElasticArrayImportScanResponse { candidates }))
}

/// Adopts one scanned array into THIS addon's database, and re-owns its
/// journal so the adopted array can actually be operated. Answers with the
/// adopted array, so the screen that asked can show what it now has.
async fn elastic_import(
    ctx: &HandlerContext,
    array_id: &str,
    confirm_name: &str,
    secret: Option<&SudoSecret>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate_destructive(ctx)?;
    tentanas_helper::elastic::validate_elastic_uuid(array_id)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let owner = elastic_owner(&g);
    // The same tenant rule as the scan: another org of this node's journal is
    // not a candidate, so the adoption refuses it like a journal that is not
    // there — hiding it in the dialog alone would not stop a crafted request.
    let orgs = orgs_on_node(ctx)?;
    let explicit = secret.map(token);
    let name = tentanas::elastic::import_apply(
        &g.db,
        &owner,
        &orgs,
        array_id,
        confirm_name,
        &g.user_id,
        explicit.as_deref(),
    )
    .await
    .map_err(|e| import_error("elastic import", e))?;
    let array = tentanas::elastic::get(&g.db, &owner, &name)
        .await
        .map_err(|e| internal("elastic get", e))?
        .ok_or_else(|| ProtocolError::not_found("Macierz nie istnieje w tej instancji"))?;
    Ok(tn(P::ElasticArrayGetResponse { array }))
}

/// The refusal of a manual Sync over a recorded Scrub or Repair fault that
/// came without the admin's acknowledgement, as the code the screen words.
const SYNC_NEEDS_ACKNOWLEDGEMENT: &str = "refusal:elastic_fault_unacknowledged";
/// ...and of one whose acknowledgement names a fault that is no longer the
/// array's: a request queued or parked over an older fault, released after
/// a newer one was recorded.
const SYNC_FAULT_CHANGED: &str = "refusal:elastic_fault_changed";

async fn elastic_snapraid(
    ctx: &HandlerContext,
    name: &str,
    secret: Option<&SudoSecret>,
    origin: Origin,
    kind: tentanas_helper::elastic::ElasticSnapraidKind,
    acknowledge_parity_fault: Option<String>,
) -> Result<MessageBody, ProtocolError> {
    use tentanas_helper::elastic::ElasticSnapraidKind;
    let g = gate_destructive(ctx)?;
    super::app_gate::require_app_permission(ctx, tentanas::PACKAGE_ID, PERM_READ)?;
    tentanas_helper::elastic::validate_array_name(name)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let array = store::elastic_array(&g.db, &elastic_owner(&g), name)
        .map_err(|e| internal("elastic array", e))?
        .ok_or_else(|| ProtocolError::not_found("Macierz nie istnieje w tej instancji"))?;
    if array.parity.is_empty() {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "SnapRAID wymaga zakończonej aktywnej macierzy z parity",
        ));
    }
    if let ElasticSnapraidKind::Fix { disk } = &kind {
        // A repair is the ONE operation an array that needs attention may
        // start, because it is the one that resolves that state: a scrub which
        // reported errors leaves exactly this array, and refusing the repair
        // here would leave it with no way out through the product.
        if !matches!(array.state.as_str(), "active" | "needs_attention") {
            return Err(ProtocolError::new(
                ProtocolErrorCode::NotAvailable,
                "Naprawa wymaga macierzy aktywnej albo wymagającej uwagi",
            ));
        }
        if !array.branches.iter().any(|b| b.role == "data" && b.name == *disk) {
            return Err(ProtocolError::bad_request(format!(
                "Macierz '{name}' nie ma dysku danych '{disk}'"
            )));
        }
        // The OBSERVED array is read here, not the database row: the button on
        // the screen reads exactly these fields off the wire, and one rule over
        // one object is what keeps the two from disagreeing.
        let observed = tentanas::elastic::get(&g.db, &elastic_owner(&g), name)
            .await
            .map_err(|e| internal("elastic get", e))?
            .ok_or_else(|| ProtocolError::not_found("Macierz nie istnieje w tej instancji"))?;
        // CAN it run, before WHETHER it should. A repair writes the named disk,
        // so a data disk that is missing or unmounted makes it impossible —
        // and that is the very state a repair is reached for, so the refusal
        // has to say what must happen first instead of handing the admin the
        // helper's `precondition_failed`.
        if let Some(blocker) = tentanas::elastic::repair_blocker(&observed) {
            return Err(ProtocolError::new(
                ProtocolErrorCode::NotAvailable,
                format!("Naprawa macierzy '{name}' nie jest teraz możliwa: {blocker}"),
            ));
        }
        // Nothing to repair — see `elastic::repair_evidence` for why a repair
        // needs evidence rather than permission.
        if tentanas::elastic::repair_evidence(&observed).is_none() {
            return Err(ProtocolError::new(
                ProtocolErrorCode::NotAvailable,
                format!(
                    "Macierz '{name}' nie zgłasza awarii dysku ani błędów parity; naprawa \
                     jest dostępna po scrubie, który wykryje błędy"
                ),
            ));
        }
    } else if !array.parity_run_available {
        // A Sync and a full Scrub are what settle a parity run that ended
        // without success, so they are offered on the array such a run left
        // behind; everything else — a mover record in flight, a half-finished
        // add — still refuses (`db::parity_admission`).
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "SnapRAID wymaga macierzy z parity bez nierozwiązanej operacji poza parity",
        ));
    }
    // What the Sync carries to the helper, and what the approver reads.
    let mut acknowledged = None;
    let mut fault_warning = false;
    if !matches!(kind, ElasticSnapraidKind::Fix { .. }) {
        // The OBSERVED array, as for a repair: the helper's recorded cause is
        // what its own gate enforces, and the wire carries exactly this
        // answer to the screen — so the button and this handler read one
        // rule over one object.
        let observed = tentanas::elastic::get(&g.db, &elastic_owner(&g), name)
            .await
            .map_err(|e| internal("elastic get", e))?
            .ok_or_else(|| ProtocolError::not_found("Macierz nie istnieje w tej instancji"))?;
        if !observed.parity_run_available {
            return Err(ProtocolError::new(
                ProtocolErrorCode::NotAvailable,
                "SnapRAID wymaga macierzy z parity bez nierozwiązanej operacji poza parity",
            ));
        }
        // I3: A SYNC OVER A RECORDED SCRUB OR REPAIR FAULT IS THE ADMIN'S
        // DECISION. Measured on rig11 (snapraid 13.0-1, probe f1-f4): the
        // marked blocks stay repairable across it, but the files the Scrub
        // could not read leave the content file for good. The screen's
        // confirm names that cost and is the only thing that sets the flag;
        // the helper refuses the Sync without it too.
        //
        // The acknowledgement is BOUND TO THE FAULT (M1 of the release
        // review): it has to name the fault this array carries now. One that
        // names another — a request parked over an older fault and released
        // after a newer Scrub recorded this one — acknowledges nothing, and
        // one the array does not need is dropped here, so no request ever
        // carries a blank cheque to the helper.
        if kind == ElasticSnapraidKind::Sync && observed.sync_needs_acknowledgement {
            match acknowledge_parity_fault.as_deref() {
                None => return Err(ProtocolError::new(ProtocolErrorCode::NotAvailable, SYNC_NEEDS_ACKNOWLEDGEMENT)),
                Some(fault) if fault != observed.sync_fault_id => {
                    return Err(ProtocolError::new(ProtocolErrorCode::NotAvailable, SYNC_FAULT_CHANGED))
                }
                Some(fault) => acknowledged = Some(fault.to_string()),
            }
            fault_warning = true;
        }
    }
    let acknowledge_parity_fault = acknowledged;
    if origin == Origin::Direct && tentanas::approvals::required(&actor(ctx, &g)?) {
        let (operation, request, description) = match &kind {
            ElasticSnapraidKind::Sync => (
                tentanas::approvals::OP_ELASTIC_SYNC,
                P::ElasticArraySyncRequest {
                    name: name.into(),
                    acknowledge_parity_fault,
                    sudo_password: None,
                },
                // A Sync over an array whose scrub reported errors is not the
                // same operation as a Sync over a healthy one, and the approver
                // reads only this sentence. MEASURED on rig11 (snapraid
                // 13.0-1, M3 f1-f4): the marked blocks of unchanged files stay
                // repairable across a Sync — an unchanged file reads `equal`
                // and gets no new parity — and so do unchanged files the scrub
                // could not read (they stay in the content file). What the
                // Sync costs is the earlier version of every file deleted or
                // changed since the previous Sync, a damaged one included. So
                // the sentence names what the approval costs, and the repair
                // is the operation to take first.
                // The warning follows the OBSERVED need — the helper's
                // recorded cause included, which a scrub whose log broke
                // leaves with no counts in the history.
                // A CODE, worded by the approver's screen in the approver's
                // language (`approvals.detail_<code>`): this is the data-loss
                // warning, and a de/en/fr/es approver has to read it.
                if fault_warning {
                    CodedText::new("elastic_sync_over_fault", &[], store::SYNC_OVER_FAULT_TEXT)
                } else {
                    CodedText::new("elastic_sync", &[], store::SYNC_TEXT)
                },
            ),
            ElasticSnapraidKind::Scrub => (
                tentanas::approvals::OP_ELASTIC_SCRUB,
                P::ElasticArrayScrubRequest {
                    name: name.into(),
                    sudo_password: None,
                },
                CodedText::new(
                    "elastic_scrub",
                    &[],
                    "checks the full checkpoint and records the scrub's metadata",
                ),
            ),
            ElasticSnapraidKind::Fix { disk } => (
                tentanas::approvals::OP_ELASTIC_FIX,
                P::ElasticArrayFixRequest {
                    name: name.into(),
                    disk: disk.clone(),
                    confirm_disk: disk.clone(),
                    sudo_password: None,
                },
                // The approval row has to name the disk: what it authorises is
                // overwriting THAT disk with what parity says it should hold,
                // and an approval reading only "repair" would be an approval
                // for whichever disk the author chose after it was granted.
                // What it authorises is `-e fix` on THAT disk: the blocks the
                // last Scrub marked bad, in files unchanged since the last
                // Sync. Saying "rebuilds the disk, overwriting its content"
                // described an unfiltered `fix` this product never runs, and an
                // approver who believed it would refuse a safe operation — or
                // approve it expecting deleted files back.
                //
                // The request keys the member by its SLOT ('d1'), which is not
                // a name the approver has ever seen: the code names the disk by
                // its kernel name now, and by its number when the node cannot
                // see it (`number`, 1-based among the data disks).
                //
                // The ENGLISH too (wave-6 critic MAJOR 1): it is the approvals
                // tooltip and the parked alert's text, and a slot there is as
                // unknown to the approver as on the line itself.
                {
                    let branch = array.data().enumerate().find(|(_, b)| b.name == *disk);
                    let shown = branch
                        .and_then(|(_, b)| tentanas::disks::disk_name(&b.disk_id))
                        .unwrap_or_default();
                    let number = branch.map(|(i, _)| (i + 1).to_string()).unwrap_or_default();
                    CodedText::new(
                        "elastic_fix",
                        &[("disk", shown.clone()), ("number", number.clone())],
                        fix_detail_text(&shown, &number),
                    )
                },
            ),
        };
        return park(ctx, &g, operation, name, description, &request);
    }
    let job = tentanas::elastic::spawn_snapraid(
        &g.db,
        &array,
        &g.user_id,
        secret.map(token),
        kind,
        acknowledge_parity_fault,
    )
    .map_err(|e| internal("elastic snapraid", e))?;
    Ok(job_response(ctx, job))
}

/// §5.3's headline: one more data disk on an array that keeps serving.
///
/// The disk is ERASED — it is formatted with the array's filesystem before it
/// joins — so this carries the create's retype and the create's gate. Freedom
/// is judged from the node's disk inventory (`disks_by_id` + the Elastic
/// claims), exactly as the wizard judges it, and NEVER from a mount table: an
/// array's branches are mounted inside the mergerfs process's own namespace, so
/// `/proc/mounts` on the host shows none of them and every member disk would
/// look free.
async fn elastic_add_disk(
    ctx: &HandlerContext,
    name: &str,
    disk_id: &str,
    confirm_name: &str,
    secret: Option<&SudoSecret>,
    origin: Origin,
) -> Result<MessageBody, ProtocolError> {
    let g = gate_destructive(ctx)?;
    require_confirm(name, confirm_name)?;
    tentanas_helper::elastic::validate_array_name(name)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    if disk_id.is_empty() || disk_id.len() > 128 {
        return Err(ProtocolError::bad_request("Nieprawidłowy identyfikator dysku"));
    }
    let owner = elastic_owner(&g);
    let array = store::elastic_array(&g.db, &owner, name)
        .map_err(|e| internal("elastic array", e))?
        .ok_or_else(|| ProtocolError::not_found("Macierz nie istnieje w tej instancji"))?;
    let persisted = array
        .persisted_spec()
        .map_err(|e| internal("elastic spec", e))?
        .clone();
    // A REPEAT of an add that stopped part-way, recognised before anything else
    // is judged. It has to reuse the SLOT'S RECORDED IDENTITY — above all the
    // filesystem UUID the previous attempt's mkfs stamped on the disk — and it
    // has to be admitted on an array that needs attention, because the
    // operation needing attention IS this add: the helper recorded the slot in
    // its journal before it formatted anything, so repeating the command is
    // what finishes the work. Minting a new identity, or refusing the repeat,
    // would leave a disk half-joined with no way through the product to
    // either finish or undo it.
    let pinned = store::unfinished_elastic_add_disk(&g.db, &owner, &persisted.array_id)
        .map_err(|e| internal("elastic add disk", e))?;
    // An add of ANOTHER disk is unfinished: only its resume or its undo may
    // start, and the helper would refuse this one for exactly that.
    if pinned.as_ref().is_some_and(|disk| disk.disk_id != disk_id) {
        return Err(ProtocolError::new(ProtocolErrorCode::NotAvailable, "refusal:elastic_attention_add_disk"));
    }
    let pending = pinned;
    if pending.is_none() {
        if array.state != "active" || !array.enabled {
            return Err(ProtocolError::new(
                ProtocolErrorCode::NotAvailable,
                "Dodanie dysku wymaga zakończonej aktywnej macierzy",
            ));
        }
        if array.unresolved_operation {
            return Err(ProtocolError::new(
                ProtocolErrorCode::NotAvailable,
                "Macierz ma niepotwierdzoną operację; rozwiąż ją przed dodaniem dysku",
            ));
        }
    } else if !array.enabled || !matches!(array.state.as_str(), "active" | "needs_attention") {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "Powtórzenie dodania dysku wymaga włączonej macierzy",
        ));
    }
    // A disk this array (or any array of this instance) already holds is not a
    // "not free" disk to the admin reading the error — it is THEIR disk in
    // THEIR array, and saying so is the difference between a sentence they can
    // act on and one they cannot.
    if let Some(member) = array
        .branches
        .iter()
        .find(|branch| branch.disk_id == disk_id)
        .map(|branch| branch.name.clone())
        .or_else(|| {
            array
                .parity
                .iter()
                .find(|parity| parity.disk_id == disk_id)
                .map(|parity| parity.name.clone())
        })
    {
        return Err(ProtocolError::bad_request(format!(
            "Dysk jest już w macierzy '{name}' jako '{member}'"
        )));
    }
    if origin == Origin::Direct && tentanas::approvals::required(&actor(ctx, &g)?) {
        return park(
            ctx,
            &g,
            tentanas::approvals::OP_ELASTIC_ADD_DISK,
            name,
            CodedText::new(
                "elastic_add_disk",
                &[("disk", tentanas::disks::disk_name(disk_id).unwrap_or_default())],
                "formats the picked disk and adds it to the running Elastic Array",
            ),
            &P::ElasticArrayAddDiskRequest {
                name: name.to_string(),
                disk_id: disk_id.to_string(),
                confirm_name: confirm_name.to_string(),
                sudo_password: None,
            },
        );
    }
    let explicit = secret.map(token);
    let disk = match pending {
        Some(recorded) => recorded,
        None => {
            tentanas::disks::refresh_inventory(&g.db)
                .await
                .map_err(|e| internal("inventory", e))?;
            let picked = disks_by_id(std::slice::from_ref(&disk_id.to_string()), true)?
                .into_iter()
                .next()
                .ok_or_else(|| ProtocolError::bad_request("Nie wybrano dysku"))?;
            let global = root_claims(&g, "elastic root claims", None, explicit.as_deref()).await?;
            let mut claims =
                store::elastic_claims(&g.db).map_err(|e| internal("elastic claims", e))?;
            claims.extend(global.disks);
            if !tentanas::elastic::claimed_disk_ids(std::slice::from_ref(&picked), &claims)
                .is_empty()
            {
                return Err(ProtocolError::bad_request(
                    "Dysk jest już zarezerwowany przez macierz Elastic na tym nodzie",
                ));
            }
            if let Some(conflict) = tentanas::elastic::conflicting_owner(&picked) {
                return Err(ProtocolError::bad_request(conflict));
            }
            ElasticDiskSpec {
                disk_id: picked.disk_id.clone(),
                wwn: picked.wwn.clone().filter(|w| !w.is_empty()),
                serial: (!picked.serial.is_empty()).then(|| picked.serial.clone()),
                bytes: picked.size_bytes,
                expected_uuid: uuid::Uuid::new_v4().to_string(),
            }
        }
    };
    // The array the add would produce has to be one this node would accept: the
    // 32-device ceiling, the identity uniqueness, and — when the array carries
    // parity — the "parity ≥ the largest data disk" rule, which is the ONLY
    // warning an admin gets in time, because snapraid validates it lazily and
    // refuses only once the data has outgrown the parity (MEASURED 2026-09-06,
    // snapraid 14.7).
    let after = tentanas::elastic::spec_with_added_disk(&persisted, &disk)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    if let Some(parity) = after.parity.iter().map(|p| p.bytes).min() {
        if disk.bytes > parity {
            return Err(ProtocolError::bad_request(
                "Dysk danych jest większy niż parity macierzy; parity nie pokryłaby go w całości",
            ));
        }
    }
    let job = tentanas::elastic::spawn_add_disk(&g.db, &array, &g.user_id, explicit, disk)
        .map_err(|e| internal("elastic add disk", e))?;
    Ok(job_response(ctx, job))
}

/// Undoes the unfinished add of `disk_id` (D3 = b): the array loses the slot
/// again, and the disk loses only the filesystem this add gave it.
///
/// Admitted only while the disk PROVABLY never joined the share — which only
/// the helper's journal proves (`joined == false`, written durably before
/// the branch is ever appended to the union); a live read of the union can
/// only add a refusal. The observed array carries the helper's verdict as
/// `pending_add_disk.undo_possible`. Once it may have joined, a branch taken
/// out of a live union can hide users' files, and the add can only go
/// forward. The helper holds the same rule (`add_undo_admission`), so this is
/// the sentence, and the helper the gate.
async fn elastic_add_disk_abort(
    ctx: &HandlerContext,
    name: &str,
    disk_id: &str,
    confirm_name: &str,
    secret: Option<&SudoSecret>,
    origin: Origin,
) -> Result<MessageBody, ProtocolError> {
    let g = gate_destructive(ctx)?;
    require_confirm(name, confirm_name)?;
    tentanas_helper::elastic::validate_array_name(name)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    if disk_id.is_empty() || disk_id.len() > 128 {
        return Err(ProtocolError::bad_request("Nieprawidłowy identyfikator dysku"));
    }
    let owner = elastic_owner(&g);
    let array = store::elastic_array(&g.db, &owner, name)
        .map_err(|e| internal("elastic array", e))?
        .ok_or_else(|| ProtocolError::not_found("Macierz nie istnieje w tej instancji"))?;
    let refuse = |code: &str| ProtocolError::new(ProtocolErrorCode::NotAvailable, format!("refusal:elastic_{code}"));
    if !array.enabled || !matches!(array.state.as_str(), "active" | "needs_attention") {
        return Err(refuse("array_not_ready"));
    }
    // The observation first: it settles an undo the core lost track of
    // (`elastic::reconcile_undone_add`), so a repeated undo reads "nothing to
    // undo" rather than "can only be finished".
    let observed = tentanas::elastic::get(&g.db, &owner, name)
        .await
        .map_err(|e| internal("elastic get", e))?
        .ok_or_else(|| ProtocolError::not_found("Macierz nie istnieje w tej instancji"))?;
    let disk = store::unfinished_elastic_add_disk(&g.db, &owner, &array.persisted_spec().map_err(|e| internal("elastic spec", e))?.array_id)
        .map_err(|e| internal("elastic add disk", e))?
        .filter(|disk| disk.disk_id == disk_id)
        .ok_or_else(|| refuse("nothing_to_undo"))?;
    // Nothing read from the helper: nothing says the disk never joined.
    if observed.state == "unknown" {
        return Err(refuse("state_unknown"));
    }
    if !observed.pending_add_disk.as_ref().is_some_and(|pending| pending.disk_id == disk_id && pending.undo_possible) {
        return Err(refuse("add_joined"));
    }
    if origin == Origin::Direct && tentanas::approvals::required(&actor(ctx, &g)?) {
        return park(
            ctx,
            &g,
            tentanas::approvals::OP_ELASTIC_ADD_DISK_ABORT,
            name,
            CodedText::new(
                "elastic_add_disk_abort",
                &[],
                "undoes an unfinished disk add before the disk joined the share; clears only the \
                 filesystem that add made",
            ),
            &P::ElasticArrayAddDiskAbortRequest {
                name: name.to_string(),
                disk_id: disk_id.to_string(),
                confirm_name: confirm_name.to_string(),
                sudo_password: None,
            },
        );
    }
    let job = tentanas::elastic::spawn_add_disk_abort(&g.db, &array, &g.user_id, secret.map(token), disk)
        .map_err(|e| internal("elastic add disk abort", e))?;
    Ok(job_response(ctx, job))
}

/// DISK REPLACEMENT IS WITHDRAWN (round 4, owner's decision), and this handler
/// is the refusal.
///
/// It refuses BEFORE it looks at the array, and that placement is the point:
/// the previous version failed at the helper's first hop (the command was
/// missing from `actions::run`) and its error path wrote both the operation row
/// and the array row `needs_attention` — and only a SUCCEEDED `fix` cleared a
/// `replace_disk` row, so one click cost the array every later Sync, Scrub,
/// mover and add. So nothing here reads a spec, opens a job, opens an operation
/// or asks the helper anything. Nothing can write a `replace_disk` row: the
/// store refuses that intent outright (`db::insert_job`).
///
/// The refusal says what an admin CAN do, because a disk that is gone is a real
/// situation with a real answer: the array goes on serving the disks it still
/// has, `snapraid` can still repair blocks a scrub marked on the disks that are
/// present, and the array can be dissolved and imported again once the member
/// is back. A replacement is its own task.
///
/// WHAT THE NEXT TASK HAS TO SOLVE (fourth review, so it is not rediscovered):
/// * the command must reach `actions::run` — a sixth table the five-tables test
///   did not cover (finding 1/W3b), now covered;
/// * a failed attempt must not wedge the array: write the row `failed` when
///   nothing ran, or let a later replacement supersede it (W3b/W7);
/// * `private_operation`'s fresh-boot block resolves EVERY member, so the dead
///   disk fails it before the executor runs, and `replaceable_slot` refuses on
///   `anchor.is_some()` while `private_operation` needs an anchor when the boot
///   is not fresh — two mutually exclusive guards, with the anchor guard also
///   ignoring the anchor's boot id (finding 5/W4);
/// * the journal swap is durable while the core DB is written only on full
///   success, so any failure in between desynchronises them and every later
///   Restore and Inspect of that array fails validation (finding 4/W1);
/// * a repeat hard-errors on the `Mkfs` step (`formatowanie już rozpoczęte`)
///   instead of skipping it the way `plan_add_data_disk` does, so an
///   interrupted replacement cannot be finished (finding 7/W2);
/// * `accept_stale_parity` admits a rebuild whose own verdict then rejects the
///   `error_unrecoverable > 0` result it produces (finding 6/W3);
/// * the union stays read-write across the multi-hour rebuild, and on a
///   cacheless array `mfs` sends every new client file onto the branch snapraid
///   is rebuilding (DL1);
/// * the approval surface: `OP_ELASTIC_REPLACE_DISK` is not in the UI allowlist
///   `approvals.js`, the parked record names the array but neither the slot,
///   the replacement disk nor the acknowledgement, `approvals::without_secret`
///   has no arm for the variant, and `jobs.kind_elastic_replace_disk` is
///   missing from all five locales.
#[allow(clippy::too_many_arguments)]
async fn elastic_replace_disk(
    ctx: &HandlerContext,
    name: &str,
    branch: &str,
    confirm_disk: &str,
    replacement_disk_id: &str,
    accept_stale_parity: bool,
    secret: Option<&SudoSecret>,
    origin: Origin,
) -> Result<MessageBody, ProtocolError> {
    // The permission gate first, so a reader learns nothing about the node's
    // arrays from a withdrawn feature either.
    let _g = gate_destructive(ctx)?;
    let _ = (branch, confirm_disk, replacement_disk_id, accept_stale_parity, secret, origin);
    Err(ProtocolError::new(
        ProtocolErrorCode::NotAvailable,
        format!(
            "Wymiana dysku macierzy '{name}' nie jest udostępniona w tej wersji. Macierz serwuje \
             dalej z dysków, które ma; naprawa z parity działa dla dysków obecnych na nodzie, a \
             macierz z brakującym dyskiem można rozwiązać i przejąć ponownie po jego podłączeniu."
        ),
    ))
}

/// The danger zone of the array detail screen: stop serving this array and
/// forget it here.
///
/// It formats NOTHING, and that is the design rather than a gap: there is no
/// wipe primitive in this product yet, so the member disks keep their
/// filesystems and their files, the parity file and the snapraid config stay,
/// and the node keeps the array's journal — which is what lets
/// `ElasticArrayImportScanRequest` find the array again and adopt it back
/// whole. This is therefore the one destructive array operation that is
/// REVERSIBLE, and the dialog says so.
///
/// A share pointed at the union REFUSES it rather than being deleted along the
/// way: deleting a share is itself a red path with its own approval, and an
/// array operation must not be a side door around it.
async fn elastic_destroy(
    ctx: &HandlerContext,
    name: &str,
    confirm_name: &str,
    secret: Option<&SudoSecret>,
    origin: Origin,
) -> Result<MessageBody, ProtocolError> {
    let g = gate_destructive(ctx)?;
    require_confirm(name, confirm_name)?;
    tentanas_helper::elastic::validate_array_name(name)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let owner = elastic_owner(&g);
    let array = store::elastic_array(&g.db, &owner, name)
        .map_err(|e| internal("elastic array", e))?
        .ok_or_else(|| ProtocolError::not_found("Macierz nie istnieje w tej instancji"))?;
    let union = array.union_path();
    let shares: Vec<String> = store::list_shares(&g.db)
        .map_err(|e| internal("shares", e))?
        .into_iter()
        .filter(|share| {
            share.source_path == union || share.source_path.starts_with(&format!("{union}/"))
        })
        .map(|share| share.name)
        .collect();
    if !shares.is_empty() {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            format!(
                "Macierz udostępnia share'y: {}. Usuń je przed rozwiązaniem macierzy",
                shares.join(", ")
            ),
        ));
    }
    if origin == Origin::Direct && tentanas::approvals::required(&actor(ctx, &g)?) {
        return park(
            ctx,
            &g,
            tentanas::approvals::OP_ELASTIC_DESTROY,
            name,
            CodedText::new(
                "elastic_destroy",
                &[],
                "stops serving the Elastic Array and removes its supervision; the disks keep \
                 their filesystems and data",
            ),
            &P::ElasticArrayDestroyRequest {
                name: name.to_string(),
                confirm_name: confirm_name.to_string(),
                sudo_password: None,
            },
        );
    }
    let job = tentanas::elastic::spawn_dissolve(&g.db, &array, &g.user_id, secret.map(token))
        .map_err(|e| internal("elastic destroy", e))?;
    Ok(job_response(ctx, job))
}

/// "Uruchom mover teraz" (n11): one mover run, started by hand.
async fn elastic_mover(
    ctx: &HandlerContext,
    name: &str,
    secret: Option<&SudoSecret>,
    origin: Origin,
) -> Result<MessageBody, ProtocolError> {
    let g = gate_destructive(ctx)?;
    super::app_gate::require_app_permission(ctx, tentanas::PACKAGE_ID, PERM_READ)?;
    tentanas_helper::elastic::validate_array_name(name)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let array = store::elastic_array(&g.db, &elastic_owner(&g), name)
        .map_err(|e| internal("elastic array", e))?
        .ok_or_else(|| ProtocolError::not_found("Macierz nie istnieje w tej instancji"))?;
    // An array whose only unresolved operations are mover runs takes the
    // next run: that run is what settles them.
    if array.state != "active" && !array.mover_settles_unresolved {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "Mover wymaga zakończonej aktywnej macierzy",
        ));
    }
    // Nothing to move. Without a cache branch every write already lands on the
    // data disks, so a run would walk an empty source and report having moved
    // nothing — a job whose success says nothing about anything.
    if array.cache().next().is_none() {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "Macierz bez dysku cache nie ma czego przenosić",
        ));
    }
    // An unresolved operation blocks the mover in the store. Without this the
    // refusal would surface as a generic internal error from `insert_job`, and
    // the one sentence that explains it would never reach the admin. It
    // OUTLIVES the array returning to 'active': a Restore after a failed sync
    // does exactly that, which is how an enabled button used to meet an opaque
    // error. Clearing such an operation is E2-13, not this path.
    if array.unresolved_operation && !array.mover_settles_unresolved {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "Macierz ma niepotwierdzoną operację; rozwiąż ją przed uruchomieniem movera",
        ));
    }
    if origin == Origin::Direct && tentanas::approvals::required(&actor(ctx, &g)?) {
        // What is recorded in the approval must be what will actually run: with
        // the coupling off the moved files stay outside parity, and promising a
        // sync that will not happen would be a lie preserved in the audit row.
        let description = CodedText::new(
            "elastic_mover",
            &[("coupled_sync", array.mover.coupled_sync.to_string())],
            if array.mover.coupled_sync {
                "moves files from the cache to the data disks and runs the coupled parity sync"
            } else {
                "moves files from the cache to the data disks WITHOUT the coupled parity sync"
            },
        );
        return park(
            ctx,
            &g,
            tentanas::approvals::OP_ELASTIC_MOVER,
            name,
            description,
            &P::ElasticArrayMoverRequest {
                name: name.into(),
                sudo_password: None,
            },
        );
    }
    let job = tentanas::elastic::spawn_mover(&g.db, &array, &g.user_id, secret.map(token))
        .map_err(|e| internal("elastic mover", e))?;
    Ok(job_response(ctx, job))
}

/// The cadence in words, for the approval row an admin has to read and agree
/// to. Unknown cadences are quoted rather than described: the handler refuses
/// them anyway, and inventing a phrase for one would be the approval saying
/// something the scheduler cannot do.
/// The English of a parked repair, naming the disk the way its code does:
/// by kernel name, else by its number among the data disks, else as "a data
/// disk of the array" — never by the slot the request keys it by.
fn fix_detail_text(disk: &str, number: &str) -> String {
    let target = if !disk.is_empty() {
        format!("disk {disk}")
    } else if !number.is_empty() {
        format!("data disk no. {number}")
    } else {
        "a data disk of the array".to_string()
    };
    format!(
        "writes back from parity the blocks the last scrub marked bad, in files unchanged since \
         the last Sync, on {target}"
    )
}

fn cadence_text(s: &NasSchedule) -> String {
    match s.every.as_str() {
        // The OFFSET inside the period is what `scheduler::period_and_offset`
        // fires on, so it is carried here too — "co 15 min" alone would imply
        // the top of the period and quietly drop the half of the cadence the
        // scheduler actually reads.
        "15m" => format!("every 15 min, at minute :{:02}", s.minute % 15),
        "30m" => format!("every 30 min, at minute :{:02}", s.minute % 30),
        "1h" => format!("hourly, at minute :{:02}", s.minute),
        "6h" => format!("every 6 hours, at {:02}:{:02} of the cycle", s.hour % 6, s.minute),
        "daily" => format!("daily at {:02}:{:02}", s.hour, s.minute),
        "weekly" => format!(
            "weekly, day {}, at {:02}:{:02}",
            s.weekday, s.hour, s.minute
        ),
        "monthly" => format!("monthly, day {} at {:02}:{:02}", s.day, s.hour, s.minute),
        other => format!("cadence '{other}'"),
    }
}

/// The longest age rule the node will store. The dialog offers at most a day;
/// this only has to be a bound that exists, so a value nothing could have
/// chosen is refused rather than persisted and rendered.
const MAX_MOVER_MIN_AGE_SECS: u64 = 365 * 24 * 60 * 60;

/// The three recurring Elastic tasks of §5.3 (E2-10). The mover's dialog is one
/// form — a cadence and the three rules chosen beside it — so `mover_rules`
/// carries them in the same request; sync and scrub pass `None` and touch no
/// settings row.
///
/// `gate_destructive` rather than the `gate(PERM_POOLS)` the pool schedules
/// use: saving a cadence here ARMS unattended privileged runs on this array —
/// the mover moves files, sync and scrub run snapraid — and every one of those
/// is admin-only when an admin starts it by hand. Scheduling what only an admin
/// may run is the same decision taken in advance, so it meets the same bar.
async fn elastic_schedule_set(
    ctx: &HandlerContext,
    task: store::ElasticTask,
    name: &str,
    enabled: bool,
    schedule: &NasSchedule,
    mover_rules: (Option<u64>, Option<u8>, Option<bool>),
    origin: Origin,
) -> Result<MessageBody, ProtocolError> {
    // Authorisation first. Validating the shape ahead of the gate answered an
    // unauthorised caller with `BadRequest` instead of `PolicyDenied` — nothing
    // leaked, but who may ask is settled before what they asked.
    let g = gate_destructive(ctx)?;
    // All three rules or none. A half-filled trio could only be completed with
    // defaults, which would write one admin's age beside a free-space rule
    // nobody chose and then report the whole thing as `configured`.
    let mover_rules = match mover_rules {
        (None, None, None) => None,
        (Some(age), Some(pct), Some(coupled)) => Some((age, pct, coupled)),
        _ => {
            return Err(ProtocolError::bad_request(
                "Niepełne reguły movera: podaj wszystkie trzy albo żadnej",
            ))
        }
    };
    // EVERY range check belongs above the park below. A value the replay would
    // refuse must never be parked: `claim` closes the row before the handler
    // runs, so the approval would be burned on a request that then fails, and
    // the approver would have been shown "próg wolnego cache 200%".
    if let Some((min_age_secs, cache_min_free_pct, _)) = mover_rules {
        if cache_min_free_pct > 100 {
            return Err(ProtocolError::bad_request(
                "Minimum wolnego miejsca na cache to 0–100%",
            ));
        }
        if min_age_secs > MAX_MOVER_MIN_AGE_SECS {
            return Err(ProtocolError::bad_request(
                "Wiek plików do przeniesienia nie może przekraczać 365 dni",
            ));
        }
    }
    tentanas_helper::elastic::validate_array_name(name)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let array_id = store::elastic_array(&g.db, &elastic_owner(&g), name)
        .map_err(|e| internal("elastic array", e))?
        .ok_or_else(|| ProtocolError::not_found("Macierz nie istnieje w tej instancji"))?
        .array_id()
        .map(str::to_string)
        .ok_or_else(|| internal("elastic schedule", "macierz bez utrwalonej intencji"))?;
    // An unknown cadence never fires — `next_run_after` refuses to guess one —
    // so an enabled schedule this node could never run is refused here instead
    // of being stored as a promise nothing keeps.
    let next = enabled
        .then(|| tentanas::scheduler::next_run_utc(schedule, chrono::Local::now()))
        .flatten();
    if enabled && next.is_none() {
        return Err(ProtocolError::bad_request(format!(
            "unknown schedule cadence '{}'",
            schedule.every
        )));
    }
    // §5.10 applies to any request that CHANGES what will run — not merely to
    // the one that flips the switch on.
    //
    // EXACTLY ONE thing escapes four eyes: standing an existing cadence down
    // and changing nothing else. Requiring a second admin to stop a runaway
    // cadence would make an incident worse, and a pure switch-off arms nothing.
    // For the mover a switch-off lifts a restriction: moving returns to the
    // automatic default every array with a cache has anyway.
    //
    // Everything else parks, `enabled: false` included, because gating on the
    // switch alone was a hole with a three-click exploit: n15's dialog sends
    // the three rules whatever the toggle says, so one admin could save
    // "age 0, coupled sync off, every 15m" with the toggle OFF — unparked and
    // unaudited — and then flip the row toggle, whose request deliberately
    // carries no rules and therefore shows the approver a bare cadence. The
    // second admin would be agreeing to a 15-minute mover that empties the
    // cache with parity sync decoupled, having been shown none of it.
    //
    // Parked AFTER validation, so an approver is never shown a request that
    // would be refused the moment they agreed to it.
    let stored = store::elastic_schedule(&g.db, &array_id, task)
        .map_err(|e| internal("elastic schedule", e))?;
    let pure_switch_off = !enabled
        && mover_rules.is_none()
        && stored.as_ref().is_some_and(|row| row.schedule == *schedule);
    if origin == Origin::Direct
        && !pure_switch_off
        && tentanas::approvals::required(&actor(ctx, &g)?)
    {
        let (verb, request) = match task {
            store::ElasticTask::Mover => (
                "mover",
                P::ElasticMoverScheduleSetRequest {
                    name: name.into(),
                    enabled,
                    schedule: schedule.clone(),
                    min_age_secs: mover_rules.map(|r| r.0),
                    cache_min_free_pct: mover_rules.map(|r| r.1),
                    coupled_sync: mover_rules.map(|r| r.2),
                },
            ),
            store::ElasticTask::Sync => (
                "sync parity",
                P::ElasticSyncScheduleSetRequest {
                    name: name.into(),
                    enabled,
                    schedule: schedule.clone(),
                },
            ),
            store::ElasticTask::Scrub => (
                "scrub parity",
                P::ElasticScrubScheduleSetRequest {
                    name: name.into(),
                    enabled,
                    schedule: schedule.clone(),
                },
            ),
        };
        // What will actually happen, in words. The approver has to see the
        // operation, the array, the cadence and the rules — and a request that
        // leaves the schedule switched off must say so rather than claim to
        // arm something.
        //
        // The codes carry the schedule itself ('every', 'hour', 'minute',
        // 'weekday', 'day'): the approver's screen words the cadence with the
        // same formatter the schedule editor uses.
        let mut text = format!(
            "{}: {verb} of array {name}, {}",
            if enabled {
                "arms the schedule"
            } else {
                "changes the schedule (it stays off)"
            },
            cadence_text(schedule)
        );
        let mut params = vec![
            ("task", task.kind().to_string()),
            ("enabled", enabled.to_string()),
            ("every", schedule.every.clone()),
            ("hour", schedule.hour.to_string()),
            ("minute", schedule.minute.to_string()),
            ("weekday", schedule.weekday.to_string()),
            ("day", schedule.day.to_string()),
        ];
        if let Some((age, pct, coupled)) = mover_rules {
            text.push_str(&format!(
                "; files older than {age} s, cache free-space threshold {pct}%, coupled sync: {}",
                if coupled { "yes" } else { "no" }
            ));
            params.push(("min_age_secs", age.to_string()));
            params.push(("cache_min_free_pct", pct.to_string()));
            params.push(("coupled_sync", coupled.to_string()));
        }
        return park(
            ctx,
            &g,
            tentanas::approvals::OP_ELASTIC_SCHEDULE,
            name,
            CodedText::new("elastic_schedule", &params, text),
            &request,
        );
    }
    if let Some((min_age_secs, cache_min_free_pct, coupled_sync)) = mover_rules {
        store::set_mover_settings(
            &g.db,
            &array_id,
            min_age_secs,
            cache_min_free_pct,
            coupled_sync,
        )
        .map_err(|e| internal("mover settings", e))?;
    }
    store::set_elastic_schedule(&g.db, &array_id, task, enabled, schedule, next.as_deref())
        .map_err(|e| internal("elastic schedule", e))?;
    let array = tentanas::elastic::get(&g.db, &elastic_owner(&g), name)
        .await
        .map_err(|e| internal("elastic get", e))?
        .ok_or_else(|| ProtocolError::not_found("Macierz nie istnieje w tej instancji"))?;
    Ok(tn(P::ElasticArrayGetResponse { array }))
}

/// One folder's cache policy — §5.3's per-folder switch, n11's "Cache" column.
///
/// PERMISSIONED AS A CONFIGURATION CHANGE, not as a red path, and the
/// difference from `elastic_schedule_set` next door is deliberate: that one
/// ARMS an unattended privileged job, which is what §5.10 gates and what four
/// eyes are for. This writes one row of this node's database, starts nothing,
/// touches no disk and needs no sudo — the pool permission is the whole gate,
/// exactly as it is for every other stored setting of a pool.
///
/// What it DOES change is the rules the next mover run is carried out under,
/// and one of the three is load-bearing: a folder pinned 'only' keeps its
/// bytes on the cache, where SnapRAID never reaches, for as long as the pin
/// lasts. That consequence belongs where the admin chooses it — the dialog in
/// `elastic-detail.js` says it in one sentence — and not only here.
async fn elastic_folder_cache_set(
    ctx: &HandlerContext,
    name: &str,
    folder: &str,
    cache_policy: &str,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_POOLS)?;
    // Both shapes before either lookup: a caller who spelled the policy wrong
    // gets told which values exist, rather than a refusal about a folder.
    let policy = tentanas::elastic::CachePolicy::parse(cache_policy).ok_or_else(|| {
        ProtocolError::bad_request("Nieznana polityka cache: dozwolone „yes”, „no” i „only”")
    })?;
    tentanas_helper::elastic::validate_array_name(name)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    if !tentanas::elastic::folder_name_valid(folder) {
        return Err(ProtocolError::bad_request(
            "Folder musi być jedną nazwą bezpośrednio pod unią macierzy",
        ));
    }
    let array = store::elastic_array(&g.db, &elastic_owner(&g), name)
        .map_err(|e| internal("elastic array", e))?
        .ok_or_else(|| ProtocolError::not_found("Macierz nie istnieje w tej instancji"))?;
    let array_id = array
        .array_id()
        .map(str::to_string)
        .ok_or_else(|| internal("elastic folder cache", "macierz bez utrwalonej intencji"))?;
    // The two refusals are NOT interchangeable. `NoSuchFolder` is a folder the
    // node looked for in a list it could read; `FoldersUnknown` is a list it
    // could not read at all, which happens on every array whose branches are
    // not mounted — and answering that as "no such folder" would tell an admin
    // their folder is gone.
    if let Some(refusal) = tentanas::elastic::folder_policy_refusal(&array, folder) {
        return Err(match refusal {
            tentanas::elastic::FolderPolicyRefusal::NoSuchFolder => {
                ProtocolError::not_found("Ten folder nie istnieje w tej macierzy")
            }
            tentanas::elastic::FolderPolicyRefusal::FoldersUnknown => ProtocolError::new(
                ProtocolErrorCode::NotAvailable,
                "Nie można odczytać listy folderów macierzy — polityka cache nie została zapisana",
            ),
        });
    }
    if !store::set_elastic_folder_policy(&g.db, &array_id, folder, policy)
        .map_err(|e| internal("elastic folder cache", e))?
    {
        return Err(ProtocolError::bad_request(format!(
            "Najwyżej {} folderów tej macierzy może mieć własną politykę cache",
            store::FOLDER_POLICY_LIMIT
        )));
    }
    let array = tentanas::elastic::get(&g.db, &elastic_owner(&g), name)
        .await
        .map_err(|e| internal("elastic get", e))?
        .ok_or_else(|| ProtocolError::not_found("Macierz nie istnieje w tej instancji"))?;
    Ok(tn(P::ElasticArrayGetResponse { array }))
}

/// Whether this node has the mkfs for one of the array filesystems. Read from
/// the same tool directories the feature probe uses, so the wizard's offer and
/// the plan's `ToolMissing` cannot disagree.
fn has_mkfs(filesystem: &str) -> bool {
    tentanas::environment::find_binary(&format!("mkfs.{filesystem}")).is_some()
}

async fn elastic_capabilities(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    let features = tentanas::environment::cached_or_probe(&g.db)
        .await
        .map(|e| e.features)
        .unwrap_or_default();
    let capabilities = tentanas::elastic::capabilities(&features, &has_mkfs);
    let free_disks =
        tentanas::elastic::free_disks(&tentanas::disks::snapshot().0, &arrays_claiming_disks(&g).await?);
    Ok(tn(P::ElasticCapabilitiesResponse {
        capabilities,
        free_disks,
    }))
}

#[allow(clippy::too_many_arguments)]
async fn elastic_array_plan(
    ctx: &HandlerContext,
    name: &str,
    data_disk_ids: &[String],
    parity_disk_ids: &[String],
    cache_disk_ids: &[String],
    filesystem: &str,
    read_disks: impl Fn(&[String]) -> Result<Vec<NasDisk>, ProtocolError>,
    namespace: impl std::future::Future<Output = Result<tentanas_helper::elastic::ElasticClaimsResult, ProtocolError>>,
) -> Result<MessageBody, ProtocolError> {
    let g = gate(ctx, PERM_READ)?;
    // `require_free` is false on every one of the three: a disk that is NOT
    // free has to come back as a named refusal ("sdb is a member of ZFS pool
    // tank"), which is the whole point of §5.3's exclusivity rule. Refusing
    // the request outright would leave the wizard with a red error box and no
    // idea which disk to unpick.
    if cache_disk_ids.len() > 1 {
        // A code the screen words in the reader's language, never a Polish
        // sentence in an English toast (critic wave 6, MINOR 5).
        return Err(ProtocolError::bad_request("refusal:elastic_one_cache_disk"));
    }
    let data = read_disks(data_disk_ids)?;
    let parity = optional_disks(parity_disk_ids, &read_disks)?;
    let cache = optional_disks(cache_disk_ids, &read_disks)?;
    // The preview's tools are the placeholder ones on purpose: an admin has to
    // be able to SEE the plan on a node where mergerfs is not installed yet,
    // because the plan is what tells them to install it. Nothing here runs.
    let global = namespace.await?;
    if !name.is_empty() && (global.name_claimed.is_none() || global.namespace_clear.is_none()) {
        return Err(ProtocolError::new(ProtocolErrorCode::NotAvailable,"Niepotwierdzona przestrzeń nazw"));
    }
    let mut reserved = std::collections::BTreeSet::new();
    if global.name_claimed == Some(true) || global.namespace_clear == Some(false) {
        reserved.insert(name.to_string());
    }
    let mut claims = store::elastic_claims(&g.db).map_err(|e| internal("elastic claims",e))?;
    claims.extend(global.disks);
    let selected: Vec<NasDisk> = data.iter().chain(&parity).chain(&cache).cloned().collect();
    let taken = tentanas::elastic::claimed_disk_ids(&selected,&claims);
    // What this node can actually format. Read from the same probe the
    // Environment tab shows, so the wizard cannot offer a filesystem the
    // create would then fail on; an empty list (nothing probed yet) skips the
    // check rather than refusing everything.
    let features = tentanas::environment::cached_or_probe(&g.db)
        .await
        .map(|e| e.features)
        .unwrap_or_default();
    let filesystems = tentanas::elastic::capabilities(&features, &has_mkfs).filesystems;
    let plan = tentanas::elastic::plan_layout(
        name,
        filesystem,
        &data,
        &parity,
        &cache,
        &taken,
        &reserved,
        &filesystems,
        &tentanas_helper::elastic::Tools::for_preview(),
    );
    Ok(tn(P::ElasticArrayPlanResponse { plan }))
}

/// `disks_by_id` for a list that may legitimately be empty — 0 parity disks
/// and no cache are both valid arrays, and its "no disks selected" refusal
/// would turn either into an error.
fn optional_disks(
    disk_ids: &[String],
    read_disks: &impl Fn(&[String]) -> Result<Vec<NasDisk>, ProtocolError>,
) -> Result<Vec<NasDisk>, ProtocolError> {
    if disk_ids.is_empty() {
        return Ok(Vec::new());
    }
    read_disks(disk_ids)
}

fn variant_of(payload: &P) -> String {
    serde_json::to_value(payload)
        .ok()
        .and_then(|v| v.as_object().and_then(|m| m.keys().next().cloned()))
        .unwrap_or_else(|| "unknown".to_string())
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
            let mut nodes = tentanas::fleet::nodes(ctx, &g.addon_id);
            // The published summary counts node-wide alerts and pools only;
            // this node's row becomes what this tenant's alert list shows,
            // plus this tenant's own Elastic Arrays.
            tentanas::fleet::scope_local_alerts(&mut nodes, &g.db, org_viewer(ctx, &g));
            // Shares are per organisation too (migration 20): the published
            // row carries none, and this node's row gets this tenant's own.
            // `per_org_counted` says whether both reads made it.
            tentanas::fleet::scope_local_org_figures(&mut nodes, &g.db, &g.org_id);
            Ok(tn(P::NodesListResponse {
                local_node_id: ctx.state.local_node_id.to_string(),
                nodes,
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
        P::DiskSmartTestBatchRequest { disk_ids, kind, sudo_password } => {
            disk_smart_test_batch(ctx, disk_ids, kind, sudo_password.as_ref()).await
        }
        P::DiskLocateRequest { disk_id, enable } => disk_locate(ctx, disk_id, *enable).await,
        P::DiskWipePlanRequest { disk_id, sudo_password } => {
            disk_wipe_plan(ctx, disk_id, sudo_password.as_ref()).await
        }
        P::DiskWipeRequest {
            disk_id,
            confirm_device,
            release_journal_array,
            sudo_password,
        } => {
            disk_wipe(
                ctx,
                disk_id,
                confirm_device,
                release_journal_array,
                sudo_password.as_ref(),
            )
            .await
        }
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
        } => pool_destroy(ctx, name, confirm_name, sudo_password.as_ref(), Origin::Direct).await,
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
                ctx,
                &g,
                "pool_export",
                name,
                HelperCommand::ZpoolExport {
                    pool: name.clone(),
                    force: *force,
                },
                sudo_password.as_ref(),
            )?;
            let _ = store::delete_pool_schedules(&g.db, name);
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
            // Titled from the dialog's own scan, never by a second one: the
            // reply must not wait on privileged work that can outlast the
            // client's timeout (`import_scan_names`).
            let subject =
                import_job_subject(&g.org_id, guid, new_name, std::time::Instant::now());
            spawn_pool_job(
                ctx,
                &g,
                "pool_import",
                &subject,
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
                ctx,
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
                ctx,
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
                .plan()
                .map_err(|e| broker_error("zpool replace", catalog_error(e)))?;
            let explicit = sudo_password.as_ref().map(token);
            let pool = name.clone();
            let job = tentanas::jobs::spawn(&g.db, "pool_replace", name, &g.user_id, None, None, move |h| {
                tentanas::pools::replace_job(h, pool, command, explicit)
            })
            .map_err(|e| internal("job", e))?;
            Ok(job_response(ctx, job))
        }
        P::PoolDeviceStateRequest {
            name,
            device,
            action,
            sudo_password,
        } => pool_device_state(ctx, name, device, action, sudo_password.as_ref()).await,
        P::PoolDetachRequest {
            name,
            device,
            sudo_password,
        } => pool_detach(ctx, name, device, sudo_password.as_ref()).await,
        P::PoolSetPropertiesRequest {
            name,
            changes,
            sudo_password,
        } => pool_set_properties(ctx, name, changes, sudo_password.as_ref()).await,
        P::ScrubScheduleSetRequest {
            name,
            enabled,
            schedule,
        } => pool_schedule_set(ctx, store::PoolTask::Scrub, name, *enabled, schedule).await,

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
            protect_days,
            sudo_password,
        } => {
            snapshot_create(
                ctx,
                dataset,
                short_name,
                *recursive,
                *protect_days,
                sudo_password.as_ref(),
            )
            .await
        }
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

        // ----- shares -----
        P::SharesListRequest {} => shares_list(ctx).await,
        P::ShareGetRequest { share_id } => share_get(ctx, share_id).await,
        P::ShareCreateRequest { .. } => share_create(ctx, payload).await,
        P::ShareUpdateRequest { .. } => share_update(ctx, payload).await,
        P::ShareDeleteRequest {
            share_id,
            confirm_name,
            sudo_password,
        } => {
            share_delete(ctx, share_id, confirm_name, sudo_password.as_ref(), Origin::Direct).await
        }
        P::ShareBrowseRequest { path } => share_browse(ctx, path).await,

        // ----- block targets -----
        P::TargetsListRequest { summary } => targets_list(ctx, *summary).await,
        P::TargetGetRequest { target_id } => target_get(ctx, target_id).await,
        P::TargetCreateRequest { .. } => target_create(ctx, payload).await,
        P::TargetUpdateRequest { .. } => target_update(ctx, payload).await,
        P::TargetDeleteRequest {
            target_id,
            confirm_name,
            sudo_password,
        } => {
            target_delete(
                ctx,
                target_id,
                confirm_name,
                sudo_password.as_ref(),
                Origin::Direct,
            )
            .await
        }
        P::ShareMountsRefreshRequest { share_id } => share_mounts_refresh(ctx, share_id).await,
        P::ShareUsersListRequest {} => {
            let g = gate(ctx, PERM_READ)?;
            share_users_response(&g)
        }
        P::ShareUserSetRequest { .. } => share_user_set(ctx, payload).await,
        P::ShareUserDeleteRequest {
            name,
            sudo_password,
        } => share_user_delete(ctx, name, sudo_password.as_ref()).await,

        // ----- fleet mounts -----
        P::FleetMountsListRequest {} => fleet_mounts_list(ctx),
        P::FleetMountRetryRequest {
            share_id,
            sudo_password,
        } => fleet_mount_retry(ctx, share_id, sudo_password.as_ref()).await,

        // ----- configuration export / import -----
        P::ConfigExportRequest {} => config_export(ctx).await,
        P::ConfigImportPlanRequest { json } => config_import_plan(ctx, json).await,
        P::ConfigImportApplyRequest { json, sudo_password } => {
            config_import_apply(ctx, json, sudo_password.as_ref(), Origin::Direct).await
        }

        // ----- ARC, the helper catalog and the snapshot browser -----
        P::ArcStatsRequest {} => arc_stats(ctx).await,
        P::ArcLimitSetRequest {
            max_bytes,
            sudo_password,
        } => arc_limit_set(ctx, *max_bytes, sudo_password.as_ref()).await,
        P::ElevationCatalogRequest {} => elevation_catalog(ctx),
        P::SnapshotBrowseRequest { snapshot, path } => snapshot_browse(ctx, snapshot, path).await,

        // ----- four eyes -----
        P::ApprovalsListRequest { include_closed } => approvals_list(ctx, *include_closed).await,
        P::ApprovalDecideRequest {
            request_id,
            approve,
            note,
            sudo_password,
        } => approval_decide(ctx, request_id, *approve, note, sudo_password.as_ref()).await,
        P::ApprovalSettingsSetRequest { enabled, ttl_hours } => {
            approval_settings_set(ctx, *enabled, *ttl_hours)
        }
        P::SnapshotProtectionReleaseRequest {
            snapshot,
            reason,
            confirm_snapshot,
            sudo_password,
        } => {
            snapshot_protection_release(
                ctx,
                snapshot,
                reason,
                confirm_snapshot,
                sudo_password.as_ref(),
            )
            .await
        }
        P::AccessLogRequest {
            share,
            user,
            operation,
            result,
            since,
            limit,
        } => {
            access_log(
                ctx,
                &store::AccessFilter {
                    share,
                    user,
                    operation,
                    result,
                    since,
                    limit: if *limit == 0 { ACCESS_LOG_PAGE } else { *limit },
                },
            )
            .await
        }
        P::AlertForwardSetRequest {
            enabled,
            syslog_target,
            webhook_url,
            include_access,
            node_wide,
        } => alert_forward_set(ctx, *enabled, syslog_target, webhook_url, *include_access, *node_wide).await,
        P::PoolTrimRequest {
            name,
            action,
            sudo_password,
        } => pool_trim(ctx, name, action, sudo_password.as_ref()).await,
        P::TrimScheduleSetRequest {
            name,
            enabled,
            schedule,
        } => pool_schedule_set(ctx, store::PoolTask::Trim, name, *enabled, schedule).await,

        // ----- Elastic Array -----
        P::ElasticArrayCreateRequest {name,filesystem,data_disk_ids,parity_disk_ids,cache_disk_ids,confirm_name,sudo_password} =>
            elastic_create(ctx,name,filesystem,data_disk_ids,parity_disk_ids,cache_disk_ids,confirm_name,sudo_password.as_ref(),Origin::Direct).await,
        P::ElasticArrayRestoreRequest {name,sudo_password} => elastic_restore(ctx,name,sudo_password.as_ref()).await,
        P::ElasticArraySyncRequest {name,acknowledge_parity_fault,sudo_password} => elastic_snapraid(ctx,name,sudo_password.as_ref(),Origin::Direct,
            tentanas_helper::elastic::ElasticSnapraidKind::Sync,acknowledge_parity_fault.clone()).await,
        P::ElasticArrayScrubRequest {name,sudo_password} => elastic_snapraid(ctx,name,sudo_password.as_ref(),Origin::Direct,
            tentanas_helper::elastic::ElasticSnapraidKind::Scrub,None).await,
        P::ElasticArrayFixRequest {name,disk,confirm_disk,sudo_password} => {
            require_confirm(disk,confirm_disk)?;
            elastic_snapraid(ctx,name,sudo_password.as_ref(),Origin::Direct,
                tentanas_helper::elastic::ElasticSnapraidKind::Fix { disk: disk.clone() },None).await
        }
        P::ElasticArrayAddDiskRequest {name,disk_id,confirm_name,sudo_password} =>
            elastic_add_disk(ctx,name,disk_id,confirm_name,sudo_password.as_ref(),Origin::Direct).await,
        P::ElasticArrayAddDiskAbortRequest {name,disk_id,confirm_name,sudo_password} =>
            elastic_add_disk_abort(ctx,name,disk_id,confirm_name,sudo_password.as_ref(),Origin::Direct).await,
        P::ElasticArrayReplaceDiskRequest {name,disk,confirm_disk,replacement_disk_id,accept_stale_parity,sudo_password} =>
            elastic_replace_disk(ctx,name,disk,confirm_disk,replacement_disk_id,*accept_stale_parity,
                sudo_password.as_ref(),Origin::Direct).await,
        P::ElasticArrayDestroyRequest {name,confirm_name,sudo_password} =>
            elastic_destroy(ctx,name,confirm_name,sudo_password.as_ref(),Origin::Direct).await,
        P::ElasticArrayMoverRequest {name,sudo_password} => elastic_mover(ctx,name,sudo_password.as_ref(),Origin::Direct).await,
        P::ElasticMoverScheduleSetRequest {name,enabled,schedule,min_age_secs,cache_min_free_pct,coupled_sync} =>
            elastic_schedule_set(ctx,store::ElasticTask::Mover,name,*enabled,schedule,
                (*min_age_secs,*cache_min_free_pct,*coupled_sync),Origin::Direct).await,
        P::ElasticSyncScheduleSetRequest {name,enabled,schedule} =>
            elastic_schedule_set(ctx,store::ElasticTask::Sync,name,*enabled,schedule,(None,None,None),Origin::Direct).await,
        P::ElasticScrubScheduleSetRequest {name,enabled,schedule} =>
            elastic_schedule_set(ctx,store::ElasticTask::Scrub,name,*enabled,schedule,(None,None,None),Origin::Direct).await,
        P::ElasticFolderCacheSetRequest {name,folder,cache_policy} =>
            elastic_folder_cache_set(ctx,name,folder,cache_policy).await,
        P::ElasticArraysListRequest {} => {
            let g = gate(ctx,PERM_READ)?;
            let arrays = tentanas::elastic::list(&g.db,&elastic_owner(&g)).await.map_err(|e| internal("elastic list",e))?;
            Ok(tn(P::ElasticArraysListResponse {arrays}))
        }
        P::ElasticArrayGetRequest {name} => {
            let g = gate(ctx,PERM_READ)?;
            let array = tentanas::elastic::get(&g.db,&elastic_owner(&g),name).await
                .map_err(|e| internal("elastic get",e))?.ok_or_else(||ProtocolError::not_found("Macierz nie istnieje w tej instancji"))?;
            Ok(tn(P::ElasticArrayGetResponse {array}))
        }
        P::ElasticArrayImportScanRequest { sudo_password } => {
            elastic_import_scan(ctx, sudo_password.as_ref()).await
        }
        P::ElasticArrayImportRequest {
            array_id,
            confirm_name,
            sudo_password,
        } => elastic_import(ctx, array_id, confirm_name, sudo_password.as_ref()).await,
        P::ElasticCapabilitiesRequest {} => elastic_capabilities(ctx).await,
        P::ElasticArrayPlanRequest {
            name,
            data_disk_ids,
            parity_disk_ids,
            cache_disk_ids,
            filesystem,
        } => {
            elastic_array_plan(
                ctx,
                name,
                data_disk_ids,
                parity_disk_ids,
                cache_disk_ids,
                filesystem,
                |ids| disks_by_id(ids, false),
                async {
                    let g = gate(ctx,PERM_READ)?;
                    root_claims(&g,"elastic namespace",(!name.is_empty()).then_some(name.as_str()),None).await
                },
            )
            .await
        }

        P::NodesListResponse { .. }
        | P::EnvironmentResponse { .. }
        | P::ElevationPlanResponse { .. }
        | P::ElevationResponse { .. }
        | P::JobsListResponse { .. }
        | P::JobResponse { .. }
        | P::DisksListResponse { .. }
        | P::DiskGetResponse { .. }
        | P::DiskLocateResponse { .. }
        | P::DiskWipePlanResponse { .. }
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
        | P::SmartScheduleResponse { .. }
        | P::SharesListResponse { .. }
        | P::ShareGetResponse { .. }
        | P::ShareBrowseResponse { .. }
        | P::ShareUsersListResponse { .. }
        | P::FleetMountsListResponse { .. }
        | P::ConfigExportResponse { .. }
        | P::ConfigImportPlanResponse { .. }
        | P::ArcStatsResponse { .. }
        | P::ElevationCatalogResponse { .. }
        | P::SnapshotBrowseResponse { .. }
        | P::ApprovalsListResponse { .. }
        | P::ApprovalPendingResponse { .. }
        | P::AccessLogResponse { .. }
        | P::TargetsListResponse { .. }
        | P::TargetGetResponse { .. }
        | P::ElasticCapabilitiesResponse { .. }
        | P::ElasticArrayPlanResponse { .. }
        | P::ElasticArraysListResponse { .. }
        | P::ElasticArrayImportScanResponse { .. }
        | P::ElasticArrayGetResponse { .. } => {
            Err(ProtocolError::bad_request("response variant sent as request"))
        }
    }
}

// =============================================================================
// Variant registration
// =============================================================================

/// `#[handler]` registers the dispatcher under the FAMILY name (`TentaNasBody`),
/// and no frame ever carries that: `variant_name_of` reports the concrete
/// variant and `dispatch::find` looks the handler up by it. Without an entry per
/// request variant this whole family answers `NotImplemented` on the wire — and
/// it did, because every test here calls the handler function directly and
/// `cargo check` cannot see the gap. `code_studio.rs` has carried the same
/// macro for exactly this reason.
macro_rules! register_tentanas_variant {
    ($variant:literal, $metric:literal) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                // The CONST `#[policy(...)]` generated on `tentanas_dispatch`,
                // never a literal. A literal here is an authorization decision
                // copied by hand: tightening the handler to `#[policy(Admin)]`
                // would compile, the suite would stay green, and all 82
                // variants would go on being admitted at `UserSession` — the
                // policy attribute would apply to nothing.
                required_auth: __tentaflow_policy_tentanas_dispatch,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_tentanas_dispatch,
            }
        }
    };
}

register_tentanas_variant!("TentaNasNodesListRequest", "tentaflow_ws_handler_nas_nodes_list");
register_tentanas_variant!("TentaNasEnvironmentRequest", "tentaflow_ws_handler_nas_environment");
register_tentanas_variant!(
    "TentaNasElevationPlanRequest",
    "tentaflow_ws_handler_nas_elevation_plan"
);
register_tentanas_variant!(
    "TentaNasElevationProvisionRequest",
    "tentaflow_ws_handler_nas_elevation_provision"
);
register_tentanas_variant!("TentaNasElevationArmRequest", "tentaflow_ws_handler_nas_elevation_arm");
register_tentanas_variant!(
    "TentaNasElevationDisarmRequest",
    "tentaflow_ws_handler_nas_elevation_disarm"
);
register_tentanas_variant!(
    "TentaNasElevationRemoveRequest",
    "tentaflow_ws_handler_nas_elevation_remove"
);
register_tentanas_variant!(
    "TentaNasPackagesInstallRequest",
    "tentaflow_ws_handler_nas_packages_install"
);
register_tentanas_variant!("TentaNasJobsListRequest", "tentaflow_ws_handler_nas_jobs_list");
register_tentanas_variant!("TentaNasJobGetRequest", "tentaflow_ws_handler_nas_job_get");
register_tentanas_variant!("TentaNasJobCancelRequest", "tentaflow_ws_handler_nas_job_cancel");
register_tentanas_variant!("TentaNasDisksListRequest", "tentaflow_ws_handler_nas_disks_list");
register_tentanas_variant!("TentaNasDiskGetRequest", "tentaflow_ws_handler_nas_disk_get");
register_tentanas_variant!(
    "TentaNasDiskSmartTestRequest",
    "tentaflow_ws_handler_nas_disk_smart_test"
);
register_tentanas_variant!(
    "TentaNasDiskSmartTestBatchRequest",
    "tentaflow_ws_handler_nas_disk_smart_test_batch"
);
register_tentanas_variant!("TentaNasDiskLocateRequest", "tentaflow_ws_handler_nas_disk_locate");
register_tentanas_variant!(
    "TentaNasDiskWipePlanRequest",
    "tentaflow_ws_handler_nas_disk_wipe_plan"
);
register_tentanas_variant!("TentaNasDiskWipeRequest", "tentaflow_ws_handler_nas_disk_wipe");
register_tentanas_variant!("TentaNasAlertsListRequest", "tentaflow_ws_handler_nas_alerts_list");
register_tentanas_variant!("TentaNasAlertAckRequest", "tentaflow_ws_handler_nas_alert_ack");
register_tentanas_variant!("TentaNasPoolsListRequest", "tentaflow_ws_handler_nas_pools_list");
register_tentanas_variant!("TentaNasPoolGetRequest", "tentaflow_ws_handler_nas_pool_get");
register_tentanas_variant!("TentaNasPoolPlanRequest", "tentaflow_ws_handler_nas_pool_plan");
register_tentanas_variant!("TentaNasPoolCreateRequest", "tentaflow_ws_handler_nas_pool_create");
register_tentanas_variant!("TentaNasPoolDestroyRequest", "tentaflow_ws_handler_nas_pool_destroy");
register_tentanas_variant!("TentaNasPoolScrubRequest", "tentaflow_ws_handler_nas_pool_scrub");
register_tentanas_variant!("TentaNasPoolExportRequest", "tentaflow_ws_handler_nas_pool_export");
register_tentanas_variant!(
    "TentaNasPoolImportScanRequest",
    "tentaflow_ws_handler_nas_pool_import_scan"
);
register_tentanas_variant!("TentaNasPoolImportRequest", "tentaflow_ws_handler_nas_pool_import");
register_tentanas_variant!("TentaNasPoolAddVdevRequest", "tentaflow_ws_handler_nas_pool_add_vdev");
register_tentanas_variant!(
    "TentaNasPoolExpandVdevRequest",
    "tentaflow_ws_handler_nas_pool_expand_vdev"
);
register_tentanas_variant!(
    "TentaNasPoolRemoveVdevRequest",
    "tentaflow_ws_handler_nas_pool_remove_vdev"
);
register_tentanas_variant!(
    "TentaNasPoolReplaceDiskRequest",
    "tentaflow_ws_handler_nas_pool_replace_disk"
);
register_tentanas_variant!(
    "TentaNasPoolDeviceStateRequest",
    "tentaflow_ws_handler_nas_pool_device_state"
);
register_tentanas_variant!("TentaNasPoolDetachRequest", "tentaflow_ws_handler_nas_pool_detach");
register_tentanas_variant!(
    "TentaNasPoolSetPropertiesRequest",
    "tentaflow_ws_handler_nas_pool_set_properties"
);
register_tentanas_variant!(
    "TentaNasScrubScheduleSetRequest",
    "tentaflow_ws_handler_nas_scrub_schedule_set"
);
register_tentanas_variant!("TentaNasDatasetsListRequest", "tentaflow_ws_handler_nas_datasets_list");
register_tentanas_variant!("TentaNasDatasetGetRequest", "tentaflow_ws_handler_nas_dataset_get");
register_tentanas_variant!(
    "TentaNasDatasetCreateRequest",
    "tentaflow_ws_handler_nas_dataset_create"
);
register_tentanas_variant!(
    "TentaNasDatasetSetPropertiesRequest",
    "tentaflow_ws_handler_nas_dataset_set_properties"
);
register_tentanas_variant!(
    "TentaNasDatasetDestroyRequest",
    "tentaflow_ws_handler_nas_dataset_destroy"
);
register_tentanas_variant!("TentaNasDatasetKeyRequest", "tentaflow_ws_handler_nas_dataset_key");
register_tentanas_variant!("TentaNasDatasetMountRequest", "tentaflow_ws_handler_nas_dataset_mount");
register_tentanas_variant!(
    "TentaNasSnapshotsListRequest",
    "tentaflow_ws_handler_nas_snapshots_list"
);
register_tentanas_variant!(
    "TentaNasSnapshotCreateRequest",
    "tentaflow_ws_handler_nas_snapshot_create"
);
register_tentanas_variant!(
    "TentaNasSnapshotDestroyRequest",
    "tentaflow_ws_handler_nas_snapshot_destroy"
);
register_tentanas_variant!(
    "TentaNasSnapshotRollbackRequest",
    "tentaflow_ws_handler_nas_snapshot_rollback"
);
register_tentanas_variant!(
    "TentaNasSnapshotCloneRequest",
    "tentaflow_ws_handler_nas_snapshot_clone"
);
register_tentanas_variant!(
    "TentaNasSnapshotScheduleSetRequest",
    "tentaflow_ws_handler_nas_snapshot_schedule_set"
);
register_tentanas_variant!(
    "TentaNasSnapshotScheduleDeleteRequest",
    "tentaflow_ws_handler_nas_snapshot_schedule_delete"
);
register_tentanas_variant!(
    "TentaNasSnapshotSchedulesListRequest",
    "tentaflow_ws_handler_nas_snapshot_schedules_list"
);
register_tentanas_variant!(
    "TentaNasSchedulesListRequest",
    "tentaflow_ws_handler_nas_schedules_list"
);
register_tentanas_variant!(
    "TentaNasSmartScheduleSetRequest",
    "tentaflow_ws_handler_nas_smart_schedule_set"
);
register_tentanas_variant!("TentaNasSharesListRequest", "tentaflow_ws_handler_nas_shares_list");
register_tentanas_variant!("TentaNasShareGetRequest", "tentaflow_ws_handler_nas_share_get");
register_tentanas_variant!("TentaNasShareCreateRequest", "tentaflow_ws_handler_nas_share_create");
register_tentanas_variant!("TentaNasShareUpdateRequest", "tentaflow_ws_handler_nas_share_update");
register_tentanas_variant!("TentaNasShareDeleteRequest", "tentaflow_ws_handler_nas_share_delete");
register_tentanas_variant!("TentaNasShareBrowseRequest", "tentaflow_ws_handler_nas_share_browse");
register_tentanas_variant!(
    "TentaNasShareMountsRefreshRequest",
    "tentaflow_ws_handler_nas_share_mounts_refresh"
);
register_tentanas_variant!(
    "TentaNasShareUsersListRequest",
    "tentaflow_ws_handler_nas_share_users_list"
);
register_tentanas_variant!(
    "TentaNasShareUserSetRequest",
    "tentaflow_ws_handler_nas_share_user_set"
);
register_tentanas_variant!(
    "TentaNasShareUserDeleteRequest",
    "tentaflow_ws_handler_nas_share_user_delete"
);
register_tentanas_variant!(
    "TentaNasFleetMountsListRequest",
    "tentaflow_ws_handler_nas_fleet_mounts_list"
);
register_tentanas_variant!(
    "TentaNasFleetMountRetryRequest",
    "tentaflow_ws_handler_nas_fleet_mount_retry"
);
register_tentanas_variant!("TentaNasConfigExportRequest", "tentaflow_ws_handler_nas_config_export");
register_tentanas_variant!(
    "TentaNasConfigImportPlanRequest",
    "tentaflow_ws_handler_nas_config_import_plan"
);
register_tentanas_variant!(
    "TentaNasConfigImportApplyRequest",
    "tentaflow_ws_handler_nas_config_import_apply"
);
register_tentanas_variant!("TentaNasArcStatsRequest", "tentaflow_ws_handler_nas_arc_stats");
register_tentanas_variant!("TentaNasArcLimitSetRequest", "tentaflow_ws_handler_nas_arc_limit_set");
register_tentanas_variant!(
    "TentaNasElevationCatalogRequest",
    "tentaflow_ws_handler_nas_elevation_catalog"
);
register_tentanas_variant!(
    "TentaNasSnapshotBrowseRequest",
    "tentaflow_ws_handler_nas_snapshot_browse"
);
register_tentanas_variant!(
    "TentaNasApprovalsListRequest",
    "tentaflow_ws_handler_nas_approvals_list"
);
register_tentanas_variant!(
    "TentaNasApprovalDecideRequest",
    "tentaflow_ws_handler_nas_approval_decide"
);
register_tentanas_variant!(
    "TentaNasApprovalSettingsSetRequest",
    "tentaflow_ws_handler_nas_approval_settings_set"
);
register_tentanas_variant!(
    "TentaNasSnapshotProtectionReleaseRequest",
    "tentaflow_ws_handler_nas_snapshot_protection_release"
);
register_tentanas_variant!("TentaNasAccessLogRequest", "tentaflow_ws_handler_nas_access_log");
register_tentanas_variant!(
    "TentaNasAlertForwardSetRequest",
    "tentaflow_ws_handler_nas_alert_forward_set"
);
register_tentanas_variant!("TentaNasPoolTrimRequest", "tentaflow_ws_handler_nas_pool_trim");
register_tentanas_variant!(
    "TentaNasTrimScheduleSetRequest",
    "tentaflow_ws_handler_nas_trim_schedule_set"
);
register_tentanas_variant!("TentaNasTargetsListRequest", "tentaflow_ws_handler_nas_targets_list");
register_tentanas_variant!("TentaNasTargetGetRequest", "tentaflow_ws_handler_nas_target_get");
register_tentanas_variant!("TentaNasTargetCreateRequest", "tentaflow_ws_handler_nas_target_create");
register_tentanas_variant!("TentaNasTargetUpdateRequest", "tentaflow_ws_handler_nas_target_update");
register_tentanas_variant!("TentaNasTargetDeleteRequest", "tentaflow_ws_handler_nas_target_delete");
register_tentanas_variant!(
    "TentaNasElasticCapabilitiesRequest",
    "tentaflow_ws_handler_nas_elastic_capabilities"
);
register_tentanas_variant!(
    "TentaNasElasticArrayPlanRequest",
    "tentaflow_ws_handler_nas_elastic_array_plan"
);
register_tentanas_variant!("TentaNasElasticArrayCreateRequest", "tentaflow_ws_handler_nas_elastic_create");
register_tentanas_variant!(
    "TentaNasElasticArrayImportScanRequest",
    "tentaflow_ws_handler_nas_elastic_import_scan"
);
register_tentanas_variant!(
    "TentaNasElasticArrayImportRequest",
    "tentaflow_ws_handler_nas_elastic_import"
);
register_tentanas_variant!("TentaNasElasticArraysListRequest", "tentaflow_ws_handler_nas_elastic_list");
register_tentanas_variant!("TentaNasElasticArrayGetRequest", "tentaflow_ws_handler_nas_elastic_get");
register_tentanas_variant!("TentaNasElasticArrayRestoreRequest", "tentaflow_ws_handler_nas_elastic_restore");
register_tentanas_variant!("TentaNasElasticArraySyncRequest", "tentaflow_ws_handler_nas_elastic_sync");
register_tentanas_variant!("TentaNasElasticArrayScrubRequest", "tentaflow_ws_handler_nas_elastic_scrub");
register_tentanas_variant!("TentaNasElasticArrayFixRequest", "tentaflow_ws_handler_nas_elastic_fix");
register_tentanas_variant!(
    "TentaNasElasticArrayAddDiskRequest",
    "tentaflow_ws_handler_nas_elastic_add_disk"
);
register_tentanas_variant!(
    "TentaNasElasticArrayAddDiskAbortRequest",
    "tentaflow_ws_handler_nas_elastic_add_disk_abort"
);
register_tentanas_variant!(
    "TentaNasElasticArrayReplaceDiskRequest",
    "tentaflow_ws_handler_nas_elastic_replace_disk"
);
register_tentanas_variant!(
    "TentaNasElasticArrayDestroyRequest",
    "tentaflow_ws_handler_nas_elastic_destroy"
);
register_tentanas_variant!("TentaNasElasticArrayMoverRequest", "tentaflow_ws_handler_nas_elastic_mover");
register_tentanas_variant!(
    "TentaNasElasticMoverScheduleSetRequest",
    "tentaflow_ws_handler_nas_elastic_mover_schedule_set"
);
register_tentanas_variant!(
    "TentaNasElasticSyncScheduleSetRequest",
    "tentaflow_ws_handler_nas_elastic_sync_schedule_set"
);
register_tentanas_variant!(
    "TentaNasElasticScrubScheduleSetRequest",
    "tentaflow_ws_handler_nas_elastic_scrub_schedule_set"
);
register_tentanas_variant!(
    "TentaNasElasticFolderCacheSetRequest",
    "tentaflow_ws_handler_nas_elastic_folder_cache_set"
);

#[cfg(test)]
mod config_import_subject_tests {
    use super::{config_import_subject, resolve_import_alerts};
    use tentaflow_protocol::tentanas::NasAlert;

    fn import_alert(title: &str, subject: &str) -> NasAlert {
        NasAlert {
            alert_id: "a-1".into(),
            code: "approval_pending".into(),
            title: title.into(),
            params: [
                ("operation".to_string(), crate::tentanas::approvals::OP_CONFIG_IMPORT.to_string()),
                ("subject".to_string(), subject.to_string()),
            ]
            .into_iter()
            .collect(),
            ..NasAlert::default()
        }
    }

    fn has_hex_run(text: &str) -> bool {
        text.split(|c: char| !c.is_ascii_hexdigit()).any(|run| run.len() >= 32)
    }

    /// Wave-4 round-2 critic M-B: only `params.subject` was resolved, and
    /// the English title — the n01/n02 tooltip — still read "a red-path
    /// operation on '<64 hex>' …". A row stored that way is rewritten on
    /// read: by the node's name when the fleet knows it, without a subject
    /// when it does not.
    #[test]
    fn a_parked_import_alert_s_title_never_carries_the_node_id() {
        let id = "9f".repeat(32);
        let stored = format!("a red-path operation on '{id}' waits for a second admin");

        let mut named = vec![import_alert(&stored, &id)];
        resolve_import_alerts(&mut named, |_| "helios".to_string());
        assert_eq!(named[0].title, "a red-path operation on 'helios' waits for a second admin");
        assert_eq!(named[0].params.get("subject").map(String::as_str), Some("helios"));

        let mut unnamed = vec![import_alert(&stored, &id)];
        resolve_import_alerts(&mut unnamed, |_| String::new());
        assert!(!has_hex_run(&unnamed[0].title), "{}", unnamed[0].title);
        assert_eq!(unnamed[0].title, "a red-path operation waits for a second admin");
        assert!(!unnamed[0].params.contains_key("subject"));

        // Another operation's alert is not touched.
        let mut other = vec![import_alert("a red-path operation on 'tank' waits for a second admin", "tank")];
        other[0].params.insert("operation".into(), crate::tentanas::approvals::OP_POOL_DESTROY.into());
        resolve_import_alerts(&mut other, |_| "helios".to_string());
        assert_eq!(other[0].title, "a red-path operation on 'tank' waits for a second admin");
    }

    /// Wave-4 critic minor 12: an export without a node name parks its
    /// import under the node id, and "Import konfiguracji „<64 hex>”" is what
    /// the alert read. The id is resolved to the fleet's name, or dropped.
    #[test]
    fn a_config_import_is_shown_by_the_node_s_name_and_never_by_its_id() {
        let id = "9f".repeat(32);
        assert_eq!(config_import_subject(&id, |_| "helios".to_string()), "helios");
        assert_eq!(config_import_subject(&id, |_| String::new()), "", "an unknown node is not its id");
        assert_eq!(config_import_subject("helios", |_| String::new()), "helios", "a name stays a name");
        assert_eq!(config_import_subject("local", |_| String::new()), "local");
    }
}

#[cfg(test)]
mod registration_tests {
    use super::*;
    use tentaflow_protocol::SessionAuth;

    struct DispatchFixture {
        ctx: HandlerContext,
        addon_id: String,
        previous_root: Option<String>,
        _data: tempfile::TempDir,
        _overrides: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for DispatchFixture {
        fn drop(&mut self) {
            crate::addon::app_db::close(&self.addon_id);
            crate::paths::set_category_override(
                crate::paths::StorageCategory::AddonData,
                self.previous_root.take(),
            );
        }
    }

    fn dispatch_fixture() -> DispatchFixture {
        let overrides = crate::paths::lock_category_overrides();
        let data = tempfile::tempdir().expect("katalog danych testu dispatchu");
        let state = crate::dispatch::state::AppState::for_test();
        let mut ctx = crate::dispatch::test_handler_context(state, Some("user"), None);
        let SessionAuth::UserSession { user_id, .. } = &ctx.session else {
            panic!("kontekst testowy musi zawierać sesję użytkownika");
        };
        let user_id = uuid::Uuid::from_bytes(*user_id).to_string();
        ctx.state
            .db
            .write()
            .unwrap()
            .execute(
                "INSERT INTO user_accounts \
                 (id, username, password_hash, is_active, must_change_password, role) \
                 VALUES (?1, ?1, 'test', 1, 0, 'user')",
                rusqlite::params![user_id],
            )
            .expect("konto odpowiadające sesji dispatchu");
        ctx.org_context = Some(crate::services::rbac::OrgContext {
            user_id,
            org_id: "org-nas-dispatch".to_string(),
            role_id: "role-user".to_string(),
            permissions: Default::default(),
        });
        let addon_id = crate::dispatch::app_gate::test_support::install_app_instance(
            &ctx.state,
            tentanas::PACKAGE_ID,
            &uuid::Uuid::new_v4().simple().to_string(),
            &[PERM_READ],
        );
        let fixture = DispatchFixture {
            ctx,
            addon_id,
            previous_root: crate::paths::category_override(crate::paths::StorageCategory::AddonData)
                .map(|p| p.to_string_lossy().into_owned()),
            _data: data,
            _overrides: overrides,
        };
        crate::paths::set_category_override(
            crate::paths::StorageCategory::AddonData,
            Some(fixture._data.path().to_string_lossy().into_owned()),
        );
        fixture
    }

    /// B (wave 6): a schedule row says what its last slot came to as
    /// structured fields, with the started job's status read by the node —
    /// the job id stays in the node's database and is in none of them.
    #[tokio::test]
    async fn a_schedule_row_says_what_its_last_slot_came_to_without_the_job_id() {
        use tentanas::scheduler::ScheduleOutcome;
        let fixture = dispatch_fixture();
        let g = gate(&fixture.ctx, PERM_READ).unwrap();
        let daily = NasSchedule { every: "daily".into(), hour: 3, minute: 0, weekday: 0, day: 1 };
        for task in [store::PoolTask::Scrub, store::PoolTask::Trim] {
            store::set_pool_schedule(&g.db, task, "tank", true, &daily, None).unwrap();
        }
        let job = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "pool_scrub".into(),
            subject: "tank".into(),
            status: "running".into(),
            started_by: tentanas::scheduler::STARTED_BY.into(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(&g.db, &job, None).unwrap();
        store::finish_job(&g.db, &job.job_id, "succeeded", None).unwrap();
        let started = ScheduleOutcome::Started { job_id: job.job_id.clone() };
        store::record_pool_schedule_run(&g.db, store::PoolTask::Scrub, "tank", &started.stored(), None).unwrap();
        let refused = ScheduleOutcome::StartFailed { detail: "the pool is busy".into() };
        store::record_pool_schedule_run(&g.db, store::PoolTask::Trim, "tank", &refused.stored(), None).unwrap();

        let MessageBody::TentaNasBody(P::SchedulesListResponse { rows, .. }) = schedules_list(&fixture.ctx).unwrap() else {
            panic!("a schedules list")
        };
        let row = |kind: &str| rows.iter().find(|r| r.kind == kind).expect(kind).clone();
        let scrub = row("scrub");
        assert_eq!((scrub.last_outcome.as_str(), scrub.last_job_status.as_str()), ("started", "succeeded"));
        assert_eq!((scrub.last_reason.as_str(), scrub.last_detail.as_str()), ("", ""));
        let trim = row("trim");
        assert_eq!((trim.last_outcome.as_str(), trim.last_detail.as_str()), ("start_failed", "the pool is busy"));
        assert!(trim.last_job_status.is_empty());
        for r in &rows {
            for field in [&r.last_outcome, &r.last_job_status, &r.last_reason, &r.last_detail] {
                assert!(!field.contains(&job.job_id), "{}: {field}", r.kind);
            }
        }
        // An older screen still gets the sentence it parses.
        assert_eq!(scrub.last_result, format!("started job {}", job.job_id));
        // The SMART pair never carries an outcome.
        assert!(row("smart_short").last_outcome.is_empty());
    }

    /// Owner decision (wave 7): the node decides what a detach may take out,
    /// from `zpool status` read again at the request — only the original disk
    /// of a `spare-N` group whose spare is ready. The spare itself, an
    /// ordinary leaf, an empty name and any disk during a resilver are refused.
    #[test]
    fn a_detach_is_allowed_only_for_the_disk_a_ready_spare_replaced() {
        let status = |scan: &str| {
            tentanas::pools::parse_status(&format!(
                "  pool: tank\n state: DEGRADED\n  scan: {scan}\nconfig:\n\n\
\tNAME            STATE     READ WRITE CKSUM\n\
\ttank            DEGRADED     0     0     0\n\
\t  mirror-0      DEGRADED     0     0     0\n\
\t    /dev/sda    ONLINE       0     0     0\n\
\t    spare-1     DEGRADED     0     0     0\n\
\t      /dev/sdb  FAULTED      9    40     0\n\
\t      /dev/sdk  ONLINE       0     0     0\n\
\tspares\n\
\t  /dev/sdk      INUSE     currently in use\n\
\nerrors: No known data errors\n"
            ))
        };
        let settled = status("resilvered 1T in 01:00:00 with 0 errors on Tue Sep  1 12:00:00 2026");
        assert!(detach_allowed(&settled, "sdb"));
        for other in ["sdk", "sda", "", "tank"] {
            assert!(!detach_allowed(&settled, other), "{other}");
        }
        let running = status("resilver in progress since Tue Sep  1 09:00:00 2026\n\t1T scanned\n\t0B repaired, 40.00% done, 01:00:00 to go");
        assert!(!detach_allowed(&running, "sdb"), "not while the spare resilvers");
        assert_eq!(POOL_DETACH_NOT_ALLOWED, "refusal:pool_detach_not_allowed");
    }

    /// Critic wave 7, MAJOR 2: the fleet's 10 s poll asks for the light
    /// answer. It never computes the capabilities (environment read + full
    /// `zfs list`), and a second poll within `FLEET_SESSIONS_MAX_AGE` makes no
    /// privileged NVMe-oF session read; the targets and the service rows are
    /// still there. The Sharing tab's full answer does compute them.
    #[tokio::test]
    async fn the_fleet_summary_of_targets_skips_the_heavy_reads() {
        let fixture = dispatch_fixture();
        let g = gate(&fixture.ctx, PERM_READ).unwrap();
        let row = store::TargetRow {
            target_id: "t-nvme".into(),
            name: "scratch".into(),
            protocol: "nvmet".into(),
            wwn: "nqn.2026-09.local.tentaflow:helios:scratch".into(),
            enabled: true,
            state: "active".into(),
            created_at: store::now(),
            updated_at: store::now(),
            ..Default::default()
        };
        store::upsert_target(&g.db, &g.org_id, &row).unwrap();
        let caps = || CAPABILITY_READS.with(|n| n.get());
        let sudo = || tentanas::targets::SESSION_READS.with(|n| n.get());

        let (caps0, sudo0) = (caps(), sudo());
        for _ in 0..2 {
            let MessageBody::TentaNasBody(P::TargetsListResponse { targets, services, .. }) =
                targets_list(&fixture.ctx, true).await.unwrap()
            else {
                panic!("a targets list")
            };
            assert_eq!(targets.len(), 1);
            assert_eq!(targets[0].name, "scratch");
            assert_eq!(services.len(), 2, "the LIO and nvmet rows the services chip reads");
        }
        assert_eq!(caps() - caps0, 0, "the summary never computes capabilities");
        assert!(sudo() - sudo0 <= 1, "at most one privileged session read for two polls, not one per poll");
        let after_two = sudo();
        targets_list(&fixture.ctx, true).await.unwrap();
        assert_eq!(sudo(), after_two, "a poll within the window reads no sessions");

        targets_list(&fixture.ctx, false).await.unwrap();
        assert_eq!(caps() - caps0, 1, "the Sharing tab's full answer still computes them");
    }

    #[tokio::test]
    async fn a_tentanas_frame_reaches_its_handler_through_dispatch() {
        let fixture = dispatch_fixture();
        let body = MessageBody::TentaNasBody(P::NodesListRequest {});

        let (answer, is_error) = crate::dispatch::dispatch(&body, &fixture.ctx).await;

        assert!(!is_error, "dispatch zwrócił błąd: {answer:?}");
        let MessageBody::TentaNasBody(P::NodesListResponse {
            local_node_id,
            nodes,
        }) = answer
        else {
            panic!("oczekiwano NodesListResponse, otrzymano: {answer:?}");
        };
        assert_eq!(local_node_id, "test-node");
        assert_eq!(nodes.len(), 1, "lista musi zawierać lokalny node");
        assert_eq!(nodes[0].node_id, local_node_id);
        assert!(nodes[0].is_local);
        assert!(nodes[0].online);
    }

    #[tokio::test]
    async fn an_anonymous_tentanas_frame_is_denied_through_dispatch() {
        let mut fixture = dispatch_fixture();
        fixture.ctx.session = SessionAuth::Anonymous;
        let body = MessageBody::TentaNasBody(P::NodesListRequest {});

        let (answer, is_error) = crate::dispatch::dispatch(&body, &fixture.ctx).await;

        assert!(is_error, "odmowa musi ustawić flagę błędu");
        let MessageBody::Error(error) = answer else {
            panic!("oczekiwano odmowy dostępu, otrzymano: {answer:?}");
        };
        assert_eq!(error.code, ProtocolErrorCode::PolicyDenied);
        assert_eq!(
            error.message,
            "TentaNasNodesListRequest requires UserSession session"
        );
    }

    #[tokio::test]
    async fn a_tentanas_frame_without_read_permission_is_denied_through_dispatch() {
        let fixture = dispatch_fixture();
        crate::dispatch::app_gate::test_support::set_permission(
            &fixture.ctx.state,
            &fixture.addon_id,
            "user",
            &fixture.ctx.org_context.as_ref().unwrap().user_id,
            PERM_READ,
            "deny",
        );
        let body = MessageBody::TentaNasBody(P::NodesListRequest {});

        let (answer, is_error) = crate::dispatch::dispatch(&body, &fixture.ctx).await;

        assert!(is_error, "odmowa musi ustawić flagę błędu");
        let MessageBody::Error(error) = answer else {
            panic!("oczekiwano odmowy dostępu, otrzymano: {answer:?}");
        };
        assert_eq!(error.code, ProtocolErrorCode::PolicyDenied);
        assert_eq!(error.message, "nas.read permission required");
    }

    fn elastic_request() -> P {
        P::ElasticArrayCreateRequest { name:"media".into(),filesystem:"ext4".into(),
            data_disk_ids:vec!["data".into()],parity_disk_ids:Vec::new(),cache_disk_ids:Vec::new(),confirm_name:"media".into(),
            sudo_password:Some(SudoSecret("never-persist-this-password".into())) }
    }

    fn elastic_admin(fixture: &mut DispatchFixture) {
        fixture.ctx.org_context.as_mut().unwrap().permissions.insert("org.admin".into());
        for permission in [PERM_READ,PERM_ADMIN,PERM_POOLS] {
            crate::dispatch::app_gate::test_support::set_permission(&fixture.ctx.state,&fixture.addon_id,
                "user",&fixture.ctx.org_context.as_ref().unwrap().user_id,permission,"allow");
        }
    }

    /// Round 4 (critic C2): the platform admin's list of internal collectors
    /// names internal hosts and networks — only a platform admin gets it
    /// from the settings list.
    #[tokio::test]
    async fn only_a_platform_admin_reads_the_collector_allowlist() {
        let state = crate::dispatch::state::AppState::for_test();
        crate::db::repository::set_setting(&state.db, tentanas::forward::ALLOWLIST_SETTING, r#"[{"entry":"siem.lan"}]"#).unwrap();
        let listed = |role: &str| {
            let ctx = crate::dispatch::test_handler_context(state.clone(), Some(role), None);
            let MessageBody::SettingsListResponse { entries } = crate::dispatch::handlers::settings_list(&MessageBody::SettingsListRequest, &ctx).unwrap() else {
                panic!("a settings list")
            };
            entries.iter().any(|e| e.key == tentanas::forward::ALLOWLIST_SETTING)
        };
        assert!(listed("admin"));
        assert!(!listed("user"), "an ordinary user gets nothing of it");
    }

    /// MAJOR 5 of the wave-9b critic, through the real dispatcher: the
    /// retired node-wide target cannot be edited or recreated — only deleted
    /// — and a reader of the access log never receives a target's address.
    #[tokio::test]
    async fn the_node_wide_target_is_retired_and_a_reader_gets_no_address() {
        let mut fixture = dispatch_fixture();
        let set = |enabled: bool, syslog: &str, webhook: &str, node_wide: bool| tn(P::AlertForwardSetRequest {
            enabled,
            syslog_target: syslog.to_string(),
            webhook_url: webhook.to_string(),
            include_access: false,
            node_wide,
        });
        // A reader first: it may read the log, never set a target.
        let (response, error) = crate::dispatch::dispatch(&set(true, "", "https://siem.example.com/in/SECRET", false), &fixture.ctx).await;
        assert!(error, "{response:?}");
        elastic_admin(&mut fixture);
        let (response, error) = crate::dispatch::dispatch(&set(true, "", "https://siem.example.com/in/SECRET", false), &fixture.ctx).await;
        assert!(!error, "{response:?}");
        let MessageBody::TentaNasBody(P::AccessLogResponse { forward, .. }) = response else { panic!("an access log") };
        assert_eq!(forward.webhook_url, "https://siem.example.com/…", "the admin reads it masked");
        // Creating or editing a node-wide target is refused; deleting is not.
        let (response, error) = crate::dispatch::dispatch(&set(true, "legacy.example.com:514", "", true), &fixture.ctx).await;
        assert!(error);
        assert!(matches!(&response, MessageBody::Error(e) if e.message == tentanas::forward::FORWARD_NODE_RETIRED), "{response:?}");
        let (response, error) = crate::dispatch::dispatch(&set(false, "", "", true), &fixture.ctx).await;
        assert!(!error, "{response:?}");
        // A reader of the same organisation: on, and nothing about where.
        fixture.ctx.org_context.as_mut().unwrap().permissions.remove("org.admin");
        let (response, error) = crate::dispatch::dispatch(&tn(P::AccessLogRequest {
            share: String::new(), user: String::new(), operation: String::new(), result: String::new(), since: String::new(), limit: 0,
        }), &fixture.ctx).await;
        assert!(!error, "{response:?}");
        let MessageBody::TentaNasBody(P::AccessLogResponse { forward, .. }) = response else { panic!("an access log") };
        assert!(forward.enabled);
        assert_eq!((forward.webhook_url.as_str(), forward.syslog_target.as_str()), ("", ""));
    }

    #[tokio::test]
    async fn elastic_create_permissions_and_retype_precede_all_storage_work() {
        let mut fixture=dispatch_fixture();
        let request=tn(elastic_request());
        let (response,error)=crate::dispatch::dispatch(&request,&fixture.ctx).await;
        assert!(error);
        assert!(matches!(response,MessageBody::Error(ProtocolError {code:ProtocolErrorCode::PolicyDenied,..})));
        elastic_admin(&mut fixture);
        let mut bad=elastic_request();
        if let P::ElasticArrayCreateRequest {confirm_name,..}=&mut bad { *confirm_name="wrong".into(); }
        let (response,error)=crate::dispatch::dispatch(&tn(bad),&fixture.ctx).await;
        assert!(error);
        assert!(matches!(response,MessageBody::Error(ProtocolError {code:ProtocolErrorCode::BadRequest,..})));
        let g=gate(&fixture.ctx,PERM_READ).unwrap();
        assert!(store::list_jobs(&g.db,100).unwrap().is_empty());
        assert!(store::elastic_claims(&g.db).unwrap().is_empty());
        assert_eq!(tentanas::elevation::audit_entries(&g.db),0);
    }

    /// Owner decision 2026-09-26: a pool holding another organisation's
    /// share or target is not destroyed, and the refusal names neither the
    /// organisation nor the resource. The asking organisation's own
    /// resources still follow the existing flow (listed by the dialog, taken
    /// with the pool). A check that could not be made refuses.
    #[test]
    fn pool_destroy_refuses_while_another_organisation_uses_the_pool() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        store::migrate(&conn).unwrap();
        let db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        let mounts = vec!["/tank".to_string(), "/tank/projekty".to_string()];
        let share = |org: &str, name: &str, path: &str, dataset: Option<&str>| {
            store::upsert_share(&db, org, &store::ShareRow {
                share_id: format!("s-{name}"),
                name: name.into(),
                protocol: "smb".into(),
                source_path: path.into(),
                dataset: dataset.map(str::to_string),
                enabled: true,
                created_at: "now".into(),
                updated_at: "now".into(),
                ..Default::default()
            }).unwrap();
        };
        let target = |org: &str, name: &str, kind: &str, source: &str| {
            store::upsert_target(&db, org, &store::TargetRow {
                target_id: format!("t-{name}"),
                name: name.into(),
                protocol: "iscsi".into(),
                wwn: format!("iqn.2026-09.test:{name}"),
                enabled: true,
                luns: vec![tentaflow_protocol::tentanas::NasTargetLun {
                    source: source.into(),
                    source_kind: kind.into(),
                    ..Default::default()
                }],
                created_at: "now".into(),
                updated_at: "now".into(),
                ..Default::default()
            }).unwrap();
        };
        let guard = |mounts: Option<&[String]>| pool_destroy_guard(&db, "org-a", "tank", mounts);

        // The asking organisation's own share and zvol: the existing flow.
        share("org-a", "projekty", "/tank/projekty", Some("tank/projekty"));
        target("org-a", "vm-a", "zvol", "tank/vm-a");
        assert!(guard(Some(&mounts)).is_ok(), "same-organisation resources follow the existing flow");
        // Another organisation's, but on another pool: not this pool's business.
        share("org-b", "media", "/srv/media/x", None);
        target("org-b", "vm-other", "zvol", "tankard/vm");
        assert!(guard(Some(&mounts)).is_ok(), "`tankard` is not `tank`");

        for (why, add) in [
            ("a share by path", Box::new(|| share("org-b", "b-docs", "/tank/projekty/b", None)) as Box<dyn Fn()>),
            ("a share on a dataset", Box::new(|| share("org-b", "b-root", "/elsewhere", Some("tank")))),
            ("a zvol LUN", Box::new(|| target("org-b", "b-vm", "zvol", "tank/b-vm"))),
            ("a file LUN", Box::new(|| target("org-b", "b-file", "file", "/tank/images/b.img"))),
        ] {
            add();
            let refused = guard(Some(&mounts)).expect_err(why);
            assert_eq!(refused.code, ProtocolErrorCode::Conflict, "{why}");
            assert_eq!(refused.message, POOL_DESTROY_FOREIGN, "{why}: coded, and it names nobody");
            db.write().unwrap().execute_batch(
                "DELETE FROM nas_shares WHERE name LIKE 'b-%'; DELETE FROM nas_targets WHERE name LIKE 'b-%';",
            ).unwrap();
            assert!(guard(Some(&mounts)).is_ok(), "{why}: clean again");
        }

        let unverified = guard(None).expect_err("datasets unreadable");
        assert_eq!(unverified.message, POOL_DESTROY_UNVERIFIED);
        // With the datasets unreadable, a foreign zvol by name is still FOREIGN.
        target("org-b", "b-zvol", "zvol", "tank/b-zvol");
        assert_eq!(guard(None).expect_err("zvol by name").message, POOL_DESTROY_FOREIGN);
        db.write().unwrap().execute_batch("DELETE FROM nas_targets WHERE name = 'b-zvol';").unwrap();

        // Critic wave 9a, MINOR 6: another organisation's target whose record
        // cannot be read may be on the pool — unverified, never "no LUNs".
        target("org-b", "b-corrupt", "zvol", "elsewhere/x");
        db.write().unwrap().execute_batch("UPDATE nas_targets SET spec_json = '{not json' WHERE name = 'b-corrupt';").unwrap();
        assert_eq!(guard(Some(&mounts)).expect_err("corrupt record").message, POOL_DESTROY_UNVERIFIED);
        db.write().unwrap().execute_batch("DELETE FROM nas_targets WHERE name = 'b-corrupt';").unwrap();
        // The asking organisation's own corrupt record is not this check's.
        target("org-a", "a-corrupt", "zvol", "elsewhere/y");
        db.write().unwrap().execute_batch("UPDATE nas_targets SET spec_json = '{not json' WHERE name = 'a-corrupt';").unwrap();
        assert!(guard(Some(&mounts)).is_ok());
        db.write().unwrap().execute_batch("ALTER TABLE nas_targets RENAME TO nas_targets_gone").unwrap();
        assert_eq!(guard(Some(&mounts)).expect_err("rows unreadable").message, POOL_DESTROY_UNVERIFIED);
    }

    /// Critic wave 9a, R2-MINOR 2: a destroy the job's own re-check refuses
    /// leaves the pool's schedules in place — they go only with a destroy
    /// that ran.
    #[tokio::test]
    async fn a_destroy_refused_by_the_job_keeps_the_pool_schedules() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        store::migrate(&conn).unwrap();
        let db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        let schedule = tentaflow_protocol::tentanas::NasSchedule::default();
        store::set_pool_schedule(&db, store::PoolTask::Scrub, "tank", true, &schedule, None).unwrap();
        store::upsert_share(&db, "org-b", &store::ShareRow {
            share_id: "s-b".into(), name: "b".into(), protocol: "smb".into(), source_path: "/tank/b".into(),
            dataset: Some("tank/b".into()), enabled: true, created_at: "now".into(), updated_at: "now".into(),
            ..Default::default()
        }).unwrap();
        let h = tentanas::jobs::JobHandle::for_test(&db, "job-destroy-refused");
        let refused = pool_destroy_job(h, db.clone(), "org-a".into(), "nas".into(), "tank".into(),
            HelperCommand::ZpoolDestroy { pool: "tank".into() }, None).await.expect_err("refused");
        assert_eq!(refused.to_string(), POOL_DESTROY_FOREIGN);
        assert!(store::pool_schedule(&db, store::PoolTask::Scrub, "tank").unwrap().is_some(), "the schedule stays");
    }

    /// Critic wave 9a, MINOR 10: the approved execution of a parked pool
    /// destroy runs the guard too, before any job exists — through
    /// `execute_approved`, so a reorder that put the guard behind the
    /// `origin` check, or after the spawn, fails here.
    #[tokio::test]
    async fn an_approved_pool_destroy_is_refused_while_another_organisation_uses_the_pool() {
        let mut fixture = dispatch_fixture();
        elastic_admin(&mut fixture);
        let g = gate(&fixture.ctx, PERM_READ).unwrap();
        store::upsert_share(&g.db, "org-other", &store::ShareRow {
            share_id: "s-theirs".into(),
            name: "theirs".into(),
            protocol: "smb".into(),
            source_path: "/tank/theirs".into(),
            dataset: Some("tank/theirs".into()),
            enabled: true,
            created_at: "now".into(),
            updated_at: "now".into(),
            ..Default::default()
        }).unwrap();
        let parked = P::PoolDestroyRequest { name: "tank".into(), confirm_name: "tank".into(), sudo_password: None };
        let refused = execute_approved(&fixture.ctx, &parked, None).await.expect_err("refused");
        assert_eq!(refused.code, ProtocolErrorCode::Conflict);
        assert_eq!(refused.message, POOL_DESTROY_FOREIGN, "coded, naming nobody");
        assert!(store::list_jobs(&g.db, 100).unwrap().is_empty(), "no destroy job was started");
    }

    /// Critic wave 9a, MINOR 5 and R2-MAJOR 1: a creation never lands on a
    /// pool between a destroy's last check and `zpool destroy`, and never
    /// waits for the destroy without limit: while the lock is held it is
    /// refused (coded) after a short wait, and it runs once the lock is free.
    #[tokio::test]
    async fn share_and_target_creation_are_refused_while_a_pool_destroy_holds_the_lock() {
        assert_eq!(POOL_DESTROY_IN_PROGRESS, tentanas::pools::POOL_DESTROY_IN_PROGRESS);
        let mut fixture = dispatch_fixture();
        elastic_admin(&mut fixture);
        for permission in [PERM_SHARES, PERM_TARGETS] {
            crate::dispatch::app_gate::test_support::set_permission(&fixture.ctx.state, &fixture.addon_id,
                "user", &fixture.ctx.org_context.as_ref().unwrap().user_id, permission, "allow");
        }
        let g = gate(&fixture.ctx, PERM_READ).unwrap();
        let share = P::ShareCreateRequest {
            name: "lockcheck".into(), protocol: "smb".into(), source_path: "/tank/lockcheck".into(),
            smb: None, nfs: None, fleet_mount: false, enabled: false, sudo_password: None,
        };
        let target = P::TargetCreateRequest {
            name: "lockcheck".into(), protocol: "iscsi".into(), source: "tank/lockcheck".into(),
            create_size_bytes: 0, thin: false, portal_interface: String::new(), transports: Vec::new(),
            auth: None, initiators: Vec::new(), confirm_all_interfaces: false, enabled: false, sudo_password: None,
        };
        let bound = tentanas::pools::RESOURCES_LOCK_WAIT + std::time::Duration::from_secs(5);
        let held = tentanas::pools::resources_lock(&g.db).lock_owned().await;
        for (what, answer) in [
            ("share", tokio::time::timeout(bound, share_create(&fixture.ctx, &share)).await.expect("bounded wait")),
            ("target", tokio::time::timeout(bound, target_create(&fixture.ctx, &target)).await.expect("bounded wait")),
        ] {
            let refused = answer.expect_err(what);
            assert_eq!(refused.code, ProtocolErrorCode::Conflict, "{what}");
            assert_eq!(refused.message, POOL_DESTROY_IN_PROGRESS, "{what}: coded");
        }
        assert!(store::share_by_name(&g.db, "lockcheck").unwrap().is_none(), "nothing was created behind the refusal");
        drop(held);
        // Free again: the creation runs (its own checks decide the rest).
        for (what, answer) in [
            ("share", tokio::time::timeout(bound, share_create(&fixture.ctx, &share)).await.expect("runs")),
            ("target", tokio::time::timeout(bound, target_create(&fixture.ctx, &target)).await.expect("runs")),
        ] {
            if let Err(e) = answer {
                assert_ne!(e.message, POOL_DESTROY_IN_PROGRESS, "{what} is not refused once the lock is free");
            }
        }
    }

    /// Owner decision 2026-09-26: the orphaned rows are one viewer's when it
    /// is active and no OTHER active organisation has resources on this node
    /// (the viewer itself need own nothing here).
    #[test]
    fn a_viewer_is_the_sole_organisation_only_when_no_other_active_one_owns_anything_here() {
        let set = |ids: &[&str]| ids.iter().map(|s| s.to_string()).collect::<std::collections::BTreeSet<_>>();
        let orgs = |rows: &[(&str, &str)]| {
            rows.iter().map(|(id, status)| (id.to_string(), status.to_string())).collect::<std::collections::BTreeMap<_, _>>()
        };
        let statuses = orgs(&[("org-a", "active"), ("org-b", "active"), ("org-default", "active"), ("org-gone", "deleted"), ("org-paused", "suspended")]);
        assert!(is_sole_org(&statuses, &set(&["org-a"]), "org-a"));
        assert!(!is_sole_org(&statuses, &set(&["org-a", "org-b"]), "org-a"), "two owners: nobody sees the orphans");
        assert!(!is_sole_org(&statuses, &set(&["org-b"]), "org-a"), "another organisation is the owner here");
        assert!(is_sole_org(&statuses, &set(&["org-a", "org-gone"]), "org-a"), "a soft-deleted owner does not count");
        assert!(
            !is_sole_org(&statuses, &set(&["org-a", "org-paused"]), "org-a"),
            "a suspended owner is still present: its rows stay hidden"
        );
        assert!(!is_sole_org(&statuses, &set(&["org-paused"]), "org-a"), "the same with the viewer owning nothing");
        assert!(
            is_sole_org(&statuses, &set(&["org-a"]), "org-a"),
            "an active organisation without resources here does not count"
        );
        assert!(
            is_sole_org(&statuses, &set(&[]), "org-a"),
            "the viewer owns nothing here and nobody else does: it sees the orphans"
        );
        assert!(is_sole_org(&statuses, &set(&["org-gone"]), "org-a"), "only a soft-deleted owner besides it");
        assert!(!is_sole_org(&statuses, &set(&["org-unknown"]), "org-a"), "an owner nothing says is gone still counts");
        assert!(!is_sole_org(&statuses, &set(&[]), "org-gone"), "a soft-deleted viewer");
        assert!(!is_sole_org(&statuses, &set(&[]), "org-paused"), "a suspended viewer");
        assert!(!is_sole_org(&statuses, &set(&[]), "org-x"), "a viewer no organisation row knows");
        assert!(!is_sole_org(&orgs(&[("", "active")]), &set(&[""]), ""), "an empty org id is no tenant");
    }

    /// The set an Elastic journal's owner is judged against: every
    /// organisation this node has a row for, a soft-deleted one included —
    /// its data is still under retention, so its array must stay out of
    /// another tenant's reach — and never an org the node has not heard of.
    #[test]
    fn orgs_on_node_holds_every_organisation_of_this_node_a_deleted_one_included() {
        let fixture = dispatch_fixture();
        let db = &fixture.ctx.state.db;
        let live = crate::services::org::create_organization(db, "Tenant A", "tenant-a", None, None, None, None).unwrap();
        let gone = crate::services::org::create_organization(db, "Tenant B", "tenant-b", None, None, None, None).unwrap();
        assert!(crate::services::org::delete_organization(db, &gone.org_id).unwrap());

        let orgs = orgs_on_node(&fixture.ctx).unwrap();
        assert!(orgs.contains(&live.org_id));
        assert!(orgs.contains(&gone.org_id), "a soft-deleted org still owns its arrays");
        assert!(!orgs.contains("org-from-another-machine"));
        let asking = ElasticOwner { org_id: live.org_id.clone(), addon_id: "nas".into() };
        let journal_owner = ElasticOwner { org_id: gone.org_id.clone(), addon_id: "nas".into() };
        assert_eq!(
            tentanas::elastic::owner_kind(&journal_owner, &asking, &orgs),
            tentanas::elastic::OWNER_KIND_OTHER_ORG_ON_NODE
        );
    }

    /// Shares, block targets and share accounts belong to the organisation
    /// that created them (migration 20). Another organisation's are not in
    /// the lists, and every read, edit, delete and mount refresh of them
    /// answers exactly like an id that does not exist — before any privileged
    /// step, and without changing a byte of the other tenant's rows. Its
    /// account's password cannot be set from here: that would hand this
    /// tenant the other tenant's login.
    #[tokio::test]
    async fn another_orgs_shares_targets_and_accounts_are_invisible_and_refused() {
        let mut fixture = dispatch_fixture();
        elastic_admin(&mut fixture);
        for permission in [PERM_SHARES, PERM_TARGETS] {
            crate::dispatch::app_gate::test_support::set_permission(
                &fixture.ctx.state,
                &fixture.addon_id,
                "user",
                &fixture.ctx.org_context.as_ref().unwrap().user_id,
                permission,
                "allow",
            );
        }
        let g = gate(&fixture.ctx, PERM_READ).unwrap();
        let theirs = store::ShareRow {
            share_id: "s-other".into(),
            name: "kadry".into(),
            protocol: "nfs".into(),
            source_path: "/mnt/tank/kadry".into(),
            nfs: Some(Default::default()),
            state: "active".into(),
            created_at: store::now(),
            updated_at: store::now(),
            ..Default::default()
        };
        store::upsert_share(&g.db, "org-other", &theirs).unwrap();
        store::upsert_target(
            &g.db,
            "org-other",
            &store::TargetRow {
                target_id: "t-other".into(),
                name: "vm-other".into(),
                protocol: "iscsi".into(),
                wwn: "iqn.2026-09.pl.test:n.vm-other".into(),
                auth_method: "none".into(),
                state: "disabled".into(),
                created_at: store::now(),
                updated_at: store::now(),
                ..Default::default()
            },
        )
        .unwrap();
        store::upsert_share_user(&g.db, "org-other", "obcy", "kadry").unwrap();

        // The lists read `list_shares_of_org` / `list_share_users` /
        // `list_targets_of_org` with the gate's org (db.rs tests hold those);
        // `shares_list` itself is not called here because its service rows
        // run the node's environment probe.
        assert!(store::list_shares_of_org(&g.db, &g.org_id).unwrap().is_empty());
        assert!(store::list_share_users(&g.db, &g.org_id).unwrap().is_empty());
        assert!(store::list_targets_of_org(&g.db, &g.org_id).unwrap().is_empty());

        let not_found = |e: ProtocolError| assert_eq!(e.code, ProtocolErrorCode::NotFound, "{}", e.message);
        not_found(share_get(&fixture.ctx, "s-other").await.unwrap_err());
        not_found(share_mounts_refresh(&fixture.ctx, "s-other").await.unwrap_err());
        not_found(
            share_update(
                &fixture.ctx,
                &P::ShareUpdateRequest {
                    share_id: "s-other".into(),
                    smb: None,
                    nfs: Some(Default::default()),
                    fleet_mount: false,
                    enabled: false,
                    sudo_password: None,
                },
            )
            .await
            .unwrap_err(),
        );
        not_found(share_delete(&fixture.ctx, "s-other", "kadry", None, Origin::Direct).await.unwrap_err());
        not_found(target_get(&fixture.ctx, "t-other").await.unwrap_err());
        not_found(target_delete(&fixture.ctx, "t-other", "vm-other", None, Origin::Direct).await.unwrap_err());
        not_found(share_user_delete(&fixture.ctx, "obcy", None).await.unwrap_err());
        let taken = share_user_set(
            &fixture.ctx,
            &P::ShareUserSetRequest {
                name: "obcy".into(),
                password: Some(tentaflow_protocol::tentanas::NasSecret("przejete".into())),
                description: String::new(),
                sudo_password: None,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(taken.code, ProtocolErrorCode::BadRequest);
        assert!(taken.message.contains("already in use on this node"), "{}", taken.message);
        assert!(!taken.message.contains("org-other"), "{}", taken.message);

        // Nothing of the other tenant's changed, and no job was started.
        assert_eq!(store::share(&g.db, "org-other", "s-other").unwrap(), Some(theirs));
        assert!(store::target(&g.db, "org-other", "t-other").unwrap().is_some());
        assert_eq!(store::share_user_owner(&g.db, "obcy").unwrap().as_deref(), Some("org-other"));
        assert_eq!(store::list_share_users(&g.db, "org-other").unwrap()[0].description, "kadry");
        assert!(store::list_jobs(&g.db, 100).unwrap().is_empty());
    }

    /// A legacy account migration 20 gave to this organisation because it
    /// had grants in two (critic wave 3, MINOR 1): deleting it would remove
    /// the node account ANOTHER organisation's share still serves its users
    /// with. Refused before either password database is touched, and the
    /// refusal names neither that share nor its organisation.
    #[tokio::test]
    async fn deleting_an_account_another_orgs_share_still_grants_is_refused_without_naming_it() {
        let mut fixture = dispatch_fixture();
        elastic_admin(&mut fixture);
        crate::dispatch::app_gate::test_support::set_permission(
            &fixture.ctx.state,
            &fixture.addon_id,
            "user",
            &fixture.ctx.org_context.as_ref().unwrap().user_id,
            PERM_SHARES,
            "allow",
        );
        let g = gate(&fixture.ctx, PERM_READ).unwrap();
        store::upsert_share_user(&g.db, &g.org_id, "wspolny", "").unwrap();
        let theirs = store::ShareRow {
            share_id: "s-other".into(),
            name: "kadry-tajne".into(),
            protocol: "smb".into(),
            source_path: "/mnt/tank/kadry-tajne".into(),
            smb: Some(tentaflow_protocol::tentanas::NasSmbOptions {
                users: vec![tentaflow_protocol::tentanas::NasShareAccess {
                    user: "wspolny".into(),
                    mode: "rw".into(),
                }],
                ..Default::default()
            }),
            state: "active".into(),
            created_at: store::now(),
            updated_at: store::now(),
            ..Default::default()
        };
        store::upsert_share(&g.db, "org-other", &theirs).unwrap();

        let refused = share_user_delete(&fixture.ctx, "wspolny", None).await.unwrap_err();
        assert_eq!(refused.code, ProtocolErrorCode::BadRequest, "{}", refused.message);
        assert_eq!(refused.message, store::SHARE_USER_IN_USE_ELSEWHERE);
        assert!(!refused.message.contains("kadry-tajne") && !refused.message.contains("org-other"));
        // The store holds the same line on its own, for a caller that forgets.
        let err = store::delete_share_user(&g.db, &g.org_id, "wspolny").unwrap_err();
        assert_eq!(err.to_string(), store::SHARE_USER_IN_USE_ELSEWHERE);
        // Nothing moved: the account and the other organisation's grant stay.
        assert!(store::share_user_exists(&g.db, &g.org_id, "wspolny").unwrap());
        assert_eq!(store::share_grants(&g.db, "s-other").unwrap().len(), 1);

        // Once no other organisation grants it, it is this one's to delete
        // (the store half; the handler would go on to the privilege channel).
        store::delete_share(&g.db, "org-other", "s-other").unwrap();
        assert!(!store::share_user_granted_elsewhere(&g.db, &g.org_id, "wspolny").unwrap());
        assert!(store::delete_share_user(&g.db, &g.org_id, "wspolny").unwrap());
    }

    /// The recovery path is admin-only and validates what it was given before
    /// it asks for root, and a node with no privilege channel says what to do
    /// about it rather than failing opaquely — the journals are unreachable
    /// without that channel, so this is the first thing an admin meets.
    #[tokio::test]
    async fn the_array_import_gates_and_validates_before_it_ever_asks_for_root() {
        let mut fixture = dispatch_fixture();
        let array_id = "11111111-1111-4111-8111-111111111111";
        for denied in [
            elastic_import_scan(&fixture.ctx, None).await.unwrap_err(),
            elastic_import(&fixture.ctx, array_id, "media", None).await.unwrap_err(),
        ] {
            assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);
        }

        elastic_admin(&mut fixture);
        // An array is addressed by its journal id. Anything that is not one is
        // refused in the handler, never handed to the privilege channel.
        let bad = elastic_import(&fixture.ctx, "media", "media", None).await.unwrap_err();
        assert_eq!(bad.code, ProtocolErrorCode::BadRequest);

        for (scope, error) in [
            ("scan", elastic_import_scan(&fixture.ctx, None).await.unwrap_err()),
            ("adopt", elastic_import(&fixture.ctx, array_id, "media", None).await.unwrap_err()),
        ] {
            assert_eq!(error.code, ProtocolErrorCode::NotAvailable, "{scope}: {}", error.message);
            assert!(
                error.message.contains("TentaNas") && error.message.contains("konfiguracji kanału"),
                "{scope} musi nazwać lekarstwo: {}",
                error.message
            );
        }

        // And nothing was written on the way to any of those refusals.
        let g = gate(&fixture.ctx, PERM_READ).unwrap();
        assert!(store::list_jobs(&g.db, 100).unwrap().is_empty());
        assert!(store::elastic_array_identities(&g.db).unwrap().is_empty());
        assert_eq!(tentanas::elevation::audit_entries(&g.db), 0);
    }

    /// One array, `active`, with parity, in the instance database — the state
    /// every lifecycle refusal below is measured against.
    fn active_array(fixture: &mut DispatchFixture) -> tentanas_helper::elastic::ElasticCreateSpec {
        elastic_admin(fixture);
        let g = gate_destructive(&fixture.ctx).unwrap();
        let mut spec = tentanas::elastic::tests::create_spec("media");
        spec.owner = elastic_owner(&g);
        let created = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "elastic_create".into(),
            subject: spec.name.clone(),
            status: "running".into(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(
            &g.db,
            &created,
            Some(&tentanas::jobs::ElasticJobIntent::Create(spec.clone())),
        )
        .unwrap();
        store::finish_elastic_operation(
            &g.db,
            &spec.owner,
            &spec.operation_id,
            Ok(&tentanas::elastic::tests::ready_result(&spec)),
        )
        .unwrap();
        store::finish_job(&g.db, &created.job_id, "succeeded", None).unwrap();
        spec
    }

    /// A scrub of this array that ended without success — the parity fault a
    /// repair is reached for, and the only thing that arms one.
    fn failed_scrub(g: &Gate, spec: &tentanas_helper::elastic::ElasticCreateSpec) {
        let operation_id = uuid::Uuid::now_v7().to_string();
        let scrub = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "elastic_scrub".into(),
            subject: spec.name.clone(),
            status: "running".into(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(
            &g.db,
            &scrub,
            Some(&tentanas::jobs::ElasticJobIntent::Snapraid {
                owner: spec.owner.clone(),
                array_id: spec.array_id.clone(),
                operation_id: operation_id.clone(),
                kind: tentanas_helper::elastic::ElasticSnapraidKind::Scrub,
                acknowledge_parity_fault: None,
            }),
        )
        .unwrap();
        // A scrub that FOUND errors reports them: a scrub with no result is
        // only interrupted, and that is no fault a repair could address.
        store::record_snapraid_result(
            &g.db,
            &spec.owner,
            &operation_id,
            &tentanas::elastic::tests::snapraid_result(
                spec,
                &operation_id,
                tentanas_helper::elastic::ElasticSnapraidKind::Scrub,
                tentanas_helper::elastic::ElasticSnapraidOutcome::Failed,
            ),
        )
        .unwrap();
        store::finish_job(&g.db, &scrub.job_id, "failed", Some("parity errors")).unwrap();
    }

    /// What a REPAIR refuses, in the order it refuses it, and the proof that
    /// none of the refusals reaches storage.
    ///
    /// A repair WRITES the named disk back from the parity checkpoint (only
    /// where a Scrub marked it bad, or where a replaced disk has nothing). So there are three separate ways for the request to be
    /// wrong, and each has to arrive as its own sentence: no parity to rebuild
    /// from, a disk this array does not carry, and — the one that keeps a
    /// healthy array healthy — nothing to repair at all.
    #[tokio::test]
    async fn a_repair_refuses_without_parity_without_the_disk_and_without_a_fault() {
        use tentanas_helper::elastic::ElasticSnapraidKind;
        let fix = |disk: &str| ElasticSnapraidKind::Fix { disk: disk.into() };
        let mut fixture = dispatch_fixture();
        // A reader cannot even ask.
        let denied = elastic_snapraid(&fixture.ctx, "media", None, Origin::Direct, fix("d1"), None)
            .await
            .unwrap_err();
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);

        let spec = active_array(&mut fixture);
        let g = gate_destructive(&fixture.ctx).unwrap();

        // NOTHING TO REPAIR. The array is active, its parity is there, the
        // disk is one of its own — and the answer is still no, because a
        // repair would overwrite healthy data. The sentence names the remedy.
        let healthy = elastic_snapraid(&fixture.ctx, "media", None, Origin::Direct, fix("d1"), None)
            .await
            .unwrap_err();
        assert_eq!(healthy.code, ProtocolErrorCode::NotAvailable);
        assert!(
            healthy.message.contains("nie zgłasza awarii") && healthy.message.contains("scrub"),
            "{}",
            healthy.message
        );

        // A disk the array does not carry, on an array that DOES have a fault
        // to repair — so the refusal can only be about the disk.
        failed_scrub(&g, &spec);
        let array = store::elastic_array(&g.db, &spec.owner, "media").unwrap().unwrap();
        assert!(
            array.unresolved_operation,
            "a scrub that failed has to leave the array with something to resolve"
        );

        let foreign = elastic_snapraid(&fixture.ctx, "media", None, Origin::Direct, fix("d9"), None)
            .await
            .unwrap_err();
        assert_eq!(foreign.code, ProtocolErrorCode::BadRequest);
        assert!(foreign.message.contains("d9"), "{}", foreign.message);

        // NO PARITY. The array's own parity disk is what a repair rebuilds
        // from; without one there is nothing to rebuild from at all.
        let mut bare = tentanas::elastic::tests::create_spec("bare");
        bare.owner = spec.owner.clone();
        bare.parity.clear();
        let bare_job = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "elastic_create".into(),
            subject: bare.name.clone(),
            status: "running".into(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(
            &g.db,
            &bare_job,
            Some(&tentanas::jobs::ElasticJobIntent::Create(bare.clone())),
        )
        .unwrap();
        let mut bare_ready = tentanas::elastic::tests::ready_result(&bare);
        // An array without parity has no sync checkpoint to report, and
        // `validate_observation` refuses one that claims otherwise.
        bare_ready.sync_completed_at = None;
        store::finish_elastic_operation(&g.db, &bare.owner, &bare.operation_id, Ok(&bare_ready))
            .unwrap();
        store::finish_job(&g.db, &bare_job.job_id, "succeeded", None).unwrap();
        let no_parity = elastic_snapraid(&fixture.ctx, "bare", None, Origin::Direct, fix("d1"), None)
            .await
            .unwrap_err();
        assert_eq!(no_parity.code, ProtocolErrorCode::NotAvailable);
        assert!(no_parity.message.contains("parity"), "{}", no_parity.message);

        // Not one of those refusals started a job or touched the channel.
        assert_eq!(
            store::list_jobs(&g.db, 100)
                .unwrap()
                .into_iter()
                .filter(|job| job.kind == "elastic_fix")
                .count(),
            0
        );
        assert_eq!(tentanas::elevation::audit_entries(&g.db), 0);
    }

    /// DISK REPLACEMENT IS WITHDRAWN, and this is the proof that asking for one
    /// changes nothing on the node.
    ///
    /// The previous version of this handler wrote an operation row and an
    /// `elastic_replace_disk` job, asked the helper, and the helper refused at
    /// its very first hop because the command was missing from `actions::run`.
    /// The failure then left BOTH the operation row and the array row
    /// `needs_attention`, and only a succeeded `fix` cleared a `replace_disk`
    /// row — so a single click cost the array every later Sync, Scrub, mover
    /// run and disk addition. Hence the refusal has to land BEFORE the first
    /// write, and hence this test counts rows rather than reading a message:
    /// a refusal that still writes is the defect, not the message.
    #[tokio::test]
    async fn a_replacement_is_refused_before_it_writes_anything() {
        let mut fixture = dispatch_fixture();
        // A reader cannot even ask: the permission gate stays first.
        let denied = elastic_replace_disk(
            &fixture.ctx, "media", "d1", "d1", "wwn-new", false, None, Origin::Direct,
        )
        .await
        .unwrap_err();
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);

        let spec = active_array(&mut fixture);
        let g = gate_destructive(&fixture.ctx).unwrap();
        // The state a replacement was meant for: the slot's disk is gone.
        g.db.write()
            .unwrap()
            .execute(
                "DELETE FROM nas_disks WHERE disk_id=?1",
                rusqlite::params![spec.data[0].disk_id],
            )
            .unwrap();
        // Four eyes on, so a version that parked would leave an approval row.
        tentanas::approvals::set_settings(&actor(&fixture.ctx, &g).unwrap(), true, 24).unwrap();
        let jobs_before = store::list_jobs(&g.db, 100).unwrap().len();
        let state_before: String = g
            .db
            .read()
            .unwrap()
            .query_row("SELECT state FROM nas_elastic_arrays WHERE name='media'", [], |r| r.get(0))
            .unwrap();

        // Every shape of the request is refused, with a sentence that tells the
        // admin what the array can still do.
        for (name, disk, confirm, replacement, stale) in [
            ("media", "d1", "d1", "wwn-new-disk", false),
            ("media", "d1", "d1", "wwn-new-disk", true),
            ("media", "d9", "d9", "wwn-new-disk", false),
            ("inna", "d1", "d1", "wwn-new-disk", false),
        ] {
            let refused = elastic_replace_disk(
                &fixture.ctx,
                name,
                disk,
                confirm,
                replacement,
                stale,
                Some(&SudoSecret("never-store-replace-secret".into())),
                Origin::Direct,
            )
            .await
            .unwrap_err();
            assert_eq!(refused.code, ProtocolErrorCode::NotAvailable, "{}", refused.message);
            assert!(refused.message.contains("naprawa"), "{}", refused.message);
        }

        // NOTHING WAS WRITTEN: no operation of any kind, no job, no approval,
        // no change to the array row, and nothing in the elevation audit that
        // would mean the helper was asked.
        let db = g.db.read().unwrap();
        let replacements: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM nas_elastic_operations WHERE kind='replace_disk'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(replacements, 0, "no replace_disk operation may ever exist");
        let approvals: i64 = db.query_row("SELECT COUNT(*) FROM nas_pending_approvals", [], |r| r.get(0)).unwrap();
        assert_eq!(approvals, 0, "a withdrawn request must not park for a second admin");
        let state_after: String = db
            .query_row("SELECT state FROM nas_elastic_arrays WHERE name='media'", [], |r| r.get(0))
            .unwrap();
        drop(db);
        assert_eq!(state_after, state_before, "the array row must be untouched");
        assert_eq!(store::list_jobs(&g.db, 100).unwrap().len(), jobs_before, "no job");
        assert_eq!(tentanas::elevation::audit_entries(&g.db), 0, "the helper was never asked");
    }

    /// A REPAIR parks for a second admin, and the approval row names the disk.
    ///
    /// Without this the `Fix` arm of `elastic_maintenance_gates_and_approval_…`
    /// was dead code — that loop only iterates Sync and Scrub — so
    /// `OP_ELASTIC_FIX` could have been mis-wired, or the payload could have
    /// carried the sudo password to disk, and every test would still have been
    /// green. A repair cannot join that loop: it needs a parity fault before it
    /// is offered at all, so it needs an array that HAS one.
    #[tokio::test]
    async fn a_repair_parks_for_a_second_admin_naming_the_disk_and_storing_no_secret() {
        use tentanas_helper::elastic::ElasticSnapraidKind;
        let mut fixture = dispatch_fixture();
        let spec = active_array(&mut fixture);
        let g = gate_destructive(&fixture.ctx).unwrap();
        failed_scrub(&g, &spec);
        tentanas::approvals::set_settings(&actor(&fixture.ctx, &g).unwrap(), true, 24).unwrap();

        let response = elastic_snapraid(
            &fixture.ctx,
            "media",
            Some(&SudoSecret("never-store-repair-secret".into())),
            Origin::Direct,
            ElasticSnapraidKind::Fix { disk: "d1".into() },
            None,
        )
        .await
        .unwrap();
        let MessageBody::TentaNasBody(P::ApprovalPendingResponse { approval }) = response else {
            panic!("a repair on a four-eyes fleet has to park")
        };
        let stored = store::approval(&g.db, &approval.request_id).unwrap().unwrap();
        assert_eq!(stored.approval.operation, tentanas::approvals::OP_ELASTIC_FIX);
        assert_eq!(stored.approval.subject, "media");
        // The row has to name the DISK: what it authorises is overwriting that
        // one disk from parity, and an approval reading only "repair" would be
        // an approval for whichever disk the author picked afterwards.
        // It names it as the approver knows it (by number here: this node
        // does not see the disk); the REQUEST keeps the slot the node keys it by.
        assert!(stored.approval.detail.contains("data disk no. 1"), "{}", stored.approval.detail);
        assert!(stored.payload_json.contains("\"disk\":\"d1\""), "{}", stored.payload_json);
        assert!(!stored.payload_json.contains("never-store-repair-secret"));
        assert!(!stored.payload_json.contains("sudo_password"));
        // Nothing ran, and the author cannot release their own request.
        assert!(store::list_jobs(&g.db, 100)
            .unwrap()
            .into_iter()
            .all(|job| job.kind != "elastic_fix"));
        assert!(matches!(
            tentanas::approvals::claim(&actor(&fixture.ctx, &g).unwrap(), &approval.request_id),
            Err(tentanas::approvals::ApprovalError::OwnRequest)
        ));
        // The repair's own sentence describes `-e fix`, not the unfiltered
        // rebuild this product never runs.
        assert!(stored.approval.detail.contains("marked bad"), "{}", stored.approval.detail);
        assert!(!stored.approval.detail.contains("overwrit"), "{}", stored.approval.detail);
        // The code names the disk by its number, never by its slot: the node
        // of this fixture does not see the disk, so it has no kernel name.
        let [fix] = stored.approval.detail_reasons.as_slice() else {
            panic!("one coded detail: {:?}", stored.approval.detail_reasons)
        };
        assert_eq!(fix.code, "elastic_fix");
        assert_eq!(fix.params.get("number").map(String::as_str), Some("1"));
        assert!(fix.params.values().all(|v| v != "d1"), "{:?}", fix.params);
        // …and so does the node's own sentence, which is the approvals
        // tooltip and the parked alert's text (wave-6 critic MAJOR 1).
        assert!(stored.approval.detail.ends_with("on data disk no. 1"), "{}", stored.approval.detail);
        assert!(!stored.approval.detail.contains("d1"), "{}", stored.approval.detail);
        let parked_alert = store::alerts_for_subject(&g.db, "approval", &approval.request_id).unwrap();
        assert!(!parked_alert.is_empty() && parked_alert.iter().all(|a| !a.detail.contains("d1")), "{parked_alert:?}");

        // AND THE SYNC PARKED ON THE SAME ARRAY SAYS WHAT IT COSTS. A Sync over
        // an array whose scrub reported errors is a different operation from a
        // Sync over a healthy one — measured: the marked blocks stay
        // repairable, the unreadable files leave the content file for good —
        // and the approver reads only this sentence.
        //
        // WITHOUT THE ADMIN'S ACKNOWLEDGEMENT it does not even park (I3): the
        // refusal is a code the screen words, and nothing is stored.
        let unacknowledged = elastic_snapraid(
            &fixture.ctx,
            "media",
            Some(&SudoSecret("never-store-sync-secret".into())),
            Origin::Direct,
            ElasticSnapraidKind::Sync,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(unacknowledged.code, ProtocolErrorCode::NotAvailable);
        assert_eq!(unacknowledged.message, SYNC_NEEDS_ACKNOWLEDGEMENT);
        // An acknowledgement of ANOTHER fault acknowledges nothing (M1).
        let observed = tentanas::elastic::get(&g.db, &spec.owner, "media").await.unwrap().unwrap();
        assert!(observed.sync_needs_acknowledgement && !observed.sync_fault_id.is_empty());
        let stale = elastic_snapraid(
            &fixture.ctx,
            "media",
            None,
            Origin::Direct,
            ElasticSnapraidKind::Sync,
            Some("abababab-abab-4bab-8bab-abababababab".into()),
        )
        .await
        .unwrap_err();
        assert_eq!(stale.message, SYNC_FAULT_CHANGED);
        let parked_sync = elastic_snapraid(
            &fixture.ctx,
            "media",
            Some(&SudoSecret("never-store-sync-secret".into())),
            Origin::Direct,
            ElasticSnapraidKind::Sync,
            Some(observed.sync_fault_id.clone()),
        )
        .await
        .unwrap();
        let MessageBody::TentaNasBody(P::ApprovalPendingResponse { approval: sync_approval }) = parked_sync
        else {
            panic!("a sync on a four-eyes fleet has to park")
        };
        let sync_row = store::approval(&g.db, &sync_approval.request_id).unwrap().unwrap();
        assert_eq!(sync_row.approval.operation, tentanas::approvals::OP_ELASTIC_SYNC);
        assert_eq!(
            sync_row.approval.detail_reasons.iter().map(|r| r.code.as_str()).collect::<Vec<_>>(),
            vec!["elastic_sync_over_fault"],
            "the warning, as a code the screen words"
        );
        assert!(sync_row.approval.detail.contains("can no longer be restored"), "{}", sync_row.approval.detail);
        // The alert the park raises carries the sentence, not a code in its
        // place: its detail is the forwarded text and the tooltip.
        let alert = store::alerts_for_subject(&g.db, "approval", &sync_approval.request_id).unwrap();
        assert!(alert.iter().all(|a| !a.detail.starts_with("text:")), "{alert:?}");
        // The acknowledgement is parked WITH the request: the approver
        // releases the author's decision, not a Sync that would be refused.
        assert!(
            sync_row.payload_json.contains(&format!("\"acknowledge_parity_fault\":\"{}\"", observed.sync_fault_id)),
            "{}",
            sync_row.payload_json
        );
    }

    /// Wave-6 critic MAJOR 1: the repair's English names the disk the way
    /// its code does, and never by a slot, whatever the node knows of it.
    #[test]
    fn a_parked_repair_is_worded_without_the_slot() {
        assert!(fix_detail_text("sdb", "2").ends_with("on disk sdb"));
        assert!(fix_detail_text("", "2").ends_with("on data disk no. 2"));
        assert!(fix_detail_text("", "").ends_with("on a data disk of the array"));
        for text in [fix_detail_text("sdb", "2"), fix_detail_text("", "2"), fix_detail_text("", "")] {
            assert!(!["d1", "d2", "c1", "parity1"].iter().any(|slot| text.contains(slot)), "{text}");
        }
    }

    /// An add that STOPPED PART-WAY (K4, F3/F4 on the node's side): while it
    /// is pinned no other disk is admitted — the refusal is a code the screen
    /// words — and its UNDO is started only when the helper said the disk
    /// never joined the share (its journal's `joined`, which a live read can
    /// only add refusals to). Without that answer the undo is refused, and so
    /// is an undo of a disk nothing pinned. Nothing is started
    /// by any of it.
    #[tokio::test]
    async fn an_unfinished_add_admits_only_itself_and_its_undo_needs_the_helpers_word() {
        let mut fixture = dispatch_fixture();
        let spec = active_array(&mut fixture);
        let g = gate_destructive(&fixture.ctx).unwrap();
        // Nothing pinned: there is nothing to undo.
        let nothing = elastic_add_disk_abort(&fixture.ctx, "media", "grow-data-2", "media", None, Origin::Direct)
            .await
            .unwrap_err();
        assert_eq!(nothing.code, ProtocolErrorCode::NotAvailable);
        assert_eq!(nothing.message, "refusal:elastic_nothing_to_undo");

        // An add that stopped after the helper grew its journal.
        let pinned = tentanas_helper::elastic::ElasticDiskSpec {
            disk_id: "grow-data-2".into(),
            wwn: Some("wwn-grow-data-2".into()),
            serial: Some("serial-grow-data-2".into()),
            bytes: 16 * 1024 * 1024 * 1024,
            expected_uuid: uuid::Uuid::new_v4().to_string(),
        };
        let operation_id = uuid::Uuid::now_v7().to_string();
        let job = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "elastic_add_disk".into(),
            subject: spec.name.clone(),
            status: "running".into(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(
            &g.db,
            &job,
            Some(&tentanas::jobs::ElasticJobIntent::AddDisk {
                owner: spec.owner.clone(),
                array_id: spec.array_id.clone(),
                operation_id: operation_id.clone(),
                disk: pinned.clone(),
            }),
        )
        .unwrap();
        store::finish_elastic_operation(&g.db, &spec.owner, &operation_id, Err("mount nie powiódł się")).unwrap();
        store::finish_job(&g.db, &job.job_id, "failed", Some("mount nie powiódł się")).unwrap();
        let jobs_before = store::list_jobs(&g.db, 100).unwrap().len();

        // Another disk is refused while it stands, by code.
        let other = elastic_add_disk(&fixture.ctx, "media", "some-other-disk", "media", None, Origin::Direct)
            .await
            .unwrap_err();
        assert_eq!(other.code, ProtocolErrorCode::NotAvailable);
        assert_eq!(other.message, "refusal:elastic_attention_add_disk");

        // The helper could not be asked here, so nothing says the disk never
        // joined the share: the undo is refused, and says why.
        let undo = elastic_add_disk_abort(&fixture.ctx, "media", "grow-data-2", "media", None, Origin::Direct)
            .await
            .unwrap_err();
        assert_eq!(undo.code, ProtocolErrorCode::NotAvailable);
        assert_eq!(undo.message, "refusal:elastic_state_unknown");
        // A wrong retype never gets that far.
        let retype = elastic_add_disk_abort(&fixture.ctx, "media", "grow-data-2", "medi", None, Origin::Direct)
            .await
            .unwrap_err();
        assert_eq!(retype.code, ProtocolErrorCode::BadRequest);
        assert_eq!(store::list_jobs(&g.db, 100).unwrap().len(), jobs_before, "nothing was started");
        let pending = store::unfinished_elastic_add_disk(&g.db, &spec.owner, &spec.array_id).unwrap();
        assert_eq!(pending.as_ref(), Some(&pinned), "and the add is still pinned");
    }

    /// What ADDING A DISK refuses.
    ///
    /// Freedom is judged from the node's disk inventory, never from a mount
    /// table: an array's branches are mounted inside the mergerfs process's
    /// own namespace, so the host's `/proc/mounts` shows none of them and
    /// every member disk would read as free. A disk that is already in THIS
    /// array therefore has to be named as such — it is the admin's disk in the
    /// admin's array, and "not free" would send them looking for an owner
    /// that is themselves.
    #[tokio::test]
    async fn adding_a_disk_refuses_a_member_a_missing_disk_and_a_wrong_retype() {
        let mut fixture = dispatch_fixture();
        let denied = elastic_add_disk(&fixture.ctx, "media", "d", "media", None, Origin::Direct)
            .await
            .unwrap_err();
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);

        let spec = active_array(&mut fixture);
        let g = gate_destructive(&fixture.ctx).unwrap();

        // The retype comes FIRST, before the array is even read: a mistyped
        // confirmation must not be answered with "that array does not exist".
        let retype =
            elastic_add_disk(&fixture.ctx, "media", "whatever", "wrong", None, Origin::Direct)
                .await
                .unwrap_err();
        assert_eq!(retype.code, ProtocolErrorCode::BadRequest);
        assert!(retype.message.contains("confirmation"), "{}", retype.message);

        // A DISK THIS ARRAY ALREADY HOLDS, by its data role and by its parity
        // role, each named as the member it is.
        for (disk_id, member) in [
            (spec.data[0].disk_id.clone(), "d1"),
            (spec.parity[0].disk_id.clone(), "parity1"),
        ] {
            let error =
                elastic_add_disk(&fixture.ctx, "media", &disk_id, "media", None, Origin::Direct)
                    .await
                    .unwrap_err();
            assert_eq!(error.code, ProtocolErrorCode::BadRequest, "{member}");
            assert!(
                error.message.contains("media") && error.message.contains(member),
                "{member}: {}",
                error.message
            );
        }

        // A disk that is not on this node at all is not free either, and the
        // refusal comes from the inventory rather than from a mount table.
        let absent =
            elastic_add_disk(&fixture.ctx, "media", "no-such-disk", "media", None, Origin::Direct)
                .await
                .unwrap_err();
        assert!(
            matches!(
                absent.code,
                ProtocolErrorCode::NotFound | ProtocolErrorCode::BadRequest
            ),
            "{absent:?}"
        );

        // An empty or oversized identifier never reaches the inventory.
        for bad in ["", &"x".repeat(129)] {
            let error =
                elastic_add_disk(&fixture.ctx, "media", bad, "media", None, Origin::Direct)
                    .await
                    .unwrap_err();
            assert_eq!(error.code, ProtocolErrorCode::BadRequest);
        }

        // And the array still has exactly the disks it started with.
        let after = store::elastic_array(&g.db, &spec.owner, "media").unwrap().unwrap();
        assert_eq!(after.data().count(), 1);
        assert_eq!(
            store::list_jobs(&g.db, 100)
                .unwrap()
                .into_iter()
                .filter(|job| job.kind == "elastic_add_disk")
                .count(),
            0
        );
        assert_eq!(tentanas::elevation::audit_entries(&g.db), 0);
    }

    /// What DESTROYING refuses: a reader, a mistyped name, and a share that
    /// still points at the union.
    ///
    /// The share is refused rather than deleted along the way, and that is a
    /// decision rather than an omission: deleting a share is its own red path
    /// with its own four-eyes approval, and an array operation must not be a
    /// side door around it.
    #[tokio::test]
    async fn destroying_refuses_a_reader_a_wrong_retype_and_a_share_on_the_union() {
        let mut fixture = dispatch_fixture();
        let denied = elastic_destroy(&fixture.ctx, "media", "media", None, Origin::Direct)
            .await
            .unwrap_err();
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);

        let spec = active_array(&mut fixture);
        let g = gate_destructive(&fixture.ctx).unwrap();

        // WITHOUT THE RETYPED NAME nothing happens, and the refusal is about
        // the confirmation rather than about the array.
        for wrong in ["", "medi", "MEDIA", "media "] {
            let error = elastic_destroy(&fixture.ctx, "media", wrong, None, Origin::Direct)
                .await
                .unwrap_err();
            assert_eq!(error.code, ProtocolErrorCode::BadRequest, "{wrong:?}");
            assert!(error.message.contains("confirmation"), "{}", error.message);
        }
        assert!(store::elastic_array(&g.db, &spec.owner, "media").unwrap().is_some());

        // A SHARE ON THE UNION holds the array: the sentence names the share
        // so the admin knows what to remove first.
        let union = tentanas_helper::elastic::union_path("media");
        store::upsert_share(
            &g.db,
            &g.org_id,
            &store::ShareRow {
                share_id: "share-1".into(),
                name: "media-smb".into(),
                protocol: "smb".into(),
                source_path: format!("{union}/filmy"),
                dataset: None,
                enabled: true,
                fleet_mount: false,
                smb: Some(Default::default()),
                nfs: None,
                state: "active".into(),
                state_detail: String::new(),
                state_reasons: Vec::new(),
                created_at: store::now(),
                updated_at: store::now(),
            },
        )
        .unwrap();
        let held = elastic_destroy(&fixture.ctx, "media", "media", None, Origin::Direct)
            .await
            .unwrap_err();
        assert_eq!(held.code, ProtocolErrorCode::NotAvailable);
        assert!(held.message.contains("media-smb"), "{}", held.message);

        // Nothing was started, and the rows are all still there.
        assert!(store::elastic_array(&g.db, &spec.owner, "media").unwrap().is_some());
        assert_eq!(
            store::list_jobs(&g.db, 100)
                .unwrap()
                .into_iter()
                .filter(|job| job.kind == "elastic_destroy")
                .count(),
            0
        );
        assert_eq!(tentanas::elevation::audit_entries(&g.db), 0);
    }

    #[tokio::test]
    async fn elastic_maintenance_gates_and_approval_do_not_start_storage_work() {
        use tentanas_helper::elastic::ElasticSnapraidKind;
        for kind in [ElasticSnapraidKind::Sync, ElasticSnapraidKind::Scrub] {
            let mut fixture = dispatch_fixture();
            let denied = elastic_snapraid(&fixture.ctx, "media", None, Origin::Direct, kind.clone(), None)
                .await
                .unwrap_err();
            assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);
            elastic_admin(&mut fixture);
            let g = gate_destructive(&fixture.ctx).unwrap();
            let mut spec = tentanas::elastic::tests::create_spec("media");
            spec.owner = elastic_owner(&g);
            let created = tentaflow_protocol::tentanas::NasJob {
                job_id: uuid::Uuid::now_v7().to_string(),
                kind: "elastic_create".into(),
                subject: spec.name.clone(),
                status: "running".into(),
                started_at: store::now(),
                ..Default::default()
            };
            store::insert_job(
                &g.db,
                &created,
                Some(&tentanas::jobs::ElasticJobIntent::Create(spec.clone())),
            )
            .unwrap();
            store::finish_elastic_operation(
                &g.db,
                &spec.owner,
                &spec.operation_id,
                Ok(&tentanas::elastic::tests::ready_result(&spec)),
            )
            .unwrap();
            store::finish_job(&g.db, &created.job_id, "succeeded", None).unwrap();
            tentanas::approvals::set_settings(&actor(&fixture.ctx, &g).unwrap(), true, 24).unwrap();
            let request = match &kind {
                ElasticSnapraidKind::Sync => P::ElasticArraySyncRequest {
                    name: "media".into(),
                    // A healthy array: an acknowledgement nothing needs is
                    // DROPPED, never parked as a blank cheque (M1).
                    acknowledge_parity_fault: Some("abababab-abab-4bab-8bab-abababababab".into()),
                    sudo_password: Some(SudoSecret("never-store-maintenance-secret".into())),
                },
                ElasticSnapraidKind::Scrub => P::ElasticArrayScrubRequest {
                    name: "media".into(),
                    sudo_password: Some(SudoSecret("never-store-maintenance-secret".into())),
                },
                ElasticSnapraidKind::Fix { disk } => P::ElasticArrayFixRequest {
                    name: "media".into(),
                    disk: disk.clone(),
                    confirm_disk: disk.clone(),
                    sudo_password: Some(SudoSecret("never-store-maintenance-secret".into())),
                },
            };
            let (response, error) = crate::dispatch::dispatch(&tn(request), &fixture.ctx).await;
            assert!(!error, "{response:?}");
            let MessageBody::TentaNasBody(P::ApprovalPendingResponse { approval }) = response
            else {
                panic!("Brak approval")
            };
            let stored = store::approval(&g.db, &approval.request_id)
                .unwrap()
                .unwrap();
            assert!(
                !stored
                    .payload_json
                    .contains("never-store-maintenance-secret")
            );
            assert!(!stored.payload_json.contains("sudo_password"));
            assert!(!stored.payload_json.contains("acknowledge_parity_fault"), "{}", stored.payload_json);
            assert_eq!(
                stored.approval.operation,
                format!("elastic_{}", tentanas::elastic::snapraid_kind(&kind))
            );
            assert!(matches!(
                tentanas::approvals::claim(&actor(&fixture.ctx, &g).unwrap(), &approval.request_id),
                Err(tentanas::approvals::ApprovalError::OwnRequest)
            ));
            let missing = elastic_snapraid(&fixture.ctx, "foreign", None, Origin::Direct, kind.clone(), None)
                .await
                .unwrap_err();
            assert_eq!(missing.code, ProtocolErrorCode::NotFound);
            assert_eq!(store::list_jobs(&g.db, 100).unwrap().len(), 1);
            assert_eq!(tentanas::elevation::audit_entries(&g.db), 0);
            for permission in [PERM_READ, PERM_ADMIN, PERM_POOLS] {
                crate::dispatch::app_gate::test_support::set_permission(
                    &fixture.ctx.state,
                    &fixture.addon_id,
                    "user",
                    &fixture.ctx.org_context.as_ref().unwrap().user_id,
                    permission,
                    "deny",
                );
                assert_eq!(
                    elastic_snapraid(&fixture.ctx, "media", None, Origin::Direct, kind.clone(), None)
                        .await
                        .unwrap_err()
                        .code,
                    ProtocolErrorCode::PolicyDenied
                );
                crate::dispatch::app_gate::test_support::set_permission(
                    &fixture.ctx.state,
                    &fixture.addon_id,
                    "user",
                    &fixture.ctx.org_context.as_ref().unwrap().user_id,
                    permission,
                    "allow",
                );
            }
        }
    }

    /// Creates a completed Elastic array from `spec` in the fixture's store.
    async fn settled_array(g: &Gate, spec: &ElasticCreateSpec) {
        let created = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "elastic_create".into(),
            subject: spec.name.clone(),
            status: "running".into(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(
            &g.db,
            &created,
            Some(&tentanas::jobs::ElasticJobIntent::Create(spec.clone())),
        )
        .unwrap();
        store::finish_elastic_operation(
            &g.db,
            &spec.owner,
            &spec.operation_id,
            Ok(&tentanas::elastic::tests::ready_result(spec)),
        )
        .unwrap();
        store::finish_job(&g.db, &created.job_id, "succeeded", None).unwrap();
    }

    #[tokio::test]
    async fn elastic_mover_refuses_a_cacheless_array_and_parks_before_moving_anything() {
        let mut fixture = dispatch_fixture();
        let denied = elastic_mover(&fixture.ctx, "media", None, Origin::Direct)
            .await
            .unwrap_err();
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);
        elastic_admin(&mut fixture);
        let g = gate_destructive(&fixture.ctx).unwrap();

        let missing = elastic_mover(&fixture.ctx, "media", None, Origin::Direct)
            .await
            .unwrap_err();
        assert_eq!(missing.code, ProtocolErrorCode::NotFound);

        // An array with no cache branch has nothing to move: the run is refused
        // rather than started and reported as a successful move of nothing.
        let mut cacheless = tentanas::elastic::tests::create_spec("media");
        cacheless.owner = elastic_owner(&g);
        settled_array(&g, &cacheless).await;
        let refused = elastic_mover(&fixture.ctx, "media", None, Origin::Direct)
            .await
            .unwrap_err();
        assert_eq!(refused.code, ProtocolErrorCode::NotAvailable);

        // The same request on an array WITH a cache parks under four eyes, and
        // parks without the password and without starting any work.
        let mut cached = tentanas::elastic::tests::mover_spec("cached");
        cached.owner = elastic_owner(&g);
        settled_array(&g, &cached).await;
        tentanas::approvals::set_settings(&actor(&fixture.ctx, &g).unwrap(), true, 24).unwrap();
        let before = store::list_jobs(&g.db, 100).unwrap().len();
        let (response, error) = crate::dispatch::dispatch(
            &tn(P::ElasticArrayMoverRequest {
                name: "cached".into(),
                sudo_password: Some(SudoSecret("never-store-mover-secret".into())),
            }),
            &fixture.ctx,
        )
        .await;
        assert!(!error, "{response:?}");
        let MessageBody::TentaNasBody(P::ApprovalPendingResponse { approval }) = response else {
            panic!("Brak approval movera")
        };
        let stored = store::approval(&g.db, &approval.request_id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.approval.operation, tentanas::approvals::OP_ELASTIC_MOVER);
        assert!(!stored.payload_json.contains("never-store-mover-secret"));
        assert!(!stored.payload_json.contains("sudo_password"));
        assert_eq!(store::list_jobs(&g.db, 100).unwrap().len(), before);
        assert_eq!(tentanas::elevation::audit_entries(&g.db), 0);
        assert!(matches!(
            tentanas::approvals::claim(&actor(&fixture.ctx, &g).unwrap(), &approval.request_id),
            Err(tentanas::approvals::ApprovalError::OwnRequest)
        ));

        // Every app permission is load-bearing on this path.
        for permission in [PERM_READ, PERM_ADMIN, PERM_POOLS] {
            crate::dispatch::app_gate::test_support::set_permission(
                &fixture.ctx.state,
                &fixture.addon_id,
                "user",
                &fixture.ctx.org_context.as_ref().unwrap().user_id,
                permission,
                "deny",
            );
            assert_eq!(
                elastic_mover(&fixture.ctx, "cached", None, Origin::Direct)
                    .await
                    .unwrap_err()
                    .code,
                ProtocolErrorCode::PolicyDenied
            );
            crate::dispatch::app_gate::test_support::set_permission(
                &fixture.ctx.state,
                &fixture.addon_id,
                "user",
                &fixture.ctx.org_context.as_ref().unwrap().user_id,
                permission,
                "allow",
            );
        }
    }

    /// The per-folder cache policy: who may set one, what it refuses, and the
    /// fact that it never asks for root.
    ///
    /// It is deliberately NOT gated like the schedule next door: that one arms
    /// unattended privileged work, this one writes a row. So the assertion
    /// about permissions is the pool permission alone, and the assertion about
    /// the red path is that nothing parked and nothing ran.
    #[tokio::test]
    async fn setting_a_folder_cache_policy_refuses_a_reader_a_bad_policy_and_an_unreadable_union() {
        let mut fixture = dispatch_fixture();
        // No pool permission at all: settled before the request's shape, so a
        // reader who also misspelled the policy still gets `PolicyDenied`.
        let denied = elastic_folder_cache_set(&fixture.ctx, "media", "foto", "maybe")
            .await
            .unwrap_err();
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);

        elastic_admin(&mut fixture);
        let g = gate(&fixture.ctx, PERM_POOLS).unwrap();
        let mut spec = tentanas::elastic::tests::create_spec("media");
        spec.owner = elastic_owner(&g);
        settled_array(&g, &spec).await;

        // An unknown policy value names the three that exist.
        let bad = elastic_folder_cache_set(&fixture.ctx, "media", "foto", "maybe")
            .await
            .unwrap_err();
        assert_eq!(bad.code, ProtocolErrorCode::BadRequest);
        assert!(bad.message.contains("only"), "{}", bad.message);

        // A folder that is a PATH is refused before anything is read: the name
        // becomes a mover rule the helper resolves under the union.
        for folder in ["a/b", "..", ""] {
            let refused = elastic_folder_cache_set(&fixture.ctx, "media", folder, "only")
                .await
                .unwrap_err();
            assert_eq!(refused.code, ProtocolErrorCode::BadRequest, "{folder:?}");
        }

        // An array nobody has is `NotFound`, and it is a different answer from
        // a folder nobody has.
        let missing = elastic_folder_cache_set(&fixture.ctx, "niema", "foto", "only")
            .await
            .unwrap_err();
        assert_eq!(missing.code, ProtocolErrorCode::NotFound);

        // The clause is that an unreadable union is never "this array has no
        // folders" — but "/mnt/media does not exist on a test host" is the
        // wrong way to produce one. MEASURED on the owner's node
        // (2026-09-15): `/mnt/media` is a real mounted union there, so the
        // list reads fine, `foto` is simply absent, and `NotFound` is the
        // CORRECT answer — this assertion failed for being right. A test that
        // only passes where the product is not installed is exactly the blind
        // spot that let seven requests ship with no wire encoder.
        //
        // So the array under test is one whose union path no machine mounts.
        let mut nowhere = tentanas::elastic::tests::create_spec("probe-unmounted-union");
        nowhere.owner = elastic_owner(&g);
        settled_array(&g, &nowhere).await;
        let unknown = elastic_folder_cache_set(&fixture.ctx, "probe-unmounted-union", "foto", "only")
            .await
            .unwrap_err();
        assert_eq!(unknown.code, ProtocolErrorCode::NotAvailable);
        assert!(
            unknown.message.contains("Nie można odczytać listy folderów"),
            "{}",
            unknown.message
        );
        // And it wrote nothing — a refused request must not leave a rule the
        // next mover run would honour.
        let stored: i64 = g
            .db
            .read()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM nas_elastic_folder_cache", [], |r| r.get(0))
            .unwrap();
        assert_eq!(stored, 0);

        // Nothing here is a red path: no approval was parked, no job started
        // (only the create that `settled_array` wrote stands) and the
        // privilege channel was never armed.
        assert!(store::list_approvals(&g.db, &g.org_id, true).unwrap().is_empty());
        assert_eq!(
            store::list_jobs(&g.db, 100)
                .unwrap()
                .iter()
                .filter(|job| job.kind != "elastic_create")
                .count(),
            0
        );
        assert_eq!(tentanas::elevation::audit_entries(&g.db), 0);

        // The policy an admin CAN always take back: a folder that already
        // carries a stored rule passes even while the union is unreadable, so
        // a pin can be lifted without waiting for a mount.
        //
        // Asserted on the array whose union is unreadable BY CONSTRUCTION, not
        // on `media`: both assertions below are about an unknown folder list,
        // and on a node that really has `/mnt/media` the list is known and
        // carries that array's actual folders — so on the owner's machine this
        // clause was checking the opposite of what it says.
        assert!(store::set_elastic_folder_policy(
            &g.db,
            &nowhere.array_id,
            "foto",
            tentanas::elastic::CachePolicy::Only,
        )
        .unwrap());
        let response = elastic_folder_cache_set(&fixture.ctx, "probe-unmounted-union", "foto", "yes")
            .await
            .unwrap();
        let MessageBody::TentaNasBody(P::ElasticArrayGetResponse { array }) = response else {
            panic!("the save answers with the array");
        };
        assert!(array.folders.is_empty(), "returning to the default drops the row");
        assert!(!array.folders_known, "an unreadable union stays unknown on the wire");
    }

    /// The schedule-set path: who may arm it, what it refuses, and the fact
    /// that saving the form is what makes `configured` true.
    #[tokio::test]
    async fn elastic_schedule_set_refuses_a_reader_an_unknown_cadence_and_half_the_mover_rules() {
        let mut fixture = dispatch_fixture();
        let hourly = NasSchedule {
            every: "1h".to_string(),
            hour: 0,
            minute: 0,
            weekday: 0,
            day: 1,
        };
        // Arming unattended privileged work is not a reader's decision.
        let denied = elastic_schedule_set(
            &fixture.ctx,
            store::ElasticTask::Mover,
            "media",
            true,
            &hourly,
            (None, None, None),
            Origin::Direct,
        )
        .await
        .unwrap_err();
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);
        // Authorisation is settled before the request's shape: a partial trio
        // from a reader is still `PolicyDenied`, never `BadRequest`.
        let denied_partial = elastic_schedule_set(
            &fixture.ctx,
            store::ElasticTask::Mover,
            "media",
            true,
            &hourly,
            (Some(1800), None, None),
            Origin::Direct,
        )
        .await
        .unwrap_err();
        assert_eq!(denied_partial.code, ProtocolErrorCode::PolicyDenied);

        elastic_admin(&mut fixture);
        let g = gate_destructive(&fixture.ctx).unwrap();
        let mut spec = tentanas::elastic::tests::create_spec("media");
        spec.owner = elastic_owner(&g);
        settled_array(&g, &spec).await;

        // A cadence this node can never fire is refused, not stored as a
        // promise nothing keeps.
        let unknown = NasSchedule {
            every: "yearly".to_string(),
            ..hourly.clone()
        };
        let refused = elastic_schedule_set(
            &fixture.ctx,
            store::ElasticTask::Sync,
            "media",
            true,
            &unknown,
            (None, None, None),
            Origin::Direct,
        )
        .await
        .unwrap_err();
        assert_eq!(refused.code, ProtocolErrorCode::BadRequest);
        assert!(
            store::elastic_schedule(&g.db, &spec.array_id, store::ElasticTask::Sync)
                .unwrap()
                .is_none(),
            "a refused cadence stores nothing"
        );

        // Half the trio could only be completed with defaults, which would
        // then be reported as somebody's decision.
        let partial = elastic_schedule_set(
            &fixture.ctx,
            store::ElasticTask::Mover,
            "media",
            true,
            &hourly,
            (Some(1800), None, None),
            Origin::Direct,
        )
        .await
        .unwrap_err();
        assert_eq!(partial.code, ProtocolErrorCode::BadRequest);
        assert!(
            store::mover_settings(&g.db, &spec.array_id).unwrap().is_none(),
            "a refused request writes no settings row"
        );

        // The whole form: the cadence and the rules, and `configured` flips —
        // which is exactly what stops n11 saying "nie skonfigurowano".
        let saved = elastic_schedule_set(
            &fixture.ctx,
            store::ElasticTask::Mover,
            "media",
            true,
            &hourly,
            (Some(1800), Some(35), Some(false)),
            Origin::Direct,
        )
        .await
        .unwrap();
        let MessageBody::TentaNasBody(P::ElasticArrayGetResponse { array }) = saved else {
            panic!("the save answers with the array");
        };
        assert!(array.mover.configured, "the rules are now a decision");
        assert_eq!(array.mover.min_age_secs, 1800);
        assert_eq!(array.mover.cache_min_free_pct, 35);
        assert!(!array.mover.coupled_sync);
        assert_eq!(array.mover.schedule, Some(hourly.clone()));
        assert!(array.mover.enabled);

        // The row toggle carries NO rules, and that must leave them standing.
        elastic_schedule_set(
            &fixture.ctx,
            store::ElasticTask::Mover,
            "media",
            false,
            &hourly,
            (None, None, None),
            Origin::Direct,
        )
        .await
        .unwrap();
        let settings = store::mover_settings(&g.db, &spec.array_id).unwrap();
        assert_eq!(
            settings,
            Some((1800, 35, false)),
            "a cadence-only request must not overwrite the rules"
        );
    }

    /// §5.10 applied to ARMING a cadence. A schedule buys exactly the
    /// privileged work the manual operations park for — deferred and
    /// repeating — so one admin alone must not be able to arm one, and the
    /// parked row has to say what will be armed.
    #[tokio::test]
    async fn arming_an_elastic_cadence_parks_and_records_what_it_arms() {
        let mut fixture = dispatch_fixture();
        elastic_admin(&mut fixture);
        let g = gate_destructive(&fixture.ctx).unwrap();
        let mut spec = tentanas::elastic::tests::create_spec("media");
        spec.owner = elastic_owner(&g);
        settled_array(&g, &spec).await;
        tentanas::approvals::set_settings(&actor(&fixture.ctx, &g).unwrap(), true, 24).unwrap();
        let hourly = NasSchedule {
            every: "1h".to_string(),
            hour: 0,
            minute: 30,
            weekday: 0,
            day: 1,
        };

        let response = elastic_schedule_set(
            &fixture.ctx,
            store::ElasticTask::Mover,
            "media",
            true,
            &hourly,
            (Some(1800), Some(35), Some(false)),
            Origin::Direct,
        )
        .await
        .unwrap();
        let MessageBody::TentaNasBody(P::ApprovalPendingResponse { approval }) = response else {
            panic!("arming a cadence must park");
        };
        assert_eq!(approval.operation, tentanas::approvals::OP_ELASTIC_SCHEDULE);
        assert_eq!(approval.subject, "media");
        // The approver must see the operation, the ARRAY, the cadence with its
        // offset, and the rules.
        assert!(approval.detail.contains("mover"), "{}", approval.detail);
        assert!(approval.detail.contains("media"), "{}", approval.detail);
        assert!(approval.detail.contains("hourly"), "{}", approval.detail);
        assert!(approval.detail.contains(":30"), "{}", approval.detail);
        assert!(approval.detail.contains("35%"), "{}", approval.detail);
        // The same, as a code the approver's screen words (wave 6): the
        // cadence travels as the schedule itself, so the screen formats it
        // the way the schedule editor does. Stored with the row.
        let stored = store::approval(&g.db, &approval.request_id).unwrap().unwrap();
        assert_eq!(stored.approval.detail_reasons, approval.detail_reasons);
        let [reason] = approval.detail_reasons.as_slice() else {
            panic!("one coded detail: {:?}", approval.detail_reasons)
        };
        assert_eq!(reason.code, "elastic_schedule");
        let param = |k: &str| reason.params.get(k).map(String::as_str);
        assert_eq!(
            (param("task"), param("enabled"), param("every"), param("minute")),
            (Some("mover"), Some("true"), Some("1h"), Some("30"))
        );
        assert_eq!(
            (param("min_age_secs"), param("cache_min_free_pct"), param("coupled_sync")),
            (Some("1800"), Some("35"), Some("false"))
        );

        // Nothing armed and no rule written until a second admin agrees.
        assert!(
            store::elastic_schedule(&g.db, &spec.array_id, store::ElasticTask::Mover)
                .unwrap()
                .is_none(),
            "a parked request arms nothing"
        );
        assert!(store::mover_settings(&g.db, &spec.array_id).unwrap().is_none());

        // WHAT WAS STORED is what will run. Building the replay payload by hand
        // would only prove the handler applies its own argument — a park that
        // dropped a rule, changed the cadence or stored the wrong variant would
        // sail through that. So the parked row is read back and IT is replayed.
        let parked_row = store::approval(&g.db, &approval.request_id)
            .unwrap()
            .unwrap();
        let parked = tentanas::approvals::stored_payload(&parked_row).unwrap();
        assert_eq!(
            parked,
            P::ElasticMoverScheduleSetRequest {
                name: "media".to_string(),
                enabled: true,
                schedule: hourly.clone(),
                min_age_secs: Some(1800),
                cache_min_free_pct: Some(35),
                coupled_sync: Some(false),
            },
            "the parked request must be the one that was asked for"
        );
        let replay = execute_approved(&fixture.ctx, &parked, None).await.unwrap();
        assert!(matches!(
            replay,
            MessageBody::TentaNasBody(P::ElasticArrayGetResponse { .. })
        ));
        assert_eq!(
            store::mover_settings(&g.db, &spec.array_id).unwrap(),
            Some((1800, 35, false)),
            "the approved request is the one that applies"
        );

        // Sync and scrub park too, each storing its OWN variant.
        for (task, cadence) in [
            (
                store::ElasticTask::Sync,
                NasSchedule {
                    every: "daily".to_string(),
                    hour: 3,
                    minute: 0,
                    weekday: 0,
                    day: 1,
                },
            ),
            (
                store::ElasticTask::Scrub,
                NasSchedule {
                    every: "weekly".to_string(),
                    hour: 4,
                    minute: 0,
                    weekday: 0,
                    day: 1,
                },
            ),
        ] {
            let parked_response = elastic_schedule_set(
                &fixture.ctx,
                task,
                "media",
                true,
                &cadence,
                (None, None, None),
                Origin::Direct,
            )
            .await
            .unwrap();
            let MessageBody::TentaNasBody(P::ApprovalPendingResponse { approval }) =
                parked_response
            else {
                panic!("{} must park", task.kind());
            };
            assert_eq!(approval.operation, tentanas::approvals::OP_ELASTIC_SCHEDULE);
            let row = store::approval(&g.db, &approval.request_id).unwrap().unwrap();
            let payload = tentanas::approvals::stored_payload(&row).unwrap();
            let expected = match task {
                store::ElasticTask::Sync => P::ElasticSyncScheduleSetRequest {
                    name: "media".to_string(),
                    enabled: true,
                    schedule: cadence.clone(),
                },
                _ => P::ElasticScrubScheduleSetRequest {
                    name: "media".to_string(),
                    enabled: true,
                    schedule: cadence.clone(),
                },
            };
            assert_eq!(payload, expected, "{} parks its own variant", task.kind());
            // A scrub row exists from creation: every array with parity gets the
            // monthly default in the same transaction as the array itself. What
            // parking must not do is arm anything BEYOND that default.
            let stored = store::elastic_schedule(&g.db, &spec.array_id, task).unwrap();
            let expected = (task == store::ElasticTask::Scrub).then(store::default_elastic_scrub_schedule);
            assert_eq!(stored.map(|row| row.schedule), expected, "{} armed nothing while parked", task.kind());
        }

        // `enabled: false` is NOT a free pass. n15's dialog sends the three
        // rules whatever the toggle says, so a disabled request carrying them
        // would otherwise persist "age 0, coupled sync off" and a 15-minute
        // cadence with nobody approving it — and the later toggle, which sends
        // no rules, would show the approver a bare cadence.
        let settings_before = store::mover_settings(&g.db, &spec.array_id).unwrap();
        let fast = NasSchedule {
            every: "15m".to_string(),
            hour: 0,
            minute: 0,
            weekday: 0,
            day: 1,
        };
        let with_rules = elastic_schedule_set(
            &fixture.ctx,
            store::ElasticTask::Mover,
            "media",
            false,
            &fast,
            (Some(0), Some(10), Some(false)),
            Origin::Direct,
        )
        .await
        .unwrap();
        assert!(
            matches!(
                with_rules,
                MessageBody::TentaNasBody(P::ApprovalPendingResponse { .. })
            ),
            "a disabled request carrying rules must not apply unapproved"
        );
        assert_eq!(
            store::mover_settings(&g.db, &spec.array_id).unwrap(),
            settings_before,
            "the rules must be exactly as they were"
        );

        // A disabled request that changes the CADENCE parks as well.
        let recadence = elastic_schedule_set(
            &fixture.ctx,
            store::ElasticTask::Mover,
            "media",
            false,
            &fast,
            (None, None, None),
            Origin::Direct,
        )
        .await
        .unwrap();
        assert!(matches!(
            recadence,
            MessageBody::TentaNasBody(P::ApprovalPendingResponse { .. })
        ));

        // …and the ONE request that escapes four eyes: standing the stored
        // cadence down, changing nothing else.
        let off = elastic_schedule_set(
            &fixture.ctx,
            store::ElasticTask::Mover,
            "media",
            false,
            &hourly,
            (None, None, None),
            Origin::Direct,
        )
        .await
        .unwrap();
        assert!(
            matches!(
                off,
                MessageBody::TentaNasBody(P::ElasticArrayGetResponse { .. })
            ),
            "a pure switch-off must not need a second admin"
        );
        assert!(
            !store::elastic_schedule(&g.db, &spec.array_id, store::ElasticTask::Mover)
                .unwrap()
                .unwrap()
                .enabled,
            "and it really switches the cadence off"
        );

        // Range checks run BEFORE the park. `park` is the only thing that
        // creates a row and it returns `Ok(ApprovalPendingResponse)`, so an
        // `Err` here is itself the proof that nothing was parked — which is
        // what stops an approval being burned on a request the replay refuses.
        for rules in [(Some(1800), Some(200), Some(true)), (Some(u64::MAX), Some(20), Some(true))] {
            let refused = elastic_schedule_set(
                &fixture.ctx,
                store::ElasticTask::Mover,
                "media",
                true,
                &hourly,
                rules,
                Origin::Direct,
            )
            .await
            .unwrap_err();
            assert_eq!(refused.code, ProtocolErrorCode::BadRequest, "{refused:?}");
        }
    }

    #[tokio::test]
    async fn elastic_mover_names_an_unresolved_operation_instead_of_failing_opaquely() {
        use tentanas_helper::elastic::{
            ElasticSnapraidKind as Kind, ElasticSnapraidOutcome as Outcome,
        };
        let mut fixture = dispatch_fixture();
        elastic_admin(&mut fixture);
        let g = gate_destructive(&fixture.ctx).unwrap();
        let mut spec = tentanas::elastic::tests::mover_spec("cached");
        spec.owner = elastic_owner(&g);
        settled_array(&g, &spec).await;

        // A nightly sync fails: array and operation both close needs_attention.
        let sync_id = uuid::Uuid::now_v7().to_string();
        let sync_job = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "elastic_sync".into(),
            subject: spec.name.clone(),
            status: "running".into(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(
            &g.db,
            &sync_job,
            Some(&tentanas::jobs::ElasticJobIntent::Snapraid {
                owner: spec.owner.clone(),
                array_id: spec.array_id.clone(),
                operation_id: sync_id.clone(),
                kind: Kind::Sync,
                acknowledge_parity_fault: None,
            }),
        )
        .unwrap();
        store::record_snapraid_result(
            &g.db,
            &spec.owner,
            &sync_id,
            &tentanas::elastic::tests::snapraid_result(&spec, &sync_id, Kind::Sync, Outcome::Failed),
        )
        .unwrap();
        store::finish_job(&g.db, &sync_job.job_id, "failed", Some("data_error")).unwrap();

        // The admin then runs Restore — which the detail view invites precisely
        // in `needs_attention` — and it succeeds, returning the array to
        // 'active' while the stale sync row stands. This is the state in which
        // the button was enabled and the refusal arrived as an internal error.
        g.db.write()
            .unwrap()
            .execute(
                "UPDATE nas_elastic_arrays SET state='active' WHERE array_id=?1",
                rusqlite::params![spec.array_id],
            )
            .unwrap();
        assert!(
            store::elastic_array(&g.db, &spec.owner, "cached")
                .unwrap()
                .unwrap()
                .unresolved_operation
        );

        let refused = elastic_mover(&fixture.ctx, "cached", None, Origin::Direct)
            .await
            .unwrap_err();
        assert_eq!(refused.code, ProtocolErrorCode::NotAvailable, "{refused:?}");
        assert!(
            refused.message.contains("niepotwierdzoną operację"),
            "odmowa musi nazwać powód: {}",
            refused.message
        );
        assert!(
            !store::list_jobs(&g.db, 100)
                .unwrap()
                .iter()
                .any(|j| j.kind == "elastic_mover"),
            "odmowa nie uruchamia zadania"
        );
    }

    #[tokio::test]
    async fn elastic_approval_parks_without_secret_job_or_helper_and_refuses_self() {
        let mut fixture=dispatch_fixture();
        elastic_admin(&mut fixture);
        let g=gate_destructive(&fixture.ctx).unwrap();
        tentanas::approvals::set_settings(&actor(&fixture.ctx,&g).unwrap(),true,24).unwrap();
        let (response,error)=crate::dispatch::dispatch(&tn(elastic_request()),&fixture.ctx).await;
        assert!(!error,"{response:?}");
        let MessageBody::TentaNasBody(P::ApprovalPendingResponse {approval})=response else { panic!("brak approval") };
        let stored=store::approval(&g.db,&approval.request_id).unwrap().unwrap();
        assert!(!stored.payload_json.contains("never-persist-this-password"));
        assert!(!stored.payload_json.contains("sudo_password"));
        assert_eq!(stored.approval.operation,tentanas::approvals::OP_ELASTIC_CREATE);
        assert!(store::list_jobs(&g.db,100).unwrap().is_empty());
        assert!(store::elastic_claims(&g.db).unwrap().is_empty());
        assert_eq!(tentanas::elevation::audit_entries(&g.db),0);
        assert!(matches!(tentanas::approvals::claim(&actor(&fixture.ctx,&g).unwrap(),&approval.request_id),
            Err(tentanas::approvals::ApprovalError::OwnRequest)));
    }

    fn elastic_preview_disk(ids: &[String]) -> Result<Vec<NasDisk>, ProtocolError> {
        assert_eq!(ids, &["preview-data".to_string()]);
        Ok(vec![NasDisk {
            disk_id: "preview-data".to_string(),
            name: "sdz".to_string(),
            path: "/dev/sdz".to_string(),
            serial: "preview-serial".to_string(),
            size_bytes: 1024 * 1024 * 1024,
            role: "free".to_string(),
            ..Default::default()
        }])
    }

    fn cache_preview_environment(ctx: &HandlerContext) {
        let g = gate(ctx, PERM_READ).expect("dostęp do testowej instancji");
        store::store_environment(
            &g.db,
            &serde_json::to_string(&tentaflow_protocol::tentanas::NasEnvironment::default())
                .unwrap(),
            &store::now(),
        )
        .expect("środowisko bez uruchamiania sond dysków");
    }

    /// A fresh install answers a NON-destructive Elastic read. Nothing about
    /// the request is wrong — the node simply has no privilege channel yet,
    /// and that is the one fact the operator can act on, so it has to arrive
    /// as `NotAvailable` naming the remedy rather than as `Internal` naming
    /// nothing. This is the shape the product actually shipped: `ok: true` on
    /// install, then `tentanas elastic namespace failed` on the first click.
    ///
    /// What this holds, precisely: `elastic_capabilities` is a real handler
    /// run end to end, and `root_claims` is the single function the plan
    /// dispatch arm, `elastic_create` and `arrays_claiming_disks` all reach
    /// `tentanas::elastic::claims` through — so the classification is pinned
    /// for all three. The previous version of this test handed
    /// `elastic_array_plan` a closure it wrote ITSELF, which would have passed
    /// with the production arm fully reverted; it proved nothing and is gone.
    #[tokio::test]
    async fn an_unconfigured_channel_refuses_elastic_reads_with_the_remedy_not_a_generic_fault() {
        let fixture = dispatch_fixture();
        cache_preview_environment(&fixture.ctx);
        {
            let g = gate(&fixture.ctx, PERM_READ).expect("dostęp do testowej instancji");
            assert_eq!(
                tentanas::elevation::mode(&g.db),
                tentanas::elevation::Mode::Unset,
                "świeża instalacja nie ma skonfigurowanego kanału"
            );
        }

        let capabilities = elastic_capabilities(&fixture.ctx)
            .await
            .expect_err("nieskonfigurowany kanał nie może zwrócić listy możliwości");
        let g = gate(&fixture.ctx, PERM_READ).expect("dostęp do testowej instancji");
        // The namespace read behind the plan — the request measured on the
        // server — and the root reservation `elastic_create` performs before
        // it formats a single disk. One function, so one refusal.
        let namespace = root_claims(&g, "elastic namespace", Some("media"), None)
            .await
            .expect_err("nieskonfigurowany kanał nie może potwierdzić przestrzeni nazw");
        let create = root_claims(&g, "elastic root claims", Some("media"), None)
            .await
            .expect_err("nieskonfigurowany kanał nie może potwierdzić rezerwacji roota");

        for (scope, error) in [
            ("capabilities", capabilities),
            ("namespace", namespace),
            ("create", create),
        ] {
            assert_eq!(error.code, ProtocolErrorCode::NotAvailable, "{scope}");
            assert!(
                error.message.contains("TentaNas")
                    && error.message.contains("konfiguracji kanału"),
                "{scope} musi nazwać lekarstwo, nie samą porażkę: {}",
                error.message
            );
            assert!(
                !error.message.contains("failed"),
                "{scope} nie może zwrócić ogólnego błędu wewnętrznego: {}",
                error.message
            );
            // The remedy must not send the operator to a tab that the setup
            // gate has replaced on exactly the nodes that emit this error.
            assert!(
                !error.message.contains("zakładkę Środowisko"),
                "{scope} nie może wskazywać zakładki, której w tym stanie nie ma: {}",
                error.message
            );
        }
    }

    #[tokio::test]
    async fn elastic_preview_refuses_failed_pool_reads_in_the_handler() {
        let fixture = dispatch_fixture();
        cache_preview_environment(&fixture.ctx);
        let failures = [
            (
                BrokerError::ToolMissing("zpool"),
                ProtocolErrorCode::NotAvailable,
                "zpool is not installed",
            ),
            (
                BrokerError::Timeout {
                    program: "zpool".to_string(),
                    secs: 15,
                },
                ProtocolErrorCode::Internal,
                "tentanas elastic pool names failed",
            ),
            (
                BrokerError::Exit {
                    program: "zpool".to_string(),
                    code: 1,
                    stderr: "cannot read pools".to_string(),
                },
                ProtocolErrorCode::Internal,
                "tentanas elastic pool names failed",
            ),
            (
                BrokerError::Io("read failed".to_string()),
                ProtocolErrorCode::Internal,
                "tentanas elastic pool names failed",
            ),
        ];
        for (failure, code, message) in failures {
            let error = elastic_array_plan(
                &fixture.ctx,
                "media",
                &["preview-data".to_string()],
                &[],
                &[],
                "ext4",
                elastic_preview_disk,
                async { Err(broker_error("elastic pool names",failure)) },
            )
            .await
            .expect_err("błąd ZFS nie może zwracać planu ani listy urządzeń do wymazania");
            assert_eq!(error.code, code);
            assert_eq!(error.message, message);
        }
    }

    #[tokio::test]
    async fn elastic_preview_accepts_empty_pool_read_and_refuses_a_name_collision() {
        let fixture = dispatch_fixture();
        cache_preview_environment(&fixture.ctx);
        let filesystem = tentanas_helper::elastic::FILESYSTEMS
            .iter()
            .copied()
            .find(|fs| has_mkfs(fs))
            .unwrap_or("ext4");
        for occupied in [false, true] {
            let claims = tentanas_helper::elastic::ElasticClaimsResult {
                name_claimed: Some(false), namespace_clear: Some(!occupied), disks: Vec::new(),
            };
            let answer = elastic_array_plan(
                &fixture.ctx,
                "media",
                &["preview-data".to_string()],
                &[],
                &[],
                filesystem,
                elastic_preview_disk,
                async { Ok(claims) },
            )
            .await
            .expect("poprawny odczyt ZFS pozwala ocenić plan");
            let MessageBody::TentaNasBody(P::ElasticArrayPlanResponse { plan }) = answer else {
                panic!("oczekiwano odpowiedzi preview, otrzymano {answer:?}");
            };
            if occupied {
                assert_eq!(plan.refusals.len(), 1);
                assert_eq!(plan.refusals[0].code, "name_taken");
                assert!(plan.steps_preview.is_empty());
                assert!(plan.wiped_devices.is_empty());
            } else {
                assert!(plan.refusals.is_empty(), "{:?}", plan.refusals);
                assert_eq!(plan.union_path, "/mnt/media");
                assert_eq!(plan.usable_bytes, 1024 * 1024 * 1024);
                assert_eq!(plan.parity_bytes, 0);
                assert_eq!(plan.cache_bytes, 0);
                assert_eq!(plan.wiped_devices.len(), 1);
                assert!(plan.steps_preview.contains("mkfs"));
                assert!(plan.steps_preview.contains("/mnt/media"));
            }
        }
    }

    #[tokio::test]
    async fn elastic_preview_checks_permission_before_inventory_or_pool_reads() {
        let fixture = dispatch_fixture();
        crate::dispatch::app_gate::test_support::set_permission(
            &fixture.ctx.state,
            &fixture.addon_id,
            "user",
            &fixture.ctx.org_context.as_ref().unwrap().user_id,
            PERM_READ,
            "deny",
        );
        let error = elastic_array_plan(
            &fixture.ctx,
            "media",
            &["preview-data".to_string()],
            &[],
            &[],
            "ext4",
            |_| panic!("brak uprawnień musi poprzedzać odczyt dysków"),
            async { panic!("brak uprawnień musi poprzedzać odczyt ZFS") },
        )
        .await
        .expect_err("brak nas.read musi odmówić");
        assert_eq!(error.code, ProtocolErrorCode::PolicyDenied);
        assert_eq!(error.message, "nas.read permission required");
    }

    /// 0 parity disks and no cache are legal arrays, so the list reader the
    /// Elastic Array plan uses for them must not borrow `disks_by_id`'s
    /// "no disks selected" refusal.
    ///
    /// This is the whole difference between the two readers, and it is worth
    /// a test because it is invisible: `disks_by_id(&[])` returns an error
    /// that would have surfaced in the wizard as a red box the moment an
    /// admin chose "no parity" — which §5.3 explicitly allows.
    #[test]
    fn an_empty_optional_disk_list_is_an_empty_list_and_not_a_refusal() {
        let none: Vec<String> = Vec::new();
        let read_disks = |ids: &[String]| disks_by_id(ids, false);
        assert!(
            optional_disks(&none, &read_disks).expect("no parity disks is a legal array").is_empty()
        );
        // The reader it delegates to says the opposite for the same input,
        // which is why the wrapper exists at all.
        assert!(
            disks_by_id(&none, false).is_err(),
            "if this ever starts succeeding, `optional_disks` has no reason to exist"
        );
        // A named disk that this node does not have is still an error — the
        // wrapper widens nothing except the empty case.
        let missing = vec!["no-such-disk".to_string()];
        assert!(optional_disks(&missing, &read_disks).is_err());
    }

    /// Every REQUEST variant of THIS family carries the family's authority.
    ///
    /// Resolution itself is checked fleet-wide in `dispatch/mod.rs`. What is
    /// left here is the part that is per-family and cannot be: with 82
    /// registrations, nine of them destructive, a single entry sitting quietly
    /// at a different `SessionAuthKind` is not a theoretical risk. The list is
    /// read from the protocol source rather than typed out here, so a variant
    /// appended there fails this test until it is registered too.
    #[test]
    fn every_request_variant_carries_the_familys_policy() {
        const PROTOCOL_SRC: &str = include_str!("../../../tentaflow-protocol/src/tentanas.rs");
        let body = PROTOCOL_SRC
            .split_once("pub enum TentaNasPayload {")
            .expect("TentaNasPayload enum")
            .1;
        let mut requests = Vec::new();
        for line in body.lines() {
            if line == "}" {
                break;
            }
            let Some(rest) = line.strip_prefix("    ") else {
                continue;
            };
            if rest.starts_with(' ') || !rest.starts_with(char::is_uppercase) {
                continue;
            }
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if name.ends_with("Request") {
                requests.push(format!("TentaNas{name}"));
            }
        }
        assert!(
            requests.len() >= 82,
            "the parser found {} request variants, which cannot be right",
            requests.len()
        );
        for variant in requests {
            let handler = crate::dispatch::find(&variant)
                .unwrap_or_else(|| panic!("{variant} has no registered handler"));
            // Every variant carries the family's `#[policy]` — one of the 82
            // cannot quietly sit at a different level.
            //
            // On its own this is a TAUTOLOGY and was one: 82 macro calls all
            // read the same const, so comparing the registry against that same
            // const compares a value with itself. `#[policy(Anonymous)]` would
            // have passed it with a green suite. The two assertions below are
            // the independent reference it lacked.
            assert_eq!(
                handler.required_auth,
                __tentaflow_policy_tentanas_dispatch,
                "{variant} must carry the family's `#[policy]`, not its own"
            );
            // THE SECURITY PROPERTY, measured rather than restated: whatever
            // the attribute says, an anonymous socket must not satisfy it.
            // This is what actually goes wrong if the attribute is loosened —
            // the dispatcher stops filtering and each handler's own `gate()`
            // becomes the only line left — and it fails on
            // `#[policy(Anonymous)]` no matter what any const says.
            assert!(
                !handler
                    .required_auth
                    .session_satisfies(&tentaflow_protocol::SessionAuth::Anonymous),
                "{variant} would admit an anonymous socket at the dispatcher"
            );
        }

        // …and the DECISION itself, read from the attribute's own text rather
        // than from the const it expands to. This is the line that records
        // what this family requires; nothing else in the repository did, so
        // loosening it was a one-token change nobody would have seen.
        //
        // Every handler here calls `gate(ctx, PERM_*)`, which needs a resolved
        // user and a role, so anything weaker than a user session is a
        // dispatcher that admits frames only the handler can then refuse.
        const OWN_SRC: &str = include_str!("tentanas.rs");
        let policy_line = OWN_SRC
            .lines()
            .find(|l| l.trim_start().starts_with("#[policy("))
            .expect("the family's policy attribute");
        assert_eq!(
            policy_line.trim(),
            "#[policy(UserSession)]",
            "the TentaNas family's authority changed — if that is deliberate, change this line \
             too, and say why in the commit"
        );
    }

    // `every_request_name_this_node_can_utter_resolves_to_a_handler` used to live
    // here. It scans `variant_name_of` for EVERY family, so it now lives beside
    // that function, in `dispatch/mod.rs::wire_name_guard`, together with two
    // assertions it did not have: that a request variant is NAMED like one (a
    // name without the suffix slipped past its own filter) and that no two arms
    // answer to one name. Moved by agreement between the two sessions working in
    // this tree; what stays here is what is about THIS family.

    /// A pool imported under its own name is titled by that NAME in the job
    /// list, resolved from the scan by the GUID the request carries — never
    /// by the GUID itself, and never by a nameless row.
    #[test]
    fn a_kept_name_import_is_titled_from_the_last_scan_and_never_by_its_guid() {
        use tentaflow_protocol::tentanas::NasImportablePool;
        // A per-test organisation: the cache is process-wide.
        let org = "org-import-title-test";
        let t0 = std::time::Instant::now();
        let pools = vec![
            NasImportablePool { name: "tank".into(), guid: "1111".into(), ..Default::default() },
            NasImportablePool { name: "tank".into(), guid: "2222".into(), ..Default::default() },
            NasImportablePool { name: String::new(), guid: "3333".into(), ..Default::default() },
        ];
        // No scan yet: the kind alone, not the GUID.
        assert_eq!(import_job_subject(org, "2222", "", t0), "");
        remember_import_scan(org, &pools, t0);
        assert_eq!(import_job_subject(org, "2222", "", t0), "tank");
        assert_eq!(import_job_subject(org, "3333", "", t0), "", "no name, no title");
        assert_eq!(import_job_subject(org, "9999", "", t0), "", "not in the scan");
        // A new name is the title whatever the cache says.
        assert_eq!(import_job_subject(org, "2222", "archiwum", t0), "archiwum");
        // Another organisation's scan does not title this one's jobs.
        assert_eq!(import_job_subject("org-import-title-other", "2222", "", t0), "");
        // Stale: past the TTL the name no longer titles anything.
        let later = t0 + IMPORT_SCAN_NAMES_TTL + Duration::from_secs(1);
        assert_eq!(import_job_subject(org, "2222", "", later), "");
        // A newer scan REPLACES the old answer: a pool it no longer lists
        // stops titling jobs.
        remember_import_scan(org, &pools[..1], t0);
        assert_eq!(import_job_subject(org, "2222", "", t0), "");
        assert_eq!(import_job_subject(org, "1111", "", t0), "tank");
    }

    #[test]
    fn the_import_request_answers_without_a_second_privileged_scan() {
        // The arm of `PoolImportRequest` must not call `importable_pools`: a
        // second scan of up to 120 s inside the client's 120 s timeout is what
        // showed the admin a failure while the node imported anyway.
        let src = include_str!("tentanas.rs");
        let arm = src
            .split("P::PoolImportRequest {")
            .nth(1)
            .and_then(|rest| rest.split("P::PoolAddVdevRequest {").next())
            .expect("the PoolImportRequest arm");
        assert!(!arm.contains("importable_pools("), "{arm}");
        assert!(arm.contains("import_job_subject("), "{arm}");
    }

    #[test]
    fn only_an_owner_of_the_asking_org_is_named_and_another_tenant_is_never_looked_up() {
        use crate::tentanas::elastic::{OWNER_KIND_OTHER_INSTALLATION, OWNER_KIND_THIS_INSTANCE, OWNER_KIND_THIS_ORG};
        assert_eq!(own_org_instance_name(OWNER_KIND_THIS_ORG, || Some(" NAS biuro ".into())), "NAS biuro");
        assert_eq!(own_org_instance_name(OWNER_KIND_THIS_ORG, || None), "");
        // The lookup itself must not run for another tenant — not merely have
        // its answer dropped.
        for kind in [OWNER_KIND_OTHER_INSTALLATION, OWNER_KIND_THIS_INSTANCE, "", "garbage"] {
            let name = own_org_instance_name(kind, || panic!("{kind}: another tenant's name was looked up"));
            assert_eq!(name, "", "{kind}");
        }
    }

    #[test]
    fn a_smart_job_on_a_disk_gone_from_the_inventory_is_titled_by_its_last_known_name() {
        use tentanas::disks::ShownDiskName;
        let id = "wwn-5000cca27dc7a4c6";
        // A live name is shown as it is; a remembered one travels flagged, so
        // the screen can mark it as last-known (the one naming rule).
        assert_eq!(smart_subject_name(id, |_| ShownDiskName::Live("sdg".into())), Some(("sdg".to_string(), false)));
        assert_eq!(smart_subject_name(id, |_| ShownDiskName::LastKnown("sdq".into())), Some(("sdq".to_string(), true)));
        assert_eq!(smart_subject_name(id, |_| ShownDiskName::Unknown), None);
        // Through the real rule and a real database: the disk is recorded,
        // then gone from the live map.
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        tentanas::db::migrate(&conn).expect("migrate");
        let db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        let gone = "wwn-smart-subject-left-the-inventory-5000cca2";
        tentanas::db::upsert_disk_seen(
            &db,
            &tentanas::db::DiskIdentity {
                disk_id: gone,
                name: "sdq",
                model: "HGST",
                serial: "S9",
                wwn: None,
                size_bytes: 1,
                kind: "hdd",
            },
        )
        .expect("record the disk");
        assert!(tentanas::disks::disk_name(gone).is_none());
        let named = smart_subject_name(gone, |id| tentanas::disks::shown_disk_name(&db, id, None));
        assert_eq!(named, Some(("sdq".to_string(), true)), "gone, so the name is flagged as last-known");
    }

    /// Wave 9b: the lines of a multi-disk SMART job reach the screen named
    /// by the one disk-naming rule — the live kernel name; a disk that left
    /// the inventory by the name it was last seen under, flagged; and one the
    /// node never recorded by the name stored when the job started, flagged
    /// too. The stored disk ids never leave the node.
    #[test]
    fn a_batch_job_lines_are_named_and_never_carry_an_id() {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        tentanas::db::migrate(&conn).expect("migrate");
        let db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        let live = "wwn-wave9b-lines-live-5000cca2";
        let gone = "wwn-wave9b-lines-gone-5000cca3";
        let never = "wwn-wave9b-lines-never-5000cca4";
        tentanas::disks::insert_live_for_test(tentaflow_protocol::tentanas::NasDisk {
            disk_id: live.into(),
            name: "sdlv".into(),
            path: "/dev/sdlv".into(),
            ..Default::default()
        });
        tentanas::db::upsert_disk_seen(
            &db,
            &tentanas::db::DiskIdentity { disk_id: gone, name: "sdq", model: "HGST", serial: "S9", wwn: None, size_bytes: 1, kind: "hdd" },
        )
        .expect("record the disk");
        let job = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: tentanas::db::SMART_BATCH_KIND.into(),
            subject: "short|sdlv, sdq, sdx".into(),
            status: "running".into(),
            started_by: "test".into(),
            started_at: store::now(),
            ..Default::default()
        };
        tentanas::db::insert_job_full(
            &db,
            &job,
            None,
            None,
            &[(live.into(), "sdlv".into()), (gone.into(), "sdq".into()), (never.into(), "sdx".into())],
        )
        .expect("batch");
        let lines = job_disk_lines(&db, &job.job_id);
        let shown: Vec<(&str, bool, &str)> = lines.iter().map(|d| (d.name.as_str(), d.last_known, d.state.as_str())).collect();
        assert_eq!(shown, vec![("sdlv", false, "pending"), ("sdq", true, "pending"), ("sdx", true, "pending")]);
        let wire = serde_json::to_string(&lines).expect("json");
        assert!(!wire.contains("wwn-"), "no disk id in the lines: {wire}");
        tentanas::disks::remove_live_for_test(live);
    }

    #[test]
    fn a_wipe_plan_for_a_vanished_disk_names_it_and_never_by_its_id() {
        use tentanas::disks::ShownDiskName;
        let last = disk_gone_message(ShownDiskName::LastKnown("sdq".into()));
        assert!(last.contains("last seen as sdq"), "{last}");
        let unknown = disk_gone_message(ShownDiskName::Unknown);
        assert!(!unknown.contains("wwn-") && !unknown.contains("sn-"), "{unknown}");
    }

    #[test]
    fn a_target_is_refused_when_the_hostname_leaves_no_iqn_host_segment() {
        // `iqn.2026-09.local.tentaflow:.vm-store` is what these used to produce.
        for hostname in ["", "   ", "___", "..."] {
            let err = target_host_for_create(hostname).expect_err(hostname);
            assert!(err.message.contains("hostname"), "{hostname:?}: {}", err.message);
        }
        // A real one passes through unchanged: `wwn_for` sanitises it itself.
        assert_eq!(target_host_for_create("Helios_02.lan").unwrap(), "Helios_02.lan");
    }

    // ----- tenants ------------------------------------------------------------------

    /// An Elastic Array of `org`, written through the real create path, with
    /// its create job; answers the job id.
    fn tenant_array(g: &Gate, org: &str, name: &str) -> String {
        let mut spec = tentanas::elastic::tests::create_spec(name);
        spec.owner = ElasticOwner { org_id: org.into(), addon_id: g.addon_id.clone() };
        let job = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "elastic_create".into(),
            subject: name.into(),
            status: "running".into(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(&g.db, &job, Some(&tentanas::jobs::ElasticJobIntent::Create(spec))).unwrap();
        job.job_id
    }

    /// The node's ONE TentaNas database holds every tenant's array work (the
    /// package is a singleton and `app_db` keys the pool by instance id). The
    /// job list, the job modal, the alert list and acknowledging answer for
    /// the caller's organisation and for the shared hardware only — another
    /// tenant's job or alert is "not found", like an id that never existed.
    #[tokio::test]
    async fn the_job_and_alert_handlers_never_hand_a_tenant_another_organisations_array_work() {
        let fixture = dispatch_fixture();
        let g = gate(&fixture.ctx, PERM_READ).unwrap();
        let mine = tenant_array(&g, &g.org_id, "media");
        let theirs = tenant_array(&g, "org-other-tenant", "ksiegowosc");
        store::raise_alert(&g.db, "k-mine", "warning", "elastic-array", "media", "Macierz media", "").unwrap();
        store::raise_alert(&g.db, "k-theirs", "warning", "elastic-array", "ksiegowosc", "Macierz ksiegowosc", "")
            .unwrap();
        store::raise_alert(&g.db, "k-disk", "warning", "disk", "d1", "Disk sda: hot", "").unwrap();

        let Ok(MessageBody::TentaNasBody(P::JobsListResponse { jobs })) = jobs_list(&fixture.ctx, 0) else {
            panic!("jobs list");
        };
        assert_eq!(jobs.iter().map(|j| j.subject.as_str()).collect::<Vec<_>>(), vec!["media"]);
        assert!(job_get(&fixture.ctx, &mine).is_ok());
        let refused = job_get(&fixture.ctx, &theirs).expect_err("another tenant's job");
        assert_eq!(refused.code, ProtocolErrorCode::NotFound);

        let Ok(MessageBody::TentaNasBody(P::AlertsListResponse { alerts })) = alerts_list(&fixture.ctx, true) else {
            panic!("alerts list");
        };
        let subjects: std::collections::BTreeSet<_> = alerts.iter().map(|a| a.subject_id.as_str()).collect();
        assert_eq!(subjects, ["d1", "media"].into());
        let their_alert = store::list_alerts(&g.db, true).unwrap()
            .into_iter().find(|a| a.subject_id == "ksiegowosc").unwrap().alert_id;
        let refused = alert_ack(&fixture.ctx, &their_alert).expect_err("another tenant's alert");
        assert_eq!(refused.code, ProtocolErrorCode::NotFound);
        assert!(store::list_alerts(&g.db, false).unwrap().iter().any(|a| a.alert_id == their_alert),
            "and it stays unacknowledged for its owner");
    }

    /// Owner decision (wave 5): the jobs and alerts whose owner is gone (a
    /// dissolved array's, `org_id = ''`) are the node's SOLE organisation's to
    /// see. Through the real handlers, so a call site that went back to the
    /// bare org id (`From<&str>`, "not sole") fails here: the job list, the job
    /// modal, the alert list and the FleetView badge must all show the orphan
    /// on a one-organisation node — the badge counting exactly what the list
    /// shows — and all hide it once a second organisation exists.
    fn db_status(ctx: &HandlerContext, org_id: &str, status: &str) {
        ctx.state.db.write().unwrap().execute(
            "UPDATE organizations SET status = ?2 WHERE org_id = ?1",
            rusqlite::params![org_id, status],
        ).unwrap();
    }

    #[tokio::test]
    async fn the_handlers_show_a_dissolved_arrays_rows_only_to_the_sole_organisation() {
        let mut fixture = dispatch_fixture();
        // The test database seeds `org-default` and nothing else.
        fixture.ctx.org_context.as_mut().unwrap().org_id = "org-default".into();
        let g = gate(&fixture.ctx, PERM_READ).unwrap();
        let orphan = tentaflow_protocol::tentanas::NasJob {
            job_id: uuid::Uuid::now_v7().to_string(),
            kind: "elastic_sync".into(),
            subject: "rozwiazana".into(),
            status: "failed".into(),
            started_at: store::now(),
            ..Default::default()
        };
        store::insert_job(&g.db, &orphan, None).unwrap();
        store::raise_alert(&g.db, "k-orphan", "warning", "elastic-array", "rozwiazana", "Macierz", "").unwrap();
        {
            let conn = g.db.write().unwrap();
            conn.execute("UPDATE nas_jobs SET org_id = '' WHERE job_id = ?1", rusqlite::params![orphan.job_id]).unwrap();
            conn.execute("UPDATE nas_alerts SET org_id = '' WHERE subject_id = 'rozwiazana'", []).unwrap();
        }
        let seen = |ctx: &HandlerContext| {
            let Ok(MessageBody::TentaNasBody(P::JobsListResponse { jobs })) = jobs_list(ctx, 0) else { panic!("jobs") };
            let Ok(MessageBody::TentaNasBody(P::AlertsListResponse { alerts })) = alerts_list(ctx, false) else { panic!("alerts") };
            (
                jobs.iter().any(|j| j.job_id == orphan.job_id),
                job_get(ctx, &orphan.job_id).is_ok(),
                alerts.iter().filter(|a| a.subject_id == "rozwiazana").count(),
                alerts.len() as u32,
            )
        };
        let body = MessageBody::TentaNasBody(P::NodesListRequest {});
        let local_badge = |answer: MessageBody| {
            let MessageBody::TentaNasBody(P::NodesListResponse { nodes, .. }) = answer else { panic!("nodes: {answer:?}") };
            nodes.iter().find(|n| n.is_local).map(|n| n.alerts_active).unwrap()
        };

        // Owner decision 2026-09-26: "sole" means no OTHER active
        // organisation has resources on this node; the viewer need own none.
        let share = |org: &str, name: &str| {
            g.db.write().unwrap().execute(
                "INSERT INTO nas_shares (share_id, name, protocol, source_path, created_at, updated_at, org_id) \
                 VALUES (?1, ?1, 'smb', '/tank/x', 'now', 'now', ?2)",
                rusqlite::params![name, org],
            ).unwrap();
        };
        // The viewer owns nothing here, and no other active organisation
        // does: its dissolved array's rows are its to see.
        let (in_list, in_modal, orphan_alerts, _) = seen(&fixture.ctx);
        assert!(in_list && in_modal && orphan_alerts == 1, "a viewer without resources, alone on the node, sees them");
        share("org-default", "projekty");

        let (in_list, in_modal, orphan_alerts, listed) = seen(&fixture.ctx);
        assert!(in_list && in_modal, "the sole organisation sees its dissolved array's job");
        assert_eq!(orphan_alerts, 1, "and its alert");
        let (answer, _) = crate::dispatch::dispatch(&body, &fixture.ctx).await;
        assert_eq!(local_badge(answer), listed, "the FleetView badge counts what the list shows");

        // A second organisation that owns nothing here does not count.
        let other = crate::services::org::create_organization(&fixture.ctx.state.db, "Tenant B", "tenant-b", None, None, None, None).unwrap();
        let (in_list, in_modal, orphan_alerts, _) = seen(&fixture.ctx);
        assert!(in_list && in_modal && orphan_alerts == 1, "an organisation without resources here is nobody to leak to");

        // It owns a target here now: nobody sees the orphans.
        g.db.write().unwrap().execute(
            "INSERT INTO nas_targets (target_id, name, protocol, wwn, created_at, updated_at, org_id) \
             VALUES ('t-b', 'vm-b', 'iscsi', 'iqn.2026-09.test:vm-b', 'now', 'now', ?1)",
            rusqlite::params![other.org_id],
        ).unwrap();
        let (in_list, in_modal, orphan_alerts, listed) = seen(&fixture.ctx);
        assert!(!in_list && !in_modal, "two organisations with resources: nobody sees it");
        assert_eq!(orphan_alerts, 0);
        let (answer, _) = crate::dispatch::dispatch(&body, &fixture.ctx).await;
        assert_eq!(local_badge(answer), listed, "and the badge still agrees with the list");

        // Suspended: still present (reversible), so its resources still count.
        db_status(&fixture.ctx, &other.org_id, "suspended");
        let (in_list, in_modal, orphan_alerts, _) = seen(&fixture.ctx);
        assert!(!in_list && !in_modal && orphan_alerts == 0, "a suspended organisation still counts");
        db_status(&fixture.ctx, &other.org_id, "active");

        // Soft-deleted, its rows kept under retention: it counts no more.
        assert!(crate::services::org::delete_organization(&fixture.ctx.state.db, &other.org_id).unwrap());
        let (in_list, in_modal, orphan_alerts, listed) = seen(&fixture.ctx);
        assert!(in_list && in_modal && orphan_alerts == 1, "a soft-deleted organisation does not count");
        let (answer, _) = crate::dispatch::dispatch(&body, &fixture.ctx).await;
        assert_eq!(local_badge(answer), listed);

        // A read of the owners that fails hides the rows again.
        g.db.write().unwrap().execute_batch("ALTER TABLE nas_targets RENAME TO nas_targets_gone").unwrap();
        let (in_list, in_modal, orphan_alerts, _) = seen(&fixture.ctx);
        assert!(!in_list && !in_modal && orphan_alerts == 0, "a failed read never leaks");
        g.db.write().unwrap().execute_batch("ALTER TABLE nas_targets_gone RENAME TO nas_targets").unwrap();
    }

    /// A job another tenant's user started on SHARED hardware stays on the
    /// list, but its author reaches this tenant as nobody: the account is
    /// that tenant's, and so are its name AND its id — as is the id of an
    /// account that no longer exists. A member of the asking organisation is
    /// named as before, its org admin included (a membership row with the
    /// admin role, the only way an admin belongs to an org), one author on
    /// many jobs is named on every one of them, and every system author
    /// (`scheduler`, `startup`) is untouched.
    #[tokio::test]
    async fn a_job_author_of_another_organisation_is_not_named_to_this_one() {
        let fixture = dispatch_fixture();
        let g = gate(&fixture.ctx, PERM_READ).unwrap();
        let db = &fixture.ctx.state.db;
        for (id, name) in [("u-member", "Anna"), ("u-stranger", "Obca Osoba"), ("u-admin", "Szefowa")] {
            db.write().unwrap().execute(
                "INSERT INTO user_accounts (id, username, display_name, password_hash, is_active, must_change_password, role) \
                 VALUES (?1, ?1, ?2, 'test', 1, 0, 'user')",
                rusqlite::params![id, name],
            ).unwrap();
        }
        let org = crate::services::org::create_organization(db, "Tenant", "tenant-x", None, None, None, None).unwrap();
        let other = crate::services::org::create_organization(db, "Other", "tenant-y", None, None, None, None).unwrap();
        let role = "role-nas-tenant-test";
        db.write().unwrap().execute(
            "INSERT OR IGNORE INTO roles (role_id, name, permissions_json, created_at) \
             VALUES (?1, ?1, '[]', 'now')",
            rusqlite::params![role],
        ).unwrap();
        let admin_role = crate::services::org::repo::list_roles(db).unwrap()
            .into_iter().find(|r| r.name == "org_admin").expect("the seeded org admin role").role_id;
        for (org_id, user, role) in [
            (&org.org_id, "u-member", role),
            (&other.org_id, "u-stranger", role),
            (&org.org_id, "u-admin", admin_role.as_str()),
        ] {
            db.write().unwrap().execute(
                "INSERT INTO org_memberships (org_id, user_id, role_id, granted_at, granted_by) \
                 VALUES (?1, ?2, ?3, 'now', 'test')",
                rusqlite::params![org_id, user, role],
            ).unwrap();
        }
        let mut ctx = fixture.ctx.clone();
        ctx.org_context.as_mut().unwrap().org_id = org.org_id.clone();
        let started_by = [
            "u-member", "u-stranger", "scheduler", "startup", "u-deleted", "u-admin", "u-member", "u-member",
        ];
        let mut jobs: Vec<tentaflow_protocol::tentanas::NasJob> = started_by
            .into_iter()
            .map(|by| tentaflow_protocol::tentanas::NasJob {
                kind: "pool_scrub".into(), subject: "tank".into(), started_by: by.into(), ..Default::default()
            })
            .collect();
        name_jobs(&ctx, Some(&g.db), &mut jobs);
        let authors: Vec<&str> = jobs.iter().map(|j| j.started_by.as_str()).collect();
        // Both system authors — the scheduler's unattended runs and a
        // boot-time Elastic Restore (elastic::STARTED_BY_STARTUP) — pass
        // through untouched, exactly like a real "start węzła" row on rig11.
        assert_eq!(authors, vec!["Anna", "", "scheduler", "startup", "", "Szefowa", "Anna", "Anna"]);

        // The lookup itself: distinct ids in, members of the asking org out.
        let ids: std::collections::BTreeSet<String> =
            ["u-member", "u-stranger", "u-deleted", "u-admin"].map(String::from).into();
        assert_eq!(org_members_among(&ctx, &org.org_id, &ids),
            ["u-admin", "u-member"].map(String::from).into());
        assert_eq!(org_members_among(&ctx, &other.org_id, &ids), ["u-stranger"].map(String::from).into());
        assert!(org_members_among(&ctx, "", &ids).is_empty(), "no org, no members");
    }

    /// The Disks tab and the wipe plan: a disk of another tenant's LIVE array
    /// leaves the server as "another organisation's array", and the caller's
    /// own array names are what decides it.
    #[test]
    fn own_array_names_are_the_callers_organisations_only() {
        let fixture = dispatch_fixture();
        let g = gate(&fixture.ctx, PERM_READ).unwrap();
        tenant_array(&g, &g.org_id, "media");
        tenant_array(&g, "org-other-tenant", "ksiegowosc");
        assert_eq!(own_array_names(&g).unwrap(), ["media".to_string()].into());
        // As the inventory holds it: named, and mounted at its branch, whose
        // path spells the array.
        let branch = |array: &str| NasDisk {
            disk_id: "wwn-0x5000c500a1b2c3d4".into(), name: "sdg".into(), path: "/dev/sdg".into(),
            role: "array_member".into(), member_of: Some(array.into()), array_role: "data".into(),
            mountpoints: vec![format!("/mnt/tentanas-branches/{array}/data/d1")],
            fs_uuid: Some("33333333-3333-4333-8333-333333333333".into()),
            ..Default::default()
        };
        let mut theirs = branch("ksiegowosc");
        tentanas::disks::hide_other_org_array(&mut theirs, &own_array_names(&g).unwrap());
        assert_eq!(theirs.role, tentanas::disks::ROLE_OTHER_ORG_ARRAY);
        // The whole disk as it goes out, not one field of it.
        let wire = serde_json::to_string(&theirs).unwrap();
        assert!(!wire.contains("ksiegowosc"), "{wire}");
        assert!(!wire.contains("tentanas-branches"), "{wire}");
        // The caller's own array is not touched.
        let mut mine = branch("media");
        tentanas::disks::hide_other_org_array(&mut mine, &own_array_names(&g).unwrap());
        assert_eq!(mine, branch("media"));
    }
}
