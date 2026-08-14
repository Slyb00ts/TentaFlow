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
            Capability::WorkspaceSettings => "workspace_settings",
            Capability::MemberManage => "member_manage",
        }
    }

    /// Executed only by the coordinator. No model agent and no terminal has it,
    /// because it is how a session would leave its own isolation.
    fn is_system(self) -> bool {
        matches!(self, Capability::GitWorktree)
    }

    /// Always asks the user, whatever is stored. A standing "always" grant for
    /// these is refused at write time, not merely ignored here.
    fn is_mandatory_interactive(self) -> bool {
        matches!(
            self,
            Capability::GitPush
                | Capability::GitMerge
                | Capability::GitMergeFinalize
                | Capability::SecretManage
        )
    }

    /// Lowest workspace role that may hold the capability at all.
    fn minimum_role(self) -> WorkspaceRole {
        match self {
            Capability::FsRead | Capability::GitRead => WorkspaceRole::Viewer,
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

    // 2. Role.
    if ctx.role < cap.minimum_role() {
        return deny(format!(
            "{} requires the {} role",
            cap.slug(),
            cap.minimum_role().slug()
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

    // 6-8. Standing permissions, widest scope first.
    if ctx.allowlisted || ctx.session_granted || ctx.run_granted {
        return Decision::Allow(profile_for(ctx.autonomy, cap));
    }

    // 9. The autonomy mode may allow it without asking.
    if autonomy_allows_silently(ctx.autonomy, cap) {
        return Decision::Allow(profile_for(ctx.autonomy, cap));
    }

    // 10. Everything else is a question.
    DecisionBuilder::ask(
        format!("the agent wants to use {}", cap.slug()),
        AskKind::Permission,
    )
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

/// Which capabilities a mode performs without a prompt. Reads are always
/// automatic; writes from `auto_edit` up; commands only in `autonomous`, and
/// even there against an allowlist the caller has already matched.
fn autonomy_allows_silently(mode: AutonomyMode, cap: Capability) -> bool {
    let is_read = matches!(
        cap,
        Capability::FsRead | Capability::GitRead | Capability::ReviewDecide
    );
    if is_read {
        return true;
    }
    match mode {
        AutonomyMode::Plan | AutonomyMode::Normal => false,
        AutonomyMode::AutoEdit => matches!(
            cap,
            Capability::FsWrite | Capability::FsDelete | Capability::GitStage
        ),
        AutonomyMode::Autonomous => matches!(
            cap,
            Capability::FsWrite
                | Capability::FsDelete
                | Capability::GitStage
                | Capability::Exec
                | Capability::NetEgress
        ),
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
        Capability::NetEgress | Capability::GitNetwork | Capability::CliDelegate => {
            NetworkAccess::Gateway
        }
        _ => NetworkAccess::None,
    };
    SandboxProfile { mount, network }
}

/// Whether a decision may be stored as a standing "always" grant. Called by the
/// grant writer, so a mandatory-interactive capability cannot be turned into a
/// silent allowance by writing straight to the table.
pub fn may_store_always_grant(cap: Capability) -> bool {
    !cap.is_mandatory_interactive() && !cap.is_system()
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
            authorize(&auto_edit, Capability::Exec, &inside),
            Decision::AskUser { .. }
        ));

        let mut autonomous = ctx();
        autonomous.autonomy = AutonomyMode::Autonomous;
        assert!(is_allow(&authorize(&autonomous, Capability::Exec, &inside)));
    }

    #[test]
    fn a_viewer_can_read_and_nothing_else() {
        let mut viewer = ctx();
        viewer.role = WorkspaceRole::Viewer;
        let inside = Target::Path {
            inside_worktree: true,
        };
        assert!(is_allow(&authorize(&viewer, Capability::FsRead, &inside)));
        assert!(is_allow(&authorize(
            &viewer,
            Capability::GitRead,
            &Target::None
        )));
        for cap in [
            Capability::FsWrite,
            Capability::Exec,
            Capability::GitCommit,
            Capability::CliDelegate,
        ] {
            assert!(
                matches!(authorize(&viewer, cap, &inside), Decision::Deny { .. }),
                "a viewer got {}",
                cap.slug()
            );
        }
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
