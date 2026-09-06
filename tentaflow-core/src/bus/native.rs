// =============================================================================
// File: bus/native.rs — TentaBus as a native app (plan-app-platform §7 W6).
//       Lifecycle hooks the platform calls per instance: `native_init`
//       provisions the instance's own `tentabus.db`, `native_on_enable`/
//       `native_on_disable` start/stop the engine (and, when a mesh manager
//       is running on this node, replication), `native_teardown_plan`/
//       `native_teardown` preview and execute the uninstall wipe.
//
//       Until this file's `native_on_enable` runs for at least one enabled
//       instance, the production binary holds no running TentaBus engine at
//       all — W4 deliberately removed the process-wide `bus::init` call from
//       `main.rs`, and W5 stashed the mesh manager handle in
//       `replication::router` for exactly this file to pick up.
// =============================================================================

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::Result;
use dashmap::DashMap;

use crate::addon::native_apps::{NativeAppContext, TeardownEntry};
use crate::addon::permissions::{global_permission_checker, PermissionChecker};
use crate::bus::replication::glue::PartitionProvider;
use crate::bus::replication::init::ReplicationInitConfig;
use crate::bus::replication::manager::ReplicationManager;
use crate::bus::replication::router;
use crate::bus::{BusInitConfig, DEFAULT_PUBLISH_ACK_TIMEOUT};
use crate::db::DbPool;
use crate::services::bus_authorizer::InstanceBusAuthorizer;

use super::db;
use super::instance::BusInstanceId;

/// `[addon].id` in `bus/app-manifest.toml`, the id `addon::native_apps`
/// registers this package's hooks under.
pub const PACKAGE_ID: &str = BusInstanceId::PACKAGE_ID;

/// `BusInitConfig::retention_interval`'s own doc: "PLAN's own default once
/// wired into real startup is 5 minutes" — this is that wiring.
const DEFAULT_RETENTION_INTERVAL: Duration = Duration::from_secs(300);
/// `BusInitConfig::dedup_expected_rate_per_sec`'s own doc: "PLAN's own
/// default... is 10,000 msg/s".
const DEFAULT_DEDUP_RATE_PER_SEC: u64 = 10_000;

/// Serializes the WHOLE enable/disable transition, across every TentaBus
/// instance — closes the TOCTOU carried over from W5 (plan-app-platform §7
/// W6's "two defects W5 handed forward" #2): `replication::init` resolves
/// `bus::instance(&id)` at its very top and only calls `set_replication`/
/// `set_assignment_store` at its very end (`init.rs`'s own doc on that
/// function); a concurrent `native_on_disable` racing in that window would
/// stop the engine `init` is still wiring, leaving a router-registered
/// manager serving a stopped engine with no coordinator ever attached. The
/// only two callers that can reach `bus::init_instance`/`stop_instance` for
/// a TentaBus instance in this build are `native_on_enable`/
/// `native_on_disable` themselves (`notify_enabled`'s enable/disable arms,
/// `addon/native_apps.rs`), so a lock held for each hook's ENTIRE body
/// closes the race completely, not just narrows it.
///
/// Global rather than per-instance: enable/disable is admin-toggle/
/// reconcile-rate, never a hot path — mirrors `bus::mod::INIT_INSTANCE_LOCK`'s
/// own reasoning for the same tradeoff on the engine registry itself.
static ENABLE_DISABLE_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

/// This node's own `ReplicationManager` for every TentaBus instance whose
/// replication `native_on_enable` started. `BusService::replication()` only
/// exposes the trait object `Arc<dyn ReplicationCoordinator>`, which cannot
/// be downcast back to the concrete `Arc<ReplicationManager>`
/// `replication::init::stop` needs — so this module remembers the concrete
/// value directly instead. `native_on_disable`/`native_teardown` (via
/// `native_on_disable`) remove and stop the entry for their own instance;
/// nothing else reads this map. Safe against the enable/disable race
/// `ENABLE_DISABLE_LOCK` closes: an insert (end of a successful enable) and
/// a remove (start of a disable) for the SAME instance can never interleave.
static REPLICATION_MANAGERS: OnceLock<DashMap<BusInstanceId, Arc<ReplicationManager>>> =
    OnceLock::new();

fn replication_managers() -> &'static DashMap<BusInstanceId, Arc<ReplicationManager>> {
    REPLICATION_MANAGERS.get_or_init(DashMap::new)
}

/// The instance's own `tentabus.db` (consumer groups, pause state), opened
/// on first use. Everything else — topics, partitions, schemas, field
/// policies, ACLs — stays in the main database (§1.4); nothing else lives
/// here.
pub fn open_db(main_db: &DbPool, org_id: &str, addon_id: &str) -> Result<DbPool> {
    crate::addon::app_db::open(main_db, org_id, addon_id, db::migrate)
}

/// Native init hook: creates the instance's `tentabus.db` and brings its
/// schema up to date. Idempotent — it re-runs on install, on every enable
/// (`native_on_enable` calls it again below), on a replicated
/// install/update (`AddonManager::reconcile_synced_addon`), and on every
/// boot for every ENABLED instance (`AddonManager::start_installed_native_
/// instances`, called from `tentaflow/src/main.rs`). Deliberately does NOT
/// start the engine (`bus::init_instance`) or touch `<data dir>/log/`: a
/// freshly installed instance starts DISABLED (`addon/lifecycle.rs:268-277`),
/// and a disabled bus must not hold flocks on its segments.
pub fn native_init(ctx: &NativeAppContext) -> Result<()> {
    open_db(ctx.db, ctx.org_id, ctx.addon_id)?;
    tracing::info!(
        "native app '{}': TentaBus instance initialized at {:?}",
        ctx.addon_id,
        ctx.data_dir
    );
    Ok(())
}

/// Starts the engine, its three background threads (metrics rollup,
/// retention sweeper, audit-flush timer — all three spawned by
/// `bus::init_instance` itself), and — when a mesh manager is running on
/// this node (`replication::router::mesh_manager()`, set by `main.rs` right
/// after the mesh pipeline starts, W5) — replication and the accept-handler
/// registration. Idempotent: `bus::init_instance` returns the already-
/// running engine on a second call, and this function only ever attempts
/// `replication::init` ONCE per instance (guarded by `REPLICATION_MANAGERS`
/// already holding an entry) — a re-run after the mesh case succeeded
/// already is a plain no-op past that check.
///
/// When the mesh is absent (single-node / mesh-disabled build, or this call
/// races AHEAD of mesh startup at boot — `main.rs` starts the mesh AFTER
/// the native-instance boot pass, an ordering gap this hook cannot close
/// from its own side), replication is skipped — exactly today's RF=1
/// behavior, not an error. Review finding F2 (fixed): `main.rs` closes that
/// exact gap right after the mesh starts, via `start_replication_for_
/// already_enabled_instances` below — every already-enabled instance that
/// came up through THIS call with no mesh yet gets one more, deterministic
/// chance to start replication a few lines later in `main.rs`, not only on
/// its next dashboard-toggle/reconcile. A later successful `native_on_
/// enable` call for the SAME instance (e.g. the dashboard toggle, or a
/// second reconcile) still has a chance to pick replication up too, since
/// the `REPLICATION_MANAGERS` guard above only blocks a redundant SECOND
/// attempt, not every attempt after the first one skipped.
pub fn native_on_enable(ctx: &NativeAppContext) -> Result<()> {
    let _guard = ENABLE_DISABLE_LOCK.lock();

    // Re-provisions the content db — cheap and idempotent (`native_init`'s
    // own doc); a reconcile-driven enable may arrive without a prior local
    // `init` call having ever run on this node.
    native_init(ctx)?;

    let instance_id = BusInstanceId::parse(ctx.addon_id).map_err(|e| {
        anyhow::anyhow!(
            "native_on_enable: '{}' is not a valid TentaBus instance id: {e}",
            ctx.addon_id
        )
    })?;

    let local_db = open_db(ctx.db, ctx.org_id, ctx.addon_id)?;
    let bus_dir = ctx.data_dir.join("log");

    // The SAME warm, cached `PermissionChecker` every dispatch request
    // authorizes through (`addon::permissions::global_permission_checker`'s
    // own doc) — never a fresh, empty-cache one, which would deny every
    // `bus.*` permission check until its own background refresh caught up.
    //
    // Review finding F6: in EVERY real production boot this branch is
    // unreachable — `AddonManager::new` (which calls `set_global_
    // permission_checker`) always runs before `AddonManager::
    // start_installed_native_instances` can call this hook, and the
    // manager itself lives for the process's whole lifetime, so the `Weak`
    // cell stays warm for as long as any instance could possibly enable.
    // It IS reachable from a bare test/tooling context that builds a
    // `NativeAppContext` directly, with no `AddonManager` ever
    // constructed — this file's own tests do exactly that. The fallback
    // used to build a `PermissionChecker` and hand it straight to
    // `InstanceBusAuthorizer` with NO `refresh_all()`/`start_background_
    // refresh()` ever called on it: a cache that starts empty and is never
    // populated denies every check FOREVER, not merely "until it warms" —
    // a silent, permanent deny-all that would be very hard to diagnose in
    // whatever unusual boot path actually hits it. Warmed synchronously
    // here (one DB read, the same `refresh_all` `AddonManager::new` itself
    // calls) and kept warm for as long as this authorizer/engine lives
    // (`start_background_refresh`, same 5-minute cadence, when a Tokio
    // runtime is actually available — `PermissionChecker::start_background_
    // refresh` unconditionally `tokio::spawn`s, which panics rather than
    // erroring outside one, so this hook cannot call it unguarded from a
    // plain `#[test]` fn the way this file's own bare-context tests do) —
    // an honest, functioning degraded mode instead of a trap.
    let checker = global_permission_checker().unwrap_or_else(|| {
        tracing::warn!(
            "native_on_enable '{}': no global PermissionChecker registered yet \
             (AddonManager not constructed on this node?) — building and warming a \
             standalone one for this instance's own authorizer",
            ctx.addon_id
        );
        let checker = Arc::new(PermissionChecker::new(ctx.db.clone()));
        checker.refresh_all();
        if tokio::runtime::Handle::try_current().is_ok() {
            checker.start_background_refresh();
        }
        checker
    });
    let authorizer = Arc::new(InstanceBusAuthorizer::new(
        ctx.db.clone(),
        instance_id.clone(),
        checker,
    ));

    let service = crate::bus::init_instance(BusInitConfig {
        instance_id: instance_id.clone(),
        bus_dir,
        db: ctx.db.clone(),
        local_db,
        authorizer,
        retention_interval: Some(DEFAULT_RETENTION_INTERVAL),
        dedup_expected_rate_per_sec: DEFAULT_DEDUP_RATE_PER_SEC,
        partition_handle_lru: None,
        publish_ack_timeout: DEFAULT_PUBLISH_ACK_TIMEOUT,
    })?;

    if replication_managers().contains_key(&instance_id) {
        return Ok(());
    }

    let Some(mesh) = router::mesh_manager() else {
        tracing::info!(
            "native_on_enable '{}': no mesh manager on this node — RF=1, replication \
             skipped (today's single-node behavior)",
            ctx.addon_id
        );
        return Ok(());
    };

    // `replication::init` is async; this hook is not (`NativeAppHooks::
    // on_enable`'s signature). Mirrors `tentanas::native_teardown`'s own
    // `block_in_place` idiom for the same reason — enable/disable is a
    // rare, deliberate transition, not a hot path, so blocking this worker
    // for the length of it is the honest trade.
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::warn!(
            "native_on_enable '{}': no tokio runtime on this thread — replication not \
             started (unit-test context?)",
            ctx.addon_id
        );
        return Ok(());
    };

    try_start_replication(&instance_id, &service, mesh, &handle);
    Ok(())
}

/// Shared tail of `native_on_enable`'s own replication step and
/// `start_replication_for_already_enabled_instances`'s boot-time re-arm
/// (plan-app-platform §8 R10, review finding F2) — both need the exact same
/// `ReplicationInitConfig` construction and success/failure handling, for a
/// `service` that is already running and a `mesh` that is already known to
/// exist. Idempotent via the SAME `REPLICATION_MANAGERS` guard both callers
/// also check up front — calling this twice for an instance that already
/// has a manager is a silent no-op, not a duplicate `ReplicationManager`.
fn try_start_replication(
    instance_id: &BusInstanceId,
    service: &Arc<crate::bus::BusService>,
    mesh: Arc<crate::mesh::iroh_manager::IrohMeshManager>,
    handle: &tokio::runtime::Handle,
) {
    if replication_managers().contains_key(instance_id) {
        return;
    }
    let local_node_id = mesh.node_id();
    let local_env = crate::services::environment::get_node_environment(service.db());
    let provider: Arc<dyn PartitionProvider> = service.clone();
    let repl_cfg = ReplicationInitConfig {
        db: service.db().clone(),
        mesh,
        instance_id: instance_id.clone(),
        local_node_id,
        local_env,
        provider,
        lease_check_interval: ReplicationInitConfig::DEFAULT_LEASE_CHECK_INTERVAL,
    };
    let result = tokio::task::block_in_place(|| {
        handle.block_on(crate::bus::replication::init::init(repl_cfg))
    });
    match result {
        Ok(manager) => {
            replication_managers().insert(instance_id.clone(), manager);
        }
        Err(e) => {
            // Best-effort: the engine itself is up and serving at RF=1
            // (today's behavior for any topic whose replication factor
            // cannot be honored) — a replication wiring failure must not
            // fail the whole enable and leave the instance unreachable.
            tracing::warn!(
                "TentaBus '{instance_id}': replication init failed, continuing without a \
                 coordinator: {e}"
            );
        }
    }
}

/// plan-app-platform §8 R10 remediation (review finding F2, BLOCKER):
/// `tentaflow/src/main.rs` calls `AddonManager::start_installed_native_
/// instances` — which runs `native_on_enable` for every already-enabled
/// instance — strictly BEFORE the mesh pipeline starts and `replication::
/// router::set_mesh_manager` runs. Every such instance's own `native_on_
/// enable` call therefore observed `router::mesh_manager() == None` and
/// came up at RF=1 even on a node where a mesh IS configured — replication
/// silently never started for any pre-existing enabled instance across a
/// restart. `main.rs` calls this function ONCE, synchronously, right after
/// `router::set_mesh_manager` (its own call site is right there, no
/// sleep/retry involved — deterministic by construction: the mesh handle
/// is known-set the instant this runs), to re-arm replication for exactly
/// those engines.
///
/// A no-op for:
///  - a build with no mesh configured at all (`router::mesh_manager()` is
///    `None` here too) — the legitimate RF=1 case (R10's own remediation
///    text: "skips replication when absent, which is exactly today's RF=1
///    behaviour"). Every already-running instance stays exactly as `native_
///    on_enable` left it: enabled, serving, usable, just without a
///    coordinator. No log at all for this case — `native_on_enable`'s own
///    `info!` already covered it once, at enable time, per instance;
///  - an instance whose `native_on_enable` already started replication (a
///    first install/enable happening AFTER mesh startup, or a second call
///    racing this one under `ENABLE_DISABLE_LOCK`) — `REPLICATION_MANAGERS`
///    already holding an entry is the exact idempotency guard `native_on_
///    enable` itself relies on, reused here via `try_start_replication`'s
///    own check.
///
/// Logs at `warn!` (not silently) when a mesh DOES exist but a specific
/// already-running instance still ends up with no coordinator for any
/// OTHER reason (`replication::init` itself failing) — `try_start_
/// replication`'s own `warn!` on that path covers it; unlike the "no mesh
/// at all" case, that is unexpected and must stay visible.
pub fn start_replication_for_already_enabled_instances() {
    let _guard = ENABLE_DISABLE_LOCK.lock();
    let Some(mesh) = router::mesh_manager() else {
        return;
    };
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::warn!(
            "start_replication_for_already_enabled_instances: no tokio runtime on this \
             thread — replication re-arm skipped"
        );
        return;
    };
    for service in crate::bus::running_instances() {
        let instance_id = match BusInstanceId::parse(service.instance_id()) {
            Ok(id) => id,
            Err(e) => {
                tracing::warn!(
                    "start_replication_for_already_enabled_instances: '{}' is not a valid \
                     TentaBus instance id, skipping: {e}",
                    service.instance_id()
                );
                continue;
            }
        };
        try_start_replication(&instance_id, &service, Arc::clone(&mesh), &handle);
    }
}

/// `disable_semantics = "stop"`: stops replication first (both leader and
/// follower runners, plus the router registration — `replication::init::
/// stop`), then the engine itself via `bus::stop_instance`, which now
/// (plan-app-platform §1.2, review finding F1/F5) calls `BusService::
/// shutdown()` on the value the registry removal returns: cancels the
/// three background threads (metrics rollup, retention sweeper,
/// audit-flush timer, all three gated by the same shutdown flag), and
/// eagerly drops every partition handle this engine cached — its own
/// (`partitions`) and every one a still-open `ConsumerHandle` is
/// separately holding (`consumer_partitions`) — so another process could
/// take the segments over. Requests already reject at the gate
/// (`AppUnavailable`); in-flight consumers fail their own authorizer
/// generation re-check on their next fetch/commit.
///
/// Together with `bus::replication::glue`'s `Weak<dyn PartitionProvider>`
/// fix (review finding F1a), this closes the reference cycle that used to
/// keep the disabled instance's `Arc<BusService>` — and the OS-level
/// advisory lock over `<data dir>/log/_meta`, `fjall::Database` is
/// `Arc`-backed internally — alive forever: stopping a replicating
/// instance no longer leaks the engine, and re-enabling the SAME instance
/// no longer risks opening a second `fjall::Database` over the same
/// directory.
///
/// Review finding F3 (closed): a `ConsumerHandle` opened through the WASM
/// host-function surface (`addon::host_functions::bus`) holds its OWN
/// clone of the engine's `offsets` store, entirely independent of the maps
/// `BusService::shutdown()` clears — left open across a disable, it used
/// to keep the SAME `_meta` lock alive for up to that module's own
/// `CONSUMER_IDLE_TIMEOUT` (300 s), which on its own would have defeated
/// F1's fix (the next enable would still race a stale fjall lock). Closed
/// by calling `addon::host_functions::bus::close_consumers_for_instance`
/// here, BEFORE `bus::stop_instance` (so the underlying engine the handle's
/// `fetch`/`commit` calls resolve against is still valid for however long
/// an already-in-flight call takes to finish) — this instance's own open
/// consumers are gone from that registry by the time this function
/// returns, not merely eligible for the idle sweeper to find eventually.
///
/// Idempotent: `close_consumers_for_instance`, `bus::stop_instance`, and a
/// missing `REPLICATION_MANAGERS` entry are all documented no-ops when
/// there is nothing left to do.
///
/// Does NOT touch the bus reactor (`bus::reactor`): pre-W8, the reactor is
/// a single PROCESS-GLOBAL subscriber against the `bus::global()`
/// single-instance shim, not yet instance-scoped (flow-node/reactor
/// instance threading is W8's own scope, §7's work-package table). §1.8's
/// own "disable" row lists exactly `bus::stop_instance` + `replication::
/// stop` + `router::unregister` — no reactor call — because `stop_instance`
/// already clears that shim when the stopped instance was the one engine it
/// pointed at, and the reactor's `subscription_loop` already tolerates
/// `bus::global()` returning `None` on its next poll tick (its own doc).
/// There is no reachable `reactor::stop_for_instance` (or equivalent) to
/// call today; inventing one here would be exactly the kind of workaround
/// this wave was told not to build for a gap that belongs to a later wave.
pub fn native_on_disable(ctx: &NativeAppContext) {
    let _guard = ENABLE_DISABLE_LOCK.lock();
    let Ok(instance_id) = BusInstanceId::parse(ctx.addon_id) else {
        tracing::warn!(
            "native_on_disable: '{}' is not a valid TentaBus instance id — nothing to stop",
            ctx.addon_id
        );
        return;
    };
    if let Some((_, manager)) = replication_managers().remove(&instance_id) {
        crate::bus::replication::init::stop(&manager);
    }
    // Review finding F3: closes every WASM-side `ConsumerHandle` this
    // instance's own addons still have open, BEFORE tearing the engine
    // down — see this function's own doc for why the ordering matters.
    crate::addon::host_functions::bus::close_consumers_for_instance(instance_id.as_str());
    crate::bus::stop_instance(&instance_id);
}

/// Counts rows of one of the five instance-scoped core tables
/// (`table` is always one of this module's own literals, never caller
/// input) for `instance_id`.
fn count_core_rows(db: &DbPool, table: &str, instance_id: &str) -> Result<u32> {
    let conn = db
        .read()
        .map_err(|_| anyhow::anyhow!("{table}: db lock poisoned"))?;
    let count: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM {table} WHERE instance_id = ?1"),
        rusqlite::params![instance_id],
        |row| row.get(0),
    )?;
    Ok(count as u32)
}

/// Consumer-group row count from the instance's OWN `tentabus.db`
/// (`bus_groups` has no `instance_id` column — the file IS the instance,
/// §1.4). Best-effort: `native_teardown_plan` must work for a disabled, or
/// even never-fully-provisioned, instance, and the dialog must never fail
/// over a missing content db — a fresh install whose `native_init` has not
/// run yet simply reports 0 groups rather than erroring the whole plan.
///
/// Review finding F7 (BLOCKER-adjacent): this used to call `open_db`
/// (`app_db::open`), which — on the common path where `tentabus.db` does
/// not exist yet — CREATES the file and runs the whole migration ladder on
/// it. A "plan" call (called on every uninstall-dialog open, including for
/// an instance nobody has enabled yet) must never have that side effect.
/// This resolves `ctx.data_dir.join(db_file)` — in PRODUCTION the exact
/// path `native_init` itself wrote to (the platform always computes
/// `ctx.data_dir` as `fs_sandbox::addon_data_dir(org_id, addon_id)` before
/// calling any hook, §1.3), so no second call into `fs_sandbox` is needed
/// here — and opens it `SQLITE_OPEN_READ_ONLY` ONLY IF the file already
/// exists on disk; a missing row, an unparseable manifest, no declared
/// `db_file`, or a missing file all fold into the same honest "0 groups",
/// not an error — `native_teardown_plan` must keep working for a disabled,
/// or even never-fully-provisioned, instance.
fn count_bus_groups(ctx: &NativeAppContext) -> u32 {
    let Some(db_path) = resolve_content_db_path(ctx) else {
        return 0;
    };
    if !db_path.is_file() {
        return 0;
    }
    let conn = match rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(
                "native_teardown_plan '{}': tentabus.db exists but could not be opened \
                 read-only, reporting 0 consumer groups: {e}",
                ctx.addon_id
            );
            return 0;
        }
    };
    conn.query_row("SELECT COUNT(*) FROM bus_groups", [], |row| {
        row.get::<_, i64>(0)
    })
    .map(|n| n as u32)
    .unwrap_or(0)
}

/// The on-disk path `open_db`/`app_db::open` would resolve for this
/// instance's content database, computed the SAME way (the `addons` row's
/// manifest `native.db_file`, joined onto `ctx.data_dir`) but WITHOUT any
/// of `app_db::open`'s side effects (no `Connection::open` create-if-
/// missing, no migration, no process-global registry insert) — a genuine
/// "where would it be" lookup for `count_bus_groups`'s read-only caller.
/// `None` for anything that would have made `app_db::open` itself fail (no
/// `addons` row, an unparseable manifest, no declared `db_file`) — all of
/// which mean "nothing to count" here, not an error worth surfacing from a
/// plan call.
fn resolve_content_db_path(ctx: &NativeAppContext) -> Option<std::path::PathBuf> {
    let row = crate::db::repository::get_addon(ctx.db, ctx.addon_id)
        .ok()
        .flatten()?;
    let manifest = crate::addon::lifecycle::parse_manifest_toml(&row.manifest_json).ok()?;
    let db_file = manifest
        .native
        .as_ref()
        .and_then(|n| n.db_file.as_deref())?;
    Some(ctx.data_dir.join(db_file))
}

/// Teardown plan (§7 W6), side-effect free — the uninstall dialog calls it
/// on every open, including for a DISABLED instance, so this makes no
/// engine call. `lifecycle::teardown_plan` sizes each returned entry
/// INDEPENDENTLY via `path_size` and the uninstall dialog SUMS `sizeBytes`
/// over every `removed` entry (`www/js/modules/addons/uninstall-dialog.js`),
/// so a second entry whose path is a subset of (or equal to) the data dir
/// would double- or triple-count bytes in the operator-facing total.
///
/// §7 W6's own table lists THREE entries (`tentabus_log` at `<data dir>/log`,
/// `tentabus_data_dir` at `<data dir>`, and `tentabus_core_rows` — marked
/// "(marker)" — also at `<data dir>`), which is exactly that double/triple
/// count: `log/` is a subset of the data dir, and the "marker" entry
/// repeats the data dir's own path a third time with no bytes of its own.
/// The two tests below (`teardown_plan_lists_exactly_one_entry_covering_
/// the_whole_data_dir`, `teardown_plan_has_no_duplicate_paths`) predate W6
/// and lock in the correct behavior instead: ONE entry, ONE path,
/// `path_size` walking the WHOLE data dir (`log/` and `tentabus.db`
/// together) exactly once. Resolution (§0.6 correction #8 anticipated
/// this): `TeardownEntry::description` is widened from `&'static str` to
/// `Cow<'static, str>` so the per-instance row counts the plan's table
/// wanted can be folded into this ONE entry's description as formatted
/// text instead of a second/third entry — the operator still sees every
/// number the plan asked for, the dialog's byte total stays correct, and
/// neither pre-existing test needed to change.
pub fn native_teardown_plan(ctx: &NativeAppContext) -> Result<Vec<TeardownEntry>> {
    let topics = count_core_rows(ctx.db, "bus_topics", ctx.addon_id)?;
    let assignments = count_core_rows(ctx.db, "bus_partition_assignments", ctx.addon_id)?;
    let field_policies = count_core_rows(ctx.db, "bus_field_policies", ctx.addon_id)?;
    let schema_subjects = count_core_rows(ctx.db, "bus_schema_subjects", ctx.addon_id)?;
    let schema_versions = count_core_rows(ctx.db, "bus_schema_versions", ctx.addon_id)?;
    // Amendment 9f: NOT a hand-rolled `"<instance>/"` prefix — the same
    // length-prefixed codec the ACL write side keys with
    // (`services::bus_authorizer::topic_acl_resource_id`), whose first
    // segment is a genuine prefix of every longer id built from the same
    // leading element (`sync::resource_id::composite_resource_id`'s doc).
    let acl_prefix = crate::sync::resource_id::composite_resource_id(&[ctx.addon_id]);
    let acl_rows = crate::db::repository::resource_permissions::count_topic_acl_by_instance_prefix(
        ctx.db,
        &acl_prefix,
    )?;
    let groups = count_bus_groups(ctx);

    let description = format!(
        "instance data directory: log/ (topics, partitions and segments) plus tentabus.db \
         (consumer groups: {groups}, pause state); core rows tracked separately in the \
         platform database: {topics} topics, {assignments} partition assignments, \
         {field_policies} field policies, {schema_subjects} schema subjects, \
         {schema_versions} schema versions, {acl_rows} topic ACL rows"
    );
    Ok(vec![TeardownEntry {
        path: ctx.data_dir.clone(),
        kind: "tentabus_data_dir",
        description: description.into(),
        removed: true,
    }])
}

/// Full teardown (§7 W6), run right before the platform wipes the data dir
/// (`lifecycle.rs:429-434`), in order:
/// 1. `native_on_disable` — stop the engine + replication + router
///    unregister (via `replication::stop`), so nothing is still writing to
///    what steps 3-4 are about to delete;
/// 2. `app_db::close` — MANDATORY before the platform's `remove_dir_all`:
///    an open WAL handle blocks it on Windows (`lifecycle.rs:461-463`'s own
///    comment);
/// 3. delete this instance's rows in the five core tables, CHILDREN FIRST
///    (`bus_schema_versions` before `bus_schema_subjects` — the one real
///    FK/cascade relationship among these five, see `bus_schema_versions_
///    delete_by_instance`'s doc for why each version still gets its own op
///    instead of relying solely on the cascade), each publishing a sync
///    Delete tombstone so the uninstall propagates fleet-wide instead of
///    resurrecting on the next reconcile;
/// 4. delete this instance's topic ACL rows the same way (amendment 9f's
///    prefix, not a literal slash-join);
/// 5. bump the schema-registry generation — every node's cached validator
///    for this instance's subjects is now stale;
/// 6. audit `bus.instance.teardown` with the counts.
///
/// Does NOT remove `<data dir>/log/` itself — the platform removes it with
/// the rest of the data dir right after this call returns.
pub fn native_teardown(ctx: &NativeAppContext) -> Result<()> {
    native_on_disable(ctx);
    crate::addon::app_db::close(ctx.addon_id);

    let instance_id = ctx.addon_id;
    let topics = crate::db::repository::bus_topics_delete_by_instance(ctx.db, instance_id)?;
    let assignments =
        crate::db::repository::bus_partition_assignments_delete_by_instance(ctx.db, instance_id)?;
    let field_policies =
        crate::db::repository::bus_field_policies_delete_by_instance(ctx.db, instance_id)?;
    let schema_versions =
        crate::db::repository::bus_schema_versions_delete_by_instance(ctx.db, instance_id)?;
    let schema_subjects =
        crate::db::repository::bus_schema_subjects_delete_by_instance(ctx.db, instance_id)?;

    let acl_prefix = crate::sync::resource_id::composite_resource_id(&[instance_id]);
    let acl_rows =
        crate::db::repository::resource_permissions::delete_topic_acl_by_instance_prefix(
            ctx.db,
            &acl_prefix,
        )?;

    crate::bus::schema_registry::bump_generation();

    // Best-effort, same convention as every other post-mutation audit call
    // in this codebase (`bus/mod.rs::delete_topic`'s own `log_audit` site):
    // the deletes above already happened either way, and an audit-write
    // failure must not turn an otherwise-successful teardown into a
    // retry-forever instance the operator can no longer remove.
    let _ = crate::db::repository::log_audit(
        ctx.db,
        None,
        Some(ctx.addon_id),
        "bus.instance.teardown",
        Some(ctx.addon_id),
        Some(&format!(
            "topics={topics} partition_assignments={assignments} \
             field_policies={field_policies} schema_subjects={schema_subjects} \
             schema_versions={schema_versions} topic_acl_rows={acl_rows}"
        )),
        None,
        None,
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(
        db: &'a DbPool,
        addon_id: &'a str,
        data_dir: std::path::PathBuf,
    ) -> NativeAppContext<'a> {
        NativeAppContext {
            db,
            addon_id,
            org_id: "default",
            data_dir,
        }
    }

    fn test_db() -> DbPool {
        let conn = rusqlite::Connection::open_in_memory().expect("open mem");
        crate::db::migrations::run(&conn).expect("migrate");
        std::sync::Arc::new(crate::db::Db::from_connection(conn))
    }

    /// Installs a bare `addons` row for `addon_id` so `app_db::open`
    /// (`open_db`/`native_init`/`native_on_enable`) can resolve a
    /// `native.db_file` to open. Deliberately NOT `bundled::native_manifest`
    /// ("tentabus"): that reads the compiled-in manifest catalog, which is
    /// EMPTY under `TENTAFLOW_FAST_BUILD=1` (`build.rs` skips addon
    /// bundling) — these are `bus::` tests, gated under the fast build, so
    /// they must not depend on it. `native_apps::test_support::
    /// fixture_manifest_toml` is a pure string generator with no bundling
    /// dependency; its own declared `db_file`/`routes` are irrelevant here —
    /// `app_db::open` only ever reads `native.db_file` off whatever manifest
    /// the row carries, never cross-checks it against `addon_id`/
    /// `package_id`.
    fn install_row(db: &DbPool, addon_id: &str) {
        let manifest = crate::addon::native_apps::test_support::fixture_manifest_toml(false);
        let conn = db.write().expect("write");
        conn.execute(
            "INSERT OR IGNORE INTO addons \
             (addon_id, name, version, package_id, package_version, runtime, is_enabled, \
              manifest_json) \
             VALUES (?1, 'Test TentaBus', '1.0.0', ?2, '1.0.0', 'native', 1, ?3)",
            rusqlite::params![addon_id, PACKAGE_ID, manifest],
        )
        .expect("insert test addons row");
    }

    /// Review finding F8: every test in this module that calls
    /// `native_init`/`native_on_enable` reaches `open_db`/`app_db::open`,
    /// which resolves its content-database DIRECTORY via `fs_sandbox::
    /// addon_data_dir(org_id, addon_id)` — a path derived from the REAL
    /// `paths::tentaflow_home()`, entirely independent of whatever tempdir
    /// a test passes as its own `NativeAppContext::data_dir` (that field
    /// only ever backs the bus LOG directory, `bus_dir = ctx.data_dir.
    /// join("log")`, never the content db). Left unguarded, such a test
    /// writes `tentabus.db` into the developer's/CI runner's real
    /// `~/.tentaflow` (or the checkout's `.runtime/`) tree instead of a
    /// throwaway directory.
    ///
    /// Mirrors the SAME one-shot redirect `bus::mod::tests::
    /// locked_ledger_fixture` and `replication::assignment::tests`'s own
    /// copy already use (that pair's own comment names this exact trio as
    /// "confirmed safe to coexist"): `paths::tentaflow_home()` is a
    /// process-wide `OnceLock`, resolved at most once per test BINARY, so
    /// the FIRST caller anywhere in the (possibly filtered) test run wins
    /// for every test that follows — this redirects it exactly once, to a
    /// tempdir deliberately leaked (`std::mem::forget`) so it outlives
    /// every test using it, not just this function call, and serializes
    /// every caller of any of these three copies against each other via
    /// the SHARED `fs_sandbox::test_home_lock()` mutex.
    fn locked_test_home() -> std::sync::MutexGuard<'static, ()> {
        let guard = crate::addon::fs_sandbox::test_home_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        static INITIALIZED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        if INITIALIZED.get().is_none() {
            let tmp = tempfile::tempdir().expect("tempdir");
            std::env::set_var("HOME", tmp.path());
            std::env::set_var("TENTAFLOW_HOME", tmp.path());
            std::mem::forget(tmp);
            let _ = INITIALIZED.set(());
        }
        guard
    }

    /// A fresh `tentabus-<8hex>` id for every test invocation — see
    /// `locked_test_home`'s doc: a hardcoded literal (this file's OWN
    /// scheme before this fix, F8) collides across the `app_db::
    /// registry()` process-global `DashMap` (keyed by `addon_id` alone) the
    /// moment two tests race under the SAME shared home directory. 8
    /// lowercase hex chars — `BusInstanceId::parse`'s own shape (`bus::
    /// instance`), not `fs_sandbox::unique_test_addon_id`'s 32-char UUID
    /// suffix.
    fn unique_addon_id() -> String {
        let hex = uuid::Uuid::new_v4().simple().to_string();
        format!("tentabus-{}", &hex[..8])
    }

    #[test]
    fn package_id_matches_the_instance_id_prefix() {
        assert_eq!(PACKAGE_ID, "tentabus");
        assert_eq!(PACKAGE_ID, BusInstanceId::PACKAGE_ID);
    }

    #[test]
    fn teardown_plan_lists_exactly_one_entry_covering_the_whole_data_dir() {
        let db = test_db();
        let tmp = tempfile::tempdir().expect("tempdir");
        let c = ctx(&db, "tentabus-00000001", tmp.path().to_path_buf());
        let entries = native_teardown_plan(&c).expect("plan");
        let kinds: Vec<&str> = entries.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec!["tentabus_data_dir"],
            "one entry per path — a second entry whose path is a subset of \
             the data dir would double-count bytes in the uninstall dialog's \
             total (it sums sizeBytes over every removed entry independently)"
        );
        assert!(
            entries.iter().all(|e| e.removed),
            "every entry is removed on wipe"
        );
        assert!(entries.iter().any(|e| e.path == tmp.path()));
    }

    #[test]
    fn teardown_plan_has_no_duplicate_paths() {
        let db = test_db();
        let tmp = tempfile::tempdir().expect("tempdir");
        let c = ctx(&db, "tentabus-00000001", tmp.path().to_path_buf());
        let entries = native_teardown_plan(&c).expect("plan");
        let mut paths: Vec<&std::path::Path> = entries.iter().map(|e| e.path.as_path()).collect();
        let before = paths.len();
        paths.sort();
        paths.dedup();
        assert_eq!(
            paths.len(),
            before,
            "no two entries may share a path — the uninstall dialog would \
             render the same directory twice and the byte total would be \
             inflated by however many entries repeat it"
        );
    }

    #[test]
    fn teardown_plan_is_side_effect_free() {
        let db = test_db();
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("marker"), b"x").expect("seed file");
        let c = ctx(&db, "tentabus-00000001", tmp.path().to_path_buf());
        native_teardown_plan(&c).expect("plan");
        assert!(
            tmp.path().join("marker").exists(),
            "plan must not touch the data dir"
        );
    }

    #[test]
    fn teardown_closes_the_content_db_handle_without_error() {
        let db = test_db();
        let tmp = tempfile::tempdir().expect("tempdir");
        let c = ctx(&db, "tentabus-00000001", tmp.path().to_path_buf());
        // No instance row installed — teardown must still succeed: closing an
        // addon_id that was never opened is a documented no-op
        // (`addon/app_db.rs::close`), and every delete_by_instance call below
        // is a legitimate zero-row no-op against a fresh migrated db.
        native_teardown(&c).expect("teardown");
    }

    #[test]
    fn native_teardown_plan_is_side_effect_free_and_counts_rows() {
        let db = test_db();
        let tmp = tempfile::tempdir().expect("tempdir");
        let addon_id = "tentabus-0000000a";
        let c = ctx(&db, addon_id, tmp.path().to_path_buf());

        crate::db::repository::bus_topic_create(
            &db,
            &crate::db::repository::DbBusTopic {
                instance_id: addon_id.to_string(),
                org_id: "org-1".to_string(),
                name: "orders.created".to_string(),
                partitions: 1,
                retention_ms: 0,
                retention_bytes: 0,
                cleanup_policy: "delete".to_string(),
                delivery: "at_least_once".to_string(),
                idempotency_key: None,
                dedup_window_ms: 0,
                max_delivery_attempts: 1,
                retry_backoff_ms: 0,
                schema_id: None,
                validation: "none".to_string(),
                content_type: "application/json".to_string(),
                replication_factor: 1,
                acks: "leader".to_string(),
                durability: "async".to_string(),
                max_inline_bytes: 1_048_576,
                compression: "none".to_string(),
                environment: "prod".to_string(),
                created_at_ms: 0,
                updated_at_ms: 0,
                durability_class: None,
            },
        )
        .expect("create topic");

        crate::db::repository::resource_permissions::set(
            &db,
            "topic",
            &crate::sync::resource_id::composite_resource_id(&[
                addon_id,
                "org-1",
                "orders.created",
            ]),
            "user",
            "u1",
            "deny",
        )
        .expect("seed acl row");

        let before_hash = {
            let conn = db.read().expect("read");
            conn.query_row("SELECT COUNT(*) FROM bus_topics", [], |r| {
                r.get::<_, i64>(0)
            })
            .expect("count")
        };

        let entries = native_teardown_plan(&c).expect("plan");
        assert_eq!(entries.len(), 1, "still exactly one entry");
        let text: &str = &entries[0].description;
        assert!(
            text.contains("1 topics"),
            "description must surface the row counts the plan's table asked \
             for: {text}"
        );
        assert!(
            text.contains("1 topic ACL rows"),
            "description must surface the ACL row count too: {text}"
        );

        // Side-effect free: the row created above (and the topic count) are
        // exactly as they were before the plan call.
        let after_hash = {
            let conn = db.read().expect("read");
            conn.query_row("SELECT COUNT(*) FROM bus_topics", [], |r| {
                r.get::<_, i64>(0)
            })
            .expect("count")
        };
        assert_eq!(before_hash, after_hash);
        assert_eq!(before_hash, 1);
    }

    /// Review finding F7 (BLOCKER-adjacent): the OLD `count_bus_groups`
    /// reached `open_db`/`app_db::open`, which CREATES the content db file
    /// (and its migration ladder) on first open — a real side effect a
    /// "plan" call must never have, called as it is on every uninstall-
    /// dialog open including for an instance nothing has ever provisioned.
    /// This installs a real `addons` row (so the row-lookup half of the
    /// new read-only path succeeds) but points `data_dir` at a directory
    /// that was NEVER created — exactly the state a freshly-installed,
    /// never-enabled instance is in before its own `native_init` runs —
    /// and asserts the plan call leaves that directory exactly as absent
    /// as it found it.
    #[test]
    fn native_teardown_plan_against_a_never_created_data_dir_leaves_the_filesystem_unchanged() {
        let db = test_db();
        let tmp = tempfile::tempdir().expect("tempdir");
        let addon_id = "tentabus-0000000e";
        install_row(&db, addon_id);
        let data_dir = tmp.path().join("never-created");
        assert!(
            !data_dir.exists(),
            "test precondition: dir must not exist yet"
        );
        let c = ctx(&db, addon_id, data_dir.clone());

        let entries = native_teardown_plan(&c).expect("plan");
        assert_eq!(entries.len(), 1, "still exactly one entry");

        assert!(
            !data_dir.exists(),
            "native_teardown_plan must not create the instance data dir — a \
             plan call is side-effect free"
        );
    }

    #[test]
    fn native_teardown_deletes_only_this_instances_rows() {
        let _home = locked_test_home();
        let db = test_db();
        let tmp_a = tempfile::tempdir().expect("tempdir a");
        let tmp_b = tempfile::tempdir().expect("tempdir b");
        let addon_a = unique_addon_id();
        let addon_b = unique_addon_id();
        let addon_a = addon_a.as_str();
        let addon_b = addon_b.as_str();

        for (addon_id, topic) in [(addon_a, "a.topic"), (addon_b, "b.topic")] {
            crate::db::repository::bus_topic_create(
                &db,
                &crate::db::repository::DbBusTopic {
                    instance_id: addon_id.to_string(),
                    org_id: "org-1".to_string(),
                    name: topic.to_string(),
                    partitions: 1,
                    retention_ms: 0,
                    retention_bytes: 0,
                    cleanup_policy: "delete".to_string(),
                    delivery: "at_least_once".to_string(),
                    idempotency_key: None,
                    dedup_window_ms: 0,
                    max_delivery_attempts: 1,
                    retry_backoff_ms: 0,
                    schema_id: None,
                    validation: "none".to_string(),
                    content_type: "application/json".to_string(),
                    replication_factor: 1,
                    acks: "leader".to_string(),
                    durability: "async".to_string(),
                    max_inline_bytes: 1_048_576,
                    compression: "none".to_string(),
                    environment: "prod".to_string(),
                    created_at_ms: 0,
                    updated_at_ms: 0,
                    durability_class: None,
                },
            )
            .expect("create topic");
            crate::db::repository::resource_permissions::set(
                &db,
                "topic",
                &crate::sync::resource_id::composite_resource_id(&[addon_id, "org-1", topic]),
                "user",
                "u1",
                "deny",
            )
            .expect("seed acl row");
        }
        std::fs::write(tmp_b.path().join("marker"), b"b-untouched").expect("seed marker");

        install_row(&db, addon_a);
        install_row(&db, addon_b);
        let ctx_a = ctx(&db, addon_a, tmp_a.path().to_path_buf());
        let ctx_b = ctx(&db, addon_b, tmp_b.path().to_path_buf());
        native_on_enable(&ctx_a).expect("enable A");
        native_on_enable(&ctx_b).expect("enable B");

        native_teardown(&ctx_a).expect("teardown A");

        let remaining_topics: Vec<String> = {
            let conn = db.read().expect("read");
            let mut stmt = conn.prepare("SELECT instance_id FROM bus_topics").unwrap();
            stmt.query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(
            remaining_topics,
            vec![addon_b.to_string()],
            "B's topic row must survive A's teardown"
        );

        let acl_rows_b =
            crate::db::repository::resource_permissions::count_topic_acl_by_instance_prefix(
                &db,
                &crate::sync::resource_id::composite_resource_id(&[addon_b]),
            )
            .expect("count B's acl rows");
        assert_eq!(acl_rows_b, 1, "B's ACL row must survive A's teardown");

        let acl_rows_a =
            crate::db::repository::resource_permissions::count_topic_acl_by_instance_prefix(
                &db,
                &crate::sync::resource_id::composite_resource_id(&[addon_a]),
            )
            .expect("count A's acl rows");
        assert_eq!(acl_rows_a, 0, "A's own ACL row must be gone");

        assert!(
            tmp_b.path().join("marker").exists(),
            "B's data dir must be untouched by A's teardown"
        );
        assert!(
            crate::bus::instance(&BusInstanceId::parse(addon_a).unwrap()).is_none(),
            "A's own engine must be stopped by its own teardown"
        );
        assert!(
            crate::bus::instance(&BusInstanceId::parse(addon_b).unwrap()).is_some(),
            "B's running engine must survive A's teardown untouched"
        );

        native_on_disable(&ctx_b);
    }

    #[test]
    fn enable_disable_enable_cycle_replaces_the_engine_and_the_router_entry() {
        let _home = locked_test_home();
        let db = test_db();
        let tmp = tempfile::tempdir().expect("tempdir");
        let addon_id = unique_addon_id();
        let addon_id = addon_id.as_str();
        install_row(&db, addon_id);
        let c = ctx(&db, addon_id, tmp.path().to_path_buf());
        let id = BusInstanceId::parse(addon_id).expect("valid id");

        // No mesh manager registered in this test process — replication is
        // skipped, exercising only the engine registry half of the cycle
        // (the router identity guard has its own dedicated unit test in
        // `replication::router`).
        native_on_enable(&c).expect("enable");
        assert!(
            crate::bus::running_instances()
                .iter()
                .any(|s| s.instance_id() == addon_id),
            "engine must be present after enable"
        );

        native_on_disable(&c);
        assert!(
            crate::bus::instance(&id).is_none(),
            "engine must be gone after disable"
        );

        // `native_on_disable` calls `BusService::shutdown` (bus/mod.rs),
        // whose sweeper-stop half only sets a flag — the old engine's
        // background threads
        // (metrics-rollup, audit-flush, and the retention sweeper this test
        // configures via `DEFAULT_RETENTION_INTERVAL`) each hold their own
        // `Arc<BusService>` clone and only notice the flag, drop their
        // clone, and (once the LAST clone anywhere is gone) let fjall
        // release the `_meta` lock file once they wake up. `bus::mod.rs`'s
        // `interruptible_sleep`/`SHUTDOWN_POLL_INTERVAL` (added by this
        // wave specifically because this test first exposed the bug: with
        // a single un-interruptible `thread::sleep(interval)`, the
        // retention sweeper alone could hold the lock for the FULL
        // configured interval — minutes, not milliseconds) bounds that to
        // one ~100 ms poll tick per thread regardless of the configured
        // interval. A same-millisecond re-enable racing that teardown can
        // still legitimately need a couple of poll ticks, so this retries
        // over a generous (but now realistic, not multi-minute) window
        // instead of asserting instantaneous re-openability.
        let mut reenabled = Err(anyhow::anyhow!("re-enable never attempted"));
        for _ in 0..50 {
            reenabled = native_on_enable(&c);
            if reenabled.is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        reenabled.expect(
            "re-enable must eventually succeed once the old engine's \
                           background threads release the fjall handle within a \
                           few SHUTDOWN_POLL_INTERVAL ticks",
        );
        assert!(
            crate::bus::instance(&id).is_some(),
            "re-enable must bring the engine back"
        );
        native_on_disable(&c);
    }

    /// Loopback, discovery-disabled `IrohMeshManager` — every test below
    /// only ever needs `router::set_mesh_manager`/`replication::init` to
    /// see a live mesh handle, never an actual peer dial. Same pattern as
    /// `router.rs`'s and `replication::init`'s own copies (private to each
    /// file's own test module, duplicated rather than shared — same
    /// reasoning those two document for their own source).
    async fn make_test_mesh_manager() -> Arc<crate::mesh::iroh_manager::IrohMeshManager> {
        let conn = rusqlite::Connection::open_in_memory().expect("open in-memory db");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS trusted_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                node_id TEXT NOT NULL UNIQUE,
                public_key TEXT NOT NULL,
                hostname TEXT DEFAULT '',
                approved_by TEXT DEFAULT '',
                approved_at TEXT NOT NULL DEFAULT (datetime('now')),
                is_active INTEGER NOT NULL DEFAULT 1,
                last_addresses TEXT NOT NULL DEFAULT '',
                environment TEXT
            );
            CREATE TABLE IF NOT EXISTS pending_pairings (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                remote_node_id TEXT NOT NULL,
                pin_code TEXT NOT NULL,
                direction TEXT NOT NULL CHECK(direction IN ('outgoing','incoming')),
                expires_at TEXT NOT NULL,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE IF NOT EXISTS revoked_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                node_id TEXT NOT NULL UNIQUE,
                revoked_by TEXT,
                revoked_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .expect("create tables");
        let mesh_db: DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        let cipher = std::sync::Arc::new(crate::crypto::SettingsCipher::new(&[7u8; 32]));
        let security = std::sync::Arc::new(
            crate::mesh::security::MeshSecurity::new(mesh_db, cipher).expect("security new"),
        );
        let cfg = crate::mesh::iroh_manager::IrohMeshConfig {
            node_id: String::new(),
            bind_addr: std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
            relay_url: None,
            enable_lan_discovery: false,
            enable_dht_discovery: false,
            ..Default::default()
        };
        crate::mesh::iroh_manager::IrohMeshManager::new(cfg, security)
            .await
            .expect("mesh manager new")
    }

    /// Review finding F1 (BLOCKER): proves the `BusService` <->
    /// `ReplicationManager` reference cycle is actually gone, not merely
    /// that the plain (no-replication) engine registry drops its own
    /// reference — `enable_disable_enable_cycle_replaces_the_engine_and_
    /// the_router_entry` above deliberately runs with NO mesh manager, so
    /// it never proves anything about the cycle this finding is about.
    /// This test drives REAL replication (a real, loopback `IrohMeshManager`,
    /// `native_on_enable`'s replication step actually reaching `replication::
    /// init::init` and inserting into `router`'s registry) before disabling,
    /// so the `Weak` upgrade below is a genuine proof: before the
    /// `Weak<dyn PartitionProvider>` fix (`bus::replication::glue`), the
    /// `ReplicationManager` this test's own `replication_managers()` entry
    /// held a STRONG clone of this exact `Arc<BusService>` inside its
    /// `leader_factory`/`follower_factory`, while this SAME `BusService`
    /// strongly held that SAME `ReplicationManager` back via `set_
    /// replication` — a self-contained cycle `native_on_disable`'s registry
    /// removals alone could never have broken.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn disable_drops_the_engine_even_when_replication_was_running() {
        let _home = locked_test_home();
        let db = test_db();
        let addon_id = unique_addon_id();
        let addon_id = addon_id.as_str();
        install_row(&db, addon_id);
        let tmp = tempfile::tempdir().expect("tempdir");
        let c = ctx(&db, addon_id, tmp.path().to_path_buf());
        let id = BusInstanceId::parse(addon_id).expect("valid id");

        let mesh = make_test_mesh_manager().await;
        router::set_mesh_manager(&mesh);

        native_on_enable(&c).expect("enable");
        assert!(
            replication_managers().contains_key(&id),
            "test setup bug: replication must actually have started for \
             this test to prove the CYCLE (not just the plain engine drop \
             `enable_disable_enable_cycle_...` already covers) is broken"
        );

        let weak = Arc::downgrade(
            &crate::bus::instance(&id).expect("engine must be running after enable"),
        );

        native_on_disable(&c);

        // `BusService::shutdown()` (called by `native_on_disable` via
        // `bus::stop_instance`) only REQUESTS its three background threads
        // stop — each notices within one bounded poll tick of its own sleep
        // loop, not synchronously before `shutdown()` returns (`bus::mod`'s
        // own `interruptible_sleep`/`SHUTDOWN_POLL_INTERVAL` doc: "bounds
        // shutdown-detection latency to one chunk per thread", deliberately
        // NOT instant). Each of those threads holds its own `Arc<BusService>`
        // clone for as long as it is still alive, so the cycle-break this
        // test proves must be observed by polling within that bounded
        // latency, not by asserting the very next instruction after
        // `native_on_disable` returns.
        let mut dropped = false;
        for _ in 0..40 {
            if weak.upgrade().is_none() {
                dropped = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            dropped,
            "review finding F1: disabling a REPLICATING instance must not \
             leak its Arc<BusService> — the reference cycle through the \
             ReplicationManager's leader/follower factories must be broken \
             (waited up to 2s for the bounded-latency background threads to \
             exit)"
        );
        assert!(
            !replication_managers().contains_key(&id),
            "native_on_disable must remove its own REPLICATION_MANAGERS entry"
        );

        // enable -> disable -> enable must not open a SECOND `fjall::
        // Database` over the same `<data dir>/log/_meta` directory — with
        // the `Weak` upgrade above already proving zero references remain,
        // this is the production-shaped consequence: the same directory is
        // immediately reopenable, not stuck behind a stale advisory lock.
        native_on_enable(&c).expect(
            "re-enable must succeed immediately — a still-held fjall lock \
             from the leaked engine would make this `Err`",
        );
        assert!(crate::bus::instance(&id).is_some());
        native_on_disable(&c);
    }

    /// Review finding F3: proves the WASM-consumer-handle half of the same
    /// reference-cycle hazard F1 closes on the replication side — F1's own
    /// `Weak` fix only reaches the `ReplicationManager` holder; a
    /// `ConsumerHandle` opened through `addon::host_functions::bus` is a
    /// SEPARATE, independent holder of the engine's `offsets` store that
    /// F1 alone cannot touch. Opens a real consumer through that module's
    /// own registry (`test_api::register_for_test`, the exact same map
    /// `bus_consume_open_v1` inserts into), leaves it open across
    /// `native_on_disable`, and proves two things: the handle is gone from
    /// that registry immediately (not merely eligible for the idle sweeper
    /// to find within `CONSUMER_IDLE_TIMEOUT`), and — the actual
    /// production hazard this closes — a second `native_on_enable` right
    /// after does not race a fjall lock the leaked handle would otherwise
    /// still be holding.
    #[test]
    fn disable_closes_open_wasm_consumer_handles_before_a_reenable_can_race_their_lock() {
        let _home = locked_test_home();
        let db = test_db();
        let addon_id = unique_addon_id();
        let addon_id = addon_id.as_str();
        install_row(&db, addon_id);
        let tmp = tempfile::tempdir().expect("tempdir");
        let c = ctx(&db, addon_id, tmp.path().to_path_buf());
        let id = BusInstanceId::parse(addon_id).expect("valid id");

        native_on_enable(&c).expect("enable");
        let svc = crate::bus::instance(&id).expect("engine running after enable");

        // `__bus.metrics` + `SYSTEM_ACTOR` (`services::bus_authorizer`'s own
        // doc: "reserved for exactly [broker-internal code]... NOT reachable
        // from any external input") — the one topic/actor pair this test can
        // open a real consumer against without first seeding ACL/matrix
        // grant rows through the real `InstanceBusAuthorizer` this hook
        // wires up.
        svc.publish_metrics_rollup();
        let bctx = crate::bus::BusCallContext {
            instance_id: id.clone(),
            org_id: crate::services::org::DEFAULT_ORG_ID.to_string(),
            actor: Some(crate::services::bus_authorizer::SYSTEM_ACTOR.to_string()),
            correlation_id: None,
            origin: "test".to_string(),
        };
        let handle = svc
            .open_consumer(
                &bctx,
                "f3-test-group",
                &[crate::bus::topics::METRICS_TOPIC_NAME.to_string()],
                crate::bus::ConsumerConfig {
                    commit_mode: crate::bus::groups::CommitMode::Explicit,
                },
            )
            .expect("open consumer");

        let wasm_addon_id = "some-wasm-addon-f3-test";
        let consumer_id = crate::addon::host_functions::bus::test_api::register_for_test(
            wasm_addon_id,
            addon_id,
            vec![crate::bus::topics::METRICS_TOPIC_NAME.to_string()],
            handle,
        )
        .expect("register consumer for test");
        assert!(
            crate::addon::host_functions::bus::test_api::registry_contains(
                wasm_addon_id,
                &consumer_id
            ),
            "test setup bug: consumer must actually be registered"
        );

        // This test's OWN `svc` binding is itself a strong `Arc<BusService>`
        // — unlike F1's test above (which only ever takes a temporary before
        // downgrading it), holding it alive across the disable below would
        // be a self-inflicted leak this test does not intend to prove
        // anything about; drop it explicitly so the ONLY thing keeping the
        // engine's fjall lock open after `native_on_disable` is whatever
        // this test is actually trying to prove (or disprove).
        drop(svc);

        native_on_disable(&c);

        assert!(
            !crate::addon::host_functions::bus::test_api::registry_contains(
                wasm_addon_id,
                &consumer_id
            ),
            "review finding F3: disable must close this instance's own open \
             WASM consumer handles immediately, not wait for the idle sweeper"
        );

        // `bus::stop_instance` (called by `native_on_disable`, via
        // `BusService::shutdown`) only REQUESTS the engine's own background
        // threads stop — same bounded, not-instant, latency the F1 test
        // above polls for (`bus::mod`'s own `SHUTDOWN_POLL_INTERVAL` doc).
        // The assertion just above already proved the CONSUMER half of this
        // fix works (the handle is gone from the registry immediately, no
        // polling needed for that part) — this retry loop tolerates only
        // the separate, already-established general engine-shutdown
        // latency, not a second consumer-specific one.
        let mut reenabled = false;
        let mut last_err = None;
        for _ in 0..40 {
            match native_on_enable(&c) {
                Ok(()) => {
                    reenabled = true;
                    break;
                }
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
        assert!(
            reenabled,
            "re-enable must succeed within the bounded shutdown-detection \
             window — a still-open ConsumerHandle left over from before \
             disable would otherwise keep holding the same fjall lock this \
             re-enable needs INDEFINITELY, not just for a bounded window: \
             {last_err:?}"
        );
        assert!(crate::bus::instance(&id).is_some());
        native_on_disable(&c);
        crate::addon::host_functions::bus::test_api::registry_clear();
    }

    /// Review finding F9: `enable_disable_enable_cycle_replaces_the_engine_
    /// and_the_router_entry` proves the ENGINE registry half of the cycle
    /// but deliberately runs with no mesh manager, so the replication half
    /// of `native_on_enable` — the part that actually touches `replication::
    /// router` — had ZERO coverage. This test drives real replication with
    /// a real mesh manager and proves all three legs: `native_on_enable`
    /// registers this instance's manager with the router, `native_on_
    /// disable` unregisters it, and a stream addressed to the now-disabled
    /// instance is rejected (the same `UnknownInstance` reject a
    /// never-registered id gets, `router::route_stream`'s own doc) rather
    /// than routed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn enable_registers_with_the_router_and_disable_unregisters_it() {
        let _home = locked_test_home();
        let db = test_db();
        let addon_id = unique_addon_id();
        let addon_id = addon_id.as_str();
        install_row(&db, addon_id);
        let tmp = tempfile::tempdir().expect("tempdir");
        let c = ctx(&db, addon_id, tmp.path().to_path_buf());
        let id = BusInstanceId::parse(addon_id).expect("valid id");

        let mesh = make_test_mesh_manager().await;
        router::set_mesh_manager(&mesh);

        native_on_enable(&c).expect("enable");
        assert!(
            router::is_registered_for_test(&id),
            "native_on_enable must register this instance's ReplicationManager \
             with the router once a mesh manager exists — the replication half \
             of native_on_enable had zero router coverage before this test \
             (review finding F9)"
        );

        native_on_disable(&c);
        assert!(
            !router::is_registered_for_test(&id),
            "native_on_disable must unregister this instance from the router"
        );

        // A stream addressed to the now-disabled instance is rejected
        // rather than routed — same `UnknownInstance` reject a
        // never-registered id gets.
        let (client, server) = tokio::io::duplex(16 * 1024);
        let (mut client_recv, mut client_send) = tokio::io::split(client);
        let (server_recv, server_send) = tokio::io::split(server);
        tokio::spawn(router::route_stream(
            "peer".to_string(),
            Box::new(server_recv),
            Box::new(server_send),
        ));
        crate::bus::replication::frames::write_frame(
            &mut client_send,
            &crate::bus::replication::frames::ReplFrame::Hello(
                crate::bus::replication::frames::ReplHello {
                    instance_id: addon_id.to_string(),
                    org_id: "org".to_string(),
                    topic: "orders".to_string(),
                    partition: 0,
                    leader_node_id: "l".to_string(),
                    leader_epoch: 1,
                    replicas: vec!["l".to_string()],
                    environment: tentaflow_protocol::environment::NodeEnvironment::Prod,
                },
            ),
        )
        .await
        .expect("write Hello");
        match crate::bus::replication::frames::read_frame(&mut client_recv)
            .await
            .expect("read HelloAck")
        {
            crate::bus::replication::frames::ReplFrame::HelloAck(ack) => {
                assert!(!ack.accepted, "a disabled instance must reject, not accept");
                assert_eq!(
                    ack.reject,
                    Some(crate::bus::replication::frames::ReplReject::UnknownInstance),
                    "a disabled instance's Hello must be rejected as UnknownInstance, \
                     exactly like an id that was never registered — the router must \
                     not keep routing to a manager whose engine has been stopped"
                );
            }
            other => panic!("expected HelloAck, got {other:?}"),
        }
    }

    /// Review finding F2 (BLOCKER): regression test for plan-app-platform
    /// §8 R10. `tentaflow/src/main.rs` calls `AddonManager::
    /// start_installed_native_instances` (which runs `native_on_enable` for
    /// every already-enabled instance) strictly BEFORE the mesh pipeline
    /// starts and `router::set_mesh_manager` runs — every such instance's
    /// own `native_on_enable` call therefore ran while `router::
    /// mesh_manager()` was still `None`, and came up at RF=1 forever, even
    /// on a node where a mesh IS configured, unless something re-arms it
    /// after the fact.
    ///
    /// This does not depend on the process-global `router::mesh_manager()`
    /// cell happening to be unset when this test runs (a real hazard: it is
    /// shared with every other test in this binary, including this file's
    /// own) — instead it forces the EXACT state the ordering bug leaves
    /// behind, deterministically: an engine running with no
    /// `REPLICATION_MANAGERS` entry at all, regardless of whatever `native_
    /// on_enable` itself managed to do with whatever mesh state happened to
    /// be ambient at the time. That is the state `start_replication_for_
    /// already_enabled_instances` must repair.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn boot_time_rearm_starts_replication_for_an_instance_enabled_before_the_mesh_existed() {
        let _home = locked_test_home();
        let db = test_db();
        let addon_id = unique_addon_id();
        let addon_id = addon_id.as_str();
        install_row(&db, addon_id);
        let tmp = tempfile::tempdir().expect("tempdir");
        let c = ctx(&db, addon_id, tmp.path().to_path_buf());
        let id = BusInstanceId::parse(addon_id).expect("valid id");

        // Simulates `start_installed_native_instances` running before the
        // mesh pipeline: the engine comes up, but (whatever the ambient
        // global mesh state actually was) this test forces the "no
        // replication manager yet" state the real boot-order bug leaves —
        // exactly what `start_replication_for_already_enabled_instances`
        // must repair.
        native_on_enable(&c).expect("enable");
        replication_managers().remove(&id);
        assert!(
            crate::bus::instance(&id).is_some(),
            "test setup: engine must be running"
        );
        assert!(
            !replication_managers().contains_key(&id),
            "test setup: this instance must start with NO replication manager \
             — the exact state the boot-order bug leaves"
        );

        // The mesh becomes available (mirrors `main.rs`'s own `router::
        // set_mesh_manager` call site) — WITHOUT this fix, the instance
        // would simply stay in the state asserted above forever.
        let mesh = make_test_mesh_manager().await;
        router::set_mesh_manager(&mesh);

        start_replication_for_already_enabled_instances();

        assert!(
            replication_managers().contains_key(&id),
            "review finding F2: an instance that was enabled before the mesh \
             existed must have replication started for it once the mesh \
             becomes available — this is exactly what would fail against the \
             pre-fix ordering (native boot pass strictly before `set_mesh_\
             manager`, with nothing ever re-arming replication afterward)"
        );
        assert!(
            router::is_registered_for_test(&id),
            "the re-armed manager must also be registered with the router"
        );

        native_on_disable(&c);
    }

    /// `start_replication_for_already_enabled_instances` must be callable
    /// with zero running instances (the real `main.rs` call site always
    /// runs it once, unconditionally, whether or not any TentaBus instance
    /// has ever been installed yet) without panicking or inserting anything
    /// for an id it never saw.
    #[test]
    fn rearm_is_a_no_op_when_no_instance_is_running() {
        let id = BusInstanceId::parse(&unique_addon_id()).expect("valid id");
        start_replication_for_already_enabled_instances();
        assert!(!replication_managers().contains_key(&id));
    }
}
