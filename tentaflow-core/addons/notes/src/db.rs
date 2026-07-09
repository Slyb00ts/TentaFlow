// =============================================================================
// File: addons/notes/src/db.rs
// Purpose: data access for the Notes addon — note CRUD, tags, shares and the
//          ACL predicate every read/write path goes through. Pure SQL-building
//          helpers are kept side-effect free so they are unit-testable on the
//          native target (host fns exist only under wasm).
// =============================================================================

use serde_json::{json, Value as JsonValue};

use tentaflow_addon_sdk::{
    directory_org, directory_users, sql_exec, sql_query, sql_query_one, sql_transaction, SqlValue,
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

/// Names of the acting user's groups, for the share selector labels.
pub fn group_names(group_ids: &[String]) -> Vec<(String, String)> {
    let groups = match tentaflow_addon_sdk::directory_groups() {
        Ok(g) => g,
        Err(_) => return vec![],
    };
    group_ids
        .iter()
        .filter_map(|gid| {
            groups
                .iter()
                .find(|g| &g.id == gid)
                .map(|g| (gid.clone(), g.name.clone()))
        })
        .collect()
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
    /// Share selector value ("private" | "org:read" | "org:write" | "group:<id>").
    pub share_mode: String,
    pub can_write: bool,
    pub is_owner: bool,
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
            ELSE 'private' END) \
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
    Ok(rows
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
        })
        .collect())
}

/// Loads one note through the read ACL. Ok(None) = absent or not accessible.
pub fn get_note(ctx: &UserCtx, note_id: &str) -> Result<Option<NoteDetail>, String> {
    let acl = acl_read_clause(ctx.group_ids.len());
    let sql = format!(
        "SELECT n.id, n.title, n.content, n.owner_user_id, n.created_at \
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
        scope: widest_scope(&share_rows),
        share_mode: share_mode(&share_rows),
        owner_user_id: owner,
        can_write,
        is_owner,
        tags,
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

/// Share selector value from the raw share rows. The selector drives one share
/// at a time (chunk-2 scope); richer per-user grants land with the sharing UI.
pub fn share_mode(shares: &[(String, String, String)]) -> String {
    if let Some((_, _, access)) = shares.iter().find(|(t, _, _)| t == "org") {
        return format!("org:{access}");
    }
    if let Some((_, gid, _)) = shares.iter().find(|(t, _, _)| t == "group") {
        return format!("group:{gid}");
    }
    "private".to_string()
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

/// Parses the share selector value into the row to insert (None = private).
/// Pure and validated BEFORE any mutation — a malformed value never reaches
/// the database.
pub fn parse_share_mode(
    mode: &str,
    group_ids: &[String],
) -> Result<Option<(String, String, String)>, String> {
    match mode {
        "private" => Ok(None),
        "org:read" => Ok(Some(("org".into(), "".into(), "read".into()))),
        "org:write" => Ok(Some(("org".into(), "".into(), "write".into()))),
        _ => {
            if let Some(gid) = mode.strip_prefix("group:") {
                if gid.is_empty() {
                    return Err("Nieznany tryb udostępniania.".to_string());
                }
                if !group_ids.iter().any(|g| g == gid) {
                    return Err("Nie należysz do wybranej grupy.".to_string());
                }
                Ok(Some(("group".into(), gid.to_string(), "read".into())))
            } else {
                Err(format!("Nieznany tryb udostępniania: {mode}"))
            }
        }
    }
}

/// Sets the single-selector share mode; owner only. Modes:
/// "private" | "org:read" | "org:write" | "group:<group_id>". Validation runs
/// first; the delete + insert pair is one transaction and every statement
/// re-checks ownership/liveness in its WHERE, so a malformed value or a race
/// can never leave the note stripped of its shares.
pub fn set_share_mode(ctx: &UserCtx, note_id: &str, mode: &str) -> Result<(), String> {
    let insert_row = parse_share_mode(mode, &ctx.group_ids)?;

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

    let guard = owner_guard_clause();
    let guard_params = vec![
        SqlValue::String(note_id.to_string()),
        SqlValue::String(ctx.user_id.clone()),
    ];

    // The selector owns exactly the org/group rows; direct per-user shares
    // (future sharing UI) are left untouched.
    let mut delete_params = vec![SqlValue::String(note_id.to_string())];
    delete_params.extend(guard_params.clone());
    let mut stmts: Vec<(String, Vec<SqlValue>)> = vec![(
        format!(
            "DELETE FROM note_shares \
             WHERE note_id = ? AND subject_type IN ('org', 'group') AND {guard}"
        ),
        delete_params,
    )];

    if let Some((subject_type, subject_id, access)) = insert_row {
        let mut params = vec![
            SqlValue::String(note_id.to_string()),
            SqlValue::String(subject_type),
            SqlValue::String(subject_id),
            SqlValue::String(access),
            SqlValue::String(ctx.user_id.clone()),
            SqlValue::I64(now_unix()),
        ];
        params.extend(guard_params);
        stmts.push((
            format!(
                "INSERT INTO note_shares \
                 (note_id, subject_type, subject_id, access, created_by, created_at) \
                 SELECT ?, ?, ?, ?, ?, ? WHERE {guard}"
            ),
            params,
        ));
    }

    run_transaction(&stmts).map_err(|e| format!("Błąd zapisu udostępnienia: {e}"))
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
// Links / entities read side (populated by the auto-graph stage; empty today,
// but the panel reads the real tables so future data shows up unchanged)
// =============================================================================

/// Related notes of `note_id` that the user can read, best first.
pub fn related_notes(ctx: &UserCtx, note_id: &str) -> Result<Vec<JsonValue>, String> {
    let acl = acl_read_clause(ctx.group_ids.len());
    let sql = format!(
        "SELECT n.id, n.title, l.kind, l.weight, l.reason \
         FROM note_links l JOIN notes n ON n.id = l.dst_note_id \
         WHERE l.src_note_id = ? AND n.deleted_at IS NULL AND {acl} \
         ORDER BY l.weight DESC LIMIT 12"
    );
    let mut params = vec![SqlValue::String(note_id.to_string())];
    params.extend(acl_params(ctx));
    let rows = sql_query(&sql, &params).map_err(|e| format!("Błąd odczytu powiązań: {e}"))?;
    Ok(rows
        .iter()
        .map(|r| {
            json!({
                "id": r.first().and_then(|v| v.as_str()).unwrap_or(""),
                "title": r.get(1).and_then(|v| v.as_str()).unwrap_or(""),
                "kind": r.get(2).and_then(|v| v.as_str()).unwrap_or(""),
                "weight": r.get(3).and_then(|v| v.as_i64()).unwrap_or(0),
                "reason": r.get(4).and_then(|v| v.as_str()).unwrap_or(""),
            })
        })
        .collect())
}

/// Detected entities of a note (name + type), canonical entity preferred.
/// Same read ACL + liveness as every other note read path.
pub fn note_entities(ctx: &UserCtx, note_id: &str) -> Result<Vec<(String, String)>, String> {
    let acl = acl_read_clause(ctx.group_ids.len());
    let sql = format!(
        "SELECT COALESCE(c.name, e.name), COALESCE(c.entity_type, e.entity_type) \
         FROM note_entities ne \
         JOIN notes n ON n.id = ne.note_id \
         JOIN entities e ON e.id = ne.entity_id \
         LEFT JOIN entities c ON c.id = e.canonical_id \
         WHERE ne.note_id = ? AND n.deleted_at IS NULL AND {acl} \
         ORDER BY ne.count DESC LIMIT 24"
    );
    let mut params = vec![SqlValue::String(note_id.to_string())];
    params.extend(acl_params(ctx));
    let rows = sql_query(&sql, &params).map_err(|e| format!("Błąd odczytu encji: {e}"))?;
    Ok(rows
        .iter()
        .map(|r| {
            (
                r.first().and_then(|v| v.as_str()).unwrap_or("").to_string(),
                r.get(1).and_then(|v| v.as_str()).unwrap_or("").to_string(),
            )
        })
        .collect())
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
    fn parse_share_mode_validates_before_any_mutation() {
        let groups = vec!["g1".to_string()];
        assert_eq!(parse_share_mode("private", &groups).unwrap(), None);
        assert_eq!(
            parse_share_mode("org:read", &groups).unwrap(),
            Some(("org".into(), "".into(), "read".into()))
        );
        assert_eq!(
            parse_share_mode("org:write", &groups).unwrap(),
            Some(("org".into(), "".into(), "write".into()))
        );
        assert_eq!(
            parse_share_mode("group:g1", &groups).unwrap(),
            Some(("group".into(), "g1".into(), "read".into()))
        );
        // Malformed / unauthorized values are rejected up front.
        assert!(parse_share_mode("group:other", &groups).is_err());
        assert!(parse_share_mode("group:", &groups).is_err());
        assert!(parse_share_mode("owner:hax", &groups).is_err());
        assert!(parse_share_mode("", &groups).is_err());
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
    fn widest_scope_and_share_mode_prefer_org_over_group() {
        let shares = vec![
            ("group".to_string(), "g1".to_string(), "read".to_string()),
            ("org".to_string(), "".to_string(), "write".to_string()),
        ];
        assert_eq!(widest_scope(&shares), "org");
        assert_eq!(share_mode(&shares), "org:write");
        let only_group = vec![("group".to_string(), "g9".to_string(), "read".to_string())];
        assert_eq!(share_mode(&only_group), "group:g9");
        assert_eq!(widest_scope(&[]), "private");
        assert_eq!(share_mode(&[]), "private");
    }
}
