//! Projection of the structure into `sync_user_org_profiles`, the table the
//! permission checks (department scope, manager subtree) read.
//!
//! Only the person's primary assignment counts (docs §1): `department_id` is
//! the unit of that position, `manager_user_id` the holder of the position it
//! reports to on the primary line, `is_department_manager` whether the position
//! is the unit head. A row is written only when a value differs, so a nightly
//! run over an unchanged organization records no sync captures at all.
//!
//! A vacant parent position that heads its unit is answered by the unit's
//! deputy heads (`Snapshot::manager_of`), so a table of deputy heads is an
//! input of the projection. Absences and temporary deputies are not: a week of
//! leave must not move anybody's rights.

use std::collections::{HashMap, HashSet};

use chrono::NaiveDate;
use rusqlite::Transaction;
use serde::Serialize;

use super::error::{OrgStructureError as E, Result};
use super::query::Snapshot;
use super::replication as repl;
use super::validate;
use crate::db::repository::{delete_sync_user_org_profile, upsert_sync_user_org_profile};
use crate::db::DbPool;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RecomputeReport {
    pub written: usize,
    pub removed: usize,
    pub unchanged: usize,
    /// People whose manager was cleared because the positions they hold make
    /// the reporting of PEOPLE circular (A manages B on one position, B manages
    /// A on another). The position tree is acyclic, the person graph need not
    /// be, and the subtree query of the permission check must never loop.
    pub cycles_broken: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Profile {
    department_id: String,
    manager_user_id: Option<String>,
    is_department_manager: bool,
}

/// True when a write that touched `table` can change the projection.
pub(super) fn affects_projection(table: &str) -> bool {
    [
        repl::UNITS.table,
        repl::POSITIONS.table,
        repl::DEPUTY_HEADS.table,
        repl::REPORTING_LINES.table,
        repl::ASSIGNMENTS.table,
        repl::SETTINGS.table,
    ]
    .contains(&table)
}

/// Recomputes profiles for `users` (all of the organization when `None`) as of
/// `at`, in its own transaction. This is the admin "Przelicz" primitive.
pub fn recompute_profiles(
    pool: &DbPool,
    org_id: &str,
    users: Option<&[String]>,
    at: NaiveDate,
) -> Result<RecomputeReport> {
    let mut conn = pool.write().map_err(|e| E::Db(e.to_string()))?;
    let tx = conn.transaction()?;
    let report = recompute_in_tx(&tx, org_id, users, at)?;
    tx.commit()?;
    Ok(report)
}

/// Recomputes the whole organization as of today in its timezone.
pub fn recompute_all(pool: &DbPool, org_id: &str) -> Result<RecomputeReport> {
    let today = {
        let conn = pool.read().map_err(|e| E::Db(e.to_string()))?;
        validate::today_in_zone(&super::repo::timezone_of(&conn, org_id)?)?
    };
    recompute_profiles(pool, org_id, None, today)
}

/// The same inside the transaction of the write that made it necessary. The
/// whole organization is always read (one snapshot: a head change moves every
/// subordinate), `users` only limits which rows are written.
pub fn recompute_in_tx(
    tx: &Transaction<'_>,
    org_id: &str,
    users: Option<&[String]>,
    at: NaiveDate,
) -> Result<RecomputeReport> {
    let snapshot = Snapshot::load(tx, org_id, at)?;
    let members = member_ids(tx, org_id)?;
    let mut desired = desired_profiles(&snapshot, &members);
    let cycles_broken = break_person_cycles(&mut desired);
    let existing = existing_profiles(tx, org_id)?;

    let scope: Option<HashSet<&str>> = users.map(|list| list.iter().map(String::as_str).collect());
    let in_scope = |user: &str| scope.as_ref().is_none_or(|set| set.contains(user));
    let mut report = RecomputeReport {
        cycles_broken,
        ..RecomputeReport::default()
    };

    let mut ids: Vec<&String> = desired.keys().chain(existing.keys()).collect();
    ids.sort_unstable();
    ids.dedup();
    for user_id in ids {
        if !in_scope(user_id) {
            continue;
        }
        match (desired.get(user_id), existing.get(user_id)) {
            (Some(want), Some(have)) if want == have => report.unchanged += 1,
            (Some(want), _) => {
                upsert_sync_user_org_profile(
                    tx,
                    org_id,
                    user_id,
                    Some(&want.department_id),
                    want.manager_user_id.as_deref(),
                    want.is_department_manager,
                )
                .map_err(|e| E::Db(e.to_string()))?;
                report.written += 1;
            }
            // A person with no assignment on the day has no department and no
            // manager, which the permission checks read the same as an absent row.
            (None, Some(_)) => {
                delete_sync_user_org_profile(tx, org_id, user_id)
                    .map_err(|e| E::Db(e.to_string()))?;
                report.removed += 1;
            }
            (None, None) => {}
        }
    }
    if !report.cycles_broken.is_empty() {
        tracing::warn!(
            org_id,
            users = ?report.cycles_broken,
            "org structure: circular person reporting, manager cleared"
        );
    }
    Ok(report)
}

fn member_ids(tx: &Transaction<'_>, org_id: &str) -> Result<HashSet<String>> {
    let mut stmt = tx.prepare("SELECT user_id FROM org_memberships WHERE org_id = ?1")?;
    let ids = stmt
        .query_map([org_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

fn desired_profiles(snapshot: &Snapshot, members: &HashSet<String>) -> HashMap<String, Profile> {
    let mut out = HashMap::new();
    for user_id in snapshot.users() {
        // A former member keeps no profile even if a dated assignment lingers.
        if !members.contains(user_id) {
            continue;
        }
        let Some(assignment) = snapshot.primary_assignment(user_id) else {
            continue;
        };
        let Some(position) = snapshot.position(&assignment.position_id) else {
            continue;
        };
        out.insert(
            user_id.to_string(),
            Profile {
                department_id: position.unit_id.clone(),
                // Same rule as a vacancy: a manager who left the organization
                // leaves the seat effectively empty, nobody is named.
                manager_user_id: snapshot
                    .manager_of(user_id)
                    .map(|m| m.user_id)
                    .filter(|id| members.contains(id)),
                is_department_manager: snapshot.is_head(position),
            },
        );
    }
    out
}

/// Clears one manager in every loop of the person graph (the one with the
/// smallest id, so every node cuts the same edge). Returns who was cleared.
fn break_person_cycles(profiles: &mut HashMap<String, Profile>) -> Vec<String> {
    let mut ids: Vec<String> = profiles.keys().cloned().collect();
    ids.sort_unstable();
    let mut done: HashSet<String> = HashSet::new();
    let mut cleared = Vec::new();
    for start in ids {
        if done.contains(&start) {
            continue;
        }
        let mut path: Vec<String> = Vec::new();
        let mut on_path: HashSet<String> = HashSet::new();
        let mut current = Some(start);
        while let Some(user) = current {
            if done.contains(&user) {
                break;
            }
            if on_path.contains(&user) {
                let from = path.iter().position(|u| *u == user).unwrap_or(0);
                if let Some(cut) = path[from..].iter().min().cloned() {
                    if let Some(profile) = profiles.get_mut(&cut) {
                        profile.manager_user_id = None;
                    }
                    cleared.push(cut);
                }
                break;
            }
            on_path.insert(user.clone());
            path.push(user.clone());
            current = profiles.get(&user).and_then(|p| p.manager_user_id.clone());
        }
        done.extend(path);
    }
    cleared.sort_unstable();
    cleared
}

fn existing_profiles(tx: &Transaction<'_>, org_id: &str) -> Result<HashMap<String, Profile>> {
    let mut stmt = tx.prepare(
        "SELECT user_id, department_id, manager_user_id, is_department_manager \
         FROM sync_user_org_profiles WHERE org_id = ?1",
    )?;
    let rows = stmt
        .query_map([org_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                Profile {
                    // A row without a department is not one this projection wrote.
                    department_id: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    manager_user_id: r.get(2)?,
                    is_department_manager: r.get(3)?,
                },
            ))
        })?
        .collect::<rusqlite::Result<HashMap<_, _>>>()?;
    Ok(rows)
}
