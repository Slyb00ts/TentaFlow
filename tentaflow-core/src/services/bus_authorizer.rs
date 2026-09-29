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
// PER-TOPIC ACTIONS AND GROUPS (SUM/tentabus/DECYZJE-2026-09-22.md,
// `topic-acl-actions`): PLAN §8.1 decision D6 describes per-topic ACL as
// three distinct actions (`produce`/`consume`/`admin`). Migration 168 widened
// `resource_permissions` (shared with model/flow/alias/model_bundle/api_key
// ACLs) with an `action` column ("read"/"write"/"admin"/"*") and its UNIQUE
// key to include it, so a topic can now carry independent read/write/admin
// rows per subject instead of one shared allow/deny gate. Every row recorded
// before that migration was pinned to `action = '*'`, which the lookup
// (`db::repository::resource_permissions::check_action`) treats as matching
// every action asked about — an old deny still denies produce/consume/admin
// alike, so existing grants keep their exact prior meaning. `subject_type =
// 'group'` rows are now consulted too, via `check_action`'s group_members
// join — previously a documented gap, group grants were silently ignored.
// Priority (same as every other resource type's `check_inner`, but with no
// admin-role bypass — the matrix layer above already gates admin access):
// user_deny > user_allow > group_deny > group_allow > default_allow, each
// level summing rows for the exact action plus `'*'` rows.
//
// ADDON SUBJECTS (migration 177, owner decision P6): the caller is looked up
// with the kind its entry point authenticated (`BusCallContext::actor_kind`,
// never the free-text `origin`). An addon is allowed only by its own
// `'addon'` rows and has no group step; a user never inherits an addon's
// row. A `'user'` DENY row carrying an addon's id still denies that addon —
// the pre-177 way of restricting one, kept fail-closed on every node (see
// `resource_permissions::check_action`).
//
// API KEY SUBJECTS (package K, owner decision P5, SUM/tentabus/
// DECYZJE-2026-09-22.md: "one general key for everything"): a general API key
// calling the records REST (`api/bus_rest.rs`) acts as itself —
// `BusCallContext::actor_kind = ApiKey`, `actor` = the key's uid. A key has no
// identity in the addon permission matrix (that matrix grants per user), so
// the matrix layer is NOT consulted for it and no matrix row is required;
// instead the per-topic ACL is its only source of rights, default DENY:
//   * only `Produce` (ACL action `write`) and `Consume` (`read`) exist for a
//     key — `Admin` (topic create/delete/config, ACL and data-hiding edits,
//     auto-creation, DLQ retry/discard, group pause/reset) is refused
//     whatever rows exist;
//   * reserved `__`-prefixed topics (the DLQ, `__bus.metrics`) are refused —
//     they are broker infrastructure, never an external system's contract;
//   * the key must still exist, be active and be a GENERAL key: a user-bound
//     key reaches the REST as its user, and a revoked key's leftover rows
//     must not keep working through any other path (a flow started by the
//     key, a replicated row);
//   * an allow counts only for the exact action (`resource_permissions::
//     check_action`); a `'*'` row still denies;
//   * a key consumes only under its own consumer groups — `k:<key uid>` or
//     `k:<key uid>.<name>` (`api_key_owns_group`) — so it can never move the
//     committed offsets of a group some other application consumes with; and
//     no other caller (a user, an addon, another key) may consume under a
//     `k:` group, so nobody moves a key's offsets either. The `:` is outside
//     the ordinary group charset (`bus::validate_group_name`), so the two
//     namespaces cannot meet.
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
use crate::auth::actor::ActorKind;
use crate::bus::dlq::DLQ_TOPIC_PREFIX;
use crate::bus::instance::BusInstanceId;
use crate::bus::topics::RESERVED_PREFIX;
use crate::bus::{BusAction, BusCallContext, BusServiceError, API_KEY_GROUP_PREFIX};
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

/// Called after every committed change to a topic's access entries — a local
/// write (`resource_permissions::set_topic_rule`, the clears in
/// `dispatch/bus.rs` and `dispatch/handlers.rs`), a topic create/delete that
/// removed entries, and a replicated change applied by
/// `sync::core_materializer`. One counter for every instance and topic: an
/// open consumer re-checks its own topics when it moves. Never called from
/// this file itself — this module only READS `resource_permissions`.
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

/// The `resource_permissions.action` value a `BusAction` maps to (migration
/// 168) — the same produce→write/consume→read/admin→admin mapping as
/// `required_permission`'s matrix permission, minus the `"bus."` prefix, so a
/// per-topic ACL row and its matrix counterpart read the same way to an
/// operator setting either one.
fn acl_action(action: BusAction) -> &'static str {
    match action {
        BusAction::Produce => "write",
        BusAction::Consume => "read",
        BusAction::Admin => "admin",
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

/// Full priority-chain ACL check for `(topic, action, actor)` — PLAN §8.1's
/// list, `user_deny > user_allow > group_deny > group_allow > default_allow`,
/// via `resource_permissions::check_action` (migration 168). Each level sums
/// rows matching `action` exactly plus `'*'` rows (every row recorded before
/// the migration, still meaning "every action"), and group rows are resolved
/// through `actor`'s `group_members` rows — the gap this file's doc used to
/// name is closed.
///
/// `actor_kind` (migration 177, owner decision P6): an addon is its own kind
/// of subject, allowed only by `subject_type = 'addon'` rows, so a user whose
/// id happens to equal an addon's never inherits the addon's rows. The one
/// crossing is fail-closed: a `'user'` deny row with the addon's id still
/// denies the addon (`resource_permissions::check_action`).
fn topic_acl_allows(
    db: &DbPool,
    instance_id: &str,
    org_id: &str,
    topic: &str,
    actor: &str,
    actor_kind: ActorKind,
    action: &str,
) -> bool {
    let resource_id = topic_acl_resource_id(instance_id, org_id, topic);
    match repository::resource_permissions::check_action(
        db,
        "topic",
        &resource_id,
        action,
        actor_kind,
        actor,
        true, // default_allow: unchanged from the pre-168 "allow unless denied" shape.
    ) {
        Ok(allowed) => allowed,
        Err(e) => {
            // Fail CLOSED: an ACL read error must never be silently
            // treated as "no rule, so allow".
            tracing::warn!(
                resource_id, action, error = %e,
                "bus ACL lookup failed, denying"
            );
            false
        }
    }
}

/// Whether `group` is one of the consumer groups the API key `key_uid` may
/// consume under: `k:<uid>`, or `k:<uid>.` followed by a name
/// (`bus::API_KEY_GROUP_PREFIX`).
pub fn api_key_owns_group(key_uid: &str, group: &str) -> bool {
    match group
        .strip_prefix(API_KEY_GROUP_PREFIX)
        .and_then(|rest| rest.strip_prefix(key_uid))
    {
        Some(rest) => rest.is_empty() || (rest.len() > 1 && rest.starts_with('.')),
        None => false,
    }
}

/// Whether the key holds `action` on `topic` of `org_id` in any instance —
/// asked by the records REST's instance-less legacy path before it resolves
/// an instance at all (`api/bus_rest.rs::precheck_api_key`). Each candidate
/// row is re-checked through `api_key_topic_allows`, so a deny, a revoked key
/// or a `'*'` row answers exactly as on the call itself.
pub fn api_key_topic_granted_anywhere(
    db: &DbPool,
    org_id: &str,
    topic: &str,
    key_uid: &str,
    action: BusAction,
) -> bool {
    let rows = match repository::resource_permissions::list_for_subject(db, "api_key", key_uid) {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "bus ACL: listing an API key's grants failed, denying");
            return false;
        }
    };
    rows.iter()
        .filter(|row| row.resource_type == "topic" && row.access_level == "allow")
        .filter_map(|row| {
            let segments = crate::sync::resource_id::decode_segments(&row.resource_id)?;
            match segments.as_slice() {
                [instance, row_org, row_topic] if *row_org == org_id && *row_topic == topic => {
                    Some(instance.to_string())
                }
                _ => None,
            }
        })
        .any(|instance| api_key_topic_allows(db, &instance, org_id, topic, key_uid, action))
}

/// The whole per-topic decision for a general API key (this file's `API KEY
/// SUBJECTS` doc): `Produce`/`Consume` only, never a reserved topic, only
/// while the key is an active general key, and only through an explicit
/// allow row for exactly that action. `topic` is the topic as addressed, a
/// `__dlq.<topic>` included — which is refused like every reserved topic.
/// Shared with the records REST, which asks it before resolving the
/// instance so a key without a grant learns nothing about which exist.
pub fn api_key_topic_allows(
    db: &DbPool,
    instance_id: &str,
    org_id: &str,
    topic: &str,
    key_uid: &str,
    action: BusAction,
) -> bool {
    if action == BusAction::Admin || topic.starts_with(RESERVED_PREFIX) {
        return false;
    }
    match repository::get_api_key_by_uid(db, key_uid) {
        Ok(Some(key)) if key.is_active && key.key_type == "general" => {}
        Ok(_) => return false,
        Err(e) => {
            tracing::warn!(error = %e, "bus ACL: API key lookup failed, denying");
            return false;
        }
    }
    let resource_id = topic_acl_resource_id(instance_id, org_id, topic);
    match repository::resource_permissions::check_action(
        db,
        "topic",
        &resource_id,
        acl_action(action),
        ActorKind::ApiKey,
        key_uid,
        false, // default DENY: a key holds only what was granted to it.
    ) {
        Ok(allowed) => allowed,
        Err(e) => {
            tracing::warn!(
                resource_id, error = %e,
                "bus ACL lookup for an API key failed, denying"
            );
            false
        }
    }
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
        // Before the SYSTEM_ACTOR bypass: that sentinel is a system caller's,
        // and an API key's uid can never make a key one.
        if ctx.actor_kind == ActorKind::ApiKey {
            return if api_key_topic_allows(
                &self.db,
                self.instance.as_str(),
                &ctx.org_id,
                topic,
                actor,
                action,
            ) {
                Ok(())
            } else {
                Err(denied(action, topic))
            };
        }
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
            ctx.actor_kind,
            acl_action(base_action),
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
        group: &str,
    ) -> Result<(), BusServiceError> {
        // The matrix/ACL model is topic-scoped, not group-scoped
        // (`BusAuthorizer::authorize_group`'s own doc explicitly allows this
        // thin delegation for such an authorizer) — except for the API key
        // namespace: a key consumes only under its own `k:` groups, and a
        // `k:` group belongs to its key alone.
        if ctx.actor_kind == ActorKind::ApiKey || group.starts_with(API_KEY_GROUP_PREFIX) {
            let owner = ctx.actor_kind == ActorKind::ApiKey
                && ctx
                    .actor
                    .as_deref()
                    .is_some_and(|key_uid| api_key_owns_group(key_uid, group));
            if !owner {
                return Err(denied(action, topic));
            }
        }
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
            actor_kind: crate::auth::actor::ActorKind::User,
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
            actor_kind: crate::auth::actor::ActorKind::User,
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

    /// migration 168, `topic-acl-actions`: a `subject_type = 'group'` deny
    /// row must be honored even though the actor never has a `subject_type
    /// = 'user'` row of their own — the documented gap this rung closes.
    #[test]
    fn group_deny_row_denies_a_member_with_full_matrix_grants() {
        let (_d, pool) = open_pool();
        let checker = checker(&pool);
        let user_id =
            repository::create_user_account(&pool, "grp-user", "hash", "Grp User", "g@x").unwrap();
        grant(&pool, &checker, &instance_a(), &user_id, "bus.read");
        grant(&pool, &checker, &instance_a(), &user_id, "bus.write");
        grant(&pool, &checker, &instance_a(), &user_id, "bus.admin");
        let group_id = repository::create_group(&pool, "readers", "").unwrap();
        repository::add_user_to_group(&pool, &group_id, &user_id).unwrap();
        repository::resource_permissions::set(
            &pool,
            "topic",
            &topic_acl_resource_id(instance_a().as_str(), "org-1", "orders.created"),
            "group",
            &group_id,
            "deny",
        )
        .unwrap();
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker);
        let c = ctx("org-1", &user_id);
        assert!(auth
            .authorize(&c, BusAction::Consume, "orders.created")
            .is_err());
        // A different topic with no ACL row at all still defaults to allow.
        assert!(auth
            .authorize(&c, BusAction::Consume, "other.topic")
            .is_ok());
    }

    /// A user-level allow overrides a group-level deny for the SAME actor —
    /// same priority order (`user_deny > user_allow > group_deny >
    /// group_allow > default_allow`) every other resource type's ACL check
    /// already applies.
    #[test]
    fn user_level_allow_overrides_group_level_deny() {
        let (_d, pool) = open_pool();
        let checker = checker(&pool);
        let user_id =
            repository::create_user_account(&pool, "grp-user2", "hash", "U", "u@x").unwrap();
        grant(&pool, &checker, &instance_a(), &user_id, "bus.read");
        let group_id = repository::create_group(&pool, "denied", "").unwrap();
        repository::add_user_to_group(&pool, &group_id, &user_id).unwrap();
        let resource_id = topic_acl_resource_id(instance_a().as_str(), "org-1", "orders.created");
        repository::resource_permissions::set(
            &pool,
            "topic",
            &resource_id,
            "group",
            &group_id,
            "deny",
        )
        .unwrap();
        repository::resource_permissions::set(
            &pool,
            "topic",
            &resource_id,
            "user",
            &user_id,
            "allow",
        )
        .unwrap();
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker);
        let c = ctx("org-1", &user_id);
        assert!(auth
            .authorize(&c, BusAction::Consume, "orders.created")
            .is_ok());
    }

    /// migration 168: an action-scoped deny row blocks ONLY that action —
    /// a `write` deny must not touch `read`/`admin` on the same topic.
    #[test]
    fn action_specific_deny_row_only_blocks_that_action() {
        let (_d, pool) = open_pool();
        let checker = checker(&pool);
        grant(&pool, &checker, &instance_a(), "u-scoped", "bus.read");
        grant(&pool, &checker, &instance_a(), "u-scoped", "bus.write");
        let resource_id = topic_acl_resource_id(instance_a().as_str(), "org-1", "orders.created");
        repository::resource_permissions::set_with_action(
            &pool,
            "topic",
            &resource_id,
            "user",
            "u-scoped",
            "write",
            "deny",
        )
        .unwrap();
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker);
        let c = ctx("org-1", "u-scoped");
        assert!(
            auth.authorize(&c, BusAction::Produce, "orders.created")
                .is_err(),
            "write-scoped deny must block Produce"
        );
        assert!(
            auth.authorize(&c, BusAction::Consume, "orders.created")
                .is_ok(),
            "write-scoped deny must not block Consume"
        );
    }

    /// A `'*'` row (the shape every pre-168 row has, per the migration's
    /// mapping) still denies every action alike.
    #[test]
    fn wildcard_action_row_denies_every_action() {
        let (_d, pool) = open_pool();
        let checker = checker(&pool);
        grant(&pool, &checker, &instance_a(), "u-wild", "bus.read");
        grant(&pool, &checker, &instance_a(), "u-wild", "bus.write");
        grant(&pool, &checker, &instance_a(), "u-wild", "bus.admin");
        let resource_id = topic_acl_resource_id(instance_a().as_str(), "org-1", "orders.created");
        repository::resource_permissions::set_with_action(
            &pool,
            "topic",
            &resource_id,
            "user",
            "u-wild",
            "*",
            "deny",
        )
        .unwrap();
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker);
        let c = ctx("org-1", "u-wild");
        assert!(auth
            .authorize(&c, BusAction::Produce, "orders.created")
            .is_err());
        assert!(auth
            .authorize(&c, BusAction::Consume, "orders.created")
            .is_err());
        assert!(auth
            .authorize(&c, BusAction::Admin, "orders.created")
            .is_err());
    }

    fn addon_ctx(org_id: &str, addon_id: &str) -> BusCallContext {
        BusCallContext {
            actor_kind: ActorKind::Addon,
            ..ctx(org_id, addon_id)
        }
    }

    fn topic_rule(pool: &DbPool, subject_type: &str, subject_id: &str, level: &str) {
        let resource_id = topic_acl_resource_id(instance_a().as_str(), "org-1", "orders.created");
        repository::resource_permissions::set_with_action(
            pool,
            "topic",
            &resource_id,
            subject_type,
            subject_id,
            "read",
            level,
        )
        .unwrap();
    }

    /// Owner decision P6 (migration 177): an addon is its own kind of subject.
    /// Its `'addon'` deny stops the addon, and a user holding the SAME id —
    /// with the same matrix grants — is not touched by it.
    #[test]
    fn addon_deny_row_stops_the_addon_but_not_a_user_with_the_same_id() {
        let (_d, pool) = open_pool();
        let checker = checker(&pool);
        grant(&pool, &checker, &instance_a(), "asystent", "bus.read");
        topic_rule(&pool, "addon", "asystent", "deny");
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker);
        assert!(auth
            .authorize(
                &addon_ctx("org-1", "asystent"),
                BusAction::Consume,
                "orders.created"
            )
            .is_err());
        assert!(auth
            .authorize(
                &ctx("org-1", "asystent"),
                BusAction::Consume,
                "orders.created"
            )
            .is_ok());
    }

    /// A `'user'` DENY row carrying an addon's id — the only way to restrict
    /// an addon before migration 177, still replayed from the sync ledger and
    /// kept by a node where the addon was installed later — denies the addon
    /// too (fail closed), as it does the user of that id. A `'user'` ALLOW row
    /// never admits the addon past its own `'addon'` deny.
    #[test]
    fn a_user_deny_row_with_an_addon_id_still_denies_the_addon() {
        let (_d, pool) = open_pool();
        let chk = checker(&pool);
        grant(&pool, &chk, &instance_a(), "asystent", "bus.read");
        topic_rule(&pool, "user", "asystent", "deny");
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), chk);
        assert!(auth
            .authorize(
                &addon_ctx("org-1", "asystent"),
                BusAction::Consume,
                "orders.created"
            )
            .is_err());
        assert!(auth
            .authorize(
                &ctx("org-1", "asystent"),
                BusAction::Consume,
                "orders.created"
            )
            .is_err());

        let (_d2, pool2) = open_pool();
        let checker2 = checker(&pool2);
        grant(&pool2, &checker2, &instance_a(), "asystent", "bus.read");
        topic_rule(&pool2, "user", "asystent", "allow");
        topic_rule(&pool2, "addon", "asystent", "deny");
        let auth2 = InstanceBusAuthorizer::new(pool2.clone(), instance_a(), checker2);
        assert!(auth2
            .authorize(
                &addon_ctx("org-1", "asystent"),
                BusAction::Consume,
                "orders.created"
            )
            .is_err());
        assert!(auth2
            .authorize(
                &ctx("org-1", "asystent"),
                BusAction::Consume,
                "orders.created"
            )
            .is_ok());
    }

    /// An addon allow row still answers for the addon, and an allow for one
    /// action does not open another (the matrix still gates the action).
    #[test]
    fn addon_allow_row_admits_the_addon() {
        let (_d, pool) = open_pool();
        let checker = checker(&pool);
        grant(&pool, &checker, &instance_a(), "asystent", "bus.read");
        topic_rule(&pool, "addon", "asystent", "allow");
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker);
        let c = addon_ctx("org-1", "asystent");
        assert!(auth
            .authorize(&c, BusAction::Consume, "orders.created")
            .is_ok());
        assert!(auth
            .authorize(&c, BusAction::Produce, "orders.created")
            .is_err());
    }

    // ---- API key subjects (package K, owner decision P5) --------------------

    /// Creates an active API key of `key_type` and returns its uid.
    fn api_key(pool: &DbPool, key_type: &str) -> String {
        let subject = (key_type == "user").then_some("u-owner");
        let (_, uid) = repository::create_api_key(
            pool,
            &format!("verifier-{}", uuid::Uuid::new_v4()),
            "sk-...abcdef",
            "Laboratorium LIS",
            key_type,
            subject,
            60,
        )
        .unwrap();
        uid
    }

    fn key_ctx(org_id: &str, key_uid: &str) -> BusCallContext {
        BusCallContext {
            actor_kind: ActorKind::ApiKey,
            ..ctx(org_id, key_uid)
        }
    }

    fn key_rule(pool: &DbPool, org_id: &str, key_uid: &str, action: &str, level: &str) {
        let resource_id = topic_acl_resource_id(instance_a().as_str(), org_id, "orders.created");
        repository::resource_permissions::set_with_action(
            pool,
            "topic",
            &resource_id,
            "api_key",
            key_uid,
            action,
            level,
        )
        .unwrap();
    }

    fn key_may(auth: &InstanceBusAuthorizer, c: &BusCallContext, action: BusAction) -> bool {
        auth.authorize(c, action, "orders.created").is_ok()
    }

    /// Default DENY: a general key with no row holds nothing — unlike a user,
    /// whose topic ACL is default-allow behind the matrix.
    #[test]
    fn api_key_without_a_row_holds_nothing() {
        let (_d, pool) = open_pool();
        let checker = checker(&pool);
        let key = api_key(&pool, "general");
        // A matrix grant naming the key's uid must not matter: a key has no
        // matrix identity and the matrix is never asked about it.
        grant(&pool, &checker, &instance_a(), &key, "bus.read");
        grant(&pool, &checker, &instance_a(), &key, "bus.write");
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker);
        let c = key_ctx("org-1", &key);
        assert!(!key_may(&auth, &c, BusAction::Consume));
        assert!(!key_may(&auth, &c, BusAction::Produce));
    }

    /// Read and write are separate grants; neither implies the other, and no
    /// matrix row is needed for either.
    #[test]
    fn api_key_read_and_write_are_separate_grants() {
        let (_d, pool) = open_pool();
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker(&pool));
        let reader = api_key(&pool, "general");
        let writer = api_key(&pool, "general");
        key_rule(&pool, "org-1", &reader, "read", "allow");
        key_rule(&pool, "org-1", &writer, "write", "allow");
        let r = key_ctx("org-1", &reader);
        let w = key_ctx("org-1", &writer);
        assert!(key_may(&auth, &r, BusAction::Consume));
        assert!(!key_may(&auth, &r, BusAction::Produce));
        assert!(key_may(&auth, &w, BusAction::Produce));
        assert!(!key_may(&auth, &w, BusAction::Consume));
    }

    /// A key never administers a topic, and a `'*'` row — the shape written
    /// before key rights were split — grants nothing while still denying.
    #[test]
    fn api_key_never_admin_and_star_rows_only_deny() {
        let (_d, pool) = open_pool();
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker(&pool));
        let key = api_key(&pool, "general");
        key_rule(&pool, "org-1", &key, "admin", "allow");
        key_rule(&pool, "org-1", &key, "*", "allow");
        let c = key_ctx("org-1", &key);
        assert!(!key_may(&auth, &c, BusAction::Admin));
        assert!(!key_may(&auth, &c, BusAction::Consume));
        assert!(!key_may(&auth, &c, BusAction::Produce));

        let denied = api_key(&pool, "general");
        key_rule(&pool, "org-1", &denied, "read", "allow");
        key_rule(&pool, "org-1", &denied, "*", "deny");
        assert!(!key_may(
            &auth,
            &key_ctx("org-1", &denied),
            BusAction::Consume
        ));
    }

    /// A grant belongs to one organisation: naming another one finds no row.
    #[test]
    fn api_key_grant_is_bound_to_its_organisation() {
        let (_d, pool) = open_pool();
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker(&pool));
        let key = api_key(&pool, "general");
        key_rule(&pool, "org-1", &key, "read", "allow");
        assert!(key_may(&auth, &key_ctx("org-1", &key), BusAction::Consume));
        assert!(!key_may(&auth, &key_ctx("org-2", &key), BusAction::Consume));
        // Nor does the grant reach the same topic on another instance.
        let auth_b = InstanceBusAuthorizer::new(pool.clone(), instance_b(), checker(&pool));
        let other = BusCallContext {
            instance_id: instance_b(),
            ..key_ctx("org-1", &key)
        };
        assert!(!key_may(&auth_b, &other, BusAction::Consume));
    }

    /// Only an existing, active, general key acts as itself: a revoked key's
    /// leftover row and a user-bound key's row admit nothing.
    #[test]
    fn api_key_rows_count_only_for_a_live_general_key() {
        let (_d, pool) = open_pool();
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker(&pool));
        let key = api_key(&pool, "general");
        key_rule(&pool, "org-1", &key, "read", "allow");
        assert!(key_may(&auth, &key_ctx("org-1", &key), BusAction::Consume));
        repository::delete_api_key_by_uid(&pool, &key).unwrap();
        assert!(!key_may(&auth, &key_ctx("org-1", &key), BusAction::Consume));

        let user_key = api_key(&pool, "user");
        key_rule(&pool, "org-1", &user_key, "read", "allow");
        assert!(!key_may(
            &auth,
            &key_ctx("org-1", &user_key),
            BusAction::Consume
        ));
    }

    /// A key reads no reserved topic, not even the DLQ of a topic it reads,
    /// and its uid never passes for the system actor.
    #[test]
    fn api_key_never_reaches_a_reserved_topic() {
        let (_d, pool) = open_pool();
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker(&pool));
        let key = api_key(&pool, "general");
        key_rule(&pool, "org-1", &key, "read", "allow");
        let c = key_ctx("org-1", &key);
        assert!(auth
            .authorize(&c, BusAction::Consume, "__dlq.orders.created")
            .is_err());
        let as_system = key_ctx("org-1", SYSTEM_ACTOR);
        assert!(auth
            .authorize(&as_system, BusAction::Consume, "__bus.metrics")
            .is_err());
    }

    /// A key consumes only under its own `k:` groups, and a `k:` group is its
    /// key's alone: no user, addon or other key consumes (and so commits)
    /// under it, so neither side can move the other's offsets.
    #[test]
    fn api_key_groups_and_ordinary_groups_never_meet() {
        let (_d, pool) = open_pool();
        let checker = checker(&pool);
        grant(&pool, &checker, &instance_a(), "u-op", "bus.read");
        grant(&pool, &checker, &instance_a(), "asystent", "bus.read");
        let auth = InstanceBusAuthorizer::new(pool.clone(), instance_a(), checker);
        let key = api_key(&pool, "general");
        let other = api_key(&pool, "general");
        key_rule(&pool, "org-1", &key, "read", "allow");
        key_rule(&pool, "org-1", &other, "read", "allow");
        let group = |c: &BusCallContext, g: &str| {
            auth.authorize_group(c, BusAction::Consume, "orders.created", g)
                .is_ok()
        };
        let k = key_ctx("org-1", &key);
        assert!(group(&k, &format!("k:{key}")));
        assert!(group(&k, &format!("k:{key}.lis")));
        for foreign in [
            "billing".to_string(),
            key.clone(),
            format!("{key}.lis"),
            format!("k:{key}x"),
            format!("k:{key}."),
            format!("k:{other}"),
        ] {
            assert!(!group(&k, &foreign), "{foreign}");
        }
        let user = ctx("org-1", "u-op");
        let addon = addon_ctx("org-1", "asystent");
        assert!(group(&user, "billing"));
        assert!(group(&addon, "billing"));
        for taken in [format!("k:{key}"), format!("k:{key}.lis")] {
            assert!(!group(&user, &taken), "{taken}");
            assert!(!group(&addon, &taken), "{taken}");
            assert!(!group(&key_ctx("org-1", &other), &taken), "{taken}");
        }
    }
}
