// ============ File: services/role_catalog/repo.rs — warstwa CRUD dla role_catalog ============
//
// Pelny CRUD nad tabela `role_catalog` (migracja v41) z twarda walidacja:
//   * slug w postaci `[a-z][a-z0-9_]*`, max 50 znakow,
//   * komplet translacji `name_translations` wzgledem aktywnych `platform_locales`,
//   * opcjonalne `description_translations` walidowane tylko gdy podane,
//   * ikona z zamknietej listy `ALLOWED_ICONS` (mod.rs),
//   * `color_hint` w formacie `#rrggbb` lub `--<css-var>`.
//
// `update_role` patchuje tylko pola obecne w `RoleUpdateInput` — pola
// `Option<Option<String>>` (icon, color_hint) pozwalaja ustawic wartosc lub
// jawnie wyzerowac kolumne na NULL. Audit log emitowany przez `audit.rs`.

use std::collections::{BTreeMap, BTreeSet};

use regex::Regex;
use rusqlite::{params, OptionalExtension};

use super::audit;
use super::error::{Result, RoleCatalogError};
use super::{PlatformLocale, Role, RoleKind, VisibilityScope, ALLOWED_ICONS};
use crate::db::DbPool;

#[derive(Debug, Clone, Default)]
pub struct RoleListFilter {
    pub kind: Option<RoleKind>,
    /// `None` = wszystkie (aktywne + nieaktywne); `Some(true)` = tylko aktywne.
    pub is_active: Option<bool>,
    pub search: Option<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct RoleCreateInput {
    pub org_id: String,
    pub slug: String,
    pub kind: RoleKind,
    pub name_translations: BTreeMap<String, String>,
    pub description_translations: BTreeMap<String, String>,
    pub icon: Option<String>,
    pub color_hint: Option<String>,
    pub is_manager: bool,
    pub default_visibility_scope: VisibilityScope,
}

#[derive(Debug, Clone, Default)]
pub struct RoleUpdateInput {
    pub kind: Option<RoleKind>,
    pub name_translations: Option<BTreeMap<String, String>>,
    pub description_translations: Option<BTreeMap<String, String>>,
    /// `None` = nie zmieniaj; `Some(None)` = ustaw na NULL; `Some(Some(s))` = ustaw na `s`.
    pub icon: Option<Option<String>>,
    pub color_hint: Option<Option<String>>,
    pub is_manager: Option<bool>,
    pub default_visibility_scope: Option<VisibilityScope>,
}

// -----------------------------------------------------------------------------
// Helpery
// -----------------------------------------------------------------------------

fn map_db<E: std::fmt::Display>(e: E) -> RoleCatalogError {
    RoleCatalogError::DbError(e.to_string())
}

fn now_utc() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn slug_regex() -> Regex {
    // Regex jest tani do skompilowania na zadanie CRUD; nie chcemy uzaleznienia
    // od dodatkowego crate'a once_cell tylko dla tego jednego miejsca.
    Regex::new(r"^[a-z][a-z0-9_]*$").expect("static slug regex must compile")
}

fn color_hint_regex() -> Regex {
    Regex::new(r"^(#[0-9a-fA-F]{6}|--[a-z][a-z0-9-]*)$")
        .expect("static color_hint regex must compile")
}

fn validate_slug(slug: &str) -> Result<()> {
    if slug.is_empty() || slug.len() > 50 || !slug_regex().is_match(slug) {
        return Err(RoleCatalogError::InvalidSlug(slug.to_string()));
    }
    Ok(())
}

fn validate_icon(icon: Option<&str>) -> Result<()> {
    let Some(name) = icon else {
        return Ok(());
    };
    if !ALLOWED_ICONS.contains(&name) {
        return Err(RoleCatalogError::UnknownIcon(name.to_string()));
    }
    Ok(())
}

fn validate_color_hint(color: Option<&str>) -> Result<()> {
    let Some(c) = color else {
        return Ok(());
    };
    if !color_hint_regex().is_match(c) {
        return Err(RoleCatalogError::InvalidColorHint(c.to_string()));
    }
    Ok(())
}

/// Weryfikuje kompletnosc i niepustosc tlumaczen w stosunku do listy
/// `required_locales`. `field` jest nazwa pola podawana w blad
/// `EmptyTranslation`. Mapa moze byc nadzbiorem wymaganych locale (tzn.
/// dodatkowe tlumaczenia sa OK, brakujace nie sa).
fn validate_translations(
    map: &BTreeMap<String, String>,
    required_locales: &[String],
    field: &str,
) -> Result<()> {
    let provided: BTreeSet<&str> = map.keys().map(String::as_str).collect();
    let missing: Vec<String> = required_locales
        .iter()
        .filter(|loc| !provided.contains(loc.as_str()))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(RoleCatalogError::MissingTranslations {
            required: required_locales.to_vec(),
            missing,
        });
    }
    for (locale, value) in map.iter() {
        if value.trim().is_empty() {
            return Err(RoleCatalogError::EmptyTranslation {
                locale: locale.clone(),
                field: field.to_string(),
            });
        }
    }
    Ok(())
}

fn parse_translations(raw: &str) -> Result<BTreeMap<String, String>> {
    if raw.is_empty() {
        return Ok(BTreeMap::new());
    }
    serde_json::from_str::<BTreeMap<String, String>>(raw)
        .map_err(|e| RoleCatalogError::InvalidJson(e.to_string()))
}

fn serialize_translations(map: &BTreeMap<String, String>) -> Result<String> {
    serde_json::to_string(map).map_err(|e| RoleCatalogError::InvalidJson(e.to_string()))
}

fn row_to_role(row: &rusqlite::Row<'_>) -> rusqlite::Result<RoleRow> {
    Ok(RoleRow {
        id: row.get(0)?,
        org_id: row.get(1)?,
        slug: row.get(2)?,
        kind: row.get(3)?,
        name_translations: row.get(4)?,
        description_translations: row.get(5)?,
        icon: row.get(6)?,
        color_hint: row.get(7)?,
        is_manager: row.get::<_, i64>(8)? != 0,
        default_visibility_scope: row.get(9)?,
        is_active: row.get::<_, i64>(10)? != 0,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
        created_by: row.get(13)?,
    })
}

struct RoleRow {
    id: String,
    org_id: String,
    slug: String,
    kind: String,
    name_translations: String,
    description_translations: String,
    icon: Option<String>,
    color_hint: Option<String>,
    is_manager: bool,
    default_visibility_scope: String,
    is_active: bool,
    created_at: String,
    updated_at: String,
    created_by: Option<String>,
}

fn role_from_row(r: RoleRow) -> Result<Role> {
    Ok(Role {
        id: r.id,
        org_id: r.org_id,
        slug: r.slug,
        kind: RoleKind::from_db_str(&r.kind)?,
        name_translations: parse_translations(&r.name_translations)?,
        description_translations: parse_translations(&r.description_translations)?,
        icon: r.icon,
        color_hint: r.color_hint,
        is_manager: r.is_manager,
        default_visibility_scope: VisibilityScope::from_db_str(&r.default_visibility_scope)?,
        is_active: r.is_active,
        created_at: r.created_at,
        updated_at: r.updated_at,
        created_by: r.created_by,
    })
}

const SELECT_COLUMNS: &str = "id, org_id, slug, kind, name_translations, \
     description_translations, icon, color_hint, is_manager, \
     default_visibility_scope, is_active, created_at, updated_at, created_by";

// -----------------------------------------------------------------------------
// Read API
// -----------------------------------------------------------------------------

/// Zwraca aktywne kody locale dla `org_id` (kolumna `code` w `platform_locales`).
/// Zwraca `NoActiveLocales` gdy lista jest pusta — adminowi nie wolno tworzyc
/// rol bez chocby jednego jezyka platformy.
pub fn list_active_locale_codes(pool: &DbPool, org_id: &str) -> Result<Vec<String>> {
    let conn = pool.read().map_err(map_db)?;
    let mut stmt = conn
        .prepare(
            "SELECT code FROM platform_locales \
             WHERE org_id = ?1 AND is_active = 1 \
             ORDER BY is_default DESC, code ASC",
        )
        .map_err(map_db)?;
    let rows = stmt
        .query_map(params![org_id], |row| row.get::<_, String>(0))
        .map_err(map_db)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(map_db)?);
    }
    if out.is_empty() {
        return Err(RoleCatalogError::NoActiveLocales(org_id.to_string()));
    }
    Ok(out)
}

/// Zwraca pelne wpisy `platform_locales` (kod + display_name + is_default) dla
/// aktywnych locale danej organizacji. UI edytora rol uzywa tego do narysowania
/// pickera jezykow oraz dopasowania `is_default` do "domyslnego" inputa.
/// `NoActiveLocales` gdy lista jest pusta — analogicznie do
/// `list_active_locale_codes`.
pub fn list_active_locales(pool: &DbPool, org_id: &str) -> Result<Vec<PlatformLocale>> {
    let conn = pool.read().map_err(map_db)?;
    let mut stmt = conn
        .prepare(
            "SELECT id, org_id, code, display_name, is_default, is_active \
             FROM platform_locales \
             WHERE org_id = ?1 AND is_active = 1 \
             ORDER BY is_default DESC, code ASC",
        )
        .map_err(map_db)?;
    let rows = stmt
        .query_map(params![org_id], |row| {
            Ok(PlatformLocale {
                id: row.get(0)?,
                org_id: row.get(1)?,
                code: row.get(2)?,
                display_name: row.get(3)?,
                is_default: row.get::<_, i64>(4)? != 0,
                is_active: row.get::<_, i64>(5)? != 0,
            })
        })
        .map_err(map_db)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(map_db)?);
    }
    if out.is_empty() {
        return Err(RoleCatalogError::NoActiveLocales(org_id.to_string()));
    }
    Ok(out)
}

/// Pobiera role po `(org_id, id)`. Zwraca `NotFound` gdy brak.
pub fn get_role(pool: &DbPool, org_id: &str, id: &str) -> Result<Role> {
    let conn = pool.read().map_err(map_db)?;
    let sql = format!("SELECT {SELECT_COLUMNS} FROM role_catalog WHERE org_id = ?1 AND id = ?2",);
    let row = conn
        .query_row(&sql, params![org_id, id], row_to_role)
        .optional()
        .map_err(map_db)?;
    match row {
        Some(r) => role_from_row(r),
        None => Err(RoleCatalogError::NotFound(id.to_string())),
    }
}

/// Pobiera role po `(org_id, slug)`. Zwraca `NotFound` gdy brak.
pub fn get_role_by_slug(pool: &DbPool, org_id: &str, slug: &str) -> Result<Role> {
    let conn = pool.read().map_err(map_db)?;
    let sql = format!("SELECT {SELECT_COLUMNS} FROM role_catalog WHERE org_id = ?1 AND slug = ?2",);
    let row = conn
        .query_row(&sql, params![org_id, slug], row_to_role)
        .optional()
        .map_err(map_db)?;
    match row {
        Some(r) => role_from_row(r),
        None => Err(RoleCatalogError::NotFound(slug.to_string())),
    }
}

/// Lista rol dla `org_id` z opcjonalnym filtrem `RoleListFilter`.
///
/// Sortowanie: `slug ASC` jako deterministyczny porzadek (niezalezny od
/// kolejnosci insertow seedu / migracji). `search` wykonuje LIKE po `slug`
/// oraz po surowym JSONie `name_translations` — to uproszczenie wystarcza dla
/// admin UI z paroma setkami rol; dla wiekszych zbiorow czesc tekstowa
/// powinna byc przeniesiona do FTS5 w osobnej iteracji.
pub fn list_roles(pool: &DbPool, org_id: &str, filter: RoleListFilter) -> Result<Vec<Role>> {
    let conn = pool.read().map_err(map_db)?;

    let mut where_sql = String::from("WHERE org_id = ?1");
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(org_id.to_string())];

    if let Some(kind) = filter.kind {
        binds.push(Box::new(kind.as_db_str().to_string()));
        where_sql.push_str(&format!(" AND kind = ?{}", binds.len()));
    }
    if let Some(active) = filter.is_active {
        binds.push(Box::new(if active { 1i64 } else { 0i64 }));
        where_sql.push_str(&format!(" AND is_active = ?{}", binds.len()));
    }
    if let Some(search) = filter.search.as_ref() {
        let pattern = format!("%{}%", search.trim());
        binds.push(Box::new(pattern));
        let n = binds.len();
        where_sql.push_str(&format!(
            " AND (slug LIKE ?{n} OR name_translations LIKE ?{n})",
            n = n
        ));
    }

    let mut sql =
        format!("SELECT {SELECT_COLUMNS} FROM role_catalog {where_sql} ORDER BY slug ASC",);
    if let Some(limit) = filter.limit {
        sql.push_str(&format!(" LIMIT {}", limit));
        if let Some(offset) = filter.offset {
            sql.push_str(&format!(" OFFSET {}", offset));
        }
    }

    let mut stmt = conn.prepare(&sql).map_err(map_db)?;
    let params_dyn: Vec<&dyn rusqlite::ToSql> = binds
        .iter()
        .map(|b| b.as_ref() as &dyn rusqlite::ToSql)
        .collect();
    let rows = stmt
        .query_map(
            rusqlite::params_from_iter(params_dyn.iter().copied()),
            row_to_role,
        )
        .map_err(map_db)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(role_from_row(r.map_err(map_db)?)?);
    }
    Ok(out)
}

/// Wyszukiwanie pelnotekstowe po slug + name_translations (LIKE). Zwraca
/// najwyzej `limit` wynikow z `is_active = 1`, sortowane po slug.
pub fn search_roles(pool: &DbPool, org_id: &str, query: &str, limit: usize) -> Result<Vec<Role>> {
    let filter = RoleListFilter {
        is_active: Some(true),
        search: Some(query.to_string()),
        limit: Some(limit),
        ..Default::default()
    };
    list_roles(pool, org_id, filter)
}

// -----------------------------------------------------------------------------
// Write API
// -----------------------------------------------------------------------------

/// Tworzy nowa role w katalogu.
///
/// Bledy:
///   * `InvalidSlug` — slug nie pasuje do regex / przekracza 50 znakow,
///   * `SlugConflict` — slug juz istnieje w tej organizacji,
///   * `NoActiveLocales` — `platform_locales` nie ma aktywnych wpisow dla `org_id`,
///   * `MissingTranslations` / `EmptyTranslation` — niekompletne name_translations
///     lub niepelne description_translations (jesli podane),
///   * `UnknownIcon` / `InvalidColorHint` — wartosci spoza dozwolonych wzorcow.
pub fn create_role(pool: &DbPool, actor_user_id: &str, input: RoleCreateInput) -> Result<Role> {
    validate_slug(&input.slug)?;

    let locales = list_active_locale_codes(pool, &input.org_id)?;
    validate_translations(&input.name_translations, &locales, "name")?;
    if !input.description_translations.is_empty() {
        validate_translations(&input.description_translations, &locales, "description")?;
    }
    validate_icon(input.icon.as_deref())?;
    validate_color_hint(input.color_hint.as_deref())?;

    let conn = pool.write().map_err(map_db)?;

    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM role_catalog WHERE org_id = ?1 AND slug = ?2",
            params![&input.org_id, &input.slug],
            |r| r.get(0),
        )
        .optional()
        .map_err(map_db)?;
    if exists.is_some() {
        return Err(RoleCatalogError::SlugConflict {
            org_id: input.org_id.clone(),
            slug: input.slug.clone(),
        });
    }

    let id = uuid::Uuid::new_v4().to_string();
    let now = now_utc();
    let name_json = serialize_translations(&input.name_translations)?;
    let desc_json = serialize_translations(&input.description_translations)?;

    conn.execute(
        "INSERT INTO role_catalog \
            (id, org_id, slug, kind, name_translations, description_translations, \
             icon, color_hint, is_manager, default_visibility_scope, is_active, \
             created_at, updated_at, created_by) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, ?11, ?11, ?12)",
        params![
            id,
            input.org_id,
            input.slug,
            input.kind.as_db_str(),
            name_json,
            desc_json,
            input.icon,
            input.color_hint,
            if input.is_manager { 1i64 } else { 0i64 },
            input.default_visibility_scope.as_db_str(),
            now,
            actor_user_id,
        ],
    )
    .map_err(map_db)?;

    drop(conn);

    let role = get_role(pool, &input.org_id, &id)?;
    audit::emit_created(pool, actor_user_id, &input.org_id, &role)?;
    Ok(role)
}

/// Patchuje pola roli wedlug `RoleUpdateInput`. Przy zmianie translacji
/// rewaliduje komplet wzgledem `platform_locales`. Pola `Option<Option<String>>`
/// pozwalaja jawnie wyzerowac kolumne (Some(None) -> NULL).
pub fn update_role(
    pool: &DbPool,
    actor_user_id: &str,
    org_id: &str,
    id: &str,
    patch: RoleUpdateInput,
) -> Result<Role> {
    let before = get_role(pool, org_id, id)?;

    let translations_changed =
        patch.name_translations.is_some() || patch.description_translations.is_some();
    if translations_changed {
        let locales = list_active_locale_codes(pool, org_id)?;
        let name_map = patch
            .name_translations
            .as_ref()
            .unwrap_or(&before.name_translations);
        validate_translations(name_map, &locales, "name")?;
        let desc_map = patch
            .description_translations
            .as_ref()
            .unwrap_or(&before.description_translations);
        if !desc_map.is_empty() {
            validate_translations(desc_map, &locales, "description")?;
        }
    }

    if let Some(icon_patch) = patch.icon.as_ref() {
        validate_icon(icon_patch.as_deref())?;
    }
    if let Some(color_patch) = patch.color_hint.as_ref() {
        validate_color_hint(color_patch.as_deref())?;
    }

    let mut sets: Vec<String> = Vec::new();
    let mut binds: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    if let Some(kind) = patch.kind {
        binds.push(Box::new(kind.as_db_str().to_string()));
        sets.push(format!("kind = ?{}", binds.len()));
    }
    if let Some(name_map) = patch.name_translations.as_ref() {
        let json = serialize_translations(name_map)?;
        binds.push(Box::new(json));
        sets.push(format!("name_translations = ?{}", binds.len()));
    }
    if let Some(desc_map) = patch.description_translations.as_ref() {
        let json = serialize_translations(desc_map)?;
        binds.push(Box::new(json));
        sets.push(format!("description_translations = ?{}", binds.len()));
    }
    if let Some(icon_patch) = patch.icon.as_ref() {
        binds.push(Box::new(icon_patch.clone()));
        sets.push(format!("icon = ?{}", binds.len()));
    }
    if let Some(color_patch) = patch.color_hint.as_ref() {
        binds.push(Box::new(color_patch.clone()));
        sets.push(format!("color_hint = ?{}", binds.len()));
    }
    if let Some(is_manager) = patch.is_manager {
        binds.push(Box::new(if is_manager { 1i64 } else { 0i64 }));
        sets.push(format!("is_manager = ?{}", binds.len()));
    }
    if let Some(scope) = patch.default_visibility_scope {
        binds.push(Box::new(scope.as_db_str().to_string()));
        sets.push(format!("default_visibility_scope = ?{}", binds.len()));
    }

    if sets.is_empty() {
        // No-op patch: nadal emitujemy audit `updated` z identycznym before/after
        // tylko gdyby cokolwiek sie zmienilo. Tutaj zwracamy `before` bez I/O.
        return Ok(before);
    }

    let now = now_utc();
    binds.push(Box::new(now));
    sets.push(format!("updated_at = ?{}", binds.len()));

    let n_org = binds.len() + 1;
    let n_id = binds.len() + 2;
    binds.push(Box::new(org_id.to_string()));
    binds.push(Box::new(id.to_string()));

    let sql = format!(
        "UPDATE role_catalog SET {} WHERE org_id = ?{} AND id = ?{}",
        sets.join(", "),
        n_org,
        n_id
    );

    {
        let conn = pool.write().map_err(map_db)?;
        let params_dyn: Vec<&dyn rusqlite::ToSql> = binds
            .iter()
            .map(|b| b.as_ref() as &dyn rusqlite::ToSql)
            .collect();
        let affected = conn
            .execute(&sql, rusqlite::params_from_iter(params_dyn.iter().copied()))
            .map_err(map_db)?;
        if affected == 0 {
            return Err(RoleCatalogError::NotFound(id.to_string()));
        }
    }

    let after = get_role(pool, org_id, id)?;
    audit::emit_updated(pool, actor_user_id, org_id, id, &before, &after)?;
    Ok(after)
}

/// Soft-delete: ustawia `is_active = 0`. Drugi deactivate na tym samym
/// wpisie zwraca `NotFound` (nie aktualizuje juz nieaktywnego rekordu).
pub fn deactivate_role(pool: &DbPool, actor_user_id: &str, org_id: &str, id: &str) -> Result<()> {
    let now = now_utc();
    let affected = {
        let conn = pool.write().map_err(map_db)?;
        conn.execute(
            "UPDATE role_catalog SET is_active = 0, updated_at = ?1 \
             WHERE org_id = ?2 AND id = ?3 AND is_active = 1",
            params![now, org_id, id],
        )
        .map_err(map_db)?
    };
    if affected == 0 {
        return Err(RoleCatalogError::NotFound(id.to_string()));
    }
    audit::emit_deactivated(pool, actor_user_id, org_id, id)?;
    Ok(())
}
