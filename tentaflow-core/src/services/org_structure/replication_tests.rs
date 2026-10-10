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

fn versions_of(pool: &crate::db::DbPool, resource_id: &str) -> i64 {
    pool.read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM core_resource_versions WHERE resource_id = ?1",
            [resource_id],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn a_write_the_organization_check_skipped_earns_no_place_in_the_lww_order() {
    let f = Fixture::new();
    f.unit("Board", None);
    let capture = captures_of(&f.pool, "core.org_unit").remove(0);
    let peer = peer();
    apply_in(&peer.pool, &operation_from(&capture)).unwrap();
    assert_eq!(versions_of(&peer.pool, &capture.resource_id), 0);

    // The other organization's envelope names the same row id: the upsert changes nothing.
    let mut hijack = operation_from(&capture);
    hijack.body.org_id = peer.other_org.clone();
    hijack.body.changed_fields.insert(
        "org_id".to_string(),
        FieldValue::String(peer.other_org.clone()),
    );
    assert_eq!(
        crate::sync::core_materializer::apply_core_operation(&peer.pool, &hijack).unwrap(),
        0
    );
    assert_eq!(
        versions_of(&peer.pool, &capture.resource_id),
        0,
        "a version stamped here would let the hijacker outrank the row's own organization"
    );
    assert_eq!(units_named(&peer.pool, ORG), ["Board"]);

    // The row's own organization still replicates it and is ordered normally.
    assert_eq!(
        crate::sync::core_materializer::apply_core_operation(&peer.pool, &operation_from(&capture))
            .unwrap(),
        1
    );
    assert_eq!(versions_of(&peer.pool, &capture.resource_id), 1);
}

#[test]
fn a_row_that_references_a_structure_row_of_another_organization_only_is_refused() {
    let f = Fixture::new();
    let member = super::tests::add_user(&f.pool, ORG, "anna");
    add_absence(
        &f.pool,
        &f.confirmed(),
        Actor { is_admin: true },
        &NewAbsence {
            user_id: member,
            valid_from: super::tests::day(1),
            valid_to: Some(super::tests::day(3)),
            kind: AbsenceKind::Leave,
        },
    )
    .unwrap();
    let capture = captures_of(&f.pool, "core.org_absence").remove(0);
    let peer = peer();
    let stranger = super::tests::add_user(&peer.pool, &peer.other_org, "stranger");

    let apply_absence = |user: &str| {
        let mut operation = operation_from(&capture);
        operation
            .body
            .changed_fields
            .insert("user_id".to_string(), FieldValue::String(user.to_string()));
        let mut conn = peer.pool.write().unwrap();
        let tx = conn.transaction().unwrap();
        repl::apply(&tx, Kind::OrgAbsence, &operation)
    };
    // A user is never refused, whatever the replica knows of the membership: the member of
    // another organization only is a former member here, and an unknown one has not arrived yet.
    assert_eq!(apply_absence(&stranger).unwrap(), 1);
    assert_eq!(apply_absence("not-yet-replicated").unwrap(), 1);

    // The structure's own references are refused when they name another organization's rows.
    {
        let conn = peer.pool.write().unwrap();
        conn.execute(
            "INSERT INTO org_unit_types (id, org_id, name) VALUES ('type-foreign', ?1, 'Foreign')",
            [&peer.other_org],
        )
        .unwrap();
    }
    let unit = {
        f.unit("Board", None);
        captures_of(&f.pool, "core.org_unit").remove(0)
    };
    let mut operation = operation_from(&unit);
    operation.body.changed_fields.insert(
        "type_id".to_string(),
        FieldValue::String("type-foreign".to_string()),
    );
    let refused = apply_in(&peer.pool, &operation);
    assert!(refused.is_err(), "{refused:?}");
    assert!(units_named(&peer.pool, ORG).is_empty());
}

fn kind_of(resource_type: &str) -> Option<Kind> {
    Some(match resource_type {
        "core.org_unit" => Kind::OrgUnit,
        "core.org_position" => Kind::OrgPosition,
        "core.org_assignment" => Kind::OrgAssignment,
        "core.org_change_set" => Kind::OrgChangeSet,
        _ => return None,
    })
}

/// A user the replica knows only as a member of another organization: the shape of
/// a person who left this one (or whose membership has not arrived yet).
fn user_of_the_other_org_only(peer: &Peer) -> String {
    super::tests::add_user(&peer.pool, &peer.other_org, "former")
}

/// Applies the structure captures of `source` in journal order on `peer`, naming
/// `replica_user` wherever the source row names `user`.
fn replicate_structure(source: &Fixture, peer: &Peer, user: &str, replica_user: &str) {
    let mut conn = peer.pool.write().unwrap();
    let mut captures = Vec::new();
    for resource_type in [
        "core.org_unit",
        "core.org_position",
        "core.org_assignment",
        "core.org_change_set",
    ] {
        captures.extend(captures_of(&source.pool, resource_type));
    }
    captures.sort_by_key(|c| (c.hlc.wall_time_ms, c.hlc.logical));
    for capture in captures {
        let mut operation = operation_from(&capture);
        for column in [
            "user_id",
            "author_user_id",
            "approver_user_id",
            "created_by",
            "deputy_user_id",
        ] {
            if operation.body.changed_fields.get(column) == Some(&FieldValue::String(user.into())) {
                operation.body.changed_fields.insert(
                    column.to_string(),
                    FieldValue::String(replica_user.to_string()),
                );
            }
        }
        let tx = conn.transaction().unwrap();
        let kind = kind_of(&capture.resource_type).unwrap();
        repl::apply(&tx, kind, &operation)
            .unwrap_or_else(|e| panic!("{} was refused: {e}", capture.resource_type));
        tx.commit().unwrap();
    }
}

fn running_assignments_of(pool: &crate::db::DbPool, user: &str) -> i64 {
    pool.read()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM org_assignments WHERE user_id = ?1 \
             AND (valid_to IS NULL OR valid_to > date('now', '+1 day'))",
            [user],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn leaving_the_organization_replicates_even_where_the_member_is_already_gone() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let position = f.position(&unit, "CEO", None);
    let anna = super::tests::add_user(&f.pool, ORG, "anna");
    f.assign_user(&position, &anna, 1.0, Some(true)).unwrap();
    // Membership first, then the assignment end it triggers.
    org::remove_membership(&f.pool, ORG, &anna).unwrap();

    let peer = peer();
    let replica_anna = user_of_the_other_org_only(&peer);
    replicate_structure(&f, &peer, &anna, &replica_anna);
    assert_eq!(running_assignments_of(&peer.pool, &replica_anna), 0);
}

#[test]
fn a_change_set_whose_author_later_left_still_replicates() {
    let f = Fixture::new();
    let author = super::tests::add_user(&f.pool, ORG, "author");
    let saved = super::change_set::save(
        &f.pool,
        ORG,
        &author,
        None,
        "Plan",
        super::tests::day(3),
        "[]",
    )
    .unwrap();
    org::remove_membership(&f.pool, ORG, &author).unwrap();

    let peer = peer();
    let replica_author = user_of_the_other_org_only(&peer);
    replicate_structure(&f, &peer, &author, &replica_author);
    let stored: String = peer
        .pool
        .read()
        .unwrap()
        .query_row(
            "SELECT author_user_id FROM org_change_sets WHERE id = ?1",
            [&saved.id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stored, replica_author);
}

#[test]
fn the_baseline_replay_of_rows_about_a_former_member_applies() {
    let f = Fixture::new();
    let unit = f.unit("Board", None);
    let position = f.position(&unit, "CEO", None);
    let anna = super::tests::add_user(&f.pool, ORG, "anna");
    f.assign_user(&position, &anna, 1.0, Some(true)).unwrap();
    org::remove_membership(&f.pool, ORG, &anna).unwrap();
    {
        let mut conn = f.pool.write().unwrap();
        let tx = conn.transaction().unwrap();
        for kind in [Kind::OrgUnit, Kind::OrgPosition, Kind::OrgAssignment] {
            repl::reseed(&tx, kind).unwrap();
        }
        tx.commit().unwrap();
    }

    let peer = peer();
    let replica_anna = user_of_the_other_org_only(&peer);
    replicate_structure(&f, &peer, &anna, &replica_anna);
    assert_eq!(running_assignments_of(&peer.pool, &replica_anna), 0);
    assert_eq!(units_named(&peer.pool, ORG), ["Board"]);
}
