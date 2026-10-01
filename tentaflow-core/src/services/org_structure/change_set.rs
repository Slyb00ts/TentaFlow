//! Planned reorganizations (docs/ORG_STRUCTURE_PLAN.md §2.4): a named set of
//! dated operations that waits for ONE approval by an administrator who is not
//! its author, then takes effect on its day.
//!
//! This layer owns the document and its state machine:
//!
//! ```text
//!   draft --submit--> pending --approve--> applied
//!     |                  |
//!     +-----withdraw-----+--> withdrawn
//! ```
//!
//! Saving a `draft` or a `pending` set puts it back to `draft` and makes the
//! saver its author: whoever last changed the content may not approve it, so the
//! approval is always somebody else's reading of it. `applied` and `withdrawn`
//! are final; an applied reorganization is in the structure as dated rows and
//! is undone with ordinary writes, not by a state change.
//!
//! The operations are opaque here (a JSON array in `payload`): they are wire
//! requests the dispatcher converts and runs through the batch machinery. That
//! is also why approval is split in two: `check_approvable` refuses early, with
//! a typed error, and `mark_applied_in` records `applied` INSIDE the transaction
//! of the batch (`Batch::with_transaction`), so the operations and the state
//! change commit together or not at all — a crash cannot leave a structure that
//! changed under a reorganization that still reads `pending`.
//!
//! Every write is captured for replication (the table is one of the nine the
//! structure replicates) and audited with the organization id, which is how the
//! history finds it.

use std::collections::{BTreeMap, BTreeSet};

use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};
use thiserror::Error;
use uuid::Uuid;

use super::error::OrgStructureError;
use super::replication as repl;
use super::repo::timezone_of;
use super::validate;
use crate::db::DbPool;
use crate::sync::runtime::SqlWriteAction;

/// The longest payload one change set stores; the request that carries it is
/// one frame of the socket, so this is only a bound on what is written.
pub const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Draft,
    Pending,
    Approved,
    Withdrawn,
    Applied,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Withdrawn => "withdrawn",
            Self::Applied => "applied",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Some(match raw {
            "draft" => Self::Draft,
            "pending" => Self::Pending,
            "approved" => Self::Approved,
            "withdrawn" => Self::Withdrawn,
            "applied" => Self::Applied,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeSet {
    pub id: String,
    pub name: String,
    pub effective_date: NaiveDate,
    pub state: State,
    pub author_user_id: String,
    pub author_name: Option<String>,
    pub approver_user_id: Option<String>,
    pub approver_name: Option<String>,
    pub created_at_ms: i64,
    pub op_count: usize,
    /// The JSON array of operations; empty in a listing.
    pub payload: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ChangeSetError {
    #[error("change set not found: {0}")]
    NotFound(String),

    #[error("change set {id} is {state}; it cannot be {action}")]
    State {
        id: String,
        state: &'static str,
        action: &'static str,
    },

    #[error("the author of a reorganization cannot approve it")]
    SelfApproval,

    #[error("the reorganization takes effect {date}, which is before today ({today})")]
    EffectiveDatePassed { date: String, today: String },

    /// Submitting needs operations that pass a dry run.
    #[error("{failed} operation(s) of the reorganization do not pass a dry run")]
    Invalid { failed: usize },

    /// The live structure no longer accepts the operations.
    #[error("{failed} operation(s) of the reorganization conflict with the current structure")]
    Conflict { failed: usize },

    /// The day has come: its rows are part of the structure like any other.
    #[error("the reorganization took effect {date}; it is changed with ordinary edits")]
    Started { date: String },

    /// Something written after the approval leans on what the reorganization made.
    #[error("later changes depend on the reorganization: {what}")]
    Dependents { what: String },

    /// Approved before the approval recorded what it changed.
    #[error("the reorganization recorded nothing to take back")]
    NoUndo,

    #[error(transparent)]
    Org(#[from] OrgStructureError),
}

pub type Result<T> = std::result::Result<T, ChangeSetError>;

impl ChangeSetError {
    /// Stable snake_case identifier the screen keys its message on.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "not_found",
            Self::State { .. } => "change_set_state",
            Self::SelfApproval => "self_approval",
            Self::EffectiveDatePassed { .. } => "effective_date_passed",
            Self::Invalid { .. } => "change_set_invalid",
            Self::Conflict { .. } => "change_set_conflict",
            Self::Started { .. } => "change_set_started",
            Self::Dependents { .. } => "change_set_dependents",
            Self::NoUndo => "change_set_no_undo",
            Self::Org(e) => e.code(),
        }
    }
}

impl From<rusqlite::Error> for ChangeSetError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Org(e.into())
    }
}

impl From<anyhow::Error> for ChangeSetError {
    fn from(e: anyhow::Error) -> Self {
        Self::Org(e.into())
    }
}

const COLUMNS: &str = "cs.id, cs.name, cs.effective_date, cs.state, cs.author_user_id, \
    COALESCE(NULLIF(au.display_name, ''), au.username), cs.approver_user_id, \
    COALESCE(NULLIF(ap.display_name, ''), ap.username), cs.created_at_ms, \
    CASE WHEN json_type(cs.payload) = 'array' THEN json_array_length(cs.payload) \
         ELSE json_array_length(cs.payload, '$.ops') END";

const FROM: &str = "FROM org_change_sets cs \
    LEFT JOIN user_accounts au ON au.id = cs.author_user_id \
    LEFT JOIN user_accounts ap ON ap.id = cs.approver_user_id";

fn read_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChangeSet> {
    let bad = |what: &str, raw: String| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            format!("{what} '{raw}'").into(),
        )
    };
    let day: String = r.get(2)?;
    let state: String = r.get(3)?;
    Ok(ChangeSet {
        id: r.get(0)?,
        name: r.get(1)?,
        effective_date: validate::parse_date(&day).map_err(|_| bad("date", day))?,
        state: State::parse(&state).ok_or_else(|| bad("state", state))?,
        author_user_id: r.get(4)?,
        author_name: r.get(5)?,
        approver_user_id: r.get(6)?,
        approver_name: r.get(7)?,
        created_at_ms: r.get(8)?,
        op_count: r.get::<_, Option<i64>>(9)?.unwrap_or(0).max(0) as usize,
        payload: String::new(),
    })
}

/// Every reorganization of the organization, the ones that take effect last
/// first; without their operations.
pub fn list(pool: &DbPool, org_id: &str) -> Result<Vec<ChangeSet>> {
    let conn = pool
        .read()
        .map_err(|e| OrgStructureError::Db(e.to_string()))?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} {FROM} WHERE cs.org_id = ?1 \
         ORDER BY cs.effective_date DESC, cs.created_at_ms DESC"
    ))?;
    let rows = stmt
        .query_map([org_id], read_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn get_in(conn: &Connection, org_id: &str, id: &str) -> Result<ChangeSet> {
    let mut found = conn
        .query_row(
            &format!("SELECT {COLUMNS} {FROM} WHERE cs.org_id = ?1 AND cs.id = ?2"),
            [org_id, id],
            read_row,
        )
        .optional()?
        .ok_or_else(|| ChangeSetError::NotFound(id.to_string()))?;
    found.payload = conn.query_row(
        "SELECT payload FROM org_change_sets WHERE org_id = ?1 AND id = ?2",
        [org_id, id],
        |r| r.get(0),
    )?;
    Ok(found)
}

/// One reorganization with its operations.
pub fn get(pool: &DbPool, org_id: &str, id: &str) -> Result<ChangeSet> {
    let conn = pool
        .read()
        .map_err(|e| OrgStructureError::Db(e.to_string()))?;
    get_in(&conn, org_id, id)
}

fn write<T>(pool: &DbPool, body: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
    let mut conn = pool
        .write()
        .map_err(|e| OrgStructureError::Db(e.to_string()))?;
    let tx = conn.transaction()?;
    let value = body(&tx)?;
    tx.commit()?;
    Ok(value)
}

fn audit(
    tx: &Transaction<'_>,
    org_id: &str,
    actor: &str,
    action: &str,
    set: &ChangeSet,
    extra: serde_json::Value,
) -> Result<()> {
    let mut details = json!({
        "org_id": org_id,
        "name": set.name,
        "effective_date": validate::format_date(set.effective_date),
        "ops": set.op_count,
    });
    if let (Some(map), Some(more)) = (details.as_object_mut(), extra.as_object()) {
        map.extend(more.clone());
    }
    crate::db::repository::log_audit_tx(
        tx,
        Some(actor),
        None,
        action,
        Some(&format!("org_change_set:{}", set.id)),
        Some(&details.to_string()),
        None,
        None,
    )?;
    Ok(())
}

fn capture(
    tx: &Transaction<'_>,
    org_id: &str,
    actor: &str,
    id: &str,
    action: SqlWriteAction,
) -> Result<()> {
    repl::capture_row(tx, &repl::CHANGE_SETS, org_id, id, action, Some(actor))?;
    Ok(())
}

fn today_in(conn: &Connection, org_id: &str) -> Result<NaiveDate> {
    Ok(validate::today_in_zone(&timezone_of(conn, org_id)?)?)
}

fn state_error(set: &ChangeSet, action: &'static str) -> ChangeSetError {
    ChangeSetError::State {
        id: set.id.clone(),
        state: set.state.as_str(),
        action,
    }
}

fn check_payload(payload: &str) -> Result<usize> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(OrgStructureError::InvalidValue {
            field: "ops",
            reason: format!("the operations exceed {MAX_PAYLOAD_BYTES} bytes"),
        }
        .into());
    }
    match serde_json::from_str::<serde_json::Value>(payload) {
        Ok(serde_json::Value::Array(items)) => Ok(items.len()),
        _ => Err(OrgStructureError::InvalidValue {
            field: "ops",
            reason: "the operations are not a list".to_string(),
        }
        .into()),
    }
}

/// Creates a draft (`id` `None`) or replaces the content of a draft or pending
/// one; either way the result is a `draft` whose author is `actor`.
pub fn save(
    pool: &DbPool,
    org_id: &str,
    actor: &str,
    id: Option<&str>,
    name: &str,
    effective_date: NaiveDate,
    payload: &str,
) -> Result<ChangeSet> {
    let name = validate::require_non_empty("name", name)?;
    if name.chars().count() > validate::MAX_NAME_CHARS {
        return Err(OrgStructureError::InvalidValue {
            field: "name",
            reason: format!("longer than {} characters", validate::MAX_NAME_CHARS),
        }
        .into());
    }
    let op_count = check_payload(payload)?;
    let day = validate::format_date(effective_date);
    write(pool, |tx| {
        let (saved_id, action, db_action) = match id {
            Some(existing) => {
                let current = get_in(tx, org_id, existing)?;
                if !matches!(current.state, State::Draft | State::Pending) {
                    return Err(state_error(&current, "edited"));
                }
                tx.execute(
                    "UPDATE org_change_sets SET name = ?3, effective_date = ?4, state = 'draft', \
                     author_user_id = ?5, approver_user_id = NULL, payload = ?6 \
                     WHERE org_id = ?1 AND id = ?2",
                    params![org_id, existing, name, day, actor, payload],
                )?;
                (
                    existing.to_string(),
                    "org.change_set.update",
                    SqlWriteAction::Update,
                )
            }
            None => {
                let new_id = Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO org_change_sets (id, org_id, name, effective_date, state, \
                     author_user_id, approver_user_id, created_at_ms, payload) \
                     VALUES (?1, ?2, ?3, ?4, 'draft', ?5, NULL, ?6, ?7)",
                    params![
                        new_id,
                        org_id,
                        name,
                        day,
                        actor,
                        chrono::Utc::now().timestamp_millis(),
                        payload
                    ],
                )?;
                (new_id, "org.change_set.create", SqlWriteAction::Insert)
            }
        };
        capture(tx, org_id, actor, &saved_id, db_action)?;
        let mut saved = get_in(tx, org_id, &saved_id)?;
        saved.op_count = op_count;
        audit(tx, org_id, actor, action, &saved, json!({}))?;
        Ok(saved)
    })
}

/// draft -> pending. The day must still be ahead (today counts): a plan that
/// already took effect would rewrite history when approved.
pub fn submit(pool: &DbPool, org_id: &str, actor: &str, id: &str) -> Result<ChangeSet> {
    write(pool, |tx| {
        let current = get_in(tx, org_id, id)?;
        if current.state != State::Draft {
            return Err(state_error(&current, "submitted"));
        }
        ensure_not_past(tx, org_id, &current)?;
        tx.execute(
            "UPDATE org_change_sets SET state = 'pending' \
             WHERE org_id = ?1 AND id = ?2 AND state = 'draft'",
            params![org_id, id],
        )?;
        capture(tx, org_id, actor, id, SqlWriteAction::Update)?;
        let submitted = get_in(tx, org_id, id)?;
        audit(
            tx,
            org_id,
            actor,
            "org.change_set.submit",
            &submitted,
            json!({}),
        )?;
        Ok(submitted)
    })
}

/// draft or pending -> withdrawn: only a document, the live structure is not
/// touched. An APPROVED one is withdrawn until its day comes: every row it made
/// starts on that day, so taking them back (and giving back the end dates it
/// moved) leaves a structure in which the plan never existed on any day. Refused
/// when something written since leans on what it made.
pub fn withdraw(pool: &DbPool, org_id: &str, actor: &str, id: &str) -> Result<ChangeSet> {
    write(pool, |tx| {
        let current = get_in(tx, org_id, id)?;
        let mut extra = json!({});
        match current.state {
            State::Draft | State::Pending => {}
            State::Applied => {
                let today = today_in(tx, org_id)?;
                if today >= current.effective_date {
                    return Err(ChangeSetError::Started {
                        date: validate::format_date(current.effective_date),
                    });
                }
                let undo = undo_of(&current.payload)?.ok_or(ChangeSetError::NoUndo)?;
                extra = json!({ "reverted": undo.counts() });
                take_back_in(tx, org_id, actor, &undo)?;
                // Only the operations stay: the record is spent.
                tx.execute(
                    "UPDATE org_change_sets SET payload = ?3 WHERE org_id = ?1 AND id = ?2",
                    params![org_id, id, ops_of(&current.payload)],
                )?;
            }
            _ => return Err(state_error(&current, "withdrawn")),
        }
        tx.execute(
            "UPDATE org_change_sets SET state = 'withdrawn' \
             WHERE org_id = ?1 AND id = ?2 AND state IN ('draft','pending','applied')",
            params![org_id, id],
        )?;
        capture(tx, org_id, actor, id, SqlWriteAction::Update)?;
        let withdrawn = get_in(tx, org_id, id)?;
        audit(
            tx,
            org_id,
            actor,
            "org.change_set.withdraw",
            &withdrawn,
            extra,
        )?;
        Ok(withdrawn)
    })
}

// ---------------------------------------------------------------------------
// What an approval changed, so that it can be taken back
// ---------------------------------------------------------------------------

type Row = BTreeMap<String, Json>;
/// table -> primary key -> row
pub type Snapshot = BTreeMap<String, BTreeMap<String, Row>>;

fn tables() -> [&'static repl::TableSpec; 7] {
    [
        &repl::UNIT_TYPES,
        &repl::UNITS,
        &repl::POSITIONS,
        &repl::DEPUTY_HEADS,
        &repl::REPORTING_LINES,
        &repl::EXTERNAL_PERSONS,
        &repl::ASSIGNMENTS,
    ]
}

/// The identity a row stands for: what other rows point at.
fn identity_of(table: &str, pk: &str, row: &Row) -> String {
    let column = match table {
        "org_units" => "unit_id",
        "org_positions" => "position_id",
        _ => return pk.to_string(),
    };
    row.get(column)
        .and_then(Json::as_str)
        .unwrap_or(pk)
        .to_string()
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct UpdatedRow {
    pub before: Row,
    pub after: Row,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct TableUndo {
    /// Rows the operations made (their content, to tell whether it changed since).
    pub inserted: Vec<Row>,
    /// Rows they changed (an end date moved): as they were and as they became.
    pub updated: Vec<UpdatedRow>,
    /// Rows they removed.
    pub deleted: Vec<Row>,
}

/// Everything the operations of one approval did to the structure's tables.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct Undo {
    pub tables: BTreeMap<String, TableUndo>,
    /// Things that did not exist before (a unit, a position, ...): what a later
    /// row must not point at when the plan is taken back.
    pub created: Vec<String>,
}

impl Undo {
    fn counts(&self) -> Json {
        let sum = |f: fn(&TableUndo) -> usize| self.tables.values().map(f).sum::<usize>();
        json!({
            "removed": sum(|t| t.inserted.len()),
            "restored": sum(|t| t.updated.len() + t.deleted.len()),
        })
    }
}

fn to_json(value: rusqlite::types::ValueRef<'_>) -> Json {
    use rusqlite::types::ValueRef as V;
    match value {
        V::Integer(i) => json!(i),
        V::Real(f) => json!(f),
        V::Text(t) => Json::String(String::from_utf8_lossy(t).into_owned()),
        V::Null | V::Blob(_) => Json::Null,
    }
}

fn to_sql(value: &Json) -> rusqlite::types::Value {
    use rusqlite::types::Value as V;
    match value {
        Json::Number(n) => n
            .as_i64()
            .map_or_else(|| V::Real(n.as_f64().unwrap_or(0.0)), V::Integer),
        Json::String(s) => V::Text(s.clone()),
        Json::Bool(b) => V::Integer(i64::from(*b)),
        _ => V::Null,
    }
}

/// The structure's rows of the organization, as they are in this transaction.
pub fn snapshot_in(tx: &Transaction<'_>, org_id: &str) -> Result<Snapshot> {
    let mut out = Snapshot::new();
    for spec in tables() {
        let mut stmt = tx.prepare(&format!(
            "SELECT {} FROM {} WHERE org_id = ?1",
            spec.columns.join(", "),
            spec.table
        ))?;
        let rows = stmt.query_map([org_id], |r| {
            let mut row = Row::new();
            for (index, column) in spec.columns.iter().enumerate() {
                row.insert((*column).to_string(), to_json(r.get_ref(index)?));
            }
            Ok(row)
        })?;
        let table = out.entry(spec.table.to_string()).or_default();
        for row in rows {
            let row = row?;
            let pk = row
                .get(spec.pk)
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string();
            table.insert(pk, row);
        }
    }
    Ok(out)
}

/// What the operations between two snapshots did.
pub fn undo_between(before: &Snapshot, after: &Snapshot) -> Undo {
    let mut undo = Undo::default();
    let empty = BTreeMap::new();
    let known: BTreeSet<String> = before
        .iter()
        .flat_map(|(table, rows)| rows.iter().map(|(pk, row)| identity_of(table, pk, row)))
        .collect();
    for spec in tables() {
        let (old, new) = (
            before.get(spec.table).unwrap_or(&empty),
            after.get(spec.table).unwrap_or(&empty),
        );
        let mut t = TableUndo::default();
        for (pk, row) in new {
            match old.get(pk) {
                None => {
                    let identity = identity_of(spec.table, pk, row);
                    if !known.contains(&identity) {
                        undo.created.push(identity);
                    }
                    t.inserted.push(row.clone());
                }
                Some(was) if was != row => t.updated.push(UpdatedRow {
                    before: was.clone(),
                    after: row.clone(),
                }),
                Some(_) => {}
            }
        }
        for (pk, row) in old {
            if !new.contains_key(pk) {
                t.deleted.push(row.clone());
            }
        }
        if !(t.inserted.is_empty() && t.updated.is_empty() && t.deleted.is_empty()) {
            undo.tables.insert(spec.table.to_string(), t);
        }
    }
    undo.created.sort();
    undo.created.dedup();
    undo
}

fn pk_of(spec: &repl::TableSpec, row: &Row) -> String {
    row.get(spec.pk)
        .and_then(Json::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Checks that nothing written since the approval touched what it made or
/// changed and that no other row points at what it created.
fn ensure_undoable(current: &Snapshot, undo: &Undo) -> Result<()> {
    let empty = BTreeMap::new();
    let created: BTreeSet<&str> = undo.created.iter().map(String::as_str).collect();
    for spec in tables() {
        let now = current.get(spec.table).unwrap_or(&empty);
        let planned = undo.tables.get(spec.table);
        let mut touched = BTreeSet::new();
        if let Some(t) = planned {
            let changed = |what: String| ChangeSetError::Dependents { what };
            for row in &t.inserted {
                let pk = pk_of(spec, row);
                if now.get(&pk) != Some(row) {
                    return Err(changed(format!("{} {pk} was changed", spec.table)));
                }
                touched.insert(pk);
            }
            for u in &t.updated {
                let pk = pk_of(spec, &u.after);
                if now.get(&pk) != Some(&u.after) {
                    return Err(changed(format!("{} {pk} was changed", spec.table)));
                }
                touched.insert(pk);
            }
            for row in &t.deleted {
                let pk = pk_of(spec, row);
                if now.contains_key(&pk) {
                    return Err(changed(format!("{} {pk} exists again", spec.table)));
                }
            }
        }
        for (pk, row) in now {
            if touched.contains(pk) {
                continue;
            }
            for (column, value) in row {
                if column != "org_id" && value.as_str().is_some_and(|v| created.contains(v)) {
                    return Err(ChangeSetError::Dependents {
                        what: format!("{} {pk} points at it ({column})", spec.table),
                    });
                }
            }
        }
    }
    Ok(())
}

fn take_back_in(tx: &Transaction<'_>, org_id: &str, actor: &str, undo: &Undo) -> Result<()> {
    ensure_undoable(&snapshot_in(tx, org_id)?, undo)?;
    for spec in tables() {
        let Some(t) = undo.tables.get(spec.table) else {
            continue;
        };
        for row in &t.inserted {
            let pk = pk_of(spec, row);
            tx.execute(
                &format!(
                    "DELETE FROM {} WHERE {} = ?1 AND org_id = ?2",
                    spec.table, spec.pk
                ),
                params![pk, org_id],
            )?;
            repl::capture_row(tx, spec, org_id, &pk, SqlWriteAction::Delete, Some(actor))?;
        }
        for u in &t.updated {
            let pk = pk_of(spec, &u.before);
            let columns: Vec<&&str> = spec.columns.iter().filter(|c| **c != spec.pk).collect();
            let sets: Vec<String> = columns
                .iter()
                .enumerate()
                .map(|(n, c)| format!("{c} = ?{}", n + 1))
                .collect();
            let mut values: Vec<rusqlite::types::Value> = columns
                .iter()
                .map(|c| {
                    u.before
                        .get(**c)
                        .map_or(rusqlite::types::Value::Null, to_sql)
                })
                .collect();
            values.push(rusqlite::types::Value::Text(pk.clone()));
            tx.execute(
                &format!(
                    "UPDATE {} SET {} WHERE {} = ?{}",
                    spec.table,
                    sets.join(", "),
                    spec.pk,
                    columns.len() + 1
                ),
                rusqlite::params_from_iter(values),
            )?;
            repl::capture_row(tx, spec, org_id, &pk, SqlWriteAction::Update, Some(actor))?;
        }
        for row in &t.deleted {
            let placeholders: Vec<String> =
                (1..=spec.columns.len()).map(|n| format!("?{n}")).collect();
            let values: Vec<rusqlite::types::Value> = spec
                .columns
                .iter()
                .map(|c| row.get(*c).map_or(rusqlite::types::Value::Null, to_sql))
                .collect();
            tx.execute(
                &format!(
                    "INSERT INTO {} ({}) VALUES ({})",
                    spec.table,
                    spec.columns.join(", "),
                    placeholders.join(", ")
                ),
                rusqlite::params_from_iter(values),
            )?;
            let pk = pk_of(spec, row);
            repl::capture_row(tx, spec, org_id, &pk, SqlWriteAction::Insert, Some(actor))?;
        }
    }
    Ok(())
}

/// The operations of a stored payload: a plain array until the approval, an
/// object with the operations and the record of what they changed after it.
pub fn ops_of(payload: &str) -> String {
    match serde_json::from_str::<Json>(payload) {
        Ok(Json::Object(mut map)) => map
            .remove("ops")
            .map_or_else(|| "[]".to_string(), |ops| ops.to_string()),
        _ => payload.to_string(),
    }
}

fn undo_of(payload: &str) -> Result<Option<Undo>> {
    let Ok(Json::Object(mut map)) = serde_json::from_str::<Json>(payload) else {
        return Ok(None);
    };
    map.remove("undo")
        .map(|undo| {
            serde_json::from_value(undo).map_err(|e| {
                ChangeSetError::Org(OrgStructureError::Db(format!(
                    "unreadable undo record: {e}"
                )))
            })
        })
        .transpose()
}

fn ensure_not_past(conn: &Connection, org_id: &str, set: &ChangeSet) -> Result<()> {
    let today = today_in(conn, org_id)?;
    if set.effective_date < today {
        return Err(ChangeSetError::EffectiveDatePassed {
            date: validate::format_date(set.effective_date),
            today: validate::format_date(today),
        });
    }
    Ok(())
}

/// Active members of the organization whose role grants `org.admin`.
pub fn active_admins(conn: &Connection, org_id: &str) -> Result<i64> {
    Ok(conn.query_row(
        "SELECT COUNT(*) FROM org_memberships m \
         JOIN roles r ON r.role_id = m.role_id \
         JOIN user_accounts u ON u.id = m.user_id \
         WHERE m.org_id = ?1 AND u.is_active = 1 \
           AND EXISTS (SELECT 1 FROM json_each(r.permissions_json) WHERE value = 'org.admin')",
        [org_id],
        |r| r.get(0),
    )?)
}

/// The two-person rule has one exception (owner decision 2026-09-30): an
/// organization with a single active administrator has nobody else to ask.
pub fn is_sole_admin(pool: &DbPool, org_id: &str) -> Result<bool> {
    let conn = pool
        .read()
        .map_err(|e| OrgStructureError::Db(e.to_string()))?;
    Ok(active_admins(&conn, org_id)? == 1)
}

/// The rules of an approval, on the row as it is: pending, somebody else's
/// (unless the approver is the only administrator), not dated in the past.
fn ensure_approvable(
    conn: &Connection,
    org_id: &str,
    approver: &str,
    set: &ChangeSet,
) -> Result<()> {
    if set.state != State::Pending {
        return Err(state_error(set, "approved"));
    }
    if set.author_user_id == approver && active_admins(conn, org_id)? != 1 {
        return Err(ChangeSetError::SelfApproval);
    }
    ensure_not_past(conn, org_id, set)
}

/// Refuses an approval that cannot succeed, before anything is run. Returns the
/// reorganization with its operations.
pub fn check_approvable(
    pool: &DbPool,
    org_id: &str,
    approver: &str,
    id: &str,
) -> Result<ChangeSet> {
    let conn = pool
        .read()
        .map_err(|e| OrgStructureError::Db(e.to_string()))?;
    let set = get_in(&conn, org_id, id)?;
    ensure_approvable(&conn, org_id, approver, &set)?;
    Ok(set)
}

/// pending -> applied, on the transaction of the batch that ran the operations.
/// The rules are checked again here, on the row the transaction sees: the
/// early check may have been made a moment before another administrator
/// withdrew or edited the set.
pub fn mark_applied_in(
    tx: &Transaction<'_>,
    org_id: &str,
    approver: &str,
    id: &str,
    undo: &Undo,
) -> Result<()> {
    let current = get_in(tx, org_id, id)?;
    ensure_approvable(tx, org_id, approver, &current)?;
    let record = json!({
        "ops": serde_json::from_str::<Json>(&current.payload).unwrap_or_else(|_| json!([])),
        "undo": undo,
    });
    let changed = tx.execute(
        "UPDATE org_change_sets SET state = 'applied', approver_user_id = ?3, payload = ?4 \
         WHERE org_id = ?1 AND id = ?2 AND state = 'pending'",
        params![org_id, id, approver, record.to_string()],
    )?;
    if changed != 1 {
        return Err(state_error(&current, "approved"));
    }
    capture(tx, org_id, approver, id, SqlWriteAction::Update)?;
    let applied = get_in(tx, org_id, id)?;
    audit(
        tx,
        org_id,
        approver,
        "org.change_set.approve",
        &applied,
        json!({ "self_approval": current.author_user_id == approver }),
    )?;
    Ok(())
}
