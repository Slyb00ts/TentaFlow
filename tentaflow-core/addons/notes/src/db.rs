// =============================================================================
// File: addons/notes/src/db.rs
// Purpose: data access for the Notes addon — note CRUD, tags, shares and the
//          ACL predicate every read/write path goes through. Pure SQL-building
//          helpers are kept side-effect free so they are unit-testable on the
//          native target (host fns exist only under wasm).
// =============================================================================

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
