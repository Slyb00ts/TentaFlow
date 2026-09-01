// =============================================================================
// File: services/bus_authorizer.rs — production `bus::BusAuthorizer`
// =============================================================================
//
// PLAN.md §8.1: two independent layers.
//
//   1. Global RBAC (`bus.read`/`bus.write`/`bus.admin`, migration v147,
//      `org_viewer`+/`org_operator`+/`org_admin` respectively) — read
//      through the SAME process-global `PermissionMatrix` every other
//      dispatch handler uses, so a role edit is visible here on the exact
//      same cache-invalidation schedule as everywhere else.
//   2. Per-topic ACL — `resource_permissions` with `resource_type = "topic"`
//      (whitelisted in `dispatch/handlers.rs::validate_scope_resource`).
//
// WHAT THIS DOES NOT MODEL (read before extending): PLAN §8.1 decision D6
// describes per-topic ACL as three distinct actions (`produce`/`consume`/
// `admin`). The underlying `resource_permissions` table (shared with
// model/flow/alias ACLs) has no action column — one row is
// `(resource_type, resource_id, subject_type, subject_id, access_level)`
// with `access_level` only ever `"allow"`/`"deny"`, i.e. "can this subject
// touch this resource AT ALL", not "can this subject produce vs. consume vs.
// admin it". Adding an action dimension would mean widening a table shared
// by every OTHER resource-scoped ACL in this codebase — out of scope for a
// single-file authorizer. Consequently: the ACL layer here is a single
// per-topic allow/deny gate applied to EVERY action alike; the
// produce/consume/admin split is enforced ONLY at the RBAC layer above
// (`bus.write`/`bus.read`/`bus.admin`). The M03 mockup's three ACL columns
// will need to collapse to one until `resource_permissions` (or a
// TentaBus-specific successor table) grows an action column.
//
// DLQ rule (PLAN §3.3 + this task's brief): `__dlq.<topic>` is never ACL'd
// on its own — both consuming FROM `__dlq.<topic>` and the broker's own
// internal republish INTO it (`bus::note_delivery_failure`) are gated on
// **Consume** rights on the SOURCE topic `<topic>` (global `bus.read` +
// per-topic ACL on `<topic>`, not `__dlq.<topic>`). `dlq_retry`/
// `dlq_discard` call `authorize(ctx, Admin, dlq_topic)`, which resolves to
// **Admin** on the source topic instead — an operator who can administer
// `<topic>` can administer its DLQ, without a separate ACL row ever having
// to exist for a topic name nobody edits ACLs on directly.
//
// SYSTEM_ACTOR rule (PLAN §8.4/M4 — `__bus.metrics` rollup): unlike DLQ
// auto-send, which piggybacks on a real caller's own existing Consume
// rights on a source topic, the metrics rollup timer has no human/addon
// principal behind it at all — it is `BusService`'s own background thread.
// `SYSTEM_ACTOR` is a bypass reserved for exactly that case: broker-internal
// code publishing/consuming a topic under the `__` reserved prefix. It is
// NOT reachable from any external input — every real dispatch-layer/addon
// call builds `ctx.actor` from a validated `user_id` or `addon_id` (see
// `dispatch/bus.rs`'s `actor: Some(org.user_id.clone())` and
// `host_functions/bus.rs`'s `call_context`), never from free-text a caller
// controls, so nobody can spoof this sentinel from the outside — same trust
// boundary the `__` topic-name prefix itself already relies on
// (`validate_user_topic_name` rejects it outright).
pub const SYSTEM_ACTOR: &str = "__system__";

use std::sync::atomic::{AtomicU64, Ordering};

use crate::bus::dlq::DLQ_TOPIC_PREFIX;
use crate::bus::topics::RESERVED_PREFIX;
use crate::bus::{BusAction, BusCallContext, BusServiceError};
use crate::db::repository;
use crate::db::DbPool;
use crate::services::rbac::PermissionMatrix;

/// Process-wide counter for per-topic ACL changes (`resource_permissions`
/// rows with `resource_type = "topic"`). Bumped by `bump_acl_generation`,
/// which the `AclSetRequest` dispatch handler calls after every
/// `set`/`clear`. Kept separate from `PermissionMatrix`'s own generation
/// (that one tracks role/membership changes, not resource ACL rows) —
/// `RbacBusAuthorizer::generation` sums both so `bus::ConsumerHandle`
/// re-checks on EITHER kind of change, per `BusAuthorizer::generation`'s
/// doc ("bump on any permission/ACL change").
static ACL_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Called by the `BusAclSetRequest` dispatch handler after
/// `resource_permissions::set`/`clear` commits. Never called from this
/// file itself — this module only READS `resource_permissions`, the
/// dispatch handler owns the write + this bump as one sequence.
pub fn bump_acl_generation() {
    ACL_GENERATION.fetch_add(1, Ordering::AcqRel);
}

fn required_permission(action: BusAction) -> &'static str {
    match action {
        BusAction::Produce => "bus.write",
        BusAction::Consume => "bus.read",
        BusAction::Admin => "bus.admin",
    }
}

/// Resolves the (topic, action) pair actually checked against RBAC/ACL —
/// the identity mapping for a normal topic, and the DLQ redirect (this
/// file's module doc) for a `__dlq.<source>` topic.
fn resolve_check(topic: &str, action: BusAction) -> (&str, BusAction) {
    match topic.strip_prefix(DLQ_TOPIC_PREFIX) {
        Some(source) => match action {
            // Consuming the DLQ, or the broker's own auto-send INTO it
            // (`note_delivery_failure` calls `publish` -> `authorize(ctx,
            // Produce, dlq_topic)`), both require Consume on the source.
            BusAction::Consume | BusAction::Produce => (source, BusAction::Consume),
            BusAction::Admin => (source, BusAction::Admin),
        },
        None => (topic, action),
    }
}

/// `true` unless a `"deny"` row exists for this exact `(user, topic)` pair
/// (PLAN §8.1's ACL priority list starts "user_deny > user_allow > ... >
/// default_allow" — this file implements the two ends of that list: an
/// explicit user-level deny, and the default when no row at all exists.
/// Group-scoped rows are not consulted: group membership is a directory
/// concept this file has no lookup for yet, left as a documented gap
/// rather than a silent no-op).
fn topic_acl_allows(db: &DbPool, topic: &str, actor: &str) -> bool {
    let rows = match repository::resource_permissions::list_for_resource(db, "topic", topic) {
        Ok(rows) => rows,
        Err(e) => {
            // Fail CLOSED: an ACL read error must never be silently treated
            // as "no rule, so allow".
            tracing::warn!(topic, error = %e, "bus ACL lookup failed, denying");
            return false;
        }
    };
    !rows
        .iter()
        .any(|r| r.subject_type == "user" && r.subject_id == actor && r.access_level == "deny")
}

/// Production `bus::BusAuthorizer` wired at `bus::init` time from real RBAC.
pub struct RbacBusAuthorizer {
    db: DbPool,
}

impl RbacBusAuthorizer {
    pub fn new(db: DbPool) -> Self {
        Self { db }
    }
}

impl crate::bus::BusAuthorizer for RbacBusAuthorizer {
    fn authorize(
        &self,
        ctx: &BusCallContext,
        action: BusAction,
        topic: &str,
    ) -> Result<(), BusServiceError> {
        // Fail-closed: a caller with no actor (a system/internal context
        // that never goes through the dispatch layer's session resolution)
        // has no permission this authorizer can ever grant.
        let Some(actor) = ctx.actor.as_deref() else {
            return Err(BusServiceError::PermissionDenied {
                action: action.as_str(),
                topic: topic.to_string(),
            });
        };
        if actor == SYSTEM_ACTOR && topic.starts_with(RESERVED_PREFIX) {
            return Ok(());
        }
        let (acl_topic, base_action) = resolve_check(topic, action);
        let perm = required_permission(base_action);
        let has_global = PermissionMatrix::global()
            .has_permission(&self.db, actor, &ctx.org_id, perm)
            .unwrap_or(false);
        if !has_global || !topic_acl_allows(&self.db, acl_topic, actor) {
            return Err(BusServiceError::PermissionDenied {
                action: action.as_str(),
                topic: topic.to_string(),
            });
        }
        Ok(())
    }

    fn authorize_group(
        &self,
        ctx: &BusCallContext,
        action: BusAction,
        topic: &str,
        _group: &str,
    ) -> Result<(), BusServiceError> {
        // PLAN §8.1's RBAC model is topic-scoped, not group-scoped, as of
        // M1 (`BusAuthorizer::authorize_group`'s own doc explicitly allows
        // this thin delegation for such an authorizer).
        self.authorize(ctx, action, topic)
    }

    fn generation(&self) -> u64 {
        PermissionMatrix::global()
            .generation()
            .wrapping_add(ACL_GENERATION.load(Ordering::Acquire))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::BusAuthorizer;
    use tempfile::TempDir;

    fn open_pool() -> (TempDir, DbPool) {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("bus_authorizer_test.db");
        let pool = crate::db::init(&path).expect("init DB");
        (dir, pool)
    }

    fn seed_membership(pool: &DbPool, user_id: &str, role: &str) -> String {
        let org = crate::services::org::repo::create_organization(
            pool,
            "Acme",
            &format!("acme-{user_id}"),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let role_row = crate::services::org::repo::list_roles(pool)
            .unwrap()
            .into_iter()
            .find(|r| r.name == role)
            .unwrap();
        crate::services::org::repo::add_membership(
            pool,
            &org.org_id,
            user_id,
            &role_row.role_id,
            "boot",
        )
        .unwrap();
        org.org_id
    }

    fn ctx(org_id: &str, actor: &str) -> BusCallContext {
        BusCallContext {
            org_id: org_id.to_string(),
            actor: Some(actor.to_string()),
            correlation_id: None,
            origin: "test".to_string(),
        }
    }

    #[test]
    fn viewer_can_consume_but_not_produce() {
        let (_d, pool) = open_pool();
        let org_id = seed_membership(&pool, "u-viewer", "org_viewer");
        let auth = RbacBusAuthorizer::new(pool.clone());
        let c = ctx(&org_id, "u-viewer");
        assert!(auth
            .authorize(&c, BusAction::Consume, "orders.created")
            .is_ok());
        assert!(auth
            .authorize(&c, BusAction::Produce, "orders.created")
            .is_err());
        assert!(auth
            .authorize(&c, BusAction::Admin, "orders.created")
            .is_err());
    }

    #[test]
    fn operator_can_produce_and_consume_but_not_admin() {
        let (_d, pool) = open_pool();
        let org_id = seed_membership(&pool, "u-op", "org_operator");
        let auth = RbacBusAuthorizer::new(pool.clone());
        let c = ctx(&org_id, "u-op");
        assert!(auth
            .authorize(&c, BusAction::Produce, "orders.created")
            .is_ok());
        assert!(auth
            .authorize(&c, BusAction::Consume, "orders.created")
            .is_ok());
        assert!(auth
            .authorize(&c, BusAction::Admin, "orders.created")
            .is_err());
    }

    #[test]
    fn admin_can_do_everything() {
        let (_d, pool) = open_pool();
        let org_id = seed_membership(&pool, "u-admin", "org_admin");
        let auth = RbacBusAuthorizer::new(pool.clone());
        let c = ctx(&org_id, "u-admin");
        assert!(auth
            .authorize(&c, BusAction::Produce, "orders.created")
            .is_ok());
        assert!(auth
            .authorize(&c, BusAction::Consume, "orders.created")
            .is_ok());
        assert!(auth
            .authorize(&c, BusAction::Admin, "orders.created")
            .is_ok());
    }

    #[test]
    fn missing_actor_is_denied() {
        let (_d, pool) = open_pool();
        let auth = RbacBusAuthorizer::new(pool.clone());
        let c = BusCallContext {
            org_id: "org-default".to_string(),
            actor: None,
            correlation_id: None,
            origin: "test".to_string(),
        };
        assert!(auth
            .authorize(&c, BusAction::Consume, "orders.created")
            .is_err());
    }

    #[test]
    fn system_actor_bypasses_authorization_on_reserved_topic() {
        let (_d, pool) = open_pool();
        let auth = RbacBusAuthorizer::new(pool.clone());
        // No membership seeded at all — the bypass must not depend on RBAC.
        let c = ctx("org-default", SYSTEM_ACTOR);
        assert!(auth
            .authorize(&c, BusAction::Produce, "__bus.metrics")
            .is_ok());
        assert!(auth
            .authorize(&c, BusAction::Consume, "__bus.metrics")
            .is_ok());
    }

    #[test]
    fn system_actor_does_not_bypass_authorization_on_normal_topic() {
        let (_d, pool) = open_pool();
        let auth = RbacBusAuthorizer::new(pool.clone());
        // The bypass is scoped to `__`-reserved topics only — SYSTEM_ACTOR
        // has no RBAC membership seeded, so a non-reserved topic must still
        // be denied.
        let c = ctx("org-default", SYSTEM_ACTOR);
        assert!(auth
            .authorize(&c, BusAction::Produce, "orders.created")
            .is_err());
    }

    #[test]
    fn per_topic_deny_row_overrides_org_admin() {
        let (_d, pool) = open_pool();
        let org_id = seed_membership(&pool, "u-admin2", "org_admin");
        repository::resource_permissions::set(
            &pool,
            "topic",
            "orders.created",
            "user",
            "u-admin2",
            "deny",
        )
        .unwrap();
        let auth = RbacBusAuthorizer::new(pool.clone());
        let c = ctx(&org_id, "u-admin2");
        assert!(auth
            .authorize(&c, BusAction::Consume, "orders.created")
            .is_err());
        // Unaffected topic without a deny row still works.
        assert!(auth
            .authorize(&c, BusAction::Consume, "other.topic")
            .is_ok());
    }

    #[test]
    fn dlq_consume_requires_consume_on_source_topic() {
        let (_d, pool) = open_pool();
        let org_id = seed_membership(&pool, "u-viewer2", "org_viewer");
        let auth = RbacBusAuthorizer::new(pool.clone());
        let c = ctx(&org_id, "u-viewer2");
        // org_viewer has bus.read (Consume) -> consuming the DLQ is allowed.
        assert!(auth
            .authorize(&c, BusAction::Consume, "__dlq.orders.created")
            .is_ok());
        // The broker's own internal auto-send (Produce action on the dlq
        // topic) is ALSO gated on Consume-on-source, per this file's DLQ
        // rule, so a viewer (bus.read only) is allowed to trigger it too.
        assert!(auth
            .authorize(&c, BusAction::Produce, "__dlq.orders.created")
            .is_ok());
    }

    #[test]
    fn dlq_admin_requires_admin_on_source_topic() {
        let (_d, pool) = open_pool();
        let org_id = seed_membership(&pool, "u-op2", "org_operator");
        let auth = RbacBusAuthorizer::new(pool.clone());
        let c = ctx(&org_id, "u-op2");
        // org_operator has no bus.admin -> dlq_retry/dlq_discard (Admin
        // action) on the DLQ topic must fail.
        assert!(auth
            .authorize(&c, BusAction::Admin, "__dlq.orders.created")
            .is_err());
    }

    #[test]
    fn generation_bumps_on_role_change_and_acl_change() {
        let (_d, pool) = open_pool();
        let org_id = seed_membership(&pool, "u-gen", "org_admin");
        let auth = RbacBusAuthorizer::new(pool.clone());
        let g0 = auth.generation();
        PermissionMatrix::global().invalidate("u-gen", &org_id);
        let g1 = auth.generation();
        assert_ne!(g0, g1, "role/membership invalidation must bump generation");
        bump_acl_generation();
        let g2 = auth.generation();
        assert_ne!(g1, g2, "ACL set/clear must bump generation");
    }
}
