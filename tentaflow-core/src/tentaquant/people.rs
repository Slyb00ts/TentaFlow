// ===== File: tentaquant/people.rs — a lab's people, read out of the matrix =====
//
// A laboratory has no member table (plan §10.1): the people in it are exactly
// the org users who resolve `quant.read` on the instance AND whom the
// instance's Visibility admits, and their "role" is the set of permissions that
// resolve for them. Every list the UI shows — the headcount on a lab tile, the
// supervisor's people view, the candidate list of a project share — is this
// expansion, never a second state that could disagree with what an
// administrator edits in Addons.
//
// Both halves are load-bearing. `quant.read` and `quant.run` are
// `default = "allow"` (plan §10.2), so the matrix alone admits every active
// user in the organization; what scopes a lab to its group is the Visibility
// tab, and §10.2 says so explicitly: every group the Visibility tab shows the
// tile to may enter and compute, and an instance created for one group gets
// only that group there. Membership is therefore the INTERSECTION, and it is
// applied at every entry point of the app, not only where a list is rendered —
// a tile that is merely hidden is not a gate.
//
// The permission half asks the SAME `PermissionChecker` the app gate uses, so
// admin bypass, per-user entries, group entries, defaults and deny-by-default
// are applied once, in one place. An administrator also bypasses Visibility,
// the way the launcher and the addon list already treat it: an admin who cannot
// enter could not administer the lab they installed.

use std::collections::{HashMap, HashSet};

use tentaflow_protocol::tentaquant::{LabPersonInfo, PERMISSION_IDS};

use crate::addon::permissions::PermissionChecker;
use crate::db::models::UserAccount;
use crate::db::DbPool;

/// The permission that means "is a member of this lab".
pub const PERM_READ: &str = "quant.read";
pub const PERM_RUN: &str = "quant.run";
pub const PERM_INSTRUCT: &str = "quant.instruct";
pub const PERM_ADMIN: &str = "quant.admin";

/// Who one instance's Visibility admits, resolved once. `None` inside means no
/// visibility rule exists for the instance, which the platform reads as
/// "everyone" — the same answer `repository::is_addon_visible_to_user` gives.
///
/// Resolving it once per lab is what keeps listing N labs at N small queries
/// instead of N × (org roster) round trips.
pub struct LabVisibility(Option<HashSet<String>>);

impl LabVisibility {
    /// Reads the instance's visibility rules. A read error fails CLOSED (an
    /// empty set): a lab whose scope cannot be resolved admits nobody rather
    /// than everybody.
    pub fn of(main_db: &DbPool, addon_id: &str) -> Self {
        match crate::db::repository::addon_visible_user_ids(main_db, addon_id) {
            Ok(ids) => LabVisibility(ids),
            Err(e) => {
                tracing::warn!(addon_id, error = %e, "tentaquant: visibility unreadable");
                LabVisibility(Some(HashSet::new()))
            }
        }
    }

    /// Whether the Visibility of this instance admits one user.
    pub fn admits(&self, checker: &PermissionChecker, user_id: &str) -> bool {
        match &self.0 {
            None => true,
            Some(ids) => ids.contains(user_id) || checker.is_admin(user_id),
        }
    }
}

/// Whether one user is in one lab: the instance matrix grants `quant.read` and
/// its Visibility admits them. THE membership predicate — the gate, the lab
/// list, the headcount and every "does this person have lab access" badge go
/// through it, so none of them can disagree.
pub fn is_member(
    main_db: &DbPool,
    checker: &PermissionChecker,
    addon_id: &str,
    user_id: &str,
) -> bool {
    is_member_of(
        &LabVisibility::of(main_db, addon_id),
        checker,
        addon_id,
        user_id,
    )
}

/// [`is_member`] against a visibility already resolved — for callers deciding
/// about several people of the SAME lab (a project's share list), which would
/// otherwise re-read the instance's visibility rules per person.
pub fn is_member_of(
    visibility: &LabVisibility,
    checker: &PermissionChecker,
    addon_id: &str,
    user_id: &str,
) -> bool {
    checker
        .check(addon_id, user_id, PERM_READ, None)
        .is_granted()
        && visibility.admits(checker, user_id)
}

/// Permissions of [`PERMISSION_IDS`] that resolve to granted for one user on
/// one instance, in catalog order. Callers pair it with [`LabVisibility`]:
/// outside the instance's visibility none of these apply.
pub fn granted_permissions(
    checker: &PermissionChecker,
    addon_id: &str,
    user_id: &str,
) -> Vec<String> {
    PERMISSION_IDS
        .iter()
        .filter(|id| checker.check(addon_id, user_id, id, None).is_granted())
        .map(|id| (*id).to_string())
        .collect()
}

/// The org's accounts, loaded once. [`list`] and [`count`] take the result
/// instead of querying, so listing N labs stays ONE user-table scan — the
/// matrix checks themselves are cache reads.
pub fn accounts(main_db: &DbPool) -> Vec<UserAccount> {
    crate::db::repository::list_user_accounts(main_db).unwrap_or_default()
}

/// Display names keyed by user id, so a list view resolves owners from the one
/// roster scan it already has instead of one query per row.
pub fn name_index(accounts: &[UserAccount]) -> HashMap<String, String> {
    accounts
        .iter()
        .map(|u| {
            let name = if u.display_name.is_empty() {
                u.username.clone()
            } else {
                u.display_name.clone()
            };
            (u.id.clone(), name)
        })
        .collect()
}

/// Everyone this lab admits, with the permissions they hold. Inactive accounts
/// are skipped: a disabled user cannot sign in, so listing them as members
/// would overstate the lab's headcount.
pub fn list(
    accounts: &[UserAccount],
    visibility: &LabVisibility,
    checker: &PermissionChecker,
    addon_id: &str,
) -> Vec<LabPersonInfo> {
    accounts
        .iter()
        .filter(|u| u.is_active)
        .filter(|u| visibility.admits(checker, &u.id))
        .filter_map(|u| {
            let permissions = granted_permissions(checker, addon_id, &u.id);
            if !permissions.iter().any(|p| p == PERM_READ) {
                return None;
            }
            Some(LabPersonInfo {
                display_name: if u.display_name.is_empty() {
                    u.username.clone()
                } else {
                    u.display_name.clone()
                },
                user_id: u.id.clone(),
                permissions,
            })
        })
        .collect()
}

/// How many people the lab admits — the number on the lab tile and the
/// dashboard KPI. Counting through [`list`] keeps the two from ever diverging.
pub fn count(
    accounts: &[UserAccount],
    visibility: &LabVisibility,
    checker: &PermissionChecker,
    addon_id: &str,
) -> u32 {
    list(accounts, visibility, checker, addon_id).len() as u32
}

/// Display name of one user id, falling back to the id itself so a row whose
/// account was removed still renders instead of vanishing.
pub fn display_name(main_db: &DbPool, user_id: &str) -> String {
    match crate::db::repository::get_user_account_by_id(main_db, user_id) {
        Ok(Some(u)) if !u.display_name.is_empty() => u.display_name,
        Ok(Some(u)) => u.username,
        _ => user_id.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::repository;
    use crate::dispatch::app_gate::test_support;
    use crate::tentaquant::PACKAGE_ID;

    /// The people of a lab are exactly the matrix expansion: a grant in lab A
    /// does not put anyone into lab B, and a per-user deny removes them from
    /// the list the same way it closes the gate.
    #[test]
    fn people_are_the_matrix_expansion_per_instance() {
        let state = crate::dispatch::state::AppState::for_test();
        let lab_a =
            test_support::install_app_instance(&state, PACKAGE_ID, "tentaquant-aaaaaaaa", &[]);
        let lab_b =
            test_support::install_app_instance(&state, PACKAGE_ID, "tentaquant-bbbbbbbb", &[]);
        let checker = state.permission_checker.as_ref().expect("checker");

        let anna =
            repository::create_user_account(&state.db, "anna", "$h$", "Anna", "").expect("user");
        let group = repository::create_group(&state.db, "lab-a-people", "").expect("group");
        repository::add_user_to_group(&state.db, &group, &anna).expect("membership");
        test_support::set_permission(&state, &lab_a, "group", &group, PERM_READ, "allow");

        let accounts = accounts(&state.db);
        let see_a = LabVisibility::of(&state.db, &lab_a);
        let see_b = LabVisibility::of(&state.db, &lab_b);
        let people = list(&accounts, &see_a, checker, &lab_a);
        assert_eq!(people.len(), 1);
        assert_eq!(people[0].user_id, anna);
        assert_eq!(people[0].permissions, vec![PERM_READ.to_string()]);
        assert!(list(&accounts, &see_b, checker, &lab_b).is_empty());
        assert_eq!(count(&accounts, &see_a, checker, &lab_a), 1);
        assert!(is_member(&state.db, checker, &lab_a, &anna));
        assert!(!is_member(&state.db, checker, &lab_b, &anna));

        // An explicit deny on the person beats the group allow.
        test_support::set_permission(&state, &lab_a, "user", &anna, PERM_READ, "deny");
        assert!(list(&accounts, &see_a, checker, &lab_a).is_empty());
        assert!(!is_member(&state.db, checker, &lab_a, &anna));
    }

    /// The half that makes `default = "allow"` safe (plan §10.2): with the
    /// manifest defaults seeded the matrix admits the whole organization, and
    /// what scopes the lab to its group is the Visibility tab. A user outside
    /// it is not a member — not in the list, not in the headcount, not admitted
    /// by the predicate the gate uses.
    #[test]
    fn visibility_scopes_a_lab_whose_permissions_default_to_allow() {
        let state = crate::dispatch::state::AppState::for_test();
        let lab = test_support::install_app_instance(
            &state,
            PACKAGE_ID,
            "tentaquant-cccccccc",
            &[PERM_READ, PERM_RUN],
        );
        let checker = state.permission_checker.as_ref().expect("checker");

        let anna =
            repository::create_user_account(&state.db, "anna", "$h$", "Anna", "").expect("anna");
        let marek =
            repository::create_user_account(&state.db, "marek", "$h$", "Marek", "").expect("marek");
        let accounts = accounts(&state.db);

        // No visibility rule: the defaults alone put both of them in the lab.
        // Counted against the roster itself rather than a literal, because the
        // organization carries accounts this test did not create.
        let open = LabVisibility::of(&state.db, &lab);
        let everyone = list(&accounts, &open, checker, &lab);
        assert!(everyone.iter().any(|p| p.user_id == anna));
        assert!(everyone.iter().any(|p| p.user_id == marek));
        assert_eq!(
            count(&accounts, &open, checker, &lab),
            everyone.len() as u32
        );

        // Scoped to one group, the lab has that group's people and nobody else.
        let group = repository::create_group(&state.db, "fizyka-3a", "").expect("group");
        repository::add_user_to_group(&state.db, &group, &anna).expect("membership");
        repository::seed_addon_visibility(&state.db, &lab, &group).expect("visibility");

        let scoped = LabVisibility::of(&state.db, &lab);
        let people = list(&accounts, &scoped, checker, &lab);
        assert!(people.iter().any(|p| p.user_id == anna));
        assert!(!people.iter().any(|p| p.user_id == marek));
        assert!(people.len() < everyone.len());
        assert!(is_member(&state.db, checker, &lab, &anna));
        assert!(!is_member(&state.db, checker, &lab, &marek));
    }
}
