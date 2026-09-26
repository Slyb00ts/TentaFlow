// =============================================================================
// File: addon/native_apps.rs — registry of NATIVE core applications
//       (app-platform). A native app is compiled into core and reuses the
//       whole addon platform (catalog, instances, permission matrix); this
//       registry supplies the per-app lifecycle hooks the WASM runtime cannot.
// =============================================================================

use std::path::PathBuf;

use anyhow::Result;

/// Instance-scoped context handed to lifecycle hooks. `data_dir` is the
/// instance's own directory (orgs/<org>/addons/<addon_id>/) — the same
/// containment every addon instance gets. `db` is the MAIN database (platform
/// layer); the app's own content database comes from `app_db::open`.
pub struct NativeAppContext<'a> {
    pub db: &'a crate::db::DbPool,
    pub addon_id: &'a str,
    pub org_id: &'a str,
    pub data_dir: PathBuf,
}

/// Key prefix of the per-node reconcile status rows in `addon_config`
/// (double-underscore namespace, same convention as `__vector_config`). The
/// rows replicate with the instance's config partition — one key per node, so
/// there are no LWW collisions — and instance uninstall purges them with the
/// rest of the scoped tables.
pub const NODE_STATUS_KEY_PREFIX: &str = "__node_status/";

/// Records THIS node's reconcile outcome for a native instance
/// ("ready" | "unsupported" | "init_error"). Best-effort: a status write must
/// never fail the reconcile that produced it.
pub fn record_node_status(db: &crate::db::DbPool, addon_id: &str, status: &str, detail: &str) {
    let node_id =
        crate::sync::runtime::local_node_id().unwrap_or_else(|| "local".to_string());
    let value = serde_json::json!({ "status": status, "detail": detail }).to_string();
    if let Err(e) = crate::db::repository::upsert_addon_config_value(
        db,
        addon_id,
        &format!("{NODE_STATUS_KEY_PREFIX}{node_id}"),
        &value,
        false,
        None,
    ) {
        tracing::warn!("native app '{addon_id}': node status write failed: {e}");
    }
}

/// One entry of the teardown manifest: what uninstall is about to remove (or
/// consciously leave behind). Surfaced in the uninstall dialog and audit log.
#[derive(Default)]
pub struct TeardownEntry {
    pub path: PathBuf,
    /// Stable id the dashboard localizes (`addon_uninstall.entries.<kind>`).
    pub kind: &'static str,
    /// English description for logs and the audit trail — usually a static
    /// literal (`Cow::Borrowed`), but widened from `&'static str` for
    /// plan-app-platform §7 W6 (`addon/mod.rs`'s own correction #8): the
    /// teardown-plan table conflict (three planned entries vs. the byte-
    /// accounting invariant this file's own tests lock in, see `bus::
    /// native::native_teardown_plan`'s doc) is resolved by folding TentaBus's
    /// per-instance row counts into ONE entry's description at plan-build
    /// time, which needs an owned, formatted `String` here.
    pub description: std::borrow::Cow<'static, str>,
    /// false = listed as "consciously left behind" instead of deleted.
    pub removed: bool,
    /// The app's teardown REFUSES while this holds (not a path it removes):
    /// the dialog says so for the node and does not offer the uninstall.
    pub blocks: bool,
    /// Named counts for the i18n template's `{name}`/`{name|a|b|c}`
    /// placeholders at `addon_uninstall.entries.<kind>` (W6-i18n-teardown):
    /// a kind whose description embeds per-instance counts (TentaBus's
    /// `tentabus_data_dir`) fills this so the dashboard can render a real
    /// localized, correctly-pluralized sentence instead of the raw English
    /// `description`. Every other kind leaves it empty and the dashboard
    /// falls back to a plain (non-templated) translation or `description`.
    pub count_vars: std::collections::BTreeMap<String, i64>,
}

/// Lifecycle hooks a native app plugs into the platform. All run on the
/// local node; the fleet-wide fan-out happens through sync reconcile.
pub struct NativeAppHooks {
    pub package_id: &'static str,
    /// Prepare instance state (data dir exists when called; create the app's
    /// own database/schema here). Must be idempotent — reconcile re-runs it.
    pub init: fn(&NativeAppContext) -> Result<()>,
    /// Instance was enabled (admin toggle or replicated enable). `None` = the
    /// app has nothing to start beyond what `init` already did. Must be
    /// idempotent; `init` runs first and may be the whole implementation.
    pub on_enable: Option<fn(&NativeAppContext) -> Result<()>>,
    /// Instance was disabled. `None` = nothing to stop (`disable_semantics`
    /// and `background_on_disable` describe the intent; this executes it).
    pub on_disable: Option<fn(&NativeAppContext)>,
    /// Enumerate app state for the uninstall dialog: what the wipe removes and
    /// what it consciously leaves behind. Must be side-effect free — the
    /// dialog calls it on every open, long before the admin confirms.
    pub teardown_plan: fn(&NativeAppContext) -> Result<Vec<TeardownEntry>>,
    /// App-specific cleanup OUTSIDE the data dir, run right before the platform
    /// removes the data dir. Must not touch user/content data other apps own.
    /// It may report its steps (`teardown_status::phase`) and what it could
    /// not do although it finished (`teardown_status::warn`), for the
    /// uninstall dialog's per-node row.
    pub teardown: fn(&NativeAppContext) -> Result<()>,
    /// What DISABLING the instance does on this node, computed from the app's
    /// real state, for the confirmation the dashboard shows before the toggle
    /// goes off (n18d). `None` = the app has nothing to say, and the toggle
    /// goes off without a dialog as before. The `&str` is the organisation of
    /// the admin asking: a consequence may NAME only that organisation's
    /// resources and count the others. Must be side-effect free.
    pub disable_consequences: Option<fn(&NativeAppContext, &str) -> Result<Vec<DisableConsequence>>>,
    /// The app's teardown may REFUSE (its plan carries `blocks` entries —
    /// TentaNas with Elastic Arrays under supervision). Such an app:
    /// - publishes each node's blockers (`record_teardown_blocks`), and an
    ///   uninstall is refused BEFORE the removal replicates while any node
    ///   blocks or has published nothing (`peer_teardown_refusal`);
    /// - keeps its data directory on a node whose replicated teardown
    ///   refused anyway, since the refusal protects exactly that state.
    /// Every other app's teardown cannot refuse, so none of this applies.
    pub refusable_teardown: bool,
    /// How this node's teardown gets root and which backup it writes (n18a's
    /// mode chip and backup column). `None` = the app needs neither.
    pub teardown_node_info: Option<fn(&NativeAppContext) -> TeardownNodeInfo>,
    /// Arms this node's privilege channel with a sudo password for the
    /// teardown about to start (n18a: a mode-B node's password prompt).
    /// Validates the password; `None` = the app needs no password.
    pub arm_teardown: Option<fn(&NativeAppContext, String) -> Result<String>>,
    /// Drops what `arm_teardown` handed this node, whenever the flow stops
    /// before the teardown consumed it.
    pub disarm_teardown: Option<fn(&NativeAppContext)>,
}

/// n18a's per-node facts that are not paths: see
/// `NativeAppHooks::teardown_node_info`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TeardownNodeInfo {
    /// '' | 'helper' | 'password' (`AddonTeardownPlanResponse::privilege`).
    pub privilege: &'static str,
    /// The backup file name pattern the teardown writes, '' for none.
    pub backup_file: String,
}

/// Key prefix of each node's published teardown blockers in `addon_config`
/// (`__teardown_blocks/<node_id>`), synced like `__node_status/`.
pub const TEARDOWN_BLOCKS_KEY_PREFIX: &str = "__teardown_blocks/";

/// One published blocker: the plan entry's kind and counts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PublishedBlock {
    pub kind: String,
    #[serde(default)]
    pub count_vars: std::collections::BTreeMap<String, i64>,
}

/// Publishes THIS node's teardown blockers for the instance, so every other
/// node knows them while this one is offline (MAJOR 1 of the wave-9b
/// critic). Written only when they changed: the row replicates to the fleet.
pub fn record_teardown_blocks(db: &crate::db::DbPool, addon_id: &str, blocks: &[PublishedBlock]) {
    let node_id = crate::sync::runtime::local_node_id().unwrap_or_else(|| "local".to_string());
    let key = format!("{TEARDOWN_BLOCKS_KEY_PREFIX}{node_id}");
    let Ok(value) = serde_json::to_string(blocks) else { return };
    let current = crate::db::repository::list_addon_config_prefixed(db, addon_id, &key)
        .ok()
        .and_then(|rows| rows.into_iter().find(|(rest, _, _)| rest.is_empty()).map(|(_, v, _)| v));
    if current.as_deref() == Some(value.as_str()) {
        return;
    }
    if let Err(e) = crate::db::repository::upsert_addon_config_value(db, addon_id, &key, &value, false, None) {
        tracing::warn!("native app '{addon_id}': teardown blockers not published: {e}");
    }
}

/// Every node's published teardown blockers, by node id. A node absent from
/// the map published nothing.
pub fn published_teardown_blocks(
    db: &crate::db::DbPool,
    addon_id: &str,
) -> std::collections::BTreeMap<String, Vec<PublishedBlock>> {
    crate::db::repository::list_addon_config_prefixed(db, addon_id, TEARDOWN_BLOCKS_KEY_PREFIX)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(node, value, _)| serde_json::from_str(&value).ok().map(|blocks| (node, blocks)))
        .collect()
}

/// A peer as the uninstall preflight judges it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerForTeardown {
    pub node_id: String,
    /// The node's name ('' when it never told one).
    pub name: String,
    /// Its reconcile status ('unsupported' = nothing to tear down).
    pub status: String,
    /// No longer a trusted peer: the removal cannot reach it.
    pub unpaired: bool,
    /// The mesh reaches it now: its published blockers are its live state.
    pub online: bool,
}

/// What the admin must retype to proceed without a node: its name, or
/// `LOST` for a node that never told one (its id is never shown).
pub fn teardown_ack_word(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() { "LOST".to_string() } else { name.to_string() }
}

/// A node the uninstall proceeds without, for the audit: why it would have
/// held the uninstall back ('unknown' | 'blocked').
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcknowledgedPeer {
    pub name: String,
    pub reason: &'static str,
}

/// Why an uninstall of an app whose teardown can refuse must not start,
/// judged on every OTHER node before the removal replicates: a node whose
/// last published blockers are not empty, or a node that has work (`status`
/// is not 'unsupported') and published nothing (MAJOR 1 of the wave-9b
/// critic).
///
/// Two ways out, so the uninstall can never be held back forever (MAJOR A of
/// round 2):
/// - an UNPAIRED node (no longer a trusted peer) does not count: the removal
///   cannot reach it, it will never run a teardown for this uninstall;
/// - the admin ACKNOWLEDGES a node by retyping its name (`acks`: node id →
///   retyped word, `teardown_ack_word`): "this node will not supervise its
///   arrays; proceed without it". A wrong word does not count.
///
/// `Ok` lists the nodes the uninstall proceeds without, for the audit.
pub fn peer_teardown_refusal(
    published: &std::collections::BTreeMap<String, Vec<PublishedBlock>>,
    nodes: &[PeerForTeardown],
    acks: &std::collections::BTreeMap<String, String>,
) -> Result<Vec<AcknowledgedPeer>, String> {
    let mut acknowledged = Vec::new();
    for node in nodes {
        if node.status == "unsupported" || node.unpaired {
            continue;
        }
        let reason = match published.get(&node.node_id) {
            None => "unknown",
            Some(blocks) if !blocks.is_empty() => "blocked",
            Some(_) => continue,
        };
        let expected = teardown_ack_word(&node.name);
        // A CONNECTED node that refuses now is not a node to proceed without:
        // its teardown refuses for certain the moment the removal reaches it,
        // and the state it protects loses supervision (wave-9b critic, round
        // 3, MAJOR C). It is resolved on that node — its arrays dissolved or
        // moved — not acknowledged. Offline and unknown nodes can be.
        let acknowledgeable = !(reason == "blocked" && node.online);
        if acknowledgeable && acks.get(&node.node_id).map(|typed| typed.trim()) == Some(expected.as_str()) {
            acknowledged.push(AcknowledgedPeer { name: expected, reason });
            continue;
        }
        let shown = if node.name.trim().is_empty() { "a node without a name".to_string() } else { node.name.clone() };
        return Err(match reason {
            "unknown" => format!("refusal:teardown_peer_unknown — {shown} has not published what its teardown would refuse"),
            _ => format!(
                "refusal:teardown_peer_blocked — {shown} refuses the teardown ({})",
                published.get(&node.node_id).map(|b| b.iter().map(|x| x.kind.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default()
            ),
        });
    }
    Ok(acknowledged)
}

/// One consequence of disabling an instance on this node (see
/// `NativeAppHooks::disable_consequences`). The dashboard words `kind`
/// (`addon_disable.consequences.<kind>`) with `count_vars` and `names`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DisableConsequence {
    pub kind: &'static str,
    /// 'continues' | 'stops' | 'kept'.
    pub effect: &'static str,
    pub count_vars: std::collections::BTreeMap<String, i64>,
    /// Real names the sentence lists — never ids.
    pub names: Vec<String>,
}

/// Where the uninstall of an instance stands IN THIS PROCESS, for the
/// uninstall dialog's per-node rows (MAJOR 22): the dialog asks every node
/// (`AddonTeardownStatusRequest`, forwarded) while the removal spreads over
/// the fleet. In memory on purpose: a node that restarts after its teardown
/// has no instance left and answers 'absent', which is true.
pub mod teardown_status {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct Status {
        /// 'running' | 'done' | 'failed'.
        pub state: &'static str,
        /// The step running now — or, once failed, the step that failed.
        pub phase: String,
        /// What the teardown could not do although it went on (codes).
        pub warnings: Vec<String>,
    }

    fn registry() -> &'static Mutex<HashMap<String, Status>> {
        static REG: OnceLock<Mutex<HashMap<String, Status>>> = OnceLock::new();
        REG.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn with(addon_id: &str, f: impl FnOnce(&mut Status)) {
        let mut reg = registry().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(status) = reg.get_mut(addon_id) {
            f(status);
        }
    }

    /// A teardown starts: any earlier record of this instance is replaced.
    pub fn begin(addon_id: &str) {
        registry().lock().unwrap_or_else(|p| p.into_inner()).insert(
            addon_id.to_string(),
            Status { state: "running", phase: "app_teardown".to_string(), warnings: Vec::new() },
        );
    }

    /// The step now running (a stable code the dashboard words).
    pub fn phase(addon_id: &str, phase: &str) {
        with(addon_id, |s| {
            if s.state == "running" {
                s.phase = phase.to_string();
            }
        });
    }

    /// Something the teardown could not do although it goes on.
    pub fn warn(addon_id: &str, code: &str) {
        with(addon_id, |s| {
            if !s.warnings.iter().any(|w| w == code) {
                s.warnings.push(code.to_string());
            }
        });
    }

    /// The end: 'done', or 'failed' in the phase it had reached.
    pub fn finish(addon_id: &str, ok: bool) {
        with(addon_id, |s| {
            s.state = if ok { "done" } else { "failed" };
            if ok {
                s.phase = "done".to_string();
            }
        });
    }

    pub fn get(addon_id: &str) -> Option<Status> {
        registry().lock().unwrap_or_else(|p| p.into_inner()).get(addon_id).cloned()
    }
}

/// Every native app compiled into this binary. Grows with plan-01 P2
/// (Studios retrofit) and new native apps (TentaNas, Chat).
static REGISTRY: &[NativeAppHooks] = &[
    NativeAppHooks {
        package_id: "benchmark-studio",
        init: benchmark_init,
        on_enable: None,
        on_disable: None,
        teardown_plan: data_dir_only_plan,
        teardown: no_external_state,
        disable_consequences: None,
        refusable_teardown: false,
        teardown_node_info: None,
        arm_teardown: None,
        disarm_teardown: None,
    },
    NativeAppHooks {
        package_id: "ml-studio",
        init: ml_studio_init,
        on_enable: None,
        on_disable: None,
        teardown_plan: data_dir_only_plan,
        teardown: no_external_state,
        disable_consequences: None,
        refusable_teardown: false,
        teardown_node_info: None,
        arm_teardown: None,
        disarm_teardown: None,
    },
    NativeAppHooks {
        package_id: "projekty",
        init: projekty_init,
        on_enable: None,
        on_disable: None,
        teardown_plan: data_dir_only_plan,
        teardown: no_external_state,
        disable_consequences: None,
        refusable_teardown: false,
        teardown_node_info: None,
        arm_teardown: None,
        disarm_teardown: None,
    },
    NativeAppHooks {
        package_id: "code-studio",
        init: code_studio_init,
        on_enable: None,
        on_disable: None,
        teardown_plan: data_dir_only_plan,
        teardown: no_external_state,
        disable_consequences: None,
        refusable_teardown: false,
        teardown_node_info: None,
        arm_teardown: None,
        disarm_teardown: None,
    },
    NativeAppHooks {
        package_id: "meeting-bot",
        init: meeting_bot_init,
        on_enable: None,
        on_disable: None,
        teardown_plan: data_dir_only_plan,
        teardown: no_external_state,
        disable_consequences: None,
        refusable_teardown: false,
        teardown_node_info: None,
        arm_teardown: None,
        disarm_teardown: None,
    },
    NativeAppHooks {
        package_id: crate::tentanas::PACKAGE_ID,
        init: crate::tentanas::native_init,
        on_enable: None,
        on_disable: None,
        teardown_plan: crate::tentanas::native_teardown_plan,
        teardown: crate::tentanas::native_teardown,
        disable_consequences: Some(crate::tentanas::native_disable_consequences),
        refusable_teardown: true,
        teardown_node_info: Some(crate::tentanas::native_teardown_node_info),
        arm_teardown: Some(crate::tentanas::native_arm_teardown),
        disarm_teardown: Some(crate::tentanas::native_disarm_teardown),
    },
    // TentaVM's hooks are registered even though the package is not in the
    // catalog yet (the tile needs the UI shell): a hook registered late is a
    // hook nobody notices is missing. Teardown has no work of its own — the
    // machines belong to the hypervisor and keep running, and uninstall only
    // reaches the `addon_*` tables plus the data dir, so the environment's
    // `vm_*` registry rows STAY in the shared database; the teardown plan says
    // so and deleting them lands with the sync step.
    NativeAppHooks {
        package_id: crate::tentavm::PACKAGE_ID,
        init: crate::tentavm::native_init,
        // main made these two fields mandatory while this branch was away;
        // TentaVM has no enable/disable side effects, so both stay None.
        on_enable: None,
        on_disable: None,
        teardown_plan: crate::tentavm::native_teardown_plan,
        teardown: no_external_state,
        disable_consequences: None,
        refusable_teardown: false,
        teardown_node_info: None,
        arm_teardown: None,
        disarm_teardown: None,
    },
    NativeAppHooks {
        package_id: crate::tentaquant::PACKAGE_ID,
        init: crate::tentaquant::native_init,
        // A laboratory has no background runtime: every run is driven by a
        // request, so enabling/disabling only has to flip the gate the
        // platform already flips.
        on_enable: None,
        on_disable: None,
        teardown_plan: crate::tentaquant::native_teardown_plan,
        teardown: crate::tentaquant::native_teardown,
        disable_consequences: None,
        refusable_teardown: false,
        teardown_node_info: None,
        arm_teardown: None,
        disarm_teardown: None,
    },
    NativeAppHooks {
        package_id: crate::bus::native::PACKAGE_ID,
        init: crate::bus::native::native_init,
        on_enable: Some(crate::bus::native::native_on_enable),
        on_disable: Some(crate::bus::native::native_on_disable),
        teardown_plan: crate::bus::native::native_teardown_plan,
        teardown: crate::bus::native::native_teardown,
        disable_consequences: None,
        refusable_teardown: false,
        teardown_node_info: None,
        arm_teardown: None,
        disarm_teardown: None,
    },
];

/// Plan for apps whose whole instance state lives in the data dir (their own
/// database included — `app_db::close` runs before the wipe).
fn data_dir_only_plan(ctx: &NativeAppContext) -> Result<Vec<TeardownEntry>> {
    Ok(vec![TeardownEntry {
        path: ctx.data_dir.clone(),
        kind: "data_dir",
        description: std::borrow::Cow::Borrowed("instance data directory"),
        removed: true,
        ..Default::default()
    }])
}

/// Teardown for apps that keep nothing outside the data dir.
fn no_external_state(_ctx: &NativeAppContext) -> Result<()> {
    Ok(())
}

/// Hooks for a package id, or None for WASM packages.
pub fn hooks_for(package_id: &str) -> Option<&'static NativeAppHooks> {
    #[cfg(any(test, feature = "test-support"))]
    if package_id == test_support::PACKAGE_ID {
        return Some(&test_support::HOOKS);
    }
    #[cfg(any(test, feature = "test-support"))]
    if package_id == test_support::REFUSING_PACKAGE_ID {
        return Some(&test_support::REFUSING_HOOKS);
    }
    REGISTRY.iter().find(|h| h.package_id == package_id)
}

/// Runs the enable/disable hook for a native instance on THIS node.
/// Best-effort and logged: a hook failure must not fail the toggle (the DB
/// flag is the truth; the gate already refuses requests either way).
///
/// Idempotence contract: this fires on EVERY reconcile of a synced instance
/// (`addon::AddonManager::reconcile_synced_addon`), not only on a real
/// enabled ↔ disabled transition — a node catching up on a replicated
/// install/update has no "previous state" of its own to diff against, only
/// the current `is_enabled` flag. The dashboard toggle handler, by contrast,
/// calls this once, only when `set_addon_enabled` actually flips the flag.
/// `on_enable`/`on_disable` hooks MUST therefore be safe to call repeatedly
/// for the same state (already required on `NativeAppHooks` itself; this is
/// the call site that exercises it).
pub fn notify_enabled(
    db: &crate::db::DbPool,
    addon_id: &str,
    package_id: &str,
    manifest: &crate::addon::AddonManifest,
    enabled: bool,
) {
    if !manifest.is_native() {
        return;
    }
    // Cheap insurance for the "nothing may ever mix" invariant: refuse to run
    // a hook against a data dir/instance that is not actually an instance of
    // the named package (a caller passing a mismatched pair would otherwise
    // silently start/stop the wrong app's state).
    match crate::db::repository::get_instance_of_package(db, package_id, addon_id) {
        Ok(Some(_)) => {}
        Ok(None) => {
            tracing::warn!(
                "native app '{addon_id}': not an instance of package '{package_id}' — \
                 refusing enable-notify"
            );
            return;
        }
        Err(e) => {
            tracing::warn!("native app '{addon_id}': membership lookup failed: {e}");
            return;
        }
    }
    let Some(hooks) = hooks_for(package_id) else {
        return;
    };
    let org_id = crate::services::org::DEFAULT_ORG_ID;
    if enabled {
        let data_dir = match crate::addon::fs_sandbox::addon_data_dir(org_id, addon_id) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("native app '{addon_id}': enable-notify data dir: {e:?}");
                return;
            }
        };
        if let Some(on_enable) = hooks.on_enable {
            let ctx = NativeAppContext {
                db,
                addon_id,
                org_id,
                data_dir,
            };
            if let Err(e) = on_enable(&ctx) {
                tracing::warn!("native app '{addon_id}': on_enable hook failed: {e}");
            }
        }
    } else if let Some(on_disable) = hooks.on_disable {
        // Non-creating resolver: a disable notification must never resurrect
        // a data dir an uninstall already removed (or one an in-flight
        // install has not created yet) — `addon_data_dir` would create it.
        let data_dir = match crate::addon::fs_sandbox::addon_data_dir_no_create(org_id, addon_id) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("native app '{addon_id}': disable-notify data dir: {e:?}");
                return;
            }
        };
        let ctx = NativeAppContext {
            db,
            addon_id,
            org_id,
            data_dir,
        };
        on_disable(&ctx);
    }
}

/// Generic fixture for platform tests: a native app entry whose enable/
/// disable hooks are observable, so a test can assert `notify_enabled`
/// actually reaches them without depending on any real app's hooks (the six
/// shipped registry entries have `on_enable`/`on_disable` set to `None`).
#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    use super::{data_dir_only_plan, no_external_state, NativeAppContext, NativeAppHooks};
    use anyhow::Result;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Package id of the fixture — never a real shipped app.
    pub const PACKAGE_ID: &str = "test-hook-app";
    pub static ENABLE_CALLS: AtomicUsize = AtomicUsize::new(0);
    pub static DISABLE_CALLS: AtomicUsize = AtomicUsize::new(0);

    /// A manifest for `PACKAGE_ID`, parseable by `lifecycle::parse_manifest_toml`.
    /// `hooks_for` only recognizes `PACKAGE_ID` itself, so every fixture
    /// instance shares this package id; a test that needs several independent
    /// catalog rows (e.g. one singleton, one not) distinguishes them by
    /// `version` when calling `upsert_addon_package`, not by package id.
    pub fn fixture_manifest_toml(singleton: bool) -> String {
        format!(
            r#"[addon]
id = "{PACKAGE_ID}"
name = "Test Fixture App"
version = "1.0.0"
description = "Generic platform fixture for native-app tests."
category = "test"
author = "TentaFlow"
icon = "trend"
runtime = "native"
platforms = []

[application]
entry_panel = "main"
title = "Test Fixture App"
icon = "trend"
description = "Test fixture"
sort_order = 100

[native]
singleton = {singleton}
routes = ["{PACKAGE_ID}"]
db_file = "fixture.db"

[[permission]]
id = "test.read"
display_name = "Read"
description = "Read the test fixture resource."
risk = "low"
default = "allow"
"#
        )
    }

    fn init(_ctx: &NativeAppContext) -> Result<()> {
        Ok(())
    }

    fn on_enable(_ctx: &NativeAppContext) -> Result<()> {
        ENABLE_CALLS.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn on_disable(_ctx: &NativeAppContext) {
        DISABLE_CALLS.fetch_add(1, Ordering::SeqCst);
    }

    pub(super) static HOOKS: NativeAppHooks = NativeAppHooks {
        package_id: PACKAGE_ID,
        init,
        on_enable: Some(on_enable),
        on_disable: Some(on_disable),
        teardown_plan: data_dir_only_plan,
        teardown: no_external_state,
        disable_consequences: None,
        refusable_teardown: false,
        teardown_node_info: None,
        arm_teardown: None,
        disarm_teardown: None,
    };

    /// A second fixture whose teardown can REFUSE, the way TentaNas's does
    /// with Elastic Arrays: its plan blocks while `REFUSE` is set.
    pub const REFUSING_PACKAGE_ID: &str = "test-refusing-app";
    pub static REFUSE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    fn refusing_plan(ctx: &NativeAppContext) -> Result<Vec<super::TeardownEntry>> {
        let mut entries = data_dir_only_plan(ctx)?;
        if REFUSE.load(Ordering::SeqCst) {
            entries.push(super::TeardownEntry {
                kind: "test_blocker",
                description: std::borrow::Cow::Borrowed("state the teardown would lose"),
                blocks: true,
                ..Default::default()
            });
        }
        Ok(entries)
    }

    pub static DISARM_CALLS: AtomicUsize = AtomicUsize::new(0);

    fn refusing_disarm(_ctx: &NativeAppContext) {
        DISARM_CALLS.fetch_add(1, Ordering::SeqCst);
    }

    pub(super) static REFUSING_HOOKS: NativeAppHooks = NativeAppHooks {
        package_id: REFUSING_PACKAGE_ID,
        init,
        on_enable: None,
        on_disable: None,
        teardown_plan: refusing_plan,
        teardown: no_external_state,
        disable_consequences: None,
        refusable_teardown: true,
        teardown_node_info: None,
        arm_teardown: None,
        disarm_teardown: Some(refusing_disarm),
    };
}

/// Recovers the package id from an instance id (`{package_id}-{8hex}`,
/// `unique_instance_id` in lifecycle.rs). Needed on the sync-remove path,
/// where the local `addons` row is already gone. None when the id does not
/// match the instance shape (e.g. a pre-split legacy row).
pub fn package_of_instance(addon_id: &str) -> Option<&str> {
    let (package, suffix) = addon_id.rsplit_once('-')?;
    if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_hexdigit()) && !package.is_empty() {
        Some(package)
    } else {
        None
    }
}

/// True when the manifest's `platforms` list covers the OS this node runs on
/// (empty list = all platforms). Values follow `std::env::consts::OS`
/// ("linux" / "macos" / "windows"), same convention the manifests use.
pub fn platform_supported(platforms: &[String]) -> bool {
    platforms.is_empty() || platforms.iter().any(|p| p == std::env::consts::OS)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(id: &str, name: &str, status: &str, unpaired: bool) -> PeerForTeardown {
        PeerForTeardown { node_id: id.into(), name: name.into(), status: status.into(), unpaired, online: false }
    }

    /// MAJOR A of round 2: how the preflight judges each peer, and the two
    /// ways out — an unpaired node does not count, and a node acknowledged
    /// by its retyped name is passed (and reported for the audit).
    #[test]
    fn the_preflight_passes_unpaired_and_acknowledged_nodes_only() {
        use std::collections::BTreeMap;
        let blocked = vec![PublishedBlock { kind: "tentanas_elastic_arrays".into(), count_vars: BTreeMap::new() }];
        let published: BTreeMap<String, Vec<PublishedBlock>> =
            [("vega".to_string(), blocked), ("atlas".to_string(), Vec::new())].into_iter().collect();
        let none = BTreeMap::new();
        // Clean, unsupported and unpaired nodes never hold it back.
        assert_eq!(peer_teardown_refusal(&published, &[
            peer("atlas", "atlas", "ready", false),
            peer("tabbie", "tabbie", "unsupported", false),
            peer("gone", "gone", "ready", true),
        ], &none), Ok(vec![]));
        // Never published, and a last plan that blocks.
        assert!(peer_teardown_refusal(&published, &[peer("orion", "orion", "unknown", false)], &none)
            .unwrap_err().starts_with("refusal:teardown_peer_unknown"));
        assert!(peer_teardown_refusal(&published, &[peer("vega", "vega", "ready", false)], &none)
            .unwrap_err().starts_with("refusal:teardown_peer_blocked"));
        // Acknowledged by name: passed and reported; a wrong name is not.
        let acks: BTreeMap<String, String> =
            [("vega".to_string(), "vega".to_string()), ("orion".to_string(), "Orion".to_string()), ("x".to_string(), "LOST".to_string())].into_iter().collect();
        assert_eq!(
            peer_teardown_refusal(&published, &[peer("vega", "vega", "ready", false), peer("x", "", "init_error", false)], &acks),
            Ok(vec![
                AcknowledgedPeer { name: "vega".into(), reason: "blocked" },
                AcknowledgedPeer { name: "LOST".into(), reason: "unknown" },
            ])
        );
        assert!(peer_teardown_refusal(&published, &[peer("orion", "orion", "ready", false)], &acks).is_err(), "names are retyped exactly");
        // MAJOR C (round 3): a CONNECTED node that refuses now cannot be
        // acknowledged — only resolved on that node.
        let online_vega = PeerForTeardown { online: true, ..peer("vega", "vega", "ready", false) };
        assert!(peer_teardown_refusal(&published, &[online_vega], &acks).unwrap_err().starts_with("refusal:teardown_peer_blocked"));
        // An online node that never published (the mesh reaches it, but its
        // record is missing) may still be acknowledged.
        let online_x = PeerForTeardown { online: true, ..peer("x", "", "init_error", false) };
        assert_eq!(peer_teardown_refusal(&published, &[online_x], &acks).map(|a| a.len()), Ok(1));
    }

    /// MAJOR 22: a node's teardown record — the step it is in, what it could
    /// not do, and how it ended in the step it had reached.
    #[test]
    fn a_teardown_record_keeps_its_phase_its_warnings_and_its_end() {
        let id = "wave9b-teardown-record-1a2b3c4d";
        assert_eq!(teardown_status::get(id), None);
        teardown_status::begin(id);
        teardown_status::phase(id, "tentanas_pools");
        teardown_status::warn(id, "tentanas_pools_not_exported");
        teardown_status::warn(id, "tentanas_pools_not_exported");
        teardown_status::finish(id, false);
        let failed = teardown_status::get(id).expect("recorded");
        assert_eq!((failed.state, failed.phase.as_str()), ("failed", "tentanas_pools"), "failed IN the step it reached");
        assert_eq!(failed.warnings, vec!["tentanas_pools_not_exported"], "a warning is said once");
        // A phase after the end changes nothing; a new attempt starts clean.
        teardown_status::phase(id, "data_dir");
        assert_eq!(teardown_status::get(id).expect("recorded").phase, "tentanas_pools");
        teardown_status::begin(id);
        teardown_status::finish(id, true);
        let done = teardown_status::get(id).expect("recorded");
        assert_eq!((done.state, done.phase.as_str(), done.warnings.len()), ("done", "done", 0));
    }

    #[test]
    fn package_of_instance_parses_instance_shape() {
        assert_eq!(
            package_of_instance("benchmark-studio-8a3f2c1d"),
            Some("benchmark-studio")
        );
        // Legacy pre-split rows (addon_id == package_id) are not instances.
        assert_eq!(package_of_instance("benchmark-studio"), None);
        assert_eq!(package_of_instance("x-12345678"), Some("x"));
        assert_eq!(package_of_instance("x-1234567z"), None);
        assert_eq!(package_of_instance("-12345678"), None);
    }

    /// Every registered app previews its wipe without side effects and lists
    /// its data dir as removed — the uninstall dialog relies on both.
    #[test]
    fn every_teardown_plan_lists_the_data_dir_and_leaves_it_untouched() {
        let conn = rusqlite::Connection::open_in_memory().expect("open mem");
        crate::db::migrations::run(&conn).expect("migrate");
        let db: crate::db::DbPool = std::sync::Arc::new(crate::db::Db::from_connection(conn));
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("content.db"), b"x").expect("seed file");
        for hooks in REGISTRY {
            let ctx = NativeAppContext {
                db: &db,
                addon_id: "plan-test-00000000",
                org_id: "default",
                data_dir: tmp.path().to_path_buf(),
            };
            let entries = (hooks.teardown_plan)(&ctx).expect(hooks.package_id);
            let data_dir = entries
                .iter()
                .find(|e| e.path == tmp.path())
                .unwrap_or_else(|| panic!("{}: data dir missing from plan", hooks.package_id));
            assert!(data_dir.removed, "{}: data dir must be removed", hooks.package_id);
            assert!(
                tmp.path().join("content.db").exists(),
                "{}: plan must not touch the data dir",
                hooks.package_id
            );
        }
    }

    #[test]
    fn platform_supported_matches_current_os_or_empty() {
        assert!(platform_supported(&[]));
        assert!(platform_supported(&[std::env::consts::OS.to_string()]));
        assert!(!platform_supported(&["solaris".to_string()]));
    }
}

// =============================================================================
// Benchmark Studio — definitions, runs and results live in the instance
// database declared by the manifest (`native.db_file`); the main DB holds only
// the platform layer. Teardown needs no extra step: the data dir wipe takes
// the file with it.
// =============================================================================

fn benchmark_init(ctx: &NativeAppContext) -> Result<()> {
    // Opening runs the schema migration, so the content db exists and is
    // current right after install/reconcile — the first request never pays
    // for it, and a migration failure surfaces as `init_error` node status
    // instead of a failing handler later.
    crate::addon::app_db::open(ctx.db, ctx.org_id, ctx.addon_id, crate::benchmark::db::migrate)?;
    tracing::info!(
        "native app '{}': instance initialized at {:?}",
        ctx.addon_id,
        ctx.data_dir
    );
    Ok(())
}

// =============================================================================
// ML Studio — content (projects, datasets, models, runs) still lives in the
// main DB until the P2.1 retrofit moves it into the instance database
// declared by the manifest.
// =============================================================================

fn ml_studio_init(ctx: &NativeAppContext) -> Result<()> {
    tracing::info!(
        "native app '{}': instance initialized at {:?}",
        ctx.addon_id,
        ctx.data_dir
    );
    Ok(())
}

// =============================================================================
// Projekty — the project registry and per-project databases stay where they
// are until the P2.1-style content move; the hooks manage only the platform
// instance surface.
// =============================================================================

fn projekty_init(ctx: &NativeAppContext) -> Result<()> {
    tracing::info!(
        "native app '{}': instance initialized at {:?}",
        ctx.addon_id,
        ctx.data_dir
    );
    Ok(())
}

// =============================================================================
// Code Studio — the workspace registry and memberships stay in the main DB
// per plan §6 (they replicate); the node-local vault and provisioning saga
// state live in the instance's own `code_studio.db`; per-workspace runtime
// content lives with each workspace on its owner node.
// =============================================================================

fn code_studio_init(ctx: &NativeAppContext) -> Result<()> {
    // Opening the content DB here creates the file and applies its schema on
    // install; every later open is a registry hit. Idempotent by construction.
    crate::addon::app_db::open(
        ctx.db,
        ctx.org_id,
        ctx.addon_id,
        crate::code_studio::db::migrate,
    )?;
    tracing::info!(
        "native app '{}': instance initialized at {:?}",
        ctx.addon_id,
        ctx.data_dir
    );
    Ok(())
}

// =============================================================================
// Meeting Bot — transcripts and recording blobs stay where they are until the
// content move; full wipe of recording blobs waits on blob GC (research/04).
// =============================================================================

fn meeting_bot_init(ctx: &NativeAppContext) -> Result<()> {
    tracing::info!(
        "native app '{}': instance initialized at {:?}",
        ctx.addon_id,
        ctx.data_dir
    );
    Ok(())
}
