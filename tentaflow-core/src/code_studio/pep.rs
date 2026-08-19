// ===== File: code_studio/pep.rs — the one place that decides whether a call may happen =====
//
// Every tool call, terminal command and git operation of a session passes
// through `authorize`. The order of the rules is the design: a refusal beats a
// grant, a mandatory question beats any stored permission, and an `Allow` never
// exists without naming the sandbox profile the operation will run in — an
// allowance that does not say "in which profile" is not an allowance, it is a
// hole.
//
// What this module deliberately does NOT do is guard syscalls. It authorizes
// CALLS. A process that is already running is constrained by its profile and by
// the token its git shim holds, nothing else. Consequently every new capability
// must come with a profile or a token restriction — otherwise adding it here is
// theatre.

use super::models::{AutonomyMode, WorkspaceRole};

/// What a session may ask for. Kept as an enum rather than a string so a typo
/// cannot silently become an unknown-and-therefore-unchecked capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    FsRead,
    /// Writing the session's PLAN — an ordered list of tasks with their
    /// state. Deliberately NOT `FsWrite`: the plan is session state, not
    /// repository content, so a planner that holds no write tools must
    /// still be able to record what it decided, and recording it must not
    /// raise a file-write approval. Reading the plan needs only `FsRead`.
    TaskPlan,
    /// Semantic search over the index (§14). Separate from `fs_read` because it
    /// answers from a DERIVED artifact with its own bounds (§10: a prefix and a
    /// result limit), and a permission written for reading files should not
    /// silently decide what the index may answer.
    CodeSearch,
    FsWrite,
    FsDelete,
    Exec,
    Terminal,
    GitRead,
    GitBranch,
    GitNetwork,
    GitStage,
    GitCommit,
    GitPush,
    GitMerge,
    GitMergeFinalize,
    /// Creating and removing worktrees. System-only: it is how a session would
    /// step outside its own isolation.
    GitWorktree,
    NetEgress,
    CliDelegate,
    ReviewDecide,
    SecretManage,
    WorkspaceSettings,
    MemberManage,
}

impl Capability {
    pub fn slug(self) -> &'static str {
        match self {
            Capability::FsRead => "fs_read",
            Capability::CodeSearch => "code_search",
            Capability::FsWrite => "fs_write",
            Capability::FsDelete => "fs_delete",
            Capability::Exec => "exec",
            Capability::Terminal => "terminal",
            Capability::GitRead => "git_read",
            Capability::GitBranch => "git_branch",
            Capability::GitNetwork => "git_network",
            Capability::GitStage => "git_stage",
            Capability::GitCommit => "git_commit",
            Capability::GitPush => "git_push",
            Capability::GitMerge => "git_merge",
            Capability::GitMergeFinalize => "git_merge_finalize",
            Capability::GitWorktree => "git_worktree",
            Capability::NetEgress => "net_egress",
            Capability::CliDelegate => "cli_delegate",
            Capability::ReviewDecide => "review_decide",
            Capability::SecretManage => "secret_manage",
            Capability::TaskPlan => "task_plan",
            Capability::WorkspaceSettings => "workspace_settings",
            Capability::MemberManage => "member_manage",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        Capability::ALL.into_iter().find(|c| c.slug() == slug)
    }

    /// Every capability, in a fixed order. Used wherever a caller has to reason
    /// about the whole set — resolving what a role holds, for instance —
    /// instead of re-listing the enum and drifting from it.
    pub const ALL: [Capability; 22] = [
        Capability::FsRead,
        Capability::CodeSearch,
        Capability::FsWrite,
        Capability::FsDelete,
        Capability::Exec,
        Capability::Terminal,
        Capability::GitRead,
        Capability::GitBranch,
        Capability::GitNetwork,
        Capability::GitStage,
        Capability::GitCommit,
        Capability::GitPush,
        Capability::GitMerge,
        Capability::GitMergeFinalize,
        Capability::GitWorktree,
        Capability::NetEgress,
        Capability::CliDelegate,
        Capability::ReviewDecide,
        Capability::SecretManage,
        Capability::TaskPlan,
        Capability::WorkspaceSettings,
        Capability::MemberManage,
    ];

    /// Executed only by the coordinator. No model agent and no terminal has it,
    /// because it is how a session would leave its own isolation.
    pub fn is_system(self) -> bool {
        matches!(self, Capability::GitWorktree)
    }

    /// Always asks the user, whatever is stored. A standing "always" grant for
    /// these is refused at write time, not merely ignored here.
    ///
    /// This is also the set of IRREVERSIBLE operations: a push and a merge
    /// publish, and secret management moves credentials. The mesh path gives
    /// exactly these an extra permission-freshness probe before it acts
    /// (§12.1) — one list, two consumers, so the two can never drift apart.
    pub fn is_mandatory_interactive(self) -> bool {
        matches!(
            self,
            Capability::GitPush
                | Capability::GitMerge
                | Capability::GitMergeFinalize
                | Capability::SecretManage
        )
    }

    /// Lowest workspace role that may hold the capability at all.
    pub fn minimum_role(self) -> WorkspaceRole {
        match self {
            Capability::FsRead | Capability::CodeSearch | Capability::GitRead => {
                WorkspaceRole::Viewer
            }
            Capability::GitPush
            | Capability::GitMerge
            | Capability::GitMergeFinalize
            | Capability::SecretManage
            | Capability::WorkspaceSettings
            | Capability::MemberManage => WorkspaceRole::Owner,
            _ => WorkspaceRole::Editor,
        }
    }
}

/// Where the operation wants to act. The PEP never derives this itself — the
/// caller resolves it, and a target outside the session's worktree or outside
/// the allowlist is refused here.
#[derive(Debug, Clone)]
pub enum Target {
    /// Path already canonicalized by the filesystem layer.
    Path { inside_worktree: bool },
    /// Network destination already matched against the workspace allowlist.
    Host { allowlisted: bool },
    /// A git operation on the session's own branch, or on something else.
    Branch { is_session_branch: bool },
    /// A git remote the call points at, already checked against the address
    /// policy (`remote_policy`) — a forbidden address never reaches here.
    ///
    /// Private and LAN addresses are legal on purpose: a company git server on
    /// the LAN is the normal target of a code workspace. They are also how a
    /// session would reach the rest of the operator's network, so §11.4 makes
    /// naming one carry the `secret_manage` threshold. The PEP therefore has to
    /// see WHICH kind of remote it is, not just that there is one.
    Remote { is_private: bool },
    /// The program a command wants to run, named by its argv[0]. Every
    /// allowlist entry and every grant pattern for `exec` is written against
    /// that name, so a command that cannot name one is a command no permission
    /// can ever cover — and is refused here rather than run unbounded.
    Program { name: String },
    /// Nothing to constrain (read-only metadata, review decisions).
    None,
}

/// Mount and network access are independent axes. An `Allow` carries both, so
/// the executor never has to guess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SandboxProfile {
    pub mount: MountAccess,
    pub network: NetworkAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountAccess {
    ReadOnly,
    CopyOnWrite,
    ReadWrite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkAccess {
    None,
    Gateway,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow(SandboxProfile),
    AskUser { summary: String, kind: AskKind },
    Deny { reason: String },
}

/// Why the user is being asked. `PatchReview` is not a normal permission
/// prompt: it shows the diff and its answer decides what gets committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AskKind {
    Permission,
    PatchReview,
}

/// Everything the decision depends on, gathered by the caller so this function
/// stays pure and testable.
#[derive(Debug, Clone)]
pub struct SessionCtx {
    pub role: WorkspaceRole,
    pub autonomy: AutonomyMode,
    /// True when the caller is the coordinator itself rather than an agent,
    /// a terminal or the UI.
    pub is_coordinator: bool,
    /// A patch set of this session has been accepted and not yet consumed.
    pub has_accepted_patch_set: bool,
    /// The capability matches a workspace-level standing allowlist entry.
    pub allowlisted: bool,
    /// The capability matches a session grant.
    pub session_granted: bool,
    /// The capability matches a grant scoped to the current run.
    pub run_granted: bool,
}

/// The single decision point. The rule order is the contract; see the module
/// header for why refusals and mandatory questions come before grants.
pub fn authorize(ctx: &SessionCtx, cap: Capability, target: &Target) -> Decision {
    // 1. A system capability in anyone else's hands is a bug, not a request.
    if cap.is_system() && !ctx.is_coordinator {
        return deny(format!(
            "{} is executed by the coordinator only",
            cap.slug()
        ));
    }

    // 2. Role. A remote on a private address raises the floor to the one
    //    `secret_manage` carries (§11.4): pointing a workspace at the
    //    operator's own network is a credential-level decision, and the
    //    capability being exercised does not lower it.
    let minimum = required_role(cap, target);
    if ctx.role < minimum {
        return deny(format!(
            "{} requires the {} role",
            cap.slug(),
            minimum.slug()
        ));
    }

    // 3. Autonomy mode. `plan` is a real mode, not a hint: it cannot write,
    //    execute or reach the network at all.
    if let Some(reason) = autonomy_forbids(ctx.autonomy, cap) {
        return deny(reason);
    }

    // 4. Target outside the boundary. Checked before any grant, because a
    //    grant is about WHAT may happen, never about WHERE.
    if let Some(reason) = target_is_out_of_bounds(cap, target) {
        return deny(reason);
    }

    // 5. Mandatory questions skip every stored permission below.
    if cap.is_mandatory_interactive() {
        return DecisionBuilder::ask(
            format!("{} needs your confirmation", cap.slug()),
            AskKind::Permission,
        );
    }

    // 5a. A commit without an accepted patch set opens the review instead of
    //     failing: the agent is doing the right thing, the human decision is
    //     what is missing. After acceptance the same call resumes and commits
    //     the accepted blobs, never the worktree.
    if cap == Capability::GitCommit && !ctx.has_accepted_patch_set {
        return DecisionBuilder::ask(
            "review the changes before they are committed".to_string(),
            AskKind::PatchReview,
        );
    }

    // 5b. A remote on a private address carries the same threshold by §11.4.
    //     The role floor in rule 2 already refused anyone who could not manage
    //     this workspace's credentials; no stored permission may answer for it
    //     either, so this skips 6-9 exactly the way rule 5 does.
    if names_a_private_remote(target) {
        return DecisionBuilder::ask(
            format!("{} points at a remote on a private address", cap.slug()),
            AskKind::Permission,
        );
    }

    // 6-8. Standing permissions, widest scope first.
    let standing = ctx.allowlisted || ctx.session_granted || ctx.run_granted;
    if standing {
        return Decision::Allow(profile_for(ctx.autonomy, cap));
    }

    // 9. The autonomy mode may allow it without asking — but for the
    //    capabilities §9.5 marks "auto (allowlist)" that is an AND with steps
    //    6-8, not an alternative to them: a mode never authorizes a target
    //    nobody named.
    if matches!(silent_allowance(ctx.autonomy, cap), SilentAllowance::Always) {
        return Decision::Allow(profile_for(ctx.autonomy, cap));
    }

    // 10. Everything else is a question.
    DecisionBuilder::ask(
        format!("the agent wants to use {}", cap.slug()),
        AskKind::Permission,
    )
}

/// The profile an operation runs in once a person has allowed THIS call.
///
/// Both the agent path (`tools::execute`, after the operator answered its
/// approval) and the dashboard path (a decided `allow_once`) need the same
/// answer, and they must not hold two opinions about it: a mandatory-interactive
/// capability never reaches `Allow` in `authorize` — that is the whole point of
/// rule 5 — so asking again after the human said yes would refuse the operation
/// forever. Rules 1-4 still decide: the role, the mode and the boundary are not
/// something an approval can buy, and a non-mandatory capability that is still
/// held by another gate (a commit awaiting its review) stays held.
pub fn authorize_after_decision(ctx: &SessionCtx, cap: Capability, target: &Target) -> Decision {
    match authorize(ctx, cap, target) {
        // Both unconditional questions, rule 5 and rule 5b, are answered by the
        // same human decision and must therefore both resume. Leaving 5b out
        // would refuse a private-address remote forever, however the operator
        // answered.
        Decision::AskUser { .. }
            if cap.is_mandatory_interactive() || names_a_private_remote(target) =>
        {
            Decision::Allow(profile_for(ctx.autonomy, cap))
        }
        other => other,
    }
}

/// A target that carries `secret_manage`'s threshold whatever capability is
/// being exercised (§11.4). One predicate, three consumers — the role floor,
/// the unconditional question and the resume path — so they cannot drift into
/// disagreeing about which remotes are privileged.
fn names_a_private_remote(target: &Target) -> bool {
    matches!(target, Target::Remote { is_private: true })
}

/// Lowest role the call needs: the capability's own floor, raised to
/// `secret_manage`'s when the target is a private remote.
fn required_role(cap: Capability, target: &Target) -> WorkspaceRole {
    if names_a_private_remote(target) {
        cap.minimum_role()
            .max(Capability::SecretManage.minimum_role())
    } else {
        cap.minimum_role()
    }
}

struct DecisionBuilder;

impl DecisionBuilder {
    fn ask(summary: String, kind: AskKind) -> Decision {
        Decision::AskUser { summary, kind }
    }
}

fn deny(reason: String) -> Decision {
    Decision::Deny { reason }
}

fn autonomy_forbids(mode: AutonomyMode, cap: Capability) -> Option<String> {
    let forbidden = match mode {
        AutonomyMode::Plan => matches!(
            cap,
            Capability::FsWrite
                | Capability::FsDelete
                | Capability::Exec
                | Capability::Terminal
                | Capability::NetEgress
                | Capability::GitStage
                | Capability::GitCommit
                | Capability::GitPush
                | Capability::GitMerge
                | Capability::GitMergeFinalize
                | Capability::CliDelegate
        ),
        _ => false,
    };
    forbidden.then(|| format!("{} is not available in plan mode", cap.slug()))
}

/// How much of a capability a mode performs without a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SilentAllowance {
    /// The mode never runs it unattended; the operator is asked.
    Never,
    /// The mode runs it unattended, whatever the target.
    Always,
    /// The mode runs it unattended ONLY against a target somebody already
    /// named — a workspace allowlist entry, a session grant or a run grant.
    /// Without one the operator is asked, which is what makes `autonomous`
    /// bounded rather than unbounded.
    WithStandingGrant,
}

/// Which capabilities a mode performs without a prompt. Reads are always
/// automatic; writes from `auto_edit` up; commands and egress only in
/// `autonomous` and only against an allowlisted target (§9.5).
fn silent_allowance(mode: AutonomyMode, cap: Capability) -> SilentAllowance {
    let is_read = matches!(
        cap,
        Capability::FsRead
            | Capability::CodeSearch
            | Capability::GitRead
            | Capability::ReviewDecide
    );
    if is_read {
        return SilentAllowance::Always;
    }
    let edits = matches!(
        cap,
        Capability::FsWrite | Capability::FsDelete | Capability::GitStage
    );
    let commands = matches!(
        cap,
        Capability::Exec | Capability::Terminal | Capability::NetEgress
    );
    match mode {
        AutonomyMode::Plan | AutonomyMode::Normal => SilentAllowance::Never,
        AutonomyMode::AutoEdit if edits => SilentAllowance::Always,
        AutonomyMode::AutoEdit => SilentAllowance::Never,
        AutonomyMode::Autonomous if edits => SilentAllowance::Always,
        AutonomyMode::Autonomous if commands => SilentAllowance::WithStandingGrant,
        AutonomyMode::Autonomous => SilentAllowance::Never,
    }
}

fn target_is_out_of_bounds(cap: Capability, target: &Target) -> Option<String> {
    match target {
        Target::Path {
            inside_worktree: false,
        } => Some(format!(
            "{} targets a path outside the worktree",
            cap.slug()
        )),
        Target::Host { allowlisted: false } => Some(format!(
            "{} targets a host outside the allowlist",
            cap.slug()
        )),
        Target::Branch {
            is_session_branch: false,
        } if matches!(cap, Capability::GitPush | Capability::GitStage) => {
            Some(format!("{} may only touch the session branch", cap.slug()))
        }
        // A command with no program name cannot be matched by any allowlist
        // entry or grant pattern, so no permission could ever describe it.
        Target::Program { name } if name.trim().is_empty() => Some(format!(
            "{} names no program, so no permission can cover it",
            cap.slug()
        )),
        _ => None,
    }
}

/// Profile an allowed operation runs in. Reads never get a writable mount, and
/// nothing gets the network unless it is the capability being asked for.
fn profile_for(_mode: AutonomyMode, cap: Capability) -> SandboxProfile {
    let mount = match cap {
        Capability::FsWrite | Capability::FsDelete | Capability::GitStage => MountAccess::ReadWrite,
        // A build writes into its own layer, never into the worktree.
        Capability::Exec | Capability::Terminal => MountAccess::CopyOnWrite,
        _ => MountAccess::ReadOnly,
    };
    let network = match cap {
        // A push talks to a remote; a merge and a finalize are local object
        // operations and get no route just for being irreversible.
        Capability::NetEgress
        | Capability::GitNetwork
        | Capability::CliDelegate
        | Capability::GitPush => NetworkAccess::Gateway,
        _ => NetworkAccess::None,
    };
    SandboxProfile { mount, network }
}

/// Whether a decision may be stored as a standing grant — an `always` entry in
/// the workspace allowlist or a session-scoped grant. Called by the grant
/// writer, so a mandatory-interactive capability cannot be turned into a silent
/// allowance by writing straight to the table (§9.3 rule 5 allows those exactly
/// one outcome: `allow_once`).
pub fn may_store_always_grant(cap: Capability) -> bool {
    !cap.is_mandatory_interactive() && !cap.is_system()
}

/// How a stored grant is read, for the dashboard path and the agent path
/// alike: `*` matches anything, a trailing `*` matches a prefix, everything
/// else is exact, and a call that names NO target is covered only by `*`.
///
/// The last clause is what makes it fail-closed, and it is why the target is an
/// `Option` rather than a string. Two readings of the same row — one treating
/// "no target" as a value to compare, the other as "nothing to compare" — give
/// the same saved permission two different meanings, so there is one function
/// and everyone calls it.
///
/// Deliberately not a glob engine: a standing permission that is hard to read
/// is a standing permission nobody audits.
pub fn pattern_matches(pattern: &str, target: Option<&str>) -> bool {
    if pattern == "*" {
        return true;
    }
    let Some(target) = target else {
        // A narrow pattern must never widen into "everything of this
        // capability".
        return false;
    };
    match pattern.strip_suffix('*') {
        Some(prefix) => target.starts_with(prefix),
        None => pattern == target,
    }
}

/// The pattern a decision about this call is stored under.
///
/// A call with nothing narrower to name — a capability whose target is the
/// session itself, or a path target that IS the worktree root — has exactly one
/// honest spelling, `*`, and `pattern_matches` reads it back as the same
/// permission. The empty string is not that spelling: it matches no target and
/// no absent target, so a row carrying it is a permission nobody can exercise.
pub fn grant_pattern(target: Option<&str>) -> &str {
    match target {
        Some(pattern) if !pattern.is_empty() => pattern,
        _ => "*",
    }
}

/// Refuses a pattern that must not reach a grant table. Called by every writer
/// of a standing permission, because a pattern the matcher gives no meaning to
/// is a row a later reader is tempted to "repair" — and the repair that widens
/// it into a blanket grant is one keystroke away.
pub fn validate_grant_pattern(pattern: &str) -> Result<(), String> {
    if pattern.is_empty() {
        return Err(
            "a grant needs a target pattern; '*' is how 'whatever the target' is written"
                .to_string(),
        );
    }
    if pattern.len() > 512 {
        return Err("grant pattern is longer than 512 bytes".to_string());
    }
    if pattern.bytes().any(|b| b.is_ascii_control()) {
        return Err("grant pattern contains a control character".to_string());
    }
    Ok(())
}

/// Stable identity of "this capability against this target". One definition,
/// because two writers with two conventions produce grants that silently never
/// match — or, worse, match everything.
pub fn target_digest(cap: Capability, pattern: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(cap.slug().as_bytes());
    hasher.update([0u8]);
    hasher.update(pattern.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> SessionCtx {
        SessionCtx {
            role: WorkspaceRole::Editor,
            autonomy: AutonomyMode::Normal,
            is_coordinator: false,
            has_accepted_patch_set: false,
            allowlisted: false,
            session_granted: false,
            run_granted: false,
        }
    }

    fn is_allow(decision: &Decision) -> bool {
        matches!(decision, Decision::Allow(_))
    }

    #[test]
    fn a_system_capability_is_refused_to_everyone_but_the_coordinator() {
        let mut c = ctx();
        c.role = WorkspaceRole::Owner;
        c.allowlisted = true;
        c.session_granted = true;
        assert!(matches!(
            authorize(&c, Capability::GitWorktree, &Target::None),
            Decision::Deny { .. }
        ));

        c.is_coordinator = true;
        assert!(is_allow(&authorize(
            &c,
            Capability::GitWorktree,
            &Target::None
        )));
    }

    #[test]
    fn a_push_asks_every_single_time_no_matter_what_is_stored() {
        let mut c = ctx();
        c.role = WorkspaceRole::Owner;
        c.allowlisted = true;
        c.session_granted = true;
        c.run_granted = true;
        c.autonomy = AutonomyMode::Autonomous;
        for cap in [
            Capability::GitPush,
            Capability::GitMerge,
            Capability::GitMergeFinalize,
            Capability::SecretManage,
        ] {
            match authorize(
                &c,
                cap,
                &Target::Branch {
                    is_session_branch: true,
                },
            ) {
                Decision::AskUser { kind, .. } => assert_eq!(kind, AskKind::Permission),
                other => panic!("{} was not a question: {other:?}", cap.slug()),
            }
            assert!(
                !may_store_always_grant(cap),
                "{} could be stored as a standing grant",
                cap.slug()
            );
        }
    }

    #[test]
    fn a_commit_without_an_accepted_patch_set_opens_a_review_rather_than_failing() {
        let mut c = ctx();
        match authorize(&c, Capability::GitCommit, &Target::None) {
            Decision::AskUser { kind, .. } => assert_eq!(kind, AskKind::PatchReview),
            other => panic!("expected a review, got {other:?}"),
        }

        c.has_accepted_patch_set = true;
        c.session_granted = true;
        assert!(is_allow(&authorize(
            &c,
            Capability::GitCommit,
            &Target::None
        )));
    }

    #[test]
    fn an_accepted_patch_set_does_not_bypass_the_role_or_the_mode() {
        let mut c = ctx();
        c.has_accepted_patch_set = true;
        c.autonomy = AutonomyMode::Plan;
        assert!(matches!(
            authorize(&c, Capability::GitCommit, &Target::None),
            Decision::Deny { .. }
        ));

        let mut viewer = ctx();
        viewer.role = WorkspaceRole::Viewer;
        viewer.has_accepted_patch_set = true;
        assert!(matches!(
            authorize(&viewer, Capability::GitCommit, &Target::None),
            Decision::Deny { .. }
        ));
    }

    #[test]
    fn a_grant_never_widens_where_an_operation_may_act() {
        let mut c = ctx();
        c.allowlisted = true;
        c.session_granted = true;
        c.run_granted = true;
        c.autonomy = AutonomyMode::Autonomous;
        assert!(matches!(
            authorize(
                &c,
                Capability::FsWrite,
                &Target::Path {
                    inside_worktree: false
                }
            ),
            Decision::Deny { .. }
        ));
        assert!(matches!(
            authorize(
                &c,
                Capability::NetEgress,
                &Target::Host { allowlisted: false }
            ),
            Decision::Deny { .. }
        ));
        assert!(matches!(
            authorize(
                &c,
                Capability::GitPush,
                &Target::Branch {
                    is_session_branch: false
                }
            ),
            Decision::Deny { .. }
        ));
    }

    #[test]
    fn plan_mode_cannot_be_talked_into_writing_or_executing() {
        let mut c = ctx();
        c.autonomy = AutonomyMode::Plan;
        c.allowlisted = true;
        c.session_granted = true;
        c.run_granted = true;
        for cap in [
            Capability::FsWrite,
            Capability::Exec,
            Capability::Terminal,
            Capability::NetEgress,
            Capability::GitCommit,
        ] {
            assert!(
                matches!(
                    authorize(
                        &c,
                        cap,
                        &Target::Path {
                            inside_worktree: true
                        }
                    ),
                    Decision::Deny { .. }
                ),
                "{} slipped through plan mode",
                cap.slug()
            );
        }
        // Reading is still fine — that is the point of the mode.
        assert!(is_allow(&authorize(
            &c,
            Capability::FsRead,
            &Target::Path {
                inside_worktree: true
            }
        )));
    }

    #[test]
    fn autonomy_decides_only_what_happens_without_asking() {
        let inside = Target::Path {
            inside_worktree: true,
        };
        let program = Target::Program {
            name: "cargo".to_string(),
        };
        let mut normal = ctx();
        normal.autonomy = AutonomyMode::Normal;
        assert!(matches!(
            authorize(&normal, Capability::FsWrite, &inside),
            Decision::AskUser { .. }
        ));

        let mut auto_edit = ctx();
        auto_edit.autonomy = AutonomyMode::AutoEdit;
        assert!(is_allow(&authorize(
            &auto_edit,
            Capability::FsWrite,
            &inside
        )));
        assert!(matches!(
            authorize(&auto_edit, Capability::Exec, &program),
            Decision::AskUser { .. }
        ));

        // §9.5 gives `autonomous` "auto (allowlist)" for a command: the mode
        // decides that no one is asked, the allowlist decides WHICH command.
        let mut autonomous = ctx();
        autonomous.autonomy = AutonomyMode::Autonomous;
        assert!(matches!(
            authorize(&autonomous, Capability::Exec, &program),
            Decision::AskUser { .. }
        ));
        autonomous.allowlisted = true;
        assert!(is_allow(&authorize(
            &autonomous,
            Capability::Exec,
            &program
        )));
        // An edit needs no allowlist entry — the mode itself is the decision.
        let mut editing = ctx();
        editing.autonomy = AutonomyMode::Autonomous;
        assert!(is_allow(&authorize(&editing, Capability::FsWrite, &inside)));
    }

    #[test]
    fn a_viewer_can_read_and_nothing_else() {
        let mut viewer = ctx();
        viewer.role = WorkspaceRole::Viewer;
        // Every standing permission a viewer could possibly hold is set, so the
        // refusal below comes from the ROLE and from nothing else.
        viewer.allowlisted = true;
        viewer.session_granted = true;
        viewer.run_granted = true;
        viewer.has_accepted_patch_set = true;
        let inside = Target::Path {
            inside_worktree: true,
        };

        // The whole enum, so lowering `minimum_role` for one capability fails
        // here instead of quietly widening what a viewer holds.
        let readable: Vec<Capability> = Capability::ALL
            .into_iter()
            .filter(|c| c.minimum_role() == WorkspaceRole::Viewer)
            .collect();
        assert_eq!(
            readable,
            vec![
                Capability::FsRead,
                Capability::CodeSearch,
                Capability::GitRead
            ],
            "a capability was moved into the viewer column"
        );
        for cap in Capability::ALL {
            let decision = authorize(&viewer, cap, &inside);
            if readable.contains(&cap) {
                assert!(is_allow(&decision), "a viewer lost {}", cap.slug());
            } else {
                assert!(
                    matches!(decision, Decision::Deny { .. }),
                    "a viewer got {}: {decision:?}",
                    cap.slug()
                );
            }
        }
    }

    #[test]
    fn adversarial_autonomous_runs_any_command_without_ever_consulting_an_allowlist() {
        // §9.5 gives `autonomous` the entry "auto (allowlista)" for
        // `exec`/`terminal`, and the comment above `autonomy_allows_silently`
        // repeats it: "commands only in `autonomous`, and even there against an
        // allowlist the caller has already matched".
        //
        // Steps 6-8 and step 9 are an OR, not an AND: with no allowlist entry,
        // no session grant and no run grant, step 9 allows the call anyway. In
        // `autonomous` the model therefore executes ANY argv it likes, silently.
        // Note also that `tools::pep_target` hands `Target::None` for
        // `CoreToolName::Exec`, so step 4 constrains nothing either — the
        // program name never reaches a policy check at all.
        let mut c = ctx();
        c.autonomy = AutonomyMode::Autonomous;
        c.allowlisted = false;
        c.session_granted = false;
        c.run_granted = false;
        assert!(
            !is_allow(&authorize(&c, Capability::Exec, &Target::None)),
            "an un-allowlisted command ran automatically in autonomous mode"
        );
        assert!(
            !is_allow(&authorize(&c, Capability::NetEgress, &Target::None)),
            "un-allowlisted egress ran automatically in autonomous mode"
        );
    }

    #[test]
    fn an_answered_question_produces_the_profile_the_work_runs_in() {
        // The resume path of both callers: the human has just answered, and the
        // operation now needs a profile rather than the same question again.
        let mut c = ctx();
        c.role = WorkspaceRole::Owner;
        c.has_accepted_patch_set = true;
        let on_branch = Target::Branch {
            is_session_branch: true,
        };
        for cap in [
            Capability::GitPush,
            Capability::GitMerge,
            Capability::GitMergeFinalize,
            Capability::SecretManage,
        ] {
            assert!(
                matches!(
                    authorize(&c, cap, &on_branch),
                    Decision::AskUser { .. }
                ),
                "{} stopped asking",
                cap.slug()
            );
            match authorize_after_decision(&c, cap, &on_branch) {
                Decision::Allow(_) => {}
                other => panic!("{} is refused after the answer: {other:?}", cap.slug()),
            }
        }

        // An answer buys the capability, never the role, the mode or the
        // boundary — and never another gate's question.
        let mut viewer = c.clone();
        viewer.role = WorkspaceRole::Viewer;
        assert!(matches!(
            authorize_after_decision(&viewer, Capability::GitPush, &on_branch),
            Decision::Deny { .. }
        ));
        assert!(matches!(
            authorize_after_decision(
                &c,
                Capability::GitPush,
                &Target::Branch {
                    is_session_branch: false
                }
            ),
            Decision::Deny { .. }
        ));
        let mut unreviewed = ctx();
        unreviewed.session_granted = true;
        assert!(matches!(
            authorize_after_decision(&unreviewed, Capability::GitCommit, &Target::None),
            Decision::AskUser {
                kind: AskKind::PatchReview,
                ..
            }
        ));
    }

    /// §11.4: a private/LAN remote is legal but carries `secret_manage`'s
    /// threshold — the owner role, and a question no stored permission answers.
    #[test]
    fn a_private_remote_carries_the_secret_manage_threshold() {
        let private = Target::Remote { is_private: true };
        let public = Target::Remote { is_private: false };

        // Every standing permission there is, and the mode that asks for the
        // least: the escalation below can only come from the TARGET.
        let mut editor = ctx();
        editor.autonomy = AutonomyMode::Autonomous;
        editor.allowlisted = true;
        editor.session_granted = true;
        editor.run_granted = true;
        assert!(
            is_allow(&authorize(&editor, Capability::GitNetwork, &public)),
            "a public remote must stay at the capability's own threshold"
        );
        assert!(
            matches!(
                authorize(&editor, Capability::GitNetwork, &private),
                Decision::Deny { .. }
            ),
            "an editor reached a private remote"
        );

        let mut owner = editor.clone();
        owner.role = WorkspaceRole::Owner;
        match authorize(&owner, Capability::GitNetwork, &private) {
            Decision::AskUser { kind, .. } => assert_eq!(kind, AskKind::Permission),
            other => panic!("a stored grant answered for a private remote: {other:?}"),
        }
        // And the operator's answer resumes the call instead of asking forever.
        assert!(is_allow(&authorize_after_decision(
            &owner,
            Capability::GitNetwork,
            &private
        )));
        // The answer buys the target, never the role.
        assert!(matches!(
            authorize_after_decision(&editor, Capability::GitNetwork, &private),
            Decision::Deny { .. }
        ));
    }

    /// §9.1: the object of a permission is capability + target, and ONE saved
    /// row must mean the same thing to the dashboard and to the agent.
    ///
    /// Both paths used to carry their own matcher, and they disagreed on
    /// exactly the case a capability with no concrete target produces: the
    /// dashboard's took a string and matched `""` against `""`, the agent's
    /// took an `Option` and refused it. The same stored consent therefore
    /// authorized the operator's own calls and stayed invisible to the model.
    #[test]
    fn one_stored_grant_reads_the_same_whether_a_target_is_named_or_not() {
        // What a call with no narrower target stores, and what it matches.
        assert_eq!(grant_pattern(None), "*");
        assert_eq!(grant_pattern(Some("")), "*");
        assert_eq!(grant_pattern(Some("src/main.rs")), "src/main.rs");
        assert!(pattern_matches(grant_pattern(None), None));
        assert!(pattern_matches(grant_pattern(None), Some("anything")));

        assert!(pattern_matches("cargo", Some("cargo")));
        assert!(!pattern_matches("cargo", Some("cargo-audit")));
        assert!(pattern_matches("crates.io*", Some("crates.io:443")));
        assert!(!pattern_matches("crates.io*", Some("evil.example")));
        // A narrow pattern never widens into "everything of this capability".
        assert!(!pattern_matches("src/*", None));
        assert!(!pattern_matches("cargo", None));

        // The empty pattern has no reading at all, so no writer may store one.
        assert!(!pattern_matches("", None));
        assert!(!pattern_matches("", Some("src/main.rs")));
        assert!(validate_grant_pattern("").is_err());
        assert!(validate_grant_pattern("\u{7}cargo").is_err());
        assert!(validate_grant_pattern(&"x".repeat(513)).is_err());
        assert!(validate_grant_pattern("*").is_ok());
        assert!(validate_grant_pattern("cargo").is_ok());
    }

    #[test]
    fn every_allowance_names_the_profile_it_runs_in() {
        let mut c = ctx();
        c.session_granted = true;
        let inside = Target::Path {
            inside_worktree: true,
        };

        // A read never gets a writable mount.
        match authorize(&c, Capability::FsRead, &inside) {
            Decision::Allow(profile) => {
                assert_eq!(profile.mount, MountAccess::ReadOnly);
                assert_eq!(profile.network, NetworkAccess::None);
            }
            other => panic!("{other:?}"),
        }
        // A build writes into its own layer, not into the worktree.
        match authorize(&c, Capability::Exec, &inside) {
            Decision::Allow(profile) => assert_eq!(profile.mount, MountAccess::CopyOnWrite),
            other => panic!("{other:?}"),
        }
        // Only the network capabilities get a route.
        match authorize(
            &c,
            Capability::NetEgress,
            &Target::Host { allowlisted: true },
        ) {
            Decision::Allow(profile) => assert_eq!(profile.network, NetworkAccess::Gateway),
            other => panic!("{other:?}"),
        }
        match authorize(&c, Capability::FsWrite, &inside) {
            Decision::Allow(profile) => assert_eq!(profile.network, NetworkAccess::None),
            other => panic!("{other:?}"),
        }
    }
}
