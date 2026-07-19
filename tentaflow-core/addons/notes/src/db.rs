// =============================================================================
// File: addons/notes/src/db.rs
// Purpose: data access for the Notes addon — note CRUD, tags, shares and the
//          ACL predicate every read/write path goes through. Pure SQL-building
//          helpers are kept side-effect free so they are unit-testable on the
//          native target (host fns exist only under wasm).
// =============================================================================

use tentaflow_addon_sdk::{
    directory_groups, directory_org, directory_users, log, sql_exec, sql_query, sql_query_one,
    sql_transaction, SqlValue,
};

// =============================================================================
// Identity
// =============================================================================

/// Acting user for one call: id + display data + group ids for ACL matching.
#[derive(Debug, Clone)]
pub struct UserCtx {
    pub user_id: String,
    pub display_name: String,
    pub group_ids: Vec<String>,
}

/// Resolves the acting user's display name and group ids from the directory.
/// A user absent from the directory (e.g. deactivated mid-session) still gets
/// a valid ctx with no groups — their own notes must stay reachable.
pub fn resolve_user_ctx(user_id: &str) -> Result<UserCtx, String> {
    if user_id.is_empty() || user_id == "system" {
        return Err("Brak tożsamości użytkownika dla tej akcji.".to_string());
    }
    let users = directory_users().map_err(|e| format!("Błąd katalogu użytkowników: {e}"))?;
    let me = users.iter().find(|u| u.id == user_id);
    Ok(UserCtx {
        user_id: user_id.to_string(),
        display_name: me
            .map(|u| {
                if u.display_name.is_empty() {
                    u.username.clone()
                } else {
                    u.display_name.clone()
                }
            })
            .unwrap_or_else(|| user_id.to_string()),
        group_ids: me.map(|u| u.groups.clone()).unwrap_or_default(),
    })
}

/// Display name of an arbitrary user (note authors on cards / in the editor).
pub fn user_display_name(user_id: &str) -> String {
    directory_users()
        .ok()
        .and_then(|users| {
            users.into_iter().find(|u| u.id == user_id).map(|u| {
                if u.display_name.is_empty() {
                    u.username
                } else {
                    u.display_name
                }
            })
        })
        .unwrap_or_else(|| user_id.to_string())
}

/// Organization id for new notes (single-org instance directory).
pub fn org_id() -> Result<String, String> {
    directory_org()
        .map(|o| o.org_id)
        .map_err(|e| format!("Błąd odczytu organizacji: {e}"))
}

// =============================================================================
// Pure ACL / SQL helpers (unit-tested on the native target)
// =============================================================================

/// Read-access predicate over alias `n` (notes): own note OR a share reaching
/// the user directly, via one of their groups, or org-wide. `group_count`
/// controls the IN placeholder list; 0 groups drops the group branch entirely.
pub fn acl_read_clause(group_count: usize) -> String {
    let mut share =
        String::from("(s.subject_type = 'user' AND s.subject_id = ?) OR (s.subject_type = 'org')");
    if group_count > 0 {
        share.push_str(&format!(
            " OR (s.subject_type = 'group' AND s.subject_id IN ({}))",
            placeholders(group_count)
        ));
    }
    format!(
        "(n.owner_user_id = ? OR EXISTS (SELECT 1 FROM note_shares s \
         WHERE s.note_id = n.id AND ({share})))"
    )
}

/// Write-access predicate over alias `n`: owner always, otherwise a share with
/// access='write' matching the user / their groups / the org.
pub fn acl_write_clause(group_count: usize) -> String {
    let mut share =
        String::from("(s.subject_type = 'user' AND s.subject_id = ?) OR (s.subject_type = 'org')");
    if group_count > 0 {
        share.push_str(&format!(
            " OR (s.subject_type = 'group' AND s.subject_id IN ({}))",
            placeholders(group_count)
        ));
    }
    format!(
        "(n.owner_user_id = ? OR EXISTS (SELECT 1 FROM note_shares s \
         WHERE s.note_id = n.id AND s.access = 'write' AND ({share})))"
    )
}

/// Bind params matching `acl_read_clause`/`acl_write_clause` placeholder order:
/// owner check first, then the share subject checks.
pub fn acl_params(ctx: &UserCtx) -> Vec<SqlValue> {
    let mut params = vec![
        SqlValue::String(ctx.user_id.clone()),
        SqlValue::String(ctx.user_id.clone()),
    ];
    params.extend(ctx.group_ids.iter().map(|g| SqlValue::String(g.clone())));
    params
}

/// Write-ACL guard usable in the WHERE clause of statements mutating tables
/// OTHER than notes (note_tags / note_shares): re-checks liveness and write
/// access at execution time, so a check-then-mutate race cannot touch a note
/// the user just lost access to. Placeholders: note id first, then
/// `acl_params`.
pub fn write_guard_clause(group_count: usize) -> String {
    format!(
        "EXISTS (SELECT 1 FROM notes n WHERE n.id = ? AND n.deleted_at IS NULL AND {})",
        acl_write_clause(group_count)
    )
}

/// Bind params matching `write_guard_clause` placeholder order.
pub fn write_guard_params(ctx: &UserCtx, note_id: &str) -> Vec<SqlValue> {
    let mut params = vec![SqlValue::String(note_id.to_string())];
    params.extend(acl_params(ctx));
    params
}

/// Owner-only guard for share mutations: the note must be alive and owned by
/// the acting user. Placeholders: note id, then owner id.
pub fn owner_guard_clause() -> &'static str {
    "EXISTS (SELECT 1 FROM notes n WHERE n.id = ? AND n.deleted_at IS NULL \
     AND n.owner_user_id = ?)"
}

/// Scope filter chips → extra predicate over alias `n` (appended AFTER the ACL
/// clause, so every scope is still bounded by accessibility). Returns the SQL
/// and its bind params.
pub fn scope_clause(scope: &str, ctx: &UserCtx) -> (String, Vec<SqlValue>) {
    match scope {
        "mine" => (
            " AND n.owner_user_id = ?".to_string(),
            vec![SqlValue::String(ctx.user_id.clone())],
        ),
        "shared" => (
            " AND n.owner_user_id != ?".to_string(),
            vec![SqlValue::String(ctx.user_id.clone())],
        ),
        "group" => {
            if ctx.group_ids.is_empty() {
                // No groups — the group scope matches nothing, not everything.
                (" AND 0".to_string(), vec![])
            } else {
                (
                    format!(
                        " AND EXISTS (SELECT 1 FROM note_shares g WHERE g.note_id = n.id \
                         AND g.subject_type = 'group' AND g.subject_id IN ({}))",
                        placeholders(ctx.group_ids.len())
                    ),
                    ctx.group_ids
                        .iter()
                        .map(|g| SqlValue::String(g.clone()))
                        .collect(),
                )
            }
        }
        "org" => (
            " AND EXISTS (SELECT 1 FROM note_shares o WHERE o.note_id = n.id \
             AND o.subject_type = 'org')"
                .to_string(),
            vec![],
        ),
        _ => (String::new(), vec![]),
    }
}

/// Escapes LIKE metacharacters for a `LIKE ? ESCAPE '\'` pattern.
pub fn escape_like(term: &str) -> String {
    let mut out = String::with_capacity(term.len() + 4);
    for c in term.chars() {
        if c == '\\' || c == '%' || c == '_' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

fn placeholders(count: usize) -> String {
    let mut s = String::with_capacity(count * 3);
    for i in 0..count {
        if i > 0 {
            s.push_str(", ");
        }
        s.push('?');
    }
    s
}

/// First `max_chars` characters of the content as a card preview, cut on a
/// char boundary (never mid-UTF-8), whitespace collapsed to single spaces.
pub fn note_preview(content: &str, max_chars: usize) -> String {
    let collapsed: String = content.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_chars {
        return collapsed;
    }
    let cut: String = collapsed.chars().take(max_chars).collect();
    format!("{cut}…")
}

/// Toolbar counter label: "1 248 znaków · 3 akapity" (Polish plural rules,
/// thousands separated by a thin space like the mockup).
pub fn counter_label(content: &str) -> String {
    let chars = content.chars().count();
    let paragraphs = content
        .split("\n\n")
        .filter(|p| !p.trim().is_empty())
        .count();
    format!(
        "{} {} · {} {}",
        group_thousands(chars),
        plural_pl(chars, "znak", "znaki", "znaków"),
        paragraphs,
        plural_pl(paragraphs, "akapit", "akapity", "akapitów")
    )
}

/// Polish plural form: 1 → one, 2-4 (except 12-14) → few, otherwise many.
pub fn plural_pl(
    n: usize,
    one: &'static str,
    few: &'static str,
    many: &'static str,
) -> &'static str {
    if n == 1 {
        return one;
    }
    match (n % 100, n % 10) {
        (12..=14, _) => many,
        (_, 2..=4) => few,
        _ => many,
    }
}

fn group_thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('\u{202f}');
        }
        out.push(c);
    }
    out
}

// =============================================================================
// Notes: read side
// =============================================================================

/// One card on the list.
#[derive(Debug, Clone)]
pub struct NoteSummary {
    pub id: String,
    pub title: String,
    pub preview: String,
    pub updated_at: i64,
    /// "private" | "user" | "group" | "org" — widest share on the note.
    pub scope: String,
    /// True when the reader owns the note (drives the "Moje" scope badge).
    pub is_owner: bool,
    /// Display name of the group the note is shared with, when group-scoped and
    /// resolvable through the directory (drives "Grupa · {name}").
    pub group_name: Option<String>,
    /// Top detected entities as (name, entity_type), capped for card chips.
    pub entities: Vec<(String, String)>,
}

/// Full note for the editor.
#[derive(Debug, Clone)]
pub struct NoteDetail {
    pub id: String,
    pub title: String,
    pub content: String,
    pub owner_user_id: String,
    pub created_at: i64,
    pub tags: Vec<String>,
    pub scope: String,
    /// Group display name when group-scoped and resolvable (scope badge).
    pub group_name: Option<String>,
    /// Share rows of the note (avatar group on the share button; the modal
    /// re-reads them fresh on open).
    pub shares: Vec<ShareEntry>,
    pub can_write: bool,
    pub is_owner: bool,
    /// "typed" | "dictated" — drives the „dyktowana" chip in the editor meta.
    pub origin: String,
}

/// Lists accessible notes for the user, newest first, with scope filter and
/// case-insensitive substring search over title/content.
pub fn list_notes(ctx: &UserCtx, scope: &str, search: &str) -> Result<Vec<NoteSummary>, String> {
    let acl = acl_read_clause(ctx.group_ids.len());
    let (scope_sql, scope_params) = scope_clause(scope, ctx);
    let mut sql = format!(
        "SELECT n.id, n.title, substr(n.content, 1, 400), n.updated_at, \
         (SELECT CASE \
            WHEN EXISTS (SELECT 1 FROM note_shares x WHERE x.note_id = n.id AND x.subject_type = 'org') THEN 'org' \
            WHEN EXISTS (SELECT 1 FROM note_shares x WHERE x.note_id = n.id AND x.subject_type = 'group') THEN 'group' \
            WHEN EXISTS (SELECT 1 FROM note_shares x WHERE x.note_id = n.id AND x.subject_type = 'user') THEN 'user' \
            ELSE 'private' END), \
         n.owner_user_id, \
         (SELECT g.subject_id FROM note_shares g WHERE g.note_id = n.id AND g.subject_type = 'group' LIMIT 1) \
         FROM notes n WHERE n.deleted_at IS NULL AND {acl}{scope_sql}"
    );
    let mut params = acl_params(ctx);
    params.extend(scope_params);

    let term = search.trim();
    if !term.is_empty() {
        sql.push_str(" AND (n.title LIKE ? ESCAPE '\\' OR n.content LIKE ? ESCAPE '\\')");
        let pattern = format!("%{}%", escape_like(term));
        params.push(SqlValue::String(pattern.clone()));
        params.push(SqlValue::String(pattern));
    }
    sql.push_str(" ORDER BY n.updated_at DESC LIMIT 200");

    let rows = sql_query(&sql, &params).map_err(|e| format!("Błąd odczytu notatek: {e}"))?;
    let mut notes: Vec<NoteSummary> = rows
        .iter()
        .map(|row| NoteSummary {
            id: row
                .first()
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            title: row
                .get(1)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            preview: note_preview(row.get(2).and_then(|v| v.as_str()).unwrap_or(""), 120),
            updated_at: row.get(3).and_then(|v| v.as_i64()).unwrap_or(0),
            scope: row
                .get(4)
                .and_then(|v| v.as_str())
                .unwrap_or("private")
                .to_string(),
            is_owner: row.get(5).and_then(|v| v.as_str()) == Some(ctx.user_id.as_str()),
            group_name: row
                .get(6)
                .and_then(|v| v.as_str())
                .map(str::to_string),
            entities: Vec::new(),
        })
        .collect();
    resolve_group_names(notes.iter_mut().map(|n| (&n.scope, &mut n.group_name)));
    attach_card_entities(ctx, &mut notes);
    Ok(notes)
}

/// Directory group id → display name. One host call; empty on failure (badge
/// degrades to a bare "Grupa"). Used to turn the group subject id carried on a
/// note into the readable name shown on the scope badge.
pub fn group_name_map() -> std::collections::HashMap<String, String> {
    directory_groups()
        .unwrap_or_default()
        .into_iter()
        .map(|g| (g.id, g.name))
        .collect()
}

/// Replaces each `group_name` slot (currently holding the group SUBJECT ID) with
/// the resolved group display name, but only for group-scoped notes. Non-group
/// notes have their slot cleared so a stray group share never labels them. Skips
/// the directory call entirely when no note is group-scoped.
fn resolve_group_names<'a>(
    notes: impl Iterator<Item = (&'a String, &'a mut Option<String>)>,
) {
    let items: Vec<(&String, &mut Option<String>)> = notes.collect();
    if !items.iter().any(|(scope, _)| scope.as_str() == "group") {
        for (_, slot) in items {
            *slot = None;
        }
        return;
    }
    let map = group_name_map();
    for (scope, slot) in items {
        if scope.as_str() == "group" {
            *slot = slot.as_ref().and_then(|id| map.get(id).cloned());
        } else {
            *slot = None;
        }
    }
}

/// Entities shown on a list card (mockup n01 micro-chips with colored dots).
const CARD_ENTITY_LIMIT: usize = 3;

/// SQL of the batched card-entities read: one query for ALL listed notes.
/// Local and canonical names come back separately together with a per-row
/// canonical-visibility flag — the same direct-mention rule as
/// `entity_visibility_sql` (correlated on `c.id` instead of a placeholder), so
/// a merge never leaks a canonical name that originated in a note the reader
/// cannot access. Pure — unit-tested natively.
/// Placeholder ORDER follows the SQL text: the visibility `EXISTS` lives in the
/// SELECT list, so its `acl_params` come FIRST, then the `WHERE ne.note_id IN`
/// ids. Callers MUST bind `acl_params` before the note ids (see
/// `card_entities_params`).
pub fn card_entities_sql(note_count: usize, group_count: usize) -> String {
    let visibility =
        entity_visibility_sql(group_count).replace("ne.entity_id = ?", "ne.entity_id = c.id");
    format!(
        "SELECT ne.note_id, COALESCE(c.id, e.id), e.name, c.name, \
                COALESCE(c.entity_type, e.entity_type), \
                CASE WHEN c.id IS NOT NULL AND EXISTS ({visibility}) \
                     THEN 1 ELSE 0 END \
         FROM note_entities ne \
         JOIN entities e ON e.id = ne.entity_id \
         LEFT JOIN entities c ON c.id = e.canonical_id \
         WHERE ne.note_id IN ({}) \
         ORDER BY ne.note_id, ne.count DESC",
        placeholders(note_count)
    )
}

/// Bind params for `card_entities_sql` in SQL-text order: `acl_params` for the
/// SELECT-clause visibility `EXISTS` FIRST, then the note ids of the `IN`
/// clause. Binding the ids first (the intuitive but wrong order) makes SQLite
/// feed note ids into the ACL owner/user slots and shift the `IN` list, so only
/// one note ever matches — that was the empty search right-rail bug.
pub fn card_entities_params(ctx: &UserCtx, note_ids: &[String]) -> Vec<SqlValue> {
    let mut params = acl_params(ctx);
    params.extend(note_ids.iter().map(|id| SqlValue::String(id.clone())));
    params
}

/// Name shown on a card chip: the canonical (merged) name only when the
/// reader can see the canonical entity through some readable note of their
/// own; otherwise the alias name from the listed note. Pure — unit-tested.
pub(crate) fn pick_card_entity_name(
    local: &str,
    canonical: Option<&str>,
    canonical_visible: bool,
) -> String {
    match canonical {
        Some(c) if canonical_visible => c.to_string(),
        _ => local.to_string(),
    }
}

/// One batch query filling `NoteSummary.entities` for all listed notes (the
/// notes themselves already passed the read ACL in `list_notes`). Canonical
/// names are disclosed per the visibility rule of `note_entities`.
fn attach_card_entities(ctx: &UserCtx, notes: &mut [NoteSummary]) {
    if notes.is_empty() {
        return;
    }
    let sql = card_entities_sql(notes.len(), ctx.group_ids.len());
    let note_ids: Vec<String> = notes.iter().map(|n| n.id.clone()).collect();
    let params = card_entities_params(ctx, &note_ids);
    let rows = match sql_query(&sql, &params) {
        Ok(r) => r,
        // Card chips are decoration — a failed lookup must not break the list.
        Err(_) => return,
    };
    for note in notes.iter_mut() {
        // Merged mentions can resolve to the same canonical — dedup per card.
        let mut seen_ids: Vec<String> = Vec::new();
        for r in &rows {
            if r.first().and_then(|v| v.as_str()) != Some(note.id.as_str()) {
                continue;
            }
            let entity_id = r.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let local = r.get(2).and_then(|v| v.as_str()).unwrap_or("");
            let canonical = r.get(3).and_then(|v| v.as_str());
            let etype = r
                .get(4)
                .and_then(|v| v.as_str())
                .unwrap_or("other")
                .to_string();
            let visible = r.get(5).and_then(|v| v.as_i64()).unwrap_or(0) == 1;
            let name = pick_card_entity_name(local, canonical, visible);
            if name.is_empty()
                || entity_id.is_empty()
                || seen_ids.iter().any(|id| id == &entity_id)
                || note.entities.iter().any(|(n, _)| n == &name)
            {
                continue;
            }
            seen_ids.push(entity_id);
            note.entities.push((name, etype));
            if note.entities.len() >= CARD_ENTITY_LIMIT {
                break;
            }
        }
    }
}

/// Loads one note through the read ACL. Ok(None) = absent or not accessible.
pub fn get_note(ctx: &UserCtx, note_id: &str) -> Result<Option<NoteDetail>, String> {
    let acl = acl_read_clause(ctx.group_ids.len());
    let sql = format!(
        "SELECT n.id, n.title, n.content, n.owner_user_id, n.created_at, n.origin \
         FROM notes n WHERE n.id = ? AND n.deleted_at IS NULL AND {acl}"
    );
    let mut params = vec![SqlValue::String(note_id.to_string())];
    params.extend(acl_params(ctx));

    let row =
        match sql_query_one(&sql, &params).map_err(|e| format!("Błąd odczytu notatki: {e}"))? {
            Some(r) => r,
            None => return Ok(None),
        };

    let id = row
        .first()
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let owner = row
        .get(3)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let tags = sql_query(
        "SELECT tag FROM note_tags WHERE note_id = ? ORDER BY tag",
        &[SqlValue::String(id.clone())],
    )
    .map_err(|e| format!("Błąd odczytu tagów: {e}"))?
    .iter()
    .filter_map(|r| r.first().and_then(|v| v.as_str()).map(str::to_string))
    .collect();

    let shares = sql_query(
        "SELECT subject_type, subject_id, access FROM note_shares WHERE note_id = ?",
        &[SqlValue::String(id.clone())],
    )
    .map_err(|e| format!("Błąd odczytu udostępnień: {e}"))?;
    let share_rows: Vec<(String, String, String)> = shares
        .iter()
        .map(|r| {
            (
                r.first().and_then(|v| v.as_str()).unwrap_or("").to_string(),
                r.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string(),
                r.get(2)
                    .and_then(|v| v.as_str())
                    .unwrap_or("read")
                    .to_string(),
            )
        })
        .collect();

    let is_owner = owner == ctx.user_id;
    let can_write = is_owner
        || share_rows.iter().any(|(t, s, a)| {
            a == "write"
                && match t.as_str() {
                    "user" => s == &ctx.user_id,
                    "group" => ctx.group_ids.contains(s),
                    "org" => true,
                    _ => false,
                }
        });

    let scope = widest_scope(&share_rows);
    // Resolve the group name only when the badge will actually show a group.
    let group_name = if scope == "group" {
        share_rows
            .iter()
            .find(|(t, _, _)| t == "group")
            .and_then(|(_, gid, _)| group_name_map().get(gid).cloned())
    } else {
        None
    };

    Ok(Some(NoteDetail {
        title: row
            .get(1)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        content: row
            .get(2)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        created_at: row.get(4).and_then(|v| v.as_i64()).unwrap_or(0),
        group_name,
        scope,
        shares: share_rows
            .iter()
            .map(|(t, s, a)| ShareEntry {
                subject_type: t.clone(),
                subject_id: s.clone(),
                access: a.clone(),
            })
            .collect(),
        owner_user_id: owner,
        can_write,
        is_owner,
        tags,
        origin: row
            .get(5)
            .and_then(|v| v.as_str())
            .unwrap_or("typed")
            .to_string(),
        id,
    }))
}

/// Widest share on a note → scope badge ("org" > "group" > "user" > "private").
pub fn widest_scope(shares: &[(String, String, String)]) -> String {
    if shares.iter().any(|(t, _, _)| t == "org") {
        "org".to_string()
    } else if shares.iter().any(|(t, _, _)| t == "group") {
        "group".to_string()
    } else if shares.iter().any(|(t, _, _)| t == "user") {
        "user".to_string()
    } else {
        "private".to_string()
    }
}

// =============================================================================
// Notes: write side
// =============================================================================

/// Creates an empty note owned by the user. Returns the new id.
pub fn create_note(ctx: &UserCtx) -> Result<String, String> {
    let id = new_id("note");
    let now = now_unix();
    let org = org_id()?;
    sql_exec(
        "INSERT INTO notes (id, org_id, owner_user_id, title, content, content_format, \
         origin, created_at, updated_at) VALUES (?, ?, ?, '', '', 'markdown', 'typed', ?, ?)",
        &[
            SqlValue::String(id.clone()),
            SqlValue::String(org),
            SqlValue::String(ctx.user_id.clone()),
            SqlValue::I64(now),
            SqlValue::I64(now),
        ],
    )
    .map_err(|e| format!("Błąd utworzenia notatki: {e}"))?;
    Ok(id)
}

/// Updates title or content; requires write access. Returns the fresh content
/// (for the char counter) or an error when the note is not writable.
pub fn update_note_field(
    ctx: &UserCtx,
    note_id: &str,
    field: &str,
    value: &str,
) -> Result<(), String> {
    let column = match field {
        "title" => "title",
        "content" => "content",
        _ => return Err(format!("Nieznane pole notatki: {field}")),
    };
    let acl = acl_write_clause(ctx.group_ids.len());
    let sql = format!(
        "UPDATE notes AS n SET {column} = ?, updated_at = ? \
         WHERE n.id = ? AND n.deleted_at IS NULL AND {acl}"
    );
    let mut params = vec![
        SqlValue::String(value.to_string()),
        SqlValue::I64(now_unix()),
        SqlValue::String(note_id.to_string()),
    ];
    params.extend(acl_params(ctx));
    let res = sql_exec(&sql, &params).map_err(|e| format!("Błąd zapisu notatki: {e}"))?;
    if res.rows_affected == 0 {
        return Err("Brak uprawnień do edycji tej notatki.".to_string());
    }
    Ok(())
}

/// Commits one dictation write: replaces the content AND flags the note as
/// dictated in the same guarded UPDATE, so the origin flip can never land on a
/// note the user lost write access to mid-dictation.
pub fn commit_dictated_content(ctx: &UserCtx, note_id: &str, content: &str) -> Result<(), String> {
    let acl = acl_write_clause(ctx.group_ids.len());
    let sql = format!(
        "UPDATE notes AS n SET content = ?, origin = 'dictated', updated_at = ? \
         WHERE n.id = ? AND n.deleted_at IS NULL AND {acl}"
    );
    let mut params = vec![
        SqlValue::String(content.to_string()),
        SqlValue::I64(now_unix()),
        SqlValue::String(note_id.to_string()),
    ];
    params.extend(acl_params(ctx));
    let res = sql_exec(&sql, &params).map_err(|e| format!("Błąd zapisu dyktowania: {e}"))?;
    if res.rows_affected == 0 {
        return Err("Brak uprawnień do edycji tej notatki.".to_string());
    }
    Ok(())
}

/// Replaces the tag set of a note; requires write access. One transaction:
/// delete + inserts + updated_at touch, every statement re-checking the write
/// ACL and liveness in its own WHERE (not only the check up front).
pub fn set_tags(ctx: &UserCtx, note_id: &str, tags: &[String]) -> Result<(), String> {
    // Friendly error for the common no-access case; the guards below make the
    // mutation itself race-safe regardless.
    ensure_writable(ctx, note_id)?;

    let guard = write_guard_clause(ctx.group_ids.len());
    let guard_params = write_guard_params(ctx, note_id);
    let acl = acl_write_clause(ctx.group_ids.len());

    let mut stmts: Vec<(String, Vec<SqlValue>)> = Vec::with_capacity(tags.len() + 2);

    let mut delete_params = vec![SqlValue::String(note_id.to_string())];
    delete_params.extend(guard_params.clone());
    stmts.push((
        format!("DELETE FROM note_tags WHERE note_id = ? AND {guard}"),
        delete_params,
    ));

    for tag in tags {
        let t = tag.trim();
        if t.is_empty() {
            continue;
        }
        let mut params = vec![
            SqlValue::String(note_id.to_string()),
            SqlValue::String(t.to_string()),
        ];
        params.extend(guard_params.clone());
        stmts.push((
            format!("INSERT OR IGNORE INTO note_tags (note_id, tag) SELECT ?, ? WHERE {guard}"),
            params,
        ));
    }

    let mut touch_params = vec![
        SqlValue::I64(now_unix()),
        SqlValue::String(note_id.to_string()),
    ];
    touch_params.extend(acl_params(ctx));
    stmts.push((
        format!(
            "UPDATE notes AS n SET updated_at = ? \
             WHERE n.id = ? AND n.deleted_at IS NULL AND {acl}"
        ),
        touch_params,
    ));

    run_transaction(&stmts).map_err(|e| format!("Błąd zapisu tagów: {e}"))
}

/// Soft delete; requires write access.
pub fn delete_note(ctx: &UserCtx, note_id: &str) -> Result<(), String> {
    let acl = acl_write_clause(ctx.group_ids.len());
    let sql = format!(
        "UPDATE notes AS n SET deleted_at = ? \
         WHERE n.id = ? AND n.deleted_at IS NULL AND {acl}"
    );
    let mut params = vec![
        SqlValue::I64(now_unix()),
        SqlValue::String(note_id.to_string()),
    ];
    params.extend(acl_params(ctx));
    let res = sql_exec(&sql, &params).map_err(|e| format!("Błąd usuwania notatki: {e}"))?;
    if res.rows_affected == 0 {
        return Err("Brak uprawnień do usunięcia tej notatki.".to_string());
    }
    Ok(())
}

fn ensure_writable(ctx: &UserCtx, note_id: &str) -> Result<(), String> {
    let acl = acl_write_clause(ctx.group_ids.len());
    let sql = format!("SELECT n.id FROM notes n WHERE n.id = ? AND n.deleted_at IS NULL AND {acl}");
    let mut params = vec![SqlValue::String(note_id.to_string())];
    params.extend(acl_params(ctx));
    match sql_query_one(&sql, &params) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err("Brak uprawnień do edycji tej notatki.".to_string()),
        Err(e) => Err(format!("Błąd sprawdzania uprawnień: {e}")),
    }
}

/// Runs owned statements atomically via the sql_transaction host fn.
fn run_transaction(stmts: &[(String, Vec<SqlValue>)]) -> Result<(), String> {
    let refs: Vec<(&str, &[SqlValue])> = stmts
        .iter()
        .map(|(sql, params)| (sql.as_str(), params.as_slice()))
        .collect();
    sql_transaction(&refs)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// =============================================================================
// Links / entities read side (populated by the analysis pipeline). Every
// query joins notes + acl_read_clause on the TARGET note, so a reader never
// sees a link into a note they cannot open.
// =============================================================================

/// One related-note card in the links panel. `reason` is the DISPLAY string,
/// resolved at read time from the persisted machine token — entity names are
/// never stored in note_links (a canonical name from a private note must not
/// leak to readers of the linked pair).
#[derive(Debug, Clone)]
pub struct RelatedNote {
    pub id: String,
    pub title: String,
    /// Ranking weight in [0,1]; cosine similarity for kind='similar'.
    pub weight: f64,
    pub reason: String,
    pub created_at: i64,
}

fn value_f64(v: &SqlValue) -> Option<f64> {
    match v {
        SqlValue::F64(f) => Some(*f),
        SqlValue::I64(i) => Some(*i as f64),
        _ => None,
    }
}

// =============================================================================
// Link reason tokens — note_links.reason persists ONLY machine identifiers;
// the human label (and any entity name) is resolved per reader at read time.
// =============================================================================

/// Parsed machine token from note_links.reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkReason {
    /// "similar" — semantic similarity, no name involved.
    Similar,
    /// "manual" — user-created link.
    Manual,
    /// "entity:{canonical_id}:{shared_count}" — shared-entity link.
    Entity { entity_id: String, shared: usize },
}

/// Builds the persisted token of a shared-entity link.
pub fn entity_reason_token(entity_id: &str, shared: usize) -> String {
    format!("entity:{entity_id}:{shared}")
}

/// Parses a persisted reason token. None = unrecognized (never produced by
/// current write paths; surfaces as a label-less card instead of leaking raw
/// stored text).
pub fn parse_link_reason(raw: &str) -> Option<LinkReason> {
    match raw {
        "similar" => Some(LinkReason::Similar),
        "manual" => Some(LinkReason::Manual),
        _ => {
            let rest = raw.strip_prefix("entity:")?;
            let (entity_id, shared) = rest.rsplit_once(':')?;
            if entity_id.is_empty() {
                return None;
            }
            Some(LinkReason::Entity {
                entity_id: entity_id.to_string(),
                shared: shared.parse().ok()?,
            })
        }
    }
}

/// Display label of a link reason. For entity links `name` is the reader-
/// visible entity name (None = the reader may not see any name — the label
/// stays generic).
pub fn link_reason_label(reason: &LinkReason, name: Option<&str>) -> String {
    match reason {
        LinkReason::Similar => "podobieństwo semantyczne".to_string(),
        LinkReason::Manual => "powiązanie ręczne".to_string(),
        LinkReason::Entity { shared, .. } => match (name, *shared) {
            (Some(n), s) if s > 1 => format!("wspólne encje: {n} +{}", s - 1),
            (Some(n), _) => format!("wspólna encja: {n}"),
            (None, s) if s > 1 => "wspólne encje".to_string(),
            (None, _) => "wspólna encja".to_string(),
        },
    }
}

/// SQL resolving the entity name a READER is allowed to see for a canonical
/// entity: prefer the canonical row itself, otherwise any alias — but only
/// through rows directly mentioned by a live note the reader can open.
/// Placeholders: entity id ×2, `acl_params`, entity id. Pure — unit-tested.
pub fn visible_entity_name_sql(group_count: usize) -> String {
    let acl = acl_read_clause(group_count);
    format!(
        "SELECT e.name FROM entities e \
         JOIN note_entities ne ON ne.entity_id = e.id \
         JOIN notes n ON n.id = ne.note_id \
         WHERE (e.id = ? OR e.canonical_id = ?) AND n.deleted_at IS NULL AND {acl} \
         ORDER BY CASE WHEN e.id = ? THEN 0 ELSE 1 END LIMIT 1"
    )
}

/// Reader-visible name of a canonical entity (canonical row first, then a
/// visible alias). None = the reader cannot see this entity under any name.
pub fn visible_entity_name(ctx: &UserCtx, entity_id: &str) -> Option<String> {
    let sql = visible_entity_name_sql(ctx.group_ids.len());
    let mut params = vec![
        SqlValue::String(entity_id.to_string()),
        SqlValue::String(entity_id.to_string()),
    ];
    params.extend(acl_params(ctx));
    params.push(SqlValue::String(entity_id.to_string()));
    sql_query_one(&sql, &params)
        .ok()
        .flatten()
        .and_then(|r| r.first().and_then(|v| v.as_str()).map(str::to_string))
}

/// SQL of the related-notes read: links joined to the TARGET note with the
/// reader's ACL and liveness re-checked there. Pure — unit-tested natively.
pub fn related_notes_sql(group_count: usize) -> String {
    let acl = acl_read_clause(group_count);
    format!(
        "SELECT n.id, n.title, l.weight, l.reason, l.created_at \
         FROM note_links l JOIN notes n ON n.id = l.dst_note_id \
         WHERE l.src_note_id = ? AND n.deleted_at IS NULL AND {acl} \
         ORDER BY l.weight DESC LIMIT 24"
    )
}

/// Related notes of `note_id` the user can read, best first. A pair linked
/// both by similarity and by shared entities collapses to its strongest row.
/// Reason labels are resolved HERE, per reader: entity names go through the
/// same visibility logic as the entity chips.
pub fn related_notes(ctx: &UserCtx, note_id: &str) -> Result<Vec<RelatedNote>, String> {
    let sql = related_notes_sql(ctx.group_ids.len());
    let mut params = vec![SqlValue::String(note_id.to_string())];
    params.extend(acl_params(ctx));
    let rows = sql_query(&sql, &params).map_err(|e| format!("Błąd odczytu powiązań: {e}"))?;
    let mut out: Vec<RelatedNote> = Vec::new();
    for r in &rows {
        let id = r
            .first()
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // Rows arrive weight-desc, so the first row per target is its best.
        if id.is_empty() || out.iter().any(|x| x.id == id) {
            continue;
        }
        let raw_reason = r.get(3).and_then(|v| v.as_str()).unwrap_or("");
        let reason = match parse_link_reason(raw_reason) {
            Some(LinkReason::Entity { entity_id, shared }) => {
                let name = visible_entity_name(ctx, &entity_id);
                link_reason_label(
                    &LinkReason::Entity { entity_id, shared },
                    name.as_deref(),
                )
            }
            Some(kind) => link_reason_label(&kind, None),
            None => String::new(),
        };
        out.push(RelatedNote {
            id,
            title: r.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string(),
            weight: r.get(2).and_then(value_f64).unwrap_or(0.0),
            reason,
            created_at: r.get(4).and_then(|v| v.as_i64()).unwrap_or(0),
        });
        if out.len() >= 12 {
            break;
        }
    }
    Ok(out)
}

/// One detected entity of a note (canonical after merges).
#[derive(Debug, Clone)]
pub struct NoteEntity {
    /// Canonical entity id (merge survivor when the mention was merged).
    pub id: String,
    pub name: String,
    pub entity_type: String,
}

/// SQL of the note-entities read: attachments joined to the note with the
/// reader's ACL and liveness. Local and canonical names are returned
/// separately — the canonical name is DISCLOSED only after a separate
/// visibility check (see `note_entities`). Pure — unit-tested natively.
pub fn note_entities_sql(group_count: usize) -> String {
    let acl = acl_read_clause(group_count);
    format!(
        "SELECT COALESCE(e.canonical_id, e.id), e.name, c.name, c.id, \
                COALESCE(c.entity_type, e.entity_type) \
         FROM note_entities ne \
         JOIN notes n ON n.id = ne.note_id \
         JOIN entities e ON e.id = ne.entity_id \
         LEFT JOIN entities c ON c.id = e.canonical_id \
         WHERE ne.note_id = ? AND n.deleted_at IS NULL AND {acl} \
         ORDER BY ne.count DESC LIMIT 24"
    )
}

/// SQL of the entity visibility test: does the reader have read access to at
/// least one live note DIRECTLY mentioning the entity row? Direct semantics
/// (ne.entity_id = ?) on purpose: resolution through an alias must not make a
/// canonical name/entity visible to someone who only reads the alias' note.
/// Pure — unit-tested natively. Placeholders: entity id, then `acl_params`.
pub fn entity_visibility_sql(group_count: usize) -> String {
    let acl = acl_read_clause(group_count);
    format!(
        "SELECT 1 FROM note_entities ne JOIN notes n ON n.id = ne.note_id \
         WHERE ne.entity_id = ? AND n.deleted_at IS NULL AND {acl} LIMIT 1"
    )
}

/// True when the reader can see the entity through some readable live note.
pub fn entity_visible(ctx: &UserCtx, entity_id: &str) -> bool {
    let sql = entity_visibility_sql(ctx.group_ids.len());
    let mut params = vec![SqlValue::String(entity_id.to_string())];
    params.extend(acl_params(ctx));
    matches!(sql_query_one(&sql, &params), Ok(Some(_)))
}

/// Detected entities of a note, canonical entity preferred. Same read ACL +
/// liveness as every other note read path. The canonical (merged) name is
/// shown only when the reader can see the canonical entity through some
/// readable note of their own — otherwise the alias name from THIS note is
/// used, so a merge never leaks a name that originated in a private note.
pub fn note_entities(ctx: &UserCtx, note_id: &str) -> Result<Vec<NoteEntity>, String> {
    let sql = note_entities_sql(ctx.group_ids.len());
    let mut params = vec![SqlValue::String(note_id.to_string())];
    params.extend(acl_params(ctx));
    let rows = sql_query(&sql, &params).map_err(|e| format!("Błąd odczytu encji: {e}"))?;
    let mut out: Vec<NoteEntity> = Vec::new();
    for r in &rows {
        let id = r
            .first()
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // Merged mentions can resolve to the same canonical — dedup for chips.
        if id.is_empty() || out.iter().any(|e| e.id == id) {
            continue;
        }
        let local_name = r.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let canonical_name = r.get(2).and_then(|v| v.as_str()).map(str::to_string);
        let canonical_id = r.get(3).and_then(|v| v.as_str()).map(str::to_string);
        let name = match (canonical_name, canonical_id) {
            (Some(cn), Some(cid)) if entity_visible(ctx, &cid) => cn,
            _ => local_name,
        };
        out.push(NoteEntity {
            id,
            name,
            entity_type: r.get(4).and_then(|v| v.as_str()).unwrap_or("").to_string(),
        });
    }
    Ok(out)
}

/// Creates a manual note-to-note link (both directions, weight 1.0). Requires
/// WRITE access to the source note (the user annotates it) and READ access to
/// the target; both notes must be alive.
pub fn manual_link(ctx: &UserCtx, src_note_id: &str, dst_note_id: &str) -> Result<(), String> {
    if src_note_id == dst_note_id {
        return Err("Nie można powiązać notatki z samą sobą.".to_string());
    }
    ensure_writable(ctx, src_note_id)?;
    if get_note(ctx, dst_note_id)?.is_none() {
        return Err("Wybrana notatka nie istnieje lub nie masz do niej dostępu.".to_string());
    }
    let now = now_unix();
    let insert = "INSERT OR REPLACE INTO note_links \
                  (src_note_id, dst_note_id, kind, weight, reason, created_at) \
                  VALUES (?, ?, 'manual', 1.0, 'manual', ?)";
    let stmts: Vec<(String, Vec<SqlValue>)> = vec![
        (
            insert.to_string(),
            vec![
                SqlValue::String(src_note_id.to_string()),
                SqlValue::String(dst_note_id.to_string()),
                SqlValue::I64(now),
            ],
        ),
        (
            insert.to_string(),
            vec![
                SqlValue::String(dst_note_id.to_string()),
                SqlValue::String(src_note_id.to_string()),
                SqlValue::I64(now),
            ],
        ),
    ];
    run_transaction(&stmts).map_err(|e| format!("Błąd zapisu powiązania: {e}"))
}

// =============================================================================
// Graph view read side (mockup n02). All rows returned here already passed
// the reader's ACL: notes through acl_read_clause, entities through the
// direct-mention visibility rule of card_entities_sql. The pure graph build
// (scope/type filters, BFS depth, node cap) lives in ui_graph.rs.
// =============================================================================

/// Upper bound of notes considered for the graph BEFORE the 500-node cap.
pub const GRAPH_NOTE_LIMIT: usize = 800;

/// One accessible note for the graph, with its filter bucket.
#[derive(Debug, Clone)]
pub struct GraphNoteRow {
    pub id: String,
    pub title: String,
    pub updated_at: i64,
    /// "mine" | "shared" (direct user share) | "group" | "org" — the grant
    /// through which THIS reader reaches the note (most specific wins).
    pub bucket: String,
}

/// SQL of the graph notes read: accessible live notes, newest first, with the
/// reader-specific bucket. Pure — unit-tested natively. Placeholders: user id
/// (mine), user id (shared), group ids (group branch, when any), then
/// `acl_params`.
pub fn graph_notes_sql(group_count: usize) -> String {
    let acl = acl_read_clause(group_count);
    let group_branch = if group_count > 0 {
        format!(
            "WHEN EXISTS (SELECT 1 FROM note_shares s WHERE s.note_id = n.id \
             AND s.subject_type = 'group' AND s.subject_id IN ({})) THEN 'group' ",
            placeholders(group_count)
        )
    } else {
        String::new()
    };
    format!(
        "SELECT n.id, n.title, n.updated_at, \
         CASE WHEN n.owner_user_id = ? THEN 'mine' \
              WHEN EXISTS (SELECT 1 FROM note_shares s WHERE s.note_id = n.id \
                   AND s.subject_type = 'user' AND s.subject_id = ?) THEN 'shared' \
              {group_branch}ELSE 'org' END \
         FROM notes n WHERE n.deleted_at IS NULL AND {acl} \
         ORDER BY n.updated_at DESC LIMIT {GRAPH_NOTE_LIMIT}"
    )
}

/// Accessible notes for the graph view, newest first.
pub fn graph_notes(ctx: &UserCtx) -> Result<Vec<GraphNoteRow>, String> {
    let sql = graph_notes_sql(ctx.group_ids.len());
    let mut params = vec![
        SqlValue::String(ctx.user_id.clone()),
        SqlValue::String(ctx.user_id.clone()),
    ];
    params.extend(ctx.group_ids.iter().map(|g| SqlValue::String(g.clone())));
    params.extend(acl_params(ctx));
    let rows = sql_query(&sql, &params).map_err(|e| format!("Błąd odczytu grafu: {e}"))?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            let id = r.first().and_then(|v| v.as_str())?.to_string();
            Some(GraphNoteRow {
                id,
                title: r.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string(),
                updated_at: r.get(2).and_then(|v| v.as_i64()).unwrap_or(0),
                bucket: r
                    .get(3)
                    .and_then(|v| v.as_str())
                    .unwrap_or("org")
                    .to_string(),
            })
        })
        .collect())
}

/// One entity mention on a note the reader can open. The entity id is the
/// canonical id (merge survivor) and the name is the reader-visible one.
#[derive(Debug, Clone)]
pub struct GraphMentionRow {
    pub note_id: String,
    pub entity_id: String,
    pub name: String,
    pub entity_type: String,
}

/// Entity mentions of the given (already ACL-passed) notes for the graph —
/// the same batched query + canonical-name disclosure rule as the list cards.
pub fn graph_mentions(ctx: &UserCtx, note_ids: &[String]) -> Vec<GraphMentionRow> {
    if note_ids.is_empty() {
        return Vec::new();
    }
    let sql = card_entities_sql(note_ids.len(), ctx.group_ids.len());
    let params = card_entities_params(ctx, note_ids);
    let rows = match sql_query(&sql, &params) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    rows.iter()
        .filter_map(|r| {
            let note_id = r.first().and_then(|v| v.as_str())?.to_string();
            let entity_id = r.get(1).and_then(|v| v.as_str())?.to_string();
            let local = r.get(2).and_then(|v| v.as_str()).unwrap_or("");
            let canonical = r.get(3).and_then(|v| v.as_str());
            let visible = r.get(4 + 1).and_then(|v| v.as_i64()).unwrap_or(0) == 1;
            let name = pick_card_entity_name(local, canonical, visible);
            if entity_id.is_empty() || name.is_empty() {
                return None;
            }
            Some(GraphMentionRow {
                note_id,
                entity_id,
                name,
                entity_type: r
                    .get(4)
                    .and_then(|v| v.as_str())
                    .unwrap_or("other")
                    .to_string(),
            })
        })
        .collect()
}

/// One note-to-note link where BOTH endpoints are in the reader's note set.
#[derive(Debug, Clone)]
pub struct GraphLinkRow {
    pub src: String,
    pub dst: String,
    pub kind: String,
    pub weight: f64,
    /// Persisted machine token (see `parse_link_reason`).
    pub reason: String,
}

/// Links among the given (already ACL-passed) notes. Constraining BOTH
/// endpoints to the accessible set is the ACL guarantee: a link touching an
/// inaccessible note never reaches the view.
pub fn graph_links(ctx: &UserCtx, note_ids: &[String]) -> Vec<GraphLinkRow> {
    let _ = ctx;
    if note_ids.is_empty() {
        return Vec::new();
    }
    let ph = placeholders(note_ids.len());
    let sql = format!(
        "SELECT src_note_id, dst_note_id, kind, weight, reason FROM note_links \
         WHERE src_note_id IN ({ph}) AND dst_note_id IN ({ph})"
    );
    let mut params: Vec<SqlValue> = note_ids
        .iter()
        .map(|id| SqlValue::String(id.clone()))
        .collect();
    params.extend(note_ids.iter().map(|id| SqlValue::String(id.clone())));
    let rows = match sql_query(&sql, &params) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    rows.iter()
        .filter_map(|r| {
            Some(GraphLinkRow {
                src: r.first().and_then(|v| v.as_str())?.to_string(),
                dst: r.get(1).and_then(|v| v.as_str())?.to_string(),
                kind: r.get(2).and_then(|v| v.as_str()).unwrap_or("").to_string(),
                weight: r.get(3).and_then(value_f64).unwrap_or(0.0),
                reason: r.get(4).and_then(|v| v.as_str()).unwrap_or("").to_string(),
            })
        })
        .collect()
}

// =============================================================================
// Hybrid search read side (mockup n05). Every helper here applies
// acl_read_clause (and liveness) BEFORE any row leaves the database — the
// engine in search.rs ranks only ids returned by these functions.
// =============================================================================

/// Metadata + content prefix of an accessible note (search result card and
/// the answer-prompt source).
#[derive(Debug, Clone)]
pub struct SearchNoteMeta {
    pub id: String,
    pub title: String,
    pub content: String,
    pub updated_at: i64,
    pub owner_user_id: String,
    /// Widest share on the note ("private" | "user" | "group" | "org").
    pub scope: String,
    /// Group display name when group-scoped and resolvable (scope badge). Holds
    /// the raw group subject id until `search_notes_meta` resolves the names.
    pub group_name: Option<String>,
}

fn search_meta_from_row(row: &[SqlValue]) -> Option<SearchNoteMeta> {
    Some(SearchNoteMeta {
        id: row.first().and_then(|v| v.as_str())?.to_string(),
        title: row.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string(),
        content: row.get(2).and_then(|v| v.as_str()).unwrap_or("").to_string(),
        updated_at: row.get(3).and_then(|v| v.as_i64()).unwrap_or(0),
        owner_user_id: row.get(4).and_then(|v| v.as_str()).unwrap_or("").to_string(),
        scope: row
            .get(5)
            .and_then(|v| v.as_str())
            .unwrap_or("private")
            .to_string(),
        group_name: row.get(6).and_then(|v| v.as_str()).map(str::to_string),
    })
}

const SEARCH_META_COLUMNS: &str = "n.id, n.title, substr(n.content, 1, 4000), n.updated_at, \
     n.owner_user_id, \
     (SELECT CASE \
        WHEN EXISTS (SELECT 1 FROM note_shares x WHERE x.note_id = n.id AND x.subject_type = 'org') THEN 'org' \
        WHEN EXISTS (SELECT 1 FROM note_shares x WHERE x.note_id = n.id AND x.subject_type = 'group') THEN 'group' \
        WHEN EXISTS (SELECT 1 FROM note_shares x WHERE x.note_id = n.id AND x.subject_type = 'user') THEN 'user' \
        ELSE 'private' END), \
     (SELECT g.subject_id FROM note_shares g WHERE g.note_id = n.id AND g.subject_type = 'group' LIMIT 1)";

/// Accessible-set filter of the search candidates: the returned map contains
/// ONLY live notes passing acl_read + the scope chip. Candidates absent here
/// never reach ranking. Placeholder order: candidate ids, acl, scope.
pub fn search_notes_meta(
    ctx: &UserCtx,
    candidate_ids: &[String],
    scope: &str,
    recent_cutoff: Option<i64>,
) -> Result<std::collections::HashMap<String, SearchNoteMeta>, String> {
    let mut out = std::collections::HashMap::new();
    if candidate_ids.is_empty() {
        return Ok(out);
    }
    let acl = acl_read_clause(ctx.group_ids.len());
    let (scope_sql, scope_params) = scope_clause(scope, ctx);
    let recent_sql = if recent_cutoff.is_some() {
        " AND n.created_at >= ?"
    } else {
        ""
    };
    let sql = format!(
        "SELECT {SEARCH_META_COLUMNS} FROM notes n \
         WHERE n.id IN ({}) AND n.deleted_at IS NULL AND {acl}{scope_sql}{recent_sql}",
        placeholders(candidate_ids.len())
    );
    let mut params: Vec<SqlValue> = candidate_ids
        .iter()
        .map(|id| SqlValue::String(id.clone()))
        .collect();
    params.extend(acl_params(ctx));
    params.extend(scope_params);
    if let Some(cutoff) = recent_cutoff {
        params.push(SqlValue::I64(cutoff));
    }
    let rows = sql_query(&sql, &params).map_err(|e| format!("Błąd odczytu wyników: {e}"))?;
    for r in &rows {
        if let Some(meta) = search_meta_from_row(r) {
            out.insert(meta.id.clone(), meta);
        }
    }
    resolve_group_names(out.values_mut().map(|m| (&m.scope, &mut m.group_name)));
    Ok(out)
}

/// LIKE fallback search (aliases unbound): accessible live notes matching the
/// phrase in title or content, newest first.
pub fn text_search_notes(
    ctx: &UserCtx,
    scope: &str,
    query: &str,
    recent_cutoff: Option<i64>,
) -> Result<Vec<SearchNoteMeta>, String> {
    let acl = acl_read_clause(ctx.group_ids.len());
    let (scope_sql, scope_params) = scope_clause(scope, ctx);
    let recent_sql = if recent_cutoff.is_some() {
        " AND n.created_at >= ?"
    } else {
        ""
    };
    let sql = format!(
        "SELECT {SEARCH_META_COLUMNS} FROM notes n \
         WHERE n.deleted_at IS NULL AND {acl}{scope_sql}{recent_sql} \
         AND (n.title LIKE ? ESCAPE '\\' OR n.content LIKE ? ESCAPE '\\') \
         ORDER BY n.updated_at DESC LIMIT 30"
    );
    let mut params = acl_params(ctx);
    params.extend(scope_params);
    if let Some(cutoff) = recent_cutoff {
        params.push(SqlValue::I64(cutoff));
    }
    let pattern = format!("%{}%", escape_like(query.trim()));
    params.push(SqlValue::String(pattern.clone()));
    params.push(SqlValue::String(pattern));
    let rows = sql_query(&sql, &params).map_err(|e| format!("Błąd wyszukiwania: {e}"))?;
    let mut metas: Vec<SearchNoteMeta> = rows.iter().filter_map(|r| search_meta_from_row(r)).collect();
    resolve_group_names(metas.iter_mut().map(|m| (&m.scope, &mut m.group_name)));
    Ok(metas)
}

/// Canonical entities whose name matches any query token, restricted to
/// entities the reader can SEE (direct mention through a readable live note —
/// the same rule as chips). Returns (id, name, entity_type).
pub fn match_query_entities(
    ctx: &UserCtx,
    tokens: &[String],
) -> Result<Vec<(String, String, String)>, String> {
    if tokens.is_empty() {
        return Ok(Vec::new());
    }
    let acl = acl_read_clause(ctx.group_ids.len());
    let name_like = tokens
        .iter()
        .map(|_| "e.name LIKE ? ESCAPE '\\'")
        .collect::<Vec<_>>()
        .join(" OR ");
    // Visibility: some live readable note mentions a row resolving to this
    // canonical entity — the reader already knows the entity exists.
    let sql = format!(
        "SELECT DISTINCT e.id, e.name, e.entity_type FROM entities e \
         WHERE e.canonical_id IS NULL AND ({name_like}) \
         AND EXISTS (SELECT 1 FROM note_entities ne \
                     JOIN entities a ON a.id = ne.entity_id \
                     JOIN notes n ON n.id = ne.note_id \
                     WHERE COALESCE(a.canonical_id, a.id) = e.id \
                       AND n.deleted_at IS NULL AND {acl}) \
         LIMIT 8"
    );
    let mut params: Vec<SqlValue> = tokens
        .iter()
        .map(|t| SqlValue::String(format!("%{}%", escape_like(t))))
        .collect();
    params.extend(acl_params(ctx));
    let rows = sql_query(&sql, &params).map_err(|e| format!("Błąd dopasowania encji: {e}"))?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            Some((
                r.first().and_then(|v| v.as_str())?.to_string(),
                r.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string(),
                r.get(2).and_then(|v| v.as_str()).unwrap_or("other").to_string(),
            ))
        })
        .collect())
}

/// Accessible live notes directly mentioning any of the given canonical
/// entities: (note_id, entity_id, mention_count), strongest mention first.
pub fn notes_mentioning_entities(
    ctx: &UserCtx,
    entity_ids: &[String],
) -> Result<Vec<(String, String, i64)>, String> {
    if entity_ids.is_empty() {
        return Ok(Vec::new());
    }
    let acl = acl_read_clause(ctx.group_ids.len());
    let sql = format!(
        "SELECT ne.note_id, COALESCE(e.canonical_id, e.id), MAX(ne.count) \
         FROM note_entities ne \
         JOIN entities e ON e.id = ne.entity_id \
         JOIN notes n ON n.id = ne.note_id \
         WHERE COALESCE(e.canonical_id, e.id) IN ({}) \
           AND n.deleted_at IS NULL AND {acl} \
         GROUP BY ne.note_id, COALESCE(e.canonical_id, e.id) \
         ORDER BY MAX(ne.count) DESC LIMIT 40",
        placeholders(entity_ids.len())
    );
    let mut params: Vec<SqlValue> = entity_ids
        .iter()
        .map(|id| SqlValue::String(id.clone()))
        .collect();
    params.extend(acl_params(ctx));
    let rows = sql_query(&sql, &params).map_err(|e| format!("Błąd odczytu wzmianek: {e}"))?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            Some((
                r.first().and_then(|v| v.as_str())?.to_string(),
                r.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string(),
                r.get(2).and_then(|v| v.as_i64()).unwrap_or(1),
            ))
        })
        .collect())
}

/// Accessible live notes linked to any of the given notes (second graph hop):
/// (via_note_id, note_id, link_weight), strongest link first. The TARGET note
/// carries the ACL — a link into an unreadable note never returns.
pub fn notes_linked_to(
    ctx: &UserCtx,
    note_ids: &[String],
) -> Result<Vec<(String, String, f64)>, String> {
    if note_ids.is_empty() {
        return Ok(Vec::new());
    }
    let acl = acl_read_clause(ctx.group_ids.len());
    let sql = format!(
        "SELECT l.src_note_id, l.dst_note_id, MAX(l.weight) FROM note_links l \
         JOIN notes n ON n.id = l.dst_note_id \
         WHERE l.src_note_id IN ({}) AND n.deleted_at IS NULL AND {acl} \
         GROUP BY l.src_note_id, l.dst_note_id \
         ORDER BY MAX(l.weight) DESC LIMIT 40",
        placeholders(note_ids.len())
    );
    let mut params: Vec<SqlValue> = note_ids
        .iter()
        .map(|id| SqlValue::String(id.clone()))
        .collect();
    params.extend(acl_params(ctx));
    let rows = sql_query(&sql, &params).map_err(|e| format!("Błąd odczytu powiązań: {e}"))?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            Some((
                r.first().and_then(|v| v.as_str())?.to_string(),
                r.get(1).and_then(|v| v.as_str())?.to_string(),
                r.get(2).and_then(value_f64).unwrap_or(0.0),
            ))
        })
        .collect())
}

/// Accessible notes connected to an entity within one hop: direct mentions
/// plus notes linked to a mentioning note (right-rail narrowing).
pub fn notes_connected_to_entity(ctx: &UserCtx, entity_id: &str) -> Result<Vec<String>, String> {
    let direct = notes_mentioning_entities(ctx, &[entity_id.to_string()])?;
    let mut out: Vec<String> = direct.iter().map(|(id, _, _)| id.clone()).collect();
    let linked = notes_linked_to(ctx, &out)?;
    for (_, id, _) in linked {
        if !out.iter().any(|x| x == &id) {
            out.push(id);
        }
    }
    Ok(out)
}

// =============================================================================
// Granular shares (mockup n03). The modal owns ALL note_shares rows of a note;
// reads are owner-gated in the UI layer, writes re-check ownership in SQL.
// =============================================================================

/// One share row of a note.
#[derive(Debug, Clone, PartialEq)]
pub struct ShareEntry {
    /// "user" | "group" | "org".
    pub subject_type: String,
    pub subject_id: String,
    /// "read" | "write".
    pub access: String,
}

/// All share rows of a note (readable by anyone who can read the note; the
/// modal itself opens only for the owner). Rows whose subject no longer
/// resolves in the directory (deactivated/removed user, deleted group) are
/// skipped: seeding the modal draft with them would make the whole save fail
/// validation later.
pub fn note_shares_list(ctx: &UserCtx, note_id: &str) -> Result<Vec<ShareEntry>, String> {
    if get_note(ctx, note_id)?.is_none() {
        return Err("Notatka nie istnieje lub nie masz do niej dostępu.".to_string());
    }
    let rows = sql_query(
        "SELECT subject_type, subject_id, access FROM note_shares \
         WHERE note_id = ? ORDER BY subject_type, subject_id",
        &[SqlValue::String(note_id.to_string())],
    )
    .map_err(|e| format!("Błąd odczytu udostępnień: {e}"))?;
    let entries: Vec<ShareEntry> = rows
        .iter()
        .filter_map(|r| {
            Some(ShareEntry {
                subject_type: r.first().and_then(|v| v.as_str())?.to_string(),
                subject_id: r.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string(),
                access: r.get(2).and_then(|v| v.as_str()).unwrap_or("read").to_string(),
            })
        })
        .collect();
    let (users, groups) = directory_id_sets()?;
    let (kept, skipped) = partition_stale_shares(entries, &users, &groups);
    for subject in &skipped {
        log::info(&format!(
            "notes: pomijam nieaktualne udostępnienie notatki {note_id} — podmiot {subject} nie istnieje w katalogu"
        ));
    }
    Ok(kept)
}

/// Current directory id sets: ACTIVE users only + all visible groups. The
/// host already filters inactive accounts out of `directory_users`, but the
/// SDK exposes `is_active`, so this filters again rather than trusting that
/// behavior to never change.
fn directory_id_sets() -> Result<(Vec<String>, Vec<String>), String> {
    let users: Vec<String> = directory_users()
        .map_err(|e| format!("Błąd katalogu użytkowników: {e}"))?
        .into_iter()
        .filter(|u| u.is_active)
        .map(|u| u.id)
        .collect();
    let groups: Vec<String> = tentaflow_addon_sdk::directory_groups()
        .map_err(|e| format!("Błąd katalogu grup: {e}"))?
        .into_iter()
        .map(|g| g.id)
        .collect();
    Ok((users, groups))
}

/// Splits share rows into (still resolvable in the directory, skipped subject
/// ids). Org rows carry no subject and always stay. Pure w.r.t. the injected
/// id sets — unit-tested natively.
pub fn partition_stale_shares(
    entries: Vec<ShareEntry>,
    user_ids: &[String],
    group_ids: &[String],
) -> (Vec<ShareEntry>, Vec<String>) {
    let mut kept = Vec::with_capacity(entries.len());
    let mut skipped = Vec::new();
    for e in entries {
        let resolvable = match e.subject_type.as_str() {
            "user" => user_ids.iter().any(|u| u == &e.subject_id),
            "group" => group_ids.iter().any(|g| g == &e.subject_id),
            _ => true,
        };
        if resolvable {
            kept.push(e);
        } else {
            skipped.push(format!("{}:{}", e.subject_type, e.subject_id));
        }
    }
    (kept, skipped)
}

/// Validates a share draft against the directory: every user id must resolve
/// to an ACTIVE account, every group id must exist, org rows are read-only,
/// access values constrained. Pure w.r.t. the injected id sets — unit-tested
/// natively (callers pass the active-only sets from `directory_id_sets`).
pub fn validate_share_entries(
    entries: &[ShareEntry],
    user_ids: &[String],
    group_ids: &[String],
    owner_id: &str,
) -> Result<(), String> {
    for e in entries {
        if !matches!(e.access.as_str(), "read" | "write") {
            return Err(format!("Nieznany poziom dostępu: {}", e.access));
        }
        match e.subject_type.as_str() {
            "user" => {
                if e.subject_id == owner_id {
                    return Err("Właściciel ma zawsze pełny dostęp — nie dodawaj go do listy.".into());
                }
                if !user_ids.iter().any(|u| u == &e.subject_id) {
                    return Err(
                        "Wybrany użytkownik nie istnieje w katalogu lub jest nieaktywny."
                            .to_string(),
                    );
                }
            }
            "group" => {
                if !group_ids.iter().any(|g| g == &e.subject_id) {
                    return Err("Wybrana grupa nie istnieje w katalogu.".to_string());
                }
            }
            "org" => {
                if e.access != "read" {
                    return Err("Udostępnienie całej organizacji jest tylko do odczytu.".into());
                }
                if !e.subject_id.is_empty() {
                    return Err("Wpis organizacyjny nie może wskazywać podmiotu.".to_string());
                }
            }
            other => return Err(format!("Nieznany typ podmiotu: {other}")),
        }
    }
    // Duplicates would silently collapse on the PK — reject up front.
    for (i, a) in entries.iter().enumerate() {
        if entries[i + 1..]
            .iter()
            .any(|b| b.subject_type == a.subject_type && b.subject_id == a.subject_id)
        {
            return Err("Ten sam podmiot występuje na liście dwa razy.".to_string());
        }
    }
    Ok(())
}

/// Replaces ALL shares of a note with the validated draft — owner only, one
/// transaction, every statement re-checking ownership/liveness in its WHERE.
pub fn replace_all_shares(
    ctx: &UserCtx,
    note_id: &str,
    entries: &[ShareEntry],
) -> Result<(), String> {
    // Friendly error up front; the guards below stay authoritative.
    let owner_check = sql_query_one(
        "SELECT owner_user_id FROM notes WHERE id = ? AND deleted_at IS NULL",
        &[SqlValue::String(note_id.to_string())],
    )
    .map_err(|e| format!("Błąd odczytu notatki: {e}"))?;
    let owner = owner_check
        .and_then(|r| r.first().and_then(|v| v.as_str()).map(str::to_string))
        .ok_or_else(|| "Notatka nie istnieje.".to_string())?;
    if owner != ctx.user_id {
        return Err("Tylko właściciel może zmieniać udostępnianie.".to_string());
    }

    // The directory is fetched HERE, right before the transaction — never
    // from the draft: a modal left open across a user deactivation or group
    // deletion must fail validation wholesale, not persist stale subjects.
    let (users, groups) = directory_id_sets()?;
    validate_share_entries(entries, &users, &groups, &owner)?;

    let guard = owner_guard_clause();
    let guard_params = vec![
        SqlValue::String(note_id.to_string()),
        SqlValue::String(ctx.user_id.clone()),
    ];
    let now = now_unix();

    let mut delete_params = vec![SqlValue::String(note_id.to_string())];
    delete_params.extend(guard_params.clone());
    let mut stmts: Vec<(String, Vec<SqlValue>)> = vec![(
        format!("DELETE FROM note_shares WHERE note_id = ? AND {guard}"),
        delete_params,
    )];
    for e in entries {
        let mut params = vec![
            SqlValue::String(note_id.to_string()),
            SqlValue::String(e.subject_type.clone()),
            SqlValue::String(e.subject_id.clone()),
            SqlValue::String(e.access.clone()),
            SqlValue::String(ctx.user_id.clone()),
            SqlValue::I64(now),
        ];
        params.extend(guard_params.clone());
        stmts.push((
            format!(
                "INSERT INTO note_shares \
                 (note_id, subject_type, subject_id, access, created_by, created_at) \
                 SELECT ?, ?, ?, ?, ?, ? WHERE {guard}"
            ),
            params,
        ));
    }
    run_transaction(&stmts).map_err(|e| format!("Błąd zapisu udostępnień: {e}"))
}

// =============================================================================
// Misc helpers
// =============================================================================

pub fn now_unix() -> i64 {
    (now_unix_ms() / 1000) as i64
}

pub fn now_unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

pub fn new_id(prefix: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}_{:x}_{:x}", now_unix_ms(), n)
}

// =============================================================================
// Tests — pure helpers only (no host fns on the native target)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(groups: &[&str]) -> UserCtx {
        UserCtx {
            user_id: "u1".into(),
            display_name: "User One".into(),
            group_ids: groups.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn acl_read_clause_without_groups_has_no_group_branch() {
        let sql = acl_read_clause(0);
        assert!(sql.contains("n.owner_user_id = ?"));
        assert!(sql.contains("s.subject_type = 'org'"));
        assert!(!sql.contains("'group'"));
    }

    #[test]
    fn acl_read_clause_group_placeholders_match_param_count() {
        let c = ctx(&["g1", "g2"]);
        let sql = acl_read_clause(c.group_ids.len());
        let params = acl_params(&c);
        assert_eq!(sql.matches('?').count(), params.len());
        assert!(sql.contains("s.subject_id IN (?, ?)"));
    }

    #[test]
    fn acl_write_clause_requires_write_access_on_shares() {
        let sql = acl_write_clause(1);
        assert!(sql.contains("s.access = 'write'"));
        // Owner branch stays unconditional.
        assert!(sql.starts_with("(n.owner_user_id = ?"));
    }

    #[test]
    fn scope_group_without_groups_matches_nothing() {
        let (sql, params) = scope_clause("group", &ctx(&[]));
        assert_eq!(sql, " AND 0");
        assert!(params.is_empty());
    }

    #[test]
    fn scope_clauses_bind_expected_params() {
        let c = ctx(&["g1"]);
        let (mine, p1) = scope_clause("mine", &c);
        assert!(mine.contains("owner_user_id = ?"));
        assert_eq!(p1.len(), 1);

        let (group, p2) = scope_clause("group", &c);
        assert_eq!(group.matches('?').count(), p2.len());

        let (org, p3) = scope_clause("org", &c);
        assert!(org.contains("subject_type = 'org'"));
        assert!(p3.is_empty());

        let (all, p4) = scope_clause("all", &c);
        assert!(all.is_empty());
        assert!(p4.is_empty());
    }

    #[test]
    fn escape_like_escapes_metacharacters() {
        assert_eq!(escape_like("100%_a\\b"), "100\\%\\_a\\\\b");
        assert_eq!(escape_like("zwykły tekst"), "zwykły tekst");
    }

    #[test]
    fn note_preview_cuts_on_char_boundary_and_collapses_whitespace() {
        assert_eq!(note_preview("a  b\n\nc", 100), "a b c");
        let cut = note_preview("żółćżółćżółć", 5);
        assert_eq!(cut, "żółćż…");
    }

    #[test]
    fn counter_label_polish_plurals() {
        assert_eq!(counter_label("a"), "1 znak · 1 akapit");
        assert!(counter_label("ab\n\ncd").contains("2 akapity"));
        assert!(counter_label("a\n\nb\n\nc\n\nd\n\ne").contains("5 akapitów"));
        let big = "x".repeat(1248);
        assert!(counter_label(&big).starts_with("1\u{202f}248 znaków"));
        assert_eq!(plural_pl(12, "znak", "znaki", "znaków"), "znaków");
        assert_eq!(plural_pl(22, "znak", "znaki", "znaków"), "znaki");
    }

    #[test]
    fn share_entries_validate_directory_and_reject_junk() {
        let users = vec!["u2".to_string(), "u3".to_string()];
        let groups = vec!["g1".to_string()];
        let entry = |t: &str, s: &str, a: &str| ShareEntry {
            subject_type: t.into(),
            subject_id: s.into(),
            access: a.into(),
        };
        // Valid draft: two users, one group, org read.
        assert!(validate_share_entries(
            &[
                entry("user", "u2", "write"),
                entry("user", "u3", "read"),
                entry("group", "g1", "read"),
                entry("org", "", "read"),
            ],
            &users,
            &groups,
            "u1",
        )
        .is_ok());
        // Unknown user / group is rejected up front.
        assert!(validate_share_entries(&[entry("user", "ghost", "read")], &users, &groups, "u1").is_err());
        assert!(validate_share_entries(&[entry("group", "g9", "read")], &users, &groups, "u1").is_err());
        // Owner never appears on their own list.
        assert!(validate_share_entries(&[entry("user", "u1", "read")], &users, &groups, "u1").is_err());
        // Org must be read-only and subject-less.
        assert!(validate_share_entries(&[entry("org", "", "write")], &users, &groups, "u1").is_err());
        assert!(validate_share_entries(&[entry("org", "x", "read")], &users, &groups, "u1").is_err());
        // Unknown access / subject type.
        assert!(validate_share_entries(&[entry("user", "u2", "admin")], &users, &groups, "u1").is_err());
        assert!(validate_share_entries(&[entry("robot", "r1", "read")], &users, &groups, "u1").is_err());
        // Duplicate subject collapses on the PK — rejected explicitly.
        assert!(validate_share_entries(
            &[entry("user", "u2", "read"), entry("user", "u2", "write")],
            &users,
            &groups,
            "u1",
        )
        .is_err());
    }

    #[test]
    fn stale_shares_are_partitioned_out_with_their_subject_ids() {
        let users = vec!["u2".to_string()];
        let groups = vec!["g1".to_string()];
        let entry = |t: &str, s: &str, a: &str| ShareEntry {
            subject_type: t.into(),
            subject_id: s.into(),
            access: a.into(),
        };
        let (kept, skipped) = partition_stale_shares(
            vec![
                entry("user", "u2", "write"),
                // Deactivated/removed user — absent from the active id set.
                entry("user", "u_gone", "read"),
                entry("group", "g1", "read"),
                entry("group", "g_deleted", "write"),
                // Org rows carry no subject and always stay.
                entry("org", "", "read"),
            ],
            &users,
            &groups,
        );
        assert_eq!(
            kept,
            vec![
                entry("user", "u2", "write"),
                entry("group", "g1", "read"),
                entry("org", "", "read"),
            ]
        );
        assert_eq!(
            skipped,
            vec!["user:u_gone".to_string(), "group:g_deleted".to_string()]
        );
        // Nothing stale → nothing skipped.
        let (kept, skipped) =
            partition_stale_shares(vec![entry("user", "u2", "read")], &users, &groups);
        assert_eq!(kept.len(), 1);
        assert!(skipped.is_empty());
    }

    #[test]
    fn search_meta_sql_paths_apply_read_acl_and_scope() {
        let c = ctx(&["g1"]);
        // The shared column set already carries the widest-scope CASE; the
        // accessible-set query gates on acl_read + liveness.
        assert!(SEARCH_META_COLUMNS.contains("ELSE 'private' END"));
        let (scope_sql, _) = scope_clause("mine", &c);
        assert!(scope_sql.contains("owner_user_id"));
    }

    #[test]
    fn write_guard_placeholders_match_param_count() {
        let c = ctx(&["g1", "g2"]);
        let guard = write_guard_clause(c.group_ids.len());
        let params = write_guard_params(&c, "note_x");
        assert_eq!(guard.matches('?').count(), params.len());
        assert!(guard.contains("n.deleted_at IS NULL"));
        assert!(guard.contains("s.access = 'write'"));
        assert_eq!(owner_guard_clause().matches('?').count(), 2);
        assert!(owner_guard_clause().contains("n.owner_user_id = ?"));
    }

    #[test]
    fn related_notes_sql_guards_target_note_with_read_acl() {
        let c = ctx(&["g1", "g2"]);
        let sql = related_notes_sql(c.group_ids.len());
        // The ACL predicate applies to the TARGET note of the link.
        assert!(sql.contains("JOIN notes n ON n.id = l.dst_note_id"));
        assert!(sql.contains("n.deleted_at IS NULL"));
        assert!(sql.contains("n.owner_user_id = ?"));
        // Placeholders: note id + full ACL parameter set.
        assert_eq!(sql.matches('?').count(), 1 + acl_params(&c).len());
    }

    #[test]
    fn note_entities_sql_guards_note_with_read_acl_and_keeps_names_separate() {
        let c = ctx(&[]);
        let sql = note_entities_sql(c.group_ids.len());
        assert!(sql.contains("COALESCE(e.canonical_id, e.id)"));
        // Local and canonical names come back separately — the canonical name
        // is disclosed only after entity_visible passes (no COALESCE leak).
        assert!(sql.contains("e.name, c.name"));
        assert!(!sql.contains("COALESCE(c.name"));
        assert!(sql.contains("n.deleted_at IS NULL"));
        assert!(sql.contains("n.owner_user_id = ?"));
        assert_eq!(sql.matches('?').count(), 1 + acl_params(&c).len());
    }

    #[test]
    fn link_reason_tokens_roundtrip_and_reject_garbage() {
        let token = entity_reason_token("ent_00ff", 3);
        assert_eq!(token, "entity:ent_00ff:3");
        assert_eq!(
            parse_link_reason(&token),
            Some(LinkReason::Entity {
                entity_id: "ent_00ff".into(),
                shared: 3
            })
        );
        assert_eq!(parse_link_reason("similar"), Some(LinkReason::Similar));
        assert_eq!(parse_link_reason("manual"), Some(LinkReason::Manual));
        // Legacy / malformed values never resolve to a label with a name.
        assert_eq!(parse_link_reason("wspólna encja: Tajna Sp. z o.o."), None);
        assert_eq!(parse_link_reason("entity:"), None);
        assert_eq!(parse_link_reason("entity::1"), None);
        assert_eq!(parse_link_reason("entity:ent_x:abc"), None);
        assert_eq!(parse_link_reason(""), None);
    }

    #[test]
    fn entity_link_label_never_uses_invisible_canonical_name() {
        // Codex scenario: the reader can open notes A and B, linked by an
        // entity whose CANONICAL name comes from a private note C. The row
        // persists only the id; at read time the name resolver returns:
        //   * None (nothing visible)          -> generic label, no name,
        //   * the reader-visible ALIAS name   -> alias label,
        //   * the canonical name ONLY when the canonical row itself is
        //     reachable through the reader's notes.
        let reason = parse_link_reason(&entity_reason_token("ent_c", 1)).unwrap();
        assert_eq!(link_reason_label(&reason, None), "wspólna encja");
        assert_eq!(
            link_reason_label(&reason, Some("Nexadata (alias)")),
            "wspólna encja: Nexadata (alias)"
        );
        let many = parse_link_reason(&entity_reason_token("ent_c", 3)).unwrap();
        assert_eq!(link_reason_label(&many, None), "wspólne encje");
        assert_eq!(
            link_reason_label(&many, Some("Nexadata")),
            "wspólne encje: Nexadata +2"
        );
        assert_eq!(
            link_reason_label(&LinkReason::Similar, None),
            "podobieństwo semantyczne"
        );
        assert_eq!(
            link_reason_label(&LinkReason::Manual, None),
            "powiązanie ręczne"
        );
    }

    #[test]
    fn visible_entity_name_sql_prefers_canonical_and_is_acl_guarded() {
        let c = ctx(&["g1"]);
        let sql = visible_entity_name_sql(c.group_ids.len());
        // Canonical row OR its aliases, only through readable live notes,
        // canonical preferred when both are visible.
        assert!(sql.contains("e.id = ? OR e.canonical_id = ?"));
        assert!(sql.contains("ORDER BY CASE WHEN e.id = ? THEN 0 ELSE 1 END"));
        assert!(sql.contains("n.deleted_at IS NULL"));
        assert!(sql.contains("n.owner_user_id = ?"));
        // Placeholders: id, id, acl params, id (textual order).
        assert_eq!(sql.matches('?').count(), 3 + acl_params(&c).len());
    }

    #[test]
    fn card_entities_sql_guards_canonical_disclosure_per_reader() {
        let c = ctx(&["g1"]);
        let sql = card_entities_sql(3, c.group_ids.len());
        // Local and canonical names are selected SEPARATELY — no blind
        // COALESCE(c.name, e.name) that would leak a private canonical name.
        assert!(sql.contains("e.name, c.name"));
        assert!(!sql.contains("COALESCE(c.name"));
        // The visibility flag reuses the direct-mention rule, correlated on
        // the canonical row (not resolved through the alias).
        assert!(sql.contains("ne.entity_id = c.id"));
        assert!(sql.contains("CASE WHEN c.id IS NOT NULL AND EXISTS"));
        assert!(sql.contains("n.deleted_at IS NULL"));
        // Placeholders: 3 note ids + the ACL params of the EXISTS clause.
        assert_eq!(sql.matches('?').count(), 3 + acl_params(&c).len());
    }

    #[test]
    fn card_entity_name_hides_invisible_canonical() {
        // Alias merged into a canonical that lives only in a private note of
        // another user: the chip must show the alias name, never canonical.
        assert_eq!(
            pick_card_entity_name("Nexadata sp. z o.o.", Some("Nexadata"), false),
            "Nexadata sp. z o.o."
        );
        // Reader can see the canonical through a readable note → canonical.
        assert_eq!(
            pick_card_entity_name("Nexadata sp. z o.o.", Some("Nexadata"), true),
            "Nexadata"
        );
        // Unmerged entity → local name regardless of the flag.
        assert_eq!(pick_card_entity_name("RODO", None, true), "RODO");
    }

    #[test]
    fn entity_visibility_sql_requires_direct_mention_through_readable_note() {
        let c = ctx(&["g1"]);
        let sql = entity_visibility_sql(c.group_ids.len());
        // Direct semantics: the entity row itself must be attached, resolution
        // through an alias must not open visibility.
        assert!(sql.contains("ne.entity_id = ?"));
        assert!(!sql.contains("canonical_id"));
        assert!(sql.contains("n.deleted_at IS NULL"));
        assert!(sql.contains("n.owner_user_id = ?"));
        assert_eq!(sql.matches('?').count(), 1 + acl_params(&c).len());
    }

    #[test]
    fn graph_notes_sql_buckets_and_placeholders_match_params() {
        let c = ctx(&["g1", "g2"]);
        let sql = graph_notes_sql(c.group_ids.len());
        // mine (owner) wins over any share; direct user share over group; org last.
        assert!(sql.contains("THEN 'mine'"));
        assert!(sql.contains("THEN 'shared'"));
        assert!(sql.contains("THEN 'group'"));
        assert!(sql.contains("ELSE 'org'"));
        assert!(sql.contains("n.deleted_at IS NULL"));
        // Placeholders: mine, shared, 2 group ids, then the ACL params.
        assert_eq!(sql.matches('?').count(), 2 + 2 + acl_params(&c).len());

        let no_groups = graph_notes_sql(0);
        assert!(!no_groups.contains("THEN 'group'"));
        assert_eq!(no_groups.matches('?').count(), 2 + acl_params(&ctx(&[])).len());
    }

    #[test]
    fn widest_scope_prefers_org_over_group_over_user() {
        let shares = vec![
            ("user".to_string(), "u2".to_string(), "read".to_string()),
            ("group".to_string(), "g1".to_string(), "read".to_string()),
            ("org".to_string(), "".to_string(), "read".to_string()),
        ];
        assert_eq!(widest_scope(&shares), "org");
        assert_eq!(widest_scope(&shares[..2]), "group");
        assert_eq!(widest_scope(&shares[..1]), "user");
        assert_eq!(widest_scope(&[]), "private");
    }

    // Binds a SqlValue param onto a rusqlite statement (only the variants the
    // card-entities query uses: text ids and integer counts).
    fn bind(v: &SqlValue) -> rusqlite::types::Value {
        match v {
            SqlValue::String(s) => rusqlite::types::Value::Text(s.clone()),
            SqlValue::I64(i) => rusqlite::types::Value::Integer(*i),
            SqlValue::Null => rusqlite::types::Value::Null,
            other => panic!("unexpected SqlValue in card-entities params: {other:?}"),
        }
    }

    // Minimal seed reproducing the live notes-427b306a data: three notes owned
    // by the reader, each with several entity mentions (none merged).
    fn seed_card_entities_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE notes (id TEXT PRIMARY KEY, owner_user_id TEXT, deleted_at INTEGER);
             CREATE TABLE note_shares (note_id TEXT, subject_type TEXT, subject_id TEXT, access TEXT);
             CREATE TABLE entities (id TEXT PRIMARY KEY, name TEXT, entity_type TEXT, canonical_id TEXT);
             CREATE TABLE note_entities (note_id TEXT, entity_id TEXT, count INTEGER);
             INSERT INTO notes VALUES ('n2','u1',NULL),('n6','u1',NULL),('n7','u1',NULL);
             INSERT INTO entities VALUES
               ('e_firma','Firma Sp. z o.o.','company',NULL),
               ('e_marta','Marta Wiśniewska','person',NULL),
               ('e_euvic','Euvic','company',NULL),
               ('e_rnd','zespół R&D','project',NULL);
             INSERT INTO note_entities VALUES
               ('n2','e_firma',1),('n2','e_marta',1),
               ('n6','e_euvic',1),('n6','e_rnd',1),('n6','e_marta',1),
               ('n7','e_firma',1),('n7','e_euvic',1);",
        )
        .unwrap();
        conn
    }

    // Executes card_entities_sql with the given bound params and returns the set
    // of note ids that came back with at least one entity row.
    fn notes_with_entities(
        conn: &rusqlite::Connection,
        sql: &str,
        params: &[SqlValue],
    ) -> std::collections::BTreeSet<String> {
        let bound: Vec<rusqlite::types::Value> = params.iter().map(bind).collect();
        let mut stmt = conn.prepare(sql).unwrap();
        let rows = stmt
            .query_map(rusqlite::params_from_iter(bound.iter()), |r| {
                r.get::<_, String>(0)
            })
            .unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    // Regression for the empty search right-rail: card_entities_sql puts the
    // visibility EXISTS (with its ACL placeholders) in the SELECT list, so those
    // '?' come BEFORE the WHERE `IN (...)` ids. Binding the note ids first shifts
    // them into the ACL owner/user slots and only ONE note survives the IN list
    // — leaving `graph_mentions`/`attach_card_entities` (hence the rail) empty.
    #[test]
    fn card_entities_returns_all_notes_only_with_correct_bind_order() {
        let c = ctx(&[]);
        let conn = seed_card_entities_db();
        let ids = vec!["n2".to_string(), "n6".to_string(), "n7".to_string()];
        let sql = card_entities_sql(ids.len(), c.group_ids.len());

        // Correct order (the fix): acl_params first, then the note ids.
        let good = card_entities_params(&c, &ids);
        let seen = notes_with_entities(&conn, &sql, &good);
        assert_eq!(
            seen,
            ["n2", "n6", "n7"].iter().map(|s| s.to_string()).collect(),
            "every result note must contribute its entities to the rail"
        );

        // The old (buggy) order — note ids first, then acl_params — collapses to
        // a single matching note, which is exactly the empty-rail symptom.
        let mut bad: Vec<SqlValue> = ids.iter().map(|i| SqlValue::String(i.clone())).collect();
        bad.extend(acl_params(&c));
        let seen_bad = notes_with_entities(&conn, &sql, &bad);
        assert!(
            seen_bad.len() < 3,
            "the pre-fix bind order must NOT return all notes (proves the regression)"
        );
    }
}
