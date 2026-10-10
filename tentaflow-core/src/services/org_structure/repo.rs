//! Database layer of the org structure.
//!
//! Every write runs in ONE transaction that (1) mutates rows, (2) validates
//! what SQLite cannot express as a constraint, (3) records a core sync capture
//! per touched row and (4) appends the audit entry. Validation runs AFTER the
//! mutation, on the state the transaction would leave behind, so a rule is
//! checked against the real result rather than a prediction of it; an error
//! drops the transaction and nothing of the write survives.

use std::collections::BTreeMap;

use chrono::NaiveDate;
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Transaction};
use serde_json::{json, Value as Json};
use uuid::Uuid;

use super::error::{OrgStructureError as E, Result};
use super::replication::{self as repl, TableSpec};
use super::types::*;
use super::validate::{self, Edge, Interval};
use crate::db::DbPool;
use crate::sync::runtime::SqlWriteAction;

// ---------------------------------------------------------------------------
// Rows and values
// ---------------------------------------------------------------------------

/// One row of any org table, keyed by column name.
#[derive(Clone)]
struct Row(BTreeMap<&'static str, Value>);

impl Row {
    fn text(&self, column: &str) -> String {
        self.opt_text(column).unwrap_or_default()
    }

    fn opt_text(&self, column: &str) -> Option<String> {
        match self.0.get(column) {
            Some(Value::Text(t)) => Some(t.clone()),
            _ => None,
        }
    }

    fn set(&mut self, column: &'static str, value: Value) {
        self.0.insert(column, value);
    }

    fn interval(&self) -> Result<Interval> {
        let from = validate::parse_date(&self.text("valid_from"))?;
        let to = self
            .opt_text("valid_to")
            .map(|raw| validate::parse_date(&raw))
            .transpose()?;
        Interval::new(from, to)
    }
}

fn text(value: &str) -> Value {
    Value::Text(value.to_string())
}

fn opt(value: Option<&str>) -> Value {
    value.map_or(Value::Null, text)
}

fn date_value(date: NaiveDate) -> Value {
    Value::Text(validate::format_date(date))
}

fn opt_date_value(date: Option<NaiveDate>) -> Value {
    date.map_or(Value::Null, date_value)
}

/// Ids of the rows an operation makes. Random, except inside a scope (see
/// `Session::scope_ids`): then the n-th id of a scope is a pure function of
/// the scope, so two nodes that run the same operation make the same rows.
const ID_NAMESPACE: Uuid = Uuid::from_u128(0x6f72_675f_7374_7275_6374_5f69_6473_0001);

fn derived_id(scope: &str, ordinal: u32) -> String {
    Uuid::new_v5(&ID_NAMESPACE, format!("{scope}#{ordinal}").as_bytes()).to_string()
}

pub(super) fn fmt(date: NaiveDate) -> String {
    validate::format_date(date)
}

fn opt_fmt(date: Option<NaiveDate>) -> Option<String> {
    date.map(fmt)
}

// ---------------------------------------------------------------------------
// Write session
// ---------------------------------------------------------------------------

pub(super) struct Audited<T> {
    pub(super) value: T,
    pub(super) target: String,
    pub(super) summary: Json,
}

struct PendingAudit {
    action: String,
    target: String,
    summary: Json,
}

pub(super) struct Session<'a> {
    pub(super) tx: &'a Transaction<'a>,
    pub(super) org_id: &'a str,
    today: NaiveDate,
    confirm_backdated: bool,
    backdated: bool,
    /// Ending something BEFORE the day it starts removes it instead of being
    /// refused (a batch that replaces the structure takes back what an earlier
    /// import made for a later day). Ending it ON the day it starts always
    /// removes it: it never existed on any day.
    allow_withdraw: bool,
    touched: Vec<(&'static TableSpec, String, SqlWriteAction)>,
    warnings: Vec<Warning>,
    audits: Vec<PendingAudit>,
    /// Set while a planned reorganization runs: its operations mint ids from
    /// (reorganization, operation index) instead of at random, so approving
    /// the same plan on two nodes at once converges on the same rows.
    id_scope: Option<String>,
    id_ordinal: std::cell::Cell<u32>,
}

impl Session<'_> {
    /// From now on ids are derived from `scope` (restarting at zero), or random again for `None`.
    pub(super) fn scope_ids(&mut self, scope: Option<String>) {
        self.id_scope = scope;
        self.id_ordinal.set(0);
    }

    fn mint_id(&self) -> String {
        match &self.id_scope {
            Some(scope) => {
                let ordinal = self.id_ordinal.get();
                self.id_ordinal.set(ordinal + 1);
                derived_id(scope, ordinal)
            }
            None => Uuid::new_v4().to_string(),
        }
    }

    /// Queues the audit entry of one operation and hands back its value. The
    /// entry is written by `finish`, after the captures and the projection.
    pub(super) fn record<T>(&mut self, action: &str, audited: Audited<T>) -> T {
        self.audits.push(PendingAudit {
            action: action.to_string(),
            target: audited.target,
            summary: audited.summary,
        });
        audited.value
    }

    /// What the operations so far warned about; the batch decides which of
    /// them still hold in the final structure.
    pub(super) fn warnings(&self) -> &[Warning] {
        &self.warnings
    }

    /// A batch of edits carries the administrator's confirmation per operation.
    pub(super) fn set_confirm_backdated(&mut self, confirmed: bool) {
        self.confirm_backdated = confirmed;
    }

    /// An audit entry that belongs to the batch as a whole (the import summary).
    pub(super) fn record_summary(&mut self, action: &str, target: String, summary: Json) {
        self.audits.push(PendingAudit {
            action: action.to_string(),
            target,
            summary,
        });
    }

    /// Runs one operation of a batch so that its failure leaves no trace.
    /// The repository functions validate AFTER mutating and rely on the
    /// transaction being dropped on error; inside a batch the transaction
    /// lives on, so the failed operation is rolled back to its savepoint and
    /// the pending captures, warnings and audit entries it queued are dropped.
    pub(super) fn attempt<T>(
        &mut self,
        action: &str,
        op: impl FnOnce(&mut Self) -> Result<Audited<T>>,
    ) -> Result<T> {
        self.tx.execute_batch("SAVEPOINT org_batch_op")?;
        let (touched, warnings, audits) =
            (self.touched.len(), self.warnings.len(), self.audits.len());
        match op(self) {
            Ok(audited) => {
                self.tx.execute_batch("RELEASE org_batch_op")?;
                Ok(self.record(action, audited))
            }
            Err(e) => {
                self.tx
                    .execute_batch("ROLLBACK TO org_batch_op; RELEASE org_batch_op")?;
                self.touched.truncate(touched);
                self.warnings.truncate(warnings);
                self.audits.truncate(audits);
                Err(e)
            }
        }
    }

    /// Absences and deputies: a day before today is an administrator's to
    /// enter (and to confirm); for anybody else the first day is today.
    fn ensure_not_backdated_by(&mut self, date: NaiveDate, is_admin: bool) -> Result<()> {
        if date < self.today && !is_admin {
            return Err(E::BackdatingAdminOnly {
                date: fmt(date),
                today: fmt(self.today),
            });
        }
        self.ensure_not_backdated(date)
    }

    /// Changing what a row means (an absence's kind, a deputy's scope) rewrites the
    /// days it already covered, so a row that began before today is the
    /// administrator's to edit, whatever the dates of the edit.
    fn ensure_history_editable(&self, row_start: NaiveDate, is_admin: bool) -> Result<()> {
        if row_start < self.today && !is_admin {
            return Err(E::BackdatingAdminOnly {
                date: fmt(row_start),
                today: fmt(self.today),
            });
        }
        Ok(())
    }

    fn ensure_not_backdated(&mut self, date: NaiveDate) -> Result<()> {
        if date < self.today {
            if !self.confirm_backdated {
                return Err(E::BackdatedConfirmationRequired {
                    date: fmt(date),
                    today: fmt(self.today),
                });
            }
            self.backdated = true;
        }
        Ok(())
    }

    /// The row must exist AND belong to this organization; the two failures are
    /// reported apart so a foreign id is never mistaken for a typo.
    fn require_in_org(
        &self,
        table: &str,
        entity: &'static str,
        key_column: &str,
        id: &str,
    ) -> Result<()> {
        let owner: Option<String> = self
            .tx
            .query_row(
                &format!("SELECT org_id FROM {table} WHERE {key_column} = ?1 LIMIT 1"),
                [id],
                |r| r.get(0),
            )
            .optional()?;
        match owner {
            None => Err(E::NotFound {
                entity,
                id: id.to_string(),
            }),
            Some(owner) if owner != self.org_id => Err(E::CrossOrgReference {
                entity,
                id: id.to_string(),
            }),
            Some(_) => Ok(()),
        }
    }

    fn require_user(&self, user_id: &str) -> Result<()> {
        let exists = self
            .tx
            .query_row(
                "SELECT 1 FROM user_accounts WHERE id = ?1",
                [user_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !exists {
            return Err(E::NotFound {
                entity: "user",
                id: user_id.to_string(),
            });
        }
        let member = self
            .tx
            .query_row(
                "SELECT 1 FROM org_memberships WHERE org_id = ?1 AND user_id = ?2",
                [self.org_id, user_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if member {
            Ok(())
        } else {
            Err(E::CrossOrgReference {
                entity: "user",
                id: user_id.to_string(),
            })
        }
    }

    /// A person who is to ACT (a deputy) must be able to: an account that is
    /// switched off would be asked and never answer.
    fn require_active_user(&self, field: &'static str, user_id: &str) -> Result<()> {
        let active: bool = self.tx.query_row(
            "SELECT COALESCE(is_active, 0) FROM user_accounts WHERE id = ?1",
            [user_id],
            |r| r.get(0),
        )?;
        if active {
            Ok(())
        } else {
            Err(E::InvalidValue {
                field,
                reason: "the person's account is not active".into(),
            })
        }
    }

    fn new_row(&self, spec: &TableSpec) -> Row {
        let mut row = Row(BTreeMap::new());
        if spec.pk == "id" {
            row.set("id", text(&self.mint_id()));
        }
        row.set("org_id", text(self.org_id));
        row
    }

    fn get_row(&self, spec: &TableSpec, id: &str) -> Result<Option<Row>> {
        let sql = format!(
            "SELECT {} FROM {} WHERE {} = ?1 AND org_id = ?2",
            spec.columns.join(", "),
            spec.table,
            spec.pk
        );
        Ok(self
            .tx
            .query_row(&sql, [id, self.org_id], |r| read_row(spec, r))
            .optional()?)
    }

    fn require_row(&self, spec: &TableSpec, entity: &'static str, id: &str) -> Result<Row> {
        self.get_row(spec, id)?.ok_or_else(|| E::NotFound {
            entity,
            id: id.to_string(),
        })
    }

    /// Rows of an interval table matching every `column = value` pair, oldest first.
    fn rows_where(&self, spec: &TableSpec, filter: &[(&str, &str)]) -> Result<Vec<Row>> {
        let mut sql = format!(
            "SELECT {} FROM {} WHERE org_id = ?1",
            spec.columns.join(", "),
            spec.table
        );
        let mut params: Vec<&str> = vec![self.org_id];
        for (index, (column, value)) in filter.iter().enumerate() {
            sql.push_str(&format!(" AND {column} = ?{}", index + 2));
            params.push(value);
        }
        sql.push_str(" ORDER BY valid_from");
        let mut stmt = self.tx.prepare(&sql)?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params), |r| read_row(spec, r))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn touch(&mut self, spec: &'static TableSpec, id: &str, action: SqlWriteAction) {
        self.touched.push((spec, id.to_string(), action));
    }

    fn insert(&mut self, spec: &'static TableSpec, row: &Row) -> Result<()> {
        let placeholders: Vec<String> = (1..=spec.columns.len()).map(|n| format!("?{n}")).collect();
        let values: Vec<Value> = spec
            .columns
            .iter()
            .map(|c| row.0.get(c).cloned().unwrap_or(Value::Null))
            .collect();
        self.tx.execute(
            &format!(
                "INSERT INTO {} ({}) VALUES ({})",
                spec.table,
                spec.columns.join(", "),
                placeholders.join(", ")
            ),
            rusqlite::params_from_iter(values),
        )?;
        let id = row.text(spec.pk);
        self.touch(spec, &id, SqlWriteAction::Insert);
        Ok(())
    }

    fn set_cols(
        &mut self,
        spec: &'static TableSpec,
        id: &str,
        changes: &[(&'static str, Value)],
    ) -> Result<()> {
        if changes.is_empty() {
            return Ok(());
        }
        let assignments: Vec<String> = changes
            .iter()
            .enumerate()
            .map(|(i, (column, _))| format!("{column} = ?{}", i + 1))
            .collect();
        let mut values: Vec<Value> = changes.iter().map(|(_, v)| v.clone()).collect();
        values.push(text(id));
        self.tx.execute(
            &format!(
                "UPDATE {} SET {} WHERE {} = ?{}",
                spec.table,
                assignments.join(", "),
                spec.pk,
                changes.len() + 1
            ),
            rusqlite::params_from_iter(values),
        )?;
        self.touch(spec, id, SqlWriteAction::Update);
        Ok(())
    }

    fn delete(&mut self, spec: &'static TableSpec, id: &str) -> Result<()> {
        self.tx.execute(
            &format!("DELETE FROM {} WHERE {} = ?1", spec.table, spec.pk),
            [id],
        )?;
        self.touch(spec, id, SqlWriteAction::Delete);
        Ok(())
    }

    /// "From `from` on, these values apply" over one scope (`filter`) of an
    /// interval table. The row valid on `from` is closed at `from` and the new
    /// row starts there and inherits the closed row's end, so nothing is
    /// left uncovered and nothing overlaps. A row that itself starts on `from`
    /// is changed in place: an interval `[D, D)` is empty and forbidden.
    ///
    /// `change = None` only closes the scope at `from` (nothing follows).
    /// `fresh` is the template used when no row covers `from`; the new row then
    /// runs until the next row of the scope starts.
    ///
    /// Returns the id of the row that is valid from `from`, if any.
    #[allow(clippy::too_many_arguments)]
    fn replace_from(
        &mut self,
        spec: &'static TableSpec,
        filter: &[(&str, &str)],
        from: NaiveDate,
        change: Option<&[(&'static str, Value)]>,
        fresh: Option<Row>,
        entity: &'static str,
        scope_id: &str,
    ) -> Result<Option<String>> {
        let rows = self.rows_where(spec, filter)?;
        let mut covering = None;
        for row in &rows {
            if row.interval()?.contains(from) {
                covering = Some(row);
                break;
            }
        }
        match covering {
            Some(current) => {
                let current_id = current.text(spec.pk);
                let start = validate::parse_date(&current.text("valid_from"))?;
                if start == from {
                    return match change {
                        Some(change) => {
                            self.set_cols(spec, &current_id, change)?;
                            Ok(Some(current_id))
                        }
                        None => {
                            self.delete(spec, &current_id)?;
                            Ok(None)
                        }
                    };
                }
                let tail = current.opt_text("valid_to");
                self.set_cols(spec, &current_id, &[("valid_to", date_value(from))])?;
                let Some(change) = change else {
                    return Ok(None);
                };
                let mut next = current.clone();
                let next_id = self.mint_id();
                next.set(spec.pk, text(&next_id));
                next.set("valid_from", date_value(from));
                next.set("valid_to", tail.map_or(Value::Null, Value::Text));
                for (column, value) in change {
                    next.set(column, value.clone());
                }
                self.insert(spec, &next)?;
                Ok(Some(next_id))
            }
            None => {
                let Some(change) = change else {
                    return Ok(None);
                };
                let mut next = fresh.ok_or_else(|| E::NotValidAt {
                    entity,
                    id: scope_id.to_string(),
                    date: fmt(from),
                })?;
                let mut tail: Option<NaiveDate> = None;
                for row in &rows {
                    let start = validate::parse_date(&row.text("valid_from"))?;
                    if start > from && tail.is_none_or(|t| start < t) {
                        tail = Some(start);
                    }
                }
                let next_id = self.mint_id();
                next.set(spec.pk, text(&next_id));
                next.set("valid_from", date_value(from));
                next.set("valid_to", opt_date_value(tail));
                for (column, value) in change {
                    next.set(column, value.clone());
                }
                self.insert(spec, &next)?;
                Ok(Some(next_id))
            }
        }
    }

    /// Ends a scope at `from`, for things that stop existing (a liquidated
    /// unit, an ended position): rows starting at or after it go, a row
    /// spanning it is closed there. Returns the ids it touched.
    fn truncate_from(
        &mut self,
        spec: &'static TableSpec,
        filter: &[(&str, &str)],
        from: NaiveDate,
    ) -> Result<Vec<String>> {
        let mut touched = Vec::new();
        for row in self.rows_where(spec, filter)? {
            let id = row.text(spec.pk);
            let interval = row.interval()?;
            if interval.from >= from {
                self.delete(spec, &id)?;
            } else if interval.to.is_none_or(|end| end > from) {
                self.set_cols(spec, &id, &[("valid_to", date_value(from))])?;
            } else {
                continue;
            }
            touched.push(id);
        }
        Ok(touched)
    }

    fn unit_pieces(&self, unit_id: &str) -> Result<Vec<Interval>> {
        self.rows_where(&repl::UNITS, &[("unit_id", unit_id)])?
            .iter()
            .map(Row::interval)
            .collect()
    }

    fn unit_version_at(&self, unit_id: &str, day: NaiveDate) -> Result<Option<Row>> {
        for row in self.rows_where(&repl::UNITS, &[("unit_id", unit_id)])? {
            if row.interval()?.contains(day) {
                return Ok(Some(row));
            }
        }
        Ok(None)
    }

    fn require_unit_covers(&self, unit_id: &str, interval: &Interval) -> Result<()> {
        if validate::is_covered(&self.unit_pieces(unit_id)?, interval) {
            Ok(())
        } else {
            Err(E::OutsideValidity {
                entity: "unit",
                id: unit_id.to_string(),
                from: fmt(interval.from),
            })
        }
    }

    fn position_pieces(&self, position_id: &str) -> Result<Vec<Interval>> {
        self.rows_where(&repl::POSITIONS, &[("position_id", position_id)])?
            .iter()
            .map(Row::interval)
            .collect()
    }

    /// The version of the position that is valid on `day`.
    fn require_position_valid_at(&self, position_id: &str, day: NaiveDate) -> Result<Row> {
        for row in self.rows_where(&repl::POSITIONS, &[("position_id", position_id)])? {
            if row.interval()?.contains(day) {
                return Ok(row);
            }
        }
        Err(E::NotValidAt {
            entity: "position",
            id: position_id.to_string(),
            date: fmt(day),
        })
    }

    fn require_position_covers(&self, position_id: &str, interval: &Interval) -> Result<()> {
        if validate::is_covered(&self.position_pieces(position_id)?, interval) {
            Ok(())
        } else {
            Err(E::OutsideValidity {
                entity: "position",
                id: position_id.to_string(),
                from: fmt(interval.from),
            })
        }
    }

    /// The day the position stops existing (`None` = open-ended).
    fn position_end(&self, position_id: &str) -> Result<Option<NaiveDate>> {
        let pieces = self.position_pieces(position_id)?;
        Ok(pieces
            .iter()
            .max_by_key(|i| i.from)
            .and_then(|last| last.to))
    }

    fn warn_if_headless(&mut self, unit_id: &str, from: NaiveDate) -> Result<()> {
        if let Some(version) = self.unit_version_at(unit_id, from)? {
            if version.opt_text("head_position_id").is_none() {
                self.warnings.push(Warning::UnitWithoutHead {
                    unit_id: unit_id.to_string(),
                    from,
                });
            }
        }
        Ok(())
    }

    // -- post-mutation invariants -------------------------------------------

    fn check_primary_line_exclusive(&self, position_id: &str) -> Result<()> {
        let rows = self.rows_where(
            &repl::REPORTING_LINES,
            &[("position_id", position_id), ("kind", "primary")],
        )?;
        let intervals = rows.iter().map(Row::interval).collect::<Result<Vec<_>>>()?;
        match validate::first_overlap(&intervals) {
            Some(day) => Err(E::PrimaryLineOverlap {
                position_id: position_id.to_string(),
                from: fmt(day),
            }),
            None => Ok(()),
        }
    }

    fn check_line_cycle(&self, position_id: &str, within: &Interval) -> Result<()> {
        let mut edges = Vec::new();
        for row in self.all_rows_where(&repl::REPORTING_LINES, "kind = 'primary'")? {
            edges.push(Edge {
                child: row.text("position_id"),
                parent: row.text("parent_position_id"),
                interval: row.interval()?,
            });
        }
        match validate::find_cycle(&edges, position_id, within) {
            Some(day) => Err(E::ReportingCycle { date: fmt(day) }),
            None => Ok(()),
        }
    }

    fn check_unit_cycle(&self, unit_id: &str, within: &Interval) -> Result<()> {
        let mut edges = Vec::new();
        for row in self.all_rows_where(&repl::UNITS, "parent_unit_id IS NOT NULL")? {
            edges.push(Edge {
                child: row.text("unit_id"),
                parent: row.text("parent_unit_id"),
                interval: row.interval()?,
            });
        }
        match validate::find_cycle(&edges, unit_id, within) {
            Some(day) => Err(E::UnitCycle { date: fmt(day) }),
            None => Ok(()),
        }
    }

    /// Every row of the organization satisfying a fixed (non-user) predicate.
    fn all_rows_where(&self, spec: &TableSpec, predicate: &str) -> Result<Vec<Row>> {
        let sql = format!(
            "SELECT {} FROM {} WHERE org_id = ?1 AND {predicate}",
            spec.columns.join(", "),
            spec.table
        );
        let mut stmt = self.tx.prepare(&sql)?;
        let rows = stmt
            .query_map([self.org_id], |r| read_row(spec, r))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    fn check_primary_assignments(&self, subject_column: &str, subject_id: &str) -> Result<()> {
        let mut primaries = Vec::new();
        for held in self.rows_where(&repl::ASSIGNMENTS, &[(subject_column, subject_id)])? {
            if matches!(held.0.get("is_primary"), Some(Value::Integer(1))) {
                primaries.push(held.interval()?);
            }
        }
        match validate::first_overlap(&primaries) {
            Some(day) => Err(E::PrimaryAssignmentOverlap { from: fmt(day) }),
            None => Ok(()),
        }
    }

    /// A person who holds anything on a day has exactly one primary position.
    /// When the primary one ended and exactly one other position continues, it
    /// becomes primary from that day; with several left the choice is the
    /// administrator's, so the write only warns.
    fn rebalance_primary(&mut self, row: &Row, from: NaiveDate) -> Result<()> {
        let (column, subject_id, subject) = subject_of(row);
        for _ in 0..64 {
            let rows = self.rows_where(&repl::ASSIGNMENTS, &[(column, &subject_id)])?;
            let mut held = Vec::new();
            for r in &rows {
                held.push((
                    r.interval()?,
                    matches!(r.0.get("is_primary"), Some(Value::Integer(1))),
                    r.text("position_id"),
                ));
            }
            let boundaries = held
                .iter()
                .flat_map(|(i, _, _)| std::iter::once(i.from).chain(i.to));
            let starts = validate::segment_starts(&Interval { from, to: None }, boundaries);
            let mut promoted = false;
            for day in starts {
                let active: Vec<_> = held.iter().filter(|(i, _, _)| i.contains(day)).collect();
                if active.is_empty() || active.iter().any(|(_, primary, _)| *primary) {
                    continue;
                }
                if active.len() > 1 {
                    self.warnings.push(Warning::PersonWithoutPrimary {
                        subject: subject.clone(),
                        from: day,
                    });
                    return Ok(());
                }
                let position_id = active[0].2.clone();
                self.replace_from(
                    &repl::ASSIGNMENTS,
                    &[("position_id", &position_id), (column, &subject_id)],
                    day,
                    Some(&[("is_primary", bool_value(true))]),
                    None,
                    "assignment",
                    &position_id,
                )?;
                promoted = true;
                break;
            }
            if !promoted {
                break;
            }
        }
        self.check_primary_assignments(column, &subject_id)
    }

    fn check_assignment_invariants(
        &mut self,
        assignment_id: &str,
        interval: &Interval,
    ) -> Result<()> {
        let row = self.require_row(&repl::ASSIGNMENTS, "assignment", assignment_id)?;
        let (subject_column, subject_id, subject) = subject_of(&row);
        let position_id = row.text("position_id");

        let same_position = self.rows_where(
            &repl::ASSIGNMENTS,
            &[("position_id", &position_id), (subject_column, &subject_id)],
        )?;
        let intervals = same_position
            .iter()
            .map(Row::interval)
            .collect::<Result<Vec<_>>>()?;
        if validate::first_overlap(&intervals).is_some() {
            return Err(E::AssignmentOverlap { position_id });
        }

        self.check_primary_assignments(subject_column, &subject_id)?;
        let mut shares = Vec::new();
        for held in self.rows_where(&repl::ASSIGNMENTS, &[(subject_column, &subject_id)])? {
            let share = match held.0.get("share") {
                Some(Value::Real(v)) => *v,
                _ => 0.0,
            };
            shares.push((held.interval()?, share));
        }
        if let Some((day, total)) = validate::peak_share(&shares, interval) {
            if validate::share_exceeds_full_time(total) {
                self.warnings.push(Warning::ShareOverbooked {
                    subject,
                    from: day,
                    total,
                });
            }
        }
        Ok(())
    }
}

fn read_row(spec: &TableSpec, r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    let mut row = Row(BTreeMap::new());
    for (index, column) in spec.columns.iter().enumerate() {
        row.set(column, r.get::<_, Value>(index)?);
    }
    Ok(row)
}

fn subject_of(row: &Row) -> (&'static str, String, Subject) {
    match row.opt_text("user_id") {
        Some(id) => ("user_id", id.clone(), Subject::User(id)),
        None => {
            let id = row.text("external_person_id");
            ("external_person_id", id.clone(), Subject::External(id))
        }
    }
}

pub(super) fn timezone_of(conn: &Connection, org_id: &str) -> Result<String> {
    Ok(conn
        .query_row(
            "SELECT timezone FROM org_structure_settings WHERE org_id = ?1",
            [org_id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_else(|| DEFAULT_TIMEZONE.to_string()))
}

/// Runs one write: mutation, validation, sync captures and the audit entry in
/// a single transaction.
pub(super) fn run<T>(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    action: &str,
    body: impl FnOnce(&mut Session<'_>) -> Result<Audited<T>>,
) -> Result<Written<T>> {
    let mut conn = pool.write().map_err(|e| E::Db(e.to_string()))?;
    let tx = conn.transaction()?;
    let written = run_in_tx(
        &tx,
        ctx.org_id,
        Some(ctx.actor_user_id),
        ctx.confirm_backdated,
        action,
        body,
    )?;
    tx.commit()?;
    Ok(written)
}

/// The body of `run` for a caller that owns the transaction (a membership or
/// user removal that has to end assignments atomically with itself). Does not
/// commit.
fn run_in_tx<T>(
    tx: &Transaction<'_>,
    org_id: &str,
    actor: Option<&str>,
    confirm_backdated: bool,
    action: &str,
    body: impl FnOnce(&mut Session<'_>) -> Result<Audited<T>>,
) -> Result<Written<T>> {
    let mut session = Session::start(tx, org_id, confirm_backdated)?;
    let audited = body(&mut session)?;
    let value = session.record(action, audited);
    let warnings = session.finish(actor, None)?;
    Ok(Written { value, warnings })
}

/// What a batch body decided: the value to hand back and whether its changes
/// are kept. A dry run and a refused apply return a value and drop the changes.
pub(super) struct BatchOutcome<T> {
    pub(super) value: T,
    pub(super) keep: bool,
}

/// How a batch treats the two rules a single write refuses by default.
#[derive(Clone, Copy)]
pub(super) struct BatchMode {
    /// `true`: the caller already refused every operation dated before today
    /// that the administrator did not confirm (an import decides per row).
    /// `false`: the repository functions refuse them, per operation.
    pub(super) backdating_checked: bool,
    /// An import replacing the structure takes back what an earlier import
    /// made for a later day; an edit that ends something before it starts is
    /// an error, as it is for a single write.
    pub(super) allow_withdraw: bool,
}

/// Runs many operations as ONE transaction with ONE projection recompute and
/// audit entries that carry `source`. With `commit == false`, or when the body
/// asks not to keep its changes, the transaction is dropped: a dry run
/// executes the same operations as the apply, so the two cannot disagree.
pub(super) fn run_batch<T>(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    source: &str,
    commit: bool,
    mode: BatchMode,
    body: impl FnOnce(&mut Session<'_>) -> Result<BatchOutcome<T>>,
) -> Result<T> {
    let mut conn = pool.write().map_err(|e| E::Db(e.to_string()))?;
    let tx = conn.transaction()?;
    let mut session = Session::start(&tx, ctx.org_id, mode.backdating_checked)?;
    session.allow_withdraw = mode.allow_withdraw;
    let outcome = body(&mut session)?;
    // The finishing work (captures, projection, audit) runs for a dry run too,
    // inside the transaction that is then dropped: a run that only tried the
    // file must cost what an apply costs, and fail where an apply would.
    session.finish(Some(ctx.actor_user_id), Some(source))?;
    if commit && outcome.keep {
        tx.commit()?;
    }
    Ok(outcome.value)
}

impl<'a> Session<'a> {
    fn start(tx: &'a Transaction<'a>, org_id: &'a str, confirm_backdated: bool) -> Result<Self> {
        Ok(Session {
            tx,
            org_id,
            today: validate::today_in_zone(&timezone_of(tx, org_id)?)?,
            confirm_backdated,
            backdated: false,
            allow_withdraw: false,
            touched: Vec::new(),
            warnings: Vec::new(),
            audits: Vec::new(),
            id_scope: None,
            id_ordinal: std::cell::Cell::new(0),
        })
    }

    /// Records the sync captures (a row written twice is captured once, with
    /// its final content), recomputes the permission projection once, then
    /// writes the queued audit entries. The projection report lands on the
    /// last entry.
    fn finish(self, actor: Option<&str>, source: Option<&str>) -> Result<Vec<Warning>> {
        let Session {
            tx,
            org_id,
            touched,
            warnings,
            backdated,
            audits,
            ..
        } = self;
        let _hlc = crate::sync::runtime::defer_hlc_persistence();
        let mut captures: Vec<(&'static TableSpec, String, SqlWriteAction)> = Vec::new();
        let mut position: std::collections::HashMap<(&'static str, String), usize> =
            std::collections::HashMap::new();
        for (spec, id, action) in touched {
            match position.get(&(spec.table, id.clone())) {
                Some(&at) => {
                    let entry = &mut captures[at];
                    if !(matches!(entry.2, SqlWriteAction::Insert)
                        && matches!(action, SqlWriteAction::Update))
                    {
                        entry.2 = action;
                    }
                }
                None => {
                    position.insert((spec.table, id.clone()), captures.len());
                    captures.push((spec, id, action));
                }
            }
        }
        let changes_projection = captures
            .iter()
            .any(|(spec, _, _)| super::projection::affects_projection(spec.table));
        for (spec, id, capture_action) in captures {
            repl::capture_row(tx, spec, org_id, &id, capture_action, actor)?;
        }
        // Same transaction as the write, so the permission tables never show a
        // structure that was rolled back or lag behind one that committed. The
        // day is read again: a timezone change moves it.
        let projection = if changes_projection {
            let day = validate::today_in_zone(&timezone_of(tx, org_id)?)?;
            Some(super::projection::recompute_in_tx(tx, org_id, None, day)?)
        } else {
            None
        };

        // A batch writes ONE entry: the operations are listed by action and
        // target inside the last (summary) entry instead of costing a hash-chain
        // row each — thousands of rows would hold the write connection.
        let audits = if source.is_some() && audits.len() > 1 {
            let mut audits = audits;
            let mut summary = audits.pop().expect("more than one entry");
            let operations: Vec<Json> =
                audits.iter().map(|a| json!([a.action, a.target])).collect();
            if let Json::Object(map) = &mut summary.summary {
                map.insert("operations".to_string(), Json::Array(operations));
            }
            vec![summary]
        } else {
            audits
        };
        let last = audits.len().saturating_sub(1);
        for (index, audit) in audits.into_iter().enumerate() {
            let mut details = audit.summary;
            if let Json::Object(map) = &mut details {
                map.insert("org_id".to_string(), json!(org_id));
                if let Some(source) = source {
                    map.insert("source".to_string(), json!(source));
                }
                if index == last {
                    if let Some(report) = projection.as_ref().filter(|r| r.written + r.removed > 0)
                    {
                        map.insert(
                            "profiles".to_string(),
                            json!({ "written": report.written, "removed": report.removed }),
                        );
                    }
                }
                if backdated {
                    map.insert("backdated".to_string(), json!(true));
                }
            }
            crate::db::repository::log_audit_tx(
                tx,
                actor,
                None,
                &audit.action,
                Some(&audit.target),
                Some(&details.to_string()),
                None,
                None,
            )?;
        }
        Ok(warnings)
    }
}

/// The audit summary of an end: ending on the day a thing starts removes it
/// (and says so), because it existed on no day at all.
fn removal_summary(from: NaiveDate, removed: bool) -> Json {
    if removed {
        json!({ "from": fmt(from), "removed": "removed same-day creation" })
    } else {
        json!({ "from": fmt(from) })
    }
}

fn diff(before: &Row, after: &Row, columns: &[&'static str]) -> Json {
    let mut changes = serde_json::Map::new();
    for column in columns {
        let old = before.0.get(column).cloned().unwrap_or(Value::Null);
        let new = after.0.get(column).cloned().unwrap_or(Value::Null);
        if old != new {
            changes.insert(
                (*column).to_string(),
                json!({ "before": value_json(&old), "after": value_json(&new) }),
            );
        }
    }
    Json::Object(changes)
}

fn value_json(value: &Value) -> Json {
    match value {
        Value::Null | Value::Blob(_) => Json::Null,
        Value::Integer(v) => json!(v),
        Value::Real(v) => json!(v),
        Value::Text(v) => json!(v),
    }
}

// ---------------------------------------------------------------------------
// Typed reads
// ---------------------------------------------------------------------------

fn date_col(r: &rusqlite::Row<'_>, column: &str) -> rusqlite::Result<NaiveDate> {
    let raw: String = r.get(column)?;
    validate::parse_date(&raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
    })
}

fn opt_date_col(r: &rusqlite::Row<'_>, column: &str) -> rusqlite::Result<Option<NaiveDate>> {
    let raw: Option<String> = r.get(column)?;
    raw.map(|v| {
        validate::parse_date(&v).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
    })
    .transpose()
}

pub(super) fn map_unit(r: &rusqlite::Row<'_>) -> rusqlite::Result<Unit> {
    Ok(Unit {
        id: r.get("id")?,
        unit_id: r.get("unit_id")?,
        name: r.get("name")?,
        code: r.get("code")?,
        type_id: r.get("type_id")?,
        parent_unit_id: r.get("parent_unit_id")?,
        color: r.get("color")?,
        head_position_id: r.get("head_position_id")?,
        valid_from: date_col(r, "valid_from")?,
        valid_to: opt_date_col(r, "valid_to")?,
    })
}

pub(super) fn map_position(r: &rusqlite::Row<'_>) -> rusqlite::Result<Position> {
    Ok(Position {
        id: r.get("id")?,
        position_id: r.get("position_id")?,
        unit_id: r.get("unit_id")?,
        name: r.get("name")?,
        code: r.get("code")?,
        role_id: r.get("role_id")?,
        is_manager: r.get::<_, Option<bool>>("is_manager")?,
        is_staff: r.get("is_staff")?,
        valid_from: date_col(r, "valid_from")?,
        valid_to: opt_date_col(r, "valid_to")?,
    })
}

pub(super) fn map_deputy(r: &rusqlite::Row<'_>) -> rusqlite::Result<DeputyHead> {
    Ok(DeputyHead {
        id: r.get("id")?,
        unit_id: r.get("unit_id")?,
        position_id: r.get("position_id")?,
        ord: r.get("ord")?,
        valid_from: date_col(r, "valid_from")?,
        valid_to: opt_date_col(r, "valid_to")?,
    })
}

pub(super) fn map_line(r: &rusqlite::Row<'_>) -> rusqlite::Result<ReportingLine> {
    let kind: String = r.get("kind")?;
    Ok(ReportingLine {
        id: r.get("id")?,
        position_id: r.get("position_id")?,
        parent_position_id: r.get("parent_position_id")?,
        kind: LineKind::parse(&kind).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("unknown line kind {kind}").into(),
            )
        })?,
        priority: r.get("priority")?,
        valid_from: date_col(r, "valid_from")?,
        valid_to: opt_date_col(r, "valid_to")?,
    })
}

pub(super) fn map_assignment(r: &rusqlite::Row<'_>) -> rusqlite::Result<Assignment> {
    let kind: String = r.get("type")?;
    let user_id: Option<String> = r.get("user_id")?;
    let external: Option<String> = r.get("external_person_id")?;
    let subject = match (user_id, external) {
        (Some(id), None) => Subject::User(id),
        (None, Some(id)) => Subject::External(id),
        _ => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                "assignment must name exactly one subject".into(),
            ))
        }
    };
    Ok(Assignment {
        id: r.get("id")?,
        position_id: r.get("position_id")?,
        subject,
        kind: AssignmentType::parse(&kind).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("unknown assignment type {kind}").into(),
            )
        })?,
        share: r.get("share")?,
        is_primary: r.get("is_primary")?,
        valid_from: date_col(r, "valid_from")?,
        valid_to: opt_date_col(r, "valid_to")?,
    })
}

fn map_unit_type(r: &rusqlite::Row<'_>) -> rusqlite::Result<UnitType> {
    Ok(UnitType {
        id: r.get("id")?,
        name: r.get("name")?,
        color: r.get("color")?,
        icon: r.get("icon")?,
    })
}

fn map_external_person(r: &rusqlite::Row<'_>) -> rusqlite::Result<ExternalPerson> {
    Ok(ExternalPerson {
        id: r.get("id")?,
        display_name: r.get("display_name")?,
        email: r.get("email")?,
        note: r.get("note")?,
    })
}

pub(super) fn select_where<T>(
    conn: &Connection,
    spec: &TableSpec,
    predicate: &str,
    params: impl rusqlite::Params,
    map: fn(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>> {
    let sql = format!(
        "SELECT {} FROM {} WHERE {predicate}",
        spec.columns.join(", "),
        spec.table
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params, map)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn one<T>(rows: Vec<T>, entity: &'static str, id: &str) -> Result<T> {
    rows.into_iter().next().ok_or_else(|| E::NotFound {
        entity,
        id: id.to_string(),
    })
}

pub(super) const VALID_AT: &str =
    "org_id = ?1 AND valid_from <= ?2 AND (valid_to IS NULL OR valid_to > ?2)";

pub fn list_units_at(pool: &DbPool, org_id: &str, day: NaiveDate) -> Result<Vec<Unit>> {
    let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
    select_where(
        &conn,
        &repl::UNITS,
        &format!("{VALID_AT} ORDER BY name, unit_id"),
        [org_id, &fmt(day)],
        map_unit,
    )
}

pub fn list_positions_at(pool: &DbPool, org_id: &str, day: NaiveDate) -> Result<Vec<Position>> {
    let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
    select_where(
        &conn,
        &repl::POSITIONS,
        &format!("{VALID_AT} ORDER BY name, id"),
        [org_id, &fmt(day)],
        map_position,
    )
}

pub fn list_assignments_at(pool: &DbPool, org_id: &str, day: NaiveDate) -> Result<Vec<Assignment>> {
    let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
    select_where(
        &conn,
        &repl::ASSIGNMENTS,
        &format!(
            "{VALID_AT} ORDER BY is_primary DESC, valid_from, \
             (SELECT p.name FROM org_positions p WHERE p.org_id = org_assignments.org_id \
                AND p.position_id = org_assignments.position_id ORDER BY p.valid_from DESC LIMIT 1), \
             position_id, id"
        ),
        [org_id, &fmt(day)],
        map_assignment,
    )
}

pub fn list_reporting_lines_at(
    pool: &DbPool,
    org_id: &str,
    day: NaiveDate,
) -> Result<Vec<ReportingLine>> {
    let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
    select_where(
        &conn,
        &repl::REPORTING_LINES,
        &format!("{VALID_AT} ORDER BY position_id, kind, priority, id"),
        [org_id, &fmt(day)],
        map_line,
    )
}

pub fn list_deputy_heads_at(
    pool: &DbPool,
    org_id: &str,
    day: NaiveDate,
) -> Result<Vec<DeputyHead>> {
    let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
    select_where(
        &conn,
        &repl::DEPUTY_HEADS,
        &format!("{VALID_AT} ORDER BY unit_id, ord"),
        [org_id, &fmt(day)],
        map_deputy,
    )
}

pub fn list_unit_types(pool: &DbPool, org_id: &str) -> Result<Vec<UnitType>> {
    let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
    unit_types_in(&conn, org_id)
}

pub(super) fn unit_types_in(conn: &Connection, org_id: &str) -> Result<Vec<UnitType>> {
    select_where(
        conn,
        &repl::UNIT_TYPES,
        "org_id = ?1 ORDER BY name, id",
        [org_id],
        map_unit_type,
    )
}

pub fn list_external_persons(pool: &DbPool, org_id: &str) -> Result<Vec<ExternalPerson>> {
    let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
    select_where(
        &conn,
        &repl::EXTERNAL_PERSONS,
        "org_id = ?1 ORDER BY display_name, id",
        [org_id],
        map_external_person,
    )
}

/// The organization's calendar day right now.
pub fn org_today(pool: &DbPool, org_id: &str) -> Result<NaiveDate> {
    validate::today_in_zone(&get_settings(pool, org_id)?.timezone)
}

pub fn get_settings(pool: &DbPool, org_id: &str) -> Result<Settings> {
    let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
    Ok(Settings {
        org_id: org_id.to_string(),
        timezone: timezone_of(&conn, org_id)?,
    })
}

// ---------------------------------------------------------------------------
// Unit types
// ---------------------------------------------------------------------------

fn ensure_unit_type_name_free(s: &Session<'_>, name: &str, except_id: Option<&str>) -> Result<()> {
    let taken = s
        .tx
        .query_row(
            "SELECT 1 FROM org_unit_types WHERE org_id = ?1 AND lower(name) = lower(?2) AND id <> ?3",
            [s.org_id, name, except_id.unwrap_or("")],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if taken {
        Err(E::Duplicate {
            entity: "unit type",
            name: name.to_string(),
        })
    } else {
        Ok(())
    }
}

pub fn create_unit_type(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    name: &str,
    color: Option<&str>,
    icon: Option<&str>,
) -> Result<Written<UnitType>> {
    run(pool, ctx, "org.unit_type.create", |s| {
        create_unit_type_in(s, name, color, icon)
    })
}

pub(super) fn create_unit_type_in(
    s: &mut Session<'_>,
    name: &str,
    color: Option<&str>,
    icon: Option<&str>,
) -> Result<Audited<UnitType>> {
    let name = validate::require_non_empty("name", name)?;
    ensure_unit_type_name_free(s, &name, None)?;
    let mut row = s.new_row(&repl::UNIT_TYPES);
    row.set("name", text(&name));
    row.set("color", opt(validate::normalize_optional(color).as_deref()));
    row.set("icon", opt(validate::normalize_optional(icon).as_deref()));
    s.insert(&repl::UNIT_TYPES, &row)?;
    let id = row.text("id");
    let value = one(
        select_where(s.tx, &repl::UNIT_TYPES, "id = ?1", [&id], map_unit_type)?,
        "unit type",
        &id,
    )?;
    Ok(Audited {
        target: format!("org_unit_type:{id}"),
        summary: json!({ "name": name }),
        value,
    })
}

pub fn update_unit_type(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    id: &str,
    patch: &UnitTypePatch,
) -> Result<Written<UnitType>> {
    run(pool, ctx, "org.unit_type.update", |s| {
        update_unit_type_in(s, id, patch)
    })
}

pub(super) fn update_unit_type_in(
    s: &mut Session<'_>,
    id: &str,
    patch: &UnitTypePatch,
) -> Result<Audited<UnitType>> {
    s.require_in_org("org_unit_types", "unit type", "id", id)?;
    let before = s.require_row(&repl::UNIT_TYPES, "unit type", id)?;
    let mut changes: Vec<(&'static str, Value)> = Vec::new();
    if let Some(name) = &patch.name {
        let name = validate::require_non_empty("name", name)?;
        ensure_unit_type_name_free(s, &name, Some(id))?;
        changes.push(("name", text(&name)));
    }
    if let Some(color) = &patch.color {
        changes.push((
            "color",
            opt(validate::normalize_optional(color.as_deref()).as_deref()),
        ));
    }
    if let Some(icon) = &patch.icon {
        changes.push((
            "icon",
            opt(validate::normalize_optional(icon.as_deref()).as_deref()),
        ));
    }
    s.set_cols(&repl::UNIT_TYPES, id, &changes)?;
    let after = s.require_row(&repl::UNIT_TYPES, "unit type", id)?;
    let value = one(
        select_where(s.tx, &repl::UNIT_TYPES, "id = ?1", [id], map_unit_type)?,
        "unit type",
        id,
    )?;
    Ok(Audited {
        target: format!("org_unit_type:{id}"),
        summary: json!({ "changes": diff(&before, &after, &["name", "color", "icon"]) }),
        value,
    })
}

pub fn delete_unit_type(pool: &DbPool, ctx: &WriteCtx<'_>, id: &str) -> Result<Written<()>> {
    run(pool, ctx, "org.unit_type.delete", |s| {
        delete_unit_type_in(s, id)
    })
}

pub(super) fn delete_unit_type_in(s: &mut Session<'_>, id: &str) -> Result<Audited<()>> {
    s.require_in_org("org_unit_types", "unit type", "id", id)?;
    let in_use =
        s.tx.query_row(
            "SELECT 1 FROM org_units WHERE org_id = ?1 AND type_id = ?2 LIMIT 1",
            [s.org_id, id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if in_use {
        return Err(E::UnitTypeInUse(id.to_string()));
    }
    s.delete(&repl::UNIT_TYPES, id)?;
    Ok(Audited {
        target: format!("org_unit_type:{id}"),
        summary: json!({}),
        value: (),
    })
}

// ---------------------------------------------------------------------------
// Units
// ---------------------------------------------------------------------------

fn unit_row(s: &Session<'_>, row_id: &str) -> Result<Unit> {
    one(
        select_where(s.tx, &repl::UNITS, "id = ?1", [row_id], map_unit)?,
        "unit",
        row_id,
    )
}

pub fn create_unit(pool: &DbPool, ctx: &WriteCtx<'_>, new: &NewUnit) -> Result<Written<Unit>> {
    run(pool, ctx, "org.unit.create", |s| create_unit_in(s, new))
}

pub(super) fn create_unit_in(s: &mut Session<'_>, new: &NewUnit) -> Result<Audited<Unit>> {
    let name = validate::require_non_empty("name", &new.name)?;
    let interval = Interval::new(new.valid_from, new.valid_to)?;
    s.ensure_not_backdated(new.valid_from)?;
    if let Some(type_id) = &new.type_id {
        s.require_in_org("org_unit_types", "unit type", "id", type_id)?;
    }
    if let Some(parent) = &new.parent_unit_id {
        s.require_in_org("org_units", "unit", "unit_id", parent)?;
        s.require_unit_covers(parent, &interval)?;
    }
    let unit_id = s.mint_id();
    let mut row = s.new_row(&repl::UNITS);
    row.set("unit_id", text(&unit_id));
    row.set("name", text(&name));
    row.set(
        "code",
        opt(validate::normalize_optional(new.code.as_deref()).as_deref()),
    );
    row.set("type_id", opt(new.type_id.as_deref()));
    row.set("parent_unit_id", opt(new.parent_unit_id.as_deref()));
    row.set(
        "color",
        opt(validate::normalize_optional(new.color.as_deref()).as_deref()),
    );
    row.set("valid_from", date_value(new.valid_from));
    row.set("valid_to", opt_date_value(new.valid_to));
    s.insert(&repl::UNITS, &row)?;
    s.warn_if_headless(&unit_id, new.valid_from)?;
    Ok(Audited {
        value: unit_row(s, &row.text("id"))?,
        target: format!("org_unit:{unit_id}"),
        summary: json!({
            "parent_unit_id": new.parent_unit_id,
            "valid_from": fmt(new.valid_from),
            "valid_to": opt_fmt(new.valid_to),
        }),
    })
}

/// Changes name, code, type or colour from `from` on; earlier days keep the
/// old values, so "state on a day" shows the old name.
pub fn update_unit(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    unit_id: &str,
    patch: &UnitPatch,
    from: NaiveDate,
) -> Result<Written<Unit>> {
    run(pool, ctx, "org.unit.update", |s| {
        update_unit_in(s, unit_id, patch, from)
    })
}

pub(super) fn update_unit_in(
    s: &mut Session<'_>,
    unit_id: &str,
    patch: &UnitPatch,
    from: NaiveDate,
) -> Result<Audited<Unit>> {
    s.ensure_not_backdated(from)?;
    s.require_in_org("org_units", "unit", "unit_id", unit_id)?;
    let before = s
        .unit_version_at(unit_id, from)?
        .ok_or_else(|| E::NotValidAt {
            entity: "unit",
            id: unit_id.to_string(),
            date: fmt(from),
        })?;
    let mut changes: Vec<(&'static str, Value)> = Vec::new();
    if let Some(name) = &patch.name {
        changes.push(("name", text(&validate::require_non_empty("name", name)?)));
    }
    if let Some(code) = &patch.code {
        changes.push((
            "code",
            opt(validate::normalize_optional(code.as_deref()).as_deref()),
        ));
    }
    if let Some(type_id) = &patch.type_id {
        if let Some(type_id) = type_id {
            s.require_in_org("org_unit_types", "unit type", "id", type_id)?;
        }
        changes.push(("type_id", opt(type_id.as_deref())));
    }
    if let Some(color) = &patch.color {
        changes.push((
            "color",
            opt(validate::normalize_optional(color.as_deref()).as_deref()),
        ));
    }
    if changes.is_empty() {
        return Err(E::InvalidValue {
            field: "patch",
            reason: "nothing to change".to_string(),
        });
    }
    let row_id = s
        .replace_from(
            &repl::UNITS,
            &[("unit_id", unit_id)],
            from,
            Some(&changes),
            None,
            "unit",
            unit_id,
        )?
        .expect("a change always yields a row");
    let after = s.require_row(&repl::UNITS, "unit", &row_id)?;
    Ok(Audited {
        value: unit_row(s, &row_id)?,
        target: format!("org_unit:{unit_id}"),
        summary: json!({
            "from": fmt(from),
            "changes": diff(&before, &after, &["name", "code", "type_id", "color"]),
        }),
    })
}

/// Changes the parent of a unit from `from` on (`None` = becomes a root).
pub fn move_unit(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    unit_id: &str,
    new_parent: Option<&str>,
    from: NaiveDate,
) -> Result<Written<Unit>> {
    run(pool, ctx, "org.unit.move", |s| {
        move_unit_in(s, unit_id, new_parent, from)
    })
}

pub(super) fn move_unit_in(
    s: &mut Session<'_>,
    unit_id: &str,
    new_parent: Option<&str>,
    from: NaiveDate,
) -> Result<Audited<Unit>> {
    s.ensure_not_backdated(from)?;
    s.require_in_org("org_units", "unit", "unit_id", unit_id)?;
    if let Some(parent) = new_parent {
        s.require_in_org("org_units", "unit", "unit_id", parent)?;
        if parent == unit_id {
            return Err(E::UnitCycle { date: fmt(from) });
        }
    }
    let before = s
        .unit_version_at(unit_id, from)?
        .ok_or_else(|| E::NotValidAt {
            entity: "unit",
            id: unit_id.to_string(),
            date: fmt(from),
        })?;
    let row_id = s
        .replace_from(
            &repl::UNITS,
            &[("unit_id", unit_id)],
            from,
            Some(&[("parent_unit_id", opt(new_parent))]),
            None,
            "unit",
            unit_id,
        )?
        .expect("a change always yields a row");
    let moved = unit_row(s, &row_id)?;
    let interval = Interval::new(moved.valid_from, moved.valid_to)?;
    if let Some(parent) = new_parent {
        s.require_unit_covers(parent, &interval)?;
    }
    s.check_unit_cycle(unit_id, &interval)?;
    Ok(Audited {
        value: moved,
        target: format!("org_unit:{unit_id}"),
        summary: json!({
            "from": fmt(from),
            "before": { "parent_unit_id": before.opt_text("parent_unit_id") },
            "after": { "parent_unit_id": new_parent },
        }),
    })
}

/// Liquidates the unit from `from`: its last version closes there. Refused
/// while child units or positions outlive that date — they have to be moved or
/// ended first, otherwise they would silently hang under a dead unit.
pub fn end_unit(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    unit_id: &str,
    from: NaiveDate,
) -> Result<Written<()>> {
    run(pool, ctx, "org.unit.end", |s| end_unit_in(s, unit_id, from))
}

pub(super) fn end_unit_in(
    s: &mut Session<'_>,
    unit_id: &str,
    from: NaiveDate,
) -> Result<Audited<()>> {
    s.ensure_not_backdated(from)?;
    s.require_in_org("org_units", "unit", "unit_id", unit_id)?;
    let pieces = s.unit_pieces(unit_id)?;
    let started = pieces.iter().map(|i| i.from).min();
    if started.is_none_or(|start| from < start && !s.allow_withdraw)
        || !pieces.iter().any(|i| i.to.is_none_or(|end| end > from))
    {
        return Err(E::NotValidAt {
            entity: "unit",
            id: unit_id.to_string(),
            date: fmt(from),
        });
    }
    let after = fmt(from);
    let child_units: i64 = s.tx.query_row(
        "SELECT COUNT(DISTINCT unit_id) FROM org_units \
         WHERE org_id = ?1 AND parent_unit_id = ?2 AND (valid_to IS NULL OR valid_to > ?3)",
        [s.org_id, unit_id, &after],
        |r| r.get(0),
    )?;
    let positions: i64 = s.tx.query_row(
        "SELECT COUNT(DISTINCT position_id) FROM org_positions \
         WHERE org_id = ?1 AND unit_id = ?2 AND (valid_to IS NULL OR valid_to > ?3)",
        [s.org_id, unit_id, &after],
        |r| r.get(0),
    )?;
    if child_units > 0 || positions > 0 {
        return Err(E::UnitNotEmpty {
            unit_id: unit_id.to_string(),
            child_units: child_units as usize,
            positions: positions as usize,
            date: after,
        });
    }
    s.truncate_from(&repl::UNITS, &[("unit_id", unit_id)], from)?;
    s.truncate_from(&repl::DEPUTY_HEADS, &[("unit_id", unit_id)], from)?;
    Ok(Audited {
        value: (),
        target: format!("org_unit:{unit_id}"),
        summary: removal_summary(from, started.is_some_and(|start| from <= start)),
    })
}

/// Sets (or clears, with `None`) the head of the unit from `from` on.
pub fn set_head(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    unit_id: &str,
    head_position_id: Option<&str>,
    from: NaiveDate,
) -> Result<Written<Unit>> {
    run(pool, ctx, "org.unit.head_set", |s| {
        set_head_in(s, unit_id, head_position_id, from)
    })
}

pub(super) fn set_head_in(
    s: &mut Session<'_>,
    unit_id: &str,
    head_position_id: Option<&str>,
    from: NaiveDate,
) -> Result<Audited<Unit>> {
    s.ensure_not_backdated(from)?;
    s.require_in_org("org_units", "unit", "unit_id", unit_id)?;
    let before = s
        .unit_version_at(unit_id, from)?
        .ok_or_else(|| E::NotValidAt {
            entity: "unit",
            id: unit_id.to_string(),
            date: fmt(from),
        })?;
    if let Some(position_id) = head_position_id {
        s.require_in_org("org_positions", "position", "position_id", position_id)?;
        let version = s.require_position_valid_at(position_id, from)?;
        if version.text("unit_id") != unit_id {
            return Err(E::PositionNotInUnit {
                position_id: position_id.to_string(),
                unit_id: unit_id.to_string(),
            });
        }
    }
    let row_id = s
        .replace_from(
            &repl::UNITS,
            &[("unit_id", unit_id)],
            from,
            Some(&[("head_position_id", opt(head_position_id))]),
            None,
            "unit",
            unit_id,
        )?
        .expect("a change always yields a row");
    let unit = unit_row(s, &row_id)?;
    let interval = Interval::new(unit.valid_from, unit.valid_to)?;
    if let Some(position_id) = head_position_id {
        s.require_position_covers(position_id, &interval)?;
        for deputy in s.rows_where(
            &repl::DEPUTY_HEADS,
            &[("unit_id", unit_id), ("position_id", position_id)],
        )? {
            if deputy.interval()?.overlaps(&interval) {
                return Err(E::HeadIsDeputy(position_id.to_string()));
            }
        }
        // A position belongs to one unit, but replicated data is not
        // guaranteed to respect that, and a person leading two units at once
        // would be a silent double approver.
        for other in s.rows_where(&repl::UNITS, &[("head_position_id", position_id)])? {
            if other.text("unit_id") != unit_id && other.interval()?.overlaps(&interval) {
                return Err(E::PositionHeadsAnotherUnit {
                    position_id: position_id.to_string(),
                });
            }
        }
    } else {
        s.warn_if_headless(unit_id, from)?;
    }
    Ok(Audited {
        value: unit,
        target: format!("org_unit:{unit_id}"),
        summary: json!({
            "from": fmt(from),
            "before": { "head_position_id": before.opt_text("head_position_id") },
            "after": { "head_position_id": head_position_id },
        }),
    })
}

/// Replaces the ordered list of deputy heads from `from` until the next
/// planned list (or open-ended), like every other dated change.
pub fn set_deputy_heads(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    unit_id: &str,
    position_ids: &[String],
    from: NaiveDate,
) -> Result<Written<Vec<DeputyHead>>> {
    run(pool, ctx, "org.unit.deputies_set", |s| {
        set_deputy_heads_in(s, unit_id, position_ids, from)
    })
}

pub(super) fn set_deputy_heads_in(
    s: &mut Session<'_>,
    unit_id: &str,
    position_ids: &[String],
    from: NaiveDate,
) -> Result<Audited<Vec<DeputyHead>>> {
    s.ensure_not_backdated(from)?;
    s.require_in_org("org_units", "unit", "unit_id", unit_id)?;
    s.unit_version_at(unit_id, from)?
        .ok_or_else(|| E::NotValidAt {
            entity: "unit",
            id: unit_id.to_string(),
            date: fmt(from),
        })?;
    let existing = s.rows_where(&repl::DEPUTY_HEADS, &[("unit_id", unit_id)])?;
    let mut tail: Option<NaiveDate> = None;
    for row in &existing {
        let start = row.interval()?.from;
        if start > from && tail.is_none_or(|t| start < t) {
            tail = Some(start);
        }
    }
    let versions = s.rows_where(&repl::UNITS, &[("unit_id", unit_id)])?;
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for position_id in position_ids {
        s.require_in_org("org_positions", "position", "position_id", position_id)?;
        let version = s.require_position_valid_at(position_id, from)?;
        if version.text("unit_id") != unit_id {
            return Err(E::PositionNotInUnit {
                position_id: position_id.clone(),
                unit_id: unit_id.to_string(),
            });
        }
        if !seen.insert(position_id.clone()) {
            return Err(E::Duplicate {
                entity: "deputy head",
                name: position_id.clone(),
            });
        }
        let end = match (s.position_end(position_id)?, tail) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let interval = Interval::new(from, end)?;
        // Checked against EVERY version of the unit the deputy row overlaps:
        // a head change scheduled later must not turn a deputy into the head.
        for unit_version in &versions {
            if unit_version.opt_text("head_position_id").as_deref() == Some(position_id.as_str())
                && unit_version.interval()?.overlaps(&interval)
            {
                return Err(E::HeadIsDeputy(position_id.clone()));
            }
        }
        rows.push((position_id.clone(), end));
    }
    let mut before = Vec::new();
    for row in &existing {
        let interval = row.interval()?;
        if interval.contains(from) {
            before.push(row.text("position_id"));
        }
        let id = row.text("id");
        if interval.from == from {
            s.delete(&repl::DEPUTY_HEADS, &id)?;
        } else if interval.from < from && interval.to.is_none_or(|end| end > from) {
            s.set_cols(&repl::DEPUTY_HEADS, &id, &[("valid_to", date_value(from))])?;
        }
    }
    for (order, (position_id, end)) in rows.iter().enumerate() {
        let mut row = s.new_row(&repl::DEPUTY_HEADS);
        row.set("unit_id", text(unit_id));
        row.set("position_id", text(position_id));
        row.set("ord", Value::Integer(order as i64));
        row.set("valid_from", date_value(from));
        row.set("valid_to", opt_date_value(*end));
        s.insert(&repl::DEPUTY_HEADS, &row)?;
    }
    let value = select_where(
        s.tx,
        &repl::DEPUTY_HEADS,
        "org_id = ?1 AND unit_id = ?2 AND valid_from = ?3 ORDER BY ord",
        [s.org_id, unit_id, &fmt(from)],
        map_deputy,
    )?;
    Ok(Audited {
        value,
        target: format!("org_unit:{unit_id}"),
        summary: json!({
            "from": fmt(from),
            "before": before,
            "after": position_ids,
        }),
    })
}

// ---------------------------------------------------------------------------
// Positions and reporting lines
// ---------------------------------------------------------------------------

fn position_row(s: &Session<'_>, row_id: &str) -> Result<Position> {
    one(
        select_where(s.tx, &repl::POSITIONS, "id = ?1", [row_id], map_position)?,
        "position",
        row_id,
    )
}

/// A role must exist in this organization and still be active: a position
/// should not be given a role the administrator has retired.
fn require_role(s: &Session<'_>, role_id: &str) -> Result<()> {
    s.require_in_org("role_catalog", "role", "id", role_id)?;
    let active: bool = s.tx.query_row(
        "SELECT is_active FROM role_catalog WHERE id = ?1",
        [role_id],
        |r| r.get(0),
    )?;
    if active {
        Ok(())
    } else {
        Err(E::InvalidValue {
            field: "role_id",
            reason: "the role is deactivated".to_string(),
        })
    }
}

fn require_can_manage(parent: &Row) -> Result<()> {
    if matches!(parent.0.get("is_staff"), Some(Value::Integer(1))) {
        return Err(E::StaffPositionCannotManage(parent.text("position_id")));
    }
    Ok(())
}

fn bool_value(value: bool) -> Value {
    Value::Integer(i64::from(value))
}

/// A position code is the key the file import matches on, so two positions of
/// one organization must not share it (case-insensitively: a spreadsheet does
/// not keep the case). `except` is the position being renamed.
fn ensure_position_code_free(s: &Session<'_>, code: &str, except: Option<&str>) -> Result<()> {
    let taken: Option<String> =
        s.tx.query_row(
            "SELECT position_id FROM org_positions \
             WHERE org_id = ?1 AND code = ?2 COLLATE NOCASE AND position_id <> ?3 LIMIT 1",
            [s.org_id, code, except.unwrap_or("")],
            |r| r.get(0),
        )
        .optional()?;
    match taken {
        Some(_) => Err(E::Duplicate {
            entity: "position code",
            name: code.to_string(),
        }),
        None => Ok(()),
    }
}

pub fn create_position(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    new: &NewPosition,
) -> Result<Written<Position>> {
    run(pool, ctx, "org.position.create", |s| {
        create_position_in(s, new)
    })
}

pub(super) fn create_position_in(
    s: &mut Session<'_>,
    new: &NewPosition,
) -> Result<Audited<Position>> {
    let name = validate::require_non_empty("name", &new.name)?;
    let code = validate::normalize_optional(new.code.as_deref());
    if let Some(code) = &code {
        ensure_position_code_free(s, code, None)?;
    }
    let interval = Interval::new(new.valid_from, new.valid_to)?;
    s.ensure_not_backdated(new.valid_from)?;
    s.require_in_org("org_units", "unit", "unit_id", &new.unit_id)?;
    s.require_unit_covers(&new.unit_id, &interval)?;
    if let Some(role_id) = &new.role_id {
        require_role(s, role_id)?;
    }
    if let Some(parent_id) = &new.parent_position_id {
        s.require_in_org("org_positions", "position", "position_id", parent_id)?;
        let parent = s.require_position_valid_at(parent_id, new.valid_from)?;
        require_can_manage(&parent)?;
        // A line that outlived its manager would report to nobody.
        s.require_position_covers(parent_id, &interval)?;
    }
    let mut row = s.new_row(&repl::POSITIONS);
    let position_id = s.mint_id();
    row.set("position_id", text(&position_id));
    row.set("unit_id", text(&new.unit_id));
    row.set("name", text(&name));
    row.set("code", opt(code.as_deref()));
    row.set("role_id", opt(new.role_id.as_deref()));
    row.set("is_manager", new.is_manager.map_or(Value::Null, bool_value));
    row.set("is_staff", bool_value(new.is_staff));
    row.set("valid_from", date_value(new.valid_from));
    row.set("valid_to", opt_date_value(new.valid_to));
    s.insert(&repl::POSITIONS, &row)?;
    if let Some(parent_id) = &new.parent_position_id {
        let mut line = s.new_row(&repl::REPORTING_LINES);
        line.set("position_id", text(&position_id));
        line.set("parent_position_id", text(parent_id));
        line.set("kind", text(LineKind::Primary.as_str()));
        line.set("priority", Value::Integer(0));
        line.set("valid_from", date_value(new.valid_from));
        line.set("valid_to", opt_date_value(new.valid_to));
        s.insert(&repl::REPORTING_LINES, &line)?;
    }
    Ok(Audited {
        value: position_row(s, &row.text("id"))?,
        target: format!("org_position:{position_id}"),
        summary: json!({
            "unit_id": new.unit_id,
            "parent_position_id": new.parent_position_id,
            "valid_from": fmt(new.valid_from),
            "valid_to": opt_fmt(new.valid_to),
        }),
    })
}

/// Changes name, role or flags from `from` on; earlier days keep the old
/// values. Returns the version valid from `from`.
pub fn update_position(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    position_id: &str,
    patch: &PositionPatch,
    from: NaiveDate,
) -> Result<Written<Position>> {
    run(pool, ctx, "org.position.update", |s| {
        update_position_in(s, position_id, patch, from)
    })
}

pub(super) fn update_position_in(
    s: &mut Session<'_>,
    position_id: &str,
    patch: &PositionPatch,
    from: NaiveDate,
) -> Result<Audited<Position>> {
    s.ensure_not_backdated(from)?;
    s.require_in_org("org_positions", "position", "position_id", position_id)?;
    let before = s.require_position_valid_at(position_id, from)?;
    let mut changes: Vec<(&'static str, Value)> = Vec::new();
    if let Some(name) = &patch.name {
        changes.push(("name", text(&validate::require_non_empty("name", name)?)));
    }
    if let Some(code) = &patch.code {
        let code = validate::normalize_optional(code.as_deref());
        if let Some(code) = &code {
            ensure_position_code_free(s, code, Some(position_id))?;
        }
        changes.push(("code", opt(code.as_deref())));
    }
    if let Some(role_id) = &patch.role_id {
        if let Some(role_id) = role_id {
            require_role(s, role_id)?;
        }
        changes.push(("role_id", opt(role_id.as_deref())));
    }
    if let Some(is_manager) = patch.is_manager {
        changes.push(("is_manager", is_manager.map_or(Value::Null, bool_value)));
    }
    if let Some(is_staff) = patch.is_staff {
        if is_staff {
            let subordinates: i64 = s.tx.query_row(
                "SELECT COUNT(*) FROM org_reporting_lines WHERE org_id = ?1 AND kind = 'primary' \
                 AND parent_position_id = ?2 AND (valid_to IS NULL OR valid_to > ?3)",
                [s.org_id, position_id, &fmt(from)],
                |r| r.get(0),
            )?;
            if subordinates > 0 {
                return Err(E::StaffPositionCannotManage(position_id.to_string()));
            }
        }
        changes.push(("is_staff", bool_value(is_staff)));
    }
    if changes.is_empty() {
        return Err(E::InvalidValue {
            field: "patch",
            reason: "nothing to change".to_string(),
        });
    }
    let row_id = s
        .replace_from(
            &repl::POSITIONS,
            &[("position_id", position_id)],
            from,
            Some(&changes),
            None,
            "position",
            position_id,
        )?
        .expect("a change always yields a row");
    let after = s.require_row(&repl::POSITIONS, "position", &row_id)?;
    Ok(Audited {
        value: position_row(s, &row_id)?,
        target: format!("org_position:{position_id}"),
        summary: json!({
            "from": fmt(from),
            "changes": diff(&before, &after, &["name", "code", "role_id", "is_manager", "is_staff"]),
        }),
    })
}

pub fn move_position(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    position_id: &str,
    new_parent: Option<&str>,
    from: NaiveDate,
) -> Result<Written<Option<ReportingLine>>> {
    run(pool, ctx, "org.position.move", |s| {
        move_position_in(s, position_id, new_parent, from)
    })
}

pub(super) fn move_position_in(
    s: &mut Session<'_>,
    position_id: &str,
    new_parent: Option<&str>,
    from: NaiveDate,
) -> Result<Audited<Option<ReportingLine>>> {
    s.ensure_not_backdated(from)?;
    s.require_in_org("org_positions", "position", "position_id", position_id)?;
    s.require_position_valid_at(position_id, from)?;
    if let Some(parent_id) = new_parent {
        s.require_in_org("org_positions", "position", "position_id", parent_id)?;
        if parent_id == position_id {
            return Err(E::ReportingCycle { date: fmt(from) });
        }
        let parent = s.require_position_valid_at(parent_id, from)?;
        require_can_manage(&parent)?;
    }
    let filter = [("position_id", position_id), ("kind", "primary")];
    let before = s
        .rows_where(&repl::REPORTING_LINES, &filter)?
        .into_iter()
        .find(|r| r.interval().is_ok_and(|i| i.contains(from)))
        .and_then(|r| r.opt_text("parent_position_id"));
    let mut fresh = s.new_row(&repl::REPORTING_LINES);
    fresh.set("position_id", text(position_id));
    fresh.set("kind", text(LineKind::Primary.as_str()));
    fresh.set("priority", Value::Integer(0));
    let change = new_parent.map(|p| [("parent_position_id", text(p))]);
    let line_id = s.replace_from(
        &repl::REPORTING_LINES,
        &filter,
        from,
        change.as_ref().map(|c| c.as_slice()),
        Some(fresh),
        "position",
        position_id,
    )?;
    let mut line = None;
    if let Some(line_id) = line_id {
        let mut stored = one(
            select_where(
                s.tx,
                &repl::REPORTING_LINES,
                "id = ?1",
                [&line_id],
                map_line,
            )?,
            "reporting line",
            &line_id,
        )?;
        // The line cannot outlive the position it belongs to.
        if let Some(end) = s.position_end(position_id)? {
            if stored.valid_to.is_none_or(|to| to > end) {
                s.set_cols(
                    &repl::REPORTING_LINES,
                    &line_id,
                    &[("valid_to", date_value(end))],
                )?;
                stored.valid_to = Some(end);
            }
        }
        let interval = Interval::new(stored.valid_from, stored.valid_to)?;
        if let Some(parent_id) = new_parent {
            s.require_position_covers(parent_id, &interval)?;
        }
        s.check_primary_line_exclusive(position_id)?;
        s.check_line_cycle(position_id, &interval)?;
        line = Some(stored);
    }
    Ok(Audited {
        value: line,
        target: format!("org_position:{position_id}"),
        summary: json!({
            "from": fmt(from),
            "before": { "parent_position_id": before },
            "after": { "parent_position_id": new_parent },
        }),
    })
}

pub fn set_reporting_line(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    new: &NewLine,
) -> Result<Written<ReportingLine>> {
    run(pool, ctx, "org.reporting_line.set", |s| {
        set_reporting_line_in(s, new)
    })
}

pub(super) fn set_reporting_line_in(
    s: &mut Session<'_>,
    new: &NewLine,
) -> Result<Audited<ReportingLine>> {
    let interval = Interval::new(new.valid_from, new.valid_to)?;
    s.ensure_not_backdated(new.valid_from)?;
    s.require_in_org("org_positions", "position", "position_id", &new.position_id)?;
    s.require_in_org(
        "org_positions",
        "position",
        "position_id",
        &new.parent_position_id,
    )?;
    if new.position_id == new.parent_position_id {
        return Err(E::ReportingCycle {
            date: fmt(new.valid_from),
        });
    }
    s.require_position_covers(&new.position_id, &interval)?;
    s.require_position_covers(&new.parent_position_id, &interval)?;
    let parent = s.require_position_valid_at(&new.parent_position_id, new.valid_from)?;
    if new.kind == LineKind::Primary {
        require_can_manage(&parent)?;
    }
    let mut row = s.new_row(&repl::REPORTING_LINES);
    let line_id = row.text("id");
    row.set("position_id", text(&new.position_id));
    row.set("parent_position_id", text(&new.parent_position_id));
    row.set("kind", text(new.kind.as_str()));
    row.set("priority", Value::Integer(new.priority));
    row.set("valid_from", date_value(new.valid_from));
    row.set("valid_to", opt_date_value(new.valid_to));
    s.insert(&repl::REPORTING_LINES, &row)?;
    match new.kind {
        LineKind::Primary => {
            s.check_primary_line_exclusive(&new.position_id)?;
            s.check_line_cycle(&new.position_id, &interval)?;
        }
        LineKind::Functional => {
            let same_parent = s.rows_where(
                &repl::REPORTING_LINES,
                &[
                    ("position_id", &new.position_id),
                    ("parent_position_id", &new.parent_position_id),
                    ("kind", "functional"),
                ],
            )?;
            let intervals = same_parent
                .iter()
                .map(Row::interval)
                .collect::<Result<Vec<_>>>()?;
            if validate::first_overlap(&intervals).is_some() {
                return Err(E::DuplicateFunctionalLine {
                    position_id: new.position_id.clone(),
                    parent_position_id: new.parent_position_id.clone(),
                });
            }
        }
    }
    Ok(Audited {
        value: one(
            select_where(
                s.tx,
                &repl::REPORTING_LINES,
                "id = ?1",
                [&line_id],
                map_line,
            )?,
            "reporting line",
            &line_id,
        )?,
        target: format!("org_position:{}", new.position_id),
        summary: json!({
            "kind": new.kind.as_str(),
            "parent_position_id": new.parent_position_id,
            "valid_from": fmt(new.valid_from),
            "valid_to": opt_fmt(new.valid_to),
        }),
    })
}

/// Closes a position from `from`, together with everything hanging on it
/// (its lines, deputy-head entries and assignments). Refused while other
/// positions still report to it or it heads a unit. The result and the audit
/// entry list what was ended.
pub fn end_position(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    position_id: &str,
    from: NaiveDate,
) -> Result<Written<Ended>> {
    run(pool, ctx, "org.position.end", |s| {
        end_position_in(s, position_id, from)
    })
}

pub(super) fn end_position_in(
    s: &mut Session<'_>,
    position_id: &str,
    from: NaiveDate,
) -> Result<Audited<Ended>> {
    s.ensure_not_backdated(from)?;
    s.require_in_org("org_positions", "position", "position_id", position_id)?;
    let pieces = s.position_pieces(position_id)?;
    if pieces
        .iter()
        .map(|i| i.from)
        .min()
        .is_none_or(|start| from < start && !s.allow_withdraw)
    {
        return Err(E::InvalidInterval {
            from: pieces.iter().map(|i| fmt(i.from)).min().unwrap_or_default(),
            to: fmt(from),
        });
    }
    let removed = pieces
        .iter()
        .map(|i| i.from)
        .min()
        .is_some_and(|start| from <= start);
    if !pieces.iter().any(|i| i.to.is_none_or(|end| end > from)) {
        return Err(E::NotValidAt {
            entity: "position",
            id: position_id.to_string(),
            date: fmt(from),
        });
    }
    let after = fmt(from);
    let subordinates: i64 = s.tx.query_row(
        "SELECT COUNT(DISTINCT position_id) FROM org_reporting_lines \
         WHERE org_id = ?1 AND kind = 'primary' AND parent_position_id = ?2 \
           AND (valid_to IS NULL OR valid_to > ?3)",
        [s.org_id, position_id, &after],
        |r| r.get(0),
    )?;
    if subordinates > 0 {
        return Err(E::PositionHasSubordinates {
            position_id: position_id.to_string(),
            subordinates: subordinates as usize,
            date: after,
        });
    }
    let heads: i64 = s.tx.query_row(
        "SELECT COUNT(*) FROM org_units WHERE org_id = ?1 AND head_position_id = ?2 \
           AND (valid_to IS NULL OR valid_to > ?3)",
        [s.org_id, position_id, &after],
        |r| r.get(0),
    )?;
    if heads > 0 {
        return Err(E::PositionIsHead(position_id.to_string()));
    }
    let held = s.rows_where(&repl::ASSIGNMENTS, &[("position_id", position_id)])?;
    s.truncate_from(&repl::POSITIONS, &[("position_id", position_id)], from)?;
    let mut ended = Ended::default();
    ended.reporting_lines.extend(s.truncate_from(
        &repl::REPORTING_LINES,
        &[("position_id", position_id)],
        from,
    )?);
    ended.reporting_lines.extend(s.truncate_from(
        &repl::REPORTING_LINES,
        &[("parent_position_id", position_id)],
        from,
    )?);
    ended.deputy_heads.extend(s.truncate_from(
        &repl::DEPUTY_HEADS,
        &[("position_id", position_id)],
        from,
    )?);
    ended.assignments.extend(s.truncate_from(
        &repl::ASSIGNMENTS,
        &[("position_id", position_id)],
        from,
    )?);
    // The people who held it may hold other positions: keep one primary.
    let mut handled = std::collections::HashSet::new();
    for row in &held {
        let (_, subject_id, _) = subject_of(row);
        if handled.insert(subject_id) {
            s.rebalance_primary(row, from)?;
        }
    }
    Ok(Audited {
        target: format!("org_position:{position_id}"),
        summary: {
            let mut summary = removal_summary(from, removed);
            summary["ended"] = json!({
                "reporting_lines": ended.reporting_lines,
                "deputy_heads": ended.deputy_heads,
                "assignments": ended.assignments,
            });
            summary
        },
        value: ended,
    })
}

// ---------------------------------------------------------------------------
// People and assignments
// ---------------------------------------------------------------------------

pub fn create_external_person(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    new: &NewExternalPerson,
) -> Result<Written<ExternalPerson>> {
    run(pool, ctx, "org.external_person.create", |s| {
        create_external_person_in(s, new)
    })
}

pub(super) fn create_external_person_in(
    s: &mut Session<'_>,
    new: &NewExternalPerson,
) -> Result<Audited<ExternalPerson>> {
    let name = validate::require_non_empty("display_name", &new.display_name)?;
    let mut row = s.new_row(&repl::EXTERNAL_PERSONS);
    row.set("display_name", text(&name));
    row.set(
        "email",
        opt(validate::normalize_optional(new.email.as_deref()).as_deref()),
    );
    row.set(
        "note",
        opt(validate::normalize_optional(new.note.as_deref()).as_deref()),
    );
    s.insert(&repl::EXTERNAL_PERSONS, &row)?;
    let id = row.text("id");
    Ok(Audited {
        value: one(
            select_where(
                s.tx,
                &repl::EXTERNAL_PERSONS,
                "id = ?1",
                [&id],
                map_external_person,
            )?,
            "external person",
            &id,
        )?,
        // The name and e-mail are personal data; the audit trail keeps the id only.
        target: format!("org_external_person:{id}"),
        summary: json!({}),
    })
}

fn assignment_by_id(s: &Session<'_>, id: &str) -> Result<Assignment> {
    one(
        select_where(s.tx, &repl::ASSIGNMENTS, "id = ?1", [id], map_assignment)?,
        "assignment",
        id,
    )
}

pub fn assign(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    new: &NewAssignment,
) -> Result<Written<Assignment>> {
    run(pool, ctx, "org.assignment.create", |s| assign_in(s, new))
}

pub(super) fn assign_in(s: &mut Session<'_>, new: &NewAssignment) -> Result<Audited<Assignment>> {
    let interval = Interval::new(new.valid_from, new.valid_to)?;
    validate::validate_share(new.share)?;
    s.ensure_not_backdated(new.valid_from)?;
    s.require_in_org("org_positions", "position", "position_id", &new.position_id)?;
    s.require_position_covers(&new.position_id, &interval)?;
    let (subject_column, subject_id) = match &new.subject {
        Subject::User(id) => {
            s.require_user(id)?;
            ("user_id", id.as_str())
        }
        Subject::External(id) => {
            s.require_in_org("org_external_persons", "external person", "id", id)?;
            ("external_person_id", id.as_str())
        }
    };
    let overlapping_held = {
        let mut any = false;
        for held in s.rows_where(&repl::ASSIGNMENTS, &[(subject_column, subject_id)])? {
            any |= held.interval()?.overlaps(&interval);
        }
        any
    };
    let is_primary = new.is_primary.unwrap_or(!overlapping_held);
    let mut row = s.new_row(&repl::ASSIGNMENTS);
    let assignment_id = row.text("id");
    row.set("position_id", text(&new.position_id));
    row.set(subject_column, text(subject_id));
    row.set("type", text(new.kind.as_str()));
    row.set("share", Value::Real(new.share));
    row.set("is_primary", bool_value(is_primary));
    row.set("valid_from", date_value(new.valid_from));
    row.set("valid_to", opt_date_value(new.valid_to));
    s.insert(&repl::ASSIGNMENTS, &row)?;
    s.check_assignment_invariants(&assignment_id, &interval)?;
    s.rebalance_primary(&row, new.valid_from)?;
    Ok(Audited {
        value: assignment_by_id(s, &assignment_id)?,
        target: format!("org_assignment:{assignment_id}"),
        summary: json!({
            "position_id": new.position_id,
            "subject": { "kind": subject_column, "id": subject_id },
            "type": new.kind.as_str(),
            "share": new.share,
            "is_primary": is_primary,
            "valid_from": fmt(new.valid_from),
            "valid_to": opt_fmt(new.valid_to),
        }),
    })
}

/// Changes type, share or the primary flag from `from` on; the earlier part of
/// the assignment stays as it was.
pub fn update_assignment(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    assignment_id: &str,
    patch: &AssignmentPatch,
    from: NaiveDate,
) -> Result<Written<Assignment>> {
    run(pool, ctx, "org.assignment.update", |s| {
        update_assignment_in(s, assignment_id, patch, from)
    })
}

pub(super) fn update_assignment_in(
    s: &mut Session<'_>,
    assignment_id: &str,
    patch: &AssignmentPatch,
    from: NaiveDate,
) -> Result<Audited<Assignment>> {
    s.ensure_not_backdated(from)?;
    s.require_in_org("org_assignments", "assignment", "id", assignment_id)?;
    let before = s.require_row(&repl::ASSIGNMENTS, "assignment", assignment_id)?;
    if !before.interval()?.contains(from) {
        return Err(E::NotValidAt {
            entity: "assignment",
            id: assignment_id.to_string(),
            date: fmt(from),
        });
    }
    let mut changes: Vec<(&'static str, Value)> = Vec::new();
    if let Some(kind) = patch.kind {
        changes.push(("type", text(kind.as_str())));
    }
    if let Some(share) = patch.share {
        validate::validate_share(share)?;
        changes.push(("share", Value::Real(share)));
    }
    if let Some(is_primary) = patch.is_primary {
        changes.push(("is_primary", bool_value(is_primary)));
    }
    if changes.is_empty() {
        return Err(E::InvalidValue {
            field: "patch",
            reason: "nothing to change".to_string(),
        });
    }
    let (subject_column, subject_id, _) = subject_of(&before);
    let position_id = before.text("position_id");
    let row_id = s
        .replace_from(
            &repl::ASSIGNMENTS,
            &[("position_id", &position_id), (subject_column, &subject_id)],
            from,
            Some(&changes),
            None,
            "assignment",
            assignment_id,
        )?
        .expect("a change always yields a row");
    let changed = assignment_by_id(s, &row_id)?;
    s.check_assignment_invariants(
        &row_id,
        &Interval::new(changed.valid_from, changed.valid_to)?,
    )?;
    let after = s.require_row(&repl::ASSIGNMENTS, "assignment", &row_id)?;
    s.rebalance_primary(&after, from)?;
    Ok(Audited {
        value: changed,
        target: format!("org_assignment:{assignment_id}"),
        summary: json!({
            "from": fmt(from),
            "changes": diff(&before, &after, &["type", "share", "is_primary"]),
        }),
    })
}

/// Ends the assignment from `from`: the person no longer holds the position
/// on that day, the position stays as a vacancy.
pub fn end_assignment(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    assignment_id: &str,
    from: NaiveDate,
) -> Result<Written<()>> {
    run(pool, ctx, "org.assignment.end", |s| {
        end_assignment_in(s, assignment_id, from)
    })
}

pub(super) fn end_assignment_in(
    s: &mut Session<'_>,
    assignment_id: &str,
    from: NaiveDate,
) -> Result<Audited<()>> {
    s.ensure_not_backdated(from)?;
    s.require_in_org("org_assignments", "assignment", "id", assignment_id)?;
    let row = s.require_row(&repl::ASSIGNMENTS, "assignment", assignment_id)?;
    let interval = row.interval()?;
    if from < interval.from && !s.allow_withdraw {
        return Err(E::InvalidInterval {
            from: fmt(interval.from),
            to: fmt(from),
        });
    }
    if interval.to.is_some_and(|end| from >= end) {
        return Err(E::NotValidAt {
            entity: "assignment",
            id: assignment_id.to_string(),
            date: fmt(from),
        });
    }
    let (subject_column, subject_id, _) = subject_of(&row);
    let position_id = row.text("position_id");
    s.truncate_from(
        &repl::ASSIGNMENTS,
        &[("position_id", &position_id), (subject_column, &subject_id)],
        from,
    )?;
    s.rebalance_primary(&row, from)?;
    Ok(Audited {
        value: (),
        target: format!("org_assignment:{assignment_id}"),
        summary: removal_summary(from, from <= interval.from),
    })
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

pub fn set_timezone(
    pool: &DbPool,
    ctx: &WriteCtx<'_>,
    timezone: &str,
) -> Result<Written<Settings>> {
    run(pool, ctx, "org.settings.timezone_set", |s| {
        let timezone = validate::require_non_empty("timezone", timezone)?;
        validate::today_in_zone(&timezone)?;
        let existing = s.get_row(&repl::SETTINGS, ctx.org_id)?;
        let before = timezone_of(s.tx, ctx.org_id)?;
        match existing {
            Some(_) => s.set_cols(
                &repl::SETTINGS,
                ctx.org_id,
                &[("timezone", text(&timezone))],
            )?,
            None => {
                let mut row = s.new_row(&repl::SETTINGS);
                row.set("org_id", text(ctx.org_id));
                row.set("timezone", text(&timezone));
                s.insert(&repl::SETTINGS, &row)?;
            }
        }
        Ok(Audited {
            value: Settings {
                org_id: ctx.org_id.to_string(),
                timezone: timezone.clone(),
            },
            target: format!("org_structure_settings:{}", ctx.org_id),
            summary: json!({ "before": before, "after": timezone }),
        })
    })
}

// ---------------------------------------------------------------------------
// Cleanup when the things the structure points at go away
// ---------------------------------------------------------------------------

/// Ends every assignment `user_id` holds in `org_id` as of today in the
/// organization's timezone, inside the caller's transaction. History is kept:
/// a running assignment is closed today, and only assignments that would have
/// started today or later (they never held a day) are removed. Returns the ids
/// of the rows it touched; writes nothing (not even an audit entry) when the
/// user holds nothing.
///
/// The peers need no call of their own: the captures recorded here replicate.
pub fn end_user_assignments_tx(
    tx: &Transaction<'_>,
    org_id: &str,
    user_id: &str,
    actor: Option<&str>,
) -> Result<Vec<String>> {
    let today = validate::today_in_zone(&timezone_of(tx, org_id)?)?;
    let running: i64 = tx.query_row(
        "SELECT COUNT(*) FROM org_assignments \
         WHERE org_id = ?1 AND user_id = ?2 AND (valid_to IS NULL OR valid_to > ?3)",
        [org_id, user_id, &fmt(today)],
        |r| r.get(0),
    )?;
    if running == 0 {
        return Ok(Vec::new());
    }
    let written = run_in_tx(
        tx,
        org_id,
        actor,
        false,
        "org.assignment.end_for_user",
        |s| {
            let mut positions: Vec<String> = s
                .rows_where(&repl::ASSIGNMENTS, &[("user_id", user_id)])?
                .iter()
                .map(|r| r.text("position_id"))
                .collect();
            positions.sort();
            positions.dedup();
            let held = s.rows_where(&repl::ASSIGNMENTS, &[("user_id", user_id)])?;
            let mut ended = Vec::new();
            for position_id in positions {
                ended.extend(s.truncate_from(
                    &repl::ASSIGNMENTS,
                    &[("position_id", &position_id), ("user_id", user_id)],
                    s.today,
                )?);
            }
            // Keeps "one primary while anything is held" if a caller ever ends
            // only part of a person's positions.
            if let Some(row) = held.first() {
                s.rebalance_primary(row, s.today)?;
            }
            Ok(Audited {
                target: format!("user:{user_id}"),
                summary: json!({ "from": fmt(s.today), "ended": ended }),
                value: ended,
            })
        },
    )?;
    Ok(written.value)
}

/// `end_user_assignments_tx` in every organization the user holds something in
/// (the account is being deleted).
pub fn end_user_assignments_everywhere_tx(
    tx: &Transaction<'_>,
    user_id: &str,
    actor: Option<&str>,
) -> Result<Vec<String>> {
    let orgs: Vec<String> = tx
        .prepare("SELECT DISTINCT org_id FROM org_assignments WHERE user_id = ?1")?
        .query_map([user_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut ended = Vec::new();
    for org_id in orgs {
        ended.extend(end_user_assignments_tx(tx, &org_id, user_id, actor)?);
    }
    Ok(ended)
}

/// Deletes every org-structure row of an organization that is being deleted,
/// with a tombstone capture per row so the peers drop them too. Unlike a
/// removed person, a deleted organization has no history worth keeping.
pub fn purge_org_structure_tx(tx: &Transaction<'_>, org_id: &str) -> Result<usize> {
    let mut removed = 0;
    for spec in repl::all_specs() {
        let ids: Vec<String> = tx
            .prepare(&format!(
                "SELECT {} FROM {} WHERE org_id = ?1",
                spec.pk, spec.table
            ))?
            .query_map([org_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        for id in &ids {
            tx.execute(
                &format!("DELETE FROM {} WHERE {} = ?1", spec.table, spec.pk),
                [id],
            )?;
            repl::capture_row(tx, spec, org_id, id, SqlWriteAction::Delete, None)?;
        }
        removed += ids.len();
    }
    if removed > 0 {
        crate::db::repository::log_audit_tx(
            tx,
            None,
            None,
            "org.structure.purge",
            Some(&format!("org:{org_id}")),
            Some(&json!({ "org_id": org_id, "rows": removed }).to_string()),
            None,
            None,
        )?;
    }
    Ok(removed)
}

// ---------------------------------------------------------------------------
// Integrity report
// ---------------------------------------------------------------------------

fn load_rows(
    conn: &Connection,
    org_id: &str,
    spec: &TableSpec,
    at: Option<NaiveDate>,
) -> Result<Vec<Row>> {
    let sql = format!(
        "SELECT {} FROM {} WHERE org_id = ?1 ORDER BY {}",
        spec.columns.join(", "),
        spec.table,
        spec.pk
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map([org_id], |r| read_row(spec, r))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    match at {
        None => Ok(rows),
        Some(day) => {
            let mut kept = Vec::new();
            for row in rows {
                if row.interval()?.contains(day) {
                    kept.push(row);
                }
            }
            Ok(kept)
        }
    }
}

fn group_by(rows: &[Row], key: impl Fn(&Row) -> String) -> BTreeMap<String, Vec<&Row>> {
    let mut groups: BTreeMap<String, Vec<&Row>> = BTreeMap::new();
    for row in rows {
        groups.entry(key(row)).or_default().push(row);
    }
    groups
}

fn intervals_of(rows: &[&Row]) -> Result<Vec<Interval>> {
    rows.iter().map(|r| r.interval()).collect()
}

/// Read-only check of the invariants the write path enforces. Two nodes that
/// edited the structure offline both passed their own checks, and the merge can
/// still break a rule; nothing rejects a replicated row, so this is where such
/// a conflict becomes visible. `at = Some(day)` looks at that day only,
/// `None` at the whole timeline. Returns nothing for a consistent structure.
pub fn integrity_report(
    pool: &DbPool,
    org_id: &str,
    at: Option<NaiveDate>,
) -> Result<Vec<Violation>> {
    let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
    let units = load_rows(&conn, org_id, &repl::UNITS, at)?;
    let positions = load_rows(&conn, org_id, &repl::POSITIONS, at)?;
    let lines = load_rows(&conn, org_id, &repl::REPORTING_LINES, at)?;
    let deputies = load_rows(&conn, org_id, &repl::DEPUTY_HEADS, at)?;
    let assignments = load_rows(&conn, org_id, &repl::ASSIGNMENTS, at)?;
    let unit_types = load_rows(&conn, org_id, &repl::UNIT_TYPES, None)?;
    let externals = load_rows(&conn, org_id, &repl::EXTERNAL_PERSONS, None)?;
    let members: std::collections::HashSet<String> = conn
        .prepare("SELECT user_id FROM org_memberships WHERE org_id = ?1")?
        .query_map([org_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let roles: std::collections::HashSet<String> = conn
        .prepare("SELECT id FROM role_catalog WHERE org_id = ?1")?
        .query_map([org_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;

    let mut found: Vec<Violation> = Vec::new();

    // Versions of one identity must not overlap.
    for (entity, rows, key) in [
        ("unit", &units, "unit_id"),
        ("position", &positions, "position_id"),
    ] {
        for (id, group) in group_by(rows, |r| r.text(key)) {
            if let Some(from) = validate::first_overlap(&intervals_of(&group)?) {
                found.push(Violation::VersionsOverlap {
                    entity: entity.to_string(),
                    id,
                    from,
                });
            }
        }
    }

    // One primary line per position per day.
    let primary_lines: Vec<Row> = lines
        .iter()
        .filter(|r| r.text("kind") == LineKind::Primary.as_str())
        .cloned()
        .collect();
    for (position_id, group) in group_by(&primary_lines, |r| r.text("position_id")) {
        if let Some(from) = validate::first_overlap(&intervals_of(&group)?) {
            found.push(Violation::PrimaryLinesOverlap { position_id, from });
        }
    }

    // One primary assignment per person per day.
    let primary_assignments: Vec<Row> = assignments
        .iter()
        .filter(|r| matches!(r.0.get("is_primary"), Some(Value::Integer(1))))
        .cloned()
        .collect();
    for (_, group) in group_by(&primary_assignments, |r| subject_of(r).1) {
        if let Some(from) = validate::first_overlap(&intervals_of(&group)?) {
            found.push(Violation::PrimaryAssignmentsOverlap {
                subject: subject_of(group[0]).2,
                from,
            });
        }
    }

    // A position heads at most one unit, and is not its own unit's deputy.
    let heads: Vec<&Row> = units
        .iter()
        .filter(|r| r.opt_text("head_position_id").is_some())
        .collect();
    for (position_id, group) in group_by_refs(&heads, |r| r.text("head_position_id")) {
        for (i, a) in group.iter().enumerate() {
            for b in &group[i + 1..] {
                if a.text("unit_id") != b.text("unit_id") && a.interval()?.overlaps(&b.interval()?)
                {
                    found.push(Violation::PositionHeadsMultipleUnits {
                        position_id: position_id.clone(),
                        from: a.interval()?.from.max(b.interval()?.from),
                    });
                }
            }
        }
        for deputy in deputies
            .iter()
            .filter(|d| d.text("position_id") == position_id)
        {
            for head in group
                .iter()
                .filter(|h| h.text("unit_id") == deputy.text("unit_id"))
            {
                if head.interval()?.overlaps(&deputy.interval()?) {
                    found.push(Violation::DeputyIsHead {
                        unit_id: deputy.text("unit_id"),
                        position_id: position_id.clone(),
                        from: head.interval()?.from.max(deputy.interval()?.from),
                    });
                }
            }
        }
    }

    // No loops, on any day of the window.
    let position_edges = primary_lines
        .iter()
        .map(|r| {
            Ok(Edge {
                child: r.text("position_id"),
                parent: r.text("parent_position_id"),
                interval: r.interval()?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let unit_edges = units
        .iter()
        .filter(|r| r.opt_text("parent_unit_id").is_some())
        .map(|r| {
            Ok(Edge {
                child: r.text("unit_id"),
                parent: r.text("parent_unit_id"),
                interval: r.interval()?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    for (edges, is_unit) in [(&position_edges, false), (&unit_edges, true)] {
        let mut children: Vec<&str> = edges.iter().map(|e| e.child.as_str()).collect();
        children.sort();
        children.dedup();
        for child in children {
            let within = match at {
                Some(day) => Interval::new(day, day.succ_opt())?,
                None => Interval {
                    from: edges
                        .iter()
                        .map(|e| e.interval.from)
                        .min()
                        .expect("edges exist"),
                    to: None,
                },
            };
            if let Some(from) = validate::find_cycle(edges, child, &within) {
                let members =
                    validate::cycle_through(edges.iter(), child, from).unwrap_or_default();
                // A loop is reported once, under its smallest member.
                if members.iter().min().map(String::as_str) == Some(child) {
                    found.push(if is_unit {
                        Violation::UnitCycle {
                            unit_id: child.to_string(),
                            from,
                        }
                    } else {
                        Violation::PositionCycle {
                            position_id: child.to_string(),
                            from,
                        }
                    });
                }
            }
        }
    }

    // References that lead nowhere.
    let ids = |rows: &[Row], key: &str| -> std::collections::HashSet<String> {
        rows.iter().map(|r| r.text(key)).collect()
    };
    let unit_ids = ids(&units, "unit_id");
    let position_ids = ids(&positions, "position_id");
    let type_ids = ids(&unit_types, "id");
    let external_ids = ids(&externals, "id");
    let mut dangling = |entity: &str,
                        id: String,
                        field: &str,
                        missing: Option<String>,
                        known: &dyn Fn(&str) -> bool| {
        if let Some(missing) = missing {
            if !known(&missing) {
                found.push(Violation::DanglingReference {
                    entity: entity.to_string(),
                    id,
                    field: field.to_string(),
                    missing_id: missing,
                });
            }
        }
    };
    for r in &units {
        let id = r.text("id");
        dangling(
            "unit",
            id.clone(),
            "parent_unit_id",
            r.opt_text("parent_unit_id"),
            &|m| unit_ids.contains(m),
        );
        dangling("unit", id.clone(), "type_id", r.opt_text("type_id"), &|m| {
            type_ids.contains(m)
        });
        dangling(
            "unit",
            id,
            "head_position_id",
            r.opt_text("head_position_id"),
            &|m| position_ids.contains(m),
        );
    }
    for r in &positions {
        let id = r.text("id");
        dangling(
            "position",
            id.clone(),
            "unit_id",
            r.opt_text("unit_id"),
            &|m| unit_ids.contains(m),
        );
        dangling("position", id, "role_id", r.opt_text("role_id"), &|m| {
            roles.contains(m)
        });
    }
    for r in &lines {
        let id = r.text("id");
        dangling(
            "reporting_line",
            id.clone(),
            "position_id",
            r.opt_text("position_id"),
            &|m| position_ids.contains(m),
        );
        dangling(
            "reporting_line",
            id,
            "parent_position_id",
            r.opt_text("parent_position_id"),
            &|m| position_ids.contains(m),
        );
    }
    for r in &deputies {
        let id = r.text("id");
        dangling(
            "deputy_head",
            id.clone(),
            "unit_id",
            r.opt_text("unit_id"),
            &|m| unit_ids.contains(m),
        );
        dangling(
            "deputy_head",
            id,
            "position_id",
            r.opt_text("position_id"),
            &|m| position_ids.contains(m),
        );
    }
    for r in &assignments {
        let id = r.text("id");
        dangling(
            "assignment",
            id.clone(),
            "position_id",
            r.opt_text("position_id"),
            &|m| position_ids.contains(m),
        );
        dangling(
            "assignment",
            id.clone(),
            "user_id",
            r.opt_text("user_id"),
            &|m| members.contains(m),
        );
        dangling(
            "assignment",
            id,
            "external_person_id",
            r.opt_text("external_person_id"),
            &|m| external_ids.contains(m),
        );
    }

    found.sort_by_cached_key(|v| serde_json::to_string(v).unwrap_or_default());
    Ok(found)
}

fn group_by_refs<'a>(
    rows: &[&'a Row],
    key: impl Fn(&Row) -> String,
) -> BTreeMap<String, Vec<&'a Row>> {
    let mut groups: BTreeMap<String, Vec<&Row>> = BTreeMap::new();
    for row in rows {
        groups.entry(key(row)).or_default().push(row);
    }
    groups
}

// Deputies and absences (WP9). A child module so it reaches the write session.
pub(super) mod cover;
pub use cover::{
    add_absence, delete_absence, end_deputy, ensure_may_write_deputy, set_deputy, update_absence,
    update_deputy, AbsencePatch, Actor, DeputyPatch, NewAbsence, NewDeputy,
};
