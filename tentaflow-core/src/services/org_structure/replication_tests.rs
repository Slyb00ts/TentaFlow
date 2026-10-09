//! What a peer may and may not write through the replication of the
//! structure's tables: the organization is the envelope's, never the row's.

use rusqlite::Transaction;
use tempfile::TempDir;

use super::availability::AbsenceKind;
use super::replication as repl;
use super::tests::{captures_of, operation_from, Fixture, ORG};
use super::*;
use crate::services::org;
use crate::sync::core_registry::CoreSyncResourceKind as Kind;
use crate::sync::ledger::{ActionType, FieldValue};

struct Peer {
    _dir: TempDir,
    pool: crate::db::DbPool,
    other_org: String,
}

fn peer() -> Peer {
    let dir = TempDir::new().unwrap();
    let pool = crate::db::init(&dir.path().join("peer.db")).unwrap();
    let other_org = org::create_organization(&pool, "Other", "other", None, None, None, None)
        .unwrap()
        .org_id;
    Peer {
        _dir: dir,
        pool,
        other_org,
    }
}

fn units_named(pool: &crate::db::DbPool, org_id: &str) -> Vec<String> {
    let conn = pool.read().unwrap();
    let mut stmt = conn
        .prepare("SELECT name FROM org_units WHERE org_id = ?1 ORDER BY name")
        .unwrap();
    stmt.query_map([org_id], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

fn apply_in(
    pool: &crate::db::DbPool,
    operation: &crate::sync::ledger::SyncOperation,
) -> crate::sync::ledger::LedgerResult<usize> {
    let mut conn = pool.write().unwrap();
    let tx: Transaction<'_> = conn.transaction().unwrap();
    let rows = repl::apply(&tx, Kind::OrgUnit, operation)?;
    tx.commit().unwrap();
    Ok(rows)
}

#[test]
fn a_row_that_names_another_organization_than_its_envelope_is_refused() {
    let f = Fixture::new();
    f.unit("Board", None);
    let capture = captures_of(&f.pool, "core.org_unit").remove(0);
    let peer = peer();

    let mut operation = operation_from(&capture);
    operation.body.org_id = peer.other_org.clone();
    let refused = apply_in(&peer.pool, &operation);
    assert!(refused.is_err(), "{refused:?}");
    assert!(units_named(&peer.pool, &peer.other_org).is_empty());
    assert!(units_named(&peer.pool, ORG).is_empty());

    let honest = operation_from(&capture);
    assert_eq!(apply_in(&peer.pool, &honest).unwrap(), 1);
    assert_eq!(units_named(&peer.pool, ORG), ["Board"]);
}

#[test]
fn an_upsert_never_takes_over_a_row_of_another_organization() {
    let f = Fixture::new();
    f.unit("Board", None);
    let capture = captures_of(&f.pool, "core.org_unit").remove(0);
    let peer = peer();
    apply_in(&peer.pool, &operation_from(&capture)).unwrap();

    // The same row id, claimed by the other organization's own envelope and row.
    let mut hijack = operation_from(&capture);
    hijack.body.org_id = peer.other_org.clone();
    hijack.body.changed_fields.insert(
        "org_id".to_string(),
        FieldValue::String(peer.other_org.clone()),
    );
    hijack
        .body
        .changed_fields
        .insert("name".to_string(), FieldValue::String("Hijacked".into()));
    assert_eq!(apply_in(&peer.pool, &hijack).unwrap(), 0);
    assert_eq!(units_named(&peer.pool, ORG), ["Board"]);
    assert!(units_named(&peer.pool, &peer.other_org).is_empty());
}

#[test]
fn a_delete_reaches_only_the_row_of_the_envelopes_organization() {
    let f = Fixture::new();
    f.unit("Board", None);
    let capture = captures_of(&f.pool, "core.org_unit").remove(0);
    let peer = peer();
    apply_in(&peer.pool, &operation_from(&capture)).unwrap();

    let mut foreign = operation_from(&capture);
    foreign.body.action = ActionType::Delete;
    foreign.body.changed_fields.clear();
    foreign.body.org_id = peer.other_org.clone();
    assert_eq!(apply_in(&peer.pool, &foreign).unwrap(), 0);
    assert_eq!(units_named(&peer.pool, ORG), ["Board"]);

    let mut own = foreign.clone();
    own.body.org_id = ORG.to_string();
    assert_eq!(apply_in(&peer.pool, &own).unwrap(), 1);
    assert!(units_named(&peer.pool, ORG).is_empty());
}

#[test]
fn a_settings_row_of_another_organization_cannot_be_written_under_this_envelope() {
    let f = Fixture::new();
    set_timezone(&f.pool, &f.ctx(), "Europe/London").unwrap();
    let capture = captures_of(&f.pool, "core.org_structure_settings")
        .pop()
        .unwrap();
    let peer = peer();

    let mut operation = operation_from(&capture);
    operation.body.resource_id = peer.other_org.clone();
    let mut conn = peer.pool.write().unwrap();
    let tx = conn.transaction().unwrap();
    assert!(repl::apply(&tx, Kind::OrgStructureSettings, &operation).is_err());
}

#[test]
fn an_absence_operation_from_an_older_node_cannot_bring_a_reason_back() {
    let f = Fixture::new();
    let member = super::tests::add_user(&f.pool, ORG, "anna");
    add_absence(
        &f.pool,
        &f.confirmed(),
        Actor { is_admin: true },
        &NewAbsence {
            user_id: member.clone(),
            valid_from: super::tests::day(1),
            valid_to: Some(super::tests::day(3)),
            kind: AbsenceKind::Leave,
        },
    )
    .unwrap();
    let capture = captures_of(&f.pool, "core.org_absence").remove(0);
    let peer = peer();
    // The peer node is also a source of this operation: it still sends a reason.
    {
        let conn = peer.pool.write().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = OFF;").unwrap();
    }
    let mut operation = operation_from(&capture);
    operation.body.changed_fields.insert(
        "reason".to_string(),
        FieldValue::String("dentist".to_string()),
    );
    let mut conn = peer.pool.write().unwrap();
    let tx = conn.transaction().unwrap();
    assert_eq!(repl::apply(&tx, Kind::OrgAbsence, &operation).unwrap(), 1);
    tx.commit().unwrap();
    let has_reason: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('org_absences') WHERE name = 'reason')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!has_reason);
    let stored: String = conn
        .query_row("SELECT user_id FROM org_absences", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stored, member);
}
