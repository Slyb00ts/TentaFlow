// =============================================================================
// File: services/bus_authorizer.rs — production `bus::BusAuthorizer`
// =============================================================================
//
// plan-app-platform §4.5 (W4): two independent layers, both scoped to ONE
// TentaBus instance.
//
//   1. The addon permission matrix (`addon::permissions::PermissionChecker`)
//      — `bus.read`/`bus.write`/`bus.admin`, granted per (instance, user)
//      exactly like every other native app's permissions. This REPLACES the
//      org-RBAC layer (`PermissionMatrix`/`bus.read`+/`bus.write`+/`bus.admin`
//      global roles) `RbacBusAuthorizer` used through W3: TentaBus is a
//      native app on the platform now, so its own authority comes from the
//      SAME matrix `dispatch::app_gate::require_instance_permission` reads,
//      not a second, bus-specific RBAC grant.
//   2. Per-topic ACL — `resource_permissions` with `resource_type = "topic"`
//      (whitelisted in `dispatch/handlers.rs::validate_scope_resource`),
//      unchanged from W3 except its `resource_id`'s composite encoding (see
//      `topic_acl_resource_id`'s own doc).
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
// produce/consume/admin split is enforced ONLY at the matrix layer above
// (`bus.write`/`bus.read`/`bus.admin`). The M03 mockup's three ACL columns
// will need to collapse to one until `resource_permissions` (or a
// TentaBus-specific successor table) grows an action column.
//
// DLQ rule (PLAN §3.3 + this task's brief): `__dlq.<topic>` is never ACL'd
// on its own — both consuming FROM `__dlq.<topic>` and the broker's own
// internal republish INTO it (`bus::note_delivery_failure`) are gated on
// **Consume** rights on the SOURCE topic `<topic>` (matrix `bus.read` +
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
use std::sync::Arc;

use crate::addon::permissions::PermissionChecker;
use crate::bus::dlq::DLQ_TOPIC_PREFIX;
use crate::bus::instance::BusInstanceId;
use crate::bus::topics::RESERVED_PREFIX;
use crate::bus::{BusAction, BusCallContext, BusServiceError};
use crate::db::repository;
use crate::db::DbPool;

/// Process-wide counter for per-topic ACL changes (`resource_permissions`
/// rows with `resource_type = "topic"`). Bumped by `bump_acl_generation`,
/// which the `AclSetRequest` dispatch handler calls after every
/// `set`/`clear`. Kept separate from `PermissionChecker`'s own generation
/// (that one tracks matrix grant changes, not resource ACL rows) —
/// `InstanceBusAuthorizer::generation` sums both so `bus::ConsumerHandle`
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

/// Resolves the (topic, action) pair actually checked against the matrix/ACL
/// — the identity mapping for a normal topic, and the DLQ redirect (this
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

fn denied(action: BusAction, topic: &str) -> BusServiceError {
    BusServiceError::PermissionDenied {
        action: action.as_str(),
        topic: topic.to_string(),
    }
}

/// plan-app-platform §7 W4: a per-topic ACL row's `resource_id` is this
/// file's OWN composite key over `(instance_id, org_id, topic)`, built via
/// `sync::resource_id::composite_resource_id`'s length-prefixed encoding
/// (rather than the W3 hand-rolled `"{instance_id}/{org_id}/{topic}"`, which
/// could not tell a topic containing a literal `/` apart from a delimiter —
/// the same injectivity concern every OTHER composite resource id in this
/// codebase already routes through that function for). Used identically by
/// `dispatch/bus.rs`'s `acl_set_v1`/`acl_list_v1` (the only writer/other
/// reader of a `resource_type = "topic"` row) — a change here without
/// updating that file would silently orphan every existing ACL row.
pub fn topic_acl_resource_id(instance_id: &str, org_id: &str, topic: &str) -> String {
    crate::sync::resource_id::composite_resource_id(&[instance_id, org_id, topic])
}

/// `true` unless a `"deny"` row exists for this exact `(user, topic)` pair
/// (PLAN §8.1's ACL priority list starts "user_deny > user_allow > ... >
/// default_allow" — this file implements the two ends of that list: an
/// explicit user-level deny, and the default when no row at all exists.
/// Group-scoped rows are not consulted: group membership is a directory
/// concept this file has no lookup for yet, left as a documented gap
/// rather than a silent no-op).
fn topic_acl_allows(
    db: &DbPool,
    instance_id: &str,
    org_id: &str,
    topic: &str,
    actor: &str,
) -> bool {
    let resource_id = topic_acl_resource_id(instance_id, org_id, topic);
    let rows = match repository::resource_permissions::list_for_resource(db, "topic", &resource_id)
    {
        Ok(rows) => rows,
        Err(e) => {
            // Fail CLOSED: an ACL read error must never be silently
            // treated as "no rule, so allow".
            tracing::warn!(
                resource_id, error = %e,
                "bus ACL lookup failed, denying"
            );
            return false;
        }
    };
    !rows
        .iter()
        .any(|r| r.subject_type == "user" && r.subject_id == actor && r.access_level == "deny")
}

/// Production `bus::BusAuthorizer` wired at `bus::init_instance` time —
/// plan-app-platform §4.5. Named for what it now is: ONE TentaBus instance's
/// authorizer, backed by the addon permission matrix rather than org-RBAC
/// (the retired `RbacBusAuthorizer`, W1-W3).
pub struct InstanceBusAuthorizer {
    db: DbPool,
    instance: BusInstanceId,
    checker: Arc<PermissionChecker>,
}

impl InstanceBusAuthorizer {
    pub fn new(db: DbPool, instance: BusInstanceId, checker: Arc<PermissionChecker>) -> Self {
        Self {
            db,
            instance,
            checker,
        }
    }
}

impl crate::bus::BusAuthorizer for InstanceBusAuthorizer {
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
            return Err(denied(action, topic));
        };
        if actor == SYSTEM_ACTOR && topic.starts_with(RESERVED_PREFIX) {
            return Ok(());
        }
        let (acl_topic, base_action) = resolve_check(topic, action);
        let perm = required_permission(base_action);
        if !self
            .checker
            .check(self.instance.as_str(), actor, perm, None)
            .is_granted()
        {
            return Err(denied(action, topic));
        }
        if !topic_acl_allows(
            &self.db,
            self.instance.as_str(),
            &ctx.org_id,
            acl_topic,
            actor,
        ) {
            return Err(denied(action, topic));
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
        // The matrix/ACL model is topic-scoped, not group-scoped
        // (`BusAuthorizer::authorize_group`'s own doc explicitly allows this
        // thin delegation for such an authorizer).
        self.authorize(ctx, action, topic)
    }

    fn generation(&self) -> u64 {
        self.checker
            .generation()
            .wrapping_add(ACL_GENERATION.load(Ordering::Acquire))
    }

    /// plan-app-platform §7 W4 finding 4: lets `BusService::new` refuse to
    /// start an engine whose authorizer was wired for a DIFFERENT instance —
    /// see `BusAuthorizer::instance_id`'s doc.
    fn instance_id(&self) -> Option<&str> {
        Some(self.instance.as_str())
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

    fn instance_a() -> BusInstanceId {
        BusInstanceId::parse("tentabus-aaaaaaaa").unwrap()
    }

    fn instance_b() -> BusInstanceId {
        BusInstanceId::parse("tentabus-bbbbbbbb").unwrap()
    }

    fn checker(db: &DbPool) -> Arc<PermissionChecker> {
        Arc::new(PermissionChecker::new(db.clone()))
    }

    /// Grants `perm` to `user_id` on `instance` (matrix row) and refreshes
    /// the checker so `check` observes it immediately — same shape
    /// `dispatch::app_gate::test_support::grant` uses for other native apps.
    fn grant(
        db: &DbPool,
        checker: &PermissionChecker,
        instance: &BusInstanceId,
        user_id: &str,
        perm: &str,
    ) {
        repository::upsert_permission(db, instance.as_str(), "user", user_id, perm, "allow", None)
            .unwrap();
        checker.refresh_addon(instance.as_str());
    }

    fn ctx(org_id: &str, actor: &str) -> BusCallContext {
        BusCallContext {
            instance_id: instance_a(),
            org_id: org_id.to_string(),
            actor: Some(actor.to_string()),
            correlation_id: None,
            origin: "test".to_string(),
        }
    }

    #[test]
    fn viewer_can_consume_but_not_produce() {
        let (_d, pool) = open_pool();
        let checker = checker(&pool);
        grant(&pool, &checker, &instance_a(), "u-viewer", "bus.read");
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker);
        let c = ctx("org-1", "u-viewer");
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
        let checker = checker(&pool);
        grant(&pool, &checker, &instance_a(), "u-op", "bus.read");
        grant(&pool, &checker, &instance_a(), "u-op", "bus.write");
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker);
        let c = ctx("org-1", "u-op");
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
        let checker = checker(&pool);
        grant(&pool, &checker, &instance_a(), "u-admin", "bus.read");
        grant(&pool, &checker, &instance_a(), "u-admin", "bus.write");
        grant(&pool, &checker, &instance_a(), "u-admin", "bus.admin");
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker);
        let c = ctx("org-1", "u-admin");
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
        let checker = checker(&pool);
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker);
        let c = BusCallContext {
            instance_id: instance_a(),
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
        let checker = checker(&pool);
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker);
        // No matrix grant seeded at all — the bypass must not depend on it.
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
        let checker = checker(&pool);
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker);
        // The bypass is scoped to `__`-reserved topics only — SYSTEM_ACTOR
        // has no matrix grant seeded, so a non-reserved topic must still be
        // denied.
        let c = ctx("org-default", SYSTEM_ACTOR);
        assert!(auth
            .authorize(&c, BusAction::Produce, "orders.created")
            .is_err());
    }

    #[test]
    fn per_topic_deny_row_overrides_matrix_admin() {
        let (_d, pool) = open_pool();
        let checker = checker(&pool);
        grant(&pool, &checker, &instance_a(), "u-admin2", "bus.read");
        grant(&pool, &checker, &instance_a(), "u-admin2", "bus.write");
        grant(&pool, &checker, &instance_a(), "u-admin2", "bus.admin");
        repository::resource_permissions::set(
            &pool,
            "topic",
            &topic_acl_resource_id(instance_a().as_str(), "org-1", "orders.created"),
            "user",
            "u-admin2",
            "deny",
        )
        .unwrap();
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker);
        let c = ctx("org-1", "u-admin2");
        assert!(auth
            .authorize(&c, BusAction::Consume, "orders.created")
            .is_err());
        // Unaffected topic without a deny row still works.
        assert!(auth
            .authorize(&c, BusAction::Consume, "other.topic")
            .is_ok());
    }

    /// plan-app-platform §7 W4: a per-topic ACL row is scoped by
    /// `topic_acl_resource_id`'s `(instance_id, org_id, topic)` composite
    /// key. A deny row seeded under instance A must have zero effect on the
    /// SAME org/topic checked against instance B — the whole point of
    /// folding the instance id into the resource id rather than leaving it
    /// bare.
    #[test]
    fn topic_acl_on_one_instance_does_not_apply_on_another() {
        let (_d, pool) = open_pool();
        let checker = checker(&pool);
        grant(&pool, &checker, &instance_a(), "u-multi", "bus.read");
        grant(&pool, &checker, &instance_b(), "u-multi", "bus.read");
        repository::resource_permissions::set(
            &pool,
            "topic",
            &topic_acl_resource_id(instance_a().as_str(), "org-1", "orders.created"),
            "user",
            "u-multi",
            "deny",
        )
        .unwrap();

        let auth_a = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker.clone());
        let auth_b = InstanceBusAuthorizer::new(pool.clone(), instance_b(), checker);
        let c = ctx("org-1", "u-multi");

        assert!(
            auth_a
                .authorize(&c, BusAction::Consume, "orders.created")
                .is_err(),
            "instance A's own deny row must still apply on instance A"
        );
        assert!(
            auth_b
                .authorize(&c, BusAction::Consume, "orders.created")
                .is_ok(),
            "instance A's deny row must not leak into instance B's ACL check"
        );
    }

    #[test]
    fn dlq_consume_requires_consume_on_source_topic() {
        let (_d, pool) = open_pool();
        let checker = checker(&pool);
        grant(&pool, &checker, &instance_a(), "u-viewer2", "bus.read");
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker);
        let c = ctx("org-1", "u-viewer2");
        // bus.read (Consume) only -> consuming the DLQ is allowed.
        assert!(auth
            .authorize(&c, BusAction::Consume, "__dlq.orders.created")
            .is_ok());
        // The broker's own internal auto-send (Produce action on the dlq
        // topic) is ALSO gated on Consume-on-source, per this file's DLQ
        // rule, so a reader (bus.read only) is allowed to trigger it too.
        assert!(auth
            .authorize(&c, BusAction::Produce, "__dlq.orders.created")
            .is_ok());
    }

    #[test]
    fn dlq_admin_requires_admin_on_source_topic() {
        let (_d, pool) = open_pool();
        let checker = checker(&pool);
        grant(&pool, &checker, &instance_a(), "u-op2", "bus.read");
        grant(&pool, &checker, &instance_a(), "u-op2", "bus.write");
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker);
        let c = ctx("org-1", "u-op2");
        // No bus.admin grant -> dlq_retry/dlq_discard (Admin action) on the
        // DLQ topic must fail.
        assert!(auth
            .authorize(&c, BusAction::Admin, "__dlq.orders.created")
            .is_err());
    }

    #[test]
    fn generation_bumps_on_matrix_change_and_acl_change() {
        let (_d, pool) = open_pool();
        let checker = checker(&pool);
        grant(&pool, &checker, &instance_a(), "u-gen", "bus.admin");
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker.clone());
        let g0 = auth.generation();
        checker.refresh_addon(instance_a().as_str());
        let g1 = auth.generation();
        assert_ne!(g0, g1, "a matrix refresh must bump generation");
        bump_acl_generation();
        let g2 = auth.generation();
        assert_ne!(g1, g2, "ACL set/clear must bump generation");
    }
}
