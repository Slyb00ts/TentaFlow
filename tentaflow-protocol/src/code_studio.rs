// =============================================================================
// File: code_studio.rs
// Purpose: Binary CBOR protocol for Code Studio — the workspace registry
//          (create, list, get, members, creator grants), work sessions
//          (open, list, close) and everything a session needs afterwards:
//          filesystem, git broker, patch sets and review, timeline, operation
//          journal, approvals and grants, runs, exec and terminal, workspace
//          settings and the semantic index. Sessions are private per user: the
//          server filters every session query by the authenticated caller, so
//          the wire never carries another person's unfinished work.
// Example: MessageBody::CodeStudioBody(CodeStudioPayload::WorkspacesListRequest {})
// =============================================================================

use serde::{Deserialize, Serialize};

/// Workspace row for the list and detail views.
///
/// `secret_ref` is deliberately ABSENT from the wire: it is a handle into the
/// node-local vault and means nothing on another node. `has_secret` is what the
/// UI actually needs — whether credentials are stored at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub workspace_id: String,
    pub name: String,
    pub slug: String,
    /// Node that OWNS the workspace. Only that node can run it; the others
    /// show it and say so.
    pub node_id: String,
    pub node_name: String,
    /// True when this Core is the owner node.
    pub is_local: bool,
    /// 'container' | 'trusted_native'. The native mode promises NO isolation
    /// from the host, so the UI marks it permanently.
    pub exec_mode: String,
    /// 'namespace' | 'firewall' | 'unrestricted' — how network policy is
    /// REALLY enforced, not what was requested.
    pub egress_enforcement: String,
    /// 'empty' | 'git'.
    pub repo_kind: String,
    pub repo_url: Option<String>,
    /// 'none' | 'token' | 'ssh_key'.
    pub repo_auth_kind: Option<String>,
    pub has_secret: bool,
    pub default_branch: Option<String>,
    pub target_branch: Option<String>,
    pub autonomy_ceiling: String,
    pub egress_policy: String,
    pub index_enabled: bool,
    /// 'provisioning' | 'active' | 'error' | 'archived'.
    pub status: String,
    /// Reason, present only for `error`.
    pub status_detail: Option<String>,
    /// Role of the calling user in this workspace.
    pub my_role: String,
    pub member_count: u32,
    pub open_sessions: u32,
    pub disk_used_bytes: u64,
    pub quota_disk_bytes: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
    /// Session ceiling of the workspace. `WorkspaceSettingsUpdateRequest` takes
    /// it, so the settings form has to be able to READ it back — otherwise it
    /// posts a value it never saw and silently resets whatever an administrator
    /// configured elsewhere.
    #[serde(default)]
    pub quota_sessions: Option<i64>,
    /// Sessions of the CALLER sitting in `waiting_user`. Not derivable from
    /// `open_sessions`: "how many are running" and "how many are blocked on a
    /// question from me" are different facts, and the second is what the
    /// dashboard KPI and the workspace tile count.
    #[serde(default)]
    pub sessions_waiting: u32,
}

/// Workspace member with display data resolved server-side.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceMemberInfo {
    pub user_id: String,
    pub display_name: String,
    /// 'owner' | 'editor' | 'viewer'.
    pub role: String,
    pub added_by: String,
    pub added_at: String,
}

/// Member entry as sent by the creation wizard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceMemberInput {
    pub user_id: String,
    pub role: String,
}

/// One provisioning step, so a failed workspace can show WHERE it stopped
/// instead of a bare error.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvisionStepInfo {
    pub step: String,
    /// 'pending' | 'done' | 'failed' | 'compensated'.
    pub status: String,
    pub detail: Option<String>,
    pub updated_at: String,
}

/// A work session on a branch of a workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub session_id: String,
    pub workspace_id: String,
    pub title: String,
    pub branch: String,
    pub autonomy_mode: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    /// Harness flow the session runs, and the version it was PINNED to when it
    /// opened (§16.6). Both travel so the UI can open the flow in the Flow
    /// Builder at the shape this session actually executes, not at the latest
    /// edit.
    #[serde(default)]
    pub flow_id: String,
    #[serde(default)]
    pub flow_version_id: String,
}

/// Node that can host a workspace, for the wizard's node picker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceNodeInfo {
    pub node_id: String,
    pub name: String,
    pub is_local: bool,
    /// Whether this node can run a container-isolated workspace at all.
    pub supports_container: bool,
    /// How this node would enforce egress policy.
    pub egress_enforcement: String,
}

/// One entry of a worktree directory listing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileEntryInfo {
    /// Always relative to the worktree root — a host path would leak the
    /// owner node's layout to every member of the workspace.
    pub path: String,
    /// 'file' | 'dir'.
    pub kind: String,
    pub size: u64,
    /// A symlink is listed but never followed by the executor, so the UI has
    /// to be able to mark it.
    pub is_symlink: bool,
}

/// One grep match. `column` is a character offset, not a byte offset, so the
/// editor can place the caret without re-decoding UTF-8.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrepHitInfo {
    pub path: String,
    pub line: u32,
    pub column: u32,
    pub text: String,
}

/// One `git status` row. Index and worktree state are separate letters, the
/// same split porcelain uses, because a file can be staged and modified again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitStatusEntry {
    pub path: String,
    pub index_status: String,
    pub worktree_status: String,
    /// Set for renames; the previous path as git resolved it.
    pub old_path: Option<String>,
}

/// One commit of the session branch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitCommitInfo {
    pub oid: String,
    pub short_oid: String,
    pub author: String,
    pub date: String,
    pub subject: String,
}

/// Branch row. `is_session` marks the `cs/<user>/<session>` branch the caller's
/// session works on, so the UI never offers to delete the branch under itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitBranchInfo {
    pub name: String,
    pub is_current: bool,
    pub is_session: bool,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

/// One diff hunk as produced by the broker. `content` is the unified-diff body
/// WITHOUT the header line, which travels separately in `header`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiffHunkInfo {
    pub idx: u32,
    pub header: String,
    pub content: String,
}

/// Worktree of a session. The on-disk path is deliberately absent: it is a host
/// path and means nothing to the browser.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub worktree_id: String,
    pub session_id: String,
    /// 'work' | 'integration'.
    pub purpose: String,
    /// Set for 'integration': the merge operation this worktree belongs to.
    pub op_id: Option<String>,
    /// NULL for an integration worktree — it is detached on purpose, so a merge
    /// cannot move the target branch before review.
    pub branch: Option<String>,
    pub head_commit: String,
    pub base_commit: String,
    /// 'creating' | 'ready' | 'dirty' | 'clean' | 'held' | 'detaching' | 'removed'.
    pub state: String,
    pub created_at: String,
    /// Paths the merge stopped on, for an integration worktree left in `held`
    /// (§11.6 pkt 3). The merge answer names them once; this is where they are
    /// read back from, because a reload has no other way to learn them and a
    /// conflict outlives the connection that produced it.
    #[serde(default)]
    pub conflict_files: Vec<String>,
}

/// Patch set header — the unit a human accepts or rejects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatchSetInfo {
    pub patch_set_id: String,
    pub session_id: String,
    pub run_id: Option<String>,
    /// 'work' | 'merge'.
    pub scope: String,
    pub base_commit: String,
    /// 'open' | 'in_review' | 'accepted' | 'partially_accepted' | 'rejected'
    /// | 'superseded' | 'conflicted'.
    pub status: String,
    pub file_count: u32,
    pub created_at: String,
    pub decided_by: Option<String>,
    pub decided_at: Option<String>,
    /// The merge operation a set of scope 'merge' belongs to; `None` for scope
    /// 'work'. Without it a reader can only guess which merge a set decides by
    /// taking the newest one, and two merges of one session are ordinary — a
    /// first attempt left `held` after a conflict and a second one started
    /// after the target moved.
    #[serde(default)]
    pub op_id: Option<String>,
}

/// One hunk of a patch file, addressable so a decision can be per hunk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatchHunkInfo {
    pub patch_hunk_id: String,
    pub idx: u32,
    /// The `@@ -a,b +c,d @@` line. Positioning metadata, and the ONLY place it
    /// travels: `content` starts at the first line of the body.
    pub header: String,
    /// The hunk body WITHOUT its `@@` line — every row is a real diff row
    /// (` `, `+`, `-`, `\`). Repeating the header here made the first body row
    /// a header a renderer had to recognise and drop, and dropping it wrongly
    /// shifted every line number in the hunk.
    pub content: String,
    /// 'pending' | 'accepted' | 'rejected'.
    pub status: String,
}

/// One file of a patch set. The three blob hashes are the CAS contract (§13.2):
/// `patch_base_blob_sha` is frozen at open, `current_blob_sha` moves with every
/// edit, `accepted_blob_sha` is what review settled on and what the commit is
/// built from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatchFileInfo {
    pub patch_file_id: String,
    pub path: String,
    pub old_path: Option<String>,
    /// 'add' | 'modify' | 'delete' | 'rename'.
    pub change_kind: String,
    /// 'pending' | 'accepted' | 'partially_accepted' | 'rejected' | 'conflicted'.
    pub status: String,
    pub patch_base_blob_sha: Option<String>,
    pub current_blob_sha: Option<String>,
    pub accepted_blob_sha: Option<String>,
    pub mode: String,
    pub hunks: Vec<PatchHunkInfo>,
}

/// Reviewer's verdict on a single hunk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatchHunkDecision {
    pub patch_hunk_id: String,
    /// 'accept' | 'reject'.
    pub decision: String,
}

/// Reviewer's verdict on a single file. `request_revision` is a third outcome
/// next to accept/reject: the change is neither taken nor thrown away, the note
/// goes back to the agent as the trigger of a revision run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PatchFileDecision {
    pub patch_file_id: String,
    /// 'accept' | 'reject' | 'request_revision'.
    pub decision: String,
    pub note: Option<String>,
    /// Empty means the file-level decision applies to every hunk.
    #[serde(default)]
    pub hunks: Vec<PatchHunkDecision>,
}

/// One event of the session timeline. `payload_json` is the event body rendered
/// as JSON for the UI; the authoritative form stays CBOR in `session_events`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimelineEventInfo {
    pub seq: u64,
    pub event_id: String,
    pub kind: String,
    pub run_id: Option<String>,
    pub agent_id: Option<String>,
    pub created_at: String,
    pub payload_json: String,
    pub security_relevant: bool,
}

/// One row of the operation journal. The mounting and network profile travels
/// with it because "what was allowed" is meaningless without "in which profile
/// it ran".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationInfo {
    pub op_id: String,
    pub run_id: Option<String>,
    /// 'tool_call' | 'terminal' | 'ui' | 'shim' | 'flow_block' | 'coordinator'.
    pub origin_kind: String,
    /// fs_write | fs_delete | fs_rename | exec | git_commit | …
    pub op_kind: String,
    pub capability: String,
    pub idempotent: bool,
    /// 'pending' | 'completed' | 'failed' | 'unknown'.
    pub status: String,
    pub error: Option<String>,
    /// 'ro' | 'cow' | 'rw'; absent for operations that never enter a sandbox.
    pub mount_access: Option<String>,
    /// 'none' | 'gateway'.
    pub network_access: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
}

/// A pending or decided permission question.
///
/// An operation the PEP suspends is answered ONE way across this family: a
/// `Conflict` error whose message is `approval_required:<approval_id>: <summary>`
/// — never a successful body carrying a status. A suspended call journals no
/// operation, so a response shaped around `op_id` would have to invent one, and
/// half the family (`fs_write`, `exec`, `review_decide`, the secret verbs) has
/// no status field to say it in at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalInfo {
    pub approval_id: String,
    pub session_id: String,
    pub run_id: Option<String>,
    pub capability: String,
    pub summary: String,
    pub detail: Option<String>,
    pub target_digest: String,
    /// 'pending' | 'decided' | 'expired' | 'abandoned'.
    pub status: String,
    /// 'allow_once' | 'allow_for_run' | 'allow_for_session' | 'always' | 'deny'.
    pub decision: Option<String>,
    /// True for `git_push`, `git_merge`, `git_merge_finalize` and
    /// `secret_manage` — the UI must say the question cannot be switched off,
    /// and `always` is refused server-side for them.
    pub mandatory_interactive: bool,
    pub requested_at: String,
    pub decided_at: Option<String>,
    pub decided_by: Option<String>,
    /// The patch set this question is about, for the review gate (§9.3 step 5a).
    /// Two concurrent sets are ordinary — a work set and a merge set can be open
    /// at once — so the approval has to name WHICH one it decides.
    #[serde(default)]
    pub patch_set_id: Option<String>,
}

/// A standing permission: either a session grant or a workspace allowlist row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GrantInfo {
    pub capability: String,
    pub pattern: String,
    /// 'session' | 'workspace'.
    pub scope: String,
    pub granted_by: String,
    pub created_at: String,
}

/// Workspace-level `always` entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AllowlistEntryInfo {
    pub entry_id: i64,
    pub capability: String,
    pub pattern: String,
    pub created_by: String,
    pub created_at: String,
}

/// One run of the session chain. `trigger` is why the run exists at all, which
/// is what makes a revision chain readable instead of a flat list.

/// One task of a session's plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskInfo {
    pub ordinal: u32,
    pub title: String,
    pub detail: String,
    /// `pending` | `in_progress` | `done` | `blocked`.
    pub status: String,
    /// Why it is blocked, or why it was reopened.
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunInfo {
    pub run_id: String,
    pub ordinal: u32,
    /// 'root' | 'subagent' | 'cli' | 'revision'.
    pub kind: String,
    /// 'user' | 'agent_spawn' | 'cli_delegate' | 'review_rejected'
    /// | 'test_failed' | 'merge_conflict' | 'merge_verify_failed' | 'resume'.
    pub trigger: String,
    pub parent_run_id: Option<String>,
    pub agent_id: Option<String>,
    pub status: String,
    /// Why this run is in the state it is: a review note for a run a person
    /// triggered, and for a run that ended badly the failure reason the
    /// timeline recorded. `session_runs` has no column for either, so the
    /// server reads it back out of the `run_finished` event — without it a
    /// failed run reads as a bare 'failed' and the reason is only in the
    /// database.
    pub note: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    /// Token accounting of the run (§17.3). Measured where the tokens are spent
    /// — `AiGateway` for the harness, the adapter for a delegated CLI — which is
    /// what makes a ticket budget enforceable instead of self-reported.
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    /// Model the run actually addressed; absent while the run has not resolved
    /// one yet.
    #[serde(default)]
    pub model: Option<String>,
    /// What the PROVIDER said the turn cost, in USD. `None` whenever nobody
    /// stated a price — which is every run we meter ourselves, because the node
    /// has no price feed and a figure derived from tokens would read as measured
    /// while being a guess.
    #[serde(default)]
    pub cost_usd: Option<f64>,
}

/// One row of the server-side VT grid. `text` holds the row's characters and
/// `attrs` one packed attribute word per character (foreground, background and
/// style flags), so a row travels as two values instead of a cell per object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalCellRow {
    pub row: u32,
    pub text: String,
    pub attrs: Vec<u32>,
}

/// Index state of one branch. A drift between `indexed_commit` and the branch
/// head is a soft degradation, not an error, so the UI needs both.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexStateInfo {
    pub branch: String,
    pub indexed_commit: Option<String>,
    pub files: u32,
    pub chunks: u32,
    pub updated_at: Option<String>,
    pub last_error: Option<String>,
}

/// One semantic-search hit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeSearchHit {
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub score: f32,
    pub snippet: String,
    pub lang: Option<String>,
    pub commit: Option<String>,
}

/// VT cursor as the server-side machine holds it (§7.9). It travels with every
/// delta, not only with a snapshot: the caret moves on each keystroke, and a
/// client that had to wait for a snapshot to see it would render a terminal
/// whose cursor lags behind the text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalCursorInfo {
    pub row: u16,
    pub col: u16,
    pub visible: bool,
}

/// An open shell of a session. Enough to rebuild the terminal dock after a
/// browser reload without opening a second shell for a terminal that is already
/// running on the owner node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminalInfo {
    pub terminal_id: String,
    pub title: String,
    pub rows: u16,
    pub cols: u16,
    /// Profile the terminal REALLY got, not the one requested (§7.2).
    pub mount_access: String,
    pub network_access: String,
    /// 'running' | 'exited' | 'reaped'.
    pub status: String,
    pub started_at: String,
}

/// One step of the merge saga (§11.6). ONE list describes the whole process,
/// from the integration worktree to the reference the finalize moves, and both
/// merge answers report it: `GitMergeResponse` for steps that already have an
/// outcome and `pending` for the rest, `GitMergeFinalizeResponse` for the same
/// list once the publish half has run. The eight steps are, in order:
///
/// `integration_worktree`, `private_ref`, `merge`, `patch_set`, `tests`,
/// `review`, `approval`, `update_ref`.
///
/// Test and review outcomes are separate steps precisely because they cannot be
/// inferred from the patch set's status — a set is `in_review` both before a
/// verification ran and after one failed. `review` is the set being handed to a
/// human, `approval` the decision that came back.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MergeStepInfo {
    pub step: String,
    /// 'pending' | 'running' | 'done' | 'failed' | 'skipped'.
    pub status: String,
    /// WHY the step is not `done`, as a stable reason CODE the client maps to
    /// its own language — never a server sentence, which no browser can
    /// translate. `None` on a step that reached `done`, because a finished step
    /// owes no reason. The codes are:
    ///
    /// - `merge_conflicted` — the merge stopped on conflicting paths; they are
    ///   in `GitMergeResponse::conflict_files`;
    /// - `no_result_to_pin` — nothing to write under the private ref, because
    ///   the merge produced no tree;
    /// - `conflict_resolved_in_revision_run` — no patch set yet: a conflicted
    ///   merge is reviewed after a revision run resolves it;
    /// - `tests_not_run_by_merge` — Code Studio's merge NEVER runs tests. §11.6
    ///   pkt 4 makes verification an ordinary agent call on the integration
    ///   worktree (`core.exec`, or a spawned `code-tester`), and nothing links
    ///   such a call back to a merge operation, so no test verdict exists here
    ///   to report;
    /// - `awaiting_review` — the patch set is open and no verdict was given;
    /// - `target_moved` — the target branch moved after the reviewed merge was
    ///   computed, so the whole attempt is void (§11.6 pkt 6);
    /// - `update_ref_failed` — the compare-and-swap on the target reference
    ///   failed for another reason.
    pub detail: Option<String>,
}

/// A user the caller may still add to a workspace. Deliberately NOT
/// `project_studio::UserRefWire`: both families are append-only on their own
/// schedule, and sharing one struct would let a field appended for Project
/// Studio change the Code Studio wire without anyone touching this file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkspaceUserCandidate {
    pub user_id: String,
    pub display_name: String,
    pub email: String,
}

/// A project this workspace is linked to (§20). The relation is N:M, the
/// project name is resolved server-side — the browser has no route into the
/// project registry from the Code Studio screen.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectLinkInfo {
    pub project_id: String,
    pub project_name: String,
    pub linked_by: String,
    pub created_at: String,
}

/// One entry of a repository listing handed to a linked project. Names only:
/// a path INSIDE the repository, its mode and the blob id. Content is not part
/// of it — a project reads what the repository contains, not what the files
/// say — and no host path can appear here at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RepoEntryInfo {
    pub path: String,
    pub mode: String,
    pub blob_oid: String,
}

/// Provider credential of one CLI engine on one node (§5.2, §7.5).
///
/// The material is deliberately absent and there is no field it could travel
/// in: the row is node-local key material, and the only two consumers are the
/// git broker and the provider adapter, both inside the owner node's process.
/// `fingerprint` is a digest that identifies WHICH key is stored without being
/// able to reconstruct it — the same thing `WorkspaceSecretSetResponse` shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCredentialInfo {
    /// Node whose vault holds the material. A credential is meaningless on any
    /// other node, so the UI has to show which one it belongs to.
    pub node_id: String,
    pub engine_id: String,
    /// Upstream the adapter forwards to once it has injected the material.
    pub provider_base_url: String,
    pub fingerprint: Option<String>,
    pub created_by: String,
    pub created_at: String,
    pub rotated_at: Option<String>,
    pub last_used_at: Option<String>,
}

/// Code Studio message family (request + response). ciborium encodes variants
/// external-tagged by variant NAME, so never rename variants or fields without
/// updating the frontend and the golden test (`code_studio_wire_golden`).
/// Variant order is the wire contract: append-only, never insert or reorder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CodeStudioPayload {
    // ---- Registry ----
    WorkspacesListRequest {
        #[serde(default)]
        include_archived: bool,
    },
    WorkspacesListResponse {
        workspaces: Vec<WorkspaceInfo>,
        /// Whether the caller holds the per-user grant needed to create one.
        can_create: bool,
        nodes: Vec<WorkspaceNodeInfo>,
    },
    WorkspaceCreateRequest {
        name: String,
        node_id: String,
        /// 'container' | 'trusted_native'.
        exec_mode: String,
        container_image: Option<String>,
        /// 'empty' | 'git'.
        repo_kind: String,
        repo_url: Option<String>,
        /// 'none' | 'token' | 'ssh_key'.
        repo_auth_kind: Option<String>,
        /// Credential material. Travels once, is stored encrypted in the
        /// node-local vault and is never sent back.
        secret_material: Option<String>,
        /// Pinned SSH host key line, shown to the user on first contact.
        ssh_host_fingerprint: Option<String>,
        default_branch: Option<String>,
        autonomy_ceiling: String,
        egress_policy: String,
        index_enabled: bool,
        members: Vec<WorkspaceMemberInput>,
    },
    WorkspaceCreateResponse {
        workspace_id: String,
        /// The workspace starts `provisioning`; the UI follows it with
        /// `WorkspaceGetRequest` rather than assuming success.
        status: String,
    },
    WorkspaceGetRequest {
        workspace_id: String,
    },
    WorkspaceGetResponse {
        workspace: WorkspaceInfo,
        members: Vec<WorkspaceMemberInfo>,
        provisioning: Vec<ProvisionStepInfo>,
    },
    /// Re-runs provisioning of a workspace left in `error`. Completed steps are
    /// skipped, so this is a resume rather than a rebuild.
    WorkspaceRetryRequest {
        workspace_id: String,
    },
    WorkspaceRetryResponse {
        workspace_id: String,
        status: String,
        status_detail: Option<String>,
    },
    WorkspaceArchiveRequest {
        workspace_id: String,
        archived: bool,
    },
    WorkspaceArchiveResponse {
        workspace_id: String,
        status: String,
    },

    // ---- Members and the create grant ----
    WorkspaceMemberSetRequest {
        workspace_id: String,
        user_id: String,
        role: String,
    },
    WorkspaceMemberRemoveRequest {
        workspace_id: String,
        user_id: String,
    },
    WorkspaceMembersResponse {
        workspace_id: String,
        members: Vec<WorkspaceMemberInfo>,
    },
    WorkspaceCreatorGrantSetRequest {
        user_id: String,
        granted: bool,
    },
    WorkspaceCreatorGrantResponse {
        user_id: String,
        granted: bool,
    },

    // ---- Sessions ----
    SessionsListRequest {
        workspace_id: String,
    },
    SessionsListResponse {
        workspace_id: String,
        sessions: Vec<SessionInfo>,
    },
    SessionOpenRequest {
        workspace_id: String,
        title: String,
        autonomy_mode: String,
    },
    SessionOpenResponse {
        session: SessionInfo,
    },
    SessionCloseRequest {
        workspace_id: String,
        session_id: String,
    },
    SessionCloseResponse {
        session_id: String,
        status: String,
    },

    // ---- Filesystem (§7.7, §10) ----
    // Every path is relative to the session worktree. The server resolves it
    // through the directory-handle guard (§8); a path that escapes the worktree
    // is refused, never clamped.
    FileTreeRequest {
        workspace_id: String,
        session_id: String,
        /// Empty means the worktree root.
        #[serde(default)]
        path: String,
        /// 1 = the directory itself, deeper values expand subdirectories.
        depth: u32,
    },
    FileTreeResponse {
        session_id: String,
        path: String,
        entries: Vec<FileEntryInfo>,
        /// The server caps the entry count; the UI must say so rather than
        /// pretend the directory ended.
        truncated: bool,
    },
    FileReadRequest {
        workspace_id: String,
        session_id: String,
        path: String,
        /// 1-based, inclusive. Both absent = whole file up to the server cap.
        #[serde(default)]
        start_line: Option<u32>,
        #[serde(default)]
        end_line: Option<u32>,
    },
    FileReadResponse {
        path: String,
        content: String,
        /// Hash of the FULL file, not of the returned slice — it is the CAS
        /// token a later write has to present.
        blob_sha: String,
        truncated: bool,
        language: Option<String>,
        total_lines: u32,
    },
    FileWriteRequest {
        workspace_id: String,
        session_id: String,
        path: String,
        content: String,
        /// Absent = the file must not exist yet; present = compare-and-swap.
        #[serde(default)]
        expected_blob_sha: Option<String>,
    },
    FileWriteResponse {
        path: String,
        blob_sha: String,
        op_id: String,
    },
    FileCreateRequest {
        workspace_id: String,
        session_id: String,
        path: String,
        #[serde(default)]
        content: String,
    },
    FileDeleteRequest {
        workspace_id: String,
        session_id: String,
        path: String,
        #[serde(default)]
        recursive: bool,
        #[serde(default)]
        expected_blob_sha: Option<String>,
    },
    FileRenameRequest {
        workspace_id: String,
        session_id: String,
        from_path: String,
        to_path: String,
        #[serde(default)]
        expected_blob_sha: Option<String>,
    },
    FileMkdirRequest {
        workspace_id: String,
        session_id: String,
        path: String,
    },
    /// Shared answer of create/delete/rename/mkdir. `blob_sha` is absent for
    /// delete and mkdir, which produce no content.
    FileMutationResponse {
        path: String,
        blob_sha: Option<String>,
        op_id: String,
    },
    FileGrepRequest {
        workspace_id: String,
        session_id: String,
        query: String,
        /// Path filter, e.g. `src/**/*.rs`. Empty = whole worktree.
        #[serde(default)]
        glob: String,
        /// False makes `query` a literal, so a user searching for `a.b` does
        /// not silently get a regex.
        #[serde(default)]
        regex: bool,
        max_results: u32,
    },
    FileGrepResponse {
        hits: Vec<GrepHitInfo>,
        truncated: bool,
    },

    // ---- Git broker (§10, §11) ----
    GitStatusRequest {
        workspace_id: String,
        session_id: String,
    },
    GitStatusResponse {
        branch: String,
        ahead: u32,
        behind: u32,
        entries: Vec<GitStatusEntry>,
    },
    GitLogRequest {
        workspace_id: String,
        session_id: String,
        /// Empty = whole branch.
        #[serde(default)]
        path: String,
        limit: u32,
    },
    GitLogResponse {
        commits: Vec<GitCommitInfo>,
    },
    GitBranchesRequest {
        workspace_id: String,
        session_id: String,
    },
    GitBranchesResponse {
        branches: Vec<GitBranchInfo>,
    },
    GitDiffRequest {
        workspace_id: String,
        session_id: String,
        /// Empty = the whole worktree.
        #[serde(default)]
        path: String,
        #[serde(default)]
        staged: bool,
        /// Commit or branch to diff against; empty = the session base commit.
        #[serde(default)]
        base: String,
    },
    GitDiffResponse {
        path: String,
        diff_text: String,
        hunks: Vec<DiffHunkInfo>,
        truncated: bool,
    },
    GitCommitRequest {
        workspace_id: String,
        session_id: String,
        message: String,
        /// The accepted patch set the commit is built from. Absent opens the
        /// review gate (§9.3 step 5a) instead of committing the worktree.
        #[serde(default)]
        patch_set_id: Option<String>,
    },
    GitCommitResponse {
        op_id: String,
        /// 'committed' | 'review_required'.
        status: String,
        commit_oid: Option<String>,
        /// Set when the gate opened a review; this is the set to decide on.
        patch_set_id: Option<String>,
    },
    GitPushRequest {
        workspace_id: String,
        session_id: String,
        remote: String,
        /// PERMANENTLY UNSUPPORTED — only `false` is accepted; `true` is
        /// refused with `NotAvailable` and the message
        /// `set_upstream_unsupported: …`.
        ///
        /// An upstream is a branch pointing at a remote NAME, and the name is
        /// resolved out of the repository's own config at push time. The broker
        /// dials only the URL its policy check judged (§11.4) and never lets
        /// repository data pick the destination, so it can neither record such
        /// a name nor honour one. The field stays on the wire because the
        /// contract is append-only.
        #[serde(default)]
        set_upstream: bool,
    },
    GitPushResponse {
        op_id: String,
        /// 'pushed' | 'failed'. A push waiting on a human is NOT a status here:
        /// every Code Studio operation the PEP suspends answers with the one
        /// error described on `ApprovalInfo`.
        status: String,
        remote_branch: Option<String>,
        error: Option<String>,
    },
    GitSyncRequest {
        workspace_id: String,
        session_id: String,
        /// 'fetch' | 'pull'.
        mode: String,
    },
    GitSyncResponse {
        op_id: String,
        /// 'fetch' | 'pull' | 'failed' — the mode that ran, or the failure. A
        /// sync waiting on a human answers with the approval error instead.
        status: String,
        ahead: u32,
        behind: u32,
        error: Option<String>,
    },
    GitMergeRequest {
        workspace_id: String,
        session_id: String,
        source_branch: String,
        target_branch: String,
    },
    GitMergeResponse {
        op_id: String,
        /// 'clean' | 'conflict'. A conflict is a result, not an error: the
        /// integration worktree stays in `held` so a revision run can work on it.
        outcome: String,
        conflict_files: Vec<String>,
        /// Patch set of scope 'merge', produced on a clean merge.
        patch_set_id: Option<String>,
        integration_worktree: Option<WorktreeInfo>,
        /// Target-branch tip the merge was computed on; finalize refuses if the
        /// branch has moved since.
        expected_old: String,
        /// The eight steps of §11.6 with their real outcome, `pending` for the
        /// half a merge has not reached yet. Without them the UI can only guess
        /// the state of the tests and the review from the patch set's status,
        /// which is a different fact.
        #[serde(default)]
        steps: Vec<MergeStepInfo>,
    },
    GitMergeFinalizeRequest {
        workspace_id: String,
        session_id: String,
        op_id: String,
        patch_set_id: String,
    },
    GitMergeFinalizeResponse {
        op_id: String,
        /// 'merged' | 'stale_base' | 'failed'. A finalize the PEP suspends
        /// answers with the approval error, like every other gated operation.
        status: String,
        merge_commit_oid: Option<String>,
        error: Option<String>,
        /// The SAME eight steps `GitMergeResponse` reports, now with the publish
        /// half decided. One list describes one process, so a client renders the
        /// merge and its finalize from one source instead of stitching two.
        #[serde(default)]
        steps: Vec<MergeStepInfo>,
    },
    /// Drops a held integration worktree and its private ref. Explicit, because
    /// nothing else may remove state a revision run is supposed to resume from.
    GitMergeAbandonRequest {
        workspace_id: String,
        session_id: String,
        op_id: String,
    },
    WorktreesListRequest {
        workspace_id: String,
        session_id: String,
    },
    WorktreesListResponse {
        session_id: String,
        worktrees: Vec<WorktreeInfo>,
    },

    // ---- Patch sets and review (§13.2, §16.4) ----
    PatchSetsListRequest {
        workspace_id: String,
        session_id: String,
        /// Empty = every status.
        #[serde(default)]
        status: String,
    },
    PatchSetsListResponse {
        session_id: String,
        patch_sets: Vec<PatchSetInfo>,
    },
    PatchSetGetRequest {
        workspace_id: String,
        session_id: String,
        patch_set_id: String,
    },
    PatchSetGetResponse {
        patch_set: PatchSetInfo,
        files: Vec<PatchFileInfo>,
    },
    PatchDecideRequest {
        workspace_id: String,
        session_id: String,
        patch_set_id: String,
        files: Vec<PatchFileDecision>,
    },
    PatchDecideResponse {
        patch_set_id: String,
        status: String,
        /// Files whose accepted hunks did not compose cleanly onto the base
        /// blob — decided at file level, never guessed.
        conflicted_paths: Vec<String>,
    },
    PatchSetAbandonRequest {
        workspace_id: String,
        session_id: String,
        patch_set_id: String,
    },

    // ---- Timeline, operations, approvals (§13.1, §13.3, §9.1) ----
    SessionTimelineRequest {
        workspace_id: String,
        session_id: String,
        /// Cursor: events with `seq` strictly greater are returned.
        #[serde(default)]
        after_seq: u64,
        limit: u32,
    },
    SessionTimelineResponse {
        session_id: String,
        events: Vec<TimelineEventInfo>,
        /// Cursor to pass as the next `after_seq`.
        next_seq: u64,
        has_more: bool,
    },
    SessionOperationsRequest {
        workspace_id: String,
        session_id: String,
        /// Empty = every status; `unknown` is what needs a human.
        #[serde(default)]
        status: String,
        limit: u32,
    },
    SessionOperationsResponse {
        session_id: String,
        operations: Vec<OperationInfo>,
    },
    /// Closes an operation left `unknown` after a crash. There is no silent
    /// retry: a non-idempotent effect is settled by a person or not at all.
    OperationResolveRequest {
        workspace_id: String,
        session_id: String,
        op_id: String,
        /// 'completed' | 'failed'.
        resolution: String,
        #[serde(default)]
        note: String,
    },
    OperationResolveResponse {
        op_id: String,
        status: String,
    },
    ApprovalsListRequest {
        workspace_id: String,
        session_id: String,
        /// Empty = every status.
        #[serde(default)]
        status: String,
    },
    ApprovalsListResponse {
        session_id: String,
        approvals: Vec<ApprovalInfo>,
    },
    ApprovalDecideRequest {
        workspace_id: String,
        session_id: String,
        approval_id: String,
        /// 'allow_once' | 'allow_for_run' | 'allow_for_session' | 'always' | 'deny'.
        /// `always` on a mandatory-interactive capability is refused at write.
        decision: String,
    },
    ApprovalDecideResponse {
        approval_id: String,
        status: String,
        decision: String,
        /// True when the answer woke a call that was parked on this card.
        ///
        /// A card raised by a run and answered too late leaves the decision
        /// recorded and the run already gone, which looks identical to a
        /// successful resume from the console. The console needs to be able to
        /// tell the operator which of the two happened, so the answer carries
        /// it rather than leaving it to be inferred from a timeline that has
        /// not been re-read yet. `false` is also the ordinary case for a card
        /// the dashboard raised for its own request — nothing parks there.
        #[serde(default)]
        resumed: bool,
    },
    SessionGrantsListRequest {
        workspace_id: String,
        session_id: String,
    },
    SessionGrantsListResponse {
        session_id: String,
        grants: Vec<GrantInfo>,
    },
    SessionGrantRevokeRequest {
        workspace_id: String,
        session_id: String,
        capability: String,
        pattern: String,
    },
    WorkspaceAllowlistListRequest {
        workspace_id: String,
    },
    WorkspaceAllowlistListResponse {
        workspace_id: String,
        entries: Vec<AllowlistEntryInfo>,
    },
    WorkspaceAllowlistSetRequest {
        workspace_id: String,
        capability: String,
        pattern: String,
    },
    WorkspaceAllowlistRemoveRequest {
        workspace_id: String,
        capability: String,
        pattern: String,
    },

    // ---- Runs and agents (§15, §16.3) ----
    SessionRunsRequest {
        workspace_id: String,
        session_id: String,
    },
    SessionRunsResponse {
        session_id: String,
        runs: Vec<RunInfo>,
    },
    /// A user turn addressed to the session's root agent.
    SessionMessageSendRequest {
        workspace_id: String,
        session_id: String,
        message: String,
    },
    SessionMessageSendResponse {
        session_id: String,
        run_id: String,
        status: String,
    },
    SessionCancelRequest {
        workspace_id: String,
        session_id: String,
        /// Absent cancels the whole session, including subagent runs.
        #[serde(default)]
        run_id: Option<String>,
    },
    SessionCancelResponse {
        session_id: String,
        cancelled_runs: Vec<String>,
        status: String,
    },
    /// Autonomy can be lowered freely and raised only up to the workspace
    /// ceiling (§25.1); the server clamps, the UI does not.
    SessionAutonomySetRequest {
        workspace_id: String,
        session_id: String,
        autonomy_mode: String,
    },
    SessionAutonomySetResponse {
        session_id: String,
        autonomy_mode: String,
        autonomy_ceiling: String,
    },

    // ---- Exec and terminal (§7.4, §7.8, §7.9) ----
    ExecStartRequest {
        workspace_id: String,
        session_id: String,
        /// Always a vector; a shell is one explicit, quoted argument.
        argv: Vec<String>,
        /// Relative to the worktree; empty = its root.
        #[serde(default)]
        cwd: String,
        timeout_secs: u32,
        /// 'ro' | 'cow' | 'rw'.
        mount_access: String,
        /// 'none' | 'gateway'.
        network_access: String,
        /// Drops the COW layer once the command ends.
        #[serde(default)]
        ephemeral: bool,
    },
    ExecStartResponse {
        exec_id: String,
        op_id: String,
        /// Echoed back RESOLVED: the PEP may hand out a narrower profile than
        /// the one asked for, and the user has to see which one ran.
        mount_access: String,
        network_access: String,
        ephemeral: bool,
        /// The mount the caller ASKED for, verbatim. `mount_access` is what the
        /// PEP resolved; when the two differ the request was narrowed, and a
        /// caller comparing one field against the other never has to guess
        /// whether the profile it got is the profile it wanted.
        #[serde(default)]
        requested_mount_access: String,
        /// True when the command runs against a COPY of the worktree: it may
        /// write, it may exit 0, and nothing it wrote reaches the worktree.
        /// `exec` is pinned to `cow`, so a caller asking for `rw` gets this
        /// flag instead of a silent no-op that reads as success.
        #[serde(default)]
        writes_discarded: bool,
    },
    ExecCancelRequest {
        workspace_id: String,
        session_id: String,
        exec_id: String,
    },
    ExecCancelResponse {
        exec_id: String,
        status: String,
    },
    TerminalOpenRequest {
        workspace_id: String,
        session_id: String,
        rows: u16,
        cols: u16,
    },
    TerminalOpenResponse {
        terminal_id: String,
        rows: u16,
        cols: u16,
        mount_access: String,
        network_access: String,
    },
    TerminalInputRequest {
        workspace_id: String,
        session_id: String,
        terminal_id: String,
        /// Key bytes as typed, already UTF-8 encoded by the browser.
        data: String,
    },
    TerminalResizeRequest {
        workspace_id: String,
        session_id: String,
        terminal_id: String,
        rows: u16,
        cols: u16,
    },
    TerminalCloseRequest {
        workspace_id: String,
        session_id: String,
        terminal_id: String,
    },
    /// Full grid, used on open and after a reconnect; the live stream carries
    /// only the changed rows.
    TerminalSnapshotRequest {
        workspace_id: String,
        session_id: String,
        terminal_id: String,
    },
    TerminalSnapshotResponse {
        terminal_id: String,
        /// Monotonic VT revision — the client discards a snapshot older than
        /// the deltas it already applied.
        revision: u64,
        rows: u16,
        cols: u16,
        cursor_row: u16,
        cursor_col: u16,
        cursor_visible: bool,
        cells: Vec<TerminalCellRow>,
    },

    // ---- Workspace settings, secrets, deletion (§19, §9.5) ----
    /// `exec_mode` is absent on purpose: the execution mode of an existing
    /// workspace is immutable (§9.5), so there is nowhere to send a change.
    WorkspaceSettingsUpdateRequest {
        workspace_id: String,
        name: String,
        autonomy_ceiling: String,
        egress_policy: String,
        #[serde(default)]
        target_branch: Option<String>,
        index_enabled: bool,
        #[serde(default)]
        quota_disk_bytes: Option<i64>,
        #[serde(default)]
        quota_sessions: Option<i64>,
    },
    WorkspaceSettingsUpdateResponse {
        workspace: WorkspaceInfo,
    },
    /// Credential material travels one way only. The response says whether a
    /// secret is stored and shows its fingerprint, never the material.
    WorkspaceSecretSetRequest {
        workspace_id: String,
        /// 'none' | 'token' | 'ssh_key'; 'none' clears the stored credential.
        repo_auth_kind: String,
        #[serde(default)]
        secret_material: Option<String>,
        #[serde(default)]
        ssh_host_fingerprint: Option<String>,
    },
    WorkspaceSecretSetResponse {
        workspace_id: String,
        has_secret: bool,
        /// Fingerprint of the stored key, i.e. a digest — not the key.
        fingerprint: Option<String>,
    },
    WorkspaceDeleteRequest {
        workspace_id: String,
    },
    WorkspaceDeleteResponse {
        workspace_id: String,
        status: String,
    },

    // ---- Semantic index (§14) ----
    IndexStatusRequest {
        workspace_id: String,
    },
    IndexStatusResponse {
        workspace_id: String,
        index_enabled: bool,
        branches: Vec<IndexStateInfo>,
    },
    IndexRebuildRequest {
        workspace_id: String,
        branch: String,
    },
    IndexRebuildResponse {
        workspace_id: String,
        branch: String,
        job_id: String,
        status: String,
    },
    CodeSearchRequest {
        workspace_id: String,
        session_id: String,
        query: String,
        /// Path prefix filter; empty = whole worktree.
        #[serde(default)]
        path_prefix: String,
        limit: u32,
        /// 'semantic' | 'grep'. The server may answer a semantic request with
        /// grep and set `degraded` — grep stays authoritative (§14).
        mode: String,
    },
    CodeSearchResponse {
        hits: Vec<CodeSearchHit>,
        /// The mode that actually ran.
        mode: String,
        degraded: bool,
    },

    // ---- Streams (§12.2, §13.3, §7.9) ----
    // Three subscriptions, each resumed from a cursor the CLIENT holds, so a
    // reconnect neither repeats nor skips: `after_seq` for the event log,
    // `after_revision` for the VT grid. A stream never carries raw VT bytes and
    // never carries an event body over the frame budget — that body is already
    // in the artifact store (§13.2) and the frame says so.
    //
    // Every stream closes with a NAMED reason, never a silent drop:
    //   `session_closed`       — the session was closed or interrupted
    //   `not_found`            — uniform denial; a non-member cannot tell a
    //                            missing workspace from someone else's
    //   `permission_revoked`   — membership, read permission or session
    //                            ownership was withdrawn mid-stream
    //   `workspace_not_local`  — the owner node is another one; mesh transport
    //                            for a remote workspace is phase 4
    //   `terminal_not_open`    — no such terminal on this node (a restart reaps
    //                            every shell, so the client must reopen)
    //   `terminal_exited`      — the shell ended; the grid stops here
    //   `index_unavailable`    — the semantic index does not exist on this node
    //   `internal_error`       — the server could not read its own state
    SessionStreamRequest {
        workspace_id: String,
        session_id: String,
        /// Resume cursor: events with `seq` strictly greater are sent. 0 replays
        /// the session from its first event.
        #[serde(default)]
        after_seq: u64,
    },
    /// One event of the session timeline, live. Same shape as
    /// `TimelineEventInfo` minus `event_id`: the stream is addressed by `seq`,
    /// which is what dedupes it.
    SessionStreamEvent {
        seq: u64,
        kind: String,
        run_id: Option<String>,
        agent_id: Option<String>,
        created_at: String,
        payload_json: String,
        security_relevant: bool,
    },
    SessionStreamEnd {
        reason: String,
    },
    TerminalStreamRequest {
        workspace_id: String,
        session_id: String,
        terminal_id: String,
        /// VT revision the client already holds. 0 — or any value that does not
        /// match the live grid — earns a full snapshot before the deltas.
        #[serde(default)]
        after_revision: u64,
    },
    /// Full grid: first frame of a stream whose client holds nothing usable,
    /// and again whenever the revision the client claims cannot be continued.
    TerminalStreamSnapshot {
        revision: u64,
        grid_rows: u16,
        grid_cols: u16,
        cursor: TerminalCursorInfo,
        rows: Vec<TerminalCellRow>,
    },
    /// Only the rows that changed. A resize rewrites every row, so the frame
    /// carries the grid size as well and a client never renders a stale width.
    TerminalStreamDelta {
        revision: u64,
        grid_rows: u16,
        grid_cols: u16,
        cursor: TerminalCursorInfo,
        rows: Vec<TerminalCellRow>,
    },
    TerminalStreamEnd {
        reason: String,
    },
    /// Indexing progress (§14). The index is phase 7: this stream exists so the
    /// UI has one place to attach, and until then it closes with
    /// `index_unavailable` rather than reporting progress that nothing produces.
    IndexStreamRequest {
        workspace_id: String,
        #[serde(default)]
        after_seq: u64,
    },
    IndexStreamEnd {
        reason: String,
    },

    // ---- Blob read and terminal inventory ----
    /// Reads one blob of the CAS by its digest (§13.2). Reconstructing a file
    /// after a partial acceptance needs the whole accepted blob, not the hunk
    /// windows the patch set carries.
    PatchBlobGetRequest {
        workspace_id: String,
        session_id: String,
        blob_sha: String,
    },
    PatchBlobGetResponse {
        blob_sha: String,
        content: String,
        /// The server caps the returned size; a truncated blob must be shown as
        /// truncated, never as the file.
        truncated: bool,
    },
    TerminalsListRequest {
        workspace_id: String,
        session_id: String,
    },
    TerminalsListResponse {
        terminals: Vec<TerminalInfo>,
    },

    // ---- Member candidates (§19) ----
    /// Users the caller may still add. `workspace_id: None` is the creation
    /// wizard, which has no workspace to exclude members of yet; `Some` is the
    /// member tab of an existing workspace and the answer leaves its current
    /// members out.
    WorkspaceMemberCandidatesRequest {
        #[serde(default)]
        workspace_id: Option<String>,
        query: String,
        limit: u32,
    },
    WorkspaceMemberCandidatesResponse {
        candidates: Vec<WorkspaceUserCandidate>,
    },

    /// One frame of `IndexStreamRequest` (§14), mirroring the indexer's own
    /// progress record field for field. `terminal` marks the last frame of THAT
    /// JOB — the stream stays open for the next one, so a client must not treat
    /// it as the end of the subscription; `IndexStreamEnd` is what ends it.
    IndexStreamProgress {
        /// Per-workspace progress cursor, the resume value of `after_seq`.
        seq: u64,
        job_id: String,
        workspace_id: String,
        branch: String,
        /// 'queued' | 'walk' | 'index' | 'done' | 'partial' | 'failed'
        /// | 'cancelled'. `partial` is a job that ran out of its time budget,
        /// which is a real outcome and not a failure.
        phase: String,
        files_done: u32,
        files_total: u32,
        chunks: u32,
        message: String,
        terminal: bool,
    },

    // ---- Project links and repository listing (§20) ----
    ProjectLinkListRequest {
        workspace_id: String,
    },
    ProjectLinkListResponse {
        workspace_id: String,
        links: Vec<ProjectLinkInfo>,
    },
    /// Links or unlinks one project. One variant for both directions, and the
    /// answer is `ProjectLinkListResponse` — the same shape the member tab
    /// uses, so the caller never has to merge a delta into a list it already
    /// holds.
    ProjectLinkSetRequest {
        workspace_id: String,
        project_id: String,
        /// True links, false unlinks.
        linked: bool,
    },
    /// Structure of a PINNED commit of a linked workspace. Addressed by a
    /// commit id and never by a path on the owner node: repeatability is the
    /// whole point of §20, and the host layout is not the project's business.
    RepoTreeRequest {
        workspace_id: String,
        /// The project asking. Visibility comes from the link, so a workspace
        /// nobody linked answers exactly like one that does not exist.
        project_id: String,
        commit: String,
        /// Path prefix INSIDE the repository; empty = the whole tree.
        #[serde(default)]
        path_prefix: String,
        limit: u32,
    },
    RepoTreeResponse {
        workspace_id: String,
        commit: String,
        entries: Vec<RepoEntryInfo>,
        /// The server caps the entry count; a cut listing has to say so rather
        /// than look like a small repository.
        truncated: bool,
    },

    // ---- Command output (§7.8) ----
    /// Reads back what a command printed. `ExecStartResponse` only says the
    /// command STARTED — the transcript lands in an artifact when it ends, and
    /// without this the only way to the stdout of a finished command was the
    /// artifact digest in a timeline row, which no client can dereference.
    ExecOutputRequest {
        workspace_id: String,
        session_id: String,
        /// The `exec_id` of `ExecStartResponse`, which is the operation id.
        exec_id: String,
        /// Cursor: lines with a lower index are skipped, so a poller can ask
        /// for the tail it has not seen.
        #[serde(default)]
        after_seq: u64,
        limit: u32,
    },
    ExecOutputResponse {
        exec_id: String,
        /// 'pending' | 'completed' | 'failed' | 'unknown' — the operation's
        /// status, because an empty tail means something different while the
        /// command is still running.
        status: String,
        /// Redacted transcript lines: the command line, then stdout, then
        /// stderr, exactly as the artifact stores them.
        lines: Vec<String>,
        /// Cursor to pass as the next `after_seq`.
        next_seq: u64,
        has_more: bool,
    },

    // ---- Provider credentials of the CLI engines (§5.2, §7.5) ----
    /// Lists what the vault of ONE node holds. `node_id` empty means this node;
    /// naming another node forwards the call there, because the answer is a
    /// fact about that node's vault and nothing else can know it.
    AgentCredentialsListRequest {
        #[serde(default)]
        node_id: String,
    },
    AgentCredentialsListResponse {
        node_id: String,
        credentials: Vec<AgentCredentialInfo>,
        /// Engines this build can put behind the adapter at all. The picker is
        /// built from it so the browser never keeps its own copy of the list.
        engines: Vec<String>,
    },
    /// Stores or ROTATES the provider credential of one engine. There is no
    /// separate rotate variant: the row is keyed by (org, node, engine), so a
    /// second write replaces the material in place instead of leaving two.
    AgentCredentialSetRequest {
        #[serde(default)]
        node_id: String,
        engine_id: String,
        /// Upstream the adapter forwards to. Stored with the material because
        /// it is part of the same decision: which provider this key pays for.
        provider_base_url: String,
        /// The one direction material travels. Nothing sends it back.
        credential_material: String,
    },
    AgentCredentialSetResponse {
        credential: AgentCredentialInfo,
    },
    AgentCredentialDeleteRequest {
        #[serde(default)]
        node_id: String,
        engine_id: String,
    },
    AgentCredentialDeleteResponse {
        node_id: String,
        engine_id: String,
        removed: bool,
    },

    /// The session's PLAN — the task rows `core.task_plan` wrote and
    /// `core.task_update` moves. Read-only: the plan belongs to the agents that
    /// work it, and an operator watching should see the same list the build
    /// loop's gate is checking.
    SessionTasksRequest {
        workspace_id: String,
        session_id: String,
    },
    SessionTasksResponse {
        session_id: String,
        /// Tasks in the planner's own order.
        tasks: Vec<TaskInfo>,
        /// How many are not `done`. The number the gate acts on, sent so the UI
        /// never has to re-derive it and disagree.
        open: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message_body::MessageBody;

    /// This very file, read at compile time. The byte goldens below pin a
    /// handful of SHAPES; the name goldens that follow pin the whole SURFACE,
    /// and they can only do that by reading the declarations. There are no
    /// `#[serde(rename)]` attributes in this module, so a declared name IS the
    /// wire name — the parser asserts the counts it finds, which is what keeps
    /// that assumption from rotting silently.
    const SOURCE: &str = include_str!("code_studio.rs");

    /// FNV-1a 64 over the names joined by newlines. A hash, not a stored list:
    /// the point is one cheap assertion that fails on ANY rename, and the
    /// failure message prints the live names so the diff is one glance away.
    fn name_digest(names: &[String]) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in names.join("\n").as_bytes() {
            hash ^= *byte as u64;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }

    /// Variant names of `CodeStudioPayload`, in declaration order. Depth is
    /// tracked by brace counting, so only the enum's own level is read.
    fn payload_variant_names(source: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut depth = 0usize;
        let mut inside = false;
        for line in source.lines() {
            if !inside {
                if line.starts_with("pub enum CodeStudioPayload {") {
                    inside = true;
                    depth = 1;
                }
                continue;
            }
            if depth == 1 {
                if let Some(rest) = line.strip_prefix("    ") {
                    if rest.starts_with(|c: char| c.is_ascii_uppercase()) {
                        out.push(
                            rest.chars()
                                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                                .collect(),
                        );
                    }
                }
            }
            depth = depth + line.matches('{').count() - line.matches('}').count();
            if depth == 0 {
                break;
            }
        }
        out
    }

    /// Every `pub struct` of this module with its `pub` field names, in
    /// declaration order — the payload structs the browser decodes by name.
    fn wire_structs(source: &str) -> Vec<(String, Vec<String>)> {
        let lines: Vec<&str> = source.lines().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let line = lines[i];
            if let Some(rest) = line.strip_prefix("pub struct ") {
                if line.trim_end().ends_with('{') {
                    let name = rest
                        .chars()
                        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                        .collect::<String>();
                    let mut fields = Vec::new();
                    let mut j = i + 1;
                    while j < lines.len() && lines[j] != "}" {
                        if let Some(field) = lines[j].strip_prefix("    pub ") {
                            if let Some((ident, _)) = field.split_once(':') {
                                fields.push(ident.trim().to_string());
                            }
                        }
                        j += 1;
                    }
                    out.push((name, fields));
                    i = j;
                }
            }
            i += 1;
        }
        out
    }

    /// The whole variant surface, pinned by count + digest.
    ///
    /// The byte goldens cover 16 of 141 variants — and of the server's own
    /// frames only two stream pushes, no `*Response` at all — which leaves the
    /// far more likely accident uncovered: ciborium tags by NAME, so renaming a
    /// variant changes the wire while every round-trip test — which encodes and
    /// decodes with the same new name — stays green, and only the browser finds
    /// out. Declaration ORDER is pinned too, because the wire contract is
    /// append-only.
    #[test]
    fn code_studio_variant_names_are_pinned() {
        let names = payload_variant_names(SOURCE);
        assert_eq!(
            names.len(),
            141,
            "CodeStudioPayload variant COUNT changed. Appending is fine — update the count and \
             the digest below in the same commit. Live variants:\n{}",
            names.join("\n")
        );
        assert_eq!(
            name_digest(&names),
            0x22a0_865d_498a_f8db,
            "CodeStudioPayload variant NAMES or their order changed. ciborium tags variants by \
             name, so a rename silently breaks every deployed browser while the round-trip tests \
             stay green. Rename back, or update this digest deliberately. Live variants:\n{}",
            names.join("\n")
        );
    }

    /// The response STRUCTS, pinned the same way — the half the byte goldens
    /// never touched. `WorkspaceInfo` alone carries 28 fields the dashboard
    /// reads by name.
    #[test]
    fn code_studio_response_field_names_are_pinned() {
        let structs = wire_structs(SOURCE);
        let names: Vec<String> = structs.iter().map(|(n, _)| n.clone()).collect();
        assert_eq!(
            names.len(),
            35,
            "wire struct COUNT changed. Live structs:\n{}",
            names.join("\n")
        );
        assert_eq!(
            name_digest(&names),
            0x3749_e55e_d5a9_3ef5,
            "wire struct NAMES changed. Live structs:\n{}",
            names.join("\n")
        );

        // (struct, field count, digest of the field names in declaration order)
        let pinned: &[(&str, usize, u64)] = &[
            ("WorkspaceInfo", 28, 0xda50_e3bd_7ed5_7aa3),
            ("WorkspaceMemberInfo", 5, 0xed06_6adb_df5a_1186),
            ("WorkspaceMemberInput", 2, 0x3b21_cb16_a0d8_f8c0),
            ("ProvisionStepInfo", 4, 0xa676_b783_4751_de5f),
            ("SessionInfo", 11, 0xd3ce_4ebd_0761_7f68),
            ("WorkspaceNodeInfo", 5, 0x77bd_1f1e_ac50_86e0),
            ("FileEntryInfo", 4, 0xa007_4d9c_0811_a939),
            ("GrepHitInfo", 4, 0xf200_61ce_b218_4983),
            ("GitStatusEntry", 4, 0x4f0d_f450_8973_4a94),
            ("GitCommitInfo", 5, 0xec42_ea3f_2bc1_d97b),
            ("GitBranchInfo", 6, 0xa773_baa1_dc07_49a1),
            ("DiffHunkInfo", 3, 0x2cb1_73a1_8f3c_eec0),
            ("WorktreeInfo", 10, 0xe62b_e926_c322_1429),
            ("PatchSetInfo", 11, 0x8f7e_fb09_b7bf_e56f),
            ("PatchHunkInfo", 5, 0xd6ff_2da7_28f6_4a53),
            ("PatchFileInfo", 10, 0xaa64_41a5_0c46_fade),
            ("PatchHunkDecision", 2, 0x3ae5_d18a_0022_c226),
            ("PatchFileDecision", 4, 0x7a54_11fd_4a0d_83e9),
            ("TimelineEventInfo", 8, 0x9670_7957_ab78_7b15),
            ("OperationInfo", 12, 0xcd8f_dd95_8b3c_856b),
            ("ApprovalInfo", 14, 0xb495_12ac_e29d_e01d),
            ("GrantInfo", 5, 0x746f_66e3_5c06_3f72),
            ("AllowlistEntryInfo", 5, 0xfa26_c8fd_98e4_01bf),
            ("TaskInfo", 5, 0xba5d_338a_d0cd_cc0f),
            ("RunInfo", 14, 0xa39b_bf6c_da1e_37ea),
            ("TerminalCellRow", 3, 0x127a_c330_42a5_759e),
            ("IndexStateInfo", 6, 0x2bdd_bc96_705b_0813),
            ("CodeSearchHit", 7, 0x69cb_cf35_e72d_b0d1),
            ("TerminalCursorInfo", 3, 0xdef3_aa96_bc15_652f),
            ("TerminalInfo", 8, 0x01bf_d3cf_af9a_4bb3),
            ("MergeStepInfo", 3, 0xc64b_6bca_ebac_b948),
            ("WorkspaceUserCandidate", 3, 0xbb5e_ffdd_77e7_5762),
            ("ProjectLinkInfo", 4, 0x7dfc_6d1e_8418_2ee2),
            ("RepoEntryInfo", 3, 0x77ff_8301_d276_7ceb),
            ("AgentCredentialInfo", 8, 0xf976_17f0_e9d7_0293),
        ];
        assert_eq!(pinned.len(), structs.len());
        for (name, count, digest) in pinned {
            let fields = &structs
                .iter()
                .find(|(n, _)| n == name)
                .unwrap_or_else(|| panic!("struct '{name}' is gone from the wire module"))
                .1;
            assert_eq!(
                fields.len(),
                *count,
                "'{name}' field COUNT changed. Adding a field with #[serde(default)] is the \
                 supported move — update the count and digest here. Live fields:\n{}",
                fields.join("\n")
            );
            assert_eq!(
                name_digest(fields),
                *digest,
                "'{name}' field NAMES or their order changed. The dashboard decodes these by \
                 name, and a round-trip test cannot see the break because it re-encodes with the \
                 new name. Live fields:\n{}",
                fields.join("\n")
            );
        }
    }

    fn hex_bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
            .collect()
    }

    /// Golden wire snapshot: ciborium encodes enum variants as a 1-element map
    /// keyed by the variant NAME (external tagging). Pinning exact bytes turns
    /// any accidental rename of a variant, field or the
    /// `MessageBody::CodeStudioBody` tag into a test failure.
    #[test]
    fn code_studio_wire_golden() {
        let list = CodeStudioPayload::WorkspacesListRequest {
            include_archived: false,
        };
        let bytes = crate::cbor::encode(&list).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a175576f726b7370616365734c69737452657175657374a170696e636c7564655f6172636869766564f4"
            ),
            "WorkspacesListRequest wire drift"
        );

        let body = MessageBody::CodeStudioBody(list);
        let bytes = crate::cbor::encode(&body).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a16e436f646553747564696f426f6479a175576f726b7370616365734c69737452657175657374a170696e636c7564655f6172636869766564f4"
            ),
            "MessageBody::CodeStudioBody wire drift"
        );

        let close = CodeStudioPayload::SessionCloseRequest {
            workspace_id: "w1".to_string(),
            session_id: "s1".to_string(),
        };
        let bytes = crate::cbor::encode(&close).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a17353657373696f6e436c6f736552657175657374a26c776f726b73706163655f69646277316a73657373696f6e5f6964627331"
            ),
            "SessionCloseRequest wire drift"
        );

        let abandon = CodeStudioPayload::PatchSetAbandonRequest {
            workspace_id: "w1".to_string(),
            session_id: "s1".to_string(),
            patch_set_id: "p1".to_string(),
        };
        let bytes = crate::cbor::encode(&abandon).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a17650617463685365744162616e646f6e52657175657374a36c776f726b73706163655f69646277316a73657373696f6e5f69646273316c70617463685f7365745f6964627031"
            ),
            "PatchSetAbandonRequest wire drift"
        );

        let decide = CodeStudioPayload::ApprovalDecideRequest {
            workspace_id: "w1".to_string(),
            session_id: "s1".to_string(),
            approval_id: "a1".to_string(),
            decision: "deny".to_string(),
        };
        let bytes = crate::cbor::encode(&decide).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes(
                "a175417070726f76616c44656369646552657175657374a46c776f726b73706163655f69646277316a73657373696f6e5f69646273316b617070726f76616c5f6964626131686465636973696f6e6464656e79"
            ),
            "ApprovalDecideRequest wire drift"
        );

        let index = CodeStudioPayload::IndexStatusRequest {
            workspace_id: "w1".to_string(),
        };
        let bytes = crate::cbor::encode(&index).expect("encode");
        assert_eq!(
            bytes,
            hex_bytes("a172496e64657853746174757352657175657374a16c776f726b73706163655f6964627731"),
            "IndexStatusRequest wire drift"
        );
    }

    /// The stream family and the two late additions, pinned the same way. These
    /// are the variants a reconnect depends on: `after_seq` and `after_revision`
    /// are the resume cursors, and a rename of either field would turn a resume
    /// into a silent replay from zero.
    #[test]
    fn code_studio_stream_wire_golden() {
        let session = CodeStudioPayload::SessionStreamRequest {
            workspace_id: "w1".to_string(),
            session_id: "s1".to_string(),
            after_seq: 0,
        };
        assert_eq!(
            crate::cbor::encode(&session).expect("encode"),
            hex_bytes(
                "a17453657373696f6e53747265616d52657175657374a36c776f726b73706163655f6964627731\
                 6a73657373696f6e5f69646273316961667465725f73657100"
            ),
            "SessionStreamRequest wire drift"
        );

        let terminal = CodeStudioPayload::TerminalStreamRequest {
            workspace_id: "w1".to_string(),
            session_id: "s1".to_string(),
            terminal_id: "t1".to_string(),
            after_revision: 7,
        };
        assert_eq!(
            crate::cbor::encode(&terminal).expect("encode"),
            hex_bytes(
                "a1755465726d696e616c53747265616d52657175657374a46c776f726b73706163655f6964627731\
                 6a73657373696f6e5f69646273316b7465726d696e616c5f69646274316e61667465725f72657669\
                 73696f6e07"
            ),
            "TerminalStreamRequest wire drift"
        );

        let index = CodeStudioPayload::IndexStreamRequest {
            workspace_id: "w1".to_string(),
            after_seq: 0,
        };
        assert_eq!(
            crate::cbor::encode(&index).expect("encode"),
            hex_bytes(
                "a172496e64657853747265616d52657175657374a26c776f726b73706163655f6964627731\
                 6961667465725f73657100"
            ),
            "IndexStreamRequest wire drift"
        );

        let end = CodeStudioPayload::SessionStreamEnd {
            reason: "session_closed".to_string(),
        };
        assert_eq!(
            crate::cbor::encode(&end).expect("encode"),
            hex_bytes(
                "a17053657373696f6e53747265616d456e64a166726561736f6e6e73657373696f6e5f636c6f736564"
            ),
            "SessionStreamEnd wire drift"
        );

        let blob = CodeStudioPayload::PatchBlobGetRequest {
            workspace_id: "w1".to_string(),
            session_id: "s1".to_string(),
            blob_sha: "abc".to_string(),
        };
        assert_eq!(
            crate::cbor::encode(&blob).expect("encode"),
            hex_bytes(
                "a1735061746368426c6f6247657452657175657374a36c776f726b73706163655f6964627731\
                 6a73657373696f6e5f696462733168626c6f625f73686163616263"
            ),
            "PatchBlobGetRequest wire drift"
        );

        let terminals = CodeStudioPayload::TerminalsListRequest {
            workspace_id: "w1".to_string(),
            session_id: "s1".to_string(),
        };
        assert_eq!(
            crate::cbor::encode(&terminals).expect("encode"),
            hex_bytes(
                "a1745465726d696e616c734c69737452657175657374a26c776f726b73706163655f6964627731\
                 6a73657373696f6e5f6964627331"
            ),
            "TerminalsListRequest wire drift"
        );
    }

    /// The fields added after the family shipped are `#[serde(default)]`, which
    /// only helps if a peer that omits them still decodes. Encoding a map
    /// WITHOUT the new keys and decoding it proves the append stayed compatible
    /// — a plain round trip would not, because it always writes them.
    #[test]
    fn appended_fields_decode_when_a_peer_omits_them() {
        let session: SessionInfo = serde_json::from_str(
            r#"{"session_id":"s1","workspace_id":"w1","title":"t","branch":"cs/u/s1",
                "autonomy_mode":"normal","status":"idle","created_at":"","updated_at":"",
                "closed_at":null}"#,
        )
        .expect("session without flow ids");
        assert_eq!(session.flow_id, "");
        assert_eq!(session.flow_version_id, "");

        let approval: ApprovalInfo = serde_json::from_str(
            r#"{"approval_id":"a1","session_id":"s1","run_id":null,"capability":"git_push",
                "summary":"push","detail":null,"target_digest":"d","status":"pending",
                "decision":null,"mandatory_interactive":true,"requested_at":"",
                "decided_at":null,"decided_by":null}"#,
        )
        .expect("approval without patch set");
        assert_eq!(approval.patch_set_id, None);

        let run: RunInfo = serde_json::from_str(
            r#"{"run_id":"r1","ordinal":1,"kind":"root","trigger":"user","parent_run_id":null,
                "agent_id":null,"status":"running","note":null,"started_at":null,
                "finished_at":null}"#,
        )
        .expect("run without usage");
        assert_eq!(run.prompt_tokens, 0);
        assert_eq!(run.completion_tokens, 0);
        assert_eq!(run.model, None);
        assert_eq!(run.cost_usd, None);

        let workspace: WorkspaceInfo = serde_json::from_str(
            r#"{"workspace_id":"w1","name":"Core","slug":"core","node_id":"n1","node_name":"dev",
                "is_local":true,"exec_mode":"trusted_native","egress_enforcement":"unrestricted",
                "repo_kind":"git","repo_url":null,"repo_auth_kind":null,"has_secret":false,
                "default_branch":null,"target_branch":null,"autonomy_ceiling":"normal",
                "egress_policy":"org_approved","index_enabled":false,"status":"active",
                "status_detail":null,"my_role":"owner","member_count":1,"open_sessions":0,
                "disk_used_bytes":0,"quota_disk_bytes":null,"created_at":"","updated_at":""}"#,
        )
        .expect("workspace without the session quota and the waiting counter");
        assert_eq!(workspace.quota_sessions, None);
        assert_eq!(workspace.sessions_waiting, 0);
    }

    /// §9.1 names four allow scopes plus the refusal, `session_grants` stores
    /// the third one and the PEP reads it — so all five have to be expressible
    /// on the wire. `decision` is a plain string, which makes this the only
    /// place the closed set is checked; a variant per scope would be a wire
    /// break for a value that is never anything else.
    #[test]
    fn every_approval_scope_survives_the_wire() {
        for decision in [
            "allow_once",
            "allow_for_run",
            "allow_for_session",
            "always",
            "deny",
        ] {
            let request = CodeStudioPayload::ApprovalDecideRequest {
                workspace_id: "w1".into(),
                session_id: "s1".into(),
                approval_id: "a1".into(),
                decision: decision.to_string(),
            };
            let bytes = crate::cbor::encode(&request).expect("encode");
            assert_eq!(
                crate::cbor::decode::<CodeStudioPayload>(&bytes).expect("decode"),
                request,
                "decision '{decision}' did not survive the wire"
            );

            let answer = CodeStudioPayload::ApprovalDecideResponse {
                approval_id: "a1".into(),
                status: "decided".into(),
                decision: decision.to_string(),
                resumed: true,
            };
            let bytes = crate::cbor::encode(&answer).expect("encode");
            assert_eq!(
                crate::cbor::decode::<CodeStudioPayload>(&bytes).expect("decode"),
                answer
            );

            let stored = ApprovalInfo {
                approval_id: "a1".into(),
                session_id: "s1".into(),
                run_id: None,
                capability: "exec".into(),
                summary: "cargo test".into(),
                detail: None,
                target_digest: "d".into(),
                status: "decided".into(),
                decision: Some(decision.to_string()),
                mandatory_interactive: false,
                requested_at: String::new(),
                decided_at: None,
                decided_by: None,
                patch_set_id: None,
            };
            let bytes = crate::cbor::encode(&stored).expect("encode");
            assert_eq!(
                crate::cbor::decode::<ApprovalInfo>(&bytes).expect("decode"),
                stored
            );
        }
    }

    /// The variants added after the family shipped, pinned the same way as the
    /// original goldens. Bytes were derived from the CBOR rules — external
    /// tagging, definite-length maps, fields in declaration order — and checked
    /// against an independent decoder, so this test fails on a rename of a
    /// variant OR of any field inside it.
    #[test]
    fn code_studio_late_variants_wire_golden() {
        let candidates = CodeStudioPayload::WorkspaceMemberCandidatesRequest {
            workspace_id: None,
            query: "an".to_string(),
            limit: 20,
        };
        assert_eq!(
            crate::cbor::encode(&candidates).expect("encode"),
            hex_bytes(
                "a17820576f726b73706163654d656d62657243616e6469646174657352657175657374a36c776f\
                 726b73706163655f6964f665717565727962616e656c696d697414"
            ),
            "WorkspaceMemberCandidatesRequest wire drift"
        );

        let progress = CodeStudioPayload::IndexStreamProgress {
            seq: 3,
            job_id: "j1".to_string(),
            workspace_id: "w1".to_string(),
            branch: "main".to_string(),
            phase: "index".to_string(),
            files_done: 2,
            files_total: 5,
            chunks: 11,
            message: String::new(),
            terminal: false,
        };
        assert_eq!(
            crate::cbor::encode(&progress).expect("encode"),
            hex_bytes(
                "a173496e64657853747265616d50726f6772657373aa6373657103666a6f625f6964626a316c776f\
                 726b73706163655f6964627731666272616e6368646d61696e65706861736565696e6465786a6669\
                 6c65735f646f6e65026b66696c65735f746f74616c05666368756e6b730b676d6573736167656068\
                 7465726d696e616cf4"
            ),
            "IndexStreamProgress wire drift"
        );

        let links = CodeStudioPayload::ProjectLinkListRequest {
            workspace_id: "w1".to_string(),
        };
        assert_eq!(
            crate::cbor::encode(&links).expect("encode"),
            hex_bytes(
                "a17650726f6a6563744c696e6b4c69737452657175657374a16c776f726b73706163655f69646277\
                 31"
            ),
            "ProjectLinkListRequest wire drift"
        );

        let set = CodeStudioPayload::ProjectLinkSetRequest {
            workspace_id: "w1".to_string(),
            project_id: "p1".to_string(),
            linked: true,
        };
        assert_eq!(
            crate::cbor::encode(&set).expect("encode"),
            hex_bytes(
                "a17550726f6a6563744c696e6b53657452657175657374a36c776f726b73706163655f6964627731\
                 6a70726f6a6563745f6964627031666c696e6b6564f5"
            ),
            "ProjectLinkSetRequest wire drift"
        );

        let tree = CodeStudioPayload::RepoTreeRequest {
            workspace_id: "w1".to_string(),
            project_id: "p1".to_string(),
            commit: "c0ffee".to_string(),
            path_prefix: String::new(),
            limit: 100,
        };
        assert_eq!(
            crate::cbor::encode(&tree).expect("encode"),
            hex_bytes(
                "a16f5265706f5472656552657175657374a56c776f726b73706163655f6964627731\
                 6a70726f6a6563745f696462703166636f6d6d6974666330666665656b706174685f\
                 70726566697860656c696d69741864"
            ),
            "RepoTreeRequest wire drift"
        );
    }

    /// The answers of the late variants, by round trip: a listing is a shape,
    /// not a fixed byte string, and pinning one would only pin the sample.
    #[test]
    fn late_response_variants_round_trip() {
        let candidates = CodeStudioPayload::WorkspaceMemberCandidatesResponse {
            candidates: vec![WorkspaceUserCandidate {
                user_id: "u1".into(),
                display_name: "Anna".into(),
                email: "anna@example.invalid".into(),
            }],
        };
        let bytes = crate::cbor::encode(&candidates).expect("encode");
        assert_eq!(
            crate::cbor::decode::<CodeStudioPayload>(&bytes).expect("decode"),
            candidates
        );

        let links = CodeStudioPayload::ProjectLinkListResponse {
            workspace_id: "w1".into(),
            links: vec![ProjectLinkInfo {
                project_id: "p1".into(),
                project_name: "Rollout".into(),
                linked_by: "u1".into(),
                created_at: "2026-08-14T10:00:00Z".into(),
            }],
        };
        let bytes = crate::cbor::encode(&links).expect("encode");
        assert_eq!(
            crate::cbor::decode::<CodeStudioPayload>(&bytes).expect("decode"),
            links
        );

        let tree = CodeStudioPayload::RepoTreeResponse {
            workspace_id: "w1".into(),
            commit: "c0ffee".into(),
            entries: vec![RepoEntryInfo {
                path: "src/main.rs".into(),
                mode: "100644".into(),
                blob_oid: "aaa".into(),
            }],
            truncated: true,
        };
        let bytes = crate::cbor::encode(&tree).expect("encode");
        assert_eq!(
            crate::cbor::decode::<CodeStudioPayload>(&bytes).expect("decode"),
            tree
        );

        // §20 pins a COMMIT, so nothing in the repository surface may name a
        // location on the owner node. `path` inside an entry is a path in the
        // repository; a host path would show up as one of these keys.
        let json = serde_json::to_string(&tree).expect("json");
        assert!(!json.contains("worktree_path"), "{json}");
        assert!(!json.contains("host_path"), "{json}");
        assert!(!json.contains("\"dir\""), "{json}");

        let output = CodeStudioPayload::ExecOutputResponse {
            exec_id: "op-1".into(),
            status: "completed".into(),
            lines: vec!["$ sh -c ls".into(), "Cargo.toml".into()],
            next_seq: 2,
            has_more: false,
        };
        let bytes = crate::cbor::encode(&output).expect("encode");
        assert_eq!(
            crate::cbor::decode::<CodeStudioPayload>(&bytes).expect("decode"),
            output
        );
    }

    /// A command that asks for a mount the PEP will not grant has to learn that
    /// from the ANSWER, not by comparing what it sent against what came back
    /// and guessing. `exec` is pinned to `cow`, so the honest answer to `rw` is
    /// "you got `cow` and your writes are dropped" — and both halves of that
    /// sentence are fields.
    #[test]
    fn a_narrowed_exec_profile_says_so_in_its_own_fields() {
        let degraded = CodeStudioPayload::ExecStartResponse {
            exec_id: "op-1".into(),
            op_id: "op-1".into(),
            mount_access: "cow".into(),
            network_access: "none".into(),
            ephemeral: false,
            requested_mount_access: "rw".into(),
            writes_discarded: true,
        };
        let bytes = crate::cbor::encode(&degraded).expect("encode");
        assert_eq!(
            crate::cbor::decode::<CodeStudioPayload>(&bytes).expect("decode"),
            degraded
        );

        // A peer built before the two fields existed still decodes, and its
        // answer claims neither a request nor a discard it cannot know about.
        let older: CodeStudioPayload = serde_json::from_str(
            r#"{"ExecStartResponse":{"exec_id":"op-1","op_id":"op-1","mount_access":"cow",
                "network_access":"none","ephemeral":false}}"#,
        )
        .expect("answer without the degradation fields");
        let CodeStudioPayload::ExecStartResponse {
            requested_mount_access,
            writes_discarded,
            ..
        } = older
        else {
            panic!("unexpected variant");
        };
        assert_eq!(requested_mount_access, "");
        assert!(!writes_discarded);
    }

    fn saga_steps() -> Vec<MergeStepInfo> {
        [
            ("integration_worktree", "done", None),
            ("private_ref", "done", None),
            ("merge", "done", None),
            ("patch_set", "done", None),
            ("tests", "pending", Some("tests_not_run_by_merge")),
            ("review", "pending", Some("awaiting_review")),
            ("approval", "pending", Some("awaiting_review")),
            ("update_ref", "pending", Some("awaiting_review")),
        ]
        .into_iter()
        .map(|(step, status, detail)| MergeStepInfo {
            step: step.into(),
            status: status.into(),
            detail: detail.map(str::to_string),
        })
        .collect()
    }

    /// A merge answer without the step list still decodes (the field is
    /// appended), and with it the eight steps of §11.6 survive in order — the UI
    /// reads the test and review outcome from THESE, not from the patch set's
    /// status.
    #[test]
    fn a_merge_answer_carries_the_steps_of_the_whole_saga() {
        let steps = saga_steps();
        let payload = CodeStudioPayload::GitMergeResponse {
            op_id: "o1".into(),
            outcome: "clean".into(),
            conflict_files: Vec::new(),
            patch_set_id: Some("ps1".into()),
            integration_worktree: None,
            expected_old: "c0ffee".into(),
            steps: steps.clone(),
        };
        let bytes = crate::cbor::encode(&payload).expect("encode");
        let decoded: CodeStudioPayload = crate::cbor::decode(&bytes).expect("decode");
        assert_eq!(decoded, payload);
        let CodeStudioPayload::GitMergeResponse { steps: back, .. } = decoded else {
            panic!("variant changed");
        };
        assert_eq!(back, steps);
    }

    /// The finalize reports the SAME list, which is the point of unifying it:
    /// one process, one set of step names. An answer from a peer built before
    /// the field existed still decodes, with an empty list rather than a
    /// fabricated one.
    #[test]
    fn a_finalize_answer_reports_the_same_step_list() {
        let steps = saga_steps();
        let payload = CodeStudioPayload::GitMergeFinalizeResponse {
            op_id: "o1".into(),
            status: "merged".into(),
            merge_commit_oid: Some("beef".into()),
            error: None,
            steps: steps.clone(),
        };
        let bytes = crate::cbor::encode(&payload).expect("encode");
        assert_eq!(
            crate::cbor::decode::<CodeStudioPayload>(&bytes).expect("decode"),
            payload
        );

        let older: CodeStudioPayload = serde_json::from_str(
            r#"{"GitMergeFinalizeResponse":{"op_id":"o1","status":"merged",
                "merge_commit_oid":"beef","error":null}}"#,
        )
        .expect("answer without the step list");
        let CodeStudioPayload::GitMergeFinalizeResponse { steps, .. } = older else {
            panic!("variant changed");
        };
        assert!(steps.is_empty());
    }

    /// The terminal stream carries CELLS, never raw VT bytes (§7.9): the parser
    /// runs on the owner node so a container and a remote node behave
    /// identically. A snapshot and a delta share one shape, and the delta keeps
    /// the cursor and the grid size — a resize rewrites every row.
    #[test]
    fn terminal_stream_frames_round_trip() {
        let snapshot = CodeStudioPayload::TerminalStreamSnapshot {
            revision: 42,
            grid_rows: 24,
            grid_cols: 80,
            cursor: TerminalCursorInfo {
                row: 1,
                col: 5,
                visible: true,
            },
            rows: vec![TerminalCellRow {
                row: 0,
                text: "$ cargo test".into(),
                attrs: vec![0; 12],
            }],
        };
        let bytes = crate::cbor::encode(&snapshot).expect("encode");
        assert_eq!(
            crate::cbor::decode::<CodeStudioPayload>(&bytes).expect("decode"),
            snapshot
        );

        let delta = CodeStudioPayload::TerminalStreamDelta {
            revision: 43,
            grid_rows: 24,
            grid_cols: 80,
            cursor: TerminalCursorInfo {
                row: 2,
                col: 0,
                visible: true,
            },
            rows: vec![TerminalCellRow {
                row: 1,
                text: "ok".into(),
                attrs: vec![1, 1],
            }],
        };
        let bytes = crate::cbor::encode(&delta).expect("encode");
        assert_eq!(
            crate::cbor::decode::<CodeStudioPayload>(&bytes).expect("decode"),
            delta
        );

        // Nothing in a terminal frame may name a byte buffer: a client that
        // received VT bytes would need its own parser, which §7.9 removed.
        let json = serde_json::to_string(&delta).expect("json");
        assert!(!json.contains("bytes"), "{json}");
        assert!(!json.contains("data"), "{json}");
    }

    #[test]
    fn a_workspace_round_trips_without_losing_a_field() {
        let info = WorkspaceInfo {
            workspace_id: "w1".into(),
            name: "Core".into(),
            slug: "core".into(),
            node_id: "n1".into(),
            node_name: "dev-ryzen".into(),
            is_local: true,
            exec_mode: "trusted_native".into(),
            egress_enforcement: "unrestricted".into(),
            repo_kind: "git".into(),
            repo_url: Some("https://example.invalid/r.git".into()),
            repo_auth_kind: Some("token".into()),
            has_secret: true,
            default_branch: Some("main".into()),
            target_branch: Some("main".into()),
            autonomy_ceiling: "normal".into(),
            egress_policy: "org_approved".into(),
            index_enabled: false,
            status: "active".into(),
            status_detail: None,
            my_role: "owner".into(),
            member_count: 1,
            open_sessions: 0,
            disk_used_bytes: 0,
            quota_disk_bytes: None,
            created_at: "2026-08-14T10:00:00Z".into(),
            updated_at: "2026-08-14T10:00:00Z".into(),
            quota_sessions: Some(4),
            sessions_waiting: 2,
        };
        let payload = CodeStudioPayload::WorkspaceGetResponse {
            workspace: info.clone(),
            members: vec![WorkspaceMemberInfo {
                user_id: "u1".into(),
                display_name: "Piotr".into(),
                role: "owner".into(),
                added_by: "u1".into(),
                added_at: "2026-08-14T10:00:00Z".into(),
            }],
            provisioning: vec![ProvisionStepInfo {
                step: "repository".into(),
                status: "done".into(),
                detail: None,
                updated_at: "2026-08-14T10:00:00Z".into(),
            }],
        };
        let bytes = crate::cbor::encode(&payload).expect("encode");
        let decoded: CodeStudioPayload = crate::cbor::decode(&bytes).expect("decode");
        assert_eq!(decoded, payload);
    }

    #[test]
    fn the_wire_has_no_place_to_put_secret_material_in_a_response() {
        // The create request carries material once; nothing sent BACK may.
        let json = serde_json::to_string(&CodeStudioPayload::WorkspaceGetResponse {
            workspace: WorkspaceInfo {
                workspace_id: "w1".into(),
                name: "Core".into(),
                slug: "core".into(),
                node_id: "n1".into(),
                node_name: "dev".into(),
                is_local: true,
                exec_mode: "trusted_native".into(),
                egress_enforcement: "unrestricted".into(),
                repo_kind: "git".into(),
                repo_url: None,
                repo_auth_kind: Some("token".into()),
                has_secret: true,
                default_branch: None,
                target_branch: None,
                autonomy_ceiling: "normal".into(),
                egress_policy: "org_approved".into(),
                index_enabled: false,
                status: "active".into(),
                status_detail: None,
                my_role: "owner".into(),
                member_count: 1,
                open_sessions: 0,
                disk_used_bytes: 0,
                quota_disk_bytes: None,
                created_at: String::new(),
                updated_at: String::new(),
                quota_sessions: None,
                sessions_waiting: 0,
            },
            members: Vec::new(),
            provisioning: Vec::new(),
        })
        .expect("json");
        assert!(!json.contains("secret_material"));
        assert!(!json.contains("secret_ref"));
    }

    fn sample_patch_set() -> PatchSetInfo {
        PatchSetInfo {
            patch_set_id: "ps1".into(),
            session_id: "s1".into(),
            run_id: Some("r1".into()),
            scope: "work".into(),
            base_commit: "c0ffee".into(),
            status: "in_review".into(),
            file_count: 1,
            created_at: "2026-08-14T10:00:00Z".into(),
            decided_by: None,
            decided_at: None,
            op_id: None,
        }
    }

    /// A merge set names the operation it belongs to; a work set has none. The
    /// pair travels because "newest set of scope 'merge'" is a guess, and a
    /// session can hold two merges — one `held` on a conflict, one started
    /// after the target branch moved.
    #[test]
    fn a_merge_patch_set_names_its_operation() {
        let merge_set = PatchSetInfo {
            scope: "merge".into(),
            op_id: Some("op-42".into()),
            ..sample_patch_set()
        };
        let bytes = crate::cbor::encode(&merge_set).expect("encode");
        assert_eq!(
            crate::cbor::decode::<PatchSetInfo>(&bytes).expect("decode"),
            merge_set
        );

        let older: PatchSetInfo = serde_json::from_str(
            r#"{"patch_set_id":"ps1","session_id":"s1","run_id":null,"scope":"work",
                "base_commit":"c0ffee","status":"open","file_count":0,
                "created_at":"","decided_by":null,"decided_at":null}"#,
        )
        .expect("set without the operation id");
        assert_eq!(older.op_id, None);
    }

    /// The review payload is the deepest nesting in the family (set → files →
    /// hunks). A round trip proves the three blob hashes and the per-hunk
    /// statuses survive the wire — losing one would silently commit the wrong
    /// content, because the commit is built from `accepted_blob_sha`.
    #[test]
    fn a_patch_set_round_trips_with_its_hunks() {
        let payload = CodeStudioPayload::PatchSetGetResponse {
            patch_set: sample_patch_set(),
            files: vec![PatchFileInfo {
                patch_file_id: "pf1".into(),
                path: "src/main.rs".into(),
                old_path: None,
                change_kind: "modify".into(),
                status: "pending".into(),
                patch_base_blob_sha: Some("aaa".into()),
                current_blob_sha: Some("bbb".into()),
                accepted_blob_sha: None,
                mode: "100644".into(),
                hunks: vec![PatchHunkInfo {
                    patch_hunk_id: "ph1".into(),
                    idx: 0,
                    header: "@@ -1,3 +1,4 @@".into(),
                    content: "+let x = 1;\n".into(),
                    status: "pending".into(),
                }],
            }],
        };
        let bytes = crate::cbor::encode(&payload).expect("encode");
        let decoded: CodeStudioPayload = crate::cbor::decode(&bytes).expect("decode");
        assert_eq!(decoded, payload);
    }

    /// Timeline, operations and the VT grid share one property: the client
    /// resumes from a cursor, so `seq`, `next_seq` and `revision` have to
    /// survive exactly — an off-by-one there silently drops or replays events.
    #[test]
    fn cursor_bearing_payloads_round_trip() {
        let timeline = CodeStudioPayload::SessionTimelineResponse {
            session_id: "s1".into(),
            events: vec![TimelineEventInfo {
                seq: 4_294_967_400,
                event_id: "e1".into(),
                kind: "tool_call".into(),
                run_id: Some("r1".into()),
                agent_id: Some("code-implementer".into()),
                created_at: "2026-08-14T10:00:00Z".into(),
                payload_json: "{\"tool\":\"core.fs_write\"}".into(),
                security_relevant: false,
            }],
            next_seq: 4_294_967_400,
            has_more: true,
        };
        let bytes = crate::cbor::encode(&timeline).expect("encode");
        assert_eq!(
            crate::cbor::decode::<CodeStudioPayload>(&bytes).expect("decode"),
            timeline
        );

        let ops = CodeStudioPayload::SessionOperationsResponse {
            session_id: "s1".into(),
            operations: vec![OperationInfo {
                op_id: "o1".into(),
                run_id: Some("r1".into()),
                origin_kind: "tool_call".into(),
                op_kind: "exec".into(),
                capability: "exec".into(),
                idempotent: false,
                status: "unknown".into(),
                error: None,
                mount_access: Some("cow".into()),
                network_access: Some("none".into()),
                started_at: "2026-08-14T10:00:00Z".into(),
                finished_at: None,
            }],
        };
        let bytes = crate::cbor::encode(&ops).expect("encode");
        assert_eq!(
            crate::cbor::decode::<CodeStudioPayload>(&bytes).expect("decode"),
            ops
        );

        let snapshot = CodeStudioPayload::TerminalSnapshotResponse {
            terminal_id: "t1".into(),
            revision: 9_876_543_210,
            rows: 24,
            cols: 80,
            cursor_row: 3,
            cursor_col: 12,
            cursor_visible: true,
            cells: vec![TerminalCellRow {
                row: 0,
                text: "$ cargo test".into(),
                attrs: vec![0, 0, 1, 1, 1, 1, 1],
            }],
        };
        let bytes = crate::cbor::encode(&snapshot).expect("encode");
        assert_eq!(
            crate::cbor::decode::<CodeStudioPayload>(&bytes).expect("decode"),
            snapshot
        );
    }

    /// Every response of the extended family, encoded to JSON: none of them may
    /// name credential material or a vault handle. `WorkspaceSecretSetResponse`
    /// is the interesting one — it reports THAT a secret exists and its
    /// fingerprint, never the key.
    #[test]
    fn no_response_of_the_extended_family_can_carry_a_secret() {
        let responses = vec![
            CodeStudioPayload::WorkspaceSecretSetResponse {
                workspace_id: "w1".into(),
                has_secret: true,
                fingerprint: Some("SHA256:abc".into()),
            },
            CodeStudioPayload::PatchSetGetResponse {
                patch_set: sample_patch_set(),
                files: Vec::new(),
            },
            CodeStudioPayload::FileReadResponse {
                path: "src/main.rs".into(),
                content: "fn main() {}".into(),
                blob_sha: "aaa".into(),
                truncated: false,
                language: Some("rust".into()),
                total_lines: 1,
            },
            CodeStudioPayload::ExecStartResponse {
                exec_id: "x1".into(),
                op_id: "o1".into(),
                mount_access: "cow".into(),
                network_access: "none".into(),
                ephemeral: true,
                requested_mount_access: "rw".into(),
                writes_discarded: true,
            },
            CodeStudioPayload::GitSyncResponse {
                op_id: "o1".into(),
                status: "completed".into(),
                ahead: 1,
                behind: 0,
                error: None,
            },
            CodeStudioPayload::AgentCredentialSetResponse {
                credential: sample_agent_credential(),
            },
            CodeStudioPayload::AgentCredentialsListResponse {
                node_id: "n1".into(),
                credentials: vec![sample_agent_credential()],
                engines: vec!["claude-code".into(), "codex".into()],
            },
        ];
        for response in responses {
            let json = serde_json::to_string(&response).expect("json");
            assert!(!json.contains("secret_material"), "{json}");
            assert!(!json.contains("secret_ref"), "{json}");
            assert!(!json.contains("credential_material"), "{json}");
        }
    }

    fn sample_agent_credential() -> AgentCredentialInfo {
        AgentCredentialInfo {
            node_id: "n1".into(),
            engine_id: "claude-code".into(),
            provider_base_url: "https://api.anthropic.com".into(),
            fingerprint: Some("sha256:abc".into()),
            created_by: "u-admin".into(),
            created_at: "2026-08-14T10:00:00Z".into(),
            rotated_at: None,
            last_used_at: None,
        }
    }

    /// The provider credential travels in exactly one direction. The request
    /// names the material; the row that comes back names a digest, an upstream
    /// and who wrote it — and a struct with nowhere to put a key is a stronger
    /// guarantee than a handler that remembers not to fill one in.
    #[test]
    fn a_provider_credential_only_travels_towards_the_vault() {
        let request = CodeStudioPayload::AgentCredentialSetRequest {
            node_id: "n1".into(),
            engine_id: "claude-code".into(),
            provider_base_url: "https://api.anthropic.com".into(),
            credential_material: "sk-ant-secret".into(),
        };
        let bytes = crate::cbor::encode(&request).expect("encode");
        assert_eq!(
            crate::cbor::decode::<CodeStudioPayload>(&bytes).expect("decode"),
            request
        );

        let json = serde_json::to_string(&sample_agent_credential()).expect("json");
        assert!(!json.contains("sk-ant-secret"), "{json}");
        assert!(!json.contains("material"), "{json}");
    }

    /// A worktree row must not carry its on-disk location: that is a host path
    /// on the owner node, useless in the browser and a layout leak to every
    /// member of the workspace.
    #[test]
    fn a_worktree_listing_never_carries_a_host_path() {
        let json = serde_json::to_string(&CodeStudioPayload::WorktreesListResponse {
            session_id: "s1".into(),
            worktrees: vec![WorktreeInfo {
                worktree_id: "wt1".into(),
                session_id: "s1".into(),
                purpose: "integration".into(),
                op_id: Some("o1".into()),
                branch: None,
                head_commit: "c0ffee".into(),
                base_commit: "c0ffee".into(),
                state: "held".into(),
                created_at: "2026-08-14T10:00:00Z".into(),
                conflict_files: vec!["src/lib.rs".into()],
            }],
        })
        .expect("json");
        assert!(!json.contains("\"path\""), "{json}");
    }
}
