// ===== File: code_studio/mod.rs — built-in Code Studio module (NOT an addon) =====
//
// A development environment for TentaFlow's own addons and other applications:
// repositories, editor, terminal, git — driven by a multi-agent harness whose
// every step is a visible block in the Flow Builder. Codex and Claude Code are
// one of its agents, not a separate world.
//
// Layering, and why it is split this way:
//
//   registry (this crate's main DB, migration 125)
//       WHAT exists and who may touch it. Travels through the Sync Ledger, so
//       a workspace is visible from every node of the org — including the ones
//       that cannot run it.
//   content (the app instance's own `code_studio.db`, see `db.rs`)
//       Key material, encrypted with the per-node SettingsCipher key, and the
//       provisioning saga state. Kept OUT of the main DB so the sync engine
//       cannot reach it by construction; a workspace opened on another node
//       reports `secret_missing` instead of silently failing to authenticate.
//   runtime (`<data>/code-studio/<workspace_id>/workspace.db`)
//       Sessions, events, operations and patch sets of the OWNER node only.
//
// Plan and rationale: `docs/CODE_STUDIO_PLAN.md`.

pub mod artifacts;
pub mod assertion;
pub mod audit_outbox;
pub mod cli_adapter;
pub mod cli_bridge;
pub mod db;
pub mod egress;
pub mod events;
pub mod exec;
pub mod fs;
pub mod git_broker;
pub mod index;
pub mod location;
pub mod mesh_stream;
pub mod models;
pub mod operations;
pub mod patch;
pub mod paths;
pub mod pep;
pub mod project_link;
pub mod provisioning;
#[path = "../../../tentaflow-containers/agents/native/process_sandbox.rs"]
pub mod process_sandbox;
pub mod redact;
pub mod remote_policy;
pub mod remote_proxy;
pub mod repository;
pub mod sandbox;
pub mod session;
pub mod sync_capture;
pub mod terminal;
pub mod tools;
pub mod vault;
pub mod workspace_db;

/// The wire spelling of a process-sandbox refusal.
///
/// The picker switches on the variant, never on the English prose a probe
/// produces — a localized message cannot be recovered from a sentence, and
/// substring-matching one in the dashboard is how the wrong message ends up
/// under the wrong node. ONE mapping, because two of them (the node picker and
/// the mesh advertisement) would eventually disagree on a new cause.
pub fn sandbox_cause(
    unavailable: &process_sandbox::SandboxUnavailable,
) -> tentaflow_protocol::code_studio::ProcessSandboxCause {
    use process_sandbox::SandboxUnavailable as Unavailable;
    use tentaflow_protocol::code_studio::ProcessSandboxCause as Cause;
    match unavailable {
        Unavailable::NoSandboxBinary => Cause::NoSandboxBinary,
        Unavailable::SupervisorNotInitialized => Cause::SupervisorNotInitialized,
        Unavailable::MissingCoalition => Cause::MissingCoalition,
        Unavailable::GuiSessionRequired => Cause::GuiSessionRequired,
    }
}

/// Whether THIS node can host a container-isolated workspace. Both halves
/// matter: a build without the `docker` feature has no sandbox backend at all,
/// and a node whose runtime socket does not answer cannot keep the promise
/// either.
///
/// One statement, two callers: the create gate and the `NodeInfo` this node
/// advertises to peers. Reading the same predicate is what stops an
/// advertisement from promising a mode the gate then refuses; a false negative
/// (a runtime reached over `DOCKER_HOST=tcp://…`, which the socket probe cannot
/// confirm) only hides the mode, it never over-promises.
pub fn container_runtime_available() -> bool {
    cfg!(feature = "docker") && egress::node_capabilities().container_runtime
}
