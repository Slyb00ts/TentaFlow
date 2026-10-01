//! From the rows of a file and the structure as it is to the list of
//! operations that turns one into the other, and every reason it cannot.
//!
//! The plan is pure of writes: it reads snapshots and decides. `apply` runs
//! the operations through the same repository functions a manual edit uses,
//! so the plan only has to be right about what to ask for, and about the
//! mistakes in the file that the repository could name only one at a time.
//!
//! Each fault is an `Issue` and the link that carries it is dropped from the
//! model, so the operations of the rest of the file are still planned and the
//! preview can show them.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use chrono::NaiveDate;
use rusqlite::Connection;

use super::codes::{self, key};
use super::columns::Column;
use super::people::{closest_name, Directory, Lookup};
use super::report::{EndedItem, FieldChange, Issue, IssueKind};
use super::rows::Row;
use super::Mode;
use crate::services::org_structure::error::Result;
use crate::services::org_structure::query::Snapshot;
use crate::services::org_structure::types::{Assignment, Position, Subject, Unit, UnitType};
use crate::services::org_structure::validate;

// ---------------------------------------------------------------------------
// The structure to compare with
// ---------------------------------------------------------------------------

/// One snapshot with the indexes the lookups by code need.
pub struct Indexed {
    pub snap: Snapshot,
    unit_stored: HashMap<String, Vec<usize>>,
    unit_derived: HashMap<String, Vec<usize>>,
    position_stored: HashMap<String, Vec<usize>>,
    position_derived: HashMap<String, Vec<usize>>,
}

impl Indexed {
    fn new(snap: Snapshot) -> Self {
        let mut unit_stored: HashMap<String, Vec<usize>> = HashMap::new();
        let mut unit_derived: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, unit) in snap.units.iter().enumerate() {
            match &unit.code {
                Some(code) => unit_stored.entry(key(code)).or_default().push(index),
                None => {
                    if let Some(derived) = codes::derived_unit_key(unit) {
                        unit_derived.entry(derived).or_default().push(index);
                    }
                }
            }
        }
        let mut position_stored: HashMap<String, Vec<usize>> = HashMap::new();
        let mut position_derived: HashMap<String, Vec<usize>> = HashMap::new();
        for (index, position) in snap.positions.iter().enumerate() {
            match &position.code {
                Some(code) => position_stored.entry(key(code)).or_default().push(index),
                None => {
                    if let Some(derived) = codes::derived_position_key(position) {
                        position_derived.entry(derived).or_default().push(index);
                    }
                }
            }
        }
        Self {
            snap,
            unit_stored,
            unit_derived,
            position_stored,
            position_derived,
        }
    }

    /// A stored code wins over a derived one; several matches are returned
    /// so the caller can call the code ambiguous.
    fn find_units(&self, code_key: &str) -> Vec<&Unit> {
        let hits = self
            .unit_stored
            .get(code_key)
            .or_else(|| self.unit_derived.get(code_key));
        hits.into_iter()
            .flatten()
            .map(|i| &self.snap.units[*i])
            .collect()
    }

    fn find_positions(&self, code_key: &str) -> Vec<&Position> {
        let hits = self
            .position_stored
            .get(code_key)
            .or_else(|| self.position_derived.get(code_key));
        hits.into_iter()
            .flatten()
            .map(|i| &self.snap.positions[*i])
            .collect()
    }
}

/// Snapshots by day, loaded once: a file mostly needs today's, a planned
/// reorganization a few more.
pub struct Snapshots<'c> {
    conn: &'c Connection,
    org_id: &'c str,
    cache: HashMap<NaiveDate, Rc<Indexed>>,
}

impl<'c> Snapshots<'c> {
    pub fn new(conn: &'c Connection, org_id: &'c str) -> Self {
        Self {
            conn,
            org_id,
            cache: HashMap::new(),
        }
    }

    fn at(&mut self, day: NaiveDate) -> Result<Rc<Indexed>> {
        if let Some(found) = self.cache.get(&day) {
            return Ok(found.clone());
        }
        let indexed = Rc::new(Indexed::new(Snapshot::load(self.conn, self.org_id, day)?));
        self.cache.insert(day, indexed.clone());
        Ok(indexed)
    }
}

/// Codes a unit or a position has EVER had, on any day. A code that is not
/// valid on the day of the row but belonged to something else is not free.
pub struct EverCodes {
    units: HashSet<String>,
    positions: HashSet<String>,
}

impl EverCodes {
    pub fn load(conn: &Connection, org_id: &str) -> Result<Self> {
        let read = |table: &str| -> Result<HashSet<String>> {
            let mut stmt = conn.prepare(&format!(
                "SELECT DISTINCT lower(code) FROM {table} WHERE org_id = ?1 AND code IS NOT NULL"
            ))?;
            let codes = stmt
                .query_map([org_id], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<HashSet<_>>>()?;
            Ok(codes)
        };
        Ok(Self {
            units: read("org_units")?,
            positions: read("org_positions")?,
        })
    }
}

// ---------------------------------------------------------------------------
// Operations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum UnitRef {
    Existing(String),
    /// A unit this file creates, by its code key.
    New(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PositionRef {
    Existing(String),
    New(String),
}

#[derive(Debug, Clone)]
pub enum OpKind {
    CreateUnit {
        key: String,
        name: String,
        code: String,
        type_id: Option<String>,
        parent: Option<UnitRef>,
        from: NaiveDate,
    },
    UpdateUnit {
        unit_id: String,
        name: Option<String>,
        type_id: Option<Option<String>>,
        from: NaiveDate,
    },
    MoveUnit {
        unit_id: String,
        parent: Option<UnitRef>,
        from: NaiveDate,
    },
    CreatePosition {
        key: String,
        unit: UnitRef,
        name: String,
        code: String,
        staff: bool,
        manager: Option<PositionRef>,
        from: NaiveDate,
    },
    UpdatePosition {
        position_id: String,
        name: Option<String>,
        staff: Option<bool>,
        from: NaiveDate,
    },
    MovePosition {
        position_id: String,
        manager: Option<PositionRef>,
        from: NaiveDate,
    },
    SetHead {
        unit: UnitRef,
        position: Option<PositionRef>,
        from: NaiveDate,
    },
    SetDeputies {
        unit: UnitRef,
        positions: Vec<PositionRef>,
        from: NaiveDate,
    },
    DemotePrimary {
        assignment_id: String,
        from: NaiveDate,
    },
    Assign {
        position: PositionRef,
        subject: Subject,
        share: f64,
        primary: Option<bool>,
        from: NaiveDate,
    },
    UpdateAssignment {
        assignment_id: String,
        share: Option<f64>,
        primary: Option<bool>,
        from: NaiveDate,
    },
    EndAssignment {
        assignment_id: String,
        from: NaiveDate,
    },
    EndPosition {
        position_id: String,
        from: NaiveDate,
    },
    EndUnit {
        unit_id: String,
        from: NaiveDate,
    },
}

impl OpKind {
    pub fn date(&self) -> NaiveDate {
        match self {
            Self::CreateUnit { from, .. }
            | Self::UpdateUnit { from, .. }
            | Self::MoveUnit { from, .. }
            | Self::CreatePosition { from, .. }
            | Self::UpdatePosition { from, .. }
            | Self::MovePosition { from, .. }
            | Self::SetHead { from, .. }
            | Self::SetDeputies { from, .. }
            | Self::DemotePrimary { from, .. }
            | Self::Assign { from, .. }
            | Self::UpdateAssignment { from, .. }
            | Self::EndAssignment { from, .. }
            | Self::EndPosition { from, .. }
            | Self::EndUnit { from, .. } => *from,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Op {
    /// The row a failure of this operation is reported on.
    pub row: u32,
    /// The operation makes something that did not exist.
    pub created: bool,
    pub changes: Vec<FieldChange>,
    pub kind: OpKind,
}

/// What a row of the file stands for, to put ids on its outcome.
#[derive(Debug, Clone)]
pub struct RowInfo {
    pub number: u32,
    pub unit_code: Option<String>,
    pub position_code: Option<String>,
    pub person: Option<String>,
    pub unit: Option<UnitRef>,
    pub position: Option<PositionRef>,
}

pub struct Plan {
    pub ops: Vec<Op>,
    pub errors: Vec<Issue>,
    pub warnings: Vec<Issue>,
    pub rows: Vec<RowInfo>,
    /// The latest day something takes effect, or the import day.
    pub preview_at: NaiveDate,
    /// Units and people the file names, with a row to attach a warning to.
    pub units: Vec<(u32, UnitRef)>,
    pub subjects: Vec<(u32, Subject)>,
    /// What a replace ends, for the administrator to confirm.
    pub ended: Vec<EndedItem>,
    /// Holders each ended position takes with it (counted as ended assignments).
    pub cascaded_holders: HashMap<String, usize>,
}

pub struct Settings {
    pub mode: Mode,
    /// The default day of a row, and the day the file is compared against.
    pub as_of: NaiveDate,
    pub today: NaiveDate,
    pub confirm_backdated: bool,
}

// ---------------------------------------------------------------------------
// The model of the file
// ---------------------------------------------------------------------------

/// What a link of the file to another entity came to.
#[derive(Debug, Clone, PartialEq)]
enum Link<T> {
    /// The cell is empty: the file says nothing.
    Unstated,
    /// The cell is wrong; the fault is reported and the link ignored.
    Dropped,
    Set(T),
}

#[derive(Debug, Clone)]
struct Attr<T> {
    value: T,
    row: u32,
}

struct UnitDef {
    key: String,
    /// As typed on its first row.
    code: String,
    rows: Vec<u32>,
    days: Vec<NaiveDate>,
    name: Option<Attr<String>>,
    parent: Option<Attr<String>>,
    type_name: Option<Attr<String>>,
    type_id: Link<String>,
    parent_ref: Link<UnitRef>,
    existing: Option<String>,
    usable: bool,
    create_day: NaiveDate,
}

struct Holder {
    row: u32,
    cell: String,
    column: Column,
    share: Option<f64>,
    primary: Option<bool>,
    day: NaiveDate,
    subject: Option<Subject>,
}

struct PositionDef {
    key: String,
    code: String,
    unit_key: String,
    rows: Vec<u32>,
    days: Vec<NaiveDate>,
    name: Option<Attr<String>>,
    staff: Option<Attr<bool>>,
    manager: Option<Attr<String>>,
    deputy: Option<Attr<u32>>,
    head: Option<u32>,
    holders: Vec<Holder>,
    manager_ref: Link<PositionRef>,
    existing: Option<String>,
    usable: bool,
    create_day: NaiveDate,
}

/// A holder row with its subject resolved: row, cell, share, primary flag, day, subject.
type ResolvedHolder = (u32, String, Option<f64>, Option<bool>, NaiveDate, Subject);

fn compared_on(row_day: NaiveDate, as_of: NaiveDate) -> NaiveDate {
    row_day.max(as_of)
}

fn merge_attr<T: Clone>(
    slot: &mut Option<Attr<T>>,
    value: Option<T>,
    row: u32,
    column: Column,
    same: impl Fn(&T, &T) -> bool,
    show: impl Fn(&T) -> String,
    issues: &mut Vec<Issue>,
) {
    let Some(value) = value else { return };
    match slot {
        None => *slot = Some(Attr { value, row }),
        Some(first) if same(&first.value, &value) => {}
        Some(first) => issues.push(
            Issue::new(
                row,
                IssueKind::ConflictingValues,
                format!(
                    "row {} says '{}', row {row} says '{}'",
                    first.row,
                    show(&first.value),
                    show(&value)
                ),
            )
            .column(column)
            .value(show(&value))
            .rows([first.row, row]),
        ),
    }
}

fn same_text(a: &String, b: &String) -> bool {
    a == b
}

// `&String` is the `&T` of `merge_attr` for `T = String`; a `&str` signature would not fit `Fn(&T)`.
#[allow(clippy::ptr_arg)]
fn same_key(a: &String, b: &String) -> bool {
    key(a) == key(b)
}

#[allow(clippy::ptr_arg)]
fn show_string(value: &String) -> String {
    value.clone()
}

fn yes_no(value: bool) -> String {
    if value { "tak" } else { "nie" }.to_string()
}

// ---------------------------------------------------------------------------
// Building the plan
// ---------------------------------------------------------------------------

pub struct Inputs<'a> {
    pub rows: &'a [Row],
    pub settings: &'a Settings,
    pub directory: &'a Directory,
    pub types: &'a [UnitType],
    pub ever: &'a EverCodes,
}

pub fn build(inputs: &Inputs<'_>, snapshots: &mut Snapshots<'_>) -> Result<Plan> {
    Planner::new(inputs, snapshots).run()
}

struct Planner<'a, 'c> {
    inputs: &'a Inputs<'a>,
    snapshots: &'a mut Snapshots<'c>,
    issues: Vec<Issue>,
    warnings: Vec<Issue>,
    units: Vec<UnitDef>,
    unit_ix: HashMap<String, usize>,
    positions: Vec<PositionDef>,
    position_ix: HashMap<String, usize>,
    /// Codes of rows that did not parse: their entities exist, they are just
    /// not usable, so references to them are not errors of their own.
    declared_units: HashSet<String>,
    declared_positions: HashSet<String>,
    row_info: Vec<RowInfo>,
    ended: Vec<EndedItem>,
    cascaded_holders: HashMap<String, usize>,
}

impl<'a, 'c> Planner<'a, 'c> {
    fn new(inputs: &'a Inputs<'a>, snapshots: &'a mut Snapshots<'c>) -> Self {
        Self {
            inputs,
            snapshots,
            issues: Vec::new(),
            warnings: Vec::new(),
            units: Vec::new(),
            unit_ix: HashMap::new(),
            positions: Vec::new(),
            position_ix: HashMap::new(),
            declared_units: HashSet::new(),
            declared_positions: HashSet::new(),
            row_info: Vec::new(),
            ended: Vec::new(),
            cascaded_holders: HashMap::new(),
        }
    }

    fn as_of(&self) -> NaiveDate {
        self.inputs.settings.as_of
    }

    fn run(mut self) -> Result<Plan> {
        self.collect_rows();
        self.resolve_people();
        self.resolve_types();
        self.match_existing()?;
        self.resolve_links()?;
        self.check_heads_and_deputies();
        self.propagate_days();
        let mut ops = self.operations()?;
        if self.inputs.settings.mode == Mode::Replace {
            let ends = self.replace_ends(&ops)?;
            ops.extend(ends);
        }
        self.check_backdated(&ops);
        let preview_at = ops
            .iter()
            .map(|op| op.kind.date())
            .max()
            .unwrap_or(self.as_of())
            .max(self.as_of());
        let units = self
            .units
            .iter()
            .filter(|u| u.usable)
            .map(|u| (u.rows[0], self.unit_ref(u)))
            .collect();
        let subjects = self
            .positions
            .iter()
            .flat_map(|p| p.holders.iter())
            .filter_map(|h| h.subject.clone().map(|s| (h.row, s)))
            .collect();
        for row in &mut self.row_info {
            row.unit = self
                .unit_ix
                .get(&key(row.unit_code.as_deref().unwrap_or("")))
                .map(|i| &self.units[*i])
                .filter(|u| u.usable)
                .map(|u| {
                    u.existing
                        .clone()
                        .map(UnitRef::Existing)
                        .unwrap_or_else(|| UnitRef::New(u.key.clone()))
                });
            row.position = row
                .position_code
                .as_deref()
                .and_then(|code| self.position_ix.get(&key(code)))
                .map(|i| &self.positions[*i])
                .filter(|p| p.usable)
                .map(|p| {
                    p.existing
                        .clone()
                        .map(PositionRef::Existing)
                        .unwrap_or_else(|| PositionRef::New(p.key.clone()))
                });
        }
        Ok(Plan {
            ops,
            errors: self.issues,
            warnings: self.warnings,
            rows: self.row_info,
            preview_at,
            units,
            subjects,
            ended: self.ended,
            cascaded_holders: self.cascaded_holders,
        })
    }

    fn unit_ref(&self, def: &UnitDef) -> UnitRef {
        match &def.existing {
            Some(id) => UnitRef::Existing(id.clone()),
            None => UnitRef::New(def.key.clone()),
        }
    }

    fn position_ref(&self, def: &PositionDef) -> PositionRef {
        match &def.existing {
            Some(id) => PositionRef::Existing(id.clone()),
            None => PositionRef::New(def.key.clone()),
        }
    }

    // -- 1. rows into units and positions -----------------------------------

    fn collect_rows(&mut self) {
        let as_of = self.as_of();
        for row in self.inputs.rows {
            self.row_info.push(RowInfo {
                number: row.number,
                unit_code: row.unit_code.clone(),
                position_code: row.position_code.clone(),
                person: row.person.clone(),
                unit: None,
                position: None,
            });
            let Some(unit_code) = &row.unit_code else {
                if !row.broken {
                    self.issues.push(
                        Issue::new(
                            row.number,
                            IssueKind::MissingUnitCode,
                            "every row needs the code of its unit",
                        )
                        .column(Column::UnitCode),
                    );
                }
                continue;
            };
            let unit_key = key(unit_code);
            if row.broken {
                self.declared_units.insert(unit_key);
                if let Some(code) = &row.position_code {
                    self.declared_positions.insert(key(code));
                }
                continue;
            }
            let day = row.from.unwrap_or(as_of);
            let ui = match self.unit_ix.get(&unit_key) {
                Some(i) => *i,
                None => {
                    self.units.push(UnitDef {
                        key: unit_key.clone(),
                        code: unit_code.clone(),
                        rows: Vec::new(),
                        days: Vec::new(),
                        name: None,
                        parent: None,
                        type_name: None,
                        type_id: Link::Unstated,
                        parent_ref: Link::Unstated,
                        existing: None,
                        usable: true,
                        create_day: day,
                    });
                    self.unit_ix.insert(unit_key.clone(), self.units.len() - 1);
                    self.units.len() - 1
                }
            };
            let unit = &mut self.units[ui];
            unit.rows.push(row.number);
            unit.days.push(day);
            merge_attr(
                &mut unit.name,
                row.unit_name.clone(),
                row.number,
                Column::UnitName,
                same_text,
                show_string,
                &mut self.issues,
            );
            merge_attr(
                &mut unit.parent,
                row.parent_code.clone(),
                row.number,
                Column::ParentCode,
                same_key,
                show_string,
                &mut self.issues,
            );
            merge_attr(
                &mut unit.type_name,
                row.unit_type.clone(),
                row.number,
                Column::UnitType,
                |a: &String, b: &String| key(a) == key(b),
                show_string,
                &mut self.issues,
            );

            if !row.is_position_row() {
                continue;
            }
            let Some(position_code) = &row.position_code else {
                self.issues.push(
                    Issue::new(
                        row.number,
                        IssueKind::MissingPositionCode,
                        "a row that describes a position needs the code of the position",
                    )
                    .column(Column::PositionCode),
                );
                continue;
            };
            let position_key = key(position_code);
            let pi = match self.position_ix.get(&position_key) {
                Some(i) => {
                    if self.positions[*i].unit_key != unit_key {
                        self.issues.push(
                            Issue::new(
                                row.number,
                                IssueKind::PositionUnitMismatch,
                                format!(
                                    "position '{position_code}' is in unit '{}' on row {}",
                                    self.positions[*i].unit_key, self.positions[*i].rows[0]
                                ),
                            )
                            .column(Column::UnitCode)
                            .value(unit_code.clone())
                            .rows([self.positions[*i].rows[0], row.number]),
                        );
                        continue;
                    }
                    *i
                }
                None => {
                    self.positions.push(PositionDef {
                        key: position_key.clone(),
                        code: position_code.clone(),
                        unit_key: unit_key.clone(),
                        rows: Vec::new(),
                        days: Vec::new(),
                        name: None,
                        staff: None,
                        manager: None,
                        deputy: None,
                        head: None,
                        holders: Vec::new(),
                        manager_ref: Link::Unstated,
                        existing: None,
                        usable: true,
                        create_day: day,
                    });
                    self.position_ix
                        .insert(position_key.clone(), self.positions.len() - 1);
                    self.positions.len() - 1
                }
            };
            let position = &mut self.positions[pi];
            position.rows.push(row.number);
            position.days.push(day);
            merge_attr(
                &mut position.name,
                row.position.clone(),
                row.number,
                Column::Position,
                same_text,
                show_string,
                &mut self.issues,
            );
            merge_attr(
                &mut position.staff,
                row.staff,
                row.number,
                Column::Staff,
                |a, b| a == b,
                |v| yes_no(*v),
                &mut self.issues,
            );
            merge_attr(
                &mut position.manager,
                row.manager.clone(),
                row.number,
                Column::Manager,
                same_key,
                show_string,
                &mut self.issues,
            );
            merge_attr(
                &mut position.deputy,
                row.deputy_order,
                row.number,
                Column::DeputyOrder,
                |a, b| a == b,
                |v| v.to_string(),
                &mut self.issues,
            );
            if row.head == Some(true) && position.head.is_none() {
                position.head = Some(row.number);
            }
            if let Some(person) = &row.person {
                position.holders.push(Holder {
                    row: row.number,
                    cell: person.clone(),
                    column: row.person_column,
                    share: row.share,
                    primary: row.primary,
                    day,
                    subject: None,
                });
            }
        }
    }

    // -- 2. people ----------------------------------------------------------

    fn resolve_people(&mut self) {
        let directory = self.inputs.directory;
        let mut seen: HashMap<(usize, Subject), u32> = HashMap::new();
        for (index, position) in self.positions.iter_mut().enumerate() {
            let mut kept = Vec::with_capacity(position.holders.len());
            for mut holder in position.holders.drain(..) {
                match directory.resolve(&holder.cell) {
                    Lookup::Found(subject) => {
                        if let Some(first) = seen.insert((index, subject.clone()), holder.row) {
                            self.issues.push(
                                Issue::new(
                                    holder.row,
                                    IssueKind::DuplicateAssignment,
                                    format!(
                                        "the same person holds position '{}' on row {first} too",
                                        position.code
                                    ),
                                )
                                .column(holder.column)
                                .value(holder.cell.clone())
                                .rows([first, holder.row]),
                            );
                            continue;
                        }
                        holder.subject = Some(subject);
                        kept.push(holder);
                    }
                    Lookup::Ambiguous(names) => {
                        self.issues.push(
                            Issue::new(
                                holder.row,
                                IssueKind::AmbiguousPerson,
                                format!(
                                    "'{}' fits more than one person: {}",
                                    holder.cell,
                                    names.join(", ")
                                ),
                            )
                            .column(holder.column)
                            .value(holder.cell.clone()),
                        );
                        kept.push(holder);
                    }
                    Lookup::Unknown => {
                        let mut issue = Issue::new(
                            holder.row,
                            IssueKind::UnknownPerson,
                            format!(
                                "no account or person '{}' in this organization",
                                holder.cell
                            ),
                        )
                        .column(holder.column)
                        .value(holder.cell.clone());
                        if let Some(found) = directory.suggest(&holder.cell) {
                            issue = issue.suggest(found.login, Some(found.name));
                        }
                        self.issues.push(issue);
                        kept.push(holder);
                    }
                }
            }
            position.holders = kept;
        }

        // A person has one primary position; two rows that claim it are a
        // contradiction the administrator has to settle, not one to pick from.
        let mut claims: BTreeMap<String, Vec<(u32, usize, usize)>> = BTreeMap::new();
        for (pi, position) in self.positions.iter().enumerate() {
            for (hi, holder) in position.holders.iter().enumerate() {
                if let (Some(subject), Some(true)) = (&holder.subject, holder.primary) {
                    claims
                        .entry(format!("{subject:?}"))
                        .or_default()
                        .push((holder.row, pi, hi));
                }
            }
        }
        for claim in claims.values().filter(|c| c.len() > 1) {
            let rows: Vec<u32> = claim.iter().map(|(row, _, _)| *row).collect();
            let last = *rows.iter().max().unwrap_or(&0);
            self.issues.push(
                Issue::new(
                    last,
                    IssueKind::TwoPrimaryPositions,
                    "a person can have only one primary position",
                )
                .column(Column::Primary)
                .rows(rows),
            );
            for (_, pi, hi) in &claim[1..] {
                self.positions[*pi].holders[*hi].primary = None;
            }
        }
    }

    // -- 3. unit types ------------------------------------------------------

    fn resolve_types(&mut self) {
        let types = self.inputs.types;
        for unit in &mut self.units {
            let Some(name) = &unit.type_name else {
                continue;
            };
            match types.iter().find(|t| key(&t.name) == key(&name.value)) {
                Some(found) => unit.type_id = Link::Set(found.id.clone()),
                None => {
                    unit.type_id = Link::Dropped;
                    let mut issue = Issue::new(
                        name.row,
                        IssueKind::UnknownUnitType,
                        format!("no unit type '{}'", name.value),
                    )
                    .column(Column::UnitType)
                    .value(name.value.clone());
                    if let Some(closest) =
                        closest_name(&name.value, types.iter().map(|t| t.name.as_str()))
                    {
                        issue = issue.suggest(closest, None);
                    }
                    self.issues.push(issue);
                }
            }
        }
    }

    // -- 4. what already exists ---------------------------------------------

    fn days_of(&self, days: &[NaiveDate]) -> Vec<NaiveDate> {
        let mut compared: Vec<NaiveDate> = days
            .iter()
            .map(|day| compared_on(*day, self.as_of()))
            .collect();
        compared.sort_unstable();
        compared.dedup();
        compared
    }

    fn match_existing(&mut self) -> Result<()> {
        for index in 0..self.units.len() {
            let days = self.days_of(&self.units[index].days);
            let code_key = self.units[index].key.clone();
            let mut found: Vec<String> = Vec::new();
            for day in days {
                let indexed = self.snapshots.at(day)?;
                found = indexed
                    .find_units(&code_key)
                    .iter()
                    .map(|u| u.unit_id.clone())
                    .collect();
                if !found.is_empty() {
                    break;
                }
            }
            let unit = &mut self.units[index];
            match found.len() {
                1 => unit.existing = found.pop(),
                0 if self.inputs.ever.units.contains(&code_key) => {
                    unit.usable = false;
                    self.issues.push(
                        Issue::new(
                            unit.rows[0],
                            IssueKind::CodeReserved,
                            "the code belongs to a unit that does not exist on that day",
                        )
                        .column(Column::UnitCode)
                        .value(unit.code.clone())
                        .rows(unit.rows.clone()),
                    );
                }
                0 => {
                    if unit.name.is_none() {
                        unit.usable = false;
                        self.issues.push(
                            Issue::new(
                                unit.rows[0],
                                IssueKind::MissingUnitName,
                                "a new unit needs a name",
                            )
                            .column(Column::UnitName)
                            .value(unit.code.clone())
                            .rows(unit.rows.clone()),
                        );
                    }
                }
                _ => {
                    unit.usable = false;
                    self.issues.push(
                        Issue::new(
                            unit.rows[0],
                            IssueKind::AmbiguousCode,
                            "several units in the structure have this code",
                        )
                        .column(Column::UnitCode)
                        .value(unit.code.clone())
                        .rows(unit.rows.clone()),
                    );
                }
            }
        }

        for index in 0..self.positions.len() {
            let days = self.days_of(&self.positions[index].days);
            let code_key = self.positions[index].key.clone();
            let mut found: Vec<(String, String)> = Vec::new();
            for day in days {
                let indexed = self.snapshots.at(day)?;
                found = indexed
                    .find_positions(&code_key)
                    .iter()
                    .map(|p| (p.position_id.clone(), p.unit_id.clone()))
                    .collect();
                if !found.is_empty() {
                    break;
                }
            }
            let unit_usable_existing = {
                let unit = &self.units[self.unit_ix[&self.positions[index].unit_key]];
                (unit.usable, unit.existing.clone())
            };
            let position = &mut self.positions[index];
            if !unit_usable_existing.0 {
                position.usable = false;
                continue;
            }
            match found.len() {
                1 => {
                    let (position_id, unit_id) = found.pop().unwrap_or_default();
                    if unit_usable_existing.1.as_deref() != Some(unit_id.as_str()) {
                        position.usable = false;
                        self.issues.push(
                            Issue::new(
                                position.rows[0],
                                IssueKind::PositionUnitMismatch,
                                "the position belongs to another unit; the import does not move positions between units",
                            )
                            .column(Column::UnitCode)
                            .value(position.unit_key.clone())
                            .rows(position.rows.clone()),
                        );
                    } else {
                        position.existing = Some(position_id);
                    }
                }
                0 if self.inputs.ever.positions.contains(&code_key) => {
                    position.usable = false;
                    self.issues.push(
                        Issue::new(
                            position.rows[0],
                            IssueKind::CodeReserved,
                            "the code belongs to a position that does not exist on that day",
                        )
                        .column(Column::PositionCode)
                        .value(position.code.clone())
                        .rows(position.rows.clone()),
                    );
                }
                0 => {
                    if position.name.is_none() {
                        position.usable = false;
                        self.issues.push(
                            Issue::new(
                                position.rows[0],
                                IssueKind::MissingPositionName,
                                "a new position needs a name",
                            )
                            .column(Column::Position)
                            .value(position.code.clone())
                            .rows(position.rows.clone()),
                        );
                    }
                }
                _ => {
                    position.usable = false;
                    self.issues.push(
                        Issue::new(
                            position.rows[0],
                            IssueKind::AmbiguousCode,
                            "several positions in the structure have this code",
                        )
                        .column(Column::PositionCode)
                        .value(position.code.clone())
                        .rows(position.rows.clone()),
                    );
                }
            }
        }
        Ok(())
    }

    // -- 5. links between entities ------------------------------------------

    /// A parent unit or a manager position named by code: in the file, else
    /// in the structure, else the error of a code that names nothing.
    fn resolve_links(&mut self) -> Result<()> {
        for index in 0..self.units.len() {
            if !self.units[index].usable {
                continue;
            }
            let Some(parent) = self.units[index].parent.clone() else {
                continue;
            };
            let parent_key = key(&parent.value);
            let link = if parent_key == self.units[index].key {
                self.issues.push(
                    Issue::new(
                        parent.row,
                        IssueKind::UnitCycle,
                        "a unit cannot be its own parent",
                    )
                    .column(Column::ParentCode)
                    .value(parent.value.clone()),
                );
                Link::Dropped
            } else if let Some(pi) = self.unit_ix.get(&parent_key).copied() {
                if self.units[pi].usable {
                    Link::Set(self.unit_ref(&self.units[pi]))
                } else {
                    Link::Dropped
                }
            } else if self.declared_units.contains(&parent_key) {
                Link::Dropped
            } else {
                let days = self.days_of(&self.units[index].days);
                let mut found: Vec<String> = Vec::new();
                for day in days {
                    found = self
                        .snapshots
                        .at(day)?
                        .find_units(&parent_key)
                        .iter()
                        .map(|u| u.unit_id.clone())
                        .collect();
                    if !found.is_empty() {
                        break;
                    }
                }
                match found.len() {
                    1 => Link::Set(UnitRef::Existing(found.remove(0))),
                    0 => {
                        self.issues.push(
                            Issue::new(
                                parent.row,
                                IssueKind::MissingParentUnit,
                                format!("no unit with the code '{}'", parent.value),
                            )
                            .column(Column::ParentCode)
                            .value(parent.value.clone()),
                        );
                        Link::Dropped
                    }
                    _ => {
                        self.issues.push(
                            Issue::new(
                                parent.row,
                                IssueKind::AmbiguousCode,
                                "several units in the structure have this code",
                            )
                            .column(Column::ParentCode)
                            .value(parent.value.clone()),
                        );
                        Link::Dropped
                    }
                }
            };
            self.units[index].parent_ref = link;
        }

        for index in 0..self.positions.len() {
            if !self.positions[index].usable {
                continue;
            }
            let Some(manager) = self.positions[index].manager.clone() else {
                continue;
            };
            let manager_key = key(&manager.value);
            let link = if manager_key == self.positions[index].key {
                self.issues.push(
                    Issue::new(
                        manager.row,
                        IssueKind::ReportingCycle,
                        "a position cannot report to itself",
                    )
                    .column(Column::Manager)
                    .value(manager.value.clone()),
                );
                Link::Dropped
            } else if let Some(mi) = self.position_ix.get(&manager_key).copied() {
                if !self.positions[mi].usable {
                    Link::Dropped
                } else if self.staff_after(mi)? {
                    self.staff_manager_issue(&manager);
                    Link::Dropped
                } else {
                    Link::Set(self.position_ref(&self.positions[mi]))
                }
            } else if self.declared_positions.contains(&manager_key) {
                Link::Dropped
            } else {
                let days = self.days_of(&self.positions[index].days);
                let mut found: Vec<(String, bool)> = Vec::new();
                for day in days {
                    found = self
                        .snapshots
                        .at(day)?
                        .find_positions(&manager_key)
                        .iter()
                        .map(|p| (p.position_id.clone(), p.is_staff))
                        .collect();
                    if !found.is_empty() {
                        break;
                    }
                }
                match found.len() {
                    1 => {
                        let (id, staff) = found.remove(0);
                        if staff {
                            self.staff_manager_issue(&manager);
                            Link::Dropped
                        } else {
                            Link::Set(PositionRef::Existing(id))
                        }
                    }
                    0 => {
                        self.issues.push(
                            Issue::new(
                                manager.row,
                                IssueKind::MissingManager,
                                format!("no position with the code '{}'", manager.value),
                            )
                            .column(Column::Manager)
                            .value(manager.value.clone()),
                        );
                        Link::Dropped
                    }
                    _ => {
                        self.issues.push(
                            Issue::new(
                                manager.row,
                                IssueKind::AmbiguousCode,
                                "several positions in the structure have this code",
                            )
                            .column(Column::Manager)
                            .value(manager.value.clone()),
                        );
                        Link::Dropped
                    }
                }
            };
            self.positions[index].manager_ref = link;
        }

        self.break_unit_cycles()?;
        self.break_position_cycles()?;
        Ok(())
    }

    fn staff_manager_issue(&mut self, manager: &Attr<String>) {
        self.issues.push(
            Issue::new(
                manager.row,
                IssueKind::StaffManager,
                "a staff position cannot be the manager of a position in the line",
            )
            .column(Column::Manager)
            .value(manager.value.clone()),
        );
    }

    /// Whether the position is staff once the file is applied.
    fn staff_after(&mut self, index: usize) -> Result<bool> {
        if let Some(staff) = &self.positions[index].staff {
            return Ok(staff.value);
        }
        let Some(id) = self.positions[index].existing.clone() else {
            return Ok(false);
        };
        let day = self.as_of();
        Ok(self
            .snapshots
            .at(day)?
            .snap
            .position(&id)
            .is_some_and(|p| p.is_staff))
    }

    fn break_unit_cycles(&mut self) -> Result<()> {
        let day = self.as_of();
        let base = self.snapshots.at(day)?;
        for _ in 0..=self.units.len() {
            let parent_of = |node: &str| -> Option<String> {
                if let Some(rest) = node.strip_prefix("new:") {
                    let def = &self.units[*self.unit_ix.get(rest)?];
                    return match &def.parent_ref {
                        Link::Set(UnitRef::Existing(id)) => Some(id.clone()),
                        Link::Set(UnitRef::New(key)) => Some(format!("new:{key}")),
                        _ => None,
                    };
                }
                let def = self
                    .units
                    .iter()
                    .find(|u| u.usable && u.existing.as_deref() == Some(node));
                match def.map(|d| &d.parent_ref) {
                    Some(Link::Set(UnitRef::Existing(id))) => Some(id.clone()),
                    Some(Link::Set(UnitRef::New(key))) => Some(format!("new:{key}")),
                    _ => base.snap.unit(node).and_then(|u| u.parent_unit_id.clone()),
                }
            };
            let nodes: Vec<String> = self
                .units
                .iter()
                .filter(|u| u.usable)
                .map(|u| {
                    u.existing
                        .clone()
                        .unwrap_or_else(|| format!("new:{}", u.key))
                })
                .collect();
            let Some(cycle) = find_cycle(&nodes, &parent_of) else {
                return Ok(());
            };
            let members: Vec<usize> = cycle
                .iter()
                .filter_map(|node| {
                    self.units.iter().position(|u| {
                        u.usable
                            && u.existing
                                .clone()
                                .unwrap_or_else(|| format!("new:{}", u.key))
                                == *node
                            && matches!(u.parent_ref, Link::Set(_))
                    })
                })
                .collect();
            let rows: Vec<u32> = members
                .iter()
                .filter_map(|i| self.units[*i].parent.as_ref().map(|p| p.row))
                .collect();
            let last = rows.iter().copied().max().unwrap_or(0);
            let breaker = members
                .iter()
                .copied()
                .find(|i| self.units[*i].parent.as_ref().map(|p| p.row) == Some(last));
            self.issues.push(
                Issue::new(
                    last,
                    IssueKind::UnitCycle,
                    "the parent units form a cycle; the structure must be a tree",
                )
                .column(Column::ParentCode)
                .rows(rows),
            );
            match breaker {
                Some(i) => self.units[i].parent_ref = Link::Dropped,
                None => return Ok(()),
            }
        }
        Ok(())
    }

    fn break_position_cycles(&mut self) -> Result<()> {
        let day = self.as_of();
        let base = self.snapshots.at(day)?;
        for _ in 0..=self.positions.len() {
            let parent_of = |node: &str| -> Option<String> {
                if let Some(rest) = node.strip_prefix("new:") {
                    let def = &self.positions[*self.position_ix.get(rest)?];
                    return match &def.manager_ref {
                        Link::Set(PositionRef::Existing(id)) => Some(id.clone()),
                        Link::Set(PositionRef::New(key)) => Some(format!("new:{key}")),
                        _ => None,
                    };
                }
                let def = self
                    .positions
                    .iter()
                    .find(|p| p.usable && p.existing.as_deref() == Some(node));
                match def.map(|d| &d.manager_ref) {
                    Some(Link::Set(PositionRef::Existing(id))) => Some(id.clone()),
                    Some(Link::Set(PositionRef::New(key))) => Some(format!("new:{key}")),
                    _ => base.snap.primary_parent_of(node).map(str::to_string),
                }
            };
            let identity = |p: &PositionDef| {
                p.existing
                    .clone()
                    .unwrap_or_else(|| format!("new:{}", p.key))
            };
            let nodes: Vec<String> = self
                .positions
                .iter()
                .filter(|p| p.usable)
                .map(identity)
                .collect();
            let Some(cycle) = find_cycle(&nodes, &parent_of) else {
                return Ok(());
            };
            let members: Vec<usize> = cycle
                .iter()
                .filter_map(|node| {
                    self.positions.iter().position(|p| {
                        p.usable && identity(p) == *node && matches!(p.manager_ref, Link::Set(_))
                    })
                })
                .collect();
            let rows: Vec<u32> = members
                .iter()
                .filter_map(|i| self.positions[*i].manager.as_ref().map(|m| m.row))
                .collect();
            let last = rows.iter().copied().max().unwrap_or(0);
            let breaker = members
                .iter()
                .copied()
                .find(|i| self.positions[*i].manager.as_ref().map(|m| m.row) == Some(last));
            self.issues.push(
                Issue::new(
                    last,
                    IssueKind::ReportingCycle,
                    "the managers form a cycle; reporting lines must be a tree",
                )
                .column(Column::Manager)
                .rows(rows),
            );
            match breaker {
                Some(i) => self.positions[i].manager_ref = Link::Dropped,
                None => return Ok(()),
            }
        }
        Ok(())
    }

    // -- 6. heads and deputies ----------------------------------------------

    fn check_heads_and_deputies(&mut self) {
        let unit_keys: Vec<String> = self.units.iter().map(|u| u.key.clone()).collect();
        for unit_key in unit_keys {
            let members: Vec<usize> = (0..self.positions.len())
                .filter(|i| self.positions[*i].usable && self.positions[*i].unit_key == unit_key)
                .collect();
            let heads: Vec<(u32, usize)> = members
                .iter()
                .filter_map(|i| self.positions[*i].head.map(|row| (row, *i)))
                .collect();
            if heads.len() > 1 {
                let rows: Vec<u32> = heads.iter().map(|(row, _)| *row).collect();
                let last = *rows.iter().max().unwrap_or(&0);
                self.issues.push(
                    Issue::new(
                        last,
                        IssueKind::TwoHeads,
                        "a unit has one head; the other should be a deputy head",
                    )
                    .column(Column::Head)
                    .rows(rows),
                );
                let keep = heads.iter().map(|(row, _)| *row).min();
                for (row, i) in &heads {
                    if Some(*row) != keep {
                        self.positions[*i].head = None;
                    }
                }
            }
            let mut orders: HashMap<u32, u32> = HashMap::new();
            for i in &members {
                let Some(deputy) = self.positions[*i].deputy.clone() else {
                    continue;
                };
                if self.positions[*i].head.is_some() {
                    self.issues.push(
                        Issue::new(
                            deputy.row,
                            IssueKind::HeadIsDeputy,
                            "the head of a unit cannot also be its deputy head",
                        )
                        .column(Column::DeputyOrder),
                    );
                    self.positions[*i].deputy = None;
                    continue;
                }
                if let Some(first) = orders.insert(deputy.value, deputy.row) {
                    self.issues.push(
                        Issue::new(
                            deputy.row,
                            IssueKind::DuplicateDeputyOrder,
                            format!(
                                "deputy order {} is already used on row {first}",
                                deputy.value
                            ),
                        )
                        .column(Column::DeputyOrder)
                        .value(deputy.value.to_string())
                        .rows([first, deputy.row]),
                    );
                    self.positions[*i].deputy = None;
                    orders.insert(deputy.value, first);
                }
            }
        }
    }

    // -- 7. the day a new entity starts -------------------------------------

    /// A new entity starts on the earliest day anything that hangs on it
    /// does: a manager no later than the people reporting to it, a unit no
    /// later than its positions and its child units.
    fn propagate_days(&mut self) {
        for unit in &mut self.units {
            unit.create_day = unit.days.iter().copied().min().unwrap_or(unit.create_day);
        }
        for position in &mut self.positions {
            position.create_day = position
                .days
                .iter()
                .copied()
                .min()
                .unwrap_or(position.create_day);
        }
        loop {
            let mut changed = false;
            for pi in 0..self.positions.len() {
                let day = self.positions[pi].create_day;
                if let Link::Set(PositionRef::New(manager)) = self.positions[pi].manager_ref.clone()
                {
                    if let Some(mi) = self.position_ix.get(&manager).copied() {
                        if day < self.positions[mi].create_day {
                            self.positions[mi].create_day = day;
                            changed = true;
                        }
                    }
                }
                if let Some(ui) = self.unit_ix.get(&self.positions[pi].unit_key).copied() {
                    if day < self.units[ui].create_day {
                        self.units[ui].create_day = day;
                        changed = true;
                    }
                }
            }
            for ui in 0..self.units.len() {
                let day = self.units[ui].create_day;
                if let Link::Set(UnitRef::New(parent)) = self.units[ui].parent_ref.clone() {
                    if let Some(pi) = self.unit_ix.get(&parent).copied() {
                        if day < self.units[pi].create_day {
                            self.units[pi].create_day = day;
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
    }

    // -- 8. operations ------------------------------------------------------

    fn operations(&mut self) -> Result<Vec<Op>> {
        let mut ops: Vec<Op> = Vec::new();
        let unit_order = self.unit_order()?;
        let position_order = self.position_order()?;

        // Units: parents before children, so a parent exists (or has moved)
        // before anything is placed under it.
        for ui in &unit_order {
            self.unit_ops(*ui, &mut ops)?;
        }
        // Positions: new ones first (managers before those reporting to them),
        // renames and un-staffing before the lines are moved (a staff
        // position cannot be given subordinates), staffing after them.
        for pi in &position_order {
            self.create_position_op(*pi, &mut ops)?;
        }
        for pi in &position_order {
            self.update_position_op(*pi, false, &mut ops)?;
        }
        for pi in &position_order {
            self.move_position_op(*pi, &mut ops)?;
        }
        for pi in &position_order {
            self.update_position_op(*pi, true, &mut ops)?;
        }
        for ui in &unit_order {
            self.head_op(*ui, &mut ops)?;
            self.deputies_op(*ui, &mut ops)?;
        }
        self.assignment_ops(&mut ops)?;
        Ok(ops)
    }

    /// Indexes of the usable defs, shallowest first in the final graph.
    fn unit_order(&mut self) -> Result<Vec<usize>> {
        let day = self.as_of();
        let base = self.snapshots.at(day)?;
        let depth_of = |start: usize, this: &Self| -> usize {
            let mut depth = 0;
            let mut node = start;
            let mut guard = 0;
            while let Link::Set(UnitRef::New(parent)) = &this.units[node].parent_ref {
                let Some(next) = this.unit_ix.get(parent) else {
                    break;
                };
                node = *next;
                depth += 1;
                guard += 1;
                if guard > this.units.len() {
                    break;
                }
            }
            if let Link::Set(UnitRef::Existing(id)) = &this.units[node].parent_ref {
                depth += 1 + unit_depth(&base.snap, id);
            }
            depth
        };
        let mut order: Vec<(usize, usize)> = (0..self.units.len())
            .filter(|i| self.units[*i].usable)
            .map(|i| (depth_of(i, self), i))
            .collect();
        order.sort();
        Ok(order.into_iter().map(|(_, i)| i).collect())
    }

    fn position_order(&mut self) -> Result<Vec<usize>> {
        let day = self.as_of();
        let base = self.snapshots.at(day)?;
        let depth_of = |start: usize, this: &Self| -> usize {
            let mut depth = 0;
            let mut node = start;
            let mut guard = 0;
            while let Link::Set(PositionRef::New(manager)) = &this.positions[node].manager_ref {
                let Some(next) = this.position_ix.get(manager) else {
                    break;
                };
                node = *next;
                depth += 1;
                guard += 1;
                if guard > this.positions.len() {
                    break;
                }
            }
            if let Link::Set(PositionRef::Existing(id)) = &this.positions[node].manager_ref {
                depth += 1 + position_depth(&base.snap, id);
            }
            depth
        };
        let mut order: Vec<(usize, usize)> = (0..self.positions.len())
            .filter(|i| self.positions[*i].usable)
            .map(|i| (depth_of(i, self), i))
            .collect();
        order.sort();
        Ok(order.into_iter().map(|(_, i)| i).collect())
    }

    fn compare_day(&self, days: &[NaiveDate]) -> NaiveDate {
        days.iter()
            .map(|d| compared_on(*d, self.as_of()))
            .min()
            .unwrap_or(self.as_of())
    }

    fn unit_label(&mut self, unit_id: &str, day: NaiveDate) -> Result<Option<String>> {
        Ok(self
            .snapshots
            .at(day)?
            .snap
            .unit(unit_id)
            .map(codes::unit_code))
    }

    fn position_label(&mut self, position_id: &str, day: NaiveDate) -> Result<Option<String>> {
        Ok(self
            .snapshots
            .at(day)?
            .snap
            .position(position_id)
            .map(codes::position_code))
    }

    fn unit_ops(&mut self, ui: usize, ops: &mut Vec<Op>) -> Result<()> {
        let (code, first_row) = (self.units[ui].code.clone(), self.units[ui].rows[0]);
        let name = self.units[ui].name.clone();
        let type_id = self.units[ui].type_id.clone();
        let parent_ref = self.units[ui].parent_ref.clone();
        let parent_attr = self.units[ui].parent.clone();
        let replace = self.inputs.settings.mode == Mode::Replace;

        let Some(unit_id) = self.units[ui].existing.clone() else {
            let Some(name) = name else { return Ok(()) };
            ops.push(Op {
                row: first_row,
                created: true,
                changes: vec![FieldChange {
                    entity: "unit",
                    field: "created",
                    before: None,
                    after: Some(name.value.clone()),
                }],
                kind: OpKind::CreateUnit {
                    key: self.units[ui].key.clone(),
                    name: name.value,
                    code,
                    type_id: match type_id {
                        Link::Set(id) => Some(id),
                        _ => None,
                    },
                    parent: match parent_ref {
                        Link::Set(parent) => Some(parent),
                        _ => None,
                    },
                    from: self.units[ui].create_day,
                },
            });
            return Ok(());
        };

        let day = self.compare_day(&self.units[ui].days);
        let indexed = self.snapshots.at(day)?;
        let Some(current) = indexed.snap.unit(&unit_id).cloned() else {
            return Ok(());
        };
        let mut changes = Vec::new();
        let mut blame = first_row;
        let mut new_name = None;
        if let Some(name) = &name {
            if name.value != current.name {
                blame = name.row;
                changes.push(FieldChange {
                    entity: "unit",
                    field: "name",
                    before: Some(current.name.clone()),
                    after: Some(name.value.clone()),
                });
                new_name = Some(name.value.clone());
            }
        }
        let mut new_type = None;
        match (&type_id, &self.units[ui].type_name) {
            (Link::Set(id), _) if current.type_id.as_deref() != Some(id.as_str()) => {
                changes.push(FieldChange {
                    entity: "unit",
                    field: "type",
                    before: current.type_id.clone(),
                    after: self.units[ui].type_name.as_ref().map(|t| t.value.clone()),
                });
                new_type = Some(Some(id.clone()));
            }
            (Link::Unstated, None) if replace && current.type_id.is_some() => {
                changes.push(FieldChange {
                    entity: "unit",
                    field: "type",
                    before: current.type_id.clone(),
                    after: None,
                });
                new_type = Some(None);
            }
            _ => {}
        }
        if !changes.is_empty() {
            ops.push(Op {
                row: blame,
                created: false,
                changes,
                kind: OpKind::UpdateUnit {
                    unit_id: unit_id.clone(),
                    name: new_name,
                    type_id: new_type,
                    from: day,
                },
            });
        }

        let target: Option<Option<UnitRef>> = match &parent_ref {
            Link::Set(parent) => Some(Some(parent.clone())),
            Link::Unstated if replace => Some(None),
            _ => None,
        };
        if let Some(target) = target {
            let same = match &target {
                Some(UnitRef::Existing(id)) => current.parent_unit_id.as_deref() == Some(id),
                Some(UnitRef::New(_)) => false,
                None => current.parent_unit_id.is_none(),
            };
            if !same {
                let before = match &current.parent_unit_id {
                    Some(id) => self.unit_label(id, day)?,
                    None => None,
                };
                ops.push(Op {
                    row: parent_attr.map_or(first_row, |p| p.row),
                    created: false,
                    changes: vec![FieldChange {
                        entity: "unit",
                        field: "parent",
                        before,
                        after: self.units[ui].parent.as_ref().map(|p| p.value.clone()),
                    }],
                    kind: OpKind::MoveUnit {
                        unit_id,
                        parent: target,
                        from: day,
                    },
                });
            }
        }
        Ok(())
    }

    fn create_position_op(&mut self, pi: usize, ops: &mut Vec<Op>) -> Result<()> {
        let position = &self.positions[pi];
        if position.existing.is_some() {
            return Ok(());
        }
        let Some(name) = position.name.clone() else {
            return Ok(());
        };
        let unit_def = &self.units[self.unit_ix[&position.unit_key]];
        let unit = self.unit_ref(unit_def);
        ops.push(Op {
            row: position.rows[0],
            created: true,
            changes: vec![FieldChange {
                entity: "position",
                field: "created",
                before: None,
                after: Some(name.value.clone()),
            }],
            kind: OpKind::CreatePosition {
                key: position.key.clone(),
                unit,
                name: name.value,
                code: position.code.clone(),
                staff: position.staff.as_ref().is_some_and(|s| s.value),
                manager: match &position.manager_ref {
                    Link::Set(manager) => Some(manager.clone()),
                    _ => None,
                },
                from: position.create_day,
            },
        });
        Ok(())
    }

    /// `staffing == false`: the name and the un-staffing; `true`: the staffing.
    fn update_position_op(&mut self, pi: usize, staffing: bool, ops: &mut Vec<Op>) -> Result<()> {
        let Some(position_id) = self.positions[pi].existing.clone() else {
            return Ok(());
        };
        let day = self.compare_day(&self.positions[pi].days);
        let indexed = self.snapshots.at(day)?;
        let Some(current) = indexed.snap.position(&position_id).cloned() else {
            return Ok(());
        };
        let replace = self.inputs.settings.mode == Mode::Replace;
        let position = &self.positions[pi];
        let wanted_staff = match &position.staff {
            Some(staff) => Some(staff.value),
            None if replace => Some(false),
            None => None,
        };
        let mut changes = Vec::new();
        let mut blame = position.rows[0];
        let mut name = None;
        let mut staff = None;
        if staffing {
            if wanted_staff == Some(true) && !current.is_staff {
                blame = position.staff.as_ref().map_or(blame, |s| s.row);
                changes.push(FieldChange {
                    entity: "position",
                    field: "staff",
                    before: Some(yes_no(false)),
                    after: Some(yes_no(true)),
                });
                staff = Some(true);
            }
        } else {
            if let Some(wanted) = &position.name {
                if wanted.value != current.name {
                    blame = wanted.row;
                    changes.push(FieldChange {
                        entity: "position",
                        field: "name",
                        before: Some(current.name.clone()),
                        after: Some(wanted.value.clone()),
                    });
                    name = Some(wanted.value.clone());
                }
            }
            if wanted_staff == Some(false) && current.is_staff {
                blame = position.staff.as_ref().map_or(blame, |s| s.row);
                changes.push(FieldChange {
                    entity: "position",
                    field: "staff",
                    before: Some(yes_no(true)),
                    after: Some(yes_no(false)),
                });
                staff = Some(false);
            }
        }
        if !changes.is_empty() {
            ops.push(Op {
                row: blame,
                created: false,
                changes,
                kind: OpKind::UpdatePosition {
                    position_id,
                    name,
                    staff,
                    from: day,
                },
            });
        }
        Ok(())
    }

    fn move_position_op(&mut self, pi: usize, ops: &mut Vec<Op>) -> Result<()> {
        let Some(position_id) = self.positions[pi].existing.clone() else {
            return Ok(());
        };
        let day = self.compare_day(&self.positions[pi].days);
        let indexed = self.snapshots.at(day)?;
        let current = indexed
            .snap
            .primary_parent_of(&position_id)
            .map(str::to_string);
        let replace = self.inputs.settings.mode == Mode::Replace;
        let position = &self.positions[pi];
        let target: Option<Option<PositionRef>> = match &position.manager_ref {
            Link::Set(manager) => Some(Some(manager.clone())),
            Link::Unstated if replace => Some(None),
            _ => None,
        };
        let Some(target) = target else { return Ok(()) };
        let same = match &target {
            Some(PositionRef::Existing(id)) => current.as_deref() == Some(id),
            Some(PositionRef::New(_)) => false,
            None => current.is_none(),
        };
        if same {
            return Ok(());
        }
        let row = position
            .manager
            .as_ref()
            .map_or(position.rows[0], |m| m.row);
        let after = position.manager.as_ref().map(|m| m.value.clone());
        let before = match &current {
            Some(id) => self.position_label(id, day)?,
            None => None,
        };
        ops.push(Op {
            row,
            created: false,
            changes: vec![FieldChange {
                entity: "position",
                field: "manager",
                before,
                after,
            }],
            kind: OpKind::MovePosition {
                position_id,
                manager: target,
                from: day,
            },
        });
        Ok(())
    }

    fn head_op(&mut self, ui: usize, ops: &mut Vec<Op>) -> Result<()> {
        let unit = &self.units[ui];
        let head = self
            .positions
            .iter()
            .find(|p| p.usable && p.unit_key == unit.key && p.head.is_some());
        let replace = self.inputs.settings.mode == Mode::Replace;
        let unit_ref = self.unit_ref(unit);
        let unit_days = unit.days.clone();
        let unit_existing = unit.existing.clone();
        let unit_create = unit.create_day;
        let unit_first_row = unit.rows[0];
        let head_info = head.map(|p| (self.position_ref(p), p.head, p.code.clone(), p.create_day));

        let (target, row, label, from_new) = match head_info {
            Some((position_ref, row, code, create_day)) => (
                Some(position_ref),
                row.unwrap_or(unit_first_row),
                Some(code),
                Some(create_day),
            ),
            None if replace && unit_existing.is_some() => (None, unit_first_row, None, None),
            None => return Ok(()),
        };
        let day = self.compare_day(&unit_days);
        let current = match &unit_existing {
            Some(id) => self
                .snapshots
                .at(day)?
                .snap
                .unit(id)
                .and_then(|u| u.head_position_id.clone()),
            None => None,
        };
        let same = match &target {
            Some(PositionRef::Existing(id)) => current.as_deref() == Some(id),
            Some(PositionRef::New(_)) => false,
            None => current.is_none(),
        };
        if same {
            return Ok(());
        }
        let before = match &current {
            Some(id) => self.position_label(id, day)?,
            None => None,
        };
        let from = match (&unit_existing, from_new) {
            // A new unit's head is new as well and starts with it.
            (None, Some(head_day)) => head_day.max(unit_create),
            (None, None) => unit_create,
            (Some(_), Some(head_day)) if matches!(target, Some(PositionRef::New(_))) => {
                head_day.max(day)
            }
            (Some(_), _) => day,
        };
        ops.push(Op {
            row,
            created: false,
            changes: vec![FieldChange {
                entity: "unit",
                field: "head",
                before,
                after: label,
            }],
            kind: OpKind::SetHead {
                unit: unit_ref,
                position: target,
                from,
            },
        });
        Ok(())
    }

    fn deputies_op(&mut self, ui: usize, ops: &mut Vec<Op>) -> Result<()> {
        let unit = &self.units[ui];
        let mut wanted: Vec<(u32, u32, usize)> = self
            .positions
            .iter()
            .enumerate()
            .filter(|(_, p)| p.usable && p.unit_key == unit.key)
            .filter_map(|(i, p)| p.deputy.as_ref().map(|d| (d.value, d.row, i)))
            .collect();
        wanted.sort_unstable();
        let replace = self.inputs.settings.mode == Mode::Replace;
        if wanted.is_empty() && !(replace && unit.existing.is_some()) {
            return Ok(());
        }
        let refs: Vec<PositionRef> = wanted
            .iter()
            .map(|(_, _, i)| self.position_ref(&self.positions[*i]))
            .collect();
        let labels: Vec<String> = wanted
            .iter()
            .map(|(_, _, i)| self.positions[*i].code.clone())
            .collect();
        let row = wanted.first().map_or(unit.rows[0], |(_, row, _)| *row);
        let unit_ref = self.unit_ref(unit);
        let unit_existing = unit.existing.clone();
        let unit_create = unit.create_day;
        let unit_days = unit.days.clone();
        let mut from = match &unit_existing {
            Some(_) => self.compare_day(&unit_days),
            None => unit_create,
        };
        for (_, _, i) in &wanted {
            if self.positions[*i].existing.is_none() {
                from = from.max(self.positions[*i].create_day);
            }
        }
        let day = self.compare_day(&unit_days);
        let current: Vec<String> = match &unit_existing {
            Some(id) => self
                .snapshots
                .at(day)?
                .snap
                .deputy_heads
                .iter()
                .filter(|d| &d.unit_id == id)
                .map(|d| d.position_id.clone())
                .collect(),
            None => Vec::new(),
        };
        let same = current.len() == refs.len()
            && current.iter().zip(&refs).all(|(id, wanted)| match wanted {
                PositionRef::Existing(other) => id == other,
                PositionRef::New(_) => false,
            });
        if same {
            return Ok(());
        }
        let mut before = Vec::new();
        for id in &current {
            if let Some(label) = self.position_label(id, day)? {
                before.push(label);
            }
        }
        ops.push(Op {
            row,
            created: false,
            changes: vec![FieldChange {
                entity: "unit",
                field: "deputies",
                before: (!before.is_empty()).then(|| before.join(", ")),
                after: (!labels.is_empty()).then(|| labels.join(", ")),
            }],
            kind: OpKind::SetDeputies {
                unit: unit_ref,
                positions: refs,
                from,
            },
        });
        Ok(())
    }

    fn assignment_ops(&mut self, ops: &mut Vec<Op>) -> Result<()> {
        let as_of = self.as_of();
        let base = self.snapshots.at(as_of)?;
        let mut demoted: HashSet<String> = HashSet::new();
        let mut demotions: Vec<Op> = Vec::new();
        let mut rest: Vec<Op> = Vec::new();

        for pi in 0..self.positions.len() {
            if !self.positions[pi].usable {
                continue;
            }
            let position_ref = self.position_ref(&self.positions[pi]);
            let existing = self.positions[pi].existing.clone();
            let position_code = self.positions[pi].code.clone();
            let holders: Vec<ResolvedHolder> = self.positions[pi]
                .holders
                .iter()
                .filter_map(|h| {
                    h.subject
                        .clone()
                        .map(|s| (h.row, h.cell.clone(), h.share, h.primary, h.day, s))
                })
                .collect();
            let mut matched: HashSet<String> = HashSet::new();
            for (row, cell, share, primary, row_day, subject) in &holders {
                let day = compared_on(*row_day, as_of);
                let found = match &existing {
                    Some(id) => self
                        .snapshots
                        .at(day)?
                        .snap
                        .holders_of(id)
                        .find(|a| &a.subject == subject)
                        .cloned(),
                    None => None,
                };
                match found {
                    Some(current) => {
                        matched.insert(current.id.clone());
                        let mut changes = Vec::new();
                        let mut new_share = None;
                        let mut new_primary = None;
                        if let Some(share) = share {
                            if (share - current.share).abs() > 1e-9 {
                                changes.push(FieldChange {
                                    entity: "assignment",
                                    field: "share",
                                    before: Some(current.share.to_string()),
                                    after: Some(share.to_string()),
                                });
                                new_share = Some(*share);
                            }
                        }
                        if let Some(primary) = primary {
                            if *primary != current.is_primary {
                                changes.push(FieldChange {
                                    entity: "assignment",
                                    field: "primary",
                                    before: Some(yes_no(current.is_primary)),
                                    after: Some(yes_no(*primary)),
                                });
                                new_primary = Some(*primary);
                            }
                        }
                        if new_primary == Some(true) {
                            self.demote_others(
                                &held_by(&base.snap, subject),
                                &current.id,
                                *row,
                                as_of,
                                &mut demoted,
                                &mut demotions,
                            );
                        }
                        if !changes.is_empty() {
                            rest.push(Op {
                                row: *row,
                                created: false,
                                changes,
                                kind: OpKind::UpdateAssignment {
                                    assignment_id: current.id.clone(),
                                    share: new_share,
                                    primary: new_primary,
                                    from: day,
                                },
                            });
                        }
                    }
                    None => {
                        if *primary == Some(true) {
                            self.demote_others(
                                &held_by(&base.snap, subject),
                                "",
                                *row,
                                as_of,
                                &mut demoted,
                                &mut demotions,
                            );
                        }
                        rest.push(Op {
                            row: *row,
                            created: true,
                            changes: vec![FieldChange {
                                entity: "assignment",
                                field: "created",
                                before: None,
                                after: Some(cell.clone()),
                            }],
                            kind: OpKind::Assign {
                                position: position_ref.clone(),
                                subject: subject.clone(),
                                share: share.unwrap_or(1.0),
                                primary: *primary,
                                from: *row_day,
                            },
                        });
                    }
                }
            }
            // Holders the file does not mention: left alone by an upsert
            // (with a word about it), ended by a replace.
            if let Some(id) = &existing {
                let others: Vec<_> = base
                    .snap
                    .holders_of(id)
                    .filter(|a| {
                        !matched.contains(&a.id) && !holders.iter().any(|h| h.5 == a.subject)
                    })
                    .cloned()
                    .collect();
                if self.inputs.settings.mode == Mode::Replace {
                    for other in others {
                        let label = self.inputs.directory.label(&other.subject);
                        self.ended.push(EndedItem {
                            kind: "assignment",
                            code: position_code.clone(),
                            name: label.clone(),
                            holders: vec![label],
                        });
                        rest.push(Op {
                            row: self.positions[pi].rows[0],
                            created: false,
                            changes: vec![FieldChange {
                                entity: "assignment",
                                field: "ended",
                                before: Some(position_code.clone()),
                                after: None,
                            }],
                            kind: OpKind::EndAssignment {
                                assignment_id: other.id,
                                from: as_of,
                            },
                        });
                    }
                } else if !holders.is_empty() && !others.is_empty() {
                    self.warnings.push(
                        Issue::new(
                            holders[0].0,
                            IssueKind::PositionHasOtherHolder,
                            format!(
                                "position '{position_code}' is also held by someone the file does not list; an upsert keeps them"
                            ),
                        )
                        .column(Column::Person),
                    );
                }
            }
        }
        // Ended first, so a person can leave one position and take another
        // in the same file; primaries demoted before any is promoted.
        let (ended, rest): (Vec<Op>, Vec<Op>) = rest
            .into_iter()
            .partition(|op| matches!(op.kind, OpKind::EndAssignment { .. }));
        ops.extend(ended);
        ops.extend(demotions);
        ops.extend(rest);
        Ok(())
    }

    /// A person's other primary assignments stop being primary when the file
    /// makes another one primary: the table refuses two at once.
    fn demote_others(
        &self,
        held: &[Assignment],
        keep: &str,
        row: u32,
        day: NaiveDate,
        demoted: &mut HashSet<String>,
        out: &mut Vec<Op>,
    ) {
        for other in held {
            if other.is_primary && other.id != keep && demoted.insert(other.id.clone()) {
                out.push(Op {
                    row,
                    created: false,
                    changes: vec![FieldChange {
                        entity: "assignment",
                        field: "primary",
                        before: Some(yes_no(true)),
                        after: Some(yes_no(false)),
                    }],
                    kind: OpKind::DemotePrimary {
                        assignment_id: other.id.clone(),
                        from: day,
                    },
                });
            }
        }
    }

    // -- 9. replace: what the file leaves out ends ---------------------------

    /// In replace mode the file is the whole structure: what it does not
    /// mention ends on the import day. What it mentions but could not use (a
    /// bad row) is not "left out", and ending it would punish the structure
    /// for a typo.
    fn replace_ends(&mut self, planned: &[Op]) -> Result<Vec<Op>> {
        let as_of = self.as_of();
        let base = self.snapshots.at(as_of)?;
        if !base.snap.units.is_empty()
            && self.declared_units.is_empty()
            && self.units.iter().all(|u| u.existing.is_none())
        {
            self.issues.push(Issue::new(
                0,
                IssueKind::ReplaceMatchesNothing,
                "replace would end the whole structure: no row of the file matches a unit that exists",
            ));
            return Ok(Vec::new());
        }
        let mentioned_units: HashSet<&str> = self
            .units
            .iter()
            .map(|u| u.key.as_str())
            .chain(self.declared_units.iter().map(String::as_str))
            .collect();
        let mentioned_positions: HashSet<&str> = self
            .positions
            .iter()
            .map(|p| p.key.as_str())
            .chain(self.declared_positions.iter().map(String::as_str))
            .collect();
        let ended_units: Vec<&Unit> = base
            .snap
            .units
            .iter()
            .filter(|u| !mentioned_units.contains(key(&codes::unit_code(u)).as_str()))
            .collect();
        let ended_positions: Vec<&Position> = base
            .snap
            .positions
            .iter()
            .filter(|p| !mentioned_positions.contains(key(&codes::position_code(p)).as_str()))
            .collect();
        let ending_position = |id: &str| ended_positions.iter().any(|p| p.position_id == id);
        for unit in &ended_units {
            self.ended.push(EndedItem {
                kind: "unit",
                code: codes::unit_code(unit),
                name: unit.name.clone(),
                holders: Vec::new(),
            });
        }
        for position in &ended_positions {
            let holders: Vec<String> = base
                .snap
                .holders_of(&position.position_id)
                .map(|a| self.inputs.directory.label(&a.subject))
                .collect();
            self.cascaded_holders
                .insert(position.position_id.clone(), holders.len());
            self.ended.push(EndedItem {
                kind: "position",
                code: codes::position_code(position),
                name: position.name.clone(),
                holders,
            });
        }

        let mut ops = Vec::new();
        // A position that heads a unit cannot end: clear the head first,
        // unless the file already names another one.
        for unit in &base.snap.units {
            let Some(head) = &unit.head_position_id else {
                continue;
            };
            let replaced = planned.iter().any(|op| {
                matches!(&op.kind, OpKind::SetHead { unit: UnitRef::Existing(id), .. } if *id == unit.unit_id)
            });
            if ending_position(head) && !replaced {
                ops.push(end_op(
                    0,
                    OpKind::SetHead {
                        unit: UnitRef::Existing(unit.unit_id.clone()),
                        position: None,
                        from: as_of,
                    },
                    "unit",
                    "head",
                    Some(codes::position_code(
                        base.snap.position(head).unwrap_or(ended_positions[0]),
                    )),
                ));
            }
        }
        // Leaf-first: a position cannot end while others report to it, a
        // unit not while it still holds units or positions.
        let mut positions: Vec<(usize, &Position)> = ended_positions
            .iter()
            .map(|p| (position_depth(&base.snap, &p.position_id), *p))
            .collect();
        positions.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.position_id.cmp(&b.1.position_id)));
        for (_, position) in positions {
            ops.push(end_op(
                0,
                OpKind::EndPosition {
                    position_id: position.position_id.clone(),
                    from: as_of,
                },
                "position",
                "ended",
                Some(codes::position_code(position)),
            ));
        }
        let mut units: Vec<(usize, &Unit)> = ended_units
            .iter()
            .map(|u| (unit_depth(&base.snap, &u.unit_id), *u))
            .collect();
        units.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.unit_id.cmp(&b.1.unit_id)));
        for (_, unit) in units {
            ops.push(end_op(
                0,
                OpKind::EndUnit {
                    unit_id: unit.unit_id.clone(),
                    from: as_of,
                },
                "unit",
                "ended",
                Some(codes::unit_code(unit)),
            ));
        }
        Ok(ops)
    }

    // -- 10. dates ------------------------------------------------------------

    /// A change dated before today rewrites history; the run refuses it
    /// until the administrator confirms. Row 0 stands for the operations no
    /// row asked for (what a replace ends).
    fn check_backdated(&mut self, ops: &[Op]) {
        if self.inputs.settings.confirm_backdated {
            return;
        }
        let today = self.inputs.settings.today;
        let mut reported: HashSet<u32> = HashSet::new();
        for op in ops {
            let date = op.kind.date();
            if date < today && reported.insert(op.row) {
                let mut issue = Issue::new(
                    op.row,
                    IssueKind::BackdatedConfirmationRequired,
                    format!(
                        "takes effect on {}, before today ({}); confirm to rewrite history",
                        validate::format_date(date),
                        validate::format_date(today)
                    ),
                )
                .value(validate::format_date(date));
                if op.row != 0 {
                    issue = issue.column(Column::From);
                }
                self.issues.push(issue);
            }
        }
    }
}

fn end_op(
    row: u32,
    kind: OpKind,
    entity: &'static str,
    field: &'static str,
    before: Option<String>,
) -> Op {
    Op {
        row,
        created: false,
        changes: vec![FieldChange {
            entity,
            field,
            before,
            after: None,
        }],
        kind,
    }
}

fn held_by(snap: &Snapshot, subject: &Subject) -> Vec<Assignment> {
    snap.assignments
        .iter()
        .filter(|a| &a.subject == subject)
        .cloned()
        .collect()
}

fn unit_depth(snap: &Snapshot, unit_id: &str) -> usize {
    let mut depth = 0;
    let mut node = unit_id;
    while let Some(parent) = snap.unit(node).and_then(|u| u.parent_unit_id.as_deref()) {
        depth += 1;
        node = parent;
        if depth > snap.units.len() {
            break;
        }
    }
    depth
}

fn position_depth(snap: &Snapshot, position_id: &str) -> usize {
    let mut depth = 0;
    let mut node = position_id;
    while let Some(parent) = snap.primary_parent_of(node) {
        depth += 1;
        node = parent;
        if depth > snap.positions.len() {
            break;
        }
    }
    depth
}

/// A cycle among the given nodes of a graph where each node has at most one
/// parent; the members of the first one found.
fn find_cycle(nodes: &[String], parent_of: &dyn Fn(&str) -> Option<String>) -> Option<Vec<String>> {
    let mut done: HashSet<String> = HashSet::new();
    for start in nodes {
        if done.contains(start) {
            continue;
        }
        let mut path: Vec<String> = Vec::new();
        let mut on_path: HashSet<String> = HashSet::new();
        let mut node = Some(start.clone());
        while let Some(current) = node {
            if done.contains(&current) {
                break;
            }
            if on_path.contains(&current) {
                let from = path.iter().position(|n| *n == current).unwrap_or(0);
                return Some(path[from..].to_vec());
            }
            on_path.insert(current.clone());
            path.push(current.clone());
            node = parent_of(&current);
        }
        done.extend(path);
    }
    None
}
