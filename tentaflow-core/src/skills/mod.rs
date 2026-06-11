// ===== File: skills/mod.rs — Skills curator (Harness §3.2 grouping/umbrella
// mechanism). Report-then-apply collection maintenance: an auxiliary LLM proposes
// merge / umbrella / archive actions over the skill index, an admin approves a
// subset, apply mutates the `skills` table with a reversible pre-apply snapshot. =====

pub mod curator;

pub use curator::{
    apply_proposal, resolve_model, rollback_snapshot, router_complete, run_curator_review,
    start_curator_schedule_task, CuratorAction, CuratorActionKind, CuratorProposal,
    CuratorRunOutcome, CURATOR_INTERVAL_SETTING, CURATOR_MODEL_SETTING,
};
