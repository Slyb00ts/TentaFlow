// =============================================================================
// File: dispatch/tentaquant.rs — the TentaQuant request family (plan §11.1).
//
//       TentaQuant is the first MULTI-instance native app: one instance is one
//       laboratory. So every request but `LabListRequest` carries the
//       `instance_id` it means, and the very first thing each handler does is
//       `require_app_instance_permission` — the gate proves that id is an
//       ENABLED instance of THIS package and then evaluates that instance's
//       permission matrix. Resolving the instance by package would pick an
//       arbitrary lab, which is why nothing here calls the singleton gate.
//
//       Two independent layers of access, and both always apply:
//         * membership of the lab — the instance matrix (Addons) INTERSECTED
//           with the instance's Visibility. `quant.read`/`quant.run` are
//           `default = "allow"` (§10.2), so the matrix alone admits the whole
//           organization; Visibility is what scopes an instance to the group it
//           was created for, and this family enforces it on every request
//           rather than only hiding a tile;
//         * project ownership (ML Studio model, §18 decision 15) decides who
//           sees one project. A supervisor (`quant.instruct`) sees run metadata
//           and course progress, NEVER the content of someone else's private
//           project — there is no bypass in `db::access` and none may be added.
//
//       A caller who is not in a project gets the same `NotFound` as for a
//       project that does not exist: the wire must not reveal that a private
//       project exists.
// =============================================================================

use std::path::PathBuf;

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::tentaquant::{
    FileInfo, LabAdminSettings, LabInfo, LabNodeInfo, LabSettings, NotebookInfo,
    NotebookVersionInfo, ProjectInfo, ProjectShareInfo, TentaQuantPayload as P,
    PEOPLE_CANDIDATES_LIMIT_MAX,
};
use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode};

use super::{HandlerContext, SessionAuthKind};
use crate::addon::native_apps::NODE_STATUS_KEY_PREFIX;
use crate::db::DbPool;
use crate::tentaquant::{
    cas, db as store,
    people::{self, PERM_ADMIN, PERM_INSTRUCT, PERM_READ, PERM_RUN},
    PACKAGE_ID,
};

fn tq(body: P) -> MessageBody {
    MessageBody::TentaQuantBody(body)
}

fn internal(scope: &str, error: impl std::fmt::Display) -> ProtocolError {
    tracing::warn!(scope, error = %error, "tentaquant error");
    ProtocolError::internal(format!("tentaquant {scope} failed"))
}

/// The one refusal a caller outside a project may ever see. Identical for
/// "no such project" and "not yours", so the wire leaks no existence.
fn not_found() -> ProtocolError {
    ProtocolError::new(ProtocolErrorCode::NotFound, "project not found")
}

/// The lab a request named, after the matrix admitted the caller.
struct Lab {
    instance_id: String,
    org_id: String,
    user_id: String,
    db: DbPool,
    data_dir: PathBuf,
}

/// Gate + database + directory of one lab, for one permission. Three conditions
/// and all must hold: the platform's instance gate (an enabled instance of THIS
/// package whose matrix grants `quant.read`), this app's membership rule (that
/// matrix intersected with the instance's Visibility) and `permission` itself.
///
/// They are asked in that order, and they answer differently on purpose.
/// Whether the caller is IN this laboratory at all is an existence question: a
/// missing instance, a disabled one, one of another package, a matrix that
/// withholds `quant.read` and a Visibility that does not reach the caller all
/// come out as `app_gate::unavailable`, which is ONE indistinguishable answer
/// for a non-admin — a caller who can tell those apart has learned that a lab
/// it may not see exists. An admin keeps the gate's own diagnostic reason,
/// because `unavailable` already decides that per session and the two classes
/// only an admin can reach (`quant.read` withheld, Visibility miss) are
/// unreachable for one: the checker and `LabVisibility::admits` both bypass for
/// admins. What a MEMBER may then do is not a secret — `permission` is refused
/// with an honest `PolicyDenied` naming it, the way every other permission
/// decision in the dashboard reads. Only a genuine server fault travels as
/// itself: an internal error does not depend on which instance was named, so it
/// reveals nothing.
fn lab(ctx: &HandlerContext, instance_id: &str, permission: &str) -> Result<Lab, ProtocolError> {
    let org = ctx.org_context.as_ref().ok_or_else(|| {
        ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required")
    })?;
    if let Err(error) =
        super::app_gate::require_app_instance_permission(ctx, PACKAGE_ID, instance_id, PERM_READ)
    {
        return Err(match error.code {
            // Already the uniform refusal, and it carries the admin-only reason
            // the gate chose — forwarding it keeps that diagnostic alive.
            ProtocolErrorCode::AppUnavailable
            | ProtocolErrorCode::Internal
            | ProtocolErrorCode::AuthRequired => error,
            // A withheld `quant.read` would answer `PolicyDenied`, which proves
            // the instance exists and belongs to this package.
            _ => super::app_gate::unavailable(ctx, PACKAGE_ID, "not available to this user"),
        });
    }
    // Membership is the intersection (§10.2): a caller the instance's
    // Visibility does not show it to is not in this laboratory, whatever the
    // matrix defaults say. The refusal is the gate's own uniform one, so the
    // existence of a lab stays as private as its content.
    if !people::is_member(&ctx.state.db, checker(ctx)?, instance_id, &org.user_id) {
        return Err(super::app_gate::unavailable(
            ctx,
            PACKAGE_ID,
            "not visible to this user",
        ));
    }
    // The instance is proven enabled and ours by the call above, so the wider
    // permission is one cache read rather than a second lookup.
    if permission != PERM_READ && !holds(ctx, instance_id, permission) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            format!("{permission} permission required"),
        ));
    }
    let db = crate::tentaquant::open_db(&ctx.state.db, &org.org_id, instance_id)
        .map_err(|e| internal("database", e))?;
    let data_dir = crate::tentaquant::data_dir(&org.org_id, instance_id)
        .map_err(|e| internal("data directory", e))?;
    Ok(Lab {
        instance_id: instance_id.to_string(),
        org_id: org.org_id.clone(),
        user_id: org.user_id.clone(),
        db,
        data_dir,
    })
}

fn checker(
    ctx: &HandlerContext,
) -> Result<&crate::addon::permissions::PermissionChecker, ProtocolError> {
    ctx.state.permission_checker.as_deref().ok_or_else(|| {
        // Fail closed: without the checker nothing can be granted.
        tracing::error!("tentaquant: permission checker not wired");
        ProtocolError::internal("permission checker unavailable")
    })
}

/// Whether the caller holds one permission in this lab, without turning a
/// missing grant into an error — used where a permission WIDENS what a handler
/// does instead of gating it. Only the matrix question: [`lab`] establishes
/// membership and the instance itself before this is ever asked.
fn holds(ctx: &HandlerContext, instance_id: &str, permission: &str) -> bool {
    let (Some(org), Some(checker)) = (
        ctx.org_context.as_ref(),
        ctx.state.permission_checker.as_ref(),
    ) else {
        return false;
    };
    checker
        .check(instance_id, &org.user_id, permission, None)
        .is_granted()
}

// =============================================================================
// Lab
// =============================================================================

/// Nodes of the fleet with this instance's reconcile status, exactly the way
/// TentaNas reads its own: the status rows travel with the instance's config
/// partition, so the node answering the request knows them all without a round
/// trip.
fn lab_nodes(ctx: &HandlerContext, instance_id: &str) -> Vec<LabNodeInfo> {
    let statuses: std::collections::HashMap<String, String> =
        crate::db::repository::list_addon_config_prefixed(
            &ctx.state.db,
            instance_id,
            NODE_STATUS_KEY_PREFIX,
        )
        .unwrap_or_default()
        .into_iter()
        .map(|(node, value, _)| (node, value))
        .collect();
    let local_id = ctx.state.local_node_id.to_string();

    let mut ids: Vec<(String, bool)> = vec![(local_id.clone(), true)];
    if let Some(iroh) = ctx.state.quic_mesh.as_ref() {
        for peer in ctx.state.mesh_peer_store.list() {
            if peer.node_id == local_id || !iroh.is_trusted(&peer.node_id) {
                continue;
            }
            ids.push((peer.node_id.clone(), peer.quic_connected));
        }
    }

    ids.into_iter()
        .map(|(node_id, online)| {
            let instance_status = statuses
                .get(&node_id)
                .and_then(|v| serde_json::from_str::<serde_json::Value>(v).ok())
                .and_then(|v| v.get("status").and_then(|s| s.as_str()).map(str::to_string))
                .unwrap_or_else(|| "unknown".to_string());
            LabNodeInfo {
                node_name: ctx
                    .state
                    .mesh_peer_store
                    .get_hostname(&node_id)
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| node_id.clone()),
                is_local: node_id == local_id,
                online,
                instance_status,
                node_id,
            }
        })
        .collect()
}

/// Labs the caller may enter. This is the ONE request without an instance id,
/// so it evaluates membership itself, per instance: a lab that does not grant
/// the caller `quant.read` OR whose Visibility does not admit them is not
/// listed at all, and every other request about it answers `AppUnavailable`.
///
/// A lab nobody has scoped yet has no visibility rule, which the platform reads
/// as "visible to the organization" — scoping an instance to the group it was
/// created for is the administrator's act at install (plan §10.2), and this
/// handler reports exactly that state rather than inventing a stricter one.
///
/// A DISABLED instance is listed for admins only. It refuses every other
/// request anyway, and an administrator needs to see why the tile is inert.
fn lab_list(ctx: &HandlerContext) -> Result<MessageBody, ProtocolError> {
    let org = ctx.org_context.as_ref().ok_or_else(|| {
        ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required")
    })?;
    let checker = checker(ctx)?;
    let is_admin = SessionAuthKind::Admin.session_satisfies(&ctx.session);
    let instances = crate::db::repository::list_package_instances(&ctx.state.db, PACKAGE_ID)
        .map_err(|e| internal("instance list", e))?;
    // One scan of the accounts for the whole list: per lab the work is then one
    // visibility read plus matrix checks the checker answers from its cache.
    let accounts = people::accounts(&ctx.state.db);

    let mut labs = Vec::new();
    for (instance_id, display_name, enabled) in instances {
        let my_permissions = people::granted_permissions(checker, &instance_id, &org.user_id);
        if !my_permissions.iter().any(|p| p == PERM_READ) {
            continue;
        }
        let visibility = people::LabVisibility::of(&ctx.state.db, &instance_id);
        if !visibility.admits(checker, &org.user_id) {
            continue;
        }
        if !enabled && !is_admin {
            continue;
        }
        // A lab whose database cannot be opened still belongs on the list with
        // zeroed counters: hiding it would make a broken instance look
        // uninstalled, and the node status column is what explains it.
        let (project_count, last_activity_at) =
            match crate::tentaquant::open_db(&ctx.state.db, &org.org_id, &instance_id) {
                Ok(db) => {
                    let projects = store::list_projects(&db, &org.user_id, false)
                        .map_err(|e| internal("project list", e))?;
                    let activity = store::last_activity(&db, &org.user_id)
                        .map_err(|e| internal("activity", e))?;
                    (projects.len() as u32, activity)
                }
                Err(e) => {
                    tracing::warn!(instance_id, error = %e, "tentaquant: lab database unavailable");
                    (0, None)
                }
            };
        labs.push(LabInfo {
            people_count: people::count(&accounts, &visibility, checker, &instance_id),
            my_permissions,
            instance_id,
            display_name,
            enabled,
            project_count,
            last_activity_at,
            nodes: Vec::new(),
        });
    }
    for lab in &mut labs {
        lab.nodes = lab_nodes(ctx, &lab.instance_id);
    }

    Ok(tq(P::LabListResponse {
        labs,
        local_node_id: ctx.state.local_node_id.to_string(),
        // Installing another instance is an administrator's act (the same gate
        // `AddonInstallRequest` applies), so the "+ new lab" tile asks here.
        can_create: is_admin,
    }))
}

fn lab_overview(ctx: &HandlerContext, instance_id: &str) -> Result<MessageBody, ProtocolError> {
    let g = lab(ctx, instance_id, PERM_READ)?;
    let (my_projects, shared_with_me, lab_projects) =
        store::project_counts(&g.db, &g.user_id).map_err(|e| internal("project counts", e))?;
    let (runs_7d_total, runs_7d_succeeded, runs_7d_failed, runs_7d_running) =
        store::run_counts_7d(&g.db, &g.user_id).map_err(|e| internal("run counts", e))?;
    Ok(tq(P::LabOverviewResponse {
        instance_id: g.instance_id.clone(),
        my_projects,
        shared_with_me,
        lab_projects,
        runs_7d_total,
        runs_7d_succeeded,
        runs_7d_failed,
        runs_7d_running,
        people_with_access: people::count(
            &people::accounts(&ctx.state.db),
            &people::LabVisibility::of(&ctx.state.db, &g.instance_id),
            checker(ctx)?,
            &g.instance_id,
        ),
        last_activity_at: store::last_activity(&g.db, &g.user_id)
            .map_err(|e| internal("activity", e))?,
    }))
}

fn lab_people(ctx: &HandlerContext, instance_id: &str) -> Result<MessageBody, ProtocolError> {
    let g = lab(ctx, instance_id, PERM_INSTRUCT)?;
    Ok(tq(P::LabPeopleResponse {
        people: people::list(
            &people::accounts(&ctx.state.db),
            &people::LabVisibility::of(&ctx.state.db, &g.instance_id),
            checker(ctx)?,
            &g.instance_id,
        ),
        instance_id: g.instance_id,
    }))
}

/// The share picker's directory: the organization's TentaFlow accounts, with
/// `in_lab` per row. Open to every member (`quant.read` — what [`lab`] already
/// proves), NOT to `quant.instruct` alone: sharing a project is the owner's
/// decision, and an owner who cannot look anybody up cannot make it. It is a
/// wider read than [`lab_people`] on purpose — the answer says who has a
/// TentaFlow account, which the person doing the sharing already knows from
/// every other screen of the dashboard, and `in_lab` is what turns "this share
/// will be dormant" into something the window can warn about BEFORE the click.
///
/// The instance's Visibility is resolved once for the whole answer; per row it
/// would re-read the same rules for every candidate.
fn people_candidates(
    ctx: &HandlerContext,
    instance_id: &str,
    query: &str,
    limit: u32,
) -> Result<MessageBody, ProtocolError> {
    let g = lab(ctx, instance_id, PERM_READ)?;
    let limit = if limit == 0 {
        PEOPLE_CANDIDATES_LIMIT_MAX
    } else {
        limit.min(PEOPLE_CANDIDATES_LIMIT_MAX)
    };
    Ok(tq(P::PeopleCandidatesResponse {
        people: people::candidates(
            &people::accounts(&ctx.state.db),
            &people::LabVisibility::of(&ctx.state.db, &g.instance_id),
            checker(ctx)?,
            &g.instance_id,
            query,
            limit as usize,
        ),
        instance_id: g.instance_id,
    }))
}

/// Every member reads the operational half — the `device="auto"` rule of §4.2
/// runs in the browser and needs the qubit ceilings and the default tier. The
/// admin half (isolation, retention, the trusted-native acknowledgement) is
/// `quant.admin` alone (§10.2), and is OMITTED rather than defaulted for
/// everyone else: a faked value here would be read as the lab's real posture.
fn settings_get(ctx: &HandlerContext, instance_id: &str) -> Result<MessageBody, ProtocolError> {
    let g = lab(ctx, instance_id, PERM_READ)?;
    Ok(tq(P::SettingsResponse {
        settings: store::settings(&g.db).map_err(|e| internal("settings", e))?,
        admin: admin_half(ctx, &g)?,
        instance_id: g.instance_id,
    }))
}

/// The admin half of the settings, or `None` when the caller may not see it.
fn admin_half(ctx: &HandlerContext, g: &Lab) -> Result<Option<LabAdminSettings>, ProtocolError> {
    if !holds(ctx, &g.instance_id, PERM_ADMIN) {
        return Ok(None);
    }
    Ok(Some(
        store::admin_settings(&g.db).map_err(|e| internal("settings", e))?,
    ))
}

/// Settings are split by who may decide what (§10.2): a supervisor turns the
/// course ranking on and off, everything else — qubit ceilings, timeouts and
/// the whole admin half — is `quant.admin`. So the gate is `instruct`, and any
/// field that actually CHANGES beyond the ranking additionally demands `admin`;
/// sending an unchanged document through as a supervisor is fine.
fn settings_set(
    ctx: &HandlerContext,
    instance_id: &str,
    incoming: &LabSettings,
    incoming_admin: Option<&LabAdminSettings>,
) -> Result<MessageBody, ProtocolError> {
    let g = lab(ctx, instance_id, PERM_INSTRUCT)?;
    let current = store::settings(&g.db).map_err(|e| internal("settings", e))?;
    let is_admin = holds(ctx, &g.instance_id, PERM_ADMIN);

    let mut ranking_only = current.clone();
    ranking_only.ranking_enabled = incoming.ranking_enabled;
    if (&ranking_only != incoming || incoming_admin.is_some()) && !is_admin {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "quant.admin permission required to change laboratory limits",
        ));
    }
    validate_settings(incoming)?;
    store::set_settings(&g.db, incoming).map_err(|e| internal("settings write", e))?;
    if let Some(admin) = incoming_admin {
        validate_admin_settings(admin)?;
        store::set_admin_settings(&g.db, admin).map_err(|e| internal("settings write", e))?;
    }
    Ok(tq(P::SettingsResponse {
        settings: store::settings(&g.db).map_err(|e| internal("settings", e))?,
        admin: admin_half(ctx, &g)?,
        instance_id: g.instance_id,
    }))
}

/// Bounds the settings document. The ceilings are not cosmetic: a state vector
/// costs 2^n amplitudes, so an unchecked value is an out-of-memory kill of the
/// node rather than a slow run.
fn validate_settings(s: &LabSettings) -> Result<(), ProtocolError> {
    if !matches!(
        s.default_tier.as_str(),
        "browser" | "core" | "python" | "gpu"
    ) {
        return Err(ProtocolError::bad_request("unknown default tier"));
    }
    for qubits in [
        s.max_qubits_browser,
        s.max_qubits_core,
        s.max_qubits_python,
        s.max_qubits_gpu,
    ] {
        if !(1..=40).contains(&qubits) {
            return Err(ProtocolError::bad_request(
                "qubit ceiling must be between 1 and 40",
            ));
        }
    }
    if s.kernel_idle_ttl_secs == 0 || s.cell_timeout_secs == 0 || s.gpu_cell_timeout_secs == 0 {
        return Err(ProtocolError::bad_request("timeouts must be positive"));
    }
    Ok(())
}

/// Bounds the admin half. Retention of zero days would delete every artifact
/// the moment the sweep runs, which is a data-loss switch, not a setting.
fn validate_admin_settings(s: &LabAdminSettings) -> Result<(), ProtocolError> {
    if !matches!(s.isolation_mode.as_str(), "container" | "trusted_native") {
        return Err(ProtocolError::bad_request("unknown isolation mode"));
    }
    if s.retention_days == 0 {
        return Err(ProtocolError::bad_request("retention must be positive"));
    }
    Ok(())
}

// =============================================================================
// Projects
// =============================================================================

/// The caller's role on a project, or the uniform NotFound.
fn role(g: &Lab, project_id: &str) -> Result<store::ProjectRole, ProtocolError> {
    store::access(&g.db, project_id, &g.user_id)
        .map_err(|e| internal("project access", e))?
        .ok_or_else(not_found)
}

/// Role for a mutation: a `viewer` may read and run in the browser, never
/// write (§10.3), and an archived project is read-only until it is restored.
fn writable_role(g: &Lab, project_id: &str) -> Result<store::ProjectRole, ProtocolError> {
    let role = role(g, project_id)?;
    if !role.may_write() {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "viewer role cannot modify this project",
        ));
    }
    let project = store::project(&g.db, project_id)
        .map_err(|e| internal("project", e))?
        .ok_or_else(not_found)?;
    if project.archived_at.is_some() {
        return Err(ProtocolError::new(
            ProtocolErrorCode::Conflict,
            "project is archived and read-only",
        ));
    }
    Ok(role)
}

/// Only the owner may change ownership, sharing or the project's existence.
fn owner_only(g: &Lab, project_id: &str) -> Result<(), ProtocolError> {
    if role(g, project_id)? != store::ProjectRole::Owner {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "only the project owner may do this",
        ));
    }
    Ok(())
}

/// Publishing to the whole lab is a supervisor's act (§10.3), so the value is
/// validated against the caller's permissions, not only against the schema.
fn validate_visibility(
    ctx: &HandlerContext,
    instance_id: &str,
    visibility: &str,
) -> Result<(), ProtocolError> {
    match visibility {
        "private" => Ok(()),
        "lab" => {
            if holds(ctx, instance_id, PERM_INSTRUCT) {
                Ok(())
            } else {
                Err(ProtocolError::new(
                    ProtocolErrorCode::PolicyDenied,
                    "quant.instruct permission required to publish to the laboratory",
                ))
            }
        }
        _ => Err(ProtocolError::bad_request(
            "visibility must be private or lab",
        )),
    }
}

fn validate_name(name: &str) -> Result<String, ProtocolError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 120 {
        return Err(ProtocolError::bad_request("name must be 1-120 characters"));
    }
    Ok(trimmed.to_string())
}

/// One project row for the wire. Owner name and counters are passed in, not
/// looked up: a single project answers with one query each, a listing resolves
/// both in bulk, and neither shape is hidden inside this mapping.
///
/// `role` is `None` only where the caller has just lost their access — a
/// transfer of a private project — and the wire says `"none"` rather than
/// naming a role the very next request would refuse.
fn project_info(
    record: &store::ProjectRecord,
    role: Option<store::ProjectRole>,
    owner_name: String,
    stats: store::ProjectStats,
) -> ProjectInfo {
    ProjectInfo {
        project_id: record.id.clone(),
        name: record.name.clone(),
        description: record.description.clone(),
        owner_name,
        owner_user_id: record.owner_user_id.clone(),
        visibility: record.visibility.clone(),
        my_role: role.map_or("none", store::ProjectRole::as_str).to_string(),
        share_count: stats.shares,
        file_count: stats.files,
        notebook_count: stats.notebooks,
        run_count: stats.runs,
        linked_project_id: record.linked_project_id.clone(),
        created_at: record.created_at.clone(),
        updated_at: record.updated_at.clone(),
        archived_at: record.archived_at.clone(),
    }
}

/// [`project_info`] for ONE project, resolving the owner's name and counters
/// the single-row way.
fn one_project_info(
    ctx: &HandlerContext,
    g: &Lab,
    record: &store::ProjectRecord,
    role: Option<store::ProjectRole>,
) -> Result<ProjectInfo, ProtocolError> {
    let stats =
        store::project_stats(&g.db, &record.id).map_err(|e| internal("project stats", e))?;
    Ok(project_info(
        record,
        role,
        people::display_name(&ctx.state.db, &record.owner_user_id),
        stats,
    ))
}

/// Answers create/update/archive/transfer with the project as it now stands.
fn project_answer(
    ctx: &HandlerContext,
    g: &Lab,
    project_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let record = store::project(&g.db, project_id)
        .map_err(|e| internal("project", e))?
        .ok_or_else(not_found)?;
    let role = role(g, project_id)?;
    Ok(tq(P::ProjectResponse {
        instance_id: g.instance_id.clone(),
        project: one_project_info(ctx, g, &record, Some(role))?,
    }))
}

fn project_list(
    ctx: &HandlerContext,
    instance_id: &str,
    include_archived: bool,
) -> Result<MessageBody, ProtocolError> {
    let g = lab(ctx, instance_id, PERM_READ)?;
    let records = store::list_projects(&g.db, &g.user_id, include_archived)
        .map_err(|e| internal("project list", e))?;
    // Three bulk reads for the whole list — the caller's share rows, every
    // project's counters and one scan of the org roster — instead of four
    // queries per listed project across two databases.
    let shares = store::shared_roles(&g.db, &g.user_id).map_err(|e| internal("shares", e))?;
    let stats = store::project_stats_all(&g.db).map_err(|e| internal("project stats", e))?;
    let names = people::name_index(&people::accounts(&ctx.state.db));
    let mut projects = Vec::with_capacity(records.len());
    for record in records {
        // `list_projects` already filtered to the visible set, so a record
        // without a role here would be a disagreement between the two rules —
        // and the safe reading of that is to leave it out.
        let Some(role) = store::role_of(&record, &g.user_id, &shares) else {
            continue;
        };
        let owner_name = names
            .get(&record.owner_user_id)
            .cloned()
            .unwrap_or_else(|| record.owner_user_id.clone());
        projects.push(project_info(
            &record,
            Some(role),
            owner_name,
            stats.get(&record.id).copied().unwrap_or_default(),
        ));
    }
    Ok(tq(P::ProjectListResponse {
        instance_id: g.instance_id,
        projects,
    }))
}

/// Share rows resolved for display, each carrying whether the person is in the
/// lab at all — a share to somebody the lab does not admit is dormant, and the
/// UI has to say so rather than imply access that does not exist.
fn shares_of(
    ctx: &HandlerContext,
    g: &Lab,
    project_id: &str,
) -> Result<Vec<ProjectShareInfo>, ProtocolError> {
    let checker = checker(ctx)?;
    // One visibility read for the whole share list: every row asks the same
    // membership question about a different person in the SAME lab.
    let visibility = people::LabVisibility::of(&ctx.state.db, &g.instance_id);
    Ok(store::list_shares(&g.db, project_id)
        .map_err(|e| internal("shares", e))?
        .into_iter()
        .map(|s| ProjectShareInfo {
            display_name: people::display_name(&ctx.state.db, &s.user_id),
            has_lab_access: people::is_member_of(&visibility, checker, &g.instance_id, &s.user_id),
            user_id: s.user_id,
            role: s.role,
            granted_by: s.granted_by,
            granted_at: s.granted_at,
        })
        .collect())
}

fn project_get(
    ctx: &HandlerContext,
    instance_id: &str,
    project_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let g = lab(ctx, instance_id, PERM_READ)?;
    let role = role(&g, project_id)?;
    let record = store::project(&g.db, project_id)
        .map_err(|e| internal("project", e))?
        .ok_or_else(not_found)?;
    // Who a project is shared with is the owner's business; a reader gets the
    // project without the guest list.
    let shares = if role == store::ProjectRole::Owner {
        shares_of(ctx, &g, project_id)?
    } else {
        Vec::new()
    };
    Ok(tq(P::ProjectGetResponse {
        project: one_project_info(ctx, &g, &record, Some(role))?,
        instance_id: g.instance_id,
        shares,
    }))
}

fn project_create(
    ctx: &HandlerContext,
    instance_id: &str,
    name: &str,
    description: &str,
    visibility: &str,
    linked_project_id: Option<&str>,
) -> Result<MessageBody, ProtocolError> {
    let g = lab(ctx, instance_id, PERM_RUN)?;
    let name = validate_name(name)?;
    validate_visibility(ctx, &g.instance_id, visibility)?;
    let id = store::create_project(
        &g.db,
        &g.user_id,
        &name,
        description.trim(),
        visibility,
        linked_project_id,
    )
    .map_err(|e| internal("project create", e))?;
    project_answer(ctx, &g, &id)
}

fn project_update(
    ctx: &HandlerContext,
    instance_id: &str,
    project_id: &str,
    name: &str,
    description: &str,
    visibility: &str,
    linked_project_id: Option<&str>,
) -> Result<MessageBody, ProtocolError> {
    let g = lab(ctx, instance_id, PERM_RUN)?;
    writable_role(&g, project_id)?;
    let name = validate_name(name)?;
    validate_visibility(ctx, &g.instance_id, visibility)?;
    store::update_project(
        &g.db,
        project_id,
        &name,
        description.trim(),
        visibility,
        linked_project_id,
    )
    .map_err(|e| internal("project update", e))?;
    project_answer(ctx, &g, project_id)
}

fn project_archive(
    ctx: &HandlerContext,
    instance_id: &str,
    project_id: &str,
    archived: bool,
) -> Result<MessageBody, ProtocolError> {
    let g = lab(ctx, instance_id, PERM_RUN)?;
    owner_only(&g, project_id)?;
    store::set_project_archived(&g.db, project_id, archived)
        .map_err(|e| internal("project archive", e))?;
    project_answer(ctx, &g, project_id)
}

fn project_transfer(
    ctx: &HandlerContext,
    instance_id: &str,
    project_id: &str,
    new_owner_user_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let g = lab(ctx, instance_id, PERM_RUN)?;
    owner_only(&g, project_id)?;
    // Handing a project to somebody the lab does not admit would make it
    // unreachable for everyone, including the person who gave it away.
    if !people::is_member(
        &ctx.state.db,
        checker(ctx)?,
        &g.instance_id,
        new_owner_user_id,
    ) {
        return Err(ProtocolError::bad_request(
            "the new owner has no access to this laboratory",
        ));
    }
    store::transfer_project(&g.db, project_id, new_owner_user_id)
        .map_err(|e| internal("project transfer", e))?;
    let record = store::project(&g.db, project_id)
        .map_err(|e| internal("project", e))?
        .ok_or_else(not_found)?;
    // Handing a PRIVATE project away leaves the former owner with no role at
    // all, and the answer has to say exactly that: the project is gone from
    // their list and the next `ProjectGetRequest` is the uniform NotFound.
    // Reporting `viewer` here would be an access the very next request refuses.
    // A lab-visible project, or one the new owner shares back, still resolves
    // to a real role, which is why this reads the access rather than assuming.
    let role =
        store::access(&g.db, project_id, &g.user_id).map_err(|e| internal("project access", e))?;
    Ok(tq(P::ProjectResponse {
        instance_id: g.instance_id.clone(),
        project: one_project_info(ctx, &g, &record, role)?,
    }))
}

fn project_delete(
    ctx: &HandlerContext,
    instance_id: &str,
    project_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let g = lab(ctx, instance_id, PERM_RUN)?;
    owner_only(&g, project_id)?;
    store::delete_project(&g.db, project_id).map_err(|e| internal("project delete", e))?;
    Ok(tq(P::ProjectDeleteResponse {
        instance_id: g.instance_id,
        project_id: project_id.to_string(),
    }))
}

fn share_set(
    ctx: &HandlerContext,
    instance_id: &str,
    project_id: &str,
    user_id: &str,
    role_name: &str,
) -> Result<MessageBody, ProtocolError> {
    let g = lab(ctx, instance_id, PERM_RUN)?;
    owner_only(&g, project_id)?;
    if store::ProjectRole::parse_share(role_name).is_none() {
        return Err(ProtocolError::bad_request("role must be editor or viewer"));
    }
    if user_id == g.user_id {
        return Err(ProtocolError::bad_request(
            "the owner already has full access",
        ));
    }
    // A share to somebody outside the lab is ACCEPTED and stored dormant: the
    // administrator may grant them `quant.read` later, and refusing here would
    // force the owner to redo the sharing after that.
    store::set_share(&g.db, project_id, user_id, role_name, &g.user_id)
        .map_err(|e| internal("share set", e))?;
    Ok(tq(P::ProjectSharesResponse {
        shares: shares_of(ctx, &g, project_id)?,
        instance_id: g.instance_id,
        project_id: project_id.to_string(),
    }))
}

fn share_remove(
    ctx: &HandlerContext,
    instance_id: &str,
    project_id: &str,
    user_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let g = lab(ctx, instance_id, PERM_RUN)?;
    owner_only(&g, project_id)?;
    store::remove_share(&g.db, project_id, user_id).map_err(|e| internal("share remove", e))?;
    Ok(tq(P::ProjectSharesResponse {
        shares: shares_of(ctx, &g, project_id)?,
        instance_id: g.instance_id,
        project_id: project_id.to_string(),
    }))
}

// =============================================================================
// Files
// =============================================================================

fn file_info(record: store::FileRecord) -> FileInfo {
    FileInfo {
        file_id: record.id,
        project_id: record.project_id,
        path: record.path,
        kind: record.kind,
        sha256: record.sha256,
        size_bytes: record.size_bytes,
        updated_at: record.updated_at,
    }
}

fn file_list(
    ctx: &HandlerContext,
    instance_id: &str,
    project_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let g = lab(ctx, instance_id, PERM_READ)?;
    role(&g, project_id)?;
    Ok(tq(P::FileListResponse {
        files: store::list_files(&g.db, project_id)
            .map_err(|e| internal("file list", e))?
            .into_iter()
            .map(file_info)
            .collect(),
        instance_id: g.instance_id,
        project_id: project_id.to_string(),
    }))
}

#[allow(clippy::too_many_arguments)]
async fn file_upload_chunk(
    ctx: &HandlerContext,
    instance_id: &str,
    project_id: &str,
    upload_id: &str,
    path: &str,
    kind: &str,
    seq: u32,
    total_chunks: u32,
    bytes: &[u8],
) -> Result<MessageBody, ProtocolError> {
    let g = lab(ctx, instance_id, PERM_RUN)?;
    writable_role(&g, project_id)?;
    let path = cas::validate_path(path).map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    let kind = cas::validate_kind(kind)
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?
        .to_string();
    // Refuse before the transfer, not after: a notebook's backing row may not
    // be retargeted, and finding that out on the last chunk would waste the
    // whole upload.
    if store::path_backs_notebook(&g.db, project_id, &path).map_err(|e| internal("file path", e))? {
        return Err(notebook_backed_path());
    }

    let (org_id, instance, user_id, project, upload, dir, payload) = (
        g.org_id.clone(),
        g.instance_id.clone(),
        g.user_id.clone(),
        project_id.to_string(),
        upload_id.to_string(),
        g.data_dir.clone(),
        bytes.to_vec(),
    );
    let outcome = tokio::task::spawn_blocking(move || {
        cas::accept_chunk(
            &org_id,
            &instance,
            &user_id,
            &project,
            &dir,
            &upload,
            seq,
            total_chunks,
            &payload,
        )
    })
    .await
    .map_err(|_| ProtocolError::internal("upload task panicked"))?
    .map_err(|e| ProtocolError::bad_request(format!("upload rejected: {e}")))?;

    let (received_chunks, received_bytes, file) = match outcome {
        cas::ChunkOutcome::Buffered {
            received_chunks,
            received_bytes,
        } => (received_chunks, received_bytes, None),
        cas::ChunkOutcome::Finalized {
            sha256,
            received_chunks,
            size_bytes,
        } => {
            match store::upsert_file(&g.db, project_id, &path, &kind, &sha256, size_bytes)
                .map_err(|e| internal("file record", e))?
            {
                store::FileUpsert::Stored(record) => {
                    (received_chunks, size_bytes, Some(file_info(record)))
                }
                store::FileUpsert::NotebookBacking => return Err(notebook_backed_path()),
            }
        }
    };
    Ok(tq(P::FileUploadChunkResponse {
        instance_id: g.instance_id,
        project_id: project_id.to_string(),
        upload_id: upload_id.to_string(),
        received_chunks,
        received_bytes,
        complete: file.is_some(),
        file,
    }))
}

/// The refusal both upload checks answer with — the pre-transfer one and the
/// one at the write — so an uploader cannot tell which of the two stopped it
/// and gets the same instruction either way.
fn notebook_backed_path() -> ProtocolError {
    ProtocolError::new(
        ProtocolErrorCode::PolicyDenied,
        "that path backs a notebook: save the notebook instead of writing its file",
    )
}

fn file_delete(
    ctx: &HandlerContext,
    instance_id: &str,
    project_id: &str,
    file_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let g = lab(ctx, instance_id, PERM_RUN)?;
    writable_role(&g, project_id)?;
    // The blob stays: it is content-addressed and another project of the same
    // lab may point at the identical bytes. Reclaiming it belongs to the
    // retention sweep, which can see every reference and does not exist in this
    // phase (see the note in `tentaquant/cas.rs`).
    match store::delete_file(&g.db, project_id, file_id).map_err(|e| internal("file delete", e))? {
        store::FileDeletion::Deleted => {}
        store::FileDeletion::Missing => {
            return Err(ProtocolError::new(
                ProtocolErrorCode::NotFound,
                "file not found",
            ))
        }
        store::FileDeletion::NotebookBacking => {
            return Err(ProtocolError::new(
                ProtocolErrorCode::PolicyDenied,
                "file backs a notebook: delete the notebook, not its file",
            ))
        }
    }
    Ok(tq(P::FileDeleteResponse {
        instance_id: g.instance_id,
        project_id: project_id.to_string(),
        file_id: file_id.to_string(),
    }))
}

// =============================================================================
// Notebooks
// =============================================================================

fn notebook_info(record: store::NotebookRecord) -> NotebookInfo {
    NotebookInfo {
        notebook_id: record.id,
        project_id: record.project_id,
        file_id: record.file_id,
        name: record.name,
        current_version: record.current_version,
        updated_by: record.updated_by,
        updated_at: record.updated_at,
    }
}

fn notebook_list(
    ctx: &HandlerContext,
    instance_id: &str,
    project_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let g = lab(ctx, instance_id, PERM_READ)?;
    role(&g, project_id)?;
    Ok(tq(P::NotebookListResponse {
        notebooks: store::list_notebooks(&g.db, project_id)
            .map_err(|e| internal("notebook list", e))?
            .into_iter()
            .map(notebook_info)
            .collect(),
        instance_id: g.instance_id,
        project_id: project_id.to_string(),
    }))
}

fn notebook_create(
    ctx: &HandlerContext,
    instance_id: &str,
    project_id: &str,
    name: &str,
    cells_json: &str,
) -> Result<MessageBody, ProtocolError> {
    let g = lab(ctx, instance_id, PERM_RUN)?;
    writable_role(&g, project_id)?;
    let name = validate_name(name)?;
    let cells = if cells_json.trim().is_empty() {
        "[]"
    } else {
        cells_json
    };
    validate_cells(cells)?;
    // The notebook's file path is derived from its name so the file list and
    // the notebook list describe the same object; the CAS never sees the path.
    let path = cas::validate_path(&format!("notebooks/{}.ipynb", slug(&name)))
        .map_err(|e| ProtocolError::bad_request(e.to_string()))?;
    match store::create_notebook(&g.db, project_id, &name, &path, cells, &g.user_id)
        .map_err(|e| internal("notebook create", e))?
    {
        store::NotebookCreation::Created(record) => Ok(tq(P::NotebookResponse {
            instance_id: g.instance_id,
            notebook: notebook_info(record),
        })),
        store::NotebookCreation::PathTaken => Err(ProtocolError::new(
            ProtocolErrorCode::Conflict,
            format!("'{path}' already exists in this project — choose another name"),
        )),
    }
}

/// Cells travel as JSON and are stored verbatim; the server does not interpret
/// them, but it refuses anything that is not a JSON array, so a corrupt save
/// cannot make a notebook unopenable later.
fn validate_cells(cells_json: &str) -> Result<(), ProtocolError> {
    match serde_json::from_str::<serde_json::Value>(cells_json) {
        Ok(serde_json::Value::Array(_)) => Ok(()),
        Ok(_) => Err(ProtocolError::bad_request("cells must be a JSON array")),
        Err(e) => Err(ProtocolError::bad_request(format!(
            "invalid cells json: {e}"
        ))),
    }
}

/// Filesystem-safe stem for a notebook's path. Non-alphanumerics collapse into
/// single dashes, so different names ("Bell" and "bell!") can land on one stem;
/// `create_notebook` answers that collision with `PathTaken`, which this family
/// turns into a Conflict the author can act on, rather than overwriting.
fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_dash = true;
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "notebook".to_string()
    } else {
        trimmed
    }
}

fn notebook_get(
    ctx: &HandlerContext,
    instance_id: &str,
    project_id: &str,
    notebook_id: &str,
    version: Option<u32>,
) -> Result<MessageBody, ProtocolError> {
    let g = lab(ctx, instance_id, PERM_READ)?;
    role(&g, project_id)?;
    let record = store::notebook(&g.db, project_id, notebook_id)
        .map_err(|e| internal("notebook", e))?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "notebook not found"))?;
    let wanted = version.unwrap_or(record.current_version);
    let cells_json = store::notebook_cells(&g.db, notebook_id, wanted)
        .map_err(|e| internal("notebook version", e))?
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::NotFound, "version not found"))?;
    Ok(tq(P::NotebookGetResponse {
        instance_id: g.instance_id,
        notebook: notebook_info(record),
        version: wanted,
        cells_json,
    }))
}

fn notebook_save(
    ctx: &HandlerContext,
    instance_id: &str,
    project_id: &str,
    notebook_id: &str,
    cells_json: &str,
    expected_version: u32,
) -> Result<MessageBody, ProtocolError> {
    let g = lab(ctx, instance_id, PERM_RUN)?;
    writable_role(&g, project_id)?;
    validate_cells(cells_json)?;
    match store::save_notebook(
        &g.db,
        project_id,
        notebook_id,
        cells_json,
        expected_version,
        &g.user_id,
    )
    .map_err(|e| internal("notebook save", e))?
    {
        store::SaveOutcome::Saved(_) => {
            let record = store::notebook(&g.db, project_id, notebook_id)
                .map_err(|e| internal("notebook", e))?
                .ok_or_else(|| {
                    ProtocolError::new(ProtocolErrorCode::NotFound, "notebook not found")
                })?;
            Ok(tq(P::NotebookResponse {
                instance_id: g.instance_id,
                notebook: notebook_info(record),
            }))
        }
        store::SaveOutcome::Conflict => Err(ProtocolError::new(
            ProtocolErrorCode::Conflict,
            "the notebook changed since it was loaded — reload before saving",
        )),
        store::SaveOutcome::NotFound => Err(ProtocolError::new(
            ProtocolErrorCode::NotFound,
            "notebook not found",
        )),
    }
}

fn notebook_versions(
    ctx: &HandlerContext,
    instance_id: &str,
    project_id: &str,
    notebook_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let g = lab(ctx, instance_id, PERM_READ)?;
    role(&g, project_id)?;
    if store::notebook(&g.db, project_id, notebook_id)
        .map_err(|e| internal("notebook", e))?
        .is_none()
    {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotFound,
            "notebook not found",
        ));
    }
    Ok(tq(P::NotebookVersionsResponse {
        versions: store::notebook_versions(&g.db, notebook_id)
            .map_err(|e| internal("notebook versions", e))?
            .into_iter()
            .map(|v| NotebookVersionInfo {
                version: v.version,
                sha256: v.sha256,
                author: v.author,
                created_at: v.created_at,
            })
            .collect(),
        instance_id: g.instance_id,
        notebook_id: notebook_id.to_string(),
    }))
}

// =============================================================================
// Dispatcher
// =============================================================================

#[handler(variant = "TentaQuantBody", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn tentaquant_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::TentaQuantBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected TentaQuantBody")),
    };
    match payload {
        P::LabListRequest {} => lab_list(ctx),
        P::LabOverviewRequest { instance_id } => lab_overview(ctx, instance_id),
        P::LabPeopleRequest { instance_id } => lab_people(ctx, instance_id),
        P::PeopleCandidatesRequest {
            instance_id,
            query,
            limit,
        } => people_candidates(ctx, instance_id, query, *limit),
        P::SettingsGetRequest { instance_id } => settings_get(ctx, instance_id),
        P::SettingsSetRequest {
            instance_id,
            settings,
            admin,
        } => settings_set(ctx, instance_id, settings, admin.as_ref()),

        P::ProjectListRequest {
            instance_id,
            include_archived,
        } => project_list(ctx, instance_id, *include_archived),
        P::ProjectGetRequest {
            instance_id,
            project_id,
        } => project_get(ctx, instance_id, project_id),
        P::ProjectCreateRequest {
            instance_id,
            name,
            description,
            visibility,
            linked_project_id,
        } => project_create(
            ctx,
            instance_id,
            name,
            description,
            visibility,
            linked_project_id.as_deref(),
        ),
        P::ProjectUpdateRequest {
            instance_id,
            project_id,
            name,
            description,
            visibility,
            linked_project_id,
        } => project_update(
            ctx,
            instance_id,
            project_id,
            name,
            description,
            visibility,
            linked_project_id.as_deref(),
        ),
        P::ProjectArchiveRequest {
            instance_id,
            project_id,
            archived,
        } => project_archive(ctx, instance_id, project_id, *archived),
        P::ProjectTransferRequest {
            instance_id,
            project_id,
            new_owner_user_id,
        } => project_transfer(ctx, instance_id, project_id, new_owner_user_id),
        P::ProjectDeleteRequest {
            instance_id,
            project_id,
        } => project_delete(ctx, instance_id, project_id),
        P::ProjectShareSetRequest {
            instance_id,
            project_id,
            user_id,
            role,
        } => share_set(ctx, instance_id, project_id, user_id, role),
        P::ProjectShareRemoveRequest {
            instance_id,
            project_id,
            user_id,
        } => share_remove(ctx, instance_id, project_id, user_id),

        P::FileListRequest {
            instance_id,
            project_id,
        } => file_list(ctx, instance_id, project_id),
        P::FileUploadChunkRequest {
            instance_id,
            project_id,
            upload_id,
            path,
            kind,
            seq,
            total_chunks,
            bytes,
        } => {
            file_upload_chunk(
                ctx,
                instance_id,
                project_id,
                upload_id,
                path,
                kind,
                *seq,
                *total_chunks,
                bytes,
            )
            .await
        }
        P::FileDeleteRequest {
            instance_id,
            project_id,
            file_id,
        } => file_delete(ctx, instance_id, project_id, file_id),

        P::NotebookListRequest {
            instance_id,
            project_id,
        } => notebook_list(ctx, instance_id, project_id),
        P::NotebookCreateRequest {
            instance_id,
            project_id,
            name,
            cells_json,
        } => notebook_create(ctx, instance_id, project_id, name, cells_json),
        P::NotebookGetRequest {
            instance_id,
            project_id,
            notebook_id,
            version,
        } => notebook_get(ctx, instance_id, project_id, notebook_id, *version),
        P::NotebookSaveRequest {
            instance_id,
            project_id,
            notebook_id,
            cells_json,
            expected_version,
        } => notebook_save(
            ctx,
            instance_id,
            project_id,
            notebook_id,
            cells_json,
            *expected_version,
        ),
        P::NotebookVersionsRequest {
            instance_id,
            project_id,
            notebook_id,
        } => notebook_versions(ctx, instance_id, project_id, notebook_id),

        // Response variants share the enum with the requests; a client sending
        // one back is a protocol error, not a request this server can answer.
        other => Err(ProtocolError::bad_request(format!(
            "'{}' is not a TentaQuant request",
            variant_of(other)
        ))),
    }
}

fn variant_of(payload: &P) -> String {
    serde_json::to_value(payload)
        .ok()
        .and_then(|v| v.as_object().and_then(|m| m.keys().next().cloned()))
        .unwrap_or_else(|| "unknown".to_string())
}

// =============================================================================
// Variant registration
// =============================================================================

/// `#[handler]` registers the dispatcher under the family name, which no frame
/// ever carries — `variant_name_of` reports the concrete variant. Each request
/// variant therefore needs its own registry entry pointing at the same
/// dispatch wrapper, or `dispatch::find` answers NotImplemented.
macro_rules! register_tentaquant_variant {
    ($variant:literal, $metric:literal) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: crate::dispatch::SessionAuthKind::UserSession,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_tentaquant_dispatch,
            }
        }
    };
}

register_tentaquant_variant!(
    "TentaQuantLabListRequest",
    "tentaflow_ws_handler_tq_lab_list"
);
register_tentaquant_variant!(
    "TentaQuantLabOverviewRequest",
    "tentaflow_ws_handler_tq_lab_overview"
);
register_tentaquant_variant!(
    "TentaQuantLabPeopleRequest",
    "tentaflow_ws_handler_tq_lab_people"
);
register_tentaquant_variant!(
    "TentaQuantPeopleCandidatesRequest",
    "tentaflow_ws_handler_tq_people_candidates"
);
register_tentaquant_variant!(
    "TentaQuantSettingsGetRequest",
    "tentaflow_ws_handler_tq_settings_get"
);
register_tentaquant_variant!(
    "TentaQuantSettingsSetRequest",
    "tentaflow_ws_handler_tq_settings_set"
);
register_tentaquant_variant!(
    "TentaQuantProjectListRequest",
    "tentaflow_ws_handler_tq_project_list"
);
register_tentaquant_variant!(
    "TentaQuantProjectGetRequest",
    "tentaflow_ws_handler_tq_project_get"
);
register_tentaquant_variant!(
    "TentaQuantProjectCreateRequest",
    "tentaflow_ws_handler_tq_project_create"
);
register_tentaquant_variant!(
    "TentaQuantProjectUpdateRequest",
    "tentaflow_ws_handler_tq_project_update"
);
register_tentaquant_variant!(
    "TentaQuantProjectArchiveRequest",
    "tentaflow_ws_handler_tq_project_archive"
);
register_tentaquant_variant!(
    "TentaQuantProjectTransferRequest",
    "tentaflow_ws_handler_tq_project_transfer"
);
register_tentaquant_variant!(
    "TentaQuantProjectDeleteRequest",
    "tentaflow_ws_handler_tq_project_delete"
);
register_tentaquant_variant!(
    "TentaQuantProjectShareSetRequest",
    "tentaflow_ws_handler_tq_share_set"
);
register_tentaquant_variant!(
    "TentaQuantProjectShareRemoveRequest",
    "tentaflow_ws_handler_tq_share_remove"
);
register_tentaquant_variant!(
    "TentaQuantFileListRequest",
    "tentaflow_ws_handler_tq_file_list"
);
register_tentaquant_variant!(
    "TentaQuantFileUploadChunkRequest",
    "tentaflow_ws_handler_tq_file_upload"
);
register_tentaquant_variant!(
    "TentaQuantFileDeleteRequest",
    "tentaflow_ws_handler_tq_file_delete"
);
register_tentaquant_variant!(
    "TentaQuantNotebookListRequest",
    "tentaflow_ws_handler_tq_notebook_list"
);
register_tentaquant_variant!(
    "TentaQuantNotebookCreateRequest",
    "tentaflow_ws_handler_tq_notebook_create"
);
register_tentaquant_variant!(
    "TentaQuantNotebookGetRequest",
    "tentaflow_ws_handler_tq_notebook_get"
);
register_tentaquant_variant!(
    "TentaQuantNotebookSaveRequest",
    "tentaflow_ws_handler_tq_notebook_save"
);
register_tentaquant_variant!(
    "TentaQuantNotebookVersionsRequest",
    "tentaflow_ws_handler_tq_notebook_versions"
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::{Arc, MutexGuard};

    use tentaflow_protocol::tentaquant::{TentaQuantPayload, PERMISSION_IDS};
    use tentaflow_protocol::SessionAuth;

    use crate::dispatch::app_gate::test_support;
    use crate::dispatch::AppState;
    use crate::services::rbac::OrgContext;

    /// Serialises every test that redirects where instance data lands.
    ///
    /// `set_category_override(AddonData, ..)` is process-global and outranks
    /// the `HOME`/`TENTAFLOW_HOME` isolation `addon::fs_sandbox::with_tmp_home`
    /// uses, so a lock private to this module would not keep an addon-storage
    /// test from resolving paths under a temporary directory this fixture has
    /// already deleted. Both mechanisms therefore share ONE lock, the one
    /// `fs_sandbox` already owns.
    fn disk_lock() -> MutexGuard<'static, ()> {
        crate::addon::fs_sandbox::test_home_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    struct Fixture {
        _data: tempfile::TempDir,
        _guard: MutexGuard<'static, ()>,
        state: Arc<AppState>,
        /// Instance ids installed by this fixture, closed on drop: the pool
        /// registry in `addon::app_db` is process-global, so a pool left open
        /// over a removed directory would poison the next test.
        labs: Vec<String>,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            for lab in &self.labs {
                crate::addon::app_db::close(lab);
            }
            crate::paths::set_category_override(crate::paths::StorageCategory::AddonData, None);
        }
    }

    fn fixture() -> Fixture {
        let guard = disk_lock();
        let data = tempfile::tempdir().expect("data dir");
        let root = data.path().to_string_lossy().to_string();
        crate::paths::set_category_override(crate::paths::StorageCategory::AddonData, Some(root));
        Fixture {
            _data: data,
            _guard: guard,
            state: AppState::for_test(),
            labs: Vec::new(),
        }
    }

    /// Installs one ENABLED lab the way a real install does: the REAL manifest
    /// (so `app_db::open` finds `[native] db_file`) AND the permission defaults
    /// `lifecycle::install_native_instance` seeds from it, then the extra
    /// `permissions` for `user_id`. Seeding the defaults matters: `quant.read`
    /// and `quant.run` are `default = "allow"`, so a fixture without them would
    /// test an access configuration production never has.
    fn install_lab(fx: &mut Fixture, name: &str, user_id: &str, permissions: &[&str]) -> String {
        let addon_id = format!("tentaquant-{name}");
        let manifest = crate::addon::bundled::native_manifest(PACKAGE_ID).expect("manifest");
        let defaults: Vec<&str> = crate::addon::lifecycle::parse_manifest_toml(manifest)
            .expect("manifest parses")
            .declared_permissions
            .iter()
            .filter(|p| p.default_grant == "allow")
            .map(|p| {
                PERMISSION_IDS
                    .iter()
                    .find(|id| **id == p.id)
                    .copied()
                    .expect("declared permission is in the catalog")
            })
            .collect();
        test_support::install_app_instance(&fx.state, PACKAGE_ID, &addon_id, &defaults);
        for perm in permissions {
            test_support::grant(&fx.state, &addon_id, user_id, perm);
        }
        fx.labs.push(addon_id.clone());
        addon_id
    }

    /// One real org account, so visibility (which is group membership) has
    /// somebody to resolve. Returns the account id the handlers see.
    fn account(fx: &Fixture, username: &str) -> String {
        crate::db::repository::create_user_account(&fx.state.db, username, "$h$", username, "")
            .expect("account")
    }

    fn ctx(fx: &Fixture, user_id: &str) -> HandlerContext {
        HandlerContext {
            session: SessionAuth::UserSession {
                user_id: [7u8; 16],
                role: None,
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state: fx.state.clone(),
            org_context: Some(OrgContext {
                user_id: user_id.to_string(),
                org_id: "org-test".to_string(),
                role_id: "role-1".to_string(),
                permissions: Default::default(),
            }),
        }
    }

    async fn call(ctx: &HandlerContext, payload: TentaQuantPayload) -> MessageBody {
        tentaquant_dispatch(&MessageBody::TentaQuantBody(payload), ctx)
            .await
            .expect("request succeeded")
    }

    async fn fail(ctx: &HandlerContext, payload: TentaQuantPayload) -> ProtocolError {
        tentaquant_dispatch(&MessageBody::TentaQuantBody(payload), ctx)
            .await
            .expect_err("request refused")
    }

    async fn create_project(ctx: &HandlerContext, lab: &str, name: &str) -> ProjectInfo {
        match call(
            ctx,
            P::ProjectCreateRequest {
                instance_id: lab.to_string(),
                name: name.to_string(),
                description: String::new(),
                visibility: "private".to_string(),
                linked_project_id: None,
            },
        )
        .await
        {
            MessageBody::TentaQuantBody(P::ProjectResponse { project, .. }) => project,
            other => panic!("expected ProjectResponse, got {other:?}"),
        }
    }

    async fn candidates(
        ctx: &HandlerContext,
        lab: &str,
        query: &str,
        limit: u32,
    ) -> Vec<tentaflow_protocol::tentaquant::PersonCandidate> {
        match call(
            ctx,
            P::PeopleCandidatesRequest {
                instance_id: lab.to_string(),
                query: query.to_string(),
                limit,
            },
        )
        .await
        {
            MessageBody::TentaQuantBody(P::PeopleCandidatesResponse { people, .. }) => people,
            other => panic!("expected PeopleCandidatesResponse, got {other:?}"),
        }
    }

    async fn projects(ctx: &HandlerContext, lab: &str) -> Vec<ProjectInfo> {
        match call(
            ctx,
            P::ProjectListRequest {
                instance_id: lab.to_string(),
                include_archived: false,
            },
        )
        .await
        {
            MessageBody::TentaQuantBody(P::ProjectListResponse { projects, .. }) => projects,
            other => panic!("expected ProjectListResponse, got {other:?}"),
        }
    }

    /// The whole reason the package is multi-instance: two labs are two
    /// databases and two directories. A project created in one is invisible in
    /// the other, and both files exist side by side.
    #[tokio::test]
    async fn two_labs_keep_separate_databases_and_directories() {
        let mut fx = fixture();
        let lab_a = install_lab(&mut fx, "aaaaaaaa", "anna", &[PERM_READ, PERM_RUN]);
        let lab_b = install_lab(&mut fx, "bbbbbbbb", "anna", &[PERM_READ, PERM_RUN]);
        let c = ctx(&fx, "anna");

        create_project(&c, &lab_a, "Grover 4q").await;
        assert_eq!(projects(&c, &lab_a).await.len(), 1);
        assert!(projects(&c, &lab_b).await.is_empty());

        let dir_a = crate::tentaquant::data_dir("org-test", &lab_a).expect("dir a");
        let dir_b = crate::tentaquant::data_dir("org-test", &lab_b).expect("dir b");
        assert_ne!(dir_a, dir_b);
        assert!(dir_a.join("tentaquant.db").exists());
        assert!(dir_b.join("tentaquant.db").exists());
    }

    /// The gate resolves the lab from the REQUEST, so an instance of another
    /// package is refused with the uniform unavailable answer even when the
    /// caller holds permissions there.
    #[tokio::test]
    async fn an_instance_of_another_package_is_refused() {
        let mut fx = fixture();
        install_lab(&mut fx, "cccccccc", "anna", &[PERM_READ]);
        let foreign = test_support::install_app_instance(&fx.state, "ml-studio", "ml-inst", &[]);
        test_support::grant(&fx.state, &foreign, "anna", PERM_READ);
        let c = ctx(&fx, "anna");

        let denied = fail(
            &c,
            P::LabOverviewRequest {
                instance_id: foreign.clone(),
            },
        )
        .await;
        assert_eq!(denied.code, ProtocolErrorCode::AppUnavailable);
        let missing = fail(
            &c,
            P::LabOverviewRequest {
                instance_id: "tentaquant-nosuchid".to_string(),
            },
        )
        .await;
        assert_eq!(missing.message, denied.message);
    }

    /// Every way the gate can refuse ENTRY to a laboratory has to look the SAME
    /// on the wire. The platform gate answers `PolicyDenied` for a permission
    /// the matrix withholds and `AppUnavailable` for an instance that is
    /// missing, disabled or another package's; a caller able to tell those
    /// apart has learned that a lab it may not see exists. What a member may
    /// then DO is a different question, and an honest `PolicyDenied` —
    /// `the_people_of_a_lab_are_readable_only_with_instruct` pins that half.
    #[tokio::test]
    async fn every_refusal_of_a_lab_is_one_indistinguishable_answer() {
        let mut fx = fixture();
        let anna = account(&fx, "anna");
        let denied = install_lab(&mut fx, "90909090", &anna, &[]);
        let disabled = install_lab(&mut fx, "a0a0a0a0", &anna, &[]);
        // The matrix withholds `quant.read` in one lab, beating the manifest
        // default that would otherwise admit the whole organization.
        test_support::set_permission(&fx.state, &denied, "user", &anna, PERM_READ, "deny");
        // The other is installed but switched off.
        {
            let conn = fx.state.db.write().unwrap();
            conn.execute(
                "UPDATE addons SET is_enabled = 0 WHERE addon_id = ?1",
                rusqlite::params![disabled],
            )
            .expect("disable the instance");
        }
        let foreign = test_support::install_app_instance(&fx.state, "ml-studio", "ml-inst", &[]);
        let c = ctx(&fx, &anna);

        let mut answers = Vec::new();
        for instance in [
            denied.as_str(),
            disabled.as_str(),
            foreign.as_str(),
            "tentaquant-nosuchid",
        ] {
            let refused = fail(
                &c,
                P::LabOverviewRequest {
                    instance_id: instance.to_string(),
                },
            )
            .await;
            answers.push((instance.to_string(), refused.code, refused.message));
        }
        for (instance, code, message) in &answers {
            assert_eq!(
                *code,
                ProtocolErrorCode::AppUnavailable,
                "{instance} answered with {code:?}"
            );
            assert_eq!(
                message, &answers[0].2,
                "{instance} answers differently from {}",
                answers[0].0
            );
        }
    }

    /// The uniform refusal is a NON-ADMIN rule. `app_gate::unavailable` gives an
    /// administrator the real cause so they can tell "no such lab" from "that
    /// lab is switched off" while debugging their own installation, and this
    /// family must forward that instead of flattening it — there is nothing to
    /// hide from someone who may edit the instance table.
    #[tokio::test]
    async fn an_admin_still_reads_why_a_lab_is_unavailable() {
        let mut fx = fixture();
        let anna = account(&fx, "anna");
        let disabled = install_lab(&mut fx, "c0c0c0c0", &anna, &[]);
        {
            let conn = fx.state.db.write().unwrap();
            conn.execute(
                "UPDATE addons SET is_enabled = 0 WHERE addon_id = ?1",
                rusqlite::params![disabled],
            )
            .expect("disable the instance");
        }
        let mut admin = ctx(&fx, &anna);
        admin.session = SessionAuth::UserSession {
            user_id: [7u8; 16],
            role: Some("admin".to_string()),
        };

        let off = fail(
            &admin,
            P::LabOverviewRequest {
                instance_id: disabled.clone(),
            },
        )
        .await;
        let missing = fail(
            &admin,
            P::LabOverviewRequest {
                instance_id: "tentaquant-nosuchid".to_string(),
            },
        )
        .await;
        assert_eq!(off.code, ProtocolErrorCode::AppUnavailable);
        assert_eq!(missing.code, ProtocolErrorCode::AppUnavailable);
        assert!(
            off.message.contains("disabled"),
            "admin lost the disabled reason: {}",
            off.message
        );
        assert!(
            missing.message.contains("not installed"),
            "admin lost the missing reason: {}",
            missing.message
        );
        assert_ne!(off.message, missing.message);
    }

    /// Handing a PRIVATE project away leaves the former owner with nothing, and
    /// every answer has to agree on that: the response may not name a role, the
    /// project is gone from their list, and a get is the uniform NotFound.
    #[tokio::test]
    async fn transferring_a_private_project_leaves_its_former_owner_no_role() {
        let mut fx = fixture();
        let lab = install_lab(&mut fx, "b0b0b0b0", "anna", &[PERM_READ, PERM_RUN]);
        let anna = ctx(&fx, "anna");
        let marek = ctx(&fx, "marek");
        let project = create_project(&anna, &lab, "Oddane").await;

        let handed = match call(
            &anna,
            P::ProjectTransferRequest {
                instance_id: lab.clone(),
                project_id: project.project_id.clone(),
                new_owner_user_id: "marek".to_string(),
            },
        )
        .await
        {
            MessageBody::TentaQuantBody(P::ProjectResponse { project, .. }) => project,
            other => panic!("expected ProjectResponse, got {other:?}"),
        };
        assert_eq!(handed.owner_user_id, "marek");
        assert_eq!(handed.my_role, "none");

        // What the answer says is what every later request does.
        assert!(projects(&anna, &lab).await.is_empty());
        let gone = fail(
            &anna,
            P::ProjectGetRequest {
                instance_id: lab.clone(),
                project_id: project.project_id.clone(),
            },
        )
        .await;
        assert_eq!(gone.code, ProtocolErrorCode::NotFound);

        // The new owner has the project, as its owner.
        let theirs = projects(&marek, &lab).await;
        assert_eq!(theirs.len(), 1);
        assert_eq!(theirs[0].my_role, "owner");
    }

    /// A person in the lab but not in the project must not be able to tell a
    /// private project from one that does not exist.
    #[tokio::test]
    async fn a_non_member_gets_the_same_not_found_as_a_missing_project() {
        let mut fx = fixture();
        let lab = install_lab(&mut fx, "dddddddd", "anna", &[PERM_READ, PERM_RUN]);
        test_support::grant(&fx.state, &lab, "marek", PERM_READ);
        test_support::grant(&fx.state, &lab, "marek", PERM_RUN);
        let anna = ctx(&fx, "anna");
        let marek = ctx(&fx, "marek");

        let project = create_project(&anna, &lab, "Prywatny").await;
        assert!(projects(&marek, &lab).await.is_empty());

        let hidden = fail(
            &marek,
            P::ProjectGetRequest {
                instance_id: lab.clone(),
                project_id: project.project_id.clone(),
            },
        )
        .await;
        let absent = fail(
            &marek,
            P::ProjectGetRequest {
                instance_id: lab.clone(),
                project_id: "no-such-project".to_string(),
            },
        )
        .await;
        assert_eq!(hidden.code, ProtocolErrorCode::NotFound);
        assert_eq!(hidden.message, absent.message);
    }

    /// A `viewer` reads and may run in the browser; every write is refused
    /// (§10.3), including through a notebook the owner shared.
    #[tokio::test]
    async fn a_viewer_may_read_but_never_write() {
        let mut fx = fixture();
        let lab = install_lab(&mut fx, "eeeeeeee", "anna", &[PERM_READ, PERM_RUN]);
        test_support::grant(&fx.state, &lab, "marek", PERM_READ);
        test_support::grant(&fx.state, &lab, "marek", PERM_RUN);
        let anna = ctx(&fx, "anna");
        let marek = ctx(&fx, "marek");

        let project = create_project(&anna, &lab, "Wspolny").await;
        call(
            &anna,
            P::ProjectShareSetRequest {
                instance_id: lab.clone(),
                project_id: project.project_id.clone(),
                user_id: "marek".to_string(),
                role: "viewer".to_string(),
            },
        )
        .await;

        let notebook = match call(
            &anna,
            P::NotebookCreateRequest {
                instance_id: lab.clone(),
                project_id: project.project_id.clone(),
                name: "Bell".to_string(),
                cells_json: String::new(),
            },
        )
        .await
        {
            MessageBody::TentaQuantBody(P::NotebookResponse { notebook, .. }) => notebook,
            other => panic!("expected NotebookResponse, got {other:?}"),
        };

        // Reading is fine.
        assert_eq!(projects(&marek, &lab).await.len(), 1);
        call(
            &marek,
            P::NotebookGetRequest {
                instance_id: lab.clone(),
                project_id: project.project_id.clone(),
                notebook_id: notebook.notebook_id.clone(),
                version: None,
            },
        )
        .await;

        // Writing is not.
        let denied = fail(
            &marek,
            P::NotebookSaveRequest {
                instance_id: lab.clone(),
                project_id: project.project_id.clone(),
                notebook_id: notebook.notebook_id.clone(),
                cells_json: "[]".to_string(),
                expected_version: notebook.current_version,
            },
        )
        .await;
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);

        let denied = fail(
            &marek,
            P::ProjectUpdateRequest {
                instance_id: lab.clone(),
                project_id: project.project_id.clone(),
                name: "Przejete".to_string(),
                description: String::new(),
                visibility: "private".to_string(),
                linked_project_id: None,
            },
        )
        .await;
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);
    }

    /// Two editors holding the same version: the second save is a Conflict on
    /// the wire, not a silent overwrite.
    #[tokio::test]
    async fn a_stale_expected_version_is_a_conflict() {
        let mut fx = fixture();
        let lab = install_lab(&mut fx, "ffffffff", "anna", &[PERM_READ, PERM_RUN]);
        let anna = ctx(&fx, "anna");
        let project = create_project(&anna, &lab, "Notatki").await;
        let notebook = match call(
            &anna,
            P::NotebookCreateRequest {
                instance_id: lab.clone(),
                project_id: project.project_id.clone(),
                name: "Bell".to_string(),
                cells_json: "[]".to_string(),
            },
        )
        .await
        {
            MessageBody::TentaQuantBody(P::NotebookResponse { notebook, .. }) => notebook,
            other => panic!("expected NotebookResponse, got {other:?}"),
        };

        let save = |version: u32, cells: &'static str| {
            let lab = lab.clone();
            let project_id = project.project_id.clone();
            let notebook_id = notebook.notebook_id.clone();
            P::NotebookSaveRequest {
                instance_id: lab,
                project_id,
                notebook_id,
                cells_json: cells.to_string(),
                expected_version: version,
            }
        };
        call(&anna, save(1, "[1]")).await;
        let conflict = fail(&anna, save(1, "[2]")).await;
        assert_eq!(conflict.code, ProtocolErrorCode::Conflict);

        // The history stayed append-only and kept the winner's content.
        match call(
            &anna,
            P::NotebookVersionsRequest {
                instance_id: lab.clone(),
                project_id: project.project_id.clone(),
                notebook_id: notebook.notebook_id.clone(),
            },
        )
        .await
        {
            MessageBody::TentaQuantBody(P::NotebookVersionsResponse { versions, .. }) => {
                assert_eq!(versions.len(), 2);
            }
            other => panic!("expected NotebookVersionsResponse, got {other:?}"),
        }
    }

    /// A notebook's backing file is not an ordinary file: `notebooks.file_id`
    /// cascades, so accepting FileDelete on it would erase the notebook and its
    /// append-only history behind a response that only mentions a file.
    #[tokio::test]
    async fn deleting_a_notebooks_file_is_refused() {
        let mut fx = fixture();
        let lab = install_lab(&mut fx, "eeeeeeee", "anna", &[PERM_READ, PERM_RUN]);
        let anna = ctx(&fx, "anna");
        let project = create_project(&anna, &lab, "Notatki").await;
        let notebook = match call(
            &anna,
            P::NotebookCreateRequest {
                instance_id: lab.clone(),
                project_id: project.project_id.clone(),
                name: "Bell".to_string(),
                cells_json: "[]".to_string(),
            },
        )
        .await
        {
            MessageBody::TentaQuantBody(P::NotebookResponse { notebook, .. }) => notebook,
            other => panic!("expected NotebookResponse, got {other:?}"),
        };

        let denied = fail(
            &anna,
            P::FileDeleteRequest {
                instance_id: lab.clone(),
                project_id: project.project_id.clone(),
                file_id: notebook.file_id.clone(),
            },
        )
        .await;
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);

        // The notebook and its history survived the attempt.
        match call(
            &anna,
            P::NotebookVersionsRequest {
                instance_id: lab.clone(),
                project_id: project.project_id.clone(),
                notebook_id: notebook.notebook_id.clone(),
            },
        )
        .await
        {
            MessageBody::TentaQuantBody(P::NotebookVersionsResponse { versions, .. }) => {
                assert_eq!(versions.len(), 1);
            }
            other => panic!("expected NotebookVersionsResponse, got {other:?}"),
        }
    }

    /// Publishing a project to the whole laboratory is a supervisor's act; an
    /// ordinary member's project stays private.
    #[tokio::test]
    async fn publishing_to_the_lab_needs_instruct() {
        let mut fx = fixture();
        let lab = install_lab(&mut fx, "10101010", "anna", &[PERM_READ, PERM_RUN]);
        let anna = ctx(&fx, "anna");

        let denied = fail(
            &anna,
            P::ProjectCreateRequest {
                instance_id: lab.clone(),
                name: "Materialy".to_string(),
                description: String::new(),
                visibility: "lab".to_string(),
                linked_project_id: None,
            },
        )
        .await;
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);

        test_support::grant(&fx.state, &lab, "anna", PERM_INSTRUCT);
        let published = create_project(&anna, &lab, "Materialy").await;
        assert_eq!(published.visibility, "private");
        match call(
            &anna,
            P::ProjectUpdateRequest {
                instance_id: lab.clone(),
                project_id: published.project_id.clone(),
                name: "Materialy".to_string(),
                description: String::new(),
                visibility: "lab".to_string(),
                linked_project_id: None,
            },
        )
        .await
        {
            MessageBody::TentaQuantBody(P::ProjectResponse { project, .. }) => {
                assert_eq!(project.visibility, "lab");
            }
            other => panic!("expected ProjectResponse, got {other:?}"),
        }
    }

    /// Membership is the matrix INTERSECTED with the instance's Visibility.
    /// With the manifest's own defaults (`quant.read`/`quant.run` = allow) the
    /// matrix admits the whole organization, so Visibility is what scopes a lab
    /// to its group — and it is a gate, not decoration: the lab is absent from
    /// the list AND every request about it answers exactly like one that is not
    /// installed.
    #[tokio::test]
    async fn a_lab_is_listed_and_entered_only_where_visibility_admits_the_caller() {
        let mut fx = fixture();
        let anna = account(&fx, "anna");
        let marek = account(&fx, "marek");
        let mine = install_lab(&mut fx, "20202020", &anna, &[]);
        let theirs = install_lab(&mut fx, "30303030", &marek, &[]);

        let group_a =
            crate::db::repository::create_group(&fx.state.db, "fizyka-3a", "").expect("group");
        crate::db::repository::add_user_to_group(&fx.state.db, &group_a, &anna)
            .expect("membership");
        crate::db::repository::seed_addon_visibility(&fx.state.db, &mine, &group_a)
            .expect("visibility");
        let group_b =
            crate::db::repository::create_group(&fx.state.db, "chemia-2b", "").expect("group");
        crate::db::repository::add_user_to_group(&fx.state.db, &group_b, &marek)
            .expect("membership");
        crate::db::repository::seed_addon_visibility(&fx.state.db, &theirs, &group_b)
            .expect("visibility");

        let c = ctx(&fx, &anna);
        match call(&c, P::LabListRequest {}).await {
            MessageBody::TentaQuantBody(P::LabListResponse { labs, .. }) => {
                let ids: Vec<&str> = labs.iter().map(|l| l.instance_id.as_str()).collect();
                assert_eq!(ids, vec![mine.as_str()]);
                // The defaults are what she holds — nobody granted her a row.
                assert!(labs[0].my_permissions.contains(&PERM_READ.to_string()));
                assert!(!labs[0].my_permissions.contains(&PERM_INSTRUCT.to_string()));
                // The headcount is the same intersection, not the org roster.
                assert_eq!(labs[0].people_count, 1);
                // A lab always reports the node that would run it.
                assert!(!labs[0].nodes.is_empty());
            }
            other => panic!("expected LabListResponse, got {other:?}"),
        }

        let refused = fail(
            &c,
            P::LabOverviewRequest {
                instance_id: theirs.clone(),
            },
        )
        .await;
        let absent = fail(
            &c,
            P::LabOverviewRequest {
                instance_id: "tentaquant-nosuchid".to_string(),
            },
        )
        .await;
        assert_eq!(refused.code, ProtocolErrorCode::AppUnavailable);
        assert_eq!(refused.message, absent.message);
    }

    /// The supervisor's view of a laboratory is `quant.instruct` alone: a
    /// member with the default permissions cannot read the roster.
    #[tokio::test]
    async fn the_people_of_a_lab_are_readable_only_with_instruct() {
        let mut fx = fixture();
        let anna = account(&fx, "anna");
        let piotr = account(&fx, "piotr");
        let lab = install_lab(&mut fx, "60606060", &piotr, &[PERM_INSTRUCT]);

        let denied = fail(
            &ctx(&fx, &anna),
            P::LabPeopleRequest {
                instance_id: lab.clone(),
            },
        )
        .await;
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);

        match call(
            &ctx(&fx, &piotr),
            P::LabPeopleRequest {
                instance_id: lab.clone(),
            },
        )
        .await
        {
            MessageBody::TentaQuantBody(P::LabPeopleResponse { people, .. }) => {
                // Both are members through the defaults; only one supervises.
                let piotrs = people
                    .iter()
                    .find(|p| p.user_id == piotr)
                    .expect("the supervisor is in the lab");
                assert!(piotrs.permissions.contains(&PERM_INSTRUCT.to_string()));
                assert!(people.iter().any(|p| p.user_id == anna));
            }
            other => panic!("expected LabPeopleResponse, got {other:?}"),
        }
    }

    /// The share picker searches every TentaFlow account of the organization,
    /// not the laboratory's roster: an owner invites people, and somebody the
    /// lab does not admit is still offered — flagged `in_lab: false`, because
    /// the share to them is stored dormant and the window has to say so. The
    /// permission it needs is plain membership, so an ordinary owner (no
    /// `quant.instruct`) gets the answer.
    #[tokio::test]
    async fn the_share_picker_finds_accounts_outside_the_laboratory() {
        let mut fx = fixture();
        let anna = account(&fx, "anna");
        let marek = account(&fx, "marek");
        let lab = install_lab(&mut fx, "70707070", &anna, &[]);

        // Only Anna's group sees the lab, so Marek is an account of the
        // organization that this laboratory does not admit.
        let group =
            crate::db::repository::create_group(&fx.state.db, "fizyka-3a", "").expect("group");
        crate::db::repository::add_user_to_group(&fx.state.db, &group, &anna).expect("membership");
        crate::db::repository::seed_addon_visibility(&fx.state.db, &lab, &group)
            .expect("visibility");

        let c = ctx(&fx, &anna);
        assert!(!holds(&c, &lab, PERM_INSTRUCT), "an ordinary member");
        let people = candidates(&c, &lab, "marek", 20).await;
        let marek_row = people
            .iter()
            .find(|p| p.user_id == marek)
            .expect("an account outside the lab is still offered");
        assert!(!marek_row.in_lab, "and is flagged as outside it");
        assert_eq!(marek_row.display_name, "marek");

        // Anna herself is in the lab, and the search reads the login as well as
        // the display name.
        let mine = candidates(&c, &lab, "ANN", 20).await;
        assert!(mine.iter().any(|p| p.user_id == anna && p.in_lab));

        // An empty query is not "everybody": the picker opens empty.
        assert!(candidates(&c, &lab, "   ", 20).await.is_empty());
    }

    /// The directory is behind the laboratory, not next to it: somebody the
    /// instance does not admit gets the same uniform refusal every other
    /// request of the family answers, and the limit is the server's to enforce.
    #[tokio::test]
    async fn the_share_picker_refuses_non_members_and_caps_the_answer() {
        let mut fx = fixture();
        let anna = account(&fx, "anna");
        let marek = account(&fx, "marek");
        let lab = install_lab(&mut fx, "80808080", &anna, &[]);
        let group =
            crate::db::repository::create_group(&fx.state.db, "fizyka-3a", "").expect("group");
        crate::db::repository::add_user_to_group(&fx.state.db, &group, &anna).expect("membership");
        crate::db::repository::seed_addon_visibility(&fx.state.db, &lab, &group)
            .expect("visibility");

        let refused = fail(
            &ctx(&fx, &marek),
            P::PeopleCandidatesRequest {
                instance_id: lab.clone(),
                query: "anna".to_string(),
                limit: 20,
            },
        )
        .await;
        let absent = fail(
            &ctx(&fx, &marek),
            P::PeopleCandidatesRequest {
                instance_id: "tentaquant-nosuchid".to_string(),
                query: "anna".to_string(),
                limit: 20,
            },
        )
        .await;
        assert_eq!(refused.code, ProtocolErrorCode::AppUnavailable);
        assert_eq!(refused.message, absent.message);

        // Several accounts share the prefix; the requested limit bounds the
        // answer and the server clamps anything above its own ceiling.
        for i in 0..4 {
            account(&fx, &format!("tester{i}"));
        }
        let c = ctx(&fx, &anna);
        assert_eq!(candidates(&c, &lab, "tester", 2).await.len(), 2);
        assert_eq!(
            candidates(&c, &lab, "tester", 0).await.len(),
            4,
            "0 means the server ceiling, not nothing"
        );
        assert_eq!(
            candidates(&c, &lab, "tester", u32::MAX).await.len(),
            4,
            "a client cannot ask for more than the ceiling"
        );
        // Ordered by display name, so a repeated search does not reshuffle.
        let names: Vec<String> = candidates(&c, &lab, "tester", 20)
            .await
            .into_iter()
            .map(|p| p.display_name)
            .collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }

    /// The settings split: a supervisor turns the ranking off, but the qubit
    /// ceilings are `quant.admin` decisions, and the admin half — isolation,
    /// retention, the trusted-native acknowledgement — is not even readable
    /// without `quant.admin`.
    #[tokio::test]
    async fn only_admin_may_read_and_change_the_laboratory_limits() {
        let mut fx = fixture();
        let lab = install_lab(&mut fx, "40404040", "piotr", &[PERM_READ, PERM_INSTRUCT]);
        let piotr = ctx(&fx, "piotr");

        let mut settings = match call(
            &piotr,
            P::SettingsGetRequest {
                instance_id: lab.clone(),
            },
        )
        .await
        {
            MessageBody::TentaQuantBody(P::SettingsResponse {
                settings, admin, ..
            }) => {
                // The ceilings the `device="auto"` rule needs are readable...
                assert_eq!(settings.max_qubits_core, 28);
                // ...the lab's isolation posture is not.
                assert!(admin.is_none());
                settings
            }
            other => panic!("expected SettingsResponse, got {other:?}"),
        };
        assert!(settings.ranking_enabled);

        settings.ranking_enabled = false;
        match call(
            &piotr,
            P::SettingsSetRequest {
                instance_id: lab.clone(),
                settings: settings.clone(),
                admin: None,
            },
        )
        .await
        {
            MessageBody::TentaQuantBody(P::SettingsResponse { settings, .. }) => {
                assert!(!settings.ranking_enabled);
            }
            other => panic!("expected SettingsResponse, got {other:?}"),
        }

        // Neither a ceiling nor the admin half moves without `quant.admin`.
        let mut raised = settings.clone();
        raised.max_qubits_gpu = 33;
        let denied = fail(
            &piotr,
            P::SettingsSetRequest {
                instance_id: lab.clone(),
                settings: raised.clone(),
                admin: None,
            },
        )
        .await;
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);

        let denied = fail(
            &piotr,
            P::SettingsSetRequest {
                instance_id: lab.clone(),
                settings: settings.clone(),
                admin: Some(LabAdminSettings {
                    isolation_mode: "trusted_native".to_string(),
                    retention_days: 30,
                    trusted_native_ack: Some("piotr 2026-09-04".to_string()),
                }),
            },
        )
        .await;
        assert_eq!(denied.code, ProtocolErrorCode::PolicyDenied);

        test_support::grant(&fx.state, &lab, "piotr", PERM_ADMIN);
        match call(
            &piotr,
            P::SettingsSetRequest {
                instance_id: lab.clone(),
                settings: raised,
                admin: Some(LabAdminSettings {
                    isolation_mode: "trusted_native".to_string(),
                    retention_days: 30,
                    trusted_native_ack: Some("piotr 2026-09-04".to_string()),
                }),
            },
        )
        .await
        {
            MessageBody::TentaQuantBody(P::SettingsResponse {
                settings, admin, ..
            }) => {
                assert_eq!(settings.max_qubits_gpu, 33);
                let admin = admin.expect("admin half readable with quant.admin");
                assert_eq!(admin.isolation_mode, "trusted_native");
                assert_eq!(admin.retention_days, 30);
            }
            other => panic!("expected SettingsResponse, got {other:?}"),
        }
    }

    /// A file arrives in 4 MiB chunks and lands in the lab's own content store
    /// under its sha256 — inside THIS instance's directory, nowhere else.
    #[tokio::test]
    async fn an_uploaded_file_lands_in_this_labs_content_store() {
        let mut fx = fixture();
        let lab = install_lab(&mut fx, "50505050", "anna", &[PERM_READ, PERM_RUN]);
        let anna = ctx(&fx, "anna");
        let project = create_project(&anna, &lab, "Dane").await;

        let chunk = |seq: u32, bytes: Vec<u8>| P::FileUploadChunkRequest {
            instance_id: lab.clone(),
            project_id: project.project_id.clone(),
            upload_id: "up-1".to_string(),
            path: "data/counts.json".to_string(),
            kind: "data".to_string(),
            seq,
            total_chunks: 2,
            bytes,
        };
        call(&anna, chunk(0, b"{\"00\":".to_vec())).await;
        let file = match call(&anna, chunk(1, b"512}".to_vec())).await {
            MessageBody::TentaQuantBody(P::FileUploadChunkResponse { file, complete, .. }) => {
                assert!(complete);
                file.expect("the final chunk carries the stored file")
            }
            other => panic!("expected FileUploadChunkResponse, got {other:?}"),
        };
        assert_eq!(file.size_bytes, 10);

        let dir = crate::tentaquant::data_dir("org-test", &lab).expect("dir");
        let blob = cas::blob_path(&dir, &file.sha256);
        assert_eq!(std::fs::read(&blob).unwrap(), b"{\"00\":512}");

        match call(
            &anna,
            P::FileListRequest {
                instance_id: lab.clone(),
                project_id: project.project_id.clone(),
            },
        )
        .await
        {
            MessageBody::TentaQuantBody(P::FileListResponse { files, .. }) => {
                // The notebook path and the uploaded file both live here.
                assert!(files.iter().any(|f| f.path == "data/counts.json"));
            }
            other => panic!("expected FileListResponse, got {other:?}"),
        }
    }

    /// Uninstalling one laboratory must take that laboratory and nothing else.
    /// The test lives here because only this fixture installs REAL instances:
    /// it drives the platform's sequence — `native_teardown` closes the pool,
    /// the platform then removes the planned path — on lab A and shows lab B
    /// still open, still holding its content, and A's pool gone rather than
    /// left serving a deleted file.
    #[tokio::test]
    async fn tearing_down_one_lab_leaves_the_other_untouched() {
        let mut fx = fixture();
        let lab_a = install_lab(&mut fx, "70707070", "anna", &[]);
        let lab_b = install_lab(&mut fx, "80808080", "anna", &[]);
        let c = ctx(&fx, "anna");
        create_project(&c, &lab_a, "Znika").await;
        create_project(&c, &lab_b, "Zostaje").await;

        let dir_a = crate::tentaquant::data_dir("org-test", &lab_a).expect("dir a");
        let dir_b = crate::tentaquant::data_dir("org-test", &lab_b).expect("dir b");
        let plan_ctx = crate::addon::native_apps::NativeAppContext {
            db: &fx.state.db,
            addon_id: &lab_a,
            org_id: "org-test",
            data_dir: dir_a.clone(),
        };
        let plan = crate::tentaquant::native_teardown_plan(&plan_ctx).expect("plan");
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].path, dir_a);
        crate::tentaquant::native_teardown(&plan_ctx).expect("teardown");
        // What the platform does with the plan, done here so the assertions
        // below describe the state a real uninstall leaves behind.
        std::fs::remove_dir_all(&dir_a).expect("wipe lab a");

        assert!(!dir_a.exists());
        assert!(dir_b.join("tentaquant.db").exists());
        let still_there = projects(&c, &lab_b).await;
        assert_eq!(still_there.len(), 1);
        assert_eq!(still_there[0].name, "Zostaje");

        // Lab A's pool was dropped, not left pointing at the removed file: the
        // next request builds a new, empty database instead of reading the old
        // one through a stale handle.
        assert!(projects(&c, &lab_a).await.is_empty());
    }

    /// Names of every request variant this dispatcher must serve, read out of
    /// the PROTOCOL SOURCE rather than from a list kept next to the
    /// registrations — a list policing itself cannot notice a variant missing
    /// from both, which is exactly how a request type goes unreachable.
    fn dispatched_request_variants() -> Vec<String> {
        const PROTOCOL_SRC: &str = include_str!("../../../tentaflow-protocol/src/tentaquant.rs");
        let body = PROTOCOL_SRC
            .split_once("pub enum TentaQuantPayload {")
            .expect("TentaQuantPayload enum")
            .1;
        let mut variants: Vec<String> = Vec::new();
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
            let name = rest
                .split(|c: char| !c.is_ascii_alphanumeric())
                .next()
                .unwrap_or_default();
            if name.ends_with("Request") {
                variants.push(format!("TentaQuant{name}"));
            }
        }
        variants
    }

    /// A handler that is written but not registered is invisible to
    /// `dispatch::find`, and the client sees NotImplemented for a request the
    /// server can actually answer.
    #[test]
    fn every_request_variant_resolves_to_a_handler() {
        let names: HashSet<String> = dispatched_request_variants().into_iter().collect();
        assert!(
            names.len() >= 20,
            "the enum scan found only {}",
            names.len()
        );
        for registered in names {
            let handler = crate::dispatch::find(&registered)
                .unwrap_or_else(|| panic!("{registered} has no registered handler"));
            assert_eq!(
                handler.required_auth,
                crate::dispatch::SessionAuthKind::UserSession,
                "{registered} must stay at UserSession — the lab matrix is the gate"
            );
        }
    }
}
