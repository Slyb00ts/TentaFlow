// ===== File: skills/curator.rs — curator engine: LLM review pass that proposes
// merge / umbrella / archive actions over the skill index, plus a reversible
// admin-confirmed apply and a snapshot-based rollback (Harness §3.2). =====
//
// The curator NEVER mutates the library autonomously. `run_curator_review` only
// produces a structured proposal (and persists a pre-apply snapshot so a later
// apply is reversible). `apply_proposal` executes an admin-approved subset and
// records each mutation in `audit_log`. `rollback_snapshot` restores the captured
// pre-apply rows verbatim. Addon-sourced skills are never restructured — the LLM
// is told it may only propose `archive` (disable) for them, and apply enforces it.

use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::api::openai::types::{ChatCompletionRequest, ContentPart, Message, MessageContent};
use crate::db::{models::DbSkill, repository, DbPool};
use crate::routing::router::Router;

/// Settings key holding the auxiliary model the curator reviews with. Unset →
/// `"default"` (the router resolves it to the system default chat model).
pub const CURATOR_MODEL_SETTING: &str = "curator_model";

/// Settings key for the optional periodic REPORT cadence in hours. Unset, empty
/// or non-positive → manual only (no background task). The schedule only ever runs
/// the report and surfaces the proposal; it never auto-applies (§3.2).
pub const CURATOR_INTERVAL_SETTING: &str = "curator_interval_hours";

/// A skill that has gone unused for at least this many days is an archive
/// candidate the curator is told to consider. The proposal still only suggests —
/// nothing is archived without an admin approving the action.
const ARCHIVE_AFTER_DAYS: i64 = 90;

/// Hard cap on skills handed to the model in one review, oldest-used first. A
/// library larger than this is reviewed in name+description form regardless;
/// the cap bounds the prompt size, not the candidate set's correctness.
const MAX_REVIEW_SKILLS: usize = 400;

/// One proposed maintenance action over the skill collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CuratorActionKind {
    /// Merge near-duplicate skills into one survivor (`target_name` = survivor,
    /// must be one of `skill_ids`). Absorbed skills are archived.
    Merge,
    /// Cluster of related skills folded under a new umbrella skill named
    /// `target_name`; the cluster members are archived.
    Umbrella,
    /// Archive an unused/obsolete skill (never delete — §3.2). `target_name`
    /// unused.
    Archive,
}

/// A single proposed action with its rationale. `skill_ids` reference live skills
/// by id; `target_name` is the survivor (merge) or umbrella (umbrella) skill name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CuratorAction {
    pub action: CuratorActionKind,
    pub skill_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_name: Option<String>,
    #[serde(default)]
    pub rationale: String,
}

/// The structured proposal the review pass returns. Carries the snapshot id so
/// the dashboard can immediately apply (with an approved subset) or roll back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CuratorProposal {
    pub actions: Vec<CuratorAction>,
}

/// Result of a review pass: the proposal plus the snapshot it was captured for.
#[derive(Debug, Clone)]
pub struct CuratorRunOutcome {
    pub snapshot_id: String,
    pub proposal: CuratorProposal,
}

/// Internal projection of a skill for the review prompt + candidate set.
#[derive(Debug, Clone)]
struct ReviewSkill {
    id: String,
    name: String,
    description: String,
    tags: Vec<String>,
    source: String,
    status: String,
    use_count: i64,
    last_used_at: Option<String>,
    stale_days: Option<i64>,
}

/// Runs one curator review pass: builds the candidate set, asks the LLM (through
/// the router, auxiliary model) for a structured proposal, validates it against
/// live skills, persists a reversible pre-apply snapshot and returns the outcome.
/// The `complete` closure performs one LLM completion (prompt → text); production
/// passes a router-backed closure, tests pass a deterministic mock.
pub async fn run_curator_review(
    pool: &DbPool,
    created_by: Option<&str>,
    model: &str,
    complete: impl FnOnce(String) -> futures::future::BoxFuture<'static, Result<String>>,
) -> Result<CuratorRunOutcome> {
    let candidates = collect_candidates(pool)?;
    if candidates.is_empty() {
        // Nothing to review — return an empty proposal anchored to a snapshot so
        // the caller still gets a stable handle and a consistent UI path.
        let snapshot_id = uuid::Uuid::new_v4().to_string();
        let proposal = CuratorProposal { actions: vec![] };
        let proposal_json = serde_json::to_string(&proposal)?;
        repository::create_curator_snapshot(pool, &snapshot_id, &proposal_json, created_by, &[])?;
        return Ok(CuratorRunOutcome {
            snapshot_id,
            proposal,
        });
    }

    let prompt = build_review_prompt(model, &candidates);
    let raw = complete(prompt).await?;
    let proposal = parse_proposal(&raw, &candidates)?;

    // Snapshot every skill the proposal touches (plus umbrella targets that don't
    // exist yet) BEFORE returning, so a subsequent apply is reversible without a
    // second LLM pass.
    let snapshot_id = uuid::Uuid::new_v4().to_string();
    let snapshot_rows = build_snapshot_rows(pool, &proposal)?;
    let proposal_json = serde_json::to_string(&proposal)?;
    repository::create_curator_snapshot(
        pool,
        &snapshot_id,
        &proposal_json,
        created_by,
        &snapshot_rows,
    )?;

    Ok(CuratorRunOutcome {
        snapshot_id,
        proposal,
    })
}

/// Builds the candidate set: every non-archived skill, oldest-used first (the
/// least-recently-used skills are the most interesting consolidation targets),
/// capped at `MAX_REVIEW_SKILLS`. Already-archived skills are excluded — they are
/// the terminal state and the curator never resurrects them.
fn collect_candidates(pool: &DbPool) -> Result<Vec<ReviewSkill>> {
    let all = repository::list_skills(pool, &Default::default())?;
    let now = chrono::Utc::now();
    let mut review: Vec<ReviewSkill> = all
        .into_iter()
        .filter(|s| s.status != "archived")
        .map(|s| {
            let tags: Vec<String> = serde_json::from_str(&s.tags_json).unwrap_or_default();
            let stale_days = s.last_used_at.as_deref().and_then(|ts| {
                chrono::NaiveDateTime::parse_from_str(ts, "%Y-%m-%d %H:%M:%S")
                    .ok()
                    .map(|naive| {
                        let used = naive.and_utc();
                        (now - used).num_days()
                    })
            });
            ReviewSkill {
                id: s.id,
                name: s.name,
                description: s.description,
                tags,
                source: s.source,
                status: s.status,
                use_count: s.use_count,
                last_used_at: s.last_used_at,
                stale_days,
            }
        })
        .collect();
    // Oldest-used first; never-used (None last_used) sort as most stale.
    review.sort_by(|a, b| {
        let a_key = a.stale_days.unwrap_or(i64::MAX);
        let b_key = b.stale_days.unwrap_or(i64::MAX);
        b_key
            .cmp(&a_key)
            .then_with(|| a.use_count.cmp(&b.use_count))
    });
    review.truncate(MAX_REVIEW_SKILLS);
    Ok(review)
}

/// Renders the review prompt. The model is instructed to return ONLY a JSON object
/// `{"actions":[...]}` — the curator is a structured proposer, not a free-text
/// auditor. Addon skills are flagged so the model knows it may only `archive`
/// (i.e. propose disabling) them, never merge/umbrella-restructure.
fn build_review_prompt(_model: &str, candidates: &[ReviewSkill]) -> String {
    let mut skills_block = String::with_capacity(candidates.len() * 160);
    for s in candidates {
        let last = s
            .last_used_at
            .as_deref()
            .map(|t| format!(", last_used={t}"))
            .unwrap_or_else(|| ", never_used".to_string());
        let stale = s
            .stale_days
            .map(|d| format!(", stale_days={d}"))
            .unwrap_or_default();
        skills_block.push_str(&format!(
            "- id={} name=\"{}\" source={} status={} use_count={}{}{} tags=[{}]\n  description: {}\n",
            s.id,
            s.name,
            s.source,
            s.status,
            s.use_count,
            last,
            stale,
            s.tags.join(", "),
            s.description,
        ));
    }

    format!(
        "You are the TentaFlow skills CURATOR. You maintain a library of reusable LLM \
         instruction skills. Your job is to propose consolidation — NOT to apply anything. \
         A downstream admin reviews and approves your proposal one action at a time.\n\n\
         Goal: a small library of broad, CLASS-LEVEL skills. Many narrow near-duplicate \
         skills are a failure of the library, not a feature. Skills are matched by an agent \
         on DESCRIPTION, so one broad umbrella beats five narrow siblings for discoverability.\n\n\
         Propose three action kinds:\n\
         - \"merge\": two or more skills are near-duplicates. Pick the best survivor as \
         target_name (it MUST be one of the listed skill names); the others are archived.\n\
         - \"umbrella\": a cluster of related-but-distinct skills should be folded under a \
         NEW broader skill. target_name is the new umbrella skill name (kebab-case, not an \
         existing name); the cluster members are archived.\n\
         - \"archive\": a single skill is unused (consider stale_days >= {archive_days}) or \
         obsolete. It is archived (never deleted).\n\n\
         Hard rules:\n\
         1. NEVER restructure (merge/umbrella) a skill whose source=addon. For an addon skill \
         the ONLY action you may propose is \"archive\" (disable). Do not put an addon skill \
         in a merge/umbrella member list.\n\
         2. Reference skills by the exact id shown. Every action needs a one-sentence \
         rationale.\n\
         3. Be conservative: only propose an action you would defend to a maintainer. An \
         empty proposal (\"actions\":[]) is a valid, correct answer for a healthy library.\n\n\
         Skills under review:\n{skills}\n\n\
         Return ONLY a JSON object, no prose, no markdown fences, of the exact shape:\n\
         {{\"actions\":[{{\"action\":\"merge|umbrella|archive\",\"skill_ids\":[\"<id>\",...],\
         \"target_name\":\"<survivor-or-umbrella-name>\",\"rationale\":\"<one sentence>\"}}]}}",
        archive_days = ARCHIVE_AFTER_DAYS,
        skills = skills_block,
    )
}

/// Parses the model's JSON reply into a validated proposal. Tolerates a reply
/// wrapped in a ```json fence or with surrounding prose by extracting the first
/// balanced JSON object. Every action is validated against the live candidate set:
/// unknown ids are dropped, addon skills are removed from restructure actions, and
/// an action left with too few members is discarded. A reply that is not parseable
/// JSON at all is an error (the caller surfaces it; nothing is persisted).
fn parse_proposal(raw: &str, candidates: &[ReviewSkill]) -> Result<CuratorProposal> {
    let json = extract_json_object(raw)
        .ok_or_else(|| anyhow!("curator model did not return a JSON object"))?;
    let parsed: CuratorProposal = serde_json::from_str(&json)
        .map_err(|e| anyhow!("curator proposal is not valid JSON: {e}"))?;

    let by_id: std::collections::HashMap<&str, &ReviewSkill> =
        candidates.iter().map(|s| (s.id.as_str(), s)).collect();

    let mut actions = Vec::new();
    for mut action in parsed.actions {
        // Keep only ids that reference a live candidate.
        action
            .skill_ids
            .retain(|id| by_id.contains_key(id.as_str()));
        if action.skill_ids.is_empty() {
            continue;
        }
        let has_addon = action
            .skill_ids
            .iter()
            .any(|id| by_id.get(id.as_str()).map(|s| s.source.as_str()) == Some("addon"));

        match action.action {
            CuratorActionKind::Archive => {
                // Archive is per-skill: split a multi-id archive into one action each
                // so an admin can approve them individually. Addon archives allowed
                // (this is the "propose disable" path §3.2).
                for id in &action.skill_ids {
                    actions.push(CuratorAction {
                        action: CuratorActionKind::Archive,
                        skill_ids: vec![id.clone()],
                        target_name: None,
                        rationale: action.rationale.clone(),
                    });
                }
            }
            CuratorActionKind::Merge | CuratorActionKind::Umbrella => {
                // Restructure must not touch addon skills and needs >= 2 members.
                if has_addon || action.skill_ids.len() < 2 {
                    continue;
                }
                let Some(target) = action.target_name.as_deref().filter(|t| !t.is_empty()) else {
                    continue;
                };
                if matches!(action.action, CuratorActionKind::Merge) {
                    // Survivor must be one of the members (by name).
                    let survivor_ok = action
                        .skill_ids
                        .iter()
                        .filter_map(|id| by_id.get(id.as_str()))
                        .any(|s| s.name == target);
                    if !survivor_ok {
                        continue;
                    }
                }
                actions.push(CuratorAction {
                    action: action.action,
                    skill_ids: action.skill_ids,
                    target_name: Some(target.to_string()),
                    rationale: action.rationale,
                });
            }
        }
    }

    Ok(CuratorProposal { actions })
}

/// Extracts the first balanced JSON object from a model reply: strips an optional
/// leading ```json fence, then scans for the outermost `{...}` honoring string
/// literals and escapes.
fn extract_json_object(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let start = raw.find('{')?;
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for i in start..bytes.len() {
        let c = bytes[i] as char;
        if in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(raw[start..=i].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Computes the set of skills a proposal touches and snapshots their verbatim
/// pre-apply state (including reference files). Umbrella target names that don't
/// yet exist are recorded as `existed=false` rows keyed by the umbrella's
/// pre-allocated id, so rollback deletes the created umbrella.
fn build_snapshot_rows(
    pool: &DbPool,
    proposal: &CuratorProposal,
) -> Result<Vec<crate::db::models::DbCuratorSnapshotRow>> {
    use crate::db::models::DbCuratorSnapshotRow;

    let mut rows: Vec<DbCuratorSnapshotRow> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for action in &proposal.actions {
        for id in &action.skill_ids {
            if !seen.insert(id.clone()) {
                continue;
            }
            let Some(skill) = repository::get_skill(pool, id)? else {
                continue;
            };
            let files = repository::list_skill_files(pool, id)?;
            let files_json = serde_json::to_string(&files)?;
            rows.push(snapshot_row_from_skill(&skill, files_json));
        }
        // Umbrella target may not exist yet — record a "will create" marker keyed
        // by the umbrella's resolved-at-apply id is impossible here (apply mints
        // the id), so we instead record by name with a sentinel id derived from it,
        // which apply reconciles. We capture the placeholder only when no existing
        // skill carries the umbrella name.
        if matches!(action.action, CuratorActionKind::Umbrella) {
            if let Some(name) = action.target_name.as_deref() {
                if repository::get_skill_by_name(pool, name)?.is_none() {
                    let sentinel = umbrella_sentinel_id(name);
                    if seen.insert(sentinel.clone()) {
                        rows.push(DbCuratorSnapshotRow {
                            skill_id: sentinel,
                            existed: false,
                            name: Some(name.to_string()),
                            display_name: None,
                            description: None,
                            content: None,
                            tags_json: None,
                            category: None,
                            source: None,
                            source_ref: None,
                            status: None,
                            files_json: "[]".to_string(),
                        });
                    }
                }
            }
        }
    }
    Ok(rows)
}

/// Deterministic sentinel id for an umbrella that does not exist at review time.
/// Apply replaces it with the real minted id once the umbrella is created.
fn umbrella_sentinel_id(name: &str) -> String {
    format!("umbrella-pending:{name}")
}

fn snapshot_row_from_skill(
    skill: &DbSkill,
    files_json: String,
) -> crate::db::models::DbCuratorSnapshotRow {
    crate::db::models::DbCuratorSnapshotRow {
        skill_id: skill.id.clone(),
        existed: true,
        name: Some(skill.name.clone()),
        display_name: skill.display_name.clone(),
        description: Some(skill.description.clone()),
        content: Some(skill.content.clone()),
        tags_json: Some(skill.tags_json.clone()),
        category: skill.category.clone(),
        source: Some(skill.source.clone()),
        source_ref: skill.source_ref.clone(),
        status: Some(skill.status.clone()),
        files_json,
    }
}

/// Records one applied mutation in the audit log. `message` is the human-readable
/// summary (e.g. `merge: a,b → survivor`).
type AuditFn<'a> = dyn Fn(&str, Option<&str>, Option<&str>) + 'a;

/// Executes an admin-approved subset of a snapshot's proposal. `approved` lists the
/// action indices (into `snapshot.proposal_json.actions`) the admin ticked. Each
/// mutation is applied through the normal `upsert_skill` capture (so it replicates
/// fleet-wide) and recorded via `audit`. After a successful apply the snapshot is
/// marked `applied`. Returns the count of skills archived + umbrellas created.
///
/// Addon-sourced skills are never restructured: a merge/umbrella action whose
/// member set contains an addon skill is skipped defensively even if it slipped
/// past the proposal validation.
pub fn apply_proposal(
    pool: &DbPool,
    snapshot_id: &str,
    approved_indices: &[usize],
    actor_user_id: Option<&str>,
    audit: &AuditFn<'_>,
) -> Result<usize> {
    let snapshot = repository::get_curator_snapshot(pool, snapshot_id)?
        .ok_or_else(|| anyhow!("curator snapshot not found: {snapshot_id}"))?;
    if snapshot.status != "open" {
        return Err(anyhow!(
            "curator snapshot '{snapshot_id}' is {} — only an open snapshot can be applied",
            snapshot.status
        ));
    }
    let proposal: CuratorProposal = serde_json::from_str(&snapshot.proposal_json)
        .map_err(|e| anyhow!("stored proposal is corrupt: {e}"))?;

    let approved: std::collections::HashSet<usize> = approved_indices.iter().copied().collect();
    let mut mutated = 0usize;

    for (idx, action) in proposal.actions.iter().enumerate() {
        if !approved.contains(&idx) {
            continue;
        }
        match action.action {
            CuratorActionKind::Archive => {
                for id in &action.skill_ids {
                    if archive_skill(pool, id, actor_user_id)? {
                        mutated += 1;
                        audit(
                            "skill.curator_archive",
                            Some(&format!("skill:{id}")),
                            Some(&action.rationale),
                        );
                    }
                }
            }
            CuratorActionKind::Merge => {
                let Some(survivor_name) = action.target_name.as_deref() else {
                    continue;
                };
                if action_touches_addon(pool, action)? {
                    continue;
                }
                // Archive every member that is NOT the survivor.
                let mut archived = Vec::new();
                for id in &action.skill_ids {
                    let Some(skill) = repository::get_skill(pool, id)? else {
                        continue;
                    };
                    if skill.name == survivor_name {
                        continue;
                    }
                    if archive_skill(pool, id, actor_user_id)? {
                        mutated += 1;
                        archived.push(skill.name);
                    }
                }
                audit(
                    "skill.curator_merge",
                    Some(&format!("skill_name:{survivor_name}")),
                    Some(&format!(
                        "merged [{}] → {survivor_name}: {}",
                        archived.join(", "),
                        action.rationale
                    )),
                );
            }
            CuratorActionKind::Umbrella => {
                let Some(umbrella_name) = action.target_name.as_deref() else {
                    continue;
                };
                if action_touches_addon(pool, action)? {
                    continue;
                }
                // Create the umbrella skill (if it doesn't already exist), then
                // archive the cluster members folded under it.
                let umbrella_id = ensure_umbrella(pool, umbrella_name, action, actor_user_id)?;
                mutated += 1;
                audit(
                    "skill.curator_umbrella",
                    Some(&format!("skill:{umbrella_id}")),
                    Some(&format!(
                        "created umbrella {umbrella_name}: {}",
                        action.rationale
                    )),
                );
                for id in &action.skill_ids {
                    if archive_skill(pool, id, actor_user_id)? {
                        mutated += 1;
                        audit(
                            "skill.curator_archive",
                            Some(&format!("skill:{id}")),
                            Some(&format!("folded under {umbrella_name}")),
                        );
                    }
                }
            }
        }
    }

    repository::set_curator_snapshot_status(pool, snapshot_id, "applied")?;
    Ok(mutated)
}

/// True when any member of a restructure action references an addon-sourced skill.
fn action_touches_addon(pool: &DbPool, action: &CuratorAction) -> Result<bool> {
    for id in &action.skill_ids {
        if let Some(skill) = repository::get_skill(pool, id)? {
            if skill.source == "addon" {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Sets a skill's status to `archived` in place, preserving every other field.
/// Addon skills CAN be archived (the §3.2 "propose disable" path); restructure
/// callers gate on `action_touches_addon` first. Returns true when the row moved.
fn archive_skill(pool: &DbPool, id: &str, actor_user_id: Option<&str>) -> Result<bool> {
    let Some(skill) = repository::get_skill(pool, id)? else {
        return Ok(false);
    };
    if skill.status == "archived" {
        return Ok(false);
    }
    let params = crate::db::models::SkillParams {
        id: &skill.id,
        name: &skill.name,
        display_name: skill.display_name.as_deref(),
        description: &skill.description,
        content: &skill.content,
        tags_json: &skill.tags_json,
        category: skill.category.as_deref(),
        source: &skill.source,
        source_ref: skill.source_ref.as_deref(),
        status: "archived",
        created_by: skill.created_by.as_deref(),
        actor_user_id,
    };
    repository::upsert_skill(pool, &params)?;
    Ok(true)
}

/// Creates the umbrella skill if one with `name` does not already exist, returning
/// its id either way. The umbrella's content stitches together the folded members'
/// descriptions as labeled sections so the new skill is immediately useful; an
/// admin can refine it later in the editor.
fn ensure_umbrella(
    pool: &DbPool,
    name: &str,
    action: &CuratorAction,
    actor_user_id: Option<&str>,
) -> Result<String> {
    if let Some(existing) = repository::get_skill_by_name(pool, name)? {
        return Ok(existing.id);
    }
    let mut members = Vec::new();
    let mut tags: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for id in &action.skill_ids {
        if let Some(skill) = repository::get_skill(pool, id)? {
            members.push(skill.clone());
            if let Ok(member_tags) = serde_json::from_str::<Vec<String>>(&skill.tags_json) {
                tags.extend(member_tags);
            }
        }
    }
    let description = format!(
        "Umbrella skill consolidating {} related skills.",
        members.len()
    );
    let mut content = String::new();
    content.push_str(&format!("# {name}\n\n"));
    content.push_str(&action.rationale);
    content.push_str("\n\n");
    for m in &members {
        content.push_str(&format!("## {}\n\n{}\n\n", m.name, m.description));
    }
    let description: String = description
        .chars()
        .take(repository::SKILL_DESCRIPTION_MAX_CHARS)
        .collect();
    let content: String = content
        .chars()
        .take(repository::SKILL_CONTENT_MAX_CHARS)
        .collect();
    let tags_vec: Vec<String> = tags.into_iter().collect();
    let tags_json = serde_json::to_string(&tags_vec)?;
    let umbrella_id = uuid::Uuid::new_v4().to_string();
    let params = crate::db::models::SkillParams {
        id: &umbrella_id,
        name,
        display_name: None,
        description: &description,
        content: &content,
        tags_json: &tags_json,
        category: None,
        source: "user",
        source_ref: None,
        status: "active",
        created_by: actor_user_id,
        actor_user_id,
    };
    repository::upsert_skill(pool, &params)?;
    Ok(umbrella_id)
}

/// Restores a snapshot's captured pre-apply rows verbatim, undoing an apply. For
/// each `existed=true` row the skill is upserted back to its captured state (and
/// its reference files replaced); each `existed=false` row (an umbrella the apply
/// created) is deleted. The snapshot is then marked `rolled_back`. Returns the
/// number of skills touched. Rollback of a non-applied snapshot is rejected.
pub fn rollback_snapshot(
    pool: &DbPool,
    snapshot_id: &str,
    actor_user_id: Option<&str>,
    audit: &AuditFn<'_>,
) -> Result<usize> {
    let snapshot = repository::get_curator_snapshot(pool, snapshot_id)?
        .ok_or_else(|| anyhow!("curator snapshot not found: {snapshot_id}"))?;
    if snapshot.status != "applied" {
        return Err(anyhow!(
            "curator snapshot '{snapshot_id}' is {} — only an applied snapshot can be rolled back",
            snapshot.status
        ));
    }
    let rows = repository::list_curator_snapshot_rows(pool, snapshot_id)?;
    let mut restored = 0usize;

    for row in &rows {
        if row.existed {
            // Restore the captured pre-apply state in place.
            let (
                Some(name),
                Some(description),
                Some(content),
                Some(tags_json),
                Some(source),
                Some(status),
            ) = (
                row.name.as_deref(),
                row.description.as_deref(),
                row.content.as_deref(),
                row.tags_json.as_deref(),
                row.source.as_deref(),
                row.status.as_deref(),
            )
            else {
                continue;
            };
            let params = crate::db::models::SkillParams {
                id: &row.skill_id,
                name,
                display_name: row.display_name.as_deref(),
                description,
                content,
                tags_json,
                category: row.category.as_deref(),
                source,
                source_ref: row.source_ref.as_deref(),
                status,
                created_by: None,
                actor_user_id,
            };
            repository::upsert_skill(pool, &params)?;
            let files: Vec<crate::db::models::DbSkillFile> =
                serde_json::from_str(&row.files_json).unwrap_or_default();
            let files: Vec<(String, String)> =
                files.into_iter().map(|f| (f.path, f.content)).collect();
            repository::replace_skill_files(pool, &row.skill_id, &files, actor_user_id)?;
            restored += 1;
        } else {
            // The apply created this umbrella under `row.name`; delete the live
            // skill carrying that name (the minted id differs from the sentinel).
            if let Some(name) = row.name.as_deref() {
                if let Some(created) = repository::get_skill_by_name(pool, name)? {
                    if repository::delete_skill(pool, &created.id)? {
                        restored += 1;
                    }
                }
            }
        }
    }

    repository::set_curator_snapshot_status(pool, snapshot_id, "rolled_back")?;
    audit(
        "skill.curator_rollback",
        Some(&format!("snapshot:{snapshot_id}")),
        Some(&format!("restored {restored} skill(s)")),
    );
    Ok(restored)
}

/// Performs one LLM completion through the router for the curator review. Mirrors
/// the addon `llm_generate` path: a single user-message chat request, auxiliary
/// model, low temperature for a deterministic structured reply. Returns the first
/// choice's text content.
pub async fn router_complete(router: &Router, model: &str, prompt: String) -> Result<String> {
    let request = ChatCompletionRequest {
        reasoning_effort: None,
        modalities: None,
        audio: None,
        model: model.to_string(),
        messages: vec![Message {
            audio: None,
            role: "user".to_string(),
            content: Some(MessageContent::Text(prompt)),
            reasoning_content: None,
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }],
        temperature: Some(0.0),
        max_tokens: Some(4096),
        top_p: None,
        frequency_penalty: None,
        presence_penalty: None,
        stop: None,
        stream: false,
        stream_options: None,
        user: Some("curator".to_string()),
        response_format: None,
        tools: None,
        tool_choice: None,
        n: None,
        memory_options: None,
        audio_input: None,
        extra: Default::default(),
    };
    // Internal core maintenance job — no external caller, no session.
    let result = router
        .route_chat_completion(
            request,
            None,
            crate::flow_engine::dispatcher::FlowOrigin::System,
            crate::flow_engine::dispatcher::FlowActor::system(),
            None,
        )
        .await?;
    let text = result
        .response
        .choices
        .first()
        .and_then(|c| c.message.content.as_ref())
        .map(|content| match content {
            MessageContent::Text(t) => t.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
        })
        .unwrap_or_default();
    Ok(text)
}

/// Resolves the configured curator interval in hours from settings. Returns
/// `None` when unset / empty / non-positive (manual-only — the default).
fn resolve_interval_hours(pool: &DbPool) -> Option<u64> {
    let raw = repository::get_setting(pool, CURATOR_INTERVAL_SETTING)
        .ok()
        .flatten()?;
    let hours: u64 = raw.trim().parse().ok()?;
    (hours > 0).then_some(hours)
}

/// Resolves the curator model from settings, defaulting to `"default"` (the router
/// resolves that alias to the system default chat model).
pub fn resolve_model(pool: &DbPool) -> String {
    repository::get_setting(pool, CURATOR_MODEL_SETTING)
        .ok()
        .flatten()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "default".to_string())
}

/// Spawns the optional periodic REPORT task. When `curator_interval_hours` is set
/// to a positive value the task runs a review every interval and persists the
/// proposal as an `open` snapshot (surfaced in the dashboard) — it NEVER auto-
/// applies (§3.2). Unset → no task is spawned (manual-only). The interval is read
/// once at startup; changing it takes effect after a restart.
pub fn start_curator_schedule_task(pool: DbPool, router: std::sync::Arc<Router>) {
    let Some(hours) = resolve_interval_hours(&pool) else {
        return;
    };
    let interval = Duration::from_secs(hours.saturating_mul(3600).max(3600));
    tracing::info!("skills curator: scheduled report every {hours}h");
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(interval).await;
            let model = resolve_model(&pool);
            let router = router.clone();
            let call_model = model.clone();
            let outcome = run_curator_review(&pool, None, &model, move |prompt| {
                Box::pin(async move { router_complete(&router, &call_model, prompt).await })
            })
            .await;
            match outcome {
                Ok(o) => {
                    if o.proposal.actions.is_empty() {
                        tracing::debug!(
                            "skills curator: scheduled report found nothing to propose"
                        );
                    } else {
                        tracing::info!(
                            "skills curator: scheduled report proposed {} action(s), snapshot {}",
                            o.proposal.actions.len(),
                            o.snapshot_id
                        );
                    }
                }
                Err(e) => tracing::warn!("skills curator: scheduled report failed: {e}"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;
    use crate::db::models::SkillParams;
    use std::sync::Arc;

    fn db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("memory db");
        migrations::run(&conn).expect("migrations");
        // Apply captures an actor into the sync-capture table, whose
        // `actor_user_id` FKs `user_accounts`. Seed the actor the apply/rollback
        // tests pass so the capture write satisfies the constraint (production
        // always passes an authenticated user that exists in the table).
        conn.execute(
            "INSERT INTO user_accounts (id, username, password_hash, display_name, is_admin) \
             VALUES ('admin', 'admin', 'x', 'Admin', 1)",
            [],
        )
        .expect("seed actor");
        Arc::new(crate::db::Db::from_connection(conn))
    }

    fn seed_skill(pool: &DbPool, id: &str, name: &str, source: &str, desc: &str) {
        repository::upsert_skill(
            pool,
            &SkillParams {
                id,
                name,
                display_name: None,
                description: desc,
                content: &format!("# {name}\nbody"),
                tags_json: "[\"alpha\"]",
                category: None,
                source,
                source_ref: if source == "addon" {
                    Some("addon-x")
                } else {
                    None
                },
                status: "active",
                created_by: None,
                actor_user_id: None,
            },
        )
        .expect("seed skill");
    }

    fn no_audit() -> impl Fn(&str, Option<&str>, Option<&str>) {
        |_, _, _| {}
    }

    fn mock_reply(
        json: &str,
    ) -> impl FnOnce(String) -> futures::future::BoxFuture<'static, Result<String>> {
        let owned = json.to_string();
        move |_prompt| Box::pin(async move { Ok(owned) })
    }

    #[tokio::test]
    async fn review_produces_a_validated_proposal_and_snapshot() {
        let pool = db();
        seed_skill(&pool, "s1", "pdf-extract", "user", "extract text from pdf");
        seed_skill(&pool, "s2", "pdf-read", "user", "read pdf contents");
        let reply = r#"{"actions":[{"action":"merge","skill_ids":["s1","s2"],"target_name":"pdf-extract","rationale":"near duplicates"}]}"#;
        let outcome = run_curator_review(&pool, Some("admin"), "default", mock_reply(reply))
            .await
            .expect("review");
        assert_eq!(outcome.proposal.actions.len(), 1);
        assert_eq!(outcome.proposal.actions[0].action, CuratorActionKind::Merge);
        // Snapshot persisted with both touched skills.
        let rows =
            repository::list_curator_snapshot_rows(&pool, &outcome.snapshot_id).expect("rows");
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.existed));
    }

    #[tokio::test]
    async fn addon_skills_are_never_restructured() {
        let pool = db();
        seed_skill(&pool, "s1", "addon-tool", "addon", "addon provided");
        seed_skill(&pool, "s2", "user-tool", "user", "user authored");
        // Model wrongly proposes merging an addon skill — validation drops it.
        let reply = r#"{"actions":[{"action":"merge","skill_ids":["s1","s2"],"target_name":"user-tool","rationale":"x"}]}"#;
        let outcome = run_curator_review(&pool, None, "default", mock_reply(reply))
            .await
            .expect("review");
        assert!(
            outcome.proposal.actions.is_empty(),
            "a merge touching an addon skill must be rejected"
        );
    }

    #[tokio::test]
    async fn addon_skill_archive_is_allowed_as_disable() {
        let pool = db();
        seed_skill(&pool, "s1", "addon-tool", "addon", "addon provided");
        let reply = r#"{"actions":[{"action":"archive","skill_ids":["s1"],"rationale":"unused"}]}"#;
        let outcome = run_curator_review(&pool, None, "default", mock_reply(reply))
            .await
            .expect("review");
        assert_eq!(outcome.proposal.actions.len(), 1);
        assert_eq!(
            outcome.proposal.actions[0].action,
            CuratorActionKind::Archive
        );
    }

    #[tokio::test]
    async fn apply_umbrella_creates_skill_and_archives_members() {
        let pool = db();
        seed_skill(&pool, "s1", "csv-parse", "user", "parse csv");
        seed_skill(&pool, "s2", "tsv-parse", "user", "parse tsv");
        let reply = r#"{"actions":[{"action":"umbrella","skill_ids":["s1","s2"],"target_name":"tabular-parsing","rationale":"both parse tables"}]}"#;
        let outcome = run_curator_review(&pool, None, "default", mock_reply(reply))
            .await
            .expect("review");
        let audit = no_audit();
        let mutated = apply_proposal(&pool, &outcome.snapshot_id, &[0], Some("admin"), &audit)
            .expect("apply");
        // 1 umbrella created + 2 members archived = 3 mutations.
        assert_eq!(mutated, 3);
        let umbrella = repository::get_skill_by_name(&pool, "tabular-parsing")
            .expect("get")
            .expect("umbrella exists");
        assert_eq!(umbrella.status, "active");
        assert_eq!(umbrella.source, "user");
        assert_eq!(
            repository::get_skill(&pool, "s1").unwrap().unwrap().status,
            "archived"
        );
        assert_eq!(
            repository::get_skill(&pool, "s2").unwrap().unwrap().status,
            "archived"
        );
        // Snapshot moved to applied.
        let snap = repository::get_curator_snapshot(&pool, &outcome.snapshot_id)
            .unwrap()
            .unwrap();
        assert_eq!(snap.status, "applied");
    }

    #[tokio::test]
    async fn rollback_restores_archived_members_and_deletes_umbrella() {
        let pool = db();
        seed_skill(&pool, "s1", "csv-parse", "user", "parse csv");
        seed_skill(&pool, "s2", "tsv-parse", "user", "parse tsv");
        let reply = r#"{"actions":[{"action":"umbrella","skill_ids":["s1","s2"],"target_name":"tabular-parsing","rationale":"both parse tables"}]}"#;
        let outcome = run_curator_review(&pool, None, "default", mock_reply(reply))
            .await
            .expect("review");
        let audit = no_audit();
        apply_proposal(&pool, &outcome.snapshot_id, &[0], Some("admin"), &audit).expect("apply");
        let restored = rollback_snapshot(&pool, &outcome.snapshot_id, Some("admin"), &audit)
            .expect("rollback");
        // 2 members restored + 1 umbrella deleted.
        assert_eq!(restored, 3);
        assert_eq!(
            repository::get_skill(&pool, "s1").unwrap().unwrap().status,
            "active"
        );
        assert_eq!(
            repository::get_skill(&pool, "s2").unwrap().unwrap().status,
            "active"
        );
        assert!(
            repository::get_skill_by_name(&pool, "tabular-parsing")
                .unwrap()
                .is_none(),
            "the created umbrella is removed on rollback"
        );
        let snap = repository::get_curator_snapshot(&pool, &outcome.snapshot_id)
            .unwrap()
            .unwrap();
        assert_eq!(snap.status, "rolled_back");
    }

    #[tokio::test]
    async fn apply_only_executes_approved_actions() {
        let pool = db();
        seed_skill(&pool, "s1", "old-one", "user", "stale skill");
        seed_skill(&pool, "s2", "old-two", "user", "another stale skill");
        let reply = r#"{"actions":[
            {"action":"archive","skill_ids":["s1"],"rationale":"stale"},
            {"action":"archive","skill_ids":["s2"],"rationale":"stale"}
        ]}"#;
        let outcome = run_curator_review(&pool, None, "default", mock_reply(reply))
            .await
            .expect("review");
        assert_eq!(outcome.proposal.actions.len(), 2);
        let audit = no_audit();
        // Approve only the first action.
        let mutated = apply_proposal(&pool, &outcome.snapshot_id, &[0], Some("admin"), &audit)
            .expect("apply");
        assert_eq!(mutated, 1);
        assert_eq!(
            repository::get_skill(&pool, "s1").unwrap().unwrap().status,
            "archived"
        );
        assert_eq!(
            repository::get_skill(&pool, "s2").unwrap().unwrap().status,
            "active",
            "the unapproved action must not run"
        );
    }

    #[tokio::test]
    async fn malformed_reply_with_prose_still_extracts_json() {
        let pool = db();
        seed_skill(&pool, "s1", "thing", "user", "a thing");
        let reply = "Here is my proposal:\n```json\n{\"actions\":[{\"action\":\"archive\",\"skill_ids\":[\"s1\"],\"rationale\":\"stale\"}]}\n```\nDone.";
        let outcome = run_curator_review(&pool, None, "default", mock_reply(reply))
            .await
            .expect("review");
        assert_eq!(outcome.proposal.actions.len(), 1);
    }

    #[tokio::test]
    async fn empty_library_returns_empty_proposal() {
        let pool = db();
        let reply = r#"{"actions":[]}"#;
        let outcome = run_curator_review(&pool, None, "default", mock_reply(reply))
            .await
            .expect("review");
        assert!(outcome.proposal.actions.is_empty());
    }

    #[tokio::test]
    async fn apply_rejects_non_open_snapshot() {
        let pool = db();
        seed_skill(&pool, "s1", "thing", "user", "a thing");
        let reply = r#"{"actions":[{"action":"archive","skill_ids":["s1"],"rationale":"stale"}]}"#;
        let outcome = run_curator_review(&pool, None, "default", mock_reply(reply))
            .await
            .expect("review");
        let audit = no_audit();
        apply_proposal(&pool, &outcome.snapshot_id, &[0], None, &audit).expect("apply");
        // Second apply on the now-applied snapshot is rejected.
        let err = apply_proposal(&pool, &outcome.snapshot_id, &[0], None, &audit).unwrap_err();
        assert!(err.to_string().contains("only an open snapshot"));
    }
}
