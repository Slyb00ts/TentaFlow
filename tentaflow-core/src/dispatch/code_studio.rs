// =============================================================================
// File: dispatch/code_studio.rs
// Purpose: Binary API handlers of Code Studio — the workspace registry
//          (create, list, detail, retry, archive, delete, settings, secret),
//          members and the org-level create grant, the workspace allowlist,
//          work sessions (list, open, close, autonomy) and the read-only git
//          views the owner node can serve today. Sessions are private per user
//          and there is no administrator bypass; an org administrator sees
//          METADATA and may administer a workspace's lifecycle, but reaching
//          its content requires joining `code_workspace_members`, which is an
//          audited, owner-visible act (§25.4).
// Example: CodeStudioPayload::WorkspacesListRequest → WorkspacesListResponse.
// =============================================================================

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use tentaflow_macros::{handler, observed, policy};
use tentaflow_protocol::code_studio::{
    AgentCredentialInfo, AllowlistEntryInfo, ApprovalInfo, CodeStudioPayload, DiffHunkInfo,
    FileEntryInfo, GitBranchInfo, GitCommitInfo, GitStatusEntry, GrantInfo, GrepHitInfo,
    OperationInfo,
    CodeSearchHit, IndexStateInfo, PatchFileDecision, PatchFileInfo, PatchHunkInfo, PatchSetInfo,
    ProjectLinkInfo, ProvisionStepInfo, RepoEntryInfo, RunInfo, SessionInfo, TaskInfo,
    TerminalCellRow,
    TimelineEventInfo, WorkspaceInfo, WorkspaceMemberInfo, WorkspaceMemberInput,
    WorkspaceNodeInfo, WorkspaceUserCandidate, WorktreeInfo,
};
use tentaflow_protocol::{MessageBody, ProtocolError, ProtocolErrorCode};

use super::HandlerContext;
use rusqlite::OptionalExtension;
use crate::agents::{InteractionReply, PermissionDecision};
use crate::code_studio::egress;
use crate::code_studio::events::{EventPayload, GitOperation, SessionEvent};
use crate::code_studio::exec::{ExecEnv, ExecRequest, Executor, NullSink, Program};
use crate::code_studio::fs::{GrepQuery, LineRange, RelPath, SessionRoot};
use crate::code_studio::git_broker::{
    record_session_head, Broker, CommitIdentity, GitAuth, MergeOutcome, RepoHandle,
};
use crate::code_studio::models::{
    AutonomyMode, EgressEnforcement, ExecMode, NewWorkspace, WorkspaceRecord, WorkspaceRole,
    WorkspaceStatus,
};
use crate::code_studio::operations::{
    OpKind, Operation, OperationInput, OperationRequest, OperationStatus, OriginKind, Postcondition,
    UnknownDecision,
};
use crate::code_studio::patch::{CommitRequest, Decisions, EditKind, FileVerdict, PatchScope};
use crate::code_studio::pep::{
    self, AskKind, Capability, Decision, MountAccess, NetworkAccess, SessionCtx, Target,
};
use crate::code_studio::provisioning::ProvisionAuth;
use crate::code_studio::sandbox::{
    ContainerConfig, ContainerRuntime, Lease, SandboxError, SandboxManager,
};
use crate::code_studio::session::{NewSession, SessionRecord};
use crate::code_studio::terminal::{Cell, Color, PtyHandle, PtyOpen, TerminalRegistry};
use crate::code_studio::vault::{self, SecretKind, VaultError};
use crate::code_studio::{
    artifacts, events, fs as cs_fs, index, operations, patch, paths, project_link, provisioning,
    redact, remote_proxy, repository, sandbox, session, tools, workspace_db,
};
use crate::db::seed::CODE_HARNESS_CRITIC_FLOW_ID;
use crate::db::DbPool;
use crate::services::rbac::OrgContext;

const PERM_READ: &str = "code_studio.read";
const PERM_ADMIN: &str = "code_studio.admin";

/// Capabilities that are administration of the workspace rather than something
/// a session does. They pass `may_store_always_grant`, but a standing grant for
/// them would be meaningless: no session operation ever asks for them.
const NON_SESSION_CAPABILITIES: &[Capability] =
    &[Capability::WorkspaceSettings, Capability::MemberManage];

/// Whether a workspace-level `always` entry may cover this capability.
///
/// The rule itself belongs to the PEP (`may_store_always_grant` refuses system
/// and mandatory-interactive capabilities); this only strips the two that are
/// not session capabilities at all. Enforced at WRITE time, not merely ignored
/// on read — §9.3 rule 5.
fn is_allowlistable(cap: Capability) -> bool {
    pep::may_store_always_grant(cap) && !NON_SESSION_CAPABILITIES.contains(&cap)
}

// =============================================================================
// Errors and authorization
// =============================================================================

fn cs(body: CodeStudioPayload) -> MessageBody {
    MessageBody::CodeStudioBody(body)
}

fn db_error(scope: &str, error: anyhow::Error) -> ProtocolError {
    tracing::warn!(scope, error = %error, "code studio database error");
    ProtocolError::internal("code studio database error")
}

fn vault_error(scope: &str, error: VaultError) -> ProtocolError {
    match error {
        // The caller has to be able to show this: the workspace is fine, the
        // key simply is not on this node.
        VaultError::SecretMissing(_) | VaultError::CredentialMissing { .. } => {
            ProtocolError::new(ProtocolErrorCode::NotAvailable, error.to_string())
        }
        VaultError::Invalid(message) => ProtocolError::bad_request(message),
        other => {
            tracing::warn!(scope, error = %other, "code studio vault error");
            ProtocolError::internal("code studio vault error")
        }
    }
}

/// Uniform answer for "you may not see this": a non-member must not be able to
/// tell someone else's workspace from one that does not exist.
fn not_found() -> ProtocolError {
    ProtocolError::not_found("workspace not found")
}

fn require_org(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    ctx.org_context
        .as_ref()
        .ok_or_else(|| ProtocolError::new(ProtocolErrorCode::AuthRequired, "org context required"))
}

fn require_read(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    let org = require_org(ctx)?;
    if !org.has(PERM_READ) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "code_studio.read permission required",
        ));
    }
    Ok(org)
}

fn require_admin(ctx: &HandlerContext) -> Result<&OrgContext, ProtocolError> {
    let org = require_org(ctx)?;
    if !org.has(PERM_ADMIN) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "code_studio.admin permission required",
        ));
    }
    Ok(org)
}

fn is_admin(org: &OrgContext) -> bool {
    org.has(PERM_ADMIN)
}

/// What the caller must be able to do. The split exists because §25.4 gives an
/// org administrator a REAL but narrow overlay: metadata and lifecycle, never
/// content. Encoding that as an enum keeps every handler from re-deciding it.
#[derive(Debug, Clone, Copy)]
enum Access {
    /// Registry metadata: list row, detail, provisioning steps. Any membership,
    /// or `code_studio.admin` without membership.
    Metadata,
    /// Membership at this role or higher, with NO administrator override. Used
    /// for everything that touches content, credentials or sessions.
    Member(WorkspaceRole),
    /// Archive, delete, quotas. Owner membership, or `code_studio.admin`.
    Lifecycle,
}

/// Loads the workspace and enforces the access gate.
///
/// Returns the record plus the caller's membership role — `None` means the
/// administrator overlay applied, which every caller must treat as "metadata
/// only". A non-member without the overlay always gets NotFound.
fn require_workspace(
    ctx: &HandlerContext,
    org: &OrgContext,
    workspace_id: &str,
    access: Access,
) -> Result<(WorkspaceRecord, Option<WorkspaceRole>), ProtocolError> {
    paths::validate_workspace_id(workspace_id)
        .map_err(|_| ProtocolError::bad_request("invalid workspace_id"))?;
    let db = &ctx.state.db;
    let record = repository::get_workspace(db, workspace_id)
        .map_err(|e| db_error("get_workspace", e))?
        .ok_or_else(not_found)?;
    if record.org_id != org.org_id || record.status == WorkspaceStatus::Deleted.slug() {
        return Err(not_found());
    }
    let role = repository::role_of(db, workspace_id, &org.user_id)
        .map_err(|e| db_error("role_of", e))?;
    let min = match access {
        Access::Metadata => WorkspaceRole::Viewer,
        Access::Member(required) => required,
        Access::Lifecycle => WorkspaceRole::Owner,
    };
    match role {
        Some(actual) if actual >= min => Ok((record, Some(actual))),
        Some(_) if matches!(access, Access::Lifecycle) && is_admin(org) => Ok((record, role)),
        Some(_) => Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            format!("requires workspace role '{}' or higher", min.slug()),
        )),
        None if is_admin(org) && !matches!(access, Access::Member(_)) => Ok((record, None)),
        None => Err(not_found()),
    }
}

/// A workspace runs only on the node that owns it.
///
/// A request that names a remote workspace is forwarded by the dispatcher
/// before any handler sees it (`route_to_owner`), so reaching this check with a
/// foreign record means the forward was not possible — no mesh, no trust, or a
/// call whose owner-side twin does not exist. The message names the node that
/// can serve it either way.
fn require_local(ctx: &HandlerContext, record: &WorkspaceRecord) -> Result<(), ProtocolError> {
    if record.node_id.as_str() != &*ctx.state.local_node_id {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            format!(
                "workspace is owned by node '{}', which is not this node",
                record.node_id
            ),
        ));
    }
    Ok(())
}

fn require_active(record: &WorkspaceRecord) -> Result<(), ProtocolError> {
    if record.status != WorkspaceStatus::Active.slug() {
        return Err(ProtocolError::new(
            ProtocolErrorCode::Conflict,
            format!("workspace is '{}', not active", record.status),
        ));
    }
    Ok(())
}

fn open_workspace_pool(record: &WorkspaceRecord) -> Result<DbPool, ProtocolError> {
    workspace_db::open(&record.id).map_err(|e| db_error("workspace_db.open", e))
}

/// Loads a session and refuses anyone else's. Sessions are private per user
/// (§5.3) — an administrator gets the same NotFound as a stranger, because a
/// session holds the person's unfinished work and their conversation with the
/// agent.
fn require_own_session(
    pool: &DbPool,
    session_id: &str,
    user_id: &str,
) -> Result<SessionRecord, ProtocolError> {
    paths::validate_session_id(session_id)
        .map_err(|_| ProtocolError::bad_request("invalid session_id"))?;
    let record = session::get_session(pool, session_id)
        .map_err(|e| db_error("get_session", e))?
        .ok_or_else(|| ProtocolError::not_found("session not found"))?;
    if record.user_id != user_id {
        return Err(ProtocolError::not_found("session not found"));
    }
    Ok(record)
}

/// Writes one audit entry into the hash-chained `audit_log` of the main
/// database. Best-effort by design for everything except workspace creation,
/// which writes its event BEFORE the row exists precisely so an unaudited
/// workspace cannot come into being (§7.1, §24).
fn audit(ctx: &HandlerContext, action: &str, resource: &str, details: &serde_json::Value) {
    let user_id = ctx.org_context.as_ref().map(|org| org.user_id.clone());
    if let Err(error) = crate::db::repository::log_audit(
        &ctx.state.db,
        user_id.as_deref(),
        None,
        action,
        Some(resource),
        Some(&details.to_string()),
        None,
        Some(&ctx.state.local_node_id),
    ) {
        tracing::warn!(action, resource, error = %error, "code studio audit write failed");
    }
}

// =============================================================================
// Node capabilities and server-side policy resolution (§9.5)
// =============================================================================

/// Node capabilities, probed once. `NodeCapabilities::probe` reads the
/// container socket and the firewall ruleset, so it is not something a list
/// request may do per workspace; neither answer can change without a restart.
fn node_capabilities() -> egress::NodeCapabilities {
    static CAPS: OnceLock<egress::NodeCapabilities> = OnceLock::new();
    *CAPS.get_or_init(egress::NodeCapabilities::probe)
}

/// Whether this node can actually run a container-isolated workspace. Both
/// halves matter: a build without the `docker` feature has no sandbox backend
/// at all, and a node whose runtime socket does not answer cannot keep the
/// promise either.
fn node_supports_container(_ctx: &HandlerContext) -> bool {
    cfg!(feature = "docker") && node_capabilities().container_runtime
}

/// How network policy is REALLY enforced here (§7.6). Computed from the node,
/// never accepted from the wire — a client that could name its own enforcement
/// could promise filtering the node cannot perform.
fn resolve_egress_enforcement(exec_mode: ExecMode) -> EgressEnforcement {
    egress::detect_enforcement(exec_mode, &node_capabilities())
}

/// The combinations §9.5 refuses, enforced here rather than in the wizard: the
/// binary protocol is reachable without the UI, so hiding an option is not
/// validation. The rules themselves live in `egress::validate_policy`, which is
/// also what the egress gateway consults — one statement of the policy, two
/// callers.
fn validate_workspace_policy(
    exec_mode: ExecMode,
    container_image: Option<&str>,
    autonomy_ceiling: AutonomyMode,
    egress_policy: &str,
    enforcement: EgressEnforcement,
) -> Result<(), ProtocolError> {
    let policy = egress::EgressPolicy::from_slug(egress_policy).ok_or_else(|| {
        ProtocolError::bad_request(format!("unknown egress policy '{egress_policy}'"))
    })?;
    egress::validate_policy(
        exec_mode,
        enforcement,
        autonomy_ceiling,
        policy,
        container_image,
    )
    .map_err(|e| ProtocolError::bad_request(format!("{e:#}")))
}

fn parse_exec_mode(raw: &str) -> Result<(ExecMode, bool), ProtocolError> {
    if raw.trim().is_empty() {
        // §7.1: omitting the field is never an invisible choice — the caller
        // records the resolved mode in the audit event.
        return Ok((ExecMode::TrustedNative, true));
    }
    ExecMode::from_slug(raw.trim())
        .map(|mode| (mode, false))
        .ok_or_else(|| ProtocolError::bad_request(format!("unknown exec mode '{raw}'")))
}

fn parse_autonomy(raw: &str) -> Result<AutonomyMode, ProtocolError> {
    AutonomyMode::from_slug(raw.trim())
        .ok_or_else(|| ProtocolError::bad_request(format!("unknown autonomy mode '{raw}'")))
}

fn parse_role(raw: &str) -> Result<WorkspaceRole, ProtocolError> {
    WorkspaceRole::from_slug(raw.trim())
        .ok_or_else(|| ProtocolError::bad_request(format!("unknown workspace role '{raw}'")))
}

/// Directory-safe, human-readable name fragment. The workspace directory is
/// derived from the id, never from this, so the slug only has to be unique per
/// (org, owner).
fn slugify(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let mut slug = String::new();
    for part in cleaned.split('-').filter(|p| !p.is_empty()) {
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(part);
    }
    if slug.is_empty() {
        "workspace".to_string()
    } else {
        slug.chars().take(48).collect()
    }
}

// =============================================================================
// Wire mapping
// =============================================================================

fn node_name(ctx: &HandlerContext, node_id: &str) -> String {
    ctx.state
        .mesh_peer_store
        .get_hostname(node_id)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| node_id.to_string())
}

fn local_node_info(ctx: &HandlerContext) -> WorkspaceNodeInfo {
    let node_id = ctx.state.local_node_id.to_string();
    node_info(ctx, node_id, node_supports_container(ctx))
}

/// One entry of the node picker.
///
/// `egress_enforcement` is what the node WOULD use, derived from whether it can
/// isolate a workspace at all. For a remote node that is necessarily
/// provisional: this node cannot probe another node's firewall, and §7.6 makes
/// the OWNER node compute the authoritative value while it provisions. The
/// picker therefore shows an expectation, and `WorkspaceInfo` returned after
/// creation carries the measured one.
fn node_info(ctx: &HandlerContext, node_id: String, supports_container: bool) -> WorkspaceNodeInfo {
    WorkspaceNodeInfo {
        name: node_name(ctx, &node_id),
        is_local: node_id.as_str() == &*ctx.state.local_node_id,
        node_id,
        supports_container,
        egress_enforcement: if supports_container {
            EgressEnforcement::Namespace.slug().to_string()
        } else {
            EgressEnforcement::Unrestricted.slug().to_string()
        },
    }
}

/// Nodes this caller may put a workspace on: this one, plus every TRUST-PAIRED
/// peer the mesh currently knows.
///
/// A workspace runs only on its owner node (§3), which is exactly why the node
/// has to be chosen when it is created — "set it up on the server, work on it
/// from the laptop and the phone" is the ordinary case, not an exotic one. An
/// unpaired peer never appears: without trust there is no assertion path to it,
/// so offering it would be a promise the wizard cannot keep.
fn node_catalog(ctx: &HandlerContext) -> Vec<WorkspaceNodeInfo> {
    let mut nodes = vec![local_node_info(ctx)];
    let Some(iroh) = ctx.state.quic_mesh.as_ref() else {
        return nodes;
    };
    for peer in ctx.state.mesh_peer_store.list() {
        if peer.node_id.as_str() == &*ctx.state.local_node_id || !iroh.is_trusted(&peer.node_id) {
            continue;
        }
        // The peer's own report of whether it can isolate a workspace. Taking
        // the local answer here would offer container mode on a node that
        // cannot deliver it.
        nodes.push(node_info(ctx, peer.node_id.clone(), peer.docker_available));
    }
    nodes
}

/// What the caller's own sessions in one workspace amount to.
///
/// "How many are running" and "how many are blocked on a question from me" are
/// different facts and the dashboard shows both, so they are counted in ONE
/// pass: opening the runtime pool twice per workspace would double the cost of
/// every workspace list.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct SessionCounts {
    open: u32,
    waiting_user: u32,
}

/// Sessions the CALLER has open in this workspace. Deliberately not a global
/// count: sessions are private, so the number of other people's open sessions
/// is not this user's business either.
fn open_sessions_of(
    record: &WorkspaceRecord,
    user_id: &str,
) -> Result<SessionCounts, anyhow::Error> {
    if record.status != WorkspaceStatus::Active.slug() {
        return Ok(SessionCounts::default());
    }
    let pool = workspace_db::open(&record.id)?;
    let sessions = session::list_sessions_for_user(&pool, user_id)?;
    let mut counts = SessionCounts::default();
    for record in &sessions {
        if matches!(record.status.as_str(), "closed" | "failed" | "cancelled") {
            continue;
        }
        counts.open += 1;
        if record.status == "waiting_user" {
            counts.waiting_user += 1;
        }
    }
    Ok(counts)
}

/// The same count for DISPLAY. A workspace whose runtime database cannot be
/// read shows no sessions rather than failing the whole list — but the §25.3
/// quota never uses this value, because a count that turns an unreadable
/// database into "zero open sessions" is a gate that opens on failure.
fn open_session_count(record: &WorkspaceRecord, user_id: &str) -> SessionCounts {
    match open_sessions_of(record, user_id) {
        Ok(counts) => counts,
        Err(error) => {
            tracing::warn!(workspace_id = %record.id, error = %error, "session count unavailable");
            SessionCounts::default()
        }
    }
}

/// Bytes the workspace holds, for display next to its quota.
///
/// Deliberately the SAME measurement the quota gate enforces with, rather than
/// a second walk of the same tree: a display that counts differently from the
/// gate tells the user they have room where the gate refuses, or the reverse.
/// A measurement that could not complete reports as absent, never as a smaller
/// number — a truncated sum is indistinguishable from a small workspace and
/// would read as free space that does not exist.
fn workspace_disk_usage(workspace_id: &str) -> u64 {
    match cs_fs::workspace_disk_usage(workspace_id) {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::warn!(%workspace_id, %error, "workspace size unavailable");
            0
        }
    }
}

fn workspace_to_wire(
    ctx: &HandlerContext,
    record: &WorkspaceRecord,
    my_role: Option<WorkspaceRole>,
    member_count: u32,
    sessions: SessionCounts,
    disk_used_bytes: u64,
) -> WorkspaceInfo {
    WorkspaceInfo {
        workspace_id: record.id.clone(),
        name: record.name.clone(),
        slug: record.slug.clone(),
        node_name: node_name(ctx, &record.node_id),
        is_local: record.node_id.as_str() == &*ctx.state.local_node_id,
        node_id: record.node_id.clone(),
        exec_mode: record.exec_mode.clone(),
        egress_enforcement: record.egress_enforcement.clone(),
        repo_kind: record.repo_kind.clone(),
        repo_url: record.repo_url.clone(),
        repo_auth_kind: record.repo_auth_kind.clone(),
        // The handle itself never crosses the wire; whether one exists does.
        has_secret: record.secret_ref.is_some(),
        default_branch: record.default_branch.clone(),
        target_branch: record.target_branch.clone(),
        autonomy_ceiling: record.autonomy_ceiling.clone(),
        egress_policy: record.egress_policy.clone(),
        index_enabled: record.index_enabled,
        status: record.status.clone(),
        status_detail: record.status_detail.clone(),
        my_role: my_role.map(|role| role.slug()).unwrap_or("none").to_string(),
        member_count,
        open_sessions: sessions.open,
        disk_used_bytes,
        quota_disk_bytes: record.quota_disk_bytes,
        created_at: record.created_at.clone(),
        updated_at: record.updated_at.clone(),
        quota_sessions: record.quota_sessions,
        sessions_waiting: sessions.waiting_user,
    }
}

fn members_to_wire(ctx: &HandlerContext, workspace_id: &str) -> Vec<WorkspaceMemberInfo> {
    let records = repository::list_members(&ctx.state.db, workspace_id).unwrap_or_default();
    records
        .into_iter()
        .map(|record| WorkspaceMemberInfo {
            display_name: display_name(ctx, &record.user_id),
            user_id: record.user_id,
            role: record.role,
            added_by: record.added_by,
            added_at: record.added_at,
        })
        .collect()
}

fn display_name(ctx: &HandlerContext, user_id: &str) -> String {
    crate::db::repository::get_user_account_by_id(&ctx.state.db, user_id)
        .ok()
        .flatten()
        .map(|account| {
            if account.display_name.is_empty() {
                account.username
            } else {
                account.display_name
            }
        })
        .unwrap_or_else(|| user_id.to_string())
}

fn user_exists(ctx: &HandlerContext, user_id: &str) -> bool {
    crate::db::repository::get_user_account_by_id(&ctx.state.db, user_id)
        .ok()
        .flatten()
        .is_some()
}

fn session_to_wire(record: SessionRecord) -> SessionInfo {
    SessionInfo {
        session_id: record.id,
        workspace_id: record.workspace_id,
        title: record.title,
        branch: record.branch,
        autonomy_mode: record.autonomy_mode,
        status: record.status,
        created_at: record.created_at,
        updated_at: record.updated_at,
        closed_at: record.closed_at,
        // The PINNED version, not the newest one: a session executes the shape
        // it opened with, and the UI has to be able to open exactly that.
        flow_id: record.flow_id,
        flow_version_id: record.flow_version_id,
    }
}

// =============================================================================
// Owner-node routing (§3, §12)
// =============================================================================

/// Workspace and session a request is addressed to, for the calls only the
/// node that OWNS the workspace can serve.
///
/// Registry reads and registry edits are deliberately absent: they answer from
/// the synced platform tables, so forwarding them would buy a round trip and a
/// new failure mode for data this node already holds. What is listed is exactly
/// the set whose handler reaches for the workspace's runtime database, its
/// worktree or its sandbox — the calls that end at `require_local`.
///
/// An unlisted variant falls through to the local handler, which is right for a
/// response, for a stream request (answered by `stream_handlers`) and for a
/// call that names no workspace at all.
fn route_target(payload: &CodeStudioPayload) -> Option<(&str, &str)> {
    use CodeStudioPayload as P;
    match payload {
        P::SessionCloseRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::SessionAutonomySetRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::SessionTimelineRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::SessionOperationsRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::SessionRunsRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::SessionTasksRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::SessionMessageSendRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::SessionCancelRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::SessionGrantsListRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::SessionGrantRevokeRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::OperationResolveRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::ApprovalsListRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::ApprovalDecideRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::FileTreeRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::FileReadRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::FileWriteRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::FileCreateRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::FileDeleteRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::FileRenameRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::FileMkdirRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::FileGrepRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::GitStatusRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::GitLogRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::GitBranchesRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::GitDiffRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::GitCommitRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::GitPushRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::GitSyncRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::GitMergeRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::GitMergeFinalizeRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::GitMergeAbandonRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::WorktreesListRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::PatchSetsListRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::PatchSetGetRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::PatchDecideRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::PatchSetAbandonRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::PatchBlobGetRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::ExecStartRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::ExecCancelRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::ExecOutputRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::TerminalOpenRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::TerminalInputRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::TerminalResizeRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::TerminalCloseRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::TerminalSnapshotRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::TerminalsListRequest {
            workspace_id,
            session_id,
            ..
        }
        | P::CodeSearchRequest {
            workspace_id,
            session_id,
            ..
        } => Some((workspace_id.as_str(), session_id.as_str())),

        P::WorkspaceRetryRequest { workspace_id, .. }
        | P::WorkspaceDeleteRequest { workspace_id, .. }
        | P::WorkspaceSecretSetRequest { workspace_id, .. }
        | P::SessionsListRequest { workspace_id, .. }
        | P::SessionOpenRequest { workspace_id, .. }
        | P::IndexStatusRequest { workspace_id, .. }
        | P::IndexRebuildRequest { workspace_id, .. }
        | P::RepoTreeRequest { workspace_id, .. } => Some((workspace_id.as_str(), "")),

        _ => None,
    }
}

/// The node a provider-credential call is about, for the calls that name one
/// instead of a workspace.
fn credential_node(payload: &CodeStudioPayload) -> Option<&str> {
    use CodeStudioPayload as P;
    match payload {
        P::AgentCredentialsListRequest { node_id }
        | P::AgentCredentialSetRequest { node_id, .. }
        | P::AgentCredentialDeleteRequest { node_id, .. } => Some(node_id.as_str()),
        _ => None,
    }
}

/// Forwards a call whose workspace lives on another node, or `None` when this
/// node owns it.
///
/// This is the ONE place the split of §3 is decided, which is why it sits in
/// the dispatcher and not in sixty handlers. `RemoteProxy` carries no decision
/// logic of its own: it mints the caller's assertion, ships the bytes, and the
/// owner node authorizes the call from scratch through this same dispatcher.
/// Forwarding is an ACTION taken for the caller — it mints an assertion, opens
/// a mesh stream and spends the owner node's time — so the caller has to be
/// entitled to it before it happens. Authorizing after the forward would make
/// the dispatcher an oracle: four distinguishable answers (unknown workspace,
/// no mesh, untrusted peer, real response) tell a stranger whether a workspace
/// exists and which node holds it, and every probe costs an outbound request.
///
/// The gate here is deliberately the WEAKEST one any handler applies —
/// `code_studio.read` plus membership, or the administrator's metadata overlay
/// — so it can never admit less than the handler that follows it. The handler
/// then authorizes from scratch, on this node or on the owner's.
async fn route_to_owner(
    payload: &CodeStudioPayload,
    ctx: &HandlerContext,
) -> Result<Option<MessageBody>, ProtocolError> {
    // A call that already travelled is executed here or refused here. Forwarding
    // it again would let two nodes with disagreeing registry rows bounce a
    // request between them until the deadline.
    if remote_proxy::current_remote_origin_id().is_some() {
        return Ok(None);
    }
    // Creating a workspace names a NODE, not a workspace: the registry row does
    // not exist yet, and provisioning — the directory, the git repository, the
    // runtime database, the authoritative `egress_enforcement` and the vault the
    // credential is sealed into — all belong to the node that will run it.
    if let CodeStudioPayload::WorkspaceCreateRequest { node_id, .. } = payload {
        let node_id = node_id.trim();
        if node_id.is_empty() || node_id == &*ctx.state.local_node_id {
            return Ok(None);
        }
        // The creator grant is the local handler's check; the permission is
        // this one's, because without it the node id off the wire would reach
        // the mesh on behalf of someone holding no Code Studio permission.
        require_read(ctx)?;
        return remote_proxy::proxy_to_node(ctx, node_id, payload)
            .await
            .map(Some);
    }
    // A provider credential names a NODE for the same reason: the material is
    // sealed with that node's `SettingsCipher` key (§5.2), so both the write
    // and the listing are facts about one vault and nowhere else. The gate here
    // is the handler's own — administering the organization's provider account
    // is not something a read permission may forward on somebody's behalf.
    if let Some(node_id) = credential_node(payload) {
        let node_id = node_id.trim();
        if node_id.is_empty() || node_id == &*ctx.state.local_node_id {
            return Ok(None);
        }
        require_admin(ctx)?;
        return remote_proxy::proxy_to_node(ctx, node_id, payload)
            .await
            .map(Some);
    }
    let Some((workspace_id, session_id)) = route_target(payload) else {
        return Ok(None);
    };
    // A malformed id is the local handler's error to report, in its own words.
    if paths::validate_workspace_id(workspace_id).is_err() {
        return Ok(None);
    }
    let org = require_read(ctx)?;
    let (record, _) = require_workspace(ctx, org, workspace_id, Access::Metadata)?;
    if record.node_id.as_str() == &*ctx.state.local_node_id {
        return Ok(None);
    }
    remote_proxy::proxy_to_owner(ctx, &record, payload, session_id)
        .await
        .map(Some)
}

// =============================================================================
// Dispatcher
// =============================================================================

#[handler(variant = "CodeStudioBody", since = (1, 0))]
#[policy(UserSession)]
#[observed]
pub async fn code_studio_dispatch(
    req: &MessageBody,
    ctx: &HandlerContext,
) -> Result<MessageBody, ProtocolError> {
    let payload = match req {
        MessageBody::CodeStudioBody(p) => p,
        _ => return Err(ProtocolError::bad_request("expected CodeStudioBody")),
    };

    // §3 — a workspace runs on its owner node and only there. Everything below
    // this line therefore acts on a LOCAL workspace.
    if let Some(response) = route_to_owner(payload, ctx).await? {
        return Ok(response);
    }

    use CodeStudioPayload as P;
    match payload {
        P::WorkspacesListRequest { include_archived } => {
            workspaces_list_v1(ctx, *include_archived)
        }
        P::WorkspaceCreateRequest {
            name,
            node_id,
            exec_mode,
            container_image,
            repo_kind,
            repo_url,
            repo_auth_kind,
            secret_material,
            ssh_host_fingerprint,
            default_branch,
            autonomy_ceiling,
            egress_policy,
            index_enabled,
            members,
        } => {
            workspace_create_v1(
                ctx,
                WorkspaceCreateInput {
                    name,
                    node_id,
                    exec_mode,
                    container_image: container_image.as_deref(),
                    repo_kind,
                    repo_url: repo_url.as_deref(),
                    repo_auth_kind: repo_auth_kind.as_deref(),
                    secret_material: secret_material.as_deref(),
                    ssh_host_fingerprint: ssh_host_fingerprint.as_deref(),
                    default_branch: default_branch.as_deref(),
                    autonomy_ceiling,
                    egress_policy,
                    index_enabled: *index_enabled,
                    members: members.as_slice(),
                },
            )
            .await
        }
        P::WorkspaceGetRequest { workspace_id } => workspace_get_v1(ctx, workspace_id),
        P::WorkspaceRetryRequest { workspace_id } => workspace_retry_v1(ctx, workspace_id),
        P::WorkspaceArchiveRequest {
            workspace_id,
            archived,
        } => workspace_archive_v1(ctx, workspace_id, *archived),
        P::WorkspaceDeleteRequest { workspace_id } => workspace_delete_v1(ctx, workspace_id).await,
        P::WorkspaceSettingsUpdateRequest {
            workspace_id,
            name,
            autonomy_ceiling,
            egress_policy,
            target_branch,
            index_enabled,
            quota_disk_bytes,
            quota_sessions,
        } => workspace_settings_update_v1(
            ctx,
            workspace_id,
            name,
            autonomy_ceiling,
            egress_policy,
            target_branch.as_deref(),
            *index_enabled,
            *quota_disk_bytes,
            *quota_sessions,
        ),
        P::WorkspaceSecretSetRequest {
            workspace_id,
            repo_auth_kind,
            secret_material,
            ssh_host_fingerprint,
        } => workspace_secret_set_v1(
            ctx,
            workspace_id,
            repo_auth_kind,
            secret_material.as_deref(),
            ssh_host_fingerprint.as_deref(),
        ),
        P::WorkspaceMemberSetRequest {
            workspace_id,
            user_id,
            role,
        } => workspace_member_set_v1(ctx, workspace_id, user_id, role),
        P::WorkspaceMemberRemoveRequest {
            workspace_id,
            user_id,
        } => workspace_member_remove_v1(ctx, workspace_id, user_id),
        P::WorkspaceCreatorGrantSetRequest { user_id, granted } => {
            workspace_creator_grant_set_v1(ctx, user_id, *granted)
        }
        P::WorkspaceAllowlistListRequest { workspace_id } => allowlist_list_v1(ctx, workspace_id),
        P::WorkspaceAllowlistSetRequest {
            workspace_id,
            capability,
            pattern,
        } => allowlist_set_v1(ctx, workspace_id, capability, pattern),
        P::WorkspaceAllowlistRemoveRequest {
            workspace_id,
            capability,
            pattern,
        } => allowlist_remove_v1(ctx, workspace_id, capability, pattern),
        P::SessionsListRequest { workspace_id } => sessions_list_v1(ctx, workspace_id),
        P::SessionOpenRequest {
            workspace_id,
            title,
            autonomy_mode,
        } => session_open_v1(ctx, workspace_id, title, autonomy_mode).await,
        P::SessionCloseRequest {
            workspace_id,
            session_id,
        } => session_close_v1(ctx, workspace_id, session_id).await,
        P::SessionAutonomySetRequest {
            workspace_id,
            session_id,
            autonomy_mode,
        } => session_autonomy_set_v1(ctx, workspace_id, session_id, autonomy_mode),
        P::GitStatusRequest {
            workspace_id,
            session_id,
        } => git_status_v1(ctx, workspace_id, session_id).await,
        P::WorktreesListRequest {
            workspace_id,
            session_id,
        } => worktrees_list_v1(ctx, workspace_id, session_id),

        P::FileTreeRequest {
            workspace_id,
            session_id,
            path,
            depth,
        } => file_tree_v1(ctx, workspace_id, session_id, path, *depth),
        P::FileReadRequest {
            workspace_id,
            session_id,
            path,
            start_line,
            end_line,
        } => file_read_v1(ctx, workspace_id, session_id, path, *start_line, *end_line),
        P::FileWriteRequest {
            workspace_id,
            session_id,
            path,
            content,
            expected_blob_sha,
        } => file_write_v1(
            ctx,
            workspace_id,
            session_id,
            path,
            content,
            expected_blob_sha.as_deref(),
        ),
        P::FileCreateRequest {
            workspace_id,
            session_id,
            path,
            content,
        } => file_create_v1(ctx, workspace_id, session_id, path, content),
        P::FileDeleteRequest {
            workspace_id,
            session_id,
            path,
            recursive,
            expected_blob_sha,
        } => file_delete_v1(
            ctx,
            workspace_id,
            session_id,
            path,
            *recursive,
            expected_blob_sha.as_deref(),
        ),
        P::FileRenameRequest {
            workspace_id,
            session_id,
            from_path,
            to_path,
            expected_blob_sha,
        } => file_rename_v1(
            ctx,
            workspace_id,
            session_id,
            from_path,
            to_path,
            expected_blob_sha.as_deref(),
        ),
        P::FileMkdirRequest {
            workspace_id,
            session_id,
            path,
        } => file_mkdir_v1(ctx, workspace_id, session_id, path),
        P::FileGrepRequest {
            workspace_id,
            session_id,
            query,
            glob,
            regex,
            max_results,
        } => file_grep_v1(
            ctx,
            workspace_id,
            session_id,
            query,
            glob,
            *regex,
            *max_results,
        ),
        P::GitLogRequest {
            workspace_id,
            session_id,
            path,
            limit,
        } => git_log_v1(ctx, workspace_id, session_id, path, *limit),
        P::GitBranchesRequest {
            workspace_id,
            session_id,
        } => git_branches_v1(ctx, workspace_id, session_id),
        P::GitDiffRequest {
            workspace_id,
            session_id,
            path,
            staged,
            base,
        } => git_diff_v1(ctx, workspace_id, session_id, path, *staged, base),
        P::GitCommitRequest {
            workspace_id,
            session_id,
            message,
            patch_set_id,
        } => git_commit_v1(
            ctx,
            workspace_id,
            session_id,
            message,
            patch_set_id.as_deref(),
        ),
        P::GitPushRequest {
            workspace_id,
            session_id,
            remote,
            set_upstream,
        } => git_push_v1(ctx, workspace_id, session_id, remote, *set_upstream),
        P::GitSyncRequest {
            workspace_id,
            session_id,
            mode,
        } => git_sync_v1(ctx, workspace_id, session_id, mode),
        P::GitMergeRequest {
            workspace_id,
            session_id,
            source_branch,
            target_branch,
        } => git_merge_v1(ctx, workspace_id, session_id, source_branch, target_branch),
        // `patch_set_id` still travels on the wire for the UI to display, and
        // is deliberately NOT read here: the set a finalize acts on is the one
        // the merge operation itself produced.
        P::GitMergeFinalizeRequest {
            workspace_id,
            session_id,
            op_id,
            patch_set_id: _,
        } => git_merge_finalize_v1(ctx, workspace_id, session_id, op_id),
        P::GitMergeAbandonRequest {
            workspace_id,
            session_id,
            op_id,
        } => git_merge_abandon_v1(ctx, workspace_id, session_id, op_id),
        P::PatchSetsListRequest {
            workspace_id,
            session_id,
            status,
        } => patch_sets_list_v1(ctx, workspace_id, session_id, status),
        P::PatchSetGetRequest {
            workspace_id,
            session_id,
            patch_set_id,
        } => patch_set_get_v1(ctx, workspace_id, session_id, patch_set_id),
        P::PatchDecideRequest {
            workspace_id,
            session_id,
            patch_set_id,
            files,
        } => patch_decide_v1(ctx, workspace_id, session_id, patch_set_id, files).await,
        P::PatchSetAbandonRequest {
            workspace_id,
            session_id,
            patch_set_id,
        } => patch_set_abandon_v1(ctx, workspace_id, session_id, patch_set_id),
        P::SessionTimelineRequest {
            workspace_id,
            session_id,
            after_seq,
            limit,
        } => session_timeline_v1(ctx, workspace_id, session_id, *after_seq, *limit),
        P::SessionOperationsRequest {
            workspace_id,
            session_id,
            status,
            limit,
        } => session_operations_v1(ctx, workspace_id, session_id, status, *limit),
        P::OperationResolveRequest {
            workspace_id,
            session_id,
            op_id,
            resolution,
            note,
        } => operation_resolve_v1(ctx, workspace_id, session_id, op_id, resolution, note),
        P::ApprovalsListRequest {
            workspace_id,
            session_id,
            status,
        } => approvals_list_v1(ctx, workspace_id, session_id, status),
        P::ApprovalDecideRequest {
            workspace_id,
            session_id,
            approval_id,
            decision,
        } => approval_decide_v1(ctx, workspace_id, session_id, approval_id, decision),
        P::SessionGrantsListRequest {
            workspace_id,
            session_id,
        } => session_grants_list_v1(ctx, workspace_id, session_id),
        P::SessionGrantRevokeRequest {
            workspace_id,
            session_id,
            capability,
            pattern,
        } => session_grant_revoke_v1(ctx, workspace_id, session_id, capability, pattern),
        P::SessionRunsRequest {
            workspace_id,
            session_id,
        } => session_runs_v1(ctx, workspace_id, session_id),
        P::SessionTasksRequest {
            workspace_id,
            session_id,
        } => session_tasks_v1(ctx, workspace_id, session_id),
        P::SessionMessageSendRequest {
            workspace_id,
            session_id,
            message,
        } => session_message_send_v1(ctx, workspace_id, session_id, message).await,
        P::SessionCancelRequest {
            workspace_id,
            session_id,
            run_id,
        } => session_cancel_v1(ctx, workspace_id, session_id, run_id.as_deref()),
        P::ExecStartRequest {
            workspace_id,
            session_id,
            argv,
            cwd,
            timeout_secs,
            mount_access,
            network_access,
            ephemeral,
        } => exec_start_v1(
            ctx,
            workspace_id,
            session_id,
            argv,
            cwd,
            *timeout_secs,
            mount_access,
            network_access,
            *ephemeral,
        ),
        P::ExecCancelRequest {
            workspace_id,
            session_id,
            exec_id,
        } => exec_cancel_v1(ctx, workspace_id, session_id, exec_id),
        P::ExecOutputRequest {
            workspace_id,
            session_id,
            exec_id,
            after_seq,
            limit,
        } => exec_output_v1(
            ctx,
            workspace_id,
            session_id,
            exec_id,
            *after_seq,
            *limit,
        ),
        P::TerminalOpenRequest {
            workspace_id,
            session_id,
            rows,
            cols,
        } => terminal_open_v1(ctx, workspace_id, session_id, *rows, *cols),
        P::TerminalInputRequest {
            workspace_id,
            session_id,
            terminal_id,
            data,
        } => terminal_input_v1(ctx, workspace_id, session_id, terminal_id, data),
        P::TerminalResizeRequest {
            workspace_id,
            session_id,
            terminal_id,
            rows,
            cols,
        } => terminal_resize_v1(ctx, workspace_id, session_id, terminal_id, *rows, *cols),
        P::TerminalCloseRequest {
            workspace_id,
            session_id,
            terminal_id,
        } => terminal_close_v1(ctx, workspace_id, session_id, terminal_id),
        P::TerminalSnapshotRequest {
            workspace_id,
            session_id,
            terminal_id,
        } => terminal_snapshot_v1(ctx, workspace_id, session_id, terminal_id),
        P::IndexStatusRequest { workspace_id } => index_status_v1(ctx, workspace_id),
        P::IndexRebuildRequest {
            workspace_id,
            branch,
        } => index_rebuild_v1(ctx, workspace_id, branch),
        P::CodeSearchRequest {
            workspace_id,
            session_id,
            query,
            path_prefix,
            limit,
            ..
        } => code_search_v1(ctx, workspace_id, session_id, query, path_prefix, *limit).await,
        P::PatchBlobGetRequest {
            workspace_id,
            session_id,
            blob_sha,
        } => patch_blob_get_v1(ctx, workspace_id, session_id, blob_sha),
        P::TerminalsListRequest {
            workspace_id,
            session_id,
        } => terminals_list_v1(ctx, workspace_id, session_id),
        P::WorkspaceMemberCandidatesRequest {
            workspace_id,
            query,
            limit,
        } => workspace_member_candidates_v1(ctx, workspace_id.as_deref(), query, *limit),
        P::ProjectLinkListRequest { workspace_id } => project_link_list_v1(ctx, workspace_id),
        P::ProjectLinkSetRequest {
            workspace_id,
            project_id,
            linked,
        } => project_link_set_v1(ctx, workspace_id, project_id, *linked),
        P::RepoTreeRequest {
            workspace_id,
            project_id,
            commit,
            path_prefix,
            limit,
        } => repo_tree_v1(ctx, workspace_id, project_id, commit, path_prefix, *limit),
        P::AgentCredentialsListRequest { node_id } => agent_credentials_list_v1(ctx, node_id),
        P::AgentCredentialSetRequest {
            node_id,
            engine_id,
            provider_base_url,
            credential_material,
        } => agent_credential_set_v1(
            ctx,
            node_id,
            engine_id,
            provider_base_url,
            credential_material,
        ),
        P::AgentCredentialDeleteRequest { node_id, engine_id } => {
            agent_credential_delete_v1(ctx, node_id, engine_id)
        }

        // The `*Stream*` family is answered by `stream_handlers.rs` through a
        // subscription, not here: a request/response dispatcher has nowhere to
        // put a stream. Everything else left is a RESPONSE variant.
        // Listing them keeps the match exhaustive: a new protocol variant
        // becomes a compile error here instead of falling into a catch-all.
        P::WorkspacesListResponse { .. }
        | P::WorkspaceCreateResponse { .. }
        | P::WorkspaceGetResponse { .. }
        | P::WorkspaceRetryResponse { .. }
        | P::WorkspaceArchiveResponse { .. }
        | P::WorkspaceMembersResponse { .. }
        | P::WorkspaceCreatorGrantResponse { .. }
        | P::SessionsListResponse { .. }
        | P::SessionOpenResponse { .. }
        | P::SessionCloseResponse { .. }
        | P::FileTreeResponse { .. }
        | P::FileReadResponse { .. }
        | P::FileWriteResponse { .. }
        | P::FileMutationResponse { .. }
        | P::FileGrepResponse { .. }
        | P::GitStatusResponse { .. }
        | P::GitLogResponse { .. }
        | P::GitBranchesResponse { .. }
        | P::GitDiffResponse { .. }
        | P::GitCommitResponse { .. }
        | P::GitPushResponse { .. }
        | P::GitSyncResponse { .. }
        | P::GitMergeResponse { .. }
        | P::GitMergeFinalizeResponse { .. }
        | P::WorktreesListResponse { .. }
        | P::PatchSetsListResponse { .. }
        | P::PatchSetGetResponse { .. }
        | P::PatchDecideResponse { .. }
        | P::SessionTimelineResponse { .. }
        | P::SessionOperationsResponse { .. }
        | P::OperationResolveResponse { .. }
        | P::ApprovalsListResponse { .. }
        | P::ApprovalDecideResponse { .. }
        | P::SessionGrantsListResponse { .. }
        | P::WorkspaceAllowlistListResponse { .. }
        | P::SessionRunsResponse { .. }
        | P::SessionTasksResponse { .. }
        | P::SessionMessageSendResponse { .. }
        | P::SessionCancelResponse { .. }
        | P::SessionAutonomySetResponse { .. }
        | P::ExecStartResponse { .. }
        | P::ExecCancelResponse { .. }
        | P::ExecOutputResponse { .. }
        | P::TerminalOpenResponse { .. }
        | P::TerminalSnapshotResponse { .. }
        | P::WorkspaceSettingsUpdateResponse { .. }
        | P::WorkspaceSecretSetResponse { .. }
        | P::WorkspaceDeleteResponse { .. }
        | P::IndexStatusResponse { .. }
        | P::IndexRebuildResponse { .. }
        | P::CodeSearchResponse { .. }
        | P::SessionStreamRequest { .. }
        | P::SessionStreamEvent { .. }
        | P::SessionStreamEnd { .. }
        | P::TerminalStreamRequest { .. }
        | P::TerminalStreamSnapshot { .. }
        | P::TerminalStreamDelta { .. }
        | P::TerminalStreamEnd { .. }
        | P::IndexStreamRequest { .. }
        | P::IndexStreamEnd { .. }
        | P::PatchBlobGetResponse { .. }
        | P::TerminalsListResponse { .. }
        | P::WorkspaceMemberCandidatesResponse { .. }
        | P::IndexStreamProgress { .. }
        | P::ProjectLinkListResponse { .. }
        | P::RepoTreeResponse { .. }
        | P::AgentCredentialsListResponse { .. }
        | P::AgentCredentialSetResponse { .. }
        | P::AgentCredentialDeleteResponse { .. } => Err(ProtocolError::bad_request(
            "variant is not a supported code studio request",
        )),
    }
}

// =============================================================================
// Registry
// =============================================================================

fn workspaces_list_v1(
    ctx: &HandlerContext,
    include_archived: bool,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let db = &ctx.state.db;
    let mut records = repository::list_workspaces_for_user(db, &org.org_id, &org.user_id)
        .map_err(|e| db_error("list_workspaces_for_user", e))?;

    // §25.4: an administrator sees the METADATA of every workspace in the org.
    // The ids are fetched here and each row is read through the same accessor
    // the membership path uses, so there is one row-mapping in the codebase.
    if is_admin(org) {
        let known: std::collections::HashSet<String> =
            records.iter().map(|r| r.id.clone()).collect();
        for id in org_workspace_ids(db, &org.org_id)? {
            if known.contains(&id) {
                continue;
            }
            if let Some(record) =
                repository::get_workspace(db, &id).map_err(|e| db_error("get_workspace", e))?
            {
                records.push(record);
            }
        }
    }

    // Roles and member counts come from TWO queries for the whole list, not two
    // per row: an administrator sees every workspace of the organization, and a
    // per-row lookup turns one list into hundreds of statements.
    let my_roles = roles_of_user(db, &org.user_id)?;
    let member_counts = member_counts(db)?;
    let workspaces = records
        .into_iter()
        .filter(|record| include_archived || record.status != WorkspaceStatus::Archived.slug())
        .map(|record| {
            let role = my_roles.get(&record.id).copied();
            let member_count = member_counts.get(&record.id).copied().unwrap_or(0);
            // Sessions are private per user, so a workspace the caller is not a
            // member of holds none of theirs — and opening its runtime database
            // to learn that would evict a live pool from a cache of 16.
            let sessions = if role.is_some() && record.node_id.as_str() == &*ctx.state.local_node_id
            {
                open_session_count(&record, &org.user_id)
            } else {
                SessionCounts::default()
            };
            // The list must not walk N repository trees; the real size is
            // computed for the one workspace the user opens.
            workspace_to_wire(ctx, &record, role, member_count, sessions, 0)
        })
        .collect();

    Ok(cs(CodeStudioPayload::WorkspacesListResponse {
        workspaces,
        can_create: repository::may_create_workspace(db, &org.org_id, &org.user_id)
            .map_err(|e| db_error("may_create_workspace", e))?,
        nodes: node_catalog(ctx),
    }))
}

/// Every workspace role this user holds, in one statement.
fn roles_of_user(
    db: &DbPool,
    user_id: &str,
) -> Result<HashMap<String, WorkspaceRole>, ProtocolError> {
    let conn = db
        .read()
        .map_err(|e| db_error("roles_of_user", anyhow::anyhow!("{e}")))?;
    let mut stmt = conn
        .prepare("SELECT workspace_id, role FROM code_workspace_members WHERE user_id = ?1")
        .map_err(|e| db_error("roles_of_user", anyhow::anyhow!("{e}")))?;
    let rows = stmt
        .query_map(rusqlite::params![user_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|e| db_error("roles_of_user", anyhow::anyhow!("{e}")))?;
    let mut roles = HashMap::new();
    for row in rows {
        let (workspace_id, role) = row.map_err(|e| db_error("roles_of_user", anyhow::anyhow!("{e}")))?;
        if let Some(role) = WorkspaceRole::from_slug(&role) {
            roles.insert(workspace_id, role);
        }
    }
    Ok(roles)
}

/// Member count per workspace, in one statement.
fn member_counts(db: &DbPool) -> Result<HashMap<String, u32>, ProtocolError> {
    let conn = db
        .read()
        .map_err(|e| db_error("member_counts", anyhow::anyhow!("{e}")))?;
    let mut stmt = conn
        .prepare("SELECT workspace_id, COUNT(*) FROM code_workspace_members GROUP BY workspace_id")
        .map_err(|e| db_error("member_counts", anyhow::anyhow!("{e}")))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|e| db_error("member_counts", anyhow::anyhow!("{e}")))?;
    let mut counts = HashMap::new();
    for row in rows {
        let (workspace_id, count) =
            row.map_err(|e| db_error("member_counts", anyhow::anyhow!("{e}")))?;
        counts.insert(workspace_id, count.max(0) as u32);
    }
    Ok(counts)
}

fn org_workspace_ids(db: &DbPool, org_id: &str) -> Result<Vec<String>, ProtocolError> {
    let conn = db
        .read()
        .map_err(|e| db_error("org_workspace_ids", anyhow::anyhow!("{e}")))?;
    let mut stmt = conn
        .prepare(
            "SELECT id FROM code_workspaces WHERE org_id = ?1 AND status <> 'deleted' \
             ORDER BY created_at DESC",
        )
        .map_err(|e| db_error("org_workspace_ids", anyhow::anyhow!("{e}")))?;
    let rows = stmt
        .query_map(rusqlite::params![org_id], |row| row.get::<_, String>(0))
        .map_err(|e| db_error("org_workspace_ids", anyhow::anyhow!("{e}")))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| db_error("org_workspace_ids", anyhow::anyhow!("{e}")))
}

/// Everything `WorkspaceCreateRequest` carries, borrowed. Grouped because the
/// wizard genuinely has this many decisions and threading them as loose
/// arguments makes the call site unreadable.
struct WorkspaceCreateInput<'a> {
    name: &'a str,
    node_id: &'a str,
    exec_mode: &'a str,
    container_image: Option<&'a str>,
    repo_kind: &'a str,
    repo_url: Option<&'a str>,
    repo_auth_kind: Option<&'a str>,
    secret_material: Option<&'a str>,
    ssh_host_fingerprint: Option<&'a str>,
    default_branch: Option<&'a str>,
    autonomy_ceiling: &'a str,
    egress_policy: &'a str,
    index_enabled: bool,
    members: &'a [WorkspaceMemberInput],
}

async fn workspace_create_v1(
    ctx: &HandlerContext,
    input: WorkspaceCreateInput<'_>,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let db = &ctx.state.db;
    if !repository::may_create_workspace(db, &org.org_id, &org.user_id)
        .map_err(|e| db_error("may_create_workspace", e))?
    {
        return Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            "creating a workspace requires a per-user grant",
        ));
    }

    // The dispatcher forwards a create addressed at another node, so reaching
    // this handler means the workspace is being made HERE.
    let requested_node = input.node_id.trim();
    if !requested_node.is_empty() && requested_node != &*ctx.state.local_node_id {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            format!(
                "workspaces are provisioned by their owner node; '{requested_node}' could not be \
                 reached from this one"
            ),
        ));
    }

    let name = input.name.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return Err(ProtocolError::bad_request(
            "workspace name must be 1-120 characters",
        ));
    }

    let (exec_mode, mode_defaulted) = parse_exec_mode(input.exec_mode)?;
    if exec_mode == ExecMode::Container && !node_supports_container(ctx) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "this node cannot run a container-isolated workspace",
        ));
    }
    let enforcement = resolve_egress_enforcement(exec_mode);
    let ceiling = parse_autonomy(input.autonomy_ceiling)?;
    validate_workspace_policy(
        exec_mode,
        input.container_image,
        ceiling,
        input.egress_policy,
        enforcement,
    )?;

    if !matches!(input.repo_kind, "empty" | "git") {
        return Err(ProtocolError::bad_request(format!(
            "unknown repository kind '{}'",
            input.repo_kind
        )));
    }
    let mut private_remote = false;
    if input.repo_kind == "git" {
        let url = input
            .repo_url
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .ok_or_else(|| ProtocolError::bad_request("a git workspace needs a repository url"))?
            .to_string();
        // Resolution is a blocking DNS round trip and the policy checks every
        // address it returns (§11.4).
        let target = tokio::task::spawn_blocking(move || {
            crate::code_studio::remote_policy::validate_remote(&url)
        })
        .await
        .map_err(|_| ProtocolError::internal("remote validation task failed"))?
        .map_err(|e| ProtocolError::bad_request(format!("{e:#}")))?;
        private_remote = target.is_private;
    }

    let auth_kind = input.repo_auth_kind.map(str::trim).unwrap_or("none");
    let material = input.secret_material.map(str::trim).filter(|m| !m.is_empty());
    let secret_kind = match auth_kind {
        "none" => {
            if material.is_some() {
                return Err(ProtocolError::bad_request(
                    "repo_auth_kind 'none' does not take credential material",
                ));
            }
            None
        }
        "token" | "ssh_key" => Some(
            SecretKind::from_repo_auth_kind(auth_kind)
                .ok_or_else(|| ProtocolError::bad_request("unknown repo_auth_kind"))?,
        ),
        other => {
            return Err(ProtocolError::bad_request(format!(
                "unknown repo_auth_kind '{other}'"
            )))
        }
    };
    if secret_kind.is_some() && material.is_none() {
        return Err(ProtocolError::bad_request(
            "the selected authentication needs credential material",
        ));
    }

    let workspace_id = uuid::Uuid::new_v4().to_string();

    // §7.1 / §24: the resolved execution mode is recorded BEFORE the row is
    // written, so an unaudited workspace cannot come into existence — not even
    // if the insert succeeds and this node dies immediately afterwards.
    audit(
        ctx,
        "code_studio.workspace_create",
        &workspace_id,
        &serde_json::json!({
            "name": name,
            "node_id": ctx.state.local_node_id.as_ref(),
            "exec_mode": exec_mode.slug(),
            "exec_mode_defaulted": mode_defaulted,
            "egress_enforcement": enforcement.slug(),
            "egress_policy": input.egress_policy,
            "autonomy_ceiling": ceiling.slug(),
            "repo_kind": input.repo_kind,
            "repo_auth_kind": auth_kind,
            "private_remote": private_remote,
        }),
    );

    // The material reaches the vault before provisioning starts (saga S3) and
    // is never sent back: `has_secret` is all the UI ever learns.
    let secret_ref = match (secret_kind, material) {
        (Some(kind), Some(material)) => Some(
            vault::put_workspace_secret(
                db,
                &ctx.state.settings_cipher,
                &workspace_id,
                kind,
                material,
                &org.user_id,
            )
            .map_err(|e| vault_error("put_workspace_secret", e))?
            .secret_ref,
        ),
        _ => None,
    };

    let created = repository::create_workspace(
        db,
        &NewWorkspace {
            id: workspace_id.clone(),
            org_id: org.org_id.clone(),
            owner_user_id: org.user_id.clone(),
            name: name.to_string(),
            slug: slugify(name),
            node_id: ctx.state.local_node_id.to_string(),
            exec_mode,
            container_image: input.container_image.map(str::to_string),
            egress_enforcement: enforcement,
            repo_kind: input.repo_kind.to_string(),
            repo_url: input.repo_url.map(str::to_string),
            repo_auth_kind: Some(auth_kind.to_string()),
            secret_ref,
            ssh_host_fingerprint: input.ssh_host_fingerprint.map(str::to_string),
            default_branch: input
                .default_branch
                .map(str::trim)
                .filter(|b| !b.is_empty())
                .map(str::to_string),
            target_branch: None,
            autonomy_ceiling: ceiling,
            egress_policy: input.egress_policy.to_string(),
            index_enabled: input.index_enabled,
            quota_disk_bytes: None,
            quota_sessions: None,
        },
    );
    let created = match created {
        Ok(record) => record,
        Err(error) => {
            // Compensation: the material was written first, so a refused insert
            // must not leave a key nobody can reach.
            if let Err(cleanup) = vault::delete_workspace_secrets(db, &workspace_id) {
                tracing::warn!(workspace_id = %workspace_id, error = %cleanup, "orphan vault row after a failed create");
            }
            let message = error.to_string();
            return Err(if message.contains("UNIQUE") {
                ProtocolError::bad_request("a workspace with this name already exists")
            } else {
                db_error("create_workspace", error)
            });
        }
    };

    for member in input.members {
        if member.user_id == org.user_id {
            continue;
        }
        let role = parse_role(&member.role)?;
        if let Err(error) =
            repository::upsert_member(db, &workspace_id, &member.user_id, role, &org.user_id)
        {
            tracing::warn!(workspace_id = %workspace_id, error = %error, "cannot add a workspace member");
        }
    }

    spawn_provisioning(ctx, created.clone());

    Ok(cs(CodeStudioPayload::WorkspaceCreateResponse {
        workspace_id,
        status: created.status,
    }))
}

/// Runs (or resumes) the provisioning saga off the request path. The workspace
/// is `provisioning` until it finishes and the UI follows it with
/// `WorkspaceGetRequest` — §6 forbids a half-built workspace that reports
/// `active`.
fn spawn_provisioning(ctx: &HandlerContext, record: WorkspaceRecord) {
    let db = ctx.state.db.clone();
    let cipher = ctx.state.settings_cipher.clone();
    tokio::task::spawn_blocking(move || {
        let auth = match resolve_provision_auth(&db, &cipher, &record) {
            Ok(auth) => auth,
            Err(error) => {
                let detail = error.to_string();
                tracing::warn!(workspace_id = %record.id, "provisioning credential: {detail}");
                if let Err(nested) = repository::set_status(
                    &db,
                    &record.id,
                    WorkspaceStatus::Error,
                    Some(&detail),
                ) {
                    tracing::warn!(workspace_id = %record.id, "cannot record credential failure: {nested:#}");
                }
                return;
            }
        };
        match provisioning::provision(&db, &record, &auth) {
            Ok(_) => {
                // The clone is the first real use of the credential, which is
                // what §5.2 waits for before dropping a superseded key.
                if let Err(error) = vault::confirm_rotation(&db, &record.id) {
                    tracing::warn!(workspace_id = %record.id, "rotation confirm: {error}");
                }
            }
            Err(error) => {
                tracing::warn!(workspace_id = %record.id, "provisioning failed: {error:#}")
            }
        }
    });
}

/// Reads the workspace credential out of the vault for the git broker. This is
/// one of the two sanctioned exits for key material (§5.2); the broker runs
/// outside any sandbox and holds it for the length of one clone.
fn resolve_provision_auth(
    db: &DbPool,
    cipher: &crate::crypto::SettingsCipher,
    record: &WorkspaceRecord,
) -> Result<ProvisionAuth, VaultError> {
    let Some(secret_ref) = record.secret_ref.as_deref() else {
        return Ok(ProvisionAuth::None);
    };
    let material = vault::get_workspace_secret(db, cipher, secret_ref)?;
    Ok(match material.kind() {
        SecretKind::GitToken => ProvisionAuth::Token(material.expose().to_string()),
        SecretKind::SshKey => ProvisionAuth::SshKey {
            private_key: material.expose().to_string(),
            known_host: record.ssh_host_fingerprint.clone(),
        },
    })
}

fn workspace_get_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, role) = require_workspace(ctx, org, workspace_id, Access::Metadata)?;
    let members = members_to_wire(ctx, workspace_id);
    let is_local = record.node_id.as_str() == &*ctx.state.local_node_id;
    let provisioning = repository::list_saga_steps(&ctx.state.db, workspace_id)
        .map_err(|e| db_error("list_saga_steps", e))?
        .into_iter()
        .map(|step| ProvisionStepInfo {
            step: step.step,
            status: step.status,
            detail: step.detail,
            updated_at: step.updated_at,
        })
        .collect();
    let workspace = workspace_to_wire(
        ctx,
        &record,
        role,
        members.len() as u32,
        if is_local {
            open_session_count(&record, &org.user_id)
        } else {
            SessionCounts::default()
        },
        if is_local {
            workspace_disk_usage(workspace_id)
        } else {
            0
        },
    );

    Ok(cs(CodeStudioPayload::WorkspaceGetResponse {
        workspace,
        members,
        provisioning,
    }))
}

fn workspace_retry_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _) = require_workspace(
        ctx,
        org,
        workspace_id,
        Access::Member(WorkspaceRole::Owner),
    )?;
    require_local(ctx, &record)?;
    if record.status != WorkspaceStatus::Error.slug() {
        return Err(ProtocolError::new(
            ProtocolErrorCode::Conflict,
            format!("workspace is '{}'; only a failed one is retried", record.status),
        ));
    }
    repository::set_status(
        &ctx.state.db,
        workspace_id,
        WorkspaceStatus::Provisioning,
        None,
    )
    .map_err(|e| db_error("set_status", e))?;
    let mut resumed = record;
    resumed.status = WorkspaceStatus::Provisioning.slug().to_string();
    resumed.status_detail = None;
    audit(
        ctx,
        "code_studio.workspace_retry",
        workspace_id,
        &serde_json::json!({ "exec_mode": resumed.exec_mode }),
    );
    spawn_provisioning(ctx, resumed);

    Ok(cs(CodeStudioPayload::WorkspaceRetryResponse {
        workspace_id: workspace_id.to_string(),
        status: WorkspaceStatus::Provisioning.slug().to_string(),
        status_detail: None,
    }))
}

fn workspace_archive_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    archived: bool,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _) = require_workspace(ctx, org, workspace_id, Access::Lifecycle)?;
    let target = if archived {
        if !matches!(record.status.as_str(), "active" | "error") {
            return Err(ProtocolError::new(
                ProtocolErrorCode::Conflict,
                format!("workspace is '{}' and cannot be archived", record.status),
            ));
        }
        WorkspaceStatus::Archived
    } else {
        if record.status != WorkspaceStatus::Archived.slug() {
            return Err(ProtocolError::new(
                ProtocolErrorCode::Conflict,
                "workspace is not archived",
            ));
        }
        WorkspaceStatus::Active
    };
    repository::set_status(&ctx.state.db, workspace_id, target, None)
        .map_err(|e| db_error("set_status", e))?;
    audit(
        ctx,
        "code_studio.workspace_archive",
        workspace_id,
        &serde_json::json!({ "archived": archived }),
    );

    Ok(cs(CodeStudioPayload::WorkspaceArchiveResponse {
        workspace_id: workspace_id.to_string(),
        status: target.slug().to_string(),
    }))
}

async fn workspace_delete_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _) = require_workspace(ctx, org, workspace_id, Access::Lifecycle)?;
    require_local(ctx, &record)?;

    if record.status == WorkspaceStatus::Active.slug() {
        let pool = open_workspace_pool(&record)?;
        let open = count_open_sessions(&pool)?;
        if open > 0 {
            return Err(ProtocolError::new(
                ProtocolErrorCode::Conflict,
                format!("workspace still has {open} open session(s); close them first"),
            ));
        }
    }

    // Data on the owner node first, tombstone second (§13.5): a registry row
    // marked deleted while the tree survives would leak disk nobody accounts
    // for, and the tombstone is what the Sync Ledger carries to other nodes.
    let id = workspace_id.to_string();
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        workspace_db::close(&id);
        let dir = paths::workspace_dir(&id)?;
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(anyhow::anyhow!("remove workspace directory: {error}")),
        }
    })
    .await
    .map_err(|_| ProtocolError::internal("workspace removal task failed"))?
    .map_err(|e| db_error("remove_workspace_dir", e))?;

    vault::delete_workspace_secrets(&ctx.state.db, workspace_id)
        .map_err(|e| vault_error("delete_workspace_secrets", e))?;
    repository::set_status(
        &ctx.state.db,
        workspace_id,
        WorkspaceStatus::Deleted,
        None,
    )
    .map_err(|e| db_error("set_status", e))?;
    audit(
        ctx,
        "code_studio.workspace_delete",
        workspace_id,
        &serde_json::json!({ "node_id": record.node_id }),
    );

    Ok(cs(CodeStudioPayload::WorkspaceDeleteResponse {
        workspace_id: workspace_id.to_string(),
        status: WorkspaceStatus::Deleted.slug().to_string(),
    }))
}

fn count_open_sessions(pool: &DbPool) -> Result<i64, ProtocolError> {
    let conn = pool
        .read()
        .map_err(|e| db_error("count_open_sessions", anyhow::anyhow!("{e}")))?;
    conn.query_row(
        "SELECT COUNT(*) FROM sessions WHERE status NOT IN ('closed','failed','cancelled')",
        [],
        |row| row.get(0),
    )
    .map_err(|e| db_error("count_open_sessions", anyhow::anyhow!("{e}")))
}

#[allow(clippy::too_many_arguments)]
fn workspace_settings_update_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    name: &str,
    autonomy_ceiling: &str,
    egress_policy: &str,
    target_branch: Option<&str>,
    index_enabled: bool,
    quota_disk_bytes: Option<i64>,
    quota_sessions: Option<i64>,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    // Quotas are an administrator's business (§25.4); everything else here is
    // `workspace_settings`, which §9.2 gives to the OWNER and to nobody else.
    // The two are gated separately: raising `autonomy_ceiling` or loosening
    // `egress_policy` decides how much an agent may do unattended and where it
    // may reach — that is a security policy write, not lifecycle, and the
    // administrator overlay of §25.4 is "metadata and lifecycle, never content".
    let (record, role) = require_workspace(ctx, org, workspace_id, Access::Lifecycle)?;
    let is_owner = role.is_some_and(|actual| actual >= WorkspaceRole::Owner);

    let name = name.trim();
    if name.is_empty() || name.chars().count() > 120 {
        return Err(ProtocolError::bad_request(
            "workspace name must be 1-120 characters",
        ));
    }
    let ceiling = parse_autonomy(autonomy_ceiling)?;
    if !is_owner {
        let target_branch_unchanged = match target_branch.map(str::trim).filter(|b| !b.is_empty()) {
            Some(requested) => record.target_branch.as_deref() == Some(requested),
            None => true,
        };
        let policy_untouched = name == record.name
            && ceiling.slug() == record.autonomy_ceiling
            && egress_policy == record.egress_policy
            && index_enabled == record.index_enabled
            && target_branch_unchanged;
        if !policy_untouched {
            return Err(ProtocolError::new(
                ProtocolErrorCode::PolicyDenied,
                "workspace settings belong to the workspace owner; an administrator \
                 may change quotas only",
            ));
        }
    }
    let exec_mode = ExecMode::from_slug(&record.exec_mode)
        .ok_or_else(|| ProtocolError::internal("workspace has an unknown execution mode"))?;
    let enforcement = EgressEnforcement::from_slug(&record.egress_enforcement)
        .ok_or_else(|| ProtocolError::internal("workspace has an unknown egress enforcement"))?;
    // The execution mode is immutable (§9.5), so it is re-read from the record
    // and validated against — the update statement below never names the
    // column, and the wire has no field for it either.
    validate_workspace_policy(
        exec_mode,
        record.container_image.as_deref(),
        ceiling,
        egress_policy,
        enforcement,
    )?;
    if quota_disk_bytes.is_some_and(|q| q <= 0) || quota_sessions.is_some_and(|q| q <= 0) {
        return Err(ProtocolError::bad_request("a quota must be positive"));
    }

    repository::set_settings(
        &ctx.state.db,
        workspace_id,
        name,
        ceiling.slug(),
        egress_policy,
        target_branch.map(str::trim).filter(|b| !b.is_empty()),
        index_enabled,
        quota_disk_bytes,
        quota_sessions,
    )
    .map_err(|e| db_error("set_settings", e))?;
    audit(
        ctx,
        "code_studio.workspace_settings",
        workspace_id,
        &serde_json::json!({
            "autonomy_ceiling": ceiling.slug(),
            "egress_policy": egress_policy,
            "index_enabled": index_enabled,
            "quota_disk_bytes": quota_disk_bytes,
            "quota_sessions": quota_sessions,
        }),
    );

    let updated = repository::get_workspace(&ctx.state.db, workspace_id)
        .map_err(|e| db_error("get_workspace", e))?
        .ok_or_else(not_found)?;
    let members = repository::list_members(&ctx.state.db, workspace_id)
        .map(|m| m.len() as u32)
        .unwrap_or(0);
    Ok(cs(CodeStudioPayload::WorkspaceSettingsUpdateResponse {
        workspace: workspace_to_wire(
            ctx,
            &updated,
            role,
            members,
            open_session_count(&updated, &org.user_id),
            0,
        ),
    }))
}

fn workspace_secret_set_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    repo_auth_kind: &str,
    secret_material: Option<&str>,
    ssh_host_fingerprint: Option<&str>,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    // `secret_manage` is owner-only (§9.2) and gets no administrator override:
    // the vault is the one place §25.4's metadata overlay must not reach.
    let (record, _) = require_workspace(
        ctx,
        org,
        workspace_id,
        Access::Member(WorkspaceRole::Owner),
    )?;
    require_local(ctx, &record)?;

    let material = secret_material.map(str::trim).filter(|m| !m.is_empty());
    let response = match repo_auth_kind.trim() {
        "none" => {
            vault::delete_workspace_secrets(&ctx.state.db, workspace_id)
                .map_err(|e| vault_error("delete_workspace_secrets", e))?;
            set_repo_auth(ctx, workspace_id, "none", None)?;
            CodeStudioPayload::WorkspaceSecretSetResponse {
                workspace_id: workspace_id.to_string(),
                has_secret: false,
                fingerprint: None,
            }
        }
        kind @ ("token" | "ssh_key") => {
            let secret_kind = SecretKind::from_repo_auth_kind(kind)
                .ok_or_else(|| ProtocolError::bad_request("unknown repo_auth_kind"))?;
            let material = material.ok_or_else(|| {
                ProtocolError::bad_request("the selected authentication needs credential material")
            })?;
            let rotation = vault::rotate_workspace_secret(
                &ctx.state.db,
                &ctx.state.settings_cipher,
                workspace_id,
                secret_kind,
                material,
                &org.user_id,
            )
            .map_err(|e| vault_error("rotate_workspace_secret", e))?;
            set_repo_auth(ctx, workspace_id, kind, ssh_host_fingerprint)?;
            audit(
                ctx,
                "code_studio.workspace_secret_set",
                workspace_id,
                &serde_json::json!({
                    "repo_auth_kind": kind,
                    "fingerprint": rotation.fingerprint,
                    "superseded": rotation.superseded_ref.is_some(),
                }),
            );
            CodeStudioPayload::WorkspaceSecretSetResponse {
                workspace_id: workspace_id.to_string(),
                has_secret: true,
                fingerprint: Some(rotation.fingerprint),
            }
        }
        other => {
            return Err(ProtocolError::bad_request(format!(
                "unknown repo_auth_kind '{other}'"
            )))
        }
    };

    Ok(cs(response))
}

fn set_repo_auth(
    ctx: &HandlerContext,
    workspace_id: &str,
    repo_auth_kind: &str,
    ssh_host_fingerprint: Option<&str>,
) -> Result<(), ProtocolError> {
    repository::set_repo_auth(
        &ctx.state.db,
        workspace_id,
        repo_auth_kind,
        ssh_host_fingerprint
            .map(str::trim)
            .filter(|f| !f.is_empty()),
    )
    .map_err(|e| db_error("set_repo_auth", e))
}

// =============================================================================
// Provider credentials of the CLI engines (§5.2, §7.5)
// =============================================================================

/// Engines that can be put behind the provider adapter at all. `EngineWiring`
/// is the authority — an engine with no wiring has no credential header, no
/// base-url variable and no way to reach a provider, so storing a key for it
/// would be storing a secret nothing can ever use.
const CREDENTIAL_ENGINES: &[&str] = &["claude-code", "codex"];

/// The node a credential call acts on: the one named, or this one.
///
/// A call that reaches a handler has already passed `route_to_owner`, so a
/// foreign node id here means the mesh could not carry it. Answering it locally
/// would write the material into THIS node's vault under someone else's node
/// id, where nothing would ever read it — so it is refused instead.
fn credential_node_id<'a>(
    ctx: &'a HandlerContext,
    node_id: &'a str,
) -> Result<&'a str, ProtocolError> {
    let node_id = node_id.trim();
    if node_id.is_empty() || node_id == &*ctx.state.local_node_id {
        return Ok(&ctx.state.local_node_id);
    }
    Err(ProtocolError::new(
        ProtocolErrorCode::NotAvailable,
        format!(
            "the provider credential of node '{node_id}' is sealed with that node's key and can \
             only be written there"
        ),
    ))
}

fn require_known_engine(engine_id: &str) -> Result<&str, ProtocolError> {
    let engine_id = engine_id.trim();
    if !CREDENTIAL_ENGINES.contains(&engine_id) {
        return Err(ProtocolError::bad_request(format!(
            "unknown CLI engine '{engine_id}'; this build reaches a provider through the adapter \
             for {}",
            CREDENTIAL_ENGINES.join(", ")
        )));
    }
    Ok(engine_id)
}

fn credential_to_wire(record: vault::AgentCredentialRecord) -> AgentCredentialInfo {
    AgentCredentialInfo {
        node_id: record.node_id,
        engine_id: record.engine_id,
        provider_base_url: record.provider_base_url,
        fingerprint: record.fingerprint,
        created_by: record.created_by,
        created_at: record.created_at,
        rotated_at: record.rotated_at,
        last_used_at: record.last_used_at,
    }
}

/// Administering the organization's provider account is `code_studio.admin`.
///
/// It is not a workspace act: the row is keyed by (org, node, engine) and every
/// workspace of the organization on that node delegates through it, so there is
/// no owner to ask. `Access::Member(Owner)` — the gate on a workspace secret —
/// has nothing to bind to here, and the closest existing org-level operation,
/// `workspace_creator_grant_set_v1`, is on `require_admin` for the same reason.
fn agent_credentials_list_v1(
    ctx: &HandlerContext,
    node_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_admin(ctx)?;
    let node_id = credential_node_id(ctx, node_id)?.to_string();
    let credentials = vault::list_agent_credentials(&ctx.state.db, &org.org_id, &node_id)
        .map_err(|e| vault_error("list_agent_credentials", e))?
        .into_iter()
        .map(credential_to_wire)
        .collect();
    Ok(cs(CodeStudioPayload::AgentCredentialsListResponse {
        node_id,
        credentials,
        engines: CREDENTIAL_ENGINES.iter().map(|e| e.to_string()).collect(),
    }))
}

/// Stores or rotates the provider credential of one engine.
///
/// Rotation is the same call: the row is keyed by (org, node, engine), so a
/// second write replaces the material and stamps `rotated_at` rather than
/// leaving a second key behind. Unlike a workspace secret there is no window in
/// which the previous key must stay readable — the adapter reads the row at
/// every start, and a run holding an adapter already has the old material in
/// memory for as long as it lives.
fn agent_credential_set_v1(
    ctx: &HandlerContext,
    node_id: &str,
    engine_id: &str,
    provider_base_url: &str,
    credential_material: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_admin(ctx)?;
    let node_id = credential_node_id(ctx, node_id)?.to_string();
    let engine_id = require_known_engine(engine_id)?;
    let material = credential_material.trim();
    if material.is_empty() {
        return Err(ProtocolError::bad_request(
            "a provider credential without material would leave the adapter unauthenticated",
        ));
    }
    let existed = vault::get_agent_credential_record(
        &ctx.state.db,
        &org.org_id,
        &node_id,
        engine_id,
    )
    .map_err(|e| vault_error("get_agent_credential_record", e))?
    .is_some();
    let fingerprint = vault::put_agent_credential(
        &ctx.state.db,
        &ctx.state.settings_cipher,
        &org.org_id,
        &node_id,
        engine_id,
        material,
        provider_base_url,
        &org.user_id,
    )
    .map_err(|e| vault_error("put_agent_credential", e))?;
    let record = vault::get_agent_credential_record(
        &ctx.state.db,
        &org.org_id,
        &node_id,
        engine_id,
    )
    .map_err(|e| vault_error("get_agent_credential_record", e))?
    .ok_or_else(|| ProtocolError::internal("the stored credential could not be read back"))?;
    // The digest identifies WHICH key was stored without being able to
    // reconstruct it — the same thing the workspace-secret event records, and
    // the only way an auditor can tell a rotation from a rewrite of the same
    // key. The material itself appears in no field of this event.
    audit(
        ctx,
        "code_studio.agent_credential_set",
        &format!("{node_id}/{engine_id}"),
        &serde_json::json!({
            "node_id": node_id,
            "engine_id": engine_id,
            "provider_base_url": record.provider_base_url,
            "fingerprint": fingerprint,
            "rotated": existed,
        }),
    );
    Ok(cs(CodeStudioPayload::AgentCredentialSetResponse {
        credential: credential_to_wire(record),
    }))
}

fn agent_credential_delete_v1(
    ctx: &HandlerContext,
    node_id: &str,
    engine_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_admin(ctx)?;
    let node_id = credential_node_id(ctx, node_id)?.to_string();
    // Deliberately NOT `require_known_engine`: a build that drops an engine
    // from `CREDENTIAL_ENGINES` must not strand its key material in the vault
    // with no way to remove it. Storing a key needs the engine to exist,
    // destroying one does not.
    let engine_id = engine_id.trim();
    if engine_id.is_empty() {
        return Err(ProtocolError::bad_request("engine_id is required"));
    }
    let removed = vault::delete_agent_credential(&ctx.state.db, &org.org_id, &node_id, engine_id)
        .map_err(|e| vault_error("delete_agent_credential", e))?;
    audit(
        ctx,
        "code_studio.agent_credential_delete",
        &format!("{node_id}/{engine_id}"),
        &serde_json::json!({
            "node_id": node_id,
            "engine_id": engine_id,
            "removed": removed > 0,
        }),
    );
    Ok(cs(CodeStudioPayload::AgentCredentialDeleteResponse {
        node_id,
        engine_id: engine_id.to_string(),
        removed: removed > 0,
    }))
}

// =============================================================================
// Members and the create grant
// =============================================================================

fn workspace_member_set_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    user_id: &str,
    role: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let role = parse_role(role)?;
    // §25.4: content is reached by JOINING the members table, and an
    // administrator may put THEMSELVES there. That is the whole override — the
    // row records who added it, so the owner sees it in the member list, and
    // the audit entry is hash-chained and cannot be removed.
    let admin_self_join = is_admin(org) && user_id == org.user_id;
    let access = if admin_self_join {
        Access::Metadata
    } else {
        Access::Member(WorkspaceRole::Owner)
    };
    let (_record, _) = require_workspace(ctx, org, workspace_id, access)?;

    if !user_exists(ctx, user_id) {
        return Err(ProtocolError::bad_request("unknown user"));
    }
    // A workspace without an owner is unreachable for everyone, so a demotion
    // is refused for the same reason a removal is.
    let current = repository::role_of(&ctx.state.db, workspace_id, user_id)
        .map_err(|e| db_error("role_of", e))?;
    if current == Some(WorkspaceRole::Owner) && role != WorkspaceRole::Owner {
        let owners = repository::list_members(&ctx.state.db, workspace_id)
            .map_err(|e| db_error("list_members", e))?
            .into_iter()
            .filter(|m| m.role == WorkspaceRole::Owner.slug())
            .count();
        if owners <= 1 {
            return Err(ProtocolError::bad_request(
                "a workspace cannot lose its last owner",
            ));
        }
    }

    repository::upsert_member(&ctx.state.db, workspace_id, user_id, role, &org.user_id)
        .map_err(|e| db_error("upsert_member", e))?;
    audit(
        ctx,
        if admin_self_join {
            "code_studio.admin_member_self_add"
        } else {
            "code_studio.workspace_member_set"
        },
        workspace_id,
        &serde_json::json!({ "user_id": user_id, "role": role.slug() }),
    );

    Ok(cs(CodeStudioPayload::WorkspaceMembersResponse {
        workspace_id: workspace_id.to_string(),
        members: members_to_wire(ctx, workspace_id),
    }))
}

fn workspace_member_remove_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    user_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    // An administrator who joined under §25.4 can step back out again without
    // asking the owner; everything else is the owner's decision.
    let admin_self_leave = is_admin(org) && user_id == org.user_id;
    let access = if admin_self_leave {
        Access::Metadata
    } else {
        Access::Member(WorkspaceRole::Owner)
    };
    require_workspace(ctx, org, workspace_id, access)?;

    repository::remove_member(&ctx.state.db, workspace_id, user_id)
        .map_err(|e| ProtocolError::bad_request(format!("{e:#}")))?;
    audit(
        ctx,
        "code_studio.workspace_member_remove",
        workspace_id,
        &serde_json::json!({ "user_id": user_id }),
    );

    Ok(cs(CodeStudioPayload::WorkspaceMembersResponse {
        workspace_id: workspace_id.to_string(),
        members: members_to_wire(ctx, workspace_id),
    }))
}

fn workspace_creator_grant_set_v1(
    ctx: &HandlerContext,
    user_id: &str,
    granted: bool,
) -> Result<MessageBody, ProtocolError> {
    let org = require_admin(ctx)?;
    if !user_exists(ctx, user_id) {
        return Err(ProtocolError::bad_request("unknown user"));
    }
    if granted {
        repository::grant_creator(&ctx.state.db, &org.org_id, user_id, &org.user_id)
            .map_err(|e| db_error("grant_creator", e))?;
    } else {
        repository::revoke_creator(&ctx.state.db, &org.org_id, user_id)
            .map_err(|e| db_error("revoke_creator", e))?;
    }
    audit(
        ctx,
        "code_studio.creator_grant_set",
        user_id,
        &serde_json::json!({ "granted": granted, "org_id": org.org_id }),
    );

    Ok(cs(CodeStudioPayload::WorkspaceCreatorGrantResponse {
        user_id: user_id.to_string(),
        granted,
    }))
}

// =============================================================================
// Workspace allowlist (standing `always` permissions, §9.1)
// =============================================================================

fn allowlist_list_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    require_workspace(
        ctx,
        org,
        workspace_id,
        Access::Member(WorkspaceRole::Viewer),
    )?;
    Ok(cs(CodeStudioPayload::WorkspaceAllowlistListResponse {
        workspace_id: workspace_id.to_string(),
        entries: read_allowlist(&ctx.state.db, workspace_id)?,
    }))
}

fn allowlist_set_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    capability: &str,
    pattern: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    require_workspace(
        ctx,
        org,
        workspace_id,
        Access::Member(WorkspaceRole::Owner),
    )?;
    let capability = capability.trim();
    let parsed = Capability::from_slug(capability)
        .ok_or_else(|| ProtocolError::bad_request(format!("unknown capability '{capability}'")))?;
    if !is_allowlistable(parsed) {
        return Err(ProtocolError::bad_request(format!(
            "capability '{capability}' cannot hold a standing permission"
        )));
    }
    let pattern = pattern.trim();
    pep::validate_grant_pattern(pattern).map_err(ProtocolError::bad_request)?;

    // Through the repository, never by hand: that write captures the row for the
    // sync ledger in the SAME transaction, so a standing permission granted on
    // one node is the same permission on every node the workspace is visible on.
    repository::add_allowlist_entry(
        &ctx.state.db,
        workspace_id,
        capability,
        pattern,
        &org.user_id,
    )
    .map_err(|e| db_error("add_allowlist_entry", e))?;
    audit(
        ctx,
        "code_studio.allowlist_set",
        workspace_id,
        &serde_json::json!({ "capability": capability, "pattern": pattern }),
    );

    Ok(cs(CodeStudioPayload::WorkspaceAllowlistListResponse {
        workspace_id: workspace_id.to_string(),
        entries: read_allowlist(&ctx.state.db, workspace_id)?,
    }))
}

fn allowlist_remove_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    capability: &str,
    pattern: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    require_workspace(
        ctx,
        org,
        workspace_id,
        Access::Member(WorkspaceRole::Owner),
    )?;
    // Withdrawal replicates as a tombstone, or a node still holding the older
    // grant would keep executing on it.
    repository::remove_allowlist_entry(
        &ctx.state.db,
        workspace_id,
        capability.trim(),
        pattern.trim(),
    )
    .map_err(|e| db_error("remove_allowlist_entry", e))?;
    audit(
        ctx,
        "code_studio.allowlist_remove",
        workspace_id,
        &serde_json::json!({ "capability": capability, "pattern": pattern }),
    );

    Ok(cs(CodeStudioPayload::WorkspaceAllowlistListResponse {
        workspace_id: workspace_id.to_string(),
        entries: read_allowlist(&ctx.state.db, workspace_id)?,
    }))
}

fn read_allowlist(
    db: &DbPool,
    workspace_id: &str,
) -> Result<Vec<AllowlistEntryInfo>, ProtocolError> {
    let conn = db
        .read()
        .map_err(|e| db_error("read_allowlist", anyhow::anyhow!("{e}")))?;
    let mut stmt = conn
        .prepare(
            "SELECT id, capability, pattern, created_by, created_at \
             FROM code_workspace_allowlist WHERE workspace_id = ?1 \
             ORDER BY capability, pattern",
        )
        .map_err(|e| db_error("read_allowlist", anyhow::anyhow!("{e}")))?;
    let rows = stmt
        .query_map(rusqlite::params![workspace_id], |row| {
            Ok(AllowlistEntryInfo {
                entry_id: row.get(0)?,
                capability: row.get(1)?,
                pattern: row.get(2)?,
                created_by: row.get(3)?,
                created_at: row.get(4)?,
            })
        })
        .map_err(|e| db_error("read_allowlist", anyhow::anyhow!("{e}")))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| db_error("read_allowlist", anyhow::anyhow!("{e}")))
}

// =============================================================================
// Sessions
// =============================================================================

fn sessions_list_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _) = require_workspace(
        ctx,
        org,
        workspace_id,
        Access::Member(WorkspaceRole::Viewer),
    )?;
    require_local(ctx, &record)?;

    // A workspace that never finished provisioning has no runtime database, and
    // opening one here would materialise an empty file that masks the failure.
    let sessions = if record.status == WorkspaceStatus::Active.slug() {
        let pool = open_workspace_pool(&record)?;
        session::list_sessions_for_user(&pool, &org.user_id)
            .map_err(|e| db_error("list_sessions_for_user", e))?
            .into_iter()
            .map(session_to_wire)
            .collect()
    } else {
        Vec::new()
    };

    Ok(cs(CodeStudioPayload::SessionsListResponse {
        workspace_id: workspace_id.to_string(),
        sessions,
    }))
}

async fn session_open_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    title: &str,
    autonomy_mode: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, role) = require_workspace(
        ctx,
        org,
        workspace_id,
        Access::Member(WorkspaceRole::Editor),
    )?;
    // `Access::Member` never applies the administrator overlay, so the role is
    // always present here; the session layer re-checks it anyway.
    let role = role.unwrap_or(WorkspaceRole::Editor);
    require_local(ctx, &record)?;
    require_active(&record)?;

    let title = title.trim();
    if title.is_empty() || title.chars().count() > 200 {
        return Err(ProtocolError::bad_request(
            "session title must be 1-200 characters",
        ));
    }
    let requested = parse_autonomy(autonomy_mode)?;
    let exec_mode = ExecMode::from_slug(&record.exec_mode)
        .ok_or_else(|| ProtocolError::internal("workspace has an unknown execution mode"))?;
    // §9.5 again, at the second door: the ceiling was validated when the
    // workspace was created, but a session request reaches the server on its
    // own and must not be able to ask for what the mode cannot deliver.
    if exec_mode == ExecMode::TrustedNative && requested == AutonomyMode::Autonomous {
        return Err(ProtocolError::bad_request(
            "a trusted_native workspace cannot run an autonomous session",
        ));
    }
    let ceiling = AutonomyMode::from_slug(&record.autonomy_ceiling)
        .ok_or_else(|| ProtocolError::internal("workspace has an unknown autonomy ceiling"))?;
    if requested > ceiling {
        return Err(ProtocolError::bad_request(format!(
            "autonomy '{}' exceeds the workspace ceiling '{}'",
            requested.slug(),
            ceiling.slug()
        )));
    }

    // The AUTHORITATIVE decision is the conditional INSERT in
    // `session::open_session`, which claims the slot atomically; this check runs
    // first only so an exhausted quota answers `Conflict` instead of the
    // internal error a failed claim maps to. It therefore reads the same
    // constant and the same counting rule — a second threshold here would be a
    // gate that can disagree with the one that actually decides.
    let quota = record
        .quota_sessions
        .unwrap_or(session::DEFAULT_SESSION_QUOTA);
    // A quota that cannot be evaluated refuses; it does not admit.
    let open_now =
        open_sessions_of(&record, &org.user_id).map_err(|e| db_error("open_sessions_of", e))?;
    if i64::from(open_now.open) >= quota {
        return Err(ProtocolError::new(
            ProtocolErrorCode::Conflict,
            format!("you already have {quota} open session(s) in this workspace"),
        ));
    }

    let (flow_id, flow_version_id) = resolve_harness_flow(&ctx.state.db)?;
    let new = NewSession {
        id: uuid::Uuid::new_v4().to_string(),
        user_id: org.user_id.clone(),
        user_slug: display_name(ctx, &org.user_id),
        title: title.to_string(),
        autonomy_mode: requested,
        flow_id,
        flow_version_id,
    };
    let session_id = new.id.clone();
    let workspace = record.clone();
    let opened =
        tokio::task::spawn_blocking(move || session::open_session(&workspace, role, &new))
            .await
            .map_err(|_| ProtocolError::internal("session open task failed"))?
            .map_err(|e| ProtocolError::internal(format!("{e:#}")))?;

    audit(
        ctx,
        "code_studio.session_open",
        workspace_id,
        &serde_json::json!({
            "session_id": session_id,
            "autonomy_mode": opened.autonomy_mode,
            "exec_mode": record.exec_mode,
            "branch": opened.branch,
        }),
    );

    Ok(cs(CodeStudioPayload::SessionOpenResponse {
        session: session_to_wire(opened),
    }))
}

/// Resolves the harness flow and the version a new session is pinned to. A node
/// without the flow refuses to open a session rather than pinning something
/// that is not the harness.
///
/// The pin is the ENFORCED pipeline (§16.2 C): planning argues with a critic
/// before anything is built, an implementer never works without a tester behind
/// it, and a critic judges the result against the original request — each as an
/// ordinary block on the canvas, so an operator who wants the plain tool loop
/// edits the graph rather than asking for a setting.
///
/// The id comes from the seed rather than a copy here: the seeded graph and the
/// session pin are two halves of one contract (§16), and a second literal is a
/// second thing to forget.
fn resolve_harness_flow(db: &DbPool) -> Result<(String, String), ProtocolError> {
    let flow = crate::db::repository::get_flow(db, CODE_HARNESS_CRITIC_FLOW_ID)
        .map_err(|e| db_error("get_flow", e))?
        .ok_or_else(|| {
            ProtocolError::new(
                ProtocolErrorCode::NotAvailable,
                "the Code Studio harness flow is not installed on this node",
            )
        })?;
    let version = crate::db::repository::list_flow_versions(db, &flow.id)
        .map_err(|e| db_error("list_flow_versions", e))?
        .into_iter()
        .next()
        .ok_or_else(|| {
            ProtocolError::new(
                ProtocolErrorCode::NotAvailable,
                "the Code Studio harness flow has no saved version to pin a session to",
            )
        })?;
    Ok((flow.id, version.id))
}

async fn session_close_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _) = require_workspace(
        ctx,
        org,
        workspace_id,
        Access::Member(WorkspaceRole::Editor),
    )?;
    require_local(ctx, &record)?;
    let pool = open_workspace_pool(&record)?;
    require_own_session(&pool, session_id, &org.user_id)?;

    let workspace_id_owned = workspace_id.to_string();
    let session_id_owned = session_id.to_string();
    let closing_pool = pool.clone();
    tokio::task::spawn_blocking(move || {
        session::close_session(&workspace_id_owned, &closing_pool, &session_id_owned)
    })
    .await
    .map_err(|_| ProtocolError::internal("session close task failed"))?
    .map_err(|e| ProtocolError::new(ProtocolErrorCode::Conflict, format!("{e:#}")))?;

    // Closing the session closes its streams NOW rather than letting the
    // producer notice at its next revalidation: a reader left hanging for five
    // seconds on a session that no longer exists is exactly the kind of stale
    // surface §12.2 asks to be torn down with a named reason.
    crate::code_studio::mesh_stream::hub().close_session(
        session_id,
        crate::code_studio::mesh_stream::REASON_SESSION_CLOSED,
        "the session was closed by its owner",
    );

    audit(
        ctx,
        "code_studio.session_close",
        workspace_id,
        &serde_json::json!({ "session_id": session_id }),
    );

    Ok(cs(CodeStudioPayload::SessionCloseResponse {
        session_id: session_id.to_string(),
        status: "closed".to_string(),
    }))
}

fn session_autonomy_set_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    autonomy_mode: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    // §25.1: lowering is free, raising needs `session_open`, which is the
    // editor role — so the gate is the same one that opened the session.
    let (record, _) = require_workspace(
        ctx,
        org,
        workspace_id,
        Access::Member(WorkspaceRole::Editor),
    )?;
    require_local(ctx, &record)?;
    let pool = open_workspace_pool(&record)?;
    let current = require_own_session(&pool, session_id, &org.user_id)?;
    if matches!(current.status.as_str(), "closed" | "failed" | "cancelled") {
        return Err(ProtocolError::new(
            ProtocolErrorCode::Conflict,
            format!("session is '{}'", current.status),
        ));
    }

    let requested = parse_autonomy(autonomy_mode)?;
    let ceiling = AutonomyMode::from_slug(&record.autonomy_ceiling)
        .ok_or_else(|| ProtocolError::internal("workspace has an unknown autonomy ceiling"))?;
    if requested > ceiling {
        return Err(ProtocolError::bad_request(format!(
            "autonomy '{}' exceeds the workspace ceiling '{}'",
            requested.slug(),
            ceiling.slug()
        )));
    }
    let exec_mode = ExecMode::from_slug(&record.exec_mode)
        .ok_or_else(|| ProtocolError::internal("workspace has an unknown execution mode"))?;
    if exec_mode == ExecMode::TrustedNative && requested == AutonomyMode::Autonomous {
        return Err(ProtocolError::bad_request(
            "a trusted_native workspace cannot run an autonomous session",
        ));
    }

    {
        let conn = pool
            .write()
            .map_err(|e| db_error("session_autonomy_set", anyhow::anyhow!("{e}")))?;
        conn.execute(
            "UPDATE sessions SET autonomy_mode = ?2, updated_at = datetime('now') WHERE id = ?1",
            rusqlite::params![session_id, requested.slug()],
        )
        .map_err(|e| db_error("session_autonomy_set", anyhow::anyhow!("{e}")))?;
    }
    audit(
        ctx,
        "code_studio.session_autonomy_set",
        workspace_id,
        &serde_json::json!({
            "session_id": session_id,
            "from": current.autonomy_mode,
            "to": requested.slug(),
        }),
    );

    Ok(cs(CodeStudioPayload::SessionAutonomySetResponse {
        session_id: session_id.to_string(),
        autonomy_mode: requested.slug().to_string(),
        autonomy_ceiling: ceiling.slug().to_string(),
    }))
}

// =============================================================================
// Read-only git views
// =============================================================================

async fn git_status_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _) = require_workspace(
        ctx,
        org,
        workspace_id,
        Access::Member(WorkspaceRole::Viewer),
    )?;
    require_local(ctx, &record)?;
    let pool = open_workspace_pool(&record)?;
    let session_record = require_own_session(&pool, session_id, &org.user_id)?;

    let workspace_id_owned = workspace_id.to_string();
    let session_id_owned = session_id.to_string();
    let raw = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<String>> {
        Broker::for_workspace(&workspace_id_owned)?.status(&session_id_owned)
    })
    .await
    .map_err(|_| ProtocolError::internal("git status task failed"))?
    .map_err(|e| ProtocolError::internal(format!("{e:#}")))?;

    Ok(cs(CodeStudioPayload::GitStatusResponse {
        branch: session_record.branch,
        // A session branch has no upstream until it is pushed, and pushing is
        // not wired yet; reporting anything but zero here would be invented.
        ahead: 0,
        behind: 0,
        entries: parse_porcelain(&raw),
    }))
}

/// Turns `git status --porcelain=v1 -z` records into wire entries. Under `-z` a
/// rename or copy emits the destination in the status record and the source as
/// the very next record, which is why this consumes the iterator by hand.
fn parse_porcelain(records: &[String]) -> Vec<GitStatusEntry> {
    let mut entries = Vec::new();
    let mut iter = records.iter();
    while let Some(record) = iter.next() {
        if record.len() < 4 {
            continue;
        }
        let mut chars = record.chars();
        let index_status = chars.next().unwrap_or(' ');
        let worktree_status = chars.next().unwrap_or(' ');
        let path = record[3..].to_string();
        let old_path = if index_status == 'R' || index_status == 'C' {
            iter.next().cloned()
        } else {
            None
        };
        entries.push(GitStatusEntry {
            path,
            index_status: index_status.to_string(),
            worktree_status: worktree_status.to_string(),
            old_path,
        });
    }
    entries
}

fn worktrees_list_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _) = require_workspace(
        ctx,
        org,
        workspace_id,
        Access::Member(WorkspaceRole::Viewer),
    )?;
    require_local(ctx, &record)?;
    let pool = open_workspace_pool(&record)?;
    require_own_session(&pool, session_id, &org.user_id)?;

    let conn = pool
        .read()
        .map_err(|e| db_error("worktrees_list", anyhow::anyhow!("{e}")))?;
    let mut stmt = conn
        .prepare(
            "SELECT id, session_id, purpose, op_id, branch, head_commit, base_commit, state, \
              created_at, conflict_paths FROM worktrees WHERE session_id = ?1 ORDER BY created_at",
        )
        .map_err(|e| db_error("worktrees_list", anyhow::anyhow!("{e}")))?;
    let rows = stmt
        .query_map(rusqlite::params![session_id], |row| {
            // `path` is deliberately not selected: it is a host path on the
            // owner node and means nothing in a browser.
            Ok(WorktreeInfo {
                worktree_id: row.get(0)?,
                session_id: row.get(1)?,
                purpose: row.get(2)?,
                op_id: row.get(3)?,
                branch: row.get(4)?,
                head_commit: row.get(5)?,
                base_commit: row.get(6)?,
                state: row.get(7)?,
                created_at: row.get(8)?,
                conflict_files: decode_conflict_paths(row.get(9)?),
            })
        })
        .map_err(|e| db_error("worktrees_list", anyhow::anyhow!("{e}")))?;
    let worktrees = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| db_error("worktrees_list", anyhow::anyhow!("{e}")))?;

    Ok(cs(CodeStudioPayload::WorktreesListResponse {
        session_id: session_id.to_string(),
        worktrees,
    }))
}

// =============================================================================
// Session scope
// =============================================================================

/// Non-root user a container sandbox runs as. Fixed rather than configurable:
/// a workspace image that needs root is a workspace image that defeats the
/// mode's only real guarantee.
const CONTAINER_USER: &str = "1000:1000";

/// How long a UI-initiated command may run before the executor kills its group.
const EXEC_MAX_TIMEOUT_SECS: u32 = 3600;

/// Ceiling on how many paths one whole-worktree diff renders.
const DIFF_MAX_PATHS: usize = 200;

/// Everything a session-scoped handler resolves before it does anything: the
/// workspace, the caller's role in it, the session (already proven to be the
/// caller's own) and the runtime database.
struct Scope {
    record: WorkspaceRecord,
    role: WorkspaceRole,
    session: SessionRecord,
    /// Runtime database of the workspace: sessions, events, operations, patch
    /// sets, approvals.
    pool: DbPool,
    /// Main database: the registry, and with it the workspace-level allowlist
    /// the PEP consults.
    registry: DbPool,
    /// Agent run this call belongs to. A request from a person at a socket
    /// belongs to none, and that is what makes a run-scoped grant unusable
    /// here — it was given for a run, not for the session.
    run_id: Option<String>,
}

impl Scope {
    /// Autonomy this call really runs under: the session's mode, never above
    /// the workspace ceiling (§9.5). It is resolved per call rather than at
    /// session open, so lowering the ceiling stops work already in flight.
    fn autonomy(&self) -> Result<AutonomyMode, ProtocolError> {
        let session = AutonomyMode::from_slug(&self.session.autonomy_mode)
            .ok_or_else(|| ProtocolError::internal("session has an unknown autonomy mode"))?;
        let ceiling = AutonomyMode::from_slug(&self.record.autonomy_ceiling)
            .ok_or_else(|| ProtocolError::internal("workspace has an unknown autonomy ceiling"))?;
        Ok(session.min(ceiling))
    }

    fn broker(&self) -> Result<Broker, ProtocolError> {
        Broker::for_workspace(&self.record.id).map_err(|e| db_error("broker", e))
    }

    fn worktree(&self) -> Result<RepoHandle, ProtocolError> {
        self.broker()?
            .session(&self.session.id)
            .map_err(|e| db_error("session worktree", e))
    }

    fn root(&self) -> Result<SessionRoot, ProtocolError> {
        SessionRoot::open_session(&self.record.id, &self.session.id).map_err(fs_error)
    }

    /// Tip of the session branch — the base of every patch set and of every
    /// diff the UI shows.
    ///
    /// Resolved from git on every call, exactly as `tools::current_patch_set`
    /// does, because the branch moves under a commit and under a fast-forward
    /// pull. Reading it from a `worktrees` column that only session creation
    /// ever wrote made every later patch set describe the session from its
    /// first base and every later commit re-parent onto that base, which
    /// orphaned the commits in between (§11.5 step 5).
    fn base_commit(&self) -> Result<String, ProtocolError> {
        let broker = self.broker()?;
        broker
            .head_commit(&self.worktree()?)
            .map_err(|e| db_error("base_commit", e))
    }
}

/// Resolves a session-scoped request. Membership is checked at `min`, the
/// workspace has to be local and active, and the session has to be the
/// caller's own — an administrator gets the same NotFound as a stranger (§25.4).
fn session_scope(
    ctx: &HandlerContext,
    org: &OrgContext,
    workspace_id: &str,
    session_id: &str,
    min: WorkspaceRole,
) -> Result<Scope, ProtocolError> {
    let (record, role) = require_workspace(ctx, org, workspace_id, Access::Member(min))?;
    require_local(ctx, &record)?;
    require_active(&record)?;
    let pool = open_workspace_pool(&record)?;
    let session = require_own_session(&pool, session_id, &org.user_id)?;
    if matches!(session.status.as_str(), "closed") {
        return Err(ProtocolError::new(
            ProtocolErrorCode::Conflict,
            "session is closed",
        ));
    }
    Ok(Scope {
        record,
        role: role.unwrap_or(min),
        session,
        pool,
        registry: ctx.state.db.clone(),
        // A socket request is not inside an agent run; a run-scoped grant
        // therefore never applies to it.
        run_id: None,
    })
}

fn fs_error(error: cs_fs::FsError) -> ProtocolError {
    use cs_fs::FsError as E;
    match error {
        E::InvalidPath(reason) | E::InvalidRequest(reason) => {
            ProtocolError::bad_request(format!("invalid request: {reason}"))
        }
        E::NotFound => ProtocolError::not_found("no such file or directory"),
        E::AlreadyExists => ProtocolError::new(ProtocolErrorCode::Conflict, "already exists"),
        // A lost compare-and-swap is the one failure a caller fixes by
        // re-reading, so it must not look like a broken request.
        E::Conflict { .. } => ProtocolError::new(ProtocolErrorCode::Conflict, error.to_string()),
        E::AmbiguousEdit { .. } | E::EditNotFound { .. } => {
            ProtocolError::bad_request(error.to_string())
        }
        E::TooLarge { .. } | E::LimitExceeded(_) => ProtocolError::bad_request(error.to_string()),
        E::Denied(reason) => ProtocolError::new(ProtocolErrorCode::PolicyDenied, reason),
        E::NotADirectory | E::IsADirectory | E::NotText => {
            ProtocolError::bad_request(error.to_string())
        }
        E::Io(err) => {
            tracing::warn!(error = %err, "code studio filesystem error");
            ProtocolError::internal("filesystem error")
        }
    }
}

// =============================================================================
// Per-workspace runtime: executor, terminals and the leases they hold
// =============================================================================

/// Live objects of one workspace. The executor holds the concurrency permits,
/// so it has to be the SAME instance across requests or "four commands at once"
/// would mean four per request; the lease of a terminal has to outlive the
/// request that opened it, or the sandbox would be torn down under a running
/// shell.
///
/// The terminal REGISTRY is deliberately not owned here: it is held once per
/// process by `stream_handlers::code_studio_terminal_registry`, because the
/// stream that renders a terminal and the handler that opens it have to look at
/// the same grid. Two registries would mean a stream polling a grid no shell
/// ever writes to.
struct WorkspaceRuntime {
    executor: Arc<Executor>,
    terminal_leases: Mutex<HashMap<String, Lease>>,
}

fn workspace_runtime(record: &WorkspaceRecord) -> Result<Arc<WorkspaceRuntime>, ProtocolError> {
    static RUNTIMES: OnceLock<Mutex<HashMap<String, Arc<WorkspaceRuntime>>>> = OnceLock::new();
    let registry = RUNTIMES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = registry
        .lock()
        .map_err(|_| ProtocolError::internal("code studio runtime registry is poisoned"))?;
    if let Some(runtime) = guard.get(&record.id) {
        return Ok(Arc::clone(runtime));
    }
    let runtime = Arc::new(WorkspaceRuntime {
        // The SAME executor the agent's tool surface uses: two instances mean
        // two sets of concurrency permits — "four commands at once" per caller
        // rather than per workspace — and a cancel that cannot find the command
        // it is asked to stop.
        executor: Executor::for_workspace(&record.id),
        terminal_leases: Mutex::new(HashMap::new()),
    });
    guard.insert(record.id.clone(), Arc::clone(&runtime));
    Ok(runtime)
}

/// The one terminal registry of this workspace, shared with the stream.
fn terminal_registry(record: &WorkspaceRecord) -> Result<Arc<TerminalRegistry>, ProtocolError> {
    super::stream_handlers::code_studio_terminal_registry(&record.id)
        .map_err(|e| db_error("terminal_registry", e))
}

/// Execution mode of a workspace. One parse site, because everything that
/// branches on the mode — mount enforcement, egress promise, autonomy ceiling —
/// has to agree on the answer.
fn exec_mode_of(record: &WorkspaceRecord) -> Result<ExecMode, ProtocolError> {
    ExecMode::from_slug(&record.exec_mode)
        .ok_or_else(|| ProtocolError::internal("workspace has an unknown execution mode"))
}

fn sandbox_manager(record: &WorkspaceRecord) -> Result<SandboxManager, ProtocolError> {
    let exec_mode = exec_mode_of(record)?;
    let container = match exec_mode {
        ExecMode::Container => Some(ContainerConfig {
            runtime: ContainerRuntime::Docker,
            image: record.container_image.clone().ok_or_else(|| {
                ProtocolError::internal("container workspace without an image reached execution")
            })?,
            // A `gateway` profile needs an INTERNAL runtime network that the
            // egress gateway sits on. None is named here, so the sandbox layer
            // refuses such a profile rather than starting the container on the
            // default bridge and calling it filtered (§7.6).
            egress_network: None,
            user: CONTAINER_USER.to_string(),
        }),
        ExecMode::TrustedNative => None,
    };
    SandboxManager::for_workspace(&record.id, exec_mode, container)
        .map_err(|e| db_error("sandbox_manager", e))
}

fn sandbox_error(error: SandboxError) -> ProtocolError {
    match error {
        // Fail-closed, and that is the WHOLE answer: `cow` is the one real
        // boundary of the native mode, so a layer that cannot be built refuses
        // the command instead of running it on the live worktree. There is no
        // escalation to `rw` — no capability names one and no code performs
        // one — because a degrade would let a command write into the tree a
        // reviewer is reading, and that is a feature with its own review
        // consequences, not a missing branch of this gate.
        SandboxError::CowUnavailable { reason, .. } => ProtocolError::new(
            ProtocolErrorCode::Conflict,
            format!(
                "a copy-on-write workplace could not be built ({reason}); the command is \
                 refused rather than run on the real worktree"
            ),
        ),
        SandboxError::SharedProfileBusy { .. } => {
            ProtocolError::new(ProtocolErrorCode::Conflict, error.to_string())
        }
        SandboxError::RuntimeUnavailable(reason) => {
            ProtocolError::new(ProtocolErrorCode::NotAvailable, reason)
        }
        SandboxError::Other(err) => db_error("sandbox", err),
    }
}

// =============================================================================
// Policy enforcement point
// =============================================================================

/// Outcome of the gate. `Ask` is not a failure: the operation is SUSPENDED and
/// an approval is now pending, which is a different thing from a refusal and
/// has to stay different all the way to the UI.
enum Gate {
    Allow(pep::SandboxProfile),
    Ask {
        approval_id: String,
        summary: String,
        kind: AskKind,
    },
}

/// Stable identity of one permission question: the same capability against the
/// same target in the same session is the same question, so a retry finds the
/// pending row instead of opening a second one.
fn interaction_id(session_id: &str, cap: Capability, pattern: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for field in [session_id, cap.slug(), pattern] {
        hasher.update((field.len() as u64).to_le_bytes());
        hasher.update(field.as_bytes());
    }
    hex::encode(hasher.finalize())
}

/// A suspended operation — the ONE answer this family gives when the PEP puts a
/// question to a person, so a client has a single branch to write instead of
/// one per verb. The message names the approval so the UI can point at the
/// pending card.
///
/// A suspended call journals no operation, which is why it cannot answer with a
/// success body: `op_id` would have to carry the approval id, and an approval
/// id is not an op id (§13.1). `GitCommitResponse`'s `review_required` is a
/// different thing and stays — gate 5a does not ask a permission question, it
/// OPENS a patch set, and the answer carries the set to decide on.
fn approval_required(approval_id: &str, summary: &str) -> ProtocolError {
    ProtocolError::new(
        ProtocolErrorCode::Conflict,
        format!("approval_required:{approval_id}: {summary}"),
    )
}

fn require_allow(gate: Gate) -> Result<pep::SandboxProfile, ProtocolError> {
    match gate {
        Gate::Allow(profile) => Ok(profile),
        Gate::Ask {
            approval_id,
            summary,
            ..
        } => Err(approval_required(&approval_id, &summary)),
    }
}

/// The single authorization path of every session operation.
///
/// Everything the PEP needs is gathered first — role, autonomy, standing
/// permissions, whether a decision is waiting to be acted on — and the verdict
/// is then honoured whole: an `Allow` carries the profile the operation must
/// run in, an `AskUser` suspends it behind a real `approvals` row, and a `Deny`
/// comes back with the PEP's own reason.
fn gate(
    scope: &Scope,
    cap: Capability,
    target: &Target,
    target_label: Option<&str>,
) -> Result<Gate, ProtocolError> {
    let autonomy = scope.autonomy()?;
    // A call either names a target or it does not; an empty string is neither.
    // The worktree root arrives as `Some("")` and means "the whole tree", which
    // is the same "nothing narrower to name" a capability without a target has.
    let target_label = target_label.filter(|label| !label.is_empty());
    let pattern = pep::grant_pattern(target_label);
    let interaction = interaction_id(&scope.session.id, cap, pattern);

    // A decision already given for exactly this question.
    if let Some((approval_id, decision, decided_by)) = decided_approval(scope, &interaction)? {
        match decision.as_str() {
            "deny" => {
                // Expired on read so the same request can be asked again later;
                // keeping it would refuse forever with no way back.
                set_approval_status(scope, &approval_id, "expired")?;
                return Err(ProtocolError::new(
                    ProtocolErrorCode::PolicyDenied,
                    format!("{} was refused by {decided_by}", cap.slug()),
                ));
            }
            "allow_once" => {
                // One shot, spent here. `expired` is the table's way of saying
                // "this can no longer authorize anything".
                set_approval_status(scope, &approval_id, "expired")?;
                return Ok(Gate::Allow(granted_profile(scope, cap, target, autonomy)?));
            }
            _ => {}
        }
    }

    let ctx = SessionCtx {
        role: scope.role,
        autonomy,
        // Every handler in this file serves a person at a socket, never the
        // coordinator, so a system capability is refused by rule 1.
        is_coordinator: false,
        // Gate 5a decides a COMMIT, which publishes the session branch: the
        // decision it looks for is the work review's, never a merge review of
        // the target branch.
        has_accepted_patch_set: patch::has_accepted_patch_set(
            &scope.pool,
            &scope.session.id,
            &PatchScope::Work,
        )
        .map_err(|e| db_error("has_accepted_patch_set", e))?,
        allowlisted: allowlist_grants(scope, cap, target_label)?,
        session_granted: session_grants_match(scope, cap, target_label)?,
        run_granted: run_grant_match(scope, cap, target_label)?,
    };

    match pep::authorize(&ctx, cap, target) {
        Decision::Allow(profile) => Ok(Gate::Allow(profile)),
        Decision::Deny { reason } => Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            reason,
        )),
        Decision::AskUser { summary, kind } => {
            let approval_id = record_approval(scope, cap, pattern, &interaction, &summary)?;
            Ok(Gate::Ask {
                approval_id,
                summary,
                kind,
            })
        }
    }
}

/// Profile of an operation a person has just allowed.
///
/// The rule itself belongs to the PEP (`authorize_after_decision`): the grant
/// flag is set and the verdict is taken whole, so the profile comes from the
/// policy engine and not from a second opinion held here.
fn granted_profile(
    scope: &Scope,
    cap: Capability,
    target: &Target,
    autonomy: AutonomyMode,
) -> Result<pep::SandboxProfile, ProtocolError> {
    let ctx = SessionCtx {
        role: scope.role,
        autonomy,
        is_coordinator: false,
        has_accepted_patch_set: patch::has_accepted_patch_set(
            &scope.pool,
            &scope.session.id,
            &PatchScope::Work,
        )
        .map_err(|e| db_error("has_accepted_patch_set", e))?,
        allowlisted: false,
        session_granted: true,
        run_granted: false,
    };
    match pep::authorize_after_decision(&ctx, cap, target) {
        Decision::Allow(profile) => Ok(profile),
        Decision::Deny { reason } => Err(ProtocolError::new(
            ProtocolErrorCode::PolicyDenied,
            reason,
        )),
        // Reached when a permission was given but a DIFFERENT gate still holds
        // the call — a commit approved before its review was decided, for
        // instance. The PEP's own summary says which, and the one-shot is spent
        // either way, so the answer is fail-closed: ask again.
        Decision::AskUser { summary, .. } => {
            Err(ProtocolError::new(ProtocolErrorCode::Conflict, summary))
        }
    }
}

fn decided_approval(
    scope: &Scope,
    interaction: &str,
) -> Result<Option<(String, String, String)>, ProtocolError> {
    let conn = scope
        .pool
        .read()
        .map_err(|e| db_error("decided_approval", anyhow::anyhow!("{e}")))?;
    conn.query_row(
        "SELECT id, COALESCE(decision,''), COALESCE(decided_by,'someone') FROM approvals \
         WHERE session_id = ?1 AND interaction_id = ?2 AND status = 'decided' \
         ORDER BY decided_at DESC LIMIT 1",
        rusqlite::params![scope.session.id, interaction],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        },
    )
    .optional()
    .map_err(|e| db_error("decided_approval", anyhow::anyhow!("{e}")))
}

fn set_approval_status(
    scope: &Scope,
    approval_id: &str,
    status: &str,
) -> Result<(), ProtocolError> {
    let conn = scope
        .pool
        .write()
        .map_err(|e| db_error("set_approval_status", anyhow::anyhow!("{e}")))?;
    conn.execute(
        "UPDATE approvals SET status = ?2 WHERE id = ?1",
        rusqlite::params![approval_id, status],
    )
    .map_err(|e| db_error("set_approval_status", anyhow::anyhow!("{e}")))?;
    drop(conn);
    sync_session_waiting(scope)?;
    Ok(())
}

/// Brings `sessions.status` in step with the pending-approval set (§9.4).
///
/// `events::verify_projection` derives the same fact from the timeline, but by
/// its own contract it runs only at coordinator start. Without a producer here
/// the column never reads `waiting_user` while a person is actually being
/// asked, so `sessions_waiting` on the workspace list is pinned at zero and the
/// "waiting for you" affordance can never light up. Projection stays the repair
/// path; this is the producer, and both derive the status the same way.
fn sync_session_waiting(scope: &Scope) -> Result<(), ProtocolError> {
    let conn = scope
        .pool
        .write()
        .map_err(|e| db_error("sync_session_waiting", anyhow::anyhow!("{e}")))?;
    let pending: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM approvals WHERE session_id = ?1 AND status = 'pending'",
            rusqlite::params![scope.session.id],
            |row| row.get(0),
        )
        .map_err(|e| db_error("sync_session_waiting", anyhow::anyhow!("{e}")))?;
    let running: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM session_runs WHERE session_id = ?1 AND status = 'running'",
            rusqlite::params![scope.session.id],
            |row| row.get(0),
        )
        .map_err(|e| db_error("sync_session_waiting", anyhow::anyhow!("{e}")))?;
    let expected = if pending > 0 {
        "waiting_user"
    } else if running > 0 {
        "running"
    } else {
        "idle"
    };
    // Only the live statuses are arbitrated, exactly as in `verify_projection`:
    // a session already `closing`, `closed` or `interrupted` must not be dragged
    // back to life by a late approval bookkeeping write.
    conn.execute(
        "UPDATE sessions SET status = ?2, updated_at = datetime('now') \
         WHERE id = ?1 AND status IN ('idle','running','waiting_user')",
        rusqlite::params![scope.session.id, expected],
    )
    .map_err(|e| db_error("sync_session_waiting", anyhow::anyhow!("{e}")))?;
    Ok(())
}

/// Opens (or finds) the permission question and records that it was asked.
/// The `approval_requested` event is security-relevant, so it reaches the audit
/// outbox by construction.
fn record_approval(
    scope: &Scope,
    cap: Capability,
    pattern: &str,
    interaction: &str,
    summary: &str,
) -> Result<String, ProtocolError> {
    // The row this writes is what an `always` or `allow_for_session` answer is
    // later stored FROM, so a pattern no grant may carry must not be recorded
    // here either.
    pep::validate_grant_pattern(pattern).map_err(ProtocolError::bad_request)?;
    if let Some(existing) = {
        let conn = scope
            .pool
            .read()
            .map_err(|e| db_error("record_approval", anyhow::anyhow!("{e}")))?;
        conn.query_row(
            "SELECT id FROM approvals WHERE session_id = ?1 AND interaction_id = ?2 \
             AND status = 'pending'",
            rusqlite::params![scope.session.id, interaction],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| db_error("record_approval", anyhow::anyhow!("{e}")))?
    } {
        return Ok(existing);
    }

    let approval_id = uuid::Uuid::new_v4().to_string();
    {
        let conn = scope
            .pool
            .write()
            .map_err(|e| db_error("record_approval", anyhow::anyhow!("{e}")))?;
        // The TARGET is stored as the pattern a grant would be written from
        // (§9.1 — the object of a permission is capability + target) next to
        // the digest that recognizes the same question again. Without the
        // pattern the decision path has nothing to store and falls back to `*`,
        // which turns "yes, run cargo" into "yes, run anything".
        conn.execute(
            "INSERT INTO approvals \
               (id, session_id, run_id, interaction_id, capability, target_digest, \
                target_pattern, summary, status, requested_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', datetime('now'))",
            rusqlite::params![
                approval_id,
                scope.session.id,
                scope.run_id,
                interaction,
                cap.slug(),
                pep::target_digest(cap, pattern),
                pattern,
                summary
            ],
        )
        .map_err(|e| db_error("record_approval", anyhow::anyhow!("{e}")))?;
    }
    append_event(
        scope,
        events::approval_requested_key(&approval_id),
        EventPayload::ApprovalRequested {
            approval_id: approval_id.clone(),
            capability: cap.slug().to_string(),
            summary: summary.to_string(),
        },
    )?;
    sync_session_waiting(scope)?;
    Ok(approval_id)
}

/// Hands the decision to the call that is parked on this card, and says whether
/// anything was actually waiting.
///
/// A suspended tool call, a `delegate_cli` turn and a vendor CLI's own request
/// all park on the interaction registry, and the `approvals` row records the id
/// they park on — so answering the card IS the resume, and without this the
/// dashboard's only answer channel decided a row nobody was reading.
///
/// Three outcomes are all normal and none of them is an error:
///   * the id belongs to no registered interaction — the card came from the
///     dashboard's own request path (`record_approval` mints a deterministic
///     id, not a registry one), which re-sends the request itself;
///   * the id was registered but the awaiter is gone — the run timed out or was
///     cancelled while the person deliberated;
///   * the reply lands and the run continues.
/// The caller reports the difference instead of failing on it.
fn resume_parked_run(interaction_id: &str, decision: &str) -> bool {
    let reply = match decision {
        "allow_once" => PermissionDecision::AllowOnce,
        "allow_for_run" => PermissionDecision::AllowForRun,
        // `session_grants` already holds the standing answer at this point, and
        // §9.3 rule 7 is what spends it. The parked call only needs to proceed
        // once; telling it "always" would make it write a WORKSPACE allowlist
        // row the operator never asked for.
        "allow_for_session" => PermissionDecision::AllowOnce,
        "always" => PermissionDecision::Always,
        "deny" => PermissionDecision::Deny,
        // Unreachable: the handler validates the vocabulary before it gets
        // here. Refusing to invent a reply keeps that true.
        _ => return false,
    };
    crate::agents::interaction_registry_global()
        .reply(interaction_id, InteractionReply::Permission(reply))
}

/// Standing workspace-level permission (§9.1 `always`), from the registry, and
/// the session-scoped one from the runtime db.
///
/// Both come from `code_studio::tools` rather than from a copy kept here: the
/// dashboard and the agent read the SAME rows, and a row that authorized one
/// caller while staying invisible to the other is not one permission.
fn allowlist_grants(
    scope: &Scope,
    cap: Capability,
    target: Option<&str>,
) -> Result<bool, ProtocolError> {
    tools::allowlist_holds(&scope.registry, &scope.record.id, cap, target)
        .map_err(|e| db_error("allowlist_grants", e))
}

fn session_grants_match(
    scope: &Scope,
    cap: Capability,
    target: Option<&str>,
) -> Result<bool, ProtocolError> {
    tools::session_grant_holds(&scope.pool, &scope.session.id, cap, target)
        .map_err(|e| db_error("session_grants", e))
}

/// A run-scoped grant is a decided `allow_for_run` approval OF THE SAME RUN.
///
/// The run is bound in the query, not merely required to be non-null: without
/// that binding every `allow_for_run` an operator ever gave would keep
/// authorizing calls for the whole life of the session, which is the session
/// scope the operator deliberately did not choose. A request that arrives
/// without a run — everything the UI sends directly — can therefore never match
/// one, which is the fail-closed reading.
fn run_grant_match(
    scope: &Scope,
    cap: Capability,
    target: Option<&str>,
) -> Result<bool, ProtocolError> {
    let Some(run_id) = scope.run_id.as_deref() else {
        return Ok(false);
    };
    let conn = scope
        .pool
        .read()
        .map_err(|e| db_error("run_grant", anyhow::anyhow!("{e}")))?;
    let mut stmt = conn
        .prepare(
            "SELECT target_pattern FROM approvals WHERE session_id = ?1 AND capability = ?2 \
             AND run_id = ?3 AND status = 'decided' AND decision = 'allow_for_run'",
        )
        .map_err(|e| db_error("run_grant", anyhow::anyhow!("{e}")))?;
    let rows = stmt
        .query_map(
            rusqlite::params![scope.session.id, cap.slug(), run_id],
            |row| row.get::<_, String>(0),
        )
        .map_err(|e| db_error("run_grant", anyhow::anyhow!("{e}")))?;
    for row in rows {
        let stored = row.map_err(|e| db_error("run_grant", anyhow::anyhow!("{e}")))?;
        if pep::pattern_matches(&stored, target) {
            return Ok(true);
        }
    }
    Ok(false)
}

// =============================================================================
// Effects: the operation journal and the timeline
// =============================================================================

/// Origin of an effect started by a person at a socket. One request is one
/// origin, so two deliberate identical writes are two operations while a
/// retried frame of the same request is one.
fn request_origin(ctx: &HandlerContext) -> String {
    format!("{}:{}", ctx.connection_id, ctx.correlation_id)
}

#[allow(clippy::too_many_arguments)]
fn begin_op(
    ctx: &HandlerContext,
    scope: &Scope,
    op_kind: OpKind,
    capability: Capability,
    logical_step: &str,
    input: OperationInput,
    precondition: operations::Precondition,
    postcondition: Postcondition,
    // Profile the PEP resolved this effect to run in — `None` for the effects
    // that never enter a sandbox (a worktree write, a git object operation).
    profile: Option<pep::SandboxProfile>,
) -> Result<String, ProtocolError> {
    // A forwarded call is journaled under the identity its assertion bound, so
    // the id the operator sees and the id the owner node deduplicates by are
    // the same value. Without this a retried mesh request would open a second
    // row and run the effect twice.
    let (origin_id, logical_step) = match remote_proxy::current_remote_origin_id() {
        Some(origin) => (origin, remote_proxy::MESH_LOGICAL_STEP.to_string()),
        None => (request_origin(ctx), logical_step.to_string()),
    };
    let request = OperationRequest {
        workspace_id: scope.record.id.clone(),
        session_id: scope.session.id.clone(),
        run_id: None,
        origin_kind: OriginKind::Ui,
        origin_id,
        logical_step,
        op_kind,
        capability,
        input,
        precondition,
        postcondition,
        profile,
    };
    operations::begin(&scope.pool, &request)
        .map(|op| op.op_id)
        .map_err(|e| db_error("operations::begin", e))
}

fn complete_op(
    scope: &Scope,
    op_id: &str,
    result_oids: &[String],
    result_ref: Option<&str>,
) -> Result<(), ProtocolError> {
    operations::complete(&scope.pool, op_id, result_oids, result_ref)
        .map(|_| ())
        .map_err(|e| db_error("operations::complete", e))
}

/// Closes a failed operation and returns the error the caller should report.
/// The journal entry is written first, so a failure is never invisible.
fn fail_op(scope: &Scope, op_id: &str, error: ProtocolError) -> ProtocolError {
    if let Err(nested) = operations::fail(&scope.pool, op_id, &error.message) {
        tracing::warn!(op_id, error = %nested, "cannot journal an operation failure");
    }
    error
}

fn append_event(
    scope: &Scope,
    idempotency_key: impl Into<String>,
    payload: EventPayload,
) -> Result<(), ProtocolError> {
    events::append(
        &scope.pool,
        &scope.session.id,
        SessionEvent::new(idempotency_key, payload),
    )
    .map(|_| ())
    .map_err(|e| db_error("events::append", e))
}

fn append_git_event(
    scope: &Scope,
    op_id: &str,
    operation: GitOperation,
    refname: Option<String>,
    old_oid: Option<String>,
    new_oid: Option<String>,
    remote: Option<String>,
) -> Result<(), ProtocolError> {
    append_event(
        scope,
        format!("op:{op_id}:git"),
        EventPayload::GitOp {
            op_id: op_id.to_string(),
            operation,
            refname,
            old_oid,
            new_oid,
            remote,
        },
    )
}

/// Whether the session has a work review open. A rename reads the file it is
/// about to move only when there is a journal that needs its content.
fn work_review_is_open(scope: &Scope) -> Result<bool, ProtocolError> {
    patch::open_patch_set_for_scope(&scope.pool, &scope.session.id, &PatchScope::Work)
        .map(|set| set.is_some())
        .map_err(|e| db_error("open_patch_set", e))
}

/// Mirrors one worktree change into the open work patch set, through the ONE
/// implementation both the dashboard and the agent use (`tools::record_work_edit`).
fn record_patch_edit(
    scope: &Scope,
    path: &str,
    kind: EditKind,
    content: Option<&[u8]>,
    expect: patch::Precondition,
) -> Result<(), ProtocolError> {
    let broker = scope.broker()?;
    tools::record_work_edit(
        &scope.pool,
        &broker,
        &scope.session.id,
        path,
        kind,
        content,
        &expect,
    )
    .map_err(|e| ProtocolError::new(ProtocolErrorCode::Conflict, format!("{e:#}")))
}

// =============================================================================
// Filesystem (§8, §10)
// =============================================================================

/// Parses a wire path and answers the PEP's containment question at the same
/// time. A path that escapes the worktree is not rejected here — it is handed
/// to the PEP as an out-of-bounds target, so the refusal comes from the one
/// place that is allowed to refuse.
fn parse_target_path(raw: &str) -> (Option<RelPath>, Target) {
    match RelPath::parse(raw) {
        Ok(path) => (
            Some(path),
            Target::Path {
                inside_worktree: true,
            },
        ),
        Err(_) => (
            None,
            Target::Path {
                inside_worktree: false,
            },
        ),
    }
}

fn gated_path(
    scope: &Scope,
    cap: Capability,
    raw: &str,
) -> Result<(RelPath, pep::SandboxProfile), ProtocolError> {
    let (parsed, target) = parse_target_path(raw);
    let profile = require_allow(gate(scope, cap, &target, Some(raw))?)?;
    let path = parsed.ok_or_else(|| ProtocolError::bad_request("invalid path"))?;
    Ok((path, profile))
}

fn file_tree_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    path: &str,
    depth: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Viewer)?;
    let (rel, _) = gated_path(&scope, Capability::FsRead, path)?;
    let root = scope.root()?;
    let limit = root.limits().max_dir_entries;
    let entries = root.list(&rel, depth.max(1)).map_err(fs_error)?;
    let truncated = entries.len() >= limit;

    Ok(cs(CodeStudioPayload::FileTreeResponse {
        session_id: session_id.to_string(),
        path: rel.as_str().to_string(),
        entries: entries
            .into_iter()
            .map(|entry| FileEntryInfo {
                path: entry.path,
                kind: if entry.is_dir { "dir" } else { "file" }.to_string(),
                size: entry.size,
                is_symlink: entry.is_symlink,
            })
            .collect(),
        truncated,
    }))
}

fn file_read_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    path: &str,
    start_line: Option<u32>,
    end_line: Option<u32>,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Viewer)?;
    let (rel, _) = gated_path(&scope, Capability::FsRead, path)?;
    let range = match (start_line, end_line) {
        (Some(start), Some(end)) if end >= start => Some(LineRange {
            start: u64::from(start.max(1)),
            count: u64::from(end - start + 1),
        }),
        (Some(start), None) => Some(LineRange {
            start: u64::from(start.max(1)),
            count: u64::MAX,
        }),
        (None, None) => None,
        _ => return Err(ProtocolError::bad_request("end_line precedes start_line")),
    };
    let slice = scope.root()?.read(&rel, range).map_err(fs_error)?;

    Ok(cs(CodeStudioPayload::FileReadResponse {
        language: language_of(rel.as_str()),
        path: rel.as_str().to_string(),
        content: slice.content,
        blob_sha: slice.blob_sha,
        truncated: slice.truncated,
        total_lines: slice.total_lines.min(u64::from(u32::MAX)) as u32,
    }))
}

/// Language tag for the editor. Extension based on purpose: the content is the
/// user's, and sniffing it would only produce a different wrong answer.
fn language_of(path: &str) -> Option<String> {
    let ext = path.rsplit_once('.').map(|(_, ext)| ext)?;
    let lang = match ext {
        "rs" => "rust",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" => "typescript",
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "cs" => "csharp",
        "c" | "h" => "c",
        "cc" | "cpp" | "hpp" => "cpp",
        "sh" | "bash" => "shell",
        "sql" => "sql",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "json" => "json",
        "md" => "markdown",
        "html" => "html",
        "css" => "css",
        _ => return None,
    };
    Some(lang.to_string())
}

fn file_write_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    path: &str,
    content: &str,
    expected_blob_sha: Option<&str>,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Editor)?;
    let (rel, _profile) = gated_path(&scope, Capability::FsWrite, path)?;

    let bytes = content.as_bytes();
    let expect = match expected_blob_sha {
        Some(sha) => cs_fs::Precondition::BlobIs(sha.to_string()),
        None => cs_fs::Precondition::Absent,
    };
    let new_blob = cs_fs::blob_sha(bytes);
    let stored = artifacts::put(&scope.pool, &scope.record.id, bytes, "file_content")
        .map_err(|e| db_error("artifacts::put", e))?;

    let op_id = begin_op(
        ctx,
        &scope,
        OpKind::FsWrite,
        Capability::FsWrite,
        &format!("fs_write:{}", rel.as_str()),
        OperationInput::FileContent {
            path: rel.as_str().to_string(),
            content_sha256: stored.sha256,
            size_bytes: bytes.len() as u64,
        },
        match expected_blob_sha {
            Some(sha) => operations::Precondition::FileBlobIs {
                path: rel.as_str().to_string(),
                sha256: sha.to_string(),
            },
            None => operations::Precondition::FileAbsent {
                path: rel.as_str().to_string(),
            },
        },
        Postcondition::FileBlobIs {
            path: rel.as_str().to_string(),
            sha256: new_blob.clone(),
        },
        None,
    )?;

    let outcome = match scope.root()?.write(&rel, bytes, expect) {
        Ok(outcome) => outcome,
        Err(error) => return Err(fail_op(&scope, &op_id, fs_error(error))),
    };
    record_patch_edit(
        &scope,
        rel.as_str(),
        if outcome.created {
            EditKind::Create
        } else {
            EditKind::Write
        },
        Some(bytes),
        match expected_blob_sha {
            Some(sha) => patch::Precondition::BlobIs(sha.to_string()),
            None => patch::Precondition::Absent,
        },
    )?;
    complete_op(&scope, &op_id, &[outcome.blob_sha.clone()], None)?;

    Ok(cs(CodeStudioPayload::FileWriteResponse {
        path: rel.as_str().to_string(),
        blob_sha: outcome.blob_sha,
        op_id,
    }))
}

fn file_create_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    path: &str,
    content: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Editor)?;
    let (rel, _) = gated_path(&scope, Capability::FsWrite, path)?;

    let bytes = content.as_bytes();
    let stored = artifacts::put(&scope.pool, &scope.record.id, bytes, "file_content")
        .map_err(|e| db_error("artifacts::put", e))?;
    let op_id = begin_op(
        ctx,
        &scope,
        OpKind::FsWrite,
        Capability::FsWrite,
        &format!("fs_create:{}", rel.as_str()),
        OperationInput::FileContent {
            path: rel.as_str().to_string(),
            content_sha256: stored.sha256,
            size_bytes: bytes.len() as u64,
        },
        operations::Precondition::FileAbsent {
            path: rel.as_str().to_string(),
        },
        Postcondition::FileBlobIs {
            path: rel.as_str().to_string(),
            sha256: cs_fs::blob_sha(bytes),
        },
        None,
    )?;

    let outcome = match scope
        .root()?
        .write(&rel, bytes, cs_fs::Precondition::Absent)
    {
        Ok(outcome) => outcome,
        Err(error) => return Err(fail_op(&scope, &op_id, fs_error(error))),
    };
    record_patch_edit(
        &scope,
        rel.as_str(),
        EditKind::Create,
        Some(bytes),
        patch::Precondition::Absent,
    )?;
    complete_op(&scope, &op_id, &[outcome.blob_sha.clone()], None)?;

    Ok(cs(CodeStudioPayload::FileMutationResponse {
        path: rel.as_str().to_string(),
        blob_sha: Some(outcome.blob_sha),
        op_id,
    }))
}

fn file_delete_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    path: &str,
    recursive: bool,
    expected_blob_sha: Option<&str>,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Editor)?;
    let (rel, _) = gated_path(&scope, Capability::FsDelete, path)?;

    let expect = match expected_blob_sha {
        Some(sha) => cs_fs::Precondition::BlobIs(sha.to_string()),
        None => cs_fs::Precondition::Any,
    };
    let op_id = begin_op(
        ctx,
        &scope,
        OpKind::FsDelete,
        Capability::FsDelete,
        &format!("fs_delete:{}", rel.as_str()),
        OperationInput::Params(
            [("path".to_string(), rel.as_str().to_string())]
                .into_iter()
                .collect(),
        ),
        match expected_blob_sha {
            Some(sha) => operations::Precondition::FileBlobIs {
                path: rel.as_str().to_string(),
                sha256: sha.to_string(),
            },
            None => operations::Precondition::None,
        },
        Postcondition::FileAbsent {
            path: rel.as_str().to_string(),
        },
        None,
    )?;

    if let Err(error) = scope.root()?.remove(&rel, recursive, expect) {
        return Err(fail_op(&scope, &op_id, fs_error(error)));
    }
    record_patch_edit(
        &scope,
        rel.as_str(),
        EditKind::Delete,
        None,
        match expected_blob_sha {
            Some(sha) => patch::Precondition::BlobIs(sha.to_string()),
            None => patch::Precondition::Absent,
        },
    )?;
    complete_op(&scope, &op_id, &[], None)?;

    Ok(cs(CodeStudioPayload::FileMutationResponse {
        path: rel.as_str().to_string(),
        blob_sha: None,
        op_id,
    }))
}

fn file_rename_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    from_path: &str,
    to_path: &str,
    expected_blob_sha: Option<&str>,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Editor)?;
    let (from, _) = gated_path(&scope, Capability::FsWrite, from_path)?;
    let (to, _) = gated_path(&scope, Capability::FsWrite, to_path)?;

    let root = scope.root()?;
    // Read before moving: a rename inside an open review still has to name the
    // content the new path ends up holding.
    let carried = if work_review_is_open(&scope)? {
        root.read(&from, None).ok().map(|slice| slice.content)
    } else {
        None
    };
    let expect = match expected_blob_sha {
        Some(sha) => cs_fs::Precondition::BlobIs(sha.to_string()),
        None => cs_fs::Precondition::Any,
    };
    let op_id = begin_op(
        ctx,
        &scope,
        OpKind::FsWrite,
        Capability::FsWrite,
        &format!("fs_rename:{}->{}", from.as_str(), to.as_str()),
        OperationInput::Params(
            [
                ("from".to_string(), from.as_str().to_string()),
                ("to".to_string(), to.as_str().to_string()),
            ]
            .into_iter()
            .collect(),
        ),
        operations::Precondition::FileAbsent {
            path: to.as_str().to_string(),
        },
        Postcondition::FileAbsent {
            path: from.as_str().to_string(),
        },
        None,
    )?;

    if let Err(error) = root.rename(&from, &to, expect) {
        return Err(fail_op(&scope, &op_id, fs_error(error)));
    }
    if let Some(content) = carried {
        record_patch_edit(
            &scope,
            from.as_str(),
            EditKind::Rename {
                new_path: to.as_str().to_string(),
            },
            Some(content.as_bytes()),
            match expected_blob_sha {
                Some(sha) => patch::Precondition::BlobIs(sha.to_string()),
                None => patch::Precondition::Absent,
            },
        )?;
    }
    complete_op(&scope, &op_id, &[], None)?;

    Ok(cs(CodeStudioPayload::FileMutationResponse {
        path: to.as_str().to_string(),
        blob_sha: None,
        op_id,
    }))
}

fn file_mkdir_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    path: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Editor)?;
    let (rel, _) = gated_path(&scope, Capability::FsWrite, path)?;

    let op_id = begin_op(
        ctx,
        &scope,
        OpKind::FsMkdir,
        Capability::FsWrite,
        &format!("fs_mkdir:{}", rel.as_str()),
        OperationInput::Params(
            [("path".to_string(), rel.as_str().to_string())]
                .into_iter()
                .collect(),
        ),
        operations::Precondition::None,
        // A directory is not a git object, so there is nothing content
        // addressed to verify afterwards.
        Postcondition::None,
        None,
    )?;
    if let Err(error) = scope.root()?.mkdir(&rel) {
        return Err(fail_op(&scope, &op_id, fs_error(error)));
    }
    complete_op(&scope, &op_id, &[], None)?;

    Ok(cs(CodeStudioPayload::FileMutationResponse {
        path: rel.as_str().to_string(),
        blob_sha: None,
        op_id,
    }))
}

fn file_grep_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    query: &str,
    glob: &str,
    regex: bool,
    max_results: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Viewer)?;
    require_allow(gate(&scope, Capability::FsRead, &Target::None, None)?)?;
    if query.is_empty() {
        return Err(ProtocolError::bad_request("an empty search matches nothing"));
    }

    let root = scope.root()?;
    let result = root
        .grep(&GrepQuery {
            pattern: query.to_string(),
            is_regex: regex,
            glob: (!glob.is_empty()).then(|| glob.to_string()),
            max_results: max_results.clamp(1, 1000) as usize,
            max_bytes_per_file: root.limits().max_read_bytes,
        })
        .map_err(fs_error)?;

    Ok(cs(CodeStudioPayload::FileGrepResponse {
        hits: result
            .hits
            .into_iter()
            .map(|hit| GrepHitInfo {
                path: hit.path,
                line: hit.line.min(u64::from(u32::MAX)) as u32,
                column: hit.column.min(u64::from(u32::MAX)) as u32,
                text: hit.text,
            })
            .collect(),
        truncated: result.truncated,
    }))
}

// =============================================================================
// Git broker (§10, §11)
// =============================================================================

fn git_log_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    path: &str,
    limit: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Viewer)?;
    require_allow(gate(&scope, Capability::GitRead, &Target::None, None)?)?;
    let handle = scope.worktree()?;
    let commits = scope
        .broker()?
        .log(&handle, path, limit.max(1))
        .map_err(|e| db_error("git log", e))?;

    Ok(cs(CodeStudioPayload::GitLogResponse {
        commits: commits
            .into_iter()
            .map(|entry| GitCommitInfo {
                oid: entry.oid,
                short_oid: entry.short_oid,
                author: entry.author,
                date: entry.date,
                subject: entry.subject,
            })
            .collect(),
    }))
}

fn git_branches_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Viewer)?;
    require_allow(gate(&scope, Capability::GitRead, &Target::None, None)?)?;
    let handle = scope.worktree()?;
    let branches = scope
        .broker()?
        .branches(&handle)
        .map_err(|e| db_error("git branches", e))?;

    Ok(cs(CodeStudioPayload::GitBranchesResponse {
        branches: branches
            .into_iter()
            .map(|line| GitBranchInfo {
                is_session: line.name == scope.session.branch,
                name: line.name,
                is_current: line.is_current,
                upstream: line.upstream,
                ahead: line.ahead,
                behind: line.behind,
            })
            .collect(),
    }))
}

fn git_diff_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    path: &str,
    staged: bool,
    base: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Viewer)?;
    require_allow(gate(&scope, Capability::GitRead, &Target::None, None)?)?;
    if staged {
        // There is no staging area to diff: a commit is assembled from the
        // blobs a review accepted, never from an index (§11.5).
        return Err(ProtocolError::bad_request(
            "Code Studio has no staging area; a commit is built from the accepted patch set",
        ));
    }

    let broker = scope.broker()?;
    let handle = scope.worktree()?;
    let base_commit = if base.is_empty() {
        scope.base_commit()?
    } else {
        base.to_string()
    };
    // The worktree is frozen into a tree first, so the diff describes one
    // immutable state instead of a directory that keeps moving under it.
    let head = broker
        .snapshot_worktree(&handle, &base_commit)
        .map_err(|e| db_error("snapshot_worktree", e))?;

    let paths: Vec<String> = if path.is_empty() {
        broker
            .diff_name_status(&handle, &base_commit, &head)
            .map_err(|e| db_error("diff_name_status", e))?
            .into_iter()
            .map(|entry| entry.path)
            .take(DIFF_MAX_PATHS)
            .collect()
    } else {
        vec![path.to_string()]
    };
    let truncated = path.is_empty() && paths.len() == DIFF_MAX_PATHS;

    let mut diff_text = String::new();
    for target in &paths {
        diff_text.push_str(
            &broker
                .diff_patch(&handle, &base_commit, &head, target)
                .map_err(|e| db_error("diff_patch", e))?,
        );
    }

    Ok(cs(CodeStudioPayload::GitDiffResponse {
        hunks: split_hunks(&diff_text),
        path: path.to_string(),
        diff_text,
        truncated,
    }))
}

/// Splits a unified diff into its hunks. The `@@` line travels in `header` and
/// the body in `content`, which is what the review UI addresses per hunk.
fn split_hunks(diff_text: &str) -> Vec<DiffHunkInfo> {
    let mut hunks: Vec<DiffHunkInfo> = Vec::new();
    let mut current: Option<DiffHunkInfo> = None;
    for line in diff_text.lines() {
        if line.starts_with("@@") {
            if let Some(hunk) = current.take() {
                hunks.push(hunk);
            }
            current = Some(DiffHunkInfo {
                idx: hunks.len() as u32,
                header: line.to_string(),
                content: String::new(),
            });
        } else if let Some(hunk) = current.as_mut() {
            hunk.content.push_str(line);
            hunk.content.push('\n');
        }
    }
    if let Some(hunk) = current {
        hunks.push(hunk);
    }
    hunks
}

fn commit_identity(ctx: &HandlerContext, org: &OrgContext) -> CommitIdentity {
    let email = crate::db::repository::get_user_email_by_id(&ctx.state.db, &org.user_id)
        .ok()
        .flatten()
        .filter(|value| value.contains('@'))
        .unwrap_or_else(|| format!("{}@code-studio.invalid", org.user_id));
    CommitIdentity {
        name: display_name(ctx, &org.user_id),
        email,
    }
}

fn git_commit_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    message: &str,
    patch_set_id: Option<&str>,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Editor)?;
    let message = message.trim();
    if message.is_empty() || message.len() > 8192 {
        return Err(ProtocolError::bad_request(
            "a commit message must be 1-8192 characters",
        ));
    }

    let branch = scope.session.branch.clone();
    let target = Target::Branch {
        is_session_branch: true,
    };
    match gate(&scope, Capability::GitCommit, &target, Some(&branch))? {
        // Gate 5a: the agent did the right thing, the human decision is what is
        // missing — so the answer is a REVIEW, not a refusal, and the patch set
        // it opens is the thing to decide on.
        Gate::Ask {
            kind: AskKind::PatchReview,
            approval_id,
            ..
        } => {
            let base_commit = scope.base_commit()?;
            let broker = scope.broker()?;
            let set = patch::open_patch_set(
                &scope.pool,
                &broker,
                &scope.session.id,
                None,
                &base_commit,
                &PatchScope::Work,
            )
            .map_err(|e| db_error("open_patch_set", e))?;
            append_event(
                &scope,
                format!("patch:{}:opened", set.id),
                EventPayload::PatchSetOpened {
                    patch_set_id: set.id.clone(),
                    files: set.files.len() as u32,
                },
            )?;
            // §9.5 — in `autonomous`, and only where the workspace says so, the
            // acceptance itself may be automatic. The gate above still ran and
            // the patch set still exists, so the commit below is built from
            // accepted blobs like every other one.
            if !set.files.is_empty() && review_is_decided_automatically(&scope)? {
                auto_accept_review(ctx, &scope, &set, &approval_id)?;
                return commit_accepted_blobs(ctx, org, &scope, &branch, message, &set.id);
            }
            // The question now names the set it decides, so the UI opens THIS
            // review rather than guessing at the newest open one.
            {
                let conn = scope
                    .pool
                    .write()
                    .map_err(|e| db_error("approval_detail", anyhow::anyhow!("{e}")))?;
                conn.execute(
                    "UPDATE approvals SET patch_set_id = ?2 WHERE id = ?1",
                    rusqlite::params![approval_id, set.id],
                )
                .map_err(|e| db_error("approval_detail", anyhow::anyhow!("{e}")))?;
            }
            Ok(cs(CodeStudioPayload::GitCommitResponse {
                op_id: operations::op_id(
                    &scope.session.id,
                    OriginKind::Ui,
                    &request_origin(ctx),
                    "git_commit",
                ),
                status: "review_required".to_string(),
                commit_oid: None,
                patch_set_id: Some(set.id),
            }))
        }
        Gate::Ask {
            approval_id,
            summary,
            ..
        } => Err(approval_required(&approval_id, &summary)),
        // The review is the interaction: accepting it decides the approval gate
        // 5a opened, and the next call arrives here. Nothing about the commit
        // reads the worktree — the content comes from the accepted blobs.
        Gate::Allow(_) => {
            // A patch set id off the wire is not an authorisation: it has to be
            // a WORK review of THIS session, so it is resolved through the
            // scoped selector rather than trusted.
            let set_id = match patch_set_id {
                Some(id) => patch::load_patch_set_for(
                    &scope.pool,
                    &scope.session.id,
                    &PatchScope::Work,
                    id,
                )
                .map_err(|e| ProtocolError::bad_request(format!("{e:#}")))?
                .id,
                None => accepted_patch_set_id(&scope)?,
            };
            commit_accepted_blobs(ctx, org, &scope, &branch, message, &set_id)
        }
    }
}

/// Builds and publishes the commit of an ALREADY ACCEPTED patch set (§11.5).
///
/// Nothing here reads the worktree: the content comes from `accepted_blob_sha`,
/// which is why the same body serves the human review and the automatic
/// acceptance of §9.5 — an auto-accepted commit that took a different route
/// would be the one commit in the system whose content nobody froze.
fn commit_accepted_blobs(
    ctx: &HandlerContext,
    org: &OrgContext,
    scope: &Scope,
    branch: &str,
    message: &str,
    set_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let broker = scope.broker()?;
    let reference = broker.reference();
    let refname = format!("refs/heads/{branch}");
    let expected_old = broker
        .read_ref(&reference, &refname)
        .map_err(|e| db_error("read_ref", e))?;
    let identity = commit_identity(ctx, org);
    let spec = patch::accepted_commit_spec(
        &scope.pool,
        &broker,
        set_id,
        &CommitRequest {
            branch: branch.to_string(),
            expected_old: expected_old.clone(),
            message: message.to_string(),
            author: identity.clone(),
            committer: identity,
            extra_parent: None,
        },
    )
    .map_err(|e| ProtocolError::bad_request(format!("{e:#}")))?;

    // The outcome IS knowable before git runs: the tree of the accepted blobs
    // on top of the base commit. Journalling it is what lets a crash between
    // "operation opened" and "commit published" be resolved by finding that
    // commit instead of asking a person.
    let planned_tree = broker
        .plan_tree(&reference, &spec)
        .map_err(|e| ProtocolError::new(ProtocolErrorCode::Conflict, format!("{e:#}")))?;
    let op_id = begin_op(
        ctx,
        scope,
        OpKind::GitCommit,
        Capability::GitCommit,
        "git_commit",
        OperationInput::Git {
            operation: "commit".to_string(),
            refname: Some(refname.clone()),
            remote: None,
            oids: expected_old.clone().into_iter().collect(),
        },
        match &expected_old {
            Some(oid) => operations::Precondition::RefEquals {
                refname: refname.clone(),
                oid: oid.clone(),
            },
            None => operations::Precondition::None,
        },
        Postcondition::CommitExists {
            tree: planned_tree,
            parent: Some(spec.base_commit.clone()),
        },
        None,
    )?;

    let outcome = match broker.build_commit(&reference, &spec) {
        Ok(outcome) => outcome,
        Err(error) => {
            return Err(fail_op(
                scope,
                &op_id,
                ProtocolError::new(ProtocolErrorCode::Conflict, format!("{error:#}")),
            ))
        }
    };
    operations::record_oids(
        &scope.pool,
        &op_id,
        &[outcome.commit_oid.clone(), outcome.tree_oid.clone()],
    )
    .map_err(|e| db_error("record_oids", e))?;
    record_session_head(
        &scope.pool,
        &broker,
        &scope.session.id,
        &outcome.commit_oid,
    );
    patch::mark_consumed(&scope.pool, set_id, &outcome)
        .map_err(|e| db_error("mark_consumed", e))?;
    complete_op(scope, &op_id, &[outcome.commit_oid.clone()], None)?;
    append_git_event(
        scope,
        &op_id,
        GitOperation::Commit,
        Some(refname),
        outcome.ref_before.clone(),
        Some(outcome.ref_after.clone()),
        None,
    )?;

    Ok(cs(CodeStudioPayload::GitCommitResponse {
        op_id,
        status: "committed".to_string(),
        commit_oid: Some(outcome.commit_oid),
        patch_set_id: Some(set_id.to_string()),
    }))
}

/// §9.5 — whether this workspace lets an `autonomous` session decide its own
/// reviews.
///
/// The switch is the workspace-level standing permission for `review_decide`
/// (§9.1 `always`, `code_workspace_allowlist`), not a new setting: that table
/// already IS "what this workspace permits without asking", writing to it is
/// owner-only and audited, and an absent row is the OFF the plan asks for as
/// the default. The mode is an AND with it — a standing grant alone never
/// accepts anything below `autonomous`, and a session above the workspace
/// ceiling cannot exist (`Scope::autonomy`).
///
/// The question names no target, so only a blanket `*` row answers it: an entry
/// scoped to one patch set is not a decision about the next one.
fn review_is_decided_automatically(scope: &Scope) -> Result<bool, ProtocolError> {
    if scope.autonomy()? != AutonomyMode::Autonomous {
        return Ok(false);
    }
    allowlist_grants(scope, Capability::ReviewDecide, None)
}

/// Accepts a whole patch set on the operator's behalf (§9.5).
///
/// It goes through `patch::decide` — the SAME call the human review makes — so
/// every file ends up with an `accepted_blob_sha` and the commit that follows is
/// built from frozen blobs. An acceptance that wrote the patch set's status
/// itself would leave the commit reading the worktree, which is the one thing
/// §11.5 forbids.
fn auto_accept_review(
    ctx: &HandlerContext,
    scope: &Scope,
    set: &patch::PatchSet,
    approval_id: &str,
) -> Result<(), ProtocolError> {
    let broker = scope.broker()?;
    let outcome = patch::decide(
        &scope.pool,
        &broker,
        &set.id,
        &Decisions {
            decided_by: scope.session.user_id.clone(),
            files: set
                .files
                .iter()
                .map(|file| (file.path.clone(), FileVerdict::Accept))
                .collect(),
        },
    )
    .map_err(|e| ProtocolError::new(ProtocolErrorCode::Conflict, format!("{e:#}")))?;
    // A whole-file acceptance composes nothing, so a conflict here means the
    // content moved under the review. Fail closed and let the human look.
    if !outcome.conflicted.is_empty() {
        return Err(ProtocolError::new(
            ProtocolErrorCode::Conflict,
            "the change set moved while it was being accepted; review it by hand",
        ));
    }
    // The question gate 5a opened has just been answered, so it must not stay
    // pending — a review card nobody can decide any more.
    set_approval_status(scope, approval_id, "expired")?;
    append_event(
        scope,
        format!("patch:{}:decided", set.id),
        EventPayload::PatchDecided {
            patch_set_id: set.id.clone(),
            decision: outcome.status.clone(),
            decided_by: scope.session.user_id.clone(),
        },
    )?;
    audit(
        ctx,
        "code_studio.review_auto_accepted",
        &set.id,
        &serde_json::json!({
            "workspace_id": scope.record.id,
            "session_id": scope.session.id,
            "files": outcome.accepted.len(),
        }),
    );
    Ok(())
}

/// The decision a commit may act on: the accepted WORK review of this session.
/// A merge review is a decision about the target branch and can never be spent
/// on a commit of the session branch.
fn accepted_patch_set_id(scope: &Scope) -> Result<String, ProtocolError> {
    patch::accepted_patch_set_for_scope(&scope.pool, &scope.session.id, &PatchScope::Work)
        .map(|set| set.id)
        .map_err(|e| ProtocolError::new(ProtocolErrorCode::Conflict, format!("{e:#}")))
}

/// Credential material for a network git operation. Reading it is a
/// security-relevant event in its own right (§13.4).
fn git_auth(ctx: &HandlerContext, scope: &Scope) -> Result<GitAuth, ProtocolError> {
    let Some(secret_ref) = scope.record.secret_ref.as_deref() else {
        return Ok(GitAuth::None);
    };
    let material = vault::get_workspace_secret(&ctx.state.db, &ctx.state.settings_cipher, secret_ref)
        .map_err(|e| vault_error("get_workspace_secret", e))?;
    append_event(
        scope,
        format!("secret:{}:{}", secret_ref, scope.session.id),
        EventPayload::SecretAccess {
            secret_ref: secret_ref.to_string(),
            purpose: "git network operation".to_string(),
        },
    )?;
    Ok(match material.kind() {
        SecretKind::GitToken => GitAuth::Token(material.expose().to_string()),
        SecretKind::SshKey => GitAuth::SshKey {
            private_key: material.expose().to_string(),
            known_host: scope.record.ssh_host_fingerprint.clone(),
        },
    })
}

fn remote_url(scope: &Scope, requested: &str) -> Result<String, ProtocolError> {
    let requested = requested.trim();
    if !requested.is_empty() {
        return Ok(requested.to_string());
    }
    scope
        .record
        .repo_url
        .clone()
        .ok_or_else(|| ProtocolError::bad_request("this workspace has no remote to talk to"))
}

fn git_push_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    remote: &str,
    set_upstream: bool,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Owner)?;
    if set_upstream {
        // Not "unimplemented" — unsupportable by the design the broker exists
        // for. An upstream binds a branch to a remote NAME, and a name is
        // resolved out of the repository's own config when git dials it. §11.4
        // requires the dialed address to be the one the policy check judged, so
        // the broker pushes the URL it validated and lets no repository data
        // pick a destination. Recording a name here would create exactly the
        // indirection the policy forbids.
        return Err(ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "set_upstream_unsupported: the broker pushes the url its policy check judged (§11.4) \
             and never lets a remote name from repository config decide where a push goes, so no \
             upstream can be recorded or honoured",
        ));
    }
    let branch = scope.session.branch.clone();
    // §11.4 — the address policy belongs to the REMOTE, so it is applied to the
    // url this push will really use, including one the request named instead of
    // the workspace's own. DNS is resolved again here rather than trusted from
    // provisioning time: a name that answered with a public address when the
    // workspace was created may answer with the metadata service today.
    let remote = crate::code_studio::remote_policy::validate_remote(&remote_url(&scope, remote)?)
        .map_err(|e| ProtocolError::bad_request(format!("{e:#}")))?;
    let target = Target::Remote {
        is_private: remote.is_private,
    };
    // `git_push` is mandatory-interactive: it asks every time, whatever grants
    // exist and whatever the autonomy mode is (§9.3 rule 5). A suspended call
    // answers like every other one in this family — an error naming the
    // approval — because it journals no operation and therefore has no `op_id`
    // to put in a successful body.
    require_allow(gate(&scope, Capability::GitPush, &target, Some(&branch))?)?;

    let url = remote.url;
    let auth = git_auth(ctx, &scope)?;
    let broker = scope.broker()?;
    let handle = scope.worktree()?;
    let refname = format!("refs/heads/{branch}");
    let old_oid = broker
        .read_ref(&broker.reference(), &refname)
        .map_err(|e| db_error("read_ref", e))?;

    let op_id = begin_op(
        ctx,
        &scope,
        OpKind::GitPush,
        Capability::GitPush,
        &format!("git_push:{branch}"),
        OperationInput::Git {
            operation: "push".to_string(),
            refname: Some(refname.clone()),
            remote: Some(url.clone()),
            oids: old_oid.clone().into_iter().collect(),
        },
        match &old_oid {
            Some(oid) => operations::Precondition::RefEquals {
                refname: refname.clone(),
                oid: oid.clone(),
            },
            None => operations::Precondition::None,
        },
        // A remote reference is not verifiable from this node, which is exactly
        // why a push is non-idempotent and ends `unknown` after a crash.
        Postcondition::None,
        None,
    )?;

    match broker.push_branch(&handle, &url, &branch, &auth) {
        Ok(()) => {
            complete_op(&scope, &op_id, &old_oid.clone().into_iter().collect::<Vec<_>>(), None)?;
            append_git_event(
                &scope,
                &op_id,
                GitOperation::Push,
                Some(refname),
                old_oid.clone(),
                old_oid,
                Some(redact::redact_url(&url)),
            )?;
            Ok(cs(CodeStudioPayload::GitPushResponse {
                op_id,
                status: "pushed".to_string(),
                remote_branch: Some(branch),
                error: None,
            }))
        }
        Err(error) => {
            let message = redact::redact_text(&format!("{error:#}"));
            fail_op(&scope, &op_id, ProtocolError::internal(message.clone()));
            Ok(cs(CodeStudioPayload::GitPushResponse {
                op_id,
                status: "failed".to_string(),
                remote_branch: None,
                error: Some(message),
            }))
        }
    }
}

fn git_sync_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    mode: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Editor)?;
    if !matches!(mode, "fetch" | "pull") {
        return Err(ProtocolError::bad_request("mode must be 'fetch' or 'pull'"));
    }
    let branch = scope
        .record
        .default_branch
        .clone()
        .unwrap_or_else(|| scope.session.branch.clone());
    // Judged on the address this fetch will really dial, like `git_push_v1`.
    // A hardcoded `Host { allowlisted: true }` asserted the answer instead of
    // asking for it, so §11.4 never fired here: a repository whose name now
    // resolves to a LAN or metadata address was pulled from on a standing
    // grant. DNS is re-resolved per call for the same reason it is there.
    let remote = crate::code_studio::remote_policy::validate_remote(&remote_url(&scope, "")?)
        .map_err(|e| ProtocolError::bad_request(format!("{e:#}")))?;
    let url = remote.url.clone();
    require_allow(gate(
        &scope,
        Capability::GitNetwork,
        &Target::Remote {
            is_private: remote.is_private,
        },
        Some(&url),
    )?)?;

    let auth = git_auth(ctx, &scope)?;
    let broker = scope.broker()?;
    let handle = scope.worktree()?;
    let op_id = begin_op(
        ctx,
        &scope,
        OpKind::GitBranch,
        Capability::GitNetwork,
        &format!("git_{mode}:{branch}"),
        OperationInput::Git {
            operation: mode.to_string(),
            refname: Some(branch.clone()),
            remote: Some(url.clone()),
            oids: Vec::new(),
        },
        operations::Precondition::None,
        Postcondition::None,
        None,
    )?;

    let fetched = match mode {
        "fetch" => broker.fetch_branch(&handle, &url, &branch, &auth),
        _ => broker.pull_branch(&handle, &url, &branch, &auth),
    };
    match fetched {
        Ok(oid) => {
            // A fast-forward pull moves the session branch as surely as a
            // commit does, so the session's recorded head moves with it; a
            // fetch touches no branch and leaves it alone.
            if mode == "pull" {
                record_session_head(&scope.pool, &broker, &scope.session.id, &oid);
            }
            complete_op(&scope, &op_id, &[oid.clone()], None)?;
            append_git_event(
                &scope,
                &op_id,
                GitOperation::Fetch,
                Some(branch.clone()),
                None,
                Some(oid),
                Some(redact::redact_url(&url)),
            )?;
            let (ahead, behind) = divergence(&broker, &handle, &scope.session.branch);
            Ok(cs(CodeStudioPayload::GitSyncResponse {
                op_id,
                status: mode.to_string(),
                ahead,
                behind,
                error: None,
            }))
        }
        Err(error) => {
            let message = redact::redact_text(&format!("{error:#}"));
            fail_op(&scope, &op_id, ProtocolError::internal(message.clone()));
            Ok(cs(CodeStudioPayload::GitSyncResponse {
                op_id,
                status: "failed".to_string(),
                ahead: 0,
                behind: 0,
                error: Some(message),
            }))
        }
    }
}

/// Divergence of a branch from its upstream, or `(0, 0)` when git records no
/// upstream — which is the truth for a session branch that has never been
/// pushed, not a placeholder.
fn divergence(broker: &Broker, handle: &RepoHandle, branch: &str) -> (u32, u32) {
    broker
        .branches(handle)
        .ok()
        .and_then(|branches| {
            branches
                .into_iter()
                .find(|line| line.name == branch)
                .map(|line| (line.ahead, line.behind))
        })
        .unwrap_or((0, 0))
}

fn git_merge_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    source_branch: &str,
    target_branch: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Owner)?;
    require_allow(gate(
        &scope,
        Capability::GitMerge,
        &Target::Branch {
            is_session_branch: source_branch == scope.session.branch,
        },
        Some(target_branch),
    )?)?;

    let broker = scope.broker()?;
    let reference = broker.reference();
    let refname = format!("refs/heads/{target_branch}");
    let expected_old = broker
        .read_ref(&reference, &refname)
        .map_err(|e| db_error("read_ref", e))?
        .ok_or_else(|| ProtocolError::bad_request("the target branch does not exist"))?;
    let source_head = broker
        .read_ref(&reference, &format!("refs/heads/{source_branch}"))
        .map_err(|e| db_error("read_ref", e))?
        .ok_or_else(|| ProtocolError::bad_request("the source branch does not exist"))?;

    let op_id = begin_op(
        ctx,
        &scope,
        OpKind::GitMerge,
        Capability::GitMerge,
        &format!("git_merge:{source_branch}->{target_branch}"),
        OperationInput::Git {
            operation: "merge".to_string(),
            refname: Some(refname.clone()),
            remote: None,
            oids: vec![expected_old.clone(), source_head.clone()],
        },
        operations::Precondition::RefEquals {
            refname: refname.clone(),
            oid: expected_old.clone(),
        },
        Postcondition::None,
        None,
    )?;

    if let Err(error) = broker.add_integration_worktree(&scope.session.id, &op_id, &expected_old) {
        return Err(fail_op(
            &scope,
            &op_id,
            ProtocolError::new(ProtocolErrorCode::Conflict, format!("{error:#}")),
        ));
    }
    let outcome = match broker.merge_into_integration(&scope.session.id, source_branch) {
        Ok(outcome) => outcome,
        Err(error) => {
            return Err(fail_op(
                &scope,
                &op_id,
                ProtocolError::new(ProtocolErrorCode::Conflict, format!("{error:#}")),
            ))
        }
    };

    let worktree_id = format!("{}-int-{}", scope.session.id, &op_id[..8.min(op_id.len())]);
    let (result, patch_set_id, state) = match &outcome {
        MergeOutcome::Clean { merge_head, .. } => {
            // The merge result is pinned by a private ref so nothing collects
            // it while the review is open (§11.6).
            broker
                .write_private_ref(&op_id, merge_head)
                .map_err(|e| db_error("write_private_ref", e))?;
            let set = patch::open_patch_set(
                &scope.pool,
                &broker,
                &scope.session.id,
                None,
                &expected_old,
                &PatchScope::Merge {
                    op_id: op_id.clone(),
                },
            )
            .map_err(|e| db_error("open_patch_set", e))?;
            ("clean", Some(set.id), "clean")
        }
        // A conflict is a RESULT: the worktree stays `held` so a revision run
        // has something to work on.
        MergeOutcome::Conflict { .. } => ("conflict", None, "held"),
    };
    let conflict_files = match outcome {
        MergeOutcome::Conflict { paths } => paths,
        MergeOutcome::Clean { .. } => Vec::new(),
    };
    // The paths go to disk BEFORE the answer leaves, so the list a reload reads
    // back is the same list this call reports (§11.6 pkt 3).
    insert_integration_worktree(
        &scope,
        &worktree_id,
        &op_id,
        &expected_old,
        &source_head,
        state,
        &conflict_files,
    )?;
    complete_op(&scope, &op_id, &[source_head.clone()], None)?;
    append_git_event(
        &scope,
        &op_id,
        GitOperation::Merge,
        Some(refname),
        Some(expected_old.clone()),
        Some(source_head),
        None,
    )?;

    let steps = merge_steps(&MergeSaga {
        clean: result == "clean",
        patch_set_id: patch_set_id.as_deref(),
        publish: None,
    });
    Ok(cs(CodeStudioPayload::GitMergeResponse {
        op_id,
        outcome: result.to_string(),
        conflict_files,
        patch_set_id,
        integration_worktree: read_worktree(&scope, &worktree_id)?,
        expected_old,
        steps,
    }))
}

/// Outcome of the compare-and-swap that publishes a reviewed merge (§11.6 pkt
/// 6). Absent until a finalize has actually tried.
#[derive(Clone, Copy)]
enum MergePublish {
    Merged,
    /// The target branch moved after the reviewed merge was computed, so the
    /// whole attempt is void — never a silent retry on a fresh base.
    StaleBase,
    Failed,
}

/// What this node knows about ONE merge saga at the moment it answers about it.
struct MergeSaga<'a> {
    /// The merge itself produced a tree without stopping on conflicting paths.
    clean: bool,
    /// The set the review acts on, once one exists. A conflicted merge has none
    /// until a revision run resolves it.
    patch_set_id: Option<&'a str>,
    publish: Option<MergePublish>,
}

/// The eight steps of §11.6 in the state they are ACTUALLY in.
///
/// ONE producer for both merge answers, so a client renders one process instead
/// of stitching two lists whose names do not meet. `tests` and `review` are
/// separate steps because neither can be read off the patch set: a set is
/// `in_review` both before a verification ran and after one failed.
///
/// `tests` never reports a verdict, and that is the truth rather than a gap:
/// §11.6 pkt 4 makes verification an ordinary agent call on the integration
/// worktree, and an `exec` operation records no merge it belongs to, so nothing
/// on this node can attribute a test run to a merge. Reading one out of an
/// adjacent step's outcome would be a guess wearing a result's clothes.
fn merge_steps(saga: &MergeSaga<'_>) -> Vec<tentaflow_protocol::code_studio::MergeStepInfo> {
    use tentaflow_protocol::code_studio::MergeStepInfo;
    let step = |name: &str, status: &str, detail: Option<&str>| MergeStepInfo {
        step: name.to_string(),
        status: status.to_string(),
        detail: detail.map(str::to_string),
    };
    // A conflicted merge has nothing to review yet; the revision run that
    // resolves it opens the set, and only then do the later steps have a
    // subject.
    let unresolved = Some("conflict_resolved_in_revision_run");
    let decided = saga.publish.is_some();
    vec![
        step("integration_worktree", "done", None),
        if saga.clean {
            step("private_ref", "done", None)
        } else {
            step("private_ref", "skipped", Some("no_result_to_pin"))
        },
        if saga.clean {
            step("merge", "done", None)
        } else {
            step("merge", "failed", Some("merge_conflicted"))
        },
        match saga.patch_set_id {
            Some(_) => step("patch_set", "done", None),
            None => step("patch_set", "pending", unresolved),
        },
        step("tests", "pending", Some("tests_not_run_by_merge")),
        match (saga.patch_set_id, decided) {
            (None, _) => step("review", "pending", unresolved),
            // A finalize only runs on an ACCEPTED set, so reaching the publish
            // half IS the review having been decided.
            (Some(_), true) => step("review", "done", None),
            (Some(_), false) => step("review", "pending", Some("awaiting_review")),
        },
        match (saga.patch_set_id, decided) {
            (None, _) => step("approval", "pending", unresolved),
            (Some(_), true) => step("approval", "done", None),
            (Some(_), false) => step("approval", "pending", Some("awaiting_review")),
        },
        match (saga.publish, saga.patch_set_id) {
            (Some(MergePublish::Merged), _) => step("update_ref", "done", None),
            (Some(MergePublish::StaleBase), _) => {
                step("update_ref", "failed", Some("target_moved"))
            }
            (Some(MergePublish::Failed), _) => {
                step("update_ref", "failed", Some("update_ref_failed"))
            }
            (None, None) => step("update_ref", "pending", unresolved),
            (None, Some(_)) => step("update_ref", "pending", Some("awaiting_review")),
        },
    ]
}

fn insert_integration_worktree(
    scope: &Scope,
    worktree_id: &str,
    op_id: &str,
    expected_old: &str,
    source_head: &str,
    state: &str,
    conflict_paths: &[String],
) -> Result<(), ProtocolError> {
    let path = scope
        .broker()?
        .integration_worktree(&scope.session.id)
        .map_err(|e| db_error("integration_worktree", e))?;
    // NULL rather than "[]" for a clean merge: the column answers "which paths
    // is this worktree held on", and a clean merge holds on none.
    let conflicts = if conflict_paths.is_empty() {
        None
    } else {
        Some(
            serde_json::to_string(conflict_paths)
                .map_err(|e| db_error("insert_worktree", anyhow::anyhow!("{e}")))?,
        )
    };
    let conn = scope
        .pool
        .write()
        .map_err(|e| db_error("insert_worktree", anyhow::anyhow!("{e}")))?;
    conn.execute(
        "INSERT INTO worktrees \
           (id, session_id, purpose, op_id, path, branch, head_commit, base_commit, state, \
            created_at, conflict_paths) \
         VALUES (?1, ?2, 'integration', ?3, ?4, NULL, ?5, ?6, ?7, datetime('now'), ?8) \
         ON CONFLICT(id) DO UPDATE SET state = excluded.state, \
            conflict_paths = excluded.conflict_paths",
        rusqlite::params![
            worktree_id,
            scope.session.id,
            op_id,
            path.display().to_string(),
            expected_old,
            source_head,
            state,
            conflicts
        ],
    )
    .map_err(|e| db_error("insert_worktree", anyhow::anyhow!("{e}")))?;
    Ok(())
}

/// Decodes the stored conflict list. A row written before v8 — or by a clean
/// merge — has none, which is the same answer as an empty list to every reader.
fn decode_conflict_paths(stored: Option<String>) -> Vec<String> {
    stored
        .and_then(|json| serde_json::from_str::<Vec<String>>(&json).ok())
        .unwrap_or_default()
}

fn read_worktree(scope: &Scope, worktree_id: &str) -> Result<Option<WorktreeInfo>, ProtocolError> {
    let conn = scope
        .pool
        .read()
        .map_err(|e| db_error("read_worktree", anyhow::anyhow!("{e}")))?;
    conn.query_row(
        "SELECT id, session_id, purpose, op_id, branch, head_commit, base_commit, state, \
          created_at, conflict_paths FROM worktrees WHERE id = ?1",
        rusqlite::params![worktree_id],
        |row| {
            Ok(WorktreeInfo {
                worktree_id: row.get(0)?,
                session_id: row.get(1)?,
                purpose: row.get(2)?,
                op_id: row.get(3)?,
                branch: row.get(4)?,
                head_commit: row.get(5)?,
                base_commit: row.get(6)?,
                state: row.get(7)?,
                created_at: row.get(8)?,
                conflict_files: decode_conflict_paths(row.get(9)?),
            })
        },
    )
    .optional()
    .map_err(|e| db_error("read_worktree", anyhow::anyhow!("{e}")))
}

/// Publishes a merge the operator reviewed.
///
/// The patch set is resolved SERVER-SIDE from the merge operation this session
/// holds open: §11.6 step 5 requires a human to have seen THIS merge result,
/// and a set id off the wire proves nothing about which tree was decided — a
/// work review of the session branch would publish unreviewed content onto the
/// target branch.
fn git_merge_finalize_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    op_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Owner)?;
    let (worktree_id, expected_old, source_head) = integration_state(&scope, op_id)?;
    let patch_set_id = patch::accepted_patch_set_for_scope(
        &scope.pool,
        &scope.session.id,
        &PatchScope::Merge {
            op_id: op_id.to_string(),
        },
    )
    .map_err(|e| ProtocolError::new(ProtocolErrorCode::Conflict, format!("{e:#}")))?
    .id;
    let patch_set_id = patch_set_id.as_str();

    require_allow(gate(
        &scope,
        Capability::GitMergeFinalize,
        &Target::None,
        Some(op_id),
    )?)?;
    // The conflicting paths the merge stopped on are persisted with the
    // worktree (§11.6 pkt 3), which is what lets this answer report the merge
    // half of the saga instead of only the half it runs itself.
    let clean = read_worktree(&scope, &worktree_id)?
        .map(|worktree| worktree.conflict_files.is_empty())
        .unwrap_or(true);

    let broker = scope.broker()?;
    let identity = commit_identity(ctx, org);
    let target_branch = scope
        .record
        .target_branch
        .clone()
        .or_else(|| scope.record.default_branch.clone())
        .ok_or_else(|| ProtocolError::bad_request("the workspace has no target branch"))?;
    let spec = patch::accepted_commit_spec(
        &scope.pool,
        &broker,
        patch_set_id,
        &CommitRequest {
            branch: target_branch.clone(),
            expected_old: Some(expected_old.clone()),
            message: format!("merge {} into {target_branch}", scope.session.branch),
            author: identity.clone(),
            committer: identity,
            extra_parent: Some(source_head),
        },
    )
    .map_err(|e| ProtocolError::bad_request(format!("{e:#}")))?;

    let finalize_op = begin_op(
        ctx,
        &scope,
        OpKind::GitMergeFinalize,
        Capability::GitMergeFinalize,
        &format!("git_merge_finalize:{op_id}"),
        OperationInput::Git {
            operation: "merge_finalize".to_string(),
            refname: Some(format!("refs/heads/{target_branch}")),
            remote: None,
            oids: vec![expected_old.clone()],
        },
        operations::Precondition::RefEquals {
            refname: format!("refs/heads/{target_branch}"),
            oid: expected_old.clone(),
        },
        Postcondition::None,
        None,
    )?;

    let outcome = match broker.finalize_merge(&spec) {
        Ok(outcome) => outcome,
        Err(error) => {
            let message = format!("{error:#}");
            fail_op(&scope, &finalize_op, ProtocolError::internal(message.clone()));
            // A moved target tip is not a failure of the merge — it is the
            // compare-and-swap doing its job, and the caller re-merges.
            let stale = message.contains("expected") || message.contains("changed");
            return Ok(cs(CodeStudioPayload::GitMergeFinalizeResponse {
                op_id: op_id.to_string(),
                status: if stale { "stale_base" } else { "failed" }.to_string(),
                merge_commit_oid: None,
                error: Some(message),
                steps: merge_steps(&MergeSaga {
                    clean,
                    patch_set_id: Some(patch_set_id),
                    publish: Some(if stale {
                        MergePublish::StaleBase
                    } else {
                        MergePublish::Failed
                    }),
                }),
            }));
        }
    };

    patch::mark_consumed(&scope.pool, patch_set_id, &outcome)
        .map_err(|e| db_error("mark_consumed", e))?;
    if let Err(error) = broker.remove_integration_worktree(&scope.session.id, op_id) {
        tracing::warn!(op_id, error = %error, "integration worktree survived a finalized merge");
    }
    if let Err(error) = broker.delete_private_ref(op_id) {
        tracing::warn!(op_id, error = %error, "private merge ref survived a finalized merge");
    }
    mark_worktree_removed(&scope, &worktree_id)?;
    complete_op(&scope, &finalize_op, &[outcome.commit_oid.clone()], None)?;
    append_git_event(
        &scope,
        &finalize_op,
        GitOperation::MergeFinalize,
        Some(format!("refs/heads/{target_branch}")),
        Some(expected_old),
        Some(outcome.ref_after.clone()),
        None,
    )?;

    Ok(cs(CodeStudioPayload::GitMergeFinalizeResponse {
        op_id: finalize_op,
        status: "merged".to_string(),
        merge_commit_oid: Some(outcome.commit_oid),
        error: None,
        steps: merge_steps(&MergeSaga {
            clean,
            patch_set_id: Some(patch_set_id),
            publish: Some(MergePublish::Merged),
        }),
    }))
}

fn integration_state(
    scope: &Scope,
    op_id: &str,
) -> Result<(String, String, String), ProtocolError> {
    let conn = scope
        .pool
        .read()
        .map_err(|e| db_error("integration_state", anyhow::anyhow!("{e}")))?;
    conn.query_row(
        "SELECT id, head_commit, base_commit FROM worktrees \
         WHERE session_id = ?1 AND purpose = 'integration' AND op_id = ?2 AND state <> 'removed'",
        rusqlite::params![scope.session.id, op_id],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        },
    )
    .optional()
    .map_err(|e| db_error("integration_state", anyhow::anyhow!("{e}")))?
    .ok_or_else(|| ProtocolError::not_found("no open merge for this operation"))
}

fn mark_worktree_removed(scope: &Scope, worktree_id: &str) -> Result<(), ProtocolError> {
    let conn = scope
        .pool
        .write()
        .map_err(|e| db_error("mark_worktree_removed", anyhow::anyhow!("{e}")))?;
    conn.execute(
        "UPDATE worktrees SET state = 'removed', removed_at = datetime('now') WHERE id = ?1",
        rusqlite::params![worktree_id],
    )
    .map_err(|e| db_error("mark_worktree_removed", anyhow::anyhow!("{e}")))?;
    Ok(())
}

fn git_merge_abandon_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    op_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Owner)?;
    let (worktree_id, _, _) = integration_state(&scope, op_id)?;
    // Dropping a held merge destroys the state a revision run would resume
    // from, so it goes through the same mandatory question as the merge itself.
    require_allow(gate(&scope, Capability::GitMerge, &Target::None, Some(op_id))?)?;

    let broker = scope.broker()?;
    if let Err(error) = broker.remove_integration_worktree(&scope.session.id, op_id) {
        return Err(ProtocolError::new(
            ProtocolErrorCode::Conflict,
            format!("{error:#}"),
        ));
    }
    if let Err(error) = broker.delete_private_ref(op_id) {
        tracing::warn!(op_id, error = %error, "private merge ref survived an abandoned merge");
    }
    mark_worktree_removed(&scope, &worktree_id)?;
    append_git_event(&scope, op_id, GitOperation::Merge, None, None, None, None)?;

    // The family has no dedicated response; the worktree listing is the honest
    // answer, because it shows exactly what the abandon changed.
    worktrees_list_v1(ctx, workspace_id, session_id)
}

// =============================================================================
// Patch sets and review (§13.2)
// =============================================================================

fn patch_sets_list_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    status: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Viewer)?;
    let conn = scope
        .pool
        .read()
        .map_err(|e| db_error("patch_sets_list", anyhow::anyhow!("{e}")))?;
    let mut stmt = conn
        .prepare(
            "SELECT s.id, s.run_id, s.base_commit, s.status, s.created_at, s.decided_by, \
              s.decided_at, (SELECT COUNT(*) FROM patch_files f WHERE f.patch_set_id = s.id), \
              s.scope, s.op_id \
             FROM patch_sets s WHERE s.session_id = ?1 AND (?2 = '' OR s.status = ?2) \
             ORDER BY s.created_at DESC",
        )
        .map_err(|e| db_error("patch_sets_list", anyhow::anyhow!("{e}")))?;
    let rows = stmt
        .query_map(rusqlite::params![scope.session.id, status], |row| {
            Ok(PatchSetInfo {
                patch_set_id: row.get(0)?,
                session_id: session_id.to_string(),
                run_id: row.get(1)?,
                // Both halves of the identity come off the row: a merge review
                // decides the target branch and a work review the session
                // branch, and the reader has to tell them apart WITHOUT
                // guessing from creation time.
                scope: row.get(8)?,
                base_commit: row.get(2)?,
                status: row.get(3)?,
                created_at: row.get(4)?,
                decided_by: row.get(5)?,
                decided_at: row.get(6)?,
                file_count: row.get::<_, i64>(7)? as u32,
                op_id: row.get(9)?,
            })
        })
        .map_err(|e| db_error("patch_sets_list", anyhow::anyhow!("{e}")))?;
    let patch_sets = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| db_error("patch_sets_list", anyhow::anyhow!("{e}")))?;

    Ok(cs(CodeStudioPayload::PatchSetsListResponse {
        session_id: session_id.to_string(),
        patch_sets,
    }))
}

fn patch_set_get_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    patch_set_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Viewer)?;
    let set = patch::load_patch_set(&scope.pool, patch_set_id)
        .map_err(|e| db_error("load_patch_set", e))?;
    if set.session_id != scope.session.id {
        return Err(ProtocolError::not_found("patch set not found"));
    }
    let broker = scope.broker()?;
    let reference = broker.reference();

    let mut files = Vec::with_capacity(set.files.len());
    for file in &set.files {
        let mut hunks = Vec::with_capacity(file.hunks.len());
        for hunk in &file.hunks {
            // The hunk text lives in the object database; the row only points
            // at it, so the review has to read it back to show anything.
            let content = broker
                .cat_file(&reference, &hunk.content_ref)
                .map_err(|e| db_error("cat_file", e))?;
            let text = String::from_utf8_lossy(&content);
            // The stored blob starts with its `@@` line because `git apply`
            // needs it when accepted hunks are composed back into a patch. The
            // WIRE does not: the header is already a field of its own, and
            // repeating it as row 0 of the body made every reader recognise and
            // drop a line whose position it also had to keep out of the
            // numbering.
            let body = text.split_once('\n').map(|(_, rest)| rest).unwrap_or("");
            hunks.push(PatchHunkInfo {
                patch_hunk_id: hunk.id.clone(),
                idx: hunk.idx.max(0) as u32,
                header: hunk.header.clone(),
                content: body.to_string(),
                status: hunk.status.clone(),
            });
        }
        files.push(PatchFileInfo {
            patch_file_id: file.id.clone(),
            path: file.path.clone(),
            old_path: file.old_path.clone(),
            change_kind: file.change_kind.clone(),
            status: file.status.clone(),
            patch_base_blob_sha: file.patch_base_blob_sha.clone(),
            current_blob_sha: file.current_blob_sha.clone(),
            accepted_blob_sha: file.accepted_blob_sha.clone(),
            mode: file.mode.clone(),
            hunks,
        });
    }

    let (created_at, decided_by, decided_at) = {
        let conn = scope
            .pool
            .read()
            .map_err(|e| db_error("patch_set_get", anyhow::anyhow!("{e}")))?;
        conn.query_row(
            "SELECT created_at, decided_by, decided_at FROM patch_sets WHERE id = ?1",
            rusqlite::params![patch_set_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .map_err(|e| db_error("patch_set_get", anyhow::anyhow!("{e}")))?
    };

    Ok(cs(CodeStudioPayload::PatchSetGetResponse {
        patch_set: PatchSetInfo {
            patch_set_id: set.id.clone(),
            session_id: set.session_id.clone(),
            run_id: set.run_id.clone(),
            scope: set.scope.as_str().to_string(),
            base_commit: set.base_commit.clone(),
            status: set.status.clone(),
            file_count: files.len() as u32,
            created_at,
            decided_by,
            decided_at,
            op_id: set.scope.op_id().map(str::to_string),
        },
        files,
    }))
}

async fn patch_decide_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    patch_set_id: &str,
    decisions: &[PatchFileDecision],
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Editor)?;
    require_allow(gate(
        &scope,
        Capability::ReviewDecide,
        &Target::None,
        Some(patch_set_id),
    )?)?;

    let set = patch::load_patch_set(&scope.pool, patch_set_id)
        .map_err(|e| db_error("load_patch_set", e))?;
    if set.session_id != scope.session.id {
        return Err(ProtocolError::not_found("patch set not found"));
    }

    // §16.3 — "revise this" is neither an acceptance nor a rejection: nothing is
    // applied and nothing is thrown away, the set stays open and the agent gets
    // another run carrying the reviewer's notes. It therefore decides the WHOLE
    // set; mixing it with an acceptance in one request would leave the set half
    // decided while a run is already rewriting it.
    if decisions
        .iter()
        .any(|d| d.decision == PATCH_DECISION_REVISION)
    {
        if !decisions
            .iter()
            .all(|d| d.decision == PATCH_DECISION_REVISION)
        {
            return Err(ProtocolError::bad_request(
                "'request_revision' sends the whole change set back; it cannot be mixed with                  an accept or a reject in one decision",
            ));
        }
        return request_revision_v1(ctx, &scope, &set, decisions).await;
    }

    let mut files = Vec::with_capacity(decisions.len());
    for decision in decisions {
        let file = set
            .files
            .iter()
            .find(|f| f.id == decision.patch_file_id)
            .ok_or_else(|| {
                ProtocolError::bad_request("a decision names a file outside this patch set")
            })?;
        let verdict = match decision.decision.as_str() {
            "accept" if decision.hunks.is_empty() => FileVerdict::Accept,
            "accept" => {
                let mut accepted = Vec::new();
                for hunk_decision in &decision.hunks {
                    let hunk = file
                        .hunks
                        .iter()
                        .find(|h| h.id == hunk_decision.patch_hunk_id)
                        .ok_or_else(|| {
                            ProtocolError::bad_request("a decision names an unknown hunk")
                        })?;
                    if hunk_decision.decision == "accept" {
                        accepted.push(hunk.idx);
                    }
                }
                FileVerdict::Hunks(accepted)
            }
            "reject" => FileVerdict::Reject,
            other => {
                return Err(ProtocolError::bad_request(format!(
                    "unknown decision '{other}'"
                )))
            }
        };
        files.push((file.path.clone(), verdict));
    }

    let broker = scope.broker()?;
    let outcome = patch::decide(
        &scope.pool,
        &broker,
        patch_set_id,
        &Decisions {
            decided_by: org.user_id.clone(),
            files,
        },
    )
    .map_err(|e| ProtocolError::new(ProtocolErrorCode::Conflict, format!("{e:#}")))?;

    // A decision that changes what the worktree should hold is applied under
    // the SAME compare-and-swap the patch set uses: a file somebody edited
    // again is a conflict, not something to overwrite.
    tools::apply_review_rewrites(
        &broker,
        &scope.record.id,
        &scope.session.id,
        &set.scope,
        &outcome.rewrites,
    )
    .map_err(|e| ProtocolError::new(ProtocolErrorCode::Conflict, format!("{e:#}")))?;

    append_event(
        &scope,
        format!("patch:{patch_set_id}:decided"),
        EventPayload::PatchDecided {
            patch_set_id: patch_set_id.to_string(),
            decision: outcome.status.clone(),
            decided_by: org.user_id.clone(),
        },
    )?;

    Ok(cs(CodeStudioPayload::PatchDecideResponse {
        patch_set_id: patch_set_id.to_string(),
        status: outcome.status,
        conflicted_paths: outcome.conflicted,
    }))
}

/// Wire value of the third review outcome (§16.3).
const PATCH_DECISION_REVISION: &str = "request_revision";

/// Sends a change set back to the agent as a revision run.
///
/// The set is NOT decided: it stays open, no blob is rewritten, and the notes
/// the reviewer wrote become the run's prompt. Turning this into a rejection
/// would throw the work away, which is exactly what the reviewer did not ask for.
async fn request_revision_v1(
    ctx: &HandlerContext,
    scope: &Scope,
    set: &patch::PatchSet,
    decisions: &[PatchFileDecision],
) -> Result<MessageBody, ProtocolError> {
    let already: i64 = {
        let conn = scope
            .pool
            .read()
            .map_err(|e| db_error("revision_count", anyhow::anyhow!("{e}")))?;
        conn.query_row(
            "SELECT COUNT(*) FROM session_runs WHERE session_id = ?1 AND kind = ?2",
            rusqlite::params![scope.session.id, RUN_KIND_REVISION],
            |row| row.get(0),
        )
        .map_err(|e| db_error("revision_count", anyhow::anyhow!("{e}")))?
    };
    // Past the budget the loop does not continue on its own. The reviewer and
    // the agent can disagree indefinitely, so the next round costs a human
    // decision instead of another run nobody asked for.
    if already >= MAX_REVISION_RUNS {
        let interaction = interaction_id(
            &scope.session.id,
            Capability::ReviewDecide,
            "revision_budget",
        );
        let summary = format!(
            "this change has already been sent back {already} times; \
             approve to ask the agent for one more revision"
        );
        match decided_approval(scope, &interaction)? {
            // One extra round per answer: the permission is spent here, so the
            // budget question comes back for the round after this one.
            Some((approval_id, decision, _)) if decision != "deny" && !decision.is_empty() => {
                set_approval_status(scope, &approval_id, "expired")?;
            }
            Some((approval_id, _, decided_by)) => {
                set_approval_status(scope, &approval_id, "expired")?;
                return Err(ProtocolError::new(
                    ProtocolErrorCode::PolicyDenied,
                    format!("{decided_by} refused a further revision of this change set"),
                ));
            }
            None => {
                let approval_id = record_approval(
                    scope,
                    Capability::ReviewDecide,
                    "revision_budget",
                    &interaction,
                    &summary,
                )?;
                return Err(approval_required(&approval_id, &summary));
            }
        }
    }

    let mut notes = String::new();
    for decision in decisions {
        let Some(file) = set.files.iter().find(|f| f.id == decision.patch_file_id) else {
            return Err(ProtocolError::bad_request(
                "a decision names a file outside this patch set",
            ));
        };
        notes.push_str("- ");
        notes.push_str(&file.path);
        match decision.note.as_deref().map(str::trim).filter(|n| !n.is_empty()) {
            Some(note) => {
                notes.push_str(": ");
                notes.push_str(note);
            }
            None => notes.push_str(": needs a revision"),
        }
        notes.push('\n');
    }
    let prompt = format!(
        "The reviewer sent your change set back for revision instead of accepting or rejecting \
         it. The files are still in the worktree exactly as you left them. Address these points \
         and produce a new change set:\n\n{notes}"
    );

    let run_id = start_session_run(
        ctx,
        scope,
        RUN_KIND_REVISION,
        RUN_TRIGGER_REVIEW_REJECTED,
        &prompt,
        None,
    )
    .await?;
    append_event(
        scope,
        format!("patch:{}:revision:{run_id}", set.id),
        EventPayload::PatchDecided {
            patch_set_id: set.id.clone(),
            decision: PATCH_DECISION_REVISION.to_string(),
            decided_by: require_org(ctx)?.user_id.clone(),
        },
    )?;

    Ok(cs(CodeStudioPayload::PatchDecideResponse {
        patch_set_id: set.id.clone(),
        // The set is untouched on purpose — the agent is working on it again.
        status: set.status.clone(),
        conflicted_paths: Vec::new(),
    }))
}

fn patch_set_abandon_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    patch_set_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Editor)?;
    require_allow(gate(
        &scope,
        Capability::ReviewDecide,
        &Target::None,
        Some(patch_set_id),
    )?)?;

    let changed = {
        let conn = scope
            .pool
            .write()
            .map_err(|e| db_error("patch_abandon", anyhow::anyhow!("{e}")))?;
        conn.execute(
            "UPDATE patch_sets SET status = 'rejected', decided_by = ?2, \
              decided_at = datetime('now') \
             WHERE id = ?1 AND session_id = ?3 AND status IN ('open','in_review','conflicted')",
            rusqlite::params![patch_set_id, org.user_id, scope.session.id],
        )
        .map_err(|e| db_error("patch_abandon", anyhow::anyhow!("{e}")))?
    };
    if changed == 0 {
        return Err(ProtocolError::new(
            ProtocolErrorCode::Conflict,
            "only an undecided patch set can be abandoned",
        ));
    }
    append_event(
        &scope,
        format!("patch:{patch_set_id}:abandoned"),
        EventPayload::PatchDecided {
            patch_set_id: patch_set_id.to_string(),
            decision: "abandoned".to_string(),
            decided_by: org.user_id.clone(),
        },
    )?;

    // No dedicated response exists; the remaining sets are the honest answer.
    patch_sets_list_v1(ctx, workspace_id, session_id, "")
}

// =============================================================================
// Timeline, operations, approvals and grants
// =============================================================================

fn session_timeline_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    after_seq: u64,
    limit: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Viewer)?;
    let limit = limit.clamp(1, 500) as usize;
    let stored = events::read_after(
        &scope.pool,
        &scope.session.id,
        after_seq.min(i64::MAX as u64) as i64,
        limit,
    )
    .map_err(|e| db_error("read_after", e))?;

    let has_more = stored.len() == limit;
    let mut next_seq = after_seq;
    let mut result = Vec::with_capacity(stored.len());
    for event in stored {
        next_seq = event.seq.max(0) as u64;
        result.push(TimelineEventInfo {
            seq: next_seq,
            event_id: event.event_id,
            kind: event.kind.slug().to_string(),
            run_id: event.run_id,
            agent_id: event.agent_id,
            created_at: event.created_at,
            // Already redacted on the way in: the timeline stores what the
            // audit mirror stores.
            payload_json: serde_json::to_string(&event.payload)
                .unwrap_or_else(|_| "{}".to_string()),
            security_relevant: event.security_relevant,
        });
    }

    Ok(cs(CodeStudioPayload::SessionTimelineResponse {
        session_id: session_id.to_string(),
        events: result,
        next_seq,
        has_more,
    }))
}

fn operation_to_wire(operation: Operation) -> OperationInfo {
    OperationInfo {
        op_id: operation.op_id,
        run_id: operation.run_id,
        origin_kind: operation.origin_kind.slug().to_string(),
        op_kind: operation.op_kind.slug().to_string(),
        capability: operation.capability,
        idempotent: operation.idempotent,
        status: operation.status.slug().to_string(),
        error: operation.error,
        mount_access: operation.mount_access,
        network_access: operation.network_access,
        started_at: operation.started_at,
        finished_at: operation.finished_at,
    }
}

fn session_operations_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    status: &str,
    limit: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Viewer)?;
    let wanted: Vec<OperationStatus> = if status.is_empty() {
        vec![
            OperationStatus::Pending,
            OperationStatus::Unknown,
            OperationStatus::Failed,
            OperationStatus::Completed,
        ]
    } else {
        vec![OperationStatus::from_slug(status)
            .ok_or_else(|| ProtocolError::bad_request("unknown operation status"))?]
    };

    let limit = limit.clamp(1, 1000);
    let mut operations_out = Vec::new();
    for status in wanted {
        let remaining = limit as usize - operations_out.len();
        let batch = operations::list_by_status(
            &scope.pool,
            &scope.session.id,
            status,
            Some(remaining as u32),
        )
        .map_err(|e| db_error("list_by_status", e))?;
        operations_out.extend(batch.into_iter().map(operation_to_wire));
        if operations_out.len() >= limit as usize {
            break;
        }
    }

    Ok(cs(CodeStudioPayload::SessionOperationsResponse {
        session_id: session_id.to_string(),
        operations: operations_out,
    }))
}

fn operation_resolve_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    op_id: &str,
    resolution: &str,
    note: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Editor)?;
    let decision = match resolution {
        "completed" => UnknownDecision::Completed {
            result_oids: Vec::new(),
        },
        "failed" => UnknownDecision::Failed {
            error: if note.is_empty() {
                "closed by a person".to_string()
            } else {
                redact::redact_text(note)
            },
        },
        other => {
            return Err(ProtocolError::bad_request(format!(
                "resolution must be 'completed' or 'failed', not '{other}'"
            )))
        }
    };
    let operation = operations::resolve_unknown(&scope.pool, op_id, &decision, &org.user_id)
        .map_err(|e| ProtocolError::new(ProtocolErrorCode::Conflict, format!("{e:#}")))?;
    if operation.session_id != scope.session.id {
        return Err(ProtocolError::not_found("operation not found"));
    }

    Ok(cs(CodeStudioPayload::OperationResolveResponse {
        op_id: op_id.to_string(),
        status: operation.status.slug().to_string(),
    }))
}

fn approvals_list_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    status: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Viewer)?;
    let conn = scope
        .pool
        .read()
        .map_err(|e| db_error("approvals_list", anyhow::anyhow!("{e}")))?;
    let mut stmt = conn
        .prepare(
            "SELECT id, run_id, capability, summary, detail_ref, target_digest, status, \
              decision, requested_at, decided_at, decided_by, patch_set_id \
             FROM approvals WHERE session_id = ?1 AND (?2 = '' OR status = ?2) \
             ORDER BY requested_at DESC",
        )
        .map_err(|e| db_error("approvals_list", anyhow::anyhow!("{e}")))?;
    let rows = stmt
        .query_map(rusqlite::params![scope.session.id, status], |row| {
            let capability: String = row.get(2)?;
            Ok(ApprovalInfo {
                approval_id: row.get(0)?,
                session_id: session_id.to_string(),
                run_id: row.get(1)?,
                mandatory_interactive: Capability::from_slug(&capability)
                    .is_some_and(Capability::is_mandatory_interactive),
                capability,
                summary: row.get(3)?,
                detail: row.get(4)?,
                // Two patch sets can be open at once — a work set and a merge
                // set — so a review question names the one it decides instead
                // of leaving the UI to guess "the newest open".
                patch_set_id: row.get(11)?,
                target_digest: row.get(5)?,
                status: row.get(6)?,
                decision: row.get(7)?,
                requested_at: row.get(8)?,
                decided_at: row.get(9)?,
                decided_by: row.get(10)?,
            })
        })
        .map_err(|e| db_error("approvals_list", anyhow::anyhow!("{e}")))?;
    let approvals = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| db_error("approvals_list", anyhow::anyhow!("{e}")))?;

    Ok(cs(CodeStudioPayload::ApprovalsListResponse {
        session_id: session_id.to_string(),
        approvals,
    }))
}

fn approval_decide_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    approval_id: &str,
    decision: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Editor)?;
    if !matches!(
        decision,
        "allow_once" | "allow_for_run" | "allow_for_session" | "always" | "deny"
    ) {
        return Err(ProtocolError::bad_request("unknown approval decision"));
    }

    let (capability, target_pattern, status, interaction_id, waiting_run) = {
        let conn = scope
            .pool
            .read()
            .map_err(|e| db_error("approval_decide", anyhow::anyhow!("{e}")))?;
        conn.query_row(
            "SELECT capability, target_pattern, status, interaction_id, run_id FROM approvals \
             WHERE id = ?1 AND session_id = ?2",
            rusqlite::params![approval_id, scope.session.id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()
        .map_err(|e| db_error("approval_decide", anyhow::anyhow!("{e}")))?
        .ok_or_else(|| ProtocolError::not_found("approval not found"))?
    };
    if status != "pending" {
        return Err(ProtocolError::new(
            ProtocolErrorCode::Conflict,
            format!("approval is already {status}"),
        ));
    }
    let cap = Capability::from_slug(&capability)
        .ok_or_else(|| ProtocolError::internal("approval names an unknown capability"))?;

    // §9.3 rule 5: a mandatory-interactive capability has exactly ONE available
    // outcome, `allow_once`. Every standing scope — workspace, session, run —
    // is refused HERE, at the write, not quietly ignored when it is read back:
    // a refusal that lives only in the reader is a refusal the next reader
    // forgets.
    if matches!(decision, "always" | "allow_for_session" | "allow_for_run")
        && !pep::may_store_always_grant(cap)
    {
        return Err(ProtocolError::bad_request(format!(
            "'{capability}' asks every time and cannot be granted '{decision}'"
        )));
    }
    if decision == "always" && !is_allowlistable(cap) {
        return Err(ProtocolError::bad_request(format!(
            "'{capability}' cannot be granted 'always'"
        )));
    }
    // A run-scoped grant needs a run to attach to. An approval raised by an
    // OPERATOR action (a commit from the Changes pane, a push from the git
    // panel) belongs to no run, so storing `allow_for_run` there would write a
    // grant that authorizes nothing — and the very next click would ask again,
    // which reads as the gate ignoring the answer. Refuse it instead, and say
    // which scopes do apply.
    if decision == "allow_for_run" && waiting_run.is_none() {
        return Err(ProtocolError::bad_request(
            "this approval belongs to no agent run, so 'allow_for_run' would              bind to nothing — answer 'allow_once', 'allow_for_session' or              'always'",
        ));
    }
    // The stored answer is only as good as the target it names. A pattern the
    // matcher gives no reading to would become a grant that authorizes nothing
    // and confuses everyone who audits the table.
    if matches!(decision, "always" | "allow_for_session") {
        pep::validate_grant_pattern(&target_pattern).map_err(ProtocolError::bad_request)?;
    }

    match decision {
        "always" => {
            // §9.2 keeps every durable workspace-scope policy write with the
            // owner, and `allowlist_remove_v1` lets only the owner take one
            // back — so an editor answering their own card must not be able to
            // install one binding everybody else's sessions.
            if scope.role < WorkspaceRole::Owner {
                return Err(ProtocolError::new(
                    ProtocolErrorCode::PolicyDenied,
                    "a standing workspace grant is the owner's decision; \
                     'allow_once' and 'allow_for_session' are yours to give",
                ));
            }
            repository::add_allowlist_entry(
                &scope.registry,
                &scope.record.id,
                &capability,
                &target_pattern,
                &org.user_id,
            )
            .map_err(|e| db_error("add_allowlist_entry", e))?;
        }
        "allow_for_session" => {
            let conn = scope
                .pool
                .write()
                .map_err(|e| db_error("approval_decide", anyhow::anyhow!("{e}")))?;
            conn.execute(
                "INSERT INTO session_grants (session_id, capability, pattern, granted_by, \
                  created_at) VALUES (?1, ?2, ?3, ?4, datetime('now')) \
                 ON CONFLICT(session_id, capability, pattern) DO NOTHING",
                rusqlite::params![
                    scope.session.id,
                    capability,
                    target_pattern,
                    org.user_id
                ],
            )
            .map_err(|e| db_error("approval_decide", anyhow::anyhow!("{e}")))?;
        }
        _ => {}
    }

    // The row and its `approval_decided` event are written by ONE function,
    // shared with the suspended call that raised the card: whichever of the two
    // closes the row writes the entry, and the other writes none.
    tools::settle_approval(
        &scope.pool,
        &scope.session.id,
        approval_id,
        decision,
        &org.user_id,
    )
    .map_err(|e| db_error("settle_approval", e))?;
    sync_session_waiting(&scope)?;

    // Everything the answer authorizes is now durable, so the parked run may
    // wake up and re-read it. Waking it earlier would race a re-authorization
    // against the grant it is supposed to find.
    let resumed = resume_parked_run(&interaction_id, decision);
    // A card raised BY A RUN that nothing was waiting on means the run gave up
    // before the person answered (§9.3 timeouts). The decision is recorded
    // either way — it still binds the next call — but the operator must not be
    // left believing the agent has resumed, so the answer says so (`resumed`)
    // and so does the audit trail.
    match waiting_run.as_deref().filter(|id| !id.is_empty()) {
        Some(run_id) if !resumed => tracing::info!(
            approval_id,
            run_id,
            decision,
            "code_studio: the approval was decided after its run stopped waiting"
        ),
        _ => {}
    }

    Ok(cs(CodeStudioPayload::ApprovalDecideResponse {
        approval_id: approval_id.to_string(),
        status: "decided".to_string(),
        decision: decision.to_string(),
        resumed,
    }))
}

fn session_grants_list_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Viewer)?;
    let mut grants = Vec::new();
    {
        let conn = scope
            .pool
            .read()
            .map_err(|e| db_error("grants_list", anyhow::anyhow!("{e}")))?;
        let mut stmt = conn
            .prepare(
                "SELECT capability, pattern, granted_by, created_at FROM session_grants \
                 WHERE session_id = ?1 ORDER BY capability, pattern",
            )
            .map_err(|e| db_error("grants_list", anyhow::anyhow!("{e}")))?;
        let rows = stmt
            .query_map(rusqlite::params![scope.session.id], |row| {
                Ok(GrantInfo {
                    capability: row.get(0)?,
                    pattern: row.get(1)?,
                    scope: "session".to_string(),
                    granted_by: row.get(2)?,
                    created_at: row.get(3)?,
                })
            })
            .map_err(|e| db_error("grants_list", anyhow::anyhow!("{e}")))?;
        grants.extend(
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| db_error("grants_list", anyhow::anyhow!("{e}")))?,
        );
    }
    for entry in read_allowlist(&scope.registry, &scope.record.id)? {
        grants.push(GrantInfo {
            capability: entry.capability,
            pattern: entry.pattern,
            scope: "workspace".to_string(),
            granted_by: entry.created_by,
            created_at: entry.created_at,
        });
    }

    Ok(cs(CodeStudioPayload::SessionGrantsListResponse {
        session_id: session_id.to_string(),
        grants,
    }))
}

fn session_grant_revoke_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    capability: &str,
    pattern: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Editor)?;
    {
        let conn = scope
            .pool
            .write()
            .map_err(|e| db_error("grant_revoke", anyhow::anyhow!("{e}")))?;
        conn.execute(
            "DELETE FROM session_grants WHERE session_id = ?1 AND capability = ?2 AND pattern = ?3",
            rusqlite::params![scope.session.id, capability, pattern],
        )
        .map_err(|e| db_error("grant_revoke", anyhow::anyhow!("{e}")))?;
    }
    session_grants_list_v1(ctx, workspace_id, session_id)
}

// =============================================================================
// Runs
// =============================================================================

/// Agent that conducts a session (§15). The roster is seeded by name, and the
/// name is the contract: the coordinator decides what to do itself and what to
/// delegate, so a session that cannot find it has no one to talk to.
const CODE_ORCHESTRATOR_AGENT: &str = "code-orchestrator";

const RUN_KIND_ROOT: &str = "root";
const RUN_KIND_REVISION: &str = "revision";
const RUN_TRIGGER_USER: &str = "user";
const RUN_TRIGGER_REVIEW_REJECTED: &str = "review_rejected";

/// How many times a review may bounce a change back to the agent before the
/// operator has to say whether to keep going (§16.3). The budget exists because
/// "revise this" is a loop with no natural end: the reviewer and the agent can
/// disagree forever, and each round costs a full run.
const MAX_REVISION_RUNS: i64 = 10;

/// Starts one run of the session's harness and journals it.
///
/// The workspace and the session are NOT parameters of the agent's tools: they
/// are minted here into `envelope.meta` (`code_session`) and the run inherits
/// them, exactly as Project Studio mints `ps_generation`. A model can therefore
/// never point a write at another person's worktree — it has no way to name one.
async fn start_session_run(
    ctx: &HandlerContext,
    scope: &Scope,
    kind: &str,
    trigger: &str,
    prompt: &str,
    parent_run_id: Option<&str>,
) -> Result<String, ProtocolError> {
    let org = require_org(ctx)?;
    // §16.6 — the session runs the graph it was PINNED to. Resolving it here
    // fails the turn when that graph is gone, instead of starting a run against
    // whatever the harness happens to look like today.
    let resolved = tools::resolve_session_flow(&ctx.state.db, &scope.session).map_err(|e| {
        ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            format!("the session's harness flow cannot be resolved: {e:#}"),
        )
    })?;

    let agent = crate::db::repository::get_agent_by_name(&ctx.state.db, CODE_ORCHESTRATOR_AGENT)
        .map_err(|e| db_error("get_agent_by_name", e))?
        .ok_or_else(|| {
            ProtocolError::new(
                ProtocolErrorCode::NotAvailable,
                "the Code Studio agent roster is not installed on this node",
            )
        })?;
    let manager = crate::agents::agent_run_manager_global().ok_or_else(|| {
        ProtocolError::new(
            ProtocolErrorCode::NotAvailable,
            "background agent runs are not available on this node",
        )
    })?;

    let binding = tools::binding_meta_value(&scope.record.id, &scope.session.id);
    let principal = crate::agents::AgentPrincipal::new(
        Some(org.user_id.clone()),
        Some(org.org_id.clone()),
    );
    let run_id = manager
        .spawn(
            &agent.id,
            prompt,
            parent_run_id,
            &principal,
            &[],
            &[(tools::SESSION_META_KEY, binding)],
            None,
            Some(&scope.session.flow_id),
        )
        .await
        .map_err(|e| ProtocolError::internal(format!("cannot start an agent run: {e:#}")))?;

    {
        let conn = scope
            .pool
            .write()
            .map_err(|e| db_error("session_run_insert", anyhow::anyhow!("{e}")))?;
        conn.execute(
            "INSERT INTO session_runs \
               (run_id, session_id, ordinal, kind, trigger, parent_run_id, agent_id, status, \
                started_at) \
             SELECT ?1, ?2, COALESCE(MAX(ordinal), 0) + 1, ?3, ?4, ?5, ?6, 'running', \
                    datetime('now') \
             FROM session_runs WHERE session_id = ?2",
            rusqlite::params![
                run_id,
                scope.session.id,
                kind,
                trigger,
                parent_run_id,
                agent.id
            ],
        )
        .map_err(|e| db_error("session_run_insert", anyhow::anyhow!("{e}")))?;
    }
    append_event(
        scope,
        format!("run:{run_id}:started"),
        EventPayload::RunStarted {
            run_id: run_id.clone(),
            kind: kind.to_string(),
            trigger: trigger.to_string(),
        },
    )?;
    watch_session_run(manager, &scope.pool, &scope.session.id, &run_id);
    if resolved.fell_back_to_live {
        // The pinned version was pruned by the version window, so this run does
        // not execute the graph the session opened with. Said once, in the
        // timeline, rather than left for someone to infer from behaviour.
        append_event(
            scope,
            format!("run:{run_id}:flow_fallback"),
            EventPayload::AgentMessage {
                role: "system".to_string(),
                text: format!(
                    "the pinned harness version of this session no longer exists; \
                     run '{run_id}' executes the current graph of flow '{}'",
                    scope.session.flow_id
                ),
            },
        )?;
    }
    Ok(run_id)
}

/// How long the watcher waits for one run. On expiry the run KEEPS executing —
/// `await_run` never cancels — and the session row is left alone rather than
/// being marked failed for a run that is still working.
const RUN_WATCH_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);

/// Follows a run to its terminal state and closes the session's row.
///
/// The agent run manager owns the run's lifetime, so the session journal
/// follows ITS answer instead of guessing from a timeout. Without this a turn
/// would stay `running` forever in the timeline even after the agent finished.
fn watch_session_run(
    manager: std::sync::Arc<crate::agents::AgentRunManager>,
    pool: &DbPool,
    session_id: &str,
    run_id: &str,
) {
    let pool = pool.clone();
    let session_id = session_id.to_string();
    let run_id = run_id.to_string();
    tokio::spawn(async move {
        let (status, error, accounting) = match manager.await_run(&run_id, RUN_WATCH_TIMEOUT).await
        {
            Ok(run) => {
                let error = run
                    .exit_reason
                    .filter(|reason| reason.starts_with("error:"));
                // §17.3 — what the turn cost, taken from the run the manager
                // settled. The harness is the only measurer of the native path,
                // so a row that does not copy it here has no other source.
                let accounting = (run.prompt_tokens, run.completion_tokens, run.model);
                (run.status, error, accounting)
            }
            // The run outlived the watcher or vanished with its process. Either
            // way the row cannot be closed on evidence, so it is left as it is.
            Err(e) => {
                tracing::warn!(run_id, error = %e, "code studio: run watcher gave up");
                return;
            }
        };
        {
            let Ok(conn) = pool.write() else {
                return;
            };
            // Never overwrite a decision somebody already made: a cancelled run
            // stays cancelled even if the manager reports it finished.
            let (prompt_tokens, completion_tokens, model) = accounting;
            if let Err(e) = conn.execute(
                "UPDATE session_runs SET status = ?2, finished_at = datetime('now'), \
                   prompt_tokens = ?3, completion_tokens = ?4, model = ?5 \
                 WHERE run_id = ?1 AND status NOT IN ('completed','failed','cancelled')",
                rusqlite::params![run_id, status, prompt_tokens, completion_tokens, model],
            ) {
                tracing::warn!(run_id, error = %e, "code studio: cannot close a run row");
                return;
            }
        }
        let event = events::SessionEvent::new(
            format!("run:{run_id}:finished"),
            EventPayload::RunFinished {
                run_id: run_id.clone(),
                status,
                error,
            },
        );
        if let Err(e) = events::append(&pool, &session_id, event) {
            tracing::warn!(run_id, error = %e, "code studio: cannot journal a run end");
        }
    });
}

/// The session's plan. Read-only and viewer-level: an operator watching a
/// session should be able to see exactly the list the build loop's gate is
/// checking, without being able to tick items off on the agents' behalf.
fn session_tasks_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Viewer)?;
    let rows = crate::code_studio::tools::session_tasks(&scope.pool, &scope.session.id)
        .map_err(|e| db_error("session_tasks", e))?;
    let open = crate::code_studio::tools::open_task_count(&scope.pool, &scope.session.id)
        .map_err(|e| db_error("open_task_count", e))?;

    let tasks = rows
        .into_iter()
        .map(|row| TaskInfo {
            ordinal: row.get("ordinal").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            title: row
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            detail: row
                .get("detail")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            status: row
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("pending")
                .to_string(),
            note: row
                .get("note")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        })
        .collect();

    Ok(cs(CodeStudioPayload::SessionTasksResponse {
        session_id: session_id.to_string(),
        tasks,
        open: open.max(0) as u32,
    }))
}

fn session_runs_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Viewer)?;
    // Why a run ended the way it did lives in the timeline, not in the row
    // (§13.3). Without it the console shows a bare 'failed' and the reason —
    // 'credential_missing', an exhausted budget, a refused approval — is only
    // reachable with a SQL client.
    let reasons = events::run_failure_reasons(&scope.pool, &scope.session.id)
        .map_err(|e| db_error("run_failure_reasons", e))?;
    let conn = scope
        .pool
        .read()
        .map_err(|e| db_error("runs_list", anyhow::anyhow!("{e}")))?;
    let mut stmt = conn
        .prepare(
            "SELECT run_id, ordinal, kind, trigger, parent_run_id, agent_id, status, \
              started_at, finished_at, prompt_tokens, completion_tokens, model, cost_usd \
             FROM session_runs WHERE session_id = ?1 ORDER BY ordinal",
        )
        .map_err(|e| db_error("runs_list", anyhow::anyhow!("{e}")))?;
    let rows = stmt
        .query_map(rusqlite::params![scope.session.id], |row| {
            let run_id: String = row.get(0)?;
            Ok(RunInfo {
                note: reasons.get(&run_id).cloned(),
                run_id,
                ordinal: row.get::<_, i64>(1)?.max(0) as u32,
                kind: row.get(2)?,
                trigger: row.get(3)?,
                parent_run_id: row.get(4)?,
                agent_id: row.get(5)?,
                status: row.get(6)?,
                started_at: row.get(7)?,
                finished_at: row.get(8)?,
                // §17.3 — copied onto the row when the run settles: the flow
                // measures a harness turn, the adapter a delegated CLI run.
                prompt_tokens: row.get::<_, i64>(9)?.max(0) as u64,
                completion_tokens: row.get::<_, i64>(10)?.max(0) as u64,
                model: row.get(11)?,
                // Only ever set where a provider stated a price for the turn;
                // NULL is "nobody quoted one", not "it was free".
                cost_usd: row.get(12)?,
            })
        })
        .map_err(|e| db_error("runs_list", anyhow::anyhow!("{e}")))?;
    let runs = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|e| db_error("runs_list", anyhow::anyhow!("{e}")))?;

    Ok(cs(CodeStudioPayload::SessionRunsResponse {
        session_id: session_id.to_string(),
        runs,
    }))
}

/// Takes one turn from the operator.
///
/// The message is journalled BEFORE the run is started, and that order is the
/// point. A run can fail to start for reasons that have nothing to do with what
/// the person wrote — no model deployed, the pinned harness gone, the run
/// manager down — and starting first meant the sentence was carried in a local
/// variable that died with the error. §13.3 makes the timeline the source of
/// truth and §13.1 wants a failure to leave a resumable state, not silence, so
/// the turn is a fact from the moment it is accepted and a failed start is a
/// second fact next to it rather than the absence of the first.
async fn session_message_send_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    message: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Editor)?;
    let message = message.trim();
    if message.is_empty() {
        return Err(ProtocolError::bad_request("a turn cannot be empty"));
    }
    // A fresh key per send: two identical sentences are two turns, and the
    // caller has no id of its own to deduplicate by.
    let turn_id = uuid::Uuid::new_v4().to_string();
    append_event(
        &scope,
        format!("turn:{turn_id}:user"),
        EventPayload::AgentMessage {
            role: "user".to_string(),
            text: message.to_string(),
        },
    )?;

    let run_id = match start_session_run(ctx, &scope, RUN_KIND_ROOT, RUN_TRIGGER_USER, message, None)
        .await
    {
        Ok(run_id) => run_id,
        Err(error) => {
            append_event(
                &scope,
                format!("turn:{turn_id}:start_failed"),
                EventPayload::AgentMessage {
                    role: "system".to_string(),
                    text: format!("the turn was recorded but no run started: {}", error.message),
                },
            )?;
            return Err(error);
        }
    };

    Ok(cs(CodeStudioPayload::SessionMessageSendResponse {
        session_id: session_id.to_string(),
        run_id,
        status: "running".to_string(),
    }))
}

fn session_cancel_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    run_id: Option<&str>,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Editor)?;
    let runtime = workspace_runtime(&scope.record)?;

    // Whatever is executing right now stops first: a run marked cancelled while
    // its command keeps writing to the worktree is the worst of both.
    let pending = operations::list_by_status(
        &scope.pool,
        &scope.session.id,
        OperationStatus::Pending,
        None,
    )
    .map_err(|e| db_error("list_by_status", e))?;
    for operation in &pending {
        if operation.op_kind == OpKind::Exec {
            let _ = runtime.executor.cancel_exec(&operation.op_id);
        }
    }

    let cancelled = {
        let conn = scope
            .pool
            .write()
            .map_err(|e| db_error("session_cancel", anyhow::anyhow!("{e}")))?;
        let mut stmt = conn
            .prepare(
                "SELECT run_id FROM session_runs WHERE session_id = ?1 \
                 AND status NOT IN ('completed','failed','cancelled') \
                 AND (?2 IS NULL OR run_id = ?2)",
            )
            .map_err(|e| db_error("session_cancel", anyhow::anyhow!("{e}")))?;
        let rows = stmt
            .query_map(rusqlite::params![scope.session.id, run_id], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|e| db_error("session_cancel", anyhow::anyhow!("{e}")))?;
        let ids = rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|e| db_error("session_cancel", anyhow::anyhow!("{e}")))?;
        drop(stmt);
        for id in &ids {
            conn.execute(
                "UPDATE session_runs SET status = 'cancelled', finished_at = datetime('now') \
                 WHERE run_id = ?1",
                rusqlite::params![id],
            )
            .map_err(|e| db_error("session_cancel", anyhow::anyhow!("{e}")))?;
        }
        ids
    };
    for id in &cancelled {
        append_event(
            &scope,
            format!("run:{id}:cancelled"),
            EventPayload::RunFinished {
                run_id: id.clone(),
                status: "cancelled".to_string(),
                error: None,
            },
        )?;
    }

    Ok(cs(CodeStudioPayload::SessionCancelResponse {
        session_id: session_id.to_string(),
        cancelled_runs: cancelled,
        status: "cancelled".to_string(),
    }))
}

// =============================================================================
// Exec and terminal (§7.4, §7.8)
// =============================================================================

/// What the audit trail may CLAIM about a sandbox (§7.6).
///
/// Only a container runtime ENFORCES the profile. In `trusted_native` the
/// command runs as the TentaFlow service user with the service user's rights on
/// the worktree and the host's network: `cow` is a real boundary, because the
/// process is pointed at a different directory, while `ro` is a promise the
/// caller keeps by withholding write tools and `none` is not kept at all.
/// Recording the requested profile as though it were enforcement is exactly the
/// fiction §7.6 exists to prevent, so the event says which of the two the row
/// describes — the same distinction the sandbox row makes with
/// `runtime_ref IS NULL`.
fn audited_access(mode: ExecMode, profile: pep::SandboxProfile) -> (String, String) {
    match mode {
        ExecMode::Container => (
            mount_slug(profile.mount).to_string(),
            network_slug(profile.network).to_string(),
        ),
        ExecMode::TrustedNative => {
            let mount = match profile.mount {
                MountAccess::CopyOnWrite => mount_slug(profile.mount).to_string(),
                other => format!("{} (not enforced)", mount_slug(other)),
            };
            (
                mount,
                format!(
                    "{} (not enforced: host network)",
                    network_slug(profile.network)
                ),
            )
        }
    }
}

fn mount_slug(mount: MountAccess) -> &'static str {
    sandbox::mount_slug(mount)
}

fn network_slug(network: NetworkAccess) -> &'static str {
    sandbox::network_slug(network)
}

fn parse_mount(raw: &str) -> Result<MountAccess, ProtocolError> {
    match raw {
        "ro" => Ok(MountAccess::ReadOnly),
        "cow" => Ok(MountAccess::CopyOnWrite),
        "rw" => Ok(MountAccess::ReadWrite),
        other => Err(ProtocolError::bad_request(format!(
            "unknown mount access '{other}'"
        ))),
    }
}

fn parse_network(raw: &str) -> Result<NetworkAccess, ProtocolError> {
    match raw {
        "none" => Ok(NetworkAccess::None),
        "gateway" => Ok(NetworkAccess::Gateway),
        other => Err(ProtocolError::bad_request(format!(
            "unknown network access '{other}'"
        ))),
    }
}

/// The profile an operation actually runs in: never wider than what the PEP
/// granted, and never wider than what the caller asked for. A caller may ask
/// for less; it may not ask for more.
fn narrow_profile(
    granted: pep::SandboxProfile,
    requested_mount: MountAccess,
    requested_network: NetworkAccess,
) -> pep::SandboxProfile {
    let rank = |mount: MountAccess| match mount {
        MountAccess::ReadOnly => 0u8,
        MountAccess::CopyOnWrite => 1,
        MountAccess::ReadWrite => 2,
    };
    pep::SandboxProfile {
        mount: if rank(requested_mount) < rank(granted.mount) {
            requested_mount
        } else {
            granted.mount
        },
        network: match (granted.network, requested_network) {
            (NetworkAccess::Gateway, NetworkAccess::Gateway) => NetworkAccess::Gateway,
            _ => NetworkAccess::None,
        },
    }
}

#[allow(clippy::too_many_arguments)]
fn exec_start_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    argv: &[String],
    cwd: &str,
    timeout_secs: u32,
    mount_access: &str,
    network_access: &str,
    ephemeral: bool,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Editor)?;
    if argv.is_empty() || argv[0].trim().is_empty() {
        return Err(ProtocolError::bad_request("a command needs a program to run"));
    }
    let (cwd_rel, target) = if cwd.is_empty() {
        (
            None,
            Target::Path {
                inside_worktree: true,
            },
        )
    } else {
        let (parsed, target) = parse_target_path(cwd);
        (parsed, target)
    };
    let granted = require_allow(gate(&scope, Capability::Exec, &target, Some(&argv[0]))?)?;
    let requested_mount = parse_mount(mount_access)?;
    let profile = narrow_profile(granted, requested_mount, parse_network(network_access)?);
    // `exec` is pinned to a copy-on-write layer (§7.2), so a caller asking for
    // `rw` is answered with a profile it did not ask for. Accepting the word
    // and dropping the meaning is what makes an empty diff after `exit 0` look
    // like a bug in the tool instead of the policy that was applied, so the
    // narrowing travels back as a field of the answer and of the timeline.
    let writes_discarded = profile.mount == MountAccess::CopyOnWrite;

    let manager = sandbox_manager(&scope.record)?;
    let lease = manager
        .acquire(
            &scope.pool,
            &scope.session.id,
            sandbox::SandboxProfile::from_decision(profile, ephemeral),
            None,
        )
        .map_err(sandbox_error)?;
    let (audited_mount, audited_network) = audited_access(exec_mode_of(&scope.record)?, profile);
    append_event(
        &scope,
        format!("sandbox:{}:acquired", lease.sandbox_id),
        EventPayload::Sandbox {
            sandbox_id: lease.sandbox_id.clone(),
            state: "ready".to_string(),
            mount_access: audited_mount,
            network_access: audited_network,
        },
    )?;

    let timeout = Duration::from_secs(u64::from(timeout_secs.clamp(1, EXEC_MAX_TIMEOUT_SECS)));
    let canonical: Vec<String> = argv.to_vec();
    let op_id = begin_op(
        ctx,
        &scope,
        OpKind::Exec,
        Capability::Exec,
        &format!("exec:{}", argv[0]),
        OperationInput::Exec {
            // The journal redacts the canonical argv itself; nothing here joins
            // it into a line a redactor could not take apart again.
            argv: canonical.clone(),
            cwd: cwd_rel
                .as_ref()
                .map(|p| p.as_str().to_string())
                .unwrap_or_default(),
            timeout_secs: timeout.as_secs(),
        },
        operations::Precondition::None,
        Postcondition::ExitCodeRecorded,
        Some(profile),
    )?;

    let runtime = workspace_runtime(&scope.record)?;
    let request = ExecRequest {
        exec_id: op_id.clone(),
        program: Program::Argv(canonical),
        cwd_rel: cwd_rel.map(|p| p.as_str().to_string()),
        timeout,
        max_output_bytes: 1024 * 1024,
    };
    let env = ExecEnv::for_lease(&lease);
    let pool = scope.pool.clone();
    let workspace = scope.record.id.clone();
    let session = scope.session.id.clone();
    let executor = Arc::clone(&runtime.executor);
    let op_for_task = op_id.clone();
    let requested_slug = mount_slug(requested_mount).to_string();

    // The command runs off the request path: `ExecStartResponse` says it
    // STARTED, and the outcome lands in the journal, the timeline and an
    // artifact, which is what the UI follows.
    tokio::task::spawn_blocking(move || {
        let outcome = executor.exec(lease.target(), &env, &request, Arc::new(NullSink));
        let released = manager.release(&pool, lease);
        if let Err(error) = released {
            tracing::warn!(workspace_id = %workspace, "sandbox release failed: {error:#}");
        }
        match outcome {
            Ok(outcome) => {
                // ONE implementation of the transcript, shared with the agent
                // path: redacted before it is stored, because an artifact is
                // read by people and mirrored into the audit trail.
                let artifact = match tools::store_exec_transcript(&pool, &workspace, &outcome) {
                    Ok(sha) => Some(sha),
                    Err(error) => {
                        // The journal refuses to close an `ExitCodeRecorded`
                        // operation without it, so the failure is loud here
                        // rather than a silently unclosable row.
                        tracing::warn!(workspace_id = %workspace, "exec transcript not stored: {error:#}");
                        None
                    }
                };
                let exit_code = match outcome.status {
                    crate::code_studio::exec::ExitStatus::Code(code) => Some(code),
                    // A signal, a timeout and a cancellation are not exit
                    // codes, and reporting one as `0` would read as success.
                    _ => None,
                };
                if let Err(error) = operations::complete(
                    &pool,
                    &op_for_task,
                    &[],
                    artifact.as_deref(),
                ) {
                    tracing::warn!("cannot close an exec operation: {error:#}");
                }
                let mut event = SessionEvent::new(
                    format!("op:{op_for_task}:exec"),
                    EventPayload::Exec {
                        op_id: op_for_task.clone(),
                        argv: outcome.argv.clone(),
                        cwd: outcome.cwd.clone(),
                        exit_code,
                        requested_mount_access: requested_slug,
                        writes_discarded,
                    },
                );
                if let Some(sha) = artifact {
                    event = event.with_artifact(sha);
                }
                if let Err(error) = events::append(&pool, &session, event) {
                    tracing::warn!("cannot journal an exec event: {error:#}");
                }
            }
            Err(error) => {
                let message = redact::redact_text(&format!("{error:#}"));
                if let Err(nested) = operations::fail(&pool, &op_for_task, &message) {
                    tracing::warn!("cannot journal an exec failure: {nested:#}");
                }
            }
        }
    });

    Ok(cs(CodeStudioPayload::ExecStartResponse {
        exec_id: op_id.clone(),
        op_id,
        mount_access: mount_slug(profile.mount).to_string(),
        network_access: network_slug(profile.network).to_string(),
        ephemeral,
        requested_mount_access: mount_slug(requested_mount).to_string(),
        writes_discarded,
    }))
}

fn exec_cancel_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    exec_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Editor)?;
    // The exec id is the operation id, so a command of another session cannot
    // be reached by guessing one.
    let operation = operations::get(&scope.pool, exec_id)
        .map_err(|e| db_error("operations::get", e))?
        .filter(|op| op.session_id == scope.session.id)
        .ok_or_else(|| ProtocolError::not_found("no such command"))?;
    let runtime = workspace_runtime(&scope.record)?;
    let status = match runtime.executor.cancel_exec(&operation.op_id) {
        Ok(()) => "cancelling",
        Err(_) => "not_running",
    };

    Ok(cs(CodeStudioPayload::ExecCancelResponse {
        exec_id: exec_id.to_string(),
        status: status.to_string(),
    }))
}

/// Reads back what a command printed (§7.8).
///
/// The transcript is the artifact the operation was closed with — the same
/// bytes the audit trail mirrors, redacted when they were written — so this is
/// a lookup, not a second capture, and there is no path by which the two could
/// tell different stories. A command still running has no artifact yet: it
/// answers with its status and an empty tail, which is not the same claim as
/// "it printed nothing".
fn exec_output_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    exec_id: &str,
    after_seq: u64,
    limit: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Viewer)?;
    // The exec id is the operation id, and the row is filtered by session for
    // the same reason `exec_cancel` filters it: a guessed id must not reach
    // another session's output.
    let operation = operations::get(&scope.pool, exec_id)
        .map_err(|e| db_error("operations::get", e))?
        .filter(|op| op.session_id == scope.session.id)
        .ok_or_else(|| ProtocolError::not_found("no such command"))?;

    let all: Vec<String> = match operation.result_ref.as_deref() {
        Some(sha) => {
            let bytes = artifacts::get(&scope.pool, &scope.record.id, sha)
                .map_err(|e| db_error("artifacts::get", e))?;
            String::from_utf8_lossy(&bytes)
                .lines()
                .map(str::to_string)
                .collect()
        }
        None => Vec::new(),
    };

    let limit = limit.clamp(1, 2000) as usize;
    let start = after_seq.min(all.len() as u64) as usize;
    let end = all.len().min(start + limit);
    let lines = all[start..end].to_vec();

    Ok(cs(CodeStudioPayload::ExecOutputResponse {
        exec_id: exec_id.to_string(),
        status: operation.status.slug().to_string(),
        lines,
        next_seq: end as u64,
        has_more: end < all.len(),
    }))
}

fn terminal_error(error: crate::code_studio::terminal::TerminalError) -> ProtocolError {
    use crate::code_studio::terminal::TerminalError as E;
    match error {
        E::NotAvailableInPlanMode => {
            ProtocolError::new(ProtocolErrorCode::PolicyDenied, error.to_string())
        }
        E::Unknown(id) => ProtocolError::not_found(format!("no terminal {id}")),
        E::Closed(id) => ProtocolError::new(
            ProtocolErrorCode::Conflict,
            format!("terminal {id} has already been closed"),
        ),
        E::Other(err) => db_error("terminal", err),
    }
}

fn terminal_handle(scope: &Scope, terminal_id: &str) -> PtyHandle {
    PtyHandle {
        terminal_id: terminal_id.to_string(),
        session_id: scope.session.id.clone(),
    }
}

fn terminal_open_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    rows: u16,
    cols: u16,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Editor)?;
    let granted = require_allow(gate(&scope, Capability::Terminal, &Target::None, Some("terminal"))?)?;

    let manager = sandbox_manager(&scope.record)?;
    let lease = manager
        .acquire(
            &scope.pool,
            &scope.session.id,
            sandbox::SandboxProfile::from_decision(granted, true),
            None,
        )
        .map_err(sandbox_error)?;
    let env = ExecEnv::for_lease(&lease);
    let terminal_id = uuid::Uuid::new_v4().to_string();
    let runtime = workspace_runtime(&scope.record)?;
    let terminals = terminal_registry(&scope.record)?;

    let opened = match terminals.pty_open(
        scope.autonomy()?,
        lease.target(),
        &env,
        &PtyOpen {
            terminal_id: terminal_id.clone(),
            session_id: scope.session.id.clone(),
            rows,
            cols,
            shell: None,
        },
    ) {
        Ok(opened) => opened,
        Err(error) => {
            if let Err(nested) = manager.release(&scope.pool, lease) {
                tracing::warn!("sandbox release after a failed terminal: {nested:#}");
            }
            return Err(terminal_error(error));
        }
    };

    let op_id = begin_op(
        ctx,
        &scope,
        OpKind::Exec,
        Capability::Terminal,
        &format!("terminal:{terminal_id}"),
        OperationInput::Exec {
            argv: opened.exec.argv.clone(),
            cwd: opened.exec.cwd.clone(),
            timeout_secs: 0,
        },
        operations::Precondition::None,
        // A shell has no exit code while it is running, and closing it is a
        // separate act; the effect being journalled here is "a shell was
        // started in this worktree".
        Postcondition::None,
        Some(granted),
    )?;
    append_event(
        &scope,
        format!("terminal:{terminal_id}:exec"),
        EventPayload::Exec {
            op_id: op_id.clone(),
            argv: opened.exec.argv.clone(),
            cwd: opened.exec.cwd.clone(),
            exit_code: None,
            // A terminal names no mount on the wire; the PEP alone decides it,
            // so there is no request to compare against — but the shell still
            // has to say whether what is typed into it survives.
            requested_mount_access: String::new(),
            writes_discarded: granted.mount == MountAccess::CopyOnWrite,
        },
    )?;
    let (audited_mount, audited_network) = audited_access(exec_mode_of(&scope.record)?, granted);
    append_event(
        &scope,
        format!("sandbox:{}:terminal", lease.sandbox_id),
        EventPayload::Sandbox {
            sandbox_id: lease.sandbox_id.clone(),
            state: "ready".to_string(),
            mount_access: audited_mount,
            network_access: audited_network,
        },
    )?;
    complete_op(&scope, &op_id, &[], None)?;

    // The lease has to outlive this request: the shell keeps running in it.
    runtime
        .terminal_leases
        .lock()
        .map_err(|_| ProtocolError::internal("terminal lease registry is poisoned"))?
        .insert(terminal_id.clone(), lease);

    Ok(cs(CodeStudioPayload::TerminalOpenResponse {
        terminal_id,
        rows,
        cols,
        mount_access: mount_slug(granted.mount).to_string(),
        network_access: network_slug(granted.network).to_string(),
    }))
}

fn terminal_input_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    terminal_id: &str,
    data: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Editor)?;
    // §9.4 exempts a RUNNING PROCESS from being re-authorized, not a new call:
    // typing a command into an open shell is a new call, so a lowered ceiling
    // or a revoked grant has to stop it here rather than at the next open.
    require_allow(gate(&scope, Capability::Terminal, &Target::None, Some("terminal"))?)?;
    let terminals = terminal_registry(&scope.record)?;
    let handle = terminal_handle(&scope, terminal_id);
    let before = terminals.snapshot(&handle).map_err(terminal_error)?.revision;
    terminals
        .pty_write(&handle, data.as_bytes())
        .map_err(terminal_error)?;
    let changes = terminals
        .changes_since(&handle, before)
        .map_err(terminal_error)?;

    Ok(cs(CodeStudioPayload::TerminalSnapshotResponse {
        terminal_id: terminal_id.to_string(),
        revision: changes.revision,
        rows: changes.rows,
        cols: changes.cols,
        cursor_row: changes.cursor.row,
        cursor_col: changes.cursor.col,
        cursor_visible: changes.cursor.visible,
        cells: pack_rows(&changes.lines),
    }))
}

fn terminal_resize_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    terminal_id: &str,
    rows: u16,
    cols: u16,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Editor)?;
    require_allow(gate(&scope, Capability::Terminal, &Target::None, Some("terminal"))?)?;
    let terminals = terminal_registry(&scope.record)?;
    let handle = terminal_handle(&scope, terminal_id);
    terminals
        .pty_resize(&handle, rows, cols)
        .map_err(terminal_error)?;
    let grid = terminals.snapshot(&handle).map_err(terminal_error)?;

    Ok(cs(CodeStudioPayload::TerminalSnapshotResponse {
        terminal_id: terminal_id.to_string(),
        revision: grid.revision,
        rows: grid.rows,
        cols: grid.cols,
        cursor_row: grid.cursor.row,
        cursor_col: grid.cursor.col,
        cursor_visible: grid.cursor.visible,
        cells: pack_rows(&grid.lines),
    }))
}

fn terminal_close_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    terminal_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Editor)?;
    let runtime = workspace_runtime(&scope.record)?;
    let terminals = terminal_registry(&scope.record)?;
    let handle = terminal_handle(&scope, terminal_id);
    let grid = terminals.snapshot(&handle).map_err(terminal_error)?;
    let state = terminals.pty_close(&handle).map_err(terminal_error)?;

    // The sandbox goes with the shell: an ephemeral layer that outlived its
    // terminal would keep a whole copy of the worktree alive.
    let lease = runtime
        .terminal_leases
        .lock()
        .map_err(|_| ProtocolError::internal("terminal lease registry is poisoned"))?
        .remove(terminal_id);
    if let Some(lease) = lease {
        let sandbox_id = lease.sandbox_id.clone();
        if let Err(error) = sandbox_manager(&scope.record)?.release(&scope.pool, lease) {
            tracing::warn!(terminal_id, "sandbox release after close failed: {error:#}");
        }
        append_event(
            &scope,
            format!("sandbox:{sandbox_id}:released"),
            EventPayload::Sandbox {
                sandbox_id,
                state: "stopped".to_string(),
                mount_access: String::new(),
                network_access: String::new(),
            },
        )?;
    }
    let _ = state;

    Ok(cs(CodeStudioPayload::TerminalSnapshotResponse {
        terminal_id: terminal_id.to_string(),
        revision: grid.revision,
        rows: grid.rows,
        cols: grid.cols,
        cursor_row: grid.cursor.row,
        cursor_col: grid.cursor.col,
        cursor_visible: grid.cursor.visible,
        cells: pack_rows(&grid.lines),
    }))
}

fn terminal_snapshot_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    terminal_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Viewer)?;
    let grid = terminal_registry(&scope.record)?
        .snapshot(&terminal_handle(&scope, terminal_id))
        .map_err(terminal_error)?;

    Ok(cs(CodeStudioPayload::TerminalSnapshotResponse {
        terminal_id: terminal_id.to_string(),
        revision: grid.revision,
        rows: grid.rows,
        cols: grid.cols,
        cursor_row: grid.cursor.row,
        cursor_col: grid.cursor.col,
        cursor_visible: grid.cursor.visible,
        cells: pack_rows(&grid.lines),
    }))
}

/// Packs a VT row into the wire's "one word per character" form.
///
/// Layout: bits 0-7 style flags, 8-15 foreground, 16-23 background, 24-25 the
/// two "is default" markers. A 24-bit colour is folded onto the xterm-256 cube,
/// which is the standard lossy mapping — carrying two true-colour triples plus
/// the flags does not fit in a word, and splitting a cell across two values
/// would double the size of every screen update.
fn pack_rows(lines: &[crate::code_studio::terminal::GridRow]) -> Vec<TerminalCellRow> {
    lines
        .iter()
        .map(|line| {
            let mut text = String::with_capacity(line.cells.len());
            let mut attrs = Vec::with_capacity(line.cells.len());
            for cell in &line.cells {
                // The second half of a double-width character is a marker, not
                // a character: it is skipped so text and attrs stay aligned.
                if cell.ch == '\0' {
                    continue;
                }
                text.push(cell.ch);
                attrs.push(pack_cell(cell));
            }
            TerminalCellRow {
                row: u32::from(line.index),
                text,
                attrs,
            }
        })
        .collect()
}

fn pack_cell(cell: &Cell) -> u32 {
    let (fg, fg_default) = pack_color(cell.fg);
    let (bg, bg_default) = pack_color(cell.bg);
    u32::from(cell.attrs & 0x00ff)
        | (u32::from(fg) << 8)
        | (u32::from(bg) << 16)
        | (u32::from(fg_default) << 24)
        | (u32::from(bg_default) << 25)
}

fn pack_color(color: Color) -> (u8, u8) {
    match color {
        Color::Default => (0, 1),
        Color::Indexed(index) => (index, 0),
        Color::Rgb(r, g, b) => (rgb_to_xterm256(r, g, b), 0),
    }
}

/// Standard xterm-256 folding: the 6x6x6 colour cube, or the 24-step grey ramp
/// when the three channels are close enough to be grey.
fn rgb_to_xterm256(r: u8, g: u8, b: u8) -> u8 {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    if max - min < 8 {
        let level = (u16::from(r) + u16::from(g) + u16::from(b)) / 3;
        if level < 8 {
            return 16;
        }
        if level > 248 {
            return 231;
        }
        return 232 + ((level - 8) / 10) as u8;
    }
    let cube = |value: u8| -> u16 {
        match value {
            0..=47 => 0,
            48..=114 => 1,
            _ => u16::from((value - 35) / 40),
        }
    };
    (16 + 36 * cube(r) + 6 * cube(g) + cube(b)).min(255) as u8
}

/// Largest blob the review may pull over the wire in one answer.
const PATCH_BLOB_MAX_BYTES: usize = 1024 * 1024;

/// Reads one blob of the object database by its digest (§13.2).
///
/// Reconstructing a file after a PARTIAL acceptance needs the whole accepted
/// blob: the patch set only carries hunk windows, and rendering those as if
/// they were the file is how a reviewer ends up approving something they never
/// saw.
fn patch_blob_get_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    blob_sha: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Viewer)?;
    require_allow(gate(&scope, Capability::FsRead, &Target::None, Some(blob_sha))?)?;

    let broker = scope.broker()?;
    let bytes = broker
        .cat_file(&broker.reference(), blob_sha)
        .map_err(|e| ProtocolError::not_found(format!("{e:#}")))?;
    let truncated = bytes.len() > PATCH_BLOB_MAX_BYTES;
    let slice = if truncated {
        &bytes[..PATCH_BLOB_MAX_BYTES]
    } else {
        &bytes[..]
    };

    Ok(cs(CodeStudioPayload::PatchBlobGetResponse {
        blob_sha: blob_sha.to_string(),
        content: String::from_utf8_lossy(slice).to_string(),
        truncated,
    }))
}

/// Terminals of this session that are still open on this node. A browser that
/// reconnects has to find its shells again rather than start new ones.
fn terminals_list_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Viewer)?;
    let registry = terminal_registry(&scope.record)?;
    let runtime = workspace_runtime(&scope.record)?;
    let leases = runtime
        .terminal_leases
        .lock()
        .map_err(|_| ProtocolError::internal("terminal lease registry is poisoned"))?;

    let mut terminals = Vec::new();
    for handle in registry.session_handles(&scope.session.id) {
        let Ok(grid) = registry.snapshot(&handle) else {
            continue;
        };
        let exec = registry.exec_event(&handle).ok();
        let state = registry.state(&handle).ok();
        // The profile is the one the lease really got; a terminal whose lease
        // this process does not hold is one it cannot describe, so it says so
        // with an empty profile rather than a plausible guess.
        let profile = leases.get(&handle.terminal_id).map(|lease| lease.profile);
        terminals.push(tentaflow_protocol::code_studio::TerminalInfo {
            title: exec
                .as_ref()
                .and_then(|e| e.argv.first().cloned())
                .unwrap_or_else(|| "shell".to_string()),
            started_at: exec.map(|e| e.started_at).unwrap_or_default(),
            terminal_id: handle.terminal_id,
            rows: grid.rows,
            cols: grid.cols,
            mount_access: profile
                .map(|p| mount_slug(p.mount).to_string())
                .unwrap_or_default(),
            network_access: profile
                .map(|p| network_slug(p.network).to_string())
                .unwrap_or_default(),
            status: match state {
                Some(crate::code_studio::terminal::TerminalState::Running) => "running",
                Some(crate::code_studio::terminal::TerminalState::Exited) => "exited",
                Some(crate::code_studio::terminal::TerminalState::Reaped) => "reaped",
                None => "exited",
            }
            .to_string(),
        });
    }

    Ok(cs(CodeStudioPayload::TerminalsListResponse { terminals }))
}

// =============================================================================
// Semantic index (§14)
// =============================================================================

/// A refusal, never an empty result: "nothing matched" and "there is no index
/// here" are different answers and the caller acts on them differently.
fn index_unavailable(detail: &str) -> ProtocolError {
    ProtocolError::new(
        ProtocolErrorCode::NotAvailable,
        format!("the semantic index cannot answer here: {detail}"),
    )
}

/// The workspace's index, built on the owner node against the shared
/// `rag-embeddings` alias — the same embedding space Project Studio uses, so
/// code and project knowledge are never two incompatible vector spaces.
fn workspace_index(
    ctx: &HandlerContext,
    record: &WorkspaceRecord,
) -> Result<index::CodeIndex, ProtocolError> {
    if !record.index_enabled {
        return Err(index_unavailable("indexing is disabled for this workspace"));
    }
    index::CodeIndex::for_workspace(&ctx.state.db, record, ctx.state.router.clone())
        .map_err(|e| db_error("code_index", e))
}

fn index_status_v1(ctx: &HandlerContext, workspace_id: &str) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _) = require_workspace(
        ctx,
        org,
        workspace_id,
        Access::Member(WorkspaceRole::Viewer),
    )?;
    // A workspace with indexing switched off is not a failure to report: the
    // answer is the empty state plus the flag that says why.
    if !record.index_enabled {
        return Ok(cs(CodeStudioPayload::IndexStatusResponse {
            workspace_id: workspace_id.to_string(),
            index_enabled: false,
            branches: Vec::new(),
        }));
    }
    let branches = workspace_index(ctx, &record)?
        .status()
        .map_err(|e| db_error("index_status", e))?
        .into_iter()
        .map(|state| IndexStateInfo {
            branch: state.branch,
            indexed_commit: state.indexed_commit,
            files: state.files,
            chunks: state.chunks,
            updated_at: state.updated_at,
            last_error: state.last_error,
        })
        .collect();
    Ok(cs(CodeStudioPayload::IndexStatusResponse {
        workspace_id: workspace_id.to_string(),
        index_enabled: true,
        branches,
    }))
}

fn index_rebuild_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    branch: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let (record, _) =
        require_workspace(ctx, org, workspace_id, Access::Member(WorkspaceRole::Owner))?;
    require_active(&record)?;
    let branch = match branch.trim() {
        "" => record
            .target_branch
            .clone()
            .or_else(|| record.default_branch.clone())
            .ok_or_else(|| ProtocolError::bad_request("this workspace has no branch to index"))?,
        named => named.to_string(),
    };
    let index = std::sync::Arc::new(workspace_index(ctx, &record)?);
    // A full pass walks the commit and embeds every chunk; it runs in the
    // background and reports through the index progress stream (§14).
    let job_id = index::start_rebuild(index, &branch);
    audit(
        ctx,
        "code_studio.index_rebuild",
        workspace_id,
        &serde_json::json!({ "branch": branch, "job_id": job_id }),
    );
    Ok(cs(CodeStudioPayload::IndexRebuildResponse {
        workspace_id: workspace_id.to_string(),
        branch,
        job_id,
        status: "queued".to_string(),
    }))
}

async fn code_search_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    session_id: &str,
    query: &str,
    path_prefix: &str,
    limit: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let scope = session_scope(ctx, org, workspace_id, session_id, WorkspaceRole::Viewer)?;
    // The prefix is a path the caller named, so it is bounded where every other
    // path argument is bounded — by the PEP, under the capability that owns the
    // index rather than under `fs_read`.
    let target = if path_prefix.is_empty() {
        Target::Path {
            inside_worktree: true,
        }
    } else {
        parse_target_path(path_prefix).1
    };
    require_allow(gate(&scope, Capability::CodeSearch, &target, Some(path_prefix))?)?;
    // §14 keeps grep authoritative. A semantic answer that does not describe the
    // current head still comes back, flagged `degraded`, so the caller can fall
    // back to the file search instead of trusting a stale hit.
    let outcome = workspace_index(ctx, &scope.record)?
        .search(query, limit.clamp(1, 50) as usize, path_prefix)
        .await
        .map_err(|e| db_error("code_search", e))?;

    Ok(cs(CodeStudioPayload::CodeSearchResponse {
        mode: "semantic".to_string(),
        degraded: outcome.degraded,
        hits: outcome
            .hits
            .into_iter()
            .map(|hit| CodeSearchHit {
                path: hit.path,
                start_line: hit.start_line,
                end_line: hit.end_line,
                score: hit.score,
                snippet: hit.snippet,
                lang: Some(hit.lang).filter(|l| !l.is_empty()),
                commit: Some(hit.commit).filter(|c| !c.is_empty()),
            })
            .collect(),
    }))
}

// =============================================================================
// Member candidates and the Projects link (§20)
// =============================================================================

/// Users the caller may put on a workspace.
///
/// The directory search is the org catalog, and the gate differs by intent: for
/// an EXISTING workspace the caller must already administer its membership, and
/// for the creation wizard they must be allowed to create one at all. Anything
/// looser would turn a member picker into an org directory dump for anybody.
fn workspace_member_candidates_v1(
    ctx: &HandlerContext,
    workspace_id: Option<&str>,
    query: &str,
    limit: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    let exclude: std::collections::HashSet<String> = match workspace_id {
        Some(workspace_id) => {
            require_workspace(ctx, org, workspace_id, Access::Member(WorkspaceRole::Owner))?;
            repository::list_members(&ctx.state.db, workspace_id)
                .map_err(|e| db_error("list_members", e))?
                .into_iter()
                .map(|member| member.user_id)
                .collect()
        }
        None => {
            let may_create =
                repository::may_create_workspace(&ctx.state.db, &org.org_id, &org.user_id)
                    .map_err(|e| db_error("may_create_workspace", e))?;
            if !may_create {
                return Err(ProtocolError::new(
                    ProtocolErrorCode::PolicyDenied,
                    "creating a workspace requires a creator grant",
                ));
            }
            // The creator becomes the owner, so they are not a candidate.
            std::iter::once(org.user_id.clone()).collect()
        }
    };

    let limit = limit.clamp(1, 50);
    let rows = crate::project_studio::repository::list_org_user_candidates(
        &org.org_id,
        query,
        limit + exclude.len() as u32,
    )
    .map_err(|e| db_error("list_org_user_candidates", e))?;
    let candidates = rows
        .into_iter()
        .filter(|(user_id, _, _)| !exclude.contains(user_id))
        .take(limit as usize)
        .map(|(user_id, display_name, email)| WorkspaceUserCandidate {
            user_id,
            display_name,
            email,
        })
        .collect();
    Ok(cs(CodeStudioPayload::WorkspaceMemberCandidatesResponse { candidates }))
}

fn project_link_list_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    require_workspace(
        ctx,
        org,
        workspace_id,
        Access::Member(WorkspaceRole::Viewer),
    )?;
    links_response(ctx, workspace_id)
}

/// Links or unlinks one project. Both answer with the whole list, because that
/// is what the caller has to render afterwards either way.
fn project_link_set_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    project_id: &str,
    linked: bool,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    // Linking hands a project's members access to this workspace's code, so it
    // is an owner's decision, not an editor's.
    require_workspace(ctx, org, workspace_id, Access::Member(WorkspaceRole::Owner))?;
    // And the caller has to be able to administer the OTHER side too, or one
    // owner could attach themselves to a project they have nothing to do with.
    let project = crate::project_studio::repository::get_project(&org.org_id, project_id)
        .map_err(|e| db_error("get_project", e))?
        .ok_or_else(|| ProtocolError::not_found("project not found"))?;
    let project_role = crate::project_studio::repository::member_role(project_id, &org.user_id)
        .map_err(|e| db_error("project_member_role", e))?;
    let may_administer = project.owner_user_id == org.user_id
        || matches!(project_role.as_deref(), Some("owner") | Some("manager"));
    if !may_administer {
        return Err(ProtocolError::not_found("project not found"));
    }

    if linked {
        project_link::link(
            &ctx.state.db,
            &org.org_id,
            workspace_id,
            project_id,
            &org.user_id,
        )
        .map_err(|e| ProtocolError::new(ProtocolErrorCode::Conflict, format!("{e:#}")))?;
        // The mirror is one-way and records what it granted, so running it here
        // is what makes the project's members appear on the workspace at once
        // instead of at the next membership change.
        project_link::sync_project(&ctx.state.db, project_id);
    } else {
        project_link::unlink(&ctx.state.db, workspace_id, project_id)
            .map_err(|e| db_error("project_unlink", e))?;
    }
    audit(
        ctx,
        if linked {
            "code_studio.project_link"
        } else {
            "code_studio.project_unlink"
        },
        workspace_id,
        &serde_json::json!({ "project_id": project_id }),
    );
    links_response(ctx, workspace_id)
}

fn links_response(ctx: &HandlerContext, workspace_id: &str) -> Result<MessageBody, ProtocolError> {
    let org = require_org(ctx)?;
    let links = project_link::links_of_workspace(&ctx.state.db, workspace_id)
        .map_err(|e| db_error("links_of_workspace", e))?
        .into_iter()
        .map(|link| ProjectLinkInfo {
            project_name: crate::project_studio::repository::get_project(
                &org.org_id,
                &link.project_id,
            )
            .ok()
            .flatten()
            .map(|project| project.name)
            .unwrap_or_default(),
            project_id: link.project_id,
            linked_by: link.linked_by,
            created_at: link.created_at,
        })
        .collect();
    Ok(cs(CodeStudioPayload::ProjectLinkListResponse {
        workspace_id: workspace_id.to_string(),
        links,
    }))
}

/// Structure of a commit, for a project that is linked to this workspace.
///
/// Visibility comes from the LINK, not from workspace membership: this is how a
/// project reads the repository it tests. A workspace that is not linked
/// answers exactly like one that does not exist.
fn repo_tree_v1(
    ctx: &HandlerContext,
    workspace_id: &str,
    project_id: &str,
    commit: &str,
    path_prefix: &str,
    limit: u32,
) -> Result<MessageBody, ProtocolError> {
    let org = require_read(ctx)?;
    paths::validate_workspace_id(workspace_id)
        .map_err(|_| ProtocolError::bad_request("invalid workspace_id"))?;
    crate::project_studio::repository::member_role(project_id, &org.user_id)
        .map_err(|e| db_error("project_member_role", e))?
        .ok_or_else(not_found)?;

    let requested = limit.clamp(1, project_link::MAX_TREE_ENTRIES as u32) as usize;
    let entries = project_link::repo_tree(
        &ctx.state.db,
        &org.org_id,
        project_id,
        workspace_id,
        commit,
        path_prefix,
        requested,
    )
    .map_err(|e| db_error("repo_tree", e))?
    .ok_or_else(not_found)?;

    Ok(cs(CodeStudioPayload::RepoTreeResponse {
        workspace_id: workspace_id.to_string(),
        commit: commit.to_string(),
        // A listing cut at the ceiling has to say so, or it reads as a small
        // repository.
        truncated: entries.len() >= requested,
        entries: entries
            .into_iter()
            .map(|entry| RepoEntryInfo {
                path: entry.path,
                mode: entry.mode,
                blob_oid: entry.blob_oid,
            })
            .collect(),
    }))
}

// =============================================================================
// Variant registration
// =============================================================================

/// `#[handler]` registers the dispatcher under the family name, which no frame
/// ever carries — `variant_name_of` reports the concrete variant. Each request
/// variant therefore needs its own registry entry pointing at the same
/// dispatch wrapper, or `dispatch::find` answers NotImplemented.
macro_rules! register_code_studio_variant {
    ($variant:literal, $metric:literal) => {
        ::inventory::submit! {
            crate::dispatch::HandlerMeta {
                variant_name: $variant,
                since_major: 1,
                since_minor: 0,
                required_auth: crate::dispatch::SessionAuthKind::UserSession,
                metric_name: $metric,
                dispatch_fn: __tentaflow_dispatch_code_studio_dispatch,
            }
        }
    };
}

register_code_studio_variant!(
    "CodeStudioWorkspacesListRequest",
    "tentaflow_ws_handler_cs_workspaces_list"
);
register_code_studio_variant!(
    "CodeStudioWorkspaceCreateRequest",
    "tentaflow_ws_handler_cs_workspace_create"
);
register_code_studio_variant!(
    "CodeStudioWorkspaceGetRequest",
    "tentaflow_ws_handler_cs_workspace_get"
);
register_code_studio_variant!(
    "CodeStudioWorkspaceRetryRequest",
    "tentaflow_ws_handler_cs_workspace_retry"
);
register_code_studio_variant!(
    "CodeStudioWorkspaceArchiveRequest",
    "tentaflow_ws_handler_cs_workspace_archive"
);
register_code_studio_variant!(
    "CodeStudioWorkspaceDeleteRequest",
    "tentaflow_ws_handler_cs_workspace_delete"
);
register_code_studio_variant!(
    "CodeStudioWorkspaceSettingsUpdateRequest",
    "tentaflow_ws_handler_cs_workspace_settings_update"
);
register_code_studio_variant!(
    "CodeStudioWorkspaceSecretSetRequest",
    "tentaflow_ws_handler_cs_workspace_secret_set"
);
register_code_studio_variant!(
    "CodeStudioWorkspaceMemberSetRequest",
    "tentaflow_ws_handler_cs_workspace_member_set"
);
register_code_studio_variant!(
    "CodeStudioWorkspaceMemberRemoveRequest",
    "tentaflow_ws_handler_cs_workspace_member_remove"
);
register_code_studio_variant!(
    "CodeStudioWorkspaceCreatorGrantSetRequest",
    "tentaflow_ws_handler_cs_workspace_creator_grant_set"
);
register_code_studio_variant!(
    "CodeStudioWorkspaceAllowlistListRequest",
    "tentaflow_ws_handler_cs_workspace_allowlist_list"
);
register_code_studio_variant!(
    "CodeStudioWorkspaceAllowlistSetRequest",
    "tentaflow_ws_handler_cs_workspace_allowlist_set"
);
register_code_studio_variant!(
    "CodeStudioWorkspaceAllowlistRemoveRequest",
    "tentaflow_ws_handler_cs_workspace_allowlist_remove"
);
register_code_studio_variant!(
    "CodeStudioSessionsListRequest",
    "tentaflow_ws_handler_cs_sessions_list"
);
register_code_studio_variant!(
    "CodeStudioSessionOpenRequest",
    "tentaflow_ws_handler_cs_session_open"
);
register_code_studio_variant!(
    "CodeStudioSessionCloseRequest",
    "tentaflow_ws_handler_cs_session_close"
);
register_code_studio_variant!(
    "CodeStudioSessionAutonomySetRequest",
    "tentaflow_ws_handler_cs_session_autonomy_set"
);
register_code_studio_variant!(
    "CodeStudioGitStatusRequest",
    "tentaflow_ws_handler_cs_git_status"
);
register_code_studio_variant!(
    "CodeStudioWorktreesListRequest",
    "tentaflow_ws_handler_cs_worktrees_list"
);
register_code_studio_variant!("CodeStudioFileTreeRequest", "tentaflow_ws_handler_cs_file_tree");
register_code_studio_variant!("CodeStudioFileReadRequest", "tentaflow_ws_handler_cs_file_read");
register_code_studio_variant!(
    "CodeStudioFileWriteRequest",
    "tentaflow_ws_handler_cs_file_write"
);
register_code_studio_variant!(
    "CodeStudioFileCreateRequest",
    "tentaflow_ws_handler_cs_file_create"
);
register_code_studio_variant!(
    "CodeStudioFileDeleteRequest",
    "tentaflow_ws_handler_cs_file_delete"
);
register_code_studio_variant!(
    "CodeStudioFileRenameRequest",
    "tentaflow_ws_handler_cs_file_rename"
);
register_code_studio_variant!(
    "CodeStudioFileMkdirRequest",
    "tentaflow_ws_handler_cs_file_mkdir"
);
register_code_studio_variant!("CodeStudioFileGrepRequest", "tentaflow_ws_handler_cs_file_grep");
register_code_studio_variant!("CodeStudioGitLogRequest", "tentaflow_ws_handler_cs_git_log");
register_code_studio_variant!(
    "CodeStudioGitBranchesRequest",
    "tentaflow_ws_handler_cs_git_branches"
);
register_code_studio_variant!("CodeStudioGitDiffRequest", "tentaflow_ws_handler_cs_git_diff");
register_code_studio_variant!(
    "CodeStudioGitCommitRequest",
    "tentaflow_ws_handler_cs_git_commit"
);
register_code_studio_variant!("CodeStudioGitPushRequest", "tentaflow_ws_handler_cs_git_push");
register_code_studio_variant!("CodeStudioGitSyncRequest", "tentaflow_ws_handler_cs_git_sync");
register_code_studio_variant!("CodeStudioGitMergeRequest", "tentaflow_ws_handler_cs_git_merge");
register_code_studio_variant!(
    "CodeStudioGitMergeFinalizeRequest",
    "tentaflow_ws_handler_cs_git_merge_finalize"
);
register_code_studio_variant!(
    "CodeStudioGitMergeAbandonRequest",
    "tentaflow_ws_handler_cs_git_merge_abandon"
);
register_code_studio_variant!(
    "CodeStudioPatchSetsListRequest",
    "tentaflow_ws_handler_cs_patch_sets_list"
);
register_code_studio_variant!(
    "CodeStudioPatchSetGetRequest",
    "tentaflow_ws_handler_cs_patch_set_get"
);
register_code_studio_variant!(
    "CodeStudioPatchDecideRequest",
    "tentaflow_ws_handler_cs_patch_decide"
);
register_code_studio_variant!(
    "CodeStudioPatchSetAbandonRequest",
    "tentaflow_ws_handler_cs_patch_set_abandon"
);
register_code_studio_variant!(
    "CodeStudioSessionTimelineRequest",
    "tentaflow_ws_handler_cs_session_timeline"
);
register_code_studio_variant!(
    "CodeStudioSessionOperationsRequest",
    "tentaflow_ws_handler_cs_session_operations"
);
register_code_studio_variant!(
    "CodeStudioOperationResolveRequest",
    "tentaflow_ws_handler_cs_operation_resolve"
);
register_code_studio_variant!(
    "CodeStudioApprovalsListRequest",
    "tentaflow_ws_handler_cs_approvals_list"
);
register_code_studio_variant!(
    "CodeStudioApprovalDecideRequest",
    "tentaflow_ws_handler_cs_approval_decide"
);
register_code_studio_variant!(
    "CodeStudioSessionGrantsListRequest",
    "tentaflow_ws_handler_cs_session_grants_list"
);
register_code_studio_variant!(
    "CodeStudioSessionGrantRevokeRequest",
    "tentaflow_ws_handler_cs_session_grant_revoke"
);
register_code_studio_variant!(
    "CodeStudioSessionRunsRequest",
    "tentaflow_ws_handler_cs_session_runs"
);
register_code_studio_variant!(
    "CodeStudioSessionTasksRequest",
    "tentaflow_ws_handler_cs_session_tasks"
);
register_code_studio_variant!(
    "CodeStudioSessionMessageSendRequest",
    "tentaflow_ws_handler_cs_session_message_send"
);
register_code_studio_variant!(
    "CodeStudioSessionCancelRequest",
    "tentaflow_ws_handler_cs_session_cancel"
);
register_code_studio_variant!(
    "CodeStudioExecStartRequest",
    "tentaflow_ws_handler_cs_exec_start"
);
register_code_studio_variant!(
    "CodeStudioExecCancelRequest",
    "tentaflow_ws_handler_cs_exec_cancel"
);
register_code_studio_variant!(
    "CodeStudioExecOutputRequest",
    "tentaflow_ws_handler_cs_exec_output"
);
register_code_studio_variant!(
    "CodeStudioTerminalOpenRequest",
    "tentaflow_ws_handler_cs_terminal_open"
);
register_code_studio_variant!(
    "CodeStudioTerminalInputRequest",
    "tentaflow_ws_handler_cs_terminal_input"
);
register_code_studio_variant!(
    "CodeStudioTerminalResizeRequest",
    "tentaflow_ws_handler_cs_terminal_resize"
);
register_code_studio_variant!(
    "CodeStudioTerminalCloseRequest",
    "tentaflow_ws_handler_cs_terminal_close"
);
register_code_studio_variant!(
    "CodeStudioTerminalSnapshotRequest",
    "tentaflow_ws_handler_cs_terminal_snapshot"
);
register_code_studio_variant!(
    "CodeStudioIndexStatusRequest",
    "tentaflow_ws_handler_cs_index_status"
);
register_code_studio_variant!(
    "CodeStudioIndexRebuildRequest",
    "tentaflow_ws_handler_cs_index_rebuild"
);
register_code_studio_variant!(
    "CodeStudioCodeSearchRequest",
    "tentaflow_ws_handler_cs_code_search"
);
register_code_studio_variant!(
    "CodeStudioPatchBlobGetRequest",
    "tentaflow_ws_handler_cs_patch_blob_get"
);
register_code_studio_variant!(
    "CodeStudioTerminalsListRequest",
    "tentaflow_ws_handler_cs_terminals_list"
);
register_code_studio_variant!(
    "CodeStudioWorkspaceMemberCandidatesRequest",
    "tentaflow_ws_handler_cs_workspace_member_candidates"
);
register_code_studio_variant!(
    "CodeStudioProjectLinkListRequest",
    "tentaflow_ws_handler_cs_project_link_list"
);
register_code_studio_variant!(
    "CodeStudioProjectLinkSetRequest",
    "tentaflow_ws_handler_cs_project_link_set"
);
register_code_studio_variant!(
    "CodeStudioRepoTreeRequest",
    "tentaflow_ws_handler_cs_repo_tree"
);
register_code_studio_variant!(
    "CodeStudioAgentCredentialsListRequest",
    "tentaflow_ws_handler_cs_agent_credentials_list"
);
register_code_studio_variant!(
    "CodeStudioAgentCredentialSetRequest",
    "tentaflow_ws_handler_cs_agent_credential_set"
);
register_code_studio_variant!(
    "CodeStudioAgentCredentialDeleteRequest",
    "tentaflow_ws_handler_cs_agent_credential_delete"
);

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use tentaflow_protocol::SessionAuth;


    /// The workspace layout is derived from a process-global storage category,
    /// so every test that touches disk has to hold this.

    struct Fixture {
        _data: tempfile::TempDir,
        ctx: HandlerContext,
    }

    fn context(state: std::sync::Arc<crate::dispatch::AppState>, org: OrgContext) -> HandlerContext {
        HandlerContext {
            session: SessionAuth::UserSession {
                user_id: [3u8; 16],
                role: None,
            },
            correlation_id: 1,
            connection_id: 0,
            resume_secret: None,
            state,
            org_context: Some(org),
        }
    }

    fn org(user_id: &str, permissions: &[&str]) -> OrgContext {
        OrgContext {
            user_id: user_id.to_string(),
            org_id: "org-1".to_string(),
            role_id: "role-1".to_string(),
            permissions: permissions.iter().map(|p| p.to_string()).collect(),
        }
    }

    fn fixture(user_id: &str, permissions: &[&str]) -> Fixture {
        let data = tempfile::tempdir().expect("data dir");
        crate::paths::set_category_override(
            crate::paths::StorageCategory::Data,
            Some(data.path().to_string_lossy().to_string()),
        );
        let state = crate::dispatch::AppState::for_test();
        Fixture {
            _data: data,
            ctx: context(state, org(user_id, permissions)),
        }
    }

    fn release() {
        crate::paths::set_category_override(crate::paths::StorageCategory::Data, None);
    }

    fn seed_user(ctx: &HandlerContext, user_id: &str) {
        let conn = ctx.state.db.write().expect("db");
        conn.execute(
            "INSERT OR IGNORE INTO user_accounts \
               (id, username, password_hash, display_name, email, is_active, is_admin, \
                created_at, updated_at, role) \
             VALUES (?1, ?1, 'x', ?1, ?1, 1, 0, datetime('now'), datetime('now'), 'user')",
            rusqlite::params![user_id],
        )
        .expect("seed user");
    }

    fn seed_workspace(ctx: &HandlerContext, id: &str, owner: &str, exec_mode: ExecMode) {
        repository::create_workspace(
            &ctx.state.db,
            &NewWorkspace {
                id: id.to_string(),
                org_id: "org-1".into(),
                owner_user_id: owner.to_string(),
                name: format!("Workspace {id}"),
                slug: id.to_string(),
                node_id: "test-node".into(),
                exec_mode,
                container_image: match exec_mode {
                    ExecMode::Container => Some("ghcr.io/example/dev:1".into()),
                    ExecMode::TrustedNative => None,
                },
                egress_enforcement: resolve_egress_enforcement(exec_mode),
                repo_kind: "empty".into(),
                repo_url: None,
                repo_auth_kind: Some("none".into()),
                secret_ref: None,
                ssh_host_fingerprint: None,
                default_branch: Some("main".into()),
                target_branch: None,
                autonomy_ceiling: AutonomyMode::Normal,
                egress_policy: "org_approved".into(),
                index_enabled: false,
                quota_disk_bytes: None,
                quota_sessions: None,
            },
        )
        .expect("create workspace");
    }

    fn activate(ctx: &HandlerContext, id: &str) {
        repository::set_status(&ctx.state.db, id, WorkspaceStatus::Active, None).expect("activate");
    }

    fn create_input<'a>(
        name: &'a str,
        exec_mode: &'a str,
        autonomy_ceiling: &'a str,
        egress_policy: &'a str,
    ) -> WorkspaceCreateInput<'a> {
        WorkspaceCreateInput {
            name,
            node_id: "test-node",
            exec_mode,
            container_image: None,
            repo_kind: "empty",
            repo_url: None,
            repo_auth_kind: None,
            secret_material: None,
            ssh_host_fingerprint: None,
            default_branch: Some("main"),
            autonomy_ceiling,
            egress_policy,
            index_enabled: false,
            members: &[],
        }
    }

    fn audit_actions(ctx: &HandlerContext, resource: &str) -> Vec<(String, String)> {
        let conn = ctx.state.db.read().expect("db");
        let mut stmt = conn
            .prepare("SELECT action, COALESCE(details,'') FROM audit_log WHERE resource = ?1")
            .expect("prepare");
        let rows = stmt
            .query_map(rusqlite::params![resource], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("query");
        rows.collect::<rusqlite::Result<Vec<_>>>().expect("rows")
    }

    /// Names of every request variant this dispatcher is expected to serve,
    /// read out of the PROTOCOL SOURCE.
    ///
    /// Deriving them from the source rather than from a list kept next to the
    /// registrations is the point: a test that walks the same hand-maintained
    /// list it is meant to police cannot notice a variant missing from both,
    /// which is exactly how a request type goes unreachable.
    fn dispatched_request_variants() -> Vec<String> {
        const PROTOCOL_SRC: &str = include_str!("../../../tentaflow-protocol/src/code_studio.rs");
        // Answered by a subscription rather than by this dispatcher.
        let stream_only: HashSet<&str> = [
            "SessionStreamRequest",
            "TerminalStreamRequest",
            "IndexStreamRequest",
        ]
        .into_iter()
        .collect();

        let body = PROTOCOL_SRC
            .split_once("pub enum CodeStudioPayload {")
            .expect("CodeStudioPayload enum")
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
            if name.ends_with("Request") && !stream_only.contains(name) {
                variants.push(format!("CodeStudio{name}"));
            }
        }
        assert!(
            variants.len() > 60,
            "the enum scan found only {} request variants — the parser drifted",
            variants.len()
        );
        variants
    }

    /// A handler that is written but not registered is invisible to
    /// `dispatch::find`, and the client sees NotImplemented for a variant the
    /// server can actually answer.
    #[test]
    fn every_supported_variant_resolves_to_a_handler() {
        for registered in dispatched_request_variants() {
            let handler = crate::dispatch::find(&registered)
                .unwrap_or_else(|| panic!("{registered} has no registered handler"));
            assert_eq!(
                handler.required_auth,
                crate::dispatch::SessionAuthKind::UserSession,
                "{registered} must stay at UserSession — membership is checked per handler"
            );
        }
    }

    /// One instance of every request the dispatcher serves, all aimed at the
    /// same workspace and session.
    ///
    /// Field values are deliberately plausible: a request that fails validation
    /// before the authorization gate would prove nothing about the gate.
    fn every_request(workspace_id: &str, session_id: &str) -> Vec<CodeStudioPayload> {
        use CodeStudioPayload as P;
        let ws = || workspace_id.to_string();
        let sess = || session_id.to_string();
        vec![
            P::WorkspacesListRequest {
                include_archived: false,
            },
            P::WorkspaceCreateRequest {
                name: "Nowy".into(),
                // Empty means "this node", so the request is served here rather
                // than forwarded over a mesh no test has.
                node_id: String::new(),
                exec_mode: "trusted_native".into(),
                container_image: None,
                repo_kind: "empty".into(),
                repo_url: None,
                repo_auth_kind: None,
                secret_material: None,
                ssh_host_fingerprint: None,
                default_branch: Some("main".into()),
                autonomy_ceiling: "normal".into(),
                egress_policy: "org_approved".into(),
                index_enabled: false,
                members: Vec::new(),
            },
            P::WorkspaceGetRequest { workspace_id: ws() },
            P::WorkspaceRetryRequest { workspace_id: ws() },
            P::WorkspaceArchiveRequest {
                workspace_id: ws(),
                archived: true,
            },
            P::WorkspaceDeleteRequest { workspace_id: ws() },
            P::WorkspaceSettingsUpdateRequest {
                workspace_id: ws(),
                name: "Zmiana".into(),
                autonomy_ceiling: "normal".into(),
                egress_policy: "org_approved".into(),
                target_branch: None,
                index_enabled: false,
                quota_disk_bytes: None,
                quota_sessions: None,
            },
            P::WorkspaceSecretSetRequest {
                workspace_id: ws(),
                repo_auth_kind: "token".into(),
                secret_material: Some("ghp_secret".into()),
                ssh_host_fingerprint: None,
            },
            P::WorkspaceMemberSetRequest {
                workspace_id: ws(),
                user_id: "u-intruder".into(),
                role: "owner".into(),
            },
            P::WorkspaceMemberRemoveRequest {
                workspace_id: ws(),
                user_id: "u-owner".into(),
            },
            P::WorkspaceCreatorGrantSetRequest {
                user_id: "u-intruder".into(),
                granted: true,
            },
            P::WorkspaceAllowlistListRequest { workspace_id: ws() },
            P::WorkspaceAllowlistSetRequest {
                workspace_id: ws(),
                capability: "exec".into(),
                pattern: "*".into(),
            },
            P::WorkspaceAllowlistRemoveRequest {
                workspace_id: ws(),
                capability: "exec".into(),
                pattern: "*".into(),
            },
            P::SessionsListRequest { workspace_id: ws() },
            P::SessionOpenRequest {
                workspace_id: ws(),
                title: "Sesja".into(),
                autonomy_mode: "normal".into(),
            },
            P::SessionCloseRequest {
                workspace_id: ws(),
                session_id: sess(),
            },
            P::SessionAutonomySetRequest {
                workspace_id: ws(),
                session_id: sess(),
                autonomy_mode: "plan".into(),
            },
            P::GitStatusRequest {
                workspace_id: ws(),
                session_id: sess(),
            },
            P::WorktreesListRequest {
                workspace_id: ws(),
                session_id: sess(),
            },
            P::FileTreeRequest {
                workspace_id: ws(),
                session_id: sess(),
                path: String::new(),
                depth: 1,
            },
            P::FileReadRequest {
                workspace_id: ws(),
                session_id: sess(),
                path: "README.md".into(),
                start_line: None,
                end_line: None,
            },
            P::FileWriteRequest {
                workspace_id: ws(),
                session_id: sess(),
                path: "README.md".into(),
                content: "hello".into(),
                expected_blob_sha: None,
            },
            P::FileCreateRequest {
                workspace_id: ws(),
                session_id: sess(),
                path: "new.txt".into(),
                content: String::new(),
            },
            P::FileDeleteRequest {
                workspace_id: ws(),
                session_id: sess(),
                path: "new.txt".into(),
                recursive: false,
                expected_blob_sha: None,
            },
            P::FileRenameRequest {
                workspace_id: ws(),
                session_id: sess(),
                from_path: "a.txt".into(),
                to_path: "b.txt".into(),
                expected_blob_sha: None,
            },
            P::FileMkdirRequest {
                workspace_id: ws(),
                session_id: sess(),
                path: "src".into(),
            },
            P::FileGrepRequest {
                workspace_id: ws(),
                session_id: sess(),
                query: "fn".into(),
                glob: String::new(),
                regex: false,
                max_results: 10,
            },
            P::GitLogRequest {
                workspace_id: ws(),
                session_id: sess(),
                path: String::new(),
                limit: 10,
            },
            P::GitBranchesRequest {
                workspace_id: ws(),
                session_id: sess(),
            },
            P::GitDiffRequest {
                workspace_id: ws(),
                session_id: sess(),
                path: String::new(),
                staged: false,
                base: String::new(),
            },
            P::GitCommitRequest {
                workspace_id: ws(),
                session_id: sess(),
                message: "wip".into(),
                patch_set_id: None,
            },
            P::GitPushRequest {
                workspace_id: ws(),
                session_id: sess(),
                remote: String::new(),
                set_upstream: false,
            },
            P::GitSyncRequest {
                workspace_id: ws(),
                session_id: sess(),
                mode: "fetch".into(),
            },
            P::GitMergeRequest {
                workspace_id: ws(),
                session_id: sess(),
                source_branch: "feature".into(),
                target_branch: "main".into(),
            },
            P::GitMergeFinalizeRequest {
                workspace_id: ws(),
                session_id: sess(),
                op_id: "op-1".into(),
                patch_set_id: "ps-1".into(),
            },
            P::GitMergeAbandonRequest {
                workspace_id: ws(),
                session_id: sess(),
                op_id: "op-1".into(),
            },
            P::PatchSetsListRequest {
                workspace_id: ws(),
                session_id: sess(),
                status: String::new(),
            },
            P::PatchSetGetRequest {
                workspace_id: ws(),
                session_id: sess(),
                patch_set_id: "ps-1".into(),
            },
            P::PatchDecideRequest {
                workspace_id: ws(),
                session_id: sess(),
                patch_set_id: "ps-1".into(),
                files: vec![PatchFileDecision {
                    patch_file_id: "pf-1".into(),
                    decision: "accept".into(),
                    note: None,
                    hunks: Vec::new(),
                }],
            },
            P::PatchSetAbandonRequest {
                workspace_id: ws(),
                session_id: sess(),
                patch_set_id: "ps-1".into(),
            },
            P::SessionTimelineRequest {
                workspace_id: ws(),
                session_id: sess(),
                after_seq: 0,
                limit: 10,
            },
            P::SessionOperationsRequest {
                workspace_id: ws(),
                session_id: sess(),
                status: String::new(),
                limit: 10,
            },
            P::OperationResolveRequest {
                workspace_id: ws(),
                session_id: sess(),
                op_id: "op-1".into(),
                resolution: "applied".into(),
                note: String::new(),
            },
            P::ApprovalsListRequest {
                workspace_id: ws(),
                session_id: sess(),
                status: String::new(),
            },
            P::ApprovalDecideRequest {
                workspace_id: ws(),
                session_id: sess(),
                approval_id: "ap-1".into(),
                decision: "allow_once".into(),
            },
            P::SessionGrantsListRequest {
                workspace_id: ws(),
                session_id: sess(),
            },
            P::SessionGrantRevokeRequest {
                workspace_id: ws(),
                session_id: sess(),
                capability: "exec".into(),
                pattern: "*".into(),
            },
            P::SessionRunsRequest {
                workspace_id: ws(),
                session_id: sess(),
            },
            P::SessionTasksRequest {
                workspace_id: ws(),
                session_id: sess(),
            },
            P::SessionMessageSendRequest {
                workspace_id: ws(),
                session_id: sess(),
                message: "zrób to".into(),
            },
            P::SessionCancelRequest {
                workspace_id: ws(),
                session_id: sess(),
                run_id: None,
            },
            P::ExecStartRequest {
                workspace_id: ws(),
                session_id: sess(),
                argv: vec!["cargo".into(), "test".into()],
                cwd: String::new(),
                timeout_secs: 30,
                mount_access: "ro".into(),
                network_access: "none".into(),
                ephemeral: false,
            },
            P::ExecCancelRequest {
                workspace_id: ws(),
                session_id: sess(),
                exec_id: "ex-1".into(),
            },
            P::ExecOutputRequest {
                workspace_id: ws(),
                session_id: sess(),
                exec_id: "ex-1".into(),
                after_seq: 0,
                limit: 200,
            },
            P::TerminalOpenRequest {
                workspace_id: ws(),
                session_id: sess(),
                rows: 24,
                cols: 80,
            },
            P::TerminalInputRequest {
                workspace_id: ws(),
                session_id: sess(),
                terminal_id: "t-1".into(),
                data: "ls\n".into(),
            },
            P::TerminalResizeRequest {
                workspace_id: ws(),
                session_id: sess(),
                terminal_id: "t-1".into(),
                rows: 40,
                cols: 120,
            },
            P::TerminalCloseRequest {
                workspace_id: ws(),
                session_id: sess(),
                terminal_id: "t-1".into(),
            },
            P::TerminalSnapshotRequest {
                workspace_id: ws(),
                session_id: sess(),
                terminal_id: "t-1".into(),
            },
            P::TerminalsListRequest {
                workspace_id: ws(),
                session_id: sess(),
            },
            P::IndexStatusRequest { workspace_id: ws() },
            P::IndexRebuildRequest {
                workspace_id: ws(),
                branch: "main".into(),
            },
            P::CodeSearchRequest {
                workspace_id: ws(),
                session_id: sess(),
                query: "fn main".into(),
                path_prefix: String::new(),
                limit: 10,
                mode: "hybrid".into(),
            },
            P::PatchBlobGetRequest {
                workspace_id: ws(),
                session_id: sess(),
                blob_sha: "0".repeat(40),
            },
            P::WorkspaceMemberCandidatesRequest {
                workspace_id: Some(ws()),
                query: "u".into(),
                limit: 10,
            },
            P::ProjectLinkListRequest { workspace_id: ws() },
            P::ProjectLinkSetRequest {
                workspace_id: ws(),
                project_id: "p-1".into(),
                linked: true,
            },
            P::RepoTreeRequest {
                workspace_id: ws(),
                project_id: "p-1".into(),
                commit: "0".repeat(40),
                path_prefix: String::new(),
                limit: 10,
            },
            // Empty node id means "this node", so the sweep exercises the
            // handler instead of a mesh forward no test has.
            P::AgentCredentialsListRequest {
                node_id: String::new(),
            },
            P::AgentCredentialSetRequest {
                node_id: String::new(),
                engine_id: "claude-code".into(),
                provider_base_url: "https://api.anthropic.com".into(),
                credential_material: "sk-ant-intruder".into(),
            },
            P::AgentCredentialDeleteRequest {
                node_id: String::new(),
                engine_id: "claude-code".into(),
            },
        ]
    }

    /// The two authorization sweeps below are only worth their name if the
    /// catalogue they walk really is every registered variant.
    #[test]
    fn the_authorization_sweep_covers_every_registered_variant() {
        let covered: HashSet<String> = every_request("ws-x", "s-x")
            .into_iter()
            .map(|payload| crate::dispatch::variant_name_of(&cs(payload)).to_string())
            .collect();
        let mut missing: Vec<String> = dispatched_request_variants()
            .into_iter()
            .filter(|variant| !covered.contains(variant))
            .collect();
        missing.sort();
        assert!(
            missing.is_empty(),
            "these variants are dispatched but never authorization-tested: {missing:?}"
        );
    }

    /// Every variant is registered at `SessionAuthKind::UserSession`, which is
    /// only defensible because each handler authorizes for itself. This is that
    /// claim executed rather than asserted: a session holding NO Code Studio
    /// permission reaches none of the sixty-seven, including the three that
    /// administer a workspace — delete, secret and the org-level create grant.
    #[tokio::test]
    async fn no_variant_acts_for_a_session_without_the_code_studio_permission() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-intruder", &[]);
        seed_workspace(&fx.ctx, "ws-gate", "u-owner", ExecMode::TrustedNative);
        activate(&fx.ctx, "ws-gate");

        for payload in every_request("ws-gate", "s-1") {
            let name = crate::dispatch::variant_name_of(&cs(payload.clone()));
            let error = code_studio_dispatch(&cs(payload), &fx.ctx)
                .await
                .err()
                .unwrap_or_else(|| panic!("{name} answered a session with no permission"));
            assert_eq!(
                error.code,
                ProtocolErrorCode::PolicyDenied,
                "{name} refused for the wrong reason: {}",
                error.message
            );
        }
        release();
    }

    /// The same sweep for someone who HAS `code_studio.read` but is not a member
    /// of the workspace. Every workspace-scoped variant has to answer NotFound,
    /// or the catalogue becomes a way of finding out which workspaces exist and
    /// who owns them (§25.4).
    #[tokio::test]
    async fn no_variant_admits_a_non_member_to_someone_elses_workspace() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-stranger", &[PERM_READ]);
        seed_user(&fx.ctx, "u-stranger");
        seed_workspace(&fx.ctx, "ws-gate", "u-owner", ExecMode::TrustedNative);
        activate(&fx.ctx, "ws-gate");

        for payload in every_request("ws-gate", "s-1") {
            let name = crate::dispatch::variant_name_of(&cs(payload.clone()));
            // The catalogue listing is the one answer that is not a refusal: a
            // stranger sees it, and sees nothing in it.
            if matches!(payload, CodeStudioPayload::WorkspacesListRequest { .. }) {
                let listed = code_studio_dispatch(&cs(payload), &fx.ctx)
                    .await
                    .expect("the catalogue answers everyone");
                let MessageBody::CodeStudioBody(CodeStudioPayload::WorkspacesListResponse {
                    workspaces,
                    ..
                }) = listed
                else {
                    panic!("unexpected response");
                };
                assert!(workspaces.is_empty(), "a stranger saw a workspace");
                continue;
            }
            let names_a_workspace = !matches!(
                payload,
                CodeStudioPayload::WorkspaceCreateRequest { .. }
                    | CodeStudioPayload::WorkspaceCreatorGrantSetRequest { .. }
                    | CodeStudioPayload::AgentCredentialsListRequest { .. }
                    | CodeStudioPayload::AgentCredentialSetRequest { .. }
                    | CodeStudioPayload::AgentCredentialDeleteRequest { .. }
            );
            let error = code_studio_dispatch(&cs(payload), &fx.ctx)
                .await
                .err()
                .unwrap_or_else(|| panic!("{name} served a non-member"));
            let expected = if names_a_workspace {
                ProtocolErrorCode::NotFound
            } else {
                // Creating a workspace and granting the right to create one are
                // org-level acts with no workspace to hide: they refuse openly.
                ProtocolErrorCode::PolicyDenied
            };
            assert_eq!(
                error.code, expected,
                "{name} refused for the wrong reason: {}",
                error.message
            );
        }
        release();
    }

    /// A stranger must not be able to tell someone else's workspace from a
    /// missing one — PolicyDenied would confirm the id exists.
    #[test]
    fn a_non_member_gets_not_found_rather_than_policy_denied() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-stranger", &[PERM_READ]);
        seed_workspace(&fx.ctx, "ws-secret", "u-owner", ExecMode::TrustedNative);

        let err = workspace_get_v1(&fx.ctx, "ws-secret").expect_err("must refuse");
        assert_eq!(err.code, ProtocolErrorCode::NotFound);
        let missing = workspace_get_v1(&fx.ctx, "ws-does-not-exist").expect_err("must refuse");
        assert_eq!(
            err.message, missing.message,
            "an existing workspace answers differently from a missing one"
        );
        release();
    }

    /// §25.4: the administrator overlay is metadata and lifecycle. Content —
    /// here the session list — stays behind real membership.
    #[test]
    fn an_administrator_sees_metadata_but_not_content() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-admin", &[PERM_READ, PERM_ADMIN]);
        seed_workspace(&fx.ctx, "ws-admin", "u-owner", ExecMode::TrustedNative);
        activate(&fx.ctx, "ws-admin");

        assert!(workspace_get_v1(&fx.ctx, "ws-admin").is_ok());
        let listed = workspaces_list_v1(&fx.ctx, false).expect("list");
        let MessageBody::CodeStudioBody(CodeStudioPayload::WorkspacesListResponse {
            workspaces,
            ..
        }) = listed
        else {
            panic!("unexpected response");
        };
        assert_eq!(workspaces.len(), 1);
        assert_eq!(workspaces[0].my_role, "none", "an overlay is not a role");

        let err = sessions_list_v1(&fx.ctx, "ws-admin").expect_err("content is off limits");
        assert_eq!(err.code, ProtocolErrorCode::NotFound);
        release();
    }

    /// §9.5: `trusted_native` promises no isolation, so it cannot carry the
    /// autonomous ceiling — hiding the option in the wizard is not validation.
    #[tokio::test]
    async fn trusted_native_cannot_be_autonomous() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-owner", &[PERM_READ]);
        seed_user(&fx.ctx, "u-owner");
        repository::grant_creator(&fx.ctx.state.db, "org-1", "u-owner", "u-admin").expect("grant");

        let err = workspace_create_v1(
            &fx.ctx,
            create_input("Autonomiczny", "trusted_native", "autonomous", "org_approved"),
        )
        .await
        .expect_err("must refuse");
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
        assert!(err.message.contains("autonomous"), "{}", err.message);
        release();
    }

    /// A network policy without a mechanism is a promise the node cannot keep.
    #[tokio::test]
    async fn local_only_is_refused_when_enforcement_is_unrestricted() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-owner", &[PERM_READ]);
        seed_user(&fx.ctx, "u-owner");
        repository::grant_creator(&fx.ctx.state.db, "org-1", "u-owner", "u-admin").expect("grant");

        let err = workspace_create_v1(
            &fx.ctx,
            create_input("Zamkniety", "trusted_native", "normal", "local_only"),
        )
        .await
        .expect_err("must refuse");
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
        assert!(err.message.contains("local_only"), "{}", err.message);
        release();
    }

    /// Container mode without an image cannot be provisioned, and the test node
    /// has no container engine at all — either way the request is refused
    /// before anything is written.
    #[tokio::test]
    async fn container_mode_without_an_image_is_refused() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-owner", &[PERM_READ]);
        seed_user(&fx.ctx, "u-owner");
        repository::grant_creator(&fx.ctx.state.db, "org-1", "u-owner", "u-admin").expect("grant");

        let err = workspace_create_v1(
            &fx.ctx,
            create_input("Kontener", "container", "normal", "org_approved"),
        )
        .await
        .expect_err("must refuse");
        // Two different refusals are possible and they are NOT interchangeable:
        // a node without a runtime cannot isolate anything, a node with one
        // still needs an image. Accepting either code without reading the
        // reason would let the image check disappear unnoticed on every test
        // machine, because those have no container runtime.
        match err.code {
            ProtocolErrorCode::NotAvailable => {
                assert!(
                    !node_supports_container(&fx.ctx),
                    "a node that CAN isolate refused for the wrong reason: {}",
                    err.message
                );
                assert!(err.message.contains("container"), "{}", err.message);
            }
            ProtocolErrorCode::BadRequest => {
                assert!(
                    err.message.contains("container_image"),
                    "the refusal does not name the missing image: {}",
                    err.message
                );
            }
            other => panic!("unexpected refusal {other:?}: {}", err.message),
        }

        // The policy check itself, isolated from node capabilities.
        let direct = validate_workspace_policy(
            ExecMode::Container,
            None,
            AutonomyMode::Normal,
            "org_approved",
            EgressEnforcement::Namespace,
        )
        .expect_err("container without an image");
        assert_eq!(direct.code, ProtocolErrorCode::BadRequest);
        assert!(validate_workspace_policy(
            ExecMode::Container,
            Some("ghcr.io/example/dev:1"),
            AutonomyMode::Autonomous,
            "local_only",
            EgressEnforcement::Namespace,
        )
        .is_ok());
        release();
    }

    /// §9.5: the execution mode is immutable. The settings request has no field
    /// for it and the update statement never names the column, so a workspace
    /// cannot change the guarantees a running session was opened under.
    #[test]
    fn settings_update_cannot_change_the_execution_mode() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-owner", &[PERM_READ]);
        seed_workspace(&fx.ctx, "ws-mode", "u-owner", ExecMode::TrustedNative);
        activate(&fx.ctx, "ws-mode");

        let request = CodeStudioPayload::WorkspaceSettingsUpdateRequest {
            workspace_id: "ws-mode".into(),
            name: "Zmieniony".into(),
            autonomy_ceiling: "auto_edit".into(),
            egress_policy: "org_approved".into(),
            target_branch: Some("main".into()),
            index_enabled: false,
            quota_disk_bytes: None,
            quota_sessions: None,
        };
        // The request type is not the guarantee; what it does to the record is.
        let _ = request;

        workspace_settings_update_v1(
            &fx.ctx,
            "ws-mode",
            "Zmieniony",
            "auto_edit",
            "org_approved",
            Some("main"),
            false,
            None,
            None,
        )
        .expect("update");
        let record = repository::get_workspace(&fx.ctx.state.db, "ws-mode")
            .unwrap()
            .unwrap();
        assert_eq!(record.exec_mode, "trusted_native");
        assert_eq!(record.name, "Zmieniony");
        assert_eq!(record.autonomy_ceiling, "auto_edit");

        // And a ceiling the mode cannot honour is still refused on update.
        let err = workspace_settings_update_v1(
            &fx.ctx,
            "ws-mode",
            "Zmieniony",
            "autonomous",
            "org_approved",
            None,
            false,
            None,
            None,
        )
        .expect_err("must refuse");
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
        release();
    }

    /// §7.1: omitting `exec_mode` resolves to `trusted_native` — a mode with no
    /// isolation — so the resolved value has to land in the audit chain.
    #[tokio::test]
    async fn an_omitted_exec_mode_resolves_to_trusted_native_and_is_audited() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-owner", &[PERM_READ]);
        seed_user(&fx.ctx, "u-owner");
        repository::grant_creator(&fx.ctx.state.db, "org-1", "u-owner", "u-admin").expect("grant");

        let response = workspace_create_v1(&fx.ctx, create_input("Domyslny", "", "normal", "any"))
            .await
            .expect("create");
        let MessageBody::CodeStudioBody(CodeStudioPayload::WorkspaceCreateResponse {
            workspace_id,
            status,
        }) = response
        else {
            panic!("unexpected response");
        };
        assert_eq!(status, "provisioning");

        let record = repository::get_workspace(&fx.ctx.state.db, &workspace_id)
            .unwrap()
            .unwrap();
        assert_eq!(record.exec_mode, "trusted_native");
        // A native workspace never gets a namespace; whether it gets a firewall
        // depends on how the node was set up, and the server decides that, not
        // the request.
        assert!(
            matches!(record.egress_enforcement.as_str(), "unrestricted" | "firewall"),
            "{}",
            record.egress_enforcement
        );

        let events = audit_actions(&fx.ctx, &workspace_id);
        let created = events
            .iter()
            .find(|(action, _)| action == "code_studio.workspace_create")
            .expect("no audit event for the create");
        assert!(created.1.contains("trusted_native"), "{}", created.1);
        assert!(
            created.1.contains("\"exec_mode_defaulted\":true"),
            "the audit event does not say the mode was defaulted: {}",
            created.1
        );
        release();
    }

    /// Credential material travels one way. Every response this module can
    /// produce is serialized and searched, because a leak here is permanent.
    #[tokio::test]
    async fn no_response_carries_credential_material() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-owner", &[PERM_READ, PERM_ADMIN]);
        seed_user(&fx.ctx, "u-owner");
        repository::grant_creator(&fx.ctx.state.db, "org-1", "u-owner", "u-admin").expect("grant");

        const TOKEN: &str = "ghp_averysecrettokenvalue";
        let mut input = create_input("Z sekretem", "trusted_native", "normal", "org_approved");
        input.repo_auth_kind = Some("token");
        input.secret_material = Some(TOKEN);
        let created = workspace_create_v1(&fx.ctx, input).await.expect("create");
        let MessageBody::CodeStudioBody(CodeStudioPayload::WorkspaceCreateResponse {
            workspace_id,
            ..
        }) = created.clone()
        else {
            panic!("unexpected response");
        };

        let mut responses = vec![created];
        responses.push(workspace_get_v1(&fx.ctx, &workspace_id).expect("get"));
        responses.push(workspaces_list_v1(&fx.ctx, true).expect("list"));
        responses.push(
            workspace_secret_set_v1(
                &fx.ctx,
                &workspace_id,
                "token",
                Some("ghp_rotatedvalue"),
                None,
            )
            .expect("secret set"),
        );

        for response in responses {
            let json = serde_json::to_string(&response).expect("json");
            assert!(!json.contains(TOKEN), "{json}");
            assert!(!json.contains("ghp_rotatedvalue"), "{json}");
            assert!(!json.contains("secret_material"), "{json}");
            assert!(!json.contains("cs-secret-"), "vault handle on the wire: {json}");
        }

        // The fingerprint the UI shows is a digest, never the token.
        let response = workspace_secret_set_v1(&fx.ctx, &workspace_id, "token", Some(TOKEN), None)
            .expect("secret set");
        let MessageBody::CodeStudioBody(CodeStudioPayload::WorkspaceSecretSetResponse {
            has_secret,
            fingerprint,
            ..
        }) = response
        else {
            panic!("unexpected response");
        };
        assert!(has_secret);
        assert_eq!(
            fingerprint.as_deref(),
            Some(vault::fingerprint_of(SecretKind::GitToken, TOKEN).as_str())
        );
        release();
    }

    // =========================================================================
    // Provider credentials (§5.2, §7.5)
    // =========================================================================

    const PROVIDER_KEY: &str = "sk-ant-api03-organizationsecretvalue";
    const PROVIDER_URL: &str = "https://api.anthropic.com";

    /// Discards what the adapter reports. The delegation block journals these
    /// onto the session timeline; a credential test has no timeline and needs
    /// none.
    struct DroppingSink;

    impl crate::code_studio::cli_adapter::AdapterEventSink for DroppingSink {
        fn record(&self, _event: EventPayload) {}
    }

    /// Records the organization's Phase 0B decision so the gate stops being the
    /// first thing that refuses. Without it `start_adapter` never reaches the
    /// vault at all, and the test would prove nothing about the credential.
    fn record_go_no_go(ctx: &HandlerContext, engine_id: &str) {
        use crate::code_studio::cli_adapter::{
            BASE_URL_OVERRIDE_VERIFIED_PREFIX, GO_NO_GO_NOTE_PREFIX,
        };
        crate::db::repository::set_setting(
            &ctx.state.db,
            &format!("{BASE_URL_OVERRIDE_VERIFIED_PREFIX}{engine_id}"),
            "true",
        )
        .expect("verified flag");
        crate::db::repository::set_setting(
            &ctx.state.db,
            &format!("{GO_NO_GO_NOTE_PREFIX}{engine_id}"),
            "verified against the pinned CLI in a test",
        )
        .expect("go/no-go note");
    }

    async fn start_adapter_for_test(
        ctx: &HandlerContext,
        dir: &std::path::Path,
        engine_id: &str,
    ) -> anyhow::Result<crate::code_studio::cli_adapter::AdapterHandle> {
        crate::code_studio::cli_adapter::start_adapter(
            &ctx.state.db,
            &ctx.state.settings_cipher,
            crate::code_studio::cli_adapter::AdapterConfig {
                bind_addr: ([127, 0, 0, 1], 0).into(),
                engine_id: engine_id.to_string(),
                org_id: "org-1".to_string(),
                node_id: ctx.state.local_node_id.to_string(),
                ca_path: dir.join("ca.pem"),
                cli_home_dir: dir.join("cli-home"),
                egress_enforcement: EgressEnforcement::Unrestricted,
                dns_names: vec!["localhost".to_string()],
                tickets: Arc::new(crate::code_studio::cli_adapter::TicketRegistry::new()),
                sink: Arc::new(DroppingSink),
            },
        )
        .await
    }

    /// The defect this closes: `code_agent_credentials` had exactly one writer
    /// and it lived behind `#[cfg(test)]`, so `delegate_cli` could not succeed
    /// on any installation. The proof is the whole path — the protocol handler
    /// writes, and the component that actually consumes the row reads.
    #[tokio::test]
    async fn a_stored_provider_credential_is_what_the_adapter_reads() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-admin", &[PERM_READ, PERM_ADMIN]);
        record_go_no_go(&fx.ctx, "claude-code");
        let dir = tempfile::tempdir().expect("adapter dir");

        let Err(refusal) = start_adapter_for_test(&fx.ctx, dir.path(), "claude-code").await else {
            panic!("an adapter must not start without a credential");
        };
        assert!(
            format!("{refusal:#}").contains("credential_missing"),
            "{refusal:#}"
        );

        agent_credential_set_v1(
            &fx.ctx,
            "",
            "claude-code",
            PROVIDER_URL,
            PROVIDER_KEY,
        )
        .expect("store the provider credential");

        let handle = start_adapter_for_test(&fx.ctx, dir.path(), "claude-code")
            .await
            .expect("the adapter starts once the node holds the credential");
        handle.shutdown();
        release();
    }

    /// The material travels one way. Every answer the credential handlers can
    /// produce, plus the audit event they write, is searched for the raw bytes.
    #[tokio::test]
    async fn no_provider_credential_response_or_audit_entry_carries_the_material() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-admin", &[PERM_READ, PERM_ADMIN]);

        let responses = vec![
            agent_credential_set_v1(&fx.ctx, "", "claude-code", PROVIDER_URL, PROVIDER_KEY)
                .expect("store"),
            agent_credentials_list_v1(&fx.ctx, "").expect("list"),
            agent_credential_delete_v1(&fx.ctx, "", "claude-code").expect("delete"),
        ];
        for response in &responses {
            let json = serde_json::to_string(response).expect("json");
            assert!(!json.contains(PROVIDER_KEY), "{json}");
            assert!(!json.contains("credential_material"), "{json}");
        }

        for (action, details) in audit_actions(&fx.ctx, "test-node/claude-code") {
            assert!(!details.contains(PROVIDER_KEY), "{action}: {details}");
        }
        let actions: Vec<String> = audit_actions(&fx.ctx, "test-node/claude-code")
            .into_iter()
            .map(|(action, _)| action)
            .collect();
        assert!(actions.contains(&"code_studio.agent_credential_set".to_string()));
        assert!(actions.contains(&"code_studio.agent_credential_delete".to_string()));

        // The listing reports the digest, which identifies the key without
        // being able to reconstruct it.
        agent_credential_set_v1(&fx.ctx, "", "claude-code", PROVIDER_URL, PROVIDER_KEY)
            .expect("store again");
        let MessageBody::CodeStudioBody(CodeStudioPayload::AgentCredentialsListResponse {
            credentials,
            ..
        }) = agent_credentials_list_v1(&fx.ctx, "").expect("list")
        else {
            panic!("unexpected response");
        };
        assert_eq!(
            credentials[0].fingerprint.as_deref(),
            Some(vault::fingerprint_of(SecretKind::GitToken, PROVIDER_KEY).as_str())
        );
        release();
    }

    /// Rotation replaces the material in place. A second row would mean the
    /// adapter picking one of two keys, and the operator having no way to
    /// retire the first.
    #[tokio::test]
    async fn rotating_a_provider_credential_replaces_it_instead_of_adding_one() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-admin", &[PERM_READ, PERM_ADMIN]);

        agent_credential_set_v1(&fx.ctx, "", "claude-code", PROVIDER_URL, "sk-ant-first")
            .expect("store");
        agent_credential_set_v1(&fx.ctx, "", "claude-code", PROVIDER_URL, "sk-ant-second")
            .expect("rotate");

        let MessageBody::CodeStudioBody(CodeStudioPayload::AgentCredentialsListResponse {
            credentials,
            ..
        }) = agent_credentials_list_v1(&fx.ctx, "").expect("list")
        else {
            panic!("unexpected response");
        };
        assert_eq!(credentials.len(), 1, "rotation left a second row");
        assert!(credentials[0].rotated_at.is_some(), "rotation was not stamped");
        assert_eq!(
            credentials[0].fingerprint.as_deref(),
            Some(vault::fingerprint_of(SecretKind::GitToken, "sk-ant-second").as_str())
        );

        // And the material the adapter would inject is the new one.
        let credential = vault::get_agent_credential(
            &fx.ctx.state.db,
            &fx.ctx.state.settings_cipher,
            "org-1",
            "test-node",
            "claude-code",
        )
        .expect("read back");
        assert_eq!(credential.material.expose(), "sk-ant-second");
        release();
    }

    /// A credential belongs to ONE node: the material is sealed with that
    /// node's key (§5.2). Neither the listing nor the vault may present one
    /// node's key as another's, and a write aimed at a node this Core is not
    /// must be refused rather than stored where nothing can open it.
    #[tokio::test]
    async fn a_provider_credential_belongs_to_exactly_one_node() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-admin", &[PERM_READ, PERM_ADMIN]);
        agent_credential_set_v1(&fx.ctx, "", "claude-code", PROVIDER_URL, PROVIDER_KEY)
            .expect("store on this node");

        let error = agent_credential_set_v1(
            &fx.ctx,
            "some-other-node",
            "claude-code",
            PROVIDER_URL,
            PROVIDER_KEY,
        )
        .expect_err("a foreign node's vault is not writable from here");
        assert_eq!(error.code, ProtocolErrorCode::NotAvailable);

        assert!(
            vault::list_agent_credentials(&fx.ctx.state.db, "org-1", "some-other-node")
                .expect("list")
                .is_empty(),
            "this node's credential showed up as another node's"
        );
        let missing = vault::get_agent_credential(
            &fx.ctx.state.db,
            &fx.ctx.state.settings_cipher,
            "org-1",
            "some-other-node",
            "claude-code",
        )
        .expect_err("another node must not resolve this node's credential");
        assert!(matches!(missing, VaultError::CredentialMissing { .. }));
        release();
    }

    /// Sessions are private per user and §25.4 grants no exception: an
    /// administrator who is a member still sees only their own sessions.
    #[test]
    fn a_session_list_never_shows_another_users_sessions_even_to_an_admin() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-admin", &[PERM_READ, PERM_ADMIN]);
        seed_workspace(&fx.ctx, "ws-private", "u-owner", ExecMode::TrustedNative);
        activate(&fx.ctx, "ws-private");
        // The administrator joins the members table, which is the only way to
        // content (§25.4) — and even then the sessions stay someone else's.
        repository::upsert_member(
            &fx.ctx.state.db,
            "ws-private",
            "u-admin",
            WorkspaceRole::Owner,
            "u-admin",
        )
        .expect("join");

        paths::create_workspace_layout("ws-private").expect("layout");
        let pool = workspace_db::open("ws-private").expect("runtime db");
        {
            let conn = pool.write().expect("write");
            for (id, user) in [("s-owner", "u-owner"), ("s-admin", "u-admin")] {
                conn.execute(
                    "INSERT INTO sessions (id, workspace_id, user_id, title, branch, \
                       autonomy_mode, flow_id, flow_version_id, status, created_at, updated_at) \
                     VALUES (?1, 'ws-private', ?2, 'Praca', 'cs/x/y', 'normal', 'cs-harness', \
                       'v1', 'idle', datetime('now'), datetime('now'))",
                    rusqlite::params![id, user],
                )
                .expect("seed session");
            }
        }

        let response = sessions_list_v1(&fx.ctx, "ws-private").expect("list");
        let MessageBody::CodeStudioBody(CodeStudioPayload::SessionsListResponse {
            sessions, ..
        }) = response
        else {
            panic!("unexpected response");
        };
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "s-admin");

        // The other user's session is not reachable by id either.
        let err = require_own_session(&pool, "s-owner", "u-admin").expect_err("must refuse");
        assert_eq!(err.code, ProtocolErrorCode::NotFound);
        workspace_db::close("ws-private");
        release();
    }

    /// A standing `always` grant for a mandatory-interactive capability is
    /// refused at WRITE time (§9.3 rule 5), not quietly ignored when the PEP
    /// reads the table back.
    #[test]
    fn the_allowlist_refuses_capabilities_that_must_always_ask() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-owner", &[PERM_READ]);
        seed_workspace(&fx.ctx, "ws-allow", "u-owner", ExecMode::TrustedNative);
        activate(&fx.ctx, "ws-allow");

        for capability in ["git_push", "git_merge", "git_merge_finalize", "secret_manage", "git_worktree"] {
            let err = allowlist_set_v1(&fx.ctx, "ws-allow", capability, "*")
                .expect_err("must refuse");
            assert_eq!(err.code, ProtocolErrorCode::BadRequest, "{capability}");
        }

        let response = allowlist_set_v1(&fx.ctx, "ws-allow", "net_egress", "crates.io:443")
            .expect("allowed capability");
        let MessageBody::CodeStudioBody(CodeStudioPayload::WorkspaceAllowlistListResponse {
            entries,
            ..
        }) = response
        else {
            panic!("unexpected response");
        };
        // A new workspace already carries the seeded read-only `exec` programs,
        // so this asserts about the capability the test is exercising.
        let egress: Vec<_> = entries
            .iter()
            .filter(|entry| entry.capability == "net_egress")
            .collect();
        assert_eq!(egress.len(), 1);
        assert_eq!(egress[0].pattern, "crates.io:443");

        allowlist_remove_v1(&fx.ctx, "ws-allow", "net_egress", "crates.io:443").expect("remove");
        assert!(read_allowlist(&fx.ctx.state.db, "ws-allow")
            .expect("read")
            .iter()
            .all(|entry| entry.capability != "net_egress"));
        release();
    }

    /// A workspace owned by another node is not silently handled here — the
    /// caller learns which node can serve it.
    #[test]
    fn a_workspace_owned_by_another_node_says_so() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-owner", &[PERM_READ]);
        seed_workspace(&fx.ctx, "ws-remote", "u-owner", ExecMode::TrustedNative);
        {
            let conn = fx.ctx.state.db.write().expect("db");
            conn.execute(
                "UPDATE code_workspaces SET node_id = 'other-node', status = 'active' \
                 WHERE id = 'ws-remote'",
                [],
            )
            .expect("move workspace");
        }

        // Reached directly, the handler still refuses by name — the dispatcher
        // forwards before a handler ever sees a foreign workspace.
        let err = sessions_list_v1(&fx.ctx, "ws-remote").expect_err("must refuse");
        assert_eq!(err.code, ProtocolErrorCode::NotAvailable);
        assert!(err.message.contains("other-node"), "{}", err.message);

        // Metadata still resolves, and it tells the truth about locality.
        let response = workspace_get_v1(&fx.ctx, "ws-remote").expect("metadata");
        let MessageBody::CodeStudioBody(CodeStudioPayload::WorkspaceGetResponse {
            workspace, ..
        }) = response
        else {
            panic!("unexpected response");
        };
        assert!(!workspace.is_local);
        assert_eq!(workspace.node_id, "other-node");
        release();
    }

    /// Joining the members table is the ONLY route to content for an
    /// administrator, and it leaves two marks the owner can see: the audit
    /// entry and `added_by` on the member row.
    #[test]
    fn an_admin_joining_a_workspace_leaves_a_visible_trail() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-admin", &[PERM_READ, PERM_ADMIN]);
        seed_user(&fx.ctx, "u-admin");
        seed_workspace(&fx.ctx, "ws-trail", "u-owner", ExecMode::TrustedNative);
        activate(&fx.ctx, "ws-trail");

        workspace_member_set_v1(&fx.ctx, "ws-trail", "u-admin", "viewer").expect("self join");
        let members = members_to_wire(&fx.ctx, "ws-trail");
        let joined = members
            .iter()
            .find(|m| m.user_id == "u-admin")
            .expect("the admin is not in the member list");
        assert_eq!(joined.added_by, "u-admin", "the owner cannot see who joined");
        assert!(audit_actions(&fx.ctx, "ws-trail")
            .iter()
            .any(|(action, _)| action == "code_studio.admin_member_self_add"));

        // Adding someone ELSE is still the owner's decision.
        seed_user(&fx.ctx, "u-third");
        let err = workspace_member_set_v1(&fx.ctx, "ws-trail", "u-third", "editor")
            .expect_err("must refuse");
        assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
        release();
    }

    #[test]
    fn porcelain_records_keep_a_rename_together_with_its_source() {
        let records = vec![
            "M  src/main.rs".to_string(),
            "R  src/new.rs".to_string(),
            "src/old.rs".to_string(),
            "?? notes.md".to_string(),
        ];
        let entries = parse_porcelain(&records);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].path, "src/main.rs");
        assert_eq!(entries[0].index_status, "M");
        assert_eq!(entries[1].path, "src/new.rs");
        assert_eq!(entries[1].old_path.as_deref(), Some("src/old.rs"));
        assert_eq!(entries[2].path, "notes.md");
        assert_eq!(entries[2].index_status, "?");
    }

    // =========================================================================
    // Session-scoped fixtures
    // =========================================================================

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    struct Live {
        fx: Fixture,
        workspace_id: String,
        session_id: String,
    }

    impl Live {
        fn scope(&self) -> Scope {
            let org = self.fx.ctx.org_context.as_ref().expect("org");
            session_scope(
                &self.fx.ctx,
                org,
                &self.workspace_id,
                &self.session_id,
                WorkspaceRole::Viewer,
            )
            .expect("scope")
        }
    }

    /// A provisioned workspace with one open session, or `None` when git is not
    /// installed — the repository half of Code Studio cannot be faked.
    fn live(workspace_id: &str, ceiling: AutonomyMode, autonomy: AutonomyMode) -> Option<Live> {
        if !git_available() {
            eprintln!("skipping: git is not installed");
            return None;
        }
        let fx = fixture("u-owner", &[PERM_READ]);
        seed_user(&fx.ctx, "u-owner");
        seed_workspace(&fx.ctx, workspace_id, "u-owner", ExecMode::TrustedNative);
        {
            let conn = fx.ctx.state.db.write().expect("db");
            conn.execute(
                "UPDATE code_workspaces SET autonomy_ceiling = ?2 WHERE id = ?1",
                rusqlite::params![workspace_id, ceiling.slug()],
            )
            .expect("ceiling");
        }
        let record = repository::get_workspace(&fx.ctx.state.db, workspace_id)
            .unwrap()
            .unwrap();
        provisioning::provision(&fx.ctx.state.db, &record, &ProvisionAuth::None).expect("provision");
        let record = repository::get_workspace(&fx.ctx.state.db, workspace_id)
            .unwrap()
            .unwrap();
        let session_id = format!("{workspace_id}-s1");
        session::open_session(
            &record,
            WorkspaceRole::Owner,
            &NewSession {
                id: session_id.clone(),
                user_id: "u-owner".into(),
                user_slug: "owner".into(),
                title: "Praca".into(),
                autonomy_mode: autonomy,
                flow_id: "cs-harness".into(),
                flow_version_id: "v1".into(),
            },
        )
        .expect("open session");

        Some(Live {
            fx,
            workspace_id: workspace_id.to_string(),
            session_id,
        })
    }

    fn write_file(live: &Live, path: &str, content: &str, expected: Option<&str>) -> MessageBody {
        file_write_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            path,
            content,
            expected,
        )
        .expect("write")
    }

    fn head_commit(live: &Live) -> String {
        let broker = Broker::for_workspace(&live.workspace_id).expect("broker");
        let handle = broker.session(&live.session_id).expect("handle");
        broker.head_commit(&handle).expect("head")
    }

    /// §9.3 gate 5a: a commit without an accepted patch set is NOT a refusal.
    /// It opens the review — and nothing is committed until a person decides.
    #[test]
    fn a_commit_without_an_accepted_patch_set_opens_a_review() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-gate", AutonomyMode::AutoEdit, AutonomyMode::AutoEdit) else {
            return;
        };
        write_file(&live, "src/main.rs", "fn main() {}\n", None);
        let before = head_commit(&live);

        let response = git_commit_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            "add main",
            None,
        )
        .expect("commit request");
        let MessageBody::CodeStudioBody(CodeStudioPayload::GitCommitResponse {
            status,
            commit_oid,
            patch_set_id,
            ..
        }) = response
        else {
            panic!("unexpected response");
        };
        assert_eq!(status, "review_required");
        assert!(commit_oid.is_none(), "the worktree was committed anyway");
        let patch_set_id = patch_set_id.expect("the gate opened no patch set");
        assert_eq!(head_commit(&live), before, "the branch moved without a review");

        // The review really describes the change, which is what makes it a
        // review rather than a permission prompt.
        let detail = patch_set_get_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            &patch_set_id,
        )
        .expect("patch set");
        let MessageBody::CodeStudioBody(CodeStudioPayload::PatchSetGetResponse { files, .. }) =
            detail
        else {
            panic!("unexpected response");
        };
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, "src/main.rs");
        assert!(!files[0].hunks.is_empty(), "the review shows no diff");
        release();
    }

    /// After the acceptance the same call commits — from the ACCEPTED BLOBS,
    /// not from whatever the worktree holds by then.
    #[tokio::test]
    async fn an_accepted_review_commits_the_accepted_blobs() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-commit", AutonomyMode::AutoEdit, AutonomyMode::AutoEdit) else {
            return;
        };
        write_file(&live, "src/main.rs", "fn main() {}\n", None);
        let MessageBody::CodeStudioBody(CodeStudioPayload::GitCommitResponse {
            patch_set_id: Some(patch_set_id),
            ..
        }) = git_commit_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            "add main",
            None,
        )
        .expect("review")
        else {
            panic!("unexpected response");
        };

        let scope = live.scope();
        let set = patch::load_patch_set(&scope.pool, &patch_set_id).expect("set");
        let file_id = set.files[0].id.clone();
        patch_decide_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            &patch_set_id,
            &[PatchFileDecision {
                patch_file_id: file_id,
                decision: "accept".into(),
                note: None,
                hunks: Vec::new(),
            }],
        )
        .await
        .expect("decide");

        // Accepting the review answers the question the gate opened, and the
        // question names the patch set it decides.
        let (approval, named_set): (String, Option<String>) = {
            let conn = scope.pool.read().expect("pool");
            conn.query_row(
                "SELECT id, patch_set_id FROM approvals \
                 WHERE capability = 'git_commit' AND status = 'pending'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("the review opened no approval")
        };
        assert_eq!(
            named_set.as_deref(),
            Some(patch_set_id.as_str()),
            "the approval does not name the patch set it decides"
        );
        approval_decide_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            &approval,
            "allow_once",
        )
        .expect("approve");

        // The agent keeps typing during the review; the commit must still carry
        // what was accepted.
        write_file(
            &live,
            "src/main.rs",
            "fn main() { panic!() }\n",
            Some(&cs_fs::blob_sha(b"fn main() {}\n")),
        );

        let response = git_commit_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            "add main",
            Some(&patch_set_id),
        )
        .expect("commit");
        let MessageBody::CodeStudioBody(CodeStudioPayload::GitCommitResponse {
            status,
            commit_oid: Some(commit_oid),
            ..
        }) = response
        else {
            panic!("unexpected response");
        };
        assert_eq!(status, "committed");

        let broker = Broker::for_workspace(&live.workspace_id).expect("broker");
        let reference = broker.reference();
        let committed = broker
            .blob_in_commit(&reference, &commit_oid, "src/main.rs")
            .expect("blob")
            .expect("path is missing from the commit");
        assert_eq!(
            committed,
            cs_fs::blob_sha(b"fn main() {}\n"),
            "the commit took the worktree instead of the accepted blob"
        );
        release();
    }

    /// §9.5 — in `autonomous` the ACCEPTANCE of the review may be automatic,
    /// per workspace and off until somebody turns it on. What does not change is
    /// where the content comes from: the commit is still built from the patch
    /// set's accepted blobs, so the record of what was committed exists even
    /// when no person read it.
    #[test]
    fn autonomous_accepts_its_own_review_only_where_the_workspace_says_so() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-auto", AutonomyMode::Autonomous, AutonomyMode::Autonomous) else {
            return;
        };
        write_file(&live, "src/main.rs", "fn main() {}\n", None);
        let before = head_commit(&live);

        // Default: `autonomous` alone decides nothing. Absence of the standing
        // permission IS the off switch.
        let MessageBody::CodeStudioBody(CodeStudioPayload::GitCommitResponse { status, .. }) =
            git_commit_v1(
                &live.fx.ctx,
                &live.workspace_id,
                &live.session_id,
                "add main",
                None,
            )
            .expect("review")
        else {
            panic!("unexpected response");
        };
        assert_eq!(status, "review_required", "autonomous decided by itself");
        assert_eq!(head_commit(&live), before, "the branch moved without a review");

        // Turned on the way §9.1 stores every standing permission: a
        // workspace-level entry, written by the owner and audited.
        allowlist_set_v1(&live.fx.ctx, &live.workspace_id, "review_decide", "*")
            .expect("standing permission");

        let MessageBody::CodeStudioBody(CodeStudioPayload::GitCommitResponse {
            status,
            commit_oid: Some(commit_oid),
            patch_set_id: Some(patch_set_id),
            ..
        }) = git_commit_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            "add main",
            None,
        )
        .expect("commit")
        else {
            panic!("unexpected response");
        };
        assert_eq!(status, "committed");

        // The acceptance is a REAL acceptance of the patch set, not a bypass:
        // the set carries the blob the commit was built from.
        let scope = live.scope();
        let set = patch::load_patch_set(&scope.pool, &patch_set_id).expect("set");
        let accepted = set.files[0]
            .accepted_blob_sha
            .clone()
            .expect("the set was committed without an accepted blob");
        assert_eq!(accepted, cs_fs::blob_sha(b"fn main() {}\n"));
        let broker = Broker::for_workspace(&live.workspace_id).expect("broker");
        assert_eq!(
            broker
                .blob_in_commit(&broker.reference(), &commit_oid, "src/main.rs")
                .expect("blob")
                .expect("path is missing from the commit"),
            accepted,
            "the commit took the worktree instead of the accepted blob"
        );

        // And the question the gate opened does not stay pending as a card
        // nobody can decide any more.
        let pending: i64 = {
            let conn = scope.pool.read().expect("pool");
            conn.query_row(
                "SELECT COUNT(*) FROM approvals WHERE capability = 'git_commit' AND status = 'pending'",
                [],
                |row| row.get(0),
            )
            .expect("approvals")
        };
        assert_eq!(pending, 0, "an answered review is still pending");
        release();
    }

    /// §11.4 — the address policy belongs to the REMOTE, so it has to be applied
    /// to the url the push will really use. A session names that url, and
    /// without this check the broker handed the string straight to git.
    #[test]
    fn a_push_to_a_forbidden_remote_is_refused_before_git_runs() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-remote", AutonomyMode::Normal, AutonomyMode::Normal) else {
            return;
        };
        for remote in [
            // Cloud instance metadata and loopback: never a git server.
            "https://169.254.169.254/repo.git",
            "https://127.0.0.1/repo.git",
            // Unauthenticated transports.
            "http://git.example.com/repo.git",
            "git://git.example.com/repo.git",
            // A credential in the url would reach `ps` and the reflog.
            "https://ghp_token@git.example.com/repo.git",
        ] {
            let error = git_push_v1(
                &live.fx.ctx,
                &live.workspace_id,
                &live.session_id,
                remote,
                false,
            )
            .expect_err(&format!("{remote} was accepted"));
            assert_eq!(error.code, ProtocolErrorCode::BadRequest, "{remote}");
        }
        release();
    }

    /// An acceptance given in another session is another person's decision
    /// about another branch, and it must not unlock a commit here.
    #[tokio::test]
    async fn an_acceptance_in_another_session_does_not_unlock_a_commit() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-scoped", AutonomyMode::AutoEdit, AutonomyMode::AutoEdit) else {
            return;
        };
        let record = repository::get_workspace(&live.fx.ctx.state.db, &live.workspace_id)
            .unwrap()
            .unwrap();
        let other_id = format!("{}-s2", live.workspace_id);
        session::open_session(
            &record,
            WorkspaceRole::Owner,
            &NewSession {
                id: other_id.clone(),
                user_id: "u-owner".into(),
                user_slug: "owner".into(),
                title: "Druga".into(),
                autonomy_mode: AutonomyMode::AutoEdit,
                flow_id: "cs-harness".into(),
                flow_version_id: "v1".into(),
            },
        )
        .expect("second session");

        // The other session reviews and accepts its own change.
        file_write_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &other_id,
            "other.txt",
            "other\n",
            None,
        )
        .expect("write");
        let MessageBody::CodeStudioBody(CodeStudioPayload::GitCommitResponse {
            patch_set_id: Some(other_set),
            ..
        }) = git_commit_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &other_id,
            "other",
            None,
        )
        .expect("review")
        else {
            panic!("unexpected response");
        };
        let scope_other = session_scope(
            &live.fx.ctx,
            live.fx.ctx.org_context.as_ref().unwrap(),
            &live.workspace_id,
            &other_id,
            WorkspaceRole::Viewer,
        )
        .expect("scope");
        let set = patch::load_patch_set(&scope_other.pool, &other_set).expect("set");
        patch_decide_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &other_id,
            &other_set,
            &[PatchFileDecision {
                patch_file_id: set.files[0].id.clone(),
                decision: "accept".into(),
                note: None,
                hunks: Vec::new(),
            }],
        )
        .await
        .expect("decide");

        // The first session still has to open its own review.
        write_file(&live, "mine.txt", "mine\n", None);
        let MessageBody::CodeStudioBody(CodeStudioPayload::GitCommitResponse { status, .. }) =
            git_commit_v1(
                &live.fx.ctx,
                &live.workspace_id,
                &live.session_id,
                "mine",
                None,
            )
            .expect("commit request")
        else {
            panic!("unexpected response");
        };
        assert_eq!(
            status, "review_required",
            "another session's acceptance unlocked this commit"
        );
        release();
    }

    /// `set_upstream` is refused BY NAME and before anything else, so a caller
    /// learns the capability does not exist here instead of reading a field
    /// validation. It is not a gap to fill later: an upstream points at a remote
    /// NAME resolved from repository config, and §11.4 lets only the
    /// policy-checked url decide where a push goes.
    #[test]
    fn an_upstream_is_refused_by_name_and_before_the_gate() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-upstream", AutonomyMode::AutoEdit, AutonomyMode::AutoEdit) else {
            return;
        };
        let err = git_push_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            "https://93.184.216.34/repo.git",
            true,
        )
        .expect_err("an upstream must be refused");
        assert_eq!(err.code, ProtocolErrorCode::NotAvailable);
        assert!(
            err.message.starts_with("set_upstream_unsupported:"),
            "{}",
            err.message
        );
        // The refusal costs nothing: no question was opened for a call that
        // can never run.
        let scope = live.scope();
        let pending: i64 = {
            let conn = scope.pool.read().expect("pool");
            conn.query_row(
                "SELECT COUNT(*) FROM approvals WHERE capability = 'git_push'",
                [],
                |row| row.get(0),
            )
            .expect("count")
        };
        assert_eq!(pending, 0);
        release();
    }

    /// §9.3 rule 5: a push asks every single time, whatever is stored and
    /// whatever the autonomy mode is.
    #[test]
    fn a_push_asks_even_with_a_standing_allowlist_and_a_session_grant() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-push", AutonomyMode::AutoEdit, AutonomyMode::AutoEdit) else {
            return;
        };
        // Written straight into the tables, the way a tampered or legacy row
        // would look: the gate must not care.
        {
            let conn = live.fx.ctx.state.db.write().expect("db");
            conn.execute(
                "INSERT INTO code_workspace_allowlist \
                   (workspace_id, capability, pattern, created_by, created_at) \
                 VALUES (?1, 'git_push', '*', 'u-owner', datetime('now'))",
                rusqlite::params![live.workspace_id],
            )
            .expect("allowlist");
        }
        {
            let scope = live.scope();
            let conn = scope.pool.write().expect("pool");
            conn.execute(
                "INSERT INTO session_grants (session_id, capability, pattern, granted_by, \
                  created_at) VALUES (?1, 'git_push', '*', 'u-owner', datetime('now'))",
                rusqlite::params![live.session_id],
            )
            .expect("grant");
        }

        // A public literal address, so the question below comes from rule 5 and
        // from nothing else: the workspace itself is `empty` and has no remote
        // of its own to push to.
        let err = git_push_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            "https://93.184.216.34/repo.git",
            false,
        )
        .expect_err("a push must ask");
        // One shape for "waiting on a person" across the family: the error
        // names the approval, and no body pretends the push produced an
        // operation it never journaled.
        assert_eq!(err.code, ProtocolErrorCode::Conflict);
        assert!(
            err.message.starts_with("approval_required:"),
            "{}",
            err.message
        );

        // And the question is a real, pending row a person can answer.
        let scope = live.scope();
        let approval: String = {
            let conn = scope.pool.read().expect("pool");
            conn.query_row(
                "SELECT id FROM approvals WHERE capability = 'git_push' AND status = 'pending'",
                [],
                |row| row.get(0),
            )
            .expect("pending approval")
        };

        // `always` for a mandatory-interactive capability is refused AT THE
        // WRITE, not quietly dropped when it is read back.
        let err = approval_decide_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            &approval,
            "always",
        )
        .expect_err("always on a push");
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
        assert!(err.message.contains("git_push"), "{}", err.message);
        release();
    }

    /// `git_worktree` is the coordinator's own capability: it is how a session
    /// would step outside its isolation.
    ///
    /// Two halves, because the PEP rule alone proves nothing about reach: no
    /// verb of the agent's tool surface maps onto it (`tools::capability_of`),
    /// and no handler here gates on it — so nothing can even ask.
    #[test]
    fn git_worktree_is_out_of_reach_for_agents_and_terminals() {
        for tool in crate::agents::CoreToolName::all().iter().copied() {
            assert_ne!(
                tools::capability_of(tool),
                Some(Capability::GitWorktree),
                "{} would let an agent create a worktree",
                tool.public_name()
            );
        }
        const SRC: &str = include_str!("code_studio.rs");
        let gated = SRC
            .lines()
            .filter(|line| line.contains("Capability::GitWorktree"))
            .filter(|line| line.contains("gate(") || line.contains("granted_profile("))
            .count();
        assert_eq!(gated, 0, "a handler gates on the coordinator's capability");

        let ctx = SessionCtx {
            role: WorkspaceRole::Owner,
            autonomy: AutonomyMode::Autonomous,
            is_coordinator: false,
            has_accepted_patch_set: true,
            allowlisted: true,
            session_granted: true,
            run_granted: true,
        };
        assert!(matches!(
            pep::authorize(&ctx, Capability::GitWorktree, &Target::None),
            Decision::Deny { .. }
        ));
        assert!(!is_allowlistable(Capability::GitWorktree));
        assert!(
            !Capability::GitWorktree.is_mandatory_interactive(),
            "a system capability is refused, not asked about"
        );
        assert!(Capability::GitWorktree.is_system());
    }

    /// A path that climbs out of the worktree is refused by the POLICY ENGINE,
    /// with the boundary as the reason — not by a string check somewhere on the
    /// way in.
    #[test]
    fn a_write_outside_the_worktree_is_refused() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-escape", AutonomyMode::AutoEdit, AutonomyMode::AutoEdit) else {
            return;
        };
        for path in ["../escape.txt", "/etc/passwd", "sub/../../out.txt"] {
            let err = file_write_v1(
                &live.fx.ctx,
                &live.workspace_id,
                &live.session_id,
                path,
                "x",
                None,
            )
            .expect_err("must refuse");
            assert_eq!(err.code, ProtocolErrorCode::PolicyDenied, "{path}");
            assert!(err.message.contains("worktree"), "{}", err.message);
        }
        release();
    }

    /// Editing the same file twice is ordinary work. It must not look like a
    /// lost race just because the first write moved the blob.
    #[test]
    fn a_second_edit_of_the_same_file_is_not_a_conflict() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-edit", AutonomyMode::AutoEdit, AutonomyMode::AutoEdit) else {
            return;
        };
        let first = write_file(&live, "notes.md", "one\n", None);
        let MessageBody::CodeStudioBody(CodeStudioPayload::FileWriteResponse { blob_sha, .. }) =
            first
        else {
            panic!("unexpected response");
        };
        assert_eq!(blob_sha, cs_fs::blob_sha(b"one\n"));

        let second = file_write_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            "notes.md",
            "two\n",
            Some(&blob_sha),
        )
        .expect("the second edit was refused as a conflict");
        let MessageBody::CodeStudioBody(CodeStudioPayload::FileWriteResponse {
            blob_sha: second_sha,
            ..
        }) = second
        else {
            panic!("unexpected response");
        };
        assert_eq!(second_sha, cs_fs::blob_sha(b"two\n"));

        // A stale expectation, on the other hand, IS a conflict.
        let err = file_write_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            "notes.md",
            "three\n",
            Some(&blob_sha),
        )
        .expect_err("a stale compare-and-swap must lose");
        assert_eq!(err.code, ProtocolErrorCode::Conflict);
        release();
    }

    /// Answering the card RESUMES the run that is parked on it.
    ///
    /// The console has exactly one answer channel — `codeStudioApprovalDecideRequest`
    /// — and it used to update the row, append the event and return, never
    /// touching the interaction the row itself names. So a suspended tool call,
    /// a `delegate_cli` turn and a vendor CLI's request all sat until their
    /// timeout while the operator looked at an approval they had already
    /// answered. The row carried the `interaction_id` the whole time.
    #[test]
    fn deciding_an_approval_wakes_the_run_parked_on_it() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-resume", AutonomyMode::AutoEdit, AutonomyMode::Normal) else {
            return;
        };
        let scope = live.scope();

        // A registry interaction, exactly as `suspend_for_operator` mints one:
        // the id the run parks on IS the id written on the approval row.
        let interaction = uuid::Uuid::new_v4().to_string();
        let approval_id = record_approval(
            &scope,
            Capability::Exec,
            "cargo",
            &interaction,
            "run 'cargo test'",
        )
        .expect("record approval");
        let mut parked = crate::agents::interaction_registry_global().register(
            crate::agents::PendingInteraction {
                id: interaction.clone(),
                run_id: "run-parked".to_string(),
                parent_run_id: None,
                kind: crate::agents::InteractionKind::Permission,
                prompt: "run 'cargo test'".to_string(),
                choices: Vec::new(),
                addon_id: None,
                tool_name: None,
                permission: Some("exec".to_string()),
                raised_at_ms: 0,
            },
        );

        let answer = approval_decide_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            &approval_id,
            "allow_once",
        )
        .expect("decide");
        let MessageBody::CodeStudioBody(CodeStudioPayload::ApprovalDecideResponse {
            resumed,
            ..
        }) = answer
        else {
            panic!("unexpected response");
        };
        assert!(resumed, "the handler did not report waking anything");
        match parked.try_recv() {
            Ok(crate::agents::InteractionReply::Permission(
                crate::agents::PermissionDecision::AllowOnce,
            )) => {}
            other => panic!(
                "the parked run was never handed the operator's answer: {other:?}"
            ),
        }
        release();
    }

    /// One decision, one `approval_decided` event.
    ///
    /// Two writers used to append it under two idempotency keys — the handler
    /// as `approval:<id>:decided`, the suspended call as `approval-dec:<id>` —
    /// so every answered card produced two entries on the timeline, one of them
    /// attributed to the run's own user rather than to the person who decided.
    /// Both now go through `tools::settle_approval`, and the entry follows the
    /// row TRANSITION: whoever closes the row writes it, the other writes none.
    #[test]
    fn one_decision_leaves_one_decided_event() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-one-event", AutonomyMode::AutoEdit, AutonomyMode::Normal)
        else {
            return;
        };
        let scope = live.scope();
        let interaction = uuid::Uuid::new_v4().to_string();
        let approval_id = record_approval(
            &scope,
            Capability::Exec,
            "cargo",
            &interaction,
            "run 'cargo test'",
        )
        .expect("record approval");

        approval_decide_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            &approval_id,
            "allow_once",
        )
        .expect("decide");

        // What the woken call does next, verbatim: it settles the same row with
        // the answer it received.
        let closed = tools::settle_approval(
            &scope.pool,
            &scope.session.id,
            &approval_id,
            "allow_once",
            "u-owner",
        )
        .expect("settle");
        assert!(
            !closed,
            "the awaiting call must not re-close a row the operator already closed"
        );

        let decided = {
            let conn = scope.pool.read().expect("pool");
            conn.query_row(
                "SELECT COUNT(*) FROM session_events WHERE session_id = ?1 AND kind = ?2",
                rusqlite::params![scope.session.id, "approval_decided"],
                |row| row.get::<_, i64>(0),
            )
            .expect("count")
        };
        assert_eq!(decided, 1, "one decision must leave one timeline entry");
        release();
    }

    /// §9.1: the object of a permission is "capability + cel". `approval_decide_v1`
    /// reads the approval's target and then throws it away (`let _ = target_pattern;`),
    /// storing the hard-coded pattern `"*"` for both `always` and
    /// `allow_for_session`. Since `pattern_matches("*", _)` is unconditionally
    /// true, a human who approved ONE command has silently granted every command
    /// of that capability — `allow_for_session` for the whole session, `always`
    /// for the whole WORKSPACE and every member of it, permanently.
    ///
    /// The agent-side twin gets this right: `code_studio::tools::persist_grant`
    /// stores `target_label`, the concrete argv[0]/path the PEP was asked about.
    #[test]
    fn adversarial_an_approval_for_one_command_grants_every_command() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-grant-scope", AutonomyMode::AutoEdit, AutonomyMode::Normal)
        else {
            return;
        };
        let scope = live.scope();

        // The operator is asked about exactly one program.
        let interaction = interaction_id(&scope.session.id, Capability::Exec, "cargo");
        let approval_id = record_approval(
            &scope,
            Capability::Exec,
            "cargo",
            &interaction,
            "run 'cargo test'",
        )
        .expect("record approval");

        approval_decide_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            &approval_id,
            "always",
        )
        .expect("decide");

        let scope = live.scope();
        assert!(
            allowlist_grants(&scope, Capability::Exec, Some("cargo")).expect("grants"),
            "the approved program is not covered by the grant it produced"
        );
        assert!(
            !allowlist_grants(&scope, Capability::Exec, Some("curl")).expect("grants"),
            "approving 'cargo' granted 'curl' as well: the target was dropped and \
             the stored pattern is '*'"
        );
    }

    /// §9.2 puts `workspace_settings` — every durable workspace-scope policy
    /// write — at owner only, and `allowlist_set_v1` enforces it
    /// (`Access::Member(WorkspaceRole::Owner)`). `approval_decide_v1` is gated
    /// at `WorkspaceRole::Editor` and writes the SAME table, so an editor
    /// answering their own approval card installs a standing workspace-wide
    /// allowlist row that binds the owner's sessions too — and cannot remove it
    /// afterwards, because `allowlist_remove_v1` is owner-only.
    #[test]
    fn adversarial_an_editor_writes_a_workspace_wide_standing_grant() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-editor-grant", AutonomyMode::AutoEdit, AutonomyMode::Normal)
        else {
            return;
        };
        // Same person, demoted to editor. The session stays theirs, so
        // `session_scope(..., Editor)` still admits them.
        {
            let conn = live.fx.ctx.state.db.write().expect("db");
            conn.execute(
                "UPDATE code_workspace_members SET role = 'editor' \
                 WHERE workspace_id = ?1 AND user_id = 'u-owner'",
                rusqlite::params![live.workspace_id],
            )
            .expect("demote");
        }

        let scope = live.scope();
        let interaction = interaction_id(&scope.session.id, Capability::Exec, "cargo");
        let approval_id = record_approval(
            &scope,
            Capability::Exec,
            "cargo",
            &interaction,
            "run 'cargo test'",
        )
        .expect("record approval");

        let outcome = approval_decide_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            &approval_id,
            "always",
        );
        assert!(
            outcome.is_err(),
            "an editor installed a workspace-level standing allowlist entry, which \
             `allowlist_set_v1` reserves for the owner and `allowlist_remove_v1` \
             lets only the owner take back"
        );
    }

    /// §9.5 — the session mode never exceeds the workspace ceiling, and the
    /// ceiling is not a value copied into the session at open time: lowering it
    /// has to stop work that is already running, otherwise "the ceiling" is a
    /// promise about future sessions only.
    #[test]
    fn lowering_the_ceiling_stops_a_session_already_running_above_it() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-ceiling", AutonomyMode::AutoEdit, AutonomyMode::AutoEdit) else {
            return;
        };
        // The session writes without asking, which is what `auto_edit` means.
        write_file(&live, "src/main.rs", "fn main() {}\n", None);
        assert_eq!(live.scope().autonomy().expect("autonomy"), AutonomyMode::AutoEdit);

        {
            let conn = live.fx.ctx.state.db.write().expect("db");
            conn.execute(
                "UPDATE code_workspaces SET autonomy_ceiling = 'plan' WHERE id = ?1",
                rusqlite::params![live.workspace_id],
            )
            .expect("lower the ceiling");
        }

        let scope = live.scope();
        assert_eq!(
            scope.autonomy().expect("autonomy"),
            AutonomyMode::Plan,
            "the live session kept the mode the ceiling no longer allows"
        );
        let err = file_write_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            "src/main.rs",
            "fn main() { unreviewed() }\n",
            None,
        )
        .expect_err("a session above the ceiling kept writing");
        assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
        assert!(err.message.contains("plan mode"), "{}", err.message);
        release();
    }

    /// §9.4 exempts a RUNNING PROCESS from re-authorization, not a new call.
    /// Typing into an open shell is a new call, so it passes the PEP like every
    /// other one — gating only `terminal_open` would leave a shell accepting
    /// commands after the mode was lowered or the grant revoked.
    #[test]
    fn terminal_input_and_resize_pass_the_policy_engine() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-term-gate", AutonomyMode::AutoEdit, AutonomyMode::Plan) else {
            return;
        };
        let err = terminal_input_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            "no-such-terminal",
            "rm -rf /\n",
        )
        .expect_err("plan mode has no terminal input");
        assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
        assert!(err.message.contains("plan mode"), "{}", err.message);

        let err = terminal_resize_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            "no-such-terminal",
            40,
            120,
        )
        .expect_err("plan mode has no terminal resize");
        assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
        release();
    }

    /// The session-scoped half of §9.1: a grant is capability + TARGET, in
    /// every scope an operator can choose.
    #[test]
    fn a_session_grant_covers_the_command_it_was_given_for_and_no_other() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-session-grant", AutonomyMode::AutoEdit, AutonomyMode::Normal)
        else {
            return;
        };
        let scope = live.scope();
        let interaction = interaction_id(&scope.session.id, Capability::Exec, "cargo");
        let approval_id = record_approval(
            &scope,
            Capability::Exec,
            "cargo",
            &interaction,
            "run 'cargo test'",
        )
        .expect("record approval");
        approval_decide_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            &approval_id,
            "allow_for_session",
        )
        .expect("decide");

        let scope = live.scope();
        assert!(session_grants_match(&scope, Capability::Exec, Some("cargo")).expect("grants"));
        assert!(
            !session_grants_match(&scope, Capability::Exec, Some("curl")).expect("grants"),
            "a session grant for 'cargo' also covers 'curl'"
        );
        release();
    }

    /// `plan` is a real mode: it has no execution, so it has no terminal.
    #[test]
    fn a_terminal_is_refused_in_plan_mode() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-plan", AutonomyMode::AutoEdit, AutonomyMode::Plan) else {
            return;
        };
        let err = terminal_open_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            24,
            80,
        )
        .expect_err("plan mode has no terminal");
        assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
        assert!(err.message.contains("plan mode"), "{}", err.message);

        // And so does an ordinary command.
        let err = exec_start_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            &["true".to_string()],
            "",
            10,
            "cow",
            "none",
            true,
        )
        .expect_err("plan mode has no exec");
        assert_eq!(err.code, ProtocolErrorCode::PolicyDenied);
        release();
    }

    /// A credential that reaches an argv must not survive into the timeline or
    /// into the artifact a person reads afterwards.
    #[test]
    fn a_token_in_an_argv_is_redacted_in_the_event_and_the_artifact() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-redact", AutonomyMode::AutoEdit, AutonomyMode::AutoEdit) else {
            return;
        };
        const TOKEN: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let argv = vec![
            "curl".to_string(),
            "-H".to_string(),
            format!("Authorization: Bearer {TOKEN}"),
            format!("https://example.invalid/?token={TOKEN}"),
        ];
        let scope = live.scope();

        // The event path is the one the exec handler uses.
        append_event(
            &scope,
            "exec:redaction-check",
            EventPayload::Exec {
                op_id: "op-1".into(),
                argv: argv.clone(),
                cwd: String::new(),
                exit_code: Some(0),
                requested_mount_access: "cow".into(),
                writes_discarded: true,
            },
        )
        .expect("append");
        let stored = events::read_after(&scope.pool, &live.session_id, 0, 100).expect("read");
        let exec_events: Vec<_> = stored
            .iter()
            .filter(|e| matches!(e.payload, EventPayload::Exec { .. }))
            .collect();
        assert_eq!(exec_events.len(), 1);
        assert!(
            !serde_json::to_string(&exec_events[0].payload)
                .unwrap()
                .contains(TOKEN),
            "the token survived into the timeline"
        );

        // And the artifact the handler stores is redacted before it is written.
        let transcript = redact::redact_text(&format!(
            "$ {}\n",
            redact::redact_argv(&argv).join(" ")
        ));
        assert!(!transcript.contains(TOKEN), "{transcript}");
        let stored = artifacts::put(
            &scope.pool,
            &live.workspace_id,
            transcript.as_bytes(),
            "exec_output",
        )
        .expect("artifact");
        let bytes = artifacts::get(&scope.pool, &live.workspace_id, &stored.sha256).expect("get");
        assert!(
            !String::from_utf8_lossy(&bytes).contains(TOKEN),
            "the token survived into the artifact"
        );
        release();
    }

    /// Lets a command run to its journal row, or gives up loudly rather than
    /// hanging: `exec_start` answers while the command is still running, so
    /// every assertion about its EFFECT has to wait for the operation to close.
    async fn await_operation(scope: &Scope, op_id: &str) -> Operation {
        for _ in 0..600 {
            let operation = operations::get(&scope.pool, op_id)
                .expect("operations::get")
                .expect("the operation exists");
            if operation.status != OperationStatus::Pending {
                return operation;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the command never closed its operation");
    }

    /// A caller that asks for `rw` gets `cow`, because §7.2 pins `exec` to a
    /// copy-on-write workplace. Taking the word and dropping the meaning is the
    /// bug: `rm` reports `exit 0`, the file is still there, and nothing in the
    /// answer distinguishes that from a deletion. The narrowing is refused
    /// nowhere — a request may always be narrowed — but it is STATED, in the
    /// answer and in the timeline, as two fields a caller can branch on.
    #[tokio::test]
    async fn a_command_narrowed_to_cow_says_its_writes_were_discarded() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-cow-truth", AutonomyMode::AutoEdit, AutonomyMode::AutoEdit)
        else {
            return;
        };
        write_file(&live, "scratch.txt", "keep me\n", None);

        // Every mode asks before a command, so the grant is the human half of
        // §9.3 — the point of the test is the profile, not the gate.
        let scope = live.scope();
        let interaction = interaction_id(&scope.session.id, Capability::Exec, "sh");
        let approval_id =
            record_approval(&scope, Capability::Exec, "sh", &interaction, "run 'rm'")
                .expect("record approval");
        approval_decide_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            &approval_id,
            "allow_for_session",
        )
        .expect("decide");

        let response = exec_start_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            &[
                "sh".to_string(),
                "-c".to_string(),
                "rm -f scratch.txt; echo removed".to_string(),
            ],
            "",
            60,
            "rw",
            "none",
            false,
        )
        .expect("exec start");
        let MessageBody::CodeStudioBody(CodeStudioPayload::ExecStartResponse {
            exec_id,
            mount_access,
            requested_mount_access,
            writes_discarded,
            ..
        }) = response
        else {
            panic!("unexpected response");
        };
        assert_eq!(requested_mount_access, "rw", "the request was not echoed");
        assert_eq!(mount_access, "cow", "exec is pinned to copy-on-write");
        assert!(
            writes_discarded,
            "a caller narrowed to cow was left believing it wrote to the worktree"
        );

        let scope = live.scope();
        let operation = await_operation(&scope, &exec_id).await;
        assert_eq!(operation.status, OperationStatus::Completed);

        // The command succeeded and the worktree did not move: the file the
        // command deleted is still readable through the session.
        let read = file_read_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            "scratch.txt",
            None,
            None,
        )
        .expect("the worktree file survived the command");
        let MessageBody::CodeStudioBody(CodeStudioPayload::FileReadResponse { content, .. }) = read
        else {
            panic!("unexpected response");
        };
        assert_eq!(content, "keep me\n");

        // And the timeline row that reports `exit 0` carries the same two
        // facts, so a reader of the history is not misled either.
        let events = events::read_after(&scope.pool, &live.session_id, 0, 200).expect("timeline");
        let exec_event = events
            .iter()
            .find_map(|event| match &event.payload {
                EventPayload::Exec {
                    op_id,
                    exit_code,
                    requested_mount_access,
                    writes_discarded,
                    ..
                } if *op_id == exec_id => {
                    Some((*exit_code, requested_mount_access.clone(), *writes_discarded))
                }
                _ => None,
            })
            .expect("the command has a timeline row");
        assert_eq!(exec_event.0, Some(0), "the command did not report success");
        assert_eq!(exec_event.1, "rw");
        assert!(exec_event.2);
        release();
    }

    /// Reading the output of a finished command answers with its lines. Before
    /// this request existed, the transcript was reachable only as an artifact
    /// digest in a timeline row, which no client can dereference — the caller
    /// could see THAT a command printed something and never what.
    #[tokio::test]
    async fn the_output_of_a_finished_command_can_be_read_back() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-exec-output", AutonomyMode::AutoEdit, AutonomyMode::Normal)
        else {
            return;
        };
        let scope = live.scope();
        let interaction = interaction_id(&scope.session.id, Capability::Exec, "sh");
        let approval_id =
            record_approval(&scope, Capability::Exec, "sh", &interaction, "run 'echo'")
                .expect("record approval");
        approval_decide_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            &approval_id,
            "allow_for_session",
        )
        .expect("decide");

        let response = exec_start_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            &[
                "sh".to_string(),
                "-c".to_string(),
                "echo first; echo second".to_string(),
            ],
            "",
            60,
            "cow",
            "none",
            true,
        )
        .expect("exec start");
        let MessageBody::CodeStudioBody(CodeStudioPayload::ExecStartResponse {
            exec_id, ..
        }) = response
        else {
            panic!("unexpected response");
        };
        let scope = live.scope();
        await_operation(&scope, &exec_id).await;

        let output = exec_output_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            &exec_id,
            0,
            200,
        )
        .expect("output");
        let MessageBody::CodeStudioBody(CodeStudioPayload::ExecOutputResponse {
            status,
            lines,
            next_seq,
            has_more,
            ..
        }) = output
        else {
            panic!("unexpected response");
        };
        assert_eq!(status, "completed");
        assert!(
            lines.iter().any(|line| line == "first")
                && lines.iter().any(|line| line == "second"),
            "the command's stdout is missing: {lines:?}"
        );
        assert_eq!(next_seq, lines.len() as u64);
        assert!(!has_more);

        // The cursor is a line cursor, so a poller asking for the tail gets the
        // tail and not the transcript again.
        let tail = exec_output_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            &exec_id,
            next_seq,
            200,
        )
        .expect("tail");
        let MessageBody::CodeStudioBody(CodeStudioPayload::ExecOutputResponse {
            lines: tail_lines,
            ..
        }) = tail
        else {
            panic!("unexpected response");
        };
        assert!(tail_lines.is_empty(), "{tail_lines:?}");

        // A command of another session is not reachable by guessing its id.
        let err = exec_output_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            "op-of-another-session",
            0,
            200,
        )
        .expect_err("a guessed id must not answer");
        assert_eq!(err.code, ProtocolErrorCode::NotFound);
        release();
    }

    /// Indexing is a per-workspace switch (§14). A workspace that has it off
    /// says so instead of failing, and a rebuild for one is refused by name —
    /// an empty result would read as "nothing matched", which is a different
    /// and much worse answer.
    #[test]
    fn a_rebuild_of_an_unindexed_workspace_is_refused_by_name() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-owner", &[PERM_READ]);
        seed_workspace(&fx.ctx, "ws-index", "u-owner", ExecMode::TrustedNative);
        activate(&fx.ctx, "ws-index");

        let response = index_status_v1(&fx.ctx, "ws-index").expect("status");
        let MessageBody::CodeStudioBody(CodeStudioPayload::IndexStatusResponse {
            index_enabled,
            ..
        }) = response
        else {
            panic!("unexpected response");
        };
        assert!(!index_enabled);

        let err = index_rebuild_v1(&fx.ctx, "ws-index", "main").expect_err("must refuse");
        assert_eq!(err.code, ProtocolErrorCode::NotAvailable);
        assert!(err.message.contains("indexing is disabled"), "{}", err.message);
        release();
    }

    #[test]
    fn a_diff_is_split_into_addressable_hunks() {
        let diff = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1,3 @@\n ctx\n+added\n@@ -10,1 +11,2 @@\n-gone\n";
        let hunks = split_hunks(diff);
        assert_eq!(hunks.len(), 2);
        assert!(hunks[0].header.starts_with("@@ -1,2"));
        assert!(hunks[0].content.contains("+added"));
        assert_eq!(hunks[1].idx, 1);
        assert!(hunks[1].content.contains("-gone"));
    }

    /// The PEP hands out a profile; a caller may narrow it and may not widen it.
    #[test]
    fn a_requested_profile_never_widens_the_granted_one() {
        let granted = pep::SandboxProfile {
            mount: MountAccess::CopyOnWrite,
            network: NetworkAccess::None,
        };
        let widened = narrow_profile(granted, MountAccess::ReadWrite, NetworkAccess::Gateway);
        assert_eq!(widened.mount, MountAccess::CopyOnWrite);
        assert_eq!(widened.network, NetworkAccess::None);

        let narrowed = narrow_profile(granted, MountAccess::ReadOnly, NetworkAccess::None);
        assert_eq!(narrowed.mount, MountAccess::ReadOnly);
    }


    // =========================================================================
    // Owner-node routing (§3, §12)
    // =========================================================================

    /// A call for a workspace this node does not own must go through the proxy,
    /// not end at a local refusal. The test node carries no mesh, so the proxy
    /// stops at its FIRST step — and that step is what proves the routing ran:
    /// `require_local` would have named the owner node instead.
    #[tokio::test]
    async fn a_request_for_a_remote_workspace_is_routed_to_its_owner() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-owner", &[PERM_READ]);
        seed_workspace(&fx.ctx, "ws-routed", "u-owner", ExecMode::TrustedNative);
        {
            let conn = fx.ctx.state.db.write().expect("db");
            conn.execute(
                "UPDATE code_workspaces SET node_id = 'other-node', status = 'active' \
                 WHERE id = 'ws-routed'",
                [],
            )
            .expect("move workspace");
        }

        let request = MessageBody::CodeStudioBody(CodeStudioPayload::SessionsListRequest {
            workspace_id: "ws-routed".into(),
        });
        let err = code_studio_dispatch(&request, &fx.ctx)
            .await
            .expect_err("no mesh on this node");
        assert_eq!(err.code, ProtocolErrorCode::NotAvailable);
        assert!(
            err.message.contains("mesh transport is not running"),
            "the call did not reach the proxy: {}",
            err.message
        );
        release();
    }

    /// A registry read is answered from the synced tables on ANY node. Routing
    /// it would buy a round trip and a new failure mode for data this node
    /// already holds.
    #[tokio::test]
    async fn a_registry_read_of_a_remote_workspace_is_answered_locally() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-owner", &[PERM_READ]);
        seed_workspace(&fx.ctx, "ws-meta", "u-owner", ExecMode::TrustedNative);
        {
            let conn = fx.ctx.state.db.write().expect("db");
            conn.execute(
                "UPDATE code_workspaces SET node_id = 'other-node' WHERE id = 'ws-meta'",
                [],
            )
            .expect("move workspace");
        }
        let request = MessageBody::CodeStudioBody(CodeStudioPayload::WorkspaceGetRequest {
            workspace_id: "ws-meta".into(),
        });
        let response = code_studio_dispatch(&request, &fx.ctx)
            .await
            .expect("metadata is local");
        let MessageBody::CodeStudioBody(CodeStudioPayload::WorkspaceGetResponse {
            workspace, ..
        }) = response
        else {
            panic!("unexpected response");
        };
        assert!(!workspace.is_local);
        release();
    }

    /// The routing table must cover exactly the calls a handler refuses when the
    /// workspace is foreign. A new session-scoped variant that is not listed
    /// would silently answer NotAvailable on a laptop instead of travelling.
    #[test]
    fn every_session_scoped_request_names_its_workspace_for_routing() {
        let payloads = [
            CodeStudioPayload::SessionsListRequest {
                workspace_id: "ws".into(),
            },
            CodeStudioPayload::SessionCloseRequest {
                workspace_id: "ws".into(),
                session_id: "s".into(),
            },
            CodeStudioPayload::GitStatusRequest {
                workspace_id: "ws".into(),
                session_id: "s".into(),
            },
        ];
        for payload in payloads {
            assert!(
                route_target(&payload).is_some(),
                "{payload:?} would never reach its owner node"
            );
        }
        // A response is not a request and must not be routed anywhere.
        assert!(route_target(&CodeStudioPayload::SessionCloseResponse {
            session_id: "s".into(),
            status: "closed".into(),
        })
        .is_none());
    }

    /// The id the assertion binds and the id the journal deduplicates by have to
    /// be the same value, or a re-sent request runs its effect twice.
    #[test]
    fn the_mesh_operation_id_is_the_one_the_journal_derives() {
        let digest = crate::code_studio::assertion::digest_hex(b"payload");
        assert_eq!(
            remote_proxy::remote_op_id("sess-7", b"payload"),
            operations::op_id(
                "sess-7",
                OriginKind::Ui,
                &digest,
                remote_proxy::MESH_LOGICAL_STEP
            )
        );
    }

    /// Inside a forwarded call the journal keys on the payload digest, not on a
    /// connection that does not exist on this node.
    #[tokio::test]
    async fn an_operation_of_a_forwarded_call_is_journaled_under_the_mesh_identity() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-mesh-op", AutonomyMode::Normal, AutonomyMode::Normal) else {
            return;
        };
        let scope = live.scope();
        let digest = crate::code_studio::assertion::digest_hex(b"forwarded-bytes");
        let expected = remote_proxy::remote_op_id(&live.session_id, b"forwarded-bytes");
        let ctx = live.fx.ctx.clone();
        let op_id = remote_proxy::with_remote_origin(digest, async {
            begin_op(
                &ctx,
                &scope,
                OpKind::FsMkdir,
                Capability::FsWrite,
                "fs_mkdir",
                OperationInput::None,
                operations::Precondition::None,
                Postcondition::None,
                None,
            )
        })
        .await
        .expect("journal the operation");
        assert_eq!(op_id, expected, "the assertion and the journal disagree");

        // A repeat of the same forwarded call finds the row it already opened.
        let again = remote_proxy::with_remote_origin(
            crate::code_studio::assertion::digest_hex(b"forwarded-bytes"),
            async {
                begin_op(
                    &live.fx.ctx,
                    &scope,
                    OpKind::FsMkdir,
                    Capability::FsWrite,
                    "fs_mkdir",
                    OperationInput::None,
                    operations::Precondition::None,
                    Postcondition::None,
                    None,
                )
            },
        )
        .await
        .expect("journal the operation");
        assert_eq!(again, op_id, "a replay opened a second operation");
        release();
    }

    // =========================================================================
    // Node catalog (§3)
    // =========================================================================

    #[test]
    fn a_node_entry_reports_locality_and_isolation_from_the_node_itself() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-owner", &[PERM_READ]);
        let local = local_node_info(&fx.ctx);
        assert!(local.is_local);
        assert_eq!(local.node_id, "test-node");

        // A peer's answer, not this node's: offering container mode on a node
        // that cannot deliver it is exactly the promise §3 forbids.
        let peer = node_info(&fx.ctx, "node-b".to_string(), true);
        assert!(!peer.is_local);
        assert!(peer.supports_container);
        assert_eq!(
            peer.egress_enforcement,
            EgressEnforcement::Namespace.slug()
        );
        let bare = node_info(&fx.ctx, "node-c".to_string(), false);
        assert_eq!(
            bare.egress_enforcement,
            EgressEnforcement::Unrestricted.slug()
        );
        release();
    }

    /// Without a mesh there is nothing to pair with, so the picker offers this
    /// node and nothing else — an unpaired peer is never a choice.
    #[test]
    fn the_node_catalog_offers_no_peer_without_a_mesh() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-owner", &[PERM_READ]);
        let nodes = node_catalog(&fx.ctx);
        assert_eq!(nodes.len(), 1);
        assert!(nodes[0].is_local);
        release();
    }

    // =========================================================================
    // Harness turns and revision runs (§16)
    // =========================================================================

    /// A turn now starts a run instead of refusing by name. Without a background
    /// run manager it stops at the spawn, which is the step that proves the
    /// harness path is wired.
    #[tokio::test]
    async fn a_turn_reaches_the_harness_instead_of_refusing_by_name() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-turn", AutonomyMode::Normal, AutonomyMode::Normal) else {
            return;
        };
        let err = session_message_send_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            "fix the build",
        )
        .await
        .expect_err("no run manager in a unit test");
        assert_eq!(err.code, ProtocolErrorCode::NotAvailable);
        assert!(
            !err.message.contains("cannot accept a turn"),
            "the old blanket refusal is still in the way: {}",
            err.message
        );
        release();
    }

    #[tokio::test]
    async fn an_empty_turn_is_refused_before_anything_starts() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-turn-empty", AutonomyMode::Normal, AutonomyMode::Normal) else {
            return;
        };
        let err = session_message_send_v1(&live.fx.ctx, &live.workspace_id, &live.session_id, "  ")
            .await
            .expect_err("empty turn");
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
        release();
    }

    /// `request_revision` decides the whole set. Mixing it with an acceptance
    /// would leave the set half decided while a run is already rewriting it.
    #[tokio::test]
    async fn a_revision_cannot_be_mixed_with_an_acceptance() {
        let _guard = paths::test_data_dir_guard();
        // `normal` asks before every write (§9.5) and that is covered by its own
        // test; here the write is setup, not the subject.
        let Some(live) = live("ws-revision-mix", AutonomyMode::AutoEdit, AutonomyMode::AutoEdit) else {
            return;
        };
        write_file(&live, "src/main.rs", "fn main() {}\n", None);
        // A write does not open a patch set — gate 5a does, when the agent
        // reaches for `git_commit` without an accepted one (§9.3). Asking for a
        // revision therefore starts from the review the gate opened.
        let MessageBody::CodeStudioBody(CodeStudioPayload::GitCommitResponse {
            patch_set_id: Some(patch_set_id),
            ..
        }) = git_commit_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            "add main",
            None,
        )
        .expect("review")
        else {
            panic!("unexpected response");
        };
        let scope = live.scope();
        let set = patch::load_patch_set(&scope.pool, &patch_set_id).expect("set");
        let mut decisions: Vec<PatchFileDecision> = set
            .files
            .iter()
            .map(|file| PatchFileDecision {
                patch_file_id: file.id.clone(),
                decision: "request_revision".into(),
                note: Some("needs a test".into()),
                hunks: Vec::new(),
            })
            .collect();
        decisions.push(PatchFileDecision {
            patch_file_id: set.files[0].id.clone(),
            decision: "accept".into(),
            note: None,
            hunks: Vec::new(),
        });
        let err = patch_decide_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            &patch_set_id,
            &decisions,
        )
        .await
        .expect_err("mixed decision");
        assert_eq!(err.code, ProtocolErrorCode::BadRequest);
        release();
    }

    /// A failed run has to say WHY in the answer the console reads. The reason
    /// is written to the timeline and to nowhere else, so a run list that does
    /// not read it back leaves the operator with a bare 'failed' and a database
    /// query as the only diagnosis.
    #[tokio::test]
    async fn a_failed_run_carries_its_reason_into_the_run_list() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-run-reason", AutonomyMode::Normal, AutonomyMode::Normal) else {
            return;
        };
        let scope = live.scope();
        {
            let conn = scope.pool.write().expect("pool");
            conn.execute(
                "INSERT INTO session_runs \
                   (run_id, session_id, ordinal, kind, trigger, status, started_at, finished_at) \
                 VALUES ('r-cli', ?1, 1, 'cli', 'cli_delegate', 'failed', datetime('now'), \
                         datetime('now'))",
                rusqlite::params![scope.session.id],
            )
            .expect("seed run");
        }
        events::append(
            &scope.pool,
            &scope.session.id,
            events::SessionEvent::new(
                "cli-run-finished:r-cli".to_string(),
                EventPayload::RunFinished {
                    run_id: "r-cli".to_string(),
                    status: "failed".to_string(),
                    error: Some(
                        "credential_missing: no credential for engine 'claude-code'".to_string(),
                    ),
                },
            )
            .with_run("r-cli".to_string()),
        )
        .expect("journal the end of the run");

        let MessageBody::CodeStudioBody(CodeStudioPayload::SessionRunsResponse { runs, .. }) =
            session_runs_v1(&live.fx.ctx, &live.workspace_id, &live.session_id).expect("runs")
        else {
            panic!("unexpected response");
        };
        let run = runs
            .iter()
            .find(|run| run.run_id == "r-cli")
            .expect("the seeded run");
        assert_eq!(run.status, "failed");
        assert!(
            run.note
                .as_deref()
                .is_some_and(|note| note.contains("credential_missing")),
            "the run list dropped the reason: {:?}",
            run.note
        );
        release();
    }

    /// Past the budget the loop stops and asks. It must not keep starting runs,
    /// and it must not turn the change into a rejection either.
    #[tokio::test]
    async fn an_exhausted_revision_budget_asks_instead_of_looping() {
        let _guard = paths::test_data_dir_guard();
        // `normal` asks before every write (§9.5) and that is covered by its own
        // test; here the write is setup, not the subject.
        let Some(live) = live("ws-revision-cap", AutonomyMode::AutoEdit, AutonomyMode::AutoEdit) else {
            return;
        };
        write_file(&live, "src/main.rs", "fn main() {}\n", None);
        // A write does not open a patch set — gate 5a does, when the agent
        // reaches for `git_commit` without an accepted one (§9.3). Asking for a
        // revision therefore starts from the review the gate opened.
        let MessageBody::CodeStudioBody(CodeStudioPayload::GitCommitResponse {
            patch_set_id: Some(patch_set_id),
            ..
        }) = git_commit_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            "add main",
            None,
        )
        .expect("review")
        else {
            panic!("unexpected response");
        };
        let scope = live.scope();
        {
            let conn = scope.pool.write().expect("pool");
            for i in 0..MAX_REVISION_RUNS {
                conn.execute(
                    "INSERT INTO session_runs \
                       (run_id, session_id, ordinal, kind, trigger, status, started_at) \
                     VALUES (?1, ?2, ?3, 'revision', 'review_rejected', 'completed', \
                             datetime('now'))",
                    rusqlite::params![format!("r-{i}"), scope.session.id, i + 1],
                )
                .expect("seed revision run");
            }
        }
        let set = patch::load_patch_set(&scope.pool, &patch_set_id).expect("set");
        let decisions: Vec<PatchFileDecision> = set
            .files
            .iter()
            .map(|file| PatchFileDecision {
                patch_file_id: file.id.clone(),
                decision: "request_revision".into(),
                note: Some("still wrong".into()),
                hunks: Vec::new(),
            })
            .collect();
        let err = patch_decide_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            &patch_set_id,
            &decisions,
        )
        .await
        .expect_err("the budget is spent");
        assert_eq!(err.code, ProtocolErrorCode::Conflict);
        assert!(
            err.message.starts_with("approval_required:"),
            "the budget must put a question to the operator: {}",
            err.message
        );

        // The question exists as a real row the dashboard can render. Asserted by
        // ITS OWN id rather than by counting: the review gate that produced the
        // patch set is a pending question too, and a count would pass while
        // pointing at the wrong one.
        let approval_id = err
            .message
            .trim_start_matches("approval_required:")
            .split(':')
            .next()
            .expect("the error names the approval")
            .to_string();
        let status: String = {
            let conn = scope.pool.read().expect("pool");
            conn.query_row(
                "SELECT status FROM approvals WHERE id = ?1 AND session_id = ?2",
                rusqlite::params![approval_id, scope.session.id],
                |row| row.get(0),
            )
            .expect("the budget question is a row")
        };
        assert_eq!(status, "pending");
        // And nothing was decided about the change set behind the operator's back.
        let set = patch::load_patch_set(&scope.pool, &patch_set_id).expect("set");
        assert!(
            matches!(set.status.as_str(), "open" | "in_review"),
            "the set was decided anyway: {}",
            set.status
        );
        release();
    }

    // =========================================================================
    // Semantic index (§14) and the workspace tile
    // =========================================================================

    #[test]
    fn a_workspace_without_an_index_reports_it_instead_of_failing() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-owner", &[PERM_READ]);
        seed_workspace(&fx.ctx, "ws-noindex", "u-owner", ExecMode::TrustedNative);
        activate(&fx.ctx, "ws-noindex");
        let response = index_status_v1(&fx.ctx, "ws-noindex").expect("status");
        let MessageBody::CodeStudioBody(CodeStudioPayload::IndexStatusResponse {
            index_enabled,
            branches,
            ..
        }) = response
        else {
            panic!("unexpected response");
        };
        assert!(!index_enabled);
        assert!(branches.is_empty());
        release();
    }

    /// The workspace tile counts sessions blocked on a question separately from
    /// sessions that are merely open — they are different facts (§19, K01).
    #[test]
    fn the_workspace_tile_separates_waiting_sessions_from_open_ones() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-owner", &[PERM_READ]);
        seed_user(&fx.ctx, "u-owner");
        seed_workspace(&fx.ctx, "ws-counts", "u-owner", ExecMode::TrustedNative);
        activate(&fx.ctx, "ws-counts");
        // `workspace.db` lives in the directory the provisioning saga creates;
        // opening it without one is what a never-provisioned workspace does.
        paths::create_workspace_layout("ws-counts").expect("layout");
        let record = repository::get_workspace(&fx.ctx.state.db, "ws-counts")
            .unwrap()
            .unwrap();
        let pool = workspace_db::open("ws-counts").expect("runtime db");
        {
            let conn = pool.write().expect("pool");
            for (id, status) in [("s-open", "idle"), ("s-wait", "waiting_user")] {
                conn.execute(
                    "INSERT INTO sessions \
                       (id, workspace_id, user_id, title, branch, autonomy_mode, flow_id, \
                        flow_version_id, status, created_at, updated_at) \
                     VALUES (?1, 'ws-counts', 'u-owner', ?1, 'cs/o/1', 'normal', 'cs-harness', \
                             'v1', ?2, datetime('now'), datetime('now'))",
                    rusqlite::params![id, status],
                )
                .expect("seed session");
            }
        }
        let counts = open_session_count(&record, "u-owner");
        assert_eq!(counts.open, 2);
        assert_eq!(counts.waiting_user, 1);

        let wire = workspace_to_wire(&fx.ctx, &record, Some(WorkspaceRole::Owner), 1, counts, 0);
        assert_eq!(wire.open_sessions, 2);
        assert_eq!(wire.sessions_waiting, 1);
        assert_eq!(wire.quota_sessions, record.quota_sessions);
        release();
    }

    /// The test above seeds `waiting_user` into the column, so it proves the
    /// counter READS the column — not that anything ever writes it. It did not:
    /// `events::verify_projection` was the only producer and by contract runs at
    /// coordinator start, so a session blocked on a live question still reported
    /// `idle` and the workspace tile was pinned at zero. This walks the real
    /// path instead.
    #[test]
    fn a_question_puts_its_session_in_the_waiting_state_and_a_decision_clears_it() {
        let _guard = paths::test_data_dir_guard();
        let Some(live) = live("ws-waiting", AutonomyMode::Normal, AutonomyMode::Normal) else {
            return;
        };
        let scope = live.scope();
        let status_now = || -> String {
            let conn = scope.pool.read().expect("pool");
            conn.query_row(
                "SELECT status FROM sessions WHERE id = ?1",
                rusqlite::params![scope.session.id],
                |row| row.get(0),
            )
            .expect("session row")
        };
        assert_ne!(
            status_now(),
            "waiting_user",
            "nothing has been asked of anybody yet"
        );

        // A write in `normal` mode is precisely the case §9.4 says must ask.
        let err = file_write_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            "src/main.rs",
            "fn main() {}",
            None,
        )
        .expect_err("a write in normal mode asks first");
        assert!(
            err.message.starts_with("approval_required:"),
            "expected a question, got: {}",
            err.message
        );
        let approval_id = err
            .message
            .trim_start_matches("approval_required:")
            .split(':')
            .next()
            .expect("the error names the approval")
            .to_string();

        assert_eq!(
            status_now(),
            "waiting_user",
            "a session with an open question must say it is waiting for a person"
        );
        let record = repository::get_workspace(&live.fx.ctx.state.db, &live.workspace_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            open_session_count(&record, "u-owner").waiting_user,
            1,
            "the workspace tile must count the waiting session"
        );

        approval_decide_v1(
            &live.fx.ctx,
            &live.workspace_id,
            &live.session_id,
            &approval_id,
            "allow_once",
        )
        .expect("decide");

        assert_ne!(
            status_now(),
            "waiting_user",
            "answering the last open question releases the session"
        );
        assert_eq!(open_session_count(&record, "u-owner").waiting_user, 0);
        release();
    }


    // =========================================================================
    // Registry writes and the Sync Ledger
    // =========================================================================

    /// A registry write that skips the capture is a change that never leaves the
    /// node: the user edits a workspace on the desktop and the phone keeps
    /// showing the old value. Every write below therefore goes through
    /// `code_studio::repository`, which captures in the SAME transaction.
    fn captured(ctx: &HandlerContext, resource_type: &str, resource_id: &str) -> bool {
        let conn = ctx.state.db.read().expect("db");
        conn.query_row(
            "SELECT COUNT(*) FROM __tentaflow_core_sync_captures \
             WHERE resource_type = ?1 AND resource_id = ?2",
            rusqlite::params![resource_type, resource_id],
            |row| row.get::<_, i64>(0),
        )
        .expect("capture query")
            > 0
    }

    #[test]
    fn a_standing_permission_is_captured_for_the_ledger() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-owner", &[PERM_READ]);
        seed_workspace(&fx.ctx, "ws-sync-allow", "u-owner", ExecMode::TrustedNative);
        activate(&fx.ctx, "ws-sync-allow");

        allowlist_set_v1(&fx.ctx, "ws-sync-allow", "net_egress", "crates.io*").expect("set");
        // The replication key is the (workspace, capability, pattern) triple —
        // the table's rowid is node-local and would name different rows on two
        // nodes.
        assert!(captured(
            &fx.ctx,
            "core.code_workspace_allowlist",
            &crate::sync::resource_id::composite_resource_id(&[
                "ws-sync-allow",
                "net_egress",
                "crates.io*"
            ])
        ));

        allowlist_remove_v1(&fx.ctx, "ws-sync-allow", "net_egress", "crates.io*")
            .expect("remove");
        let entries = read_allowlist(&fx.ctx.state.db, "ws-sync-allow").expect("list");
        assert!(
            entries.iter().all(|entry| entry.capability != "net_egress"),
            "the grant survived its withdrawal"
        );
        release();
    }

    #[test]
    fn a_settings_change_is_captured_for_the_ledger() {
        let _guard = paths::test_data_dir_guard();
        let fx = fixture("u-owner", &[PERM_READ, PERM_ADMIN]);
        seed_workspace(&fx.ctx, "ws-sync-settings", "u-owner", ExecMode::TrustedNative);
        activate(&fx.ctx, "ws-sync-settings");
        // Creation captures too, so the assertion has to be about THIS write.
        let before = {
            let conn = fx.ctx.state.db.read().expect("db");
            conn.query_row(
                "SELECT COUNT(*) FROM __tentaflow_core_sync_captures \
                 WHERE resource_type = 'core.code_workspace' AND resource_id = ?1",
                rusqlite::params!["ws-sync-settings"],
                |row| row.get::<_, i64>(0),
            )
            .expect("count")
        };

        workspace_settings_update_v1(
            &fx.ctx,
            "ws-sync-settings",
            "Renamed",
            "normal",
            "org_approved",
            None,
            false,
            None,
            Some(5),
        )
        .expect("settings");

        let after = {
            let conn = fx.ctx.state.db.read().expect("db");
            conn.query_row(
                "SELECT COUNT(*) FROM __tentaflow_core_sync_captures \
                 WHERE resource_type = 'core.code_workspace' AND resource_id = ?1",
                rusqlite::params!["ws-sync-settings"],
                |row| row.get::<_, i64>(0),
            )
            .expect("count")
        };
        assert!(after > before, "the settings change never left this node");

        // And the value the settings form has to read back comes with it.
        let record = repository::get_workspace(&fx.ctx.state.db, "ws-sync-settings")
            .unwrap()
            .unwrap();
        assert_eq!(record.quota_sessions, Some(5));
        release();
    }

    #[test]
    fn a_workspace_name_becomes_a_safe_slug() {
        assert_eq!(slugify("TentaFlow Core"), "tentaflow-core");
        assert_eq!(slugify("../../etc/passwd"), "etc-passwd");
        assert_eq!(slugify("   "), "workspace");
        assert!(!slugify("a..b").contains(".."));
    }
}
