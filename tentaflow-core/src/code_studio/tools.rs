// ===== File: code_studio/tools.rs — the agent's tool surface (§10).
//
// The complete verb set of a coding agent, executed in Core: filesystem,
// command execution, git through the broker, and workspace facts. Three
// invariants hold for EVERY call here and are the reason this module exists
// instead of nineteen ad-hoc handlers:
//
//   1. the session is bound SERVER-SIDE from `envelope.meta["code_session"]`
//      (minted at spawn like Project Studio's `ps_generation`), so the model
//      cannot name a workspace, a session, or someone else's worktree;
//   2. every call passes `pep::authorize` and the whole `Decision` is honoured:
//      `Allow` carries the profile the work runs in, `AskUser` SUSPENDS the
//      call behind an `approvals` row, `Deny` returns a reason;
//   3. every effect is journaled through `operations::begin` → `complete`/
//      `fail` with its pre/postcondition, and argv plus output are redacted
//      before anything is stored.
//
// `core.code_search` answers from the semantic index (§14) and is the one verb
// here whose result is explicitly NOT authoritative: grep reads the files,
// the index describes them as of its last pass, so a degraded answer carries
// the instruction to search again with `core.fs_grep`.
// =====

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde_json::{json, Value};

use crate::agents::CoreToolName;
use crate::db::DbPool;

use super::artifacts;
use super::events::{self, EventPayload, GitOperation, SessionEvent};
use super::exec::{ExecEnv, ExecRequest, Executor, NullSink, Program};
use super::fs::{self, GrepQuery, LineRange, Precondition as FsPrecondition, RelPath, SessionRoot};
use super::git_broker::{Broker, CommitIdentity, CommitSpec, GitAuth, MergeOutcome, RepoHandle};
use super::index::{CodeIndex, CodeSearchOutcome};
use super::models::{AutonomyMode, WorkspaceRecord, WorkspaceRole};
use super::operations::{
    self, OpKind, OperationInput, OperationRequest, OriginKind, Postcondition, Precondition,
};
use super::patch::{self, CommitRequest, PatchScope, PatchSet};
use super::pep::{
    self, AskKind, Capability, Decision, MountAccess as PepMountAccess,
    SandboxProfile as PepProfile,
};
use super::sandbox::{mount_slug, SandboxManager, SandboxProfile};
use super::session::{self, SessionRecord};
use super::{redact, repository, workspace_db};

/// Envelope meta key of the server-minted session binding. The value is
/// `{"workspace_id": ..., "session_id": ...}`; it is written by the session
/// coordinator at spawn and is never a tool parameter.
pub const SESSION_META_KEY: &str = "code_session";

/// Budget of one tool result handed to the model, mirroring the `tool_exec`
/// block's `max_result_chars`. Applied to the rendered JSON so an oversized
/// grep or command output cannot blow the turn.
pub const MAX_RESULT_CHARS: usize = 16_000;

/// Ceiling on one `core.exec` run when the model names no timeout.
const DEFAULT_EXEC_TIMEOUT_SECS: u64 = 300;
/// Hard ceiling on a model-chosen `core.exec` timeout.
const MAX_EXEC_TIMEOUT_SECS: u64 = 1800;
/// Default and maximum number of results a listing/search call returns.
const DEFAULT_SEARCH_LIMIT: usize = 100;
const MAX_SEARCH_LIMIT: usize = 1000;
/// Default and maximum `core.git_read` log/ls-files page size.
const DEFAULT_GIT_LIMIT: u32 = 50;
/// How long gate 5a waits for the operator's review before rejecting. A commit
/// nobody looked at is not a commit; silence therefore means "no".
const DEFAULT_REVIEW_TIMEOUT_SECS: u64 = 1800;

/// The server-minted binding, read back from the run envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBinding {
    pub workspace_id: String,
    pub session_id: String,
}

/// Extracts the binding from `envelope.meta[SESSION_META_KEY]`. Absent or
/// malformed = this run was not opened by a Code Studio session, so every tool
/// in this module refuses. The model cannot forge it: meta is server-owned.
pub fn binding_from_meta(meta: &BTreeMap<String, Value>) -> Option<SessionBinding> {
    let value = meta.get(SESSION_META_KEY)?;
    let workspace_id = value.get("workspace_id")?.as_str()?.trim();
    let session_id = value.get("session_id")?.as_str()?.trim();
    if workspace_id.is_empty() || session_id.is_empty() {
        return None;
    }
    Some(SessionBinding {
        workspace_id: workspace_id.to_string(),
        session_id: session_id.to_string(),
    })
}

/// Builds the meta value the coordinator mints at spawn. Kept here so the
/// producer and the consumer of the binding share one shape.
pub fn binding_meta_value(workspace_id: &str, session_id: &str) -> Value {
    json!({ "workspace_id": workspace_id, "session_id": session_id })
}

/// What the operator decided about one suspended call. Mirrors the `approvals`
/// table's `decision` column. `allow_for_session` is absent on purpose: the
/// permission card the dashboard renders carries four outcomes, and inventing a
/// fifth here would produce a decision no operator can actually give. The PEP
/// still honours `session_grants` written by other paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    AllowOnce,
    AllowForRun,
    Always,
    Deny,
}

impl ApprovalDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            ApprovalDecision::AllowOnce => "allow_once",
            ApprovalDecision::AllowForRun => "allow_for_run",
            ApprovalDecision::Always => "always",
            ApprovalDecision::Deny => "deny",
        }
    }

    pub fn allows(self) -> bool {
        !matches!(self, ApprovalDecision::Deny)
    }
}

/// One suspended call put to the operator.
#[derive(Debug, Clone)]
pub struct Approval {
    pub interaction_id: String,
    pub capability: Capability,
    pub summary: String,
    pub kind: AskKind,
}

/// A change set put to the operator for review.
#[derive(Debug, Clone)]
pub struct ReviewPrompt {
    pub patch_set_id: String,
    /// Human-readable file/hunk listing with the diff, already redacted.
    pub detail: String,
    /// `file` or `hunk` — how finely the answer may decide.
    pub granularity: String,
    pub timeout: Duration,
}

/// How this module reaches the human. The flow layer owns the interaction
/// registry and the progress stream, so it supplies the gate; `tools.rs` stays
/// free of `flow_engine` types and testable with a scripted gate.
#[async_trait]
pub trait ApprovalGate: Send + Sync {
    /// Puts one capability decision to the operator.
    async fn request(&self, ask: &Approval) -> ApprovalDecision;

    /// Presents a change set and returns the operator's raw answer, or `None`
    /// when nobody answered inside the budget.
    async fn present_review(&self, prompt: &ReviewPrompt) -> Option<String>;
}

/// Everything one tool call needs beyond its arguments.
pub struct ToolCallCtx<'a> {
    /// Main (registry) database — workspaces, members, allowlist.
    pub main_db: &'a DbPool,
    /// Principal the run acts for. A run without a user identity cannot touch
    /// a workspace: membership is what authorizes the call.
    pub user_id: &'a str,
    /// Agent run id, used as the operation's run scope and the event's run.
    pub run_id: Option<&'a str>,
    /// The model's tool_call id — the operation's `origin_id`, which makes the
    /// journal entry stable across a retry of the same call.
    pub tool_call_id: &'a str,
    pub binding: &'a SessionBinding,
    pub gate: &'a dyn ApprovalGate,
}

/// The resolved session a call runs against.
///
/// Public because the Code Studio flow blocks (`exec_command`, `delegate_cli`)
/// authorize against the SAME resolved session as a model-issued tool call. A
/// second resolution path would be a second reading of membership, autonomy and
/// the workspace ceiling — the two would drift, and the drift would be a
/// permission bug nobody could see in either file.
pub struct Bound {
    pub workspace: WorkspaceRecord,
    pub session: SessionRecord,
    pub role: WorkspaceRole,
    pub autonomy: AutonomyMode,
    pub pool: DbPool,
    pub broker: Broker,
}

/// Capability each verb is authorized under (§10, column "Capability").
pub fn capability_of(tool: CoreToolName) -> Option<Capability> {
    Some(match tool {
        // §9.2 lists `code_search` next to the read verbs, granted to every
        // role including viewer; it carries its own capability because its
        // bounds (§10: prefix, limit) are not a file path's bounds.
        CoreToolName::FsRead
        | CoreToolName::FsList
        | CoreToolName::FsGlob
        | CoreToolName::FsGrep
        | CoreToolName::WorkspaceInfo => Capability::FsRead,
        CoreToolName::CodeSearch => Capability::CodeSearch,
        CoreToolName::FsWrite | CoreToolName::FsEdit | CoreToolName::FsMkdir => Capability::FsWrite,
        // A move destroys the source path just as a delete does, so it is
        // authorized as a delete rather than as a plain write.
        CoreToolName::FsMove | CoreToolName::FsDelete => Capability::FsDelete,
        CoreToolName::Exec => Capability::Exec,
        CoreToolName::GitRead => Capability::GitRead,
        CoreToolName::GitBranch => Capability::GitBranch,
        CoreToolName::GitSync => Capability::GitNetwork,
        CoreToolName::GitStage => Capability::GitStage,
        CoreToolName::GitCommit => Capability::GitCommit,
        CoreToolName::GitPush => Capability::GitPush,
        CoreToolName::GitMerge => Capability::GitMerge,
        CoreToolName::GitMergeFinalize => Capability::GitMergeFinalize,
        _ => return None,
    })
}

/// Operation kind journaled for the effectful verbs. A read leaves no effect
/// and therefore no operation row — the tool_call/tool_result events already
/// record that it happened.
fn op_kind_of(tool: CoreToolName) -> Option<OpKind> {
    Some(match tool {
        CoreToolName::FsWrite => OpKind::FsWrite,
        CoreToolName::FsEdit => OpKind::FsEdit,
        CoreToolName::FsMkdir => OpKind::FsMkdir,
        CoreToolName::FsMove | CoreToolName::FsDelete => OpKind::FsDelete,
        CoreToolName::Exec => OpKind::Exec,
        CoreToolName::GitBranch | CoreToolName::GitSync | CoreToolName::GitStage => {
            OpKind::GitBranch
        }
        CoreToolName::GitCommit => OpKind::GitCommit,
        CoreToolName::GitPush => OpKind::GitPush,
        CoreToolName::GitMerge => OpKind::GitMerge,
        CoreToolName::GitMergeFinalize => OpKind::GitMergeFinalize,
        _ => return None,
    })
}

// =============================================================================
// Entry point
// =============================================================================

/// Executes one Code Studio builtin and returns the JSON handed to the model.
///
/// An `Err` here is a recoverable `[TOOL_ERROR]`, never an aborted run: a denial,
/// a refused path and a failed command all read the same way to the model — a
/// message it can act on.
pub async fn execute(ctx: &ToolCallCtx<'_>, tool: CoreToolName, args: &Value) -> Result<Value> {
    let bound = bind(ctx).await?;
    let capability = capability_of(tool)
        .ok_or_else(|| anyhow!("'{}' is not a Code Studio tool", tool.public_name()))?;

    // The authorization target: what the PEP is being asked about. It is derived
    // from the arguments here, ONCE, and reused for the grant-pattern match, so
    // a grant can never be earned for one path and spent on another.
    let target_label = target_label(tool, args);
    let target = match pep_target(&bound, tool, args).await {
        Ok(target) => target,
        // A target that cannot be resolved is a refusal, not a crash — a git
        // remote the address policy will not dial is the case that matters
        // (§11.4), and it belongs on the session timeline rather than only in
        // the model's transcript.
        Err(e) => {
            let reason = redact::redact_text(&format!("{e:#}"));
            emit_tool_event(&bound, ctx, false, &reason);
            return Err(anyhow!("[TOOL_ERROR] {reason}"));
        }
    };

    let session_ctx = session_ctx_for(ctx.main_db, &bound, capability, target_label.as_deref())?;

    let profile = match pep::authorize(&session_ctx, capability, &target) {
        Decision::Allow(profile) => profile,
        Decision::Deny { reason } => {
            emit_tool_event(&bound, ctx, false, &reason);
            return Err(anyhow!("[TOOL_ERROR] {reason}"));
        }
        // Gate 5a: a commit without an accepted set is not a violation, it is a
        // missing human decision — so the review opens HERE, through the very
        // implementation the `patch_review` block uses, and the call resumes.
        Decision::AskUser {
            kind: AskKind::PatchReview,
            ..
        } => {
            let report = run_review(
                &bound.pool,
                &bound.workspace.id,
                &bound.session.id,
                &PatchScope::Work,
                "hunk",
                ctx.user_id,
                ctx.gate,
                Duration::from_secs(DEFAULT_REVIEW_TIMEOUT_SECS),
                ReviewTimeout::Reject,
            )
            .await?;
            if !report.accepted_anything() {
                let reason = format!(
                    "the change was not accepted (review {}); nothing was committed",
                    report.status
                );
                emit_tool_event(&bound, ctx, false, &reason);
                return Err(anyhow!("[TOOL_ERROR] {reason}"));
            }
            match pep::authorize_after_decision(
                &pep::SessionCtx {
                    has_accepted_patch_set: true,
                    ..session_ctx
                },
                capability,
                &target,
            ) {
                Decision::Allow(profile) => profile,
                Decision::Deny { reason }
                | Decision::AskUser {
                    summary: reason, ..
                } => {
                    emit_tool_event(&bound, ctx, false, &reason);
                    return Err(anyhow!("[TOOL_ERROR] {reason}"));
                }
            }
        }
        Decision::AskUser { summary, kind } => {
            let decision = suspend_for_operator(
                ctx,
                &bound,
                capability,
                target_label.as_deref(),
                &summary,
                kind,
            )
            .await?;
            if !decision.allows() {
                let reason = format!("the operator refused '{}'", capability.slug());
                emit_tool_event(&bound, ctx, false, &reason);
                return Err(anyhow!("[TOOL_ERROR] {reason}"));
            }
            persist_grant(ctx, &bound, capability, target_label.as_deref(), decision)?;
            // The decision replaces steps 6-10 of §9.3, not the profile choice:
            // re-running the PEP with the grant recorded yields the profile the
            // work must execute in. `git_commit` re-enters gate 5a, which is now
            // satisfied by the accepted patch set the review produced, and a
            // mandatory-interactive capability resolves to its profile instead
            // of asking the very question the operator just answered.
            match pep::authorize_after_decision(
                &pep::SessionCtx {
                    allowlisted: true,
                    has_accepted_patch_set: patch::has_accepted_patch_set(
                        &bound.pool,
                        &bound.session.id,
                        &PatchScope::Work,
                    )?,
                    ..session_ctx
                },
                capability,
                &target,
            ) {
                Decision::Allow(profile) => profile,
                Decision::Deny { reason }
                | Decision::AskUser {
                    summary: reason, ..
                } => {
                    emit_tool_event(&bound, ctx, false, &reason);
                    return Err(anyhow!("[TOOL_ERROR] {reason}"));
                }
            }
        }
    };

    let outcome = run_tool(ctx, &bound, tool, args, profile).await;
    match &outcome {
        Ok(value) => emit_tool_event(&bound, ctx, true, &summarize(value)),
        Err(e) => emit_tool_event(&bound, ctx, false, &redact::redact_text(&e.to_string())),
    }
    outcome.map(|v| bound_result(v))
}

/// Everything the PEP needs to know about this session for ONE capability.
///
/// The only producer of a `SessionCtx` on the agent path. `exec_command` and
/// `delegate_cli` call it too, so "what standing permissions does this session
/// hold" is answered once, from the same tables, whether the caller is a model,
/// a graph block or a vendor CLI.
pub fn session_ctx_for(
    main_db: &DbPool,
    bound: &Bound,
    capability: Capability,
    target_label: Option<&str>,
) -> Result<pep::SessionCtx> {
    Ok(pep::SessionCtx {
        role: bound.role,
        autonomy: bound.autonomy,
        // Agents are never the coordinator: `git_worktree` stays a system
        // capability no matter which agent asks (§9.2).
        is_coordinator: false,
        // Gate 5a is the COMMIT gate, and a commit publishes the session
        // branch, so the decision it looks for is the work review's.
        has_accepted_patch_set: patch::has_accepted_patch_set(
            &bound.pool,
            &bound.session.id,
            &PatchScope::Work,
        )?,
        allowlisted: allowlist_holds(main_db, &bound.workspace.id, capability, target_label)?,
        session_granted: session_grant_holds(
            &bound.pool,
            &bound.session.id,
            capability,
            target_label,
        )?,
        // Per-run grants live in the flow layer's interaction registry; the gate
        // consults them when it raises the card, so the PEP sees `false` here
        // and the ask is short-circuited by the gate rather than by this state.
        run_granted: false,
    })
}

/// Resolves the binding into a live workspace + session, refusing anything the
/// principal is not a member of. Non-membership and a missing workspace answer
/// identically, so a run cannot probe which workspaces exist.
pub async fn bind(ctx: &ToolCallCtx<'_>) -> Result<Bound> {
    let binding = ctx.binding.clone();
    let user_id = ctx.user_id.to_string();
    let db = ctx.main_db.clone();
    tokio::task::spawn_blocking(move || -> Result<Bound> { bind_blocking(&db, &user_id, &binding) })
        .await
        .map_err(|e| anyhow!("session bind task failed: {e}"))?
}

fn bind_blocking(main_db: &DbPool, user_id: &str, binding: &SessionBinding) -> Result<Bound> {
    if user_id.is_empty() {
        return Err(anyhow!(
            "[TOOL_ERROR] this run has no user identity, so it cannot act on a workspace"
        ));
    }
    let not_bound = || anyhow!("[TOOL_ERROR] this run is not bound to an open Code Studio session");

    let workspace =
        repository::get_workspace(main_db, &binding.workspace_id)?.ok_or_else(not_bound)?;
    let role = repository::role_of(main_db, &workspace.id, user_id)?.ok_or_else(not_bound)?;
    if workspace.status != "active" {
        return Err(anyhow!(
            "[TOOL_ERROR] workspace '{}' is {} and accepts no tool calls",
            workspace.name,
            workspace.status
        ));
    }

    let pool = workspace_db::open(&workspace.id)?;
    let session = session::get_session(&pool, &binding.session_id)?.ok_or_else(not_bound)?;
    if session.user_id != user_id {
        return Err(not_bound());
    }
    if matches!(session.status.as_str(), "closed" | "closing") {
        return Err(anyhow!(
            "[TOOL_ERROR] this session is closed; open a new one to keep working"
        ));
    }
    let session_autonomy = AutonomyMode::from_slug(&session.autonomy_mode).ok_or_else(|| {
        anyhow!(
            "session carries an unknown autonomy mode '{}'",
            session.autonomy_mode
        )
    })?;
    let ceiling = AutonomyMode::from_slug(&workspace.autonomy_ceiling).ok_or_else(|| {
        anyhow!(
            "workspace carries an unknown autonomy ceiling '{}'",
            workspace.autonomy_ceiling
        )
    })?;
    // §9.5 — the session mode never exceeds the workspace ceiling, and the
    // ceiling is read at every call rather than at session open: lowering it is
    // how an owner stops work that is already running.
    let autonomy = session_autonomy.min(ceiling);
    let broker = Broker::for_workspace(&workspace.id)?;

    Ok(Bound {
        workspace,
        session,
        role,
        autonomy,
        pool,
        broker,
    })
}

// =============================================================================
// Authorization helpers
// =============================================================================

/// The concrete thing a grant pattern is matched against: a repository path, a
/// program name, a branch. `None` for verbs whose target is the session itself.
///
/// An EMPTY argument is `None` too, not `Some("")`: the worktree root and an
/// absent prefix both mean "the whole repository", which is no narrower a
/// target than naming none at all, and `pep::pattern_matches` has exactly one
/// reading for that case.
fn target_label(tool: CoreToolName, args: &Value) -> Option<String> {
    match tool {
        CoreToolName::FsRead
        | CoreToolName::FsList
        | CoreToolName::FsGlob
        | CoreToolName::FsGrep
        | CoreToolName::FsWrite
        | CoreToolName::FsEdit
        | CoreToolName::FsDelete
        | CoreToolName::FsMkdir => optional_str(args, "path").map(str::to_string),
        CoreToolName::CodeSearch => optional_str(args, "prefix").map(str::to_string),
        CoreToolName::FsMove => optional_str(args, "to").map(str::to_string),
        CoreToolName::Exec => args
            .get("argv")
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .and_then(|v| v.as_str())
            .filter(|name| !name.is_empty())
            .map(str::to_string),
        CoreToolName::GitMerge => optional_str(args, "target_branch").map(str::to_string),
        _ => None,
    }
}

/// Builds the PEP target. The containment question is answered by `RelPath`
/// parsing, which is the same guard the filesystem layer applies — a path that
/// cannot be parsed relative to the worktree is outside it by construction.
async fn pep_target(bound: &Bound, tool: CoreToolName, args: &Value) -> Result<pep::Target> {
    // §11.4 — a remote is judged by its ADDRESS, and the address the model
    // named is the one the broker will really dial, so it is resolved here the
    // same way `push_branch` resolves it. Without this the PEP saw a branch,
    // rule 5b never fired on the agent path, and a standing `git_network:*`
    // grant covered a LAN target the operator was supposed to be asked about.
    if matches!(tool, CoreToolName::GitPush | CoreToolName::GitSync) {
        return remote_pep_target(bound, args).await;
    }
    Ok(match tool {
        CoreToolName::FsRead
        | CoreToolName::FsList
        | CoreToolName::FsGlob
        | CoreToolName::FsGrep
        | CoreToolName::FsWrite
        | CoreToolName::FsEdit
        | CoreToolName::FsDelete
        | CoreToolName::FsMkdir
        | CoreToolName::FsMove
        | CoreToolName::CodeSearch => {
            let inside = match tool {
                CoreToolName::FsMove => parses_inside(args, "from") && parses_inside(args, "to"),
                CoreToolName::FsGlob | CoreToolName::FsGrep => {
                    args.get("path").is_none() || parses_inside(args, "path")
                }
                // A prefix outside the tree can only ever match nothing, but it
                // is still a path the model named: it is refused where every
                // other path argument is refused, not silently ignored.
                CoreToolName::CodeSearch => {
                    optional_str(args, "prefix").is_none() || parses_inside(args, "prefix")
                }
                CoreToolName::FsList => args.get("path").is_none() || parses_inside(args, "path"),
                _ => parses_inside(args, "path"),
            };
            pep::Target::Path {
                inside_worktree: inside,
            }
        }
        CoreToolName::GitMerge | CoreToolName::GitMergeFinalize => pep::Target::Branch {
            // A merge deliberately targets a branch that is NOT the session's;
            // `mandatory_interactive` covers it, and the broker keeps the target
            // ref untouched until finalize.
            is_session_branch: false,
        },
        CoreToolName::GitBranch => pep::Target::Branch {
            is_session_branch: str_arg(args, "name")
                .map(|n| n == bound.session.branch)
                .unwrap_or(true),
        },
        // The program is the target of an `exec`, and it is the same string
        // every allowlist entry and grant pattern is written against — handing
        // the PEP `None` here would leave the argv unbounded by anything.
        CoreToolName::Exec => pep::Target::Program {
            name: target_label(CoreToolName::Exec, args).unwrap_or_default(),
        },
        _ => pep::Target::None,
    })
}

/// The remote of a push or a sync, resolved and put through `remote_policy`
/// before the PEP is asked about it.
///
/// The model may name a remote NAME or a whole url, so both go through
/// `Broker::resolve_remote` — the same call `push_branch` and `fetch_branch`
/// make, and therefore the same rules rather than a second copy of them. A
/// forbidden address fails here, before any grant is consulted, and a private
/// one comes back flagged so rule 5b can raise the threshold.
async fn remote_pep_target(bound: &Bound, args: &Value) -> Result<pep::Target> {
    let remote = optional_str(args, "remote").unwrap_or("origin").to_string();
    let workspace_id = bound.workspace.id.clone();
    let session_id = bound.session.id.clone();
    // Resolving a name runs `git config` and validating an address resolves
    // DNS; neither belongs on the async runtime's thread.
    let is_private = tokio::task::spawn_blocking(move || -> Result<bool> {
        let broker = Broker::for_workspace(&workspace_id)?;
        let handle = broker.session(&session_id)?;
        Ok(broker.resolve_remote(&handle, &remote)?.is_private)
    })
    .await
    .map_err(|e| anyhow!("remote policy task failed: {e}"))??;
    Ok(pep::Target::Remote { is_private })
}

fn parses_inside(args: &Value, key: &str) -> bool {
    str_arg(args, key)
        .map(|p| RelPath::parse(p).is_ok())
        .unwrap_or(false)
}

/// `always` grants recorded per workspace (§9.1), read the one way
/// `pep::pattern_matches` defines. The dashboard path calls this too: a
/// standing permission that meant one thing to the operator's own calls and
/// another to the model's would be two permissions wearing one row.
pub(crate) fn allowlist_holds(
    main_db: &DbPool,
    workspace_id: &str,
    capability: Capability,
    target: Option<&str>,
) -> Result<bool> {
    let conn = main_db
        .read()
        .map_err(|e| anyhow!("registry db read: {e}"))?;
    let mut stmt = conn.prepare_cached(
        "SELECT pattern FROM code_workspace_allowlist WHERE workspace_id = ?1 AND capability = ?2",
    )?;
    let patterns: Vec<String> = stmt
        .query_map(rusqlite::params![workspace_id, capability.slug()], |row| {
            row.get(0)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(patterns
        .iter()
        .any(|p| pep::pattern_matches(p, target)))
}

/// `allow_for_session` grants (§9.1), stored in the workspace runtime db.
pub(crate) fn session_grant_holds(
    pool: &DbPool,
    session_id: &str,
    capability: Capability,
    target: Option<&str>,
) -> Result<bool> {
    let conn = pool.read().map_err(|e| anyhow!("workspace db read: {e}"))?;
    let mut stmt = conn.prepare_cached(
        "SELECT pattern FROM session_grants WHERE session_id = ?1 AND capability = ?2",
    )?;
    let patterns: Vec<String> = stmt
        .query_map(rusqlite::params![session_id, capability.slug()], |row| {
            row.get(0)
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(patterns
        .iter()
        .any(|p| pep::pattern_matches(p, target)))
}

/// Suspends the call: writes the `approvals` row, puts the question to the
/// operator, records the decision back on the same row.
pub async fn suspend_for_operator(
    ctx: &ToolCallCtx<'_>,
    bound: &Bound,
    capability: Capability,
    target: Option<&str>,
    summary: &str,
    kind: AskKind,
) -> Result<ApprovalDecision> {
    let approval_id = uuid::Uuid::new_v4().to_string();
    let interaction_id = uuid::Uuid::new_v4().to_string();
    let summary = redact::redact_text(summary);

    record_approval_request(
        &bound.pool,
        &approval_id,
        &bound.session.id,
        ctx.run_id,
        &interaction_id,
        capability,
        target,
        &summary,
    )?;
    let _ = events::append(
        &bound.pool,
        &bound.session.id,
        SessionEvent::new(
            format!("approval-req:{approval_id}"),
            EventPayload::ApprovalRequested {
                approval_id: approval_id.clone(),
                capability: capability.slug().to_string(),
                summary: summary.clone(),
            },
        ),
    );

    let decision = ctx
        .gate
        .request(&Approval {
            interaction_id,
            capability,
            summary,
            kind,
        })
        .await;

    record_approval_decision(&bound.pool, &approval_id, decision, ctx.user_id)?;
    let _ = events::append(
        &bound.pool,
        &bound.session.id,
        SessionEvent::new(
            format!("approval-dec:{approval_id}"),
            EventPayload::ApprovalDecided {
                approval_id,
                decision: decision.as_str().to_string(),
                decided_by: ctx.user_id.to_string(),
            },
        ),
    );
    Ok(decision)
}

/// Writes the pending question. The TARGET is stored twice on purpose: as the
/// pattern a grant would be written from (§9.1 — the object of a permission is
/// capability + target) and as the digest that recognizes the same question
/// again. The digest comes from the PEP, so both writers of this table produce
/// the same value for the same question.
fn record_approval_request(
    pool: &DbPool,
    approval_id: &str,
    session_id: &str,
    run_id: Option<&str>,
    interaction_id: &str,
    capability: Capability,
    target: Option<&str>,
    summary: &str,
) -> Result<()> {
    let pattern = pep::grant_pattern(target);
    pep::validate_grant_pattern(pattern).map_err(|e| anyhow!("{e}"))?;
    let conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    conn.execute(
        "INSERT INTO approvals \
            (id, session_id, run_id, interaction_id, capability, target_digest, target_pattern, \
             summary, detail_ref, status, requested_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, 'pending', ?9)",
        rusqlite::params![
            approval_id,
            session_id,
            run_id,
            interaction_id,
            capability.slug(),
            pep::target_digest(capability, pattern),
            pattern,
            summary,
            chrono::Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn record_approval_decision(
    pool: &DbPool,
    approval_id: &str,
    decision: ApprovalDecision,
    decided_by: &str,
) -> Result<()> {
    let conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    conn.execute(
        "UPDATE approvals SET status = 'decided', decision = ?2, decided_at = ?3, \
         decided_by = ?4 WHERE id = ?1 AND status = 'pending'",
        rusqlite::params![
            approval_id,
            decision.as_str(),
            chrono::Utc::now().to_rfc3339(),
            decided_by,
        ],
    )?;
    Ok(())
}

/// Persists a standing grant when the operator chose one. `mandatory_interactive`
/// capabilities refuse a standing grant at the point of WRITING it (§9.3 step 5),
/// so an `always` on `git_push` cannot be stored even if the card offered it.
pub fn persist_grant(
    ctx: &ToolCallCtx<'_>,
    bound: &Bound,
    capability: Capability,
    target: Option<&str>,
    decision: ApprovalDecision,
) -> Result<()> {
    let pattern = pep::grant_pattern(target);
    match decision {
        ApprovalDecision::Always if pep::may_store_always_grant(capability) => {
            repository::add_allowlist_entry(
                ctx.main_db,
                &bound.workspace.id,
                capability.slug(),
                pattern,
                ctx.user_id,
            )?;
        }
        _ => {}
    }
    Ok(())
}

// =============================================================================
// Dispatch
// =============================================================================

async fn run_tool(
    ctx: &ToolCallCtx<'_>,
    bound: &Bound,
    tool: CoreToolName,
    args: &Value,
    profile: PepProfile,
) -> Result<Value> {
    match tool {
        CoreToolName::WorkspaceInfo => workspace_info(bound),
        CoreToolName::FsRead
        | CoreToolName::FsList
        | CoreToolName::FsGlob
        | CoreToolName::FsGrep => fs_read_call(bound, tool, args).await,
        CoreToolName::CodeSearch => code_search_call(ctx, bound, args).await,
        CoreToolName::FsWrite
        | CoreToolName::FsEdit
        | CoreToolName::FsMove
        | CoreToolName::FsDelete
        | CoreToolName::FsMkdir => fs_write_call(ctx, bound, tool, args).await,
        CoreToolName::Exec => exec_call(ctx, bound, args, profile).await,
        CoreToolName::GitRead => git_read_call(bound, args).await,
        CoreToolName::GitBranch => git_branch_call(bound).await,
        CoreToolName::GitSync => git_sync_call(ctx, bound, args).await,
        CoreToolName::GitStage => git_stage_call(bound, args).await,
        CoreToolName::GitCommit => git_commit_call(ctx, bound, args).await,
        CoreToolName::GitPush => git_push_call(ctx, bound, args).await,
        CoreToolName::GitMerge => git_merge_call(ctx, bound, args).await,
        CoreToolName::GitMergeFinalize => git_merge_finalize_call(ctx, bound, args).await,
        other => Err(anyhow!(
            "'{}' is not a Code Studio tool",
            other.public_name()
        )),
    }
}

fn workspace_info(bound: &Bound) -> Result<Value> {
    let dirty = bound
        .broker
        .status(&bound.session.id)
        .map(|entries| !entries.is_empty())
        .unwrap_or(false);
    let head = bound
        .broker
        .session(&bound.session.id)
        .and_then(|h| bound.broker.head_commit(&h))
        .ok();
    Ok(json!({
        "workspace": bound.workspace.name,
        "repo_kind": bound.workspace.repo_kind,
        "default_branch": bound.workspace.default_branch,
        "target_branch": bound.workspace.target_branch,
        "branch": bound.session.branch,
        "head_commit": head,
        "dirty": dirty,
        "autonomy_mode": bound.session.autonomy_mode,
        "your_role": bound.role.slug(),
        "exec_mode": bound.workspace.exec_mode,
        "network": bound.workspace.egress_policy,
        "semantic_index": "available through core.code_search; core.fs_grep stays authoritative",
    }))
}

// --- semantic index (§14) ---------------------------------------------------

/// Default and maximum number of chunks one semantic search returns. Far lower
/// than the grep limits on purpose: a hit carries a code snippet, so a page of
/// them costs the turn far more than a page of matching lines.
const DEFAULT_CODE_SEARCH_LIMIT: usize = 12;
const MAX_CODE_SEARCH_LIMIT: usize = 50;

/// What a degraded answer obliges the model to do. §14 keeps grep
/// authoritative, so an index that does not describe the current head produces
/// leads, never conclusions — and no hits at all proves nothing whatsoever.
const CODE_SEARCH_FALLBACK: &str =
    "the semantic index does not describe this repository as it is now; treat these hits as \
     leads and repeat the search with core.fs_grep, which reads the files";

async fn code_search_call(ctx: &ToolCallCtx<'_>, bound: &Bound, args: &Value) -> Result<Value> {
    let query = require_str(args, "query")?;
    let limit = clamp_limit(args, DEFAULT_CODE_SEARCH_LIMIT, MAX_CODE_SEARCH_LIMIT);
    let prefix = optional_str(args, "prefix").unwrap_or_default();
    // The embedder behind the index is the process-wide model router, which
    // only a booted server owns. Without one there is no index to consult —
    // which is a degraded answer, not a failure: the model still has grep.
    let Some(state) = super::remote_proxy::node_state() else {
        return Ok(code_search_result(&index_unwired_outcome()));
    };
    let index = CodeIndex::for_workspace(ctx.main_db, &bound.workspace, state.router.clone())?;
    let outcome = index.search(query, limit, prefix).await?;
    Ok(code_search_result(&outcome))
}

/// The answer when this node carries no router behind the index.
fn index_unwired_outcome() -> CodeSearchOutcome {
    CodeSearchOutcome {
        hits: Vec::new(),
        degraded: true,
        reason: Some("index_unavailable_on_this_node".to_string()),
    }
}

/// Renders one search outcome for the model. `degraded` never travels alone:
/// it arrives with the fallback instruction, because a flag the model has to
/// interpret on its own is a flag it will read as "slightly worse hits".
fn code_search_result(outcome: &CodeSearchOutcome) -> Value {
    let mut value = json!({
        "hits": outcome
            .hits
            .iter()
            .map(|hit| json!({
                "path": hit.path,
                "start_line": hit.start_line,
                "end_line": hit.end_line,
                "score": hit.score,
                "snippet": hit.snippet,
                "lang": hit.lang,
                "commit": hit.commit,
                "branch": hit.branch,
            }))
            .collect::<Vec<_>>(),
        "degraded": outcome.degraded,
        "authoritative_tool": "core.fs_grep",
    });
    if let Some(reason) = &outcome.reason {
        value["reason"] = json!(reason);
    }
    if outcome.degraded {
        value["fallback"] = json!(CODE_SEARCH_FALLBACK);
    }
    value
}

// --- filesystem -------------------------------------------------------------

async fn fs_read_call(bound: &Bound, tool: CoreToolName, args: &Value) -> Result<Value> {
    let workspace_id = bound.workspace.id.clone();
    let session_id = bound.session.id.clone();
    let args = args.clone();
    tokio::task::spawn_blocking(move || -> Result<Value> {
        let root = SessionRoot::open_session(&workspace_id, &session_id)
            .map_err(|e| anyhow!("{}", fs_error_text(&e)))?;
        match tool {
            CoreToolName::FsRead => {
                let path = rel_path(&args, "path")?;
                let range = match (
                    args.get("offset").and_then(|v| v.as_u64()),
                    args.get("limit").and_then(|v| v.as_u64()),
                ) {
                    (None, None) => None,
                    (offset, limit) => Some(LineRange {
                        start: offset.unwrap_or(1).max(1),
                        count: limit.unwrap_or(u64::MAX),
                    }),
                };
                let slice = root
                    .read(&path, range)
                    .map_err(|e| anyhow!("{}", fs_error_text(&e)))?;
                Ok(json!({
                    "path": path.as_str(),
                    "content": slice.content,
                    "sha256": slice.blob_sha,
                    "total_lines": slice.total_lines,
                    "truncated": slice.truncated,
                }))
            }
            CoreToolName::FsList => {
                let path = optional_rel_path(&args, "path")?;
                let entries = root
                    .list(&path, 1)
                    .map_err(|e| anyhow!("{}", fs_error_text(&e)))?;
                Ok(json!({
                    "path": path.as_str(),
                    "entries": entries
                        .iter()
                        .map(|e| json!({
                            "path": e.path,
                            "kind": if e.is_symlink { "symlink" }
                                    else if e.is_dir { "dir" } else { "file" },
                            "size": e.size,
                        }))
                        .collect::<Vec<_>>(),
                }))
            }
            CoreToolName::FsGlob => {
                let pattern = require_str(&args, "pattern")?;
                let scoped = match optional_str(&args, "path") {
                    Some(prefix) => format!("{}/{pattern}", prefix.trim_end_matches('/')),
                    None => pattern.to_string(),
                };
                let limit = clamp_limit(&args, DEFAULT_SEARCH_LIMIT, MAX_SEARCH_LIMIT);
                let paths = root
                    .glob(&scoped, limit)
                    .map_err(|e| anyhow!("{}", fs_error_text(&e)))?;
                Ok(json!({
                    "pattern": scoped,
                    "paths": paths.iter().map(|p| p.as_str()).collect::<Vec<_>>(),
                    "truncated": paths.len() >= limit,
                }))
            }
            CoreToolName::FsGrep => {
                let pattern = require_str(&args, "pattern")?;
                let case_insensitive = args
                    .get("case_insensitive")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                // `(?i)` is the regex crate's inline flag; building it here keeps
                // one query shape instead of a second search entry point.
                let effective = if case_insensitive {
                    format!("(?i){pattern}")
                } else {
                    pattern.to_string()
                };
                let limit = clamp_limit(&args, DEFAULT_SEARCH_LIMIT, MAX_SEARCH_LIMIT);
                let glob = match (optional_str(&args, "path"), optional_str(&args, "glob")) {
                    (Some(prefix), Some(g)) => {
                        Some(format!("{}/{g}", prefix.trim_end_matches('/')))
                    }
                    (Some(prefix), None) => Some(format!("{}/**", prefix.trim_end_matches('/'))),
                    (None, g) => g.map(str::to_string),
                };
                let result = root
                    .grep(&GrepQuery {
                        pattern: effective,
                        is_regex: true,
                        glob,
                        max_results: limit,
                        max_bytes_per_file: root.limits().max_read_bytes,
                    })
                    .map_err(|e| anyhow!("{}", fs_error_text(&e)))?;
                Ok(json!({
                    "matches": result
                        .hits
                        .iter()
                        .map(|h| json!({
                            "path": h.path,
                            "line": h.line,
                            "column": h.column,
                            "text": h.text,
                        }))
                        .collect::<Vec<_>>(),
                    "files_scanned": result.files_scanned,
                    "truncated": result.truncated,
                }))
            }
            other => Err(anyhow!("'{}' is not a read call", other.public_name())),
        }
    })
    .await
    .map_err(|e| anyhow!("filesystem read task failed: {e}"))?
}

async fn fs_write_call(
    ctx: &ToolCallCtx<'_>,
    bound: &Bound,
    tool: CoreToolName,
    args: &Value,
) -> Result<Value> {
    let (precondition, postcondition, input) = fs_conditions(tool, args)?;
    let op = begin_operation(ctx, bound, tool, input, precondition, postcondition, None)?;

    let workspace_id = bound.workspace.id.clone();
    let session_id = bound.session.id.clone();
    let owned_args = args.clone();
    let effect = tokio::task::spawn_blocking(move || -> Result<WriteEffect> {
        let root = SessionRoot::open_session(&workspace_id, &session_id)
            .map_err(|e| anyhow!("{}", fs_error_text(&e)))?;
        let expect = expected_precondition(&owned_args);
        match tool {
            CoreToolName::FsWrite => {
                let path = rel_path(&owned_args, "path")?;
                let content = require_str(&owned_args, "content")?.to_string();
                // Read BEFORE the write: the review journal records the
                // transition, and only the state the path was really in makes a
                // concurrent edit visible as a conflict.
                let before = prior_state(&root, &path)?;
                let outcome = root
                    .write(&path, content.as_bytes(), expect)
                    .map_err(|e| anyhow!("{}", fs_error_text(&e)))?;
                Ok(WriteEffect {
                    value: json!({
                        "path": path.as_str(),
                        "sha256": outcome.blob_sha,
                        "bytes": outcome.bytes,
                        "created": outcome.created,
                    }),
                    edit: before.map(|expect| RecordedEdit {
                        path: path.as_str().to_string(),
                        kind: if outcome.created {
                            patch::EditKind::Create
                        } else {
                            patch::EditKind::Write
                        },
                        content: Some(content.into_bytes()),
                        expect,
                    }),
                })
            }
            CoreToolName::FsEdit => {
                let path = rel_path(&owned_args, "path")?;
                let edit = fs::TextEdit {
                    old_string: require_str(&owned_args, "old_string")?.to_string(),
                    new_string: require_str(&owned_args, "new_string")?.to_string(),
                };
                let before = prior_state(&root, &path)?;
                let outcome = root
                    .edit(&path, std::slice::from_ref(&edit), expect)
                    .map_err(|e| anyhow!("{}", fs_error_text(&e)))?;
                let content = root
                    .read(&path, None)
                    .map_err(|e| anyhow!("{}", fs_error_text(&e)))?
                    .content;
                Ok(WriteEffect {
                    value: json!({
                        "path": path.as_str(),
                        "sha256": outcome.blob_sha,
                        "bytes": outcome.bytes,
                    }),
                    edit: before.map(|expect| RecordedEdit {
                        path: path.as_str().to_string(),
                        kind: patch::EditKind::Write,
                        content: Some(content.into_bytes()),
                        expect,
                    }),
                })
            }
            CoreToolName::FsMove => {
                let from = rel_path(&owned_args, "from")?;
                let to = rel_path(&owned_args, "to")?;
                let before = prior_state(&root, &from)?;
                root.rename(&from, &to, expect)
                    .map_err(|e| anyhow!("{}", fs_error_text(&e)))?;
                Ok(WriteEffect {
                    value: json!({ "from": from.as_str(), "to": to.as_str() }),
                    // A rename does not change content, so the journal keeps
                    // the blob the path already carries: re-reading the file
                    // would refuse a binary one for no gain.
                    edit: before.map(|expect| RecordedEdit {
                        path: from.as_str().to_string(),
                        kind: patch::EditKind::Rename {
                            new_path: to.as_str().to_string(),
                        },
                        content: None,
                        expect,
                    }),
                })
            }
            CoreToolName::FsDelete => {
                let path = rel_path(&owned_args, "path")?;
                let recursive = owned_args
                    .get("recursive")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let before = prior_state(&root, &path)?;
                root.remove(&path, recursive, expect)
                    .map_err(|e| anyhow!("{}", fs_error_text(&e)))?;
                Ok(WriteEffect {
                    value: json!({ "path": path.as_str(), "deleted": true }),
                    // Deleting a DIRECTORY records nothing here: git has no
                    // object for one, and the paths it contained are picked up
                    // by the snapshot the next review opens with.
                    edit: before.map(|expect| RecordedEdit {
                        path: path.as_str().to_string(),
                        kind: patch::EditKind::Delete,
                        content: None,
                        expect,
                    }),
                })
            }
            CoreToolName::FsMkdir => {
                let path = rel_path(&owned_args, "path")?;
                root.mkdir(&path)
                    .map_err(|e| anyhow!("{}", fs_error_text(&e)))?;
                // A directory is not a git object: there is nothing for a
                // review to decide on until a file appears in it.
                Ok(WriteEffect {
                    value: json!({ "path": path.as_str(), "created": true }),
                    edit: None,
                })
            }
            other => Err(anyhow!("'{}' is not a write call", other.public_name())),
        }
    })
    .await
    .map_err(|e| anyhow!("filesystem write task failed: {e}"))?;

    let (value, edit) = match effect {
        Ok(effect) => (Ok(effect.value), effect.edit),
        Err(error) => (Err(error), None),
    };
    settle_operation(bound, &op, &value, &[], None)?;
    let value = value?;
    if let Some(edit) = edit {
        record_work_edit(
            &bound.pool,
            &bound.broker,
            &bound.session.id,
            &edit.path,
            edit.kind,
            edit.content.as_deref(),
            &edit.expect,
        )
        .map_err(|e| {
            anyhow!(
                "[TOOL_ERROR] '{}' landed in the worktree but could not be recorded for review, \
                 so it would neither be reviewed nor committed: {e:#}",
                edit.path
            )
        })?;
    }
    Ok(value)
}

/// One completed write, plus what the review journal has to record about it.
struct WriteEffect {
    value: Value,
    edit: Option<RecordedEdit>,
}

/// One worktree change in the form `patch::record_edit` takes it.
struct RecordedEdit {
    path: String,
    kind: patch::EditKind,
    /// Content the path ends up holding; `None` for a delete.
    content: Option<Vec<u8>>,
    /// What the path held BEFORE the call.
    expect: patch::Precondition,
}

/// The state a path is in right now, in the form the review journal compares
/// against.
///
/// `None` means there is nothing for a review to record about this path — a
/// directory is not a git object. A file the filesystem layer will not hash,
/// on the other hand, is an ERROR rather than a guess: recording it as absent
/// would turn an ordinary edit into a conflict, and recording nothing would
/// drop a real change out of the review and out of the commit.
fn prior_state(root: &SessionRoot, path: &RelPath) -> Result<Option<patch::Precondition>> {
    match root.stat(path) {
        Ok(stat) if stat.is_file => match stat.blob_sha {
            Some(sha) => Ok(Some(patch::Precondition::BlobIs(sha))),
            None => Err(anyhow!(
                "[TOOL_ERROR] {} is too large to record in a review, so it cannot be \
                 changed while one is open",
                path.as_str()
            )),
        },
        Ok(_) => Ok(None),
        Err(fs::FsError::NotFound) => Ok(Some(patch::Precondition::Absent)),
        Err(error) => Err(anyhow!("{}", fs_error_text(&error))),
    }
}

/// Derives the operation's conditions from the call. The precondition is the
/// CAS the model supplied; the postcondition is what must hold afterwards, so a
/// crash between effect and acknowledgement can be reconciled without guessing.
fn fs_conditions(
    tool: CoreToolName,
    args: &Value,
) -> Result<(Precondition, Postcondition, OperationInput)> {
    let expected = optional_str(args, "expected_sha256").map(str::to_string);
    let path = match tool {
        CoreToolName::FsMove => require_str(args, "from")?.to_string(),
        _ => require_str(args, "path")?.to_string(),
    };
    let precondition = match expected.as_deref() {
        Some("") => Precondition::FileAbsent { path: path.clone() },
        Some(sha) => Precondition::FileBlobIs {
            path: path.clone(),
            sha256: sha.to_string(),
        },
        None => Precondition::None,
    };
    let (postcondition, input) = match tool {
        CoreToolName::FsWrite | CoreToolName::FsEdit => {
            let content = if tool == CoreToolName::FsWrite {
                require_str(args, "content")?.to_string()
            } else {
                String::new()
            };
            (
                Postcondition::None,
                OperationInput::FileContent {
                    path: path.clone(),
                    content_sha256: fs::blob_sha(content.as_bytes()),
                    size_bytes: content.len() as u64,
                },
            )
        }
        CoreToolName::FsDelete => (
            Postcondition::FileAbsent { path: path.clone() },
            OperationInput::Params(BTreeMap::from([("path".to_string(), path.clone())])),
        ),
        CoreToolName::FsMove => {
            let to = require_str(args, "to")?.to_string();
            (
                Postcondition::FileAbsent { path: path.clone() },
                OperationInput::Params(BTreeMap::from([
                    ("from".to_string(), path.clone()),
                    ("to".to_string(), to.clone()),
                ])),
            )
        }
        _ => (
            Postcondition::None,
            OperationInput::Params(BTreeMap::from([("path".to_string(), path.clone())])),
        ),
    };
    Ok((precondition, postcondition, input))
}

fn expected_precondition(args: &Value) -> FsPrecondition {
    match optional_str(args, "expected_sha256") {
        Some("") => FsPrecondition::Absent,
        Some(sha) => FsPrecondition::BlobIs(sha.to_string()),
        None => FsPrecondition::Any,
    }
}

// --- exec -------------------------------------------------------------------

/// One finished command: what the model is told, and the artifact the journal
/// closes the operation with.
struct ExecEffect {
    value: Value,
    artifact: String,
}

/// Stores a command's transcript as the artifact §24 requires, REDACTED before
/// it is written: an artifact is read by people and mirrored into the audit
/// trail, so a token in an argv must never reach the blob.
///
/// One implementation for the agent and for the dashboard. It is also what
/// makes an `ExitCodeRecorded` operation closable at all — the journal refuses
/// to call a command "completed" when nothing holds its outcome.
pub fn store_exec_transcript(
    pool: &DbPool,
    workspace_id: &str,
    outcome: &super::exec::ExecOutcome,
) -> Result<String> {
    let transcript = redact::redact_text(&format!(
        "$ {}\n{}\n{}",
        redact::redact_argv(&outcome.argv).join(" "),
        outcome.stdout.text,
        outcome.stderr.text
    ));
    artifacts::put(pool, workspace_id, transcript.as_bytes(), "exec_output")
        .map(|stored| stored.sha256)
}

async fn exec_call(
    ctx: &ToolCallCtx<'_>,
    bound: &Bound,
    args: &Value,
    profile: PepProfile,
) -> Result<Value> {
    let argv: Vec<String> = args
        .get("argv")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    if argv.is_empty() {
        return Err(anyhow!(
            "'argv' is required and must be a non-empty array of strings"
        ));
    }
    let cwd = optional_str(args, "cwd").map(str::to_string);
    if let Some(dir) = cwd.as_deref() {
        RelPath::parse(dir).map_err(|e| anyhow!("{}", fs_error_text(&e)))?;
    }
    let timeout_secs = args
        .get("timeout_secs")
        .and_then(|v| v.as_u64())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_EXEC_TIMEOUT_SECS)
        .min(MAX_EXEC_TIMEOUT_SECS);

    // The profile is resolved BEFORE the journal row so the operation records
    // what the command actually ran under, not what was asked for.
    let effective_profile = narrow_profile(profile, args);
    // A model that asked for `rw` and silently got `cow` would report a file it
    // never changed, so the answer it reads carries the narrowing itself.
    let requested_mount = optional_str(args, "mount_access").unwrap_or("").to_string();
    let effective_mount = mount_slug(effective_profile.mount).to_string();
    let writes_discarded = effective_profile.mount == PepMountAccess::CopyOnWrite;
    let op = begin_operation(
        ctx,
        bound,
        CoreToolName::Exec,
        OperationInput::Exec {
            argv: redact::redact_argv(&argv),
            cwd: cwd.clone().unwrap_or_else(|| ".".to_string()),
            timeout_secs,
        },
        Precondition::None,
        Postcondition::ExitCodeRecorded,
        Some(effective_profile),
    )?;

    let workspace_id = bound.workspace.id.clone();
    let session_id = bound.session.id.clone();
    let exec_mode = bound.workspace.exec_mode.clone();
    let exec_id = op.op_id.clone();
    let run_id = ctx.run_id.map(str::to_string);
    let sandbox_profile = SandboxProfile::from_decision(
        effective_profile,
        args.get("ephemeral")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    );
    let pool = bound.pool.clone();

    let workspace_for_artifact = workspace_id.clone();
    let requested_for_answer = requested_mount.clone();
    let effect = tokio::task::spawn_blocking(move || -> Result<ExecEffect> {
        let mode = super::models::ExecMode::from_slug(&exec_mode)
            .ok_or_else(|| anyhow!("workspace has an unknown exec mode '{exec_mode}'"))?;
        let manager = SandboxManager::for_workspace(&workspace_id, mode, None)?;
        let lease = manager
            .acquire(&pool, &session_id, sandbox_profile, run_id.as_deref())
            .map_err(|e| anyhow!("{e}"))?;
        let env = ExecEnv::for_lease(&lease);
        let request = ExecRequest {
            exec_id,
            program: Program::Argv(argv),
            cwd_rel: cwd,
            timeout: Duration::from_secs(timeout_secs),
            max_output_bytes: super::exec::DEFAULT_MAX_OUTPUT_BYTES,
        };
        // The executor is shared per workspace: a second instance would mean a
        // second set of concurrency permits, and `exec_cancel` would look for
        // the command in a registry that never saw it.
        let executor = Executor::for_workspace(&workspace_id);
        let result = executor.exec(lease.target(), &env, &request, Arc::new(NullSink));
        // The lease is released whatever the command did: a failed build must
        // not leak a copy-on-write layer for the rest of the session.
        let release = manager.release(&pool, lease);
        let outcome = result?;
        release?;
        // Written before the operation is closed: the transcript IS the
        // recorded outcome, and §24 requires it to exist for every command.
        let artifact = store_exec_transcript(&pool, &workspace_for_artifact, &outcome)?;
        Ok(ExecEffect {
            value: json!({
                "exit_code": match outcome.status {
                    super::exec::ExitStatus::Code(c) => Some(c),
                    _ => None,
                },
                "status": exit_status_slug(outcome.status),
                "duration_ms": outcome.duration_ms,
                "stdout": redact::redact_text(&outcome.stdout.text),
                "stdout_truncated": outcome.stdout.truncated,
                "stderr": redact::redact_text(&outcome.stderr.text),
                "stderr_truncated": outcome.stderr.truncated,
                "artifact": artifact.clone(),
                "mount_access": effective_mount,
                "requested_mount_access": requested_for_answer,
                "writes_discarded": writes_discarded,
            }),
            artifact,
        })
    })
    .await
    .map_err(|e| anyhow!("exec task failed: {e}"))?;

    let (outcome, artifact) = match effect {
        Ok(effect) => (Ok(effect.value), Some(effect.artifact)),
        Err(error) => (Err(error), None),
    };
    settle_operation(bound, &op, &outcome, &[], artifact.as_deref())?;
    if let Ok(value) = &outcome {
        let _ = events::append(
            &bound.pool,
            &bound.session.id,
            SessionEvent::new(
                format!("exec:{}", op.op_id),
                EventPayload::Exec {
                    op_id: op.op_id.clone(),
                    argv: redact::redact_argv(
                        &args
                            .get("argv")
                            .and_then(|v| v.as_array())
                            .map(|a| {
                                a.iter()
                                    .filter_map(|x| x.as_str().map(String::from))
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default(),
                    ),
                    cwd: optional_str(args, "cwd").unwrap_or(".").to_string(),
                    exit_code: value
                        .get("exit_code")
                        .and_then(|v| v.as_i64())
                        .map(|c| c as i32),
                    requested_mount_access: requested_mount,
                    writes_discarded,
                },
            ),
        );
    }
    outcome
}

/// Intersects a caller's requested sandbox profile with the one the PEP
/// allowed. A request can only ever NARROW: asking for `rw` in a session the
/// policy limited to `cow` yields `cow`, and asking for `ro` when `rw` was
/// allowed is honoured, because running with less access is always safe.
fn narrow_profile(allowed: PepProfile, args: &Value) -> PepProfile {
    use super::pep::{MountAccess, NetworkAccess};
    fn mount_rank(m: MountAccess) -> u8 {
        match m {
            MountAccess::ReadOnly => 0,
            MountAccess::CopyOnWrite => 1,
            MountAccess::ReadWrite => 2,
        }
    }
    let requested_mount = match args.get("mount_access").and_then(|v| v.as_str()) {
        Some("ro") => Some(MountAccess::ReadOnly),
        Some("cow") => Some(MountAccess::CopyOnWrite),
        Some("rw") => Some(MountAccess::ReadWrite),
        _ => None,
    };
    let mount = match requested_mount {
        Some(requested) if mount_rank(requested) < mount_rank(allowed.mount) => requested,
        _ => allowed.mount,
    };
    let network = match args.get("network_access").and_then(|v| v.as_str()) {
        Some("none") => NetworkAccess::None,
        _ => allowed.network,
    };
    PepProfile { mount, network }
}

fn exit_status_slug(status: super::exec::ExitStatus) -> &'static str {
    match status {
        super::exec::ExitStatus::Code(0) => "ok",
        super::exec::ExitStatus::Code(_) => "failed",
        super::exec::ExitStatus::Signal(_) => "signalled",
        super::exec::ExitStatus::Timeout => "timeout",
        super::exec::ExitStatus::Cancelled => "cancelled",
    }
}

// --- git --------------------------------------------------------------------

async fn git_read_call(bound: &Bound, args: &Value) -> Result<Value> {
    let operation = require_str(args, "operation")?.to_string();
    let path = optional_str(args, "path").map(str::to_string);
    if let Some(p) = path.as_deref() {
        RelPath::parse(p).map_err(|e| anyhow!("{}", fs_error_text(&e)))?;
    }
    let rev = optional_str(args, "rev").map(str::to_string);
    let staged = args
        .get("staged")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_GIT_LIMIT as u64) as u32;

    let workspace_id = bound.workspace.id.clone();
    let session_id = bound.session.id.clone();
    tokio::task::spawn_blocking(move || -> Result<Value> {
        let broker = Broker::for_workspace(&workspace_id)?;
        let handle = broker.session(&session_id)?;
        match operation.as_str() {
            "status" => Ok(json!({ "entries": broker.status(&session_id)? })),
            "log" => {
                let entries = broker.log(&handle, path.as_deref().unwrap_or(""), limit)?;
                Ok(json!({
                    "commits": entries
                        .iter()
                        .map(|e| json!({
                            "oid": e.oid,
                            "short_oid": e.short_oid,
                            "author": e.author,
                            "date": e.date,
                            "subject": e.subject,
                        }))
                        .collect::<Vec<_>>(),
                }))
            }
            "ls_files" => {
                let tree = resolve_tree(&broker, &handle, rev.as_deref())?;
                let entries = broker.list_tree(&handle, &tree)?;
                let filtered: Vec<&super::git_broker::TreeEntry> = entries
                    .iter()
                    .filter(|e| match path.as_deref() {
                        Some(prefix) => e.path.starts_with(prefix),
                        None => true,
                    })
                    .take(limit as usize)
                    .collect();
                Ok(json!({
                    "files": filtered.iter().map(|e| e.path.clone()).collect::<Vec<_>>(),
                    "truncated": filtered.len() as u32 >= limit,
                }))
            }
            "show" => {
                let rev = rev.as_deref().unwrap_or("HEAD");
                let commit = broker
                    .rev_parse(&handle, rev)?
                    .ok_or_else(|| anyhow!("git does not know the revision '{rev}'"))?;
                match path.as_deref() {
                    Some(p) => {
                        let oid = broker
                            .blob_in_commit(&handle, &commit, p)?
                            .ok_or_else(|| anyhow!("'{p}' does not exist in {rev}"))?;
                        let bytes = broker.cat_file(&handle, &oid)?;
                        Ok(json!({
                            "commit": commit,
                            "path": p,
                            "content": String::from_utf8_lossy(&bytes),
                        }))
                    }
                    None => {
                        let meta = broker
                            .commit_metadata(&handle, &commit)?
                            .ok_or_else(|| anyhow!("commit '{commit}' has no metadata"))?;
                        Ok(json!({
                            "commit": commit,
                            "tree": meta.tree,
                            "parents": meta.parents,
                        }))
                    }
                }
            }
            "diff" => {
                let base = match rev.as_deref() {
                    Some(r) => broker
                        .rev_parse(&handle, r)?
                        .ok_or_else(|| anyhow!("git does not know the revision '{r}'"))?,
                    None => broker.head_commit(&handle)?,
                };
                // Without an explicit head the comparison is against the working
                // tree, snapshotted into a temporary index — the same material a
                // patch set is built from, so diff and review never disagree.
                let head = if staged {
                    base.clone()
                } else {
                    broker.snapshot_worktree(&handle, &base)?
                };
                let entries = broker.diff_name_status(&handle, &base, &head)?;
                match path.as_deref() {
                    Some(p) => Ok(json!({
                        "base": base,
                        "path": p,
                        "patch": broker.diff_patch(&handle, &base, &head, p)?,
                    })),
                    None => Ok(json!({
                        "base": base,
                        "files": entries
                            .iter()
                            .map(|e| json!({
                                "status": e.status.to_string(),
                                "path": e.path,
                                "old_path": e.old_path,
                            }))
                            .collect::<Vec<_>>(),
                    })),
                }
            }
            other => Err(anyhow!(
                "unknown git read operation '{other}'; use status, diff, log, show or ls_files"
            )),
        }
    })
    .await
    .map_err(|e| anyhow!("git read task failed: {e}"))?
}

fn resolve_tree(broker: &Broker, handle: &RepoHandle, rev: Option<&str>) -> Result<String> {
    let rev = rev.unwrap_or("HEAD");
    broker
        .rev_parse(handle, rev)?
        .ok_or_else(|| anyhow!("git does not know the revision '{rev}'"))
}

async fn git_branch_call(bound: &Bound) -> Result<Value> {
    let workspace_id = bound.workspace.id.clone();
    let session_id = bound.session.id.clone();
    let session_branch = bound.session.branch.clone();
    tokio::task::spawn_blocking(move || -> Result<Value> {
        let broker = Broker::for_workspace(&workspace_id)?;
        let handle = broker.session(&session_id)?;
        let branches = broker.branches(&handle)?;
        Ok(json!({
            "session_branch": session_branch,
            "branches": branches
                .iter()
                .map(|b| json!({
                    "name": b.name,
                    "current": b.is_current,
                    "upstream": b.upstream,
                    "ahead": b.ahead,
                    "behind": b.behind,
                }))
                .collect::<Vec<_>>(),
        }))
    })
    .await
    .map_err(|e| anyhow!("git branch task failed: {e}"))?
}

async fn git_sync_call(ctx: &ToolCallCtx<'_>, bound: &Bound, args: &Value) -> Result<Value> {
    let operation = require_str(args, "operation")?.to_string();
    if !matches!(operation.as_str(), "fetch" | "pull") {
        return Err(anyhow!(
            "unknown git sync operation '{operation}'; use fetch or pull"
        ));
    }
    let remote = optional_str(args, "remote").unwrap_or("origin").to_string();
    let branch = bound.session.branch.clone();
    let op = begin_operation(
        ctx,
        bound,
        CoreToolName::GitSync,
        OperationInput::Git {
            operation: operation.clone(),
            refname: Some(branch.clone()),
            remote: Some(remote.clone()),
            oids: Vec::new(),
        },
        Precondition::None,
        Postcondition::None,
        None,
    )?;

    let workspace_id = bound.workspace.id.clone();
    let session_id = bound.session.id.clone();
    let outcome = tokio::task::spawn_blocking(move || -> Result<Value> {
        let broker = Broker::for_workspace(&workspace_id)?;
        let handle = broker.session(&session_id)?;
        // Credentials come from the workspace vault inside the broker; a tool
        // call never carries them, so `GitAuth::None` here means "use whatever
        // the repository is configured with", not "unauthenticated".
        let head = match operation.as_str() {
            "fetch" => broker.fetch_branch(&handle, &remote, &branch, &GitAuth::None)?,
            _ => broker.pull_branch(&handle, &remote, &branch, &GitAuth::None)?,
        };
        Ok(json!({ "operation": operation, "remote": remote, "head": head }))
    })
    .await
    .map_err(|e| anyhow!("git sync task failed: {e}"))?;

    settle_operation(bound, &op, &outcome, &[], None)?;
    outcome
}

/// `core.git_stage` — prepares the change set the next commit acts on.
///
/// Staging in Code Studio is not a git index: the commit is built from the blobs
/// the operator ACCEPTED (§11.5), so what the agent can do is close the current
/// worktree state into a patch set and see which paths it captured. Unstaging is
/// refused by name: dropping a changed file from a commit is the reviewer's
/// per-file reject, and letting the agent do it silently would defeat the point.
async fn git_stage_call(bound: &Bound, args: &Value) -> Result<Value> {
    if args
        .get("unstage")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return Err(anyhow!(
            "unstaging is not available: the commit is assembled from the reviewed patch set, \
             so excluding a changed file is a per-file reject in the review, not a tool call"
        ));
    }
    let requested: Vec<String> = args
        .get("paths")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    for path in &requested {
        RelPath::parse(path).map_err(|e| anyhow!("{}", fs_error_text(&e)))?;
    }
    let set = current_patch_set(
        &bound.pool,
        &bound.broker,
        &bound.session.id,
        &PatchScope::Work,
    )?;
    let staged: Vec<&patch::PatchFile> = set
        .files
        .iter()
        .filter(|f| requested.is_empty() || requested.iter().any(|p| p == &f.path))
        .collect();
    let missing: Vec<&String> = requested
        .iter()
        .filter(|p| !set.files.iter().any(|f| &f.path == *p))
        .collect();
    Ok(json!({
        "patch_set_id": set.id,
        "staged": staged
            .iter()
            .map(|f| json!({ "path": f.path, "change": f.change_kind }))
            .collect::<Vec<_>>(),
        "unchanged": missing,
    }))
}

async fn git_commit_call(ctx: &ToolCallCtx<'_>, bound: &Bound, args: &Value) -> Result<Value> {
    let message = require_str(args, "message")?.to_string();
    // The WORK review, not "the newest acceptance of this session": a merge
    // result accepted for the target branch is a decision about another tree.
    let set = patch::accepted_patch_set_for_scope(
        &bound.pool,
        &bound.session.id,
        &PatchScope::Work,
    )?;

    let workspace_id = bound.workspace.id.clone();
    let session_id = bound.session.id.clone();
    let branch = bound.session.branch.clone();
    let identity = commit_identity(bound, ctx.user_id);
    let pool = bound.pool.clone();
    let patch_set_id = set.id.clone();

    // The spec and the tree it produces are resolved BEFORE the operation is
    // opened: §13.1 makes a commit's outcome verifiable, and the journal can
    // only say so if it knows the tree the accepted blobs compose to.
    let (spec, planned_tree) = {
        let workspace_id = workspace_id.clone();
        let session_id = session_id.clone();
        let branch = branch.clone();
        let pool = pool.clone();
        let patch_set_id = patch_set_id.clone();
        tokio::task::spawn_blocking(move || -> Result<(CommitSpec, String)> {
            let broker = Broker::for_workspace(&workspace_id)?;
            let handle = broker.session(&session_id)?;
            let expected_old = broker.rev_parse(&handle, &format!("refs/heads/{branch}"))?;
            let spec = patch::accepted_commit_spec(
                &pool,
                &broker,
                &patch_set_id,
                &CommitRequest {
                    branch,
                    expected_old,
                    message,
                    author: identity.clone(),
                    committer: identity,
                    extra_parent: None,
                },
            )?;
            if spec.files.is_empty() {
                return Err(anyhow!(
                    "the accepted review contains no file, so there is nothing to commit"
                ));
            }
            let tree = broker.plan_tree(&broker.reference(), &spec)?;
            Ok((spec, tree))
        })
        .await
        .map_err(|e| anyhow!("git commit planning task failed: {e}"))??
    };

    let op = begin_operation(
        ctx,
        bound,
        CoreToolName::GitCommit,
        OperationInput::Git {
            operation: "commit".to_string(),
            refname: Some(bound.session.branch.clone()),
            remote: None,
            oids: vec![spec.base_commit.clone()],
        },
        Precondition::None,
        Postcondition::CommitExists {
            tree: planned_tree,
            parent: Some(spec.base_commit.clone()),
        },
        None,
    )?;

    let op_id_for_task = op.op_id.clone();
    let outcome = tokio::task::spawn_blocking(move || -> Result<Value> {
        let broker = Broker::for_workspace(&workspace_id)?;
        let commit = broker.build_commit(&broker.reference(), &spec)?;
        // The id is journalled the moment git returns it: after a crash the
        // probe identifies the commit by tree, parent and the branch head, and
        // it can only look for an id somebody wrote down.
        operations::record_oids(&pool, &op_id_for_task, &[commit.commit_oid.clone()])?;
        patch::mark_consumed(&pool, &patch_set_id, &commit)?;
        Ok(json!({
            "commit": commit.commit_oid,
            "branch": commit.ref_name,
            "files": commit.blob_oids.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>(),
        }))
    })
    .await
    .map_err(|e| anyhow!("git commit task failed: {e}"))?;

    let oids = outcome
        .as_ref()
        .ok()
        .and_then(|v| v.get("commit").and_then(|c| c.as_str()))
        .map(|c| vec![c.to_string()])
        .unwrap_or_default();
    settle_operation(bound, &op, &outcome, &oids, None)?;
    emit_git_event(bound, &op.op_id, GitOperation::Commit, &outcome, None);
    outcome
}

async fn git_push_call(ctx: &ToolCallCtx<'_>, bound: &Bound, args: &Value) -> Result<Value> {
    let remote = optional_str(args, "remote").unwrap_or("origin").to_string();
    let branch = bound.session.branch.clone();
    let op = begin_operation(
        ctx,
        bound,
        CoreToolName::GitPush,
        OperationInput::Git {
            operation: "push".to_string(),
            refname: Some(branch.clone()),
            remote: Some(remote.clone()),
            oids: Vec::new(),
        },
        Precondition::None,
        Postcondition::None,
        None,
    )?;

    let workspace_id = bound.workspace.id.clone();
    let session_id = bound.session.id.clone();
    let remote_for_task = remote.clone();
    let outcome = tokio::task::spawn_blocking(move || -> Result<Value> {
        let broker = Broker::for_workspace(&workspace_id)?;
        let handle = broker.session(&session_id)?;
        broker.push_branch(&handle, &remote_for_task, &branch, &GitAuth::None)?;
        Ok(json!({ "pushed": branch, "remote": remote_for_task }))
    })
    .await
    .map_err(|e| anyhow!("git push task failed: {e}"))?;

    settle_operation(bound, &op, &outcome, &[], None)?;
    emit_git_event(bound, &op.op_id, GitOperation::Push, &outcome, Some(remote));
    outcome
}

async fn git_merge_call(ctx: &ToolCallCtx<'_>, bound: &Bound, args: &Value) -> Result<Value> {
    let target_branch = require_str(args, "target_branch")?.to_string();
    let op = begin_operation(
        ctx,
        bound,
        CoreToolName::GitMerge,
        OperationInput::Git {
            operation: "merge".to_string(),
            refname: Some(target_branch.clone()),
            remote: None,
            oids: Vec::new(),
        },
        Precondition::None,
        Postcondition::None,
        None,
    )?;

    let workspace_id = bound.workspace.id.clone();
    let session_id = bound.session.id.clone();
    let source_branch = bound.session.branch.clone();
    let op_id = op.op_id.clone();
    let pool = bound.pool.clone();
    let outcome = tokio::task::spawn_blocking(move || -> Result<Value> {
        let broker = Broker::for_workspace(&workspace_id)?;
        let reference = broker.reference();
        let expected_old = broker
            .read_ref(&reference, &format!("refs/heads/{target_branch}"))?
            .ok_or_else(|| anyhow!("target branch '{target_branch}' does not exist"))?;
        let path = broker.add_integration_worktree(&session_id, &op_id, &expected_old)?;
        let merged = broker.merge_into_integration(&session_id, &source_branch)?;
        // The held worktree is journaled here and nowhere else: §16.3 requires a
        // revision run to FIND the half-merged tree, and the broker deliberately
        // owns only the git side of it.
        let (head_commit, result) = match &merged {
            MergeOutcome::Clean { merge_head, .. } => (merge_head.clone(), "clean"),
            MergeOutcome::Conflict { .. } => (expected_old.clone(), "conflict"),
        };
        record_integration_worktree(
            &pool,
            &session_id,
            &op_id,
            &path.display().to_string(),
            &target_branch,
            &expected_old,
            &head_commit,
        )?;
        Ok(match merged {
            MergeOutcome::Clean {
                merge_head,
                fast_forward,
            } => json!({
                "result": result,
                "merge_head": merge_head,
                "fast_forward": fast_forward,
                "expected_old": expected_old,
                "target_branch": target_branch,
                "next": "verify the result, then call core.git_merge_finalize",
            }),
            MergeOutcome::Conflict { paths } => json!({
                "result": result,
                "conflicted_paths": paths,
                "expected_old": expected_old,
                "target_branch": target_branch,
                "next": "resolve the conflicts in the integration worktree, \
                         then call core.git_merge_finalize",
            }),
        })
    })
    .await
    .map_err(|e| anyhow!("git merge task failed: {e}"))?;

    settle_operation(bound, &op, &outcome, &[], None)?;
    emit_git_event(bound, &op.op_id, GitOperation::Merge, &outcome, None);
    outcome
}

#[allow(clippy::too_many_arguments)]
fn record_integration_worktree(
    pool: &DbPool,
    session_id: &str,
    op_id: &str,
    path: &str,
    branch: &str,
    base_commit: &str,
    head_commit: &str,
) -> Result<()> {
    let conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    conn.execute(
        "INSERT INTO worktrees \
            (id, session_id, purpose, op_id, path, branch, head_commit, base_commit, state, \
             created_at) \
         VALUES (?1, ?2, 'integration', ?3, ?4, ?5, ?6, ?7, 'held', datetime('now')) \
         ON CONFLICT(session_id, purpose, op_id) DO UPDATE SET \
            head_commit = excluded.head_commit, state = 'held'",
        rusqlite::params![
            uuid::Uuid::new_v4().to_string(),
            session_id,
            op_id,
            path,
            branch,
            head_commit,
            base_commit,
        ],
    )?;
    Ok(())
}

async fn git_merge_finalize_call(
    ctx: &ToolCallCtx<'_>,
    bound: &Bound,
    args: &Value,
) -> Result<Value> {
    let message = optional_str(args, "message")
        .map(str::to_string)
        .unwrap_or_else(|| format!("Merge {} into the target branch", bound.session.branch));

    // The held merge decides which review is being published: the decision has
    // to be the one taken on THIS merge result, not the session's newest
    // acceptance, which is a review of the session branch (§11.6 step 5).
    let held = open_merge_state(bound)?;
    let set = patch::accepted_patch_set_for_scope(
        &bound.pool,
        &bound.session.id,
        &PatchScope::Merge {
            op_id: held.op_id.clone(),
        },
    )?;
    let op = begin_operation(
        ctx,
        bound,
        CoreToolName::GitMergeFinalize,
        OperationInput::Git {
            operation: "merge_finalize".to_string(),
            refname: Some(held.target_branch.clone()),
            remote: None,
            oids: vec![held.expected_old.clone()],
        },
        // The compare-and-swap the whole merge hangs on: if the target branch
        // moved while the result was being verified, this operation must abort
        // rather than publish a merge nobody checked against the new base.
        Precondition::RefEquals {
            refname: format!("refs/heads/{}", held.target_branch),
            oid: held.expected_old.clone(),
        },
        Postcondition::None,
        None,
    )?;

    let workspace_id = bound.workspace.id.clone();
    let session_id = bound.session.id.clone();
    let source_branch = bound.session.branch.clone();
    let identity = commit_identity(bound, ctx.user_id);
    let pool = bound.pool.clone();
    let patch_set_id = set.id.clone();
    let held_op_id = held.op_id.clone();
    let target_branch = held.target_branch.clone();
    let expected_old = held.expected_old.clone();
    let outcome = tokio::task::spawn_blocking(move || -> Result<Value> {
        let broker = Broker::for_workspace(&workspace_id)?;
        let reference = broker.reference();
        // The merge's SECOND parent is the branch that was merged in, not the
        // integration head: the tree comes from the accepted blobs, the parents
        // record where it came from.
        let source_tip = broker
            .read_ref(&reference, &format!("refs/heads/{source_branch}"))?
            .ok_or_else(|| anyhow!("session branch '{source_branch}' no longer exists"))?;
        let spec = patch::accepted_commit_spec(
            &pool,
            &broker,
            &patch_set_id,
            &CommitRequest {
                branch: target_branch.clone(),
                expected_old: Some(expected_old),
                message,
                author: identity.clone(),
                committer: identity,
                extra_parent: Some(source_tip),
            },
        )?;
        let commit = broker.finalize_merge(&spec)?;
        patch::mark_consumed(&pool, &patch_set_id, &commit)?;
        broker.remove_integration_worktree(&session_id, &held_op_id)?;
        release_integration_worktree(&pool, &session_id, &held_op_id)?;
        Ok(json!({
            "commit": commit.commit_oid,
            "branch": commit.ref_name,
            "moved_from": commit.ref_before,
            "moved_to": commit.ref_after,
        }))
    })
    .await
    .map_err(|e| anyhow!("git merge finalize task failed: {e}"))?;

    settle_operation(bound, &op, &outcome, &[], None)?;
    emit_git_event(
        bound,
        &op.op_id,
        GitOperation::MergeFinalize,
        &outcome,
        None,
    );
    outcome
}

/// The pending merge a finalize acts on.
struct HeldMerge {
    op_id: String,
    target_branch: String,
    expected_old: String,
}

/// The merge operation this session currently holds open, if any. It is the
/// identity a merge review and its finalize are both scoped to, so both resolve
/// it from the journal instead of taking it from a caller.
pub fn held_merge_op(pool: &DbPool, session_id: &str) -> Result<Option<String>> {
    let conn = pool.read().map_err(|e| anyhow!("workspace db read: {e}"))?;
    let op_id: Option<Option<String>> = conn
        .query_row(
            "SELECT op_id FROM worktrees \
             WHERE session_id = ?1 AND purpose = 'integration' AND state = 'held' \
             ORDER BY created_at DESC LIMIT 1",
            rusqlite::params![session_id],
            |row| row.get(0),
        )
        .ok();
    Ok(op_id.flatten())
}

/// Reads back the state the pending `core.git_merge` left in the worktree
/// journal. Finalizing without that state is refused rather than guessed: the
/// target ref may only move onto a result somebody verified.
fn open_merge_state(bound: &Bound) -> Result<HeldMerge> {
    let conn = bound
        .pool
        .read()
        .map_err(|e| anyhow!("workspace db read: {e}"))?;
    let row: Option<(Option<String>, Option<String>, String)> = conn
        .query_row(
            "SELECT op_id, branch, base_commit FROM worktrees \
             WHERE session_id = ?1 AND purpose = 'integration' AND state = 'held' \
             ORDER BY created_at DESC LIMIT 1",
            rusqlite::params![bound.session.id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .ok();
    let (op_id, branch, base_commit) = row.ok_or_else(|| {
        anyhow!(
            "there is no merge waiting to be finalized in this session; \
             call core.git_merge first"
        )
    })?;
    Ok(HeldMerge {
        op_id: op_id.ok_or_else(|| anyhow!("the held merge carries no operation id"))?,
        target_branch: branch.ok_or_else(|| anyhow!("the held merge carries no target branch"))?,
        expected_old: base_commit,
    })
}

fn release_integration_worktree(pool: &DbPool, session_id: &str, op_id: &str) -> Result<()> {
    let conn = pool
        .write()
        .map_err(|e| anyhow!("workspace db write: {e}"))?;
    conn.execute(
        "UPDATE worktrees SET state = 'removed', removed_at = datetime('now') \
         WHERE session_id = ?1 AND purpose = 'integration' AND op_id = ?2",
        rusqlite::params![session_id, op_id],
    )?;
    Ok(())
}

fn commit_identity(bound: &Bound, user_id: &str) -> CommitIdentity {
    CommitIdentity {
        name: bound.session.user_id.clone(),
        // A synthetic address keeps the commit self-describing without leaking
        // a directory address into every repository the workspace pushes to.
        email: format!("{user_id}@code-studio.tentaflow.local"),
    }
}

// =============================================================================
// Patch sets
// =============================================================================

/// The session's open patch set OF THIS SCOPE, created lazily at the first
/// effect. A work review and a merge review live side by side — the agent keeps
/// editing its worktree while a merge result waits for a decision — so the
/// selector follows the scope of the row and never answers with the other one.
pub fn current_patch_set(
    bound_pool: &DbPool,
    broker: &Broker,
    session_id: &str,
    scope: &PatchScope,
) -> Result<PatchSet> {
    if let Some(existing) = patch::open_patch_set_for_scope(bound_pool, session_id, scope)? {
        return Ok(existing);
    }
    let handle = match scope {
        PatchScope::Work => broker.session(session_id)?,
        PatchScope::Merge { .. } => broker.integration(session_id)?,
    };
    let base_commit = broker.head_commit(&handle)?;
    patch::open_patch_set(bound_pool, broker, session_id, None, &base_commit, scope)
}

/// Mirrors one worktree change into the session's open WORK patch set.
///
/// ONE implementation for the agent and for the dashboard. The content is
/// written into the object database first: a review decides on BLOBS and §11.5
/// commits those blobs, so an edit whose content only exists on disk is an edit
/// the reviewer never sees and the commit never carries. Recording is therefore
/// not best-effort — a change that cannot be journalled is reported to its
/// caller, because the alternative is work that silently disappears.
pub fn record_work_edit(
    pool: &DbPool,
    broker: &Broker,
    session_id: &str,
    path: &str,
    kind: patch::EditKind,
    content: Option<&[u8]>,
    expect: &patch::Precondition,
) -> Result<()> {
    let Some(set) = patch::open_patch_set_for_scope(pool, session_id, &PatchScope::Work)? else {
        // Nothing is open, so the change is captured by the snapshot the next
        // review takes; there is no journal to fall behind.
        return Ok(());
    };
    let oid = match content {
        Some(bytes) => Some(broker.hash_object(&broker.reference(), bytes)?),
        None => None,
    };
    patch::record_edit(
        pool,
        broker,
        &set.id,
        path,
        kind,
        oid.as_deref(),
        expect,
    )
}

// =============================================================================
// Review — ONE implementation, two callers
// =============================================================================
//
// The `patch_review` block and PEP gate 5a on `core.git_commit` are the same
// mechanism seen from two places: a flow that wants the review at a fixed point
// calls the block, and a commit that arrives without an accepted set opens it
// on the spot. There is deliberately no second implementation, because two
// would eventually disagree about what "accepted" means.

/// What a finished review settled.
#[derive(Debug, Clone)]
pub struct ReviewReport {
    pub patch_set_id: String,
    pub status: String,
    pub accepted: Vec<String>,
    pub rejected: Vec<String>,
    /// Files whose accepted hunks no longer compose onto the base — the CAS
    /// conflict of §13.2. They are neither accepted nor silently dropped.
    pub conflicted: Vec<String>,
    pub timed_out: bool,
}

impl ReviewReport {
    pub fn accepted_anything(&self) -> bool {
        matches!(self.status.as_str(), "accepted" | "partially_accepted")
    }

    pub fn to_json(&self) -> Value {
        json!({
            "patch_set_id": self.patch_set_id,
            "status": self.status,
            "accepted": self.accepted,
            "rejected": self.rejected,
            "conflicted": self.conflicted,
            "timed_out": self.timed_out,
        })
    }
}

/// What to do when nobody answers in time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewTimeout {
    /// Reject the whole set — the safe default: silence is not consent.
    Reject,
    /// Leave the set open and report the timeout; the run continues without an
    /// accepted set, so a later commit re-opens the same question.
    Keep,
}

impl ReviewTimeout {
    pub fn from_slug(slug: &str) -> Self {
        match slug {
            "keep" => ReviewTimeout::Keep,
            _ => ReviewTimeout::Reject,
        }
    }
}

/// Runs one review to a decision. Opens (or reuses) the session's patch set,
/// renders the diff, asks, records the verdicts through `patch::decide`.
pub async fn run_review(
    pool: &DbPool,
    workspace_id: &str,
    session_id: &str,
    scope: &PatchScope,
    granularity: &str,
    decided_by: &str,
    gate: &dyn ApprovalGate,
    timeout: Duration,
    on_timeout: ReviewTimeout,
) -> Result<ReviewReport> {
    let pool_for_open = pool.clone();
    let workspace = workspace_id.to_string();
    let session = session_id.to_string();
    let scope_for_open = scope.clone();
    let set = tokio::task::spawn_blocking(move || -> Result<PatchSet> {
        let broker = Broker::for_workspace(&workspace)?;
        current_patch_set(&pool_for_open, &broker, &session, &scope_for_open)
    })
    .await
    .map_err(|e| anyhow!("patch set task failed: {e}"))??;

    if set.files.is_empty() {
        return Ok(ReviewReport {
            patch_set_id: set.id,
            status: "empty".to_string(),
            accepted: Vec::new(),
            rejected: Vec::new(),
            conflicted: Vec::new(),
            timed_out: false,
        });
    }

    let detail = render_patch_set(&set, workspace_id, session_id);
    let _ = events::append(
        pool,
        session_id,
        SessionEvent::new(
            format!("patch-open:{}", set.id),
            EventPayload::PatchSetOpened {
                patch_set_id: set.id.clone(),
                files: set.files.len() as u32,
            },
        ),
    );

    let answer = gate
        .present_review(&ReviewPrompt {
            patch_set_id: set.id.clone(),
            detail,
            granularity: granularity.to_string(),
            timeout,
        })
        .await;

    let (decisions, timed_out) = match answer {
        Some(raw) => (parse_review_answer(&raw, &set, decided_by), false),
        None => match on_timeout {
            ReviewTimeout::Reject => (reject_all(&set, decided_by), true),
            ReviewTimeout::Keep => {
                return Ok(ReviewReport {
                    patch_set_id: set.id,
                    status: "pending".to_string(),
                    accepted: Vec::new(),
                    rejected: Vec::new(),
                    conflicted: Vec::new(),
                    timed_out: true,
                })
            }
        },
    };

    let pool_for_decide = pool.clone();
    let workspace = workspace_id.to_string();
    let patch_set_id = set.id.clone();
    let outcome = tokio::task::spawn_blocking(move || -> Result<patch::DecisionOutcome> {
        let broker = Broker::for_workspace(&workspace)?;
        patch::decide(&pool_for_decide, &broker, &patch_set_id, &decisions)
    })
    .await
    .map_err(|e| anyhow!("patch decision task failed: {e}"))??;

    let _ = events::append(
        pool,
        session_id,
        SessionEvent::new(
            format!("patch-dec:{}", set.id),
            EventPayload::PatchDecided {
                patch_set_id: set.id.clone(),
                decision: outcome.status.clone(),
                decided_by: decided_by.to_string(),
            },
        ),
    );

    Ok(ReviewReport {
        patch_set_id: set.id,
        status: outcome.status,
        accepted: outcome.accepted,
        rejected: outcome.rejected,
        conflicted: outcome.conflicted,
        timed_out,
    })
}

/// Renders the change set for a human: one line per file, then the hunk headers
/// so a `path#0,2` answer has something to name.
fn render_patch_set(set: &PatchSet, workspace_id: &str, session_id: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{} file(s) changed on base {} (workspace {workspace_id}, session {session_id}):\n",
        set.files.len(),
        &set.base_commit[..set.base_commit.len().min(12)]
    ));
    for file in &set.files {
        out.push_str(&format!("\n[{}] {}\n", file.change_kind, file.path));
        for hunk in &file.hunks {
            out.push_str(&format!(
                "  #{} {}\n",
                hunk.idx,
                truncate(&redact::redact_text(&hunk.header), 160)
            ));
        }
    }
    out.push_str(
        "\nAnswer 'accept' to take everything, 'reject' to take nothing, or list the paths to \
         accept (comma separated). Append '#0,2' to a path to accept only those hunks.\n",
    );
    out
}

fn reject_all(set: &PatchSet, decided_by: &str) -> patch::Decisions {
    patch::Decisions {
        decided_by: decided_by.to_string(),
        files: set
            .files
            .iter()
            .map(|f| (f.path.clone(), patch::FileVerdict::Reject))
            .collect(),
    }
}

/// Turns the operator's answer into per-file verdicts. A path not named is
/// rejected: acceptance is always explicit, never the residue of an ambiguous
/// answer.
fn parse_review_answer(raw: &str, set: &PatchSet, decided_by: &str) -> patch::Decisions {
    let answer = raw.trim().to_lowercase();
    if answer == "accept" || answer == "accept all" {
        return patch::Decisions {
            decided_by: decided_by.to_string(),
            files: set
                .files
                .iter()
                .map(|f| (f.path.clone(), patch::FileVerdict::Accept))
                .collect(),
        };
    }
    if answer.is_empty() || answer == "reject" || answer == "reject all" {
        return reject_all(set, decided_by);
    }

    let mut verdicts: BTreeMap<String, patch::FileVerdict> = set
        .files
        .iter()
        .map(|f| (f.path.clone(), patch::FileVerdict::Reject))
        .collect();
    for token in raw.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let (path, hunks) = match token.split_once('#') {
            Some((path, list)) => {
                let idx: Vec<i64> = list
                    .split(|c: char| !c.is_ascii_digit())
                    .filter(|s| !s.is_empty())
                    .filter_map(|s| s.parse().ok())
                    .collect();
                (path.trim(), Some(idx))
            }
            None => (token, None),
        };
        // Only paths the set actually carries may be decided; anything else is
        // a typo, and silently inventing a file would be worse than ignoring it.
        if !verdicts.contains_key(path) {
            continue;
        }
        let verdict = match hunks {
            Some(idx) if !idx.is_empty() => patch::FileVerdict::Hunks(idx),
            _ => patch::FileVerdict::Accept,
        };
        verdicts.insert(path.to_string(), verdict);
    }
    patch::Decisions {
        decided_by: decided_by.to_string(),
        files: verdicts.into_iter().collect(),
    }
}

// =============================================================================
// Journal + events
// =============================================================================

fn begin_operation(
    ctx: &ToolCallCtx<'_>,
    bound: &Bound,
    tool: CoreToolName,
    input: OperationInput,
    precondition: Precondition,
    postcondition: Postcondition,
    // Sandbox profile the effect was authorized to run in, or `None` for an
    // effect that never enters a sandbox (§19 shows it per operation).
    profile: Option<pep::SandboxProfile>,
) -> Result<operations::Operation> {
    let op_kind = op_kind_of(tool)
        .ok_or_else(|| anyhow!("'{}' has no journaled effect", tool.public_name()))?;
    let capability =
        capability_of(tool).ok_or_else(|| anyhow!("'{}' has no capability", tool.public_name()))?;
    operations::begin(
        &bound.pool,
        &OperationRequest {
            workspace_id: bound.workspace.id.clone(),
            session_id: bound.session.id.clone(),
            run_id: ctx.run_id.map(str::to_string),
            origin_kind: OriginKind::ToolCall,
            origin_id: ctx.tool_call_id.to_string(),
            logical_step: tool.bare_name().to_string(),
            op_kind,
            capability,
            input,
            precondition,
            postcondition,
            profile,
        },
    )
}

/// Closes the journal entry of one effect.
///
/// `result_ref` is the artifact that HOLDS the outcome — a command's redacted
/// transcript, for instance. An operation whose postcondition is
/// `ExitCodeRecorded` cannot be completed without one, because "the exit code
/// was recorded" is a claim about something a person can read afterwards.
fn settle_operation(
    bound: &Bound,
    op: &operations::Operation,
    outcome: &Result<Value>,
    result_oids: &[String],
    result_ref: Option<&str>,
) -> Result<()> {
    match outcome {
        Ok(_) => {
            operations::complete(&bound.pool, &op.op_id, result_oids, result_ref)?;
        }
        Err(e) => {
            operations::fail(&bound.pool, &op.op_id, &redact::redact_text(&e.to_string()))?;
        }
    }
    Ok(())
}

fn emit_tool_event(bound: &Bound, ctx: &ToolCallCtx<'_>, ok: bool, summary: &str) {
    let mut event = SessionEvent::new(
        format!("tool-result:{}", ctx.tool_call_id),
        EventPayload::ToolResult {
            call_id: ctx.tool_call_id.to_string(),
            ok,
            summary: truncate(&redact::redact_text(summary), 512),
        },
    );
    if let Some(run_id) = ctx.run_id {
        event = event.with_run(run_id);
    }
    let _ = events::append(&bound.pool, &bound.session.id, event);
}

fn emit_git_event(
    bound: &Bound,
    op_id: &str,
    operation: GitOperation,
    outcome: &Result<Value>,
    remote: Option<String>,
) {
    let Ok(value) = outcome else { return };
    let _ = events::append(
        &bound.pool,
        &bound.session.id,
        SessionEvent::new(
            format!("git:{op_id}"),
            EventPayload::GitOp {
                op_id: op_id.to_string(),
                operation,
                refname: value
                    .get("branch")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                old_oid: value
                    .get("moved_from")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                new_oid: value
                    .get("commit")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                remote,
            },
        ),
    );
}

// =============================================================================
// Argument helpers
// =============================================================================

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

fn optional_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    str_arg(args, key).filter(|s| !s.is_empty())
}

fn require_str<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    str_arg(args, key).ok_or_else(|| anyhow!("missing required argument '{key}'"))
}

fn rel_path(args: &Value, key: &str) -> Result<RelPath> {
    let raw = require_str(args, key)?;
    RelPath::parse(raw).map_err(|e| anyhow!("{}", fs_error_text(&e)))
}

fn optional_rel_path(args: &Value, key: &str) -> Result<RelPath> {
    match optional_str(args, key) {
        Some(raw) => RelPath::parse(raw).map_err(|e| anyhow!("{}", fs_error_text(&e))),
        None => Ok(RelPath::root()),
    }
}

fn clamp_limit(args: &Value, default: usize, max: usize) -> usize {
    args.get("limit")
        .and_then(|v| v.as_u64())
        .filter(|n| *n > 0)
        .map(|n| n as usize)
        .unwrap_or(default)
        .min(max)
}

/// Renders a filesystem error as advice the model can act on. Host paths never
/// appear: the model works in repository-relative terms and nothing else.
fn fs_error_text(error: &fs::FsError) -> String {
    match error {
        fs::FsError::InvalidPath(detail) => {
            format!("path refused: {detail}. Use a path relative to the repository root.")
        }
        fs::FsError::InvalidRequest(detail) => detail.clone(),
        fs::FsError::NotFound => "no such file or directory in the repository".to_string(),
        fs::FsError::AlreadyExists => "that path already exists".to_string(),
        fs::FsError::NotADirectory => "that path is not a directory".to_string(),
        fs::FsError::IsADirectory => "that path is a directory".to_string(),
        fs::FsError::NotText => "that file is not text, so it cannot be read or edited".to_string(),
        fs::FsError::Conflict { expected, actual } => format!(
            "the file changed since you read it (expected {expected}, found {}). \
             Read it again and redo the edit on the current content.",
            actual.as_deref().unwrap_or("nothing")
        ),
        fs::FsError::AmbiguousEdit { excerpt, matches } => format!(
            "'{excerpt}' occurs {matches} times; include more surrounding lines so the edit \
             matches exactly once"
        ),
        fs::FsError::EditNotFound { excerpt } => {
            format!("'{excerpt}' does not occur in the file; read it again and match it verbatim")
        }
        fs::FsError::TooLarge { size, limit } => {
            format!("that file is {size} bytes, over the {limit}-byte limit")
        }
        fs::FsError::LimitExceeded(detail) => detail.clone(),
        fs::FsError::Denied(detail) => format!("refused: {detail}"),
        fs::FsError::Io(e) => format!("filesystem error: {e}"),
    }
}

fn summarize(value: &Value) -> String {
    truncate(&value.to_string(), 512)
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect::<String>() + "…"
}

/// Keeps one tool result inside the turn budget.
///
/// The cut lands on the widest string FIELD rather than in the middle of the
/// rendered JSON: a middle-out cut through JSON produces something the model
/// cannot parse, and a tool result it cannot read is worse than a short one.
/// The trim repeats until the rendered value fits, because marking the result
/// as truncated lengthens it again.
fn bound_result(mut value: Value) -> Value {
    if value.to_string().chars().count() <= MAX_RESULT_CHARS {
        return value;
    }
    if let Some(obj) = value.as_object_mut() {
        let widest = obj
            .iter()
            .filter_map(|(key, item)| item.as_str().map(|t| (key.clone(), t.chars().count())))
            .max_by_key(|(_, len)| *len);
        if let Some((key, _)) = widest {
            obj.insert("truncated".to_string(), Value::Bool(true));
            // Each pass removes the current overflow plus a margin; a handful of
            // passes converge even though the trim itself shifts the length.
            for _ in 0..8 {
                let rendered = Value::Object(obj.clone()).to_string().chars().count();
                if rendered <= MAX_RESULT_CHARS {
                    break;
                }
                let overflow = rendered - MAX_RESULT_CHARS;
                let text = match obj.get(&key).and_then(|v| v.as_str()) {
                    Some(text) => text.to_string(),
                    None => break,
                };
                let keep = text.chars().count().saturating_sub(overflow + 64);
                obj.insert(
                    key.clone(),
                    Value::String(text.chars().take(keep).collect::<String>()),
                );
                if keep == 0 {
                    break;
                }
            }
        }
    }
    value
}

/// Best-effort path of the session worktree, for callers that need to show it.
pub fn session_worktree(workspace_id: &str, session_id: &str) -> Result<PathBuf> {
    super::paths::session_worktree_dir(workspace_id, session_id)
}

// =============================================================================
// Harness versioning (§16.6)
// =============================================================================
//
// `flow_versions` and the List/Get/Restore handlers already exist. What Phase 5
// adds is the part that keeps a SESSION honest: a session is pinned to one
// version for its whole life, restoring the factory graph is a new version
// rather than an overwrite, and a version that does not compile never becomes
// the active one.

/// Compiles a candidate graph. This is the gate, not a formality: an active
/// harness that does not compile turns every new session into a runtime error
/// at the worst possible moment, so the check happens BEFORE activation.
pub fn assert_compiles(
    flow_id: &str,
    flow_json: &str,
    registry: &crate::flow_engine::node_adapter::AdapterRegistry,
) -> Result<()> {
    crate::flow_engine::cache::CompiledFlow::from_json(flow_id, flow_json, registry)
        .map(|_| ())
        .map_err(|e| anyhow!("flow '{flow_id}' does not compile: {e}"))
}

/// Activates a stored version as the flow's current graph. Refuses an
/// uncompilable version instead of writing it and discovering the problem when
/// somebody opens a session.
pub fn activate_flow_version(
    db: &DbPool,
    flow_id: &str,
    version_id: &str,
    registry: &crate::flow_engine::node_adapter::AdapterRegistry,
) -> Result<()> {
    let version = crate::db::repository::get_flow_version(db, flow_id, version_id)?
        .ok_or_else(|| anyhow!("flow '{flow_id}' has no version '{version_id}'"))?;
    let flow_json = version
        .flow_json
        .ok_or_else(|| anyhow!("version '{version_id}' carries no graph"))?;
    assert_compiles(flow_id, &flow_json, registry)?;
    let conn = db.write().map_err(|e| anyhow!("core db write: {e}"))?;
    conn.execute(
        "UPDATE flows SET flow_json = ?2, updated_at = datetime('now') WHERE id = ?1",
        rusqlite::params![flow_id, flow_json],
    )?;
    Ok(())
}

/// Factory restore: writes the seeded graph as a NEW version and activates it.
///
/// A restore is deliberately additive. Overwriting the current graph would
/// destroy whatever the operator had — including the edit they are trying to
/// undo, which they may still want to read — so the pristine graph enters the
/// history like any other version and the old one stays listed.
pub fn restore_factory_version(
    db: &DbPool,
    flow_id: &str,
    factory_json: &str,
    name: &str,
    actor_user_id: Option<&str>,
    registry: &crate::flow_engine::node_adapter::AdapterRegistry,
) -> Result<String> {
    assert_compiles(flow_id, factory_json, registry)?;
    let version_id = uuid::Uuid::new_v4().to_string();
    {
        let mut conn = db.write().map_err(|e| anyhow!("core db write: {e}"))?;
        let tx = conn.transaction()?;
        let next: i64 = tx.query_row(
            "SELECT COALESCE(MAX(version_num), 0) + 1 FROM flow_versions WHERE flow_id = ?1",
            rusqlite::params![flow_id],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO flow_versions \
                (id, flow_id, version_num, flow_json, name, description, status, created_by) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'factory restore', 'active', ?6)",
            rusqlite::params![version_id, flow_id, next, factory_json, name, actor_user_id],
        )?;
        tx.execute(
            "UPDATE flows SET flow_json = ?2, updated_at = datetime('now') WHERE id = ?1",
            rusqlite::params![flow_id, factory_json],
        )?;
        tx.commit()?;
    }
    Ok(version_id)
}

/// The graph a session actually runs.
///
/// A session is pinned at open time (`sessions.flow_version_id`) and keeps that
/// graph for its whole life: editing the harness mid-session would change the
/// rules under a conversation that is already in flight, and the timeline would
/// no longer explain itself. The live flow is used only when the pinned version
/// is gone (pruned by the 5-version window), and that fallback is reported so
/// the caller can say which graph it ran.
pub struct ResolvedFlow {
    pub flow_json: String,
    pub version_id: Option<String>,
    /// True when the pinned version no longer exists and the live graph was
    /// used instead.
    pub fell_back_to_live: bool,
}

pub fn resolve_session_flow(db: &DbPool, session: &SessionRecord) -> Result<ResolvedFlow> {
    if !session.flow_version_id.is_empty() {
        if let Some(version) =
            crate::db::repository::get_flow_version(db, &session.flow_id, &session.flow_version_id)?
        {
            if let Some(flow_json) = version.flow_json {
                return Ok(ResolvedFlow {
                    flow_json,
                    version_id: Some(version.id),
                    fell_back_to_live: false,
                });
            }
        }
    }
    let flow = crate::db::repository::get_flow(db, &session.flow_id)?
        .ok_or_else(|| anyhow!("session flow '{}' no longer exists", session.flow_id))?;
    Ok(ResolvedFlow {
        flow_json: flow.flow_json,
        version_id: None,
        fell_back_to_live: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_round_trips_and_rejects_malformed_meta() {
        let mut meta: BTreeMap<String, Value> = BTreeMap::new();
        assert!(binding_from_meta(&meta).is_none());
        meta.insert(
            SESSION_META_KEY.to_string(),
            binding_meta_value("ws-1", "sess-1"),
        );
        assert_eq!(
            binding_from_meta(&meta),
            Some(SessionBinding {
                workspace_id: "ws-1".into(),
                session_id: "sess-1".into(),
            })
        );
        // A half-filled binding is not a binding: the tool must refuse rather
        // than fall back to "some session".
        meta.insert(
            SESSION_META_KEY.to_string(),
            json!({"workspace_id": "ws-1", "session_id": ""}),
        );
        assert!(binding_from_meta(&meta).is_none());
    }

    #[test]
    fn every_code_studio_tool_has_a_capability_and_reads_have_no_operation() {
        for tool in CoreToolName::all().iter().copied() {
            if !tool.is_code_studio() {
                assert!(capability_of(tool).is_none(), "{}", tool.public_name());
                continue;
            }
            assert!(
                capability_of(tool).is_some(),
                "{} has no capability",
                tool.public_name()
            );
        }
        // Reads leave no effect, so they journal no operation.
        for read in [
            CoreToolName::FsRead,
            CoreToolName::FsList,
            CoreToolName::FsGlob,
            CoreToolName::FsGrep,
            CoreToolName::CodeSearch,
            CoreToolName::GitRead,
            CoreToolName::WorkspaceInfo,
        ] {
            assert!(op_kind_of(read).is_none(), "{}", read.public_name());
        }
        // Every effectful verb does.
        for effect in [
            CoreToolName::FsWrite,
            CoreToolName::FsEdit,
            CoreToolName::FsMove,
            CoreToolName::FsDelete,
            CoreToolName::FsMkdir,
            CoreToolName::Exec,
            CoreToolName::GitCommit,
            CoreToolName::GitPush,
            CoreToolName::GitMerge,
            CoreToolName::GitMergeFinalize,
        ] {
            assert!(op_kind_of(effect).is_some(), "{}", effect.public_name());
        }
    }

    #[test]
    fn a_degraded_search_sends_the_model_back_to_grep() {
        use super::super::index::CodeHit;

        // §14: an index that does not describe the current head answers with
        // leads, and an empty degraded answer says nothing about the code at
        // all — so the result must carry the fallback, not just a flag.
        let degraded = code_search_result(&index_unwired_outcome());
        assert_eq!(degraded["degraded"], true);
        assert_eq!(degraded["hits"].as_array().expect("hits array").len(), 0);
        assert_eq!(degraded["reason"], "index_unavailable_on_this_node");
        assert_eq!(degraded["authoritative_tool"], "core.fs_grep");
        assert!(degraded["fallback"]
            .as_str()
            .expect("fallback instruction")
            .contains("core.fs_grep"));

        let stale = code_search_result(&CodeSearchOutcome {
            hits: vec![CodeHit {
                path: "src/main.rs".to_string(),
                start_line: 10,
                end_line: 24,
                score: 0.71,
                snippet: "fn main() {}".to_string(),
                lang: "rust".to_string(),
                commit: "deadbeef".to_string(),
                branch: "work".to_string(),
            }],
            degraded: true,
            reason: Some("index_behind_head".to_string()),
        });
        assert_eq!(stale["reason"], "index_behind_head");
        assert!(stale["fallback"].is_string());
        assert_eq!(stale["hits"][0]["path"], "src/main.rs");
        assert_eq!(stale["hits"][0]["start_line"], 10);

        // A healthy answer still names the authoritative tool, but carries no
        // instruction to search again — there is nothing to correct for.
        let healthy = code_search_result(&CodeSearchOutcome {
            hits: Vec::new(),
            degraded: false,
            reason: None,
        });
        assert_eq!(healthy["degraded"], false);
        assert!(healthy.get("fallback").is_none());
        assert!(healthy.get("reason").is_none());
        assert_eq!(healthy["authoritative_tool"], "core.fs_grep");
    }

    #[test]
    fn a_search_prefix_outside_the_worktree_is_refused_like_any_other_path() {
        // The prefix is a path the model named, so it is bounded by the same
        // containment check as every fs argument (§9.3 step 4).
        assert!(parses_inside(&json!({"prefix": "src/"}), "prefix"));
        assert!(!parses_inside(&json!({"prefix": "../etc"}), "prefix"));
        assert_eq!(
            target_label(CoreToolName::CodeSearch, &json!({"prefix": "src/"})),
            Some("src/".to_string())
        );
        // Without a prefix the search spans the repository, so only a `*` grant
        // can cover it.
        assert_eq!(
            target_label(CoreToolName::CodeSearch, &json!({"query": "retry budget"})),
            None
        );
    }

    /// §9.1: one saved consent, one meaning — whoever is asking.
    ///
    /// `dispatch::code_studio` reads standing permissions through these very
    /// functions, so the row an operator's answer produced is read back the
    /// same way for the operator's own calls and for the model's. The two paths
    /// used to carry a private matcher each, and they disagreed on exactly the
    /// case a capability without a concrete target produces: the question was
    /// persisted with an empty `target_pattern`, the dashboard's matcher read
    /// `""` against `""` as a hit, and the agent's read it as a miss. The same
    /// grant authorized one caller and was invisible to the other.
    #[test]
    fn one_saved_grant_answers_the_same_for_the_operator_and_for_the_model() {
        use crate::code_studio::models::{
            AutonomyMode as Mode, EgressEnforcement, ExecMode, NewWorkspace,
        };

        let dir = tempfile::tempdir().expect("tempdir");
        let db = crate::db::init(&dir.path().join("tentaflow.db")).expect("init db");
        repository::create_workspace(
            &db,
            &NewWorkspace {
                id: "ws-1".into(),
                org_id: "org-1".into(),
                owner_user_id: "u-owner".into(),
                name: "Workspace".into(),
                slug: "workspace".into(),
                node_id: "node-1".into(),
                exec_mode: ExecMode::TrustedNative,
                container_image: None,
                egress_enforcement: EgressEnforcement::Unrestricted,
                repo_kind: "git".into(),
                repo_url: None,
                repo_auth_kind: Some("none".into()),
                secret_ref: None,
                ssh_host_fingerprint: None,
                default_branch: Some("main".into()),
                target_branch: None,
                autonomy_ceiling: Mode::Autonomous,
                egress_policy: "org_approved".into(),
                index_enabled: false,
                quota_disk_bytes: None,
                quota_sessions: None,
            },
        )
        .expect("create workspace");

        // A question with no concrete target is stored as `*` — never as the
        // empty string, which no reader gives a meaning to.
        let blanket = pep::grant_pattern(None);
        assert_eq!(blanket, "*");
        repository::add_allowlist_entry(&db, "ws-1", Capability::GitRead.slug(), blanket, "u-owner")
            .expect("store the blanket grant");
        assert!(allowlist_holds(&db, "ws-1", Capability::GitRead, None).expect("read"));
        assert!(
            allowlist_holds(&db, "ws-1", Capability::GitRead, Some("src/main.rs")).expect("read")
        );

        // A grant written from a named target stays that narrow, and covers
        // neither another target nor a call that names none.
        repository::add_allowlist_entry(&db, "ws-1", Capability::Exec.slug(), "cargo", "u-owner")
            .expect("store the narrow grant");
        assert!(allowlist_holds(&db, "ws-1", Capability::Exec, Some("cargo")).expect("read"));
        assert!(!allowlist_holds(&db, "ws-1", Capability::Exec, Some("curl")).expect("read"));
        assert!(!allowlist_holds(&db, "ws-1", Capability::Exec, None).expect("read"));

        // And the meaningless pattern never reaches the table in the first
        // place, so no reader ever has to invent a reading for it.
        assert!(
            repository::add_allowlist_entry(&db, "ws-1", Capability::Exec.slug(), "", "u-owner")
                .is_err(),
            "an empty pattern was stored as a standing grant"
        );
    }

    /// §11.4 on the path that can actually be talked into it.
    ///
    /// `git_push` and `git_sync` take their remote from the MODEL's arguments,
    /// so the address the broker dials is the model's choice. The PEP used to be
    /// handed `Target::Branch` for both, which carries no address at all: rule
    /// 5b never fired, and a standing `git_network:*` grant covered a LAN target
    /// the operator was supposed to be asked about every single time.
    #[tokio::test]
    async fn adversarial_a_model_chosen_private_remote_is_not_covered_by_a_standing_grant() {
        let _guard = super::super::paths::test_data_dir_guard();
        let data = tempfile::tempdir().expect("data dir");
        crate::paths::set_category_override(
            crate::paths::StorageCategory::Data,
            Some(data.path().to_string_lossy().to_string()),
        );
        let bound = test_bound(data.path());

        // The model names a private address outright, so no repository config
        // and no DNS answer can be blamed for it.
        let target = pep_target(
            &bound,
            CoreToolName::GitSync,
            &json!({"operation": "fetch", "remote": "https://192.168.7.7/repo.git"}),
        )
        .await
        .expect("resolve the remote the broker would dial");
        assert!(
            matches!(target, pep::Target::Remote { is_private: true }),
            "the PEP was handed {target:?} instead of the address it must judge"
        );

        // Everything a workspace can standingly permit, and the mode that asks
        // for the least: whatever refuses below comes from the TARGET alone.
        let ctx = pep::SessionCtx {
            role: WorkspaceRole::Owner,
            autonomy: AutonomyMode::Autonomous,
            is_coordinator: false,
            has_accepted_patch_set: true,
            allowlisted: true,
            session_granted: true,
            run_granted: true,
        };
        match pep::authorize(&ctx, Capability::GitNetwork, &target) {
            Decision::AskUser { kind, .. } => assert_eq!(kind, AskKind::Permission),
            other => panic!("a standing grant covered a private remote: {other:?}"),
        }

        // A public one stays at the capability's own threshold, so the rule
        // above is the address speaking and not a blanket refusal of remotes.
        let public = pep_target(
            &bound,
            CoreToolName::GitSync,
            &json!({"operation": "fetch", "remote": "https://93.184.216.34/repo.git"}),
        )
        .await
        .expect("resolve a public remote");
        assert!(matches!(
            pep::authorize(&ctx, Capability::GitNetwork, &public),
            Decision::Allow(_)
        ));

        crate::paths::set_category_override(crate::paths::StorageCategory::Data, None);
    }

    /// A `Bound` over a temporary workspace root. Nothing here touches git: the
    /// broker resolves a url-shaped remote without opening a repository.
    fn test_bound(_data_dir: &std::path::Path) -> Bound {
        let dir = super::super::paths::workspace_dir("ws-remote").expect("workspace dir");
        std::fs::create_dir_all(&dir).expect("workspace layout");
        let (pool, _) = workspace_db::open_pool_at(&dir).expect("workspace.db");
        Bound {
            workspace: WorkspaceRecord {
                id: "ws-remote".into(),
                org_id: "org-1".into(),
                owner_user_id: "u-owner".into(),
                name: "Workspace".into(),
                slug: "workspace".into(),
                node_id: "node-1".into(),
                exec_mode: "trusted_native".into(),
                container_image: None,
                egress_enforcement: "unrestricted".into(),
                repo_kind: "git".into(),
                repo_url: Some("https://example.invalid/repo.git".into()),
                repo_auth_kind: Some("none".into()),
                secret_ref: None,
                ssh_host_fingerprint: None,
                default_branch: Some("main".into()),
                target_branch: None,
                autonomy_ceiling: "autonomous".into(),
                egress_policy: "org_approved".into(),
                index_enabled: false,
                quota_disk_bytes: None,
                quota_sessions: None,
                status: "active".into(),
                status_detail: None,
                created_at: "now".into(),
                updated_at: "now".into(),
            },
            session: SessionRecord {
                id: "sess-remote".into(),
                workspace_id: "ws-remote".into(),
                user_id: "u-owner".into(),
                title: "Session".into(),
                branch: "cs/u/1".into(),
                autonomy_mode: "autonomous".into(),
                flow_id: "flow".into(),
                flow_version_id: "v1".into(),
                status: "idle".into(),
                created_at: "now".into(),
                updated_at: "now".into(),
                closed_at: None,
            },
            role: WorkspaceRole::Owner,
            autonomy: AutonomyMode::Autonomous,
            pool,
            broker: Broker::for_workspace("ws-remote").expect("broker"),
        }
    }

    #[test]
    fn mandatory_interactive_capabilities_refuse_standing_grants() {
        for cap in [
            Capability::GitPush,
            Capability::GitMerge,
            Capability::GitMergeFinalize,
            Capability::SecretManage,
        ] {
            assert!(
                !pep::may_store_always_grant(cap),
                "{} must not be storable",
                cap.slug()
            );
        }
        assert!(pep::may_store_always_grant(Capability::FsWrite));
    }

    #[test]
    fn adversarial_an_operator_approved_push_can_never_reach_the_broker() {
        // `execute` handles `Decision::AskUser { kind: Permission }` by asking
        // the operator, persisting the grant, and then RE-DECIDING with exactly
        // the context below. Anything other than `Allow` from that second call
        // becomes a `[TOOL_ERROR]`.
        //
        // It used to re-run `pep::authorize`, and step 5 of §9.3 answers
        // `AskUser` for a mandatory-interactive capability whatever is stored —
        // by design — so `git_push`, `git_merge` and `git_merge_finalize` were
        // refused even after the human said yes, and the §11 push/merge path was
        // unreachable from a tool call. The dashboard path did it right through
        // a SECOND, divergent copy of the rule (`granted_profile`).
        //
        // The rule now lives in ONE place, `pep::authorize_after_decision`, and
        // this test pins the composition both callers depend on: the question is
        // still asked every time, and the answer still resolves to a profile.
        let approved = pep::SessionCtx {
            role: WorkspaceRole::Owner,
            autonomy: AutonomyMode::Normal,
            is_coordinator: false,
            has_accepted_patch_set: true,
            // exactly what `execute` passes on the second call
            allowlisted: true,
            session_granted: false,
            run_granted: false,
        };
        let target = pep::Target::Branch {
            is_session_branch: true,
        };
        for cap in [
            Capability::GitPush,
            Capability::GitMerge,
            Capability::GitMergeFinalize,
        ] {
            assert!(
                matches!(
                    pep::authorize(&approved, cap, &target),
                    Decision::AskUser { .. }
                ),
                "{} stopped asking the operator",
                cap.slug()
            );
            match pep::authorize_after_decision(&approved, cap, &target) {
                Decision::Allow(_) => {}
                other => panic!(
                    "{} is still refused after the operator approved it: {other:?}",
                    cap.slug()
                ),
            }
        }
    }

    #[test]
    fn oversized_result_is_cut_to_the_budget_and_stays_parseable() {
        let value = json!({
            "stdout": "x".repeat(MAX_RESULT_CHARS * 2),
            "exit_code": 0,
        });
        let bounded = bound_result(value);
        assert!(bounded.to_string().chars().count() <= MAX_RESULT_CHARS);
        assert_eq!(bounded["exit_code"], 0);
        assert_eq!(bounded["truncated"], true);
    }
}
