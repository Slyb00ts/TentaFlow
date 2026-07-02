// ===== File: ml_studio/repository.rs — SQL access for ML Studio projects =====

use anyhow::{bail, Result};
use rusqlite::{params, OptionalExtension};

use super::models::{
    Dataset, MemberStatus, ModelSummary, Project, ProjectMember, ProjectRole, ProjectSummary,
    ProjectType, ResourceGrant, TrainingRunSummary, GRANT_RESOURCE_KINDS, GRANT_SUBJECT_KINDS,
};

/// Lists projects the user is an active member of (owner or invited),
/// newest first, each with its per-project KPIs (dataset/model count) plus the
/// user's role and an `is_owner` flag. Membership is the access boundary: a
/// project the user is not a member of is invisible here.
pub fn list_projects(user_id: &str) -> Result<Vec<ProjectSummary>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT p.project_id, p.name, p.description, p.project_type, p.status, \
                p.owner_user_id, p.org_id, p.created_at, p.updated_at, \
                (SELECT COUNT(*) FROM models m WHERE m.project_id = p.project_id), \
                (SELECT COUNT(*) FROM datasets d WHERE d.project_id = p.project_id), \
                (SELECT COUNT(*) FROM training_runs t WHERE t.project_id = p.project_id), \
                pm.role \
         FROM projects p \
         JOIN project_members pm ON pm.project_id = p.project_id \
         WHERE pm.user_id = ?1 AND pm.status = 'active' \
         ORDER BY p.updated_at DESC, p.name",
    )?;
    let rows = stmt.query_map(params![user_id], read_summary)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Creates a new project owned by the calling user inside their organization.
/// Validates the name and project type; returns the created summary.
pub fn create_project(
    owner_user_id: &str,
    org_id: &str,
    name: &str,
    description: &str,
    project_type: &str,
) -> Result<ProjectSummary> {
    let name = name.trim();
    if name.is_empty() {
        bail!("project name is required");
    }
    if name.chars().count() > 128 {
        bail!("project name must be at most 128 characters");
    }
    if description.chars().count() > 4096 {
        bail!("project description must be at most 4096 characters");
    }
    let kind = ProjectType::from_slug(project_type)
        .ok_or_else(|| anyhow::anyhow!("unknown project_type '{}'", project_type))?;

    let project_id = uuid::Uuid::new_v4().to_string();
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(|e| anyhow::anyhow!("db write: {e}"))?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO projects \
             (project_id, name, description, project_type, status, owner_user_id, org_id) \
         VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?6)",
        params![
            project_id,
            name,
            description,
            kind.slug(),
            owner_user_id,
            org_id
        ],
    )?;
    tx.execute(
        "INSERT INTO project_members \
             (project_id, user_id, role, status, invited_by) \
         VALUES (?1, ?2, 'owner', 'active', ?2)",
        params![project_id, owner_user_id],
    )?;
    tx.commit()?;
    drop(conn);

    get_project(owner_user_id, &project_id)?
        .ok_or_else(|| anyhow::anyhow!("project not found after create"))
}

/// Fetches a single project (with KPIs and the user's role) scoped to the user's
/// active membership. Returns `None` when the user is not an active member, so a
/// non-member cannot probe a project's existence by id.
pub fn get_project(user_id: &str, project_id: &str) -> Result<Option<ProjectSummary>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    conn.query_row(
        "SELECT p.project_id, p.name, p.description, p.project_type, p.status, \
                p.owner_user_id, p.org_id, p.created_at, p.updated_at, \
                (SELECT COUNT(*) FROM models m WHERE m.project_id = p.project_id), \
                (SELECT COUNT(*) FROM datasets d WHERE d.project_id = p.project_id), \
                (SELECT COUNT(*) FROM training_runs t WHERE t.project_id = p.project_id), \
                pm.role \
         FROM projects p \
         JOIN project_members pm ON pm.project_id = p.project_id \
         WHERE pm.user_id = ?1 AND pm.status = 'active' AND p.project_id = ?2",
        params![user_id, project_id],
        read_summary,
    )
    .optional()
    .map_err(Into::into)
}

/// Returns the membership role slug of `user_id` in `project_id`, or `None` when
/// the user has no membership row. Used as the authorization primitive for
/// owner-only project actions. All membership rows are `active`, so a returned
/// role always grants the access tied to that role.
pub fn member_role(project_id: &str, user_id: &str) -> Result<Option<String>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    conn.query_row(
        "SELECT role FROM project_members WHERE project_id = ?1 AND user_id = ?2",
        params![project_id, user_id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Lists every member of a project, owner first then by creation time. No
/// authorization is enforced here; callers gate visibility.
pub fn list_members(project_id: &str) -> Result<Vec<ProjectMember>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT project_id, user_id, role, status, invited_by, created_at \
         FROM project_members \
         WHERE project_id = ?1 \
         ORDER BY (role = 'owner') DESC, created_at, user_id",
    )?;
    let rows = stmt.query_map(params![project_id], read_member)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Rozwiązuje nazwy wyświetlane użytkowników z katalogu CORE (`user_accounts`).
/// Lookup jest per-id w pętli, bo `project_members` żyje w `ml_studio.db`, a
/// `user_accounts` w bazie CORE (osobny pool) — to inne pliki SQLite, więc nie da
/// się zrobić JOIN-a między bazami w jednym zapytaniu. Zwraca mapę id→nazwa tylko
/// dla znalezionych wierszy; gdy CORE jest niedostępne lub id nie istnieje,
/// pomija je (frontend ma własny fallback do UUID). Nie panikuje przy błędzie.
pub fn resolve_display_names(
    user_ids: &[String],
) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let Some(core) = crate::db::global_pool() else {
        return out;
    };
    let Ok(conn) = core.read() else {
        return out;
    };
    for id in user_ids {
        let name: Option<String> = conn
            .query_row(
                "SELECT COALESCE(NULLIF(display_name, ''), NULLIF(username, ''), id) \
                 FROM user_accounts WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .ok()
            .flatten();
        if let Some(name) = name {
            out.insert(id.clone(), name);
        }
    }
    out
}

/// Invites a user to a project with a grantable role (`editor`/`viewer`). Only
/// the project owner may invite. There is no acceptance step: the new member is
/// created `active` and gains access immediately.
pub fn invite_member(
    project_id: &str,
    inviter_user_id: &str,
    invitee_user_id: &str,
    role: &str,
) -> Result<ProjectMember> {
    let role = ProjectRole::from_grantable_slug(role)
        .ok_or_else(|| anyhow::anyhow!("role must be 'editor' or 'viewer'"))?;
    let invitee_user_id = invitee_user_id.trim();
    if invitee_user_id.is_empty() {
        bail!("invitee user id is required");
    }
    if invitee_user_id == inviter_user_id {
        bail!("cannot invite yourself");
    }

    let pool = super::db::pool()?;
    let conn = pool.write().map_err(|e| anyhow::anyhow!("db write: {e}"))?;
    require_owner(&conn, project_id, inviter_user_id)?;

    let existing: Option<String> = conn
        .query_row(
            "SELECT role FROM project_members WHERE project_id = ?1 AND user_id = ?2",
            params![project_id, invitee_user_id],
            |row| row.get(0),
        )
        .optional()?;
    if existing.is_some() {
        bail!("user is already a member of this project");
    }

    conn.execute(
        "INSERT INTO project_members \
             (project_id, user_id, role, status, invited_by) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            project_id,
            invitee_user_id,
            role.slug(),
            MemberStatus::Active.slug(),
            inviter_user_id
        ],
    )?;

    conn.query_row(
        "SELECT project_id, user_id, role, status, invited_by, created_at \
         FROM project_members WHERE project_id = ?1 AND user_id = ?2",
        params![project_id, invitee_user_id],
        read_member,
    )
    .map_err(Into::into)
}

/// Removes a member from a project. Only the owner may remove, and the owner
/// row itself cannot be removed.
pub fn remove_member(
    project_id: &str,
    requester_user_id: &str,
    target_user_id: &str,
) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(|e| anyhow::anyhow!("db write: {e}"))?;
    require_owner(&conn, project_id, requester_user_id)?;

    let target_role = conn
        .query_row(
            "SELECT role FROM project_members WHERE project_id = ?1 AND user_id = ?2",
            params![project_id, target_user_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| anyhow::anyhow!("target user is not a member of this project"))?;
    if target_role == ProjectRole::Owner.slug() {
        bail!("project owner cannot be removed");
    }

    conn.execute(
        "DELETE FROM project_members WHERE project_id = ?1 AND user_id = ?2",
        params![project_id, target_user_id],
    )?;
    Ok(())
}

/// Changes a member's role. Only the owner may change roles; the role may only
/// be set to a grantable role (`editor`/`viewer`) and the owner row is immutable.
pub fn set_member_role(
    project_id: &str,
    requester_user_id: &str,
    target_user_id: &str,
    role: &str,
) -> Result<ProjectMember> {
    let role = ProjectRole::from_grantable_slug(role)
        .ok_or_else(|| anyhow::anyhow!("role must be 'editor' or 'viewer'"))?;

    let pool = super::db::pool()?;
    let conn = pool.write().map_err(|e| anyhow::anyhow!("db write: {e}"))?;
    require_owner(&conn, project_id, requester_user_id)?;

    let target_role = conn
        .query_row(
            "SELECT role FROM project_members WHERE project_id = ?1 AND user_id = ?2",
            params![project_id, target_user_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| anyhow::anyhow!("target user is not a member of this project"))?;
    if target_role == ProjectRole::Owner.slug() {
        bail!("project owner role cannot be changed");
    }

    conn.execute(
        "UPDATE project_members SET role = ?3 \
         WHERE project_id = ?1 AND user_id = ?2",
        params![project_id, target_user_id, role.slug()],
    )?;

    conn.query_row(
        "SELECT project_id, user_id, role, status, invited_by, created_at \
         FROM project_members WHERE project_id = ?1 AND user_id = ?2",
        params![project_id, target_user_id],
        read_member,
    )
    .map_err(Into::into)
}

/// Asserts that `user_id` is the owner of `project_id`, returning an error
/// otherwise. The single repository-side authorization gate for owner-only
/// actions (invite/remove/role change).
fn require_owner(
    conn: &rusqlite::Connection,
    project_id: &str,
    user_id: &str,
) -> Result<()> {
    let role: Option<String> = conn
        .query_row(
            "SELECT role FROM project_members WHERE project_id = ?1 AND user_id = ?2",
            params![project_id, user_id],
            |row| row.get(0),
        )
        .optional()?;
    match role.as_deref() {
        Some(r) if r == ProjectRole::Owner.slug() => Ok(()),
        _ => bail!("only the project owner may perform this action"),
    }
}

/// Asserts `user_id` is an active member of `project_id`. Membership is the
/// access boundary for dataset operations, mirroring `get_project`. A non-member
/// cannot create, list or read datasets in a project they cannot see.
fn require_member(
    conn: &rusqlite::Connection,
    project_id: &str,
    user_id: &str,
) -> Result<()> {
    let role: Option<String> = conn
        .query_row(
            "SELECT role FROM project_members \
             WHERE project_id = ?1 AND user_id = ?2 AND status = 'active'",
            params![project_id, user_id],
            |row| row.get(0),
        )
        .optional()?;
    if role.is_none() {
        bail!("not a member of this project");
    }
    Ok(())
}

/// Persists a profiled dataset for a project. `profile_json` is the serialized
/// `profile::TableProfile`. Authorization is by project membership. Returns the
/// stored row.
#[allow(clippy::too_many_arguments)]
pub fn create_dataset(
    user_id: &str,
    project_id: &str,
    name: &str,
    kind: &str,
    row_count: u64,
    column_count: u32,
    profile_json: &str,
    raw_data: &[u8],
) -> Result<Dataset> {
    let name = name.trim();
    if name.is_empty() {
        bail!("dataset name is required");
    }
    if name.chars().count() > 256 {
        bail!("dataset name must be at most 256 characters");
    }

    let dataset_id = uuid::Uuid::new_v4().to_string();
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(|e| anyhow::anyhow!("db write: {e}"))?;
    require_member(&conn, project_id, user_id)?;
    conn.execute(
        "INSERT INTO datasets \
             (dataset_id, project_id, name, kind, row_count, column_count, profile_json, raw_data) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            dataset_id,
            project_id,
            name,
            kind,
            row_count as i64,
            column_count as i64,
            profile_json,
            raw_data
        ],
    )?;
    conn.query_row(
        "SELECT dataset_id, project_id, name, kind, row_count, column_count, profile_json, created_at \
         FROM datasets WHERE dataset_id = ?1",
        params![dataset_id],
        read_dataset,
    )
    .map_err(Into::into)
}

/// Nadpisuje dane datasetu (raw_data + row_count) po zakonczeniu generacji
/// destylacji i oznacza profil jako completed. Autoryzacja: dataset powstal przez
/// `create_dataset` (require_member), wiec update po dataset_id jest bezpieczny.
pub fn update_dataset_data(
    _user_id: &str,
    dataset_id: &str,
    row_count: u64,
    raw_data: &[u8],
) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(|e| anyhow::anyhow!("db write: {e}"))?;
    conn.execute(
        "UPDATE datasets SET row_count = ?1, raw_data = ?2, \
             profile_json = json_set(COALESCE(NULLIF(profile_json, ''), '{}'), '$.distill_status', 'completed') \
         WHERE dataset_id = ?3",
        params![row_count as i64, raw_data, dataset_id],
    )?;
    Ok(())
}

/// Ustawia `distill_status` w profile_json (np. "failed" gdy generacja padnie).
/// Bez tego nieudany dataset zostaje "pending" w bazie i blokada edycji trzyma go
/// na zawsze (stan in-memory znika po restarcie). json_set zachowuje distill_meta.
pub fn set_dataset_distill_status(dataset_id: &str, status: &str) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(|e| anyhow::anyhow!("db write: {e}"))?;
    conn.execute(
        "UPDATE datasets SET \
             profile_json = json_set(COALESCE(NULLIF(profile_json, ''), '{}'), '$.distill_status', ?1) \
         WHERE dataset_id = ?2",
        params![status, dataset_id],
    )?;
    Ok(())
}

/// Lists datasets of a project, newest first. Authorization by membership.
pub fn list_datasets(user_id: &str, project_id: &str) -> Result<Vec<Dataset>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    require_member(&conn, project_id, user_id)?;
    let mut stmt = conn.prepare(
        "SELECT dataset_id, project_id, name, kind, row_count, column_count, profile_json, created_at \
         FROM datasets WHERE project_id = ?1 ORDER BY created_at DESC, name",
    )?;
    let rows = stmt.query_map(params![project_id], read_dataset)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Fetches a single dataset by id, scoped to the caller's project membership.
/// Returns `None` when the dataset does not exist or the user is not a member of
/// its project, so a non-member cannot probe dataset ids.
pub fn get_dataset(user_id: &str, dataset_id: &str) -> Result<Option<Dataset>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let dataset: Option<Dataset> = conn
        .query_row(
            "SELECT dataset_id, project_id, name, kind, row_count, column_count, profile_json, created_at \
             FROM datasets WHERE dataset_id = ?1",
            params![dataset_id],
            read_dataset,
        )
        .optional()?;
    let Some(dataset) = dataset else {
        return Ok(None);
    };
    match require_member(&conn, &dataset.project_id, user_id) {
        Ok(()) => Ok(Some(dataset)),
        Err(_) => Ok(None),
    }
}

/// Returns the raw uploaded bytes of a dataset, scoped to the caller's project
/// membership. Errors when the dataset does not exist, the user is not a member
/// of its project, or no raw data was stored (datasets created before the
/// `raw_data` migration). The bytes feed tabular training.
pub fn get_dataset_raw(user_id: &str, dataset_id: &str) -> Result<Vec<u8>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let row: Option<(String, Option<Vec<u8>>)> = conn
        .query_row(
            "SELECT project_id, raw_data FROM datasets WHERE dataset_id = ?1",
            params![dataset_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((project_id, raw)) = row else {
        bail!("dataset not found");
    };
    require_member(&conn, &project_id, user_id)?;
    match raw {
        Some(bytes) if !bytes.is_empty() => Ok(bytes),
        _ => bail!("dataset has no stored raw data (re-upload to enable training)"),
    }
}

/// Records a finished tabular training run plus its best model in one
/// transaction, returning `(run_id, model_id)`. `config_json` carries the
/// requested target/task; `metrics_json` is the best model's metric blob (the
/// same JSON stored on the model row). The run is marked `succeeded` with a
/// `finished_at` timestamp. `metric_history` is an optional list of
/// `(step, key, value)` rows persisted to `metrics_history` (e.g. per-iteration
/// training loss) so the leaderboard view can chart convergence.
pub fn record_training_result(
    project_id: &str,
    model_name: &str,
    framework: &str,
    config_json: &str,
    metrics_json: &str,
    metric_history: &[(i64, String, f64)],
) -> Result<(String, String)> {
    let run_id = uuid::Uuid::new_v4().to_string();
    let model_id = uuid::Uuid::new_v4().to_string();
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(|e| anyhow::anyhow!("db write: {e}"))?;
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO models \
             (model_id, project_id, name, framework, base_model, metrics_json, status) \
         VALUES (?1, ?2, ?3, ?4, '', ?5, 'trained')",
        params![model_id, project_id, model_name, framework, metrics_json],
    )?;
    tx.execute(
        "INSERT INTO training_runs \
             (run_id, project_id, model_id, status, config_json, started_at, finished_at) \
         VALUES (?1, ?2, ?3, 'succeeded', ?4, datetime('now'), datetime('now'))",
        params![run_id, project_id, model_id, config_json],
    )?;
    for (step, key, value) in metric_history {
        tx.execute(
            "INSERT INTO metrics_history (run_id, step, metric_key, metric_value) \
             VALUES (?1, ?2, ?3, ?4)",
            params![run_id, step, key, value],
        )?;
    }
    tx.commit()?;
    Ok((run_id, model_id))
}

/// Tworzy wiersz `training_runs` w stanie `running` dla asynchronicznego
/// fine-tuningu LLM. W odróżnieniu od `record_training_result` (który zapisuje
/// gotowy wynik jednym transactem) ten run jest „żywy" — task w tle aktualizuje
/// jego status i metryki przez kolejne wywołania. Zwraca wygenerowany `run_id`.
pub fn create_training_run(project_id: &str, config_json: &str) -> Result<String> {
    let run_id = uuid::Uuid::new_v4().to_string();
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(|e| anyhow::anyhow!("db write: {e}"))?;
    conn.execute(
        "INSERT INTO training_runs (run_id, project_id, status, config_json, started_at) \
         VALUES (?1, ?2, 'running', ?3, datetime('now'))",
        params![run_id, project_id, config_json],
    )?;
    Ok(run_id)
}

/// Aktualizuje status runu. Gdy status nie jest już `running` (terminalny:
/// `succeeded`/`failed`), ustawia także `finished_at`, bo run się zakończył.
pub fn update_training_run_status(run_id: &str, status: &str) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(|e| anyhow::anyhow!("db write: {e}"))?;
    if status == "running" {
        conn.execute(
            "UPDATE training_runs SET status = ?2 WHERE run_id = ?1",
            params![run_id, status],
        )?;
    } else {
        conn.execute(
            "UPDATE training_runs SET status = ?2, finished_at = datetime('now') \
             WHERE run_id = ?1",
            params![run_id, status],
        )?;
    }
    Ok(())
}

/// Zapisuje komunikat błędu treningu w `config_json` runu (klucz `$.error`),
/// żeby status handler mógł go zwrócić do UI. Bez osobnej kolumny — błąd to
/// metadana runu, a config_json jest jego workiem na metadane.
pub fn set_training_run_error(run_id: &str, error: &str) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(|e| anyhow::anyhow!("db write: {e}"))?;
    conn.execute(
        "UPDATE training_runs SET config_json = json_set(config_json, '$.error', ?2) \
         WHERE run_id = ?1",
        params![run_id, error],
    )?;
    Ok(())
}

/// Wiąże run z wytrenowanym modelem (po sukcesie treningu).
pub fn set_training_run_model(run_id: &str, model_id: &str) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(|e| anyhow::anyhow!("db write: {e}"))?;
    conn.execute(
        "UPDATE training_runs SET model_id = ?2 WHERE run_id = ?1",
        params![run_id, model_id],
    )?;
    Ok(())
}

/// Dopisuje pojedynczą metrykę treningu (np. `train_loss`/`eval_loss`) dla
/// danego kroku. Wołane na żywo z taska w tle przy każdym statusie z serwisu.
pub fn record_training_metric(run_id: &str, step: i64, key: &str, value: f64) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(|e| anyhow::anyhow!("db write: {e}"))?;
    conn.execute(
        "INSERT INTO metrics_history (run_id, step, metric_key, metric_value) \
         VALUES (?1, ?2, ?3, ?4)",
        params![run_id, step, key, value],
    )?;
    Ok(())
}

/// Wstawia model wytrenowany asynchronicznie (poza transactem
/// `record_training_result`) i zwraca jego `model_id`. Używane po sukcesie
/// fine-tuningu LLM — run jest już utworzony, więc model dopinamy osobno przez
/// `set_training_run_model`.
pub fn insert_model(
    project_id: &str,
    name: &str,
    framework: &str,
    base_model: &str,
    metrics_json: &str,
) -> Result<String> {
    let model_id = uuid::Uuid::new_v4().to_string();
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(|e| anyhow::anyhow!("db write: {e}"))?;
    conn.execute(
        "INSERT INTO models \
             (model_id, project_id, name, framework, base_model, metrics_json, status) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'trained')",
        params![model_id, project_id, name, framework, base_model, metrics_json],
    )?;
    Ok(model_id)
}

/// Jeden wiersz `models` z polami potrzebnymi do eksportu GGUF i autoryzacji.
/// `project_id` służy do `require_project_member`, `base_model` + `metrics_json`
/// (skąd handler wyłuskuje `artifact_path`) zasilają żądanie eksportu.
pub struct ModelRow {
    pub model_id: String,
    pub project_id: String,
    pub name: String,
    pub framework: String,
    pub base_model: String,
    pub metrics_json: String,
    pub status: String,
}

/// Pobiera pojedynczy model po `model_id`. Bez autoryzacji — handler bramkuje
/// dostęp przez `project_id` (członkostwo w projekcie). Zwraca `None` gdy brak.
pub fn get_model(model_id: &str) -> Result<Option<ModelRow>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let row = conn
        .query_row(
            "SELECT model_id, project_id, name, framework, base_model, metrics_json, status \
             FROM models WHERE model_id = ?1",
            params![model_id],
            |row| {
                Ok(ModelRow {
                    model_id: row.get(0)?,
                    project_id: row.get(1)?,
                    name: row.get(2)?,
                    framework: row.get(3)?,
                    base_model: row.get(4)?,
                    metrics_json: row.get(5)?,
                    status: row.get(6)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// Nadpisuje `metrics_json` modelu. Używane przez task eksportu w tle do zapisu
/// stanu (`export_status`/`gguf_path`/...) wmergowanego w istniejący JSON metryk.
pub fn update_model_metrics(model_id: &str, metrics_json: &str) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(|e| anyhow::anyhow!("db write: {e}"))?;
    conn.execute(
        "UPDATE models SET metrics_json = ?1 WHERE model_id = ?2",
        params![metrics_json, model_id],
    )?;
    Ok(())
}

/// Jeden run z `project_id`, do autoryzacji w handlerze statusu (status
/// odpytuje członek projektu, do którego należy run). Zwraca `None` gdy brak.
pub struct TrainingRunRow {
    pub run_id: String,
    pub project_id: String,
    pub model_id: Option<String>,
    pub status: String,
    pub config_json: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
}

/// Jeden AKTYWNY (running/pending) run wraz z nazwą projektu — do panelu jobów.
/// Nazwa projektu pochodzi z JOIN `projects`, widoczność zawężona do projektów,
/// których pytający jest aktywnym członkiem.
pub struct ActiveRunRow {
    pub run_id: String,
    pub project_id: String,
    pub project_name: String,
    pub status: String,
    pub config_json: String,
    pub started_at: Option<String>,
}

/// Lista wszystkich aktywnych jobów (running/pending) widocznych dla `user_id`
/// (członkostwo aktywne w projekcie). Najnowsze wg `started_at` pierwsze.
pub fn list_active_runs_for_user(user_id: &str) -> Result<Vec<ActiveRunRow>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT t.run_id, t.project_id, p.name, t.status, t.config_json, t.started_at \
         FROM training_runs t \
         JOIN projects p ON p.project_id = t.project_id \
         JOIN project_members pm ON pm.project_id = t.project_id \
         WHERE pm.user_id = ?1 AND pm.status = 'active' \
           AND t.status IN ('running', 'pending', 'syncing') \
         ORDER BY t.started_at DESC, t.run_id",
    )?;
    let rows = stmt.query_map(params![user_id], |row| {
        Ok(ActiveRunRow {
            run_id: row.get(0)?,
            project_id: row.get(1)?,
            project_name: row.get(2)?,
            status: row.get(3)?,
            config_json: row.get(4)?,
            started_at: row.get(5)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Pobiera pojedynczy run razem z `project_id` (potrzebnym do autoryzacji).
pub fn get_training_run(run_id: &str) -> Result<Option<TrainingRunRow>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let row = conn
        .query_row(
            "SELECT run_id, project_id, model_id, status, config_json, started_at, finished_at \
             FROM training_runs WHERE run_id = ?1",
            params![run_id],
            |row| {
                Ok(TrainingRunRow {
                    run_id: row.get(0)?,
                    project_id: row.get(1)?,
                    model_id: row.get(2)?,
                    status: row.get(3)?,
                    config_json: row.get(4)?,
                    started_at: row.get(5)?,
                    finished_at: row.get(6)?,
                })
            },
        )
        .optional()?;
    Ok(row)
}

/// Buduje krzywą straty runu z `metrics_history`: pivotuje wiersze
/// `metric_key in ('train_loss','eval_loss')` per `step`, zwracając listę
/// `(step, train_loss, eval_loss)` posortowaną rosnąco po kroku. Brakująca
/// metryka dla danego kroku zostaje `None`.
pub fn loss_curve_for_run(run_id: &str) -> Result<Vec<(i64, Option<f64>, Option<f64>)>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT step, metric_key, metric_value FROM metrics_history \
         WHERE run_id = ?1 AND metric_key IN ('train_loss', 'eval_loss') \
         ORDER BY step ASC, id ASC",
    )?;
    let rows = stmt.query_map(params![run_id], |row| {
        let step: i64 = row.get(0)?;
        let key: String = row.get(1)?;
        let value: f64 = row.get(2)?;
        Ok((step, key, value))
    })?;
    // Pivot: agregujemy per step zachowując kolejność pierwszego wystąpienia
    // (kroki rosną monotonicznie, więc wystarczy lista + indeks ostatniego kroku).
    let mut curve: Vec<(i64, Option<f64>, Option<f64>)> = Vec::new();
    for r in rows {
        let (step, key, value) = r?;
        let entry = match curve.last_mut() {
            Some(last) if last.0 == step => last,
            _ => {
                curve.push((step, None, None));
                curve.last_mut().expect("just pushed")
            }
        };
        match key.as_str() {
            "train_loss" => entry.1 = Some(value),
            "eval_loss" => entry.2 = Some(value),
            _ => {}
        }
    }
    Ok(curve)
}

/// Krzywa treningu detekcji: pivot metryk per epoka (step=epoka) na
/// (epoch, train_loss, map50). Analogiczne do `loss_curve_for_run`, ale dla
/// metryk recognition (`train_loss` + `map50`).
pub fn recog_curve_for_run(run_id: &str) -> Result<Vec<(i64, Option<f64>, Option<f64>)>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT step, metric_key, metric_value FROM metrics_history \
         WHERE run_id = ?1 AND metric_key IN ('train_loss', 'map50') \
         ORDER BY step ASC, id ASC",
    )?;
    let rows = stmt.query_map(params![run_id], |row| {
        let step: i64 = row.get(0)?;
        let key: String = row.get(1)?;
        let value: f64 = row.get(2)?;
        Ok((step, key, value))
    })?;
    let mut curve: Vec<(i64, Option<f64>, Option<f64>)> = Vec::new();
    for r in rows {
        let (step, key, value) = r?;
        let entry = match curve.last_mut() {
            Some(last) if last.0 == step => last,
            _ => {
                curve.push((step, None, None));
                curve.last_mut().expect("just pushed")
            }
        };
        match key.as_str() {
            "train_loss" => entry.1 = Some(value),
            "map50" => entry.2 = Some(value),
            _ => {}
        }
    }
    Ok(curve)
}

/// Generyczna krzywa treningu: wszystkie metryki runu jako
/// `(step, metric_key, metric_value)` posortowane rosnąco po kroku. Używane przez
/// status klasyfikatora atrybutu (metryki train_loss/val_acc/val_macro_f1) i inne
/// tory generyczne, gdzie zestaw metryk nie jest sztywno ustalony.
pub fn generic_curve_for_run(run_id: &str) -> Result<Vec<(i64, String, f64)>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT step, metric_key, metric_value FROM metrics_history \
         WHERE run_id = ?1 ORDER BY step ASC, id ASC",
    )?;
    let rows = stmt.query_map(params![run_id], |row| {
        let step: i64 = row.get(0)?;
        let key: String = row.get(1)?;
        let value: f64 = row.get(2)?;
        Ok((step, key, value))
    })?;
    let mut out: Vec<(i64, String, f64)> = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Returns the number of registered models for a project.
pub fn count_models_per_project(project_id: &str) -> Result<u32> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM models WHERE project_id = ?1",
        params![project_id],
        |row| row.get(0),
    )?;
    Ok(count.max(0) as u32)
}

/// Confirms a grant subject actually exists before a grant is stored, so a typo
/// in `subject_id` can never create an orphaned grant (§11.3). `project`
/// subjects live in the ML Studio database (reuses the held connection); `user`
/// and `group` subjects live in the CORE user directory, reached through
/// `db::global_pool`. Returns a `BadRequest`-style error (anyhow) for unknown
/// subjects and when the core directory is unavailable.
fn validate_grant_subject(
    conn: &rusqlite::Connection,
    subject_kind: &str,
    subject_id: &str,
) -> Result<()> {
    match subject_kind {
        "project" => {
            let exists = conn
                .query_row(
                    "SELECT 1 FROM projects WHERE project_id = ?1",
                    params![subject_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !exists {
                bail!("nie ma takiego projektu");
            }
        }
        "user" => {
            let core = crate::db::global_pool()
                .ok_or_else(|| anyhow::anyhow!("core directory unavailable"))?;
            let core_conn = core
                .read()
                .map_err(|e| anyhow::anyhow!("core db read: {e}"))?;
            let exists = core_conn
                .query_row(
                    "SELECT 1 FROM user_accounts WHERE id = ?1",
                    params![subject_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !exists {
                bail!("nie ma takiego użytkownika");
            }
        }
        "group" => {
            let core = crate::db::global_pool()
                .ok_or_else(|| anyhow::anyhow!("core directory unavailable"))?;
            let core_conn = core
                .read()
                .map_err(|e| anyhow::anyhow!("core db read: {e}"))?;
            let exists = core_conn
                .query_row(
                    "SELECT 1 FROM user_groups WHERE id = ?1",
                    params![subject_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some();
            if !exists {
                bail!("nie ma takiej grupy");
            }
        }
        _ => bail!("subject_kind must be one of user/group/project"),
    }
    Ok(())
}

/// Creates a mesh resource grant (§11.3). Validates `subject_kind` and
/// `resource_kind` against the fixed catalogues and requires a non-empty
/// `subject_id` and `node_id`. `resource_ref` (card id) and `quota` are
/// free-form and may be empty. Returns the stored row.
#[allow(clippy::too_many_arguments)]
pub fn create_grant(
    subject_kind: &str,
    subject_id: &str,
    node_id: &str,
    resource_kind: &str,
    resource_ref: &str,
    quota: &str,
    granted_by: &str,
) -> Result<ResourceGrant> {
    if !GRANT_SUBJECT_KINDS.contains(&subject_kind) {
        bail!("subject_kind must be one of user/group/project");
    }
    if !GRANT_RESOURCE_KINDS.contains(&resource_kind) {
        bail!("resource_kind must be one of gpu/cpu/ram");
    }
    let subject_id = subject_id.trim();
    if subject_id.is_empty() {
        bail!("subject_id is required");
    }
    let node_id = node_id.trim();
    if node_id.is_empty() {
        bail!("node_id is required");
    }

    let grant_id = uuid::Uuid::new_v4().to_string();
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(|e| anyhow::anyhow!("db write: {e}"))?;
    validate_grant_subject(&conn, subject_kind, subject_id)?;
    conn.execute(
        "INSERT INTO resource_grants \
             (grant_id, subject_kind, subject_id, node_id, resource_kind, resource_ref, quota, granted_by) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            grant_id,
            subject_kind,
            subject_id,
            node_id,
            resource_kind,
            resource_ref,
            quota,
            granted_by
        ],
    )?;
    conn.query_row(
        "SELECT grant_id, subject_kind, subject_id, node_id, resource_kind, resource_ref, quota, granted_by, created_at \
         FROM resource_grants WHERE grant_id = ?1",
        params![grant_id],
        read_grant,
    )
    .map_err(Into::into)
}

/// Lists every training run of a project, most recently active first (finished
/// runs by finish time, otherwise by start time). No authorization here; callers
/// gate visibility by project membership.
pub fn list_training_runs(project_id: &str) -> Result<Vec<TrainingRunSummary>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT run_id, model_id, status, config_json, started_at, finished_at \
         FROM training_runs WHERE project_id = ?1 \
         ORDER BY COALESCE(finished_at, started_at) DESC, run_id",
    )?;
    let rows = stmt.query_map(params![project_id], read_training_run)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Lists every model of a project, newest first. No authorization here; callers
/// gate visibility by project membership.
pub fn list_models(project_id: &str) -> Result<Vec<ModelSummary>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT model_id, name, framework, base_model, status, metrics_json, created_at \
         FROM models WHERE project_id = ?1 \
         ORDER BY created_at DESC, model_id",
    )?;
    let rows = stmt.query_map(params![project_id], read_model)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn read_training_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<TrainingRunSummary> {
    Ok(TrainingRunSummary {
        run_id: row.get(0)?,
        model_id: row.get(1)?,
        status: row.get(2)?,
        config_json: row.get(3)?,
        started_at: row.get(4)?,
        finished_at: row.get(5)?,
    })
}

fn read_model(row: &rusqlite::Row<'_>) -> rusqlite::Result<ModelSummary> {
    Ok(ModelSummary {
        model_id: row.get(0)?,
        name: row.get(1)?,
        framework: row.get(2)?,
        base_model: row.get(3)?,
        status: row.get(4)?,
        metrics_json: row.get(5)?,
        created_at: row.get(6)?,
    })
}

/// Lists every resource grant, newest first. Admin-wide view — no subject
/// scoping. Caller gates visibility (admin-only).
pub fn list_grants() -> Result<Vec<ResourceGrant>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT grant_id, subject_kind, subject_id, node_id, resource_kind, resource_ref, quota, granted_by, created_at \
         FROM resource_grants ORDER BY created_at DESC, grant_id",
    )?;
    let rows = stmt.query_map([], read_grant)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Lists grants targeting one specific subject (`kind`/`id`), newest first.
pub fn list_grants_for_subject(kind: &str, id: &str) -> Result<Vec<ResourceGrant>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT grant_id, subject_kind, subject_id, node_id, resource_kind, resource_ref, quota, granted_by, created_at \
         FROM resource_grants WHERE subject_kind = ?1 AND subject_id = ?2 \
         ORDER BY created_at DESC, grant_id",
    )?;
    let rows = stmt.query_map(params![kind, id], read_grant)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Lists grants allocated to one project (`subject_kind = 'project'`).
pub fn list_grants_for_project(project_id: &str) -> Result<Vec<ResourceGrant>> {
    list_grants_for_subject("project", project_id)
}

/// Removes a grant by id. Returns `true` when a row was deleted.
pub fn revoke_grant(grant_id: &str) -> Result<bool> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(|e| anyhow::anyhow!("db write: {e}"))?;
    let affected = conn.execute(
        "DELETE FROM resource_grants WHERE grant_id = ?1",
        params![grant_id],
    )?;
    Ok(affected > 0)
}

fn read_grant(row: &rusqlite::Row<'_>) -> rusqlite::Result<ResourceGrant> {
    Ok(ResourceGrant {
        grant_id: row.get(0)?,
        subject_kind: row.get(1)?,
        subject_id: row.get(2)?,
        node_id: row.get(3)?,
        resource_kind: row.get(4)?,
        resource_ref: row.get(5)?,
        quota: row.get(6)?,
        granted_by: row.get(7)?,
        created_at: row.get(8)?,
    })
}

fn read_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectSummary> {
    let project = Project {
        project_id: row.get(0)?,
        name: row.get(1)?,
        description: row.get(2)?,
        project_type: row.get(3)?,
        status: row.get(4)?,
        owner_user_id: row.get(5)?,
        org_id: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    };
    let model_count = row.get::<_, i64>(9)?.max(0) as u32;
    let dataset_count = row.get::<_, i64>(10)?.max(0) as u32;
    let training_count = row.get::<_, i64>(11)?.max(0) as u32;
    let role: String = row.get(12)?;
    let is_owner = role == ProjectRole::Owner.slug();
    Ok(ProjectSummary {
        project,
        model_count,
        dataset_count,
        training_count,
        role,
        is_owner,
    })
}

fn read_dataset(row: &rusqlite::Row<'_>) -> rusqlite::Result<Dataset> {
    Ok(Dataset {
        dataset_id: row.get(0)?,
        project_id: row.get(1)?,
        name: row.get(2)?,
        kind: row.get(3)?,
        row_count: row.get::<_, i64>(4)?.max(0) as u64,
        column_count: row.get::<_, i64>(5)?.max(0) as u32,
        profile_json: row.get(6)?,
        created_at: row.get(7)?,
    })
}

fn read_member(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectMember> {
    Ok(ProjectMember {
        project_id: row.get(0)?,
        user_id: row.get(1)?,
        role: row.get(2)?,
        status: row.get(3)?,
        invited_by: row.get(4)?,
        created_at: row.get(5)?,
    })
}

/// Returns the project's recognition schema JSON verbatim, or `"{}"` when no
/// schema row exists yet. The JSON is stored opaquely — Core never parses its
/// internal shape. Authorization is the caller's responsibility (handler gate).
pub fn schema_get(project_id: &str) -> Result<String> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let json: Option<String> = conn
        .query_row(
            "SELECT json FROM schemas WHERE project_id = ?1",
            params![project_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(json.unwrap_or_else(|| "{}".to_string()))
}

/// Upserts the project's recognition schema (one row per project). Updates the
/// existing row's `json` + `updated_at` in place, or inserts a fresh
/// `schema_id` when none exists. `schema_json` is stored verbatim.
pub fn schema_upsert(project_id: &str, schema_json: &str) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(|e| anyhow::anyhow!("db write: {e}"))?;
    let updated = conn.execute(
        "UPDATE schemas SET json = ?2, updated_at = datetime('now') WHERE project_id = ?1",
        params![project_id, schema_json],
    )?;
    if updated == 0 {
        let schema_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO schemas (schema_id, project_id, json) VALUES (?1, ?2, ?3)",
            params![schema_id, project_id, schema_json],
        )?;
    }
    Ok(())
}

/// Lists the project's lookup dictionaries as `(dict_id, name, rows_json)`,
/// newest stable order by name then id. `rows_json` is stored opaquely.
pub fn lookup_dicts_list(project_id: &str) -> Result<Vec<(String, String, String)>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let mut stmt = conn.prepare(
        "SELECT dict_id, name, rows_json FROM lookup_dicts \
         WHERE project_id = ?1 ORDER BY name, dict_id",
    )?;
    let rows = stmt.query_map(params![project_id], |row| {
        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Upserts one lookup dictionary. Empty `dict_id` inserts with a fresh uuid
/// (returned); a non-empty id updates that dict's `name` + `rows_json` (scoped to
/// the project) and returns the same id. `rows_json` is stored opaquely.
pub fn lookup_dict_upsert(
    project_id: &str,
    dict_id: &str,
    name: &str,
    rows_json: &str,
) -> Result<String> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(|e| anyhow::anyhow!("db write: {e}"))?;
    if dict_id.is_empty() {
        let new_id = uuid::Uuid::new_v4().to_string();
        conn.execute(
            "INSERT INTO lookup_dicts (dict_id, project_id, name, rows_json) \
             VALUES (?1, ?2, ?3, ?4)",
            params![new_id, project_id, name, rows_json],
        )?;
        Ok(new_id)
    } else {
        let updated = conn.execute(
            "UPDATE lookup_dicts SET name = ?3, rows_json = ?4 \
             WHERE dict_id = ?1 AND project_id = ?2",
            params![dict_id, project_id, name, rows_json],
        )?;
        if updated == 0 {
            bail!("lookup dict not found in project");
        }
        Ok(dict_id.to_string())
    }
}

/// Deletes a lookup dictionary by id. Returns the project id it belonged to so
/// the handler can authorize the delete against project membership; errors when
/// no such dict exists.
pub fn lookup_dict_delete(dict_id: &str) -> Result<()> {
    let pool = super::db::pool()?;
    let conn = pool.write().map_err(|e| anyhow::anyhow!("db write: {e}"))?;
    let deleted = conn.execute(
        "DELETE FROM lookup_dicts WHERE dict_id = ?1",
        params![dict_id],
    )?;
    if deleted == 0 {
        bail!("lookup dict not found");
    }
    Ok(())
}

/// Resolves the project id that owns a lookup dictionary, or `None` when the
/// dict does not exist. The delete handler uses this to gate on project access
/// before removing the row.
pub fn lookup_dict_project(dict_id: &str) -> Result<Option<String>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    conn.query_row(
        "SELECT project_id FROM lookup_dicts WHERE dict_id = ?1",
        params![dict_id],
        |row| row.get::<_, String>(0),
    )
    .optional()
    .map_err(Into::into)
}

/// One model a schema field can bind to: id, display name, capability and source.
/// `capability` is the primary capability (detector/ocr/classifier/...), `source`
/// distinguishes stored `service_models` rows from built-in in-core models.
pub struct ServiceModelEntry {
    pub id: String,
    pub name: String,
    pub capability: String,
    pub source: String,
}

/// Lists the stored `service_models` rows, one entry per declared capability
/// (a model advertising several capabilities yields several entries so the
/// `capability` filter in the handler can match any of them). `capabilities_json`
/// is parsed as either a JSON array of strings or a JSON object whose keys are
/// capabilities; an unparseable/empty blob yields a single entry with an empty
/// capability.
pub fn service_models_list() -> Result<Vec<ServiceModelEntry>> {
    let pool = super::db::pool()?;
    let conn = pool.read().map_err(|e| anyhow::anyhow!("db read: {e}"))?;
    let mut stmt =
        conn.prepare("SELECT id, name, capabilities_json, source FROM service_models")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (id, name, caps_json, source) = row?;
        let caps = parse_capabilities(&caps_json);
        if caps.is_empty() {
            out.push(ServiceModelEntry {
                id: id.clone(),
                name: name.clone(),
                capability: String::new(),
                source: source.clone(),
            });
        } else {
            for cap in caps {
                out.push(ServiceModelEntry {
                    id: id.clone(),
                    name: name.clone(),
                    capability: cap,
                    source: source.clone(),
                });
            }
        }
    }
    Ok(out)
}

/// Extracts capability names from a `capabilities_json` blob, accepting either a
/// JSON array of strings (`["detector","ocr"]`) or a JSON object keyed by
/// capability (`{"detector":...}`). Returns an empty vec for anything else.
fn parse_capabilities(caps_json: &str) -> Vec<String> {
    match serde_json::from_str::<serde_json::Value>(caps_json) {
        Ok(serde_json::Value::Array(items)) => items
            .into_iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect(),
        Ok(serde_json::Value::Object(map)) => map.into_iter().map(|(k, _)| k).collect(),
        _ => Vec::new(),
    }
}
