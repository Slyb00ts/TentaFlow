// =============================================================================
// File: tentanas/fleet_mounts.rs — a share of one node, available on every
//       node (plan-02 §5.4, "Dostępność na całej flocie").
//
// WHY EVERY FLEET MOUNT IS NFS, WHATEVER THE SHARE'S OWN PROTOCOL IS:
// mounting an SMB share needs a credential on the CLIENT — a password that
// would have to be generated, stored, replicated to every node and rotated,
// which is exactly the secret distribution the app is built to avoid (§3.4:
// nothing secret ever enters the Sync Ledger). An NFS export authorizes by
// ADDRESS instead, and the addresses of the fleet are already known: each node
// publishes its own, and the source node's export names exactly those. So a
// share marked "mount on every node" gets an extra app-managed export line
// restricted to the fleet, and every other node mounts it read-write under
// `/mnt/tentanas/<name>` — no credential is created, distributed or stored.
//
// DESIRED STATE AND STATUS travel as synced `addon_config` rows, the same
// per-node registry pattern as the platform's `__node_status/<node_id>`:
//
//   __share/<source_node_id>/<share_id>          what the source node offers
//   __mount/<source_node_id>/<share_id>/<node>   what one node did about it
//   __addr/<node_id>                             a node's own LAN addresses
//
// One key per writer, so there are no last-writer-wins collisions, and the
// rows go with the instance's config partition — an offline node still shows
// its last known mount state. The uninstall cascade drops them with the rest
// of the instance's scoped tables; a deleted share purges its own.
// =============================================================================

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tentaflow_protocol::tentanas::{NasFleetMount, NasMountStatus};
use tentanas_helper::HelperCommand;

use super::db::{self as store, ShareRow};
use crate::db::DbPool;
use crate::dispatch::HandlerContext;
use crate::mesh::network_interfaces::sort_prefer_same_subnet;

pub const SHARE_KEY_PREFIX: &str = "__share/";
pub const MOUNT_KEY_PREFIX: &str = "__mount/";
pub const ADDR_KEY_PREFIX: &str = "__addr/";

/// One reconcile a minute, like the scheduler tick; a share change asks for
/// one immediately through `request_reconcile`.
const TICK: Duration = Duration::from_secs(60);
const MOUNT_TIMEOUT: Duration = Duration::from_secs(60);

/// What a source node offers the fleet.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SharePublication {
    pub name: String,
    pub protocol: String,
    /// The path on the SOURCE node — what the UI shows as the share's origin.
    pub source_path: String,
    /// The path a client passes to `mount`. Identical to `source_path` today
    /// (NFSv4 exports the real path); it is a separate field because the
    /// mounting node must not have to know how the source names its data.
    pub export_path: String,
    pub fleet_mount: bool,
    pub enabled: bool,
    /// The source node's LAN addresses, most specific first.
    pub addresses: Vec<String>,
    pub updated_at: String,
}

/// What one node did about one share.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MountPublication {
    pub state: String,
    pub detail: String,
    pub mountpoint: String,
    pub checked_at: String,
    /// The addresses this node mounts FROM, so the source can put them in the
    /// export's client list. Without it the source would have to guess who is
    /// allowed to mount.
    #[serde(default)]
    pub addresses: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct AddrPublication {
    addresses: Vec<String>,
    updated_at: String,
}

// =============================================================================
// the registry
// =============================================================================

pub fn local_node_id() -> String {
    crate::sync::runtime::local_node_id().unwrap_or_else(|| "local".to_string())
}

/// This node's routable addresses. Loopback and link-local are dropped: an
/// export restricted to `fe80::…` authorizes nothing useful, and `127.0.0.1`
/// would authorize every container on the source host.
pub fn local_addresses() -> Vec<String> {
    crate::mesh::node_info_collector::collect_local_addresses()
        .into_iter()
        .filter(|ip| match ip {
            std::net::IpAddr::V4(v4) => !v4.is_loopback() && !v4.is_link_local(),
            std::net::IpAddr::V6(v6) => !v6.is_loopback() && !(v6.segments()[0] & 0xffc0 == 0xfe80),
        })
        .map(|ip| ip.to_string())
        .collect()
}

fn rows(db: &DbPool, addon_id: &str, prefix: &str) -> Vec<(String, String)> {
    crate::db::repository::list_addon_config_prefixed(db, addon_id, prefix)
        .unwrap_or_default()
        .into_iter()
        .map(|(key, value, _)| (key, value))
        .collect()
}

fn put(db: &DbPool, addon_id: &str, key: &str, value: &impl Serialize) {
    let Ok(json) = serde_json::to_string(value) else {
        return;
    };
    if let Err(e) =
        crate::db::repository::upsert_addon_config_value(db, addon_id, key, &json, false, None)
    {
        tracing::warn!("tentanas: fleet row {key} not written: {e}");
    }
}

fn drop_row(db: &DbPool, addon_id: &str, key: &str) {
    if let Err(e) = crate::db::repository::delete_addon_config_value(db, addon_id, key) {
        tracing::warn!("tentanas: fleet row {key} not removed: {e}");
    }
}

/// Every `__share/` row of the fleet as (source_node_id, share_id, row).
pub fn published_shares(db: &DbPool, addon_id: &str) -> Vec<(String, String, SharePublication)> {
    rows(db, addon_id, SHARE_KEY_PREFIX)
        .into_iter()
        .filter_map(|(key, value)| {
            let (node, share_id) = key.split_once('/')?;
            let row: SharePublication = serde_json::from_str(&value).ok()?;
            Some((node.to_string(), share_id.to_string(), row))
        })
        .collect()
}

/// Every `__mount/` row as ((source_node_id, share_id, node_id), row).
pub fn published_mounts(
    db: &DbPool,
    addon_id: &str,
) -> Vec<((String, String, String), MountPublication)> {
    rows(db, addon_id, MOUNT_KEY_PREFIX)
        .into_iter()
        .filter_map(|(key, value)| {
            let mut parts = key.splitn(3, '/');
            let source = parts.next()?.to_string();
            let share_id = parts.next()?.to_string();
            let node = parts.next()?.to_string();
            let row: MountPublication = serde_json::from_str(&value).ok()?;
            Some(((source, share_id, node), row))
        })
        .collect()
}

/// The addresses a source node must allow in its fleet exports: every OTHER
/// node's published addresses. The local node reaches its own shares through
/// the filesystem, so it is not a client of itself.
pub fn fleet_client_addresses(db: &DbPool, addon_id: &str) -> Vec<String> {
    let local = local_node_id();
    let mut out: Vec<String> = rows(db, addon_id, ADDR_KEY_PREFIX)
        .into_iter()
        .filter(|(node, _)| *node != local)
        .filter_map(|(_, value)| serde_json::from_str::<AddrPublication>(&value).ok())
        .flat_map(|a| a.addresses)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Publishes this node's shares and drops the rows of shares it no longer has.
pub fn publish_shares(db: &DbPool, addon_id: &str, shares: &[ShareRow]) {
    let node = local_node_id();
    let addresses = local_addresses();
    let now = store::now();
    for share in shares {
        put(
            db,
            addon_id,
            &format!("{SHARE_KEY_PREFIX}{node}/{}", share.share_id),
            &SharePublication {
                name: share.name.clone(),
                protocol: share.protocol.clone(),
                source_path: share.source_path.clone(),
                export_path: share.source_path.clone(),
                fleet_mount: share.fleet_mount,
                enabled: share.enabled && share.state != "error",
                addresses: addresses.clone(),
                updated_at: now.clone(),
            },
        );
    }
    let live: Vec<&str> = shares.iter().map(|s| s.share_id.as_str()).collect();
    for (source, share_id, _) in published_shares(db, addon_id) {
        if source == node && !live.contains(&share_id.as_str()) {
            purge_share(db, addon_id, &share_id);
        }
    }
}

/// Drops the desired-state row of a deleted share and every mount status any
/// node published for it.
pub fn purge_share(db: &DbPool, addon_id: &str, share_id: &str) {
    let node = local_node_id();
    drop_row(db, addon_id, &format!("{SHARE_KEY_PREFIX}{node}/{share_id}"));
    for ((source, id, client), _) in published_mounts(db, addon_id) {
        if source == node && id == share_id {
            drop_row(
                db,
                addon_id,
                &format!("{MOUNT_KEY_PREFIX}{source}/{id}/{client}"),
            );
        }
    }
}

fn publish_mount(
    db: &DbPool,
    addon_id: &str,
    source: &str,
    share_id: &str,
    row: MountPublication,
) {
    let node = local_node_id();
    put(
        db,
        addon_id,
        &format!("{MOUNT_KEY_PREFIX}{source}/{share_id}/{node}"),
        &row,
    );
}

// =============================================================================
// address preference
// =============================================================================

/// The source address this node should mount from: one that shares a /24 with
/// an address of ours when there is one, otherwise the first the source
/// published. Storage traffic belongs on the storage LAN, not on whichever
/// interface happens to be listed first.
///
/// `sort_prefer_same_subnet` only moves an entry when it found a match, so a
/// sentinel in front turns "did it move?" into a decidable question — the head
/// is no longer the sentinel exactly when a same-subnet address exists.
pub fn preferred_address(source: &[String], local: &[String]) -> Option<String> {
    const SENTINEL: &str = "0.0.0.0";
    for mine in local {
        let mut candidates = vec![SENTINEL.to_string()];
        candidates.extend(source.iter().cloned());
        sort_prefer_same_subnet(&mut candidates, Some(mine.as_str()));
        if candidates.first().map(String::as_str) != Some(SENTINEL) {
            return candidates.into_iter().next();
        }
    }
    source.first().cloned()
}

/// The device spec `mount` is given for an export. An IPv6 literal needs
/// brackets or the colons read as the host/path separator.
pub fn mount_spec(address: &str, export_path: &str) -> String {
    if address.contains(':') {
        format!("[{address}]:{export_path}")
    } else {
        format!("{address}:{export_path}")
    }
}

/// What is mounted at `mountpoint` right now, from the kernel's own list.
pub fn current_mount(mountpoint: &str) -> Option<String> {
    let text = std::fs::read_to_string("/proc/self/mounts").ok()?;
    text.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            Some((f.next()?, f.next()?))
        })
        .find(|(_, m)| *m == mountpoint)
        .map(|(device, _)| device.to_string())
}

pub fn mountpoint_of(name: &str) -> String {
    format!("{}{name}", super::shares::MOUNT_ROOT)
}

// =============================================================================
// reconcile
// =============================================================================

fn wanted() -> &'static AtomicBool {
    static FLAG: AtomicBool = AtomicBool::new(false);
    &FLAG
}

fn stopped() -> &'static AtomicBool {
    static FLAG: AtomicBool = AtomicBool::new(false);
    &FLAG
}

/// Runs the reconcile on the next tick instead of waiting a minute. Called
/// right after a share change so the fleet follows it at once.
pub fn request_reconcile() {
    wanted().store(true, Ordering::Relaxed);
}

/// Stops the loop for good — the uninstall teardown, before it unmounts.
pub fn stop() {
    stopped().store(true, Ordering::Relaxed);
}

/// Starts the per-node reconcile loop once per process, next to the scheduler.
pub fn start(main_db: DbPool, addon_id: String, db: DbPool) {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        tracing::warn!("tentanas: no tokio runtime, fleet reconcile not started");
        return;
    };
    handle.spawn(async move {
        loop {
            if stopped().load(Ordering::Relaxed) {
                return;
            }
            if super::instance_should_run(&main_db, &db) {
                wanted().store(false, Ordering::Relaxed);
                reconcile(&main_db, &addon_id, &db, None).await;
            }
            // A requested reconcile is served within a second; otherwise the
            // loop sleeps out the full tick.
            for _ in 0..TICK.as_secs() {
                tokio::time::sleep(Duration::from_secs(1)).await;
                if wanted().load(Ordering::Relaxed) || stopped().load(Ordering::Relaxed) {
                    break;
                }
            }
        }
    });
}

/// Why this node cannot mount anything right now, or None when it can.
fn blocked() -> Option<(&'static str, &'static str)> {
    if !cfg!(target_os = "linux") || super::environment::find_binary("mount").is_none() {
        return Some(("unsupported", "this platform has no NFS client"));
    }
    None
}

/// One pass: publish this node's addresses, mark the shares it hosts, and
/// mount/unmount what the fleet's desired state asks for. `only` limits the
/// mount work to a single share (the retry button), which also means the pass
/// must not clean up mountpoints — the shares it skipped still own theirs.
pub async fn reconcile(main_db: &DbPool, addon_id: &str, db: &DbPool, only: Option<&str>) {
    let node = local_node_id();
    let addresses = local_addresses();
    put(
        main_db,
        addon_id,
        &format!("{ADDR_KEY_PREFIX}{node}"),
        &AddrPublication {
            addresses: addresses.clone(),
            updated_at: store::now(),
        },
    );

    let published = published_shares(main_db, addon_id);
    let armed = super::broker::channel_available(db).await;
    let platform = blocked();
    let mut keep: Vec<String> = Vec::new();

    for (source, share_id, row) in &published {
        if *source == node {
            // The node that hosts the share is not a client of it; its own
            // status row is what makes the UI show "source" instead of a gap.
            publish_mount(
                main_db,
                addon_id,
                source,
                share_id,
                MountPublication {
                    state: "source".to_string(),
                    detail: String::new(),
                    mountpoint: row.source_path.clone(),
                    checked_at: store::now(),
                    addresses: addresses.clone(),
                },
            );
            continue;
        }
        let mountpoint = mountpoint_of(&row.name);
        if only.is_some_and(|id| id != share_id) {
            keep.push(mountpoint);
            continue;
        }
        let mut status = MountPublication {
            mountpoint: mountpoint.clone(),
            checked_at: store::now(),
            addresses: addresses.clone(),
            ..Default::default()
        };
        if !row.fleet_mount || !row.enabled {
            unmount_if_present(db, &mountpoint).await;
            status.state = "disabled".to_string();
            publish_mount(main_db, addon_id, source, share_id, status);
            continue;
        }
        keep.push(mountpoint.clone());
        if let Some((state, detail)) = platform {
            status.state = state.to_string();
            status.detail = detail.to_string();
            publish_mount(main_db, addon_id, source, share_id, status);
            continue;
        }
        let Some(address) = preferred_address(&row.addresses, &addresses) else {
            status.state = "error".to_string();
            status.detail = "the source node published no address".to_string();
            publish_mount(main_db, addon_id, source, share_id, status);
            continue;
        };
        let spec = mount_spec(&address, &row.export_path);
        if current_mount(&mountpoint).as_deref() == Some(spec.as_str()) {
            status.state = "mounted".to_string();
            publish_mount(main_db, addon_id, source, share_id, status);
            continue;
        }
        if !armed {
            // Mode B between sessions: the mount is not an error, it is
            // waiting for the admin to arm the channel (the orion case).
            status.state = "pending".to_string();
            status.detail = "privilege channel not armed".to_string();
            publish_mount(main_db, addon_id, source, share_id, status);
            continue;
        }
        // Mounted from somewhere else (the source changed address): take it
        // down first, a second mount would stack over the first.
        if current_mount(&mountpoint).is_some() {
            unmount_if_present(db, &mountpoint).await;
        }
        let command = HelperCommand::FleetMount {
            source: address,
            export_path: row.export_path.clone(),
            mountpoint: mountpoint.clone(),
        };
        match super::broker::run_privileged(db, &command, None, MOUNT_TIMEOUT).await {
            Ok((out, _)) if out.success() => status.state = "mounted".to_string(),
            Ok((out, _)) => {
                status.state = "error".to_string();
                status.detail = out
                    .stderr
                    .trim()
                    .lines()
                    .next()
                    .unwrap_or("mount failed")
                    .to_string();
            }
            Err(e) => {
                status.state = "error".to_string();
                status.detail = e.to_string();
            }
        }
        publish_mount(main_db, addon_id, source, share_id, status);
    }

    // A share that disappeared from the fleet leaves a mount behind that
    // nothing would ever unmount again.
    if only.is_none() {
        for mountpoint in stale_mountpoints(&keep) {
            unmount_if_present(db, &mountpoint).await;
        }
    }

    // The source side of the exports: peers appear and change address, and the
    // fleet export's client list has to follow them.
    if armed && hosts_exports(db) {
        refresh_fleet_exports(main_db, addon_id, db).await;
    }
}

/// Whether this node exports anything at all. A node with no share of its own
/// has nothing to re-export, and reading its rows is cheaper than a privileged
/// call that would rewrite an empty file.
fn hosts_exports(db: &DbPool) -> bool {
    store::list_shares(db)
        .map(|shares| shares.iter().any(|s| s.fleet_mount || s.protocol == "nfs"))
        .unwrap_or(false)
}

/// Rewrites the exports file when the fleet's client list changed. Compared
/// against the document the last apply stored, so an unchanged fleet costs one
/// string comparison instead of an `exportfs -ra` every minute.
async fn refresh_fleet_exports(main_db: &DbPool, addon_id: &str, db: &DbPool) {
    let Ok(shares) = store::list_shares(db) else {
        return;
    };
    let active: Vec<ShareRow> = shares.into_iter().filter(|s| s.state == "active").collect();
    let clients = fleet_client_addresses(main_db, addon_id);
    let document = super::shares::exports_document(&active, &clients);
    let previous = store::setting(db, super::shares::SETTING_EXPORTS)
        .ok()
        .flatten()
        .unwrap_or_default();
    if previous == document {
        return;
    }
    match super::broker::run_privileged_with_key(
        db,
        &HelperCommand::NfsExportsWrite {},
        document.as_bytes(),
        None,
        MOUNT_TIMEOUT,
    )
    .await
    {
        Ok((out, _)) if out.success() => {
            let _ = store::set_setting(db, super::shares::SETTING_EXPORTS, &document);
            tracing::info!("tentanas: fleet exports refreshed for {} clients", clients.len());
        }
        Ok((out, _)) => tracing::warn!("tentanas: exports refresh failed: {}", out.stderr.trim()),
        Err(e) => tracing::warn!("tentanas: exports refresh failed: {e}"),
    }
}

/// Mountpoints under the app's root that no live share claims any more.
fn stale_mountpoints(keep: &[String]) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string("/proc/self/mounts") else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        .filter(|m| m.starts_with(super::shares::MOUNT_ROOT))
        .filter(|m| !keep.iter().any(|k| k == m))
        .map(str::to_string)
        .collect()
}

async fn unmount_if_present(db: &DbPool, mountpoint: &str) {
    if current_mount(mountpoint).is_none() {
        return;
    }
    let command = HelperCommand::FleetUmount {
        mountpoint: mountpoint.to_string(),
    };
    if let Err(e) = super::broker::run_privileged(db, &command, None, MOUNT_TIMEOUT).await {
        tracing::warn!("tentanas: {mountpoint} not unmounted: {e}");
    }
}

/// Unmounts every fleet mount of this node — the uninstall teardown (§5.8
/// step 2). The remote data is untouched: only this node stops reading it.
pub async fn unmount_all(db: &DbPool) -> Vec<String> {
    let mut log = Vec::new();
    for mountpoint in stale_mountpoints(&[]) {
        unmount_if_present(db, &mountpoint).await;
        log.push(format!("unmounted {mountpoint}"));
    }
    log
}

// =============================================================================
// views
// =============================================================================

/// The mount status of one share on every node of the fleet, joined with the
/// mesh roster so a node that never reported still gets a row.
pub fn mounts_for(
    ctx: &HandlerContext,
    addon_id: &str,
    source_node: &str,
    share_id: &str,
    fleet_mount: bool,
) -> Vec<NasMountStatus> {
    let reported: BTreeMap<String, MountPublication> = published_mounts(&ctx.state.db, addon_id)
        .into_iter()
        .filter(|((source, id, _), _)| source == source_node && id == share_id)
        .map(|((_, _, node), row)| (node, row))
        .collect();
    super::fleet::nodes(ctx, addon_id)
        .into_iter()
        .map(|node| {
            let row = reported.get(&node.node_id);
            let (state, detail, mountpoint, checked_at) = match row {
                Some(r) => (
                    r.state.clone(),
                    r.detail.clone(),
                    r.mountpoint.clone(),
                    Some(r.checked_at.clone()),
                ),
                // No row yet: either the platform cannot host the app at all,
                // or the node has not run its first reconcile.
                None if node.node_id == source_node => (
                    "source".to_string(),
                    String::new(),
                    String::new(),
                    None,
                ),
                None if node.instance_status == "unsupported" => (
                    "unsupported".to_string(),
                    "the platform of this node has no NAS support".to_string(),
                    String::new(),
                    None,
                ),
                None if !fleet_mount => {
                    ("disabled".to_string(), String::new(), String::new(), None)
                }
                None => (
                    "pending".to_string(),
                    "this node has not reported yet".to_string(),
                    String::new(),
                    None,
                ),
            };
            NasMountStatus {
                node_id: node.node_id,
                node_name: node.node_name,
                state,
                detail,
                mountpoint,
                checked_at,
            }
        })
        .collect()
}

/// The shares of OTHER nodes as this node sees them — the compute node's view.
pub fn fleet_mounts(ctx: &HandlerContext, addon_id: &str) -> Vec<NasFleetMount> {
    let node = local_node_id();
    let names: BTreeMap<String, String> = super::fleet::nodes(ctx, addon_id)
        .into_iter()
        .map(|n| (n.node_id, n.node_name))
        .collect();
    let reported: BTreeMap<(String, String), MountPublication> =
        published_mounts(&ctx.state.db, addon_id)
            .into_iter()
            .filter(|((_, _, client), _)| *client == node)
            .map(|((source, share_id, _), row)| ((source, share_id), row))
            .collect();
    let mut out: Vec<NasFleetMount> = published_shares(&ctx.state.db, addon_id)
        .into_iter()
        .filter(|(source, _, row)| *source != node && row.fleet_mount)
        .map(|(source, share_id, row)| {
            let status = reported.get(&(source.clone(), share_id.clone()));
            NasFleetMount {
                share_name: row.name.clone(),
                protocol: row.protocol,
                source_node_name: names.get(&source).cloned().unwrap_or_else(|| source.clone()),
                mountpoint: status
                    .map(|s| s.mountpoint.clone())
                    .unwrap_or_else(|| mountpoint_of(&row.name)),
                state: status
                    .map(|s| s.state.clone())
                    .unwrap_or_else(|| "pending".to_string()),
                detail: status.map(|s| s.detail.clone()).unwrap_or_default(),
                checked_at: status.map(|s| s.checked_at.clone()),
                source_node_id: source,
                share_id,
            }
        })
        .collect();
    out.sort_by(|a, b| a.share_name.cmp(&b.share_name));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_same_subnet_source_address_wins_over_the_first_one() {
        let source = vec![
            "192.168.1.5".to_string(),
            "10.10.0.5".to_string(),
            "172.16.0.5".to_string(),
        ];
        // Our storage interface is on 10.10.0/24, so that is the address the
        // mount uses even though it is not listed first.
        assert_eq!(
            preferred_address(&source, &["10.10.0.7".to_string()]),
            Some("10.10.0.5".to_string())
        );
        // Nothing in common: the source's own order decides.
        assert_eq!(
            preferred_address(&source, &["203.0.113.9".to_string()]),
            Some("192.168.1.5".to_string())
        );
        // The first local address that matches anything wins.
        assert_eq!(
            preferred_address(
                &source,
                &["203.0.113.9".to_string(), "172.16.0.9".to_string()]
            ),
            Some("172.16.0.5".to_string())
        );
        assert_eq!(preferred_address(&[], &["10.10.0.7".to_string()]), None);
        assert_eq!(
            preferred_address(&source, &[]),
            Some("192.168.1.5".to_string())
        );
    }

    #[test]
    fn the_mount_spec_brackets_an_ipv6_source() {
        assert_eq!(
            mount_spec("10.10.0.5", "/mnt/tank/projekty"),
            "10.10.0.5:/mnt/tank/projekty"
        );
        assert_eq!(
            mount_spec("fd00::5", "/mnt/tank/projekty"),
            "[fd00::5]:/mnt/tank/projekty"
        );
    }

    #[test]
    fn a_share_name_maps_to_exactly_one_mountpoint() {
        assert_eq!(mountpoint_of("projekty"), "/mnt/tentanas/projekty");
        assert!(tentanas_helper::validate_fleet_mountpoint(&mountpoint_of("projekty")).is_ok());
    }
}
