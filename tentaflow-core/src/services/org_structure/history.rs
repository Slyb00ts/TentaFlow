//! History of the structure and the difference between two states of it
//! (docs/ORG_STRUCTURE_PLAN.md §2.4, §3.4, §6.3).
//!
//! Two questions, two mechanisms:
//!   * "what changed, by whom, when" is read from the audit trail (the
//!     repository writes one entry per change, in the transaction of the
//!     change), resolved to names through the versioned rows;
//!   * "what differs between the 1st of July and today" is a pure comparison
//!     of two structure views (`diff_views`), which is also how a planned
//!     reorganization shows what it would change.
//!
//! Privacy (§6.3): the position history of a person is an administrator's and
//! that person's own. Everything here takes a `Privacy` and drops the personal
//! part instead of masking it, so a caller cannot forget to. The structural
//! part (units, positions, lines) is open to every member.
//!
//! The answers are the wire types of `tentaflow_protocol::org_history`: they
//! are only ever built here and read by the dispatcher, and a second set of
//! identical types would be a copy to keep in step.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::NaiveDate;
use rusqlite::Connection;
use serde_json::Value as Json;
use tentaflow_protocol::org_history::{OrgDiffItem, OrgFieldChange, OrgHistoryEntry, OrgHistoryOp};
use tentaflow_protocol::org_structure::{
    OrgAssignment, OrgPosition, OrgStructureView, OrgSubject, OrgUnit, OrgWarning,
};

use super::error::{OrgStructureError as E, Result};
use super::repo::timezone_of;
use super::validate;
use crate::db::DbPool;

/// Who is asking, as far as the history is concerned.
#[derive(Debug, Clone, Copy)]
pub struct Privacy<'a> {
    /// An administrator (`org.admin`) sees every person's history.
    pub personal_visible: bool,
    pub viewer_user_id: &'a str,
}

impl Privacy<'_> {
    /// Whether the history of `subject` (who holds what, since when) may be shown.
    pub fn may_see(&self, subject: &OrgSubject) -> bool {
        self.personal_visible
            || matches!(subject, OrgSubject::User(id) if id == self.viewer_user_id)
    }
}

// ---------------------------------------------------------------------------
// Difference between two states
// ---------------------------------------------------------------------------

/// Names the views cannot give: unit types are not part of a structure view.
#[derive(Debug, Default)]
pub struct DiffContext<'a> {
    pub type_names: HashMap<String, String>,
    /// Limits the answer to a unit, its sub-units and their positions.
    pub unit_id: Option<&'a str>,
}

fn opt_text(value: &Option<String>) -> Option<String> {
    value.clone()
}

fn flag(value: Option<bool>) -> Option<String> {
    value.map(|v| v.to_string())
}

/// Names of positions as one view shows them; `with_holders` puts the people
/// on a position in place of its name (the head of a unit is "Anna
/// Kowalska", not "Kierownik") and is off for anyone who may not see them.
struct Labels<'a> {
    view: &'a OrgStructureView,
    unit_names: HashMap<&'a str, &'a str>,
    position_names: HashMap<&'a str, &'a str>,
    holders: HashMap<&'a str, Vec<&'a str>>,
    with_holders: bool,
}

impl<'a> Labels<'a> {
    fn new(view: &'a OrgStructureView, with_holders: bool) -> Self {
        let mut holders: HashMap<&str, Vec<&str>> = HashMap::new();
        for a in &view.assignments {
            if !a.display_name.is_empty() {
                holders
                    .entry(a.position_id.as_str())
                    .or_default()
                    .push(a.display_name.as_str());
            }
        }
        Self {
            view,
            unit_names: view
                .units
                .iter()
                .map(|u| (u.unit_id.as_str(), u.name.as_str()))
                .collect(),
            position_names: view
                .positions
                .iter()
                .map(|p| (p.position_id.as_str(), p.name.as_str()))
                .collect(),
            holders,
            with_holders,
        }
    }

    fn unit(&self, id: &str) -> Option<String> {
        self.unit_names.get(id).map(|n| n.to_string())
    }

    fn position(&self, id: &str) -> Option<String> {
        if self.with_holders {
            if let Some(names) = self.holders.get(id) {
                return Some(names.join(", "));
            }
        }
        self.position_names.get(id).map(|n| n.to_string())
    }

    fn positions(&self, ids: &[String]) -> Option<String> {
        if ids.is_empty() {
            return None;
        }
        Some(
            ids.iter()
                .filter_map(|id| self.position(id))
                .collect::<Vec<_>>()
                .join(", "),
        )
    }

    /// The unit a position sits in on this view.
    fn unit_of_position(&self, id: &str) -> Option<&'a str> {
        self.view
            .positions
            .iter()
            .find(|p| p.position_id == id)
            .map(|p| p.unit_id.as_str())
    }
}

struct Sink<'a> {
    items: Vec<OrgDiffItem>,
    entity: &'static str,
    id: &'a str,
    name: &'a str,
    unit_id: Option<&'a str>,
}

impl Sink<'_> {
    fn base(&self, change: &str) -> OrgDiffItem {
        OrgDiffItem {
            change: change.to_string(),
            entity: self.entity.to_string(),
            id: self.id.to_string(),
            name: self.name.to_string(),
            unit_id: self.unit_id.map(str::to_string),
            ..Default::default()
        }
    }

    fn field(
        &mut self,
        field: &str,
        before: Option<String>,
        after: Option<String>,
        labels: (Option<String>, Option<String>),
    ) {
        if before == after {
            return;
        }
        self.items.push(OrgDiffItem {
            field: Some(field.to_string()),
            before,
            after,
            before_label: labels.0,
            after_label: labels.1,
            ..self.base("changed")
        });
    }
}

fn diff_unit(
    from: Option<(&OrgUnit, &Labels<'_>)>,
    to: Option<(&OrgUnit, &Labels<'_>)>,
    ctx: &DiffContext<'_>,
    out: &mut Vec<OrgDiffItem>,
) {
    let (id, name) = match (from, to) {
        (_, Some((u, _))) | (Some((u, _)), None) => (u.unit_id.as_str(), u.name.as_str()),
        (None, None) => return,
    };
    let mut sink = Sink {
        items: Vec::new(),
        entity: "unit",
        id,
        name,
        unit_id: Some(id),
    };
    match (from, to) {
        (None, Some(_)) => sink.items.push(sink.base("added")),
        (Some(_), None) => sink.items.push(sink.base("removed")),
        (Some((a, la)), Some((b, lb))) => {
            let type_label =
                |id: &Option<String>| id.as_ref().and_then(|t| ctx.type_names.get(t).cloned());
            sink.field(
                "name",
                Some(a.name.clone()),
                Some(b.name.clone()),
                (None, None),
            );
            sink.field("code", opt_text(&a.code), opt_text(&b.code), (None, None));
            sink.field(
                "type_id",
                opt_text(&a.type_id),
                opt_text(&b.type_id),
                (type_label(&a.type_id), type_label(&b.type_id)),
            );
            sink.field(
                "parent_unit_id",
                opt_text(&a.parent_unit_id),
                opt_text(&b.parent_unit_id),
                (
                    a.parent_unit_id.as_deref().and_then(|p| la.unit(p)),
                    b.parent_unit_id.as_deref().and_then(|p| lb.unit(p)),
                ),
            );
            sink.field(
                "head_position_id",
                opt_text(&a.head_position_id),
                opt_text(&b.head_position_id),
                (
                    a.head_position_id.as_deref().and_then(|p| la.position(p)),
                    b.head_position_id.as_deref().and_then(|p| lb.position(p)),
                ),
            );
            let join = |ids: &[String]| (!ids.is_empty()).then(|| ids.join(","));
            sink.field(
                "deputy_head_position_ids",
                join(&a.deputy_head_position_ids),
                join(&b.deputy_head_position_ids),
                (
                    la.positions(&a.deputy_head_position_ids),
                    lb.positions(&b.deputy_head_position_ids),
                ),
            );
        }
        (None, None) => {}
    }
    out.extend(sink.items);
}

fn diff_position(
    from: Option<(&OrgPosition, &Labels<'_>)>,
    to: Option<(&OrgPosition, &Labels<'_>)>,
    out: &mut Vec<OrgDiffItem>,
) {
    let (id, name, unit) = match (from, to) {
        (_, Some((p, _))) | (Some((p, _)), None) => {
            (p.position_id.as_str(), p.name.as_str(), p.unit_id.as_str())
        }
        (None, None) => return,
    };
    let mut sink = Sink {
        items: Vec::new(),
        entity: "position",
        id,
        name,
        unit_id: Some(unit),
    };
    match (from, to) {
        (None, Some(_)) => sink.items.push(sink.base("added")),
        (Some(_), None) => sink.items.push(sink.base("removed")),
        (Some((a, la)), Some((b, lb))) => {
            sink.field(
                "name",
                Some(a.name.clone()),
                Some(b.name.clone()),
                (None, None),
            );
            sink.field("code", opt_text(&a.code), opt_text(&b.code), (None, None));
            sink.field(
                "unit_id",
                Some(a.unit_id.clone()),
                Some(b.unit_id.clone()),
                (la.unit(&a.unit_id), lb.unit(&b.unit_id)),
            );
            sink.field(
                "primary_parent_position_id",
                opt_text(&a.primary_parent_position_id),
                opt_text(&b.primary_parent_position_id),
                (
                    a.primary_parent_position_id
                        .as_deref()
                        .and_then(|p| la.position(p)),
                    b.primary_parent_position_id
                        .as_deref()
                        .and_then(|p| lb.position(p)),
                ),
            );
            let join = |ids: &[String]| (!ids.is_empty()).then(|| ids.join(","));
            sink.field(
                "functional_parent_position_ids",
                join(&a.functional_parent_position_ids),
                join(&b.functional_parent_position_ids),
                (
                    la.positions(&a.functional_parent_position_ids),
                    lb.positions(&b.functional_parent_position_ids),
                ),
            );
            sink.field(
                "role_id",
                opt_text(&a.role_id),
                opt_text(&b.role_id),
                (None, None),
            );
            sink.field(
                "is_manager",
                flag(a.is_manager),
                flag(b.is_manager),
                (None, None),
            );
            sink.field(
                "is_staff",
                Some(a.is_staff.to_string()),
                Some(b.is_staff.to_string()),
                (None, None),
            );
        }
        (None, None) => {}
    }
    out.extend(sink.items);
}

fn assignment_key(a: &OrgAssignment) -> (String, String) {
    let subject = match &a.subject {
        OrgSubject::User(id) => format!("user:{id}"),
        OrgSubject::External(id) => format!("external:{id}"),
    };
    (a.position_id.clone(), subject)
}

fn diff_assignments(
    from: &OrgStructureView,
    to: &OrgStructureView,
    privacy: &Privacy<'_>,
    la: &Labels<'_>,
    lb: &Labels<'_>,
    out: &mut Vec<OrgDiffItem>,
) {
    let before: BTreeMap<_, _> = from
        .assignments
        .iter()
        .filter(|a| privacy.may_see(&a.subject))
        .map(|a| (assignment_key(a), a))
        .collect();
    let after: BTreeMap<_, _> = to
        .assignments
        .iter()
        .filter(|a| privacy.may_see(&a.subject))
        .map(|a| (assignment_key(a), a))
        .collect();
    let keys: BTreeSet<_> = before.keys().chain(after.keys()).cloned().collect();
    for key in keys {
        let (a, b) = (before.get(&key), after.get(&key));
        let any = b.or(a).expect("a key comes from one of the two maps");
        let position_name = lb
            .position_names
            .get(any.position_id.as_str())
            .or_else(|| la.position_names.get(any.position_id.as_str()))
            .copied()
            .unwrap_or("");
        let unit = lb
            .unit_of_position(&any.position_id)
            .or_else(|| la.unit_of_position(&any.position_id))
            .map(str::to_string);
        let mut sink = Sink {
            items: Vec::new(),
            entity: "assignment",
            id: &any.position_id,
            name: position_name,
            unit_id: unit.as_deref(),
        };
        let person = |item: OrgDiffItem| OrgDiffItem {
            subject: Some(any.subject.clone()),
            subject_name: Some(any.display_name.clone()),
            ..item
        };
        match (a, b) {
            (None, Some(_)) => sink.items.push(sink.base("added")),
            (Some(_), None) => sink.items.push(sink.base("removed")),
            (Some(x), Some(y)) => {
                sink.field(
                    "assignment_type",
                    Some(format!("{:?}", x.assignment_type).to_lowercase()),
                    Some(format!("{:?}", y.assignment_type).to_lowercase()),
                    (None, None),
                );
                sink.field(
                    "share",
                    Some(x.share.to_string()),
                    Some(y.share.to_string()),
                    (None, None),
                );
                sink.field(
                    "is_primary",
                    Some(x.is_primary.to_string()),
                    Some(y.is_primary.to_string()),
                    (None, None),
                );
            }
            (None, None) => {}
        }
        out.extend(sink.items.into_iter().map(person));
    }
}

/// Units in the subtree of `root`, following the parent links of both views.
fn subtree(root: &str, views: [&OrgStructureView; 2]) -> HashSet<String> {
    let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
    for view in views {
        for u in &view.units {
            if let Some(parent) = &u.parent_unit_id {
                children
                    .entry(parent.as_str())
                    .or_default()
                    .push(u.unit_id.as_str());
            }
        }
    }
    let mut seen = HashSet::from([root.to_string()]);
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        for child in children.get(id).into_iter().flatten() {
            if seen.insert((*child).to_string()) {
                stack.push(child);
            }
        }
    }
    seen
}

/// What turns `from` into `to`: units, positions and who holds them. The
/// people are left out (not masked) for a caller who may not see them, and
/// then head and deputy labels name the positions instead of their holders.
pub fn diff_views(
    from: &OrgStructureView,
    to: &OrgStructureView,
    privacy: &Privacy<'_>,
    ctx: &DiffContext<'_>,
) -> Vec<OrgDiffItem> {
    let la = Labels::new(from, privacy.personal_visible);
    let lb = Labels::new(to, privacy.personal_visible);
    let mut out = Vec::new();

    let units_a: BTreeMap<&str, &OrgUnit> =
        from.units.iter().map(|u| (u.unit_id.as_str(), u)).collect();
    let units_b: BTreeMap<&str, &OrgUnit> =
        to.units.iter().map(|u| (u.unit_id.as_str(), u)).collect();
    let unit_ids: BTreeSet<&str> = units_a.keys().chain(units_b.keys()).copied().collect();
    for id in unit_ids {
        diff_unit(
            units_a.get(id).map(|u| (*u, &la)),
            units_b.get(id).map(|u| (*u, &lb)),
            ctx,
            &mut out,
        );
    }

    let pos_a: BTreeMap<&str, &OrgPosition> = from
        .positions
        .iter()
        .map(|p| (p.position_id.as_str(), p))
        .collect();
    let pos_b: BTreeMap<&str, &OrgPosition> = to
        .positions
        .iter()
        .map(|p| (p.position_id.as_str(), p))
        .collect();
    let position_ids: BTreeSet<&str> = pos_a.keys().chain(pos_b.keys()).copied().collect();
    for id in position_ids {
        diff_position(
            pos_a.get(id).map(|p| (*p, &la)),
            pos_b.get(id).map(|p| (*p, &lb)),
            &mut out,
        );
    }

    diff_assignments(from, to, privacy, &la, &lb, &mut out);

    if let Some(root) = ctx.unit_id {
        let within = subtree(root, [from, to]);
        out.retain(|item| item.unit_id.as_deref().is_some_and(|u| within.contains(u)));
    }
    out
}

// ---------------------------------------------------------------------------
// The audit trail
// ---------------------------------------------------------------------------

/// The most audit rows one call reads: the filter is applied in SQL, so this
/// only bounds a request for the whole history of a very old organization.
const MAX_AUDIT_ROWS: usize = 20_000;
pub const DEFAULT_PAGE: usize = 50;
pub const MAX_PAGE: usize = 200;

/// Actions of the structure; `org.` alone would also match membership audit.
const ACTION_FILTER: &str = "(action LIKE 'org.unit%' OR action LIKE 'org.position%' \
    OR action LIKE 'org.reporting_line%' OR action LIKE 'org.assignment%' \
    OR action LIKE 'org.external_person%' OR action LIKE 'org.settings.%' \
    OR action LIKE 'org.structure.%' OR action LIKE 'org.change_set.%')";

#[derive(Debug, Clone, Default)]
pub struct HistoryQuery {
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    pub unit_id: Option<String>,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug)]
pub struct HistoryPage {
    pub entries: Vec<OrgHistoryEntry>,
    pub total: usize,
    pub today: NaiveDate,
}

#[derive(Default)]
struct Names {
    units: HashMap<String, String>,
    unit_edges: Vec<(String, String)>,
    positions: HashMap<String, String>,
    position_unit: HashMap<String, String>,
    unit_types: HashMap<String, String>,
    /// assignment id -> (position id, subject)
    assignments: HashMap<String, (String, OrgSubject)>,
    users: HashMap<String, String>,
    externals: HashMap<String, String>,
}

impl Names {
    fn load(conn: &Connection, org_id: &str) -> Result<Self> {
        let mut names = Names::default();
        // Oldest version first, so the newest name wins.
        let mut stmt = conn.prepare(
            "SELECT unit_id, name, parent_unit_id FROM org_units WHERE org_id = ?1 ORDER BY valid_from",
        )?;
        for row in stmt.query_map([org_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })? {
            let (id, name, parent) = row?;
            if let Some(parent) = parent {
                names.unit_edges.push((parent, id.clone()));
            }
            names.units.insert(id, name);
        }
        let mut stmt = conn.prepare(
            "SELECT position_id, name, unit_id FROM org_positions WHERE org_id = ?1 ORDER BY valid_from",
        )?;
        for row in stmt.query_map([org_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })? {
            let (id, name, unit) = row?;
            names.positions.insert(id.clone(), name);
            names.position_unit.insert(id, unit);
        }
        let mut stmt = conn.prepare("SELECT id, name FROM org_unit_types WHERE org_id = ?1")?;
        for row in stmt.query_map([org_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })? {
            let (id, name) = row?;
            names.unit_types.insert(id, name);
        }
        let mut stmt = conn.prepare(
            "SELECT id, position_id, user_id, external_person_id FROM org_assignments WHERE org_id = ?1",
        )?;
        for row in stmt.query_map([org_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })? {
            let (id, position, user, external) = row?;
            let subject = match (user, external) {
                (Some(user), _) => OrgSubject::User(user),
                (None, Some(external)) => OrgSubject::External(external),
                (None, None) => continue,
            };
            names.assignments.insert(id, (position, subject));
        }
        let mut stmt = conn.prepare(
            "SELECT u.id, COALESCE(NULLIF(u.display_name, ''), u.username) FROM user_accounts u",
        )?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
            let (id, name) = row?;
            names.users.insert(id, name);
        }
        let mut stmt =
            conn.prepare("SELECT id, display_name FROM org_external_persons WHERE org_id = ?1")?;
        for row in stmt.query_map([org_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })? {
            let (id, name) = row?;
            names.externals.insert(id, name);
        }
        Ok(names)
    }

    fn subject_name(&self, subject: &OrgSubject) -> Option<String> {
        match subject {
            OrgSubject::User(id) => self.users.get(id).cloned(),
            OrgSubject::External(id) => self.externals.get(id).cloned(),
        }
    }

    fn unit_of(&self, kind: &str, id: &str) -> Option<String> {
        match kind {
            "unit" => Some(id.to_string()),
            "position" => self.position_unit.get(id).cloned(),
            "assignment" => self
                .assignments
                .get(id)
                .and_then(|(position, _)| self.position_unit.get(position).cloned()),
            _ => None,
        }
    }

    fn unit_subtree(&self, root: &str) -> HashSet<String> {
        let mut children: HashMap<&str, Vec<&str>> = HashMap::new();
        for (parent, child) in &self.unit_edges {
            children
                .entry(parent.as_str())
                .or_default()
                .push(child.as_str());
        }
        let mut seen = HashSet::from([root.to_string()]);
        let mut stack = vec![root.to_string()];
        while let Some(id) = stack.pop() {
            for child in children.get(id.as_str()).into_iter().flatten() {
                if seen.insert((*child).to_string()) {
                    stack.push((*child).to_string());
                }
            }
        }
        seen
    }

    fn label(&self, field: &str, value: &str) -> Option<String> {
        match field {
            "parent_unit_id" | "unit_id" => self.units.get(value).cloned(),
            "head_position_id" | "parent_position_id" | "position_id" => {
                self.positions.get(value).cloned()
            }
            "type_id" => self.unit_types.get(value).cloned(),
            "deputy_head_position_ids" => Some(
                value
                    .split(',')
                    .filter_map(|id| self.positions.get(id))
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            _ => None,
        }
    }
}

struct AuditRow {
    id: i64,
    timestamp: String,
    user_id: Option<String>,
    action: String,
    resource: Option<String>,
    details: Json,
}

/// `org_unit:<id>` -> (`unit`, `<id>`).
fn split_target(resource: &str) -> (String, String) {
    match resource.split_once(':') {
        Some((kind, id)) => (kind.trim_start_matches("org_").to_string(), id.to_string()),
        None => ("structure".to_string(), resource.to_string()),
    }
}

fn scalar(value: &Json) -> Option<String> {
    match value {
        Json::Null => None,
        Json::String(s) => Some(s.clone()),
        Json::Array(items) => {
            let parts: Vec<String> = items.iter().filter_map(scalar).collect();
            (!parts.is_empty()).then(|| parts.join(","))
        }
        other => Some(other.to_string()),
    }
}

fn change(
    names: &Names,
    field: &str,
    before: Option<String>,
    after: Option<String>,
) -> OrgFieldChange {
    OrgFieldChange {
        field: field.to_string(),
        before_label: before.as_deref().and_then(|v| names.label(field, v)),
        after_label: after.as_deref().and_then(|v| names.label(field, v)),
        before,
        after,
    }
}

/// The fields an audit entry says changed. An update lists `changes` (field ->
/// before/after), a move or a head change `before` and `after` objects, a
/// deputy list two arrays, a creation the facts it was made with (after only).
fn field_changes(names: &Names, action: &str, details: &Json) -> Vec<OrgFieldChange> {
    let mut out = Vec::new();
    if let Some(changes) = details.get("changes").and_then(Json::as_object) {
        for (field, pair) in changes {
            out.push(change(
                names,
                field,
                pair.get("before").and_then(scalar),
                pair.get("after").and_then(scalar),
            ));
        }
        return out;
    }
    match (details.get("before"), details.get("after")) {
        (Some(Json::Object(before)), Some(Json::Object(after))) => {
            let fields: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
            for field in fields {
                out.push(change(
                    names,
                    field,
                    before.get(field).and_then(scalar),
                    after.get(field).and_then(scalar),
                ));
            }
        }
        (Some(before), Some(after)) => {
            let field = if action.ends_with("deputies_set") {
                "deputy_head_position_ids"
            } else if action.contains("timezone") {
                "timezone"
            } else {
                "value"
            };
            out.push(change(names, field, scalar(before), scalar(after)));
        }
        _ => {
            const FACTS: [&str; 7] = [
                "parent_unit_id",
                "unit_id",
                "parent_position_id",
                "position_id",
                "type",
                "share",
                "is_primary",
            ];
            if action.ends_with(".create") || action.ends_with(".set") {
                for field in FACTS {
                    if let Some(after) = details.get(field).and_then(scalar) {
                        // A position's own id is the entry's target, not a change.
                        if field == "position_id" && action.starts_with("org.reporting_line") {
                            continue;
                        }
                        out.push(change(names, field, None, Some(after)));
                    }
                }
                if let Some(kind) = details.get("kind").and_then(scalar) {
                    if action.starts_with("org.reporting_line") {
                        out.push(change(names, "line_kind", None, Some(kind)));
                    }
                }
                if let Some(name) = details.get("name").and_then(scalar) {
                    out.push(change(names, "name", None, Some(name)));
                }
            }
        }
    }
    out
}

fn is_personal_action(action: &str) -> bool {
    action.starts_with("org.assignment") || action.starts_with("org.external_person")
}

fn operations(names: &Names, details: &Json, privacy: &Privacy<'_>) -> (Vec<OrgHistoryOp>, u32) {
    let mut ops = Vec::new();
    let mut hidden = 0;
    for pair in details
        .get("operations")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(action), Some(target)) = (
            pair.get(0).and_then(Json::as_str),
            pair.get(1).and_then(Json::as_str),
        ) else {
            continue;
        };
        let (kind, id) = split_target(target);
        let visible = if is_personal_action(action) || kind == "user" {
            match kind.as_str() {
                "assignment" => names
                    .assignments
                    .get(&id)
                    .is_some_and(|(_, subject)| privacy.may_see(subject)),
                "user" => privacy.personal_visible || id == privacy.viewer_user_id,
                _ => privacy.personal_visible,
            }
        } else {
            true
        };
        if !visible {
            hidden += 1;
            continue;
        }
        let target_name = match kind.as_str() {
            "unit" => names.units.get(&id).cloned(),
            "position" => names.positions.get(&id).cloned(),
            "assignment" => names
                .assignments
                .get(&id)
                .and_then(|(position, _)| names.positions.get(position).cloned()),
            "unit_type" => names.unit_types.get(&id).cloned(),
            _ => None,
        };
        ops.push(OrgHistoryOp {
            action: action.to_string(),
            target_kind: kind,
            target_id: id,
            target_name,
        });
    }
    (ops, hidden)
}

/// The person an `org.assignment.create` entry records (`subject: { kind, id }`).
fn subject_in(details: &Json) -> Option<OrgSubject> {
    let subject = details.get("subject")?;
    let id = subject.get("id")?.as_str()?.to_string();
    match subject.get("kind")?.as_str()? {
        "user_id" => Some(OrgSubject::User(id)),
        "external_person_id" => Some(OrgSubject::External(id)),
        _ => None,
    }
}

fn day_of(row: &AuditRow) -> String {
    for key in ["from", "valid_from", "preview_at", "as_of"] {
        if let Some(day) = row.details.get(key).and_then(Json::as_str) {
            return day.to_string();
        }
    }
    row.timestamp.chars().take(10).collect()
}

/// One audit row as a history entry, or `None` when the caller may not see it.
fn entry_of(row: &AuditRow, names: &Names, privacy: &Privacy<'_>) -> Option<OrgHistoryEntry> {
    let resource = row.resource.clone().unwrap_or_default();
    let (target_kind, target_id) = split_target(&resource);
    let mut entry = OrgHistoryEntry {
        id: row.id,
        at: row.timestamp.clone(),
        actor_user_id: row.user_id.clone(),
        actor_name: row
            .user_id
            .as_ref()
            .and_then(|u| names.users.get(u).cloned()),
        action: row.action.clone(),
        effective_date: Some(day_of(row)),
        ..Default::default()
    };

    if row.action.starts_with("org.change_set.") {
        // A plan is an administrator's document until it is applied.
        if !privacy.personal_visible {
            return None;
        }
        entry.target_kind = "change_set".to_string();
        entry.target_id = target_id;
        entry.target_name = row.details.get("name").and_then(scalar);
        return Some(entry);
    }

    if row.action.starts_with("org.structure.") {
        // The batch that applied a planned reorganization belongs to the plan, which is an
        // administrator's document in every state (§2.4), withdrawn ones included.
        let from_change_set = row.details.get("change_set").and_then(Json::as_bool) == Some(true);
        if from_change_set && !privacy.personal_visible {
            return None;
        }
        let (ops, hidden) = operations(names, &row.details, privacy);
        if ops.is_empty() && hidden > 0 {
            return None;
        }
        entry.target_kind = "structure".to_string();
        entry.target_id = target_id;
        entry.ops = ops;
        entry.hidden_ops = hidden;
        entry.source = Some(row.action.rsplit('.').next().unwrap_or("batch").to_string());
        return Some(entry);
    }

    entry.target_kind = target_kind.clone();
    entry.target_id = target_id.clone();
    match target_kind.as_str() {
        "assignment" => {
            let (position, subject) = names.assignments.get(&target_id).cloned().unzip();
            // A row taken back the same day is gone, but the entry that made it names the person.
            let subject = subject.or_else(|| subject_in(&row.details));
            match &subject {
                Some(subject) if privacy.may_see(subject) => {}
                Some(_) => return None,
                None if privacy.personal_visible => {}
                None => return None,
            }
            entry.subject_name = subject.as_ref().and_then(|s| names.subject_name(s));
            entry.subject = subject;
            entry.position_name = position
                .as_ref()
                .and_then(|p| names.positions.get(p).cloned());
            entry.target_name = entry.position_name.clone();
            entry.position_id = position;
        }
        "external_person" => {
            if !privacy.personal_visible {
                return None;
            }
            entry.target_name = names.externals.get(&target_id).cloned();
        }
        "user" => {
            // A person's assignments ended because they left: their own history.
            if !(privacy.personal_visible || target_id == privacy.viewer_user_id) {
                return None;
            }
            entry.subject = Some(OrgSubject::User(target_id.clone()));
            entry.subject_name = names.users.get(&target_id).cloned();
        }
        "unit" => entry.target_name = names.units.get(&target_id).cloned(),
        "position" => entry.target_name = names.positions.get(&target_id).cloned(),
        "unit_type" => {
            entry.target_name = names
                .unit_types
                .get(&target_id)
                .cloned()
                .or_else(|| row.details.get("name").and_then(scalar));
        }
        _ => {}
    }
    entry.unit_id = names.unit_of(&target_kind, &target_id);
    entry.unit_name = entry
        .unit_id
        .as_ref()
        .and_then(|u| names.units.get(u).cloned());
    entry.changes = field_changes(names, &row.action, &row.details);
    Some(entry)
}

fn read_audit(
    conn: &Connection,
    org_id: &str,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> Result<Vec<AuditRow>> {
    let sql = format!(
        "WITH a AS (SELECT id, timestamp, user_id, action, resource, \
            CASE WHEN json_valid(details) THEN details ELSE '{{}}' END AS d \
            FROM audit_log WHERE details IS NOT NULL AND {ACTION_FILTER}), \
         b AS (SELECT id, timestamp, user_id, action, resource, d, \
            COALESCE(json_extract(d, '$.from'), json_extract(d, '$.valid_from'), \
                     json_extract(d, '$.preview_at'), json_extract(d, '$.as_of'), \
                     substr(timestamp, 1, 10)) AS eff \
            FROM a WHERE json_extract(d, '$.org_id') = ?1) \
         SELECT id, timestamp, user_id, action, resource, d FROM b \
         WHERE (?2 IS NULL OR eff >= ?2) AND (?3 IS NULL OR eff <= ?3) \
         ORDER BY eff DESC, id DESC LIMIT {MAX_AUDIT_ROWS}"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(
            rusqlite::params![
                org_id,
                from.map(validate::format_date),
                to.map(validate::format_date)
            ],
            |r| {
                let details: String = r.get(5)?;
                Ok(AuditRow {
                    id: r.get(0)?,
                    timestamp: r.get(1)?,
                    user_id: r.get(2)?,
                    action: r.get(3)?,
                    resource: r.get(4)?,
                    details: serde_json::from_str(&details).unwrap_or(Json::Null),
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The changes of the structure that `privacy` may see, newest effective day
/// first, cut to one page.
pub fn list_changes(
    pool: &DbPool,
    org_id: &str,
    privacy: &Privacy<'_>,
    query: &HistoryQuery,
) -> Result<HistoryPage> {
    let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
    let today = validate::today_in_zone(&timezone_of(&conn, org_id)?)?;
    let names = Names::load(&conn, org_id)?;
    let rows = read_audit(&conn, org_id, query.from, query.to)?;
    let within = query
        .unit_id
        .as_deref()
        .map(|unit| names.unit_subtree(unit));

    let mut entries: Vec<OrgHistoryEntry> = rows
        .iter()
        .filter_map(|row| entry_of(row, &names, privacy))
        .filter(|entry| match &within {
            None => true,
            Some(units) => {
                let own = entry.unit_id.as_ref().is_some_and(|u| units.contains(u));
                let of_ops = entry.ops.iter().any(|op| {
                    names
                        .unit_of(&op.target_kind, &op.target_id)
                        .is_some_and(|u| units.contains(&u))
                });
                own || of_ops
            }
        })
        .collect();
    let total = entries.len();
    let limit = match query.limit {
        0 => DEFAULT_PAGE,
        n => n.min(MAX_PAGE),
    };
    entries = entries.into_iter().skip(query.offset).take(limit).collect();
    Ok(HistoryPage {
        entries,
        total,
        today,
    })
}

/// Unit type names of the organization, for the labels of a diff.
pub fn unit_type_names(pool: &DbPool, org_id: &str) -> Result<HashMap<String, String>> {
    Ok(super::repo::list_unit_types(pool, org_id)?
        .into_iter()
        .map(|t| (t.id, t.name))
        .collect())
}

/// A snapshot of a PAST day shows who held what then — the position history
/// of every person (§6.3). For a viewer who may not see it, the people on it are
/// replaced by "Osoba #n" (numbered in this answer only, so nothing links the
/// same person across two answers), except the viewer's own seats; the
/// warnings about a hidden person go with them. Positions, units and lines are
/// untouched: the structure itself is open to every member. `today` is the
/// organization's day; a snapshot of today or later is not history.
pub fn pseudonymize_past_holders(
    view: &mut OrgStructureView,
    today: NaiveDate,
    privacy: &Privacy<'_>,
) {
    if privacy.personal_visible {
        return;
    }
    let Ok(day) = validate::parse_date(&view.at) else {
        return;
    };
    if day >= today {
        return;
    }
    let mut numbers: HashMap<OrgSubject, usize> = HashMap::new();
    for assignment in &mut view.assignments {
        if privacy.may_see(&assignment.subject) {
            continue;
        }
        let next = numbers.len() + 1;
        let number = *numbers.entry(assignment.subject.clone()).or_insert(next);
        assignment.subject = OrgSubject::External(format!("hidden:{number}"));
        assignment.display_name = format!("Osoba #{number}");
    }
    view.warnings.retain(|warning| match warning {
        OrgWarning::UnitWithoutHead { .. } => true,
        OrgWarning::ShareOverbooked { subject, .. }
        | OrgWarning::PersonWithoutPrimary { subject, .. } => privacy.may_see(subject),
    });
}
