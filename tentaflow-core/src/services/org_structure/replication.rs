//! Replication of the eleven org-structure tables.
//!
//! Every table is one row shape: a text primary key, `org_id`, and plain
//! columns. That lets a single description of the columns drive all three
//! places that must agree — the capture a write records, the materializer that
//! applies it on a peer, and the baseline reseed — instead of nine hand-kept
//! copies of the same column list.

use std::collections::BTreeMap;

use rusqlite::types::{Value, ValueRef};
use rusqlite::Transaction;

use crate::sync::core_registry::CoreSyncResourceKind as Kind;
use crate::sync::ledger::{ActionType, FieldValue, LedgerResult, SyncLedgerError, SyncOperation};
use crate::sync::runtime::SqlWriteAction;

pub(crate) struct TableSpec {
    pub kind: Kind,
    pub table: &'static str,
    pub pk: &'static str,
    /// Every column in the table, primary key included. Must match migrations 180 and 181.
    pub columns: &'static [&'static str],
}

pub(crate) const UNIT_TYPES: TableSpec = TableSpec {
    kind: Kind::OrgUnitType,
    table: "org_unit_types",
    pk: "id",
    columns: &["id", "org_id", "name", "color", "icon"],
};

pub(crate) const UNITS: TableSpec = TableSpec {
    kind: Kind::OrgUnit,
    table: "org_units",
    pk: "id",
    columns: &[
        "id",
        "org_id",
        "unit_id",
        "name",
        "code",
        "type_id",
        "parent_unit_id",
        "color",
        "head_position_id",
        "valid_from",
        "valid_to",
    ],
};

pub(crate) const POSITIONS: TableSpec = TableSpec {
    kind: Kind::OrgPosition,
    table: "org_positions",
    pk: "id",
    columns: &[
        "id",
        "org_id",
        "position_id",
        "unit_id",
        "name",
        "code",
        "role_id",
        "is_manager",
        "is_staff",
        "valid_from",
        "valid_to",
    ],
};

pub(crate) const DEPUTY_HEADS: TableSpec = TableSpec {
    kind: Kind::OrgUnitDeputyHead,
    table: "org_unit_deputy_heads",
    pk: "id",
    columns: &[
        "id",
        "org_id",
        "unit_id",
        "position_id",
        "ord",
        "valid_from",
        "valid_to",
    ],
};

pub(crate) const REPORTING_LINES: TableSpec = TableSpec {
    kind: Kind::OrgReportingLine,
    table: "org_reporting_lines",
    pk: "id",
    columns: &[
        "id",
        "org_id",
        "position_id",
        "parent_position_id",
        "kind",
        "priority",
        "valid_from",
        "valid_to",
    ],
};

pub(crate) const EXTERNAL_PERSONS: TableSpec = TableSpec {
    kind: Kind::OrgExternalPerson,
    table: "org_external_persons",
    pk: "id",
    columns: &["id", "org_id", "display_name", "email", "note"],
};

pub(crate) const ASSIGNMENTS: TableSpec = TableSpec {
    kind: Kind::OrgAssignment,
    table: "org_assignments",
    pk: "id",
    columns: &[
        "id",
        "org_id",
        "position_id",
        "user_id",
        "external_person_id",
        "type",
        "share",
        "is_primary",
        "valid_from",
        "valid_to",
    ],
};

pub(crate) const SETTINGS: TableSpec = TableSpec {
    kind: Kind::OrgStructureSettings,
    table: "org_structure_settings",
    pk: "org_id",
    columns: &["org_id", "timezone"],
};

pub(crate) const CHANGE_SETS: TableSpec = TableSpec {
    kind: Kind::OrgChangeSet,
    table: "org_change_sets",
    pk: "id",
    columns: &[
        "id",
        "org_id",
        "name",
        "effective_date",
        "state",
        "author_user_id",
        "approver_user_id",
        "created_at_ms",
        "payload",
    ],
};

pub(crate) const DEPUTIES: TableSpec = TableSpec {
    kind: Kind::OrgDeputy,
    table: "org_deputies",
    pk: "id",
    columns: &[
        "id",
        "org_id",
        "user_id",
        "deputy_user_id",
        "scope",
        "valid_from",
        "valid_to",
        "created_by",
    ],
};

pub(crate) const ABSENCES: TableSpec = TableSpec {
    kind: Kind::OrgAbsence,
    table: "org_absences",
    pk: "id",
    columns: &[
        "id",
        "org_id",
        "user_id",
        "valid_from",
        "valid_to",
        "kind",
        "source",
        "created_by",
    ],
};

const ALL: [&TableSpec; 11] = [
    &UNIT_TYPES,
    &UNITS,
    &POSITIONS,
    &DEPUTY_HEADS,
    &REPORTING_LINES,
    &EXTERNAL_PERSONS,
    &ASSIGNMENTS,
    &SETTINGS,
    &CHANGE_SETS,
    &DEPUTIES,
    &ABSENCES,
];

pub(crate) fn all_specs() -> &'static [&'static TableSpec] {
    &ALL
}

pub(crate) fn spec_for(kind: Kind) -> &'static TableSpec {
    ALL.iter()
        .copied()
        .find(|spec| spec.kind == kind)
        .expect("every org-structure kind has a table spec")
}

/// The row as the ledger carries it, or `None` when it no longer exists.
pub(crate) fn read_fields(
    tx: &Transaction<'_>,
    spec: &TableSpec,
    id: &str,
) -> rusqlite::Result<Option<BTreeMap<String, FieldValue>>> {
    let sql = format!(
        "SELECT {} FROM {} WHERE {} = ?1",
        spec.columns.join(", "),
        spec.table,
        spec.pk
    );
    let mut stmt = tx.prepare(&sql)?;
    let mut rows = stmt.query([id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    let mut fields = BTreeMap::new();
    for (index, column) in spec.columns.iter().enumerate() {
        let value = match row.get_ref(index)? {
            ValueRef::Null => FieldValue::Null,
            ValueRef::Integer(v) => FieldValue::I64(v),
            // Reals ride the wire as exact decimal strings, like every other core kind.
            ValueRef::Real(v) => FieldValue::Decimal(v.to_string()),
            ValueRef::Text(t) => FieldValue::String(String::from_utf8_lossy(t).into_owned()),
            ValueRef::Blob(_) => {
                return Err(rusqlite::Error::InvalidColumnType(
                    index,
                    (*column).to_string(),
                    rusqlite::types::Type::Blob,
                ))
            }
        };
        fields.insert((*column).to_string(), value);
    }
    Ok(Some(fields))
}

/// Records the capture for one row: its current content, or a tombstone when
/// the row is gone.
pub(crate) fn capture_row(
    tx: &Transaction<'_>,
    spec: &TableSpec,
    org_id: &str,
    id: &str,
    action: SqlWriteAction,
    actor_user_id: Option<&str>,
) -> anyhow::Result<()> {
    let fields = match action {
        SqlWriteAction::Delete => BTreeMap::new(),
        _ => read_fields(tx, spec, id)?.ok_or_else(|| {
            anyhow::anyhow!("{} row {id} vanished before its capture", spec.table)
        })?,
    };
    crate::db::repository::record_core_capture_for_org_tx(
        tx,
        spec.kind,
        org_id,
        id.to_string(),
        action,
        fields,
        actor_user_id.map(str::to_string),
    )?;
    Ok(())
}

fn to_sql_value(value: Option<&FieldValue>) -> LedgerResult<Value> {
    Ok(match value {
        None | Some(FieldValue::Null) => Value::Null,
        Some(FieldValue::String(v)) => Value::Text(v.clone()),
        Some(FieldValue::I64(v)) => Value::Integer(*v),
        Some(FieldValue::U64(v)) => Value::Integer(
            i64::try_from(*v).map_err(|e| SyncLedgerError::Runtime(format!("u64 field: {e}")))?,
        ),
        Some(FieldValue::Bool(v)) => Value::Integer(i64::from(*v)),
        Some(FieldValue::Decimal(v)) => Value::Real(
            v.parse::<f64>()
                .map_err(|e| SyncLedgerError::Runtime(format!("decimal field: {e}")))?,
        ),
        Some(FieldValue::Bytes(_)) => {
            return Err(SyncLedgerError::Runtime(
                "org-structure rows carry no binary fields".to_string(),
            ))
        }
    })
}

/// The columns of `table` that name structure rows owned by an organization, as
/// `(column, referenced table, referenced column)`. User ids are deliberately absent:
/// membership is enforced on the local write path, and a former member (or one whose
/// membership has not arrived yet) is a legitimate subject of replicated history.
fn references_of(table: &str) -> &'static [(&'static str, &'static str, &'static str)] {
    match table {
        "org_units" => &[
            ("type_id", "org_unit_types", "id"),
            ("parent_unit_id", "org_units", "unit_id"),
            ("head_position_id", "org_positions", "position_id"),
        ],
        "org_positions" => &[("unit_id", "org_units", "unit_id")],
        "org_unit_deputy_heads" => &[
            ("unit_id", "org_units", "unit_id"),
            ("position_id", "org_positions", "position_id"),
        ],
        "org_reporting_lines" => &[
            ("position_id", "org_positions", "position_id"),
            ("parent_position_id", "org_positions", "position_id"),
        ],
        "org_assignments" => &[
            ("position_id", "org_positions", "position_id"),
            ("external_person_id", "org_external_persons", "id"),
        ],
        _ => &[],
    }
}

/// True when the referenced row exists only in other organizations. A row that has
/// not arrived yet is not refused: operations of different tables are not ordered.
fn names_foreign_row(
    tx: &Transaction<'_>,
    table: &str,
    column: &str,
    value: &str,
    org_id: &str,
) -> rusqlite::Result<bool> {
    tx.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM {table} WHERE {column} = ?1 AND org_id <> ?2) \
             AND NOT EXISTS(SELECT 1 FROM {table} WHERE {column} = ?1 AND org_id = ?2)"
        ),
        [value, org_id],
        |r| r.get(0),
    )
}

/// True when the operation's row key exists, but in another organization: such an
/// operation changed nothing and must not earn a place in the LWW order of the row.
pub fn row_is_owned_elsewhere(
    tx: &Transaction<'_>,
    kind: Kind,
    operation: &SyncOperation,
) -> LedgerResult<bool> {
    let spec = spec_for(kind);
    tx.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM {} WHERE {} = ?1 AND org_id <> ?2)",
            spec.table, spec.pk
        ),
        [&operation.body.resource_id, &operation.body.org_id],
        |r| r.get(0),
    )
    .map_err(|e| SyncLedgerError::Runtime(e.to_string()))
}

/// Applies one replicated operation. The whole row travels on every write, so
/// an upsert is a full replace of the non-key columns.
///
/// The organization comes from the signed envelope (`body.org_id`), never from
/// the row a peer sends: a row that names another organization is refused, an
/// upsert never replaces a row that belongs to another organization, and a
/// delete reaches only the organization's own row. A row whose references name rows
/// of another organization only is refused as well.
pub fn apply(tx: &Transaction<'_>, kind: Kind, operation: &SyncOperation) -> LedgerResult<usize> {
    let spec = spec_for(kind);
    let id = &operation.body.resource_id;
    let org_id = &operation.body.org_id;
    let sql_error = |e: rusqlite::Error| SyncLedgerError::Runtime(e.to_string());
    match operation.body.action {
        ActionType::Delete => tx
            .execute(
                &format!(
                    "DELETE FROM {} WHERE {} = ?1 AND org_id = ?2",
                    spec.table, spec.pk
                ),
                [id, org_id],
            )
            .map_err(sql_error),
        ActionType::Insert | ActionType::Update => {
            let stated = match operation.body.changed_fields.get("org_id") {
                Some(FieldValue::String(stated)) => Some(stated.as_str()),
                _ => None,
            };
            let key_matches = spec.pk != "org_id" || id == org_id;
            if stated != Some(org_id.as_str()) || !key_matches {
                return Err(SyncLedgerError::Runtime(format!(
                    "{} row names a different organization than its operation",
                    spec.table
                )));
            }
            for (column, table, target_column) in references_of(spec.table) {
                let Some(FieldValue::String(value)) = operation.body.changed_fields.get(*column)
                else {
                    continue;
                };
                if names_foreign_row(tx, table, target_column, value, org_id).map_err(sql_error)? {
                    return Err(SyncLedgerError::Runtime(format!(
                        "{} row references {column} of another organization",
                        spec.table
                    )));
                }
            }
            let placeholders: Vec<String> =
                (1..=spec.columns.len()).map(|n| format!("?{n}")).collect();
            let updates: Vec<String> = spec
                .columns
                .iter()
                .filter(|column| **column != spec.pk)
                .map(|column| format!("{column} = excluded.{column}"))
                .collect();
            let sql = format!(
                "INSERT INTO {table} ({cols}) VALUES ({vals}) ON CONFLICT({pk}) DO UPDATE SET {updates} \
                 WHERE {table}.org_id = excluded.org_id",
                table = spec.table,
                cols = spec.columns.join(", "),
                vals = placeholders.join(", "),
                pk = spec.pk,
                updates = updates.join(", "),
            );
            let mut values = Vec::with_capacity(spec.columns.len());
            for column in spec.columns {
                if *column == spec.pk {
                    values.push(Value::Text(id.clone()));
                } else {
                    values.push(to_sql_value(operation.body.changed_fields.get(*column))?);
                }
            }
            tx.execute(&sql, rusqlite::params_from_iter(values))
                .map_err(sql_error)
        }
    }
}

/// Baseline: one Insert capture per existing row, so a node that joins later
/// receives the structure it never saw being built.
pub fn reseed(tx: &Transaction<'_>, kind: Kind) -> anyhow::Result<usize> {
    let spec = spec_for(kind);
    let keys: Vec<(String, String)> = {
        let mut stmt = tx.prepare(&format!(
            "SELECT {pk}, org_id FROM {table} ORDER BY {pk}",
            pk = spec.pk,
            table = spec.table
        ))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };
    for (id, org_id) in &keys {
        capture_row(tx, spec, org_id, id, SqlWriteAction::Insert, None)?;
    }
    Ok(keys.len())
}
