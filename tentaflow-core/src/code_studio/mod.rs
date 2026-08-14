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
//   vault (same DB, deliberately NOT in sync/core_registry.rs)
//       Key material, encrypted with the per-node SettingsCipher key. It never
//       leaves the node, so a workspace opened on another node reports
//       `secret_missing` instead of silently failing to authenticate.
//   runtime (`<data>/code-studio/<workspace_id>/workspace.db`)
//       Sessions, events, operations and patch sets of the OWNER node only.
//
// Plan and rationale: `docs/CODE_STUDIO_PLAN.md`.

pub mod models;
pub mod paths;
pub mod repository;
