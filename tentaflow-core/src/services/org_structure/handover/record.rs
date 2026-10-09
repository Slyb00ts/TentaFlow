//! The record of a handover: what was asked, of whom, to whom and how far each
//! item got. It is written BEFORE the first item moves, so a failure half way
//! leaves a record the retry and the return job can finish from. Node-local
//! (migration 182): it describes rows of stores that are not replicated.

use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value as Json;

use super::{Category, Reason};
use crate::services::org_structure::error::{OrgStructureError as E, Result};
use crate::services::org_structure::validate::{format_date, parse_date};

/// The header of a handover.
#[derive(Debug, Clone)]
pub(crate) struct Header {
    pub id: String,
    pub org_id: String,
    pub user_id: String,
    pub reason: Reason,
    pub project_id: Option<String>,
    pub effective_on: NaiveDate,
    pub return_on: Option<NaiveDate>,
    pub note: String,
    pub created_by: String,
    pub created_at_ms: i64,
}

/// One recorded item.
#[derive(Debug, Clone)]
pub(crate) struct Item {
    pub handover_id: String,
    pub key: String,
    pub category: Category,
    pub project_id: Option<String>,
    pub title: String,
    pub from_user_id: String,
    pub taker_user_id: Option<String>,
    pub status: String,
    pub reason: Option<String>,
    pub detail: Json,
}

pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Writes the header and its items, all `pending`, in one transaction.
pub(crate) fn create(conn: &mut Connection, header: &Header, items: &[Item]) -> Result<()> {
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO org_handovers (id, org_id, user_id, reason, project_id, effective_on, \
            return_on, note, created_by, created_at_ms) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            header.id,
            header.org_id,
            header.user_id,
            header.reason.as_str(),
            header.project_id,
            format_date(header.effective_on),
            header.return_on.map(format_date),
            header.note,
            header.created_by,
            header.created_at_ms,
        ],
    )?;
    for item in items {
        tx.execute(
            "INSERT INTO org_handover_items (id, handover_id, item_key, category, project_id, \
                title, from_user_id, taker_user_id, status, reason, detail, updated_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', NULL, ?9, ?10)",
            params![
                uuid::Uuid::new_v4().to_string(),
                item.handover_id,
                item.key,
                item.category.as_str(),
                item.project_id,
                item.title,
                item.from_user_id,
                item.taker_user_id,
                item.detail.to_string(),
                header.created_at_ms,
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Sets the state of one item. Works on a plain connection and inside the
/// organization's transaction alike (a `Transaction` derefs to a `Connection`).
pub(crate) fn set_status(
    conn: &Connection,
    handover_id: &str,
    key: &str,
    status: &str,
    reason: Option<&str>,
    detail: Option<&Json>,
) -> Result<()> {
    conn.execute(
        "UPDATE org_handover_items SET status = ?1, reason = ?2, \
            detail = COALESCE(?3, detail), updated_at_ms = ?4 \
         WHERE handover_id = ?5 AND item_key = ?6",
        params![
            status,
            reason,
            detail.map(Json::to_string),
            now_ms(),
            handover_id,
            key
        ],
    )?;
    Ok(())
}

/// `set_status` on its own write connection.
pub(crate) fn mark(
    pool: &crate::db::DbPool,
    handover_id: &str,
    key: &str,
    status: &str,
    reason: Option<&str>,
    detail: Option<&Json>,
) -> Result<()> {
    let conn = pool.write().map_err(|e| E::Db(e.to_string()))?;
    set_status(&conn, handover_id, key, status, reason, detail)
}

fn header_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<(Header, String, Option<String>)> {
    let reason: String = r.get(3)?;
    let effective: String = r.get(5)?;
    let return_on: Option<String> = r.get(6)?;
    Ok((
        Header {
            id: r.get(0)?,
            org_id: r.get(1)?,
            user_id: r.get(2)?,
            reason: Reason::parse(&reason).unwrap_or(Reason::Departure),
            project_id: r.get(4)?,
            effective_on: NaiveDate::MIN,
            return_on: None,
            note: r.get(7)?,
            created_by: r.get(8)?,
            created_at_ms: r.get(9)?,
        },
        effective,
        return_on,
    ))
}

fn finish_header(raw: (Header, String, Option<String>)) -> Result<Header> {
    let (mut header, effective, return_on) = raw;
    header.effective_on = parse_date(&effective)?;
    header.return_on = return_on.as_deref().map(parse_date).transpose()?;
    Ok(header)
}

const HEADER_COLS: &str = "id, org_id, user_id, reason, project_id, effective_on, return_on, \
     note, created_by, created_at_ms";

pub(crate) fn header(conn: &Connection, org_id: &str, id: &str) -> Result<Header> {
    let raw = conn
        .query_row(
            &format!("SELECT {HEADER_COLS} FROM org_handovers WHERE org_id = ?1 AND id = ?2"),
            [org_id, id],
            header_from,
        )
        .optional()?
        .ok_or_else(|| E::NotFound {
            entity: "handover",
            id: id.to_string(),
        })?;
    finish_header(raw)
}

fn item_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<Item> {
    let category: String = r.get(2)?;
    let detail: String = r.get(9)?;
    Ok(Item {
        handover_id: r.get(0)?,
        key: r.get(1)?,
        category: Category::parse(&category).unwrap_or(Category::Task),
        project_id: r.get(3)?,
        title: r.get(4)?,
        from_user_id: r.get(5)?,
        taker_user_id: r.get(6)?,
        status: r.get(7)?,
        reason: r.get(8)?,
        detail: serde_json::from_str(&detail).unwrap_or(Json::Null),
    })
}

const ITEM_COLS: &str = "handover_id, item_key, category, project_id, title, from_user_id, \
     taker_user_id, status, reason, detail";

pub(crate) fn items(conn: &Connection, handover_id: &str) -> Result<Vec<Item>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ITEM_COLS} FROM org_handover_items WHERE handover_id = ?1 \
         ORDER BY updated_at_ms, item_key"
    ))?;
    let rows = stmt
        .query_map([handover_id], item_from)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The handovers made for one person, newest first.
pub(crate) fn headers_of(conn: &Connection, org_id: &str, user_id: &str) -> Result<Vec<Header>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {HEADER_COLS} FROM org_handovers WHERE org_id = ?1 AND user_id = ?2 \
         ORDER BY created_at_ms DESC LIMIT 50"
    ))?;
    let raws = stmt
        .query_map([org_id, user_id], header_from)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raws.into_iter().map(finish_header).collect()
}

/// A temporary handover's items that are due to go back: `done`, in an absence
/// handover whose return day has come.
pub(crate) fn due_returns(
    conn: &Connection,
    org_id: &str,
    today: NaiveDate,
) -> Result<Vec<(Header, Item)>> {
    pairs(
        conn,
        org_id,
        "h.reason = 'absence' AND h.return_on <= ?2 AND i.status = 'done'",
        "done",
        today,
    )
}

/// Items scheduled for a day that has come (a membership that ends on the
/// departure date).
pub(crate) fn due_scheduled(
    conn: &Connection,
    org_id: &str,
    today: NaiveDate,
) -> Result<Vec<(Header, Item)>> {
    pairs(
        conn,
        org_id,
        "i.status = 'scheduled' AND h.effective_on <= ?2",
        "scheduled",
        today,
    )
}

fn pairs(
    conn: &Connection,
    org_id: &str,
    predicate: &str,
    wanted: &str,
    today: NaiveDate,
) -> Result<Vec<(Header, Item)>> {
    let handovers: Vec<String> = {
        let mut stmt = conn.prepare(&format!(
            "SELECT DISTINCT h.id FROM org_handovers h \
             JOIN org_handover_items i ON i.handover_id = h.id \
             WHERE h.org_id = ?1 AND {predicate} ORDER BY h.created_at_ms"
        ))?;
        let ids = stmt
            .query_map([org_id, &format_date(today)], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids
    };
    let mut out = Vec::new();
    for id in handovers {
        let head = header(conn, org_id, &id)?;
        for item in items(conn, &id)? {
            if item.status == wanted {
                out.push((head.clone(), item));
            }
        }
    }
    Ok(out)
}

/// Organizations that have a handover, for the background loop.
pub(crate) fn organizations(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT DISTINCT org_id FROM org_handovers ORDER BY org_id")?;
    let ids = stmt
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(ids)
}
